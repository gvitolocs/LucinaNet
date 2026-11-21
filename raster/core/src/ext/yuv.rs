/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! swgl_ext.h's YUV paths: `swgl_commitTextureLinearYUV` over one, two or three planes (`blendYUV`, `sampleYUV`,
//! `blendYUVFallback`), and the `sampler2DRect` three-plane case that reuses CompositeYUV's row loop
//! (`linear_row_yuv`) when the planes allow it.
//!
//! C++ overloads `swgl_commitTextureLinearYUV(...)` by argument count; here the plane count is in the name
//! (`swgl_commitTextureLinearYUV`, `swgl_commitTextureLinearYUV2`, `swgl_commitTextureLinearYUV3`), and the
//! `swgl_commitTextureLinearColorYUV*` forms take the color last.

use crate::blend::{PackColor, BLEND_KEY_NONE};
use crate::composite::{linear_row_yuv, SpanSink, YUVMatrix};
use crate::state::RasterState;
use crate::texture::{
    textureLinearPlanarRG8, textureLinearPlanarRGBA8, textureLinearPlanarYUY2, textureLinearUnpackedR16,
    textureLinearUnpackedR8, textureLinearUnpackedRG16, unpack, PackedRGBA8, Sampler, WideRGBA8,
};
use super::{add2, clamp2, iv2, linear_quantize_uv, swgl_isTextureLinear, toi, ExtColor, ExtPx, STEP};
use crate::glsl::{
    bit_cast, ceil, force_scalar, high_half, ivec2, low_half, mat3_scalar, max, min, sampler2DRect_impl, samplerFilter,
    vec2, vec2_scalar, vec3_scalar, vec4_scalar, zip, TextureFormat, U16, V8,
};
use crate::program::FragmentShaderState;
use crate::blend::NoColor;

/// `convertYUV(rgb_from_ycbcr, y, u, v)`.
#[inline(always)]
fn convert_yuv(m: &YUVMatrix, y: U16, u: U16, v: U16) -> PackedRGBA8 {
    let yy = bit_cast::<V8<i16>, V8<u16>>(zip(y, y));
    let uv = bit_cast::<V8<i16>, V8<u16>>(zip(u, v));
    m.convert(yy, uv)
}

/// `(colorDepth - 1) - 8` for `colorDepth = 16 - rescaleFactor`: the shift from the 15 bits of
/// `textureLinearUnpackedR16` to 8.
#[inline(always)]
fn rescale_bits(rescale_factor: i32) -> i32 {
    (16 - rescale_factor - 1) - 8
}

/// `sampleYUV(sampler0, uv0, ...)`, one plane: packed RGBA8 or YUY2.
#[inline(always)]
fn sample_yuv1<S0: Sampler>(s0: &S0, uv0: ivec2, m: &YUVMatrix) -> PackedRGBA8 {
    match s0.common().format {
        TextureFormat::RGBA8 => {
            let p = textureLinearPlanarRGBA8(s0, uv0);
            convert_yuv(m, high_half(p.rg), low_half(p.rg), low_half(p.ba))
        }
        TextureFormat::YUY2 => {
            let p = textureLinearPlanarYUY2(s0, uv0);
            convert_yuv(m, p.y, p.u, p.v)
        }
        _ => {
            debug_assert!(false, "sampleYUV: plane format");
            PackedRGBA8::splat(0)
        }
    }
}

/// `sampleYUV(sampler0, uv0, sampler1, uv1, ...)`, Y and interleaved UV planes.
#[inline(always)]
fn sample_yuv2<S0: Sampler, S1: Sampler>(s0: &S0, uv0: ivec2, s1: &S1, uv1: ivec2, m: &YUVMatrix, rescale: i32) -> PackedRGBA8 {
    match s1.common().format {
        TextureFormat::RG8 => {
            debug_assert!(s0.common().format == TextureFormat::R8);
            let y = textureLinearUnpackedR8(s0, uv0);
            let p = textureLinearPlanarRG8(s1, uv1);
            convert_yuv(m, y, low_half(p.rg), high_half(p.rg))
        }
        TextureFormat::RGBA8 => {
            debug_assert!(s0.common().format == TextureFormat::R8);
            let y = textureLinearUnpackedR8(s0, uv0);
            let p = textureLinearPlanarRGBA8(s1, uv1);
            convert_yuv(m, y, low_half(p.ba), high_half(p.rg))
        }
        TextureFormat::RG16 => {
            debug_assert!(s0.common().format == TextureFormat::R16);
            let bits = rescale_bits(rescale);
            let y = textureLinearUnpackedR16(s0, uv0) >> bits;
            let uv = textureLinearUnpackedRG16(s1, uv1) >> bits;
            m.convert(zip(y, y), uv)
        }
        _ => {
            debug_assert!(false, "sampleYUV: plane format");
            PackedRGBA8::splat(0)
        }
    }
}

/// `sampleYUV(sampler0, uv0, sampler1, uv1, sampler2, uv2, ...)`, three planes of R8 or R16.
#[inline(always)]
fn sample_yuv3<S0: Sampler, S1: Sampler, S2: Sampler>(
    s0: &S0,
    uv0: ivec2,
    s1: &S1,
    uv1: ivec2,
    s2: &S2,
    uv2: ivec2,
    m: &YUVMatrix,
    rescale: i32,
) -> PackedRGBA8 {
    match s0.common().format {
        TextureFormat::R8 => convert_yuv(
            m,
            textureLinearUnpackedR8(s0, uv0),
            textureLinearUnpackedR8(s1, uv1),
            textureLinearUnpackedR8(s2, uv2),
        ),
        TextureFormat::R16 => {
            let bits = rescale_bits(rescale);
            let y = textureLinearUnpackedR16(s0, uv0) >> bits;
            let u = textureLinearUnpackedR16(s1, uv1) >> bits;
            let v = textureLinearUnpackedR16(s2, uv2) >> bits;
            convert_yuv(m, bit_cast(y), bit_cast(u), bit_cast(v))
        }
        _ => {
            debug_assert!(false, "sampleYUV: plane format");
            PackedRGBA8::splat(0)
        }
    }
}

/// A quantized plane: `(uv, uv_step, min_uv, max_uv)` from `LINEAR_QUANTIZE_UV`.
type Plane = (vec2, vec2_scalar, vec2_scalar, vec2_scalar);

#[inline(always)]
fn tap(p: &Plane) -> ivec2 {
    iv2(clamp2(p.0, p.2, p.3))
}

#[inline(always)]
fn step(p: &mut Plane) {
    p.0 = add2(p.0, p.1);
}

/// One plane: `blendYUV<BLEND>(buf, span, sampler0, uv0, uv_rect0, ycbcr_bias, rgb_from_debiased_ycbcr,
/// rescaleFactor, color)`.
///
/// # Safety
/// The span contract of the ext module doc for `span` pixels from `buf`.
unsafe fn blend_yuv1<const BLEND: bool, S0: Sampler, C: ExtColor<WideRGBA8>>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    mut buf: *mut u32,
    span: i32,
    s0: &S0,
    uv0: vec2,
    uv_rect0: &vec4_scalar,
    bias: &vec3_scalar,
    m: &mat3_scalar,
    rescale: i32,
    color: C,
) -> i32 {
    if !swgl_isTextureLinear(*s0) {
        return 0;
    }
    let mut p0 = linear_quantize_uv(s0, uv0, uv_rect0);
    let rgb_from_ycbcr = YUVMatrix::From(bias, m, rescale);
    let end = buf.wrapping_add(span.max(0) as usize);
    while buf < end {
        let r = color.apply(unpack(sample_yuv1(s0, tap(&p0), &rgb_from_ycbcr)));
        // SAFETY: a chunk starting inside the span.
        unsafe { <*mut u32 as ExtPx>::commit::<BLEND>(ctx, fs, buf, r) };
        buf = buf.wrapping_add(4);
        step(&mut p0);
    }
    span
}

/// Two planes: `blendYUV<BLEND>(buf, span, sampler0, uv0, uv_rect0, sampler1, uv1, uv_rect1, ...)`.
///
/// # Safety
/// The span contract of the ext module doc for `span` pixels from `buf`.
unsafe fn blend_yuv2<const BLEND: bool, S0: Sampler, S1: Sampler, C: ExtColor<WideRGBA8>>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    mut buf: *mut u32,
    span: i32,
    s0: &S0,
    uv0: vec2,
    uv_rect0: &vec4_scalar,
    s1: &S1,
    uv1: vec2,
    uv_rect1: &vec4_scalar,
    bias: &vec3_scalar,
    m: &mat3_scalar,
    rescale: i32,
    color: C,
) -> i32 {
    if !swgl_isTextureLinear(*s0) || !swgl_isTextureLinear(*s1) {
        return 0;
    }
    let mut p0 = linear_quantize_uv(s0, uv0, uv_rect0);
    let mut p1 = linear_quantize_uv(s1, uv1, uv_rect1);
    let rgb_from_ycbcr = YUVMatrix::From(bias, m, rescale);
    let end = buf.wrapping_add(span.max(0) as usize);
    while buf < end {
        let r = color.apply(unpack(sample_yuv2(s0, tap(&p0), s1, tap(&p1), &rgb_from_ycbcr, rescale)));
        // SAFETY: a chunk starting inside the span.
        unsafe { <*mut u32 as ExtPx>::commit::<BLEND>(ctx, fs, buf, r) };
        buf = buf.wrapping_add(4);
        step(&mut p0);
        step(&mut p1);
    }
    span
}

/// `blendYUVFallback<BLEND>`: three planes sampled per chunk.
///
/// # Safety
/// The span contract of the ext module doc for `span` pixels from `buf`.
unsafe fn blend_yuv_fallback<const BLEND: bool, S0: Sampler, S1: Sampler, S2: Sampler, C: ExtColor<WideRGBA8>>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    mut buf: *mut u32,
    span: i32,
    s0: &S0,
    mut p0: Plane,
    s1: &S1,
    mut p1: Plane,
    s2: &S2,
    mut p2: Plane,
    bias: &vec3_scalar,
    m: &mat3_scalar,
    rescale: i32,
    color: C,
) {
    let rgb_from_ycbcr = YUVMatrix::From(bias, m, rescale);
    let end = buf.wrapping_add(span.max(0) as usize);
    while buf < end {
        let px = sample_yuv3(s0, tap(&p0), s1, tap(&p1), s2, tap(&p2), &rgb_from_ycbcr, rescale);
        // SAFETY: a chunk starting inside the span.
        unsafe { <*mut u32 as ExtPx>::commit::<BLEND>(ctx, fs, buf, color.apply(unpack(px))) };
        buf = buf.wrapping_add(4);
        step(&mut p0);
        step(&mut p1);
        step(&mut p2);
    }
}

/// A `sampler2DRect` with the sampler's fields, for `linear_row_yuv`.
#[inline(always)]
fn as_rect<S: Sampler>(s: &S) -> sampler2DRect_impl {
    sampler2DRect_impl { common: *s.common(), filter: samplerFilter { filter: s.filter() } }
}

/// Three planes: `blendYUV<BLEND>(buf, span, sampler0, uv0, uv_rect0, sampler1, ..., sampler2, ...)`. With
/// `sampler2DRect` planes and no color this is the C++ specialization that hands rows inside the clamp rect to
/// CompositeYUV's `linear_row_yuv`.
///
/// # Safety
/// The span contract of the ext module doc for `span` pixels from `buf`.
unsafe fn blend_yuv3<const BLEND: bool, S0: Sampler, S1: Sampler, S2: Sampler, C: ExtColor<WideRGBA8>>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    mut buf: *mut u32,
    span: i32,
    s0: &S0,
    uv0: vec2,
    uv_rect0: &vec4_scalar,
    s1: &S1,
    uv1: vec2,
    uv_rect1: &vec4_scalar,
    s2: &S2,
    uv2: vec2,
    uv_rect2: &vec4_scalar,
    bias: &vec3_scalar,
    m: &mat3_scalar,
    rescale: i32,
    color: C,
) -> i32 {
    if !swgl_isTextureLinear(*s0) || !swgl_isTextureLinear(*s1) || !swgl_isTextureLinear(*s2) {
        return 0;
    }
    let mut p0 = linear_quantize_uv(s0, uv0, uv_rect0);
    let mut p1 = linear_quantize_uv(s1, uv1, uv_rect1);
    let mut p2 = linear_quantize_uv(s2, uv2, uv_rect2);
    let end = buf.wrapping_add(span.max(0) as usize);
    let chunks_left = |buf: *mut u32| ((end as usize - buf as usize) / 4) as i32 / STEP;
    if S0::RECT && S1::RECT && S2::RECT && C::NONE {
        let (c0, c1, c2) = (s0.common(), s1.common(), s2.common());
        // CompositeYUV wants matching formats, matching U/V sizes and coordinates, and one row for the span.
        if c0.format == c1.format
            && c1.format == c2.format
            && c1.width == c2.width
            && c1.height == c2.height
            && p0.1.y == 0.0
            && p0.1.x > 0.0
            && p1.1.y == 0.0
            && p1.1.x > 0.0
            && p1.1.x == p2.1.x
            && p1.1.y == p2.1.y
            && p1.0.x.0[0] == p2.0.x.0[0]
            && p1.0.y.0[0] == p2.0.y.0[0]
        {
            // No clamp rect in CompositeYUV: first advance until inside it.
            let outside = min(
                toi(ceil(max((p0.2.x - p0.0.x.0[0]) / p0.1.x, (p1.2.x - p1.0.x.0[0]) / p1.1.x))),
                chunks_left(buf),
            );
            if outside > 0 {
                // SAFETY: `outside` chunks fit in the span.
                unsafe {
                    blend_yuv_fallback::<BLEND, S0, S1, S2, C>(
                        ctx, fs, buf, outside * STEP, s0, p0, s1, p1, s2, p2, bias, m, rescale, color,
                    )
                };
                buf = buf.wrapping_add((outside * STEP) as usize);
                p0.0.x += crate::glsl::Float::splat(outside as f32 * p0.1.x);
                p1.0.x += crate::glsl::Float::splat(outside as f32 * p1.1.x);
                p2.0.x += crate::glsl::Float::splat(outside as f32 * p2.1.x);
            }
            // The chunks inside the clamp rect go to CompositeYUV's inner loop.
            let inside = min(
                toi(min((p0.3.x - p0.0.x.0[0]) / p0.1.x, (p1.3.x - p1.0.x.0[0]) / p1.1.x)),
                chunks_left(buf),
            );
            if inside > 0 {
                let color_depth = (if c0.format == TextureFormat::R16 { 16 } else { 8 }) - rescale;
                let rgb_from_ycbcr = YUVMatrix::From(bias, m, rescale);
                let (ry, ru, rv) = (as_rect(s0), as_rect(s1), as_rect(s2));
                // SAFETY: `inside` chunks fit in the span.
                let mut sink = unsafe { SpanSink::<BLEND>::new(ctx, fs, buf) };
                linear_row_yuv(
                    &mut sink,
                    inside * STEP,
                    &ry,
                    &force_scalar(p0.0),
                    p0.1.x / STEP as f32,
                    &ru,
                    &rv,
                    &force_scalar(p1.0),
                    p1.1.x / STEP as f32,
                    color_depth,
                    &rgb_from_ycbcr,
                );
                buf = buf.wrapping_add((inside * STEP) as usize);
                p0.0.x += crate::glsl::Float::splat(inside as f32 * p0.1.x);
                p1.0.x += crate::glsl::Float::splat(inside as f32 * p1.1.x);
                p2.0.x += crate::glsl::Float::splat(inside as f32 * p2.1.x);
            }
        }
        // Whatever is left: outside the clamp rect, or the preconditions failed.
        let left = ((end as usize - buf as usize) / 4) as i32;
        // SAFETY: the rest of the span.
        unsafe { blend_yuv_fallback::<BLEND, S0, S1, S2, C>(ctx, fs, buf, left, s0, p0, s1, p1, s2, p2, bias, m, rescale, color) };
        return span;
    }
    // SAFETY: the span.
    unsafe { blend_yuv_fallback::<BLEND, S0, S1, S2, C>(ctx, fs, buf, span, s0, p0, s1, p1, s2, p2, bias, m, rescale, color) };
    span
}

macro_rules! yuv_commit {
    ($ctx:expr, $fs:expr, $f:ident::<$($t:ty),*>($($a:expr),*)) => {{
        let buf = $fs.swgl_OutRGBA8.0;
        let len = $fs.swgl_SpanLength;
        let f: &FragmentShaderState = $fs;
        // SAFETY: the span contract of the ext module doc.
        let drawn = unsafe {
            if $ctx.blend_key != BLEND_KEY_NONE {
                $f::<true, $($t),*>($ctx, f, buf, len, $($a),*)
            } else {
                $f::<false, $($t),*>($ctx, f, buf, len, $($a),*)
            }
        };
        super::advance::<*mut u32>($fs, drawn);
    }};
}

/// `swgl_commitTextureLinearYUV(s0, p0, uv_rect0, ycbcr_bias, rgb_from_debiased_ycbcr, rescaleFactor)`: one plane
/// (RGBA8-packed or YUY2).
pub fn swgl_commitTextureLinearYUV<S0: Sampler>(
    ctx: &mut RasterState,
    s0: S0,
    p0: vec2,
    uv_rect0: vec4_scalar,
    ycbcr_bias: vec3_scalar,
    rgb_from_debiased_ycbcr: mat3_scalar,
    rescale_factor: i32,
    fs: &mut FragmentShaderState,
) {
    yuv_commit!(ctx, fs, blend_yuv1::<S0, NoColor>(&s0, p0, &uv_rect0, &ycbcr_bias, &rgb_from_debiased_ycbcr, rescale_factor, NoColor));
}

/// `swgl_commitTextureLinearYUV(s0, p0, uv_rect0, s1, p1, uv_rect1, ...)`: Y and interleaved UV planes.
pub fn swgl_commitTextureLinearYUV2<S0: Sampler, S1: Sampler>(
    ctx: &mut RasterState,
    s0: S0,
    p0: vec2,
    uv_rect0: vec4_scalar,
    s1: S1,
    p1: vec2,
    uv_rect1: vec4_scalar,
    ycbcr_bias: vec3_scalar,
    rgb_from_debiased_ycbcr: mat3_scalar,
    rescale_factor: i32,
    fs: &mut FragmentShaderState,
) {
    yuv_commit!(ctx, fs, blend_yuv2::<S0, S1, NoColor>(&s0, p0, &uv_rect0, &s1, p1, &uv_rect1, &ycbcr_bias,
        &rgb_from_debiased_ycbcr, rescale_factor, NoColor));
}

/// `swgl_commitTextureLinearYUV(s0, p0, uv_rect0, s1, p1, uv_rect1, s2, p2, uv_rect2, ...)`: Y, U and V planes.
pub fn swgl_commitTextureLinearYUV3<S0: Sampler, S1: Sampler, S2: Sampler>(
    ctx: &mut RasterState,
    s0: S0,
    p0: vec2,
    uv_rect0: vec4_scalar,
    s1: S1,
    p1: vec2,
    uv_rect1: vec4_scalar,
    s2: S2,
    p2: vec2,
    uv_rect2: vec4_scalar,
    ycbcr_bias: vec3_scalar,
    rgb_from_debiased_ycbcr: mat3_scalar,
    rescale_factor: i32,
    fs: &mut FragmentShaderState,
) {
    yuv_commit!(ctx, fs, blend_yuv3::<S0, S1, S2, NoColor>(&s0, p0, &uv_rect0, &s1, p1, &uv_rect1, &s2, p2, &uv_rect2,
        &ycbcr_bias, &rgb_from_debiased_ycbcr, rescale_factor, NoColor));
}

/// `swgl_commitTextureLinearColorYUV(s0, p0, uv_rect0, ..., color)`: one plane, scaled by `color`.
pub fn swgl_commitTextureLinearColorYUV<S0: Sampler, K: PackColor<*mut u32>>(
    ctx: &mut RasterState,
    s0: S0,
    p0: vec2,
    uv_rect0: vec4_scalar,
    ycbcr_bias: vec3_scalar,
    rgb_from_debiased_ycbcr: mat3_scalar,
    rescale_factor: i32,
    color: K,
    fs: &mut FragmentShaderState,
) where
    K::Output: ExtColor<WideRGBA8>,
{
    let c = color.pack_color();
    yuv_commit!(ctx, fs, blend_yuv1::<S0, K::Output>(&s0, p0, &uv_rect0, &ycbcr_bias, &rgb_from_debiased_ycbcr, rescale_factor, c));
}

/// `swgl_commitTextureLinearColorYUV(...)` with two planes.
pub fn swgl_commitTextureLinearColorYUV2<S0: Sampler, S1: Sampler, K: PackColor<*mut u32>>(
    ctx: &mut RasterState,
    s0: S0,
    p0: vec2,
    uv_rect0: vec4_scalar,
    s1: S1,
    p1: vec2,
    uv_rect1: vec4_scalar,
    ycbcr_bias: vec3_scalar,
    rgb_from_debiased_ycbcr: mat3_scalar,
    rescale_factor: i32,
    color: K,
    fs: &mut FragmentShaderState,
) where
    K::Output: ExtColor<WideRGBA8>,
{
    let c = color.pack_color();
    yuv_commit!(ctx, fs, blend_yuv2::<S0, S1, K::Output>(&s0, p0, &uv_rect0, &s1, p1, &uv_rect1, &ycbcr_bias,
        &rgb_from_debiased_ycbcr, rescale_factor, c));
}

/// `swgl_commitTextureLinearColorYUV(...)` with three planes.
pub fn swgl_commitTextureLinearColorYUV3<S0: Sampler, S1: Sampler, S2: Sampler, K: PackColor<*mut u32>>(
    ctx: &mut RasterState,
    s0: S0,
    p0: vec2,
    uv_rect0: vec4_scalar,
    s1: S1,
    p1: vec2,
    uv_rect1: vec4_scalar,
    s2: S2,
    p2: vec2,
    uv_rect2: vec4_scalar,
    ycbcr_bias: vec3_scalar,
    rgb_from_debiased_ycbcr: mat3_scalar,
    rescale_factor: i32,
    color: K,
    fs: &mut FragmentShaderState,
) where
    K::Output: ExtColor<WideRGBA8>,
{
    let c = color.pack_color();
    yuv_commit!(ctx, fs, blend_yuv3::<S0, S1, S2, K::Output>(&s0, p0, &uv_rect0, &s1, p1, &uv_rect1, &s2, p2, &uv_rect2,
        &ycbcr_bias, &rgb_from_debiased_ycbcr, rescale_factor, c));
}

/// The stages of `blendYUV` for R16 + RG16 planes on the first chunk, for the oracle (tests/oracle_ext.rs,
/// `texture_yuv_stages`): the quantized and clamped taps of both planes `[x0..x3, y0..y3]` each, the raw Y and UV
/// samples, the samples shifted by the colour depth, the six `YUVMatrix` vectors, and the converted pixels.
#[doc(hidden)]
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn yuv2_r16_stages<S0: Sampler, S1: Sampler>(
    s0: &S0,
    p0: vec2,
    uv_rect0: &vec4_scalar,
    s1: &S1,
    p1: vec2,
    uv_rect1: &vec4_scalar,
    bias: &vec3_scalar,
    m: &mat3_scalar,
    rescale: i32,
) -> ([i32; 16], [i16; 12], [i16; 12], [i16; 48], [u8; 16]) {
    let q0 = linear_quantize_uv(s0, p0, uv_rect0);
    let q1 = linear_quantize_uv(s1, p1, uv_rect1);
    let (i0, i1) = (tap(&q0), tap(&q1));
    let mut taps = [0i32; 16];
    taps[..4].copy_from_slice(&i0.x.0);
    taps[4..8].copy_from_slice(&i0.y.0);
    taps[8..12].copy_from_slice(&i1.x.0);
    taps[12..].copy_from_slice(&i1.y.0);
    let y = textureLinearUnpackedR16(s0, i0);
    let uv = textureLinearUnpackedRG16(s1, i1);
    let bits = rescale_bits(rescale);
    let (ys, uvs) = (y >> bits, uv >> bits);
    let cat = |a: &[i16], b: &[i16]| {
        let mut r = [0i16; 12];
        r[..4].copy_from_slice(a);
        r[4..].copy_from_slice(b);
        r
    };
    let mx = YUVMatrix::From(bias, m, rescale);
    let mut mv = [0i16; 48];
    for (k, v) in [mx.br_uvCoeffs, mx.gg_uvCoeffs, bit_cast::<V8<i16>, V8<u16>>(mx.yCoeffs), mx.yBias, mx.uvBias, mx.br_yMask]
        .iter()
        .enumerate()
    {
        mv[k * 8..k * 8 + 8].copy_from_slice(&v.0);
    }
    let px = mx.convert(zip(ys, ys), uvs);
    (taps, cat(&y.0, &uv.0), cat(&ys.0, &uvs.0), mv, px.0)
}
