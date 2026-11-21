/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Port of swgl's `texture.h` (texel fetch, nearest and linear sampling, `textureLinear*`, the Gaussian blur taps),
//! together with the parts of `gl.cc` that sampling needs: `IntRect`, the `Texture` object with its buffer,
//! delayed-clear state and clears (`clear_buffer`, `force_clear`, `prepare_texture`, `request_clear`), and the
//! sampler binding (`init_sampler`, `null_sampler`, `init_filter`, `lookup_sampler`, `lookup_isampler`).
//!
//! Every texel read goes through [`load_texels`]: the sampler carries the length of its buffer, and a read that
//! would leave it gives zero bytes where the C++ reads whatever lies there. The C++ reads outside the buffer in two
//! places, both with zero weight in the result: a linear filter on a row of fewer than 2 pixels (the tap before
//! the row) and the 4-pixel YUY2 chunk at the end of an odd row; the result is the same whatever the bytes are.
//!
//! The integer instructions the C++ picks per target (`_mm_min_epi16` for `clampCoord`, `_mm_mulhi_epi16` vs
//! `vqrdmulhq_s16` for the 16-bit lerps) are reproduced by their exact integer semantics under the same `cfg`.

// Lane loops index several arrays by the lane number, as the C++ lane expressions do.
#![allow(clippy::needless_range_loop)]

use crate::glsl::vector;
use crate::glsl::{
    bit_cast, cast, combine, combine4, floor, force_scalar, high_half, if_then_else, isampler2DRGBA32I_impl,
    isampler2D_impl, ivec2, ivec2_scalar, ivec4, ivec4_scalar, load_bytes, low_half, mix, roundzero, sampler2DR8_impl,
    sampler2DRG8_impl, sampler2DRGBA32F_impl, sampler2DRGBA8_impl, sampler2DRect_impl, sampler2D_impl, samplerCommon,
    shuffle, store_bytes, test_any, unaligned_load, vec2, vec2_scalar, vec4, vec4_scalar, zip2_high, zip2_low,
    zip_high, zip_low, Float, Lane, Pod, TexelPtr, TextureFilter, TextureFormat, Vector, I16, I32, U16, U32, V16, V2,
    V4, V8,
};
use crate::state::RasterState;
use core::sync::atomic::AtomicI32;

// ---- gl_defs.h: the enums textures use ------------------------------------------------------------------------

pub const GL_NEAREST: u32 = 0x2600;
pub const GL_LINEAR: u32 = 0x2601;
pub const GL_NEAREST_MIPMAP_NEAREST: u32 = 0x2700;
pub const GL_LINEAR_MIPMAP_NEAREST: u32 = 0x2701;
pub const GL_NEAREST_MIPMAP_LINEAR: u32 = 0x2702;
pub const GL_LINEAR_MIPMAP_LINEAR: u32 = 0x2703;
pub const GL_RGBA32F: u32 = 0x8814;
pub const GL_RGBA32I: u32 = 0x8D82;
pub const GL_RGBA_INTEGER: u32 = 0x8D99;
pub const GL_RGBA8: u32 = 0x8058;
pub const GL_BGRA8: u32 = 0x93A1;
pub const GL_RGBA: u32 = 0x1908;
pub const GL_BGRA: u32 = 0x80E1;
pub const GL_R8: u32 = 0x8229;
pub const GL_RED: u32 = 0x1903;
pub const GL_RG8: u32 = 0x822B;
pub const GL_RG: u32 = 0x8227;
pub const GL_R16: u32 = 0x822A;
pub const GL_RG16: u32 = 0x822C;
pub const GL_DEPTH_COMPONENT: u32 = 0x1902;
pub const GL_DEPTH_COMPONENT16: u32 = 0x81A5;
pub const GL_DEPTH_COMPONENT24: u32 = 0x81A6;
pub const GL_DEPTH_COMPONENT32: u32 = 0x81A7;
pub const GL_RGB_RAW_422_APPLE: u32 = 0x8A51;

// ---- gl.cc: IntPoint, IntRect ---------------------------------------------------------------------------------

pub type IntPoint = ivec2_scalar;

/// `IntRect` (gl.cc): `[x0, x1) x [y0, y1)`, with C++ `int` arithmetic (wrapping where the C++ would overflow).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IntRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IntRect {
    #[inline(always)]
    pub const fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Self { x0, y0, x1, y1 }
    }
    /// `IntRect(IntPoint origin, IntPoint size)`.
    #[inline(always)]
    pub fn from_origin_size(origin: IntPoint, size: IntPoint) -> Self {
        Self::new(origin.x, origin.y, origin.x.wrapping_add(size.x), origin.y.wrapping_add(size.y))
    }
    #[inline(always)]
    pub fn width(&self) -> i32 {
        self.x1.wrapping_sub(self.x0)
    }
    #[inline(always)]
    pub fn height(&self) -> i32 {
        self.y1.wrapping_sub(self.y0)
    }
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }
    #[inline(always)]
    pub fn origin(&self) -> IntPoint {
        IntPoint::new(self.x0, self.y0)
    }
    #[inline(always)]
    pub fn same_size(&self, o: &IntRect) -> bool {
        self.width() == o.width() && self.height() == o.height()
    }
    #[inline(always)]
    pub fn contains(&self, o: &IntRect) -> bool {
        o.x0 >= self.x0 && o.y0 >= self.y0 && o.x1 <= self.x1 && o.y1 <= self.y1
    }
    #[inline(always)]
    pub fn intersect(&mut self, o: &IntRect) -> &mut Self {
        self.x0 = self.x0.max(o.x0);
        self.y0 = self.y0.max(o.y0);
        self.x1 = self.x1.min(o.x1);
        self.y1 = self.y1.min(o.y1);
        self
    }
    #[inline(always)]
    pub fn intersection(&self, o: &IntRect) -> IntRect {
        let mut r = *self;
        r.intersect(o);
        r
    }
    /// `scale(srcWidth, srcHeight, dstWidth, dstHeight, roundIn)`. Division by zero, which the C++ leaves
    /// undefined (it traps on x86), gives 0 here.
    pub fn scale(
        &mut self,
        src_width: i32,
        src_height: i32,
        dst_width: i32,
        dst_height: i32,
        round_in: bool,
    ) -> &mut Self {
        let div = |a: i32, b: i32| if b == 0 { 0 } else { a.wrapping_div(b) };
        self.x0 = div(
            self.x0.wrapping_mul(dst_width).wrapping_add(if round_in { src_width.wrapping_sub(1) } else { 0 }),
            src_width,
        );
        self.y0 = div(
            self.y0.wrapping_mul(dst_height).wrapping_add(if round_in { src_height.wrapping_sub(1) } else { 0 }),
            src_height,
        );
        self.x1 = div(self.x1.wrapping_mul(dst_width), src_width);
        self.y1 = div(self.y1.wrapping_mul(dst_height), src_height);
        self
    }
    pub fn invert_y(&mut self, offset: i32) {
        self.y0 = offset.wrapping_sub(self.y0);
        self.y1 = offset.wrapping_sub(self.y1);
        core::mem::swap(&mut self.y0, &mut self.y1);
    }
    #[inline(always)]
    pub fn offset(&mut self, o: &IntPoint) -> &mut Self {
        self.x0 = self.x0.wrapping_add(o.x);
        self.y0 = self.y0.wrapping_add(o.y);
        self.x1 = self.x1.wrapping_add(o.x);
        self.y1 = self.y1.wrapping_add(o.y);
        self
    }
}

impl core::ops::Add<IntPoint> for IntRect {
    type Output = IntRect;
    #[inline(always)]
    fn add(self, o: IntPoint) -> IntRect {
        let mut r = self;
        r.offset(&o);
        r
    }
}

impl core::ops::Sub<IntPoint> for IntRect {
    type Output = IntRect;
    #[inline(always)]
    fn sub(self, o: IntPoint) -> IntRect {
        let mut r = self;
        r.offset(&IntPoint::new(o.x.wrapping_neg(), o.y.wrapping_neg()));
        r
    }
}

// ---- gl.cc: formats ---------------------------------------------------------------------------------------------

/// `bytes_for_internal_format` (gl.cc); 0 for an unknown format (the C++ asserts).
pub fn bytes_for_internal_format(internal_format: u32) -> i32 {
    match internal_format {
        GL_RGBA32F | GL_RGBA32I | GL_RGBA_INTEGER => 16,
        GL_RGBA8 | GL_BGRA8 | GL_RGBA | GL_BGRA => 4,
        GL_R8 | GL_RED => 1,
        GL_RG8 | GL_RG => 2,
        GL_DEPTH_COMPONENT | GL_DEPTH_COMPONENT16 | GL_DEPTH_COMPONENT24 | GL_DEPTH_COMPONENT32 => 4,
        GL_RGB_RAW_422_APPLE => 2,
        GL_R16 => 2,
        GL_RG16 => 4,
        _ => {
            debug_assert!(false, "internal format: {internal_format:x}");
            0
        }
    }
}

/// `realloc(v, new_len)` for a texel or buffer allocation; false (and `v` unchanged) if the allocation failed. The
/// new bytes come from `alloc_zeroed` (calloc), whose large blocks are fresh mmap pages that stay out of RSS until
/// drawn, as the C++ `realloc`'s untouched tail does; `Vec::resize` would write every new byte. Contents are kept.
pub fn realloc_untouched(v: &mut Vec<u8>, new_len: usize) -> bool {
    if new_len <= v.len() {
        v.truncate(new_len);
        return true;
    }
    let Ok(layout) = std::alloc::Layout::array::<u8>(new_len) else { return false };
    // SAFETY: `new_len > v.len() >= 0`, so the layout is not zero-sized.
    let p = unsafe { std::alloc::alloc_zeroed(layout) };
    if p.is_null() {
        return false;
    }
    // SAFETY: `p` is a fresh, zeroed allocation of `new_len` bytes from the global allocator with `u8`'s layout,
    // which is what `Vec<u8>` owns; the old bytes fit in it.
    let mut grown = unsafe { Vec::from_raw_parts(p, new_len, new_len) };
    grown[..v.len()].copy_from_slice(v);
    *v = grown;
    true
}

/// `aligned_stride` (gl.cc): `rules::aligned_stride` for in-range rows, `int` arithmetic otherwise.
#[inline]
pub fn aligned_stride(row_bytes: i32) -> i32 {
    (row_bytes.wrapping_add(3)) & !3
}

/// `gl_format_to_texture_format` (gl.cc).
pub fn gl_format_to_texture_format(t: u32) -> TextureFormat {
    match t {
        GL_RGBA32F => TextureFormat::RGBA32F,
        GL_RGBA32I => TextureFormat::RGBA32I,
        GL_RGBA8 => TextureFormat::RGBA8,
        GL_R8 => TextureFormat::R8,
        GL_RG8 => TextureFormat::RG8,
        GL_R16 => TextureFormat::R16,
        GL_RG16 => TextureFormat::RG16,
        GL_RGB_RAW_422_APPLE => TextureFormat::YUY2,
        _ => {
            debug_assert!(false, "texture format: {t:x}");
            TextureFormat::RGBA8
        }
    }
}

/// `gl_filter_to_texture_filter` (gl.cc).
pub fn gl_filter_to_texture_filter(t: u32) -> TextureFilter {
    match t {
        GL_NEAREST | GL_NEAREST_MIPMAP_LINEAR | GL_NEAREST_MIPMAP_NEAREST => TextureFilter::NEAREST,
        GL_LINEAR | GL_LINEAR_MIPMAP_LINEAR | GL_LINEAR_MIPMAP_NEAREST => TextureFilter::LINEAR,
        _ => {
            debug_assert!(false, "filter: {t:x}");
            TextureFilter::NEAREST
        }
    }
}

// ---- gl.cc: Texture ----------------------------------------------------------------------------------------------

/// `Texture` (gl.cc). `buf` points either at `owned` (swgl's own allocation, `SHOULD_FREE`) or at an embedder's
/// buffer given to [`Texture::set_buffer`]. Pixel code reaches the bytes through [`Texture::bytes`] /
/// [`Texture::bytes_mut`] or a [`SamplerSource`], never through `buf` alone.
///
/// The buffer fields hold an invariant the draws and `bytes()` rely on ([`Texture::rows_fit`], checked wherever the
/// buffer is set up): `buf` is null and `len` 0, or `len` bytes are readable and writable at `buf`, and every row of
/// `width * bpp` bytes at `y * stride` (`0 <= y < height`) lies inside them. So they are not `pub`: outside the
/// crate they are read through accessors, and the dimensions change only through `set_tex_storage`, which sets the
/// buffer up again.
#[derive(Debug)]
pub struct Texture {
    pub internal_format: u32,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) buf: *mut u8,
    /// Bytes allocated for the texels, without the SIMD padding (C++ `buf_size`); 0 for an external buffer.
    pub(crate) buf_size: usize,
    pub(crate) buf_stride: u32,
    pub(crate) buf_bpp: u8,
    pub min_filter: u32,
    pub mag_filter: u32,
    /// The number of active locks; a locked texture is not reallocated or destroyed.
    pub locked: AtomicI32,
    /// Rendering to the texture behaves as if it were at this offset.
    pub offset: IntPoint,
    pub flags: i32,
    /// Delayed-clear state: rows still to clear, the value, and one bit per row (set once cleared).
    pub delay_clear: i32,
    pub clear_val: u32,
    pub cleared_rows: Option<Vec<u32>>,
    /// The bytes of an internally allocated buffer (texels plus padding); `buf` points at them while it owns them.
    owned: Vec<u8>,
    /// The readable and writable bytes at `buf`: `owned.len()` for an internal buffer, the rows `set_buffer` was
    /// given for an external one. Kept apart from `flags`, which are `pub`, so that no flag changes it.
    len: usize,
}

// SAFETY: `buf` is either `owned`'s heap allocation, which moves with the texture, or an external buffer whose
// owner guarantees it outlives the texture (`set_buffer`'s contract). swgl shares textures between the GL thread
// and compositor threads only under its texture locks (`locked`), which is what makes the C++ sound as well.
unsafe impl Send for Texture {}
// SAFETY: as above; `&Texture` only reads through `buf`.
unsafe impl Sync for Texture {}

impl Default for Texture {
    fn default() -> Self {
        Texture {
            internal_format: 0,
            width: 0,
            height: 0,
            buf: core::ptr::null_mut(),
            buf_size: 0,
            buf_stride: 0,
            buf_bpp: 0,
            min_filter: GL_NEAREST,
            mag_filter: GL_LINEAR,
            locked: AtomicI32::new(0),
            offset: IntPoint::default(),
            flags: Texture::SHOULD_FREE,
            delay_clear: 0,
            clear_val: 0,
            cleared_rows: None,
            owned: Vec::new(),
            len: 0,
        }
    }
}

impl Drop for Texture {
    fn drop(&mut self) {
        self.cleanup();
    }
}

impl Texture {
    /// The buffer is internally allocated by swgl.
    pub const SHOULD_FREE: i32 = 1 << 1;
    /// The buffer has been cleared to initialize it (depth runs are valid).
    pub const CLEARED: i32 = 1 << 2;
    /// Deleted while locked; freed when the last lock goes.
    pub const ZOMBIE: i32 = 1 << 3;

    /// A texture of `internal_format` and `width` x `height` with no buffer yet: [`Texture::allocate`] gives it one
    /// (`SHOULD_FREE`, the default), or [`Texture::set_buffer`] an external one after `set_should_free(false)`.
    pub fn new(internal_format: u32, width: i32, height: i32) -> Texture {
        let mut t = Texture::default();
        (t.internal_format, t.width, t.height) = (internal_format, width, height);
        t
    }

    pub fn width(&self) -> i32 {
        self.width
    }
    pub fn height(&self) -> i32 {
        self.height
    }
    /// The start of the texels (null without a buffer). Reading or writing through it is `bytes()`'s business.
    pub fn buf(&self) -> *mut u8 {
        self.buf
    }
    /// Bytes allocated for the texels, without the SIMD padding (C++ `buf_size`); 0 for an external buffer.
    pub fn buf_size(&self) -> usize {
        self.buf_size
    }

    /// The buffer invariant: `buf` is null and `len` 0, or every row of `width * bpp` bytes at `y * stride`
    /// (`0 <= y < height`) lies inside the `len` bytes at `buf` and rows do not overlap (`stride >= width * bpp`).
    /// Draws index rows by `width`, `height` and `stride` alone (`rules::row_span`), so this is what keeps them inside
    /// the buffer.
    pub fn rows_fit(&self) -> bool {
        if self.buf.is_null() {
            return self.len == 0;
        }
        if self.width <= 0 || self.height <= 0 {
            return true;
        }
        let row = i64::from(self.width) * i64::from(self.buf_bpp);
        let stride = i64::from(self.buf_stride);
        stride >= row
            && (i64::from(self.height) - 1).checked_mul(stride).and_then(|r| r.checked_add(row)).is_some_and(|end| {
                end as u64 <= self.len as u64
            })
    }

    pub fn should_free(&self) -> bool {
        self.flags & Self::SHOULD_FREE != 0
    }
    pub fn cleared(&self) -> bool {
        self.flags & Self::CLEARED != 0
    }
    pub fn zombie(&self) -> bool {
        self.flags & Self::ZOMBIE != 0
    }
    pub fn set_flag(&mut self, flag: i32, val: bool) {
        if val {
            self.flags |= flag;
        } else {
            self.flags &= !flag;
        }
    }
    pub fn set_should_free(&mut self, val: bool) {
        debug_assert!(self.buf.is_null());
        self.set_flag(Self::SHOULD_FREE, val);
    }
    pub fn set_cleared(&mut self, val: bool) {
        self.set_flag(Self::CLEARED, val);
    }
    pub fn set_zombie(&mut self, val: bool) {
        self.set_flag(Self::ZOMBIE, val);
    }

    pub fn enable_delayed_clear(&mut self, val: u32) {
        self.delay_clear = self.height;
        self.clear_val = val;
        let words = ((self.height.max(0) + 31) / 32) as usize;
        // The C++ keeps an existing bit vector even if the height grew since; it is sized to the height here.
        let rows = self.cleared_rows.get_or_insert_with(Vec::new);
        rows.clear();
        rows.resize(words, 0);
        if self.height & 31 != 0 {
            rows[(self.height / 32) as usize] = !0u32 << (self.height & 31);
        }
    }

    pub fn disable_delayed_clear(&mut self) {
        if self.cleared_rows.is_some() {
            self.cleared_rows = None;
            self.delay_clear = 0;
        }
    }

    pub fn bpp(&self) -> i32 {
        self.buf_bpp as i32
    }
    pub fn compute_bpp(&self) -> i32 {
        bytes_for_internal_format(self.internal_format)
    }
    pub fn stride(&self) -> usize {
        self.buf_stride as usize
    }
    pub fn compute_stride(&self, bpp: i32, width: i32) -> usize {
        aligned_stride(bpp.wrapping_mul(width)) as usize
    }

    /// `set_buffer`: an external backing buffer of `new_stride` bytes per row.
    ///
    /// # Safety
    /// `new_buf` must be valid for reads and writes of `(height - 1) * new_stride + width * bpp` bytes (for the
    /// texture's current `width`, `height` and format) for as long as the texture uses it, and nothing else may
    /// write it while a draw or composite reads it.
    pub unsafe fn set_buffer(&mut self, new_buf: *mut u8, new_stride: usize) {
        debug_assert!(!self.should_free());
        let new_bpp = self.compute_bpp();
        debug_assert!(
            new_stride >= (new_bpp * self.width) as usize && new_stride.is_multiple_of(new_bpp.clamp(1, 4) as usize)
        );
        self.buf = new_buf;
        self.buf_size = 0;
        self.buf_bpp = new_bpp as u8;
        self.buf_stride = new_stride as u32;
        self.len = if new_buf.is_null() || self.width <= 0 || self.height <= 0 {
            0
        } else {
            (self.height as usize - 1) * new_stride + (self.width * new_bpp) as usize
        };
        assert!(self.rows_fit(), "external texture buffer: stride {new_stride} under the row's bytes");
    }

    /// `allocate(force, min_width, min_height)`: true if (re)allocation succeeded.
    pub fn allocate(&mut self, force: bool, min_width: i32, min_height: i32) -> bool {
        debug_assert!(self.locked.load(core::sync::atomic::Ordering::Relaxed) == 0);
        self.set_cleared(false);
        if (self.buf.is_null() || force) && self.should_free() {
            let new_bpp = self.compute_bpp();
            let new_stride = self.compute_stride(new_bpp, self.width);
            let max_stride = self.compute_stride(new_bpp, self.width.max(min_width));
            let size = max_stride * self.height.max(min_height).max(0) as usize;
            if (self.buf.is_null() && size > 0) || size > self.buf_size {
                // A SIMD register of padding at the end for the depth buffer's full-register loads and for linear
                // filters on rows of fewer than 2 pixels, as the C++.
                let padding =
                    if self.internal_format == GL_DEPTH_COMPONENT24 || self.width.max(min_width) < 2 { 16 } else { 0 };
                if !realloc_untouched(&mut self.owned, size + padding) {
                    self.cleanup();
                    return false;
                }
                self.buf = self.owned.as_mut_ptr();
                self.len = self.owned.len();
                self.buf_size = size;
            }
            self.buf_bpp = new_bpp as u8;
            self.buf_stride = new_stride as u32;
        }
        assert!(self.rows_fit(), "texture rows outside its buffer");
        true
    }

    pub fn cleanup(&mut self) {
        if !self.buf.is_null() {
            if self.should_free() {
                self.owned = Vec::new();
            }
            self.buf = core::ptr::null_mut();
            self.buf_size = 0;
            self.buf_bpp = 0;
            self.buf_stride = 0;
            self.len = 0;
        }
        self.disable_delayed_clear();
    }

    pub fn bounds(&self) -> IntRect {
        IntRect::new(0, 0, self.width, self.height)
    }
    pub fn offset_bounds(&self) -> IntRect {
        self.bounds() + self.offset
    }
    /// The valid sampling bounds relative to the requested region.
    pub fn sample_bounds(&self, req: &IntRect, invert_y: bool) -> IntRect {
        let mut bb = *self.bounds().intersect(req) - req.origin();
        if invert_y {
            bb.invert_y(req.height());
        }
        bb
    }
    /// `sample_ptr(x, y)` as a byte offset into [`Texture::bytes`].
    pub fn sample_offset(&self, x: i32, y: i32) -> isize {
        y as isize * self.stride() as isize + x as isize * self.bpp() as isize
    }
    /// `sample_ptr(x, y)`. Only an address: reading it is `bytes()`'s business.
    pub fn sample_ptr(&self, x: i32, y: i32) -> *mut u8 {
        self.buf.wrapping_offset(self.sample_offset(x, y))
    }
    /// `end_ptr()`.
    pub fn end_ptr(&self) -> *mut u8 {
        self.buf
            .wrapping_offset((self.height as isize - 1) * self.stride() as isize + (self.width * self.bpp()) as isize)
    }
    /// `sample_ptr(req, bounds, invertY)` as a byte offset.
    pub fn sample_offset_req(&self, req: &IntRect, bounds: &IntRect, invert_y: bool) -> isize {
        let x = req.x0 + bounds.x0;
        let y = if invert_y { req.y1 - 1 - bounds.y0 } else { req.y0 + bounds.y0 };
        self.sample_offset(x, y)
    }
    pub fn sample_ptr_req(&self, req: &IntRect, bounds: &IntRect, invert_y: bool) -> *mut u8 {
        self.buf.wrapping_offset(self.sample_offset_req(req, bounds, invert_y))
    }

    /// The readable length of the buffer in bytes.
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The texels, row `y` at `y * stride()`.
    pub fn bytes(&self) -> &[u8] {
        if self.buf.is_null() {
            return &[];
        }
        // SAFETY: `buf` is `owned`'s allocation of `len()` bytes, or an external buffer of `len()` bytes that
        // `set_buffer`'s caller keeps valid.
        unsafe { core::slice::from_raw_parts(self.buf, self.len()) }
    }
    /// The bytes of row `y` (`width * bpp`), empty for a row outside the buffer.
    pub fn row_bytes_mut(&mut self, y: i32) -> &mut [u8] {
        let n = (self.width.max(0) * self.bpp()) as usize;
        if self.buf.is_null() || y < 0 || y >= self.height || y as usize * self.stride() + n > self.len() {
            return &mut [];
        }
        // SAFETY: the row's `n` bytes are inside the buffer's `len()`, as `bytes_mut`; only this row is borrowed, so
        // stripe views of one texture (`stripe_view`) may hold disjoint rows at once.
        unsafe { core::slice::from_raw_parts_mut(self.sample_ptr(0, y), n) }
    }

    /// A texture over the same texels, for one thread of a parallel draw (raster/DESIGN.md, "Parallel rasterizing"):
    /// it does not own the buffer, and has its own copy of the delayed-clear state, merged back by
    /// [`Texture::merge_stripe_views`]. The view must not outlive `self`'s buffer.
    pub(crate) fn stripe_view(&self) -> Texture {
        Texture {
            internal_format: self.internal_format,
            width: self.width,
            height: self.height,
            buf: self.buf,
            buf_size: 0,
            buf_stride: self.buf_stride,
            buf_bpp: self.buf_bpp,
            min_filter: self.min_filter,
            mag_filter: self.mag_filter,
            locked: AtomicI32::new(0),
            offset: self.offset,
            flags: self.flags & !Self::SHOULD_FREE,
            delay_clear: self.delay_clear,
            clear_val: self.clear_val,
            cleared_rows: self.cleared_rows.clone(),
            owned: Vec::new(),
            len: self.len,
        }
    }

    /// Takes back the delayed clears that the `views` of a parallel draw did: view `t` owns the bit-vector words of
    /// the stripes in `claimed[t]` (a word is the 32 rows of one stripe), and the rows it cleared count down
    /// `delay_clear`.
    pub(crate) fn merge_stripe_views(&mut self, views: &[(&Texture, &[i64])]) {
        let mut cleared = 0;
        for (v, claimed) in views {
            cleared += self.delay_clear - v.delay_clear;
            if let (Some(rows), Some(vrows)) = (self.cleared_rows.as_mut(), v.cleared_rows.as_ref()) {
                for &w in claimed.iter() {
                    if let (Some(r), Some(vr)) = (rows.get_mut(w as usize), vrows.get(w as usize)) {
                        *r = *vr;
                    }
                }
            }
        }
        self.delay_clear -= cleared;
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        if self.buf.is_null() {
            return &mut [];
        }
        let len = self.len();
        // SAFETY: as `bytes`; `&mut self` makes this the only access through the texture.
        unsafe { core::slice::from_raw_parts_mut(self.buf, len) }
    }

    /// What a sampler needs of this texture (`init_sampler`'s inputs); `None` without a buffer.
    pub fn sampler_source(&self) -> Option<SamplerSource> {
        if self.buf.is_null() {
            return None;
        }
        Some(SamplerSource {
            buf: TexelPtr(self.buf as *mut u32),
            len: self.len(),
            width: self.width,
            height: self.height,
            stride: self.buf_stride,
            bpp: self.buf_bpp,
            internal_format: self.internal_format,
            mag_filter: self.mag_filter,
        })
    }
}

/// What a sampler needs of a texture. Only [`Texture::sampler_source`] makes one with a buffer, so `buf` and `len`
/// always describe a real texture's buffer (the samplers built from it read nothing else, `load_texels`).
#[derive(Clone, Copy, Debug, Default)]
pub struct SamplerSource {
    pub(crate) buf: TexelPtr,
    /// Readable bytes at `buf`.
    pub(crate) len: usize,
    pub(crate) width: i32,
    pub(crate) height: i32,
    /// Bytes per row.
    pub(crate) stride: u32,
    pub(crate) bpp: u8,
    pub(crate) internal_format: u32,
    pub(crate) mag_filter: u32,
}

impl SamplerSource {
    /// Whether the texels share a byte with the `len` bytes at `buf`: a draw that samples its own target.
    pub fn overlaps(&self, buf: *const u8, len: usize) -> bool {
        let (a, b) = (self.buf.0 as usize, buf as usize);
        self.len > 0 && len > 0 && a < b.saturating_add(len) && b < a.saturating_add(self.len)
    }
}

/// One texture unit's bindings for the current draw, as `lookup_sampler` sees them: what the textures of
/// `GL_TEXTURE_2D` and `GL_TEXTURE_RECTANGLE` give a sampler, `None` for no texture or no buffer. The GL side fills
/// them at the start of a draw, from the texture objects, and empties them after it; a shader binding its samplers
/// reads these and never the texture objects, so it forms no reference to a texture the draw may be writing.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextureUnitView {
    pub texture_2d: Option<SamplerSource>,
    pub texture_rectangle: Option<SamplerSource>,
}

pub const MAX_TEXTURE_UNITS: usize = 16;

// ---- gl.cc: clears and delayed clears ---------------------------------------------------------------------------

/// A texel type a texture can be cleared with: `uint8_t`, `uint16_t`, `uint32_t`.
pub trait ClearValue: Copy + Pod {
    /// `clear_chunk(value)`: the value repeated over 32 bits.
    fn clear_chunk(self) -> u32;
    fn from_u32(v: u32) -> Self;
}
impl ClearValue for u8 {
    fn clear_chunk(self) -> u32 {
        self as u32 * 0x0101_0101
    }
    fn from_u32(v: u32) -> Self {
        v as u8
    }
}
impl ClearValue for u16 {
    fn clear_chunk(self) -> u32 {
        self as u32 | ((self as u32) << 16)
    }
    fn from_u32(v: u32) -> Self {
        v as u16
    }
}
impl ClearValue for u32 {
    fn clear_chunk(self) -> u32 {
        self
    }
    fn from_u32(v: u32) -> Self {
        v
    }
}

/// `clear_row(buf, len, value, chunk)` on `len` texels of `bytes` from byte `at`. The C++ fills the leading
/// texels up to a 4-byte address boundary one by one and the rest a word at a time; the bytes written are the same.
pub fn clear_row<T: ClearValue>(bytes: &mut [u8], at: usize, len: usize, value: T, _chunk: u32) {
    let n = T::SIZE;
    let Some(row) = bytes.get_mut(at..at + len * n) else {
        debug_assert!(false, "clear_row outside the buffer");
        return;
    };
    for px in row.chunks_exact_mut(n) {
        store_bytes(px, value);
    }
}

/// `clear_buffer<T>(t, value, bb, skip_start, skip_end)`: rows `bb` without columns `[skip_start, skip_end)`.
pub fn clear_buffer<T: ClearValue>(t: &mut Texture, value: T, bb: IntRect, skip_start: i32, skip_end: i32) {
    if t.buf.is_null() {
        return;
    }
    // Rows outside the texture, which the C++ would write past the buffer, are cut.
    let mut bb = bb.intersection(&t.bounds());
    if bb.is_empty() {
        return;
    }
    let skip_start = skip_start.max(bb.x0);
    let skip_end = skip_end.max(skip_start);
    debug_assert!(T::SIZE as i32 == t.bpp());
    let stride = t.stride();
    let n = T::SIZE;
    // Full-width rows of a tightly packed buffer collapse into one long row (the C++'s optimisation; the same bytes).
    if bb.width() == t.width
        && bb.height() > 1
        && skip_start >= skip_end
        && (t.should_free() || stride == t.width as usize * n)
    {
        bb.x1 += (stride / n) as i32 * (bb.height() - 1);
        bb.y1 = bb.y0 + 1;
    }
    let mut at = t.sample_offset(bb.x0, bb.y0) as usize;
    let chunk = value.clear_chunk();
    let bytes = t.bytes_mut();
    for _ in 0..bb.height() {
        if bb.x0 < skip_start {
            clear_row(bytes, at, (skip_start - bb.x0) as usize, value, chunk);
        }
        if skip_end < bb.x1 {
            clear_row(bytes, at + (skip_end - bb.x0) as usize * n, (bb.x1 - skip_end) as usize, value, chunk);
        }
        at += stride;
    }
}

/// `force_clear_row<T>(t, y, skip_start, skip_end)`.
pub fn force_clear_row<T: ClearValue>(t: &mut Texture, y: i32, skip_start: i32, skip_end: i32) {
    debug_assert!(!t.buf.is_null());
    debug_assert!(T::SIZE as i32 == t.bpp());
    debug_assert!(skip_start <= skip_end);
    if y < 0 || y >= t.height {
        return;
    }
    let value = T::from_u32(t.clear_val);
    let chunk = value.clear_chunk();
    let width = t.width;
    // Only row `y` is borrowed: the threads of a parallel draw clear their own rows of one buffer.
    let bytes = t.row_bytes_mut(y);
    if skip_start > 0 {
        clear_row(bytes, 0, skip_start.min(width) as usize, value, chunk);
    }
    if skip_end < width {
        let s = skip_end.max(0);
        clear_row(bytes, s as usize * T::SIZE, (width - s) as usize, value, chunk);
    }
}

/// `force_clear<T>(t, skip)`: clear every row still pending, except the `skip` rectangle.
pub fn force_clear<T: ClearValue>(t: &mut Texture, skip: Option<&IntRect>) {
    if t.delay_clear == 0 || t.cleared_rows.is_none() {
        return;
    }
    let mut y0 = 0;
    let mut y1 = t.height;
    let mut skip_start = 0;
    let mut skip_end = 0;
    if let Some(skip) = skip {
        y0 = skip.y0.clamp_c(0, t.height);
        y1 = skip.y1.clamp_c(y0, t.height);
        skip_start = skip.x0.clamp_c(0, t.width);
        skip_end = skip.x1.clamp_c(skip_start, t.width);
        if skip_start <= 0 && skip_end >= t.width && y0 <= 0 && y1 >= t.height {
            t.disable_delayed_clear();
            return;
        }
    }
    let num_masks = (y1 + 31) / 32;
    let value = T::from_u32(t.clear_val);
    for i in (y0 / 32)..num_masks {
        let rows = t.cleared_rows.as_mut().expect("cleared_rows");
        let Some(word) = rows.get_mut(i as usize) else { break };
        let mut mask = *word;
        if mask != !0u32 {
            *word = !0u32;
            let mut start = i * 32;
            while mask != 0 {
                let count = mask.trailing_zeros() as i32;
                if count > 0 {
                    clear_buffer::<T>(t, value, IntRect::new(0, start, t.width, start + count), skip_start, skip_end);
                    t.delay_clear -= count;
                    start += count;
                    mask >>= count;
                }
                let count = mask.wrapping_add(1).trailing_zeros() as i32;
                start += count;
                // `mask >> 32` is undefined in C++; a fully set mask is shifted out to 0.
                mask = if count >= 32 { 0 } else { mask >> count };
            }
            let count = (i + 1) * 32 - start;
            if count > 0 {
                clear_buffer::<T>(t, value, IntRect::new(0, start, t.width, start + count), skip_start, skip_end);
                t.delay_clear -= count;
            }
        }
    }
    if t.delay_clear <= 0 {
        t.disable_delayed_clear();
    }
}

trait ClampC {
    fn clamp_c(self, lo: Self, hi: Self) -> Self;
}
impl ClampC for i32 {
    /// glsl.h's `clamp(int, int, int)`: `min(max(x, lo), hi)`, which unlike `Ord::clamp` allows `lo > hi`.
    #[inline(always)]
    fn clamp_c(self, lo: i32, hi: i32) -> i32 {
        self.max(lo).min(hi)
    }
}

/// `prepare_texture(t, skip)`: perform the delayed clear of everything outside `skip`.
pub fn prepare_texture(t: &mut Texture, skip: Option<&IntRect>) {
    if t.delay_clear != 0 {
        match t.internal_format {
            GL_RGBA8 => force_clear::<u32>(t, skip),
            GL_R8 => force_clear::<u8>(t, skip),
            GL_RG8 => force_clear::<u16>(t, skip),
            _ => debug_assert!(false, "delayed clear of format {:x}", t.internal_format),
        }
    }
}

/// `request_clear<T>(t, value, scissor)`: clear now inside the scissor, or delay a clear of the whole texture.
pub fn request_clear<T: ClearValue + Into<u32>>(t: &mut Texture, value: T, scissor: &IntRect) {
    if !scissor.contains(&t.offset_bounds()) {
        let skip = *scissor - t.offset;
        force_clear::<T>(t, Some(&skip));
        let bb = skip.intersection(&t.bounds());
        clear_buffer::<T>(t, value, bb, 0, 0);
    } else {
        t.enable_delayed_clear(value.into());
    }
}

// ---- gl.cc: sampler binding -------------------------------------------------------------------------------------

/// A sampler type: `sampler2D`, `isampler2D`, `sampler2DRect` and their format-typed forms.
pub trait Sampler: Copy {
    fn common(&self) -> &samplerCommon;
    fn common_mut(&mut self) -> &mut samplerCommon;
    /// `sampler2DRect` coordinates are in texels, not normalised.
    const RECT: bool = false;
    /// The binding `lookup_sampler` reads from a unit.
    fn unit_source(unit: &TextureUnitView) -> Option<SamplerSource> {
        unit.texture_2d
    }
    /// `init_filter` / `null_filter` for the samplers that have a filter.
    fn set_filter(&mut self, _f: TextureFilter) {}
    fn filter(&self) -> TextureFilter {
        TextureFilter::NEAREST
    }
}

macro_rules! sampler_filtered {
    ($($t:ty $(, $rect:literal)?);*) => {$(
        impl Sampler for $t {
            #[inline(always)]
            fn common(&self) -> &samplerCommon { self }
            #[inline(always)]
            fn common_mut(&mut self) -> &mut samplerCommon { self }
            $(const RECT: bool = $rect;
            fn unit_source(unit: &TextureUnitView) -> Option<SamplerSource> { unit.texture_rectangle })?
            #[inline(always)]
            fn set_filter(&mut self, f: TextureFilter) { self.filter.filter = f; }
            #[inline(always)]
            fn filter(&self) -> TextureFilter { self.filter.filter }
        }
    )*};
}
sampler_filtered!(sampler2D_impl; sampler2DRect_impl, true);

macro_rules! sampler_derived {
    ($($t:ty: filtered $f:literal);*) => {$(
        impl Sampler for $t {
            #[inline(always)]
            fn common(&self) -> &samplerCommon { self }
            #[inline(always)]
            fn common_mut(&mut self) -> &mut samplerCommon { self }
            #[inline(always)]
            fn set_filter(&mut self, f: TextureFilter) { if $f { self.0.set_filter(f) } }
            #[inline(always)]
            fn filter(&self) -> TextureFilter { if $f { self.0.filter() } else { TextureFilter::NEAREST } }
        }
    )*};
}
sampler_derived!(sampler2DR8_impl: filtered true; sampler2DRG8_impl: filtered true; sampler2DRGBA8_impl: filtered true;
    sampler2DRGBA32F_impl: filtered true; isampler2DRGBA32I_impl: filtered false);

impl Sampler for isampler2D_impl {
    #[inline(always)]
    fn common(&self) -> &samplerCommon {
        self
    }
    #[inline(always)]
    fn common_mut(&mut self) -> &mut samplerCommon {
        self
    }
}

/// `init_sampler(s, t)` from a snapshot: size, stride in texel units (dwords from 4 bytes per texel up), buffer,
/// format.
pub fn init_sampler_from<S: Sampler>(s: &mut S, t: &SamplerSource) {
    let c = s.common_mut();
    c.width = t.width as u32;
    c.height = t.height as u32;
    c.stride = t.stride;
    let bpp = t.bpp as i32;
    if bpp >= 4 {
        c.stride /= 4;
    } else if bpp == 2 {
        c.stride /= 2;
    } else {
        debug_assert!(bpp == 1);
    }
    c.buf = t.buf;
    c.len = t.len;
    c.format = gl_format_to_texture_format(t.internal_format);
}

/// `init_sampler(s, t)`: prepares (delay-clears) the texture first, as the C++.
pub fn init_sampler<S: Sampler>(s: &mut S, t: &mut Texture) {
    prepare_texture(t, None);
    match t.sampler_source() {
        Some(src) => init_sampler_from(s, &src),
        None => {
            let c = s.common_mut();
            c.width = t.width as u32;
            c.height = t.height as u32;
            c.stride = 0;
            c.buf = TexelPtr::default();
            c.len = 0;
            c.format = gl_format_to_texture_format(t.internal_format);
        }
    }
}

/// `init_filter(s, t)`: linear filtering needs rows of at least 2 pixels, else nearest.
pub fn init_filter<S: Sampler>(s: &mut S, width: i32, mag_filter: u32) {
    s.set_filter(if width >= 2 { gl_filter_to_texture_filter(mag_filter) } else { TextureFilter::NEAREST });
}

/// The 1x1 transparent black texture of `null_sampler`, a SIMD register long.
static ZERO_BUF: [u32; 4] = [0; 4];

/// `null_sampler(s)`.
pub fn null_sampler<S: Sampler>(s: &mut S) {
    let c = s.common_mut();
    c.width = 1;
    c.height = 1;
    c.stride = 1;
    c.buf = TexelPtr(ZERO_BUF.as_ptr() as *mut u32);
    c.len = core::mem::size_of_val(&ZERO_BUF);
    c.format = TextureFormat::RGBA8;
}

/// `null_filter(s)`.
pub fn null_filter<S: Sampler>(s: &mut S) {
    s.set_filter(TextureFilter::NEAREST);
}

/// The texture of unit `slot` for `S`, or `None` for no texture or no buffer. The GL side prepared (delay-cleared)
/// it when it filled the unit views, as `init_sampler` does in the C++.
fn bound_texture<S: Sampler>(ctx: &RasterState, slot: i32) -> Option<SamplerSource> {
    usize::try_from(slot).ok().and_then(|i| ctx.texture_units.get(i)).and_then(S::unit_source)
}

/// `lookup_sampler(s, texture)`: binds `s` to the texture of unit `slot` (or the null texture) and returns it.
pub fn lookup_sampler<S: Sampler>(ctx: &mut RasterState, s: &mut S, slot: i32) -> S {
    match bound_texture::<S>(ctx, slot) {
        None => {
            null_sampler(s);
            null_filter(s);
        }
        Some(t) => {
            init_sampler_from(s, &t);
            init_filter(s, t.width, t.mag_filter);
        }
    }
    *s
}

/// `lookup_isampler(s, texture)`.
pub fn lookup_isampler<S: Sampler>(ctx: &mut RasterState, s: &mut S, slot: i32) -> S {
    match bound_texture::<S>(ctx, slot) {
        None => null_sampler(s),
        Some(t) => init_sampler_from(s, &t),
    }
    *s
}

// ---- texel loads -------------------------------------------------------------------------------------------------

/// Read a `T` at byte offset `off` of the `len` bytes at `base`. Bytes outside `[0, len)` read as zero, where the C++
/// would read past the texture (see the module doc).
///
/// # Safety
/// `base` must be valid for reading `len` bytes (or `len` must be 0).
#[inline(always)]
pub(crate) unsafe fn load_texels<T: Pod>(base: *const u8, len: usize, off: isize) -> T {
    if off >= 0 && (off as usize) <= len && len - (off as usize) >= T::SIZE {
        // SAFETY: `[off, off + T::SIZE)` lies within the `len` readable bytes at `base` (checked just above).
        unsafe { unaligned_load::<T>(base.offset(off)) }
    } else {
        // SAFETY: forwarded from the caller.
        unsafe { load_texels_partial(base, len, off) }
    }
}

/// # Safety
/// As [`load_texels`].
#[cold]
#[inline(never)]
unsafe fn load_texels_partial<T: Pod>(base: *const u8, len: usize, off: isize) -> T {
    let mut tmp = [0u8; 64];
    debug_assert!(T::SIZE <= tmp.len());
    for (i, b) in tmp.iter_mut().enumerate().take(T::SIZE) {
        let p = off.wrapping_add(i as isize);
        if p >= 0 && (p as usize) < len {
            // SAFETY: `p` is within the `len` readable bytes at `base`.
            *b = unsafe { *base.offset(p) };
        }
    }
    load_bytes(&tmp[..T::SIZE])
}

impl samplerCommon {
    /// `T` at `buf[idx]` with `buf` typed as `E` (`uint32_t*`, `uint16_t*`, `uint8_t*`).
    #[inline(always)]
    pub(crate) fn at<T: Pod, const E: usize>(&self, idx: i32) -> T {
        // SAFETY: a sampler's `buf` holds `len` readable bytes: `init_sampler_from` takes both from a texture's
        // buffer (`Texture::sampler_source`), `null_sampler` and `texelFetchPtr` from static zero buffers, and a
        // default sampler has `len == 0`.
        unsafe { load_texels(self.buf.0 as *const u8, self.len, idx as isize * E as isize) }
    }
    #[inline(always)]
    pub(crate) fn at32<T: Pod>(&self, idx: i32) -> T {
        self.at::<T, 4>(idx)
    }
    #[inline(always)]
    pub(crate) fn at16<T: Pod>(&self, idx: i32) -> T {
        self.at::<T, 2>(idx)
    }
    #[inline(always)]
    pub(crate) fn at8<T: Pod>(&self, idx: i32) -> T {
        self.at::<T, 1>(idx)
    }
    /// `T` at `buf[idx]` without the bounds check.
    ///
    /// # Safety
    /// `idx >= 0` and the `T::SIZE` bytes at byte offset `idx * E` lie within the `len` readable bytes at `buf`.
    #[inline(always)]
    pub(crate) unsafe fn at_unchecked<T: Pod, const E: usize>(&self, idx: i32) -> T {
        // SAFETY: forwarded from the caller.
        unsafe { unaligned_load::<T>((self.buf.0 as *const u8).offset(idx as isize * E as isize)) }
    }
    #[inline(always)]
    fn stride_i(&self) -> i32 {
        self.stride as i32
    }
}

// ---- packing (texture.h) -------------------------------------------------------------------------------------

pub type PackedRGBA8 = V16<u8>;
pub type WideRGBA8 = V16<u16>;
pub type HalfRGBA8 = V8<u16>;
pub type PackedR8 = V4<u8>;
pub type WideR8 = V4<u16>;
pub type PackedRG8 = V8<u8>;
pub type WideRG8 = V8<u16>;
pub type PackedRGBA32F = V16<f32>;
pub type WideRGBA32F = V16<f32>;

/// `unpack(PackedRGBA8)` and the other byte-to-word widenings.
#[inline(always)]
pub fn unpack<const N: usize>(p: Vector<u8, N>) -> Vector<u16, N> {
    p.convert::<u16>()
}

/// `pack(Wide*)`: words to bytes with unsigned saturation of the signed word (`_mm_packus_epi16`, `vqmovun_s16`,
/// and `genericPackWide`, which all give `0` for words of `0x8000` and up and `255` for `256..0x7FFF`).
#[inline(always)]
pub fn pack<const N: usize>(p: Vector<u16, N>) -> Vector<u8, N> {
    let mut r = [0u8; N];
    for i in 0..N {
        r[i] = (p.0[i] as i16).clamp(0, 255) as u8;
    }
    Vector(r)
}

/// `genericPackWide`, named for the portable path.
#[inline(always)]
pub fn genericPackWide<const N: usize>(p: Vector<u16, N>) -> Vector<u8, N> {
    pack(p)
}

/// `clampCoord(I32 coord, int limit, int base = 0)`. SSE2 clamps with `_mm_max_epi16` / `_mm_min_epi16`, that is
/// each 16-bit half of the lanes separately, which differs from a 32-bit clamp outside `±2^15`.
#[inline(always)]
pub fn clampCoord(coord: I32, limit: i32, base: i32) -> I32 {
    #[cfg(target_arch = "x86_64")]
    {
        let half = |v: i32, lo: i32, hi: i32| -> i32 {
            let l = ((v as i16).max(lo as i16)).min(hi as i16) as u16 as u32;
            let h = (((v >> 16) as i16).max((lo >> 16) as i16)).min((hi >> 16) as i16) as u16 as u32;
            (l | (h << 16)) as i32
        };
        let hi = limit.wrapping_sub(1);
        I32::new([
            half(coord.0[0], base, hi),
            half(coord.0[1], base, hi),
            half(coord.0[2], base, hi),
            half(coord.0[3], base, hi),
        ])
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let hi = limit.wrapping_sub(1);
        coord.map_lanes(|v| v.max(base).min(hi))
    }
}

/// `clampCoord(int coord, int limit, int base = 0)`.
#[inline(always)]
pub fn clampCoord_scalar(coord: i32, limit: i32, base: i32) -> i32 {
    coord.max(base).min(limit.wrapping_sub(1))
}

#[cfg(not(target_arch = "x86_64"))]
trait MapLanes {
    fn map_lanes(self, f: impl Fn(i32) -> i32) -> Self;
}
#[cfg(not(target_arch = "x86_64"))]
impl MapLanes for I32 {
    #[inline(always)]
    fn map_lanes(self, f: impl Fn(i32) -> i32) -> Self {
        I32::new([f(self.0[0]), f(self.0[1]), f(self.0[2]), f(self.0[3])])
    }
}

/// `clamp2D(P, sampler)`.
#[inline(always)]
fn clamp2D<S: Sampler>(p: ivec2, s: &S) -> ivec2 {
    let c = s.common();
    ivec2::new(clampCoord(p.x, c.width as i32, 0), clampCoord(p.y, c.height as i32, 0))
}

#[inline(always)]
fn clamp2D_scalar<S: Sampler>(p: ivec2_scalar, s: &S) -> ivec2_scalar {
    let c = s.common();
    ivec2_scalar::new(clampCoord_scalar(p.x, c.width as i32, 0), clampCoord_scalar(p.y, c.height as i32, 0))
}

#[inline(always)]
pub fn to_float(x: u32) -> f32 {
    x as f32 * (1.0f32 / 255.0f32)
}

#[inline(always)]
fn u32_lanes(a: u32, b: u32, c: u32, d: u32) -> U32 {
    U32::new([a, b, c, d])
}

/// `pixel_to_vec4(a, b, c, d)`: BGRA8 texels to RGBA lanes.
#[inline(always)]
pub fn pixel_to_vec4(a: u32, b: u32, c: u32, d: u32) -> vec4 {
    let pixels = u32_lanes(a, b, c, d);
    vec4::new(cast((pixels >> 16) & 0xFF), cast((pixels >> 8) & 0xFF), cast(pixels & 0xFF), cast(pixels >> 24))
        * (1.0f32 / 255.0f32)
}

/// `pixel_float_to_vec4(a, b, c, d)`: four RGBA32F texels transposed.
#[inline(always)]
pub fn pixel_float_to_vec4(a: Float, b: Float, c: Float, d: Float) -> vec4 {
    vec4::new(
        Float::new([a.0[0], b.0[0], c.0[0], d.0[0]]),
        Float::new([a.0[1], b.0[1], c.0[1], d.0[1]]),
        Float::new([a.0[2], b.0[2], c.0[2], d.0[2]]),
        Float::new([a.0[3], b.0[3], c.0[3], d.0[3]]),
    )
}

#[inline(always)]
pub fn pixel_int_to_ivec4(a: I32, b: I32, c: I32, d: I32) -> ivec4 {
    ivec4::new(
        I32::new([a.0[0], b.0[0], c.0[0], d.0[0]]),
        I32::new([a.0[1], b.0[1], c.0[1], d.0[1]]),
        I32::new([a.0[2], b.0[2], c.0[2], d.0[2]]),
        I32::new([a.0[3], b.0[3], c.0[3], d.0[3]]),
    )
}

/// `pixel_to_vec4(uint32_t p)`.
#[inline(always)]
pub fn pixel_to_vec4_scalar(p: u32) -> vec4_scalar {
    let i = u32_lanes((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF, p >> 24);
    let f = cast(i) * (1.0f32 / 255.0f32);
    vec4_scalar::new(f.0[0], f.0[1], f.0[2], f.0[3])
}

// ---- texel fetch ----------------------------------------------------------------------------------------------

#[inline(always)]
fn fetchOffsetsRGBA8(c: &samplerCommon, offset: I32) -> vec4 {
    pixel_to_vec4(c.at32(offset.0[0]), c.at32(offset.0[1]), c.at32(offset.0[2]), c.at32(offset.0[3]))
}

#[inline(always)]
fn row_offsets(c: &samplerCommon, p: ivec2) -> I32 {
    p.x + p.y * c.stride_i()
}

pub fn texelFetchRGBA8<S: Sampler>(s: &S, p: ivec2) -> vec4 {
    let c = s.common();
    fetchOffsetsRGBA8(c, row_offsets(c, p))
}

#[inline(always)]
fn fetchOffsetsR8(c: &samplerCommon, offset: I32) -> Float {
    let i = u32_lanes(
        c.at8::<u8>(offset.0[0]) as u32,
        c.at8::<u8>(offset.0[1]) as u32,
        c.at8::<u8>(offset.0[2]) as u32,
        c.at8::<u8>(offset.0[3]) as u32,
    );
    cast(i) * (1.0f32 / 255.0f32)
}

pub fn texelFetchR8<S: Sampler>(s: &S, p: ivec2) -> vec4 {
    let c = s.common();
    vec4::new(fetchOffsetsR8(c, row_offsets(c, p)), Float::splat(0.0), Float::splat(0.0), Float::splat(1.0))
}

#[inline(always)]
fn fetchOffsetsRG8(c: &samplerCommon, offset: I32) -> vec4 {
    let pixels = U16::new([c.at16(offset.0[0]), c.at16(offset.0[1]), c.at16(offset.0[2]), c.at16(offset.0[3])]);
    let r = (pixels & 0xFF).convert::<f32>() * (1.0f32 / 255.0f32);
    let g = (pixels >> 8).convert::<f32>() * (1.0f32 / 255.0f32);
    vec4::new(r, g, Float::splat(0.0), Float::splat(1.0))
}

pub fn texelFetchRG8<S: Sampler>(s: &S, p: ivec2) -> vec4 {
    let c = s.common();
    fetchOffsetsRG8(c, row_offsets(c, p))
}

#[inline(always)]
fn fetchOffsetsR16(c: &samplerCommon, offset: I32) -> Float {
    let i = u32_lanes(
        c.at16::<u16>(offset.0[0]) as u32,
        c.at16::<u16>(offset.0[1]) as u32,
        c.at16::<u16>(offset.0[2]) as u32,
        c.at16::<u16>(offset.0[3]) as u32,
    );
    cast(i) * (1.0f32 / 65535.0f32)
}

pub fn texelFetchR16<S: Sampler>(s: &S, p: ivec2) -> vec4 {
    let c = s.common();
    vec4::new(fetchOffsetsR16(c, row_offsets(c, p)), Float::splat(0.0), Float::splat(0.0), Float::splat(1.0))
}

#[inline(always)]
fn fetchOffsetsRG16(c: &samplerCommon, offset: I32) -> vec4 {
    let pixels = u32_lanes(c.at32(offset.0[0]), c.at32(offset.0[1]), c.at32(offset.0[2]), c.at32(offset.0[3]));
    let r = cast(pixels & 0xFFFF) * (1.0f32 / 65535.0f32);
    let g = cast(pixels >> 16) * (1.0f32 / 65535.0f32);
    vec4::new(r, g, Float::splat(0.0), Float::splat(1.0))
}

pub fn texelFetchRG16<S: Sampler>(s: &S, p: ivec2) -> vec4 {
    let c = s.common();
    fetchOffsetsRG16(c, row_offsets(c, p))
}

/// `fetchOffsetsFloat(buf, offset)`, `buf` given as a dword index `base` into the sampler's buffer.
#[inline(always)]
fn fetchOffsetsFloat(c: &samplerCommon, base: i32, offset: I32) -> vec4 {
    pixel_float_to_vec4(
        c.at32(base.wrapping_add(offset.0[0])),
        c.at32(base.wrapping_add(offset.0[1])),
        c.at32(base.wrapping_add(offset.0[2])),
        c.at32(base.wrapping_add(offset.0[3])),
    )
}

pub fn texelFetchFloat<S: Sampler>(s: &S, p: ivec2) -> vec4 {
    let c = s.common();
    let offset = p.x * 4 + p.y * c.stride_i();
    fetchOffsetsFloat(c, 0, offset)
}

#[inline(always)]
fn fetchOffsetsYUY2(c: &samplerCommon, offset: I32) -> vec4 {
    // 2-pixel chunks of 4 bytes, G0 B G1 R; the offset is aligned to the chunk and the selector picks the pixel.
    let selector = offset & 1;
    let offset = offset & !1;
    let pixels = u32_lanes(c.at16(offset.0[0]), c.at16(offset.0[1]), c.at16(offset.0[2]), c.at16(offset.0[3]));
    let b = ((pixels >> 8) & 0xFF).convert::<f32>() * (1.0f32 / 255.0f32);
    let r = (pixels >> 24).convert::<f32>() * (1.0f32 / 255.0f32);
    let g = (if_then_else(-selector, pixels >> 16, pixels) & 0xFF).convert::<f32>() * (1.0f32 / 255.0f32);
    vec4::new(r, g, b, Float::splat(1.0))
}

pub fn texelFetchYUY2<S: Sampler>(s: &S, p: ivec2) -> vec4 {
    let c = s.common();
    fetchOffsetsYUY2(c, row_offsets(c, p))
}

#[inline(always)]
fn fetchOffsetsInt(c: &samplerCommon, base: i32, offset: I32) -> ivec4 {
    pixel_int_to_ivec4(
        c.at32(base.wrapping_add(offset.0[0])),
        c.at32(base.wrapping_add(offset.0[1])),
        c.at32(base.wrapping_add(offset.0[2])),
        c.at32(base.wrapping_add(offset.0[3])),
    )
}

/// The `texelFetch` overloads: `(sampler, ivec2 | ivec2_scalar, lod)`. `sampler2DRect` takes no lod in the C++;
/// here its `lod` is ignored.
pub trait TexelFetch<P> {
    type Output;
    fn texel_fetch(&self, p: P, lod: i32) -> Self::Output;
}

#[inline(always)]
pub fn texelFetch<S: TexelFetch<P>, P>(s: S, p: P, lod: i32) -> S::Output {
    s.texel_fetch(p, lod)
}

fn texel_fetch_any<S: Sampler>(s: &S, p: ivec2, rect: bool) -> vec4 {
    let p = clamp2D(p, s);
    match s.common().format {
        TextureFormat::RGBA32F if !rect => texelFetchFloat(s, p),
        TextureFormat::RGBA8 => texelFetchRGBA8(s, p),
        TextureFormat::R8 => texelFetchR8(s, p),
        TextureFormat::RG8 => texelFetchRG8(s, p),
        TextureFormat::R16 => texelFetchR16(s, p),
        TextureFormat::RG16 => texelFetchRG16(s, p),
        TextureFormat::YUY2 => texelFetchYUY2(s, p),
        _ => {
            debug_assert!(false);
            vec4::default()
        }
    }
}

impl TexelFetch<ivec2> for sampler2D_impl {
    type Output = vec4;
    fn texel_fetch(&self, p: ivec2, lod: i32) -> vec4 {
        debug_assert!(lod == 0);
        texel_fetch_any(self, p, false)
    }
}

impl TexelFetch<ivec2> for sampler2DRect_impl {
    type Output = vec4;
    fn texel_fetch(&self, p: ivec2, _lod: i32) -> vec4 {
        texel_fetch_any(self, p, true)
    }
}

macro_rules! typed_fetch {
    ($($t:ty => $f:ident, $fmt:ident);*) => {$(
        impl TexelFetch<ivec2> for $t {
            type Output = vec4;
            fn texel_fetch(&self, p: ivec2, lod: i32) -> vec4 {
                debug_assert!(lod == 0);
                let p = clamp2D(p, self);
                debug_assert!(self.common().format == TextureFormat::$fmt);
                $f(self, p)
            }
        }
    )*};
}
typed_fetch!(sampler2DRGBA32F_impl => texelFetchFloat, RGBA32F; sampler2DRGBA8_impl => texelFetchRGBA8, RGBA8;
    sampler2DR8_impl => texelFetchR8, R8; sampler2DRG8_impl => texelFetchRG8, RG8);

#[inline(always)]
fn scalar_index(c: &samplerCommon, x: i32, y: i32) -> i32 {
    // `P.x + P.y * sampler->stride` is unsigned arithmetic in C++; the bits are those of the wrapping `int` sum.
    (x as u32).wrapping_add((y as u32).wrapping_mul(c.stride)) as i32
}

#[inline(always)]
fn fetch_vec4_scalar_f32(c: &samplerCommon, idx: i32) -> vec4_scalar {
    let v: Float = c.at32(idx);
    vec4_scalar::new(v.0[0], v.0[1], v.0[2], v.0[3])
}

impl TexelFetch<ivec2_scalar> for sampler2D_impl {
    type Output = vec4_scalar;
    fn texel_fetch(&self, p: ivec2_scalar, lod: i32) -> vec4_scalar {
        debug_assert!(lod == 0);
        let p = clamp2D_scalar(p, self);
        let c = self.common();
        if c.format == TextureFormat::RGBA32F {
            fetch_vec4_scalar_f32(c, scalar_index(c, p.x.wrapping_mul(4), p.y))
        } else {
            debug_assert!(c.format == TextureFormat::RGBA8);
            pixel_to_vec4_scalar(c.at32(scalar_index(c, p.x, p.y)))
        }
    }
}

impl TexelFetch<ivec2_scalar> for sampler2DRGBA32F_impl {
    type Output = vec4_scalar;
    fn texel_fetch(&self, p: ivec2_scalar, lod: i32) -> vec4_scalar {
        debug_assert!(lod == 0);
        let p = clamp2D_scalar(p, self);
        let c = self.common();
        fetch_vec4_scalar_f32(c, scalar_index(c, p.x.wrapping_mul(4), p.y))
    }
}

impl TexelFetch<ivec2_scalar> for sampler2DRGBA8_impl {
    type Output = vec4_scalar;
    fn texel_fetch(&self, p: ivec2_scalar, lod: i32) -> vec4_scalar {
        debug_assert!(lod == 0);
        let p = clamp2D_scalar(p, self);
        let c = self.common();
        pixel_to_vec4_scalar(c.at32(scalar_index(c, p.x, p.y)))
    }
}

impl TexelFetch<ivec2_scalar> for sampler2DR8_impl {
    type Output = vec4_scalar;
    fn texel_fetch(&self, p: ivec2_scalar, lod: i32) -> vec4_scalar {
        debug_assert!(lod == 0);
        let p = clamp2D_scalar(p, self);
        let c = self.common();
        vec4_scalar::new(to_float(c.at8::<u8>(scalar_index(c, p.x, p.y)) as u32), 0.0, 0.0, 1.0)
    }
}

impl TexelFetch<ivec2_scalar> for sampler2DRG8_impl {
    type Output = vec4_scalar;
    fn texel_fetch(&self, p: ivec2_scalar, lod: i32) -> vec4_scalar {
        debug_assert!(lod == 0);
        let p = clamp2D_scalar(p, self);
        let c = self.common();
        let pixel: u16 = c.at16(scalar_index(c, p.x, p.y));
        vec4_scalar::new(to_float((pixel & 0xFF) as u32), to_float((pixel >> 8) as u32), 0.0, 1.0)
    }
}

impl TexelFetch<ivec2> for isampler2D_impl {
    type Output = ivec4;
    fn texel_fetch(&self, p: ivec2, lod: i32) -> ivec4 {
        debug_assert!(lod == 0);
        let p = clamp2D(p, self);
        let c = self.common();
        debug_assert!(c.format == TextureFormat::RGBA32I);
        let offset = p.x * 4 + p.y * c.stride_i();
        fetchOffsetsInt(c, 0, offset)
    }
}

impl TexelFetch<ivec2_scalar> for isampler2D_impl {
    type Output = ivec4_scalar;
    fn texel_fetch(&self, p: ivec2_scalar, lod: i32) -> ivec4_scalar {
        debug_assert!(lod == 0);
        let p = clamp2D_scalar(p, self);
        let c = self.common();
        debug_assert!(c.format == TextureFormat::RGBA32I);
        let v: I32 = c.at32(scalar_index(c, p.x.wrapping_mul(4), p.y));
        ivec4_scalar::new(v.0[0], v.0[1], v.0[2], v.0[3])
    }
}

pub const MAX_TEXEL_OFFSET: i32 = 8;

/// `zeroFetchBuf`: one row of zero texels for `texelFetchOffset` outside the texture (stride 0).
static ZERO_FETCH_BUF: [u32; (MAX_TEXEL_OFFSET * 4) as usize] = [0; (MAX_TEXEL_OFFSET * 4) as usize];

/// `FetchScalar`: the start of a block of texels and the stride to step rows by.
#[derive(Clone, Copy, Debug)]
pub struct FetchScalar {
    buf: TexelPtr,
    len: usize,
    /// Dword index of the block's first texel.
    base: i32,
    pub stride: u32,
}

/// `FetchVector`: the buffer, the per-lane dword offsets of the block and the row stride.
#[derive(Clone, Copy, Debug)]
pub struct FetchVector {
    buf: TexelPtr,
    len: usize,
    pub offset: I32,
    pub stride: u32,
}

fn zero_fetch() -> (TexelPtr, usize) {
    (TexelPtr(ZERO_FETCH_BUF.as_ptr() as *mut u32), core::mem::size_of_val(&ZERO_FETCH_BUF))
}

/// The `texelFetchPtr(sampler, P, min_x, max_x, min_y, max_y)` overloads.
pub trait TexelFetchPtr<P> {
    type Output;
    fn texel_fetch_ptr(&self, p: P, min_x: i32, max_x: i32, min_y: i32, max_y: i32) -> Self::Output;
}

#[inline(always)]
pub fn texelFetchPtr<S: TexelFetchPtr<P>, P>(s: S, p: P, min_x: i32, max_x: i32, min_y: i32, max_y: i32) -> S::Output {
    s.texel_fetch_ptr(p, min_x, max_x, min_y, max_y)
}

impl<S: Sampler> TexelFetchPtr<ivec2_scalar> for S {
    type Output = FetchScalar;
    #[inline(always)]
    fn texel_fetch_ptr(&self, p: ivec2_scalar, min_x: i32, max_x: i32, min_y: i32, max_y: i32) -> FetchScalar {
        debug_assert!(max_x < MAX_TEXEL_OFFSET);
        let c = self.common();
        if p.x < -min_x || p.x >= c.width as i32 - max_x || p.y < -min_y || p.y >= c.height as i32 - max_y {
            let (buf, len) = zero_fetch();
            return FetchScalar { buf, len, base: 0, stride: 0 };
        }
        FetchScalar { buf: c.buf, len: c.len, base: scalar_index(c, p.x.wrapping_mul(4), p.y), stride: c.stride }
    }
}

impl<S: Sampler> TexelFetchPtr<ivec2> for S {
    type Output = FetchVector;
    #[inline(always)]
    fn texel_fetch_ptr(&self, p: ivec2, min_x: i32, max_x: i32, min_y: i32, max_y: i32) -> FetchVector {
        debug_assert!(max_x < MAX_TEXEL_OFFSET);
        let c = self.common();
        let outside = p.x.cmp_lt(-min_x)
            | p.x.cmp_ge(c.width as i32 - max_x)
            | p.y.cmp_lt(-min_y)
            | p.y.cmp_ge(c.height as i32 - max_y);
        if test_any(outside) {
            let (buf, len) = zero_fetch();
            return FetchVector { buf, len, offset: I32::splat(0), stride: 0 };
        }
        FetchVector { buf: c.buf, len: c.len, offset: p.x * 4 + p.y * c.stride_i(), stride: c.stride }
    }
}

/// The `texelFetchUnchecked(sampler, ptr, x, y)` overloads.
pub trait TexelFetchUnchecked<F> {
    type Output;
    fn texel_fetch_unchecked(&self, ptr: F, x: i32, y: i32) -> Self::Output;
}

#[inline(always)]
pub fn texelFetchUnchecked<S: TexelFetchUnchecked<F>, F>(s: S, ptr: F, x: i32, y: i32) -> S::Output {
    s.texel_fetch_unchecked(ptr, x, y)
}

#[inline(always)]
fn fetch_common(buf: TexelPtr, len: usize) -> samplerCommon {
    samplerCommon { buf, len, ..Default::default() }
}

/// `x * 4 + y * stride` as a dword index. The C++ computes it unsigned and indexes with the 32-bit result, so a
/// negative offset (which no WebRender shader uses) reads 4 GiB away there; here it reads before the block.
#[inline(always)]
fn unchecked_base(x: i32, y: i32, stride: u32) -> i32 {
    (x.wrapping_mul(4) as u32).wrapping_add((y as u32).wrapping_mul(stride)) as i32
}

impl TexelFetchUnchecked<FetchScalar> for sampler2D_impl {
    type Output = vec4_scalar;
    #[inline(always)]
    fn texel_fetch_unchecked(&self, ptr: FetchScalar, x: i32, y: i32) -> vec4_scalar {
        debug_assert!(self.format == TextureFormat::RGBA32F);
        let c = fetch_common(ptr.buf, ptr.len);
        fetch_vec4_scalar_f32(&c, ptr.base.wrapping_add(unchecked_base(x, y, ptr.stride)))
    }
}

impl TexelFetchUnchecked<FetchScalar> for isampler2D_impl {
    type Output = ivec4_scalar;
    #[inline(always)]
    fn texel_fetch_unchecked(&self, ptr: FetchScalar, x: i32, y: i32) -> ivec4_scalar {
        debug_assert!(self.format == TextureFormat::RGBA32I);
        let c = fetch_common(ptr.buf, ptr.len);
        let v: I32 = c.at32(ptr.base.wrapping_add(unchecked_base(x, y, ptr.stride)));
        ivec4_scalar::new(v.0[0], v.0[1], v.0[2], v.0[3])
    }
}

impl TexelFetchUnchecked<FetchVector> for sampler2D_impl {
    type Output = vec4;
    #[inline(always)]
    fn texel_fetch_unchecked(&self, ptr: FetchVector, x: i32, y: i32) -> vec4 {
        debug_assert!(self.format == TextureFormat::RGBA32F);
        let c = fetch_common(ptr.buf, ptr.len);
        fetchOffsetsFloat(&c, unchecked_base(x, y, ptr.stride), ptr.offset)
    }
}

impl TexelFetchUnchecked<FetchVector> for isampler2D_impl {
    type Output = ivec4;
    #[inline(always)]
    fn texel_fetch_unchecked(&self, ptr: FetchVector, x: i32, y: i32) -> ivec4 {
        debug_assert!(self.format == TextureFormat::RGBA32I);
        let c = fetch_common(ptr.buf, ptr.len);
        fetchOffsetsInt(&c, unchecked_base(x, y, ptr.stride), ptr.offset)
    }
}

// ---- linear filtering ---------------------------------------------------------------------------------------

/// `linearQuantize(P, scale)`: scale for quantization, subtract the filter offset, as `P * scale + (0.5 - 0.5 *
/// scale)`.
#[inline(always)]
pub fn linearQuantize(p: vec2, scale: f32) -> vec2 {
    let k = 0.5f32 - 0.5f32 * scale;
    vec2::new(p.x * scale + k, p.y * scale + k)
}

#[inline(always)]
pub fn linearQuantize_scalar(p: vec2_scalar, scale: f32) -> vec2_scalar {
    let k = 0.5f32 - 0.5f32 * scale;
    vec2_scalar::new(p.x * scale + k, p.y * scale + k)
}

/// `samplerScale(sampler, P)`: normalised coordinates to texels, except for `sampler2DRect`.
#[inline(always)]
pub fn samplerScale<S: Sampler>(s: &S, p: vec2) -> vec2 {
    if S::RECT {
        return p;
    }
    let c = s.common();
    vec2::new(p.x * (c.width as f32), p.y * (c.height as f32))
}

/// `linearQuantize(P, scale, sampler)`: `samplerScale` first.
/// `samplerScale(sampler, P)` on a scalar coordinate.
#[inline(always)]
pub fn samplerScale_scalar<S: Sampler>(s: &S, p: vec2_scalar) -> vec2_scalar {
    if S::RECT {
        return p;
    }
    let c = s.common();
    vec2_scalar::new(p.x * (c.width as f32), p.y * (c.height as f32))
}

/// `linearQuantize(P, scale, sampler)` on a scalar coordinate.
#[inline(always)]
pub fn linearQuantize_scalar_s<S: Sampler>(p: vec2_scalar, scale: f32, s: &S) -> vec2_scalar {
    linearQuantize_scalar(samplerScale_scalar(s, p), scale)
}

#[inline(always)]
pub fn linearQuantize_s<S: Sampler>(p: vec2, scale: f32, s: &S) -> vec2 {
    linearQuantize(samplerScale(s, p), scale)
}

/// `computeRow(sampler, i, margin)`: the clamped offset of the first row's tap.
#[inline(always)]
pub fn computeRow(c: &samplerCommon, i: ivec2, margin: u32) -> I32 {
    clampCoord(i.x, c.width.wrapping_sub(margin) as i32, 0) + clampCoord(i.y, c.height as i32, 0) * c.stride_i()
}

#[inline(always)]
pub fn computeRow_scalar(c: &samplerCommon, i: ivec2_scalar, margin: u32) -> i32 {
    (clampCoord_scalar(i.x, c.width.wrapping_sub(margin) as i32, 0) as u32)
        .wrapping_add((clampCoord_scalar(i.y, c.height as i32, 0) as u32).wrapping_mul(c.stride)) as i32
}

/// `computeNextRowOffset(sampler, i)`: the stride, or 0 on the last row and outside.
#[inline(always)]
pub fn computeNextRowOffset(c: &samplerCommon, i: ivec2) -> I32 {
    if_then_else(i.y.cmp_ge(0) & i.y.cmp_lt(c.height as i32 - 1), I32::splat(c.stride_i()), I32::splat(0))
}

/// `computeFracX(sampler, i, frac)`: the X fraction in 2^7, 128 past the last pixel.
#[inline(always)]
pub fn computeFracX(c: &samplerCommon, i: ivec2, frac: ivec2) -> I16 {
    let overread = i.x.cmp_gt(c.width as i32 - 2);
    ((((frac.x & i.x.cmp_ge(0)) | overread) & 0x7F) - overread).convert::<i16>()
}

#[inline(always)]
pub fn computeFracNoClamp(frac: I32) -> I16 {
    (frac & 0x7F).convert::<i16>()
}

#[inline(always)]
pub fn computeFracY(frac: ivec2) -> I16 {
    computeFracNoClamp(frac.y)
}

#[inline(always)]
fn shr7(i: ivec2) -> ivec2 {
    ivec2::new(i.x >> 7, i.y >> 7)
}

#[inline(always)]
fn i16x8(v: V8<u8>) -> V8<i16> {
    v.convert::<i16>()
}

#[inline(always)]
fn lerp7(a: V8<i16>, b: V8<i16>, f: impl Into<V8<i16>>) -> V8<i16> {
    a + (((b - a) * f.into()) >> 7)
}

#[inline(always)]
fn sw<T: Lane, const N: usize, const M: usize>(v: Vector<T, N>, idx: [usize; M]) -> Vector<T, M> {
    v.swizzle(idx)
}

#[inline(always)]
fn bc<D: Pod, S: Pod>(s: S) -> D {
    bit_cast::<D, S>(s)
}

/// The filtering of [`textureLinearUnpackedR8`] in SSE2, or `None` when a tap is not inside the sampler's bytes
/// (the portable path clips those). LLVM left the generic code scalar (one `imul` per 16-bit lane, every tap
/// bounds-checked); swgl's is vector code. The arithmetic is the same lane by lane, so the bytes are: the taps are
/// kept as left texels `[a b c d]` and right texels in the two halves of one register instead of interleaved, which
/// is the `SHUFFLE(0, 2, 4, 6, 1, 3, 5, 7)` done before the row lerp instead of after it.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn linear_r8_sse2(c: &samplerCommon, row0: I32, row1: I32, fracx: I16, fracy: I16) -> Option<U16> {
    use core::arch::x86_64::*;
    let base = c.buf.0 as *const u8;
    let fits = |r: I32| r.0.iter().all(|&o| (o as u32 as usize).saturating_add(2) <= c.len);
    if base.is_null() || !fits(row0) || !fits(row1) {
        return None;
    }
    // SAFETY: SSE2 is part of x86_64; every tap's two bytes are inside the `len` readable bytes at `base` (checked
    // just above).
    unsafe {
        let taps = |r: I32| {
            let t = |k: usize| core::ptr::read_unaligned(base.add(r.0[k] as usize) as *const i16);
            let v = _mm_set_epi16(0, 0, 0, 0, t(3), t(2), t(1), t(0));
            // Left texels (low bytes) in lanes 0-3, right texels (high bytes) in lanes 4-7.
            _mm_unpacklo_epi64(_mm_and_si128(v, _mm_set1_epi16(0xFF)), _mm_srli_epi16(v, 8))
        };
        let (v0, v1) = (taps(row0), taps(row1));
        let fy = _mm_loadl_epi64(fracy.0.as_ptr() as *const __m128i);
        let fy = _mm_unpacklo_epi64(fy, fy);
        let v = _mm_add_epi16(v0, _mm_srai_epi16(_mm_mullo_epi16(_mm_sub_epi16(v1, v0), fy), 7));
        let (l, h) = (v, _mm_unpackhi_epi64(v, v));
        let fx = _mm_loadl_epi64(fracx.0.as_ptr() as *const __m128i);
        let r = _mm_add_epi16(l, _mm_srai_epi16(_mm_mullo_epi16(_mm_sub_epi16(h, l), fx), 7));
        let mut out = U16::default();
        _mm_storel_epi64(out.0.as_mut_ptr() as *mut __m128i, r);
        Some(out)
    }
}

/// [`linear_r8_sse2`] in NEON.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn linear_r8_neon(c: &samplerCommon, row0: I32, row1: I32, fracx: I16, fracy: I16) -> Option<U16> {
    use core::arch::aarch64::*;
    let base = c.buf.0 as *const u8;
    let fits = |r: I32| r.0.iter().all(|&o| (o as u32 as usize).saturating_add(2) <= c.len);
    if base.is_null() || !fits(row0) || !fits(row1) {
        return None;
    }
    // SAFETY: NEON is part of aarch64; every tap's two bytes are inside the `len` readable bytes at `base` (checked
    // just above); the lane arrays are 2-byte aligned for `vld1`/`vst1`.
    unsafe {
        let taps = |r: I32| {
            let t: [u16; 4] = core::array::from_fn(|k| core::ptr::read_unaligned(base.add(r.0[k] as usize) as *const u16));
            let v = vld1_u16(t.as_ptr());
            // Left texels (low bytes) in lanes 0-3, right texels (high bytes) in lanes 4-7.
            vreinterpretq_s16_u16(vcombine_u16(vand_u16(v, vdup_n_u16(0xFF)), vshr_n_u16::<8>(v)))
        };
        let (v0, v1) = (taps(row0), taps(row1));
        let fy = vld1_s16(fracy.0.as_ptr());
        let fy = vcombine_s16(fy, fy);
        let v = vaddq_s16(v0, vshrq_n_s16::<7>(vmulq_s16(vsubq_s16(v1, v0), fy)));
        let (l, h) = (vget_low_s16(v), vget_high_s16(v));
        let r = vadd_s16(l, vshr_n_s16::<7>(vmul_s16(vsub_s16(h, l), vld1_s16(fracx.0.as_ptr()))));
        let mut out = U16::default();
        vst1_s16(out.0.as_mut_ptr() as *mut i16, r);
        Some(out)
    }
}

/// `WidePlanarRGBA8`.
#[derive(Clone, Copy, Debug)]
pub struct WidePlanarRGBA8 {
    pub rg: V8<u16>,
    pub ba: V8<u16>,
}

pub fn textureLinearPlanarRGBA8<S: Sampler>(s: &S, i: ivec2) -> WidePlanarRGBA8 {
    let c = s.common();
    debug_assert!(c.format == TextureFormat::RGBA8);
    let frac = i;
    let i = shr7(i);
    let row0 = computeRow(c, i, 1);
    let row1 = row0 + computeNextRowOffset(c, i);
    let fracx = computeFracX(c, i, frac);
    let fracy = computeFracY(frac);

    let tap = |r: i32| i16x8(c.at32::<V8<u8>>(r));
    let a0 = lerp7(tap(row0.0[0]), tap(row1.0[0]), fracy.0[0]);
    let b0 = lerp7(tap(row0.0[1]), tap(row1.0[1]), fracy.0[1]);
    let abl = zip_low(a0, b0);
    let abh = zip_high(a0, b0);
    let abl = lerp7(abl, abh, sw(fracx, [0, 1, 0, 1, 0, 1, 0, 1]));

    let c0 = lerp7(tap(row0.0[2]), tap(row1.0[2]), fracy.0[2]);
    let d0 = lerp7(tap(row0.0[3]), tap(row1.0[3]), fracy.0[3]);
    let cdl = zip_low(c0, d0);
    let cdh = zip_high(c0, d0);
    let cdl = lerp7(cdl, cdh, sw(fracx, [2, 3, 2, 3, 2, 3, 2, 3]));

    WidePlanarRGBA8 { rg: bc(zip2_low(abl, cdl)), ba: bc(zip2_high(abl, cdl)) }
}

pub fn textureLinearRGBA8<S: Sampler>(s: &S, p: vec2) -> vec4 {
    let i = ivec2::from_vec2(linearQuantize_s(p, 128.0, s));
    let planar = textureLinearPlanarRGBA8(s, i);
    let rg = planar.rg.convert::<f32>();
    let ba = planar.ba.convert::<f32>();
    let r: Float = low_half(rg);
    let g: Float = high_half(rg);
    let b: Float = low_half(ba);
    let a: Float = high_half(ba);
    vec4::new(b, g, r, a) * (1.0f32 / 255.0f32)
}

/// `textureLinearUnpackedR8`: inlined into its callers (swgl's clang inlines it into `textureLinearR8`, which
/// `texture()` inlines into the fragment shader), so the R8 bilinear sample costs one call less per chunk.
#[inline(always)]
pub fn textureLinearUnpackedR8<S: Sampler>(s: &S, i: ivec2) -> U16 {
    let c = s.common();
    debug_assert!(c.format == TextureFormat::R8);
    let frac = i;
    let i = shr7(i);
    let row0 = computeRow(c, i, 1);
    let row1 = row0 + computeNextRowOffset(c, i);
    let fracx = computeFracX(c, i, frac);
    let fracy = computeFracY(frac);

    #[cfg(target_arch = "x86_64")]
    if let Some(r) = linear_r8_sse2(c, row0, row1, fracx, fracy) {
        return r;
    }
    #[cfg(target_arch = "aarch64")]
    if let Some(r) = linear_r8_neon(c, row0, row1, fracx, fracy) {
        return r;
    }
    let tap = |r: i32| c.at8::<V2<u8>>(r);
    let abcd0 = i16x8(combine4(tap(row0.0[0]), tap(row0.0[1]), tap(row0.0[2]), tap(row0.0[3])));
    let abcd1 = i16x8(combine4(tap(row1.0[0]), tap(row1.0[1]), tap(row1.0[2]), tap(row1.0[3])));
    let abcd0 = lerp7(abcd0, abcd1, sw(fracy, [0, 0, 1, 1, 2, 2, 3, 3]));
    let abcd0 = shuffle(abcd0, abcd0, [0, 2, 4, 6, 1, 3, 5, 7]);
    let abcdl: I16 = low_half(abcd0);
    let abcdh: I16 = high_half(abcd0);
    let abcdl = abcdl + (((abcdh - abcdl) * fracx) >> 7);
    bc(abcdl)
}

pub fn textureLinearR8<S: Sampler>(s: &S, p: vec2) -> vec4 {
    debug_assert!(s.common().format == TextureFormat::R8);
    let i = ivec2::from_vec2(linearQuantize_s(p, 128.0, s));
    let r = textureLinearUnpackedR8(s, i).convert::<f32>();
    vec4::new(r * (1.0f32 / 255.0f32), Float::splat(0.0), Float::splat(0.0), Float::splat(1.0))
}

/// `WidePlanarRG8`.
#[derive(Clone, Copy, Debug)]
pub struct WidePlanarRG8 {
    pub rg: V8<u16>,
}

/// The RG8 taps of both rows, blended by the Y fraction: `(ab0, cd0)`.
#[inline(always)]
fn rg8_rows(c: &samplerCommon, row0: I32, row1: I32, fracy: I16) -> (V8<i16>, V8<i16>) {
    let tap = |r: i32| c.at16::<V4<u8>>(r);
    let ab0 = i16x8(combine(tap(row0.0[0]), tap(row0.0[1])));
    let ab1 = i16x8(combine(tap(row1.0[0]), tap(row1.0[1])));
    let ab0 = lerp7(ab0, ab1, sw(fracy, [0, 0, 0, 0, 1, 1, 1, 1]));
    let cd0 = i16x8(combine(tap(row0.0[2]), tap(row0.0[3])));
    let cd1 = i16x8(combine(tap(row1.0[2]), tap(row1.0[3])));
    let cd0 = lerp7(cd0, cd1, sw(fracy, [2, 2, 2, 2, 3, 3, 3, 3]));
    (ab0, cd0)
}

pub fn textureLinearPlanarRG8<S: Sampler>(s: &S, i: ivec2) -> WidePlanarRG8 {
    let c = s.common();
    debug_assert!(c.format == TextureFormat::RG8);
    let frac = i;
    let i = shr7(i);
    let row0 = computeRow(c, i, 1);
    let row1 = row0 + computeNextRowOffset(c, i);
    let fracx = computeFracX(c, i, frac);
    let fracy = computeFracY(frac);
    let (ab0, cd0) = rg8_rows(c, row0, row1, fracy);
    let ac = zip_low(ab0, cd0);
    let bd = zip_high(ab0, cd0);
    let abcdl = zip_low(ac, bd);
    let abcdh = zip_high(ac, bd);
    let abcdl = lerp7(abcdl, abcdh, sw(fracx, [0, 1, 2, 3, 0, 1, 2, 3]));
    WidePlanarRG8 { rg: bc(abcdl) }
}

pub fn textureLinearRG8<S: Sampler>(s: &S, p: vec2) -> vec4 {
    let i = ivec2::from_vec2(linearQuantize_s(p, 128.0, s));
    let planar = textureLinearPlanarRG8(s, i);
    let rg = planar.rg.convert::<f32>() * (1.0f32 / 255.0f32);
    let r: Float = low_half(rg);
    let g: Float = high_half(rg);
    vec4::new(r, g, Float::splat(0.0), Float::splat(1.0))
}

/// `a += mulhi(b, f) << 1` as each target computes it: `_mm_mulhi_epi16(b, f) << 1` on SSE2, `vqrdmulhq_s16(b, f)`
/// (rounding, doubling, saturating) on NEON, `((int32(b) * int32(f)) >> 16) << 1` elsewhere.
#[inline(always)]
fn add_mulhi<const N: usize>(a: Vector<i16, N>, b: Vector<i16, N>, f: Vector<i16, N>) -> Vector<i16, N> {
    // The instructions themselves where the vector fits a register (LLVM left the lane loop below scalar).
    #[cfg(target_arch = "x86_64")]
    if vector::sse16::fits::<i16, N>() {
        use core::arch::x86_64::*;
        // SAFETY: SSE2 is part of x86_64.
        return a + vector::sse16::zip(b, f, |x, y| unsafe { _mm_slli_epi16(_mm_mulhi_epi16(x, y), 1) });
    }
    #[cfg(target_arch = "aarch64")]
    if vector::neon16::fits::<i16, N>() {
        // SAFETY: NEON is part of aarch64.
        return a + vector::neon16::zip(b, f, |x, y| unsafe { core::arch::aarch64::vqrdmulhq_s16(x, y) });
    }
    let mut r = [0i16; N];
    for k in 0..N {
        #[cfg(target_arch = "aarch64")]
        let m = {
            let p = 2 * (b.0[k] as i64) * (f.0[k] as i64) + (1 << 15);
            (p >> 16).clamp(i16::MIN as i64, i16::MAX as i64) as i16
        };
        #[cfg(not(target_arch = "aarch64"))]
        let m = ((((b.0[k] as i32) * (f.0[k] as i32)) >> 16) as i16).wrapping_shl(1);
        r[k] = a.0[k].wrapping_add(m);
    }
    Vector(r)
}

/// The R16/RG16 X fraction in 2^15: `CONVERT(((frac.x & (i.x >= 0)) | (i.x > w - 2)) & 0x7F, I16) << 8`.
#[inline(always)]
fn frac15_x(c: &samplerCommon, i: ivec2, frac: ivec2) -> I16 {
    (((frac.x & i.x.cmp_ge(0)) | i.x.cmp_gt(c.width as i32 - 2)) & 0x7F).convert::<i16>() << 8
}

/// Samples an R16 texture with linear filtering into signed 15-bit lanes.
pub fn textureLinearUnpackedR16<S: Sampler>(s: &S, i: ivec2) -> I16 {
    let c = s.common();
    debug_assert!(c.format == TextureFormat::R16);
    let frac = i;
    let i = shr7(i);
    let row0 = computeRow(c, i, 1);
    let row1 = row0 + computeNextRowOffset(c, i);
    let fracx = frac15_x(c, i, frac);
    let fracy = computeFracY(frac) << 8;

    let tap = |r: i32| c.at16::<V2<u16>>(r);
    let abcd0: V8<i16> = (combine4(tap(row0.0[0]), tap(row0.0[1]), tap(row0.0[2]), tap(row0.0[3])) >> 1).convert();
    let abcd1: V8<i16> = (combine4(tap(row1.0[0]), tap(row1.0[1]), tap(row1.0[2]), tap(row1.0[3])) >> 1).convert();
    let abcd0 = add_mulhi(abcd0, abcd1 - abcd0, sw(fracy, [0, 0, 1, 1, 2, 2, 3, 3]));
    let abcd0 = shuffle(abcd0, abcd0, [0, 2, 4, 6, 1, 3, 5, 7]);
    let abcdl: I16 = low_half(abcd0);
    let abcdh: I16 = high_half(abcd0);
    add_mulhi(abcdl, abcdh - abcdl, fracx)
}

pub fn textureLinearR16<S: Sampler>(s: &S, p: vec2) -> vec4 {
    debug_assert!(s.common().format == TextureFormat::R16);
    let i = ivec2::from_vec2(linearQuantize_s(p, 128.0, s));
    let r = textureLinearUnpackedR16(s, i).convert::<f32>();
    vec4::new(r * (1.0f32 / 32767.0f32), Float::splat(0.0), Float::splat(0.0), Float::splat(1.0))
}

/// Samples an RG16 texture with linear filtering into signed 15-bit lanes, `r g` per pixel.
pub fn textureLinearUnpackedRG16<S: Sampler>(s: &S, i: ivec2) -> V8<i16> {
    let c = s.common();
    debug_assert!(c.format == TextureFormat::RG16);
    let frac = i;
    let i = shr7(i);
    let row0 = computeRow(c, i, 1);
    let row1 = row0 + computeNextRowOffset(c, i);
    let fracx = frac15_x(c, i, frac);
    let fracy = computeFracY(frac) << 8;

    let tap = |r: i32| c.at32::<V4<u16>>(r);
    let pair = |r: I32, k: usize| -> V8<i16> { (combine(tap(r.0[k]), tap(r.0[k + 1])) >> 1).convert() };
    let ab0 = pair(row0, 0);
    let cd0 = pair(row0, 2);
    let ab1 = pair(row1, 0);
    let cd1 = pair(row1, 2);
    let ab0 = add_mulhi(ab0, ab1 - ab0, sw(fracy, [0, 0, 0, 0, 1, 1, 1, 1]));
    let cd0 = add_mulhi(cd0, cd1 - cd0, sw(fracy, [2, 2, 2, 2, 3, 3, 3, 3]));
    let ac = zip2_low(ab0, cd0);
    let bd = zip2_high(ab0, cd0);
    let abcdl = zip2_low(ac, bd);
    let abcdh = zip2_high(ac, bd);
    add_mulhi(abcdl, abcdh - abcdl, sw(fracx, [0, 0, 1, 1, 2, 2, 3, 3]))
}

pub fn textureLinearRG16<S: Sampler>(s: &S, p: vec2) -> vec4 {
    debug_assert!(s.common().format == TextureFormat::RG16);
    let i = ivec2::from_vec2(linearQuantize_s(p, 128.0, s));
    let rg: I32 = bc(textureLinearUnpackedRG16(s, i));
    let r = cast(rg & 0xFFFF) * (1.0f32 / 32767.0f32);
    let g = cast(rg >> 16) * (1.0f32 / 32767.0f32);
    vec4::new(r, g, Float::splat(0.0), Float::splat(1.0))
}

pub fn textureLinearRGBA32F<S: Sampler>(s: &S, p: vec2) -> vec4 {
    let c = s.common();
    debug_assert!(c.format == TextureFormat::RGBA32F);
    let p = samplerScale(s, p);
    let p = vec2::new(p.x - 0.5f32, p.y - 0.5f32);
    let f = vec2::new(floor(p.x), floor(p.y));
    let mut r = vec2::new(p.x - f.x, p.y - f.y);
    let i = ivec2::from_vec2(f);
    let cx = clampCoord(i.x, c.width as i32 - 1, 0);
    let cy = clampCoord(i.y, c.height as i32, 0);
    r.x = if_then_else(
        i.x.cmp_ge(0),
        if_then_else(i.x.cmp_lt(c.width.wrapping_sub(1) as i32), r.x, Float::splat(1.0)),
        Float::splat(0.0),
    );
    let offset0 = cx * 4 + cy * c.stride_i();
    let offset1 = offset0 + computeNextRowOffset(c, i);
    let px = |o: i32| c.at32::<Float>(o);
    let lane = |k: usize| {
        mix(
            mix(px(offset0.0[k]), px(offset0.0[k].wrapping_add(4)), r.x),
            mix(px(offset1.0[k]), px(offset1.0[k].wrapping_add(4)), r.x),
            r.y,
        )
    };
    pixel_float_to_vec4(lane(0), lane(1), lane(2), lane(3))
}

/// `WidePlanarYUV8`.
#[derive(Clone, Copy, Debug)]
pub struct WidePlanarYUV8 {
    pub y: U16,
    pub u: U16,
    pub v: U16,
}

pub fn textureLinearPlanarYUY2<S: Sampler>(s: &S, i: ivec2) -> WidePlanarYUV8 {
    let c = s.common();
    debug_assert!(c.format == TextureFormat::YUY2);
    let mut frac = i;
    let i = shr7(i);
    let row0 = computeRow(c, i, 2);
    // 2-pixel chunks of 4 bytes, G0 B G1 R: the selector picks the pixel, the row is aligned to the chunk.
    let selector = row0 & 1;
    let row0 = row0 & !1;
    let row1 = row0 + computeNextRowOffset(c, i);
    frac.x &= i.x.cmp_ge(0);
    let over = i.x.cmp_gt(c.width as i32 - 3);
    let fracx: V8<i16> = (combine(frac.x | over, (frac.x >> 1) | over) & 0x7F).convert();
    let fracy = computeFracY(frac);

    let tap = |r: i32| i16x8(c.at16::<V8<u8>>(r));
    let a0 = lerp7(tap(row0.0[0]), tap(row1.0[0]), fracy.0[0]);
    let b0 = lerp7(tap(row0.0[1]), tap(row1.0[1]), fracy.0[1]);
    let c0 = lerp7(tap(row0.0[2]), tap(row1.0[2]), fracy.0[2]);
    let d0 = lerp7(tap(row0.0[3]), tap(row1.0[3]), fracy.0[3]);

    let abl = zip_low(a0, b0);
    let cdl = zip_low(c0, d0);
    let g0b = zip2_low(abl, cdl);
    let g1r = zip2_high(abl, cdl);

    let abh = shuffle(a0, b0, [2, 10, 5, 13, 4, 12, 7, 15]);
    let cdh = shuffle(c0, d0, [2, 10, 5, 13, 4, 12, 7, 15]);
    let g1b = zip2_low(abh, cdh);
    let g0r = zip2_high(abh, cdh);

    let g0b = lerp7(g0b, g1b, fracx);
    let g1r = lerp7(g1r, g0r, fracx);

    let sel: I16 = (-selector).convert::<i16>();
    WidePlanarYUV8 {
        y: bc(if_then_else(sel, low_half(g1r), low_half(g0b))),
        u: bc::<U16, I16>(high_half(g0b)),
        v: bc::<U16, I16>(high_half(g1r)),
    }
}

pub fn textureLinearYUY2<S: Sampler>(s: &S, p: vec2) -> vec4 {
    let i = ivec2::from_vec2(linearQuantize_s(p, 128.0, s));
    let planar = textureLinearPlanarYUY2(s, i);
    let y = planar.y.convert::<f32>() * (1.0f32 / 255.0f32);
    let u = planar.u.convert::<f32>() * (1.0f32 / 255.0f32);
    let v = planar.v.convert::<f32>() * (1.0f32 / 255.0f32);
    vec4::new(v, y, u, Float::splat(1.0))
}

/// The `texture(sampler, P)` overloads.
pub trait TextureSample<P> {
    type Output;
    fn texture(&self, p: P) -> Self::Output;
}

#[inline(always)]
pub fn texture<S: TextureSample<P>, P>(s: S, p: P) -> S::Output {
    s.texture(p)
}

fn texture_linear_any<S: Sampler>(s: &S, p: vec2, rect: bool) -> vec4 {
    match s.common().format {
        TextureFormat::RGBA32F if !rect => textureLinearRGBA32F(s, p),
        TextureFormat::RGBA8 => textureLinearRGBA8(s, p),
        TextureFormat::R8 => textureLinearR8(s, p),
        TextureFormat::RG8 => textureLinearRG8(s, p),
        TextureFormat::R16 => textureLinearR16(s, p),
        TextureFormat::RG16 => textureLinearRG16(s, p),
        TextureFormat::YUY2 => textureLinearYUY2(s, p),
        _ => {
            debug_assert!(false);
            vec4::default()
        }
    }
}

impl TextureSample<vec2> for sampler2D_impl {
    type Output = vec4;
    #[inline(always)]
    fn texture(&self, p: vec2) -> vec4 {
        if self.filter() == TextureFilter::LINEAR {
            // swgl's `texture(sampler2D, vec2)` is `SI`: the format switch is inlined at every call site and
            // the common formats call their sampler directly, without the shared dispatch below.
            match self.common.format {
                TextureFormat::R8 => textureLinearR8(self, p),
                TextureFormat::RGBA8 => textureLinearRGBA8(self, p),
                _ => texture_linear_any(self, p, false),
            }
        } else {
            let coord = ivec2::new(
                roundzero(p.x, Float::splat(self.width as f32)),
                roundzero(p.y, Float::splat(self.height as f32)),
            );
            texelFetch(*self, coord, 0)
        }
    }
}

impl TextureSample<vec2> for sampler2DRect_impl {
    type Output = vec4;
    #[inline(always)]
    fn texture(&self, p: vec2) -> vec4 {
        if self.filter() == TextureFilter::LINEAR {
            texture_linear_any(self, p, true)
        } else {
            let coord = ivec2::new(roundzero(p.x, Float::splat(1.0)), roundzero(p.y, Float::splat(1.0)));
            texelFetch(*self, coord, 0)
        }
    }
}

impl<S: TextureSample<vec2, Output = vec4>> TextureSample<vec2_scalar> for S {
    type Output = vec4_scalar;
    #[inline(always)]
    fn texture(&self, p: vec2_scalar) -> vec4_scalar {
        force_scalar(self.texture(vec2::new(Float::splat(p.x), Float::splat(p.y))))
    }
}

/// `textureSize(sampler, lod)`; for `sampler2DRect` (no lod in the C++) the lod is ignored.
pub fn textureSize<S: Sampler>(s: S, _lod: i32) -> ivec2_scalar {
    let c = s.common();
    ivec2_scalar::new(c.width as i32, c.height as i32)
}

// ---- packed linear filters (texture.h, for the span fast paths and composites) -------------------------------

pub fn textureLinearUnpackedRGBA8<S: Sampler>(s: &S, i: ivec2) -> WideRGBA8 {
    let c = s.common();
    debug_assert!(c.format == TextureFormat::RGBA8);
    let frac = i;
    let i = shr7(i);
    let row0 = computeRow(c, i, 1);
    let row1 = row0 + computeNextRowOffset(c, i);
    let fracx = computeFracX(c, i, frac);
    let fracy = computeFracY(frac);

    let tap = |r: i32| i16x8(c.at32::<V8<u8>>(r));
    let a0 = lerp7(tap(row0.0[0]), tap(row1.0[0]), fracy.0[0]);
    let b0 = lerp7(tap(row0.0[1]), tap(row1.0[1]), fracy.0[1]);
    let abl = combine(low_half(a0), low_half(b0));
    let abh = combine(high_half(a0), high_half(b0));
    let abl = lerp7(abl, abh, sw(fracx, [0, 0, 0, 0, 1, 1, 1, 1]));

    let c0 = lerp7(tap(row0.0[2]), tap(row1.0[2]), fracy.0[2]);
    let d0 = lerp7(tap(row0.0[3]), tap(row1.0[3]), fracy.0[3]);
    let cdl = combine(low_half(c0), low_half(d0));
    let cdh = combine(high_half(c0), high_half(d0));
    let cdl = lerp7(cdl, cdh, sw(fracx, [2, 2, 2, 2, 3, 3, 3, 3]));

    combine(bc::<HalfRGBA8, V8<i16>>(abl), bc::<HalfRGBA8, V8<i16>>(cdl))
}

pub fn textureLinearPackedRGBA8<S: Sampler>(s: &S, i: ivec2) -> PackedRGBA8 {
    pack(textureLinearUnpackedRGBA8(s, i))
}

pub fn textureNearestPackedRGBA8<S: Sampler>(s: &S, i: ivec2) -> PackedRGBA8 {
    let c = s.common();
    debug_assert!(c.format == TextureFormat::RGBA8);
    let row = computeRow(c, i, 0);
    let tap = |r: i32| c.at32::<V4<u8>>(r);
    combine4(tap(row.0[0]), tap(row.0[1]), tap(row.0[2]), tap(row.0[3]))
}

pub fn textureLinearPackedR8<S: Sampler>(s: &S, i: ivec2) -> PackedR8 {
    pack(textureLinearUnpackedR8(s, i))
}

pub fn textureLinearUnpackedRG8<S: Sampler>(s: &S, i: ivec2) -> WideRG8 {
    let c = s.common();
    debug_assert!(c.format == TextureFormat::RG8);
    let frac = ivec2::new(i.x & 0x7F, i.y & 0x7F);
    let i = shr7(i);
    let row0 = computeRow(c, i, 1);
    let row1 = row0 + computeNextRowOffset(c, i);
    let fracx = computeFracX(c, i, frac);
    let fracy = computeFracY(frac);
    let (ab0, cd0) = rg8_rows(c, row0, row1, fracy);
    let ac = zip2_low(ab0, cd0);
    let bd = zip2_high(ab0, cd0);
    let abcdl = zip2_low(ac, bd);
    let abcdh = zip2_high(ac, bd);
    let abcdl = lerp7(abcdl, abcdh, sw(fracx, [0, 0, 1, 1, 2, 2, 3, 3]));
    bc(abcdl)
}

pub fn textureLinearPackedRG8<S: Sampler>(s: &S, i: ivec2) -> PackedRG8 {
    pack(textureLinearUnpackedRG8(s, i))
}

/// `addsat(x, y)` on words: unsigned saturating (`_mm_adds_epu16`, `vqaddq_u16`, `r | (r < x)`: all the same).
#[inline(always)]
pub fn addsat<const N: usize>(x: Vector<u16, N>, y: Vector<u16, N>) -> Vector<u16, N> {
    // The instructions swgl calls (one each; LLVM left the lane loop below scalar), as packRGBA8 does.
    #[cfg(target_arch = "x86_64")]
    if vector::sse16::fits::<u16, N>() {
        // SAFETY: SSE2 is part of x86_64.
        return vector::sse16::zip(x, y, |a, b| unsafe { core::arch::x86_64::_mm_adds_epu16(a, b) });
    }
    #[cfg(target_arch = "aarch64")]
    if vector::neon16::fits::<u16, N>() {
        // SAFETY: NEON is part of aarch64.
        return vector::neon16::zip(x, y, |a, b| unsafe {
            core::arch::aarch64::vreinterpretq_s16_u16(core::arch::aarch64::vqaddq_u16(
                core::arch::aarch64::vreinterpretq_u16_s16(a),
                core::arch::aarch64::vreinterpretq_u16_s16(b),
            ))
        });
    }
    let mut r = [0u16; N];
    for k in 0..N {
        r[k] = x.0[k].saturating_add(y.0[k]);
    }
    Vector(r)
}

/// `uint16_t(coeff + 0.5f)`: float to `int` as the target converts it, then truncated to 16 bits.
#[inline(always)]
fn coeff_u16(coeff: f32) -> u16 {
    vector::f32_to_i32(coeff + 0.5f32) as u16
}

macro_rules! gaussian_blur {
    ($h:ident, $v:ident, $p:ty, $n:literal, $e:literal) => {
        /// `gaussianBlurHorizontal<P>`: a horizontal Gaussian over a 4-pixel chunk at `i`, 8.8 fixed point.
        /// `UNCHECKED`: the caller proved every texel read below lies inside the sampler's bytes, so the loads
        /// skip `load_texels`' bounds check (the C++ reads raw pointers there).
        pub fn $h<S: Sampler, const UNCHECKED: bool>(
            s: &S,
            i: &ivec2_scalar,
            min_x: i32,
            max_x: i32,
            radius: i32,
            mut coeff: f32,
            mut coeff_step: f32,
        ) -> Vector<u16, $n> {
            let c = s.common();
            coeff *= (1 << 8) as f32;
            let coeff_step2 = coeff_step * coeff_step;
            let row = computeRow_scalar(c, *i, 1);
            let px = |k: i32| -> $p {
                if UNCHECKED {
                    // SAFETY: the caller's proof covers every `k` read below.
                    unsafe { c.at_unchecked::<$p, $e>(k) }
                } else {
                    c.at::<$p, $e>(k)
                }
            };
            let chunk = |k: i32| -> V4<$p> {
                if UNCHECKED {
                    // SAFETY: as `px`.
                    unsafe { c.at_unchecked::<V4<$p>, $e>(k) }
                } else {
                    c.at::<V4<$p>, $e>(k)
                }
            };
            let wide = |v: V4<$p>| -> Vector<u16, $n> { bc::<Vector<u8, $n>, V4<$p>>(v).convert::<u16>() };
            let mut pixels_right = chunk(row);
            let mut pixels_left = pixels_right;
            let mut sum = wide(pixels_right) * coeff_u16(coeff);

            let mut offset = 1;
            let left_bound = i.x - min_x.max(0);
            let right_bound = max_x.min(c.width as i32 - 1) - i.x;
            let valid_radius = radius.min(left_bound.min(right_bound - (4 - 1)));
            while offset <= valid_radius {
                // The C++ rotates the chunk registers and reloads one pixel per offset; after the rotation
                // the registers hold exactly the whole shifted chunk (`offset + 3 <= right_bound`,
                // `offset <= left_bound`), so load it directly and keep the pixels in registers instead of
                // spilling them around a lane store.
                pixels_right = chunk(row.wrapping_add(offset));
                pixels_left = chunk(row.wrapping_sub(offset));
                coeff *= coeff_step;
                coeff_step *= coeff_step2;
                sum = addsat(sum, (wide(pixels_right) + wide(pixels_left)) * coeff_u16(coeff));
                offset += 1;
            }
            while offset <= radius {
                // Clamped loop, exactly as the C++: rotate the registers and reload only the shifted-out
                // pixel, clamped. Lanes that still come from the initial chunk keep their unclamped texels,
                // as in swgl's raw reads.
                pixels_right.0[0] = px(row.wrapping_add((offset + 4 - 1).min(right_bound)));
                pixels_right = sw(pixels_right, [1, 2, 3, 0]);
                pixels_left = sw(pixels_left, [3, 0, 1, 2]);
                pixels_left.0[0] = px(row.wrapping_sub(offset.min(left_bound)));
                coeff *= coeff_step;
                coeff_step *= coeff_step2;
                sum = addsat(sum, (wide(pixels_right) + wide(pixels_left)) * coeff_u16(coeff));
                offset += 1;
            }
            sum >> 8
        }

        /// `gaussianBlurVertical<P>`: a vertical Gaussian over a 4-pixel chunk at `i`, 8.8 fixed point.
        /// `UNCHECKED`: as [`gaussianBlurHorizontal`]'s.
        pub fn $v<S: Sampler, const UNCHECKED: bool>(
            s: &S,
            i: &ivec2_scalar,
            min_y: i32,
            max_y: i32,
            radius: i32,
            mut coeff: f32,
            mut coeff_step: f32,
        ) -> Vector<u16, $n> {
            let c = s.common();
            coeff *= (1 << 8) as f32;
            let coeff_step2 = coeff_step * coeff_step;
            let mut row_above = computeRow_scalar(c, *i, 1);
            let mut row_below = row_above;
            let chunk = |k: i32| -> V4<$p> {
                if UNCHECKED {
                    // SAFETY: the caller's proof covers every `k` read below.
                    unsafe { c.at_unchecked::<V4<$p>, $e>(k) }
                } else {
                    c.at::<V4<$p>, $e>(k)
                }
            };
            let wide = |v: V4<$p>| -> Vector<u16, $n> { bc::<Vector<u8, $n>, V4<$p>>(v).convert::<u16>() };
            let mut sum = wide(chunk(row_above)) * coeff_u16(coeff);

            let mut offset = 1;
            let below_bound = i.y - min_y.max(0);
            let above_bound = max_y.min(c.height as i32 - 1) - i.y;
            let valid_radius = radius.min(below_bound.min(above_bound));
            while offset <= valid_radius {
                row_above = row_above.wrapping_add(c.stride_i());
                row_below = row_below.wrapping_sub(c.stride_i());
                coeff *= coeff_step;
                coeff_step *= coeff_step2;
                sum = addsat(sum, (wide(chunk(row_above)) + wide(chunk(row_below))) * coeff_u16(coeff));
                offset += 1;
            }
            while offset <= radius {
                if offset <= above_bound {
                    row_above = row_above.wrapping_add(c.stride_i());
                }
                if offset <= below_bound {
                    row_below = row_below.wrapping_sub(c.stride_i());
                }
                coeff *= coeff_step;
                coeff_step *= coeff_step2;
                sum = addsat(sum, (wide(chunk(row_above)) + wide(chunk(row_below))) * coeff_u16(coeff));
                offset += 1;
            }
            sum >> 8
        }
    };
}
gaussian_blur!(gaussianBlurHorizontal_RGBA8, gaussianBlurVertical_RGBA8, u32, 16, 4);
gaussian_blur!(gaussianBlurHorizontal_R8, gaussianBlurVertical_R8, u8, 4, 1);
