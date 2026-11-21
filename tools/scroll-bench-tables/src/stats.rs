//! The four statistics the findings tables take over frame values.
//!
//! They are the Python's, not the usual ones. `percentile` is the rule `scrollbench::percentile` uses
//! (`crates/lucinanet-ui/src/scrollbench.rs`), which the analysis re-implements because it wants a
//! `None` on an empty list rather than a panic; `median` is `statistics.median` over the non-`None`
//! values; and `mean` is `statistics.mean`, which sums exactly and divides once, so it is a different
//! double than the one you get by adding the values into a float.

use std::cmp::Ordering;

/// `sorted(v for v in values if v is not None)`.
pub fn sorted(values: &[f64]) -> Vec<f64> {
    let mut s: Vec<f64> = values.iter().copied().collect();
    s.sort_by(f64::total_cmp);
    s
}

/// The same linear-between-nearest rule as `scrollbench::percentile`, with the analysis' own two
/// shortcuts: nothing at all for an empty list, and the single value back for a list of one.
pub fn percentile(values: &[f64], p: f64) -> Option<f64> {
    let s = sorted(values);
    let n = s.len();
    if n == 0 {
        return None;
    }
    if n == 1 {
        return Some(s[0]);
    }
    let x = (n - 1) as f64 * p;
    let lo = x as usize;
    let hi = (lo + 1).min(n - 1);
    Some(s[lo] + (s[hi] - s[lo]) * (x - lo as f64))
}

/// `statistics.median` of the values, or `None` of no values: the mean of the middle two, never the
/// mean of everything.
pub fn median(values: &[f64]) -> Option<f64> {
    let s = sorted(values);
    let n = s.len();
    if n == 0 {
        return None;
    }
    let mid = n / 2;
    if n % 2 == 1 {
        Some(s[mid])
    } else {
        Some((s[mid - 1] + s[mid]) / 2.0)
    }
}

/// `statistics.mean`: the exact sum of the values divided by their count and rounded once.
///
/// The order the values are added in does not matter here, which is what makes the difference from a
/// float loop visible: `mean` of the tile areas of a run is printed to one decimal, and a sum that
/// drifts by an ulp can print a different digit. Where the exact sum does not fit in 128 bits the
/// naive one is used instead, which is the same answer to within an ulp of the largest value.
pub fn mean(values: &[f64]) -> Option<f64> {
    let n = values.len();
    if n == 0 {
        return None;
    }
    if let Some((mantissa, exp2)) = exact_sum(values) {
        if let Some(v) = ratio(mantissa, exp2, n as u64) {
            return Some(v);
        }
    }
    let mut sum = 0.0;
    for &v in values {
        sum += v;
    }
    Some(sum / n as f64)
}

/// The values as `mantissa * 2^exp2`, exactly, or `None` if they do not add up inside 128 bits.
fn exact_sum(values: &[f64]) -> Option<(u128, i32)> {
    let mut parts = Vec::with_capacity(values.len());
    let (mut low, mut high) = (i32::MAX, i32::MIN);
    for &v in values {
        let (m, e) = decompose(v)?;
        if m != 0 {
            low = low.min(e);
            high = high.max(e);
        }
        parts.push((m, e));
    }
    if high < low {
        return Some((0, 0)); // every value zero, so the sum needs no scale
    }
    let mut sum = 0u128;
    for (m, e) in parts {
        let shift = (e - low) as u32;
        // A left shift loses the bits that come out above the top of the word, which is what `checked_shl`
        // does not report: it answers None only for a shift of 128 or more.
        if shift >= 128 || m > (u128::MAX >> shift) {
            return None;
        }
        sum = sum.checked_add(m << shift)?;
    }
    Some((sum, low))
}

/// `|v| = mantissa * 2^exp2` with the hidden bit in, for a non-negative finite double.
fn decompose(v: f64) -> Option<(u128, i32)> {
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    if v == 0.0 {
        return Some((0, 0));
    }
    let bits = v.to_bits();
    let fraction = bits & 0xf_ffff_ffff_ffff;
    let biased = (bits >> 52) as i32;
    Some(if biased == 0 {
        (fraction as u128, -1074)
    } else {
        ((fraction | (1 << 52)) as u128, biased - 1075)
    })
}

/// The nearest double to `mantissa * 2^exp2 / d`, ties to even, from the exact rational.
///
/// The quotient is brought to 53 significant bits by doubling the denominator - an exact change of
/// scale, with the exponent tracked - so that the whole question is whether the leftover fraction
/// reaches a half, which is one comparison against the denominator.
fn ratio(mantissa: u128, exp2: i32, d: u64) -> Option<f64> {
    if mantissa == 0 {
        return Some(0.0);
    }
    let mut num = mantissa;
    let mut den = u128::from(d);
    let mut e = 0i32;
    while bitlen(num / den) > 53 {
        den = den.checked_mul(2)?;
        e += 1;
        if e > 110 {
            return None;
        }
    }
    while bitlen(num / den) < 53 {
        num = num.checked_mul(2)?;
        e -= 1;
        if e < -110 {
            return None;
        }
    }
    let q = num / den;
    if bitlen(q) != 53 {
        return None;
    }
    let r = num - q * den;
    let mut m = q;
    match r.checked_mul(2)?.cmp(&den) {
        Ordering::Greater => m += 1,
        Ordering::Equal => m += m & 1,
        Ordering::Less => {}
    }
    if bitlen(m) > 53 {
        m >>= 1;
        e += 1;
    }
    let exponent = e + exp2;
    if !(-1000..=1000).contains(&exponent) {
        return None;
    }
    // Both factors are exact, so the one rounding the product takes is the rounding of the rational.
    Some(m as f64 * 2f64.powi(exponent))
}

fn bitlen(x: u128) -> u32 {
    128 - x.leading_zeros()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_of_one_value_is_that_value_and_of_none_is_nothing() {
        assert_eq!(percentile(&[], 0.5), None);
        assert_eq!(percentile(&[7.0], 0.0), Some(7.0));
        assert_eq!(percentile(&[7.0], 1.0), Some(7.0));
    }

    #[test]
    fn percentile_interpolates_between_the_nearest_ranks() {
        // p50 of ten values lands between the fifth and the sixth, 0.0 of the way along: the rule is
        // `(n-1) * p`, so with n = 10 and p = .5 the position is 4.5 of the zero-based ranks.
        let v: Vec<f64> = (1..=10).map(|i| i as f64).collect();
        assert_eq!(percentile(&v, 0.5), Some(5.5));
        // Nine times 0.95 is not exactly 8.55 - it is 8.549999999999999, the double nearest it - and the
        // interpolation inherits that, so the p95 of 1 to 10 is a hair under 9.55. Python answers the same
        // hair, which is the whole reason this is a port of an expression rather than of an idea.
        assert_eq!(percentile(&v, 0.95), Some(9.549999999999999));
        assert_eq!(percentile(&v, 1.0), Some(10.0));
        assert_eq!(percentile(&v, 0.0), Some(1.0));
    }

    #[test]
    fn percentile_of_an_unsorted_list_sorts_first() {
        assert_eq!(percentile(&[9.0, 1.0, 5.0], 0.5), Some(5.0));
        assert_eq!(percentile(&[9.0, 1.0, 5.0], 0.95), Some(8.6));
    }

    #[test]
    fn median_of_an_even_count_is_the_mean_of_the_middle_two() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[4.0]), Some(4.0));
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(median(&[1.0, 2.0]), Some(1.5));
        // The two middle values of a frame's times are very often a whole number apart, so the half is
        // exact and the median prints as `.0` more often than a mean would.
        assert_eq!(median(&[7.0, 11.0]), Some(9.0));
    }

    #[test]
    fn mean_is_the_exact_sum_divided_once() {
        assert_eq!(mean(&[]), None);
        assert_eq!(mean(&[3.0]), Some(3.0));
        assert_eq!(mean(&[1.0, 2.0, 6.0]), Some(3.0));
        assert_eq!(mean(&[0.5, 0.25]), Some(0.375));
        assert_eq!(mean(&[1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0]), Some(1.0 / 3.0));
        // `statistics.mean([0.1, 0.2])` is 0.15000000000000002, one ulp above the double that spells 0.15,
        // because the exact sum of the two doubles sits above the halfway point between them.
        assert_eq!(mean(&[0.1, 0.2]).unwrap().to_bits(), 0.15000000000000002f64.to_bits());
    }

    #[test]
    fn mean_of_tile_areas_is_not_the_naive_sum() {
        // The shape the tool asks for: a run's `wr_tiles_kpx`, three decimal places, hundreds of them.
        // Adding these three into a float and dividing once gives 0x1.fff46508dfea3p+7; the exact mean,
        // which is what Python answers and what the area column of section 1 is built from, is one ulp
        // below. Roughly half of all such lists differ from the naive sum by that ulp.
        let v = [304.705, 352.431, 110.796];
        // The Python's answer, `0x1.fff46508dfea2p7`, as the bits it is stored in: `{:#x}` on an f64 is
        // not stable Rust.
        assert_eq!(mean(&v).unwrap().to_bits(), 0x406fff46508dfea2, "the naive sum is one ulp high");
        assert_ne!((v[0] + v[1] + v[2]) / 3.0, mean(&v).unwrap());
    }

    #[test]
    fn a_span_too_wide_for_128_bits_falls_back_and_still_answers() {
        // 1 and 1e-300 are 996 binary orders apart, so the exact sum will not fit; the naive one is a
        // whole ulp of the larger value away from the truth either way, and the column prints `.1`.
        assert_eq!(mean(&[1.0, 1e-300]), Some((1.0 + 1e-300) / 2.0));
        assert_eq!(mean(&[0.0, 0.0]), Some(0.0));
        assert_eq!(mean(&[f64::MIN_POSITIVE, 3.0 * f64::MIN_POSITIVE]), Some(2.0 * f64::MIN_POSITIVE));
    }

    #[test]
    fn the_ratio_helper_rounds_the_halfway_quotient_to_the_even_significand() {
        // A quotient that fits in 53 bits is exact, however it lands: 5/2 is 2.5 and not a tie, because
        // halves are representable. The ties are one exponent further out, where the 54th bit of the
        // quotient is a 1 with nothing after it - which is what `(2^53 + odd) / 2` produces.
        assert_eq!(ratio_of(5, 2), 2.5);
        assert_eq!(ratio_of(7, 2), 3.5);
        assert_eq!(ratio_of(1, 3), 0.3333333333333333);
        assert_eq!(ratio_of(2, 3), 0.6666666666666666);
        assert_eq!(ratio_of(0, 7), 0.0);
        assert_eq!(ratio_of((1u128 << 53) + 1, 2), 4503599627370496.0, "down to the even 2^52");
        assert_eq!(ratio_of((1u128 << 53) + 3, 2), 4503599627370498.0, "up to the even 2^52 + 2");
    }

    /// The nearest double to `a / b`, worked out through the same path the mean uses.
    fn ratio_of(a: u128, b: u64) -> f64 {
        ratio(a, 0, b).unwrap()
    }
}
