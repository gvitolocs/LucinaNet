/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Tests for the pure folded-stack part of the sampling profiler. Compiles standalone:
//! rustc --edition 2021 --test tests/profile_folded.rs

#[path = "../src/engine/profile_folded.rs"]
mod profile_folded;

use profile_folded::{Folded, Frame};

fn frame(name: Option<&str>, file: Option<&str>, line: Option<u32>, native: bool) -> Frame {
    Frame {
        name: name.map(|s| s.to_string()),
        file: file.map(|s| s.to_string()),
        line,
        native,
    }
}

#[test]
fn key_is_oldest_first_with_top_and_anonymous() {
    let mut f = Folded::new();
    // newest first: inner anonymous, then top-level (no name, oldest)
    f.add(
        &[
            frame(Some("inner"), Some("https://a.b/c/d.js?x=1"), Some(10), false),
            frame(None, Some("https://a.b/c/d.js?x=1"), Some(1), false),
        ],
        1000,
        false,
    );
    assert_eq!(
        f.folded_text(),
        "<top> (d.js:1);inner (d.js:10) 1000\n"
    );
}

#[test]
fn anonymous_non_oldest_frame_and_builtin_file() {
    let mut f = Folded::new();
    f.add(
        &[
            frame(None, None, None, false),
            frame(Some("main"), Some("app.js"), Some(3), false),
        ],
        500,
        false,
    );
    assert_eq!(
        f.folded_text(),
        "main (app.js:3);<anonymous> (<builtin>) 500\n"
    );
}

#[test]
fn native_frame_gets_native_marker() {
    let mut f = Folded::new();
    f.add(
        &[frame(Some("Array.map"), None, None, true)],
        250,
        false,
    );
    assert_eq!(f.folded_text(), "Array.map [native] (<builtin>) 250\n");
}

#[test]
fn query_stripped_and_last_segment_kept() {
    let mut f = Folded::new();
    f.add(
        &[frame(
            Some("f"),
            Some("https://cdn.example.com/assets/lib.min.js?v=9#frag"),
            Some(42),
            false,
        )],
        100,
        false,
    );
    assert_eq!(f.folded_text(), "f (lib.min.js:42) 100\n");
}

#[test]
fn identical_stacks_accumulate_weight() {
    let mut f = Folded::new();
    let stack = [frame(Some("g"), Some("a.js"), Some(1), false)];
    f.add(&stack, 1000, false);
    f.add(&stack, 1500, false);
    f.add(&stack, 500, false);
    assert_eq!(f.folded_text(), "g (a.js:1) 3000\n");
    assert_eq!(f.samples(), 3);
    assert_eq!(f.total_us(), 3000);
}

#[test]
fn delayed_sample_appends_delayed_leaf() {
    let mut f = Folded::new();
    f.add(
        &[frame(Some("h"), Some("b.js"), Some(7), false)],
        5000,
        true,
    );
    assert_eq!(f.folded_text(), "h (b.js:7);(delayed) 5000\n");
}

#[test]
fn self_vs_inclusive_file_with_recursion_counted_once() {
    let mut f = Folded::new();
    // r appears twice in one stack (recursion); inclusive counts a.js once per sample
    f.add(
        &[
            frame(Some("r"), Some("a.js"), Some(2), false),
            frame(Some("r"), Some("a.js"), Some(1), false),
            frame(None, Some("a.js"), Some(1), false),
        ],
        1000,
        false,
    );
    // Self weight is the newest frame only: one sample can never give more than its own weight.
    let self_files = f.self_by_file();
    assert_eq!(self_files, vec![("a.js".to_string(), 1000)]);
    let incl_files = f.inclusive_by_file();
    assert_eq!(incl_files, vec![("a.js".to_string(), 1000)]);
    // A library frame called from page code: self goes to the library, inclusive to both.
    f.add(
        &[frame(Some("lib"), Some("lib.js"), Some(9), false), frame(None, Some("page.js"), Some(1), false)],
        500,
        true,
    );
    // That sample was delayed: it stays out of the tables and only counts as delayed time.
    assert_eq!(f.self_by_file(), vec![("a.js".to_string(), 1000)]);
    assert_eq!(f.delayed_us(), 500);
    f.add(
        &[frame(Some("lib"), Some("lib.js"), Some(9), false), frame(None, Some("page.js"), Some(1), false)],
        700,
        false,
    );
    assert_eq!(f.self_by_file(), vec![("a.js".to_string(), 1000), ("lib.js".to_string(), 700)]);
    assert_eq!(f.self_by_function()[0], ("r".to_string(), 1000));
    let incl = f.inclusive_by_file();
    assert!(incl.contains(&("page.js".to_string(), 700)) && incl.contains(&("lib.js".to_string(), 700)));
}

#[test]
fn summary_top_n_ordering_and_percentages() {
    let mut f = Folded::new();
    f.add(&[frame(Some("hot"), Some("hot.js"), Some(1), false)], 7500, false);
    f.add(&[frame(Some("cold"), Some("cold.js"), Some(1), false)], 2500, false);
    let summary = f.summary(20);
    assert!(summary.contains("total sampled: 10 ms (2 samples)"));
    assert!(summary.contains("wall: 20 ms"));
    let hot_line = "  hot.js  7 ms  75.0%";
    let cold_line = "  cold.js  2 ms  25.0%";
    assert!(summary.contains(hot_line));
    assert!(summary.contains(cold_line));
    // hot.js sorts before cold.js in both file sections
    let self_pos = summary.find("top files by self weight:").unwrap();
    let incl_pos = summary.find("top files by inclusive weight:").unwrap();
    assert!(summary[self_pos..incl_pos].find("hot.js").unwrap() < summary[self_pos..incl_pos].find("cold.js").unwrap());
    assert!(summary.contains("  hot  7 ms  75.0%"));
}

#[test]
fn empty_profile() {
    let f = Folded::new();
    assert_eq!(f.folded_text(), "");
    let summary = f.summary(0);
    assert!(summary.contains("total sampled: 0 ms (0 samples)"));
    assert!(summary.contains("wall: 0 ms"));
    assert!(f.self_by_file().is_empty());
    assert!(f.inclusive_by_file().is_empty());
    assert!(f.self_by_function().is_empty());
}

#[test]
fn folded_text_is_sorted_by_key() {
    let mut f = Folded::new();
    f.add(&[frame(Some("z"), Some("z.js"), Some(1), false)], 100, false);
    f.add(&[frame(Some("a"), Some("a.js"), Some(1), false)], 200, false);
    f.add(&[frame(Some("m"), Some("m.js"), Some(1), false)], 300, false);
    let text = f.folded_text();
    let a = text.find("a (a.js:1)").unwrap();
    let m = text.find("m (m.js:1)").unwrap();
    let z = text.find("z (z.js:1)").unwrap();
    assert!(a < m && m < z);
}

#[test]
fn sixty_four_frame_stack() {
    let mut f = Folded::new();
    let frames: Vec<Frame> = (0..64)
        .map(|i| frame(Some(&format!("f{i}")), Some("deep.js"), Some(i as u32 + 1), false))
        .collect();
    f.add(&frames, 1000, false);
    let text = f.folded_text();
    // oldest first: f63 is the oldest frame
    assert!(text.starts_with("f63 (deep.js:64);"));
    assert!(text.ends_with("f0 (deep.js:1) 1000\n"));
    assert_eq!(text.matches(';').count(), 63);
}
