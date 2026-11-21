/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Stylo's CSS feature switches.
//!
//! Stylo gates newer CSS behind `stylo_static_prefs` entries that default to off, and Servo maps only a handful of them
//! (`components/config/prefs.rs`). Anything else is parsed as invalid, so a stylesheet that says
//! `content: "\200b" " (external)"; content: "\200b" / " (external)";` (MDN's external-link marker) falls back to the first
//! declaration and prints "(external)" over the text, and `@supports (text-overflow: ellipsis)` answers no. This is a class of
//! bug, not a site's: every property or value behind an off switch is dropped from every page that uses it.
//!
//! The switches turned on here are the ones whose features Stylo only has to *parse* and compute: layout either ignores
//! them (the page looks as it did) or implements them. Features that need layout or paint support we do not have
//! (`background-clip: text`, `anchor-positioning`, scroll-driven animations, …) stay off, because accepting the
//! declaration then draws something worse than the fallback the page offers. `LUCINANET_CSS_PREFS` changes the set for
//! one run: `+name` turns a switch on, `-name` off, comma separated, names as in the table below.
//!
//! `:has()` is on, and it is the one switch on with a hole, which is why the hole is written out here rather than
//! hidden in the table. Parsing and matching are complete; *invalidation after a mutation of an element that already
//! has a style* is not. Measured by slice `has-selector-measure` (one binary, run twice): the 229-235 `:has()` rules
//! github.com's home feed was dropping are accepted, the 11 MDN's `CSS/:has` page was dropping are accepted, and a
//! local fixture draws all three shapes (`:has(.b)`, `:has(> .b)`, `:has(+ .b)`: 9 520 px of each signature colour and
//! 30 271 px of newly visible panel, every negative control still at 0 px). Stale is the other half: a DOM mutation
//! that changes whether an already-styled element matches never recomputes that element, so a panel stays revealed
//! after the `.b` that justified it is gone, or stays hidden though the element now matches - while a *newly inserted*
//! element styles itself correctly in the same frame, which is what pins the gap on invalidation and not on parsing.
//! It is on anyway because off is not the neutral side: with the switch off `:has()` does not parse in scripts either
//! (`querySelector`/`matches` throw SyntaxError and `CSS.supports("selector(:has(.a))")` answers false), so a
//! feature-detecting page takes its `:has`-free branch and draws less than it meant to. The open item is recorded in
//! `tools/wpt-reftest/README.md`, and the fix is engine-side, not a preference: at this stylo pin
//! `invalidation/element/relative_selector.rs` is dead code, un-called from `style/data.rs`.
//!
//! The end-to-end check is `tests/css-supports` (a page that lists `CSS.supports` answers for what this module promises).

use stylo_static_prefs::set_pref;

macro_rules! table {
    ($($name:tt),* $(,)?) => {
        /// Every switch this module can set, with the function that sets it (`set_pref!` takes literal names only).
        const SWITCHES: &[(&str, fn(bool))] = &[$(($name, |on| { set_pref!($name, on); })),*];
        /// The names alone, for the tests.
        #[cfg(test)]
        const NAMES: &[&str] = &[$($name),*];
    };
}

table![
    "layout.css.content.alt-text.enabled",
    "layout.css.scroll-behavior.enabled",
    "layout.css.overscroll-behavior.enabled",
    "layout.css.scrollbar-styling.enabled",
    "layout.css.hyphens.enabled",
    "layout.css.line-clamp.enabled",
    "layout.css.text-wrap-style.balance.enabled",
    "layout.css.text-wrap-style.pretty.enabled",
    "layout.css.font-synthesis.enabled",
    "layout.css.font-size-adjust.enabled",
    "layout.css.text-emphasis.enabled",
    "layout.css.text-underline.enabled",
    "layout.css.text-overflow.enabled",
    "layout.css.counters.enabled",
    "layout.css.dominant-baseline.enabled",
    "layout.css.forced-color-adjust.enabled",
    // On with the incomplete pass named in the module doc: matching works, invalidation of an already-styled element does not.
    "layout.css.has-selector.enabled",
    // Off by default here and not turned on: they change what is drawn or laid out, and nothing here implements them.
    "layout.css.background-clip.text.enabled",
    "layout.css.anchor-positioning.enabled",
    "layout.css.contain.enabled",
    "layout.css.appearance.enabled",
];

/// The switches that are on unless `LUCINANET_CSS_PREFS` says otherwise: the first block of the table.
const ON: &[&str] = &[
    "layout.css.content.alt-text.enabled",
    "layout.css.scroll-behavior.enabled",
    "layout.css.overscroll-behavior.enabled",
    "layout.css.scrollbar-styling.enabled",
    "layout.css.hyphens.enabled",
    "layout.css.line-clamp.enabled",
    "layout.css.text-wrap-style.balance.enabled",
    "layout.css.text-wrap-style.pretty.enabled",
    "layout.css.font-synthesis.enabled",
    "layout.css.font-size-adjust.enabled",
    "layout.css.text-emphasis.enabled",
    "layout.css.text-underline.enabled",
    "layout.css.text-overflow.enabled",
    "layout.css.counters.enabled",
    "layout.css.dominant-baseline.enabled",
    "layout.css.forced-color-adjust.enabled",
    "layout.css.has-selector.enabled",
];

/// What `spec` (the `LUCINANET_CSS_PREFS` text) asks for: `(name, on)` pairs, names matched with or without the
/// `layout.css.` prefix and `.enabled` suffix. Unknown names are returned as errors, so a typo is not silent.
pub(crate) fn parse(spec: &str) -> Vec<Result<(&'static str, bool), String>> {
    spec.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| {
            let (on, name) = match item.as_bytes()[0] {
                b'+' => (true, &item[1..]),
                b'-' => (false, &item[1..]),
                _ => (true, item),
            };
            SWITCHES
                .iter()
                .map(|(full, _)| *full)
                .find(|full| {
                    *full == name || full.strip_prefix("layout.css.").and_then(|s| s.strip_suffix(".enabled")) == Some(name)
                })
                .map(|full| (full, on))
                .ok_or_else(|| format!("unknown CSS switch {name}"))
        })
        .collect()
}

/// Set the switches. Call once, before the engine parses a stylesheet.
pub(crate) fn apply() {
    for (name, set) in SWITCHES {
        set(ON.contains(name));
    }
    if let Ok(spec) = std::env::var("LUCINANET_CSS_PREFS") {
        for item in parse(&spec) {
            match item {
                Ok((name, on)) => {
                    if let Some((_, set)) = SWITCHES.iter().find(|(full, _)| *full == name) {
                        set(on);
                    }
                },
                Err(why) => eprintln!("lucinanet: LUCINANET_CSS_PREFS: {why}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_on_switch_is_in_the_table() {
        for name in ON {
            assert!(NAMES.contains(name), "{name} is not in the table");
        }
    }

    #[test]
    fn spec_names_match_short_and_full() {
        let got = parse("+content.alt-text, -layout.css.hyphens.enabled,scroll-behavior");
        assert_eq!(
            got,
            vec![
                Ok(("layout.css.content.alt-text.enabled", true)),
                Ok(("layout.css.hyphens.enabled", false)),
                Ok(("layout.css.scroll-behavior.enabled", true)),
            ]
        );
    }

    #[test]
    fn a_typo_is_an_error_not_a_silent_no_op() {
        assert!(parse("-hyphenz")[0].is_err());
        assert!(parse(",,").is_empty());
    }
}
