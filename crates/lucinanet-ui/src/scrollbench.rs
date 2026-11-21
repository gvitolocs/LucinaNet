//! The scroll benchmark: what one wheel step costs, and where the time goes.
//!
//! `LUCINANET_SCROLL_BENCH=<steps>x<delta CSS px>` turns the offscreen `--render` mode into a scrolling run:
//! the root is scrolled `steps` times by `delta` CSS px, one step per nominal vsync tick, and every presented
//! frame is timed. The pure half lives here - the spec parse, the diagnostic line parse, the percentiles, the
//! table and the JSON - so each can be tested without a page.
//!
//! Two sources of numbers:
//!  - this process's own: the frame interval, the scroll-to-present latency, and `paint` (WebRender's update
//!    and render), `present` (swgl's flush) and `copy` (the frame into `Page`), split apart in `engine.rs` and
//!    carried by [`crate::News::Painted`];
//!  - the engine's and WebRender's own lines, read back from the run's log ([`Tail`]): `LUCINANET_RENDER_STATS`,
//!    `LUCINANET_WR_PHASES`, `LUCINANET_LAYOUT_PHASES` and `LUCINANET_SWGL_PROFILE`. They give the picture-cache
//!    and composite split, the tiles redrawn, the display lists built and swgl's own draw time.
//!
//! A line cannot be tied to a frame more closely than "written since the frame before", so a stage is charged
//! to the frame presented after it ([`attribute`]).

use std::io::Read;
use std::path::{Path, PathBuf};

/// The nominal frame at 60 Hz, in milliseconds: the budget a scroll run is judged against.
pub const NOMINAL_MS: f64 = 16.7;
/// The slow frame a person notices.
pub const SLOW_MS: f64 = 33.3;
/// How long a step waits for its frame before it is said to have had none.
pub const STEP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

const MS: &str = "ms";
const COUNT: &str = "count";
const KPX: &str = "kpx";

/// `LUCINANET_SCROLL_BENCH=<steps>x<delta>`: 60 steps of 120 CSS px. The delta may be a fraction; a bare
/// number is steps with 120 px, twice a wheel notch. None for anything else, which is how the bench stays off.
pub fn parse_spec(text: Option<&str>) -> Option<(u32, f64)> {
    let text = text?.trim();
    if text.is_empty() {
        return None;
    }
    let (steps, delta) = match text.split_once(['x', 'X']) {
        Some((steps, delta)) => (steps, Some(delta)),
        None => (text, None),
    };
    let steps: u32 = steps.trim().parse().ok()?;
    let delta: f64 = match delta {
        Some(delta) => delta.trim().parse().ok()?,
        None => 120.0,
    };
    if steps == 0 || !delta.is_finite() || delta <= 0.0 {
        return None;
    }
    Some((steps, delta))
}

/// The same for `LUCINANET_SCALE`, the device pixels per CSS pixel of a `--render` run. A desktop window gets
/// its scale from the toolkit; an offscreen run has no toolkit to ask, so the benchmark says.
pub fn parse_scale(text: Option<&str>) -> f32 {
    text.and_then(|s| s.trim().parse::<f32>().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(1.0)
}

/// One presented frame: what was measured around it, and what the engine said since the frame before.
#[derive(Clone, Copy, Default, Debug)]
pub struct Frame {
    /// Which wheel step this frame answers: the first frame after a step claims it, a later frame of the same
    /// step is the page's own doing. `None` when no step was waiting.
    pub step: Option<usize>,
    /// Milliseconds since the previous frame was presented. `None` for the first frame of the run: there is
    /// no frame before it, and a 0.0 there would be read as the fastest frame in the whole table.
    pub interval_ms: Option<f64>,
    /// Milliseconds from the wheel step that this frame answers to this frame being presented; only the first
    /// frame after a step has one. It is measured, and a step that was replaced by a newer one has none.
    pub since_step_ms: Option<f64>,
    /// `webview.paint()`: WebRender's update and render, its whole `render()` from the embedder's side.
    pub paint_ms: f64,
    /// `context.present()`: swgl flushing what the frame drew into the pixels of the framebuffer.
    pub present_ms: f64,
    /// The finished frame copied into `Page`: the blit the compositor of the window waits for.
    pub copy_ms: f64,
    /// `LUCINANET_RENDER_STATS`: the same paint from inside WebRender, in whole milliseconds.
    pub wr_update_ms: Option<f64>,
    pub wr_render_ms: Option<f64>,
    pub wr_draws: Option<u64>,
    /// `frame_build` in the same line: WebRender's own build of the frame, which is inside `paint` too.
    pub wr_frame_build_ms: Option<f64>,
    /// The texture uploads of the frame: `upload 0.512 MB 0.3 ms` in the same line.
    pub wr_upload_mb: Option<f64>,
    pub wr_upload_ms: Option<f64>,
    /// How many documents WebRender said it rendered for this frame. A page with frames in it renders one line
    /// per document, so the stage numbers below are the sum of them and this says how many were added.
    pub wr_docs: Option<u64>,
    /// Whether WebRender said this frame painted the whole window instead of what its caches invalidated.
    pub wr_full_paint: Option<bool>,
    /// `LUCINANET_WR_PHASES`: the picture cache, the passes of the frame, and the areas they covered.
    pub wr_cache_ms: Option<f64>,
    pub wr_tiles: Option<u64>,
    pub wr_tiles_ms: Option<f64>,
    pub wr_tiles_kpx: Option<u64>,
    pub wr_alpha_ms: Option<f64>,
    pub wr_color_ms: Option<f64>,
    pub wr_color_kpx: Option<u64>,
    pub wr_composite_ms: Option<f64>,
    /// `LUCINANET_LAYOUT_PHASES`: the reflows of this frame's window, and whether one built a display list.
    pub layout_restyle_ms: Option<f64>,
    pub layout_list_ms: Option<f64>,
    pub display_list: Option<bool>,
}

/// A diagnostic of the run's log, said by a thread this harness does not own and read back by [`Tail`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Line {
    /// `wr: update 0 render 1 ms, draws 12 color 1 alpha 0 frame_build 0.0 upload 0.000 MB 0.0 ms full_paint true`
    Wr {
        update_ms: f64,
        render_ms: f64,
        draws: u64,
        frame_build_ms: f64,
        upload_mb: f64,
        upload_ms: f64,
        full_paint: bool,
    },
    /// `wr-phases cache 0.2 tiles 1 0.7 ms 152 kpx, alpha 0.0, color 3.1 ms 7590 kpx, composite 0.4 ms`
    WrPhases {
        cache_ms: f64,
        tiles: u64,
        tiles_ms: f64,
        tiles_kpx: u64,
        alpha_ms: f64,
        color_ms: f64,
        color_kpx: u64,
        composite_ms: f64,
    },
    /// `layout-phases restyle+trees 1.20 ms, stacking contexts and display list 0.30 ms, ran RanLayout | BuiltDisplayList`
    LayoutPhases { restyle_ms: f64, list_ms: f64, display_list: bool },
    /// `swgl-prof window 2010 ms, swgl 431.2 ms`: the rasterizer's own share of the wall clock, summed over
    /// its draws of the last two seconds. The rows under it name shaders; they stay in the log.
    SwglWindow { window_ms: f64, swgl_ms: f64 },
}

/// One line of the ones the benchmark reads. Anything else - console output, a phase, a warning - is None and
/// stays only in the log.
pub fn parse_line(line: &str) -> Option<Line> {
    let line = line.trim_end_matches('\r');
    if let Some(rest) = line.strip_prefix("wr-phases ") {
        let f = words(rest);
        // `cache 0.2 tiles 1 0.7 ms 152 kpx, alpha 0.0, color 3.1 ms 7590 kpx, composite 0.4 ms`
        // The tile and colour areas follow a `ms` of their own, so they are counted from their label.
        let (tiles, color) = (word(&f, "tiles")?, word(&f, "color")?);
        Some(Line::WrPhases {
            cache_ms: after(&f, "cache")?,
            tiles: whole_at(&f, tiles + 1)?,
            tiles_ms: dec(&f, tiles + 2)?,
            tiles_kpx: whole_at(&f, tiles + 4)?,
            alpha_ms: after(&f, "alpha")?,
            color_ms: dec(&f, color + 1)?,
            color_kpx: whole_at(&f, color + 3)?,
            composite_ms: after(&f, "composite")?,
        })
    } else if let Some(rest) = line.strip_prefix("wr: ") {
        let f = words(rest);
        // `update 0 render 1 ms, draws 12 color 1 alpha 0 frame_build 0.0 upload 0.000 MB 0.0 ms full_paint true`
        Some(Line::Wr {
            update_ms: after(&f, "update")?,
            render_ms: after(&f, "render")?,
            draws: slot(&f, "draws")?.parse().ok()?,
            frame_build_ms: after(&f, "frame_build")?,
            upload_mb: after(&f, "upload")?,
            upload_ms: after(&f, "MB")?,
            full_paint: match slot(&f, "full_paint")? {
                "true" => true,
                "false" => false,
                _ => return None,
            },
        })
    } else if let Some(rest) = line.strip_prefix("layout-phases ") {
        // `restyle+trees 1.20 ms, stacking contexts and display list 0.30 ms, ran RanLayout | BuiltDisplayList`
        let f = words(rest);
        Some(Line::LayoutPhases {
            restyle_ms: after(&f, "restyle+trees")?,
            list_ms: after(&f, "list")?,
            display_list: line.contains("BuiltDisplayList"),
        })
    } else if let Some(rest) = line.strip_prefix("swgl-prof window ") {
        // `2010 ms, swgl 431.2 ms`
        let f = words(rest);
        Some(Line::SwglWindow {
            window_ms: f.first()?.parse().ok()?,
            swgl_ms: after(&f, "swgl")?,
        })
    } else {
        None
    }
}

/// A diagnostic as words: `,` is as much a separator as a space, and the units are words of their own.
fn words(text: &str) -> Vec<&str> {
    text.split([' ', ',']).map(str::trim).filter(|w| !w.is_empty()).collect()
}

/// Where a label of the format string is.
fn word(f: &[&str], label: &str) -> Option<usize> {
    f.iter().position(|w| *w == label)
}

/// The word a label introduces.
fn slot<'a>(f: &[&'a str], label: &str) -> Option<&'a str> {
    f.get(word(f, label)? + 1).copied()
}

/// The number a label introduces.
fn after(f: &[&str], label: &str) -> Option<f64> {
    slot(f, label)?.parse().ok()
}

fn dec(f: &[&str], at: usize) -> Option<f64> {
    f.get(at)?.parse().ok()
}

fn whole_at(f: &[&str], at: usize) -> Option<u64> {
    f.get(at)?.parse().ok()
}

/// What the run is, for the table and the JSON.
#[derive(Clone, Debug)]
pub struct Meta {
    pub url: String,
    pub steps: usize,
    pub delta_css_px: f64,
    pub scale: f32,
    /// The window in device pixels; in CSS px it is the quotient.
    pub width: u32,
    pub height: u32,
    pub paced: bool,
    pub raster: String,
    pub raster_threads: String,
    /// Whether `LUCINANET_LAYOUT_PHASES` was on. Silence from it counts as "no reflow" only when it was, which is what
    /// makes the async-scroll line honest rather than flattering.
    pub layout_diag: bool,
    pub log: Option<PathBuf>,
}

impl Meta {
    /// The window in device pixels: the area a frame could repaint.
    pub fn window_px(&self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

/// What one column of the run says: how many frames, the percentiles, the mean, and how many were slow.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub n: usize,
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
    pub max: f64,
    pub mean: f64,
    pub over_nominal: usize,
    pub over_slow: usize,
}

/// `p` of a sorted sample, 0 to 1, between the two nearest samples.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 || p <= 0.0 {
        return sorted[0];
    }
    let pos = p.min(1.0) * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
}

/// The sample of one column, with the slow frames of a time column counted.
pub fn stats(values: &[f64]) -> Stats {
    if values.is_empty() {
        return Stats::default();
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    Stats {
        n: sorted.len(),
        p50: percentile(&sorted, 0.50),
        p90: percentile(&sorted, 0.90),
        p99: percentile(&sorted, 0.99),
        max: sorted[sorted.len() - 1],
        mean: sorted.iter().sum::<f64>() / sorted.len() as f64,
        over_nominal: sorted.iter().filter(|v| **v > NOMINAL_MS).count(),
        over_slow: sorted.iter().filter(|v| **v > SLOW_MS).count(),
    }
}

/// The mean of a kilo-pixel column as a percent of the window: how much of the screen a frame redrew.
pub fn mean_share_kpx(values: &[f64], window_px: u64) -> f64 {
    if values.is_empty() || window_px == 0 {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64 * 1000.0 / window_px as f64 * 100.0
}

/// Whether a scroll moved the page without a display list: the two counts the question is asked with. A run
/// that builds none scrolls on the compositor, from the scroll tree it already has; one that builds one per
/// frame scrolls on the main thread.
pub fn async_scroll(frames: &[Frame]) -> (usize, usize) {
    let lists = frames.iter().filter(|f| f.display_list == Some(true)).count();
    let stepped = frames.iter().filter(|f| f.step.is_some()).count();
    (lists, stepped)
}

/// One named sample of the run, with the unit it is printed in.
struct Column {
    name: &'static str,
    unit: &'static str,
    values: Vec<f64>,
}

impl Column {
    fn new(name: &'static str, unit: &'static str, values: Vec<f64>) -> Column {
        Column { name, unit, values }
    }

    /// `frame interval  ms    n  120  p50    8.20  p90   14.10  p99   30.22  max   55.10  mean    9.35  over 16.7  11  over 33.3   3`
    fn row(&self) -> String {
        let s = stats(&self.values);
        let mut line = format!(
            "{:<26} {:<5} n {:>4}  p50 {:>7.2}  p90 {:>7.2}  p99 {:>7.2}  max {:>7.2}  mean {:>7.2}",
            self.name, self.unit, s.n, s.p50, s.p90, s.p99, s.max, s.mean,
        );
        if self.unit == MS {
            line.push_str(&format!("  over {NOMINAL_MS:.1} {:>3}  over {SLOW_MS:.1} {:>3}", s.over_nominal, s.over_slow));
        }
        line
    }
}

fn picked(frames: &[Frame], pick: fn(&Frame) -> Option<f64>) -> Vec<f64> {
    frames.iter().filter_map(|f| pick(f)).collect()
}

/// The columns, in the order they are printed. A stage the log did not say is skipped rather than printed as
/// zero, so an absent number is visibly absent.
fn columns(frames: &[Frame]) -> Vec<Column> {
    let mut out = vec![
        Column::new(
            "frame interval",
            MS,
            picked(frames, |f| f.interval_ms),
        ),
        Column::new(
            "scroll to present",
            MS,
            frames.iter().filter_map(|f| f.since_step_ms).collect(),
        ),
        Column::new("paint: wr update+render", MS, frames.iter().map(|f| f.paint_ms).collect()),
        Column::new("present: swgl flush", MS, frames.iter().map(|f| f.present_ms).collect()),
        Column::new("copy into page", MS, frames.iter().map(|f| f.copy_ms).collect()),
    ];
    for (name, unit, values) in [
        ("wr update (stats)", MS, picked(frames, |f| f.wr_update_ms)),
        ("wr render (stats)", MS, picked(frames, |f| f.wr_render_ms)),
        ("wr frame build (stats)", MS, picked(frames, |f| f.wr_frame_build_ms)),
        ("wr upload (stats)", MS, picked(frames, |f| f.wr_upload_ms)),
        ("documents per frame", COUNT, picked(frames, |f| f.wr_docs.map(|v| v as f64))),
        ("cache: tiles redrawn", COUNT, picked(frames, |f| f.wr_tiles.map(|v| v as f64))),
        ("cache: tiles time", MS, picked(frames, |f| f.wr_tiles_ms)),
        ("cache: tiles area", KPX, picked(frames, |f| f.wr_tiles_kpx.map(|v| v as f64))),
        ("cache: alpha targets", MS, picked(frames, |f| f.wr_alpha_ms)),
        ("cache: colour pass", MS, picked(frames, |f| f.wr_color_ms)),
        ("cache: colour area", KPX, picked(frames, |f| f.wr_color_kpx.map(|v| v as f64))),
        ("cache: texture cache", MS, picked(frames, |f| f.wr_cache_ms)),
        ("wr composite", MS, picked(frames, |f| f.wr_composite_ms)),
        ("draw calls", COUNT, picked(frames, |f| f.wr_draws.map(|v| v as f64))),
        ("layout: restyle+trees", MS, picked(frames, |f| f.layout_restyle_ms)),
        ("layout: list build", MS, picked(frames, |f| f.layout_list_ms)),
    ] {
        if !values.is_empty() {
            out.push(Column::new(name, unit, values));
        }
    }
    out.retain(|c| !c.values.is_empty());
    out
}

/// What the run says about async scrolling - and whether it is entitled to say it. `LUCINANET_LAYOUT_PHASES` is the
/// only thing here that reports a reflow, so when it was off the counts mean nothing and the line says so.
pub fn async_evidence(frames: &[Frame], meta: &Meta) -> String {
    let (lists, stepped) = async_scroll(frames);
    let refl = frames.iter().filter(|f| f.display_list.is_some()).count();
    if !meta.layout_diag {
        return format!(
            "{lists} display lists over {stepped} stepped frames, which proves nothing: LUCINANET_LAYOUT_PHASES was \
             off, so the log could not have said otherwise"
        );
    }
    if stepped == 0 {
        return "no frame was presented while the steps went out, so the run says nothing about the scroll".to_string();
    }
    if lists > 0 {
        return format!("{lists} of the {stepped} frames that followed a step rebuilt a display list: the scroll went through layout");
    }
    if refl > 0 {
        return format!(
            "{stepped} frames followed a step; {refl} of them reported a reflow and none built a display list, so the \
             scroll did not go through layout"
        );
    }
    format!("{stepped} frames followed a step and none reported a reflow at all: the compositor moved the scroll tree alone")
}

/// The table the run ends with: what was asked, one row per stage, then the two counts the questions are
/// asked with.
pub fn table(frames: &[Frame], meta: &Meta, inputs: usize, merged_steps: usize) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "scroll-bench {} steps x {:.0} CSS px at scale {:.2}, window {}x{} device px ({}x{} CSS), {}\n",
        meta.steps,
        meta.delta_css_px,
        meta.scale,
        meta.width,
        meta.height,
        (meta.width as f64 / meta.scale as f64).round(),
        (meta.height as f64 / meta.scale as f64).round(),
        if meta.paced { "paced to 16.7 ms" } else { "unpaced: as fast as the engine presents" },
    ));
    out.push_str(&format!("scroll-bench page {} rasterizer {} (threads {})\n", meta.url, meta.raster, meta.raster_threads));
    out.push_str(&format!(
        "scroll-bench frames: {} of the {} wanted, from {} wheel steps; {} of those steps were merged into a newer one \
         before a frame showed them, which is what a page too slow for the input looks like to a hand\n",
        frames.len(),
        meta.steps,
        inputs,
        merged_steps,
    ));
    for column in columns(frames) {
        out.push_str(&column.row());
        out.push('\n');
    }
    let tiles = picked(frames, |f| f.wr_tiles_kpx.map(|v| v as f64));
    let color = picked(frames, |f| f.wr_color_kpx.map(|v| v as f64));
    out.push_str(&format!(
        "scroll-bench redrawn area: {:.1} % of the window re-rastered on average, colour pass {:.1} %; {} frames of {} said WebRender painted the whole window\n",
        mean_share_kpx(&tiles, meta.window_px()),
        mean_share_kpx(&color, meta.window_px()),
        frames.iter().filter(|f| f.wr_full_paint == Some(true)).count(),
        frames.iter().filter(|f| f.wr_full_paint.is_some()).count(),
    ));
    out.push_str(&format!("scroll-bench async scroll: {}\n", async_evidence(frames, meta)));
    out
}

/// The same run as JSON, for the record. The frames are kept whole: a percentile cannot show a frame that was
/// slow for a reason the average hides.
pub fn json(frames: &[Frame], meta: &Meta, inputs: usize, merged_steps: usize, swgl_windows: &[(f64, f64)]) -> String {
    let (lists, stepped) = async_scroll(frames);
    let tiles = picked(frames, |f| f.wr_tiles_kpx.map(|v| v as f64));
    let color = picked(frames, |f| f.wr_color_kpx.map(|v| v as f64));
    let mut out = String::new();
    out.push_str("{\n  \"meta\": {");
    out.push_str(&format!(
        "\"url\": \"{}\", \"steps\": {}, \"delta_css_px\": {:.3}, \"scale\": {:.3}, \"device_px\": [{}, {}], \"css_px\": [{}, {}], \"paced\": {}, \"raster\": \"{}\", \"raster_threads\": \"{}\", \"nominal_ms\": {:.1}, \"step_timeout_ms\": {:.0}, \"log\": {}",
        escape(&meta.url),
        meta.steps,
        meta.delta_css_px,
        meta.scale,
        meta.width,
        meta.height,
        (meta.width as f64 / meta.scale as f64).round() as u32,
        (meta.height as f64 / meta.scale as f64).round() as u32,
        meta.paced,
        escape(&meta.raster),
        escape(&meta.raster_threads),
        NOMINAL_MS,
        STEP_TIMEOUT.as_secs_f64() * 1000.0,
        match &meta.log {
            Some(path) => format!("\"{}\"", escape(&path.display().to_string())),
            None => "null".to_string(),
        },
    ));
    out.push_str("},\n");
    out.push_str(&format!("  \"inputs\": {}, \"merged_steps\": {},\n", inputs, merged_steps));
    out.push_str(&format!(
        "  \"summary\": {{\"frames\": {}, \"display_lists\": {}, \"stepped_frames\": {}, \"window_device_px\": {}, \"tiles_redrawn_pct\": {:.3}, \"colour_dirty_pct\": {:.3}, \"full_paint_frames\": {}, \"layout_diag\": {}, \"async_evidence\": \"{}\", \"swgl_ms\": {:.1}, \"swgl_window_ms\": {:.0}}},\n",
        frames.len(),
        lists,
        stepped,
        meta.window_px(),
        mean_share_kpx(&tiles, meta.window_px()),
        mean_share_kpx(&color, meta.window_px()),
        frames.iter().filter(|f| f.wr_full_paint == Some(true)).count(),
        meta.layout_diag,
        escape(&async_evidence(frames, meta)),
        swgl_windows.iter().map(|(_, ms)| *ms).sum::<f64>(),
        swgl_windows.iter().map(|(ms, _)| *ms).sum::<f64>(),
    ));
    out.push_str("  \"stats\": {");
    for (i, column) in columns(frames).iter().enumerate() {
        let s = stats(&column.values);
        out.push_str(&format!(
            "{}\n    \"{}: {}\": {{\"unit\": \"{}\", \"n\": {}, \"p50\": {:.3}, \"p90\": {:.3}, \"p99\": {:.3}, \"max\": {:.3}, \"mean\": {:.3}, \"over_16_7\": {}, \"over_33_3\": {}}}",
            if i == 0 { "" } else { "," },
            escape(column.name),
            column.unit,
            column.unit,
            s.n,
            s.p50,
            s.p90,
            s.p99,
            s.max,
            s.mean,
            s.over_nominal,
            s.over_slow,
        ));
    }
    out.push_str("\n  },\n  \"frames\": [\n");
    for (i, frame) in frames.iter().enumerate() {
        out.push_str(&format!(
            "    {{\"step\": {}, \"interval_ms\": {}, \"since_step_ms\": {}, \"paint_ms\": {:.3}, \"present_ms\": {:.3}, \"copy_ms\": {:.3}{}}}{}",
            match frame.step {
                Some(step) => step.to_string(),
                None => "null".to_string(),
            },
            num(frame.interval_ms),
            num(frame.since_step_ms),
            frame.paint_ms,
            frame.present_ms,
            frame.copy_ms,
            extra(frame),
            if i + 1 < frames.len() { "," } else { "" },
        ));
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

fn num(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.3}"),
        None => "null".to_string(),
    }
}

fn whole_json(value: Option<u64>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "null".to_string(),
    }
}

fn flag(value: Option<bool>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "null".to_string(),
    }
}

/// The stages that came from the log, so a frame the log said nothing about carries nothing.
fn extra(frame: &Frame) -> String {
    let mut out = String::new();
    for (name, value) in [
        ("wr_update_ms", num(frame.wr_update_ms)),
        ("wr_render_ms", num(frame.wr_render_ms)),
        ("wr_draws", whole_json(frame.wr_draws)),
        ("wr_frame_build_ms", num(frame.wr_frame_build_ms)),
        ("wr_upload_mb", num(frame.wr_upload_mb)),
        ("wr_upload_ms", num(frame.wr_upload_ms)),
        ("wr_docs", whole_json(frame.wr_docs)),
        ("wr_full_paint", flag(frame.wr_full_paint)),
        ("wr_cache_ms", num(frame.wr_cache_ms)),
        ("wr_tiles", whole_json(frame.wr_tiles)),
        ("wr_tiles_ms", num(frame.wr_tiles_ms)),
        ("wr_tiles_kpx", whole_json(frame.wr_tiles_kpx)),
        ("wr_alpha_ms", num(frame.wr_alpha_ms)),
        ("wr_color_ms", num(frame.wr_color_ms)),
        ("wr_color_kpx", whole_json(frame.wr_color_kpx)),
        ("wr_composite_ms", num(frame.wr_composite_ms)),
        ("layout_restyle_ms", num(frame.layout_restyle_ms)),
        ("layout_list_ms", num(frame.layout_list_ms)),
        ("display_list", flag(frame.display_list)),
    ] {
        if value != "null" {
            out.push_str(&format!(", \"{name}\": {value}"));
        }
    }
    out
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Put a run's diagnostics into the frames they came with: the lines between two presented frames belong to the
/// later one, because WebRender says them inside the `render()` that ends with it.
pub fn attribute(frames: &mut [Frame], windows: Vec<Vec<Line>>) {
    for (frame, lines) in frames.iter_mut().zip(windows) {
        for line in lines {
            fold(frame, line);
        }
    }
}

/// A diagnostic, folded into the frame it belongs to. Lines of the same kind add up: two reflows between two
/// frames are the price of that frame. WebRender says one pair of lines per frame, so those simply land.
pub fn fold(frame: &mut Frame, line: Line) {
    match line {
        // A frame can hold several documents, each with its own line: they are summed, because the frame is what
        // the hand sees, and the count of them is kept so the sum is not read as one document's.
        Line::Wr { update_ms, render_ms, draws, frame_build_ms, upload_mb, upload_ms, full_paint } => {
            let add = |to: &mut Option<f64>, v: f64| *to = Some(to.unwrap_or(0.0) + v);
            add(&mut frame.wr_update_ms, update_ms);
            add(&mut frame.wr_render_ms, render_ms);
            add(&mut frame.wr_frame_build_ms, frame_build_ms);
            add(&mut frame.wr_upload_ms, upload_ms);
            frame.wr_upload_mb = Some(frame.wr_upload_mb.unwrap_or(0.0) + upload_mb);
            frame.wr_draws = Some(frame.wr_draws.unwrap_or(0) + draws);
            frame.wr_docs = Some(frame.wr_docs.unwrap_or(0) + 1);
            // "the whole window" of any one document is a full composite of the frame.
            frame.wr_full_paint = Some(frame.wr_full_paint.unwrap_or(false) || full_paint);
        },
        Line::WrPhases { cache_ms, tiles, tiles_ms, tiles_kpx, alpha_ms, color_ms, color_kpx, composite_ms } => {
            let add = |to: &mut Option<f64>, v: f64| *to = Some(to.unwrap_or(0.0) + v);
            let whole = |to: &mut Option<u64>, v: u64| *to = Some(to.unwrap_or(0) + v);
            add(&mut frame.wr_cache_ms, cache_ms);
            whole(&mut frame.wr_tiles, tiles);
            add(&mut frame.wr_tiles_ms, tiles_ms);
            whole(&mut frame.wr_tiles_kpx, tiles_kpx);
            add(&mut frame.wr_alpha_ms, alpha_ms);
            add(&mut frame.wr_color_ms, color_ms);
            whole(&mut frame.wr_color_kpx, color_kpx);
            add(&mut frame.wr_composite_ms, composite_ms);
        },
        Line::LayoutPhases { restyle_ms, list_ms, display_list } => {
            frame.layout_restyle_ms = Some(frame.layout_restyle_ms.unwrap_or(0.0) + restyle_ms);
            frame.layout_list_ms = Some(frame.layout_list_ms.unwrap_or(0.0) + list_ms);
            frame.display_list = Some(frame.display_list.unwrap_or(false) || display_list);
        },
        Line::SwglWindow { .. } => {},
    }
}

/// The run's own log, read from the byte this process has already seen. The engine's diagnostics come from
/// threads this harness does not own, so they are read back rather than passed along.
pub struct Tail {
    path: PathBuf,
    /// The bytes read so far.
    seen: u64,
    /// A line the log has only the beginning of.
    part: String,
}

impl Tail {
    pub fn new(path: &Path) -> Tail {
        Tail { path: path.to_path_buf(), seen: 0, part: String::new() }
    }

    /// The whole lines written since the last call. An unreadable log is no lines: the benchmark's own numbers
    /// do not depend on it.
    pub fn lines(&mut self) -> Vec<String> {
        let Ok(mut file) = std::fs::File::open(&self.path) else { return Vec::new() };
        let Ok(size) = file.metadata().map(|m| m.len()) else { return Vec::new() };
        if size < self.seen {
            // Truncated and started again.
            self.seen = 0;
            self.part.clear();
        }
        if size <= self.seen {
            return Vec::new();
        }
        let mut buf = vec![0u8; (size - self.seen).min(8 << 20) as usize];
        use std::io::{Seek, SeekFrom};
        if file.seek(SeekFrom::Start(self.seen)).is_err() || file.read_exact(&mut buf).is_err() {
            return Vec::new();
        }
        self.seen += buf.len() as u64;
        self.part.push_str(&String::from_utf8_lossy(&buf));
        let mut whole: Vec<String> = self.part.split('\n').map(str::to_string).collect();
        self.part = whole.pop().unwrap_or_default();
        whole.retain(|line| !line.is_empty());
        whole
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spec_reads_steps_and_a_delta() {
        assert_eq!(parse_spec(Some("60x120")), Some((60, 120.0)));
        assert_eq!(parse_spec(Some(" 12 x 88.5 ")), Some((12, 88.5)));
        assert_eq!(parse_spec(Some("40X9")), Some((40, 9.0)));
        assert_eq!(parse_spec(Some("25")), Some((25, 120.0)));
    }

    #[test]
    fn anything_else_leaves_the_bench_off() {
        for text in [None, Some(""), Some("0x120"), Some("60x"), Some("x120"), Some("no"), Some("60x0"), Some("60x-4"), Some("60xnan")] {
            assert_eq!(parse_spec(text), None, "{text:?}");
        }
    }

    #[test]
    fn the_scale_defaults_to_one_and_refuses_nonsense() {
        assert_eq!(parse_scale(Some("2")), 2.0);
        assert_eq!(parse_scale(Some(" 1.5 ")), 1.5);
        for text in [None, Some(""), Some("0"), Some("-2"), Some("no"), Some("inf")] {
            assert_eq!(parse_scale(text), 1.0, "{text:?}");
        }
    }

    #[test]
    fn a_percentile_sits_between_the_nearest_samples() {
        let v = [1.0, 2.0, 3.0, 4.0, 10.0];
        assert_eq!(percentile(&v, 0.0), 1.0);
        assert_eq!(percentile(&v, 1.0), 10.0);
        assert_eq!(percentile(&v, 0.5), 3.0);
        // Nine tenths of the way from the 4th sample to the 5th: 4 + 0.6 * 6.
        assert!((percentile(&v, 0.9) - 7.6).abs() < 1e-9, "{}", percentile(&v, 0.9));
        assert_eq!(percentile(&[], 0.5), 0.0);
        assert_eq!(percentile(&[7.0], 0.9), 7.0);
    }

    #[test]
    fn the_counts_of_a_column_name_the_slow_frames() {
        let s = stats(&[10.0, 20.0, 40.0]);
        assert_eq!((s.n, s.p50, s.max), (3, 20.0, 40.0));
        assert_eq!(s.mean, 70.0 / 3.0);
        assert_eq!((s.over_nominal, s.over_slow), (2, 1));
        assert_eq!(stats(&[]).n, 0);
    }

    #[test]
    fn a_frame_of_several_documents_sums_their_render_lines() {
        // A page with frames in it renders one WebRender document per frame in it, and each prints its own
        // `wr: update` and `wr-phases`. Last-wins would have charged the frame with only the last document's
        // work: on Wikipedia that made 1.9 Mpx of tile raster in a frame read as none at all.
        let mut frame = Frame::default();
        for line in [
            "wr: update 0 render 9 ms, draws 0 color 1 alpha 0 frame_build 0.1 upload 0.000 MB 9.5 ms full_paint false",
            "wr-phases cache 15.8 tiles 17 103.1 ms 5189 kpx, alpha 0.5, color 6.2 ms 83 kpx, composite 0.0 ms",
            "wr: update 0 render 188 ms, draws 61 color 19 alpha 1 frame_build 20.7 upload 0.070 MB 0.3 ms full_paint true",
            "wr-phases cache 0.0 tiles 0 0.0 ms 0 kpx, alpha 0.0, color 0.0 ms 0 kpx, composite 0.0 ms",
            "layout-phases restyle+trees 8.24 ms, stacking contexts and display list 19.41 ms, ran Reflow | Nothing",
            "layout-phases restyle+trees 1.20 ms, stacking contexts and display list 0.30 ms, ran Reflow | BuiltDisplayList",
        ] {
            if let Some(parsed) = parse_line(line) {
                fold(&mut frame, parsed);
            }
        }
        assert_eq!(frame.wr_docs, Some(2), "two documents rendered this frame");
        assert_eq!(frame.wr_render_ms, Some(197.0), "{:?}", frame.wr_render_ms);
        assert_eq!(frame.wr_frame_build_ms, Some(20.8), "{:?}", frame.wr_frame_build_ms);
        assert_eq!(frame.wr_upload_ms, Some(9.8), "{:?}", frame.wr_upload_ms);
        assert_eq!(frame.wr_draws, Some(61), "{:?}", frame.wr_draws);
        assert_eq!(frame.wr_tiles, Some(17), "{:?}", frame.wr_tiles);
        assert_eq!(frame.wr_tiles_ms, Some(103.1), "{:?}", frame.wr_tiles_ms);
        assert_eq!(frame.wr_tiles_kpx, Some(5189), "{:?}", frame.wr_tiles_kpx);
        // One document said the whole window, which is a full composite of the frame.
        assert_eq!(frame.wr_full_paint, Some(true), "{:?}", frame.wr_full_paint);
        assert_eq!(frame.layout_restyle_ms, Some(9.44), "{:?}", frame.layout_restyle_ms);
        assert_eq!(frame.display_list, Some(true), "{:?}", frame.display_list);
    }

    #[test]
    fn the_stats_line_carries_the_frame_build_and_the_upload() {
        // These two were in the log all along and no column read them, which is why 27 ms of a 32 ms paint looked
        // like time nobody accounted for.
        let line = parse_line("wr: update 0 render 188 ms, draws 61 color 19 alpha 1 frame_build 20.7 upload 0.070 MB 0.3 ms full_paint true");
        assert_eq!(
            line,
            Some(Line::Wr {
                update_ms: 0.0,
                render_ms: 188.0,
                draws: 61,
                frame_build_ms: 20.7,
                upload_mb: 0.070,
                upload_ms: 0.3,
                full_paint: true
            })
        );
    }

    #[test]
    fn a_wr_phases_line_reads_across_its_repeated_words() {
        let line = "wr-phases cache 0.2 tiles 1 0.7 ms 152 kpx, alpha 0.0, color 3.1 ms 7590 kpx, composite 0.4 ms";
        assert_eq!(
            parse_line(line),
            Some(Line::WrPhases {
                cache_ms: 0.2,
                tiles: 1,
                tiles_ms: 0.7,
                tiles_kpx: 152,
                alpha_ms: 0.0,
                color_ms: 3.1,
                color_kpx: 7590,
                composite_ms: 0.4,
            })
        );
        // A frame that re-rastered nothing still says the whole window was composited.
        assert_eq!(
            parse_line("wr-phases cache 0.0 tiles 0 0.0 ms 0 kpx, alpha 0.0, color 0.3 ms 7590 kpx, composite 0.3 ms"),
            Some(Line::WrPhases {
                cache_ms: 0.0,
                tiles: 0,
                tiles_ms: 0.0,
                tiles_kpx: 0,
                alpha_ms: 0.0,
                color_ms: 0.3,
                color_kpx: 7590,
                composite_ms: 0.3,
            })
        );
    }

    #[test]
    fn the_wr_stats_line_gives_the_same_frame_in_whole_milliseconds() {
        assert_eq!(
            parse_line("wr: update 0 render 1 ms, draws 12 color 1 alpha 0 frame_build 0.0 upload 0.000 MB 0.0 ms full_paint true"),
            Some(Line::Wr { update_ms: 0.0, render_ms: 1.0, draws: 12, frame_build_ms: 0.0, upload_mb: 0.0, upload_ms: 0.0, full_paint: true })
        );
        assert_eq!(
            parse_line("wr: update 3 render 40 ms, draws 1455 color 4 alpha 1 frame_build 2.1 upload 0.512 MB 0.3 ms full_paint false"),
            Some(Line::Wr { update_ms: 3.0, render_ms: 40.0, draws: 1455, frame_build_ms: 2.1, upload_mb: 0.512, upload_ms: 0.3, full_paint: false })
        );
    }

    #[test]
    fn a_layout_line_says_whether_a_display_list_was_built() {
        assert_eq!(
            parse_line("layout-phases restyle+trees 1.20 ms, stacking contexts and display list 0.30 ms, ran RanLayout | BuiltStackingContextTree | BuiltDisplayList"),
            Some(Line::LayoutPhases { restyle_ms: 1.2, list_ms: 0.3, display_list: true })
        );
        assert_eq!(
            parse_line("layout-phases restyle+trees 0.05 ms, stacking contexts and display list 0.00 ms, ran UpdatedScrollNodeOffset"),
            Some(Line::LayoutPhases { restyle_ms: 0.05, list_ms: 0.0, display_list: false })
        );
    }

    #[test]
    fn the_rasterizers_window_line_adds_up() {
        assert_eq!(parse_line("swgl-prof window 2010 ms, swgl 431.2 ms"), Some(Line::SwglWindow { window_ms: 2010.0, swgl_ms: 431.2 }));
    }

    #[test]
    fn lines_that_are_not_diags_are_left_alone() {
        for line in [
            "console log: hi",
            "render: complete true in 1531 ms, rss 79372 KiB",
            "phase main",
            // A shader row of the same profile, and a window line with nothing in it.
            "swgl-prof ps_quad_gradient blend 2 calls 12 inst 1 px 1024 ms 0.10",
            "swgl-prof window",
            "wr-phases cache 0.2 tiles",
            "wr: update",
            "layout-phases restyle+trees 1.20 ms,",
            "",
        ] {
            assert_eq!(parse_line(line), None, "{line}");
        }
    }

    #[test]
    fn a_trailing_cr_is_not_part_of_a_number() {
        assert_eq!(parse_line("wr-phases cache 0.2 tiles 1 0.7 ms 152 kpx, alpha 0.0, color 3.1 ms 7590 kpx, composite 0.4 ms\r"),
            Some(Line::WrPhases { cache_ms: 0.2, tiles: 1, tiles_ms: 0.7, tiles_kpx: 152, alpha_ms: 0.0, color_ms: 3.1, color_kpx: 7590, composite_ms: 0.4 }));
    }

    #[test]
    fn a_fold_adds_the_reflows_of_one_frame_and_keeps_the_last_paint() {
        let mut frame = Frame::default();
        fold(&mut frame, Line::LayoutPhases { restyle_ms: 1.5, list_ms: 0.5, display_list: true });
        fold(&mut frame, Line::LayoutPhases { restyle_ms: 0.25, list_ms: 0.0, display_list: false });
        fold(&mut frame, Line::Wr { update_ms: 1.0, render_ms: 4.0, draws: 90, frame_build_ms: 0.4, upload_mb: 0.0, upload_ms: 0.0, full_paint: false });
        assert_eq!(frame.layout_restyle_ms, Some(1.75));
        assert_eq!(frame.layout_list_ms, Some(0.5));
        assert_eq!(frame.display_list, Some(true));
        assert_eq!(frame.wr_render_ms, Some(4.0));
    }

    #[test]
    fn each_frame_gets_the_lines_of_its_own_window() {
        let mut frames = vec![Frame::default(), Frame::default()];
        attribute(
            &mut frames,
            vec![
                vec![Line::WrPhases {
                    cache_ms: 0.0,
                    tiles: 3,
                    tiles_ms: 1.0,
                    tiles_kpx: 400,
                    alpha_ms: 0.0,
                    color_ms: 2.0,
                    color_kpx: 5000,
                    composite_ms: 0.5,
                }],
                vec![Line::LayoutPhases { restyle_ms: 9.0, list_ms: 4.0, display_list: true }],
            ],
        );
        assert_eq!(frames[0].wr_tiles, Some(3));
        assert_eq!(frames[1].wr_tiles, None);
        assert_eq!(frames[1].layout_restyle_ms, Some(9.0));
    }

    fn frames() -> Vec<Frame> {
        vec![
            Frame {
                step: Some(0),
                interval_ms: Some(15.0),
                since_step_ms: Some(12.0),
                paint_ms: 11.0,
                present_ms: 1.0,
                copy_ms: 2.0,
                wr_tiles_kpx: Some(150),
                wr_color_kpx: Some(7590),
                wr_full_paint: Some(true),
                display_list: Some(false),
                ..Default::default()
            },
            Frame {
                step: Some(1),
                interval_ms: Some(40.0),
                since_step_ms: Some(38.0),
                paint_ms: 35.0,
                present_ms: 1.0,
                copy_ms: 2.0,
                wr_tiles_kpx: Some(150),
                wr_color_kpx: Some(0),
                wr_full_paint: Some(false),
                display_list: Some(false),
                ..Default::default()
            },
        ]
    }

    fn meta() -> Meta {
        Meta {
            url: "file:///tmp/long.html".to_string(),
            steps: 2,
            delta_css_px: 120.0,
            scale: 2.0,
            width: 3480,
            height: 2180,
            paced: true,
            raster: "rust".to_string(),
            raster_threads: "8".to_string(),
            layout_diag: true,
            log: None,
        }
    }

    #[test]
    fn the_table_names_every_stage_it_has_a_number_for() {
        let text = table(&frames(), &meta(), 0, 0);
        assert!(
            text.starts_with("scroll-bench 2 steps x 120 CSS px at scale 2.00, window 3480x2180 device px (1740x1090 CSS), paced to 16.7 ms\n"),
            "{text}"
        );
        assert!(text.contains("scroll-bench page file:///tmp/long.html rasterizer rust (threads 8)\n"), "{text}");
        assert!(text.contains("frames: 2 of the 2 wanted, from 0 wheel steps; 0 of those steps were merged"), "{text}");
        for want in ["frame interval", "scroll to present", "paint: wr update+render", "present: swgl flush", "copy into page"] {
            assert!(text.contains(want), "missing {want}:\n{text}");
        }
        // A stage the log never said is not printed at all.
        assert!(!text.contains("wr composite"), "{text}");
        // The two intervals are 15 and 40: p50 27.5, one over each budget.
        let row = text.lines().find(|l| l.starts_with("frame interval")).expect(&text);
        assert!(
            row.contains("n    2") && row.contains("p50   27.50") && row.contains("over 16.7   1") && row.contains("over 33.3   1"),
            "{row}"
        );
    }

    #[test]
    fn a_count_column_never_counts_frame_budgets() {
        let text = table(&[Frame { step: Some(0), interval_ms: Some(1.0), wr_tiles: Some(4), ..Default::default() }], &meta(), 0, 0);
        let row = text.lines().find(|l| l.starts_with("cache: tiles redrawn")).expect(&text);
        assert!(row.contains("count"), "{row}");
        assert!(!row.contains("over 16.7"), "{row}");
    }

    #[test]
    fn the_redrawn_area_is_a_share_of_the_window() {
        let frames = frames();
        // Both frames re-rastered 150 kpx of a 3480 x 2180 window.
        let tiles = picked(&frames, |f| f.wr_tiles_kpx.map(|v| v as f64));
        assert!((mean_share_kpx(&tiles, meta().window_px()) - 1.98).abs() < 0.01, "{tiles:?}");
        // One full window and one empty frame of colour pass is half the window.
        let color = picked(&frames, |f| f.wr_color_kpx.map(|v| v as f64));
        assert!((mean_share_kpx(&color, meta().window_px()) - 50.02).abs() < 0.02, "{color:?}");
        assert_eq!(mean_share_kpx(&[], 1000), 0.0);
        assert_eq!(mean_share_kpx(&[10.0], 0), 0.0);
        let text = table(&frames, &meta(), 0, 0);
        assert!(text.contains("re-rastered on average, colour pass 50.0 %; 1 frames of 2"), "{text}");
    }

    #[test]
    fn async_scroll_counts_lists_against_stepped_frames() {
        assert_eq!(async_scroll(&frames()), (0, 2));
        let mut one = frames();
        one[0].display_list = Some(true);
        assert_eq!(async_scroll(&one), (1, 2));
        assert!(
            table(&frames(), &meta(), 0, 0).contains("2 frames followed a step; 2 of them reported a reflow and none built a display list"),
            "no list: {}",
            table(&frames(), &meta(), 0, 0)
        );
        let mut every = frames();
        every[0].display_list = Some(true);
        every[1].display_list = Some(true);
        assert!(
            table(&every, &meta(), 0, 0).contains("2 of the 2 frames that followed a step rebuilt a display list"),
            "one per frame: {}",
            table(&every, &meta(), 0, 0)
        );
        let mut blind = meta();
        blind.layout_diag = false;
        assert!(
            table(&every, &blind, 0, 0).contains("which proves nothing: LUCINANET_LAYOUT_PHASES was off"),
            "blind: {}",
            table(&every, &blind, 0, 0)
        );
        // Stepped frames are frames that followed a step, so a run whose frames came after the last step says nothing.
        assert!(
            async_evidence(&[Frame { step: None, ..Default::default() }], &meta()).contains("no frame was presented"),
            "{}",
            async_evidence(&[Frame { step: None, ..Default::default() }], &meta())
        );
        let silent = vec![Frame { step: Some(0), ..Default::default() }];
        assert!(
            async_evidence(&silent, &meta()).contains("none reported a reflow at all"),
            "{}",
            async_evidence(&silent, &meta())
        );
        assert_eq!(async_scroll(&[]), (0, 0));
    }

    #[test]
    fn the_json_carries_the_frames_and_the_counts() {
        let text = json(&frames(), &meta(), 4, 1, &[(2010.0, 431.2)]);
        assert!(
            text.starts_with("{\n  \"meta\": {\"url\": \"file:///tmp/long.html\", \"steps\": 2, \"delta_css_px\": 120.000, \"scale\": 2.000, \"device_px\": [3480, 2180], \"css_px\": [1740, 1090], \"paced\": true"),
            "{text}"
        );
        assert!(text.contains("\"merged_steps\": 1"), "{text}");
        assert!(text.contains("\"full_paint_frames\": 1"), "{text}");
        assert!(text.contains("\"window_device_px\": 7586400"), "{text}");
        assert!(text.contains("\"swgl_ms\": 431.2"), "{text}");
        assert!(text.contains("\"frame interval: ms\": {\"unit\": \"ms\", \"n\": 2, \"p50\": 27.500"), "{text}");
        assert!(text.contains("\"step\": 0"), "{text}");
        assert!(text.contains("\"since_step_ms\": 12.000"), "{text}");
        assert!(text.contains("\"wr_full_paint\": true"), "{text}");
        // Stages the frame does not have are absent, not zero.
        assert!(!text.contains("wr_tiles_ms"), "{text}");
        assert!(text.ends_with("  ]\n}\n"), "{text}");
        check_json(&text);
    }

    #[test]
    fn the_json_of_no_frames_is_still_json() {
        let mut meta = meta();
        meta.log = Some("scroll-bench-out/run.log".into());
        let text = json(&[], &meta, 0, 0, &[]);
        assert!(text.contains("\"frames\": 0"), "{text}");
        assert!(text.contains("\"log\": \"scroll-bench-out/run.log\""), "{text}");
        assert!(text.contains("\"stats\": {\n  },"), "{text}");
        assert!(text.ends_with("  ]\n}\n"), "{text}");
        check_json(&text);
    }

    #[test]
    fn json_strings_are_escaped() {
        let mut meta = meta();
        meta.url = "a \"quoted\" \\ path".to_string();
        check_json(&json(&frames(), &meta, 0, 0, &[]));
        assert!(json(&[], &meta, 0, 0, &[]).contains("\"url\": \"a \\\"quoted\\\" \\\\ path\""));
    }

    /// The JSON the benchmark writes has to be readable, and nothing else in the workspace is a JSON parser
    /// at this point: a small one that only has to accept what `json` emits, to catch a missing comma.
    fn check_json(text: &str) {
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escape = false;
        for c in text.chars() {
            if escape {
                escape = false;
                continue;
            }
            match c {
                '\\' if in_string => escape = true,
                '"' => in_string = !in_string,
                '{' | '[' if !in_string => depth += 1,
                '}' | ']' if !in_string => depth -= 1,
                _ => {},
            }
        }
        assert_eq!(depth, 0, "unbalanced: {text}");
        assert!(!in_string, "unterminated string: {text}");
        // Every value of an object or array is followed by a comma or the close of its parent.
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let end = line.trim_end();
            assert!(
                end.ends_with('{') || end.ends_with('[') || end.ends_with(',') || end.ends_with('}') || end.ends_with(']'),
                "line does not end in a separator: {line}"
            );
        }
    }

    #[test]
    fn the_tail_reads_only_what_was_written_since_last_time() {
        let path = temp_log("tail");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let mut tail = Tail::new(&path);
        assert_eq!(tail.lines(), vec!["one", "two"]);
        assert!(tail.lines().is_empty());
        append(&path, "three\nfo");
        assert_eq!(tail.lines(), vec!["three"]);
        // A line the log has only the start of waits for the rest of it.
        append(&path, "ur\n");
        assert_eq!(tail.lines(), vec!["four"]);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_missing_or_restarted_log_is_no_lines_or_the_new_ones() {
        let mut tail = Tail::new(Path::new("/nonexistent/scrollbench.log"));
        assert!(tail.lines().is_empty());
        let path = temp_log("cut");
        std::fs::write(&path, b"0123456789").unwrap();
        let mut tail = Tail::new(&path);
        assert_eq!(tail.lines(), Vec::<String>::new());
        std::fs::write(&path, "again\n").unwrap();
        assert_eq!(tail.lines(), vec!["again"]);
        std::fs::remove_file(&path).ok();
    }

    fn temp_log(what: &str) -> PathBuf {
        std::env::temp_dir().join(format!("scrollbench-{what}-{}.log", std::process::id()))
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(path).unwrap().write_all(text.as_bytes()).unwrap();
    }
}
