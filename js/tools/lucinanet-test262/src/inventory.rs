//! Inventory of what the engine still lacks, computed from the runner's results.jsonl.
//!
//! Pure post-processor: it reads the `--out` files of one or two test262 runs and writes
//! TSV tables plus a markdown summary. No engine and no corpus are needed; the corpus root
//! is only used to shorten the paths.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One test variant as written by `lucinanet-test262 --out`.
#[derive(Debug, Clone, Deserialize)]
pub struct Row {
    pub path: String,
    pub variant: String,
    pub result: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug)]
pub struct Args {
    pub results: PathBuf,
    pub intl_results: Option<PathBuf>,
    pub out_dir: PathBuf,
    pub test262: Option<String>,
    pub help: bool,
}

/// The full command line, also printed by `lucinanet-test262 --help` (`lucinanet-inventory`
/// is the same command as a binary that does not link the engine).
pub const USAGE: &str =
    "lucinanet-test262 inventory --results <jsonl> [--intl-results <jsonl>] --out-dir <dir> [--test262 <corpus root>]";

pub fn parse_args<I: Iterator<Item = String>>(it: &mut I) -> Result<Args, String> {
    let mut args = Args {
        results: PathBuf::new(),
        intl_results: None,
        out_dir: PathBuf::new(),
        test262: None,
        help: false,
    };
    while let Some(flag) = it.next() {
        let mut take = |what: &str| -> Result<String, String> {
            it.next().ok_or(format!("{what} needs a value"))
        };
        match flag.as_str() {
            "--results" => args.results = PathBuf::from(take("--results")?),
            "--intl-results" => args.intl_results = Some(PathBuf::from(take("--intl-results")?)),
            "--out-dir" => args.out_dir = PathBuf::from(take("--out-dir")?),
            "--test262" => args.test262 = Some(take("--test262")?),
            "--help" | "-h" => args.help = true,
            other => return Err(usage(&format!("unknown argument: {other}"))),
        }
    }
    if args.help {
        return Ok(args);
    }
    if args.results.as_os_str().is_empty() {
        return Err(usage("--results <jsonl> is required"));
    }
    if args.out_dir.as_os_str().is_empty() {
        return Err(usage("--out-dir <dir> is required"));
    }
    Ok(args)
}

fn usage(problem: &str) -> String {
    if problem.is_empty() {
        format!("usage: {}", USAGE)
    } else {
        format!("{problem}\nusage: {}", USAGE)
    }
}

/// Pass, non-pass and skip counts of one group (a feature or an area).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub total: usize,
    pub pass: usize,
    pub fail: usize,
    pub skipped: usize,
}

impl Counts {
    fn add(&mut self, row: &Row) {
        self.total += 1;
        match row.result.as_str() {
            "pass" => self.pass += 1,
            "skipped" => self.skipped += 1,
            _ => self.fail += 1,
        }
    }

    /// Variants that are not a pass, skips included.
    pub fn non_passes(&self) -> usize {
        self.total - self.pass
    }

    pub fn pass_pct(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.pass as f64 * 100.0 / self.total as f64
        }
    }

    pub fn status(&self) -> &'static str {
        if self.total > 0 && self.pass == self.total {
            "complete"
        } else if self.total > 0 && self.skipped == self.total {
            "unsupported-by-runner"
        } else if self.pass == 0 {
            "none"
        } else {
            "partial"
        }
    }
}

/// Rank order used by every table: most non-passes first, then by name.
fn rank(a: &(&String, &Counts), b: &(&String, &Counts)) -> std::cmp::Ordering {
    b.1.non_passes()
        .cmp(&a.1.non_passes())
        .then_with(|| a.0.cmp(b.0))
}

pub fn ranked(map: &BTreeMap<String, Counts>) -> Vec<(&String, &Counts)> {
    let mut v: Vec<(&String, &Counts)> = map.iter().collect();
    v.sort_by(rank);
    v
}

pub fn group_by_feature(rows: &[Row]) -> BTreeMap<String, Counts> {
    let mut map: BTreeMap<String, Counts> = BTreeMap::new();
    for row in rows {
        for feature in &row.features {
            map.entry(feature.clone()).or_default().add(row);
        }
    }
    map
}

/// The path relative to `<corpus>/test/`. With a root it is a prefix strip; without one the
/// first `/test/` in the path is the boundary, so a directory really named `test` inside the
/// corpus (built-ins/RegExp/prototype/test/) keeps its prefix.
pub fn relative_path(path: &str, root: Option<&str>) -> String {
    if let Some(root) = root {
        let prefix = format!("{}/test/", root.trim_end_matches('/'));
        if let Some(rest) = path.strip_prefix(&prefix) {
            return rest.to_string();
        }
    }
    match path.find("/test/") {
        Some(i) => path[i + "/test/".len()..].to_string(),
        None => path.to_string(),
    }
}

/// test262 directory area: the first two directory components of the test, plus a third when
/// a `built-ins/` or `intl402/` area is named `Intl` or `Temporal`. A test that sits directly
/// in a directory names that directory, not itself.
pub fn area_of(rel: &str) -> String {
    let mut parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    if parts.last().map(|p| p.contains('.')).unwrap_or(false) {
        parts.pop();
    }
    let deep = parts.len() > 2
        && matches!(parts[0], "built-ins" | "intl402")
        && matches!(parts[1], "Intl" | "Temporal");
    let keep = if deep { 3 } else { parts.len().min(2) };
    if keep == 0 {
        return "(root)".to_string();
    }
    parts[..keep].join("/")
}

pub fn group_by_area(rows: &[Row], root: Option<&str>) -> BTreeMap<String, Counts> {
    let mut map: BTreeMap<String, Counts> = BTreeMap::new();
    for row in rows {
        map.entry(area_of(&relative_path(&row.path, root)))
            .or_default()
            .add(row);
    }
    map
}

#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub message: String,
    pub count: usize,
    /// The feature attached to most of these variants, empty when none was.
    pub feature: String,
    /// One `path [variant]` example.
    pub example: String,
}

#[derive(Default)]
struct FailureAcc {
    count: usize,
    example: String,
    features: BTreeMap<String, usize>,
}

/// Distinct failing messages (fail, timeout, crash), most frequent first, then by text.
/// Rows are visited in path order so a group's example does not depend on input order.
pub fn top_failures(rows: &[Row], limit: usize, root: Option<&str>) -> Vec<Failure> {
    let mut map: BTreeMap<String, FailureAcc> = BTreeMap::new();
    let mut order: Vec<&Row> = rows.iter().collect();
    order.sort_by(|a, b| a.path.cmp(&b.path).then(a.variant.cmp(&b.variant)));
    for row in order
        .into_iter()
        .filter(|r| r.result != "pass" && r.result != "skipped")
    {
        let key = normalize_message(&row.message);
        let acc = map.entry(key).or_insert_with(FailureAcc::default);
        if acc.count == 0 {
            acc.example = format!("{} [{}]", relative_path(&row.path, root), row.variant);
        }
        acc.count += 1;
        for f in &row.features {
            *acc.features.entry(f.clone()).or_insert(0) += 1;
        }
    }
    let mut failures: Vec<Failure> = map
        .into_iter()
        .map(|(message, acc)| {
            let feature = top_feature(&acc.features);
            Failure {
                count: acc.count,
                example: acc.example,
                feature,
                message,
            }
        })
        .collect();
    failures.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.message.cmp(&b.message)));
    failures.truncate(limit);
    failures
}

/// Most frequent key; ties go to the first name, so the table is stable.
fn top_feature(features: &BTreeMap<String, usize>) -> String {
    match features.values().max() {
        Some(best) => features
            .iter()
            .find(|(_, count)| *count == best)
            .map(|(name, _)| name.clone())
            .unwrap_or_default(),
        None => String::new(),
    }
}

/// Numbers become `N` and backtick-, double-quote- and «»-quoted text becomes `X`, so that
/// equal causes group whatever test-specific values separate them.
pub fn normalize_message(message: &str) -> String {
    let chars: Vec<char> = message
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let quotes = [('\u{60}', '\u{60}'), ('"', '"'), ('\u{ab}', '\u{bb}')];
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some((_, close)) = quotes.iter().find(|(open, _)| chars[i] == *open) {
            if let Some(j) = chars[i + 1..].iter().position(|c| c == close).map(|d| i + 1 + d) {
                out.push('X');
                i = j + 1;
                continue;
            }
        }
        if chars[i].is_ascii_digit() {
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            out.push('N');
            continue;
        }
        if chars[i] != ' ' || !out.ends_with(' ') {
            out.push(chars[i]);
        }
        i += 1;
    }
    out.trim().to_string()
}

fn header(sources: &str, lines: &[&str], columns: &str) -> String {
    let mut out = String::new();
    out.push_str("# Generated by `lucinanet-inventory` (tools/lucinanet-test262); do not edit by hand.\n");
    out.push_str(&format!("# Sources: {sources}\n"));
    out.push_str("# Each test variant (sloppy, strict, module, raw) counts once, as the runner reports them separately.\n");
    for line in lines {
        out.push_str(&format!("# {line}\n"));
    }
    out.push_str(&format!("# Columns: {columns}\n"));
    out
}

fn render_groups(name: &str, about: &str, groups: &[(&String, &Counts)], sources: &str) -> String {
    let mut out = header(
        sources,
        &[
            about,
            "`fail` is fail, timeout and crash; `skipped` is a variant the runner did not execute.",
            "status: complete (all pass), unsupported-by-runner (all skipped), none (no pass, at least one executed), partial.",
            "Rows are ranked by non-passing variants, then by name.",
        ],
        &format!("{name}, total, pass, fail, skipped, pass_pct, status"),
    );
    out.push_str(&format!("{name}\ttotal\tpass\tfail\tskipped\tpass_pct\tstatus\n"));
    for (key, c) in groups {
        out.push_str(&format!(
            "{key}\t{}\t{}\t{}\t{}\t{:.2}\t{}\n",
            c.total,
            c.pass,
            c.fail,
            c.skipped,
            c.pass_pct(),
            c.status()
        ));
    }
    out
}

pub fn render_features(groups: &[(&String, &Counts)], sources: &str) -> String {
    render_groups(
        "feature",
        "A row per test262 `features:` value; a test that lists several counts under each of them.",
        groups,
        sources,
    )
}

pub fn render_areas(groups: &[(&String, &Counts)], sources: &str) -> String {
    render_groups(
        "area",
        "A row per test262 directory area: the first two directories under `test/`, plus a third \
         when `built-ins` or `intl402` names `Intl` or `Temporal`; a test directly in a directory \
         names that directory.",
        groups,
        sources,
    )
}

pub fn render_failures(failures: &[Failure], sources: &str) -> String {
    let mut out = header(
        sources,
        &[
            "The 200 most frequent failing message texts (skips are not failures), ranked by count then text.",
            "A number is N and quoted text is X, so equal causes group; `example` keeps one variant of the first test seen.",
        ],
        "message, count, feature, example",
    );
    out.push_str("message\tcount\tfeature\texample\n");
    for f in failures {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            f.message.replace('\t', " "),
            f.count,
            f.feature,
            f.example
        ));
    }
    out
}

/// Newest modification date (UTC) of the given files, as `YYYY-MM-DD`.
pub fn source_stamp(path: &Path) -> Option<String> {
    let time = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = time.duration_since(std::time::UNIX_EPOCH).ok()?;
    let (y, m, d) = civil_from_days((secs.as_secs() / 86_400) as i64);
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// Howard Hinnant's civil_from_days, so a date stamp needs no date crate.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

fn source_list(paths: &[&Path]) -> String {
    paths
        .iter()
        .map(|p| match source_stamp(p) {
            Some(d) => format!("{} ({d})", p.display()),
            None => format!("{}", p.display()),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every variant, grouped by nothing: the totals line of the summary.
pub fn totals(rows: &[Row]) -> Counts {
    let mut c = Counts::default();
    for row in rows {
        c.add(row);
    }
    c
}

/// The summary, under 120 lines: totals, the top 25 features, every feature with no pass,
/// every area below 90%, and the features the runner skips. Each figure is a `Counts` the
/// TSVs show too.
pub fn render_markdown(
    features: &BTreeMap<String, Counts>,
    areas: &BTreeMap<String, Counts>,
    overall: &Counts,
    sources: &str,
) -> String {
    let feats = ranked(features);
    let areas_ranked = ranked(areas);
    let names = |wanted: &str| -> Vec<String> {
        feats
            .iter()
            .filter(|(_, c)| c.status() == wanted)
            .map(|(k, _)| k.to_string())
            .collect()
    };
    let unsupported = names("unsupported-by-runner");
    let none = names("none");
    let weak: Vec<(&String, &Counts)> = areas_ranked
        .iter()
        .filter(|(_, c)| c.pass_pct() < 90.0)
        .map(|(k, c)| (*k, *c))
        .collect();

    let mut out = String::new();
    out.push_str("# What LucinaJS lacks, by test262\n\n");
    out.push_str(&format!(
        "Generated by `lucinanet-inventory` from {sources}. One count per test variant \
         (sloppy, strict, module, raw); a test with several `features:` counts under each. \
         `fail` is fail, timeout and crash; `skipped` is what the runner did not execute. \
         Full tables: missing-test262-features.tsv, missing-test262-areas.tsv, \
         missing-test262-top-failures.tsv.\n\n"
    ));
    out.push_str("## Totals\n\n");
    out.push_str(&format!(
        "{} variants: {} pass ({:.2}%), {} fail, {} skipped. Areas cover every variant, \
         so their counts add up to these; features cover only tests that declare some.\n\n",
        overall.total, overall.pass, overall.pass_pct(), overall.fail, overall.skipped
    ));

    out.push_str("## The 25 features with the most non-passing variants\n");
    out.push_str("```\nfeature\ttotal\tnon_pass\tpass_pct\tstatus\n");
    for &(f, c) in feats.iter().take(25) {
        out.push_str(&summary_row(f, *c));
    }
    out.push_str("```\n\n");

    out.push_str(&format!(
        "## Features with no passing variant ({} of {})\n\n",
        none.len(),
        feats.len()
    ));
    out.push_str(&list_or_none(&none));

    out.push_str(&format!(
        "## Areas below 90% pass ({} of {})\n",
        weak.len(),
        areas_ranked.len()
    ));
    out.push_str("```\narea\ttotal\tnon_pass\tpass_pct\tstatus\n");
    for &(a, c) in &weak {
        out.push_str(&summary_row(a, *c));
    }
    out.push_str("```\n\n");

    out.push_str(&format!(
        "## Features the runner does not support at all ({}): every variant skipped\n\n",
        unsupported.len()
    ));
    out.push_str(&list_or_none(&unsupported));
    out
}

fn summary_row(name: &str, c: Counts) -> String {
    format!(
        "{name}\t{}\t{}\t{:.2}\t{}\n",
        c.total,
        c.non_passes(),
        c.pass_pct(),
        c.status()
    )
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        return "(none)\n\n".to_string();
    }
    let mut out = String::new();
    let mut line = String::new();
    for item in items {
        if line.chars().count() + item.chars().count() + 2 > 118 {
            out.push_str(line.trim_end().trim_end_matches(','));
            out.push('\n');
            line = String::new();
        }
        line.push_str(item);
        line.push_str(", ");
    }
    out.push_str(line.trim_end().trim_end_matches(','));
    out.push_str("\n\n");
    out
}

pub fn read_rows(path: &Path) -> Result<Vec<Row>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut rows = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: Row = serde_json::from_str(line)
            .map_err(|e| format!("{}:{}: bad result line: {e}", path.display(), i + 1))?;
        rows.push(row);
    }
    Ok(rows)
}

/// Parse command-line arguments and write the inventory.
pub fn run_args(args: &[String]) -> Result<(), String> {
    run(&parse_args(&mut args.iter().cloned())?)
}

/// Read the runs, then write the four files.
pub fn run(args: &Args) -> Result<(), String> {
    if args.help {
        println!("usage: {}", USAGE);
        return Ok(());
    }
    let mut sources: Vec<&Path> = vec![args.results.as_path()];
    if let Some(intl) = &args.intl_results {
        sources.push(intl.as_path());
    }
    let mut rows: Vec<Row> = Vec::new();
    for path in &sources {
        rows.append(&mut read_rows(path)?);
    }

    let sources = source_list(&sources);
    let features = group_by_feature(&rows);
    let areas = group_by_area(&rows, args.test262.as_deref());
    let failures = top_failures(&rows, 200, args.test262.as_deref());
    let overall = totals(&rows);
    let files = [
        (
            "missing-test262-features.tsv",
            render_features(&ranked(&features), &sources),
        ),
        (
            "missing-test262-areas.tsv",
            render_areas(&ranked(&areas), &sources),
        ),
        (
            "missing-test262-top-failures.tsv",
            render_failures(&failures, &sources),
        ),
        ("missing-test262.md", render_markdown(&features, &areas, &overall, &sources)),
    ];
    std::fs::create_dir_all(&args.out_dir)
        .map_err(|e| format!("cannot create {}: {e}", args.out_dir.display()))?;
    for (name, text) in &files {
        let path = args.out_dir.join(name);
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        eprintln!("{}: {} bytes", path.display(), text.len());
    }
    eprintln!(
        "{} variants: {} pass, {} fail, {} skipped; {} features, {} areas, {} distinct failing messages",
        overall.total,
        overall.pass,
        overall.fail,
        overall.skipped,
        features.len(),
        areas.len(),
        failures.len()
    );
    Ok(())
}
