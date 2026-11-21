/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The state of an open find: which match is current, what Next and Previous do at the ends of the list, and
//! what the bar says about it.
//!
//! [`Finder`] holds the matches [`crate::search`] found for one query and walks them. The walking is the part
//! with browser behaviour in it: whether a step that wraps is *also* a move, what happens when the page changes
//! under an open bar, and which English words the counter shows. Firefox is the source for all three, with
//! Chromium's counter kept alongside because the two read differently (Firefox *3 of 12 matches*, Chromium
//! *3/12*).
//!
//! Nothing here touches the page beyond the blocks it is given, and nothing decides where a highlight is drawn:
//! [`Match`] ranges go out, the engine side draws them.

use crate::search::{Block, Match, Options, Pos, find_all};

/// The highest match count Firefox's find bar will report, and the number the "More than" wording uses.
/// Firefox `accessibility.typeaheadfind.matchesCountLimit`, default 1000 (`modules/libpref/init/all.js`:491),
/// read by `Finder.sys.mjs`:187-194 and passed to the iterator as its limit (`:646`).
pub const MATCH_LIMIT: usize = 1000;

/// A step that ran off the end of the page and continued at the other one, which is what the findbar puts on
/// screen instead of saying nothing (`findbar.js`:1077-1083).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrapped {
    /// Next ran past the last match and went on from the top of the page.
    ToTop,
    /// Previous ran past the first match and went on from the bottom of the page.
    ToBottom,
}

/// Which browser's counter wording to produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CounterStyle {
    /// Firefox's findbar, from `findbar.ftl`.
    Firefox,
    /// Chromium's find bubble, `*ordinal*/*total*`.
    Chromium,
}

/// What the bar knows about one search: which match is selected out of how many, and whether the last step
/// wrapped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    /// Index into the match list, so `None` with `total > 0` means nothing is selected yet. The counter shows
    /// this one-based, as both browsers do.
    pub current: Option<usize>,
    /// Matches found, which is `MATCH_LIMIT` when the count was stopped ([`State::truncated`]).
    pub total: usize,
    /// The count stopped at [`MATCH_LIMIT`], so there may be more matches than `total`.
    pub truncated: bool,
    /// Set by the step that wrapped, cleared by the next step.
    pub wrapped: Option<Wrapped>,
}

/// The `findbar-not-found` string (`toolkit/locales/en-US/toolkit/main-window/findbar.ftl`:38 @33abb90).
const NOT_FOUND: &str = "Phrase not found";
/// `findbar-wrapped-to-top` (`findbar.ftl`:40).
const WRAPPED_TO_TOP: &str = "Reached end of page, continued from top";
/// `findbar-wrapped-to-bottom` (`findbar.ftl`:41).
const WRAPPED_TO_BOTTOM: &str = "Reached top of page, continued from bottom";

/// Find in page state: the matches of one query and the current one among them.
///
/// The finder keeps the query and options it was last given so [`Finder::refresh`] can re-run the same search
/// when the page changes underneath the bar.
#[derive(Clone, Debug, Default)]
pub struct Finder {
    matches: Vec<Match>,
    query: String,
    options: Options,
    current: Option<usize>,
    truncated: bool,
    wrapped: Option<Wrapped>,
}

impl Finder {
    /// Nothing found, nothing selected.
    pub fn new() -> Finder {
        Finder::default()
    }

    /// A new search. The current match is the first one at or after `from` (a block index and a position in it;
    /// `None` = top of the page); if there is none after `from` it is the first match and `wrapped` is
    /// `ToTop`. Empty `query` clears everything.
    ///
    /// `from` is how incremental typing is kept from jumping: pass the start of the match that is current, and
    /// a query that grows one letter while that match still starts where it did stays put (rule 8). Firefox does
    /// the same thing from the other side -- the engine searches from the existing selection
    /// (`Finder.sys.mjs`:211-216, `fastFind` always uses `FIND_INITIAL`) -- but it does not restart the count,
    /// so a retype can move the selection; here the caller says where it was.
    pub fn search(&mut self, blocks: &[Block], query: &str, options: Options, from: Option<(usize, Pos)>) -> State {
        self.query = query.to_string();
        self.options = options;
        if query.trim().is_empty() {
            self.clear();
            return self.state();
        }
        let found = find_all(blocks, query, options, MATCH_LIMIT);
        self.matches = found.matches;
        self.truncated = found.truncated;
        self.wrapped = None;
        self.current = self.pick_from(from);
        self.state()
    }

    /// One match further down the page; past the last one, from the top, reporting it.
    // The name is the find bar's *Find Next*, not an iterator step: a `Finder` also moves backwards,
    // wraps at both ends and carries the counter, so `Iterator` would be the wrong contract.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> State {
        self.step(true)
    }

    /// One match further up the page; before the first one, from the bottom, reporting it.
    pub fn previous(&mut self) -> State {
        self.step(false)
    }

    /// The current state, without changing it.
    pub fn state(&self) -> State {
        State {
            current: self.current,
            total: self.matches.len(),
            truncated: self.truncated,
            wrapped: self.wrapped,
        }
    }

    /// The range to select and scroll to, or `None` when nothing is current.
    pub fn current_match(&self) -> Option<Match> {
        self.matches.get(self.current?).copied()
    }

    /// Every match, for "highlight all" (`findbar.js`'s Highlight All checkbox, `findbar.ftl`:15-21).
    pub fn matches(&self) -> &[Match] {
        &self.matches
    }

    /// The page changed under an open find bar: search again and keep the current match as close to where it
    /// was as possible.
    ///
    /// The new current match is the first one starting at or after the old current match's `(block, start)`, and
    /// if none does, the last match; with no matches left at all nothing is current. `wrapped` is cleared: this
    /// is not a step. Firefox has no equivalent -- it re-runs the search and lets the engine's selection fall
    /// where it falls (`Finder.sys.mjs`:254-263, `findAgain` searches on from the current selection) -- so rule
    /// 9 is ours, see PORTING.md.
    pub fn refresh(&mut self, blocks: &[Block]) -> State {
        let from = self.current_match().map(|m| (m.block, m.start));
        if self.query.trim().is_empty() {
            self.clear();
            return self.state();
        }
        let found = find_all(blocks, &self.query, self.options, MATCH_LIMIT);
        self.matches = found.matches;
        self.truncated = found.truncated;
        self.wrapped = None;
        self.current = match self.matches.len() {
            0 => None,
            n => Some(self.first_at_or_after(from).unwrap_or(n - 1)),
        };
        self.state()
    }

    /// The counter for the current state, in `style`'s wording. See [`counter_text`].
    pub fn counter_text(&self, style: CounterStyle) -> String {
        counter_text(&self.state(), self.query.trim().is_empty(), style)
    }

    /// Forget everything: an empty query means the bar holds no search (rule 5).
    fn clear(&mut self) {
        self.matches = Vec::new();
        self.truncated = false;
        self.wrapped = None;
        self.current = None;
    }

    /// The first match at or after `from`, or `None` when the page runs out before one does.
    fn first_at_or_after(&self, from: Option<(usize, Pos)>) -> Option<usize> {
        let (block, pos) = from?;
        self.matches.iter().position(|m| (m.block, m.start) >= (block, pos))
    }

    /// `search`'s rule, used by both browsers' "start from here": the first match at or after `from`, else the
    /// first match with the wrap reported. `None` when there is nothing to point at.
    fn pick_from(&mut self, from: Option<(usize, Pos)>) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        if let Some(i) = self.first_at_or_after(from) {
            return Some(i);
        }
        // Only a search that started partway down the page can run off the bottom and come back to the top;
        // a fresh one picks the first match without having wrapped anywhere.
        self.wrapped = from.is_some().then_some(Wrapped::ToTop);
        Some(0)
    }

    /// Move the current match one way or the other, wrapping at the ends.
    ///
    /// With nothing found, a step changes nothing at all -- not even the wrap flag (rule 7; Firefox leaves the
    /// bar reporting *Phrase not found* and disables the buttons, `findbar.js`:1084-1090, :1116). With one
    /// match, the step stays on it and still reports the wrap, which is what the engine does: it answers
    /// `FIND_WRAPPED` because it searched the whole document again, not because it moved
    /// (`Finder.sys.mjs`:278-279 plays the wrap sound, and `findbar.js`:1080-1082 picks the message by
    /// direction alone).
    fn step(&mut self, forward: bool) -> State {
        if self.matches.is_empty() {
            return self.state();
        }
        let last = self.matches.len() - 1;
        let (at, wrapped) = match self.current {
            // Nothing selected yet: a step forward starts at the top of the page, back at the bottom, and
            // neither has run off an end.
            None => (if forward { 0 } else { last }, None),
            Some(i) if forward => {
                if i < last {
                    (i + 1, None)
                } else {
                    (0, Some(Wrapped::ToTop))
                }
            }
            Some(i) => {
                if i > 0 {
                    (i - 1, None)
                } else {
                    (last, Some(Wrapped::ToBottom))
                }
            }
        };
        self.current = Some(at);
        self.wrapped = wrapped;
        self.state()
    }
}

/// What the bar shows for `state`, in `style`'s wording, or the wrap message in Firefox's case.
///
/// Firefox (`findbar.ftl` @33abb90, the strings copied there):
///
/// - empty query: nothing at all -- the counter is hidden (`findbar.js`:1325-1328).
/// - `total` 0 with a query: *Phrase not found* (`:1089`, `findbar.ftl`:38).
/// - truncated: *More than 1000 matches* (`findbar.ftl`:69-74, with `limit` from the pref; the `total == limit`
///   case is what sets it, `Finder.sys.mjs`:598-600).
/// - otherwise *3 of 12 matches*, and *1 of 1 match* when there is one match: the plural is chosen by the
///   `total`, not by the current index (`findbar.ftl`:60-65). The index is shown one-based, as Firefox counts
///   it (`Finder.sys.mjs`:674-691, `++result.current` over the matches up to the selected one).
/// - a step that wrapped adds its message after the counter, separated by one space. The brief said "instead
///   of", but the findbar has two elements for this and shows both at once: the message goes in
///   `find-status` (`findbar.js`:59, set at :1105-1106) and the counter stays in `found-matches` (:57, set at
///   :1338-1339), and only *Phrase not found* hides the counter (:1088). `find-status` follows `found-matches`
///   in the markup, so that is the order used here. See PORTING.md.
///
/// Chromium (`find_bar_view.cc`:131-144, `IDS_FIND_IN_PAGE_COUNT` with `active_match_ordinal` and
/// `number_of_matches`): *3/12*, and *0/0* for an empty query or no matches -- the ordinal is
/// `active_match_index_ + 1`, so nothing selected reads 0 (`text_finder.cc`:427, `:793`). The count limit is
/// not shown: Chromium has no limit on the total it reports, only throttling while it paints
/// (`text_finder.cc`:1061-1065).
pub fn counter_text(state: &State, query_empty: bool, style: CounterStyle) -> String {
    match style {
        CounterStyle::Firefox => firefox_counter(state, query_empty),
        CounterStyle::Chromium => chromium_counter(state, query_empty),
    }
}

/// Firefox's wording, with the wrap message after the counter when the last step wrapped.
fn firefox_counter(state: &State, query_empty: bool) -> String {
    if query_empty {
        return String::new();
    }
    let counter = if state.total == 0 {
        NOT_FOUND.to_string()
    } else if state.truncated {
        format!("More than {} {}", MATCH_LIMIT, matches_word(MATCH_LIMIT))
    } else {
        // The current index is 0-based here and 1-based on screen.
        let current = state.current.map_or(0, |i| i + 1);
        format!("{} of {} {}", current, state.total, matches_word(state.total))
    };
    match state.wrapped {
        Some(Wrapped::ToTop) => format!("{counter} {WRAPPED_TO_TOP}"),
        Some(Wrapped::ToBottom) => format!("{counter} {WRAPPED_TO_BOTTOM}"),
        None => counter,
    }
}

/// Chromium's wording: `ordinal/total`, `0/0` when there is nothing to show.
fn chromium_counter(state: &State, query_empty: bool) -> String {
    if query_empty || state.total == 0 {
        return "0/0".to_string();
    }
    format!("{}/{}", state.current.map_or(0, |i| i + 1), state.total)
}

/// English's one/other for `n`, the way `findbar.ftl` picks `[one]` against `*[other]`.
fn matches_word(n: usize) -> &'static str {
    if n == 1 { "match" } else { "matches" }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_OPTIONS: Options = Options { match_case: false, whole_word: false, match_diacritics: false };

    fn block(runs: &[&str]) -> Block {
        Block {
            runs: runs
                .iter()
                .enumerate()
                .map(|(i, text)| crate::search::Run { node: i as u32, text: (*text).to_string() })
                .collect(),
        }
    }

    fn at(offset: usize) -> Option<(usize, Pos)> {
        Some((0, Pos { run: 0, offset }))
    }

    /// A finder over "cat cat cat": matches at 0..3, 4..7, 8..11.
    fn three() -> Finder {
        let mut f = Finder::new();
        f.search(&[block(&["cat cat cat"])], "cat", NO_OPTIONS, None);
        f
    }

    #[test]
    fn next_wraps_to_top_and_reports_it() {
        let mut f = three();
        assert_eq!(f.state().current, Some(0));
        assert_eq!(f.next().current, Some(1));
        assert_eq!(f.state().wrapped, None);
        assert_eq!(f.next().current, Some(2));
        // Past the last match: back to the first, and the step says so.
        let wrapped = f.next();
        assert_eq!((wrapped.current, wrapped.wrapped), (Some(0), Some(Wrapped::ToTop)));
        // The next plain step clears it again (rule 7).
        assert_eq!(f.next().wrapped, None);
        assert_eq!(f.state().current, Some(1));
    }

    #[test]
    fn previous_wraps_to_bottom_and_reports_it() {
        let mut f = three();
        assert_eq!(f.state().current, Some(0));
        let wrapped = f.previous();
        assert_eq!((wrapped.current, wrapped.wrapped), (Some(2), Some(Wrapped::ToBottom)));
        assert_eq!(f.previous().current, Some(1));
        assert_eq!(f.state().wrapped, None);
    }

    #[test]
    fn one_match_next_stays_and_wraps() {
        let mut f = Finder::new();
        // "cat dog": one match, 0..3.
        f.search(&[block(&["cat dog"])], "cat", NO_OPTIONS, None);
        let state = f.next();
        assert_eq!((state.current, state.total, state.wrapped), (Some(0), 1, Some(Wrapped::ToTop)));
        assert_eq!(f.current_match().map(|m| m.end.offset), Some(3));
        assert_eq!(
            f.counter_text(CounterStyle::Firefox),
            "1 of 1 match Reached end of page, continued from top"
        );
        assert_eq!(f.previous().wrapped, Some(Wrapped::ToBottom));
    }

    #[test]
    fn no_matches_next_is_noop() {
        let mut f = Finder::new();
        f.search(&[block(&["cat"])], "dog", NO_OPTIONS, None);
        let before = f.state();
        assert_eq!(before, State { current: None, total: 0, truncated: false, wrapped: None });
        assert_eq!(f.next(), before);
        assert_eq!(f.previous(), before);
        assert!(f.matches().is_empty());
        assert_eq!(f.counter_text(CounterStyle::Firefox), NOT_FOUND);
        assert_eq!(f.counter_text(CounterStyle::Chromium), "0/0");
    }

    #[test]
    fn search_from_position_picks_next_match() {
        let mut f = Finder::new();
        // "cat cat cat": from offset 4 the first match at or after it is the second one (4..7).
        f.search(&[block(&["cat cat cat"])], "cat", NO_OPTIONS, at(4));
        assert_eq!(f.state().current, Some(1));
        assert_eq!(f.state().wrapped, None);
        // From offset 5 the match starting at 4 is behind it, so the third one (8..11) is current.
        f.search(&[block(&["cat cat cat"])], "cat", NO_OPTIONS, at(5));
        assert_eq!(f.state().current, Some(2));
    }

    #[test]
    fn search_from_after_last_wraps_to_first() {
        let mut f = Finder::new();
        // The last match of "cat cat cat" starts at 8; asking for 11 finds nothing after it.
        f.search(&[block(&["cat cat cat"])], "cat", NO_OPTIONS, at(11));
        assert_eq!(f.state().current, Some(0));
        assert_eq!(f.state().wrapped, Some(Wrapped::ToTop));
        // A `from` in a block past the last one is the same case.
        f.search(&[block(&["cat"])], "cat", NO_OPTIONS, Some((7, Pos { run: 0, offset: 0 })));
        assert_eq!(f.state().current, Some(0));
        assert_eq!(f.state().wrapped, Some(Wrapped::ToTop));
    }

    /// Rule 8: the caller passes the start of the current match as `from`, so typing one more letter keeps the
    /// match that is current when it still starts where it did.
    #[test]
    fn typing_more_keeps_current_match() {
        let blocks = [block(&["catalog cat"])];
        let mut f = Finder::new();
        // "ca" from offset 5: matches at 0..2 and 8..10, so the second is current.
        f.search(&blocks, "ca", NO_OPTIONS, at(5));
        assert_eq!(f.current_match().map(|m| m.start), Some(Pos { run: 0, offset: 8 }));
        // Typing `t` gives "cat": matches at 0..3 and 8..11. From the current match's start, 8, the one that
        // stays current is the same match -- the search does not jump back to the first.
        let from = f.current_match().map(|m| (m.block, m.start));
        f.search(&blocks, "cat", NO_OPTIONS, from);
        assert_eq!(f.state().current, Some(1));
        assert_eq!(f.current_match().map(|m| m.start), Some(Pos { run: 0, offset: 8 }));
        assert_eq!(f.state().wrapped, None);
        // One more letter and the match is gone: "catx" is nowhere in the text, so nothing is current.
        let from = f.current_match().map(|m| (m.block, m.start));
        f.search(&blocks, "catx", NO_OPTIONS, from);
        assert_eq!(f.state().current, None);
    }

    #[test]
    fn refresh_keeps_nearest_match() {
        let mut f = three();
        // Current is the match at 4..7.
        f.next();
        assert_eq!(f.current_match().map(|m| m.start), Some(Pos { run: 0, offset: 4 }));
        // The text before it is reflowed away: "xxx cat cat" has matches at 4..7 and 8..11, so the one that
        // starts at 4 is still first at or after 4 -- index 0, even though it was index 1.
        f.refresh(&[block(&["xxx cat cat"])]);
        assert_eq!((f.state().current, f.state().total), (Some(0), 2));
        assert_eq!(f.current_match().map(|m| m.start), Some(Pos { run: 0, offset: 4 }));
        // Now the match at 4 goes too: the only one left starts at 8, which is the first at or after 4.
        f.refresh(&[block(&["xxxxxxx cat"])]);
        assert_eq!(f.current_match().map(|m| m.start), Some(Pos { run: 0, offset: 8 }));
        assert_eq!(f.state().wrapped, None);
        // And if nothing starts at or after it any more, rule 9 falls back to the last match.
        f.refresh(&[block(&["cat xxxxxxx"])]);
        assert_eq!(f.state().current, Some(0));
        assert_eq!(f.state().total, 1);
    }

    #[test]
    fn refresh_when_query_vanishes_clears_current() {
        let mut f = three();
        f.refresh(&[block(&["dogs dogs dogs"])]);
        assert_eq!(f.state(), State { current: None, total: 0, truncated: false, wrapped: None });
        assert!(f.matches().is_empty());
        assert_eq!(f.counter_text(CounterStyle::Firefox), NOT_FOUND);
    }

    #[test]
    fn counter_text_firefox_cases() {
        let state = |current: Option<usize>, total: usize, truncated: bool, wrapped: Option<Wrapped>| {
            State { current, total, truncated, wrapped }
        };
        let firefox = |s: State, empty: bool| counter_text(&s, empty, CounterStyle::Firefox);

        // "3 of 12 matches", and the first match of five.
        assert_eq!(firefox(state(Some(2), 12, false, None), false), "3 of 12 matches");
        assert_eq!(firefox(state(Some(0), 5, false, None), false), "1 of 5 matches");
        // One match: `findbar.ftl`:62-64 picks the `[one]` form by `$total`, which drops the trailing `es` and
        // keeps the "N of N" shape. The spec's shorthand "1 match" is this string with the total filled in.
        assert_eq!(firefox(state(Some(0), 1, false, None), false), "1 of 1 match");
        // Truncated, at the pref's 1000 (`findbar.ftl`:69-74).
        assert_eq!(firefox(state(Some(999), MATCH_LIMIT, true, None), false), "More than 1000 matches");
        // Nothing found, and the empty bar.
        assert_eq!(firefox(state(None, 0, false, None), false), "Phrase not found");
        assert_eq!(firefox(state(None, 0, false, None), true), "");
        assert_eq!(firefox(state(Some(0), 5, false, None), true), "");
        // The wrap message comes with the counter, not instead of it (see the `counter_text` docs).
        assert_eq!(
            firefox(state(Some(0), 5, false, Some(Wrapped::ToTop)), false),
            "1 of 5 matches Reached end of page, continued from top"
        );
        assert_eq!(
            firefox(state(Some(4), 5, false, Some(Wrapped::ToBottom)), false),
            "5 of 5 matches Reached top of page, continued from bottom"
        );
        // A wrapped search of one match, the message the findbar shows for it.
        assert_eq!(
            firefox(state(Some(0), 1, false, Some(Wrapped::ToBottom)), false),
            "1 of 1 match Reached top of page, continued from bottom"
        );
    }

    #[test]
    fn counter_text_chromium_cases() {
        let firefox_only = State { current: Some(999), total: MATCH_LIMIT, truncated: true, wrapped: None };
        assert_eq!(counter_text(&State { current: Some(2), total: 12, truncated: false, wrapped: None }, false, CounterStyle::Chromium), "3/12");
        // The limit is not shown, and the wrap message never is: `find_in_page` reports the ordinal and the
        // total and nothing else.
        assert_eq!(counter_text(&firefox_only, false, CounterStyle::Chromium), "1000/1000");
        assert_eq!(
            counter_text(&State { current: Some(0), total: 1, truncated: false, wrapped: Some(Wrapped::ToTop) }, false, CounterStyle::Chromium),
            "1/1"
        );
        // No matches, and an empty query.
        assert_eq!(counter_text(&State { current: None, total: 0, truncated: false, wrapped: None }, false, CounterStyle::Chromium), "0/0");
        assert_eq!(counter_text(&State { current: None, total: 0, truncated: false, wrapped: None }, true, CounterStyle::Chromium), "0/0");
        // Matches but none selected yet, which is ordinal 0 in Chromium too (`text_finder.cc`:793).
        assert_eq!(counter_text(&State { current: None, total: 5, truncated: false, wrapped: None }, false, CounterStyle::Chromium), "0/5");
    }

    /// A blank query clears the search rather than searching for whitespace (rule 5), and the counter goes
    /// quiet with it.
    #[test]
    fn blank_query_clears_everything() {
        let mut f = three();
        f.next();
        for query in ["", "  ", "\u{a0}"] {
            let state = f.search(&[block(&["cat cat cat"])], query, NO_OPTIONS, None);
            assert_eq!(state, State { current: None, total: 0, truncated: false, wrapped: None }, "{query:?}");
            assert!(f.matches().is_empty());
            assert_eq!(f.counter_text(CounterStyle::Firefox), "");
            assert_eq!(f.counter_text(CounterStyle::Chromium), "0/0");
        }
    }

    /// The real limit, from the pref: 1001 matches on the page, 1000 in the list, and *More than*.
    #[test]
    fn search_stops_at_the_match_limit() {
        let text = format!("{}cat", "cat ".repeat(MATCH_LIMIT));
        let mut f = Finder::new();
        let state = f.search(&[block(&[text.as_str()])], "cat", NO_OPTIONS, None);
        assert_eq!(state.total, MATCH_LIMIT);
        assert!(state.truncated);
        assert_eq!(f.matches().len(), MATCH_LIMIT);
        assert_eq!(f.counter_text(CounterStyle::Firefox), "More than 1000 matches");
        // Navigation stays inside the list it was given: 999 steps from the first match land on the last one,
        // and only the step after that wraps.
        for _ in 0..MATCH_LIMIT - 1 {
            f.next();
        }
        assert_eq!(f.state().current, Some(MATCH_LIMIT - 1));
        assert_eq!(f.state().wrapped, None);
        assert_eq!(f.next().wrapped, Some(Wrapped::ToTop));
    }
}
