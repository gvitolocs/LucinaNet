//! Parsers and renderers for `lucinanet --render` stderr logs, plus the
//! screenshot comparison behind `sitecheck diff`.
//!
//! The library owns all the logic so the tests can exercise parsing,
//! verdicts, both output formats and the pixel diff without going through
//! the CLI.

pub mod diff;

use serde::{Deserialize, Serialize};

/// Default budget for the page load, in milliseconds.
pub const DEFAULT_LOAD_MS: u64 = 15000;
/// Default budget for a scroll step (median of the down steps), in ms.
pub const DEFAULT_SCROLL_MS: u64 = 50;
/// Only the first few console errors are kept in the report.
const MAX_CONSOLE_ERRORS: usize = 3;
/// Each kept console error is cut to this many characters.
const CONSOLE_ERROR_LEN: usize = 120;

/// How a page died, as a crash record names it. The words the engine printed are never part of a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashReason {
    Panic,
    OutOfMemory,
    Crash,
}

impl CrashReason {
    pub fn name(self) -> &'static str {
        match self {
            CrashReason::Panic => "Panic",
            CrashReason::OutOfMemory => "OutOfMemory",
            CrashReason::Crash => "Crash",
        }
    }

    /// Which death a line reports. A failed allocation is running out of memory whatever word it panicked with,
    /// so it is looked for first; anything that is neither that nor a panic is a crash.
    pub fn of(line: &str) -> CrashReason {
        let low = line.to_lowercase();
        if low.contains("out of memory")
            || low.contains("outofmemory")
            || low.contains("memory allocation of")
        {
            CrashReason::OutOfMemory
        } else if line.contains("panicked at") {
            CrashReason::Panic
        } else {
            CrashReason::Crash
        }
    }
}

/// The thread a panic line names, if it names one. Rust quotes it (`thread 'Script#3' (1) panicked at
/// csp.rs:225:22:`) and a harness line sometimes leaves the quotes out; both give `Script#3`. The thread id in
/// parentheses is not the name, so it is dropped with everything else the line adds.
pub fn crash_thread(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("thread ")?;
    let name = match rest.split_once('\'') {
        Some((_, quoted)) => quoted.split('\'').next(),
        None => rest.split_whitespace().next().filter(|w| *w != "panicked"),
    }?;
    (!name.is_empty()).then(|| name.to_string())
}

/// `file:line[:col]`, cut where the location ends, so a message glued to the same line cannot come with it. A URL
/// is not a location, and neither is a `pc 0x12345678`.
fn location_of(token: &str) -> Option<String> {
    let token = token.trim_end_matches(':');
    if token.is_empty() || token.contains("://") {
        return None;
    }
    let parts: Vec<&str> = token.split(':').collect();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    let head = |upto: usize| parts[..upto].join(":");
    let n = parts.len();
    if n >= 3 && digits(parts[n - 1]) && digits(parts[n - 2]) && !head(n - 2).is_empty() {
        Some(format!("{}:{}:{}", head(n - 2), parts[n - 2], parts[n - 1]))
    } else if n >= 2 && digits(parts[n - 1]) && !head(n - 1).is_empty() {
        Some(format!("{}:{}", head(n - 1), parts[n - 1]))
    } else {
        None
    }
}

/// The location a panic line names, if it names one: the first word after an `at` that is a location.
pub fn crash_location(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    while let Some(w) = words.next() {
        if w == "at" {
            if let Some(loc) = words.next().and_then(location_of) {
                return Some(loc);
            }
        }
    }
    None
}

/// The shim a crash line names, in the one form that is machine written: `shim=<name>`. Nothing else in the line is
/// taken for a name, so a word of the message cannot come out as one.
pub fn crash_shim(line: &str) -> Option<String> {
    let (_, rest) = line.split_once("shim=")?;
    let name = rest
        .trim_start()
        .split_whitespace()
        .next()?
        .trim_end_matches([',', ';']);
    (!name.is_empty()).then(|| name.to_string())
}

/// One panic or crash line, REDACTED: `thread=<name> location=<file:line[:col]> shim=<name> reason=<Panic>`, only
/// the fields the line carries. The message stays in the session log and on the crash page; the table footnote and
/// the JSON carry this.
pub fn crash_record(line: &str) -> String {
    let mut out = String::new();
    if let Some(t) = crash_thread(line) {
        out.push_str(&format!("thread={t} "));
    }
    if let Some(l) = crash_location(line) {
        out.push_str(&format!("location={l} "));
    }
    if let Some(s) = crash_shim(line) {
        out.push_str(&format!("shim={s} "));
    }
    out.push_str(&format!("reason={}", CrashReason::of(line).name()));
    out
}

/// The record of a run an unimplemented binding aborted: the stub that tripped it, named as a shim, and nothing of
/// the abort message.
fn shim_record(shim: &str) -> String {
    let reason = format!("reason={}", CrashReason::Crash.name());
    if shim.is_empty() {
        return reason;
    }
    format!("shim={shim} {reason}")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Site {
    pub url: String,
    pub loaded: bool,
    pub load_ms: Option<u64>,
    pub page_load_ms: Option<u64>,
    /// Down scroll step times in whole milliseconds.
    pub scroll_steps: Vec<u64>,
    /// First three console error texts, each cut to 120 chars.
    pub console_errors: Vec<String>,
    pub crashed: bool,
    /// The crash in the REDACTED form: `thread=`, `location=`, `shim=` and `reason=`, never the engine's own words.
    pub crash_reason: Option<String>,
}

impl Default for Site {
    fn default() -> Self {
        Site {
            url: String::new(),
            loaded: false,
            load_ms: None,
            page_load_ms: None,
            scroll_steps: Vec::new(),
            console_errors: Vec::new(),
            crashed: false,
            crash_reason: None,
        }
    }
}

/// Fold one log line into the site's state. Lines that do not match the
/// known formats are ignored, since `lucinanet --render` stderr is noisy.
pub fn parse_line(site: &mut Site, line: &str) {
    if let Some(url) = line.strip_prefix("# url ") {
        // Harness header written as the first line of the log file.
        site.url = url.trim().to_string();
        return;
    }
    if line.starts_with("render: complete ") {
        // `render: complete true in 5310 ms, rss 170816 KiB`
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 5 {
            site.loaded = parts[2] == "true";
            site.load_ms = parts[4].parse().ok();
        }
    } else if line.starts_with("lucinanet: load Complete in ") {
        // `lucinanet: load Complete in 1532 ms, rss 170484 KiB` (may be absent)
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 5 {
            site.page_load_ms = parts[4].parse().ok();
        }
    } else if line.starts_with("render: down to frame ") {
        // `render: down to frame 82123 us`; up steps are not part of the verdict.
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 5 {
            if let Ok(us) = parts[4].parse::<u64>() {
                site.scroll_steps.push(us / 1000);
            }
        }
    } else if let Some(text) = line.strip_prefix("console Error: ") {
        if site.console_errors.len() < MAX_CONSOLE_ERRORS {
            site.console_errors.push(text.chars().take(CONSOLE_ERROR_LEN).collect());
        }
    } else if line.starts_with("mozjs_sys (Rust): ") {
        // Unimplemented JS bindings abort the run; name the stub that tripped it.
        let name = line["mozjs_sys (Rust): ".len()..]
            .split_whitespace()
            .next()
            .unwrap_or("");
        site.crashed = true;
        // The first crash line is the one to report; later noise (e.g. a
        // trailing `Aborted`) must not replace a specific reason.
        if site.crash_reason.is_none() {
            site.crash_reason = Some(shim_record(name));
        }
    } else if line.starts_with("lucinanet: signal 11")
        || line.contains("panicked at")
        || line.contains("Aborted")
    {
        site.crashed = true;
        if site.crash_reason.is_none() {
            site.crash_reason = Some(crash_record(line));
        }
    } else if line.starts_with("phase navigation Ok(String(\"[[\"") && site.url.is_empty() {
        // Fallback url source: the first quoted string right after `[["`.
        let rest = &line["phase navigation Ok(String(\"[[\"".len()..];
        let rest = rest.strip_prefix('"').unwrap_or(rest);
        let end = rest.find('"').unwrap_or(rest.len());
        site.url = rest[..end].to_string();
    }
}

/// Median of the down scroll steps in ms; with an even count it is the
/// integer mean of the two middle steps. An empty list yields 0.
pub fn scroll_median(steps: &[u64]) -> u64 {
    if steps.is_empty() {
        return 0;
    }
    let mut sorted = steps.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[mid - 1] + sorted[mid]) / 2
    } else {
        sorted[mid]
    }
}

/// Fastest-or-slowest check in one place so the table and tests share it.
pub fn verdict(site: &Site, load_limit_ms: u64, scroll_limit_ms: u64) -> &'static str {
    if site.crashed {
        "CRASH"
    } else if !site.loaded || site.load_ms.unwrap_or(0) > load_limit_ms {
        "FAIL load"
    } else if site.scroll_steps.is_empty() || scroll_median(&site.scroll_steps) > scroll_limit_ms {
        "FAIL scroll"
    } else {
        "PASS"
    }
}

/// Markdown table with one row per site; crashed sites get a numbered
/// footnote list under the table carrying the crash reason.
pub fn print_table(sites: &[Site], load_limit_ms: u64, scroll_limit_ms: u64) -> String {
    let mut out = String::from(
        "| site | loaded | load ms | page load ms | scroll median ms | scroll worst ms | console errors | verdict |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|---|\n");
    let mut footnotes = 0;
    for site in sites {
        let worst = site.scroll_steps.iter().copied().max().unwrap_or(0);
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            site.url,
            site.loaded,
            site.load_ms.unwrap_or(0),
            site.page_load_ms.unwrap_or(0),
            scroll_median(&site.scroll_steps),
            worst,
            site.console_errors.len(),
            verdict(site, load_limit_ms, scroll_limit_ms),
        ));
        if site.crashed {
            footnotes += 1;
            out.push_str(&format!(
                "{}. {}: {}\n",
                footnotes,
                site.url,
                site.crash_reason.as_deref().unwrap_or("crashed")
            ));
        }
    }
    out
}

/// One compact JSON object per site, in the same order as the input.
pub fn print_json(sites: &[Site]) -> String {
    sites
        .iter()
        .map(|site| {
            serde_json::to_string(site)
                .expect("a Site is always serializable")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
