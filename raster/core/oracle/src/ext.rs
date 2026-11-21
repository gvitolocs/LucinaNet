/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The `extern "C"` wrappers of `../tests/oracle/shim_x.cc` around swgl's span fast paths (swgl_ext.h), and the
//! structs they share with it. A span wrapper returns the span length left, or -1000 if the span pointer did not
//! advance by the pixels drawn. Keep in step with the shim.

use core::ffi::c_void;

/// One sampler: `TextureFormat` and `TextureFilter` as their numeric values.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XTex {
    pub buf: *const c_void,
    pub stride: u32,
    pub height: u32,
    pub width: u32,
    pub format: u32,
    pub filter: u32,
}

/// The raster state a span sees (swgl's statics as the rasterizer leaves them).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct XState {
    pub blend_key: u32,
    pub clip_flags: i32,
    pub blend_color: [u16; 16],
    pub blend_alpha: [u16; 16],
    pub blendcolor: [u16; 8],
    pub clip_mask_buf: *const u8,
    /// `swgl_OpaqueStart` in pixels from the span start.
    pub opaque_start: i32,
    pub opaque_size: u32,
    pub left_aa_dist: [f32; 4],
    pub right_aa_dist: [f32; 4],
    pub aa_slope: [f32; 4],
    pub frag_coord: [f32; 16],
    pub secondary: [f32; 16],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XClip {
    pub clip_flags: i32,
    pub offset: [i32; 2],
    pub bounds: [i32; 4],
    pub mask_buf: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XOverride {
    pub clip_flags: i32,
    pub blend_override: u32,
    pub color: [u16; 16],
    pub alpha: [u16; 16],
}

unsafe extern "C" {
    pub fn x_commitSolidRGBA8(st: *const XState, buf: *mut u32, len: i32, v: *const f32) -> i32;
    pub fn x_commitSolidR8(st: *const XState, buf: *mut u8, len: i32, v: f32) -> i32;
    pub fn x_commitPartialSolidRGBA8(st: *const XState, buf: *mut u32, len: i32, n: i32, v: *const f32) -> i32;
    pub fn x_commitPartialSolidR8(st: *const XState, buf: *mut u8, len: i32, n: i32, v: f32) -> i32;
    pub fn x_commitColorRGBA8(st: *const XState, buf: *mut u32, len: i32, c: *const f32) -> i32;
    pub fn x_commitColorR8(st: *const XState, buf: *mut u8, len: i32, c: *const f32) -> i32;
    pub fn x_commitTextureLinearRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, rect: i32, p: *const f32, r: *const f32) -> i32;
    pub fn x_commitTextureLinearR8(st: *const XState, buf: *mut u8, len: i32, s: *const XTex, rect: i32, p: *const f32, r: *const f32) -> i32;
    pub fn x_commitPartialTextureLinearR8(st: *const XState, buf: *mut u8, len: i32, n: i32, s: *const XTex, rect: i32, p: *const f32, r: *const f32) -> i32;
    pub fn x_commitPartialTextureLinearInvertR8(st: *const XState, buf: *mut u8, len: i32, n: i32, s: *const XTex, rect: i32, p: *const f32, r: *const f32) -> i32;
    pub fn x_commitTextureLinearColorRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, rect: i32, p: *const f32, r: *const f32, c: *const f32) -> i32;
    pub fn x_commitTextureLinearColorRGBA8f(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, rect: i32, p: *const f32, r: *const f32, c: f32) -> i32;
    pub fn x_commitTextureLinearR8ToRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, rect: i32, p: *const f32, r: *const f32) -> i32;
    pub fn x_commitTextureLinearColorR8ToRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, rect: i32, p: *const f32, r: *const f32, c: *const f32) -> i32;
    pub fn x_commitTextureLinearRepeatRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, rect: i32, p: *const f32, t: *const f32, rep: *const f32, r: *const f32) -> i32;
    pub fn x_commitTextureLinearRepeatColorRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, rect: i32, p: *const f32, t: *const f32, rep: *const f32, r: *const f32, c: *const f32) -> i32;
    pub fn x_commitTextureNearestRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, rect: i32, p: *const f32, r: *const f32) -> i32;
    pub fn x_commitTextureNearestColorRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, rect: i32, p: *const f32, r: *const f32, c: *const f32) -> i32;
    pub fn x_commitTextureNearestRepeatRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, rect: i32, p: *const f32, t: *const f32, rep: *const f32, r: *const f32) -> i32;
    pub fn x_commitTextureNearestRepeatColorRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, rect: i32, p: *const f32, t: *const f32, rep: *const f32, r: *const f32, c: *const f32) -> i32;
    pub fn x_commitTextureRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, rect: i32, p: *const f32, r: *const f32) -> i32;
    pub fn x_commitTextureColorRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, rect: i32, p: *const f32, r: *const f32, c: *const f32) -> i32;
    pub fn x_commitTextureRepeatRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, rect: i32, p: *const f32, t: *const f32, rep: *const f32, r: *const f32) -> i32;
    pub fn x_commitTextureRepeatColorRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, rect: i32, p: *const f32, t: *const f32, rep: *const f32, r: *const f32, c: *const f32) -> i32;
    pub fn x_commitGaussianBlurRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, p: *const f32, r: *const f32, hori: i32, radius: i32, coeffs: *const f32) -> i32;
    pub fn x_commitGaussianBlurR8(st: *const XState, buf: *mut u8, len: i32, s: *const XTex, p: *const f32, r: *const f32, hori: i32, radius: i32, coeffs: *const f32) -> i32;
    pub fn x_commitTextureLinearYUV1(st: *const XState, buf: *mut u32, len: i32, s0: *const XTex, rect: i32, p0: *const f32, r0: *const f32, b: *const f32, m: *const f32, k: i32) -> i32;
    pub fn x_commitTextureLinearYUV2(st: *const XState, buf: *mut u32, len: i32, s0: *const XTex, s1: *const XTex, rect: i32, p0: *const f32, r0: *const f32, p1: *const f32, r1: *const f32, b: *const f32, m: *const f32, k: i32) -> i32;
    pub fn x_yuv2_r16_stages(s0: *const XTex, s1: *const XTex, rect: i32, p0: *const f32, r0: *const f32, p1: *const f32, r1: *const f32, b: *const f32, m: *const f32, k: i32, taps: *mut i32, raw: *mut i16, shifted: *mut i16, mat: *mut i16, px: *mut u8);
    pub fn x_commitTextureLinearYUV3(st: *const XState, buf: *mut u32, len: i32, s0: *const XTex, s1: *const XTex, s2: *const XTex, rect: i32, p0: *const f32, r0: *const f32, p1: *const f32, r1: *const f32, p2: *const f32, r2: *const f32, b: *const f32, m: *const f32, k: i32) -> i32;
    pub fn x_commitTextureLinearColorYUV1(st: *const XState, buf: *mut u32, len: i32, s0: *const XTex, rect: i32, p0: *const f32, r0: *const f32, b: *const f32, m: *const f32, k: i32, c: *const f32) -> i32;
    pub fn x_commitTextureLinearColorYUV2(st: *const XState, buf: *mut u32, len: i32, s0: *const XTex, s1: *const XTex, rect: i32, p0: *const f32, r0: *const f32, p1: *const f32, r1: *const f32, b: *const f32, m: *const f32, k: i32, c: *const f32) -> i32;
    pub fn x_commitTextureLinearColorYUV3(st: *const XState, buf: *mut u32, len: i32, s0: *const XTex, s1: *const XTex, s2: *const XTex, rect: i32, p0: *const f32, r0: *const f32, p1: *const f32, r1: *const f32, p2: *const f32, r2: *const f32, b: *const f32, m: *const f32, k: i32, c: *const f32) -> i32;
    pub fn x_commitGradientRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, a: i32, e: *const f32) -> i32;
    pub fn x_validateGradient(s: *const XTex, ax: i32, ay: i32, entries: i32) -> i32;
    pub fn x_validateGradientFromStops(s: *const XTex, ax: i32, ay: i32, entries: i32) -> i32;
    pub fn x_isTexture(s: *const XTex, rect: i32, which: i32) -> i32;
    pub fn x_clipMask(flags_in: i32, mask: *const XTex, off: *const f32, origin: *const f32, size: *const f32, out: *mut XClip);
    pub fn x_antiAlias(flags_in: i32, kind: i32, edges: i32, out_flags: *mut i32, out_mask: *mut i32);
    pub fn x_blendDropShadow(flags_in: i32, c: *const f32, out: *mut XOverride);
    pub fn x_blendSubpixelText(flags_in: i32, c: *const f32, out: *mut XOverride);
    pub fn x_commitLinearGradientRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, a: i32, size: f32, tr: i32, gr: i32, pos: *const f32, dir: *const f32, so: f32) -> i32;
    pub fn x_commitDitheredLinearGradientRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, a: i32, size: f32, tr: i32, gr: i32, pos: *const f32, dir: *const f32, so: f32) -> i32;
    pub fn x_commitLinearGradientFromStopsRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, oa: i32, ca: i32, size: f32, gr: i32, pos: *const f32, dir: *const f32, so: f32) -> i32;
    pub fn x_commitDitheredLinearGradientFromStopsRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, oa: i32, ca: i32, size: f32, gr: i32, pos: *const f32, dir: *const f32, so: f32) -> i32;
    pub fn x_commitRadialGradientRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, a: i32, size: f32, rep: i32, pos: *const f32, radius: f32) -> i32;
    pub fn x_commitDitheredRadialGradientRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, a: i32, size: f32, rep: i32, pos: *const f32, radius: f32) -> i32;
    pub fn x_commitRadialGradientFromStopsRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, oa: i32, ca: i32, size: f32, rep: i32, pos: *const f32, sr: f32) -> i32;
    pub fn x_commitDitheredRadialGradientFromStopsRGBA8(st: *const XState, buf: *mut u32, len: i32, s: *const XTex, oa: i32, ca: i32, size: f32, rep: i32, pos: *const f32, sr: f32) -> i32;
}
