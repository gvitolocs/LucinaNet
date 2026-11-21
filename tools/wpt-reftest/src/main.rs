//! Runs the vendored Web Platform Tests against the real Rust renderer, `lucinanet-servo --render`.
//!
//! `mach test-wpt --bin <lucinanet-servo>` cannot work: wptrunner's "servo" product launches the binary
//! with `--webdriver=PORT` and this binary has no WebDriver server, only `--render` and the GUI. So this
//! harness takes the path `tests/media/wpt.sh` proved: a local `wpt serve`, one `--render` per test, and
//! for testharness tests a user script that prints `add_completion_callback`'s results to the console.
//! On top of that it adds what the media script does not have: reftests, scored with WPT's fuzzy rule.
//!
//! The wrapper `tests/wpt/run-reftests.sh` starts the server and builds this tool; this binary only
//! selects tests and drives the renderer, so it can also be pointed at an already-running server.

mod compare;
mod manifest;
mod report;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// The proven hook from tests/media/wpt.sh, copied here so this tool owns its own report.js and does
/// not depend on the media script's internals.
const REPORT_JS: &str = r#"// Waits for testharness.js, then prints each subtest's result and a last line when the file is done.
(function wait() {
  if (typeof add_completion_callback !== "function") { setTimeout(wait, 20); return; }
  const names = ["PASS", "FAIL", "TIMEOUT", "NOTRUN", "PRECONDITION_FAILED"];
  add_completion_callback(function (tests, status) {
    for (const t of tests) console.log("wpt-result: " + names[t.status] + " " + t.name.replace(/\s+/g, " ") + (t.message ? " | " + String(t.message).slice(0, 200).replace(/\s+/g, " ") : ""));
    console.log("wpt-done: harness=" + ["OK", "ERROR", "TIMEOUT", "PRECONDITION_FAILED"][status.status]);
  });
})();
"#;

const USAGE: &str = "\
wpt-reftest: WPT reftest/testharness/crashtest runner for lucinanet-servo --render

  wpt-reftest --manifest <MANIFEST.json> --bin <lucinanet-servo> --out <dir> \
              --base-url http://localhost:8123 --include css/css-masking/ --limit 8

Required (or via env): --manifest (WPT_MANIFEST), --bin (LUCINANET_BIN), --out, --base-url.
  --include PREFIX   URL-prefix filter, repeatable (css/css-masking/, css/css-fonts/, svg/)
  --exclude SUBSTR   substring filter, repeatable
  --types LIST       comma-separated: reftest,testharness,crashtest (default: all three)
  --limit N          stop after N selected tests (smoke runs)
  --jobs N           concurrent renders, default 1; the box is thermally limited, keep it low
  --viewport WxH     default 800x600, WPT's reftest window
  --raster rust|swgl default rust; swgl runs the same binary on upstream's C++ rasterizer (the baseline)
  --hold SECS        LUCINANET_RENDER_HOLD for reftest/crashtest, default 2
  --harness-hold S   LUCINANET_RENDER_HOLD for testharness, default 10
  --timeout SECS     per-render timeout, default 60
  --font-dirs DIR    LUCINANET_FONT_DIRS
  --report-js FILE   user script for testharness; default: written to <out>/report.js
  --max-c N          pause while k10temp Tctl >= N (default 90); --max-c 0 disables the gate
  --keep-all         keep every PNG and log, not just the failures'
  --no-default-excludes  do not drop .https./.h2./.webtransport. variants
  --list             print the selection and exit without rendering

Report on a finished run instead of rendering (see `wpt-reftest report --help`):

  wpt-reftest report clusters <out-dir> [--md <file>] [--top N]
  wpt-reftest report compare <before-dir> <after-dir> [--md <file>]
";

struct Args {
    manifest: PathBuf,
    bin: PathBuf,
    out: PathBuf,
    base_url: String,
    includes: Vec<String>,
    excludes: Vec<String>,
    types: Vec<String>,
    limit: Option<usize>,
    jobs: usize,
    viewport: (u32, u32),
    /// `LUCINANET_RASTER` for the renders: `rust` (our rasterizer) or `swgl` (upstream's C++ swgl, the baseline).
    raster: String,
    hold: u64,
    harness_hold: u64,
    timeout: u64,
    font_dirs: Option<String>,
    report_js: PathBuf,
    max_c: u32,
    keep_all: bool,
    list: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut manifest = std::env::var("WPT_MANIFEST").ok().map(PathBuf::from);
    let mut bin = std::env::var("LUCINANET_BIN").ok().map(PathBuf::from);
    let mut out = PathBuf::from("/tmp/lucinanet-wpt-reftest");
    let mut base_url = String::from("http://localhost:8123");
    let mut a = Args {
        manifest: PathBuf::new(),
        bin: PathBuf::new(),
        out: PathBuf::new(),
        base_url: String::new(),
        includes: Vec::new(),
        excludes: Vec::new(),
        types: manifest::RUNNABLE.iter().map(|s| s.to_string()).collect(),
        limit: None,
        jobs: 1,
        viewport: (800, 600),
        raster: "rust".to_string(),
        hold: 2,
        harness_hold: 10,
        timeout: 60,
        font_dirs: std::env::var("LUCINANET_FONT_DIRS").ok(),
        report_js: PathBuf::new(),
        max_c: 90,
        keep_all: false,
        list: false,
    };
    let mut report_js = None;
    let mut no_default_excludes = false;
    while let Some(arg) = it.next() {
        // A closure rather than a macro: macro_rules cannot see `arg` from its call site.
        let mut need = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--manifest" => manifest = Some(PathBuf::from(need()?)),
            "--bin" => bin = Some(PathBuf::from(need()?)),
            "--out" => out = PathBuf::from(need()?),
            "--base-url" => base_url = need()?,
            "--include" => a.includes.push(need()?),
            "--exclude" => a.excludes.push(need()?),
            "--types" => a.types = need()?.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            "--limit" => a.limit = Some(need()?.parse().map_err(|_| "--limit wants a number")?),
            "--raster" => {
                a.raster = need()?;
                if a.raster != "rust" && a.raster != "swgl" {
                    return Err("--raster wants rust or swgl".into());
                }
            }
            "--jobs" => a.jobs = need()?.parse().map_err(|_| "--jobs wants a number")?,
            "--viewport" => {
                let v = need()?;
                let (w, h) = v.split_once('x').ok_or("--viewport wants WxH")?;
                a.viewport = (w.parse().map_err(|_| "--viewport wants WxH")?, h.parse().map_err(|_| "--viewport wants WxH")?);
            },
            "--hold" => a.hold = need()?.parse().map_err(|_| "--hold wants seconds")?,
            "--harness-hold" => a.harness_hold = need()?.parse().map_err(|_| "--harness-hold wants seconds")?,
            "--timeout" => a.timeout = need()?.parse().map_err(|_| "--timeout wants seconds")?,
            "--font-dirs" => a.font_dirs = Some(need()?),
            "--report-js" => report_js = Some(PathBuf::from(need()?)),
            "--max-c" => a.max_c = need()?.parse().map_err(|_| "--max-c wants a temperature")?,
            "--keep-all" => a.keep_all = true,
            "--no-default-excludes" => no_default_excludes = true,
            "--list" => a.list = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            },
            other => return Err(format!("unknown argument {other}\n\n{USAGE}")),
        }
    }
    a.manifest = manifest.ok_or("--manifest (or WPT_MANIFEST) is required")?;
    a.bin = bin.ok_or("--bin (or LUCINANET_BIN) is required")?;
    if !a.bin.is_file() {
        return Err(format!("{}: not a file", a.bin.display()));
    }
    if a.jobs == 0 || a.jobs > 8 {
        return Err("--jobs must be 1..=8; this box is thermally limited, 1 or 2 is the sane range".into());
    }
    for t in &a.types {
        if !manifest::RUNNABLE.contains(&t.as_str()) {
            return Err(format!("--types: {t} is not runnable (choose from {})", manifest::RUNNABLE.join(",")));
        }
    }
    if !no_default_excludes {
        a.excludes.extend(manifest::DEFAULT_EXCLUDES.iter().map(|s| s.to_string()));
    }
    a.out = out;
    a.base_url = base_url.trim_end_matches('/').to_string();
    a.report_js = report_js.unwrap_or_else(|| a.out.join("report.js"));
    Ok(a)
}

// --------------------------------------------------------------------------------------------
// Thermal discipline: the project's rule is that background CPU work waits at or above 90 C.
// --------------------------------------------------------------------------------------------

fn tctl_c() -> Option<i64> {
    for entry in fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
        let p = entry.path();
        if fs::read_to_string(p.join("name")).ok().as_deref().map(str::trim) != Some("k10temp") {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(p.join("temp1_input")) {
            if let Ok(milli) = raw.trim().parse::<i64>() {
                return Some(milli / 1000);
            }
        }
    }
    None
}

/// Pauses while k10temp Tctl is at or above `max_c`. A no-op where there is no k10temp (not Linux, or
/// `--max-c 0`); on such a host the caller must wrap the run in the project's heatpause guard.
fn cool(max_c: u32) {
    if max_c == 0 {
        return;
    }
    let mut warned = false;
    while let Some(c) = tctl_c() {
        if c < max_c as i64 {
            return;
        }
        if !warned {
            eprintln!("heat: Tctl {c} C >= {max_c} C, pausing renders");
            warned = true;
        }
        std::thread::sleep(Duration::from_secs(20));
    }
}

// --------------------------------------------------------------------------------------------
// Driving the renderer
// --------------------------------------------------------------------------------------------

struct Rendered {
    code: Option<i32>,
    signal: Option<i32>,
    timed_out: bool,
    ms: u128,
}

impl Rendered {
    fn crashed(&self) -> bool {
        self.signal.is_some() || self.code.is_some_and(|c| c != 0 && c != 124)
    }
}

fn render(a: &Args, url: &str, png: &Path, log: &Path, hold: u64, user_js: Option<&Path>) -> Result<Rendered, String> {
    for p in [png, log] {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
    }
    let sink = File::create(log).map_err(|e| format!("{}: {e}", log.display()))?;
    let err = sink.try_clone().map_err(|e| e.to_string())?;
    let started = Instant::now();
    // `timeout` is what tests/media/wpt.sh uses and what gives us a hard 124 on a hung page: the
    // renderer's own load deadline is 120 s, far longer than a static CSS page should ever need.
    let mut cmd = Command::new("timeout");
    cmd.arg("-k").arg("5").arg(a.timeout.to_string());
    cmd.arg(&a.bin)
        .arg("--render")
        .arg(url)
        .arg(png)
        .arg(format!("{}x{}", a.viewport.0, a.viewport.1));
    cmd.env("LUCINANET_RASTER", &a.raster)
        .env("LUCINANET_CONSOLE", "1")
        .env("LUCINANET_RENDER_HOLD", hold.to_string())
        .env("LUCINANET_PROFILE_DIR", "none")
        .stdout(Stdio::from(sink))
        .stderr(Stdio::from(err));
    if let Some(dirs) = &a.font_dirs {
        cmd.env("LUCINANET_FONT_DIRS", dirs);
    }
    if let Some(js) = user_js {
        cmd.env("LUCINANET_USER_JS", js);
    }
    let status = cmd.status().map_err(|e| format!("timeout/{}: {e}", a.bin.display()))?;
    use std::os::unix::process::ExitStatusExt;
    let code = status.code();
    Ok(Rendered { code, signal: status.signal(), timed_out: code == Some(124), ms: started.elapsed().as_millis() })
}

fn test_url(a: &Args, path: &str) -> String {
    format!("{}/{}", a.base_url, path.trim_start_matches('/'))
}

/// References are root-relative in the manifest; a few are relative to the test's own directory.
fn ref_url(a: &Args, test_path: &str, reference: &str) -> String {
    if reference.starts_with("http://") || reference.starts_with("https://") {
        return reference.to_string();
    }
    if reference.starts_with('/') {
        return format!("{}{}", a.base_url, reference);
    }
    let dir = test_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    format!("{}/{dir}/{reference}", a.base_url)
}

fn fnv(s: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// References keep their directory layout under <out>/refs, which is readable and collision-free; a
/// query or fragment gets a hash suffix because it can change the rendering.
fn ref_png(out: &Path, url: &str) -> PathBuf {
    let (path, suffix) = match url.find(['?', '#']) {
        Some(i) => (&url[..i], format!("-{:08x}", fnv(&url[i..]) as u32)),
        None => (url, String::new()),
    };
    let rel = path.split("://").nth(1).and_then(|r| r.split_once('/')).map(|(_, p)| p).unwrap_or(path);
    out.join("refs").join(format!("{}{suffix}.png", rel.trim_start_matches('/')))
}

enum RefState {
    Rendering,
    Ready(PathBuf),
    Failed(String),
}

/// Many tests share one reference. Render it once; with --jobs > 1 the first worker to ask for a URL
/// renders it and the others wait on the condvar rather than rendering it again.
struct RefCache {
    inner: Mutex<HashMap<String, RefState>>,
    cv: Condvar,
}

impl RefCache {
    fn new() -> Self {
        RefCache { inner: Mutex::new(HashMap::new()), cv: Condvar::new() }
    }

    fn get(&self, a: &Args, url: &str) -> Result<PathBuf, String> {
        let mut g = self.inner.lock().unwrap();
        loop {
            match g.get(url) {
                Some(RefState::Ready(p)) => return Ok(p.clone()),
                Some(RefState::Failed(e)) => return Err(e.clone()),
                Some(RefState::Rendering) => g = self.cv.wait(g).unwrap(),
                None => {
                    g.insert(url.to_string(), RefState::Rendering);
                    break;
                },
            }
        }
        drop(g);
        let png = ref_png(&a.out, url);
        let log = png.with_extension("log");
        let result = (|| {
            let r = render(a, url, &png, &log, a.hold, None)?;
            if r.timed_out {
                return Err(format!("reference timed out after {} s", a.timeout));
            }
            if r.crashed() {
                return Err(format!("reference render exited {:?} signal {:?}", r.code, r.signal));
            }
            if !png.is_file() {
                return Err("reference render wrote no PNG".to_string());
            }
            Ok(png)
        })();
        let mut g = self.inner.lock().unwrap();
        g.insert(
            url.to_string(),
            match &result {
                Ok(p) => RefState::Ready(p.clone()),
                Err(e) => RefState::Failed(e.clone()),
            },
        );
        self.cv.notify_all();
        result
    }
}

// --------------------------------------------------------------------------------------------
// Scoring one test
// --------------------------------------------------------------------------------------------

/// What tests/media/wpt.sh's grep does, parsed instead: LucinaNet prefixes console lines, so the
/// markers are found anywhere in the line.
fn parse_harness(log: &str) -> (HashMap<String, u64>, Option<String>, Vec<String>) {
    let mut counts: HashMap<String, u64> = HashMap::new();
    let mut harness = None;
    let mut failures = Vec::new();
    for line in log.lines() {
        if let Some(i) = line.find("wpt-result: ") {
            let rest = &line[i + "wpt-result: ".len()..];
            let name = rest.split_whitespace().next().unwrap_or("").to_string();
            *counts.entry(name.clone()).or_insert(0) += 1;
            if name != "PASS" && failures.len() < 20 {
                failures.push(rest.chars().take(300).collect());
            }
        }
        if let Some(i) = line.find("wpt-done: harness=") {
            let rest = &line[i + "wpt-done: harness=".len()..];
            harness = Some(rest.split_whitespace().next().unwrap_or("").to_string());
        }
    }
    (counts, harness, failures)
}

fn shot_paths(a: &Args, t: &manifest::Test) -> (PathBuf, PathBuf) {
    let stem = t.artifact();
    (a.out.join("shots").join(format!("{stem}.png")), a.out.join("logs").join(format!("{stem}.log")))
}

/// Total wall time for the test, including its references and the comparison.
fn run_test(a: &Args, t: &manifest::Test, cache: &RefCache) -> Value {
    let started = Instant::now();
    let mut r = score_test(a, t, cache);
    r["ms"] = json!(started.elapsed().as_millis());
    r
}

fn score_test(a: &Args, t: &manifest::Test, cache: &RefCache) -> Value {
    let (png, log) = shot_paths(a, t);
    let dir = t.dir_under(&a.includes);
    let mut result = json!({
        "path": t.path, "type": t.kind, "dir": dir, "status": "FAIL",
        "png": png.display().to_string(), "log": log.display().to_string(),
    });
    let fail = |result: &mut Value, status: &str, reason: String| {
        result["status"] = json!(status);
        result["reason"] = json!(reason);
    };

    let hold = if t.kind == "testharness" { a.harness_hold } else { a.hold };
    let js = if t.kind == "testharness" { Some(a.report_js.as_path()) } else { None };
    let r = match render(a, &test_url(a, &t.path), &png, &log, hold, js) {
        Ok(r) => r,
        Err(e) => {
            fail(&mut result, "ERROR", e);
            return result;
        },
    };
    result["render_ms"] = json!(r.ms);
    result["render_exit"] = json!(r.code);
    if r.timed_out {
        fail(&mut result, "TIMEOUT", format!("render timed out after {} s", a.timeout));
        return result;
    }
    if t.kind == "crashtest" {
        // A crashtest passes when the browser survives it. `--render` hard-exits 0 on success, so a
        // non-zero code or a signal is the crash; a panic caught in a worker thread is only a note.
        let text = fs::read_to_string(&log).unwrap_or_default();
        let panicked = text.contains("panicked at");
        result["panic_in_log"] = json!(panicked);
        if r.crashed() {
            fail(&mut result, "FAIL", format!("crash: exit {:?} signal {:?}", r.code, r.signal));
        } else {
            result["status"] = json!("PASS");
        }
        return result;
    }
    if r.crashed() {
        fail(&mut result, "FAIL", format!("render crashed: exit {:?} signal {:?}", r.code, r.signal));
        return result;
    }
    if t.kind == "testharness" {
        let text = fs::read_to_string(&log).unwrap_or_default();
        let (counts, harness, failures) = parse_harness(&text);
        let n = |k: &str| counts.get(k).copied().unwrap_or(0);
        let total: u64 = counts.values().sum();
        result["subtests"] = json!({
            "total": total, "pass": n("PASS"), "fail": n("FAIL"), "timeout": n("TIMEOUT"),
            "notrun": n("NOTRUN"), "precondition_failed": n("PRECONDITION_FAILED"),
        });
        result["harness"] = json!(harness);
        result["subtest_failures"] = json!(failures);
        match harness.as_deref() {
            None => fail(&mut result, "ERROR", "no wpt-done line: testharness.js never completed".into()),
            Some("OK") => {
                // WPT scores a file with no subtests and an OK harness as a pass; it is flagged here so
                // a report.js that never attached cannot hide behind it.
                result["no_subtests"] = json!(total == 0);
                if n("FAIL") + n("TIMEOUT") + n("NOTRUN") + n("PRECONDITION_FAILED") == 0 {
                    result["status"] = json!("PASS");
                } else {
                    fail(&mut result, "FAIL", format!("{}/{} subtests pass, harness=OK", n("PASS"), total));
                }
            },
            Some(other) => fail(&mut result, "FAIL", format!("harness={other}")),
        }
        return result;
    }

    // reftest
    if !png.is_file() {
        fail(&mut result, "ERROR", "render wrote no PNG".into());
        return result;
    }
    if t.refs.is_empty() {
        fail(&mut result, "ERROR", "reftest has no references in the manifest".into());
        return result;
    }
    let mut relations = Vec::new();
    let mut all_hold = true;
    for rel in &t.refs {
        let url = ref_url(a, t.file(), &rel.url);
        let fuzzy = t.fuzzy_for(&rel.url);
        let mut entry = json!({
            "ref": rel.url, "op": rel.op, "url": url,
            "fuzzy": [fuzzy.max_difference, fuzzy.max_pixels],
        });
        match cache.get(a, &url) {
            Err(e) => {
                all_hold = false;
                entry["pass"] = json!(false);
                entry["error"] = json!(e);
            },
            Ok(ref_png) => {
                entry["ref_png"] = json!(ref_png.display().to_string());
                match compare::compare(&png, &ref_png, fuzzy.max_difference) {
                    Err(e) => {
                        all_hold = false;
                        entry["pass"] = json!(false);
                        entry["error"] = json!(e);
                    },
                    Ok(d) => {
                        let holds = d.holds(&rel.op, fuzzy);
                        all_hold &= holds;
                        entry["pass"] = json!(holds);
                        entry["blank"] = json!(d.test_uniform && d.ref_uniform && d.differing == 0);
                        entry["pixels"] = json!({
                            "differing": d.differing, "differing_over_max_difference": d.differing_over,
                            "max_difference": d.max_difference, "total": d.total_pixels,
                            "width": d.width, "height": d.height, "size_mismatch": d.size_mismatch,
                        });
                    },
                }
            },
        }
        relations.push(entry);
    }
    result["relations"] = json!(relations);
    if all_hold {
        result["status"] = json!("PASS");
        // A pass where every reference was a blank image matching a blank test rendered nothing.
        if relations.iter().all(|r| r["blank"].as_bool() == Some(true)) {
            result["blank_pass"] = json!(true);
        }
    } else {
        result["reason"] = json!("one or more references did not hold");
    }
    result
}

/// The one-line-per-test stdout form, with the actual pixel counts so a failure is diagnosable.
fn line(r: &Value) -> String {
    let status = r["status"].as_str().unwrap_or("?");
    let kind = r["type"].as_str().unwrap_or("?");
    let path = r["path"].as_str().unwrap_or("?");
    let ms = r["ms"].as_u64().unwrap_or(0);
    let detail = if let Some(rels) = r["relations"].as_array() {
        rels.iter()
            .map(|e| {
                let op = e["op"].as_str().unwrap_or("==");
                let f = e["fuzzy"].as_array();
                let (fd, fp) = (
                    f.and_then(|v| v.first()).and_then(Value::as_u64).unwrap_or(0),
                    f.and_then(|v| v.get(1)).and_then(Value::as_u64).unwrap_or(0),
                );
                match e["pixels"].as_object() {
                    Some(p) => format!(
                        "{op} px={}/{} maxdiff={} fuzzy={fd}/{fp}{}",
                        p["differing"].as_u64().unwrap_or(0),
                        p["total"].as_u64().unwrap_or(0),
                        p["max_difference"].as_u64().unwrap_or(0),
                        if p["size_mismatch"].as_bool().unwrap_or(false) { " SIZE-MISMATCH" } else { "" },
                    ),
                    None => format!("{op} ERROR {}", e["error"].as_str().unwrap_or("")),
                }
            })
            .collect::<Vec<_>>()
            .join("; ")
    } else if let Some(s) = r["subtests"].as_object() {
        format!(
            "{}/{} subtests harness={}{}",
            s["pass"].as_u64().unwrap_or(0),
            s["total"].as_u64().unwrap_or(0),
            r["harness"].as_str().unwrap_or("none"),
            if r["no_subtests"].as_bool().unwrap_or(false) { " (no subtests)" } else { "" },
        )
    } else {
        r["reason"].as_str().unwrap_or("").to_string()
    };
    format!("{status:7} {kind:11} {path}  {detail}  {ms}ms")
}

// --------------------------------------------------------------------------------------------
// Summary
// --------------------------------------------------------------------------------------------

fn tally() -> Value {
    json!({"pass": 0, "fail": 0, "error": 0, "timeout": 0, "total": 0})
}

fn bump(bucket: &mut Value, status: &str) {
    let key = match status {
        "PASS" => "pass",
        "TIMEOUT" => "timeout",
        "ERROR" => "error",
        _ => "fail",
    };
    bucket[key] = json!(bucket[key].as_u64().unwrap_or(0) + 1);
    bucket["total"] = json!(bucket["total"].as_u64().unwrap_or(0) + 1);
}

fn with_rate(mut bucket: Value) -> Value {
    let total = bucket["total"].as_u64().unwrap_or(0);
    let pass = bucket["pass"].as_u64().unwrap_or(0);
    bucket["pass_rate"] = json!(if total == 0 { 0.0 } else { pass as f64 / total as f64 });
    bucket
}

fn summarize(a: &Args, selected: usize, results: &[Value], elapsed: u128) -> Value {
    let (mut total, mut by_type, mut by_dir) = (tally(), serde_json::Map::new(), serde_json::Map::new());
    let (mut subtests, mut no_subtests, mut panics, mut blank_passes) = (tally(), 0u64, 0u64, 0u64);
    for r in results {
        let status = r["status"].as_str().unwrap_or("FAIL");
        bump(&mut total, status);
        let kind = r["type"].as_str().unwrap_or("?");
        bump(by_type.entry(kind).or_insert_with(tally), status);
        let dir = r["dir"].as_str().unwrap_or("?");
        bump(by_dir.entry(dir).or_insert_with(tally), status);
        if let Some(s) = r["subtests"].as_object() {
            for k in ["pass", "fail", "timeout", "notrun", "precondition_failed"] {
                subtests[k] = json!(subtests[k].as_u64().unwrap_or(0) + s.get(k).and_then(Value::as_u64).unwrap_or(0));
            }
            subtests["total"] = json!(subtests["total"].as_u64().unwrap_or(0) + s.get("total").and_then(Value::as_u64).unwrap_or(0));
        }
        if r["no_subtests"].as_bool().unwrap_or(false) {
            no_subtests += 1;
        }
        if r["panic_in_log"].as_bool().unwrap_or(false) {
            panics += 1;
        }
        if r["blank_pass"].as_bool().unwrap_or(false) {
            blank_passes += 1;
        }
    }
    let pass = total["pass"].as_u64().unwrap_or(0);
    let n = total["total"].as_u64().unwrap_or(0);
    json!({
        "started_unix": SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        "binary": a.bin.display().to_string(),
        "manifest": a.manifest.display().to_string(),
        "base_url": a.base_url,
        "viewport": format!("{}x{}", a.viewport.0, a.viewport.1),
        "includes": a.includes, "excludes": a.excludes, "types": a.types,
        "limit": a.limit, "jobs": a.jobs, "hold": a.hold, "harness_hold": a.harness_hold,
        "selected": selected, "run": n, "elapsed_ms": elapsed,
        "vacuous": selected == 0,
        "total": with_rate(total),
        "by_type": by_type.into_iter().map(|(k, v)| (k, with_rate(v))).collect::<serde_json::Map<_, _>>(),
        "by_dir": by_dir.into_iter().map(|(k, v)| (k, with_rate(v))).collect::<serde_json::Map<_, _>>(),
        "subtests": subtests,
        "testharness_ok_with_no_subtests": no_subtests,
        "crashtests_with_a_panic_in_the_log": panics,
        "blank_passes": blank_passes,
        "pass_rate": if n == 0 { 0.0 } else { pass as f64 / n as f64 },
        "exit_code": if selected == 0 { 2 } else if pass == n { 0 } else { 1 },
    })
}

/// Keep the failures' PNGs and logs; drop the passes' and every reference no failure points at.
fn prune(a: &Args, results: &[Value]) {
    if a.keep_all {
        return;
    }
    let mut keep_refs: HashSet<String> = HashSet::new();
    for r in results {
        if r["status"].as_str() == Some("PASS") && r["blank_pass"].as_bool() != Some(true) {
            for key in ["png", "log"] {
                if let Some(p) = r[key].as_str() {
                    fs::remove_file(p).ok();
                }
            }
        } else if let Some(rels) = r["relations"].as_array() {
            for e in rels {
                if let Some(p) = e["ref_png"].as_str() {
                    keep_refs.insert(p.to_string());
                }
            }
        }
    }
    let refs = a.out.join("refs");
    let mut stack = vec![refs.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if !keep_refs.contains(&p.display().to_string()) {
                fs::remove_file(&p).ok();
            }
        }
    }
}

fn main() {
    let code = run();
    std::process::exit(code);
}

fn run() -> i32 {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().is_some_and(|a| a == "report") {
        return report::run(&argv[1..]);
    }
    let a = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("wpt-reftest: {e}");
            return 3;
        },
    };
    if let Err(e) = fs::create_dir_all(&a.out) {
        eprintln!("wpt-reftest: {}: {e}", a.out.display());
        return 3;
    }
    if !a.report_js.is_file() {
        if let Err(e) = fs::write(&a.report_js, REPORT_JS) {
            eprintln!("wpt-reftest: {}: {e}", a.report_js.display());
            return 3;
        }
    }
    let m = match manifest::load(&a.manifest) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("wpt-reftest: {e}");
            return 3;
        },
    };
    let sel = manifest::Select { types: &a.types, includes: &a.includes, excludes: &a.excludes, limit: a.limit };
    let tests = manifest::select(&m, &sel);
    eprintln!(
        "wpt-reftest: manifest v{} url_base={} selected {} tests (types={} includes={:?}) jobs={} viewport={}x{}",
        m.version, m.url_base, tests.len(), a.types.join(","), a.includes, a.jobs, a.viewport.0, a.viewport.1
    );
    if tests.is_empty() {
        eprintln!("wpt-reftest: ZERO tests selected -- vacuity is an error, check --include/--types/--exclude");
        let summary = summarize(&a, 0, &[], 0);
        let _ = fs::write(a.out.join("summary.json"), serde_json::to_string_pretty(&summary).unwrap_or_default());
        let _ = File::create(a.out.join("results.jsonl"));
        return 2;
    }
    // --include is a URL prefix, so a bare filename matches nothing. Warn per prefix rather than let
    // one half of a multi-prefix selection silently disappear.
    for inc in &a.includes {
        let p = inc.trim_start_matches('/').trim_end_matches('/');
        if !tests.iter().any(|t| t.path == p || t.path.starts_with(&format!("{p}/"))) {
            eprintln!("wpt-reftest: warning: --include {inc} matched 0 tests (it is a URL prefix from the WPT root)");
        }
    }
    if a.list {
        for t in &tests {
            println!("{}\t{}\trefs={}", t.kind, t.path, t.refs.len());
        }
        return 0;
    }
    if a.jobs > 2 {
        eprintln!("wpt-reftest: --jobs {} on a thermally limited box; 1 or 2 is the sane range", a.jobs);
    }

    let jsonl = match OpenOptions::new().create(true).truncate(true).write(true).open(a.out.join("results.jsonl")) {
        Ok(f) => Mutex::new(f),
        Err(e) => {
            eprintln!("wpt-reftest: results.jsonl: {e}");
            return 3;
        },
    };
    let print = Mutex::new(());
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Value>> = Mutex::new(Vec::with_capacity(tests.len()));
    let cache = RefCache::new();
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..a.jobs {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(t) = tests.get(i) else { break };
                    cool(a.max_c);
                    let r = run_test(&a, t, &cache);
                    let text = line(&r);
                    {
                        let _g = print.lock().unwrap();
                        println!("{text}");
                        if let Ok(mut f) = jsonl.lock() {
                            let _ = writeln!(f, "{}", serde_json::to_string(&r).unwrap_or_default());
                            let _ = f.flush();
                        }
                    }
                    results.lock().unwrap().push(r);
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by(|x, y| x["path"].as_str().cmp(&y["path"].as_str()));
    let summary = summarize(&a, tests.len(), &results, started.elapsed().as_millis());
    prune(&a, &results);
    let pretty = serde_json::to_string_pretty(&summary).unwrap_or_default();
    if let Err(e) = fs::write(a.out.join("summary.json"), &pretty) {
        eprintln!("wpt-reftest: summary.json: {e}");
    }
    let t = &summary["total"];
    println!(
        "\ntotal: {}/{} pass ({} fail, {} error, {} timeout, {} blank) in {} ms -- {}",
        t["pass"].as_u64().unwrap_or(0),
        t["total"].as_u64().unwrap_or(0),
        t["fail"].as_u64().unwrap_or(0),
        t["error"].as_u64().unwrap_or(0),
        t["timeout"].as_u64().unwrap_or(0),
        summary["blank_passes"].as_u64().unwrap_or(0),
        started.elapsed().as_millis(),
        a.out.join("summary.json").display()
    );
    summary["exit_code"].as_i64().unwrap_or(1) as i32
}