//! The baseline gate end to end, over the runner's own synthetic fixtures (`fixtures/test/`) as a tiny checkout:
//! a new non-pass is confirmed by a rerun alone whose result is read back, and a shrink keeps the header.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A throwaway checkout under the temp dir with the given fixture tests copied into `test/`, removed with it.
struct Checkout {
    root: PathBuf,
}

impl Checkout {
    fn new(name: &str, tests: &[&str]) -> Checkout {
        let root =
            std::env::temp_dir().join(format!("lucinanet-test262-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let test_dir = root.join("test");
        std::fs::create_dir_all(&test_dir).expect("create the throwaway checkout");
        for t in tests {
            let from = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join("test")
                .join(t);
            std::fs::copy(&from, test_dir.join(t))
                .unwrap_or_else(|e| panic!("copy {}: {e}", from.display()));
        }
        Checkout { root }
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::write(&path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        path
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// One gated run: its exit code and what it printed.
fn gated(root: &Path, baseline: &Path, out: &Path, shrink: bool) -> (i32, String) {
    let mut run = Command::new(env!("CARGO_BIN_EXE_lucinanet-test262"));
    run.arg("--test262")
        .arg(root)
        .arg("--jobs")
        .arg("1")
        .arg("--baseline")
        .arg(baseline)
        .arg("--out")
        .arg(out);
    if shrink {
        run.arg("--shrink-baseline");
    }
    let done = run.output().expect("the runner runs");
    (
        done.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&done.stdout).into_owned(),
    )
}

#[test]
fn a_new_non_pass_is_rerun_alone_and_its_result_read_back() {
    // The rerun selects the test by a filter of the same shape the discovery matches, so its own message comes
    // back; a filter of the checkout-relative key selects nothing and the gate would only say "no result".
    let dir = Checkout::new("rerun", &["async-missing-negative.js"]);
    let baseline = dir.write("baseline.txt", "# throwaway baseline, no entries\n");
    let out = dir.root.join("results.jsonl");
    let (code, stdout) = gated(&dir.root, &baseline, &out, false);
    assert_eq!(code, 1, "a new non-pass fails the gate\n{stdout}");
    assert!(
        stdout.contains("NEW  test/async-missing-negative.js [raw] fail: expected runtime error (TypeError), test passed"),
        "the rerun alone must report what the test did:\n{stdout}"
    );
    assert!(
        !stdout.contains("rerun produced no result"),
        "the rerun selected no variant:\n{stdout}"
    );
}

#[test]
fn a_shrink_keeps_the_header_lines_and_retakes_the_entry_count() {
    let dir = Checkout::new(
        "shrink",
        &["runtime-negative.js", "async-missing-negative.js"],
    );
    let baseline = dir.write(
        "baseline.txt",
        concat!(
            "# Known test262 non-passes of the Rust JavaScript engine: path (relative to the checkout) and variant.\n",
            "# This file only shrinks, by deliberate commits.\n",
            "# Engine: Brimstone fork engine-annexb 189b2fe2, test262 227d905513, 2 entries.\n",
            "test/async-missing-negative.js\traw\n",
            "test/runtime-negative.js\traw\n",
        ),
    );
    let out = dir.root.join("results.jsonl");
    let (code, stdout) = gated(&dir.root, &baseline, &out, true);
    assert_eq!(code, 0, "every non-pass was known\n{stdout}");
    let shrunk = std::fs::read_to_string(&baseline).expect("the shrunk baseline reads back");
    assert_eq!(
        shrunk,
        concat!(
            "# Known test262 non-passes of the Rust JavaScript engine: path (relative to the checkout) and variant.\n",
            "# This file only shrinks, by deliberate commits.\n",
            "# Engine: Brimstone fork engine-annexb 189b2fe2, test262 227d905513, 1 entries.\n",
            "test/async-missing-negative.js\traw\n",
        ),
        "a shrink drops what passes now and keeps the provenance header\n{stdout}"
    );
}
