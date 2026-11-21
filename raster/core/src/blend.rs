/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Port of swgl's `blend.h`: the blend keys of gl.cc, packing of shader output to 8-bit pixels, `blend_pixels`
//! for every key (with the MASK, AA and AA_MASK groups), and the span loads, stores and commits built on it.
//!
//! The pixel pointers are the rasterizer's span pointers, as in the C++: functions that dereference one are
//! `unsafe fn`, and their callers name the bound that keeps the span inside its buffer. The integer
//! instructions the C++ picks per target (`_mm_packs_epi32` vs `vqmovun_s32`, `_mm_min_epi16` vs `vminq_u16`) are
//! reproduced by their exact semantics under the same `cfg`.

// Lane loops index several arrays by the lane number, as the C++ lane expressions do.
#![allow(clippy::needless_range_loop)]

use crate::glsl::{
    bit_cast, clamp, combine, if_then_else, inversesqrt, ivec4, load_bytes, max, min, recip, repeat2, round_pixel,
    shuffle, store_bytes, unaligned_load, unaligned_store, vec3, vec4, vec4_scalar, zip, zip2_high, zip2_low, zip_high,
    zip_low, Float, Lane, Pod, Vector, I32, U32, V16,
};
use crate::program::FragmentShaderState;
use crate::state::RasterState;
use crate::texture::{
    pack, unpack, HalfRGBA8, PackedR8, PackedRGBA32F, PackedRGBA8, WideR8, WideRG8, WideRGBA32F, WideRGBA8,
};

// ---- gl.cc: BlendKey ----------------------------------------------------------------------------------------

/// gl.cc's `enum BlendKey : uint8_t`: an index into `rules::BLEND_TABLE` (the keys of `FOR_EACH_BLEND_KEY`), plus
/// `MASK_BLEND_KEY_NONE`, `AA_BLEND_KEY_NONE` or `AA_MASK_BLEND_KEY_NONE` for the clip-mask and AA forms.
pub type BlendKey = u8;

/// Keys in each group: the entries of `FOR_EACH_BLEND_KEY` (`rules::BLEND_TABLE` without its padding entry).
pub const BLEND_KEY_COUNT: u8 = 31;

pub const BLEND_ONE_ZERO: BlendKey = 0;
pub const BLEND_SRC_ALPHA_ONE_MINUS_SRC_ALPHA_ONE_ONE_MINUS_SRC_ALPHA: BlendKey = 1;
pub const BLEND_ONE_ONE_MINUS_SRC_ALPHA: BlendKey = 2;
pub const BLEND_ZERO_ONE_MINUS_SRC_COLOR: BlendKey = 3;
pub const BLEND_ZERO_ONE_MINUS_SRC_COLOR_ZERO_ONE: BlendKey = 4;
pub const BLEND_ZERO_ONE_MINUS_SRC_ALPHA: BlendKey = 5;
pub const BLEND_ZERO_SRC_COLOR: BlendKey = 6;
pub const BLEND_ONE_ONE: BlendKey = 7;
pub const BLEND_ONE_ONE_ONE_ONE_MINUS_SRC_ALPHA: BlendKey = 8;
pub const BLEND_ONE_MINUS_DST_ALPHA_ONE_ZERO_ONE: BlendKey = 9;
pub const BLEND_CONSTANT_COLOR_ONE_MINUS_SRC_COLOR: BlendKey = 10;
pub const BLEND_ONE_ONE_MINUS_SRC1_COLOR: BlendKey = 11;
pub const BLEND_MIN: BlendKey = 12;
pub const BLEND_MAX: BlendKey = 13;
pub const BLEND_MULTIPLY_KHR: BlendKey = 14;
pub const BLEND_SCREEN_KHR: BlendKey = 15;
pub const BLEND_OVERLAY_KHR: BlendKey = 16;
pub const BLEND_DARKEN_KHR: BlendKey = 17;
pub const BLEND_LIGHTEN_KHR: BlendKey = 18;
pub const BLEND_COLORDODGE_KHR: BlendKey = 19;
pub const BLEND_COLORBURN_KHR: BlendKey = 20;
pub const BLEND_HARDLIGHT_KHR: BlendKey = 21;
pub const BLEND_SOFTLIGHT_KHR: BlendKey = 22;
pub const BLEND_DIFFERENCE_KHR: BlendKey = 23;
pub const BLEND_EXCLUSION_KHR: BlendKey = 24;
pub const BLEND_HSL_HUE_KHR: BlendKey = 25;
pub const BLEND_HSL_SATURATION_KHR: BlendKey = 26;
pub const BLEND_HSL_COLOR_KHR: BlendKey = 27;
pub const BLEND_HSL_LUMINOSITY_KHR: BlendKey = 28;
pub const SWGL_BLEND_DROP_SHADOW: BlendKey = 29;
pub const SWGL_BLEND_SUBPIXEL_TEXT: BlendKey = 30;

pub const BLEND_KEY_NONE: BlendKey = BLEND_ONE_ZERO;
pub const MASK_BLEND_KEY_NONE: BlendKey = BLEND_KEY_COUNT;
pub const AA_BLEND_KEY_NONE: BlendKey = 2 * BLEND_KEY_COUNT;
pub const AA_MASK_BLEND_KEY_NONE: BlendKey = 3 * BLEND_KEY_COUNT;

/// The key for `rules::BLEND_TABLE[i]` (`BLEND_KEY(...)`): the table and the enum share their order.
#[inline]
pub fn blend_key_for_table_index(i: usize) -> BlendKey {
    debug_assert!(i < BLEND_KEY_COUNT as usize);
    const { assert!(lucinanet_raster_rules::BLEND_TABLE.len() == BLEND_KEY_COUNT as usize + 1) };
    i as BlendKey
}

// ---- packing ------------------------------------------------------------------------------------------------

/// `packRGBA8(a, b)`: eight `int`s to words. SSE2 saturates to the signed range (`_mm_packs_epi32`), NEON to the
/// unsigned one (`vqmovun_s32`), the portable path truncates.
#[inline(always)]
pub fn packRGBA8(a: I32, b: I32) -> HalfRGBA8 {
    // The instructions swgl calls (one each; LLVM left the lane loop below scalar), saturating as `pack_word` says.
    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::x86_64::*;
        let mut r = HalfRGBA8::default();
        // SAFETY: SSE2 is part of x86_64; I32 and HalfRGBA8 are 16 bytes each (unaligned loads and stores).
        unsafe {
            let v = _mm_packs_epi32(_mm_loadu_si128(a.0.as_ptr() as *const __m128i), _mm_loadu_si128(b.0.as_ptr() as *const __m128i));
            _mm_storeu_si128(r.0.as_mut_ptr() as *mut __m128i, v);
        }
        r
    }
    #[cfg(target_arch = "aarch64")]
    {
        use core::arch::aarch64::*;
        let mut r = HalfRGBA8::default();
        // SAFETY: NEON is part of aarch64; I32 and HalfRGBA8 are 16 bytes each, 4-byte aligned or more.
        unsafe {
            let v = vcombine_u16(vqmovun_s32(vld1q_s32(a.0.as_ptr())), vqmovun_s32(vld1q_s32(b.0.as_ptr())));
            vst1q_u16(r.0.as_mut_ptr(), v);
        }
        r
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    packRGBA8_lanes(a, b)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline(always)]
fn packRGBA8_lanes(a: I32, b: I32) -> HalfRGBA8 {
    let mut r = [0u16; 8];
    for k in 0..8 {
        let v = if k < 4 { a.0[k] } else { b.0[k - 4] };
        r[k] = pack_word(v);
    }
    Vector(r)
}

#[inline(always)]
fn pack_word(v: i32) -> u16 {
    #[cfg(target_arch = "x86_64")]
    {
        Ord::clamp(v, i16::MIN as i32, i16::MAX as i32) as i16 as u16
    }
    #[cfg(target_arch = "aarch64")]
    {
        Ord::clamp(v, 0, u16::MAX as i32) as u16
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        v as u16
    }
}

/// `packR8(a)`: as `packRGBA8`, one pixel per lane.
#[inline(always)]
pub fn packR8(a: I32) -> WideR8 {
    WideR8::new([pack_word(a.0[0]), pack_word(a.0[1]), pack_word(a.0[2]), pack_word(a.0[3])])
}

/// The `pack_pixels_RGBA8(v, scale)` overloads; `scale` is the C++ default `255.0f` unless given.
pub trait PackPixelsRGBA8 {
    fn pack_pixels_RGBA8(self, scale: f32) -> WideRGBA8;
}

#[inline(always)]
pub fn pack_pixels_RGBA8<T: PackPixelsRGBA8>(v: T, scale: f32) -> WideRGBA8 {
    v.pack_pixels_RGBA8(scale)
}

impl PackPixelsRGBA8 for &vec4 {
    #[inline(always)]
    fn pack_pixels_RGBA8(self, scale: f32) -> WideRGBA8 {
        let i: ivec4 = round_pixel(*self, scale);
        let xz = packRGBA8(i.z, i.x);
        let yw = packRGBA8(i.y, i.w);
        let xyzwl = zip_low(xz, yw);
        let xyzwh = zip_high(xz, yw);
        let lo = zip2_low(xyzwl, xyzwh);
        let hi = zip2_high(xyzwl, xyzwh);
        combine(lo, hi)
    }
}

impl PackPixelsRGBA8 for vec4 {
    #[inline(always)]
    fn pack_pixels_RGBA8(self, scale: f32) -> WideRGBA8 {
        (&self).pack_pixels_RGBA8(scale)
    }
}

impl PackPixelsRGBA8 for Float {
    #[inline(always)]
    fn pack_pixels_RGBA8(self, scale: f32) -> WideRGBA8 {
        let i: I32 = round_pixel(self, scale);
        let c = packRGBA8(i, i);
        let c = zip_low(c, c);
        zip(c, c)
    }
}

impl PackPixelsRGBA8 for f32 {
    #[inline(always)]
    fn pack_pixels_RGBA8(self, scale: f32) -> WideRGBA8 {
        let i: I32 = round_pixel(Float::splat(self), scale);
        repeat2(packRGBA8(i, i))
    }
}

impl PackPixelsRGBA8 for &vec4_scalar {
    #[inline(always)]
    fn pack_pixels_RGBA8(self, scale: f32) -> WideRGBA8 {
        let i: I32 = round_pixel(Float::new([self.z, self.y, self.x, self.w]), scale);
        repeat2(packRGBA8(i, i))
    }
}

impl PackPixelsRGBA8 for vec4_scalar {
    #[inline(always)]
    fn pack_pixels_RGBA8(self, scale: f32) -> WideRGBA8 {
        (&self).pack_pixels_RGBA8(scale)
    }
}

/// `pack_pixels_RGBA8()`: the fragment shader's `gl_FragColor`.
impl PackPixelsRGBA8 for &FragmentShaderState {
    #[inline(always)]
    fn pack_pixels_RGBA8(self, scale: f32) -> WideRGBA8 {
        (&self.gl_FragColor).pack_pixels_RGBA8(scale)
    }
}

impl PackPixelsRGBA8 for WideRGBA32F {
    #[inline(always)]
    fn pack_pixels_RGBA8(self, scale: f32) -> WideRGBA8 {
        let v: vec4 = wide_f32_to_vec4(self);
        let i: ivec4 = round_pixel(v, scale);
        combine(packRGBA8(i.x, i.y), packRGBA8(i.z, i.w))
    }
}

/// `bit_cast<vec4>(V16<float>)`: lanes 0-3 are `x`, 4-7 `y`, ...
#[inline(always)]
fn wide_f32_to_vec4(v: WideRGBA32F) -> vec4 {
    let q = quarters(v);
    vec4::new(q[0], q[1], q[2], q[3])
}

#[inline(always)]
fn quarters(v: V16<f32>) -> [Float; 4] {
    let a = v.0;
    [
        Float::new([a[0], a[1], a[2], a[3]]),
        Float::new([a[4], a[5], a[6], a[7]]),
        Float::new([a[8], a[9], a[10], a[11]]),
        Float::new([a[12], a[13], a[14], a[15]]),
    ]
}

#[inline(always)]
fn from_quarters(q: [Float; 4]) -> V16<f32> {
    combine(combine(q[0], q[1]), combine(q[2], q[3]))
}

/// The `pack_pixels_R8(c, scale)` overloads.
pub trait PackPixelsR8 {
    fn pack_pixels_R8(self, scale: f32) -> WideR8;
}

#[inline(always)]
pub fn pack_pixels_R8<T: PackPixelsR8>(c: T, scale: f32) -> WideR8 {
    c.pack_pixels_R8(scale)
}

impl PackPixelsR8 for Float {
    #[inline(always)]
    fn pack_pixels_R8(self, scale: f32) -> WideR8 {
        packR8(round_pixel(self, scale))
    }
}

/// `pack_pixels_R8()`: `gl_FragColor.x`.
impl PackPixelsR8 for &FragmentShaderState {
    #[inline(always)]
    fn pack_pixels_R8(self, scale: f32) -> WideR8 {
        packR8(round_pixel(self.gl_FragColor.x, scale))
    }
}

// ---- 8-bit arithmetic ---------------------------------------------------------------------------------------

/// `muldiv256(x, y)`: `(x * y) >> 8`.
#[inline(always)]
pub fn muldiv256<const N: usize>(x: Vector<u16, N>, y: Vector<u16, N>) -> Vector<u16, N> {
    (x * y) >> 8
}

/// `muldiv255(x, y)`: `(x * y + x) >> 8`, swgl's approximation of `x * y / 255` (`rules::muldiv255` per channel).
#[inline(always)]
pub fn muldiv255<const N: usize>(x: Vector<u16, N>, y: Vector<u16, N>) -> Vector<u16, N> {
    (x * y + x) >> 8
}

// ---- pack_span, applyColor, packColor --------------------------------------------------------------------

/// `pack_span(buf, v, scale)`: `pack_pixels_RGBA8` for a `uint32_t*` span, `pack_pixels_R8` for a `uint8_t*` one.
pub trait PackSpan<P: SpanPtr> {
    fn pack_span(self, scale: f32) -> P::Wide;
}

impl<T: PackPixelsRGBA8> PackSpan<*mut u32> for T {
    #[inline(always)]
    fn pack_span(self, scale: f32) -> WideRGBA8 {
        pack_pixels_RGBA8(self, scale)
    }
}

impl<T: PackPixelsR8> PackSpan<*mut u8> for T {
    #[inline(always)]
    fn pack_span(self, scale: f32) -> WideR8 {
        pack_pixels_R8(self, scale)
    }
}

#[inline(always)]
pub fn pack_span<P: SpanPtr, V: PackSpan<P>>(_buf: P, v: V, scale: f32) -> P::Wide {
    v.pack_span(scale)
}

/// `NoColor`: leave the source as it is.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoColor;

/// `InvertColor`: `255 - src`.
#[derive(Clone, Copy, Debug, Default)]
pub struct InvertColor;

/// The `applyColor(src, color)` overloads.
pub trait ApplyColor<P> {
    fn apply_color(self, src: P) -> P;
}

impl<P> ApplyColor<P> for NoColor {
    #[inline(always)]
    fn apply_color(self, src: P) -> P {
        src
    }
}

impl<const N: usize> ApplyColor<Vector<u16, N>> for InvertColor {
    #[inline(always)]
    fn apply_color(self, src: Vector<u16, N>) -> Vector<u16, N> {
        Vector::splat(255) - src
    }
}

impl<const N: usize> ApplyColor<Vector<u16, N>> for Vector<u16, N> {
    /// `muldiv255(color, src)`.
    #[inline(always)]
    fn apply_color(self, src: Vector<u16, N>) -> Vector<u16, N> {
        muldiv255(self, src)
    }
}

#[inline(always)]
pub fn applyColor<P, C: ApplyColor<P>>(src: P, color: C) -> P {
    color.apply_color(src)
}

/// `applyColor(PackedRGBA8 src, WideRGBA8 color)`.
#[inline(always)]
pub fn applyColor_packed(src: PackedRGBA8, color: WideRGBA8) -> WideRGBA8 {
    applyColor(unpack(src), color)
}

/// The `packColor(buf, color)` overloads: a color packed for span pointer type `P`, or `NoColor`/`InvertColor`
/// passed through.
pub trait PackColor<P: SpanPtr> {
    type Output;
    fn pack_color(self) -> Self::Output;
}

impl<P: SpanPtr> PackColor<P> for NoColor {
    type Output = NoColor;
    #[inline(always)]
    fn pack_color(self) -> NoColor {
        self
    }
}

impl<P: SpanPtr> PackColor<P> for InvertColor {
    type Output = InvertColor;
    #[inline(always)]
    fn pack_color(self) -> InvertColor {
        self
    }
}

macro_rules! pack_color_rgba8 {
    ($($t:ty),*) => {$(
        impl PackColor<*mut u32> for $t {
            type Output = WideRGBA8;
            #[inline(always)]
            fn pack_color(self) -> WideRGBA8 {
                pack_pixels_RGBA8(self, 255.0)
            }
        }
    )*};
}
pack_color_rgba8!(vec4, Float, f32, vec4_scalar, WideRGBA32F);

impl PackColor<*mut u8> for Float {
    type Output = WideR8;
    #[inline(always)]
    fn pack_color(self) -> WideR8 {
        pack_pixels_R8(self, 255.0)
    }
}

/// `packColor(buf, color)`.
#[inline(always)]
pub fn packColor<P: SpanPtr, C: PackColor<P>>(_buf: P, color: C) -> C::Output {
    color.pack_color()
}

/// `addlow(x, y)`: byte-wise addition of the words, so a negative 8-bit value in the low byte wraps without carry.
#[inline(always)]
pub fn addlow<const N: usize>(x: Vector<u16, N>, y: Vector<u16, N>) -> Vector<u16, N> {
    let mut r = [0u16; N];
    for k in 0..N {
        let lo = (x.0[k] as u8).wrapping_add(y.0[k] as u8) as u16;
        let hi = ((x.0[k] >> 8) as u8).wrapping_add((y.0[k] >> 8) as u8) as u16;
        r[k] = lo | (hi << 8);
    }
    Vector(r)
}

/// `alphas(c)`: each pixel's alpha in all four channels.
#[inline(always)]
pub fn alphas<T: Lane>(c: V16<T>) -> V16<T> {
    c.swizzle([3, 3, 3, 3, 7, 7, 7, 7, 11, 11, 11, 11, 15, 15, 15, 15])
}

/// `set_alphas(c, a)`: `c`'s colors with `a`'s alphas.
#[inline(always)]
pub fn set_alphas<T: Lane>(c: V16<T>, a: V16<T>) -> V16<T> {
    shuffle(c, a, [0, 1, 2, 19, 4, 5, 6, 23, 8, 9, 10, 27, 12, 13, 14, 31])
}

/// `if_then_else(V8<int16_t> c, t, e)` and its wider forms: bitwise select.
#[inline(always)]
pub fn if_then_else_w<const N: usize>(c: Vector<i16, N>, t: Vector<u16, N>, e: Vector<u16, N>) -> Vector<u16, N> {
    let c: Vector<u16, N> = c.convert::<u16>();
    (c & t) | (!c & e)
}

/// `min(HalfRGBA8, HalfRGBA8)` and its wider forms: `_mm_min_epi16` (signed) on SSE2, `vminq_u16` (unsigned) on
/// NEON and in the portable `x < y` select.
#[inline(always)]
pub fn min_w<const N: usize>(x: Vector<u16, N>, y: Vector<u16, N>) -> Vector<u16, N> {
    let mut r = [0u16; N];
    for k in 0..N {
        #[cfg(target_arch = "x86_64")]
        {
            r[k] = (x.0[k] as i16).min(y.0[k] as i16) as u16;
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            r[k] = x.0[k].min(y.0[k]);
        }
    }
    Vector(r)
}

/// `max(HalfRGBA8, HalfRGBA8)` and its wider forms (see [`min_w`]).
#[inline(always)]
pub fn max_w<const N: usize>(x: Vector<u16, N>, y: Vector<u16, N>) -> Vector<u16, N> {
    let mut r = [0u16; N];
    for k in 0..N {
        #[cfg(target_arch = "x86_64")]
        {
            r[k] = (x.0[k] as i16).max(y.0[k] as i16) as u16;
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            r[k] = x.0[k].max(y.0[k]);
        }
    }
    Vector(r)
}

#[inline(always)]
fn map4(v: V16<f32>, f: impl Fn(Float) -> Float) -> V16<f32> {
    let q = quarters(v);
    from_quarters([f(q[0]), f(q[1]), f(q[2]), f(q[3])])
}

#[inline(always)]
fn zip4(a: V16<f32>, b: V16<f32>, f: impl Fn(Float, Float) -> Float) -> V16<f32> {
    let (qa, qb) = (quarters(a), quarters(b));
    from_quarters([f(qa[0], qb[0]), f(qa[1], qb[1]), f(qa[2], qb[2]), f(qa[3], qb[3])])
}

/// `min` on `V16<float>`: glsl.h's `min(Float, Float)` on each quarter.
#[inline(always)]
fn min_f(a: V16<f32>, b: V16<f32>) -> V16<f32> {
    zip4(a, b, min)
}

/// `recip_or(v, f)`: `recip(v)` where `v != 0`, else `f`.
#[inline(always)]
pub fn recip_or(v: Float, f: f32) -> Float {
    if_then_else(v.cmp_ne(0.0f32), recip(v), Float::splat(f))
}

#[inline(always)]
fn recip_or_w(v: V16<f32>, f: f32) -> V16<f32> {
    map4(v, |x| recip_or(x, f))
}

#[inline(always)]
fn select_f(c: V16<i32>, t: V16<f32>, e: V16<f32>) -> V16<f32> {
    let c: V16<u32> = bit_cast(c);
    let t: V16<u32> = bit_cast(t);
    let e: V16<u32> = bit_cast(e);
    bit_cast((c & t) | (!c & e))
}

/// `unpremultiply(v)`: colors divided by alpha (`recip_or(a, 0)`), so 0 alpha gives transparent black.
#[inline(always)]
pub fn unpremultiply(v: WideRGBA32F) -> WideRGBA32F {
    let a = recip_or(Float::new([v.0[3], v.0[7], v.0[11], v.0[15]]), 0.0);
    v * a.swizzle([0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3])
}

/// `unpack(PackedRGBA32F)`: BGRA texels transposed to RGBA lanes.
#[inline(always)]
pub fn unpack_f32(c: PackedRGBA32F) -> vec4 {
    wide_f32_to_vec4(shuffle(c, c, [2, 6, 10, 14, 1, 5, 9, 13, 0, 4, 8, 12, 3, 7, 11, 15]))
}

#[inline(always)]
fn lumv3(v: vec3) -> Float {
    v.x * 0.30f32 + v.y * 0.59f32 + v.z * 0.11f32
}

#[inline(always)]
fn minv3(v: vec3) -> Float {
    min(min(v.x, v.y), v.z)
}

#[inline(always)]
fn maxv3(v: vec3) -> Float {
    max(max(v.x, v.y), v.z)
}

#[inline(always)]
fn v3_add_f(v: vec3, f: Float) -> vec3 {
    vec3::new(f + v.x, f + v.y, f + v.z)
}

#[inline(always)]
fn v3_sub_f(v: vec3, f: Float) -> vec3 {
    vec3::new(v.x - f, v.y - f, v.z - f)
}

#[inline(always)]
fn v3_mul_f(v: vec3, f: Float) -> vec3 {
    vec3::new(v.x * f, v.y * f, v.z * f)
}

#[inline]
fn clip_color(v: vec3, lum: Float, alpha: Float) -> vec3 {
    let mincol = max(-minv3(v), lum);
    let maxcol = max(maxv3(v), alpha - lum);
    v3_add_f(v3_mul_f(v, lum * (alpha - lum) * recip_or(mincol * maxcol, 0.0)), lum)
}

// `static inline` in blend.h.
#[inline]
fn set_lum(base: vec3, r: vec3, alpha: Float) -> vec3 {
    clip_color(v3_sub_f(base, lumv3(base)), lumv3(r), alpha)
}

// `static inline` in blend.h.
#[inline]
fn set_lum_sat(base: vec3, sref: vec3, lref: vec3, alpha: Float) -> vec3 {
    let diff = v3_sub_f(base, minv3(base));
    let sbase = maxv3(diff);
    let ssat = maxv3(sref) - minv3(sref);
    set_lum(v3_mul_f(v3_mul_f(diff, ssat), recip_or(sbase, 0.0)), lref, alpha)
}

// ---- clip masks and AA --------------------------------------------------------------------------------------

/// A span pointer: `*mut u32` (RGBA8) or `*mut u8` (R8) into a color buffer.
pub trait SpanPtr: Copy {
    /// Pixel lanes of a 4-pixel chunk, 8 bits in 16 (`WideRGBA8`, `WideR8`).
    type Wide: Copy;
    /// The chunk as stored (`PackedRGBA8`, `PackedR8`).
    type Packed: Copy + Pod;
    /// The pixel (`uint32_t`, `uint8_t`).
    type Pixel: Copy + Pod + Default;
    fn addr(self) -> usize;
    fn byte_ptr(self) -> *mut u8;
    fn add(self, n: usize) -> Self;
    fn pack(w: Self::Wide) -> Self::Packed;
    fn unpack(p: Self::Packed) -> Self::Wide;
    /// `expand_mask(buf, mask)`: one mask byte per pixel to the lanes of the pixel.
    fn expand_mask(mask: WideR8) -> Self::Wide;
    /// The unblended `commit_solid_span<false>(buf, r, len)` with `p = pack(r)`.
    ///
    /// # Safety
    /// `self` must be valid for writing `len` pixels.
    unsafe fn fill_solid(self, p: Self::Packed, len: i32);
    /// `blend_pixels(buf, dst, src, span)` for this pixel type.
    ///
    /// # Safety
    /// As [`blend_pixels_RGBA8`].
    unsafe fn blend_pixels(
        self,
        ctx: &RasterState,
        fs: &FragmentShaderState,
        pdst: Self::Packed,
        src: Self::Wide,
        span: i32,
    ) -> Self::Wide;
}

impl SpanPtr for *mut u32 {
    type Wide = WideRGBA8;
    type Packed = PackedRGBA8;
    type Pixel = u32;
    #[inline(always)]
    fn addr(self) -> usize {
        self as usize
    }
    #[inline(always)]
    fn byte_ptr(self) -> *mut u8 {
        self as *mut u8
    }
    #[inline(always)]
    fn add(self, n: usize) -> Self {
        self.wrapping_add(n)
    }
    #[inline(always)]
    fn pack(w: WideRGBA8) -> PackedRGBA8 {
        pack(w)
    }
    #[inline(always)]
    fn unpack(p: PackedRGBA8) -> WideRGBA8 {
        unpack(p)
    }
    /// `fill_n(buf, len, bit_cast<U32>(pack(r)).x)`: the first pixel everywhere.
    #[inline(always)]
    unsafe fn fill_solid(self, p: PackedRGBA8, len: i32) {
        let px = bit_cast::<U32, PackedRGBA8>(p).0[0];
        for k in 0..len.max(0) as usize {
            // SAFETY: the caller guarantees `len` writable pixels; `k < len`.
            unsafe { unaligned_store(self.add(k).byte_ptr(), px) };
        }
    }
    #[inline(always)]
    fn expand_mask(mask: WideR8) -> WideRGBA8 {
        let mask_rg: WideRG8 = zip(mask, mask);
        zip(mask_rg, mask_rg)
    }
    #[inline(always)]
    unsafe fn blend_pixels(
        self,
        ctx: &RasterState,
        fs: &FragmentShaderState,
        pdst: PackedRGBA8,
        src: WideRGBA8,
        span: i32,
    ) -> WideRGBA8 {
        // SAFETY: forwarded from the caller.
        unsafe { blend_pixels_RGBA8(ctx, fs, self, pdst, src, span) }
    }
}

impl SpanPtr for *mut u8 {
    type Wide = WideR8;
    type Packed = PackedR8;
    type Pixel = u8;
    #[inline(always)]
    fn addr(self) -> usize {
        self as usize
    }
    #[inline(always)]
    fn byte_ptr(self) -> *mut u8 {
        self
    }
    #[inline(always)]
    fn add(self, n: usize) -> Self {
        self.wrapping_add(n)
    }
    #[inline(always)]
    fn pack(w: WideR8) -> PackedR8 {
        pack(w)
    }
    #[inline(always)]
    fn unpack(p: PackedR8) -> WideR8 {
        unpack(p)
    }
    /// The four packed bytes repeated from the first 4-byte boundary, the bytes before it from lane 0.
    #[inline(always)]
    unsafe fn fill_solid(self, p: PackedR8, len: i32) {
        let mut buf = self;
        let mut len = len;
        if buf.addr() & 3 != 0 {
            let align = (4 - (buf.addr() & 3) as i32).min(len);
            // SAFETY: `align <= len` pixels from `buf`.
            unsafe { partial_store_span(buf, p, align) };
            buf = buf.add(align.max(0) as usize);
            len -= align;
        }
        let word = bit_cast::<u32, PackedR8>(p);
        for k in 0..(len / 4).max(0) as usize {
            // SAFETY: the `4 * (len / 4)` pixels after the aligned start are within the `len` pixels.
            unsafe { unaligned_store(buf.add(4 * k).byte_ptr(), word) };
        }
        buf = buf.add((len & !3).max(0) as usize);
        len &= 3;
        if len > 0 {
            // SAFETY: the last `len & 3` pixels.
            unsafe { partial_store_span(buf, p, len) };
        }
    }
    #[inline(always)]
    fn expand_mask(mask: WideR8) -> WideR8 {
        mask
    }
    #[inline(always)]
    unsafe fn blend_pixels(
        self,
        ctx: &RasterState,
        _fs: &FragmentShaderState,
        pdst: PackedR8,
        src: WideR8,
        span: i32,
    ) -> WideR8 {
        // SAFETY: forwarded from the caller.
        unsafe { blend_pixels_R8(ctx, self, unpack(pdst), src, span) }
    }
}

/// `partial_load_span<V>(src, span)`: a partial span of 1 to 3 pixels, zero beyond.
///
/// # Safety
/// `src` must be valid for reading `span` pixels (at least 1).
#[inline(always)]
pub unsafe fn partial_load_span<P: SpanPtr>(src: P, span: i32) -> P::Packed {
    let mut px = [P::Pixel::default(); 4];
    let n = span.clamp(1, 3) as usize;
    for (k, p) in px.iter_mut().enumerate().take(n) {
        // SAFETY: the caller guarantees `span` readable pixels at `src`; `k < span`.
        *p = unsafe { unaligned_load::<P::Pixel>(src.add(k).byte_ptr()) };
    }
    // The pixels as the packed chunk: four `P::Pixel`s laid out in order.
    let mut bytes = [0u8; 16];
    for (k, p) in px.iter().enumerate() {
        store_bytes(&mut bytes[k * P::Pixel::SIZE..], *p);
    }
    load_bytes(&bytes[..P::Packed::SIZE])
}

/// `partial_store_span(dst, src, span)`: store 1 to 3 pixels.
///
/// # Safety
/// `dst` must be valid for writing `span` pixels (at least 1).
#[inline(always)]
pub unsafe fn partial_store_span<P: SpanPtr>(dst: P, src: P::Packed, span: i32) {
    let mut bytes = [0u8; 16];
    store_bytes(&mut bytes[..P::Packed::SIZE], src);
    let n = if span >= 2 {
        if span > 2 {
            3
        } else {
            2
        }
    } else {
        1
    };
    for k in 0..n {
        let p: P::Pixel = load_bytes(&bytes[k * P::Pixel::SIZE..]);
        // SAFETY: the caller guarantees `span` writable pixels at `dst`; `k < span`.
        unsafe { unaligned_store(dst.add(k).byte_ptr(), p) };
    }
}

/// `load_span<V>(src, span)`: a full chunk for `span >= 4`, else a partial one.
///
/// # Safety
/// `src` must be valid for reading `min(span, 4)` pixels.
#[inline(always)]
pub unsafe fn load_span<P: SpanPtr>(src: P, span: i32) -> P::Packed {
    if span >= 4 {
        // SAFETY: the caller guarantees 4 readable pixels.
        unsafe { unaligned_load::<P::Packed>(src.byte_ptr()) }
    } else {
        // SAFETY: the caller guarantees `span` readable pixels.
        unsafe { partial_load_span(src, span) }
    }
}

/// `store_span(dst, src, span)`.
///
/// # Safety
/// `dst` must be valid for writing `min(span, 4)` pixels.
#[inline(always)]
pub unsafe fn store_span<P: SpanPtr>(dst: P, src: P::Packed, span: i32) {
    if span >= 4 {
        // SAFETY: the caller guarantees 4 writable pixels.
        unsafe { unaligned_store(dst.byte_ptr(), src) }
    } else {
        // SAFETY: the caller guarantees `span` writable pixels.
        unsafe { partial_store_span(dst, src, span) }
    }
}

/// `get_clip_mask(buf)`: the clip mask byte of the pixel at `buf`.
#[inline(always)]
pub fn get_clip_mask<P: SpanPtr>(ctx: &RasterState, buf: P) -> *mut u8 {
    let px = (buf.addr() as isize).wrapping_sub(ctx.swgl_SpanBuf as isize) / P::Pixel::SIZE as isize;
    ctx.swgl_ClipMaskBuf.wrapping_offset(px)
}

/// `load_clip_mask(buf, span)`.
///
/// # Safety
/// `swgl_ClipMaskBuf` must hold a readable mask byte for each of the `min(span, 4)` pixels from `buf`
/// (the rasterizer sets it to the span's row of the clip mask).
#[inline(always)]
pub unsafe fn load_clip_mask<P: SpanPtr>(ctx: &RasterState, buf: P, span: i32) -> P::Wide {
    // SAFETY: forwarded from the caller.
    let m: PackedR8 = unsafe { load_span(get_clip_mask(ctx, buf), span) };
    P::expand_mask(unpack(m))
}

/// `override_clip_mask()`: drop masking from the blend stage, the caller applies it.
#[inline(always)]
pub fn override_clip_mask(ctx: &mut RasterState) {
    ctx.blend_key = ctx.blend_key.wrapping_sub(MASK_BLEND_KEY_NONE);
}

/// `restore_clip_mask()`.
#[inline(always)]
pub fn restore_clip_mask(ctx: &mut RasterState) {
    ctx.blend_key = MASK_BLEND_KEY_NONE.wrapping_add(ctx.blend_key);
}

/// `get_aa_opaque_start(buf)`: pixels before the opaque part of the span.
#[inline(always)]
pub fn get_aa_opaque_start<P: SpanPtr>(ctx: &RasterState, buf: P) -> i32 {
    (((ctx.swgl_OpaqueStart as isize).wrapping_sub(buf.addr() as isize) / P::Pixel::SIZE as isize) as i32).max(0)
}

/// `get_aa_opaque_size(buf)`: pixels left in the opaque part of the span.
#[inline(always)]
pub fn get_aa_opaque_size<P: SpanPtr>(ctx: &RasterState, buf: P) -> i32 {
    let end = (ctx.swgl_OpaqueStart as isize).wrapping_add(ctx.swgl_OpaqueSize as isize);
    ((end.wrapping_sub(buf.addr() as isize) / P::Pixel::SIZE as isize) as i32).max(0)
}

/// `override_aa()`.
#[inline(always)]
pub fn override_aa(ctx: &mut RasterState) {
    ctx.blend_key = ctx.blend_key.wrapping_sub(AA_BLEND_KEY_NONE);
}

/// `restore_aa()`.
#[inline(always)]
pub fn restore_aa(ctx: &mut RasterState) {
    ctx.blend_key = AA_BLEND_KEY_NONE.wrapping_add(ctx.blend_key);
}

/// `DO_AA`: the AA coverage of the chunk at `buf` as an 8.8 weight, or `None` inside the opaque part.
#[inline(always)]
fn aa_dist(ctx: &RasterState, buf: usize) -> Option<Float> {
    let offset = (buf as isize).wrapping_sub(ctx.swgl_OpaqueStart as isize) as i32;
    if (offset as u32) >= ctx.swgl_OpaqueSize {
        let delta = ctx.swgl_AASlope * (offset as f32);
        let dist = clamp(
            min(ctx.swgl_LeftAADist + delta.0[0], ctx.swgl_RightAADist + delta.0[1]),
            Float::splat(0.0),
            Float::splat(256.0),
        );
        Some(dist)
    } else {
        None
    }
}

// ---- blend_pixels ---------------------------------------------------------------------------------------------

const RGB_MASK: WideRGBA8 = Vector([
    0xFFFF, 0xFFFF, 0xFFFF, 0, 0xFFFF, 0xFFFF, 0xFFFF, 0, 0xFFFF, 0xFFFF, 0xFFFF, 0, 0xFFFF, 0xFFFF, 0xFFFF, 0,
]);
const ALPHA_MASK: WideRGBA8 = Vector([0, 0, 0, 0xFFFF, 0, 0, 0, 0xFFFF, 0, 0, 0, 0xFFFF, 0, 0, 0, 0xFFFF]);
const ALPHA_OPAQUE: WideRGBA8 = Vector([0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]);

#[inline(always)]
fn cvt_f(v: WideRGBA8) -> WideRGBA32F {
    v.convert::<f32>()
}

/// `blend_pixels(uint32_t* buf, PackedRGBA8 pdst, WideRGBA8 src, int span)`: `src` blended over the chunk `pdst`
/// read from `buf` by the current `ctx.blend_key`. `fs` gives `gl_SecondaryFragColor` for dual-source blending.
///
/// # Safety
/// For the MASK and AA_MASK keys, `ctx.swgl_ClipMaskBuf` must hold a readable mask byte for each of the
/// `min(span, 4)` pixels from `buf` (see [`load_clip_mask`]). `buf` itself is only compared, never read.
#[inline(always)]
pub unsafe fn blend_pixels_RGBA8(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    buf: *mut u32,
    pdst: PackedRGBA8,
    src: WideRGBA8,
    span: i32,
) -> WideRGBA8 {
    let dst = unpack(pdst);
    let key = ctx.blend_key;
    let group = key / BLEND_KEY_COUNT;
    let base = key % BLEND_KEY_COUNT;
    let aa = || aa_dist(ctx, buf as usize).map(|d| pack_pixels_RGBA8(d, 1.0));
    // SAFETY (for every `load_clip_mask` below): the caller guarantees the mask bytes of the span for MASK keys,
    // and these calls are only made for the MASK (1) and AA_MASK (3) groups.
    let mask = || unsafe { load_clip_mask(ctx, buf, span) };

    if base == BLEND_ONE_ONE_MINUS_SRC1_COLOR {
        // The masked/anti-aliased secondary blend: the secondary color is weighted too.
        let secondary = muldiv255(pack_pixels_RGBA8(fs.gl_SecondaryFragColor, 255.0), dst);
        return match group {
            0 => src + dst - secondary,
            1 => {
                let m = mask();
                muldiv255(src, m) + dst - muldiv255(secondary, m)
            }
            2 => {
                let (mut src, mut secondary) = (src, secondary);
                if let Some(aa) = aa() {
                    src = muldiv256(src, aa);
                    secondary = muldiv256(secondary, aa);
                }
                src + dst - secondary
            }
            _ => {
                let mut m = mask();
                if let Some(aa) = aa() {
                    m = muldiv256(m, aa);
                }
                muldiv255(src, m) + dst - muldiv255(secondary, m)
            }
        };
    }

    let mut src = src;
    if group >= 2 {
        if let Some(aa) = aa() {
            src = muldiv256(src, aa);
        }
    }
    if group == 1 || group == 3 {
        src = muldiv255(src, mask());
    }

    match base {
        BLEND_ONE_ZERO => src,
        BLEND_SRC_ALPHA_ONE_MINUS_SRC_ALPHA_ONE_ONE_MINUS_SRC_ALPHA => {
            addlow(dst, muldiv255(alphas(src), (src | ALPHA_OPAQUE) - dst))
        }
        BLEND_ONE_ONE_MINUS_SRC_ALPHA => src + dst - muldiv255(dst, alphas(src)),
        BLEND_ZERO_ONE_MINUS_SRC_COLOR => dst - muldiv255(dst, src),
        BLEND_ZERO_ONE_MINUS_SRC_COLOR_ZERO_ONE => dst - (muldiv255(dst, src) & RGB_MASK),
        BLEND_ZERO_ONE_MINUS_SRC_ALPHA => dst - muldiv255(dst, alphas(src)),
        BLEND_ZERO_SRC_COLOR => muldiv255(src, dst),
        BLEND_ONE_ONE => src + dst,
        BLEND_ONE_ONE_ONE_ONE_MINUS_SRC_ALPHA => src + dst - (muldiv255(dst, src) & ALPHA_MASK),
        BLEND_ONE_MINUS_DST_ALPHA_ONE_ZERO_ONE => dst + ((src - muldiv255(src, alphas(dst))) & RGB_MASK),
        BLEND_CONSTANT_COLOR_ONE_MINUS_SRC_COLOR => addlow(dst, muldiv255(src, repeat2(ctx.blendcolor) - dst)),
        BLEND_MIN => min_w(src, dst),
        BLEND_MAX => max_w(src, dst),
        BLEND_MULTIPLY_KHR => {
            let diff = muldiv255(alphas(src) - (src & RGB_MASK), alphas(dst) - (dst & RGB_MASK));
            src + dst + (diff & RGB_MASK) - alphas(diff)
        }
        BLEND_SCREEN_KHR => src + dst - muldiv255(src, dst),
        BLEND_OVERLAY_KHR => {
            let src_a = alphas(src);
            let dst_a = alphas(dst);
            let diff = muldiv255(src, dst) + muldiv255(src_a - src, dst_a - dst);
            src + dst + if_then_else_w((dst * 2).cmp_le(dst_a), (diff & RGB_MASK) - alphas(diff), -diff)
        }
        BLEND_DARKEN_KHR => src + dst - max_w(muldiv255(src, alphas(dst)), muldiv255(dst, alphas(src))),
        BLEND_LIGHTEN_KHR => src + dst - min_w(muldiv255(src, alphas(dst)), muldiv255(dst, alphas(src))),
        BLEND_COLORDODGE_KHR => {
            let src_f = cvt_f(src);
            let src_a = alphas(src_f);
            let dst_f = cvt_f(dst);
            let dst_a = alphas(dst_f);
            pack_pixels_RGBA8(
                src_a * set_alphas(min_f(dst_a, dst_f * src_a * recip_or_w(src_a - src_f, 255.0)), dst_f)
                    + src_f * (255.0f32 - dst_a)
                    + dst_f * (255.0f32 - src_a),
                1.0 / 255.0,
            )
        }
        BLEND_COLORBURN_KHR => {
            let src_f = cvt_f(src);
            let src_a = alphas(src_f);
            let dst_f = cvt_f(dst);
            let dst_a = alphas(dst_f);
            pack_pixels_RGBA8(
                src_a * set_alphas(dst_a - min_f(dst_a, (dst_a - dst_f) * src_a * recip_or_w(src_f, 255.0)), dst_f)
                    + src_f * (255.0f32 - dst_a)
                    + dst_f * (255.0f32 - src_a),
                1.0 / 255.0,
            )
        }
        BLEND_HARDLIGHT_KHR => {
            let src_a = alphas(src);
            let dst_a = alphas(dst);
            let diff = muldiv255(src, dst) + muldiv255(src_a - src, dst_a - dst);
            src + dst + if_then_else_w((src * 2).cmp_le(src_a), (diff & RGB_MASK) - alphas(diff), -diff)
        }
        BLEND_SOFTLIGHT_KHR => {
            let src_f = cvt_f(src);
            let src_a = alphas(src_f);
            let dst_f = cvt_f(dst);
            let dst_a = alphas(dst_f);
            let dst_u = unpremultiply(dst_f);
            let scale = src_f + src_f - src_a;
            let curve = min_f((16.0f32 * dst_u - 12.0f32) * dst_u + 3.0f32, map4(dst_u, |x| inversesqrt(x) - 1.0f32));
            let t = select_f(scale.cmp_lt(0.0f32), 1.0f32 - dst_u, curve);
            pack_pixels_RGBA8(
                dst_f * (255.0f32 + set_alphas(scale * t, WideRGBA32F::splat(0.0))) + src_f * (255.0f32 - dst_a),
                1.0 / 255.0,
            )
        }
        BLEND_DIFFERENCE_KHR => {
            let diff = min_w(muldiv255(dst, alphas(src)), muldiv255(src, alphas(dst)));
            src + dst - diff - (diff & RGB_MASK)
        }
        BLEND_EXCLUSION_KHR => {
            let diff = muldiv255(src, dst);
            src + dst - diff - (diff & RGB_MASK)
        }
        BLEND_HSL_HUE_KHR | BLEND_HSL_SATURATION_KHR | BLEND_HSL_COLOR_KHR | BLEND_HSL_LUMINOSITY_KHR => {
            // DO_HSL: the non-separable modes in floats, transposed to vec4.
            let src_v = unpack_f32(src.convert::<f32>());
            let dst_v = unpack_f32(dst.convert::<f32>());
            let src_a = src_v.w * (1.0f32 / 255.0f32);
            let dst_a = dst_v.w * (1.0f32 / 255.0f32);
            let src_dst_a = src_v.w * dst_a;
            let src3 = vec3::new(src_v.x, src_v.y, src_v.z);
            let dst3 = vec3::new(dst_v.x, dst_v.y, dst_v.z);
            let src_c = v3_mul_f(src3, dst_a);
            let dst_c = v3_mul_f(dst3, src_a);
            let rgb = match base {
                BLEND_HSL_HUE_KHR => set_lum_sat(src_c, dst_c, dst_c, src_dst_a),
                BLEND_HSL_SATURATION_KHR => set_lum_sat(dst_c, src_c, dst_c, src_dst_a),
                BLEND_HSL_COLOR_KHR => set_lum(src_c, dst_c, src_dst_a),
                _ => set_lum(dst_c, src_c, src_dst_a),
            };
            let c = rgb + src3 - src_c + dst3 - dst_c;
            pack_pixels_RGBA8(vec4::new(c.x, c.y, c.z, src_v.w + dst_v.w - src_dst_a), 1.0)
        }
        SWGL_BLEND_DROP_SHADOW => {
            let color = muldiv255(ctx.swgl_BlendColorRGBA8, alphas(src));
            color + dst - muldiv255(dst, alphas(color))
        }
        SWGL_BLEND_SUBPIXEL_TEXT => {
            muldiv255(ctx.swgl_BlendColorRGBA8, src) + dst - muldiv255(dst, muldiv255(ctx.swgl_BlendAlphaRGBA8, src))
        }
        _ => {
            // UNREACHABLE in the C++.
            debug_assert!(false, "blend key {key}");
            src
        }
    }
}

/// `blend_pixels(uint8_t* buf, WideR8 dst, WideR8 src, int span)`: the R8 blend keys (`GL_ONE, GL_ZERO`,
/// `GL_ZERO, GL_SRC_COLOR`, `GL_ONE, GL_ONE`) in their four groups.
///
/// # Safety
/// As [`blend_pixels_RGBA8`].
#[inline(always)]
pub unsafe fn blend_pixels_R8(ctx: &RasterState, buf: *mut u8, dst: WideR8, src: WideR8, span: i32) -> WideR8 {
    let key = ctx.blend_key;
    let group = key / BLEND_KEY_COUNT;
    let base = key % BLEND_KEY_COUNT;
    let mut src = src;
    if group >= 2 {
        if let Some(dist) = aa_dist(ctx, buf as usize) {
            src = muldiv256(src, pack_pixels_R8(dist, 1.0));
        }
    }
    if group == 1 || group == 3 {
        // SAFETY: the caller guarantees the span's mask bytes for MASK keys.
        src = muldiv255(src, unsafe { load_clip_mask(ctx, buf, span) });
    }
    match base {
        BLEND_ONE_ZERO => src,
        BLEND_ZERO_SRC_COLOR => muldiv255(src, dst),
        BLEND_ONE_ONE => src + dst,
        _ => {
            debug_assert!(false, "R8 blend key {key}");
            src
        }
    }
}

// ---- commits ---------------------------------------------------------------------------------------------------

/// What `commit_span` / `blend_span` accept for a span pointer `P`: its wide or its packed chunk.
pub trait SpanValue<P: SpanPtr>: Copy {
    fn to_packed(self) -> P::Packed;
    fn to_wide(self) -> P::Wide;
    fn from_wide(w: P::Wide) -> Self;
}

impl SpanValue<*mut u32> for WideRGBA8 {
    #[inline(always)]
    fn to_packed(self) -> PackedRGBA8 {
        pack(self)
    }
    #[inline(always)]
    fn to_wide(self) -> WideRGBA8 {
        self
    }
    #[inline(always)]
    fn from_wide(w: WideRGBA8) -> Self {
        w
    }
}

impl SpanValue<*mut u32> for PackedRGBA8 {
    #[inline(always)]
    fn to_packed(self) -> PackedRGBA8 {
        self
    }
    #[inline(always)]
    fn to_wide(self) -> WideRGBA8 {
        unpack(self)
    }
    #[inline(always)]
    fn from_wide(w: WideRGBA8) -> Self {
        pack(w)
    }
}

impl SpanValue<*mut u8> for WideR8 {
    #[inline(always)]
    fn to_packed(self) -> PackedR8 {
        pack(self)
    }
    #[inline(always)]
    fn to_wide(self) -> WideR8 {
        self
    }
    #[inline(always)]
    fn from_wide(w: WideR8) -> Self {
        w
    }
}

impl SpanValue<*mut u8> for PackedR8 {
    #[inline(always)]
    fn to_packed(self) -> PackedR8 {
        self
    }
    #[inline(always)]
    fn to_wide(self) -> WideR8 {
        unpack(self)
    }
    #[inline(always)]
    fn from_wide(w: WideR8) -> Self {
        pack(w)
    }
}

/// `commit_span(buf, r)`: store a whole chunk.
///
/// # Safety
/// `buf` must be valid for writing 4 pixels.
#[inline(always)]
pub unsafe fn commit_span<P: SpanPtr, R: SpanValue<P>>(buf: P, r: R) {
    // SAFETY: forwarded from the caller.
    unsafe { unaligned_store(buf.byte_ptr(), r.to_packed()) }
}

/// `commit_span(buf, r, len)`: store 1 to 3 pixels.
///
/// # Safety
/// `buf` must be valid for writing `len` pixels.
#[inline(always)]
pub unsafe fn commit_span_len<P: SpanPtr, R: SpanValue<P>>(buf: P, r: R, len: i32) {
    // SAFETY: forwarded from the caller.
    unsafe { partial_store_span(buf, r.to_packed(), len) }
}

/// `blend_span(buf, r)`: `r` blended over the chunk at `buf`.
///
/// # Safety
/// `buf` must be valid for reading 4 pixels, and the clip mask as [`blend_pixels_RGBA8`] requires.
#[inline(always)]
pub unsafe fn blend_span<P: SpanPtr, R: SpanValue<P>>(ctx: &RasterState, fs: &FragmentShaderState, buf: P, r: R) -> R {
    // SAFETY: forwarded from the caller.
    unsafe {
        let pdst = unaligned_load::<P::Packed>(buf.byte_ptr());
        R::from_wide(buf.blend_pixels(ctx, fs, pdst, r.to_wide(), 4))
    }
}

/// `blend_span(buf, r, len)`.
///
/// # Safety
/// `buf` must be valid for reading `len` pixels, and the clip mask as [`blend_pixels_RGBA8`] requires.
#[inline(always)]
pub unsafe fn blend_span_len<P: SpanPtr, R: SpanValue<P>>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    buf: P,
    r: R,
    len: i32,
) -> R {
    // SAFETY: forwarded from the caller.
    unsafe {
        let pdst = partial_load_span(buf, len);
        R::from_wide(buf.blend_pixels(ctx, fs, pdst, r.to_wide(), len))
    }
}

/// `commit_blend_span<BLEND>(buf, r)`.
///
/// # Safety
/// `buf` must be valid for reading and writing 4 pixels, and the clip mask as [`blend_pixels_RGBA8`] requires.
#[inline(always)]
pub unsafe fn commit_blend_span<const BLEND: bool, P: SpanPtr, R: SpanValue<P>>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    buf: P,
    r: R,
) {
    // SAFETY: forwarded from the caller.
    unsafe {
        if BLEND {
            commit_span(buf, blend_span(ctx, fs, buf, r));
        } else {
            commit_span(buf, r);
        }
    }
}

/// `commit_blend_span<BLEND>(buf, r, len)`.
///
/// # Safety
/// `buf` must be valid for reading and writing `len` pixels, and the clip mask as [`blend_pixels_RGBA8`] requires.
#[inline(always)]
pub unsafe fn commit_blend_span_len<const BLEND: bool, P: SpanPtr, R: SpanValue<P>>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    buf: P,
    r: R,
    len: i32,
) {
    // SAFETY: forwarded from the caller.
    unsafe {
        if BLEND {
            commit_span_len(buf, blend_span_len(ctx, fs, buf, r, len), len);
        } else {
            commit_span_len(buf, r, len);
        }
    }
}

/// `commit_blend_solid_span(buf, r, len)`: the same chunk blended over `len` pixels.
///
/// # Safety
/// `buf` must be valid for reading and writing `len` pixels, and the clip mask as [`blend_pixels_RGBA8`] requires.
#[inline(always)]
pub unsafe fn commit_blend_solid_span<P: SpanPtr, R: SpanValue<P>>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    buf: P,
    r: R,
    len: i32,
) {
    let mut buf = buf;
    let full = (len & !3).max(0) as usize;
    let end = buf.add(full).addr();
    while buf.addr() < end {
        // SAFETY: `buf` is within the first `len & !3` pixels, 4 of which are valid from it.
        unsafe { commit_span(buf, blend_span(ctx, fs, buf, r)) };
        buf = buf.add(4);
    }
    let len = len & 3;
    if len > 0 {
        // SAFETY: the last `len & 3` pixels of the span.
        unsafe { partial_store_span(buf, blend_span_len(ctx, fs, buf, r, len).to_packed(), len) };
    }
}

/// `commit_solid_span<BLEND>(buf, r, len)`: without blending, the packed first pixel fills the span.
///
/// # Safety
/// As [`commit_blend_solid_span`].
pub unsafe fn commit_solid_span<const BLEND: bool, P: SpanPtr>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    buf: P,
    r: P::Wide,
    len: i32,
) where
    P::Wide: SpanValue<P>,
{
    if BLEND {
        // SAFETY: forwarded from the caller.
        unsafe { commit_blend_solid_span(ctx, fs, buf, r, len) };
    } else {
        // SAFETY: forwarded from the caller.
        unsafe { buf.fill_solid(r.to_packed(), len) };
    }
}
