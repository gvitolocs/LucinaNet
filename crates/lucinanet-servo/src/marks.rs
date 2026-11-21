//! The bookmarks the engine keeps: `lucinanet-bookmarks`' tree over the profile's `bookmarks.json`
//! (docs/specs/wiring-bookmarks.md). Nothing here touches Servo, so the glue is tested on its own; the engine asks
//! [`Marks`] to toggle the shown page's bookmark, whether an address is bookmarked, and for the bookmarks the address bar
//! may suggest. Saving is `persist::Saver`, the same as the history's.

use std::path::Path;
use std::sync::Arc;

use lucinanet_bookmarks::{Bookmarks, Id, Kind, OTHER, ROOT};
use lucinanet_omnibox::BookmarkEntry;
use url::Url;

use crate::persist::{Saver, Writer};
use crate::visits::is_error_title;
use crate::zoom::write_atomic;

/// The file's name in the profile directory.
const FILE: &str = "bookmarks.json";

/// Where the file of an unusable `bookmarks.json` is moved, so the next write cannot destroy what the person had.
const ASIDE: &str = "bookmarks.json.unreadable";

/// The most bookmarks kept. Neither Firefox nor Chromium has a limit; a person's bookmarks are not ours to expire, so this
/// only stops growth: past it the star refuses and says why. 20000 bookmarks of ordinary length are a few MB, rewritten whole.
pub const MAX_BOOKMARKS: usize = 20_000;

/// The largest `bookmarks.json` read. A full store is a few MB; a bigger file is not ours.
pub const MAX_FILE: u64 = 32 << 20;

/// The longest address the star bookmarks (bytes). The crate takes 65536 (Firefox's `DB_URL_LENGTH_MAX`); a page that long is
/// a data transport and not worth a bookmark, and the bound keeps the file's size predictable.
pub const MAX_URL: usize = 8192;

/// The bookmarks file is written at most this often (microseconds), and at exit. A bookmark is a deliberate act, so it
/// waits much less than a visit does.
pub const SAVE_EVERY_US: i64 = 2_000_000;

/// How many bookmarks one address-bar query hands to the ranker.
pub const CANDIDATES: usize = 200;

/// The folder a new bookmark goes to. Firefox's default is the Bookmarks Toolbar (`PlacesUIUtils.defaultParentGuid`, pref
/// `browser.bookmarks.defaultLocation` empty); there is no bar yet to show it, so it is "Other Bookmarks" (`OTHER`) until
/// the bar exists. One constant, so that piece changes one line.
pub const NEW_BOOKMARK_FOLDER: Id = OTHER;

/// What the star did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggled {
    Added,
    /// Every bookmark of the address went; how many.
    Removed(usize),
    Refused(Refusal),
}

/// Why the star did nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Not a web or file page (`about:blank`, `data:`, an error page, no address).
    NotBookmarkable,
    TooLong,
    Full,
}

impl Refusal {
    /// The line the bar shows.
    pub fn note(self) -> String {
        match self {
            Refusal::NotBookmarkable => "This page cannot be bookmarked".into(),
            Refusal::TooLong => "This address is too long to bookmark".into(),
            Refusal::Full => format!("No more bookmarks: {MAX_BOOKMARKS} are kept, remove some first"),
        }
    }
}

/// The address as a bookmark keeps it: Firefox bookmarks `createExposableURI(browser.currentURI)`, the address without a user
/// name and password (`PlacesCommandHook.bookmarkPage`, browser-places.js @33abb90). None when the page is not one to bookmark.
pub fn bookmarkable(url: &Url) -> Option<Url> {
    if !matches!(url.scheme(), "http" | "https" | "ftp" | "file") {
        return None;
    }
    let mut exposable = url.clone();
    if exposable.set_username("").is_err() || exposable.set_password(None).is_err() {
        // A scheme without user info (file:) has nothing to take out.
        return Some(url.clone());
    }
    Some(exposable)
}

/// The title a new bookmark gets: the page's, else its address (`info.title = info.title || url.href`, same function). An
/// error page's title is not the page's.
pub fn title_for(url: &Url, title: &str) -> String {
    let title = title.trim();
    if title.is_empty() || is_error_title(title) { url.as_str().to_owned() } else { title.to_owned() }
}

/// The bookmarks and their file.
pub struct Marks {
    bm: Bookmarks,
    /// `<profile>/bookmarks.json`; no file with `LUCINANET_PROFILE_DIR=none`, and then nothing is ever written.
    saver: Saver<Bookmarks>,
    cap: usize,
}

fn write_file(path: &Path, bm: &Bookmarks) -> std::io::Result<()> {
    write_atomic(path, bm.to_json().as_bytes())
}

impl Marks {
    /// The bookmarks of the profile in `dir`, or none if there is no profile or no file. A file that cannot be read (too big, not
    /// ours, a newer version) is moved to `bookmarks.json.unreadable` and the store starts empty, so the first bookmark cannot
    /// replace what the person had with nothing. A store with more than [`MAX_BOOKMARKS`] is kept whole; it only cannot grow.
    pub fn load(dir: Option<&Path>) -> Marks {
        let file = dir.map(|dir| dir.join(FILE));
        let bm = file.as_deref().and_then(read).unwrap_or_default();
        let writer: Writer<Bookmarks> = Arc::new(write_file);
        Marks { bm, saver: Saver::new(file, "bookmarks", SAVE_EVERY_US, writer), cap: MAX_BOOKMARKS }
    }

    /// The bookmarks of nobody's profile, for tests.
    #[cfg(test)]
    pub fn in_memory() -> Marks {
        Marks::load(None)
    }

    /// Whether the address is bookmarked (by its exposable form, as the star would store it).
    pub fn is_bookmarked(&self, url: &Url) -> bool {
        bookmarkable(url).is_some_and(|url| self.bm.is_bookmarked(url.as_str()))
    }

    /// The star on the page at `url` with `title`: a bookmarked page loses all its bookmarks (Firefox's "Remove Bookmark" takes
    /// every bookmark of the address, `StarUI` `_itemGuids`; Chromium's `RemoveAllBookmarks`), any other becomes one in
    /// [`NEW_BOOKMARK_FOLDER`], last (`AddIfNotBookmarked` adds at the end of the folder).
    pub fn toggle(&mut self, url: &Url, title: &str, now_us: i64) -> Toggled {
        let Some(url) = bookmarkable(url) else { return Toggled::Refused(Refusal::NotBookmarkable) };
        let existing = self.bm.find_by_url(url.as_str());
        if !existing.is_empty() {
            let removed = existing.into_iter().filter(|id| self.bm.remove(*id).is_ok()).count();
            self.saver.changed(now_us);
            return Toggled::Removed(removed);
        }
        if url.as_str().len() > MAX_URL {
            return Toggled::Refused(Refusal::TooLong);
        }
        if self.bm.len_bookmarks() >= self.cap {
            return Toggled::Refused(Refusal::Full);
        }
        match self.bm.add_bookmark(NEW_BOOKMARK_FOLDER, None, &title_for(&url, title), url.as_str(), now_us) {
            Ok(_) => {
                self.saver.changed(now_us);
                Toggled::Added
            },
            // The URL was parsed already; the folder is built in. Reaching here is a bug, and the star must not lie.
            Err(error) => {
                eprintln!("lucinanet: bookmark not added: {error:?}");
                Toggled::Refused(Refusal::NotBookmarkable)
            },
        }
    }

    /// The bookmarks the address bar may suggest for `text`: every typed word is somewhere in the title, address, tags or keyword
    /// (the widest the ranker narrows from), best first, at most [`CANDIDATES`].
    pub fn candidates(&self, text: &str) -> Vec<BookmarkEntry> {
        entries_for(&self.bm, text, CANDIDATES)
    }

    pub fn bookmark_count(&self) -> usize {
        self.bm.len_bookmarks()
    }

    pub fn tick(&mut self, now_us: i64) {
        self.saver.tick(now_us, &self.bm);
    }

    /// Write what is left and wait for it (exit, and the command channel closing).
    pub fn flush(&mut self, now_us: i64) {
        self.saver.flush(now_us, &self.bm);
    }

    /// Bookmarks with a fake writer, for tests.
    #[cfg(test)]
    fn with_writer(file: std::path::PathBuf, writer: Writer<Bookmarks>) -> Marks {
        Marks { bm: Bookmarks::new(), saver: Saver::new(None, "bookmarks", SAVE_EVERY_US, writer).with_file(file), cap: MAX_BOOKMARKS }
    }

    #[cfg(test)]
    fn settle(&mut self) {
        self.saver.settle();
    }
}

/// The omnibox's view of the bookmarks matching `text`. The words are read as the ranker reads them: a typed `https://` or
/// `www.` is not part of what a bookmark's address is searched for.
pub fn entries_for(bm: &Bookmarks, text: &str, limit: usize) -> Vec<BookmarkEntry> {
    let words: Vec<&str> = text
        .split_whitespace()
        .map(|w| {
            let w = w.strip_prefix("https://").or_else(|| w.strip_prefix("http://")).unwrap_or(w);
            w.strip_prefix("www.").unwrap_or(w)
        })
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    bm.search(&words.join(" "), limit)
        .into_iter()
        .filter_map(|id| {
            let node = bm.get(id)?;
            let Kind::Bookmark { url } = &node.kind else { return None };
            Some(BookmarkEntry { url: url.clone(), title: node.title.clone(), folder_path: folder_path(bm, node.parent) })
        })
        .collect()
}

/// "Other Bookmarks/News" for display.
fn folder_path(bm: &Bookmarks, mut at: Option<Id>) -> String {
    let mut names = Vec::new();
    while let Some(id) = at.filter(|id| *id != ROOT) {
        let Some(node) = bm.get(id) else { break };
        names.push(node.title.as_str());
        at = node.parent;
    }
    names.reverse();
    names.join("/")
}

/// The store from `path`, or None when there is no file or it is not usable (then moved aside, see [`Marks::load`]).
fn read(path: &Path) -> Option<Bookmarks> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!("lucinanet: {} not read: {error}", path.display());
            return None;
        },
    };
    let usable = if !meta.is_file() || meta.len() > MAX_FILE {
        Err(format!("not a file of at most {MAX_FILE} bytes"))
    } else {
        std::fs::read_to_string(path)
            .map_err(|error| error.to_string())
            .and_then(|text| Bookmarks::from_json(&text).map_err(|error| format!("{error:?}")))
    };
    match usable {
        Ok(bm) => Some(bm),
        Err(why) => {
            let aside = path.with_file_name(ASIDE);
            match std::fs::rename(path, &aside) {
                Ok(()) => eprintln!("lucinanet: {} unusable ({why}); kept as {}", path.display(), aside.display()),
                Err(error) => eprintln!("lucinanet: {} unusable ({why}) and not moved aside: {error}", path.display()),
            }
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Mutex, mpsc};

    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lucinanet-marks-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_star_adds_a_page_and_the_next_press_removes_it() {
        let mut m = Marks::in_memory();
        let page = url("https://example.com/a");
        assert!(!m.is_bookmarked(&page));
        assert_eq!(m.toggle(&page, "A page", 10), Toggled::Added);
        assert!(m.is_bookmarked(&page));
        assert_eq!(m.bookmark_count(), 1);
        // It is in the Other folder, last, with the title and the time.
        let id = m.bm.find_by_url(page.as_str())[0];
        let node = m.bm.get(id).unwrap();
        assert_eq!((node.parent, node.title.as_str(), node.added_us), (Some(NEW_BOOKMARK_FOLDER), "A page", 10));
        assert_eq!(m.toggle(&page, "A page", 20), Toggled::Removed(1));
        assert!(!m.is_bookmarked(&page));
        assert_eq!(m.bookmark_count(), 0);
        // And again: the star is a toggle, not a one-way add.
        assert_eq!(m.toggle(&page, "A page", 30), Toggled::Added);
    }

    #[test]
    fn new_bookmarks_go_last_in_the_folder() {
        let mut m = Marks::in_memory();
        for (i, name) in ["a", "b", "c"].iter().enumerate() {
            assert_eq!(m.toggle(&url(&format!("https://example.com/{name}")), name, i as i64), Toggled::Added);
        }
        let titles: Vec<_> = m.bm.children(OTHER).unwrap().iter().map(|id| m.bm.get(*id).unwrap().title.clone()).collect();
        assert_eq!(titles, ["a", "b", "c"]);
    }

    #[test]
    fn the_same_address_spelled_differently_is_the_same_bookmark() {
        let mut m = Marks::in_memory();
        // The page the web view reports is `Url::as_str`; the store keeps that form, so a host in capitals or a bare host is one page.
        assert_eq!(m.toggle(&url("https://EXAMPLE.com"), "x", 1), Toggled::Added);
        assert!(m.is_bookmarked(&url("https://example.com/")));
        // A fragment is a different address, as in both browsers.
        assert!(!m.is_bookmarked(&url("https://example.com/#top")));
        assert_eq!(m.toggle(&url("https://example.com/"), "x", 2), Toggled::Removed(1));
    }

    #[test]
    fn user_info_is_not_bookmarked() {
        let mut m = Marks::in_memory();
        let page = url("https://me:secret@example.com/private");
        assert_eq!(m.toggle(&page, "Private", 1), Toggled::Added);
        let id = m.bm.find_by_url("https://example.com/private")[0];
        let Kind::Bookmark { url: stored } = &m.bm.get(id).unwrap().kind else { panic!() };
        assert_eq!(stored, "https://example.com/private");
        assert!(!m.bm.to_json().contains("secret"));
        // The same page, with or without the password, is the one bookmark.
        assert!(m.is_bookmarked(&url("https://example.com/private")));
        assert!(m.is_bookmarked(&page));
    }

    #[test]
    fn removing_takes_every_bookmark_of_the_address() {
        let mut m = Marks::in_memory();
        let page = url("https://example.com/a");
        assert_eq!(m.toggle(&page, "A", 1), Toggled::Added);
        // The same address in another folder, as an import or a manager makes it.
        let folder = m.bm.add_folder(lucinanet_bookmarks::TOOLBAR, None, "Work", 2).unwrap();
        m.bm.add_bookmark(folder, None, "A again", page.as_str(), 3).unwrap();
        assert_eq!(m.bookmark_count(), 2);
        assert_eq!(m.toggle(&page, "A", 4), Toggled::Removed(2));
        assert_eq!(m.bookmark_count(), 0);
        // The folder it sat in stays.
        assert!(m.bm.get(folder).is_some());
    }

    #[test]
    fn the_title_is_the_pages_else_its_address() {
        let page = url("https://example.com/a");
        assert_eq!(title_for(&page, "  A page  "), "A page");
        assert_eq!(title_for(&page, ""), "https://example.com/a");
        assert_eq!(title_for(&page, "   "), "https://example.com/a");
        assert_eq!(title_for(&page, "Error loading page"), "https://example.com/a");
        assert_eq!(title_for(&page, "Certificate error"), "https://example.com/a");
    }

    #[test]
    fn only_web_and_file_pages_are_bookmarked() {
        let mut m = Marks::in_memory();
        for page in ["about:blank", "data:text/html,hi", "blob:https://example.com/1", "javascript:alert(1)"] {
            assert_eq!(m.toggle(&url(page), "t", 1), Toggled::Refused(Refusal::NotBookmarkable), "{page}");
            assert!(!m.is_bookmarked(&url(page)));
        }
        assert_eq!(m.toggle(&url("file:///tmp/a.html"), "t", 1), Toggled::Added);
        assert_eq!(m.toggle(&url("ftp://example.com/f"), "t", 2), Toggled::Added);
        assert_eq!(m.bookmark_count(), 2);
    }

    #[test]
    fn the_store_is_bounded() {
        let mut m = Marks::in_memory();
        m.cap = 3;
        for i in 0..3 {
            assert_eq!(m.toggle(&url(&format!("https://example.com/{i}")), "t", i), Toggled::Added);
        }
        assert_eq!(m.toggle(&url("https://example.com/3"), "t", 9), Toggled::Refused(Refusal::Full));
        assert_eq!(m.bookmark_count(), 3);
        assert!(Refusal::Full.note().contains("20000"));
        // A full store still removes: the star on a bookmarked page works, and frees a place.
        assert_eq!(m.toggle(&url("https://example.com/1"), "t", 10), Toggled::Removed(1));
        assert_eq!(m.toggle(&url("https://example.com/3"), "t", 11), Toggled::Added);
        // An address past the bound is refused, even in an empty store.
        let mut m = Marks::in_memory();
        let long = url(&format!("https://example.com/{}", "a".repeat(MAX_URL)));
        assert_eq!(m.toggle(&long, "t", 1), Toggled::Refused(Refusal::TooLong));
        assert_eq!(m.bookmark_count(), 0);
    }

    #[test]
    fn a_store_over_the_cap_on_disk_is_kept_whole() {
        let dir = temp("overcap");
        let mut bm = Bookmarks::new();
        for i in 0..12 {
            bm.add_bookmark(OTHER, None, "t", &format!("https://example.com/{i}"), 1).unwrap();
        }
        std::fs::write(dir.join(FILE), bm.to_json()).unwrap();
        let mut m = Marks::load(Some(&dir));
        m.cap = 10;
        assert_eq!(m.bookmark_count(), 12, "nothing the person kept is dropped");
        assert_eq!(m.toggle(&url("https://example.com/new"), "t", 1), Toggled::Refused(Refusal::Full));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_saved_store_comes_back() {
        let dir = temp("roundtrip");
        let mut m = Marks::load(Some(&dir));
        assert_eq!(m.toggle(&url("https://example.com/a"), "A", 1_000), Toggled::Added);
        assert_eq!(m.toggle(&url("https://example.org/"), "Org \u{00e5}\"q\"", 2_000), Toggled::Added);
        m.flush(3_000);
        let back = Marks::load(Some(&dir));
        assert_eq!(back.bookmark_count(), 2);
        assert!(back.is_bookmarked(&url("https://example.com/a")));
        let id = back.bm.find_by_url("https://example.org/")[0];
        let node = back.bm.get(id).unwrap();
        assert_eq!((node.title.as_str(), node.added_us, node.parent), ("Org \u{00e5}\"q\"", 2_000, Some(OTHER)));
        // A removal is saved too, and no temp file is left behind.
        let mut back = back;
        assert_eq!(back.toggle(&url("https://example.com/a"), "A", 4_000), Toggled::Removed(1));
        back.flush(5_000);
        assert_eq!(Marks::load(Some(&dir)).bookmark_count(), 1);
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_profile_writes_nothing_and_still_remembers_the_session() {
        let dir = temp("none");
        let mut m = Marks::load(None);
        assert_eq!(m.toggle(&url("https://example.com/a"), "A", 1_000), Toggled::Added);
        m.tick(i64::MAX / 2);
        m.flush(i64::MAX / 2);
        assert!(m.is_bookmarked(&url("https://example.com/a")));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_is_not_ours_is_kept_aside_and_the_store_starts_empty() {
        let dir = temp("bad");
        let file = dir.join(FILE);
        let aside = dir.join(ASIDE);
        std::fs::write(&file, "not json").unwrap();
        assert_eq!(Marks::load(Some(&dir)).bookmark_count(), 0);
        assert!(!file.exists());
        assert_eq!(std::fs::read_to_string(&aside).unwrap(), "not json");
        // A newer version of the format.
        std::fs::write(&file, r#"{"version":9,"next_id":4,"nodes":[]}"#).unwrap();
        assert_eq!(Marks::load(Some(&dir)).bookmark_count(), 0);
        assert!(std::fs::read_to_string(&aside).unwrap().contains("\"version\":9"));
        // One over the read limit is not read, and not lost either.
        let big = std::fs::File::create(&file).unwrap();
        big.set_len(MAX_FILE + 1).unwrap();
        drop(big);
        assert_eq!(Marks::load(Some(&dir)).bookmark_count(), 0);
        assert_eq!(std::fs::metadata(&aside).unwrap().len(), MAX_FILE + 1);
        // No file at all is just a new profile: nothing is moved.
        let fresh = temp("fresh");
        assert_eq!(Marks::load(Some(&fresh)).bookmark_count(), 0);
        assert_eq!(std::fs::read_dir(&fresh).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&fresh);
    }

    /// A writer that waits for a token per write, and logs how many bookmarks each snapshot held.
    fn gated() -> (Writer<Bookmarks>, Arc<Mutex<Vec<usize>>>, mpsc::Sender<()>) {
        let (release, gate) = mpsc::channel::<()>();
        let (gate, log) = (Mutex::new(gate), Arc::new(Mutex::new(Vec::new())));
        let seen = log.clone();
        let writer: Writer<Bookmarks> = Arc::new(move |_, bm| {
            gate.lock().unwrap().recv().ok();
            seen.lock().unwrap().push(bm.len_bookmarks());
            Ok(())
        });
        (writer, log, release)
    }

    #[test]
    fn writes_are_coalesced_off_the_calling_thread_and_the_flush_writes_the_rest() {
        let (writer, log, release) = gated();
        let mut m = Marks::with_writer(PathBuf::from("bookmarks.json"), writer);
        assert_eq!(m.toggle(&url("https://example.com/a"), "A", 1_000), Toggled::Added);
        m.tick(1_000 + SAVE_EVERY_US - 1);
        m.tick(1_000 + SAVE_EVERY_US);
        // The worker is parked on the gate; the caller went on, and more stars were pressed.
        assert_eq!(m.toggle(&url("https://example.com/b"), "B", 1_000 + SAVE_EVERY_US + 5), Toggled::Added);
        m.tick(1_000 + 5 * SAVE_EVERY_US);
        assert!(log.lock().unwrap().is_empty(), "nothing written yet, and no second write started");
        release.send(()).unwrap();
        m.settle();
        assert_eq!(*log.lock().unwrap(), [1], "the snapshot is the state when the write began");
        // What changed meanwhile is written at the exit: the latest state, once.
        release.send(()).unwrap();
        m.flush(1_000 + 6 * SAVE_EVERY_US);
        assert_eq!(*log.lock().unwrap(), [1, 2]);
        m.flush(i64::MAX / 2);
        assert_eq!(log.lock().unwrap().len(), 2, "nothing left to write");
    }

    #[test]
    fn a_refused_star_changes_nothing_and_writes_nothing() {
        let (writer, log, _release) = gated();
        let mut m = Marks::with_writer(PathBuf::from("bookmarks.json"), writer);
        assert_eq!(m.toggle(&url("about:blank"), "", 1), Toggled::Refused(Refusal::NotBookmarkable));
        m.tick(i64::MAX / 2);
        m.flush(i64::MAX / 2);
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn the_bar_is_offered_the_bookmarks_with_every_typed_word() {
        let mut m = Marks::in_memory();
        m.toggle(&url("https://news.example.com/world"), "World News", 1);
        m.toggle(&url("https://rust-lang.org/"), "Rust Programming Language", 2);
        m.toggle(&url("https://example.com/rust-news"), "Other", 3);
        let urls = |text: &str| m.candidates(text).into_iter().map(|e| e.url).collect::<Vec<_>>();
        assert_eq!(urls("rust").len(), 2);
        assert_eq!(urls("rust language"), ["https://rust-lang.org/"]);
        assert!(urls("").is_empty());
        assert!(urls("   ").is_empty());
        assert!(urls("zzz").is_empty());
        // What the person typed of the scheme or `www.` is not searched for.
        assert_eq!(urls("https://news.example"), ["https://news.example.com/world"]);
        assert_eq!(urls("www.rust-lang"), ["https://rust-lang.org/"]);
        assert!(urls("https://").is_empty());
        let top = m.candidates("world");
        assert_eq!(top.len(), 1);
        assert_eq!((top[0].title.as_str(), top[0].folder_path.as_str()), ("World News", "Other Bookmarks"));
    }

    #[test]
    fn the_candidates_are_bounded_and_say_their_folder() {
        let mut bm = Bookmarks::new();
        let work = bm.add_folder(lucinanet_bookmarks::TOOLBAR, None, "Work", 1).unwrap();
        let docs = bm.add_folder(work, None, "Docs", 1).unwrap();
        for i in 0..30 {
            bm.add_bookmark(docs, None, "page", &format!("https://example.com/{i}"), 1).unwrap();
        }
        let found = entries_for(&bm, "page", 10);
        assert_eq!(found.len(), 10);
        assert_eq!(found[0].folder_path, "Bookmarks Toolbar/Work/Docs");
        assert!(entries_for(&bm, "page", 0).is_empty());
    }
}
