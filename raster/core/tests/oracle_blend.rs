/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Bit-exactness of `lucinanet_raster::blend` against swgl's blend.h (`tests/oracle/shim_t.cc`): `blend_pixels`
//! for every blend key of every group (plain, MASK, AA, AA_MASK) on spans of 1 to 4 pixels with random source,
//! destination, clip mask, AA state, blend color and secondary color; the commits; the packing of shader output;
//! and, exhaustively, `muldiv255` and premultiplied source-over against the Verus-proved `rules`.

mod common;

use common::*;
use lucinanet_raster::blend::{self as bl, BLEND_KEY_COUNT};
use lucinanet_raster::glsl::*;
use lucinanet_raster::program::FragmentShaderState;
use lucinanet_raster::state::RasterState;
use lucinanet_raster::texture::{PackedRGBA8, WideR8, WideRGBA8};
use lucinanet_raster_oracle as o;
use lucinanet_raster_rules as rules;

const CASES_PER_KEY: usize = 10_000;

/// A span of pixels in a guarded color buffer, a clip mask for it, and the blend state around it.
struct Setup {
    color: Guarded,
    /// Owns the clip mask `st.clip_mask_buf` points into.
    #[allow(dead_code)]
    mask: Guarded,
    /// Pixel index of the chunk in `color`, and of the span start.
    at: usize,
    span_start: usize,
    /// Byte offset of `swgl_OpaqueStart` from the chunk.
    opaque_rel: isize,
    st: o::TBlendState,
}

fn lanes(r: &mut Rng, lo: f32, hi: f32) -> [f32; 4] {
    let mut v = [0f32; 4];
    for x in v.iter_mut() {
        *x = match r.below(12) {
            0 => [f32::NAN, f32::INFINITY, -f32::INFINITY, 0.0, -0.0, 256.0][r.below(6) as usize],
            _ => r.range(lo, hi),
        };
    }
    v
}

/// A source chunk: bytes mostly, sometimes words past 255 (unclamped shader output packs to up to 0x7FFF).
fn wide(r: &mut Rng) -> [u16; 16] {
    let mut v = [0u16; 16];
    for x in v.iter_mut() {
        *x = match r.below(24) {
            0 => r.int(256, 0x7FFF) as u16,
            1 => r.next() as u16,
            _ => r.byte_edgy() as u16,
        };
    }
    v
}

fn bytes16(r: &mut Rng) -> [u16; 16] {
    let mut v = [0u16; 16];
    for x in v.iter_mut() {
        *x = r.byte_edgy() as u16;
    }
    v
}

impl Setup {
    fn new(r: &mut Rng, key: u8, pixel_bytes: usize) -> Setup {
        let mut color = Guarded::new(64 * pixel_bytes);
        for b in color.bytes().iter_mut() {
            *b = r.byte_edgy();
        }
        let mut mask = Guarded::new(64);
        for b in mask.bytes().iter_mut() {
            *b = r.byte_edgy();
        }
        let at = r.int(8, 40) as usize;
        let span_start = at - r.int(0, 8) as usize;
        let opaque_rel = -(r.int(-12, 24) as isize) * if r.chance(3) { 1 } else { pixel_bytes as isize };
        let mut bc = [0u16; 8];
        for x in bc.iter_mut() {
            *x = r.byte() as u16;
        }
        let mut secondary = [0f32; 16];
        for x in secondary.iter_mut() {
            *x = if r.chance(16) { r.range(-0.5, 1.5) } else { r.unit() };
        }
        let cptr = color.ptr();
        let mptr = mask.ptr();
        let st = o::TBlendState {
            key: key as i32,
            span_buf: cptr.wrapping_add(span_start * pixel_bytes),
            clip_mask_buf: mptr.wrapping_add(r.int(0, 16) as usize),
            opaque_start: cptr.wrapping_add(at * pixel_bytes).wrapping_offset(opaque_rel),
            opaque_size: r.int(0, 40) as u32 * if r.chance(3) { 1 } else { pixel_bytes as u32 },
            left_aa: lanes(r, -40.0, 300.0),
            right_aa: lanes(r, -40.0, 300.0),
            aa_slope: lanes(r, -12.0, 12.0),
            blendcolor: bc,
            blend_color_rgba8: bytes16(r),
            blend_alpha_rgba8: bytes16(r),
            secondary,
        };
        Setup { color, mask, at, span_start, opaque_rel, st }
    }

    fn rust_state(&self) -> (RasterState, FragmentShaderState) {
        rust_state(&self.st)
    }

    fn describe(&self) -> String {
        describe(&self.st)
            + &format!(" chunk {} span start {} opaque at {:+}", self.at, self.span_start, self.opaque_rel)
    }
}

fn rust_state(s: &o::TBlendState) -> (RasterState, FragmentShaderState) {
    {
        let mut ctx = RasterState {
            blend_key: s.key as u8,
            swgl_SpanBuf: s.span_buf,
            swgl_ClipMaskBuf: s.clip_mask_buf,
            swgl_OpaqueStart: s.opaque_start,
            swgl_OpaqueSize: s.opaque_size,
            swgl_LeftAADist: Float::new(s.left_aa),
            swgl_RightAADist: Float::new(s.right_aa),
            swgl_AASlope: Float::new(s.aa_slope),
            blendcolor: V8::new(s.blendcolor),
            ..Default::default()
        };
        ctx.swgl_BlendColorRGBA8 = WideRGBA8::new(s.blend_color_rgba8);
        ctx.swgl_BlendAlphaRGBA8 = WideRGBA8::new(s.blend_alpha_rgba8);
        let mut fs = FragmentShaderState::default();
        let q = |k: usize| Float::new([s.secondary[k], s.secondary[k + 1], s.secondary[k + 2], s.secondary[k + 3]]);
        fs.gl_SecondaryFragColor = vec4::new(q(0), q(4), q(8), q(12));
        (ctx, fs)
    }
}

fn describe(s: &o::TBlendState) -> String {
    format!(
        "key {} (group {}, base {}) opaque size {} aa {:?} {:?} slope {:?}",
        s.key,
        s.key / BLEND_KEY_COUNT as i32,
        s.key % BLEND_KEY_COUNT as i32,
        s.opaque_size,
        s.left_aa,
        s.right_aa,
        s.aa_slope
    )
}

#[test]
fn blend_pixels_rgba8_every_key() {
    let _lock = oracle_lock();
    let mut r = Rng::new("blend_pixels RGBA8");
    for key in 0..4 * BLEND_KEY_COUNT {
        for case in 0..CASES_PER_KEY {
            let span = 1 + (case % 4) as i32;
            let mut s = Setup::new(&mut r, key, 4);
            let buf = s.color.ptr().wrapping_add(s.at * 4) as *mut u32;
            let mut pdst = [0u8; 16];
            for b in pdst.iter_mut() {
                *b = r.byte_edgy();
            }
            let src = wide(&mut r);
            let mut cxx = [0u16; 16];
            // SAFETY: the blend state points into the guarded color and mask buffers, alive for the call; the
            // mask bytes of the span (at most 4 from clip_mask_buf + up to 8) are inside the 64-byte mask.
            unsafe { o::t_blend_pixels_rgba8(&s.st, buf, pdst.as_ptr(), src.as_ptr(), span, cxx.as_mut_ptr()) };
            let (ctx, fs) = s.rust_state();
            // SAFETY: as above.
            let rust =
                unsafe { bl::blend_pixels_RGBA8(&ctx, &fs, buf, PackedRGBA8::new(pdst), WideRGBA8::new(src), span) };
            assert_eq!(rust.0, cxx, "blend_pixels RGBA8 {} span {span}\n src {src:?}\n dst {pdst:?}", s.describe());
        }
    }
}

#[test]
fn blend_pixels_r8_every_key() {
    let _lock = oracle_lock();
    let mut r = Rng::new("blend_pixels R8");
    let bases = [bl::BLEND_ONE_ZERO, bl::BLEND_ZERO_SRC_COLOR, bl::BLEND_ONE_ONE];
    for group in 0..4u8 {
        for base in bases {
            let key = group * BLEND_KEY_COUNT + base;
            for case in 0..CASES_PER_KEY {
                let span = 1 + (case % 4) as i32;
                let mut s = Setup::new(&mut r, key, 1);
                let buf = s.color.ptr().wrapping_add(s.at);
                let mut dst = [0u16; 4];
                let mut src = [0u16; 4];
                for k in 0..4 {
                    dst[k] = r.byte_edgy() as u16;
                    src[k] = if r.chance(20) { r.next() as u16 } else { r.byte_edgy() as u16 };
                }
                let mut cxx = [0u16; 4];
                // SAFETY: as in blend_pixels_rgba8_every_key.
                unsafe { o::t_blend_pixels_r8(&s.st, buf, dst.as_ptr(), src.as_ptr(), span, cxx.as_mut_ptr()) };
                let (ctx, _) = s.rust_state();
                // SAFETY: as above.
                let rust = unsafe { bl::blend_pixels_R8(&ctx, buf, WideR8::new(dst), WideR8::new(src), span) };
                assert_eq!(rust.0, cxx, "blend_pixels R8 {} span {span} src {src:?} dst {dst:?}", s.describe());
            }
        }
    }
}

#[test]
fn commits_and_solid_spans() {
    let _lock = oracle_lock();
    let mut r = Rng::new("commit");
    for case in 0..40_000 {
        let r8 = r.chance(2);
        let blend = r.chance(2);
        let kind = r.int(0, 2);
        let px = if r8 { 1 } else { 4 };
        let key = if r8 {
            r.int(0, 3) as u8 * BLEND_KEY_COUNT
                + [bl::BLEND_ONE_ZERO, bl::BLEND_ZERO_SRC_COLOR, bl::BLEND_ONE_ONE][r.below(3) as usize]
        } else {
            r.int(0, 4 * BLEND_KEY_COUNT as i32 - 1) as u8
        };
        let s = Setup::new(&mut r, key, px);
        // The span: any alignment for R8 (the solid fill depends on it), `len` pixels from the chunk.
        let len = match kind {
            0 => 4,
            1 => r.int(1, 3),
            _ => r.int(0, 20),
        };
        let mut val = [0u16; 16];
        for (k, v) in val.iter_mut().enumerate() {
            *v = if r8 && k >= 4 {
                0
            } else if r.chance(20) {
                r.next() as u16
            } else {
                r.byte_edgy() as u16
            };
        }
        let mut s = s;
        let mut a = s.color.clone();
        let mut b = s.color.clone();
        // The blend state points into `s.color`; the copies are laid out the same, so move the pointers along.
        let base = s.color.ptr();
        let (pa, pb) = (a.ptr(), b.ptr());
        let shift = |p: *const u8, to: *mut u8| to.wrapping_offset(p as isize - base as isize);
        let mut st_a = s.st;
        let mut st_b = s.st;
        st_a.span_buf = shift(s.st.span_buf, pa);
        st_a.opaque_start = shift(s.st.opaque_start, pa);
        st_b.span_buf = shift(s.st.span_buf, pb);
        st_b.opaque_start = shift(s.st.opaque_start, pb);
        let at = s.at * px;
        // SAFETY: the chunk plus up to 20 pixels from `at <= 40` stay inside the 64-pixel color buffers; the mask
        // pointer is `s.mask`'s, alive here.
        unsafe { o::t_commit(&st_a, r8 as i32, blend as i32, kind, pa.wrapping_add(at), val.as_ptr(), len) };
        let (ctx, fs) = rust_state(&st_b);
        let pbuf = pb.wrapping_add(at);
        // SAFETY: as above.
        unsafe {
            if r8 {
                let v = WideR8::new([val[0], val[1], val[2], val[3]]);
                let p = pbuf;
                match (kind, blend) {
                    (0, true) => bl::commit_blend_span::<true, _, _>(&ctx, &fs, p, v),
                    (0, false) => bl::commit_blend_span::<false, _, _>(&ctx, &fs, p, v),
                    (1, true) => bl::commit_blend_span_len::<true, _, _>(&ctx, &fs, p, v, len),
                    (1, false) => bl::commit_blend_span_len::<false, _, _>(&ctx, &fs, p, v, len),
                    (_, true) => bl::commit_solid_span::<true, *mut u8>(&ctx, &fs, p, v, len),
                    (_, false) => bl::commit_solid_span::<false, *mut u8>(&ctx, &fs, p, v, len),
                }
            } else {
                let v = WideRGBA8::new(val);
                let p = pbuf as *mut u32;
                match (kind, blend) {
                    (0, true) => bl::commit_blend_span::<true, _, _>(&ctx, &fs, p, v),
                    (0, false) => bl::commit_blend_span::<false, _, _>(&ctx, &fs, p, v),
                    (1, true) => bl::commit_blend_span_len::<true, _, _>(&ctx, &fs, p, v, len),
                    (1, false) => bl::commit_blend_span_len::<false, _, _>(&ctx, &fs, p, v, len),
                    (_, true) => bl::commit_solid_span::<true, *mut u32>(&ctx, &fs, p, v, len),
                    (_, false) => bl::commit_solid_span::<false, *mut u32>(&ctx, &fs, p, v, len),
                }
            }
        }
        if a.all() != b.all() {
            let d: Vec<(usize, u8, u8)> = a
                .all()
                .iter()
                .zip(b.all())
                .enumerate()
                .filter(|(_, (x, y))| x != y)
                .map(|(i, (x, y))| (i, *x, *y))
                .collect();
            eprintln!("differences (index, c++, rust) {d:?} chunk at byte {}", GUARD + at);
        }
        assert!(
            a.all() == b.all(),
            "case {case}: commit r8 {r8} blend {blend} kind {kind} len {len} {}",
            describe(&st_b)
        );
    }
}

#[test]
fn pack_pixels_every_overload() {
    let mut r = Rng::new("pack_pixels");
    for _ in 0..100_000 {
        let kind = r.int(0, 5);
        let mut v = [0f32; 16];
        for x in v.iter_mut() {
            *x = match r.below(10) {
                0 => [f32::NAN, f32::INFINITY, -f32::INFINITY, 0.5 / 255.0, 1.5 / 255.0, 1.0e9][r.below(6) as usize],
                1 => r.range(-300.0, 70000.0),
                _ => r.range(-0.2, 1.2),
            };
        }
        let scale = [255.0, 1.0, 1.0 / 255.0][r.below(3) as usize];
        let mut cxx = [0u16; 16];
        // SAFETY: `v` has the 16 floats the widest overload reads, `cxx` the 16 words it writes.
        unsafe { o::t_pack_pixels(kind, v.as_ptr(), scale, cxx.as_mut_ptr()) };
        let q = |k: usize| Float::new([v[k], v[k + 1], v[k + 2], v[k + 3]]);
        let rust: Vec<u16> = match kind {
            0 => bl::pack_pixels_RGBA8(vec4::new(q(0), q(4), q(8), q(12)), scale).0.to_vec(),
            1 => bl::pack_pixels_RGBA8(q(0), scale).0.to_vec(),
            2 => bl::pack_pixels_RGBA8(v[0], scale).0.to_vec(),
            3 => bl::pack_pixels_RGBA8(vec4_scalar::new(v[0], v[1], v[2], v[3]), scale).0.to_vec(),
            4 => bl::pack_pixels_RGBA8(V16::new(v), scale).0.to_vec(),
            _ => bl::pack_pixels_R8(q(0), scale).0.to_vec(),
        };
        assert_eq!(rust[..], cxx[..rust.len()], "pack_pixels kind {kind} scale {scale} of {v:?}");
    }
}

#[test]
fn muldiv255_exhaustive() {
    // Every (x, y) in 0..=255: the lane form equals the proved scalar one, and the C++.
    for x in 0..=255u16 {
        for y0 in (0..=255u16).step_by(16) {
            let xs = WideRGBA8::splat(x);
            let ys = WideRGBA8::new(core::array::from_fn(|k| y0 + k as u16));
            let lanes = bl::muldiv255(xs, ys);
            let mut cxx = [0u16; 16];
            // SAFETY: 16-word arrays in and out.
            unsafe { o::t_muldiv255(xs.0.as_ptr(), ys.0.as_ptr(), cxx.as_mut_ptr()) };
            assert_eq!(lanes.0, cxx, "muldiv255({x}, {y0}..)");
            for k in 0..16 {
                assert_eq!(lanes.0[k], rules::muldiv255(x, y0 + k as u16), "muldiv255({x}, {})", y0 + k as u16);
            }
        }
    }
}

#[test]
fn source_over_exhaustive() {
    // Every premultiplied source (src <= alpha <= 255) over every destination channel (0..=255), through
    // blend_pixels with BLEND_KEY(GL_ONE, GL_ONE_MINUS_SRC_ALPHA): the lane path equals `rules::source_over` on the
    // color channels and on alpha (whose source is its own alpha).
    let ctx = RasterState { blend_key: bl::BLEND_ONE_ONE_MINUS_SRC_ALPHA, ..Default::default() };
    let fs = FragmentShaderState::default();
    let mut checked = 0u64;
    for a in 0..=255u16 {
        // (src, dst) pairs for this alpha, 12 color channels a call.
        let pairs: Vec<(u16, u16)> = (0..=a).flat_map(|s| (0..=255u16).map(move |d| (s, d))).collect();
        for chunk in pairs.chunks(12) {
            let mut src = [0u16; 16];
            let mut dst = [0u8; 16];
            for p in 0..4 {
                src[4 * p + 3] = a;
                dst[4 * p + 3] = chunk.get(3 * p).map_or(0, |c| c.1) as u8;
            }
            for (k, &(s, d)) in chunk.iter().enumerate() {
                let lane = 4 * (k / 3) + k % 3;
                src[lane] = s;
                dst[lane] = d as u8;
            }
            // SAFETY: the plain source-over key reads no clip mask; the pointer is never dereferenced.
            let out = unsafe {
                bl::blend_pixels_RGBA8(&ctx, &fs, core::ptr::null_mut(), PackedRGBA8::new(dst), WideRGBA8::new(src), 4)
            };
            for lane in 0..16 {
                let (s, d) = (src[lane], dst[lane] as u16);
                assert_eq!(out.0[lane], rules::source_over(s, d, a), "source_over(src {s}, dst {d}, alpha {a})");
                // And it packs without clipping.
                assert!(out.0[lane] <= 255);
            }
            checked += chunk.len() as u64;
        }
    }
    assert_eq!(checked, (0..=255u64).map(|a| (a + 1) * 256).sum::<u64>());
}

/// The chunk of the AArch64 `texture_yuv` case 7 difference (oracle_ext: `commitTextureLinearYUV2`, key 25): an
/// opaque YUV source over pixels 8-11 of that span, under each HSL key and with the clip state of a fresh `Setup`.
/// x86_64 C++ and the Rust give B 99 and R 86 for pixels 10 and 11 there, AArch64 C++ 98 and 85: if this passes on
/// AArch64, the difference is not in `blend_pixels` itself.
#[test]
fn blend_pixels_hsl_yuv_chunk() {
    let _lock = oracle_lock();
    let mut r = Rng::new("blend_pixels HSL YUV chunk");
    let pdst: [u8; 16] = [34, 171, 0, 63, 224, 0, 234, 122, 54, 6, 247, 94, 48, 224, 52, 174];
    let src: [u16; 16] = [53, 255, 195, 255, 53, 255, 195, 255, 53, 255, 195, 255, 53, 255, 195, 255];
    for key in [bl::BLEND_HSL_HUE_KHR, bl::BLEND_HSL_SATURATION_KHR, bl::BLEND_HSL_COLOR_KHR, bl::BLEND_HSL_LUMINOSITY_KHR] {
        let mut s = Setup::new(&mut r, key, 4);
        let buf = s.color.ptr().wrapping_add(s.at * 4) as *mut u32;
        let mut cxx = [0u16; 16];
        // SAFETY: as in blend_pixels_rgba8_every_key; no mask or AA group, so no mask bytes are read.
        unsafe { o::t_blend_pixels_rgba8(&s.st, buf, pdst.as_ptr(), src.as_ptr(), 4, cxx.as_mut_ptr()) };
        let (ctx, fs) = s.rust_state();
        // SAFETY: as above.
        let rust = unsafe { bl::blend_pixels_RGBA8(&ctx, &fs, buf, PackedRGBA8::new(pdst), WideRGBA8::new(src), 4) };
        eprintln!("HSL key {key}: C++ {cxx:?}\n           Rust {:?}", rust.0);
        assert_eq!(rust.0, cxx, "blend_pixels HSL key {key}");
    }
}
