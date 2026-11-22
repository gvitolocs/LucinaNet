/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The session error log, wired: what the engine's hooks saw, turned into `lucinanet_errlog::Event`s
//! and written off the engine's thread.
//!
//! `lucinanet-errlog` is the half that can be tested without a browser — it takes its clock, its
//! session number and its directory as arguments. This module is the other half: it reads the clock,
//! owns the queue and the writer thread, decides what a console line and a load status *mean*, and
//! renders `about:errors`. Nothing here is worth a page's content: a host, a file name, a line, a
//! name that was not there and a count. What a hook hands over is redacted by the library's own
//! constructors before it leaves this file, so a leak would have to be built on purpose.
//!
//! The engine thread never waits for a disk. A hook builds an event and puts it on a bounded queue;
//! one thread takes the queue, does the JSON and appends. A full queue drops the event rather than
//! holding Servo: a log that costs a frame is a log that gets turned off, and the errors it would
//! have missed are the same ones the console already printed.
//!
//! `LUCINANET_ERRLOG=0` turns the whole thing off: no queue, no thread, no file, no `about:errors`
//! lines from this session. `privacy.error_log` in the profile's `prefs.json` says the same, and the
//! environment wins over the file because the variable is what a person reaches for when they want it
//! quiet *now* — including in a run whose settings file they cannot open. The file is
//! `<profile>/errors.jsonl`, capped by the library at 256 KiB per generation and 500 lines per
//! session. `about:errors` also carries the controls that empty it: one host's `Clear` and a
//! `Clear all`, each of which asks first with a token only this run can mint, because a link on any
//! web page can reach an `about:` address in this build (see the errlog wiring spec). On Juno
//! every event is also one `browser.*` line on the semantic event hub; `LUCINANET_ERRLOG_JUNO=0`
//! skips that and keeps the file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use servo::WebViewId;

use lucinanet_errlog::{ClearLink, Event, Lines, Log, Page, SessionErrors, Target, clear_files, group_by_host, host_of,
    read_sessions, redact_detail, render_clear_ask, render_html_with};
#[cfg(target_os = "linux")]
use lucinanet_errlog::Kind;

/// The internal page that shows the log, and the root the two controls hang under. Typing any of
/// them in the address bar is the only way to them; the engine serves them itself, see
/// `engine.rs`'s `load_web_resource`.
pub const ABOUT_ERRORS: &str = "about:errors";
/// `about:errors/clear-all`: the page that asks about the whole log.
pub const CLEAR_ALL: &str = "about:errors/clear-all";
/// How many sessions that page lists.
pub const SESSIONS_SHOWN: usize = 5;
/// How long a navigation may take before the log says the page never loaded.
pub const NEVER_LOADED_SECS: u64 = 30;
/// How many events may wait for the writer thread. Past these the newest ones are dropped.
const QUEUE: usize = 64;
/// How long a flush waits for the writer thread, at `about:errors` and at exit.
const FLUSH_WAIT: Duration = Duration::from_millis(500);
/// How long a `Clear` waits. Longer than a flush on purpose: this is a person's click rewriting up
/// to three generations and fsyncing each, on the one path in this module where waiting costs a
/// second of attention rather than a frame of a page.
const CLEAR_WAIT: Duration = Duration::from_secs(5);
/// How many ask pages stay good at once. Older than these is a link that says it has gone stale
/// rather than one that works forever, so nothing minted is kept for the life of the run.
const ASKS_KEPT: usize = 8;
/// The settings file, in the same profile directory as the log. Read for one field, at start only.
const PREFS_FILE: &str = "prefs.json";
/// The file the log lives in, under the profile directory.
pub use lucinanet_errlog::FILE_NAME as LOG_FILE;

/// What `LUCINANET_ERRLOG` says, when it says anything: `None` is the variable not in the environment,
/// `Some(false)` is `=0`, `Some(true)` is every other value.
pub fn env_says() -> Option<bool> {
    std::env::var("LUCINANET_ERRLOG").ok().map(|value| value != "0")
}

/// The environment wins over the file, everywhere and every time: `LUCINANET_ERRLOG=1` puts the log
/// back on for a run whose `prefs.json` says off, which is the only way to answer "is it off because
/// I set it or because something set it" without a settings page.
pub fn wanted(env: Option<bool>, setting: bool) -> bool {
    env.unwrap_or(setting)
}

/// What `privacy.error_log` says in the text of a `prefs.json`, with the settings crate's own rules:
/// no file, unreadable file, or a file that is not a settings file at all is the default, which is on.
/// A broken settings file silencing the diagnostics would be a broken settings file hiding its own
/// damage, and `Err` here is precisely that case: [`lucinanet_settings::Settings::from_json`] reports
/// a file from a newer LucinaNet rather than rewriting it.
pub fn error_log_in_prefs(text: Option<&str>) -> bool {
    match text {
        Some(text) => lucinanet_settings::Settings::from_json(text).map(|(settings, _)| settings.privacy.error_log).unwrap_or(true),
        None => true,
    }
}

/// The same, from the profile directory the engine handed this run. `None` profile has no file to ask.
pub fn error_log_setting(profile: Option<&Path>) -> bool {
    let bytes = match profile {
        None => return true,
        Some(dir) => std::fs::read_to_string(dir.join(PREFS_FILE)).ok(),
    };
    // `Err` is no file, an unreadable one or one that is not a settings file: the default either way,
    // and no disk error here is worth a message, because the log itself says on its own page whether
    // it is being written.
    error_log_in_prefs(bytes.as_deref())
}

/// Why this run writes the log or does not, so that the first line on stderr and the page's own notice
/// say the same thing about which of the two switches is responsible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Writing.
    On,
    /// `LUCINANET_ERRLOG=0`, whatever the settings file says.
    ByEnv,
    /// `privacy.error_log` false, with nothing in the environment about it.
    BySetting,
}

impl Reason {
    /// The reason, in the words both places use.
    pub fn words(self) -> &'static str {
        match self {
            Reason::On => "nothing says otherwise",
            Reason::ByEnv => "LUCINANET_ERRLOG=0",
            Reason::BySetting => "privacy.error_log is false in the profile's prefs.json",
        }
    }
}

/// The whole answer, from the two inputs the run read: which switch is in force.
pub fn reason(env: Option<bool>, setting: bool) -> Reason {
    if wanted(env, setting) {
        Reason::On
    } else if env == Some(false) {
        Reason::ByEnv
    } else {
        Reason::BySetting
    }
}

/// One of the addresses this page owns, read out of a URL the engine was asked to load. Two of the
/// three change nothing at all: the list, and the question the `Clear` link opens. Only
/// [`Route::Confirm`] takes anything away, and it is worth nothing without a token this run minted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// `about:errors`.
    List,
    /// `about:errors/clear-all`, `about:errors/clear?host=<host>`: the page that asks first.
    Ask(Target),
    /// `about:errors/confirm?token=<n>`: the answer to that question, and the only state in the run
    /// this module changes.
    Confirm { token: u64 },
}

/// Is this address one of ours? `None` for anything else, including `about:errors/clear` with no host
/// and `about:errors/confirm` with no token: a link this page never wrote is not a page of ours, and
/// the engine's answer to it is the answer it gives to any address it does not serve.
pub fn route(url: &str) -> Option<Route> {
    let parsed = url::Url::parse(url).ok()?;
    if parsed.scheme() != "about" {
        return None;
    }
    // An `about:` path is opaque and carries no leading slash; a hand-typed address may have one, and
    // both mean the same page.
    let path = parsed.path().trim_start_matches('/');
    match path {
        "errors" => Some(Route::List),
        "errors/clear-all" => Some(Route::Ask(Target::All)),
        "errors/clear" => {
            let (_, host) = parsed.query_pairs().find(|(key, _)| key == "host")?;
            Some(Route::Ask(Target::Host(host.into_owned())))
        },
        "errors/confirm" => {
            let (_, token) = parsed.query_pairs().find(|(key, _)| key == "token")?;
            Some(Route::Confirm { token: token.parse().ok()? })
        },
        _ => None,
    }
}

/// The `Clear` link beside one host's row. The host is escaped because it is a query value, and it
/// comes from a file a person may have edited rather than from [`host_of`].
pub fn href_clear_host(page_host: &str) -> String {
    format!("about:errors/clear?host={}", encode_query_value(page_host))
}

/// The link the ask page carries: the only address that takes anything away, and only for as long as
/// [`Asks`] remembers the number.
pub fn href_confirm(token: u64) -> String {
    format!("about:errors/confirm?token={token}")
}

/// Percent-encode a query value: RFC 3986's unreserved set and nothing else, so that `%`, `+`, `&`,
/// `=`, `#` and a space come back as the characters they were. [`url::Url::query_pairs`] decodes every
/// byte this escapes, and because `+` is escaped rather than passed through, nothing in a host name
/// can turn into a space on the way back.
fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A' ..= b'Z' | b'a' ..= b'z' | b'0' ..= b'9' | b'-' | b'.' | b'_' | b'~' => out.push(*byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The ask pages this run handed out: a number nobody outside the process can guess, bound to the one
/// thing it was minted for, and spent the first time it is used.
///
/// The token is the whole of what makes a `Clear` safe, because nothing else in the request can tell
/// an address bar from a click on somebody else's page. In this rev a web page *can* navigate to an
/// `about:` address: `components/script/navigation.rs` gates only `javascript:`, and the embedder is
/// asked about the request before the fetch dispatches on the scheme
/// (`components/net/fetch/methods.rs`, `request_interceptor.rs`), so an `<a href="about:errors/clear-all">`
/// reaches us. The three signals that might have said otherwise all fail: `is_for_main_frame` is
/// `destination == Document` (`request_interceptor.rs:33`), which a subframe's document is too; the
/// referrer of a page on a local scheme is dropped (`http_loader.rs`, `strip_url_for_use_as_referrer`,
/// `url/lib.rs:104` `is_local_scheme`), so our own page's click looks exactly like an address-bar load
/// and like a `rel="noreferrer"` one; and `target_webview_id` names the web view, not who asked.
///
/// What a web page cannot do is read the number off the page it cannot see: the ask page is a document
/// of ours, cross-origin to anything on the network, and the link with the token in it exists only in
/// this map and on the screen. So the forgeable request is the one that changes nothing, and the one
/// that changes something is unforgeable.
#[derive(Default)]
pub struct Asks {
    counter: u64,
    /// token -> (the count it was minted at, what it authorises).
    pending: BTreeMap<u64, (u64, Target)>,
}

impl Asks {
    /// Mint a token for `target` and hand back its number, for the link the ask page carries.
    pub fn mint(&mut self, target: Target) -> u64 {
        self.counter = self.counter.wrapping_add(1);
        let token = ask_token(self.counter);
        self.pending.insert(token, (self.counter, target));
        // A question asked and left unanswered stops working rather than staying good for the rest of
        // the session: the oldest by the count it was minted at goes first.
        while self.pending.len() > ASKS_KEPT {
            let Some(oldest) = self.pending.iter().min_by_key(|(_, (order, _))| *order).map(|(token, _)| *token) else { break };
            self.pending.remove(&oldest);
        }
        token
    }

    /// Spend a token: what it was minted for, once, and nothing at all the second time.
    pub fn spend(&mut self, token: u64) -> Option<Target> {
        self.pending.remove(&token).map(|(_, target)| target)
    }

    /// How many links are good, for the tests and for `about:errors` when it says nothing about them.
    pub fn outstanding(&self) -> usize {
        self.pending.len()
    }
}

/// The number for the `n`th ask page: `n` and this process's id through a hasher whose keys are random
/// to the process. Both inputs are guessable and the keys are not, which is the only property a token
/// needs — and it is the reason this is a hash rather than `n`: a person who has seen one link of ours
/// must not be able to write the next.
///
/// It is not a secret the person has to keep: it authorises one click, once, and is forgotten when it
/// is spent or when the run ends.
fn ask_token(counter: u64) -> u64 {
    use std::hash::BuildHasher;
    std::hash::RandomState::new().hash_one((counter, std::process::id()))
}

fn now_secs() -> u64 {
    unix_ms() / 1000
}

fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|since| since.as_millis() as u64).unwrap_or_default()
}

/// The number this run's lines carry: one past the highest session in the profile's log, or the clock
/// in seconds for a first run. A counter and not a timestamp, because the page lists "session 12"
/// next to "session 11" and a person reads that as the order they opened the browser in.
pub fn next_session(dir: Option<&Path>, now_secs: u64) -> u32 {
    let previous = dir.map(|dir| read_sessions(dir, usize::MAX)).unwrap_or_default();
    let highest = previous.iter().map(|session| session.session).filter(|session| *session > 0).max();
    match highest {
        Some(highest) => highest.saturating_add(1),
        // No readable session: the clock, which is a number no two runs of one machine share. A
        // session 0 from a file that is there but unreadable is not a session and does not count.
        None => u32::try_from(now_secs).unwrap_or(u32::MAX).max(1),
    }
}

/// Where a script said it broke, out of the engine's console line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConsoleError<'a> {
    /// The script's URL, query and all — the reading of it drops the query.
    pub script: &'a str,
    pub line: u32,
    pub message: &'a str,
}

/// The shape script globalscope hands a console message for an uncaught exception:
/// `Error at <url>:<line>:<col> <message>`. Anything else — a page's own `console.log`, a warning —
/// is not an error and gives `None`.
///
/// The URL is cut at the last two `:`-separated numbers, so an address with a port in it survives.
pub fn parse_console(message: &str) -> Option<ConsoleError<'_>> {
    let rest = message.strip_prefix("Error at ")?;
    // The location holds no space in any real address; the message may hold anything.
    let (location, text) = rest.split_once(' ')?;
    let (before, column) = location.rsplit_once(':')?;
    if column.is_empty() || !column.bytes().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    let (script, line) = before.rsplit_once(':')?;
    if script.is_empty() {
        return None;
    }
    Some(ConsoleError { script, line: line.parse().ok()?, message: text.trim() })
}

/// The name a message says was not there, when it says so: `` `x` is not defined``, `y.x is not a
/// function`, `Cannot read properties of undefined (reading 'x')`. The shapes are the JS engine's
/// own (`components/script/dom/bindings/error.rs`), and they are the only place a missing API shows
/// up: this build has no hook that reports one. The engine quotes the name it does not know, in
/// backticks, so the quotes come off before the name is read.
///
/// `None` for every other message, which the caller logs as a script error instead — one line per
/// exception, never two.
pub fn missing_name(message: &str) -> Option<String> {
    let message = message.trim().trim_end_matches(['.', '!', '?']);
    for phrase in [" is not defined", " is not a function"] {
        if let Some(culprit) = message.strip_suffix(phrase) {
            return trailing_name(culprit.trim_end_matches(['`', '\'', '"']));
        }
    }
    // `Cannot read properties of undefined (reading 'x')`, and the older `Cannot read property 'x' of
    // undefined`: the name is the quoted property either way.
    for marker in ["(reading ", "property "] {
        if let Some(at) = message.find(marker) {
            let rest = message[at + marker.len() ..].trim_start_matches(['\'', '"', '`']);
            let name: String = rest.chars().take_while(|c| !matches!(c, '\'' | '"' | '`' | ')' | ' ')).collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

/// The last run of name characters in `text`: `ReferenceError: a.b is not defined` names `a.b`.
fn trailing_name(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let is_name = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b'.');
    let mut start = bytes.len();
    while start > 0 && is_name(bytes[start - 1]) {
        start -= 1;
    }
    let name = &text[start ..];
    // A leading dot is the end of the ellipsis of a truncated message, not part of a name.
    if name.is_empty() || name.starts_with('.') || name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(name.to_owned())
}

/// The class of one of the engine's own error pages, from its title alone. `visits.rs` knows the two
/// titles the default neterror page is built with; the paragraph inside that page that names the
/// reason (`Could not load the requested page: <reason>`) is never sent to the embedder, so a plain
/// load failure has no class finer than "it did not arrive". A status code is not reachable here at
/// all: a 404 renders the server's own page, which the embedder sees as a loaded document.
pub fn error_class(title: &str) -> &'static str {
    match title.trim() {
        "Certificate error" => "tls",
        _ => "unreachable",
    }
}

/// How often a page host has reached for a name it does not have.
///
/// The count is the library's dedupe key's (`Kind`, page host, redacted name), so the number this
/// returns is the number the written line carries and the log's own doubling works from it.
#[derive(Default)]
pub struct MissingCounts(BTreeMap<(String, String), u32>);

impl MissingCounts {
    /// Count this occurrence and say how many there have been, which is what goes into the event.
    pub fn bump(&mut self, page_url: &str, name: &str) -> u32 {
        let key = (host_of(page_url), redact_detail(name));
        let count = self.0.entry(key).or_default();
        *count = count.saturating_add(1);
        *count
    }
}

/// Which navigations have used up their budget. Pure: the clock arrives at every call, so the thirty
/// seconds are testable without waiting for them.
#[derive(Debug)]
pub struct NeverLoaded<K> {
    budget_secs: u64,
    pending: BTreeMap<K, (u64, String)>,
}

impl<K: Ord + Clone> NeverLoaded<K> {
    pub fn new(budget_secs: u64) -> NeverLoaded<K> {
        NeverLoaded { budget_secs, pending: BTreeMap::new() }
    }

    /// A navigation began at `now_secs`. A second start for the same tab replaces the first: the
    /// person pressed reload, and the page they are waiting for is the new one.
    pub fn started(&mut self, tab: K, url: &str, now_secs: u64) {
        self.pending.insert(tab, (now_secs, url.to_owned()));
    }

    /// The load ended, however it ended: a page that stopped answering is not a page that never
    /// loaded, and an error page is told better by the failed-document hook.
    pub fn finished(&mut self, tab: &K) {
        self.pending.remove(tab);
    }

    /// The tab closed with the page still loading: nothing to say about it.
    pub fn closed(&mut self, tab: &K) {
        self.pending.remove(tab);
    }

    /// The tabs that have been loading for the whole budget, oldest wait first, and are forgotten here
    /// so that one navigation costs one event.
    pub fn expired(&mut self, now_secs: u64) -> Vec<(K, String)> {
        let mut fired: Vec<(K, String)> = self
            .pending
            .iter()
            .filter(|(_, (since, _))| now_secs.saturating_sub(*since) >= self.budget_secs)
            .map(|(tab, (_, url))| (tab.clone(), url.clone()))
            .collect();
        fired.sort_by(|a, b| self.pending.get(&a.0).cmp(&self.pending.get(&b.0)));
        for (index, _) in &fired {
            self.pending.remove(index);
        }
        fired
    }

    /// The tabs waiting, for the tests.
    #[cfg(test)]
    pub fn waiting(&self) -> usize {
        self.pending.len()
    }
}

/// What the writer thread is told to do. `pub` only because `Sink`'s variant holds it: the queue is
/// this crate's own, and nothing outside `ErrLog` and the panic hook puts anything on it.
pub enum Msg {
    Event(Event),
    /// Write what is owed and answer on the channel, so a page or an exit can wait for the file.
    Flush(mpsc::Sender<()>),
    /// Take a target's lines out of the files the writer owns, and answer with what went or with why
    /// nothing did. The writer holds the open handle, so it and only it may rewrite the log: a clear
    /// from anywhere else would leave this thread appending to bytes nobody can open again.
    Clear(Target, mpsc::Sender<std::io::Result<usize>>),
}

/// The log's other end, cheap to clone and safe to hold from a thread that is not the engine's.
#[derive(Clone)]
pub enum Sink {
    /// The lines go to the writer thread, which owns the file.
    Threaded(SyncSender<Msg>),
    /// `LUCINANET_PROFILE_DIR=none`: the lines are memory, so the engine thread appends them itself
    /// and `about:errors` reads them back from the same lock. There is no disk to keep off the path.
    Memory(Arc<Mutex<Log>>),
    /// `LUCINANET_ERRLOG=0`, or a queue whose writer is gone.
    Off,
}

/// Whether this run also talks to Juno's event hub. On by default: the client's own connection is
/// lazy and its writes are non-blocking, so a machine with no hub loses nothing.
#[cfg(target_os = "linux")]
fn juno_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("LUCINANET_ERRLOG_JUNO").map_or(true, |value| value != "0"))
}

/// The hub's name for one kind: its alphabet is lower case, `_` and one dot between words, and a
/// reader filters on it (`browser.*` takes every one of them). `None` for [`lucinanet_errlog::Kind::Truncated`],
/// which the library writes when a session fills its share of the file — not something a page did.
#[cfg(target_os = "linux")]
fn juno_kind(kind: Kind) -> Option<&'static str> {
    Some(match kind {
        Kind::ScriptException => "browser.script_exception",
        Kind::MissingApi => "browser.missing_api",
        Kind::ResourceFailed => "browser.resource_failed",
        Kind::EnginePanic => "browser.engine_panic",
        Kind::PageNeverLoaded => "browser.page_never_loaded",
        Kind::Truncated => return None,
    })
}

/// The same event, as one `browser.*` line on Juno's semantic event hub (`lucina-events`, already in
/// this tree through `lucina-app`). The hub is how Lucina follows the machine without reading a file,
/// and an error the browser knows about is exactly that.
///
/// What goes over it is what the log keeps, minus the message: the kind, the page host as the subject
/// so a reader can filter by site, and the numbers. A script exception's text may quote the page's
/// own source, and a hub line is readable by anything on the machine that is allowed to watch events.
#[cfg(target_os = "linux")]
fn juno_emit(event: &Event) {
    if !juno_on() {
        return;
    }
    let Some(kind) = juno_kind(event.kind) else { return };
    let mut line = lucina_events::emit(kind)
        .field("session", i64::from(event.session))
        .field("tab", i64::from(event.tab))
        .field("count", i64::from(event.count));
    if !event.page_host.is_empty() {
        line = line.subject(&event.page_host);
    }
    if !event.host.is_empty() {
        line = line.field("host", event.host.clone());
    }
    if !event.file.is_empty() {
        line = line.field("file", event.file.clone());
    }
    if event.line > 0 {
        line = line.field("line", i64::from(event.line));
    }
    match event.kind {
        // The detail of these two is a symbol name and a count of seconds, which is the useful part.
        Kind::MissingApi | Kind::PageNeverLoaded => line = line.field("name", event.detail.clone()),
        _ => line = line.private("detail"),
    }
    line.send();
}

/// No hub off Linux, and nothing to gate: the file is the only record.
#[cfg(not(target_os = "linux"))]
fn juno_emit(_event: &Event) {}

impl Sink {
    /// Hand a line over. Never waits.
    pub fn emit(&self, event: Event) {
        if matches!(self, Sink::Off) {
            // Off is the whole record off, hub line included — whether `LUCINANET_ERRLOG=0` or
            // `privacy.error_log` said so. A person who turned the log off did not ask for the same
            // lines on a different channel.
            return;
        }
        juno_emit(&event);
        match self {
            Sink::Threaded(tx) => {
                if tx.try_send(Msg::Event(event)).is_err() {
                    // Full, or the writer thread is gone. Either way the log loses a line and the
                    // browser keeps its frames.
                    eprintln!("lucinanet: the error log is not keeping up; an event was dropped");
                }
            },
            Sink::Memory(log) => {
                let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
                log.record(event);
            },
            Sink::Off => {},
        }
    }

    /// The file holds everything said so far, as far as the writer will let us wait.
    pub fn flush(&self) {
        match self {
            Sink::Threaded(tx) => {
                let (ack_tx, ack_rx) = mpsc::channel();
                if tx.send(Msg::Flush(ack_tx)).is_ok() {
                    let _ = ack_rx.recv_timeout(FLUSH_WAIT);
                }
            },
            Sink::Memory(log) => {
                log.lock().unwrap_or_else(|e| e.into_inner()).finish();
            },
            Sink::Off => {},
        }
    }

    /// This session's lines, for the page when there is no file to read.
    pub fn lines(&self) -> Vec<Event> {
        match self {
            Sink::Memory(log) => log.lock().unwrap_or_else(|e| e.into_inner()).events().to_vec(),
            _ => Vec::new(),
        }
    }

    /// Take the target's lines out and say how many went. `Err` is the reason the page shows, and it
    /// is never a guess: nobody to ask (`LUCINANET_ERRLOG=0` — the caller goes to [`clear_files`]
    /// itself, there being no handle open to fight), a writer that did not answer in [`CLEAR_WAIT`],
    /// or the rewrite refusing to work.
    pub fn clear(&self, target: &Target) -> Result<usize, String> {
        match self {
            Sink::Threaded(tx) => {
                let (ack_tx, ack_rx) = mpsc::channel();
                if tx.send(Msg::Clear(target.clone(), ack_tx)).is_err() {
                    return Err("the log's writer thread is gone".to_string());
                }
                match ack_rx.recv_timeout(CLEAR_WAIT) {
                    Ok(removed) => removed.map_err(|error| error.to_string()),
                    // The writer is alive but slow: the clear may still happen after this answer. The
                    // page reads the file again on its way out, so what it says about the timeout is
                    // about the wait and the table beside it is the truth either way.
                    Err(_) => Err("it did not answer within the wait".to_string()),
                }
            },
            // The same order the writer would take, under the lock that stands in for it.
            Sink::Memory(log) => {
                let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
                log.clear(target).map_err(|error| error.to_string())
            },
            Sink::Off => Err("logging is off".to_string()),
        }
    }
}

/// One run's error log: its session number, the lines' other end, and the counts the hooks keep.
pub struct ErrLog {
    session: u32,
    profile: Option<PathBuf>,
    sink: Sink,
    /// Which of the two switches, if either, said this run writes nothing.
    reason: Reason,
    /// The `Clear` links this run has handed out and not yet spent.
    asks: Asks,
    /// Per tab: the number the window knows it by, which is the number an event carries. `Tabs` owns
    /// these and Servo does not know them, so `Browser::tabs_changed` hands them over.
    tabs: BTreeMap<WebViewId, u32>,
    missing: MissingCounts,
    never: NeverLoaded<WebViewId>,
}

impl ErrLog {
    /// Start the run's log. `None` profile keeps it in memory; `LUCINANET_ERRLOG=0` and a
    /// `privacy.error_log` of false start nothing, the environment first.
    pub fn start(profile: Option<PathBuf>) -> ErrLog {
        let session = next_session(profile.as_deref(), now_secs());
        let setting = error_log_setting(profile.as_deref());
        let env = env_says();
        let reason = reason(env, setting);
        let sink = if !matches!(reason, Reason::On) {
            Sink::Off
        } else if let Some(dir) = &profile {
            let (tx, rx) = mpsc::sync_channel(QUEUE);
            let dir = dir.clone();
            match std::thread::Builder::new().name("errlog".to_owned()).spawn(move || writer(rx, dir, session)) {
                Ok(thread) => {
                    drop(thread);
                    Sink::Threaded(tx)
                },
                // No thread means no writer to own the file: fall back to memory rather than to a
                // queue nobody reads.
                Err(error) => {
                    eprintln!("lucinanet: no error log thread: {error}; this session is kept in memory");
                    Sink::Memory(Arc::new(Mutex::new(Log::memory(session))))
                },
            }
        } else {
            Sink::Memory(Arc::new(Mutex::new(Log::memory(session))))
        };
        ErrLog {
            session,
            profile,
            sink,
            reason,
            asks: Asks::default(),
            tabs: BTreeMap::new(),
            missing: MissingCounts::default(),
            never: NeverLoaded::new(NEVER_LOADED_SECS),
        }
    }

    /// Which switch said this run writes nothing: [`Reason::On`] when it writes.
    pub fn why(&self) -> Reason {
        self.reason
    }

    /// Whether anything this run says will be written down.
    pub fn is_on(&self) -> bool {
        !matches!(self.sink, Sink::Off)
    }

    pub fn session(&self) -> u32 {
        self.session
    }

    /// The queue on its own, for the panic hook — which must not reach into the engine thread's data.
    pub fn sink(&self) -> Sink {
        self.sink.clone()
    }

    /// What has been said is in the file. Waits for the writer, so it belongs at an ending and on the
    /// way to a page, never on a hook.
    pub fn flush(&self) {
        self.sink.flush();
    }

    fn emit(&self, event: Event) {
        self.sink.emit(event);
    }

    /// Which tab Servo's web view is, as the window numbers it. `0` for one the map has not seen yet:
    /// a pop-up's first error can arrive before the tab strip is rebuilt.
    pub fn tab_of(&self, id: WebViewId) -> u32 {
        self.tabs.get(&id).copied().unwrap_or(0)
    }

    /// The window's tab numbers, whole, because a closed tab leaves nothing behind.
    pub fn remember_tabs(&mut self, tabs: impl IntoIterator<Item = (WebViewId, u32)>) {
        self.tabs = tabs.into_iter().collect();
    }

    /// A page error, a missing API, or a line the log has no business with.
    pub fn console(&mut self, id: WebViewId, page_url: &str, message: &str) {
        let Some(found) = parse_console(message) else { return };
        let tab = self.tab_of(id);
        match missing_name(found.message) {
            Some(name) => {
                let count = self.missing.bump(page_url, &name);
                self.emit(Event::missing_api(unix_ms(), self.session, tab, page_url, &name, count));
            },
            None => {
                self.emit(Event::script(unix_ms(), self.session, tab, page_url, found.script, found.line, found.message));
            },
        }
    }

    /// A document that did not come back with a page in it: the class of the engine's own error page.
    /// Only pages with a host have one: an `about:` or `data:` that failed is a typed address, not a
    /// site that broke, and this is a log about sites.
    pub fn failed_document(&mut self, id: WebViewId, url: &str, reason: &str) {
        if host_of(url).is_empty() {
            return;
        }
        let tab = self.tab_of(id);
        self.emit(Event::resource_failed(unix_ms(), self.session, tab, url, url, reason));
    }

    /// A pipeline that died. `tab` arrives from `Browser`, which is the only one who knows the numbers.
    pub fn crashed(&mut self, tab: u32, url: &str, reason: &str) {
        self.emit(Event::engine_panic(unix_ms(), self.session, tab, url, "", reason));
    }

    /// A navigation began. Pages with no host — `about:`, `data:`, a new tab — are not waited for:
    /// they cannot be slow, and a budget spent on one would be a fault with nobody to fix it.
    ///
    /// Called twice over, and the later call wins: once when the bar asks for an address, which is the
    /// only moment a page that never resolves is known about at all, and once when the document
    /// arrives, after which the thirty seconds are about the rest of the page rather than the wait.
    pub fn load_started(&mut self, id: WebViewId, url: &str) {
        if host_of(url).is_empty() {
            return;
        }
        self.never.started(id, url, now_secs());
    }

    pub fn load_finished(&mut self, id: WebViewId) {
        self.never.finished(&id);
    }

    /// Forget a closed tab's navigation and its number.
    pub fn tab_closed(&mut self, id: WebViewId) {
        self.never.closed(&id);
        self.tabs.remove(&id);
    }

    /// Called from the engine's loop: the pages that were still loading when their budget ran out.
    pub fn check_never_loaded(&mut self) {
        let now = now_secs();
        for (id, url) in self.never.expired(now) {
            let tab = self.tab_of(id);
            self.emit(Event::page_never_loaded(unix_ms(), self.session, tab, &url, NEVER_LOADED_SECS as u32));
        }
    }

    /// The `about:errors` document with nothing to report on it: what the engine answers for a bare
    /// `about:errors`, and what the tests read.
    pub fn html(&self) -> String {
        self.page(self.off_note())
    }

    /// The document for one of the addresses this page owns.
    ///
    /// `top_level_click` is the engine's answer to "did a person press this, or did a page ask for
    /// it": a top-level GET whose referrer is not somebody else's document. Only [`Route::Confirm`]
    /// changes anything, so a `false` there refuses the confirm without spending its token — the
    /// cheap gate beside the token, not instead of it, because none of those three signals can tell
    /// the address bar from a link on a site (see [`Asks`]).
    pub fn serve(&mut self, request: &Route, top_level_click: bool) -> String {
        match request {
            Route::List => self.html(),
            Route::Ask(target) => {
                let confirm = href_confirm(self.asks.mint(target.clone()));
                render_clear_ask(target, &confirm, ABOUT_ERRORS)
            },
            Route::Confirm { token } => {
                if !top_level_click {
                    return self.page(Some(
                        "Nothing was cleared: that request did not arrive as a click on this page's own link.".to_string(),
                    ));
                }
                let notice = match self.asks.spend(*token) {
                    Some(target) => self.clear_words(&target),
                    None => "Nothing was cleared: that confirmation link has been used already, or the page that made it is \
                             older than the ones this run still remembers. Open about:errors again and press Clear."
                        .to_string(),
                };
                self.page(Some(notice))
            },
        }
    }

    /// The list, with a `Clear` for every host the page shows, one `Clear all`, where the lines came
    /// from and, when there is something to say, the line that says it.
    fn page(&self, notice: Option<String>) -> String {
        let sessions = self.sessions();
        let dir_name = self.profile.as_ref().map(|dir| dir.display().to_string());
        let lines = match &dir_name {
            Some(dir) => Lines::File(dir.as_str()),
            None => Lines::Memory,
        };
        let hosts: BTreeSet<String> = sessions.iter().flat_map(|session| group_by_host(session)).map(|group| group.page_host).collect();
        let links: Vec<ClearLink> =
            hosts.iter().map(|host| ClearLink { page_host: host.clone(), href: href_clear_host(host) }).collect();
        render_html_with(
            &sessions,
            &Page { clear_hosts: &links, clear_all: Some(CLEAR_ALL), notice: notice.as_deref(), lines },
        )
    }

    /// What the page lists: the profile's file read to the moment of asking, or this session's memory
    /// when there is no file. The flush first is what makes the page true about lines a hook queued a
    /// moment ago.
    fn sessions(&self) -> Vec<SessionErrors> {
        match &self.profile {
            Some(dir) => {
                self.sink.flush();
                read_sessions(dir, SESSIONS_SHOWN)
            },
            None => {
                let events = self.sink.lines();
                if events.is_empty() {
                    return Vec::new();
                }
                vec![SessionErrors { session: self.session, events, corrupt: 0 }]
            },
        }
    }

    /// The line that says nothing is being written, and which switch said so — a person who finds a
    /// page full of lines in a run that writes none is looking at an earlier run, and has to be told.
    fn off_note(&self) -> Option<String> {
        match self.reason {
            Reason::On => None,
            why => Some(format!(
                "Logging is off for this run: {}. What is below is from before it went off, and nothing new is being written.",
                why.words()
            )),
        }
    }

    /// Take the target's lines out and say what happened in the words the page shows.
    fn clear_words(&self, target: &Target) -> String {
        match clear_order(&self.sink, self.profile.as_deref(), target) {
            Ok(0) => match target {
                Target::All => "Nothing to clear: the log holds no lines.".to_string(),
                Target::Host(host) => format!("Nothing to clear: {} holds no lines.", shown_host(host)),
            },
            Ok(count) => {
                let lines = if count == 1 { "1 line".to_string() } else { format!("{count} lines") };
                match target {
                    Target::All => format!("{lines} cleared, out of every file the log keeps."),
                    Target::Host(host) => format!("{lines} of {} cleared, out of every file the log keeps.", shown_host(host)),
                }
            },
            Err(why) => format!("Nothing was cleared: the log would not answer ({why})."),
        }
    }
}

/// Which end owns the file decides who is asked: the writer thread for a log that is running, the
/// directory itself for a run with logging off (no handle open, so nothing to fight with), and the
/// lock for a memory log. `pub` for the tests, which would otherwise have to set a process-global
/// environment variable to reach the `LUCINANET_ERRLOG=0` branch; the only caller is
/// [`ErrLog::clear_words`].
pub fn clear_order(sink: &Sink, profile: Option<&Path>, target: &Target) -> Result<usize, String> {
    match sink {
        Sink::Off => match profile {
            Some(dir) => clear_files(dir, target).map_err(|error| error.to_string()),
            // No profile and no writer: whatever this run said in memory left with it.
            None => Ok(0),
        },
        sink => sink.clear(target),
    }
}

/// A page host as the notice names it, including the group of pages that have none — the same row the
/// table draws, said the same way.
fn shown_host(page_host: &str) -> String {
    if page_host.is_empty() { "the pages with no host".to_string() } else { format!("`{page_host}`") }
}

/// The writer thread: it owns the log, and with it the file, the JSON and the waits.
fn writer(rx: mpsc::Receiver<Msg>, dir: PathBuf, session: u32) {
    // A profile this cannot write into is not a reason to lose the session's lines, nor to hold a
    // queue nobody drains: keep them in memory instead, as `LUCINANET_PROFILE_DIR=none` does.
    let mut log = Log::open(&dir, session).unwrap_or_else(|error| {
        eprintln!("lucinanet: no error log in {}: {error}; this session is kept in memory", dir.display());
        Log::memory(session)
    });
    for msg in rx.iter() {
        match msg {
            Msg::Event(event) => log.record(event),
            Msg::Flush(ack) => {
                log.finish();
                let _ = ack.send(());
            },
            Msg::Clear(target, ack) => {
                // In queue order: everything a hook said before the click is in the file before the
                // file is rewritten, and the handle the log keeps is the one the rewrite left.
                let _ = ack.send(log.clear(&target));
            },
        }
    }
}

/// Where the panic hook writes. `OnceLock` because a hook is installed once, by `engine::run`, and a
/// panic can happen on any thread — including before the delegate exists.
static PANIC_SINK: OnceLock<Sink> = OnceLock::new();

/// The panic hook: the message first, exactly as the hook before it printed it, then one
/// `EnginePanic` line. Chaining rather than replacing keeps `LUCINA_APP_TRACE`, a debugger and the
/// tests' own capture of panics working.
pub fn install_panic_hook(sink: &Sink) {
    if PANIC_SINK.set(sink.clone()).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        let sink = PANIC_SINK.get().expect("set just above");
        sink.emit(panic_event(unix_ms(), info));
        // The process is ending on a panic in most paths: say what was already said, while there is
        // still a thread to write it.
        sink.flush();
    }));
}

/// The event a panic gives: the message, and the source file it came from cut to its last two
/// segments by the library. No argument, no stack, no home directory.
pub fn panic_event(now_ms: u64, info: &std::panic::PanicHookInfo<'_>) -> Event {
    let path = info.location().map(|at| at.file().to_owned()).unwrap_or_default();
    Event::engine_panic(now_ms, 0, 0, "", &path, &panic_text(info.payload()))
}

/// The panic's own words: the payload's first line, with any absolute path in it shortened.
pub fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    let text = match (payload.downcast_ref::<String>(), payload.downcast_ref::<&str>()) {
        (Some(text), _) => text.clone(),
        (_, Some(text)) => (*text).to_owned(),
        _ => return "a panic this build could not read".to_owned(),
    };
    shorten_paths(text.lines().next().unwrap_or_default())
}

/// A message with no long paths in it: a word that reads as an absolute path deeper than a directory
/// and a file keeps only those two, which names the code without naming the person's directory or
/// their machine.
pub fn shorten_paths(message: &str) -> String {
    message
        .split_whitespace()
        .map(|word| {
            let parts: Vec<&str> = word.split('/').filter(|part| !part.is_empty()).collect();
            if !word.starts_with('/') || parts.len() <= 2 {
                return word.to_owned();
            }
            let mut tail = parts[parts.len() - 2 ..].join("/");
            if word.ends_with('/') {
                tail.push('/');
            }
            tail
        })
        .collect::<Vec<String>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_console_line_is_read_out_of_the_engines_shape() {
        let found = parse_console("Error at http://127.0.0.1:8177/app.js:42:9 TypeError: x is not a function")
            .expect("an exception");
        assert_eq!(found.script, "http://127.0.0.1:8177/app.js");
        assert_eq!(found.line, 42);
        assert_eq!(found.message, "TypeError: x is not a function");
    }

    #[test]
    fn a_message_of_its_own_is_not_an_exception() {
        assert_eq!(parse_console("log: hello"), None);
        assert_eq!(parse_console("Error at app.js:notaline:1 boom"), None);
        assert_eq!(parse_console("Error at app.js:12"), None);
        // Only the shape with the two numbers is the engine's.
        assert!(parse_console("Error at file:///home/user/a.js:7:1 boom").is_some());
    }

    #[test]
    fn a_file_url_and_a_port_are_both_read() {
        assert_eq!(
            parse_console("Error at https://example.com:8443/a.js:1:1 boom").map(|found| (found.script, found.line)),
            Some(("https://example.com:8443/a.js", 1))
        );
        assert_eq!(
            parse_console("Error at file:///home/user/www/app.js:7:1 boom").map(|found| (found.script, found.line)),
            Some(("file:///home/user/www/app.js", 7))
        );
        // The engine's own worker name, with a space in it, is not a location: no line, no event.
        assert_eq!(parse_console("Error at inline script:3:4 boom"), None);
    }

    #[test]
    fn a_missing_api_is_named_and_everything_else_is_a_script_error() {
        assert_eq!(missing_name("ReferenceError: requestIdleCallback is not defined").as_deref(), Some("requestIdleCallback"));
        assert_eq!(missing_name("TypeError: window.requestIdleCallback is not a function").as_deref(), Some("window.requestIdleCallback"));
        assert_eq!(
            missing_name("TypeError: Cannot read properties of undefined (reading 'idleCallback')").as_deref(),
            Some("idleCallback")
        );
        assert_eq!(missing_name("TypeError: Cannot read property 'x' of undefined").as_deref(), Some("x"));
        assert_eq!(missing_name("TypeError: x is not a function."), Some("x".to_owned()));
        // Verbatim from the built binary (`proof.sh`, a page that calls a name that is not there):
        // the engine puts the name between backticks, and the quotes are not part of it.
        assert_eq!(missing_name("ReferenceError: `notAFunctionAnywhere` is not defined").as_deref(), Some("notAFunctionAnywhere"));
        assert_eq!(missing_name("TypeError: `window.foo` is not a function").as_deref(), Some("window.foo"));
        assert_eq!(missing_name("SyntaxError: unexpected token"), None);
        assert_eq!(missing_name("RangeError: too many digits 42 is not defined"), None);
    }

    #[test]
    fn a_message_is_counted_per_host_and_name() {
        let mut counts = MissingCounts::default();
        assert_eq!(counts.bump("https://example.com/a", "idleCallback"), 1);
        assert_eq!(counts.bump("https://example.com/b", "idleCallback"), 2);
        assert_eq!(counts.bump("https://other.example/", "idleCallback"), 1);
        assert_eq!(counts.bump("https://example.com/a", "window.x"), 1);
    }

    #[test]
    fn the_thirty_seconds_are_a_pure_clock() {
        let mut never = NeverLoaded::new(NEVER_LOADED_SECS);
        never.started(1, "https://example.com/", 100);
        never.started(2, "https://other.example/", 101);
        assert_eq!(never.expired(129), Vec::<(u32, String)>::new());
        let fired = never.expired(130);
        assert_eq!(fired, vec![(1, "https://example.com/".to_owned())]);
        // Once said, never said again for the same navigation.
        assert_eq!(never.expired(400), vec![(2, "https://other.example/".to_owned())]);
        assert_eq!(never.expired(900), Vec::new());
    }

    #[test]
    fn a_load_that_ends_or_a_reload_does_not_cost_an_event() {
        let mut never = NeverLoaded::new(NEVER_LOADED_SECS);
        never.started(1, "https://example.com/", 0);
        never.finished(&1);
        assert_eq!(never.expired(100), Vec::<(u32, String)>::new());

        never.started(1, "https://example.com/", 0);
        never.started(1, "https://example.com/", 20);
        assert_eq!(never.waiting(), 1);
        assert_eq!(never.expired(30), Vec::new());
        assert_eq!(never.expired(50).len(), 1);

        never.started(2, "https://example.com/", 0);
        never.closed(&2);
        assert_eq!(never.expired(100), Vec::new());
    }

    #[test]
    fn the_session_counts_up_from_the_log_and_off_the_clock_otherwise() {
        assert_eq!(next_session(None, 1_731_400_000), 1_731_400_000);
        let dir = std::env::temp_dir().join(format!("lucinanet-errlog-session-{}", unix_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(next_session(Some(&dir), 1_731_400_000), 1_731_400_000);
        Log::open(&dir, 7).unwrap().record(Event::script(1, 0, 0, "https://example.com/", "https://example.com/a.js", 1, "boom"));
        assert_eq!(next_session(Some(&dir), 1_731_400_000), 8);
        Log::open(&dir, 9).unwrap().record(Event::script(2, 0, 0, "https://example.com/", "https://example.com/a.js", 1, "boom"));
        assert_eq!(next_session(Some(&dir), 1_731_400_000), 10);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_log_is_still_not_session_zero() {
        let dir = std::env::temp_dir().join(format!("lucinanet-errlog-junk-{}", unix_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("errors.jsonl"), "not json at all\n").unwrap();
        assert_eq!(next_session(Some(&dir), 1_731_400_000), 1_731_400_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_panic_keeps_its_words_and_loses_the_home_directory() {
        assert_eq!(
            shorten_paths("panicked at /home/user/Projects/LucinaNet/crates/x/src/engine.rs:7:1"),
            "panicked at src/engine.rs:7:1"
        );
        assert_eq!(
            shorten_paths("index out of bounds: the len is 3 but the index is 10"),
            "index out of bounds: the len is 3 but the index is 10"
        );
        // Two segments is already as short as the rule allows, so it says nothing about the machine.
        assert_eq!(shorten_paths("/etc/passwd"), "/etc/passwd");
        assert_eq!(shorten_paths("/home/user/.cargo/registry/"), ".cargo/registry/");

        let owned: Box<dyn std::any::Any + Send> = Box::new(String::from("/home/user/Projects/LucinaNet/a/b.rs said no\nsecond line"));
        assert_eq!(panic_text(&*owned), "a/b.rs said no");
        let borrowed: Box<dyn std::any::Any + Send> = Box::new("boom");
        assert_eq!(panic_text(&*borrowed), "boom");
        let other: Box<dyn std::any::Any + Send> = Box::new(7u8);
        assert_eq!(panic_text(&*other), "a panic this build could not read");
    }

    #[test]
    fn a_panic_event_names_no_directory_and_no_page_content() {
        let payload: Box<dyn std::any::Any + Send> = Box::new(String::from("the file /home/user/private/notes.txt would not open"));
        let line = Event::engine_panic(1, 4, 2, "https://example.com/a", "crates/lucinanet-servo/src/engine.rs", &panic_text(&*payload)).to_line();
        assert!(line.contains("\"file\":\"src/engine.rs\""), "{line}");
        assert!(line.contains("private"), "the name of the file is the point: {line}");
        assert!(!line.contains("/home/user"), "{line}");
    }

    #[test]
    fn a_script_error_line_holds_no_token_and_names_the_file() {
        // The library's own constructors are the redaction; this is the wiring's promise that it used
        // them with the page and the script it was given.
        let event = Event::script(
            1,
            1,
            2,
            "http://127.0.0.1:8177/star.html?user=alex",
            "http://127.0.0.1:8177/app.js?token=SECRET",
            42,
            "Error: boom https://a.example/x?token=SECRET",
        );
        let line = event.to_line();
        assert!(line.contains("\"host\":\"127.0.0.1\""), "{line}");
        assert!(line.contains("\"file\":\"app.js\""), "{line}");
        assert!(line.contains("\"line\":42"), "{line}");
        assert!(!line.contains("SECRET"), "{line}");
        assert!(!line.contains("user=alex"), "{line}");
        assert_eq!(event.page_host, "127.0.0.1");
    }

    #[test]
    fn a_memory_log_answers_the_page_with_what_it_heard() {
        // `LUCINANET_PROFILE_DIR=none`: no file, and still a page.
        let log = ErrLog::start(None);
        assert!(log.is_on());
        assert!(log.html().contains("Nothing has gone wrong yet"));
        log.sink().emit(Event::script(
            1_731_400_000_000,
            log.session(),
            0,
            "http://127.0.0.1:8177/x.html",
            "http://127.0.0.1:8177/app.js?token=SECRET",
            12,
            "Error: boom",
        ));
        let page = log.html();
        assert!(page.contains("127.0.0.1"), "{page}");
        assert!(page.contains("app.js"));
        assert!(!page.contains("SECRET"), "{page}");
    }

    #[test]
    fn the_page_reads_the_profile_file_it_was_started_with() {
        let dir = std::env::temp_dir().join(format!("lucinanet-errlog-page-{}", unix_ms()));
        {
            let mut log = Log::open(&dir, 3).unwrap();
            log.record(Event::script(1_731_400_000_000, 3, 1, "https://example.com/", "https://example.com/app.js", 42, "boom"));
        }
        let log = ErrLog::start(Some(dir.clone()));
        assert_eq!(log.session(), 4);
        let page = log.html();
        assert!(page.contains("example.com"), "{page}");
        assert!(page.contains("app.js"));
        assert!(page.contains("42"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The hub's own validator says whether a name is one a reader can filter on, and the mapping is
    /// the vocabulary: five kinds, no two the same, and the log's line about itself kept off the hub.
    #[cfg(target_os = "linux")]
    #[test]
    fn every_kind_the_log_writes_has_a_hub_name_of_its_own() {
        let kinds = [
            Kind::ScriptException,
            Kind::MissingApi,
            Kind::ResourceFailed,
            Kind::EnginePanic,
            Kind::PageNeverLoaded,
        ];
        let names: Vec<&str> = kinds.iter().filter_map(|kind| juno_kind(*kind)).collect();
        assert_eq!(names.len(), kinds.len(), "{names:?}");
        assert_eq!(names.iter().collect::<std::collections::BTreeSet<_>>().len(), names.len(), "{names:?}");
        for name in &names {
            assert!(lucina_events::rules::valid_kind(name.as_bytes()), "{name} is not a hub kind");
        }
        assert_eq!(juno_kind(Kind::Truncated), None);
    }

    /// The whole of what the engine asks about: three addresses are ours, and a URL that is not one of
    /// them is not an error page and not a clear either. The leading-slash case is here because a hand-
    /// typed `about:/errors/clear-all` is the same page as the one the bar writes.
    #[test]
    fn only_the_three_addresses_this_page_writes_are_ours() {
        assert_eq!(route("about:errors"), Some(Route::List));
        assert_eq!(route("about:errors/clear-all"), Some(Route::Ask(Target::All)));
        assert_eq!(route("about:/errors/clear-all"), Some(Route::Ask(Target::All)));
        assert_eq!(
            route("about:errors/clear?host=example.com"),
            Some(Route::Ask(Target::Host("example.com".to_string())))
        );
        // The row of pages with no host is a row, so `host=` is a target: it is the missing `host`
        // that is not ours, not the empty one.
        assert_eq!(route("about:errors/clear?host="), Some(Route::Ask(Target::Host(String::new()))));
        assert_eq!(route("about:errors/clear"), None);
        assert_eq!(route("about:errors/confirm?token=17"), Some(Route::Confirm { token: 17 }));
        assert_eq!(route("about:errors/confirm?token=not-a-number"), None);
        assert_eq!(route("about:errors/confirm"), None);
        assert_eq!(route("about:errors/clearer?host=example.com"), None);
        assert_eq!(route("about:errors/clear-all?host=example.com"), Some(Route::Ask(Target::All)));
        assert_eq!(route("about:blank"), None);
        // A site cannot borrow our path: the scheme is the whole of the claim, and a link to
        // `https://example.com/errors/clear-all` is a request for a page on a server, not one for us.
        assert_eq!(route("https://example.com/errors/clear-all"), None);
        assert_eq!(route("https://example.com/about:errors"), None);
    }

    /// A host name is a query value, and the log's own field can hold anything a hand-edited file said
    /// it did. The link must therefore round-trip the bytes it was given, `&` and `#` and `%` and a
    /// space included: a link that decoded into two parameters would clear something else.
    #[test]
    fn a_host_that_is_not_a_plain_name_survives_the_link_it_is_written_into() {
        for host in [
            "example.com",
            "127.0.0.1:8443",
            "",
            "exa mple.com&x=1=y#%+~",
            "xn--80ak6aa92e.com",
            "例え.com",
            "example.com/../../etc/passwd",
        ] {
            let href = href_clear_host(host);
            assert_eq!(route(&href), Some(Route::Ask(Target::Host(host.to_string()))), "{href} did not come back");
        }
        // The plain cases say what a person reads in the status line, with nothing escaped in them.
        assert_eq!(href_clear_host("example.com"), "about:errors/clear?host=example.com");
        assert_eq!(href_confirm(42), "about:errors/confirm?token=42");
        assert_eq!(href_clear_host(""), "about:errors/clear?host=");
    }

    /// The token is the only thing between a link on a web page and a log that is gone: one use, no
    /// number nobody minted works, and the numbers are not a counter a person who saw one can extend.
    #[test]
    fn a_confirmation_is_good_for_one_use_and_for_one_thing_only() {
        let mut asks = Asks::default();
        let first = asks.mint(Target::All);
        let second = asks.mint(Target::Host("example.com".to_string()));
        assert_ne!(first, second);
        assert!(first.abs_diff(second) > 1_000_000, "{first} and {second} are too close to be spread out");
        assert_eq!(asks.outstanding(), 2);
        assert_eq!(asks.spend(first), Some(Target::All), "the token authorises the thing it was minted for");
        assert_eq!(asks.spend(first), None, "and nothing at all the second time");
        assert_eq!(asks.spend(second), Some(Target::Host("example.com".to_string())));
        assert_eq!(asks.spend(42), None, "a number nobody minted authorises nothing");
        assert_eq!(asks.outstanding(), 0);
    }

    #[test]
    fn an_abandoned_question_goes_stale_rather_than_lasting_the_session() {
        let mut asks = Asks::default();
        let oldest = asks.mint(Target::All);
        let mut newest = oldest;
        for _ in 0 .. ASKS_KEPT {
            newest = asks.mint(Target::Host("example.com".to_string()));
        }
        assert_eq!(asks.outstanding(), ASKS_KEPT, "the page remembers the {} it handed out", ASKS_KEPT);
        assert_eq!(asks.spend(oldest), None, "the first ask page's link is the one that went stale");
        assert_eq!(asks.spend(newest), Some(Target::Host("example.com".to_string())), "the last still works");
    }

    #[test]
    fn the_environment_answers_before_the_file_and_says_which_it_was() {
        assert!(wanted(None, true));
        assert!(!wanted(None, false));
        assert!(!wanted(Some(false), false));
        assert!(wanted(Some(true), false), "LUCINANET_ERRLOG=1 puts the log back on for a run whose file says off");
        assert_eq!(reason(None, true), Reason::On);
        assert_eq!(reason(Some(false), true), Reason::ByEnv, "the variable, whatever the file says");
        assert_eq!(reason(None, false), Reason::BySetting);
        assert_eq!(reason(Some(false), false), Reason::ByEnv, "and the variable is the one named when both said no");
        assert_eq!(reason(Some(true), false), Reason::On);
    }

    #[test]
    fn a_prefs_file_that_says_nothing_about_the_log_leaves_it_on() {
        assert!(error_log_in_prefs(None), "no file at all is the default");
        assert!(error_log_in_prefs(Some("")), "so is one that is not JSON");
        assert!(error_log_in_prefs(Some(r#"{"version":1,"privacy":{}}"#)));
        assert!(error_log_in_prefs(Some(r#"{"version":1,"general":{"zoom":900}}"#)));
        // A file from a newer LucinaNet is kept alone rather than rewritten; the run's own default is on.
        assert!(error_log_in_prefs(Some(r#"{"version":99,"privacy":{"error_log":false}}"#)));
        assert!(!error_log_in_prefs(Some(r#"{"version":1,"privacy":{"error_log":false}}"#)));
        assert!(error_log_in_prefs(Some(r#"{"version":1,"privacy":{"error_log":true}}"#)));
        // A value of the wrong shape costs that value, which is the default: on. A file nobody can read
        // must not be the thing that silenced the diagnostics.
        assert!(error_log_in_prefs(Some(r#"{"version":1,"privacy":{"error_log":"no"}}"#)));

        let dir = std::env::temp_dir().join(format!("lucinanet-errlog-prefs-{}", unix_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(error_log_setting(Some(&dir)), "a profile with no prefs.json is the default");
        std::fs::write(dir.join(PREFS_FILE), r#"{"version":1,"privacy":{"error_log":false}}"#).unwrap();
        assert!(!error_log_setting(Some(&dir)));
        assert!(error_log_setting(None), "and a run with no profile has no file to be told off by");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The page, the ask, the token and the removal, end to end through the writer thread — the order a
    /// person's four clicks take, with the file read back from disk at the end to check the promise.
    #[test]
    fn a_cleared_host_leaves_the_other_hosts_and_the_link_that_cleared_it() {
        let dir = std::env::temp_dir().join(format!("lucinanet-errlog-clear-{}", unix_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let mut log = Log::open(&dir, 3).unwrap();
            log.record(Event::script(1_731_400_000_000, 3, 1, "https://example.com/", "https://example.com/app.js", 42, "boom"));
            log.record(Event::script(1_731_400_000_500, 3, 1, "https://other.example/", "https://other.example/app.js", 7, "bang"));
        }
        let mut errlog = ErrLog::start(Some(dir.clone()));

        let page = errlog.serve(&Route::List, true);
        assert!(page.contains("about:errors/clear?host=example.com"), "{page}");
        assert!(page.contains("about:errors/clear?host=other.example"), "{page}");
        assert!(page.contains(CLEAR_ALL), "{page}");
        assert!(page.contains(&format!("{}/errors.jsonl", dir.display())), "the footer names the file it read");

        // The ask changes nothing: not one line is gone before the answer.
        let ask = errlog.serve(&Route::Ask(Target::Host("example.com".to_string())), true);
        assert!(ask.contains("Clear example.com?"), "{ask}");
        assert!(ask.contains("Yes, clear it"), "{ask}");
        assert!(hosts_in(&dir).contains(&"other.example".to_string()), "asking is not clearing");

        let token = token_in(&ask);
        let after = errlog.serve(&Route::Confirm { token }, true);
        assert!(after.contains("1 line of `example.com` cleared"), "{after}");
        let left = hosts_in(&dir);
        assert!(!left.contains(&"example.com".to_string()), "the host's lines are gone: {left:?}");
        assert!(left.contains(&"other.example".to_string()), "and nothing else is: {left:?}");

        // The same link again says it has been used, and takes nothing further.
        let twice = errlog.serve(&Route::Confirm { token }, true);
        assert!(twice.contains("has been used already"), "{twice}");
        assert!(!twice.contains("cleared, out of"), "a spent link clears nothing twice");

        // A number nobody minted is refused the same way.
        let forged = errlog.serve(&Route::Confirm { token: 123456789 }, true);
        assert!(forged.contains("Nothing was cleared"), "{forged}");
        assert_eq!(hosts_in(&dir), left, "and the file did not change");

        // And the same token from a request that is not a person's click: refused, and still unspent,
        // so the page the person is looking at keeps working after a drive-by asks for the same thing.
        let ask = errlog.serve(&Route::Ask(Target::All), true);
        let token = token_in(&ask);
        let refused = errlog.serve(&Route::Confirm { token }, false);
        assert!(refused.contains("did not arrive as a click"), "{refused}");
        assert_eq!(hosts_in(&dir), left, "a refused confirm takes nothing");
        let accepted = errlog.serve(&Route::Confirm { token }, true);
        assert!(accepted.contains("cleared, out of every file"), "{accepted}");
        assert!(hosts_in(&dir).is_empty(), "the clear-all took what was left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `LUCINANET_PROFILE_DIR=none`: the lines are memory, the page is still a page, and a clear still
    /// takes the host out of the run without ending it.
    #[test]
    fn a_memory_log_clears_one_host_and_keeps_answering() {
        let mut errlog = ErrLog::start(None);
        let sink = errlog.sink();
        sink.emit(Event::script(1_731_400_000_000, errlog.session(), 1, "https://example.com/", "https://example.com/a.js", 1, "boom"));
        sink.emit(Event::script(1_731_400_000_100, errlog.session(), 1, "https://other.example/", "https://other.example/a.js", 1, "bang"));
        let ask = errlog.serve(&Route::Ask(Target::Host("example.com".to_string())), true);
        let after = errlog.serve(&Route::Confirm { token: token_in(&ask) }, true);
        assert!(after.contains("1 line of `example.com` cleared"), "{after}");
        assert!(!after.contains("<td>example.com</td>"), "the row is gone from the table: {after}");
        assert!(after.contains("other.example"), "the other host stayed: {after}");
        assert!(errlog.is_on(), "and the run keeps logging");
        sink.emit(Event::script(1_731_400_000_200, errlog.session(), 1, "https://example.com/", "https://example.com/a.js", 2, "boom"));
        assert!(errlog.serve(&Route::List, true).contains("example.com"), "counting starts over from one");
    }

    /// The branch the environment variable selects, reached without touching the process's environment:
    /// logging off still deletes what an earlier run left, and a run with no profile has nothing to
    /// delete and says so rather than failing.
    #[test]
    fn a_run_with_logging_off_still_takes_the_files_away() {
        let dir = std::env::temp_dir().join(format!("lucinanet-errlog-off-{}", unix_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let mut log = Log::open(&dir, 2).unwrap();
            log.record(Event::script(1_731_400_000_000, 2, 1, "https://example.com/", "https://example.com/a.js", 1, "boom"));
            log.record(Event::script(1_731_400_000_100, 2, 1, "https://other.example/", "https://other.example/a.js", 1, "bang"));
        }
        assert_eq!(clear_order(&Sink::Off, Some(&dir), &Target::Host("example.com".to_string())), Ok(1));
        assert_eq!(hosts_in(&dir), vec!["other.example".to_string()]);
        assert_eq!(clear_order(&Sink::Off, None, &Target::All), Ok(0), "nothing on disk and nobody to ask");
        assert_eq!(
            clear_order(&Sink::Off, Some(&dir), &Target::All),
            Ok(1),
            "and the clear-all takes the last of them"
        );
        assert!(hosts_in(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The settings file's answer is what a run honours, and the off switch is visible on the page:
    /// lines an earlier run wrote stay readable, and the page says they are not from this one.
    #[test]
    fn a_run_the_settings_told_to_be_quiet_says_so_on_its_own_page() {
        let dir = std::env::temp_dir().join(format!("lucinanet-errlog-quiet-{}", unix_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(PREFS_FILE), r#"{"version":1,"privacy":{"error_log":false}}"#).unwrap();
        {
            let mut log = Log::open(&dir, 2).unwrap();
            log.record(Event::script(1_731_400_000_000, 2, 1, "https://example.com/", "https://example.com/a.js", 1, "boom"));
        }
        let mut errlog = ErrLog::start(Some(dir.clone()));
        assert!(!errlog.is_on());
        assert_eq!(errlog.why(), Reason::BySetting);
        let page = errlog.serve(&Route::List, true);
        assert!(page.contains("Logging is off for this run"), "{page}");
        assert!(page.contains("privacy.error_log is false"), "{page}");
        // On because it was asked to be, in spite of the file: the environment wins.
        std::fs::write(dir.join(PREFS_FILE), r#"{"version":1,"privacy":{"error_log":true}}"#).unwrap();
        let mut loud = ErrLog::start(Some(dir.clone()));
        assert!(loud.is_on());
        assert_eq!(loud.why(), Reason::On);
        assert!(!loud.serve(&Route::List, true).contains("Logging is off"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The hosts the files hold, straight from disk: the test checks the promise against the bytes the
    /// person's `Clear` was supposed to have taken, not against the page that says it did.
    fn hosts_in(dir: &Path) -> Vec<String> {
        read_sessions(dir, usize::MAX)
            .iter()
            .flat_map(|session| session.events.iter().map(|event| event.page_host.clone()))
            .filter(|host| !host.is_empty())
            .collect()
    }

    /// The token out of the ask page's own link, the way a person gets it: by reading the page.
    fn token_in(page: &str) -> u64 {
        let href = "about:errors/confirm?token=";
        let at = page.find(href).expect("a confirmation link on the page that asks");
        page[at + href.len() ..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().expect("a number")
    }
}
