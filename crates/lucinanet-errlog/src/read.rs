/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The reader: the sessions in the log, newest first, and what one session says per page host.
//!
//! Nothing here trusts the file. It is the part that meets a crash, a full disk and a hand with a
//! text editor, so a line that is not a line costs that line alone: it is skipped and counted, and
//! the count is shown on the page rather than hidden. A corrupt line has no session to belong to, so
//! it is charged to the session it sat among — the nearest thing to the truth a damaged file still
//! has, and a reason to show the number rather than explain it away.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::log::{Key, key_of, log_paths};
use crate::model::{Event, Kind};

/// How many of a host's details are brought to the top of a group. Past these the rest are still in
/// the counts, just not named.
pub const TOP_DETAILS: usize = 5;

/// One session's lines, oldest first, exactly as they were written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionErrors {
    pub session: u32,
    pub events: Vec<Event>,
    /// Lines that could not be read: not JSON, not an object, a kind this build does not know, or a
    /// line cut in half by a crash.
    pub corrupt: usize,
}

impl SessionErrors {
    /// When this session's first error is, or `None` if none of its lines could be read.
    pub fn first_t_ms(&self) -> Option<u64> {
        self.events.iter().map(|event| event.t_ms).min()
    }

    /// When its last error is, which is what puts the sessions in order.
    pub fn last_t_ms(&self) -> Option<u64> {
        self.events.iter().map(|event| event.t_ms).max()
    }

    /// Whether the log wrote this session its [`Kind::Truncated`] marker.
    pub fn truncated(&self) -> bool {
        self.events.iter().any(|event| event.kind == Kind::Truncated)
    }
}

/// The page hosts of one session, the one that broke most first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostGroup {
    /// Empty for the pages that have no host: `about:`, `data:`, a new tab.
    pub page_host: String,
    /// How many of each [`Kind`], in the order the kinds are declared, kinds that did not happen left
    /// out.
    pub kinds: Vec<(Kind, u32)>,
    /// The details that happened most, at most [`TOP_DETAILS`] of them.
    pub details: Vec<DetailCount>,
    /// How often anything on this host went wrong, counting the repeats each line stands for.
    pub total: u32,
    /// How many different errors that is.
    pub distinct: usize,
}

/// One message, API name or failure reason, how often it happened, and where it came from — because
/// "a `TypeError`" is not yet anything a person can act on, and `app.js:42` is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailCount {
    pub kind: Kind,
    pub detail: String,
    /// The script or resource host, empty when the error is about the page or about us.
    pub host: String,
    /// The file name, empty when there is none.
    pub file: String,
    pub line: u32,
    pub count: u32,
}

/// Read the sessions in `dir`, newest first, at most `last` of them.
///
/// The live file and every generation are read, oldest generation first, because one session's lines
/// can straddle a rollover. A missing file is not a failure: there are simply no sessions yet.
pub fn read_sessions(dir: &Path, last: usize) -> Vec<SessionErrors> {
    let mut buckets: BTreeMap<u32, SessionErrors> = BTreeMap::new();
    let mut unreadable = 0usize;
    let mut newest_read: Option<u32> = None;
    for path in log_paths(dir) {
        let Ok(file) = File::open(&path) else { continue };
        for line in BufReader::new(file).lines() {
            let Ok(text) = line else {
                unreadable += 1; // The file ended in a way that is not a line; what follows is gone.
                break;
            };
            if text.trim().is_empty() {
                continue;
            }
            match Event::from_line(&text) {
                Some(event) => {
                    let bucket = buckets
                        .entry(event.session)
                        .or_insert_with(|| SessionErrors { session: event.session, events: Vec::new(), corrupt: 0 });
                    bucket.corrupt += std::mem::take(&mut unreadable);
                    newest_read = Some(event.session);
                    bucket.events.push(event);
                }
                None => unreadable += 1,
            }
        }
    }
    if unreadable > 0 {
        match newest_read {
            Some(session) => {
                if let Some(bucket) = buckets.get_mut(&session) {
                    bucket.corrupt += unreadable;
                }
            }
            // Nothing readable anywhere, but something was there. Say so in a session of none rather
            // than hand back an empty list and let the page claim that nothing went wrong.
            None => {
                buckets.insert(0, SessionErrors { session: 0, events: Vec::new(), corrupt: unreadable });
            }
        }
    }

    let mut sessions: Vec<SessionErrors> = buckets.into_values().collect();
    // Newest session last in time first; a session with nothing readable in it goes to the bottom,
    // and equal times are broken by the session number rather than by the order the files came in.
    sessions.sort_by(|a, b| {
        b.last_t_ms().unwrap_or(0).cmp(&a.last_t_ms().unwrap_or(0)).then(b.session.cmp(&a.session)).then(b.events.len().cmp(&a.events.len()))
    });
    sessions.truncate(last);
    sessions
}

/// Fold one key's repeat lines back into one event.
///
/// The log wrote a key's line again at 1, 2, 4, 8, so those four lines are one error that happened
/// eight times, and adding the counts would report fifteen. The newest line carries the total; the
/// fields come from the first, which is the one that was written when the error was new. Folded
/// events come back in the order their keys first appeared, oldest first, which is what makes a
/// session read the way it happened.
fn collapse(events: &[Event]) -> Vec<Event> {
    let mut order: Vec<Key> = Vec::with_capacity(events.len());
    let mut by_key: BTreeMap<Key, Event> = BTreeMap::new();
    for event in events {
        let key = key_of(event);
        match by_key.get_mut(&key) {
            Some(kept) => kept.count = kept.count.max(event.count),
            None => {
                order.push(key.clone());
                by_key.insert(key, event.clone());
            }
        }
    }
    order.into_iter().filter_map(|key| by_key.remove(&key)).collect()
}

/// The page hosts of one session, most-failed first.
pub fn group_by_host(session: &SessionErrors) -> Vec<HostGroup> {
    #[derive(Default)]
    struct Acc {
        kinds: BTreeMap<Kind, u32>,
        details: BTreeMap<(Kind, String, String, String, u32), u32>,
        total: u32,
        distinct: usize,
    }

    let mut by_host: BTreeMap<String, Acc> = BTreeMap::new();
    for event in collapse(&session.events) {
        let acc = by_host.entry(event.page_host).or_default();
        acc.distinct += 1;
        *acc.kinds.entry(event.kind).or_insert(0) += event.count;
        *acc.details.entry((event.kind, event.detail, event.host, event.file, event.line)).or_insert(0) += event.count;
        acc.total += event.count;
    }

    let mut groups: Vec<HostGroup> = by_host
        .into_iter()
        .map(|(page_host, acc)| {
            let mut details: Vec<DetailCount> = acc
                .details
                .into_iter()
                .map(|((kind, detail, host, file, line), count)| DetailCount { kind, detail, host, file, line, count })
                .collect();
            details.sort_by(|a, b| {
                b.count.cmp(&a.count).then(a.detail.cmp(&b.detail)).then(a.file.cmp(&b.file)).then(a.line.cmp(&b.line))
            });
            details.truncate(TOP_DETAILS);
            HostGroup { page_host, kinds: acc.kinds.into_iter().collect(), details, total: acc.total, distinct: acc.distinct }
        })
        .collect();
    groups.sort_by(|a, b| b.total.cmp(&a.total).then(a.page_host.cmp(&b.page_host)));
    groups
}
