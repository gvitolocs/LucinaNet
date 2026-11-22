/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The `LUCINANET_JS_MISSING_LOG` site log of the missing-feature inventory: the two line kinds, the
//! error cap, and that a log which cannot be opened changes nothing.
//!
//! Its own process, and one test in it, because the target is read once per process (the `OnceLock`
//! that makes the unset case free) and because `missing()` itself aborts: what is tested here is the
//! writer it calls, `record_missing`, plus `record_error` from the same module.

use mozjs_sys::engine::{record_error, record_missing};
use std::path::{Path, PathBuf};

fn logged(path: &Path) -> Vec<Vec<String>> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines().filter(|l| !l.is_empty()).map(|l| l.split('\t').map(String::from).collect()).collect()
}

/// One line of the log: `<unix_ms>\t<kind>\t<text>`, plus `\t<location>` when there is one.
fn check_row(row: &[String], kind: &str, text: &str, at: Option<&str>) {
    let ms: u128 = row[0].parse().expect("milliseconds since the epoch");
    assert!(ms > 1_700_000_000_000, "implausible clock: {ms}");
    assert_eq!(row[1], kind);
    assert_eq!(row[2], text);
    match at {
        Some(at) => assert_eq!(row[3], at),
        None => assert_eq!(row.len(), 3, "no location field: {row:?}"),
    }
}

#[test]
fn site_log_lines_and_the_error_cap_and_a_lost_file() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("missing-log-{}", std::process::id()));
    let file = dir.join("site.tsv");
    std::fs::create_dir_all(&dir).expect("log directory");
    std::env::set_var("LUCINANET_JS_MISSING_LOG", &file);

    record_missing("JS_TestProbe");
    let rows = logged(&file);
    assert_eq!(rows.len(), 1, "one line per call");
    check_row(&rows[0], "missing", "JS_TestProbe", None);

    record_error("TypeError: nope", "https://example.com/a.js", 7);
    let rows = logged(&file);
    assert_eq!(rows.len(), 2);
    check_row(&rows[1], "error", "TypeError: nope", Some("https://example.com/a.js:7"));

    // A thrown non-Error has no frame, so it keeps no location field either.
    record_error("uncaught exception: 42", "", 0);
    let rows = logged(&file);
    assert_eq!(rows.len(), 3);
    check_row(&rows[2], "error", "uncaught exception: 42", None);

    // The cap is per process and counts error lines only: 500 of them, whatever the page throws after.
    for _ in 0..(500 + 60) {
        record_error("Error: again", "https://example.com/a.js", 1);
    }
    let errors = logged(&file).into_iter().filter(|r| r[1] == "error").count();
    assert_eq!(errors, 500, "error lines stop at the cap");

    // Nothing the log cannot do is allowed to change the run: with the file gone the calls are silent.
    std::fs::remove_dir_all(&dir).expect("log directory removed");
    record_missing("JS_Gone");
    record_error("Error: gone", "https://example.com/a.js", 1);
    assert!(!file.exists(), "a failed open writes nothing and does not recreate the file");
}
