//! What the address bar decides, apart from drawing it: what Enter does with the text (`lucinanet-omnibox`'s
//! `classify`), which rows the dropdown shows and how the arrow keys move over them, and what the field shows of an address
//! while nobody is typing in it. All of it is plain data in and out, so it is tested without a window
//! (docs/specs/wiring-omnibox-history.md).

use lucinanet_omnibox::{
    Action, BookmarkEntry, Context, HistoryEntry, Kind, MAX_SUGGESTIONS, RankInput, SearchEngine, Suggestion, classify, suggest,
};
use url::Url;

use crate::Cmd;

/// The engines the bar searches with. DuckDuckGo's HTML page is the default, as it was before the omnibox crate
/// (`SearchEngine::duckduckgo_html`); `lucinanet-settings` holds no profile file to read a choice from yet.
pub struct Engines {
    default_engine: SearchEngine,
    keywords: Vec<SearchEngine>,
}

impl Default for Engines {
    fn default() -> Engines {
        Engines { default_engine: SearchEngine::duckduckgo_html(), keywords: Vec::new() }
    }
}

impl Engines {
    pub fn context(&self) -> Context<'_> {
        Context { default_engine: &self.default_engine, keywords: &self.keywords, allow_single_label_hosts: false }
    }
}

/// Where Enter goes: the address, and whether the text became a search (a typed visit from the search source).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Go {
    pub url: Url,
    pub search: bool,
}

impl Go {
    /// The command that loads it as a typed address.
    pub fn command(self) -> Cmd {
        Cmd::LoadTyped { url: self.url, search: self.search }
    }
}

/// What Enter does with `typed`; None for blank text.
pub fn go_for(typed: &str, engines: &Engines) -> Option<Go> {
    match classify(typed, &engines.context()) {
        Action::Empty => None,
        Action::Navigate { url, .. } => Some(Go { url, search: false }),
        Action::Search { url, .. } | Action::Keyword { url, .. } => Some(Go { url, search: true }),
    }
}

/// An address, as the bar reads it: a URL, a bare host, or a search with the default engine.
pub fn to_url(typed: &str) -> Option<Url> {
    go_for(typed, &Engines::default()).map(|go| go.url)
}

/// The most characters a field is ever measured for: a ceiling for a caller whose box is wider than any address, and for
/// the search below an upper bound. `lucinanet-omnibox` counts scalars, so this is characters and not bytes.
pub const FIELD_MAX_CHARS: usize = 4096;

/// What a field of `width` points shows of `address`, with `domain` (the engine's registrable domain,
/// [`crate::TabInfo::registrable_domain`]) emphasised inside it: `lucinanet-omnibox`'s display, shortened by its own rules
/// until the caller's `measure` says it fits.
///
/// The field's text is not the address, and neither is the host: `https://` and a `www.` go, the tail that does not fit
/// becomes `…`, and what is left is what the person reads to know where he is (`docs/specs/wiring-omnibox-history.md`,
/// "Unfocused address display"). The width is searched over rather than computed because a glyph run is not a fixed number
/// of points: `measure` is the caller's own text measurement, so a glass canvas with its `Font` and a classic `Ui` with its
/// face each get the fit of their own glyphs, and both chromes still share the one rule about *what* is cut.
pub fn field_display(address: &str, domain: Option<&str>, width: f32, mut measure: impl FnMut(&str) -> f32) -> lucinanet_omnibox::display::Display {
    use lucinanet_omnibox::display::unfocused_display;

    // A display never grows with `max_chars` past the whole address, so the answer is a character count in this range and
    // every step of the search is a display that either fits or does not.
    let whole = address.chars().count().clamp(1, FIELD_MAX_CHARS);
    let widest = unfocused_display(address, domain, whole);
    if measure(&widest.text) <= width {
        return widest;
    }
    let (mut lo, mut hi) = (1, whole);
    while lo < hi {
        // Rounded up, so that `lo` is a width that has to be proved (see the step below) and the loop cannot stall.
        let mid = lo + (hi - lo + 1) / 2;
        if measure(&unfocused_display(address, domain, mid).text) <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    // Dropping a character can hand back more than one fewer (a `…` taking the place of a longer tail), so the search's
    // answer is verified downwards rather than trusted.
    let mut display = unfocused_display(address, domain, lo);
    while lo > 1 && measure(&display.text) > width {
        lo -= 1;
        display = unfocused_display(address, domain, lo);
    }
    display
}

/// [`field_display`]'s text in the three runs a field draws: the part before the emphasised one, that part, and the rest.
/// Either end can be empty; all three are when there is nothing to emphasise, which is the whole text drawn in one ink.
///
/// The range is the display's own, so it is a byte range of `display.text` on a char boundary — unless a caller paired a
/// display with another's range, in which case the text is drawn with nothing emphasised rather than panicking on a slice.
pub fn field_pieces(display: &lucinanet_omnibox::display::Display) -> (&str, &str, &str) {
    let text = display.text.as_str();
    let Some(range) = &display.domain else { return (text, "", "") };
    match (text.get(..range.start), text.get(range.clone())) {
        (Some(before), Some(strong)) => (before, strong, &text[range.end..]),
        _ => (text, "", ""),
    }
}

/// The dropdown's rows for `typed`: the omnibox crate's ranking over the history and bookmarks it was handed (remote
/// suggestions are not wired yet), plus a "search for" row under a host-like text that Enter would open as an address.
pub fn rows(typed: &str, history: &[HistoryEntry], bookmarks: &[BookmarkEntry], now_us: i64, cursor_at_end: bool, deleting: bool, engines: &Engines) -> Vec<Suggestion> {
    let cx = engines.context();
    let mut rows = suggest(&RankInput { typed, cursor_at_end, deleting, now_us, history, bookmarks, remote: &[], cx: &cx });
    if matches!(classify(typed, &cx), Action::Navigate { typed_scheme: false, .. }) && !rows.is_empty() {
        let terms = typed.trim().to_owned();
        let url = cx.default_engine.url_for(&terms);
        let row = Suggestion {
            kind: Kind::KeywordSearch,
            title: format!("{terms} \u{2014} search"),
            text: terms,
            url,
            score: 0,
            inline_completion: None,
            bookmarked: false,
        };
        rows.truncate(MAX_SUGGESTIONS - 1);
        rows.push(row);
    }
    rows
}

/// A key the dropdown takes while the address field has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Down,
    Up,
    Escape,
}

/// What a key did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// The field now holds this text (the highlighted row's, or what was typed).
    Text(String),
    /// The dropdown closes and the field goes back to the page's address.
    Revert,
}

/// The open dropdown: the rows, the highlighted one, and whether the person moved off the first.
#[derive(Debug)]
pub struct Dropdown {
    typed: String,
    rows: Vec<Suggestion>,
    selected: usize,
}

impl Dropdown {
    fn new(typed: &str, rows: Vec<Suggestion>) -> Option<Dropdown> {
        (!rows.is_empty()).then(|| Dropdown { typed: typed.to_owned(), rows, selected: 0 })
    }

    pub fn rows(&self) -> &[Suggestion] {
        &self.rows
    }

    /// The highlighted row; the first is Enter's own choice until an arrow key moves.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The text the person typed, without a completion or a row's text.
    pub fn typed(&self) -> &str {
        &self.typed
    }

    /// Down and Up move over the rows and stop at the ends; the field shows the row's text (the typed text on the first
    /// row). Escape closes.
    pub fn key(&mut self, key: Key) -> Step {
        match key {
            Key::Escape => return Step::Revert,
            Key::Down => self.selected = (self.selected + 1).min(self.rows.len() - 1),
            Key::Up => self.selected = self.selected.saturating_sub(1),
        }
        Step::Text(if self.selected == 0 { self.typed.clone() } else { self.rows[self.selected].text.clone() })
    }

    /// What Enter opens when the person moved to another row; None on the first row (Enter then reads the field).
    pub fn chosen(&self) -> Option<Go> {
        self.pick(self.selected).filter(|_| self.selected != 0)
    }

    /// What a click on row `i` opens.
    pub fn pick(&self, i: usize) -> Option<Go> {
        let row = self.rows.get(i)?;
        Some(Go { url: row.url.clone(), search: matches!(row.kind, Kind::RemoteSearch | Kind::KeywordSearch) })
    }

    /// The tail to put after the typed text, selected, once the history is in; only while the first row is highlighted.
    pub fn completion(&self) -> Option<&str> {
        if self.selected != 0 {
            return None;
        }
        self.rows.first().and_then(|row| row.inline_completion.as_deref()).filter(|tail| !tail.is_empty())
    }
}

/// The omnibox: the engines, the history the engine last answered, and the dropdown over them.
pub struct Omnibox {
    pub engines: Engines,
    pub dropdown: Option<Dropdown>,
    /// The id of the query the engine is to answer; an older answer is ignored.
    query: u64,
    history: Vec<HistoryEntry>,
    bookmarks: Vec<BookmarkEntry>,
    cursor_at_end: bool,
    deleting: bool,
}

impl Default for Omnibox {
    fn default() -> Omnibox {
        Omnibox { engines: Engines::default(), dropdown: None, query: 0, history: Vec::new(), bookmarks: Vec::new(), cursor_at_end: true, deleting: false }
    }
}

impl Omnibox {
    /// The person changed the text. Rows from the search and address rules show at once; the history comes with the
    /// engine's answer to the returned query. Blank text closes the dropdown.
    pub fn typed(&mut self, text: &str, deleting: bool, cursor_at_end: bool, now_us: i64) -> Option<Cmd> {
        self.history.clear();
        self.bookmarks.clear();
        self.cursor_at_end = cursor_at_end;
        self.deleting = deleting;
        self.dropdown = Dropdown::new(text, rows(text, &[], &[], now_us, cursor_at_end, deleting, &self.engines));
        if text.trim().is_empty() {
            self.dropdown = None;
            return None;
        }
        self.query += 1;
        Some(Cmd::HistoryQuery { id: self.query, text: text.to_owned() })
    }

    /// The engine answered a query. Rows are made again over the pages; the person's place among them is kept if they
    /// moved, and goes back to the first row if not.
    pub fn answered(&mut self, id: u64, entries: Vec<HistoryEntry>, now_us: i64) {
        if id != self.query {
            return;
        }
        self.history = entries;
        self.rerank(now_us);
    }

    /// The engine's other answer to the same query: the bookmarks that match. Same rules as [`Omnibox::answered`].
    pub fn bookmarks_answered(&mut self, id: u64, entries: Vec<BookmarkEntry>, now_us: i64) {
        if id != self.query {
            return;
        }
        self.bookmarks = entries;
        self.rerank(now_us);
    }

    fn rerank(&mut self, now_us: i64) {
        let Some(open) = &self.dropdown else { return };
        let selected = open.selected;
        let typed = open.typed.clone();
        let rows = rows(&typed, &self.history, &self.bookmarks, now_us, self.cursor_at_end, self.deleting, &self.engines);
        self.dropdown = Dropdown::new(&typed, rows).map(|mut d| {
            d.selected = if selected == 0 { 0 } else { selected.min(d.rows.len() - 1) };
            d
        });
    }

    /// The id of the query the engine is to answer now.
    pub fn latest_query(&self) -> u64 {
        self.query
    }

    pub fn close(&mut self) {
        self.dropdown = None;
        // An answer on its way is for text that is gone.
        self.query += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000_000;

    fn entry(url: &str, title: &str, visits: u32, typed: u32) -> HistoryEntry {
        HistoryEntry { url: url.into(), title: title.into(), visit_count: visits, typed_count: typed, last_visit_us: NOW }
    }

    fn engines() -> Engines {
        Engines::default()
    }

    #[test]
    fn the_address_bar_reads_urls_hosts_and_searches() {
        let url = |s| to_url(s).unwrap().to_string();
        assert_eq!(url("https://example.com/a"), "https://example.com/a");
        assert_eq!(url("example.com"), "https://example.com/");
        assert_eq!(url("lucina os"), "https://duckduckgo.com/html/?q=lucina+os");
        assert!(to_url("   ").is_none());
        // What the old reader did with the rest of its inputs.
        assert_eq!(url("about:blank"), "about:blank");
        assert_eq!(url("  https://example.com/x y  "), "https://example.com/x%20y");
        assert_eq!(url("rust"), "https://duckduckgo.com/html/?q=rust");
        assert_eq!(url("a&b c"), "https://duckduckgo.com/html/?q=a%26b+c");
    }

    #[test]
    fn enter_says_whether_the_text_became_a_search() {
        let go = |s| go_for(s, &engines()).unwrap();
        assert!(!go("example.com").search);
        assert!(!go("https://example.com/").search);
        assert!(go("lucina os").search);
        assert!(go("? example.com").search);
        assert!(go_for("", &engines()).is_none());
        let Cmd::LoadTyped { url, search } = go("lucina os").command() else { panic!() };
        assert_eq!((url.as_str(), search), ("https://duckduckgo.com/html/?q=lucina+os", true));
    }

    #[test]
    fn localhost_with_a_port_and_an_ip_literal_are_addresses_and_question_mark_terms_a_search() {
        let go = |s| go_for(s, &engines()).unwrap();
        assert_eq!((go("localhost:3000").url.as_str(), go("localhost:3000").search), ("http://localhost:3000/", false));
        assert_eq!((go("192.168.1.1").url.as_str(), go("192.168.1.1").search), ("http://192.168.1.1/", false));
        assert_eq!((go("127.0.0.1:8080/a").url.as_str(), go("127.0.0.1:8080/a").search), ("http://127.0.0.1:8080/a", false));
        let q = go("?terms here");
        assert_eq!((q.url.as_str(), q.search), ("https://duckduckgo.com/html/?q=terms+here", true));
    }

    #[test]
    fn a_search_gets_the_search_row_first_and_history_below() {
        let history = [entry("https://rust-lang.org/", "Rust Programming Language", 5, 2)];
        let rows = rows("rust", &history, &[], NOW, true, false, &engines());
        assert_eq!(rows[0].kind, Kind::Heuristic);
        assert!(rows[0].url.as_str().starts_with("https://duckduckgo.com/html/?q=rust"));
        assert!(rows.iter().any(|r| r.kind == Kind::History && r.url.as_str() == "https://rust-lang.org/"));
        assert!(rows.len() <= MAX_SUGGESTIONS);
    }

    #[test]
    fn a_host_like_text_also_offers_to_search_for_it() {
        let rows = rows("example.com", &[], &[], NOW, true, false, &engines());
        assert_eq!(rows[0].url.as_str(), "https://example.com/");
        let last = rows.last().unwrap();
        assert_eq!((last.kind, last.title.as_str()), (Kind::KeywordSearch, "example.com \u{2014} search"));
        assert_eq!(last.url.as_str(), "https://duckduckgo.com/html/?q=example.com");
        // A typed scheme is an address and nothing else; blank text has no rows.
        assert_eq!(super::rows("https://example.com/", &[], &[], NOW, true, false, &engines()).len(), 1);
        assert!(super::rows("  ", &[], &[], NOW, true, false, &engines()).is_empty());
    }

    #[test]
    fn the_search_row_never_pushes_the_list_past_its_limit() {
        let history: Vec<_> = (0..20).map(|i| entry(&format!("https://example.com/{i}"), &format!("example page {i}"), 3, 1)).collect();
        let rows = rows("example", &history, &[], NOW, true, false, &engines());
        assert!(rows.len() <= MAX_SUGGESTIONS, "{}", rows.len());
    }

    fn bookmark(url: &str, title: &str) -> BookmarkEntry {
        BookmarkEntry { url: url.into(), title: title.into(), folder_path: "Other Bookmarks".into() }
    }

    #[test]
    fn a_bookmark_that_is_not_in_the_history_is_a_row_and_a_visited_one_is_starred() {
        let marks = [bookmark("https://news.example.org/", "World News"), bookmark("https://rust-lang.org/", "Rust")];
        let history = [entry("https://rust-lang.org/", "Rust Programming Language", 5, 2)];
        let all = rows("news", &[], &marks, NOW, true, false, &engines());
        assert!(all.iter().any(|r| r.kind == Kind::Bookmark && r.url.as_str() == "https://news.example.org/"), "{all:?}");
        let both = rows("rust", &history, &marks, NOW, true, false, &engines());
        let rust = both.iter().find(|r| r.url.as_str() == "https://rust-lang.org/").unwrap();
        assert_eq!(rust.kind, Kind::History);
        assert!(rust.bookmarked, "the visited page that is also bookmarked says so");
        assert!(rows("news", &[], &[], NOW, true, false, &engines()).iter().all(|r| r.kind != Kind::Bookmark));
    }

    #[test]
    fn the_two_answers_to_a_query_come_in_either_order_and_each_counts_only_for_its_own_query() {
        let marks = [bookmark("https://news.example.org/", "World News")];
        let history = [entry("https://news.example.com/", "News", 5, 2)];
        let rows_of = |omni: &Omnibox| omni.dropdown.as_ref().unwrap().rows().iter().map(|r| (r.kind, r.url.to_string())).collect::<Vec<_>>();
        let mut omni = Omnibox::default();
        let Some(Cmd::HistoryQuery { id, .. }) = omni.typed("news", false, true, NOW) else { panic!() };
        omni.bookmarks_answered(id, marks.to_vec(), NOW);
        omni.answered(id, history.to_vec(), NOW);
        let one = rows_of(&omni);
        assert!(one.contains(&(Kind::Bookmark, "https://news.example.org/".into())) && one.iter().any(|r| r.0 == Kind::History), "{one:?}");
        let mut other = Omnibox::default();
        let Some(Cmd::HistoryQuery { id, .. }) = other.typed("news", false, true, NOW) else { panic!() };
        other.answered(id, history.to_vec(), NOW);
        other.bookmarks_answered(id, marks.to_vec(), NOW);
        assert_eq!(rows_of(&other), one, "the order of the answers does not matter");
        // Typing on drops the old bookmarks: they were for other text. A stale or late answer is ignored.
        let Some(Cmd::HistoryQuery { id: next, .. }) = omni.typed("newsx", false, true, NOW) else { panic!() };
        assert!(omni.dropdown.as_ref().unwrap().rows().iter().all(|r| r.kind != Kind::Bookmark));
        omni.bookmarks_answered(id, marks.to_vec(), NOW);
        assert!(omni.dropdown.as_ref().unwrap().rows().iter().all(|r| r.kind != Kind::Bookmark));
        assert_ne!(id, next);
        omni.close();
        omni.bookmarks_answered(next, marks.to_vec(), NOW);
        assert!(omni.dropdown.is_none());
    }

    #[test]
    fn a_first_row_may_complete_the_text() {
        let history = [entry("https://example.com/", "Example", 10, 5)];
        let mut omni = Omnibox::default();
        let query = omni.typed("exam", false, true, NOW).unwrap();
        let Cmd::HistoryQuery { id, text } = query else { panic!("{query:?}") };
        assert_eq!(text, "exam");
        assert_eq!(omni.dropdown.as_ref().unwrap().completion(), None, "no history yet");
        omni.answered(id, history.to_vec(), NOW);
        assert_eq!(omni.dropdown.as_ref().unwrap().completion(), Some("ple.com/"));
        // Deleting or a caret in the middle: nothing is completed.
        let query = omni.typed("exam", true, true, NOW).unwrap();
        let Cmd::HistoryQuery { id, .. } = query else { panic!() };
        omni.answered(id, history.to_vec(), NOW);
        assert_eq!(omni.dropdown.as_ref().unwrap().completion(), None);
    }

    #[test]
    fn an_old_answer_or_one_after_the_dropdown_closed_is_ignored() {
        let history = [entry("https://example.com/", "Example", 10, 5)];
        let mut omni = Omnibox::default();
        let Some(Cmd::HistoryQuery { id: first, .. }) = omni.typed("exa", false, true, NOW) else { panic!() };
        let Some(Cmd::HistoryQuery { id: second, .. }) = omni.typed("exam", false, true, NOW) else { panic!() };
        assert_ne!(first, second);
        omni.answered(first, history.to_vec(), NOW);
        assert_eq!(omni.dropdown.as_ref().unwrap().typed(), "exam");
        assert!(omni.dropdown.as_ref().unwrap().rows().iter().all(|r| r.kind != Kind::History));
        omni.close();
        omni.answered(second, history.to_vec(), NOW);
        assert!(omni.dropdown.is_none());
        // Blank text closes and asks nothing.
        assert!(omni.typed("", false, true, NOW).is_none());
        assert!(omni.dropdown.is_none());
    }

    fn open(typed: &str) -> Dropdown {
        let history = [entry("https://rust-lang.org/", "Rust Programming Language", 5, 2), entry("https://rustup.rs/", "rustup", 3, 1)];
        let mut omni = Omnibox::default();
        let Some(Cmd::HistoryQuery { id, .. }) = omni.typed(typed, false, true, NOW) else { panic!() };
        omni.answered(id, history.to_vec(), NOW);
        omni.dropdown.take().unwrap()
    }

    #[test]
    fn the_arrows_move_over_the_rows_and_stop_at_the_ends() {
        let mut d = open("rust");
        let n = d.rows().len();
        assert!(n >= 3, "{n}");
        assert_eq!((d.selected(), d.chosen()), (0, None), "Enter on the first row reads the field");
        assert_eq!(d.key(Key::Up), Step::Text("rust".into()), "Up on the first row stays");
        assert_eq!(d.selected(), 0);
        let Step::Text(text) = d.key(Key::Down) else { panic!() };
        assert_eq!((d.selected(), text.as_str()), (1, d.rows()[1].text.as_str()));
        let go = d.chosen().unwrap();
        assert_eq!(go.url, d.rows()[1].url);
        assert!(!go.search);
        for _ in 0..20 {
            d.key(Key::Down);
        }
        assert_eq!(d.selected(), n - 1);
        // Back up to the first row puts the typed text back in the field.
        let mut last = Step::Revert;
        for _ in 0..n {
            last = d.key(Key::Up);
        }
        assert_eq!((d.selected(), last), (0, Step::Text("rust".into())));
        assert_eq!(d.key(Key::Escape), Step::Revert);
    }

    #[test]
    fn a_search_row_opens_as_a_search_and_a_click_picks_its_row() {
        let d = open("example.com");
        let last = d.rows().len() - 1;
        let go = d.pick(last).unwrap();
        assert!(go.search);
        assert_eq!(go.url.as_str(), "https://duckduckgo.com/html/?q=example.com");
        assert!(d.pick(last + 1).is_none());
        assert!(!d.pick(0).unwrap().search);
    }

    #[test]
    fn a_late_answer_keeps_the_row_the_person_moved_to() {
        let history = [entry("https://rust-lang.org/", "Rust", 5, 2)];
        let mut omni = Omnibox::default();
        let Some(Cmd::HistoryQuery { id, .. }) = omni.typed("rust", false, true, NOW) else { panic!() };
        omni.answered(id, history.to_vec(), NOW);
        omni.dropdown.as_mut().unwrap().key(Key::Down);
        let before = omni.dropdown.as_ref().unwrap().selected();
        omni.answered(id, history.to_vec(), NOW);
        assert_eq!(omni.dropdown.as_ref().unwrap().selected(), before);
    }

    /// A field whose glyphs are `a_char` wide each, so a box of `n` units shows `n / a_char` characters and the tests can
    /// say what fits in it in characters instead of in points.
    fn measure_each(a_char: f32) -> impl Fn(&str) -> f32 {
        move |s: &str| s.chars().count() as f32 * a_char
    }

    /// `https://news.bbc.co.uk/world/2026/08` displayed: the site 14 characters, the whole display 28 (`https://` dropped).
    const BBC: &str = "https://news.bbc.co.uk/world/2026/08";

    #[test]
    fn the_field_splits_the_display_at_the_emphasised_part() {
        for (address, domain, want) in [
            (BBC, Some("bbc.co.uk"), ("news.", "bbc.co.uk", "/world/2026/08")),
            // A `https://` is dropped, an `http://` is kept as the mark of an insecure page, and the port is not part of the
            // site: it is the rest's ink, glued to the path.
            ("https://accounts.google.com/v3/signin", Some("google.com"), ("accounts.", "google.com", "/v3/signin")),
            ("http://accounts.google.com:8080/v3/signin", Some("google.com"), ("http://accounts.", "google.com", ":8080/v3/signin")),
            // Nothing to emphasise inside the host: the whole host is the strong part, `www.` and all.
            ("https://me.github.io/path", Some("me.github.io"), ("", "me.github.io", "/path")),
            ("http://127.0.0.1:8080/", None, ("http://", "127.0.0.1", ":8080")),
            // No host at all, so the address is shown whole and nothing is emphasised.
            ("about:blank", None, ("about:blank", "", "")),
        ] {
            let display = field_display(address, domain, 4096.0, measure_each(1.0));
            assert_eq!(field_pieces(&display), want, "{address} with {domain:?}");
            assert!(!display.elided, "{address}: a box that wide elides nothing");
        }
    }

    #[test]
    fn what_does_not_fit_elides_from_the_front_of_the_path_not_the_site() {
        // 20 units, and the display wants 28: the tail goes and the site stays whole.
        let display = field_display(BBC, Some("bbc.co.uk"), 20.0, measure_each(1.0));
        assert!(display.elided, "{display:?}");
        assert_eq!(field_pieces(&display), ("news.", "bbc.co.uk", "/worl\u{2026}"));
        assert_eq!(display.text.chars().count(), 20, "the box is filled, not left short");

        // The address bar's own bug: a Google sign-in query of 200 characters. The field of a laptop window is a few
        // hundred points, which is nowhere near the whole address, and what must survive is `accounts.google.com`.
        let long = "https://accounts.google.com/v3/signin/identifier?continue=https%3A%2F%2Fwww.google.com%2F&followup=https%3A%2F%2Fwww.google.com%2F&hl=da&flowName=GlifWebSignIn&flow=entry";
        let display = field_display(long, Some("google.com"), 40.0, measure_each(1.0));
        assert!(display.elided, "{display:?}");
        let (before, strong, after) = field_pieces(&display);
        assert_eq!((before, strong), ("accounts.", "google.com"), "{display:?}");
        assert!(after.contains('\u{2026}'), "the query is what went: {after:?}");
        assert_eq!(display.text.chars().count(), 40, "a box of 40 single-unit glyphs holds 40 characters");
    }

    #[test]
    fn a_wider_field_shows_more_and_a_narrower_one_never_shows_the_site_in_pieces() {
        let at = |width: f32| field_display(BBC, Some("bbc.co.uk"), width, measure_each(1.0));
        assert_eq!(at(28.0).text, "news.bbc.co.uk/world/2026/08", "wide enough, the display is whole");
        assert!(!at(28.0).elided);
        assert_eq!(at(20.0).text.chars().count(), 20, "one unit less is one character less: {}", at(20.0).text);
        assert!(at(20.0).elided);
        // Narrower than the host: the subdomain is what goes, first with a marker in its place and then on its own.
        assert_eq!(at(14.0).text, "news.bbc.co.uk", "the host's own width is the floor: it is never cut");
        assert!(at(14.0).elided, "the tail is still missing, even with no room to say so");
        assert_eq!(at(10.0).text, "\u{2026}bbc.co.uk", "a host too wide for the field loses its front label, not its site");
        assert_eq!(field_pieces(&at(10.0)), ("\u{2026}", "bbc.co.uk", ""));
        assert_eq!(at(9.0).text, "bbc.co.uk", "and at the site's own width nothing is left of the rest");
    }

    /// A host the engine could not place (`None`, the answer for `localhost` and an IP) is shown with its whole host
    /// emphasised and its `www.` kept: dropping the prefix is something only a known registrable domain licenses.
    #[test]
    fn without_an_answer_from_the_engine_the_whole_host_is_the_site() {
        for (address, want) in [
            ("https://localhost:8080/x", ("", "localhost", ":8080/x")),
            ("http://127.0.0.1:8080/x", ("http://", "127.0.0.1", ":8080/x")),
            ("https://example.invalid/x", ("", "example.invalid", "/x")),
        ] {
            let display = field_display(address, None, 4096.0, measure_each(1.0));
            assert_eq!(field_pieces(&display), want, "{address}");
            assert!(!display.elided, "{address}");
        }
        let www = field_display("https://www.localhost/", None, 4096.0, measure_each(1.0));
        assert_eq!(www.text, "www.localhost", "the `www.` stays when nothing says what the site is: {www:?}");
        assert_eq!(field_pieces(&www), ("", "www.localhost", ""), "and a bare `/` is not shown");
    }

    /// The search is over what the caller's own glyphs measure, so wide glyphs buy fewer characters than narrow ones for
    /// the same address in the same box.
    #[test]
    fn the_fit_comes_from_the_callers_own_measurement() {
        let address = "https://example.com/a/path";
        let wide = field_display(address, Some("example.com"), 45.0, measure_each(3.0));
        let narrow = field_display(address, Some("example.com"), 45.0, measure_each(1.0));
        assert!(wide.elided && !narrow.elided, "{wide:?} against {narrow:?}");
        assert_eq!(narrow.text, "example.com/a/path", "an 18-character display fits 45 single-unit glyphs whole");
        assert_eq!(wide.text.chars().count(), 15, "at three units a character, 45 units hold 15: {}", wide.text);
    }

    /// A range from another display is not clipped to fit: the caller paired news it should not have, and what it gets is
    /// the drawing with nothing emphasised rather than a slice that panics.
    #[test]
    fn a_range_outside_the_text_emphasises_nothing() {
        let display = lucinanet_omnibox::display::unfocused_display("https://example.com/", Some("example.com"), 64);
        for bad in [display.text.len() + 4, 1 << 30] {
            let mut other = display.clone();
            other.domain = Some(bad..bad);
            assert_eq!(field_pieces(&other), (other.text.as_str(), "", ""));
        }
        // A range inside the text is trusted byte for byte, wherever it came from - and the text is the display's, not
        // the address: this one reads `example.com`, scheme and root path both gone.
        assert_eq!(display.text, "example.com");
        let mut split = display.clone();
        split.domain = Some(0..2);
        assert_eq!(field_pieces(&split), ("", "ex", "ample.com"), "the range is trusted as the display's own bytes");
    }
}
