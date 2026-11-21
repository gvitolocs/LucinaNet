// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What Servo's FreeType `PlatformFont` read from `FT_Face`, read from the font's tables with read-fonts and skrifa:
//! glyph lookup, advances, face and size metrics, `kern` pairs, glyph bounds, variations, bitmap strikes.
//!
//! Each function names the FreeType 2.13.2 code it stands in for. The arithmetic is `lucina_ftcompat`'s; this crate only
//! reads tables. The original (Servo's `components/fonts/platform/freetype` on FreeType) loaded glyphs with
//! `FT_LOAD_TARGET_LIGHT`, so advances are whole pixels; see [`glyph_h_advance`].

use lucina_ftcompat::{self as ft, F26Dot6, FaceTables, Fixed, Hinting, MvarDeltas, Os2Metrics};
use read_fonts::tables::glyf::{CompositeGlyphFlags, Glyph};
use read_fonts::tables::kern::{Kern, Subtable, Subtable0, SubtableKind};
use read_fonts::tables::loca::LocaGlyph;
use read_fonts::types::{GlyphId, Tag};
use read_fonts::{FontRef, TableProvider};
use skrifa::bitmap::{BitmapFormat, BitmapStrikes};
use skrifa::instance::{Location, LocationRef, NormalizedCoord, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::MetadataProvider;

pub use lucina_ftcompat;

/// How the face stores glyphs, as FreeType's sfnt loader decides `FT_FACE_FLAG_SCALABLE` (`sfobjs.c`: an outline table
/// makes the face scalable; a font with only bitmap strikes is not).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outlines {
    Glyf,
    Cff,
    Cff2,
    /// No outlines: bitmap strikes only (CBDT/EBDT/sbix), or nothing usable.
    None,
}

pub fn outlines(font: &FontRef) -> Outlines {
    if font.table_data(Tag::new(b"glyf")).is_some() && font.table_data(Tag::new(b"loca")).is_some()
    {
        Outlines::Glyf
    } else if font.table_data(Tag::new(b"CFF ")).is_some() {
        Outlines::Cff
    } else if font.table_data(Tag::new(b"CFF2")).is_some() {
        Outlines::Cff2
    } else {
        Outlines::None
    }
}

/// `FT_IS_SCALABLE`.
pub fn is_scalable(font: &FontRef) -> bool {
    outlines(font) != Outlines::None
}

/// `FT_HAS_FIXED_SIZES`: the font has bitmap strikes FreeType would load (EBLC/CBLC or sbix).
pub fn has_strikes(font: &FontRef) -> bool {
    !BitmapStrikes::new(font).is_empty()
}

/// `FT_HAS_COLOR`: `sfnt_load_face` sets `FT_FACE_FLAG_COLOR` for sbix, CBDT, COLR+CPAL and SVG tables.
pub fn has_color(font: &FontRef) -> bool {
    let has = |t: &[u8; 4]| font.table_data(Tag::new(t)).is_some();
    has(b"sbix") || has(b"CBDT") || (has(b"COLR") && has(b"CPAL")) || has(b"SVG ")
}

/// `head.unitsPerEm`, 0 when unreadable.
pub fn units_per_em(font: &FontRef) -> u16 {
    font.head().map(|h| h.units_per_em()).unwrap_or(0)
}

/// The size a face is set to: FreeType's `FT_Size_Metrics` after `FT_Set_Char_Size` (scalable) or `FT_Select_Size`
/// (strikes), in 26.6 / 16.16 as FreeType keeps them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sizing {
    pub x_ppem: u16,
    pub y_ppem: u16,
    pub x_scale: Fixed,
    pub y_scale: Fixed,
    pub ascender: F26Dot6,
    pub descender: F26Dot6,
    pub height: F26Dot6,
    pub max_advance: F26Dot6,
    /// The 26.6 size `FT_Set_Char_Size` was given (scalable faces; 0 for strikes). Outlines are drawn at exactly
    /// `char_size / 64` pixels: that is the size FreeType's scale stands for, and the one skrifa turns back into the
    /// same 26.6 value.
    pub char_size: F26Dot6,
    /// The chosen strike for a bitmap-only face.
    pub strike: Option<usize>,
}

/// `FreeTypeFace::set_size` on top of `FT_Set_Char_Size(face, size, 0, 72, 72)` / `FT_Select_Size`. Returns the size
/// metrics and the size the face actually has, in pixels (the requested one for scalable faces, the strike's otherwise).
pub fn set_size(font: &FontRef, requested_px: f64) -> Result<(Sizing, f64), &'static str> {
    if is_scalable(font) {
        let char_size = ft::char_size(requested_px);
        let upem = units_per_em(font);
        let scale = ft::scale_for_char_size(char_size, upem).ok_or("FT_Set_Char_Size failed")?;
        let face = face_metrics(font, &[]);
        let ppem = ft::ppem(char_size) as u16;
        // ft_recompute_scaled_metrics: FT_PIX_CEIL / FT_PIX_FLOOR / FT_PIX_ROUND of the scaled face values.
        let sizing = Sizing {
            x_ppem: ppem,
            y_ppem: ppem,
            x_scale: scale,
            y_scale: scale,
            ascender: ft::pix_ceil(ft::mul_fix(face.ascender as i64, scale)),
            descender: ft::pix_floor(ft::mul_fix(face.descender as i64, scale)),
            height: ft::pix_round(ft::mul_fix(face.height as i64, scale)),
            max_advance: ft::pix_round(ft::mul_fix(face.max_advance_width as i64, scale)),
            char_size,
            strike: None,
        };
        return Ok((sizing, requested_px));
    }
    let strikes = BitmapStrikes::new(font);
    if strikes.is_empty() {
        return Err("No fixed sizes available");
    }
    // Servo's set_size truncates the request to 26.6 here, unlike FT_Set_Char_Size.
    let requested = (requested_px * 64.0) as i64;
    // FT_Bitmap_Size::y_ppem, 26.6, from each strike's metrics as tt_face_load_strike_metrics reads them.
    let sizes: Vec<F26Dot6> = (0..strikes.len())
        .map(|i| strike_metrics(font, &strikes, i).map_or(0, |m| m.y_ppem as i64 * 64))
        .collect();
    let index = ft::choose_strike(&sizes, requested).ok_or("No fixed sizes available")?;
    let sizing = strike_metrics(font, &strikes, index).ok_or("FT_Select_Size failed")?;
    Ok((sizing, sizes[index] as f64 / 64.0))
}

/// `tt_face_load_strike_metrics` (`ttsbit.c`) for EBLC/CBLC and sbix strikes.
fn strike_metrics(font: &FontRef, strikes: &BitmapStrikes, index: usize) -> Option<Sizing> {
    let upem = units_per_em(font) as i64;
    match strikes.format()? {
        BitmapFormat::Sbix => {
            let strike = strikes.get(index)?;
            let ppem = strike.ppem().round() as i64;
            let hhea = font.hhea().ok()?;
            let scale = ft::div_fix(ppem * 64, upem);
            let face = face_metrics(font, &[]);
            Some(Sizing {
                x_ppem: ppem as u16,
                y_ppem: ppem as u16,
                x_scale: scale,
                y_scale: scale,
                ascender: ft::mul_fix(face.ascender as i64, scale),
                descender: ft::mul_fix(face.descender as i64, scale),
                height: ft::mul_fix(face.height as i64, scale),
                max_advance: ft::mul_fix(hhea.advance_width_max().to_u16() as i64, scale),
                char_size: 0,
                strike: Some(index),
            })
        }
        BitmapFormat::Cbdt | BitmapFormat::Ebdt => {
            let data = font
                .table_data(Tag::new(b"CBLC"))
                .or_else(|| font.table_data(Tag::new(b"EBLC")))?;
            let bytes = data.as_bytes();
            let at = 8 + index * 48;
            let strike = bytes.get(at..at + 48)?;
            let x_ppem = strike[44] as i64;
            let y_ppem = strike[45] as i64;
            let mut ascender = (strike[16] as i8 as i64) * 64;
            let mut descender = (strike[17] as i8 as i64) * 64;
            let max_before_bl = strike[24] as i8 as i64;
            let min_after_bl = strike[25] as i8 as i64;
            if descender > 0 {
                if min_after_bl < 0 {
                    descender = -descender;
                }
            } else if descender == 0 && ascender == 0 {
                if max_before_bl != 0 || min_after_bl != 0 {
                    ascender = max_before_bl * 64;
                    descender = min_after_bl * 64;
                } else {
                    ascender = y_ppem * 64;
                    descender = 0;
                }
            }
            let mut height = ascender - descender;
            if height == 0 {
                height = y_ppem * 64;
                descender = ascender - height;
            }
            let max_advance =
                ((strike[22] as i8 as i64) + strike[18] as i64 + (strike[23] as i8 as i64)) * 64;
            Some(Sizing {
                x_ppem: x_ppem as u16,
                y_ppem: y_ppem as u16,
                x_scale: ft::div_fix(x_ppem * 64, upem),
                y_scale: ft::div_fix(y_ppem * 64, upem),
                ascender,
                descender,
                height,
                max_advance,
                char_size: 0,
                strike: Some(index),
            })
        }
    }
}

/// The `FT_FaceRec` metric fields at a variation location: `sfnt_load_face`, then `tt_apply_mvar` when the location is
/// not the default one.
pub fn face_metrics(font: &FontRef, coords: &[NormalizedCoord]) -> ft::FaceMetrics {
    let tables = face_tables(font);
    let base = ft::face_metrics(&tables);
    if coords.iter().all(|c| c.to_bits() == 0) {
        return base;
    }
    let Ok(mvar) = font.mvar() else {
        return base;
    };
    let delta = |tag: &[u8; 4]| -> i16 {
        mvar.metric_delta(Tag::new(tag), coords)
            .map(|d| ft::fixed_to_int(d.to_bits() as i64))
            .unwrap_or(0)
    };
    let deltas = MvarDeltas {
        hasc: delta(b"hasc"),
        hdsc: delta(b"hdsc"),
        hlgp: delta(b"hlgp"),
        undo: delta(b"undo"),
        unds: delta(b"unds"),
    };
    ft::apply_mvar(base, &tables, &deltas)
}

fn face_tables(font: &FontRef) -> FaceTables {
    let mut t = FaceTables::default();
    if let Ok(hhea) = font.hhea() {
        t.hhea_ascender = hhea.ascender().to_i16();
        t.hhea_descender = hhea.descender().to_i16();
        t.hhea_line_gap = hhea.line_gap().to_i16();
        t.hhea_advance_width_max = hhea.advance_width_max().to_u16();
    }
    if let Ok(os2) = font.os2() {
        t.os2 = Some(Os2Metrics {
            typo_ascender: os2.s_typo_ascender(),
            typo_descender: os2.s_typo_descender(),
            typo_line_gap: os2.s_typo_line_gap(),
            win_ascent: os2.us_win_ascent(),
            win_descent: os2.us_win_descent(),
        });
    }
    if let Ok(post) = font.post() {
        t.post_underline_position = post.underline_position().to_i16();
        t.post_underline_thickness = post.underline_thickness().to_i16();
    }
    t
}

/// `FT_Get_Char_Index`: the glyph for a character through the cmap, `None` for glyph 0.
pub fn glyph_index(font: &FontRef, ch: char) -> Option<u32> {
    font.charmap()
        .map(ch)
        .map(|g| g.to_u32())
        .filter(|&g| g != 0)
}

/// A glyph's horizontal advance in 26.6 as `FT_Load_Glyph(FT_LOAD_TARGET_LIGHT [| FT_LOAD_COLOR])` leaves it in
/// `slot->metrics.horiAdvance`, followed by `mozilla_glyphslot_embolden_less` when `synthetic_bold`. `None` where
/// FreeType fails to load the glyph.
///
/// Outline glyphs: the design advance at `coords` (HVAR/gvar applied, rounded to whole units as `tt_hadvance_adjust`
/// does), scaled with `FT_MulFix` and rounded to whole pixels (light hinting). Bitmap glyphs of a bitmap-only face: the
/// strike's advance in whole pixels.
pub fn glyph_h_advance(
    font: &FontRef,
    sizing: &Sizing,
    coords: &[NormalizedCoord],
    glyph: u32,
    hinting: Hinting,
    synthetic_bold: bool,
) -> Option<F26Dot6> {
    let upem = units_per_em(font);
    if let Some(strike) = sizing.strike {
        let advance = bitmap_advance(font, sizing, strike, glyph)?;
        let mut advance = advance * 64;
        if synthetic_bold {
            // FT_GlyphSlot_Embolden for bitmaps: the strength rounded down to whole pixels, at least one.
            let mut xstr = ft::mul_fix(upem as i64, sizing.y_scale) / 24;
            xstr &= !63;
            if xstr == 0 {
                xstr = 64;
            }
            advance += xstr;
        }
        return Some(advance);
    }
    let gid = GlyphId::new(glyph);
    if glyph >= glyph_count(font) {
        return None;
    }
    let metrics = font.glyph_metrics(Size::unscaled(), LocationRef::new(coords));
    let units = metrics.advance_width(metrics_glyph(font, gid))?.round() as i64;
    let mut advance = ft::glyph_advance(units, sizing.x_scale, hinting);
    if synthetic_bold {
        advance += ft::embolden_strength(upem, sizing.y_scale);
    }
    Some(advance)
}

/// The glyph whose metrics a glyph uses: a composite glyph with a `USE_MY_METRICS` component takes that component's
/// phantom points, so its advance (`load_truetype_glyph` in `ttgload.c`: the last flagged component wins, and the rule
/// applies again inside it). FreeType has no fixed depth limit for this; 16 levels here only stop a malformed font's
/// component cycle.
fn metrics_glyph(font: &FontRef, gid: GlyphId) -> GlyphId {
    let (Ok(glyf), Ok(loca)) = (font.glyf(), font.loca(None)) else {
        return gid;
    };
    let mut current = gid;
    for _ in 0..16 {
        let Some(LocaGlyph::Glyph(Glyph::Composite(composite))) = loca.get(current, &glyf) else {
            break;
        };
        let Some(component) = composite
            .components()
            .filter(|c| c.flags.contains(CompositeGlyphFlags::USE_MY_METRICS))
            .last()
        else {
            break;
        };
        current = component.glyph;
    }
    current
}

fn glyph_count(font: &FontRef) -> u32 {
    font.maxp().map(|m| m.num_glyphs() as u32).unwrap_or(0)
}

/// The advance FreeType reads for a bitmap glyph, in whole strike pixels (`tt_sbit_decoder_load_metrics` for
/// EBDT/CBDT; `tt_face_load_sbix_image`'s hmtx-based advance for sbix).
fn bitmap_advance(font: &FontRef, sizing: &Sizing, strike: usize, glyph: u32) -> Option<i64> {
    let strikes = BitmapStrikes::new(font);
    let bitmap = strikes.get(strike)?.get(GlyphId::new(glyph))?;
    match strikes.format()? {
        BitmapFormat::Sbix => {
            let advance = font.hmtx().ok()?.advance(GlyphId::new(glyph))? as i64;
            Some(advance * sizing.x_ppem as i64 / units_per_em(font).max(1) as i64)
        }
        _ => bitmap.advance.map(|a| a.round() as i64),
    }
}

/// `FT_Get_Kerning(..., FT_KERNING_DEFAULT, ...)`.x in 26.6: the `kern` table's format-0 pairs (`tt_face_get_kerning`:
/// horizontal, non-minimum, non-cross-stream subtables; values add, or replace with the override bit), scaled with
/// `FT_MulFix`, scaled down below 25 ppem and rounded to whole pixels.
pub fn glyph_h_kerning(font: &FontRef, sizing: &Sizing, left: u32, right: u32) -> F26Dot6 {
    let units = kern_units(font, left, right);
    if units == 0 {
        return 0;
    }
    let mut x = ft::mul_fix(units, sizing.x_scale);
    if sizing.x_ppem < 25 {
        x = ft::mul_div(x, sizing.x_ppem as i64, 25);
    }
    ft::pix_round(x)
}

fn kern_units(font: &FontRef, left: u32, right: u32) -> i64 {
    // tt_face_load_kern reads only the OpenType (16-bit version) table.
    let Ok(kern @ Kern::Ot(_)) = font.kern() else {
        return 0;
    };
    let (Ok(left), Ok(right)) = (u16::try_from(left), u16::try_from(right)) else {
        return 0;
    };
    let mut value: i64 = 0;
    // tt_face_load_kern keeps at most 32 subtables (one bit each in kern_avail_bits) and makes available only
    // horizontal (coverage bit 0), not minimum (bit 1), not cross-stream (bit 2), format 0 ones.
    for subtable in kern.subtables().take(32).flatten() {
        let Subtable::Ot(ot) = &subtable else {
            continue;
        };
        let coverage = ot.coverage();
        if coverage & 0x0007 != 0x0001 || coverage >> 8 != 0 {
            continue;
        }
        let Ok(SubtableKind::Format0(f0)) = subtable.kind() else {
            continue;
        };
        if let Some(pair) = format0_pair(&f0, left, right) {
            if coverage & 0x0008 != 0 {
                value = pair;
            } else {
                value += pair;
            }
        }
    }
    value
}

fn format0_pair(table: &Subtable0, left: u16, right: u16) -> Option<i64> {
    table
        .kerning(GlyphId::new(left as u32), GlyphId::new(right as u32))
        .map(|v| v as i64)
}

/// The glyph's box as `FT_Load_Glyph(FT_LOAD_DEFAULT | FT_LOAD_NO_HINTING)` reports it in `slot->metrics`
/// (`horiBearingX`, `horiBearingY - height`, `width`, `height`), in 26.6: the control box of the scaled outline, its
/// points rounded to 26.6 as FreeType's loaders store them. Bitmap glyphs: the bitmap's box.
pub fn glyph_box(
    font: &FontRef,
    sizing: &Sizing,
    coords: &[NormalizedCoord],
    glyph: u32,
) -> Option<[F26Dot6; 4]> {
    if let Some(strike) = sizing.strike {
        let strikes = BitmapStrikes::new(font);
        let bitmap = strikes.get(strike)?.get(GlyphId::new(glyph))?;
        let ppem = bitmap.ppem_y.max(1.0);
        let upem = units_per_em(font).max(1) as f32;
        let left = (bitmap.bearing_x * ppem / upem).round() as i64 * 64;
        let top = (bitmap.bearing_y * ppem / upem).round() as i64 * 64;
        let (w, h) = (bitmap.width as i64 * 64, bitmap.height as i64 * 64);
        return Some([left, top - h, w, h]);
    }
    let outline = font.outline_glyphs().get(GlyphId::new(glyph))?;
    let mut pen = ControlBox::default();
    outline
        .draw(
            DrawSettings::unhinted(
                Size::new(sizing.char_size as f32 / 64.0),
                LocationRef::new(coords),
            ),
            &mut pen,
        )
        .ok()?;
    let Some([x0, y0, x1, y1]) = pen.bounds else {
        return Some([0, 0, 0, 0]);
    };
    Some([x0, y0, x1 - x0, y1 - y0])
}

#[derive(Default)]
struct ControlBox {
    bounds: Option<[F26Dot6; 4]>,
}

impl ControlBox {
    fn add(&mut self, x: f32, y: f32) {
        let (x, y) = ((x * 64.0).round() as i64, (y * 64.0).round() as i64);
        self.bounds = Some(match self.bounds {
            None => [x, y, x, y],
            Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
        });
    }
}

impl OutlinePen for ControlBox {
    fn move_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.add(cx0, cy0);
        self.add(x, y);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.add(cx0, cy0);
        self.add(cx1, cy1);
        self.add(x, y);
    }
    fn close(&mut self) {}
}

/// A variation request turned into what `FreeTypeFace::set_variations_for_font` produced: the values Servo reports
/// (clamped to each axis's range, unknown axes dropped, in axis order) and the normalized location FreeType would use
/// after `FT_Set_Var_Design_Coordinates` (design coordinates clamped, then normalized through avar).
pub struct Variations {
    pub reported: Vec<(Tag, f32)>,
    pub location: Location,
}

pub fn variations(font: &FontRef, requested: &[(Tag, f32)]) -> Option<Variations> {
    let axes = font.axes();
    if axes.is_empty() || requested.is_empty() {
        return None;
    }
    let mut reported = Vec::new();
    let mut settings = Vec::new();
    for axis in axes.iter() {
        let Some(&(tag, value)) = requested.iter().find(|(tag, _)| *tag == axis.tag()) else {
            continue;
        };
        // Servo's set_variations_for_font clamps the reported value in f32 from the 16.16 axis limits.
        let (min, max) = (axis.min_value(), axis.max_value());
        reported.push((tag, value.min(max).max(min)));
        // FreeType receives `(value * 65536.0) as FT_Fixed` and clamps to the axis (`TT_Set_Var_Design`).
        let fixed = (value * 65536.0) as i64;
        let clamped = (fixed as f64 / 65536.0).clamp(min as f64, max as f64) as f32;
        settings.push((tag, clamped));
    }
    let location = axes.location(settings.iter().copied());
    Some(Variations { reported, location })
}
