/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The writer: one `errors.jsonl` in the profile directory, appended as pages hit errors.
//!
//! Three things this file has to survive — a tab stuck in a loop, a disk with no room left, and a
//! power cut — and each one is a rule about how little it may write:
//!
//! - a repeat of an error is counted, not written. Its line goes out again only when the count
//!   reaches the next power of two, so a loop that runs a thousand times a second costs ten lines
//!   and still shows the real total on the newest one;
//! - the file is capped at [`MAX_BYTES`] and rolled to `errors.1.jsonl` … `errors.3.jsonl`, oldest
//!   dropped, which is what stops an error log eating the storage on Juno;
//! - one session stops at [`MAX_LINES`] with a single [`Kind::Truncated`] marker, so one bad page
//!   cannot push the sessions before it out of the file.
//!
//! Writing never fails upward: an event that cannot be written is dropped and the browser carries on
//! without an error log rather than stopping with one. The file is append-only and flushed line by
//! line, so a crash loses at most the line in flight — which the reader skips and counts — and
//! nothing that came before it.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::model::{Event, Kind};

/// The one file the log keeps in the profile directory, as `prefs.json` and `zoom.json` each keep
/// their own (`crates/lucinanet-servo/src/engine.rs`, `profile_dir`).
pub const FILE_NAME: &str = "errors.jsonl";

/// How big [`FILE_NAME`] may grow before it rolls over. A line is a few hundred bytes, so this is
/// more than a thousand distinct errors and about no measurable space on a machine that runs a
/// browser.
pub const MAX_BYTES: u64 = 256 * 1024;

/// How many rolled-away files are kept, `errors.1.jsonl` being the newest of them.
pub const GENERATIONS: usize = 3;

/// How many lines one session may write before it stops being listened to.
pub const MAX_LINES: u32 = 500;

/// What the log remembers about an error it has already written: how many times it has happened, and
/// at what count the line goes out again.
type Seen = (u32, u32);

/// An error's identity — everything about it except when it happened, how many times, and in which
/// tab. Two events with this key are one line in the file, and the reader folds its repeat lines
/// back into it with the same key.
pub(crate) type Key = (Kind, String, String, String, u32, String);

pub(crate) fn key_of(event: &Event) -> Key {
    (
        event.kind,
        event.page_host.clone(),
        event.host.clone(),
        event.file.clone(),
        event.line,
        event.detail.clone(),
    )
}

/// What a `Clear` control takes out of the log. The page's link and the writer's order are the same
/// thing, so the two cannot disagree about what a click means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Every line there is: this session's, the older sessions', and the rolled-away generations'.
    All,
    /// The lines of one page host. The empty string is the group of pages that have no host
    /// (`about:`, `data:`, a new tab), which is a row on the page and a thing to clear like any
    /// other.
    Host(String),
}

impl Target {
    /// Whether one line of the log is in this target. A line that cannot be read is in none of them:
    /// clearing one host has no right to delete what it cannot identify, and only [`Target::All`]
    /// takes the whole file.
    pub fn holds(&self, event: &Event) -> bool {
        match self {
            Target::All => true,
            Target::Host(page_host) => &event.page_host == page_host,
        }
    }
}

/// The session's error log.
///
/// One writer's business: no locks, no threads, no clock. The wiring hands over the time and the
/// session number and calls [`Log::record`] from whichever thread owns the profile.
#[derive(Debug)]
pub struct Log {
    session: u32,
    /// The live file, open for appending. `None` once a rollover could not reopen it, which is the
    /// only way this log stops working.
    file: Option<File>,
    /// `None` for a [`Log::memory`] log: no directory, so no file is ever touched.
    dir: Option<PathBuf>,
    /// Bytes in the live file, so the rollover decision costs no second look at it.
    written: u64,
    /// Lines this session has written, marker included.
    lines: u32,
    /// Events folded into a line already written, or refused after the cap. The marker counts them.
    suppressed: u32,
    truncated: bool,
    /// Whether the marker line has gone out. It goes out once, and once only, when the log closes.
    marker_written: bool,
    last_t_ms: u64,
    seen: BTreeMap<Key, Seen>,
    /// The lines a [`Log::memory`] log would have written, oldest first. Empty for a file log, which
    /// keeps nothing in memory: read the file back with [`crate::read_sessions`].
    events: Vec<Event>,
}

impl Log {
    /// Open, or create, the log in `dir` for session `session`. The directory is made here, so a
    /// first run has somewhere to put the file; anything else that goes wrong is the `io::Error`
    /// this returns, and the wiring's decision to carry on with no log at all.
    pub fn open(dir: &Path, session: u32) -> std::io::Result<Log> {
        std::fs::create_dir_all(dir)?;
        let (file, written) = open_append(dir)?;
        Ok(Log {
            session,
            file: Some(file),
            dir: Some(dir.to_path_buf()),
            written,
            lines: 0,
            suppressed: 0,
            truncated: false,
            marker_written: false,
            last_t_ms: 0,
            seen: BTreeMap::new(),
            events: Vec::new(),
        })
    }

    /// A log that keeps its lines in memory and touches no file: what the wiring uses when there is
    /// no profile directory (`LUCINANET_PROFILE_DIR=none`), so a private window still answers
    /// `about:errors` for as long as it lives. The counting and the caps are the same as the file's;
    /// only the writing is not.
    pub fn memory(session: u32) -> Log {
        Log {
            session,
            file: None,
            dir: None,
            written: 0,
            lines: 0,
            suppressed: 0,
            truncated: false,
            marker_written: false,
            last_t_ms: 0,
            seen: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    /// The session number this log stamps onto every event it is given.
    pub fn session(&self) -> u32 {
        self.session
    }

    /// The lines this log wrote, oldest first.
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Whether this session hit its cap. True from the first event refused for it, before the marker
    /// line that reports it has gone out.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Write the [`Kind::Truncated`] line, if this session was cut off and it has not been written
    /// yet. [`Drop`] calls it, so a log that merely goes out of scope still says how much it left
    /// out; call it to get the line before the session ends.
    pub fn finish(&mut self) {
        if self.truncated && !self.marker_written {
            self.marker_written = true;
            let marker = self.marker();
            self.write(&marker);
        }
    }

    /// Record one event: count it, and write its line if counting says it is time.
    ///
    /// This log's session number goes onto the event, so a line always says which session it came
    /// from whoever wrote it, and a `count` of none becomes one.
    pub fn record(&mut self, event: Event) {
        let mut event = event;
        event.session = self.session;
        event.count = event.count.max(1);

        let count = match self.seen.get_mut(&key_of(&event)) {
            Some((seen, next_write)) => {
                *seen = seen.saturating_add(1);
                if *seen < *next_write {
                    self.suppressed += 1;
                    return;
                }
                *next_write = seen.saturating_mul(2);
                *seen
            }
            None => {
                // The first line of a key goes out at once, whatever count the wiring brought with
                // it; the next one goes out when the count has doubled.
                let count = event.count;
                self.seen.insert(key_of(&event), (count, count.saturating_mul(2)));
                count
            }
        };
        event.count = count;

        if self.lines >= MAX_LINES {
            // Refused, counted, and said once — at the close, when the number of refusals is worth
            // telling.
            self.truncated = true;
            self.suppressed += 1;
            return;
        }
        self.write(&event);
    }

    /// Take [`Target`]'s lines out of the log, and say how many went.
    ///
    /// The live file is rewritten and opened again — the handle this log holds points at the bytes
    /// that were there before, and after a rewrite those bytes are in a file nobody can reach. The
    /// generations beside it are rewritten, or removed when nothing in them is left. A
    /// [`Log::memory`] log simply keeps what the target does not name.
    ///
    /// The counting starts over with it: the dedupe map is emptied, and the session is back to
    /// [`MAX_LINES`] of its own. A person who cleared `example.com` and goes back to the same page
    /// must be told about the error again, not met with a count nobody can see.
    pub fn clear(&mut self, target: &Target) -> std::io::Result<usize> {
        let removed = match &self.dir {
            None => {
                let before = self.events.len();
                self.events.retain(|event| !target.holds(event));
                before - self.events.len()
            }
            Some(dir) => clear_files(dir, target)?,
        };
        self.seen.clear();
        self.lines = 0;
        self.suppressed = 0;
        self.truncated = false;
        self.marker_written = false;
        if let Some(dir) = &self.dir {
            // The rewrite went, so the removal is true whatever this does next. A handle that will
            // not open again is the state a failed roll leaves too: nothing more this session says
            // reaches the file, and the events after it are counted rather than written.
            match open_append(dir) {
                Ok((file, written)) => {
                    self.file = Some(file);
                    self.written = written;
                }
                Err(_) => self.file = None,
            }
        }
        Ok(removed)
    }

    /// The line that says this session was cut off, and how much of it was left out.
    fn marker(&self) -> Event {
        Event {
            t_ms: self.last_t_ms,
            session: self.session,
            tab: 0,
            page_host: String::new(),
            kind: Kind::Truncated,
            detail: format!("cut off at {MAX_LINES} lines; {suppressed} more events counted only", suppressed = self.suppressed),
            host: String::new(),
            file: String::new(),
            line: 0,
            count: self.suppressed.max(1),
        }
    }

    /// Append one flushed line, rolling the file first if the line would take it past [`MAX_BYTES`],
    /// or keep it in memory. A failure here is the end of the story: the event is gone and the
    /// browser is not.
    fn write(&mut self, event: &Event) {
        self.last_t_ms = event.t_ms;
        let line = event.to_line();
        let Some(dir) = self.dir.clone() else {
            self.events.push(event.clone());
            self.lines = self.lines.saturating_add(1);
            return;
        };
        if self.written + line.len() as u64 + 1 > MAX_BYTES {
            self.roll(&dir);
        }
        let Some(file) = self.file.as_mut() else {
            return; // The roll could not reopen the file: nothing more can be written this session.
        };
        let mut bytes = line.into_bytes();
        bytes.push(b'\n');
        if file.write_all(&bytes).is_ok() && file.flush().is_ok() {
            self.written = self.written.saturating_add(bytes.len() as u64);
            self.lines = self.lines.saturating_add(1);
        }
    }

    /// `errors.3.jsonl` goes, `errors.2` becomes `errors.3`, `errors.1` becomes `errors.2`, the live
    /// file becomes `errors.1`, and a fresh live file opens. A rename with nothing to move is the
    /// normal case until there are three generations, and says nothing about it.
    fn roll(&mut self, dir: &Path) {
        for older in (1..GENERATIONS).rev() {
            let _ = std::fs::rename(generation_path(dir, older), generation_path(dir, older + 1));
        }
        let _ = std::fs::rename(dir.join(FILE_NAME), generation_path(dir, 1));
        self.file = None;
        self.written = 0;
        self.file = open_append(dir).map(|(file, _)| file).ok();
    }
}

impl Drop for Log {
    /// A log that goes out of scope closes: the marker line, if there is one to write, goes out here
    /// rather than being lost.
    fn drop(&mut self) {
        self.finish();
    }
}

/// The files of the log, oldest first: `errors.3.jsonl` … `errors.1.jsonl`, then the live
/// `errors.jsonl` last. The reader walks them in this order so that one session's lines come out in
/// the order they went in, across any number of rollovers.
pub(crate) fn log_paths(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = (1..=GENERATIONS).rev().map(|older| generation_path(dir, older)).collect();
    paths.push(dir.join(FILE_NAME));
    paths
}

/// `errors.1.jsonl` for 1, the live file for 0.
pub(crate) fn generation_path(dir: &Path, older: usize) -> PathBuf {
    if older == 0 {
        return dir.join(FILE_NAME);
    }
    dir.join(format!("errors.{older}.jsonl"))
}

fn open_append(dir: &Path) -> std::io::Result<(File, u64)> {
    let file = OpenOptions::new().create(true).append(true).open(dir.join(FILE_NAME))?;
    let written = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    Ok((file, written))
}

/// Take [`Target`]'s lines out of a profile directory's own files, and say how many went. This is
/// [`Log::clear`] without a log: the wiring uses it when the run has no writer thread to ask —
/// `LUCINANET_ERRLOG=0`, or a profile that would not take one — and the file a previous session left
/// is still there to clear.
///
/// Nothing here creates anything. A directory with no log is `Ok(0)`, not a directory, and not an
/// error: a person who opens the page to find nothing in it has not asked for a profile.
pub fn clear_files(dir: &Path, target: &Target) -> std::io::Result<usize> {
    let mut removed = 0;
    for older in 1..=GENERATIONS {
        let path = generation_path(dir, older);
        let Some(kept) = kept_lines(&path, target, &mut removed)? else { continue };
        if kept.is_empty() {
            // Nothing of this target's left in it: the file goes rather than sitting there as zero
            // bytes for the next reader to open.
            std::fs::remove_file(&path)?;
        } else {
            write_atomic(&path, &kept)?;
        }
    }
    let live = dir.join(FILE_NAME);
    if let Some(kept) = kept_lines(&live, target, &mut removed)? {
        // The live file stays even when it is empty: it is the one this session writes to, and the
        // name is what the next line of the log expects.
        write_atomic(&live, &kept)?;
    }
    Ok(removed)
}

/// The lines of one file that the target does not take, `None` when there is no such file.
///
/// A line that is not an event is kept by a per-host clear and dropped by [`Target::All`]: clearing
/// one host has no right to delete what it cannot identify, while clearing the log means exactly that
/// the file is to hold nothing — a hand-edited line in it goes with the rest, and the next line the
/// log writes will not be judged by what the damaged one said.
fn kept_lines(path: &Path, target: &Target, removed: &mut usize) -> std::io::Result<Option<Vec<u8>>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut kept: Vec<u8> = Vec::with_capacity(bytes.len());
    // The file is rewritten line by line, always whole lines with a newline after each: a blank line
    // or a fragment a crash left carries nothing the reader would show, and both are gone as surely
    // as a line the target names.
    for line in bytes.split(|byte| *byte == b'\n').filter(|line| !line.iter().all(u8::is_ascii_whitespace)) {
        let event = Event::from_line(&String::from_utf8_lossy(line));
        let drops = match (event, target) {
            (Some(event), Target::Host(page_host)) => page_host == &event.page_host,
            (_, Target::All) => true,
            (None, Target::Host(_)) => false,
        };
        if drops {
            *removed += 1;
        } else {
            kept.extend_from_slice(line);
            kept.push(b'\n');
        }
    }
    Ok(Some(kept))
}

/// Replace a file's bytes so that a crash leaves either the old file or the new one: write the temp,
/// fsync it, rename over the target, then fsync the directory so the rename itself survives. The
/// temp is a dot file the reader never looks at, named after the process so two runs cannot write
/// the same one.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let file = File::create(&temp)?;
    write_all_and_sync(file, bytes)?;
    std::fs::rename(&temp, path)?;
    // The directory entry is the part that makes the rename durable; a filesystem that will not open
    // a directory for this loses the extra assurance and nothing else.
    if let Ok(dir) = File::open(dir) {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn write_all_and_sync(mut file: File, bytes: &[u8]) -> std::io::Result<()> {
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()
}
