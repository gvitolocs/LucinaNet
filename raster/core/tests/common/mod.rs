/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Shared inputs of `oracle_texture.rs`, `oracle_blend.rs` and `oracle_composite.rs`: a fixed-seed RNG, buffers
//! with zeroed guard bands on both sides, and the same texture or sampler built for the Rust and for the C++
//! (`tests/oracle/shim_t.cc`).
//!
//! The C++ reads up to a pixel before a row of fewer than 2 pixels and a YUY2 chunk past an odd row (with zero
//! weight); the guard bands make those bytes zero, which is what the Rust reads outside a texture. Destination
//! buffers are compared whole, guard bands included, so a write outside the texture shows too.

#![allow(dead_code)]

use lucinanet_raster::glsl::{
    sampler2DRect_impl, sampler2D_impl, samplerCommon, TextureFilter, TextureFormat,
};
use lucinanet_raster::texture::{self as tx, Texture};
use lucinanet_raster_oracle as o;

pub const SEED: u64 = 0x5eed_7e47_b1e4_d0c5;
pub const GUARD: usize = 256;

pub struct Rng(u64);

impl Rng {
    pub fn new(salt: &str) -> Self {
        let mut h = SEED;
        for b in salt.bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
        Rng(h)
    }
    pub fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    /// Uniform in `lo..=hi`.
    pub fn int(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo + 1) as u64) as i32
    }
    pub fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
    pub fn chance(&mut self, n: u64) -> bool {
        self.below(n) == 0
    }
    pub fn byte(&mut self) -> u8 {
        self.next() as u8
    }
    /// A byte biased to 0 and 255, which hit the blend edge cases.
    pub fn byte_edgy(&mut self) -> u8 {
        match self.below(8) {
            0 => 0,
            1 => 255,
            2 => self.int(250, 255) as u8,
            3 => self.int(0, 5) as u8,
            _ => self.byte(),
        }
    }
}

/// `len` bytes with `GUARD` zero bytes before and after, starting 16-byte aligned (as swgl's allocations and the
/// buffers WebRender hands it are; the C++ loads RGBA32F/RGBA32I texels as aligned vectors).
pub struct Guarded {
    v: Vec<u128>,
    pub len: usize,
}

impl Clone for Guarded {
    fn clone(&self) -> Self {
        Guarded { v: self.v.clone(), len: self.len }
    }
}

impl Guarded {
    pub fn new(len: usize) -> Self {
        Guarded { v: vec![0; (len + 2 * GUARD).div_ceil(16)], len }
    }
    /// Everything, guards included.
    pub fn all(&self) -> &[u8] {
        // SAFETY: the u128s are plain bytes; the slice covers exactly the vector's storage.
        unsafe { core::slice::from_raw_parts(self.v.as_ptr() as *const u8, self.v.len() * 16) }
    }
    fn all_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above.
        unsafe { core::slice::from_raw_parts_mut(self.v.as_mut_ptr() as *mut u8, self.v.len() * 16) }
    }
    pub fn ptr(&mut self) -> *mut u8 {
        self.all_mut()[GUARD..].as_mut_ptr()
    }
    pub fn bytes(&mut self) -> &mut [u8] {
        let len = self.len;
        &mut self.all_mut()[GUARD..GUARD + len]
    }
}

/// The internal formats the tests sample, with their `TextureFormat` and bytes per texel.
pub const FORMATS: &[(u32, TextureFormat, usize)] = &[
    (tx::GL_RGBA8, TextureFormat::RGBA8, 4),
    (tx::GL_R8, TextureFormat::R8, 1),
    (tx::GL_RG8, TextureFormat::RG8, 2),
    (tx::GL_R16, TextureFormat::R16, 2),
    (tx::GL_RG16, TextureFormat::RG16, 4),
    (tx::GL_RGBA32F, TextureFormat::RGBA32F, 16),
    (tx::GL_RGBA32I, TextureFormat::RGBA32I, 16),
    (tx::GL_RGB_RAW_422_APPLE, TextureFormat::YUY2, 2),
];

pub fn bpp_of(internal_format: u32) -> usize {
    FORMATS.iter().find(|f| f.0 == internal_format).map(|f| f.2).expect("format")
}

pub fn format_of(internal_format: u32) -> TextureFormat {
    FORMATS.iter().find(|f| f.0 == internal_format).map(|f| f.1).expect("format")
}

/// A texture's contents and layout: `w` x `h` texels of `bpp` bytes, rows `stride` bytes apart, in a guarded
/// buffer of `(h - 1) * stride + w * bpp` bytes (what `Texture::set_buffer` allows).
#[derive(Clone)]
pub struct Tex {
    pub fmt: u32,
    pub w: i32,
    pub h: i32,
    pub stride: usize,
    pub bpp: usize,
    pub buf: Guarded,
    pub off: (i32, i32),
    pub mag_filter: u32,
    /// A delayed clear: the cleared-rows words, rows still to clear, the value.
    pub delay: Option<(Vec<u32>, i32, u32)>,
}

/// A size in `1..=70`, with 1 and 2 often.
pub fn side(r: &mut Rng) -> i32 {
    match r.below(6) {
        0 => 1,
        1 => 2,
        2 => r.int(3, 5),
        _ => r.int(1, 70),
    }
}

/// A stride `set_buffer` accepts: at least a row, a multiple of `min(bpp, 4)`. For 16-byte texels a multiple of 16:
/// the C++ reads them as aligned vectors and faults otherwise.
pub fn stride_for(r: &mut Rng, w: i32, bpp: usize) -> usize {
    let unit = if bpp == 16 { 16 } else { bpp.min(4) };
    let base = w as usize * bpp;
    if r.chance(3) {
        base
    } else {
        base + unit * r.below(4) as usize
    }
}

impl Tex {
    pub fn random(r: &mut Rng, fmt: u32, w: i32, h: i32) -> Tex {
        let bpp = bpp_of(fmt);
        let stride = stride_for(r, w, bpp);
        let len = (h as usize - 1) * stride + w as usize * bpp;
        let mut buf = Guarded::new(len);
        let float = fmt == tx::GL_RGBA32F;
        let b = buf.bytes();
        if float {
            for c in b.as_chunks_mut::<4>().0 {
                let f: f32 = match r.below(10) {
                    0 => f32::from_bits(r.next() as u32),
                    1 => [0.0, -0.0, 1.0, f32::INFINITY, f32::NAN][r.below(5) as usize],
                    _ => r.range(-2.0, 2.0),
                };
                c.copy_from_slice(&f.to_le_bytes());
            }
            // A trailing partial texel (a stride not a multiple of 16) stays random bytes.
        } else {
            for x in b.iter_mut() {
                *x = r.byte_edgy();
            }
        }
        Tex { fmt, w, h, stride, bpp, buf, off: (0, 0), mag_filter: tx::GL_LINEAR, delay: None }
    }

    /// The Rust texture over this buffer.
    pub fn rust(&mut self) -> Texture {
        let mut t = Texture::new(self.fmt, self.w, self.h);
        t.set_should_free(false);
        // SAFETY: the guarded buffer holds `(h - 1) * stride + w * bpp` bytes at `ptr()` and outlives the texture.
        unsafe { t.set_buffer(self.buf.ptr(), self.stride) };
        t.offset = tx::IntPoint::new(self.off.0, self.off.1);
        t.mag_filter = self.mag_filter;
        if let Some((rows, delay, val)) = &self.delay {
            t.enable_delayed_clear(*val);
            t.cleared_rows.as_mut().unwrap().copy_from_slice(rows);
            t.delay_clear = *delay;
        }
        t
    }

    /// The C++ texture over this buffer (`rows` must outlive the call it is passed to).
    pub fn oracle(&mut self) -> o::TTexture {
        let (rows, delay, val) = match &self.delay {
            Some((rows, delay, val)) => (rows.as_ptr(), *delay, *val),
            None => (core::ptr::null(), 0, 0),
        };
        o::TTexture {
            buf: self.buf.ptr(),
            stride: self.stride as i32,
            width: self.w,
            height: self.h,
            internal_format: self.fmt,
            off_x: self.off.0,
            off_y: self.off.1,
            mag_filter: self.mag_filter,
            delay,
            clear_val: val,
            rows,
        }
    }

    /// The sampler stride: bytes below 2 bytes per texel, words for 2, dwords from 4 (`init_sampler`).
    pub fn sampler_stride(&self) -> u32 {
        (match self.bpp {
            1 => self.stride,
            2 => self.stride / 2,
            _ => self.stride / 4,
        }) as u32
    }

    pub fn common(&mut self) -> samplerCommon {
        let (stride, h, w, format) = (self.sampler_stride(), self.h as u32, self.w as u32, format_of(self.fmt));
        // SAFETY: the guarded buffer holds `len` bytes at `ptr()` and outlives the samplers the tests make from it.
        unsafe { samplerCommon::from_raw(self.buf.ptr() as *const u32, self.buf.len, stride, h, w, format) }
    }

    pub fn sampler2d(&mut self, filter: TextureFilter) -> sampler2D_impl {
        let mut s = sampler2D_impl { common: self.common(), ..Default::default() };
        s.filter.filter = filter;
        s
    }

    pub fn sampler_rect(&mut self, filter: TextureFilter) -> sampler2DRect_impl {
        let mut s = sampler2DRect_impl { common: self.common(), ..Default::default() };
        s.filter.filter = filter;
        s
    }

    pub fn oracle_sampler(&mut self, filter: TextureFilter) -> o::TSampler {
        o::TSampler {
            buf: self.buf.ptr(),
            stride: self.sampler_stride(),
            width: self.w as u32,
            height: self.h as u32,
            format: format_of(self.fmt) as i32,
            filter: filter as i32,
        }
    }
}

/// Floats equal by bits, or both NaN.
pub fn same_f32(a: f32, b: f32) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

pub fn same_f32s(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same_f32(*x, *y))
}

/// A random rectangle around a `w` x `h` area: often inside, sometimes partly or wholly outside, sometimes empty.
pub fn rect_around(r: &mut Rng, w: i32, h: i32) -> [i32; 4] {
    let span = |r: &mut Rng, n: i32| -> (i32, i32) {
        match r.below(8) {
            0 => {
                let a = r.int(-n, 2 * n);
                (a, a) // empty
            }
            1 => {
                let a = r.int(-n - 5, 2 * n + 5);
                (a, a + r.int(-3, n + 5)) // anywhere, possibly inverted
            }
            2 => (0, n),
            _ => {
                let a = r.int(-2, n);
                (a, a + r.int(1, n + 2))
            }
        }
    };
    let (x0, x1) = span(r, w);
    let (y0, y1) = span(r, h);
    [x0, y0, x1, y1]
}

/// A random cleared-rows bit vector for a `h`-row texture and the rows it leaves to clear.
pub fn random_delay(r: &mut Rng, h: i32) -> (Vec<u32>, i32) {
    let words = ((h + 31) / 32) as usize;
    let mut rows = vec![0u32; words];
    for w in rows.iter_mut() {
        *w = match r.below(4) {
            0 => 0,
            1 => !0,
            _ => r.next() as u32,
        };
    }
    if h & 31 != 0 {
        rows[(h / 32) as usize] |= !0u32 << (h & 31);
    }
    let pending = (0..h).filter(|&y| rows[(y / 32) as usize] & (1 << (y & 31)) == 0).count() as i32;
    (rows, pending)
}

/// The bytes of a lane vector (without its alignment padding).
pub fn as_bytes<T: lucinanet_raster::glsl::Pod>(v: T) -> Vec<u8> {
    let mut b = vec![0u8; T::SIZE];
    lucinanet_raster::glsl::store_bytes(&mut b, v);
    b
}

pub fn le_bytes(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// The C++ keeps the blend state in statics (`blend_key`, `swgl_*`, `ctx`): tests that set it hold this lock, since
/// the test harness runs tests on parallel threads.
pub fn oracle_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
