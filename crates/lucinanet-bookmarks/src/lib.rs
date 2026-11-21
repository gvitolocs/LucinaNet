/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Bookmark store model: the in-memory tree with its rules (ordering, moves, dedupe, search), its own JSON
//! file format, and importers/exporters for Netscape bookmark HTML and Firefox's places JSON backup.
//!
//! Pure and deterministic: no file access, no clock (times arrive as microseconds since the Unix epoch),
//! no favicons. Text that comes from outside never panics; it returns an error or is counted as skipped.

#![forbid(unsafe_code)]

mod format;
mod netscape;
mod places_json;
mod store;

#[cfg(test)]
mod tests;

pub use store::Bookmarks;

/// Node identifier. Ids are never reused inside one store.
pub type Id = u64;
/// Invisible root, never shown, cannot be removed or moved. Its only children are the three built-in folders.
pub const ROOT: Id = 0;
/// "Bookmarks Toolbar".
pub const TOOLBAR: Id = 1;
/// "Bookmarks Menu" (Firefox; Chromium has none, Chromium "Other bookmarks" imports into [`OTHER`]).
pub const MENU: Id = 2;
/// "Other Bookmarks" (Firefox "unfiled").
pub const OTHER: Id = 3;

/// What a node is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A link. `url` is always in `url::Url::as_str` form.
    Bookmark { url: String },
    /// A container; the only kind that can have children.
    Folder,
    /// A divider line; never has children and has an empty title.
    Separator,
}

/// One node of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub id: Id,
    /// `None` only for [`ROOT`].
    pub parent: Option<Id>,
    pub kind: Kind,
    /// Empty for separators; a bookmark with an empty title shows its URL.
    pub title: String,
    pub added_us: i64,
    pub modified_us: i64,
    /// Lower-case, sorted, no duplicates (Firefox tags). Only bookmarks have tags.
    pub tags: Vec<String>,
    /// Firefox bookmark keyword / Netscape SHORTCUTURL. Only bookmarks have one.
    pub keyword: Option<String>,
}

/// Why an operation was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    NoSuchNode(Id),
    /// The target of an insert or move is not a folder (separators and bookmarks cannot have children).
    NotAFolder(Id),
    /// [`ROOT`] and the three built-in folders cannot be removed, renamed or moved, and nothing but them lives in [`ROOT`].
    ProtectedNode(Id),
    /// A folder cannot move into itself or its own subtree.
    Cycle,
    EmptyUrl,
    InvalidUrl(String),
    IndexOutOfRange,
    /// Malformed file, or an operation on a node of the wrong kind (for example tags on a folder).
    Format(String),
}

/// Options shared by both importers.
#[derive(Clone, Copy, Debug, Default)]
pub struct ImportOptions {
    /// Skip a bookmark whose URL already exists in the same destination folder.
    pub skip_duplicates: bool,
}

/// What an import did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub bookmarks: usize,
    pub folders: usize,
    pub separators: usize,
    pub skipped_duplicates: usize,
    pub skipped_invalid: usize,
}
