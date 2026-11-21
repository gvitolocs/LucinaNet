/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Port of swgl's `composite.h`: format-converting copies and nearest scaling (`scale_blit`), linear scaling
//! (`linear_blit`), both opaque and blended over the destination (`Composite`), the alpha mask (`ApplyMask`), and
//! YUV planes to BGRA (`CompositeYUV`, with `linear_row_yuv` for the span fast path), and `BlitFramebuffer`'s pixel
//! work. The GL object lookups (framebuffers, locked textures) are the GL side's; these take the textures.
//!
//! Rows are addressed with `rules::row_span` and pixels through [`Buf`], which reads zero and writes nothing
//! outside the texture's bytes. A source and a destination may be the same texture (`blit_framebuffer_same`):
//! both are then raw views of one buffer, and a row copy is a `memmove` where the C++'s `memcpy` was undefined.

// Lane loops index several arrays by the lane number, as the C++ lane expressions do.
#![allow(clippy::needless_range_loop)]

use crate::blend::{alphas, commit_blend_span, commit_blend_span_len, commit_solid_span, muldiv255};
use crate::glsl::{
    bit_cast, cast, combine, combine4, high_half, init_interp, low_half, mat3_scalar, round_pixel, shuffle,
    vec2_scalar, vec3_scalar, vec4_scalar, zip, Float, Pod, TextureFilter, TextureFormat, Vector, I16, I32, U32, V2,
    V4, V8,
};
use crate::glsl::{ivec2, sampler2DRect_impl, sampler2D_impl, vec2};
use crate::program::FragmentShaderState;
use crate::state::RasterState;
use crate::texture::{
    clampCoord, clampCoord_scalar, init_sampler, linearQuantize_scalar, load_texels, pack, prepare_texture, texelFetch,
    textureLinearPackedR8, textureLinearPackedRG8, textureLinearPackedRGBA8, textureLinearUnpackedR16,
    textureLinearUnpackedRGBA8, unpack, IntRect, PackedR8, PackedRGBA8, Texture, WideRGBA8, GL_LINEAR, GL_R8, GL_RG8,
    GL_RGBA8,
};
use lucinanet_raster_rules as rules;

// ---- raw views of texture bytes ---------------------------------------------------------------------------

/// A texture's bytes as a raw view: loads outside read zero (`texture::load_texels`), stores outside are dropped.
#[derive(Clone, Copy, Debug)]
pub struct Buf {
    ptr: *mut u8,
    len: usize,
}

impl Buf {
    /// The bytes of `t`. The view is valid while `t` is neither reallocated nor dropped.
    pub fn of(t: &mut Texture) -> Buf {
        let b = t.bytes_mut();
        Buf { ptr: b.as_mut_ptr(), len: b.len() }
    }
    #[inline(always)]
    fn ld<T: Pod>(self, off: isize) -> T {
        // SAFETY: `Buf::of` took `ptr` and `len` from the texture's byte slice.
        unsafe { load_texels(self.ptr, self.len, off) }
    }
    #[inline(always)]
    fn st<T: Pod>(self, off: isize, v: T) {
        if off >= 0 && (off as usize) <= self.len && self.len - off as usize >= T::SIZE {
            // SAFETY: `[off, off + T::SIZE)` is within the view's `len` bytes (checked above), which `Buf::of`
            // took from the texture's buffer.
            unsafe { crate::glsl::unaligned_store(self.ptr.offset(off), v) }
        } else {
            debug_assert!(false, "store outside the texture at {off}");
        }
    }
    /// `memmove` of `n` bytes from `src` to `dst` within the views (which may be the same buffer).
    fn copy(dst: Buf, dst_off: isize, src: Buf, src_off: isize, n: usize) {
        let ok = |b: Buf, o: isize| o >= 0 && (o as usize) <= b.len && b.len - o as usize >= n;
        if ok(dst, dst_off) && ok(src, src_off) {
            // SAFETY: both ranges lie within their views (checked above); `copy` allows them to overlap.
            unsafe { core::ptr::copy(src.ptr.offset(src_off), dst.ptr.offset(dst_off), n) }
        } else {
            for k in 0..n as isize {
                dst.st::<u8>(dst_off + k, src.ld::<u8>(src_off + k));
            }
        }
    }
}

/// Byte offset of `[x0, x1)` in row `y` of a `w` x `h` texture, from `rules::row_span`.
fn row_at(t: &Texture, x0: i32, x1: i32, y: i32) -> Option<isize> {
    rules::row_span(x0 as i64, x1 as i64, y as i64, t.bpp() as i64, t.stride() as i64, t.width as i64, t.height as i64)
        .map(|(a, _)| a as isize)
}

// ---- convert_pixel ------------------------------------------------------------------------------------------

/// A pixel of 1, 2 or 4 bytes (R8, RG8, RGBA8).
pub trait BlitPixel: Copy + Pod + Default {
    /// `convert_pixel<Self>(src)`.
    fn convert_from<U: BlitPixel>(src: U) -> Self;
    fn as_u32(self) -> u32;
    const BYTES: usize;
}

impl BlitPixel for u8 {
    const BYTES: usize = 1;
    #[inline(always)]
    fn convert_from<U: BlitPixel>(src: U) -> u8 {
        let s = src.as_u32();
        match U::BYTES {
            4 => ((s >> 16) & 0xFF) as u8, // RGBA8 maps to R
            2 => (s & 0xFF) as u8,         // RG8 maps to R
            _ => s as u8,
        }
    }
    fn as_u32(self) -> u32 {
        self as u32
    }
}

impl BlitPixel for u16 {
    const BYTES: usize = 2;
    #[inline(always)]
    fn convert_from<U: BlitPixel>(src: U) -> u16 {
        let s = src.as_u32();
        match U::BYTES {
            4 => (((s >> 16) & 0x00FF) | (s & 0xFF00)) as u16, // RGBA8 maps to R, G
            1 => s as u16,                                     // R8 maps to R, 0
            _ => s as u16,
        }
    }
    fn as_u32(self) -> u32 {
        self as u32
    }
}

impl BlitPixel for u32 {
    const BYTES: usize = 4;
    #[inline(always)]
    fn convert_from<U: BlitPixel>(src: U) -> u32 {
        let s = src.as_u32();
        match U::BYTES {
            1 => (s << 16) | 0xFF00_0000,                           // R8 maps to BGRA 0, 0, R, 1
            2 => ((s & 0x00FF) << 16) | (s & 0xFF00) | 0xFF00_0000, // RG8 maps to BGRA 0, G, R, 1
            _ => s,
        }
    }
    fn as_u32(self) -> u32 {
        self
    }
}

// ---- mask_blit ------------------------------------------------------------------------------------------------

#[inline(always)]
fn ld_partial<P: Pod + Default, V: Pod>(b: Buf, off: isize, span: usize, elem: usize) -> V {
    // `partial_load_span<V, P>`: pixels 0..span (1 to 3), zero after.
    let mut bytes = [0u8; 16];
    for k in 0..span.min(3) {
        let p: P = b.ld(off + (k * elem) as isize);
        crate::glsl::store_bytes(&mut bytes[k * elem..], p);
    }
    crate::glsl::load_bytes(&bytes[..V::SIZE])
}

#[inline(always)]
fn st_partial<P: Pod, V: Pod>(b: Buf, off: isize, v: V, span: usize, elem: usize) {
    let mut bytes = [0u8; 16];
    crate::glsl::store_bytes(&mut bytes[..V::SIZE], v);
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
        let p: P = crate::glsl::load_bytes(&bytes[k * elem..]);
        b.st(off + (k * elem) as isize, p);
    }
}

#[inline(always)]
fn expand_mask_rgba8(mask: crate::texture::WideR8) -> WideRGBA8 {
    let rg: V8<u16> = zip(mask, mask);
    zip(rg, rg)
}

/// `mask_row(dst, mask, span)`: multiply a row of RGBA8 by an R8 mask.
fn mask_row(dst: Buf, mut d: isize, mask: Buf, mut m: isize, span: i32) {
    let end = d + span as isize * 4;
    while d + 16 <= end {
        let maskpx = expand_mask_rgba8(unpack(mask.ld::<PackedR8>(m)));
        let dstpx = unpack(dst.ld::<PackedRGBA8>(d));
        dst.st(d, pack(muldiv255(dstpx, maskpx)));
        m += 4;
        d += 16;
    }
    if d < end {
        let n = ((end - d) / 4) as usize;
        let maskpx = expand_mask_rgba8(unpack(ld_partial::<u8, PackedR8>(mask, m, n, 1)));
        let dstpx = unpack(ld_partial::<u32, PackedRGBA8>(dst, d, n, 4));
        // The C++ tail adds the mask (`maskpx + dstpx - muldiv255(dstpx, maskpx)`), unlike the full chunks.
        let r = pack(maskpx + dstpx - muldiv255(dstpx, maskpx));
        st_partial::<u32, PackedRGBA8>(dst, d, r, n, 4);
    }
}

/// `mask_blit(masktex, dsttex)`: apply an R8 alpha mask to an RGBA8 texture of the same size.
pub fn mask_blit(masktex: &mut Texture, dsttex: &mut Texture) {
    let mask_stride = masktex.stride() as isize;
    let dest_stride = dsttex.stride() as isize;
    let span = dsttex.width;
    let (mb, db) = (Buf::of(masktex), Buf::of(dsttex));
    let (mut dest, mut mask) = (0isize, 0isize);
    for _ in 0..dsttex.height.max(0) {
        mask_row(db, dest, mb, mask, span);
        dest += dest_stride;
        mask += mask_stride;
    }
}

// ---- scale_blit -----------------------------------------------------------------------------------------------

/// `copy_row<COMPOSITE, P>`: a straight copy, or for `COMPOSITE` RGBA8 a premultiplied source-over.
fn copy_row<const COMPOSITE: bool, P: BlitPixel>(db: Buf, dst: isize, sb: Buf, src: isize, span: i32) {
    if !(COMPOSITE && P::BYTES == 4) {
        Buf::copy(db, dst, sb, src, span.max(0) as usize * P::BYTES);
        return;
    }
    let (mut d, mut s) = (dst, src);
    let end = dst + span as isize * 4;
    while d + 16 <= end {
        let srcpx = unpack(sb.ld::<PackedRGBA8>(s));
        let dstpx = unpack(db.ld::<PackedRGBA8>(d));
        db.st(d, pack(srcpx + dstpx - muldiv255(dstpx, alphas(srcpx))));
        s += 16;
        d += 16;
    }
    if d < end {
        let n = ((end - d) / 4) as usize;
        let srcpx = unpack(ld_partial::<u32, PackedRGBA8>(sb, s, n, 4));
        let dstpx = unpack(ld_partial::<u32, PackedRGBA8>(db, d, n, 4));
        st_partial::<u32, PackedRGBA8>(db, d, pack(srcpx + dstpx - muldiv255(dstpx, alphas(srcpx))), n, 4);
    }
}

/// `scale_row<COMPOSITE, P, U>`: nearest scaling of a row, `frac` stepping the source by `src_width / dst_width`.
#[allow(clippy::too_many_arguments)]
fn scale_row<const COMPOSITE: bool, P: BlitPixel, U: BlitPixel>(
    db: Buf,
    dst: isize,
    dst_width: i32,
    sb: Buf,
    src: isize,
    src_width: i32,
    span: i32,
    mut frac: i32,
) {
    let mut s = src;
    let step = |frac: &mut i32, s: &mut isize| {
        *frac = frac.wrapping_add(src_width);
        while *frac >= dst_width {
            *s += U::BYTES as isize;
            *frac -= dst_width;
        }
    };
    if COMPOSITE && P::BYTES == 4 && U::BYTES == 4 {
        // `scale_row<true, uint32_t, uint32_t>`: gather four source pixels, then composite.
        let mut d = dst;
        let end = dst + span as isize * 4;
        while d + 16 <= end {
            let mut srcn = [0u32; 4];
            for p in srcn.iter_mut() {
                *p = sb.ld(s);
                step(&mut frac, &mut s);
            }
            let srcpx = unpack(bit_cast::<PackedRGBA8, U32>(U32::new(srcn)));
            let dstpx = unpack(db.ld::<PackedRGBA8>(d));
            db.st(d, pack(srcpx + dstpx - muldiv255(dstpx, alphas(srcpx))));
            d += 16;
        }
        if d < end {
            let left = (end - d) / 4;
            let mut srcn = [sb.ld::<u32>(s), 0, 0, 0];
            if left > 1 {
                step(&mut frac, &mut s);
                srcn[1] = sb.ld(s);
                if left > 2 {
                    step(&mut frac, &mut s);
                    srcn[2] = sb.ld(s);
                }
            }
            let srcpx = unpack(bit_cast::<PackedRGBA8, U32>(U32::new(srcn)));
            let n = left as usize;
            let dstpx = unpack(ld_partial::<u32, PackedRGBA8>(db, d, n, 4));
            st_partial::<u32, PackedRGBA8>(db, d, pack(srcpx + dstpx - muldiv255(dstpx, alphas(srcpx))), n, 4);
        }
        return;
    }
    for k in 0..span.max(0) as isize {
        let px: U = sb.ld(s);
        db.st(dst + k * P::BYTES as isize, P::convert_from(px));
        step(&mut frac, &mut s);
    }
}

/// Whether one axis is inside the preconditions of `rules::blit` (sizes `1..=MAX`, coordinates within `±MAX`).
fn in_blit_range(src: i32, dst: i32, coords: &[i32]) -> bool {
    let max = rules::blit::MAX;
    (1..=max).contains(&(src as i64))
        && (1..=max).contains(&(dst as i64))
        && coords.iter().all(|&c| (-max..=max).contains(&(c as i64)))
}

/// `srcClip.scale(srcWidth, srcHeight, dstWidth, dstHeight, true)` as `scale_blit` uses it, through the
/// Verus-proved `rules::blit::scale_clip` on each axis. That cuts the start and the end at 0 where the C++ divides
/// a non-positive coordinate with truncation; the destination bounds it is intersected with start at 0 or later
/// (`sample_bounds` is relative to the request), so the intersection, and the blit, are the same. Outside the
/// proof's range (which the C++ would overflow or divide by zero in) the `int` arithmetic of `IntRect::scale` stays.
fn scale_clip(src_clip: &IntRect, src_width: i32, src_height: i32, dst_width: i32, dst_height: i32) -> IntRect {
    let c = src_clip;
    if !in_blit_range(src_width, dst_width, &[c.x0, c.x1]) || !in_blit_range(src_height, dst_height, &[c.y0, c.y1]) {
        let mut r = *c;
        r.scale(src_width, src_height, dst_width, dst_height, true);
        return r;
    }
    let (x0, x1) = rules::blit::scale_clip(c.x0 as i64, c.x1 as i64, src_width as i64, dst_width as i64);
    let (y0, y1) = rules::blit::scale_clip(c.y0 as i64, c.y1 as i64, src_height as i64, dst_height as i64);
    IntRect::new(x0 as i32, y0 as i32, x1 as i32, y1 as i32)
}

/// `frac = src * x; frac / dst` and `frac % dst` (`scale_blit`'s start in the source and its step remainder), the
/// quotient through the proved `rules::blit::source_of`. `x` is a destination bound, so `>= 0`; outside the proof's
/// range the `int` arithmetic stays.
fn source_of(x: i32, src: i32, dst: i32) -> (i32, i32) {
    if !in_blit_range(src, dst, &[x]) || x < 0 {
        let frac = src.wrapping_mul(x);
        return if dst == 0 { (0, 0) } else { (frac.wrapping_div(dst), frac.wrapping_rem(dst)) };
    }
    let q = rules::blit::source_of(x as i64, src as i64, dst as i64);
    (q as i32, (x as i64 * src as i64 - q * dst as i64) as i32)
}

/// `scale_blit<COMPOSITE>(srctex, srcReq, dsttex, dstReq, invertY, clipRect)` on raw views, `src` and `dst`
/// possibly the same texture.
#[allow(clippy::too_many_arguments)]
fn scale_blit_views<const COMPOSITE: bool>(
    srctex: &Texture,
    sb: Buf,
    src_req: &IntRect,
    dsttex: &Texture,
    db: Buf,
    dst_req: &IntRect,
    invert_y: bool,
    clip_rect: &IntRect,
) {
    let src_width = src_req.width();
    let src_height = src_req.height();
    let dst_width = dst_req.width();
    let dst_height = dst_req.height();
    let mut dst_bounds = dsttex.sample_bounds(dst_req, false);
    dst_bounds.intersect(clip_rect);
    let mut src_bounds = srctex.sample_bounds(src_req, invert_y);
    // Never sample outside the source: its bounds relative to srcReq, scaled to the destination, rounded inward.
    let mut src_clip = srctex.bounds() - src_req.origin();
    if invert_y {
        src_clip.invert_y(src_req.height());
    }
    src_clip = scale_clip(&src_clip, src_width, src_height, dst_width, dst_height);
    dst_bounds.intersect(&src_clip);
    if dst_bounds.is_empty() {
        return;
    }
    let mut src_stride = srctex.stride() as isize;
    let dest_stride = dsttex.stride() as isize;
    let x0 = dst_req.x0 + dst_bounds.x0;
    let y0 = dst_req.y0 + dst_bounds.y0;
    let Some(mut dest) = row_at(dsttex, x0, x0 + dst_bounds.width(), y0) else {
        debug_assert!(false, "scale_blit destination outside the texture");
        return;
    };
    let (sx0, frac_x) = source_of(dst_bounds.x0, src_width, dst_width);
    let (sy0, mut frac_y) = source_of(dst_bounds.y0, src_height, dst_height);
    src_bounds.x0 = sx0.max(src_bounds.x0);
    src_bounds.y0 = sy0.max(src_bounds.y0);
    let mut src = srctex.sample_offset_req(src_req, &src_bounds, invert_y);
    if invert_y {
        src_stride = -src_stride;
    }
    let span = dst_bounds.width();
    let (sbpp, dbpp) = (srctex.bpp(), dsttex.bpp());
    for _ in 0..dst_bounds.height() {
        match (sbpp, dbpp) {
            (1, 2) => scale_row::<COMPOSITE, u16, u8>(db, dest, dst_width, sb, src, src_width, span, frac_x),
            (1, 4) => scale_row::<COMPOSITE, u32, u8>(db, dest, dst_width, sb, src, src_width, span, frac_x),
            (1, _) if src_width == dst_width => copy_row::<COMPOSITE, u8>(db, dest, sb, src, span),
            (1, _) => scale_row::<COMPOSITE, u8, u8>(db, dest, dst_width, sb, src, src_width, span, frac_x),
            (2, 1) => scale_row::<COMPOSITE, u8, u16>(db, dest, dst_width, sb, src, src_width, span, frac_x),
            (2, 4) => scale_row::<COMPOSITE, u32, u16>(db, dest, dst_width, sb, src, src_width, span, frac_x),
            (2, _) if src_width == dst_width => copy_row::<COMPOSITE, u16>(db, dest, sb, src, span),
            (2, _) => scale_row::<COMPOSITE, u16, u16>(db, dest, dst_width, sb, src, src_width, span, frac_x),
            (4, 1) => scale_row::<COMPOSITE, u8, u32>(db, dest, dst_width, sb, src, src_width, span, frac_x),
            (4, 2) => scale_row::<COMPOSITE, u16, u32>(db, dest, dst_width, sb, src, src_width, span, frac_x),
            (4, _) if src_width == dst_width => copy_row::<COMPOSITE, u32>(db, dest, sb, src, span),
            (4, _) => scale_row::<COMPOSITE, u32, u32>(db, dest, dst_width, sb, src, src_width, span, frac_x),
            _ => debug_assert!(false, "scale_blit of {sbpp}-byte texels"),
        }
        dest += dest_stride;
        frac_y = frac_y.wrapping_add(src_height);
        while frac_y >= dst_height {
            src += src_stride;
            frac_y -= dst_height;
        }
    }
}

/// `scale_blit<COMPOSITE>`: nearest scaling (or a copy) of `src_req` of `srctex` to `dst_req` of `dsttex`.
pub fn scale_blit<const COMPOSITE: bool>(
    srctex: &mut Texture,
    src_req: &IntRect,
    dsttex: &mut Texture,
    dst_req: &IntRect,
    invert_y: bool,
    clip_rect: &IntRect,
) {
    debug_assert!(!COMPOSITE || (srctex.internal_format == GL_RGBA8 && dsttex.internal_format == GL_RGBA8));
    let (sb, db) = (Buf::of(srctex), Buf::of(dsttex));
    scale_blit_views::<COMPOSITE>(srctex, sb, src_req, dsttex, db, dst_req, invert_y, clip_rect);
}

// ---- linear_blit ----------------------------------------------------------------------------------------------

/// A destination format of `linear_row_blit`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowFormat {
    R8,
    RG8,
    RGBA8,
}

/// `linear_row_blit<COMPOSITE>(dest, span, srcUV, srcDU, sampler)` for an R8, RG8 or RGBA8 row.
fn linear_row_blit<const COMPOSITE: bool>(
    db: Buf,
    mut dest: isize,
    fmt: RowFormat,
    mut span: i32,
    src_uv: vec2_scalar,
    src_du: f32,
    sampler: &sampler2D_impl,
) {
    let mut uv = vec2::new(init_interp(src_uv.x, src_du), init_interp(src_uv.y, 0.0f32));
    let bpp: isize = match fmt {
        RowFormat::R8 => 1,
        RowFormat::RG8 => 2,
        RowFormat::RGBA8 => 4,
    };
    let chunk = |db: Buf, dest: isize, uv: vec2, n: i32| {
        let i = ivec2::from_vec2(uv);
        let n = n as usize;
        match fmt {
            RowFormat::RGBA8 if COMPOSITE => {
                let srcpx = textureLinearUnpackedRGBA8(sampler, i);
                if n >= 4 {
                    let dstpx = unpack(db.ld::<PackedRGBA8>(dest));
                    db.st(dest, pack(srcpx + dstpx - muldiv255(dstpx, alphas(srcpx))));
                } else {
                    let dstpx = unpack(ld_partial::<u32, PackedRGBA8>(db, dest, n, 4));
                    st_partial::<u32, PackedRGBA8>(
                        db,
                        dest,
                        pack(srcpx + dstpx - muldiv255(dstpx, alphas(srcpx))),
                        n,
                        4,
                    );
                }
            }
            RowFormat::RGBA8 => {
                let srcpx = textureLinearPackedRGBA8(sampler, i);
                if n >= 4 {
                    db.st(dest, srcpx);
                } else {
                    st_partial::<u32, PackedRGBA8>(db, dest, srcpx, n, 4);
                }
            }
            RowFormat::R8 => {
                let srcpx = textureLinearPackedR8(sampler, i);
                if n >= 4 {
                    db.st(dest, srcpx);
                } else {
                    st_partial::<u8, PackedR8>(db, dest, srcpx, n, 1);
                }
            }
            RowFormat::RG8 => {
                let srcpx = textureLinearPackedRG8(sampler, i);
                if n >= 4 {
                    db.st(dest, srcpx);
                } else {
                    st_partial::<u16, V8<u8>>(db, dest, srcpx, n, 2);
                }
            }
        }
    };
    while span >= 4 {
        chunk(db, dest, uv, 4);
        dest += 4 * bpp;
        uv.x += 4.0f32 * src_du;
        span -= 4;
    }
    if span > 0 {
        chunk(db, dest, uv, span);
    }
}

#[allow(clippy::too_many_arguments)]
fn linear_blit_views<const COMPOSITE: bool>(
    sampler: &sampler2D_impl,
    src_req: &IntRect,
    dsttex: &Texture,
    db: Buf,
    dst_req: &IntRect,
    invert_x: bool,
    invert_y: bool,
    clip_rect: &IntRect,
) {
    let mut dst_bounds = dsttex.sample_bounds(dst_req, false);
    dst_bounds.intersect(clip_rect);
    if dst_bounds.is_empty() {
        return;
    }
    let mut src_uv = vec2_scalar::new(src_req.x0 as f32, src_req.y0 as f32);
    let mut src_duv = vec2_scalar::new(
        src_req.width() as f32 / dst_req.width() as f32,
        src_req.height() as f32 / dst_req.height() as f32,
    );
    if invert_x {
        src_uv.x += src_req.width() as f32;
        src_duv.x = -src_duv.x;
    }
    if invert_y {
        src_uv.y += src_req.height() as f32;
        src_duv.y = -src_duv.y;
    }
    src_uv.x += src_duv.x * (dst_bounds.x0 as f32 + 0.5f32);
    src_uv.y += src_duv.y * (dst_bounds.y0 as f32 + 0.5f32);
    let mut src_uv = linearQuantize_scalar(src_uv, 128.0);
    src_duv.x *= 128.0f32;
    src_duv.y *= 128.0f32;
    let fmt = match dsttex.bpp() {
        1 => RowFormat::R8,
        2 => RowFormat::RG8,
        4 => RowFormat::RGBA8,
        b => {
            debug_assert!(false, "linear_blit to {b}-byte texels");
            return;
        }
    };
    let dest_stride = dsttex.stride() as isize;
    let x0 = dst_req.x0 + dst_bounds.x0;
    let y0 = dst_req.y0 + dst_bounds.y0;
    let Some(mut dest) = row_at(dsttex, x0, x0 + dst_bounds.width(), y0) else {
        debug_assert!(false, "linear_blit destination outside the texture");
        return;
    };
    let span = dst_bounds.width();
    for _ in 0..dst_bounds.height() {
        linear_row_blit::<COMPOSITE>(db, dest, fmt, span, src_uv, src_duv.x, sampler);
        dest += dest_stride;
        src_uv.y += src_duv.y;
    }
}

/// `linear_blit<COMPOSITE>`: bilinear scaling of `src_req` of `srctex` (R8, RG8 or RGBA8) to `dst_req`.
#[allow(clippy::too_many_arguments)]
pub fn linear_blit<const COMPOSITE: bool>(
    srctex: &mut Texture,
    src_req: &IntRect,
    dsttex: &mut Texture,
    dst_req: &IntRect,
    invert_x: bool,
    invert_y: bool,
    clip_rect: &IntRect,
) {
    debug_assert!(matches!(srctex.internal_format, GL_RGBA8 | GL_R8 | GL_RG8));
    debug_assert!(!COMPOSITE || (srctex.internal_format == GL_RGBA8 && dsttex.internal_format == GL_RGBA8));
    let mut dst_bounds = dsttex.sample_bounds(dst_req, false);
    dst_bounds.intersect(clip_rect);
    if dst_bounds.is_empty() {
        return;
    }
    let mut sampler = sampler2D_impl::default();
    init_sampler(&mut sampler, srctex);
    sampler.filter.filter = TextureFilter::LINEAR;
    let db = Buf::of(dsttex);
    linear_blit_views::<COMPOSITE>(&sampler, src_req, dsttex, db, dst_req, invert_x, invert_y, clip_rect);
}

/// `is_renderable_format(format)`.
pub fn is_renderable_format(format: u32) -> bool {
    matches!(format, GL_R8 | GL_RG8 | GL_RGBA8)
}

// ---- BlitFramebuffer, Composite, ApplyMask --------------------------------------------------------------------

/// The arguments of `BlitFramebuffer` after the framebuffer lookup.
#[derive(Clone, Copy, Debug)]
struct BlitArgs {
    src_req: IntRect,
    dst_req: IntRect,
    invert_y: bool,
    linear: bool,
}

#[allow(clippy::too_many_arguments)]
fn blit_args(
    srctex: &Texture,
    dsttex: &Texture,
    src_x0: i32,
    mut src_y0: i32,
    src_x1: i32,
    mut src_y1: i32,
    dst_x0: i32,
    mut dst_y0: i32,
    dst_x1: i32,
    mut dst_y1: i32,
    filter: u32,
) -> Option<BlitArgs> {
    if srctex.buf.is_null() || dsttex.buf.is_null() {
        return None;
    }
    if srctex.internal_format != dsttex.internal_format
        && (!is_renderable_format(srctex.internal_format) || !is_renderable_format(dsttex.internal_format))
    {
        // Converting between these formats is not supported (the C++ asserts).
        return None;
    }
    // Force flipped Y onto dest coordinates.
    if src_y1 < src_y0 {
        core::mem::swap(&mut src_y0, &mut src_y1);
        core::mem::swap(&mut dst_y0, &mut dst_y1);
    }
    let invert_y = dst_y1 < dst_y0;
    if invert_y {
        core::mem::swap(&mut dst_y0, &mut dst_y1);
    }
    let src_req = IntRect::new(src_x0, src_y0, src_x1, src_y1) - srctex.offset;
    let dst_req = IntRect::new(dst_x0, dst_y0, dst_x1, dst_y1) - dsttex.offset;
    if src_req.is_empty() || dst_req.is_empty() {
        return None;
    }
    let linear = !src_req.same_size(&dst_req)
        && srctex.width >= 2
        && filter == GL_LINEAR
        && srctex.internal_format == dsttex.internal_format
        && is_renderable_format(srctex.internal_format);
    Some(BlitArgs { src_req, dst_req, invert_y, linear })
}

/// `BlitFramebuffer`'s pixel work, from the format check on: the color attachments `srctex` and `dsttex` of the
/// read and draw framebuffers. `filter` is `GL_NEAREST` or `GL_LINEAR`.
#[allow(clippy::too_many_arguments)]
pub fn blit_framebuffer(
    srctex: &mut Texture,
    dsttex: &mut Texture,
    src_x0: i32,
    src_y0: i32,
    src_x1: i32,
    src_y1: i32,
    dst_x0: i32,
    dst_y0: i32,
    dst_x1: i32,
    dst_y1: i32,
    filter: u32,
) {
    let Some(a) = blit_args(srctex, dsttex, src_x0, src_y0, src_x1, src_y1, dst_x0, dst_y0, dst_x1, dst_y1, filter)
    else {
        return;
    };
    let clip_rect = IntRect::new(0, 0, a.dst_req.width(), a.dst_req.height());
    prepare_texture(srctex, None);
    prepare_texture(dsttex, Some(&a.dst_req));
    if a.linear {
        // swgl passes dstReq as the clip rectangle here (not clipRect); kept.
        linear_blit::<false>(srctex, &a.src_req, dsttex, &a.dst_req, false, a.invert_y, &a.dst_req);
    } else {
        scale_blit::<false>(srctex, &a.src_req, dsttex, &a.dst_req, a.invert_y, &clip_rect);
    }
}

/// [`blit_framebuffer`] when the read and draw framebuffers share their color attachment.
#[allow(clippy::too_many_arguments)]
pub fn blit_framebuffer_same(
    tex: &mut Texture,
    src_x0: i32,
    src_y0: i32,
    src_x1: i32,
    src_y1: i32,
    dst_x0: i32,
    dst_y0: i32,
    dst_x1: i32,
    dst_y1: i32,
    filter: u32,
) {
    let Some(a) = blit_args(tex, tex, src_x0, src_y0, src_x1, src_y1, dst_x0, dst_y0, dst_x1, dst_y1, filter) else {
        return;
    };
    let clip_rect = IntRect::new(0, 0, a.dst_req.width(), a.dst_req.height());
    prepare_texture(tex, None);
    prepare_texture(tex, Some(&a.dst_req));
    let b = Buf::of(tex);
    if a.linear {
        let mut sampler = sampler2D_impl::default();
        init_sampler(&mut sampler, tex);
        sampler.filter.filter = TextureFilter::LINEAR;
        linear_blit_views::<false>(&sampler, &a.src_req, tex, b, &a.dst_req, false, a.invert_y, &a.dst_req);
    } else {
        scale_blit_views::<false>(tex, b, &a.src_req, tex, b, &a.dst_req, a.invert_y, &clip_rect);
    }
}

/// `Composite(lockedDst, lockedSrc, ...)`: RGBA8 `src` rect onto `dst` rect, opaque (copy) or premultiplied
/// source-over, scaled nearest or linear, flipped, clipped to the clip rect.
#[allow(clippy::too_many_arguments)]
pub fn Composite(
    dsttex: &mut Texture,
    srctex: &mut Texture,
    src_x: i32,
    src_y: i32,
    src_width: i32,
    src_height: i32,
    dst_x: i32,
    dst_y: i32,
    dst_width: i32,
    dst_height: i32,
    opaque: bool,
    flip_x: bool,
    flip_y: bool,
    filter: u32,
    clip_x: i32,
    clip_y: i32,
    clip_width: i32,
    clip_height: i32,
) {
    debug_assert!(srctex.bpp() == 4);
    debug_assert!(dsttex.bpp() == 4);
    let src_req = IntRect::new(src_x, src_y, src_x + src_width, src_y + src_height) - srctex.offset;
    let dst_req = IntRect::new(dst_x, dst_y, dst_x + dst_width, dst_y + dst_height) - dsttex.offset;
    if src_req.is_empty() || dst_req.is_empty() {
        return;
    }
    // The clip rect relative to dstReq, the coordinates of the sampling bounds.
    let clip_rect =
        IntRect::new(clip_x - dst_x, clip_y - dst_y, clip_x - dst_x + clip_width, clip_y - dst_y + clip_height);
    // Linear needs rows of at least 2 pixels; X flips always go linear.
    let use_linear = srctex.width >= 2 && (flip_x || (!src_req.same_size(&dst_req) && filter == GL_LINEAR));
    match (opaque, use_linear) {
        (true, true) => linear_blit::<false>(srctex, &src_req, dsttex, &dst_req, flip_x, flip_y, &clip_rect),
        (true, false) => scale_blit::<false>(srctex, &src_req, dsttex, &dst_req, flip_y, &clip_rect),
        (false, true) => linear_blit::<true>(srctex, &src_req, dsttex, &dst_req, flip_x, flip_y, &clip_rect),
        (false, false) => scale_blit::<true>(srctex, &src_req, dsttex, &dst_req, flip_y, &clip_rect),
    }
}

/// `ApplyMask(lockedDst, lockedMask)`: multiply an RGBA8 texture by an R8 mask of the same size.
pub fn ApplyMask(dsttex: &mut Texture, masktex: &mut Texture) {
    debug_assert!(masktex.bpp() == 1);
    debug_assert!(dsttex.bpp() == 4);
    debug_assert!(masktex.width == dsttex.width && masktex.height == dsttex.height);
    mask_blit(masktex, dsttex);
}

// ---- YUV ------------------------------------------------------------------------------------------------------

/// `addsat(V8<int16_t>, V8<int16_t>)`: signed saturating (`_mm_adds_epi16`, `vqaddq_s16`, the portable form).
#[inline(always)]
pub fn addsat_i16(x: V8<i16>, y: V8<i16>) -> V8<i16> {
    // `_mm_adds_epi16` / `vqaddq_s16`, as swgl's addsat; the same saturating lanes as the loop below.
    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::x86_64::*;
        let mut r = V8::<i16>::default();
        // SAFETY: SSE2 is part of x86_64; the vectors are 16 bytes (unaligned loads and stores).
        unsafe {
            let v = _mm_adds_epi16(_mm_loadu_si128(x.0.as_ptr() as *const __m128i), _mm_loadu_si128(y.0.as_ptr() as *const __m128i));
            _mm_storeu_si128(r.0.as_mut_ptr() as *mut __m128i, v);
        }
        return r;
    }
    #[cfg(target_arch = "aarch64")]
    {
        use core::arch::aarch64::*;
        let mut r = V8::<i16>::default();
        // SAFETY: NEON is part of aarch64; the vectors are 16 bytes, 2-byte aligned or more.
        unsafe { vst1q_s16(r.0.as_mut_ptr(), vqaddq_s16(vld1q_s16(x.0.as_ptr()), vld1q_s16(y.0.as_ptr()))) };
        return r;
    }
    #[allow(unreachable_code)]
    let mut r = [0i16; 8];
    for k in 0..8 {
        r[k] = x.0[k].saturating_add(y.0[k]);
    }
    Vector(r)
}

/// `packYUV(gg, br)`: interleave B/R with G/opaque and pack to BGRA8.
#[inline(always)]
pub fn packYUV(gg: V8<i16>, br: V8<i16>) -> PackedRGBA8 {
    let opaque = PackedRGBA8::new([0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]);
    pack(bit_cast::<WideRGBA8, Vector<i16, 16>>(zip(br, gg))) | opaque
}

/// `YUVMatrix`: a YCbCr to RGB matrix in fixed point (6 fractional bits, 7 for Y).
#[derive(Clone, Copy, Debug)]
pub struct YUVMatrix {
    pub br_uvCoeffs: V8<i16>,
    pub gg_uvCoeffs: V8<i16>,
    pub yCoeffs: V8<u16>,
    pub yBias: V8<i16>,
    pub uvBias: V8<i16>,
    pub br_yMask: V8<i16>,
}

/// C++ `int16_t(double)`: through a 32-bit truncating conversion, then the low 16 bits.
#[inline(always)]
fn d_to_i16(d: f64) -> i16 {
    d_to_i32(d) as i16
}

#[inline(always)]
fn d_to_i32(d: f64) -> i32 {
    #[cfg(target_arch = "x86_64")]
    {
        if (-2147483648.0..2147483648.0).contains(&d) {
            d as i32
        } else {
            i32::MIN
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        d as i32
    }
}

impl YUVMatrix {
    /// `YUVMatrix::From` (the C++ name).
    pub fn From(ycbcr_bias: &vec3_scalar, m: &mat3_scalar, rescale_factor: i32) -> YUVMatrix {
        Self::from_matrix(ycbcr_bias, m, rescale_factor)
    }

    /// `YUVMatrix::From(ycbcr_bias, rgb_from_debiased_ycbcr, rescale_factor)`.
    pub fn from_matrix(ycbcr_bias: &vec3_scalar, m: &mat3_scalar, rescale_factor: i32) -> YUVMatrix {
        debug_assert!(ycbcr_bias.z == ycbcr_bias.y);
        let rgb_from_y = m[0].y;
        let br_from_y_mask: i16 = if m[0].x == 0.0 { 0 } else { -1 };
        let g_from_u = m[1].y;
        let b_from_u = m[1].z;
        let r_from_v = m[2].x;
        let g_from_v = m[2].y;
        YUVMatrix::new(
            vec2_scalar::new(ycbcr_bias.x, ycbcr_bias.y),
            rgb_from_y as f64,
            br_from_y_mask,
            r_from_v as f64,
            g_from_u as f64,
            g_from_v as f64,
            b_from_u as f64,
            rescale_factor,
        )
    }

    /// `YUVMatrix(yuv_bias, yCoeff, br_yMask, rv, gu, gv, bu, rescale_factor)`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        yuv_bias: vec2_scalar,
        y_coeff: f64,
        br_y_mask: i16,
        rv: f64,
        gu: f64,
        gv: f64,
        bu: f64,
        rescale_factor: i32,
    ) -> YUVMatrix {
        let s6 = (1i32 << (6 - rescale_factor)) as f64;
        let s7 = (1i32 << (6 + 1 - rescale_factor)) as f64;
        let br: V8<i16> = zip(I16::splat(d_to_i16(bu * s6 + 0.5)), I16::splat(d_to_i16(rv * s6 + 0.5)));
        // Negative coefficients, rounded away from zero.
        let gg: V8<i16> = zip(
            I16::splat(d_to_i16(-gu * s6 + 0.5).wrapping_neg()),
            I16::splat(d_to_i16(-gv * s6 + 0.5).wrapping_neg()),
        );
        let y_coeffs = V8::<u16>::splat(d_to_i32(y_coeff * s7 + 0.5) as u16);
        // The -0.5 fudge factor for the Y bias approximates rounding in `gg >>= 6` (white is 255, not 254).
        let y_bias = V8::<i16>::splat(d_to_i16((((yuv_bias.x * 255.0f32) as f64 * y_coeff) - 0.5) * 64.0));
        let uv_bias = V8::<i16>::splat(d_to_i16((yuv_bias.y * ((255i32 << rescale_factor) as f32)) as f64 + 0.5));
        YUVMatrix {
            br_uvCoeffs: br,
            gg_uvCoeffs: gg,
            yCoeffs: y_coeffs,
            yBias: y_bias,
            uvBias: uv_bias,
            br_yMask: V8::splat(br_y_mask),
        }
    }

    /// `convert(yy, uv)`: Y (doubled) and interleaved U/V lanes of 4 pixels to BGRA8.
    #[inline(always)]
    pub fn convert(&self, yy: V8<i16>, uv: V8<i16>) -> PackedRGBA8 {
        let yy: V8<i16> = bit_cast((bit_cast::<V8<u16>, V8<i16>>(yy) * self.yCoeffs) >> 1);
        let yy = yy - self.yBias;
        let uv = uv - self.uvBias;
        let br = self.br_uvCoeffs * uv;
        let br = addsat_i16(yy & self.br_yMask, br) >> 6;
        let gg = self.gg_uvCoeffs * uv;
        let gg = addsat_i16(gg, bit_cast::<V8<i16>, V4<u32>>(bit_cast::<V4<u32>, V8<i16>>(gg) >> 16));
        let gg = addsat_i16(yy, gg) >> 6;
        packYUV(gg, br)
    }
}

/// `linearRowTapsR8(sampler, ix, offsety, stridey, fracy)`: horizontal tap pairs of a row and the next, blended.
#[inline(always)]
fn linearRowTapsR8<S: crate::texture::Sampler>(s: &S, ix: I32, offsety: i32, stridey: i32, fracy: i16) -> V8<i16> {
    let c = s.common();
    let tap = |o: i32, x: i32| c.at8::<V2<u8>>(o.wrapping_add(x));
    let row = |o: i32| -> V8<i16> {
        combine4(tap(o, ix.0[0]), tap(o, ix.0[1]), tap(o, ix.0[2]), tap(o, ix.0[3])).convert::<i16>()
    };
    let abcd0 = row(offsety);
    let abcd1 = row(offsety.wrapping_add(stridey));
    abcd0 + (((abcd1 - abcd0) * fracy) >> 7)
}

#[inline(always)]
fn row_fracx(c: &crate::glsl::samplerCommon, ix: &mut I32) -> I16 {
    let fracx = *ix;
    *ix >>= 7;
    let fracx = ((fracx & ix.cmp_ge(0)) | ix.cmp_gt(c.width as i32 - 2)) & 0x7F;
    *ix = clampCoord(*ix, c.width as i32 - 1, 0);
    fracx.convert::<i16>()
}

/// `textureLinearRowR8(sampler, ix, offsety, stridey, fracy)`: a Y row of 4 pixels, each doubled.
pub fn textureLinearRowR8<S: crate::texture::Sampler>(
    s: &S,
    ix: I32,
    offsety: i32,
    stridey: i32,
    fracy: i16,
) -> V8<i16> {
    debug_assert!(s.common().format == TextureFormat::R8);
    let mut ix = ix;
    let fracx = row_fracx(s.common(), &mut ix);
    let abcd = linearRowTapsR8(s, ix, offsety, stridey, fracy);
    let l = shuffle(abcd, abcd, [0, 0, 2, 2, 4, 4, 6, 6]);
    let h = shuffle(abcd, abcd, [1, 1, 3, 3, 5, 5, 7, 7]);
    l + (((h - l) * fracx.swizzle([0, 0, 1, 1, 2, 2, 3, 3])) >> 7)
}

/// `textureLinearRowPairedR8(sampler, sampler2, ix, offsety, stridey, fracy)`: U and V rows interleaved.
pub fn textureLinearRowPairedR8<S: crate::texture::Sampler>(
    s: &S,
    s2: &S,
    ix: I32,
    offsety: i32,
    stridey: i32,
    fracy: i16,
) -> V8<i16> {
    debug_assert!(s.common().format == TextureFormat::R8 && s2.common().format == TextureFormat::R8);
    let mut ix = ix;
    let fracx = row_fracx(s.common(), &mut ix);
    let abcd = linearRowTapsR8(s, ix, offsety, stridey, fracy);
    let xyzw = linearRowTapsR8(s2, ix, offsety, stridey, fracy);
    let l = shuffle(abcd, xyzw, [0, 8, 2, 10, 4, 12, 6, 14]);
    let h = shuffle(abcd, xyzw, [1, 9, 3, 11, 5, 13, 7, 15]);
    l + (((h - l) * fracx.swizzle([0, 0, 1, 1, 2, 2, 3, 3])) >> 7)
}

/// Extra bits of precision for stepping the YUV taps.
pub const STEP_BITS: i32 = 8;

/// Where `linear_row_yuv` puts its pixels: a destination row, blended or not.
pub trait YuvSink {
    /// `commit_blend_span<BLEND>(dest + i, r)`: pixels `i..i + 4`.
    fn commit(&mut self, i: usize, r: PackedRGBA8);
    /// `commit_blend_span<BLEND>(dest + i, r, len)`.
    fn commit_len(&mut self, i: usize, r: PackedRGBA8, len: i32);
    /// `commit_solid_span<BLEND>(dest, r, len)`.
    fn commit_solid(&mut self, r: WideRGBA8, len: i32);
}

/// An unblended row of a texture (`BLEND = false`): pixel `i` at byte `off + 4 i` of `buf`.
pub struct RowSink {
    pub buf: Buf,
    pub off: isize,
}

impl YuvSink for RowSink {
    fn commit(&mut self, i: usize, r: PackedRGBA8) {
        self.buf.st(self.off + 4 * i as isize, r);
    }
    fn commit_len(&mut self, i: usize, r: PackedRGBA8, len: i32) {
        st_partial::<u32, PackedRGBA8>(self.buf, self.off + 4 * i as isize, r, len as usize, 4);
    }
    fn commit_solid(&mut self, r: WideRGBA8, len: i32) {
        let px = bit_cast::<U32, PackedRGBA8>(pack(r)).0[0];
        for k in 0..len.max(0) as isize {
            self.buf.st(self.off + 4 * k, px);
        }
    }
}

/// A span of the color buffer being rasterized, committed through `blend.rs` (`BLEND` blends by `ctx.blend_key`).
pub struct SpanSink<'a, const BLEND: bool> {
    ctx: &'a RasterState,
    fs: &'a FragmentShaderState,
    dest: *mut u32,
}

impl<'a, const BLEND: bool> SpanSink<'a, BLEND> {
    /// # Safety
    /// `dest` must be valid for reading and writing the span's pixels, and the clip mask as
    /// `blend::blend_pixels_RGBA8` requires, while the sink is used.
    pub unsafe fn new(ctx: &'a RasterState, fs: &'a FragmentShaderState, dest: *mut u32) -> Self {
        SpanSink { ctx, fs, dest }
    }
}

impl<const BLEND: bool> YuvSink for SpanSink<'_, BLEND> {
    fn commit(&mut self, i: usize, r: PackedRGBA8) {
        // SAFETY: `SpanSink::new`'s contract covers the span's pixels; `linear_row_yuv` only commits inside it.
        unsafe { commit_blend_span::<BLEND, _, _>(self.ctx, self.fs, self.dest.wrapping_add(i), r) }
    }
    fn commit_len(&mut self, i: usize, r: PackedRGBA8, len: i32) {
        // SAFETY: as above.
        unsafe { commit_blend_span_len::<BLEND, _, _>(self.ctx, self.fs, self.dest.wrapping_add(i), r, len) }
    }
    fn commit_solid(&mut self, r: WideRGBA8, len: i32) {
        // SAFETY: as above.
        unsafe { commit_solid_span::<BLEND, *mut u32>(self.ctx, self.fs, self.dest, r, len) }
    }
}

#[inline(always)]
fn i16x4(v: I32) -> I16 {
    v.convert::<i16>()
}

/// The Y (4 bytes) and U, V (2 bytes each) taps at the given rows, as one vector.
#[inline(always)]
fn yc_taps(
    sy: &sampler2DRect_impl,
    su: &sampler2DRect_impl,
    sv: &sampler2DRect_impl,
    y_off: i32,
    yi: i32,
    c_off: i32,
    ci: i32,
) -> V8<i16> {
    let y = sy.at8::<V4<u8>>(y_off.wrapping_add(yi));
    let u = su.at8::<V2<u8>>(c_off.wrapping_add(ci));
    let v = sv.at8::<V2<u8>>(c_off.wrapping_add(ci));
    combine(y, combine(u, v)).convert::<i16>()
}

/// `upscaleYUV42R8<BLEND>`: Y at full and U/V at half resolution, both upscaled, `span` a multiple of 4.
#[allow(clippy::too_many_arguments)]
fn upscaleYUV42R8<K: YuvSink>(
    dest: &mut K,
    span: i32,
    sy: &sampler2DRect_impl,
    y_row: i32,
    mut y_u: I32,
    y_du: i32,
    y_stride_v: i32,
    y_frac_v: i16,
    su: &sampler2DRect_impl,
    sv: &sampler2DRect_impl,
    c_row: i32,
    c_u: I32,
    c_du: i32,
    c_stride_v: i32,
    c_frac_v: i16,
    color_space: &YUVMatrix,
) {
    // UV taps at offsets 0.5 and 1.5 of each pair of pixels; X fractions in the top 7 bits of a word.
    let mut c_u = (c_u.swizzle([0, 2, 0, 2]) + c_u.swizzle([1, 3, 1, 3])) >> 1;
    let shift = 16 - (STEP_BITS + 7);
    let mut yc_frac_x: V8<u16> = combine(y_u, c_u).convert::<u16>() << shift;
    let yc_frac_dx: V8<i16> = combine(I16::splat(y_du as i16), I16::splat(c_du as i16)) << shift;
    let yc_frac_v: V8<i16> = combine(I16::splat(y_frac_v), I16::splat(c_frac_v));
    let mut y_i = y_u >> (STEP_BITS + 7);
    let mut c_i = c_u >> (STEP_BITS + 7);
    let taps = |yi: i32, ci: i32| -> V8<i16> {
        let a = yc_taps(sy, su, sv, y_row, yi, c_row, ci);
        let b = yc_taps(sy, su, sv, y_row.wrapping_add(y_stride_v), yi, c_row.wrapping_add(c_stride_v), ci);
        a + (((b - a) * yc_frac_v) >> 7)
    };
    let mut yc_src = taps(y_i.0[0], c_i.0[0]);
    let mut i = 0usize;
    while (i as i32) < span {
        y_u += y_du;
        let y_in = y_u >> (STEP_BITS + 7);
        c_u += c_du;
        let c_in = c_u >> (STEP_BITS + 7);
        let yc_srcn = taps(y_in.0[0], c_in.0[0]);

        let mut yshuf: I16 = low_half(yc_src);
        let lo_n: I16 = low_half(yc_srcn);
        let mut yshufn =
            shuffle(yshuf, if y_in.0[0] == y_i.0[3] { lo_n.swizzle([1, 1, 1, 1]) } else { lo_n }, [1, 2, 3, 4]);
        if y_i.0[1] == y_i.0[0] {
            yshuf = yshuf.swizzle([0, 0, 1, 2]);
            yshufn = yshufn.swizzle([0, 0, 1, 2]);
        }
        if y_i.0[2] == y_i.0[1] {
            yshuf = yshuf.swizzle([0, 1, 1, 2]);
            yshufn = yshufn.swizzle([0, 1, 1, 2]);
        }
        if y_i.0[3] == y_i.0[2] {
            yshuf = yshuf.swizzle([0, 1, 2, 2]);
            yshufn = yshufn.swizzle([0, 1, 2, 2]);
        }
        let mut cshuf: I16 = high_half(yc_src);
        let hi_n: I16 = high_half(yc_srcn);
        let mut cshufn =
            shuffle(cshuf, if c_in.0[0] == c_i.0[1] { hi_n.swizzle([1, 1, 3, 3]) } else { hi_n }, [1, 4, 3, 6]);
        if c_i.0[1] == c_i.0[0] {
            cshuf = cshuf.swizzle([0, 0, 2, 2]);
            cshufn = cshufn.swizzle([0, 0, 2, 2]);
        }

        let yuv_px = combine(yshuf, cshuf);
        let yuv_px =
            yuv_px + (((combine(yshufn, cshufn) - yuv_px) * bit_cast::<V8<i16>, V8<u16>>(yc_frac_x >> (16 - 7))) >> 7);

        yc_src = yc_srcn;
        yc_frac_x += bit_cast::<V8<u16>, V8<i16>>(yc_frac_dx);
        y_i = y_in;
        c_i = c_in;

        let y_px = shuffle(yuv_px, yuv_px, [0, 0, 1, 1, 2, 2, 3, 3]);
        let uv_px = shuffle(yuv_px, yuv_px, [4, 6, 4, 6, 5, 7, 5, 7])
            + ((shuffle(yuv_px, yuv_px, [4, 6, 5, 7, 4, 6, 5, 7]) - shuffle(yuv_px, yuv_px, [5, 7, 4, 6, 5, 7, 4, 6]))
                >> 2);
        dest.commit(i, color_space.convert(y_px, uv_px));
        i += 4;
    }
}

/// `linear_row_yuv<BLEND>(dest, span, samplerY, srcUV, srcDU, samplerU, samplerV, chromaUV, chromaDU,
/// colorDepth, colorSpace)`: one row of `CompositeYUV`, also the span fast path of `swgl_commitTextureLinearYUV`.
#[allow(clippy::too_many_arguments)]
pub fn linear_row_yuv<K: YuvSink>(
    dest: &mut K,
    mut span: i32,
    sy: &sampler2DRect_impl,
    src_uv: &vec2_scalar,
    src_du: f32,
    su: &sampler2DRect_impl,
    sv: &sampler2DRect_impl,
    chroma_uv: &vec2_scalar,
    chroma_du: f32,
    color_depth: i32,
    color_space: &YUVMatrix,
) {
    let mut y_u: I32 = cast(init_interp(src_uv.x, src_du) * ((1 << STEP_BITS) as f32));
    let mut y_v = crate::glsl::vector::f32_to_i32(src_uv.y);
    let mut c_u: I32 = cast(init_interp(chroma_uv.x, chroma_du) * ((1 << STEP_BITS) as f32));
    let mut c_v = crate::glsl::vector::f32_to_i32(chroma_uv.y);
    let y_du = crate::glsl::vector::f32_to_i32(((4 << STEP_BITS) as f32) * src_du);
    let c_du = crate::glsl::vector::f32_to_i32(((4 << STEP_BITS) as f32) * chroma_du);
    let mut at = 0usize;

    if sy.width < 2 || su.width < 2 {
        // Rows of fewer than 2 pixels: nearest filtering of the one pixel, filling the row.
        let p = |uv: &vec2_scalar| {
            ivec2::new(
                I32::splat(crate::glsl::vector::f32_to_i32(uv.x)),
                I32::splat(crate::glsl::vector::f32_to_i32(uv.y)),
            )
        };
        let mut yuv_f = Float::new([
            texelFetch(*sy, p(src_uv), 0).x.0[0],
            texelFetch(*su, p(chroma_uv), 0).x.0[0],
            texelFetch(*sv, p(chroma_uv), 0).x.0[0],
            1.0,
        ]);
        if color_depth > 8 {
            let rescale_factor = 16 - color_depth;
            yuv_f *= (1i32 << rescale_factor) as f32;
        }
        let yuv: I16 = i16x4(round_pixel(yuv_f, 255.0));
        let r = color_space.convert(V8::splat(yuv.0[0]), zip(I16::splat(yuv.0[1]), I16::splat(yuv.0[2])));
        dest.commit_solid(unpack(r), span);
    } else if sy.format == TextureFormat::R16 {
        debug_assert!(color_depth > 8);
        // Samples come out of textureLinearUnpackedR16 with 15 bits; shift them down to 8.
        let rescale_bits = (color_depth - 1) - 8;
        let sample = |s: &sampler2DRect_impl, u: I32, v: i32| -> I16 {
            textureLinearUnpackedR16(s, ivec2::new(u >> STEP_BITS, I32::splat(v))) >> rescale_bits
        };
        while span >= 4 {
            let y_px = sample(sy, y_u, y_v);
            let u_px = sample(su, c_u, c_v);
            let v_px = sample(sv, c_u, c_v);
            dest.commit(at, color_space.convert(zip(y_px, y_px), zip(u_px, v_px)));
            at += 4;
            y_u += y_du;
            c_u += c_du;
            span -= 4;
        }
        if span > 0 {
            let y_px = sample(sy, y_u, y_v);
            let u_px = sample(su, c_u, c_v);
            let v_px = sample(sv, c_u, c_v);
            dest.commit_len(at, color_space.convert(zip(y_px, y_px), zip(u_px, v_px)), span);
        }
    } else {
        debug_assert!(sy.format == TextureFormat::R8);
        debug_assert!(color_depth == 8);
        let y_frac_v = (y_v & 0x7F) as i16;
        y_v >>= 7;
        let y_offset_v = (clampCoord_scalar(y_v, sy.height as i32, 0) as u32).wrapping_mul(sy.stride) as i32;
        let y_stride_v = if y_v >= 0 && y_v < sy.height as i32 - 1 { sy.stride as i32 } else { 0 };
        let c_frac_v = (c_v & 0x7F) as i16;
        c_v >>= 7;
        let c_offset_v = (clampCoord_scalar(c_v, su.height as i32, 0) as u32).wrapping_mul(su.stride) as i32;
        let c_stride_v = if c_v >= 0 && c_v < su.height as i32 - 1 { su.stride as i32 } else { 0 };

        let chunk = |y_u: I32, c_u: I32| {
            let y_px = textureLinearRowR8(sy, y_u >> STEP_BITS, y_offset_v, y_stride_v, y_frac_v);
            let uv_px = textureLinearRowPairedR8(su, sv, c_u >> STEP_BITS, c_offset_v, c_stride_v, c_frac_v);
            color_space.convert(y_px, uv_px)
        };

        // UV at half the resolution of Y: the half-resolution fast path.
        if y_du >= c_du && c_du > 0 && y_du <= (4 << (STEP_BITS + 7)) && c_du <= (2 << (STEP_BITS + 7)) {
            // Step until the initial X coordinates are positive.
            while (y_u.0[0] < 0 || c_u.0[0] < 0) && span >= 4 {
                dest.commit(at, chunk(y_u, c_u));
                at += 4;
                y_u += y_du;
                c_u += c_du;
                span -= 4;
            }
            // The aligned chunks that stay inside each plane without overreading.
            let inside = ((((sy.width as i32 - 4) << (STEP_BITS + 7)) - y_u.0[0]) / y_du)
                .min((((su.width as i32 - 4) << (STEP_BITS + 7)) - c_u.0[0]) / c_du)
                .wrapping_mul(4)
                .min(span & !3);
            if inside > 0 {
                let mut sub = OffsetSink { inner: &mut *dest, base: at };
                upscaleYUV42R8(
                    &mut sub,
                    inside,
                    sy,
                    y_offset_v,
                    y_u,
                    y_du,
                    y_stride_v,
                    y_frac_v,
                    su,
                    sv,
                    c_offset_v,
                    c_u,
                    c_du,
                    c_stride_v,
                    c_frac_v,
                    color_space,
                );
                span -= inside;
                at += inside as usize;
                y_u += (inside / 4) * y_du;
                c_u += (inside / 4) * c_du;
            }
        }
        while span >= 4 {
            dest.commit(at, chunk(y_u, c_u));
            at += 4;
            y_u += y_du;
            c_u += c_du;
            span -= 4;
        }
        if span > 0 {
            dest.commit_len(at, chunk(y_u, c_u), span);
        }
    }
}

/// A sink shifted by `base` pixels.
struct OffsetSink<'a, K: YuvSink> {
    inner: &'a mut K,
    base: usize,
}

impl<K: YuvSink> YuvSink for OffsetSink<'_, K> {
    fn commit(&mut self, i: usize, r: PackedRGBA8) {
        self.inner.commit(self.base + i, r);
    }
    fn commit_len(&mut self, i: usize, r: PackedRGBA8, len: i32) {
        self.inner.commit_len(self.base + i, r, len);
    }
    fn commit_solid(&mut self, r: WideRGBA8, len: i32) {
        self.inner.commit_solid(r, len);
    }
}

/// `linear_convert_yuv(ytex, utex, vtex, rgbFromYcbcr, colorDepth, srcReq, dsttex, dstReq, invertX, invertY,
/// clipRect)`.
#[allow(clippy::too_many_arguments)]
pub fn linear_convert_yuv(
    ytex: &mut Texture,
    utex: &mut Texture,
    vtex: &mut Texture,
    rgb_from_ycbcr: &YUVMatrix,
    color_depth: i32,
    src_req: &IntRect,
    dsttex: &mut Texture,
    dst_req: &IntRect,
    invert_x: bool,
    invert_y: bool,
    clip_rect: &IntRect,
) {
    let mut dst_bounds = dsttex.sample_bounds(dst_req, false);
    dst_bounds.intersect(clip_rect);
    if dst_bounds.is_empty() {
        return;
    }
    let mut sampler = [sampler2DRect_impl::default(); 3];
    init_sampler(&mut sampler[0], ytex);
    init_sampler(&mut sampler[1], utex);
    init_sampler(&mut sampler[2], vtex);

    let mut src_uv = vec2_scalar::new(src_req.x0 as f32, src_req.y0 as f32);
    let mut src_duv = vec2_scalar::new(
        src_req.width() as f32 / dst_req.width() as f32,
        src_req.height() as f32 / dst_req.height() as f32,
    );
    if invert_x {
        src_uv.x += src_req.width() as f32;
        src_duv.x = -src_duv.x;
    }
    if invert_y {
        src_uv.y += src_req.height() as f32;
        src_duv.y = -src_duv.y;
    }
    src_uv.x += src_duv.x * (dst_bounds.x0 as f32 + 0.5f32);
    src_uv.y += src_duv.y * (dst_bounds.y0 as f32 + 0.5f32);
    let chroma_scale = vec2_scalar::new(utex.width as f32 / ytex.width as f32, utex.height as f32 / ytex.height as f32);
    let mut chroma_uv = vec2_scalar::new(src_uv.x * chroma_scale.x, src_uv.y * chroma_scale.y);
    let mut chroma_duv = vec2_scalar::new(src_duv.x * chroma_scale.x, src_duv.y * chroma_scale.y);
    // Quantize for the linear filter, except for 1-pixel rows, which sample nearest to avoid overreads.
    if ytex.width >= 2 && utex.width >= 2 {
        src_uv = linearQuantize_scalar(src_uv, 128.0);
        src_duv.x *= 128.0f32;
        src_duv.y *= 128.0f32;
        chroma_uv = linearQuantize_scalar(chroma_uv, 128.0);
        chroma_duv.x *= 128.0f32;
        chroma_duv.y *= 128.0f32;
    }
    let dest_stride = dsttex.stride() as isize;
    let x0 = dst_req.x0 + dst_bounds.x0;
    let y0 = dst_req.y0 + dst_bounds.y0;
    let Some(mut dest) = row_at(dsttex, x0, x0 + dst_bounds.width(), y0) else {
        debug_assert!(false, "CompositeYUV destination outside the texture");
        return;
    };
    let db = Buf::of(dsttex);
    let span = dst_bounds.width();
    for _ in 0..dst_bounds.height() {
        let mut sink = RowSink { buf: db, off: dest };
        linear_row_yuv(
            &mut sink,
            span,
            &sampler[0],
            &src_uv,
            src_duv.x,
            &sampler[1],
            &sampler[2],
            &chroma_uv,
            chroma_duv.x,
            color_depth,
            rgb_from_ycbcr,
        );
        dest += dest_stride;
        src_uv.y += src_duv.y;
        chroma_uv.y += chroma_duv.y;
    }
}

/// `YUVRangedColorSpace` (gfx/2d/Types.h).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum YUVRangedColorSpace {
    BT601_Narrow = 0,
    BT601_Full,
    BT709_Narrow,
    BT709_Full,
    BT2020_Narrow,
    BT2020_Full,
    GbrIdentity,
}

impl YUVRangedColorSpace {
    pub fn from_u8(v: u8) -> Option<Self> {
        use YUVRangedColorSpace::*;
        [BT601_Narrow, BT601_Full, BT709_Narrow, BT709_Full, BT2020_Narrow, BT2020_Full, GbrIdentity]
            .get(v as usize)
            .copied()
    }
}

/// `get_ycbcr_zeros_ones(color_space, color_depth)` (yuv.glsl): Y and CbCr zero, Y and CbCr one, normalised.
pub fn get_ycbcr_zeros_ones(color_space: YUVRangedColorSpace, color_depth: u32) -> vec4_scalar {
    use YUVRangedColorSpace::*;
    match color_space {
        BT601_Narrow | BT709_Narrow | BT2020_Narrow => {
            let extra = color_depth.wrapping_sub(8);
            let all_bits = (1u32.wrapping_shl(color_depth)).wrapping_sub(1) as i32 as f32;
            vec4_scalar::new(
                (16u32.wrapping_shl(extra)) as i32 as f32 / all_bits,
                (128u32.wrapping_shl(extra)) as i32 as f32 / all_bits,
                (235u32.wrapping_shl(extra)) as i32 as f32 / all_bits,
                (240u32.wrapping_shl(extra)) as i32 as f32 / all_bits,
            )
        }
        BT601_Full | BT709_Full | BT2020_Full => {
            let narrow = get_ycbcr_zeros_ones(BT601_Narrow, color_depth);
            vec4_scalar::new(0.0, narrow.y, 1.0, 1.0)
        }
        GbrIdentity => vec4_scalar::new(0.0, 0.0, 1.0, 1.0),
    }
}

const fn m3(c0: [f32; 3], c1: [f32; 3], c2: [f32; 3]) -> mat3_scalar {
    mat3_scalar::new(
        vec3_scalar::new(c0[0], c0[1], c0[2]),
        vec3_scalar::new(c1[0], c1[1], c1[2]),
        vec3_scalar::new(c2[0], c2[1], c2[2]),
    )
}

pub const RgbFromYuv_Rec601: mat3_scalar = m3([1.0, 1.0, 1.0], [0.0, -0.17207, 0.88600], [0.70100, -0.35707, 0.0]);
pub const RgbFromYuv_Rec709: mat3_scalar = m3([1.0, 1.0, 1.0], [0.0, -0.09366, 0.92780], [0.78740, -0.23406, 0.0]);
pub const RgbFromYuv_Rec2020: mat3_scalar = m3([1.0, 1.0, 1.0], [0.0, -0.08228, 0.94070], [0.73730, -0.28568, 0.0]);
pub const RgbFromYuv_GbrIdentity: mat3_scalar = m3([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);

/// `get_rgb_from_yuv(color_space)`.
pub fn get_rgb_from_yuv(color_space: YUVRangedColorSpace) -> mat3_scalar {
    use YUVRangedColorSpace::*;
    match color_space {
        BT601_Narrow | BT601_Full => RgbFromYuv_Rec601,
        BT709_Narrow | BT709_Full => RgbFromYuv_Rec709,
        BT2020_Narrow | BT2020_Full => RgbFromYuv_Rec2020,
        GbrIdentity => RgbFromYuv_GbrIdentity,
    }
}

/// `YcbcrInfo`.
#[derive(Clone, Copy, Debug)]
pub struct YcbcrInfo {
    pub ycbcr_bias: vec3_scalar,
    pub rgb_from_debiased_ycbcr: mat3_scalar,
}

/// `get_ycbcr_info(color_space, color_depth)`: swgl does 8-bit math whatever the depth.
pub fn get_ycbcr_info(color_space: YUVRangedColorSpace, _color_depth: u32) -> YcbcrInfo {
    let color_depth = 8;
    let zo = get_ycbcr_zeros_ones(color_space, color_depth);
    let zeros = vec2_scalar::new(zo.x, zo.y);
    let ones = vec2_scalar::new(zo.z, zo.w);
    let scale = vec2_scalar::new(1.0f32 / (ones.x - zeros.x), 1.0f32 / (ones.y - zeros.y));
    let rgb_from_yuv = get_rgb_from_yuv(color_space);
    let yuv_from_debiased_ycbcr = m3([scale.x, 0.0, 0.0], [0.0, scale.y, 0.0], [0.0, 0.0, scale.y]);
    YcbcrInfo {
        ycbcr_bias: vec3_scalar::new(zeros.x, zeros.y, zeros.y),
        rgb_from_debiased_ycbcr: rgb_from_yuv * yuv_from_debiased_ycbcr,
    }
}

/// `CompositeYUV(lockedDst, lockedY, lockedU, lockedV, colorSpace, colorDepth, ...)`: YUV planes (R8 for 8-bit,
/// R16 for deeper) to BGRA, always through the linear filter path.
#[allow(clippy::too_many_arguments)]
pub fn CompositeYUV(
    dsttex: &mut Texture,
    ytex: &mut Texture,
    utex: &mut Texture,
    vtex: &mut Texture,
    color_space: YUVRangedColorSpace,
    color_depth: u32,
    src_x: i32,
    src_y: i32,
    src_width: i32,
    src_height: i32,
    dst_x: i32,
    dst_y: i32,
    dst_width: i32,
    dst_height: i32,
    flip_x: bool,
    flip_y: bool,
    clip_x: i32,
    clip_y: i32,
    clip_width: i32,
    clip_height: i32,
) {
    let info = get_ycbcr_info(color_space, color_depth);
    let rgb_from_ycbcr = YUVMatrix::from_matrix(&info.ycbcr_bias, &info.rgb_from_debiased_ycbcr, 0);
    debug_assert!(ytex.bpp() == utex.bpp() && ytex.bpp() == vtex.bpp());
    debug_assert!(utex.width == vtex.width && utex.height == vtex.height);
    debug_assert!(dsttex.bpp() == 4);
    let src_req = IntRect::new(src_x, src_y, src_x + src_width, src_y + src_height) - ytex.offset;
    let dst_req = IntRect::new(dst_x, dst_y, dst_x + dst_width, dst_y + dst_height) - dsttex.offset;
    if src_req.is_empty() || dst_req.is_empty() {
        return;
    }
    let clip_rect =
        IntRect::new(clip_x - dst_x, clip_y - dst_y, clip_x - dst_x + clip_width, clip_y - dst_y + clip_height);
    linear_convert_yuv(
        ytex,
        utex,
        vtex,
        &rgb_from_ycbcr,
        color_depth as i32,
        &src_req,
        dsttex,
        &dst_req,
        flip_x,
        flip_y,
        &clip_rect,
    );
}
