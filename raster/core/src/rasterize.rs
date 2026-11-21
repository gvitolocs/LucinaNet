/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Port of swgl's `rasterize.h`: depth runs, the 2D quad and perspective span rasterizers, clipping against the
//! view frustum, AA edges, user clip distances, delayed clears per row (`prepare_row`) and `draw_elements`.
//!
//! What gl.cc reads from `ctx` during a draw is passed in a [`DrawCtx`]; swgl's raster statics (`blend_key`,
//! `swgl_ClipFlags`, the AA distances...) are in the [`RasterState`] each call gets. So a draw needs no globals and
//! can later run its rows on several threads (raster/DESIGN.md, "Parallel rasterizing").
//!
//! Pixels are reached through raw pointers into the color buffer, as in the C++ and as the generated span shaders
//! take them. Every row the rasterizer touches is inside the clip rectangle, which lies inside the texture
//! (`Context::apply_scissor`), and each depth row is checked with `rules::row_span` before it is used.

use crate::gl::defs::{GL_ALWAYS, GL_LEQUAL, GL_LESS, GL_R8, GL_RGBA8};
use crate::gl::{FloatRange, IntRange, IntRect, Texture};
use crate::glsl::{
    bit_cast, cast, clamp, f32_to_i32_scalar, f32_to_u32_scalar, if_then_else, init_interp, inversesqrt, max, min,
    recip, test_all, test_any, vec2_scalar, vec3_scalar, vec4, vec4_scalar, Float, I32, X, XYZW, Y, Z,
};
use crate::blend::{
    blend_pixels_R8, blend_pixels_RGBA8, load_span, packR8, pack_pixels_R8, pack_pixels_RGBA8, restore_aa,
    restore_clip_mask, store_span, BlendKey, BLEND_KEY_NONE,
};
use crate::state::{SWGL_CLIP_FLAG_AA, SWGL_CLIP_FLAG_BLEND_OVERRIDE, SWGL_CLIP_FLAG_MASK};
use crate::texture::{force_clear_row, pack, unpack, ClearValue, PackedRGBA8, WideR8};
use crate::program::{FragmentShaderImpl, FragmentShaderState, ProgramImpl, VertexAttrib};
use crate::RasterState;
use lucinanet_raster_rules as rules;

/// What the rasterizer reads of the GL context during a draw, and the counters it keeps there.
#[derive(Clone, Copy, Debug)]
pub struct DrawCtx {
    pub viewport: IntRect,
    /// The scissor rectangle when the scissor test is on.
    pub scissor: Option<IntRect>,
    pub blend: bool,
    pub blend_key: BlendKey,
    pub depthfunc: u32,
    pub depthmask: bool,
    pub shaded_rows: i32,
    pub shaded_pixels: i32,
    /// `Some(s)` when a split draw rasterizes stripe `s` (rows `32 * s..32 * s + 32`, `rules::stripe_span`) on the
    /// thread that claimed it (raster/DESIGN.md, "Parallel rasterizing"). `None` on the one-thread path.
    pub stripe: Option<i64>,
}

impl DrawCtx {
    /// `ctx->apply_scissor(t)`.
    pub fn apply_scissor(&self, t: &Texture) -> IntRect {
        let bb = t.bounds();
        match self.scissor {
            Some(s) => bb.intersection(&(s - t.offset)),
            None => bb,
        }
    }
}

/// Whether row `y` is shaded: every row on the one-thread path, else the rows of the stripe being drawn. The rows
/// above it are still walked, stepping the edges and interpolants as one thread does, so the rows shaded start from the
/// same values.
#[inline(always)]
fn owns_row(dc: &DrawCtx, y: i32) -> bool {
    match dc.stripe {
        None => true,
        Some(s) => y >= 0 && i64::from(y) / rules::STRIPE == s,
    }
}

/// Whether row `y` is below the stripe being drawn, so the primitive is done.
#[inline(always)]
fn past_stripe(dc: &DrawCtx, y: i32) -> bool {
    matches!(dc.stripe, Some(s) if i64::from(y) >= (s + 1) * rules::STRIPE)
}

// ---- interpolants ------------------------------------------------------------------------------------------------

/// `Interpolants` (program.h, a `vec3`): twelve floats the rasterizer only adds, subtracts and scales lane by lane,
/// which a shader reads and writes as its `InterpOutputs` bytes. Lane-wise `f32` arithmetic rounds exactly like the
/// C++ vector operators.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Interpolants(pub [f32; 12]);

impl Interpolants {
    fn zip(self, b: Self, f: impl Fn(f32, f32) -> f32) -> Self {
        Interpolants(core::array::from_fn(|i| f(self.0[i], b.0[i])))
    }
    fn add(self, b: Self) -> Self {
        self.zip(b, |x, y| x + y)
    }
    fn sub(self, b: Self) -> Self {
        self.zip(b, |x, y| x - y)
    }
    fn scale(self, s: f32) -> Self {
        Interpolants(self.0.map(|x| x * s))
    }
    fn bytes(&self) -> &[u8] {
        // SAFETY: twelve `f32`s, `repr(C)`, no padding.
        unsafe { core::slice::from_raw_parts(self as *const Self as *const u8, core::mem::size_of::<Self>()) }
    }
    /// `get_clip_distances(interp)`: the first chunk.
    fn clip_distances(&self) -> Float {
        Float::new([self.0[0], self.0[1], self.0[2], self.0[3]])
    }
}

fn interps_bytes_mut(i: &mut [Interpolants]) -> &mut [u8] {
    // SAFETY: `Interpolants` is plain `f32`s with no padding, so its bytes may be written freely.
    unsafe { core::slice::from_raw_parts_mut(i.as_mut_ptr() as *mut u8, core::mem::size_of_val(i)) }
}

// ---- depth runs ---------------------------------------------------------------------------------------------------

/// The largest 24-bit depth value supported.
pub const MAX_DEPTH_VALUE: u32 = 0xFF_FFFF;
/// The longest 8-bit depth run that is supported, aligned to SIMD chunk size.
pub const MAX_DEPTH_RUN: u32 = 255 & !3;

/// `DepthRun`: depth in the low 24 bits, the run length in the high 8. A zero count marks a flat sample (the word
/// is then the depth itself, sign extended).
#[inline(always)]
fn depth_run(depth: u32, count: u32) -> u32 {
    (depth & MAX_DEPTH_VALUE) | ((count & 0xFF) << 24)
}
#[inline(always)]
fn run_depth(r: u32) -> u32 {
    r & MAX_DEPTH_VALUE
}
#[inline(always)]
fn run_count(r: u32) -> u32 {
    r >> 24
}
#[inline(always)]
fn is_flat(r: u32) -> bool {
    run_count(r) == 0
}

/// `DepthRun::compare<FUNC>(src)`.
#[inline(always)]
fn compare(func: u32, r: u32, src: u32) -> bool {
    match func {
        GL_LEQUAL => src <= run_depth(r),
        GL_LESS => src < run_depth(r),
        // GL_ALWAYS
        _ => true,
    }
}


/// One row of a depth texture: `len` 32-bit entries, possibly unaligned.
#[derive(Clone, Copy)]
struct DepthRow {
    p: *mut u8,
    len: usize,
}

impl DepthRow {
    /// Row `y` of `t`, if it lies inside the buffer (`rules::row_span`).
    fn new(t: &Texture, y: i32) -> Option<DepthRow> {
        if t.buf.is_null() {
            return None;
        }
        let (off, end) =
            rules::row_span(0, t.width as i64, y as i64, 4, t.stride() as i64, t.width as i64, t.height as i64)?;
        Some(DepthRow { p: t.buf.wrapping_add(off as usize), len: ((end - off) / 4) as usize })
    }
    #[inline(always)]
    fn get(&self, i: usize) -> u32 {
        assert!(i < self.len, "depth sample past the end of the row");
        // SAFETY: `i < len`, and the row's `len * 4` bytes are inside the texture buffer (`DepthRow::new`).
        unsafe { (self.p.add(i * 4) as *const u32).read_unaligned() }
    }
    #[inline(always)]
    fn set(&self, i: usize, v: u32) {
        assert!(i < self.len, "depth sample past the end of the row");
        // SAFETY: as in `get`; the depth texture is only written by the thread drawing this row.
        unsafe { (self.p.add(i * 4) as *mut u32).write_unaligned(v) }
    }
    /// Lane `k` of the chunk at `i`, 0 past the end (those lanes are masked off by every caller).
    #[inline(always)]
    fn load4(&self, i: usize, span: usize) -> I32 {
        I32::new(core::array::from_fn(|k| if k < span && i + k < self.len { self.get(i + k) as i32 } else { 0 }))
    }
    #[inline(always)]
    fn store4(&self, i: usize, span: usize, v: I32) {
        for k in 0..span.min(4) {
            if i + k < self.len {
                self.set(i + k, v.0[k] as u32);
            }
        }
    }
}

/// `set_depth_runs(runs, depth, width)`: runs of at most `MAX_DEPTH_RUN` from `at`.
fn set_depth_runs(row: &DepthRow, mut at: usize, depth: u32, mut width: u32) {
    while width >= MAX_DEPTH_RUN {
        row.set(at, depth_run(depth, MAX_DEPTH_RUN));
        at += MAX_DEPTH_RUN as usize;
        width -= MAX_DEPTH_RUN;
    }
    if width > 0 {
        row.set(at, depth_run(depth, width));
    }
}

/// `DepthCursor`: positions in a row's run array.
#[derive(Clone, Copy, Debug, Default)]
struct DepthCursor {
    cur: usize,
    start: usize,
    end: usize,
}

impl DepthCursor {
    /// The run after `i`. A zero count inside a run array (a corrupt row) would loop forever in the C++; it reads as
    /// a run of one here.
    #[inline(always)]
    fn next(row: &DepthRow, i: usize) -> usize {
        i + (run_count(row.get(i)).max(1)) as usize
    }

    fn new(row: &DepthRow, num_runs: usize, span_offset: i32, span_count: i32) -> DepthCursor {
        debug_assert!(!is_flat(row.get(0)));
        let start = span_offset.max(0) as usize;
        let mut c = DepthCursor { cur: 0, start, end: (start + span_count.max(0) as usize).min(num_runs) };
        if c.start >= num_runs {
            c.cur = num_runs;
            c.start = num_runs;
            return c;
        }
        loop {
            let next = Self::next(row, c.cur);
            if c.start < next {
                break;
            }
            c.cur = next;
        }
        c
    }

    fn skip_failed(&mut self, row: &DepthRow, val: u32, func: u32) -> i32 {
        let prev = self.start;
        while self.cur < self.end {
            if compare(func, row.get(self.cur), val) {
                return (self.start - prev) as i32;
            }
            self.cur = Self::next(row, self.cur);
            self.start = self.cur;
        }
        -1
    }

    fn check_passed(&mut self, row: &DepthRow, val: u32, func: u32, mask: bool) -> i32 {
        let prev = self.cur;
        while self.cur < self.end {
            let r = row.get(self.cur);
            if !compare(func, r, val) {
                break;
            }
            let mut next = Self::next(row, self.cur);
            if next > self.end {
                if mask {
                    row.set(self.end, depth_run(run_depth(r), (next - self.end) as u32));
                }
                next = self.end;
            }
            self.cur = next;
        }
        if self.cur <= self.start {
            return 0;
        }
        let passed = self.cur - self.start;
        if mask {
            if prev < self.start {
                let r = row.get(prev);
                row.set(prev, depth_run(run_depth(r), (self.start - prev) as u32));
            }
            set_depth_runs(row, self.start, val, passed as u32);
        }
        self.start = self.cur;
        passed as i32
    }

    fn fill(&mut self, row: &DepthRow, depth: u32) {
        self.check_passed(row, depth, GL_ALWAYS, true);
    }
}

impl Texture {
    /// `Texture::init_depth_runs(depth)`: every row one maximal run.
    pub fn init_depth_runs(&mut self, depth: u32) {
        if self.buf.is_null() {
            return;
        }
        for y in 0..self.height {
            if let Some(row) = DepthRow::new(self, y) {
                set_depth_runs(&row, 0, depth, self.width.max(0) as u32);
            }
        }
        self.set_cleared(true);
    }

    /// `Texture::fill_depth_runs(depth, scissor)`. A rectangle of negative width fills nothing (the C++ would fill a
    /// huge count).
    pub fn fill_depth_runs(&mut self, depth: u32, scissor: &IntRect) {
        if self.buf.is_null() {
            return;
        }
        debug_assert!(self.cleared());
        let bb = self.bounds().intersection(&(*scissor - self.offset));
        for y in bb.y0..bb.y1 {
            let Some(row) = DepthRow::new(self, y) else { continue };
            if bb.width() >= self.width {
                set_depth_runs(&row, 0, depth, self.width.max(0) as u32);
            } else if is_flat(row.get(0)) {
                for x in bb.x0.max(0)..bb.x1 {
                    row.set(x as usize, depth);
                }
            } else {
                DepthCursor::new(&row, self.width as usize, bb.x0, bb.width()).fill(&row, depth);
            }
        }
    }
}

/// `flatten_depth_runs(runs, width)`: each run written out sample by sample.
fn flatten_depth_runs(row: &DepthRow, width: usize) {
    if is_flat(row.get(0)) {
        return;
    }
    let mut i = 0;
    while i < width {
        let r = row.get(i);
        let n = run_count(r).max(1) as usize;
        for k in i..(i + n).min(width) {
            row.set(k, run_depth(r));
        }
        i += n;
    }
}

/// `check_depth<DISCARD>(src, zbuf, outmask, span)`: the lanes that fail the depth test, or `None` if all do.
/// Only the `span` lanes inside the row are read and written (the C++ reads a whole chunk and writes the masked-off
/// lanes back unchanged).
#[inline(always)]
fn check_depth<const DISCARD: bool>(dc: &DrawCtx, src: I32, row: &DepthRow, at: usize, span: i32) -> Option<I32> {
    let dest = row.load4(at, span as usize);
    let mut mask = if dc.depthfunc == GL_LEQUAL { src.cmp_gt(dest) } else { src.cmp_ge(dest) };
    mask |= I32::splat(span).cmp_lt(I32::new([1, 2, 3, 4]));
    if mask.0.iter().all(|&m| m != 0) {
        return None;
    }
    if !DISCARD && dc.depthmask {
        row.store4(at, span as usize, (mask & dest) | (!mask & src));
    }
    Some(mask)
}

/// `packDepth()`.
#[inline(always)]
fn pack_depth(fs: &FragmentShaderState) -> I32 {
    cast(fs.gl_FragCoord.z * Float::splat(MAX_DEPTH_VALUE as f32))
}

/// `discard_depth(src, zbuf, mask)`.
#[inline(always)]
fn discard_depth(dc: &DrawCtx, fs: &FragmentShaderState, src: I32, row: &DepthRow, at: usize, mut mask: I32, span: i32) {
    if dc.depthmask {
        let dest = row.load4(at, span as usize);
        mask |= fs.swgl_IsPixelDiscarded;
        row.store4(at, span as usize, (mask & dest) | (!mask & src));
    }
}

// ---- pixel formats -----------------------------------------------------------------------------------------------

/// A color buffer texel the rasterizer draws: `uint32_t` (RGBA8) or `uint8_t` (R8).
pub trait Pixel: ClearValue + 'static {
    fn has_draw_span(fs: &dyn FragmentShaderImpl) -> bool;
    /// # Safety
    /// `buf..buf + len` are pixels of one row of the target.
    unsafe fn draw_span(fs: &mut dyn FragmentShaderImpl, rs: &mut RasterState, buf: *mut Self, len: i32) -> i32;
    /// `mask_output(buf, zmask, span)`.
    ///
    /// # Safety
    /// `buf..buf + span` are pixels of one row of the target.
    unsafe fn mask_output(rs: &mut RasterState, fs: &FragmentShaderState, buf: *mut Self, zmask: I32, span: i32);
    /// `discard_output<false>(buf, span)`.
    ///
    /// # Safety
    /// As [`Pixel::mask_output`].
    unsafe fn output(rs: &mut RasterState, fs: &FragmentShaderState, buf: *mut Self, span: i32);
}

impl Pixel for u32 {
    fn has_draw_span(fs: &dyn FragmentShaderImpl) -> bool {
        fs.has_draw_span_rgba8()
    }
    unsafe fn draw_span(fs: &mut dyn FragmentShaderImpl, rs: &mut RasterState, buf: *mut u32, len: i32) -> i32 {
        fs.draw_span_rgba8(rs, buf, len)
    }
    unsafe fn mask_output(rs: &mut RasterState, fs: &FragmentShaderState, buf: *mut u32, zmask: I32, span: i32) {
        let mut r = pack_pixels_RGBA8(fs, 255.0);
        // SAFETY: the caller guarantees `span` pixels at `buf`.
        unsafe {
            let dst: PackedRGBA8 = load_span(buf, span);
            if rs.blend_key != BLEND_KEY_NONE {
                r = blend_pixels_RGBA8(rs, fs, buf, dst, r, span);
            }
            let mask: PackedRGBA8 = bit_cast(zmask);
            store_span(buf, (mask & dst) | (!mask & pack(r)), span);
        }
    }
    unsafe fn output(rs: &mut RasterState, fs: &FragmentShaderState, buf: *mut u32, span: i32) {
        let mut r = pack_pixels_RGBA8(fs, 255.0);
        // SAFETY: the caller guarantees `span` pixels at `buf`.
        unsafe {
            if rs.blend_key != BLEND_KEY_NONE {
                let dst: PackedRGBA8 = load_span(buf, span);
                r = blend_pixels_RGBA8(rs, fs, buf, dst, r, span);
            }
            store_span(buf, pack(r), span);
        }
    }
}

impl Pixel for u8 {
    fn has_draw_span(fs: &dyn FragmentShaderImpl) -> bool {
        fs.has_draw_span_r8()
    }
    unsafe fn draw_span(fs: &mut dyn FragmentShaderImpl, rs: &mut RasterState, buf: *mut u8, len: i32) -> i32 {
        fs.draw_span_r8(rs, buf, len)
    }
    unsafe fn mask_output(rs: &mut RasterState, fs: &FragmentShaderState, buf: *mut u8, zmask: I32, span: i32) {
        let mut r = pack_pixels_R8(fs, 255.0);
        // SAFETY: the caller guarantees `span` pixels at `buf`.
        unsafe {
            let dst: WideR8 = unpack(load_span(buf, span));
            if rs.blend_key != BLEND_KEY_NONE {
                r = blend_pixels_R8(rs, buf, dst, r, span);
            }
            let mask = packR8(zmask);
            store_span(buf, pack((mask & dst) | (!mask & r)), span);
        }
    }
    unsafe fn output(rs: &mut RasterState, fs: &FragmentShaderState, buf: *mut u8, span: i32) {
        let mut r = pack_pixels_R8(fs, 255.0);
        // SAFETY: the caller guarantees `span` pixels at `buf`.
        unsafe {
            if rs.blend_key != BLEND_KEY_NONE {
                let dst: WideR8 = unpack(load_span(buf, span));
                r = blend_pixels_R8(rs, buf, dst, r, span);
            }
            store_span(buf, pack(r), span);
        }
    }
}

/// `discard_output<DISCARD>(buf, span)`.
///
/// # Safety
/// `buf..buf + span` are pixels of one row of the target.
#[inline(always)]
unsafe fn discard_output<const DISCARD: bool, P: Pixel>(rs: &mut RasterState, fs: &FragmentShaderState, buf: *mut P, span: i32) {
    // SAFETY: the caller's guarantee.
    unsafe {
        if DISCARD {
            P::mask_output(rs, fs, buf, fs.swgl_IsPixelDiscarded, span)
        } else {
            P::output(rs, fs, buf, span)
        }
    }
}

// ---- ClipRect ----------------------------------------------------------------------------------------------------

/// `ClipRect`.
#[derive(Clone, Copy, Debug)]
struct ClipRect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl ClipRect {
    fn from_int(i: &IntRect) -> Self {
        ClipRect { x0: i.x0 as f32, y0: i.y0 as f32, x1: i.x1 as f32, y1: i.y1 as f32 }
    }

    /// `ClipRect(const Texture& t)`: the scissored target, and the blend key and clip mask state for the primitive.
    fn new(dc: &DrawCtx, rs: &mut RasterState, t: &Texture) -> Self {
        let mut c = Self::from_int(&dc.apply_scissor(t));
        if dc.blend {
            rs.blend_key = dc.blend_key;
            if rs.swgl_ClipFlags != 0 {
                if rs.swgl_ClipFlags & SWGL_CLIP_FLAG_BLEND_OVERRIDE != 0 {
                    rs.blend_key = rs.swgl_BlendOverride;
                }
                if rs.swgl_ClipFlags & SWGL_CLIP_FLAG_MASK != 0 {
                    let m = &rs.swgl_ClipMask.common;
                    let bounds = IntRect::new(0, 0, m.width as i32, m.height as i32);
                    rs.swgl_ClipMaskBounds.intersect(&bounds);
                    let o = dc.viewport.origin();
                    rs.swgl_ClipMaskOffset.x = rs.swgl_ClipMaskOffset.x.wrapping_add(o.x.wrapping_sub(t.offset.x));
                    rs.swgl_ClipMaskOffset.y = rs.swgl_ClipMaskOffset.y.wrapping_add(o.y.wrapping_sub(t.offset.y));
                    let off = rs.swgl_ClipMaskOffset;
                    rs.swgl_ClipMaskBounds.offset(&off);
                    c.intersect(&rs.swgl_ClipMaskBounds);
                    restore_clip_mask(rs);
                }
                if rs.swgl_ClipFlags & SWGL_CLIP_FLAG_AA != 0 {
                    restore_aa(rs);
                }
            }
        } else {
            rs.blend_key = BLEND_KEY_NONE;
            rs.swgl_ClipFlags = 0;
        }
        c
    }

    fn x_range(&self) -> FloatRange {
        FloatRange { start: self.x0, end: self.x1 }
    }

    fn intersect(&mut self, c: &IntRect) {
        self.x0 = max(self.x0, c.x0 as f32);
        self.y0 = max(self.y0, c.y0 as f32);
        self.x1 = min(self.x1, c.x1 as f32);
        self.y1 = min(self.y1, c.y1 as f32);
    }

    /// `set_clip_mask(x, y, buf)`.
    fn set_clip_mask<P>(&self, rs: &mut RasterState, x: i32, y: i32, buf: *mut P) {
        if rs.swgl_ClipFlags & SWGL_CLIP_FLAG_MASK != 0 {
            rs.swgl_SpanBuf = buf as *mut u8;
            let m = &rs.swgl_ClipMask.common;
            let off = (y.wrapping_sub(rs.swgl_ClipMaskOffset.y) as isize) * m.stride as isize
                + x.wrapping_sub(rs.swgl_ClipMaskOffset.x) as isize;
            rs.swgl_ClipMaskBuf = (m.buf.0 as *mut u8).wrapping_offset(off);
        }
    }

    /// `overlaps(nump, p)`.
    fn overlaps(&self, p: &[(f32, f32)]) -> bool {
        let mut sides = 0;
        for &(x, y) in p {
            sides |= if x < self.x1 { if x > self.x0 { 1 | 2 } else { 1 } } else { 2 };
            sides |= if y < self.y1 { if y > self.y0 { 4 | 8 } else { 4 } } else { 8 };
        }
        sides == 0xF
    }
}

// ---- edges and spans ---------------------------------------------------------------------------------------------

/// What `aa_span` and `clip_distance_range` need of an edge.
trait SpanEdge {
    fn cur_x(&self) -> f32;
    fn x_slope(&self) -> f32;
    fn edge_mask(&self) -> bool;
    fn interp(&self) -> &Interpolants;
}

/// `x_range(p0, p1)`.
fn x_range(a: f32, b: f32) -> FloatRange {
    FloatRange { start: min(a, b), end: max(a, b) }
}

/// `x_intercepts(e)`.
fn x_intercepts<E: SpanEdge>(e: &E) -> FloatRange {
    let rad = 0.5f32 * e.x_slope().abs();
    FloatRange { start: e.cur_x() - rad, end: e.cur_x() + rad }
}

/// `aa_edge(e, bounds)`.
fn aa_edge<E: SpanEdge>(e: &E, bounds: &FloatRange) -> IntRange {
    if e.edge_mask() {
        bounds.clip(x_intercepts(e)).round_out()
    } else {
        bounds.clip(FloatRange { start: e.cur_x(), end: e.cur_x() }).round()
    }
}

/// `aa_dist(e, dir)`.
fn aa_dist<E: SpanEdge>(e: &E, dir: f32) -> FloatRange {
    if e.edge_mask() {
        let dx = (dir * 256.0f32) * inversesqrt(1.0f32 + e.x_slope() * e.x_slope());
        FloatRange { start: 128.0f32 + dx * (e.cur_x() - 0.5f32), end: -dx }
    } else {
        FloatRange { start: 256.0, end: 0.0 }
    }
}

/// `aa_span(buf, left, right, bounds)`.
fn aa_span<P, E: SpanEdge>(rs: &mut RasterState, buf: *mut P, left: &E, right: &E, bounds: &FloatRange) -> IntRange {
    if rs.swgl_ClipFlags & SWGL_CLIP_FLAG_AA == 0 {
        return bounds.clip(FloatRange { start: left.cur_x(), end: right.cur_x() }).round();
    }
    let left_aa = aa_edge(left, bounds);
    let left_dist = aa_dist(left, -1.0);
    let right_aa = aa_edge(right, bounds);
    let right_dist = aa_dist(right, 1.0);
    let psize = core::mem::size_of::<P>();
    rs.swgl_OpaqueStart = buf.wrapping_offset(left_aa.end as isize) as *const u8;
    rs.swgl_OpaqueSize = ((right_aa.start.wrapping_sub(left_aa.end).wrapping_sub(3)).max(0) as u32).wrapping_mul(psize as u32);
    let offset: Float = cast(I32::splat(left_aa.end) + I32::new([0, 1, 2, 3]));
    rs.swgl_LeftAADist = Float::splat(left_dist.start) + offset * Float::splat(left_dist.end);
    rs.swgl_RightAADist = Float::splat(right_dist.start) + offset * Float::splat(right_dist.end);
    rs.swgl_AASlope = Float::new([left_dist.end, right_dist.end, 0.0, 0.0]) / Float::splat(psize as f32);
    IntRange { start: left_aa.start, end: right_aa.end }
}

/// `clip_distance_range(left, right)`.
fn clip_distance_range<E: SpanEdge>(left: &E, right: &E) -> IntRange {
    let left_clip = left.interp().clip_distances();
    let right_clip = right.interp().clip_distances();
    let clip_step = (right_clip - left_clip) / Float::splat(right.cur_x() - left.cur_x());
    let clip_dist = clamp(Float::splat(left.cur_x()) - left_clip * recip(clip_step), Float::splat(0.0), Float::splat(1.0e6));
    let zero = Float::splat(0.0);
    let far = Float::splat(1.0e6);
    let mut start: Float =
        if_then_else(clip_step.cmp_gt(zero), clip_dist, if_then_else(left_clip.cmp_lt(zero), far, zero));
    let mut end: Float =
        if_then_else(clip_step.cmp_lt(zero), clip_dist, if_then_else(right_clip.cmp_ge(zero), far, zero));
    start = max(start, start.swizzle([2, 3, 0, 1]));
    end = min(end, end.swizzle([2, 3, 0, 1]));
    FloatRange { start: max(start.0[0], start.0[1]), end: min(end.0[0], end.0[1]) }.round()
}

/// The `Edge` of `draw_quad_spans`.
#[derive(Clone, Copy, Debug)]
struct Edge2 {
    x_slope: f32,
    x: f32,
    interp_slope: Interpolants,
    interp: Interpolants,
    edge_mask: bool,
}

impl Edge2 {
    fn new(y: f32, p0: vec2_scalar, p1: vec2_scalar, i0: &Interpolants, i1: &Interpolants, edge_index: usize, aa_mask: i32) -> Self {
        let y_scale = 1.0f32 / max(p1.y - p0.y, 1.0f32 / 256.0);
        let x_slope = (p1.x - p0.x) * y_scale;
        let x = p0.x + (y - p0.y) * x_slope;
        let interp_slope = i1.sub(*i0).scale(y_scale);
        let interp = i0.add(interp_slope.scale(y - p0.y));
        Edge2 { x_slope, x, interp_slope, interp, edge_mask: (aa_mask >> edge_index) & 1 != 0 }
    }
    fn next_row(&mut self) {
        self.x += self.x_slope;
        self.interp = self.interp.add(self.interp_slope);
    }
}

impl SpanEdge for Edge2 {
    fn cur_x(&self) -> f32 {
        self.x
    }
    fn x_slope(&self) -> f32 {
        self.x_slope
    }
    fn edge_mask(&self) -> bool {
        self.edge_mask
    }
    fn interp(&self) -> &Interpolants {
        &self.interp
    }
}

/// The `Edge` of `draw_perspective_spans`: the whole point is stepped (Z and W too), and the interpolants are
/// scaled by the points' 1/w.
#[derive(Clone, Copy, Debug)]
struct Edge3 {
    p_slope: vec4_scalar,
    p: vec4_scalar,
    interp_slope: Interpolants,
    interp: Interpolants,
    edge_mask: bool,
}

impl Edge3 {
    fn new(y: f32, p0: vec4_scalar, p1: vec4_scalar, i0: &Interpolants, i1: &Interpolants, edge_index: usize, aa_mask: i32) -> Self {
        let y_scale = 1.0f32 / max(p1.y - p0.y, 1.0f32 / 256.0);
        let p_slope = (p1 - p0) * y_scale;
        let p = p0 + p_slope * (y - p0.y);
        let interp_slope = i1.scale(p1.w).sub(i0.scale(p0.w)).scale(y_scale);
        let interp = i0.scale(p0.w).add(interp_slope.scale(y - p0.y));
        Edge3 { p_slope, p, interp_slope, interp, edge_mask: (aa_mask >> edge_index) & 1 != 0 }
    }
    fn zw(&self) -> vec2_scalar {
        vec2_scalar { x: self.p.z, y: self.p.w }
    }
    fn next_row(&mut self) {
        self.p += self.p_slope;
        self.interp = self.interp.add(self.interp_slope);
    }
}

impl SpanEdge for Edge3 {
    fn cur_x(&self) -> f32 {
        self.p.x
    }
    fn x_slope(&self) -> f32 {
        self.p_slope.x
    }
    fn edge_mask(&self) -> bool {
        self.edge_mask
    }
    fn interp(&self) -> &Interpolants {
        &self.interp
    }
}

/// `perpDot(a, b)`.
fn perp_dot(a: vec2_scalar, b: vec2_scalar) -> f32 {
    a.x * b.y - a.y * b.x
}

/// `checkIfEdgesFlipped(l0, l1, r0, r1)`.
fn check_if_edges_flipped(l0: vec2_scalar, l1: vec2_scalar, r0: vec2_scalar, r1: vec2_scalar) -> bool {
    l0.x > r0.x || (l0.x == r0.x && perp_dot(l1 - l0, r1 - r0) > 0.0)
}

/// Where the depth test reads a span's Z: one value for the primitive, or `packDepth()` per chunk.
#[derive(Clone, Copy)]
enum ZSource {
    Const(u32),
    Pack,
}

/// What a span draws against: the color row and, with a depth test, the depth row.
struct SpanTarget<'a> {
    dc: &'a mut DrawCtx,
    rs: &'a mut RasterState,
    fs: &'a mut dyn FragmentShaderImpl,
}

/// `prepare_row<P>(colortex, y, startx, endx, use_discard, depth, z, cursor)`. `depth_flat` is `None` without a
/// depth test, else whether the depth sample the span starts at is flat.
#[allow(clippy::too_many_arguments)]
fn prepare_row<P: Pixel>(
    dc: &DrawCtx,
    rs: &RasterState,
    colortex: &mut Texture,
    y: i32,
    startx: i32,
    endx: i32,
    use_discard: bool,
    depth: Option<(&DepthRow, bool)>,
    z: u32,
    cursor: Option<&DepthCursor>,
) {
    debug_assert!(colortex.delay_clear > 0);
    let Some(rows) = colortex.cleared_rows.as_mut() else { return };
    let Some(mask) = rows.get_mut((y / 32) as usize) else { return };
    let bit = 1u32 << (y & 31);
    if *mask & bit != 0 {
        return;
    }
    *mask |= bit;
    colortex.delay_clear -= 1;
    let width = colortex.width;
    if rs.blend_key != BLEND_KEY_NONE || use_discard {
        force_clear_row::<P>(colortex, y, 0, 0);
    } else if let Some((row, flat)) = depth {
        match cursor {
            Some(c) if !flat => {
                let passed = { *c }.check_passed(row, z, dc.depthfunc, false);
                if startx > 0 || startx + passed < width {
                    force_clear_row::<P>(colortex, y, startx, startx + passed);
                }
            }
            _ => force_clear_row::<P>(colortex, y, 0, 0),
        }
    } else if startx > 0 || endx < width {
        force_clear_row::<P>(colortex, y, startx, endx);
    }
}

/// `draw_depth_span(z, buf, cursor)`: the passing runs of a row that is still runs.
///
/// # Safety
/// `buf` is the pixel at the cursor's start in the color row the depth row belongs to.
unsafe fn draw_depth_span<P: Pixel>(t: &mut SpanTarget<'_>, z: u32, mut buf: *mut P, row: &DepthRow, cursor: &mut DepthCursor) {
    loop {
        let mut span = cursor.check_passed(row, z, t.dc.depthfunc, t.dc.depthmask);
        if span <= 0 {
            break;
        }
        // SAFETY (for the block): the passed runs lie inside the span, which lies inside the color row.
        unsafe {
            if span >= 4 {
                if P::has_draw_span(t.fs) {
                    let drawn = P::draw_span(t.fs, t.rs, buf, span & !3);
                    buf = buf.wrapping_offset(drawn as isize);
                    span -= drawn;
                }
                while span >= 4 {
                    t.fs.run_dyn(t.rs, false);
                    discard_output::<false, P>(t.rs, t.fs.state(), buf, 4);
                    buf = buf.wrapping_add(4);
                    span -= 4;
                }
            }
            if span > 0 {
                t.fs.run_dyn(t.rs, false);
                discard_output::<false, P>(t.rs, t.fs.state(), buf, span);
                buf = buf.wrapping_offset(span as isize);
            }
        }
        let skip = cursor.skip_failed(row, z, t.dc.depthfunc);
        if skip <= 0 {
            break;
        }
        t.fs.skip_dyn(false, skip - if span > 0 { 4 - span } else { 0 });
        buf = buf.wrapping_offset(skip as isize);
    }
}

/// `draw_span<DISCARD, W>(buf, depth, span, z)`: 4-pixel chunks, with the depth test if `depth` is given.
///
/// # Safety
/// `buf..buf + span` are pixels of one row of the target; `depth` is that row's depth samples from the same x.
unsafe fn draw_span<const DISCARD: bool, const W: bool, P: Pixel>(
    t: &mut SpanTarget<'_>,
    mut buf: *mut P,
    depth: Option<(&DepthRow, usize)>,
    mut span: i32,
    z: ZSource,
) {
    let zsrc = |fs: &FragmentShaderState| match z {
        ZSource::Const(z) => I32::splat(z as i32),
        ZSource::Pack => pack_depth(fs),
    };
    // SAFETY (for the block): each chunk is inside the span the caller vouches for.
    unsafe {
        if let Some((row, mut at)) = depth {
            while span >= 4 {
                let src = zsrc(t.fs.state());
                if let Some(zmask) = check_depth::<DISCARD>(t.dc, src, row, at, 4) {
                    t.fs.run_dyn(t.rs, W);
                    P::mask_output(t.rs, t.fs.state(), buf, zmask, 4);
                    if DISCARD {
                        discard_depth(t.dc, t.fs.state(), src, row, at, zmask, 4);
                    }
                } else {
                    t.fs.skip_dyn(W, 4);
                }
                span -= 4;
                buf = buf.wrapping_add(4);
                at += 4;
            }
            if span > 0 {
                let src = zsrc(t.fs.state());
                if let Some(zmask) = check_depth::<DISCARD>(t.dc, src, row, at, span) {
                    t.fs.run_dyn(t.rs, W);
                    P::mask_output(t.rs, t.fs.state(), buf, zmask, span);
                    if DISCARD {
                        discard_depth(t.dc, t.fs.state(), src, row, at, zmask, span);
                    }
                }
            }
        } else {
            while span >= 4 {
                t.fs.run_dyn(t.rs, W);
                discard_output::<DISCARD, P>(t.rs, t.fs.state(), buf, 4);
                span -= 4;
                buf = buf.wrapping_add(4);
            }
            if span > 0 {
                t.fs.run_dyn(t.rs, W);
                discard_output::<DISCARD, P>(t.rs, t.fs.state(), buf, span);
            }
        }
    }
}

/// The row pointer of row `y` of `t` (wrapping; dereferenced only for rows inside the clip rectangle).
fn row_ptr<P>(t: &Texture, y: i32) -> *mut P {
    t.sample_ptr(0, y) as *mut P
}

/// Steps an edge's end points to the next vertex that does not loop back (`STEP_EDGE`); false when the edge
/// crossed `end` and the polygon is done.
#[allow(clippy::too_many_arguments)]
fn step_edge<T: Copy>(y: f32, e0i: &mut usize, e0: &mut T, e1i: &mut usize, e1: &mut T, p: &[T], step: impl Fn(usize) -> usize, end: usize, ey: impl Fn(&T) -> f32) -> bool {
    loop {
        *e0i = *e1i;
        *e0 = *e1;
        *e1i = step(*e1i);
        *e1 = p[*e1i];
        if *e0i == end {
            return false;
        }
        // `while (y > e1.y)`: a NaN ends the search, as in the C++.
        #[allow(clippy::neg_cmp_op_on_partial_ord)]
        let done = !(y > ey(e1));
        if done {
            return true;
        }
    }
}

/// `draw_quad_spans<P>(nump, p, z, interp_outs, colortex, depthtex, clipRect)`.
#[allow(clippy::too_many_arguments)]
fn draw_quad_spans<P: Pixel>(
    t: &mut SpanTarget<'_>,
    use_clip_distance: bool,
    nump: usize,
    p: &[vec2_scalar; 4],
    z: u32,
    interp_outs: &[Interpolants; 4],
    colortex: &mut Texture,
    depthtex: &mut Texture,
    clip_rect: &ClipRect,
) {
    debug_assert!(nump == 3 || nump == 4);
    let next_point = |idx: usize| if idx + 1 < nump { idx + 1 } else { 0 };
    let prev_point = |idx: usize| if idx >= 1 { idx - 1 } else { nump - 1 };
    let (mut l0i, mut r0i, mut l1i, mut r1i);
    {
        let top = if nump > 3 && p[3].y < p[2].y {
            if p[0].y < p[1].y {
                if p[0].y < p[3].y { 0 } else { 3 }
            } else if p[1].y < p[3].y {
                1
            } else {
                3
            }
        } else if p[0].y < p[1].y {
            if p[0].y < p[2].y { 0 } else { 2 }
        } else if p[1].y < p[2].y {
            1
        } else {
            2
        };
        let next = next_point(top);
        let prev = prev_point(top);
        if p[top].y == p[next].y {
            l0i = next;
            l1i = next_point(next);
            r0i = top;
            r1i = prev;
        } else if p[top].y == p[prev].y {
            l0i = top;
            l1i = next;
            r0i = prev;
            r1i = prev_point(prev);
        } else {
            l0i = top;
            r0i = top;
            l1i = next;
            r1i = prev;
        }
    }
    let (mut l0, mut r0, mut l1, mut r1) = (p[l0i], p[r0i], p[l1i], p[r1i]);
    debug_assert!(l0.y == r0.y);
    let aa_round = if t.rs.swgl_ClipFlags & SWGL_CLIP_FLAG_AA != 0 { 0.0f32 } else { 0.5f32 };
    let mut y = (max(min(l0.y, clip_rect.y1), clip_rect.y0) + aa_round).floor() + 0.5f32;
    let aa = t.rs.swgl_AAEdgeMask;
    let mut left = Edge2::new(y, l0, l1, &interp_outs[l0i], &interp_outs[l1i], l1i, aa);
    let mut right = Edge2::new(y, r0, r1, &interp_outs[r0i], &interp_outs[r1i], r0i, aa);
    let flipped = check_if_edges_flipped(l0, l1, r0, r1);
    if flipped {
        core::mem::swap(&mut left, &mut right);
    }
    let mut yi = f32_to_i32_scalar(y);
    let mut check_y = min(min(l1.y, r1.y), clip_rect.y1);
    let mut clip_span = clip_rect.x_range().clip(x_range(l0.x, l1.x).merge(x_range(r0.x, r1.x)));
    loop {
        if y > check_y {
            if y > clip_rect.y1 {
                break;
            }
            if y > l1.y {
                if !step_edge(y, &mut l0i, &mut l0, &mut l1i, &mut l1, p, next_point, r1i, |q| q.y) {
                    return;
                }
                let e = Edge2::new(y, l0, l1, &interp_outs[l0i], &interp_outs[l1i], l1i, aa);
                if flipped { right = e } else { left = e }
            }
            if y > r1.y {
                if !step_edge(y, &mut r0i, &mut r0, &mut r1i, &mut r1, p, prev_point, l1i, |q| q.y) {
                    return;
                }
                let e = Edge2::new(y, r0, r1, &interp_outs[r0i], &interp_outs[r1i], r0i, aa);
                if flipped { left = e } else { right = e }
            }
            clip_span = clip_rect.x_range().clip(x_range(l0.x, l1.x).merge(x_range(r0.x, r1.x)));
            check_y = min((min(l1.y, r1.y) - aa_round).ceil(), clip_rect.y1);
        }

        if past_stripe(t.dc, yi) {
            break;
        }
        let fbuf: *mut P = row_ptr(colortex, yi);
        'span: {
            if !owns_row(t.dc, yi) {
                break 'span;
            }
            let mut span = aa_span(t.rs, fbuf, &left, &right, &clip_span);
            if span.len() <= 0 {
                break 'span;
            }
            if use_clip_distance {
                span = span.intersect(clip_distance_range(&left, &right));
                if span.len() <= 0 {
                    break 'span;
                }
            }
            t.dc.shaded_rows += 1;
            t.dc.shaded_pixels += span.len();
            let mut buf = fbuf.wrapping_offset(span.start as isize);
            let depth_row = if depthtex.cleared() { DepthRow::new(depthtex, yi) } else { None };
            let mut depth_at: Option<usize> = None;
            let mut cursor = None;
            let use_discard = t.fs.use_discard();
            if let Some(row) = &depth_row {
                if use_discard {
                    flatten_depth_runs(row, depthtex.width as usize);
                    depth_at = Some(span.start as usize);
                } else if !is_flat(row.get(0)) {
                    let mut c = DepthCursor::new(row, depthtex.width as usize, span.start, span.len());
                    let skipped = c.skip_failed(row, z, t.dc.depthfunc);
                    if skipped < 0 {
                        break 'span;
                    }
                    buf = buf.wrapping_offset(skipped as isize);
                    span.start += skipped;
                    cursor = Some(c);
                    depth_at = Some(0);
                } else {
                    depth_at = Some(span.start as usize);
                }
            }
            if colortex.delay_clear != 0 {
                let depth = depth_row.as_ref().zip(depth_at).map(|(r, at)| (r, is_flat(r.get(at.min(r.len.saturating_sub(1))))));
                prepare_row::<P>(t.dc, t.rs, colortex, yi, span.start, span.end, use_discard, depth, z, cursor.as_ref());
            }
            {
                let fs = t.fs.state_mut();
                fs.gl_FragCoord.x = init_interp(span.start as f32 + 0.5f32, 1.0f32);
                fs.gl_FragCoord.y = Float::splat(y);
                let mut step_scale = 1.0f32 / (right.x - left.x);
                if !step_scale.is_finite() {
                    step_scale = 0.0;
                }
                let step = right.interp.sub(left.interp).scale(step_scale);
                let o = left.interp.add(step.scale(span.start as f32 + 0.5f32 - left.x));
                t.fs.init_span_dyn(false, o.bytes(), step.bytes());
            }
            clip_rect.set_clip_mask(t.rs, span.start, f32_to_i32_scalar(y), buf);
            // SAFETY: `buf..buf + span.len()` is inside row `yi` of the color buffer: the span is clipped to
            // `clip_rect`, which is inside the texture, and the row is inside it as `y <= clip_rect.y1`.
            unsafe {
                if !use_discard {
                    if let (Some(row), Some(at)) = (&depth_row, depth_at) {
                        if let Some(c) = cursor.as_mut() {
                            draw_depth_span::<P>(t, z, buf, row, c);
                            break 'span;
                        }
                        draw_span::<false, false, P>(t, buf, Some((row, at)), span.len(), ZSource::Const(z));
                    } else {
                        if span.len() >= 4 && P::has_draw_span(t.fs) {
                            let drawn = P::draw_span(t.fs, t.rs, buf, span.len() & !3);
                            buf = buf.wrapping_offset(drawn as isize);
                            span.start += drawn;
                        }
                        draw_span::<false, false, P>(t, buf, None, span.len(), ZSource::Const(z));
                    }
                } else {
                    let depth = depth_row.as_ref().zip(depth_at);
                    draw_span::<true, false, P>(t, buf, depth, span.len(), ZSource::Const(z));
                }
            }
        }
        y += 1.0;
        yi += 1;
        left.next_row();
        right.next_row();
    }
}

/// `draw_perspective_spans<P>(nump, p, interp_outs, colortex, depthtex, clipRect)`.
#[allow(clippy::too_many_arguments)]
fn draw_perspective_spans<P: Pixel>(
    t: &mut SpanTarget<'_>,
    use_clip_distance: bool,
    nump: usize,
    p: &[vec4_scalar],
    interp_outs: &[Interpolants],
    colortex: &mut Texture,
    depthtex: &mut Texture,
    clip_rect: &ClipRect,
) {
    let next_point = |idx: usize| if idx + 1 < nump { idx + 1 } else { 0 };
    let prev_point = |idx: usize| if idx >= 1 { idx - 1 } else { nump - 1 };
    let (mut l0i, mut r0i, mut l1i, mut r1i);
    {
        let mut top = 0;
        for i in 1..nump {
            if p[i].y < p[top].y {
                top = i;
            }
        }
        l0i = top;
        let mut i = top + 1;
        while i < nump && p[i].y == p[top].y {
            l0i = i;
            i += 1;
        }
        if l0i == nump - 1 {
            let mut i = 0;
            while i <= top && p[i].y == p[top].y {
                l0i = i;
                i += 1;
            }
        }
        r0i = top;
        let mut i = top as isize - 1;
        while i >= 0 && p[i as usize].y == p[top].y {
            r0i = i as usize;
            i -= 1;
        }
        if r0i == 0 {
            let mut i = nump as isize - 1;
            while i >= top as isize && p[i as usize].y == p[top].y {
                r0i = i as usize;
                i -= 1;
            }
        }
        l1i = next_point(l0i);
        r1i = prev_point(r0i);
    }
    let (mut l0, mut r0, mut l1, mut r1) = (p[l0i], p[r0i], p[l1i], p[r1i]);
    debug_assert!(l0.y == r0.y);
    let aa_round = if t.rs.swgl_ClipFlags & SWGL_CLIP_FLAG_AA != 0 { 0.0f32 } else { 0.5f32 };
    let mut y = (max(min(l0.y, clip_rect.y1), clip_rect.y0) + aa_round).floor() + 0.5f32;
    let aa = t.rs.swgl_AAEdgeMask;
    let mut left = Edge3::new(y, l0, l1, &interp_outs[l0i], &interp_outs[l1i], l1i, aa);
    let mut right = Edge3::new(y, r0, r1, &interp_outs[r0i], &interp_outs[r1i], r0i, aa);
    let xy = |q: vec4_scalar| vec2_scalar { x: q.x, y: q.y };
    let flipped = check_if_edges_flipped(xy(l0), xy(l1), xy(r0), xy(r1));
    if flipped {
        core::mem::swap(&mut left, &mut right);
    }
    let mut yi = f32_to_i32_scalar(y);
    let mut check_y = min(min(l1.y, r1.y), clip_rect.y1);
    let mut clip_span = clip_rect.x_range().clip(x_range(l0.x, l1.x).merge(x_range(r0.x, r1.x)));
    loop {
        if y > check_y {
            if y > clip_rect.y1 {
                break;
            }
            if y > l1.y {
                if !step_edge(y, &mut l0i, &mut l0, &mut l1i, &mut l1, p, next_point, r1i, |q| q.y) {
                    return;
                }
                let e = Edge3::new(y, l0, l1, &interp_outs[l0i], &interp_outs[l1i], l1i, aa);
                if flipped { right = e } else { left = e }
            }
            if y > r1.y {
                if !step_edge(y, &mut r0i, &mut r0, &mut r1i, &mut r1, p, prev_point, l1i, |q| q.y) {
                    return;
                }
                let e = Edge3::new(y, r0, r1, &interp_outs[r0i], &interp_outs[r1i], r0i, aa);
                if flipped { left = e } else { right = e }
            }
            clip_span = clip_rect.x_range().clip(x_range(l0.x, l1.x).merge(x_range(r0.x, r1.x)));
            check_y = min((min(l1.y, r1.y) - aa_round).ceil(), clip_rect.y1);
        }

        if past_stripe(t.dc, yi) {
            break;
        }
        let fbuf: *mut P = row_ptr(colortex, yi);
        'span: {
            if !owns_row(t.dc, yi) {
                break 'span;
            }
            let mut span = aa_span(t.rs, fbuf, &left, &right, &clip_span);
            if span.len() <= 0 {
                break 'span;
            }
            if use_clip_distance {
                span = span.intersect(clip_distance_range(&left, &right));
                if span.len() <= 0 {
                    break 'span;
                }
            }
            t.dc.shaded_rows += 1;
            t.dc.shaded_pixels += span.len();
            let buf = fbuf.wrapping_offset(span.start as isize);
            let depth_row = if depthtex.cleared() { DepthRow::new(depthtex, yi) } else { None };
            let use_discard = t.fs.use_discard();
            if let Some(row) = &depth_row {
                flatten_depth_runs(row, depthtex.width as usize);
            }
            if colortex.delay_clear != 0 {
                let depth = depth_row.as_ref().map(|r| (r, true));
                prepare_row::<P>(t.dc, t.rs, colortex, yi, span.start, span.end, use_discard, depth, 0, None);
            }
            {
                let fs = t.fs.state_mut();
                fs.gl_FragCoord.x = init_interp(span.start as f32 + 0.5f32, 1.0f32);
                fs.gl_FragCoord.y = Float::splat(y);
                let mut step_scale = 1.0f32 / (right.p.x - left.p.x);
                if !step_scale.is_finite() {
                    step_scale = 0.0;
                }
                let step_zw = (right.zw() - left.zw()) * step_scale;
                let zw = left.zw() + step_zw * (span.start as f32 + 0.5f32 - left.p.x);
                fs.gl_FragCoord.z = init_interp(zw.x, step_zw.x);
                fs.gl_FragCoord.w = init_interp(zw.y, step_zw.y);
                fs.swgl_StepZW = step_zw;
                let step = right.interp.sub(left.interp).scale(step_scale);
                let o = left.interp.add(step.scale(span.start as f32 + 0.5f32 - left.p.x));
                t.fs.init_span_dyn(true, o.bytes(), step.bytes());
            }
            clip_rect.set_clip_mask(t.rs, span.start, f32_to_i32_scalar(y), buf);
            let depth = depth_row.as_ref().map(|r| (r, span.start as usize));
            // SAFETY: as in `draw_quad_spans`.
            unsafe {
                if !use_discard {
                    draw_span::<false, true, P>(t, buf, depth, span.len(), ZSource::Pack);
                } else {
                    draw_span::<true, true, P>(t, buf, depth, span.len(), ZSource::Pack);
                }
            }
        }
        y += 1.0;
        yi += 1;
        left.next_row();
        right.next_row();
    }
}

/// `nextafterf(x, toward)`.
fn nextafterf(x: f32, toward: f32) -> f32 {
    if x.is_nan() || toward.is_nan() {
        return x + toward;
    }
    if x == toward {
        return toward;
    }
    if x == 0.0 {
        return f32::from_bits(1).copysign(toward);
    }
    let bits = x.to_bits();
    let up = (toward > x) == (x > 0.0);
    f32::from_bits(if up { bits + 1 } else { bits - 1 })
}

/// `clip_side<AXIS>(nump, p, interp, outP, outInterp, outEdgeMask)`: the polygon clipped to `-w <= c <= w` on
/// `axis`, at most `nump + 2` points; 0 if it would produce more.
fn clip_side(
    axis: XYZW,
    nump: usize,
    p: &[vec4_scalar],
    interp: &[Interpolants],
    out_p: &mut [vec4_scalar],
    out_interp: &mut [Interpolants],
    out_edge_mask: &mut i32,
) -> usize {
    const POSITIVE: i32 = 1;
    const NEGATIVE: i32 = 2;
    let mut num_clip = 0usize;
    let mut edge_mask = *out_edge_mask;
    let mut prev = p[nump - 1];
    let mut prev_interp = interp[nump - 1];
    let mut prev_coord = prev.select(axis);
    let mut prev_mask = (if prev_coord < -prev.w { NEGATIVE } else { 0 }) | (if prev_coord > prev.w { POSITIVE } else { 0 });
    *out_edge_mask = 0;
    for i in 0..nump {
        let cur = p[i];
        let cur_interp = interp[i];
        let cur_coord = cur.select(axis);
        let cur_mask = (if cur_coord < -cur.w { NEGATIVE } else { 0 }) | (if cur_coord > cur.w { POSITIVE } else { 0 });
        if cur_mask & prev_mask == 0 {
            if prev_mask != 0 {
                if num_clip >= nump + 2 {
                    debug_assert!(false);
                    return 0;
                }
                let prev_side: f32 = if (prev_mask & NEGATIVE) != 0
                    && ((prev_mask & POSITIVE) == 0 || prev_coord * (cur.w - prev.w) < prev.w * (cur_coord - prev_coord))
                {
                    -1.0
                } else {
                    1.0
                };
                let prev_dist = prev_coord - prev_side * prev.w;
                let cur_dist = cur_coord - prev_side * cur.w;
                let mut k = prev_dist / (prev_dist - cur_dist);
                let mut clipped = prev + (cur - prev) * k;
                if prev_side * clipped.select(axis) > clipped.w {
                    k = nextafterf(k, 1.0);
                    clipped = prev + (cur - prev) * k;
                }
                out_p[num_clip] = clipped;
                out_interp[num_clip] = prev_interp.add(cur_interp.sub(prev_interp).scale(k));
                num_clip += 1;
            }
            if cur_mask != 0 {
                if num_clip >= nump + 2 {
                    debug_assert!(false);
                    return 0;
                }
                let cur_side: f32 = if (cur_mask & POSITIVE) != 0
                    && ((cur_mask & NEGATIVE) == 0 || prev_coord * (cur.w - prev.w) < prev.w * (cur_coord - prev_coord))
                {
                    1.0
                } else {
                    -1.0
                };
                let prev_dist = prev_coord - cur_side * prev.w;
                let cur_dist = cur_coord - cur_side * cur.w;
                let mut k = prev_dist / (prev_dist - cur_dist);
                let mut clipped = prev + (cur - prev) * k;
                if cur_side * clipped.select(axis) > clipped.w {
                    k = nextafterf(k, 0.0);
                    clipped = prev + (cur - prev) * k;
                }
                out_p[num_clip] = clipped;
                out_interp[num_clip] = prev_interp.add(cur_interp.sub(prev_interp).scale(k));
                *out_edge_mask |= (edge_mask & 1) << num_clip;
                num_clip += 1;
            }
        }
        if cur_mask == 0 {
            if num_clip >= nump + 2 {
                debug_assert!(false);
                return 0;
            }
            out_p[num_clip] = cur;
            out_interp[num_clip] = cur_interp;
            *out_edge_mask |= (edge_mask & 1) << num_clip;
            num_clip += 1;
        }
        prev = cur;
        prev_interp = cur_interp;
        prev_coord = cur_coord;
        prev_mask = cur_mask;
        edge_mask >>= 1;
    }
    num_clip
}

/// `draw_perspective_clipped(nump, p_clip, interp_clip, colortex, depthtex)`.
#[allow(clippy::too_many_arguments)]
fn draw_perspective_clipped(
    t: &mut SpanTarget<'_>,
    use_clip_distance: bool,
    nump: usize,
    p_clip: &[vec4_scalar],
    interp_clip: &[Interpolants],
    colortex: &mut Texture,
    depthtex: &mut Texture,
) {
    let clip_rect = ClipRect::new(t.dc, t.rs, colortex);
    let pts: Vec<(f32, f32)> = p_clip[..nump].iter().map(|q| (q.x, q.y)).collect();
    if !clip_rect.overlaps(&pts) {
        return;
    }
    match colortex.internal_format {
        GL_RGBA8 => draw_perspective_spans::<u32>(t, use_clip_distance, nump, p_clip, interp_clip, colortex, depthtex, &clip_rect),
        GL_R8 => draw_perspective_spans::<u8>(t, use_clip_distance, nump, p_clip, interp_clip, colortex, depthtex, &clip_rect),
        _ => debug_assert!(false),
    }
}

fn lane(v: &vec4, i: usize) -> vec4_scalar {
    vec4_scalar { x: v.x.0[i], y: v.y.0[i], z: v.z.0[i], w: v.w.0[i] }
}

/// `draw_perspective(nump, interp_outs, colortex, depthtex)`.
#[allow(clippy::too_many_arguments)]
fn draw_perspective(
    t: &mut SpanTarget<'_>,
    use_clip_distance: bool,
    pos: vec4,
    nump: usize,
    interp_outs: &[Interpolants; 4],
    colortex: &mut Texture,
    depthtex: &mut Texture,
) {
    debug_assert!(nump >= 3);
    let vp = t.dc.viewport;
    let scale = vec3_scalar { x: vp.width() as f32, y: vp.height() as f32, z: 1.0 } * 0.5f32;
    let o = vp.origin();
    let offset = vec3_scalar {
        x: o.x.wrapping_sub(colortex.offset.x) as f32,
        y: o.y.wrapping_sub(colortex.offset.y) as f32,
        z: 0.0,
    } + scale;
    let inside = pos.z.cmp_gt(-pos.w) & pos.z.cmp_lt(pos.w);
    if test_all(inside) {
        let w = Float::splat(1.0) / pos.w;
        let sx = pos.x * w * Float::splat(scale.x) + Float::splat(offset.x);
        let sy = pos.y * w * Float::splat(scale.y) + Float::splat(offset.y);
        let sz = pos.z * w * Float::splat(scale.z) + Float::splat(offset.z);
        let p: [vec4_scalar; 4] =
            core::array::from_fn(|i| vec4_scalar { x: sx.0[i], y: sy.0[i], z: sz.0[i], w: w.0[i] });
        draw_perspective_clipped(t, use_clip_distance, nump, &p, interp_outs, colortex, depthtex);
    } else {
        let p: [vec4_scalar; 4] = core::array::from_fn(|i| lane(&pos, i));
        let mut p_clip = [vec4_scalar::default(); 4 + 6];
        let mut interp_clip = [Interpolants::default(); 4 + 6];
        let mut mask = t.rs.swgl_AAEdgeMask;
        let mut nump = clip_side(Z, nump, &p, interp_outs, &mut p_clip, &mut interp_clip, &mut mask);
        t.rs.swgl_AAEdgeMask = mask;
        if nump < 3 {
            return;
        }
        // A W of 0 or less left after the Z planes: clip against X and Y too (the C++ loop stops at the first).
        if p_clip[..nump].iter().any(|q| q.w <= 0.0) {
            let mut p_tmp = [vec4_scalar::default(); 4 + 6];
            let mut interp_tmp = [Interpolants::default(); 4 + 6];
            let mut mask = t.rs.swgl_AAEdgeMask;
            nump = clip_side(X, nump, &p_clip, &interp_clip, &mut p_tmp, &mut interp_tmp, &mut mask);
            t.rs.swgl_AAEdgeMask = mask;
            if nump < 3 {
                return;
            }
            let mut mask = t.rs.swgl_AAEdgeMask;
            nump = clip_side(Y, nump, &p_tmp, &interp_tmp, &mut p_clip, &mut interp_clip, &mut mask);
            t.rs.swgl_AAEdgeMask = mask;
            if nump < 3 {
                return;
            }
        }
        for q in p_clip.iter_mut().take(nump) {
            let w = 1.0f32 / q.w;
            *q = if w.is_finite() {
                vec4_scalar { x: q.x * w * scale.x + offset.x, y: q.y * w * scale.y + offset.y, z: q.z * w * scale.z + offset.z, w }
            } else {
                vec4_scalar::default()
            };
        }
        draw_perspective_clipped(t, use_clip_distance, nump, &p_clip, &interp_clip, colortex, depthtex);
    }
}

/// The vertex stage of `draw_quad`: the clip-space positions, whether clip distances are used, and the interpolants.
fn run_vertex(rs: &mut RasterState, prog: &mut dyn ProgramImpl) -> (vec4, bool, [Interpolants; 4]) {
    let mut interp_outs = [Interpolants::default(); 4];
    rs.swgl_ClipFlags = 0;
    let vs = prog.get_vertex_shader();
    vs.run_primitive(rs, interps_bytes_mut(&mut interp_outs), core::mem::size_of::<Interpolants>());
    (vs.state().gl_Position, vs.use_clip_distance(), interp_outs)
}

/// `draw_quad(nump, colortex, depthtex)`: runs the vertex shader for the primitive loaded with `load_attribs`, then
/// rasterizes it.
pub fn draw_quad(
    dc: &mut DrawCtx,
    rs: &mut RasterState,
    prog: &mut dyn ProgramImpl,
    nump: i32,
    colortex: &mut Texture,
    depthtex: &mut Texture,
) {
    let (pos, use_clip_distance, interp_outs) = run_vertex(rs, prog);
    raster_quad(dc, rs, prog.get_fragment_shader(), nump, pos, use_clip_distance, &interp_outs, colortex, depthtex);
}

/// A primitive after its vertex stage, for a draw split across threads (raster/DESIGN.md, "Parallel rasterizing"):
/// what `draw_quad` rasterizes, with the raster state and fragment shader (flat varyings included) as the vertex
/// shader left them, and the rows and area it can cover.
pub struct Shaded {
    nump: i32,
    pos: vec4,
    use_clip_distance: bool,
    interp_outs: [Interpolants; 4],
    /// Texture rows `rows.0..rows.1` hold every row the primitive can touch.
    pub rows: (i32, i32),
    /// Pixels of its bounding box inside the clip rectangle.
    pub area: i64,
}

/// Runs the vertex stage of `draw_quad` for the primitive loaded with `load_attribs` and keeps its outputs.
pub fn shade_quad(dc: &DrawCtx, rs: &mut RasterState, prog: &mut dyn ProgramImpl, nump: i32, colortex: &Texture) -> Shaded {
    let (pos, use_clip_distance, interp_outs) = run_vertex(rs, prog);
    let clip = dc.apply_scissor(colortex);
    let n = nump.clamp(0, 4) as usize;
    let w0 = pos.w.0[0];
    // A 2D primitive's bounding box, one pixel wider on each side (lines are widened by one, AA rounds out); the
    // perspective path clips against the frustum, so it may cover the whole clip rectangle.
    let bounds = if n > 0 && !test_any(pos.w.cmp_ne(Float::splat(w0))) && w0 > 0.0 && (1.0 / w0).is_finite() {
        let w = 1.0f32 / w0;
        let vp = dc.viewport;
        let o = vp.origin();
        let (ox, oy) = (o.x.wrapping_sub(colortex.offset.x) as f32, o.y.wrapping_sub(colortex.offset.y) as f32);
        let xs = pos.x.0[..n].iter().map(|&x| (x * w + 1.0) * 0.5 * vp.width() as f32 + ox);
        let ys = pos.y.0[..n].iter().map(|&y| (y * w + 1.0) * 0.5 * vp.height() as f32 + oy);
        let (x0, x1) = xs.fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), x| (a.min(x), b.max(x)));
        let (y0, y1) = ys.fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), y| (a.min(y), b.max(y)));
        let c = |v: f32| v.clamp(-1.0e6, 1.0e6) as i32;
        if x0.is_finite() && x1.is_finite() && y0.is_finite() && y1.is_finite() {
            clip.intersection(&IntRect::new(c(x0.floor()) - 1, c(y0.floor()) - 1, c(x1.ceil()) + 2, c(y1.ceil()) + 2))
        } else {
            clip
        }
    } else {
        clip
    };
    let (rows, area) = if bounds.width() > 0 && bounds.height() > 0 {
        ((bounds.y0, bounds.y1), i64::from(bounds.width()) * i64::from(bounds.height()))
    } else {
        ((0, 0), 0)
    };
    Shaded { nump, pos, use_clip_distance, interp_outs, rows, area }
}

/// Whether rows `rows.0..rows.1` meet stripe `s`.
pub fn stripe_meets(rows: (i32, i32), s: i64) -> bool {
    let (y0, y1) = (i64::from(rows.0.max(0)), i64::from(rows.1.max(0)));
    y0 < y1 && s <= rules::MAX_SIDE && rules::stripe_span(s, y0.min(rules::MAX_SIDE), y1.min(rules::MAX_SIDE)).is_some()
}

/// Rasterizes a primitive `shade_quad` kept, with the raster state and fragment shader its vertex stage left.
pub fn raster_shaded(
    dc: &mut DrawCtx,
    rs: &mut RasterState,
    fs: &mut dyn FragmentShaderImpl,
    sh: &Shaded,
    colortex: &mut Texture,
    depthtex: &mut Texture,
) {
    raster_quad(dc, rs, fs, sh.nump, sh.pos, sh.use_clip_distance, &sh.interp_outs, colortex, depthtex);
}

/// The rasterizing half of `draw_quad`.
#[allow(clippy::too_many_arguments)]
fn raster_quad(
    dc: &mut DrawCtx,
    rs: &mut RasterState,
    fs: &mut dyn FragmentShaderImpl,
    nump: i32,
    pos: vec4,
    use_clip_distance: bool,
    interp_outs: &[Interpolants; 4],
    colortex: &mut Texture,
    depthtex: &mut Texture,
) {
    let mut t = SpanTarget { dc, rs, fs };
    if test_any(pos.w.cmp_ne(Float::splat(pos.w.0[0]))) {
        draw_perspective(&mut t, use_clip_distance, pos, nump.max(0) as usize, interp_outs, colortex, depthtex);
        return;
    }
    let mut w = 1.0f32 / pos.w.0[0];
    if !w.is_finite() {
        w = 0.0;
    }
    let vp = t.dc.viewport;
    let (vw, vh) = (vp.width() as f32, vp.height() as f32);
    let o = vp.origin();
    let (ox, oy) = (o.x.wrapping_sub(colortex.offset.x) as f32, o.y.wrapping_sub(colortex.offset.y) as f32);
    let mut p: [vec2_scalar; 4] = core::array::from_fn(|i| vec2_scalar {
        x: (pos.x.0[i] * w + 1.0f32) * 0.5f32 * vw + ox,
        y: (pos.y.0[i] * w + 1.0f32) * 0.5f32 * vh + oy,
    });

    let clip_rect = ClipRect::new(t.dc, t.rs, colortex);
    let n = nump.clamp(0, 4) as usize;
    let pts: Vec<(f32, f32)> = p[..n].iter().map(|q| (q.x, q.y)).collect();
    if !clip_rect.overlaps(&pts) {
        return;
    }
    let screen_z = (pos.z.0[0] * w + 1.0f32) * 0.5f32;
    if screen_z < 0.0 || screen_z > 1.0 {
        return;
    }
    let z = f32_to_u32_scalar(MAX_DEPTH_VALUE as f32 * screen_z);
    {
        let fs = t.fs.state_mut();
        fs.gl_FragCoord.z = Float::splat(screen_z);
        fs.gl_FragCoord.w = Float::splat(w);
    }
    let mut nump = nump;
    if nump == 2 {
        let r = |v: f32| f32_to_i32_scalar(v + 0.5f32);
        if r(p[0].y) == r(p[1].y) {
            p[2].y = (1 + r(p[1].y)) as f32;
            p[3].y = p[2].y;
            if r(p[0].x) == r(p[1].x) {
                p[1].x += 1.0;
                p[2].x += 1.0;
            }
        } else {
            p[2].x += 1.0;
            p[3].x += 1.0;
        }
        nump = 4;
    }
    let nump = nump.clamp(3, 4) as usize;
    match colortex.internal_format {
        GL_RGBA8 => draw_quad_spans::<u32>(&mut t, use_clip_distance, nump, &p, z, interp_outs, colortex, depthtex, &clip_rect),
        GL_R8 => draw_quad_spans::<u8>(&mut t, use_clip_distance, nump, &p, z, interp_outs, colortex, depthtex, &clip_rect),
        _ => debug_assert!(false),
    }
}

/// `draw_elements<INDEX>(count, instancecount, offset, v, colortex, depthtex)` for `index_size`-byte indices in
/// `indices` (the element array buffer's bytes).
#[allow(clippy::too_many_arguments)]
pub fn draw_elements(
    dc: &mut DrawCtx,
    rs: &mut RasterState,
    prog: &mut dyn ProgramImpl,
    index_size: usize,
    count: i32,
    instancecount: i32,
    offset: usize,
    attribs: &[VertexAttrib<'_>],
    indices: &[u8],
    colortex: &mut Texture,
    depthtex: &mut Texture,
) {
    for_each_primitive(prog, index_size, count, instancecount, offset, attribs, indices, &mut |prog, nump| {
        draw_quad(dc, rs, prog, nump, colortex, depthtex)
    });
}

/// The primitives of `draw_elements`, in order: each loaded with `load_attribs`, then passed to `f` with its point
/// count (`draw_quad`, or `shade_quad` for a split draw).
#[allow(clippy::too_many_arguments)]
pub fn for_each_primitive(
    prog: &mut dyn ProgramImpl,
    index_size: usize,
    count: i32,
    instancecount: i32,
    offset: usize,
    attribs: &[VertexAttrib<'_>],
    indices: &[u8],
    f: &mut dyn FnMut(&mut dyn ProgramImpl, i32),
) {
    if indices.is_empty() || offset >= indices.len() {
        return;
    }
    debug_assert!(offset & (index_size - 1) == 0);
    let count = (count as usize).min((indices.len() - offset) / index_size);
    let idx = |i: usize| -> u32 {
        let o = offset + i * index_size;
        if index_size == 2 {
            u32::from(u16::from_ne_bytes([indices[o], indices[o + 1]]))
        } else {
            u32::from_ne_bytes([indices[o], indices[o + 1], indices[o + 2], indices[o + 3]])
        }
    };
    if count == 6 && idx(1) == idx(0).wrapping_add(1) && idx(2) == idx(0).wrapping_add(2) && idx(5) == idx(0).wrapping_add(3) {
        prog.get_vertex_shader().load_attribs(attribs, idx(0), 0, 4);
        f(prog, 4);
        for instance in 1..instancecount {
            prog.get_vertex_shader().load_attribs(attribs, idx(0), instance, 0);
            f(prog, 4);
        }
    } else {
        for instance in 0..instancecount {
            let mut i = 0;
            while i + 3 <= count {
                if idx(i + 1) != idx(i).wrapping_add(1) || idx(i + 2) != idx(i).wrapping_add(2) {
                    i += 3;
                    continue;
                }
                if i + 6 <= count && idx(i + 5) == idx(i).wrapping_add(3) {
                    prog.get_vertex_shader().load_attribs(attribs, idx(i), instance, 4);
                    f(prog, 4);
                    i += 3;
                } else {
                    prog.get_vertex_shader().load_attribs(attribs, idx(i), instance, 3);
                    f(prog, 3);
                }
                i += 3;
            }
        }
    }
}
