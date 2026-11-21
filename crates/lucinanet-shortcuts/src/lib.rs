/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! What the browser itself answers to when a key is pressed, as data (docs/specs/lucinanet-shortcuts.md).
//! [`table`] holds the defaults of each engine on Linux, [`matcher`] turns a physical key press plus the
//! modifiers held into a [`Command`], and nothing here touches an engine, a window or a clock: the wiring in
//! `browser.rs` and `desktop.rs` asks [`lookup`] for every key press and runs the command it gets back, or
//! hands the press to the page when it gets `None`.

#![forbid(unsafe_code)]

pub mod code;
mod matcher;
mod table;

pub use matcher::{binding_for, label, lookup};
pub use table::bindings;

/// The modifiers held with a key, exactly as the key event says. Caps and Num lock are not here: they change
/// what a key means, not which shortcut it is, and no browser shortcut names them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// The Super/Windows key. Linux leaves it to the desktop, so no binding has it and [`lookup`] never
    /// matches with it held.
    pub meta: bool,
}

/// What a shortcut does to the browser. Page keys (arrows, Space, Ctrl+Home, PageUp/PageDown without Ctrl) are
/// not commands: neither browser takes them from the page, so they are not here and not bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    NewTab,
    CloseTab,
    ReopenClosedTab,
    NextTab,
    PreviousTab,
    /// The `n`th tab in the strip, 1..=8, the most `lucinanet-ui`'s `tabs::MAX_TABS` keeps.
    SelectTab(u8),
    /// The last tab, what Ctrl/Alt+9 selects in both browsers.
    SelectLastTab,
    NewWindow,
    NewPrivateWindow,
    CloseWindow,
    Quit,
    FocusAddressBar,
    FocusSearch,
    Find,
    FindNext,
    FindPrevious,
    BookmarkPage,
    ShowBookmarks,
    ShowHistory,
    ShowDownloads,
    ClearBrowsingData,
    Reload,
    ReloadBypassCache,
    Stop,
    Back,
    Forward,
    Home,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Fullscreen,
    Print,
    SavePage,
    ViewSource,
    DevTools,
}

/// Which browser's defaults to follow. The tables are the two browsers' Linux defaults; Juno has no Windows or
/// macOS builds to table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Firefox,
    Chromium,
}

/// One shortcut: the physical key, the modifiers that must be held with it, and what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub code: u16,
    pub mods: Mods,
    pub command: Command,
}

/// What the browser is doing at the key press, as the wiring sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// A load is in flight, so Esc stops it.
    pub loading: bool,
    /// The find bar is open. Esc is not a find-bar entry in either table, so this flag changes nothing in
    /// [`lookup`]: Firefox's Esc row is `key_stop` → `Browser:Stop` and nothing else (`browser-sets.inc.xhtml`;
    /// the find bar's own closing lives in its widget code, not in a `<key>`), and Chromium's is
    /// `IDC_CLOSE_FIND_OR_STOP`, which runs `CanCloseFind` → `CloseFind` before it considers stopping
    /// (`browser_command_controller.cc:1128`). Either way the *wiring* gives the key to the open find bar
    /// before it asks this matcher.
    pub find_bar_open: bool,
    /// A text field has the caret, in the page or in the address bar. What the sources filter on this is
    /// nothing: the editing keys (Ctrl+C, Ctrl+V, Backspace, bare letters) are not browser commands and so are
    /// not in the tables at all; see PORTING.md. The wiring uses the flag for what happens after the command.
    pub text_field_focused: bool,
}
