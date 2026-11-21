//! See README.md.

mod cdp;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1000;

struct Args {
    engine: PathBuf,
    name: String,
    out: PathBuf,
    sites: PathBuf,
}

fn args() -> Args {
    let mut a = std::env::args().skip(1);
    let (mut engine, mut name, mut out, mut sites) = (None, None, None, None);
    while let Some(arg) = a.next() {
        match arg.as_str() {
            "--engine" => engine = a.next().map(PathBuf::from),
            "--name" => name = a.next(),
            "--out" => out = a.next().map(PathBuf::from),
            _ => sites = Some(PathBuf::from(arg)),
        }
    }
    Args {
        engine: engine.expect("--engine <binary>"),
        name: name.expect("--name <label>"),
        out: out.expect("--out <dir>"),
        sites: sites.expect("a sites file"),
    }
}

fn slug(url: &str) -> String {
    url.trim_start_matches("https://")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(60)
        .collect()
}

/// Mean luminance of 8x8 cells: what a person sees at a glance, without font rendering's noise.
fn pixel_score(a: &Path, b: &Path) -> f64 {
    let (Ok(a), Ok(b)) = (image::open(a), image::open(b)) else {
        return 0.0;
    };
    let (a, b) = (a.to_luma8(), b.to_luma8());
    let (w, h) = (a.width().min(b.width()), a.height().min(b.height()));
    let (mut total, mut cells) = (0.0, 0u32);
    for cy in (0..h.saturating_sub(7)).step_by(8) {
        for cx in (0..w.saturating_sub(7)).step_by(8) {
            let mean = |img: &image::GrayImage| {
                let mut sum = 0u32;
                for y in cy..cy + 8 {
                    for x in cx..cx + 8 {
                        sum += img.get_pixel(x, y).0[0] as u32;
                    }
                }
                sum as f64 / 64.0
            };
            total += (mean(&a) - mean(&b)).abs() / 255.0;
            cells += 1;
        }
    }
    if cells == 0 { 0.0 } else { 100.0 * (1.0 - total / cells as f64) }
}

fn words(text: &str) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for word in text.split_whitespace() {
        let word: String = word.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
        if !word.is_empty() {
            *map.entry(word).or_insert(0) += 1;
        }
    }
    map
}

/// The share of reference words the engine has, and the most common ones it lacks.
fn text_score(reference: &str, engine: &str) -> (f64, Vec<String>) {
    let (r, e) = (words(reference), words(engine));
    let total: usize = r.values().sum();
    let mut found = 0;
    let mut missing: Vec<(usize, &String)> = Vec::new();
    for (word, &n) in &r {
        let have = e.get(word).copied().unwrap_or(0).min(n);
        found += have;
        if have < n {
            missing.push((n - have, word));
        }
    }
    missing.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    let score = if total == 0 { 100.0 } else { 100.0 * found as f64 / total as f64 };
    (score, missing.into_iter().take(15).map(|(_, w)| w.clone()).collect())
}

fn iou(a: &Value, b: &Value) -> f64 {
    let v = |o: &Value, k: &str| o[k].as_f64().unwrap_or(0.0);
    let (ax, ay, aw, ah) = (v(a, "x"), v(a, "y"), v(a, "w"), v(a, "h"));
    let (bx, by, bw, bh) = (v(b, "x"), v(b, "y"), v(b, "w"), v(b, "h"));
    let ix = (ax + aw).min(bx + bw) - ax.max(bx);
    let iy = (ay + ah).min(by + bh) - ay.max(by);
    if ix <= 0.0 || iy <= 0.0 {
        return 0.0;
    }
    let i = ix * iy;
    i / (aw * ah + bw * bh - i).max(1.0)
}

/// Within 8 px on every edge: the same box to the eye.
fn close(a: &Value, b: &Value) -> bool {
    ["x", "y", "w", "h"]
        .iter()
        .all(|k| (a[*k].as_f64().unwrap_or(0.0) - b[*k].as_f64().unwrap_or(0.0)).abs() <= 8.0)
}

/// Mean overlap by path, and the largest elements that are most wrong.
fn box_score(reference: &Value, engine: &Value) -> (f64, Vec<String>) {
    let empty = Vec::new();
    let rb = reference["boxes"].as_array().unwrap_or(&empty);
    let eb: HashMap<&str, &Value> = engine["boxes"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|b| Some((b["p"].as_str()?, b)))
        .collect();
    if rb.is_empty() {
        return (100.0, Vec::new());
    }
    let mut total = 0.0;
    let mut counted = 0usize;
    let mut worst: Vec<(f64, String)> = Vec::new();
    // What is on the first two screens: further down, one line's wrap moves everything without being many bugs.
    for b in rb.iter().filter(|b| b["y"].as_f64().unwrap_or(0.0) < 2.0 * HEIGHT as f64) {
        let path = b["p"].as_str().unwrap_or("");
        let overlap = eb.get(path).map_or(0.0, |e| if close(b, e) { 1.0 } else { iou(b, e) });
        total += overlap;
        counted += 1;
        let area = b["w"].as_f64().unwrap_or(0.0) * b["h"].as_f64().unwrap_or(0.0);
        let label = match eb.get(path) {
            Some(e) => format!(
                "{path}: {:.0}x{:.0} at {:.0},{:.0}, engine {:.0}x{:.0} at {:.0},{:.0}",
                b["w"].as_f64().unwrap_or(0.0), b["h"].as_f64().unwrap_or(0.0),
                b["x"].as_f64().unwrap_or(0.0), b["y"].as_f64().unwrap_or(0.0),
                e["w"].as_f64().unwrap_or(0.0), e["h"].as_f64().unwrap_or(0.0),
                e["x"].as_f64().unwrap_or(0.0), e["y"].as_f64().unwrap_or(0.0)
            ),
            None => format!("{path}: missing in the engine"),
        };
        worst.push(((1.0 - overlap) * area.sqrt(), label));
    }
    worst.sort_by(|a, b| b.0.total_cmp(&a.0));
    (100.0 * total / counted.max(1) as f64, worst.into_iter().take(8).map(|(_, l)| l).collect())
}

fn dom_score(reference: &Value, engine: &Value) -> f64 {
    let r = reference["elements"].as_f64().unwrap_or(0.0);
    let e = engine["elements"].as_f64().unwrap_or(0.0);
    if r.max(e) == 0.0 { 100.0 } else { 100.0 * r.min(e) / r.max(e) }
}

/// This page is a site or the network turning the visit away, not the site. Scoring an engine against it
/// says nothing about layout, so Chromium getting one leaves the site out of the mean.
fn refusal(page: &Value) -> Option<&'static str> {
    const MARKS: [(&str, &str); 8] = [
        ("just a moment", "challenge"),
        ("been blocked", "blocked"),
        ("unusual traffic", "blocked"),
        ("not a robot", "challenge"),
        ("enable js and disable any ad blocker", "challenge"),
        ("select all squares", "challenge"),
        ("err_", "network error"),
        ("this site can\x27t be reached", "network error"),
    ];
    let title = page["title"].as_str().unwrap_or("").to_lowercase();
    let text: String = page["text"].as_str().unwrap_or("").chars().take(1500).collect::<String>().to_lowercase();
    MARKS
        .iter()
        .find(|(mark, _)| title.contains(mark) || text.contains(mark))
        .map(|(_, why)| *why)
}

fn run_engine(args: &Args, url: &str, png: &Path, dump: &Path) -> (bool, u128, u64, String) {
    let t = Instant::now();
    let child = Command::new("timeout")
        .arg("150")
        .arg(&args.engine)
        .args(["--render", url])
        .arg(png)
        .arg(format!("{WIDTH}x{HEIGHT}"))
        .arg(dump)
        .output();
    let ms = t.elapsed().as_millis();
    let Ok(output) = child else {
        return (false, ms, 0, "could not start".into());
    };
    let log = String::from_utf8_lossy(&output.stderr).into_owned();
    let rss = log
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("render: ")?.rsplit("rss ").next()?.split(' ').next()?.parse().ok())
        .unwrap_or(0);
    let tail: String = log.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
    (png.exists() && dump.exists(), ms, rss, tail)
}

fn main() {
    let args = args();
    std::fs::create_dir_all(&args.out).expect("the output directory");
    let sites = std::fs::read_to_string(&args.sites).expect("the sites file");
    let mut chrome = cdp::Chrome::start(WIDTH, HEIGHT);
    let mut tsv = String::from("score\tpixels\ttext\tboxes\tdom\tms\trss_kib\tchrome_ms\turl\n");
    let mut todo = format!("# {}: what differs from Chromium, worst site first\n\n", args.name);
    let mut rows: Vec<(f64, String)> = Vec::new();
    let (mut sum, mut n, mut skipped) = (0.0, 0, 0);
    for url in sites.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let s = slug(url);
        let (rp, rd) = (args.out.join(format!("{s}.chrome.png")), args.out.join(format!("{s}.chrome.json")));
        let (ep, ed) = (args.out.join(format!("{s}.png")), args.out.join(format!("{s}.json")));
        let _ = std::fs::remove_file(&ep);
        let _ = std::fs::remove_file(&ed);
        let t = Instant::now();
        let mut reference = chrome.page(url, &rp, Duration::from_secs(45));
        if matches!(&reference, Ok(page) if refusal(page).is_some()) {
            std::thread::sleep(Duration::from_secs(5));
            reference = chrome.page(url, &rp, Duration::from_secs(45));
        }
        let chrome_ms = t.elapsed().as_millis();
        let Ok(reference) = reference else {
            eprintln!("{url}: Chromium failed: {reference:?}");
            continue;
        };
        std::fs::write(&rd, reference.to_string()).expect("write");
        let (ok, ms, rss, log) = run_engine(&args, url, &ep, &ed);
        let engine: Value = std::fs::read(&ed)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        if let Some(why) = refusal(&reference) {
            eprintln!("  --  no reference ({why}) {url}");
            let _ = writeln!(tsv, "\t\t\t\t\t{ms}\t{rss}\t{chrome_ms}\t{url} (no reference: {why})");
            skipped += 1;
            continue;
        }
        let (mut score, mut pixels, mut text, mut boxes, mut dom) = (0.0, 0.0, 0.0, 0.0, 0.0);
        let mut note = String::new();
        if ok {
            pixels = pixel_score(&rp, &ep);
            let (t, missing) = text_score(
                reference["text"].as_str().unwrap_or(""),
                engine["text"].as_str().unwrap_or(""),
            );
            text = t;
            let (b, worst) = box_score(&reference, &engine);
            boxes = b;
            dom = dom_score(&reference, &engine);
            score = (pixels + text + boxes + dom) / 4.0;
            let _ = writeln!(note, "- words missing: {}", missing.join(", "));
            for w in worst {
                let _ = writeln!(note, "- {w}");
            }
        } else {
            let _ = writeln!(note, "- FAILED: {log}");
        }
        eprintln!("{score:5.1} px {pixels:5.1} text {text:5.1} box {boxes:5.1} dom {dom:5.1} {ms:6} ms {rss:7} KiB {url}");
        let _ = writeln!(tsv, "{score:.1}\t{pixels:.1}\t{text:.1}\t{boxes:.1}\t{dom:.1}\t{ms}\t{rss}\t{chrome_ms}\t{url}");
        rows.push((score, format!("## {score:.1} {url}\n\nPixels {pixels:.1}, text {text:.1}, boxes {boxes:.1}, DOM {dom:.1}.\n\n{note}\n")));
        sum += score;
        n += 1;
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, row) in rows {
        todo.push_str(&row);
    }
    let mean = if n == 0 { 0.0 } else { sum / n as f64 };
    let _ = writeln!(tsv, "{mean:.1}\t\t\t\t\t\t\t\tMEAN of {n}, {skipped} without a reference");
    std::fs::write(args.out.join("scores.tsv"), tsv).expect("write");
    std::fs::write(args.out.join("todo.md"), todo).expect("write");
    eprintln!("mean {mean:.1} over {n} sites");
}
