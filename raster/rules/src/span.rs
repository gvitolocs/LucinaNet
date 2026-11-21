// The index and range arithmetic of swgl's span fast paths (swgl_ext.h, raster/core/src/ext.rs): how a span is cut
// against a texture row, into 4-pixel chunks and a remainder, where a clamped or repeated texel lies, and which entry
// of a gradient table a sample reads. Verus checks this very file as raster/verify.sh does for lib.rs: `//@`
// lines hold the proof. The C++ does this arithmetic in `int` and overflows (undefined behaviour) on coordinates no
// shader produces; here it is widened to `i64` first, so the results agree wherever the C++ is defined.
#![allow(clippy::needless_return)] //@omit

//@ global size_of usize == 8;

/// The longest span the rasterizer hands a span shader: a row of the largest texture (`GL_MAX_TEXTURE_SIZE`).
pub const MAX_SPAN: i64 = 32768;
/// The largest texture side (gl.cc, `1 << 15`).
pub const SIDE: i64 = 32768;
/// The largest row stride of any texture, in its own units (`rules::MAX_STRIDE` bytes).
pub const STRIDE: i64 = 524288;
/// The largest texel index of the largest texture: row `SIDE - 1` at the largest stride, plus a row of `SIDE`
/// texels (`computeRow` of the linear filters, `clampCoord(x, width - 1) + clampCoord(y, height) * stride`).
pub const MAX_ROW: i64 = (SIDE - 1) * STRIDE + SIDE;

/// `len & ~3` and `len & 3` of a non-negative pixel count: the pixels in whole 4-pixel chunks and those left over.
pub fn chunks(len: i64) -> (i64, i64) //@ret
//@ requires 0 <= len <= MAX_SPAN,
//@ ensures ret.0 % 4 == 0, 0 <= ret.1 < 4, ret.0 + ret.1 == len, 0 <= ret.0 <= len,
{
    let rest = len % 4;
    (len - rest, rest)
}

/// `min((n + 3) & ~3, len)` (`commit_aa_solid_span`): `n` pixels rounded up to whole chunks, cut to the span.
pub fn chunk_ceil(n: i64, len: i64) -> i64 //@ret
//@ requires 0 <= n <= MAX_SPAN, 0 <= len <= MAX_SPAN,
//@ ensures 0 <= ret <= len, ret == len || ret % 4 == 0, ret >= n || ret == len, ret == len || ret < n + 4,
{
    let up = (n + 3) / 4 * 4;
    if up < len {
        return up;
    }
    len
}

/// `min(n & ~3, avail)` for a count from a float conversion that may be negative or huge (the inside part of the
/// linear filters): `None` when the C++ `if (inside > 0)` skips it.
pub fn chunk_floor(n: i64, avail: i64) -> Option<i64> //@ret
//@ requires 0 <= avail <= MAX_SPAN, -0x8000_0000 <= n <= 0x7fff_ffff,
//@ ensures match ret { Some(k) => 0 < k <= avail && (k % 4 == 0 || k == avail), None => true },
{
    if n < 4 {
        return None;
    }
    let down = n / 4 * 4;
    let k = if down < avail { down } else { avail };
    if k <= 0 {
        return None;
    }
    Some(k)
}

/// `clamp(steps * 4, 0, avail)` with C++'s 32-bit wrapping of `steps * 4` (`blendTextureLinearDispatch`'s
/// `before`): the pixels to send through the clamping fallback before the row.
pub fn clamp_steps(steps: i32, avail: i64) -> i64 //@ret
//@ requires 0 <= avail <= MAX_SPAN,
//@ ensures 0 <= ret <= avail,
{
    let px = steps.wrapping_mul(4) as i64;
    if px < 0 {
        return 0;
    }
    if px > avail {
        return avail;
    }
    px
}

/// `min(n, len)` (`swgl_commitPartialSolid*`, `swgl_commitPartialTextureLinear*`), never negative: the C++ moves
/// the span pointer backwards on a negative count, which no shader passes.
pub fn partial_len(n: i64, len: i64) -> i64 //@ret
//@ requires 0 <= len <= MAX_SPAN,
//@ ensures 0 <= ret <= len, n >= 0 && n <= len ==> ret == n, n >= len ==> ret == len,
{
    if n < 0 {
        return 0;
    }
    if n < len {
        return n;
    }
    len
}

/// `clampCoord(c, limit)` on scalars (texture.h): `min(max(c, 0), limit - 1)`.
pub fn clamp_coord(c: i64, limit: i64) -> i64 //@ret
//@ requires 1 <= limit <= SIDE,
//@ ensures 0 <= ret < limit, 0 <= c < limit ==> ret == c, c < 0 ==> ret == 0, c >= limit ==> ret == limit - 1,
{
    let lo = if c > 0 { c } else { 0 };
    if lo < limit - 1 {
        return lo;
    }
    limit - 1
}

/// The row of `blendTextureNearestFast`: `clampCoord(clamp(y, min_y, max_y), height) * stride`, in texels.
pub fn nearest_row(y: i64, min_y: i64, max_y: i64, height: i64, stride: i64) -> i64 //@ret
//@ requires 1 <= height <= SIDE, 1 <= stride <= STRIDE,
//@ ensures 0 <= ret <= (height - 1) * stride,
{
    // glsl.h's scalar `clamp(x, lo, hi)` is `min(max(x, lo), hi)`.
    let a = if y > min_y { y } else { min_y };
    let b = if a < max_y { a } else { max_y };
    let r = clamp_coord(b, height);
    //@ assert(0 <= r * stride <= (height - 1) * stride) by (nonlinear_arith) requires 0 <= r <= height - 1, 1 <= stride;
    //@ assert((height - 1) * stride <= SIDE * STRIDE) by (nonlinear_arith) requires 0 <= height - 1 < SIDE, 1 <= stride <= STRIDE;
    r * stride
}

/// How `blendTextureNearestFast` cuts the span of `len` pixels starting at texel `x` of a row `width` texels wide,
/// sampling only texels `min_uv..=max_uv`: `before` pixels repeat texel `min_x`, `inside` (a multiple of 4) and then
/// `rest` (< 4) pixels read texels from `x + before` on, `after` pixels repeat texel `max_x`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NearestSplit {
    pub min_x: i64,
    pub max_x: i64,
    pub before: i64,
    pub inside: i64,
    pub rest: i64,
    pub after: i64,
}

pub fn nearest_split(x: i64, len: i64, min_uv: i64, max_uv: i64, width: i64) -> NearestSplit //@ret
//@ requires 1 <= width <= SIDE, 0 <= len <= MAX_SPAN, -0x8000_0000 <= x <= 0x7fff_ffff,
//@     -0x8000_0000 <= min_uv <= 0x7fff_ffff, -0x8000_0000 <= max_uv <= 0x7fff_ffff,
//@ ensures 0 <= ret.min_x <= ret.max_x < width,
//@     0 <= ret.before, 0 <= ret.inside, 0 <= ret.rest < 4, 0 <= ret.after, ret.inside % 4 == 0,
//@     ret.before + ret.inside + ret.rest + ret.after == len,
//@     ret.inside + ret.rest > 0 ==> ret.min_x <= x + ret.before && x + ret.before + ret.inside + ret.rest <= ret.max_x + 1,
{
    let min_x = if min_uv < 0 { 0 } else if min_uv > width - 1 { width - 1 } else { min_uv };
    let max_x = if max_uv < min_x { min_x } else if max_uv > width - 1 { width - 1 } else { max_uv };
    let end_x = x + len;
    let mut cur = x;
    let mut before = 0;
    if cur < min_x {
        before = (if min_x < end_x { min_x } else { end_x }) - cur;
        cur = cur + before;
    }
    let lim = if max_x + 1 < end_x { max_x + 1 } else { end_x };
    let n = if lim - cur > 0 { lim - cur } else { 0 };
    let rest = n % 4;
    let inside = n - rest;
    cur = cur + n;
    let after = if cur < end_x { end_x - cur } else { 0 };
    NearestSplit { min_x, max_x, before, inside, rest, after }
}

/// `blendGaussianBlur`'s pixels: from texel `x`, whole chunks while `x + 4 <= min(min(bound, x + len), width)`.
pub fn blur_len(x: i64, len: i64, bound: i64, width: i64) -> i64 //@ret
//@ requires 0 <= len <= MAX_SPAN, 0 <= width <= SIDE, -0x8000_0000 <= x <= 0x7fff_ffff,
//@     -0x8000_0000 <= bound <= 0x7fff_ffff,
//@ ensures 0 <= ret <= len, ret % 4 == 0, ret > 0 ==> x + ret <= width && x + ret <= bound,
{
    let a = if bound < x + len { bound } else { x + len };
    let end = if a < width { a } else { width };
    let n = end - x;
    if n < 4 {
        return 0;
    }
    n / 4 * 4
}

/// The texel range the fast and downscale linear filters read (`blendTextureLinearFast` and
/// `blendTextureLinearDownscale`, swgl_ext.h): the loop loads 4 texels from `row0 + 4*j` and from
/// `row1 + 4*j` for `j = 0..=last`, so every texel it reads lies in
/// `[min(row0, row1), max(row0, row1) + 4*last + 4)`. True when that whole range is inside the sampler's
/// `len_texels` texels, which is what lets the loads skip their bounds check (`ExtPx::tex_signed_unchecked`).
pub fn linear_reads_in_bounds(row0: i64, row1: i64, last: i64, len_texels: i64) -> bool //@ret
//@ requires -MAX_ROW <= row0 <= MAX_ROW, -MAX_ROW <= row1 <= MAX_ROW, -1 <= last <= MAX_SPAN,
//@     0 <= len_texels <= SIDE * STRIDE,
//@ ensures ret == (row0 >= 0 && row1 >= 0 && last >= 0
//@     && (if row0 >= row1 { row0 } else { row1 }) + 4 * last + 4 <= len_texels),
//@     ret ==> forall|j: i64, t: i64| 0 <= j <= last && 0 <= t < 4 ==>
//@         0 <= #[trigger] (row0 + 4 * j + t) && #[trigger] (row0 + 4 * j + t) < len_texels
//@         && 0 <= #[trigger] (row1 + 4 * j + t) && #[trigger] (row1 + 4 * j + t) < len_texels,
{
    let m = if row0 >= row1 { row0 } else { row1 };
    row0 >= 0 && row1 >= 0 && last >= 0 && m + 4 * last + 4 <= len_texels
}

/// `(y & 7) * 8 + (x & 7)`: the `ditherNoise` entry for fragment `(x, y)` (`getDitherNoise`, `dither`).
pub fn dither_index(x: i32, y: i32) -> usize //@ret
//@ ensures ret < 64,
{
    let yi = ((y % 8) + 8) % 8;
    let xi = ((x % 8) + 8) % 8;
    (yi * 8 + xi) as usize
}

/// `findGradientStopPair`'s last step: the tree index clamped to `1..=stops - 1`, minus one: the first stop of the
/// pair, so that `stop` and `stop + 1` are both stops when there are at least 2.
pub fn stop_pair(index: i64, stops: i64) -> i64 //@ret
//@ requires 1 <= stops <= 0x7fff_ffff,
//@ ensures 0 <= ret, stops >= 2 ==> ret + 1 < stops,
{
    let i = if index < 1 { 1 } else if index > stops - 1 { stops - 1 } else { index };
    if i < 1 {
        return 0;
    }
    i - 1
}

/// `swgl_validateGradient` (`stops_px` = 2 texels per entry) and `swgl_validateGradientFromStops` (`entries`
/// colors plus a texel of 4 offsets per 4 stops): the float offset of the table at texel `(ax, ay)` of a `width` by
/// `height` RGBA32F texture with `stride` floats per row, or -1. A table found lies inside the texture's floats.
pub fn gradient_address(ax: i64, ay: i64, entries: i64, from_stops: bool, width: i64, height: i64, stride: i64) -> i64 //@ret
//@ requires 0 <= width <= SIDE, 0 <= height <= SIDE, 4 * width <= stride <= 4 * SIDE,
//@     -0x8000_0000 <= ax <= 0x7fff_ffff, -0x8000_0000 <= ay <= 0x7fff_ffff, -0x8000_0000 <= entries <= 0x7fff_ffff,
//@ ensures ret == -1 || (0 <= ret && ret + 4 * (if from_stops { entries + (entries + 3) / 4 } else { 2 * entries }) <= height * stride
//@     ),
{
    if ay < 0 || ay >= height || ax < 0 || ax >= width || entries <= 0 {
        return -1;
    }
    let texels = if from_stops { entries + (entries + 3) / 4 } else { 2 * entries };
    if ax + texels > width {
        return -1;
    }
    //@ assert(0 <= ay * stride <= (height - 1) * stride) by (nonlinear_arith) requires 0 <= ay <= height - 1, 0 <= stride;
    //@ assert((height - 1) * stride + stride == height * stride) by (nonlinear_arith);
    //@ assert((height - 1) * stride <= SIDE * 4 * SIDE) by (nonlinear_arith) requires 0 <= height - 1 < SIDE, 0 <= stride <= 4 * SIDE;
    ay * stride + ax * 4
}

/// The table entry `sampleGradient` reads for an entry index that the caller clamped to `0..=entries - 1`: its
/// float offset, `address + 8 * index`, inside `[address, address + 8 * entries)`.
pub fn gradient_entry(address: i64, index: i64, entries: i64) -> i64 //@ret
//@ requires 0 <= address <= 0x7fff_ffff, 1 <= entries <= SIDE,
//@ ensures address <= ret, ret + 8 <= address + 8 * entries,
{
    let i = if index < 0 { 0 } else if index > entries - 1 { entries - 1 } else { index };
    address + 8 * i
}
