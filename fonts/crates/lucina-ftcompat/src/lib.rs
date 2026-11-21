// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from FreeType 2.13.2 (FreeType License, see LICENSES/FTL.TXT).

//! FreeType 2.13.2's numbers, without FreeType: the fixed-point arithmetic, size scaling, face metrics, hinted
//! advances and synthetic-bold strength that LucinaNet's Servo fonts and WebRender glyph rasterizer used to get from
//! `libfreetype`. Each function names the FreeType function it reproduces; the source of truth is the FreeType tree
//! bundled in `freetype-sys` 0.23.0 (`freetype2/src/...`), the library the old binary linked.
//!
//! Everything here is pure (no allocation, no I/O), so the same code runs in tests against FreeType itself and under
//! Verus (`verification/`).
//!
//! Verus proves these functions in this very file, not a model of them (the rule; `verification/ftcompat.rs`
//! names this file with `#[path]`). The attributes below are only seen by Verus; cargo ignores them. What each function
//! promises is in its `ensures`. Trusted, not proved: [`char_size`] and [`ppem_f32_for_scale`], which are float
//! arithmetic (`external_body`) — Rust's float-to-int casts and float rounding are trusted to match the C ones.
#![cfg_attr(not(test), no_std)]
#![allow(unexpected_cfgs)]

// Not yet under Verus: verification/ftcompat.rs loads this file by #[path], where the child module would not resolve.
#[cfg(not(verus_keep_ghost))]
pub mod embolden;

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg(verus_keep_ghost)]
verus! {

// Spec-mode helpers: two's-complement truncation and the wrapping shifts the exec code's `as` casts, `<<` and `>>`
// perform on fixed-width integers. In spec mode Verus integers are mathematical, so the bit operations are written
// out: `x >> k` on a signed value is floor division by 2^k, `x << k` on a 64-bit value wraps mod 2^64.

/// Truncate to a signed 32-bit value.
pub open spec fn trunc_i32(x: int) -> int {
    let m = x % 4294967296;
    if m >= 2147483648 { m - 4294967296 } else { m }
}

/// Truncate to a signed 64-bit value.
pub open spec fn trunc_i64(x: int) -> int {
    let m = x % 18446744073709551616;
    if m >= 9223372036854775808 { m - 18446744073709551616 } else { m }
}

/// Truncate to an unsigned 32-bit value.
pub open spec fn trunc_u32(x: int) -> int { x % 4294967296 }

/// Truncate to a signed 16-bit value.
pub open spec fn trunc_i16(x: int) -> int {
    let m = x % 65536;
    if m >= 32768 { m - 65536 } else { m }
}

/// `|x|` as a non-negative integer (the spec of `unsigned_abs`).
pub open spec fn uabs(x: i64) -> int {
    if x < 0 { -(x as int) } else { x as int }
}

/// Rust's truncating integer division by a positive divisor.
pub open spec fn tdiv(a: int, b: int) -> int {
    if a >= 0 || a % b == 0 { a / b } else { a / b + 1 }
}

/// Spec of `FT_MulFix` on the 32-bit truncated arguments: round halves away from zero, then truncate to `FT_Long`'s
/// 32-bit result.
pub open spec fn mul_fix_int(a: int, b: int) -> int {
    let ab = trunc_i32(a) * trunc_i32(b);
    trunc_i32(floor_div(ab + 0x8000 - (if ab < 0 { 1int } else { 0int }), 65536))
}

/// Floor division by a positive divisor (Verus's `/` on `int` is Euclidean, which is floor division for `d > 0`).
pub open spec fn floor_div(n: int, d: int) -> int { n / d }

/// `n / d` rounded to the nearest integer, halves away from zero (`d > 0`).
pub open spec fn round_half_away(n: int, d: int) -> int {
    if n >= 0 { (n + d / 2) / d } else { -((-n + d / 2) / d) }
}

/// FreeType's rounded quotient: `(n + d/2) / d` on the absolute values, made negative when `negative`.
pub open spec fn signed_rounded_quot(negative: bool, n: int, d: int) -> int {
    let q = (n + d / 2) / d;
    if negative { -q } else { q }
}

// Lemmas connecting the machine operations of mul_fix/div_fix/mul_div to integer arithmetic.

/// An `as i32` cast is `trunc_i32`.
proof fn lemma_cast_i32(a: i64)
    ensures (a as i32) as int == trunc_i32(a as int),
{
    let lo = a & 0xFFFFFFFF;
    assert(0 <= a & 0xFFFFFFFF && a & 0xFFFFFFFF < 4294967296) by(bit_vector);
    assert(a == (a >> 32) * 4294967296 + (a & 0xFFFFFFFF)) by(bit_vector);
    assert((a as int) % 4294967296 == lo as int) by {
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(a as int, 4294967296, (a >> 32) as int, lo as int);
    }
    assert((a as i32) as i64 == (if a & 0xFFFFFFFF >= 2147483648 { (a & 0xFFFFFFFF) - 4294967296 }
        else { (a & 0xFFFFFFFF) as int })) by(bit_vector);
}

/// The arithmetic shift `t >> 16` is floor division by 65536.
proof fn lemma_shr16(t: i64)
    ensures (t >> 16) as int == floor_div(t as int, 65536),
{
    let s = t >> 16;
    let m = t & 0xFFFF;
    assert(0 <= t & 0xFFFF && t & 0xFFFF < 65536) by(bit_vector);
    assert(t == (t >> 16) * 65536 + (t & 0xFFFF)) by(bit_vector);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(t as int, 65536, s as int, m as int);
}

/// `trunc_i32` is the identity on the i32 range.
proof fn lemma_trunc_i32_id(x: int)
    requires -2147483648 <= x && x <= 2147483647,
    ensures trunc_i32(x) == x,
{
    if x >= 0 {
        vstd::arithmetic::div_mod::lemma_small_mod(x as nat, 4294967296);
    } else {
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(x, 4294967296, -1, x + 4294967296);
    }
}

/// FT_MulFix's `+ 0x8000 - (ab < 0)` then floor division is rounding halves away from zero.
proof fn lemma_round_65536(n: int)
    ensures floor_div(n + 0x8000 - (if n < 0 { 1int } else { 0int }), 65536) == round_half_away(n, 65536),
{
    if n < 0 {
        let q = (-n + 32768) / 65536;
        let r = (-n + 32768) % 65536;
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(-n + 32768, 65536);
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod_converse(n + 32767, 65536, -q, 65535 - r);
    }
}

/// `|b|` as FreeType computes it, `(0 - b) as u64` wrapping, for every i64 including `i64::MIN`.
proof fn lemma_neg_abs(b: i64)
    ensures b < 0 ==> (0i64.wrapping_sub(b) as u64) as int == -(b as int),
{
    assert(b < 0 ==> (0i64.wrapping_sub(b) as u64) as int == -(b as int)) by(bit_vector);
}

pub open spec fn mul_fix_spec(a: i64, b: i64) -> i64 {
    let a32 = a as i32;
    let b32 = b as i32;
    let ab = (a32 as i64).wrapping_mul(b32 as i64);
    let t = ab.wrapping_add(0x8000).wrapping_add(ab >> 63);
    ((t >> 16) as i32) as i64
}

/// Spec of `FT_DivFix`: rounded quotient of the absolute values, sign moved back out, `0x7FFFFFFF` when dividing by
/// zero. The `<< 16` and the `+` wrap in 64 bits exactly as the shipped code's do.
pub open spec fn div_fix_spec(a: i64, b: i64) -> i64 {
    let negative = (a < 0) != (b < 0);
    let au = if a < 0 { 0i64.wrapping_sub(a) as u64 } else { a as u64 };
    let bu = if b < 0 { 0i64.wrapping_sub(b) as u64 } else { b as u64 };
    let q = if bu > 0 { au.wrapping_shl(16).wrapping_add(bu >> 1) / bu } else { 0x7FFF_FFFFu64 };
    let q = q as i64;
    if negative { 0i64.wrapping_sub(q) } else { q }
}

/// Spec of `FT_MulDiv`: `a * b / c` on the absolute values in 128 bits, rounded, sign moved back out, `0x7FFFFFFF`
/// when `c` is zero.
pub open spec fn mul_div_spec(a: i64, b: i64, c: i64) -> i64 {
    let negative = ((a < 0) != (b < 0)) != (c < 0);
    let au = if a < 0 { (0i64.wrapping_sub(a) as u64) as u128 } else { a as u128 };
    let bu = if b < 0 { (0i64.wrapping_sub(b) as u64) as u128 } else { b as u128 };
    let cu = if c < 0 { (0i64.wrapping_sub(c) as u64) as u128 } else { c as u128 };
    let d = if cu > 0 { au.wrapping_mul(bu).wrapping_add(cu >> 1) / cu } else { 0x7FFF_FFFFu128 };
    let d = d as i64;
    if negative { 0i64.wrapping_sub(d) } else { d }
}

/// Spec of `FT_Size_Metrics::y_ppem` for a nominal request.
pub open spec fn ppem_spec(char_size: i64) -> int {
    (char_size as int + 32) / 64
}

/// The update condition of `choose_strike`: a later strike replaces the chosen one when it is at least as large while
/// the chosen one was smaller than requested, or when it is at least as close.
pub open spec fn iabs(x: int) -> int { if x < 0 { -x } else { x } }

pub open spec fn strike_update(best_dist: int, dist: int) -> bool {
    (best_dist < 0 && dist >= best_dist) || iabs(dist) <= best_dist
}

/// What `choose_strike` promises: `None` exactly for an empty slice, a valid index otherwise, and no later strike
/// would have replaced the chosen one under the update condition.
pub open spec fn choose_strike_ok(strike_y_ppem: &[i64], requested: i64, r: Option<usize>) -> bool {
    &&& strike_y_ppem@.len() == 0 <==> r is None
    &&& r matches Some(i) ==> 0 <= i as int && (i as int) < strike_y_ppem@.len()
    &&& r matches Some(i) ==> forall |j: int|
            (i as int) < j && j < strike_y_ppem@.len()
                ==> !strike_update(strike_y_ppem@[i as int] - requested, strike_y_ppem@[j] - requested)
}

/// Spec of `sfnt_load_face`'s field arithmetic, in `FT_Short` (wrapping) semantics.
pub open spec fn face_metrics_spec(t: FaceTables) -> FaceMetrics {
    let ascender0 = t.hhea_ascender;
    let descender0 = t.hhea_descender;
    let height0 = ascender0.wrapping_sub(descender0).wrapping_add(t.hhea_line_gap);
    let underline_position = t.post_underline_position.wrapping_sub((tdiv(t.post_underline_thickness as int, 2)) as i16);
    if ascender0 == 0 && descender0 == 0 {
        match t.os2 {
            Some(os2) => {
                if os2.typo_ascender != 0 || os2.typo_descender != 0 {
                    FaceMetrics {
                        ascender: os2.typo_ascender,
                        descender: os2.typo_descender,
                        height: os2.typo_ascender.wrapping_sub(os2.typo_descender).wrapping_add(os2.typo_line_gap),
                        max_advance_width: t.hhea_advance_width_max as i16,
                        underline_position,
                        underline_thickness: t.post_underline_thickness,
                    }
                } else {
                    FaceMetrics {
                        ascender: os2.win_ascent as i16,
                        descender: 0i16.wrapping_sub(os2.win_descent as i16),
                        height: (os2.win_ascent as i16).wrapping_sub(0i16.wrapping_sub(os2.win_descent as i16)),
                        max_advance_width: t.hhea_advance_width_max as i16,
                        underline_position,
                        underline_thickness: t.post_underline_thickness,
                    }
                }
            }
            None => FaceMetrics {
                ascender: ascender0,
                descender: descender0,
                height: height0,
                max_advance_width: t.hhea_advance_width_max as i16,
                underline_position,
                underline_thickness: t.post_underline_thickness,
            },
        }
    } else {
        FaceMetrics {
            ascender: ascender0,
            descender: descender0,
            height: height0,
            max_advance_width: t.hhea_advance_width_max as i16,
            underline_position,
            underline_thickness: t.post_underline_thickness,
        }
    }
}

/// Spec of `tt_apply_mvar`, in `FT_Short` (wrapping) semantics.
pub open spec fn apply_mvar_spec(m: FaceMetrics, t: FaceTables, d: MvarDeltas) -> FaceMetrics {
    let current_line_gap = m.height.wrapping_sub(m.ascender).wrapping_add(m.descender);
    let ascender = m.ascender.wrapping_add(d.hasc);
    let descender = m.descender.wrapping_add(d.hdsc);
    let underline_position = t.post_underline_position.wrapping_add(d.undo);
    let underline_thickness = t.post_underline_thickness.wrapping_add(d.unds);
    FaceMetrics {
        ascender,
        descender,
        height: ascender.wrapping_sub(descender).wrapping_add(current_line_gap).wrapping_add(d.hlgp),
        max_advance_width: m.max_advance_width,
        underline_position: underline_position.wrapping_sub((tdiv(underline_thickness as int, 2)) as i16),
        underline_thickness,
    }
}

} // verus!

/// A 26.6 fixed-point value (`FT_F26Dot6`, `FT_Pos`): 1/64 of a pixel.
pub type F26Dot6 = i64;
/// A 16.16 fixed-point value (`FT_Fixed`).
pub type Fixed = i64;

/// `FT_MulFix` (`ftcalc.h` `FT_MulFix_x86_64`, and the portable `ftcalc.c` one used on AArch64): both take their
/// arguments as 32-bit integers and round halves away from zero.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == mul_fix_spec(a, b),
        -2147483648 <= r && r <= 2147483647,
        0 <= a && a <= 65535 && 0 <= b && b <= 2147483647 ==> r >= 0,
        // On integers: A = trunc_i32(a), B = trunc_i32(b), floor((A*B + 0x8000 - (A*B < 0)) / 65536) truncated
        // to 32 bits.
        r as int == mul_fix_int(a as int, b as int),
        // While the rounded quotient fits in 32 bits (|A*B| < 2^47 - 2^15) it is A*B/65536 rounded half away from
        // zero. (At A*B = 2^47 - 1 it rounds to 2^31, which the 32-bit result wraps to -2^31, as FreeType's does.)
        iabs(trunc_i32(a as int) * trunc_i32(b as int)) < 140737488322560
            ==> r as int == round_half_away(trunc_i32(a as int) * trunc_i32(b as int), 65536),
))]
pub fn mul_fix(a: i64, b: i64) -> i64 {
    // `wrapping_*` and `#[verifier::truncate]` only make explicit what the plain operators already do here (the
    // product of two 32-bit values fits in 64 bits); behaviour is identical for every input.
    #[cfg_attr(verus_keep_ghost, verifier::truncate)]
    let a32 = a as i32;
    #[cfg_attr(verus_keep_ghost, verifier::truncate)]
    let b32 = b as i32;
    let ab = (a32 as i64).wrapping_mul(b32 as i64);
    let t = ab.wrapping_add(0x8000).wrapping_add(ab >> 63);
    #[cfg_attr(verus_keep_ghost, verifier::truncate)]
    let r32 = (t >> 16) as i32;
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(r32 as i64 == mul_fix_spec(a, b));
        assert(-2147483648 <= r32 as i64 && r32 as i64 <= 2147483647) by(bit_vector);
        assert(0 <= a && a <= 65535 && 0 <= b && b <= 2147483647 ==> mul_fix_spec(a, b) >= 0) by(bit_vector);
        lemma_cast_i32(a);
        lemma_cast_i32(b);
        let ai = a32 as int;
        let bi = b32 as int;
        assert(-4611686018427387904 <= ai * bi && ai * bi <= 4611686018427387904) by(nonlinear_arith)
            requires -2147483648 <= ai && ai <= 2147483647 && -2147483648 <= bi && bi <= 2147483647;
        assert(ab as int == ai * bi);
        assert(ab >> 63 == (if ab < 0 { -1i64 } else { 0i64 })) by(bit_vector);
        assert(t as int == ai * bi + 0x8000 - (if ai * bi < 0 { 1int } else { 0int }));
        lemma_shr16(t);
        lemma_cast_i32(t >> 16);
        lemma_round_65536(ai * bi);
        if iabs(ai * bi) < 140737488322560 {
            let n = ai * bi;
            assert(n >= 0 ==> 0 <= (n + 32768) / 65536 && (n + 32768) / 65536 <= 2147483647) by(nonlinear_arith)
                requires n < 140737488322560;
            assert(n < 0 ==> 0 <= (-n + 32768) / 65536 && (-n + 32768) / 65536 <= 2147483647) by(nonlinear_arith)
                requires -n < 140737488322560;
            lemma_trunc_i32_id(round_half_away(n, 65536));
        }
    }
    r32 as i64
}

/// `FT_DivFix` (`ftcalc.c`): rounded, sign moved out, `0x7FFFFFFF` when dividing by zero.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == div_fix_spec(a, b),
        b == 0 ==> r == (if a < 0 { -0x7FFF_FFFFi64 } else { 0x7FFF_FFFFi64 }),
        // On integers, for b != 0 and |a| < 2^47 (nothing wraps): |r| == (|a|*65536 + |b|/2) / |b|, negative
        // exactly when a and b have opposite signs (a == 0 gives 0 either way).
        b != 0 && iabs(a as int) < 140737488355328
            ==> r as int == signed_rounded_quot((a < 0) != (b < 0), iabs(a as int) * 65536, iabs(b as int)),
))]
#[allow(clippy::manual_checked_ops)] // proved as written (Verus); `checked_div` would change the shipped proof
pub fn div_fix(a: i64, b: i64) -> i64 {
    let negative = (a < 0) != (b < 0);
    let au = if a < 0 {
        0i64.wrapping_sub(a) as u64
    } else {
        a as u64
    };
    let bu = if b < 0 {
        0i64.wrapping_sub(b) as u64
    } else {
        b as u64
    };
    // `wrapping_*` only makes explicit the wrap the plain `<<`/`+` already perform; behaviour is identical.
    let q = if bu > 0 {
        au.wrapping_shl(16).wrapping_add(bu >> 1) / bu
    } else {
        0x7FFF_FFFF
    };
    #[cfg_attr(verus_keep_ghost, verifier::truncate)]
    let q = q as i64;
    let r = if negative { 0i64.wrapping_sub(q) } else { q };
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(r == div_fix_spec(a, b));
        lemma_neg_abs(a);
        lemma_neg_abs(b);
        if b != 0 && iabs(a as int) < 140737488355328 {
            let n = iabs(a as int) * 65536;
            let d = iabs(b as int);
            assert(au as int == iabs(a as int) && bu as int == d);
            assert(au < 0x800000000000 ==> au.wrapping_shl(16) == au * 65536) by(bit_vector);
            assert(bu >> 1 == bu / 2) by(bit_vector);
            let qi = (n + d / 2) / d;
            assert(0 <= qi && qi <= n) by(nonlinear_arith)
                requires 0 <= n, 1 <= d, qi == (n + d / 2) / d;
            assert(n <= 9223372036854710272);
        }
    }
    r
}

/// `FT_MulDiv` (`ftcalc.c`): `a * b / c`, rounded, `0x7FFFFFFF` when `c` is zero.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == mul_div_spec(a, b, c),
        c == 0 ==> r == (if (a < 0) != (b < 0) { -0x7FFF_FFFFi64 } else { 0x7FFF_FFFFi64 }),
        // On integers, for c != 0 and |a|, |b| < 2^62 (the 128-bit product never wraps), while the quotient fits in
        // i64: |r| == (|a|*|b| + |c|/2) / |c|, negative exactly when an odd number of a, b, c are negative.
        c != 0 && iabs(a as int) < 4611686018427387904 && iabs(b as int) < 4611686018427387904
            && (iabs(a as int) * iabs(b as int) + iabs(c as int) / 2) / iabs(c as int) <= 9223372036854775807
            ==> r as int == signed_rounded_quot(((a < 0) != (b < 0)) != (c < 0), iabs(a as int) * iabs(b as int),
                iabs(c as int)),
))]
#[allow(clippy::manual_checked_ops)] // proved as written (Verus); `checked_div` would change the shipped proof
pub fn mul_div(a: i64, b: i64, c: i64) -> i64 {
    let negative = ((a < 0) != (b < 0)) != (c < 0);
    let au = if a < 0 {
        (0i64.wrapping_sub(a) as u64) as u128
    } else {
        a as u128
    };
    let bu = if b < 0 {
        (0i64.wrapping_sub(b) as u64) as u128
    } else {
        b as u128
    };
    let cu = if c < 0 {
        (0i64.wrapping_sub(c) as u64) as u128
    } else {
        c as u128
    };
    let d = if cu > 0 {
        au.wrapping_mul(bu).wrapping_add(cu >> 1) / cu
    } else {
        0x7FFF_FFFF
    };
    #[cfg_attr(verus_keep_ghost, verifier::truncate)]
    let d = d as i64;
    let r = if negative { 0i64.wrapping_sub(d) } else { d };
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(r == mul_div_spec(a, b, c));
        lemma_neg_abs(a);
        lemma_neg_abs(b);
        lemma_neg_abs(c);
        if c != 0 && iabs(a as int) < 4611686018427387904 && iabs(b as int) < 4611686018427387904 {
            assert(au as int == iabs(a as int) && bu as int == iabs(b as int) && cu as int == iabs(c as int));
            assert(au as int * bu as int <= 0x4000000000000000 * 0x4000000000000000) by(nonlinear_arith)
                requires au < 0x4000000000000000, bu < 0x4000000000000000;
            assert(cu >> 1 == cu / 2) by(bit_vector);
            assert(au.wrapping_mul(bu).wrapping_add(cu / 2) as int == au as int * bu as int + cu as int / 2);
        }
    }
    r
}

/// `FT_PIX_ROUND`: to the nearest whole pixel, halves up.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires -1152921504606846976 <= x && x <= 1152921504606846976,
    ensures
        (r as int) % 64 == 0,
        r - x <= 32 && x - r <= 32,
))]
pub fn pix_round(x: F26Dot6) -> F26Dot6 {
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(((x + 32) as i64) & !63i64 == ((x + 32) as i64) - ((x + 32) as i64) % 64) by(bit_vector);
    }
    (x + 32) & !63
}

/// `FT_PIX_FLOOR`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires -1152921504606846976 <= x && x <= 1152921504606846976,
    ensures
        (r as int) % 64 == 0,
        r <= x && x < r + 64,
))]
pub fn pix_floor(x: F26Dot6) -> F26Dot6 {
    #[cfg(verus_keep_ghost)]
    proof! {
        assert((x & !63i64) == x - x % 64) by(bit_vector);
    }
    x & !63
}

/// `FT_PIX_CEIL`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires -1152921504606846976 <= x && x <= 1152921504606846976,
    ensures
        (r as int) % 64 == 0,
        x <= r && r < x + 64,
))]
pub fn pix_ceil(x: F26Dot6) -> F26Dot6 {
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(((x + 63) as i64) & !63i64 == ((x + 63) as i64) - ((x + 63) as i64) % 64) by(bit_vector);
    }
    (x + 63) & !63
}

/// What Servo and WebRender pass to `FT_Set_Char_Size` for a size in pixels: `(px * 64.0 + 0.5) as FT_F26Dot6`.
/// Sizes Rust cannot represent become 0 (the cast saturates; NaN is 0), which [`scale_for_char_size`] rejects.
// Trusted, not proved (`external_body`): this is float arithmetic — `px * 64.0 + 0.5` and the saturating `as i64`
// cast. Verus does not model IEEE floats, so the cast semantics are trusted to match C's, which is what Servo did.
#[cfg_attr(verus_keep_ghost, verifier::external_body)]
pub fn char_size(px: f64) -> F26Dot6 {
    (px * 64.0 + 0.5) as i64
}

/// The scale `FT_Request_Metrics` sets for a nominal size request (`FT_Set_Char_Size` at 72 dpi): design units to
/// 26.6 pixels, `FT_DivFix(char_size, units_per_em)`. `None` where FreeType fails the request: no size, no
/// `units_per_EM`, or a ppem above `FT_USHORT_MAX`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires char_size <= 9223372036854775743,
    ensures
        r matches Some(s) ==> char_size > 0 && units_per_em > 0 && ppem_spec(char_size) <= 65535 && s > 0,
        r is None ==> !(char_size > 0 && units_per_em > 0 && ppem_spec(char_size) <= 65535),
))]
pub fn scale_for_char_size(char_size: F26Dot6, units_per_em: u16) -> Option<Fixed> {
    if char_size <= 0 || units_per_em == 0 {
        return None;
    }
    if ppem(char_size) > u16::MAX as i64 {
        return None;
    }
    let s = div_fix(char_size, units_per_em as i64);
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(ppem_spec(char_size) <= 65535 ==> char_size <= 4194303);
        assert(char_size > 0 && units_per_em > 0 && char_size <= 4194303
            ==> div_fix_spec(char_size, units_per_em as i64) > 0) by(bit_vector);
        assert(s == div_fix_spec(char_size, units_per_em as i64));
    }
    Some(s)
}

/// `FT_Size_Metrics::y_ppem` for a nominal request: `(char_size + 32) >> 6`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires char_size <= 9223372036854775743,
    ensures r == ppem_spec(char_size),
))]
pub fn ppem(char_size: F26Dot6) -> i64 {
    // `(char_size + 32) >> 6` written as floor division: identical for every i64 (arithmetic shift right by 6
    // is floor division by 64).
    let t = char_size + 32;
    let q = t / 64;
    let r = if t >= 0 || t % 64 == 0 { q } else { q - 1 };
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(r as int == ppem_spec(char_size));
    }
    r
}

/// The size skrifa draws at so that its scale equals FreeType's: `scale * units_per_em` in pixels.
/// FreeType keeps the scale, not the size, so this is the size the scale stands for.
// Trusted, not proved (`external_body`): float arithmetic (`as f64`, `*`, `/`, `as f32`). Verus does not model IEEE
// floats, so the rounding is trusted to match the C expression skrifa's caller used.
#[cfg_attr(verus_keep_ghost, verifier::external_body)]
pub fn ppem_f32_for_scale(scale: Fixed, units_per_em: u16) -> f32 {
    (scale as f64 * units_per_em as f64 / 65536.0 / 64.0) as f32
}

/// The `FT_FaceRec` fields `sfnt_load_face` (`sfobjs.c`) fills from hhea, OS/2 and post, in design units.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FaceMetrics {
    pub ascender: i16,
    pub descender: i16,
    pub height: i16,
    pub max_advance_width: i16,
    pub underline_position: i16,
    pub underline_thickness: i16,
}

/// The tables `sfnt_load_face` reads. `os2` is `None` when the font has no OS/2 table (FreeType's `version ==
/// 0xFFFF`).
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FaceTables {
    pub hhea_ascender: i16,
    pub hhea_descender: i16,
    pub hhea_line_gap: i16,
    pub hhea_advance_width_max: u16,
    pub os2: Option<Os2Metrics>,
    pub post_underline_position: i16,
    pub post_underline_thickness: i16,
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Os2Metrics {
    pub typo_ascender: i16,
    pub typo_descender: i16,
    pub typo_line_gap: i16,
    pub win_ascent: u16,
    pub win_descent: u16,
}

/// `sfnt_load_face` (`sfobjs.c`): hhea first; OS/2 typo values when hhea's ascender and descender are both zero;
/// OS/2 win values when the typo ones are zero too. The underline position moves from the stroke's top edge to its
/// centre. FreeType does this arithmetic in `FT_Short`, so it wraps the same way.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == face_metrics_spec(*t),
))]
pub fn face_metrics(t: &FaceTables) -> FaceMetrics {
    let mut ascender = t.hhea_ascender;
    let mut descender = t.hhea_descender;
    let mut height = ascender
        .wrapping_sub(descender)
        .wrapping_add(t.hhea_line_gap);
    if ascender == 0 && descender == 0 {
        if let Some(os2) = t.os2 {
            if os2.typo_ascender != 0 || os2.typo_descender != 0 {
                ascender = os2.typo_ascender;
                descender = os2.typo_descender;
                height = ascender
                    .wrapping_sub(descender)
                    .wrapping_add(os2.typo_line_gap);
            } else {
                ascender = os2.win_ascent as i16;
                descender = 0i16.wrapping_sub(os2.win_descent as i16);
                height = ascender.wrapping_sub(descender);
            }
        }
    }
    FaceMetrics {
        ascender,
        descender,
        height,
        max_advance_width: t.hhea_advance_width_max as i16,
        underline_position: t
            .post_underline_position
            .wrapping_sub(t.post_underline_thickness / 2),
        underline_thickness: t.post_underline_thickness,
    }
}

/// The MVAR deltas `tt_apply_mvar` (`ttgxvar.c`) applies to the face fields, already rounded to design units the way
/// FreeType does (`FT_fixedToInt`, see [`fixed_to_int`]).
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MvarDeltas {
    pub hasc: i16,
    pub hdsc: i16,
    pub hlgp: i16,
    pub undo: i16,
    pub unds: i16,
}

/// `tt_apply_mvar`: keeps the line gap the face had, moves ascender and descender, and recomputes height and the
/// underline from the varied post values.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == apply_mvar_spec(m, *t, *d),
))]
pub fn apply_mvar(m: FaceMetrics, t: &FaceTables, d: &MvarDeltas) -> FaceMetrics {
    let current_line_gap = m.height.wrapping_sub(m.ascender).wrapping_add(m.descender);
    let ascender = m.ascender.wrapping_add(d.hasc);
    let descender = m.descender.wrapping_add(d.hdsc);
    let underline_position = t.post_underline_position.wrapping_add(d.undo);
    let underline_thickness = t.post_underline_thickness.wrapping_add(d.unds);
    FaceMetrics {
        ascender,
        descender,
        height: ascender
            .wrapping_sub(descender)
            .wrapping_add(current_line_gap)
            .wrapping_add(d.hlgp),
        max_advance_width: m.max_advance_width,
        underline_position: underline_position.wrapping_sub(underline_thickness / 2),
        underline_thickness,
    }
}

/// `FT_fixedToInt`: a 16.16 value to the nearest integer, halves up, as `FT_Short`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == (((x as u32).wrapping_add(0x8000) >> 16) as u16 as i16),
))]
pub fn fixed_to_int(x: Fixed) -> i16 {
    #[cfg_attr(verus_keep_ghost, verifier::truncate)]
    let t = (x as u32).wrapping_add(0x8000);
    #[cfg_attr(verus_keep_ghost, verifier::truncate)]
    let r = (t >> 16) as u16;
    #[cfg_attr(verus_keep_ghost, verifier::truncate)]
    let r16 = r as i16;
    r16
}

/// Which hinting a glyph load asks for, as far as advances and outlines are concerned.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hinting {
    /// `FT_LOAD_NO_HINTING`.
    None,
    /// `FT_LOAD_TARGET_LIGHT`: the autohinter for TrueType outlines, the native light hinter for CFF; both round the
    /// advance to whole pixels and leave x alone.
    Light,
}

/// `slot->metrics.horiAdvance` after `FT_Load_Glyph`, in 26.6, for an outline glyph whose advance is
/// `advance_units` design units (HVAR applied and rounded): scaled with `FT_MulFix`, then rounded to whole pixels
/// when hinted (`af_loader_load_glyph` and `ft_glyphslot_grid_fit_metrics` both use `FT_PIX_ROUND`).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        hinting == Hinting::None ==> r == mul_fix_spec(advance_units, x_scale),
        hinting == Hinting::Light ==>
            (r as int) % 64 == 0
            && r - mul_fix_spec(advance_units, x_scale) <= 32
            && mul_fix_spec(advance_units, x_scale) - r <= 32,
))]
pub fn glyph_advance(advance_units: i64, x_scale: Fixed, hinting: Hinting) -> F26Dot6 {
    let scaled = mul_fix(advance_units, x_scale);
    match hinting {
        Hinting::None => scaled,
        Hinting::Light => pix_round(scaled),
    }
}

/// The outline shift `mozilla_glyphslot_embolden_less` uses for synthetic bold: half of `FT_GlyphSlot_Embolden`'s,
/// `FT_MulFix(units_per_EM, y_scale) / 48` (C division, toward zero). The glyph's advance, width, height and
/// `horiBearingY` each grow by this much.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires 0 <= y_scale && y_scale <= 2147483647,
    ensures
        (r as int) == tdiv(mul_fix_spec(units_per_em as i64, y_scale) as int, 48),
        y_scale >= 0 ==> r >= 0,
))]
pub fn embolden_strength(units_per_em: u16, y_scale: Fixed) -> F26Dot6 {
    let r = mul_fix(units_per_em as i64, y_scale) / 48;
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(y_scale >= 0 ==> r >= 0);
    }
    r
}

/// The strike `FreeTypeFace::set_size` (Servo) and `choose_bitmap_size` (WebRender) pick for a bitmap-only font:
/// strike sizes are `y_ppem` in 26.6; a larger strike beats any smaller one, otherwise the closest wins, and later
/// strikes win ties. `None` without strikes.
// The `requires` bounds say what FreeType can produce: 26.6 strike sizes and requests are ppem values, at most
// 255 * 64. They keep `dist.abs()` from overflowing on `i64::MIN`. The last `ensures` says no later strike would
// have replaced the chosen one under the update condition.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        -16320 <= requested && requested <= 16320,
        forall |i: int| 0 <= i && i < strike_y_ppem@.len() ==> -16320 <= strike_y_ppem@[i] && strike_y_ppem@[i] <= 16320,
    ensures
        choose_strike_ok(strike_y_ppem, requested, r),
))]
pub fn choose_strike(strike_y_ppem: &[F26Dot6], requested: F26Dot6) -> Option<usize> {
    let first = *strike_y_ppem.first()?;
    let mut best = 0;
    let mut best_dist = first - requested;
    let mut i = 1;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            1 <= i && i <= strike_y_ppem@.len(),
            0 <= best && best < i,
            -16320 <= requested && requested <= 16320,
            forall |j: int| 0 <= j && j < strike_y_ppem@.len() ==> -16320 <= strike_y_ppem@[j] && strike_y_ppem@[j] <= 16320,
            best_dist == (strike_y_ppem@[best as int] - requested) as int,
            forall |j: int| best < j && j < i ==> !strike_update(best_dist as int, (strike_y_ppem@[j] - requested) as int),
        decreases strike_y_ppem@.len() - i,
    ))]
    while i < strike_y_ppem.len() {
        let dist = strike_y_ppem[i] - requested;
        if (best_dist < 0 && dist >= best_dist)
            || (if dist < 0 {
                0i64.wrapping_sub(dist)
            } else {
                dist
            }) <= best_dist
        {
            best_dist = dist;
            best = i;
        }
        i += 1;
    }
    Some(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_bitmap_matches_freetype() {
        // The wr_glyph_rasterizer cases: a 10.5 x 10 px box at (0.25, 0).
        let (a, b, c, d) = (16, 0, 16 + 672, 640);
        assert_eq!(
            preset_bitmap(a, b, c, d, PresetMode::Gray),
            Some((0, 10, 11, 10))
        );
        assert_eq!(
            preset_bitmap(a, b, c, d, PresetMode::Lcd),
            Some((-1, 10, 39, 10))
        );
        assert_eq!(
            preset_bitmap(a, b, c, d, PresetMode::LcdV),
            Some((0, 11, 11, 36))
        );
        assert_eq!(
            preset_bitmap(a, b, c, d, PresetMode::Mono),
            Some((0, 10, 11, 10))
        );
        assert_eq!(preset_bitmap(0, 0, 0x8000 << 6, 64, PresetMode::Gray), None);
        // An empty box: Mono still gets one pixel, the others none.
        assert_eq!(
            preset_bitmap(0, 0, 0, 0, PresetMode::Mono),
            Some((0, 1, 1, 1))
        );
        assert_eq!(
            preset_bitmap(0, 0, 0, 0, PresetMode::Gray),
            Some((0, 0, 0, 0))
        );
        // Hostile extremes: None, no overflow.
        assert_eq!(
            preset_bitmap(i64::MIN, i64::MIN, i64::MAX, i64::MAX, PresetMode::Lcd),
            None
        );
        assert_eq!(
            preset_bitmap(i64::MAX - 1, 0, i64::MAX, 64, PresetMode::Mono),
            None
        );
    }

    #[test]
    fn mul_fix_rounds_like_freetype() {
        assert_eq!(mul_fix(2048, 0x10000), 2048);
        assert_eq!(mul_fix(1, 0x8000), 1); // 0.5 rounds up
        assert_eq!(mul_fix(-1, 0x8000), -1); // -0.5 rounds away from zero
        assert_eq!(mul_fix(-3, 0x8000), -2); // -1.5 -> -2
        assert_eq!(mul_fix(1000, div_fix(16 * 64, 2048)), 500);
    }

    #[test]
    fn div_fix_by_zero_is_max() {
        assert_eq!(div_fix(5, 0), 0x7FFF_FFFF);
        assert_eq!(div_fix(-5, 0), -0x7FFF_FFFF);
        assert_eq!(div_fix(1024, 2048), 0x8000);
    }

    #[test]
    fn char_size_and_scale() {
        assert_eq!(char_size(13.333), 853);
        assert_eq!(ppem(853), 13);
        assert_eq!(scale_for_char_size(16 * 64, 2048), Some(0x8000));
        assert_eq!(scale_for_char_size(0, 2048), None);
        assert_eq!(scale_for_char_size(64, 0), None);
        assert_eq!(char_size(f64::NAN), 0);
    }

    #[test]
    fn face_metrics_fallbacks() {
        let mut t = FaceTables {
            hhea_ascender: 0,
            hhea_descender: 0,
            hhea_line_gap: 10,
            os2: Some(Os2Metrics {
                typo_ascender: 800,
                typo_descender: -200,
                typo_line_gap: 90,
                win_ascent: 900,
                win_descent: 300,
            }),
            post_underline_position: -100,
            post_underline_thickness: 50,
            ..Default::default()
        };
        let m = face_metrics(&t);
        assert_eq!((m.ascender, m.descender, m.height), (800, -200, 1090));
        assert_eq!(m.underline_position, -125);
        t.os2.as_mut().unwrap().typo_ascender = 0;
        t.os2.as_mut().unwrap().typo_descender = 0;
        let m = face_metrics(&t);
        assert_eq!((m.ascender, m.descender, m.height), (900, -300, 1200));
        t.hhea_ascender = 1000;
        let m = face_metrics(&t);
        assert_eq!((m.ascender, m.descender, m.height), (1000, 0, 1010));
    }

    #[test]
    fn hinted_advance_is_whole_pixels() {
        let scale = scale_for_char_size(char_size(13.333), 2048).unwrap();
        let a = glyph_advance(1233, scale, Hinting::Light);
        assert_eq!(a % 64, 0);
        assert!((glyph_advance(1233, scale, Hinting::None) - a).abs() <= 32);
    }

    #[test]
    fn strike_choice_prefers_larger() {
        assert_eq!(choose_strike(&[], 64), None);
        assert_eq!(choose_strike(&[109 * 64, 136 * 64], 20 * 64), Some(0));
        assert_eq!(choose_strike(&[16 * 64, 109 * 64], 20 * 64), Some(1));
        assert_eq!(choose_strike(&[16 * 64, 18 * 64], 20 * 64), Some(1));
    }
}

/// The bitmap modes `ft_glyphslot_preset_bitmap` tells apart (`FT_RENDER_MODE_*`; BGRA is sized as gray).
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresetMode {
    Mono,
    Gray,
    Lcd,
    LcdV,
}

#[cfg(verus_keep_ghost)]
verus! {

/// The cbox coordinates `preset_bitmap` is proved for (26.6, about ±2^34 pixels).
pub open spec fn preset_coord_ok(x: i64) -> bool { -0x100_0000_0000 <= x && x <= 0x100_0000_0000 }

/// One axis of the Mono pixel box: rounded to pixel centres, then widened to one pixel when empty.
pub open spec fn preset_mono_spec(min: i64, max: i64) -> (i64, i64) {
    let lo = (min + 31) as i64 >> 6;
    let hi = (max + 32) as i64 >> 6;
    if lo == hi {
        if ((min + 31) as i64 & 63) - 31 + ((max + 32) as i64 & 63) - 32 < 0 { ((lo - 1) as i64, hi) }
        else { (lo, (hi + 1) as i64) }
    } else {
        (lo, hi)
    }
}

/// One axis of the anti-aliased pixel box, `pad` (1/64 px) of LCD padding on each side: floor of the min, ceiling of
/// the max.
pub open spec fn preset_aa_spec(min: i64, max: i64, pad: i64) -> (i64, i64) {
    ((min - pad) as i64 >> 6, (max + pad + 63) as i64 >> 6)
}

/// FreeType's pixel box `(x_min, y_min, x_max, y_max)` for a cbox (LCD: the Harmony subpixels at -21, 0, +21 / 64 px
/// pad the x axis, LCD_V the y axis).
pub open spec fn preset_pbox_spec(x_min: i64, y_min: i64, x_max: i64, y_max: i64, mode: PresetMode)
    -> (i64, i64, i64, i64)
{
    let (xa, xb, ya, yb) = match mode {
        PresetMode::Mono => {
            let x = preset_mono_spec(x_min, x_max);
            let y = preset_mono_spec(y_min, y_max);
            (x.0, x.1, y.0, y.1)
        }
        _ => {
            let x = preset_aa_spec(x_min, x_max, if mode == PresetMode::Lcd { 21 } else { 0 });
            let y = preset_aa_spec(y_min, y_max, if mode == PresetMode::LcdV { 21 } else { 0 });
            (x.0, x.1, y.0, y.1)
        }
    };
    (xa, ya, xb, yb)
}

/// FreeType's range check: the pixel box must fit in 16 bits, or rendering fails with `Raster_Overflow`.
pub open spec fn preset_in_range(p: (i64, i64, i64, i64)) -> bool {
    -0x8000 <= p.0 && p.2 <= 0x7FFF && -0x8000 <= p.1 && p.3 <= 0x7FFF
}

} // verus!

/// One axis of `ft_glyphslot_preset_bitmap`'s Mono rounding (`min`, `max` in 26.6).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires preset_coord_ok(min), preset_coord_ok(max), min <= max,
    ensures r == preset_mono_spec(min, max), r.0 <= r.1,
        -0x4_0000_0001 <= r.0 && r.1 <= 0x4_0000_0001,
))]
fn preset_axis_mono(min: F26Dot6, max: F26Dot6) -> (i64, i64) {
    let c_min = min & 63;
    let c_max = max & 63;
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(-0x100_0000_0000 <= min && min <= 0x100_0000_0000 && -0x100_0000_0000 <= max && max <= 0x100_0000_0000
            && min <= max ==> {
            &&& 0 <= min & 63 && min & 63 < 64 && 0 <= max & 63 && max & 63 < 64
            &&& (min >> 6) + (((min & 63) + 31) as i64 >> 6) == (min + 31) as i64 >> 6
            &&& (max >> 6) + (((max & 63) + 32) as i64 >> 6) == (max + 32) as i64 >> 6
            &&& (((min & 63) + 31) as i64 & 63) == ((min + 31) as i64 & 63)
            &&& (((max & 63) + 32) as i64 & 63) == ((max + 32) as i64 & 63)
            &&& (min + 31) as i64 >> 6 <= (max + 32) as i64 >> 6
            &&& -0x4_0000_0000 <= (min + 31) as i64 >> 6 && (max + 32) as i64 >> 6 <= 0x4_0000_0000
            &&& -0x4_0000_0000 <= min >> 6 && max >> 6 <= 0x4_0000_0000
        }) by(bit_vector);
    }
    #[cfg(verus_keep_ghost)]
    proof! {
        let a = (c_min + 31) as i64;
        let b = (c_max + 32) as i64;
        assert(0 <= a & 63 && a & 63 < 64 && 0 <= b & 63 && b & 63 < 64) by(bit_vector);
    }
    let mut lo = (min >> 6) + ((c_min + 31) >> 6);
    let mut hi = (max >> 6) + ((c_max + 32) >> 6);
    if lo == hi {
        if ((c_min + 31) & 63) - 31 + ((c_max + 32) & 63) - 32 < 0 {
            lo -= 1;
        } else {
            hi += 1;
        }
    }
    (lo, hi)
}

/// One axis of `ft_glyphslot_preset_bitmap` for the anti-aliased modes: `pad` (1/64 px) of LCD padding subtracted
/// from `min` and added to `max` before taking the floor and the ceiling.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires preset_coord_ok(min), preset_coord_ok(max), min <= max, pad == 0 || pad == 21,
    ensures r == preset_aa_spec(min, max, pad), r.0 <= r.1,
        -0x4_0000_0001 <= r.0 && r.1 <= 0x4_0000_0002,
))]
fn preset_axis_aa(min: F26Dot6, max: F26Dot6, pad: F26Dot6) -> (i64, i64) {
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(-0x100_0000_0000 <= min && min <= 0x100_0000_0000 && -0x100_0000_0000 <= max && max <= 0x100_0000_0000
            && min <= max && 0 <= pad && pad <= 21 ==> {
            &&& 0 <= min & 63 && min & 63 < 64 && 0 <= max & 63 && max & 63 < 64
            &&& (min >> 6) + (((min & 63) - pad) as i64 >> 6) == (min - pad) as i64 >> 6
            &&& (max >> 6) + (((max & 63) + pad + 63) as i64 >> 6) == (max + pad + 63) as i64 >> 6
            &&& (min - pad) as i64 >> 6 <= (max + pad + 63) as i64 >> 6
            &&& -0x4_0000_0001 <= (min - pad) as i64 >> 6 && (max + pad + 63) as i64 >> 6 <= 0x4_0000_0002
            &&& -0x4_0000_0000 <= min >> 6 && max >> 6 <= 0x4_0000_0000
        }) by(bit_vector);
    }
    let c_min = (min & 63) - pad;
    let c_max = (max & 63) + pad;
    ((min >> 6) + (c_min >> 6), (max >> 6) + ((c_max + 63) >> 6))
}

/// `ft_glyphslot_preset_bitmap` (`ftobjs.c`) for an outline: the bitmap's pixel box `(left, top, width, rows)` for
/// `mode`, with `ft_lcd_padding` for the Harmony LCD geometry (`ft_smooth_init`: subpixels at -21, 0 and +21 / 64 px).
/// `width` and `rows` count `FT_Bitmap` units (tripled for LCD and LCD_V). `None` for a box beyond FreeType's 16-bit
/// limits, where rendering fails with `Raster_Overflow`. The cbox is `(x_min, y_min, x_max, y_max)` in 26.6.
///
/// Proved for every cbox with min <= max (any `FT_Outline_Get_CBox`): no overflow; for coordinates in [-2^40, 2^40],
/// `None` exactly when FreeType's range check fails, and `None` outside that domain (where the pixel box is past
/// ±2^34 pixels, so FreeType's check fails too); a `Some` box is at most 3 * 0x10000 units each way, so the BGRA
/// buffer the caller allocates, `width * rows * 4` bytes, is below 2^36.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires x_min <= x_max, y_min <= y_max,
    ensures
        preset_coord_ok(x_min) && preset_coord_ok(y_min) && preset_coord_ok(x_max) && preset_coord_ok(y_max)
            ==> (r is None <==> !preset_in_range(preset_pbox_spec(x_min, y_min, x_max, y_max, mode))),
        !(preset_coord_ok(x_min) && preset_coord_ok(y_min) && preset_coord_ok(x_max) && preset_coord_ok(y_max))
            ==> r is None,
        r matches Some((left, top, width, rows)) ==> {
            let p = preset_pbox_spec(x_min, y_min, x_max, y_max, mode);
            &&& left == p.0 && top == p.3
            &&& width == (p.2 - p.0) * (if mode == PresetMode::Lcd { 3int } else { 1int })
            &&& rows == (p.3 - p.1) * (if mode == PresetMode::LcdV { 3int } else { 1int })
            &&& 0 <= width && width <= 3 * 0x10000 && 0 <= rows && rows <= 3 * 0x10000
            &&& (width as int) * (rows as int) * 4 < 0x10_0000_0000
        },
))]
pub fn preset_bitmap(
    x_min: F26Dot6,
    y_min: F26Dot6,
    x_max: F26Dot6,
    y_max: F26Dot6,
    mode: PresetMode,
) -> Option<(i64, i64, i64, i64)> {
    // Past ±2^40 (±2^34 px) the pixel box is far outside FreeType's 16-bit range, so this is the same `None` the
    // range check below gives (min <= max: a too-small min fails `-0x8000 <=`, a too-large max fails `<= 0x7FFF`);
    // it only keeps the arithmetic below away from i64's ends.
    const LIMIT: i64 = 0x100_0000_0000;
    if x_min < -LIMIT || x_max > LIMIT || y_min < -LIMIT || y_max > LIMIT {
        return None;
    }
    let ((px_min, px_max), (py_min, py_max)) = match mode {
        PresetMode::Mono => (
            preset_axis_mono(x_min, x_max),
            preset_axis_mono(y_min, y_max),
        ),
        // The LCD padding is max/min of the subpixel offsets: x [-21, 0, 21] for LCD, y the same for LCD_V.
        PresetMode::Gray => (
            preset_axis_aa(x_min, x_max, 0),
            preset_axis_aa(y_min, y_max, 0),
        ),
        PresetMode::Lcd => (
            preset_axis_aa(x_min, x_max, 21),
            preset_axis_aa(y_min, y_max, 0),
        ),
        PresetMode::LcdV => (
            preset_axis_aa(x_min, x_max, 0),
            preset_axis_aa(y_min, y_max, 21),
        ),
    };
    let mut width = px_max - px_min;
    let mut rows = py_max - py_min;
    match mode {
        PresetMode::Lcd => width *= 3,
        PresetMode::LcdV => rows *= 3,
        _ => {}
    }
    if px_min < -0x8000 || px_max > 0x7FFF || py_min < -0x8000 || py_max > 0x7FFF {
        return None;
    }
    #[cfg(verus_keep_ghost)]
    proof! {
        assert((width as int) * (rows as int) * 4 < 0x10_0000_0000) by(nonlinear_arith)
            requires 0 <= width && width <= 3 * 0x10000 && 0 <= rows && rows <= 3 * 0x10000,
                (width <= 0x10000 || rows <= 0x10000);
    }
    Some((px_min, py_max, width, rows))
}
