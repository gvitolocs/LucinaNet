//! One run of the benchmark: the json it wrote, read the way the analysis reads it.
//!
//! The bench's own frame records carry a column per stage of a frame, and the driver has changed
//! schema twice, so every column here is an option: a run of the pre-rewrite binary has no
//! `since_step_ms` (only `latency_ms`), no `inputs` and no `merged_steps`, and a run from before the
//! layout columns were folded in has no layout keys at all - which is a different fact from a run
//! whose page did no layout.

use std::path::Path;
use std::time::UNIX_EPOCH;

use serde_json::Value;

use crate::stats;

/// The timed columns a frame record can carry, in the order the bench names them.
pub const INTERVAL: usize = 0;
pub const SINCE_STEP: usize = 1;
pub const LATENCY: usize = 2;
pub const PAINT: usize = 3;
pub const PRESENT: usize = 4;
pub const COPY: usize = 5;
pub const LAYOUT_RESTYLE: usize = 6;
pub const LAYOUT_LIST: usize = 7;
pub const WR_UPDATE: usize = 8;
pub const WR_RENDER: usize = 9;
pub const WR_FRAME_BUILD: usize = 10;
pub const WR_UPLOAD: usize = 11;
pub const WR_TILES_MS: usize = 12;
pub const WR_ALPHA: usize = 13;
pub const WR_COLOR: usize = 14;
pub const WR_COMPOSITE: usize = 15;
pub const WR_CACHE: usize = 16;
pub const COLUMNS: usize = 17;

const NAMES: [(&str, usize); COLUMNS] = [
    ("interval_ms", INTERVAL),
    ("since_step_ms", SINCE_STEP),
    ("latency_ms", LATENCY),
    ("paint_ms", PAINT),
    ("present_ms", PRESENT),
    ("copy_ms", COPY),
    ("layout_restyle_ms", LAYOUT_RESTYLE),
    ("layout_list_ms", LAYOUT_LIST),
    ("wr_update_ms", WR_UPDATE),
    ("wr_render_ms", WR_RENDER),
    ("wr_frame_build_ms", WR_FRAME_BUILD),
    ("wr_upload_ms", WR_UPLOAD),
    ("wr_tiles_ms", WR_TILES_MS),
    ("wr_alpha_ms", WR_ALPHA),
    ("wr_color_ms", WR_COLOR),
    ("wr_composite_ms", WR_COMPOSITE),
    ("wr_cache_ms", WR_CACHE),
];

/// One frame, as the json gave it: absent and null are the same thing here, because `f.get(field)`
/// answers `None` for both.
#[derive(Debug)]
pub struct Frame {
    pub times: [Option<f64>; COLUMNS],
    pub step: Option<i64>,
    pub draws: Option<i64>,
    pub docs: Option<f64>,
    pub tiles: Option<f64>,
    pub tiles_kpx: Option<f64>,
    pub full_paint: bool,
    pub list_built: bool,
    /// Whether the record carries a layout column at all, as distinct from carrying a zero one.
    pub carries_layout: bool,
}

/// What one frame moved and what it redrew, per `frame_need`.
pub struct Need {
    /// The steps that arrived since the frame before.
    pub steps: i64,
    /// The strip those steps uncovered, as a share of the window.
    pub need: f64,
    /// The area WebRender said it re-rastered, as a share of the same window.
    pub tile: f64,
    pub paint: Option<f64>,
    /// Whether the frame reported a tile area at all.
    pub measured: bool,
}

/// What a finished block of a job log says about the machine a run went round on.
#[derive(Clone, Copy, Debug, Default)]
pub struct Event {
    pub load_before: f64,
    pub wall_ms: u64,
}

#[derive(Debug)]
pub struct Run {
    /// The run directory, which is what names the cell: page-scale-raster-pace[-run].
    pub dir: String,
    /// The matrix directory the run came from: m1 is the pre-rewrite driver.
    pub src: String,
    /// When its json was written, seconds since midnight, which is what meets the job log.
    pub mtime: i64,
    pub page: String,
    pub scale: String,
    pub raster: String,
    pub pace: String,
    pub rep: String,
    pub frames: Vec<Frame>,
    pub n: usize,
    pub inputs: Option<Value>,
    pub merged: Option<Value>,
    pub width: f64,
    pub height: f64,
    pub window: f64,
    pub delta: f64,
    pub async_evidence: String,
    pub measures_layout: bool,
    /// The log block whose clock window holds `mtime`, or None where no block claimed the run: the
    /// load and wall columns of the first table then read `-`.
    pub event: Option<Event>,
}

impl Run {
    /// Reads one `json/*.json`. The path is only kept for the message: the names come from the
    /// directory, because the driver renamed the directories after some runs and the json files were
    /// left with the old ones.
    pub fn read(path: &Path, dir: &str, src: &str) -> Result<Run, String> {
        let bad = |what: std::fmt::Arguments<'_>| format!("{}: {what}", path.display());
        let text = std::fs::read(path).map_err(|e| bad(format_args!("{e}")))?;
        let doc: Value = serde_json::from_slice(&text).map_err(|e| bad(format_args!("{e}")))?;
        let mtime = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .map_err(|e| bad(format_args!("{e}")))?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
            % 86400;
        Self::parse(&doc, &path.display().to_string(), dir, src, mtime)
    }

    /// Everything the run's json says about it, the two names the paths give it, and the second its json
    /// was written at. Split out so that a frame record can be handed to a test without a filesystem.
    pub fn parse(doc: &Value, name: &str, dir: &str, src: &str, mtime: i64) -> Result<Run, String> {
        let bad = |what: std::fmt::Arguments<'_>| format!("{name}: {what}");
        let missing = |what: &str| bad(format_args!("no {what}"));
        let meta = doc.get("meta").ok_or_else(|| missing("meta"))?;
        let summary = doc.get("summary").ok_or_else(|| missing("summary"))?;
        let json_frames = doc.get("frames").and_then(Value::as_array).ok_or_else(|| missing("frames"))?;
        // Both are indexed, not fetched, by the analysis: a run whose json has neither has no area to
        // divide a frame by, and naming the file beats printing `inf` in a column.
        let device =
            meta.get("device_px").and_then(Value::as_array).ok_or_else(|| missing("meta.device_px"))?;
        let [width, height] = <[f64; 2]>::try_from(
            device.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect::<Vec<f64>>(),
        )
        .map_err(|_| bad(format_args!("meta.device_px is not two numbers")))?;
        let window = summary
            .get("window_device_px")
            .and_then(Value::as_f64)
            .ok_or_else(|| missing("summary.window_device_px"))?
            / 1000.0;

        let parts: Vec<&str> = dir.split('-').collect();
        if parts.len() < 4 {
            return Err(format!("{dir}: a run directory names page-scale-raster-pace, and this one does not"));
        }
        let frames = json_frames.iter().map(frame).collect::<Vec<Frame>>();
        let measures_layout =
            doc.get("inputs").is_some_and(|v| !v.is_null()) || frames.iter().any(|f| f.carries_layout);
        // `%s` of what the json gave, so that a count written as 120 prints 120 and one written as a
        // float keeps its `.0`.
        let inputs = match doc.get("inputs") {
            Some(v) if !v.is_null() => Some(v.clone()),
            _ => meta.get("steps").filter(|v| !v.is_null()).cloned(),
        };
        let merged =
            doc.get("merged_steps").or_else(|| doc.get("steps_without_frame")).map(Value::clone);
        let n = frames.len();
        Ok(Run {
            dir: dir.into(),
            src: src.into(),
            mtime,
            page: parts[0].into(),
            scale: parts[1].into(),
            raster: parts[2].into(),
            pace: parts[3].into(),
            rep: parts.get(4).unwrap_or(&"r1").to_string(),
            frames,
            n,
            merged,
            width,
            height,
            window,
            delta: or_falsy(meta.get("delta_css_px").and_then(Value::as_f64), 120.0)
                * or_falsy(meta.get("scale").and_then(Value::as_f64), 1.0),
            async_evidence: summary
                .get("async_evidence")
                .map(|v| match v.as_str() {
                    Some(s) => s.to_string(),
                    None => v.to_string(),
                })
                .unwrap_or_else(|| "-".into()),
            measures_layout,
            inputs,
            event: None,
        })
    }

    pub fn key(&self) -> (String, String, String, String) {
        (self.page.clone(), self.scale.clone(), self.raster.clone(), self.pace.clone())
    }

    /// One column down the frames, with the two readings the old schema needs: `latency_ms` stands in
    /// for `since_step_ms`, and the pre-rewrite driver wrote the first frame's interval as 0.0 where
    /// the rewrite leaves it out.
    pub fn column(&self, field: usize) -> Vec<Option<f64>> {
        let alt = if field == SINCE_STEP { Some(LATENCY) } else { None };
        self.frames
            .iter()
            .map(|f| {
                let v = match f.times[field] {
                    Some(v) => Some(v),
                    None => alt.and_then(|a| f.times[a]),
                };
                if field == INTERVAL && v == Some(0.0) { None } else { v }
            })
            .collect()
    }

    pub fn stat(&self, field: usize, p: f64) -> Option<f64> {
        stats::percentile(&self.values(field), p)
    }

    pub fn median(&self, field: usize) -> Option<f64> {
        stats::median(&self.values(field))
    }

    /// One column with the missing frames left out, which is what a percentile and a maximum are taken
    /// over when the analysis wants the last frame's value rather than a median of the ones it has.
    pub fn values(&self, field: usize) -> Vec<f64> {
        self.column(field).into_iter().flatten().collect()
    }

    /// The tiles a frame re-rastered, as a share of the window: `100 * mean(kpx) / (window_device_px / 1000)`,
    /// which is the mean the engine's own `kpx` column answers with rather than a re-derivation of it.
    pub fn area_pct(&self) -> Option<f64> {
        let kpx: Vec<f64> = self.frames.iter().filter_map(|f| f.tiles_kpx).collect();
        if kpx.is_empty() || self.window == 0.0 {
            return None;
        }
        Some(100.0 * stats::mean(&kpx)? / self.window)
    }

    pub fn docs(&self) -> Option<f64> {
        stats::median(&self.frames.iter().filter_map(|f| f.docs).collect::<Vec<f64>>())
    }

    pub fn full_paint(&self) -> usize {
        self.frames.iter().filter(|f| f.full_paint).count()
    }

    pub fn layout_frames(&self) -> usize {
        self.frames
            .iter()
            .filter(|f| {
                f.times[LAYOUT_RESTYLE].unwrap_or(0.0) > 0.0 || f.times[LAYOUT_LIST].unwrap_or(0.0) > 0.0
            })
            .count()
    }

    pub fn list_frames(&self) -> usize {
        self.frames.iter().filter(|f| f.list_built).count()
    }

    /// The frames that had nothing to draw - zero or one draw call - and how long a render of that
    /// took. The count is the column; the median is the one the table prints over the runs of a cell.
    pub fn empty_render(&self) -> (Option<f64>, usize) {
        let v: Vec<f64> = self
            .frames
            .iter()
            .filter(|f| matches!(f.draws, Some(0) | Some(1)))
            .filter_map(|f| f.times[WR_RENDER])
            .collect();
        if v.is_empty() {
            (None, 0)
        } else {
            (stats::median(&v), v.len())
        }
    }

    /// Per frame: the steps that came since the frame before, the strip they uncovered and the area
    /// re-rastered as shares of the window, the paint, and whether the frame reported an area at all.
    ///
    /// The step index is the only record of how far the wheel went between two frames, so `n` steps is
    /// `n * delta` CSS px moved, clipped to the window because a page at the bottom of its scroll
    /// uncovers nothing however many steps arrive.
    pub fn frame_need(&self) -> Vec<Need> {
        let win = self.width * self.height;
        let mut out = Vec::with_capacity(self.n);
        let mut prev = 0i64;
        for f in &self.frames {
            let s = f.step.unwrap_or(prev);
            let n = (s - prev).max(0);
            if f.step.is_some() {
                prev = f.step.unwrap();
            }
            out.push(Need {
                steps: n,
                need: 100.0 * ((n as f64 * self.delta).min(self.height)) * self.width / win,
                tile: 100.0 * f.tiles_kpx.unwrap_or(0.0) * 1000.0 / win,
                paint: f.times[PAINT],
                measured: f.tiles_kpx.is_some(),
            });
        }
        out
    }
}

fn frame(json: &Value) -> Frame {
    let mut times = [None; COLUMNS];
    for (name, at) in NAMES {
        times[at] = json.get(name).and_then(Value::as_f64);
    }
    Frame {
        times,
        step: json.get("step").and_then(Value::as_i64),
        draws: json.get("wr_draws").and_then(Value::as_i64),
        docs: json.get("wr_docs").and_then(Value::as_f64),
        tiles: json.get("wr_tiles").and_then(Value::as_f64),
        tiles_kpx: json.get("wr_tiles_kpx").and_then(Value::as_f64),
        full_paint: truthy(json.get("wr_full_paint")),
        list_built: truthy(json.get("display_list")),
        carries_layout: json.get("layout_restyle_ms").is_some() || json.get("layout_list_ms").is_some(),
    }
}

/// Python's truthiness for the two flags, which the json gives as booleans.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|v| v != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        _ => false,
    }
}

/// `(meta.get(k) or default)` for a float: the bench's zero means the same nothing as a missing key.
fn or_falsy(v: Option<f64>, default: f64) -> f64 {
    v.filter(|v| *v != 0.0).unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::fmt;

    /// A run of 1000 by 1000 device pixels and a 120 CSS px step at scale 2, so that every share asserted
    /// below is a number a reader can work out in their head: `need` comes out at a tenth of the strip the
    /// steps moved, and `tile` at a tenth of the kpx WebRender reported.
    fn run(frames: Value) -> Run {
        let doc = serde_json::json!({
            "meta": {"device_px": [1000, 1000], "scale": 2.0, "delta_css_px": 120.0, "steps": 60},
            "summary": {"window_device_px": 1_000_000, "async_evidence": "APZ: consumed 12 of 60 steps"},
            "frames": frames,
        });
        Run::parse(&doc, "test.json", "long-s2-rust-vsync-r1", "m2", 3600).unwrap()
    }

    /// The same column printed as the tables would print it, for the assertions that are about order.
    fn printed(v: &[f64], digits: usize) -> Vec<String> {
        v.iter().map(|x| fmt::fixed(*x, digits)).collect()
    }

    #[test]
    fn the_directory_names_the_cell_and_the_json_names_the_counts() {
        let r = run(Value::Array(vec![]));
        assert_eq!(
            (r.page.as_str(), r.scale.as_str(), r.raster.as_str(), r.pace.as_str(), r.rep.as_str()),
            ("long", "s2", "rust", "vsync", "r1")
        );
        assert_eq!(r.src, "m2");
        assert_eq!(r.mtime, 3600, "the second its json was written, which is how it meets the job log");
        // No `inputs` key at all, so the plan's step count stands in for the steps the driver sent.
        assert_eq!(fmt::shown(r.inputs.as_ref()), "60");
        assert_eq!(fmt::shown(r.merged.as_ref()), "-");
        assert_eq!(r.async_evidence, "APZ: consumed 12 of 60 steps");
        assert_eq!(r.delta, 240.0, "120 CSS px of a step, at scale 2");
        assert_eq!(r.key(), ("long".into(), "s2".into(), "rust".into(), "vsync".into()));
    }

    #[test]
    fn a_json_that_cannot_be_divided_up_is_an_error_that_names_its_file() {
        let doc = serde_json::json!({"meta": {}, "summary": {}, "frames": []});
        assert!(
            Run::parse(&doc, "test.json", "long-s2-rust-vsync", "m2", 0)
                .unwrap_err()
                .contains("test.json: no meta.device_px"),
            "the Python raised a KeyError here; a row of `inf` would be worse"
        );
        let doc = serde_json::json!({
            "meta": {"device_px": [1000, 1000]}, "summary": {}, "frames": []
        });
        assert!(Run::parse(&doc, "t", "long-s2-rust-vsync", "m2", 0)
            .unwrap_err()
            .contains("no summary.window_device_px"));
        let doc = serde_json::json!({"meta": {"device_px": [1, 1]}, "summary": {"window_device_px": 1}, "frames": []});
        assert!(Run::parse(&doc, "t", "long-s2", "m2", 0).unwrap_err().contains("names page-scale-raster-pace"));
    }

    #[test]
    fn an_interval_of_zero_is_no_interval_at_all() {
        // The pre-rewrite driver wrote the first frame's gap as 0.0 because there was no frame before it to
        // measure from; the rewrite leaves it out. Both readings have to drop it, or the p50 of a
        // sixty-frame run moves by a frame's worth.
        let r = run(serde_json::json!([
            {"interval_ms": 0.0, "paint_ms": 9.0},
            {"interval_ms": 10.0, "paint_ms": 11.0},
            {"interval_ms": 20.0, "paint_ms": 13.0},
            {"interval_ms": 30.0, "paint_ms": null},
        ]));
        assert_eq!(r.column(INTERVAL), vec![None, Some(10.0), Some(20.0), Some(30.0)]);
        assert_eq!(r.median(INTERVAL), Some(20.0));
        assert_eq!(r.stat(INTERVAL, 0.95), Some(29.0), "0.95 of the way from 20.0 to 30.0");
        assert_eq!(stats::percentile(&r.values(INTERVAL), 1.0), Some(30.0));
        assert_eq!(r.values(PAINT).len(), 3, "a null paint is not a frame's paint");
        assert_eq!(r.n, 4, "but it is still a frame, and the frames column counts them");
    }

    #[test]
    fn latency_stands_in_for_a_since_step_the_old_schema_never_wrote() {
        let r = run(serde_json::json!([
            {"since_step_ms": 4.0, "latency_ms": 99.0},
            {"since_step_ms": null, "latency_ms": 6.0},
            {"latency_ms": 8.0},
        ]));
        // A `null` is the same absence as no key at all, which is what the old runs leaned on.
        assert_eq!(r.column(SINCE_STEP), vec![Some(4.0), Some(6.0), Some(8.0)]);
        assert_eq!(r.median(SINCE_STEP), Some(6.0));
        assert_eq!(r.column(LATENCY), vec![Some(99.0), Some(6.0), Some(8.0)], "and not the other way about");
    }

    #[test]
    fn the_area_of_a_run_is_the_mean_of_its_tile_areas_over_the_window() {
        let r = run(serde_json::json!([
            {"wr_tiles_kpx": 5.0}, {"wr_tiles_kpx": 0.0}, {"wr_tiles_kpx": 12.0},
            {"wr_tiles_kpx": 7.0}, {"wr_tiles_kpx": 3.0},
        ]));
        assert_eq!(r.window, 1000.0, "the thousand device px the summary counts");
        // (5 + 0 + 12 + 7 + 3) / 5 is 5.4 thousand px, over a window of 1000 of them, times 100.
        assert_eq!(r.area_pct(), Some(0.54));
        assert_eq!(fmt::ms(r.area_pct(), 1), "0.5");
        assert_eq!(
            run(serde_json::json!([{"paint_ms": 1.0}])).area_pct(),
            None,
            "a run with no tile areas has no area, which is a dash and not a zero"
        );
    }

    #[test]
    fn a_frame_needs_what_its_steps_brought_on_screen() {
        let r = run(serde_json::json!([
            {"step": 0, "wr_tiles_kpx": 5.0, "paint_ms": 9.0},
            {"step": 1, "wr_tiles_kpx": 0.0, "paint_ms": 11.0},
            {"step": 5, "wr_tiles_kpx": 12.0, "paint_ms": 13.0},
            {"step": 5, "wr_tiles_kpx": 7.0, "paint_ms": 15.0},
            {"step": 12, "wr_tiles_kpx": 3.0, "paint_ms": null},
        ]));
        let need = r.frame_need();
        assert_eq!(need.iter().map(|f| f.steps).collect::<Vec<i64>>(), vec![0, 1, 4, 0, 7]);
        // One step is 240 device px of a 1000 px window, four are 960, and seven are clipped to the window
        // because a page at the bottom of its scroll uncovers nothing however far the wheel went.
        assert_eq!(printed(&need.iter().map(|f| f.need).collect::<Vec<f64>>(), 1),
            ["0.0", "24.0", "96.0", "0.0", "100.0"]);
        assert_eq!(printed(&need.iter().map(|f| f.tile).collect::<Vec<f64>>(), 1),
            ["0.5", "0.0", "1.2", "0.7", "0.3"]);
        assert!(need.iter().all(|f| f.measured));
        assert_eq!(need[4].paint, None);
    }

    #[test]
    fn the_count_of_a_step_is_measured_from_the_frame_before() {
        // A frame with no `step` key is the old schema's; the index it would have had is the one before it,
        // so it counts as a frame no step moved rather than one that rewound the page.
        let r = run(serde_json::json!([{"step": 4}, {"paint_ms": 1.0}, {"step": 9}]));
        assert_eq!(r.frame_need().iter().map(|f| f.steps).collect::<Vec<i64>>(), vec![4, 0, 5]);
    }

    #[test]
    fn layout_is_a_different_fact_from_a_zero_layout() {
        let r = run(serde_json::json!([
            {"layout_restyle_ms": 1.5, "layout_list_ms": 0.0, "display_list": true},
            {"layout_restyle_ms": 0.0, "layout_list_ms": 2.5, "display_list": false},
            {"layout_restyle_ms": 0.0, "layout_list_ms": 0.0},
        ]));
        assert!(r.measures_layout);
        assert_eq!(r.layout_frames(), 2, "a frame whose two layout columns are both zero did no layout");
        assert_eq!(r.list_frames(), 1, "and the flag is not the same question");

        let before_the_fold = run(serde_json::json!([{"paint_ms": 4.0}, {"paint_ms": 5.0}]));
        assert!(!before_the_fold.measures_layout, "a key that does not exist yet is not a zero");
        assert_eq!(before_the_fold.layout_frames(), 0);
    }

    #[test]
    fn the_flag_says_only_that_a_frame_was_rendered() {
        let r = run(serde_json::json!([
            {"wr_draws": 0, "wr_render_ms": 3.0, "wr_full_paint": true},
            {"wr_draws": 1, "wr_render_ms": 5.0, "wr_full_paint": true},
            {"wr_draws": 2, "wr_render_ms": 9.0, "wr_full_paint": true},
            {"wr_draws": 1, "wr_render_ms": null, "wr_full_paint": true},
        ]));
        assert_eq!(r.full_paint(), 4, "every frame of every page, which is the whole point of the column");
        let (median, count) = r.empty_render();
        assert_eq!(count, 2, "the two 0-or-1-draw frames that reported a render, not the third");
        assert_eq!(median, Some(4.0), "the median of 3.0 and 5.0");
        assert_eq!(r.docs(), None, "no `wr_docs` key at all");
    }

    #[test]
    fn docs_is_the_median_document_count_of_the_run() {
        let r = run(serde_json::json!([
            {"wr_docs": 1.0}, {"wr_docs": 3.0}, {"wr_docs": null}, {"wr_docs": 2.0}
        ]));
        assert_eq!(r.docs(), Some(2.0));
    }
}
