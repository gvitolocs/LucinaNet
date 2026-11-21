//! The command line: what a run prints, what it exits with, and that stdin is the same as a file.

use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_media-trace-check");

fn run(args: &[&str], stdin: Option<&str>) -> (i32, String, String) {
    let mut child = Command::new(BIN)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary should start");
    if let Some(text) = stdin {
        child.stdin.take().expect("piped stdin").write_all(text.as_bytes()).expect("writing stdin");
    }
    let out = child.wait_with_output().expect("the binary should finish");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

#[test]
fn a_clean_log_exits_zero_and_says_so() {
    let (code, out, err) = run(&["tests/fixtures/healthy-all.log"], None);
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    assert!(out.starts_with("ok: no violation (17 lines, properties checked: element-contract-1"), "{out}");
}

#[test]
fn the_first_violation_is_named_and_the_run_exits_one() {
    let (code, out, err) = run(&["tests/fixtures/planted/clock-contract-stall.log"], None);
    assert_eq!((code, err.as_str()), (1, ""), "{out}");
    assert!(out.starts_with("clock-contract: violated at t+5.400, line 13:"), "{out}");
    assert!(out.contains("=> "), "{out}");
}

#[test]
fn stdin_reads_the_same_as_a_file() {
    let text = include_str!("fixtures/planted/MEDIA-CUR.log");
    let (code, out, _) = run(&["-"], Some(text));
    assert_eq!(code, 1);
    assert!(out.starts_with("MEDIA-CUR: violated at t+0.900, line 8:"), "{out}");
    assert_eq!(out, run(&["tests/fixtures/planted/MEDIA-CUR.log"], None).1, "the same bytes, the same report");
}

#[test]
fn windows_move_the_verdict_from_the_command_line() {
    let args = ["--clock-window", "5", "tests/fixtures/planted/clock-contract-stall.log"];
    assert_eq!(run(&args, None).0, 0, "5 s of standing still is what this run asked to allow");
    let bad = run(&["--clock-window", "not-a-number", "tests/fixtures/healthy-all.log"], None);
    assert_eq!(bad.0, 2, "a window that is not a number is the caller's mistake: {}", bad.2);
    assert!(bad.2.contains("--clock-window"), "{}", bad.2);
}

/// A trace too thin to judge anything is not a green light; `--strict` is how a caller says so.
#[test]
fn strict_refuses_a_trace_that_observed_nothing() {
    let lenient = run(&["tests/fixtures/prefix-play2-M-frozen.log"], None);
    assert_eq!(lenient.0, 0, "{}", lenient.1);
    assert!(lenient.1.contains("not observed: clock-contract"), "{}", lenient.1);
    let strict = run(&["--strict", "tests/fixtures/prefix-play2-M-frozen.log"], None);
    assert_eq!(strict.0, 1, "eight properties unjudged is a failure when asked strictly: {}", strict.1);
    let strict_healthy = run(&["--strict", "tests/fixtures/healthy-all.log"], None);
    assert_eq!(strict_healthy.0, 0, "{}", strict_healthy.1);
}

#[test]
fn a_missing_log_is_usage_not_a_panic() {
    let (code, out, err) = run(&["tests/fixtures/nope.log"], None);
    assert_eq!((code, out.as_str()), (2, ""), "{err}");
    assert!(err.contains("cannot read"), "{err}");
    assert!(err.contains("usage:"), "{err}");
}

#[test]
fn a_call_that_cannot_be_answered_is_a_usage_error() {
    for (args, want) in [
        (vec!["--clock-window"], "wants a value"),
        (vec!["tests/fixtures/healthy-all.log", "tests/fixtures/yt-f4-freeze.log"], "one log at a time"),
        (vec!["--verbose"], "unknown argument"),
    ] {
        let (code, out, err) = run(&args, None);
        assert_eq!((code, out.as_str()), (2, ""), "{args:?} → {err}");
        assert!(err.contains(want) && err.contains("usage:"), "{args:?} → {err}");
    }
}

/// A caller that pipes a log in without saying so means the same thing as `-`.
#[test]
fn no_argument_is_stdin() {
    let (code, out, _) = run(&[], Some(include_str!("fixtures/planted/MEDIA-STR-held.log")));
    assert_eq!(code, 1);
    assert!(out.starts_with("MEDIA-STR: violated at t+0.105, line 6:"), "{out}");
}
