/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The address bar's decisions as pure functions: what Enter does with typed text ([`classify`]), which
//! suggestions appear under the bar and in what order ([`suggest`]), the history score they use
//! ([`history_score`]), and what the field shows when it is not being edited and where it is scrolled when it is
//! ([`display`]). No network, no storage, no clock: history, bookmarks, remote search suggestions, the
//! registrable domain and the current time arrive as arguments, so the crate builds and tests without the rest
//! of LucinaNet.
//! See `PORTING.md` for the originals each rule comes from.

#![forbid(unsafe_code)]

pub mod classify;
pub mod display;
pub mod fixup;
pub mod history_score;
pub mod rank;

pub use classify::{Action, Context, SearchEngine, classify, to_url};
pub use fixup::fix_scheme_typos;
pub use history_score::{history_score, history_score_among};
pub use rank::{
    BookmarkEntry, HistoryEntry, Kind, MAX_SUGGESTIONS, RankInput, Suggestion, suggest,
};
