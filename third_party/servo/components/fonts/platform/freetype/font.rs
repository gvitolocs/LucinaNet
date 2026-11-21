/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The platform font of FreeType platforms, without FreeType: read-fonts and skrifa read the tables, and
//! `lucina_fontface` reproduces the numbers FreeType 2.13.2 gave this code (size scale, face metrics, light-hinted
//! advances, `kern` pairs, glyph boxes, bitmap strikes).

use std::path::Path;

use app_units::Au;
use euclid::default::{Point2D, Rect, Size2D};
use fonts_traits::{FontIdentifier, FontTemplateDescriptor, LocalFontIdentifier};
use log::debug;
use lucina_fontface::lucina_ftcompat::{F26Dot6, Hinting};
use lucina_fontface::{self as face, Sizing};
use read_fonts::types::Tag;
use read_fonts::{FontRef, ReadError, TableProvider};
use servo_arc::Arc;
use skrifa::MetadataProvider;
use skrifa::attribute::Weight;
use skrifa::instance::Location;
use style::Zero;
use webrender_api::{FontHinting, FontInstanceFlags, FontInstancePlatformOptions, FontVariation};

use super::{FontFileBytes, font_file_bytes};
use crate::FontData;
use crate::font::{FontMetrics, FontTableMethods, FractionalPixel, PlatformFontMethods};
use crate::glyph::GlyphId;

const SEMI_BOLD_U16: u16 = Weight::SEMI_BOLD.value() as u16;

/// The fallback hinting style to use when there is no user-specified default hinting style.
///
/// This defaults to slight hinting, which is what most Linux distros use by default, and is a better default than no
/// hinting.
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
pub(crate) const FALLBACK_HINTING_STYLE: FontHinting = FontHinting::Light;

/// The fallback hinting style to use when there is no user-specified default hinting style.
///
/// This defaults to no hinting, as embedded devices such as Android and OHOS typically have high density screens.
#[cfg(any(target_os = "android", target_env = "ohos"))]
pub(crate) const FALLBACK_HINTING_STYLE: FontHinting = FontHinting::None;

/// What advances look like under [`FALLBACK_HINTING_STYLE`]. FreeType rounds the advance to whole pixels for every
/// hinted load (light: the autohinter and the CFF hinter; mono and normal: the TrueType interpreter), and leaves it
/// fractional only with `FT_LOAD_NO_HINTING`.
fn advance_hinting() -> Hinting {
    match FALLBACK_HINTING_STYLE {
        FontHinting::None => Hinting::None,
        FontHinting::Mono | FontHinting::Light | FontHinting::LCD | FontHinting::Normal => Hinting::Light,
    }
}

fn f26dot6_to_float(value: F26Dot6) -> f64 {
    value as f64 / 64.0
}

#[derive(Debug)]
pub struct FontTable {
    data: FontBacking,
    tag: Tag,
}

impl FontTableMethods for FontTable {
    fn buffer(&self) -> &[u8] {
        let font_ref = self.data.font_ref().expect("Font checked before creating");
        let table_data = font_ref
            .table_data(self.tag)
            .expect("Table existence checked before creating");
        table_data.as_bytes()
    }
}

#[derive(Debug)]
pub struct PlatformFont {
    data: FontBacking,
    requested_face_size: Au,
    actual_face_size: Au,
    /// The size the face is set to; `None` when no size was requested (FreeType then had an unsized face, on which
    /// glyph loads fail and size metrics are zero).
    sizing: Option<Sizing>,
    /// Normalized variation coordinates: the default instance, the named instance of a local font, or the applied
    /// variations.
    location: Location,
    variations: Vec<FontVariation>,
    synthetic_bold: bool,
}

impl PlatformFontMethods for PlatformFont {
    fn new_from_data(
        _font_identifier: FontIdentifier,
        font_data: &FontData,
        requested_size: Option<Au>,
        synthetic_bold: bool,
    ) -> Result<PlatformFont, &'static str> {
        Self::new(FontBacking::Web(font_data.clone()), 0, requested_size, synthetic_bold)
    }

    fn new_from_local_font_identifier(
        font_identifier: LocalFontIdentifier,
        requested_size: Option<Au>,
        synthetic_bold: bool,
    ) -> Result<PlatformFont, &'static str> {
        let Ok(memory_mapped_font_data) =
            font_file_bytes(Path::new(&*font_identifier.path)).map(Arc::new)
        else {
            return Err("Could not memory map font");
        };
        Self::new(
            FontBacking::Local(memory_mapped_font_data, font_identifier.index()),
            font_identifier.named_instance_index(),
            requested_size,
            synthetic_bold,
        )
    }

    fn copy_with_variations(
        mut self,
        _: &FontIdentifier,
        variations: &[FontVariation],
    ) -> Result<Self, &'static str> {
        if variations.is_empty() || !servo_config::pref!(layout_css_font_variations_enabled) {
            self.variations = vec![];
            return Ok(self);
        }
        let font_ref = self
            .data
            .font_ref()
            .map_err(|_| "Failed to query font variations")?;
        let requested: Vec<(Tag, f32)> = variations
            .iter()
            .map(|variation| (Tag::from_u32(variation.tag), variation.value))
            .collect();
        let Some(applied) = face::variations(&font_ref, &requested) else {
            // FT_HAS_MULTIPLE_MASTERS is false: nothing to do.
            self.variations = vec![];
            return Ok(self);
        };
        self.location = applied.location;
        self.variations = applied
            .reported
            .into_iter()
            .map(|(tag, value)| FontVariation {
                tag: u32::from_be_bytes(tag.to_be_bytes()),
                value,
            })
            .collect();
        Ok(self)
    }

    fn descriptor(&self) -> FontTemplateDescriptor {
        let Ok(font_ref) = self.data.font_ref() else {
            return FontTemplateDescriptor::default();
        };
        let Ok(os2) = font_ref.os2() else {
            return FontTemplateDescriptor::default();
        };
        Self::descriptor_from_os2_table(&os2)
    }

    fn glyph_index(&self, codepoint: char) -> Option<GlyphId> {
        let font_ref = self.data.font_ref().ok()?;
        let index = face::glyph_index(&font_ref, codepoint);
        if index.is_none() {
            debug!(
                "Invalid codepoint: U+{:04X} ('{}')",
                codepoint as u32, codepoint
            );
        }
        index
    }

    fn glyph_h_kerning(&self, first_glyph: GlyphId, second_glyph: GlyphId) -> FractionalPixel {
        let (Ok(font_ref), Some(sizing)) = (self.data.font_ref(), self.sizing.as_ref()) else {
            return 0.0;
        };
        let kerning = face::glyph_h_kerning(&font_ref, sizing, first_glyph, second_glyph);
        f26dot6_to_float(kerning) * self.unscalable_font_metrics_scale()
    }

    fn glyph_h_advance(&self, glyph: GlyphId) -> Option<FractionalPixel> {
        let font_ref = self.data.font_ref().ok()?;
        let sizing = self.sizing.as_ref()?;
        let Some(advance) = face::glyph_h_advance(
            &font_ref,
            sizing,
            self.location.coords(),
            glyph,
            advance_hinting(),
            self.synthetic_bold,
        ) else {
            debug!("Unable to load glyph {}", glyph);
            return None;
        };
        Some(f26dot6_to_float(advance) * self.unscalable_font_metrics_scale())
    }

    fn metrics(&self) -> FontMetrics {
        let font_ref = self.data.font_ref();
        let sizing = self.sizing.unwrap_or_default();
        let scalable = font_ref.as_ref().is_ok_and(face::is_scalable);

        let mut max_advance;
        let mut max_ascent;
        let mut max_descent;
        let mut line_height;
        let mut y_scale = 0.0;
        let mut em_height;
        if scalable {
            // FT_Size_Metrics::y_scale is in 16.16 fixed point format. Its (fractional) value is a factor that
            // converts vertical metrics from design units to units of 1/64 pixels, so that the result may be
            // interpreted as pixels in 26.6 fixed point format. This converts the value to a float without losing
            // precision.
            y_scale = sizing.y_scale as f64 / 65536.0 / 64.0;
            let (face_metrics, units_per_em) = match font_ref.as_ref() {
                Ok(font_ref) => (
                    face::face_metrics(font_ref, self.location.coords()),
                    face::units_per_em(font_ref),
                ),
                Err(_) => Default::default(),
            };
            max_advance = face_metrics.max_advance_width as f64 * y_scale;
            max_ascent = face_metrics.ascender as f64 * y_scale;
            max_descent = -(face_metrics.descender as f64) * y_scale;
            line_height = face_metrics.height as f64 * y_scale;
            em_height = units_per_em as f64 * y_scale;
        } else {
            max_advance = f26dot6_to_float(sizing.max_advance);
            max_ascent = f26dot6_to_float(sizing.ascender);
            max_descent = -f26dot6_to_float(sizing.descender);
            line_height = f26dot6_to_float(sizing.height);

            em_height = sizing.y_ppem as f64;
            // Without outlines there is no units_per_EM in FreeType's face, but the 'head' table has one.
            if let Ok(head) = font_ref.clone().and_then(|font_ref| font_ref.head()) {
                // Bug 1267909 - Even if the font is not explicitly scalable, if the face has color bitmaps, it
                // should be treated as scalable and scaled to the desired size. Metrics based on y_ppem need to be
                // rescaled for the adjusted size.
                if font_ref.as_ref().is_ok_and(face::has_color) {
                    em_height = self.requested_face_size.to_f64_px();
                    let adjust_scale = em_height / (sizing.y_ppem as f64);
                    max_advance *= adjust_scale;
                    max_descent *= adjust_scale;
                    max_ascent *= adjust_scale;
                    line_height *= adjust_scale;
                }
                y_scale = em_height / head.units_per_em() as f64;
            }
        }

        // 'leading' is supposed to be the vertical distance between two baselines, reflected by the height
        // attribute in freetype. On OS X (w/ CTFont), leading represents the distance between the bottom of a line
        // descent to the top of the next line's ascent or: (line_height - ascent - descent), see
        // http://stackoverflow.com/a/5635981 for CTFont implementation. Convert using a formula similar to what
        // CTFont returns for consistency.
        let leading = line_height - (max_ascent + max_descent);

        let face_metrics = match font_ref.as_ref() {
            Ok(font_ref) => face::face_metrics(font_ref, self.location.coords()),
            Err(_) => Default::default(),
        };
        let underline_size = face_metrics.underline_thickness as f64 * y_scale;
        let underline_offset = face_metrics.underline_position as f64 * y_scale + 0.5;

        // The default values for strikeout size and offset. Use OpenType spec's suggested position for Roman font
        // as the default for offset.
        let mut strikeout_size = underline_size;
        let mut strikeout_offset = em_height * 409.0 / 2048.0 + 0.5 * strikeout_size;

        // CSS 2.1, section 4.3.2 Lengths: "In the cases where it is impossible or impractical to determine the
        // x-height, a value of 0.5em should be used."
        let mut x_height = 0.5 * em_height;
        let mut average_advance = 0.0;

        if let Ok(os2) = font_ref.and_then(|font_ref| font_ref.os2()) {
            let y_strikeout_size = os2.y_strikeout_size();
            let y_strikeout_position = os2.y_strikeout_position();
            if !y_strikeout_size.is_zero() && !y_strikeout_position.is_zero() {
                strikeout_size = y_strikeout_size as f64 * y_scale;
                strikeout_offset = y_strikeout_position as f64 * y_scale;
            }

            let sx_height = os2.sx_height().unwrap_or(0);
            if !sx_height.is_zero() {
                x_height = sx_height as f64 * y_scale;
            }

            let x_average_char_width = os2.x_avg_char_width();
            if !x_average_char_width.is_zero() {
                average_advance = x_average_char_width as f64 * y_scale;
            }
        }

        if average_advance.is_zero() {
            average_advance = self
                .glyph_index('0')
                .and_then(|idx| self.glyph_h_advance(idx))
                .unwrap_or(max_advance);
        }

        let zero_horizontal_advance = self
            .glyph_index('0')
            .and_then(|idx| self.glyph_h_advance(idx))
            .map(Au::from_f64_px);
        let ic_horizontal_advance = self
            .glyph_index('\u{6C34}')
            .and_then(|idx| self.glyph_h_advance(idx))
            .map(Au::from_f64_px);
        let space_advance = self
            .glyph_index(' ')
            .and_then(|idx| self.glyph_h_advance(idx))
            .unwrap_or(average_advance);

        FontMetrics {
            underline_size: Au::from_f64_px(underline_size),
            underline_offset: Au::from_f64_px(underline_offset),
            strikeout_size: Au::from_f64_px(strikeout_size),
            strikeout_offset: Au::from_f64_px(strikeout_offset),
            leading: Au::from_f64_px(leading),
            x_height: Au::from_f64_px(x_height),
            em_size: Au::from_f64_px(em_height),
            ascent: Au::from_f64_px(max_ascent),
            descent: Au::from_f64_px(max_descent),
            max_advance: Au::from_f64_px(max_advance),
            average_advance: Au::from_f64_px(average_advance),
            line_gap: Au::from_f64_px(line_height),
            zero_horizontal_advance,
            ic_horizontal_advance,
            space_advance: Au::from_f64_px(space_advance),
        }
    }

    fn table_for_tag(&self, tag: Tag) -> Option<FontTable> {
        let font_ref = self.data.font_ref().ok()?;
        let _table_data = font_ref.table_data(tag)?;
        Some(FontTable {
            data: self.data.clone(),
            tag,
        })
    }

    fn typographic_bounds(&self, glyph_id: GlyphId) -> Rect<f32> {
        let (Ok(font_ref), Some(sizing)) = (self.data.font_ref(), self.sizing.as_ref()) else {
            return Rect::default();
        };
        let Some([x, y, width, height]) =
            face::glyph_box(&font_ref, sizing, self.location.coords(), glyph_id)
        else {
            debug!("Unable to load glyph {}", glyph_id);
            return Rect::default();
        };
        Rect::new(
            Point2D::new(x as f32, y as f32),
            Size2D::new(width as f32, height as f32),
        ) * (1. / 64.)
    }

    fn webrender_font_instance_flags(&self) -> FontInstanceFlags {
        // On other platforms, we only pass this when we know that we are loading a font with color characters, but
        // not passing this flag simply *prevents* WebRender from loading bitmaps. There's no harm to always passing
        // it.
        let mut flags = FontInstanceFlags::EMBEDDED_BITMAPS;

        // TODO: Add support for synthetic italics.
        // <https://github.com/servo/servo/issues/39637>
        if self.synthetic_bold {
            flags |= FontInstanceFlags::SYNTHETIC_BOLD;
        }

        flags
    }

    fn webrender_font_instance_platform_options(&self) -> FontInstancePlatformOptions {
        FontInstancePlatformOptions {
            // TODO: We should eventually read the hinting style from the system when
            // possible such as from Fontconfig.
            hinting: FALLBACK_HINTING_STYLE,
            ..Default::default()
        }
    }

    fn variations(&self) -> &[FontVariation] {
        &self.variations
    }
}

impl PlatformFont {
    fn new(
        data: FontBacking,
        named_instance_index: u32,
        requested_size: Option<Au>,
        synthetic_bold: bool,
    ) -> Result<PlatformFont, &'static str> {
        let font_ref = data.font_ref().map_err(|_| "Could not read font")?;
        // FT_New_Memory_Face fails without 'head' and 'maxp', and for a face with neither outlines nor strikes.
        if font_ref.head().is_err() ||
            font_ref.maxp().is_err() ||
            !(face::is_scalable(&font_ref) || face::has_strikes(&font_ref))
        {
            return Err("Could not read font");
        }

        let (sizing, requested_face_size, actual_face_size) = match requested_size {
            Some(requested_size) => {
                let (sizing, actual) = face::set_size(&font_ref, requested_size.to_f64_px())?;
                let actual = if face::is_scalable(&font_ref) {
                    requested_size
                } else {
                    Au::from_f64_px(actual)
                };
                (Some(sizing), requested_size, actual)
            },
            None => (None, Au::zero(), Au::zero()),
        };

        // FT_New_Face with a named instance in the upper 16 bits of the face index starts at that instance.
        let location = match named_instance_index.checked_sub(1) {
            Some(instance) => font_ref
                .named_instances()
                .get(instance as usize)
                .map(|instance| instance.location())
                .unwrap_or_else(|| font_ref.axes().location(std::iter::empty::<(Tag, f32)>())),
            None => font_ref.axes().location(std::iter::empty::<(Tag, f32)>()),
        };

        let synthetic_bold = data.should_apply_synthetic_bold(synthetic_bold);
        Ok(PlatformFont {
            data,
            requested_face_size,
            actual_face_size,
            sizing,
            location,
            variations: vec![],
            synthetic_bold,
        })
    }

    /// Find the scale to use for metrics of unscalable fonts. Unscalable fonts, those using bitmap glyphs, are
    /// scaled after glyph rasterization. In order for metrics to match the final scaled font, we need to scale them
    /// based on the final size and the actual font size.
    fn unscalable_font_metrics_scale(&self) -> f64 {
        self.requested_face_size.to_f64_px() / self.actual_face_size.to_f64_px()
    }
}

/// The bytes of a font: shared web font data, or a local file (usually mapped) and the face's index in it.
#[derive(Clone)]
enum FontBacking {
    Web(FontData),
    Local(Arc<FontFileBytes>, u32),
}

impl FontBacking {
    fn font_ref(&self) -> Result<FontRef<'_>, ReadError> {
        match self {
            Self::Web(font_data) => FontRef::new(font_data.as_ref()),
            Self::Local(mmap, index) => FontRef::from_index(mmap, *index),
        }
    }

    fn should_apply_synthetic_bold(&self, synthetic_bold: bool) -> bool {
        // Ensures that a font face is not emboldened if it's a variable font or
        // if it's already bold.
        let face_is_bold = self
            .font_ref()
            .and_then(|font_ref| font_ref.os2())
            .is_ok_and(|table| table.us_weight_class() >= SEMI_BOLD_U16);
        let is_variable_font = self
            .font_ref()
            .and_then(|font_ref| font_ref.fvar())
            .is_ok_and(|table| table.axis_count() > 0);
        !face_is_bold && !is_variable_font && synthetic_bold
    }
}

impl std::fmt::Debug for FontBacking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = match self {
            Self::Web(font_data) => font_data.as_ref().len(),
            Self::Local(mmap, _) => mmap.len(),
        };
        f.debug_struct("FontBacking").field("len", &len).finish()
    }
}
