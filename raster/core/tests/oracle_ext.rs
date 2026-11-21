/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Bit-exactness of `lucinanet_raster::ext` (swgl_ext.h) against swgl's C++ (`tests/oracle/shim_x.cc`, the whole
//! of gl.cc built strict-float by the `lucinanet-raster-oracle` dev-dependency).
//!
//! Each case draws a random span of 1 to 40 pixels at a random alignment over random destination pixels, with a
//! random raster state (every blend key, with and without the clip-mask and AA groups, random clip mask, AA
//! distances, blend colors, dual-source color and fragment position) and random textures, coordinates, bounds and
//! colors built to reach every filter path (1:1, subpixel offsets, up- and downscaling, flips, clamping,
//! repeats). The C++ and the Rust run on copies of the same bytes; the whole buffer, including 2 chunks past the
//! span (the C++ writes whole chunks), and the span length left must be identical.

// The states start from Default and take the fields a case sets, one per line, as the C++ wrappers do.
#![allow(clippy::too_many_arguments, clippy::field_reassign_with_default)]

use lucinanet_raster::state::RasterState;
use lucinanet_raster::ext::*;
use lucinanet_raster::glsl::*;
use lucinanet_raster::program::{FragmentShaderState, OutPtr};
use lucinanet_raster_oracle::ext as o;

const N: usize = 4000;
/// Bytes after the span's slack that nothing may write.
const GUARD: usize = 4096;

/// swgl keeps its raster state in C++ statics: one test thread in the oracle at a time.
static ORACLE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    ORACLE.lock().unwrap_or_else(|e| e.into_inner())
}
const SEED: u64 = 0x5eed_e47e_5753_4c21;

struct Rng(u64);

impl Rng {
    fn new(salt: &str) -> Self {
        let mut h = SEED;
        for b in salt.bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
        Rng(h)
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn int(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo + 1) as u64) as i32
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
    fn pick<T: Copy>(&mut self, v: &[T]) -> T {
        v[self.below(v.len() as u64) as usize]
    }
}

// ---- textures -----------------------------------------------------------------------------------------------------

/// A texture both sides sample: `len` bytes of texels and 64 zero bytes after them, which the C++ may read past
/// the texture where the Rust reads zero.
struct Tex {
    data: Texels,
    len: usize,
    w: u32,
    h: u32,
    stride: u32,
    format: TextureFormat,
    filter: TextureFilter,
}

fn bpp(f: TextureFormat) -> u32 {
    match f {
        TextureFormat::RGBA32F | TextureFormat::RGBA32I => 16,
        TextureFormat::RGBA8 | TextureFormat::RG16 => 4,
        TextureFormat::R8 => 1,
        TextureFormat::RG8 | TextureFormat::R16 | TextureFormat::YUY2 => 2,
    }
}

/// A texture buffer of `u32`s on a 16-byte boundary, as glibc's realloc returns them.
struct Texels(Vec<A16>, usize);

#[derive(Clone, Copy)]
#[repr(C, align(16))]
struct A16([u32; 4]);

impl Texels {
    /// `n` texel words after one 16-byte guard of 0xA5 bytes, so that a read before the texture (which the C++ does
    /// for a 1-texel-wide linear plane: `clampCoord(x, width - 1)` is -1) sees the same non-zero bytes on every
    /// target instead of whatever the allocator keeps there (zero on glibc x86_64, not on the laptop's musl).
    fn new(n: usize) -> Texels {
        let mut v = vec![A16([0; 4]); n.div_ceil(4) + 1];
        v[0] = A16([0xA5A5_A5A5; 4]);
        Texels(v, n)
    }
}

impl std::ops::Deref for Texels {
    type Target = [u32];
    fn deref(&self) -> &[u32] {
        // SAFETY: `n` words from the second 16-byte element are inside the vector (`n.div_ceil(4)` elements).
        unsafe { core::slice::from_raw_parts((self.0.as_ptr() as *const u32).add(4), self.1) }
    }
}

impl std::ops::DerefMut for Texels {
    fn deref_mut(&mut self) -> &mut [u32] {
        // SAFETY: as in `deref`.
        unsafe { core::slice::from_raw_parts_mut((self.0.as_mut_ptr() as *mut u32).add(4), self.1) }
    }
}

impl Tex {
    fn new(rng: &mut Rng, format: TextureFormat, w: u32, h: u32, filter: TextureFilter) -> Tex {
        let b = bpp(format);
        // As gl.cc allocates them: realloc'd (16-byte aligned) and rows of whole texels, so the RGBA32F gradient
        // tables, which swgl reads as `Float*` (an aligned vector load), keep every texel on 16 bytes. The extra
        // row padding of an external buffer is whole texels too.
        let unit = b.max(4);
        let stride_bytes = (w * b).div_ceil(4) * 4 + if rng.chance(30) { unit * rng.below(3) as u32 } else { 0 };
        let len = (stride_bytes * h) as usize;
        let mut data = Texels::new(len / 4 + 16);
        for d in data.iter_mut().take(len / 4) {
            *d = rng.next() as u32;
        }
        let stride = if b >= 4 { stride_bytes / 4 } else { stride_bytes / b };
        Tex { data, len, w, h, stride, format, filter }
    }
    /// [`Tex::random`] at least 2 texels wide, 4 for YUY2 (the same draws): YUV planes are sampled linearly, and on
    /// narrower planes the C++ taps start before the texture (`clampCoord(x, width - margin)` is negative).
    fn random_yuv(rng: &mut Rng, format: TextureFormat, filter: TextureFilter) -> Tex {
        let w = if rng.chance(10) { rng.int(1, 3) as u32 } else { rng.int(2, 48) as u32 };
        let h = rng.int(1, 12) as u32;
        // YUY2 taps two texel pairs with a margin of 2 (`computeRow(sampler, i, 2)`), so 4 wide.
        let min = if format == TextureFormat::YUY2 { 4 } else { 2 };
        Tex::new(rng, format, w.max(min), h, filter)
    }
    fn random(rng: &mut Rng, format: TextureFormat, filter: TextureFilter) -> Tex {
        let w = if rng.chance(10) { rng.int(1, 3) as u32 } else { rng.int(2, 48) as u32 };
        let h = rng.int(1, 12) as u32;
        Tex::new(rng, format, w, h, filter)
    }
    fn x(&self) -> o::XTex {
        o::XTex {
            buf: self.data.as_ptr() as *const _,
            stride: self.stride,
            height: self.h,
            width: self.w,
            format: self.format as u32,
            filter: self.filter as u32,
        }
    }
    fn common(&self) -> samplerCommon {
        let buf = self.data.as_ptr();
        // SAFETY: `data` holds `len` bytes and outlives the samplers made from it.
        unsafe { samplerCommon::from_raw(buf, self.len, self.stride, self.h, self.w, self.format) }
    }
    fn s2d(&self) -> sampler2D_impl {
        sampler2D_impl { common: self.common(), filter: samplerFilter { filter: self.filter } }
    }
    fn rect(&self) -> sampler2DRect_impl {
        sampler2DRect_impl { common: self.common(), filter: samplerFilter { filter: self.filter } }
    }
}

// ---- spans and state ---------------------------------------------------------------------------------------------

/// The destination span and the raster state around it.
struct Span {
    st: o::XState,
    mask: Vec<u8>,
    len: i32,
    bytes: Vec<u8>,
    /// Byte offset of the span in `bytes`.
    off: usize,
    rgba8: bool,
}

const RGBA8_KEYS: u8 = 31;
const R8_KEYS: [u8; 3] = [0, 6, 7];

fn span(rng: &mut Rng, rgba8: bool) -> Span {
    let len = rng.int(1, 40);
    let b = if rgba8 { 4 } else { 1 };
    let off = if rgba8 { 4 * rng.below(4) as usize } else { rng.below(8) as usize };
    let total = off + (len as usize + 8) * b + 8 + GUARD;
    let bytes: Vec<u8> = (0..total).map(|_| rng.next() as u8).collect();
    let base = if rgba8 { rng.below(RGBA8_KEYS as u64) as u8 } else { rng.pick(&R8_KEYS) };
    let blend = rng.chance(75);
    let (mask, aa) = if blend { (rng.chance(30), rng.chance(30)) } else { (false, false) };
    let key = if blend { base + if mask { 31 } else { 0 } + if aa { 62 } else { 0 } } else { 0 };
    let flags = (if mask { 1 } else { 0 }) | (if aa { 2 } else { 0 });
    let mut st = o::XState {
        blend_key: key as u32,
        clip_flags: flags,
        blend_color: [0; 16],
        blend_alpha: [0; 16],
        blendcolor: [0; 8],
        clip_mask_buf: core::ptr::null(),
        opaque_start: rng.int(-6, len + 6),
        opaque_size: (rng.int(0, len + 4) as u32) * b as u32,
        left_aa_dist: [0.0; 4],
        right_aa_dist: [0.0; 4],
        aa_slope: [0.0; 4],
        frag_coord: [0.0; 16],
        secondary: [0.0; 16],
    };
    for k in 0..16 {
        st.blend_color[k] = rng.below(256) as u16;
        st.blend_alpha[k] = rng.below(256) as u16;
        st.secondary[k] = if rng.chance(90) { rng.unit() } else { rng.range(-0.5, 1.5) };
    }
    for k in 0..8 {
        st.blendcolor[k] = rng.below(256) as u16;
    }
    for k in 0..4 {
        st.left_aa_dist[k] = rng.range(-64.0, 320.0);
        st.right_aa_dist[k] = rng.range(-64.0, 320.0);
    }
    st.aa_slope = [rng.range(-64.0, 64.0) / b as f32, rng.range(-64.0, 64.0) / b as f32, 0.0, 0.0];
    let (fx, fy) = (rng.int(-20, 2000) as f32, rng.int(-20, 2000) as f32);
    for k in 0..4 {
        st.frag_coord[k] = fx + k as f32 + 0.5;
        st.frag_coord[4 + k] = fy + 0.5;
        st.frag_coord[8 + k] = 0.5;
        st.frag_coord[12 + k] = 1.0;
    }
    // Debugging aid: ORACLE_KEY=<n> replaces every span's blend key (and clears the clip flags) after the random
    // draws, so a failing case can be rerun on the same inputs without blending (0) or with another key.
    if let Some(k) = std::env::var("ORACLE_KEY").ok().and_then(|v| v.parse::<u32>().ok()) {
        st.blend_key = k;
        st.clip_flags = 0;
    }
    let mask_bytes: Vec<u8> = (0..len as usize + 16).map(|_| if rng.chance(30) { { let a_ = [0, 255]; rng.pick(&a_) } } else { rng.next() as u8 }).collect();
    Span { st, mask: mask_bytes, len, bytes, off, rgba8 }
}

fn lanes(p: &[f32]) -> Float {
    Float::new([p[0], p[1], p[2], p[3]])
}

fn rust_state(sp: &Span, buf: *mut u8) -> RasterState {
    let st = &sp.st;
    let b = if sp.rgba8 { 4 } else { 1 };
    let mut c = RasterState::default();
    c.blend_key = st.blend_key as u8;
    c.swgl_ClipFlags = st.clip_flags;
    c.swgl_BlendColorRGBA8 = V16::new(st.blend_color);
    c.swgl_BlendAlphaRGBA8 = V16::new(st.blend_alpha);
    c.blendcolor = V8::new(st.blendcolor);
    c.swgl_SpanBuf = buf;
    c.swgl_ClipMaskBuf = sp.mask.as_ptr() as *mut u8;
    c.swgl_OpaqueStart = buf.wrapping_offset(st.opaque_start as isize * b);
    c.swgl_OpaqueSize = st.opaque_size;
    c.swgl_LeftAADist = lanes(&st.left_aa_dist);
    c.swgl_RightAADist = lanes(&st.right_aa_dist);
    c.swgl_AASlope = lanes(&st.aa_slope);
    c
}

fn v4(p: &[f32; 16]) -> vec4 {
    vec4::new(lanes(&p[0..4]), lanes(&p[4..8]), lanes(&p[8..12]), lanes(&p[12..16]))
}

fn rust_fs(sp: &Span, buf: *mut u8) -> FragmentShaderState {
    let mut fs = FragmentShaderState::default();
    fs.gl_FragCoord = v4(&sp.st.frag_coord);
    fs.gl_SecondaryFragColor = v4(&sp.st.secondary);
    if sp.rgba8 {
        fs.swgl_OutRGBA8 = OutPtr(buf as *mut u32);
    } else {
        fs.swgl_OutR8 = OutPtr(buf);
    }
    fs.swgl_SpanLength = sp.len;
    fs
}

/// Run one case through both sides and require the same bytes and span length left.
fn check(
    name: &str,
    case: usize,
    sp: &Span,
    what: &dyn std::fmt::Debug,
    cpp: &mut dyn FnMut(*const o::XState, *mut u8, i32) -> i32,
    rust: &mut dyn FnMut(&mut RasterState, &mut FragmentShaderState),
) {
    let _guard = lock();
    if std::env::var_os("XTRACE").is_some() {
        eprintln!("{name} {case} len {} key {} {:?}", sp.len, sp.st.blend_key, what);
    }
    let mut a = sp.bytes.clone();
    let mut b = sp.bytes.clone();
    let mut st = sp.st;
    st.clip_mask_buf = sp.mask.as_ptr();
    let ra = if std::env::var_os("XSKIPCPP").is_some() { 0 } else { cpp(&st, a[sp.off..].as_mut_ptr(), sp.len) };
    if std::env::var_os("XTRACE").is_some() {
        eprintln!("  C++ done");
    }
    let bb = b[sp.off..].as_mut_ptr();
    let mut ctx = rust_state(sp, bb);
    let mut fs = rust_fs(sp, bb);
    rust(&mut ctx, &mut fs);
    if std::env::var_os("XSKIPCPP").is_some() {
        return;
    }
    // Debugging aid: ORACLE_DUMP=<name>:<case> prints both outputs of that case, to compare one ISA with another.
    if std::env::var("ORACLE_DUMP").ok().as_deref() == Some(&format!("{name}:{case}")) {
        eprintln!("{name} {case} key {} in {:?}", sp.st.blend_key, &sp.bytes[sp.off..sp.off + 4 * sp.len as usize]);
        eprintln!("{name} {case} key {} C++ {:?}", sp.st.blend_key, &a[sp.off..sp.off + 4 * sp.len as usize]);
        eprintln!("{name} {case} key {} Rust {:?}", sp.st.blend_key, &b[sp.off..sp.off + 4 * sp.len as usize]);
    }
    let px = if sp.rgba8 { 4 } else { 1 };
    let out = if sp.rgba8 { fs.swgl_OutRGBA8.0 as usize } else { fs.swgl_OutR8.0 as usize };
    let rb = if (out - bb as usize) as i64 == (sp.len - fs.swgl_SpanLength) as i64 * px { fs.swgl_SpanLength } else { -1000 };
    let limit = sp.bytes.len() - GUARD;
    for (side, buf) in [("C++", &a), ("Rust", &b)] {
        if buf[limit..] != sp.bytes[limit..] {
            panic!("{name} case {case}: {side} wrote past the span's slack (len {}): {:?}", sp.len, what);
        }
    }
    if ra != rb || a != b {
        let first = a.iter().zip(&b).position(|(x, y)| x != y);
        panic!(
            "{name} case {case}: len {} key {} flags {} off {}: C++ left {ra}, Rust left {rb}; first differing byte {:?} \
             (C++ {:?}, Rust {:?})\n{:?}",
            sp.len,
            sp.st.blend_key,
            sp.st.clip_flags,
            sp.off,
            first.map(|i| i as isize - sp.off as isize),
            first.map(|i| &a[i..(i + 8).min(a.len())]),
            first.map(|i| &b[i..(i + 8).min(b.len())]),
            what,
        );
    }
}

// ---- argument generators -----------------------------------------------------------------------------------------

fn color(rng: &mut Rng) -> [f32; 4] {
    let mut c = [0.0; 4];
    for v in c.iter_mut() {
        *v = if rng.chance(85) { rng.unit() } else { rng.range(-0.5, 1.5) };
    }
    if rng.chance(20) {
        c[3] = 1.0;
    }
    c
}

/// UVs of a span over `t`: 4 lanes, a start and a step chosen to reach each filter path, in texels for a
/// `sampler2DRect` and normalised for a `sampler2D`.
fn uv(rng: &mut Rng, t: &Tex, rect: bool) -> [f32; 8] {
    let (w, h) = (t.w as f32, t.h as f32);
    let frac = { let a_ = [0.5, 0.5, 0.0, 0.25, 0.75, 0.125, rng.unit()]; rng.pick(&a_) };
    let x0 = rng.int(-6, t.w as i32 + 4) as f32 + frac;
    let y0 = rng.int(-2, t.h as i32 + 1) as f32 + { let a_ = [0.5, 0.5, 0.25, rng.unit()]; rng.pick(&a_) };
    let dx = match rng.below(8) {
        0..=2 => 1.0,
        3 => rng.range(0.1, 0.99),
        4 => 2.0,
        5 => -1.0,
        6 => rng.range(-3.0, 3.0),
        _ => { let a_ = [0.5, 0.25, 4.0, 0.0]; rng.pick(&a_) },
    };
    let dy = if rng.chance(75) { 0.0 } else { rng.range(-1.0, 1.0) };
    let (sx, sy) = if rect { (1.0, 1.0) } else { (1.0 / w, 1.0 / h) };
    let mut p = [0.0; 8];
    for k in 0..4 {
        p[k] = (x0 + dx * k as f32) * sx;
        p[4 + k] = (y0 + dy * k as f32) * sy;
    }
    p
}

/// A sampling rect: texel centres (as shaders give them) or edges, partly inside the texture.
fn uv_rect(rng: &mut Rng, t: &Tex, rect: bool) -> [f32; 4] {
    let (w, h) = (t.w as i32, t.h as i32);
    let x0 = rng.int(-2, w);
    let y0 = rng.int(-2, h);
    let x1 = rng.int(x0 - 1, w + 2);
    let y1 = rng.int(y0 - 1, h + 2);
    let c = if rng.chance(70) { 0.5 } else { 0.0 };
    let mut r = [x0 as f32 + c, y0 as f32 + c, x1 as f32 - c, y1 as f32 - c];
    if rng.chance(20) {
        r = [-100.0, -100.0, 1000.0, 1000.0];
    }
    if !rect {
        r = [r[0] / t.w as f32, r[1] / t.h as f32, r[2] / t.w as f32, r[3] / t.h as f32];
    }
    r
}

fn tile_repeat(rng: &mut Rng) -> [f32; 2] {
    if rng.chance(50) {
        [0.0, 0.0]
    } else {
        [{ let a_ = [1.0, 2.0, 3.0, rng.range(0.5, 4.0)]; rng.pick(&a_) }, { let a_ = [1.0, 2.0, rng.range(0.5, 4.0)]; rng.pick(&a_) }]
    }
}

/// The repeated area and the matching UVs, which run over `[0, n)` repeats.
fn repeat_args(rng: &mut Rng, t: &Tex, rect: bool) -> ([f32; 8], [f32; 4]) {
    let rep = uv_rect(rng, t, rect);
    let steps = { let a_ = [0.25, 0.5, 0.1, 0.03, rng.range(-0.3, 0.3)]; rng.pick(&a_) };
    let x0 = rng.range(-0.5, 3.0);
    let y0 = rng.range(-0.5, 3.0);
    let dy = if rng.chance(70) { 0.0 } else { rng.range(-0.2, 0.2) };
    let mut p = [0.0; 8];
    for k in 0..4 {
        p[k] = x0 + steps * k as f32;
        p[4 + k] = y0 + dy * k as f32;
    }
    (p, rep)
}

fn v2l(p: &[f32; 8]) -> vec2 {
    vec2::new(lanes(&p[0..4]), lanes(&p[4..8]))
}
fn v4s(p: &[f32; 4]) -> vec4_scalar {
    vec4_scalar::new(p[0], p[1], p[2], p[3])
}
fn v2s(p: &[f32; 2]) -> vec2_scalar {
    vec2_scalar::new(p[0], p[1])
}

fn rgba8_format(rng: &mut Rng) -> TextureFormat {
    if rng.chance(90) {
        TextureFormat::RGBA8
    } else {
        TextureFormat::R8
    }
}
fn r8_format(rng: &mut Rng) -> TextureFormat {
    if rng.chance(90) {
        TextureFormat::R8
    } else {
        TextureFormat::RGBA8
    }
}
fn filter(rng: &mut Rng) -> TextureFilter {
    if rng.chance(70) {
        TextureFilter::LINEAR
    } else {
        TextureFilter::NEAREST
    }
}

// ---- solid and color ---------------------------------------------------------------------------------------------

#[test]
fn solid_and_color() {
    let mut rng = Rng::new("solid");
    for case in 0..N {
        let c = color(&mut rng);
        let sp = span(&mut rng, true);
        check("commitSolidRGBA8", case, &sp, &c, &mut |st, b, l| unsafe { o::x_commitSolidRGBA8(st, b as _, l, c.as_ptr()) },
            &mut |ctx, fs| swgl_commitSolidRGBA8(ctx, v4s(&c), fs));
        let n = rng.int(0, 44);
        let sp = span(&mut rng, true);
        check("commitPartialSolidRGBA8", case, &sp, &(n, c), &mut |st, b, l| unsafe {
            o::x_commitPartialSolidRGBA8(st, b as _, l, n, c.as_ptr())
        }, &mut |ctx, fs| swgl_commitPartialSolidRGBA8(ctx, n, v4s(&c), fs));
        let v = c[0];
        let sp = span(&mut rng, false);
        check("commitSolidR8", case, &sp, &v, &mut |st, b, l| unsafe { o::x_commitSolidR8(st, b, l, v) },
            &mut |ctx, fs| swgl_commitSolidR8(ctx, v, fs));
        let n = rng.int(0, 44);
        let sp = span(&mut rng, false);
        check("commitPartialSolidR8", case, &sp, &(n, v), &mut |st, b, l| unsafe { o::x_commitPartialSolidR8(st, b, l, n, v) },
            &mut |ctx, fs| swgl_commitPartialSolidR8(ctx, n, v, fs));
        let mut c16 = [0.0f32; 16];
        for x in c16.iter_mut() {
            *x = if rng.chance(90) { rng.unit() } else { rng.range(-0.5, 1.5) };
        }
        let sp = span(&mut rng, true);
        check("commitColorRGBA8", case, &sp, &c16, &mut |st, b, l| unsafe { o::x_commitColorRGBA8(st, b as _, l, c16.as_ptr()) },
            &mut |ctx, fs| {
                let v = vec4::new(lanes(&c16[0..4]), lanes(&c16[4..8]), lanes(&c16[8..12]), lanes(&c16[12..16]));
                swgl_commitColorRGBA8(ctx, v, fs)
            });
        let sp = span(&mut rng, false);
        check("commitColorR8", case, &sp, &c16, &mut |st, b, l| unsafe { o::x_commitColorR8(st, b, l, c16.as_ptr()) },
            &mut |ctx, fs| swgl_commitColorR8(ctx, lanes(&c16[0..4]), fs));
    }
}

// ---- linear ------------------------------------------------------------------------------------------------------

/// Calls `$rust` with the sampler as a `sampler2D` or a `sampler2DRect`.
macro_rules! sampled {
    ($rect:expr, $t:expr, |$s:ident| $body:expr) => {
        if $rect {
            let $s = $t.rect();
            $body
        } else {
            let $s = $t.s2d();
            $body
        }
    };
}

#[test]
fn texture_linear() {
    let mut rng = Rng::new("linear");
    for case in 0..N {
        let rect = rng.chance(30);
        let t = { let f_ = rgba8_format(&mut rng); let g_ = filter(&mut rng); Tex::random(&mut rng, f_, g_) };
        let (p, r, c) = (uv(&mut rng, &t, rect), uv_rect(&mut rng, &t, rect), color(&mut rng));
        let x = t.x();
        let ri = rect as i32;
        let what = (t.w, t.h, t.format, p, r, c);
        let sp = span(&mut rng, true);
        check("commitTextureLinearRGBA8", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureLinearRGBA8(st, b as _, l, &x, ri, p.as_ptr(), r.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t, |s| swgl_commitTextureLinearRGBA8(ctx, s, v2l(&p), v4s(&r), fs)));
        let sp = span(&mut rng, true);
        check("commitTextureLinearColorRGBA8", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureLinearColorRGBA8(st, b as _, l, &x, ri, p.as_ptr(), r.as_ptr(), c.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t, |s| swgl_commitTextureLinearColorRGBA8(ctx, s, v2l(&p), v4s(&r), v4s(&c), fs)));
        let sp = span(&mut rng, true);
        let f = c[3];
        check("commitTextureLinearColorRGBA8(float)", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureLinearColorRGBA8f(st, b as _, l, &x, ri, p.as_ptr(), r.as_ptr(), f)
        }, &mut |ctx, fs| sampled!(rect, t, |s| swgl_commitTextureLinearColorRGBA8(ctx, s, v2l(&p), v4s(&r), f, fs)));

        // R8 spans.
        let t8 = { let f_ = r8_format(&mut rng); let g_ = filter(&mut rng); Tex::random(&mut rng, f_, g_) };
        let (p, r) = (uv(&mut rng, &t8, rect), uv_rect(&mut rng, &t8, rect));
        let x8 = t8.x();
        let what = (t8.w, t8.h, t8.format, p, r);
        let sp = span(&mut rng, false);
        check("commitTextureLinearR8", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureLinearR8(st, b, l, &x8, ri, p.as_ptr(), r.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t8, |s| swgl_commitTextureLinearR8(ctx, s, v2l(&p), v4s(&r), fs)));
        let n = rng.int(0, 44);
        let sp = span(&mut rng, false);
        check("commitPartialTextureLinearR8", case, &sp, &(n, what), &mut |st, b, l| unsafe {
            o::x_commitPartialTextureLinearR8(st, b, l, n, &x8, ri, p.as_ptr(), r.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t8, |s| swgl_commitPartialTextureLinearR8(ctx, n, s, v2l(&p), v4s(&r), fs)));
        let sp = span(&mut rng, false);
        check("commitPartialTextureLinearInvertR8", case, &sp, &(n, what), &mut |st, b, l| unsafe {
            o::x_commitPartialTextureLinearInvertR8(st, b, l, n, &x8, ri, p.as_ptr(), r.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t8, |s| swgl_commitPartialTextureLinearInvertR8(ctx, n, s, v2l(&p), v4s(&r), fs)));

        // R8 textures into RGBA8 spans.
        let sp = span(&mut rng, true);
        check("commitTextureLinearR8ToRGBA8", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureLinearR8ToRGBA8(st, b as _, l, &x8, ri, p.as_ptr(), r.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t8, |s| swgl_commitTextureLinearR8ToRGBA8(ctx, s, v2l(&p), v4s(&r), fs)));
        let sp = span(&mut rng, true);
        check("commitTextureLinearColorR8ToRGBA8", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureLinearColorR8ToRGBA8(st, b as _, l, &x8, ri, p.as_ptr(), r.as_ptr(), c.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t8, |s| swgl_commitTextureLinearColorR8ToRGBA8(ctx, s, v2l(&p), v4s(&r), v4s(&c), fs)));
    }
}

#[test]
fn texture_repeat_and_nearest() {
    let mut rng = Rng::new("repeat");
    for case in 0..N {
        let rect = rng.chance(30);
        let ri = rect as i32;
        let t = { let f_ = rgba8_format(&mut rng); let g_ = filter(&mut rng); Tex::random(&mut rng, f_, g_) };
        let x = t.x();
        let (rp, rep) = repeat_args(&mut rng, &t, rect);
        let (tr, r, c) = (tile_repeat(&mut rng), uv_rect(&mut rng, &t, rect), color(&mut rng));
        let what = (t.w, t.h, t.format, t.filter, rp, tr, rep, r);
        let sp = span(&mut rng, true);
        check("commitTextureLinearRepeatRGBA8", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureLinearRepeatRGBA8(st, b as _, l, &x, ri, rp.as_ptr(), tr.as_ptr(), rep.as_ptr(), r.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t, |s| swgl_commitTextureLinearRepeatRGBA8(ctx, s, v2l(&rp), v2s(&tr), v4s(&rep), v4s(&r), fs)));
        let sp = span(&mut rng, true);
        check("commitTextureLinearRepeatColorRGBA8", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureLinearRepeatColorRGBA8(st, b as _, l, &x, ri, rp.as_ptr(), tr.as_ptr(), rep.as_ptr(), r.as_ptr(), c.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t, |s| swgl_commitTextureLinearRepeatColorRGBA8(ctx, s, v2l(&rp), v2s(&tr), v4s(&rep), v4s(&r), v4s(&c), fs)));
        let sp = span(&mut rng, true);
        check("commitTextureNearestRepeatRGBA8", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureNearestRepeatRGBA8(st, b as _, l, &x, ri, rp.as_ptr(), tr.as_ptr(), rep.as_ptr(), r.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t, |s| swgl_commitTextureNearestRepeatRGBA8(ctx, s, v2l(&rp), v2s(&tr), v4s(&rep), v4s(&r), fs)));
        let sp = span(&mut rng, true);
        check("commitTextureNearestRepeatColorRGBA8", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureNearestRepeatColorRGBA8(st, b as _, l, &x, ri, rp.as_ptr(), tr.as_ptr(), rep.as_ptr(), r.as_ptr(), c.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t, |s| swgl_commitTextureNearestRepeatColorRGBA8(ctx, s, v2l(&rp), v2s(&tr), v4s(&rep), v4s(&r), v4s(&c), fs)));
        let sp = span(&mut rng, true);
        check("commitTextureRepeatRGBA8", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureRepeatRGBA8(st, b as _, l, &x, ri, rp.as_ptr(), tr.as_ptr(), rep.as_ptr(), r.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t, |s| swgl_commitTextureRepeatRGBA8(ctx, s, v2l(&rp), v2s(&tr), v4s(&rep), v4s(&r), fs)));
        let sp = span(&mut rng, true);
        check("commitTextureRepeatColorRGBA8", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureRepeatColorRGBA8(st, b as _, l, &x, ri, rp.as_ptr(), tr.as_ptr(), rep.as_ptr(), r.as_ptr(), c.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t, |s| swgl_commitTextureRepeatColorRGBA8(ctx, s, v2l(&rp), v2s(&tr), v4s(&rep), v4s(&r), v4s(&c), fs)));

        let p = uv(&mut rng, &t, rect);
        let what = (t.w, t.h, t.format, t.filter, p, r);
        let sp = span(&mut rng, true);
        check("commitTextureNearestRGBA8", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureNearestRGBA8(st, b as _, l, &x, ri, p.as_ptr(), r.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t, |s| swgl_commitTextureNearestRGBA8(ctx, s, v2l(&p), v4s(&r), fs)));
        let sp = span(&mut rng, true);
        check("commitTextureNearestColorRGBA8", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureNearestColorRGBA8(st, b as _, l, &x, ri, p.as_ptr(), r.as_ptr(), c.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t, |s| swgl_commitTextureNearestColorRGBA8(ctx, s, v2l(&p), v4s(&r), v4s(&c), fs)));
        let sp = span(&mut rng, true);
        check("commitTextureRGBA8", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureRGBA8(st, b as _, l, &x, ri, p.as_ptr(), r.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t, |s| swgl_commitTextureRGBA8(ctx, s, v2l(&p), v4s(&r), fs)));
        let sp = span(&mut rng, true);
        check("commitTextureColorRGBA8", case, &sp, &what, &mut |st, b, l| unsafe {
            o::x_commitTextureColorRGBA8(st, b as _, l, &x, ri, p.as_ptr(), r.as_ptr(), c.as_ptr())
        }, &mut |ctx, fs| sampled!(rect, t, |s| swgl_commitTextureColorRGBA8(ctx, s, v2l(&p), v4s(&r), v4s(&c), fs)));
    }
}

// ---- blur --------------------------------------------------------------------------------------------------------

/// The uv and clamp rect cs_blur passes: `vUvClampRect` is the source rect inside the texture, inset by half a
/// texel, and `uv = mix(vUvRect.xy, vUvRect.zw, vUv)` is inside it, stepping one texel per pixel. (gl.cc does not
/// accept a uv outside the texture: gaussianBlurVertical then steps its rows outside the buffer.)
fn blur_uv(rng: &mut Rng, t: &Tex) -> ([f32; 8], [f32; 4]) {
    let (w, h) = (t.w as i32, t.h as i32);
    let x0 = rng.int(0, w - 1);
    let x1 = rng.int(x0 + 1, w);
    let y0 = rng.int(0, h - 1);
    let y1 = rng.int(y0 + 1, h);
    let r = [(x0 as f32 + 0.5) / w as f32, (y0 as f32 + 0.5) / h as f32, (x1 as f32 - 0.5) / w as f32,
        (y1 as f32 - 0.5) / h as f32];
    let ux = rng.range(x0 as f32, x1 as f32);
    let uy = rng.range(y0 as f32, y1 as f32);
    let mut p = [0.0; 8];
    for k in 0..4 {
        p[k] = (ux + k as f32) / w as f32;
        p[4 + k] = uy / h as f32;
    }
    (p, r)
}

#[test]
fn gaussian_blur() {
    let mut rng = Rng::new("blur");
    for case in 0..N {
        let rgba8 = rng.chance(60);
        let fmt = if rgba8 { rgba8_format(&mut rng) } else { r8_format(&mut rng) };
        let t = { let f_ = fmt; let g_ = TextureFilter::LINEAR; Tex::random(&mut rng, f_, g_) };
        let x = t.x();
        let (p, r) = blur_uv(&mut rng, &t);
        let hori = rng.chance(50) as i32;
        let radius = rng.int(0, 12);
        let coeffs = [rng.range(0.0, 1.0), rng.range(0.3, 1.0)];
        let what = (t.w, t.h, t.format, p, r, hori, radius, coeffs);
        let sp = span(&mut rng, rgba8);
        if rgba8 {
            check("commitGaussianBlurRGBA8", case, &sp, &what, &mut |st, b, l| unsafe {
                o::x_commitGaussianBlurRGBA8(st, b as _, l, &x, p.as_ptr(), r.as_ptr(), hori, radius, coeffs.as_ptr())
            }, &mut |ctx, fs| swgl_commitGaussianBlurRGBA8(ctx, t.s2d(), v2l(&p), v4s(&r), hori != 0, radius, v2s(&coeffs), fs));
        } else {
            check("commitGaussianBlurR8", case, &sp, &what, &mut |st, b, l| unsafe {
                o::x_commitGaussianBlurR8(st, b, l, &x, p.as_ptr(), r.as_ptr(), hori, radius, coeffs.as_ptr())
            }, &mut |ctx, fs| swgl_commitGaussianBlurR8(ctx, t.s2d(), v2l(&p), v4s(&r), hori != 0, radius, v2s(&coeffs), fs));
        }
    }
}

// ---- YUV ---------------------------------------------------------------------------------------------------------

fn yuv_matrix(rng: &mut Rng) -> ([f32; 3], [f32; 9]) {
    let by = rng.range(0.0, 0.6);
    let bias = [rng.range(0.0, 0.1), by, by];
    let mut m = [0.0f32; 9];
    for v in m.iter_mut() {
        *v = rng.range(-2.0, 2.0);
    }
    m[1] = rng.range(0.8, 1.3);
    if rng.chance(50) {
        m[0] = 0.0;
    }
    (bias, m)
}

fn mat3(m: &[f32; 9]) -> mat3_scalar {
    make_mat3((m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8]))
}

#[test]
fn texture_yuv() {
    let mut rng = Rng::new("yuv");
    for case in 0..N {
        let rect = rng.chance(50);
        let ri = rect as i32;
        let filt = if rng.chance(92) { TextureFilter::LINEAR } else { TextureFilter::NEAREST };
        let (bias, m) = yuv_matrix(&mut rng);
        let c = color(&mut rng);
        let with_color = rng.chance(25);

        // One plane.
        let f0 = { let a_ = [TextureFormat::RGBA8, TextureFormat::YUY2]; rng.pick(&a_) };
        let t0 = Tex::random_yuv(&mut rng, f0, filt);
        let (p0, r0) = (uv(&mut rng, &t0, rect), uv_rect(&mut rng, &t0, rect));
        let x0 = t0.x();
        let what = (t0.w, t0.h, t0.format, p0, r0, bias, m);
        let sp = span(&mut rng, true);
        if with_color {
            check("commitTextureLinearColorYUV1", case, &sp, &what, &mut |st, b, l| unsafe {
                o::x_commitTextureLinearColorYUV1(st, b as _, l, &x0, ri, p0.as_ptr(), r0.as_ptr(), bias.as_ptr(), m.as_ptr(), 0, c.as_ptr())
            }, &mut |ctx, fs| sampled!(rect, t0, |s| swgl_commitTextureLinearColorYUV(ctx, s, v2l(&p0), v4s(&r0),
                vec3_scalar::new(bias[0], bias[1], bias[2]), mat3(&m), 0, v4s(&c), fs)));
        } else {
            check("commitTextureLinearYUV1", case, &sp, &what, &mut |st, b, l| unsafe {
                o::x_commitTextureLinearYUV1(st, b as _, l, &x0, ri, p0.as_ptr(), r0.as_ptr(), bias.as_ptr(), m.as_ptr(), 0)
            }, &mut |ctx, fs| sampled!(rect, t0, |s| swgl_commitTextureLinearYUV(ctx, s, v2l(&p0), v4s(&r0),
                vec3_scalar::new(bias[0], bias[1], bias[2]), mat3(&m), 0, fs)));
        }

        // Two planes: R8 + RG8 or RGBA8, R16 + RG16.
        let hdr = rng.chance(35);
        let rescale = if hdr { rng.int(0, 6) } else { 0 };
        let (f0, f1) = if hdr {
            (TextureFormat::R16, TextureFormat::RG16)
        } else {
            (TextureFormat::R8, { let a_ = [TextureFormat::RG8, TextureFormat::RGBA8]; rng.pick(&a_) })
        };
        let ty = { let f_ = f0; let g_ = filt; Tex::random_yuv(&mut rng, f_, g_) };
        // At least 2 texels wide: on a 1-texel-wide plane the C++ linear taps start at x = -1 (`clampCoord(x,
        // width - 1)`), a read before the texture whose bytes are the allocator's; the Rust clips it (DESIGN rule 5).
        let tuv = Tex::new(&mut rng, f1, (ty.w / 2).max(2), (ty.h / 2).max(1), filt);
        let (py, ry) = (uv(&mut rng, &ty, rect), uv_rect(&mut rng, &ty, rect));
        let (pc, rc) = (uv(&mut rng, &tuv, rect), uv_rect(&mut rng, &tuv, rect));
        let (xy, xc) = (ty.x(), tuv.x());
        let what = (ty.w, ty.h, f0, f1, py, ry, pc, rc, rescale);
        let sp = span(&mut rng, true);
        let b3 = vec3_scalar::new(bias[0], bias[1], bias[2]);
        if f1 == TextureFormat::RG16 {
            // The YUV2 stages on the first chunk, so a difference names its stage (AArch64 case 7, 2026-11-05).
            let (mut taps, mut raw, mut shifted, mut mat, mut px) = ([0i32; 16], [0i16; 12], [0i16; 12], [0i16; 48], [0u8; 16]);
            // SAFETY: the textures and float arrays outlive the call; the outputs have the sizes the shim writes.
            unsafe {
                o::x_yuv2_r16_stages(&xy, &xc, ri, py.as_ptr(), ry.as_ptr(), pc.as_ptr(), rc.as_ptr(), bias.as_ptr(), m.as_ptr(),
                    rescale, taps.as_mut_ptr(), raw.as_mut_ptr(), shifted.as_mut_ptr(), mat.as_mut_ptr(), px.as_mut_ptr())
            };
            let rust = if rect {
                yuv2_r16_stages(&ty.rect(), v2l(&py), &v4s(&ry), &tuv.rect(), v2l(&pc), &v4s(&rc), &b3, &mat3(&m), rescale)
            } else {
                yuv2_r16_stages(&ty.s2d(), v2l(&py), &v4s(&ry), &tuv.s2d(), v2l(&pc), &v4s(&rc), &b3, &mat3(&m), rescale)
            };
            let stage = if taps != rust.0 {
                Some(format!("taps (quantize, clamp): C++ {taps:?} Rust {:?}", rust.0))
            } else if raw != rust.1 {
                Some(format!("R16/RG16 samples: C++ {raw:?} Rust {:?}", rust.1))
            } else if shifted != rust.2 {
                Some(format!("rescale shift: C++ {shifted:?} Rust {:?}", rust.2))
            } else if mat != rust.3 {
                Some(format!("YUVMatrix::From [br, gg, yCoeffs, yBias, uvBias, br_yMask]: C++ {mat:?} Rust {:?}", rust.3))
            } else if px != rust.4 {
                Some(format!("YUVMatrix::convert: C++ {px:?} Rust {:?}", rust.4))
            } else {
                None
            };
            if let Some(st) = stage {
                panic!("YUV2 R16 stages case {case}: first differing stage {st}\n{what:?}");
            }
        }
        if with_color {
            check("commitTextureLinearColorYUV2", case, &sp, &what, &mut |st, b, l| unsafe {
                o::x_commitTextureLinearColorYUV2(st, b as _, l, &xy, &xc, ri, py.as_ptr(), ry.as_ptr(), pc.as_ptr(), rc.as_ptr(),
                    bias.as_ptr(), m.as_ptr(), rescale, c.as_ptr())
            }, &mut |ctx, fs| if rect {
                swgl_commitTextureLinearColorYUV2(ctx, ty.rect(), v2l(&py), v4s(&ry), tuv.rect(), v2l(&pc), v4s(&rc), b3, mat3(&m), rescale, v4s(&c), fs)
            } else {
                swgl_commitTextureLinearColorYUV2(ctx, ty.s2d(), v2l(&py), v4s(&ry), tuv.s2d(), v2l(&pc), v4s(&rc), b3, mat3(&m), rescale, v4s(&c), fs)
            });
        } else {
            check("commitTextureLinearYUV2", case, &sp, &what, &mut |st, b, l| unsafe {
                o::x_commitTextureLinearYUV2(st, b as _, l, &xy, &xc, ri, py.as_ptr(), ry.as_ptr(), pc.as_ptr(), rc.as_ptr(),
                    bias.as_ptr(), m.as_ptr(), rescale)
            }, &mut |ctx, fs| if rect {
                swgl_commitTextureLinearYUV2(ctx, ty.rect(), v2l(&py), v4s(&ry), tuv.rect(), v2l(&pc), v4s(&rc), b3, mat3(&m), rescale, fs)
            } else {
                swgl_commitTextureLinearYUV2(ctx, ty.s2d(), v2l(&py), v4s(&ry), tuv.s2d(), v2l(&pc), v4s(&rc), b3, mat3(&m), rescale, fs)
            });
        }

        // Three planes, R8 or R16, often in the shape CompositeYUV accepts (same U/V sizes and coordinates, one
        // row, stepping right).
        let f = if hdr { TextureFormat::R16 } else { TextureFormat::R8 };
        let ty = { let f_ = f; let g_ = filt; Tex::random_yuv(&mut rng, f_, g_) };
        // At least 2 texels wide, as the two-plane case (a 1-texel-wide linear plane is read from x = -1 in the C++).
        let (cw, ch) = ((ty.w / 2).max(2), (ty.h / 2).max(1));
        let tu = Tex::new(&mut rng, f, cw, ch, filt);
        let tv = if rng.chance(85) { Tex::new(&mut rng, f, cw, ch, filt) } else { { let f_ = f; let g_ = filt; Tex::random_yuv(&mut rng, f_, g_) } };
        let composite = rng.chance(70);
        let mut py = uv(&mut rng, &ty, rect);
        let ry = uv_rect(&mut rng, &ty, rect);
        let mut pu = uv(&mut rng, &tu, rect);
        let ru = uv_rect(&mut rng, &tu, rect);
        if composite {
            let step = { let a_ = [1.0, 0.5, 0.75, rng.range(0.2, 2.0)]; rng.pick(&a_) };
            let sx = if rect { 1.0 } else { 1.0 / ty.w as f32 };
            for k in 0..4 {
                py[k] = py[0] + step * k as f32 * sx;
                py[4 + k] = py[4];
                pu[k] = py[k] * 0.5;
                pu[4 + k] = pu[4];
            }
        }
        let pv = if composite || rng.chance(50) { pu } else { uv(&mut rng, &tv, rect) };
        let rv = if rng.chance(70) { ru } else { uv_rect(&mut rng, &tv, rect) };
        let (xy, xu, xv) = (ty.x(), tu.x(), tv.x());
        let what = (ty.w, ty.h, f, py, ry, pu, ru, pv, rv, rescale);
        let sp = span(&mut rng, true);
        if with_color {
            check("commitTextureLinearColorYUV3", case, &sp, &what, &mut |st, b, l| unsafe {
                o::x_commitTextureLinearColorYUV3(st, b as _, l, &xy, &xu, &xv, ri, py.as_ptr(), ry.as_ptr(), pu.as_ptr(), ru.as_ptr(),
                    pv.as_ptr(), rv.as_ptr(), bias.as_ptr(), m.as_ptr(), rescale, c.as_ptr())
            }, &mut |ctx, fs| if rect {
                swgl_commitTextureLinearColorYUV3(ctx, ty.rect(), v2l(&py), v4s(&ry), tu.rect(), v2l(&pu), v4s(&ru), tv.rect(), v2l(&pv),
                    v4s(&rv), b3, mat3(&m), rescale, v4s(&c), fs)
            } else {
                swgl_commitTextureLinearColorYUV3(ctx, ty.s2d(), v2l(&py), v4s(&ry), tu.s2d(), v2l(&pu), v4s(&ru), tv.s2d(), v2l(&pv),
                    v4s(&rv), b3, mat3(&m), rescale, v4s(&c), fs)
            });
        } else {
            check("commitTextureLinearYUV3", case, &sp, &what, &mut |st, b, l| unsafe {
                o::x_commitTextureLinearYUV3(st, b as _, l, &xy, &xu, &xv, ri, py.as_ptr(), ry.as_ptr(), pu.as_ptr(), ru.as_ptr(),
                    pv.as_ptr(), rv.as_ptr(), bias.as_ptr(), m.as_ptr(), rescale)
            }, &mut |ctx, fs| if rect {
                swgl_commitTextureLinearYUV3(ctx, ty.rect(), v2l(&py), v4s(&ry), tu.rect(), v2l(&pu), v4s(&ru), tv.rect(), v2l(&pv),
                    v4s(&rv), b3, mat3(&m), rescale, fs)
            } else {
                swgl_commitTextureLinearYUV3(ctx, ty.s2d(), v2l(&py), v4s(&ry), tu.s2d(), v2l(&pu), v4s(&ru), tv.s2d(), v2l(&pv),
                    v4s(&rv), b3, mat3(&m), rescale, fs)
            });
        }
    }
}

// ---- gradients ---------------------------------------------------------------------------------------------------

/// An RGBA32F texture holding a gradient table at `address` (floats): `entries` pairs of start color and step,
/// some consecutive ones mergeable (the same step).
fn gradient_table(rng: &mut Rng, entries: u32) -> (Tex, i32) {
    let w = (2 * entries + rng.int(0, 8) as u32).max(1);
    let h = rng.int(1, 3) as u32;
    let t = Tex::new(rng, TextureFormat::RGBA32F, w, h, TextureFilter::NEAREST);
    let y = rng.int(0, h as i32 - 1);
    let x = rng.int(0, (w - 2 * entries) as i32);
    let address = y * t.stride as i32 + x * 4;
    let mut t = t;
    let floats = unsafe { core::slice::from_raw_parts_mut(t.data.as_mut_ptr() as *mut f32, t.len / 4) };
    let mut step = [0.0f32; 4];
    for e in 0..entries as usize {
        let a = address as usize + 8 * e;
        if e == 0 || rng.chance(60) {
            for s in step.iter_mut() {
                *s = rng.range(-0.3, 0.3);
            }
        }
        for k in 0..4 {
            floats[a + k] = rng.unit();
            floats[a + 4 + k] = step[k];
        }
    }
    // Everything else finite too: tables only hold colors.
    for f in floats.iter_mut() {
        if !f.is_finite() || f.abs() > 4.0 {
            *f = rng.unit();
        }
    }
    (t, address)
}

/// An RGBA32F texture with `stops` colors and then their offsets (the 5-ary tree of findGradientStopPair, as
/// sorted values in [0, 1]).
fn stops_table(rng: &mut Rng, stops: u32) -> (Tex, i32, i32) {
    let off_texels = 8;
    let w = stops + 1 + off_texels + rng.int(0, 4) as u32;
    let h = rng.int(1, 2) as u32;
    let mut t = Tex::new(rng, TextureFormat::RGBA32F, w, h, TextureFilter::NEAREST);
    let y = rng.int(0, h as i32 - 1);
    let colors = y * t.stride as i32;
    let offsets = colors + 4 * (stops as i32 + 1);
    let floats = unsafe { core::slice::from_raw_parts_mut(t.data.as_mut_ptr() as *mut f32, t.len / 4) };
    for f in floats.iter_mut() {
        *f = rng.unit();
    }
    let mut offs: Vec<f32> = (0..off_texels * 4).map(|_| rng.unit()).collect();
    offs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if rng.chance(50) {
        offs[0] = 0.0;
    }
    for (k, v) in offs.iter().enumerate() {
        floats[offsets as usize + k] = *v;
    }
    (t, colors, offsets)
}

fn grad_pos(rng: &mut Rng, scale: f32) -> [f32; 8] {
    let x0 = rng.range(-1.0, 2.0) * scale;
    let y0 = rng.range(-1.0, 2.0) * scale;
    let dx = { let a_ = [0.01, 0.003, -0.02, rng.range(-0.1, 0.1), 0.0]; rng.pick(&a_) } * scale;
    let dy = { let a_ = [0.0, 0.0, 0.01, rng.range(-0.05, 0.05)]; rng.pick(&a_) } * scale;
    let mut p = [0.0; 8];
    for k in 0..4 {
        p[k] = x0 + dx * k as f32;
        p[4 + k] = y0 + dy * k as f32;
    }
    p
}

#[test]
fn gradients() {
    let mut rng = Rng::new("gradients");
    for case in 0..N {
        let size = rng.int(1, 30) as u32;
        if std::env::var_os("XTRACE").is_some() {
            eprintln!("gradients case {case}");
        }
        let (t, address) = gradient_table(&mut rng, size + 2);
        let x = t.x();
        let mut entry = [0.0f32; 4];
        for e in entry.iter_mut() {
            *e = rng.range(0.0, size as f32 + 1.0);
        }
        let sp = span(&mut rng, true);
        check("commitGradientRGBA8", case, &sp, &entry, &mut |st, b, l| unsafe {
            o::x_commitGradientRGBA8(st, b as _, l, &x, address, entry.as_ptr())
        }, &mut |ctx, fs| swgl_commitGradientRGBA8(ctx, t.s2d(), address, lanes(&entry), fs));

        let (tr, gr) = (rng.chance(30), rng.chance(30));
        let pos = grad_pos(&mut rng, 1.0);
        let dir = [rng.range(-2.0, 2.0), rng.range(-2.0, 2.0)];
        let so = rng.range(-0.5, 0.5);
        let sz = size as f32;
        let what = (size, tr, gr, pos, dir, so);
        for dither in [false, true] {
            let sp = span(&mut rng, true);
            check(if dither { "commitDitheredLinearGradientRGBA8" } else { "commitLinearGradientRGBA8" }, case, &sp, &what,
                &mut |st, b, l| unsafe {
                    if dither {
                        o::x_commitDitheredLinearGradientRGBA8(st, b as _, l, &x, address, sz, tr as i32, gr as i32, pos.as_ptr(), dir.as_ptr(), so)
                    } else {
                        o::x_commitLinearGradientRGBA8(st, b as _, l, &x, address, sz, tr as i32, gr as i32, pos.as_ptr(), dir.as_ptr(), so)
                    }
                },
                &mut |ctx, fs| if dither {
                    swgl_commitDitheredLinearGradientRGBA8(ctx, t.s2d(), address, sz, tr, gr, v2l(&pos), v2s(&dir), so, fs)
                } else {
                    swgl_commitLinearGradientRGBA8(ctx, t.s2d(), address, sz, tr, gr, v2l(&pos), v2s(&dir), so, fs)
                });
        }

        let rep = rng.chance(30);
        let rpos = grad_pos(&mut rng, 2.0);
        let radius = { let a_ = [0.0, 0.25, rng.range(0.0, 2.0)]; rng.pick(&a_) };
        let what = (size, rep, rpos, radius);
        for dither in [false, true] {
            let sp = span(&mut rng, true);
            check(if dither { "commitDitheredRadialGradientRGBA8" } else { "commitRadialGradientRGBA8" }, case, &sp, &what,
                &mut |st, b, l| unsafe {
                    if dither {
                        o::x_commitDitheredRadialGradientRGBA8(st, b as _, l, &x, address, sz, rep as i32, rpos.as_ptr(), radius)
                    } else {
                        o::x_commitRadialGradientRGBA8(st, b as _, l, &x, address, sz, rep as i32, rpos.as_ptr(), radius)
                    }
                },
                &mut |ctx, fs| if dither {
                    swgl_commitDitheredRadialGradientRGBA8(ctx, t.s2d(), address, sz, rep, v2l(&rpos), radius, fs)
                } else {
                    swgl_commitRadialGradientRGBA8(ctx, t.s2d(), address, sz, rep, v2l(&rpos), radius, fs)
                });
        }

        // From stops.
        let n = rng.int(2, 20) as u32;
        let (ts, colors, offsets) = stops_table(&mut rng, n);
        let xs = ts.x();
        let nf = n as f32;
        let what = (n, gr, pos, dir, so);
        if case == 18 && std::env::var_os("XDUMP").is_some() {
            std::fs::write("/tmp/xd_tex.bin", unsafe { core::slice::from_raw_parts(ts.data.as_ptr() as *const u8, ts.data.len() * 4) }).unwrap();
            eprintln!("DUMP w {} h {} stride {} colors {colors} offsets {offsets} n {n} gr {gr} pos {:?} dir {:?} so {so:?}", ts.w, ts.h, ts.stride, pos, dir);
        }
        for dither in [false, true] {
            let sp = span(&mut rng, true);
            check(if dither { "commitDitheredLinearGradientFromStopsRGBA8" } else { "commitLinearGradientFromStopsRGBA8" }, case,
                &sp, &what,
                &mut |st, b, l| unsafe {
                    if dither {
                        o::x_commitDitheredLinearGradientFromStopsRGBA8(st, b as _, l, &xs, offsets, colors, nf, gr as i32, pos.as_ptr(), dir.as_ptr(), so)
                    } else {
                        o::x_commitLinearGradientFromStopsRGBA8(st, b as _, l, &xs, offsets, colors, nf, gr as i32, pos.as_ptr(), dir.as_ptr(), so)
                    }
                },
                &mut |ctx, fs| if dither {
                    swgl_commitDitheredLinearGradientFromStopsRGBA8(ctx, ts.s2d(), offsets, colors, nf, false, gr, v2l(&pos), v2s(&dir), so, fs)
                } else {
                    swgl_commitLinearGradientFromStopsRGBA8(ctx, ts.s2d(), offsets, colors, nf, gr, v2l(&pos), v2s(&dir), so, fs)
                });
            let sp = span(&mut rng, true);
            let what = (n, rep, rpos, radius);
            check(if dither { "commitDitheredRadialGradientFromStopsRGBA8" } else { "commitRadialGradientFromStopsRGBA8" }, case,
                &sp, &what,
                &mut |st, b, l| unsafe {
                    if dither {
                        o::x_commitDitheredRadialGradientFromStopsRGBA8(st, b as _, l, &xs, offsets, colors, nf, rep as i32, rpos.as_ptr(), radius)
                    } else {
                        o::x_commitRadialGradientFromStopsRGBA8(st, b as _, l, &xs, offsets, colors, nf, rep as i32, rpos.as_ptr(), radius)
                    }
                },
                &mut |ctx, fs| if dither {
                    swgl_commitDitheredRadialGradientFromStopsRGBA8(ctx, ts.s2d(), offsets, colors, nf, rep, v2l(&rpos), radius, fs)
                } else {
                    swgl_commitRadialGradientFromStopsRGBA8(ctx, ts.s2d(), offsets, colors, nf, rep, v2l(&rpos), radius, fs)
                });
        }
    }
}

// ---- the intrinsics without a span -------------------------------------------------------------------------------

#[test]
fn validate_and_state() {
    let mut rng = Rng::new("state");
    let _guard = lock();
    for case in 0..N * 5 {
        let fmt = if rng.chance(80) { TextureFormat::RGBA32F } else { TextureFormat::RGBA8 };
        let t = { let f_ = fmt; let g_ = filter(&mut rng); Tex::random(&mut rng, f_, g_) };
        let x = t.x();
        let (ax, ay, e) = (rng.int(-3, t.w as i32 + 2), rng.int(-2, t.h as i32 + 1), rng.int(-2, 30));
        let a = ivec2_scalar::new(ax, ay);
        assert_eq!(unsafe { o::x_validateGradient(&x, ax, ay, e) }, swgl_validateGradient(t.s2d(), a, e), "validateGradient {case}");
        assert_eq!(unsafe { o::x_validateGradientFromStops(&x, ax, ay, e) }, swgl_validateGradientFromStops(t.s2d(), a, e),
            "validateGradientFromStops {case}");
        for rect in [false, true] {
            let ri = rect as i32;
            let r = |which| unsafe { o::x_isTexture(&x, ri, which) != 0 };
            let (l, c, e8) = sampled!(rect, t, |s| (swgl_isTextureLinear(s), swgl_isTextureRGBA8(s), swgl_isTextureR8(s)));
            assert_eq!((r(0), r(1), r(2)), (l, c, e8), "isTexture {case}");
        }

        let flags = rng.int(0, 7);
        let off = [rng.range(-50.0, 50.0), rng.range(-50.0, 50.0)];
        let origin = [rng.range(-10.0, 50.0), rng.range(-10.0, 50.0)];
        let size = if rng.chance(30) { [0.0, 0.0] } else { [{ let a_ = [0.0, rng.range(0.0, 60.0)]; rng.pick(&a_) }, rng.range(0.0, 60.0)] };
        let mut out = o::XClip::default();
        unsafe { o::x_clipMask(flags, &x, off.as_ptr(), origin.as_ptr(), size.as_ptr(), &mut out) };
        let mut ctx = RasterState::default();
        ctx.swgl_ClipFlags = flags;
        swgl_clipMask(&mut ctx, t.s2d(), v2s(&off), v2s(&origin), v2s(&size));
        let b = ctx.swgl_ClipMaskBounds;
        let mine = o::XClip {
            clip_flags: ctx.swgl_ClipFlags,
            offset: [ctx.swgl_ClipMaskOffset.x, ctx.swgl_ClipMaskOffset.y],
            bounds: [b.x0, b.y0, b.x1, b.y1],
            mask_buf: if size[0] != 0.0 || size[1] != 0.0 { ctx.swgl_ClipMask.common.buf() as usize } else { 0 },
        };
        assert_eq!(out, mine, "clipMask {case}");

        let kind = rng.int(0, 2);
        let edges = rng.int(0, 15);
        let (mut of, mut om) = (0, 0);
        unsafe { o::x_antiAlias(flags, kind, edges, &mut of, &mut om) };
        let mut ctx = RasterState::default();
        ctx.swgl_ClipFlags = flags;
        match kind {
            0 => swgl_antiAlias(&mut ctx, edges),
            1 => swgl_antiAlias(&mut ctx, edges != 0),
            _ => swgl_antiAlias(&mut ctx, bvec4_scalar::new(edges & 1 != 0, edges & 2 != 0, edges & 4 != 0, edges & 8 != 0)),
        }
        assert_eq!((of, om), (ctx.swgl_ClipFlags, ctx.swgl_AAEdgeMask), "antiAlias {case}");

        let c = color(&mut rng);
        for sub in [false, true] {
            let mut out = o::XOverride::default();
            unsafe {
                if sub {
                    o::x_blendSubpixelText(flags, c.as_ptr(), &mut out)
                } else {
                    o::x_blendDropShadow(flags, c.as_ptr(), &mut out)
                }
            };
            let mut ctx = RasterState::default();
            ctx.swgl_ClipFlags = flags;
            if sub {
                swgl_blendSubpixelText(&mut ctx, v4s(&c));
            } else {
                swgl_blendDropShadow(&mut ctx, v4s(&c));
            }
            let mine = o::XOverride {
                clip_flags: ctx.swgl_ClipFlags,
                blend_override: ctx.swgl_BlendOverride as u32,
                color: ctx.swgl_BlendColorRGBA8.0,
                alpha: ctx.swgl_BlendAlphaRGBA8.0,
            };
            assert_eq!(out, mine, "blend override {sub} {case}");
        }
    }
}

// ---- scaled YUV image path (flaw a tracked flaw: vertical colour bars, aarch64 macOS) --------------------------------

/// A packed R8 plane `w x h` with rows `pad` bytes past `w` (a decoder's rows are rarely a multiple of 4: they are
/// whatever its own alignment gives them), after the 16-byte `0xA5` guard `Texels::new` prepends, so a read before a
/// plane is the same on every target.
fn plane_r8(w: u32, h: u32, pad: u32, fill: impl Fn(u32, u32) -> u8) -> Tex {
    let stride_bytes = w.div_ceil(4) * 4 + pad;
    let len = (stride_bytes * h) as usize;
    let mut data = Texels::new(len / 4 + 16);
    // SAFETY: `len` bytes from the post-guard address are inside the vector (`(len/4 + 16) * 4 >= len`).
    let bytes = unsafe { core::slice::from_raw_parts_mut(data.as_mut_ptr() as *mut u8, len) };
    for y in 0..h {
        for x in 0..w {
            bytes[(y * stride_bytes + x) as usize] = fill(x, y);
        }
    }
    let stride = stride_bytes;
    Tex { data, len, w, h, stride, format: TextureFormat::R8, filter: TextureFilter::LINEAR }
}

/// The I420 planes of a recognisable picture: a diagonal gradient overlaid with a coarse checkerboard, all three
/// planes varying in both x and y (a vertical-bar bug shows up as a plane that no longer varies down a column).
/// Every row of every plane carries `pad` bytes of slack.
fn i420_planes(w: u32, h: u32, pad: u32) -> (Tex, Tex, Tex) {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let y = plane_r8(w, h, pad, |x, y| {
        let base = (8u32.wrapping_mul(x).wrapping_add(11 * y)) as u8;
        let check = if ((x >> 4) + (y >> 4)) & 1 == 1 { 40 } else { 0 };
        base.wrapping_add(check)
    });
    let u = plane_r8(cw, ch, pad, |x, y| (6 * x).wrapping_add(5 * y) as u8);
    let v = plane_r8(cw, ch, pad, |x, y| (3 * x).wrapping_add(9 * y) as u8);
    (y, u, v)
}

/// One no-blend raster state for a row of `len` RGBA8 pixels into `buf`.
fn row_state(buf: *mut u32, len: i32) -> (RasterState, FragmentShaderState) {
    let mut ctx = RasterState::default();
    ctx.blend_key = 0; // BLEND_KEY_NONE: the plain (unblended) commit both sides take.
    ctx.swgl_ClipFlags = 0;
    ctx.swgl_SpanBuf = buf as *mut u8;
    ctx.swgl_OpaqueStart = buf as *mut u8;
    ctx.swgl_OpaqueSize = 0;
    let mut fs = FragmentShaderState::default();
    fs.swgl_OutRGBA8 = OutPtr(buf);
    fs.swgl_SpanLength = len;
    (ctx, fs)
}

/// Render `sw x sh` -> `dw x dh` through the three-plane R8 image path (`swgl_commitTextureLinearYUV3`, which for a
/// scale != 1 forces swgl's `blendYUVFallback` -> `sampleYUV3` -> `textureLinearUnpackedR8` per plane: the kernel
/// suspect #1 convicts) over `sampler2D` planes, or the `sampler2DRect` planes the compositor-surface path gives.
/// Returns the Rust output as `dh` rows of `dw` RGBA pixels.
///
/// Compares every row against swgl's C++ oracle on the same bytes and asserts no output column is constant down its
/// height (the vertical-bar signature): a correct scaled render of a picture that varies in y varies down every
/// column. `SCALED_DUMP` prints an FNV hash of the whole Rust image, to diff aarch64 against x86.
fn render_scaled_yuv(sw: u32, sh: u32, dw: u32, dh: u32, rect_sampler: bool, pad: u32) -> Vec<u32> {
    let (ty, tu, tv) = i420_planes(sw, sh, pad);
    // A YUV->RGB matrix with every channel coupled to Y, U and V, so both chroma planes reach every output channel
    // and the render varies down y; the exact coefficients are irrelevant to Rust-vs-C++ equality (identical inputs).
    let bias = [0.0f32, 0.5, 0.5];
    let m = [1.0f32, 0.5, 0.5, 1.0, 0.5, -0.5, 1.0, -0.5, 0.5];
    let b3 = vec3_scalar::new(bias[0], bias[1], bias[2]);
    let (xy, xu, xv) = (ty.x(), tu.x(), tv.x());
    let slack = dw as usize + 8;
    let mut out = vec![0u32; dw as usize * dh as usize];
    let mut constant_columns = 0usize;

    // One destination row of a plane `pw x ph`: the 4 starting pixels' coords (u steps by one destination column, v
    // is fixed down the chunk) and the sampling rect, in texel centres as the shaders give them, normalised for a
    // `sampler2D`.
    let row_args = |pw: u32, ph: u32, row: u32| -> ([f32; 8], [f32; 4]) {
        let mut p = [0.0f32; 8];
        for k in 0..4 {
            p[k] = (k as f32 + 0.5) * pw as f32 / dw as f32;
            p[4 + k] = (row as f32 + 0.5) * ph as f32 / dh as f32;
        }
        let mut r = [0.5, 0.5, pw as f32 - 0.5, ph as f32 - 0.5];
        if !rect_sampler {
            for k in 0..4 {
                p[k] /= pw as f32;
                p[4 + k] /= ph as f32;
            }
            for (i, s) in r.iter_mut().zip([pw as f32, ph as f32, pw as f32, ph as f32]) {
                *i /= s;
            }
        }
        (p, r)
    };

    for row in 0..dh {
        let (py, ry) = row_args(ty.w, ty.h, row);
        let (pu, ru) = row_args(tu.w, tu.h, row);
        let (pv, rv) = row_args(tv.w, tv.h, row);

        // C++ oracle row.
        let mut a = vec![0u32; slack];
        let st = o::XState {
            blend_key: 0,
            clip_flags: 0,
            blend_color: [0; 16],
            blend_alpha: [0; 16],
            blendcolor: [0; 8],
            clip_mask_buf: core::ptr::null(),
            opaque_start: 0,
            opaque_size: 0,
            left_aa_dist: [0.0; 4],
            right_aa_dist: [0.0; 4],
            aa_slope: [0.0; 4],
            frag_coord: [0.0; 16],
            secondary: [0.0; 16],
        };
        // SAFETY: the textures and float arrays outlive the call; `a` has slack pixels for the chunk overrun.
        unsafe {
            o::x_commitTextureLinearYUV3(
                &st, a.as_mut_ptr(), dw as i32, &xy, &xu, &xv, rect_sampler as i32, py.as_ptr(), ry.as_ptr(),
                pu.as_ptr(), ru.as_ptr(), pv.as_ptr(), rv.as_ptr(), bias.as_ptr(), m.as_ptr(), 0,
            )
        };

        // Rust row (the image path), collected into the output.
        let mut b = vec![0u32; slack];
        {
            let (mut ctx, mut fs) = row_state(b.as_mut_ptr(), dw as i32);
            if rect_sampler {
                swgl_commitTextureLinearYUV3(
                    &mut ctx, ty.rect(), v2l(&py), v4s(&ry), tu.rect(), v2l(&pu), v4s(&ru), tv.rect(), v2l(&pv),
                    v4s(&rv), b3, mat3(&m), 0, &mut fs,
                );
            } else {
                swgl_commitTextureLinearYUV3(
                    &mut ctx, ty.s2d(), v2l(&py), v4s(&ry), tu.s2d(), v2l(&pu), v4s(&ru), tv.s2d(), v2l(&pv),
                    v4s(&rv), b3, mat3(&m), 0, &mut fs,
                );
            }
        }

        // Byte-exact agreement with the C++ oracle over the requested pixels (the whole chunk overrun included).
        if a[..slack] != b[..slack] {
            let first = a.iter().zip(&b).position(|(x, y)| x != y).unwrap();
            panic!(
                "scaled YUV3 {sw}x{sh}->{dw}x{dh} {} stride {} row {row}: Rust != C++ oracle at pixel {first} \
                 (C++ {:#010x}, Rust {:#010x})",
                if rect_sampler { "rect" } else { "2D" },
                ty.stride,
                a[first],
                b[first]
            );
        }
        out[row as usize * dw as usize..(row as usize + 1) * dw as usize].copy_from_slice(&b[..dw as usize]);
    }

    // Column-variation: count columns constant down their full height. A correct render of a y-varying picture has
    // none; a vertical-bar bug makes the output a function of x only, i.e. every column constant.
    for x in 0..dw as usize {
        let first = out[x];
        if (1..dh as usize).all(|r| out[r * dw as usize + x] == first) {
            constant_columns += 1;
        }
    }

    if std::env::var_os("SCALED_DUMP").is_some() {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &px in &out {
            h = (h ^ px as u64).wrapping_mul(0x100_0000_01b3);
        }
        println!(
            "SCALED {sw}x{sh}->{dw}x{dh} {} stride {} rust_hash={h:016x} constant_columns={constant_columns}/{dw}",
            if rect_sampler { "rect" } else { "2D" },
            ty.stride
        );
    }

    assert_eq!(
        constant_columns, 0,
        "scaled YUV3 {sw}x{sh}->{dw}x{dh} {} stride {}: {constant_columns} of {dw} output columns are constant down \
         their height - vertical colour bars (each column the same for every row), the a tracked flaw symptom",
        if rect_sampler { "rect" } else { "2D" },
        ty.stride
    );
    out
}

#[test]
fn texture_yuv_scaled_image_path() {
    let _g = lock();
    // Every scaled case is anisotropic: up in one axis and down in the other (960x270 from 640x360 downscales
    // vertically, as the reported YouTube box does; 1920x1080 from 640x360 upscales both ways). 942 is not a
    // multiple of 4, so a row's last chunk runs past the span. The 1:1 rect case is the control: what
    // CompositeYUV draws, which the report says was already correct.
    let cases = [
        (320u32, 240u32, 960u32, 270u32),
        (320, 240, 942, 707),
        (640, 360, 960, 270),
        (640, 360, 942, 707),
        (1280, 720, 960, 270),
        (640, 360, 1920, 1080),
    ];
    for (sw, sh, dw, dh) in cases {
        render_scaled_yuv(sw, sh, dw, dh, false, 0);
        render_scaled_yuv(sw, sh, dw, dh, true, 0);
    }
    render_scaled_yuv(640, 360, 640, 360, true, 0);
}

/// The same image path over planes whose rows carry slack, as a decoder's do: a row stride that is not a multiple of
/// 4 moves every row's start off the boundary the aligned planes keep, which is what the NEON load of a scaled tap
/// sees.
#[test]
fn texture_yuv_scaled_image_path_padded_strides() {
    let _g = lock();
    for (sw, sh, dw, dh) in [(640u32, 360u32, 960u32, 270u32), (640, 360, 942, 707)] {
        for pad in [1u32, 2, 3, 6] {
            render_scaled_yuv(sw, sh, dw, dh, false, pad);
            render_scaled_yuv(sw, sh, dw, dh, true, pad);
        }
    }
}
