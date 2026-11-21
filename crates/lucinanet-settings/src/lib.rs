/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! LucinaNet's settings: the typed schema the settings page and the rest of the browser read, the defaults we
//! ship, the file format with its version, a loader that never loses the person's other settings because one value
//! is bad, and the chain that brings an old file up to date.
//!
//! Before this there were only environment variables (`LUCINANET_PREFS`, `LUCINANET_PROFILE_DIR`,
//! `LUCINANET_DOWNLOADS`, `crates/lucinanet-servo/src/engine.rs:697` and `:1176`). Nothing here reads a file, the
//! environment or the clock: [`Settings::to_json`] and [`Settings::from_json`] move between a value and a string,
//! and the wiring owns `prefs.json` in the profile directory. Zoom percent, search engines and permission defaults
//! are plain fields here for the same reason — the crates that act on them are wired to these values later.
//!
//! - [`schema`] the structs, the defaults (taken from Firefox's own prefs, table in `PORTING.md`) and the clamping.
//! - [`load`] the lenient reader: a bad value costs that value, never the file.
//! - [`migrate`] the version chain; version 0 is the flat file the early builds could have written.
//! - [`diff`] what changed between two settings, as the dotted names the wiring applies to a live page.
//!
//! The one rule that outlives every other: a file from a newer LucinaNet is [`SettingsError::TooNew`], and the
//! caller then keeps the defaults in memory and leaves the file alone. Reading forwards is lossless; writing
//! backwards is what would destroy settings this build has no field for.

#![forbid(unsafe_code)]

mod diff;
mod load;
mod migrate;
mod schema;

pub use migrate::migrate;
pub use schema::{
    Appearance, BookmarksBar, ClearOnExit, CookiePolicy, Downloads, Engine, General, NewTabPage, Privacy, Search,
    Settings, Startup, Tabs, Theme,
};

/// The version [`Settings::to_json`] writes, and the newest version this crate can read. A higher version in a file
/// is [`SettingsError::TooNew`], so a build that knows fewer settings never rewrites one that knows more.
pub const CURRENT_VERSION: u32 = 1;

/// One thing the loader or [`Settings::validate`] had to change. The settings page shows these instead of quietly
/// disagreeing with the file: a person who finds a checkbox back where they left it must not be told nothing happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Warning {
    /// `path` is a dotted setting name ("appearance.theme"). Its value was unusable, so the default went in.
    Dropped {
        path: String,
        reason: String,
    },
    /// `path` was outside the range the browser can act on; `from` is what was set, `to` what it became.
    Clamped {
        path: String,
        from: String,
        to: String,
    },
    /// The file was at version `from` and one step of the chain brought it to `to`.
    Migrated {
        from: u32,
        to: u32,
    },
    /// A top-level key this crate has no field for (written by a newer LucinaNet, or by hand). Kept in
    /// [`Settings::unknown`] and written back, so saving does not erase it.
    UnknownKeyKept(String),
}

/// Why a file could not be read at all. Everything short of this is a [`Warning`]: one bad value costs that value,
/// never the file.
#[derive(Debug, PartialEq, Eq)]
pub enum SettingsError {
    /// Not JSON. The string is `serde_json`'s own message, for the wiring to show as is.
    Syntax(String),
    /// JSON that is well formed but not an object: a list, a number, a bare string or `null`.
    NotAnObject,
    /// A `version` above [`CURRENT_VERSION`]. The caller must keep the defaults in memory and not write over the file.
    TooNew {
        found: u32,
        supported: u32,
    },
}

/// A top-level JSON object, the shape every version of the file has.
pub(crate) type Object = serde_json::Map<String, serde_json::Value>;
