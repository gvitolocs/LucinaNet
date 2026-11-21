// The arithmetic of swgl's scaled blits (composite.h `scale_blit`, `Composite`; gl.cc `IntRect::scale`): which
// destination pixels may be drawn so that each one reads a source texel inside the source texture. Verus checks this
// very file (raster/verify.sh). Coordinates are along one axis; the blit applies it to x and to y.

//@ global size_of usize == 8;

/// Larger than any texture side or blit size (gl.cc `GL_MAX_TEXTURE_SIZE`).
pub const MAX: i64 = 32768;

/// The source texel destination pixel `x` reads when `src` source pixels are stretched over `dst` destination pixels:
/// `floor(x * src / dst)`, as `scale_blit` steps it (`fracX = srcWidth * x0; x0 / dstWidth`, then `+ srcWidth` per pixel).
pub fn source_of(x: i64, src: i64, dst: i64) -> i64 //@ret
//@ requires 0 <= x <= MAX, 1 <= src <= MAX, 1 <= dst <= MAX,
//@ ensures ret == (x * src) / (dst as int), 0 <= ret,
{
    //@ assert(0 <= x * src <= MAX * MAX) by (nonlinear_arith) requires 0 <= x <= MAX, 1 <= src <= MAX;
    //@ proof { vstd::arithmetic::div_mod::lemma_div_pos_is_pos(x * src, dst as int); }
    x * src / dst
}

/// `IntRect::scale(src, ..., dst, ..., roundIn = true)` (gl.cc) for one axis, applied to the source texture's extent
/// `t0..t1` taken relative to the blit's source rectangle, then cut at 0 as `scale_blit` does by intersecting with the
/// destination bounds: the destination pixels `d0..d1` whose source texel is inside the texture. The C++ divides a
/// negative start with truncation; cutting at 0 first gives the same range after that intersection.
pub fn scale_clip(t0: i64, t1: i64, src: i64, dst: i64) -> (i64, i64) //@ret
//@ requires -MAX <= t0 <= MAX, -MAX <= t1 <= MAX, 1 <= src <= MAX, 1 <= dst <= MAX,
//@ ensures 0 <= ret.0, 0 <= ret.1,
//@     t0 > 0 ==> ret.0 * src >= t0 * dst,
//@     t0 <= 0 ==> ret.0 == 0,
//@     t1 > 0 ==> ret.1 * src <= t1 * dst,
//@     t1 <= 0 ==> ret.1 == 0,
{
    let d0 = if t0 <= 0 {
        0
    } else {
        //@ assert(0 < t0 * dst <= MAX * MAX) by (nonlinear_arith) requires 0 < t0 <= MAX, 1 <= dst <= MAX;
        let n = t0 * dst + src - 1;
        //@ proof { vstd::arithmetic::div_mod::lemma_fundamental_div_mod(n as int, src as int); vstd::arithmetic::div_mod::lemma_mod_pos_bound(n as int, src as int); }
        //@ assert((n as int / (src as int)) * src >= t0 * dst) by (nonlinear_arith) requires n == t0 * dst + src - 1, n == src * (n as int / (src as int)) + n as int % (src as int), 0 <= n as int % (src as int) < src;
        //@ proof { vstd::arithmetic::div_mod::lemma_div_pos_is_pos(n as int, src as int); }
        n / src
    };
    let d1 = if t1 <= 0 {
        0
    } else {
        //@ assert(0 < t1 * dst <= MAX * MAX) by (nonlinear_arith) requires 0 < t1 <= MAX, 1 <= dst <= MAX;
        let m = t1 * dst;
        //@ proof { vstd::arithmetic::div_mod::lemma_fundamental_div_mod(m as int, src as int); vstd::arithmetic::div_mod::lemma_mod_pos_bound(m as int, src as int); vstd::arithmetic::div_mod::lemma_div_pos_is_pos(m as int, src as int); }
        //@ assert((m as int / (src as int)) * src <= m) by (nonlinear_arith) requires m == src * (m as int / (src as int)) + m as int % (src as int), 0 <= m as int % (src as int);
        m / src
    };
    (d0, d1)
}

//@ /// The point of `scale_clip`: every destination pixel in `d0..d1` reads a source texel in `t0..t1`.
//@ proof fn scale_clip_reads_inside(t0: i64, t1: i64, src: i64, dst: i64, x: i64)
//@     requires -MAX <= t0 <= MAX, -MAX <= t1 <= MAX, 1 <= src <= MAX, 1 <= dst <= MAX, 0 <= x <= MAX,
//@         (t0 > 0 ==> x * src >= t0 * dst), t1 > 0, (x + 1) * src <= t1 * dst,
//@     ensures t0 <= (x * src) / (dst as int) < t1, 0 <= (x * src) / (dst as int),
//@ {
//@     let q = (x * src) / (dst as int);
//@     assert(0 <= x * src) by (nonlinear_arith) requires 0 <= x, 1 <= src;
//@     vstd::arithmetic::div_mod::lemma_fundamental_div_mod(x * src, dst as int);
//@     vstd::arithmetic::div_mod::lemma_mod_pos_bound(x * src, dst as int);
//@     vstd::arithmetic::div_mod::lemma_div_pos_is_pos(x * src, dst as int);
//@     if t0 > 0 {
//@         assert(q >= t0) by (nonlinear_arith) requires x * src == dst * q + (x * src) % (dst as int), 0 <= (x * src) % (dst as int) < dst, x * src >= t0 * dst, 1 <= dst;
//@     }
//@     assert(q < t1) by (nonlinear_arith) requires x * src == dst * q + (x * src) % (dst as int), 0 <= (x * src) % (dst as int), (x + 1) * src <= t1 * dst, 1 <= src, 1 <= dst;
//@ }

//@ /// `d1` from `scale_clip` keeps `(x + 1) * src <= t1 * dst` for every `x < d1`, which the lemma above needs.
//@ proof fn below_d1_fits(t1: i64, src: i64, dst: i64, d1: i64, x: i64)
//@     requires 1 <= src, 1 <= dst, t1 > 0, d1 * src <= t1 * dst, 0 <= x < d1,
//@     ensures (x + 1) * src <= t1 * dst,
//@ {
//@     assert((x + 1) * src <= d1 * src) by (nonlinear_arith) requires x + 1 <= d1, 1 <= src;
//@ }
