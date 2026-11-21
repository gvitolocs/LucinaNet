/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! swgl_ext.h's gradients: the table lookups (`swgl_validateGradient`, `sampleGradient`), linear and radial
//! gradients over a table of merged stops (`commitLinearGradient`, `commitRadialGradient`) or over a tree of stop
//! offsets (`commitLinearGradientFromStops`, `commitRadialGradientFromStops`), with and without dithering.
//!
//! Tables live in an RGBA32F texture (`sGpuBufferF`); addresses are in floats from the texture's start. Reads go
//! through `samplerCommon::at32`, which reads zero outside the texture.

use crate::blend::{packRGBA8, BLEND_KEY_NONE};
use crate::state::RasterState;
use crate::texture::{Sampler, WideRGBA8};
use super::{rules, toi, v2, v2s, ExtColor, ExtPx};
use crate::glsl::{
    cast, ceil, clamp, combine, dFdx, dot, fract, inversesqrt, max, min, recip, repeat4, round_pixel, step,
    vec2, vec2_scalar, vec4, Float, I32, TextureFormat, U16, V16,
};
use crate::program::FragmentShaderState;
use pack_color::PackColorRGBA8;

mod pack_color {
    use crate::blend::PackColor;
    /// A color `packColor(swgl_OutRGBA8, color)` accepts.
    pub trait PackColorRGBA8: PackColor<*mut u32> {}
    impl<T: PackColor<*mut u32>> PackColorRGBA8 for T {}
}

/// One `GradientStops` entry: the start color and the step to the end color, RGBA32F.
#[derive(Clone, Copy)]
struct GradientStops {
    start_color: Float,
    step_color: Float,
}

impl GradientStops {
    #[inline(always)]
    fn at<S: Sampler>(s: &S, address: i32, index: i32) -> GradientStops {
        let c = s.common();
        let total = (c.height as i64) * (c.stride as i64);
        // Entries that fit in the texture from `address`: the C++ reads past it otherwise.
        let entries = ((total - address as i64) / 8).clamp(1, rules::SIDE);
        let off = if address >= 0 { rules::gradient_entry(address as i64, index as i64, entries) } else { address as i64 };
        let off = off as i32;
        GradientStops { start_color: c.at32::<Float>(off), step_color: c.at32::<Float>(off.wrapping_add(4)) }
    }
    /// `can_merge(next)`: the same step, so both are one larger gradient.
    #[inline(always)]
    fn can_merge(&self, next: &GradientStops) -> bool {
        let (a, b) = (self.step_color.0, next.step_color.0);
        a[0] == b[0] && a[1] == b[1] && a[2] == b[2] && a[3] == b[3]
    }
    #[inline(always)]
    fn interpolate(&self, offset: f32) -> Float {
        self.start_color + self.step_color * offset
    }
    #[inline(always)]
    fn end_color(&self) -> Float {
        self.start_color + self.step_color
    }
}

#[inline(always)]
fn zyxw(v: Float) -> Float {
    v.swizzle([2, 1, 0, 3])
}

/// `commit_span_chunk`: `commit_blend_span<BLEND>(buf, r)` at a chunk of the span: an `#[inline(always)]` fn, not a closure, so that
/// LLVM inlines the blend into the span loops as swgl's `PREFER_INLINE` `commit_blend_span` does.
#[inline(always)]
fn commit_span_chunk<const BLEND: bool>(ctx: &RasterState, fs: &FragmentShaderState, buf: *mut u32, r: WideRGBA8) {
    // SAFETY: a chunk inside the span: the loops below write `span` pixels in all.
    unsafe { <*mut u32 as ExtPx>::commit::<BLEND>(ctx, fs, buf, r) }
}

/// `swgl_validateGradient(sampler, address, entries)`: the float address of a table of `entries` entries at texel
/// `address` of an RGBA32F texture, or -1.
pub fn swgl_validateGradient<S: Sampler>(s: S, address: crate::glsl::ivec2_scalar, entries: i32) -> i32 {
    validate(&s, address, entries, false)
}

/// `swgl_validateGradientFromStops(sampler, address, entries)`: as [`swgl_validateGradient`] for `entries` colors
/// followed by their offsets, 4 to a texel.
pub fn swgl_validateGradientFromStops<S: Sampler>(s: S, address: crate::glsl::ivec2_scalar, entries: i32) -> i32 {
    validate(&s, address, entries, true)
}

fn validate<S: Sampler>(s: &S, a: crate::glsl::ivec2_scalar, entries: i32, from_stops: bool) -> i32 {
    let c = s.common();
    let (w, h, stride) = (c.width as i64, c.height as i64, c.stride as i64);
    // An RGBA32F sampler's stride is 4 floats a texel; anything else is not a table swgl could have uploaded.
    if c.format != TextureFormat::RGBA32F || w > rules::SIDE || h > rules::SIDE || stride < 4 * w || stride > 4 * rules::SIDE {
        return -1;
    }
    rules::gradient_address(a.x as i64, a.y as i64, entries as i64, from_stops, w, h, stride) as i32
}

/// `sampleGradient(sampler, address, entry)`: per lane, the table entry `int(entry)` blended by `fract(entry)`, as
/// BGRA8.
#[inline(always)]
fn sample_gradient_px<S: Sampler>(s: &S, address: i32, index: i32, offset: f32) -> I32 {
    round_pixel(zyxw(GradientStops::at(s, address, index).interpolate(offset)), 255.0)
}

pub(super) fn sample_gradient<S: Sampler>(s: &S, address: i32, entry: Float) -> WideRGBA8 {
    let index = cast(entry);
    let offset = entry - cast(index);
    combine(
        packRGBA8(sample_gradient_px(s, address, index.0[0], offset.0[0]), sample_gradient_px(s, address, index.0[1], offset.0[1])),
        packRGBA8(sample_gradient_px(s, address, index.0[2], offset.0[2]), sample_gradient_px(s, address, index.0[3], offset.0[3])),
    )
}

/// `swgl_commitGradientRGBA8(sampler, address, entry)`: one chunk from the table.
pub fn swgl_commitGradientRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    address: i32,
    entry: Float,
    fs: &mut FragmentShaderState,
) {
    super::commit_chunk::<*mut u32>(ctx, fs, sample_gradient(&s, address, entry));
}

/// `swgl_commitGradientColorRGBA8(sampler, address, entry, color)`: one chunk from the table scaled by `color`. The
/// C++ macro names `swgl_OutRGBA`, which does not exist, so no shader can use it; this is what it means.
pub fn swgl_commitGradientColorRGBA8<S: Sampler, K: PackColorRGBA8>(
    ctx: &mut RasterState,
    s: S,
    address: i32,
    entry: Float,
    color: K,
    fs: &mut FragmentShaderState,
) where
    K::Output: ExtColor<WideRGBA8>,
{
    let r = color.pack_color().apply(sample_gradient(&s, address, entry));
    super::commit_chunk::<*mut u32>(ctx, fs, r);
}

/// `ditherNoise`: noise added to four horizontally adjacent pixels, by row and column modulo 8.
#[rustfmt::skip]
static DITHER_NOISE: [[u16; 4]; 64] = {
    const T: [u16; 67] = [
        2, 194, 50, 242, 14, 206, 62, 254, 130, 66, 178, 114, 142, 78, 190, 126, 34, 226, 18, 210, 46, 238, 30,
        222, 162, 98, 146, 82, 174, 110, 158, 94, 10, 202, 58, 250, 6, 198, 54, 246, 138, 74, 186, 122, 134, 70,
        182, 118, 42, 234, 26, 218, 38, 230, 22, 214, 170, 106, 154, 90, 166, 102, 150, 86, 2, 194, 50,
    ];
    let mut n = [[0u16; 4]; 64];
    let mut i = 0;
    while i < 64 {
        n[i] = [T[i], T[i + 1], T[i + 2], T[i + 3]];
        i += 1;
    }
    n
};

/// `dither(color, fragCoordX, getDitherNoise(fragCoordY))`.
#[inline(always)]
fn dither(color: WideRGBA8, x: i32, y: i32) -> WideRGBA8 {
    let n = DITHER_NOISE[rules::dither_index(x, y)];
    let mut d = [0u16; 16];
    for p in 0..4 {
        d[p * 4] = n[p];
        d[p * 4 + 1] = n[p];
        d[p * 4 + 2] = n[p];
        d[p * 4 + 3] = 128;
    }
    color + V16::new(d)
}

/// `combine(CONVERT(round_pixel(colorF + deltaColorF * k, 1), U16), ...)` for `k` = 0, 0.25, 0.5, 0.75.
#[inline(always)]
fn ramp4_px(color_f: Float, delta_color_f: Float, k: f32) -> U16 {
    let v = if k == 0.0 { color_f } else { color_f + delta_color_f * k };
    round_pixel(v, 1.0).convert::<u16>()
}

#[inline(always)]
fn ramp4(color_f: Float, delta_color_f: Float) -> WideRGBA8 {
    combine(combine(ramp4_px(color_f, delta_color_f, 0.0), ramp4_px(color_f, delta_color_f, 0.25)), combine(ramp4_px(color_f, delta_color_f, 0.5), ramp4_px(color_f, delta_color_f, 0.75)))
}

/// `findGradientStopPair(offset, stops, numStops, prevOffset, nextOffset)`: walk the 5-ary tree of stop offsets
/// at float `address`; returns the first stop of the pair around `offset` and the pair's offsets.
#[inline]
fn find_gradient_stop_pair<S: Sampler>(s: &S, offset: f32, address: i32, num_stops: i32) -> (i32, f32, f32) {
    let c = s.common();
    let mut level_base_addr: i32 = 0;
    let mut level_stride: i32 = 1;
    let mut offset_in_level: i32 = 0;
    let mut index: i32 = 0;
    let mut index_stride: i64 = 1;
    while index_stride * 5 <= num_stops as i64 {
        index_stride *= 5;
    }
    let mut prev = 0.0f32;
    let mut next = 1.0f32;
    loop {
        let addr = address.wrapping_add(level_base_addr.wrapping_add(offset_in_level).wrapping_mul(4));
        // One load for the node's four offsets (one bounds check, not four; bytes outside read as 0 either way).
        let node = c.at32::<Float>(addr).0;
        let (s0, s1, s2, s3) = (node[0], node[1], node[2], node[3]);
        let mut part = 4;
        if s0 > offset {
            part = 0;
            next = s0;
        } else if s1 > offset {
            part = 1;
            prev = s0;
            next = s1;
        } else if s2 > offset {
            part = 2;
            prev = s1;
            next = s2;
        } else if s3 > offset {
            part = 3;
            prev = s2;
            next = s3;
        } else {
            prev = s3;
        }
        index = index.wrapping_add((part as i64 * index_stride) as i32);
        if index_stride == 1 {
            break;
        }
        index_stride /= 5;
        level_base_addr = level_base_addr.wrapping_add(level_stride);
        level_stride = level_stride.wrapping_mul(5);
        offset_in_level = offset_in_level.wrapping_mul(5).wrapping_add(part);
    }
    // Fewer than one stop only comes from a malformed table, where the C++ indexes before it.
    (rules::stop_pair(index as i64, (num_stops as i64).max(1)) as i32, prev, next)
}

/// `commitLinearGradient<BLEND, DITHER>`: the span of a linear gradient over a table of merged stops. False if the
/// step along the gradient is not finite (nothing drawn).
///
/// # Safety
/// The span contract of the ext module doc for `span` pixels from `buf`.
unsafe fn commit_linear_gradient<const BLEND: bool, const DITHER: bool, S: Sampler>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    address: i32,
    size: f32,
    tile_repeat: bool,
    gradient_repeat: bool,
    mut pos: vec2,
    scale_dir: &vec2_scalar,
    start_offset: f32,
    mut buf: *mut u32,
    mut span: i32,
    frag_coord: &vec4,
) -> bool {
    let stops = |i: i32| GradientStops::at(s, address, i);
    let pos_step = dFdx(pos) * 4.0f32;
    let delta = dot(pos_step, *scale_dir);
    if !delta.is_finite() {
        return false;
    }
    let mut cur_x = toi(frag_coord.x.0[0]);
    let cur_y = toi(frag_coord.y.0[0]);
    let mut dist_x = v2s(0.25 * span as f32, 0.0);
    let mut dist_y = dist_x;
    if tile_repeat {
        if pos_step.x != 0.0 {
            dist_x = v2s(step(0.0f32, pos_step.x), 1.0) * recip(pos_step.x);
        }
        if pos_step.y != 0.0 {
            dist_y = v2s(step(0.0f32, pos_step.y), 1.0) * recip(pos_step.y);
        }
    }

    while span > 0 {
        let mut chunks = 0.25 * span as f32;
        let mut repeat_pos = pos;
        if tile_repeat {
            repeat_pos = fract(pos);
            chunks = min(chunks, dist_x.x - repeat_pos.x.0[0] * dist_x.y);
            chunks = min(chunks, dist_y.x - repeat_pos.y.0[0] * dist_y.y);
        }
        let mut offset = repeat_pos.x * scale_dir.x + repeat_pos.y * scale_dir.y - start_offset;
        if gradient_repeat {
            offset = fract(offset);
        }
        let ox = offset.0[0];
        let start_entry: f32;
        let mut min_index: i32;
        let mut max_index: i32;
        if ox < 0.0 {
            start_entry = 0.0;
            min_index = toi(start_entry);
            max_index = min_index;
            if delta > 0.0 {
                chunks = min(chunks, -ox / delta);
            }
        } else if ox < 1.0 {
            start_entry = 1.0 + ox * size;
            if delta < 0.0 {
                chunks = min(chunks, -ox / delta);
            } else if delta > 0.0 {
                chunks = min(chunks, (1.0 - ox) / delta);
            }
            let end_entry = clamp(1.0 + (ox + delta * toi(chunks) as f32) * size, 0.0f32, 1.0 + size);
            min_index = toi(start_entry);
            max_index = min_index;
            if delta > 0.0 {
                while ((max_index.wrapping_add(1)) as f32) < end_entry && stops(max_index).can_merge(&stops(max_index + 1)) {
                    max_index += 1;
                }
                chunks = min(chunks, (max_index.wrapping_add(1) as f32 - start_entry) / (delta * size));
            } else if delta < 0.0 {
                while (min_index.wrapping_sub(1) as f32) > end_entry && stops(min_index - 1).can_merge(&stops(min_index)) {
                    min_index -= 1;
                }
                chunks = min(chunks, (min_index as f32 - start_entry) / (delta * size));
            }
        } else {
            start_entry = 1.0 + size;
            min_index = toi(start_entry);
            max_index = min_index;
            if delta < 0.0 {
                chunks = min(chunks, (1.0 - ox) / delta);
            }
        }
        if chunks >= 1.0 {
            let inside = toi(chunks);
            // Colors scaled to 0..0xFF00, BGRA, stepped in 16 bits and shifted to 8 on output.
            let min_color_f = zyxw(stops(min_index).start_color) * (0xFF00 as f32);
            let max_color_f = zyxw(stops(max_index).end_color()) * (0xFF00 as f32);
            let color_range_f = (max_color_f - min_color_f) * (1.0f32 / (max_index.wrapping_add(1).wrapping_sub(min_index)) as f32);
            let mut color_f = min_color_f + color_range_f * (start_entry - min_index as f32) + (0x80 as f32);
            let delta_color_f = color_range_f * (delta * size);
            let delta_color: WideRGBA8 = repeat4(round_pixel(delta_color_f, 1.0).convert::<u16>());
            let mut remaining = inside;
            loop {
                let mut color = ramp4(color_f, delta_color_f);
                // A segment at a time, so that rounded deltas do not overflow 8 bits.
                let segment = remaining.min(256 / 4);
                for _ in 0..segment.max(0) {
                    if DITHER {
                        commit_span_chunk::<BLEND>(ctx, fs, buf, dither(color, cur_x, cur_y) >> 8);
                        cur_x = cur_x.wrapping_add(4);
                    } else {
                        commit_span_chunk::<BLEND>(ctx, fs, buf, color >> 8);
                    }
                    color += delta_color;
                    buf = buf.wrapping_add(4);
                }
                remaining -= segment;
                if remaining <= 0 {
                    break;
                }
                color_f += delta_color_f * segment as f32;
            }
            span -= inside * 4;
            if span <= 0 {
                break;
            }
            // Probably a transition between stops next: fall through to per-sample lookups.
            pos = v2(pos.x + pos_step.x * inside as f32, pos.y + pos_step.y * inside as f32);
            repeat_pos = if tile_repeat { fract(pos) } else { pos };
            offset = repeat_pos.x * scale_dir.x + repeat_pos.y * scale_dir.y - start_offset;
            if gradient_repeat {
                offset = fract(offset);
            }
        }
        let entry = clamp(offset * size + 1.0f32, Float::splat(0.0), Float::splat(1.0 + size));
        if DITHER {
            let sample = sample_gradient(s, address, entry) << 8;
            commit_span_chunk::<BLEND>(ctx, fs, buf, dither(sample, cur_x, cur_y) >> 8);
            cur_x = cur_x.wrapping_add(4);
        } else {
            commit_span_chunk::<BLEND>(ctx, fs, buf, sample_gradient(s, address, entry));
        }
        span -= 4;
        buf = buf.wrapping_add(4);
        pos = v2(pos.x + pos_step.x, pos.y + pos_step.y);
    }
    true
}

/// `commitLinearGradientFromStops<BLEND, DITHER>`.
///
/// # Safety
/// The span contract of the ext module doc for `span` pixels from `buf`.
unsafe fn commit_linear_gradient_from_stops<const BLEND: bool, const DITHER: bool, S: Sampler>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    offsets_address: i32,
    colors_address: i32,
    stop_count: f32,
    gradient_repeat: bool,
    mut pos: vec2,
    scale_dir: &vec2_scalar,
    start_offset: f32,
    mut buf: *mut u32,
    mut span: i32,
    frag_coord: &vec4,
) -> bool {
    let c = s.common();
    let stop_color = |i: i32| c.at32::<Float>(colors_address.wrapping_add(i.wrapping_mul(4)));
    const CHUNK_SIZE: f32 = 4.0;
    let mut cur_x = toi(frag_coord.x.0[0]);
    let cur_y = toi(frag_coord.y.0[0]);
    let pos_step = dFdx(pos);
    let delta = dot(pos_step, *scale_dir);
    if !delta.is_finite() {
        return false;
    }

    while span > 0 {
        let mut sub_span = span as f32;
        let mut offset = pos.x * scale_dir.x + pos.y * scale_dir.y - start_offset;
        if gradient_repeat {
            offset = fract(offset);
        }
        let ox = offset.0[0];
        let mut stop_index: i32 = 0;
        let mut prev_offset = 0.0f32;
        let mut next_offset = 0.0f32;
        if ox < 0.0 {
            if delta > 0.0 {
                sub_span = min(sub_span, -ox / delta);
            }
        } else if ox >= 1.0 {
            stop_index = toi(stop_count - 1.0);
            if delta < 0.0 {
                sub_span = min(sub_span, (1.0 - ox) / delta);
            }
        } else {
            let (i, p, n) = find_gradient_stop_pair(s, ox, offsets_address, toi(stop_count));
            stop_index = i;
            prev_offset = p;
            next_offset = n;
            let offset_range = if delta > 0.0 { next_offset - ox } else { prev_offset - ox };
            sub_span = min(sub_span, offset_range / delta);
        }
        // At least a pixel.
        sub_span = max(ceil(sub_span), 1.0f32);

        let color_scale = (if DITHER { 0xFF00 as f32 } else { 255.0 }) * 256.0;
        let min_color_f = zyxw(stop_color(stop_index)) * color_scale;
        let max_color_f = zyxw(stop_color(stop_index.wrapping_add(1))) * color_scale;
        let delta_offset = next_offset - prev_offset;
        // C++ `1.0 / deltaOffset` is a double, rounded to float by the vector multiply.
        let color_range_f = if delta_offset == 0.0 {
            Float::splat(0.0)
        } else {
            (max_color_f - min_color_f) * ((1.0f64 / delta_offset as f64) as f32)
        };
        let mut color_f = min_color_f + color_range_f * (ox - prev_offset) + (0x80 as f32);
        let delta_color_f = color_range_f * delta * CHUNK_SIZE;
        let delta_color: WideRGBA8 = repeat4(round_pixel(delta_color_f, 1.0).convert::<u16>());
        let chunks = toi(sub_span) / 4;
        if chunks > 0 {
            let mut remaining = chunks;
            loop {
                let mut color = ramp4(color_f, delta_color_f);
                let segment = remaining.min(256 / 4);
                for _ in 0..segment {
                    if DITHER {
                        commit_span_chunk::<BLEND>(ctx, fs, buf, dither(color, cur_x, cur_y) >> 8);
                        cur_x = cur_x.wrapping_add(4);
                    } else {
                        commit_span_chunk::<BLEND>(ctx, fs, buf, color >> 8);
                    }
                    color += delta_color;
                    buf = buf.wrapping_add(4);
                }
                remaining -= segment;
                color_f += delta_color_f * segment as f32;
                if remaining <= 0 {
                    break;
                }
            }
            span -= chunks * 4;
            let k = chunks as f32;
            pos = v2(pos.x + pos_step.x * k * CHUNK_SIZE, pos.y + pos_step.y * k * CHUNK_SIZE);
        }
        // A partial chunk.
        let remainder = toi(sub_span - (chunks * 4) as f32);
        if remainder > 0 {
            let mut color = ramp4(color_f, delta_color_f);
            if DITHER {
                color = dither(color, cur_x, cur_y);
                cur_x = cur_x.wrapping_add(remainder);
            }
            // SAFETY: `remainder <= span` pixels (`sub_span <= span`).
            unsafe { <*mut u32 as ExtPx>::commit_len::<BLEND>(ctx, fs, buf, color >> 8, remainder) };
            buf = buf.wrapping_add(remainder as usize);
            span -= remainder;
            let k = remainder as f32;
            pos = v2(pos.x + pos_step.x * k, pos.y + pos_step.y * k);
        }
    }
    true
}

/// `fastSqrt<CLAMP>(v)` on lanes: `v * inversesqrt(v)`, as swgl on SSE2 and NEON.
#[inline(always)]
fn fast_sqrt<const CLAMP: bool>(v: Float) -> Float {
    let v = if CLAMP { max(v, Float::splat(1.0e-12)) } else { v };
    v * inversesqrt(v)
}

#[inline(always)]
fn fast_sqrt_scalar(v: f32) -> f32 {
    v * inversesqrt(v)
}

/// The constants of the radial gradient quadratic (`commitRadialGradient*`): `(invDelta, middleT, middleB)`.
#[inline(always)]
fn radial_coefficients(pos0: vec2_scalar, delta: vec2_scalar, delta_delta: f32, span: i32) -> (f32, f32, f32) {
    if delta_delta > 0.0 {
        let inv_delta = 1.0 / delta_delta;
        let middle_t = -dot(delta, pos0) * inv_delta;
        (inv_delta, middle_t, middle_t * middle_t - dot(pos0, pos0) * inv_delta)
    } else {
        // An invariant position: the quadratic always reduces to the end of the span.
        (0.0, span as f32, 0.0)
    }
}

/// `combine(packRGBA8(round_pixel(colorF + deltaColorF * offsetG.x, 1), ...))`.
#[inline(always)]
fn ramp_offsets_packed_px(color_f: Float, delta_color_f: Float, g: f32) -> I32 {
    round_pixel(color_f + delta_color_f * g, 1.0)
}

#[inline(always)]
fn ramp_offsets_packed(color_f: Float, delta_color_f: Float, g: Float) -> WideRGBA8 {
    combine(
        packRGBA8(ramp_offsets_packed_px(color_f, delta_color_f, g.0[0]), ramp_offsets_packed_px(color_f, delta_color_f, g.0[1])),
        packRGBA8(ramp_offsets_packed_px(color_f, delta_color_f, g.0[2]), ramp_offsets_packed_px(color_f, delta_color_f, g.0[3])),
    )
}

/// `combine(CONVERT(round_pixel(colorF + deltaColorF * offsetG.x, 1), U16), ...)`.
#[inline(always)]
fn ramp_offsets_wide_px(color_f: Float, delta_color_f: Float, g: f32) -> U16 {
    round_pixel(color_f + delta_color_f * g, 1.0).convert::<u16>()
}

#[inline(always)]
fn ramp_offsets_wide(color_f: Float, delta_color_f: Float, g: Float) -> WideRGBA8 {
    combine(
        combine(ramp_offsets_wide_px(color_f, delta_color_f, g.0[0]), ramp_offsets_wide_px(color_f, delta_color_f, g.0[1])),
        combine(ramp_offsets_wide_px(color_f, delta_color_f, g.0[2]), ramp_offsets_wide_px(color_f, delta_color_f, g.0[3])),
    )
}

/// `commitRadialGradient<BLEND, DITHER>`.
///
/// # Safety
/// The span contract of the ext module doc for `span` pixels from `buf`.
unsafe fn commit_radial_gradient<const BLEND: bool, const DITHER: bool, S: Sampler>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    address: i32,
    size: f32,
    repeat: bool,
    pos: vec2,
    radius: f32,
    mut buf: *mut u32,
    span: i32,
    frag_coord: &vec4,
) -> bool {
    let stops = |i: i32| GradientStops::at(s, address, i);
    let pos0 = v2s(pos.x.0[0], pos.y.0[0]);
    let mut delta = v2s(pos.x.0[1] - pos.x.0[0], pos.y.0[1] - pos.y.0[0]);
    let mut delta_delta = dot(delta, delta);
    if !delta_delta.is_finite() || !radius.is_finite() {
        return false;
    }
    let mut cur_x = toi(frag_coord.x.0[0]);
    let cur_y = toi(frag_coord.y.0[0]);
    let (inv_delta, middle_t, middle_b) = radial_coefficients(pos0, delta, delta_delta, span);
    // Search for merged stops only up to the middle or the end of the span, whichever is first.
    let mt = Float::new([middle_t, span as f32, 0.0, 0.0]);
    let me = v2(pos0.x + delta.x * mt, pos0.y + delta.y * mt);
    let middle_end_radius = fast_sqrt::<true>(dot(me, me));
    let middle_radius = if (span as f32) < middle_t { middle_end_radius.0[1] } else { middle_end_radius.0[0] };
    let end_radius = middle_end_radius.0[1];
    delta *= 4.0f32;
    delta_delta *= (4 * 4) as f32;
    let mut dot_pos = dot(pos, pos);
    let mut dot_pos_delta = 2.0f32 * dot(pos, vec2::from(delta)) + delta_delta;
    let delta_delta2 = 2.0 * delta_delta;

    let mut t: i32 = 0;
    while t < span {
        let mut offset = fast_sqrt::<true>(dot_pos) - radius;
        let mut start_radius = radius;
        if repeat {
            // radius + floor(offset), the radius at which the table starts.
            start_radius += offset.0[0];
            offset = fract(offset);
            start_radius -= offset.0[0];
        }
        let tf = t as f32;
        let mut intercept: f32 = -1.0;
        let mut min_index: i32 = 0;
        let mut max_index: i32 = toi(1.0 + size);
        let ox = offset.0[0];
        if ox < 0.0 {
            max_index = min_index;
            if tf >= middle_t {
                intercept = radius;
            }
        } else if ox < 1.0 {
            min_index = toi(1.0 + ox * size);
            max_index = min_index;
            let search_offset = (if tf >= middle_t { end_radius } else { middle_radius }) - start_radius;
            let search_index = toi(clamp(1.0 + size * search_offset, 1.0f32, size));
            if tf >= middle_t {
                while max_index.wrapping_add(1) <= search_index && stops(max_index).can_merge(&stops(max_index + 1)) {
                    max_index += 1;
                }
                intercept = max_index.wrapping_add(1) as f32;
            } else {
                while min_index.wrapping_sub(1) >= search_index && stops(min_index - 1).can_merge(&stops(min_index)) {
                    min_index -= 1;
                }
                intercept = min_index as f32;
            }
            intercept = clamp((intercept - 1.0) / size, 0.0f32, 1.0f32) + start_radius;
        } else {
            min_index = max_index;
            if tf < middle_t {
                intercept = radius + 1.0;
            }
        }
        let mut end_t: f32 = if tf >= middle_t { span as f32 } else { span.min(toi(middle_t)) as f32 };
        if intercept >= 0.0 {
            let b = middle_b + intercept * intercept * inv_delta;
            if b > 0.0 {
                let b = fast_sqrt_scalar(b);
                end_t = min(end_t, if tf >= middle_t { middle_t + b } else { middle_t - b });
            } else {
                // fastSqrt's imprecision can make the discriminant slightly negative: take it as 0.
                end_t = min(end_t, middle_t);
            }
        }
        if tf + 4.0 <= end_t {
            let inside = rules::chunks((toi(end_t - tf) as i64).clamp(0, rules::MAX_SPAN)).0 as i32;
            let scale = if DITHER { 0xFF00 as f32 } else { 255.0 };
            let min_color_f = zyxw(stops(min_index).start_color) * scale;
            let max_color_f = zyxw(stops(max_index).end_color()) * scale;
            let delta_color_f = (max_color_f - min_color_f) * (size / (max_index.wrapping_add(1).wrapping_sub(min_index)) as f32);
            let color_f = min_color_f - delta_color_f * (start_radius + (min_index.wrapping_sub(1)) as f32 / size);
            for _ in 0..inside / 4 {
                let g = fast_sqrt::<false>(dot_pos);
                if DITHER {
                    commit_span_chunk::<BLEND>(ctx, fs, buf, dither(ramp_offsets_wide(color_f, delta_color_f, g), cur_x, cur_y) >> 8);
                    cur_x = cur_x.wrapping_add(4);
                } else {
                    commit_span_chunk::<BLEND>(ctx, fs, buf, ramp_offsets_packed(color_f, delta_color_f, g));
                }
                buf = buf.wrapping_add(4);
                dot_pos += dot_pos_delta;
                dot_pos_delta += delta_delta2;
            }
            t += inside;
            if t >= span {
                break;
            }
            // Probably a transition: fall through to per-sample lookups.
            offset = fast_sqrt::<true>(dot_pos) - radius;
            if repeat {
                offset = fract(offset);
            }
        }
        let entry = clamp(offset * size + 1.0f32, Float::splat(0.0), Float::splat(1.0 + size));
        commit_span_chunk::<BLEND>(ctx, fs, buf, sample_gradient(s, address, entry));
        buf = buf.wrapping_add(4);
        t += 4;
        dot_pos += dot_pos_delta;
        dot_pos_delta += delta_delta2;
    }
    true
}

/// `commitRadialGradientFromStops<BLEND, DITHER>`.
///
/// # Safety
/// The span contract of the ext module doc for `span` pixels from `buf`.
unsafe fn commit_radial_gradient_from_stops<const BLEND: bool, const DITHER: bool, S: Sampler>(
    ctx: &RasterState,
    fs: &FragmentShaderState,
    s: &S,
    offsets_address: i32,
    colors_address: i32,
    stop_count: f32,
    repeat: bool,
    pos: vec2,
    start_radius: f32,
    mut buf: *mut u32,
    span: i32,
    frag_coord: &vec4,
) -> bool {
    let c = s.common();
    let stop_color = |i: i32| c.at32::<Float>(colors_address.wrapping_add(i.wrapping_mul(4)));
    let pos0 = v2s(pos.x.0[0], pos.y.0[0]);
    let mut delta = v2s(pos.x.0[1] - pos.x.0[0], pos.y.0[1] - pos.y.0[0]);
    let mut delta_delta = dot(delta, delta);
    if !delta_delta.is_finite() || !start_radius.is_finite() {
        return false;
    }
    let mut cur_x = toi(frag_coord.x.0[0]);
    let cur_y = toi(frag_coord.y.0[0]);
    let (inv_delta, middle_t, middle_b) = radial_coefficients(pos0, delta, delta_delta, span);
    delta *= 4.0f32;
    delta_delta *= (4 * 4) as f32;
    let mut dot_pos = dot(pos, pos);
    let mut dot_pos_delta = 2.0f32 * dot(pos, vec2::from(delta)) + delta_delta;
    let delta_delta2 = 2.0 * delta_delta;
    let mut t: i32 = 0;
    while t < span {
        let mut offset = fast_sqrt::<true>(dot_pos) - start_radius;
        let mut adjusted_start_radius = start_radius;
        if repeat {
            adjusted_start_radius += offset.0[0];
            offset = fract(offset);
            adjusted_start_radius -= offset.0[0];
        }
        let tf = t as f32;
        let ox = offset.0[0];
        let mut intercept: f32 = -1.0;
        let mut stop_index: i32 = 0;
        let mut prev_offset = 0.0f32;
        let mut next_offset = 0.0f32;
        if ox < 0.0 {
            if tf >= middle_t {
                intercept = start_radius;
            }
        } else if ox >= 1.0 {
            stop_index = toi(stop_count - 1.0);
            if tf < middle_t {
                intercept = start_radius + 1.0;
            }
        } else {
            let (i, p, n) = find_gradient_stop_pair(s, ox, offsets_address, toi(stop_count));
            stop_index = i;
            prev_offset = p;
            next_offset = n;
            intercept = adjusted_start_radius + if tf >= middle_t { next_offset } else { prev_offset };
        }
        let mut end_t: f32 = if tf >= middle_t { span as f32 } else { span.min(toi(middle_t)) as f32 };
        if intercept >= 0.0 {
            let b = middle_b + intercept * intercept * inv_delta;
            if b > 0.0 {
                let b = fast_sqrt_scalar(b);
                end_t = min(end_t, if tf >= middle_t { middle_t + b } else { middle_t - b });
            } else {
                end_t = min(end_t, middle_t);
            }
        }
        // At least a pixel each time.
        end_t = max(ceil(end_t), tf + 1.0);

        let inside = rules::chunks((toi(end_t - tf) as i64).clamp(0, rules::MAX_SPAN)).0 as i32;
        let scale = if DITHER { 0xFF00 as f32 } else { 255.0 };
        let min_color_f = zyxw(stop_color(stop_index)) * scale;
        let max_color_f = zyxw(stop_color(stop_index.wrapping_add(1))) * scale;
        let delta_offset = next_offset - prev_offset;
        let delta_color_f = if delta_offset == 0.0 { Float::splat(0.0) } else { (max_color_f - min_color_f) / delta_offset };
        let color_f = min_color_f - delta_color_f * (adjusted_start_radius + prev_offset);
        for _ in 0..inside / 4 {
            let g = fast_sqrt::<false>(dot_pos);
            let r = if DITHER {
                let r = dither(ramp_offsets_wide(color_f, delta_color_f, g), cur_x, cur_y) >> 8;
                cur_x = cur_x.wrapping_add(4);
                r
            } else {
                ramp_offsets_packed(color_f, delta_color_f, g)
            };
            // SAFETY: `inside <= end_t - t <= span - t` pixels.
            unsafe { <*mut u32 as ExtPx>::commit::<BLEND>(ctx, fs, buf, r) };
            buf = buf.wrapping_add(4);
            dot_pos += dot_pos_delta;
            dot_pos_delta += delta_delta2;
        }
        t += inside;
        if t >= span {
            break;
        }
        // A partial chunk.
        let remainder = toi(end_t - t as f32);
        if remainder > 0 {
            let g = fast_sqrt::<false>(dot_pos);
            let r = if DITHER {
                let r = dither(ramp_offsets_wide(color_f, delta_color_f, g), cur_x, cur_y) >> 8;
                cur_x = cur_x.wrapping_add(4);
                r
            } else {
                ramp_offsets_packed(color_f, delta_color_f, g)
            };
            // SAFETY: `remainder <= end_t - t <= span - t` pixels.
            unsafe { <*mut u32 as ExtPx>::commit_len::<BLEND>(ctx, fs, buf, r, remainder) };
            buf = buf.wrapping_add(remainder as usize);
            t += remainder;
            // dotPosDelta grows linearly: a quarter step per pixel.
            dot_pos_delta += delta_delta2 * 0.25 * remainder as f32;
            // dotPos compounds: step it a pixel at a time.
            let single = delta_delta2 * 0.0625;
            let first = dot_pos.0[1] - dot_pos.0[0];
            let mut partial = Float::new([0.0, 1.0, 2.0, 3.0]) * single + first;
            for _ in 0..remainder {
                dot_pos += partial;
                partial += single;
            }
        }
    }
    true
}

/// The `drawn` epilogue of the gradient macros: the whole span is done.
#[inline(always)]
fn finish(fs: &mut FragmentShaderState, drawn: bool) {
    if drawn {
        let n = fs.swgl_SpanLength;
        super::advance::<*mut u32>(fs, n);
    }
}

macro_rules! dispatch_blend {
    ($ctx:expr, $f:ident, $dither:ident, $($a:expr),*) => {
        if $ctx.blend_key != BLEND_KEY_NONE { $f::<true, $dither, S>($ctx, $($a),*) } else { $f::<false, $dither, S>($ctx, $($a),*) }
    };
}

/// `swgl_commitLinearGradientRGBA8(sampler, address, size, tileRepeat, gradientRepeat, pos, scaleDir, startOffset)`.
pub fn swgl_commitLinearGradientRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    address: i32,
    size: f32,
    tile_repeat: bool,
    gradient_repeat: bool,
    pos: vec2,
    scale_dir: vec2_scalar,
    start_offset: f32,
    fs: &mut FragmentShaderState,
) {
    linear::<S, false>(ctx, s, address, size, tile_repeat, gradient_repeat, pos, scale_dir, start_offset, fs);
}

/// `swgl_commitDitheredLinearGradientRGBA8(...)`: dithered by `gl_FragCoord`.
pub fn swgl_commitDitheredLinearGradientRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    address: i32,
    size: f32,
    tile_repeat: bool,
    gradient_repeat: bool,
    pos: vec2,
    scale_dir: vec2_scalar,
    start_offset: f32,
    fs: &mut FragmentShaderState,
) {
    linear::<S, true>(ctx, s, address, size, tile_repeat, gradient_repeat, pos, scale_dir, start_offset, fs);
}

fn linear<S: Sampler, const DITHER: bool>(
    ctx: &RasterState,
    s: S,
    address: i32,
    size: f32,
    tile_repeat: bool,
    gradient_repeat: bool,
    pos: vec2,
    scale_dir: vec2_scalar,
    start_offset: f32,
    fs: &mut FragmentShaderState,
) {
    let buf = fs.swgl_OutRGBA8.0;
    let (span, fc) = (fs.swgl_SpanLength, fs.gl_FragCoord);
    let f: &FragmentShaderState = fs;
    // SAFETY: the span contract of the ext module doc.
    let drawn = unsafe {
        dispatch_blend!(ctx, commit_linear_gradient, DITHER, f, &s, address, size, tile_repeat, gradient_repeat, pos,
            &scale_dir, start_offset, buf, span, &fc)
    };
    finish(fs, drawn);
}

/// `swgl_commitLinearGradientFromStopsRGBA8(sampler, offsetsAddress, colorsAddress, size, gradientRepeat, pos,
/// scaleDir, startOffset)`.
pub fn swgl_commitLinearGradientFromStopsRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    offsets_address: i32,
    colors_address: i32,
    size: f32,
    gradient_repeat: bool,
    pos: vec2,
    scale_dir: vec2_scalar,
    start_offset: f32,
    fs: &mut FragmentShaderState,
) {
    linear_from_stops::<S, false>(ctx, s, offsets_address, colors_address, size, gradient_repeat, pos, scale_dir, start_offset, fs);
}

/// `swgl_commitDitheredLinearGradientFromStopsRGBA8(sampler, offsetsAddress, colorsAddress, size, tileRepeat,
/// gradientRepeat, pos, scaleDir, startOffset)`: `tileRepeat` is unused, as in the C++; the dither position is
/// `gl_FragCoord` (the C++ macro reads a shader variable `fragCoord`).
pub fn swgl_commitDitheredLinearGradientFromStopsRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    offsets_address: i32,
    colors_address: i32,
    size: f32,
    _tile_repeat: bool,
    gradient_repeat: bool,
    pos: vec2,
    scale_dir: vec2_scalar,
    start_offset: f32,
    fs: &mut FragmentShaderState,
) {
    linear_from_stops::<S, true>(ctx, s, offsets_address, colors_address, size, gradient_repeat, pos, scale_dir, start_offset, fs);
}

fn linear_from_stops<S: Sampler, const DITHER: bool>(
    ctx: &RasterState,
    s: S,
    offsets_address: i32,
    colors_address: i32,
    size: f32,
    gradient_repeat: bool,
    pos: vec2,
    scale_dir: vec2_scalar,
    start_offset: f32,
    fs: &mut FragmentShaderState,
) {
    let buf = fs.swgl_OutRGBA8.0;
    let (span, fc) = (fs.swgl_SpanLength, fs.gl_FragCoord);
    let f: &FragmentShaderState = fs;
    // SAFETY: the span contract of the ext module doc.
    let drawn = unsafe {
        dispatch_blend!(ctx, commit_linear_gradient_from_stops, DITHER, f, &s, offsets_address, colors_address, size,
            gradient_repeat, pos, &scale_dir, start_offset, buf, span, &fc)
    };
    finish(fs, drawn);
}

/// `swgl_commitRadialGradientRGBA8(sampler, address, size, repeat, pos, radius)`.
pub fn swgl_commitRadialGradientRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    address: i32,
    size: f32,
    repeat: bool,
    pos: vec2,
    radius: f32,
    fs: &mut FragmentShaderState,
) {
    radial::<S, false>(ctx, s, address, size, repeat, pos, radius, fs);
}

/// `swgl_commitDitheredRadialGradientRGBA8(sampler, address, size, repeat, pos, radius)`.
pub fn swgl_commitDitheredRadialGradientRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    address: i32,
    size: f32,
    repeat: bool,
    pos: vec2,
    radius: f32,
    fs: &mut FragmentShaderState,
) {
    radial::<S, true>(ctx, s, address, size, repeat, pos, radius, fs);
}

fn radial<S: Sampler, const DITHER: bool>(
    ctx: &RasterState,
    s: S,
    address: i32,
    size: f32,
    repeat: bool,
    pos: vec2,
    radius: f32,
    fs: &mut FragmentShaderState,
) {
    let buf = fs.swgl_OutRGBA8.0;
    let (span, fc) = (fs.swgl_SpanLength, fs.gl_FragCoord);
    let f: &FragmentShaderState = fs;
    // SAFETY: the span contract of the ext module doc.
    let drawn = unsafe {
        dispatch_blend!(ctx, commit_radial_gradient, DITHER, f, &s, address, size, repeat, pos, radius, buf, span, &fc)
    };
    finish(fs, drawn);
}

/// `swgl_commitRadialGradientFromStopsRGBA8(sampler, offsetsAddress, colorsAddress, size, repeat, pos,
/// startRadius)`.
pub fn swgl_commitRadialGradientFromStopsRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    offsets_address: i32,
    colors_address: i32,
    size: f32,
    repeat: bool,
    pos: vec2,
    start_radius: f32,
    fs: &mut FragmentShaderState,
) {
    radial_from_stops::<S, false>(ctx, s, offsets_address, colors_address, size, repeat, pos, start_radius, fs);
}

/// `swgl_commitDitheredRadialGradientFromStopsRGBA8(...)`.
pub fn swgl_commitDitheredRadialGradientFromStopsRGBA8<S: Sampler>(
    ctx: &mut RasterState,
    s: S,
    offsets_address: i32,
    colors_address: i32,
    size: f32,
    repeat: bool,
    pos: vec2,
    start_radius: f32,
    fs: &mut FragmentShaderState,
) {
    radial_from_stops::<S, true>(ctx, s, offsets_address, colors_address, size, repeat, pos, start_radius, fs);
}

fn radial_from_stops<S: Sampler, const DITHER: bool>(
    ctx: &RasterState,
    s: S,
    offsets_address: i32,
    colors_address: i32,
    size: f32,
    repeat: bool,
    pos: vec2,
    start_radius: f32,
    fs: &mut FragmentShaderState,
) {
    let buf = fs.swgl_OutRGBA8.0;
    let (span, fc) = (fs.swgl_SpanLength, fs.gl_FragCoord);
    let f: &FragmentShaderState = fs;
    // SAFETY: the span contract of the ext module doc.
    let drawn = unsafe {
        dispatch_blend!(ctx, commit_radial_gradient_from_stops, DITHER, f, &s, offsets_address, colors_address, size,
            repeat, pos, start_radius, buf, span, &fc)
    };
    finish(fs, drawn);
}

