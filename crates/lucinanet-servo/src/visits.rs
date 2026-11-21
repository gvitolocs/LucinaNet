//! The visited-page history the engine keeps: `lucinanet-historydb`'s store over the profile's `history.json`
//! (docs/specs/wiring-omnibox-history.md). Nothing here touches Servo, so the glue is tested on its own; the engine
//! tells [`Visits`] what the tabs do (an address committed, a title arrived, a load completed) and asks it for the
//! pages the address bar may suggest.

use std::collections::HashMap;
use std::hash::Hash;
use std::path::Path;

use lucinanet_historydb::{HistoryDb, MatchBehavior, NewVisit, Query, Source, Transition};
use lucinanet_omnibox::HistoryEntry;
use url::Url;

use lucinanet_ui::title::{MAX_TITLE, title_for};

use crate::persist::{SAVE_EVERY_US, Saver, Writer};
use crate::zoom::write_atomic;

/// The file's name in the profile directory.
const FILE: &str = "history.json";

/// The most pages kept. Firefox sizes the limit from a 75 MiB database budget, falling back to 100000 pages
/// (`PlacesExpiration.sys.mjs` `getPagesLimit` @33abb90); a file of 20000 pages is a few MB and is rewritten whole.
pub const MAX_PAGES: usize = 20_000;

/// Where an over-full history is cut back to, so the next few hundred visits do not each trigger another cut.
pub const TRIM_TO: usize = 18_000;

/// The largest `history.json` read. A full history is a few MB; a bigger file is not ours.
pub const MAX_FILE: u64 = 32 << 20;

/// How many pages one address-bar query hands to the ranker.
pub const CANDIDATES: usize = 200;

/// The titles of the pages Servo makes for a failed load (`resources/neterror.html`, `badcert.html` in the pinned
/// fork). The error page keeps the address that failed, so the title is the only mark the embedder sees.
const ERROR_TITLES: [&str; 2] = ["Error loading page", "Certificate error"];

/// How the navigation that is under way began.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// From the address bar; `search` when the text became a search.
    Typed { search: bool },
    /// Anything else: a link, a script, a pop-up, the first page.
    Link,
    /// Back or Forward: Firefox does not add a visit for these (`ShouldUpdateGlobalHistory`).
    Traversal,
}

struct Pending {
    url: Url,
    how: How,
    at_us: i64,
}

#[derive(Default)]
struct Tab {
    /// Set by a command, taken by the next committed address.
    intent: Option<How>,
    /// Committed, not yet complete.
    pending: Option<Pending>,
    /// The title the tab last reported.
    title: String,
    /// The address recorded last, for a title that arrives after the load completed.
    recorded: Option<String>,
    /// The address committed last.
    last: Option<String>,
}

/// The history and what the tabs are doing to it.
pub struct Visits<K> {
    db: HistoryDb,
    tabs: HashMap<K, Tab>,
    /// `<profile>/history.json`; no file with `LUCINANET_PROFILE_DIR=none`, and then nothing is ever written.
    saver: Saver<HistoryDb>,
}

fn write_file(path: &Path, db: &HistoryDb) -> std::io::Result<()> {
    write_atomic(path, db.to_json().as_bytes())
}

/// Whether a committed page is a visit. Firefox keeps out what `BaseHistory::CanStore` refuses (the store does that:
/// `about:`, `data:`, `blob:`, long and user-info URLs) and error pages (`ShouldUpdateGlobalHistory`); Chromium keeps
/// out error navigations too (`ShouldUpdateHistory`). Only web and file pages are visits here.
pub fn worth_recording(url: &Url, title: &str) -> bool {
    matches!(url.scheme(), "http" | "https" | "ftp" | "file") && !is_error_title(title)
}

/// The title of an error page Servo made.
pub fn is_error_title(title: &str) -> bool {
    ERROR_TITLES.contains(&title.trim())
}

/// The visit type and source a navigation is stored as. A search typed in the bar is a typed visit from the search
/// source, which keeps result pages out of the typed boost of the frecency (`SQLFunctions.cpp`, historydb PORTING.md).
pub fn stored_as(how: How) -> (Transition, Source) {
    match how {
        How::Typed { search: false } => (Transition::Typed, Source::Organic),
        How::Typed { search: true } => (Transition::Typed, Source::Search),
        How::Link | How::Traversal => (Transition::Link, Source::Organic),
    }
}

/// Cut `db` back to `keep` pages when it holds more than `max`: the pages visited longest ago go (Firefox's expiration
/// takes the oldest visits first, `QUERY_FIND_VISITS_TO_EXPIRE`). Firefox spares the last 7 days to protect bookmarks;
/// there are none here yet and the bound matters more on a small device, so nothing is spared. Returns the pages removed.
pub fn trim(db: &mut HistoryDb, max: usize, keep: usize) -> usize {
    if db.len() <= max {
        return 0;
    }
    let doomed: Vec<String> = db.recent(usize::MAX, true).iter().skip(keep).map(|p| p.url.clone()).collect();
    for url in &doomed {
        db.remove_url(url);
    }
    doomed.len()
}

impl<K: Hash + Eq + Copy> Visits<K> {
    /// The history of the profile in `dir`, or an empty one if there is no profile or no usable file. A file that cannot
    /// be read is left where it is and replaced by the next write; a file with more than [`MAX_PAGES`] pages is cut back.
    pub fn load(dir: Option<&Path>) -> Visits<K> {
        let file = dir.map(|dir| dir.join(FILE));
        let mut db = file.as_deref().and_then(read).unwrap_or_default();
        trim(&mut db, MAX_PAGES, TRIM_TO);
        Visits { db, tabs: HashMap::new(), saver: Saver::new(file, "history", SAVE_EVERY_US, std::sync::Arc::new(write_file)) }
    }

    /// A history that keeps nothing on disk, for tests.
    #[cfg(test)]
    pub fn in_memory() -> Visits<K> {
        Visits::load(None)
    }

    /// The tab's next committed address began like this (a command: the address bar, Back, Forward).
    pub fn intend(&mut self, tab: K, how: How) {
        self.tabs.entry(tab).or_default().intent = Some(how);
    }

    /// An address committed in `tab`. It becomes a visit when the load completes, unless it was a Back or Forward or
    /// the same address again (a reload, `pushState`).
    pub fn committed(&mut self, tab: K, url: &Url, now_us: i64) {
        let t = self.tabs.entry(tab).or_default();
        let how = t.intent.take().unwrap_or(How::Link);
        let same = t.last.as_deref() == Some(url.as_str());
        t.last = Some(url.to_string());
        t.title.clear();
        t.pending = (how != How::Traversal && !same).then(|| Pending { url: url.clone(), how, at_us: now_us });
    }

    /// The tab's title changed. A page already recorded takes it; an error page's title never does.
    pub fn titled(&mut self, tab: K, title: &str, now_us: i64) {
        let title: String = title.chars().take(MAX_TITLE).collect();
        let t = self.tabs.entry(tab).or_default();
        t.title = title.clone();
        if is_error_title(&title) {
            return;
        }
        if t.pending.is_none()
            && let Some(url) = t.recorded.as_deref().filter(|url| t.last.as_deref() == Some(*url))
            && let shown = title_for(Some(&title), url)
            && self.db.page(url).is_some_and(|p| p.title.as_deref() != Some(shown.as_str()))
        {
            self.db.set_title(url, &shown);
            self.saver.changed(now_us);
        }
    }

    /// The tab's load completed, with the title the engine reports for the document that finished loading. Returns whether
    /// a visit was recorded.
    pub fn completed(&mut self, tab: K, title_now: Option<&str>, now_us: i64) -> bool {
        let Some(t) = self.tabs.get_mut(&tab) else { return false };
        let Some(pending) = t.pending.take() else { return false };
        // The current document's title, else its host: never the previous page's (`t.title` is cleared at the commit).
        let title = title_for(Some(title_now.filter(|s| !s.trim().is_empty()).unwrap_or(&t.title)), pending.url.as_str());
        if !worth_recording(&pending.url, &title) {
            return false;
        }
        let (transition, source) = stored_as(pending.how);
        let recorded = self.db.record_visit(NewVisit {
            url: pending.url.as_str(),
            title: Some(&title),
            time_us: pending.at_us,
            transition,
            source,
            interesting: false,
            from: None,
        });
        if recorded.is_none() {
            return false;
        }
        t.recorded = Some(pending.url.to_string());
        trim(&mut self.db, MAX_PAGES, TRIM_TO);
        self.saver.changed(now_us);
        true
    }

    /// The tab is gone.
    pub fn forget(&mut self, tab: K) {
        self.tabs.remove(&tab);
    }

    /// Pages the address bar may suggest for `text`, best frecency first: every typed word is somewhere in the address or
    /// the title (`MatchBehavior::Anywhere`, the widest the ranker narrows from).
    pub fn candidates(&self, text: &str) -> Vec<HistoryEntry> {
        let query = Query { text, behavior: MatchBehavior::Anywhere, limit: CANDIDATES, include_hidden: false, since_us: None };
        self.db
            .search(&query)
            .into_iter()
            .map(|p| HistoryEntry {
                url: p.url.clone(),
                title: p.title.clone().unwrap_or_default(),
                visit_count: p.visit_count,
                typed_count: p.typed_count,
                last_visit_us: p.last_visit_us,
            })
            .collect()
    }

    /// The loop's clock; see [`Saver::tick`]: a snapshot of the history goes to a short-lived worker thread when the file is due.
    pub fn tick(&mut self, now_us: i64) {
        self.saver.tick(now_us, &self.db);
    }

    /// Write what is left and wait for it (exit, and the command channel closing); see [`Saver::flush`].
    pub fn flush(&mut self, now_us: i64) {
        self.saver.flush(now_us, &self.db);
    }

    /// A history with a fake writer, for tests.
    #[cfg(test)]
    fn with_writer(file: std::path::PathBuf, writer: Writer<HistoryDb>) -> Visits<K> {
        let mut v = Visits::load(None);
        v.saver = Saver::new(None, "history", SAVE_EVERY_US, writer).with_file(file);
        v
    }

    /// Wait for the write in flight, for tests.
    #[cfg(test)]
    fn settle(&mut self) {
        self.saver.settle();
    }

    /// The store, for tests.
    #[cfg(test)]
    fn db(&self) -> &HistoryDb {
        &self.db
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.db.len()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.db.is_empty()
    }
}

/// The store from `path`, or None when there is no file, it is too big, or it is not a history file.
fn read(path: &Path) -> Option<HistoryDb> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        eprintln!("lucinanet: {} ignored: not a file of at most {MAX_FILE} bytes", path.display());
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    match HistoryDb::from_json(&text) {
        Ok(db) => Some(db),
        Err(error) => {
            eprintln!("lucinanet: {} ignored: {error:?}", path.display());
            None
        },
    }
}

/// Microseconds since the Unix epoch.
pub fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, mpsc};

    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    /// A page committed and completed in tab 1.
    fn visit(v: &mut Visits<u32>, s: &str, title: &str, at: i64) -> bool {
        v.committed(1, &url(s), at);
        v.completed(1, Some(title), at + 1)
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lucinanet-visits-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_completed_page_is_a_visit_with_its_title() {
        let mut v = Visits::<u32>::in_memory();
        assert!(visit(&mut v, "https://example.com/a", "A page", 1_000));
        let p = v.db().page("https://example.com/a").unwrap();
        assert_eq!((p.title.as_deref(), p.visit_count, p.typed_count, p.last_visit_us), (Some("A page"), 1, 0, 1_000));
    }

    #[test]
    fn a_typed_address_is_typed_and_a_search_comes_from_the_search_source() {
        assert_eq!(stored_as(How::Typed { search: false }), (Transition::Typed, Source::Organic));
        assert_eq!(stored_as(How::Typed { search: true }), (Transition::Typed, Source::Search));
        assert_eq!(stored_as(How::Link), (Transition::Link, Source::Organic));
        let mut v = Visits::<u32>::in_memory();
        v.intend(1, How::Typed { search: false });
        assert!(visit(&mut v, "https://example.com/", "E", 5));
        // The intent was used up: the next commit is a link.
        assert!(visit(&mut v, "https://example.com/next", "N", 10));
        assert_eq!(v.db().page("https://example.com/").unwrap().typed_count, 1);
        assert_eq!(v.db().page("https://example.com/next").unwrap().typed_count, 0);
    }

    #[test]
    fn back_forward_the_same_address_and_unfinished_loads_are_no_visit() {
        let mut v = Visits::<u32>::in_memory();
        assert!(visit(&mut v, "https://example.com/a", "A", 1));
        v.intend(1, How::Traversal);
        v.committed(1, &url("https://example.com/b"), 5);
        assert!(!v.completed(1, Some("B"), 6));
        assert!(v.db().page("https://example.com/b").is_none());
        // The same address again (pushState, reload): not a second visit.
        v.committed(1, &url("https://example.com/b"), 7);
        assert!(!v.completed(1, Some("B"), 8));
        // Committed and never completed (navigated away): nothing.
        v.committed(1, &url("https://example.com/c"), 9);
        v.committed(1, &url("https://example.com/d"), 10);
        assert!(v.completed(1, Some("D"), 11));
        assert!(v.db().page("https://example.com/c").is_none());
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn error_pages_and_pages_the_store_refuses_are_no_visit() {
        let mut v = Visits::<u32>::in_memory();
        assert!(!visit(&mut v, "https://down.example/", "Error loading page", 1));
        assert!(!visit(&mut v, "https://bad.example/", "Certificate error", 2));
        assert!(!visit(&mut v, "about:blank", "", 3));
        assert!(!visit(&mut v, "data:text/html,hi", "hi", 4));
        assert!(!visit(&mut v, "https://user:pw@example.com/", "x", 5));
        assert!(!visit(&mut v, &format!("https://example.com/{}", "a".repeat(2100)), "long", 6));
        assert!(v.is_empty());
        assert!(worth_recording(&url("file:///tmp/a.html"), "t"));
        assert!(!worth_recording(&url("blob:https://example.com/1"), "t"));
    }

    #[test]
    fn a_title_that_arrives_after_the_load_is_kept_but_an_error_title_is_not() {
        let mut v = Visits::<u32>::in_memory();
        assert!(visit(&mut v, "https://example.com/", "", 1));
        // No title: the host stands in for it (never nothing, never another page's).
        assert_eq!(v.db().page("https://example.com/").unwrap().title.as_deref(), Some("example.com"));
        v.titled(1, "Late title", 3);
        assert_eq!(v.db().page("https://example.com/").unwrap().title.as_deref(), Some("Late title"));
        v.titled(1, "Error loading page", 4);
        assert_eq!(v.db().page("https://example.com/").unwrap().title.as_deref(), Some("Late title"));
        // The title before completion is used when the web view gives none at completion.
        v.committed(1, &url("https://example.com/2"), 5);
        v.titled(1, "Second", 5);
        assert!(v.completed(1, None, 6));
        assert_eq!(v.db().page("https://example.com/2").unwrap().title.as_deref(), Some("Second"));
    }

    #[test]
    fn a_visit_takes_the_current_documents_title_never_the_previous_pages() {
        let mut v = Visits::<u32>::in_memory();
        assert!(visit(&mut v, "https://www.youtube.com/", "YouTube", 1));
        // The next document (a sign-in redirect) reports no title, and the web view still holds the old one.
        v.committed(1, &url("https://accounts.google.com/signin"), 10);
        v.titled(1, "", 10);
        assert!(v.completed(1, None, 11));
        assert_eq!(v.db().page("https://accounts.google.com/signin").unwrap().title.as_deref(), Some("accounts.google.com"));
        // A document with no title event at all, and one whose title is only spaces.
        v.committed(1, &url("https://www.example.org/c"), 20);
        assert!(v.completed(1, None, 21));
        assert_eq!(v.db().page("https://www.example.org/c").unwrap().title.as_deref(), Some("example.org"));
        v.committed(1, &url("https://www.example.org/d"), 30);
        assert!(v.completed(1, Some("  \n "), 31));
        assert_eq!(v.db().page("https://www.example.org/d").unwrap().title.as_deref(), Some("example.org"));
        // Spaces inside a title are collapsed as in the tab strip.
        assert!(visit(&mut v, "https://example.net/e", "  Spaced \n out ", 40));
        assert_eq!(v.db().page("https://example.net/e").unwrap().title.as_deref(), Some("Spaced out"));
    }

    #[test]
    fn the_history_entry_reads_as_the_tab_does() {
        // One rule for both: what `title_for` gives the strip is what the entry keeps.
        let untitled = "http://127.0.0.1:18777/b.html";
        let mut v = Visits::<u32>::in_memory();
        assert!(visit(&mut v, untitled, "", 1));
        assert_eq!(v.db().page(untitled).unwrap().title.as_deref(), Some(title_for(None, untitled).as_str()));
        assert_eq!(v.db().page(untitled).unwrap().title.as_deref(), Some("127.0.0.1"));
        assert!(visit(&mut v, "https://example.com/c", "  A   Title ", 2));
        assert_eq!(v.db().page("https://example.com/c").unwrap().title.as_deref(), Some("A Title"));
    }

    #[test]
    fn tabs_do_not_mix_their_navigations() {
        let mut v = Visits::<u32>::in_memory();
        v.committed(1, &url("https://one.example/"), 1);
        v.committed(2, &url("https://two.example/"), 2);
        assert!(v.completed(2, Some("Two"), 3));
        v.forget(1);
        assert!(!v.completed(1, Some("One"), 4));
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn writes_are_coalesced_and_the_flush_writes_what_is_left() {
        let dir = temp("coalesce");
        let mut v = Visits::<u32>::load(Some(&dir));
        let file = dir.join("history.json");
        assert!(visit(&mut v, "https://example.com/a", "A", 1_000));
        v.tick(2_000);
        assert!(!file.exists(), "not within the interval");
        let t0 = 1_001 + SAVE_EVERY_US;
        v.tick(t0);
        v.settle();
        let first = std::fs::read_to_string(&file).unwrap();
        assert!(first.contains("example.com/a"));
        // A second visit inside the interval waits for it ...
        assert!(visit(&mut v, "https://example.com/b", "B", t0 + 3_000_000));
        v.tick(t0 + 5_000_000);
        assert!(!std::fs::read_to_string(&file).unwrap().contains("example.com/b"));
        // ... or for the exit.
        v.flush(t0 + 5_000_100);
        assert!(std::fs::read_to_string(&file).unwrap().contains("example.com/b"));
        // Nothing left to write: the flush leaves the file alone.
        let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
        v.flush(t0 + 6_000_000);
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), mtime);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_saved_history_comes_back() {
        let dir = temp("roundtrip");
        let mut v = Visits::<u32>::load(Some(&dir));
        v.intend(1, How::Typed { search: false });
        assert!(visit(&mut v, "https://example.com/a", "A", 1_000));
        assert!(visit(&mut v, "https://example.org/", "Org", 2_000));
        v.flush(3_000);
        let back = Visits::<u32>::load(Some(&dir));
        assert_eq!(back.len(), 2);
        let p = back.db().page("https://example.com/a").unwrap();
        assert_eq!((p.title.as_deref(), p.typed_count, p.visit_count), (Some("A"), 1, 1));
        assert_eq!(back.candidates("org")[0].url, "https://example.org/");
        // No temp file is left behind.
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_profile_writes_nothing_and_still_remembers_the_session() {
        let dir = temp("none");
        let mut v = Visits::<u32>::load(None);
        assert!(visit(&mut v, "https://example.com/a", "A", 1_000));
        v.tick(i64::MAX / 2);
        v.flush(i64::MAX / 2);
        assert_eq!(v.candidates("example").len(), 1);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unusable_file_starts_empty_and_a_big_one_is_not_read() {
        let dir = temp("bad");
        let file = dir.join("history.json");
        std::fs::write(&file, "not json").unwrap();
        assert!(Visits::<u32>::load(Some(&dir)).is_empty());
        std::fs::write(&file, r#"{"version":9,"next_page":1,"next_visit":1,"pages":[]}"#).unwrap();
        assert!(Visits::<u32>::load(Some(&dir)).is_empty());
        let big = std::fs::File::create(&file).unwrap();
        big.set_len(MAX_FILE + 1).unwrap();
        drop(big);
        assert!(Visits::<u32>::load(Some(&dir)).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_history_is_bounded_and_the_oldest_pages_go() {
        let mut db = HistoryDb::new();
        for i in 0..50i64 {
            db.record_visit(NewVisit {
                url: &format!("https://example.com/{i}"),
                title: None,
                time_us: 1_000 + i,
                transition: Transition::Link,
                source: Source::Organic,
                interesting: false,
                from: None,
            });
        }
        assert_eq!(trim(&mut db, 50, 40), 0, "at the limit is not over it");
        assert_eq!(trim(&mut db, 49, 40), 10);
        assert_eq!(db.len(), 40);
        assert!(db.page("https://example.com/9").is_none());
        assert!(db.page("https://example.com/10").is_some());
        // All at one instant: still cut to the number asked for.
        let mut same = HistoryDb::new();
        for i in 0..30 {
            same.record_visit(NewVisit {
                url: &format!("https://example.com/{i}"),
                title: None,
                time_us: 7,
                transition: Transition::Link,
                source: Source::Organic,
                interesting: false,
                from: None,
            });
        }
        assert_eq!(trim(&mut same, 20, 10), 20);
        assert_eq!(same.len(), 10);
    }

    #[test]
    fn recording_past_the_limit_cuts_back_and_the_newest_stay() {
        let mut v = Visits::<u32>::in_memory();
        for i in 0..(MAX_PAGES as i64 + 3) {
            assert!(visit(&mut v, &format!("https://example.com/{i}"), "t", 10 * i));
        }
        assert_eq!(v.len(), TRIM_TO + 2);
        assert!(v.db().page(&format!("https://example.com/{}", MAX_PAGES + 2)).is_some());
        assert!(v.db().page("https://example.com/0").is_none());
    }

    #[test]
    fn a_file_over_the_limit_is_cut_on_load() {
        let dir = temp("over");
        let mut db = HistoryDb::new();
        for i in 0..(MAX_PAGES as i64 + 5) {
            db.record_visit(NewVisit {
                url: &format!("https://example.com/{i}"),
                title: None,
                time_us: i,
                transition: Transition::Link,
                source: Source::Organic,
                interesting: false,
                from: None,
            });
        }
        std::fs::write(dir.join("history.json"), db.to_json()).unwrap();
        assert_eq!(Visits::<u32>::load(Some(&dir)).len(), TRIM_TO);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn candidates_are_the_pages_with_every_typed_word_best_first() {
        let mut v = Visits::<u32>::in_memory();
        let now = now_us();
        v.intend(1, How::Typed { search: false });
        assert!(visit(&mut v, "https://rust-lang.org/", "Rust Programming Language", now));
        assert!(visit(&mut v, "https://example.com/rust", "Other", now));
        assert!(visit(&mut v, "https://example.com/go", "Go", now));
        let urls = |text| v.candidates(text).into_iter().map(|e| e.url).collect::<Vec<_>>();
        assert_eq!(urls("rust").len(), 2);
        assert_eq!(urls("rust language"), ["https://rust-lang.org/"]);
        assert!(urls("").is_empty());
        let top = &v.candidates("rust")[0];
        assert_eq!((top.typed_count, top.visit_count), (1, 1), "the typed one ranks first");
    }

    /// A writer that waits for a token per write, and logs the number of pages in each snapshot it was given.
    fn gated() -> (Writer<HistoryDb>, Arc<Mutex<Vec<usize>>>, mpsc::Sender<()>) {
        let (release, gate) = mpsc::channel::<()>();
        let (gate, log) = (Mutex::new(gate), Arc::new(Mutex::new(Vec::new())));
        let seen = log.clone();
        let writer: Writer<HistoryDb> = Arc::new(move |_, db| {
            gate.lock().unwrap().recv().ok();
            seen.lock().unwrap().push(db.len());
            Ok(())
        });
        (writer, log, release)
    }

    #[test]
    fn a_write_in_flight_does_not_block_tick() {
        let (writer, log, release) = gated();
        let mut v = Visits::<u32>::with_writer(PathBuf::from("history.json"), writer);
        assert!(visit(&mut v, "https://example.com/a", "A", 1_000));
        v.tick(1_001 + SAVE_EVERY_US);
        // The worker is parked on the gate; the engine thread went on, and the history keeps changing.
        let t1 = 1_001 + SAVE_EVERY_US + 10;
        assert!(visit(&mut v, "https://example.com/b", "B", t1));
        v.tick(t1 + SAVE_EVERY_US + 5);
        v.tick(t1 + 3 * SAVE_EVERY_US);
        assert!(log.lock().unwrap().is_empty(), "nothing has been written yet, and no second write was started");
        release.send(()).unwrap();
        v.settle();
        assert_eq!(*log.lock().unwrap(), [1], "the snapshot is the state when the write began");
        // What changed meanwhile is still due, and goes out next.
        v.tick(t1 + 4 * SAVE_EVERY_US);
        release.send(()).unwrap();
        v.settle();
        assert_eq!(*log.lock().unwrap(), [1, 2]);
    }

    #[test]
    fn flush_after_a_write_in_flight_writes_the_latest_state() {
        let (writer, log, release) = gated();
        let mut v = Visits::<u32>::with_writer(PathBuf::from("history.json"), writer);
        release.send(()).unwrap();
        release.send(()).unwrap();
        assert!(visit(&mut v, "https://example.com/a", "A", 1_000));
        v.tick(1_001 + SAVE_EVERY_US);
        assert!(visit(&mut v, "https://example.com/b", "B", 1_001 + SAVE_EVERY_US + 10));
        v.flush(1_001 + SAVE_EVERY_US + 20);
        assert_eq!(*log.lock().unwrap(), [1, 2], "the one in flight lands, then the latest state");
        // Nothing left: a second flush writes nothing.
        v.flush(i64::MAX / 2);
        assert_eq!(log.lock().unwrap().len(), 2);
    }

    #[test]
    fn history_json_holds_no_credential_from_an_oauth_redirect() {
        let dir = temp("redact");
        let mut v = Visits::<u32>::load(Some(&dir));
        assert!(visit(&mut v, "https://app.example/cb?code=SECRETCODE1&x=1&state=st#frag", "Signed in", 10));
        // A second login to the same place is the same page.
        assert!(visit(&mut v, "https://app.example/cb?code=SECRETCODE2&state=other", "Signed in", 20));
        assert!(visit(&mut v, "https://spa.example/cb#access_token=SECRETTOKEN&token_type=bearer&x=1", "SPA", 30));
        assert!(visit(&mut v, "https://spa.example/route#/settings", "Settings", 40));
        // A title arriving late reaches the redacted page.
        v.titled(1, "Settings late", 41);
        let places: Vec<String> = v.candidates("example").into_iter().map(|e| e.url).collect();
        assert_eq!(v.len(), 3, "{places:?}");
        assert!(places.contains(&"https://app.example/cb".to_string()), "{places:?}");
        assert!(places.contains(&"https://spa.example/cb".to_string()), "{places:?}");
        assert!(places.contains(&"https://spa.example/route#/settings".to_string()), "{places:?}");
        assert_eq!(v.db().page("https://app.example/cb").unwrap().visit_count, 2);
        v.flush(100);
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        for s in ["SECRET", "code=", "access_token", "state=", "frag"] {
            assert!(!text.contains(s), "{s} in {text}");
        }
        assert!(text.contains("https://app.example/cb"));
        // Loading it back gives the same places.
        let back = Visits::<u32>::load(Some(&dir));
        assert_eq!(back.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
