/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Bit-exactness of `lucinanet_raster::composite` against swgl's composite.h (`tests/oracle/shim_t.cc`):
//! `scale_blit` and `linear_blit` (opaque and composited), `Composite` (opaque or not, flips, clips, filters),
//! `CompositeYUV` (every color space, 8-bit R8 and 10/12/16-bit R16 planes, chroma at full and half resolution),
//! `ApplyMask` and `BlitFramebuffer` (through the C++ GL entry points), on random textures and rects including
//! empty and partly or wholly outside ones, from fixed seeds. The whole destination buffer, guard bands included,
//! must come out identical, and so must the sources (which only delayed clears change).

mod common;

use common::*;
use lucinanet_raster::composite::{self as cp, YUVRangedColorSpace};
use lucinanet_raster::texture::{self as tx, IntRect};
use lucinanet_raster_oracle as o;

const CASES: usize = 10_000;
const RENDERABLE: [u32; 3] = [tx::GL_R8, tx::GL_RG8, tx::GL_RGBA8];

fn rtex(r: &mut Rng, fmt: u32) -> Tex {
    let (w, h) = (side(r), side(r));
    Tex::random(r, fmt, w, h)
}

fn ir(a: [i32; 4]) -> IntRect {
    IntRect::new(a[0], a[1], a[2], a[3])
}

fn nonempty(a: &[i32; 4]) -> bool {
    a[2] > a[0] && a[3] > a[1]
}

/// Sometimes an offset and a pending delayed clear on a texture.
fn decorate(r: &mut Rng, t: &mut Tex, delay: bool) {
    if r.chance(4) {
        t.off = (r.int(-3, 3), r.int(-3, 3));
    }
    if delay && r.chance(5) && RENDERABLE.contains(&t.fmt) {
        let (rows, pending) = random_delay(r, t.h);
        t.delay = Some((rows, pending, r.next() as u32));
    }
}

/// Runs `cxx` on copies of the textures for the C++ and `rust` on copies for the Rust, then requires every buffer
/// to be identical.
fn compare(what: &str, texs: &[&Tex], cxx: impl FnOnce(&mut [Tex]), rust: impl FnOnce(&mut [Tex])) {
    let mut c: Vec<Tex> = texs.iter().map(|t| (*t).clone()).collect();
    let mut rs: Vec<Tex> = texs.iter().map(|t| (*t).clone()).collect();
    cxx(&mut c);
    rust(&mut rs);
    for (k, (a, b)) in c.iter().zip(&rs).enumerate() {
        if a.buf.all() != b.buf.all() {
            let first = a.buf.all().iter().zip(b.buf.all()).position(|(x, y)| x != y).unwrap();
            let t = texs[k];
            let px = (first as isize - GUARD as isize) as f64 / t.stride as f64;
            panic!(
                "{what}: texture {k} (format {:x} {}x{} stride {} offset {:?}) differs first at byte {} (row {px:.2}): \
                 c++ {} rust {}\n textures: {}",
                t.fmt,
                t.w,
                t.h,
                t.stride,
                t.off,
                first as isize - GUARD as isize,
                a.buf.all()[first],
                b.buf.all()[first],
                texs.iter().map(|t| format!("{:x} {}x{} s{} off{:?} delay {}", t.fmt, t.w, t.h, t.stride, t.off, t.delay.is_some())).collect::<Vec<_>>().join("; ")
            );
        }
    }
}

#[test]
fn scale_blit_every_format_pair() {
    let mut r = Rng::new("scale_blit");
    for case in 0..CASES {
        let composite = r.chance(3);
        let (sf, df) = if composite {
            (tx::GL_RGBA8, tx::GL_RGBA8)
        } else {
            (RENDERABLE[r.below(3) as usize], RENDERABLE[r.below(3) as usize])
        };
        let mut src = rtex(&mut r, sf);
        let mut dst = rtex(&mut r, df);
        decorate(&mut r, &mut src, false);
        decorate(&mut r, &mut dst, false);
        let src_req = rect_around(&mut r, src.w, src.h);
        let dst_req = rect_around(&mut r, dst.w, dst.h);
        // scale_blit divides by the request sizes: its callers never pass empty ones.
        if !nonempty(&src_req) || !nonempty(&dst_req) {
            continue;
        }
        let clip = rect_around(&mut r, dst_req[2] - dst_req[0], dst_req[3] - dst_req[1]);
        let invert_y = r.chance(2);
        compare(
            &format!(
                "case {case} scale_blit<{composite}> {src_req:?} -> {dst_req:?} clip {clip:?} invert_y {invert_y}"
            ),
            &[&src, &dst],
            |t| {
                let (a, b) = (t[0].oracle(), t[1].oracle());
                // SAFETY: the textures point at the copies' guarded buffers, alive for the call.
                unsafe {
                    o::t_scale_blit(
                        &a,
                        src_req.as_ptr(),
                        &b,
                        dst_req.as_ptr(),
                        composite as i32,
                        invert_y as i32,
                        clip.as_ptr(),
                    )
                }
            },
            |t| {
                let (mut a, mut b) = (t[0].rust(), t[1].rust());
                if composite {
                    cp::scale_blit::<true>(&mut a, &ir(src_req), &mut b, &ir(dst_req), invert_y, &ir(clip));
                } else {
                    cp::scale_blit::<false>(&mut a, &ir(src_req), &mut b, &ir(dst_req), invert_y, &ir(clip));
                }
            },
        );
    }
}

#[test]
fn linear_blit_every_format() {
    let mut r = Rng::new("linear_blit");
    for case in 0..CASES {
        let composite = r.chance(3);
        let f = if composite { tx::GL_RGBA8 } else { RENDERABLE[r.below(3) as usize] };
        let sw = r.int(2, 70).min(if r.chance(3) { 2 } else { 70 });
        let sh = side(&mut r);
        let mut src = Tex::random(&mut r, f, sw, sh);
        let mut dst = rtex(&mut r, f);
        decorate(&mut r, &mut src, true);
        decorate(&mut r, &mut dst, false);
        let src_req = rect_around(&mut r, src.w, src.h);
        let dst_req = rect_around(&mut r, dst.w, dst.h);
        if !nonempty(&src_req) || !nonempty(&dst_req) {
            continue;
        }
        let clip = rect_around(&mut r, dst_req[2] - dst_req[0], dst_req[3] - dst_req[1]);
        let (ix, iy) = (r.chance(2), r.chance(2));
        compare(
            &format!("case {case} linear_blit<{composite}> {src_req:?} -> {dst_req:?} clip {clip:?} flips {ix} {iy}"),
            &[&src, &dst],
            |t| {
                let (a, b) = (t[0].oracle(), t[1].oracle());
                // SAFETY: as in scale_blit_every_format_pair; `a.rows` is the copy's, alive for the call.
                unsafe {
                    o::t_linear_blit(
                        &a,
                        src_req.as_ptr(),
                        &b,
                        dst_req.as_ptr(),
                        composite as i32,
                        ix as i32,
                        iy as i32,
                        clip.as_ptr(),
                    )
                }
            },
            |t| {
                let (mut a, mut b) = (t[0].rust(), t[1].rust());
                if composite {
                    cp::linear_blit::<true>(&mut a, &ir(src_req), &mut b, &ir(dst_req), ix, iy, &ir(clip));
                } else {
                    cp::linear_blit::<false>(&mut a, &ir(src_req), &mut b, &ir(dst_req), ix, iy, &ir(clip));
                }
            },
        );
    }
}

/// A size for a request: often the texture's, sometimes scaled, sometimes empty or negative.
fn req_size(r: &mut Rng, n: i32) -> i32 {
    match r.below(10) {
        0 => r.int(-3, 0),
        1 | 2 => n,
        _ => r.int(1, 2 * n + 4),
    }
}

#[test]
fn composite_opaque_and_blended() {
    let mut r = Rng::new("Composite");
    for case in 0..CASES {
        let mut src = rtex(&mut r, tx::GL_RGBA8);
        let mut dst = rtex(&mut r, tx::GL_RGBA8);
        decorate(&mut r, &mut src, true);
        decorate(&mut r, &mut dst, false);
        let (sx, sy) = (r.int(-4, src.w), r.int(-4, src.h));
        let (sw, sh) = (req_size(&mut r, src.w), req_size(&mut r, src.h));
        let (dx, dy) = (r.int(-4, dst.w), r.int(-4, dst.h));
        let (dw, dh) = if r.chance(3) { (sw, sh) } else { (req_size(&mut r, dst.w), req_size(&mut r, dst.h)) };
        let (opaque, fx, fy) = (r.chance(2), r.chance(2), r.chance(2));
        let filter = if r.chance(2) { tx::GL_LINEAR } else { tx::GL_NEAREST };
        let c = rect_around(&mut r, dst.w, dst.h);
        let (cx, cy, cw, ch) = (c[0], c[1], c[2] - c[0], c[3] - c[1]);
        let a: [i32; 16] =
            [sx, sy, sw, sh, dx, dy, dw, dh, opaque as i32, fx as i32, fy as i32, filter as i32, cx, cy, cw, ch];
        compare(
            &format!("case {case} Composite {a:?}"),
            &[&src, &dst],
            |t| {
                let (s, d) = (t[0].oracle(), t[1].oracle());
                // SAFETY: as in scale_blit_every_format_pair.
                unsafe { o::t_composite(&d, &s, a.as_ptr()) }
            },
            |t| {
                let (mut s, mut d) = (t[0].rust(), t[1].rust());
                cp::Composite(&mut d, &mut s, sx, sy, sw, sh, dx, dy, dw, dh, opaque, fx, fy, filter, cx, cy, cw, ch);
            },
        );
    }
}

#[test]
fn apply_mask() {
    let mut r = Rng::new("ApplyMask");
    for case in 0..CASES {
        let (w, h) = (side(&mut r), side(&mut r));
        let dst = Tex::random(&mut r, tx::GL_RGBA8, w, h);
        let mask = Tex::random(&mut r, tx::GL_R8, w, h);
        compare(
            &format!("case {case} ApplyMask {w}x{h}"),
            &[&dst, &mask],
            |t| {
                let (d, m) = (t[0].oracle(), t[1].oracle());
                // SAFETY: as in scale_blit_every_format_pair.
                unsafe { o::t_apply_mask(&d, &m) }
            },
            |t| {
                let (mut d, mut m) = (t[0].rust(), t[1].rust());
                cp::ApplyMask(&mut d, &mut m);
            },
        );
    }
}

#[test]
fn composite_yuv_every_color_space_and_depth() {
    let mut r = Rng::new("CompositeYUV");
    for case in 0..CASES {
        let cs = r.int(0, 6);
        let depth = [8, 8, 10, 12, 16][r.below(5) as usize];
        let fmt = if depth == 8 { tx::GL_R8 } else { tx::GL_R16 };
        let (yw, yh) = if r.chance(2) { (r.int(40, 70), side(&mut r)) } else { (side(&mut r), side(&mut r)) };
        let half = r.below(3) != 0;
        let (cw, ch) = if half { ((yw + 1) / 2, (yh + 1) / 2) } else { (yw, yh) };
        let mut y = Tex::random(&mut r, fmt, yw, yh);
        let mut u = Tex::random(&mut r, fmt, cw, ch);
        let mut v = Tex::random(&mut r, fmt, cw, ch);
        if r.chance(4) {
            let off = (r.int(-3, 3), r.int(-3, 3));
            y.off = off;
            u.off = off;
            v.off = off;
        }
        let dw0 = side(&mut r).max(if r.chance(2) { 40 } else { 1 });
        let dh0 = side(&mut r);
        let mut dst = Tex::random(&mut r, tx::GL_RGBA8, dw0, dh0);
        decorate(&mut r, &mut dst, false);
        let (sx, sy) = (r.int(-4, yw), r.int(-4, yh));
        let (sw, sh) = (req_size(&mut r, yw), req_size(&mut r, yh));
        let (dx, dy) = (r.int(-4, dst.w), r.int(-4, dst.h));
        // Upscales (the half-resolution fast path), same size, and downscales.
        let (dw, dh) = match r.below(3) {
            0 => (sw, sh),
            1 => (sw * r.int(1, 3), sh * r.int(1, 3)),
            _ => (req_size(&mut r, dst.w), req_size(&mut r, dst.h)),
        };
        let (fx, fy) = (r.chance(4), r.chance(4));
        let c = rect_around(&mut r, dst.w, dst.h);
        let (cx, cy, cw2, ch2) = (c[0], c[1], c[2] - c[0], c[3] - c[1]);
        let a: [i32; 16] = [cs, depth, sx, sy, sw, sh, dx, dy, dw, dh, fx as i32, fy as i32, cx, cy, cw2, ch2];
        compare(
            &format!("case {case} CompositeYUV {a:?} planes {yw}x{yh} / {cw}x{ch}"),
            &[&dst, &y, &u, &v],
            |t| {
                let (d, y, u, v) = (t[0].oracle(), t[1].oracle(), t[2].oracle(), t[3].oracle());
                // SAFETY: as in scale_blit_every_format_pair.
                unsafe { o::t_composite_yuv(&d, &y, &u, &v, a.as_ptr()) }
            },
            |t| {
                let mut tt: Vec<_> = t.iter_mut().map(|t| t.rust()).collect();
                let (d, rest) = tt.split_first_mut().unwrap();
                let (y, rest) = rest.split_first_mut().unwrap();
                let (u, v) = rest.split_first_mut().unwrap();
                cp::CompositeYUV(
                    d,
                    y,
                    u,
                    &mut v[0],
                    YUVRangedColorSpace::from_u8(cs as u8).unwrap(),
                    depth as u32,
                    sx,
                    sy,
                    sw,
                    sh,
                    dx,
                    dy,
                    dw,
                    dh,
                    fx,
                    fy,
                    cx,
                    cy,
                    cw2,
                    ch2,
                );
            },
        );
    }
}

#[test]
fn blit_framebuffer_through_the_gl_entry_points() {
    let _lock = oracle_lock();
    let mut r = Rng::new("BlitFramebuffer");
    for case in 0..CASES {
        let sf = RENDERABLE[r.below(3) as usize];
        let df = if r.chance(2) { sf } else { RENDERABLE[r.below(3) as usize] };
        let mut src = rtex(&mut r, sf);
        let mut dst = rtex(&mut r, df);
        // SetTextureBuffer resets delayed clears, so neither texture has one here.
        decorate(&mut r, &mut src, false);
        decorate(&mut r, &mut dst, false);
        let s = rect_around(&mut r, src.w, src.h);
        let d = if r.chance(3) { s } else { rect_around(&mut r, dst.w, dst.h) };
        let flip = |r: &mut Rng, a: [i32; 4]| -> [i32; 4] {
            match r.below(4) {
                0 => [a[0], a[3], a[2], a[1]],
                1 => [a[2], a[1], a[0], a[3]],
                _ => a,
            }
        };
        let (s, d) = (flip(&mut r, s), flip(&mut r, d));
        let a = [s[0], s[1], s[2], s[3], d[0], d[1], d[2], d[3]];
        let filter = if r.chance(2) { tx::GL_LINEAR } else { tx::GL_NEAREST };
        compare(
            &format!("case {case} BlitFramebuffer {a:?} filter {filter:x}"),
            &[&src, &dst],
            |t| {
                let (s, d) = (t[0].oracle(), t[1].oracle());
                // SAFETY: as in scale_blit_every_format_pair; the C++ context is made and destroyed in the call.
                unsafe { o::t_blit_framebuffer(&s, &d, a.as_ptr(), filter) }
            },
            |t| {
                let (mut s, mut d) = (t[0].rust(), t[1].rust());
                cp::blit_framebuffer(&mut s, &mut d, a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], filter);
            },
        );
    }
}

#[test]
fn empty_and_outside_rects_leave_the_destination() {
    // Requests that are empty, inverted or wholly outside: nothing is written, on either side.
    let mut r = Rng::new("outside");
    for case in 0..CASES {
        let mut src = rtex(&mut r, tx::GL_RGBA8);
        let dst = rtex(&mut r, tx::GL_RGBA8);
        decorate(&mut r, &mut src, false);
        let far = |r: &mut Rng, n: i32| if r.chance(2) { r.int(n + 1, n + 50) } else { r.int(-60, -10) };
        let (sx, sy) = (far(&mut r, src.w), far(&mut r, src.h));
        let (dx, dy) = (far(&mut r, dst.w), far(&mut r, dst.h));
        let (sw, sh) = (r.int(-5, 5), r.int(-5, 5));
        let (dw, dh) = (r.int(-5, 5), r.int(-5, 5));
        let (opaque, fx, fy) = (r.chance(2), r.chance(2), r.chance(2));
        let filter = if r.chance(2) { tx::GL_LINEAR } else { tx::GL_NEAREST };
        let a: [i32; 16] =
            [sx, sy, sw, sh, dx, dy, dw, dh, opaque as i32, fx as i32, fy as i32, filter as i32, dx, dy, dw, dh];
        let before = dst.buf.all().to_vec();
        compare(
            &format!("case {case} Composite outside {a:?}"),
            &[&src, &dst],
            |t| {
                let (s, d) = (t[0].oracle(), t[1].oracle());
                // SAFETY: as in scale_blit_every_format_pair.
                unsafe { o::t_composite(&d, &s, a.as_ptr()) }
            },
            |t| {
                let (mut s, mut d) = (t[0].rust(), t[1].rust());
                cp::Composite(&mut d, &mut s, sx, sy, sw, sh, dx, dy, dw, dh, opaque, fx, fy, filter, dx, dy, dw, dh);
                drop(d);
                assert!(t[1].buf.all() == &before[..], "case {case}: Composite outside wrote to the destination");
            },
        );
    }
}
