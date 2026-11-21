/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! LucinaNet: shaping with HarfRust, the Rust port of HarfBuzz. Same scale and cluster level as
//! the HarfBuzz shaper this replaces, and glyph advances still come from `Font`, so layout agrees.

use std::str::FromStr;
use std::sync::{Arc, RwLock};

use app_units::Au;
use euclid::default::Point2D;
use harfrust::{
    Buffer, ClusterLevel, Direction, Feature, FontFuncs, GlyphInfo, GlyphPosition, Language,
    Script, ShapeOptions, ShapePlan, ShaperFont,
};
use num_traits::Zero;
use read_fonts::tables::base::Axis;
use read_fonts::types::Tag;
use read_fonts::{FontRef, TableProvider};
use rustc_hash::FxHashMap;

use super::{GlyphShapingResult, ShapedGlyph, unicode_script_to_iso15924_tag};
use crate::{
    BASE, Font, FontBaseline, GlyphId, ShapedText, ShapingFlags, ShapingOptions, fixed_to_float,
    float_to_fixed,
};

const DEFAULT_SCRIPT: Tag = Tag::new(b"DFLT");
const BASELINE_ROMAN: Tag = Tag::new(b"romn");
const BASELINE_HANGING: Tag = Tag::new(b"hang");
const BASELINE_IDEO_EMBOX_BOTTOM: Tag = Tag::new(b"ideo");

pub(crate) struct HarfRustGlyphShapingResult {
    infos: Vec<GlyphInfo>,
    positions: Vec<GlyphPosition>,
}

struct ShapedGlyphIterator<'a> {
    result: &'a HarfRustGlyphShapingResult,
    current_glyph_offset: usize,
    y_position: Au,
}

impl Iterator for ShapedGlyphIterator<'_> {
    type Item = ShapedGlyph;

    fn next(&mut self) -> Option<Self::Item> {
        let info = self.result.infos.get(self.current_glyph_offset)?;
        let position = self.result.positions.get(self.current_glyph_offset)?;

        let x_offset = Au::from_f64_px(Shaper::fixed_to_float(position.x_offset));
        let y_offset = Au::from_f64_px(Shaper::fixed_to_float(position.y_offset));
        let x_advance = Au::from_f64_px(Shaper::fixed_to_float(position.x_advance));
        let y_advance = Au::from_f64_px(Shaper::fixed_to_float(position.y_advance));

        let offset = if x_offset.is_zero() && y_offset.is_zero() && y_advance.is_zero() {
            None
        } else {
            // adjust the pen..
            if y_advance > Au::zero() {
                self.y_position -= y_advance;
            }

            Some(Point2D::new(x_offset, self.y_position - y_offset))
        };

        self.current_glyph_offset += 1;
        Some(ShapedGlyph {
            glyph_id: info.glyph_id as GlyphId,
            string_byte_offset: info.cluster as usize,
            advance: x_advance,
            offset,
        })
    }
}

impl GlyphShapingResult for HarfRustGlyphShapingResult {
    #[inline]
    fn len(&self) -> usize {
        self.infos.len()
    }

    fn iter(&self) -> impl Iterator<Item = ShapedGlyph> {
        ShapedGlyphIterator {
            result: self,
            current_glyph_offset: 0,
            y_position: Au::zero(),
        }
    }

    fn is_rtl(&self) -> bool {
        match (self.infos.first(), self.infos.last()) {
            (Some(first), Some(last)) => last.cluster < first.cluster,
            _ => false,
        }
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct PlanKey {
    rtl: bool,
    script: u32,
    language: String,
    features: Vec<(u32, u32)>,
}

/// What HarfRust calls back for: the advances `Font` already knows, so that they agree with its metrics.
struct Advances<'a>(&'a Font);

impl FontFuncs for Advances<'_> {
    fn glyph_h_advance(&self, _: &ShaperFont, glyph: harfrust::GlyphId) -> i32 {
        Shaper::float_to_fixed(self.0.glyph_h_advance(glyph.to_u32() as GlyphId))
    }
}

/// The shaping state of a `Font`, which owns it. It keeps no pointer back to its `Font`, which can
/// be moved: every call that needs the font's metrics is given the `Font`.
pub(crate) struct Shaper {
    shaping_font: Option<harfrust::Font>,
    scale: i32,
    plans: RwLock<FxHashMap<PlanKey, Arc<ShapePlan>>>,
}

impl std::fmt::Debug for Shaper {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Shaper").finish_non_exhaustive()
    }
}

/// The font's data as HarfRust wants it to be shared.
struct FontBytes(crate::FontData);

impl AsRef<[u8]> for FontBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl Shaper {
    pub(crate) fn new(font: &Font) -> Shaper {
        let pt_size = font.descriptor.pt_size.to_f64_px();
        let shaping_font = font.font_data_and_index().ok().and_then(|data| {
            let bytes: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(FontBytes(data.data.clone()));
            let shaping_font = harfrust::Font::new(bytes, data.index)?;
            let variations = font.variations();
            if !servo_config::pref!(layout_css_font_variations_enabled) || variations.is_empty() {
                return Some(shaping_font);
            }
            Some(
                shaping_font
                    .instance_builder()
                    .variations(variations.iter().map(|variation| {
                        harfrust::font::Variation::new(
                            Tag::from_u32(variation.tag),
                            variation.value,
                        )
                    }))
                    .build(),
            )
        });
        Shaper {
            shaping_font,
            scale: Self::float_to_fixed(pt_size),
            plans: RwLock::default(),
        }
    }

    /// Calculate the layout metrics associated with the given text with the [`Shaper`]s font.
    fn shaped_glyph_data(
        &self,
        font: &Font,
        text: &str,
        options: &ShapingOptions,
        font_features: &[(Tag, u32)],
    ) -> HarfRustGlyphShapingResult {
        let Some(shaping_font) = &self.shaping_font else {
            return HarfRustGlyphShapingResult {
                infos: Vec::new(),
                positions: Vec::new(),
            };
        };
        let rtl = options.flags.contains(ShapingFlags::RTL_FLAG);
        let script_tag = unicode_script_to_iso15924_tag(options.script);
        let script = Script::from_iso15924_tag(Tag::from_u32(script_tag));
        let language = Language::from_str(options.language.as_str()).ok();

        let mut buffer = Buffer::new();
        buffer.set_cluster_level(ClusterLevel::MonotoneCharacters);
        buffer.set_direction(if rtl {
            Direction::RightToLeft
        } else {
            Direction::LeftToRight
        });
        buffer.set_script(script);
        buffer.set_language(language.clone());
        buffer.push_str(text);

        let features: Vec<_> = font_features
            .iter()
            .map(|(tag, value)| Feature::new(*tag, *value, ..))
            .collect();
        let advances = Advances(font);
        let shaper_font = ShaperFont::new(shaping_font)
            .with_scale(self.scale)
            .with_font_funcs(Some(&advances));

        // Planning walks the font's lookups, and HarfBuzz caches plans the same way.
        let key = PlanKey {
            rtl,
            script: script_tag,
            language: options.language.as_str().to_owned(),
            features: font_features
                .iter()
                .map(|(tag, value)| (u32::from_be_bytes(tag.to_be_bytes()), *value))
                .collect(),
        };
        let cached = self
            .plans
            .read()
            .ok()
            .and_then(|plans| plans.get(&key).cloned());
        let plan = cached.unwrap_or_else(|| {
            let plan = Arc::new(ShapePlan::new(
                shaping_font,
                if rtl {
                    Direction::RightToLeft
                } else {
                    Direction::LeftToRight
                },
                script,
                language.as_ref(),
                &features,
            ));
            if let Ok(mut plans) = self.plans.write() {
                plans.insert(key, plan.clone());
            }
            plan
        });

        let shaped = harfrust::shape(
            &shaper_font,
            &mut buffer,
            ShapeOptions::new().plan(Some(&plan)).features(&features),
        );
        if shaped.is_err() {
            return HarfRustGlyphShapingResult {
                infos: Vec::new(),
                positions: Vec::new(),
            };
        }
        HarfRustGlyphShapingResult {
            infos: buffer.glyph_infos().to_vec(),
            positions: buffer.glyph_positions().to_vec(),
        }
    }

    pub(crate) fn shape_text(
        &self,
        font: &Font,
        text: &str,
        options: &ShapingOptions,
        font_features: &[(Tag, u32)],
    ) -> ShapedText {
        ShapedText::with_shaped_glyph_data(
            text,
            options,
            &self.shaped_glyph_data(font, text, options, font_features),
        )
    }

    pub(crate) fn baseline(&self, font: &Font) -> Option<FontBaseline> {
        font.table_for_tag(BASE)?;
        let data = font.font_data_and_index().ok()?;
        let font_ref = FontRef::from_index(data.data.as_ref(), data.index).ok()?;
        let axis = font_ref.base().ok()?.horiz_axis()?.ok()?;
        let per_unit =
            font.descriptor.pt_size.to_f64_px() / f64::from(font_ref.head().ok()?.units_per_em());
        let baseline =
            |tag| (baseline_in_font_units(&axis, tag).unwrap_or(0) as f64 * per_unit) as f32;

        Some(FontBaseline {
            ideographic_baseline: baseline(BASELINE_IDEO_EMBOX_BOTTOM),
            alphabetic_baseline: baseline(BASELINE_ROMAN),
            hanging_baseline: baseline(BASELINE_HANGING),
        })
    }

    fn float_to_fixed(f: f64) -> i32 {
        float_to_fixed(16, f)
    }

    fn fixed_to_float(i: i32) -> f64 {
        fixed_to_float(16, i)
    }
}

/// `hb_ot_layout_get_baseline` for the default script, whose base values give this baseline tag.
fn baseline_in_font_units(axis: &Axis, tag: Tag) -> Option<i16> {
    let tags = axis.base_tag_list()?.ok()?;
    let index = tags.baseline_tags().iter().position(|t| t.get() == tag)?;
    let scripts = axis.base_script_list().ok()?;
    let record = scripts
        .base_script_records()
        .iter()
        .find(|record| record.base_script_tag() == DEFAULT_SCRIPT)?;
    let script = record.base_script(scripts.offset_data()).ok()?;
    let values = script.base_values()?.ok()?;
    let coordinate = values.base_coords().get(index).ok()?;
    Some(coordinate.coordinate())
}
