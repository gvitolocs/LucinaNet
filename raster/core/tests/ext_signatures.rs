/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Every `crate::ext` entry of the runtime inventory in raster/glsl-to-rust/README.md, called with the argument
//! types the emitted shaders pass: this file compiling is the check. Regenerate it from the README's `fn swgl_` lines
//! when the inventory changes.
#![allow(dead_code, clippy::too_many_arguments, clippy::unused_unit, non_snake_case)]

use lucinanet_raster::ext::*;
use lucinanet_raster::glsl::*;
use lucinanet_raster::program::FragmentShaderState;
use lucinanet_raster::state::RasterState;

fn sig_0_swgl_antiAlias(ctx: &mut RasterState, a1: i32) -> () {
    swgl_antiAlias(ctx, a1)
}

fn sig_1_swgl_blendDropShadow(ctx: &mut RasterState, a1: vec4_scalar) -> () {
    swgl_blendDropShadow(ctx, a1)
}

fn sig_2_swgl_blendSubpixelText(ctx: &mut RasterState, a1: vec4_scalar) -> () {
    swgl_blendSubpixelText(ctx, a1)
}

fn sig_3_swgl_clipMask(ctx: &mut RasterState, a1: sampler2D, a2: vec2_scalar, a3: vec2_scalar, a4: vec2_scalar) -> () {
    swgl_clipMask(ctx, a1, a2, a3, a4)
}

fn sig_4_swgl_commitColorR8(ctx: &mut RasterState, a1: Float, fs: &mut FragmentShaderState) -> () {
    swgl_commitColorR8(ctx, a1, fs)
}

fn sig_5_swgl_commitGaussianBlurR8(ctx: &mut RasterState, a1: sampler2D, a2: vec2, a3: vec4_scalar, a4: bool, a5: i32, a6: vec2_scalar, fs: &mut FragmentShaderState) -> () {
    swgl_commitGaussianBlurR8(ctx, a1, a2, a3, a4, a5, a6, fs)
}

fn sig_6_swgl_commitGaussianBlurRGBA8(ctx: &mut RasterState, a1: sampler2D, a2: vec2, a3: vec4_scalar, a4: bool, a5: i32, a6: vec2_scalar, fs: &mut FragmentShaderState) -> () {
    swgl_commitGaussianBlurRGBA8(ctx, a1, a2, a3, a4, a5, a6, fs)
}

fn sig_7_swgl_commitLinearGradientFromStopsRGBA8(ctx: &mut RasterState, a1: sampler2D, a2: i32, a3: i32, a4: f32, a5: bool, a6: vec2, a7: vec2_scalar, a8: f32, fs: &mut FragmentShaderState) -> () {
    swgl_commitLinearGradientFromStopsRGBA8(ctx, a1, a2, a3, a4, a5, a6, a7, a8, fs)
}

fn sig_8_swgl_commitPartialSolidR8(ctx: &mut RasterState, a1: i32, a2: f32, fs: &mut FragmentShaderState) -> () {
    swgl_commitPartialSolidR8(ctx, a1, a2, fs)
}

fn sig_9_swgl_commitRadialGradientFromStopsRGBA8(ctx: &mut RasterState, a1: sampler2D, a2: i32, a3: i32, a4: f32, a5: bool, a6: vec2, a7: f32, fs: &mut FragmentShaderState) -> () {
    swgl_commitRadialGradientFromStopsRGBA8(ctx, a1, a2, a3, a4, a5, a6, a7, fs)
}

fn sig_10_swgl_commitSolidR8(ctx: &mut RasterState, a1: f32, fs: &mut FragmentShaderState) -> () {
    swgl_commitSolidR8(ctx, a1, fs)
}

fn sig_11_swgl_commitSolidRGBA8(ctx: &mut RasterState, a1: vec4_scalar, fs: &mut FragmentShaderState) -> () {
    swgl_commitSolidRGBA8(ctx, a1, fs)
}

fn sig_12_swgl_commitTextureColorRGBA8(ctx: &mut RasterState, a1: sampler2D, a2: vec2, a3: vec4_scalar, a4: vec4_scalar, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureColorRGBA8(ctx, a1, a2, a3, a4, fs)
}

fn sig_13_swgl_commitTextureColorRGBA8(ctx: &mut RasterState, a1: sampler2DRect, a2: vec2, a3: vec4_scalar, a4: vec4_scalar, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureColorRGBA8(ctx, a1, a2, a3, a4, fs)
}

fn sig_14_swgl_commitTextureLinearColorR8ToRGBA8(ctx: &mut RasterState, a1: sampler2D, a2: vec2, a3: vec4_scalar, a4: vec4_scalar, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureLinearColorR8ToRGBA8(ctx, a1, a2, a3, a4, fs)
}

fn sig_15_swgl_commitTextureLinearColorRGBA8(ctx: &mut RasterState, a1: sampler2D, a2: vec2, a3: vec4_scalar, a4: f32, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureLinearColorRGBA8(ctx, a1, a2, a3, a4, fs)
}

fn sig_16_swgl_commitTextureLinearColorRGBA8(ctx: &mut RasterState, a1: sampler2D, a2: vec2, a3: vec4_scalar, a4: vec4_scalar, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureLinearColorRGBA8(ctx, a1, a2, a3, a4, fs)
}

fn sig_17_swgl_commitTextureLinearRGBA8(ctx: &mut RasterState, a1: sampler2D, a2: vec2, a3: vec4_scalar, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureLinearRGBA8(ctx, a1, a2, a3, fs)
}

fn sig_18_swgl_commitTextureLinearRGBA8(ctx: &mut RasterState, a1: sampler2DRect, a2: vec2, a3: vec4_scalar, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureLinearRGBA8(ctx, a1, a2, a3, fs)
}

fn sig_19_swgl_commitTextureLinearYUV(ctx: &mut RasterState, a1: sampler2D, a2: vec2, a3: vec4_scalar, a4: vec3_scalar, a5: mat3_scalar, a6: i32, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureLinearYUV(ctx, a1, a2, a3, a4, a5, a6, fs)
}

fn sig_20_swgl_commitTextureLinearYUV(ctx: &mut RasterState, a1: sampler2DRect, a2: vec2, a3: vec4_scalar, a4: vec3_scalar, a5: mat3_scalar, a6: i32, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureLinearYUV(ctx, a1, a2, a3, a4, a5, a6, fs)
}

fn sig_21_swgl_commitTextureLinearYUV2(ctx: &mut RasterState, a1: sampler2D, a2: vec2, a3: vec4_scalar, a4: sampler2D, a5: vec2, a6: vec4_scalar, a7: vec3_scalar, a8: mat3_scalar, a9: i32, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureLinearYUV2(ctx, a1, a2, a3, a4, a5, a6, a7, a8, a9, fs)
}

fn sig_22_swgl_commitTextureLinearYUV2(ctx: &mut RasterState, a1: sampler2DRect, a2: vec2, a3: vec4_scalar, a4: sampler2DRect, a5: vec2, a6: vec4_scalar, a7: vec3_scalar, a8: mat3_scalar, a9: i32, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureLinearYUV2(ctx, a1, a2, a3, a4, a5, a6, a7, a8, a9, fs)
}

fn sig_23_swgl_commitTextureLinearYUV3(ctx: &mut RasterState, a1: sampler2D, a2: vec2, a3: vec4_scalar, a4: sampler2D, a5: vec2, a6: vec4_scalar, a7: sampler2D, a8: vec2, a9: vec4_scalar, a10: vec3_scalar, a11: mat3_scalar, a12: i32, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureLinearYUV3(ctx, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, fs)
}

fn sig_24_swgl_commitTextureLinearYUV3(ctx: &mut RasterState, a1: sampler2DRect, a2: vec2, a3: vec4_scalar, a4: sampler2DRect, a5: vec2, a6: vec4_scalar, a7: sampler2DRect, a8: vec2, a9: vec4_scalar, a10: vec3_scalar, a11: mat3_scalar, a12: i32, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureLinearYUV3(ctx, a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, fs)
}

fn sig_25_swgl_commitTextureRGBA8(ctx: &mut RasterState, a1: sampler2D, a2: vec2, a3: vec4_scalar, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureRGBA8(ctx, a1, a2, a3, fs)
}

fn sig_26_swgl_commitTextureRGBA8(ctx: &mut RasterState, a1: sampler2DRect, a2: vec2, a3: vec4_scalar, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureRGBA8(ctx, a1, a2, a3, fs)
}

fn sig_27_swgl_commitTextureRepeatColorRGBA8(ctx: &mut RasterState, a1: sampler2D, a2: vec2, a3: vec2_scalar, a4: vec4_scalar, a5: vec4_scalar, a6: vec4_scalar, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureRepeatColorRGBA8(ctx, a1, a2, a3, a4, a5, a6, fs)
}

fn sig_28_swgl_commitTextureRepeatColorRGBA8(ctx: &mut RasterState, a1: sampler2DRect, a2: vec2, a3: vec2_scalar, a4: vec4_scalar, a5: vec4_scalar, a6: vec4_scalar, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureRepeatColorRGBA8(ctx, a1, a2, a3, a4, a5, a6, fs)
}

fn sig_29_swgl_commitTextureRepeatRGBA8(ctx: &mut RasterState, a1: sampler2D, a2: vec2, a3: vec2_scalar, a4: vec4_scalar, a5: vec4_scalar, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureRepeatRGBA8(ctx, a1, a2, a3, a4, a5, fs)
}

fn sig_30_swgl_commitTextureRepeatRGBA8(ctx: &mut RasterState, a1: sampler2DRect, a2: vec2, a3: vec2_scalar, a4: vec4_scalar, a5: vec4_scalar, fs: &mut FragmentShaderState) -> () {
    swgl_commitTextureRepeatRGBA8(ctx, a1, a2, a3, a4, a5, fs)
}

fn sig_31_swgl_forceScalar(a0: Float) -> f32 {
    swgl_forceScalar(a0)
}

fn sig_32_swgl_forceScalar(a0: vec2) -> vec2_scalar {
    swgl_forceScalar(a0)
}

fn sig_33_swgl_isTextureR8(a0: sampler2D) -> bool {
    swgl_isTextureR8(a0)
}

fn sig_34_swgl_isTextureRGBA8(a0: sampler2D) -> bool {
    swgl_isTextureRGBA8(a0)
}

fn sig_35_swgl_isTextureRGBA8(a0: sampler2DRect) -> bool {
    swgl_isTextureRGBA8(a0)
}

fn sig_36_swgl_validateGradientFromStops(a0: sampler2D, a1: ivec2_scalar, a2: i32) -> i32 {
    swgl_validateGradientFromStops(a0, a1, a2)
}

#[test]
fn inventory_compiles() {}
