//! The known-failure baseline of the test262 gate: `path<TAB>variant` lines, `path` relative to the test262 checkout, sorted, `#`
//! lines are comments. A run is judged by the non-passes that are not in the baseline; the baseline only shrinks (a test that now
//! passes is removed by `--shrink-baseline`, in a deliberate commit) and never grows.
use std::collections::BTreeSet;

/// One test variant: path relative to the test262 checkout, and `sloppy` / `strict` / `module`.
pub type Key = (String, String);

/// The keys of a baseline file; comments and blank lines are skipped, a line without a tab is an error.
pub fn parse(text: &str) -> Result<BTreeSet<Key>, String> {
    let mut keys = BTreeSet::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (path, variant) = line
            .split_once('\t')
            .ok_or_else(|| format!("baseline line {}: expected path<TAB>variant", n + 1))?;
        keys.insert((path.to_string(), variant.to_string()));
    }
    Ok(keys)
}

/// The baseline file text for these keys, sorted by byte order.
pub fn render(keys: &BTreeSet<Key>, header: &str) -> String {
    let mut out = String::new();
    for line in header.lines() {
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
    for (path, variant) in keys {
        out.push_str(path);
        out.push('\t');
        out.push_str(variant);
        out.push('\n');
    }
    out
}

/// `path` relative to the checkout `root` (a result path is `<root>/test/...`).
pub fn relative(root: &str, path: &str) -> String {
    let root = root.trim_end_matches('/');
    path.strip_prefix(root).map(|p| p.trim_start_matches('/')).unwrap_or(path).to_string()
}

/// `line` with the number that counts its entries set to `entries`; unchanged when it counts none.
fn retally(line: &str, entries: usize) -> String {
    let Some(at) = line.find(" entries") else {
        return line.to_string();
    };
    let mut start = at;
    while start > 0 && line.as_bytes()[start - 1].is_ascii_digit() {
        start -= 1;
    }
    if start == at {
        return line.to_string();
    }
    format!("{}{entries}{}", &line[..start], &line[at..])
}

/// The header a shrink writes: the leading `#` lines of the baseline as `render` wants them, its entry count
/// line retaken for `entries` entries. The provenance a baseline carries (engine and corpus revs) is inherited.
pub fn header_with_count(text: &str, entries: usize) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if !line.starts_with('#') {
            break;
        }
        let body = line[1..].trim_start();
        lines.push(retally(body, entries));
    }
    lines.join("\n")
}

/// What a run says about the baseline.
#[derive(Debug, PartialEq, Eq)]
pub struct Verdict {
    /// Non-passes that are not in the baseline.
    pub new: Vec<Key>,
    /// Baseline entries that ran and passed.
    pub fixed: Vec<Key>,
    /// Non-passes that are in the baseline.
    pub known: usize,
}

/// Compare a run with the baseline: `non_passes` are the failed, timed-out and crashed variants, `passed` the variants that passed.
pub fn judge(baseline: &BTreeSet<Key>, non_passes: &BTreeSet<Key>, passed: &BTreeSet<Key>) -> Verdict {
    Verdict {
        new: non_passes.difference(baseline).cloned().collect(),
        fixed: baseline.intersection(passed).cloned().collect(),
        known: non_passes.intersection(baseline).count(),
    }
}

/// The baseline without the entries that pass now. By construction it contains nothing the old baseline did not.
pub fn shrink(baseline: &BTreeSet<Key>, fixed: &[Key]) -> BTreeSet<Key> {
    let fixed: BTreeSet<&Key> = fixed.iter().collect();
    baseline.iter().filter(|k| !fixed.contains(k)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(p: &str, v: &str) -> Key {
        (p.to_string(), v.to_string())
    }

    #[test]
    fn parse_skips_comments_and_rejects_bad_lines() {
        let keys = parse("# header\n\ntest/a.js\tsloppy\ntest/b.js\tstrict\n").unwrap();
        assert_eq!(keys, BTreeSet::from([key("test/a.js", "sloppy"), key("test/b.js", "strict")]));
        assert!(parse("test/a.js sloppy\n").is_err());
    }

    #[test]
    fn render_round_trips_and_sorts() {
        let keys = BTreeSet::from([key("test/b.js", "strict"), key("test/a.js", "sloppy")]);
        let text = render(&keys, "known failures\nsecond line");
        assert!(text.starts_with("# known failures\n# second line\ntest/a.js\tsloppy\n"));
        assert_eq!(parse(&text).unwrap(), keys);
    }

    #[test]
    fn relative_strips_the_checkout() {
        assert_eq!(relative("/c/t262", "/c/t262/test/a.js"), "test/a.js");
        assert_eq!(relative("/c/t262/", "/c/t262/test/a.js"), "test/a.js");
        assert_eq!(relative("/c/t262", "/other/test/a.js"), "/other/test/a.js");
    }

    #[test]
    fn judge_separates_new_known_and_fixed() {
        let baseline = BTreeSet::from([key("a", "sloppy"), key("b", "sloppy"), key("c", "strict")]);
        let non_passes = BTreeSet::from([key("a", "sloppy"), key("d", "sloppy")]);
        let passed = BTreeSet::from([key("b", "sloppy"), key("e", "sloppy")]);
        let v = judge(&baseline, &non_passes, &passed);
        assert_eq!(v.new, vec![key("d", "sloppy")]);
        assert_eq!(v.fixed, vec![key("b", "sloppy")]);
        assert_eq!(v.known, 1);
    }

    #[test]
    fn a_baseline_entry_that_did_not_run_is_neither_fixed_nor_new() {
        let baseline = BTreeSet::from([key("a", "sloppy")]);
        let v = judge(&baseline, &BTreeSet::new(), &BTreeSet::new());
        assert!(v.new.is_empty() && v.fixed.is_empty() && v.known == 0);
    }

    #[test]
    fn shrink_only_removes() {
        let baseline = BTreeSet::from([key("a", "sloppy"), key("b", "sloppy")]);
        let shrunk = shrink(&baseline, &[key("a", "sloppy"), key("zzz", "sloppy")]);
        assert_eq!(shrunk, BTreeSet::from([key("b", "sloppy")]));
        assert!(shrunk.is_subset(&baseline));
    }

    #[test]
    fn header_with_count_keeps_every_line_and_retakes_the_entry_count() {
        let text = "# Known test262 non-passes of the Rust JavaScript engine: path (relative to the checkout) and variant.\n\
                    # This file only shrinks, by deliberate commits.\n\
                    # Engine: Brimstone fork engine-annexb 189b2fe2, test262 227d905513, 3028 entries.\n\
                    test/a.js\tsloppy\n";
        let header = header_with_count(text, 3027);
        assert_eq!(
            header,
            "Known test262 non-passes of the Rust JavaScript engine: path (relative to the checkout) and variant.\n\
             This file only shrinks, by deliberate commits.\n\
             Engine: Brimstone fork engine-annexb 189b2fe2, test262 227d905513, 3027 entries."
        );
        let written = render(&parse(text).unwrap(), &header);
        assert_eq!(
            written,
            "# Known test262 non-passes of the Rust JavaScript engine: path (relative to the checkout) and variant.\n\
             # This file only shrinks, by deliberate commits.\n\
             # Engine: Brimstone fork engine-annexb 189b2fe2, test262 227d905513, 3027 entries.\n\
             test/a.js\tsloppy\n"
        );
    }

    #[test]
    fn header_with_count_stops_at_the_entries_and_invents_nothing() {
        // A header of two lines (what an earlier shrink left) stays two lines; no provenance is invented for it.
        assert_eq!(header_with_count("# a\n# b\ntest/x.js\traw\n", 7), "a\nb");
        // A line that counts no entries is kept verbatim, and so is a file with no header.
        assert_eq!(header_with_count("# a\n", 7), "a");
        assert_eq!(header_with_count("test/x.js\traw\n", 7), "");
    }
}
