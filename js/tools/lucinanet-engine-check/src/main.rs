/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! LucinaNet's engine acceptance gate: a fresh process per mozjs test.
//! mozjs allows one JSEngine initialization per process, so running its glue
//! matrix in one libtest process produces unrelated AlreadyInitialized errors.
use serde_json::{json, Value};
use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn safe_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
fn git(repo: &Path, args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}
fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("lucinanet-engine-check: {e}");
            ExitCode::from(2)
        }
    }
}
fn run() -> Result<bool, Box<dyn std::error::Error>> {
    let mut repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut out = None;
    let mut timeout = Duration::from_secs(30);
    let mut filter = String::new();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--repo" => repo = PathBuf::from(value()?),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--timeout-secs" => timeout = Duration::from_secs(value()?.parse()?),
            "--filter" => filter = value()?,
            "--help" | "-h" => {
                println!("lucinanet-engine-check [--repo PATH] [--out PATH] [--timeout-secs 30] [--filter TEXT]");
                return Ok(true);
            }
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    if timeout.is_zero() {
        return Err("timeout must be positive".into());
    }
    let repo = repo.canonicalize()?;
    let out = out.unwrap_or_else(|| repo.join("target/lucinanet-engine-check"));
    fs::create_dir_all(&out)?;
    let out = out.canonicalize()?;
    println!("LucinaNet: building host API tests in {}", repo.display());
    let build = Command::new("cargo")
        .args([
            "test",
            "--locked",
            "--offline",
            "-p",
            "mozjs",
            "--tests",
            "--no-run",
            "--message-format=json",
            "-j",
            "2",
        ])
        .current_dir(&repo)
        .output()?;
    fs::write(out.join("build.jsonl"), &build.stdout)?;
    fs::write(out.join("build.log"), &build.stderr)?;
    if !build.status.success() {
        return Err(format!("build failed; see {}", out.join("build.log").display()).into());
    }
    let mut artifacts = Vec::new();
    for line in String::from_utf8_lossy(&build.stdout).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["reason"] == "compiler-artifact" && v["profile"]["test"] == true {
            if let (Some(exe), Some(name)) =
                (v["executable"].as_str(), v["target"]["name"].as_str())
            {
                artifacts.push((name.to_owned(), exe.to_owned()));
            }
        }
    }
    artifacts.sort();
    artifacts.dedup();
    let mut tests = Vec::new();
    let mut empty = Vec::new();
    for (target, executable) in artifacts {
        let listed = Command::new(&executable).arg("--list").output()?;
        if !listed.status.success() {
            return Err(format!("could not list {target}").into());
        }
        let names: Vec<_> = String::from_utf8_lossy(&listed.stdout)
            .lines()
            .filter_map(|s| s.strip_suffix(": test"))
            .map(str::to_owned)
            .collect();
        if names.is_empty() {
            empty.push(target.clone());
        }
        for name in names {
            if !format!("{target}::{name}").contains(&filter) {
                continue;
            }
            let log = out.join(format!("{}--{}.log", safe_name(&target), safe_name(&name)));
            let stdout = fs::File::create(&log)?;
            let stderr = stdout.try_clone()?;
            let start = Instant::now();
            let mut child = Command::new(&executable)
                .args(["--exact", &name, "--nocapture"])
                .current_dir(&repo)
                .env("RUST_BACKTRACE", "0")
                .stdin(Stdio::null())
                .stdout(stdout)
                .stderr(stderr)
                .spawn()?;
            let mut timed_out = false;
            let status = loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if start.elapsed() >= timeout {
                    timed_out = true;
                    match child.kill() {
                        Ok(()) => (),
                        Err(e) if e.kind() == io::ErrorKind::InvalidInput => (),
                        Err(e) => return Err(e.into()),
                    }
                    break child.wait()?;
                }
                thread::sleep(Duration::from_millis(20));
            };
            let text = fs::read_to_string(&log)?;
            // A successful exit must actually have executed exactly one test.
            let passed = !timed_out && status.success() && text.contains("1 passed; 0 failed");
            let reason = if timed_out {
                "test timed out".to_owned()
            } else {
                text.lines()
                    .find(|line| {
                        line.contains("is not implemented")
                            || line.contains("stack overflow")
                            || line.contains("panicked at")
                    })
                    .unwrap_or("")
                    .to_owned()
            };
            if !passed {
                println!("FAIL {target}::{name}: {reason} (see {})", log.display());
            }
            tests.push(json!({"target":target,"test":name,"status":if passed {"pass"} else if timed_out {"timeout"} else {"fail"},
                "exit_code":status.code(),"termination":status.to_string(),"elapsed_ms":start.elapsed().as_millis(),
                "reason":reason,"log":log.file_name().unwrap().to_string_lossy()}));
        }
    }
    if tests.is_empty() {
        return Err("no tests selected; refusing to report success".into());
    }
    let passed = tests.iter().filter(|t| t["status"] == "pass").count();
    let total = tests.len();
    let report = json!({"schema_version":1,"tool":"lucinanet-engine-check",
        "unix_seconds":SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        "repository":repo,"revision":git(&repo,&["rev-parse","HEAD"]),
        "dirty":!git(&repo,&["status","--porcelain"]).is_empty(),
        "filter":filter,"timeout_seconds":timeout.as_secs(),
        "passed":passed,"total":total,"empty_targets":empty,"tests":tests});
    fs::write(
        out.join("results.json"),
        serde_json::to_string_pretty(&report)? + "\n",
    )?;
    println!(
        "LucinaNet: {passed}/{total} host API tests passed; {}",
        out.join("results.json").display()
    );
    Ok(passed == total)
}
