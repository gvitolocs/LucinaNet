/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The unix glyph rasterizer without FreeType. Fonts are read with skrifa (outlines, hinting, embedded bitmaps),
//! coverage comes from zeno (a port of FreeType's `ftgrays` rasterizer), and every box, offset, scale and advance is
//! FreeType 2.13.2's arithmetic (`lucina-ftcompat`), so glyphs keep the metrics and placement the FreeType backend
//! gave them. FreeType was built without `FT_CONFIG_OPTION_SUBPIXEL_RENDERING`, so subpixel text is its "Harmony"
//! renderer: three coverage renders shifted by 21/64 px, no FIR filter.

use api::{ColorU, GlyphDimensions, FontKey, FontRenderMode};
use api::{FontInstancePlatformOptions, FontHinting};
use api::{FontInstanceFlags, FontTemplate, FontVariation, NativeFontHandle};
use crate::gamma_lut::{ColorLut, GammaLut};
use crate::rasterizer::{FontInstance, FontTransform, GlyphFormat, GlyphKey};
use crate::rasterizer::{GlyphRasterError, GlyphRasterResult, RasterizedGlyph};
use crate::types::FastHashMap;
use lucina_ftcompat as ftc;
use lucina_ftcompat::{F26Dot6, Fixed};
use skrifa::bitmap::{BitmapData, BitmapFormat, BitmapStrikes};
use skrifa::instance::{LocationRef, NormalizedCoord, Size};
use skrifa::outline::{DrawSettings, Engine, GlyphStyles, HintingInstance, HintingOptions};
use skrifa::outline::{OutlineGlyphFormat, OutlinePen, SmoothMode, Target};
use skrifa::raw::types::{F2Dot14, Fixed as RawFixed};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider, Tag};
use std::cmp::max;
use std::fs::File;
use std::mem;
use std::sync::{Arc, Mutex, MutexGuard};

/// How many hinting instances one font keeps. Building one runs the font's `fpgm`/`prep` or the autohinter's
/// style analysis, so instances are reused across glyphs; a page rarely uses more than a few sizes of one face.
const MAX_HINTERS_PER_FONT: usize = 8;

/// `FT_Render_Mode` / `FT_LOAD_TARGET_*` of a glyph load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoadTarget {
    /// `FT_LOAD_NO_HINTING`.
    NoHinting,
    Normal,
    Light,
    Mono,
    Lcd,
    LcdV,
}

/// The hinter FreeType's `FT_Load_Glyph` picks for a load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HintEngine {
    None,
    /// The autohinter (`af_loader_load_glyph`).
    Auto,
    /// The font driver's own hinter: the TrueType bytecode interpreter or the CFF hinter.
    Native,
}

/// FreeType's outline point tags (`FT_CURVE_TAG_*`).
const TAG_CONIC: u8 = 0;
const TAG_ON: u8 = 1;
const TAG_CUBIC: u8 = 2;

/// An `FT_Outline`: 26.6 points, their tags and the index of each contour's last point.
#[derive(Clone, Debug, Default, PartialEq)]
struct Outline {
    points: Vec<(F26Dot6, F26Dot6)>,
    tags: Vec<u8>,
    contours: Vec<usize>,
}

/// `FT_BBox`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BBox {
    x_min: F26Dot6,
    y_min: F26Dot6,
    x_max: F26Dot6,
    y_max: F26Dot6,
}

impl Outline {
    /// `FT_Outline_Get_CBox`: the box of all points, control points included; zero for an empty outline.
    fn cbox(&self) -> BBox {
        let mut points = self.points.iter();
        let Some(&(x, y)) = points.next() else {
            return BBox::default();
        };
        let mut b = BBox { x_min: x, y_min: y, x_max: x, y_max: y };
        for &(x, y) in points {
            b.x_min = b.x_min.min(x);
            b.x_max = b.x_max.max(x);
            b.y_min = b.y_min.min(y);
            b.y_max = b.y_max.max(y);
        }
        b
    }

    /// `FT_Outline_Translate`.
    fn translate(&mut self, dx: F26Dot6, dy: F26Dot6) {
        if dx == 0 && dy == 0 {
            return;
        }
        for p in &mut self.points {
            p.0 += dx;
            p.1 += dy;
        }
    }

    /// `FT_Outline_Transform` with a 16.16 matrix.
    fn transform(&mut self, m: &Matrix) {
        for p in &mut self.points {
            let (x, y) = *p;
            *p = (
                ftc::mul_fix(x, m.xx) + ftc::mul_fix(y, m.xy),
                ftc::mul_fix(x, m.yx) + ftc::mul_fix(y, m.yy),
            );
        }
    }

    /// The outline as path commands, following `FT_Outline_Decompose` (contours may start on a conic control point,
    /// consecutive conic points imply on-curve midpoints, computed in 26.6 with C division). Point `(x, y)` maps to
    /// `(x * scale / 64, y * scale / 64)`. A contour FreeType rejects as invalid ends the path there.
    fn to_commands(&self, scale: i64, commands: &mut Vec<zeno::Command>) {
        use zeno::Command::*;
        commands.clear();
        let pt = |(x, y): (i64, i64)| zeno::Point::new((x * scale) as f32 / 64.0, (y * scale) as f32 / 64.0);
        let mid = |a: (i64, i64), b: (i64, i64)| ((a.0 + b.0) / 2, (a.1 + b.1) / 2);
        let mut first = 0;
        for &last in &self.contours {
            if last < first || last >= self.points.len() {
                return;
            }
            let mut v_start = self.points[first];
            let v_last = self.points[last];
            let mut limit = last as isize;
            // Index of the current point; the loop steps to the next one first.
            let mut cur = first as isize;
            match self.tags[first] {
                // A contour cannot start with a cubic control point.
                TAG_CUBIC => return,
                TAG_CONIC => {
                    if self.tags[last] == TAG_ON {
                        // Start at the last point if it is on the curve.
                        v_start = v_last;
                        limit -= 1;
                    } else {
                        // Both first and last points are conic: start at their middle.
                        v_start = mid(v_start, v_last);
                    }
                    cur -= 1;
                }
                _ => {}
            }
            commands.push(MoveTo(pt(v_start)));
            let mut closed = false;
            'contour: while cur < limit {
                cur += 1;
                let i = cur as usize;
                match self.tags[i] {
                    TAG_ON => commands.push(LineTo(pt(self.points[i]))),
                    TAG_CONIC => {
                        let mut v_control = self.points[i];
                        loop {
                            if cur < limit {
                                cur += 1;
                                let j = cur as usize;
                                let vec = self.points[j];
                                match self.tags[j] {
                                    TAG_ON => {
                                        commands.push(QuadTo(pt(v_control), pt(vec)));
                                        continue 'contour;
                                    }
                                    TAG_CONIC => {
                                        commands.push(QuadTo(pt(v_control), pt(mid(v_control, vec))));
                                        v_control = vec;
                                    }
                                    _ => return,
                                }
                            } else {
                                commands.push(QuadTo(pt(v_control), pt(v_start)));
                                closed = true;
                                break 'contour;
                            }
                        }
                    }
                    _ => {
                        if cur + 1 > limit || self.tags[i + 1] != TAG_CUBIC {
                            return;
                        }
                        cur += 2;
                        let (vec1, vec2) = (self.points[i], self.points[i + 1]);
                        if cur <= limit {
                            commands.push(CurveTo(pt(vec1), pt(vec2), pt(self.points[cur as usize])));
                        } else {
                            commands.push(CurveTo(pt(vec1), pt(vec2), pt(v_start)));
                            closed = true;
                            break 'contour;
                        }
                    }
                }
            }
            if !closed {
                // Close the contour with a line segment.
                commands.push(LineTo(pt(v_start)));
            }
            commands.push(Close);
            first = last + 1;
        }
    }
}

/// Collects skrifa's path into a FreeType-style [`Outline`]: coordinates rounded to 26.6 (x scaled by `x_ratio`
/// first), control points kept with their tags. skrifa closes each contour with a segment back to its start; that
/// repeated start point is dropped, since FreeType contours close implicitly. Implied on-curve midpoints of
/// TrueType quadratics appear as explicit on-curve points.
struct OutlineCollector<'a> {
    outline: &'a mut Outline,
    x_ratio: f64,
    start: usize,
    open: bool,
}

impl OutlineCollector<'_> {
    fn push(&mut self, x: f32, y: f32, tag: u8) {
        let x = (x as f64 * self.x_ratio * 64.0).round() as i64;
        let y = (y as f64 * 64.0).round() as i64;
        self.outline.points.push((x, y));
        self.outline.tags.push(tag);
    }

    fn finish(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        let o = &mut *self.outline;
        let last = o.points.len() - 1;
        if last > self.start && o.tags[last] == TAG_ON && o.points[last] == o.points[self.start] {
            o.points.pop();
            o.tags.pop();
        }
        self.drop_implied_points();
        let o = &mut *self.outline;
        o.contours.push(o.points.len() - 1);
    }

    /// Drops the on-curve points skrifa inserted between two conic control points, where a TrueType contour only
    /// implies them. FreeType's outline has no such points: emboldening moves the control points and
    /// `FT_Outline_Decompose` derives the midpoints afterwards, which [`Outline::to_commands`] does too. A point
    /// counts as implied when it is the midpoint of its neighbours to within the 26.6 rounding of the three.
    fn drop_implied_points(&mut self) {
        const IMPLIED: u8 = 0xFF;
        let o = &mut *self.outline;
        let (start, n) = (self.start, o.points.len() - self.start);
        if n < 3 {
            return;
        }
        let tolerance = if self.x_ratio == 1.0 { 1 } else { 2 };
        for i in 0 .. n {
            let (prev, next) = (start + (i + n - 1) % n, start + (i + 1) % n);
            let i = start + i;
            if o.tags[i] == TAG_ON && o.tags[prev] == TAG_CONIC && o.tags[next] == TAG_CONIC {
                let (p, a, b) = (o.points[i], o.points[prev], o.points[next]);
                if (2 * p.0 - a.0 - b.0).abs() <= tolerance && (2 * p.1 - a.1 - b.1).abs() <= tolerance {
                    o.tags[i] = IMPLIED;
                }
            }
        }
        let mut w = start;
        for r in start .. start + n {
            if o.tags[r] != IMPLIED {
                o.points[w] = o.points[r];
                o.tags[w] = o.tags[r];
                w += 1;
            }
        }
        o.points.truncate(w);
        o.tags.truncate(w);
    }
}

impl OutlinePen for OutlineCollector<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.finish();
        self.start = self.outline.points.len();
        self.open = true;
        self.push(x, y, TAG_ON);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.push(x, y, TAG_ON);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.push(cx0, cy0, TAG_CONIC);
        self.push(x, y, TAG_ON);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.push(cx0, cy0, TAG_CUBIC);
        self.push(cx1, cy1, TAG_CUBIC);
        self.push(x, y, TAG_ON);
    }

    fn close(&mut self) {
        self.finish();
    }
}

/// `FT_Matrix`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Matrix {
    xx: Fixed,
    xy: Fixed,
    yx: Fixed,
    yy: Fixed,
}

impl Matrix {
    const IDENTITY: Matrix = Matrix { xx: 0x10000, xy: 0, yx: 0, yy: 0x10000 };
}

/// `FT_Pixel_Mode` of a glyph bitmap. Mono bitmaps are kept one byte per pixel (0 or 0xFF).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PixelMode {
    Mono,
    Gray,
    /// Three bytes (R, G, B) per pixel; `width` counts bytes, as `FT_Bitmap::width` does.
    Lcd,
    /// Three rows (R, G, B) per pixel row; `rows` counts them, as `FT_Bitmap::rows` does.
    LcdV,
    /// Premultiplied BGRA.
    Bgra,
}

/// The rendered `FT_GlyphSlot::bitmap` with `bitmap_left` and `bitmap_top`.
#[derive(Clone, Debug)]
struct GlyphBitmap {
    mode: PixelMode,
    width: usize,
    rows: usize,
    buffer: Vec<u8>,
    left: i32,
    top: i32,
}

impl GlyphBitmap {
    fn empty(mode: PixelMode) -> Self {
        GlyphBitmap { mode, width: 0, rows: 0, buffer: Vec::new(), left: 0, top: 0 }
    }

    fn bytes_per_pixel(&self) -> usize {
        if self.mode == PixelMode::Bgra { 4 } else { 1 }
    }
}

/// A loaded glyph slot.
enum LoadedGlyph {
    Outline {
        outline: Outline,
        /// `slot->metrics.horiAdvance`, 26.6.
        advance: F26Dot6,
        /// `FT_OUTLINE_OVERLAP`: the glyph's `glyf` flags mark overlapping contours.
        overlap: bool,
    },
    Bitmap {
        bitmap: GlyphBitmap,
        advance: F26Dot6,
    },
}

/// A variation axis, `FT_Var_Axis` in 16.16.
#[derive(Clone, Copy, Debug)]
struct VarAxis {
    tag: u32,
    minimum: Fixed,
    def: Fixed,
    maximum: Fixed,
}

/// One entry of `FT_Face::available_sizes`: `index` is the strike in the bitmap table, ppems are in 26.6.
#[derive(Clone, Copy, Debug)]
struct Strike {
    index: usize,
    x_ppem: F26Dot6,
    y_ppem: F26Dot6,
}

/// The face-wide facts the glyph loads need, read once.
struct Face {
    units_per_em: u16,
    /// `FT_FACE_FLAG_SCALABLE`.
    scalable: bool,
    outline_format: Option<OutlineGlyphFormat>,
    /// False for a TrueType font without any instructions, which FreeType autohints even with native hinting.
    has_instructions: bool,
    axes: Vec<VarAxis>,
    /// `FT_FACE_FLAG_FIXED_SIZES` when not empty.
    strikes: Vec<Strike>,
    strike_format: Option<BitmapFormat>,
    /// Number of glyphs (`maxp`), as FreeType validates glyph indices.
    num_glyphs: u32,
}

/// Font bytes: a memory-mapped file or bytes WebRender was given.
enum FontBytes {
    Raw(Arc<Vec<u8>>),
    Mapped(memmap2::Mmap),
}

impl FontBytes {
    fn as_slice(&self) -> &[u8] {
        match self {
            FontBytes::Raw(bytes) => bytes,
            FontBytes::Mapped(map) => map,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct HinterKey {
    char_size: F26Dot6,
    coords: Vec<NormalizedCoord>,
    target: Target,
    auto: bool,
}

struct CachedFont {
    template: FontTemplate,
    bytes: Option<FontBytes>,
    index: u32,
    /// `None` when skrifa cannot read the font; glyph loads then fail.
    face: Option<Face>,
    variations: Vec<FontVariation>,
    coords: Vec<NormalizedCoord>,
    hinters: Vec<(HinterKey, HintingInstance)>,
    glyph_styles: Option<GlyphStyles>,
}

impl CachedFont {
    fn font_ref(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(self.bytes.as_ref()?.as_slice(), self.index).ok()
    }
}

struct FontCache {
    // Maps a template to a cached font that may be used across all threads.
    fonts: FastHashMap<FontTemplate, Arc<Mutex<CachedFont>>>,
}

fn read_face(font: &FontRef) -> Option<Face> {
    let units_per_em = font.head().ok()?.units_per_em();
    let has_glyf = font.table_data(Tag::new(b"glyf")).is_some();
    let has_cff = font.table_data(Tag::new(b"CFF ")).is_some() || font.table_data(Tag::new(b"CFF2")).is_some();
    let outline_format = font.outline_glyphs().format();
    let num_glyphs = font.maxp().map(|m| m.num_glyphs() as u32).unwrap_or(0);

    // `tt_face_load_sbit`: CBLC, then EBLC, then sbix. Strikes without a ppem are skipped (`sfnt_load_face`).
    let mut strikes = Vec::new();
    let mut strike_format = None;
    if let (Ok(cblc), true) = (font.cblc(), font.cbdt().is_ok()) {
        strike_format = Some(BitmapFormat::Cbdt);
        for (index, size) in cblc.bitmap_sizes().iter().enumerate() {
            strikes.push(Strike { index, x_ppem: (size.ppem_x() as i64) << 6, y_ppem: (size.ppem_y() as i64) << 6 });
        }
    } else if let (Ok(eblc), true) = (font.eblc(), font.ebdt().is_ok()) {
        strike_format = Some(BitmapFormat::Ebdt);
        for (index, size) in eblc.bitmap_sizes().iter().enumerate() {
            strikes.push(Strike { index, x_ppem: (size.ppem_x() as i64) << 6, y_ppem: (size.ppem_y() as i64) << 6 });
        }
    } else if let Ok(sbix) = font.sbix() {
        strike_format = Some(BitmapFormat::Sbix);
        for (index, strike) in sbix.strikes().iter().enumerate() {
            if let Ok(strike) = strike {
                let ppem = (strike.ppem() as i64) << 6;
                strikes.push(Strike { index, x_ppem: ppem, y_ppem: ppem });
            }
        }
    }
    strikes.retain(|s| s.x_ppem != 0 && s.y_ppem != 0);
    if strikes.is_empty() {
        strike_format = None;
    }
    // `sfnt_load_face`: a font with outlines is scalable, and so is one with neither outlines nor bitmaps.
    let scalable = has_glyf || has_cff || strikes.is_empty();

    let has_instructions = if has_glyf {
        let max_instructions = font.maxp().ok().and_then(|m| m.max_size_of_instructions()).unwrap_or(0);
        let fpgm = font.table_data(Tag::new(b"fpgm")).map_or(0, |d| d.len());
        let prep = font.table_data(Tag::new(b"prep")).map_or(0, |d| d.len());
        max_instructions != 0 || fpgm != 0 || prep != 0
    } else {
        true
    };

    let axes = match font.fvar() {
        Ok(fvar) => fvar
            .axes()
            .map(|axes| {
                axes.iter()
                    .map(|a| VarAxis {
                        tag: u32::from_be_bytes(a.axis_tag().to_be_bytes()),
                        minimum: a.min_value().to_bits() as Fixed,
                        def: a.default_value().to_bits() as Fixed,
                        maximum: a.max_value().to_bits() as Fixed,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    Some(Face {
        units_per_em,
        scalable,
        outline_format,
        has_instructions,
        axes,
        strikes,
        strike_format,
        num_glyphs,
    })
}

impl FontCache {
    fn new() -> Self {
        FontCache { fonts: FastHashMap::default() }
    }

    /// Opens and parses the font of `template` (`FT_New_Face` / `FT_New_Memory_Face`).
    fn open_font(template: &FontTemplate) -> Result<(FontBytes, Face, usize), String> {
        let (bytes, index) = match template {
            FontTemplate::Raw(bytes, index) => (FontBytes::Raw(bytes.clone()), *index),
            FontTemplate::Native(NativeFontHandle { path, index }) => {
                // The map is read-only and the font files are not modified while they are in use, as with
                // FreeType's own mapping of the file in `FT_New_Face`.
                let map = File::open(path).and_then(|file| unsafe { memmap2::Mmap::map(&file) });
                match map {
                    Ok(map) => (FontBytes::Mapped(map), *index),
                    Err(err) => return Err(format!("file={:?} err={}", path, err)),
                }
            }
        };
        let font = match FontRef::from_index(bytes.as_slice(), index) {
            Ok(font) => font,
            Err(err) => return Err(format!("{} index={} err={}", template_name(template), index, err)),
        };
        let Some(face) = read_face(&font) else {
            return Err(format!("{} index={} err=no head table", template_name(template), index));
        };
        let num_axes = font.axes().len();
        Ok((bytes, face, num_axes))
    }

    /// Returns the cached font for `template`, reading it on first use. The FreeType backend panicked when
    /// FreeType could not open a font; such a font is now logged and cached without a face, so the error is
    /// reported once and its glyph loads fail.
    fn add_font(&mut self, template: FontTemplate) -> Arc<Mutex<CachedFont>> {
        if let Some(cached) = self.fonts.get(&template) {
            return cached.clone();
        }
        let index = match template {
            FontTemplate::Raw(_, index) => index,
            FontTemplate::Native(NativeFontHandle { index, .. }) => index,
        };
        let mut cached = CachedFont {
            template: template.clone(),
            bytes: None,
            index,
            face: None,
            variations: Vec::new(),
            coords: Vec::new(),
            hinters: Vec::new(),
            glyph_styles: None,
        };
        match Self::open_font(&template) {
            Ok((bytes, face, num_axes)) => {
                cached.bytes = Some(bytes);
                cached.face = Some(face);
                cached.coords = vec![NormalizedCoord::default(); num_axes];
            }
            Err(err) => error!("adding font failed: {}", err),
        }
        let cached = Arc::new(Mutex::new(cached));
        self.fonts.insert(template, cached.clone());
        cached
    }

    fn delete_font(&mut self, cached: Arc<Mutex<CachedFont>>) {
        self.fonts.remove(&cached.lock().unwrap().template);
    }
}

fn template_name(template: &FontTemplate) -> String {
    match template {
        FontTemplate::Raw(bytes, _) => format!("<{} bytes>", bytes.len()),
        FontTemplate::Native(handle) => handle.path.to_string_lossy().into_owned(),
    }
}

lazy_static! {
    static ref FONT_CACHE: Mutex<FontCache> = Mutex::new(FontCache::new());
}

pub struct FontContext {
    fonts: FastHashMap<FontKey, Arc<Mutex<CachedFont>>>,
    gamma_luts: FastHashMap<(i16, i16), GammaLut>,
    scratch: zeno::Scratch,
    commands: Vec<zeno::Command>,
}

fn get_skew_bounds(bottom: i32, top: i32, skew_factor: f32, _vertical: bool) -> (f32, f32) {
    let skew_min = (bottom as f32 + 0.5) * skew_factor;
    let skew_max = (top as f32 - 0.5) * skew_factor;
    // Negative skew factor may switch the sense of skew_min and skew_max.
    (skew_min.min(skew_max).floor(), skew_min.max(skew_max).ceil())
}

fn skew_bitmap(
    bitmap: &[u8],
    width: usize,
    height: usize,
    left: i32,
    top: i32,
    skew_factor: f32,
    vertical: bool, // Unused: bitmaps are skewed horizontally only, as the FreeType backend did.
) -> (Vec<u8>, usize, i32) {
    let stride = width * 4;
    // Calculate the skewed horizontal offsets of the bottom and top of the glyph.
    let (skew_min, skew_max) = get_skew_bounds(top - height as i32, top, skew_factor, vertical);
    // Allocate enough extra width for the min/max skew offsets.
    let skew_width = width + (skew_max - skew_min) as usize;
    let mut skew_buffer = vec![0u8; skew_width * height * 4];
    for y in 0 .. height {
        // Calculate a skew offset at the vertical center of the current row.
        let offset = (top as f32 - y as f32 - 0.5) * skew_factor - skew_min;
        // Get a blend factor in 0..256 constant across all pixels in the row.
        let blend = (offset.fract() * 256.0) as u32;
        let src_row = y * stride;
        let dest_row = (y * skew_width + offset.floor() as usize) * 4;
        let mut prev_px = [0u32; 4];
        for (src, dest) in
            bitmap[src_row .. src_row + stride].chunks(4).zip(
                skew_buffer[dest_row .. dest_row + stride].chunks_mut(4)
            ) {
            let px = [src[0] as u32, src[1] as u32, src[2] as u32, src[3] as u32];
            // Blend current pixel with previous pixel based on blend factor.
            let next_px = [px[0] * blend, px[1] * blend, px[2] * blend, px[3] * blend];
            dest[0] = ((((px[0] << 8) - next_px[0]) + prev_px[0] + 128) >> 8) as u8;
            dest[1] = ((((px[1] << 8) - next_px[1]) + prev_px[1] + 128) >> 8) as u8;
            dest[2] = ((((px[2] << 8) - next_px[2]) + prev_px[2] + 128) >> 8) as u8;
            dest[3] = ((((px[3] << 8) - next_px[3]) + prev_px[3] + 128) >> 8) as u8;
            // Save the remainder for blending onto the next pixel.
            prev_px = next_px;
        }
        // If the skew misaligns the final pixel, write out the remainder.
        if blend > 0 {
            let dest = &mut skew_buffer[dest_row + stride .. dest_row + stride + 4];
            dest[0] = ((prev_px[0] + 128) >> 8) as u8;
            dest[1] = ((prev_px[1] + 128) >> 8) as u8;
            dest[2] = ((prev_px[2] + 128) >> 8) as u8;
            dest[3] = ((prev_px[3] + 128) >> 8) as u8;
        }
    }
    (skew_buffer, skew_width, left + skew_min as i32)
}

fn transpose_bitmap(bitmap: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut transposed = vec![0u8; width * height * 4];
    for (y, row) in bitmap.chunks(width * 4).enumerate() {
        let mut offset = y * 4;
        for src in row.chunks(4) {
            transposed[offset .. offset + 4].copy_from_slice(src);
            offset += height * 4;
        }
    }
    transposed
}

fn flip_bitmap_x(bitmap: &mut [u8], width: usize, height: usize) {
    assert!(bitmap.len() == width * height * 4);
    for row in bitmap.chunks_mut(width * 4) {
        // Reverse the order of the 4-byte pixels in the row.
        for x in 0 .. width / 2 {
            let (a, b) = (x * 4, (width - 1 - x) * 4);
            for c in 0 .. 4 {
                row.swap(a + c, b + c);
            }
        }
    }
}

fn flip_bitmap_y(bitmap: &mut [u8], width: usize, height: usize) {
    assert!(bitmap.len() == width * height * 4);
    let stride = width * 4;
    for y in 0 .. height / 2 {
        let (low, high) = bitmap.split_at_mut((height - 1 - y) * stride);
        low[y * stride .. (y + 1) * stride].swap_with_slice(&mut high[.. stride]);
    }
}

/// The 16.16 design coordinates of `FT_Set_Var_Design_Coordinates` for `variations` (old `load_glyph`), normalized
/// through fvar and avar to skrifa's coordinates.
fn normalized_coords(font: &FontRef, axes: &[VarAxis], variations: &[FontVariation], out: &mut Vec<NormalizedCoord>) {
    let settings: Vec<(Tag, RawFixed)> = axes
        .iter()
        .map(|axis| {
            let mut value = axis.def;
            if let Some(var) = variations.iter().find(|var| var.tag == axis.tag) {
                value = (var.value * 65536.0 + 0.5) as Fixed;
                value = value.min(axis.maximum).max(axis.minimum);
            }
            (Tag::from_u32(axis.tag), RawFixed::from_bits(value as i32))
        })
        .collect();
    out.clear();
    out.resize(axes.len(), F2Dot14::ZERO);
    if let Ok(fvar) = font.fvar() {
        let avar = font.avar().ok();
        fvar.user_to_normalized(avar.as_ref(), settings, out);
    }
}

/// `FT_Match_Size` as `tt_size_request` uses it: the strike whose rounded ppems equal the rounded request.
fn match_strike(strikes: &[Strike], char_w: F26Dot6, char_h: F26Dot6) -> Option<Strike> {
    let (w, h) = (ftc::pix_round(char_w), ftc::pix_round(char_h));
    if w == 0 || h == 0 {
        return None;
    }
    strikes.iter().copied().find(|s| h == ftc::pix_round(s.y_ppem) && w == ftc::pix_round(s.x_ppem))
}

/// Un-premultiplied RGBA to premultiplied BGRA, as `premultiply_data` in FreeType's `pngshim.c`.
fn premultiply_rgba_to_bgra(rgba: &mut [u8]) {
    fn multiply_alpha(alpha: u32, color: u32) -> u8 {
        let temp = alpha * color + 0x80;
        ((temp + (temp >> 8)) >> 8) as u8
    }
    for px in rgba.chunks_exact_mut(4) {
        let (r, g, b, a) = (px[0] as u32, px[1] as u32, px[2] as u32, px[3] as u32);
        if a == 0 {
            px.copy_from_slice(&[0, 0, 0, 0]);
        } else if a == 0xFF {
            px.copy_from_slice(&[b as u8, g as u8, r as u8, 0xFF]);
        } else {
            px.copy_from_slice(&[multiply_alpha(a, b), multiply_alpha(a, g), multiply_alpha(a, r), a as u8]);
        }
    }
}

/// Decodes PNG glyph data to premultiplied BGRA the way `Load_SBit_Png` sets up libpng: palette and gray expanded
/// to RGB, tRNS to alpha, 16-bit stripped, RGB filled with opaque alpha.
fn decode_png(data: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let mut decoder = png::Decoder::new(data);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = (reader.info().width as usize, reader.info().height as usize);
    if width > 0x7FFF || height > 0x7FFF {
        return None;
    }
    let mut buf = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf).ok()?;
    if frame.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let src = &buf[.. frame.buffer_size()];
    let line = frame.line_size;
    let mut rgba = vec![0u8; width * height * 4];
    for y in 0 .. height {
        let row = &src[y * line .. (y + 1) * line];
        let out = &mut rgba[y * width * 4 .. (y + 1) * width * 4];
        for x in 0 .. width {
            let px = match frame.color_type {
                png::ColorType::Rgba => [row[x * 4], row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3]],
                png::ColorType::Rgb => [row[x * 3], row[x * 3 + 1], row[x * 3 + 2], 0xFF],
                png::ColorType::GrayscaleAlpha => [row[x * 2], row[x * 2], row[x * 2], row[x * 2 + 1]],
                png::ColorType::Grayscale => [row[x], row[x], row[x], 0xFF],
                png::ColorType::Indexed => return None,
            };
            out[x * 4 .. x * 4 + 4].copy_from_slice(&px);
        }
    }
    premultiply_rgba_to_bgra(&mut rgba);
    Some((width, height, rgba))
}

/// `FT_Bitmap_Embolden` for a gray or mono bitmap (one byte per pixel; mono pixels are 0 or 0xFF), by whole
/// pixels: rows are added on top, columns on the right, each pixel accumulates the `xstr` pixels before it and every
/// row is or'ed into the `ystr` rows above it.
fn embolden_bitmap(bitmap: &mut GlyphBitmap, xstr: usize, ystr: usize) {
    let mono = bitmap.mode == PixelMode::Mono;
    let xstr = if mono { xstr.min(8) } else { xstr };
    let (width, rows) = (bitmap.width, bitmap.rows);
    let pitch = width + xstr;
    let mut buffer = vec![0u8; pitch * (rows + ystr)];
    for y in 0 .. rows {
        let dst = (y + ystr) * pitch;
        buffer[dst .. dst + width].copy_from_slice(&bitmap.buffer[y * width .. (y + 1) * width]);
    }
    for y in 0 .. rows {
        let row = (y + ystr) * pitch;
        for x in (0 .. pitch).rev() {
            for i in 1 ..= xstr {
                if x < i {
                    break;
                }
                let (cur, prev) = (buffer[row + x], buffer[row + x - i]);
                if mono {
                    buffer[row + x] = cur | prev;
                } else if cur as u32 + prev as u32 > 255 {
                    buffer[row + x] = 255;
                    break;
                } else {
                    buffer[row + x] = cur + prev;
                    if buffer[row + x] == 255 {
                        break;
                    }
                }
            }
        }
        for k in 1 ..= ystr {
            let above = row - k * pitch;
            for i in 0 .. pitch {
                buffer[above + i] |= buffer[row + i];
            }
        }
    }
    bitmap.buffer = buffer;
    bitmap.width = pitch;
    bitmap.rows = rows + ystr;
}

/// `ft_glyphslot_preset_bitmap` for an outline: the bitmap's pixel box `(left, top, width, rows)` for `mode`, with
/// `ft_lcd_padding` for the Harmony LCD geometry (`ft_smooth_init`: subpixels at -21, 0 and +21 / 64 px). `width`
/// and `rows` count `FT_Bitmap` units (tripled for LCD and LCD_V). `None` for a box beyond FreeType's 16-bit
/// limits, where rendering fails with `Raster_Overflow`.
fn preset_bitmap(cbox: BBox, mode: PixelMode) -> Option<(i64, i64, i64, i64)> {
    // The arithmetic is lucina-ftcompat's, proved by Verus there (no overflow, None exactly on FreeType's range check,
    // width and rows <= 3 * 0x10000). BGRA is sized as gray, as FreeType does.
    let mode = match mode {
        PixelMode::Mono => ftc::PresetMode::Mono,
        PixelMode::Gray | PixelMode::Bgra => ftc::PresetMode::Gray,
        PixelMode::Lcd => ftc::PresetMode::Lcd,
        PixelMode::LcdV => ftc::PresetMode::LcdV,
    };
    ftc::preset_bitmap(cbox.x_min, cbox.y_min, cbox.x_max, cbox.y_max, mode)
}

impl FontContext {
    pub fn distribute_across_threads() -> bool {
        false
    }

    pub fn new() -> FontContext {
        FontContext {
            fonts: FastHashMap::default(),
            gamma_luts: FastHashMap::default(),
            scratch: zeno::Scratch::new(),
            commands: Vec::new(),
        }
    }

    pub fn add_raw_font(&mut self, font_key: &FontKey, bytes: Arc<Vec<u8>>, index: u32) {
        if !self.fonts.contains_key(font_key) {
            let font = FONT_CACHE.lock().unwrap().add_font(FontTemplate::Raw(bytes, index));
            self.fonts.insert(*font_key, font);
        }
    }

    pub fn add_native_font(&mut self, font_key: &FontKey, native_font_handle: NativeFontHandle) {
        if !self.fonts.contains_key(font_key) {
            let font = FONT_CACHE.lock().unwrap().add_font(FontTemplate::Native(native_font_handle));
            self.fonts.insert(*font_key, font);
        }
    }

    /// Returns whether the font described by `template` actually contains
    /// embedded bitmap strikes (FreeType `FT_FACE_FLAG_FIXED_SIZES`). The
    /// `EMBEDDED_BITMAPS` instance flag is unreliable here because on unix it
    /// only reflects a fontconfig preference and is set even for ordinary
    /// outline fonts, so anything that needs to know whether a glyph will really
    /// rasterize to a bitmap must consult the face itself. The face is loaded
    /// into the shared cache that the rasterizing worker threads reuse, so this
    /// does not duplicate work.
    pub fn has_bitmap_strikes(template: &FontTemplate) -> bool {
        let cached = FONT_CACHE.lock().unwrap().add_font(template.clone());
        let cached = cached.lock().unwrap();
        cached.face.as_ref().is_some_and(|face| !face.strikes.is_empty())
    }

    pub fn delete_font(&mut self, font_key: &FontKey) {
        if let Some(cached) = self.fonts.remove(font_key) {
            // If the only references to this font are the FontCache and this FontContext,
            // then delete the font as there are no other existing users.
            if Arc::strong_count(&cached) <= 2 {
                FONT_CACHE.lock().unwrap().delete_font(cached);
            }
        }
    }

    pub fn delete_font_instance(&mut self, _instance: &FontInstance) {
    }

    /// The shape matrix and delta the FreeType backend passed to `FT_Set_Transform`.
    fn shape_transform(font: &FontInstance, x_scale: f64, y_scale: f64, req_size: f64) -> (Matrix, (F26Dot6, F26Dot6)) {
        let mut shape: FontTransform = font.transform.invert_scale(x_scale, y_scale);
        if font.flags.contains(FontInstanceFlags::FLIP_X) {
            shape = shape.flip_x();
        }
        if font.flags.contains(FontInstanceFlags::FLIP_Y) {
            shape = shape.flip_y();
        }
        if font.flags.contains(FontInstanceFlags::TRANSPOSE) {
            shape = shape.swap_xy();
        }
        let (mut tx, mut ty) = (0.0, 0.0);
        if font.synthetic_italics.is_enabled() {
            let (shape_, (tx_, ty_)) = font.synthesize_italics(shape, y_scale * req_size);
            shape = shape_;
            tx = tx_;
            ty = ty_;
        };
        let matrix = Matrix {
            xx: (shape.scale_x * 65536.0) as Fixed,
            xy: (shape.skew_x * -65536.0) as Fixed,
            yx: (shape.skew_y * -65536.0) as Fixed,
            yy: (shape.scale_y * 65536.0) as Fixed,
        };
        // The delta vector for FT_Set_Transform is in units of 1/64 pixel.
        let delta = ((tx * 64.0) as F26Dot6, (ty * -64.0) as F26Dot6);
        (matrix, delta)
    }

    /// `FT_Load_Glyph` with the FreeType backend's flags, followed by `mozilla_glyphslot_embolden_less` for
    /// synthetic bold. Returns the glyph and the scale WebRender applies to it.
    fn load_glyph(cached: &mut CachedFont, font: &FontInstance, glyph: &GlyphKey) -> Option<(LoadedGlyph, f32)> {
        let face = cached.face.as_ref()?;
        let font_ref = FontRef::from_index(cached.bytes.as_ref()?.as_slice(), cached.index).ok()?;

        if !face.axes.is_empty() && font.variations != cached.variations {
            cached.variations.clear();
            cached.variations.extend_from_slice(&font.variations);
            normalized_coords(&font_ref, &face.axes, &font.variations, &mut cached.coords);
        }

        let FontInstancePlatformOptions { mut hinting, .. } = font.platform_options.unwrap_or_default();
        // Disable hinting if there is a non-axis-aligned transform.
        if font.synthetic_italics.is_enabled() ||
           ((font.transform.scale_x != 0.0 || font.transform.scale_y != 0.0) &&
            (font.transform.skew_x != 0.0 || font.transform.skew_y != 0.0)) {
            hinting = FontHinting::None;
        }
        let mut force_autohint = false;
        let target = match (hinting, font.render_mode) {
            (FontHinting::None, _) => LoadTarget::NoHinting,
            (FontHinting::Mono, _) => LoadTarget::Mono,
            (FontHinting::Light, _) => LoadTarget::Light,
            (FontHinting::LCD, FontRenderMode::Subpixel) => {
                force_autohint = font.flags.contains(FontInstanceFlags::FORCE_AUTOHINT);
                if font.flags.contains(FontInstanceFlags::LCD_VERTICAL) { LoadTarget::LcdV } else { LoadTarget::Lcd }
            }
            _ => {
                force_autohint = font.flags.contains(FontInstanceFlags::FORCE_AUTOHINT);
                LoadTarget::Normal
            }
        };
        let no_autohint = font.flags.contains(FontInstanceFlags::NO_AUTOHINT);
        let load_bitmaps = font.flags.contains(FontInstanceFlags::EMBEDDED_BITMAPS);

        let (x_scale, y_scale) = font.transform.compute_scale().unwrap_or((1.0, 1.0));
        let req_size = font.size.to_f64_px();
        let gid = GlyphId::new(glyph.index());
        if glyph.index() >= face.num_glyphs {
            error!("Unable to load glyph: invalid glyph index {}", glyph.index());
            return None;
        }

        if !face.strikes.is_empty() && !face.scalable {
            if !load_bitmaps {
                // FreeType cannot load a bitmap-only glyph with FT_LOAD_NO_BITMAP.
                error!("Unable to load glyph: bitmap-only font without embedded bitmaps");
                return None;
            }
            let ppems: Vec<F26Dot6> = face.strikes.iter().map(|s| s.y_ppem).collect();
            let strike = face.strikes[ftc::choose_strike(&ppems, ftc::char_size(req_size * y_scale))?];
            let (bitmap, advance) = Self::load_strike_glyph(&font_ref, face, strike, gid, true)?;
            return Self::finish_bitmap(font, strike, bitmap, advance, req_size);
        }

        let mut char_w = ftc::char_size(req_size * x_scale);
        let mut char_h = ftc::char_size(req_size * y_scale);
        if ftc::ppem(char_w) < 1 || ftc::ppem(char_h) < 1 {
            error!("Unable to set glyph size and transform: size {:?} scale {:?}", req_size, (x_scale, y_scale));
            return None;
        }
        // `tt_size_request` / `cff_size_request`: a strike matching the request selects that strike's size.
        let strike = match_strike(&face.strikes, char_w, char_h);
        if let Some(strike) = strike {
            char_w = strike.x_ppem;
            char_h = strike.y_ppem;
            let default_instance = cached.coords.iter().all(|c| *c == F2Dot14::ZERO);
            if load_bitmaps && default_instance {
                if let Some((bitmap, advance)) = Self::load_strike_glyph(&font_ref, face, strike, gid, false) {
                    return Self::finish_bitmap(font, strike, bitmap, advance, req_size);
                }
            }
        }
        // FT_LOAD_COLOR, which the FreeType backend set for fonts with strikes, makes FreeType load an OT-SVG
        // document instead of the outline; the backend could not render that format.
        if !face.strikes.is_empty() && font_ref.svg().is_ok_and(|svg| svg.glyph_data(gid).is_some()) {
            error!("Unsupported format");
            debug!("format=SVG for glyph {} from font {:?}", glyph.index(), font.font_key);
            return None;
        }
        let (Some(x_scale_fixed), Some(y_scale_fixed)) =
            (ftc::scale_for_char_size(char_w, face.units_per_em), ftc::scale_for_char_size(char_h, face.units_per_em))
        else {
            error!("Unable to set glyph size and transform: size {:?} scale {:?}", req_size, (x_scale, y_scale));
            return None;
        };

        let (matrix, delta) = Self::shape_transform(font, x_scale, y_scale, req_size);
        let is_cff = matches!(face.outline_format, Some(OutlineGlyphFormat::Cff) | Some(OutlineGlyphFormat::Cff2));
        // FT_Load_Glyph's autohinter rules: no hinting requested, a transform other than slants and quarter turns,
        // or FT_LOAD_NO_AUTOHINT leave the driver's hinter; FT_LOAD_FORCE_AUTOHINT, light hinting of TrueType
        // outlines (the TrueType driver does not hint lightly; the CFF driver does) and TrueType fonts without
        // instructions use the autohinter.
        let axis_aligned = (matrix.yx == 0 && matrix.xx != 0) || (matrix.xx == 0 && matrix.yx != 0);
        let engine = if target == LoadTarget::NoHinting || face.outline_format.is_none() {
            HintEngine::None
        } else if !no_autohint && face.scalable && axis_aligned &&
                  (force_autohint || (target == LoadTarget::Light && !is_cff) || (!is_cff && !face.has_instructions)) {
            HintEngine::Auto
        } else {
            HintEngine::Native
        };

        let outlines = font_ref.outline_glyphs();
        let Some(outline_glyph) = outlines.get(gid) else {
            error!("Unable to load glyph: no outline for glyph {}", glyph.index());
            return None;
        };
        let size = Size::new(char_h as f32 / 64.0);
        let x_ratio = x_scale_fixed as f64 / y_scale_fixed as f64;
        let mut outline = Outline::default();
        let mut pen = OutlineCollector { outline: &mut outline, x_ratio, start: 0, open: false };
        let drawn = match engine {
            HintEngine::None => outline_glyph.draw(DrawSettings::unhinted(size, LocationRef::new(&cached.coords)), &mut pen),
            HintEngine::Auto | HintEngine::Native => {
                let hint_target = match target {
                    LoadTarget::Mono => Target::Mono,
                    LoadTarget::Light => SmoothMode::Light.into(),
                    LoadTarget::Lcd => SmoothMode::Lcd.into(),
                    LoadTarget::LcdV => SmoothMode::VerticalLcd.into(),
                    LoadTarget::Normal | LoadTarget::NoHinting => SmoothMode::Normal.into(),
                };
                let auto = engine == HintEngine::Auto;
                let found = cached.hinters.iter().position(|(k, _)| {
                    k.char_size == char_h && k.target == hint_target && k.auto == auto && k.coords == cached.coords
                });
                let position = match found {
                    Some(position) => position,
                    None => {
                        let key = HinterKey { char_size: char_h, coords: cached.coords.clone(), target: hint_target, auto };
                        let hint_engine = if auto {
                            let styles = cached.glyph_styles.get_or_insert_with(|| GlyphStyles::new(&outlines));
                            Engine::Auto(Some(styles.clone()))
                        } else {
                            Engine::Interpreter
                        };
                        let options = HintingOptions { engine: hint_engine, target: hint_target };
                        let instance = match HintingInstance::new(&outlines, size, LocationRef::new(&cached.coords), options) {
                            Ok(instance) => instance,
                            Err(err) => {
                                error!("Unable to set up hinting: {:?}", err);
                                return None;
                            }
                        };
                        if cached.hinters.len() >= MAX_HINTERS_PER_FONT {
                            cached.hinters.remove(0);
                        }
                        cached.hinters.push((key, instance));
                        cached.hinters.len() - 1
                    }
                };
                outline_glyph.draw(DrawSettings::hinted(&cached.hinters[position].1, false), &mut pen)
            }
        };
        let metrics = match drawn {
            Ok(metrics) => metrics,
            Err(err) => {
                error!("Unable to load glyph: {:?}", err);
                debug!("\t[{}] with target {:?} from font {:?}", glyph.index(), target, font.font_key);
                return None;
            }
        };
        pen.finish();

        // `slot->metrics.horiAdvance`: the scaled advance, rounded to whole pixels when hinted. The autohinter in
        // light mode and the CFF hinter keep the linear advance; the TrueType interpreter and the autohinter's
        // other modes move it, which skrifa reports.
        let advance_units = font_ref
            .glyph_metrics(Size::unscaled(), LocationRef::new(&cached.coords))
            .advance_width(gid)
            .map_or(0, |a| (a as f64 + 0.5).floor() as i64);
        let mut advance = match engine {
            HintEngine::None => ftc::glyph_advance(advance_units, x_scale_fixed, ftc::Hinting::None),
            HintEngine::Auto if target == LoadTarget::Light => {
                ftc::glyph_advance(advance_units, x_scale_fixed, ftc::Hinting::Light)
            }
            HintEngine::Native if is_cff => ftc::glyph_advance(advance_units, x_scale_fixed, ftc::Hinting::Light),
            _ => match metrics.advance_width {
                Some(a) => ftc::pix_round((a as f64 * x_ratio * 64.0).round() as i64),
                None => ftc::glyph_advance(advance_units, x_scale_fixed, ftc::Hinting::Light),
            },
        };

        // FT_Load_Glyph transforms the loaded (and hinted) outline last.
        if matrix != Matrix::IDENTITY {
            outline.transform(&matrix);
        }
        outline.translate(delta.0, delta.1);

        if font.flags.contains(FontInstanceFlags::SYNTHETIC_BOLD) {
            let strength = ftc::embolden_strength(face.units_per_em, y_scale_fixed);
            ftc::embolden::embolden(&mut outline.points, &outline.contours, strength);
            advance += strength;
        }

        Some((LoadedGlyph::Outline { outline, advance, overlap: metrics.has_overlaps }, 1.0))
    }

    /// Bitmap-glyph tail of `load_glyph`: `FT_GlyphSlot_Embolden` for synthetic bold, and the scale from the strike
    /// size to the requested one.
    fn finish_bitmap(
        font: &FontInstance,
        strike: Strike,
        mut bitmap: GlyphBitmap,
        mut advance: F26Dot6,
        req_size: f64,
    ) -> Option<(LoadedGlyph, f32)> {
        if font.flags.contains(FontInstanceFlags::SYNTHETIC_BOLD) {
            // FT_GlyphSlot_AdjustWeight(slot, 0x0AAA, 0x0AAA) on a bitmap: whole pixels, at least one across.
            let mut xstr = (strike.x_ppem >> 6) * 0x0AAA / 1024;
            let ystr = ((strike.y_ppem >> 6) * 0x0AAA / 1024) & !63;
            xstr &= !63;
            if xstr == 0 {
                xstr = 1 << 6;
            }
            let (xpx, ypx) = ((ftc::pix_round(xstr) >> 6) as usize, (ftc::pix_round(ystr) >> 6) as usize);
            match bitmap.mode {
                // FreeType does not embolden colour glyphs, but still adjusts their metrics.
                PixelMode::Bgra => {}
                _ if bitmap.buffer.is_empty() => {}
                _ => embolden_bitmap(&mut bitmap, xpx, ypx),
            }
            advance += xstr;
            bitmap.top += (ystr >> 6) as i32;
        }
        let scale = req_size as f32 / (strike.y_ppem >> 6) as f32;
        Some((LoadedGlyph::Bitmap { bitmap, advance }, scale))
    }

    /// Loads glyph `gid` from a strike as `tt_face_load_sbit_image` does. `bitmap_only` gives an empty glyph for a
    /// glyph the strike lacks (FreeType's behaviour for bitmap-only fonts); otherwise a missing glyph is `None` so
    /// the caller falls back to the outline.
    fn load_strike_glyph(
        font_ref: &FontRef,
        face: &Face,
        strike: Strike,
        gid: GlyphId,
        bitmap_only: bool,
    ) -> Option<(GlyphBitmap, F26Dot6)> {
        let x_ppem = strike.x_ppem >> 6;
        let loaded = match face.strike_format? {
            BitmapFormat::Sbix => Self::load_sbix_glyph(font_ref, face, strike, gid),
            format => {
                let strikes = BitmapStrikes::with_format(font_ref, format)?;
                strikes.get(strike.index).and_then(|s| s.get(gid)).and_then(|g| {
                    let (w, h) = (g.width as usize, g.height as usize);
                    let (mode, buffer) = match g.data {
                        BitmapData::Png(data) => match decode_png(data) {
                            Some((pw, ph, bgra)) if pw == w && ph == h => (PixelMode::Bgra, bgra),
                            // Load_SBit_Png leaves the bitmap blank when the image does not match the metrics.
                            Some(_) => (PixelMode::Bgra, vec![0; w * h * 4]),
                            None => return None,
                        },
                        BitmapData::Bgra(data) => {
                            if data.len() < w * h * 4 {
                                return None;
                            }
                            (PixelMode::Bgra, data[.. w * h * 4].to_vec())
                        }
                        BitmapData::Mask(mask) => {
                            let mut buffer = vec![0u8; w * h];
                            mask.decode_to_slice(g.width, g.height, &mut buffer).ok()?;
                            (if mask.bpp == 1 { PixelMode::Mono } else { PixelMode::Gray }, buffer)
                        }
                    };
                    let bitmap = GlyphBitmap {
                        mode,
                        width: w,
                        rows: h,
                        buffer,
                        left: g.inner_bearing_x as i32,
                        top: g.inner_bearing_y as i32,
                    };
                    Some((bitmap, (g.advance.unwrap_or(0.0) as i64) * 64))
                })
            }
        };
        match loaded {
            Some(loaded) => Some(loaded),
            None if bitmap_only => {
                // TT_Load_Glyph: an incomplete strike of a bitmap-only font gives an empty glyph with the hmtx
                // advance scaled to the strike.
                let x_scale = ftc::div_fix(x_ppem * 64, face.units_per_em as i64);
                let advance_units = font_ref
                    .glyph_metrics(Size::unscaled(), LocationRef::default())
                    .advance_width(gid)
                    .unwrap_or(0.0) as i64;
                Some((GlyphBitmap::empty(PixelMode::Mono), ftc::mul_fix(advance_units, x_scale)))
            }
            None => None,
        }
    }

    /// `tt_face_load_sbix_image`: PNG glyphs (following `dupe` references), placed at their origin offset.
    fn load_sbix_glyph(font_ref: &FontRef, face: &Face, strike: Strike, gid: GlyphId) -> Option<(GlyphBitmap, F26Dot6)> {
        let sbix = font_ref.sbix().ok()?;
        let sbix_strike = sbix.strikes().get(strike.index).ok()?;
        let mut gid = gid;
        for _ in 0 ..= 4 {
            let data = sbix_strike.glyph_data(gid).ok()??;
            match &data.graphic_type().to_be_bytes() {
                b"dupe" => {
                    let bytes = data.data();
                    if bytes.len() < 2 {
                        return None;
                    }
                    gid = GlyphId::new(u16::from_be_bytes([bytes[0], bytes[1]]) as u32);
                }
                b"png " => {
                    let (w, h, bgra) = decode_png(data.data())?;
                    let x_ppem = strike.x_ppem >> 6;
                    let advance_units = font_ref
                        .glyph_metrics(Size::unscaled(), LocationRef::default())
                        .advance_width(gid)
                        .unwrap_or(0.0) as i64;
                    let advance = advance_units * x_ppem / (face.units_per_em.max(1) as i64);
                    let bitmap = GlyphBitmap {
                        mode: PixelMode::Bgra,
                        width: w,
                        rows: h,
                        buffer: bgra,
                        left: data.origin_offset_x() as i32,
                        top: data.origin_offset_y() as i32 + h as i32,
                    };
                    return Some((bitmap, (advance as u16 as i64) * 64));
                }
                _ => return None,
            }
        }
        None
    }

    fn pad_bounding_box(font: &FontInstance, cbox: &mut BBox) {
        // Apply extra pixel of padding for subpixel AA, due to the filter.
        if font.render_mode == FontRenderMode::Subpixel {
            // The FreeType backend added one full pixel unconditionally when subpixel AA is used, whatever the
            // LCD filter; the glyph cache relies on that footprint.
            let padding: F26Dot6 = 64;
            if font.flags.contains(FontInstanceFlags::LCD_VERTICAL) {
                cbox.y_min -= padding;
                cbox.y_max += padding;
            } else {
                cbox.x_min -= padding;
                cbox.x_max += padding;
            }
        }
    }

    /// The subpixel offset in 26.6, as the FreeType backend computed it.
    fn subpx_offset_26_6(font: &FontInstance, glyph: &GlyphKey, scale: f32) -> (F26Dot6, F26Dot6) {
        let (dx, dy) = font.get_subpx_offset(glyph);
        (
            (dx / scale as f64 * 64.0 + 0.5) as F26Dot6,
            -(dy / scale as f64 * 64.0 + 0.5) as F26Dot6,
        )
    }

    // Get the bounding box for a glyph, accounting for sub-pixel positioning.
    fn get_bounding_box(outline: &Outline, font: &FontInstance, glyph: &GlyphKey, scale: f32) -> BBox {
        // Get the estimated bounding box from the control points.
        let mut cbox = outline.cbox();

        // For spaces and other non-printable characters, early out.
        if outline.contours.is_empty() {
            return cbox;
        }

        Self::pad_bounding_box(font, &mut cbox);

        // Offset the bounding box by subpixel positioning.
        let (dx, dy) = Self::subpx_offset_26_6(font, glyph, scale);
        cbox.x_min += dx;
        cbox.x_max += dx;
        cbox.y_min += dy;
        cbox.y_max += dy;

        // Outset the box to device pixel boundaries
        cbox.x_min &= !63;
        cbox.y_min &= !63;
        cbox.x_max = (cbox.x_max + 63) & !63;
        cbox.y_max = (cbox.y_max + 63) & !63;

        cbox
    }

    fn get_glyph_dimensions_impl(
        loaded: &LoadedGlyph,
        font: &FontInstance,
        glyph: &GlyphKey,
        scale: f32,
        use_transform: bool,
    ) -> Option<GlyphDimensions> {
        let (mut left, mut top, mut width, mut height, advance) = match loaded {
            LoadedGlyph::Bitmap { bitmap, advance } => {
                (bitmap.left, bitmap.top, bitmap.width as i32, bitmap.rows as i32, *advance)
            }
            LoadedGlyph::Outline { outline, advance, .. } => {
                let cbox = Self::get_bounding_box(outline, font, glyph, scale);
                (
                    (cbox.x_min >> 6) as i32,
                    (cbox.y_max >> 6) as i32,
                    ((cbox.x_max - cbox.x_min) >> 6) as i32,
                    ((cbox.y_max - cbox.y_min) >> 6) as i32,
                    *advance,
                )
            }
        };
        let mut advance = advance as f32 / 64.0;
        if use_transform {
            if scale != 1.0 {
                let x0 = left as f32 * scale;
                let x1 = width as f32 * scale + x0;
                let y1 = top as f32 * scale;
                let y0 = y1 - height as f32 * scale;
                left = x0.round() as i32;
                top = y1.round() as i32;
                width = (x1.ceil() - x0.floor()) as i32;
                height = (y1.ceil() - y0.floor()) as i32;
                advance *= scale;
            }
            // An outline glyph's cbox has already been transformed by the load, so only bitmap glyphs need the
            // italic, transpose and flip adjustments.
            if let LoadedGlyph::Bitmap { .. } = loaded {
                if font.synthetic_italics.is_enabled() {
                    let (skew_min, skew_max) = get_skew_bounds(
                        top - height as i32,
                        top,
                        font.synthetic_italics.to_skew(),
                        font.flags.contains(FontInstanceFlags::VERTICAL),
                    );
                    left += skew_min as i32;
                    width += (skew_max - skew_min) as i32;
                }
                if font.flags.contains(FontInstanceFlags::TRANSPOSE) {
                    mem::swap(&mut width, &mut height);
                    mem::swap(&mut left, &mut top);
                    left -= width as i32;
                    top += height as i32;
                }
                if font.flags.contains(FontInstanceFlags::FLIP_X) {
                    left = -(left + width as i32);
                }
                if font.flags.contains(FontInstanceFlags::FLIP_Y) {
                    top = -(top - height as i32);
                }
            }
        }
        Some(GlyphDimensions {
            left,
            top,
            width,
            height,
            advance,
        })
    }

    pub fn get_glyph_index(&mut self, font_key: FontKey, ch: char) -> Option<u32> {
        let cached = self.fonts.get(&font_key)?.lock().ok()?;
        let font = cached.font_ref()?;
        match font.charmap().map(ch) {
            Some(gid) if gid.to_u32() != 0 => Some(gid.to_u32()),
            _ => None,
        }
    }

    pub fn get_glyph_dimensions(
        &mut self,
        font: &FontInstance,
        key: &GlyphKey,
    ) -> Option<GlyphDimensions> {
        let mut cached: MutexGuard<CachedFont> = self.fonts.get(&font.font_key)?.lock().ok()?;
        let (loaded, scale) = Self::load_glyph(&mut cached, font, key)?;
        Self::get_glyph_dimensions_impl(&loaded, font, key, scale, true)
    }

    pub fn prepare_font(font: &mut FontInstance) {
        let preblend_enabled = font.platform_options.map_or(false, |o| o.gamma >= 0 || o.enhanced_contrast > 0);
        match font.render_mode {
            FontRenderMode::Mono => {
                // In mono mode the color of the font is irrelevant.
                font.color = ColorU::new(0xFF, 0xFF, 0xFF, 0xFF);
                // Subpixel positioning is disabled in mono mode.
                font.disable_subpixel_position();
            }
            FontRenderMode::Alpha => {
                if preblend_enabled {
                    font.color = font.color.luminance_color().quantize();
                } else {
                    // Color is unused if there is no preblend.
                    font.color = ColorU::new(0xFF, 0xFF, 0xFF, 0xFF);
                }
            }
            FontRenderMode::Subpixel => {
                if preblend_enabled {
                    font.color = font.color.quantize();
                } else {
                    // Color is unused if there is no preblend.
                    font.color = ColorU::new(0xFF, 0xFF, 0xFF, 0xFF);
                }
            }
        }
    }

    /// Coverage of `outline` (26.6, already placed in bitmap space with y up from the bitmap's bottom edge) in a
    /// `width` x `rows` mask, top row first. `oversample` renders at that many times the resolution, as
    /// `ft_smooth_raster_overlap` does for glyphs with overlapping contours, and averages the result the same way.
    fn render_coverage(
        &mut self,
        outline: &Outline,
        width: usize,
        rows: usize,
        oversample: usize,
    ) -> Vec<u8> {
        let mut coverage = vec![0u8; width * rows];
        if width == 0 || rows == 0 {
            return coverage;
        }
        outline.to_commands(oversample as i64, &mut self.commands);
        let (w, h) = (width * oversample, rows * oversample);
        let mut fine = if oversample == 1 { Vec::new() } else { vec![0u8; w * h] };
        {
            let target: &mut [u8] = if oversample == 1 { &mut coverage } else { &mut fine };
            zeno::Mask::with_scratch(&self.commands[..], &mut self.scratch)
                .style(zeno::Fill::NonZero)
                .format(zeno::Format::Alpha)
                .origin(zeno::Origin::BottomLeft)
                .size(w as u32, h as u32)
                .render_into(target, None);
        }
        if oversample > 1 {
            let n = (oversample * oversample) as u32;
            for y in 0 .. rows {
                for x in 0 .. width {
                    let mut sum = 0u32;
                    for sy in 0 .. oversample {
                        let row = &fine[(y * oversample + sy) * w ..][.. w];
                        for sx in 0 .. oversample {
                            let c = row[x * oversample + sx] as u32;
                            if c != 0 {
                                sum += (c + n / 2) / n;
                                sum -= sum >> 8;
                            }
                        }
                    }
                    coverage[y * width + x] = sum as u8;
                }
            }
        }
        coverage
    }

    /// `FT_Render_Glyph` after the FreeType backend moved the outline to the origin (old `rasterize_glyph_outline`).
    /// `None` where FreeType fails with `Raster_Overflow`.
    fn rasterize_glyph_outline(
        &mut self,
        mut outline: Outline,
        overlap: bool,
        font: &FontInstance,
        key: &GlyphKey,
        scale: f32,
    ) -> Option<GlyphBitmap> {
        // Move the outline curves to be at the origin, taking into account the subpixel positioning.
        let (dx, dy) = Self::subpx_offset_26_6(font, key, scale);
        let mut cbox = outline.cbox();
        Self::pad_bounding_box(font, &mut cbox);
        outline.translate(dx - ((cbox.x_min + dx) & !63), dy - ((cbox.y_min + dy) & !63));

        let mode = match font.render_mode {
            FontRenderMode::Mono => PixelMode::Mono,
            FontRenderMode::Alpha => PixelMode::Gray,
            FontRenderMode::Subpixel => if font.flags.contains(FontInstanceFlags::LCD_VERTICAL) {
                PixelMode::LcdV
            } else {
                PixelMode::Lcd
            },
        };
        let (left, top, width, rows) = preset_bitmap(outline.cbox(), mode)?;
        let mut bitmap = GlyphBitmap {
            mode,
            width: width.max(0) as usize,
            rows: rows.max(0) as usize,
            buffer: Vec::new(),
            left: left as i32,
            top: top as i32,
        };
        // Pixel rows of one coverage render.
        let (px_w, px_h) = match mode {
            PixelMode::Lcd => (bitmap.width / 3, bitmap.rows),
            PixelMode::LcdV => (bitmap.width, bitmap.rows / 3),
            _ => (bitmap.width, bitmap.rows),
        };
        outline.translate(-64 * left, -64 * top + 64 * px_h as i64);
        // `ft_smooth_raster_overlap` rejects outlines too wide for its 16-bit oversampled spans.
        let oversample = if overlap && mode == PixelMode::Gray { 4 } else { 1 };
        if oversample > 1 && px_w * oversample > 0x7FFF {
            return None;
        }

        bitmap.buffer = match mode {
            PixelMode::Gray => self.render_coverage(&outline, px_w, px_h, oversample),
            PixelMode::Mono => {
                // FreeType renders mono glyphs with its 1-bit scan converter (`ftraster.c`, with drop-out control);
                // this approximates it by thresholding the smooth coverage at one half.
                let mut coverage = self.render_coverage(&outline, px_w, px_h, 1);
                for c in &mut coverage {
                    *c = if *c >= 128 { 0xFF } else { 0 };
                }
                coverage
            }
            PixelMode::Lcd | PixelMode::LcdV => {
                // `ft_smooth_raster_lcd` / `ft_smooth_raster_lcdv`: one render per subpixel, the outline shifted by
                // the negated subpixel position (rotated for LCD_V), written to that subpixel's channel.
                const SUB_X: [F26Dot6; 3] = [-21, 0, 21];
                let mut channels = Vec::with_capacity(3);
                for &sub in &SUB_X {
                    let mut shifted = outline.clone();
                    if mode == PixelMode::Lcd {
                        shifted.translate(-sub, 0);
                    } else {
                        shifted.translate(0, sub);
                    }
                    channels.push(self.render_coverage(&shifted, px_w, px_h, 1));
                }
                let mut buffer = vec![0u8; bitmap.width * bitmap.rows];
                for y in 0 .. px_h {
                    for x in 0 .. px_w {
                        for (i, channel) in channels.iter().enumerate() {
                            let value = channel[y * px_w + x];
                            if mode == PixelMode::Lcd {
                                buffer[y * bitmap.width + x * 3 + i] = value;
                            } else {
                                buffer[(y * 3 + i) * bitmap.width + x] = value;
                            }
                        }
                    }
                }
                buffer
            }
            PixelMode::Bgra => Vec::new(),
        };
        Some(bitmap)
    }

    /// The FreeType backend kept a global LCD filter in step with the instances being rasterized. That FreeType was
    /// built without `FT_CONFIG_OPTION_SUBPIXEL_RENDERING`, where `FT_Library_SetLcdFilter` always fails and no
    /// filter ever runs, so `FontLCDFilter` never changed the output and there is no shared state to manage.
    pub fn begin_rasterize(_font: &FontInstance) {
    }

    pub fn end_rasterize(_font: &FontInstance) {
    }

    pub fn rasterize_glyph(&mut self, font: &FontInstance, key: &GlyphKey) -> GlyphRasterResult {
        let cached_font = self.fonts.get(&font.font_key).ok_or(GlyphRasterError::LoadFailed)?.clone();
        let (loaded, scale) = {
            let mut cached = cached_font.lock().map_err(|_| GlyphRasterError::LoadFailed)?;
            Self::load_glyph(&mut cached, font, key).ok_or(GlyphRasterError::LoadFailed)?
        };

        // Get dimensions of the glyph, to see if we need to rasterize it.
        // Don't apply scaling to the dimensions, as the glyph cache needs to know the actual
        // footprint of the glyph.
        let dimensions = Self::get_glyph_dimensions_impl(&loaded, font, key, scale, false)
                             .ok_or(GlyphRasterError::LoadFailed)?;
        let GlyphDimensions { mut left, mut top, width, height, .. } = dimensions;

        // For spaces and other non-printable characters, early out.
        if width == 0 || height == 0 {
            return Err(GlyphRasterError::LoadFailed);
        }

        let is_bitmap_glyph = matches!(loaded, LoadedGlyph::Bitmap { .. });
        let bitmap = match loaded {
            LoadedGlyph::Bitmap { bitmap, .. } => bitmap,
            LoadedGlyph::Outline { outline, overlap, .. } => {
                match self.rasterize_glyph_outline(outline, overlap, font, key, scale) {
                    Some(bitmap) => bitmap,
                    None => {
                        error!("Unable to rasterize");
                        debug!("{:?} with {:?}: raster overflow", key, font.render_mode);
                        return Err(GlyphRasterError::LoadFailed);
                    }
                }
            }
        };

        debug!(
            "Rasterizing {:?} as {:?} with dimensions {:?}",
            key,
            font.render_mode,
            dimensions
        );

        let pixel_mode = bitmap.mode;
        let (mut actual_width, mut actual_height) = match pixel_mode {
            PixelMode::Lcd => {
                assert!(bitmap.width % 3 == 0);
                (bitmap.width / 3, bitmap.rows)
            }
            PixelMode::LcdV => {
                assert!(bitmap.rows % 3 == 0);
                (bitmap.width, bitmap.rows / 3)
            }
            PixelMode::Mono | PixelMode::Gray | PixelMode::Bgra => (bitmap.width, bitmap.rows),
        };

        // If we need padding, we will need to expand the buffer size.
        let (buffer_width, buffer_height, padding) = if font.use_texture_padding() {
            (actual_width + 2, actual_height + 2, 1)
        } else {
            (actual_width, actual_height, 0)
        };

        let mut final_buffer = vec![0u8; buffer_width * buffer_height * 4];

        // Convert the glyph into BGRA8 format, which is what WR expects.
        let subpixel_bgr = font.flags.contains(FontInstanceFlags::SUBPIXEL_BGR);
        let pitch = bitmap.width * bitmap.bytes_per_pixel();
        for y in 0 .. actual_height {
            let dest_row = ((y + padding) * buffer_width + padding) * 4;
            let dest = &mut final_buffer[dest_row .. dest_row + actual_width * 4];
            match pixel_mode {
                PixelMode::Mono | PixelMode::Gray => {
                    let src = &bitmap.buffer[y * pitch .. y * pitch + actual_width];
                    for (d, &alpha) in dest.chunks_exact_mut(4).zip(src) {
                        d.copy_from_slice(&[alpha; 4]);
                    }
                }
                PixelMode::Lcd => {
                    let src = &bitmap.buffer[y * pitch .. y * pitch + actual_width * 3];
                    for (d, s) in dest.chunks_exact_mut(4).zip(src.chunks_exact(3)) {
                        let (mut r, g, mut b) = (s[0], s[1], s[2]);
                        if subpixel_bgr {
                            mem::swap(&mut r, &mut b);
                        }
                        d.copy_from_slice(&[b, g, r, max(max(b, g), r)]);
                    }
                }
                PixelMode::LcdV => {
                    let row = 3 * y * pitch;
                    for (x, d) in dest.chunks_exact_mut(4).enumerate() {
                        let (mut r, g, mut b) = (
                            bitmap.buffer[row + x],
                            bitmap.buffer[row + pitch + x],
                            bitmap.buffer[row + 2 * pitch + x],
                        );
                        if subpixel_bgr {
                            mem::swap(&mut r, &mut b);
                        }
                        d.copy_from_slice(&[b, g, r, max(max(b, g), r)]);
                    }
                }
                PixelMode::Bgra => {
                    // The source is premultiplied BGRA data.
                    dest.copy_from_slice(&bitmap.buffer[y * pitch .. y * pitch + actual_width * 4]);
                }
            }
        }

        if font.use_texture_padding() {
            left -= padding as i32;
            top += padding as i32;
            actual_width = buffer_width;
            actual_height = buffer_height;
        }

        if is_bitmap_glyph {
            if font.synthetic_italics.is_enabled() {
                let (skew_buffer, skew_width, skew_left) = skew_bitmap(
                    &final_buffer,
                    actual_width,
                    actual_height,
                    left,
                    top,
                    font.synthetic_italics.to_skew(),
                    font.flags.contains(FontInstanceFlags::VERTICAL),
                );
                final_buffer = skew_buffer;
                actual_width = skew_width;
                left = skew_left;
            }
            if font.flags.contains(FontInstanceFlags::TRANSPOSE) {
                final_buffer = transpose_bitmap(&final_buffer, actual_width, actual_height);
                mem::swap(&mut actual_width, &mut actual_height);
                mem::swap(&mut left, &mut top);
                left -= actual_width as i32;
                top += actual_height as i32;
            }
            if font.flags.contains(FontInstanceFlags::FLIP_X) {
                flip_bitmap_x(&mut final_buffer, actual_width, actual_height);
                left = -(left + actual_width as i32);
            }
            if font.flags.contains(FontInstanceFlags::FLIP_Y) {
                flip_bitmap_y(&mut final_buffer, actual_width, actual_height);
                top = -(top - actual_height as i32);
            }
        } else {
            left += bitmap.left;
            top += bitmap.top - height as i32;
        }

        let glyph_format = match (pixel_mode, is_bitmap_glyph) {
            (PixelMode::Lcd, _) | (PixelMode::LcdV, _) => font.get_subpixel_glyph_format(),
            (PixelMode::Bgra, _) => GlyphFormat::ColorBitmap,
            (_, true) => GlyphFormat::Bitmap,
            _ => font.get_alpha_glyph_format(),
        };

        Self::gamma_correct_pixels(font, &mut self.gamma_luts, &mut final_buffer, glyph_format);

        Ok(RasterizedGlyph {
            left: left as f32,
            top: top as f32,
            width: actual_width as i32,
            height: actual_height as i32,
            scale,
            format: glyph_format,
            bytes: final_buffer,
            is_packed_glyph: false,
        })
    }

    fn gamma_correct_pixels(
        font: &FontInstance,
        gamma_luts: &mut FastHashMap<(i16, i16), GammaLut>,
        pixels: &mut Vec<u8>,
        glyph_format: GlyphFormat,
    ) {
        let FontInstancePlatformOptions { gamma, enhanced_contrast, .. } =
            font.platform_options.unwrap_or_default();

        if gamma < 0 && enhanced_contrast <= 0 {
            return;
        }

        match glyph_format {
            GlyphFormat::ColorBitmap | GlyphFormat::Bitmap => return,
            _ => {}
        }

        let gamma = gamma.min(400);
        let enhanced_contrast = enhanced_contrast.clamp(0, 100);
        let g = if gamma < 0 { 1.0 } else { gamma as f32 / 100.0 };
        let c = enhanced_contrast as f32 / 100.0;

        if gamma_luts.len() > 4 {
            gamma_luts.clear();
        }
        let gamma_lut = gamma_luts
            .entry((gamma, enhanced_contrast))
            .or_insert_with(|| GammaLut::new(c, g, g));

        match font.render_mode {
            FontRenderMode::Alpha => {
                gamma_lut.preblend_grayscale(pixels, font.color);
            }
            FontRenderMode::Subpixel => {
                gamma_lut.preblend(pixels, font.color);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: i64, y0: i64, x1: i64, y1: i64) -> Outline {
        Outline {
            points: vec![(x0, y0), (x0, y1), (x1, y1), (x1, y0)],
            tags: vec![TAG_ON; 4],
            contours: vec![3],
        }
    }

    #[test]
    fn collector_drops_the_repeated_start_point() {
        let mut outline = Outline::default();
        let mut pen = OutlineCollector { outline: &mut outline, x_ratio: 1.0, start: 0, open: false };
        pen.move_to(0.0, 0.0);
        pen.line_to(1.0, 0.0);
        pen.quad_to(1.0, 1.0, 0.5, 1.0);
        pen.line_to(0.0, 0.0);
        pen.close();
        pen.move_to(2.0, 2.0);
        pen.line_to(3.0, 2.0);
        pen.finish();
        assert_eq!(outline.points, vec![(0, 0), (64, 0), (64, 64), (32, 64), (128, 128), (192, 128)]);
        assert_eq!(outline.tags, vec![TAG_ON, TAG_ON, TAG_CONIC, TAG_ON, TAG_ON, TAG_ON]);
        assert_eq!(outline.contours, vec![3, 5]);
    }

    #[test]
    fn collector_drops_implied_midpoints() {
        // A circle-like contour of four conic control points, as skrifa draws it: starting at the implied midpoint
        // of the last and first control points, with every on-curve point implied.
        let mut outline = Outline::default();
        let mut pen = OutlineCollector { outline: &mut outline, x_ratio: 1.0, start: 0, open: false };
        pen.move_to(0.0, 0.5);
        pen.quad_to(0.0, 1.0, 0.5, 1.0);
        pen.quad_to(1.0, 1.0, 1.0, 0.5);
        // An odd sum: the midpoint of 64 and 1 is 32.5/64, which rounds to 33.
        pen.quad_to(1.0, 1.0 / 64.0, 0.5, 1.0 / 128.0);
        pen.quad_to(0.0, 0.0, 0.0, 0.5);
        pen.close();
        assert_eq!(outline.points, vec![(0, 64), (64, 64), (64, 1), (0, 0)]);
        assert_eq!(outline.tags, vec![TAG_CONIC; 4]);
        assert_eq!(outline.contours, vec![3]);
        // A real on-curve point off the midpoint stays.
        let mut outline = Outline::default();
        let mut pen = OutlineCollector { outline: &mut outline, x_ratio: 1.0, start: 0, open: false };
        pen.move_to(0.0, 0.0);
        pen.quad_to(0.0, 1.0, 0.25, 1.0);
        pen.quad_to(1.0, 1.0, 1.0, 0.0);
        pen.close();
        assert_eq!(outline.points, vec![(0, 0), (0, 64), (16, 64), (64, 64), (64, 0)]);
    }

    #[test]
    fn decompose_handles_conic_start_and_implied_midpoints() {
        // A contour of four conic control points: FreeType starts at the midpoint of the last and first.
        let outline = Outline {
            points: vec![(0, 64), (64, 64), (64, 0), (0, 0)],
            tags: vec![TAG_CONIC; 4],
            contours: vec![3],
        };
        let mut commands = Vec::new();
        outline.to_commands(1, &mut commands);
        use zeno::Command::*;
        let p = |x: f32, y: f32| zeno::Point::new(x, y);
        assert_eq!(
            commands,
            vec![
                MoveTo(p(0.0, 0.5)),
                QuadTo(p(0.0, 1.0), p(0.5, 1.0)),
                QuadTo(p(1.0, 1.0), p(1.0, 0.5)),
                QuadTo(p(1.0, 0.0), p(0.5, 0.0)),
                QuadTo(p(0.0, 0.0), p(0.0, 0.5)),
                Close,
            ]
        );
    }

    /// The pre-move preset_bitmap (FreeType's ft_glyphslot_preset_bitmap line by line), kept as the oracle for the
    /// lucina-ftcompat one.
    fn preset_bitmap_reference(cbox: BBox, mode: PixelMode) -> Option<(i64, i64, i64, i64)> {
        const SUB_X: [F26Dot6; 3] = [-21, 0, 21];
        const SUB_Y: [F26Dot6; 3] = [0, 0, 0];
        let mut pbox = BBox { x_min: cbox.x_min >> 6, y_min: cbox.y_min >> 6, x_max: cbox.x_max >> 6, y_max: cbox.y_max >> 6 };
        let mut c = BBox { x_min: cbox.x_min & 63, y_min: cbox.y_min & 63, x_max: cbox.x_max & 63, y_max: cbox.y_max & 63 };
        let max3 = |v: [i64; 3]| v[0].max(v[1]).max(v[2]);
        let min3 = |v: [i64; 3]| v[0].min(v[1]).min(v[2]);
        match mode {
            PixelMode::Mono => {
                pbox.x_min += (c.x_min + 31) >> 6;
                pbox.x_max += (c.x_max + 32) >> 6;
                if pbox.x_min == pbox.x_max {
                    if ((c.x_min + 31) & 63) - 31 + ((c.x_max + 32) & 63) - 32 < 0 {
                        pbox.x_min -= 1;
                    } else {
                        pbox.x_max += 1;
                    }
                }
                pbox.y_min += (c.y_min + 31) >> 6;
                pbox.y_max += (c.y_max + 32) >> 6;
                if pbox.y_min == pbox.y_max {
                    if ((c.y_min + 31) & 63) - 31 + ((c.y_max + 32) & 63) - 32 < 0 {
                        pbox.y_min -= 1;
                    } else {
                        pbox.y_max += 1;
                    }
                }
            }
            _ => {
                match mode {
                    PixelMode::Lcd => {
                        c.x_min -= max3(SUB_X);
                        c.x_max -= min3(SUB_X);
                        c.y_min -= max3(SUB_Y);
                        c.y_max -= min3(SUB_Y);
                    }
                    PixelMode::LcdV => {
                        c.x_min -= max3(SUB_Y);
                        c.x_max -= min3(SUB_Y);
                        c.y_min += min3(SUB_X);
                        c.y_max += max3(SUB_X);
                    }
                    _ => {}
                }
                pbox.x_min += c.x_min >> 6;
                pbox.y_min += c.y_min >> 6;
                pbox.x_max += (c.x_max + 63) >> 6;
                pbox.y_max += (c.y_max + 63) >> 6;
            }
        }
        let mut width = pbox.x_max - pbox.x_min;
        let mut rows = pbox.y_max - pbox.y_min;
        match mode {
            PixelMode::Lcd => width *= 3,
            PixelMode::LcdV => rows *= 3,
            _ => {}
        }
        if pbox.x_min < -0x8000 || pbox.x_max > 0x7FFF || pbox.y_min < -0x8000 || pbox.y_max > 0x7FFF {
            return None;
        }
        Some((pbox.x_min, pbox.y_max, width, rows))
    }

    #[test]
    fn preset_bitmap_same_as_before_the_move() {
        let mut v: Vec<i64> = vec![0, 1, 31, 32, 33, 63, 64, 65, -1, -31, -32, -33, -63, -64, -65, 0x7FFF << 6,
            0x8000 << 6, -(0x8000 << 6), (-0x8001) << 6, 1 << 40, -(1 << 40)];
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..200 {
            x ^= x << 13; x ^= x >> 7; x ^= x << 17;
            v.push((x as i64) >> (x % 50 + 13));
        }
        let modes = [PixelMode::Mono, PixelMode::Gray, PixelMode::Lcd, PixelMode::LcdV, PixelMode::Bgra];
        for (i, &a) in v.iter().enumerate() {
            for &b in &v[i % 7..] {
                let (x_min, x_max) = (a.min(b), a.max(b));
                let (y_min, y_max) = (a.min(b ^ 17), a.max(b ^ 17));
                let cbox = BBox { x_min, y_min, x_max, y_max };
                for &m in &modes {
                    assert_eq!(preset_bitmap(cbox, m), preset_bitmap_reference(cbox, m), "{:?} {:?}", cbox, m);
                }
            }
        }
    }

    #[test]
    fn preset_box_matches_freetype() {
        // A 10.5 x 10 px box at (0.25, 0): gray covers 11 x 10 pixels.
        let cbox = BBox { x_min: 16, y_min: 0, x_max: 16 + 672, y_max: 640 };
        assert_eq!(preset_bitmap(cbox, PixelMode::Gray), Some((0, 10, 11, 10)));
        // LCD widens by 21/64 px on each side before rounding (0.25 - 0.33 floors to -1, 10.75 + 0.33 ceils to
        // 12), and triples the width.
        assert_eq!(preset_bitmap(cbox, PixelMode::Lcd), Some((-1, 10, 39, 10)));
        assert_eq!(preset_bitmap(cbox, PixelMode::LcdV), Some((0, 11, 11, 36)));
        // Mono rounds the box to pixel centres.
        assert_eq!(preset_bitmap(cbox, PixelMode::Mono), Some((0, 10, 11, 10)));
        // Boxes past 0x7FFF pixels fail as FreeType's Raster_Overflow.
        let huge = BBox { x_min: 0, y_min: 0, x_max: 0x8000 << 6, y_max: 64 };
        assert_eq!(preset_bitmap(huge, PixelMode::Gray), None);
    }

    #[test]
    fn square_coverage_is_full_inside_and_partial_on_edges() {
        let mut ctx = FontContext::new();
        // A 2 x 2 px square from (0.5, 0) to (2.5, 2) in a 3 x 2 bitmap.
        let outline = square(32, 0, 160, 128);
        let coverage = ctx.render_coverage(&outline, 3, 2, 1);
        assert_eq!(coverage, vec![128, 255, 128, 128, 255, 128]);
        let oversampled = ctx.render_coverage(&outline, 3, 2, 4);
        assert_eq!(oversampled, vec![128, 255, 128, 128, 255, 128]);
        // Two overlapping copies: non-zero winding keeps full coverage at 255.
        let mut doubled = square(0, 0, 128, 128);
        let second = square(0, 0, 128, 128);
        doubled.points.extend(second.points);
        doubled.tags.extend(second.tags);
        doubled.contours.push(7);
        assert_eq!(ctx.render_coverage(&doubled, 2, 2, 1), vec![255; 4]);
    }

    #[test]
    fn coverage_rows_are_top_first() {
        let mut ctx = FontContext::new();
        // Only the bottom row of a 1 x 2 bitmap is covered.
        let coverage = ctx.render_coverage(&square(0, 0, 64, 64), 1, 2, 1);
        assert_eq!(coverage, vec![0, 255]);
    }

    #[test]
    fn png_premultiply_matches_pngshim() {
        let mut px = [255, 128, 0, 128, 10, 20, 30, 0, 1, 2, 3, 255];
        premultiply_rgba_to_bgra(&mut px);
        assert_eq!(px, [0, 64, 128, 128, 0, 0, 0, 0, 3, 2, 1, 255]);
    }

    #[test]
    fn bitmap_embolden_grows_right_and_up() {
        let mut bitmap = GlyphBitmap { mode: PixelMode::Gray, width: 2, rows: 1, buffer: vec![100, 200], left: 0, top: 0 };
        embolden_bitmap(&mut bitmap, 1, 1);
        assert_eq!((bitmap.width, bitmap.rows), (3, 2));
        assert_eq!(bitmap.buffer, vec![100, 255, 200, 100, 255, 200]);
        let mut mono = GlyphBitmap { mode: PixelMode::Mono, width: 2, rows: 1, buffer: vec![0xFF, 0], left: 0, top: 0 };
        embolden_bitmap(&mut mono, 1, 0);
        assert_eq!(mono.buffer, vec![0xFF, 0xFF, 0]);
    }

    #[test]
    fn unreadable_font_fails_glyph_calls_without_panicking() {
        use api::{FontInstanceKey, IdNamespace};
        use crate::rasterizer::BaseFontInstance;
        let mut ctx = FontContext::new();
        let key = FontKey::new(IdNamespace(0), 0x7EAD);
        let bytes = Arc::new(b"definitely not a font".to_vec());
        ctx.add_raw_font(&key, bytes.clone(), 0);
        assert!(!FontContext::has_bitmap_strikes(&FontTemplate::Raw(bytes, 0)));
        let font = FontInstance::from_base(Arc::new(BaseFontInstance::new(
            FontInstanceKey::new(IdNamespace(0), 0x7EAD),
            key,
            16.0,
            None,
            None,
            Vec::new(),
        )));
        let glyph = GlyphKey::new(1, api::units::DevicePoint::zero(), font.get_subpx_dir());
        assert_eq!(ctx.get_glyph_index(key, 'a'), None);
        assert!(ctx.get_glyph_dimensions(&font, &glyph).is_none());
        assert!(matches!(ctx.rasterize_glyph(&font, &glyph), Err(GlyphRasterError::LoadFailed)));
        ctx.add_native_font(&FontKey::new(IdNamespace(0), 0x7EAE), NativeFontHandle {
            path: "/nonexistent/font.ttf".into(),
            index: 0,
        });
        ctx.delete_font(&key);
    }

    #[test]
    fn strike_match_needs_rounded_ppem() {
        let strikes = [Strike { index: 0, x_ppem: 13 << 6, y_ppem: 13 << 6 }];
        assert!(match_strike(&strikes, ftc::char_size(13.333), ftc::char_size(13.333)).is_some());
        assert!(match_strike(&strikes, ftc::char_size(13.6), ftc::char_size(13.6)).is_none());
    }
}
