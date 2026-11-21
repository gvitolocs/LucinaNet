/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The permission store: stored answers with their expiry, the decision for a request, prompt answers, and the file format.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::embargo::Counters;
use crate::feature::{Feature, Setting};
use crate::origin::is_secure_origin;

/// How long a stored answer lasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Expiry {
    /// Until the browser quits (Firefox EXPIRE_SESSION).
    Session,
    /// Until all tabs of the origin are closed (Chromium "Allow this time" / one-time grant). Dropped by `origin_closed` and by `session_end`.
    OneTime,
    /// Until this time, microseconds since the epoch (Firefox EXPIRE_TIME, which counts milliseconds).
    Until(i64),
    /// Until the person changes it.
    Forever,
}

impl Expiry {
    fn expired(self, now_us: i64) -> bool {
        matches!(self, Expiry::Until(t) if now_us >= t)
    }

    fn persistent(self) -> bool {
        matches!(self, Expiry::Until(_) | Expiry::Forever)
    }
}

/// What the caller does with a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Block,
    Prompt,
}

/// What the person did with a prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Allowed(Expiry),
    Denied(Expiry),
    Dismissed,
    Ignored,
}

/// One stored answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub origin: String,
    pub feature: Feature,
    pub setting: Setting,
    pub expiry: Expiry,
}

/// Why a file could not be loaded.
#[derive(Debug, PartialEq, Eq)]
pub enum PermissionsError {
    /// Not JSON, or not the expected shape.
    Syntax(String),
    /// A `version` this build does not read.
    Version(u32),
}

impl fmt::Display for PermissionsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PermissionsError::Syntax(m) => write!(f, "permissions file: {m}"),
            PermissionsError::Version(v) => write!(f, "permissions file: unsupported version {v}"),
        }
    }
}

impl std::error::Error for PermissionsError {}

const FILE_VERSION: u32 = 1;

type Key = (String, Feature);

/// Stored answers and prompt history per (origin, feature).
#[derive(Clone, Debug, Default)]
pub struct Permissions {
    /// The setting is always `Allow` or `Block`: `Ask` is the absence of an entry.
    entries: BTreeMap<Key, (Setting, Expiry)>,
    counters: BTreeMap<Key, Counters>,
}

impl Permissions {
    pub fn new() -> Permissions {
        Permissions::default()
    }

    /// The decision for a request. Order: (1) a secure-context feature on an insecure origin is `Block`; (2) a non-expired
    /// stored entry wins (`Allow`/`Block`); (3) an embargo in force gives `Block`; (4) the feature's default (`Ask` -> `Prompt`).
    pub fn decide(&self, origin: &str, feature: Feature, now_us: i64) -> Decision {
        if feature.needs_secure_context() && !is_secure_origin(origin) {
            return Decision::Block;
        }
        match self.stored(origin, feature, now_us) {
            Some((Setting::Allow, _)) => return Decision::Allow,
            Some((Setting::Block, _)) => return Decision::Block,
            Some((Setting::Ask, _)) => return Decision::Prompt,
            None => {}
        }
        if self.embargoed_until(origin, feature, now_us).is_some() {
            return Decision::Block;
        }
        match feature.default_setting() {
            Setting::Allow => Decision::Allow,
            Setting::Block => Decision::Block,
            Setting::Ask => Decision::Prompt,
        }
    }

    /// The stored answer, if there is one that has not expired at `now_us` (whether or not `purge_expired` has run).
    pub fn stored(&self, origin: &str, feature: Feature, now_us: i64) -> Option<(Setting, Expiry)> {
        self.entries
            .get(&key(origin, feature))
            .copied()
            .filter(|&(_, expiry)| !expiry.expired(now_us))
    }

    /// Set directly (the site-info panel / settings page). `Setting::Ask` removes the entry.
    pub fn set(&mut self, origin: &str, feature: Feature, setting: Setting, expiry: Expiry) {
        match setting {
            Setting::Ask => {
                self.entries.remove(&key(origin, feature));
            }
            Setting::Allow | Setting::Block => {
                self.entries.insert(key(origin, feature), (setting, expiry));
            }
        }
    }

    /// Removes a stored answer; true when there was one. Prompt history is kept.
    pub fn remove(&mut self, origin: &str, feature: Feature) -> bool {
        self.entries.remove(&key(origin, feature)).is_some()
    }

    /// Forgets everything about one origin: its answers, dismissal counts and embargoes.
    pub fn reset_site(&mut self, origin: &str) {
        self.entries.retain(|(o, _), _| o != origin);
        self.counters.retain(|(o, _), _| o != origin);
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.counters.clear();
    }

    /// The person answered (or did not) a prompt. Allowed/Denied store an entry with its expiry and clear the
    /// dismissal and ignore counts and the embargo of that (origin, feature) (Chromium RemoveEmbargoAndResetCounts);
    /// Dismissed and Ignored count and may start an embargo.
    pub fn record_answer(&mut self, origin: &str, feature: Feature, answer: Answer, now_us: i64) {
        match answer {
            Answer::Allowed(expiry) => {
                self.set(origin, feature, Setting::Allow, expiry);
                self.counters.remove(&key(origin, feature));
            }
            Answer::Denied(expiry) => {
                self.set(origin, feature, Setting::Block, expiry);
                self.counters.remove(&key(origin, feature));
            }
            Answer::Dismissed => self.counters.entry(key(origin, feature)).or_default().dismissed(now_us),
            Answer::Ignored => self.counters.entry(key(origin, feature)).or_default().ignored(now_us),
        }
    }

    /// All tabs of the origin are closed: drop its `OneTime` entries.
    pub fn origin_closed(&mut self, origin: &str) {
        self.entries
            .retain(|(o, _), (_, expiry)| o != origin || *expiry != Expiry::OneTime);
    }

    /// The browser quits (or starts): drop `Session` and `OneTime` entries. Prompt counts stay: Chromium keeps them in
    /// the profile (see PORTING.md).
    pub fn session_end(&mut self) {
        self.entries.retain(|_, (_, expiry)| expiry.persistent());
    }

    /// Removes the entries that expired at `now_us` and returns how many; also forgets embargoes that ran out
    /// (their counts stay).
    pub fn purge_expired(&mut self, now_us: i64) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, (_, expiry)| !expiry.expired(now_us));
        for c in self.counters.values_mut() {
            if c.until.is_some_and(|t| now_us >= t) {
                c.until = None;
            }
        }
        self.counters.retain(|_, c| !c.is_empty());
        before - self.entries.len()
    }

    /// When the embargo of this (origin, feature) ends, if one is in force at `now_us`.
    pub fn embargoed_until(&self, origin: &str, feature: Feature, now_us: i64) -> Option<i64> {
        self.counters.get(&key(origin, feature)).and_then(|c| c.active_until(now_us))
    }

    /// Stored answers not expired at `now_us`, sorted by origin, then feature.
    pub fn entries(&self, now_us: i64) -> Vec<Entry> {
        self.entries
            .iter()
            .filter(|(_, (_, expiry))| !expiry.expired(now_us))
            .map(|((origin, feature), &(setting, expiry))| Entry { origin: origin.clone(), feature: *feature, setting, expiry })
            .collect()
    }

    /// The persistent state: Forever and Until entries, prompt counts and embargoes; never Session or OneTime.
    pub fn to_json(&self) -> String {
        let file = FileOut {
            version: FILE_VERSION,
            entries: self
                .entries
                .iter()
                .filter(|(_, (_, expiry))| expiry.persistent())
                .map(|((origin, feature), &(setting, expiry))| EntryOut {
                    origin,
                    feature: feature.name(),
                    setting: if setting == Setting::Allow { "allow" } else { "block" },
                    until: match expiry {
                        Expiry::Until(t) => Some(t),
                        _ => None,
                    },
                })
                .collect(),
            embargo: self
                .counters
                .iter()
                .filter(|(_, c)| !c.is_empty())
                .map(|((origin, feature), c)| EmbargoOut {
                    origin,
                    feature: feature.name(),
                    until: c.until,
                    dismissals: c.dismissals,
                    ignores: c.ignores,
                })
                .collect(),
        };
        // Plain structs of strings and numbers cannot fail to serialize; an empty file is the safe fallback.
        serde_json::to_string(&file).unwrap_or_else(|_| String::from("{\"version\":1,\"entries\":[],\"embargo\":[]}"))
    }

    /// Loads a file written by [`Permissions::to_json`]. Entries for unknown features (a newer version may have more)
    /// and entries that expired before `now_us` are dropped; bad syntax or a `version` other than 1 is an error.
    pub fn from_json(text: &str, now_us: i64) -> Result<Permissions, PermissionsError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| PermissionsError::Syntax(e.to_string()))?;
        let version = value
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| PermissionsError::Syntax("missing version".into()))?;
        if version != u64::from(FILE_VERSION) {
            return Err(PermissionsError::Version(u32::try_from(version).unwrap_or(u32::MAX)));
        }
        let file: FileIn = serde_json::from_value(value).map_err(|e| PermissionsError::Syntax(e.to_string()))?;
        let mut out = Permissions::new();
        for e in file.entries {
            let Some(feature) = Feature::from_name(&e.feature) else { continue };
            let setting = match e.setting.as_str() {
                "allow" => Setting::Allow,
                "block" => Setting::Block,
                other => return Err(PermissionsError::Syntax(format!("unknown setting {other:?}"))),
            };
            let expiry = e.until.map_or(Expiry::Forever, Expiry::Until);
            if expiry.expired(now_us) {
                continue;
            }
            out.entries.insert((e.origin, feature), (setting, expiry));
        }
        for e in file.embargo {
            let Some(feature) = Feature::from_name(&e.feature) else { continue };
            let c = Counters {
                dismissals: e.dismissals,
                ignores: e.ignores,
                until: e.until.filter(|&t| now_us < t),
            };
            if !c.is_empty() {
                out.counters.insert((e.origin, feature), c);
            }
        }
        Ok(out)
    }
}

fn key(origin: &str, feature: Feature) -> Key {
    (origin.to_owned(), feature)
}

#[derive(Serialize)]
struct FileOut<'a> {
    version: u32,
    entries: Vec<EntryOut<'a>>,
    embargo: Vec<EmbargoOut<'a>>,
}

#[derive(Serialize)]
struct EntryOut<'a> {
    origin: &'a str,
    feature: &'static str,
    setting: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    until: Option<i64>,
}

#[derive(Serialize)]
struct EmbargoOut<'a> {
    origin: &'a str,
    feature: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    until: Option<i64>,
    dismissals: u32,
    ignores: u32,
}

#[derive(Deserialize)]
struct FileIn {
    #[serde(default)]
    entries: Vec<EntryIn>,
    #[serde(default)]
    embargo: Vec<EmbargoIn>,
}

#[derive(Deserialize)]
struct EntryIn {
    origin: String,
    feature: String,
    setting: String,
    until: Option<i64>,
}

#[derive(Deserialize)]
struct EmbargoIn {
    origin: String,
    feature: String,
    until: Option<i64>,
    #[serde(default)]
    dismissals: u32,
    #[serde(default)]
    ignores: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use Feature::*;

    const A: &str = "https://example.org";
    const B: &str = "https://other.example";
    const DAY: i64 = 86_400_000_000;
    const NOW: i64 = 1_730_000_000_000_000;

    #[test]
    fn insecure_origin_blocks_secure_features() {
        let mut p = Permissions::new();
        let insecure = "http://example.org";
        for f in [Geolocation, Camera, Microphone, Notifications, Push, Midi, Bluetooth, ClipboardRead, DeviceInfo, ScreenWakeLock, PersistentStorage] {
            assert_eq!(p.decide(insecure, f, NOW), Decision::Block, "{f:?}");
        }
        // The block comes before a stored allow.
        p.set(insecure, Camera, Setting::Allow, Expiry::Forever);
        assert_eq!(p.decide(insecure, Camera, NOW), Decision::Block);
        // Features that do not need a secure context are not affected.
        assert_eq!(p.decide(insecure, Gamepad, NOW), Decision::Allow);
        assert_eq!(p.decide("file://", Camera, NOW), Decision::Block);
    }

    #[test]
    fn localhost_and_loopback_are_secure_for_decide() {
        let p = Permissions::new();
        for o in ["http://localhost:3000", "http://127.0.0.1", "http://[::1]:8080", "https://example.org"] {
            assert_eq!(p.decide(o, Camera, NOW), Decision::Prompt, "{o}");
        }
    }

    #[test]
    fn popups_default_blocks() {
        assert_eq!(Permissions::new().decide(A, Popups, NOW), Decision::Block);
        let mut p = Permissions::new();
        p.set(A, Popups, Setting::Allow, Expiry::Forever);
        assert_eq!(p.decide(A, Popups, NOW), Decision::Allow);
    }

    #[test]
    fn decide_default_ask_prompts() {
        let p = Permissions::new();
        assert_eq!(p.decide(A, Geolocation, NOW), Decision::Prompt);
        assert_eq!(p.decide(A, Notifications, NOW), Decision::Prompt);
        assert_eq!(p.decide(A, BackgroundSync, NOW), Decision::Allow);
    }

    #[test]
    fn decide_stored_allow_and_block_win_over_default() {
        let mut p = Permissions::new();
        p.set(A, Camera, Setting::Allow, Expiry::Forever);
        p.set(A, Popups, Setting::Allow, Expiry::Session);
        p.set(A, BackgroundSync, Setting::Block, Expiry::Forever);
        assert_eq!(p.decide(A, Camera, NOW), Decision::Allow);
        assert_eq!(p.decide(A, Popups, NOW), Decision::Allow);
        assert_eq!(p.decide(A, BackgroundSync, NOW), Decision::Block);
        assert_eq!(p.decide(B, Camera, NOW), Decision::Prompt);
    }

    #[test]
    fn decide_stored_beats_embargo() {
        let mut p = Permissions::new();
        for _ in 0..3 {
            p.record_answer(A, Camera, Answer::Dismissed, NOW);
        }
        assert_eq!(p.decide(A, Camera, NOW + 1), Decision::Block);
        p.set(A, Camera, Setting::Allow, Expiry::Forever);
        assert_eq!(p.decide(A, Camera, NOW + 1), Decision::Allow);
        // After an explicit allow removed the stored entry the embargo applies again.
        assert!(p.remove(A, Camera));
        assert_eq!(p.decide(A, Camera, NOW + 1), Decision::Block);
    }

    #[test]
    fn set_ask_removes_entry() {
        let mut p = Permissions::new();
        p.set(A, Camera, Setting::Allow, Expiry::Forever);
        p.set(A, Camera, Setting::Ask, Expiry::Forever);
        assert_eq!(p.stored(A, Camera, NOW), None);
        assert!(p.entries(NOW).is_empty());
        assert!(!p.remove(A, Camera));
    }

    #[test]
    fn expiry_until_is_exclusive_at_boundary() {
        let mut p = Permissions::new();
        p.set(A, Camera, Setting::Allow, Expiry::Until(1000));
        assert!(p.stored(A, Camera, 999).is_some());
        assert!(p.stored(A, Camera, 1000).is_none());
        assert_eq!(p.decide(A, Camera, 999), Decision::Allow);
        assert_eq!(p.decide(A, Camera, 1000), Decision::Prompt);
    }

    #[test]
    fn expired_entry_ignored_before_purge() {
        let mut p = Permissions::new();
        p.set(A, Camera, Setting::Block, Expiry::Until(5));
        assert_eq!(p.stored(A, Camera, 6), None);
        assert!(p.entries(6).is_empty());
        assert_eq!(p.decide(A, Camera, 6), Decision::Prompt);
    }

    #[test]
    fn purge_expired_counts_and_removes() {
        let mut p = Permissions::new();
        p.set(A, Camera, Setting::Allow, Expiry::Until(5));
        p.set(A, Microphone, Setting::Allow, Expiry::Until(50));
        p.set(A, Geolocation, Setting::Allow, Expiry::Forever);
        p.set(A, Popups, Setting::Allow, Expiry::Session);
        assert_eq!(p.purge_expired(10), 1);
        assert_eq!(p.entries(10).len(), 3);
        assert_eq!(p.purge_expired(10), 0);
        assert_eq!(p.purge_expired(100), 1);
        assert_eq!(p.entries(100).len(), 2);
    }

    #[test]
    fn session_entries_dropped_by_session_end() {
        let mut p = Permissions::new();
        p.set(A, Camera, Setting::Allow, Expiry::Session);
        p.set(A, Microphone, Setting::Allow, Expiry::OneTime);
        p.set(A, Geolocation, Setting::Block, Expiry::Forever);
        p.set(A, Push, Setting::Block, Expiry::Until(NOW + 1));
        p.session_end();
        let left: Vec<_> = p.entries(NOW).into_iter().map(|e| e.feature).collect();
        assert_eq!(left, vec![Geolocation, Push]);
    }

    #[test]
    fn onetime_entries_dropped_by_origin_closed() {
        let mut p = Permissions::new();
        p.set(A, Camera, Setting::Allow, Expiry::OneTime);
        p.origin_closed(A);
        assert_eq!(p.stored(A, Camera, NOW), None);
    }

    #[test]
    fn origin_closed_leaves_other_origins_and_forever_entries() {
        let mut p = Permissions::new();
        p.set(A, Camera, Setting::Allow, Expiry::OneTime);
        p.set(A, Microphone, Setting::Allow, Expiry::Forever);
        p.set(A, Geolocation, Setting::Allow, Expiry::Session);
        p.set(B, Camera, Setting::Allow, Expiry::OneTime);
        p.origin_closed(A);
        assert_eq!(p.stored(A, Camera, NOW), None);
        assert!(p.stored(A, Microphone, NOW).is_some());
        assert!(p.stored(A, Geolocation, NOW).is_some());
        assert!(p.stored(B, Camera, NOW).is_some());
    }

    #[test]
    fn record_allowed_forever_stores_allow() {
        let mut p = Permissions::new();
        p.record_answer(A, Camera, Answer::Allowed(Expiry::Forever), NOW);
        assert_eq!(p.stored(A, Camera, NOW), Some((Setting::Allow, Expiry::Forever)));
        p.record_answer(A, Microphone, Answer::Allowed(Expiry::OneTime), NOW);
        assert_eq!(p.stored(A, Microphone, NOW), Some((Setting::Allow, Expiry::OneTime)));
    }

    #[test]
    fn record_denied_session_stores_block() {
        let mut p = Permissions::new();
        p.record_answer(A, Geolocation, Answer::Denied(Expiry::Session), NOW);
        assert_eq!(p.stored(A, Geolocation, NOW), Some((Setting::Block, Expiry::Session)));
        assert_eq!(p.decide(A, Geolocation, NOW), Decision::Block);
    }

    #[test]
    fn allowed_clears_dismissal_counter() {
        let mut p = Permissions::new();
        p.record_answer(A, Camera, Answer::Dismissed, NOW);
        p.record_answer(A, Camera, Answer::Dismissed, NOW);
        p.record_answer(A, Camera, Answer::Allowed(Expiry::OneTime), NOW);
        p.remove(A, Camera);
        // Two dismissals before and one after would be three; the allow reset the count, so no embargo.
        p.record_answer(A, Camera, Answer::Dismissed, NOW);
        assert_eq!(p.embargoed_until(A, Camera, NOW), None);
        // A deny also clears the ignore count and a running embargo.
        for _ in 0..3 {
            p.record_answer(A, Camera, Answer::Dismissed, NOW);
        }
        assert!(p.embargoed_until(A, Camera, NOW + 1).is_some());
        p.record_answer(A, Camera, Answer::Denied(Expiry::Session), NOW);
        p.remove(A, Camera);
        assert_eq!(p.embargoed_until(A, Camera, NOW + 1), None);
    }

    #[test]
    fn three_dismissals_start_seven_day_embargo() {
        let mut p = Permissions::new();
        p.record_answer(A, Notifications, Answer::Dismissed, NOW);
        p.record_answer(A, Notifications, Answer::Dismissed, NOW);
        p.record_answer(A, Notifications, Answer::Dismissed, NOW + 5);
        assert_eq!(p.embargoed_until(A, Notifications, NOW + 6), Some(NOW + 5 + 7 * DAY));
        assert_eq!(p.decide(A, Notifications, NOW + 6), Decision::Block);
    }

    #[test]
    fn two_dismissals_do_not_embargo() {
        let mut p = Permissions::new();
        p.record_answer(A, Notifications, Answer::Dismissed, NOW);
        p.record_answer(A, Notifications, Answer::Dismissed, NOW);
        assert_eq!(p.embargoed_until(A, Notifications, NOW), None);
        assert_eq!(p.decide(A, Notifications, NOW), Decision::Prompt);
    }

    #[test]
    fn four_ignores_start_embargo() {
        let mut p = Permissions::new();
        for _ in 0..3 {
            p.record_answer(A, Geolocation, Answer::Ignored, NOW);
        }
        assert_eq!(p.embargoed_until(A, Geolocation, NOW), None);
        p.record_answer(A, Geolocation, Answer::Ignored, NOW);
        assert_eq!(p.embargoed_until(A, Geolocation, NOW), Some(NOW + 7 * DAY));
    }

    #[test]
    fn embargo_ends_after_seven_days() {
        let mut p = Permissions::new();
        for _ in 0..3 {
            p.record_answer(A, Camera, Answer::Dismissed, NOW);
        }
        let end = NOW + 7 * DAY;
        assert_eq!(p.decide(A, Camera, end - 1), Decision::Block);
        assert_eq!(p.decide(A, Camera, end), Decision::Prompt);
        // Chromium does not reset the count: the next dismissal after the embargo starts a new one at once.
        p.record_answer(A, Camera, Answer::Dismissed, end + 1);
        assert_eq!(p.embargoed_until(A, Camera, end + 2), Some(end + 1 + 7 * DAY));
    }

    #[test]
    fn embargo_is_per_feature_per_origin() {
        let mut p = Permissions::new();
        for _ in 0..3 {
            p.record_answer(A, Camera, Answer::Dismissed, NOW);
        }
        assert!(p.embargoed_until(A, Camera, NOW + 1).is_some());
        assert_eq!(p.embargoed_until(A, Microphone, NOW + 1), None);
        assert_eq!(p.embargoed_until(B, Camera, NOW + 1), None);
        assert_eq!(p.decide(B, Camera, NOW + 1), Decision::Prompt);
        // Dismissals and ignores of different features do not add up.
        p.record_answer(B, Camera, Answer::Dismissed, NOW);
        p.record_answer(B, Microphone, Answer::Dismissed, NOW);
        p.record_answer(B, Camera, Answer::Dismissed, NOW);
        assert_eq!(p.embargoed_until(B, Camera, NOW + 1), None);
    }

    #[test]
    fn reset_site_removes_all_features_of_origin() {
        let mut p = Permissions::new();
        p.set(A, Camera, Setting::Allow, Expiry::Forever);
        p.set(A, Microphone, Setting::Block, Expiry::Session);
        p.set(B, Camera, Setting::Allow, Expiry::Forever);
        for _ in 0..3 {
            p.record_answer(A, Geolocation, Answer::Dismissed, NOW);
        }
        p.reset_site(A);
        assert!(p.entries(NOW).iter().all(|e| e.origin == B));
        assert_eq!(p.entries(NOW).len(), 1);
        assert_eq!(p.embargoed_until(A, Geolocation, NOW + 1), None);
    }

    #[test]
    fn clear_removes_everything() {
        let mut p = Permissions::new();
        p.set(A, Camera, Setting::Allow, Expiry::Forever);
        for _ in 0..3 {
            p.record_answer(B, Camera, Answer::Dismissed, NOW);
        }
        p.clear();
        assert!(p.entries(NOW).is_empty());
        assert_eq!(p.embargoed_until(B, Camera, NOW + 1), None);
        assert_eq!(p.to_json(), r#"{"version":1,"entries":[],"embargo":[]}"#);
    }

    #[test]
    fn json_roundtrip_persistent_only() {
        let mut p = Permissions::new();
        p.set(A, Camera, Setting::Allow, Expiry::Forever);
        p.set(A, Microphone, Setting::Block, Expiry::Until(NOW + 10));
        p.set(A, Geolocation, Setting::Allow, Expiry::Session);
        p.set(A, Push, Setting::Allow, Expiry::OneTime);
        for _ in 0..3 {
            p.record_answer(B, Notifications, Answer::Dismissed, NOW);
        }
        p.record_answer(B, Camera, Answer::Ignored, NOW);
        let text = p.to_json();
        assert!(!text.contains("geolocation") && !text.contains("push"));
        let q = Permissions::from_json(&text, NOW).expect("loads");
        assert_eq!(
            q.entries(NOW),
            vec![
                Entry { origin: A.into(), feature: Camera, setting: Setting::Allow, expiry: Expiry::Forever },
                Entry { origin: A.into(), feature: Microphone, setting: Setting::Block, expiry: Expiry::Until(NOW + 10) },
            ]
        );
        assert_eq!(q.embargoed_until(B, Notifications, NOW + 1), Some(NOW + 7 * DAY));
        // The ignore count survived: three more make four.
        let mut q = q;
        for _ in 0..3 {
            q.record_answer(B, Camera, Answer::Ignored, NOW + 2);
        }
        assert!(q.embargoed_until(B, Camera, NOW + 3).is_some());
        let again = Permissions::from_json(&q.to_json(), NOW).expect("reloads");
        assert_eq!(again.to_json(), q.to_json());
    }

    #[test]
    fn json_deterministic() {
        let build = |order: &[(&str, Feature)]| {
            let mut p = Permissions::new();
            for &(o, f) in order {
                p.set(o, f, Setting::Allow, Expiry::Forever);
            }
            p.to_json()
        };
        let one = build(&[(A, Camera), (B, Microphone), (A, Geolocation)]);
        let two = build(&[(A, Geolocation), (B, Microphone), (A, Camera)]);
        assert_eq!(one, two);
        // Hand-written expectation: sorted by origin, then feature order (geolocation before camera).
        assert_eq!(
            one,
            concat!(
                r#"{"version":1,"entries":["#,
                r#"{"origin":"https://example.org","feature":"geolocation","setting":"allow"},"#,
                r#"{"origin":"https://example.org","feature":"camera","setting":"allow"},"#,
                r#"{"origin":"https://other.example","feature":"microphone","setting":"allow"}],"embargo":[]}"#
            )
        );
    }

    #[test]
    fn json_drops_expired_on_load() {
        let text = r#"{"version":1,"entries":[
            {"origin":"https://example.org","feature":"camera","setting":"allow","until":100},
            {"origin":"https://example.org","feature":"microphone","setting":"allow","until":101},
            {"origin":"https://example.org","feature":"geolocation","setting":"block"}],
            "embargo":[{"origin":"https://example.org","feature":"push","until":100,"dismissals":3}]}"#;
        let p = Permissions::from_json(text, 100).expect("loads");
        let features: Vec<_> = p.entries(100).into_iter().map(|e| e.feature).collect();
        assert_eq!(features, vec![Geolocation, Microphone]);
        // The ended embargo is gone; its count stays, so one more dismissal embargoes again.
        assert_eq!(p.embargoed_until(A, Push, 100), None);
        let mut p = p;
        p.record_answer(A, Push, Answer::Dismissed, 100);
        assert!(p.embargoed_until(A, Push, 101).is_some());
    }

    #[test]
    fn json_drops_unknown_feature() {
        let text = r#"{"version":1,"entries":[
            {"origin":"https://example.org","feature":"teleportation","setting":"allow"},
            {"origin":"https://example.org","feature":"camera","setting":"allow"}],
            "embargo":[{"origin":"https://example.org","feature":"teleportation","dismissals":9}]}"#;
        let p = Permissions::from_json(text, 0).expect("loads");
        assert_eq!(p.entries(0).len(), 1);
        assert_eq!(p.to_json().matches("teleportation").count(), 0);
    }

    #[test]
    fn json_rejects_bad_version_and_garbage() {
        assert_eq!(Permissions::from_json(r#"{"version":2,"entries":[]}"#, 0).err(), Some(PermissionsError::Version(2)));
        assert_eq!(Permissions::from_json(r#"{"version":0}"#, 0).err(), Some(PermissionsError::Version(0)));
        assert!(matches!(Permissions::from_json("not json", 0), Err(PermissionsError::Syntax(_))));
        assert!(matches!(Permissions::from_json("", 0), Err(PermissionsError::Syntax(_))));
        assert!(matches!(Permissions::from_json("[]", 0), Err(PermissionsError::Syntax(_))));
        assert!(matches!(Permissions::from_json(r#"{"entries":[]}"#, 0), Err(PermissionsError::Syntax(_))));
        assert!(matches!(Permissions::from_json(r#"{"version":1,"entries":[{"origin":1}]}"#, 0), Err(PermissionsError::Syntax(_))));
        assert!(matches!(
            Permissions::from_json(r#"{"version":1,"entries":[{"origin":"https://a.b","feature":"camera","setting":"ask"}]}"#, 0),
            Err(PermissionsError::Syntax(_))
        ));
        // A version too large for u32 is still a version error, not a panic.
        assert_eq!(Permissions::from_json(r#"{"version":99999999999}"#, 0).err(), Some(PermissionsError::Version(u32::MAX)));
    }

    #[test]
    fn entries_sorted_and_unexpired() {
        let mut p = Permissions::new();
        p.set(B, Camera, Setting::Allow, Expiry::Forever);
        p.set(A, Microphone, Setting::Allow, Expiry::Forever);
        p.set(A, Camera, Setting::Block, Expiry::Session);
        p.set(A, Push, Setting::Allow, Expiry::Until(10));
        let got: Vec<_> = p.entries(10).into_iter().map(|e| (e.origin, e.feature)).collect();
        assert_eq!(got, vec![(A.to_owned(), Camera), (A.to_owned(), Microphone), (B.to_owned(), Camera)]);
    }

    #[test]
    fn session_end_keeps_prompt_counts() {
        let mut p = Permissions::new();
        for _ in 0..2 {
            p.record_answer(A, Camera, Answer::Ignored, NOW);
        }
        p.session_end();
        p.record_answer(A, Camera, Answer::Ignored, NOW);
        p.record_answer(A, Camera, Answer::Ignored, NOW);
        assert!(p.embargoed_until(A, Camera, NOW + 1).is_some());
    }
}
