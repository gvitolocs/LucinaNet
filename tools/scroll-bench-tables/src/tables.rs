//! The six findings tables, in the order and under the headings the analysis prints them.
//!
//! Every cell is a Python `%` format gone through `crate::fmt`, and every row is a fact about a run
//! rather than a value the driver was asked to record: the first table is one row per run, the second
//! one row per configuration, the third divides a frame of each configuration into the containers it
//! spent its milliseconds in, and four through six answer what the first three cannot - what the frame
//! records say about layout, whether a scroll gets cheaper as its run goes on, and what a frame
//! re-rastered against the strip the steps brought on screen.

use std::collections::BTreeMap;


use crate::fmt;
use crate::joblog::{self, Block};
use crate::run::{
    Run, COPY, INTERVAL, PAINT, PRESENT, SINCE_STEP, WR_ALPHA, WR_CACHE, WR_COLOR, WR_COMPOSITE,
    WR_FRAME_BUILD, WR_RENDER, WR_TILES_MS, WR_UPDATE, WR_UPLOAD,
};
use crate::stats;

/// The stage columns, in the order a frame spends them: what layout did, what WebRender built, what was drawn.
const STAGES: [(&str, usize); 10] = [
    ("layout: restyle+trees", crate::run::LAYOUT_RESTYLE),
    ("layout: display list", crate::run::LAYOUT_LIST),
    ("wr update", WR_UPDATE),
    ("wr frame build", WR_FRAME_BUILD),
    ("wr upload", WR_UPLOAD),
    ("raster: tiles", WR_TILES_MS),
    ("cache: alpha", WR_ALPHA),
    ("colour pass", WR_COLOR),
    ("composite", WR_COMPOSITE),
    ("picture cache", WR_CACHE),
];

/// The phases `wr-phases` prints are WebRender's own breakdown of its render call; the layout lines are its own
/// breakdown of nothing in that call. Which container each one is inside is a question the data answers, not the
/// column order, so they are kept apart here.
const RASTER_PHASES: [&str; 7] = [
    "raster: tiles",
    "cache: alpha",
    "colour pass",
    "composite",
    "picture cache",
    "wr frame build",
    "wr upload",
];
const LAYOUT_STAGES: [&str; 2] = ["layout: restyle+trees", "layout: display list"];

/// The positions within a run that section 5 pools frames by, to ask whether a scroll gets cheaper once
/// WebRender's caches hold the tiles it is about to uncover.
const BUCKETS: [(usize, usize); 8] =
    [(1, 1), (2, 2), (3, 3), (4, 6), (7, 12), (13, 24), (25, 48), (49, 1_000_000_000)];

/// What the tables could not attribute, for the caller's note on stderr.
pub struct Found {
    /// The directories of the runs no job log gave a load line to.
    pub unmatched: Vec<String>,
}

/// One key of the tables: page, scale, rasterizer, pace, in the order `sorted(groups)` gives them.
type Cell = (String, String, String, String);

pub fn report(runs: &mut [Run], blocks: &[Block]) -> Found {
    let disagreed = joblog::assign(runs, blocks);
    let hit = runs.iter().filter(|r| r.event.is_some()).count();
    let unmatched: Vec<String> = runs.iter().filter(|r| r.event.is_none()).map(|r| format!("{}/{}", r.src, r.dir)).collect();

    // The groups are built in the order the runs were read, which is what makes the first run of a key
    // the one section 4 quotes its async line from; the key order a table iterates is sorted, which a
    // `BTreeMap` gives for free.
    let mut groups: BTreeMap<Cell, Vec<usize>> = BTreeMap::new();
    for (i, r) in runs.iter().enumerate() {
        groups.entry(r.key()).or_default().push(i);
    }

    let mut out = String::new();
    out.push_str(&format!(
        "{} runs read, {hit} matched to a load line by the clock, {disagreed} whose block disagreed with the directory\n",
        runs.len()
    ));
    frames(&mut out, runs);
    spread(&mut out, runs, &groups);
    division(&mut out, runs, &groups);
    layout_and_flags(&mut out, runs, &groups);
    warmup(&mut out, runs, &groups);
    uncovered(&mut out, runs, &groups);
    print!("{out}");
    Found { unmatched }
}

/// The rows of one configuration that divide a frame into stages: the rewritten driver's, and only the
/// runs of it that returned twenty frames or more.
fn real(runs: &[Run], group: &[usize]) -> Vec<usize> {
    group
        .iter()
        .copied()
        .filter(|&i| runs[i].src != "m1" && runs[i].n >= 20)
        .collect()
}

fn head(out: &mut String, header: &[&str]) {
    row(out, &header.iter().map(|s| s.to_string()).collect::<Vec<String>>());
    out.push('|');
    for _ in header {
        out.push_str("---|");
    }
    out.push('\n');
}

fn row(out: &mut String, cells: &[String]) {
    out.push_str("| ");
    out.push_str(&cells.join(" | "));
    out.push_str(" |\n");
}

fn ms(v: Option<f64>) -> String {
    fmt::ms(v, 1)
}

fn ms_at(v: Option<f64>, digits: usize) -> String {
    fmt::ms(v, digits)
}

fn p(part: Option<f64>, whole: Option<f64>) -> String {
    match (part, whole) {
        (Some(part), Some(whole)) if whole != 0.0 => fmt::fixed(100.0 * part / whole, 0) + "%",
        _ => "-".into(),
    }
}

/// `"%s (%s..%s)"` of the median, the least and the most: how a column of per-run medians says how far
/// the runs of one configuration agree. Section 2 prints the range even of a single run.
fn spread3(v: &[f64], digits: usize) -> String {
    if v.is_empty() {
        return "-".into();
    }
    format!(
        "{} ({}..{})",
        ms_at(stats::median(v), digits),
        ms_at(Some(least(v)), digits),
        ms_at(Some(greatest(v)), digits)
    )
}

/// The same for sections 5 and 6, where a bucket with one frame in it prints the one value and no range:
/// a spread of a single sample is a claim the row does not have.
fn range3(v: &[f64], digits: usize) -> String {
    match v.len() {
        0 => "-".into(),
        1 => ms_at(stats::median(v), digits),
        _ => spread3(v, digits),
    }
}

fn least(v: &[f64]) -> f64 {
    v.iter().fold(f64::INFINITY, |a, &b| a.min(b))
}

fn greatest(v: &[f64]) -> f64 {
    v.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b))
}

/// The median of the per-run medians of one column, which is how every pooled cell of sections 2, 3
/// and 4 is taken: a run of three frames does not outvote a run of sixty.
pub(crate) fn median_of_medians(runs: &[Run], group: &[usize], field: usize) -> Option<f64> {
    stats::median(&group.iter().filter_map(|&i| runs[i].median(field)).collect::<Vec<f64>>())
}

/// `iv/paint`, the last column of section 2: what share of a frame the paint accounts for, taken over
/// the same two medians the two columns to its left print. A run whose paint measured nothing, or a
/// paint that came out at zero, leaves the cell a dash rather than a division by nothing.
fn share(iv: &[f64], paint: &[f64]) -> String {
    match (stats::median(iv), stats::median(paint)) {
        _ if iv.is_empty() || paint.is_empty() => "-".into(),
        (Some(iv), Some(paint)) if paint != 0.0 => fmt::fixed(iv / paint, 2),
        _ => "-".into(),
    }
}

/// ## 1 - one row per run: what it returned, what a frame cost, and the load the machine was under.
fn frames(out: &mut String, runs: &[Run]) {
    out.push_str("## 1. frames returned, and the frame time, one row per run\n\n");
    head(
        out,
        &[
            "page", "s", "raster", "pace", "run", "frames", "steps", "merged", "iv p50", "iv p95", "iv max",
            "lat p50", "paint p50", "paint p95", "paint max", "copy p50", "copy p95", "present", "area %",
            "docs", "load", "wall s", "set",
        ],
    );
    let mut order: Vec<usize> = (0..runs.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&runs[a], &runs[b]);
        (&a.page, &a.scale, &a.raster, &a.pace, &a.src, &a.rep)
            .cmp(&(&b.page, &b.scale, &b.raster, &b.pace, &b.src, &b.rep))
    });
    for i in order {
        let r = &runs[i];
        let area = r.area_pct().map(|v| fmt::fixed(v, 1)).unwrap_or_else(|| "-".into());
        let (load, wall) = match r.event {
            Some(e) => (
                fmt::str_float(e.load_before),
                // The wall the log prints is in milliseconds, and the column asks for seconds.
                fmt::fixed(e.wall_ms as f64 / 1000.0, 0),
            ),
            None => ("-".into(), "-".into()),
        };
        row(
            out,
            &[
                r.page.clone(),
                r.scale.get(1..).unwrap_or("").to_string(),
                r.raster.clone(),
                r.pace.clone(),
                r.rep.clone(),
                r.n.to_string(),
                fmt::shown(r.inputs.as_ref()),
                fmt::shown(r.merged.as_ref()),
                ms(r.stat(INTERVAL, 0.5)),
                ms(r.stat(INTERVAL, 0.95)),
                ms(stats::percentile(&r.values(INTERVAL), 1.0)),
                ms(r.stat(SINCE_STEP, 0.5)),
                ms(r.median(PAINT)),
                ms(r.stat(PAINT, 0.95)),
                ms(stats::percentile(&r.values(PAINT), 1.0)),
                ms(r.median(COPY)),
                ms(r.stat(COPY, 0.95)),
                ms_at(r.median(PRESENT), 2),
                area,
                ms_at(r.docs(), 2),
                load,
                wall,
                r.src.clone(),
            ],
        );
    }
}

/// ## 2 - one row per configuration: the median of its runs' medians, and how far apart they were.
fn spread(out: &mut String, runs: &[Run], groups: &BTreeMap<Cell, Vec<usize>>) {
    out.push_str("\n## 2. the spread over the runs of one configuration (median of the per-run medians)\n\n");
    head(
        out,
        &[
            "page", "s", "raster", "pace", "runs", "sets", "frames per run", "interval p50", "interval p95",
            "paint p50", "copy p50", "tiles p50", "tile area %", "iv/paint",
        ],
    );
    for (key, all) in groups {
        let (page, scale, raster, pace) = key;
        let g: Vec<usize> = all.iter().copied().filter(|&i| runs[i].n >= 3).collect();
        if g.is_empty() {
            row(
                out,
                &[
                    page.clone(),
                    scale.get(1..).unwrap_or("").into(),
                    raster.clone(),
                    pace.clone(),
                    all.len().to_string(),
                    sets(runs, all),
                    "none of 3+ frames".into(),
                    "-".into(),
                    "-".into(),
                    "-".into(),
                    "-".into(),
                    "-".into(),
                    "-".into(),
                    "-".into(),
                ],
            );
            continue;
        }
        let per_run: Vec<String> =
            g.iter().map(|&i| format!("{}:{}", runs[i].src, runs[i].n)).collect();
        let iv: Vec<f64> = g.iter().filter_map(|&i| runs[i].stat(INTERVAL, 0.5)).collect();
        let pa: Vec<f64> = g.iter().filter_map(|&i| runs[i].stat(PAINT, 0.5)).collect();
        let areas: Vec<f64> = g.iter().filter_map(|&i| runs[i].area_pct()).collect();
        let ratio = share(&iv, &pa);
        row(
            out,
            &[
                page.clone(),
                scale.get(1..).unwrap_or("").into(),
                raster.clone(),
                pace.clone(),
                g.len().to_string(),
                sets(runs, &g),
                per_run.join(","),
                spread3(&iv, 1),
                spread3(&g.iter().filter_map(|&i| runs[i].stat(INTERVAL, 0.95)).collect::<Vec<f64>>(), 1),
                spread3(&pa, 1),
                spread3(&g.iter().filter_map(|&i| runs[i].stat(COPY, 0.5)).collect::<Vec<f64>>(), 1),
                spread3(&g.iter().filter_map(|&i| runs[i].stat(WR_TILES_MS, 0.5)).collect::<Vec<f64>>(), 1),
                spread3(&areas, 1),
                ratio,
            ],
        );
    }
}

/// Which matrices the runs of a row came from, sorted and comma-joined: a row whose sets read `m2,m3`
/// pooled two re-runs of the same cell.
fn sets(runs: &[Run], group: &[usize]) -> String {
    let mut srcs: Vec<&str> = group.iter().map(|&i| runs[i].src.as_str()).collect();
    srcs.sort_unstable();
    srcs.dedup();
    srcs.join(",")
}

/// ## 3 - where the milliseconds of a frame go, by measured container and then by the named parts inside.
fn division(out: &mut String, runs: &[Run], groups: &BTreeMap<Cell, Vec<usize>>) {
    out.push_str("\n## 3. where the frame's milliseconds go: the measured containers, then the named parts inside them\n\n");
    out.push_str("Every number below is a median of per-run medians over the runs of the rewritten driver that\n");
    out.push_str("returned twenty frames or more - the pre-rewrite rows are in sections 1, 2 and 4, where the count\n");
    out.push_str("of frames each run got is the point. Prose on the findings page quotes these cells and nothing else.\n");
    out.push_str("Medians do not add, so a named part can exceed the container it is inside (the row reading 117 % of\n");
    out.push_str("`wr render` says the two medians came from different frames of the same cell) and the remainder can\n");
    out.push_str("come out negative. Read the shares, not the subtraction.\n\n");
    for (key, all) in groups {
        let g = real(runs, all);
        if g.is_empty() {
            continue;
        }
        let m = |field: usize| median_of_medians(runs, &g, field);
        let (frame, paint) = (m(INTERVAL), m(PAINT));
        let (present, copy) = (m(PRESENT), m(COPY));
        let (render, update) = (m(WR_RENDER), m(WR_UPDATE));
        let inner: Vec<Option<f64>> = STAGES.iter().map(|(_, field)| m(*field)).collect();
        let at = |label: &str| {
            STAGES.iter().position(|(name, _)| *name == label).and_then(|i| inner[i])
        };
        let at_or = |label: &str| at(label).unwrap_or(0.0);
        // Added in the order the stages are listed, which is the order Python's dict kept them in: the
        // same terms in another order are a different double, and this sum is printed to a whole percent.
        let sum_of = |labels: &[&str]| -> f64 {
            STAGES
                .iter()
                .enumerate()
                .filter(|(i, _)| labels.contains(&STAGES[*i].0))
                .fold(0.0, |sum, (i, _)| sum + inner[i].unwrap_or(0.0))
        };
        let phase_sum = sum_of(&RASTER_PHASES);
        let lay = sum_of(&LAYOUT_STAGES);
        let rest = match (frame, paint, present, copy) {
            (Some(f), Some(p), Some(c), Some(s)) => Some(f - p - c - s),
            _ => None,
        };
        out.push_str(&format!("- {} {} {} {}\n", key.0, key.1, key.2, key.3));
        out.push_str(&format!(
            "    frame {} ms = paint {} ({}) + copy {} ({}) + present {} + the rest of the loop {} ({})\n",
            ms(frame),
            ms(paint),
            p(paint, frame),
            ms(copy),
            p(copy, frame),
            ms(present),
            ms(rest),
            p(rest, frame)
        ));
        out.push_str(&format!(
            "    paint {} ms = wr render {} ({} of paint) + wr update {} + the part of paint WebRender does not name {}\n",
            ms(paint),
            ms(render),
            p(render, paint),
            ms(update),
            ms(match (paint, render, update) {
                (Some(p), Some(r), Some(u)) => Some(p - r - u),
                _ => None,
            })
        ));
        // Ordered by how much of the container each part took, name breaking a tie, which is how the
        // analysis keeps the list stable when two phases cost the same - and `or 0` is what puts a part
        // the runs never reported at the end rather than first.
        let ordered = |labels: &[&'static str]| {
            let mut v: Vec<&'static str> = labels.to_vec();
            v.sort_by(|a, b| {
                (-at_or(*a)).total_cmp(&(-at_or(*b))).then_with(|| (*a).cmp(*b))
            });
            v
        };
        let parts = ordered(&RASTER_PHASES)
            .into_iter()
            .filter(|l| at(l).is_some_and(|v| v > 0.0))
            .map(|l| format!("{} {} ms ({})", l, ms(at(l)), p(at(l), render)))
            .collect::<Vec<String>>();
        let unnamed = render.map(|r| r - phase_sum);
        out.push_str(&format!(
            "    wr render {} ms named parts: {} | inside render and unnamed {} ({} of render)\n",
            ms(render),
            if parts.is_empty() { "none".into() } else { parts.join(", ") },
            ms(unnamed),
            p(unnamed, render)
        ));
        let layout_line = ordered(&LAYOUT_STAGES)
            .into_iter()
            .filter(|l| at(l).is_some_and(|v| v > 0.0))
            .map(|l| format!("{} {} ms", l, ms(at(l))))
            .collect::<Vec<String>>();
        let placed = match (paint, render) {
            (Some(paint), Some(render)) if lay != 0.0 && paint != 0.0 && render != 0.0 => {
                let gap = if paint - render == 0.0 { 0.0 } else { paint - render };
                lay > gap
            }
            _ => false,
        };
        out.push_str(&format!(
            "    layout lines of the frame: {} | frame minus (paint+copy+present) is {}, so layout sits {}\n",
            if layout_line.is_empty() { "nothing".into() } else { layout_line.join(", ") },
            ms(rest),
            if placed { "outside the timed paint()" } else { "inside paint or too small to place" }
        ));
    }
}

/// ## 4 - what the frame records say about layout, the `full_paint` flag, and the frames with nothing to draw.
fn layout_and_flags(out: &mut String, runs: &[Run], groups: &BTreeMap<Cell, Vec<usize>>) {
    out.push_str("\n## 4. layout, the `full_paint` flag, and the frame that had nothing to draw\n\n");
    out.push_str("The flag column is here to be struck out: webrender's `RendererStats::merge` assigns `full_paint = true`\n");
    out.push_str("unconditionally (src/renderer/mod.rs:4731 of the pinned rev), so it says a frame was rendered and nothing\n");
    out.push_str("about how much of the window it covered. A claim resting on it - every frame repaints the whole window -\n");
    out.push_str("is not supported by this diagnostic. Section 6 is where that question can actually be answered.\n\n");
    head(
        out,
        &[
            "page", "s", "raster", "pace", "runs", "frames", "frames with layout", "frames that built a list",
            "`full_paint` true (always)", "0-1-draw frames", "their wr render p50", "async scroll line",
        ],
    );
    for (key, g) in groups {
        let total: usize = g.iter().map(|&i| runs[i].n).sum();
        let measures = g.iter().any(|&i| runs[i].measures_layout);
        let layout = if measures {
            g.iter().map(|&i| runs[i].layout_frames()).sum::<usize>().to_string()
        } else {
            "-".into()
        };
        let lists = if measures {
            g.iter().map(|&i| runs[i].list_frames()).sum::<usize>().to_string()
        } else {
            "-".into()
        };
        let empties: Vec<f64> =
            g.iter().filter_map(|&i| runs[i].empty_render().0).collect();
        row(
            out,
            &[
                key.0.clone(),
                key.1.get(1..).unwrap_or("").into(),
                key.2.clone(),
                key.3.clone(),
                g.len().to_string(),
                total.to_string(),
                layout,
                lists,
                g.iter().map(|&i| runs[i].full_paint()).sum::<usize>().to_string(),
                g.iter().map(|&i| runs[i].empty_render().1).sum::<usize>().to_string(),
                ms(stats::median(&empties)),
                runs[*g.first().expect("a group has a run")].async_evidence.clone(),
            ],
        );
    }
}

/// ## 5 - the frames of a run in the order they came, pooled by position: does a scroll get cheaper warm?
fn warmup(out: &mut String, runs: &[Run], groups: &BTreeMap<Cell, Vec<usize>>) {
    out.push_str("\n## 5. the frames of a run in the order they came: does a scroll get cheaper once the cache is warm?\n\n");
    out.push_str("Pooled per-frame values, every run of the configuration together, by the position of the frame in its\n");
    out.push_str("run. `area` is the tiles a frame re-rastered as a share of the window. A scroll of 120 CSS px in a 1090 CSS\n");
    out.push_str("px window uncovers 11 % of it at one step per frame; the paced runs merge two to four steps into a frame, so\n");
    out.push_str("the share a frame re-rasters in passing is larger than 11 % and section 6 is what separates that from waste.\n\n");
    head(out, &["page", "s", "raster", "pace", "frames", "area %", "tiles", "paint", "interval", "kpx"]);
    for (key, all) in groups {
        let g = real(runs, all);
        if g.is_empty() {
            continue;
        }
        let (page, scale, raster, pace) = key;
        for (lo, hi) in BUCKETS {
            let mut tiles = Vec::new();
            let mut areas = Vec::new();
            let mut paints = Vec::new();
            let mut ivs = Vec::new();
            let mut kpxs = Vec::new();
            for &i in &g {
                for f in runs[i].frames.iter().skip(lo - 1).take(hi - lo + 1) {
                    if let Some(v) = f.tiles {
                        tiles.push(v);
                    }
                    if let Some(v) = f.tiles_kpx {
                        if runs[i].window != 0.0 {
                            kpxs.push(v);
                            areas.push(100.0 * v / runs[i].window);
                        }
                    }
                    if let Some(v) = f.times[PAINT] {
                        paints.push(v);
                    }
                    // Truthiness, not a null check: the pre-rewrite driver's first frame interval of 0.0 is
                    // not a frame gap and does not belong in the column.
                    if let Some(v) = f.times[INTERVAL] {
                        if v != 0.0 {
                            ivs.push(v);
                        }
                    }
                }
            }
            if tiles.is_empty() {
                continue;
            }
            row(
                out,
                &[
                    page.clone(),
                    scale.get(1..).unwrap_or("").into(),
                    raster.clone(),
                    pace.clone(),
                    tiles.len().to_string(),
                    range3(&areas, 1),
                    ms(stats::median(&tiles)),
                    ms(stats::median(&paints)),
                    ms(stats::median(&ivs)),
                    if kpxs.is_empty() { "-".into() } else { ms_at(stats::median(&kpxs), 0) },
                ],
            );
        }
    }
}

/// ## 6 - what a frame re-rastered, against the strip its steps brought on screen.
fn uncovered(out: &mut String, runs: &[Run], groups: &BTreeMap<Cell, Vec<usize>>) {
    out.push_str("\n## 6. what a frame re-rastered, against what the scroll uncovered\n\n");
    out.push_str("`need` is the strip the frame's steps brought on screen - steps since the last frame, times the 120 CSS px\n");
    out.push_str("of a step, times the scale, clipped to the window - as a share of the window. `tile` is what WebRender said\n");
    out.push_str("it re-rastered into its picture cache (`wr-phases tiles .. kpx`, the dirty rect of every tile target, from\n");
    out.push_str("the patch in patches/webrender/0003). `ratio` is tile over need, so 1.0 is a renderer that draws exactly\n");
    out.push_str("what moved and 3.0 one that redraws two windows' worth of unchanged tiles for each one it needed.\n");
    out.push_str("\n");
    out.push_str("`scroll` counts the frames a step moved; `idle` the ones whose step index did not advance. A run keeps\n");
    out.push_str("recording until 300 ms after the last frame, so a page that has reached the bottom, or that repaints itself\n");
    out.push_str("(yt's video, wiki's late loads), runs to its time budget with hundreds of idle frames. On the two static\n");
    out.push_str("pages an idle frame has nothing new to show, so whatever it re-rasters is the waste spec 05 asks about; on\n");
    out.push_str("yt it is the next video frame, so idle means something else there - and so, for that matter, does a\n");
    out.push_str("scroll frame, since the video repaints whether or not the page moved: the 12x in yt's unpaced row is the\n");
    out.push_str("picture moving, not a cache being thrown away.\n");
    out.push_str("\n");
    out.push_str("`need` is a model and not a measurement: it assumes every answered step moves the page. That holds\n");
    out.push_str("to the end on the synthetic one (section 5's last bucket of it still re-rasters a quarter of a window),\n");
    out.push_str("but a real page can swallow a step into an inner scroller or a carousel, so wiki's 0.00 ratio is the\n");
    out.push_str("model overstating the need and not a renderer that draws nothing. Read the ratio on the long page; the\n");
    out.push_str("`idle` columns carry no assumption at all, since the step index did not move whatever the page did.\n\n");
    head(
        out,
        &[
            "page", "s", "raster", "pace", "frames", "scroll", "idle", "steps/f", "need %", "tile %", "ratio",
            "idle tile %", "idle paint",
        ],
    );
    let mut pool: Vec<Needish> = Vec::new();
    for (key, all) in groups {
        let g: Vec<usize> = real(runs, all)
            .into_iter()
            .filter(|&i| runs[i].frames.iter().any(|f| f.step.is_some()))
            .collect();
        if g.is_empty() {
            continue;
        }
        let (page, scale, raster, pace) = key;
        let start = pool.len();
        for &i in &g {
            pool.extend(runs[i].frame_need().into_iter().map(Needish::from));
        }
        let flat: Vec<&Needish> = pool[start..].iter().collect();
        if flat.is_empty() {
            continue;
        }
        let moved: Vec<&Needish> = flat.iter().copied().filter(|f| f.steps != 0).collect();
        let idle: Vec<&Needish> = flat.iter().copied().filter(|f| f.steps == 0).collect();
        let per_frame = flat.len();
        let ratios: Vec<f64> = moved
            .iter()
            .filter(|f| f.measured && f.need > 0.0)
            .map(|f| f.tile / f.need)
            .collect();
        row(
            out,
            &[
                page.clone(),
                scale.get(1..).unwrap_or("").into(),
                raster.clone(),
                pace.clone(),
                per_frame.to_string(),
                moved.len().to_string(),
                idle.len().to_string(),
                if moved.is_empty() {
                    "-".into()
                } else {
                    ms_at(
                        stats::median(&moved.iter().map(|f| f.steps as f64).collect::<Vec<f64>>()),
                        1,
                    )
                },
                range3(&moved.iter().map(|f| f.need).collect::<Vec<f64>>(), 1),
                range3(&moved.iter().map(|f| f.tile).collect::<Vec<f64>>(), 1),
                range3(&ratios, 2),
                range3(&idle.iter().map(|f| f.tile).collect::<Vec<f64>>(), 1),
                if idle.is_empty() {
                    "-".into()
                } else {
                    ms(stats::median(
                        &idle.iter().filter_map(|f| f.paint).collect::<Vec<f64>>(),
                    ))
                },
            ],
        );
    }
    out.push('\n');
    if pool.is_empty() {
        return;
    }
    let moved: Vec<&Needish> = pool.iter().filter(|f| f.steps > 0).collect();
    let idle: Vec<&Needish> = pool.iter().filter(|f| f.steps == 0).collect();
    let dry: Vec<&Needish> = moved.iter().copied().filter(|f| f.measured && f.tile == 0.0).collect();
    out.push_str(&format!(
        "Over the {} frames pooled above that a step moved the page: the strip uncovered is a median {} of a window,",
        moved.len(),
        ms_at(stats::median(&moved.iter().map(|f| f.need).collect::<Vec<f64>>()), 1)
    ));
    out.push('\n');
    out.push_str(&format!(
        "the area re-rastered a median {} of one, a ratio of {}; {} of them re-rastered nothing at all and still",
        ms_at(stats::median(&moved.iter().map(|f| f.tile).collect::<Vec<f64>>()), 1),
        ms_at(
            stats::median(
                &moved
                    .iter()
                    .filter(|f| f.measured && f.need > 0.0)
                    .map(|f| f.tile / f.need)
                    .collect::<Vec<f64>>(),
            ),
            2
        ),
        dry.len()
    ));
    out.push('\n');
    out.push_str(&format!(
        "painted a median {} ms. The other {} frames had no step in them at all - the page had reached the bottom,",
        if dry.is_empty() { "-".into() } else { ms(stats::median(&dry.iter().filter_map(|f| f.paint).collect::<Vec<f64>>())) },
        idle.len()
    ));
    out.push('\n');
    out.push_str(&format!(
        "or keeps animating by itself - and re-rastered a median {} of a window while painting {} ms.",
        ms(stats::median(&idle.iter().map(|f| f.tile).collect::<Vec<f64>>())),
        if idle.is_empty() { "-".into() } else { ms(stats::median(&idle.iter().filter_map(|f| f.paint).collect::<Vec<f64>>())) },
    ));
    out.push('\n');
}

/// A frame's need-and-tile pair, kept by value so the pooled paragraph can borrow across groups.
struct Needish {
    steps: i64,
    need: f64,
    tile: f64,
    paint: Option<f64>,
    measured: bool,
}

impl From<crate::run::Need> for Needish {
    fn from(n: crate::run::Need) -> Self {
        Needish { steps: n.steps, need: n.need, tile: n.tile, paint: n.paint, measured: n.measured }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three runs of one cell: sixty frames alternating at 16 and 17 ms, three frames at 1, 2 and 3 ms, and
    /// one frame of 800 ms. A median of the three medians is what the tables pool, so the short runs count
    /// once each however few frames they hold, and the long one cannot outvote them both.
    fn three_runs() -> Vec<Run> {
        let json = |frames: &[f64]| {
            serde_json::json!({
                "meta": {"device_px": [1000, 1000], "scale": 1.0, "steps": 60},
                "summary": {"window_device_px": 1_000_000},
                "frames": frames.iter().map(|ms| serde_json::json!({"interval_ms": ms, "paint_ms": ms / 2.0})).collect::<Vec<_>>(),
            })
        };
        [
            json(&(0..60).map(|i| 16.0 + (i % 2) as f64).collect::<Vec<f64>>()),
            json(&[1.0, 2.0, 3.0]),
            json(&[800.0]),
        ]
        .iter()
        .map(|doc| Run::parse(doc, "t", "long-s1-rust-fast", "m2", 0).unwrap())
        .collect()
    }

    #[test]
    fn a_pooled_median_is_the_median_of_the_runs_medians() {
        let runs = three_runs();
        let group: Vec<usize> = (0..runs.len()).collect();
        // 16.5, 2.0 and 800.0: the middle run's median, not the 16.0 you get by pooling all 64 frames of
        // the cell into one list (where the long run is 60 of them) nor the 272.8 of a mean.
        assert_eq!(median_of_medians(&runs, &group, INTERVAL), Some(16.5));
        assert_eq!(median_of_medians(&runs, &group, PAINT), Some(8.25));
        // Two runs leave the mean of their two medians, and an empty pool leaves a dash.
        let only_two = vec![group[0], group[1]];
        assert_eq!(median_of_medians(&runs, &only_two, INTERVAL), Some(9.25));
        assert_eq!(median_of_medians(&runs, &[], INTERVAL), None);
    }

    #[test]
    fn the_iv_over_paint_column_is_a_share_of_the_frame_not_a_ratio_of_frames() {
        // Two cells' worth of per-run medians: 20 over 16 is 1.25, and the order the runs came in does
        // not matter because both columns are medians of their own pool.
        assert_eq!(share(&[20.0, 16.0, 24.0], &[16.0, 8.0, 12.0]), "1.67", "20 over the median paint of 12");
        assert_eq!(share(&[16.0], &[16.0]), "1.00");
        assert_eq!(share(&[], &[16.0]), "-", "no interval at all is not an infinite ratio");
        assert_eq!(share(&[16.0], &[]), "-");
        assert_eq!(share(&[16.0], &[0.0]), "-", "and a paint of nothing is a dash, not a division");
    }

    #[test]
    fn a_spread_of_one_number_is_a_range_and_a_lone_frame_is_not() {
        // Section 2 pools runs, and one run of them still says what the spread across the configuration
        // was; sections 5 and 6 pool frames, where one frame has no spread to report.
        assert_eq!(spread3(&[7.0], 1), "7.0 (7.0..7.0)");
        assert_eq!(range3(&[7.0], 1), "7.0");
        assert_eq!(range3(&[7.0, 3.0, 5.0], 1), "5.0 (3.0..7.0)");
        assert_eq!(spread3(&[], 1), "-");
        assert_eq!(range3(&[], 2), "-");
        // A ratio column, which is the same shape at two digits.
        assert_eq!(range3(&[1.005, 2.0], 2), "1.50 (1.00..2.00)");
    }

    #[test]
    fn a_share_is_a_percent_or_a_dash_never_a_division_by_nothing() {
        assert_eq!(p(Some(10.0), Some(40.0)), "25%");
        assert_eq!(p(Some(10.0), Some(0.0)), "-", "a container that measured nothing has no share");
        assert_eq!(p(None, Some(40.0)), "-");
        assert_eq!(p(Some(10.0), None), "-");
        assert_eq!(p(Some(47.0), Some(40.0)), "118%", "medians do not add, and the tables say so");
    }
}
