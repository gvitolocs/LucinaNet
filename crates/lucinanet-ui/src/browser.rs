//! The browser's window: a bar in the shell's style over the page. It draws what it knows and sends the
//! rest to the engine; nothing in `view` waits for a page.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lucina_app::keys::{self, Press};
use lucina_app::ui::Input;
use lucina_app::{App, Face, Rect, Rgb, Sender, TextEdit, Ui};
use keyboard_types::{Key, Modifiers, NamedKey};
use url::Url;

use lucinanet_omnibox::Kind;

use crate::chrome::{self, Mode};
use crate::onebar::{BarInput, BarLayout, Part, Placed};
use crate::glass::{self, GlassChrome};
use crate::address::{self, Dropdown, Go, Key as OmniKey, Omnibox, Step, to_url};
use crate::keymap::{self, Action};
use crate::tabs::MAX_TABS;
use crate::{
    Answer, Cmd, Control, DialogKind, DownloadState, Engine, News, Page, Security, SelectItem, Start, Starting, TabInfo,
    security_of,
};

const FIELD: &str = "address";
const PAGE: &str = "page";
const PROMPT: &str = "prompt";
/// The find bar's field.
const FIND: &str = "find";

/// How long a note stays in the strip.
const NOTE_FOR: std::time::Duration = std::time::Duration::from_secs(4);

/// Wheel notch in reference pixels, as Firefox's three lines.
const NOTCH: f64 = 57.0;

/// A row of the address bar's dropdown.
const OMNI_ROW_PX: f32 = 34.0;


/// Microseconds since the Unix epoch.
fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
}

/// How long an ended download stays in the strip, so "Saved to" can be read.
const DOWNLOAD_SHOWN: Duration = Duration::from_secs(10);
/// Rows in the downloads strip, newest at the bottom; more would hide too much of the page.
const DOWNLOAD_ROWS: usize = 3;
const DOWNLOAD_ROW_PX: f32 = 40.0;
/// What the panel says when the command opens it with nothing downloaded and nothing running.
const NO_DOWNLOADS: &str = "No downloads";
/// Rows of the history panel in view at once; the selection scrolls the list when it leaves them.
const HISTORY_ROWS: usize = 8;
/// What the history panel says when the store has nothing to list.
const NO_HISTORY: &str = "No history";
/// What the bookmarks panel says when the store has nothing to list.
const NO_BOOKMARKS: &str = "No bookmarks";
/// The find strip's height in reference pixels, at the top of the page.
const FIND_STRIP_PX: f32 = 44.0;
/// What the counter says after the count when the engine's byte limit cut the page's text short.
const PARTIAL: &str = " (partial)";

pub enum Msg {
    News(News),
    /// A line of the test script (`LUCINANET_SCRIPT`): acts as the person at the keyboard would.
    Script(String),
}

pub struct Browser {
    tell: Arc<dyn Fn(News) + Send + Sync>,
    first: Url,
    start: Start,
    engine: Option<Box<dyn Engine>>,
    page: Arc<Mutex<Page>>,
    address: TextEdit,
    title: String,
    loading: bool,
    back: bool,
    forward: bool,
    crashed: Option<String>,
    page_rect: Rect,
    scale: f32,
    typing: bool,
    /// A scripted input and when it was sent: its next frame gives the time from input to painted page.
    waiting: Option<(String, std::time::Instant)>,
    report_frame: bool,
    /// The engine's tabs as it last told them, and the shown one.
    tabs: Vec<TabInfo>,
    active: u32,
    /// Ctrl+T or the + button asked for a tab to type an address into: it keeps the address field's focus.
    typing_new_tab: bool,
    /// The shown tab changed: its page gets the keys.
    focus_page: bool,
    /// What the shown tab asks, drawn over it; nothing reaches the page meanwhile.
    control: Option<Asking>,
    /// A press closed a `<select>`'s list: its release is not the page's either.
    swallow_up: bool,
    note: Option<(String, std::time::Instant)>,
    /// When Enter on the bar was last refused: the field shakes and flashes for [`address::SHAKE_FOR`] after it.
    refused_at: Option<Instant>,
    downloads: Vec<DownloadRow>,
    /// Whether the downloads panel is put up by the command rather than by a download of its own: the
    /// `downloads_open` the keymap resolves Esc against, and the reason a strip is drawn over the page with nothing
    /// in it but the note that says so.
    downloads_open: bool,
    /// Whether the history panel is up (Ctrl+H): never together with the downloads panel.
    history_open: bool,
    /// The pages the engine last listed for the panel, newest first, and the row Enter would open.
    history: Vec<lucinanet_omnibox::HistoryEntry>,
    history_sel: usize,
    /// The engine has answered the last request, so an empty list is an empty store and not a list still on its way.
    history_ready: bool,
    /// The id of the list asked for last; an answer with another id is an old one.
    history_ask: u64,
    /// The bookmarks panel (Ctrl+Shift+O, Ctrl+B): the same state as the history panel's, over the bookmarks list.
    bookmarks_open: bool,
    bookmarks: Vec<lucinanet_omnibox::BookmarkEntry>,
    bookmarks_sel: usize,
    bookmarks_ready: bool,
    bookmarks_ask: u64,
    /// Where the downloads strip is drawn over the page this pass; the page gets no input there.
    strip: Option<Rect>,
    /// The shown tab's zoom in whole percent, as the engine last told it; 100 draws nothing.
    zoom: u16,
    /// Whether the shown tab's page is bookmarked, as the engine last told it; the star is filled when it is.
    bookmarked: bool,
    /// Whether the find bar is up: the `find_bar_open` the keymap resolves its find keys against.
    find_open: bool,
    /// What the find bar's field holds.
    find_text: TextEdit,
    /// The page's text, held here and nowhere else: taken for the open bar, replaced on each request, dropped
    /// when the bar closes, the page changes, the tab changes or the page crashes.
    find_blocks: Vec<lucinanet_find::Block>,
    /// The engine's byte limit cut the text short.
    find_partial: bool,
    /// The id of the page text asked for last; an answer with another id is an old one.
    find_ask: u64,
    /// A request is out and not answered.
    find_pending: bool,
    /// The last request was answered, so the blocks (possibly none) are the page's text and typing searches them.
    find_ready: bool,
    /// The matches over `find_blocks` and the current one.
    finder: lucinanet_find::Finder,
    /// The address of the shown page, as the engine last told it: another address is another page.
    page_url: String,
    /// Where the find strip is drawn this pass; the page gets no input there.
    find_rect: Option<Rect>,
    /// The address bar's suggestions and the engine it searches with (`address.rs`).
    omni: Omnibox,
    /// Where the dropdown was drawn this pass; a press there picks a row and the page gets nothing.
    omni_at: Option<Rect>,
    /// The key that last reached the address field was Backspace or Delete: nothing is completed after it.
    deleting: bool,
    /// Which top bar is drawn and the tab strip's left inset: read once at startup (`chrome.rs`).
    chrome: chrome::Settings,
    /// The glass bar's canvas and backdrop; only in glass mode.
    glass: Option<GlassChrome>,
    /// Where the row was laid out this pass: what both chromes draw from, what both hit-test, and what the tests read.
    layout: BarLayout,
    /// Which tab the pointer is over, so its slot shows a close mark; only a full slot has room for one.
    tab_close_hover: Option<usize>,
    /// The row's empty stretch, as laid out this pass: a window host may drag the window from there.
    drag_area: Rect,
}

/// A [`Control`] as the window shows it.
struct Asking {
    control: Control,
    /// A prompt's field.
    text: TextEdit,
    /// A `<select>`'s list.
    menu: Option<Menu>,
    /// Its first pass gives the prompt's field the keys.
    fresh: bool,
}

struct DownloadRow {
    id: u64,
    name: String,
    received: u64,
    total: Option<u64>,
    state: DownloadState,
    ended: Option<Instant>,
}

impl DownloadRow {
    /// What the row says right of the name.
    fn status(&self) -> String {
        match &self.state {
            DownloadState::Running => match self.total {
                Some(total) => format!("{} of {}", bytes(self.received), bytes(total)),
                None => bytes(self.received),
            },
            DownloadState::Done(path) => format!("Saved to {}", path.display()),
            DownloadState::Failed(reason) => reason.clone(),
        }
    }
}

/// A size as people read it.
fn bytes(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} B"),
    }
}

/// The rows still shown at `now`, and when the next ended one is due to go.
fn shown_downloads(rows: &mut Vec<DownloadRow>, now: Instant) -> Option<Instant> {
    rows.retain(|row| row.ended.is_none_or(|ended| now < ended + DOWNLOAD_SHOWN));
    rows.iter().filter_map(|row| row.ended).min().map(|ended| ended + DOWNLOAD_SHOWN)
}

/// The rows the panel draws: the last [`DOWNLOAD_ROWS`], newest at the bottom. The one place that decides which of
/// the chrome's rows reach the page, so a test can read the same slice the draw loops over.
fn shown_rows(rows: &[DownloadRow]) -> &[DownloadRow] {
    &rows[rows.len().saturating_sub(DOWNLOAD_ROWS)..]
}

/// The line an empty panel says, if this one is empty: the command opens the panel on demand, and a bare band over
/// the page would read as a draw that failed rather than as nothing to show.
fn panel_note(rows: &[DownloadRow]) -> Option<&'static str> {
    rows.is_empty().then_some(NO_DOWNLOADS)
}

/// The rows of the history panel in view: `rows` of the `len` listed, the window that keeps `selected` in sight and
/// moves no further than it has to.
fn history_window(len: usize, selected: usize, rows: usize) -> std::ops::Range<usize> {
    let end = len.min(selected.saturating_add(1).max(rows));
    end.saturating_sub(rows)..end
}

/// The line an empty history panel says, if this one is empty.
fn history_note(entries: &[lucinanet_omnibox::HistoryEntry]) -> Option<&'static str> {
    entries.is_empty().then_some(NO_HISTORY)
}

/// What a history row says: the title (the address when the page has none) and the host, dimmed beside it.
fn history_row(entry: &lucinanet_omnibox::HistoryEntry) -> (String, String) {
    let host = Url::parse(&entry.url).ok().and_then(|url| url.host_str().map(str::to_owned)).unwrap_or_default();
    let title = if entry.title.trim().is_empty() { entry.url.clone() } else { entry.title.clone() };
    (title, host)
}

/// The line an empty bookmarks panel says, if this one is empty.
fn bookmark_note(entries: &[lucinanet_omnibox::BookmarkEntry]) -> Option<&'static str> {
    entries.is_empty().then_some(NO_BOOKMARKS)
}

/// What a bookmarks row says: the title (the address when it has none) and the host, dimmed beside it.
fn bookmark_row(entry: &lucinanet_omnibox::BookmarkEntry) -> (String, String) {
    let host = Url::parse(&entry.url).ok().and_then(|url| url.host_str().map(str::to_owned)).unwrap_or_default();
    let title = if entry.title.trim().is_empty() { entry.url.clone() } else { entry.title.clone() };
    (title, host)
}

impl Browser {
    /// What the find bar's counter says: the finder's words (Firefox's, the profile the keymap follows), the wrap message
    /// after the count, and "(partial)" when the engine cut the text. Empty with no query or no text held.
    fn find_counter(&self) -> String {
        let mut text = self.finder.counter_text(lucinanet_find::CounterStyle::Firefox);
        if self.find_partial && !text.is_empty() {
            text.push_str(PARTIAL);
        }
        text
    }

    /// Let go of the page's text and everything found in it: the blocks, the matches, and any request still out (its answer
    /// is an old one from now on). The text is held nowhere else, so this is the whole of it.
    fn drop_find_text(&mut self) {
        self.find_blocks = Vec::new();
        self.find_partial = false;
        self.find_pending = false;
        self.find_ready = false;
        self.finder = lucinanet_find::Finder::new();
        self.find_ask += 1;
    }

    /// Ask the engine for the shown page's text, dropping what the last request brought: one text at a time.
    fn find_request(&mut self) {
        self.drop_find_text();
        self.find_pending = true;
        self.send(Cmd::PageText { id: self.find_ask });
    }

    /// Run the typed query over the blocks, from where the current match starts so a longer query stays put.
    fn find_search(&mut self) {
        let from = self.finder.current_match().map(|m| (m.block, m.start));
        self.finder.search(&self.find_blocks, &self.find_text.text, lucinanet_find::Options::default(), from);
    }

    /// Enter, Shift+Enter, F3 and Shift+F3: one match on, or back. With no bar up there is nothing to move through.
    fn find_step(&mut self, forward: bool) {
        if self.find_open && self.find_ready {
            if forward { self.finder.next() } else { self.finder.previous() };
        }
    }

    /// The bar goes down and takes the page's text with it.
    fn close_find(&mut self) {
        self.find_open = false;
        self.find_rect = None;
        self.drop_find_text();
    }

    pub fn new(tx: Sender<Msg>, first: Url, start: Start) -> Browser {
        Browser::with_news(Arc::new(move |news| tx.send(Msg::News(news))), first, start)
    }

    pub fn with_news(tell: Arc<dyn Fn(News) + Send + Sync>, first: Url, start: Start) -> Browser {
        Browser::with_chrome(tell, first, start, chrome::Settings::from_env())
    }

    /// As [`with_news`](Self::with_news) with the chrome settings given (they are read from the environment once, by it).
    pub fn with_chrome(tell: Arc<dyn Fn(News) + Send + Sync>, first: Url, start: Start, chrome: chrome::Settings) -> Browser {
        let omni = Omnibox::default();
        // The hint an empty field shows is what the engines allow (`address::placeholder_for`); it is read again every
        // pass below, because the omnibox's engines are what a settings reload hands over.
        let mut address = TextEdit::new().placeholder(address::placeholder_for(&omni.engines));
        // The blank start page shows an empty field and the placeholder, not "about:blank".
        if first.as_str() != "about:blank" {
            address.set(first.as_str());
        }
        Browser {
            tell,
            first,
            start,
            engine: None,
            page: Arc::default(),
            address,
            title: String::new(),
            loading: true,
            back: false,
            forward: false,
            crashed: None,
            page_rect: Rect::new(0, 0, 0, 0),
            scale: 1.0,
            typing: false,
            waiting: None,
            report_frame: false,
            tabs: Vec::new(),
            active: 0,
            typing_new_tab: false,
            focus_page: false,
            control: None,
            swallow_up: false,
            note: None,
            refused_at: None,
            downloads: Vec::new(),
            downloads_open: false,
            history_open: false,
            history: Vec::new(),
            history_sel: 0,
            history_ready: false,
            history_ask: 0,
            bookmarks_open: false,
            bookmarks: Vec::new(),
            bookmarks_sel: 0,
            bookmarks_ready: false,
            bookmarks_ask: 0,
            strip: None,
            zoom: lucinanet_zoom::DEFAULT,
            bookmarked: false,
            find_open: false,
            find_text: TextEdit::new(),
            find_blocks: Vec::new(),
            find_partial: false,
            find_ask: 0,
            find_pending: false,
            find_ready: false,
            finder: lucinanet_find::Finder::new(),
            page_url: String::new(),
            find_rect: None,
            omni,
            omni_at: None,
            deleting: false,
            chrome,
            glass: (chrome.mode == Mode::Glass).then(GlassChrome::new),
            layout: BarLayout::new(&BarInput { width: 1024.0, scale: 1.0, left_inset: 0.0, tabs: 0, active: 0, editing: false, can_back: false, can_forward: false }),
            tab_close_hover: None,
            drag_area: Rect::new(0, 0, 0, 0),
        }
    }

    /// What the glass bar did so far, for the render shot: draws, unchanged frames, the last draw's and the last cached call's ms.
    pub fn glass_stats(&self) -> Option<(u32, u32, f32, f32)> {
        self.glass.as_ref().map(|g| (g.draws, g.cached, g.last_draw_ms, g.last_cached_ms))
    }

    /// What the last pass laid the row out as, and where the page begins: what a picture is measured against (`shot.rs`).
    pub(crate) fn one_bar(&self) -> (BarLayout, Rect) {
        (self.layout.clone(), self.page_rect)
    }

    /// The id the omnibox last asked the engine with, so a picture can answer it as the engine would.
    pub(crate) fn omni_query(&self) -> u64 {
        self.omni.latest_query()
    }

    /// dp kept free at the row's left end (a host with window buttons over it sets this).
    pub fn set_left_inset(&mut self, dp: f32) {
        self.chrome.left_inset = chrome::parse_inset(Some(&dp.to_string()));
    }

    /// Whether a press at (x, y) is on the row's empty stretch, which a window host may use to drag the window.
    pub fn drags_window(&self, x: i32, y: i32) -> bool {
        self.drag_area.contains(x, y)
    }

    fn note(&mut self, text: String) {
        self.note = Some((text, std::time::Instant::now()));
    }

    /// The bar refused to act on what was typed: nothing is loaded or sent; it says why, and the field shakes.
    fn refuse(&mut self, why: &str) {
        self.omni.close();
        self.note(why.to_owned());
        self.refused_at = Some(Instant::now());
    }

    /// The field's sideways push (points) and red flash (0 to 1) at `now`: a pure function of the time since the last
    /// refusal, `(0.0, 0.0)` when there was none or it is over.
    fn shake_at(&self, now: Instant) -> (f32, f32) {
        self.refused_at.map_or((0.0, 0.0), |at| {
            let elapsed = now.saturating_duration_since(at);
            (address::shake_offset(elapsed), address::shake_flash(elapsed))
        })
    }

    fn new_tab(&mut self) {
        // The engine refuses too, but the window knows first and says why at once.
        if self.tabs.len() >= MAX_TABS {
            self.note(format!("No new tab: {MAX_TABS} are open, close one first"));
            return;
        }
        self.typing_new_tab = true;
        self.omni.close();
        self.address.clear();
        self.send(Cmd::NewTab(None));
    }

    fn select_tab(&mut self, id: u32) {
        if id != self.active {
            self.send(Cmd::SelectTab(id));
        }
    }

    /// Answer what the shown tab asks, and stop drawing it until the engine says what comes next.
    fn answer(&mut self, answer: Answer) {
        if let Some(asking) = self.control.take() {
            self.send(Cmd::Answer(asking.control.id(), answer));
            // The keys were the dialog's; they are the page's again.
            self.focus_page = true;
        }
    }

    /// OK on a dialog, or the highlighted row of a `<select>`.
    fn confirm(&mut self) {
        let Some(asking) = &self.control else { return };
        let answer = match (&asking.control, &asking.menu) {
            (Control::Dialog { kind: DialogKind::Prompt(_), .. }, _) => Answer::Ok(Some(asking.text.text.clone())),
            (Control::Dialog { .. }, _) => Answer::Ok(None),
            (Control::Select { .. }, Some(menu)) => match menu.chosen() {
                Some(id) => Answer::Chose(id),
                None => Answer::Cancel,
            },
            (Control::Select { .. }, None) => Answer::Cancel,
        };
        self.answer(answer);
    }

    /// The shortcuts, before anything else sees the key: `lucinanet-shortcuts` says which command a press is
    /// (`keymap`), the window does what it can about it. A press that is a command is taken, wired or not;
    /// the rest goes on to the page or the field.
    fn shortcuts(&mut self, ui: &mut Ui) {
        let Some(Input::Key(press)) = ui.input() else { return };
        let Some(command) = keymap::command(press, self.loading, self.find_open, ui.is_focused(FIELD), self.downloads_open || self.history_open || self.bookmarks_open) else { return };
        let action = keymap::plan(command, self.find_open, self.downloads_open || self.history_open || self.bookmarks_open, &self.tabs, self.active, self.back, self.forward);
        if action == Action::PassToPage {
            return;
        }
        ui.take_input();
        self.run(action, ui);
    }

    fn run(&mut self, action: Action, ui: &mut Ui) {
        match action {
            Action::FocusAddress => {
                self.address.select_all();
                ui.focus(FIELD);
            },
            Action::NewTab => {
                self.new_tab();
                ui.focus(FIELD);
            },
            Action::CloseTab(id) => self.send(Cmd::CloseTab(id)),
            Action::SelectTab(id) => self.select_tab(id),
            Action::Back => self.send(Cmd::Back),
            Action::Forward => self.send(Cmd::Forward),
            Action::Reload => self.send(Cmd::Reload),
            Action::Zoom(step) => self.send(Cmd::Zoom(step)),
            Action::ToggleBookmark => self.send(Cmd::ToggleBookmark),
            // The bar's own state, and the only thing this pass can do for it: up on Ctrl+F, down when Esc was the
            // bar's key rather than the load's. The repeat has nothing to move through while there is no bar.
            // The bar up asks the engine for the page's text, as often as the key is pressed (the text of a page changes), and
            // the field has the keys; down, the text goes with it and the page has the keys again.
            Action::FindOpen => {
                self.find_open = true;
                self.find_text.select_all();
                self.find_request();
                ui.focus(FIND);
                ui.repaint();
            },
            Action::FindClose => {
                self.close_find();
                ui.focus(PAGE);
                ui.repaint();
            },
            Action::FindNext => {
                self.find_step(true);
                ui.repaint();
            },
            Action::FindPrev => {
                self.find_step(false);
                ui.repaint();
            },
            // The panel's own state, and all this pass can do for it: the command is a toggle, so the key up puts
            // the rows over the page and the key up again is the way out; Esc while they are up is theirs too. What
            // is drawn is what the chrome already holds, so the engine is asked for nothing - and the strip keeps
            // standing on its own rule (a download running or just ended) when the command's flag goes back down.
            Action::ShowDownloads => {
                self.downloads_open = !self.downloads_open;
                // One panel at a time: this one takes the history and bookmarks lists down.
                self.history_open = false;
                self.bookmarks_open = false;
                ui.repaint();
            },
            // Esc closes whichever of the two panels is up.
            Action::CloseDownloads => {
                self.downloads_open = false;
                self.history_open = false;
                self.bookmarks_open = false;
                ui.repaint();
            },
            // The history panel: up on the key (the list is asked of the engine, which reads its local store and
            // sends nothing out), down on the key again. The page has the focus meanwhile, so the arrows are the
            // panel's and not the address field's.
            Action::ShowHistory => {
                if std::mem::take(&mut self.history_open) {
                    ui.repaint();
                    return;
                }
                self.downloads_open = false;
                self.bookmarks_open = false;
                self.history_open = true;
                self.history_ready = false;
                self.history.clear();
                self.history_sel = 0;
                self.history_ask += 1;
                self.send(Cmd::ListHistory { id: self.history_ask, limit: crate::HISTORY_LIST_MAX });
                ui.focus(PAGE);
                ui.repaint();
            },
            // The toolkit's own end: the desktop host reads it as `core.quit_requested()` and exits the event loop,
            // the call a `WindowEvent::CloseRequested` makes. There is no second way out to keep in step with this one.
            Action::Quit => ui.quit(),
            // The bookmarks panel is the history panel's twin: up on the key (the engine lists its local bookmarks), down on
            // the key again, and never together with another panel.
            Action::ShowBookmarks => {
                if std::mem::take(&mut self.bookmarks_open) {
                    ui.repaint();
                    return;
                }
                self.downloads_open = false;
                self.history_open = false;
                self.bookmarks_open = true;
                self.bookmarks_ready = false;
                self.bookmarks.clear();
                self.bookmarks_sel = 0;
                self.bookmarks_ask += 1;
                self.send(Cmd::ListBookmarks { id: self.bookmarks_ask, limit: crate::BOOKMARK_LIST_MAX });
                ui.focus(PAGE);
                ui.repaint();
            },
            Action::Unwired | Action::PassToPage => {},
        }
    }

    /// The event, while the shown tab asks something: the dialog's or the list's, never the page's. The
    /// bar and the tabs still work, so another tab can be read meanwhile.
    fn control_input(&mut self, ui: &mut Ui) {
        let Some(asking) = &mut self.control else { return };
        let input = ui.input();
        if let Some(menu) = &mut asking.menu {
            let list = menu.rect;
            match input {
                Some(Input::Key(press)) if !ui.is_focused(FIELD) => {
                    ui.take_input();
                    match menu.key(press.code) {
                        Some(answer) => self.answer(answer),
                        None => ui.repaint(),
                    }
                },
                Some(Input::Move(x, y)) if list.contains(x, y) => {
                    if menu.hover(y) {
                        ui.repaint();
                    }
                },
                Some(Input::Wheel(notches)) if list.contains(ui.pointer().0, ui.pointer().1) => {
                    ui.take_input();
                    menu.scroll(-notches);
                    ui.repaint();
                },
                Some(Input::Down(x, y)) if list.contains(x, y) => ui.take_input(),
                Some(Input::Up(x, y)) if list.contains(x, y) => {
                    ui.take_input();
                    if let Some(id) = menu.at(y) {
                        self.answer(Answer::Chose(id));
                    }
                },
                // A press anywhere else closes the list, as in other browsers; the press itself goes no further.
                Some(Input::Down(..)) | Some(Input::Secondary(..)) => {
                    ui.take_input();
                    self.swallow_up = matches!(input, Some(Input::Down(..)));
                    self.answer(Answer::Cancel);
                },
                Some(Input::Wheel(_)) => ui.take_input(),
                _ => {},
            }
            return;
        }
        match input {
            Some(Input::Key(press)) if !ui.is_focused(FIELD) => match dialog_key(press.code) {
                Some(true) => {
                    ui.take_input();
                    self.confirm();
                },
                Some(false) => {
                    ui.take_input();
                    self.answer(Answer::Cancel);
                },
                // Typing for the prompt's field, which is drawn after this; nothing else.
                None if ui.is_focused(PROMPT) => {},
                None => ui.take_input(),
            },
            // The dialog's own buttons take the pointer when they are drawn; the page never does.
            Some(Input::Wheel(_)) => ui.take_input(),
            _ => {},
        }
    }

    /// This pass's row: the window's width and scale, the tabs, and what the field is doing. Both chromes draw and
    /// hit-test from the same answer, and `view` gives the engine the same height (`onebar.rs`, `docs/design/one-bar.md`).
    fn row_layout(&self, ui: &mut Ui) -> BarLayout {
        let t = ui.theme();
        BarLayout::new(&BarInput {
            width: ui.area().w as f32 / t.k,
            scale: t.k,
            left_inset: self.chrome.left_inset,
            tabs: self.tabs.len(),
            active: self.tabs.iter().position(|tab| tab.id == self.active).unwrap_or(0),
            editing: self.typing,
            can_back: self.back,
            can_forward: self.forward,
        })
    }

    /// The row's presses, in both chromes: the same ids on the same rectangles, since the same layout places them.
    /// Returns the address field's editable rectangle. Nothing is drawn here in glass mode (`glass.rs` paints the row
    /// into its own canvas at the end of the pass); classic paints its solid version first.
    fn bar(&mut self, ui: &mut Ui, l: &BarLayout, band: Rect) -> Rect {
        let (mx, my) = ui.pointer();
        self.drag_area = l.placed(Part::DragRegion).map_or(Rect::new(0, 0, 0, 0), |b| rect(&b));
        // The mark appears under the pointer and is the press that closes; a compact slot has no room for one, and
        // Ctrl+Tab and Ctrl+W reach those tabs.
        self.tab_close_hover = l.parts.iter().find(|b| {
            matches!(b.part, Part::Tab(_)) && !b.compact && b.contains(mx as f32, my as f32)
        }).and_then(|b| match b.part {
            Part::Tab(i) => Some(i),
            _ => None,
        });
        if self.chrome.mode == Mode::Classic {
            self.draw_row(ui, l, band);
        }
        let close_w = ui.px(24.0);
        let mut field = Rect::new(0, 0, 0, 0);
        let mut chosen = None;
        let mut closed = None;
        for b in &l.parts {
            let r = rect(b);
            match b.part {
                // The row's controls are the toolkit's own buttons in both chromes: that is what puts them in the
                // tab chain, and the glass paints over their drawing with its own version of the row.
                Part::Back => {
                    if ui.button_at(r, "back", "\u{2190}") && self.back {
                        self.send(Cmd::Back);
                    }
                },
                Part::Forward => {
                    if ui.button_at(r, "forward", "\u{2192}") && self.forward {
                        self.send(Cmd::Forward);
                    }
                },
                Part::Tab(i) => {
                    if !b.compact {
                        let close = Rect::new(r.right() - close_w, r.y, close_w, r.h);
                        if ui.hit(("close tab", i), close).clicked {
                            closed = self.tabs.get(i).map(|tab| tab.id);
                        }
                    }
                    // The tab's own area stops short of the mark, which took its press first.
                    let body = Rect::new(r.x, r.y, (r.w - if b.compact { 0 } else { close_w }).max(0), r.h);
                    if ui.hit(("tab", i), body).clicked {
                        chosen = self.tabs.get(i).map(|tab| tab.id);
                    }
                },
                Part::Field => {
                    field = r;
                    // The field is editable where its text is. A click there from the host view takes the keys and
                    // the whole address, as the row is read while it is being edited (`one-bar.md`).
                    self.take_address(ui, "field", r);
                },
                // The icon end of the capsule: a click there is meant for the address, not for the state it shows.
                Part::FieldIcon => self.take_address(ui, "field icon", r),
                Part::FieldReload => {
                    if ui.button_at(r, "reload", if self.loading { "\u{00d7}" } else { "\u{21bb}" }) {
                        self.send(Cmd::Reload);
                    }
                },
                Part::NewTab => {
                    if ui.button_at(r, "new tab", "+") {
                        self.new_tab();
                        ui.focus(FIELD);
                    }
                },
                Part::OverflowChip => {
                    if ui.button_at(r, "overflow", &self.tabs.len().saturating_sub(l.parts.iter().filter(|b| matches!(b.part, Part::Tab(_))).count()).to_string()) {
                        // There is no tab list sheet yet; the chip says what is not shown and how to reach it.
                        self.note(format!("{} tabs are hidden: Ctrl+Tab moves between them", self.tabs.len().saturating_sub(l.overflow.len())));
                    }
                },
                // The row's own stretch of nothing: `drags_window` reads it back, and the zoom and the note are shown there.
                Part::DragRegion => {},
                Part::Star => {
                    if ui.button_at(r, "star", if self.bookmarked { "\u{2605}" } else { "\u{2606}" }) {
                        self.send(Cmd::ToggleBookmark);
                    }
                },
                Part::Menu => {
                    if ui.button_at(r, "menu", "\u{22ef}") {
                        self.note("No menu behind it yet: the keys are Ctrl+T, Ctrl+L and Ctrl+P".to_owned());
                    }
                },
            }
        }
        if let Some(id) = closed {
            self.send(Cmd::CloseTab(id));
        } else if let Some(id) = chosen {
            self.select_tab(id);
        }
        if let Some((_, at)) = &self.note {
            if at.elapsed() < NOTE_FOR {
                ui.wake_after(NOTE_FOR - at.elapsed());
            } else {
                self.note = None;
            }
        }
        if let Some(at) = self.refused_at {
            if at.elapsed() < address::SHAKE_FOR {
                ui.wake_after(Duration::from_millis(16));
            } else {
                self.refused_at = None;
            }
        }
        field
    }

    /// A press in the field that is not editing: the keys and the whole address, so what is there can be replaced.
    fn take_address(&mut self, ui: &mut Ui, id: &'static str, r: Rect) {
        if ui.hit(id, r).clicked && !ui.is_focused(FIELD) {
            self.address.select_all();
            ui.focus(FIELD);
        }
    }

    /// The classic row: the same rectangles as the glass, solid fills, and DejaVu's own glyphs. The toolkit draws no
    /// favicons, so a tab is its title and the field is its host; the glass has both pictures (`glass.rs`).
    fn draw_row(&mut self, ui: &mut Ui, l: &BarLayout, band: Rect) {
        let t = ui.theme();
        ui.fill(band, t.bar);
        // One hairline under the row; the page starts below it.
        ui.fill(Rect::new(band.x, band.bottom() - 1, band.w, 1), t.panel);
        let editing = ui.is_focused(FIELD);
        // The capsule the field, its mark and the reload sit in: one step brighter than the row, and behind all three.
        // While editing, the toolkit draws the field's own box and needs no capsule under it.
        if !editing && let Some(cap) = l.field_capsule() {
            let cap = rect(&cap);
            ui.fill_round(cap, cap.h / 2, t.panel_hi);
        }
        for b in &l.parts {
            let r = rect(b);
            let rad = r.h / 2;
            match b.part {
                Part::Back | Part::Forward | Part::FieldReload | Part::NewTab | Part::OverflowChip | Part::Star | Part::Menu => {},
                Part::Tab(i) => {
                    let (label, shown, host) = match self.tabs.get(i) {
                        Some(tab) => (tab_label(tab), tab.id == self.active, chrome::host_of(&tab.url)),
                        None => continue,
                    };
                    ui.fill_round(r, rad, if shown { t.panel_hi } else { t.panel });
                    if b.compact {
                        // The compact form is the site's initial, as the glass chrome draws it: 30 dp holds no title, and
                        // an ellipsised one reads as a control with nothing to say.
                        let initial = host.chars().next().unwrap_or('?').to_ascii_uppercase().to_string();
                        let w = ui.measure(Face::Sans, t.pt(13.0), &initial);
                        ui.text_in(Rect::new(r.x + (r.w - w) / 2, r.y, w.max(1), r.h), Face::Sans, t.pt(13.0), t.item, &initial);
                    } else {
                        // The last 24 dp of a full slot are the close mark's, shown only under the pointer; the title is
                        // cut short of them either way, so nothing moves when the mark appears.
                        let mark = ui.px(24.0);
                        let text = Rect::new(r.x + ui.px(10.0), r.y, (r.w - ui.px(20.0) - mark).max(0), r.h);
                        ui.text_in(text, Face::Sans, t.pt(13.0), if shown { t.text } else { t.item }, &label);
                        if self.tab_close_hover == Some(i) {
                            ui.text_in(Rect::new(r.right() - mark, r.y, mark, r.h), Face::Sans, t.pt(14.0), t.dim, "\u{00d7}");
                        }
                    }
                },
                // What the field says is the toolkit's own text, or `draw_classic_field`'s host over the capsule.
                Part::Field => {},
                Part::FieldIcon => field_mark(ui, r, Mark::of(editing, &self.address.text), t.item, if editing { t.bar } else { t.panel_hi }),
                // What the row has to say in its own empty stretch: the page's zoom, or a note that has not gone away.
                Part::DragRegion => {
                    let (text, colour, px) = match &self.note {
                        Some((n, at)) if at.elapsed() < NOTE_FOR => (n.clone(), t.danger, t.pt(13.0)),
                        _ if self.zoom != lucinanet_zoom::DEFAULT => (lucinanet_zoom::label(self.zoom), t.item, t.pt(11.0)),
                        _ => (String::new(), t.item, t.pt(13.0)),
                    };
                    if !text.is_empty() && r.w >= ui.px(40.0) {
                        ui.text_in(Rect::new(r.x + ui.px(8.0), r.y, r.w - ui.px(16.0), r.h), Face::Sans, px, colour, &text);
                    }
                },
            }
        }
    }

    /// The field of the classic row, after the toolkit's own box: the address's display - `https://` and all dropped, the
    /// registrable domain the engine named in its own ink - centred while nobody is editing, and the load's line along the
    /// capsule's bottom edge. `one-bar.md`: no spinner anywhere else.
    fn draw_classic_field(&mut self, ui: &mut Ui, l: &BarLayout, editing: bool) {
        let Some(cap) = l.field_capsule() else { return };
        let (cap, text) = (rect(&cap), rect(&l.placed(Part::Field).expect("a field")));
        let t = ui.theme();
        let (dx, flash) = self.shake_at(Instant::now());
        let dx = (dx * t.k).round() as i32;
        let cap = Rect::new(cap.x + dx, cap.y, cap.w, cap.h);
        if flash > 0.0 {
            // A red ring round the capsule that fades with the shake: the refusal, seen.
            let ring = mix(if editing { t.bar } else { t.panel_hi }, t.danger, flash);
            let w = ui.px(2.0);
            ui.fill(Rect::new(cap.x, cap.y, cap.w, w), ring);
            ui.fill(Rect::new(cap.x, cap.bottom() - w, cap.w, w), ring);
            ui.fill(Rect::new(cap.x, cap.y, w, cap.h), ring);
            ui.fill(Rect::new(cap.right() - w, cap.y, w, cap.h), ring);
        }
        if !editing {
            let px = t.pt(15.0);
            let display = crate::address::field_display(&self.address.text, self.field_domain(), text.w as f32, |s| ui.measure(Face::Sans, px, s) as f32);
            let (before, strong, after) = crate::address::field_pieces(&display);
            // The whole text in one ink when there is nothing to emphasise inside it (no host at all); otherwise the site
            // in the row's own text colour and the rest of the address - scheme, subdomain, path - dimmed beside it.
            let rest = if display.domain.is_none() { t.text } else { t.dim };
            let widths = [before, strong, after].map(|s| ui.measure(Face::Sans, px, s).max(0));
            let mut x = cap.x + (cap.w - widths.iter().sum::<i32>()) / 2;
            for ((run, ink), w) in [(before, rest), (strong, t.text), (after, rest)].iter().zip(widths) {
                if !run.is_empty() {
                    ui.text_in(Rect::new(x, cap.y, w.max(1), text.h), Face::Sans, px, *ink, run);
                }
                x += w;
            }
        }
        if self.loading {
            let line = ui.px(2.0);
            ui.fill(Rect::new(cap.x, cap.bottom() - line, cap.w, line), t.accent);
        }
    }

    /// The shown tab's registrable domain, the engine's answer from its Public Suffix List.
    ///
    /// Trusted only for the address it describes: the tabs are a snapshot of a load, and the field's text is what the
    /// person has in it, so an emphasis taken from another page's host - or from an earlier page in this tab - would mark
    /// out a piece of the address that is not the site. `None` then means the whole host is emphasised, which is what the
    /// field did before there was a list.
    fn field_domain(&self) -> Option<&str> {
        self.tabs
            .iter()
            .find(|tab| tab.id == self.active && tab.url == self.address.text)
            .and_then(|tab| tab.registrable_domain.as_deref())
    }

    /// The dialog or the list the shown tab asks for, over the page.
    fn draw_control(&mut self, ui: &mut Ui) {
        let page = self.page_rect;
        let Some(asking) = &mut self.control else { return };
        let t = ui.theme();
        let px = t.pt(15.0);
        if let Some(menu) = &mut asking.menu {
            menu.place(ui, page);
            menu.draw(ui);
            return;
        }
        let Control::Dialog { origin, message, kind, .. } = &asking.control else { return };
        // The page dims under the box: the box is the window's, and says whose words it holds.
        let w = ui.px(460.0).min(page.w - ui.px(16.0));
        let pad = ui.px(16.0);
        let line = ui.line_height(Face::Sans, px);
        let lines = ui.wrap_count(Face::Sans, px, message, w - pad * 2).clamp(1, 12);
        let field = if matches!(kind, DialogKind::Prompt(_)) { ui.px(34.0) + pad } else { 0 };
        let h = pad * 3 + line + line * lines + field + ui.px(34.0) + pad;
        let r = Rect::new(page.x + (page.w - w) / 2, page.y + ui.px(48.0).min((page.h - h).max(0) / 3), w, h);
        ui.fill_round(Rect::new(r.x - 2, r.y - 2, r.w + 4, r.h + 4), ui.px(10.0), t.accent);
        ui.fill_round(r, ui.px(10.0), t.panel);
        let inner = Rect::new(r.x + pad, r.y + pad, r.w - pad * 2, r.h - pad * 2);
        let title = if origin.is_empty() { "This page says".to_owned() } else { format!("{origin} says") };
        ui.text_in(Rect::new(inner.x, inner.y, inner.w, line), Face::Bold, px, t.text, &title);
        let body = Rect::new(inner.x, inner.y + line + pad, inner.w, line * lines);
        ui.within(body, |ui| ui.wrapped(Face::Sans, px, t.item, message));
        let mut y = body.bottom() + pad;
        let mut submitted = false;
        if matches!(kind, DialogKind::Prompt(_)) {
            let f = Rect::new(inner.x, y, inner.w, ui.px(34.0));
            if std::mem::take(&mut asking.fresh) {
                asking.text.select_all();
                ui.focus(PROMPT);
            }
            submitted = ui.text_field_at(f, PROMPT, &mut asking.text).submitted;
            y += f.h + pad;
        } else if std::mem::take(&mut asking.fresh) {
            ui.focus(("dialog", 0));
        }
        let bw = ui.px(96.0);
        let ok = Rect::new(inner.right() - bw, y, bw, ui.px(34.0));
        let cancel = Rect::new(ok.x - bw - pad / 2, y, bw, ui.px(34.0));
        let mut cancelled = false;
        if !matches!(kind, DialogKind::Alert) && ui.button_at(cancel, ("dialog", 1), "Cancel") {
            cancelled = true;
        }
        if ui.button_at(ok, ("dialog", 0), "OK") {
            submitted = true;
        }
        if cancelled {
            self.answer(Answer::Cancel);
        } else if submitted {
            self.confirm();
        }
    }

    fn download_news(&mut self, id: u64, name: String, received: u64, total: Option<u64>, state: DownloadState) {
        let ended = (state != DownloadState::Running).then(Instant::now);
        let row = DownloadRow {
            id,
            name,
            received,
            total,
            state,
            ended,
        };
        match self.downloads.iter_mut().find(|row| row.id == id) {
            Some(old) => *old = row,
            None => self.downloads.push(row),
        }
    }

    /// The strip over the bottom of the page while a download runs or has just ended, and while the command has the
    /// panel up: the page keeps its size, as the engine has no way to be resized.
    fn downloads_strip(&mut self, ui: &mut Ui) {
        if let Some(next) = shown_downloads(&mut self.downloads, Instant::now()) {
            ui.wake_after(next.saturating_duration_since(Instant::now()));
        }
        let Some(strip) = self.strip else { return };
        let t = ui.theme();
        ui.fill(strip, t.bar);
        let row_h = ui.px(DOWNLOAD_ROW_PX);
        let gap = ui.px(8.0);
        let side = row_h - gap;
        if self.history_open {
            self.history_panel(ui, strip);
            return;
        }
        if self.bookmarks_open {
            self.bookmarks_panel(ui, strip);
            return;
        }
        if let Some(note) = panel_note(&self.downloads) {
            let at = Rect::new(strip.x + gap * 2, strip.y + gap / 2, (strip.w - gap * 4).max(0), row_h);
            ui.text_in(at, Face::Sans, t.pt(15.0), t.dim, note);
            return;
        }
        let mut cancel = None;
        for (i, row) in shown_rows(&self.downloads).iter().enumerate() {
            let r = Rect::new(strip.x, strip.y + gap / 2 + i as i32 * row_h, strip.w, row_h);
            let name_w = (r.w * 2 / 5).min(ui.px(360.0));
            let name = Rect::new(r.x + gap * 2, r.y, name_w, r.h);
            ui.text_in(name, Face::Sans, t.pt(15.0), t.text, &row.name);
            let mut x = name.right() + gap * 2;
            let right = r.right() - gap * 2;
            if row.state == DownloadState::Running {
                let button = Rect::new(right - side, r.y + (r.h - side) / 2, side, side);
                if ui.button_at(button, ("download-cancel", row.id as usize), "\u{00d7}") {
                    cancel = Some(row.id);
                }
                // A bar for the part that is known; without a size it stays empty and the text counts.
                let track_w = ((button.x - gap * 2 - x) / 2).max(0);
                let track = Rect::new(x, r.y + (r.h - ui.px(6.0)) / 2, track_w, ui.px(6.0));
                ui.fill_round(track, track.h / 2, t.panel_hi);
                if let Some(total) = row.total.filter(|total| *total > 0) {
                    let done = (track.w as u64 * row.received.min(total) / total) as i32;
                    ui.fill_round(Rect::new(track.x, track.y, done, track.h), track.h / 2, t.accent);
                }
                x = track.right() + gap * 2;
                let status = Rect::new(x, r.y, (button.x - gap - x).max(0), r.h);
                ui.text_in(status, Face::Sans, t.pt(13.0), t.dim, &row.status());
            } else {
                let colour = match row.state {
                    DownloadState::Failed(_) => t.danger,
                    _ => t.dim,
                };
                let status = Rect::new(x, r.y, (right - x).max(0), r.h);
                ui.text_in(status, Face::Sans, t.pt(13.0), colour, &row.status());
            }
        }
        if let Some(id) = cancel {
            self.send(Cmd::CancelDownload(id));
        }
    }

    /// Up, Down, Enter while the history panel is up: the panel's, never the page's or the field's. Enter opens the
    /// selected page in the shown tab the way an address-bar row is opened, and the panel goes down.
    fn history_keys(&mut self, ui: &mut Ui) {
        if !self.history_open || ui.is_focused(FIELD) {
            return;
        }
        let Some(Input::Key(press)) = ui.input() else { return };
        if press.mods.ctrl || press.mods.alt || press.mods.meta || press.mods.shift {
            return;
        }
        match press.code {
            keys::KEY_DOWN => self.history_sel = (self.history_sel + 1).min(self.history.len().saturating_sub(1)),
            keys::KEY_UP => self.history_sel = self.history_sel.saturating_sub(1),
            keys::KEY_ENTER | keys::KEY_KPENTER => {
                if let Some(url) = self.history.get(self.history_sel).and_then(|e| Url::parse(&e.url).ok()) {
                    self.history_open = false;
                    self.open(Go { url, search: false });
                }
            },
            _ => return,
        }
        ui.take_input();
        ui.repaint();
    }

    /// Esc while the find bar is up is the bar's own key, whatever else is loading or focused: down it goes, the page's text with
    /// it, and the page has the keys again.
    fn find_keys(&mut self, ui: &mut Ui) {
        if !self.find_open {
            return;
        }
        let Some(Input::Key(press)) = ui.input() else { return };
        if press.code == keys::KEY_ESC && !(press.mods.ctrl || press.mods.alt || press.mods.meta || press.mods.shift) {
            ui.take_input();
            self.close_find();
            ui.focus(PAGE);
            ui.repaint();
        }
    }

    /// The find strip: the field the query is typed into and the counter after it. Typing searches the page's text the bar
    /// holds (and asks for it when it holds none); Enter goes to the next match, Shift+Enter to the one before. Nothing is
    /// highlighted or scrolled to in the page yet: that needs the matches' nodes handed back to it.
    fn find_strip(&mut self, ui: &mut Ui) {
        let Some(bar) = self.find_rect else { return };
        let t = ui.theme();
        ui.fill(bar, t.bar);
        let gap = ui.px(8.0);
        let h = ui.px(32.0);
        let field = Rect::new(bar.x + gap * 2, bar.y + (bar.h - h) / 2, ui.px(360.0).min(bar.w / 2), h);
        // The field takes the key with its modifiers, so Shift is read before it does.
        let shifted = shift_return(ui.input());
        let typed = ui.text_field_at(field, FIND, &mut self.find_text);
        if typed.changed {
            if self.find_ready {
                self.find_search();
            } else if !self.find_pending {
                self.find_request();
            }
            ui.repaint();
        }
        if typed.submitted {
            self.find_step(!shifted);
            ui.repaint();
        }
        let x = field.right() + gap * 2;
        let counter = self.find_counter();
        ui.text_in(Rect::new(x, bar.y, (bar.right() - gap * 2 - x).max(0), bar.h), Face::Sans, t.pt(14.0), t.dim, &counter);
    }

    /// Up, Down, Enter while the bookmarks panel is up: the history panel's keys over the bookmarks list.
    fn bookmarks_keys(&mut self, ui: &mut Ui) {
        if !self.bookmarks_open || ui.is_focused(FIELD) {
            return;
        }
        let Some(Input::Key(press)) = ui.input() else { return };
        if press.mods.ctrl || press.mods.alt || press.mods.meta || press.mods.shift {
            return;
        }
        match press.code {
            keys::KEY_DOWN => self.bookmarks_sel = (self.bookmarks_sel + 1).min(self.bookmarks.len().saturating_sub(1)),
            keys::KEY_UP => self.bookmarks_sel = self.bookmarks_sel.saturating_sub(1),
            keys::KEY_ENTER | keys::KEY_KPENTER => {
                if let Some(url) = self.bookmarks.get(self.bookmarks_sel).and_then(|e| Url::parse(&e.url).ok()) {
                    self.bookmarks_open = false;
                    self.open(Go { url, search: false });
                }
            },
            _ => return,
        }
        ui.take_input();
        ui.repaint();
    }

    /// The bookmarks panel's rows in the strip, drawn as the history panel's are: the title (the address when there is none)
    /// and the host dimmed beside it; one line when there is nothing.
    fn bookmarks_panel(&mut self, ui: &mut Ui, strip: Rect) {
        let t = ui.theme();
        let row_h = ui.px(DOWNLOAD_ROW_PX);
        let gap = ui.px(8.0);
        if let Some(note) = bookmark_note(&self.bookmarks).filter(|_| self.bookmarks_ready) {
            let at = Rect::new(strip.x + gap * 2, strip.y + gap / 2, (strip.w - gap * 4).max(0), row_h);
            ui.text_in(at, Face::Sans, t.pt(15.0), t.dim, note);
            return;
        }
        let window = history_window(self.bookmarks.len(), self.bookmarks_sel, HISTORY_ROWS);
        for (i, at) in window.enumerate() {
            let r = Rect::new(strip.x, strip.y + gap / 2 + i as i32 * row_h, strip.w, row_h);
            if at == self.bookmarks_sel {
                ui.fill(r, t.panel_hi);
            }
            let (title, host) = bookmark_row(&self.bookmarks[at]);
            let title_w = (r.w * 3 / 5).max(0);
            ui.text_in(Rect::new(r.x + gap * 2, r.y, title_w, r.h), Face::Sans, t.pt(15.0), t.text, &title);
            let x = r.x + gap * 2 + title_w + gap * 2;
            ui.text_in(Rect::new(x, r.y, (r.right() - gap * 2 - x).max(0), r.h), Face::Sans, t.pt(13.0), t.dim, &host);
        }
    }

    /// The history panel's rows in the strip: the title (the address when there is none) and the host dimmed beside it,
    /// the selected row on the raised fill; one line when the store has nothing.
    fn history_panel(&mut self, ui: &mut Ui, strip: Rect) {
        let t = ui.theme();
        let row_h = ui.px(DOWNLOAD_ROW_PX);
        let gap = ui.px(8.0);
        if let Some(note) = history_note(&self.history).filter(|_| self.history_ready) {
            let at = Rect::new(strip.x + gap * 2, strip.y + gap / 2, (strip.w - gap * 4).max(0), row_h);
            ui.text_in(at, Face::Sans, t.pt(15.0), t.dim, note);
            return;
        }
        let window = history_window(self.history.len(), self.history_sel, HISTORY_ROWS);
        for (i, at) in window.enumerate() {
            let r = Rect::new(strip.x, strip.y + gap / 2 + i as i32 * row_h, strip.w, row_h);
            if at == self.history_sel {
                ui.fill(r, t.panel_hi);
            }
            let (title, host) = history_row(&self.history[at]);
            let title_w = (r.w * 3 / 5).max(0);
            ui.text_in(Rect::new(r.x + gap * 2, r.y, title_w, r.h), Face::Sans, t.pt(15.0), t.text, &title);
            let x = r.x + gap * 2 + title_w + gap * 2;
            ui.text_in(Rect::new(x, r.y, (r.right() - gap * 2 - x).max(0), r.h), Face::Sans, t.pt(13.0), t.dim, &host);
        }
    }

    fn send(&self, cmd: Cmd) {
        if let Some(engine) = &self.engine {
            engine.send(cmd);
        }
    }

    /// Enter on the typed text: what `classify` makes of it, loaded as a typed address.
    fn go(&mut self, typed: &str) {
        if let Some(go) = address::go_for(typed, &self.omni.engines) {
            self.open(go);
        } else if let Some(why) = address::refusal_for(typed, &self.omni.engines) {
            // Nothing to act on (no engine to search with, or text the bar never opens): nothing is loaded or sent.
            self.refuse(why);
        }
    }

    /// Shift-Return on the typed text: the same refusals as Return, an address opened as Return opens it, and a search sent
    /// verbatim to the second engine (when there is none this is [`Browser::go`]).
    fn go_shift(&mut self, typed: &str) {
        if let Some(go) = address::go_shift_for(typed, &self.omni.engines) {
            self.open(go);
        } else if let Some(why) = address::refusal_shift_for(typed, &self.omni.engines) {
            self.refuse(why);
        }
    }

    /// Load `go` as an address from the bar and close the dropdown.
    fn open(&mut self, go: Go) {
        self.omni.close();
        self.address.set(go.url.as_str());
        self.send(go.command());
    }

    /// The address the shown tab is at, as the field shows it when nothing is being typed.
    fn page_address(&self) -> String {
        let url = self.tabs.iter().find(|t| t.id == self.active).map_or(self.first.as_str(), |t| t.url.as_str());
        if url == "about:blank" { String::new() } else { url.to_owned() }
    }

    /// Up, Down and Esc while the dropdown is open and the field has the keyboard (they move over the rows, or revert
    /// and close); and what the last key was, for the completion.
    fn omnibox_keys(&mut self, ui: &mut Ui) {
        if !ui.is_focused(FIELD) {
            return;
        }
        let Some(Input::Key(press)) = ui.input() else { return };
        self.deleting = matches!(press.code, keys::KEY_BACKSPACE | keys::KEY_DELETE);
        let key = match press.code {
            keys::KEY_DOWN => OmniKey::Down,
            keys::KEY_UP => OmniKey::Up,
            keys::KEY_ESC => OmniKey::Escape,
            _ => return,
        };
        if press.mods.ctrl || press.mods.alt || press.mods.meta {
            return;
        }
        let Some(dropdown) = &mut self.omni.dropdown else { return };
        ui.take_input();
        match dropdown.key(key) {
            Step::Text(text) => self.address.set(&text),
            Step::Revert => {
                self.omni.close();
                let url = self.page_address();
                self.address.set(&url);
            },
        }
    }

    /// A press on a row of the dropdown opens it; its release is not the page's either.
    fn omnibox_click(&mut self, ui: &mut Ui) {
        let (Some(at), Some(Input::Down(x, y))) = (self.omni_at, ui.input()) else { return };
        if !at.contains(x, y) {
            return;
        }
        ui.take_input();
        self.swallow_up = true;
        let row = ((y - at.y) / ui.px(OMNI_ROW_PX).max(1)) as usize;
        if let Some(go) = self.omni.dropdown.as_ref().and_then(|d| d.pick(row)) {
            self.open(go);
            ui.focus(PAGE);
        }
    }

    /// The tail of the first row's address, selected after the typed text, once the history has answered. Only while the
    /// field still holds exactly what was typed, with the caret at its end.
    fn complete_inline(&mut self) {
        let Some(dropdown) = &self.omni.dropdown else { return };
        let Some(tail) = dropdown.completion() else { return };
        let at_end = self.address.cursor == self.address.text.len() && self.address.anchor == self.address.cursor;
        if !self.typing || !at_end || self.address.text != dropdown.typed() {
            return;
        }
        let typed_len = self.address.text.len();
        self.address.text.push_str(tail);
        self.address.anchor = typed_len;
        self.address.cursor = self.address.text.len();
    }

    /// What the glass row is drawn from: everything of the browser's that the frame shows, read out of it. The hint of an
    /// empty field comes from the engines the omnibox holds ([`address::placeholder_for`], the same call the classic field
    /// makes), so the glass says what the bar can really do. A function of its own so a test can read the frame's inputs
    /// without a window.
    fn glass_state(&self) -> glass::State {
        let shake = self.shake_at(Instant::now());
        glass::State {
            tabs: self.tabs.clone(),
            active: self.active,
            address: self.address.text.clone(),
            domain: self.field_domain().map(str::to_owned),
            typing: self.typing,
            cursor: self.address.cursor,
            anchor: self.address.anchor,
            placeholder: address::placeholder_for(&self.omni.engines).to_owned(),
            loading: self.loading,
            close_hover: self.tab_close_hover,
            zoom: (self.zoom != lucinanet_zoom::DEFAULT).then(|| lucinanet_zoom::label(self.zoom)),
            bookmarked: self.bookmarked,
            note: self.note.as_ref().filter(|(_, at)| at.elapsed() < NOTE_FOR).map(|(n, _)| n.clone()),
            shake: shake.0,
            flash: shake.1,
        }
    }

    /// The glass row over `band`: the page's top rows are the backdrop, the pixels go over what the toolkit drew there.
    fn draw_glass(&mut self, ui: &mut Ui, l: &BarLayout, band: Rect) {
        let k = ui.theme().k;
        let (bw, bh) = glass::band_px(l);
        let state = self.glass_state();
        let Some(g) = self.glass.as_mut() else { return };
        {
            let page = self.page.lock().unwrap();
            g.backdrop.sample(&page.px, page.w as usize, page.h as usize, bw, bh, k);
        }
        let frame = g.frame(&state, l);
        ui.pixels(band, frame.w as usize, frame.px);
        if let Some(d) = g.wake_after() {
            ui.wake_after(d);
        }
    }

    /// The dropdown, over the page and under the bar's field. Basic drawing with the toolkit's own parts.
    fn omnibox_draw(&self, ui: &mut Ui) {
        let (Some(dropdown), Some(at)) = (&self.omni.dropdown, self.omni_at) else { return };
        let mut t = ui.theme();
        // Under the glass bar the list takes the bar's tint and ink, so it does not clash (a plain list; glass is the next slice).
        if let Some(g) = &self.glass {
            let c = glass::dropdown_colours(&g.look);
            (t.panel, t.text, t.dim, t.selection, t.accent) = (Rgb(c[0]), Rgb(c[1]), Rgb(c[2]), Rgb(c[3]), Rgb(c[4]));
        }
        let (px, pad, row_h) = (t.pt(15.0), ui.px(10.0), ui.px(OMNI_ROW_PX));
        ui.fill(Rect::new(at.x - 1, at.y - 1, at.w + 2, at.h + 2), t.accent);
        ui.fill(at, t.panel);
        ui.within(at, |ui| {
            for (i, row) in dropdown.rows().iter().enumerate() {
                let r = Rect::new(at.x, at.y + i as i32 * row_h, at.w, row_h);
                if i == dropdown.selected() {
                    ui.fill(r, t.selection);
                }
                // A page shows its title and, dim at the right, its address; the rest are one line.
                let page = matches!(row.kind, Kind::History | Kind::Bookmark) && !row.title.is_empty();
                let main = if row.title.is_empty() { &row.text } else { &row.title };
                let left = if page { r.w * 3 / 5 } else { r.w };
                ui.text_in(Rect::new(r.x + pad, r.y, (left - pad * 2).max(0), r.h), Face::Sans, px, t.text, main);
                if page {
                    let x = r.x + left;
                    ui.text_in(Rect::new(x, r.y, (r.right() - x - pad).max(0), r.h), Face::Sans, t.pt(13.0), t.dim, &row.text);
                }
            }
        });
    }

    fn inside(&self, x: i32, y: i32) -> (f32, f32) {
        (
            (x - self.page_rect.x) as f32,
            (y - self.page_rect.y) as f32,
        )
    }

    fn script(&mut self, line: &str) {
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        match word {
            "load" => self.go(rest),
            "back" => self.send(Cmd::Back),
            "forward" => self.send(Cmd::Forward),
            "reload" => self.send(Cmd::Reload),
            "wheel" => {
                let notches: f64 = rest.parse().unwrap_or(-1.0);
                self.send(Cmd::Wheel {
                    y: notches * NOTCH * self.scale as f64,
                    x: 200.0,
                    y_at: 200.0,
                });
            },
            "click" => {
                if let Some((x, y)) = rest.split_once(' ') {
                    let (x, y) = (x.parse().unwrap_or(0.0), y.parse().unwrap_or(0.0));
                    self.send(Cmd::Move(x, y));
                    self.send(Cmd::Button { down: true, x, y });
                    self.send(Cmd::Button { down: false, x, y });
                }
            },
            "type" => {
                for c in rest.chars() {
                    self.send(Cmd::Key {
                        key: Key::Character(c.to_string()),
                        modifiers: Modifiers::empty(),
                    });
                }
            },
            "key" => {
                if let Some(key) = named(rest) {
                    self.send(Cmd::Key {
                        key,
                        modifiers: Modifiers::empty(),
                    });
                }
            },
            // `newtab [address]`, `tab <n>` (the n-th in the strip, from 1), `closetab` (the shown one).
            "newtab" => match to_url(rest) {
                Some(url) => {
                    self.address.set(url.as_str());
                    self.send(Cmd::NewTab(Some(url)));
                },
                None => self.new_tab(),
            },
            "tab" => {
                let n: usize = rest.trim().parse().unwrap_or(0);
                if let Some(id) = n.checked_sub(1).and_then(|i| self.tabs.get(i)).map(|t| t.id) {
                    self.select_tab(id);
                }
            },
            "closetab" => self.send(Cmd::CloseTab(self.active)),
            // `dialog ok`, `dialog cancel`, `dialog text <s>`: what the person would do with the dialog or list
            // the shown tab asks for. `text` fills a prompt's field, or highlights a `<select>`'s option by its
            // label; `ok` then answers with it.
            "dialog" => {
                let (what, text) = rest.split_once(' ').unwrap_or((rest, ""));
                match what {
                    "ok" => self.confirm(),
                    "cancel" => self.answer(Answer::Cancel),
                    "text" => match &mut self.control {
                        Some(Asking { menu: Some(menu), .. }) => menu.highlight_label(text),
                        Some(asking) => asking.text.set(text),
                        None => {},
                    },
                    _ => eprintln!("lucinanet: unknown script line: {line}"),
                }
            },
            _ => eprintln!("lucinanet: unknown script line: {line}"),
        }
    }

    /// The event, if it is the page's. Runs before the bar's widgets; the page goes first in the
    /// focus order, so a new window's keys go to the page.
    fn page_input(&mut self, ui: &mut Ui) {
        let r = self.page_rect;
        let input = ui.input();
        let strip = self.strip;
        let find = self.find_rect;
        let on_strip = |x, y| strip.is_some_and(|strip| strip.contains(x, y)) || find.is_some_and(|bar| bar.contains(x, y));
        match input {
            // The strip's own buttons take these.
            Some(Input::Move(x, y) | Input::Down(x, y) | Input::Up(x, y)) if on_strip(x, y) => {},
            Some(Input::Key(press)) if ui.is_focused(PAGE) => {
                if let Some((key, modifiers)) = to_key(press) {
                    self.send(Cmd::Key { key, modifiers });
                }
                ui.take_input();
            },
            Some(Input::Move(x, y)) if r.contains(x, y) => {
                let (x, y) = self.inside(x, y);
                self.send(Cmd::Move(x, y));
            },
            Some(Input::Down(x, y)) if r.contains(x, y) => {
                let (x, y) = self.inside(x, y);
                self.send(Cmd::Button { down: true, x, y });
            },
            Some(Input::Up(x, y)) if r.contains(x, y) => {
                let (x, y) = self.inside(x, y);
                self.send(Cmd::Button { down: false, x, y });
            },
            Some(Input::Wheel(notches)) => {
                let (x, y) = ui.pointer();
                let (x, y) = self.inside(x, y);
                self.send(Cmd::Wheel {
                    y: notches as f64 * NOTCH * self.scale as f64,
                    x,
                    y_at: y,
                });
                ui.take_input();
            },
            _ => {},
        }
        // Above the strip only, or the page would take the press meant for a strip button.
        let above_strip = strip.map_or(r, |strip| Rect::new(r.x, r.y, r.w, strip.y - r.y));
        ui.focus_area(PAGE, above_strip);
    }
}

impl App for Browser {
    type Msg = Msg;

    fn message(&mut self, msg: Msg) {
        match msg {
            Msg::News(News::Frame) => {
                if std::mem::take(&mut self.report_frame) {
                    crate::phase("first frame reached the window");
                }
                if let Some((line, sent)) = self.waiting.take() {
                    eprintln!("lucinanet: {line} to frame {} ms", sent.elapsed().as_millis());
                }
            },
            // What a frame's parts cost, for the scroll benchmark (`scrollbench.rs`): a window draws from the
            // page and does not care how the frame was made.
            Msg::News(News::Painted { .. }) => {},
            // What someone is typing stays.
            // Another address is another page: what the find bar took from the last one is no answer for this one.
            Msg::News(News::Url(url)) => {
                if url != self.page_url {
                    self.page_url.clone_from(&url);
                    self.drop_find_text();
                }
                if !self.typing {
                    self.address.set(&url);
                }
            },
            Msg::News(News::Title(title)) => self.title = title,
            Msg::News(News::Loading(loading)) => {
                if loading {
                    self.report_frame = true;
                    // A load that starts replaces the page under the bar, the same address or not.
                    self.drop_find_text();
                }
                self.loading = loading;
            },
            Msg::News(News::History { back, forward }) => {
                self.back = back;
                self.forward = forward;
            },
            Msg::News(News::Crashed(reason)) => {
                self.crashed = Some(reason);
                self.drop_find_text();
            },
            Msg::News(News::Dumped) => {},
            Msg::News(News::Tabs { tabs, active }) => {
                if active != self.active {
                    // Another tab's page: its own address, and no crash notice from the last one.
                    self.crashed = None;
                    self.omni.close();
                    // The bar belonged to the other tab's page, and so does the text it holds.
                    self.close_find();
                    if let Some(tab) = tabs.iter().find(|t| t.id == active) {
                        if std::mem::take(&mut self.typing_new_tab) {
                            self.address.clear();
                        } else {
                            self.address.set(&tab.url);
                            self.focus_page = true;
                        }
                    }
                    self.active = active;
                }
                self.tabs = tabs;
            },
            Msg::News(News::Control(control)) => {
                // The same question again (a tab shown anew) keeps what was typed.
                if self.control.as_ref().map(|a| a.control.id()) != control.as_ref().map(Control::id) {
                    self.focus_page |= self.control.is_some() && control.is_none();
                    self.control = control.map(Asking::new);
                }
            },
            Msg::News(News::Note(note)) => self.note(note),
            Msg::News(News::Zoom(percent)) => self.zoom = percent,
            Msg::News(News::Bookmarked(on)) => self.bookmarked = on,
            // The page's text, for the bar that asked. An answer to any other request is an old one, and so is one that comes
            // when no bar is up: dropped here, never kept for later.
            Msg::News(News::PageText { id, blocks, partial }) => {
                if self.find_open && self.find_pending && id == self.find_ask {
                    self.find_blocks = blocks;
                    self.find_partial = partial;
                    self.find_pending = false;
                    self.find_ready = true;
                    self.find_search();
                }
            },
            Msg::News(News::BookmarkList { id, entries }) => {
                if self.bookmarks_open && id == self.bookmarks_ask {
                    self.bookmarks = entries;
                    self.bookmarks_ready = true;
                    self.bookmarks_sel = 0;
                }
            },
            Msg::News(News::HistoryList { id, entries }) => {
                if self.history_open && id == self.history_ask {
                    self.history = entries;
                    self.history_ready = true;
                    self.history_sel = 0;
                }
            },
            Msg::News(News::BookmarkHits { id, entries }) => {
                self.omni.bookmarks_answered(id, entries, now_us());
                self.complete_inline();
            },
            Msg::News(News::HistoryHits { id, entries }) => {
                self.omni.answered(id, entries, now_us());
                self.complete_inline();
            },
            Msg::News(News::Download {
                id,
                name,
                received,
                total,
                state,
            }) => self.download_news(id, name, received, total, state),
            Msg::Script(line) => {
                self.waiting = Some((line.clone(), std::time::Instant::now()));
                self.script(&line);
            },
        }
    }

    fn view(&mut self, ui: &mut Ui) {
        let t = ui.theme();
        let l = self.row_layout(ui);
        // One row, in both chromes, and the engine is told exactly its height: criterion 7 of `one-bar.md`.
        let band = ui.cut_top(l.band_h.round() as i32);
        self.layout = l.clone();
        self.page_rect = ui.area();
        self.scale = t.k;
        if std::mem::take(&mut self.focus_page) {
            ui.focus(PAGE);
        }
        self.shortcuts(ui);
        self.omnibox_keys(ui);
        self.omnibox_click(ui);
        if let Some(Input::Up(..)) = ui.input()
            && std::mem::take(&mut self.swallow_up)
        {
            ui.take_input();
        }
        shown_downloads(&mut self.downloads, Instant::now());
        // The strip is over the page while a download runs or has just ended, and for as long as the command has the
        // panel up - one row of the note when there is nothing in it, the rows themselves when there is.
        self.strip = (!self.downloads.is_empty() || self.downloads_open || self.history_open || self.bookmarks_open).then(|| {
            // The history panel, while it is up, has the strip to itself: its rows or its one line.
            let rows = if self.history_open {
                self.history.len().clamp(1, HISTORY_ROWS)
            } else if self.bookmarks_open {
                self.bookmarks.len().clamp(1, HISTORY_ROWS)
            } else {
                self.downloads.len().clamp(1, DOWNLOAD_ROWS)
            } as i32;
            let h = rows * ui.px(DOWNLOAD_ROW_PX) + ui.px(8.0);
            let page = self.page_rect;
            Rect::new(page.x, page.bottom() - h, page.w, h)
        });
        // The find strip is a row along the top of the page while the bar is up: one row in both chromes, over the page.
        self.find_rect = self.find_open.then(|| {
            let page = self.page_rect;
            Rect::new(page.x, page.y, page.w, ui.px(FIND_STRIP_PX))
        });
        if self.control.is_none() {
            self.history_keys(ui);
            self.bookmarks_keys(ui);
            self.find_keys(ui);
        }
        if self.control.is_some() {
            self.control_input(ui);
        } else {
            self.page_input(ui);
        }
        // Whether the field has the keys: the row is laid out for it (`editing` in `row_layout`) and the classic field
        // either lets the toolkit draw (editing) or shows its host centred (not). The glass paints the row over the
        // band at the end of the pass either way.
        let editing = ui.is_focused(FIELD);
        let field = self.bar(ui, &l, band);
        let box_rect = if editing || self.chrome.mode == Mode::Glass { field } else { Rect::new(field.x, field.y, 0, 0) };
        // The hint follows the engines the omnibox holds: read again every pass, so an engine that appears or goes is
        // said by the next frame rather than frozen at the window's start. The glass takes the same string from the
        // engines in `glass_state`, so neither chrome can show a hint the other does not.
        let hint = address::placeholder_for(&self.omni.engines).to_owned();
        self.address.placeholder = hint;
        // The field takes the key (and its modifiers with it), so Shift is read before it does.
        let shifted = shift_return(ui.input());
        let typed = ui.text_field_at(box_rect, FIELD, &mut self.address);
        if typed.changed {
            let text = self.address.text.clone();
            let at_end = self.address.cursor == text.len() && self.address.anchor == self.address.cursor;
            if let Some(query) = self.omni.typed(&text, self.deleting, at_end, now_us()) {
                self.send(query);
            }
        }
        if typed.submitted && shifted {
            // Shift-Return: the text as typed (no row the arrows moved to, no inline completion) to the second engine.
            let text = self.omni.literal(&self.address.text).to_owned();
            self.go_shift(&text);
            ui.focus(PAGE);
        } else if typed.submitted {
            // A row the arrows moved to is what Enter opens; on the first row it is the field's text.
            match self.omni.dropdown.as_ref().and_then(Dropdown::chosen) {
                Some(go) => self.open(go),
                None => {
                    let text = self.address.text.clone();
                    // A completion left in place opens the origin as stored; any other text is what Enter classifies.
                    match self.omni.completed_go(&text, now_us()) {
                        Some(go) => self.open(go),
                        None => self.go(&text),
                    }
                },
            }
            ui.focus(PAGE);
        }
        self.typing = ui.is_focused(FIELD);
        if !self.typing {
            self.omni.close();
        }
        if self.chrome.mode == Mode::Classic {
            self.draw_classic_field(ui, &l, self.typing);
        }
        // The list hangs from the field's bottom edge, the capsule's width, at most eight rows (`one-bar.md`),
        // and the row is never drawn over: the band is above it.
        let rows = self.omni.dropdown.as_ref().map_or(0, |d| d.rows().len().min(chrome::DROPDOWN_ROWS) as i32);
        let drop = l.field_capsule().map_or(field, |cap| rect(&cap));
        self.omni_at = (rows > 0).then(|| Rect::new(drop.x, drop.bottom(), drop.w, rows * ui.px(OMNI_ROW_PX)));

        // The page takes the rest of the screen, at the toolkit's scale.
        if self.engine.is_none() {
            let tell = self.tell.clone();
            self.engine = Some((self.start)(Starting {
                width: self.page_rect.w as u32,
                height: self.page_rect.h as u32,
                scale: t.k,
                first: self.first.clone(),
                page: self.page.clone(),
                news: Box::new(move |news| tell(news)),
            }));
        }
        {
            let page = self.page.lock().unwrap();
            if page.w > 0 {
                ui.pixels(self.page_rect, page.w as usize, &page.px);
            }
        }
        if self.chrome.mode == Mode::Glass && ui.painting() {
            self.draw_glass(ui, &l, band);
        }
        if let Some(reason) = &self.crashed {
            ui.within(self.page_rect, |ui| {
                ui.heading("This page crashed");
                ui.paragraph(reason);
            });
        }
        self.downloads_strip(ui);
        self.find_strip(ui);
        self.draw_control(ui);
        self.omnibox_draw(ui);
    }
}

impl Asking {
    fn new(control: Control) -> Asking {
        let mut text = TextEdit::new();
        if let Control::Dialog { kind: DialogKind::Prompt(value), .. } = &control {
            text.set(value);
        }
        let menu = match &control {
            Control::Select { items, selected, at, .. } => Some(Menu::new(items.clone(), selected, *at)),
            Control::Dialog { .. } => None,
        };
        Asking {
            control,
            text,
            menu,
            fresh: true,
        }
    }
}

/// What a key does to a simple dialog: `Some(true)` is OK, `Some(false)` Cancel.
pub fn dialog_key(code: u16) -> Option<bool> {
    match code {
        keys::KEY_ENTER | keys::KEY_KPENTER => Some(true),
        keys::KEY_ESC => Some(false),
        _ => None,
    }
}

/// The most rows a `<select>`'s list shows at once; it scrolls for the rest.
const MENU_ROWS: usize = 12;

/// A `<select>`'s list: its rows, the highlighted one, and the first row in view.
pub struct Menu {
    items: Vec<SelectItem>,
    selected: Vec<usize>,
    highlight: Option<usize>,
    top: usize,
    /// The element, in the page's pixels.
    at: (i32, i32, i32, i32),
    /// Where the list is on the screen, and its row height, from the last draw.
    rect: Rect,
    row: i32,
}

impl Menu {
    pub fn new(items: Vec<SelectItem>, selected: &[usize], at: (i32, i32, i32, i32)) -> Menu {
        let current = items
            .iter()
            .position(|item| matches!(item, SelectItem::Option { id, .. } if selected.contains(id)));
        let mut menu = Menu {
            highlight: None,
            items,
            selected: selected.to_vec(),
            top: 0,
            at,
            rect: Rect::new(0, 0, 0, 0),
            row: 1,
        };
        menu.highlight = current.or_else(|| menu.next_choosable(0, 1));
        menu.keep_in_view();
        menu
    }

    fn choosable(&self, i: usize) -> bool {
        matches!(self.items.get(i), Some(SelectItem::Option { disabled: false, .. }))
    }

    /// The first choosable row from `from` going `dir` (1 or -1), `from` included.
    fn next_choosable(&self, from: usize, dir: i32) -> Option<usize> {
        let mut i = from as i64;
        while i >= 0 && (i as usize) < self.items.len() {
            if self.choosable(i as usize) {
                return Some(i as usize);
            }
            i += dir as i64;
        }
        None
    }

    /// Moves the highlight `by` rows, landing on a choosable one; stays put if there is none that way.
    fn step(&mut self, by: i32) {
        let dir = by.signum();
        let Some(at) = self.highlight else {
            self.highlight = self.next_choosable(0, 1);
            return;
        };
        let target = (at as i64 + by as i64).clamp(0, self.items.len() as i64 - 1) as usize;
        self.highlight = self
            .next_choosable(target, dir)
            .or_else(|| self.next_choosable(target, -dir))
            .or(Some(at));
        self.keep_in_view();
    }

    fn keep_in_view(&mut self) {
        if let Some(h) = self.highlight {
            if h < self.top {
                self.top = h;
            } else if h >= self.top + MENU_ROWS {
                self.top = h + 1 - MENU_ROWS;
            }
        }
    }

    /// What a key does: `Some` answers the page, `None` only moved the highlight (or did nothing).
    pub fn key(&mut self, code: u16) -> Option<Answer> {
        match code {
            keys::KEY_ENTER | keys::KEY_KPENTER | keys::KEY_SPACE => {
                return Some(self.chosen().map_or(Answer::Cancel, Answer::Chose));
            },
            keys::KEY_ESC => return Some(Answer::Cancel),
            keys::KEY_DOWN => self.step(1),
            keys::KEY_UP => self.step(-1),
            keys::KEY_PAGEDOWN => self.step(MENU_ROWS as i32),
            keys::KEY_PAGEUP => self.step(-(MENU_ROWS as i32)),
            keys::KEY_HOME => {
                self.highlight = self.next_choosable(0, 1).or(self.highlight);
                self.keep_in_view();
            },
            keys::KEY_END => {
                self.highlight = self.next_choosable(self.items.len().saturating_sub(1), -1).or(self.highlight);
                self.keep_in_view();
            },
            _ => {},
        }
        None
    }

    /// Highlights the first choosable option with this label.
    fn highlight_label(&mut self, label: &str) {
        let found = (0..self.items.len())
            .find(|&i| self.choosable(i) && matches!(&self.items[i], SelectItem::Option { label: l, .. } if l == label));
        if found.is_some() {
            self.highlight = found;
            self.keep_in_view();
        }
    }

    /// The option id of the highlighted row.
    pub fn chosen(&self) -> Option<usize> {
        match self.items.get(self.highlight?) {
            Some(SelectItem::Option { id, disabled: false, .. }) => Some(*id),
            _ => None,
        }
    }

    pub fn scroll(&mut self, rows: i32) {
        let most = self.items.len().saturating_sub(MENU_ROWS) as i64;
        self.top = (self.top as i64 + rows as i64).clamp(0, most) as usize;
    }

    fn row_at(&self, y: i32) -> Option<usize> {
        if y < self.rect.y || self.row <= 0 {
            return None;
        }
        let i = self.top + ((y - self.rect.y) / self.row) as usize;
        (i < self.items.len()).then_some(i)
    }

    /// Highlights the choosable row under the pointer; true if that changed it.
    fn hover(&mut self, y: i32) -> bool {
        match self.row_at(y) {
            Some(i) if self.choosable(i) && self.highlight != Some(i) => {
                self.highlight = Some(i);
                true
            },
            _ => false,
        }
    }

    /// The option id under the pointer, if that row can be chosen.
    fn at(&self, y: i32) -> Option<usize> {
        let i = self.row_at(y)?;
        match &self.items[i] {
            SelectItem::Option { id, disabled: false, .. } => Some(*id),
            _ => None,
        }
    }

    /// Under the element, or over it when there is no room below; inside the page either way.
    fn place(&mut self, ui: &Ui, page: Rect) {
        self.row = ui.px(28.0);
        let rows = self.items.len().clamp(1, MENU_ROWS) as i32;
        let h = (self.row * rows).min(page.h);
        let w = self.at.2.max(ui.px(160.0)).min(page.w);
        let x = (page.x + self.at.0).clamp(page.x, page.right() - w);
        let below = page.y + self.at.1 + self.at.3;
        let y = if below + h <= page.bottom() {
            below
        } else {
            (page.y + self.at.1 - h).max(page.y)
        };
        self.rect = Rect::new(x, y, w, h);
    }

    fn draw(&self, ui: &mut Ui) {
        let t = ui.theme();
        let px = t.pt(15.0);
        let r = self.rect;
        ui.fill(Rect::new(r.x - 1, r.y - 1, r.w + 2, r.h + 2), t.accent);
        ui.fill(r, t.panel);
        let pad = ui.px(10.0);
        ui.within(r, |ui| {
            for (n, item) in self.items.iter().enumerate().skip(self.top).take(MENU_ROWS) {
                let row = Rect::new(r.x, r.y + (n - self.top) as i32 * self.row, r.w, self.row);
                if self.highlight == Some(n) {
                    ui.fill(row, t.selection);
                }
                match item {
                    SelectItem::Group(label) => {
                        let text = Rect::new(row.x + pad, row.y, row.w - pad * 2, row.h);
                        ui.text_in(text, Face::Bold, px, t.dim, label);
                    },
                    SelectItem::Option { id, label, disabled } => {
                        // The current choice has a tick; options in a group sit under its label.
                        let indent = if self.items[..n].iter().any(|i| matches!(i, SelectItem::Group(_))) {
                            pad * 3
                        } else {
                            pad * 2
                        };
                        if self.selected.contains(id) {
                            ui.text_in(Rect::new(row.x + pad / 2, row.y, indent, row.h), Face::Sans, px, t.accent, "\u{2713}");
                        }
                        let text = Rect::new(row.x + indent, row.y, row.w - indent - pad, row.h);
                        ui.text_in(text, Face::Sans, px, if *disabled { t.dim } else { t.text }, label);
                    },
                }
            }
        });
    }
}

/// A placed part of the row, which `onebar.rs` already gave in physical pixels, as the toolkit's rectangle: edges rounded,
/// so boxes that touch still touch.
fn rect(b: &Placed) -> Rect {
    let (x0, y0) = (b.x.round() as i32, b.y.round() as i32);
    Rect::new(x0, y0, (b.right().round() as i32) - x0, (b.bottom().round() as i32) - y0)
}

/// What the field's left end shows: `one-bar.md`'s lock or warning while nobody is editing, its search glass while
/// someone is. The glass chrome makes the same three-way choice on its own canvas (`glass.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    /// The closed padlock: the address came over a scheme encrypted on the wire.
    Secure,
    /// The warning triangle: it did not.
    NotSecure,
    /// The search glass, while the field holds what the person typed.
    Search,
}

impl Mark {
    /// The mark for the address the field is showing.
    fn of(editing: bool, address: &str) -> Mark {
        match (editing, security_of(address)) {
            (true, _) => Mark::Search,
            (false, Security::Secure) => Mark::Secure,
            (false, Security::NotSecure) => Mark::NotSecure,
        }
    }
}

/// The kit's `color::WARNING`, which the classic theme has no answer to: its `danger` is the red of a refused action, and
/// a page that arrived in the clear is neither. Both chromes mark it in this amber.
const WARN: Rgb = Rgb::hex(0xff9f0a);

/// `a` towards `b` by `f` (0 to 1), per channel.
fn mix(a: Rgb, b: Rgb, f: f32) -> Rgb {
    let ch = |shift: u32| {
        let (x, y) = (((a.0 >> shift) & 0xff) as f32, ((b.0 >> shift) & 0xff) as f32);
        ((x + (y - x) * f.clamp(0.0, 1.0)).round() as u32) & 0xff
    };
    Rgb::hex((ch(16) << 16) | (ch(8) << 8) | ch(0))
}

/// The field's left-end mark, in fills: DejaVu has no padlock, no warning and no search glass, and the classic chrome has
/// no canvas to outline one on. `back` is what the mark sits on, so the shackle, the ring and the exclamation read as
/// holes. Measures are in `u`, a twelfth of the box: the mark is the glass chrome's 18-point icon's size, not the 28
/// points of the box it is placed in, and filling the box is what made it read as a blob rather than a lock.
fn field_mark(ui: &mut Ui, r: Rect, mark: Mark, ink: Rgb, back: Rgb) {
    let u = (r.h.min(r.w) / 12).max(2);
    let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
    match mark {
        Mark::Search => {
            // The glass: a 5u ring one u thick, its hole 3u across.
            let (d, hole) = (5 * u, 3 * u);
            ui.fill_round(Rect::new(cx - d / 2, cy - d / 2 - u, d, d), d / 2, ink);
            ui.fill_round(Rect::new(cx - hole / 2, cy - hole / 2 - u, hole, hole), hole / 2, back);
            // The handle steps down the ring's own diagonal, one u per step and overlapping the last by half: starting on
            // the ring's ink rather than beside it is what makes this a magnifier rather than a ring with a dot near it.
            for i in 0..3 {
                ui.fill(Rect::new(cx + 3 * u / 2 + i * u, cy + u / 2 + i * u, u + u / 2, u + u / 2), ink);
            }
        },
        Mark::Secure => {
            // Closed rather than hooked: the shackle is one 4u-wide pill whose bottom reaches the body's top edge, and the
            // hole cut inside it stops at that same edge - so both legs end on the body and the arch opens onto it. The
            // body is filled last, over the bottom of both, from `cy - u` down.
            ui.fill_round(Rect::new(cx - 2 * u, cy - 4 * u, 4 * u, 4 * u), 2 * u, ink);
            ui.fill_round(Rect::new(cx - u, cy - 3 * u, 2 * u, 3 * u), u, back);
            ui.fill_round(Rect::new(cx - 3 * u, cy - u, 6 * u, 5 * u), u, ink);
        },
        Mark::NotSecure => {
            // The triangle, stepped row by row because the toolkit fills rectangles and no polygon: 7u wide at its base,
            // 7u tall, its bottom edge on the lock's. The exclamation is cut out of it in `back`, its stem clear of both
            // sloped sides and a row of amber left under the dot.
            let (top, h, half) = (cy - 3 * u, 7 * u, 7 * u / 2);
            for row in 0..h {
                let w = (half * (row + 1) + h / 2) / h;
                ui.fill(Rect::new(cx - w, top + row, 2 * w, 1), WARN);
            }
            let stem = u.max(2);
            ui.fill(Rect::new(cx - stem / 2, top + 2 * u, stem, 2 * u), back);
            ui.fill(Rect::new(cx - stem / 2, top + 5 * u, stem, u), back);
        },
    }
}

/// A tab's label: its title, else its address, else that it is new.
fn tab_label(tab: &TabInfo) -> String {
    let name = if !tab.title.is_empty() {
        tab.title.as_str()
    } else if !tab.url.is_empty() && tab.url != "about:blank" {
        tab.url.as_str()
    } else {
        "New tab"
    };
    if tab.loading { format!("\u{21bb} {name}") } else { name.to_owned() }
}

/// Whether the key the field is about to take is Return with Shift and no other modifier. The text field answers only
/// "submitted", so this reads the pending key before the field does.
fn shift_return(input: Option<Input>) -> bool {
    matches!(input, Some(Input::Key(press))
        if matches!(press.code, keys::KEY_ENTER | keys::KEY_KPENTER)
            && press.mods.shift && !press.mods.ctrl && !press.mods.alt && !press.mods.meta)
}

fn named(name: &str) -> Option<Key> {
    let key = match name {
        "enter" => NamedKey::Enter,
        "pagedown" => NamedKey::PageDown,
        "pageup" => NamedKey::PageUp,
        "down" => NamedKey::ArrowDown,
        "up" => NamedKey::ArrowUp,
        "tab" => NamedKey::Tab,
        _ => return None,
    };
    Some(Key::Named(key))
}

fn to_key(press: Press) -> Option<(Key, Modifiers)> {
    let mut modifiers = Modifiers::empty();
    modifiers.set(Modifiers::SHIFT, press.mods.shift);
    modifiers.set(Modifiers::CONTROL, press.mods.ctrl);
    modifiers.set(Modifiers::ALT, press.mods.alt);
    modifiers.set(Modifiers::META, press.mods.meta);
    let key = match press.code {
        keys::KEY_ENTER | keys::KEY_KPENTER => NamedKey::Enter,
        keys::KEY_BACKSPACE => NamedKey::Backspace,
        keys::KEY_DELETE => NamedKey::Delete,
        keys::KEY_TAB => NamedKey::Tab,
        keys::KEY_ESC => NamedKey::Escape,
        keys::KEY_UP => NamedKey::ArrowUp,
        keys::KEY_DOWN => NamedKey::ArrowDown,
        keys::KEY_LEFT => NamedKey::ArrowLeft,
        keys::KEY_RIGHT => NamedKey::ArrowRight,
        keys::KEY_HOME => NamedKey::Home,
        keys::KEY_END => NamedKey::End,
        keys::KEY_PAGEUP => NamedKey::PageUp,
        keys::KEY_PAGEDOWN => NamedKey::PageDown,
        code => {
            let c = keys::char_for(code, press.mods)?;
            return Some((Key::Character(c.to_string()), modifiers));
        },
    };
    Some((Key::Named(key), modifiers))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use lucina_app::Harness;

    const KEY_L: u16 = 38;

    use super::*;

    static SENT: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// The fake engine's log is one, so the tests that read it take turns.
    static SERIAL: Mutex<()> = Mutex::new(());

    struct Fake;

    impl Engine for Fake {
        fn send(&self, cmd: Cmd) {
            SENT.lock().unwrap().push(format!("{cmd:?}"));
        }
    }

    fn fake(starting: Starting) -> Box<dyn Engine> {
        let (w, h) = (starting.width, starting.height);
        let mut page = starting.page.lock().unwrap();
        page.w = w;
        page.h = h;
        page.px = vec![0x00ff_ffff; (w * h) as usize];
        Box::new(Fake)
    }

    fn sent() -> Vec<String> {
        std::mem::take(&mut *SENT.lock().unwrap())
    }

    /// One test, because the fake engine's log is one.
    #[test]
    fn the_window_gives_the_page_its_input_and_the_bar_its_own() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let first = Url::parse("https://example.com/").unwrap();
        let browser = Browser::with_news(Arc::new(|_| {}), first, fake);
        let mut h = Harness::new(browser, 1920, 1080);
        if let Ok(path) = std::env::var("LUCINANET_TEST_PPM") {
            std::fs::write(path, h.ppm()).unwrap();
        }
        sent();

        // A new window's keys go to the page.
        h.key(keys::KEY_PAGEDOWN);
        assert!(sent().iter().any(|c| c.contains("PageDown")));

        // The wheel and a click, in the page's own coordinates.
        let top = h.app().page_rect.y;
        h.wheel(-2);
        assert!(sent().iter().any(|c| c.starts_with("Wheel")));
        h.click(400, top + 100);
        let log = sent();
        assert!(log.iter().any(|c| *c == "Button { down: true, x: 400.0, y: 100.0 }"), "{log:?}");
        assert!(log.iter().any(|c| *c == "Button { down: false, x: 400.0, y: 100.0 }"), "{log:?}");

        // Ctrl+L, a host, Enter: the bar takes the typing and loads it; afterwards the keys are the page's again.
        h.press(KEY_L, keys::Mods { ctrl: true, ..keys::Mods::default() });
        h.type_str("wikipedia.org");
        assert!(!sent().iter().any(|c| c.starts_with("Key")));
        h.key(keys::KEY_ENTER);
        assert!(sent().iter().any(|c| c.starts_with("Load") && c.contains("wikipedia.org")));
        h.key(keys::KEY_DOWN);
        assert!(sent().iter().any(|c| c.contains("ArrowDown")));

        // Back is greyed out by having nowhere to go; with history it goes.
        let (bar_x, bar_y) = (top / 2, top / 2);
        h.click(bar_x, bar_y);
        assert!(!sent().iter().any(|c| c == "Back"));
        h.app().back = true;
        h.click(bar_x, bar_y);
        assert!(sent().iter().any(|c| c == "Back"));

        // A download puts a strip over the bottom of the page; its × cancels it, and a click
        // there does not reach the page.
        assert!(h.app().strip.is_none());
        h.app().message(Msg::News(News::Download {
            id: 7,
            name: "report.pdf".into(),
            received: 1 << 20,
            total: Some(4 << 20),
            state: DownloadState::Running,
        }));
        h.wheel(0);
        sent();
        let strip = h.app().strip.expect("a strip while a download runs");
        let page = h.app().page_rect;
        assert_eq!(strip.bottom(), page.bottom());
        let scale = h.app().scale;
        let px = |v: f32| (v * scale).round() as i32;
        let (gap, side) = (px(8.0), px(DOWNLOAD_ROW_PX) - px(8.0));
        h.click(strip.right() - gap * 2 - side / 2, strip.y + strip.h / 2);
        let log = sent();
        assert!(log.iter().any(|c| c == "CancelDownload(7)"), "{log:?}");
        assert!(!log.iter().any(|c| c.starts_with("Button")), "{log:?}");

        // Ended, it stays to say where the file went; the page still gets clicks above it.
        h.app().message(Msg::News(News::Download {
            id: 7,
            name: "report.pdf".into(),
            received: 4 << 20,
            total: Some(4 << 20),
            state: DownloadState::Done("/tmp/dl/report.pdf".into()),
        }));
        h.wheel(0);
        assert!(h.app().strip.is_some());
        assert_eq!(h.app().downloads[0].status(), "Saved to /tmp/dl/report.pdf");
        h.click(400, strip.y - 10);
        assert!(sent().iter().any(|c| c.starts_with("Button")));
    }

    fn row(id: u64, ended: Option<Instant>) -> DownloadRow {
        DownloadRow {
            id,
            name: format!("{id}.zip"),
            received: 0,
            total: None,
            state: if ended.is_some() {
                DownloadState::Failed("Cancelled".into())
            } else {
                DownloadState::Running
            },
            ended,
        }
    }

    #[test]
    fn ended_downloads_go_after_ten_seconds_running_ones_stay() {
        let now = Instant::now();
        let mut rows = vec![
            row(1, None),
            row(2, Some(now - Duration::from_secs(11))),
            row(3, Some(now - Duration::from_secs(4))),
        ];
        let next = shown_downloads(&mut rows, now);
        assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(next, Some(now + Duration::from_secs(6)));
        assert_eq!(shown_downloads(&mut vec![row(1, None)], now), None);
    }

    #[test]
    fn download_sizes_read_as_people_say_them() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(3 << 10), "3 KB");
        assert_eq!(bytes(5 << 20 | 1 << 19), "5.5 MB");
        assert_eq!(bytes(2 << 30), "2.0 GB");
        let mut running = row(1, None);
        running.received = 1 << 20;
        assert_eq!(running.status(), "1.0 MB");
        running.total = Some(4 << 20);
        assert_eq!(running.status(), "1.0 MB of 4.0 MB");
    }

    fn ctrl(shift: bool) -> keys::Mods {
        keys::Mods { ctrl: true, shift, ..keys::Mods::default() }
    }

    fn window() -> Harness<Browser> {
        let first = Url::parse("https://example.com/").unwrap();
        let classic = chrome::Settings { mode: Mode::Classic, left_inset: 0.0 };
        let mut h = Harness::new(Browser::with_chrome(Arc::new(|_| {}), first, fake, classic), 1920, 1080);
        h.press(KEY_L, ctrl(false));
        h.key(keys::KEY_ESC);
        tell(&mut h, News::Tabs { tabs: tabs(2), active: 1 });
        sent();
        h
    }

    fn tell(h: &mut Harness<Browser>, news: News) {
        h.core.message(Msg::News(news));
        h.core.paint();
    }

    fn tabs(n: u32) -> Vec<TabInfo> {
        (1..=n)
            .map(|id| TabInfo {
                id,
                title: format!("Page {id}"),
                url: format!("https://example.com/{id}"),
                registrable_domain: Some("example.com".into()),
                loading: false,
            })
            .collect()
    }

    #[test]
    fn tabs_answer_their_keys_and_the_strip() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        h.press(crate::tabs::KEY_TAB, ctrl(false));
        assert_eq!(sent(), ["SelectTab(2)"]);
        // Two tabs: the previous one wraps around to the second too.
        h.press(crate::tabs::KEY_TAB, ctrl(true));
        assert_eq!(sent(), ["SelectTab(2)"]);
        h.press(crate::tabs::KEY_W, ctrl(false));
        assert_eq!(sent(), ["CloseTab(1)"]);

        // The engine shows the second: its address, and the keys are its page's.
        tell(&mut h, News::Tabs { tabs: tabs(2), active: 2 });
        assert_eq!(h.app().address.text, "https://example.com/2");
        h.key(keys::KEY_DOWN);
        assert!(sent().iter().any(|c| c.contains("ArrowDown")));

        // A new tab takes the typing in its address field.
        h.press(crate::tabs::KEY_T, ctrl(false));
        assert_eq!(sent(), ["NewTab(None)"]);
        tell(&mut h, News::Tabs { tabs: tabs(3), active: 3 });
        tell(&mut h, News::Url("about:blank".into()));
        h.type_str("wiki.org");
        assert!(!sent().iter().any(|c| c.starts_with("Key")));
        h.key(keys::KEY_ENTER);
        assert!(sent().iter().any(|c| c.starts_with("Load") && c.contains("wiki.org")));

        // The row: a click in a tab's own space shows it, on its close mark closes it.
        let tab = placed(&mut h, Part::Tab(0));
        h.click(centre(&tab).0 - ui_px(12.0), centre(&tab).1);
        assert_eq!(sent(), ["SelectTab(1)"]);
        let (x, y) = close_centre(&tab);
        h.click(x, y);
        assert_eq!(sent(), ["CloseTab(1)"]);

        // Eight is all: the ninth is refused with a note, and nothing reaches the engine.
        tell(&mut h, News::Tabs { tabs: tabs(MAX_TABS as u32), active: 1 });
        h.press(crate::tabs::KEY_T, ctrl(false));
        assert!(sent().is_empty());
        assert!(h.app().note.as_ref().is_some_and(|(n, _)| n.contains("8")));
    }

    #[test]
    fn shortcuts_run_the_commands_the_window_has_and_take_the_ones_it_has_not() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        let alt = keys::Mods { alt: true, ..keys::Mods::default() };

        // Zoom, in all the spellings of the keys, goes to the engine as steps.
        h.press(code::EQUAL, ctrl(false));
        assert_eq!(sent(), ["Zoom(In)"]);
        h.press(code::KP_PLUS, ctrl(false));
        assert_eq!(sent(), ["Zoom(In)"]);
        h.press(code::MINUS, ctrl(false));
        assert_eq!(sent(), ["Zoom(Out)"]);
        h.press(code::N0, ctrl(false));
        assert_eq!(sent(), ["Zoom(Reset)"]);

        // Reload, by Ctrl+R and by F5.
        h.press(code::R, ctrl(false));
        assert_eq!(sent(), ["Reload"]);
        h.key(code::F5);
        assert_eq!(sent(), ["Reload"]);

        // Alt+2 and Alt+9 pick tabs by position; the shown one is not asked for again.
        h.press(code::N2, alt);
        assert_eq!(sent(), ["SelectTab(2)"]);
        h.press(code::N9, alt);
        assert_eq!(sent(), ["SelectTab(2)"]);
        h.press(code::N1, alt);
        assert!(sent().is_empty());
        h.press(code::N7, alt);
        assert!(sent().is_empty());

        // Back and forward go only where the buttons would.
        h.press(code::LEFT, alt);
        assert!(sent().is_empty());
        h.app().back = true;
        h.press(code::LEFT, alt);
        assert_eq!(sent(), ["Back"]);
        h.app().forward = true;
        h.press(code::RIGHT, alt);
        assert_eq!(sent(), ["Forward"]);

        // Ctrl+K and Alt+D go to the address bar: typing then reaches the field, not the page.
        h.press(code::K, ctrl(false));
        h.type_str("x");
        assert!(!sent().iter().any(|c| c.starts_with("Key")));
        assert_eq!(h.app().address.text, "x");

        // A shortcut with no feature behind it is taken: the page never sees Ctrl+Shift+T or Alt+Home
        // (Ctrl+H moved to the wired ones with the history panel: ; Ctrl+Shift+O with the bookmarks panel: ).
        let mut h = window();
        h.press(code::T, ctrl(true));
        h.press(code::HOME, alt);
        assert!(sent().is_empty());
        // What is not a shortcut is the page's: Ctrl+C, and Esc (nothing is loading).
        h.press(code::C, ctrl(false));
        assert!(sent().iter().any(|c| c.starts_with("Key")));
        tell(&mut h, News::Loading(false));
        h.key(code::ESC);
        assert!(sent().iter().any(|c| c.contains("Escape")));
        // Esc while loading is the browser's Stop, which the engine cannot do yet: the page keeps it with no find
        // bar up.
        tell(&mut h, News::Loading(true));
        h.key(code::ESC);
        assert!(sent().iter().any(|c| c.contains("Escape")));
        // Ctrl+F is the browser's too, and opens the bar. The same Esc then belongs to the bar: taken, the page
        // never sees it, and the bar is down again.
        h.press(code::F, ctrl(false));
        assert!(!sent().iter().any(|c| c.starts_with("Key")), "the page never sees Ctrl+F (the bar asks the engine for the text: )");
        assert!(h.app().find_open, "and it opened the bar");
        h.key(code::ESC);
        assert!(sent().is_empty(), "the open bar took Esc, not the page");
        assert!(!h.app().find_open, "and the key closed the bar");
    }

    /// the downloads command opens the panel the chrome already draws from, and Esc is the panel's own key.
    /// Windowless, as the rest of them are: the keys go in through the harness, the panel is read from the state both
    /// chromes draw it from (`strip` over the page and the rows `shown_rows` picks for it), and the fake engine's log
    /// says nothing was asked of it - the panel reads what the chrome holds.
    #[test]
    fn the_downloads_command_opens_the_panel_and_esc_closes_it() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        tell(&mut h, News::Loading(false));
        assert!(h.app().strip.is_none(), "no download and no command yet, so nothing over the page");

        // The chrome holds a running download. With no load in flight, Esc is still the page's while the panel is
        // down: the command below is what makes the key the browser's.
        tell(&mut h, News::Download {
            id: 7,
            name: "report.pdf".into(),
            received: 1 << 20,
            total: Some(4 << 20),
            state: DownloadState::Running,
        });
        sent();
        h.key(code::ESC);
        assert!(sent().iter().any(|c| c.contains("Escape")), "the page keeps Esc while the panel is down");

        // Ctrl+Shift+Y, the downloads key of the profile the keymap follows, puts the panel up.
        h.press(code::Y, ctrl(true));
        assert!(h.app().downloads_open, "the command opened the panel");
        assert!(sent().is_empty(), "the panel asks the engine for nothing");
        let strip = h.app().strip.expect("and the panel is drawn over the page");
        assert_eq!(strip.bottom(), h.app().page_rect.bottom(), "at the page's bottom edge, where the strip is");
        let rows = shown_rows(&h.app().downloads);
        assert_eq!(rows.len(), 1, "the one download the chrome holds");
        assert_eq!(rows[0].name, "report.pdf", "the row says which file");
        assert_eq!(rows[0].status(), "1.0 MB of 4.0 MB", "and how far it has come");

        // Esc is the panel's own close, taken with no load in flight: the page never sees it, and the panel is down.
        // The strip stays by its own rule, a download still running in it.
        h.key(code::ESC);
        assert!(sent().is_empty(), "the panel took the Esc the page had");
        assert!(!h.app().downloads_open, "Esc closed the panel");
        assert!(h.app().strip.is_some(), "the running download's strip is up on its own");

        // The key is a toggle: it opens the panel again and closes it once more.
        h.press(code::Y, ctrl(true));
        assert!(h.app().downloads_open, "the same key opens it again");
        h.press(code::Y, ctrl(true));
        assert!(!h.app().downloads_open, "and takes it down again");
    }

    /// The command shows the panel on demand, with nothing downloaded and nothing running: it says so in one plain
    /// line rather than drawing an empty band over the page.
    #[test]
    fn the_downloads_panel_says_so_when_there_is_nothing_to_show() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        assert!(h.app().downloads.is_empty(), "no download news at all");
        assert!(h.app().strip.is_none());

        h.press(code::Y, ctrl(true));
        assert!(h.app().downloads_open, "the panel is up with nothing in it");
        let strip = h.app().strip.expect("and it is drawn all the same");
        let k = h.app().scale;
        let px = |v: f32| (v * k).round() as i32;
        assert_eq!(strip.h, px(DOWNLOAD_ROW_PX) + px(8.0), "one row, not the three a full panel takes");
        assert_eq!(panel_note(&h.app().downloads), Some(NO_DOWNLOADS), "the line that one row says");
        assert_eq!(NO_DOWNLOADS, "No downloads", "in the words a person reads");
        assert!(shown_rows(&h.app().downloads).is_empty(), "and nothing to move over");
        h.key(keys::KEY_PAGEDOWN);
        assert!(sent().iter().any(|c| c.contains("PageDown")), "the panel is an overlay, not a new focus: the page has its keys");

        h.key(code::ESC);
        assert!(sent().is_empty(), "the page never sees the panel's Esc");
        assert!(!h.app().downloads_open, "and Esc takes the empty panel down again");
        assert!(h.app().strip.is_none(), "with no download of its own nothing is left over the page");
    }

    /// The quit reaches the end a window close ends by: `Core::quit_requested()` is what the desktop host reads and
    /// answers with the same `event_loop.exit()` a `WindowEvent::CloseRequested` gets, so the flag is the whole path
    /// from the window. The press is the browser's own: the engine is asked for nothing and the page sees no key.
    #[test]
    fn ctrl_q_requests_the_same_end_a_window_close_gives() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        assert!(!h.core.quit_requested());
        h.press(code::Q, ctrl(false));
        assert!(h.core.quit_requested());
        assert!(sent().is_empty());
    }

    #[test]
    fn a_zoomed_page_says_so_in_the_rows_own_space() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        use lucinanet_shortcuts::code;
        let mut h = window();
        // The row has no zoom button of its own (`one-bar.md` gives it back, forward, the tabs, the field, +, the
        // star and the menu): a zoomed page says so in the row's empty stretch, and the key puts it back.
        let (x, y) = centre(&placed(&mut h, Part::DragRegion));
        h.click(x, y);
        assert!(sent().is_empty(), "the readout is not a button, at 100 % or at any other");
        tell(&mut h, News::Zoom(150));
        assert_eq!(h.app().zoom, 150);
        h.click(x, y);
        assert!(sent().is_empty(), "nor is it one while the page is zoomed");
        h.press(code::N0, ctrl(false));
        assert_eq!(sent(), ["Zoom(Reset)"]);
    }

    #[test]
    fn the_star_toggles_the_bookmark_and_is_filled_while_the_page_is_bookmarked() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        let (x, y) = centre(&placed(&mut h, Part::Star));
        assert!(!h.app().bookmarked);
        h.click(x, y);
        assert_eq!(sent(), ["ToggleBookmark"]);
        // The window does not guess: it shows what the engine says.
        assert!(!h.app().bookmarked);
        tell(&mut h, News::Bookmarked(true));
        assert!(h.app().bookmarked);
        h.click(x, y);
        assert_eq!(sent(), ["ToggleBookmark"]);
        tell(&mut h, News::Bookmarked(false));
        assert!(!h.app().bookmarked);
    }

    #[test]
    fn ctrl_d_is_the_star_even_while_typing_in_the_field() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        h.press(code::D, ctrl(false));
        assert_eq!(sent(), ["ToggleBookmark"]);
        // In the address field, too: the press is taken, and no letter is typed.
        h.press(code::L, ctrl(false));
        h.press(code::D, ctrl(false));
        assert_eq!(sent(), ["ToggleBookmark"]);
        assert!(!h.app().address.text.ends_with('d'));
        // The page never sees Ctrl+D.
        assert!(!sent().iter().any(|c| c.starts_with("Key")));
        // Ctrl+Shift+T, reopen-closed-tab, is still taken and still nothing (Ctrl+Shift+O became the bookmarks panel: ).
        h.press(code::T, ctrl(true));
        assert!(sent().is_empty());
    }

    /// Reference pixels on the test's 1920x1080 window.
    fn ui_px(v: f32) -> i32 {
        lucina_app::Theme::new(1920, 1080).px(v)
    }

    fn dialog(id: u64, kind: DialogKind) -> News {
        News::Control(Some(Control::Dialog {
            id,
            origin: "https://example.com".into(),
            message: "Sure?".into(),
            kind,
        }))
    }

    #[test]
    fn a_dialog_takes_the_keys_and_answers_the_page() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        tell(&mut h, dialog(5, DialogKind::Confirm));
        // Nothing reaches the page while it asks.
        h.type_str("a");
        h.key(keys::KEY_PAGEDOWN);
        let top = h.app().page_rect.y;
        h.click(20, top + 600);
        h.wheel(-2);
        assert!(sent().is_empty());
        h.key(keys::KEY_ESC);
        assert_eq!(sent(), ["Answer(5, Cancel)"]);
        assert!(h.app().control.is_none());

        tell(&mut h, dialog(6, DialogKind::Alert));
        h.key(keys::KEY_ENTER);
        assert_eq!(sent(), ["Answer(6, Ok(None))"]);

        // A prompt starts with its text chosen, so typing replaces it.
        tell(&mut h, dialog(7, DialogKind::Prompt("old".into())));
        h.type_str("new");
        h.key(keys::KEY_ENTER);
        assert_eq!(sent(), ["Answer(7, Ok(Some(\"new\")))"]);

        // Another tab's question waits: the engine says the shown tab asks nothing, and the page has its keys.
        tell(&mut h, dialog(8, DialogKind::Confirm));
        tell(&mut h, News::Control(None));
        h.key(keys::KEY_DOWN);
        assert!(sent().iter().any(|c| c.contains("ArrowDown")));

        // Scripted, as the headless checks drive it.
        tell(&mut h, dialog(9, DialogKind::Prompt(String::new())));
        h.core.message(Msg::Script("dialog text hello".into()));
        h.core.message(Msg::Script("dialog ok".into()));
        assert_eq!(sent(), ["Answer(9, Ok(Some(\"hello\")))"]);
        tell(&mut h, dialog(10, DialogKind::Confirm));
        h.core.message(Msg::Script("dialog cancel".into()));
        assert_eq!(sent(), ["Answer(10, Cancel)"]);
    }

    fn options() -> Vec<SelectItem> {
        let option = |id, label: &str, disabled| SelectItem::Option { id, label: label.into(), disabled };
        vec![
            option(0, "none", false),
            SelectItem::Group("Fruit".into()),
            option(1, "apple", false),
            option(2, "pear", true),
            option(3, "plum", false),
        ]
    }

    #[test]
    fn a_select_list_moves_over_what_can_be_chosen() {
        let mut menu = Menu::new(options(), &[1], (0, 0, 100, 20));
        assert_eq!(menu.chosen(), Some(1));
        // The disabled pear is passed over, and the end stays the end.
        assert_eq!(menu.key(keys::KEY_DOWN), None);
        assert_eq!(menu.chosen(), Some(3));
        menu.key(keys::KEY_DOWN);
        assert_eq!(menu.chosen(), Some(3));
        // Up passes the pear and the group's label.
        menu.key(keys::KEY_UP);
        menu.key(keys::KEY_UP);
        assert_eq!(menu.chosen(), Some(0));
        menu.key(keys::KEY_END);
        assert_eq!(menu.key(keys::KEY_ENTER), Some(Answer::Chose(3)));
        assert_eq!(menu.key(keys::KEY_ESC), Some(Answer::Cancel));
        // Nothing chosen yet: the first that can be.
        assert_eq!(Menu::new(options(), &[], (0, 0, 1, 1)).chosen(), Some(0));
    }

    #[test]
    fn a_select_list_answers_a_click_or_cancels_outside() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        let select = |id| {
            News::Control(Some(Control::Select { id, at: (100, 50, 200, 30), items: options(), selected: vec![0] }))
        };
        tell(&mut h, select(3));
        let row = ui_px(28.0);
        let (x, y) = (h.app().page_rect.x + 150, h.app().page_rect.y + 80);
        // The fifth row is plum.
        h.click(x, y + row * 4 + row / 2);
        assert_eq!(sent(), ["Answer(3, Chose(3))"]);
        tell(&mut h, select(4));
        // The disabled row does nothing; a press elsewhere cancels and goes no further.
        h.click(x, y + row * 3 + row / 2);
        assert!(sent().is_empty());
        h.click(x + 900, y + 500);
        assert_eq!(sent(), ["Answer(4, Cancel)"]);
        tell(&mut h, select(5));
        h.key(keys::KEY_DOWN);
        h.key(keys::KEY_ENTER);
        assert_eq!(sent(), ["Answer(5, Chose(1))"]);
        // Scripted: by the option's label; a disabled one cannot be.
        tell(&mut h, select(6));
        h.core.message(Msg::Script("dialog text pear".into()));
        h.core.message(Msg::Script("dialog text plum".into()));
        h.core.message(Msg::Script("dialog ok".into()));
        assert_eq!(sent(), ["Answer(6, Chose(3))"]);
    }

    #[test]
    fn dialog_keys() {
        assert_eq!(dialog_key(keys::KEY_ENTER), Some(true));
        assert_eq!(dialog_key(keys::KEY_KPENTER), Some(true));
        assert_eq!(dialog_key(keys::KEY_ESC), Some(false));
        assert_eq!(dialog_key(keys::KEY_DOWN), None);
    }

    #[test]
    fn script_lines_drive_the_tabs() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        h.core.message(Msg::Script("newtab example.org".into()));
        assert!(sent().iter().any(|c| c.starts_with("NewTab(Some") && c.contains("example.org")));
        h.core.message(Msg::Script("newtab".into()));
        assert_eq!(sent(), ["NewTab(None)"]);
        h.core.message(Msg::Script("tab 2".into()));
        assert_eq!(sent(), ["SelectTab(2)"]);
        h.core.message(Msg::Script("closetab".into()));
        assert_eq!(sent(), ["CloseTab(1)"]);
    }

    fn entry(url: &str, title: &str) -> lucinanet_omnibox::HistoryEntry {
        lucinanet_omnibox::HistoryEntry {
            url: url.into(),
            title: title.into(),
            visit_count: 10,
            typed_count: 5,
            last_visit_us: now_us(),
        }
    }

    /// The window with the address field focused and `text` typed into it.
    fn typing(text: &str) -> Harness<Browser> {
        let mut h = window();
        h.press(KEY_L, ctrl(false));
        h.type_str(text);
        h
    }

    fn marked(url: &str, title: &str) -> lucinanet_omnibox::BookmarkEntry {
        lucinanet_omnibox::BookmarkEntry { url: url.into(), title: title.into(), folder_path: "Other Bookmarks".into() }
    }

    #[test]
    fn the_bookmarks_the_engine_found_are_rows_of_the_dropdown() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = typing("news");
        let id = h.app().omni.latest_query();
        tell(&mut h, News::BookmarkHits { id, entries: vec![marked("https://news.example.org/", "World News")] });
        let rows = h.app().omni.dropdown.as_ref().unwrap().rows().to_vec();
        assert!(rows.iter().any(|r| r.kind == lucinanet_omnibox::Kind::Bookmark && r.url.as_str() == "https://news.example.org/"), "{rows:?}");
        // An answer for text that has moved on is ignored.
        h.type_str("x");
        tell(&mut h, News::BookmarkHits { id, entries: vec![marked("https://old.example.org/", "Old")] });
        assert!(h.app().omni.dropdown.as_ref().unwrap().rows().iter().all(|r| r.url.as_str() != "https://old.example.org/"));
    }

    fn answer(h: &mut Harness<Browser>, entries: Vec<lucinanet_omnibox::HistoryEntry>) {
        let id = h.app().omni.latest_query();
        tell(h, News::HistoryHits { id, entries });
    }

    #[test]
    fn typing_in_the_bar_opens_the_dropdown_and_asks_the_engine_for_pages() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = typing("rust");
        let asked = sent();
        assert!(asked.last().unwrap().starts_with("HistoryQuery") && asked.last().unwrap().contains("\"rust\""), "{asked:?}");
        // The search row is there before the history answers.
        let rows = h.app().omni.dropdown.as_ref().unwrap().rows().len();
        assert_eq!(rows, 1);
        answer(&mut h, vec![entry("https://rust-lang.org/", "Rust Programming Language")]);
        assert_eq!(h.app().omni.dropdown.as_ref().unwrap().rows().len(), 2);
        // Clearing the field closes it and asks nothing.
        h.press(keys::KEY_A, ctrl(false));
        h.key(keys::KEY_BACKSPACE);
        assert!(h.app().omni.dropdown.is_none());
    }

    #[test]
    fn the_arrows_choose_a_row_and_enter_opens_it_as_a_typed_address() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = typing("rust");
        answer(&mut h, vec![entry("https://rust-lang.org/", "Rust Programming Language")]);
        sent();
        h.key(keys::KEY_DOWN);
        assert_eq!(h.app().address.text, "https://rust-lang.org/");
        h.key(keys::KEY_UP);
        assert_eq!(h.app().address.text, "rust");
        h.key(keys::KEY_DOWN);
        h.key(keys::KEY_ENTER);
        let loaded = sent();
        assert_eq!(loaded.len(), 1, "{loaded:?}");
        assert!(loaded[0].starts_with("LoadTyped") && loaded[0].contains("rust-lang.org") && loaded[0].contains("search: false"), "{loaded:?}");
        assert!(h.app().omni.dropdown.is_none());
        assert_eq!(h.app().address.text, "https://rust-lang.org/");
    }

    /// A window on the blank page shows an empty field (and its placeholder), not "about:blank".
    #[test]
    fn a_window_on_the_blank_page_has_an_empty_address_field() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let classic = chrome::Settings { mode: Mode::Classic, left_inset: 0.0 };
        let mut h = Harness::new(Browser::with_chrome(Arc::new(|_| {}), crate::address::blank_url(), fake, classic), 1920, 1080);
        assert!(h.app().address.text.is_empty(), "{:?}", h.app().address.text);
    }

    /// With no search engine, Enter on text that is not an address loads nothing, sends nothing, and says why.
    #[test]
    fn enter_on_a_search_with_no_engine_sends_nothing_and_says_so() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        h.app().omni.engines = crate::address::Engines::none();
        h.press(KEY_L, ctrl(false));
        h.type_str("rust lang");
        sent();
        h.key(keys::KEY_ENTER);
        let after = sent();
        assert!(!after.iter().any(|c| c.starts_with("LoadTyped") || c.starts_with("Load")), "{after:?}");
        let note = h.app().note.as_ref().map(|(text, _)| text.clone()).expect("the bar says why");
        assert!(note.contains("No search engine"), "{note}");
        assert!(h.app().omni.dropdown.is_none());
    }

    /// `javascript:` in the bar is refused: nothing is loaded or sent, the bar says why, the field shakes.
    #[test]
    fn enter_on_javascript_is_refused_and_shakes_the_field() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        assert_eq!(h.app().shake_at(Instant::now()), (0.0, 0.0), "no shake before a refusal");
        h.press(KEY_L, ctrl(false));
        h.type_str("javascript:alert(1)");
        sent();
        h.key(keys::KEY_ENTER);
        let after = sent();
        assert!(!after.iter().any(|c| c.starts_with("LoadTyped") || c.starts_with("Load")), "{after:?}");
        let note = h.app().note.as_ref().map(|(text, _)| text.clone()).expect("the bar says why");
        assert!(note.contains("javascript:"), "{note}");
        // Pure in the time: full flash and no push at the instant, a push in the middle, nothing once it is over.
        let at = h.app().refused_at.expect("the refusal is remembered");
        let ms = Duration::from_millis;
        assert_eq!(h.app().shake_at(at), (0.0, 1.0));
        let mid = h.app().shake_at(at + ms(60));
        assert!(mid.0 != 0.0 && mid.1 > 0.0 && mid.1 < 1.0, "{mid:?}");
        assert!(h.app().shake_at(at + ms(60)) == mid);
        assert_eq!(h.app().shake_at(at + address::SHAKE_FOR), (0.0, 0.0));
        // An ordinary address does not shake.
        let mut h = window();
        h.press(KEY_L, ctrl(false));
        h.type_str("example.org");
        h.key(keys::KEY_ENTER);
        assert!(h.app().refused_at.is_none());
    }

    #[test]
    fn enter_on_the_first_row_reads_the_field_and_a_search_is_marked_as_one() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = typing("lucina os");
        sent();
        h.key(keys::KEY_ENTER);
        let loaded = sent();
        assert!(loaded[0].starts_with("LoadTyped") && loaded[0].contains("lucina+os") && loaded[0].contains("search: true"), "{loaded:?}");
        let mut h = typing("example.com");
        sent();
        h.key(keys::KEY_ENTER);
        let loaded = sent();
        assert!(loaded[0].contains("example.com") && loaded[0].contains("search: false"), "{loaded:?}");
    }

    #[test]
    fn escape_puts_the_pages_address_back_and_closes_the_dropdown() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = typing("rust");
        h.key(keys::KEY_DOWN);
        h.key(keys::KEY_ESC);
        assert!(h.app().omni.dropdown.is_none());
        assert_eq!(h.app().address.text, "https://example.com/1");
        // With nothing open the key is not the dropdown's: it reaches no page either, as the field has the keyboard.
        sent();
        h.key(keys::KEY_ESC);
        assert!(sent().is_empty());
    }

    #[test]
    fn a_click_on_a_row_opens_it_and_a_click_elsewhere_closes_the_dropdown() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = typing("rust");
        answer(&mut h, vec![entry("https://rust-lang.org/", "Rust Programming Language")]);
        let at = h.app().omni_at.expect("the dropdown is drawn");
        sent();
        // Two rows: the second is the history page.
        h.click(at.x + 20, at.y + at.h / 2 + at.h / 4);
        let loaded = sent();
        assert!(loaded.iter().any(|c| c.starts_with("LoadTyped") && c.contains("rust-lang.org")), "{loaded:?}");
        assert!(!loaded.iter().any(|c| c.starts_with("Button")), "the page gets no part of the click: {loaded:?}");
        let mut h = typing("rust");
        h.click(900, 700);
        assert!(h.app().omni.dropdown.is_none());
    }

    #[test]
    fn a_page_the_history_knows_completes_the_typed_text_until_the_person_types_on() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = typing("exam");
        answer(&mut h, vec![entry("https://example.com/", "Example")]);
        assert_eq!(h.app().address.text, "example.com/");
        assert_eq!((h.app().address.anchor, h.app().address.cursor), (4, 12));
        // Typing the next letter replaces the selected tail.
        h.type_str("p");
        assert_eq!(h.app().address.text, "examp");
        // A late answer for text that has moved on completes nothing.
        let stale = h.app().omni.latest_query() - 1;
        tell(&mut h, News::HistoryHits { id: stale, entries: vec![entry("https://example.com/", "Example")] });
        assert_eq!(h.app().address.text, "examp");
        // Enter on a completion opens the completed address.
        let mut h = typing("exam");
        answer(&mut h, vec![entry("https://example.com/", "Example")]);
        sent();
        h.key(keys::KEY_ENTER);
        let loaded = sent();
        assert!(loaded[0].starts_with("LoadTyped") && loaded[0].contains("example.com"), "{loaded:?}");
    }

    /// What the fake engine logged for the `LoadTyped`, as `scheme://host/path?query search=<bool>` (its log is the Debug text).
    fn loaded_typed(log: &[String]) -> String {
        let line = log.iter().find(|c| c.starts_with("LoadTyped")).unwrap_or_else(|| panic!("nothing was loaded: {log:?}"));
        let between = |from: &str, to: &str| line.split_once(from).and_then(|(_, rest)| rest.split_once(to)).map_or(String::new(), |(x, _)| x.to_owned());
        let query = between("query: Some(\"", "\"");
        format!("{}://{}{}{} {}", between("scheme: \"", "\""), between("Domain(\"", "\""), between("path: \"", "\""), if query.is_empty() { String::new() } else { format!("?{query}") }, between("search: ", " "))
    }

    #[test]
    fn enter_on_one_letter_opens_the_most_visited_origin_over_the_scheme_the_history_has() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        // One letter, a most-visited origin: the field shows the completion and Enter opens that origin, not a search.
        let mut h = typing("w");
        answer(&mut h, vec![entry("https://wikipedia.org/wiki/Rust", "Rust")]);
        assert_eq!(h.app().address.text, "wikipedia.org/");
        sent();
        h.key(keys::KEY_ENTER);
        let log = sent();
        let load = loaded_typed(&log);
        assert_eq!(load, "https://wikipedia.org/ false");
        // The origin has only an http entry: it is opened as stored, never over a scheme the history does not have.
        let mut h = typing("o");
        answer(&mut h, vec![entry("http://old.example/page", "Old")]);
        assert_eq!(h.app().address.text, "old.example/");
        sent();
        h.key(keys::KEY_ENTER);
        let log = sent();
        let load = loaded_typed(&log);
        assert_eq!(load, "http://old.example/ false");
    }

    #[test]
    fn enter_on_one_letter_with_no_history_searches_as_before() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = typing("w");
        answer(&mut h, vec![]);
        assert_eq!(h.app().address.text, "w");
        sent();
        h.key(keys::KEY_ENTER);
        let log = sent();
        let load = loaded_typed(&log);
        assert_eq!(load, "https://duckduckgo.com/html/?q=w true");
    }

    #[test]
    fn a_letter_that_is_a_keyword_is_not_completed_and_enter_keeps_the_keyword() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        // The shipped default engine has the keyword "ddg": history for an origin that starts with it does not take it over.
        let mut h = typing("ddg");
        answer(&mut h, vec![entry("https://ddgfan.example/", "Fan")]);
        assert_eq!(h.app().address.text, "ddg");
        sent();
        h.key(keys::KEY_ENTER);
        let log = sent();
        let load = loaded_typed(&log);
        assert_eq!(load, "https://duckduckgo.com/html/?q=ddg true");
        // With text after the keyword the keyword searches, completion or not.
        let mut h = typing("ddg rust");
        answer(&mut h, vec![entry("https://ddgfan.example/", "Fan")]);
        sent();
        h.key(keys::KEY_ENTER);
        let log = sent();
        assert_eq!(loaded_typed(&log), "https://duckduckgo.com/html/?q=rust true", "{log:?}");
    }

    #[test]
    fn delete_on_a_completion_makes_enter_search_the_letter() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = typing("w");
        answer(&mut h, vec![entry("https://wikipedia.org/", "Wikipedia")]);
        assert_eq!(h.app().address.text, "wikipedia.org/");
        // Delete removes the selected tail, as it does for any completion; the same answer again does not bring it back.
        h.key(keys::KEY_DELETE);
        answer(&mut h, vec![entry("https://wikipedia.org/", "Wikipedia")]);
        assert_eq!(h.app().address.text, "w");
        sent();
        h.key(keys::KEY_ENTER);
        let log = sent();
        let load = loaded_typed(&log);
        assert_eq!(load, "https://duckduckgo.com/html/?q=w true");
    }

    #[test]
    fn two_letters_complete_and_open_the_same_way() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = typing("wi");
        answer(&mut h, vec![entry("https://wikipedia.org/", "Wikipedia")]);
        assert_eq!(h.app().address.text, "wikipedia.org/");
        sent();
        h.key(keys::KEY_ENTER);
        let log = sent();
        assert_eq!(loaded_typed(&log), "https://wikipedia.org/ false", "{log:?}");
    }

    #[test]
    fn deleting_a_letter_completes_nothing() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = typing("examx");
        h.key(keys::KEY_BACKSPACE);
        answer(&mut h, vec![entry("https://example.com/", "Example")]);
        assert_eq!(h.app().address.text, "exam");
    }

    fn glass_window(left_inset: f32) -> Harness<Browser> {
        let first = Url::parse("https://example.com/").unwrap();
        let chrome = chrome::Settings { mode: Mode::Glass, left_inset };
        let mut h = Harness::new(Browser::with_chrome(Arc::new(|_| {}), first, fake, chrome), 1366, 768);
        tell(&mut h, News::Tabs { tabs: tabs(3), active: 2 });
        tell(&mut h, News::History { back: true, forward: true });
        sent();
        h
    }

    /// The centre of a placed part of the row, in screen pixels (`onebar.rs` gives them already scaled).
    fn centre(b: &Placed) -> (i32, i32) {
        (b.cx().round() as i32, b.cy().round() as i32)
    }

    /// Where the row laid a part out last pass.
    fn placed(h: &mut Harness<Browser>, part: Part) -> Placed {
        h.app().layout.placed(part).expect("the row has this part")
    }

    /// The close mark of a full slot: its own press, at the slot's right end (`bar` gives it the last 24 dp).
    fn close_centre(b: &Placed) -> (i32, i32) {
        ((b.right() - 12.0).round() as i32, b.cy().round() as i32)
    }

    #[test]
    fn the_classic_chrome_draws_no_glass_and_the_default_is_the_platforms() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        assert_eq!(h.app().chrome.mode, Mode::Classic);
        assert_eq!(chrome::Settings::default().mode, chrome::default_mode(cfg!(target_os = "macos")));
        assert!(h.app().glass.is_none());
        assert_eq!(h.app().page_rect.y, ui_px(44.0), "one row, in the classic chrome too");
        assert!(h.app().glass_stats().is_none());
    }

    #[test]
    fn one_row_in_the_classic_chrome_too() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        for (n, glass) in [(1, false), (3, false), (12, false), (3, true)] {
            let mut h = if glass { glass_window(0.0) } else { window() };
            tell(&mut h, News::Tabs { tabs: tabs(n), active: 1 });
            let (y, k) = (h.app().page_rect.y, h.app().scale);
            assert_eq!(y, (44.0 * k).round() as i32, "{n} tabs, glass {glass}: still one row");
        }
    }

    #[test]
    fn glass_clicks_land_on_what_is_drawn_and_send_the_same_commands() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = glass_window(0.0);
        let l = h.app().layout.clone();
        let k = h.app().scale;
        assert!((l.band_h * k) as i32 <= h.app().page_rect.y, "the page starts under the row");
        assert!(h.app().glass_stats().is_some_and(|(draws, ..)| draws >= 1), "and the row was drawn");
        // The tab capsules: a click on the slot selects it, on its close mark closes it.
        let t = l.placed(Part::Tab(0)).expect("the first tab");
        h.click(centre(&t).0 - ui_px(12.0), centre(&t).1);
        assert_eq!(sent(), ["SelectTab(1)"]);
        h.click(close_centre(&t).0, close_centre(&t).1);
        assert_eq!(sent(), ["CloseTab(1)"]);
        let t = l.placed(Part::Tab(2)).expect("the third tab");
        h.click(close_centre(&t).0, close_centre(&t).1);
        assert_eq!(sent(), ["CloseTab(3)"]);
        // + opens a tab, the arrows and the field's own reload go to the engine.
        h.click(centre(&l.placed(Part::NewTab).expect("+")).0, centre(&l.placed(Part::NewTab).expect("+")).1);
        assert_eq!(sent(), ["NewTab(None)"]);
        h.click(centre(&l.placed(Part::Back).expect("back")).0, centre(&l.placed(Part::Back).expect("back")).1);
        assert_eq!(sent(), ["Back"]);
        h.click(centre(&l.placed(Part::Forward).expect("forward")).0, centre(&l.placed(Part::Forward).expect("forward")).1);
        assert_eq!(sent(), ["Forward"]);
        h.click(centre(&l.placed(Part::FieldReload).expect("reload")).0, centre(&l.placed(Part::FieldReload).expect("reload")).1);
        assert_eq!(sent(), ["Reload"]);
        // The row's own empty stretch is nothing to the page and nothing to the engine.
        let d = l.placed(Part::DragRegion).expect("the drag region");
        h.click(centre(&d).0, centre(&d).1);
        assert!(sent().is_empty());
        assert!(h.app().drags_window(centre(&d).0, centre(&d).1), "and a window host may drag there");
    }

    /// The glass canvas's pixels of a placed part, to compare what the row shows before and after.
    fn band_pixels(h: &mut Harness<Browser>, b: &Placed) -> Vec<u32> {
        let g = h.app().glass.as_ref().expect("a glass window draws the glass");
        let mut v = Vec::new();
        for y in b.y as u32..b.bottom() as u32 {
            for x in b.x as u32..b.right() as u32 {
                v.push(g.pixel(x, y));
            }
        }
        v
    }

    #[test]
    fn the_glass_star_is_the_same_press_as_ctrl_d_and_draws_what_the_engine_says() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        use lucinanet_shortcuts::code;
        let mut h = glass_window(0.0);
        let k = h.app().scale;
        let star = placed(&mut h, Part::Star);
        let (x, y) = centre(&star);
        let plain = band_pixels(&mut h, &star);
        h.click(x, y);
        assert_eq!(sent(), ["ToggleBookmark"], "the star goes to the engine, as in the classic row");
        assert!(!h.app().bookmarked, "and the window does not guess the outcome");
        tell(&mut h, News::Bookmarked(true));
        assert!(h.app().bookmarked);
        assert_ne!(band_pixels(&mut h, &star), plain, "the glass shows the page's bookmark");
        h.click(x, y);
        assert_eq!(sent(), ["ToggleBookmark"]);
        tell(&mut h, News::Bookmarked(false));
        assert!(!h.app().bookmarked);
        // Ctrl+D is the very same press.
        h.press(code::D, ctrl(false));
        assert_eq!(sent(), ["ToggleBookmark"]);
        let _ = k;
    }

    #[test]
    fn glass_address_field_takes_the_keys_through_the_existing_editor() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        for glass in [true, false] {
            let mut h = if glass { glass_window(0.0) } else { window() };
            let field = placed(&mut h, Part::Field);
            let (x, y) = centre(&field);
            h.click(x, y);
            assert!(h.app().typing, "a click in the field's text area focuses it, glass {glass}");
            h.app().address.select_all();
            h.type_str("wiki.org");
            h.key(keys::KEY_ENTER);
            assert!(sent().iter().any(|c| c.starts_with("LoadTyped") && c.contains("wiki.org")), "glass {glass}: {:?}", sent());
        }
    }

    #[test]
    fn every_control_the_glass_bar_draws_does_something() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = glass_window(0.0);
        tell(&mut h, News::Zoom(150));
        sent();
        let parts: Vec<Placed> = h.app().layout.parts.clone();
        for b in &parts {
            // The row's empty stretch draws the zoom readout; it is not a control. The shown tab (id 2, index 1)
            // has nothing to do, as in the classic row.
            if matches!(b.part, Part::DragRegion | Part::Tab(1)) {
                continue;
            }
            // A fresh window for each, so one click's focus does not hide the next one's effect.
            let mut h = glass_window(0.0);
            tell(&mut h, News::Zoom(150));
            sent();
            let (x, y) = centre(b);
            h.click(x, y);
            // A click answers with a command, or with the field's keys, or with what the row has to say.
            let acted = !sent().is_empty() || h.app().typing || h.app().note.is_some();
            assert!(acted, "{:?} is drawn and does nothing", b.part);
        }
    }

    #[test]
    fn the_left_inset_moves_the_row_and_leaves_its_own_space_free() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut a = glass_window(0.0);
        let mut b = glass_window(76.0);
        let (la, lb) = (a.app().layout.clone(), b.app().layout.clone());
        let (ta, tb) = (la.placed(Part::Tab(0)).expect("a tab"), lb.placed(Part::Tab(0)).expect("a tab"));
        // The inset is the room the host's own window buttons take, padding included, so the tabs move by what is
        // left of it after the row's own side padding.
        assert_eq!((tb.x - ta.x) / b.app().scale, 76.0 - crate::onebar::PAD, "the row of tabs moves into the inset's room");
        assert_eq!(a.app().page_rect, b.app().page_rect, "the page does not move");
        // The inset's own middle is free of controls in the window that has it, and is the first tab in the one that has not.
        let x = ui_px(38.0) as f32;
        let y = ta.cy();
        assert!(matches!(la.part_at(x, y), Some(Part::Back | Part::Forward)), "without the inset that point is one of the arrows");
        assert_eq!(lb.part_at(x, y), None, "with the inset it is free");
        b.click(x as i32, y as i32);
        assert!(sent().is_empty(), "the inset is free of tabs");
        let (x, y) = centre(&tb);
        b.click(x, y);
        assert_eq!(sent(), ["SelectTab(1)"]);
    }

    #[test]
    fn the_glass_bar_is_not_drawn_again_when_nothing_changed() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = glass_window(0.0);
        // A loading page is a change every frame (its line moves); this is about a row that shows the same thing.
        tell(&mut h, News::Loading(false));
        h.core.paint();
        let (draws, ..) = h.app().glass_stats().unwrap();
        let page = h.app().page_rect;
        for _ in 0..5 {
            // The toolkit skips the draw pass when nothing went dirty, so a bare second `paint` never reaches
            // the bars. A click in the page draws the window again and changes nothing the bar shows.
            h.click(page.x + 10, page.y + 10);
        }
        let (after, cached, ..) = h.app().glass_stats().unwrap();
        assert_eq!(after, draws, "five more passes, no new draw of the bars");
        assert!(cached >= 1, "and they were found unchanged");
        assert!(h.app().glass.as_ref().unwrap().backdrop.blurs <= 2, "the backdrop was not blurred again either");
    }

    #[test]
    fn page_clicks_are_not_shifted_in_either_chrome_and_the_engine_has_the_page_rectangle() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        for glass in [false, true] {
            let mut h = if glass { glass_window(0.0) } else { window() };
            let r = h.app().page_rect;
            let k = h.app().scale;
            assert!(r.y > 0 && r.w > 0 && r.h > 0);
            // The engine was started with exactly the page rectangle and the toolkit's scale (it is told once; the window does not resize).
            let (w, hh) = h.app().page.lock().map(|p| (p.w as i32, p.h as i32)).unwrap();
            assert_eq!((w, hh), (r.w, r.h), "glass {glass}");
            assert!(k > 0.0);
            sent();
            // A press 10 px right of and 20 px below the page's corner reaches the engine at (10, 20), whatever the bars above.
            h.click(r.x + 10, r.y + 20);
            let cmds = sent();
            assert!(cmds.iter().any(|c| c == "Button { down: true, x: 10.0, y: 20.0 }"), "glass {glass}: {cmds:?}");
            // And one in the bars' band is the page's no more.
            h.click(r.x + 10, r.y - 3);
            assert!(!sent().iter().any(|c| c.starts_with("Button")), "glass {glass}");
        }
    }

    /// two engines, as a later settings wire-up will hand them over (the UI builds `Engines` from the shipped
    /// settings today, which hold one).
    fn two_engines() -> crate::address::Engines {
        let mut settings = lucinanet_settings::Settings::default();
        settings.search.engines.push(lucinanet_settings::Engine {
            id: "second".to_owned(),
            name: "Second".to_owned(),
            keyword: Some("sec".to_owned()),
            template: "https://second.test/?q={searchTerms}".to_owned(),
            suggest_url: None,
        });
        crate::address::Engines::from_settings(&settings)
    }

    fn shift() -> keys::Mods {
        keys::Mods { shift: true, ..keys::Mods::default() }
    }

    /// The bar with two engines, `text` typed in it.
    fn typing_with_two_engines(text: &str) -> Harness<Browser> {
        let mut h = window();
        h.app().omni.engines = two_engines();
        h.press(KEY_L, ctrl(false));
        h.type_str(text);
        h
    }

    #[test]
    fn shift_return_is_read_from_the_pending_key_and_nothing_else() {
        let enter = |mods| Some(Input::Key(Press { code: keys::KEY_ENTER, mods }));
        assert!(shift_return(enter(shift())));
        assert!(shift_return(Some(Input::Key(Press { code: keys::KEY_KPENTER, mods: shift() }))));
        assert!(!shift_return(enter(keys::Mods::default())));
        assert!(!shift_return(enter(keys::Mods { shift: true, ctrl: true, ..keys::Mods::default() })));
        assert!(!shift_return(Some(Input::Key(Press { code: keys::KEY_DOWN, mods: shift() }))));
        assert!(!shift_return(Some(Input::Move(1, 1))));
        assert!(!shift_return(None));
    }

    #[test]
    fn shift_enter_in_the_bar_searches_the_second_engine_with_the_typed_text() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = typing_with_two_engines("lucina os");
        sent();
        h.press(keys::KEY_ENTER, shift());
        let log = sent();
        assert_eq!(loaded_typed(&log), "https://second.test/?q=lucina+os true", "{log:?}");
        assert_eq!(log.iter().filter(|c| c.starts_with("Load")).count(), 1, "{log:?}");
        // Plain Enter on the same text still goes to the default engine.
        let mut h = typing_with_two_engines("lucina os");
        sent();
        h.key(keys::KEY_ENTER);
        assert_eq!(loaded_typed(&sent()), "https://duckduckgo.com/html/?q=lucina+os true");
    }

    #[test]
    fn typing_alone_sends_nothing_to_either_engine() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _h = typing_with_two_engines("lucina os");
        let log = sent();
        assert!(!log.iter().any(|c| c.starts_with("Load")), "{log:?}");
    }

    #[test]
    fn shift_enter_with_one_engine_is_exactly_enter() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = typing("lucina os");
        sent();
        h.press(keys::KEY_ENTER, shift());
        assert_eq!(loaded_typed(&sent()), "https://duckduckgo.com/html/?q=lucina+os true");
    }

    #[test]
    fn shift_enter_on_an_address_opens_it_and_a_completion_or_row_is_not_used_for_a_search() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = typing_with_two_engines("example.com");
        sent();
        h.press(keys::KEY_ENTER, shift());
        assert_eq!(loaded_typed(&sent()), "https://example.com/ false");
        // An inline completion is left out of the search: the second engine gets "exam".
        let mut h = typing_with_two_engines("exam");
        answer(&mut h, vec![entry("https://example.com/", "Example")]);
        assert_eq!(h.app().address.text, "example.com/");
        sent();
        h.press(keys::KEY_ENTER, shift());
        assert_eq!(loaded_typed(&sent()), "https://second.test/?q=exam true");
    }

    #[test]
    fn shift_enter_of_javascript_is_refused_and_shakes_like_enter() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = typing_with_two_engines("javascript:alert(1)");
        sent();
        h.press(keys::KEY_ENTER, shift());
        let after = sent();
        assert!(!after.iter().any(|c| c.starts_with("Load")), "{after:?}");
        let note = h.app().note.as_ref().map(|(text, _)| text.clone()).expect("the bar says why");
        assert!(note.contains("javascript:"), "{note}");
        assert!(h.app().refused_at.is_some());
    }

    /// , red first: the hint of an empty field is what the engines make it. Both chromes draw the `TextEdit`'s
    /// placeholder (classic through the toolkit, glass into its own frame), so this reads the one value the row shows
    /// after the omnibox's engines change.
    #[test]
    fn the_empty_field_hint_follows_the_engines_the_omnibox_holds() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        for glass in [false, true] {
            let mut h = if glass { glass_window(0.0) } else { window() };
            assert_eq!(h.app().address.placeholder, "Search or enter an address", "glass {glass}: the shipped engines search");
            h.app().omni.engines = crate::address::Engines::none();
            // A key, not `paint` on its own: the core only runs a pass when something has asked for one, and a bare
            // `paint` after a field the app cannot see change would draw the old frame. Esc with the field not focused
            // is the one input that takes the row to no action at all.
            h.key(keys::KEY_ESC);
            let hint = h.app().address.placeholder.clone();
            assert!(!hint.contains("Search"), "glass {glass}: no engine, yet the field promises a search: {hint}");
            assert_eq!(hint, "Enter an address", "glass {glass}");
        }
    }

    /// , red first: an engine whose template cannot be filled is no engine for the sake of the hint.
    #[test]
    fn an_unusable_template_makes_the_hint_promise_no_search() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        let mut settings = lucinanet_settings::Settings::default();
        settings.search.engines[0].template = "not a url".to_owned();
        h.app().omni.engines = crate::address::Engines::from_settings(&settings);
        h.key(keys::KEY_ESC);
        let hint = h.app().address.placeholder.clone();
        assert!(!hint.contains("Search"), "the template cannot be filled, yet the field promises a search: {hint}");
    }

    /// what each chrome shows is the helper's string for the engines the omnibox holds - the classic field the
    /// toolkit draws, and the glass frame the row is painted from - for every shape of engine, before and after a change.
    #[test]
    fn both_chromes_show_the_helpers_hint_for_the_same_engines() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut broken = lucinanet_settings::Settings::default();
        broken.search.engines[0].template = "not a url".to_owned();
        for glass in [false, true] {
            let mut h = if glass { glass_window(0.0) } else { window() };
            for (name, engines) in [
                ("shipped", crate::address::Engines::default()),
                ("none", crate::address::Engines::none()),
                ("broken template", crate::address::Engines::from_settings(&broken)),
            ] {
                h.app().omni.engines = engines;
                h.key(keys::KEY_ESC);
                let want = crate::address::placeholder_for(&h.app().omni.engines).to_owned();
                let frame = h.app().glass_state();
                assert_eq!(frame.placeholder, want, "glass {glass}: the frame's hint, {name}");
                assert_eq!(h.app().address.placeholder, want, "glass {glass}: the field's hint, {name}");
                if name == "shipped" {
                    assert_eq!(want, "Search or enter an address", "{name}: the words are unchanged");
                } else {
                    assert!(!want.contains("Search"), "{name}: {want} promises a search");
                }
            }
        }
    }

    /// the downloads panel is one state that both chromes draw - `downloads_strip` runs from `strip` and
    /// `downloads` at the end of the pass whatever the row at the top of the window looks like - so a glass window
    /// opens, shows and closes the same panel a classic one does.
    #[test]
    fn both_chromes_show_the_same_downloads_panel() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        for glass in [false, true] {
            let mut h = if glass { glass_window(0.0) } else { window() };
            if glass {
                assert!(h.app().glass_stats().is_some_and(|(draws, ..)| draws >= 1), "the glass row was drawn");
            } else {
                assert!(h.app().glass.is_none(), "the classic row draws no glass");
            }
            tell(&mut h, News::Loading(false));
            tell(&mut h, News::Download {
                id: 11,
                name: "clip.mp4".into(),
                received: 1 << 20,
                total: Some(2 << 20),
                state: DownloadState::Running,
            });
            sent();

            h.press(code::Y, ctrl(true));
            assert!(h.app().downloads_open, "glass {glass}: the command opened the panel");
            let strip = h
                .app()
                .strip
                .unwrap_or_else(|| panic!("glass {glass}: the panel is over the page in both chromes"));
            assert_eq!(strip.bottom(), h.app().page_rect.bottom(), "glass {glass}: at the page's bottom edge");
            let names: Vec<&str> = shown_rows(&h.app().downloads).iter().map(|row| row.name.as_str()).collect();
            assert_eq!(names, ["clip.mp4"], "glass {glass}: the same rows either chrome shows");

            h.key(code::ESC);
            assert!(sent().is_empty(), "glass {glass}: the page never sees the panel's Esc");
            assert!(!h.app().downloads_open, "glass {glass}: and Esc closes it in both");
        }
    }

    /// Ctrl+H puts the history panel up, asks the engine for the list (and only then), shows it newest first and
    /// moves over it with Up and Down; Enter opens the row in the shown tab the way an address-bar row does, Esc closes.
    #[test]
    fn the_history_command_lists_the_pages_and_enter_opens_one() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        tell(&mut h, News::Loading(false));
        assert!(!h.app().history_open && h.app().strip.is_none(), "nothing up before the command");

        h.press(code::H, ctrl(false));
        assert!(h.app().history_open, "Ctrl+H opened the panel");
        let asked = sent();
        assert_eq!(asked.len(), 1, "one request, to the engine: {asked:?}");
        assert!(asked[0].starts_with("ListHistory") && asked[0].contains("limit: 200"), "{asked:?}");
        assert!(h.app().strip.is_some(), "the panel is over the page while it waits for the list");
        let id = h.app().history_ask;

        // An answer to another request is an old one.
        tell(&mut h, News::HistoryList { id: id + 1, entries: vec![entry("https://old.example.org/", "Old")] });
        assert!(h.app().history.is_empty(), "a stale answer is dropped");

        let entries = vec![
            entry("https://c.example.com/page", "Third"),
            entry("https://b.example.org/", ""),
            entry("https://a.example.net/x", "First"),
        ];
        tell(&mut h, News::HistoryList { id, entries: entries.clone() });
        let urls: Vec<&str> = h.app().history.iter().map(|e| e.url.as_str()).collect();
        assert_eq!(urls, ["https://c.example.com/page", "https://b.example.org/", "https://a.example.net/x"], "in the order given");
        assert_eq!(h.app().history_sel, 0, "the first row is the one Enter would open");
        assert_eq!(history_note(&h.app().history), None, "a list says nothing");
        let strip = h.app().strip.expect("the panel is drawn");
        assert_eq!(strip.bottom(), h.app().page_rect.bottom(), "at the page's bottom edge, as the downloads panel is");
        assert!(sent().is_empty(), "listing asks for nothing more");

        // Up and Down are the panel's, not the page's.
        h.key(keys::KEY_DOWN);
        h.key(keys::KEY_DOWN);
        assert_eq!(h.app().history_sel, 2, "two Downs reached the third row");
        h.key(keys::KEY_DOWN);
        assert_eq!(h.app().history_sel, 2, "and the list does not wrap past its end");
        h.key(keys::KEY_UP);
        h.key(keys::KEY_DOWN);
        assert_eq!(h.app().history_sel, 2);
        assert!(sent().is_empty(), "the page never saw an arrow");

        h.key(keys::KEY_ENTER);
        let went = sent();
        assert_eq!(went.len(), 1, "{went:?}");
        assert!(went[0].starts_with("LoadTyped") && went[0].contains("a.example.net") && went[0].contains("path: \"/x\"") && went[0].contains("search: false"), "{went:?}");
        assert!(!h.app().history_open, "Enter closed the panel");
        assert!(h.app().strip.is_none(), "and nothing is left over the page");
    }

    /// Esc closes the panel and is not the page's; an empty store says so in one line; the key is a toggle.
    #[test]
    fn the_history_panel_closes_on_esc_and_says_so_when_empty() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        h.press(code::H, ctrl(false));
        sent();
        let id = h.app().history_ask;
        tell(&mut h, News::HistoryList { id, entries: Vec::new() });
        assert!(h.app().history_open);
        assert_eq!(history_note(&h.app().history), Some(NO_HISTORY), "an empty list says so");
        assert_eq!(NO_HISTORY, "No history", "in the words a person reads");
        let k = h.app().scale;
        let strip = h.app().strip.expect("drawn all the same");
        assert_eq!(strip.h, (DOWNLOAD_ROW_PX * k).round() as i32 + (8.0 * k).round() as i32, "one row, as the downloads note");
        h.key(keys::KEY_ENTER);
        assert!(sent().is_empty() && h.app().history_open, "Enter with nothing to open does nothing");

        h.key(code::ESC);
        assert!(sent().is_empty(), "the page never sees the panel's Esc");
        assert!(!h.app().history_open, "Esc closed it");
        assert!(h.app().strip.is_none());

        h.press(code::H, ctrl(false));
        assert!(h.app().history_open, "the key opens it again, and asks again");
        assert_eq!(sent().len(), 1);
        h.press(code::H, ctrl(false));
        assert!(!h.app().history_open, "and takes it down again");
        assert!(sent().is_empty(), "closing asks for nothing");
    }

    /// One panel at a time: each command takes the other one down.
    #[test]
    fn the_history_and_downloads_panels_are_never_up_together() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        h.press(code::Y, ctrl(true));
        assert!(h.app().downloads_open && !h.app().history_open);
        h.press(code::H, ctrl(false));
        assert!(h.app().history_open && !h.app().downloads_open, "history took the downloads panel down");
        h.press(code::Y, ctrl(true));
        assert!(h.app().downloads_open && !h.app().history_open, "and downloads took history down");
    }

    /// What the panel draws: which rows are in view, and what a row says.
    #[test]
    fn history_rows_are_windowed_and_read_title_then_host() {
        assert_eq!(history_window(3, 0, 8), 0..3, "a short list is all of it");
        assert_eq!(history_window(0, 0, 8), 0..0);
        assert_eq!(history_window(200, 0, 8), 0..8);
        assert_eq!(history_window(200, 7, 8), 0..8, "the last row in view does not scroll");
        assert_eq!(history_window(200, 8, 8), 1..9, "one past it scrolls by one");
        assert_eq!(history_window(200, 199, 8), 192..200, "the end of the list");
        assert_eq!(
            history_row(&entry("https://www.example.com/a/b?q=1", "A title")),
            ("A title".to_owned(), "www.example.com".to_owned())
        );
        assert_eq!(
            history_row(&entry("https://example.org/path", "")),
            ("https://example.org/path".to_owned(), "example.org".to_owned()),
            "no title: the address stands in for it"
        );
        assert_eq!(history_row(&entry("about:blank", "")).1, "", "no host, nothing dimmed");
    }

    /// Both chromes draw the one history state, as they do the downloads panel's.
    #[test]
    fn both_chromes_show_the_same_history_panel() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        for glass in [false, true] {
            let mut h = if glass { glass_window(0.0) } else { window() };
            tell(&mut h, News::Loading(false));
            sent();
            h.press(code::H, ctrl(false));
            assert!(h.app().history_open, "glass {glass}: the command opened the panel");
            let id = h.app().history_ask;
            tell(&mut h, News::HistoryList { id, entries: vec![entry("https://a.example.com/", "A"), entry("https://b.example.com/", "B")] });
            let strip = h.app().strip.unwrap_or_else(|| panic!("glass {glass}: over the page in both chromes"));
            assert_eq!(strip.bottom(), h.app().page_rect.bottom(), "glass {glass}");
            assert_eq!(h.app().history.len(), 2, "glass {glass}");
            sent();
            h.key(keys::KEY_DOWN);
            h.key(keys::KEY_ENTER);
            let went = sent();
            assert!(went.len() == 1 && went[0].contains("b.example.com"), "glass {glass}: {went:?}");
            assert!(!h.app().history_open, "glass {glass}: Enter closes it in both");
        }
    }

    fn bentry(url: &str, title: &str) -> lucinanet_omnibox::BookmarkEntry {
        lucinanet_omnibox::BookmarkEntry { url: url.into(), title: title.into(), folder_path: "Other Bookmarks".into() }
    }

    /// Ctrl+Shift+O (and Ctrl+B) put the bookmarks panel up, ask the engine for the list (and only then), and move
    /// over it with Up and Down; Enter opens the row in the shown tab the way an address-bar row does, Esc closes.
    #[test]
    fn the_bookmarks_command_lists_the_bookmarks_and_enter_opens_one() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        tell(&mut h, News::Loading(false));
        assert!(!h.app().bookmarks_open && h.app().strip.is_none(), "nothing up before the command");

        h.press(code::O, ctrl(true));
        assert!(h.app().bookmarks_open, "Ctrl+Shift+O opened the panel");
        let asked = sent();
        assert_eq!(asked.len(), 1, "one request, to the engine: {asked:?}");
        assert!(asked[0].starts_with("ListBookmarks") && asked[0].contains("limit: 200"), "{asked:?}");
        assert!(h.app().strip.is_some(), "the panel is over the page while it waits for the list");
        let id = h.app().bookmarks_ask;

        // An answer to another request is an old one.
        tell(&mut h, News::BookmarkList { id: id + 1, entries: vec![bentry("https://old.example.org/", "Old")] });
        assert!(h.app().bookmarks.is_empty(), "a stale answer is dropped");

        let entries = vec![
            bentry("https://c.example.com/page", "Third"),
            bentry("https://b.example.org/", ""),
            bentry("https://a.example.net/x", "First"),
        ];
        tell(&mut h, News::BookmarkList { id, entries });
        let urls: Vec<&str> = h.app().bookmarks.iter().map(|e| e.url.as_str()).collect();
        assert_eq!(urls, ["https://c.example.com/page", "https://b.example.org/", "https://a.example.net/x"], "in the order given");
        assert_eq!(h.app().bookmarks_sel, 0, "the first row is the one Enter would open");
        assert_eq!(bookmark_note(&h.app().bookmarks), None, "a list says nothing");
        let strip = h.app().strip.expect("the panel is drawn");
        assert_eq!(strip.bottom(), h.app().page_rect.bottom(), "at the page's bottom edge, as the other panels are");
        assert!(sent().is_empty(), "listing asks for nothing more");

        h.key(keys::KEY_DOWN);
        h.key(keys::KEY_DOWN);
        assert_eq!(h.app().bookmarks_sel, 2, "two Downs reached the third row");
        h.key(keys::KEY_DOWN);
        assert_eq!(h.app().bookmarks_sel, 2, "and the list does not wrap past its end");
        h.key(keys::KEY_UP);
        h.key(keys::KEY_DOWN);
        assert_eq!(h.app().bookmarks_sel, 2);
        assert!(sent().is_empty(), "the page never saw an arrow");

        h.key(keys::KEY_ENTER);
        let went = sent();
        assert_eq!(went.len(), 1, "{went:?}");
        assert!(went[0].starts_with("LoadTyped") && went[0].contains("a.example.net") && went[0].contains("path: \"/x\"") && went[0].contains("search: false"), "{went:?}");
        assert!(!h.app().bookmarks_open, "Enter closed the panel");
        assert!(h.app().strip.is_none(), "and nothing is left over the page");
    }

    /// Esc closes the panel and is not the page's; an empty store says so in one line; the key is a toggle; Ctrl+B is the same.
    #[test]
    fn the_bookmarks_panel_closes_on_esc_and_says_so_when_empty() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        h.press(code::B, ctrl(false));
        assert!(h.app().bookmarks_open, "Ctrl+B is the other key for it");
        sent();
        let id = h.app().bookmarks_ask;
        tell(&mut h, News::BookmarkList { id, entries: Vec::new() });
        assert!(h.app().bookmarks_open);
        assert_eq!(bookmark_note(&h.app().bookmarks), Some(NO_BOOKMARKS), "an empty list says so");
        assert_eq!(NO_BOOKMARKS, "No bookmarks", "in the words a person reads");
        let k = h.app().scale;
        let strip = h.app().strip.expect("drawn all the same");
        assert_eq!(strip.h, (DOWNLOAD_ROW_PX * k).round() as i32 + (8.0 * k).round() as i32, "one row, as the other notes");
        h.key(keys::KEY_ENTER);
        assert!(sent().is_empty() && h.app().bookmarks_open, "Enter with nothing to open does nothing");

        h.key(code::ESC);
        assert!(sent().is_empty(), "the page never sees the panel's Esc");
        assert!(!h.app().bookmarks_open, "Esc closed it");
        assert!(h.app().strip.is_none());

        h.press(code::O, ctrl(true));
        assert!(h.app().bookmarks_open, "the key opens it again, and asks again");
        assert_eq!(sent().len(), 1);
        h.press(code::O, ctrl(true));
        assert!(!h.app().bookmarks_open, "and takes it down again");
        assert!(sent().is_empty(), "closing asks for nothing");
    }

    /// One panel at a time: each of the three commands takes the others down, and the history panel still works.
    #[test]
    fn the_three_panels_are_never_up_together() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        let up = |h: &mut Harness<Browser>| {
            let a = h.app();
            (a.downloads_open, a.history_open, a.bookmarks_open)
        };
        h.press(code::Y, ctrl(true));
        assert_eq!(up(&mut h), (true, false, false));
        h.press(code::O, ctrl(true));
        assert_eq!(up(&mut h), (false, false, true), "bookmarks took the downloads panel down");
        h.press(code::H, ctrl(false));
        assert_eq!(up(&mut h), (false, true, false), "history took the bookmarks panel down");
        h.press(code::O, ctrl(true));
        assert_eq!(up(&mut h), (false, false, true), "and bookmarks took history down");
        h.press(code::Y, ctrl(true));
        assert_eq!(up(&mut h), (true, false, false), "and downloads took bookmarks down");

        // The history panel works as before after the others have come and gone.
        sent();
        h.press(code::H, ctrl(false));
        let asked = sent();
        assert!(asked.len() == 1 && asked[0].starts_with("ListHistory"), "{asked:?}");
        let id = h.app().history_ask;
        tell(&mut h, News::HistoryList { id, entries: vec![entry("https://h.example.com/", "H")] });
        h.key(keys::KEY_ENTER);
        let went = sent();
        assert!(went.len() == 1 && went[0].contains("h.example.com"), "{went:?}");

        // An answer for the bookmarks panel does nothing while it is not up, and a history answer is not a bookmarks one.
        h.press(code::O, ctrl(true));
        sent();
        let bid = h.app().bookmarks_ask;
        h.press(code::H, ctrl(false));
        sent();
        tell(&mut h, News::BookmarkList { id: bid, entries: vec![bentry("https://late.example.com/", "Late")] });
        assert!(h.app().bookmarks.is_empty(), "a bookmarks answer arriving after the panel went down is dropped");
    }

    /// What the panel says in a row: the title (the address when there is none) and the host, dimmed beside it.
    #[test]
    fn bookmark_rows_read_title_then_host() {
        assert_eq!(
            bookmark_row(&bentry("https://www.example.com/a/b?q=1", "A title")),
            ("A title".to_owned(), "www.example.com".to_owned())
        );
        assert_eq!(
            bookmark_row(&bentry("https://example.org/path", "")),
            ("https://example.org/path".to_owned(), "example.org".to_owned()),
            "no title: the address stands in for it"
        );
        assert_eq!(bookmark_row(&bentry("https://example.org/path", "  ")).0, "https://example.org/path", "a blank title is none");
        assert_eq!(bookmark_row(&bentry("about:blank", "")).1, "", "no host, nothing dimmed");
    }

    /// Both chromes draw the one bookmarks state, as they do the other panels'.
    #[test]
    fn both_chromes_show_the_same_bookmarks_panel() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        for glass in [false, true] {
            let mut h = if glass { glass_window(0.0) } else { window() };
            tell(&mut h, News::Loading(false));
            sent();
            h.press(code::O, ctrl(true));
            assert!(h.app().bookmarks_open, "glass {glass}: the command opened the panel");
            let id = h.app().bookmarks_ask;
            tell(&mut h, News::BookmarkList { id, entries: vec![bentry("https://a.example.com/", "A"), bentry("https://b.example.com/", "B")] });
            let strip = h.app().strip.unwrap_or_else(|| panic!("glass {glass}: over the page in both chromes"));
            assert_eq!(strip.bottom(), h.app().page_rect.bottom(), "glass {glass}");
            assert_eq!(h.app().bookmarks.len(), 2, "glass {glass}");
            sent();
            h.key(keys::KEY_DOWN);
            h.key(keys::KEY_ENTER);
            let went = sent();
            assert!(went.len() == 1 && went[0].contains("b.example.com"), "glass {glass}: {went:?}");
            assert!(!h.app().bookmarks_open, "glass {glass}: Enter closes it in both");
        }
    }

    // ---- the find bar over the page's text ----

    fn tblock(texts: &[&str]) -> lucinanet_find::Block {
        lucinanet_find::Block {
            runs: texts.iter().enumerate().map(|(i, t)| lucinanet_find::Run { node: i as u64 + 1, text: (*t).to_string() }).collect(),
        }
    }

    /// Answer the text request that is out, the way the engine does.
    fn answer_text(h: &mut Harness<Browser>, blocks: Vec<lucinanet_find::Block>, partial: bool) {
        let id = h.app().find_ask;
        tell(h, News::PageText { id, blocks, partial });
    }

    fn shift_mods() -> keys::Mods {
        keys::Mods { shift: true, ..keys::Mods::default() }
    }

    /// An open bar over a page that says "alpha beta alpha" and a typed "alpha".
    fn find_window(glass: bool) -> Harness<Browser> {
        use lucinanet_shortcuts::code;
        let mut h = if glass { glass_window(0.0) } else { window() };
        tell(&mut h, News::Loading(false));
        tell(&mut h, News::Url("https://example.com/1".into()));
        sent();
        h.press(code::F, ctrl(false));
        answer_text(&mut h, vec![tblock(&["alpha beta alpha"])], false);
        h.type_str("alpha");
        sent();
        h
    }

    /// Ctrl+F puts the bar up and asks the engine for the page's text, once per press; the page never sees the key, and the
    /// find keys with no bar up do nothing at all.
    #[test]
    fn ctrl_f_opens_the_bar_and_asks_for_the_page_text() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        tell(&mut h, News::Loading(false));
        sent();
        h.press(code::F3, keys::Mods::default());
        h.press(code::F3, shift_mods());
        assert!(sent().is_empty(), "F3 with no bar up asks for nothing and the page does not see it");
        assert!(!h.app().find_open);

        h.press(code::F, ctrl(false));
        assert!(h.app().find_open);
        let first = h.app().find_ask;
        assert_eq!(sent(), vec![format!("PageText {{ id: {first} }}")], "one request, no key to the page");
        h.press(code::F, ctrl(false));
        let second = h.app().find_ask;
        assert_ne!(first, second, "Ctrl+F again asks again, with a new id");
        assert_eq!(sent(), vec![format!("PageText {{ id: {second} }}")]);
    }

    /// Typing runs the search over the blocks, Enter and F3 go to the next match, Shift+Enter and Shift+F3 to the previous
    /// one, and the counter says "n of m" in Firefox's words, the wrap after it. None of it reaches the page.
    #[test]
    fn typing_searches_the_text_and_enter_and_f3_step_through_the_matches() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        tell(&mut h, News::Loading(false));
        sent();
        h.press(code::F, ctrl(false));
        // Typed before the text came: nothing to show yet, and the answer runs the search by itself.
        h.type_str("alpha");
        assert_eq!(h.app().find_counter(), "", "no text yet, nothing said");
        answer_text(&mut h, vec![tblock(&["alpha beta alpha"]), tblock(&["gamma"])], false);
        assert_eq!(h.app().find_counter(), "1 of 2 matches");
        h.key(keys::KEY_ENTER);
        assert_eq!(h.app().find_counter(), "2 of 2 matches");
        h.key(keys::KEY_ENTER);
        assert_eq!(h.app().find_counter(), "1 of 2 matches Reached end of page, continued from top");
        h.press(keys::KEY_ENTER, shift_mods());
        assert_eq!(h.app().find_counter(), "2 of 2 matches Reached top of page, continued from bottom");
        h.press(code::F3, shift_mods());
        assert_eq!(h.app().find_counter(), "1 of 2 matches");
        h.press(code::F3, keys::Mods::default());
        assert_eq!(h.app().find_counter(), "2 of 2 matches");
        // Backspace re-runs it: "alph" still matches twice, "alphax" none.
        h.key(keys::KEY_BACKSPACE);
        assert_eq!(h.app().find_counter().split(' ').nth(2), Some("2"), "{}", h.app().find_counter());
        h.type_str("ax");
        assert_eq!(h.app().find_counter(), "Phrase not found");
        assert!(!sent().iter().any(|c| c.starts_with("Key")), "the page saw none of it");
    }

    /// The engine's limit is said in the counter.
    #[test]
    fn a_partial_text_says_so_in_the_counter() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        tell(&mut h, News::Loading(false));
        h.press(lucinanet_shortcuts::code::F, ctrl(false));
        h.type_str("alpha");
        answer_text(&mut h, vec![tblock(&["alpha beta alpha"])], true);
        assert_eq!(h.app().find_counter(), "1 of 2 matches (partial)");
        h.type_str("zz");
        assert_eq!(h.app().find_counter(), "Phrase not found (partial)", "a miss in a cut text may be a miss of the cut");
    }

    /// An answer to another request is an old one, and so is one that comes after the bar went down.
    #[test]
    fn an_old_answer_is_dropped() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        tell(&mut h, News::Loading(false));
        h.press(lucinanet_shortcuts::code::F, ctrl(false));
        let id = h.app().find_ask;
        tell(&mut h, News::PageText { id: id + 1, blocks: vec![tblock(&["alpha"])], partial: false });
        assert!(h.app().find_blocks.is_empty(), "another request's answer is dropped");
        h.key(keys::KEY_ESC);
        tell(&mut h, News::PageText { id, blocks: vec![tblock(&["alpha"])], partial: false });
        assert!(h.app().find_blocks.is_empty(), "an answer after the bar closed is dropped, not kept for later");
    }

    /// Esc (the bar's own key, with the page's text held) closes the bar and drops every block and match; the page never
    /// sees the key; opening again asks again and shows the new text only.
    #[test]
    fn closing_the_bar_drops_the_text() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = find_window(false);
        assert_eq!(h.app().find_counter(), "1 of 2 matches");
        assert!(!h.app().find_blocks.is_empty());
        h.key(keys::KEY_ESC);
        assert!(!sent().iter().any(|c| c.starts_with("Key")), "Esc was the bar's");
        assert!(!h.app().find_open, "the bar is down");
        assert!(h.app().find_blocks.is_empty(), "the blocks are gone with it");
        assert_eq!(h.app().find_counter(), "", "and so are the matches");
        assert!(h.app().finder.matches().is_empty());
        // Up again: the text is asked for afresh, and what comes is what is searched.
        h.press(lucinanet_shortcuts::code::F, ctrl(false));
        assert_eq!(h.app().find_counter(), "", "nothing from the last time");
        answer_text(&mut h, vec![tblock(&["alpha"])], false);
        assert_eq!(h.app().find_counter(), "1 of 1 match", "the query stayed in the field, the text is the new one");
    }

    /// A page that changes under an open bar leaves it with no stale result: another address, a new load, another tab and a
    /// crash each drop the blocks, and an answer to the request made before is dropped. Typing then asks again.
    #[test]
    fn navigation_a_new_load_a_tab_change_and_a_crash_drop_the_text() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let events: [(&str, News); 4] = [
            ("another address", News::Url("https://example.com/elsewhere".into())),
            ("a load starting", News::Loading(true)),
            ("another tab", News::Tabs { tabs: tabs(2), active: 2 }),
            ("a crash", News::Crashed("boom".into())),
        ];
        for (what, news) in events {
            let mut h = find_window(false);
            // The same address again is the same page.
            let old = h.app().find_ask;
            tell(&mut h, News::Url("https://example.com/1".into()));
            assert!(!h.app().find_blocks.is_empty(), "{what}: the same address keeps the text");
            tell(&mut h, news);
            assert!(h.app().find_blocks.is_empty(), "{what}: the blocks are dropped");
            assert_eq!(h.app().find_counter(), "", "{what}: no stale result");
            assert!(h.app().finder.matches().is_empty(), "{what}: no stale matches");
            tell(&mut h, News::PageText { id: old, blocks: vec![tblock(&["alpha alpha alpha"])], partial: false });
            assert!(h.app().find_blocks.is_empty(), "{what}: the old request's answer is dropped");
        }
        // A bar still up asks again when typing resumes.
        let mut h = find_window(false);
        tell(&mut h, News::Url("https://example.com/other".into()));
        sent();
        h.type_str("a");
        let asked = sent();
        assert!(asked.iter().any(|c| c.starts_with("PageText")), "typing on a bar with no text asks for it: {asked:?}");
        // The field still holds "alpha", and the typed "a" made it "alphaa".
        answer_text(&mut h, vec![tblock(&["alphaa b alphaa"])], false);
        assert_eq!(h.app().find_counter(), "1 of 2 matches");
        // Another tab closes the bar: it was the other page's.
        tell(&mut h, News::Tabs { tabs: tabs(2), active: 2 });
        assert!(!h.app().find_open, "the tab change took the bar down");
    }

    /// Both chromes hold the one find state.
    #[test]
    fn both_chromes_hold_the_same_find_state() {
        use lucinanet_shortcuts::code;
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        for glass in [false, true] {
            let mut h = find_window(glass);
            assert!(h.app().find_open, "glass {glass}");
            assert_eq!(h.app().find_counter(), "1 of 2 matches", "glass {glass}");
            h.press(code::F3, keys::Mods::default());
            assert_eq!(h.app().find_counter(), "2 of 2 matches", "glass {glass}");
            let bar = h.app().find_rect.unwrap_or_else(|| panic!("glass {glass}: the strip is drawn over the page"));
            assert!(h.app().page_rect.contains(bar.x, bar.y), "glass {glass}: over the page");
            h.key(keys::KEY_ESC);
            assert!(!h.app().find_open && h.app().find_blocks.is_empty() && h.app().find_rect.is_none(), "glass {glass}");
        }
    }

    /// A page with no text, or none yet: the finder's own words, and nothing breaks on an empty block.
    #[test]
    fn a_page_without_text_says_what_the_finder_says() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = window();
        tell(&mut h, News::Loading(false));
        h.press(lucinanet_shortcuts::code::F, ctrl(false));
        assert_eq!(h.app().find_counter(), "", "no query, no counter");
        h.type_str("x");
        answer_text(&mut h, Vec::new(), false);
        assert_eq!(h.app().find_counter(), "Phrase not found");
        h.key(keys::KEY_ENTER);
        h.press(lucinanet_shortcuts::code::F3, shift_mods());
        assert_eq!(h.app().find_counter(), "Phrase not found", "stepping over nothing changes nothing");
        h.press(lucinanet_shortcuts::code::F, ctrl(false));
        answer_text(&mut h, vec![lucinanet_find::Block { runs: Vec::new() }, tblock(&[""])], false);
        assert_eq!(h.app().find_counter(), "Phrase not found");
    }
}
