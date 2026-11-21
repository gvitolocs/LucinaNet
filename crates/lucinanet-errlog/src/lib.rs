/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! What a person's browsing hit, in their own machine's words and none of their pages: the session
//! error log behind `about:errors`.
//!
//! LucinaNet records nothing about the errors its pages run into. When a site breaks, the evidence is
//! a console nobody opens, and it is gone the moment the tab closes. This crate is the library under a
//! later wiring: the browser writes one line per error into the profile directory as it browses, and
//! an internal page lists the last few sessions' errors grouped by host, so that "this site has been
//! broken for a week" is answerable from the machine itself without opening a developer tool.
//!
//! Privacy is the design and not a setting. There is no field for page content, cookies, query
//! strings, fragments, form data or user info, and the three functions that fill the fields there are
//! ([`host_of`], [`file_of`], [`redact_detail`]) are what take the text a page controls down to a
//! host, a name and a number. `errors.jsonl` says that `example.com`'s `app.js` threw the same
//! `TypeError` four hundred times — no more than the address bar and a status line already showed.
//!
//! - [`model`] the [`Event`], the [`Kind`]s, and the line format.
//! - [`redact`] the URL readers and the message redactor, which decide what may be kept at all.
//! - [`log`] the writer: append-only, capped, counted down to powers of two, and never a panic; and
//!   [`Target`], what one `Clear` control takes out of it.
//! - [`read`] the reader: sessions newest first, corrupt lines skipped and counted, per-host groups.
//! - [`page`] the HTML and the text rendering of what the reader returns, with the controls that
//!   empty it and the page that asks before it does.
//!
//! Pure and deterministic throughout: the clock, the session number, the tab and the directory arrive
//! as arguments; nothing here reads an environment variable, a global, or a thread's name. The wiring
//! that owns `profile_dir()` and the `window.onerror` hook lives in
//! `crates/lucinanet-servo/src/engine.rs` and is deliberately not part of this crate — this is the
//! half that can be tested without a browser.
//!
//! ```
//! use lucinanet_errlog::{Event, Log, render_text};
//!
//! let mut log = Log::memory(1);
//! log.record(Event::script(1_731_400_000_000, 1, 1, "https://example.com/", "https://cdn.example/app.js?token=secret", 42, "TypeError: x is not a function"));
//! log.record(Event::script(1_731_400_001_000, 1, 1, "https://example.com/", "https://cdn.example/app.js?token=secret", 42, "TypeError: x is not a function"));
//! // The second one was the same error: it is counted, and its line goes out at two.
//! assert_eq!(log.events().len(), 2);
//! assert_eq!(log.events()[1].count, 2);
//! assert!(log.events()[0].to_line().contains("\"file\":\"app.js\""));
//! assert!(!log.events()[0].to_line().contains("secret"));
//! ```

#![forbid(unsafe_code)]

mod log;
mod model;
mod page;
mod read;
mod redact;

#[cfg(test)]
mod tests;

pub use log::{GENERATIONS, Log, FILE_NAME, MAX_BYTES, MAX_LINES, Target, clear_files};
pub use model::{DETAIL_MAX_CHARS, Event, FILE_MAX_CHARS, Kind};
pub use page::{ClearLink, Lines, Page, render_clear_ask, render_html, render_html_with, render_text, stamp, target_words};
pub use read::{DetailCount, HostGroup, SessionErrors, TOP_DETAILS, group_by_host, read_sessions};
pub use redact::{file_of, host_of, redact_detail};
