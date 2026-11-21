/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The fast and downscale linear filters' unchecked texel loads (`ext.rs`): the dispatch checks
//! `rules::linear_reads_in_bounds` once per span and runs the filter with `UNCHECKED = true` only when the
//! whole texel range its loop reads lies inside the sampler's bytes. These cases drive the dispatch
//! (`blend_texture_linear_force_checked`) on a texture whose last row ends exactly at the end of its buffer,
//! with a span that starts left of `min_uv`, and require the unchecked path to give the same bytes as the
//! checked one. The bytes after the buffer are a pattern the sampler does not own, so a load past `len`
//! changes the output (the checked loads read zero there); in debug builds `tex_signed_unchecked`'s
//! `debug_assert!` also fires on any out-of-bounds read.

use lucinanet_raster::blend::NoColor;
use lucinanet_raster::ext::{blend_texture_linear_force_checked, LinearFilter};
use lucinanet_raster::glsl::{sampler2D_impl, samplerCommon, vec2, vec4_scalar, Float, TextureFilter, TextureFormat};
use lucinanet_raster::program::FragmentShaderState;
use lucinanet_raster::state::RasterState;

const W: u32 = 32;
const H: u32 = 4;

/// A texture whose last row ends exactly at the end of its buffer: `len` bytes, no padding. The 64 bytes
/// after the buffer are a pattern the sampler does not own.
struct Tex {
    data: Vec<u8>,
    len: usize,
    stride: u32,
    format: TextureFormat,
}

fn tex(format: TextureFormat) -> Tex {
    let bpp = match format {
        TextureFormat::RGBA8 => 4,
        TextureFormat::R8 => 1,
        _ => unreachable!(),
    };
    // The stride is in dwords for RGBA8 and in bytes for R8, so a row is `W` units either way.
    let stride = W;
    let len = (W * H * bpp) as usize;
    let mut data = vec![0u8; len + 64];
    for (i, b) in data[..len].iter_mut().enumerate() {
        *b = (i as u32).wrapping_mul(0x9E37_79B9) as u8;
    }
    for b in data[len..].iter_mut() {
        *b = 0xA5;
    }
    Tex { data, len, stride, format }
}

impl Tex {
    fn sampler(&self) -> sampler2D_impl {
        // SAFETY: `data` holds `len` readable bytes and outlives the sampler.
        let common = unsafe {
            samplerCommon::from_raw(self.data.as_ptr() as *const u32, self.len, self.stride, H, W, self.format)
        };
        let mut s = sampler2D_impl { common, ..Default::default() };
        s.filter.filter = TextureFilter::LINEAR;
        s
    }
}

fn lanes(p: &[f32]) -> Float {
    Float::new([p[0], p[1], p[2], p[3]])
}
fn v2l(p: &[f32; 8]) -> vec2 {
    vec2::new(lanes(&p[0..4]), lanes(&p[4..8]))
}
fn v4s(p: &[f32; 4]) -> vec4_scalar {
    vec4_scalar::new(p[0], p[1], p[2], p[3])
}

/// Run the dispatch over an RGBA8 span of `span` pixels at `dest` (which has slack for the whole chunks the
/// filters commit past the span).
fn run_rgba8(
    s: &sampler2D_impl,
    p: [f32; 8],
    uv_rect: [f32; 4],
    span: i32,
    filter: LinearFilter,
    force_checked: bool,
    dest: &mut [u8],
) -> (i32, bool) {
    let ctx = RasterState::default();
    let fs = FragmentShaderState::default();
    // SAFETY: `dest` is valid for `span` RGBA8 pixels plus the chunk slack, and the sampler's buffer
    // outlives the call.
    unsafe {
        blend_texture_linear_force_checked::<false, _, NoColor, *mut u32>(
            &ctx,
            &fs,
            s,
            v2l(&p),
            span,
            &v4s(&uv_rect),
            NoColor,
            dest.as_mut_ptr() as *mut u32,
            filter,
            force_checked,
        )
    }
}

/// Run the dispatch over an R8 span of `span` pixels at `dest`.
fn run_r8(
    s: &sampler2D_impl,
    p: [f32; 8],
    uv_rect: [f32; 4],
    span: i32,
    filter: LinearFilter,
    force_checked: bool,
    dest: &mut [u8],
) -> (i32, bool) {
    let ctx = RasterState::default();
    let fs = FragmentShaderState::default();
    // SAFETY: as `run_rgba8`, for R8 pixels.
    unsafe {
        blend_texture_linear_force_checked::<false, _, NoColor, *mut u8>(
            &ctx,
            &fs,
            s,
            v2l(&p),
            span,
            &v4s(&uv_rect),
            NoColor,
            dest.as_mut_ptr(),
            filter,
            force_checked,
        )
    }
}

#[test]
fn linear_unchecked_matches_checked() {
    let px = 1.0 / W as f32;
    // (name, format, p, uv_rect, span, filter, whether the dispatch must use the unchecked loads)
    type Case = (&'static str, TextureFormat, [f32; 8], [f32; 4], i32, LinearFilter, bool);
    let cases: [Case; 5] = [
        // 1:1 fast filter on the last row, span starting left of min_uv (uv_rect.x = 0.25): the rule passes
        // exactly at the end of the buffer (max(row0, row1) + 4*last + 4 == len_texels), so the loads are
        // unchecked and read the last texels of the buffer.
        (
            "fast, last row, span left of min_uv",
            TextureFormat::RGBA8,
            [0.0, px, 2.0 * px, 3.0 * px, 0.75, 0.75, 0.75, 0.75],
            [0.25, 0.0, 1.0, 1.0],
            40,
            LinearFilter::LINEAR_FILTER_FAST,
            true,
        ),
        // The same on row 2, where row1 = row0 + stride is the larger of the two rows.
        (
            "fast, row 1 of 3, row1 past row0",
            TextureFormat::RGBA8,
            [0.0, px, 2.0 * px, 3.0 * px, 0.5, 0.5, 0.5, 0.5],
            [0.25, 0.0, 1.0, 1.0],
            40,
            LinearFilter::LINEAR_FILTER_FAST,
            true,
        ),
        // 2x downscale on the last row, span left of min_uv.
        (
            "downscale, last row, span left of min_uv",
            TextureFormat::RGBA8,
            [0.0, 2.0 * px, 4.0 * px, 6.0 * px, 0.75, 0.75, 0.75, 0.75],
            [0.25, 0.0, 1.0, 1.0],
            40,
            LinearFilter::LINEAR_FILTER_DOWNSCALE,
            true,
        ),
        // R8 fast filter on the last row, span left of min_uv.
        (
            "r8 fast, last row, span left of min_uv",
            TextureFormat::R8,
            [0.0, px, 2.0 * px, 3.0 * px, 0.75, 0.75, 0.75, 0.75],
            [0.25, 0.0, 1.0, 1.0],
            40,
            LinearFilter::LINEAR_FILTER_FAST,
            true,
        ),
        // A span that starts left of min_uv with a step so small that the `before` block computes 0 pixels
        // (`int(ceil(before_dist / uv_step.x))` overflows), so the fast filter runs from min_uv with an
        // `inside` measured from the unclamped uv: its last chunk would read past the last row, which ends
        // exactly at the end of the buffer, so the rule must refuse and the loads stay checked.
        (
            "fast, before block under-advanced",
            TextureFormat::RGBA8,
            [0.0, 1e-11, 2e-11, 3e-11, 0.75, 0.75, 0.75, 0.75],
            [0.25, 0.0, 1.0, 1.0],
            40,
            LinearFilter::LINEAR_FILTER_FAST,
            false,
        ),
    ];
    for (name, format, p, uv_rect, span, filter, expect_unchecked) in cases {
        let t = tex(format);
        let s = t.sampler();
        let bpp = if format == TextureFormat::RGBA8 { 4 } else { 1 };
        let mut a = vec![0x5Au8; span as usize * bpp + 64];
        let mut b = a.clone();
        let run = |dest: &mut [u8], force_checked: bool| {
            if bpp == 4 {
                run_rgba8(&s, p, uv_rect, span, filter, force_checked, dest)
            } else {
                run_r8(&s, p, uv_rect, span, filter, force_checked, dest)
            }
        };
        let (ra, unchecked) = run(&mut a, false);
        let (rb, forced) = run(&mut b, true);
        assert_eq!(ra, rb, "{name}: the span left differs");
        assert!(!forced, "{name}: the forced-checked run used unchecked loads");
        assert_eq!(unchecked, expect_unchecked, "{name}: dispatch used unchecked loads = {unchecked}");
        assert_eq!(a, b, "{name}: the unchecked and checked outputs differ");
    }
}
