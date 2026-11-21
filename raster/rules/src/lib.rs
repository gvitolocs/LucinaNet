// The pure arithmetic of the Rust rasterizer: rectangles cut to a texture, the 32-row stripes that threads own, where a
// texel lives in a buffer, the blend-mode table and the 8-bit blend arithmetic. Verus checks this very file:
// `//@` lines hold the proof, see raster/verify.sh. The original for each function is named on it; swgl is in ../../swgl.
// What it does not cover: that a caller's buffer really is `stride * height` bytes long. That is the allocator's, and
// `texture_size` below is what it must allocate.
#![no_std] //@omit
// The code is the subset Verus reads: proof lines (`//@`) between a doc comment and its item, plain `%` and `x = x + y`
// as the proofs state them, the argument lists of the C++ originals.
#![allow(clippy::needless_return)] //@omit
#![allow(clippy::empty_line_after_doc_comments, clippy::assign_op_pattern, clippy::manual_rem_euclid)] //@omit
#![allow(clippy::too_many_arguments, clippy::manual_unwrap_or)] //@omit

pub mod span; //@omit

//@ global size_of usize == 8;

/// Scaled blits (composite.h): which destination pixels read inside the source. Verified as its own file.
pub mod blit; //@omit

/// swgl's `GL_MAX_TEXTURE_SIZE` (gl.cc, `GetIntegerv`, `1 << 15`): no texture side is larger.
pub const MAX_SIDE: i64 = 32768;
/// Bytes in the largest texel swgl knows (`GL_RGBA32F`, `GL_RGBA32I`).
pub const MAX_BPP: i64 = 16;
/// The largest row: `aligned_stride(MAX_SIDE * MAX_BPP)`.
pub const MAX_STRIDE: i64 = 524288;
/// Rows in a stripe. The delayed-clear bit vector packs 32 rows to a word, so a stripe owns whole words.
pub const STRIPE: i64 = 32;
/// Most threads that share one target.
pub const MAX_THREADS: i64 = 64;

// ---------------------------------------------------------------------------------------------------------------------
// Rectangles

/// `IntRect::intersection(...)` then `is_empty()` (gl.cc), cut to a `w` by `h` texture placed at `(ox, oy)`
/// (`Texture::offset_bounds`), in texture coordinates. `None` when nothing is left. Unlike the C++, no `int` arithmetic
/// can overflow: coordinates are widened first.
pub fn clip_to_texture(x0: i32, y0: i32, x1: i32, y1: i32, ox: i32, oy: i32, w: i64, h: i64) -> Option<(i64, i64, i64, i64)> //@ret
//@ requires 0 <= w <= MAX_SIDE, 0 <= h <= MAX_SIDE,
//@ ensures match ret {
//@     Some(r) => 0 <= r.0 < r.2 <= w && 0 <= r.1 < r.3 <= h
//@         && r.0 >= x0 - ox && r.2 <= x1 - ox && r.1 >= y0 - oy && r.3 <= y1 - oy,
//@     None => w == 0 || h == 0 || x1 - ox <= 0 || y1 - oy <= 0 || x0 - ox >= w || y0 - oy >= h || x0 >= x1 || y0 >= y1,
//@ },
{
    let ax0 = x0 as i64 - ox as i64;
    let ay0 = y0 as i64 - oy as i64;
    let ax1 = x1 as i64 - ox as i64;
    let ay1 = y1 as i64 - oy as i64;
    let cx0 = if ax0 > 0 { ax0 } else { 0 };
    let cy0 = if ay0 > 0 { ay0 } else { 0 };
    let cx1 = if ax1 < w { ax1 } else { w };
    let cy1 = if ay1 < h { ay1 } else { h };
    if cx0 >= cx1 || cy0 >= cy1 {
        return None;
    }
    Some((cx0, cy0, cx1, cy1))
}

// ---------------------------------------------------------------------------------------------------------------------
// Stripes: which thread rasterizes which rows (raster/DESIGN.md, "Parallel rasterizing"). New; swgl has one thread.

/// The thread that owns row `y` when `threads` threads share a target.
pub fn stripe_owner(y: i64, threads: i64) -> i64 //@ret
//@ requires 0 <= y <= MAX_SIDE, 1 <= threads <= MAX_THREADS,
//@ ensures 0 <= ret < threads, ret as int == (y as int / 32) % (threads as int),
{
    let s = y / STRIPE;
    //@ assert(s as int == y as int / 32);
    //@ assert(0 <= s);
    //@ proof { vstd::arithmetic::div_mod::lemma_mod_pos_bound(s as int, threads as int); }
    // Unsigned, so that `%` is the remainder of a division of non-negative numbers in Verus too.
    ((s as u64) % (threads as u64)) as i64
}

/// The `k`-th stripe of thread `t`, cut to rows `y0..y1`: `None` when it lies outside them.
pub fn stripe_rows(t: i64, threads: i64, k: i64, y0: i64, y1: i64) -> Option<(i64, i64)> //@ret
//@ requires 1 <= threads <= MAX_THREADS, 0 <= t < threads, 0 <= k <= MAX_SIDE, 0 <= y0 <= y1 <= MAX_SIDE,
//@ ensures match ret {
//@     Some(r) => y0 <= r.0 < r.1 <= y1 && r.1 - r.0 <= 32
//@         && r.0 as int / 32 == k * threads + t && (r.1 as int - 1) / 32 == k * threads + t
//@         && (r.0 as int / 32) % (threads as int) == t as int,
//@     None => true,
//@ },
{
    //@ assert(0 <= k * threads <= MAX_SIDE * MAX_THREADS) by (nonlinear_arith) requires 0 <= k <= MAX_SIDE, 1 <= threads <= MAX_THREADS;
    let s = k * threads + t;
    let a0 = s * STRIPE;
    let a1 = a0 + STRIPE;
    let r0 = if a0 > y0 { a0 } else { y0 };
    let r1 = if a1 < y1 { a1 } else { y1 };
    if r0 >= r1 {
        return None;
    }
    //@ assert(r0 as int / 32 == s as int);
    //@ assert((r1 as int - 1) / 32 == s as int);
    //@ proof { vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(s as int, threads as int, k as int, t as int); }
    Some((r0, r1))
}

/// Rows of stripe `s` (rows `32 * s..32 * s + 32`) within `y0..y1`: `None` when they do not meet. The claim path of a
/// split draw: threads take stripe indices from one counter, so each stripe, and so each row, is drawn by one thread.
pub fn stripe_span(s: i64, y0: i64, y1: i64) -> Option<(i64, i64)> //@ret
//@ requires 0 <= s <= MAX_SIDE, 0 <= y0 <= y1 <= MAX_SIDE,
//@ ensures match ret {
//@     Some(r) => y0 <= r.0 < r.1 <= y1 && r.1 - r.0 <= 32
//@         && r.0 as int / 32 == s && (r.1 as int - 1) / 32 == s
//@         && r.0 == (if y0 > s * 32 { y0 as int } else { s * 32 }) && r.1 == (if y1 < s * 32 + 32 { y1 as int } else { s * 32 + 32 }),
//@     None => (if y0 > s * 32 { y0 as int } else { s * 32 }) >= (if y1 < s * 32 + 32 { y1 as int } else { s * 32 + 32 }),
//@ },
{
    let top = s * STRIPE;
    let bottom = top + STRIPE;
    let lo = if top > y0 { top } else { y0 };
    let hi = if bottom < y1 { bottom } else { y1 };
    if lo >= hi {
        return None;
    }
    //@ assert(lo as int / 32 == s as int);
    //@ assert((hi as int - 1) / 32 == s as int);
    Some((lo, hi))
}

//@ /// Every row `y` lies in exactly one stripe: `y` is in rows `32 * s..32 * s + 32` exactly when `s == y / 32`, so
//@ /// `stripe_span(s, y0, y1)` holds a row `y` of `y0..y1` for that one `s` and for no other.
//@ proof fn every_row_in_one_stripe_span(y: int, s: int)
//@     requires 0 <= y, 0 <= s,
//@     ensures (s * 32 <= y < s * 32 + 32) == (s == y / 32),
//@ {
//@     vstd::arithmetic::div_mod::lemma_fundamental_div_mod(y, 32);
//@     vstd::arithmetic::div_mod::lemma_mod_pos_bound(y, 32);
//@     if s * 32 <= y && y < s * 32 + 32 {
//@         vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(y, 32, s, y - s * 32);
//@     }
//@ }

//@ /// The stripes of all threads cover each row once: row `y` is in stripe `k` of thread `t` for exactly the `k` and `t`
//@ /// below, and `stripe_rows(t, threads, k, y0, y1)` returns the rows of that stripe within `y0..y1`, `y` among them.
//@ proof fn every_row_has_one_stripe(y: i64, threads: i64)
//@     requires 1 <= threads <= MAX_THREADS, 0 <= y < MAX_SIDE,
//@     ensures ({
//@         let s = y as int / 32;
//@         let t = s % (threads as int);
//@         let k = s / (threads as int);
//@         0 <= t < threads && 0 <= k <= MAX_SIDE && k * threads + t == s && s * 32 <= y < s * 32 + 32
//@     }),
//@ {
//@     let s = y as int / 32;
//@     vstd::arithmetic::div_mod::lemma_fundamental_div_mod(s, threads as int);
//@     vstd::arithmetic::div_mod::lemma_mod_pos_bound(s, threads as int);
//@     vstd::arithmetic::div_mod::lemma_div_pos_is_pos(s, threads as int);
//@     vstd::arithmetic::div_mod::lemma_div_is_ordered_by_denominator(s, 1, threads as int);
//@ }

// ---------------------------------------------------------------------------------------------------------------------
// Buffers

/// `aligned_stride` (gl.cc): a row of `row_bytes` padded to a multiple of 4.
pub fn aligned_stride(row_bytes: i64) -> i64 //@ret
//@ requires 0 <= row_bytes <= MAX_SIDE * MAX_BPP,
//@ ensures row_bytes <= ret < row_bytes + 4, ret % 4 == 0, ret == (row_bytes + 3) / 4 * 4, ret <= MAX_STRIDE,
{
    (row_bytes + 3) / 4 * 4
}

/// What `Texture::allocate` (gl.cc) allocates for a `w` by `h` texture of `bpp`-byte texels that may grow to
/// `min_w` by `min_h`, without the SIMD padding: `(stride, size)`.
pub fn texture_size(bpp: i64, w: i64, h: i64, min_w: i64, min_h: i64) -> (i64, i64) //@ret
//@ requires 1 <= bpp <= MAX_BPP, 0 <= w <= MAX_SIDE, 0 <= h <= MAX_SIDE, 0 <= min_w <= MAX_SIDE, 0 <= min_h <= MAX_SIDE,
//@ ensures bpp * w <= ret.0 <= MAX_STRIDE, ret.0 % 4 == 0, ret.0 * h <= ret.1,
{
    //@ assert(0 <= bpp * w <= MAX_SIDE * MAX_BPP) by (nonlinear_arith) requires 1 <= bpp <= MAX_BPP, 0 <= w <= MAX_SIDE;
    let stride = aligned_stride(bpp * w);
    let mw = if w > min_w { w } else { min_w };
    let mh = if h > min_h { h } else { min_h };
    //@ assert(bpp * w <= bpp * mw <= MAX_SIDE * MAX_BPP) by (nonlinear_arith) requires 1 <= bpp <= MAX_BPP, 0 <= w <= mw <= MAX_SIDE;
    let max_stride = aligned_stride(bpp * mw);
    //@ assert(stride <= max_stride);
    //@ assert(0 <= max_stride * mh <= MAX_STRIDE * MAX_SIDE) by (nonlinear_arith) requires 0 <= max_stride <= MAX_STRIDE, 0 <= mh <= MAX_SIDE;
    //@ assert(stride * h <= max_stride * mh) by (nonlinear_arith) requires 0 <= stride <= max_stride, 0 <= h <= mh;
    (stride, max_stride * mh)
}

/// `Texture::sample_ptr(x, y)` (gl.cc) as a byte offset: the texel is `buf[off..off + bpp]`.
pub fn texel_offset(x: i64, y: i64, bpp: i64, stride: i64, w: i64, h: i64) -> Option<i64> //@ret
//@ requires 1 <= bpp <= MAX_BPP, 0 <= w <= MAX_SIDE, 0 <= h <= MAX_SIDE, bpp * w <= stride <= MAX_STRIDE,
//@ ensures match ret { Some(off) => 0 <= off && off + bpp <= stride * h, None => !(0 <= x < w && 0 <= y < h) },
{
    if x < 0 || y < 0 || x >= w || y >= h {
        return None;
    }
    //@ assert(0 <= y * stride <= (h - 1) * stride) by (nonlinear_arith) requires 0 <= y <= h - 1, 0 <= stride;
    //@ assert((h - 1) * stride <= MAX_SIDE * MAX_STRIDE) by (nonlinear_arith) requires 0 <= h - 1 < MAX_SIDE, 0 <= stride <= MAX_STRIDE;
    //@ assert(0 <= x * bpp && x * bpp + bpp <= w * bpp) by (nonlinear_arith) requires 0 <= x < w, 1 <= bpp;
    //@ assert((h - 1) * stride + stride == h * stride) by (nonlinear_arith);
    Some(y * stride + x * bpp)
}

/// One row of a clipped span, `[x0, x1)` of row `y`, as a byte range of the buffer.
pub fn row_span(x0: i64, x1: i64, y: i64, bpp: i64, stride: i64, w: i64, h: i64) -> Option<(i64, i64)> //@ret
//@ requires 1 <= bpp <= MAX_BPP, 0 <= w <= MAX_SIDE, 0 <= h <= MAX_SIDE, bpp * w <= stride <= MAX_STRIDE,
//@ ensures match ret { Some(r) => 0 <= r.0 < r.1 <= stride * h && r.1 - r.0 == (x1 - x0) * bpp, None => true },
{
    if x0 < 0 || x1 > w || x0 >= x1 || y < 0 || y >= h {
        return None;
    }
    //@ assert(0 <= y * stride <= (h - 1) * stride) by (nonlinear_arith) requires 0 <= y <= h - 1, 0 <= stride;
    //@ assert((h - 1) * stride <= MAX_SIDE * MAX_STRIDE) by (nonlinear_arith) requires 0 <= h - 1 < MAX_SIDE, 0 <= stride <= MAX_STRIDE;
    //@ assert(0 <= x0 * bpp < x1 * bpp <= w * bpp) by (nonlinear_arith) requires 0 <= x0 < x1 <= w, 1 <= bpp;
    //@ assert(x1 * bpp - x0 * bpp == (x1 - x0) * bpp) by (nonlinear_arith);
    //@ assert((h - 1) * stride + stride == h * stride) by (nonlinear_arith);
    Some((y * stride + x0 * bpp, y * stride + x1 * bpp))
}

// ---------------------------------------------------------------------------------------------------------------------
// The blend table (gl.cc: FOR_EACH_BLEND_KEY, hash_blend_key)

pub const GL_ZERO: u32 = 0;
pub const GL_ONE: u32 = 1;
pub const GL_SRC_COLOR: u32 = 0x0300;
pub const GL_ONE_MINUS_SRC_COLOR: u32 = 0x0301;
pub const GL_SRC_ALPHA: u32 = 0x0302;
pub const GL_ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
pub const GL_ONE_MINUS_DST_ALPHA: u32 = 0x0305;
pub const GL_CONSTANT_COLOR: u32 = 0x8001;
pub const GL_ONE_MINUS_SRC1_COLOR: u32 = 0x88FA;
pub const GL_FUNC_ADD: u32 = 0x8006;

/// The table, in `BlendKey` order: `(srgb, drgb, sa, da)`, with `(0, 0)` for "alpha as rgb", and `(equation, 0, 0, 0)`
/// for equations other than `GL_FUNC_ADD` (`GL_MIN` 0x8007, `GL_MAX` 0x8008, the KHR advanced modes). The last two are
/// swgl's own overrides, chosen by shaders, never by `glBlendFunc`.
pub const BLEND_TABLE: [(u32, u32, u32, u32); 32] = [
    (GL_ONE, GL_ZERO, 0, 0),
    (GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA, GL_ONE, GL_ONE_MINUS_SRC_ALPHA),
    (GL_ONE, GL_ONE_MINUS_SRC_ALPHA, 0, 0),
    (GL_ZERO, GL_ONE_MINUS_SRC_COLOR, 0, 0),
    (GL_ZERO, GL_ONE_MINUS_SRC_COLOR, GL_ZERO, GL_ONE),
    (GL_ZERO, GL_ONE_MINUS_SRC_ALPHA, 0, 0),
    (GL_ZERO, GL_SRC_COLOR, 0, 0),
    (GL_ONE, GL_ONE, 0, 0),
    (GL_ONE, GL_ONE, GL_ONE, GL_ONE_MINUS_SRC_ALPHA),
    (GL_ONE_MINUS_DST_ALPHA, GL_ONE, GL_ZERO, GL_ONE),
    (GL_CONSTANT_COLOR, GL_ONE_MINUS_SRC_COLOR, 0, 0),
    (GL_ONE, GL_ONE_MINUS_SRC1_COLOR, 0, 0),
    (0x8007, 0, 0, 0),
    (0x8008, 0, 0, 0),
    (0x9294, 0, 0, 0),
    (0x9295, 0, 0, 0),
    (0x9296, 0, 0, 0),
    (0x9297, 0, 0, 0),
    (0x9298, 0, 0, 0),
    (0x9299, 0, 0, 0),
    (0x929A, 0, 0, 0),
    (0x929B, 0, 0, 0),
    (0x929C, 0, 0, 0),
    (0x929E, 0, 0, 0),
    (0x92A0, 0, 0, 0),
    (0x92AD, 0, 0, 0),
    (0x92AE, 0, 0, 0),
    (0x92AF, 0, 0, 0),
    (0x92B0, 0, 0, 0),
    (0xB001, 0, 0, 0),
    (0xB002, 0, 0, 0),
    (0, 0, 0, 0),
];

/// Entries reachable from `glBlendFunc`/`glBlendEquation`: the first 29. 29 and 30 are the shader overrides
/// (`SWGL_BLEND_DROP_SHADOW`, `SWGL_BLEND_SUBPIXEL_TEXT`) and 31 is padding that is never matched.
pub const BLEND_KEYS_FROM_STATE: usize = 29;

/// The tuple `hash_blend_key` (gl.cc) looks up, before hashing: alpha factors equal to the rgb ones are dropped, and
/// any equation but `GL_FUNC_ADD` replaces the factors. Like swgl, `(sa, da) == (0, 0)` reads as "alpha as rgb".
//@ pub open spec fn blend_tuple_spec(srgb: u32, drgb: u32, sa: u32, da: u32, equation: u32) -> (u32, u32, u32, u32) {
//@     if equation != GL_FUNC_ADD { (equation, 0u32, 0u32, 0u32) }
//@     else if srgb == sa && drgb == da { (srgb, drgb, 0u32, 0u32) }
//@     else { (srgb, drgb, sa, da) }
//@ }

pub fn blend_tuple(srgb: u32, drgb: u32, sa: u32, da: u32, equation: u32) -> (u32, u32, u32, u32) //@ret
//@ ensures ret == blend_tuple_spec(srgb, drgb, sa, da, equation),
{
    if equation != GL_FUNC_ADD {
        return (equation, 0, 0, 0);
    }
    if srgb == sa && drgb == da {
        return (srgb, drgb, 0, 0);
    }
    (srgb, drgb, sa, da)
}

/// The blend key for a GL blend state: the index of its exact tuple among the first [`BLEND_KEYS_FROM_STATE`]
/// entries, or `None`. swgl hashes the four enums into one word whose fields overlap; this compares them, so a state
/// outside the table cannot alias an entry (`tests::hash_agrees_on_gl_enums` checks where the two agree).
pub fn blend_key(srgb: u32, drgb: u32, sa: u32, da: u32, equation: u32) -> Option<usize> //@ret
//@ ensures match ret {
//@     Some(i) => i < BLEND_KEYS_FROM_STATE && BLEND_TABLE[i as int] == blend_tuple_spec(srgb, drgb, sa, da, equation),
//@     None => forall|j: int| 0 <= j < BLEND_KEYS_FROM_STATE ==> BLEND_TABLE[j] != blend_tuple_spec(srgb, drgb, sa, da, equation),
//@ },
{
    let key = blend_tuple(srgb, drgb, sa, da, equation);
    let mut i = 0;
    while i < BLEND_KEYS_FROM_STATE
    //@ invariant 0 <= i <= BLEND_KEYS_FROM_STATE, key == blend_tuple_spec(srgb, drgb, sa, da, equation),
    //@     forall|j: int| 0 <= j < i ==> BLEND_TABLE[j] != key,
    //@ decreases BLEND_KEYS_FROM_STATE - i,
    {
        let e = BLEND_TABLE[i];
        if e.0 == key.0 && e.1 == key.1 && e.2 == key.2 && e.3 == key.3 {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// What LucinaNet's swgl does with a state the table lacks (patches/swgl): blend as premultiplied source-over
/// (`GL_ONE, GL_ONE_MINUS_SRC_ALPHA`, key 2) rather than abort.
pub fn blend_key_or_fallback(srgb: u32, drgb: u32, sa: u32, da: u32, equation: u32) -> usize //@ret
//@ ensures ret < BLEND_KEYS_FROM_STATE,
//@     (exists|j: int| 0 <= j < BLEND_KEYS_FROM_STATE && BLEND_TABLE[j] == blend_tuple_spec(srgb, drgb, sa, da, equation))
//@         ==> BLEND_TABLE[ret as int] == blend_tuple_spec(srgb, drgb, sa, da, equation),
{
    match blend_key(srgb, drgb, sa, da, equation) {
        Some(i) => i,
        None => 2,
    }
}

//@ /// No two entries are the same tuple, so a state has at most one key.
//@ proof fn blend_table_entries_differ()
//@     ensures forall|i: int, j: int| 0 <= i < j < 32 ==> BLEND_TABLE[i] != BLEND_TABLE[j],
//@ {
//@     assert forall|i: int, j: int| 0 <= i < j < 32 implies BLEND_TABLE[i] != BLEND_TABLE[j] by {
//@         reveal_with_fuel(BLEND_TABLE, 1);
//@     }
//@ }

// ---------------------------------------------------------------------------------------------------------------------
// 8-bit blend arithmetic (blend.h), one channel of the u16 lanes it works on

/// `muldiv255` (blend.h): `(x*y + x) >> 8`, swgl's approximation of `x*y/255`.
pub fn muldiv255(x: u16, y: u16) -> u16 //@ret
//@ requires x <= 255, y <= 255,
//@ ensures ret as int == muldiv255_spec(x as int, y as int), ret <= x, ret <= y, ret as int * 256 <= x as int * (y as int + 1) < (ret as int + 1) * 256,
//@     ret as int * 255 <= x as int * y as int + 255, x as int * y as int <= (ret as int + 1) * 255,
{
    //@ assert(x as int * y as int + x as int <= 65280) by (nonlinear_arith) requires x <= 255, y <= 255;
    let r = (x * y + x) >> 8;
    //@ assert(r as int == (x as int * (y as int + 1)) / 256) by (bit_vector) requires r == ((x * y + x) as u16) >> 8u16, x <= 255u16, y <= 255u16;
    //@ assert(r as int <= x as int && r as int <= y as int) by (nonlinear_arith) requires r as int == (x as int * (y as int + 1)) / 256, x <= 255, y <= 255;
    //@ assert(r as int * 255 <= x as int * y as int + 255 && x as int * y as int <= (r as int + 1) * 255) by (nonlinear_arith) requires r as int == (x as int * (y as int + 1)) / 256, x <= 255, y <= 255;
    r
}

/// One channel of premultiplied source-over, `BLEND_CASE(GL_ONE, GL_ONE_MINUS_SRC_ALPHA)` in blend.h:
/// `src + dst - muldiv255(dst, src_alpha)`. With a premultiplied source (`src <= src_alpha`) it neither wraps below 0
/// nor exceeds 255, so the u16 lane arithmetic and the final pack cannot clip.
pub fn source_over(src: u16, dst: u16, src_alpha: u16) -> u16 //@ret
//@ requires src <= src_alpha <= 255, dst <= 255,
//@ ensures ret <= 255, ret >= src, ret as int == src as int + dst as int - muldiv255_spec(dst as int, src_alpha as int),
{
    let m = muldiv255(dst, src_alpha);
    //@ assert(m as int * 256 + 256 > dst as int * (src_alpha as int + 1));
    //@ assert(dst as int + src_alpha as int - m as int <= 255) by (nonlinear_arith) requires m as int * 256 + 256 > dst as int * (src_alpha as int + 1), dst <= 255, src_alpha <= 255;
    src + (dst - m)
}

//@ pub open spec fn muldiv255_spec(x: int, y: int) -> int { (x * (y + 1)) / 256 }
