//! The three number formats the findings tables print through: CPython's `"%.*f"`, its `str(float)`, and its
//! `"%s"` of whatever the json happened to hold.
//!
//! `"%.*f"` turns out to be Rust's `"{:.N}"`. Both round the *exact* decimal expansion of the double half to
//! even, so they agree on the ties this bench produces constantly - a median of two middle frames is a half,
//! and every whole millisecond count is one - and on everything else. That is worth saying because the obvious
//! port is wrong: `x * 10f64.powi(n)` then rounding ties away from zero, which is what a float format usually
//! means, disagrees with both on `2.5` at 0 digits and on `0.15` at 1. The claim was checked by printing
//! 53,087 doubles at 0, 1 and 2 digits through each side - every float in the 72 runs' frame records, the
//! midpoints of consecutive pairs of them, the exact ties at each printed digit (`(2k+1) / 2·10ⁿ`), 20,000
//! random bit patterns and 20,000 values spread log-uniformly over 1e-30 to 1e30 - and the two outputs are
//! byte-identical. The unit tests below keep the ties that a naive scaling loses.
//!
//! `str(float)` is a different matter: it keeps a `.0` on a whole number that Rust's `Display` drops, and that
//! is the only difference the tables can reach, because a load column never needs exponent notation.

use serde_json::Value;

/// `"-" if v is None else "%.*f" % (digits, v)`: the analysis' `ms`, which every millisecond column goes
/// through, and the dash that says a run recorded no such thing.
pub fn ms(v: Option<f64>, digits: usize) -> String {
    v.map(|v| fixed(v, digits)).unwrap_or_else(|| "-".into())
}

/// `"%.*f" % (digits, v)`.
pub fn fixed(v: f64, digits: usize) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    format!("{v:.digits$}")
}

/// `str(v)` for a float: the shortest digits that read back as this double, and a `.0` on a whole number so it
/// still reads as a float. Loads are `uptime`'s own one- or two-place decimals, so the exponent notation the
/// two spell differently - `1e+22` against `1e22` - is never reached.
pub fn str_float(v: f64) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let mut s = format!("{v}");
    if !s.contains('.') && !s.contains('e') {
        s.push_str(".0");
    }
    s
}

/// The same, for a key the json may not have: `"-" if v is None else "%s" % v`, which is how the step
/// and merged counts of a run print when the driver wrote them and when it did not.
pub fn shown(v: Option<&Value>) -> String {
    v.filter(|v| !v.is_null()).map(py_str).unwrap_or_else(|| "-".into())
}

/// `"%s" % v` for whatever the json held, which the analysis prints without re-typing: the step counts of a run
/// are ints and print as digits, and one written as a float keeps its `.0`.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True".into() } else { "False".into() },
        Value::Null => "None".into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                str_float(n.as_f64().unwrap_or(0.0))
            }
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `("%.*f" % (d, v), d)` for the values whose decimal expansion sits exactly on a printed half, which is
    /// where round-half-even is visible, and each one's answer as CPython gives it.
    #[test]
    fn halves_round_to_the_even_digit_as_python_does() {
        let ties = [
            (0.5, 0, "0"),
            (1.5, 0, "2"),
            (2.5, 0, "2"),
            (3.5, 0, "4"),
            (-2.5, 0, "-2"),
            (0.125, 2, "0.12"),
            (0.375, 2, "0.38"),
            (0.25, 1, "0.2"),
            (0.75, 1, "0.8"),
            (11.25, 1, "11.2"),
            (1024.5, 0, "1024"),
            (1025.5, 0, "1026"),
        ];
        for (v, digits, want) in ties {
            assert_eq!(fixed(v, digits), want, "\"%.*f\" % ({digits}, {v})");
        }
    }

    #[test]
    fn a_double_that_only_looks_like_a_tie_rounds_on_its_true_value() {
        // 2.675 is the classic, and the two sides of 0.05 and 0.15 are what a median of two frames at those
        // milliseconds would print: 2.675 is just under the half, and 0.05 and 0.15 straddle it.
        let cases = [(2.675, 2, "2.67"), (0.05, 1, "0.1"), (0.15, 1, "0.1"), (1e15 + 0.5, 0, "1000000000000000")];
        for (v, digits, want) in cases {
            assert_eq!(fixed(v, digits), want, "\"%.*f\" % ({digits}, {v})");
        }
    }

    #[test]
    fn the_columns_the_bench_actually_writes() {
        // A whole millisecond count stays whole-looking, a median of 7.0 and 8.0 keeps its half, and the
        // 62.0 of a load is what `str(float)` answers rather than the bare `62` of Rust's Display.
        assert_eq!(ms(Some(22.9), 1), "22.9");
        assert_eq!(ms(Some((7.0 + 8.0) / 2.0), 1), "7.5");
        assert_eq!(ms(Some(63.0 / 1000.0), 0), "0");
        assert_eq!(ms(None, 1), "-");
        assert_eq!(str_float(62.0), "62.0");
        assert_eq!(str_float(61.32), "61.32");
        assert_eq!(str_float(-0.0), "-0.0");
        assert_eq!(fixed(-0.0, 1), "-0.0");
        assert_eq!(fixed(f64::NAN, 1), "nan");
        assert_eq!(fixed(f64::NEG_INFINITY, 0), "-inf");
    }

    #[test]
    fn counts_print_as_the_json_wrote_them() {
        assert_eq!(py_str(&serde_json::json!(120)), "120");
        assert_eq!(py_str(&serde_json::json!(120.0)), "120.0");
        assert_eq!(py_str(&serde_json::json!("vsync")), "vsync");
        assert_eq!(py_str(&serde_json::json!(true)), "True");
        assert_eq!(py_str(&serde_json::json!(null)), "None");
    }
}
