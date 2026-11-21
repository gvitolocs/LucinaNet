// Runs the tool over `tests/fixtures`, the way a caller runs it over the corpus, and
// asserts the JSONL it writes. The fixtures stand in for the parts of the real corpus
// that decide a runner's behaviour: a jsshell `.any.js` with `test()` and `promise_test()`
// results in both directions, a file whose test never settles, and a file that declares
// no jsshell global at all.
//
// This is an integration test on purpose: the unit tests in `src/meta.rs` and `src/exec.rs`
// cover one function each, and the thing worth pinning down is the contract - META header,
// `script=` resolution, the shell environment, the job and timer loop, the record shapes.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn tool() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lucinanet-wpt-jsshell"))
}

/// One corpus run over the fixtures: the records it wrote, and the run's summary.
struct Run {
    records: Vec<Value>,
    stdout: String,
    code: Option<i32>,
}

impl Run {
    fn file(&self, rel: &str) -> &Value {
        self.records
            .iter()
            .find(|line| line["kind"] == "file" && line["file"] == rel)
            .unwrap_or_else(|| panic!("no file record for {rel} in {:?}", self.records))
    }

    fn subtests(&self, rel: &str) -> Vec<(&str, &str, &str)> {
        self.records
            .iter()
            .filter(|line| line["kind"] == "subtest" && line["file"] == rel)
            .map(|line| {
                (
                    line["name"].as_str().unwrap_or_default(),
                    line["status"].as_str().unwrap_or_default(),
                    line["message"].as_str().unwrap_or_default(),
                )
            })
            .collect()
    }

    fn summary(&self) -> &Value {
        self.records
            .iter()
            .find(|line| line["kind"] == "summary")
            .expect("the run wrote no summary record")
    }
}

fn run(name: &str, args: &[&str]) -> Run {
    let out = std::env::temp_dir().join(format!(
        "lucinanet-wpt-jsshell-{}-{name}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&out);
    let mut command = Command::new(tool());
    command.arg("--wpt").arg(fixtures());
    for arg in args {
        command.arg(arg);
    }
    command.arg("--out").arg(&out);
    let output = command
        .output()
        .expect("the tool did not start");
    let text = std::fs::read_to_string(&out).unwrap_or_else(|error| {
        panic!(
            "no results in {}: {error}; stdout {} stderr {}",
            out.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let _ = std::fs::remove_file(&out);
    Run {
        records: text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("not a JSON line: {line}: {error}"))
            })
            .collect(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        code: output.status.code(),
    }
}

#[test]
fn a_jsshell_file_reports_each_of_its_results() {
    let run = run("basic", &["--filter", "basic.any.js", "--timeout-secs", "5"]);
    assert_eq!(run.code, Some(0), "stdout said {}", run.stdout);
    assert_eq!(
        run.subtests("basic.any.js"),
        vec![
            ("sync pass", "PASS", ""),
            (
                "sync fail",
                "FAIL",
                "assert_equals: doubling expected 5 but got 4"
            ),
            ("promise pass", "PASS", ""),
            ("promise fail", "FAIL", "the timer fired and reported"),
        ],
        "the four fixture results"
    );

    let file = run.file("basic.any.js");
    assert_eq!(file["result"], "ran");
    assert_eq!(file["harness_status"], "OK");
    assert_eq!(file["harness_message"], "");
    assert_eq!(file["reason"], "");
    assert_eq!(file["subtests"], 4);
    assert_eq!(file["pass"], 2);
    assert_eq!(file["fail"], 2);
    assert_eq!(file["timeout"], 0);
    assert_eq!(file["runner_errors"], serde_json::json!([]));

    let summary = run.summary();
    assert_eq!(summary["files"], 1);
    assert_eq!(summary["files_run"], 1);
    assert_eq!(summary["subtests"], 4);
    assert_eq!(summary["pass"], 2);
    assert_eq!(summary["fail"], 2);
    assert_eq!(summary["files_crashed"], 0);
}

#[test]
fn a_run_the_clock_ends_still_reports_its_results() {
    let run = run("hang", &["--filter", "hang.any.js", "--timeout-secs", "1"]);
    assert_eq!(run.code, Some(0), "stdout said {}", run.stdout);
    assert_eq!(
        run.subtests("hang.any.js"),
        vec![
            ("hang: before the hang", "PASS", ""),
            ("hang: never settles", "TIMEOUT", ""),
            ("hang: after the hang", "PASS", ""),
        ],
        "the snapshot taken when the clock ran out"
    );

    let file = run.file("hang.any.js");
    assert_eq!(file["result"], "timeout");
    assert_eq!(file["reason"], "no completion after 1 s");
    assert_eq!(file["harness_status"], "NONE", "no callback ever fired");
    assert_eq!(file["subtests"], 3);
    assert_eq!(file["pass"], 2);
    assert_eq!(file["timeout"], 1);

    let summary = run.summary();
    assert_eq!(summary["files_timeout"], 1);
    assert_eq!(summary["files_crashed"], 0);
}

#[test]
fn a_file_without_a_jsshell_global_is_skipped_not_run() {
    let run = run("corpus", &["--timeout-secs", "5"]);
    assert_eq!(run.code, Some(0), "stdout said {}", run.stdout);

    let file = run.file("window-only.any.js");
    assert_eq!(file["result"], "skipped");
    assert_eq!(file["reason"], "not-jsshell");
    assert_eq!(file["subtests"], 0);
    assert!(
        run.subtests("window-only.any.js").is_empty(),
        "a skipped file reports no subtests"
    );

    // A file the engine will not parse: reported, and not waited for. The timeout is 5 s,
    // so a runner that drove the loop for a completion that cannot arrive shows up here.
    let file = run.file("broken-parse.any.js");
    assert_eq!(file["result"], "error");
    assert_eq!(file["subtests"], 0);
    assert_eq!(file["harness_status"], "NONE");
    assert!(
        file["reason"].as_str().unwrap_or_default().contains("SyntaxError"),
        "the reason says what the engine refused: {}",
        file["reason"]
    );
    assert!(
        file["secs"].as_f64().unwrap_or(99.0) < 4.0,
        "an unparseable file must not wait for the clock, took {}",
        file["secs"]
    );

    // A `script=` file that throws is the file's problem, not the runner's, and the test
    // body after it is not evaluated. `wasm/jsapi/js-string/basic.any.js` in the real
    // corpus is this shape.
    let file = run.file("broken-script.any.js");
    assert_eq!(file["result"], "error");
    assert_eq!(file["subtests"], 0);
    assert!(
        file["secs"].as_f64().unwrap_or(99.0) < 4.0,
        "a file with no registered test must not wait for the clock, took {}",
        file["secs"]
    );
    assert_eq!(
        file["reason"],
        "script throwing.js threw throwing.js: ReferenceError: this helper needs a global the engine does not have"
    );
    assert!(run.subtests("broken-script.any.js").is_empty());

    // The whole fixture corpus, in path order, with nothing missing and nothing extra.
    let files: Vec<&str> = run
        .records
        .iter()
        .filter(|line| line["kind"] == "file")
        .map(|line| line["file"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        files,
        vec![
            "basic.any.js",
            "broken-parse.any.js",
            "broken-script.any.js",
            "hang.any.js",
            "window-only.any.js",
        ]
    );

    let summary = run.summary();
    assert_eq!(summary["files"], 5);
    assert_eq!(summary["files_run"], 1);
    assert_eq!(summary["files_timeout"], 1);
    assert_eq!(summary["files_error"], 2);
    assert_eq!(summary["files_skipped"], 1);
    assert_eq!(summary["files_harness_error"], 0);
    assert_eq!(summary["files_crashed"], 0);
    assert_eq!(summary["subtests"], 7);
    assert_eq!(summary["pass"], 4);
    assert_eq!(summary["fail"], 2);
    assert_eq!(summary["timeout"], 1);
}

#[test]
fn the_worker_refuses_a_file_it_was_not_asked_to_run() {
    // `--test` is the child half of the protocol, and the parent only reaches it with a
    // jsshell file. Called directly with the other kind, it must fail rather than invent
    // results.
    let output = Command::new(tool())
        .arg("--wpt")
        .arg(fixtures())
        .arg("--test")
        .arg(fixtures().join("window-only.any.js"))
        .output()
        .expect("the tool did not start");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stdout).is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no jsshell global"));
}
