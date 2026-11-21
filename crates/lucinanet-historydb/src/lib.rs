/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Visited-page store modelled on Firefox places (`moz_places` / `moz_historyvisits`), with Firefox's frecency and the
//! prefix/substring queries the history list and address bar need. Pure and deterministic: times arrive as arguments and
//! persistence is `to_json`/`from_json`; file I/O belongs to the wiring.

#![forbid(unsafe_code)]

mod db;
mod frecency;
mod query;
mod redact;

pub use db::{
    HistoryDb, HistoryError, MAX_VISITS_PER_PAGE, NewVisit, Page, PageId, Source, Transition, Visit, VisitId,
};
pub use frecency::{FrecencyParams, frecency, url_frecency};
pub use query::{MatchBehavior, Query};
pub use redact::{SECRET_PARAMS, redact_url};

#[cfg(test)]
mod tests;
