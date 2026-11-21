/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `RasterState`: swgl's raster statics (blend.h, swgl_ext.h, gl.cc) as one plain struct, one per rasterizing
//! thread, passed as `&mut` (raster/DESIGN.md, "No globals"). Fields keep the C++ names.

use crate::blend::{BlendKey, BLEND_KEY_NONE};
use crate::glsl::{sampler2D, Float, V8};
use crate::texture::{IntPoint, IntRect, TextureUnitView, WideRGBA8, MAX_TEXTURE_UNITS};

/// `SWGLClipFlag` (blend.h).
pub const SWGL_CLIP_FLAG_MASK: i32 = 1 << 0;
pub const SWGL_CLIP_FLAG_AA: i32 = 1 << 1;
pub const SWGL_CLIP_FLAG_BLEND_OVERRIDE: i32 = 1 << 2;

#[derive(Clone, Debug)]
pub struct RasterState {
    /// gl.cc's `blend_key`: the blend stage of the current draw, with the MASK/AA groups added.
    pub blend_key: BlendKey,
    /// `ctx->blendcolor`, copied in per draw: `glBlendColor` as 8-bit lanes, twice.
    pub blendcolor: V8<u16>,
    pub swgl_ClipFlags: i32,
    pub swgl_BlendOverride: BlendKey,
    pub swgl_BlendColorRGBA8: WideRGBA8,
    pub swgl_BlendAlphaRGBA8: WideRGBA8,
    /// The start of the span in the color buffer.
    pub swgl_SpanBuf: *mut u8,
    /// The start of the span in the clip mask; the mask byte of pixel `buf` is at `buf - swgl_SpanBuf`.
    pub swgl_ClipMaskBuf: *mut u8,
    pub swgl_ClipMask: sampler2D,
    pub swgl_ClipMaskOffset: IntPoint,
    pub swgl_ClipMaskBounds: IntRect,
    pub swgl_AAEdgeMask: i32,
    /// The start of the opaque part of the span for AA, and its size in bytes.
    pub swgl_OpaqueStart: *const u8,
    pub swgl_OpaqueSize: u32,
    /// AA coverage distances of the left and right edges, and their slopes per step.
    pub swgl_LeftAADist: Float,
    pub swgl_RightAADist: Float,
    pub swgl_AASlope: Float,
    /// What the textures bound to each unit give a sampler, for `lookup_sampler` during the current draw.
    pub texture_units: [TextureUnitView; MAX_TEXTURE_UNITS],
}

impl Default for RasterState {
    fn default() -> Self {
        RasterState {
            blend_key: BLEND_KEY_NONE,
            blendcolor: V8::splat(0),
            swgl_ClipFlags: 0,
            swgl_BlendOverride: BLEND_KEY_NONE,
            swgl_BlendColorRGBA8: WideRGBA8::splat(0),
            swgl_BlendAlphaRGBA8: WideRGBA8::splat(0),
            swgl_SpanBuf: core::ptr::null_mut(),
            swgl_ClipMaskBuf: core::ptr::null_mut(),
            swgl_ClipMask: sampler2D::default(),
            swgl_ClipMaskOffset: IntPoint::default(),
            swgl_ClipMaskBounds: IntRect::default(),
            swgl_AAEdgeMask: 0,
            swgl_OpaqueStart: core::ptr::null(),
            swgl_OpaqueSize: 0,
            swgl_LeftAADist: Float::splat(0.0),
            swgl_RightAADist: Float::splat(0.0),
            swgl_AASlope: Float::splat(0.0),
            texture_units: [TextureUnitView::default(); MAX_TEXTURE_UNITS],
        }
    }
}

// SAFETY: the pointers address the color buffer, clip mask and textures of the draw in progress, which the GL
// context keeps alive and locked while any thread rasterizes it; each rasterizing thread owns its RasterState.
unsafe impl Send for RasterState {}
