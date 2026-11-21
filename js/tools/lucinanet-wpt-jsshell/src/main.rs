//! LucinaNet WPT jsshell: a runner for the `.any.js` variant of a Web Platform Test,
//! which runs the test in the Brimstone engine used as a JavaScript shell.
//!
//! The parent discovers tests and runs each one in a child process
//! (`lucinanet-wpt-jsshell --wpt <root> --test <file>`), so an engine fault costs one
//! file and not the run: a child that dies, hangs past its deadline or never reports is
//! written up as `crash` or `timeout` by the parent, and every other file still gets a
//! result. A child prints its records on stdout and nothing else goes there, because
//! test output is written to stderr.

mod exec;
mod meta;

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::exec::STATUS_NAMES;

struct Args {
    wpt: PathBuf,
    filters: Vec<String>,
    out: Option<PathBuf>,
    summary: Option<PathBuf>,
    /// Wall-clock limit for one file, given to the child and enforced by it.
    timeout: Duration,
    /// On top of `timeout`, before the parent kills a child that has not reported.
    grace: Duration,
    jobs: usize,
    /// Set by `--test`: this process runs one file instead of a corpus.
    test: Option<PathBuf>,
}

fn value_after(flag: &str, it: &mut std::iter::Skip<std::env::Args>) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn secs_after(flag: &str, it: &mut std::iter::Skip<std::env::Args>) -> Result<u64, String> {
    value_after(flag, it)?
        .parse()
        .map_err(|_| format!("{flag} must be a number"))
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        wpt: PathBuf::new(),
        filters: Vec::new(),
        out: None,
        summary: None,
        timeout: Duration::from_secs(10),
        grace: Duration::from_secs(20),
        jobs: 4,
        test: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--wpt" => args.wpt = PathBuf::from(value_after("--wpt", &mut it)?),
            "--test" => args.test = Some(PathBuf::from(value_after("--test", &mut it)?)),
            "--filter" => args.filters.push(value_after("--filter", &mut it)?),
            "--out" => args.out = Some(PathBuf::from(value_after("--out", &mut it)?)),
            "--summary" => {
                args.summary = Some(PathBuf::from(value_after("--summary", &mut it)?))
            }
            "--timeout-secs" => args.timeout = Duration::from_secs(secs_after("--timeout-secs", &mut it)?),
            "--grace-secs" => args.grace = Duration::from_secs(secs_after("--grace-secs", &mut it)?),
            "--jobs" => args.jobs = secs_after("--jobs", &mut it)? as usize,
            "--help" | "-h" => {
                println!("usage: lucinanet-wpt-jsshell --wpt <root> [--filter <substr>]... --out <results.jsonl>");
                println!("       [--timeout-secs 10] [--jobs 4] [--summary <file>]");
                println!("internal: lucinanet-wpt-jsshell --wpt <root> --test <file.any.js> [--timeout-secs 10]");
                return Err("HELP".to_string());
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if args.wpt.as_os_str().is_empty() {
        return Err("--wpt <root> is required".to_string());
    }
    if args.test.is_none() && args.out.is_none() {
        return Err("--out <results.jsonl> is required for a corpus run".to_string());
    }
    if args.jobs == 0 {
        return Err("--jobs must be at least 1".to_string());
    }
    Ok(args)
}

/// A candidate test, with its path relative to the WPT root.
struct Candidate {
    path: PathBuf,
    rel: String,
    jsshell: bool,
}

/// The directory the filters narrow to: their common static path prefix, so that
/// `--filter wasm/jsapi/` walks that directory and not the whole corpus.
fn walk_root(wpt: &Path, filters: &[String]) -> PathBuf {
    let mut common: Option<Vec<&str>> = None;
    for filter in filters {
        let dir = match filter.rfind('/') {
            Some(i) => &filter[..i],
            None => "",
        };
        let parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
        common = Some(match common {
            None => parts,
            Some(previous) => {
                let mut both = Vec::new();
                for (a, b) in previous.iter().zip(parts.iter()) {
                    if a != b {
                        break;
                    }
                    both.push(*a);
                }
                both
            }
        });
    }
    let mut root = wpt.to_path_buf();
    for part in common.unwrap_or_default() {
        root.push(part);
    }
    if root.is_dir() {
        root
    } else {
        wpt.to_path_buf()
    }
}

/// Every `.any.js` under the filtered directory, in path order, with its jsshell flag.
fn discover(args: &Args) -> Vec<Candidate> {
    let mut files = Vec::new();
    walk(&walk_root(&args.wpt, &args.filters), &mut files);
    files.sort();
    let mut candidates = Vec::new();
    for path in files {
        // The `.any.js` file *is* the jsshell variant of an `any` test
        // (`tools/manifest/sourcefile.py:113`); `.any.html`, `.any.worker.html` and the
        // rest belong to a runner with a browser or a worker in it.
        if !path
            .file_name()
            .map(|name| name.to_string_lossy().ends_with(".any.js"))
            .unwrap_or(false)
        {
            continue;
        }
        let rel = path
            .strip_prefix(&args.wpt)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if !args.filters.is_empty() && !args.filters.iter().any(|f| rel.contains(f.as_str())) {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        let jsshell = meta::parse(&source).is_jsshell();
        candidates.push(Candidate { path, rel, jsshell });
    }
    candidates
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

/// One file record, the shape both the child and the parent write.
fn file_record(rel: &str, result: &str, reason: &str) -> Value {
    json!({
        "kind": "file",
        "file": rel,
        "result": result,
        "reason": reason,
        "harness_status": "NONE",
        "harness_message": "",
        "subtests": 0,
        "pass": 0,
        "fail": 0,
        "timeout": 0,
        "notrun": 0,
        "precondition_failed": 0,
        "secs": 0.0,
        "runner_errors": [],
    })
}

fn subtest_record(rel: &str, record: &Value) -> Value {
    json!({
        "kind": "subtest",
        "file": rel,
        "name": record.get("name").and_then(Value::as_str).unwrap_or(""),
        "status": record.get("status").and_then(Value::as_str).unwrap_or("NOTRUN"),
        "message": record.get("message").and_then(Value::as_str).unwrap_or(""),
    })
}

fn main() -> std::process::ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            if error != "HELP" {
                eprintln!("error: {error}");
            }
            return std::process::ExitCode::from(if error == "HELP" { 0 } else { 2 });
        }
    };
    let code = match &args.test {
        Some(test) => run_worker(&args, test),
        None => run_parent(&args),
    };
    std::process::ExitCode::from(code)
}

/// One file, in this process: run it and print its records on stdout.
fn run_worker(args: &Args, test: &Path) -> u8 {
    let source = match std::fs::read_to_string(test) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("cannot read {}: {error}", test.display());
            return 2;
        }
    };
    let meta = meta::parse(&source);
    if !meta.is_jsshell() {
        eprintln!("{} declares no jsshell global; the parent reports it", test.display());
        return 2;
    }
    let engine = match mozjs::rust::JSEngine::init() {
        Ok(engine) => engine,
        Err(_) => {
            eprintln!("cannot start the engine");
            return 2;
        }
    };
    let (subtests, outcome) = exec::run_file(&engine, &args.wpt, test, &meta, args.timeout);
    let rel = test
        .strip_prefix(&args.wpt)
        .unwrap_or(test)
        .to_string_lossy()
        .replace('\\', "/");
    let counts = exec::counts(&subtests);

    let mut out = std::io::stdout().lock();
    let mut lines: Vec<String> = subtests
        .iter()
        .map(|(_, record)| subtest_record(&rel, record).to_string())
        .collect();
    lines.push(
        json!({
            "kind": "file",
            "file": rel,
            "result": outcome.result,
            "reason": outcome.reason,
            "harness_status": outcome.harness,
            "harness_message": outcome.harness_message,
            "subtests": subtests.len(),
            "pass": counts[0],
            "fail": counts[1],
            "timeout": counts[2],
            "notrun": counts[3],
            "precondition_failed": counts[4],
            "secs": (outcome.secs * 1000.0).round() / 1000.0,
            "runner_errors": exec::runner_errors(),
        })
        .to_string(),
    );
    for line in lines {
        if writeln!(out, "{line}").is_err() || out.flush().is_err() {
            return 2;
        }
    }
    0
}

/// A live child and the lines it has printed.
struct Running {
    rel: String,
    deadline: Instant,
    child: Child,
    lines: Vec<String>,
    rx: Receiver<String>,
}

fn spawn_child(args: &Args, exe: &Path, test: &Path, rel: &str) -> Option<Running> {
    let mut child = Command::new(exe)
        .arg("--wpt")
        .arg(&args.wpt)
        .arg("--test")
        .arg(test)
        .arg("--timeout-secs")
        .arg(args.timeout.as_secs().max(1).to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    Some(Running {
        rel: rel.to_string(),
        deadline: Instant::now() + args.timeout + args.grace,
        child,
        lines: Vec::new(),
        rx,
    })
}

/// Read everything the child has printed, with a bound when it is already gone.
fn drain(child: &mut Running) {
    let until = Instant::now() + Duration::from_millis(500);
    loop {
        match child.rx.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => child.lines.push(line),
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {
                if Instant::now() >= until {
                    return;
                }
            }
        }
    }
}

/// Write up a finished child: its subtest lines, and its file record - the one it
/// printed, or the one the parent has to invent when it did not.
fn finish(mut child: Running, killed: bool, limit: Duration) -> (String, Vec<Value>, Value) {
    drain(&mut child);
    let mut subtests = Vec::new();
    let mut unexpected: Vec<String> = Vec::new();
    let mut file: Option<Value> = None;
    for line in std::mem::take(&mut child.lines) {
        match serde_json::from_str::<Value>(&line) {
            Ok(record) if record.get("kind").and_then(Value::as_str).is_some() => {
                if record["kind"] == json!("file") {
                    file = Some(record);
                } else {
                    subtests.push(record);
                }
            }
            // Not JSONL: an engine message that reached stdout. Kept out of the results,
            // and named, because it means the child did not run as planned.
            _ => unexpected.push(line),
        }
    }
    let status = child
        .child
        .try_wait()
        .ok()
        .flatten()
        .map(|exit| exit.code().map(|c| c.to_string()).unwrap_or_else(|| "a signal".to_string()))
        .unwrap_or_else(|| "unknown".to_string());
    let rel = child.rel;
    let record = match file {
        Some(record) => record,
        None => {
            let result = if killed { "timeout" } else { "crash" };
            let reason = if killed {
                format!("killed after {} s without a result", limit.as_secs())
            } else if unexpected.is_empty() {
                format!("worker exited with {status} and reported no result")
            } else {
                format!(
                    "worker exited with {status} and reported no result; stdout began with {:?}",
                    unexpected[0]
                )
            };
            file_record(&rel, result, &reason)
        }
    };
    (rel, subtests, record)
}

/// The parent: discover, run each jsshell file in a child, write the JSONL and summary.
fn run_parent(args: &Args) -> u8 {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(_) => return 2,
    };
    if !args.wpt.is_dir() {
        eprintln!("{} is not a directory", args.wpt.display());
        return 2;
    }
    let candidates = discover(args);
    if candidates.is_empty() {
        eprintln!("no `.any.js` test matched; refusing to report success");
        return 2;
    }
    let jsshell = candidates.iter().filter(|c| c.jsshell).count();
    eprintln!(
        "discovered {} .any.js files, {} of them declaring jsshell",
        candidates.len(),
        jsshell
    );

    // rel -> (subtest records, file record). A BTreeMap keeps the output in path order
    // however the children finish, so two runs of one engine diff clean.
    let mut done: BTreeMap<String, (Vec<Value>, Value)> = BTreeMap::new();
    let mut queue: VecDeque<Candidate> = candidates.into();
    let mut running: Vec<Running> = Vec::new();

    while !queue.is_empty() || !running.is_empty() {
        while running.len() < args.jobs {
            let Some(candidate) = queue.pop_front() else { break };
            if !candidate.jsshell {
                done.insert(
                    candidate.rel.clone(),
                    (
                        vec![],
                        file_record(&candidate.rel, "skipped", "not-jsshell"),
                    ),
                );
                continue;
            }
            match spawn_child(args, &exe, &candidate.path, &candidate.rel) {
                Some(child) => running.push(child),
                None => {
                    let rel = candidate.rel;
                    done.insert(
                        rel.clone(),
                        (vec![], file_record(&rel, "crash", "cannot spawn the worker")),
                    );
                }
            }
        }
        let mut idle = true;
        let mut i = 0;
        while i < running.len() {
            let mut pending = false;
            while let Ok(line) = running[i].rx.try_recv() {
                running[i].lines.push(line);
                pending = true;
            }
            if pending {
                idle = false;
            }
            let finished = match running[i].child.try_wait() {
                Ok(Some(_)) => Some(false),
                Ok(None) if Instant::now() >= running[i].deadline => {
                    // Our own child, past its deadline: stop it. A hang that outlives the
                    // grace is an engine fault, and it is reported, not swallowed.
                    let _ = running[i].child.kill();
                    let _ = running[i].child.wait();
                    Some(true)
                }
                Ok(None) => None,
                Err(_) => Some(false),
            };
            if let Some(killed) = finished {
                let limit = args.timeout + args.grace;
                let child = running.swap_remove(i);
                let (rel, subtests, record) = finish(child, killed, limit);
                eprintln!(
                    "{rel}: {}",
                    record.get("result").and_then(Value::as_str).unwrap_or("?")
                );
                done.insert(rel, (subtests, record));
                idle = false;
                continue;
            }
            i += 1;
        }
        if idle && !running.is_empty() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    let mut lines: Vec<Value> = Vec::new();
    for (rel, (subtests, record)) in &done {
        for subtest in subtests {
            lines.push(subtest_record(rel, subtest));
        }
        lines.push(record.clone());
    }

    let summary = summarize(args, &lines, done.len());
    let mut text = String::new();
    for line in &lines {
        text.push_str(&line.to_string());
        text.push('\n');
    }
    text.push_str(&summary.to_string());
    text.push('\n');

    if let Some(out) = &args.out {
        if let Err(error) = std::fs::write(out, &text) {
            eprintln!("cannot write {}: {error}", out.display());
            return 2;
        }
    }
    if let Some(path) = &args.summary {
        if let Err(error) = std::fs::write(path, format!("{summary}\n")) {
            eprintln!("cannot write {}: {error}", path.display());
            return 2;
        }
    }
    println!("{}", serde_json::to_string_pretty(&summary).unwrap_or_default());

    let crashed = summary["files_crashed"].as_u64().unwrap_or(0);
    if crashed > 0 {
        eprintln!("{crashed} file(s) crashed");
        return 1;
    }
    0
}

/// Counts over the records that were written: files by result, subtests by status.
fn summarize(args: &Args, lines: &[Value], files: usize) -> Value {
    let mut counts = vec![0usize; STATUS_NAMES.len()];
    let mut subtests = 0usize;
    let mut ran = 0usize;
    let mut skipped = 0usize;
    let mut harness_error = 0usize;
    let mut error = 0usize;
    let mut timeout = 0usize;
    let mut crashed = 0usize;
    for line in lines {
        match line.get("kind").and_then(Value::as_str) {
            Some("subtest") => {
                subtests += 1;
                let status = line["status"].as_str().unwrap_or("");
                if let Some(pos) = STATUS_NAMES.iter().position(|name| *name == status) {
                    counts[pos] += 1;
                }
            }
            Some("file") => match line["result"].as_str() {
                Some("ran") => ran += 1,
                Some("skipped") => skipped += 1,
                Some("harness-error") => harness_error += 1,
                Some("error") => error += 1,
                Some("timeout") => timeout += 1,
                _ => crashed += 1,
            },
            _ => {}
        }
    }
    json!({
        "kind": "summary",
        "tool": "lucinanet-wpt-jsshell",
        "wpt": args.wpt,
        "filters": args.filters,
        "timeout_secs": args.timeout.as_secs(),
        "files": files,
        "files_run": ran,
        "files_skipped": skipped,
        "files_harness_error": harness_error,
        "files_error": error,
        "files_timeout": timeout,
        "files_crashed": crashed,
        "subtests": subtests,
        "pass": counts[0],
        "fail": counts[1],
        "timeout": counts[2],
        "notrun": counts[3],
        "precondition_failed": counts[4],
    })
}
