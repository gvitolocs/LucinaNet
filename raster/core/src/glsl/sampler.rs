/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! glsl.h's sampler structs, as plain data. The sampling functions (`texelFetch`, `texture`, ...) are the
//! `texture` module's (texture.h); the derived C++ structs (`sampler2DR8_impl : sampler2D_impl`) are newtypes
//! that dereference to their base.

use super::{TextureFilter, TextureFormat};

/// The texel storage a sampler reads: a pointer into a texture the GL context owns.
#[derive(Clone, Copy, Debug)]
pub struct TexelPtr(pub *mut u32);

impl Default for TexelPtr {
    fn default() -> Self {
        Self(core::ptr::null_mut())
    }
}

// SAFETY: a sampler only points at texture storage the GL context keeps alive and locked for the duration of a
// draw; rasterizing threads read it and nobody writes it during the draw (swgl's texture locks), so sharing the
// pointer between threads is no different from sharing `&[u32]`.
unsafe impl Send for TexelPtr {}
// SAFETY: as above, reads only during a draw.
unsafe impl Sync for TexelPtr {}

/// `samplerCommon`. `buf` and `len` are what sampling trusts (`texture::load_texels` reads the `len` bytes at `buf`
/// unchecked), so they are set only from a texture (`texture::init_sampler_from`), a static zero buffer, or
/// [`samplerCommon::from_raw`]'s caller. The dimensions only index: every read is checked against `len`.
#[derive(Clone, Copy, Debug, Default)]
pub struct samplerCommon {
    pub(crate) buf: TexelPtr,
    /// Readable bytes at `buf`: sampling reads nothing outside them (`texture::load_texels`).
    pub(crate) len: usize,
    /// In units of BPP if < 4, or dwords if BPP >= 4.
    pub stride: u32,
    pub height: u32,
    pub width: u32,
    pub format: TextureFormat,
}

impl samplerCommon {
    /// A sampler over the `len` bytes at `buf`.
    ///
    /// # Safety
    /// `buf` must be valid for reads of `len` bytes (or `len` must be 0) for as long as this sampler or any copy of
    /// it samples, and nothing may write those bytes while it does.
    pub unsafe fn from_raw(
        buf: *const u32,
        len: usize,
        stride: u32,
        height: u32,
        width: u32,
        format: TextureFormat,
    ) -> Self {
        samplerCommon { buf: TexelPtr(buf as *mut u32), len, stride, height, width, format }
    }
    /// The texels (null for a default sampler).
    pub fn buf(&self) -> *const u32 {
        self.buf.0
    }
    /// Readable bytes at [`samplerCommon::buf`].
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// `samplerFilter`.
#[derive(Clone, Copy, Debug, Default)]
pub struct samplerFilter {
    pub filter: TextureFilter,
}

/// `sampler2D_impl : samplerCommon, samplerFilter`.
#[derive(Clone, Copy, Debug, Default)]
pub struct sampler2D_impl {
    pub common: samplerCommon,
    pub filter: samplerFilter,
}

/// `isampler2D_impl : samplerCommon`.
#[derive(Clone, Copy, Debug, Default)]
pub struct isampler2D_impl {
    pub common: samplerCommon,
}

/// `sampler2DRect_impl : samplerCommon, samplerFilter`.
#[derive(Clone, Copy, Debug, Default)]
pub struct sampler2DRect_impl {
    pub common: samplerCommon,
    pub filter: samplerFilter,
}

macro_rules! derived_sampler {
    ($($name:ident: $base:ident),*) => {$(
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $name(pub $base);
        impl core::ops::Deref for $name {
            type Target = $base;
            fn deref(&self) -> &$base {
                &self.0
            }
        }
        impl core::ops::DerefMut for $name {
            fn deref_mut(&mut self) -> &mut $base {
                &mut self.0
            }
        }
    )*};
}
derived_sampler!(
    sampler2DR8_impl: sampler2D_impl,
    sampler2DRG8_impl: sampler2D_impl,
    sampler2DRGBA8_impl: sampler2D_impl,
    sampler2DRGBA32F_impl: sampler2D_impl,
    isampler2DRGBA32I_impl: isampler2D_impl
);

macro_rules! common_deref {
    ($($name:ident),*) => {$(
        impl core::ops::Deref for $name {
            type Target = samplerCommon;
            fn deref(&self) -> &samplerCommon {
                &self.common
            }
        }
        impl core::ops::DerefMut for $name {
            fn deref_mut(&mut self) -> &mut samplerCommon {
                &mut self.common
            }
        }
    )*};
}
common_deref!(sampler2D_impl, isampler2D_impl, sampler2DRect_impl);

// Generated shaders hold samplers by value: the C++ `sampler2D` is a pointer to the `_impl` struct the program
// owns, the Rust one a copy of it that `lookup_sampler` returns.
pub type sampler2D = sampler2D_impl;
pub type isampler2D = isampler2D_impl;
pub type sampler2DRect = sampler2DRect_impl;
pub type sampler2DR8 = sampler2DR8_impl;
pub type sampler2DRG8 = sampler2DRG8_impl;
pub type sampler2DRGBA8 = sampler2DRGBA8_impl;
pub type sampler2DRGBA32F = sampler2DRGBA32F_impl;
pub type isampler2DRGBA32I = isampler2DRGBA32I_impl;
