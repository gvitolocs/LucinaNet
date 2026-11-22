//! LucinaNet's window and the seam under it: the bar and the input live here, and an engine (Servo, Blitz)
//! is a thread behind [`Engine`] that takes [`Cmd`]s, reports [`News`] and leaves frames in [`Page`].
//! An engine's binary calls [`run`] with its `start`.

pub mod address;
pub mod browser;
pub mod chrome;
#[cfg(feature = "desktop")]
pub mod desktop;
pub mod fonts;
pub mod frametimeline;
pub mod glass;
pub mod keymap;
pub mod onebar;
pub mod platform;
pub mod scrollbench;
pub mod shot;
pub mod tabs;
pub mod title;

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub use keyboard_types::{Key, Modifiers, NamedKey};
pub use url::Url;

/// The page's pixels, `0x00RRGGBB`, top row first.
#[derive(Default)]
pub struct Page {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
}

/// One press of Ctrl +, Ctrl - or Ctrl 0 on the shown tab (`lucinanet-zoom` holds the levels and the rules).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoomStep {
    In,
    Out,
    /// Back to the default level, and the site forgotten (not "100": the default is a setting).
    Reset,
}

#[derive(Debug)]
pub enum Cmd {
    Load(Url),
    /// A load that began in the address bar. `search` when the typed text became a search. The engine records the visit as
    /// typed (`Load` is a link, a script or the first page).
    LoadTyped { url: Url, search: bool },
    /// The pages and bookmarks the address bar may suggest for this text; the engine answers with [`News::HistoryHits`] and
    /// [`News::BookmarkHits`], both with the same id.
    HistoryQuery { id: u64, text: String },
    Back,
    Forward,
    Reload,
    Move(f32, f32),
    Button { down: bool, x: f32, y: f32 },
    /// Pixels, positive scrolls towards the top of the page.
    Wheel { y: f64, x: f32, y_at: f32 },
    /// A key went down and up.
    Key { key: Key, modifiers: Modifiers },
    /// Write the page's title, text and element boxes as JSON (`dump.js`), then report [`News::Dumped`].
    Dump(std::path::PathBuf),
    /// A tab with this address, or `about:blank`, shown at once.
    NewTab(Option<Url>),
    SelectTab(u32),
    CloseTab(u32),
    /// The person's answer to the [`Control`] with this id.
    Answer(u64, Answer),
    /// Stop a running download (by its [`News::Download`] id); the engine deletes what it wrote.
    CancelDownload(u64),
    /// One zoom step on the shown tab. The engine keeps the level per site and answers with [`News::Zoom`].
    Zoom(ZoomStep),
    /// The star: bookmark the shown page, or remove its bookmarks if it has any. The engine answers with
    /// [`News::Bookmarked`] (and a [`News::Note`] when it refused).
    ToggleBookmark,
    /// The local history for the history panel (Ctrl+H): the pages last visited, newest first, at most `limit` of them and
    /// never more than [`HISTORY_LIST_MAX`]. The engine answers with [`News::HistoryList`] and the same id; it reads the
    /// local store only and sends no request.
    ListHistory { id: u64, limit: usize },
    /// The local bookmarks for the bookmarks panel (Ctrl+Shift+O, Ctrl+B): the bookmarks last added first, at most `limit` of
    /// them and never more than [`BOOKMARK_LIST_MAX`]. The engine answers with [`News::BookmarkList`] and the same id; it
    /// reads the local store only and sends no request.
    ListBookmarks { id: u64, limit: usize },
    /// The find bar (Ctrl+F) wants the shown page's text: the engine asks the page for it (a walk of its own, no script runs
    /// in the page) and answers with [`News::PageText`] and the same id. The text goes to the find bar only.
    PageText { id: u64 },
}

/// The most bookmarks one [`Cmd::ListBookmarks`] is answered with, cut before the answer is built.
pub const BOOKMARK_LIST_MAX: usize = 200;

/// The most pages one [`Cmd::ListHistory`] is answered with: a panel shows a few at a time and the store holds up to twenty
/// thousand, so the answer is cut before it is built.
pub const HISTORY_LIST_MAX: usize = 200;

#[derive(Debug)]
pub enum News {
    Frame,
    /// A frame this engine painted, with what its three parts took in microseconds: `paint` (WebRender's
    /// update and render), `present` (the rasterizer flushing what the frame drew) and `copy` (the finished
    /// frame into [`Page`]). Sent just before the [`News::Frame`] of the same frame, for
    /// [`crate::scrollbench`]; a window that shows the page on a screen ignores it.
    Painted { paint_us: u64, present_us: u64, copy_us: u64 },
    Url(String),
    Title(String),
    Loading(bool),
    History { back: bool, forward: bool },
    Crashed(String),
    Dumped,
    /// A navigation is being saved as a file instead of shown. Sent when it starts, as it goes
    /// and when it ends; `id` is the engine's, for [`Cmd::CancelDownload`].
    Download {
        id: u64,
        name: String,
        received: u64,
        total: Option<u64>,
        state: DownloadState,
    },
    /// The tabs in strip order and the one shown, whenever one opens, closes, is chosen or changes its title,
    /// address or loading. The other news above is the shown tab's only.
    Tabs { tabs: Vec<TabInfo>, active: u32 },
    /// What the shown tab asks of the person, drawn by the window over the page; `None` when it no longer
    /// asks anything. Sent again whenever another tab is shown, so a background tab's dialog waits for it.
    Control(Option<Control>),
    /// A short line for the bar, such as why a tab was not opened.
    Note(String),
    /// The shown tab's zoom in whole percent, sent whenever it changes or another tab is shown. 100 is no zoom.
    Zoom(u16),
    /// The answer to [`Cmd::HistoryQuery`]: visited pages that match the text, best first. Empty without a history.
    HistoryHits { id: u64, entries: Vec<lucinanet_omnibox::HistoryEntry> },
    /// The other half of the answer to [`Cmd::HistoryQuery`], with the same id: the bookmarks that match the text.
    BookmarkHits { id: u64, entries: Vec<lucinanet_omnibox::BookmarkEntry> },
    /// Whether the shown tab's page is bookmarked, sent whenever its address changes, another tab is shown or the star was
    /// pressed.
    Bookmarked(bool),
    /// The answer to [`Cmd::ListHistory`]: the visited pages by last visit, newest first, with the title (possibly empty),
    /// the address and the time of the last visit in microseconds since the Unix epoch. Empty without a history.
    HistoryList { id: u64, entries: Vec<lucinanet_omnibox::HistoryEntry> },
    /// The answer to [`Cmd::ListBookmarks`]: the bookmarks, the last added first, with the title (possibly empty) and the
    /// address. Not [`News::BookmarkHits`]: that one is the address bar's and never carries a list for the panel.
    BookmarkList { id: u64, entries: Vec<lucinanet_omnibox::BookmarkEntry> },
    /// The answer to [`Cmd::PageText`]: the shown page's text as blocks of runs, for the find bar and nothing else. `partial`
    /// when the engine's byte limit cut the text short. Empty when the page went away before it was walked. The text is
    /// never logged, never kept but by the open find bar, and dropped with it.
    PageText { id: u64, blocks: Vec<lucinanet_find::Block>, partial: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabInfo {
    pub id: u32,
    pub title: String,
    pub url: String,
    pub loading: bool,
    /// The address host's registrable domain (eTLD+1), as the engine's Public Suffix List answers for it: what the field
    /// draws in its strong ink inside the host. `None` is an answer, not a failure - `localhost`, a single label, an IP
    /// literal, a bare suffix, and no host at all - and the field then emphasises the whole host. Computed by the engine
    /// (`lucinanet-servo`'s `domains`), which is where the list lives; the UI has no copy of it.
    pub registrable_domain: Option<String>,
}

/// What the address field's mark says about the page it is on, and nothing else: it is not a claim about the page's
/// content, only about how it arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    /// The lock: the address came over a scheme that is encrypted on the wire.
    Secure,
    /// The warning: it did not.
    NotSecure,
}

/// The shown address's [`Security`].
///
/// The scheme is the only input today. The engine reports nothing about the certificate: no [`News`] variant carries one
/// (`News::Crashed` is a dead tab, `News::Note` a line about a refused action), so an `https` page whose chain does not
/// verify still shows the lock. When the engine does say, this is the one function that changes and the chromes do not.
pub fn security_of(url: &str) -> Security {
    // Read the scheme by hand rather than parsing: `Url` refuses what has none, and everything that is not an encrypted
    // scheme is not secure, so a failure to parse is an answer rather than a question.
    let head = url.trim_start().split([':', '/', '?', '#']).next().unwrap_or("");
    if head.eq_ignore_ascii_case("https") || head.eq_ignore_ascii_case("wss") { Security::Secure } else { Security::NotSecure }
}

/// A question from the page that the window answers with [`Cmd::Answer`] and the same id.
#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    /// `alert`, `confirm` or `prompt`. `origin` is the page's, shown as the title: the message is the page's
    /// own words and must not pass for the browser's.
    Dialog {
        id: u64,
        origin: String,
        message: String,
        kind: DialogKind,
    },
    /// A `<select>`'s list, under the element at `at` (x, y, width, height in the page's pixels).
    Select {
        id: u64,
        at: (i32, i32, i32, i32),
        items: Vec<SelectItem>,
        /// The option ids chosen now.
        selected: Vec<usize>,
    },
}

impl Control {
    pub fn id(&self) -> u64 {
        match self {
            Control::Dialog { id, .. } | Control::Select { id, .. } => *id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogKind {
    Alert,
    Confirm,
    /// With the text the field starts with.
    Prompt(String),
}

/// A row of a `<select>`'s list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectItem {
    /// An `<optgroup>`'s label: shown, never chosen.
    Group(String),
    Option { id: usize, label: String, disabled: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// OK, with the prompt's text.
    Ok(Option<String>),
    Cancel,
    /// The option id chosen in a `<select>`.
    Chose(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub enum DownloadState {
    Running,
    /// Complete, at this path.
    Done(std::path::PathBuf),
    /// Failed or cancelled, and why; nothing is left on disk.
    Failed(String),
}

/// The parity harness's questions, for engines with JavaScript.
pub const DUMP_JS: &str = include_str!("dump.js");

/// How the window reaches its engine.
pub trait Engine {
    fn send(&self, cmd: Cmd);
}

/// What an engine starts from.
pub struct Starting {
    pub width: u32,
    pub height: u32,
    /// Device pixels per CSS pixel: the toolkit's own scale.
    pub scale: f32,
    pub first: Url,
    pub page: Arc<Mutex<Page>>,
    pub news: Box<dyn Fn(News) + Send>,
}

pub type Start = fn(Starting) -> Box<dyn Engine>;

/// Wakes an engine's loop from any thread.
#[derive(Clone, Default)]
pub struct Waker(Arc<(Mutex<bool>, Condvar)>);

impl Waker {
    pub fn wait(&self, timeout: Duration) {
        let (lock, cond) = &*self.0;
        let mut woken = lock.lock().unwrap();
        if !*woken {
            woken = cond.wait_timeout(woken, timeout).unwrap().0;
        }
        *woken = false;
    }

    pub fn wake(&self) {
        let (lock, cond) = &*self.0;
        *lock.lock().unwrap() = true;
        cond.notify_all();
    }
}

/// The quiet window and the longest wait, in milliseconds, from the two settings (None = unset): a quiet window of 0
/// keeps the old fixed 800 ms, and the longest wait is never shorter than the quiet window.
#[cfg(feature = "harness")]
fn settle_windows(quiet: Option<u64>, max: Option<u64>) -> (u64, u64) {
    let quiet = quiet.unwrap_or(1500);
    if quiet == 0 {
        return (800, 800);
    }
    (quiet, max.unwrap_or(12_000).max(quiet))
}

/// Whether a loaded page has settled: no frame for `quiet` ms, or `max` ms since load.
#[cfg(feature = "harness")]
fn settled(since_last_frame_ms: u64, since_load_ms: u64, quiet: u64, max: u64) -> bool {
    since_last_frame_ms >= quiet || since_load_ms >= max
}

/// Load a page without a screen, write a PNG and, if asked, the dump. Timings go to stderr.
///
/// `LUCINANET_SCALE` is the device pixels per CSS pixel: a window on a screen gets that from the toolkit, and
/// an offscreen run has no toolkit to ask, so the benchmark says (1.0 when unset, as it always was).
#[cfg(feature = "harness")]
pub fn render(start: Start, url: &str, out: &str, width: u32, height: u32, dump: Option<&str>) {
    let started = std::time::Instant::now();
    let scale = scrollbench::parse_scale(std::env::var("LUCINANET_SCALE").ok().as_deref());
    frametimeline::set_geometry(width, height, scale, None);
    let page = Arc::new(Mutex::new(Page::default()));
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = Mutex::new(tx);
    // Running downloads, kept here because the waits below drop the news they do not need.
    let downloads = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let running = downloads.clone();
    // `LUCINANET_CHROME_SHOT=<png>`: also write the window with its bars (shot.rs), from the news the engine sends.
    let shot_out = std::env::var("LUCINANET_CHROME_SHOT").ok();
    let shot = shot_out.is_some();
    let recorded_news = Arc::new(Mutex::new(Vec::new()));
    let recorded = recorded_news.clone();
    let engine = start(Starting {
        width,
        height,
        scale,
        first: Url::parse(url).expect("a valid URL"),
        page: page.clone(),
        news: Box::new(move |news| {
            if let News::Download { id, name, state, .. } = &news {
                let mut running = running.lock().unwrap();
                match state {
                    DownloadState::Running => {
                        if running.insert(*id) {
                            eprintln!("render: download {name} started");
                        }
                    },
                    DownloadState::Done(path) => {
                        running.remove(id);
                        eprintln!("render: download {name} saved to {}", path.display());
                    },
                    DownloadState::Failed(reason) => {
                        running.remove(id);
                        eprintln!("render: download {name} failed: {reason}");
                    },
                }
            }
            if shot {
                if let Some(r) = shot::record(&news) {
                    recorded.lock().unwrap().push(r);
                }
            }
            let _ = tx.lock().unwrap().send(news);
        }),
    });
    // `LUCINANET_LOADS=<file>` with a `timing` line: the first load is timed too and the fixed settle below is skipped.
    let loads_text = std::env::var_os("LUCINANET_LOADS").and_then(|path| std::fs::read_to_string(path).ok());
    let timing_mode = loads_text.as_deref().is_some_and(|t| t.lines().any(|l| l.trim() == "timing"));
    let deadline = started + Duration::from_secs(120);
    let mut done = false;
    let (mut first_frame, mut first_complete) = (None, None);
    while !done && std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(if timing_mode { 5 } else { 200 })) {
            Ok(News::Frame) if first_frame.is_none() => first_frame = Some(started.elapsed()),
            Ok(News::Loading(false)) => {
                done = true;
                first_complete = Some(started.elapsed());
            },
            _ => {},
        }
    }
    if timing_mode {
        // A frame that came after the signal is still the first frame; a page with none in a second has none to report.
        let limit = std::time::Instant::now() + Duration::from_millis(1000);
        while first_frame.is_none() && std::time::Instant::now() < limit {
            if let Ok(News::Frame) = rx.recv_timeout(Duration::from_millis(5)) {
                first_frame = Some(started.elapsed());
            }
        }
        step_line(0, "load", url, None, first_frame, first_complete, !done, "cold");
        quiet(&rx, Duration::from_millis(500), Duration::from_secs(5));
    } else {
        // What a finished page still paints. A single-page app fills in icons, rows and images after `load` (YouTube's
        // icons arrive 4-6 s after it), so the snapshot waits until the page has painted no new frame for a quiet window,
        // and at most `max_ms` after load. `LUCINANET_RENDER_QUIET_MS` (default 1500) and `LUCINANET_RENDER_SETTLE_MAX_MS`
        // (default 12000); a quiet window of 0 is the old fixed 800 ms.
        let (quiet_ms, max_ms) = settle_windows(
            std::env::var("LUCINANET_RENDER_QUIET_MS").ok().and_then(|s| s.parse().ok()),
            std::env::var("LUCINANET_RENDER_SETTLE_MAX_MS").ok().and_then(|s| s.parse().ok()),
        );
        let loaded = std::time::Instant::now();
        let mut last_frame = loaded;
        let mut frames = 0u32;
        loop {
            if let Ok(News::Frame) = rx.recv_timeout(Duration::from_millis(100)) {
                last_frame = std::time::Instant::now();
                frames += 1;
            }
            let now = std::time::Instant::now();
            if settled(
                now.duration_since(last_frame).as_millis() as u64,
                now.duration_since(loaded).as_millis() as u64,
                quiet_ms,
                max_ms,
            ) {
                break;
            }
        }
        eprintln!(
            "render: settled {} ms after load ({frames} frames, quiet {quiet_ms} ms, at most {max_ms} ms)",
            loaded.elapsed().as_millis()
        );
    }
    eprintln!(
        "render: complete {done} in {} ms, rss {} KiB",
        started.elapsed().as_millis(),
        rss_kib()
    );
    // `LUCINANET_RENDER_HOLD=s`: keep the engine running s seconds more before the hard exit, as a browser that is
    // used for a while and then killed: what the profile keeps across that is what a restart finds.
    if let Some(hold) = std::env::var("LUCINANET_RENDER_HOLD").ok().and_then(|s| s.parse::<u64>().ok()) {
        let until = std::time::Instant::now() + Duration::from_secs(hold);
        while std::time::Instant::now() < until {
            let _ = rx.recv_timeout(Duration::from_millis(200));
        }
        eprintln!("render: held {hold} s");
    }
    // `LUCINANET_SCROLL_TEST=n`: n wheel steps of `LUCINANET_SCROLL_PX` down and back up, each timed to the
    // frame it paints. Going back up repaints nothing WebRender still has.
    let step: f64 = std::env::var("LUCINANET_SCROLL_PX").ok().and_then(|n| n.parse().ok()).unwrap_or(285.0);
    let scrolls: u32 = std::env::var("LUCINANET_SCROLL_TEST").ok().and_then(|n| n.parse().ok()).unwrap_or(0);
    for i in 0..scrolls * 2 {
        while rx.try_recv().is_ok() {}
        let sent = std::time::Instant::now();
        engine.send(Cmd::Wheel {
            y: if i < scrolls { -step } else { step },
            x: 200.0,
            y_at: 200.0,
        });
        let limit = sent + Duration::from_secs(2);
        while std::time::Instant::now() < limit {
            if let Ok(News::Frame) = rx.recv_timeout(Duration::from_millis(50)) {
                eprintln!("render: {} to frame {} us", if i < scrolls { "down" } else { "up" }, sent.elapsed().as_micros());
                break;
            }
        }
        // Wheel steps as a hand sends them, inside the time animations hold for input.
        std::thread::sleep(Duration::from_millis(100));
    }
    // `LUCINANET_SCROLL_BENCH=<steps>x<delta CSS px>`: the same scrolling, measured frame by frame.
    let bench_spec = std::env::var("LUCINANET_SCROLL_BENCH").ok();
    if let Some((steps, delta)) = scrollbench::parse_spec(bench_spec.as_deref()) {
        scroll_bench(
            engine.as_ref(),
            &rx,
            scrollbench::Meta {
                url: url.to_string(),
                steps: steps as usize,
                delta_css_px: delta,
                scale,
                width,
                height,
                paced: std::env::var("LUCINANET_SCROLL_BENCH_PACE").ok().as_deref() != Some("0"),
                raster: {
                    let raster = std::env::var("LUCINANET_RASTER").unwrap_or_default();
                    if raster.trim().is_empty() { "rust".to_string() } else { raster }
                },
                raster_threads: std::env::var("LUCINANET_RASTER_THREADS").unwrap_or_else(|_| "default".to_string()),
                layout_diag: std::env::var_os("LUCINANET_LAYOUT_PHASES").is_some(),
                log: Some(std::path::PathBuf::from(
                    std::env::var("LUCINANET_SCROLL_BENCH_LOG").unwrap_or_else(|_| "/dev/stderr".to_string()),
                )),
            },
        );
    } else if let Some(raw) = bench_spec.as_deref().filter(|s| !s.trim().is_empty()) {
        // A set-but-unreadable spec has to say so: the run otherwise finishes looking like it measured something.
        eprintln!("scroll-bench: LUCINANET_SCROLL_BENCH={raw} is not <steps> or <steps>x<delta CSS px>, so nothing was measured");
    }
    // `LUCINANET_LOADS=<file>`: a line each, in turn, with the memory it leaves: a URL, `back`, `forward`, `click <x> <y>`
    // (a press and release of the left button), `typed <url>` (a load that counts as typed in the history), `newtab <url>` (a tab with that address, shown at once), `bookmark` (the star on the shown page: adds it, or removes it if it is bookmarked), `pagetext` (the find bar's page text, printed as counts), `wait <ms>`. A line `timing` turns on the timing output: per step one line
    // `timing step=N cmd=.. arg=.. phase=cold|warm url_ms=.. frame_ms=.. complete_ms=.. loading=0|1 timed_out=0|1 rss_mib=..
    // load1=..` (ms from the command to the first URL news, the first frame and the load-complete signal; `-` where it
    // never came, as for a Back from the cache, which sends no Loading signal), and no fixed settle after a step.
    if let Some(list) = loads_text {
        let mut step = 0;
        for line in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let mut words = line.split_whitespace();
            let cmd = words.next().unwrap_or_default();
            let (name, sent_cmd): (&str, Vec<Cmd>) = match cmd {
                "timing" => continue,
                "wait" => {
                    std::thread::sleep(Duration::from_millis(words.next().and_then(|n| n.parse().ok()).unwrap_or(0)));
                    continue;
                },
                "back" => ("back", vec![Cmd::Back]),
                "forward" => ("forward", vec![Cmd::Forward]),
                // `typed <url>`: a load as if it came from the address bar (the visit is recorded as typed).
                "typed" => match words.next().map(Url::parse) {
                    Some(Ok(next)) => {
                        phase(&format!("load sent {line}"));
                        ("load", vec![Cmd::LoadTyped { url: next, search: false }])
                    },
                    _ => continue,
                },
                // `bookmark`: the star on the shown page; waits for the engine's answer (it always gives one).
                "bookmark" => {
                    step += 1;
                    engine.send(Cmd::ToggleBookmark);
                    let limit = std::time::Instant::now() + Duration::from_secs(5);
                    let mut on = None;
                    while on.is_none() && std::time::Instant::now() < limit {
                        if let Ok(News::Bookmarked(now)) = rx.recv_timeout(Duration::from_millis(50)) {
                            on = Some(now);
                        }
                    }
                    eprintln!("render: bookmark {step} on {on:?}");
                    continue;
                },
                // `pagetext`: the same request Ctrl+F makes; waits for the answer and prints its counts, never its text
                // (`render: pagetext blocks=N runs=M partial=B`). For `tests/web/page-text-unobserved.sh`.
                "pagetext" => {
                    step += 1;
                    engine.send(Cmd::PageText { id: step as u64 });
                    let limit = std::time::Instant::now() + Duration::from_secs(5);
                    let mut counts = None;
                    while counts.is_none() && std::time::Instant::now() < limit {
                        if let Ok(News::PageText { blocks, partial, .. }) = rx.recv_timeout(Duration::from_millis(50)) {
                            counts = Some((blocks.len(), blocks.iter().map(|b| b.runs.len()).sum::<usize>(), partial));
                        }
                    }
                    match counts {
                        Some((blocks, runs, partial)) => eprintln!("render: pagetext blocks={blocks} runs={runs} partial={partial}"),
                        None => eprintln!("render: pagetext no answer"),
                    }
                    continue;
                },
                "newtab" => match words.next().map(Url::parse) {
                    Some(Ok(next)) => ("newtab", vec![Cmd::NewTab(Some(next))]),
                    _ => continue,
                },
                "click" => {
                    let (Some(x), Some(y)) = (
                        words.next().and_then(|n| n.parse::<f32>().ok()),
                        words.next().and_then(|n| n.parse::<f32>().ok()),
                    ) else {
                        continue;
                    };
                    ("click", vec![Cmd::Move(x, y), Cmd::Button { down: true, x, y }, Cmd::Button { down: false, x, y }])
                },
                _ => match Url::parse(line) {
                    Ok(next) => {
                        phase(&format!("load sent {line}"));
                        ("load", vec![Cmd::Load(next)])
                    },
                    Err(_) => continue,
                },
            };
            step += 1;
            if timing_mode {
                quiet(&rx, Duration::from_millis(500), Duration::from_secs(5));
            }
            let sent = std::time::Instant::now();
            for cmd in sent_cmd {
                engine.send(cmd);
            }
            let seen = watch_step(&rx, sent, Duration::from_secs(40), name == "load" || name == "newtab");
            if timing_mode {
                step_line(step, name, line, seen.url, seen.frame, seen.complete, seen.timed_out, "warm");
                continue;
            }
            let done = sent.elapsed().as_millis();
            std::thread::sleep(Duration::from_millis(1000));
            while rx.try_recv().is_ok() {}
            eprintln!(
                "render: load {step} {} {done} ms rss {} MiB {line}",
                seen.complete.is_some(),
                rss_kib() / 1024
            );
        }
    }
    // `LUCINANET_IDLE_TEST=<seconds>`: frames and CPU per thread while nothing is asked of the page.
    if let Some(seconds) = std::env::var("LUCINANET_IDLE_TEST").ok().and_then(|n| n.parse::<u64>().ok()) {
        while rx.try_recv().is_ok() {}
        let before = thread_ticks();
        let until = std::time::Instant::now() + Duration::from_secs(seconds);
        let mut frames = 0;
        while std::time::Instant::now() < until {
            if let Ok(News::Frame) = rx.recv_timeout(Duration::from_millis(100)) {
                frames += 1;
            }
        }
        let mut used: Vec<(u64, String)> = thread_ticks()
            .into_iter()
            .map(|(name, ticks)| (ticks - before.get(&name).copied().unwrap_or(0), name))
            .filter(|(ticks, _)| *ticks > 0)
            .collect();
        used.sort();
        used.reverse();
        used.truncate(8);
        eprintln!("render: idle {seconds} s: {frames} frames, ticks {used:?}");
    }
    // The process ends with a hard exit, which would leave a running download as a `.part`.
    let limit = std::time::Instant::now() + Duration::from_secs(120);
    while !downloads.lock().unwrap().is_empty() && std::time::Instant::now() < limit {
        let _ = rx.recv_timeout(Duration::from_millis(100));
    }
    if let Some(dump) = dump {
        engine.send(Cmd::Dump(dump.into()));
        let limit = std::time::Instant::now() + Duration::from_secs(20);
        while std::time::Instant::now() < limit {
            if let Ok(News::Dumped) = rx.recv_timeout(Duration::from_millis(200)) {
                break;
            }
        }
    }
    let page = page.lock().unwrap();
    if let Some(out) = &shot_out
        && page.w > 0
    {
        shot::write(url, width, height, &page, std::mem::take(&mut *recorded_news.lock().unwrap()), out);
    }
    if page.w == 0 || page.h == 0 {
        // A navigation that became a download paints no page, so there is no PNG to write.
        eprintln!("render: no page was painted, no PNG written");
        return;
    }
    let mut rgba = Vec::with_capacity(page.px.len() * 4);
    for px in &page.px {
        rgba.extend_from_slice(&[(px >> 16) as u8, (px >> 8) as u8, *px as u8, 255]);
    }
    let file = std::fs::File::create(out).expect("create the PNG");
    let mut encoder = png::Encoder::new(file, page.w, page.h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("a PNG header");
    writer.write_image_data(&rgba).expect("write the PNG");
}

/// The scroll benchmark: [`scrollbench`] says what the numbers mean.
///
/// A step goes through [`Cmd::Wheel`], the command the window's own wheel sends, so script sees a wheel event
/// first, the engine may swallow it, and the page moves in the scroll tree: what is measured is a user's
/// scroll, not `window.scrollBy` called from script.
///
/// The steps are paced at the nominal 60 Hz, which is what a hand asks of a page. With
/// `LUCINANET_SCROLL_BENCH_PACE=0` the next step goes as soon as a frame has been presented, which is the same
/// scroll at the machine's own pace: the run then says how fast the engine can go, not how well it holds 60 Hz.
#[cfg(feature = "harness")]
fn scroll_bench(engine: &dyn Engine, rx: &std::sync::mpsc::Receiver<News>, meta: scrollbench::Meta) {
    use scrollbench::{Frame, Line, NOMINAL_MS};
    // The wheel counts device pixels; the page moves by that many over the scale, in CSS pixels. The point is
    // the middle of the window, where a hand holds a page: a corner would hit a scroll chain of its own.
    let wheel = |down: bool| Cmd::Wheel {
        y: if down { -meta.delta_css_px } else { meta.delta_css_px } * meta.scale as f64,
        x: meta.width as f32 / 2.0,
        y_at: meta.height as f32 / 2.0,
    };
    let mut tail = match &meta.log {
        Some(path) => scrollbench::Tail::new(path),
        None => scrollbench::Tail::new(std::path::Path::new("/dev/null")),
    };
    let mut frames: Vec<Frame> = Vec::new();
    // The diagnostics said between two frames, and the swgl windows, which are the run's and not a frame's.
    let mut windows: Vec<Vec<Line>> = Vec::new();
    let mut window: Vec<Line> = Vec::new();
    let mut swgl: Vec<(f64, f64)> = Vec::new();
    // The parts of the frame being presented, from the news that comes just before it.
    let mut painted = None;
    let mut last: Option<std::time::Instant> = None;
    // The newest step no frame has shown yet, and the count of steps a frame never showed: their offsets were
    // replaced by a later step before the engine got to them, which is what a slow page looks like to a hand.
    let mut waiting: Option<(usize, std::time::Instant)> = None;
    let mut without = 0usize;
    // What the load painted is not part of the run.
    while rx.try_recv().is_ok() {}
    let _ = tail.lines();
    let began = std::time::Instant::now();
    let tick = |step: usize| Duration::from_secs_f64(step as f64 * NOMINAL_MS / 1000.0);
    // The steps go out on their ticks until the run has `steps` frames to report, so a slow engine is measured
    // over the frames it actually presented rather than over the two the first second of ticks happened to
    // finish. The caps are the wall clock and, more usefully, silence: a page that has run out stops damaging
    // the window, and there is no point waiting for a frame that is not coming.
    let budget = tick(meta.steps) * 10 + Duration::from_secs(30);
    let mut sent = 0usize;
    loop {
        let now = std::time::Instant::now();
        // Paced: the next step at its tick, whether or not the page answered the last one, which is what a hand
        // does to a page that is too slow. Unpaced: the next step only once a frame has shown the last one.
        let due = frames.len() < meta.steps
            && if meta.paced { now >= began + tick(sent) } else { waiting.is_none() };
        if due {
            if waiting.take().is_some() {
                without += 1;
            }
            engine.send(wheel(true));
            waiting = Some((sent, now));
            sent += 1;
        }
        // Up to the next step, but never asleep for long: a presented frame is read the moment it comes.
        let slice = if meta.paced && frames.len() < meta.steps {
            (began + tick(sent)).saturating_duration_since(std::time::Instant::now()).clamp(Duration::from_millis(1), Duration::from_millis(5))
        } else {
            Duration::from_millis(2)
        };
        match rx.recv_timeout(slice) {
            Ok(News::Painted { paint_us, present_us, copy_us }) => {
                painted = Some((paint_us, present_us, copy_us));
            },
            Ok(News::Frame) => {
                let at = std::time::Instant::now();
                // WebRender says the stages of a frame inside the paint that ends with it, so read them here
                // and not later: they belong to this frame.
                for line in tail.lines() {
                    match scrollbench::parse_line(&line) {
                        Some(Line::SwglWindow { window_ms, swgl_ms }) => swgl.push((window_ms, swgl_ms)),
                        Some(line) => window.push(line),
                        None => {},
                    }
                }
                let (step, since_step) = match waiting.take() {
                    Some((step, when)) => (Some(step), Some(when.elapsed().as_secs_f64() * 1000.0)),
                    None => (None, None),
                };
                let (paint, present, copy) = painted.take().unwrap_or((0, 0, 0));
                frames.push(Frame {
                    step,
                    interval_ms: last.map(|was| at.duration_since(was).as_secs_f64() * 1000.0),
                    since_step_ms: since_step,
                    paint_ms: paint as f64 / 1000.0,
                    present_ms: present as f64 / 1000.0,
                    copy_ms: copy as f64 / 1000.0,
                    ..Default::default()
                });
                windows.push(std::mem::take(&mut window));
                last = Some(at);
            },
            _ => {},
        }
        let since_frame = last.map(|at| at.elapsed()).unwrap_or_else(|| began.elapsed());
        if frames.len() >= meta.steps && since_frame >= Duration::from_millis(300) {
            break;
        }
        if since_frame >= scrollbench::STEP_TIMEOUT {
            eprintln!("scroll-bench: {} frames, no frame for {since_frame:?}: the page stopped repainting", frames.len());
            break;
        }
        if began.elapsed() > budget {
            eprintln!("scroll-bench: {} frames after {sent} steps and {budget:?}: the run is out of time", frames.len());
            break;
        }
    }
    if waiting.take().is_some() {
        without += 1;
    }
    scrollbench::attribute(&mut frames, windows);
    use std::io::Write;
    print!("{}", scrollbench::table(&frames, &meta, sent, without));
    std::io::stdout().flush().ok();
    if let Some(out) = std::env::var_os("LUCINANET_SCROLL_BENCH_OUT") {
        if let Err(e) = std::fs::write(&out, scrollbench::json(&frames, &meta, sent, without, &swgl)) {
            eprintln!("scroll-bench: {out:?}: {e}");
        }
    }
    // Back to the top, unrecorded: what the caller writes as a PNG is the page it loaded, not its bottom.
    for _ in 0..sent {
        engine.send(wheel(false));
        std::thread::sleep(Duration::from_millis(2));
    }
    quiet(rx, Duration::from_millis(300), Duration::from_secs(3));
}

/// What one step of `LUCINANET_LOADS` showed, each as the time since the command.
#[cfg(feature = "harness")]
struct Seen {
    url: Option<Duration>,
    frame: Option<Duration>,
    complete: Option<Duration>,
    timed_out: bool,
}

/// Waits for a step to show itself: the load-complete signal (and a frame, up to a second after it), or, when no load began
/// (a Back from the cache sends no Loading signal; a load always waits for its signal), the first frame and 300 ms without news about the navigation.
#[cfg(feature = "harness")]
fn watch_step(rx: &std::sync::mpsc::Receiver<News>, sent: std::time::Instant, limit: Duration, expect_load: bool) -> Seen {
    let mut seen = Seen { url: None, frame: None, complete: None, timed_out: false };
    let mut loading = false;
    let mut last_news = std::time::Instant::now();
    loop {
        let now = std::time::Instant::now();
        if now >= sent + limit {
            seen.timed_out = true;
            return seen;
        }
        match seen.complete {
            Some(at) if seen.frame.is_some() || now >= sent + at + Duration::from_millis(1000) => return seen,
            _ => {},
        }
        if !expect_load && !loading && seen.frame.is_some() && now >= last_news + Duration::from_millis(300) {
            return seen;
        }
        match rx.recv_timeout(Duration::from_millis(2)) {
            Ok(News::Frame) => {
                if seen.frame.is_none() {
                    seen.frame = Some(sent.elapsed());
                    last_news = std::time::Instant::now();
                }
            },
            Ok(News::Loading(true)) => {
                loading = true;
                last_news = std::time::Instant::now();
            },
            Ok(News::Loading(false)) => {
                if seen.complete.is_none() {
                    seen.complete = Some(sent.elapsed());
                }
                last_news = std::time::Instant::now();
            },
            Ok(News::Url(_) | News::Title(_) | News::History { .. }) => {
                if seen.url.is_none() {
                    seen.url = Some(sent.elapsed());
                }
                last_news = std::time::Instant::now();
            },
            _ => {},
        }
    }
}

/// Reads and drops the news until `quiet` has passed without any, or `max` has.
#[cfg(feature = "harness")]
fn quiet(rx: &std::sync::mpsc::Receiver<News>, quiet: Duration, max: Duration) {
    let end = std::time::Instant::now() + max;
    let mut last = std::time::Instant::now();
    while std::time::Instant::now() < end && last.elapsed() < quiet {
        if rx.recv_timeout(Duration::from_millis(20)).is_ok() {
            last = std::time::Instant::now();
        }
    }
}

#[cfg(feature = "harness")]
fn load_average() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().map(str::to_owned))
        .unwrap_or_else(|| "-".into())
}

/// One parseable line per step (see `LUCINANET_LOADS`).
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "harness")]
fn step_line(
    step: u32,
    cmd: &str,
    arg: &str,
    url: Option<Duration>,
    frame: Option<Duration>,
    complete: Option<Duration>,
    timed_out: bool,
    phase: &str,
) {
    let ms = |d: Option<Duration>| d.map_or("-".to_owned(), |d| format!("{:.1}", d.as_secs_f64() * 1000.0));
    eprintln!(
        "timing step={step} cmd={cmd} arg={} phase={phase} url_ms={} frame_ms={} complete_ms={} loading={} timed_out={} rss_mib={} load1={}",
        arg.trim().replace(" ", "_"),
        ms(url),
        ms(frame),
        ms(complete),
        u8::from(complete.is_some()),
        u8::from(timed_out),
        rss_kib() / 1024,
        load_average()
    );
}

static EXIT_HOOKS: Mutex<Vec<Box<dyn Fn() + Send>>> = Mutex::new(Vec::new());

/// Run `hook` when the program ends through [`exit_now`], which every way out of `main` and `desktop::run` uses: the
/// process ends with `_exit`, so a store that keeps writes back (the history) flushes here and not in a destructor.
pub fn on_exit(hook: Box<dyn Fn() + Send>) {
    EXIT_HOOKS.lock().unwrap_or_else(|e| e.into_inner()).push(hook);
}

/// Run the exit hooks, then end the process without running destructors.
pub fn exit_now() -> ! {
    for hook in EXIT_HOOKS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        hook();
    }
    // SpiderMonkey's destructors abort if run with its threads alive.
    unsafe { libc::_exit(0) }
}

/// What the command line asks for (see [`parse_mode`]).
#[derive(Debug, PartialEq, Eq)]
pub enum Mode<'a> {
    /// `--version`: the version line, then exit.
    Version,
    /// `--render <url> <out.png> [WxH] [dump.json]`: a harness run, no window (harness builds only).
    #[cfg(feature = "harness")]
    Render { url: &'a str, out: &'a str, width: u32, height: u32, dump: Option<&'a str> },
    /// A `--option` this build does not have: exit with an error and start nothing (normal builds only).
    #[cfg(not(feature = "harness"))]
    Refused(&'a str),
    /// The browser; the first argument, if any, is the start address or `--desktop`.
    Browser(Option<&'a str>),
}

/// The flag of the unattended page load. Compared as data (`black_box`), not folded into an integer compare, so a harness
/// binary holds the string and `tests/harness-gate/check-release-binary.sh` can tell it from a build without the door.
#[cfg(feature = "harness")]
const RENDER_FLAG: &str = "--render";

/// Reads the command line: which mode, and nothing else (no engine is started and no variable is read).
pub fn parse_mode(args: &[String]) -> Mode<'_> {
    match args.get(1).map(String::as_str) {
        Some("--version") => Mode::Version,
        #[cfg(feature = "harness")]
        Some(flag) if args.len() >= 4 && flag == std::hint::black_box(RENDER_FLAG) => {
            let (width, height) = args
                .get(4)
                .and_then(|s| s.split_once('x'))
                .map_or((1920, 1000), |(w, h)| (w.parse().unwrap(), h.parse().unwrap()));
            Mode::Render { url: &args[2], out: &args[3], width, height, dump: args.get(5).map(String::as_str) }
        },
        // A normal build has no `--option` of its own besides `--desktop` and `--version`: any other is refused, so `--render` or a
        // probe flag is not taken for an address either (that would be a search request). A harness build reads the line as before.
        #[cfg(not(feature = "harness"))]
        Some(option) if option.starts_with("--") && option != "--desktop" => Mode::Refused(option),
        // The launcher always puts `--desktop` first, so the arguments after it are read the same way.
        #[cfg(not(feature = "harness"))]
        Some("--desktop") if args.get(2).is_some_and(|option| option.starts_with("--")) => Mode::Refused(&args[2]),
        first => Mode::Browser(first),
    }
}

/// The version line `--version` prints. A harness build says so, with the marker that tools/release, the packagers and the
/// scripts that need the doors look for in a binary (`LUCINANET-HARNESS-BUILD`).
#[cfg(not(feature = "harness"))]
pub const VERSION_LINE: &str = concat!("lucinanet ", env!("CARGO_PKG_VERSION"));
#[cfg(feature = "harness")]
pub const VERSION_LINE: &str = concat!(
    "lucinanet ",
    env!("CARGO_PKG_VERSION"),
    " harness build (LUCINANET-HARNESS-BUILD: for tests and measurements, never to be installed or shipped)"
);

/// The window's title: a harness build says what it is, so it cannot be taken for the browser.
#[cfg(not(feature = "harness"))]
pub const WINDOW_TITLE: &str = "LucinaNet";
#[cfg(feature = "harness")]
pub const WINDOW_TITLE: &str = "LucinaNet (harness)";

/// An engine's `main`: the browser, `--version`, or in a harness build `<bin> --render <url> <out.png> [WxH] [dump.json]`.
pub fn main(start: Start) -> ! {
    install_crash_report();
    // Before any engine thread starts: a font directory the list missed is a page of boxes.
    fonts::ensure_font_env();
    let args: Vec<String> = std::env::args().collect();
    match parse_mode(&args) {
        Mode::Version => {
            println!("{VERSION_LINE}");
            exit_now()
        },
        #[cfg(not(feature = "harness"))]
        Mode::Refused(option) => {
            eprintln!("lucinanet: unknown option {option}");
            std::process::exit(2)
        },
        #[cfg(feature = "harness")]
        Mode::Render { url, out, width, height, dump } => {
            render(start, url, out, width, height, dump);
            exit_now()
        },
        #[cfg(feature = "desktop")]
        Mode::Browser(first) => {
            let first = address::start_url(first.filter(|a| *a != "--desktop").or(args.get(2).map(String::as_str)));
            desktop::run(first, start)
        },
        #[cfg(not(feature = "desktop"))]
        Mode::Browser(first) => run("lucinanet", first, start),
    }
}

pub fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            let line = status.lines().find(|l| l.starts_with("VmRSS:"))?.to_string();
            line.split_whitespace().nth(1)?.parse().ok()
        })
        .unwrap_or(0)
}

/// A crash on Juno says where: the kernel only logs the signal.
#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
extern "C" fn report_crash(signal: i32, info: *mut libc::siginfo_t, context: *mut core::ffi::c_void) {
    // SAFETY: the kernel passes valid pointers to a SA_SIGINFO handler. Not async-signal-safe in general,
    // but the process is ending and this is its last word.
    unsafe {
        let pc = (*context.cast::<libc::ucontext_t>()).uc_mcontext.pc;
        let addr = (*info).si_addr() as u64;
        let instruction = if signal == libc::SIGILL {
            (pc as *const u32).read_unaligned()
        } else {
            0
        };
        eprintln!(
            "lucinanet: signal {signal} at pc {pc:#x}, address {addr:#x}, instruction {instruction:#010x}"
        );
        libc::signal(signal, libc::SIG_DFL);
    }
}

pub fn install_crash_report() {
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    for signal in [libc::SIGILL, libc::SIGBUS, libc::SIGSEGV] {
        // SAFETY: a zeroed sigaction with a valid handler and SA_SIGINFO, on its own stack for overflows.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = report_crash as usize;
            action.sa_flags = libc::SA_SIGINFO;
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

/// The test script's file (`LUCINANET_SCRIPT=<file>`, a harness build only) from `get`, the environment as a lookup so a test
/// can stand in for it.
#[cfg(feature = "harness")]
fn script_door(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
    get("LUCINANET_SCRIPT")
}

/// Without `harness` the variable is not read: its name is not in the program.
#[cfg(not(feature = "harness"))]
fn script_door(_get: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
    None
}

/// The window, until it ends. In a harness build `LUCINANET_SCRIPT=<file>` replays test input (`Browser::script`): one
/// command a line, `wait <ms>` between them.
pub fn run(name: &str, first: Option<&str>, start: Start) -> ! {
    let first = address::start_url(first);
    lucina_app::run(name, |tx| {
        // `None` unless this is a harness build (see `script_door`); the thread itself is not built otherwise.
        let script_file = script_door(|name| std::env::var_os(name));
        #[cfg(feature = "harness")]
        if let Some(path) = script_file {
            let tx = tx.clone();
            std::thread::spawn(move || script(path, tx));
        }
        #[cfg(not(feature = "harness"))]
        let _ = script_file;
        browser::Browser::new(tx, first, start)
    })
}

#[cfg(feature = "harness")]
fn script(path: std::ffi::OsString, tx: lucina_app::Sender<browser::Msg>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        match line.split_once(' ') {
            Some(("wait", ms)) => {
                std::thread::sleep(Duration::from_millis(ms.parse().unwrap_or(0)));
            },
            _ => tx.send(browser::Msg::Script(line.to_owned())),
        }
    }
}

/// What both engines say they are (Chromium's form, the platform token from the build target: `platform.rs`).
pub use platform::USER_AGENT;

/// CPU ticks so far per thread name, adding up threads with the same name.
#[cfg(feature = "harness")]
fn thread_ticks() -> std::collections::HashMap<String, u64> {
    let mut ticks = std::collections::HashMap::new();
    for task in std::fs::read_dir("/proc/self/task").into_iter().flatten().flatten() {
        let Ok(stat) = std::fs::read_to_string(task.path().join("stat")) else { continue };
        let Some((name, rest)) = stat.split_once(" (").and_then(|(_, r)| r.rsplit_once(") ")) else { continue };
        let fields: Vec<&str> = rest.split(' ').collect();
        let used: u64 = fields.get(11).and_then(|f| f.parse::<u64>().ok()).unwrap_or(0)
            + fields.get(12).and_then(|f| f.parse::<u64>().ok()).unwrap_or(0);
        *ticks.entry(name.to_owned()).or_insert(0) += used;
    }
    ticks
}

/// `LUCINANET_TIMING`: each phase of a start and a load, on the clock of /proc/uptime.
pub fn timing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LUCINANET_TIMING").is_some())
}

pub fn phase(name: &str) {
    if timing() {
        // CLOCK_MONOTONIC to the microsecond: /proc/uptime has 10 ms steps, too coarse for one navigation. Servo's
        // `netphase` lines use the same clock.
        let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: a valid clock id and a timespec to fill.
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
        eprintln!("phase {}.{:06} {name}", now.tv_sec, now.tv_nsec / 1000);
    }
}

#[cfg(all(test, feature = "harness"))]
mod settle_tests {
    use super::{settle_windows, settled};

    #[test]
    fn a_quiet_page_settles_and_a_busy_one_is_cut_off() {
        assert!(!settled(0, 0, 1500, 12_000));
        assert!(!settled(1499, 3000, 1500, 12_000));
        assert!(settled(1500, 3000, 1500, 12_000));
        // Frames never stop (an animation): the cap ends the wait.
        assert!(!settled(10, 11_999, 1500, 12_000));
        assert!(settled(10, 12_000, 1500, 12_000));
    }

    #[test]
    fn the_windows_default_and_zero_means_the_old_fixed_wait() {
        assert_eq!(settle_windows(None, None), (1500, 12_000));
        assert_eq!(settle_windows(Some(0), Some(30_000)), (800, 800));
        assert_eq!(settle_windows(Some(3000), Some(1000)), (3000, 3000));
        assert_eq!(settle_windows(Some(2500), None), (2500, 12_000));
    }
}

#[cfg(test)]
mod security_tests {
    use super::{Security, security_of};

    /// What the field marks each address it can be left showing. Only the two encrypted schemes get the lock; the plain
    /// ones, the ones that never left the machine, and what is not an address at all are the warning. Case belongs to the
    /// scheme, not to the machine.
    #[test]
    fn the_scheme_alone_decides_the_marks_security() {
        use Security::{NotSecure, Secure};
        const TABLE: [(&str, Security); 16] = [
            ("https://example.com/", Secure),
            ("HTTPS://example.com/", Secure),
            ("HtTpS://Example.COM/a?b#c", Secure),
            ("wss://echo.example/stream", Secure),
            ("WSS://echo.example/stream", Secure),
            ("http://example.com/", NotSecure),
            ("HTTP://example.com/", NotSecure),
            ("ws://echo.example/stream", NotSecure),
            ("ftp://files.example/pub", NotSecure),
            ("file:///home/user/index.html", NotSecure),
            ("data:text/html,<b>hi</b>", NotSecure),
            ("blob:https://example.com/6b7a", NotSecure),
            ("about:blank", NotSecure),
            ("", NotSecure),
            ("   ", NotSecure),
            ("example.com:8080", NotSecure),
        ];
        for (url, want) in TABLE {
            assert_eq!(security_of(url), want, "{url}");
        }
    }
}

/// the doors that start a request or run script from the command line or the environment exist only in a
/// `harness` build. The environment is a lookup here, so no test sets a variable.
#[cfg(test)]
mod door_tests {
    use super::{Mode, VERSION_LINE, WINDOW_TITLE, parse_mode, script_door};

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_owned()).collect()
    }

    /// F5: `LUCINANET_SCRIPT`.
    #[cfg(not(feature = "harness"))]
    #[test]
    fn without_harness_the_script_variable_does_nothing() {
        assert_eq!(script_door(|_| Some("planted/script".into())), None);
    }

    #[cfg(feature = "harness")]
    #[test]
    fn with_harness_the_script_variable_names_the_script() {
        let get = |name: &str| (name == "LUCINANET_SCRIPT").then(|| std::ffi::OsString::from("planted/script"));
        assert_eq!(script_door(get), Some("planted/script".into()));
        assert_eq!(script_door(|_| None), None);
    }

    /// F6: `--render`, and with it `LUCINANET_LOADS` and `LUCINANET_RENDER_HOLD`, which are read inside it.
    #[cfg(not(feature = "harness"))]
    #[test]
    fn without_harness_render_is_refused_not_taken_for_an_address() {
        let args = argv(&["lucinanet", "--render", "http://127.0.0.1:9/", "out.png"]);
        assert_eq!(parse_mode(&args), Mode::Refused("--render"));
        // Through the launcher (`--desktop` first) it is refused the same way, not taken for an address.
        let args = argv(&["lucinanet", "--desktop", "--render", "http://127.0.0.1:9/", "out.png"]);
        assert_eq!(parse_mode(&args), Mode::Refused("--render"));
    }

    #[cfg(feature = "harness")]
    #[test]
    fn with_harness_render_is_parsed_as_before() {
        let args = argv(&["lucinanet", "--render", "http://127.0.0.1:9/", "out.png", "800x600", "dump.json"]);
        assert_eq!(
            parse_mode(&args),
            Mode::Render { url: "http://127.0.0.1:9/", out: "out.png", width: 800, height: 600, dump: Some("dump.json") }
        );
        let args = argv(&["lucinanet", "--render", "http://127.0.0.1:9/", "out.png"]);
        assert_eq!(
            parse_mode(&args),
            Mode::Render { url: "http://127.0.0.1:9/", out: "out.png", width: 1920, height: 1000, dump: None }
        );
    }

    /// F7 (the diagnostic subcommands of the binary crate reach `main` here when it does not take them): any other
    /// `--option` is refused in a normal build, so none becomes a search.
    #[cfg(not(feature = "harness"))]
    #[test]
    fn without_harness_an_unknown_option_is_refused() {
        for option in ["--netprobe", "--udpdns", "--dnsprobe", "--whatever"] {
            assert_eq!(parse_mode(&argv(&["lucinanet", option, "example.org"])), Mode::Refused(option));
        }
    }

    #[test]
    fn an_address_a_desktop_flag_and_no_argument_still_start_the_browser() {
        assert_eq!(parse_mode(&argv(&["lucinanet"])), Mode::Browser(None));
        assert_eq!(parse_mode(&argv(&["lucinanet", "--desktop", "https://example.org/"])), Mode::Browser(Some("--desktop")));
        assert_eq!(parse_mode(&argv(&["lucinanet", "https://example.org/"])), Mode::Browser(Some("https://example.org/")));
        assert_eq!(parse_mode(&argv(&["lucinanet", "--version"])), Mode::Version);
    }

    /// A harness build says so in its version line (and its window title); a normal build does not.
    #[test]
    fn the_version_line_says_harness_only_in_a_harness_build() {
        assert_eq!(VERSION_LINE.contains("harness"), cfg!(feature = "harness"));
        assert_eq!(VERSION_LINE.contains("LUCINANET-HARNESS-BUILD"), cfg!(feature = "harness"));
        assert_eq!(WINDOW_TITLE.contains("harness"), cfg!(feature = "harness"));
        assert!(VERSION_LINE.starts_with("lucinanet "));
    }
}
