/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The typed schema: what the settings page holds, the default each field ships with, the fixing that keeps a
//! value inside what the browser can act on, and the JSON the file holds.
//!
//! The defaults are Firefox's where Firefox has the setting (`PORTING.md` carries the table of pref, value and
//! file:line behind every one), Chromium only as a cross-check, and LucinaNet's own choice where neither applies.
//! They are what a person gets on a fresh profile, and also what a dropped or clamped setting falls back to: the
//! loader and [`Settings::validate`] both go through [`Settings::fix`], and one field's bad value never drags
//! another down with it.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde_json::Value;

use crate::{CURRENT_VERSION, Warning};

/// The whole settings page: one section per group of controls, plus the keys this build has no field for.
///
/// The default is each section's own default, and every section's default is the value Firefox ships; `PORTING.md`
/// carries the pref behind each.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Settings {
    /// What opening the browser does, and which languages sites are told we speak.
    pub general: General,
    /// The address field's search engines and what it suggests while typing.
    pub search: Search,
    /// The tracking header, cookies, the blocklist, and what to forget on exit.
    pub privacy: Privacy,
    /// Where a saved file goes and whether to ask first.
    pub downloads: Downloads,
    /// Theme, zoom, minimum font size, bookmarks bar.
    pub appearance: Appearance,
    /// How links open and how closing behaves.
    pub tabs: Tabs,
    /// Top-level keys we do not know (written by a newer LucinaNet), kept so saving does not erase them.
    pub unknown: BTreeMap<String, Value>,
}

/// Startup, home page and the languages offered to sites.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct General {
    /// What the browser shows when it opens.
    pub startup: Startup,
    /// The page `Startup::HomePage` and the home button go to; anything the wiring can hand to a URL parser.
    pub home_page: String,
    /// The `Accept-Language` offer list, most preferred first.
    pub accept_languages: Vec<String>,
}

/// What opening the browser does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Startup {
    /// Show [`General::home_page`] (Firefox's `browser.startup.page` 1, @firefox.js:340).
    #[default]
    HomePage,
    /// Put back the windows and tabs of the last run (Firefox's 2, Chromium's "Continue where you left off").
    RestoreSession,
    /// Show the empty page (Firefox's 4, Chromium's "Open the New Tab page").
    Blank,
}

/// The address field: which engine a search uses, the engines on offer, and what gets suggested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Search {
    /// The [`Engine`] a search uses; after [`Settings::validate`] it names one of `engines`.
    pub default_engine: String,
    /// The engines on offer, the first being the fallback when `default_engine` names nothing.
    pub engines: Vec<Engine>,
    /// Ask the engine for completions over the network (Firefox's "Provide search suggestions"). Off by default: no
    /// request leaves the machine per keystroke unless the person turns this on.
    pub suggest_remote: bool,
    /// Suggest pages from the history.
    pub suggest_history: bool,
    /// Suggest bookmarks.
    pub suggest_bookmarks: bool,
}

/// One search engine: where a typed word goes, and where completions come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Engine {
    /// The name the file and this build agree on: `[a-z0-9_-]`, 1 to 32 of them.
    pub id: String,
    /// The name the settings page and the address dropdown show.
    pub name: String,
    /// The shortcut typed before the words ("ddg cats"), or `None` for no shortcut.
    pub keyword: Option<String>,
    /// The search URL, holding `{searchTerms}` and starting with `https://` or `http://`.
    pub template: String,
    /// The completion URL, holding `{searchTerms}`, or `None` when this engine suggests nothing.
    pub suggest_url: Option<String>,
}

/// What a page is told about us and what it may leave behind.
///
/// The default is Firefox's: no `DNT` header (`privacy.donottrackheader.enabled`, @StaticPrefList.yaml:18630), the
/// tracking-protection cookie mode it ships (`network.cookie.cookieBehavior` 5, @firefox.js:2703), no blocklist, and
/// nothing cleared on exit (`privacy.sanitize.sanitizeOnShutdown` false, @StaticPrefList.yaml:18905). The one field
/// with no Firefox behind it is [`Privacy::error_log`], on because a browser that keeps no record of what broke
/// cannot be told about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Privacy {
    /// Send `DNT: 1` (Firefox's `privacy.donottrackheader.enabled`, off there and off here).
    pub do_not_track: bool,
    /// Which cookies to accept.
    pub cookies: CookiePolicy,
    /// Whether the content blocklist is consulted at all. Off until a list is chosen: there is nothing to apply yet.
    pub content_blocking: bool,
    /// What to forget when the last window closes.
    pub clear_on_exit: ClearOnExit,
    /// Whether LucinaNet writes `errors.jsonl` in the profile directory: what went wrong, on which site, how often,
    /// with nothing about the page but its host. This is the switch `about:errors` points at. Off is quiet everywhere:
    /// no file, no line on the event bus, and nothing kept in memory — `LUCINANET_ERRLOG=0` says the same for one run
    /// and wins over this (the errlog wiring spec).
    pub error_log: bool,
}

/// Which third-party cookies to take. The names are Firefox's `network.cookie.cookieBehavior` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CookiePolicy {
    /// Firefox's 0.
    AllowAll,
    /// Firefox's 5, the mode it ships (@firefox.js:2703): cookies of tracking sites refused, the rest partitioned
    /// per site.
    #[default]
    BlockThirdPartyTrackers,
    /// Firefox's 1: no third-party cookies at all.
    BlockThirdParty,
    /// Firefox's 2: no cookies at all, first-party included.
    BlockAll,
}

/// What the browser forgets when it closes. All off, because Firefox's master switch
/// `privacy.sanitize.sanitizeOnShutdown` is false in a fresh profile: nothing is cleared until asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClearOnExit {
    /// Forget the pages visited.
    pub history: bool,
    /// Forget cookies.
    pub cookies: bool,
    /// Forget the HTTP cache.
    pub cache: bool,
    /// Forget the list of saved files (the files stay where they are).
    pub downloads_list: bool,
}

/// Where saved files go and whether to ask first.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Downloads {
    /// The chosen directory, or `None` to let the wiring resolve one: `LUCINANET_DOWNLOADS`, then
    /// `/data/home/Downloads`, then `$HOME/Downloads` (the downloads spec, step 4).
    pub directory: Option<String>,
    /// Ask where each file goes instead of taking the directory. Firefox's `browser.download.useDownloadDir` true
    /// (@firefox.js:936) means "take the folder, do not ask"; Chromium agrees with `download.prompt_for_download`
    /// false (@download_prefs.cc:356), so the default here is no question.
    pub ask_where: bool,
}

/// How the browser looks and how big its text is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Appearance {
    /// Light, dark, or whatever the system says.
    pub theme: Theme,
    /// The zoom every page starts at; the controls still move from there.
    pub default_zoom_percent: u16,
    /// The smallest text laid out may be, in CSS pixels; 0 lets the page have its way.
    pub minimum_font_size: u8,
    /// When the bookmarks bar is drawn.
    pub show_bookmarks_bar: BookmarksBar,
}

/// The colour scheme pages are given.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Theme {
    /// Follow the system. Firefox's `layout.css.prefers-color-scheme.content-override` 2 is "system color scheme
    /// unless overridden by the theme" (@StaticPrefList.yaml:11534-11539).
    #[default]
    Auto,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

/// When the bookmarks bar is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BookmarksBar {
    /// On every page.
    Always,
    /// Never.
    Never,
    /// Only on the page a new tab opens. Firefox's `browser.toolbars.bookmarks.visibility` "newtab" (@firefox.js:3192).
    #[default]
    NewTabOnly,
}

/// How links and closing behave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tabs {
    /// Open a link that asks for a new window as a tab beside it, instead of replacing the page.
    pub open_links_in_new_tab: bool,
    /// Ask before closing a window that holds several tabs.
    pub confirm_close_multiple: bool,
    /// What a new tab shows.
    pub new_tab_page: NewTabPage,
}

/// What a new tab shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NewTabPage {
    /// The empty page, so a new tab costs one layout and no network.
    #[default]
    Blank,
    /// [`General::home_page`].
    HomePage,
}

/// The zoom the settings file may hold, with [`ZOOM_MAX`]: the ends of Firefox's `toolkit.zoomManager.zoomValues`
/// table, `.2` to `5` (@all.js:3028), which `zoom.minPercent` 20 and `zoom.maxPercent` 500 leave whole
/// (@StaticPrefList.yaml:22022, :22013). These are the ends `lucinanet-zoom` uses, so a stored default is always a
/// level its stepper can reach. `PORTING.md` says why this is 20 and not the spec's 30.
pub(crate) const ZOOM_MIN: i128 = 20;
/// See [`ZOOM_MIN`].
pub(crate) const ZOOM_MAX: i128 = 500;
/// The smallest text that can be forced.
pub(crate) const FONT_MIN: i128 = 0;
/// The largest minimum font size the settings page offers, above which the browser is no longer reading the page.
pub(crate) const FONT_MAX: i128 = 72;
/// A home page longer than this is not a page a person typed; it is a mistake or a pasted document.
pub(crate) const HOME_PAGE_MAX: usize = 2048;
/// Firefox's own limit on how many `Accept-Language` offers a server is worth sent.
pub(crate) const LANGUAGE_MAX: usize = 10;
/// The longest engine id the settings page can show beside its name.
pub(crate) const ENGINE_ID_MAX: usize = 32;

/// LucinaNet's start page. Firefox's is `about:home` (@firefox.js:341); we have no branded start page, and the
/// wiring still lets a command-line argument win over this.
const HOME_PAGE: &str = "about:blank";

impl Default for General {
    fn default() -> Self {
        Self {
            startup: Startup::default(),
            home_page: Self::default_home_page(),
            accept_languages: Self::default_languages(),
        }
    }
}

impl General {
    pub(crate) fn default_home_page() -> String {
        HOME_PAGE.to_string()
    }

    /// Firefox reads `intl.accept_languages` through `Services.locale.acceptLanguages`, whose fallback for an
    /// en-US build is that locale and then its base language: "en-US, en". The pref's own static value is the
    /// placeholder "und" because it depends on the locale (@all.js:1457-1459).
    pub(crate) fn default_languages() -> Vec<String> {
        ["en-US", "en"].iter().map(|tag| (*tag).to_string()).collect()
    }
}

impl Default for Search {
    fn default() -> Self {
        Self {
            default_engine: Engine::BUILTIN_ID.to_string(),
            engines: vec![Engine::builtin()],
            // Off: no request to the engine on a keystroke unless the person turns it on. Firefox ships
            // `browser.search.suggest.enabled` true (@all.js:3441) with `browser.urlbar.suggest.searches` true
            // (@firefox.js:500) behind the same checkbox; LucinaNet differs on purpose (the rule: no request
            // the person did not cause).
            suggest_remote: false,
            // Firefox `browser.urlbar.suggest.history` true (@firefox.js:497).
            suggest_history: true,
            // Firefox `browser.urlbar.suggest.bookmark` true (@firefox.js:495).
            suggest_bookmarks: true,
        }
    }
}

impl Engine {
    /// The id of the engine LucinaNet ships.
    pub(crate) const BUILTIN_ID: &'static str = "duckduckgo";

    /// Where the typed words go in a template; Firefox's OpenSearch reader takes `%s` in the same place, which the
    /// v0 migration turns into this.
    pub(crate) const PLACEHOLDER: &'static str = "{searchTerms}";

    /// The built-in engine: DuckDuckGo's search endpoint, which is the initial `search.default_engine` and the one place
    /// the code names an engine (no browser code names one), and its public completion endpoint for suggestions. No
    /// remote-suggestion request exists in this build, so the completion endpoint is not called.
    pub(crate) fn builtin() -> Engine {
        Engine {
            id: Self::BUILTIN_ID.to_string(),
            name: "DuckDuckGo".to_string(),
            keyword: Some("ddg".to_string()),
            template: "https://duckduckgo.com/html/?q={searchTerms}".to_string(),
            suggest_url: Some("https://duckduckgo.com/ac/?q={searchTerms}&type=list".to_string()),
        }
    }

    /// The built-in engine as a reference, for [`Settings::default_engine`] to hand out when the list holds no
    /// engine at all — which only a value built by hand can be, and that function must not panic.
    fn builtin_ref() -> &'static Engine {
        &BUILTIN
    }
}

static BUILTIN: LazyLock<Engine> = LazyLock::new(Engine::builtin);

impl Default for Appearance {
    fn default() -> Self {
        Self {
            // Firefox `layout.css.prefers-color-scheme.content-override` 2 = auto (@StaticPrefList.yaml:11537).
            theme: Theme::default(),
            // The `1` step of Firefox's `toolkit.zoomManager.zoomValues` (@all.js:3028); Chromium's
            // `kPresetBrowserZoomFactors` holds the same 1.0 (@page_zoom.cc:13-17).
            default_zoom_percent: 100,
            // Firefox `font.minimum-size.*` is 0 for every script: 33 entries from `ar` (@all.js:1899) on, and only
            // `th` is raised above it, to 10 or 13 in three locale blocks (@all.js:2178, :2457, :2709).
            minimum_font_size: 0,
            // Firefox `browser.toolbars.bookmarks.visibility` "newtab" (@firefox.js:3192).
            show_bookmarks_bar: BookmarksBar::default(),
        }
    }
}

impl Default for Tabs {
    fn default() -> Self {
        Self {
            // Firefox `browser.link.open_newwindow` 3 = open in a new tab (@firefox.js:1110).
            open_links_in_new_tab: true,
            // Firefox `browser.tabs.warnOnClose` false (@firefox.js:1153). Chromium warns before closing; we
            // follow Firefox, and `PORTING.md` records the difference from this spec's example file.
            confirm_close_multiple: false,
            // LucinaNet has no new-tab page to show, and an empty tab costs one layout and no network.
            new_tab_page: NewTabPage::default(),
        }
    }
}

impl Default for Privacy {
    fn default() -> Self {
        Self {
            // Firefox `privacy.donottrackheader.enabled` false (@StaticPrefList.yaml:18630).
            do_not_track: false,
            // Firefox `network.cookie.cookieBehavior` 5, the mode it ships (@firefox.js:2703).
            cookies: CookiePolicy::default(),
            // Nothing to apply until a list is chosen, so nothing to apply by default.
            content_blocking: false,
            // Firefox `privacy.sanitize.sanitizeOnShutdown` false (@StaticPrefList.yaml:18905).
            clear_on_exit: ClearOnExit::default(),
            // No Firefox pref: this is LucinaNet's own log. On, because the alternative to a record the person can
            // read is no record at all, and what it keeps is a host, a name and a count (the errlog wiring spec).
            error_log: true,
        }
    }
}

impl Settings {
    /// The engine with this id, or `None` when the list holds no such engine.
    pub fn engine(&self, id: &str) -> Option<&Engine> {
        self.search.engines.iter().find(|engine| engine.id == id)
    }

    /// The engine a search uses. Never panics: a `default_engine` that names nothing falls back to the first
    /// engine, and a list with no engine at all falls back to the built-in one.
    pub fn default_engine(&self) -> &Engine {
        match self.engine(&self.search.default_engine) {
            Some(engine) => engine,
            None => match self.search.engines.first() {
                Some(engine) => engine,
                None => Engine::builtin_ref(),
            },
        }
    }

    /// Put back what the person or the file set outside what the browser can act on, and say what was changed.
    /// The loader runs the same fixing on the way in, so this is what the settings page calls on values it just
    /// set. It is idempotent: run on what it has already fixed, it changes nothing and warns about nothing.
    pub fn validate(&mut self) -> Vec<Warning> {
        let mut warnings = Vec::new();
        self.fix(&mut warnings);
        warnings
    }

    /// The fixing behind [`Settings::validate`], with the warnings going where the caller keeps them.
    ///
    /// `privacy`, `downloads` and `tabs` are not here: their fields are booleans, enums and an optional path, and
    /// a value of one of those is either usable or the loader already replaced it with the default.
    pub(crate) fn fix(&mut self, warnings: &mut Vec<Warning>) {
        self.general.fix(warnings);
        self.search.fix(warnings);
        self.appearance.fix(warnings);
    }

    /// The file: pretty JSON, `"version"` first and the sections in the order the settings page holds them, the
    /// keys this build does not know last, and a newline at the end so two files can be told apart by `diff`.
    /// The same settings always write the same bytes.
    pub fn to_json(&self) -> String {
        let mut fields = vec![
            field("version", count(CURRENT_VERSION as u64)),
            field("general", self.general.node()),
            field("search", self.search.node()),
            field("privacy", self.privacy.node()),
            field("downloads", self.downloads.node()),
            field("appearance", self.appearance.node()),
            field("tabs", self.tabs.node()),
        ];
        for (key, value) in &self.unknown {
            fields.push((key.clone(), node_from_value(value)));
        }
        let mut out = String::new();
        write(&mut out, &Node::Object(fields), 0);
        out.push('\n');
        out
    }
}

impl General {
    pub(crate) fn fix(&mut self, warnings: &mut Vec<Warning>) {
        let trimmed = self.home_page.trim();
        if trimmed.is_empty() {
            // Whitespace only is no page at all: the person gets the start page back rather than a blank field
            // that fails to load.
            warnings.push(Warning::Dropped {
                path: "general.home_page".to_string(),
                reason: "no page given".to_string(),
            });
            self.home_page = Self::default_home_page();
        } else if trimmed.len() > HOME_PAGE_MAX {
            warnings.push(Warning::Dropped {
                path: "general.home_page".to_string(),
                reason: format!("longer than {HOME_PAGE_MAX} bytes"),
            });
            self.home_page = Self::default_home_page();
        } else {
            self.home_page = trimmed.to_string();
        }
        self.accept_languages = fix_languages(&self.accept_languages, warnings);
    }

    pub(crate) fn node(&self) -> Node {
        object(vec![
            field("startup", text(self.startup.name())),
            field("home_page", text(&self.home_page)),
            field(
                "accept_languages",
                Node::Array(self.accept_languages.iter().map(|tag| text(tag)).collect()),
            ),
        ])
    }
}

/// The offer list, cleaned: a bad tag costs its own entry, a repeat of one already kept goes quietly, and the
/// first [`LANGUAGE_MAX`] are what a server is sent. Nothing left over would mean sending no offer at all, so the
/// default list comes back.
fn fix_languages(given: &[String], warnings: &mut Vec<Warning>) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (index, entry) in given.iter().enumerate() {
        let path = format!("general.accept_languages[{index}]");
        let tag = entry.trim();
        if !is_language_tag(tag) {
            warnings.push(Warning::Dropped {
                path,
                reason: "not a language tag".to_string(),
            });
            continue;
        }
        // BCP 47 tags are case-insensitive, so "en-US" after "en-us" is the same offer twice. The first spelling
        // is the one kept and shown.
        let lower = tag.to_ascii_lowercase();
        if seen.contains(&lower) {
            continue;
        }
        seen.push(lower.clone());
        if kept.len() >= LANGUAGE_MAX {
            warnings.push(Warning::Dropped {
                path,
                reason: format!("only the first {LANGUAGE_MAX} languages are offered"),
            });
            continue;
        }
        kept.push(tag.to_string());
    }
    if kept.is_empty() {
        warnings.push(Warning::Dropped {
            path: "general.accept_languages".to_string(),
            reason: "no usable language tag".to_string(),
        });
        return General::default_languages();
    }
    kept
}

/// `^[A-Za-z]{2,3}(-[A-Za-z0-9]{2,8})*$` — the shape of one `Accept-Language` offer, checked by hand because this
/// crate pulls in no regular expressions.
fn is_language_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let primary = match parts.next() {
        Some(primary) => primary,
        None => return false,
    };
    if !(2..=3).contains(&primary.len()) || !primary.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return false;
    }
    // `split('-')` yields an empty piece for a leading, trailing or doubled dash, and the length test refuses it,
    // so "en-", "en--US" and "-en" are not tags.
    parts.all(|part| (2..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_alphanumeric()))
}

impl Search {
    pub(crate) fn fix(&mut self, warnings: &mut Vec<Warning>) {
        let mut kept: Vec<Engine> = Vec::new();
        for (index, engine) in self.engines.iter().enumerate() {
            let engine = engine.normalized();
            match engine.problem(&kept) {
                Some(reason) => warnings.push(Warning::Dropped {
                    path: format!("search.engines[{index}]"),
                    reason: reason.to_string(),
                }),
                None => kept.push(engine),
            }
        }
        if kept.is_empty() {
            warnings.push(Warning::Dropped {
                path: "search.engines".to_string(),
                reason: "no usable engine left".to_string(),
            });
            kept.push(Engine::builtin());
        }
        self.engines = kept;
        if !self.engines.iter().any(|engine| engine.id == self.default_engine) {
            let reason = format!("no engine with id {}", self.default_engine);
            if let Some(first) = self.engines.first() {
                self.default_engine = first.id.clone();
            }
            warnings.push(Warning::Dropped {
                path: "search.default_engine".to_string(),
                reason,
            });
        }
    }

    pub(crate) fn node(&self) -> Node {
        object(vec![
            field("default_engine", text(&self.default_engine)),
            field(
                "engines",
                Node::Array(self.engines.iter().map(Engine::node).collect()),
            ),
            field("suggest_remote", flag(self.suggest_remote)),
            field("suggest_history", flag(self.suggest_history)),
            field("suggest_bookmarks", flag(self.suggest_bookmarks)),
        ])
    }
}

impl Engine {
    /// The trims that make an engine comparable: name and keyword are what the person typed around a space. A
    /// keyword of only spaces means "no keyword" — that is what a settings page sends to clear one, so it must not
    /// cost the engine.
    fn normalized(&self) -> Engine {
        let keyword = self
            .keyword
            .as_deref()
            .map(str::trim)
            .filter(|keyword| !keyword.is_empty())
            .map(str::to_string);
        Engine {
            id: self.id.clone(),
            name: self.name.trim().to_string(),
            keyword,
            template: self.template.clone(),
            suggest_url: self.suggest_url.clone(),
        }
    }

    /// Why this engine cannot be offered, given the ones already kept in front of it. `None` means usable.
    fn problem(&self, kept: &[Engine]) -> Option<&'static str> {
        if !is_engine_id(&self.id) {
            return Some("id must be 1 to 32 of a-z, 0-9, - or _");
        }
        if kept.iter().any(|engine| engine.id == self.id) {
            return Some("another engine already has this id");
        }
        if self.name.is_empty() {
            return Some("no name given");
        }
        if !is_http_url(&self.template) {
            return Some("template is not a http(s) URL");
        }
        if !self.template.contains(Self::PLACEHOLDER) {
            return Some("template has no {searchTerms}");
        }
        if let Some(keyword) = &self.keyword {
            if keyword != &keyword.to_lowercase() {
                return Some("keyword must be lower-case");
            }
            if keyword.chars().any(char::is_whitespace) {
                return Some("keyword must hold no space");
            }
            if kept
                .iter()
                .any(|engine| engine.keyword.as_deref() == Some(keyword.as_str()))
            {
                return Some("another engine already has this keyword");
            }
        }
        if let Some(url) = &self.suggest_url {
            if !is_http_url(url) {
                return Some("suggest_url is not a http(s) URL");
            }
            if !url.contains(Self::PLACEHOLDER) {
                return Some("suggest_url has no {searchTerms}");
            }
        }
        None
    }

    pub(crate) fn node(&self) -> Node {
        object(vec![
            field("id", text(&self.id)),
            field("name", text(&self.name)),
            field("keyword", maybe(&self.keyword)),
            field("template", text(&self.template)),
            field("suggest_url", maybe(&self.suggest_url)),
        ])
    }
}

/// An engine id: lower-case letters, digits, `-` and `_`, at least one and at most [`ENGINE_ID_MAX`].
fn is_engine_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= ENGINE_ID_MAX
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_')
}

/// A URL a search may be sent to: plain HTTP or HTTPS, no other scheme.
pub(crate) fn is_http_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

impl Privacy {
    pub(crate) fn node(&self) -> Node {
        object(vec![
            field("do_not_track", flag(self.do_not_track)),
            field("cookies", text(self.cookies.name())),
            field("content_blocking", flag(self.content_blocking)),
            field("error_log", flag(self.error_log)),
            field("clear_on_exit", self.clear_on_exit.node()),
        ])
    }
}

impl ClearOnExit {
    pub(crate) fn node(&self) -> Node {
        object(vec![
            field("history", flag(self.history)),
            field("cookies", flag(self.cookies)),
            field("cache", flag(self.cache)),
            field("downloads_list", flag(self.downloads_list)),
        ])
    }
}

impl Downloads {
    pub(crate) fn node(&self) -> Node {
        object(vec![
            field("directory", maybe(&self.directory)),
            field("ask_where", flag(self.ask_where)),
        ])
    }
}

impl Appearance {
    pub(crate) fn fix(&mut self, warnings: &mut Vec<Warning>) {
        self.default_zoom_percent = limit_to(
            self.default_zoom_percent as i128,
            "appearance.default_zoom_percent",
            ZOOM_MIN,
            ZOOM_MAX,
            warnings,
        ) as u16;
        self.minimum_font_size = limit_to(
            self.minimum_font_size as i128,
            "appearance.minimum_font_size",
            FONT_MIN,
            FONT_MAX,
            warnings,
        ) as u8;
    }

    pub(crate) fn node(&self) -> Node {
        object(vec![
            field("theme", text(self.theme.name())),
            field("default_zoom_percent", count(self.default_zoom_percent as u64)),
            field("minimum_font_size", count(self.minimum_font_size as u64)),
            field("show_bookmarks_bar", text(self.show_bookmarks_bar.name())),
        ])
    }
}

/// `found` into `min..=max`, the warning naming what was found rather than what it became: a person who set 5000 %
/// is told about 5000, and about the ceiling they hit.
pub(crate) fn limit_to(found: i128, path: &str, min: i128, max: i128, warnings: &mut Vec<Warning>) -> i128 {
    if found < min {
        warnings.push(Warning::Clamped {
            path: path.to_string(),
            from: found.to_string(),
            to: min.to_string(),
        });
        return min;
    }
    if found > max {
        warnings.push(Warning::Clamped {
            path: path.to_string(),
            from: found.to_string(),
            to: max.to_string(),
        });
        return max;
    }
    found
}

impl Tabs {
    pub(crate) fn node(&self) -> Node {
        object(vec![
            field("open_links_in_new_tab", flag(self.open_links_in_new_tab)),
            field("confirm_close_multiple", flag(self.confirm_close_multiple)),
            field("new_tab_page", text(self.new_tab_page.name())),
        ])
    }
}

/// Part of the file, holding its own key order: `serde_json`'s map sorts keys, and the format documents an order
/// that reads as the settings page does, so the tree is built here rather than through a `Value`.
pub(crate) enum Node {
    Object(Vec<(String, Node)>),
    Array(Vec<Node>),
    /// A scalar: a string, a number, a boolean or null.
    Leaf(Value),
}

pub(crate) fn text(value: &str) -> Node {
    Node::Leaf(Value::String(value.to_string()))
}

pub(crate) fn flag(value: bool) -> Node {
    Node::Leaf(Value::Bool(value))
}

pub(crate) fn count(value: u64) -> Node {
    Node::Leaf(Value::from(value))
}

/// `None` is written as `null`, so the key stays in the file and a reader sees the setting exists and was unset.
pub(crate) fn maybe(value: &Option<String>) -> Node {
    Node::Leaf(match value {
        Some(value) => Value::String(value.clone()),
        None => Value::Null,
    })
}

pub(crate) fn field(name: &str, value: Node) -> (String, Node) {
    (name.to_string(), value)
}

pub(crate) fn object(fields: Vec<(String, Node)>) -> Node {
    Node::Object(fields)
}

/// A value this crate kept as it found it. Its keys come out in `serde_json`'s own (sorted) order, which is the
/// same every run, so writing a newer build's settings back is deterministic too.
pub(crate) fn node_from_value(value: &Value) -> Node {
    match value {
        Value::Object(map) => Node::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), node_from_value(value)))
                .collect(),
        ),
        Value::Array(items) => Node::Array(items.iter().map(node_from_value).collect()),
        scalar => Node::Leaf(scalar.clone()),
    }
}

/// Write a node at `depth`: two spaces per level, one key or item per line, `{}` and `[]` when empty.
fn write(out: &mut String, node: &Node, depth: usize) {
    match node {
        Node::Object(fields) => {
            if fields.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (index, (key, value)) in fields.iter().enumerate() {
                spaces(out, depth + 1);
                out.push_str(&Value::String(key.clone()).to_string());
                out.push_str(": ");
                write(out, value, depth + 1);
                if index + 1 < fields.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            spaces(out, depth);
            out.push('}');
        }
        Node::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                spaces(out, depth + 1);
                write(out, item, depth + 1);
                if index + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            spaces(out, depth);
            out.push(']');
        }
        Node::Leaf(scalar) => out.push_str(&scalar.to_string()),
    }
}

fn spaces(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

impl Startup {
    /// The file's names for the values, in the order the format lists them.
    pub(crate) const NAMES: &'static [(&'static str, Startup)] = &[
        ("home_page", Startup::HomePage),
        ("restore_session", Startup::RestoreSession),
        ("blank", Startup::Blank),
    ];

    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::HomePage => "home_page",
            Self::RestoreSession => "restore_session",
            Self::Blank => "blank",
        }
    }
}

impl CookiePolicy {
    pub(crate) const NAMES: &'static [(&'static str, CookiePolicy)] = &[
        ("allow_all", CookiePolicy::AllowAll),
        ("block_third_party_trackers", CookiePolicy::BlockThirdPartyTrackers),
        ("block_third_party", CookiePolicy::BlockThirdParty),
        ("block_all", CookiePolicy::BlockAll),
    ];

    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::AllowAll => "allow_all",
            Self::BlockThirdPartyTrackers => "block_third_party_trackers",
            Self::BlockThirdParty => "block_third_party",
            Self::BlockAll => "block_all",
        }
    }
}

impl Theme {
    pub(crate) const NAMES: &'static [(&'static str, Theme)] = &[
        ("auto", Theme::Auto),
        ("light", Theme::Light),
        ("dark", Theme::Dark),
    ];

    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

impl BookmarksBar {
    pub(crate) const NAMES: &'static [(&'static str, BookmarksBar)] = &[
        ("always", BookmarksBar::Always),
        ("never", BookmarksBar::Never),
        ("new_tab_only", BookmarksBar::NewTabOnly),
    ];

    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Never => "never",
            Self::NewTabOnly => "new_tab_only",
        }
    }
}

impl NewTabPage {
    pub(crate) const NAMES: &'static [(&'static str, NewTabPage)] = &[
        ("blank", NewTabPage::Blank),
        ("home_page", NewTabPage::HomePage),
    ];

    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Blank => "blank",
            Self::HomePage => "home_page",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped defaults, each one where the browser itself says it. `PORTING.md` carries the same table.
    #[test]
    fn default_settings_values() {
        let settings = Settings::default();
        // Firefox `browser.startup.page` 1 = homepage (@firefox.js:340).
        assert_eq!(settings.general.startup, Startup::HomePage);
        // LucinaNet: no branded start page. Firefox's `browser.startup.homepage` is "about:home" (@firefox.js:341).
        assert_eq!(settings.general.home_page, "about:blank");
        // Firefox `intl.accept_languages` for the en-US locale (@all.js:1459 and the comment above it).
        assert_eq!(settings.general.accept_languages, vec!["en-US".to_string(), "en".to_string()]);
        // LucinaNet's one built-in engine, the endpoint browser.rs:1043 already uses.
        assert_eq!(settings.search.default_engine, "duckduckgo");
        assert_eq!(settings.search.engines.len(), 1);
        assert_eq!(settings.search.engines[0].id, "duckduckgo");
        assert_eq!(settings.search.engines[0].name, "DuckDuckGo");
        assert_eq!(settings.search.engines[0].keyword.as_deref(), Some("ddg"));
        assert_eq!(
            settings.search.engines[0].template,
            "https://duckduckgo.com/html/?q={searchTerms}"
        );
        assert_eq!(
            settings.search.engines[0].suggest_url.as_deref(),
            Some("https://duckduckgo.com/ac/?q={searchTerms}&type=list")
        );
        // Off by default: no per-keystroke request to the engine unless the person turns it on.
        assert!(!settings.search.suggest_remote);
        // Firefox `browser.urlbar.suggest.history` true (@firefox.js:497).
        assert!(settings.search.suggest_history);
        // Firefox `browser.urlbar.suggest.bookmark` true (@firefox.js:495).
        assert!(settings.search.suggest_bookmarks);
        // Firefox `privacy.donottrackheader.enabled` false (@StaticPrefList.yaml:18630).
        assert!(!settings.privacy.do_not_track);
        // Firefox `network.cookie.cookieBehavior` 5 (@firefox.js:2703).
        assert_eq!(settings.privacy.cookies, CookiePolicy::BlockThirdPartyTrackers);
        assert!(!settings.privacy.content_blocking);
        // Firefox `privacy.sanitize.sanitizeOnShutdown` false (@StaticPrefList.yaml:18905).
        assert_eq!(settings.privacy.clear_on_exit, ClearOnExit::default());
        assert_eq!(settings.downloads.directory, None);
        // Firefox `browser.download.useDownloadDir` true (@firefox.js:936): the folder, no question.
        assert!(!settings.downloads.ask_where);
        // Firefox content-override 2 = auto (@StaticPrefList.yaml:11539).
        assert_eq!(settings.appearance.theme, Theme::Auto);
        assert_eq!(settings.appearance.default_zoom_percent, 100);
        // Firefox `font.minimum-size.*` 0 (@all.js:1899).
        assert_eq!(settings.appearance.minimum_font_size, 0);
        // Firefox `browser.toolbars.bookmarks.visibility` "newtab" (@firefox.js:3192).
        assert_eq!(settings.appearance.show_bookmarks_bar, BookmarksBar::NewTabOnly);
        // Firefox `browser.link.open_newwindow` 3 (@firefox.js:1110).
        assert!(settings.tabs.open_links_in_new_tab);
        // Firefox `browser.tabs.warnOnClose` false (@firefox.js:1153).
        assert!(!settings.tabs.confirm_close_multiple);
        assert_eq!(settings.tabs.new_tab_page, NewTabPage::Blank);
        assert!(settings.unknown.is_empty());
        // The defaults are already inside every rule, so nothing has to be fixed on them.
        assert_eq!(Settings::default().validate(), Vec::new());
    }

    /// What the default settings write, read back through `serde_json` and checked against the file format the
    /// spec documents: every key present, every enum the snake_case name.
    #[test]
    fn default_json_matches_the_documented_shape() {
        let value: Value = serde_json::from_str(&Settings::default().to_json())
            .expect("the file we just wrote is JSON");
        assert_eq!(value["version"], Value::from(1u64));
        assert_eq!(value["general"]["startup"], Value::String("home_page".to_string()));
        assert_eq!(value["general"]["home_page"], Value::String("about:blank".to_string()));
        assert_eq!(
            value["general"]["accept_languages"],
            Value::Array(vec![
                Value::String("en-US".to_string()),
                Value::String("en".to_string())
            ])
        );
        assert_eq!(value["search"]["default_engine"], Value::String("duckduckgo".to_string()));
        assert_eq!(value["search"]["engines"].as_array().map(Vec::len), Some(1));
        assert_eq!(value["search"]["engines"][0]["id"], Value::String("duckduckgo".to_string()));
        assert_eq!(value["search"]["suggest_remote"], Value::Bool(false));
        assert_eq!(value["search"]["suggest_history"], Value::Bool(true));
        assert_eq!(value["search"]["suggest_bookmarks"], Value::Bool(true));
        assert_eq!(value["privacy"]["do_not_track"], Value::Bool(false));
        assert_eq!(
            value["privacy"]["cookies"],
            Value::String("block_third_party_trackers".to_string())
        );
        assert_eq!(value["privacy"]["content_blocking"], Value::Bool(false));
        assert_eq!(value["privacy"]["clear_on_exit"]["history"], Value::Bool(false));
        assert_eq!(value["privacy"]["clear_on_exit"]["downloads_list"], Value::Bool(false));
        assert_eq!(value["downloads"]["directory"], Value::Null);
        assert_eq!(value["downloads"]["ask_where"], Value::Bool(false));
        assert_eq!(value["appearance"]["theme"], Value::String("auto".to_string()));
        assert_eq!(value["appearance"]["default_zoom_percent"], Value::from(100u64));
        assert_eq!(value["appearance"]["minimum_font_size"], Value::from(0u64));
        assert_eq!(
            value["appearance"]["show_bookmarks_bar"],
            Value::String("new_tab_only".to_string())
        );
        assert_eq!(value["tabs"]["open_links_in_new_tab"], Value::Bool(true));
        assert_eq!(value["tabs"]["new_tab_page"], Value::String("blank".to_string()));
    }

    /// A default file read back is the default settings again, with nothing to report about it.
    #[test]
    fn default_json_roundtrip() {
        let written = Settings::default().to_json();
        let (read, warnings) = Settings::from_json(&written).expect("the file we wrote reads");
        assert_eq!(read, Settings::default());
        assert_eq!(warnings, Vec::new());
    }

    /// The bytes depend on the settings and on nothing else, and the file ends in one newline.
    #[test]
    fn to_json_is_deterministic_and_ends_with_newline() {
        let mut settings = Settings::default();
        settings.privacy.cookies = CookiePolicy::BlockThirdParty;
        settings.unknown.insert("sync_device".to_string(), Value::from("juno-1"));
        settings.unknown.insert(
            "newer_section".to_string(),
            serde_json::json!({ "zeta": 1, "alpha": [1, 2] }),
        );
        let first = settings.to_json();
        let second = settings.to_json();
        assert_eq!(first, second);
        assert!(first.ends_with("}\n"), "the file must end in a newline");
        assert!(!first.ends_with("}\n\n"), "and in exactly one");
        assert!(!first.contains('\r'), "no platform-dependent line ends");
    }

    /// The order the format documents: version first, then the sections as the settings page holds them, then what
    /// we do not know. It is not the alphabetical order a `serde_json::Map` would have written.
    #[test]
    fn to_json_key_order_is_fixed() {
        let mut settings = Settings::default();
        settings.unknown.insert("zz_first_alphabetically".to_string(), Value::from(1u64));
        let written = settings.to_json();
        let order = [
            "\"version\"",
            "\"general\"",
            "\"search\"",
            "\"privacy\"",
            "\"downloads\"",
            "\"appearance\"",
            "\"tabs\"",
            "\"zz_first_alphabetically\"",
        ];
        let mut previous = 0;
        for key in order {
            let found = written.find(key).unwrap_or_else(|| panic!("{key} missing from the file"));
            assert!(found > previous, "{key} is not after the key before it");
            previous = found;
        }
        // Inside the engine the same holds: id comes first although "keyword" would sort ahead of it.
        let engines = &written[written.find("\"engines\"").expect("engines")..];
        let engine = &engines[..engines.find(']').expect("the engine list's end")];
        assert!(engine.find("\"id\"").expect("id") < engine.find("\"name\"").expect("name"));
        assert!(engine.find("\"name\"").expect("name") < engine.find("\"keyword\"").expect("keyword"));
    }

    /// The fixing a settings page needs after it set values by hand, with the person told each time.
    #[test]
    fn validate_fixes_ui_values() {
        let mut settings = Settings::default();
        settings.appearance.default_zoom_percent = 900;
        settings.appearance.minimum_font_size = 200;
        settings.general.home_page = "   ".to_string();
        settings.general.accept_languages = vec!["not a tag!".to_string()];
        settings.search.engines.push(Engine {
            id: "Bad Id".to_string(),
            name: "Broken".to_string(),
            keyword: None,
            template: "https://broken.example/?q={searchTerms}".to_string(),
            suggest_url: None,
        });
        settings.search.default_engine = "gone".to_string();
        let warnings = settings.validate();
        // Hand-worked: the two ranges, the empty page, the one bad tag and the one bad id.
        assert_eq!(settings.appearance.default_zoom_percent, 500);
        assert_eq!(settings.appearance.minimum_font_size, 72);
        assert_eq!(settings.general.home_page, "about:blank");
        assert_eq!(
            settings.general.accept_languages,
            vec!["en-US".to_string(), "en".to_string()]
        );
        assert_eq!(settings.search.engines.len(), 1);
        assert_eq!(settings.search.engines[0].id, "duckduckgo");
        assert_eq!(settings.search.default_engine, "duckduckgo");
        assert_eq!(warnings.len(), 7, "one per value that moved: {warnings:?}");
        // Hand-worked, in the order `fix` walks the sections: home page, the two language entries, the broken id,
        // the engine name that no longer exists, then the two ranges.
        assert_eq!(
            warnings,
            vec![
                Warning::Dropped {
                    path: "general.home_page".to_string(),
                    reason: "no page given".to_string()
                },
                Warning::Dropped {
                    path: "general.accept_languages[0]".to_string(),
                    reason: "not a language tag".to_string()
                },
                Warning::Dropped {
                    path: "general.accept_languages".to_string(),
                    reason: "no usable language tag".to_string()
                },
                Warning::Dropped {
                    path: "search.engines[1]".to_string(),
                    reason: "id must be 1 to 32 of a-z, 0-9, - or _".to_string()
                },
                Warning::Dropped {
                    path: "search.default_engine".to_string(),
                    reason: "no engine with id gone".to_string()
                },
                Warning::Clamped {
                    path: "appearance.default_zoom_percent".to_string(),
                    from: "900".to_string(),
                    to: "500".to_string()
                },
                Warning::Clamped {
                    path: "appearance.minimum_font_size".to_string(),
                    from: "200".to_string(),
                    to: "72".to_string()
                },
            ]
        );
        // Fixed once is fixed for good.
        assert_eq!(settings.validate(), Vec::new());
    }

    /// The engine a search uses, for every shape of the list — including the empty one a value built by hand can
    /// have, which must still give an engine and not a panic.
    #[test]
    fn default_engine_never_panics() {
        let settings = Settings::default();
        assert_eq!(settings.default_engine().id, "duckduckgo");
        assert_eq!(settings.engine("ddg-missing"), None);
        let mut second = settings.clone();
        second.search.engines.push(Engine {
            id: "wikipedia".to_string(),
            name: "Wikipedia".to_string(),
            keyword: Some("wiki".to_string()),
            template: "https://wikipedia.org/w/index.php?search={searchTerms}".to_string(),
            suggest_url: None,
        });
        second.search.default_engine = "wikipedia".to_string();
        assert_eq!(second.default_engine().name, "Wikipedia");
        // A default_engine that names nothing: the first engine takes its place.
        let mut third = second.clone();
        third.search.default_engine = "no-such-engine".to_string();
        assert_eq!(third.default_engine().id, "duckduckgo");
        // No engines at all, and even no default_engine: still an engine out of this.
        let mut empty = Settings::default();
        empty.search.engines.clear();
        empty.search.default_engine = String::new();
        assert_eq!(empty.default_engine().id, "duckduckgo");
        assert_eq!(empty.default_engine().template, Engine::builtin().template);
    }
}
