//! `LUCINANET_FRAME_LOG=<file>`: one line per presented frame, for the windowed app and for `--render`, so one
//! session of someone's own use shows where each frame went.
//!
//! Why here and not in `lucinanet-servo`: the servo crate depends on this one (never the other way round), the
//! window code (`desktop.rs`) lives here, and the record reuses [`crate::scrollbench::Frame`] and its parser, so
//! both crates call into this module and neither duplicates the other.
//!
//! The record is the offscreen bench's own: `paint`, `present` and `copy` are the engine's three timings of the
//! frame (the same microseconds `News::Painted` carries), and the stage columns come from the engine's and
//! WebRender's diagnostic lines, read back with [`Tail`] and folded with [`scrollbench::fold`] exactly as the
//! bench does. They exist only when the run has `LUCINANET_RENDER_STATS`, `LUCINANET_WR_PHASES` and
//! `LUCINANET_LAYOUT_PHASES` on and its stderr is a file (or `LUCINANET_FRAME_LOG_DIAG` names one). `why` needs
//! `LUCINANET_RENDERING_REASON` as well, and the engine says it at most once a second: `unknown` otherwise.
//!
//! Mapping of the bench's fields to the columns of a line:
//! `layout` = restyle and trees, `dl` = stacking contexts and display list, `scene` = WebRender's `update`,
//! `build` = `frame_build`, `render` = WebRender's `render`; `paint` is the whole `webview.paint()`.
//!
//! Windowed: the line of an engine frame is held until the window has drawn it, then written with the three
//! steps the window takes after the engine presented (`win_draw`, `win_upload`, `win_swap`) and the time of the
//! glass. Offscreen there is no window, so the line is written when the engine presents.
//!
//! Privacy: a line holds numbers and short enums only. Never a URL, a title, page text or a path.
//!
//! Cost: unset, every hook is one relaxed atomic load. Set, the hot path formats one line and hands it to a
//! writer thread through a bounded channel with `try_send`: it never waits for the disk, and a full channel
//! drops the line.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crate::Cmd;
use crate::scrollbench::{self, Frame, Tail};

/// The size at which the file is rolled to `<file>.1`: two files of at most this much each exist at any time.
pub const MAX_BYTES: u64 = 8 << 20;
/// How long a written line may stay in the writer's buffer.
const FLUSH_EVERY: Duration = Duration::from_millis(250);
/// Lines the writer may be behind by before new ones are dropped.
const QUEUE: usize = 4096;

// 0 = not configured (hooks do nothing), 1 = off, 2 = on.
static MODE: AtomicU8 = AtomicU8::new(0);
static WINDOWED: AtomicBool = AtomicBool::new(false);
/// Microseconds on [`epoch`]'s clock (plus one, so 0 means none) of the oldest input command since the last frame.
static OLDEST_INPUT_US: AtomicU64 = AtomicU64::new(0);
static GEOM_W: AtomicU32 = AtomicU32::new(0);
static GEOM_H: AtomicU32 = AtomicU32::new(0);
static GEOM_SCALE: AtomicU32 = AtomicU32::new(0);
static GEOM_REFRESH_MHZ: AtomicU32 = AtomicU32::new(0);
static TIMELINE: Mutex<Option<Timeline>> = Mutex::new(None);
/// Only the engine thread uses this one: the file reads of the diagnostics stay out of the window thread's lock.
static DIAG: Mutex<Option<Tail>> = Mutex::new(None);

fn epoch() -> Instant {
    static E: OnceLock<Instant> = OnceLock::new();
    *E.get_or_init(Instant::now)
}

fn now_us() -> u64 {
    epoch().elapsed().as_micros() as u64 + 1
}

fn lock<T>(m: &'static Mutex<T>) -> MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether the log is on. One relaxed load; false until [`configure`] has said otherwise.
pub fn enabled() -> bool {
    MODE.load(Ordering::Relaxed) == 2
}

/// What the environment asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Off,
    /// The path is in the profile directory, which holds browsing data.
    Refused,
    On(PathBuf),
}

/// Pure: the value of `LUCINANET_FRAME_LOG` and the profile directory in, the decision out. Unset or empty is off.
pub fn decide(env: Option<&OsStr>, profile: Option<&Path>) -> Decision {
    let Some(raw) = env.filter(|v| !v.is_empty()) else { return Decision::Off };
    let path = PathBuf::from(raw);
    match profile {
        Some(dir) if resolve(&path).starts_with(resolve(dir)) => Decision::Refused,
        _ => Decision::On(path),
    }
}

/// An absolute path with every part that exists resolved (links included), so a path through a link into the
/// profile directory is the profile directory's too. The parts that do not exist yet are kept as written.
fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map(|cwd| cwd.join(path)).unwrap_or_else(|_| path.to_path_buf())
    };
    let mut rest: Vec<&OsStr> = Vec::new();
    let mut at = absolute.as_path();
    loop {
        if let Ok(mut real) = at.canonicalize() {
            for part in rest.iter().rev() {
                real.push(part);
            }
            return real;
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                at = parent;
            },
            _ => return absolute.clone(),
        }
    }
}

const HEADER: &str = "# lucinanet frame log v1: one line per presented frame, key=value, times in ms, no urls or text";

/// Read the switch, once. The engine calls this first, with the profile directory it will use; before that the log
/// is off, which is also what a build with another engine gets.
pub fn configure(profile: Option<&Path>) {
    if MODE.load(Ordering::Relaxed) != 0 {
        return;
    }
    let env = std::env::var_os("LUCINANET_FRAME_LOG");
    match decide(env.as_deref(), profile) {
        Decision::Off => MODE.store(1, Ordering::Relaxed),
        Decision::Refused => {
            eprintln!("lucinanet: LUCINANET_FRAME_LOG refused: the profile directory holds browsing data; the frame log is off");
            MODE.store(1, Ordering::Relaxed);
        },
        Decision::On(path) => match Sink::open(&path, MAX_BYTES) {
            Ok(sink) => {
                let (tx, rx) = sync_channel(QUEUE);
                if std::thread::Builder::new().name("frame-log".into()).spawn(move || write_all(rx, sink)).is_err() {
                    eprintln!("lucinanet: frame log: no writer thread; the frame log is off");
                    MODE.store(1, Ordering::Relaxed);
                    return;
                }
                let _ = tx.try_send(HEADER.to_string());
                let diag = std::env::var_os("LUCINANET_FRAME_LOG_DIAG").map(PathBuf::from).or_else(stderr_path);
                if let Some(diag) = diag.filter(|d| resolve(d) != resolve(&path)) {
                    let mut tail = Tail::new(&diag);
                    // What the start said is not a frame's.
                    let _ = tail.lines();
                    *lock(&DIAG) = Some(tail);
                }
                *lock(&TIMELINE) = Some(Timeline::new(tx, epoch()));
                MODE.store(2, Ordering::Relaxed);
            },
            Err(error) => {
                eprintln!("lucinanet: frame log: cannot open the file: {error}; the frame log is off");
                MODE.store(1, Ordering::Relaxed);
            },
        },
    }
}

/// The file the process's stderr is, when it is a regular file (the owner redirected it): where the engine's and
/// WebRender's diagnostic lines can be read back from.
fn stderr_path() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link("/proc/self/fd/2").ok().filter(|p| p.is_absolute() && p.is_file())
    }
    #[cfg(target_os = "macos")]
    {
        let mut buf = [0 as libc::c_char; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH fills a buffer of PATH_MAX bytes, which `buf` is.
        let r = unsafe { libc::fcntl(2, libc::F_GETPATH, buf.as_mut_ptr()) };
        if r < 0 {
            return None;
        }
        // SAFETY: on success the buffer holds a NUL-terminated path.
        let text = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_str().ok()?.to_owned();
        let path = PathBuf::from(text);
        path.is_file().then_some(path)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// The record of one presented frame.
#[derive(Clone, Debug, Default)]
pub struct Record {
    pub t_ms: f64,
    pub n: u64,
    pub why: &'static str,
    pub input_age_ms: Option<f64>,
    pub interval_ms: Option<f64>,
    /// The script thread's turn for this frame. The engine does not expose it, so it is never filled in today.
    pub script_ms: Option<f64>,
    /// The bench's own frame: `paint`, `present`, `copy` and the stage columns.
    pub frame: Frame,
    pub win_upload_ms: Option<f64>,
    pub win_swap_ms: Option<f64>,
    pub win_draw_ms: Option<f64>,
    pub scale: f32,
    pub size: (u32, u32),
    pub refresh_hz: Option<f32>,
}

fn num(out: &mut String, key: &str, value: Option<f64>) {
    if let Some(v) = value.filter(|v| v.is_finite()) {
        let _ = write!(out, " {key}={v:.2}");
    }
}

/// The line of a record: `key=value` words, numbers and the `why` enum only.
pub fn format_line(r: &Record) -> String {
    let mut out = String::with_capacity(256);
    let _ = write!(out, "t={:.2} n={} why={}", r.t_ms, r.n, r.why);
    num(&mut out, "input_age", r.input_age_ms);
    num(&mut out, "interval", r.interval_ms);
    num(&mut out, "script", r.script_ms);
    let f = &r.frame;
    num(&mut out, "layout", f.layout_restyle_ms);
    num(&mut out, "dl", f.layout_list_ms);
    num(&mut out, "scene", f.wr_update_ms);
    num(&mut out, "build", f.wr_frame_build_ms);
    num(&mut out, "render", f.wr_render_ms);
    num(&mut out, "paint", Some(f.paint_ms));
    num(&mut out, "present", Some(f.present_ms));
    num(&mut out, "copy", Some(f.copy_ms));
    num(&mut out, "win_upload", r.win_upload_ms);
    num(&mut out, "win_swap", r.win_swap_ms);
    num(&mut out, "win_draw", r.win_draw_ms);
    if r.size.0 > 0 && r.size.1 > 0 {
        let _ = write!(out, " scale={:.2} size={}x{}", r.scale, r.size.0, r.size.1);
    }
    if let Some(hz) = r.refresh_hz {
        let _ = write!(out, " refresh_hz={hz:.1}");
    }
    out
}

/// The rendering reason as one of a few words: what `LUCINANET_RENDERING_REASON` prints is mapped, nothing else is kept.
pub fn classify_reason(said: &str) -> &'static str {
    let said = said.trim();
    if said.contains("input") {
        "input_events"
    } else if said.contains("scroll") {
        "scroll_events"
    } else if said.contains("animat") || said.contains("canvas") {
        "animation"
    } else if said.contains("restyle") || said.contains("display list") {
        "restyle"
    } else if said.contains("load") {
        "load"
    } else {
        "other"
    }
}

/// A frame the engine has presented, until the window (if any) has drawn it.
struct Pending {
    frame: Frame,
    why: &'static str,
    oldest_input_us: u64,
}

struct Timeline {
    tx: SyncSender<String>,
    start: Instant,
    n: u64,
    last_present: Option<Instant>,
    pending: Option<Pending>,
}

impl Timeline {
    fn new(tx: SyncSender<String>, start: Instant) -> Timeline {
        Timeline { tx, start, n: 0, last_present: None, pending: None }
    }

    /// Format and queue the line of `p`. `now` is the glass (or, offscreen, the engine's present); `now_us` is the
    /// same instant on the input clock. `win` is (draw, upload, swap) in ms.
    fn write(&mut self, p: Pending, win: Option<(f64, f64, f64)>, now: Instant, now_us: u64) {
        self.n += 1;
        let (w, h, scale, mhz) = (
            GEOM_W.load(Ordering::Relaxed),
            GEOM_H.load(Ordering::Relaxed),
            f32::from_bits(GEOM_SCALE.load(Ordering::Relaxed)),
            GEOM_REFRESH_MHZ.load(Ordering::Relaxed),
        );
        let record = Record {
            t_ms: now.saturating_duration_since(self.start).as_secs_f64() * 1000.0,
            n: self.n,
            why: p.why,
            input_age_ms: (p.oldest_input_us != 0).then(|| now_us.saturating_sub(p.oldest_input_us) as f64 / 1000.0),
            interval_ms: self.last_present.map(|was| now.saturating_duration_since(was).as_secs_f64() * 1000.0),
            script_ms: None,
            frame: p.frame,
            win_draw_ms: win.map(|w| w.0),
            win_upload_ms: win.map(|w| w.1),
            win_swap_ms: win.map(|w| w.2),
            scale,
            size: (w, h),
            refresh_hz: (mhz != 0).then(|| mhz as f32 / 1000.0),
        };
        self.last_present = Some(now);
        // A full queue drops the line: the engine and the window never wait for the disk.
        let _ = self.tx.try_send(format_line(&record));
    }
}

/// An input command reached the engine. Keeps the oldest one since the last presented frame.
pub fn command(cmd: &Cmd) {
    if !enabled() {
        return;
    }
    if matches!(cmd, Cmd::Wheel { .. } | Cmd::Key { .. } | Cmd::Move(..) | Cmd::Button { .. }) {
        let _ = OLDEST_INPUT_US.compare_exchange(0, now_us(), Ordering::Relaxed, Ordering::Relaxed);
    }
}

/// The window's (or the offscreen page's) geometry, for the `scale` and `size` columns.
pub fn set_geometry(width: u32, height: u32, scale: f32, refresh_millihertz: Option<u32>) {
    GEOM_W.store(width, Ordering::Relaxed);
    GEOM_H.store(height, Ordering::Relaxed);
    GEOM_SCALE.store(scale.to_bits(), Ordering::Relaxed);
    GEOM_REFRESH_MHZ.store(refresh_millihertz.unwrap_or(0), Ordering::Relaxed);
}

/// A window shows the pages: lines are written when it has drawn a frame, not when the engine presents.
pub fn window_opened(width: u32, height: u32, scale: f32, refresh_millihertz: Option<u32>) {
    WINDOWED.store(true, Ordering::Relaxed);
    set_geometry(width, height, scale, refresh_millihertz);
}

/// The engine presented a frame (`paint`, `present` and `copy` in microseconds, as `News::Painted` has them).
pub fn engine_frame(paint_us: u64, present_us: u64, copy_us: u64) {
    if !enabled() {
        return;
    }
    let oldest_input_us = OLDEST_INPUT_US.swap(0, Ordering::Relaxed);
    let mut frame = Frame {
        paint_ms: paint_us as f64 / 1000.0,
        present_ms: present_us as f64 / 1000.0,
        copy_ms: copy_us as f64 / 1000.0,
        ..Default::default()
    };
    let mut why = "unknown";
    if let Some(tail) = lock(&DIAG).as_mut() {
        for line in tail.lines() {
            if let Some(said) = line.strip_prefix("rendering update: ") {
                why = classify_reason(said);
            } else if let Some(parsed) = scrollbench::parse_line(&line) {
                scrollbench::fold(&mut frame, parsed);
            }
        }
    }
    let pending = Pending { frame, why, oldest_input_us };
    let (now, at_us) = (Instant::now(), now_us());
    if let Some(tl) = lock(&TIMELINE).as_mut() {
        if WINDOWED.load(Ordering::Relaxed) {
            // The engine got ahead of the window: the older frame was never drawn on its own.
            if let Some(older) = tl.pending.replace(pending) {
                tl.write(older, None, now, at_us);
            }
        } else {
            tl.write(pending, None, now, at_us);
        }
    }
}

/// The window drew and presented; the three steps took this long, in ms.
fn window_painted(draw_ms: f64, upload_ms: f64, swap_ms: f64) {
    let (now, at_us) = (Instant::now(), now_us());
    if let Some(tl) = lock(&TIMELINE).as_mut() {
        if let Some(p) = tl.pending.take() {
            tl.write(p, Some((draw_ms, upload_ms, swap_ms)), now, at_us);
        }
    }
}

/// The three steps of the window after the engine presented, timed with `Instant` pairs:
/// `drawn` = the toolkit recomposing the frame (`Core::paint`: chrome and page), `uploaded` = taking the
/// surface's buffer and copying the frame into it, `swapped` = `buffer.present()`.
pub struct WinSteps {
    at: Option<Instant>,
    draw_ms: f64,
    upload_ms: f64,
}

impl WinSteps {
    /// Starts the clock, if the log is on.
    pub fn begin() -> WinSteps {
        WinSteps { at: enabled().then(Instant::now), draw_ms: 0.0, upload_ms: 0.0 }
    }

    fn lap(&mut self) -> f64 {
        let Some(at) = self.at else { return 0.0 };
        let now = Instant::now();
        self.at = Some(now);
        now.saturating_duration_since(at).as_secs_f64() * 1000.0
    }

    pub fn drawn(&mut self) {
        self.draw_ms = self.lap();
    }

    pub fn uploaded(&mut self) {
        self.upload_ms = self.lap();
    }

    pub fn swapped(mut self) {
        if self.at.is_some() {
            let swap_ms = self.lap();
            window_painted(self.draw_ms, self.upload_ms, swap_ms);
        }
    }
}

/// The file with its one rotation.
pub struct Sink {
    path: PathBuf,
    out: std::io::BufWriter<std::fs::File>,
    written: u64,
    cap: u64,
    flushed: Instant,
}

impl Sink {
    pub fn open(path: &Path, cap: u64) -> std::io::Result<Sink> {
        let file = std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(path)?;
        Ok(Sink { path: path.to_path_buf(), out: std::io::BufWriter::new(file), written: 0, cap, flushed: Instant::now() })
    }

    pub fn line(&mut self, text: &str) {
        let need = text.len() as u64 + 1;
        if self.written > 0 && self.written + need > self.cap {
            self.rotate();
        }
        let _ = writeln!(self.out, "{text}");
        self.written += need;
        if self.flushed.elapsed() >= FLUSH_EVERY {
            self.flush();
        }
    }

    pub fn flush(&mut self) {
        let _ = self.out.flush();
        self.flushed = Instant::now();
    }

    /// The file becomes `<file>.1` (the one before it is replaced) and a new one starts.
    fn rotate(&mut self) {
        let _ = self.out.flush();
        let mut old = self.path.clone().into_os_string();
        old.push(".1");
        let _ = std::fs::rename(&self.path, &old);
        if let Ok(file) = std::fs::File::create(&self.path) {
            self.out = std::io::BufWriter::new(file);
        }
        self.written = 0;
    }
}

fn write_all(rx: Receiver<String>, mut sink: Sink) {
    loop {
        match rx.recv_timeout(FLUSH_EVERY) {
            Ok(text) => sink.line(&text),
            Err(RecvTimeoutError::Timeout) => sink.flush(),
            Err(RecvTimeoutError::Disconnected) => {
                sink.flush();
                return;
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder that is removed when it goes out of scope, a failed assertion included.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("lucinanet-frametimeline-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sample() -> Record {
        let mut frame = Frame { paint_ms: 4.5, present_ms: 0.7, copy_ms: 1.25, ..Default::default() };
        let layout = "layout-phases restyle+trees 1.20 ms, stacking contexts and display list 0.30 ms, ran RanLayout | BuiltDisplayList";
        let wr = "wr: update 2 render 3 ms, draws 12 color 1 alpha 0 frame_build 0.5 upload 0.000 MB 0.0 ms full_paint true";
        scrollbench::fold(&mut frame, scrollbench::parse_line(layout).unwrap());
        scrollbench::fold(&mut frame, scrollbench::parse_line(wr).unwrap());
        Record {
            t_ms: 1234.5,
            n: 7,
            why: "scroll_events",
            input_age_ms: Some(12.3),
            interval_ms: Some(16.7),
            frame,
            win_upload_ms: Some(0.9),
            win_swap_ms: Some(2.1),
            win_draw_ms: Some(3.0),
            scale: 2.0,
            size: (3480, 2180),
            refresh_hz: Some(60.0),
            ..Default::default()
        }
    }

    #[test]
    fn a_line_names_every_stage_and_leaves_out_what_is_unknown() {
        let line = format_line(&sample());
        for word in [
            "t=1234.50", "n=7", "why=scroll_events", "input_age=12.30", "interval=16.70", "layout=1.20", "dl=0.30", "scene=2.00",
            "build=0.50", "render=3.00", "paint=4.50", "present=0.70", "copy=1.25", "win_upload=0.90", "win_swap=2.10", "win_draw=3.00",
            "scale=2.00", "size=3480x2180", "refresh_hz=60.0",
        ] {
            assert!(line.split(' ').any(|w| w == word), "{word} in {line}");
        }
        assert!(!line.contains("script="), "the engine exposes no script time, so none is invented");
        assert!(!line.contains('\n'));
        // Nothing known: no input waiting, no window, no diagnostics.
        let bare = format_line(&Record { why: "unknown", ..Default::default() });
        assert_eq!(bare, "t=0.00 n=0 why=unknown paint=0.00 present=0.00 copy=0.00");
    }

    #[test]
    fn a_line_never_holds_a_url_or_a_path() {
        for r in [sample(), Record { why: classify_reason("https://example.com/a/b"), ..Default::default() }] {
            let line = format_line(&r);
            assert!(!line.contains("://"), "{line}");
            assert!(!line.contains('/'), "{line}");
            assert!(!line.contains('\\'), "{line}");
        }
        // Whatever the engine said, the reason is one of the fixed words.
        let said = ["", "/Users/x/secret", "https://a.example/", "input events", "scroll events", "restyle", "new display list", "animated image", "canvas", "media query"];
        for said in said {
            let why = classify_reason(said);
            assert!(["input_events", "scroll_events", "animation", "restyle", "load", "other"].contains(&why), "{said} -> {why}");
        }
        assert_eq!(classify_reason("input events"), "input_events");
        assert_eq!(classify_reason("scroll events"), "scroll_events");
    }

    #[test]
    fn unset_or_empty_is_off_and_a_hook_costs_nothing() {
        assert_eq!(decide(None, None), Decision::Off);
        assert_eq!(decide(Some(OsStr::new("")), None), Decision::Off);
        // No test in this binary configures the global log, so the hooks are on their one-load path.
        assert!(!enabled());
        command(&Cmd::Wheel { y: 1.0, x: 1.0, y_at: 1.0 });
        assert_eq!(OLDEST_INPUT_US.load(Ordering::Relaxed), 0);
        engine_frame(1, 2, 3);
        let steps = WinSteps::begin();
        assert!(steps.at.is_none());
        steps.swapped();
    }

    #[test]
    fn a_path_in_the_profile_directory_is_refused() {
        let scratch = Scratch::new("profile");
        let profile = scratch.0.join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let outside = scratch.0.join("frames.log");
        assert_eq!(decide(Some(profile.join("frames.log").as_os_str()), Some(&profile)), Decision::Refused);
        assert_eq!(decide(Some(profile.join("deeper").join("er").join("frames.log").as_os_str()), Some(&profile)), Decision::Refused);
        assert_eq!(decide(Some(profile.as_os_str()), Some(&profile)), Decision::Refused);
        assert_eq!(decide(Some(outside.as_os_str()), Some(&profile)), Decision::On(outside.clone()));
        // A sibling that only shares the prefix of the name is not inside.
        let sibling = scratch.0.join("profile-two.log");
        assert_eq!(decide(Some(sibling.as_os_str()), Some(&profile)), Decision::On(sibling));
        // No profile (in-memory): nothing to protect.
        assert_eq!(decide(Some(outside.as_os_str()), None), Decision::On(outside));
    }

    #[test]
    fn the_file_is_rolled_once_at_the_cap() {
        let scratch = Scratch::new("rotate");
        let path = scratch.0.join("frames.log");
        let mut sink = Sink::open(&path, 1000).unwrap();
        let line = "x".repeat(49);
        for _ in 0..60 {
            sink.line(&line);
        }
        sink.flush();
        let old = scratch.0.join("frames.log.1");
        let (now, before) = (std::fs::metadata(&path).unwrap().len(), std::fs::metadata(&old).unwrap().len());
        assert!(now <= 1000 && before <= 1000, "{now} {before}");
        assert!(now > 0 && before > 0);
        // Only the one rotation exists: there is no `.2`.
        assert!(!scratch.0.join("frames.log.2").exists());
    }

    #[test]
    fn the_time_runs_forward_and_the_interval_is_the_gap_between_presents() {
        let (tx, rx) = sync_channel(16);
        let start = Instant::now();
        let mut tl = Timeline::new(tx, start);
        let pending = || Pending { frame: Frame::default(), why: "unknown", oldest_input_us: 0 };
        for ms in [10u64, 26, 60] {
            tl.write(pending(), None, start + Duration::from_millis(ms), 1);
        }
        let lines: Vec<String> = rx.try_iter().collect();
        assert_eq!(lines.len(), 3);
        let field = |line: &str, key: &str| -> Option<f64> { line.split(' ').find_map(|w| w.strip_prefix(key)?.strip_prefix('=')?.parse().ok()) };
        let ts: Vec<f64> = lines.iter().map(|l| field(l, "t").unwrap()).collect();
        assert_eq!(ts, vec![10.0, 26.0, 60.0]);
        assert_eq!(field(&lines[0], "interval"), None, "no frame before the first");
        assert_eq!(field(&lines[1], "interval"), Some(16.0));
        assert_eq!(field(&lines[2], "interval"), Some(34.0));
        assert_eq!(lines.iter().map(|l| field(l, "n").unwrap()).collect::<Vec<_>>(), vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn the_age_of_the_oldest_input_is_measured_to_the_present() {
        let (tx, rx) = sync_channel(4);
        let start = Instant::now();
        let mut tl = Timeline::new(tx, start);
        let p = Pending { frame: Frame::default(), why: "input_events", oldest_input_us: 1_000 };
        tl.write(p, Some((1.0, 2.0, 3.0)), start + Duration::from_millis(40), 13_500);
        let line = rx.try_recv().unwrap();
        assert!(line.contains("input_age=12.50"), "{line}");
        for word in ["win_draw=1.00", "win_upload=2.00", "win_swap=3.00"] {
            assert!(line.contains(word), "{word} in {line}");
        }
    }

    #[test]
    fn a_full_queue_drops_the_line_and_never_blocks() {
        let (tx, _rx) = sync_channel(1);
        let mut tl = Timeline::new(tx, Instant::now());
        for _ in 0..5 {
            tl.write(Pending { frame: Frame::default(), why: "unknown", oldest_input_us: 0 }, None, Instant::now(), 1);
        }
        assert_eq!(tl.n, 5);
    }
}
