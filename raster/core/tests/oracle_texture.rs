/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Bit-exactness of `lucinanet_raster::texture` against swgl's texture.h (`tests/oracle/shim_t.cc`): every texel
//! fetch, nearest and linear sampling of every format, the packed linear filters, `texelFetchPtr` /
//! `texelFetchUnchecked`, the Gaussian blur taps and delayed clears, on random textures (sizes 1..70, 1 and 2
//! often, strides `set_buffer` accepts) and random coordinates in and out of range, from fixed seeds.

mod common;

use common::*;
use lucinanet_raster::glsl::*;
use lucinanet_raster::texture::{self as tx};
use lucinanet_raster_oracle as o;

const TEXTURES: usize = 5000;
const COORDS: usize = 24;

fn all_formats(r: &mut Rng) -> (u32, i32, i32) {
    let f = FORMATS[r.below(FORMATS.len() as u64) as usize].0;
    (f, side(r), side(r))
}

/// An integer coordinate around `0..n`, sometimes far outside (including where SSE2's 16-bit clamp differs).
fn icoord(r: &mut Rng, n: i32) -> i32 {
    match r.below(16) {
        0 => [i32::MIN, i32::MAX, -1, 0x1_8000, -0x1_8000, 0x7FFF, 0x8000, 0x10000, -70000][r.below(9) as usize],
        1 => r.next() as i32,
        _ => r.int(-4, n + 4),
    }
}

/// A float coordinate around `0..n` (texels) or `0..1` (normalised), with edge values.
fn fcoord(r: &mut Rng, n: i32, normalised: bool) -> f32 {
    let scale = if normalised { 1.0 / n as f32 } else { 1.0 };
    match r.below(20) {
        0 => [f32::NAN, f32::INFINITY, -f32::INFINITY, 1.0e9, -1.0e9, 0.0, -0.0][r.below(7) as usize],
        1 => (r.int(-2, n + 2) as f32 + 0.5) * scale,
        2 => r.int(-2, n + 2) as f32 * scale,
        _ => r.range(-3.0, n as f32 + 3.0) * scale,
    }
}

fn lanes_i(r: &mut Rng, n: i32) -> [i32; 4] {
    [icoord(r, n), icoord(r, n), icoord(r, n), icoord(r, n)]
}

fn lanes_f(r: &mut Rng, n: i32, normalised: bool) -> [f32; 4] {
    if r.chance(4) {
        // Four adjacent pixels of a span, as shaders step them.
        let x = fcoord(r, n, normalised);
        let d = if normalised { 1.0 / n as f32 } else { 1.0 } * r.range(0.0, 2.0);
        [x, x + d, x + 2.0 * d, x + 3.0 * d]
    } else {
        [fcoord(r, n, normalised), fcoord(r, n, normalised), fcoord(r, n, normalised), fcoord(r, n, normalised)]
    }
}

fn vec4_out(v: vec4) -> [f32; 16] {
    let mut o = [0f32; 16];
    o[0..4].copy_from_slice(&v.x.0);
    o[4..8].copy_from_slice(&v.y.0);
    o[8..12].copy_from_slice(&v.z.0);
    o[12..16].copy_from_slice(&v.w.0);
    o
}

fn ivec4_out(v: ivec4) -> [i32; 16] {
    let mut o = [0i32; 16];
    o[0..4].copy_from_slice(&v.x.0);
    o[4..8].copy_from_slice(&v.y.0);
    o[8..12].copy_from_slice(&v.z.0);
    o[12..16].copy_from_slice(&v.w.0);
    o
}

fn check_f(what: &str, t: &Tex, args: &dyn core::fmt::Debug, rust: &[f32], cxx: &[f32]) {
    assert!(
        same_f32s(rust, cxx),
        "{what}: format {:x} {}x{} stride {} at {args:?}\n rust {rust:?}\n  c++ {cxx:?}",
        t.fmt,
        t.w,
        t.h,
        t.stride
    );
}

#[test]
fn texel_fetch_every_sampler_and_format() {
    let mut r = Rng::new("texelFetch");
    for _ in 0..TEXTURES {
        let (fmt, w, h) = all_formats(&mut r);
        if fmt == tx::GL_RGBA32I {
            continue;
        }
        let mut t = Tex::random(&mut r, fmt, w, h);
        let os = t.oracle_sampler(TextureFilter::NEAREST);
        let s2 = t.sampler2d(TextureFilter::NEAREST);
        let sr = t.sampler_rect(TextureFilter::NEAREST);
        let f = format_of(fmt);
        for _ in 0..COORDS {
            let (x, y) = (lanes_i(&mut r, w), lanes_i(&mut r, h));
            let p = ivec2::new(I32::new(x), I32::new(y));
            let mut kinds = vec![0];
            if f != TextureFormat::RGBA32F {
                kinds.push(1);
            }
            kinds.push(match f {
                TextureFormat::RGBA32F => 2,
                TextureFormat::RGBA8 => 3,
                TextureFormat::R8 => 4,
                TextureFormat::RG8 => 5,
                _ => 0,
            });
            for kind in kinds {
                let rust = match kind {
                    0 => texel_fetch_v(s2, p),
                    1 => tx::texelFetch(sr, p, 0),
                    2 => tx::texelFetch(sampler2DRGBA32F_impl(s2), p, 0),
                    3 => tx::texelFetch(sampler2DRGBA8_impl(s2), p, 0),
                    4 => tx::texelFetch(sampler2DR8_impl(s2), p, 0),
                    _ => tx::texelFetch(sampler2DRG8_impl(s2), p, 0),
                };
                let mut cxx = [0f32; 16];
                // SAFETY: the sampler points at the guarded buffer; the arrays have the lengths the shim writes.
                unsafe { o::t_texel_fetch(&os, kind, x.as_ptr(), y.as_ptr(), cxx.as_mut_ptr()) };
                check_f(&format!("texelFetch kind {kind}"), &t, &(x, y), &vec4_out(rust), &cxx);
            }
            // The scalar forms: RGBA8 and RGBA32F through sampler2D, and each typed sampler.
            let (sx, sy) = (x[0], y[0]);
            let ps = ivec2_scalar::new(sx, sy);
            let mut kinds = vec![];
            match f {
                TextureFormat::RGBA32F => kinds.extend([0, 2]),
                TextureFormat::RGBA8 => kinds.extend([0, 3]),
                TextureFormat::R8 => kinds.push(4),
                TextureFormat::RG8 => kinds.push(5),
                _ => {}
            }
            for kind in kinds {
                let rust = match kind {
                    0 => tx::texelFetch(s2, ps, 0),
                    2 => tx::texelFetch(sampler2DRGBA32F_impl(s2), ps, 0),
                    3 => tx::texelFetch(sampler2DRGBA8_impl(s2), ps, 0),
                    4 => tx::texelFetch(sampler2DR8_impl(s2), ps, 0),
                    _ => tx::texelFetch(sampler2DRG8_impl(s2), ps, 0),
                };
                let mut cxx = [0f32; 4];
                // SAFETY: as above.
                unsafe { o::t_texel_fetch_s(&os, kind, sx, sy, cxx.as_mut_ptr()) };
                check_f(
                    &format!("texelFetch scalar kind {kind}"),
                    &t,
                    &(sx, sy),
                    &[rust.x, rust.y, rust.z, rust.w],
                    &cxx,
                );
            }
        }
    }
}

fn texel_fetch_v(s: sampler2D_impl, p: ivec2) -> vec4 {
    tx::texelFetch(s, p, 0)
}

#[test]
fn texel_fetch_isampler() {
    let mut r = Rng::new("texelFetch isampler");
    for _ in 0..TEXTURES / 2 {
        let (w, h) = (side(&mut r), side(&mut r));
        let mut t = Tex::random(&mut r, tx::GL_RGBA32I, w, h);
        let os = t.oracle_sampler(TextureFilter::NEAREST);
        let s = isampler2D_impl { common: t.common() };
        for _ in 0..COORDS {
            let (x, y) = (lanes_i(&mut r, w), lanes_i(&mut r, h));
            let rust = ivec4_out(tx::texelFetch(s, ivec2::new(I32::new(x), I32::new(y)), 0));
            let mut cxx = [0i32; 16];
            // SAFETY: as in texel_fetch_every_sampler_and_format.
            unsafe { o::t_texel_fetch_i(&os, x.as_ptr(), y.as_ptr(), cxx.as_mut_ptr()) };
            assert_eq!(rust, cxx, "isampler texelFetch {w}x{h} at {x:?} {y:?}");
            let rs = tx::texelFetch(s, ivec2_scalar::new(x[0], y[0]), 0);
            let mut cs = [0i32; 4];
            // SAFETY: as above.
            unsafe { o::t_texel_fetch_i_s(&os, x[0], y[0], cs.as_mut_ptr()) };
            assert_eq!([rs.x, rs.y, rs.z, rs.w], cs, "isampler scalar texelFetch {w}x{h} at {} {}", x[0], y[0]);
        }
    }
}

#[test]
fn texel_fetch_ptr_and_unchecked() {
    let mut r = Rng::new("texelFetchPtr");
    for _ in 0..TEXTURES {
        let is_int = r.chance(2);
        let fmt = if is_int { tx::GL_RGBA32I } else { tx::GL_RGBA32F };
        let (w, h) = (side(&mut r), side(&mut r));
        let mut t = Tex::random(&mut r, fmt, w, h);
        let os = t.oracle_sampler(TextureFilter::NEAREST);
        let s2 = t.sampler2d(TextureFilter::NEAREST);
        let si = isampler2D_impl { common: t.common() };
        for _ in 0..COORDS {
            // Offsets are non-negative, as in WebRender's shaders: the C++ computes `x * 4 + y * stride` unsigned,
            // so a negative offset indexes 4 GiB away and faults.
            let (min_x, max_x) = (0, r.int(0, 7));
            let (min_y, max_y) = (0, r.int(0, 1));
            let (dx, dy) = (r.int(min_x, max_x), r.int(min_y, max_y));
            let near = |r: &mut Rng, n: i32| if r.chance(6) { icoord(r, n) } else { r.int(-3, n + 1) };
            let x = [near(&mut r, w), near(&mut r, w), near(&mut r, w), near(&mut r, w)];
            let y = [near(&mut r, h), near(&mut r, h), near(&mut r, h), near(&mut r, h)];
            let p = ivec2::new(I32::new(x), I32::new(y));
            let ps = ivec2_scalar::new(x[0], y[0]);
            let mut cxx = [0u8; 64];
            let mut cs = [0u8; 16];
            // SAFETY: as in texel_fetch_every_sampler_and_format; `cxx` has the 64 bytes a vec4/ivec4 needs.
            unsafe {
                o::t_texel_fetch_ptr(
                    &os,
                    is_int as i32,
                    x.as_ptr(),
                    y.as_ptr(),
                    min_x,
                    max_x,
                    min_y,
                    max_y,
                    dx,
                    dy,
                    cxx.as_mut_ptr(),
                );
                o::t_texel_fetch_ptr_s(
                    &os,
                    is_int as i32,
                    x[0],
                    y[0],
                    min_x,
                    max_x,
                    min_y,
                    max_y,
                    dx,
                    dy,
                    cs.as_mut_ptr(),
                );
            }
            let (rust, rs): (Vec<u8>, Vec<u8>) = if is_int {
                let f = tx::texelFetchPtr(si, p, min_x, max_x, min_y, max_y);
                let fs = tx::texelFetchPtr(si, ps, min_x, max_x, min_y, max_y);
                let v = tx::texelFetchUnchecked(si, f, dx, dy);
                let vs = tx::texelFetchUnchecked(si, fs, dx, dy);
                (le_bytes(&ivec4_out(v).map(|x| x as u32)), le_bytes(&[vs.x, vs.y, vs.z, vs.w].map(|x| x as u32)))
            } else {
                let f = tx::texelFetchPtr(s2, p, min_x, max_x, min_y, max_y);
                let fs = tx::texelFetchPtr(s2, ps, min_x, max_x, min_y, max_y);
                let v = tx::texelFetchUnchecked(s2, f, dx, dy);
                let vs = tx::texelFetchUnchecked(s2, fs, dx, dy);
                (le_bytes(&vec4_out(v).map(f32::to_bits)), le_bytes(&[vs.x, vs.y, vs.z, vs.w].map(f32::to_bits)))
            };
            assert_eq!(
                rust[..],
                cxx[..],
                "texelFetchPtr {w}x{h} int {is_int} at {x:?} {y:?} [{min_x},{max_x}]x[{min_y},{max_y}] +({dx},{dy})"
            );
            assert_eq!(rs[..], cs[..], "scalar texelFetchPtr {w}x{h} int {is_int} at {ps:?} +({dx},{dy})");
        }
    }
}

#[test]
fn texture_nearest_and_linear_every_format() {
    let mut r = Rng::new("texture");
    for _ in 0..TEXTURES * 2 {
        let (fmt, w, h) = all_formats(&mut r);
        if fmt == tx::GL_RGBA32I {
            continue;
        }
        let mut t = Tex::random(&mut r, fmt, w, h);
        let rect_ok = format_of(fmt) != TextureFormat::RGBA32F;
        for filter in [TextureFilter::NEAREST, TextureFilter::LINEAR] {
            let os = t.oracle_sampler(filter);
            let s2 = t.sampler2d(filter);
            let sr = t.sampler_rect(filter);
            for _ in 0..COORDS / 2 {
                for rect in [false, true] {
                    if rect && !rect_ok {
                        continue;
                    }
                    let x = lanes_f(&mut r, w, !rect);
                    let y = lanes_f(&mut r, h, !rect);
                    let p = vec2::new(Float::new(x), Float::new(y));
                    let rust = if rect { tx::texture(sr, p) } else { tx::texture(s2, p) };
                    let mut cxx = [0f32; 16];
                    // SAFETY: as in texel_fetch_every_sampler_and_format.
                    unsafe { o::t_texture(&os, rect as i32, x.as_ptr(), y.as_ptr(), cxx.as_mut_ptr()) };
                    check_f(&format!("texture {filter:?} rect {rect}"), &t, &(x, y), &vec4_out(rust), &cxx);
                }
                let (x, y) = (fcoord(&mut r, w, true), fcoord(&mut r, h, true));
                let rs = tx::texture(s2, vec2_scalar::new(x, y));
                let mut cs = [0f32; 4];
                // SAFETY: as above.
                unsafe { o::t_texture_s(&os, x, y, cs.as_mut_ptr()) };
                check_f(&format!("scalar texture {filter:?}"), &t, &(x, y), &[rs.x, rs.y, rs.z, rs.w], &cs);
            }
        }
    }
}

#[test]
fn packed_linear_filters() {
    let mut r = Rng::new("textureLinearPacked");
    // (kind, format, output bytes)
    let kinds: &[(i32, u32, usize)] = &[
        (0, tx::GL_RGBA8, 32),
        (1, tx::GL_RGBA8, 16),
        (2, tx::GL_RGBA8, 16),
        (9, tx::GL_RGBA8, 32),
        (3, tx::GL_R8, 8),
        (4, tx::GL_R8, 4),
        (5, tx::GL_RG8, 16),
        (6, tx::GL_RG8, 8),
        (10, tx::GL_RG8, 16),
        (7, tx::GL_R16, 8),
        (8, tx::GL_RG16, 16),
        (11, tx::GL_RGB_RAW_422_APPLE, 24),
    ];
    for _ in 0..TEXTURES * 2 {
        let &(kind, fmt, n) = &kinds[r.below(kinds.len() as u64) as usize];
        let (w, h) = (side(&mut r), side(&mut r));
        let mut t = Tex::random(&mut r, fmt, w, h);
        let os = t.oracle_sampler(TextureFilter::LINEAR);
        let s2 = t.sampler2d(TextureFilter::LINEAR);
        let sr = t.sampler_rect(TextureFilter::LINEAR);
        for _ in 0..COORDS {
            let q = |r: &mut Rng, n: i32| -> i32 {
                if r.chance(10) {
                    icoord(r, n)
                } else if kind == 2 {
                    r.int(-2, n + 2)
                } else {
                    r.int(-3 * 128, (n + 3) * 128)
                }
            };
            let x = [q(&mut r, w), q(&mut r, w), q(&mut r, w), q(&mut r, w)];
            let y = [q(&mut r, h), q(&mut r, h), q(&mut r, h), q(&mut r, h)];
            let i = ivec2::new(I32::new(x), I32::new(y));
            for rect in [false, true] {
                let mut cxx = [0u8; 32];
                // SAFETY: as in texel_fetch_every_sampler_and_format; `cxx` holds the largest output.
                unsafe { o::t_linear_packed(&os, rect as i32, kind, x.as_ptr(), y.as_ptr(), cxx.as_mut_ptr()) };
                let rust = if rect { packed(&sr, kind, i) } else { packed(&s2, kind, i) };
                assert_eq!(
                    &rust[..n],
                    &cxx[..n],
                    "linear kind {kind} format {fmt:x} {w}x{h} stride {} rect {rect} at {x:?} {y:?}",
                    t.stride
                );
            }
        }
    }
}

fn packed<S: tx::Sampler>(s: &S, kind: i32, i: ivec2) -> [u8; 32] {
    let mut o = [0u8; 32];
    let mut put = |b: &[u8]| o[..b.len()].copy_from_slice(b);
    match kind {
        0 => put(&as_bytes(tx::textureLinearUnpackedRGBA8(s, i))),
        1 => put(&as_bytes(tx::textureLinearPackedRGBA8(s, i))),
        2 => put(&as_bytes(tx::textureNearestPackedRGBA8(s, i))),
        3 => put(&as_bytes(tx::textureLinearUnpackedR8(s, i))),
        4 => put(&as_bytes(tx::textureLinearPackedR8(s, i))),
        5 => put(&as_bytes(tx::textureLinearUnpackedRG8(s, i))),
        6 => put(&as_bytes(tx::textureLinearPackedRG8(s, i))),
        7 => put(&as_bytes(tx::textureLinearUnpackedR16(s, i))),
        8 => put(&as_bytes(tx::textureLinearUnpackedRG16(s, i))),
        9 => {
            let p = tx::textureLinearPlanarRGBA8(s, i);
            put(&[as_bytes(p.rg), as_bytes(p.ba)].concat());
        }
        10 => put(&as_bytes(tx::textureLinearPlanarRG8(s, i).rg)),
        _ => {
            let p = tx::textureLinearPlanarYUY2(s, i);
            let mut b = [0u8; 24];
            b[0..8].copy_from_slice(&as_bytes(p.y));
            b[8..16].copy_from_slice(&as_bytes(p.u));
            b[16..24].copy_from_slice(&as_bytes(p.v));
            put(&b);
        }
    }
    o
}

#[test]
fn gaussian_blur_taps() {
    let mut r = Rng::new("gaussianBlur");
    for _ in 0..TEXTURES {
        let rgba8 = r.chance(2);
        let fmt = if rgba8 { tx::GL_RGBA8 } else { tx::GL_R8 };
        let (w, h) = (side(&mut r), side(&mut r));
        let mut t = Tex::random(&mut r, fmt, w, h);
        let os = t.oracle_sampler(TextureFilter::NEAREST);
        let s = t.sampler2d(TextureFilter::NEAREST);
        for _ in 0..COORDS / 2 {
            let vertical = r.chance(2);
            let (x, y) = (r.int(0, w - 1), r.int(0, h - 1));
            let n = if vertical { h } else { w };
            let lo = r.int(-3, n);
            let hi = r.int(lo, n + 3);
            let radius = r.int(0, 24);
            let coeff = r.range(0.01, 1.0);
            let step = r.range(0.3, 1.0);
            let i = ivec2_scalar::new(x, y);
            let mut cxx = [0u16; 16];
            // SAFETY: as in texel_fetch_every_sampler_and_format; `cxx` holds 16 lanes.
            unsafe {
                o::t_gaussian(&os, rgba8 as i32, vertical as i32, x, y, lo, hi, radius, coeff, step, cxx.as_mut_ptr())
            };
            let rust: Vec<u16> = match (rgba8, vertical) {
                (true, false) => tx::gaussianBlurHorizontal_RGBA8::<_, false>(&s, &i, lo, hi, radius, coeff, step).0.to_vec(),
                (true, true) => tx::gaussianBlurVertical_RGBA8::<_, false>(&s, &i, lo, hi, radius, coeff, step).0.to_vec(),
                (false, false) => tx::gaussianBlurHorizontal_R8::<_, false>(&s, &i, lo, hi, radius, coeff, step).0.to_vec(),
                (false, true) => tx::gaussianBlurVertical_R8::<_, false>(&s, &i, lo, hi, radius, coeff, step).0.to_vec(),
            };
            assert_eq!(
                rust[..],
                cxx[..rust.len()],
                "gaussian rgba8 {rgba8} vertical {vertical} {w}x{h} at ({x},{y}) [{lo},{hi}] r {radius} c {coeff} s {step}"
            );
        }
    }
}

#[test]
fn delayed_clears() {
    let mut r = Rng::new("prepare_texture");
    for _ in 0..TEXTURES {
        let fmt = [tx::GL_RGBA8, tx::GL_R8, tx::GL_RG8][r.below(3) as usize];
        let w = side(&mut r);
        let h = if r.chance(3) { r.int(1, 140) } else { side(&mut r) };
        let mut t = Tex::random(&mut r, fmt, w, h);
        let (rows, pending) = random_delay(&mut r, h);
        let val = r.next() as u32;
        t.delay = Some((rows, pending, val));
        let skip = if r.chance(3) { None } else { Some(rect_around(&mut r, w, h)) };
        let mut c = t.clone();
        let ot = c.oracle();
        let (mut delay, mut has) = (0i32, 0i32);
        let mut out_rows = vec![0u32; ((h + 31) / 32) as usize];
        // SAFETY: the texture points at `c`'s guarded buffer and rows, alive for the call.
        unsafe {
            o::t_prepare_texture(
                &ot,
                skip.as_ref().map_or(core::ptr::null(), |s| s.as_ptr()),
                &mut delay,
                out_rows.as_mut_ptr(),
                &mut has,
            )
        };
        let mut rt = t.rust();
        let skip_r = skip.map(|s| tx::IntRect::new(s[0], s[1], s[2], s[3]));
        tx::prepare_texture(&mut rt, skip_r.as_ref());
        let ctx = format!("format {fmt:x} {w}x{h} stride {} skip {skip:?}", t.stride);
        assert_eq!(rt.delay_clear, delay, "delay_clear {ctx}");
        assert_eq!(rt.cleared_rows.is_some(), has != 0, "cleared_rows {ctx}");
        if let Some(rows) = &rt.cleared_rows {
            assert_eq!(rows, &out_rows, "cleared_rows {ctx}");
        }
        drop(rt);
        assert!(t.buf.all() == c.buf.all(), "buffer after prepare_texture {ctx}");
    }
}

#[test]
fn lookup_sampler_binds_like_init_sampler() {
    // lookup_sampler on a bound texture gives the sampler init_sampler + init_filter build (nearest under 2 pixels);
    // sampling with it then matches the C++ sampler made the same way.
    let mut r = Rng::new("lookup_sampler");
    for _ in 0..200 {
        let (fmt, w, h) = all_formats(&mut r);
        if fmt == tx::GL_RGBA32I || fmt == tx::GL_RGBA32F {
            continue;
        }
        let mut t = Tex::random(&mut r, fmt, w, h);
        t.mag_filter = if r.chance(2) { tx::GL_LINEAR } else { tx::GL_NEAREST };
        let filter =
            if w >= 2 && t.mag_filter == tx::GL_LINEAR { TextureFilter::LINEAR } else { TextureFilter::NEAREST };
        let os = t.oracle_sampler(filter);
        let tex = t.rust();
        let mut ctx = lucinanet_raster::state::RasterState::default();
        let slot = r.int(0, 15);
        ctx.texture_units[slot as usize].texture_2d = tex.sampler_source();
        let mut s = sampler2D::default();
        let s = tx::lookup_sampler(&mut ctx, &mut s, slot);
        assert_eq!(s.filter.filter, filter);
        assert_eq!((s.width, s.height, s.stride, s.format), (w as u32, h as u32, t.sampler_stride(), format_of(fmt)));
        let x = lanes_f(&mut r, w, true);
        let y = lanes_f(&mut r, h, true);
        let rust = vec4_out(tx::texture(s, vec2::new(Float::new(x), Float::new(y))));
        let mut cxx = [0f32; 16];
        // SAFETY: as in texel_fetch_every_sampler_and_format.
        unsafe { o::t_texture(&os, 0, x.as_ptr(), y.as_ptr(), cxx.as_mut_ptr()) };
        check_f("texture after lookup_sampler", &t, &(x, y), &rust, &cxx);
        // An empty unit gives the 1x1 transparent black texture.
        let mut n = sampler2D::default();
        let n = tx::lookup_sampler(&mut ctx, &mut n, (slot + 1) % 16);
        assert_eq!(
            (n.width, n.height, n.format, n.filter.filter),
            (1, 1, TextureFormat::RGBA8, TextureFilter::NEAREST)
        );
        drop(tex);
    }
}
