//! t262: a test262 runner written against the mozjs crate API.
//!
//! The parent process discovers tests and spawns `--jobs` long-lived worker
//! processes (`t262 --worker`). Each worker owns one SpiderMonkey `JSEngine`
//! and executes one test variant per line received on stdin, answering with
//! one JSON result line on stdout.

mod exec;
mod meta;

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use exec::{run_job, Job, TestResult};
use meta::{parse_front_matter, variants_for};

struct Args {
    test262: PathBuf,
    filters: Vec<String>,
    jobs: usize,
    timeout_ms: u64,
    out: Option<PathBuf>,
    summary: Option<PathBuf>,
    with_intl: bool,
    with_staging: bool,
    worker: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        test262: PathBuf::new(),
        filters: Vec::new(),
        jobs: 4,
        timeout_ms: 10_000,
        out: None,
        summary: None,
        with_intl: false,
        with_staging: false,
        worker: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--worker" => args.worker = true,
            "--test262" => args.test262 = PathBuf::from(it.next().ok_or("--test262 needs a value")?),
            "--filter" => args.filters.push(it.next().ok_or("--filter needs a value")?),
            "--jobs" => {
                args.jobs = it
                    .next()
                    .ok_or("--jobs needs a value")?
                    .parse()
                    .map_err(|_| "--jobs must be a number")?
            }
            "--timeout-ms" => {
                args.timeout_ms = it
                    .next()
                    .ok_or("--timeout-ms needs a value")?
                    .parse()
                    .map_err(|_| "--timeout-ms must be a number")?
            }
            "--out" => args.out = Some(PathBuf::from(it.next().ok_or("--out needs a value")?)),
            "--summary" => {
                args.summary = Some(PathBuf::from(it.next().ok_or("--summary needs a value")?))
            }
            "--with-intl" => args.with_intl = true,
            "--with-staging" => args.with_staging = true,
            "--help" | "-h" => {
                println!("t262 --test262 <dir> [--filter <substring>]... [--jobs N] [--timeout-ms 10000] [--out <results.jsonl>] [--summary <summary.json>] [--with-intl] [--with-staging]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if !args.worker && args.test262.as_os_str().is_empty() {
        return Err("--test262 <dir> is required".to_string());
    }
    Ok(args)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().map(|e| e == "js").unwrap_or(false) {
            out.push(p);
        }
    }
}

fn discover(args: &Args) -> Vec<(PathBuf, Vec<meta::Variant>)> {
    let test_dir = args.test262.join("test");
    let mut files = Vec::new();
    walk(&test_dir, &mut files);
    files.sort();
    let mut tests = Vec::new();
    for f in files {
        let name = f.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if name.ends_with("_FIXTURE.js") {
            continue;
        }
        let rel = f.strip_prefix(&test_dir).unwrap_or(&f).to_string_lossy().into_owned();
        if !args.with_intl && rel.starts_with("intl402/") {
            continue;
        }
        if !args.with_staging && rel.starts_with("staging/") {
            continue;
        }
        if !args.filters.is_empty() && !args.filters.iter().any(|flt| rel.contains(flt.as_str())) {
            continue;
        }
        let source = std::fs::read_to_string(&f).unwrap_or_default();
        let fm = parse_front_matter(&source).unwrap_or_default();
        let variants = variants_for(&fm);
        if !variants.is_empty() {
            tests.push((f, variants));
        }
    }
    tests
}

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    cond: Condvar,
    shutdown: Mutex<bool>,
}

enum WorkerMsg {
    Line(String),
    Eof,
}

fn spawn_worker(exe: &PathBuf) -> std::io::Result<(Child, ChildStdin, Receiver<WorkerMsg>)> {
    let mut child = Command::new(exe)
        .arg("--worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(l) => {
                    if tx.send(WorkerMsg::Line(l)).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(WorkerMsg::Eof);
    });
    Ok((child, stdin, rx))
}

fn worker_loop(exe: PathBuf, shared: Arc<Shared>, results: Sender<TestResult>, timeout: Duration) {
    let mut child: Option<Child> = None;
    let mut stdin: Option<ChildStdin> = None;
    let mut rx: Option<Receiver<WorkerMsg>> = None;
    loop {
        // (Re)spawn the worker if needed.
        if child.is_none() {
            match spawn_worker(&exe) {
                Ok((c, s, r)) => {
                    child = Some(c);
                    stdin = Some(s);
                    rx = Some(r);
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
            }
        }
        // Pop the next job.
        let job = {
            let mut q = shared.queue.lock().unwrap();
            loop {
                if let Some(j) = q.pop_front() {
                    break Some(j);
                }
                if *shared.shutdown.lock().unwrap() {
                    break None;
                }
                q = shared.cond.wait(q).unwrap();
            }
        };
        let Some(job) = job else { break };

        // A job must always produce exactly one result, even if this thread
        // panics mid-way; otherwise the parent deadlocks waiting for it.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let line = serde_json::to_string(&job).unwrap();
            let send_ok = stdin
                .as_mut()
                .map(|s| writeln!(s, "{line}").and_then(|_| s.flush()).is_ok())
                .unwrap_or(false);
            if !send_ok {
                return (true, "crash".to_string(), "worker write failed".to_string());
            }
            let r = rx.as_ref().unwrap();
            match r.recv_timeout(timeout) {
                Ok(WorkerMsg::Line(l)) => match serde_json::from_str::<TestResult>(&l) {
                    Ok(res) => return (false, res.result, res.message),
                    Err(_) => {
                        return (
                            true,
                            "crash".to_string(),
                            "unparseable worker output".to_string(),
                        )
                    }
                },
                Ok(WorkerMsg::Eof) => return (true, "crash".to_string(), "worker died".to_string()),
                Err(_) => {
                    return (
                        true,
                        "timeout".to_string(),
                        format!("exceeded {} ms", timeout.as_millis()),
                    )
                }
            }
        }));
        let (restart, result, message) = match outcome {
            Ok(t) => t,
            Err(_) => (true, "crash".to_string(), "worker manager panicked".to_string()),
        };
        if restart {
            if let Some(mut c) = child.take() {
                let _ = c.kill();
            }
        }
        results
            .send(TestResult {
                path: job.path.clone(),
                variant: job.variant.clone(),
                result,
                message,
                features: vec![],
            })
            .ok();
    }
}

fn run_worker() -> ! {
    let engine = match mozjs::rust::JSEngine::init() {
        Ok(e) => e,
        Err(_) => std::process::exit(2),
    };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let job: Job = match serde_json::from_str(&line) {
            Ok(j) => j,
            Err(_) => continue,
        };
        let res = run_job(&engine, &job);
        writeln!(out, "{}", serde_json::to_string(&res).unwrap()).ok();
        out.flush().ok();
    }
    std::process::exit(0);
}

#[derive(Default, Clone, serde::Serialize)]
struct Totals {
    pass: usize,
    fail: usize,
    timeout: usize,
    crash: usize,
    skipped: usize,
}

impl Totals {
    fn add(&mut self, result: &str) {
        match result {
            "pass" => self.pass += 1,
            "fail" => self.fail += 1,
            "timeout" => self.timeout += 1,
            "crash" => self.crash += 1,
            _ => self.skipped += 1,
        }
    }
    fn total(&self) -> usize {
        self.pass + self.fail + self.timeout + self.crash + self.skipped
    }
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    if args.worker {
        run_worker();
    }
    let exe = std::env::current_exe().unwrap();
    let tests = discover(&args);
    let mut jobs = VecDeque::new();
    for (path, variants) in &tests {
        for v in variants {
            jobs.push_back(Job {
                path: path.to_string_lossy().into_owned(),
                variant: v.as_str().to_string(),
                root: args.test262.to_string_lossy().into_owned(),
            });
        }
    }
    let total = jobs.len();
    eprintln!("discovered {total} test variants");

    let shared = Arc::new(Shared {
        queue: Mutex::new(jobs),
        cond: Condvar::new(),
        shutdown: Mutex::new(false),
    });
    let (results_tx, results_rx) = channel::<TestResult>();
    let mut handles = Vec::new();
    for _ in 0..args.jobs.max(1) {
        let shared = Arc::clone(&shared);
        let tx = results_tx.clone();
        let exe = exe.clone();
        let timeout = Duration::from_millis(args.timeout_ms);
        handles.push(std::thread::spawn(move || worker_loop(exe, shared, tx, timeout)));
    }
    drop(results_tx);

    let mut results: Vec<TestResult> = Vec::with_capacity(total);
    for res in results_rx.iter() {
        results.push(res);
        if results.len() >= total {
            break;
        }
    }
    *shared.shutdown.lock().unwrap() = true;
    shared.cond.notify_all();
    for h in handles {
        h.join().ok();
    }
    results.sort_by(|a, b| a.path.cmp(&b.path).then(a.variant.cmp(&b.variant)));

    // Write results.jsonl
    if let Some(out_path) = &args.out {
        let mut f = std::fs::File::create(out_path).unwrap();
        for r in &results {
            let line = serde_json::json!({
                "path": r.path,
                "variant": r.variant,
                "result": r.result,
                "message": r.message,
            });
            writeln!(f, "{line}").ok();
        }
    }

    // Summary
    let mut overall = Totals::default();
    let mut per_dir: BTreeMap<String, Totals> = BTreeMap::new();
    let mut feature_fail: BTreeMap<String, usize> = BTreeMap::new();
    let test_prefix = format!("{}/test/", args.test262.to_string_lossy());
    for r in &results {
        overall.add(&r.result);
        let rel = r.path.strip_prefix(&test_prefix).unwrap_or(&r.path);
        let dir = rel.split('/').next().unwrap_or("").to_string();
        per_dir.entry(dir).or_default().add(&r.result);
        if r.result == "fail" {
            for f in &r.features {
                *feature_fail.entry(f.clone()).or_insert(0) += 1;
            }
        }
    }
    let mut top_features: Vec<(String, usize)> = feature_fail.into_iter().collect();
    top_features.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    top_features.truncate(30);

    let pass_pct = if overall.total() > 0 {
        overall.pass as f64 * 100.0 / overall.total() as f64
    } else {
        0.0
    };
    let summary = serde_json::json!({
        "total": overall.total(),
        "pass": overall.pass,
        "fail": overall.fail,
        "timeout": overall.timeout,
        "crash": overall.crash,
        "skipped": overall.skipped,
        "pass_pct": format!("{pass_pct:.2}"),
        "per_dir": per_dir,
        "top_failing_features": top_features.iter().map(|(f, n)| serde_json::json!({"feature": f, "failures": n})).collect::<Vec<_>>(),
    });
    if let Some(s_path) = &args.summary {
        std::fs::write(s_path, serde_json::to_string_pretty(&summary).unwrap()).ok();
    }
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
}
