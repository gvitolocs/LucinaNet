/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Seed a profile directory with history and bookmarks, written by the two stores' own serializers, so what
//! lands on disk is byte-for-byte what the browser would have saved itself:
//!
//! ```text
//! cargo run -p lucinanet-historydb --example seed-profile --offline -- <profile-dir>
//! ```
//!
//! `tests/zero-network/run.sh` uses it to ask what a profile that already has a past does at a start nobody
//! typed anything into. Times are microseconds since the Unix epoch, as the stores take them, and the visits
//! are spread over the last week so the list reads like a profile that was really used.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use lucinanet_bookmarks::{Bookmarks, OTHER, TOOLBAR};
use lucinanet_historydb::{HistoryDb, NewVisit, Source, Transition};

const HOUR_US: i64 = 3_600_000_000;
const DAY_US: i64 = 24 * HOUR_US;

/// What the browser itself stores: `How::Typed { search: false }` and `{ true }` and `How::Link`, the three ways a
/// visit reaches these files.
const VISITS: &[(&str, &str, i64, Transition, Source)] = &[
    ("https://example.com/", "Example Domain", 6 * DAY_US, Transition::Typed, Source::Organic),
    ("https://example.org/", "Example Organization", 6 * DAY_US, Transition::Link, Source::Organic),
    ("https://example.com/docs", "Documentation — example.com", 5 * DAY_US, Transition::Link, Source::Organic),
    ("https://example.com/docs/install", "Install — example.com", 5 * DAY_US, Transition::Link, Source::Organic),
    ("https://example.net/", "Example Network", 3 * DAY_US, Transition::Typed, Source::Organic),
    ("https://example.org/news/", "News — Example Organization", 2 * DAY_US, Transition::Link, Source::Organic),
    ("https://example.com/search?q=favicon+cache", "favicon cache — Search", 2 * DAY_US, Transition::Typed, Source::Search),
    ("https://example.com/", "Example Domain", 6 * HOUR_US, Transition::Typed, Source::Organic),
    ("https://example.net/", "Example Network", 3 * HOUR_US, Transition::Link, Source::Organic),
];

fn now_us() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as i64).unwrap_or(0)
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(dir) = args.next().filter(|d| !d.is_empty()) else {
        eprintln!("usage: seed-profile <profile-dir>");
        std::process::exit(2);
    };
    if args.next().is_some() {
        eprintln!("usage: seed-profile <profile-dir>");
        std::process::exit(2);
    }
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    let t0 = now_us();

    let mut db = HistoryDb::new();
    let mut recorded = 0;
    for (url, title, ago, transition, source) in VISITS {
        // `interesting: false` is what `Visits::completed` stores; the transitions and sources are the ones
        // `stored_as` gives for a typed address, a typed search and a link.
        if db
            .record_visit(NewVisit {
                url,
                title: Some(title),
                time_us: t0 - ago,
                transition: *transition,
                source: *source,
                interesting: false,
                from: None,
            })
            .is_some()
        {
            recorded += 1;
        }
    }
    let history = db.to_json();
    // Read it back with the same loader the browser uses, so what is claimed below is what is on disk.
    let back = HistoryDb::from_json(&history).unwrap_or_else(|e| panic!("the history written here does not read back: {e:?}"));
    let pages = back.len();
    write(&dir.join("history.json"), &history);

    let mut bm = Bookmarks::new();
    let example = bm.add_bookmark(TOOLBAR, None, "Example", "https://example.com/", t0 - 6 * DAY_US).unwrap();
    bm.set_tags(example, &["docs"], t0 - 6 * DAY_US).unwrap();
    bm.set_keyword(example, Some("ex"), t0 - 6 * DAY_US).unwrap();
    let reading = bm.add_folder(TOOLBAR, None, "Reading", t0 - 5 * DAY_US).unwrap();
    bm.add_bookmark(reading, None, "Example Docs", "https://example.com/docs", t0 - 5 * DAY_US).unwrap();
    bm.add_bookmark(reading, None, "Example News", "https://example.org/news/", t0 - 2 * DAY_US).unwrap();
    bm.add_bookmark(OTHER, None, "Example Organization", "https://example.org/", t0 - 6 * DAY_US).unwrap();
    bm.add_bookmark(OTHER, None, "Example Network", "https://example.net/", t0 - 3 * DAY_US).unwrap();
    let bookmarks = bm.to_json();
    let back = Bookmarks::from_json(&bookmarks).unwrap_or_else(|e| panic!("the bookmarks written here do not read back: {e:?}"));
    let marked = back.len_bookmarks();
    write(&dir.join("bookmarks.json"), &bookmarks);

    println!("seeded {} history pages / {} visits / {} bookmarks into {}", pages, recorded, marked, dir.display());
    println!("  history.json {} bytes, bookmarks.json {} bytes", history.len(), bookmarks.len());
}
