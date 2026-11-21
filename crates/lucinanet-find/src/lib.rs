/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! LucinaNet's find in page: which text a query matches, which of those matches is current, and what the
//! counter says. Ctrl+F logic only -- no DOM, no Servo, no widgets.
//!
//! Servo has no find API yet, so the page arrives as text the engine side already extracted: [`Block`]s of
//! [`Run`]s, one run per text node. [`find_all`] turns those plus a query into [`Match`] ranges, which are
//! `(block, Pos)` pairs the engine can draw highlights from and hand back to a DOM Range; [`Finder`] keeps the
//! list, moves the current match with [`Finder::next`] and [`Finder::previous`], survives a page that changes
//! under an open bar with [`Finder::refresh`], and produces the bar's text with [`counter_text`].
//!
//! The behaviour is Firefox's, at the pinned revision: `nsFind.cpp` for what matches (case and diacritic
//! folding, whole words, whitespace runs, matches across inline elements), `Finder.sys.mjs` and
//! `FinderIterator.sys.mjs` for counting and the limit, `findbar.js` and `findbar.ftl` for the wrap messages
//! and the counter. Chromium's `text_finder.cc`/`find_buffer.cc` settle the questions Firefox leaves open --
//! matches are non-overlapping, a block ends a match -- and contribute only its `3/12` counter. Every source
//! read, what came from it, and each place this crate follows the spec instead of the browser is recorded in
//! `PORTING.md`; the same file notes why there is no `NOTICE`.
//!
//! ```
//! use lucinanet_find::{Block, CounterStyle, Finder, Options, Run, Wrapped};
//!
//! // A page with one paragraph; the engine side hands the text over, this crate only matches it.
//! let blocks = [Block { runs: vec![Run { node: 1, text: "Résumé — resume again".into() }] }];
//! let mut finder = Finder::new();
//! let state = finder.search(&blocks, "resume", Options::default(), None);
//! assert_eq!(state.total, 2); // accents folded away, both spellings found
//! assert_eq!(finder.counter_text(CounterStyle::Firefox), "1 of 2 matches");
//!
//! assert_eq!(finder.next().current, Some(1));
//! assert_eq!(finder.counter_text(CounterStyle::Chromium), "2/2");
//! // `next` past the last match comes back to the top and says so.
//! assert_eq!(finder.next().wrapped, Some(Wrapped::ToTop));
//! ```

#![forbid(unsafe_code)]

mod finder;
mod fold;
mod search;

pub use finder::CounterStyle;
pub use finder::Finder;
pub use finder::MATCH_LIMIT;
pub use finder::State;
pub use finder::Wrapped;
pub use finder::counter_text;
pub use search::Block;
pub use search::Found;
pub use search::Match;
pub use search::Options;
pub use search::Pos;
pub use search::Run;
pub use search::find_all;
