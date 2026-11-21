/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Port of swgl's `swgl_ext.h`: the `swgl_*` intrinsics WebRender's shaders call under swgl, chiefly the span fast
//! paths (`swgl_commit*`) that a `swgl_drawSpanRGBA8`/`swgl_drawSpanR8` uses to fill a whole span at once.
//!
//! The C++ macros read the shader members `swgl_OutRGBA8`/`swgl_OutR8` and `swgl_SpanLength` (here
//! `&mut FragmentShaderState`, last argument) and swgl's raster statics (`blend_key`, `swgl_ClipFlags`, ...: here
//! `ctx: &mut RasterState`, first argument), as `raster/glsl-to-rust/README.md` has the emitted code call them. Each
//! macro is one function of the same name; overloads that C++ resolves by type are generic (`sampler2D` or
//! `sampler2DRect`, a color that is a `vec4_scalar`, a `float` or none). The YUV intrinsic, which C++ overloads by
//! argument count, is three functions: `swgl_commitTextureLinearYUV` (one plane), `..YUV2` and `..YUV3`.
//! `swgl_stepInterp()` and `swgl_interpStep(v)` work on the shader object and are emitted as its methods
//! (`self.step_interp_inputs(4)`, `self.interp_step.v`); `DISPATCH_DRAW_SPAN` is emitted as `draw_span_RGBA8`.
//!
//! The span: the rasterizer (rasterize.h `draw_span`) points the output at a span of the color buffer and sets
//! `swgl_SpanLength` to a multiple of 4. Like the C++, the commits write whole 4-pixel chunks, so after a partial
//! commit (`swgl_commitPartialSolid*`) has left a length that is not a multiple of 4 a following commit writes up to 3
//! pixels past the span; the span buffer must stay writable for `swgl_SpanLength` rounded up to 4 pixels, and the
//! clip mask (`swgl_ClipMaskBuf`) readable as far. Texels are read through `samplerCommon::at*`, which reads zero
//! outside the texture where the C++ reads past it.
//!
//! The index and range arithmetic (cutting a span against a row, chunk counts, clamped texels, gradient table
//! addresses) is `lucinanet_raster_rules::span`, proved by Verus.
// Lane loops index several arrays by lane number, as the C++ lane expressions do.
#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]

mod gradient;
mod yuv;

pub use gradient::*;
pub use yuv::*;

use crate::glsl::unaligned_load;
use crate::glsl::vector::f32_to_i32;
use crate::glsl::{
    abs, bit_cast, bvec4_scalar, ceil, clamp, floor, force_scalar, fract, ivec2, ivec2_scalar, ivec4_scalar, make_ivec2,
    make_ivec4, max, min, round, sampler2D_impl, samplerCommon, shuffle, vec2, vec2_scalar, vec4_scalar,
    Float, ForceScalar, TextureFilter, TextureFormat, I16, I32, V16, V4,
};
use crate::program::{swgl_StepSize, FragmentShaderState};
use lucinanet_raster_rules::span as rules;
use crate::blend::{
    alphas, blend_span, commit_blend_span, commit_blend_span_len, commit_solid_span, commit_span, get_aa_opaque_size,
    get_aa_opaque_start, get_clip_mask, override_aa, override_clip_mask, pack_pixels_R8, pack_pixels_RGBA8, pack_span,
    restore_aa, restore_clip_mask, ApplyColor, InvertColor, NoColor, PackColor, PackPixelsR8, PackPixelsRGBA8,
    SpanPtr, BLEND_KEY_NONE, SWGL_BLEND_DROP_SHADOW, SWGL_BLEND_SUBPIXEL_TEXT,
};
use crate::state::{RasterState, SWGL_CLIP_FLAG_AA, SWGL_CLIP_FLAG_BLEND_OVERRIDE, SWGL_CLIP_FLAG_MASK};
use crate::texture::{
    computeFracNoClamp, computeFracX, computeFracY, computeRow_scalar, gaussianBlurHorizontal_R8,
    gaussianBlurHorizontal_RGBA8, gaussianBlurVertical_R8, gaussianBlurVertical_RGBA8, linearQuantize_s,
    linearQuantize_scalar_s, samplerScale, samplerScale_scalar, textureLinearUnpackedR8, textureLinearUnpackedRGBA8,
    textureNearestPackedRGBA8, unpack, IntRect, PackedR8, PackedRGBA8, Sampler, WideR8, WideRGBA8,
};

const STEP: i32 = swgl_StepSize;

/// C++ `int(f)`: truncation, with x86's `0x80000000` for values out of range (glsl.h scalar conversions).
#[inline(always)]
fn toi(f: f32) -> i32 {
    f32_to_i32(f)
}

#[inline(always)]
fn v2(x: Float, y: Float) -> vec2 {
    vec2::new(x, y)
}

#[inline(always)]
fn v2s(x: f32, y: f32) -> vec2_scalar {
    vec2_scalar::new(x, y)
}

/// `vec2 += vec2_scalar` and `vec2 * vec2_scalar + vec2_scalar`, lane by lane as glsl.h.
#[inline(always)]
fn add2(a: vec2, b: vec2_scalar) -> vec2 {
    v2(a.x + b.x, a.y + b.y)
}
#[inline(always)]
fn mad2(a: vec2, s: vec2_scalar, o: vec2_scalar) -> vec2 {
    v2(a.x * s.x + o.x, a.y * s.y + o.y)
}
/// `clamp(vec2, vec2_scalar, vec2_scalar)`.
#[inline(always)]
fn clamp2(a: vec2, lo: vec2_scalar, hi: vec2_scalar) -> vec2 {
    v2(clamp(a.x, Float::splat(lo.x), Float::splat(hi.x)), clamp(a.y, Float::splat(lo.y), Float::splat(hi.y)))
}
/// `ivec2(vec2)`: truncating lane conversion.
#[inline(always)]
fn iv2(a: vec2) -> ivec2 {
    make_ivec2(a)
}

// ---- span pixel types -------------------------------------------------------------------------------------------

/// The span pixel types of the C++ templates (`uint32_t*` for RGBA8, `uint8_t*` for R8), with what swgl_ext.h
/// selects by overloading on them.
pub trait ExtPx: SpanPtr + 'static {
    /// Channels per pixel (4 or 1).
    const C: usize;
    fn out(fs: &FragmentShaderState) -> Self;
    fn set_out(fs: &mut FragmentShaderState, p: Self);
    /// `matchTextureFormat(s, buf)`.
    fn match_format(f: TextureFormat) -> bool;
    /// `textureLinearUnpacked(buf, sampler, i)`.
    fn linear_unpacked<S: Sampler>(s: &S, i: ivec2) -> Self::Wide;
    /// `gaussianBlurHorizontal<P>` / `gaussianBlurVertical<P>`. `unchecked`: the caller proved every texel the
    /// blur reads lies inside the sampler's bytes, so its loads skip the bounds check.
    fn blur<S: Sampler>(
        s: &S,
        hori: bool,
        i: &ivec2_scalar,
        lo: i32,
        hi: i32,
        radius: i32,
        c0: f32,
        c1: f32,
        unchecked: bool,
    ) -> Self::Wide;
    /// `unaligned_load<packed_type>(&row[idx])` from texel `idx` of the sampler: 4 texels, unpacked.
    fn tex_chunk(c: &samplerCommon, idx: i32) -> Self::Wide;
    /// [`ExtPx::tex_chunk`] without its bounds check.
    ///
    /// # Safety
    /// `idx >= 0` and the 4 texels from `idx` lie inside the sampler's `len` bytes.
    unsafe fn tex_chunk_unchecked(c: &samplerCommon, idx: i32) -> Self::Wide;
    /// Bytes of one texel.
    const TEXEL_BYTES: usize;
    /// `unpack(bit_cast<packed_type>(V4<P>(row[idx])))`: texel `idx` in all 4 pixels.
    fn tex_texel(c: &samplerCommon, idx: i32) -> Self::Wide;
    /// `partial_load_span<packed_type>(&row[idx], n)`: `n` texels, zero beyond, unpacked.
    fn tex_partial(c: &samplerCommon, idx: i32, n: i32) -> Self::Wide;
    /// `CONVERT(unaligned_load<packed_type>(&row[idx]), signed_unpacked_type)`, lanes `0..4 * C` used.
    fn tex_signed(c: &samplerCommon, idx: i32) -> V16<i16>;
    /// [`ExtPx::tex_signed`] without its bounds check.
    ///
    /// # Safety
    /// `idx >= 0` and the 4 texels from `idx` lie inside the sampler's `len` bytes.
    unsafe fn tex_signed_unchecked(c: &samplerCommon, idx: i32) -> V16<i16>;
    /// `bit_cast<unpacked_type>(v)`.
    fn wide_from_signed(v: V16<i16>) -> Self::Wide;
    /// `expand_mask(buf, mask)`.
    fn expand(mask: WideR8) -> Self::Wide;
    /// `commit_blend_span<BLEND>(buf, r)`.
    ///
    /// # Safety
    /// `buf` valid for reading and writing 4 pixels, and the clip mask as `blend::blend_pixels_*` requires.
    unsafe fn commit<const BLEND: bool>(ctx: &RasterState, fs: &FragmentShaderState, buf: Self, r: Self::Wide);
    /// `commit_blend_span<BLEND>(buf, r, len)`.
    ///
    /// # Safety
    /// As [`ExtPx::commit`], for `len` pixels.
    unsafe fn commit_len<const BLEND: bool>(
        ctx: &RasterState,
        fs: &FragmentShaderState,
        buf: Self,
        r: Self::Wide,
        len: i32,
    );
    /// `commit_solid_span<BLEND>(buf, r, len)`.
    ///
    /// # Safety
    /// As [`ExtPx::commit`], for `len` pixels.
    unsafe fn commit_solid<const BLEND: bool>(
        ctx: &RasterState,
        fs: &FragmentShaderState,
        buf: Self,
        r: Self::Wide,
        len: i32,
    );
    /// `commit_span(buf, blend_span(buf, r))`, for a commit that blends whatever the current key.
    ///
    /// # Safety
    /// As [`ExtPx::commit`].
    unsafe fn commit_blended(ctx: &RasterState, fs: &FragmentShaderState, buf: Self, r: Self::Wide);
    /// `commit_span(buf, r)`.
    ///
    /// # Safety
    /// `buf` valid for writing 4 pixels.
    unsafe fn store(buf: Self, r: Self::Wide);
}

macro_rules! ext_px_common {
    ($p:ty, $wide:ty, $packed:ty, $bytes:literal) => {
        #[inline(always)]
        fn tex_chunk(c: &samplerCommon, idx: i32) -> $wide {
            unpack(c.at::<$packed, $bytes>(idx))
        }
        const TEXEL_BYTES: usize = $bytes;
        #[inline(always)]
        unsafe fn tex_chunk_unchecked(c: &samplerCommon, idx: i32) -> $wide {
            // SAFETY: the caller's contract: the chunk is inside the `len` readable bytes at `buf`.
            unpack(unsafe { unaligned_load::<$packed>((c.buf.0 as *const u8).add(idx as usize * $bytes)) })
        }
        #[inline(always)]
        unsafe fn commit<const BLEND: bool>(ctx: &RasterState, fs: &FragmentShaderState, buf: $p, r: $wide) {
            // SAFETY: forwarded from the caller.
            unsafe { commit_blend_span::<BLEND, $p, $wide>(ctx, fs, buf, r) }
        }
        #[inline(always)]
        unsafe fn commit_len<const BLEND: bool>(
            ctx: &RasterState,
            fs: &FragmentShaderState,
            buf: $p,
            r: $wide,
            len: i32,
        ) {
            // SAFETY: forwarded from the caller.
            unsafe { commit_blend_span_len::<BLEND, $p, $wide>(ctx, fs, buf, r, len) }
        }
        #[inline(always)]
        unsafe fn commit_solid<const BLEND: bool>(
            ctx: &RasterState,
            fs: &FragmentShaderState,
            buf: $p,
            r: $wide,
            len: i32,
        ) {
            // SAFETY: forwarded from the caller.
            unsafe { commit_solid_span::<BLEND, $p>(ctx, fs, buf, r, len) }
        }
        #[inline(always)]
        unsafe fn commit_blended(ctx: &RasterState, fs: &FragmentShaderState, buf: $p, r: $wide) {
            // SAFETY: forwarded from the caller.
            unsafe { commit_span(buf, blend_span(ctx, fs, buf, r)) }
        }
        #[inline(always)]
        unsafe fn store(buf: $p, r: $wide) {
            // SAFETY: forwarded from the caller.
            unsafe { commit_span(buf, r) }
        }
    };
}

impl ExtPx for *mut u32 {
    const C: usize = 4;
    #[inline(always)]
    fn out(fs: &FragmentShaderState) -> Self {
        fs.swgl_OutRGBA8.0
    }
    #[inline(always)]
    fn set_out(fs: &mut FragmentShaderState, p: Self) {
        fs.swgl_OutRGBA8.0 = p;
    }
    #[inline(always)]
    fn match_format(f: TextureFormat) -> bool {
        f == TextureFormat::RGBA8
    }
    #[inline(always)]
    fn linear_unpacked<S: Sampler>(s: &S, i: ivec2) -> WideRGBA8 {
        textureLinearUnpackedRGBA8(s, i)
    }
    #[inline(always)]
    fn blur<S: Sampler>(
        s: &S,
        hori: bool,
        i: &ivec2_scalar,
        lo: i32,
        hi: i32,
        r: i32,
        c0: f32,
        c1: f32,
        unchecked: bool,
    ) -> WideRGBA8 {
        if hori {
            if unchecked {
                gaussianBlurHorizontal_RGBA8::<S, true>(s, i, lo, hi, r, c0, c1)
            } else {
                gaussianBlurHorizontal_RGBA8::<S, false>(s, i, lo, hi, r, c0, c1)
            }
        } else if unchecked {
            gaussianBlurVertical_RGBA8::<S, true>(s, i, lo, hi, r, c0, c1)
        } else {
            gaussianBlurVertical_RGBA8::<S, false>(s, i, lo, hi, r, c0, c1)
        }
    }
    #[inline(always)]
    fn tex_texel(c: &samplerCommon, idx: i32) -> WideRGBA8 {
        let t = c.at32::<u32>(idx);
        unpack(bit_cast::<PackedRGBA8, V4<u32>>(V4::splat(t)))
    }
    #[inline(always)]
    fn tex_partial(c: &samplerCommon, idx: i32, n: i32) -> WideRGBA8 {
        let mut t = [0u32; 4];
        for k in 0..n.clamp(0, 4) as usize {
            t[k] = c.at32::<u32>(idx.wrapping_add(k as i32));
        }
        unpack(bit_cast::<PackedRGBA8, V4<u32>>(V4::new(t)))
    }
    #[inline(always)]
    fn tex_signed(c: &samplerCommon, idx: i32) -> V16<i16> {
        // `CONVERT` of the 16 packed bytes to signed lanes: `punpcklbw`/`punpckhbw` on SSE2, `ushll` on NEON,
        // kept in one register as a `V16` (an `[i16; 16]` return spilled through the stack).
        c.at32::<PackedRGBA8>(idx).convert::<i16>()
    }
    #[inline(always)]
    unsafe fn tex_signed_unchecked(c: &samplerCommon, idx: i32) -> V16<i16> {
        debug_assert!(idx >= 0 && idx as usize * 4 + 16 <= c.len);
        // SAFETY: the caller's contract: `idx >= 0` and the 4 texels from `idx` lie inside the `len` readable
        // bytes at `buf`.
        unsafe { unaligned_load::<PackedRGBA8>((c.buf.0 as *const u8).add(idx as usize * 4)) }.convert::<i16>()
    }
    #[inline(always)]
    fn wide_from_signed(v: V16<i16>) -> WideRGBA8 {
        v.convert::<u16>()
    }
    #[inline(always)]
    fn expand(mask: WideR8) -> WideRGBA8 {
        <*mut u32 as SpanPtr>::expand_mask(mask)
    }
    ext_px_common!(*mut u32, WideRGBA8, PackedRGBA8, 4);
}

impl ExtPx for *mut u8 {
    const C: usize = 1;
    #[inline(always)]
    fn out(fs: &FragmentShaderState) -> Self {
        fs.swgl_OutR8.0
    }
    #[inline(always)]
    fn set_out(fs: &mut FragmentShaderState, p: Self) {
        fs.swgl_OutR8.0 = p;
    }
    #[inline(always)]
    fn match_format(f: TextureFormat) -> bool {
        f == TextureFormat::R8
    }
    #[inline(always)]
    fn linear_unpacked<S: Sampler>(s: &S, i: ivec2) -> WideR8 {
        textureLinearUnpackedR8(s, i)
    }
    #[inline(always)]
    fn blur<S: Sampler>(
        s: &S,
        hori: bool,
        i: &ivec2_scalar,
        lo: i32,
        hi: i32,
        r: i32,
        c0: f32,
        c1: f32,
        unchecked: bool,
    ) -> WideR8 {
        if hori {
            if unchecked {
                gaussianBlurHorizontal_R8::<S, true>(s, i, lo, hi, r, c0, c1)
            } else {
                gaussianBlurHorizontal_R8::<S, false>(s, i, lo, hi, r, c0, c1)
            }
        } else if unchecked {
            gaussianBlurVertical_R8::<S, true>(s, i, lo, hi, r, c0, c1)
        } else {
            gaussianBlurVertical_R8::<S, false>(s, i, lo, hi, r, c0, c1)
        }
    }
    #[inline(always)]
    fn tex_texel(c: &samplerCommon, idx: i32) -> WideR8 {
        unpack(PackedR8::splat(c.at8::<u8>(idx)))
    }
    #[inline(always)]
    fn tex_partial(c: &samplerCommon, idx: i32, n: i32) -> WideR8 {
        let mut t = [0u8; 4];
        for k in 0..n.clamp(0, 4) as usize {
            t[k] = c.at8::<u8>(idx.wrapping_add(k as i32));
        }
        unpack(PackedR8::new(t))
    }
    #[inline(always)]
    fn tex_signed(c: &samplerCommon, idx: i32) -> V16<i16> {
        let p = c.at8::<PackedR8>(idx).convert::<i16>();
        V16::new([p.0[0], p.0[1], p.0[2], p.0[3], 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
    }
    #[inline(always)]
    unsafe fn tex_signed_unchecked(c: &samplerCommon, idx: i32) -> V16<i16> {
        debug_assert!(idx >= 0 && idx as usize + 4 <= c.len);
        // SAFETY: the caller's contract: `idx >= 0` and the 4 texels from `idx` lie inside the `len` readable
        // bytes at `buf`.
        let p = unsafe { unaligned_load::<PackedR8>((c.buf.0 as *const u8).add(idx as usize)) }.convert::<i16>();
        V16::new([p.0[0], p.0[1], p.0[2], p.0[3], 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
    }
    #[inline(always)]
    fn wide_from_signed(v: V16<i16>) -> WideR8 {
        V4::new([v.0[0] as u16, v.0[1] as u16, v.0[2] as u16, v.0[3] as u16])
    }
    #[inline(always)]
    fn expand(mask: WideR8) -> WideR8 {
        mask
    }
    ext_px_common!(*mut u8, WideR8, PackedR8, 1);
}

/// A color modulus after `packColor`: `NoColor`, `InvertColor` or packed lanes (`applyColor`).
pub trait ExtColor<W>: Copy {
    /// Whether this is `NoColor` (the template specializations that take `NoColor` only).
    const NONE: bool = false;
    fn apply(self, src: W) -> W;
}
impl<W> ExtColor<W> for NoColor
where
    NoColor: ApplyColor<W>,
{
    const NONE: bool = true;
    #[inline(always)]
    fn apply(self, src: W) -> W {
        self.apply_color(src)
    }
}
impl<W> ExtColor<W> for InvertColor
where
    InvertColor: ApplyColor<W>,
{
    #[inline(always)]
    fn apply(self, src: W) -> W {
        self.apply_color(src)
    }
}
impl<const N: usize> ExtColor<crate::glsl::Vector<u16, N>> for crate::glsl::Vector<u16, N> {
    #[inline(always)]
    fn apply(self, src: crate::glsl::Vector<u16, N>) -> crate::glsl::Vector<u16, N> {
        self.apply_color(src)
    }
}

/// Advance the span output by `n` pixels: `swgl_Out##format += n; swgl_SpanLength -= n`.
#[inline(always)]
fn advance<P: ExtPx>(fs: &mut FragmentShaderState, n: i32) {
    let p = P::out(fs);
    P::set_out(fs, p.add(n as isize as usize));
    fs.swgl_SpanLength -= n;
}

// ---- solid spans ------------------------------------------------------------------------------------------------

/// `commit_masked_solid_span(buf, color, len)`: the clip mask applied to the color before the blend stage.
///
/// # Safety
/// The span contract of the module doc.
unsafe fn commit_masked_solid_span<P: ExtPx>(
    ctx: &mut RasterState,
    fs: &FragmentShaderState,
    buf: P,
    color: P::Wide,
    len: i32,
) where
    P::Wide: ExtColor<P::Wide>,
{
    override_clip_mask(ctx);
    let mut mask = get_clip_mask(&*ctx, buf);
    let mut buf = buf;
    let end = buf.add(len.max(0) as usize).addr();
    while buf.addr() < end {
        // SAFETY: the clip mask holds a byte for each pixel of the span, rounded up to 4 (module doc).
        let m: PackedR8 = unsafe { crate::glsl::vector::unaligned_load(mask) };
        let src = color.apply(P::expand(unpack(m)));
        // SAFETY: within the span rounded up to 4 pixels.
        unsafe { P::commit_blended(ctx, fs, buf, src) };
        buf = buf.add(4);
        mask = mask.wrapping_add(4);
    }
    restore_clip_mask(ctx);
}

/// `commit_aa_solid_span(buf, r, len)`: AA blending only outside the opaque part of the span.
///
/// # Safety
/// The span contract of the module doc.
unsafe fn commit_aa_solid_span<P: ExtPx>(ctx: &mut RasterState, fs: &FragmentShaderState, buf: P, r: P::Wide, len: i32) {
    let mut buf = buf;
    let mut len = len as i64;
    let n = (get_aa_opaque_start(&*ctx, buf) as i64).min(rules::MAX_SPAN);
    let start = rules::chunk_ceil(n, len);
    if start != 0 {
        // SAFETY: `start <= len` pixels of the span.
        unsafe { P::commit_solid::<true>(ctx, fs, buf, r, start as i32) };
        buf = buf.add(start as usize);
        len -= start;
    }
    let n = (get_aa_opaque_size(&*ctx, buf) as i64).min(rules::MAX_SPAN);
    let opaque = rules::chunk_ceil(n, len);
    if opaque != 0 {
        override_aa(ctx);
        // SAFETY: `opaque <= len` pixels of what is left of the span.
        unsafe { P::commit_solid::<true>(ctx, fs, buf, r, opaque as i32) };
        restore_aa(ctx);
        buf = buf.add(opaque as usize);
        len -= opaque;
    }
    if len > 0 {
        // SAFETY: the rest of the span.
        unsafe { P::commit_solid::<true>(ctx, fs, buf, r, len as i32) };
    }
}

/// `swgl_commitSolid(format, v, n)`.
fn commit_solid<P: ExtPx>(ctx: &mut RasterState, fs: &mut FragmentShaderState, v: P::Wide, len: i32)
where
    P::Wide: ExtColor<P::Wide>,
{
    let buf = P::out(fs);
    // SAFETY: the span contract of the module doc; `len <= swgl_SpanLength`.
    unsafe {
        if ctx.blend_key != BLEND_KEY_NONE {
            if ctx.swgl_ClipFlags & SWGL_CLIP_FLAG_MASK != 0 {
                commit_masked_solid_span(ctx, fs, buf, v, len);
            } else if ctx.swgl_ClipFlags & SWGL_CLIP_FLAG_AA != 0 {
                commit_aa_solid_span(ctx, fs, buf, v, len);
            } else {
                P::commit_solid::<true>(ctx, fs, buf, v, len);
            }
        } else {
            P::commit_solid::<false>(ctx, fs, buf, v, len);
        }
    }
    advance::<P>(fs, len);
}

/// `swgl_commitSolidRGBA8(v)`: the whole span in one color.
pub fn swgl_commitSolidRGBA8<V: PackPixelsRGBA8>(ctx: &mut RasterState, v: V, fs: &mut FragmentShaderState) {
    let len = fs.swgl_SpanLength;
    commit_solid::<*mut u32>(ctx, fs, pack_pixels_RGBA8(v, 255.0), len);
}

/// `swgl_commitSolidR8(v)`: `v` a `float` (made a `Float`, as C++'s `pack_pixels_R8(Float c)` takes it) or `Float`.
pub fn swgl_commitSolidR8<V: Into<Float>>(ctx: &mut RasterState, v: V, fs: &mut FragmentShaderState) {
    let len = fs.swgl_SpanLength;
    commit_solid::<*mut u8>(ctx, fs, pack_pixels_R8(v.into(), 255.0), len);
}

/// `swgl_commitPartialSolidRGBA8(len, v)`: the first `min(len, swgl_SpanLength)` pixels.
pub fn swgl_commitPartialSolidRGBA8<V: PackPixelsRGBA8>(
    ctx: &mut RasterState,
    len: i32,
    v: V,
    fs: &mut FragmentShaderState,
) {
    let n = rules::partial_len(len as i64, fs.swgl_SpanLength as i64) as i32;
    commit_solid::<*mut u32>(ctx, fs, pack_pixels_RGBA8(v, 255.0), n);
}

/// `swgl_commitPartialSolidR8(len, v)`.
pub fn swgl_commitPartialSolidR8<V: Into<Float>>(ctx: &mut RasterState, len: i32, v: V, fs: &mut FragmentShaderState) {
    let n = rules::partial_len(len as i64, fs.swgl_SpanLength as i64) as i32;
    commit_solid::<*mut u8>(ctx, fs, pack_pixels_R8(v.into(), 255.0), n);
}

/// `swgl_commitChunk(format, chunk)`: one chunk, blended if a blend key is set.
fn commit_chunk<P: ExtPx>(ctx: &RasterState, fs: &mut FragmentShaderState, r: P::Wide) {
    let buf = P::out(fs);
    // SAFETY: the span contract of the module doc (a chunk is 4 pixels).
    unsafe {
        if ctx.blend_key != BLEND_KEY_NONE {
            P::commit_blended(ctx, fs, buf, r);
        } else {
            P::store(buf, r);
        }
    }
    advance::<P>(fs, STEP);
}

/// `swgl_commitColorRGBA8(color)`: one chunk of shader colors.
pub fn swgl_commitColorRGBA8<V: PackPixelsRGBA8>(ctx: &mut RasterState, color: V, fs: &mut FragmentShaderState) {
    commit_chunk::<*mut u32>(ctx, fs, pack_pixels_RGBA8(color, 255.0));
}

/// `swgl_commitColorR8(color)`.
pub fn swgl_commitColorR8<V: PackPixelsR8>(ctx: &mut RasterState, color: V, fs: &mut FragmentShaderState) {
    commit_chunk::<*mut u8>(ctx, fs, pack_pixels_R8(color, 255.0));
}

/// `swgl_forceScalar(v)`.
#[inline(always)]
pub fn swgl_forceScalar<T: ForceScalar>(v: T) -> T::Output {
    force_scalar(v)
}

/// `swgl_isTextureLinear(s)`.
#[inline(always)]
pub fn swgl_isTextureLinear<S: Sampler>(s: S) -> bool {
    s.filter() == TextureFilter::LINEAR
}

/// `swgl_isTextureRGBA8(s)`.
#[inline(always)]
pub fn swgl_isTextureRGBA8<S: Sampler>(s: S) -> bool {
    s.common().format == TextureFormat::RGBA8
}

/// `swgl_isTextureR8(s)`.
#[inline(always)]
pub fn swgl_isTextureR8<S: Sampler>(s: S) -> bool {
    s.common().format == TextureFormat::R8
}

// ---- linear filtering -------------------------------------------------------------------------------------------

/// `swgl_LinearQuantizeScale`: 7 bits of fraction.
pub const swgl_LinearQuantizeScale: i32 = 128;
const LQS: f32 = swgl_LinearQuantizeScale as f32;

/// `LINEAR_QUANTIZE_UV(sampler, uv, uv_step, uv_rect, min_uv, max_uv)`: `(uv, uv_step, min_uv, max_uv)`.
#[inline(always)]
fn linear_quantize_uv<S: Sampler>(s: &S, uv: vec2, uv_rect: &vec4_scalar) -> (vec2, vec2_scalar, vec2_scalar, vec2_scalar) {
    let uv = linearQuantize_s(uv, LQS, s);
    let uv_step = (STEP as f32) * v2s(uv.x.0[1] - uv.x.0[0], uv.y.0[1] - uv.y.0[0]);
    let min_uv = max(linearQuantize_scalar_s(v2s(uv_rect.x, uv_rect.y), LQS, s), 0.0f32);
    let max_uv = max(linearQuantize_scalar_s(v2s(uv_rect.z, uv_rect.w), LQS, s), min_uv);
    (uv, uv_step, min_uv, max_uv)
}

/// `blendTextureLinearFallback<BLEND>`: any scale, clamped per sample. Returns the buffer past the span.
///
/// # Safety
/// The span contract of the module doc for `span` pixels from `buf`.
unsafe fn blend_texture_linear_fallback<const BLEND: bool, S: Sampler, C: ExtColor<P::Wide>, P: ExtPx>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    mut uv: vec2,
    span: i32,
    uv_step: vec2_scalar,
    min_uv: vec2_scalar,
    max_uv: vec2_scalar,
    color: C,
    mut buf: P,
) -> P {
    let end = buf.add(span.max(0) as usize).addr();
    while buf.addr() < end {
        let r = color.apply(P::linear_unpacked(s, iv2(clamp2(uv, min_uv, max_uv))));
        // SAFETY: a chunk starting inside the span (module doc).
        unsafe { P::commit::<BLEND>(ctx, fs, buf, r) };
        buf = buf.add(4);
        uv = add2(uv, uv_step);
    }
    buf
}

/// `src0 + (((src1 - src0) * f) >> 7)` on the signed unpacked lanes of a chunk, with one fraction per lane.
#[inline(always)]
fn lerp_lanes(a: V16<i16>, b: V16<i16>, f: V16<i16>) -> V16<i16> {
    // V16<i16>'s + - * >> are SSE2/NEON instructions (paddw/psubw/pmullw/psraw, add/sub/mul/sshl .8h): the same
    // wrapping lanes, where an element loop was left scalar.
    a + (((b - a) * f) >> 7)
}

/// A chunk's lanes as 4 pixels of `C` channels: pixel `p` from `src` pixel `idx[p]` (`SHUFFLE` on the
/// `castForShuffle` form, where one lane is one pixel).
#[inline(always)]
fn shuffle_px<const C: usize>(a: V16<i16>, b: V16<i16>, idx: [usize; 4]) -> V16<i16> {
    let mut lanes = [0i32; 16];
    for p in 0..4 {
        for ch in 0..C {
            // `shuffle` maps lanes `0..16` to `a` and `16..` to `b`, while a chunk holds its 4 pixels of `C`
            // channels in lanes `0..4*C` of each: pixel `idx[p] >= 4` of `b` is lane `16 + (idx[p] - 4) * C + ch`.
            let q = idx[p] * C + ch;
            lanes[p * C + ch] = (if q < 4 * C { q } else { q + 16 - 4 * C }) as i32;
        }
    }
    shuffle(a, b, lanes)
}

/// A fraction per pixel spread over its `C` channels (`fracx.xxxxyyyyzzzzwwww` for RGBA8).
#[inline(always)]
fn spread<const C: usize>(f: [i16; 4]) -> V16<i16> {
    let mut r = [0i16; 16];
    for p in 0..4 {
        for ch in 0..C {
            r[p * C + ch] = f[p];
        }
    }
    V16::new(r)
}

/// `computeNextRowOffset(sampler, ivec2_scalar)`.
#[inline(always)]
fn next_row_offset_scalar(c: &samplerCommon, y: i32) -> i32 {
    if y >= 0 && y < (c.height as i32).wrapping_sub(1) {
        c.stride as i32
    } else {
        0
    }
}

/// The start of the fast and downscale linear filters (`blendTextureLinearFast`/`...Downscale`): the clamped
/// texel `i`, its fraction, and the two rows `row0`/`row1` the loop reads (`computeRow` plus
/// `computeNextRowOffset`). `blend_texture_linear_dispatch` computes the same rows to check
/// `rules::linear_reads_in_bounds` on, so the check and the loads cannot drift apart.
#[inline(always)]
fn linear_filter_start(c: &samplerCommon, uv: vec2, min_uv: vec2_scalar, max_uv: vec2_scalar) -> (ivec2, ivec2, i32, i32) {
    let i0 = iv2(clamp2(uv, min_uv, max_uv));
    let frac = i0;
    let i = ivec2::new(i0.x >> 7, i0.y >> 7);
    let fi = force_scalar(i);
    let row0 = computeRow_scalar(c, fi, 1);
    let row1 = row0.wrapping_add(next_row_offset_scalar(c, fi.y));
    (i, frac, row0, row1)
}

/// `blendTextureLinearUpscale<BLEND>`: constant Y, scale below 1; samples ahead one chunk.
///
/// # Safety
/// The span contract of the module doc for `span` pixels from `buf`.
unsafe fn blend_texture_linear_upscale<const BLEND: bool, const CH: usize, S: Sampler, C: ExtColor<P::Wide>, P: ExtPx>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    mut uv: vec2,
    span: i32,
    uv_step: vec2_scalar,
    min_uv: vec2_scalar,
    max_uv: vec2_scalar,
    color: C,
    mut buf: P,
) {
    let c = s.common();
    let mut i = iv2(clamp2(uv, min_uv, max_uv));
    let frac = i;
    i = ivec2::new(i.x >> 7, i.y >> 7);
    let row0 = computeRow_scalar(c, ivec2_scalar::new(0, i.y.0[0]), 1);
    let row1 = row0.wrapping_add(next_row_offset_scalar(c, i.y.0[0]));
    let mut fracx: I16 = computeFracX(c, i, frac);
    let fracy = computeFracY(frac).0[0];
    let fy = V16::splat(fracy);
    // Checked loads: swgl reads these taps unchecked, relying on the dispatch's `inside`, but `inside` does not
    // bound a span that starts left of `min_uv` (review 2026-11-07), so the port keeps the bounds check.
    let at = |r: i32, x: i32| P::tex_signed(c, r.wrapping_add(x));
    let mut src = lerp_lanes(at(row0, i.x.0[0]), at(row1, i.x.0[0]), fy);

    let end = buf.add(span.max(0) as usize).addr();
    while buf.addr() < end {
        uv.x += Float::splat(uv_step.x);
        let mut ixn: I32 = crate::glsl::cast(uv.x);
        let fracn = computeFracNoClamp(ixn);
        ixn >>= 7;
        let srcn = lerp_lanes(at(row0, ixn.0[0]), at(row1, ixn.0[0]), fy);

        let mut shuf = src;
        let next = if ixn.0[0] == i.x.0[3] { shuffle_px::<CH>(srcn, srcn, [1, 1, 1, 1]) } else { srcn };
        let mut shufn = shuffle_px::<CH>(src, next, [1, 2, 3, 4]);
        if i.x.0[1] == i.x.0[0] {
            shuf = shuffle_px::<CH>(shuf, shuf, [0, 0, 1, 2]);
            shufn = shuffle_px::<CH>(shufn, shufn, [0, 0, 1, 2]);
        }
        if i.x.0[2] == i.x.0[1] {
            shuf = shuffle_px::<CH>(shuf, shuf, [0, 1, 1, 2]);
            shufn = shuffle_px::<CH>(shufn, shufn, [0, 1, 1, 2]);
        }
        if i.x.0[3] == i.x.0[2] {
            shuf = shuffle_px::<CH>(shuf, shuf, [0, 1, 2, 2]);
            shufn = shuffle_px::<CH>(shufn, shufn, [0, 1, 2, 2]);
        }
        let interp = lerp_lanes(shuf, shufn, spread::<CH>(fracx.0));
        // SAFETY: a chunk starting inside the span.
        unsafe { P::commit::<BLEND>(ctx, fs, buf, color.apply(P::wide_from_signed(interp))) };

        i.x = ixn;
        fracx = fracn;
        src = srcn;
        buf = buf.add(4);
    }
}

/// `blendTextureLinearFast<BLEND>`: no scaling, a subpixel offset; samples ahead one chunk. `UNCHECKED`: the
/// dispatch proved (`rules::linear_reads_in_bounds`) that every texel this loop reads lies inside the
/// sampler, so its loads skip the bounds check.
///
/// # Safety
/// The span contract of the module doc for `span` pixels from `buf`.
unsafe fn blend_texture_linear_fast<
    const BLEND: bool,
    const CH: usize,
    const UNCHECKED: bool,
    S: Sampler,
    C: ExtColor<P::Wide>,
    P: ExtPx,
>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    uv: vec2,
    span: i32,
    min_uv: vec2_scalar,
    max_uv: vec2_scalar,
    color: C,
    mut buf: P,
) {
    let c = s.common();
    let (i, frac, mut row0, mut row1) = linear_filter_start(c, uv, min_uv, max_uv);
    let fracx = V16::splat(computeFracX(c, i, frac).0[0]);
    let fy = V16::splat(computeFracY(frac).0[0]);
    // The texel loads: unchecked when the dispatch proved the whole range this loop reads lies inside the
    // sampler, else the checked `tex_signed`.
    let load = |idx: i32| -> V16<i16> {
        if UNCHECKED {
            // SAFETY: the dispatch instantiated `UNCHECKED = true` only when
            // `rules::linear_reads_in_bounds(row0, row1, last, len_texels)` held for the initial rows and the
            // last chunk index `last` of this loop (every load is at `row0/row1 + 4*j` with `j <= last`), so
            // `idx >= 0` and the 4 texels from `idx` lie inside the sampler's `len` bytes.
            unsafe { P::tex_signed_unchecked(c, idx) }
        } else {
            P::tex_signed(c, idx)
        }
    };
    let mut src = lerp_lanes(load(row0), load(row1), fy);

    let end = buf.add(span.max(0) as usize).addr();
    while buf.addr() < end {
        row0 = row0.wrapping_add(4);
        row1 = row1.wrapping_add(4);
        let srcn = lerp_lanes(load(row0), load(row1), fy);
        let interpn = shuffle_px::<CH>(src, srcn, [1, 2, 3, 4]);
        let interp = lerp_lanes(src, interpn, fracx);
        // SAFETY: a chunk starting inside the span.
        unsafe { P::commit::<BLEND>(ctx, fs, buf, color.apply(P::wide_from_signed(interp))) };
        src = srcn;
        buf = buf.add(4);
    }
}

/// `blendTextureLinearDownscale<BLEND>`: constant Y, 2x downscaling. `UNCHECKED`: as in
/// `blend_texture_linear_fast`.
///
/// # Safety
/// The span contract of the module doc for `span` pixels from `buf`.
unsafe fn blend_texture_linear_downscale<
    const BLEND: bool,
    const CH: usize,
    const UNCHECKED: bool,
    S: Sampler,
    C: ExtColor<P::Wide>,
    P: ExtPx,
>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    uv: vec2,
    span: i32,
    min_uv: vec2_scalar,
    max_uv: vec2_scalar,
    color: C,
    mut buf: P,
) {
    let c = s.common();
    let (i, frac, mut row0, mut row1) = linear_filter_start(c, uv, min_uv, max_uv);
    let fracx = V16::splat(computeFracX(c, i, frac).0[0]);
    let fy = V16::splat(computeFracY(frac).0[0]);
    // The texel loads: unchecked when the dispatch proved the whole range this loop reads lies inside the
    // sampler, else the checked `tex_signed`.
    let load = |idx: i32| -> V16<i16> {
        if UNCHECKED {
            // SAFETY: as in `blend_texture_linear_fast`: the dispatch proved
            // `rules::linear_reads_in_bounds(row0, row1, last, len_texels)` for the initial rows and the last
            // chunk index `last` of this loop (every load is at `row0/row1 + 4*j` with `j <= last`), so
            // `idx >= 0` and the 4 texels from `idx` lie inside the sampler's `len` bytes.
            unsafe { P::tex_signed_unchecked(c, idx) }
        } else {
            P::tex_signed(c, idx)
        }
    };

    let end = buf.add(span.max(0) as usize).addr();
    while buf.addr() < end {
        let src = lerp_lanes(load(row0), load(row1), fy);
        row0 = row0.wrapping_add(4);
        row1 = row1.wrapping_add(4);
        let srcn = lerp_lanes(load(row0), load(row1), fy);
        row0 = row0.wrapping_add(4);
        row1 = row1.wrapping_add(4);
        let interp = shuffle_px::<CH>(src, srcn, [0, 2, 4, 6]);
        let interpn = shuffle_px::<CH>(src, srcn, [1, 3, 5, 7]);
        let interp = lerp_lanes(interp, interpn, fracx);
        // SAFETY: a chunk starting inside the span.
        unsafe { P::commit::<BLEND>(ctx, fs, buf, color.apply(P::wide_from_signed(interp))) };
        buf = buf.add(4);
    }
}

/// `enum LinearFilter`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinearFilter {
    /// No linear filter is needed.
    LINEAR_FILTER_NEAREST = 0,
    /// Any clamping and scale.
    LINEAR_FILTER_FALLBACK,
    /// Axis-aligned upscaling.
    LINEAR_FILTER_UPSCALE,
    /// No scaling, a subpixel offset.
    LINEAR_FILTER_FAST,
    /// Axis-aligned 2x downscaling.
    LINEAR_FILTER_DOWNSCALE,
}
use LinearFilter::*;

/// Dispatch to the linear filter for the channel count of `P` (the C++ templates on the pixel type).
macro_rules! by_channels {
    ($p:ty, $f:ident, $($a:expr),*) => {
        if <$p as ExtPx>::C == 4 { $f::<BLEND, 4, S, C, $p>($($a),*) } else { $f::<BLEND, 1, S, C, $p>($($a),*) }
    };
}

/// As [`by_channels!`], for a linear filter that takes a `const UNCHECKED: bool` after `CH`.
macro_rules! by_channels_unchecked {
    ($p:ty, $f:ident, $u:literal, $($a:expr),*) => {
        if <$p as ExtPx>::C == 4 { $f::<BLEND, 4, $u, S, C, $p>($($a),*) } else { $f::<BLEND, 1, $u, S, C, $p>($($a),*) }
    };
}

/// `blendTextureLinearDispatch<BLEND>`: the fast filters inside the row, the fallback for the rest. Returns the
/// buffer past what was drawn, and whether the fast or downscale filter ran with `UNCHECKED = true`.
/// `FORCE_CHECKED`: run those two filters with their checked loads whatever the bounds rule says (the test
/// hook below).
///
/// # Safety
/// The span contract of the module doc for `span` pixels from `buf`.
unsafe fn blend_texture_linear_dispatch<
    const BLEND: bool,
    const FORCE_CHECKED: bool,
    S: Sampler,
    C: ExtColor<P::Wide>,
    P: ExtPx,
>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    mut uv: vec2,
    span: i32,
    uv_step: vec2_scalar,
    min_uv: vec2_scalar,
    max_uv: vec2_scalar,
    color: C,
    mut buf: P,
    filter: LinearFilter,
) -> (P, bool) {
    let end = buf.add(span.max(0) as usize);
    // C++ `int(end - buf)`: negative once a fallback's last chunk has run past the end.
    let left = |buf: P| ((end.addr() as isize - buf.addr() as isize) / core::mem::size_of::<P::Pixel>() as isize) as i64;
    let mut unchecked_used = false;
    // SAFETY (all blocks): every call covers a part of the span between `buf` and `end`.
    unsafe {
        if filter != LINEAR_FILTER_FALLBACK {
            // Y is constant across the row; first handle samples before the row that need clamping.
            let before_dist = max(0.0f32, min_uv.x) - uv.x.0[0];
            if before_dist > 0.0 {
                let before = rules::clamp_steps(toi(ceil(before_dist / uv_step.x)), left(buf)) as i32;
                buf = blend_texture_linear_fallback::<BLEND, S, C, P>(
                    ctx, fs, s, uv, before, uv_step, min_uv, max_uv, color, buf,
                );
                uv.x += Float::splat(((before / STEP) as f32) * uv_step.x);
            }
            // The samples inside the row, leaving a step of room in case the filter oversamples.
            let inside_dist =
                min(max_uv.x, ((s.common().width as i32).wrapping_sub(STEP).wrapping_mul(swgl_LinearQuantizeScale)) as f32)
                    - uv.x.0[0];
            let avail = left(buf);
            // With nothing left (`inside <= 0` in the C++) no filter runs.
            if uv_step.x > 0.0 && inside_dist >= uv_step.x && avail > 0 {
                let c = s.common();
                // The two rows the fast and downscale filters read, exactly as those filters compute them
                // (`linear_filter_start`), and the sampler's length in texels.
                let (_, _, row0, row1) = linear_filter_start(c, uv, min_uv, max_uv);
                let len_texels = (c.len / P::TEXEL_BYTES) as i64;
                if filter == LINEAR_FILTER_DOWNSCALE {
                    if let Some(inside) = rules::chunk_floor(toi(inside_dist * (0.5f32 / LQS)) as i64, avail) {
                        let inside = inside as i32;
                        // One bounds check for the whole span: the loop runs `n = ceil(inside/4)` times and
                        // loads at `row0/row1 + 4*j` for `j = 0..2n`, so the last chunk index is `2*n - 1`.
                        let last = 2 * ((inside as i64 + 3) / 4) - 1;
                        let unchecked =
                            !FORCE_CHECKED && rules::linear_reads_in_bounds(row0 as i64, row1 as i64, last, len_texels);
                        if unchecked {
                            by_channels_unchecked!(P, blend_texture_linear_downscale, true, ctx, fs, s, uv, inside, min_uv, max_uv, color, buf);
                        } else {
                            by_channels_unchecked!(P, blend_texture_linear_downscale, false, ctx, fs, s, uv, inside, min_uv, max_uv, color, buf);
                        }
                        unchecked_used = unchecked;
                        buf = buf.add(inside as usize);
                        uv.x += Float::splat(((inside / STEP) as f32) * uv_step.x);
                    }
                } else if filter == LINEAR_FILTER_UPSCALE {
                    let inside = (toi(inside_dist / uv_step.x).wrapping_mul(STEP) as i64).min(avail) as i32;
                    if inside > 0 {
                        by_channels!(P, blend_texture_linear_upscale, ctx, fs, s, uv, inside, uv_step, min_uv, max_uv, color, buf);
                        buf = buf.add(inside as usize);
                        uv.x += Float::splat(((inside / STEP) as f32) * uv_step.x);
                    }
                } else if let Some(inside) = rules::chunk_floor(toi(inside_dist * (1.0f32 / LQS)) as i64, avail) {
                    let inside = inside as i32;
                    // One bounds check for the whole span: the loop loads at `row0/row1 + 4*j` for
                    // `j = 0..=n` with `n = ceil(inside/4)` iterations, so the last chunk index is `n`.
                    let last = (inside as i64 + 3) / 4;
                    let unchecked =
                        !FORCE_CHECKED && rules::linear_reads_in_bounds(row0 as i64, row1 as i64, last, len_texels);
                    if unchecked {
                        by_channels_unchecked!(P, blend_texture_linear_fast, true, ctx, fs, s, uv, inside, min_uv, max_uv, color, buf);
                    } else {
                        by_channels_unchecked!(P, blend_texture_linear_fast, false, ctx, fs, s, uv, inside, min_uv, max_uv, color, buf);
                    }
                    unchecked_used = unchecked;
                    buf = buf.add(inside as usize);
                    uv.x += Float::splat(((inside / STEP) as f32) * uv_step.x);
                }
            }
        }
        // The fallback for whatever is left, or for all of it.
        if buf.addr() < end.addr() {
            buf = blend_texture_linear_fallback::<BLEND, S, C, P>(
                ctx,
                fs,
                s,
                uv,
                left(buf) as i32,
                uv_step,
                min_uv,
                max_uv,
                color,
                buf,
            );
        }
    }
    (buf, unchecked_used)
}

/// `blendTextureLinear<BLEND>`: quantize, then dispatch. 0 if the texture format does not match the span.
///
/// # Safety
/// The span contract of the module doc for `span` pixels from `buf`.
unsafe fn blend_texture_linear<const BLEND: bool, S: Sampler, C: ExtColor<P::Wide>, P: ExtPx>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    uv: vec2,
    span: i32,
    uv_rect: &vec4_scalar,
    color: C,
    buf: P,
    filter: LinearFilter,
) -> i32 {
    if !P::match_format(s.common().format) {
        return 0;
    }
    let (uv, uv_step, min_uv, max_uv) = linear_quantize_uv(s, uv, uv_rect);
    // SAFETY: forwarded from the caller.
    unsafe { blend_texture_linear_dispatch::<BLEND, false, S, C, P>(ctx, fs, s, uv, span, uv_step, min_uv, max_uv, color, buf, filter) };
    span
}

/// `blendTextureLinear<BLEND>` for `raster/core/tests/linear_unchecked.rs`: as the private
/// `blend_texture_linear`, but `force_checked` runs the fast and downscale filters with `UNCHECKED = false`
/// whatever the bounds rule says, and the second return value says whether the dispatch ran one of them with
/// `UNCHECKED = true`.
///
/// # Safety
/// The span contract of the module doc for `span` pixels from `buf`.
#[doc(hidden)]
pub unsafe fn blend_texture_linear_force_checked<const BLEND: bool, S: Sampler, C: ExtColor<P::Wide>, P: ExtPx>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    uv: vec2,
    span: i32,
    uv_rect: &vec4_scalar,
    color: C,
    buf: P,
    filter: LinearFilter,
    force_checked: bool,
) -> (i32, bool) {
    if !P::match_format(s.common().format) {
        return (0, false);
    }
    let (uv, uv_step, min_uv, max_uv) = linear_quantize_uv(s, uv, uv_rect);
    // SAFETY: forwarded from the caller.
    let (_, unchecked) = unsafe {
        if force_checked {
            blend_texture_linear_dispatch::<BLEND, true, S, C, P>(
                ctx, fs, s, uv, span, uv_step, min_uv, max_uv, color, buf, filter,
            )
        } else {
            blend_texture_linear_dispatch::<BLEND, false, S, C, P>(
                ctx, fs, s, uv, span, uv_step, min_uv, max_uv, color, buf, filter,
            )
        }
    };
    (span, unchecked)
}

/// `blendTextureNearestFast<BLEND>`: 1:1 nearest sampling of one row, clamped to the UV bounds.
///
/// # Safety
/// The span contract of the module doc for `span` pixels from `buf`.
unsafe fn blend_texture_nearest_fast<const BLEND: bool, S: Sampler, C: ExtColor<P::Wide>, P: ExtPx>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    uv: vec2,
    span: i32,
    uv_rect: &vec4_scalar,
    color: C,
    mut buf: P,
) -> i32 {
    let c = s.common();
    if !P::match_format(c.format) {
        return 0;
    }
    let i = make_ivec2(samplerScale_scalar(s, force_scalar(uv)));
    let min_uv = make_ivec2(samplerScale_scalar(s, v2s(uv_rect.x, uv_rect.y)));
    let max_uv = make_ivec2(samplerScale_scalar(s, v2s(uv_rect.z, uv_rect.w)));
    let height = (c.height as i64).clamp(1, rules::SIDE);
    let stride = (c.stride as i64).clamp(1, rules::STRIDE);
    let row = rules::nearest_row(i.y as i64, min_uv.y as i64, max_uv.y as i64, height, stride) as i32;
    let width = (c.width as i64).clamp(1, rules::SIDE);
    let split = rules::nearest_split(i.x as i64, span.max(0) as i64, min_uv.x as i64, max_uv.x as i64, width);
    let mut cur_x = (i.x as i64 + split.before) as i32;
    // SAFETY (all blocks): the four parts add up to the span (`rules::nearest_split`).
    unsafe {
        // The C++ fills `min(minX, endX) - curX` pixels whenever it starts before `minX`, also none for an empty
        // span: an R8 fill of none at an unaligned pointer still stores one byte (commit_solid_span<false>).
        if split.before > 0 || (span == 0 && (i.x as i64) < split.min_x) {
            let src = color.apply(P::tex_texel(c, row + split.min_x as i32));
            P::commit_solid::<BLEND>(ctx, fs, buf, src, split.before as i32);
            buf = buf.add(split.before as usize);
        }
        // One bounds check for all the chunks, as swgl reads them unchecked (the per-chunk check and its cold
        // partial load kept the loop from being a plain load and store).
        let chunks = split.inside / 4;
        let first = row as i64 + cur_x as i64;
        let whole = first >= 0
            && !c.buf.0.is_null()
            && ((first + chunks * 4) as u64).saturating_mul(P::TEXEL_BYTES as u64) <= c.len as u64;
        if whole {
            for _ in 0..chunks {
                let src = color.apply(P::tex_chunk_unchecked(c, row + cur_x));
                P::commit::<BLEND>(ctx, fs, buf, src);
                cur_x += 4;
                buf = buf.add(4);
            }
        } else {
            for _ in 0..chunks {
                let src = color.apply(P::tex_chunk(c, row + cur_x));
                P::commit::<BLEND>(ctx, fs, buf, src);
                cur_x += 4;
                buf = buf.add(4);
            }
        }
        if split.rest > 0 {
            let src = color.apply(P::tex_partial(c, row + cur_x, split.rest as i32));
            P::commit_len::<BLEND>(ctx, fs, buf, src, split.rest as i32);
            buf = buf.add(split.rest as usize);
        }
        if split.after > 0 {
            let src = color.apply(P::tex_texel(c, row + split.max_x as i32));
            P::commit_solid::<BLEND>(ctx, fs, buf, src, split.after as i32);
        }
    }
    span
}

/// `spanNeedsScale(span, P)`: 0 for a 1:1 step, 2 for a 2x downscale, 1 for any other scale.
#[inline(always)]
fn span_needs_scale(span: i32, p: &vec2) -> i32 {
    let span = (span & !(128 - 1)).wrapping_add(128);
    let scaled = toi(round((p.x.0[1] - p.x.0[0]) * span as f32));
    if scaled != span {
        if scaled == span.wrapping_mul(2) {
            2
        } else {
            1
        }
    } else {
        0
    }
}

/// `needsTextureLinear(sampler, P, span)`.
fn needs_texture_linear<S: Sampler>(s: &S, p: vec2, span: i32) -> LinearFilter {
    if s.common().width < 2 {
        return LINEAR_FILTER_NEAREST;
    }
    if p.y.0[0] != p.y.0[1] {
        return LINEAR_FILTER_FALLBACK;
    }
    let p = samplerScale(s, p);
    let scale = span_needs_scale(span, &p);
    if scale != 0 {
        return if p.x.0[0] < p.x.0[1] && p.x.0[1] - p.x.0[0] <= 1.0 {
            LINEAR_FILTER_UPSCALE
        } else if scale == 2 {
            LINEAR_FILTER_DOWNSCALE
        } else {
            LINEAR_FILTER_FALLBACK
        };
    }
    if (toi(p.x.0[0] * 4.0 + 0.5) & 3) != 2 || (toi(p.y.0[0] * 4.0 + 0.5) & 3) != 2 {
        return LINEAR_FILTER_FAST;
    }
    LINEAR_FILTER_NEAREST
}

/// `swgl_commitTextureLinear(format, s, p, uv_rect, color, n)`.
fn commit_texture_linear<S: Sampler, C: ExtColor<P::Wide>, P: ExtPx>(
    ctx: &RasterState,
    s: &S,
    p: vec2,
    uv_rect: &vec4_scalar,
    color: C,
    len: i32,
    fs: &mut FragmentShaderState,
) {
    let buf = P::out(fs);
    let f: &FragmentShaderState = fs;
    let blend = ctx.blend_key != BLEND_KEY_NONE;
    // SAFETY: the span contract of the module doc; `len <= swgl_SpanLength`.
    let drawn = unsafe {
        let filter = needs_texture_linear(s, p, len);
        if filter != LINEAR_FILTER_NEAREST {
            if blend {
                blend_texture_linear::<true, S, C, P>(ctx, f, s, p, len, uv_rect, color, buf, filter)
            } else {
                blend_texture_linear::<false, S, C, P>(ctx, f, s, p, len, uv_rect, color, buf, filter)
            }
        } else if blend {
            blend_texture_nearest_fast::<true, S, C, P>(ctx, f, s, p, len, uv_rect, color, buf)
        } else {
            blend_texture_nearest_fast::<false, S, C, P>(ctx, f, s, p, len, uv_rect, color, buf)
        }
    };
    advance::<P>(fs, drawn);
}

/// `swgl_commitTextureLinearRGBA8(s, p, uv_rect)`: the span of a linearly filtered texture.
pub fn swgl_commitTextureLinearRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    uv_rect: vec4_scalar,
    fs: &mut FragmentShaderState,
) {
    let len = fs.swgl_SpanLength;
    commit_texture_linear::<S, NoColor, *mut u32>(ctx, &s, p, &uv_rect, NoColor, len, fs);
}

/// `swgl_commitTextureLinearR8(s, p, uv_rect)`.
pub fn swgl_commitTextureLinearR8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    uv_rect: vec4_scalar,
    fs: &mut FragmentShaderState,
) {
    let len = fs.swgl_SpanLength;
    commit_texture_linear::<S, NoColor, *mut u8>(ctx, &s, p, &uv_rect, NoColor, len, fs);
}

/// `swgl_commitPartialTextureLinearR8(len, s, p, uv_rect)`.
pub fn swgl_commitPartialTextureLinearR8<S: Sampler>(
    ctx: &mut RasterState,
    len: i32,
    s: S,
    p: vec2,
    uv_rect: vec4_scalar,
    fs: &mut FragmentShaderState,
) {
    let n = rules::partial_len(len as i64, fs.swgl_SpanLength as i64) as i32;
    commit_texture_linear::<S, NoColor, *mut u8>(ctx, &s, p, &uv_rect, NoColor, n, fs);
}

/// `swgl_commitPartialTextureLinearInvertR8(len, s, p, uv_rect)`: `255 - texel`.
pub fn swgl_commitPartialTextureLinearInvertR8<S: Sampler>(
    ctx: &mut RasterState,
    len: i32,
    s: S,
    p: vec2,
    uv_rect: vec4_scalar,
    fs: &mut FragmentShaderState,
) {
    let n = rules::partial_len(len as i64, fs.swgl_SpanLength as i64) as i32;
    commit_texture_linear::<S, InvertColor, *mut u8>(ctx, &s, p, &uv_rect, InvertColor, n, fs);
}

/// `swgl_commitTextureLinearColorRGBA8(s, p, uv_rect, color)`: scaled by a color (`vec4_scalar` or `float`).
pub fn swgl_commitTextureLinearColorRGBA8<S: Sampler, K: PackColor<*mut u32>>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    uv_rect: vec4_scalar,
    color: K,
    fs: &mut FragmentShaderState,
) where
    K::Output: ExtColor<WideRGBA8>,
{
    let len = fs.swgl_SpanLength;
    commit_texture_linear::<S, K::Output, *mut u32>(ctx, &s, p, &uv_rect, color.pack_color(), len, fs);
}

/// `swgl_commitTextureLinearColorR8(s, p, uv_rect, color)`.
pub fn swgl_commitTextureLinearColorR8<S: Sampler, K: PackColor<*mut u8>>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    uv_rect: vec4_scalar,
    color: K,
    fs: &mut FragmentShaderState,
) where
    K::Output: ExtColor<WideR8>,
{
    let len = fs.swgl_SpanLength;
    commit_texture_linear::<S, K::Output, *mut u8>(ctx, &s, p, &uv_rect, color.pack_color(), len, fs);
}

/// `blendTextureLinearR8<BLEND>`: an R8 texture expanded to the span's format.
///
/// # Safety
/// The span contract of the module doc for `span` pixels from `buf`.
unsafe fn blend_texture_linear_r8<const BLEND: bool, S: Sampler, C: ExtColor<P::Wide>, P: ExtPx>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    uv: vec2,
    span: i32,
    uv_rect: &vec4_scalar,
    color: C,
    mut buf: P,
) -> i32 {
    if s.common().format != TextureFormat::R8 || s.common().width < 2 {
        return 0;
    }
    let (mut uv, uv_step, min_uv, max_uv) = linear_quantize_uv(s, uv, uv_rect);
    let end = buf.add(span.max(0) as usize).addr();
    while buf.addr() < end {
        let r = color.apply(P::expand(textureLinearUnpackedR8(s, iv2(clamp2(uv, min_uv, max_uv)))));
        // SAFETY: a chunk starting inside the span.
        unsafe { P::commit::<BLEND>(ctx, fs, buf, r) };
        buf = buf.add(4);
        uv = add2(uv, uv_step);
    }
    span
}

/// `swgl_commitTextureLinearColorR8ToRGBA8(s, p, uv_rect, color)`.
pub fn swgl_commitTextureLinearColorR8ToRGBA8<S: Sampler, K: PackColor<*mut u32>>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    uv_rect: vec4_scalar,
    color: K,
    fs: &mut FragmentShaderState,
) where
    K::Output: ExtColor<WideRGBA8>,
{
    let color = color.pack_color();
    let buf = <*mut u32 as ExtPx>::out(fs);
    let len = fs.swgl_SpanLength;
    let f: &FragmentShaderState = fs;
    // SAFETY: the span contract of the module doc.
    let drawn = unsafe {
        if ctx.blend_key != BLEND_KEY_NONE {
            blend_texture_linear_r8::<true, S, K::Output, *mut u32>(ctx, f, &s, p, len, &uv_rect, color, buf)
        } else {
            blend_texture_linear_r8::<false, S, K::Output, *mut u32>(ctx, f, &s, p, len, &uv_rect, color, buf)
        }
    };
    advance::<*mut u32>(fs, drawn);
}

/// `swgl_commitTextureLinearR8ToRGBA8(s, p, uv_rect)`.
pub fn swgl_commitTextureLinearR8ToRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    uv_rect: vec4_scalar,
    fs: &mut FragmentShaderState,
) {
    swgl_commitTextureLinearColorR8ToRGBA8(ctx, s, p, uv_rect, NoColor, fs);
}

// ---- repeating UVs ----------------------------------------------------------------------------------------------

/// `tileRepeatUV(uv, tile_repeat)`.
#[inline(always)]
fn tile_repeat_uv(uv: vec2, tile_repeat: &vec2_scalar) -> vec2 {
    let mut uv = uv;
    if tile_repeat.x > 0.0 {
        // Just under the limit, so that fract() comes close to but not to 1.
        uv = clamp2(uv, v2s(0.0, 0.0), *tile_repeat - 1.0e-6f32);
    }
    fract(uv)
}

/// `computeNoRepeatSteps(uv, uv_step, tile_repeat, steps)`.
#[inline(always)]
fn compute_no_repeat_steps(uv: Float, uv_step: f32, tile_repeat: f32, steps: i32) -> i32 {
    let mut uv = uv;
    if uv.0[3] < uv.0[0] {
        uv = uv.swizzle([3, 2, 1, 0]);
    }
    let mut limit = floor(uv.0[0]) + 1.0;
    if tile_repeat > 0.0 {
        limit = min(limit, tile_repeat);
    }
    if uv.0[0] >= 0.0 && uv.0[3] < limit {
        if uv_step != 0.0 {
            toi(clamp((limit - uv.0[0]) / uv_step, 0.0f32, steps as f32))
        } else {
            steps
        }
    } else {
        0
    }
}

/// `blendTextureLinearRepeat<BLEND>`.
///
/// # Safety
/// The span contract of the module doc for `span` pixels from `buf`.
unsafe fn blend_texture_linear_repeat<const BLEND: bool, S: Sampler, C: ExtColor<P::Wide>, P: ExtPx>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    mut uv: vec2,
    span: i32,
    tile_repeat: &vec2_scalar,
    uv_repeat: &vec4_scalar,
    uv_rect: &vec4_scalar,
    color: C,
    mut buf: P,
) -> i32 {
    if !P::match_format(s.common().format) {
        return 0;
    }
    let mut uv_scale = v2s(uv_repeat.z - uv_repeat.x, uv_repeat.w - uv_repeat.y);
    let mut uv_offset = v2s(uv_repeat.x, uv_repeat.y);
    let filter = needs_texture_linear(s, mad2(uv, uv_scale, uv_offset), span);
    let uv_step = (STEP as f32) * v2s(uv.x.0[1] - uv.x.0[0], uv.y.0[1] - uv.y.0[0]);
    uv_scale = samplerScale_scalar(s, uv_scale) * LQS;
    uv_offset = linearQuantize_scalar_s(uv_offset, LQS, s);
    let min_uv = max(linearQuantize_scalar_s(v2s(uv_rect.x, uv_rect.y), LQS, s), 0.0f32);
    let max_uv = max(linearQuantize_scalar_s(v2s(uv_rect.z, uv_rect.w), LQS, s), min_uv);
    let end = buf.add(span.max(0) as usize);
    while buf.addr() < end.addr() {
        let mut steps = (((end.addr() - buf.addr()) / core::mem::size_of::<P::Pixel>()) as i32) / STEP;
        // The sub-span before UVs repeat, without repeat math.
        steps = compute_no_repeat_steps(uv.x, uv_step.x, tile_repeat.x, steps);
        if steps > 0 {
            steps = compute_no_repeat_steps(uv.y, uv_step.y, tile_repeat.y, steps);
            if steps > 0 {
                // SAFETY: `steps` chunks fit in what is left of the span (`compute_no_repeat_steps`' clamp).
                buf = unsafe {
                    blend_texture_linear_dispatch::<BLEND, false, S, C, P>(
                        ctx,
                        fs,
                        s,
                        mad2(fract(uv), uv_scale, uv_offset),
                        steps * STEP,
                        uv_step * uv_scale,
                        min_uv,
                        max_uv,
                        color,
                        buf,
                        filter,
                    )
                }
                .0;
                if buf.addr() >= end.addr() {
                    break;
                }
                uv = add2(uv, (steps as f32) * uv_step);
            }
        }
        // UVs may repeat within this chunk.
        let repeated_uv = clamp2(mad2(tile_repeat_uv(uv, tile_repeat), uv_scale, uv_offset), min_uv, max_uv);
        let r = color.apply(P::linear_unpacked(s, iv2(repeated_uv)));
        // SAFETY: a chunk starting inside the span.
        unsafe { P::commit::<BLEND>(ctx, fs, buf, r) };
        buf = buf.add(4);
        uv = add2(uv, uv_step);
    }
    span
}

/// `swgl_commitTextureLinearRepeat(format, s, p, tile_repeat, uv_repeat, uv_rect, color)`.
fn commit_texture_linear_repeat<S: Sampler, C: ExtColor<WideRGBA8>>(
    ctx: &RasterState,
    s: &S,
    p: vec2,
    tile_repeat: &vec2_scalar,
    uv_repeat: &vec4_scalar,
    uv_rect: &vec4_scalar,
    color: C,
    fs: &mut FragmentShaderState,
) {
    let buf = <*mut u32 as ExtPx>::out(fs);
    let len = fs.swgl_SpanLength;
    let f: &FragmentShaderState = fs;
    // SAFETY: the span contract of the module doc.
    let drawn = unsafe {
        if ctx.blend_key != BLEND_KEY_NONE {
            blend_texture_linear_repeat::<true, S, C, *mut u32>(ctx, f, s, p, len, tile_repeat, uv_repeat, uv_rect, color, buf)
        } else {
            blend_texture_linear_repeat::<false, S, C, *mut u32>(ctx, f, s, p, len, tile_repeat, uv_repeat, uv_rect, color, buf)
        }
    };
    advance::<*mut u32>(fs, drawn);
}

/// `swgl_commitTextureLinearRepeatRGBA8(s, p, tile_repeat, uv_repeat, uv_rect)`.
pub fn swgl_commitTextureLinearRepeatRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    tile_repeat: vec2_scalar,
    uv_repeat: vec4_scalar,
    uv_rect: vec4_scalar,
    fs: &mut FragmentShaderState,
) {
    commit_texture_linear_repeat(ctx, &s, p, &tile_repeat, &uv_repeat, &uv_rect, NoColor, fs);
}

/// `swgl_commitTextureLinearRepeatColorRGBA8(s, p, tile_repeat, uv_repeat, uv_rect, color)`.
pub fn swgl_commitTextureLinearRepeatColorRGBA8<S: Sampler, K: PackColor<*mut u32>>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    tile_repeat: vec2_scalar,
    uv_repeat: vec4_scalar,
    uv_rect: vec4_scalar,
    color: K,
    fs: &mut FragmentShaderState,
) where
    K::Output: ExtColor<WideRGBA8>,
{
    commit_texture_linear_repeat(ctx, &s, p, &tile_repeat, &uv_repeat, &uv_rect, color.pack_color(), fs);
}

// ---- nearest filtering ------------------------------------------------------------------------------------------

/// `blendTextureNearestRepeat<BLEND, REPEAT>` (RGBA8 only, as `textureNearestPacked`).
///
/// # Safety
/// The span contract of the module doc for `span` pixels from `buf`.
unsafe fn blend_texture_nearest_repeat<const BLEND: bool, const REPEAT: bool, S: Sampler, C: ExtColor<WideRGBA8>>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    mut uv: vec2,
    span: i32,
    tile_repeat: &vec2_scalar,
    uv_rect: &vec4_scalar,
    color: C,
    mut buf: *mut u32,
) -> i32 {
    if s.common().format != TextureFormat::RGBA8 {
        return 0;
    }
    if !REPEAT {
        // Clamping steps in texels; repeating scales by uv_scale instead.
        uv = samplerScale(s, uv);
    }
    let uv_step = (STEP as f32) * v2s(uv.x.0[1] - uv.x.0[0], uv.y.0[1] - uv.y.0[0]);
    let min_uv = samplerScale_scalar(s, v2s(uv_rect.x, uv_rect.y));
    let max_uv = samplerScale_scalar(s, v2s(uv_rect.z, uv_rect.w));
    let uv_scale = max_uv - min_uv;
    let r = if REPEAT { 1 } else { 0 };
    let sample = |uv: vec2| unpack(textureNearestPackedRGBA8(s, iv2(uv)));
    let repeated = |uv: vec2| if REPEAT { mad2(tile_repeat_uv(uv, tile_repeat), uv_scale, min_uv) } else { clamp2(uv, min_uv, max_uv) };
    // A single texel on both axes (bounds on texel edges for repeat, on centres for clamp, or too little movement
    // to reach the next texel): a solid span.
    if (toi(min_uv.x).wrapping_add(r) >= toi(max_uv.x)
        || (abs(uv_step.x) * span as f32 * (if REPEAT { uv_scale.x } else { 1.0 }) < 0.5))
        && (toi(min_uv.y).wrapping_add(r) >= toi(max_uv.y)
            || (abs(uv_step.y) * span as f32 * (if REPEAT { uv_scale.y } else { 1.0 }) < 0.5))
    {
        // SAFETY: the span.
        unsafe { <*mut u32 as ExtPx>::commit_solid::<BLEND>(ctx, fs, buf, color.apply(sample(repeated(uv))), span) };
        return span;
    }
    let end = buf.wrapping_add(span.max(0) as usize);
    while buf < end {
        if REPEAT {
            let mut steps = ((end as usize - buf as usize) / 4) as i32 / STEP;
            steps = compute_no_repeat_steps(uv.x, uv_step.x, tile_repeat.x, steps);
            if steps > 0 {
                steps = compute_no_repeat_steps(uv.y, uv_step.y, tile_repeat.y, steps);
                if steps > 0 {
                    let mut inside_uv = mad2(fract(uv), uv_scale, min_uv);
                    let inside_step = uv_step * uv_scale;
                    let outside = buf.wrapping_add((steps * STEP) as usize);
                    while buf < outside {
                        // SAFETY: `steps` chunks fit in what is left of the span.
                        unsafe { <*mut u32 as ExtPx>::commit::<BLEND>(ctx, fs, buf, color.apply(sample(inside_uv))) };
                        buf = buf.wrapping_add(4);
                        inside_uv = add2(inside_uv, inside_step);
                    }
                    if buf >= end {
                        break;
                    }
                    uv = add2(uv, (steps as f32) * uv_step);
                }
            }
        }
        // UVs may repeat within this chunk.
        // SAFETY: a chunk starting inside the span.
        unsafe { <*mut u32 as ExtPx>::commit::<BLEND>(ctx, fs, buf, color.apply(sample(repeated(uv)))) };
        buf = buf.wrapping_add(4);
        uv = add2(uv, uv_step);
    }
    span
}

/// `needsNearestFallback(sampler, P, span)`: Y moves half a texel or more, or X scales.
#[inline(always)]
fn needs_nearest_fallback<S: Sampler>(s: &S, p: vec2, span: i32) -> bool {
    let p = samplerScale(s, p);
    (p.y.0[1] - p.y.0[0]) * span as f32 >= 0.5 || span_needs_scale(span, &p) != 0
}

/// `swgl_commitTextureNearest(format, s, p, uv_rect, color)` (RGBA8).
fn commit_texture_nearest<S: Sampler, C: ExtColor<WideRGBA8>>(
    ctx: &RasterState,
    s: &S,
    p: vec2,
    uv_rect: &vec4_scalar,
    color: C,
    fs: &mut FragmentShaderState,
) {
    let buf = <*mut u32 as ExtPx>::out(fs);
    let len = fs.swgl_SpanLength;
    let f: &FragmentShaderState = fs;
    let blend = ctx.blend_key != BLEND_KEY_NONE;
    let zero = v2s(0.0, 0.0);
    // SAFETY: the span contract of the module doc.
    let drawn = unsafe {
        if needs_nearest_fallback(s, p, len) {
            if blend {
                blend_texture_nearest_repeat::<true, false, S, C>(ctx, f, s, p, len, &zero, uv_rect, color, buf)
            } else {
                blend_texture_nearest_repeat::<false, false, S, C>(ctx, f, s, p, len, &zero, uv_rect, color, buf)
            }
        } else if blend {
            blend_texture_nearest_fast::<true, S, C, *mut u32>(ctx, f, s, p, len, uv_rect, color, buf)
        } else {
            blend_texture_nearest_fast::<false, S, C, *mut u32>(ctx, f, s, p, len, uv_rect, color, buf)
        }
    };
    advance::<*mut u32>(fs, drawn);
}

/// `swgl_commitTextureNearestRGBA8(s, p, uv_rect)`.
pub fn swgl_commitTextureNearestRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    uv_rect: vec4_scalar,
    fs: &mut FragmentShaderState,
) {
    commit_texture_nearest(ctx, &s, p, &uv_rect, NoColor, fs);
}

/// `swgl_commitTextureNearestColorRGBA8(s, p, uv_rect, color)`.
pub fn swgl_commitTextureNearestColorRGBA8<S: Sampler, K: PackColor<*mut u32>>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    uv_rect: vec4_scalar,
    color: K,
    fs: &mut FragmentShaderState,
) where
    K::Output: ExtColor<WideRGBA8>,
{
    commit_texture_nearest(ctx, &s, p, &uv_rect, color.pack_color(), fs);
}

/// `swgl_commitTextureNearestRepeat(format, s, p, tile_repeat, uv_rect, color)` (RGBA8).
fn commit_texture_nearest_repeat<S: Sampler, C: ExtColor<WideRGBA8>>(
    ctx: &RasterState,
    s: &S,
    p: vec2,
    tile_repeat: &vec2_scalar,
    uv_rect: &vec4_scalar,
    color: C,
    fs: &mut FragmentShaderState,
) {
    let buf = <*mut u32 as ExtPx>::out(fs);
    let len = fs.swgl_SpanLength;
    let f: &FragmentShaderState = fs;
    // SAFETY: the span contract of the module doc.
    let drawn = unsafe {
        if ctx.blend_key != BLEND_KEY_NONE {
            blend_texture_nearest_repeat::<true, true, S, C>(ctx, f, s, p, len, tile_repeat, uv_rect, color, buf)
        } else {
            blend_texture_nearest_repeat::<false, true, S, C>(ctx, f, s, p, len, tile_repeat, uv_rect, color, buf)
        }
    };
    advance::<*mut u32>(fs, drawn);
}

/// `swgl_commitTextureNearestRepeatRGBA8(s, p, tile_repeat, uv_repeat, uv_rect)`. As the C++ macro, the sampling
/// bounds are `uv_repeat` (texel edges of the repeated area); `uv_rect` is not used.
pub fn swgl_commitTextureNearestRepeatRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    tile_repeat: vec2_scalar,
    uv_repeat: vec4_scalar,
    _uv_rect: vec4_scalar,
    fs: &mut FragmentShaderState,
) {
    commit_texture_nearest_repeat(ctx, &s, p, &tile_repeat, &uv_repeat, NoColor, fs);
}

/// `swgl_commitTextureNearestRepeatColorRGBA8(s, p, tile_repeat, uv_repeat, uv_rect, color)`; `uv_rect` unused, as
/// [`swgl_commitTextureNearestRepeatRGBA8`].
pub fn swgl_commitTextureNearestRepeatColorRGBA8<S: Sampler, K: PackColor<*mut u32>>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    tile_repeat: vec2_scalar,
    uv_repeat: vec4_scalar,
    _uv_rect: vec4_scalar,
    color: K,
    fs: &mut FragmentShaderState,
) where
    K::Output: ExtColor<WideRGBA8>,
{
    commit_texture_nearest_repeat(ctx, &s, p, &tile_repeat, &uv_repeat, color.pack_color(), fs);
}

// ---- filtering by sampler state ---------------------------------------------------------------------------------

/// `swgl_commitTextureRGBA8(s, p, uv_rect)`: linear or nearest as the sampler's filter says.
pub fn swgl_commitTextureRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    uv_rect: vec4_scalar,
    fs: &mut FragmentShaderState,
) {
    if s.filter() == TextureFilter::LINEAR {
        swgl_commitTextureLinearRGBA8(ctx, s, p, uv_rect, fs);
    } else {
        swgl_commitTextureNearestRGBA8(ctx, s, p, uv_rect, fs);
    }
}

/// `swgl_commitTextureColorRGBA8(s, p, uv_rect, color)`.
pub fn swgl_commitTextureColorRGBA8<S: Sampler, K: PackColor<*mut u32>>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    uv_rect: vec4_scalar,
    color: K,
    fs: &mut FragmentShaderState,
) where
    K::Output: ExtColor<WideRGBA8>,
{
    if s.filter() == TextureFilter::LINEAR {
        swgl_commitTextureLinearColorRGBA8(ctx, s, p, uv_rect, color, fs);
    } else {
        swgl_commitTextureNearestColorRGBA8(ctx, s, p, uv_rect, color, fs);
    }
}

/// `swgl_commitTextureRepeatRGBA8(s, p, tile_repeat, uv_repeat, uv_rect)`.
pub fn swgl_commitTextureRepeatRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    tile_repeat: vec2_scalar,
    uv_repeat: vec4_scalar,
    uv_rect: vec4_scalar,
    fs: &mut FragmentShaderState,
) {
    if s.filter() == TextureFilter::LINEAR {
        swgl_commitTextureLinearRepeatRGBA8(ctx, s, p, tile_repeat, uv_repeat, uv_rect, fs);
    } else {
        swgl_commitTextureNearestRepeatRGBA8(ctx, s, p, tile_repeat, uv_repeat, uv_rect, fs);
    }
}

/// `swgl_commitTextureRepeatColorRGBA8(s, p, tile_repeat, uv_repeat, uv_rect, color)`.
pub fn swgl_commitTextureRepeatColorRGBA8<S: Sampler, K: PackColor<*mut u32>>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    tile_repeat: vec2_scalar,
    uv_repeat: vec4_scalar,
    uv_rect: vec4_scalar,
    color: K,
    fs: &mut FragmentShaderState,
) where
    K::Output: ExtColor<WideRGBA8>,
{
    if s.filter() == TextureFilter::LINEAR {
        swgl_commitTextureLinearRepeatColorRGBA8(ctx, s, p, tile_repeat, uv_repeat, uv_rect, color, fs);
    } else {
        swgl_commitTextureNearestRepeatColorRGBA8(ctx, s, p, tile_repeat, uv_repeat, uv_rect, color, fs);
    }
}

// ---- Gaussian blur ----------------------------------------------------------------------------------------------

/// `blendGaussianBlur<BLEND>`: a separable blur pass over whole chunks inside the row and `uv_rect`.
///
/// # Safety
/// The span contract of the module doc for `span` pixels from `buf`.
unsafe fn blend_gaussian_blur<const BLEND: bool, S: Sampler, P: ExtPx>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    uv: vec2,
    uv_rect: &vec4_scalar,
    mut buf: P,
    span: i32,
    hori: bool,
    radius: i32,
    coeffs: vec2_scalar,
) -> i32 {
    let c = s.common();
    if !P::match_format(c.format) {
        return 0;
    }
    let size = v2s(c.width as f32, c.height as f32);
    let mut cur_uv = make_ivec2(force_scalar(uv) * size);
    let bounds: ivec4_scalar = make_ivec4(*uv_rect * vec4_scalar::new(size.x, size.y, size.x, size.y));
    let n = rules::blur_len(cur_uv.x as i64, span.max(0) as i64, bounds.z as i64, (toi(size.x) as i64).clamp(0, rules::SIDE));
    let (lo, hi) = if hori { (bounds.x, bounds.z) } else { (bounds.y, bounds.w) };
    // Prove the blur's texel reads stay inside the sampler, so they can skip the per-load bounds check
    // (`gaussianBlur*` with `UNCHECKED`): `rules::blur_len` gives `cur_uv.x + n <= width`, so every chunk's
    // `x` lies in `[0, width - 4]` and its row is `x + y * stride`; the horizontal loops clamp their reads to
    // `[row - left_bound, row + right_bound] ⊆ [y*stride, y*stride + width)`, and the vertical loops read rows
    // `[max(lo, 0), min(hi, height - 1)]`, each a whole readable row. So it is enough that row `y` and the last
    // row are readable, i.e. `(height - 1) * stride + width <= len` in texels. The horizontal window needs two
    // more facts: `lo <= x` (else `left_bound` is negative and the left read `row - left_bound` lands at
    // `y*stride + lo`, past `x` and possibly past the row; `x` only grows, so the first chunk decides), and
    // `hi >= 0` (else, once `right_bound < 0`, the right read `row + right_bound` lands at `y*stride + hi`,
    // before the row, and before the buffer on row 0). The vertical loops move a row only while the offset is
    // within its bound, so negative bounds keep them on row `y`. Width, height and stride are capped at
    // `rules::SIDE`, so the `i32` row arithmetic does not overflow.
    let len_texels = (c.len / P::TEXEL_BYTES) as i64;
    let unchecked = cur_uv.x >= 0
        && cur_uv.y >= 0
        && (!hori || (lo <= cur_uv.x && hi >= 0))
        && (cur_uv.y as i64) < c.height as i64
        && (c.height as i64 - 1) * c.stride as i64 + c.width as i64 <= len_texels;
    for _ in 0..n / 4 {
        let r = P::blur(s, hori, &cur_uv, lo, hi, radius, coeffs.x, coeffs.y, unchecked);
        // SAFETY: `n <= span` (`rules::blur_len`).
        unsafe { P::commit::<BLEND>(ctx, fs, buf, r) };
        buf = buf.add(4);
        cur_uv.x += STEP;
    }
    n as i32
}

fn commit_gaussian_blur<S: Sampler, P: ExtPx>(
    ctx: &RasterState,
    s: &S,
    p: vec2,
    uv_rect: &vec4_scalar,
    hori: bool,
    radius: i32,
    coeffs: vec2_scalar,
    fs: &mut FragmentShaderState,
) {
    let buf = P::out(fs);
    let len = fs.swgl_SpanLength;
    let f: &FragmentShaderState = fs;
    // SAFETY: the span contract of the module doc.
    let drawn = unsafe {
        if ctx.blend_key != BLEND_KEY_NONE {
            blend_gaussian_blur::<true, S, P>(ctx, f, s, p, uv_rect, buf, len, hori, radius, coeffs)
        } else {
            blend_gaussian_blur::<false, S, P>(ctx, f, s, p, uv_rect, buf, len, hori, radius, coeffs)
        }
    };
    advance::<P>(fs, drawn);
}

/// `swgl_commitGaussianBlurRGBA8(s, p, uv_rect, hori, radius, coeffs)`.
pub fn swgl_commitGaussianBlurRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    uv_rect: vec4_scalar,
    hori: bool,
    radius: i32,
    coeffs: vec2_scalar,
    fs: &mut FragmentShaderState,
) {
    commit_gaussian_blur::<S, *mut u32>(ctx, &s, p, &uv_rect, hori, radius, coeffs, fs);
}

/// `swgl_commitGaussianBlurR8(s, p, uv_rect, hori, radius, coeffs)`.
pub fn swgl_commitGaussianBlurR8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    p: vec2,
    uv_rect: vec4_scalar,
    hori: bool,
    radius: i32,
    coeffs: vec2_scalar,
    fs: &mut FragmentShaderState,
) {
    commit_gaussian_blur::<S, *mut u8>(ctx, &s, p, &uv_rect, hori, radius, coeffs, fs);
}

// ---- clip mask, AA, blend overrides -----------------------------------------------------------------------------

/// `swgl_clipMask(mask, offset, bb_origin, bb_size)`: sample `mask` while blending, positioned at `offset` from the
/// viewport origin and constrained to the box `bb_origin`, `bb_size` (in the mask). Needs blending.
pub fn swgl_clipMask(
    ctx: &mut RasterState,
    mask: sampler2D_impl,
    offset: vec2_scalar,
    bb_origin: vec2_scalar,
    bb_size: vec2_scalar,
) {
    // glsl.h's `vec2_scalar != vec2_scalar`: either component differs (NaN differs).
    if bb_size.x != 0.0 || bb_size.y != 0.0 {
        ctx.swgl_ClipFlags |= SWGL_CLIP_FLAG_MASK;
        ctx.swgl_ClipMask = mask;
        ctx.swgl_ClipMaskOffset = make_ivec2(offset);
        ctx.swgl_ClipMaskBounds = IntRect::from_origin_size(make_ivec2(bb_origin), make_ivec2(bb_size));
    }
}

/// The edge argument of `swgl_antiAlias` (`calcAAEdgeMask`): an `int` mask, a `bool` (all edges) or a `bvec4`.
pub trait AAEdges {
    fn calc_aa_edge_mask(self) -> i32;
}
impl AAEdges for i32 {
    #[inline(always)]
    fn calc_aa_edge_mask(self) -> i32 {
        self
    }
}
impl AAEdges for bool {
    #[inline(always)]
    fn calc_aa_edge_mask(self) -> i32 {
        if self {
            0xF
        } else {
            0
        }
    }
}
impl AAEdges for bvec4_scalar {
    #[inline(always)]
    fn calc_aa_edge_mask(self) -> i32 {
        (if self.x { 1 } else { 0 }) | (if self.y { 2 } else { 0 }) | (if self.z { 4 } else { 0 }) | (if self.w { 8 } else { 0 })
    }
}

/// `swgl_antiAlias(edges)`: anti-alias the given edges of the quad. Needs blending.
pub fn swgl_antiAlias<E: AAEdges>(ctx: &mut RasterState, edges: E) {
    ctx.swgl_AAEdgeMask = edges.calc_aa_edge_mask();
    if ctx.swgl_AAEdgeMask != 0 {
        ctx.swgl_ClipFlags |= SWGL_CLIP_FLAG_AA;
    }
}

/// `swgl_blendDropShadow(color)`: premultiplied over, the source being its alpha times `color`.
pub fn swgl_blendDropShadow(ctx: &mut RasterState, color: vec4_scalar) {
    ctx.swgl_ClipFlags |= SWGL_CLIP_FLAG_BLEND_OVERRIDE;
    ctx.swgl_BlendOverride = SWGL_BLEND_DROP_SHADOW;
    ctx.swgl_BlendColorRGBA8 = pack_span(core::ptr::null_mut::<u32>(), color, 255.0);
}

/// `swgl_blendSubpixelText(color)`: premultiplied over, the source a subpixel mask times `color`.
pub fn swgl_blendSubpixelText(ctx: &mut RasterState, color: vec4_scalar) {
    ctx.swgl_ClipFlags |= SWGL_CLIP_FLAG_BLEND_OVERRIDE;
    ctx.swgl_BlendOverride = SWGL_BLEND_SUBPIXEL_TEXT;
    ctx.swgl_BlendColorRGBA8 = pack_span(core::ptr::null_mut::<u32>(), color, 255.0);
    ctx.swgl_BlendAlphaRGBA8 = alphas(ctx.swgl_BlendColorRGBA8);
}

