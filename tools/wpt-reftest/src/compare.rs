//! The WPT fuzzy pixel comparison, on its own so it can be unit-tested without a browser.
//!
//! WPT's rule, as this project scores it: `maxDifference` is the largest per-channel difference allowed
//! for any pixel, `maxPixels` is how many pixels may differ at all. A `==` relation holds when the number
//! of differing pixels is <= maxPixels AND the largest per-channel difference is <= maxDifference. A
//! missing fuzzy entry is exact, i.e. (0, 0). `!=` is the negation of `==`.

use std::path::Path;

/// The two fuzzy tolerances, in WPT's order: per-channel difference, then pixel count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fuzzy {
    pub max_difference: u8,
    pub max_pixels: u64,
}

impl Fuzzy {
    pub const EXACT: Fuzzy = Fuzzy { max_difference: 0, max_pixels: 0 };

    /// The manifest carries `[[minDiff, maxDiff], [minPix, maxPix]]`; WPT writes `<min>-<max>` and only
    /// the max is scored, the min is a floor it also records.
    pub fn from_manifest(pair: &[serde_json::Value]) -> Option<Fuzzy> {
        let max = |v: Option<&serde_json::Value>| {
            v.and_then(|v| v.as_array()).and_then(|a| a.get(1)).and_then(|v| v.as_u64())
        };
        Some(Fuzzy {
            max_difference: max(pair.first())?.min(255) as u8,
            max_pixels: max(pair.get(1))?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Diff {
    pub width: u32,
    pub height: u32,
    /// Pixels with any channel difference at all: what maxPixels is compared against.
    pub differing: u64,
    /// Pixels whose largest channel difference exceeds `max_difference`: WPT's own counter, kept as a
    /// diagnostic so a failure can be read either way.
    pub differing_over: u64,
    /// The largest per-channel difference seen anywhere: what maxDifference is compared against.
    pub max_difference: u8,
    pub total_pixels: u64,
    /// The two PNGs are not the same size. WPT calls that a mismatch outright.
    pub size_mismatch: bool,
    /// Every pixel of the test image is the same colour. A blank render matching a blank reference
    /// passes `==` without rendering anything, so the count is reported rather than hidden.
    pub test_uniform: bool,
    pub ref_uniform: bool,
}

impl Diff {
    /// Does this relation hold? `op` is the manifest's `"=="` or `"!="`.
    pub fn holds(&self, op: &str, fuzzy: Fuzzy) -> bool {
        let matched = !self.size_mismatch
            && self.differing <= fuzzy.max_pixels
            && self.max_difference <= fuzzy.max_difference;
        match op {
            "!=" => !matched,
            // Anything else in the manifest is treated as `==`, which is the only other relation WPT has.
            _ => matched,
        }
    }
}

/// Compare two PNGs. `allowance` only feeds the extra `differing_over` diagnostic; `differing` and
/// `max_difference` are measured against zero and are what `Diff::holds` scores.
pub fn compare(a: &Path, b: &Path, allowance: u8) -> Result<Diff, String> {
    let a = image::open(a).map_err(|e| format!("{}: {e}", a.display()))?;
    let b = image::open(b).map_err(|e| format!("{}: {e}", b.display()))?;
    let (a, b) = (a.to_rgba8(), b.to_rgba8());
    let (aw, ah, bw, bh) = (a.width(), a.height(), b.width(), b.height());
    let total = (aw as u64 * ah as u64).max(bw as u64 * bh as u64);
    if aw != bw || ah != bh {
        return Ok(Diff {
            width: aw.max(bw),
            height: ah.max(bh),
            differing: total,
            differing_over: total,
            max_difference: 255,
            total_pixels: total,
            size_mismatch: true,
            test_uniform: false,
            ref_uniform: false,
        });
    }
    let (mut differing, mut differing_over, mut max_difference) = (0u64, 0u64, 0u8);
    let (mut test_uniform, mut ref_uniform) = (true, true);
    let first_a = a.pixels().next().map(|p| p.0);
    let first_b = b.pixels().next().map(|p| p.0);
    for (p, q) in a.pixels().zip(b.pixels()) {
        let d = p.0.iter().zip(q.0.iter()).map(|(u, v)| u.abs_diff(*v)).max().unwrap_or(0);
        if d > 0 {
            differing += 1;
        }
        if d > allowance {
            differing_over += 1;
        }
        max_difference = max_difference.max(d);
        test_uniform &= Some(p.0) == first_a;
        ref_uniform &= Some(q.0) == first_b;
    }
    Ok(Diff {
        width: aw,
        height: ah,
        differing,
        differing_over,
        max_difference,
        total_pixels: total,
        size_mismatch: false,
        test_uniform,
        ref_uniform,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG of `w`x`h` white, with `edits` applied as (x, y, grey) before it is written.
    fn png(dir: &Path, name: &str, w: u32, h: u32, edits: &[(u32, u32, u8)]) -> std::path::PathBuf {
        let mut img = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255]));
        for &(x, y, g) in edits {
            img.put_pixel(x, y, image::Rgba([g, g, g, 255]));
        }
        let p = dir.join(name);
        img.save(&p).expect("write the test PNG");
        p
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wpt-reftest-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).expect("a scratch dir");
        d
    }

    #[test]
    fn identical_is_exact_pass() {
        let d = scratch("identical");
        let a = png(&d, "a.png", 16, 16, &[]);
        let diff = compare(&a, &a, 0).unwrap();
        assert_eq!((diff.differing, diff.max_difference), (0, 0));
        assert!(diff.holds("==", Fuzzy::EXACT));
        assert!(!diff.holds("!=", Fuzzy::EXACT));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn blank_render_is_reported_as_uniform() {
        let d = scratch("uniform");
        let a = png(&d, "a.png", 16, 16, &[]);
        let diff = compare(&a, &a, 0).unwrap();
        assert!(diff.holds("==", Fuzzy::EXACT), "a blank pair still scores as a pass");
        assert!(diff.test_uniform && diff.ref_uniform, "but it must be visible as a blank pass");
        let b = png(&d, "b.png", 16, 16, &[(3, 4, 254)]);
        let diff = compare(&a, &b, 0).unwrap();
        assert!(diff.test_uniform && !diff.ref_uniform);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn one_pixel_off_by_one_needs_fuzzy() {
        let d = scratch("one");
        let a = png(&d, "a.png", 16, 16, &[]);
        let b = png(&d, "b.png", 16, 16, &[(3, 4, 254)]);
        let diff = compare(&a, &b, 0).unwrap();
        assert_eq!((diff.differing, diff.max_difference), (1, 1));
        assert!(!diff.holds("==", Fuzzy::EXACT), "exact must fail on one pixel");
        assert!(diff.holds("==", Fuzzy { max_difference: 1, max_pixels: 1 }));
        assert!(diff.holds("!=", Fuzzy::EXACT), "a mismatch relation must pass on a real difference");
        assert!(!diff.holds("!=", Fuzzy { max_difference: 1, max_pixels: 1 }));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn max_difference_is_a_ceiling_not_a_budget() {
        // The css-masking shape in the brief: 0-69 maxDiff, 0-360 maxPixels.
        let d = scratch("ceiling");
        let fuzzy = Fuzzy { max_difference: 69, max_pixels: 360 };
        let a = png(&d, "a.png", 32, 32, &[]);
        let ok = png(&d, "ok.png", 32, 32, &[(1, 1, 255 - 69)]);
        let bad = png(&d, "bad.png", 32, 32, &[(1, 1, 255 - 70)]);
        assert!(compare(&a, &ok, 69).unwrap().holds("==", fuzzy));
        let diff = compare(&a, &bad, 69).unwrap();
        assert_eq!((diff.differing, diff.differing_over, diff.max_difference), (1, 1, 70));
        assert!(!diff.holds("==", fuzzy), "one pixel over the per-channel ceiling must fail");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn max_pixels_is_a_count_of_any_difference() {
        let d = scratch("count");
        let fuzzy = Fuzzy { max_difference: 69, max_pixels: 360 };
        let a = png(&d, "a.png", 32, 32, &[]);
        let edits: Vec<(u32, u32, u8)> = (0..361).map(|i| (i % 32, i / 32, 254)).collect();
        let b = png(&d, "b.png", 32, 32, &edits);
        let diff = compare(&a, &b, 69).unwrap();
        assert_eq!(diff.differing, 361);
        // Every pixel is inside the per-channel ceiling, but there are too many of them.
        assert_eq!(diff.differing_over, 0);
        assert!(!diff.holds("==", fuzzy), "361 differing pixels must fail a 360-pixel budget");
        assert!(diff.holds("==", Fuzzy { max_difference: 69, max_pixels: 361 }));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn alpha_counts_as_a_channel() {
        let d = scratch("alpha");
        let mut b = image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 255, 255, 255]));
        b.put_pixel(0, 0, image::Rgba([255, 255, 255, 254]));
        let bp = d.join("b.png");
        b.save(&bp).unwrap();
        let a = png(&d, "a.png", 8, 8, &[]);
        let diff = compare(&a, &bp, 0).unwrap();
        assert_eq!((diff.differing, diff.max_difference), (1, 1));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn size_mismatch_is_a_mismatch() {
        let d = scratch("size");
        let a = png(&d, "a.png", 16, 16, &[]);
        let b = png(&d, "b.png", 16, 17, &[]);
        let diff = compare(&a, &b, 0).unwrap();
        assert!(diff.size_mismatch);
        assert!(!diff.holds("==", Fuzzy { max_difference: 255, max_pixels: u64::MAX }));
        assert!(diff.holds("!=", Fuzzy::EXACT));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn manifest_fuzzy_pairs_parse() {
        let v: serde_json::Value = serde_json::json!([[0, 69], [0, 360]]);
        let f = Fuzzy::from_manifest(v.as_array().unwrap()).unwrap();
        assert_eq!(f, Fuzzy { max_difference: 69, max_pixels: 360 });
        assert_eq!(Fuzzy::from_manifest(&[]), None);
    }
}