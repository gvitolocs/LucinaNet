//! What the address bar decides, apart from drawing it: what Enter does with the text (`lucinanet-omnibox`'s
//! `classify`), which rows the dropdown shows and how the arrow keys move over them, and what the field shows of an address
//! while nobody is typing in it. All of it is plain data in and out, so it is tested without a window
//! (the omnibox/history wiring spec).

use lucinanet_omnibox::{
    Action, BookmarkEntry, Context, HistoryEntry, Kind, MAX_SUGGESTIONS, NoEngineReason, RankInput, Refusal, SearchEngine, Suggestion, classify, inline_origin, suggest,
};
use lucinanet_settings::Settings;
use url::Url;

use crate::Cmd;

/// The engines the bar searches with, read from the settings (`search.default_engine` and `search.engines`): this file
/// names no engine of its own. The shipped settings hold one engine as the initial preference, and a person can change
/// it. No engine (an empty list) means a typed search goes nowhere ([`refusal_for`]).
pub struct Engines {
    default_engine: Option<SearchEngine>,
    keywords: Vec<SearchEngine>,
    /// The engine Shift-Return searches with: the first engine on the list that is not the default one. `None` when the
    /// list holds only the default (then Shift-Return is Return).
    second: Option<SearchEngine>,
    suggest_remote: bool,
}

impl Default for Engines {
    /// The engines of the shipped settings, until a profile's settings are loaded.
    fn default() -> Engines {
        Engines::from_settings(&Settings::default())
    }
}

impl Engines {
    /// The engines `settings` names. The default engine is the one `search.default_engine` names, or the first one on
    /// the list; an empty list leaves none.
    pub fn from_settings(settings: &Settings) -> Engines {
        let engines = &settings.search.engines;
        let as_engine = |engine: &lucinanet_settings::Engine| SearchEngine {
            name: engine.name.clone(),
            keyword: engine.keyword.clone(),
            template: engine.template.clone(),
        };
        let default_engine = if engines.is_empty() { None } else { Some(as_engine(settings.default_engine())) };
        let keywords = engines.iter().filter(|engine| engine.keyword.is_some()).map(as_engine).collect();
        let default_id = &settings.default_engine().id;
        let second = engines.iter().find(|engine| engine.id != *default_id).filter(|_| default_engine.is_some()).map(as_engine);
        Engines { default_engine, keywords, second, suggest_remote: settings.search.suggest_remote }
    }

    /// The engine Shift-Return sends a search to, if there is one besides the default.
    pub fn second(&self) -> Option<&SearchEngine> {
        self.second.as_ref()
    }

    /// No engine: typed text that is not an address is refused.
    pub fn none() -> Engines {
        Engines { default_engine: None, keywords: Vec::new(), second: None, suggest_remote: false }
    }

    pub fn context(&self) -> Context<'_> {
        Context { default_engine: self.default_engine.as_ref(), keywords: &self.keywords, allow_single_label_hosts: false }
    }
}

/// What the bar says when Enter on `typed` has nothing it may do: a search with no engine to send it to, or text the bar
/// refuses outright (`javascript:`); `None` when Enter has something to do (or nothing to do).
pub fn refusal_for(typed: &str, engines: &Engines) -> Option<&'static str> {
    match classify(typed, &engines.context()) {
        Action::NoEngine { reason: NoEngineReason::Missing, .. } => Some("No search engine is set: choose one in settings, or type an address"),
        Action::NoEngine { reason: NoEngineReason::Unusable, .. } => {
            Some("The search engine's template is not usable: fix it in settings, or type an address")
        }
        Action::Refused { reason: Refusal::Javascript } => Some("javascript: addresses are not opened from the bar"),
        _ => None,
    }
}

/// The hint of an empty field while Enter on a term has an engine to send it to. The words the bar has always shown: with
/// a usable engine the hint says nothing different, so it changes nothing seen today.
pub const PLACEHOLDER_SEARCH: &str = "Search or enter an address";
/// The hint of an empty field when Enter on a term would be refused for want of an engine ([`refusal_for`]'s `NoEngine`):
/// an address is still something the bar can do, a search is not, and the hint must not promise one.
pub const PLACEHOLDER_ADDRESS_ONLY: &str = "Enter an address";

/// The hint an empty address field shows, from the engines the omnibox holds. Both chromes read this (the classic field's
/// `TextEdit` and the glass frame's [`crate::glass::State`]), so the one string is the one rule and a field never says
/// more than the bar can do.
///
/// A default engine whose template cannot be filled counts as none, exactly as [`refusal_for`] counts it: `"a b"` is the
/// shortest text Enter makes a search of, and an engine that cannot turn it into an address cannot search at all.
pub fn placeholder_for(engines: &Engines) -> &'static str {
    let can_search = engines.context().default_engine.is_some_and(|engine| engine.url_for("a b").is_some());
    if can_search { PLACEHOLDER_SEARCH } else { PLACEHOLDER_ADDRESS_ONLY }
}

/// How long a refusal's shake and red flash last. A fixed short time: there is no reduce-motion switch in the tree yet.
pub const SHAKE_FOR: std::time::Duration = std::time::Duration::from_millis(420);
/// The widest the field moves sideways in a shake, in points.
pub const SHAKE_AMPLITUDE: f32 = 6.0;
/// How many back-and-forth swings fit in [`SHAKE_FOR`].
const SHAKE_SWINGS: f32 = 3.0;

/// How far, in points, the field is pushed sideways `elapsed` after a refusal: a decaying swing, zero at the start and
/// from [`SHAKE_FOR`] on, never wider than [`SHAKE_AMPLITUDE`]. A pure function of the time, so it needs no window.
pub fn shake_offset(elapsed: std::time::Duration) -> f32 {
    if elapsed >= SHAKE_FOR {
        return 0.0;
    }
    let t = elapsed.as_secs_f32() / SHAKE_FOR.as_secs_f32();
    SHAKE_AMPLITUDE * (1.0 - t) * (t * SHAKE_SWINGS * std::f32::consts::TAU).sin()
}

/// How red the field's edge is `elapsed` after a refusal, 1.0 at the start fading to 0.0 at [`SHAKE_FOR`] and after.
pub fn shake_flash(elapsed: std::time::Duration) -> f32 {
    if elapsed >= SHAKE_FOR {
        return 0.0;
    }
    (1.0 - elapsed.as_secs_f32() / SHAKE_FOR.as_secs_f32()).clamp(0.0, 1.0)
}

/// The page a window starts on when no address is given: the blank page, which loads without any network request. A
/// fresh profile makes no request until the person acts.
pub fn start_url(first: Option<&str>) -> Url {
    first.and_then(to_url).unwrap_or_else(blank_url)
}

/// `about:blank`.
pub fn blank_url() -> Url {
    Url::parse("about:blank").expect("a valid URL")
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
        Action::Empty | Action::NoEngine { .. } | Action::Refused { .. } => None,
        Action::Navigate { url, .. } => Some(Go { url, search: false }),
        Action::Search { url, .. } | Action::Keyword { url, .. } => Some(Go { url, search: true }),
    }
}

/// What Shift-Return does with `typed`, the exact text the person typed. A refusal (`javascript:`, no usable engine) is the
/// same as Return's, and an address is opened as Return opens it. Only text that Return would search for (or send to a
/// keyword) goes, verbatim, to the second engine: no row, no completion, no keyword rewrite. With no second engine this
/// is exactly [`go_for`].
pub fn go_shift_for(typed: &str, engines: &Engines) -> Option<Go> {
    let Some(second) = engines.second() else { return go_for(typed, engines) };
    match classify(typed, &engines.context()) {
        Action::Search { .. } | Action::Keyword { .. } => second.url_for(typed.trim()).map(|url| Go { url, search: true }),
        _ => go_for(typed, engines),
    }
}

/// What the bar says when Shift-Return on `typed` has nothing it may do: Return's refusal, or the second engine's template
/// not being usable.
pub fn refusal_shift_for(typed: &str, engines: &Engines) -> Option<&'static str> {
    if let Some(why) = refusal_for(typed, engines) {
        return Some(why);
    }
    let second = engines.second()?;
    let search = matches!(classify(typed, &engines.context()), Action::Search { .. } | Action::Keyword { .. });
    (search && second.url_for(typed.trim()).is_none()).then_some("The second search engine's template is not usable: fix it in settings")
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
/// becomes `…`, and what is left is what the person reads to know where they are (the omnibox/history wiring spec,
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
    rows_with_remote(typed, history, bookmarks, &[], now_us, cursor_at_end, deleting, engines)
}

/// [`rows`] with the suggestions a remote source answered. They are used only when `search.suggest_remote` is on, and
/// there is no remote-suggestion request in this build: [`rows`] hands in none, so nothing asks a search engine while the
/// person types.
#[allow(clippy::too_many_arguments)]
pub fn rows_with_remote(typed: &str, history: &[HistoryEntry], bookmarks: &[BookmarkEntry], remote: &[String], now_us: i64, cursor_at_end: bool, deleting: bool, engines: &Engines) -> Vec<Suggestion> {
    let cx = engines.context();
    let remote = if engines.suggest_remote { remote } else { &[] };
    let mut rows = suggest(&RankInput { typed, cursor_at_end, deleting, now_us, history, bookmarks, remote, cx: &cx });
    if matches!(classify(typed, &cx), Action::Navigate { typed_scheme: false, .. }) && !rows.is_empty() {
        let terms = typed.trim().to_owned();
        // The search row goes to the default engine; with none there is no such row.
        let Some(url) = cx.default_engine.and_then(|engine| engine.url_for(&terms)) else { return rows };
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

    /// What Enter opens when `field` is the typed text with its inline completion after it, the completion being the first
    /// row's (the dropdown is on its first row): the completed origin as the history or bookmarks stored it, over the scheme
    /// they used. `None` when the field is anything else (a search, a keyword, a completion the person deleted, text typed on
    /// since), when nothing is stored for it, or when the first row is not the highlighted one; Enter then reads the field.
    pub fn completed_go(&self, field: &str, now_us: i64) -> Option<Go> {
        let dropdown = self.dropdown.as_ref().filter(|d| d.selected == 0)?;
        let tail = dropdown.completion()?;
        let cx = self.engines.context();
        let target = inline_origin(&RankInput {
            typed: &dropdown.typed,
            cursor_at_end: self.cursor_at_end,
            deleting: self.deleting,
            now_us,
            history: &self.history,
            bookmarks: &self.bookmarks,
            remote: &[],
            cx: &cx,
        })?;
        if target.tail != tail || format!("{}{}", dropdown.typed, target.tail) != field {
            return None;
        }
        Some(Go { url: Url::parse(&target.url).ok()?, search: false })
    }

    /// The text Shift-Return searches for when the field holds `field`: what the person typed, without an inline completion
    /// and without the text of a row the arrows moved to (the dropdown keeps the typed text apart from both).
    pub fn literal<'a>(&'a self, field: &'a str) -> &'a str {
        self.dropdown.as_ref().map_or(field, Dropdown::typed)
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

/// No request leaves the machine that the person did not cause: a fresh profile starts on a blank page, typing
/// asks nobody anything, and only Enter on typed text builds a search request, to the engine the settings name.
#[cfg(test)]
mod no_unprompted_requests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000_000;

    #[test]
    fn with_no_address_the_window_starts_on_the_blank_page() {
        assert_eq!(start_url(None).as_str(), "about:blank");
        assert_eq!(start_url(Some("")).as_str(), "about:blank");
        assert_eq!(start_url(Some("   ")).as_str(), "about:blank");
        assert_eq!(blank_url().as_str(), "about:blank");
        // An address the person gave is still what opens.
        assert_eq!(start_url(Some("example.com")).as_str(), "https://example.com/");
    }

    #[test]
    fn default_settings_do_not_ask_the_engine_while_typing() {
        let settings = Settings::default();
        assert!(!settings.search.suggest_remote, "remote suggestions are off until the person turns them on");
        let mut omni = Omnibox::default();
        for typed in ["r", "ru", "rus", "rust", "rust l"] {
            // Typing asks the local history and nothing else.
            assert!(matches!(omni.typed(typed, false, true, NOW), Some(Cmd::HistoryQuery { .. })), "{typed}");
            let dropdown = omni.dropdown.as_ref().expect("rows");
            assert!(dropdown.rows().iter().all(|row| row.kind != Kind::RemoteSearch), "{typed}");
            // The search row is only a row: nothing is loaded until Enter.
        }
    }

    #[test]
    fn a_search_request_is_the_settings_engine_and_only_on_commit() {
        let settings = Settings::default();
        let template = &settings.default_engine().template;
        let want = Url::parse(&template.replace("{searchTerms}", "rust")).unwrap();
        let engines = Engines::from_settings(&settings);
        let go = go_for("rust", &engines).expect("Enter on text is a search");
        assert_eq!((go.url.clone(), go.search), (want, true));
        // The default engines are the shipped settings' engines.
        assert_eq!(go_for("rust", &Engines::default()).unwrap().url, go.url);
    }

    #[test]
    fn the_engine_follows_the_settings() {
        let mut settings = Settings::default();
        settings.search.engines[0].template = "https://search.test/?q={searchTerms}".to_owned();
        let go = go_for("a b", &Engines::from_settings(&settings)).unwrap();
        assert_eq!(go.url.as_str(), "https://search.test/?q=a+b");
    }

    /// `search.suggest_remote` decides whether remote suggestions are ever rows: the same list of remote answers shows
    /// nothing with the flag off and rows with it on.
    #[test]
    fn suggest_remote_flag_decides_remote_rows() {
        let remote = vec!["rust lang".to_owned(), "rustup".to_owned()];
        let mut settings = Settings::default();
        settings.search.suggest_remote = false;
        let off = rows_with_remote("rust", &[], &[], &remote, NOW, true, false, &Engines::from_settings(&settings));
        assert!(off.iter().all(|row| row.kind != Kind::RemoteSearch), "flag off: {off:?}");
        settings.search.suggest_remote = true;
        let on = rows_with_remote("rust", &[], &[], &remote, NOW, true, false, &Engines::from_settings(&settings));
        assert!(on.iter().any(|row| row.kind == Kind::RemoteSearch), "flag on: {on:?}");
        // With no remote source (nothing is wired in this build) the flag alone adds nothing.
        let none = rows_with_remote("rust", &[], &[], &[], NOW, true, false, &Engines::from_settings(&settings));
        assert!(none.iter().all(|row| row.kind != Kind::RemoteSearch));
    }

    /// An engine whose template cannot be filled is told apart from no engine at all: the refusal names the template.
    #[test]
    fn a_refusal_tells_a_missing_engine_from_an_unusable_template() {
        let mut broken = Settings::default();
        broken.search.engines[0].template = "not a url".to_owned();
        let unusable = refusal_for("a b", &Engines::from_settings(&broken)).expect("refused");
        assert!(unusable.contains("template"), "{unusable}");
        let missing = refusal_for("a b", &Engines::none()).expect("refused");
        assert!(!missing.contains("template"), "{missing}");
        assert_ne!(unusable, missing);
    }

    #[test]
    fn with_no_engine_a_typed_search_goes_nowhere() {
        let mut settings = Settings::default();
        settings.search.engines.clear();
        for engines in [Engines::none(), Engines::from_settings(&settings)] {
            assert!(go_for("rust lang", &engines).is_none());
            assert!(go_for("rust", &engines).is_none());
            assert!(refusal_for("rust lang", &engines).is_some());
            // An address, and blank text, are not refused.
            assert!(go_for("example.com", &engines).is_some());
            assert!(refusal_for("example.com", &engines).is_none());
            assert!(refusal_for("   ", &engines).is_none());
            // The dropdown holds no search row either.
            assert!(rows("rust lang", &[], &[], NOW, true, false, &engines).is_empty());
        }
    }

    /// Every non-test source file of the crates that can build a request: the bar and window (`lucinanet-ui`), the
    /// classifier and ranker (`lucinanet-omnibox`) and the engine (`lucinanet-servo`). Found on disk, not listed by hand,
    /// so a new file is covered without anyone remembering to add it.
    fn production_files() -> Vec<(String, std::path::PathBuf)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())).map(|e| e.unwrap().path()).collect();
            entries.sort();
            for path in entries {
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    out.push(path);
                }
            }
        }
        let ui = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        for krate in ["lucinanet-ui", "lucinanet-omnibox", "lucinanet-servo"] {
            let mut found = Vec::new();
            walk(&ui.join("..").join(krate).join("src"), &mut found);
            files.extend(found.into_iter().map(|path| (krate.to_owned(), path)));
        }
        files
    }

    /// `source` without its `#[cfg(test)]` items and without comment lines, so test data and prose are not counted. An
    /// item ends at the first line that is its own indent plus `}` (rustfmt's closing brace), or at a line ending in `;`
    /// when it has no body. Panics when an item never ends, which is how a form this reader does not understand shows up
    /// instead of passing silently.
    fn production_code(name: &str, source: &str) -> String {
        let mut out = String::new();
        // The closing line the skipped item ends at, once its first line is seen.
        let mut closing: Option<String> = None;
        let mut indent = String::new();
        let mut after_attr = false;
        let mut opened = false;
        for line in source.lines() {
            let trimmed = line.trim();
            if closing.is_none() && !after_attr && trimmed == "#[cfg(test)]" {
                after_attr = true;
                indent = line[..line.len() - line.trim_start().len()].to_owned();
                continue;
            }
            if after_attr {
                after_attr = false;
                closing = Some(format!("{indent}}}"));
                opened = false;
            }
            if let Some(end) = &closing {
                opened |= trimmed.ends_with('{');
                if (opened && line == end) || (!opened && trimmed.ends_with(';')) {
                    closing = None;
                }
                continue;
            }
            if trimmed.starts_with("//") {
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        assert!(closing.is_none() && !after_attr, "{name}: a #[cfg(test)] item was not closed");
        out
    }

    #[test]
    fn the_scan_reads_real_files_and_sees_the_test_markers() {
        let files = production_files();
        assert!(files.len() >= 20, "the scan found only {} files", files.len());
        for krate in ["lucinanet-ui", "lucinanet-omnibox", "lucinanet-servo"] {
            assert!(files.iter().any(|(k, _)| k == krate), "no file of {krate}");
        }
        let mut with_tests = 0;
        for (_, path) in &files {
            assert!(path.exists(), "{}", path.display());
            let source = std::fs::read_to_string(path).unwrap();
            let code = production_code(&path.display().to_string(), &source);
            if source.contains("#[cfg(test)]") {
                with_tests += 1;
                assert_ne!(code, source, "{} has a test marker the scan did not strip", path.display());
            }
        }
        assert!(with_tests >= 5, "only {with_tests} files had tests to strip: the marker form changed?");
    }

    #[test]
    fn no_search_engine_is_named_in_the_code_that_builds_a_request() {
        // The settings schema is the one place that names an engine, and it is not among these crates.
        const HOSTS: [&str; 8] = ["duckduckgo", "google.", "bing.com", "yahoo.", "ecosia", "startpage", "yandex", "brave.com/search"];
        for (_, path) in production_files() {
            let name = path.display().to_string();
            let code = production_code(&name, &std::fs::read_to_string(&path).unwrap()).to_lowercase();
            for host in HOSTS {
                assert!(!code.contains(host), "{name} names a search engine ({host}) outside the settings");
            }
            // The start page is no real host either.
            assert!(!code.contains("https://example.com"), "{name} falls back to a real host");
        }
    }

    #[test]
    fn javascript_in_the_bar_is_refused_with_a_note_and_goes_nowhere() {
        for engines in [Engines::default(), Engines::none()] {
            for typed in ["javascript:alert(1)", "  JAVASCRIPT:void(0)", "view-source:javascript:x"] {
                assert!(go_for(typed, &engines).is_none(), "{typed}");
                let why = refusal_for(typed, &engines).unwrap_or_else(|| panic!("{typed} is refused"));
                assert!(why.contains("javascript:") && why.contains("not opened"), "{why}");
                assert_eq!(to_url(typed), None, "{typed}");
            }
        }
        // The no-engine note is still its own.
        assert!(!refusal_for("a b", &Engines::none()).unwrap().contains("javascript"));
        // Ordinary words are still a search with the shipped engine, not a refusal.
        assert!(refusal_for("javascript tutorial", &Engines::default()).is_none());
        assert!(go_for("javascript tutorial", &Engines::default()).is_some_and(|go| go.search));
    }

    #[test]
    fn the_all_zeros_literal_opens_localhost_from_the_bar() {
        for (typed, expected) in [("0.0.0.0:8080/x", "http://localhost:8080/x"), ("[::]", "http://localhost/"), ("::", "http://localhost/")] {
            let go = go_for(typed, &Engines::default()).unwrap();
            assert_eq!((go.url.as_str(), go.search), (expected, false), "{typed}");
        }
    }

    #[test]
    fn the_shake_is_zero_outside_its_time_bounded_and_repeatable() {
        use std::time::Duration;
        let ms = Duration::from_millis;
        assert_eq!(shake_offset(ms(0)), 0.0);
        assert_eq!(shake_offset(SHAKE_FOR), 0.0);
        assert_eq!(shake_offset(SHAKE_FOR + ms(1)), 0.0);
        assert_eq!(shake_offset(Duration::from_secs(60)), 0.0);
        let mut widest = 0.0f32;
        for t in 0..=SHAKE_FOR.as_millis() as u64 {
            let x = shake_offset(ms(t));
            assert!(x.abs() <= SHAKE_AMPLITUDE, "{t} ms: {x}");
            assert_eq!(x.to_bits(), shake_offset(ms(t)).to_bits(), "deterministic at {t} ms");
            widest = widest.max(x.abs());
        }
        assert!(widest > SHAKE_AMPLITUDE / 3.0, "it moves: {widest}");
        // It swings both ways.
        let xs: Vec<f32> = (0..SHAKE_FOR.as_millis() as u64).map(|t| shake_offset(ms(t))).collect();
        assert!(xs.iter().any(|x| *x > 0.5) && xs.iter().any(|x| *x < -0.5));
    }

    #[test]
    fn the_flash_starts_full_and_fades_to_nothing() {
        use std::time::Duration;
        assert_eq!(shake_flash(Duration::ZERO), 1.0);
        assert_eq!(shake_flash(SHAKE_FOR), 0.0);
        assert_eq!(shake_flash(Duration::from_secs(9)), 0.0);
        let mut last = 1.0f32;
        for t in 0..=SHAKE_FOR.as_millis() as u64 {
            let f = shake_flash(Duration::from_millis(t));
            assert!((0.0..=1.0).contains(&f) && f <= last, "{t} ms: {f} after {last}");
            last = f;
        }
    }
}

/// Shift-Return sends the exact typed text to the second engine; everything else about Return stays.
#[cfg(test)]
mod shift_return {
    use super::*;

    const NOW: i64 = 1_800_000_000_000_000;

    fn two_engines() -> Engines {
        let mut settings = Settings::default();
        settings.search.engines.push(lucinanet_settings::Engine {
            id: "second".to_owned(),
            name: "Second".to_owned(),
            keyword: Some("sec".to_owned()),
            template: "https://second.test/?q={searchTerms}".to_owned(),
            suggest_url: None,
        });
        Engines::from_settings(&settings)
    }

    #[test]
    fn shift_enter_uses_the_other_engine_with_raw_text() {
        let engines = two_engines();
        let go = go_shift_for("foo bar", &engines).expect("a search");
        assert_eq!((go.url.as_str(), go.search), ("https://second.test/?q=foo+bar", true));
        // Return of the same text still goes to the default engine.
        assert_eq!(go_for("foo bar", &engines).unwrap().url.as_str(), "https://duckduckgo.com/html/?q=foo+bar");
        // A keyword is not rewritten: the whole text, keyword included, is the search.
        let go = go_shift_for("ddg cats", &engines).unwrap();
        assert_eq!(go.url.as_str(), "https://second.test/?q=ddg+cats");
        assert_eq!(go_shift_for("sec cats", &engines).unwrap().url.as_str(), "https://second.test/?q=sec+cats");
    }

    #[test]
    fn shift_enter_with_one_engine_is_plain_enter() {
        let engines = Engines::default();
        assert!(engines.second().is_none());
        for typed in ["foo bar", "ddg cats", "example.com", "https://example.org/x", "javascript:alert(1)", "", "  "] {
            assert_eq!(go_shift_for(typed, &engines), go_for(typed, &engines), "{typed:?}");
            assert_eq!(refusal_shift_for(typed, &engines), refusal_for(typed, &engines), "{typed:?}");
        }
        assert!(Engines::none().second().is_none());
        assert!(go_shift_for("foo bar", &Engines::none()).is_none());
        assert!(refusal_shift_for("foo bar", &Engines::none()).unwrap().contains("No search engine"));
    }

    #[test]
    fn the_second_engine_is_the_first_one_that_is_not_the_default() {
        let mut settings = Settings::default();
        let mut add = |id: &str| {
            settings.search.engines.push(lucinanet_settings::Engine {
                id: id.to_owned(),
                name: id.to_owned(),
                keyword: None,
                template: format!("https://{id}.test/?q={{searchTerms}}"),
                suggest_url: None,
            });
        };
        add("b");
        add("c");
        assert_eq!(Engines::from_settings(&settings).second().unwrap().name, "b");
        // The default moved to the second entry: the first entry is now the other one.
        settings.search.default_engine = "b".to_owned();
        assert_eq!(Engines::from_settings(&settings).second().unwrap().name, "DuckDuckGo");
    }

    #[test]
    fn shift_enter_on_an_address_navigates_like_enter() {
        let engines = two_engines();
        for typed in ["example.com", "https://example.org/a?b=c", "localhost:8080"] {
            assert_eq!(go_shift_for(typed, &engines), go_for(typed, &engines), "{typed}");
            let go = go_shift_for(typed, &engines);
            assert!(go.is_some_and(|go| !go.search && go.url.host_str() != Some("second.test")), "{typed}");
        }
    }

    #[test]
    fn shift_enter_ignores_the_inline_completion_and_the_selected_row() {
        let engines = two_engines();
        // The field holds "exam" plus an inline completion; Shift-Return searches for "exam" only.
        let mut omni = Omnibox { engines: two_engines(), ..Omnibox::default() };
        omni.typed("exam", false, true, NOW);
        let entry = |url: &str| HistoryEntry { url: url.into(), title: "Example".into(), visit_count: 5, typed_count: 5, last_visit_us: NOW - 1_000_000 };
        let id = omni.latest_query();
        omni.answered(id, vec![entry("https://example.com/"), entry("https://exam.org/page")], NOW);
        assert!(omni.dropdown.as_ref().unwrap().rows().len() >= 2);
        assert_eq!(omni.literal("example.com/"), "exam");
        assert_eq!(go_shift_for(omni.literal("example.com/"), &engines).unwrap().url.as_str(), "https://second.test/?q=exam");
        // With no dropdown the field is the text.
        assert_eq!(Omnibox::default().literal("raw text"), "raw text");
        // An arrow moved to a row and the field shows that row's text: still the typed text.
        omni.dropdown.as_mut().unwrap().key(Key::Down);
        assert_eq!(omni.literal("https://example.com/"), "exam");
    }

    #[test]
    fn shift_enter_of_javascript_is_refused() {
        let engines = two_engines();
        assert!(go_shift_for("javascript:alert(1)", &engines).is_none());
        assert_eq!(refusal_shift_for("javascript:alert(1)", &engines), refusal_for("javascript:alert(1)", &engines));
        assert!(refusal_shift_for("javascript:alert(1)", &engines).unwrap().contains("javascript:"));
        // Blank text does nothing, as Return.
        assert!(go_shift_for("  ", &engines).is_none() && refusal_shift_for("  ", &engines).is_none());
    }

    #[test]
    fn plain_enter_is_unchanged_by_a_second_engine() {
        let engines = two_engines();
        assert_eq!(go_for("foo bar", &engines).unwrap().url.as_str(), "https://duckduckgo.com/html/?q=foo+bar");
        assert_eq!(go_for("sec cats", &engines).unwrap().url.as_str(), "https://second.test/?q=cats", "a keyword still rewrites on Return");
        assert_eq!(go_for("example.com", &engines).unwrap().url.as_str(), "https://example.com/");
    }

    #[test]
    fn an_unusable_second_template_is_said_not_sent() {
        let mut settings = Settings::default();
        settings.search.engines.push(lucinanet_settings::Engine {
            id: "bad".to_owned(),
            name: "Bad".to_owned(),
            keyword: None,
            template: "not a url {searchTerms}".to_owned(),
            suggest_url: None,
        });
        let engines = Engines::from_settings(&settings);
        assert!(go_shift_for("a b", &engines).is_none());
        assert!(refusal_shift_for("a b", &engines).unwrap().contains("second search engine"));
    }
}

/// the hint of an empty field says what the engines can really do. Pure and windowless, like the rest of this file.
#[cfg(test)]
mod placeholder_hint {
    use super::*;

    /// The shipped settings, with the first engine's template replaced by one that cannot be filled.
    fn broken() -> Engines {
        let mut settings = Settings::default();
        settings.search.engines[0].template = "not a url".to_owned();
        Engines::from_settings(&settings)
    }

    /// The shipped settings with no engine on the list at all.
    fn empty_list() -> Engines {
        let mut settings = Settings::default();
        settings.search.engines.clear();
        Engines::from_settings(&settings)
    }

    #[test]
    fn a_usable_default_engine_keeps_the_hint_that_says_search() {
        // The words the bar has always shown, unchanged: with an engine nothing visible moves.
        assert_eq!(placeholder_for(&Engines::default()), "Search or enter an address");
        assert_eq!(placeholder_for(&Engines::from_settings(&Settings::default())), "Search or enter an address");
        // A second engine changes nothing about the hint; the default is what Enter searches with.
        let mut two = Settings::default();
        two.search.engines.push(lucinanet_settings::Engine {
            id: "second".to_owned(),
            name: "Second".to_owned(),
            keyword: Some("sec".to_owned()),
            template: "https://second.test/?q={searchTerms}".to_owned(),
            suggest_url: None,
        });
        assert_eq!(placeholder_for(&Engines::from_settings(&two)), PLACEHOLDER_SEARCH);
    }

    #[test]
    fn with_no_engine_the_hint_promises_no_search() {
        let hint = placeholder_for(&Engines::none());
        assert!(!hint.contains("Search"), "{hint}");
        assert_eq!(hint, "Enter an address");
        // The same answer through the settings: an empty engine list is no engine.
        assert_eq!(placeholder_for(&empty_list()), PLACEHOLDER_ADDRESS_ONLY);
    }

    #[test]
    fn an_unusable_template_promises_no_search_either() {
        let hint = placeholder_for(&broken());
        assert!(!hint.contains("Search"), "{hint}");
        assert_eq!(hint, "Enter an address", "a template that cannot be filled is no engine for the hint");
    }

    /// The hint and Enter's refusal are one rule about one engine: whatever text Enter would be refused for, the field
    /// never promised a search for.
    #[test]
    fn the_hint_agrees_with_what_enter_would_refuse() {
        for (name, engines) in [
            ("shipped", Engines::default()),
            ("none", Engines::none()),
            ("empty list", empty_list()),
            ("broken template", broken()),
        ] {
            let promises_search = placeholder_for(&engines) == PLACEHOLDER_SEARCH;
            assert_eq!(promises_search, refusal_for("a b", &engines).is_none(), "{name}: the hint and the refusal disagree");
        }
    }
}
