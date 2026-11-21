//! The engine thread: Servo, its one web view and swgl live here and never leave it. The screen's thread
//! stays free to draw and take input whatever a page does; on one CPU the two share time, but a page
//! that lays out for a second cannot freeze the address bar.
//!
//! Commands come in through [`Engine`]; news goes out through the callback, and finished frames into
//! [`Page`], top row first, which the screen's thread copies when it next draws.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dpi::PhysicalSize;
use euclid::Scale;
use servo::{
    Code, ConsoleLogLevel, CreateNewWebViewRequest, DevicePoint, DownloadId, EmbedderControl, EmbedderControlId,
    EventLoopWaker, InputEvent, JSValue, KeyState, KeyboardEvent, LoadStatus, Location, MouseButton, MouseButtonAction,
    MouseButtonEvent, MouseMoveEvent, PrefValue, Preferences, RenderingContext, SelectElementOption,
    SelectElementOptionOrOptgroup, Servo, ServoBuilder, SimpleDialog, WebResourceLoad, WebView, WebViewBuilder,
    WebViewDelegate, WebViewId, WheelDelta, WheelEvent, WheelMode,
};
use lucinanet_ui::tabs::{MAX_TABS, Tabs};
use lucinanet_ui::title::DocTitle;
use lucinanet_ui::{
    Answer, Cmd, Control, DialogKind, DownloadState, News, Page, SelectItem, Starting, TabInfo, Waker, ZoomStep, rss_kib,
};
use crate::errlog_glue::{self, ErrLog};
use crate::soft::Pacing;
use crate::marks::{Marks, Toggled};
use crate::visits::{How, Visits, now_us};
use crate::zoom::ZoomState;
use url::Url;

use crate::soft::SoftContext;

#[derive(Clone)]
struct Wake(Waker);

impl EventLoopWaker for Wake {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        self.0.wake();
    }
}

/// How the window reaches the engine.
struct Handle {
    commands: mpsc::Sender<Cmd>,
    waker: Waker,
}

impl lucinanet_ui::Engine for Handle {
    fn send(&self, cmd: Cmd) {
        // The engine outlives the window; if it has gone, the app is ending.
        let _ = self.commands.send(cmd);
        self.waker.wake();
    }
}

/// What the delegate saw that the loop has to act on: Servo calls the delegate with the tabs borrowed, so it
/// only notes these down.
enum Event {
    /// A pop-up's web view, built at once as Servo wants; it becomes the shown tab.
    Opened(WebView),
    /// The page closed itself (`window.close()`).
    Closed(WebView),
    Show(WebView, EmbedderControl),
    Hide(WebView, EmbedderControlId),
    Crashed(WebView, String),
    /// A download's news: it goes to the window whether its tab is shown or not.
    News(News),
}

/// One delegate for every tab. News that the window shows for the page (address, title, loading, history)
/// goes out only for the tab that is shown; every tab's changes mark the tab strip for an update.
struct Delegate {
    news: Rc<dyn Fn(News)>,
    /// The page's accessibility tree, when it is on (on Juno, or with `LUCINANET_AX`).
    ax: Option<Arc<Mutex<lucinanet_ax::PageAx>>>,
    frame_ready: Cell<bool>,
    started: Cell<Option<Instant>>,
    frames: Cell<u32>,
    wants_timing: Cell<bool>,
    console: bool,
    /// The shown tab's web view.
    shown: Cell<Option<WebViewId>>,
    tabs_changed: Cell<bool>,
    /// Whether the strip is at `MAX_TABS`, so a pop-up is refused before it is built.
    full: Cell<bool>,
    events: RefCell<Vec<Event>>,
    /// What a pop-up's web view is built with, as the first one.
    context: Rc<SoftContext>,
    scale: f32,
    downloads: Downloads,
    /// Per tab: when each history entry was left, for the depth policy (docs/specs/history-depth.md).
    history: RefCell<HashMap<WebViewId, lucinanet_history::ledger::Ledger>>,
    /// A navigation began or an entry was left: the history depth is sampled next time round the loop (never on a timer).
    sample_depth: Cell<bool>,
    /// The zoom of each site, kept in the profile's `zoom.json` (`zoom.rs`).
    zoom: RefCell<ZoomState>,
    /// The visited pages, kept in the profile's `history.json` (`visits.rs`). Shared with the exit hook, which flushes it.
    visits: Arc<Mutex<Visits<WebViewId>>>,
    /// Per tab: the title of the document it shows (`lucinanet_ui::title`), reset when a navigation commits.
    titles: RefCell<HashMap<WebViewId, DocTitle>>,
    /// The bookmarks, kept in the profile's `bookmarks.json` (`marks.rs`). Shared with the exit hook, which flushes it.
    marks: Arc<Mutex<Marks>>,
    /// The session error log (`errlog_glue`): the counts its hooks keep, and the queue to its writer thread.
    errlog: RefCell<ErrLog>,
}

/// The downloads that have not ended, under the small ids the window knows them by: Servo's ids
/// are UUIDs, which the window has no use for.
#[derive(Default)]
struct Downloads {
    next: Cell<u64>,
    running: RefCell<HashMap<DownloadId, Download>>,
}

struct Download {
    id: u64,
    name: String,
    received: u64,
    total: Option<u64>,
}

impl Downloads {
    fn news(download: &Download, state: DownloadState) -> News {
        News::Download {
            id: download.id,
            name: download.name.clone(),
            received: download.received,
            total: download.total,
            state,
        }
    }

    fn started(&self, servo_id: DownloadId, name: String, total: Option<u64>) -> News {
        let id = self.next.get() + 1;
        self.next.set(id);
        let download = Download {
            id,
            name,
            received: 0,
            total,
        };
        let news = Self::news(&download, DownloadState::Running);
        self.running.borrow_mut().insert(servo_id, download);
        news
    }

    fn progress(&self, servo_id: DownloadId, received: u64, total: Option<u64>) -> Option<News> {
        let mut running = self.running.borrow_mut();
        let download = running.get_mut(&servo_id)?;
        download.received = received;
        download.total = total.or(download.total);
        Some(Self::news(download, DownloadState::Running))
    }

    fn finished(&self, servo_id: DownloadId, result: Result<PathBuf, String>) -> Option<News> {
        let download = self.running.borrow_mut().remove(&servo_id)?;
        let state = match result {
            Ok(path) => DownloadState::Done(path),
            Err(reason) => DownloadState::Failed(reason),
        };
        Some(Self::news(&download, state))
    }

    fn servo_id(&self, id: u64) -> Option<DownloadId> {
        let running = self.running.borrow();
        running
            .iter()
            .find(|(_, download)| download.id == id)
            .map(|(servo_id, _)| *servo_id)
    }
}

impl Delegate {
    fn is_shown(&self, webview: &WebView) -> bool {
        self.shown.get() == Some(webview.id())
    }

    fn event(&self, event: Event) {
        self.events.borrow_mut().push(event);
    }
}

impl Delegate {
    /// What the tab strip and the window show as `webview`'s title: its document's, else its host.
    fn title_of(&self, webview: &WebView) -> String {
        let url = webview.url().map(|u| u.to_string()).unwrap_or_default();
        let titles = self.titles.borrow();
        match titles.get(&webview.id()) {
            Some(t) => t.shown(&url),
            None => lucinanet_ui::title::title_for(webview.page_title().as_deref(), &url),
        }
    }

    /// The title `webview`'s own document reported, "" when it reported none - what it is, not what was shown before it.
    /// Without the host fallback of `title_of`, for the callers that have a fallback of their own.
    fn document_title_of(&self, webview: &WebView) -> String {
        let titles = self.titles.borrow();
        match titles.get(&webview.id()).and_then(|t| t.document_title()) {
            Some(own) => own.to_owned(),
            None => webview.page_title().unwrap_or_default(),
        }
    }

    fn visits(&self) -> std::sync::MutexGuard<'_, Visits<WebViewId>> {
        self.visits.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn marks(&self) -> std::sync::MutexGuard<'_, Marks> {
        self.marks.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the page the web view shows is bookmarked.
    fn is_bookmarked(&self, webview: &WebView) -> bool {
        webview.url().is_some_and(|url| self.marks().is_bookmarked(&url))
    }

    /// The address `webview` shows, "" when it has none yet. What the error log files an event under.
    fn url_of(&self, webview: &WebView) -> String {
        webview.url().map(|url| url.to_string()).unwrap_or_default()
    }
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, webview: WebView) {
        // A background tab's frames are not painted: only the shown tab is in `Page`.
        if self.is_shown(&webview) {
            self.frame_ready.set(true);
        }
    }

    fn notify_url_changed(&self, webview: WebView, url: Url) {
        self.tabs_changed.set(true);
        self.titles.borrow_mut().entry(webview.id()).or_default().url_committed();
        self.visits().committed(webview.id(), &url, now_us());
        // The address committed: the host's zoom applies to this tab, which also drops the last host's.
        let wanted = self.zoom.borrow().percent_for(&url);
        if shown_percent(&webview) != wanted {
            webview.set_page_zoom(lucinanet_zoom::factor(wanted));
        }
        if self.is_shown(&webview) {
            (self.news)(News::Zoom(wanted));
        }
        if !self.is_shown(&webview) {
            return;
        }
        if let Some(ax) = &self.ax {
            ax.lock().unwrap().set_origin(url.origin().ascii_serialization());
        }
        (self.news)(News::Url(url.to_string()));
        (self.news)(News::Bookmarked(self.marks().is_bookmarked(&url)));
    }

    fn notify_accessibility_tree_update(&self, webview: WebView, update: servo::accesskit::TreeUpdate) {
        if let Some(ax) = &self.ax
            && self.is_shown(&webview)
        {
            ax.lock().unwrap().apply(update);
        }
    }

    fn notify_page_title_changed(&self, webview: WebView, title: Option<String>) {
        self.tabs_changed.set(true);
        self.titles.borrow_mut().entry(webview.id()).or_default().title_changed(title.clone());
        self.visits().titled(webview.id(), title.as_deref().unwrap_or_default(), now_us());
        // The engine's own error page is the one load failure this build can see: it keeps the address
        // that failed, so the title is all the embedder gets, and the failed document and its address
        // go to the log together.
        if let Some(title) = &title && crate::visits::is_error_title(title) {
            let url = self.url_of(&webview);
            self.errlog.borrow_mut().failed_document(webview.id(), &url, errlog_glue::error_class(title));
        }
        if self.is_shown(&webview) {
            (self.news)(News::Title(self.title_of(&webview)));
        }
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        self.tabs_changed.set(true);
        match status {
            LoadStatus::Started => self.titles.borrow_mut().entry(webview.id()).or_default().load_started(),
            LoadStatus::Complete | LoadStatus::Stopped => {
                self.titles.borrow_mut().entry(webview.id()).or_default().load_ended(webview.page_title());
            },
            _ => {},
        }
        // The error log waits on the load from here: `Started` is the document's own beginning, so the
        // wait is for the rest of the page, and any end of the load ends the wait.
        match status {
            LoadStatus::Started => self.errlog.borrow_mut().load_started(webview.id(), &self.url_of(&webview)),
            LoadStatus::Complete | LoadStatus::Stopped => self.errlog.borrow_mut().load_finished(webview.id()),
            _ => {},
        }
        if matches!(status, LoadStatus::Complete) {
            let title = self.document_title_of(&webview);
            self.visits().completed(webview.id(), Some(title.as_str()), now_us());
        }
        if !self.is_shown(&webview) {
            return;
        }
        match status {
            LoadStatus::Started => {
                lucinanet_ui::phase("load started");
                self.sample_depth.set(true);
                self.frames.set(0);
                self.started.set(Some(Instant::now()));
                (self.news)(News::Loading(true));
            },
            LoadStatus::Complete | LoadStatus::Stopped => {
                lucinanet_ui::phase("load complete");
                self.wants_timing.set(true);
                if let Some(at) = self.started.take() {
                    eprintln!(
                        "lucinanet: load {status:?} in {} ms, rss {} KiB",
                        at.elapsed().as_millis(),
                        rss_kib()
                    );
                }
                (self.news)(News::Loading(false));
            },
            _ => {},
        }
    }

    fn notify_history_changed(&self, webview: WebView, urls: Vec<Url>, current: usize) {
        // Every tab keeps its own ledger and its own pages; the depth Servo gets is shared out between them.
        self.sample_depth.set(true);
        let urls: Vec<String> = urls.iter().map(Url::to_string).collect();
        self.history.borrow_mut().entry(webview.id()).or_default().update(&urls, current, history_clock());
        if self.is_shown(&webview) {
            (self.news)(News::History {
                back: webview.can_go_back(),
                forward: webview.can_go_forward(),
            });
        }
    }

    fn notify_crashed(&self, webview: WebView, reason: String, _: Option<String>) {
        self.event(Event::Crashed(webview, reason));
    }

    fn notify_closed(&self, webview: WebView) {
        self.event(Event::Closed(webview));
    }

    fn request_create_new(&self, parent: WebView, request: CreateNewWebViewRequest) {
        if self.full.get() {
            // Dropping the request answers Servo with no web view: `window.open` returns null.
            eprintln!("lucinanet: pop-up refused, {MAX_TABS} tabs are open");
            (self.news)(News::Note(format!("A pop-up was blocked: {MAX_TABS} tabs are open")));
            return;
        }
        // Built from the request, so the new page keeps its opener; shown as a tab of its own, as servoshell does.
        let webview = request
            .builder(self.context.clone())
            .hidpi_scale_factor(Scale::new(self.scale))
            .delegate(parent.delegate())
            .build();
        self.event(Event::Opened(webview));
    }

    fn show_embedder_control(&self, webview: WebView, control: EmbedderControl) {
        match control {
            EmbedderControl::SimpleDialog(_) | EmbedderControl::SelectElement(_) => {
                self.event(Event::Show(webview, control));
            },
            // Not drawn yet: each is answered "nothing" at once so the page does not wait for it.
            EmbedderControl::ColorPicker(picker) => {
                eprintln!("lucinanet: colour picker refused");
                picker.submit();
            },
            EmbedderControl::FilePicker(picker) => {
                eprintln!("lucinanet: file picker refused");
                picker.dismiss();
            },
            EmbedderControl::ContextMenu(menu) => {
                eprintln!("lucinanet: context menu refused");
                menu.dismiss();
            },
            // Only a notice that a text field has the keyboard; the window's keys already reach it.
            EmbedderControl::InputMethod(_) => log::info!("lucinanet: input method not shown"),
        }
    }

    fn hide_embedder_control(&self, webview: WebView, id: EmbedderControlId) {
        self.event(Event::Hide(webview, id));
    }

    fn show_console_message(&self, webview: WebView, level: ConsoleLogLevel, message: String) {
        if self.console {
            eprintln!("console {level:?}: {message}");
        }
        // An uncaught exception reaches the log and nothing else does: a page's own `console.error` is
        // its opinion, not a fault, and `errlog_glue` reads the engine's `Error at …` shape out of it.
        if !matches!(level, ConsoleLogLevel::Error) {
            return;
        }
        let url = self.url_of(&webview);
        self.errlog.borrow_mut().console(webview.id(), &url, &message);
    }

    fn load_web_resource(&self, _: WebView, load: WebResourceLoad) {
        // `about:errors`, its `Clear` links and the page that asks are the documents this engine serves
        // itself. Servo asks the embedder about a request before it dispatches on the scheme
        // (`components/net/fetch/methods.rs`), which is why an internal page works here and would not
        // through a registered protocol handler: `about` is one of the schemes Servo refuses an
        // embedder (`components/net/protocols/mod.rs`).
        let Some(request) = errlog_glue::route(load.request.url.as_str()) else { return };
        // The three cheap gates beside the token: a top-level document request, a GET (nothing else
        // this page writes), and no other site's document in front of it. None of the three can tell
        // the bar from a link on a page — see `errlog_glue::Asks` and the section in
        // `docs/specs/wiring-errlog.md` — which is why the confirm step also needs a token this run
        // minted for one thing and one use.
        let referrer_is_somebody_elses = load.request.referrer_url.as_ref().is_some_and(|from| {
            matches!(from.scheme(), "http" | "https" | "ftp" | "file")
        });
        let click = load.request.is_for_main_frame && load.request.method.as_str() == "GET" && !referrer_is_somebody_elses;
        let html = self.errlog.borrow_mut().serve(&request, click);
        let mut response = servo::WebResourceResponse::new(load.request.url.clone().into_url());
        // The value's type is the map's, so it is parsed rather than named: `http` is not a dependency
        // of this crate, and a literal header value cannot fail to parse.
        let _ = response.headers.insert("content-type", "text/html; charset=utf-8".parse().unwrap());
        let mut intercepted = load.intercept(response);
        intercepted.send_body_data(html.into_bytes());
        intercepted.finish();
    }

    fn notify_download_started(&self, _: WebView, id: DownloadId, file_name: String, total: Option<u64>) {
        self.event(Event::News(self.downloads.started(id, file_name, total)));
    }

    fn notify_download_progress(&self, _: WebView, id: DownloadId, received: u64, total: Option<u64>) {
        if let Some(news) = self.downloads.progress(id, received, total) {
            self.event(Event::News(news));
        }
    }

    fn notify_download_finished(&self, _: WebView, id: DownloadId, result: Result<PathBuf, String>) {
        if let Err(reason) = &result {
            eprintln!("lucinanet: download failed: {reason}");
        }
        if let Some(news) = self.downloads.finished(id, result) {
            self.event(Event::News(news));
        }
    }
}

/// A tab: its web view, what it asks of the person in the order it asked, and why it crashed if it did.
struct Tab {
    webview: WebView,
    controls: Vec<(u64, EmbedderControl)>,
    crashed: Option<String>,
}

impl Tab {
    fn new(webview: WebView) -> Tab {
        Tab {
            webview,
            controls: Vec::new(),
            crashed: None,
        }
    }
}

/// The tabs and what they share: one Servo, one rendering context, one delegate.
struct Browser {
    servo: Servo,
    tabs: Tabs<Tab>,
    delegate: Rc<Delegate>,
    user_content: Option<Rc<servo::UserContentManager>>,
    ax: Option<Arc<Mutex<lucinanet_ax::PageAx>>>,
    /// Whether someone reads the accessibility tree: it follows the shown tab.
    ax_on: bool,
    next_control: u64,
}

impl Browser {
    fn news(&self, news: News) {
        (self.delegate.news)(news);
    }

    fn shown(&self) -> &WebView {
        &self.tabs.active().expect("there is always a tab").webview
    }

    fn open(&mut self, url: Option<Url>) {
        if self.tabs.is_full() {
            self.news(News::Note(format!("No new tab: {MAX_TABS} are open, close one first")));
            return;
        }
        let url = url.unwrap_or_else(blank);
        let webview = build(&self.servo, &self.delegate, &self.user_content, url);
        self.add(webview);
    }

    fn add(&mut self, webview: WebView) {
        let previous = self.tabs.active_id();
        match self.tabs.open(Tab::new(webview), true) {
            Ok(_) => self.switched(previous),
            // Only the delegate's pop-ups get here past the check in `open`; dropping the web view closes it.
            Err(_) => self.news(News::Note(format!("A pop-up was blocked: {MAX_TABS} tabs are open"))),
        }
    }

    fn select(&mut self, id: u32) {
        let previous = self.tabs.active_id();
        if previous != Some(id) && self.tabs.select(id) {
            self.switched(previous);
        }
    }

    fn close(&mut self, id: u32) {
        let previous = self.tabs.active_id();
        // A fresh tab is built only when the last one closes, so the browser never stands empty.
        let (servo, delegate, user_content) = (&self.servo, &self.delegate, &self.user_content);
        let fresh = || Tab::new(build(servo, delegate, user_content, blank()));
        let Some(closed) = self.tabs.close(id, fresh) else { return };
        // Its pending dialogs answer Cancel as they drop.
        if self.ax_on && previous == Some(id) {
            closed.webview.set_accessibility_active(false);
        }
        self.delegate.visits().forget(closed.webview.id());
        self.delegate.titles.borrow_mut().remove(&closed.webview.id());
        self.delegate.errlog.borrow_mut().tab_closed(closed.webview.id());
        drop(closed);
        if previous == Some(id) {
            self.switched(None);
        } else {
            self.tabs_changed();
        }
    }

    /// The shown tab changed from `previous`: hide that one, show this one, and tell the window all about it.
    fn switched(&mut self, previous: Option<u32>) {
        if let Some(old) = previous.and_then(|id| self.tabs.get(id)) {
            old.webview.set_focused(false);
            old.webview.hide();
            if self.ax_on {
                old.webview.set_accessibility_active(false);
            }
        }
        let webview = self.shown().clone();
        self.delegate.shown.set(Some(webview.id()));
        webview.show();
        webview.set_focused(true);
        if let Some(ax) = &self.ax {
            let mut ax = ax.lock().unwrap();
            ax.clear();
            if let Some(url) = webview.url() {
                ax.set_origin(url.origin().ascii_serialization());
            }
            if self.ax_on {
                match webview.set_accessibility_active(true) {
                    Some(tree_id) => ax.set_webview_tree(tree_id),
                    None => eprintln!("lucinanet: accessibility could not be turned on"),
                }
            }
        }
        // Its last frame may be long gone from `Page`.
        self.delegate.frame_ready.set(true);
        self.tabs_changed();
        self.news(News::Url(webview.url().map(|u| u.to_string()).unwrap_or_default()));
        self.news(News::Title(self.delegate.title_of(&webview)));
        self.news(News::Loading(loading(&webview)));
        self.news(News::History {
            back: webview.can_go_back(),
            forward: webview.can_go_forward(),
        });
        self.news(News::Zoom(shown_percent(&webview)));
        self.news(News::Bookmarked(self.delegate.is_bookmarked(&webview)));
        if let Some(reason) = self.tabs.active().and_then(|tab| tab.crashed.clone()) {
            self.news(News::Crashed(reason));
        }
        self.control();
    }

    /// One zoom step on the shown tab: from the level it shows, by the Firefox table, kept for its host.
    fn zoom(&mut self, step: ZoomStep) {
        let webview = self.shown().clone();
        let Some(url) = webview.url() else { return };
        let new = self.delegate.zoom.borrow_mut().step(&url, shown_percent(&webview), step);
        webview.set_page_zoom(lucinanet_zoom::factor(new));
        self.news(News::Zoom(new));
    }

    /// The star on the shown tab: its bookmarks go if it has any, else it gets one. The window is told the outcome either way,
    /// and why when the star refused.
    fn toggle_bookmark(&mut self) {
        let webview = self.shown().clone();
        let outcome = match webview.url() {
            Some(url) => {
                // The document's own title, "" when it has none, so the star is never named after the page shown before it.
                let title = self.delegate.document_title_of(&webview);
                self.delegate.marks().toggle(&url, &title, now_us())
            },
            None => Toggled::Refused(crate::marks::Refusal::NotBookmarkable),
        };
        if let Toggled::Refused(why) = outcome {
            self.news(News::Note(why.note()));
        }
        self.news(News::Bookmarked(self.delegate.is_bookmarked(&webview)));
    }

    fn tabs_changed(&self) {
        self.delegate.tabs_changed.set(false);
        self.delegate.full.set(self.tabs.is_full());
        let tabs = self
            .tabs
            .iter()
            .map(|(id, tab)| {
                let url = tab.webview.url().map(|u| u.to_string()).unwrap_or_default();
                TabInfo {
                    id,
                    title: self.delegate.title_of(&tab.webview),
                    registrable_domain: crate::domains::registrable_domain_of(&url),
                    url,
                    loading: loading(&tab.webview),
                }
            })
            .collect();
        // The error log files an event under the tab number a person counts in the strip, and only the
        // strip knows them; the map is rebuilt whole, so a closed tab leaves nothing behind.
        self.delegate.errlog.borrow_mut().remember_tabs(self.tabs.iter().map(|(id, tab)| (tab.webview.id(), id)));
        self.news(News::Tabs {
            tabs,
            active: self.tabs.active_id().unwrap_or(0),
        });
    }

    /// Tell the window what the shown tab asks now, if anything.
    fn control(&self) {
        let tab = self.tabs.active();
        let control = tab.and_then(|tab| tab.controls.first().and_then(|(id, c)| describe(*id, &tab.webview, c)));
        self.news(News::Control(control));
    }

    fn tab_of(&self, webview: &WebView) -> Option<u32> {
        self.tabs.find(|tab| tab.webview.id() == webview.id())
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Opened(webview) => self.add(webview),
            Event::Closed(webview) => {
                if let Some(id) = self.tab_of(&webview) {
                    self.close(id);
                }
            },
            Event::Show(webview, control) => {
                let Some(id) = self.tab_of(&webview) else { return };
                let number = self.next_control;
                self.next_control += 1;
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.controls.push((number, control));
                }
                if self.tabs.active_id() == Some(id) {
                    self.control();
                }
            },
            Event::Hide(webview, servo_id) => {
                let Some(id) = self.tab_of(&webview) else { return };
                if let Some(tab) = self.tabs.get_mut(id) {
                    // Servo ignores an answer after this, so the control is dropped without one.
                    tab.controls.retain(|(_, c)| c.id() != servo_id);
                }
                if self.tabs.active_id() == Some(id) {
                    self.control();
                }
            },
            Event::Crashed(webview, reason) => {
                let Some(id) = self.tab_of(&webview) else { return };
                // A dead pipeline is the engine's own fault on a page, and the page is the whole of
                // what the log is allowed to know about it.
                let url = self.delegate.url_of(&webview);
                self.delegate.errlog.borrow_mut().crashed(id, &url, &reason);
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.crashed = Some(reason.clone());
                }
                if self.tabs.active_id() == Some(id) {
                    self.news(News::Crashed(reason));
                }
            },
            // A download's, out whatever tab started it: the strip shows them all.
            Event::News(news) => self.news(news),
        }
    }

    fn answer(&mut self, number: u64, answer: Answer) {
        let Some(id) = self.tabs.find(|tab| tab.controls.iter().any(|(n, _)| *n == number)) else { return };
        let Some(tab) = self.tabs.get_mut(id) else { return };
        let Some(at) = tab.controls.iter().position(|(n, _)| *n == number) else { return };
        let (_, control) = tab.controls.remove(at);
        match (control, answer) {
            (EmbedderControl::SimpleDialog(SimpleDialog::Alert(dialog)), _) => dialog.confirm(),
            (EmbedderControl::SimpleDialog(SimpleDialog::Confirm(dialog)), Answer::Ok(_)) => dialog.confirm(),
            (EmbedderControl::SimpleDialog(SimpleDialog::Prompt(mut dialog)), Answer::Ok(text)) => {
                if let Some(text) = text {
                    dialog.set_current_value(&text);
                }
                dialog.confirm();
            },
            (EmbedderControl::SimpleDialog(dialog), _) => dialog.dismiss(),
            (EmbedderControl::SelectElement(mut select), Answer::Chose(option)) => {
                select.select(vec![option]);
                select.submit();
            },
            // Dropped, a `<select>` keeps what it had: that is Cancel.
            (other, _) => drop(other),
        }
        if self.tabs.active_id() == Some(id) {
            self.control();
        }
    }
}

fn build(servo: &Servo, delegate: &Rc<Delegate>, user_content: &Option<Rc<servo::UserContentManager>>, url: Url) -> WebView {
    let mut builder = WebViewBuilder::new(servo, delegate.context.clone());
    if let Some(manager) = user_content {
        builder = builder.user_content_manager(manager.clone());
    }
    builder
        .url(url)
        .hidpi_scale_factor(Scale::new(delegate.scale))
        .delegate(delegate.clone())
        .build()
}

/// The page zoom a web view shows now, in whole percent.
fn shown_percent(webview: &WebView) -> lucinanet_zoom::Percent {
    (webview.page_zoom() * 100.0).round() as lucinanet_zoom::Percent
}

/// Stopped counts as done, as the bar's reload button has it.
fn loading(webview: &WebView) -> bool {
    matches!(webview.load_status(), LoadStatus::Started | LoadStatus::HeadParsed)
}

fn blank() -> Url {
    Url::parse("about:blank").expect("a valid URL")
}

/// A control as plain data for the window, which cannot hold Servo's objects. Only dialogs and `<select>`s
/// are kept for the window (`show_embedder_control`).
fn describe(number: u64, webview: &WebView, control: &EmbedderControl) -> Option<Control> {
    let control = match control {
        EmbedderControl::SelectElement(select) => {
            let rect = select.position();
            let option = |o: &SelectElementOption| SelectItem::Option {
                id: o.id,
                label: o.label.clone(),
                disabled: o.is_disabled,
            };
            let mut items = Vec::new();
            for entry in select.options() {
                match entry {
                    SelectElementOptionOrOptgroup::Option(o) => items.push(option(o)),
                    SelectElementOptionOrOptgroup::Optgroup { label, options } => {
                        items.push(SelectItem::Group(label.clone()));
                        items.extend(options.iter().map(option));
                    },
                }
            }
            Control::Select {
                id: number,
                at: (rect.min.x, rect.min.y, rect.width(), rect.height()),
                items,
                selected: select.selected_options(),
            }
        },
        EmbedderControl::SimpleDialog(dialog) => {
            let kind = match dialog {
                SimpleDialog::Alert(_) => DialogKind::Alert,
                SimpleDialog::Confirm(_) => DialogKind::Confirm,
                SimpleDialog::Prompt(prompt) => DialogKind::Prompt(prompt.current_value().to_owned()),
            };
            // An opaque origin (a data: URL, say) shows the whole address's scheme, never the page's own words.
            let origin = webview
                .url()
                .map(|url| match url.origin() {
                    origin @ url::Origin::Tuple(..) => origin.ascii_serialization(),
                    url::Origin::Opaque(_) => format!("{}:", url.scheme()),
                })
                .unwrap_or_default();
            Control::Dialog {
                id: number,
                origin,
                message: dialog.message().to_owned(),
                kind,
            }
        },
        _ => return None,
    };
    Some(control)
}

const NAVIGATION_TIMING: &str = "JSON.stringify(performance.getEntriesByType('navigation').concat(performance.getEntriesByType('paint')).map(e => [e.name, e.domainLookupStart, e.domainLookupEnd, e.connectStart, e.secureConnectionStart, e.connectEnd, e.requestStart, e.responseStart, e.responseEnd, e.domInteractive, e.domContentLoadedEventEnd, e.domComplete, e.loadEventEnd, e.startTime]))";

/// The web APIs LucinaJS does not implement, in a script that runs at document start in every frame of every page:
/// `requestIdleCallback`, `scheduler`, the `PerformanceObserver` entry types. Each is defined only where the engine
/// has none, and Web Animations is deliberately left alone (docs/compat-shims.md). `LUCINANET_COMPAT_SHIMS=0` leaves
/// the page as it was (crates/lucinanet-servo/src/compat.js).
const COMPAT_JS: &str = include_str!("compat.js");

/// Off by default in Servo, needed by real sites: GitHub's file view dies without IntersectionObserver and
/// adoptedStyleSheets. servoshell's experimental list, less what needs GL or a store we lack.
const WEB_FEATURES: [&str; 10] = [
    "dom_adoptedstylesheet_enabled",
    "dom_fontface_enabled",
    // IndexedDB: off by default in Servo, on in Chromium and Firefox, and sites expect it (lucinanet, 2026-11-04).
    "dom_indexeddb_enabled",
    "dom_intersection_observer_enabled",
    // Web Animations: this one pref gates the engine's `Animation`, `KeyframeEffect` and `Element.prototype.animate`
    // (not `document.timeline`, which is never gated). YouTube reads `Animation` while its own polyfill loader is still
    // in flight, and threw `ReferenceError` in every run where that loader lost the race (docs/compat-shims.md, B05).
    "dom_web_animations_enabled",
    "layout_css_backdrop_filter_enabled",
    "layout_css_columns_enabled",
    "layout_css_container_queries_enabled",
    "layout_css_font_variations_enabled",
    "layout_css_masking_enabled",
];

pub fn start(starting: Starting) -> Box<dyn lucinanet_ui::Engine> {
    let (commands, inbox) = mpsc::channel();
    let waker = Waker::default();
    let handle = Handle {
        commands,
        waker: waker.clone(),
    };
    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || run(starting, inbox, waker))
        .expect("the engine thread");
    Box::new(handle)
}

fn run(starting: Starting, inbox: Receiver<Cmd>, waker: Waker) {
    let Starting {
        width,
        height,
        scale,
        first,
        page,
        news,
    } = starting;
    let size = PhysicalSize::new(width, height);
    let start = Instant::now();
    lucinanet_ui::phase("engine thread");
    let pacing = Arc::new(Pacing::default());
    let frame_waker = waker.clone();
    pacing.set_engine_wake(move || frame_waker.wake());
    let context = Rc::new(SoftContext::new(size, pacing.clone()));
    let mut low_memory = LowMemory::default();
    let mut preferences = Preferences::default();
    preferences.user_agent = lucinanet_ui::USER_AGENT.to_owned();
    // Each page kept for Back holds all its memory; further back reloads. How many follows the memory left
    // (docs/specs/history-depth.md, `HistoryDepth`).
    preferences.session_history_max_length = initial_history_depth();
    for pref in WEB_FEATURES {
        preferences.set_value(pref, PrefValue::Bool(true));
    }
    // The page's accessibility tree, served with actions by node id through Juno's AccessKit adapter (lucina-ax,
    // crates/lucinanet-ax): on Juno at /run/lucina/ax/<pid>.sock, as every app, where lucina-devd reads the app in
    // front; elsewhere only when `LUCINANET_AX=<socket path>` asks. Off otherwise: the tree costs a walk of the page
    // per change.
    let ax_socket = std::env::var_os("LUCINANET_AX").map(std::path::PathBuf::from);
    let ax_wanted = ax_socket.is_some() || std::path::Path::new("/run/lucina").is_dir();
    if ax_wanted {
        preferences.set_value("accessibility_enabled", PrefValue::Bool(true));
    }
    // An experiment from the command line, as servoshell's --pref: name=value,name=value.
    for pref in std::env::var("LUCINANET_PREFS").unwrap_or_default().split(',').filter(|p| !p.is_empty()) {
        let (name, value) = pref.split_once('=').unwrap_or((pref, "true"));
        preferences.set_value(name, PrefValue::from_booleanish_str(value));
    }
    lucinanet_ui::phase("rendering context");
    // Servo's own time profiler, every `LUCINANET_PROFILE` seconds to stderr: script, style, layout, paint.
    let mut opts = servo::Opts::default();
    if let Some(every) = std::env::var("LUCINANET_PROFILE").ok().and_then(|s| s.parse().ok()) {
        opts.time_profiling = Some(servo::OutputOptions::Stdout(every));
    }
    // Servo's diagnostics, as servoshell's -Z: `LUCINANET_DEBUG=relayout-event,profile-script-events`.
    if let Ok(debug) = std::env::var("LUCINANET_DEBUG") {
        let _ = opts.debug.extend_from_string(&debug);
    }
    // The profile: cookies, HSTS, localStorage, IndexedDB and the storage registry live here and survive a restart.
    // `LUCINANET_PROFILE_DIR=none` keeps everything in memory, as before 2026-11-04.
    lucinanet_ui::frametimeline::configure(profile_dir().as_deref());
    opts.config_dir = profile_dir();
    if let Some(dir) = &opts.config_dir {
        if let Err(error) = std::fs::create_dir_all(dir) {
            eprintln!("lucinanet: no profile at {}: {error}; nothing will be kept", dir.display());
            opts.config_dir = None;
        }
    }
    // Where `zoom.json` lives: the same profile, so none when there is none.
    let profile = opts.config_dir.clone();
    // The session error log (`errlog_glue.rs`, docs/specs/wiring-errlog.md): `<profile>/errors.jsonl`,
    // one line per thing that went wrong, written by a thread of its own so that no page error costs
    // this loop a disk wait. `LUCINANET_PROFILE_DIR=none` keeps the lines in memory, and
    // `LUCINANET_ERRLOG=0` or `privacy.error_log` false keeps nothing at all — the variable wins.
    let errlog = errlog_glue::ErrLog::start(profile.clone());
    // The queue on its own, outliving the delegate: a panic can happen on a thread that has no
    // delegate to ask, and it is the one fault worth a line wherever it lands.
    let errlog_sink = errlog.sink();
    errlog_glue::install_panic_hook(&errlog_sink);
    // Where the lines go, said once, because a person answering "the site is broken" has to know what
    // file to be asked for.
    if errlog.is_on() {
        match &profile {
            // The session number, said once, so that "the lines are mine" is answerable from the file.
            Some(dir) => {
                eprintln!(
                    "lucinanet: error log at {} (session {})",
                    dir.join(errlog_glue::LOG_FILE).display(),
                    errlog.session()
                );
            },
            None => eprintln!("lucinanet: error log in memory only, session {} (no profile directory)", errlog.session()),
        }
    } else {
        // Which of the two switches said no: a person who set one and sees the other in the same
        // terminal is looking at two different files, and the run should not pretend otherwise.
        eprintln!("lucinanet: error log off ({})", errlog.why().words());
    }
    // The HTTP cache on disk (`http-cache.redb`, through Servo's store). Without it every warm load refetches everything the
    // server allowed to cache; Firefox wins warm loads 2-8x on that. On Juno the file goes to tmpfs (`/run/lucina/net`), not
    // into the profile on `/data`: a successful fsync on `/data` does not prove the data is on disk yet (writeback errors are
    // not reported), and this is a bursty writer that can stall behind a sync. The cache is lost at reboot, which is fine.
    // This is INTERIM: it goes back under the profile on /data when Juno kernel patch 1242 (write errors reported at fsync)
    // and the writeback steps are on the installed generation.
    // Elsewhere it stays in the profile. `LUCINANET_HTTP_CACHE_DIR` (a directory) overrides the location;
    // `LUCINANET_HTTP_CACHE_MB` bounds it (default 256 MB in the profile, 64 MB on tmpfs since that is RAM; 0 turns it off);
    // the oldest entries go first.
    {
        let dir_override = std::env::var("LUCINANET_HTTP_CACHE_DIR").ok();
        let mb_override = std::env::var("LUCINANET_HTTP_CACHE_MB").ok();
        if let Some((file, bytes, home)) = http_cache_location(opts.config_dir.as_deref(), juno_cache_dir(), dir_override.as_deref(), mb_override.as_deref()) {
            let dir = file.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
            match std::fs::create_dir_all(&dir) {
                Ok(()) => {
                    preferences.set_value("network_http_disk_cache", PrefValue::Str(file.to_string_lossy().into_owned()));
                    preferences.set_value("network_http_disk_cache_size", PrefValue::UInt(bytes));
                    eprintln!("lucinanet: HTTP cache at {} ({} MB bound, {})", file.display(), bytes / (1024 * 1024), home.reason());
                },
                Err(error) => eprintln!("lucinanet: no HTTP cache directory at {}: {error}", dir.display()),
            }
        } else if http_cache_bytes(mb_override.as_deref()).is_none() {
            eprintln!("lucinanet: HTTP disk cache off (LUCINANET_HTTP_CACHE_MB=0)");
        } else {
            eprintln!("lucinanet: HTTP disk cache in memory only (no profile directory)");
        }
    }
    // <video>, <audio> and WebAudio on Juno's own Rust decoders (crates/lucinanet-media, docs/media.md), not servo-media's dummy.
    #[cfg(feature = "media")]
    servo::set_media_backend(lucinanet_media::backend);
    #[cfg(feature = "media")]
    crate::media_images::install();
    crate::css_prefs::apply();
    let servo = ServoBuilder::default()
        .preferences(preferences)
        .opts(opts)
        .event_loop_waker(Box::new(Wake(waker.clone())))
        .build();
    // What the media backend decides — `no sound:`, `first frame after N ms`, `MSE sound stopped:` — goes through `log::info!`
    // and `log::warn!` in lucinanet-media. `setup_logging` below builds env_logger from `RUST_LOG` and clamps the whole process
    // to its max level; with `RUST_LOG` unset env_logger's default is error, so those lines were dropped in a normal run.
    // Default `RUST_LOG` to `error,lucinanet_media=info`: only the media target reaches info/warn on stderr, every other
    // crate stays at error exactly as before (env_logger drops an unmatched target outright, so a bare `lucinanet_media=info`
    // would have silenced third-party errors too). The operator's `RUST_LOG` still wins.
    if std::env::var_os("RUST_LOG").filter(|v| !v.is_empty()).is_none() {
        // SAFETY: still in single-threaded setup before `setup_logging` and before the engine's threads start; env_logger reads
        // `RUST_LOG` right after, and nothing else mutates the environment here. Edition 2024 makes `set_var` unsafe for this.
        unsafe { std::env::set_var("RUST_LOG", "error,lucinanet_media=info") };
    }
    servo.setup_logging();
    lucinanet_ui::phase("servo built");
    let news: Rc<dyn Fn(News)> = Rc::new(move |n| news(n));
    let ax = ax_wanted.then(|| Arc::new(Mutex::new(lucinanet_ax::PageAx::new())));
    let (ax_control, ax_inbox) = mpsc::channel::<lucinanet_ax::Control>();
    let delegate = Rc::new(Delegate {
        news,
        ax: ax.clone(),
        frame_ready: Cell::new(false),
        started: Cell::new(None),
        frames: Cell::new(0),
        wants_timing: Cell::new(false),
        console: std::env::var_os("LUCINANET_CONSOLE").is_some(),
        shown: Cell::new(None),
        tabs_changed: Cell::new(false),
        full: Cell::new(false),
        events: RefCell::new(Vec::new()),
        context: context.clone(),
        scale,
        downloads: Downloads::default(),
        history: RefCell::default(),
        sample_depth: Cell::new(false),
        zoom: RefCell::new(ZoomState::load(profile.as_deref())),
        visits: Arc::new(Mutex::new(Visits::load(profile.as_deref()))),
        titles: RefCell::default(),
        marks: Arc::new(Mutex::new(Marks::load(profile.as_deref()))),
        errlog: RefCell::new(errlog),
    });
    {
        // The process ends with `_exit`: what the history and the bookmarks still hold back is written here.
        let visits = delegate.visits.clone();
        lucinanet_ui::on_exit(Box::new(move || visits.lock().unwrap_or_else(|e| e.into_inner()).flush(now_us())));
        let marks = delegate.marks.clone();
        lucinanet_ui::on_exit(Box::new(move || marks.lock().unwrap_or_else(|e| e.into_inner()).flush(now_us())));
        // The error log's lines are on a queue and a thread of their own; a short wait for the writer
        // is the difference between `about:errors` tomorrow saying what happened and saying nothing.
        let errlog = errlog_sink.clone();
        lucinanet_ui::on_exit(Box::new(move || errlog.flush()));
    }
    let mut history_depth = HistoryDepth::default();
    // `LUCINANET_USER_CSS=<file>` and `LUCINANET_USER_JS=<file>`: a user style sheet or script on every page,
    // for experiments and diagnosis.
    let css = std::env::var("LUCINANET_USER_CSS").ok();
    let js = std::env::var("LUCINANET_USER_JS").ok();
    // The compat shims of `COMPAT_JS` run on every page unless `LUCINANET_COMPAT_SHIMS=0` (docs/compat-shims.md).
    let compat = std::env::var("LUCINANET_COMPAT_SHIMS").as_deref() != Ok("0");
    let user_content = (compat || css.is_some() || js.is_some()).then(|| {
        let manager = Rc::new(servo::UserContentManager::new(&servo));
        if let Some(path) = css {
            if let (Ok(source), Ok(url)) = (std::fs::read_to_string(&path), lucinanet_ui::Url::from_file_path(&path)) {
                manager.add_stylesheet(Rc::new(servo::UserStyleSheet::new(source, url)));
            }
        }
        // First of the scripts, so the user script and the page itself both see what it defines.
        if compat {
            manager.add_script(Rc::new(servo::UserScript::new(COMPAT_JS.to_string(), None)));
        }
        if let Some(path) = js {
            if let Ok(source) = std::fs::read_to_string(&path) {
                manager.add_script(Rc::new(servo::UserScript::new(source, Some(path.into()))));
            }
        }
        manager
    });
    let mut browser = Browser {
        servo,
        tabs: Tabs::new(),
        delegate: delegate.clone(),
        user_content,
        ax: ax.clone(),
        ax_on: false,
        next_control: 1,
    };
    let webview = build(&browser.servo, &delegate, &browser.user_content, first);
    browser.add(webview);
    lucinanet_ui::phase("web view built");
    if let Some(ax) = &ax {
        let server = match &ax_socket {
            Some(path) => lucinanet_ax::lucina_ax::Server::bind_at(path).map(Some),
            None => lucinanet_ax::lucina_ax::Server::bind(),
        };
        // The tree is built only while someone reads it (lucinanet_ax::serve turns it on and off).
        match server {
            Ok(Some(server)) => {
                let waker = waker.clone();
                let ax_control = Mutex::new(ax_control);
                let served = lucinanet_ax::serve(server, ax.clone(), move |control| {
                    // Servo lives on this thread: the socket's thread only queues the request.
                    let _ = ax_control.lock().unwrap().send(control);
                    waker.wake();
                });
                match served {
                    Ok(()) => eprintln!("lucinanet: accessibility socket open, tree off until read"),
                    Err(error) => eprintln!("lucinanet: accessibility socket thread: {error}"),
                }
            },
            Ok(None) => {},
            Err(error) => eprintln!("lucinanet: accessibility socket: {error}"),
        }
    }
    eprintln!(
        "lucinanet: engine up in {} ms, rss {} KiB",
        start.elapsed().as_millis(),
        rss_kib()
    );

    let tracing = std::env::var_os("LUCINA_APP_TRACE").is_some();
    let (mut loops, mut spin_us, mut paint_total, mut last_report) = (0u32, 0u128, 0u128, Instant::now());
    loop {
        loop {
            match inbox.try_recv() {
                Ok(cmd) => {
                    lucinanet_ui::frametimeline::command(&cmd);
                    if matches!(cmd, Cmd::Move(..) | Cmd::Button { .. } | Cmd::Wheel { .. } | Cmd::Key { .. }) {
                        pacing.input();
                    }
                    match cmd {
                        Cmd::NewTab(url) => browser.open(url),
                        Cmd::SelectTab(id) => browser.select(id),
                        Cmd::CloseTab(id) => browser.close(id),
                        Cmd::Zoom(step) => browser.zoom(step),
                        Cmd::LoadTyped { url, search } => {
                            delegate.visits().intend(browser.shown().id(), How::Typed { search });
                            // The wait for a page starts here and not at the document: an address that
                            // never resolves never reaches the engine's load hooks at all.
                            delegate.errlog.borrow_mut().load_started(browser.shown().id(), &url.to_string());
                            browser.shown().load(url);
                        },
                        Cmd::HistoryQuery { id, text } => {
                            let entries = delegate.visits().candidates(&text);
                            (delegate.news)(News::HistoryHits { id, entries });
                            let entries = delegate.marks().candidates(&text);
                            (delegate.news)(News::BookmarkHits { id, entries });
                        },
                        Cmd::ToggleBookmark => browser.toggle_bookmark(),
                        cmd @ (Cmd::Back | Cmd::Forward) => {
                            delegate.visits().intend(browser.shown().id(), How::Traversal);
                            apply(browser.shown(), cmd, &delegate.news, &delegate.downloads);
                        },
                        Cmd::Answer(id, answer) => browser.answer(id, answer),
                        cmd => apply(browser.shown(), cmd, &delegate.news, &delegate.downloads),
                    }
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    delegate.visits().flush(now_us());
                    delegate.marks().flush(now_us());
                    delegate.errlog.borrow().flush();
                    return;
                },
            }
        }
        while let Ok(control) = ax_inbox.try_recv() {
            match control {
                // The tree is the shown tab's; `Browser::switched` moves it with the tab.
                lucinanet_ax::Control::On => {
                    browser.ax_on = true;
                    match browser.shown().set_accessibility_active(true) {
                        Some(tree_id) => {
                            if let Some(ax) = &ax {
                                ax.lock().unwrap().set_webview_tree(tree_id);
                            }
                        },
                        None => eprintln!("lucinanet: accessibility could not be turned on"),
                    }
                },
                lucinanet_ax::Control::Off => {
                    browser.ax_on = false;
                    browser.shown().set_accessibility_active(false);
                },
                lucinanet_ax::Control::Act(request) => browser.servo.forward_accessibility_action(request),
            }
        }
        let spun = Instant::now();
        browser.servo.spin_event_loop();
        spin_us += spun.elapsed().as_micros();
        loops += 1;
        // Taken out first: acting on them calls into Servo, which may call the delegate again.
        let events = std::mem::take(&mut *delegate.events.borrow_mut());
        for event in events {
            browser.event(event);
        }
        if delegate.tabs_changed.get() {
            browser.tabs_changed();
        }
        let webview = browser.shown().clone();
        if delegate.wants_timing.take() && lucinanet_ui::timing() {
            webview.evaluate_javascript(NAVIGATION_TIMING, |result| eprintln!("phase navigation {result:?}"));
        }
        if delegate.frame_ready.take() {
            let painted = Instant::now();
            webview.paint();
            let paint_us = painted.elapsed().as_micros() as u64;
            let presented = Instant::now();
            context.present();
            let present_us = presented.elapsed().as_micros() as u64;
            // What a frame costs the loop, which is what the pacing is about: the whole of it, not the paint.
            pacing.painted(Duration::from_micros(paint_us + present_us));
            paint_total += (paint_us + present_us) as u128;
            let copied = Instant::now();
            context.with_frame(|px, size, top_down| {
                let mut page = page.lock().unwrap();
                copy_frame(px, size, top_down, &mut page);
            });
            let copy_us = copied.elapsed().as_micros() as u64;
            if tracing {
                eprintln!("lucinanet: paint {paint_us} us, present {present_us} us, copy {copy_us} us");
            }
            if delegate.frames.replace(delegate.frames.get() + 1) == 0 {
                lucinanet_ui::phase("first frame painted");
            }
            lucinanet_ui::frametimeline::engine_frame(paint_us, present_us, copy_us);
            (delegate.news)(News::Painted { paint_us, present_us, copy_us });
            (delegate.news)(News::Frame);
        }
        if tracing && last_report.elapsed() >= Duration::from_secs(1) {
            eprintln!("lucinanet: each second: {loops} loops, spin {} ms, paint {} ms", spin_us / 1000, paint_total / 1000);
            (loops, spin_us, paint_total, last_report) = (0, 0, 0, Instant::now());
        }
        low_memory.check(&browser.servo, &webview, &delegate.news);
        delegate.visits().tick(now_us());
        delegate.marks().tick(now_us());
        // The loop is the error log's clock: every pass asks which page has now been loading for the
        // whole budget, which is the only way a page that never answers gets noticed.
        delegate.errlog.borrow_mut().check_never_loaded();
        if delegate.sample_depth.take() {
            let live: Vec<WebViewId> = browser.tabs.iter().map(|(_, tab)| tab.webview.id()).collect();
            history_depth.apply(&browser.servo, &delegate.history, &live);
        }
        waker.wait(Duration::from_millis(250));
    }
}

/// swgl's framebuffer is bottom row first, BGRA in memory, so its u32s are already 0xAARRGGBB.
fn copy_frame(px: &[u32], size: PhysicalSize<u32>, top_down: bool, page: &mut Page) {
    let (w, h) = (size.width as usize, size.height as usize);
    page.w = size.width;
    page.h = size.height;
    page.px.resize(w * h, 0);
    if top_down {
        page.px.copy_from_slice(px);
        return;
    }
    for (dst, src) in page.px.chunks_exact_mut(w).zip(px.chunks_exact(w).rev()) {
        dst.copy_from_slice(src);
    }
}

fn apply(webview: &WebView, cmd: Cmd, news: &Rc<dyn Fn(News)>, downloads: &Downloads) {
    match cmd {
        // Servo answers with notify_download_finished, which tells the window.
        Cmd::CancelDownload(id) => {
            if let Some(servo_id) = downloads.servo_id(id) {
                webview.cancel_download(servo_id);
            }
        },
        Cmd::Dump(path) => {
            let news = news.clone();
            webview.evaluate_javascript(lucinanet_ui::DUMP_JS, move |result| {
                match result {
                    Ok(JSValue::String(json)) => {
                        if let Err(error) = std::fs::write(&path, json) {
                            eprintln!("lucinanet: dump: {error}");
                        }
                    },
                    other => eprintln!("lucinanet: dump: {other:?}"),
                }
                news(News::Dumped);
            });
        },
        Cmd::Load(url) => webview.load(url),
        // The tabs' own, which `Browser` takes before a command reaches a web view.
        Cmd::NewTab(_)
        | Cmd::SelectTab(_)
        | Cmd::CloseTab(_)
        | Cmd::Zoom(_)
        | Cmd::Answer(..)
        | Cmd::LoadTyped { .. }
        | Cmd::HistoryQuery { .. }
        | Cmd::ToggleBookmark => {},
        Cmd::Back => {
            webview.go_back(1);
        },
        Cmd::Forward => {
            webview.go_forward(1);
        },
        Cmd::Reload => webview.reload(),
        Cmd::Move(x, y) => {
            webview.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(
                DevicePoint::new(x, y).into(),
            )));
        },
        Cmd::Button { down, x, y } => {
            let action = if down {
                MouseButtonAction::Down
            } else {
                MouseButtonAction::Up
            };
            webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                action,
                MouseButton::Primary,
                DevicePoint::new(x, y).into(),
            )));
        },
        Cmd::Wheel { y, x, y_at } => {
            let delta = WheelDelta {
                x: 0.0,
                y,
                z: 0.0,
                mode: WheelMode::DeltaPixel,
            };
            webview.notify_input_event(InputEvent::Wheel(WheelEvent::new(
                delta,
                DevicePoint::new(x, y_at).into(),
            )));
        },
        Cmd::Key { key, modifiers } => {
            for state in [KeyState::Down, KeyState::Up] {
                webview.notify_input_event(InputEvent::Keyboard(KeyboardEvent::new_without_event(
                    state,
                    key.clone(),
                    Code::Unidentified,
                    Location::Standard,
                    modifiers,
                    false,
                    false,
                )));
            }
        },
    }
}

/// What is left of the machine's memory. There is no swap on Juno, so running out takes everything down.
fn available_mib() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = meminfo.lines().find(|l| l.starts_with("MemAvailable:"))?;
    Some(line.split_whitespace().nth(1)?.parse::<u64>().ok()? / 1024)
}

/// Below this the caches go, below `CLOSE_PAGE_MIB` the page does, and below `QUIT_MIB` the browser does.
/// `LUCINANET_LOW_MEMORY_SCALE` scales them, for testing.
const FREE_CACHES_MIB: u64 = 500;
const CLOSE_PAGE_MIB: u64 = 300;
const QUIT_MIB: u64 = 150;

/// Seconds on the engine's own monotonic clock, for the history ledger.
fn history_clock() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs()
}

fn total_mib() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
    Some(line.split_whitespace().nth(1)?.parse::<u64>().ok()? / 1024)
}

/// The depth now, from memory and age, or `LUCINANET_HISTORY_DEPTH=n` for an experiment. `tabs` holds the ages of each
/// tab (behind, ahead). Memory is read here, when a navigation starts or an entry is left, never on a timer; a pressure
/// level from the OS would replace the sample (docs/specs/history-depth.md, "Which layer owns the signal"). Servo has one
/// preference for all tabs, so the memory above the reserve is shared between them (`shared_available`) and the deepest
/// tab sets the value.
fn history_depth(tabs: &[(Vec<u64>, Vec<u64>)]) -> i64 {
    if let Some(fixed) = std::env::var("LUCINANET_HISTORY_DEPTH").ok().and_then(|s| s.parse::<i64>().ok()) {
        return fixed;
    }
    use lucinanet_history::policy::{fresh_entries, pressure_from_available, shared_available, side_depth, ttl_secs};
    let (Some(total), Some(available)) = (total_mib(), available_mib()) else { return 2 };
    let pressure = pressure_from_available(available);
    let cap = side_depth(total, shared_available(total, available, tabs.len() as u64), pressure) as usize;
    // The value is a capacity per side, not a count: with every entry fresh it is the cap itself (so the entry about to
    // be added is not trimmed at once); with stale ones it is the fresh ones and room for one more.
    let side = |ages: &[u64]| {
        let fresh = fresh_entries(ages, ttl_secs(total));
        if fresh == ages.len() { cap } else { cap.min(fresh + 1) }
    };
    tabs.iter().map(|(past, future)| side(past).max(side(future))).max().unwrap_or(cap) as i64
}

fn initial_history_depth() -> i64 {
    history_depth(&[])
}

/// Sets Servo session_history_max_length from the policy when a navigation starts or an entry is left. Servo trims the
/// oldest pages beyond it at the navigation (constellation `trim_history`), so a lower value drops the oldest first and
/// Back to them reloads.
#[derive(Default)]
struct HistoryDepth {
    set: Option<i64>,
}

impl HistoryDepth {
    fn apply(&mut self, servo: &Servo, ledgers: &RefCell<HashMap<WebViewId, lucinanet_history::ledger::Ledger>>, live: &[WebViewId]) {
        let mut ledgers = ledgers.borrow_mut();
        ledgers.retain(|id, _| live.contains(id));
        let now = history_clock();
        let tabs: Vec<(Vec<u64>, Vec<u64>)> = live.iter().map(|id| ledgers.get(id).map_or_else(Default::default, |l| l.ages(now))).collect();
        let depth = history_depth(&tabs);
        // Released before the call: if set_preference ever reaches the delegate at once, notify_history_changed borrows the map.
        drop(ledgers);
        // A change of memory is seen when a load starts and applied at the end of that loop pass, so it takes effect for the
        // navigation AFTER the one that triggered it (Servo trims at a navigation).
        if self.set != Some(depth) {
            eprintln!("lucinanet: history depth {depth} over {} tabs (was {:?})", live.len(), self.set);
            servo.set_preference("session_history_max_length", PrefValue::Int(depth));
            self.set = Some(depth);
        }
    }
}

#[derive(Default)]
struct LowMemory {
    checked: Option<Instant>,
    caches_freed: bool,
    page_closed: bool,
}

impl LowMemory {
    fn check(&mut self, servo: &Servo, webview: &WebView, news: &Rc<dyn Fn(News)>) {
        if self.checked.is_some_and(|at| at.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.checked = Some(Instant::now());
        let Some(available) = available_mib() else { return };
        let scale: u64 = std::env::var("LUCINANET_LOW_MEMORY_SCALE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        if available >= FREE_CACHES_MIB * scale {
            self.caches_freed = false;
            self.page_closed = false;
            return;
        }
        if !self.caches_freed {
            self.caches_freed = true;
            eprintln!("lucinanet: low memory, {available} MiB left: freeing caches");
            servo.network_manager().clear_cache();
            // The kept pages go with the depth policy, which `HistoryDepth::apply` sets at the next navigation (Servo trims only then).
            servo_allocator_purge();
        }
        if available < CLOSE_PAGE_MIB * scale && !self.page_closed {
            self.page_closed = true;
            eprintln!("lucinanet: low memory, {available} MiB left: closing the page");
            webview.load(Url::parse("about:blank").expect("a valid URL"));
            news(News::Crashed("Not enough memory for this page. It was closed.".into()));
            servo_allocator_purge();
        }
        if available < QUIT_MIB * scale {
            // Better the browser than the whole machine.
            eprintln!("lucinanet: low memory, {available} MiB left: quitting");
            std::process::exit(3);
        }
    }
}

/// Gives freed pages back to the system now.
fn servo_allocator_purge() {
    // SAFETY: the name is a valid NUL-terminated jemalloc control, which takes no arguments.
    unsafe {
        tikv_jemalloc_sys::mallctl(
            c"arena.4096.purge".as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        );
    }
}

/// Brimstone's structural function cache lives in the profile (`js-fn-cache`, size-capped by the engine): scripts compiled
/// once are reused on the next visit and across a player's releases. The engine turns the cache on when
/// `LUCINANET_JS_FN_CACHE_DIR` is set, so this sets it to the profile's, unless it is set already or the profile is `none`
/// (then nothing is kept). SpiderMonkey builds ignore the variable.
///
/// Opt-in with `LUCINANET_JS_FN_CACHE=1` until the engine partitions the cache per top-level site: a cache shared across
/// sites lets one site time another's scripts, which leaks history. Measurements only until then.
///
/// # Safety
/// No other thread may be running: `main` calls this first, before it starts any.
pub(crate) unsafe fn set_js_fn_cache_dir() {
    if std::env::var_os("LUCINANET_JS_FN_CACHE_DIR").is_some() || std::env::var("LUCINANET_JS_FN_CACHE").as_deref() != Ok("1") {
        return;
    }
    if let Some(dir) = profile_dir() {
        // SAFETY: the caller's contract: the process is single-threaded, so nothing reads the environment meanwhile.
        unsafe { std::env::set_var("LUCINANET_JS_FN_CACHE_DIR", dir.join("js-fn-cache")) };
    }
}

/// Where the profile lives: `LUCINANET_PROFILE_DIR`, else `/data/lucina/net` on Juno (as other apps' `/data/lucina/<app>`),
/// else `$XDG_DATA_HOME/lucinanet` or `~/.local/share/lucinanet` on a desktop.
/// The HTTP disk cache bound in bytes from `LUCINANET_HTTP_CACHE_MB` (unset or not a number: 256 MB; 0: no disk cache).
fn http_cache_bytes(megabytes: Option<&str>) -> Option<u64> {
    let mb = megabytes.and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(256);
    (mb > 0).then(|| mb.saturating_mul(1024 * 1024))
}

/// Why the HTTP disk cache file is where it is (logged once at start).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CacheHome {
    /// Juno's tmpfs, an INTERIM measure (see `http_cache_location`).
    Tmpfs,
    /// The profile directory (every other machine).
    Profile,
    /// `LUCINANET_HTTP_CACHE_DIR`.
    Override,
}

impl CacheHome {
    fn reason(self) -> &'static str {
        match self {
            CacheHome::Tmpfs => "tmpfs, interim: /data writeback errors are not reported yet",
            CacheHome::Profile => "profile",
            CacheHome::Override => "LUCINANET_HTTP_CACHE_DIR",
        }
    }
}

/// Juno's tmpfs directory for the HTTP disk cache, or None on every other machine. Juno is the host with
/// `/run/lucina`, the directory of the AccessKit adapter's sockets (crates/lucinanet-ax); `/run` is tmpfs there.
/// INTERIM; see `http_cache_location`.
fn juno_cache_dir() -> Option<&'static std::path::Path> {
    std::path::Path::new("/run/lucina").is_dir().then_some(std::path::Path::new("/run/lucina/net"))
}

/// Where the HTTP disk cache file goes and how big it may be, or None when there is no disk cache. Pure; the caller
/// creates the directory. `profile` is None when there is no profile directory (`LUCINANET_PROFILE_DIR=none` or an
/// unusable one); then only an explicit `dir_override` (`LUCINANET_HTTP_CACHE_DIR`, ignored when empty) gives a cache.
/// Otherwise the directory is `dir_override`, else `tmpfs_dir` (Some only on Juno), else the profile. Size:
/// `megabytes_override` (`LUCINANET_HTTP_CACHE_MB`, see `http_cache_bytes`; 0 means off) wins, else 64 MB on tmpfs and 256 MB elsewhere.
/// The tmpfs location is INTERIM: it goes back under the profile on `/data` when Juno kernel patch 1242 (write errors reported
/// at fsync) and the writeback steps are on the installed generation. The third value says why the file is where it is.
fn http_cache_location(
    profile: Option<&std::path::Path>,
    tmpfs_dir: Option<&std::path::Path>,
    dir_override: Option<&str>,
    megabytes_override: Option<&str>,
) -> Option<(PathBuf, u64, CacheHome)> {
    let dir_override = dir_override.map(str::trim).filter(|d| !d.is_empty());
    let (dir, default_mb, home) = match (dir_override, profile) {
        (Some(dir), _) => (PathBuf::from(dir), 256, CacheHome::Override),
        (None, None) => return None,
        (None, Some(profile)) => match tmpfs_dir {
            Some(tmpfs) => (tmpfs.to_path_buf(), 64, CacheHome::Tmpfs),
            None => (profile.to_path_buf(), 256, CacheHome::Profile),
        },
    };
    let bytes = match megabytes_override {
        Some(_) => http_cache_bytes(megabytes_override)?,
        None => default_mb * 1024 * 1024,
    };
    Some((dir.join("http-cache.redb"), bytes, home))
}

fn profile_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    match std::env::var_os("LUCINANET_PROFILE_DIR") {
        Some(dir) if dir == "none" => return None,
        Some(dir) if !dir.is_empty() => return Some(PathBuf::from(dir)),
        _ => {},
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(home).join("Library/Application Support/LucinaNet"));
    }
    if std::path::Path::new("/data").is_dir() {
        return Some(PathBuf::from("/data/lucina/net"));
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))?;
    Some(data.join("lucinanet"))
}

#[cfg(test)]
mod http_cache_tests {
    use super::{CacheHome, http_cache_bytes, http_cache_location, juno_cache_dir};
    use std::path::{Path, PathBuf};

    #[test]
    fn the_bound_defaults_to_256_mb_and_zero_turns_the_cache_off() {
        assert_eq!(http_cache_bytes(None), Some(256 * 1024 * 1024));
        assert_eq!(http_cache_bytes(Some("junk")), Some(256 * 1024 * 1024));
        assert_eq!(http_cache_bytes(Some(" 64 ")), Some(64 * 1024 * 1024));
        assert_eq!(http_cache_bytes(Some("0")), None);
        assert_eq!(http_cache_bytes(Some("18446744073709551615")), Some(u64::MAX));
    }

    const MB: u64 = 1024 * 1024;

    fn loc(
        profile: Option<&str>,
        tmpfs: Option<&str>,
        dir: Option<&str>,
        mb: Option<&str>,
    ) -> Option<(PathBuf, u64, CacheHome)> {
        http_cache_location(profile.map(Path::new), tmpfs.map(Path::new), dir, mb)
    }

    #[test]
    fn the_logged_reason_names_the_home() {
        assert_eq!(CacheHome::Tmpfs.reason(), "tmpfs, interim: /data writeback errors are not reported yet");
        assert_eq!(CacheHome::Profile.reason(), "profile");
        assert_eq!(CacheHome::Override.reason(), "LUCINANET_HTTP_CACHE_DIR");
    }

    #[test]
    fn profile_only_is_256_mb_in_the_profile() {
        assert_eq!(
            loc(Some("/p"), None, None, None),
            Some((PathBuf::from("/p/http-cache.redb"), 256 * MB, CacheHome::Profile))
        );
    }

    #[test]
    fn tmpfs_present_is_64_mb_there() {
        assert_eq!(
            loc(Some("/p"), Some("/run/lucina/net"), None, None),
            Some((PathBuf::from("/run/lucina/net/http-cache.redb"), 64 * MB, CacheHome::Tmpfs))
        );
    }

    #[test]
    fn juno_is_the_host_with_run_lucina_and_the_cache_goes_under_it() {
        let juno = std::path::Path::new("/run/lucina").is_dir();
        assert_eq!(juno_cache_dir().is_some(), juno);
        assert_eq!(juno_cache_dir(), juno.then(|| Path::new("/run/lucina/net")));
    }

    #[test]
    fn the_cache_file_is_inside_the_chosen_directory_whatever_the_home() {
        // The caller creates the parent of the returned file, so it must be the directory the home names.
        for (got, dir) in [
            (loc(Some("/p"), None, None, None), "/p"),
            (loc(Some("/p"), Some("/run/lucina/net"), None, None), "/run/lucina/net"),
            (loc(None, None, Some("/x"), Some("64")), "/x"),
        ] {
            let (file, _, home) = got.unwrap();
            assert_eq!(file.parent(), Some(Path::new(dir)), "{home:?}");
            assert_eq!(file.file_name().unwrap(), "http-cache.redb");
        }
    }

    #[test]
    fn dir_override_wins_over_tmpfs() {
        assert_eq!(
            loc(Some("/p"), Some("/run/lucina/net"), Some("/x"), None),
            Some((PathBuf::from("/x/http-cache.redb"), 256 * MB, CacheHome::Override))
        );
    }

    #[test]
    fn megabytes_override_wins_on_tmpfs() {
        assert_eq!(
            loc(Some("/p"), Some("/run/lucina/net"), None, Some("100")),
            Some((PathBuf::from("/run/lucina/net/http-cache.redb"), 100 * MB, CacheHome::Tmpfs))
        );
    }

    #[test]
    fn zero_megabytes_turns_it_off() {
        assert_eq!(loc(Some("/p"), None, None, Some("0")), None);
        assert_eq!(loc(Some("/p"), Some("/run/lucina/net"), Some("/x"), Some("0")), None);
    }

    #[test]
    fn no_profile_and_no_override_is_no_cache_even_with_tmpfs() {
        assert_eq!(loc(None, Some("/run/lucina/net"), None, None), None);
        assert_eq!(
            loc(None, None, Some("/x"), None),
            Some((PathBuf::from("/x/http-cache.redb"), 256 * MB, CacheHome::Override))
        );
    }

    #[test]
    fn an_empty_dir_override_is_unset() {
        assert_eq!(
            loc(Some("/p"), Some("/run/lucina/net"), Some(""), None),
            Some((PathBuf::from("/run/lucina/net/http-cache.redb"), 64 * MB, CacheHome::Tmpfs))
        );
        assert_eq!(loc(None, None, Some(""), None), None);
    }
}
