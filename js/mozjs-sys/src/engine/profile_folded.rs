/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Pure folded-stack bookkeeping for the sampling JS profiler: no engine, no unsafe. The engine glue in
//! `profile.rs` turns captured stacks into `Frame`s and hands them here; the folded text is the input format
//! of Brendan Gregg's flamegraph.pl / inferno, the summary is the human-readable report.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// One captured stack frame, as `embed::capture_stack` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub name: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub native: bool,
}

/// Aggregated samples: folded key -> weight in microseconds, plus per-sample records for the summary.
pub struct Folded {
    stacks: BTreeMap<String, u64>,
    /// (names, files, weight) of samples taken inside JS; delayed samples are only counted in `delayed_us`.
    records: Vec<(Vec<String>, Vec<String>, u64)>,
    samples: u64,
    total_us: u64,
    delayed_us: u64,
}

/// Shorten a script URL to its last path segment with the query (and fragment) stripped:
/// `https://a.b/c/d.js?x=1` -> `d.js`.
fn shorten_file(file: &str) -> String {
    let no_query = file.split(['?', '#']).next().unwrap_or(file);
    no_query.rsplit('/').next().unwrap_or(no_query).to_string()
}

fn display_name(frame: &Frame, is_oldest: bool) -> String {
    match &frame.name {
        Some(name) => name.clone(),
        None if is_oldest => "<top>".to_string(),
        None => "<anonymous>".to_string(),
    }
}

fn display_file(frame: &Frame) -> String {
    frame
        .file
        .as_deref()
        .map(shorten_file)
        .unwrap_or_else(|| "<builtin>".to_string())
}

/// `name (file:line)`, with ` [native]` after the name for natives and no `:line` when the frame has none.
fn frame_text(frame: &Frame, is_oldest: bool) -> String {
    let mut text = display_name(frame, is_oldest);
    if frame.native {
        text.push_str(" [native]");
    }
    text.push_str(" (");
    text.push_str(&display_file(frame));
    if let Some(line) = frame.line {
        let _ = write!(text, ":{line}");
    }
    text.push(')');
    text
}

impl Folded {
    pub fn new() -> Self {
        Folded {
            stacks: BTreeMap::new(),
            records: Vec::new(),
            samples: 0,
            total_us: 0,
            delayed_us: 0,
        }
    }

    /// Add one sample. `frames_newest_first` is the captured stack (newest frame first); the folded key is
    /// oldest frame first, frames joined by `;`. A delayed sample (the interrupt fired much later than
    /// requested) gets a `(delayed)` leaf frame appended.
    pub fn add(&mut self, frames_newest_first: &[Frame], weight_us: u64, delayed: bool) {
        let mut parts: Vec<String> = frames_newest_first
            .iter()
            .enumerate()
            .map(|(i, f)| frame_text(f, i == frames_newest_first.len() - 1))
            .collect();
        parts.reverse();
        if delayed {
            parts.push("(delayed)".to_string());
        }
        let key = parts.join(";");
        *self.stacks.entry(key).or_insert(0) += weight_us;

        let names: Vec<String> = frames_newest_first
            .iter()
            .enumerate()
            .map(|(i, f)| display_name(f, i == frames_newest_first.len() - 1))
            .collect();
        let files: Vec<String> = frames_newest_first.iter().map(display_file).collect();
        // A delayed sample waited for the engine outside JS (parse/compile, DOM, layout, or idle between scripts): it is
        // kept in the folded stacks, marked, but not attributed to the files and functions of the stack that follows.
        if !delayed {
            self.records.push((names, files, weight_us));
        }

        self.samples += 1;
        self.total_us += weight_us;
        if delayed {
            self.delayed_us += weight_us;
        }
    }

    pub fn samples(&self) -> u64 {
        self.samples
    }

    pub fn total_us(&self) -> u64 {
        self.total_us
    }

    /// The folded-stack text: one `key weight` line per key, sorted by key for determinism.
    pub fn folded_text(&self) -> String {
        let mut out = String::new();
        for (key, weight) in &self.stacks {
            let _ = writeln!(out, "{key} {weight}");
        }
        out
    }

    /// Self weight per file: the newest frame of each sample (where the interrupt found the engine).
    pub fn self_by_file(&self) -> Vec<(String, u64)> {
        let mut map: BTreeMap<String, u64> = BTreeMap::new();
        for (_, files, weight) in &self.records {
            if let Some(file) = files.first() {
                *map.entry(file.clone()).or_insert(0) += weight;
            }
        }
        sorted_desc(map)
    }

    /// Inclusive weight per file: a file counts once per sample even if it appears in several frames.
    pub fn inclusive_by_file(&self) -> Vec<(String, u64)> {
        let mut map: BTreeMap<String, u64> = BTreeMap::new();
        for (_, files, weight) in &self.records {
            let mut seen: Vec<&str> = Vec::new();
            for file in files {
                if !seen.contains(&file.as_str()) {
                    seen.push(file);
                    *map.entry(file.clone()).or_insert(0) += weight;
                }
            }
        }
        sorted_desc(map)
    }

    /// Self weight per function name: the newest frame of each sample.
    pub fn self_by_function(&self) -> Vec<(String, u64)> {
        let mut map: BTreeMap<String, u64> = BTreeMap::new();
        for (names, _, weight) in &self.records {
            if let Some(name) = names.first() {
                *map.entry(name.clone()).or_insert(0) += weight;
            }
        }
        sorted_desc(map)
    }

    /// Weight of samples marked `(delayed)`: time the engine spent outside JS (parsing and compiling a script before its
    /// first instruction, the DOM, layout) before it reached the next interrupt point.
    pub fn delayed_us(&self) -> u64 {
        self.delayed_us
    }

    /// Human-readable summary: totals, then the top files by self and inclusive weight and the top functions
    /// by self weight, each with ms and % of the total sampled time.
    pub fn summary(&self, wall_ms: u64) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "total sampled: {} ms ({} samples)",
            self.total_us / 1000,
            self.samples
        );
        let _ = writeln!(out, "wall: {wall_ms} ms");
        let _ = writeln!(
            out,
            "of which delayed (outside JS before the next interrupt point: parse/compile, DOM, layout, idle; not in the tables below): {} ms",
            self.delayed_us / 1000
        );

        // Percentages are of the time sampled inside JS (the tables leave delayed samples out).
        let in_js = self.total_us - self.delayed_us;
        let pct = |weight: u64| -> String {
            if in_js == 0 {
                "0.0".to_string()
            } else {
                format!("{:.1}", weight as f64 * 100.0 / in_js as f64)
            }
        };

        let _ = writeln!(out, "top files by self weight:");
        for (file, weight) in self.self_by_file().into_iter().take(40) {
            let _ = writeln!(out, "  {file}  {} ms  {}%", weight / 1000, pct(weight));
        }
        let _ = writeln!(out, "top files by inclusive weight:");
        for (file, weight) in self.inclusive_by_file().into_iter().take(40) {
            let _ = writeln!(out, "  {file}  {} ms  {}%", weight / 1000, pct(weight));
        }
        let _ = writeln!(out, "top functions by self weight:");
        for (name, weight) in self.self_by_function().into_iter().take(60) {
            let _ = writeln!(out, "  {name}  {} ms  {}%", weight / 1000, pct(weight));
        }
        out
    }
}

impl Default for Folded {
    fn default() -> Self {
        Self::new()
    }
}

fn sorted_desc(map: BTreeMap<String, u64>) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = map.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}
