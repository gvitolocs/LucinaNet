//! sweep-score: judge one run of the site sweep and diff two runs.
//!
//! `tests/sweep/sweep.sh` leaves `<out-dir>/<id>/{log,png,rc,dump,wall_ms,meta}` for every page. This crate reads
//! those and writes `<out-dir>/results.tsv`, one row per page:
//!
//! `id verdict reason rc png_bytes content_ratio errlines panics wall_ms first_paint_ms category`
//!
//! The first ten columns are the sweep's contract; `category` (from `tests/sweep/sites.tsv`) is appended so the
//! diff can count per category.
//!
//! # The rules, in this order (the first that applies decides; unit-tested in `tests` below)
//!
//! 1. `broken`
//!    * the run timed out (`rc` 124, 137 or 143) or the log says `render: complete false`;
//!    * `rc != 0` (or no `rc` file at all);
//!    * a line with `panicked at` or one that starts with `CRASHED:` (counted in `panics`); the reason then names
//!      the thread, the location, the shim and the kind of death, and never the message the engine printed;
//!    * the engine's own error page: an `error` marker of `walls.tsv` in the page title, or in a small visible text;
//!    * no PNG, an unreadable PNG, or a PNG whose `content_ratio` is below `blank_content_ratio_pct` (0.5 percent).
//!      `content_ratio` is the percentage of pixels that differ from the most frequent colour of the frame.
//! 2. `walled`, and only through one of these two evidences, never through a guess:
//!    * the document request's HTTP status is 403, 429 or 503. DORMANT TODAY: the render log carries no status
//!      (`netphase ... response headers <url> <ms>` names the URL only). The rule reads a status from a line that
//!      carries `status=NNN` (also `status NNN`, `HTTP/x NNN`, `HTTP NNN`) and either is the proposed document line
//!      `netphase <t> document <origin> status=<n> final=<origin>` or names the document URL of the page. Until the
//!      net layer logs one, `doc_status` finds nothing and this rule never fires;
//!    * a wall marker (kinds `consent`, `bot`, `login` of `tests/sweep/walls.tsv`) is found in the visible text of
//!      the dump (the log text when there is no dump) AND the page has a small visible-text amount: at most
//!      `wall_max_text_chars` characters of dump text, or without a dump a log of at most `wall_max_log_bytes`.
//! 3. `loaded`: everything else. `errlines` (console errors and `ERROR` log lines) is counted apart; a loaded page
//!    may have many. With more than `errlines_top3_threshold` of them the reason lists the top 3 error texts.
//!
//! `first_paint_ms`: from the document's first `netphase ... fetch` line to the first `phase ... first frame
//! painted` line after the document's `response headers` line; empty when the log has no such pair.
//! `wall_ms` is the runner's wall clock for the page (one-second resolution).
//!
//! # PNG decode
//! `png` 0.18.1, which is in the repo's `Cargo.lock` (so it resolves offline); nothing else beyond `std`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::BufReader;
use std::path::Path;

// ---------------------------------------------------------------------------------------------------------------
// walls.tsv

/// What kind of marker a `walls.tsv` line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Consent,
    Bot,
    Login,
    Error,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Consent => "consent",
            Kind::Bot => "bot",
            Kind::Login => "login",
            Kind::Error => "error",
        }
    }
}

/// The parameters and markers of `tests/sweep/walls.tsv`.
#[derive(Debug, Clone)]
pub struct Walls {
    pub wall_max_text_chars: usize,
    pub wall_max_log_bytes: usize,
    pub blank_content_ratio_pct: f64,
    pub errlines_top3_threshold: usize,
    /// Lowercased marker text.
    pub markers: Vec<(Kind, String)>,
}

impl Default for Walls {
    fn default() -> Self {
        Walls {
            wall_max_text_chars: 1500,
            wall_max_log_bytes: 20000,
            blank_content_ratio_pct: 0.5,
            errlines_top3_threshold: 20,
            markers: Vec::new(),
        }
    }
}

impl Walls {
    /// Parse the file: `kind TAB value` lines; `#` comments and blank lines are skipped.
    pub fn parse(src: &str) -> Result<Walls, String> {
        let mut w = Walls::default();
        for (n, raw) in src.lines().enumerate() {
            let line = raw.trim_end_matches('\r');
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let at = n + 1;
            let (kind, val) = line.split_once('\t').ok_or_else(|| format!("walls.tsv line {at}: no tab"))?;
            let val = val.trim();
            match kind {
                "param" => {
                    let (k, v) = val.split_once('=').ok_or_else(|| format!("walls.tsv line {at}: param wants name=value"))?;
                    let bad = || format!("walls.tsv line {at}: bad number {v:?}");
                    match k.trim() {
                        "wall_max_text_chars" => w.wall_max_text_chars = v.trim().parse().map_err(|_| bad())?,
                        "wall_max_log_bytes" => w.wall_max_log_bytes = v.trim().parse().map_err(|_| bad())?,
                        "blank_content_ratio_pct" => w.blank_content_ratio_pct = v.trim().parse().map_err(|_| bad())?,
                        "errlines_top3_threshold" => w.errlines_top3_threshold = v.trim().parse().map_err(|_| bad())?,
                        other => return Err(format!("walls.tsv line {at}: unknown param {other:?}")),
                    }
                },
                "consent" | "bot" | "login" | "error" => {
                    if val.is_empty() {
                        return Err(format!("walls.tsv line {at}: empty marker"));
                    }
                    let k = match kind {
                        "consent" => Kind::Consent,
                        "bot" => Kind::Bot,
                        "login" => Kind::Login,
                        _ => Kind::Error,
                    };
                    w.markers.push((k, val.to_lowercase()));
                },
                other => return Err(format!("walls.tsv line {at}: unknown kind {other:?}")),
            }
        }
        Ok(w)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// the facts of one page

/// Everything the rules look at, read from a page directory.
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub id: String,
    pub category: String,
    pub url: String,
    /// `None` when there is no `rc` file.
    pub rc: Option<i32>,
    pub log: String,
    /// Title and visible text of the dump, when there is one.
    pub dump: Option<(String, String)>,
    pub png_bytes: u64,
    /// Percent; `None` when there is no PNG or it cannot be read.
    pub content_ratio: Option<f64>,
    pub png_error: Option<String>,
    pub wall_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Loaded,
    Broken,
    Walled,
}

impl Verdict {
    pub fn name(self) -> &'static str {
        match self {
            Verdict::Loaded => "loaded",
            Verdict::Broken => "broken",
            Verdict::Walled => "walled",
        }
    }
}

/// One row of results.tsv.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: String,
    pub verdict: String,
    pub reason: String,
    pub rc: String,
    pub png_bytes: u64,
    pub content_ratio: String,
    pub errlines: u64,
    pub panics: u64,
    pub wall_ms: String,
    pub first_paint_ms: String,
    pub category: String,
}

// ---------------------------------------------------------------------------------------------------------------
// log readers

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

/// The thread a panic or crash line names, if it names one. Rust quotes it (`thread 'Script#3' (1) panicked at
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

/// `file:line[:col]`, cut where the location ends: a message glued to the same line (a harness that joins the
/// panic's two lines with `tr '\n' ' '`) cannot ride along, and neither can a `pc 0x...` nor a URL.
fn location_of(token: &str) -> Option<String> {
    let token = token.trim_end_matches(':');
    if token.is_empty() || token.contains("://") {
        return None;
    }
    let parts: Vec<&str> = token.split(':').collect();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let n = parts.len();
    let head = |upto: usize| parts[..upto].join(":");
    if n >= 3 && digits(parts[n - 1]) && digits(parts[n - 2]) && !head(n - 2).is_empty() {
        Some(format!("{}:{}:{}", head(n - 2), parts[n - 2], parts[n - 1]))
    } else if n >= 2 && digits(parts[n - 1]) && !head(n - 1).is_empty() {
        Some(format!("{}:{}", head(n - 1), parts[n - 1]))
    } else {
        None
    }
}

/// The location a panic or crash line names, if it names one: the first word after an `at` that is a location.
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
/// the fields the line carries. The message itself belongs to the session log and to the crash page and to nothing
/// else, so it never reaches `results.tsv`, a diff report or this tool's stdout.
pub fn crash_record(line: &str) -> String {
    let mut out = String::new();
    if let Some(t) = crash_thread(line) {
        let _ = write!(out, "thread={t} ");
    }
    if let Some(l) = crash_location(line) {
        let _ = write!(out, "location={l} ");
    }
    if let Some(s) = crash_shim(line) {
        let _ = write!(out, "shim={s} ");
    }
    let _ = write!(out, "reason={}", CrashReason::of(line).name());
    out
}

/// REDACTED records of the lines that are a panic or a crash report. There is one per such line, which is what
/// `panics` counts; the text of those lines is not in any of them.
pub fn panic_lines(log: &str) -> Vec<String> {
    log.lines()
        .filter(|l| l.contains("panicked at") || l.trim_start().starts_with("CRASHED:"))
        .map(crash_record)
        .collect()
}

/// The text of every error line: console errors and `ERROR` log records.
pub fn error_texts(log: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in log.lines() {
        if let Some(rest) = line.strip_prefix("console Error") {
            out.push(rest.trim_start_matches(':').trim().to_string());
        } else if line.starts_with('[') {
            // `[2026-11-13T20:10:44Z ERROR script::x] message`
            if let Some(end) = line.find(']') {
                if line[..end].contains(" ERROR ") {
                    out.push(line[end + 1..].trim().to_string());
                }
            }
        }
    }
    out
}

/// `text (xN)` for the `n` most frequent error texts, each cut to 80 characters.
pub fn top_errors(texts: &[String], n: usize) -> Vec<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for t in texts {
        *counts.entry(t.as_str()).or_insert(0) += 1;
    }
    let mut v: Vec<(&str, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    v.into_iter()
        .take(n)
        .map(|(t, c)| {
            let cut: String = t.chars().take(80).collect();
            format!("{cut} (x{c})")
        })
        .collect()
}

fn three_digits_at(s: &str, from: usize) -> Option<u16> {
    let b = s.as_bytes();
    let mut i = from;
    while i < b.len() && (b[i] == b' ' || b[i] == b'=' || b[i] == b':') {
        i += 1;
    }
    if i + 3 <= b.len() && b[i..i + 3].iter().all(|c| c.is_ascii_digit()) && (i + 3 == b.len() || !b[i + 3].is_ascii_digit()) {
        s[i..i + 3].parse().ok()
    } else {
        None
    }
}

/// A status in one line: `status=NNN`, `status NNN`, `status: NNN`, `HTTP/x NNN` or `HTTP NNN`.
pub fn status_in(line: &str) -> Option<u16> {
    let low = line.to_lowercase();
    if let Some(i) = low.find("status") {
        if let Some(s) = three_digits_at(&low, i + "status".len()) {
            return Some(s);
        }
    }
    if let Some(i) = low.find("http/") {
        // "http/1.1 403" or "http/2 403": the version token ends at the next space.
        if let Some(sp) = low[i..].find(' ') {
            if let Some(s) = three_digits_at(&low, i + sp) {
                return Some(s);
            }
        }
    }
    if let Some(i) = low.find("http ") {
        if let Some(s) = three_digits_at(&low, i + "http".len()) {
            return Some(s);
        }
    }
    None
}

/// The status of the document request, if the log carries one. The proposed line
/// `netphase <t> document <origin> status=<n> final=<origin>` wins; otherwise a line that names the page's own URL.
pub fn doc_status(log: &str, url: &str) -> Option<u16> {
    let mut found = None;
    for line in log.lines() {
        if line.starts_with("netphase") && line.contains(" document ") {
            if let Some(s) = status_in(line) {
                found = Some(s);
            }
        }
    }
    if found.is_some() {
        return found;
    }
    let bare = url.trim_end_matches('/');
    if bare.is_empty() {
        return None;
    }
    for line in log.lines() {
        if line.contains(bare) {
            if let Some(s) = status_in(line) {
                found = Some(s);
            }
        }
    }
    found
}

fn phase_time(line: &str) -> Option<f64> {
    let mut it = line.split_whitespace();
    let tag = it.next()?;
    if tag != "phase" && tag != "netphase" {
        return None;
    }
    it.next()?.parse().ok()
}

/// Milliseconds from the document's first `fetch` to the first frame painted after its response headers.
pub fn first_paint_ms(log: &str) -> Option<u64> {
    let mut fetch = None;
    let mut headers = None;
    for line in log.lines() {
        if fetch.is_none() && line.starts_with("netphase") && line.contains(" fetch ") {
            fetch = phase_time(line);
        } else if headers.is_none() && line.starts_with("netphase") && line.contains(" response headers ") {
            headers = phase_time(line);
        } else if line.starts_with("phase") && line.contains("first frame painted") {
            if let (Some(f), Some(h), Some(t)) = (fetch, headers, phase_time(line)) {
                if t >= h {
                    return Some(((t - f) * 1000.0).round().max(0.0) as u64);
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------------------------
// dump.json: title and text of the top-level object, without a JSON crate

fn parse_json_string(c: &[char], i: &mut usize) -> Option<String> {
    if c.get(*i) != Some(&'"') {
        return None;
    }
    *i += 1;
    let mut out = String::new();
    while *i < c.len() {
        let ch = c[*i];
        *i += 1;
        match ch {
            '"' => return Some(out),
            '\\' => {
                let e = *c.get(*i)?;
                *i += 1;
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'u' => {
                        let hex: String = c.get(*i..*i + 4)?.iter().collect();
                        *i += 4;
                        let mut cp = u32::from_str_radix(&hex, 16).ok()?;
                        if (0xD800..0xDC00).contains(&cp) && c.get(*i) == Some(&'\\') && c.get(*i + 1) == Some(&'u') {
                            let hex2: String = c.get(*i + 2..*i + 6)?.iter().collect();
                            if let Ok(lo) = u32::from_str_radix(&hex2, 16) {
                                if (0xDC00..0xE000).contains(&lo) {
                                    cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    *i += 6;
                                }
                            }
                        }
                        out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                    },
                    other => out.push(other),
                }
            },
            other => out.push(other),
        }
    }
    None
}

fn skip_ws(c: &[char], i: &mut usize) {
    while *i < c.len() && c[*i].is_whitespace() {
        *i += 1;
    }
}

fn skip_value(c: &[char], i: &mut usize) -> Option<()> {
    let mut depth = 0usize;
    while *i < c.len() {
        match c[*i] {
            '"' => {
                parse_json_string(c, i)?;
                if depth == 0 {
                    return Some(());
                }
                continue;
            },
            '{' | '[' => depth += 1,
            '}' | ']' => {
                if depth == 0 {
                    return Some(());
                }
                depth -= 1;
                if depth == 0 {
                    *i += 1;
                    return Some(());
                }
            },
            ',' if depth == 0 => return Some(()),
            _ => {},
        }
        *i += 1;
    }
    Some(())
}

/// `(title, text)` of the dump's top-level object; `None` when it is not that.
pub fn dump_fields(src: &str) -> Option<(String, String)> {
    let c: Vec<char> = src.chars().collect();
    let mut i = 0;
    skip_ws(&c, &mut i);
    if c.get(i) != Some(&'{') {
        return None;
    }
    i += 1;
    let (mut title, mut text) = (String::new(), String::new());
    loop {
        skip_ws(&c, &mut i);
        match c.get(i) {
            Some('}') | None => break,
            Some(',') => {
                i += 1;
                continue;
            },
            _ => {},
        }
        let key = parse_json_string(&c, &mut i)?;
        skip_ws(&c, &mut i);
        if c.get(i) != Some(&':') {
            return None;
        }
        i += 1;
        skip_ws(&c, &mut i);
        if (key == "title" || key == "text") && c.get(i) == Some(&'"') {
            let v = parse_json_string(&c, &mut i)?;
            if key == "title" {
                title = v;
            } else {
                text = v;
            }
        } else {
            skip_value(&c, &mut i)?;
        }
    }
    Some((title, text))
}

// ---------------------------------------------------------------------------------------------------------------
// PNG

/// Percentage of pixels that differ from the most frequent pixel value. `channels` is samples per pixel (1..=4).
pub fn content_ratio_pct(pixels: &[u8], channels: usize) -> f64 {
    if channels == 0 || pixels.len() < channels {
        return 0.0;
    }
    let mut counts: BTreeMap<[u8; 4], u64> = BTreeMap::new();
    let mut total = 0u64;
    for px in pixels.chunks_exact(channels) {
        let mut key = [0u8; 4];
        for (k, v) in key.iter_mut().zip(px.iter()) {
            *k = *v;
        }
        *counts.entry(key).or_insert(0) += 1;
        total += 1;
    }
    let most = counts.values().copied().max().unwrap_or(0);
    if total == 0 {
        0.0
    } else {
        100.0 * (total - most) as f64 / total as f64
    }
}

/// Decode a PNG to 8-bit samples and return its `content_ratio_pct`.
pub fn png_content_ratio(path: &Path) -> Result<f64, String> {
    let file = fs::File::open(path).map_err(|e| format!("png: {e}"))?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| format!("png: {e}"))?;
    let size = reader.output_buffer_size().ok_or_else(|| "png: too large".to_string())?;
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("png: {e}"))?;
    let channels = info.color_type.samples();
    Ok(content_ratio_pct(&buf[..info.buffer_size()], channels))
}

// ---------------------------------------------------------------------------------------------------------------
// the rules

fn oneline(s: &str) -> String {
    s.chars().map(|c| if c == '\t' || c == '\n' || c == '\r' { ' ' } else { c }).collect()
}

fn first_marker<'a>(walls: &'a Walls, haystack: &str, want: &[Kind]) -> Option<&'a (Kind, String)> {
    let low = haystack.to_lowercase();
    walls.markers.iter().find(|(k, m)| want.contains(k) && low.contains(m.as_str()))
}

/// Judge one page. Pure: everything it needs is in `page` and `walls`.
pub fn score(page: &Page, walls: &Walls) -> Row {
    let panics = panic_lines(&page.log);
    let errs = error_texts(&page.log);
    let (verdict, reason) = decide(page, walls, &panics, &errs);
    Row {
        id: page.id.clone(),
        verdict: verdict.name().to_string(),
        reason: oneline(&reason),
        rc: page.rc.map(|r| r.to_string()).unwrap_or_else(|| "-".to_string()),
        png_bytes: page.png_bytes,
        content_ratio: page.content_ratio.map(|r| format!("{r:.3}")).unwrap_or_else(|| "-".to_string()),
        errlines: errs.len() as u64,
        panics: panics.len() as u64,
        wall_ms: page.wall_ms.map(|m| m.to_string()).unwrap_or_else(|| "-".to_string()),
        first_paint_ms: first_paint_ms(&page.log).map(|m| m.to_string()).unwrap_or_default(),
        category: page.category.clone(),
    }
}

fn decide(page: &Page, walls: &Walls, panics: &[String], errs: &[String]) -> (Verdict, String) {
    // 1. broken
    match page.rc {
        Some(124) | Some(137) | Some(143) => {
            return (Verdict::Broken, format!("timeout (rc {})", page.rc.unwrap_or(0)));
        },
        _ => {},
    }
    if page.log.lines().any(|l| l.trim() == "render: complete false" || l.starts_with("render: complete false ")) {
        return (Verdict::Broken, "timeout: the render did not complete".to_string());
    }
    match page.rc {
        Some(0) => {},
        Some(rc) => return (Verdict::Broken, format!("rc {rc}")),
        None => return (Verdict::Broken, "no rc file".to_string()),
    }
    if let Some(p) = panics.first() {
        return (Verdict::Broken, p.clone());
    }
    let text_len = page.dump.as_ref().map(|(_, t)| t.chars().count());
    if let Some((title, text)) = &page.dump {
        if let Some((_, m)) = first_marker(walls, title, &[Kind::Error]) {
            return (Verdict::Broken, format!("engine error page (title matches {m:?})"));
        }
        if text.chars().count() <= walls.wall_max_text_chars {
            if let Some((_, m)) = first_marker(walls, text, &[Kind::Error]) {
                return (Verdict::Broken, format!("engine error page (text matches {m:?})"));
            }
        }
    }
    match (page.content_ratio, &page.png_error) {
        (Some(r), _) if r < walls.blank_content_ratio_pct => {
            // Not blank-and-broken when the page is a wall: a wall page is mostly background too, but the spec
            // orders blank first, so a blank frame is broken whatever the text says.
            return (Verdict::Broken, format!("blank frame ({r:.3}% of pixels differ from the background)"));
        },
        (None, Some(e)) => return (Verdict::Broken, e.clone()),
        (None, None) => return (Verdict::Broken, "no png".to_string()),
        _ => {},
    }
    // 2. walled
    if let Some(s) = doc_status(&page.log, &page.url) {
        if matches!(s, 403 | 429 | 503) {
            return (Verdict::Walled, format!("HTTP {s} on the document request"));
        }
    }
    let wall_kinds = [Kind::Consent, Kind::Bot, Kind::Login];
    match &page.dump {
        Some((title, text)) => {
            if let Some(n) = text_len {
                if n <= walls.wall_max_text_chars {
                    let both = format!("{title}\n{text}");
                    if let Some((k, m)) = first_marker(walls, &both, &wall_kinds) {
                        return (Verdict::Walled, format!("{} wall marker {m:?} with {n} chars of visible text", k.name()));
                    }
                }
            }
        },
        None => {
            if page.log.len() <= walls.wall_max_log_bytes {
                if let Some((k, m)) = first_marker(walls, &page.log, &wall_kinds) {
                    return (
                        Verdict::Walled,
                        format!("{} wall marker {m:?} in a {} byte log (no dump)", k.name(), page.log.len()),
                    );
                }
            }
        },
    }
    // 3. loaded
    if errs.len() > walls.errlines_top3_threshold {
        let top = top_errors(errs, 3).join(" | ");
        return (Verdict::Loaded, format!("{} console errors, top: {top}", errs.len()));
    }
    let r = page.content_ratio.unwrap_or(0.0);
    (Verdict::Loaded, format!("rendered, {r:.1}% content"))
}

// ---------------------------------------------------------------------------------------------------------------
// reading a run

/// Read one page directory. Missing files are facts (`None`), not errors.
pub fn read_page(dir: &Path, id: &str, category: &str) -> Page {
    let mut page = Page { id: id.to_string(), category: category.to_string(), ..Page::default() };
    if let Ok(meta) = fs::read_to_string(dir.join("meta")) {
        for line in meta.lines() {
            if let Some(u) = line.strip_prefix("url=") {
                page.url = u.to_string();
            }
            if page.category.is_empty() {
                if let Some(c) = line.strip_prefix("category=") {
                    page.category = c.to_string();
                }
            }
        }
    }
    page.rc = fs::read_to_string(dir.join("rc")).ok().and_then(|s| s.trim().parse().ok());
    page.log = fs::read(dir.join("log")).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    page.dump = fs::read(dir.join("dump")).ok().and_then(|b| dump_fields(&String::from_utf8_lossy(&b)));
    page.wall_ms = fs::read_to_string(dir.join("wall_ms")).ok().and_then(|s| s.trim().parse().ok());
    match fs::metadata(dir.join("png")) {
        Ok(m) if m.len() > 0 => {
            page.png_bytes = m.len();
            match png_content_ratio(&dir.join("png")) {
                Ok(r) => page.content_ratio = Some(r),
                Err(e) => page.png_error = Some(e),
            }
        },
        _ => {},
    }
    page
}

/// `id -> category` from a sites.tsv (comment lines skipped).
pub fn read_list(src: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for line in src.lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut f = line.split('\t');
        if let (Some(id), Some(cat)) = (f.next(), f.next()) {
            if !id.is_empty() {
                m.insert(id.to_string(), cat.to_string());
            }
        }
    }
    m
}

/// Score every `S*` directory of a run, in id order.
pub fn score_run(out: &Path, walls: &Walls, categories: &BTreeMap<String, String>) -> Result<Vec<Row>, String> {
    let mut ids: Vec<String> = Vec::new();
    for entry in fs::read_dir(out).map_err(|e| format!("{}: {e}", out.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() && !name.starts_with('.') {
            ids.push(name);
        }
    }
    ids.sort();
    Ok(ids
        .iter()
        .map(|id| {
            let cat = categories.get(id).cloned().unwrap_or_default();
            score(&read_page(&out.join(id), id, &cat), walls)
        })
        .collect())
}

// ---------------------------------------------------------------------------------------------------------------
// results.tsv

pub const HEADER: &str =
    "id\tverdict\treason\trc\tpng_bytes\tcontent_ratio\terrlines\tpanics\twall_ms\tfirst_paint_ms\tcategory";

pub fn format_results(rows: &[Row]) -> String {
    let mut s = String::from(HEADER);
    s.push('\n');
    for r in rows {
        let _ = writeln!(
            s,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.id, r.verdict, r.reason, r.rc, r.png_bytes, r.content_ratio, r.errlines, r.panics, r.wall_ms, r.first_paint_ms, r.category
        );
    }
    s
}

pub fn parse_results(src: &str) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    for (n, line) in src.lines().enumerate() {
        if n == 0 && line.starts_with("id\t") {
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 10 {
            return Err(format!("results line {}: {} fields, want at least 10", n + 1, f.len()));
        }
        let num = |s: &str| s.parse::<u64>().map_err(|_| format!("results line {}: bad number {s:?}", n + 1));
        rows.push(Row {
            id: f[0].to_string(),
            verdict: f[1].to_string(),
            reason: f[2].to_string(),
            rc: f[3].to_string(),
            png_bytes: num(f[4])?,
            content_ratio: f[5].to_string(),
            errlines: num(f[6])?,
            panics: num(f[7])?,
            wall_ms: f[8].to_string(),
            first_paint_ms: f[9].to_string(),
            category: f.get(10).copied().unwrap_or("").to_string(),
        });
    }
    Ok(rows)
}

// ---------------------------------------------------------------------------------------------------------------
// diff

pub struct DiffReport {
    pub text: String,
    /// A page went loaded -> broken.
    pub regression: bool,
}

fn tally(rows: &[&Row]) -> (usize, usize, usize) {
    let c = |v: &str| rows.iter().filter(|r| r.verdict == v).count();
    (c("loaded"), c("broken"), c("walled"))
}

/// Compare two runs. Exit status of the tool is 1 when `regression` is set, 0 otherwise.
pub fn diff(old: &[Row], new: &[Row]) -> DiffReport {
    let old_by: BTreeMap<&str, &Row> = old.iter().map(|r| (r.id.as_str(), r)).collect();
    let new_by: BTreeMap<&str, &Row> = new.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut text = String::new();
    let mut regression = false;

    let _ = writeln!(text, "== verdict changes");
    let mut any = false;
    for (id, n) in &new_by {
        if let Some(o) = old_by.get(id) {
            if o.verdict != n.verdict {
                any = true;
                if o.verdict == "loaded" && n.verdict == "broken" {
                    regression = true;
                }
                let _ = writeln!(text, "{id}\t{} -> {}\t[{}] -> [{}]", o.verdict, n.verdict, o.reason, n.reason);
            }
        }
    }
    if !any {
        let _ = writeln!(text, "(none)");
    }

    let _ = writeln!(text, "== newly panicking");
    any = false;
    for (id, n) in &new_by {
        let before = old_by.get(id).map(|o| o.panics).unwrap_or(0);
        if before == 0 && n.panics > 0 {
            any = true;
            let _ = writeln!(text, "{id}\t{} panic line(s)\t[{}]", n.panics, n.reason);
        }
    }
    if !any {
        let _ = writeln!(text, "(none)");
    }

    let _ = writeln!(text, "== errlines grew by more than 50 percent, or png_bytes changed by more than 30 percent");
    any = false;
    for (id, n) in &new_by {
        let Some(o) = old_by.get(id) else { continue };
        let err_grew = if o.errlines > 0 { n.errlines * 2 > o.errlines * 3 } else { n.errlines >= 5 };
        let png_moved = o.png_bytes > 0 && n.png_bytes.abs_diff(o.png_bytes) * 100 > o.png_bytes * 30;
        if err_grew || png_moved {
            any = true;
            let _ = writeln!(text, "{id}\terrlines {} -> {}\tpng_bytes {} -> {}", o.errlines, n.errlines, o.png_bytes, n.png_bytes);
        }
    }
    if !any {
        let _ = writeln!(text, "(none)");
    }

    let only_old: Vec<&&str> = old_by.keys().filter(|k| !new_by.contains_key(*k)).collect();
    let only_new: Vec<&&str> = new_by.keys().filter(|k| !old_by.contains_key(*k)).collect();
    if !only_old.is_empty() || !only_new.is_empty() {
        let _ = writeln!(text, "== pages in one run only");
        for id in only_old {
            let _ = writeln!(text, "{id}\tonly in the old run");
        }
        for id in only_new {
            let _ = writeln!(text, "{id}\tonly in the new run");
        }
    }

    let _ = writeln!(text, "== per category (loaded/broken/walled, old -> new)");
    let mut cats: BTreeMap<&str, (Vec<&Row>, Vec<&Row>)> = BTreeMap::new();
    for r in old {
        cats.entry(r.category.as_str()).or_default().0.push(r);
    }
    for r in new {
        cats.entry(r.category.as_str()).or_default().1.push(r);
    }
    for (cat, (o, n)) in &cats {
        let (ol, ob, ow) = tally(o);
        let (nl, nb, nw) = tally(n);
        let name = if cat.is_empty() { "(none)" } else { *cat };
        let _ = writeln!(text, "{name}\t{ol}/{ob}/{ow} -> {nl}/{nb}/{nw}");
    }
    let all_o: Vec<&Row> = old.iter().collect();
    let all_n: Vec<&Row> = new.iter().collect();
    let (ol, ob, ow) = tally(&all_o);
    let (nl, nb, nw) = tally(&all_n);
    let _ = writeln!(text, "TOTAL\t{ol}/{ob}/{ow} -> {nl}/{nb}/{nw}");
    if regression {
        let _ = writeln!(text, "REGRESSION: at least one page went loaded -> broken");
    }
    DiffReport { text, regression }
}

/// The summary counts of one run, for the tool's own output.
pub fn summary(rows: &[Row]) -> String {
    let all: Vec<&Row> = rows.iter().collect();
    let (l, b, w) = tally(&all);
    let mut s = format!("{} pages: {l} loaded, {b} broken, {w} walled\n", rows.len());
    let mut cats: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for r in rows {
        cats.entry(r.category.as_str()).or_default().push(r);
    }
    for (cat, rs) in cats {
        let (l, b, w) = tally(&rs);
        let _ = writeln!(s, "  {cat}: {l} loaded, {b} broken, {w} walled");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walls() -> Walls {
        Walls::parse(
            "param\twall_max_text_chars=1500\nconsent\tbefore you continue\nconsent\taccept all\nbot\tverify you are human\n\
             bot\tcaptcha\nlogin\tsign in to continue\nerror\tcould not load the requested page\n",
        )
        .unwrap()
    }

    fn good_page() -> Page {
        Page {
            id: "S001".into(),
            category: "news".into(),
            url: "https://example.com/".into(),
            rc: Some(0),
            log: "phase 100.0 main\nnetphase 100.10 fetch https://example.com/ 0.0 ms\n\
                  netphase 100.30 response headers https://example.com/ 200.0 ms\n\
                  phase 100.50 first frame painted\nrender: complete true in 900 ms\n"
                .into(),
            dump: Some(("Example".into(), "plenty of normal page text ".repeat(100))),
            png_bytes: 50_000,
            content_ratio: Some(12.5),
            png_error: None,
            wall_ms: Some(4000),
            ..Page::default()
        }
    }

    #[test]
    fn loaded_is_the_default() {
        let r = score(&good_page(), &walls());
        assert_eq!(r.verdict, "loaded");
        assert_eq!(r.errlines, 0);
        assert_eq!(r.first_paint_ms, "400");
        assert_eq!(r.rc, "0");
    }

    #[test]
    fn broken_by_rc_timeout_panic_blank_and_error_page() {
        let mut p = good_page();
        p.rc = Some(124);
        assert!(score(&p, &walls()).reason.starts_with("timeout"));
        p.rc = Some(1);
        assert_eq!(score(&p, &walls()).reason, "rc 1");
        p.rc = None;
        assert_eq!(score(&p, &walls()).verdict, "broken");

        let mut p = good_page();
        p.log.push_str("thread 'Script#3' (1) panicked at csp.rs:225:22:\n");
        let r = score(&p, &walls());
        assert_eq!((r.verdict.as_str(), r.panics), ("broken", 1));

        let mut p = good_page();
        p.log.push_str("CRASHED: signal 11\n");
        assert_eq!(score(&p, &walls()).panics, 1);

        let mut p = good_page();
        p.content_ratio = Some(0.2);
        let r = score(&p, &walls());
        assert_eq!(r.verdict, "broken");
        assert!(r.reason.starts_with("blank frame"));

        let mut p = good_page();
        p.dump = Some(("Certificate error".into(), "x".into()));
        let mut w = walls();
        w.markers.push((Kind::Error, "certificate error".into()));
        assert!(score(&p, &w).reason.starts_with("engine error page"));

        let mut p = good_page();
        p.dump = Some(("t".into(), "Could not load the requested page: refused".into()));
        assert_eq!(score(&p, &walls()).verdict, "broken");

        let mut p = good_page();
        p.log.push_str("render: complete false in 120000 ms\n");
        assert!(score(&p, &walls()).reason.starts_with("timeout"));

        let mut p = good_page();
        p.content_ratio = None;
        p.png_error = Some("png: bad".into());
        assert_eq!(score(&p, &walls()).reason, "png: bad");
        p.png_error = None;
        assert_eq!(score(&p, &walls()).reason, "no png");
    }

    // The line shapes below are copied from real logs: the engine's panic line and its message from
    // `tests/web/csp-origin/README.md`, a runner's `CRASHED:` line, and a harness that joined the message onto the
    // panic line with `tr '\n' ' '`. What a record may carry is the thread, the location, the shim and the reason.
    #[test]
    fn crash_records_name_the_facts_and_drop_the_message() {
        assert_eq!(
            crash_record("thread 'Script#3' (1) panicked at csp.rs:225:22:"),
            "thread=Script#3 location=csp.rs:225:22 reason=Panic"
        );
        assert_eq!(
            crash_record("thread Script#3 (1) panicked at csp.rs:225:22:"),
            "thread=Script#3 location=csp.rs:225:22 reason=Panic"
        );
        assert_eq!(crash_record("thread 'engine' (1053) panicked at /home/user/.cargo/registry/src/vello_cpu-0.3.0/src/fine/mod.rs:1085:44:"), "thread=engine location=/home/user/.cargo/registry/src/vello_cpu-0.3.0/src/fine/mod.rs:1085:44 reason=Panic");

        // The message is the line after the location; one panic line is still one record.
        let two_lines = "thread 'Script#3' (1) panicked at csp.rs:225:22:\nMust always be able to parse document origin: RelativeUrlWithoutBase\n";
        let recs = panic_lines(two_lines);
        assert_eq!(recs.len(), 1);
        assert_eq!(
            recs[0],
            "thread=Script#3 location=csp.rs:225:22 reason=Panic"
        );
        assert!(
            !recs[0].contains("Must always") && !recs[0].contains("RelativeUrlWithoutBase"),
            "{}",
            recs[0]
        );

        // A CRASHED: line: the signal and the addresses are the message, so all that is left is the reason.
        assert_eq!(
            crash_record("CRASHED: signal 11 at pc 0x12345678, lr 0x23456789"),
            "reason=Crash"
        );
        assert_eq!(
            crash_record("CRASHED: OutOfMemory at raster/core/src/pool.rs:304 shim=timeout-shim"),
            "location=raster/core/src/pool.rs:304 shim=timeout-shim reason=OutOfMemory"
        );
        assert_eq!(
            crash_record("CRASHED: shim=stat-shim"),
            "shim=stat-shim reason=Crash"
        );

        // A message with a URL and a colon in it, glued onto the line: the location ends at its own colons, and a
        // URL is not a location, so nothing of it is printed.
        let glued = "thread 'main' (42) panicked at csp.rs:225:22: refused https://user.example.com/a:b for that origin";
        let rec = crash_record(glued);
        assert_eq!(rec, "thread=main location=csp.rs:225:22 reason=Panic");
        assert!(
            !rec.contains("://") && !rec.contains("refused") && !rec.contains("user.example.com"),
            "{rec}"
        );

        assert_eq!(CrashReason::of("thread 'main' panicked at alloc.rs:1:1: memory allocation of 123456789 bytes failed"), CrashReason::OutOfMemory);
        assert_eq!(CrashReason::of("Aborted"), CrashReason::Crash);
    }

    #[test]
    fn no_panic_message_reaches_the_tsv_the_diff_or_the_summary() {
        let mut p = good_page();
        p.log.push_str("thread 'Script#3' (1) panicked at csp.rs:225:22:\nMust always be able to parse document origin: RelativeUrlWithoutBase https://opaque.example.com/x:y\n");
        let scored = score(&p, &walls());
        assert_eq!((scored.verdict.as_str(), scored.panics), ("broken", 1));
        assert_eq!(
            scored.reason,
            "thread=Script#3 location=csp.rs:225:22 reason=Panic"
        );

        let older = vec![row("S001", "loaded", 0, 1000, 0, "news")];
        let newer = vec![scored.clone()];
        for printed in [
            format_results(&newer),
            diff(&older, &newer).text,
            summary(&newer),
        ] {
            assert!(!printed.contains("Must always"), "{printed}");
            assert!(!printed.contains("RelativeUrlWithoutBase"), "{printed}");
            assert!(!printed.contains("://"), "{printed}");
        }
    }

    #[test]
    fn walled_by_marker_only_with_small_text() {
        let mut p = good_page();
        p.dump = Some(("Google".into(), "Before you continue to Google. Accept all / Reject all".into()));
        let r = score(&p, &walls());
        assert_eq!(r.verdict, "walled");
        assert!(r.reason.contains("before you continue"), "{}", r.reason);

        let mut p = good_page();
        p.dump = Some(("Check".into(), "Please verify you are human.".into()));
        assert_eq!(score(&p, &walls()).verdict, "walled");

        // A long article that mentions a captcha is a loaded page.
        let mut p = good_page();
        let long = format!("{} captcha {}", "word ".repeat(400), "word ".repeat(400));
        p.dump = Some(("Article".into(), long));
        assert_eq!(score(&p, &walls()).verdict, "loaded");

        // No marker, small text: not walled, never a guess.
        let mut p = good_page();
        p.dump = Some(("Tiny".into(), "hello".into()));
        assert_eq!(score(&p, &walls()).verdict, "loaded");
    }

    #[test]
    fn walled_without_dump_uses_the_log_size() {
        let mut p = good_page();
        p.dump = None;
        p.log.push_str("console Log: Performing security verification, this website verifies you are not a bot\n");
        let mut w = walls();
        w.markers.push((Kind::Bot, "verifies you are not a bot".into()));
        assert_eq!(score(&p, &w).verdict, "walled");
        p.log.push_str(&"x".repeat(30_000));
        assert_eq!(score(&p, &w).verdict, "loaded");
    }

    #[test]
    fn walled_by_status_when_the_log_carries_one() {
        let mut p = good_page();
        p.log.push_str("netphase 100.40 document https://example.com status=403 final=https://example.com\n");
        let r = score(&p, &walls());
        assert_eq!(r.verdict, "walled");
        assert_eq!(r.reason, "HTTP 403 on the document request");

        for s in [429, 503] {
            let mut p = good_page();
            p.log.push_str(&format!("netphase 100.40 document https://example.com status={s} final=https://example.com\n"));
            assert_eq!(score(&p, &walls()).verdict, "walled");
        }
        let mut p = good_page();
        p.log.push_str("netphase 100.40 document https://example.com status=200 final=https://example.com\n");
        assert_eq!(score(&p, &walls()).verdict, "loaded");

        // The URL-naming form.
        let mut p = good_page();
        p.log.push_str("net: https://example.com/ status=429\n");
        assert_eq!(score(&p, &walls()).verdict, "walled");
        // A status of another request is not the document's.
        let mut p = good_page();
        p.log.push_str("net: https://cdn.example.net/a.js status=403\n");
        assert_eq!(score(&p, &walls()).verdict, "loaded");
    }

    #[test]
    fn status_forms() {
        assert_eq!(status_in("status=403"), Some(403));
        assert_eq!(status_in("got status 429 here"), Some(429));
        assert_eq!(status_in("HTTP/1.1 503 Service Unavailable"), Some(503));
        assert_eq!(status_in("HTTP/2 403"), Some(403));
        assert_eq!(status_in("HTTP 403"), Some(403));
        assert_eq!(status_in("no code here 403"), None);
        assert_eq!(status_in("status=4030"), None);
        // The legacy log line has no status at all.
        assert_eq!(doc_status("netphase 1.0 response headers https://a.b/ 12.0 ms\n", "https://a.b/"), None);
    }

    #[test]
    fn blank_wins_over_wall_and_loaded_lists_top_three_errors() {
        let mut p = good_page();
        p.content_ratio = Some(0.1);
        p.dump = Some(("x".into(), "verify you are human".into()));
        assert_eq!(score(&p, &walls()).verdict, "broken");

        let mut p = good_page();
        for _ in 0..15 {
            p.log.push_str("console Error: Unhandled promise rejection: TypeError: A\n");
        }
        for _ in 0..7 {
            p.log.push_str("console Error: B went wrong\n");
        }
        p.log.push_str("[2026-11-17T10:00:00Z ERROR script::x] C failed\n");
        p.log.push_str("console Error: D\n");
        let r = score(&p, &walls());
        assert_eq!(r.verdict, "loaded");
        assert_eq!(r.errlines, 24);
        assert!(r.reason.contains("Unhandled promise rejection: TypeError: A (x15)"), "{}", r.reason);
        assert!(r.reason.contains("B went wrong (x7)"), "{}", r.reason);
        assert!(r.reason.contains("C failed (x1)"), "{}", r.reason);
        assert!(!r.reason.contains('\n') && !r.reason.contains('\t'));

        // At or under the threshold a loaded page keeps a plain reason.
        let mut p = good_page();
        p.log.push_str("console Error: one\n");
        assert!(score(&p, &walls()).reason.starts_with("rendered"));
    }

    #[test]
    fn content_ratio_of_synthetic_frames() {
        assert_eq!(content_ratio_pct(&vec![255u8; 100 * 3], 3), 0.0);
        let mut px = vec![255u8; 1000 * 3];
        for b in px.iter_mut().take(10 * 3) {
            *b = 0;
        }
        let r = content_ratio_pct(&px, 3);
        assert!((r - 1.0).abs() < 1e-9, "{r}");
        assert_eq!(content_ratio_pct(&[], 3), 0.0);
        // RGBA: alpha counts as a sample.
        let px = [1u8, 2, 3, 255, 1, 2, 3, 255, 9, 9, 9, 255, 1, 2, 3, 255];
        assert!((content_ratio_pct(&px, 4) - 25.0).abs() < 1e-9);
    }

    #[test]
    fn dump_fields_reads_title_and_text() {
        let src = r#"{"title":"Café \"x\"","text":"line1\nline2 😀","elements":3,"boxes":[{"p":"html","x":0,"y":0,"w":1,"h":1,"text":"no"}]}"#;
        let (t, x) = dump_fields(src).unwrap();
        assert_eq!(t, "Café \"x\"");
        assert_eq!(x, "line1\nline2 \u{1F600}");
        assert!(dump_fields("not json").is_none());
        assert_eq!(dump_fields(r#"{"elements":1}"#).unwrap(), (String::new(), String::new()));
    }

    #[test]
    fn walls_file_parses_and_rejects_nonsense() {
        let w = Walls::parse("# c\n\nparam\twall_max_text_chars=10\nbot\tCaptcha\n").unwrap();
        assert_eq!(w.wall_max_text_chars, 10);
        assert_eq!(w.markers, vec![(Kind::Bot, "captcha".to_string())]);
        assert!(Walls::parse("bot captcha\n").is_err());
        assert!(Walls::parse("weird\tx\n").is_err());
        assert!(Walls::parse("param\tnope=1\n").is_err());
    }

    fn row(id: &str, v: &str, errs: u64, png: u64, panics: u64, cat: &str) -> Row {
        Row {
            id: id.into(),
            verdict: v.into(),
            reason: format!("r-{v}"),
            rc: "0".into(),
            png_bytes: png,
            content_ratio: "1.0".into(),
            errlines: errs,
            panics,
            wall_ms: "1000".into(),
            first_paint_ms: String::new(),
            category: cat.into(),
        }
    }

    #[test]
    fn results_roundtrip() {
        let rows = vec![row("S001", "loaded", 3, 100, 0, "news"), row("S002", "broken", 0, 5, 1, "search")];
        assert_eq!(parse_results(&format_results(&rows)).unwrap(), rows);
        assert!(parse_results("id\tverdict\nS1\tloaded\n").is_err());
    }

    #[test]
    fn diff_reports_changes_and_gates_on_regression() {
        let old = vec![
            row("S001", "loaded", 10, 1000, 0, "news"),
            row("S002", "loaded", 10, 1000, 0, "news"),
            row("S003", "walled", 0, 100, 0, "search"),
            row("S004", "loaded", 10, 1000, 0, "search"),
        ];
        let new = vec![
            row("S001", "broken", 10, 1000, 2, "news"),
            row("S002", "loaded", 16, 1400, 0, "news"),
            row("S003", "loaded", 0, 100, 0, "search"),
            row("S004", "loaded", 10, 1000, 0, "search"),
        ];
        let d = diff(&old, &new);
        assert!(d.regression);
        assert!(d.text.contains("S001\tloaded -> broken"), "{}", d.text);
        assert!(d.text.contains("S003\twalled -> loaded"), "{}", d.text);
        assert!(d.text.contains("S001\t2 panic line(s)"), "{}", d.text);
        assert!(d.text.contains("S002\terrlines 10 -> 16"), "{}", d.text);
        assert!(!d.text.contains("S004\terrlines"), "{}", d.text);
        assert!(d.text.contains("news\t2/0/0 -> 1/1/0"), "{}", d.text);
        assert!(d.text.contains("TOTAL\t3/0/1 -> 3/1/0"), "{}", d.text);

        // walled -> loaded and broken -> loaded are not regressions.
        let better = vec![row("S001", "loaded", 10, 1000, 0, "news")];
        let worse = vec![row("S001", "walled", 10, 1000, 0, "news")];
        assert!(!diff(&worse, &better).regression);
        assert!(!diff(&better, &worse).regression);
        assert!(diff(&[], &[]).text.contains("TOTAL"));
    }

    #[test]
    fn list_categories() {
        let m = read_list("# h\nS001\tnews\thttps://a/\tnote\nS002\tsearch\thttps://b/\t\n");
        assert_eq!(m.get("S002").map(String::as_str), Some("search"));
        assert_eq!(m.len(), 2);
    }
}
