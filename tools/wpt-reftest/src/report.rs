//! `wpt-reftest report ...`: read a run's `results.jsonl` back and answer the two questions a large
//! selection actually poses -- which group of tests is failing, and what changed between two runs.
//!
//! Nothing here renders or compares pixels: the runner's JSONL is the only input, so a report can be
//! regenerated from any directory the runner or `tests/wpt/run-reftests.sh` left behind.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const REPORT_USAGE: &str = "\
wpt-reftest report: read a run's results.jsonl and report on it

  wpt-reftest report clusters <results-dir> [--md <file>] [--top N]
  wpt-reftest report compare <before-dir> <after-dir> [--md <file>]

  clusters   totals by type with pass rate, then the failing tests grouped into clusters, biggest first
  compare    tests present in both runs only: overall pass delta, a table per dir with fixed and
             regressed counts, every regressed path, then the top 10 clusters still failing after
  --md FILE  also write the markdown to FILE
  --top N    how many clusters `clusters` shows, default 25

<dir> is a directory holding the runner's results.jsonl. Exit 0 normally, 3 on a missing or unreadable
file or a bad argument.
";

/// Examples per cluster, clusters per `report clusters`, clusters per `report compare`.
const EXAMPLES: usize = 3;
const DEFAULT_TOP: usize = 25;
const STILL_FAILING_TOP: usize = 10;

/// How a failing test failed, as the report groups it. The order of `ALL` is the print order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Timeout,
    Error,
    SizeMismatch,
    PixelsSmall,
    PixelsLarge,
    Subtests,
    Crash,
}

impl Kind {
    const ALL: [Kind; 7] = [
        Kind::Timeout,
        Kind::Error,
        Kind::SizeMismatch,
        Kind::PixelsSmall,
        Kind::PixelsLarge,
        Kind::Subtests,
        Kind::Crash,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Kind::Timeout => "timeout",
            Kind::Error => "error",
            Kind::SizeMismatch => "size-mismatch",
            Kind::PixelsSmall => "pixels-small",
            Kind::PixelsLarge => "pixels-large",
            Kind::Subtests => "subtests",
            Kind::Crash => "crash",
        }
    }
}

/// One line of `results.jsonl`, kept only as far as the report reads it.
#[derive(Clone, Debug)]
pub struct Row {
    pub path: String,
    pub rtype: String,
    pub dir: String,
    pub status: String,
    /// `None` for a PASS.
    pub failure: Option<Kind>,
}

/// The parsed file and the number of lines that were not JSON at all -- a run interrupted mid-write
/// leaves a torn last line, and a report should still be readable rather than refuse the file.
pub fn parse(jsonl: &str) -> (Vec<Row>, usize) {
    let mut rows = Vec::new();
    let mut skipped = 0;
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            skipped += 1;
            continue;
        };
        let status = v["status"].as_str().unwrap_or("").to_string();
        let failure = failure_kind(&v);
        rows.push(Row {
            path: v["path"].as_str().unwrap_or("").to_string(),
            rtype: v["type"].as_str().unwrap_or("").to_string(),
            dir: v["dir"].as_str().unwrap_or("").to_string(),
            failure,
            status,
        });
    }
    (rows, skipped)
}

/// The one kind that explains a failing test, `None` for a pass. The order of the checks *is* the rule:
/// a TIMEOUT is a timeout whatever its type; a testharness FAIL that names no failing subtest is a
/// harness problem, so it counts as `error` rather than disappearing from the breakdown.
fn failure_kind(r: &Value) -> Option<Kind> {
    let status = r["status"].as_str().unwrap_or("");
    if status == "PASS" {
        return None;
    }
    let rtype = r["type"].as_str().unwrap_or("");
    if status == "TIMEOUT" {
        return Some(Kind::Timeout);
    }
    if rtype == "crashtest" {
        // The runner fails a crashtest only on a crash and reports a hang as TIMEOUT, which is taken
        // above, so anything left here is a crash.
        return Some(Kind::Crash);
    }
    if rtype == "testharness" && r["subtest_failures"].as_array().is_some_and(|a| !a.is_empty()) {
        return Some(Kind::Subtests);
    }
    if status == "ERROR" {
        return Some(Kind::Error);
    }
    if rtype == "reftest" {
        let none = Vec::new();
        let relations = r["relations"].as_array().unwrap_or(&none);
        if relations.iter().any(|e| e["pixels"]["size_mismatch"].as_bool().unwrap_or(false)) {
            return Some(Kind::SizeMismatch);
        }
        // Several relations can fail at once; the largest differing/total ratio is the one to report.
        let mut worst: Option<f64> = None;
        for e in relations.iter().filter(|e| e["pass"].as_bool() != Some(true)) {
            let (Some(d), Some(t)) = (e["pixels"]["differing"].as_u64(), e["pixels"]["total"].as_u64()) else {
                continue;
            };
            let ratio = if t == 0 { 0.0 } else { d as f64 / t as f64 };
            worst = Some(worst.map_or(ratio, |w| w.max(ratio)));
        }
        if let Some(ratio) = worst {
            return Some(if ratio < 0.01 { Kind::PixelsSmall } else { Kind::PixelsLarge });
        }
        // A reftest that failed with no pixel numbers at all: its reference never rendered.
    }
    Some(Kind::Error)
}

/// `css/css-masking/clip-path/clip-path-inset-001.html` -> `css/css-masking/clip-path/clip-path-inset`.
///
/// The directory is kept; the file name loses its extension, then any trailing `-ref` / `-notref` and
/// any trailing `-NNN` / `_NNN` number, repeatedly, so `foo.html`, `foo-002.html` and `foo-001-ref.html`
/// all land in the cluster `dir/foo`. A variant's query string or fragment is dropped first.
pub fn cluster_key(path: &str) -> String {
    let file = path.split(['?', '#']).next().unwrap_or(path);
    let (dir, name) = match file.rsplit_once('/') {
        Some((d, n)) => (format!("{d}/"), n),
        None => (String::new(), file),
    };
    let stem = match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    };
    let mut key = stem.to_string();
    while let Some(shorter) = strip_suffix(&key) {
        key = shorter.to_string();
    }
    format!("{dir}{key}")
}

/// One cluster suffix, or `None` when the stem has none. Never strips a name down to nothing.
fn strip_suffix(stem: &str) -> Option<&str> {
    for suffix in ["-notref", "-ref"] {
        if stem.len() > suffix.len() && stem.ends_with(suffix) {
            return Some(&stem[..stem.len() - suffix.len()]);
        }
    }
    let head = stem.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = stem.len() - head.len();
    if digits == 0 || digits > 3 || !(head.ends_with('-') || head.ends_with('_')) {
        return None;
    }
    let trimmed = &head[..head.len() - 1];
    (!trimmed.is_empty()).then_some(trimmed)
}

#[derive(Clone, Debug)]
pub struct Cluster {
    pub key: String,
    pub failing: usize,
    /// Every test in that cluster that ran, pass included.
    pub total: usize,
    pub kinds: Vec<(Kind, usize)>,
    pub examples: Vec<String>,
}

/// The failing clusters, biggest first, ties by key so two runs of the same file agree line for line.
pub fn cluster_list(rows: &[Row]) -> Vec<Cluster> {
    let mut by_key: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
    for r in rows {
        by_key.entry(cluster_key(&r.path)).or_default().push(r);
    }
    let mut out = Vec::new();
    for (key, rs) in by_key {
        let failing: Vec<&&Row> = rs.iter().filter(|r| r.failure.is_some()).collect();
        if failing.is_empty() {
            continue;
        }
        let mut counts = [0usize; Kind::ALL.len()];
        for r in &failing {
            let kind = r.failure.unwrap();
            counts[Kind::ALL.iter().position(|k| *k == kind).unwrap_or(0)] += 1;
        }
        let kinds = Kind::ALL
            .iter()
            .enumerate()
            .filter(|(i, _)| counts[*i] > 0)
            .map(|(i, k)| (*k, counts[i]))
            .collect();
        let mut examples: Vec<String> = failing.iter().map(|r| r.path.clone()).collect();
        examples.sort();
        examples.truncate(EXAMPLES);
        out.push(Cluster { key, failing: failing.len(), total: rs.len(), kinds, examples });
    }
    out.sort_by(|a, b| b.failing.cmp(&a.failing).then(a.key.cmp(&b.key)));
    out
}

#[derive(Clone, Copy, Default, Debug)]
struct Tally {
    pass: usize,
    fail: usize,
    error: usize,
    timeout: usize,
}

impl Tally {
    fn push(&mut self, status: &str) {
        match status {
            "PASS" => self.pass += 1,
            "ERROR" => self.error += 1,
            "TIMEOUT" => self.timeout += 1,
            _ => self.fail += 1,
        }
    }

    fn total(self) -> usize {
        self.pass + self.fail + self.error + self.timeout
    }
}

fn pct(pass: usize, total: usize) -> String {
    if total == 0 {
        return "-".to_string();
    }
    format!("{:.1}%", 100.0 * pass as f64 / total as f64)
}

fn tally_row(name: &str, t: Tally) -> String {
    format!("| {name} | {} | {} | {} | {} | {} | {} |\n", t.total(), t.pass, t.fail, t.error, t.timeout, pct(t.pass, t.total()))
}

fn kinds_text(kinds: &[(Kind, usize)]) -> String {
    if kinds.is_empty() {
        return "none".to_string();
    }
    kinds.iter().map(|(k, n)| format!("{} {}", k.as_str(), n)).collect::<Vec<_>>().join(", ")
}

pub fn clusters_markdown(rows: &[Row], skipped: usize, source: &Path, top: usize) -> String {
    let mut all = Tally::default();
    let mut by_type: BTreeMap<String, Tally> = BTreeMap::new();
    for r in rows {
        all.push(&r.status);
        by_type.entry(r.rtype.clone()).or_default().push(&r.status);
    }
    let clusters = cluster_list(rows);
    let failing: usize = clusters.iter().map(|c| c.failing).sum();

    let mut out = String::new();
    let _ = writeln!(out, "# WPT failure clusters\n\nSource: `{}`\n", source.display());
    let _ = write!(out, "| type | tests | pass | fail | error | timeout | pass rate |\n| --- | --- | --- | --- | --- | --- | --- |\n");
    for (t, tally) in &by_type {
        let _ = write!(out, "{}", tally_row(t, *tally));
    }
    let _ = write!(out, "{}", tally_row("**all**", all));

    let _ = write!(
        out,
        "\n{failing} failing test{} in {} cluster{}, {} shown.\n",
        if failing == 1 { "" } else { "s" },
        clusters.len(),
        if clusters.len() == 1 { "" } else { "s" },
        clusters.len().min(top),
    );
    if clusters.is_empty() {
        let _ = write!(out, "\nNothing failed.\n");
    }
    for c in clusters.iter().take(top) {
        let _ = write!(out, "\n## {}\n\n- failing {} of {} test(s)\n- kinds: {}\n", c.key, c.failing, c.total, kinds_text(&c.kinds));
        let _ = write!(out, "- examples: {}\n", c.examples.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(", "));
    }
    if let Some(rest) = clusters.get(top..) {
        let rest_failing: usize = rest.iter().map(|c| c.failing).sum();
        let _ = write!(out, "\n{} further cluster(s) not shown, {rest_failing} failing test(s).\n", rest.len());
    }
    if skipped > 0 {
        let _ = write!(out, "\nNote: {skipped} line(s) of results.jsonl were not JSON and were ignored.\n");
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirDelta {
    pub dir: String,
    pub tests: usize,
    pub pass_before: usize,
    pub pass_after: usize,
    pub fixed: usize,
    pub regressed: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Comparison {
    pub tests_before: usize,
    pub tests_after: usize,
    /// The tests present in both runs: the only ones compared.
    pub compared: usize,
    pub only_before: usize,
    pub only_after: usize,
    pub pass_before: usize,
    pub pass_after: usize,
    pub dirs: Vec<DirDelta>,
    /// Every regressed path with the status it regressed to, sorted by path.
    pub regressed: Vec<(String, String)>,
    /// Cluster key with its failing count before and after, still-failing clusters only.
    pub still_failing: Vec<(String, usize, usize)>,
}

/// A path that appears twice keeps its first row; the runner writes one line per test, so a duplicate
/// means the same URL was scored twice and the first score is the one that was reported.
fn index(rows: &[Row]) -> BTreeMap<&str, &Row> {
    let mut m = BTreeMap::new();
    for r in rows {
        m.entry(r.path.as_str()).or_insert(r);
    }
    m
}

pub fn compare_rows(before: &[Row], after: &[Row]) -> Comparison {
    let (bi, ai) = (index(before), index(after));
    let mut c = Comparison { tests_before: bi.len(), tests_after: ai.len(), ..Default::default() };
    let mut dirs: BTreeMap<String, DirDelta> = BTreeMap::new();
    // cluster key -> (failing before, failing after), over the compared tests only.
    let mut clusters: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (path, b) in &bi {
        let Some(a) = ai.get(path) else {
            c.only_before += 1;
            continue;
        };
        c.compared += 1;
        let (b_pass, a_pass) = (b.status == "PASS", a.status == "PASS");
        c.pass_before += b_pass as usize;
        c.pass_after += a_pass as usize;
        let dir = if b.dir.is_empty() { a.dir.clone() } else { b.dir.clone() };
        let d = dirs.entry(dir.clone()).or_insert_with(|| DirDelta { dir: dir.clone(), ..Default::default() });
        d.tests += 1;
        d.pass_before += b_pass as usize;
        d.pass_after += a_pass as usize;
        d.fixed += (!b_pass && a_pass) as usize;
        if b_pass && !a_pass {
            d.regressed += 1;
            c.regressed.push(((*path).to_string(), a.status.clone()));
        }
        let e = clusters.entry(cluster_key(path)).or_default();
        e.0 += (!b_pass) as usize;
        e.1 += (!a_pass) as usize;
    }
    c.only_after = ai.len() - c.compared;
    c.dirs = dirs.into_values().collect();
    c.regressed.sort();
    c.still_failing = {
        let mut sf: Vec<(String, usize, usize)> =
            clusters.into_iter().filter(|(_, (_, after))| *after > 0).map(|(k, (b, a))| (k, b, a)).collect();
        sf.sort_by(|x, y| y.2.cmp(&x.2).then(x.0.cmp(&y.0)));
        sf.truncate(STILL_FAILING_TOP);
        sf
    };
    c
}

pub fn compare_markdown(c: &Comparison, before_dir: &Path, after_dir: &Path) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# WPT before/after comparison\n\nBefore: `{}`\nAfter: `{}`\n", before_dir.display(), after_dir.display());
    let delta = c.pass_after as i64 - c.pass_before as i64;
    let _ = write!(out, "| | tests compared | pass | pass rate |\n| --- | --- | --- | --- |\n");
    for (label, p) in [("before", c.pass_before), ("after", c.pass_after)] {
        let _ = write!(out, "| {label} | {} | {p} | {} |\n", c.compared, pct(p, c.compared));
    }
    let _ = write!(
        out,
        "\nPass delta **{delta:+}** over the {} test(s) present in both runs ({} before, {} after; {} only \
         before and {} only after are not compared).\n",
        c.compared, c.tests_before, c.tests_after, c.only_before, c.only_after
    );

    let _ = write!(
        out,
        "\n## By dir\n\n| dir | tests | pass before | pass after | fixed | regressed |\n\
         | --- | --- | --- | --- | --- | --- |\n"
    );
    if c.dirs.is_empty() {
        let _ = write!(out, "| _none in common_ | | | | | |\n");
    }
    for d in &c.dirs {
        let _ = write!(
            out,
            "| {} | {} | {} | {} | {} | {} |\n",
            d.dir, d.tests, d.pass_before, d.pass_after, d.fixed, d.regressed
        );
    }

    let _ = write!(out, "\n## Regressed ({}): PASS before, not PASS after\n\n", c.regressed.len());
    if c.regressed.is_empty() {
        let _ = write!(out, "None.\n");
    }
    for (path, status) in &c.regressed {
        let _ = write!(out, "- `{path}` -> {status}\n");
    }

    let _ = write!(
        out,
        "\n## Still failing after: top {} clusters\n\n| cluster | failing before | failing after |\n| --- | --- | --- |\n",
        STILL_FAILING_TOP
    );
    if c.still_failing.is_empty() {
        let _ = write!(out, "| _nothing failing after_ | | |\n");
    }
    for (key, before, after) in &c.still_failing {
        let _ = write!(out, "| `{key}` | {before} | {after} |\n");
    }
    out
}

// --------------------------------------------------------------------------------------------
// The `report` subcommand itself
// --------------------------------------------------------------------------------------------

pub fn run(args: &[String]) -> i32 {
    match dispatch(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("wpt-reftest: {e}");
            3
        },
    }
}

fn dispatch(args: &[String]) -> Result<i32, String> {
    // One line on stderr for every failure, as the runner's exit-3 contract has it; `--help` for the rest.
    let Some(sub) = args.first() else {
        return Err("report wants a subcommand: clusters or compare (see --help)".into());
    };
    let rest = &args[1..];
    if sub == "--help" || sub == "-h" || rest.iter().any(|a| a == "--help" || a == "-h") {
        print!("{REPORT_USAGE}");
        return Ok(0);
    }
    let (pos, md, top) = parse_flags(rest)?;
    let text = match (sub.as_str(), pos.as_slice()) {
        ("clusters", [dir]) => clusters_text(Path::new(dir), top)?,
        ("clusters", other) => return Err(too_many_args("report clusters wants one results directory", other)),
        ("compare", [before, after]) => compare_text(Path::new(before), Path::new(after))?,
        ("compare", other) => return Err(too_many_args("report compare wants a before directory and an after directory", other)),
        (other, _) => return Err(format!("unknown report subcommand {other} (see --help)")),
    };
    print!("{text}");
    if let Some(p) = md {
        fs::write(&p, text).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(0)
}

fn too_many_args(want: &str, pos: &[String]) -> String {
    if pos.is_empty() {
        return format!("{want} (see --help)");
    }
    format!("{want}, got {}: {} (see --help)", pos.len(), pos.join(" "))
}

fn parse_flags(args: &[String]) -> Result<(Vec<String>, Option<PathBuf>, usize), String> {
    let mut pos = Vec::new();
    let mut md = None;
    let mut top = DEFAULT_TOP;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        // A closure rather than a macro: macro_rules cannot see `arg` from its call site.
        let mut need = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--md" => md = Some(PathBuf::from(need()?)),
            "--top" => top = need()?.parse().map_err(|_| "--top wants a number")?,
            other if other.starts_with('-') && other != "-" => return Err(format!("unknown argument {other}")),
            other => pos.push(other.to_string()),
        }
    }
    Ok((pos, md, top))
}

fn read_rows(dir: &Path) -> Result<(Vec<Row>, usize), String> {
    let p = dir.join("results.jsonl");
    let text = fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(parse(&text))
}

pub fn clusters_text(dir: &Path, top: usize) -> Result<String, String> {
    let (rows, skipped) = read_rows(dir)?;
    Ok(clusters_markdown(&rows, skipped, &dir.join("results.jsonl"), top))
}

pub fn compare_text(before_dir: &Path, after_dir: &Path) -> Result<String, String> {
    let (before, s1) = read_rows(before_dir)?;
    let (after, s2) = read_rows(after_dir)?;
    let mut text = compare_markdown(&compare_rows(&before, &after), before_dir, after_dir);
    if s1 + s2 > 0 {
        let _ = write!(text, "\nNote: {} unparseable line(s) across the two files were ignored.\n", s1 + s2);
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    /// One `results.jsonl` object, with only the fields the report reads.
    fn jrow(path: &str, rtype: &str, dir: &str, status: &str) -> Value {
        json!({"path": path, "type": rtype, "dir": dir, "status": status})
    }

    fn with(mut v: Value, key: &str, val: Value) -> Value {
        v[key] = val;
        v
    }

    /// A failing reftest relation with its pixel counts.
    fn rel(differing: u64, total: u64, size_mismatch: bool) -> Value {
        json!([{"pass": false, "pixels": {"differing": differing, "total": total, "size_mismatch": size_mismatch}}])
    }

    /// The objects as file text, then parsed back -- the report only ever sees the JSONL.
    fn rows(objs: &[Value]) -> Vec<Row> {
        let jsonl = objs.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
        parse(&jsonl).0
    }

    fn kinds(c: &Cluster) -> Vec<(&'static str, usize)> {
        c.kinds.iter().map(|(k, n)| (k.as_str(), *n)).collect()
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wpt-reftest-report-test-{tag}-{}", std::process::id()));
        fs::create_dir_all(&d).expect("a scratch dir");
        d
    }

    /// Ten lines of every shape the runner can write, in four clusters of different sizes.
    fn mixed() -> Vec<Value> {
        vec![
            with(jrow("css/css-masking/clip-path/clip-path-inset-001.html", "reftest", "css/css-masking", "FAIL"), "relations", rel(5000, 480000, false)),
            with(jrow("css/css-masking/clip-path/clip-path-inset-002.html", "reftest", "css/css-masking", "FAIL"), "relations", rel(100, 480000, false)),
            jrow("css/css-masking/clip-path/clip-path-inset-003.html", "reftest", "css/css-masking", "PASS"),
            // the reference's own name joins the cluster it is the reference of
            with(jrow("css/css-masking/clip-path/clip-path-inset-ref.html", "reftest", "css/css-masking", "FAIL"), "relations", rel(480000, 480000, true)),
            jrow("css/css-fonts/font-display-001.html", "reftest", "css/css-fonts", "TIMEOUT"),
            jrow("css/css-fonts/font-display-002.html", "reftest", "css/css-fonts", "PASS"),
            with(jrow("css/css-fonts/font-face-rules-001.html", "testharness", "css/css-fonts", "FAIL"), "subtest_failures", json!(["FAIL the family is used"])),
            jrow("css/css-fonts/font-face-rules-002.html", "testharness", "css/css-fonts", "PASS"),
            jrow("svg/crash-opacity-001.html", "crashtest", "svg", "FAIL"),
            jrow("svg/crash-opacity-002.html", "crashtest", "svg", "TIMEOUT"),
        ]
    }

    #[test]
    fn cluster_key_strips_number_ref_and_keeps_the_directory() {
        assert_eq!(cluster_key("css/css-masking/clip-path/clip-path-inset-001.html"), "css/css-masking/clip-path/clip-path-inset");
        assert_eq!(cluster_key("css/css-masking/clip-path/clip-path-inset-002.html"), "css/css-masking/clip-path/clip-path-inset");
        // a name with no number at all keeps its stem
        assert_eq!(cluster_key("css/css-masking/clip/clip-filter-order.html"), "css/css-masking/clip/clip-filter-order");
        // a -ref suffix goes, so the reference joins the tests it references
        assert_eq!(cluster_key("css/css-masking/clip/clip-absolute-positioned-ref.html"), "css/css-masking/clip/clip-absolute-positioned");
        assert_eq!(cluster_key("css/css-masking/mask/mask-notref-01.html"), "css/css-masking/mask/mask");
        // underscore numbering, and a directory several levels deep
        assert_eq!(cluster_key("a/b/c/d/e/font-face_12.html"), "a/b/c/d/e/font-face");
        // no directory, a variant's query string, a dot in a directory name
        assert_eq!(cluster_key("foo.html"), "foo");
        assert_eq!(cluster_key("css/x/y-001.html?width=100"), "css/x/y");
        assert_eq!(cluster_key("v1.0/css/x-001.html"), "v1.0/css/x");
        // a stem that is only a number is not stripped away
        assert_eq!(cluster_key("css/x/-001.html"), "css/x/-001");
    }

    #[test]
    fn failure_kinds_follow_the_rule() {
        let kind = |v: Value| failure_kind(&v);
        assert_eq!(kind(jrow("a.html", "reftest", "a", "PASS")), None);
        // 4799/480000 is under 1%, 4800/480000 is exactly 1% and therefore large
        assert_eq!(kind(with(jrow("a.html", "reftest", "a", "FAIL"), "relations", rel(4799, 480000, false))), Some(Kind::PixelsSmall));
        assert_eq!(kind(with(jrow("a.html", "reftest", "a", "FAIL"), "relations", rel(4800, 480000, false))), Some(Kind::PixelsLarge));
        // a size mismatch outranks the pixel count it also has
        assert_eq!(kind(with(jrow("a.html", "reftest", "a", "FAIL"), "relations", rel(1, 480000, true))), Some(Kind::SizeMismatch));
        // a reftest whose reference never rendered has no numbers at all
        assert_eq!(kind(with(jrow("a.html", "reftest", "a", "FAIL"), "relations", json!([{"pass": false, "error": "reference timed out"}]))), Some(Kind::Error));
        assert_eq!(kind(jrow("a.html", "testharness", "a", "TIMEOUT")), Some(Kind::Timeout));
        assert_eq!(kind(jrow("a.html", "reftest", "a", "TIMEOUT")), Some(Kind::Timeout));
        assert_eq!(kind(with(jrow("a.html", "testharness", "a", "ERROR"), "subtest_failures", json!(["FAIL x"]))), Some(Kind::Subtests));
        // a testharness failure that names no failing subtest is the harness itself
        assert_eq!(kind(jrow("a.html", "testharness", "a", "FAIL")), Some(Kind::Error));
        assert_eq!(kind(jrow("a.html", "crashtest", "a", "FAIL")), Some(Kind::Crash));
        assert_eq!(kind(jrow("a.html", "crashtest", "a", "TIMEOUT")), Some(Kind::Timeout));
    }

    #[test]
    fn clusters_group_by_key_order_by_failing_count_and_break_down_kinds() {
        let r = rows(&mixed());
        let c = cluster_list(&r);
        let keys: Vec<&str> = c.iter().map(|x| x.key.as_str()).collect();
        assert_eq!(
            keys,
            ["css/css-masking/clip-path/clip-path-inset", "svg/crash-opacity", "css/css-fonts/font-display", "css/css-fonts/font-face-rules"]
        );
        assert_eq!((c[0].failing, c[0].total), (3, 4), "three of the four inset tests fail, the -ref included");
        assert_eq!(kinds(&c[0]), [("size-mismatch", 1), ("pixels-small", 1), ("pixels-large", 1)]);
        assert_eq!(c[0].examples, ["css/css-masking/clip-path/clip-path-inset-001.html", "css/css-masking/clip-path/clip-path-inset-002.html", "css/css-masking/clip-path/clip-path-inset-ref.html"]);
        assert_eq!((c[1].failing, c[1].total), (2, 2));
        assert_eq!(kinds(&c[1]), [("timeout", 1), ("crash", 1)]);
        assert_eq!(kinds(&c[2]), [("timeout", 1)]);
        assert_eq!(kinds(&c[3]), [("subtests", 1)]);

        let text = clusters_markdown(&r, 0, Path::new("out/results.jsonl"), DEFAULT_TOP);
        assert!(text.contains("| reftest | 6 | 2 | 3 | 0 | 1 | 33.3% |"), "{text}");
        assert!(text.contains("| testharness | 2 | 1 | 1 | 0 | 0 | 50.0% |"), "{text}");
        assert!(text.contains("| **all** | 10 | 3 | 5 | 0 | 2 | 30.0% |"), "{text}");
        assert!(text.contains("7 failing tests in 4 clusters, 4 shown."), "{text}");
        assert!(text.contains("- failing 3 of 4 test(s)"), "{text}");

        // --top truncates and says what it left out
        let short = clusters_markdown(&r, 0, Path::new("out/results.jsonl"), 2);
        assert!(short.contains("2 further cluster(s) not shown, 2 failing test(s)."), "{short}");
        assert!(!short.contains("## css/css-fonts/font-face-rules"), "{short}");
    }

    #[test]
    fn clusters_of_a_run_with_nothing_failing_say_so() {
        let r = rows(&[jrow("a/b-001.html", "reftest", "a", "PASS"), jrow("a/b-002.html", "reftest", "a", "PASS")]);
        assert!(cluster_list(&r).is_empty());
        let text = clusters_markdown(&r, 0, Path::new("out/results.jsonl"), DEFAULT_TOP);
        assert!(text.contains("0 failing tests in 0 clusters, 0 shown."), "{text}");
        assert!(text.contains("Nothing failed."), "{text}");
    }

    #[test]
    fn compare_counts_fixed_and_regressed_per_dir_and_ignores_one_sided_tests() {
        let before = rows(&[
            jrow("a/one-001.html", "reftest", "a", "PASS"),
            jrow("a/two-001.html", "reftest", "a", "FAIL"),
            jrow("a/two-002.html", "reftest", "a", "TIMEOUT"),
            jrow("b/three-001.html", "reftest", "b", "PASS"),
            jrow("a/only-before-001.html", "reftest", "a", "PASS"),
        ]);
        let after = rows(&[
            jrow("a/one-001.html", "reftest", "a", "PASS"),
            jrow("a/two-001.html", "reftest", "a", "PASS"),
            jrow("a/two-002.html", "reftest", "a", "FAIL"),
            jrow("b/three-001.html", "reftest", "b", "TIMEOUT"),
            jrow("b/only-after-001.html", "reftest", "b", "FAIL"),
        ]);
        let c = compare_rows(&before, &after);
        assert_eq!((c.compared, c.only_before, c.only_after), (4, 1, 1), "only the tests in both are compared");
        assert_eq!((c.pass_before, c.pass_after), (2, 2));
        assert_eq!(
            c.dirs,
            vec![
                DirDelta { dir: "a".into(), tests: 3, pass_before: 1, pass_after: 2, fixed: 1, regressed: 0 },
                DirDelta { dir: "b".into(), tests: 1, pass_before: 1, pass_after: 0, fixed: 0, regressed: 1 },
            ]
        );
        assert_eq!(c.regressed, vec![("b/three-001.html".to_string(), "TIMEOUT".to_string())]);
        assert_eq!(c.still_failing, vec![("a/two".to_string(), 2, 1), ("b/three".to_string(), 0, 1)]);

        let text = compare_markdown(&c, Path::new("before"), Path::new("after"));
        assert!(text.contains("| a | 3 | 1 | 2 | 1 | 0 |"), "{text}");
        assert!(text.contains("Pass delta **+0**"), "{text}");
        assert!(text.contains("- `b/three-001.html` -> TIMEOUT"), "{text}");
        assert!(!text.contains("only-before"), "{text}");
        assert!(!text.contains("only-after"), "{text}");
    }

    #[test]
    fn compare_lists_every_regressed_path() {
        let before: Vec<Value> = (0..12).map(|i| jrow(&format!("css/x/t-{i:02}.html"), "reftest", "css/x", "PASS")).collect();
        let after: Vec<Value> = (0..12).map(|i| jrow(&format!("css/x/t-{i:02}.html"), "reftest", "css/x", "FAIL")).collect();
        let text = compare_markdown(&compare_rows(&rows(&before), &rows(&after)), Path::new("b"), Path::new("a"));
        assert!(text.contains("## Regressed (12)"), "{text}");
        for i in 0..12 {
            let p = format!("`css/x/t-{i:02}.html`");
            assert!(text.contains(&p), "{p} missing from the regressed list");
        }
        // and the still-failing cluster is counted, not truncated away
        assert!(text.contains("| `css/x/t` | 0 | 12 |"), "{text}");
    }

    #[test]
    fn report_reads_a_directory_and_rejects_bad_input_with_exit_3() {
        let d = scratch("clusters");
        fs::write(d.join("results.jsonl"), mixed().iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).expect("write results.jsonl");
        let dir = d.display().to_string();
        let md = d.join("report.md");
        let args = ["clusters", &dir, "--md", &md.display().to_string(), "--top", "2"].map(String::from).to_vec();
        assert_eq!(run(&args), 0);
        let text = fs::read_to_string(&md).expect("the markdown was written");
        assert!(text.contains("## css/css-masking/clip-path/clip-path-inset"), "{text}");
        assert!(text.contains("2 further cluster(s) not shown"), "{text}");

        let missing = d.join("nope").display().to_string();
        let cases = [
            vec![],
            vec!["bogus".into()],
            vec!["clusters".into()],
            vec!["clusters".into(), missing],
            vec!["clusters".into(), dir.clone(), "--top".into(), "many".into()],
            vec!["clusters".into(), dir.clone(), "--nope".into()],
            vec!["compare".into(), dir.clone()],
        ];
        for c in cases {
            assert_eq!(run(&c), 3, "{c:?}");
        }
        assert_eq!(run(&["compare".into(), dir.clone(), dir.clone()]), 0);
        assert_eq!(run(&["clusters".into(), dir.clone(), "--help".into()]), 0);
        // a torn last line is ignored rather than fatal
        fs::OpenOptions::new().append(true).open(d.join("results.jsonl")).and_then(|mut f| {
            use std::io::Write;
            f.write_all(b"\n{\"path\": \"trunc")
        })
        .expect("append a torn line");
        assert_eq!(run(&["clusters".into(), dir]), 0);
        fs::remove_dir_all(&d).ok();
    }
}
