/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Recording a page's Media Source Extensions calls to disk, and replaying a recording in a test.
//!
//! `LUCINANET_MEDIA_MSE_DUMP=<dir>`: every player that attaches a MediaSource writes its `mse_*` calls to
//! `<dir>/<unix-seconds>-<pid>-<player-seq>/calls.tsv`, one line per call (`seq  call  sb  args...`, tab-separated, appended
//! and flushed per line), and every appended segment's bytes to `a<seq:06>-sb<sb>.bin` in the same directory, in full before
//! the log line. With the variable unset nothing is written and no file is touched. A write error turns that player's
//! recorder off (logged once); it never fails the page.
//!
//! [`replay`] reads such a directory and drives another player with the same calls in the same order, mapping the recorded
//! SourceBuffer ids to the ones the new player returns, so a recorded YouTube session plays again without a browser.

use std::io::{BufWriter, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use servo_media_player::mse::{AppendMode, AppendResult, MseError, SourceBufferId};
use servo_media_player::Player;

/// The directory `LUCINANET_MEDIA_MSE_DUMP` names, read once per process.
static DUMP_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Players in this process, numbered from 0 for the directory names.
static PLAYER_SEQ: AtomicUsize = AtomicUsize::new(0);

fn dump_dir() -> Option<&'static PathBuf> {
    DUMP_DIR.get_or_init(|| std::env::var_os("LUCINANET_MEDIA_MSE_DUMP").map(PathBuf::from)).as_ref()
}

/// This player's recorder, or none when `LUCINANET_MEDIA_MSE_DUMP` is unset.
pub fn recorder() -> Option<Recorder> {
    dump_dir().map(|base| Recorder { base: base.clone(), inner: Mutex::new(Inner::default()) })
}

#[derive(Default)]
struct Inner {
    /// This player's subdirectory, made on the first recorded call.
    dir: Option<PathBuf>,
    log: Option<BufWriter<std::fs::File>>,
    /// Lines written so far; the next line's `seq`.
    seq: u64,
    /// A write failed: the recorder is off, and the page never hears of it.
    broken: bool,
}

/// Records one player's `mse_*` calls. A write error turns it off, logged once; it never fails the page.
pub struct Recorder {
    base: PathBuf,
    inner: Mutex<Inner>,
}

impl Recorder {
    /// Runs `write` with the player's directory, the log and the line's `seq`; appends and flushes the line, or turns off.
    fn record(&self, write: impl FnOnce(&Path, &mut BufWriter<std::fs::File>, u64) -> std::io::Result<()>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.broken {
            return;
        }
        if inner.dir.is_none() {
            let n = PLAYER_SEQ.fetch_add(1, Ordering::Relaxed);
            let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let dir = self.base.join(format!("{secs}-{}-{n}", std::process::id()));
            match std::fs::create_dir_all(&dir).and_then(|_| std::fs::File::create(dir.join("calls.tsv"))) {
                Ok(f) => {
                    inner.dir = Some(dir);
                    inner.log = Some(BufWriter::new(f));
                },
                Err(e) => {
                    inner.broken = true;
                    trace!("MSE dump off, cannot record in {}: {e}", self.base.display());
                    return;
                },
            }
        }
        let seq = inner.seq;
        let dir = inner.dir.clone().unwrap();
        let log = inner.log.as_mut().unwrap();
        match write(&dir, &mut *log, seq).and_then(|()| log.flush()) {
            Ok(()) => inner.seq = seq + 1,
            Err(e) => {
                inner.broken = true;
                trace!("MSE dump off, a write failed: {e}");
            },
        }
    }

    pub fn attach(&self) {
        self.record(|_, log, seq| writeln!(log, "{seq}\tattach\t-"));
    }

    /// The returned id on the line, or `-` and `error` when the type was refused.
    pub fn add_source_buffer(&self, mime: &str, result: &Result<SourceBufferId, MseError>) {
        self.record(|_, log, seq| match result {
            Ok(id) => writeln!(log, "{seq}\tadd_source_buffer\t{id}\t{mime}"),
            Err(_) => writeln!(log, "{seq}\tadd_source_buffer\t-\t{mime}\terror"),
        });
    }

    pub fn remove_source_buffer(&self, sb: SourceBufferId) {
        self.record(|_, log, seq| writeln!(log, "{seq}\tremove_source_buffer\t{sb}"));
    }

    /// The bytes go to `a<seq:06>-sb<sb>.bin` in full before the log line, which also carries the result.
    pub fn append(&self, sb: SourceBufferId, current_time: f64, data: &[u8], result: &Result<AppendResult, MseError>) {
        self.record(|dir, log, seq| {
            let file = format!("a{seq:06}-sb{sb}.bin");
            std::fs::write(dir.join(&file), data)?;
            let out = match result {
                Ok(_) => String::from("ok"),
                Err(e) => format!("error:{e:?}"),
            };
            writeln!(log, "{seq}\tappend\t{sb}\t{current_time:?}\t{}\t{file}\t{out}", data.len())
        });
    }

    pub fn remove(&self, sb: SourceBufferId, start: f64, end: f64) {
        self.record(|_, log, seq| writeln!(log, "{seq}\tremove\t{sb}\t{start:?}\t{end:?}"));
    }

    pub fn reset_parser(&self, sb: SourceBufferId) {
        self.record(|_, log, seq| writeln!(log, "{seq}\treset_parser\t{sb}"));
    }

    pub fn set_mode(&self, sb: SourceBufferId, mode: AppendMode) {
        let mode = match mode {
            AppendMode::Segments => "segments",
            AppendMode::Sequence => "sequence",
        };
        self.record(|_, log, seq| writeln!(log, "{seq}\tset_mode\t{sb}\t{mode}"));
    }

    pub fn set_timestamp_offset(&self, sb: SourceBufferId, offset: f64) {
        self.record(|_, log, seq| writeln!(log, "{seq}\tset_timestamp_offset\t{sb}\t{offset:?}"));
    }

    pub fn set_append_window(&self, sb: SourceBufferId, start: f64, end: f64) {
        self.record(|_, log, seq| writeln!(log, "{seq}\tset_append_window\t{sb}\t{start:?}\t{end:?}"));
    }

    pub fn change_type(&self, sb: SourceBufferId, mime: &str) {
        self.record(|_, log, seq| writeln!(log, "{seq}\tchange_type\t{sb}\t{mime}"));
    }

    pub fn set_duration(&self, duration: f64) {
        self.record(|_, log, seq| writeln!(log, "{seq}\tset_duration\t-\t{duration:?}"));
    }

    pub fn end_of_stream(&self) {
        self.record(|_, log, seq| writeln!(log, "{seq}\tend_of_stream\t-"));
    }

    pub fn reopen(&self) {
        self.record(|_, log, seq| writeln!(log, "{seq}\treopen\t-"));
    }
}

/// What a replay did.
#[derive(Debug, Default)]
pub struct ReplayReport {
    /// `appendBuffer` calls replayed.
    pub appends: usize,
    /// Their errors, as `MseError`'s Debug.
    pub append_errors: Vec<String>,
    /// Each SourceBuffer's `buffered` after the last call, by the recorded id.
    pub buffered: Vec<(usize, Vec<Range<f64>>)>,
}

/// Replays a recorded session (the `calls.tsv` and `a*.bin` files in `dir`) into `player`, in order, mapping the recorded
/// SourceBuffer ids to the ones `player` returns. The recorded `attach` is skipped when the caller is attached already (a
/// refusal there only means so).
pub fn replay(dir: &Path, player: &dyn Player) -> Result<ReplayReport, String> {
    let log = dir.join("calls.tsv");
    let text = std::fs::read_to_string(&log).map_err(|e| format!("{}: {e}", log.display()))?;
    let mut map: Vec<(SourceBufferId, SourceBufferId)> = Vec::new();
    let mut report = ReplayReport::default();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        let call = f.get(1).copied().unwrap_or("");
        let sb = f.get(2).copied().unwrap_or("-");
        let args: &[&str] = f.get(3..).unwrap_or(&[]);
        match call {
            "attach" => {
                let _ = player.mse_attach();
            },
            "add_source_buffer" => {
                let mime = args.first().copied().ok_or("add_source_buffer without a mime")?;
                if let Ok(new) = player.mse_add_source_buffer(mime)
                    && let Ok(old) = sb.parse()
                {
                    map.push((old, new));
                }
            },
            "remove_source_buffer" => {
                if let Some(new) = mapped(&map, sb) {
                    player.mse_remove_source_buffer(new);
                    map.retain(|(_, n)| *n != new);
                }
            },
            "append" => {
                let Some(new) = mapped(&map, sb) else { continue };
                let current_time = f64_arg(args, 0, "append")?;
                let len: usize = args.get(1).copied().ok_or("append without a length")?.parse().map_err(|_| "append: bad length".to_string())?;
                let file = args.get(2).copied().ok_or("append without a file")?;
                let data = std::fs::read(dir.join(file)).map_err(|e| format!("{file}: {e}"))?;
                if data.len() != len {
                    return Err(format!("{file}: {len} bytes recorded, {} on disk", data.len()));
                }
                report.appends += 1;
                if let Err(e) = player.mse_append(new, data, current_time) {
                    report.append_errors.push(format!("{e:?}"));
                }
            },
            "remove" => {
                if let Some(new) = mapped(&map, sb) {
                    player.mse_remove(new, f64_arg(args, 0, "remove")?, f64_arg(args, 1, "remove")?);
                }
            },
            "reset_parser" => {
                if let Some(new) = mapped(&map, sb) {
                    player.mse_reset_parser(new);
                }
            },
            "set_mode" => {
                if let Some(new) = mapped(&map, sb) {
                    let mode = match args.first() {
                        Some(&"segments") => AppendMode::Segments,
                        Some(&"sequence") => AppendMode::Sequence,
                        _ => return Err(format!("set_mode: bad mode {:?}", args.first())),
                    };
                    player.mse_set_mode(new, mode);
                }
            },
            "set_timestamp_offset" => {
                if let Some(new) = mapped(&map, sb) {
                    player.mse_set_timestamp_offset(new, f64_arg(args, 0, "set_timestamp_offset")?);
                }
            },
            "set_append_window" => {
                if let Some(new) = mapped(&map, sb) {
                    player.mse_set_append_window(new, f64_arg(args, 0, "set_append_window")?, f64_arg(args, 1, "set_append_window")?);
                }
            },
            "change_type" => {
                if let Some(new) = mapped(&map, sb) {
                    let mime = args.first().copied().ok_or("change_type without a mime")?;
                    let _ = player.mse_change_type(new, mime);
                }
            },
            "set_duration" => player.mse_set_duration(f64_arg(args, 0, "set_duration")?),
            "end_of_stream" => player.mse_end_of_stream(),
            "reopen" => player.mse_reopen(),
            other => return Err(format!("{}: unknown call {other:?}", log.display())),
        }
    }
    report.buffered = map.iter().map(|(old, new)| (*old, player.mse_buffered(*new))).collect();
    Ok(report)
}

/// The id `player` returned for the recorded `sb`, if it was added.
fn mapped(map: &[(SourceBufferId, SourceBufferId)], sb: &str) -> Option<SourceBufferId> {
    let old: SourceBufferId = sb.parse().ok()?;
    map.iter().find(|(o, _)| *o == old).map(|(_, n)| *n)
}

/// Argument `i` of a call, an `f64` written with `{:?}` (NaN, inf and every digit round-trip).
fn f64_arg(args: &[&str], i: usize, call: &str) -> Result<f64, String> {
    let a = args.get(i).copied().ok_or_else(|| format!("{call}: missing argument {i}"))?;
    a.parse().map_err(|_| format!("{call}: bad argument {a:?}"))
}
