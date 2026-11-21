//! Inventory of what real sites find missing at run time, computed from their logs.
//!
//! Pure post-processor: it reads the `lucinanet --render` stderr logs of the browser and the
//! `LUCINANET_JS_MISSING_LOG` files the glue writes, and writes the two tables of docs/missing.md
//! section 3. No engine, no mozjs, no network. The output is deterministic (files by path, rows by
//! rank then text), so diffs between runs stay small.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

pub mod names;

/// What kind of thing a row is about. The order is the tie-break inside one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// An uncaught script error, from a `console Error:` line or a `MISSING_LOG` `error` line.
    Error,
    /// A glue function with no implementation yet, from its abort line or a `MISSING_LOG` `missing` line.
    Missing,
    /// A `console Warn:` line: the page noticed something absent without dying on it.
    Warn,
    /// The glue's own `lucinanet-js:` note.
    Js,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Error => "error",
            Kind::Missing => "missing",
            Kind::Warn => "warn",
            Kind::Js => "js",
        }
    }

    fn parse(text: &str) -> Option<Kind> {
        match text {
            "error" => Some(Kind::Error),
            "missing" => Some(Kind::Missing),
            "warn" => Some(Kind::Warn),
            "js" => Some(Kind::Js),
            _ => None,
        }
    }
}

/// One event as read from one site's log, before grouping. Tabs and newlines cannot survive in it,
/// because they are the row and field separators of the output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub kind: Kind,
    pub message: String,
    /// Where it happened, `<file>:<line>`, when the log says.
    pub location: String,
}

fn event(kind: Kind, message: &str, location: &str) -> Event {
    Event { kind, message: message.replace(['\t', '\r', '\n'], " "), location: location.replace(['\t', '\r', '\n'], " ") }
}

/// What a `not implemented` event says, in one wording for both channels: the glue's
/// `MISSING_LOG` line carries the bare name, the abort prints a sentence, and a stub seen through both
/// is one row.
fn miss_text(name: &str) -> String {
    format!("{name} is not implemented")
}

/// A line of a `LUCINANET_JS_MISSING_LOG` file: `<unix_ms>\t<kind>\t<text>` and, for an error with a
/// frame, `\t<file>:<line>`. `None` for anything else, which is what lets a stderr line and a log line
/// be read from the same file.
pub fn parse_log_line(line: &str) -> Option<Event> {
    let mut fields = line.trim_end().split('\t');
    let ms = fields.next()?;
    if ms.is_empty() || !ms.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let kind = Kind::parse(fields.next()?)?;
    let text = fields.next()?.trim();
    if text.is_empty() {
        return None;
    }
    let at = fields.next().unwrap_or("");
    Some(match kind {
        Kind::Missing => event(kind, &miss_text(text), ""),
        other => event(other, text, at),
    })
}

/// A line of a site's stderr: the console messages the browser forwards, the glue's own notes, and the
/// abort line of a stub. Everything else (phases, timings, network) is not a missing feature.
pub fn parse_stderr_line(line: &str) -> Option<Event> {
    let line = line.trim_end();
    if let Some(rest) = line.strip_prefix("console Error: Error at ") {
        // `<url>:<line>:<col> <Type>: <message>`: the location is one token, the message is the rest.
        let (at, message) = rest.split_once(' ')?;
        return Some(event(Kind::Error, message, &location_of(at)));
    }
    if let Some(rest) = line.strip_prefix("console Error: Unhandled promise rejection: ") {
        return Some(event(Kind::Error, rest, ""));
    }
    if let Some(rest) = line.strip_prefix("console Error: ") {
        return Some(event(Kind::Error, rest, ""));
    }
    if let Some(rest) = line.strip_prefix("console Warn: ") {
        return Some(event(Kind::Warn, rest, ""));
    }
    if let Some(rest) = line.strip_prefix("lucinanet-js: ") {
        return Some(event(Kind::Js, rest, ""));
    }
    let name = line.strip_suffix(" is not implemented")?;
    Some(event(Kind::Missing, &miss_text(name.strip_prefix("mozjs_sys (Rust): ").unwrap_or(name)), ""))
}

/// `<url>:<line>:<col>` as the `<url>:<line>` of the example column; a token that is not of that shape
/// is kept whole.
fn location_of(token: &str) -> String {
    let without_col = match token.rsplit_once(':') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => return token.to_string(),
    };
    match without_col.rsplit_once(':') {
        // A token with no file in front of it (`:0:0`, an error the page did not originate) is not a
        // place in a script, and a location column of `:0` would only be noise.
        Some((url, line)) if !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()) => {
            if url.is_empty() {
                String::new()
            } else {
                format!("{url}:{line}")
            }
        }
        _ => token.to_string(),
    }
}

/// One line of one log, whichever channel it came from.
pub fn parse_line(line: &str) -> Option<Event> {
    parse_log_line(line).or_else(|| parse_stderr_line(line))
}

/// A character that makes a digit run part of a name rather than a number on its own (`Int8Array`).
fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// `true` for a quoted run that reads as a JavaScript name (`Intl`, `Int8Array`,
/// `glue::NewExternalArrayBuffer`) rather than as a value (a sentence, a path, a number, a URL, a word
/// of prose in any language).
fn is_quoted_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.' | ':'))
}

const QUOTES: [char; 3] = ['\u{60}', '"', '\u{ab}'];
const CLOSES: [char; 3] = ['\u{60}', '"', '\u{bb}'];

/// Text quoted with backticks, double quotes or `«»` is a value and becomes `X`, except a name, which
/// keeps its text without the quotes. The run-time table exists to say *which* thing is missing, so
/// `` `Intl` `` survives normalisation while `` `a very long value` `` does not; the cost is that two
/// messages differing only in a property name stay two rows.
fn unquote_names(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some(q) = QUOTES.iter().position(|open| chars[i] == *open) {
            if let Some(close) = chars[i + 1..].iter().position(|c| *c == CLOSES[q]).map(|d| i + 1 + d) {
                let inner: String = chars[i + 1..close].iter().collect();
                if is_quoted_name(&inner) {
                    out.push_str(&inner);
                } else {
                    out.push('X');
                }
                i = close + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// A URL is one thing however long it is.
fn is_url(token: &str) -> bool {
    token.contains("://") || token.starts_with("//") || token.starts_with("www.")
}

/// One whitespace-separated word: `U` for a URL, `H` for a build hash (twelve or more hex digits with a
/// digit in it, on its own), `N` for a number on its own. A digit inside a name is left alone, or every
/// `Int8Array` in the table would be an `IntNArray`.
fn normalize_token(token: &str) -> String {
    if is_url(token) {
        return String::from("U");
    }
    let c: Vec<char> = token.chars().collect();
    let mut out = String::with_capacity(token.len());
    let mut i = 0;
    while i < c.len() {
        if !c[i].is_ascii_hexdigit() {
            out.push(c[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < c.len() && c[i].is_ascii_hexdigit() {
            i += 1;
        }
        let run = &c[start..i];
        let in_name = (start > 0 && is_name_char(c[start - 1])) || (i < c.len() && is_name_char(c[i]));
        let digits = run.iter().filter(|d| d.is_ascii_digit()).count();
        if digits == 0 || in_name {
            out.extend(run);
        } else if run.len() >= 12 {
            out.push('H');
        } else {
            out.push('N');
        }
    }
    out
}

/// The message with what varies between two hits of one cause replaced, so that they are one row:
/// whitespace collapsed, a URL `U`, a hash `H`, a number on its own `N`, a quoted value `X`.
pub fn normalize(message: &str) -> String {
    let flat = message.split_whitespace().collect::<Vec<_>>().join(" ");
    unquote_names(&flat).split(' ').map(normalize_token).collect::<Vec<_>>().join(" ")
}

/// The distinct sites behind each kind, for the totals: counted over the events, so a site that hit the
/// same message twice is one site.
pub fn sites_per_kind(events: &[(String, Event)]) -> BTreeMap<Kind, usize> {
    let mut map: BTreeMap<Kind, BTreeSet<String>> = BTreeMap::new();
    for (site, e) in events {
        map.entry(e.kind).or_default().insert(site.clone());
    }
    map.into_iter().map(|(kind, sites)| (kind, sites.len())).collect()
}

/// What one normalised message gathered across the sites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub kind: Kind,
    pub message: String,
    pub sites: usize,
    pub hits: usize,
    pub example_site: String,
    pub example_location: String,
}

#[derive(Default)]
struct Group {
    hits: usize,
    sites: BTreeSet<String>,
    example_site: String,
    example_location: String,
}

/// Groups the events, which must arrive in a deterministic order (see `read_events`), and ranks them:
/// most sites first, then most hits, then kind, then text.
///
/// The example is the first hit that names a location, so that the last column is rarely empty, and
/// the first hit when no location was logged anywhere.
pub fn aggregate(events: &[(String, Event)]) -> Vec<Row> {
    let mut map: BTreeMap<(Kind, String), Group> = BTreeMap::new();
    for (site, e) in events {
        let group = map.entry((e.kind, normalize(&e.message))).or_default();
        group.hits += 1;
        group.sites.insert(site.clone());
        if group.example_site.is_empty() || (group.example_location.is_empty() && !e.location.is_empty()) {
            group.example_site = site.clone();
            group.example_location = e.location.clone();
        }
    }
    let mut rows: Vec<Row> = map
        .into_iter()
        .map(|((kind, message), g)| Row {
            kind,
            message,
            sites: g.sites.len(),
            hits: g.hits,
            example_site: g.example_site,
            example_location: g.example_location,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.sites.cmp(&a.sites).then(b.hits.cmp(&a.hits)).then(a.kind.cmp(&b.kind)).then(a.message.cmp(&b.message))
    });
    rows
}

/// The extensions a directory scan takes. The run directory holds the page screenshots and the stdout
/// of the browser too, and neither is a log; a file named on the command line is read whatever it is
/// called.
const LOG_EXTENSIONS: &[&str] = &["err", "log", "missing", "mlog", "stderr", "txt"];

/// The log files to read: a directory contributes its regular files with a log extension, a file
/// argument contributes itself. Sorted by path, so which hit is the example never depends on the order
/// the arguments came in.
pub fn collect_logs(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut found = BTreeSet::new();
    for input in inputs {
        if input.is_dir() {
            let entries = std::fs::read_dir(input).map_err(|e| format!("cannot read {}: {e}", input.display()))?;
            for entry in entries.flatten() {
                let path = entry.path();
                let taken = path.is_file()
                    && path.extension().and_then(|e| e.to_str()).is_some_and(|e| LOG_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()));
                if taken {
                    found.insert(path);
                }
            }
        } else if input.is_file() {
            found.insert(input.clone());
        } else {
            return Err(format!("no log to read at {}", input.display()));
        }
    }
    if found.is_empty() {
        return Err(format!("no log file ({}) in the {} input(s) given", LOG_EXTENSIONS.join(" or ."), inputs.len()));
    }
    Ok(found.into_iter().collect())
}

/// The site a file is attributed to: its stem, so `sites/github.err` and `sites/github.missing` are the
/// same site. `docs/missing/sites-script-heavy.txt` names the stems the run writes.
pub fn site_of(path: &Path) -> String {
    path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string()
}

/// Every event of every file, in file order then line order. A file that is not text is skipped with a
/// note: one stray binary in a run directory is not a reason to lose the run.
pub fn read_events(paths: &[PathBuf]) -> Vec<(String, Event)> {
    let mut events = Vec::new();
    for path in paths {
        let site = site_of(path);
        match std::fs::read_to_string(path) {
            Ok(text) => events.extend(text.lines().filter_map(parse_line).map(|e| (site.clone(), e))),
            Err(e) => eprintln!("{}: skipped ({e})", path.display()),
        }
    }
    events
}

/// Newest modification date (UTC) of the inputs, as `YYYY-MM-DD`, the date the table is from.
pub fn newest_stamp(paths: &[&Path]) -> Option<String> {
    let secs = paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok()?.modified().ok())
        .filter_map(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() / 86_400)
        .max()?;
    Some(format_date(secs as i64))
}

/// Howard Hinnant's civil_from_days, so a date stamp needs no date crate (as in `lucinanet-inventory`).
fn format_date(days: i64) -> String {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

pub struct Summary<'a> {
    pub rows: &'a [Row],
    pub kind_sites: &'a BTreeMap<Kind, usize>,
    pub files: usize,
    pub sites: usize,
    /// Sites whose logs held nothing that counts as a missing feature.
    pub silent: usize,
    pub events: usize,
    pub stamp: Option<String>,
    pub build: &'a str,
}

fn header(about: &[&str], columns: &str, s: &Summary) -> String {
    let mut out = String::from("# Generated by `lucinanet-sitelog` (tools/lucinanet-sitelog); do not edit by hand.\n");
    out.push_str(&format!(
        "# Sources: {} log file(s), {} site(s), {} event(s){}.\n",
        s.files,
        s.sites,
        s.events,
        match &s.stamp {
            Some(d) => format!(", newest of them dated {d}"),
            None => String::new(),
        }
    ));
    if !s.build.is_empty() {
        out.push_str(&format!("# Build: {}.\n", s.build));
    }
    for line in about {
        out.push_str(&format!("# {line}\n"));
    }
    out.push_str(&format!("# Columns: {columns}\n"));
    out
}

/// The table: one row per kind and normalised message, ranked by sites then hits then kind then text.
pub fn render_tsv(s: &Summary) -> String {
    let mut out = header(
        &[
            "A row is one message that several hits share after normalisation: a URL is U, a build hash H, a number on its",
            "own N, text quoted as a value X, and a name quoted with backticks keeps its text, because naming it is the point.",
            "`missing` rows are glue functions with no implementation yet, printed as `<name> is not implemented`. The",
            "`error`, `warn` and `js` rows are what a script threw, what the page warned about and what the glue noted, from",
            "the site's stderr or from LUCINANET_JS_MISSING_LOG. `sites` counts distinct file stems, `hits` every line read.",
        ],
        "kind, normalised message, sites, total hits, example site, example location",
        s,
    );
    out.push_str("kind\tnormalised message\tsites\ttotal hits\texample site\texample location\n");
    for r in s.rows {
        out.push_str(&format!("{}\t{}\t{}\t{}\t{}\t{}\n", r.kind.as_str(), r.message, r.sites, r.hits, r.example_site, r.example_location));
    }
    out
}

/// A location in the summary keeps its tail, where the file and the line are; a bundle URL with a
/// megabyte-long path is unreadable here and stays whole in the TSV.
fn short_location(at: &str) -> String {
    let chars: Vec<char> = at.chars().collect();
    if chars.len() <= 72 {
        return at.to_string();
    }
    let mut out = String::from("…");
    out.extend(chars[chars.len() - 70..].iter());
    out
}

/// The human summary: totals, what the most sites hit, and the rows that name something missing.
/// Kept under 80 lines; `missing-runtime.tsv` is the whole table.
pub fn render_md(s: &Summary) -> String {
    const TOP: usize = 20;
    const GAP_ROWS: usize = 15;
    let mut out = String::from("# What real sites hit at run time\n\n");
    out.push_str(&format!(
        "Generated by `lucinanet-sitelog` from {} log file(s) over {} site(s), {} event(s){}.\n",
        s.files,
        s.sites,
        s.events,
        match &s.stamp {
            Some(d) => format!(" dated {d}"),
            None => String::new(),
        }
    ));
    // Its own line, because a build string is long and a sentence that carries it becomes unreadable.
    if !s.build.is_empty() {
        out.push_str(&format!("Build: {}\n", s.build));
    }
    out.push('\n');
    out.push_str(
        "Rules of the normalisation and of the engine/web-engine split: `tools/lucinanet-sitelog/README.md`.\n\
         Every row of the whole table is in [missing-runtime.tsv](missing-runtime.tsv); this page is the short read.\n\n",
    );

    out.push_str("## Totals\n\n");
    out.push_str("```\nkind\trows\thits\tsites\n");
    for kind in [Kind::Error, Kind::Missing, Kind::Warn, Kind::Js] {
        let rows: Vec<&Row> = s.rows.iter().filter(|r| r.kind == kind).collect();
        if rows.is_empty() {
            continue;
        }
        let hits: usize = rows.iter().map(|r| r.hits).sum();
        out.push_str(&format!("{}\t{}\t{}\t{}\n", kind.as_str(), rows.len(), hits, s.kind_sites.get(&kind).copied().unwrap_or(0)));
    }
    out.push_str("```\n\n");
    out.push_str(&format!(
        "{} rows in all; {} of the {} site(s) logged nothing that counts as a missing feature.\n\n",
        s.rows.len(),
        s.silent,
        s.sites
    ));

    out.push_str(&format!("## What the most sites hit (top {})\n\n", TOP));
    out.push_str("```\nsites\thits\tkind\tmessage\texample location\n");
    for r in s.rows.iter().take(TOP) {
        out.push_str(&format!("{}\t{}\t{}\t{}\t{}\n", r.sites, r.hits, r.kind.as_str(), r.message, short_location(&r.example_location)));
    }
    out.push_str("```\n\n");

    let gaps: Vec<&Row> = s.rows.iter().filter(|r| names::missing_name(&r.message).is_some()).collect();
    out.push_str(&format!(
        "## Rows that name a missing built-in or global ({} of {} rows)\n\n",
        gaps.len(),
        s.rows.len()
    ));
    out.push_str("```\nowner\tname\tsites\thits\tkind\tmessage\n");
    for r in gaps.iter().take(GAP_ROWS) {
        let name = names::missing_name(&r.message).unwrap_or_default();
        let stub = r.kind == Kind::Missing;
        out.push_str(&format!("{}\t{}\t{}\t{}\t{}\t{}\n", names::owner(name, stub), name, r.sites, r.hits, r.kind.as_str(), r.message));
    }
    out.push_str("```\n");
    if gaps.len() > GAP_ROWS {
        out.push_str(&format!("\n…and {} more rows of the same shape in the TSV.\n", gaps.len() - GAP_ROWS));
    }
    out.push_str("\n`owner` is `engine` for a name ECMAScript or ECMA-402 defines, `web-engine` for a Web platform one, and\nfor anything else it follows the channel: a `not implemented` line is the glue's, a name a page reached\nfor is the web engine's.\n");
    out
}

/// What the tool was asked to do.
#[derive(Debug)]
pub struct Args {
    pub logs: Vec<PathBuf>,
    pub out_dir: PathBuf,
    pub build: String,
    pub help: bool,
}

pub const USAGE: &str = "lucinanet-sitelog --logs <dir-or-file>... --out-dir <dir> [--build <what produced the logs>]";

pub fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args { logs: Vec::new(), out_dir: PathBuf::new(), build: String::new(), help: false };
    let mut i = 0;
    let value = |i: &mut usize, what: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i).cloned().ok_or(format!("{what} needs a value"))
    };
    while i < args.len() {
        let flag = args[i].as_str();
        match flag {
            // `--logs` takes every following word up to the next flag, so one run can name a directory
            // and the two or three files that sit outside it.
            "--logs" => {
                while i + 1 < args.len() && !args[i + 1].starts_with('-') {
                    i += 1;
                    out.logs.push(PathBuf::from(args[i].clone()));
                }
                if out.logs.is_empty() {
                    return Err(usage("--logs needs at least one directory or file"));
                }
            }
            "--out-dir" => out.out_dir = PathBuf::from(value(&mut i, "--out-dir")?),
            "--build" => out.build = value(&mut i, "--build")?,
            "--help" | "-h" => out.help = true,
            other => return Err(usage(&format!("unknown argument: {other}"))),
        }
        i += 1;
    }
    if out.help {
        return Ok(out);
    }
    if out.logs.is_empty() {
        return Err(usage("--logs <dir-or-file>... is required"));
    }
    if out.out_dir.as_os_str().is_empty() {
        return Err(usage("--out-dir <dir> is required"));
    }
    Ok(out)
}

fn usage(problem: &str) -> String {
    if problem.is_empty() {
        format!("usage: {USAGE}")
    } else {
        format!("{problem}\nusage: {USAGE}")
    }
}

pub fn run_args(args: &[String]) -> Result<(), String> {
    run(&parse_args(args)?)
}

/// Reads the logs, then writes the two files.
pub fn run(args: &Args) -> Result<(), String> {
    if args.help {
        println!("usage: {USAGE}");
        return Ok(());
    }
    let paths = collect_logs(&args.logs)?;
    let given: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let events = read_events(&paths);
    let seen: BTreeSet<String> = paths.iter().map(|p| site_of(p)).collect();
    let loud: BTreeSet<String> = events.iter().map(|(site, _)| site.clone()).collect();
    let kind_sites = sites_per_kind(&events);
    let rows = aggregate(&events);
    let stamp = newest_stamp(&given);
    let summary = Summary {
        rows: &rows,
        kind_sites: &kind_sites,
        files: paths.len(),
        sites: seen.len(),
        silent: seen.len() - loud.len(),
        events: events.len(),
        stamp: stamp.clone(),
        build: &args.build,
    };
    let files = [
        ("missing-runtime.tsv", render_tsv(&summary)),
        ("missing-runtime.md", render_md(&summary)),
    ];
    std::fs::create_dir_all(&args.out_dir).map_err(|e| format!("cannot create {}: {e}", args.out_dir.display()))?;
    for (name, text) in &files {
        let path = args.out_dir.join(name);
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        eprintln!("{}: {} bytes", path.display(), text.len());
    }
    eprintln!(
        "{} log file(s), {} site(s) of which {} said nothing, {} event(s), {} row(s), {} site(s) hit{}",
        paths.len(),
        seen.len(),
        summary.silent,
        events.len(),
        rows.len(),
        loud.len(),
        stamp.map(|d| format!(", newest dated {d}")).unwrap_or_default()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(line: &str) -> Option<(Kind, String, String)> {
        parse_line(line).map(|e| (e.kind, e.message, e.location))
    }

    #[test]
    fn a_console_error_keeps_its_message_and_its_file_and_line() {
        assert_eq!(
            shape("console Error: Error at https://cdn.example.com/app-a0e205571facc60b.js:2:184560 ReferenceError: `Intl` is not defined"),
            Some((
                Kind::Error,
                "ReferenceError: `Intl` is not defined".into(),
                "https://cdn.example.com/app-a0e205571facc60b.js:2".into()
            ))
        );
    }

    #[test]
    fn the_other_stderr_shapes() {
        assert_eq!(
            shape("console Error: Unhandled promise rejection: TypeError: nope"),
            Some((Kind::Error, "TypeError: nope".into(), String::new()))
        );
        assert_eq!(shape("console Error: TypeError: nope").map(|(k, m, _)| (k, m)), Some((Kind::Error, "TypeError: nope".into())));
        // One of the two lines the 2026-11-13 run of the ten sites produced, verbatim.
        assert_eq!(
            shape("console Error: Error at :0:0 Error: Unsupported structured clone data version."),
            Some((Kind::Error, "Error: Unsupported structured clone data version.".into(), String::new()))
        );
        assert_eq!(shape("console Warn: nothing observed").map(|(k, m, _)| (k, m)), Some((Kind::Warn, "nothing observed".into())));
        assert_eq!(shape("lucinanet-js: could not write the dump").map(|(k, _, _)| k), Some(Kind::Js));
        // The abort line of a stub, with and without the glue's own prefix.
        assert_eq!(shape("mozjs_sys (Rust): JS_GetProperty is not implemented"), Some((Kind::Missing, "JS_GetProperty is not implemented".into(), String::new())));
        assert_eq!(shape("JS_GetProperty is not implemented").map(|(_, m, _)| m), Some("JS_GetProperty is not implemented".into()));
        // Phases, timings and network lines are not missing features.
        assert_eq!(shape("netphase 51722.525228 fetch https://example-site.com/ 0.0 ms"), None);
        assert_eq!(shape("render: complete true in 13057 ms, rss 274140 KiB"), None);
        assert_eq!(shape("rc=0"), None);
    }

    #[test]
    fn a_glue_log_line_and_a_stderr_abort_line_are_the_same_row() {
        let log = shape("1794528000123\tmissing\tJS_GetProperty").expect("a log line");
        let stderr = shape("mozjs_sys (Rust): JS_GetProperty is not implemented").expect("an abort line");
        assert_eq!(log.0, stderr.0);
        assert_eq!(normalize(&log.1), normalize(&stderr.1));
        assert_eq!(
            shape("1794528000123\terror\tTypeError: nope\tapp.js:7:1"),
            Some((Kind::Error, "TypeError: nope".into(), "app.js:7:1".into()))
        );
        assert_eq!(shape("1794528000123\tmissing\t"), None, "a nameless stub is no evidence");
        assert_eq!(shape("1794528000123\tnonsense\tx"), None);
        assert_eq!(shape("1794528000123\twarn\tslow\ta.js:1"), Some((Kind::Warn, "slow".into(), "a.js:1".into())));
    }

    #[test]
    fn the_location_of_an_error_token_drops_the_column() {
        assert_eq!(location_of("https://x.dev/a.js:2:184560"), "https://x.dev/a.js:2");
        assert_eq!(location_of("app.js:10"), "app.js:10", "already a file and a line");
        assert_eq!(location_of("inline script"), "inline script", "no line numbers to take");
        assert_eq!(location_of(":0:0"), "", "an error the page did not originate has no place to point at");
    }

    #[test]
    fn normalization_replaces_what_differs_between_two_hits_of_one_cause() {
        assert_eq!(normalize("ReferenceError:  `Intl`   is not defined"), "ReferenceError: Intl is not defined");
        assert_eq!(normalize("failed to fetch https://cdn.example.com/v2/x.js?t=1794528000"), "failed to fetch U");
        assert_eq!(normalize("no such asset code-view-a0e205571facc60b.js"), "no such asset code-view-H.js");
        assert_eq!(normalize("at offset 42 of 8"), "at offset N of N");
        assert_eq!(normalize("\"a long value here\" is not a function"), "X is not a function");
        assert_eq!(normalize("«délai» expiré"), "X expiré");
        assert_eq!(normalize("Int8Array and v2 and sha1 stay"), "Int8Array and v2 and sha1 stay");
        assert_eq!(normalize("undefined\tis not\ta function"), "undefined is not a function", "tabs cannot survive a TSV");
    }

    #[test]
    fn rows_group_by_normalised_message_and_rank_by_sites_then_hits() {
        let intl = |site: &str, at: &str| (site.to_string(), event(Kind::Error, "ReferenceError: `Intl` is not defined", at));
        let keyed = vec![
            intl("b", ""),
            intl("a", "a.js:1"),
            ("a".to_string(), event(Kind::Missing, &miss_text("JS_Foo"), "")),
            intl("c", ""),
        ];
        let rows = aggregate(&keyed);
        assert_eq!(rows.len(), 2, "the hits of one normalised message are one row");
        assert_eq!((rows[0].message.as_str(), rows[0].sites, rows[0].hits), ("ReferenceError: Intl is not defined", 3, 3));
        // The example prefers the first hit that names a location, so the last column is rarely empty.
        assert_eq!((rows[0].example_site.as_str(), rows[0].example_location.as_str()), ("a", "a.js:1"));
        assert_eq!((rows[1].message.as_str(), rows[1].sites, rows[1].hits), ("JS_Foo is not implemented", 1, 1));
        assert_eq!(sites_per_kind(&keyed), BTreeMap::from([(Kind::Error, 3), (Kind::Missing, 1)]));
    }

    #[test]
    fn the_markdown_stays_short_and_names_the_split() {
        let rows: Vec<Row> = (0..40)
            .map(|i| Row {
                kind: if i % 2 == 0 { Kind::Error } else { Kind::Missing },
                message: format!("TypeError: Name{i} is not defined"),
                sites: 40 - i,
                hits: 1,
                example_site: "site".into(),
                example_location: "a.js:1".into(),
            })
            .collect();
        let kind_sites = BTreeMap::from([(Kind::Error, 1), (Kind::Missing, 1)]);
        let summary = Summary {
            rows: &rows,
            kind_sites: &kind_sites,
            files: 4,
            sites: 5,
            silent: 1,
            events: 80,
            stamp: Some("2026-11-13".into()),
            build: "the probe binary of 2026-11-13",
        };
        let md = render_md(&summary);
        assert!(md.lines().count() < 80, "{} lines of summary", md.lines().count());
        assert!(md.contains("top 20"), "the by-sites table is capped");
        assert!(md.contains("\nBuild: the probe binary of 2026-11-13\n"), "its own line, whatever its length");
        assert!(md.contains("and 25 more rows"), "the overflow is stated, not hidden");
        assert!(md.contains("\nweb-engine\tName0\t"), "a name a page reached for is the web engine's");
        assert!(md.contains("\nengine\tName1\t"), "a not-implemented line is the glue's");
        assert_eq!(md.matches('\n').count(), md.lines().count(), "no stray blank at the end");
        let tsv = render_tsv(&summary);
        assert_eq!(tsv.lines().filter(|l| l.starts_with('#')).count(), 9, "the header explains the table");
        assert!(tsv.contains("# Build: the probe binary of 2026-11-13."));
        assert_eq!(tsv.lines().filter(|l| !l.starts_with('#')).count(), 41, "one header line and one row each");
    }
}
