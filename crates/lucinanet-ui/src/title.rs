//! The title a tab and the history show for the current document: the one rule, in one place, with no engine in it.
//!
//! Servo reports the document's `<title>` as `Option<String>` (None when the document has no title element, Some("")
//! for an empty one) and only for the document that is active. Whatever the engine says, the title shown is
//! `title_for(that, the current address)`, so an address without a usable title is labelled by its host and never by
//! the page that was shown before.

use url::Url;

/// Titles are cut here: a page cannot make a tab strip or the history file grow by naming itself at length.
pub const MAX_TITLE: usize = 512;

/// The title for a document at `url`: its own title with white space trimmed and each run of it (newlines and tabs
/// included) made one space, cut at [`MAX_TITLE`] characters; when that leaves nothing, the host of `url` without a
/// leading `www.`. An address without a host (`about:blank`, `data:`) has no fallback and gives ""; a `file:` address
/// falls back to its last path segment.
pub fn title_for(document_title: Option<&str>, url: &str) -> String {
    let own: String = document_title
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
        .chars()
        .take(MAX_TITLE)
        .collect();
    if !own.is_empty() {
        return own;
    }
    fallback(url)
}

fn fallback(url: &str) -> String {
    let Ok(url) = Url::parse(url) else { return String::new() };
    if url.scheme() == "file" {
        return url.path_segments().and_then(|mut s| s.next_back()).unwrap_or_default().to_owned();
    }
    match url.host_str() {
        Some(host) => host.strip_prefix("www.").filter(|h| !h.is_empty()).unwrap_or(host).to_owned(),
        None => String::new(),
    }
}

/// What a tab knows of its document's title. Servo reports a title only when the new document has one (or has
/// changed it), so the old document's title would stay on a document without a title; this resets it when a navigation
/// commits, and the tab strip and the history read from here and never from the web view's own last value.
///
/// The order of the callbacks for a navigation: `load_started`, then `url_committed` and `title_changed` in either
/// order (the new document's title can arrive before or after its address), then `load_ended`.
#[derive(Debug, Default)]
pub struct DocTitle {
    title: Option<String>,
    /// A navigation has started and not ended: the next committed address is a new document.
    navigating: bool,
    /// The title of the document being navigated to has been reported.
    fresh: bool,
}

impl DocTitle {
    /// A navigation began. The old document keeps its title until the new one commits.
    pub fn load_started(&mut self) {
        self.navigating = true;
        self.fresh = false;
    }

    /// An address committed. During a navigation that is the new document: until it reports a title it has none.
    /// Outside one (`pushState`, a fragment) it is the same document and keeps its title.
    pub fn url_committed(&mut self) {
        if self.navigating && !self.fresh {
            self.title = None;
        }
    }

    /// The document reported its title (None: it has no `<title>`).
    pub fn title_changed(&mut self, title: Option<String>) {
        self.title = title;
        self.fresh = true;
    }

    /// The load ended (complete or stopped): `page_title` is what the engine holds for the active document, which is
    /// the truth now that every callback of the navigation has come. Servo re-reports the title, `None` included, when a
    /// document becomes fully active (`Document::set_activity` and `Window::resume` both call `title_changed`), so by the
    /// end of the load the web view holds this document's own title, not the previous document's.
    pub fn load_ended(&mut self, page_title: Option<String>) {
        self.title = page_title;
        self.navigating = false;
        self.fresh = false;
    }

    /// The document's own title as last reported, if any.
    pub fn document_title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// What the tab shows for the document at `url`.
    pub fn shown(&self, url: &str) -> String {
        title_for(self.document_title(), url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule_as_a_table() {
        let long = "x".repeat(600);
        let cases: [(Option<&str>, &str, String); 14] = [
            (Some("Alpha"), "http://a.test/", "Alpha".into()),
            (None, "http://a.test/page", "a.test".into()),
            (Some(""), "http://a.test/", "a.test".into()),
            (Some("   \n\t "), "https://a.test/", "a.test".into()),
            (Some("  Spaced \n  out\t\tnow  "), "http://a.test/", "Spaced out now".into()),
            (Some(&long), "http://a.test/", "x".repeat(MAX_TITLE)),
            (None, "https://www.youtube.com/watch?v=1", "youtube.com".into()),
            (None, "https://accounts.google.com/signin", "accounts.google.com".into()),
            (Some(""), "https://www2.example.org/", "www2.example.org".into()),
            (None, "http://127.0.0.1:18781/c", "127.0.0.1".into()),
            (None, "file:///home/user/notes/todo.html", "todo.html".into()),
            (None, "about:blank", String::new()),
            (Some("T"), "about:blank", "T".into()),
            (None, "not a url", String::new()),
        ];
        for (title, url, want) in cases {
            assert_eq!(title_for(title, url), want, "title {title:?} at {url}");
        }
    }

    #[test]
    fn the_cap_counts_characters_not_bytes() {
        let title = "\u{e9}".repeat(MAX_TITLE + 5);
        assert_eq!(title_for(Some(&title), "http://a.test/").chars().count(), MAX_TITLE);
    }

    const FROM: &str = "https://www.youtube.com/";
    const TO: &str = "https://accounts.google.com/signin";

    fn on(title: &str) -> DocTitle {
        let mut t = DocTitle::default();
        t.load_started();
        t.url_committed();
        t.title_changed(Some(title.into()));
        t.load_ended(Some(title.into()));
        t
    }

    #[test]
    fn a_new_document_without_a_title_does_not_keep_the_old_one() {
        let mut t = on("YouTube");
        assert_eq!(t.shown(FROM), "YouTube");
        t.load_started();
        assert_eq!(t.shown(FROM), "YouTube", "the old document is still shown until the new one commits");
        t.url_committed();
        assert_eq!(t.shown(TO), "accounts.google.com");
        t.load_ended(None);
        assert_eq!(t.shown(TO), "accounts.google.com");
    }

    #[test]
    fn the_new_documents_title_replaces_the_old_in_either_order() {
        // The title before the address.
        let mut t = on("YouTube");
        t.load_started();
        t.title_changed(Some("Sign in".into()));
        t.url_committed();
        assert_eq!(t.shown(TO), "Sign in");
        t.load_ended(Some("Sign in".into()));
        assert_eq!(t.shown(TO), "Sign in");
        // The address before the title.
        let mut t = on("YouTube");
        t.load_started();
        t.url_committed();
        assert_eq!(t.shown(TO), "accounts.google.com");
        t.title_changed(Some("Sign in".into()));
        assert_eq!(t.shown(TO), "Sign in");
    }

    #[test]
    fn an_empty_title_is_the_host() {
        let mut t = on("Alpha");
        t.load_started();
        t.url_committed();
        t.title_changed(Some(String::new()));
        t.load_ended(Some(String::new()));
        assert_eq!(t.shown("http://www.a.test/d"), "a.test");
    }

    #[test]
    fn the_same_document_keeps_its_title() {
        let mut t = on("Video");
        t.url_committed(); // pushState: no navigation under way
        assert_eq!(t.shown(FROM), "Video");
        // A reload of a page whose title does not change: no title news, the end of the load restores it.
        t.load_started();
        t.url_committed();
        t.load_ended(Some("Video".into()));
        assert_eq!(t.shown(FROM), "Video");
    }

    #[test]
    fn a_stopped_navigation_leaves_the_old_title() {
        let mut t = on("Alpha");
        t.load_started();
        t.load_ended(Some("Alpha".into()));
        assert_eq!(t.shown("http://a.test/"), "Alpha");
    }

    /// What the engine really sends for the end-to-end run: a titled page, then one with no `<title>` at all, then Back.
    /// Servo reports an untitled document's title as `None` when that document becomes active, and a Back from the kept
    /// page sends no load status at all, only the committed address and the restored document's title.
    #[test]
    fn the_two_local_pages_and_the_back_of_the_end_to_end_run() {
        const A: &str = "http://127.0.0.1:18777/a.html";
        const B: &str = "http://127.0.0.1:18777/b.html";
        let mut t = DocTitle::default();
        // Page A, with a title.
        t.load_started();
        t.url_committed();
        t.title_changed(Some("A".into()));
        t.load_ended(Some("A".into()));
        assert_eq!(t.shown(A), "A");
        // Page B, without one: it commits, and the activation reports that this document has no title.
        t.load_started();
        t.url_committed();
        assert_eq!(t.shown(B), "127.0.0.1", "the address is the new one before the title arrives");
        t.title_changed(None);
        t.load_ended(None);
        assert_eq!(t.shown(B), "127.0.0.1", "and it stays the host: never page A's title");
        // Back to A: the kept page sends no Started and no Complete, only its address and its title.
        t.url_committed();
        assert_eq!(t.shown(A), "127.0.0.1", "B's title, which is none, holds until A's is reported");
        t.title_changed(Some("A".into()));
        assert_eq!(t.shown(A), "A");
    }

    #[test]
    fn a_page_that_never_reports_a_title_is_the_host_from_the_first_load() {
        let mut t = DocTitle::default();
        t.load_started();
        t.url_committed();
        t.load_ended(None);
        assert_eq!(t.shown("https://www.example.com/x"), "example.com");
    }
}
