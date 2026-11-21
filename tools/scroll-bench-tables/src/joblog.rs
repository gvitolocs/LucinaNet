//! The matrix job logs: which run happened inside which clock window, under what load.
//!
//! The logs hold three header formats, because the scripts were edited between the runs - `### RUN page=... scale=
//! ... raster=... pace=... run=...`, `### RUN` without the `run=`, and only the script's own `=== NAME url  WxH
//! device px at scale S  [rust raster]`. m1's directories were renamed to carry the rasterizer and the pace *after*
//! the run, so a name cannot be trusted to point at the log lines of its own run; the clock can. Each block is
//! matched to a directory by the time its json was written, which is the time the run ended: the block window is the
//! `before:` clock to the `after:` clock, and the files inside a directory are written between them.
//!
//! The patterns are matched here by hand rather than with a regex crate, because the tool links nothing but the
//! standard library and serde_json, and because two of them turn on what a greedy `.*` gives up to: the load
//! printed is the number after the last `load average` label on the line - the 1-minute one, when `uptime`
//! answers with three - and the clock an `after:` gives up is the last one.

use crate::run::{Event, Run};

#[derive(Clone, Debug, Default)]
pub struct Block {
    /// What the block says it ran, when it says at all: m1's headers name five parts and its run lines do not
    /// always exist, so some blocks claim nothing and are checked against nothing.
    pub key: Option<[String; 4]>,
    pub t_before: Option<i64>,
    pub load_before: Option<f64>,
    pub wall_ms: Option<u64>,
    pub t_after: Option<i64>,
}

/// `^\s+before: HH:MM:SS up .*load averages?: ([0-9.]+)`
fn before_line(line: &str) -> Option<(i64, f64)> {
    let rest = spaced(line)?.strip_prefix("before: ")?;
    let (clock, rest) = clock(rest)?;
    if !rest.starts_with(" up ") {
        return None;
    }
    Some((clock, last_load(rest)?))
}

/// `^\s+(\d+) ms wall, exit (\d+).*after: HH:MM:SS up .*load averages?: ([0-9.]+)`
fn after_line(line: &str) -> Option<(u64, i64)> {
    let rest = spaced(line)?;
    let (wall, rest) = digits(rest)?;
    let rest = rest.strip_prefix(" ms wall, exit ")?;
    let (_exit, rest) = digits(rest)?;
    let rest = last_occurrence(rest, "after: ")?;
    let (clock, rest) = clock(rest)?;
    if !rest.starts_with(" up ") {
        return None;
    }
    // The load after the run is read because the line has to end with one for the block to be a finished run; the
    // table prints the load before it, which is the machine the frame times were measured on.
    last_load(rest)?;
    Some((wall, clock))
}

/// `^=== (\S+)\s+(\S+)\s+\d+x\d+ device px at scale (\S+)\s+\[(\w+) raster\]`, of which only the name is used: the
/// configuration it names comes from the name's own parts, the way the analysis takes them.
fn header_line(line: &str) -> Option<String> {
    let rest = line.strip_prefix("=== ")?;
    let (name, rest) = word(rest)?;
    let rest = blanks(rest)?;
    let (_url, rest) = word(rest)?;
    let rest = blanks(rest)?;
    let (_w, rest) = digits(rest)?;
    let rest = rest.strip_prefix('x')?;
    let (_h, rest) = digits(rest)?;
    let rest = rest.strip_prefix(" device px at scale ")?;
    let (_scale, rest) = word(rest)?;
    let rest = blanks(rest)?;
    let rest = rest.strip_prefix('[')?;
    let (_raster, rest) = word_chars(rest)?;
    rest.strip_prefix(" raster]")?;
    Some(name.to_string())
}

/// `^### RUN page=(\S+) scale=(\S+) raster=(\S+) pace=(\S+)(?: run=(\d+))?`, the key a block is checked against.
fn run_line(line: &str) -> Option<[String; 4]> {
    let rest = line.strip_prefix("### RUN page=")?;
    let (page, rest) = word(rest)?;
    let rest = rest.strip_prefix(" scale=")?;
    let (scale, rest) = word(rest)?;
    let rest = rest.strip_prefix(" raster=")?;
    let (raster, rest) = word(rest)?;
    let rest = rest.strip_prefix(" pace=")?;
    let (pace, _) = word(rest)?;
    Some([page.to_string(), format!("s{scale}"), raster.to_string(), pace.to_string()])
}

pub fn load_log(path: &std::path::Path) -> Vec<Block> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    // `open(path, errors="replace")`: an undecodable byte becomes one replacement character, as it does for Python,
    // and the line it sits on then simply fails to match.
    let text = String::from_utf8_lossy(&bytes);
    let mut blocks: Vec<Block> = Vec::new();
    // `cur` in the analysis: the block the lines so far belong to, and None once its wall line has closed it.
    let mut open: Option<usize> = None;
    for line in text.split('\n') {
        if line.starts_with("== machine load around each run") {
            break;
        }
        if let Some(key) = run_line(line) {
            blocks.push(Block { key: Some(key), ..Default::default() });
            open = Some(blocks.len() - 1);
            continue;
        }
        if let Some(name) = header_line(line) {
            if open.is_none_or(|i| blocks[i].wall_ms.is_some() || blocks[i].t_before.is_some()) {
                blocks.push(Block::default());
                open = Some(blocks.len() - 1);
            }
            let parts: Vec<&str> = name.split('-').collect();
            if parts.len() == 5 && parts[1].starts_with('s') {
                // `setdefault`: a `### RUN` line above has already named this block, and its name wins - it was the
                // directories that were renamed after the runs, not the logs.
                if let Some(i) = open {
                    if blocks[i].key.is_none() {
                        blocks[i].key = Some([0, 1, 2, 3].map(|j| parts[j].to_string()));
                    }
                }
            }
            continue;
        }
        if let Some((at, load)) = before_line(line) {
            if let Some(i) = open {
                if blocks[i].t_before.is_none() {
                    blocks[i].t_before = Some(at);
                    blocks[i].load_before = Some(load);
                }
            }
            continue;
        }
        if let Some((wall, at)) = after_line(line) {
            if let Some(i) = open {
                if blocks[i].wall_ms.is_none() {
                    blocks[i].wall_ms = Some(wall);
                    blocks[i].t_after = Some(at);
                    open = None;
                }
            }
        }
    }
    // A block whose wall line was never written is a run still going, or a header the script printed and then filled
    // in no timings for: no load to attribute either of them.
    blocks.retain(|b| b.t_before.is_some() && b.t_after.is_some());
    blocks
}

/// Attach to each run the block whose clock window holds the time its json was written, and count the runs whose
/// block names a different configuration than the directory does.
///
/// Where the two disagree the run keeps no block: a load line belongs to some run, and it is not this one, so
/// choosing it would attribute a machine state to the wrong row.
pub fn assign(runs: &mut [Run], blocks: &[Block]) -> usize {
    let mut bad = 0;
    for r in runs {
        r.event = None;
        // A run that falls in the tail of one block and the head of the next is matched to whichever of them ended
        // first: `min_by_key` keeps the first minimum, as Python's `min` does.
        let Some(best) = (0..blocks.len())
            .filter(|&i| {
                blocks[i].t_before.unwrap_or_default() - 5 <= r.mtime
                    && r.mtime <= blocks[i].t_after.unwrap_or_default() + 30
            })
            .min_by_key(|&i| (blocks[i].t_after.unwrap_or_default() - r.mtime).abs())
        else {
            continue;
        };
        if let Some(key) = &blocks[best].key {
            if key[..] != [r.page.as_str(), r.scale.as_str(), r.raster.as_str(), r.pace.as_str()] {
                bad += 1;
                continue;
            }
        }
        r.event = Some(Event {
            load_before: blocks[best].load_before.unwrap_or(0.0),
            wall_ms: blocks[best].wall_ms.unwrap_or(0),
        });
    }
    bad
}

/// `^\s+` - at least one blank, without taking the greedy `+`'s match away from what follows.
fn spaced(line: &str) -> Option<&str> {
    if !line.starts_with(is_space) {
        return None;
    }
    Some(trim_blanks(line))
}

fn is_space(c: char) -> bool {
    c == ' ' || c == '\t'
}

fn trim_blanks(s: &str) -> &str {
    let end = s.find(|c: char| !is_space(c)).unwrap_or(s.len());
    &s[end..]
}

/// `\S+`, which spans neither a space nor a tab and so is never empty.
fn word(s: &str) -> Option<(&str, &str)> {
    if s.is_empty() || s.starts_with(is_space) {
        return None;
    }
    let end = s.find(is_space).unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

/// `\s+` where a token has to start: at least one blank.
fn blanks(s: &str) -> Option<&str> {
    if !s.starts_with(is_space) {
        return None;
    }
    Some(trim_blanks(s))
}

/// `\w+` - a rasterizer's name.
fn word_chars(s: &str) -> Option<(&str, &str)> {
    let end = s.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    Some((&s[..end], &s[end..]))
}

/// `(\d+)`, maximal, so that what follows it starts where the digits stop.
fn digits(s: &str) -> Option<(u64, &str)> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    Some((s[..end].parse().ok()?, &s[end..]))
}

/// `(\d\d:\d\d:\d\d)` through `clock()`: two digits each, and seconds since midnight.
fn clock(s: &str) -> Option<(i64, &str)> {
    let b = s.as_bytes();
    let pair = |i: usize| -> Option<i64> {
        if i + 1 < b.len() && b[i].is_ascii_digit() && b[i + 1].is_ascii_digit() {
            Some((b[i] - b'0') as i64 * 10 + (b[i + 1] - b'0') as i64)
        } else {
            None
        }
    };
    if b.get(2) != Some(&b':') || b.get(5) != Some(&b':') {
        return None;
    }
    let (h, m, sec) = (pair(0)?, pair(3)?, pair(6)?);
    // Eight characters, and in both patterns the group is followed by a literal space, so a longer digit run is not
    // this clock either way.
    Some((h * 3600 + m * 60 + sec, s.get(8..)?))
}

/// `.*load averages?: ([0-9.]+)` - the digits after the last `load average` label on the line. The greedy `.*`
/// walks back to the last label that is followed by a colon and a number, and `[0-9.]+` then stops at the first
/// space, so of `load averages: 61.32 68.10 72.01` the analysis takes 61.32: the 1-minute load, which is what a
/// run starts under.
fn last_load(s: &str) -> Option<f64> {
    let mut limit = s.len();
    loop {
        let at = s[..limit].rfind("load average")?;
        let tail = &s[at + "load average".len()..];
        // The `s?` of `averages?`, then the `: ` and the run of digits and dots.
        let tail = tail.strip_prefix('s').unwrap_or(tail);
        if let Some(rest) = tail.strip_prefix(": ") {
            let end = rest.find(|c: char| !(c == '.' || c.is_ascii_digit())).unwrap_or(rest.len());
            if end > 0 {
                if let Ok(v) = rest[..end].parse::<f64>() {
                    return Some(v);
                }
            }
        }
        if at == 0 {
            return None;
        }
        limit = at;
    }
}

/// `.*needle` for a greedy `.*`: the last occurrence, or None where there is no needle at all.
fn last_occurrence<'a>(s: &'a str, needle: &str) -> Option<&'a str> {
    let at = s.rfind(needle)?;
    Some(&s[at + needle.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn before_line_reads_the_clock_and_the_one_minute_load() {
        let (at, load) = before_line("  before: 20:58:45 up 3 users, load averages: 61.32 68.10 72.01").unwrap();
        assert_eq!(at, 20 * 3600 + 58 * 60 + 45);
        // The first of the three, because `[0-9.]+` stops at the space after it.
        assert_eq!(load, 61.32);
    }

    #[test]
    fn a_second_load_label_on_the_line_wins_over_the_first() {
        // The greedy `.*` before the label walks back only when the last label is not the one with the
        // reading after it.
        let (_, load) = before_line("  before: 09:00:00 up load average: 1.0 (was load averages: 9.9)").unwrap();
        assert_eq!(load, 9.9);
        let (_, load) = before_line("  before: 09:00:00 up load average: 3.0 then load average 4.0").unwrap();
        assert_eq!(load, 3.0, "the last label has no colon, so the first one's reading stands");
        let (_, load) = before_line("  before: 09:00:00 up 3 users, 1 min load average: 0.5").unwrap();
        assert_eq!(load, 0.5);
    }

    #[test]
    fn a_load_average_singular_is_read_too() {
        // Linux's `uptime` prints the one number three times, macOS prints the plural.
        let (_, load) = before_line("\tbefore: 00:00:07 up load average: 1.5").unwrap();
        assert_eq!(load, 1.5);
    }

    #[test]
    fn before_line_wants_the_whole_shape() {
        assert!(before_line("before: 20:58:45 up load average: 1.0").is_none(), "no leading blank");
        assert!(before_line("  before:20:58:45 up load average: 1.0").is_none(), "no space after the colon");
        assert!(before_line("  before: 20:58:45 down load average: 1.0").is_none(), "no ` up `");
        assert!(before_line("  before: 2:58:45 up load average: 1.0").is_none(), "one-digit hour");
        assert!(before_line("  before: 20:58:45 up no load here").is_none(), "no load at all");
    }

    #[test]
    fn after_line_gives_up_the_last_after_colon() {
        // The greedy `.*` before `after:` is what makes a wall line that quotes another line still read its own end.
        let (wall, at) =
            after_line("  18932 ms wall, exit 0; after: 21:00:14 up load averages: 62.00 63 64").unwrap();
        assert_eq!(wall, 18932);
        assert_eq!(at, 21 * 3600 + 14);
        assert!(after_line("  18932 ms wall, exit 0").is_none());
        assert!(after_line("  18932 ms wall exit 0; after: 21:00:14 up load average: 1").is_none());
    }

    #[test]
    fn headers_are_named_by_their_five_parts_or_not_at_all() {
        let line = "=== long-s2-rust-vsync-r1 file:///tmp/long.html 3480x2180 device px at scale 2  [rust raster]";
        assert_eq!(header_line(line).as_deref(), Some("long-s2-rust-vsync-r1"));
        // Three parts is a name the analysis will not take a key from, and four either.
        assert_eq!(
            header_line("=== long-s2-rust file:///x 800x600 device px at scale 1 [swgl raster]").as_deref(),
            Some("long-s2-rust")
        );
        assert!(header_line("=== long-s2-rust-vsync-r1 url 800x600 device px at scale 1 [swgl rasterx]").is_none());
        assert!(header_line("== long-s2-rust-vsync-r1 url 800x600 device px at scale 1 [swgl raster]").is_none());
    }

    #[test]
    fn run_lines_name_their_configuration() {
        let key = run_line("### RUN page=long scale=2 raster=rust pace=vsync run=3 20:58").unwrap();
        assert_eq!(key, ["long", "s2", "rust", "vsync"].map(String::from));
        // The `run=` is optional in the pattern, and so is everything after it.
        let key = run_line("### RUN page=wiki scale=1 raster=swgl pace=fast").unwrap();
        assert_eq!(key, ["wiki", "s1", "swgl", "fast"].map(String::from));
        assert!(run_line("### RUN page=long scale=2 raster=rust pace=").is_none());
    }

    #[test]
    fn a_block_needs_both_ends_of_its_window() {
        let log = "\
### RUN page=long scale=2 raster=rust pace=vsync run=1
  before: 10:00:00 up load averages: 10.0 10.1 10.2
  1000 ms wall, exit 0; after: 10:00:30 up load averages: 11.0 11.1 11.2
### RUN page=long scale=2 raster=rust pace=vsync run=2
  before: 10:01:00 up load averages: 12.0 12.1 12.2
";
        let blocks = blocks_of("window", log);
        assert_eq!(blocks.len(), 1, "the run still going has no load to attribute");
        assert_eq!(blocks[0].t_before, Some(10 * 3600));
        assert_eq!(blocks[0].wall_ms, Some(1000));
        assert_eq!(blocks[0].key.as_ref().unwrap()[..], ["long", "s2", "rust", "vsync"].map(String::from)[..]);
    }

    #[test]
    fn a_run_line_names_its_block_and_a_lone_header_names_the_next() {
        let log = "\
### RUN page=long scale=2 raster=rust pace=vsync
=== long-s1-rust-free-r1 file:///long.html 1920x1080 device px at scale 1  [rust raster]
  before: 10:00:00 up load average: 3.0
  1 ms wall, exit 0; after: 10:00:01 up load average: 4.0
=== wiki-s2-rust-vsync-r2 file:///wiki.html 1920x1080 device px at scale 2  [rust raster]
  before: 10:02:00 up load average: 5.0
  1 ms wall, exit 0; after: 10:02:01 up load average: 6.0
";
        let blocks = blocks_of("both", log);
        assert_eq!(blocks.len(), 2);
        // The header that followed the unfinished run block joined it rather than opening a block of its own, so the
        // pair is the run line's key with the header's timings; only the second header stands on its own.
        assert_eq!(blocks[0].key.as_ref().unwrap()[..], ["long", "s2", "rust", "vsync"].map(String::from)[..]);
        assert_eq!(blocks[0].t_before, Some(10 * 3600));
        assert_eq!(blocks[1].key.as_ref().unwrap()[..], ["wiki", "s2", "rust", "vsync"].map(String::from)[..]);
    }

    #[test]
    fn a_run_line_wins_over_the_header_beneath_it() {
        // m1's `### RUN` lines carry no run=, and its directories were renamed afterwards: the header under the run
        // line names a different cell, and the analysis keeps the run line's key.
        let log = "\
### RUN page=long scale=2 raster=swgl pace=free
=== long-s2-rust-vsync-r9 file:///long.html 1920x1080 device px at scale 2  [rust raster]
  before: 10:00:00 up load average: 3.0
  1 ms wall, exit 0; after: 10:00:01 up load average: 4.0
";
        let blocks = blocks_of("run_line_wins", log);
        assert_eq!(blocks[0].key.as_ref().unwrap()[..], ["long", "s2", "swgl", "free"].map(String::from)[..]);
    }

    #[test]
    fn parsing_stops_where_collect_condensed_the_log() {
        let log = "\
=== long-s2-rust-vsync-r1 file:///long.html 1920x1080 device px at scale 2  [rust raster]
  before: 10:00:00 up load average: 3.0
  1 ms wall, exit 0; after: 10:00:01 up load average: 4.0
== machine load around each run (from the job log) ==
=== long-s2-rust-vsync-r2 file:///long.html 1920x1080 device px at scale 2  [rust raster]
  before: 11:00:00 up load average: 9.0
  1 ms wall, exit 0; after: 11:00:01 up load average: 9.5
";
        assert_eq!(blocks_of("condensed", log).len(), 1);
    }

    /// Writes a log to a scratch name, reads it back, and takes the file away again.
    fn blocks_of(name: &str, text: &str) -> Vec<Block> {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("scroll-bench-tables-log-{name}"));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
        let blocks = load_log(&path);
        drop(file);
        std::fs::remove_file(&path).unwrap();
        blocks
    }
}
