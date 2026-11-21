/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Case and diacritic folding: how one page character is compared with one query character.
//!
//! Firefox folds both sides the same way, in this order: case first, then the accent off
//! (`nsFind.cpp` @33abb90:798-803 for the query, :1034-1039 for the page text). The order matters for
//! `É` with both options off: `É` -> `é` -> `e`, so one comparison finds the other.
//!
//! What is *not* here: Firefox also ignores soft hyphens (`nsFind.cpp`:1003-1006) and default-ignorable
//! characters such as ZWSP (:1053-1056), skips a newline between two CJK characters (:1042-1051), and
//! leaves a combining mark in place when it follows a math or music symbol (:998-1001, so `x` + combining
//! mark still reads as `x`). Only the soft hyphen is ported: the rest needs Unicode property tables
//! (Default_Ignorable, CJK classes, Sm/So categories) that std does not carry, and none of them change
//! where a Latin word is found. See PORTING.md.

use crate::search::Options;

/// The soft hyphen. Firefox strips it from the query and skips it in the page, so `for\xadmatted` matches
/// `formatted` (`nsFind.cpp`:805-807, :1003-1006).
pub(crate) const SOFT_HYPHEN: char = '\u{ad}';

/// Unicode simple case folding, one character at a time: what Firefox's `ToFoldedCase` gives per character.
///
/// Folding is *simple*, never full: `ß` does not match `ss`, and the digraph `ǅ` folds to the single lowercase
/// digraph `ǆ` rather than to `dž`. Rust's `char::to_lowercase` is simple casing too, so the two agree, with
/// one exception: U+0130 `İ` is the only character whose Rust lowercase is two characters (`i` + U+0307), and
/// Firefox's table gives plain `i` for it -- which is what the special case below does as well.
fn folded_case(c: char) -> char {
    // Firefox folds the two dotted/dotless I to plain `i` before anything else, the Turkish rules mixed
    // into the ordinary ones (`nsUnicodeProperties.h`:106-112). Rust's table leaves `ı` as itself.
    if matches!(c, '\u{130}' | '\u{131}') {
        return 'i';
    }
    // `to_lowercase` yields at least one character for every `char`.
    c.to_lowercase().next().unwrap_or(c)
}

/// `A` with a diacritic -> `A`: the base letter, what Firefox calls the "naked" character (`ToNaked`,
/// `nsUnicharUtils.cpp`:168-173). Characters with no canonical decomposition of base + mark (`ø`, `ß`, `æ`,
/// `đ`, `ł`) are their own base and come back unchanged, as in Firefox.
pub(crate) fn naked(c: char) -> char {
    match BASE_LETTERS.binary_search_by(|(from, _)| from.cmp(&c)) {
        Ok(i) => BASE_LETTERS.get(i).map_or(c, |(_, to)| *to),
        Err(_) => c,
    }
}

/// Whether `c` is a combining diacritical mark, which Firefox drops from the page text when diacritics
/// are not matched (`nsFind.cpp`:998-1001) and from the query with the rest of the folding
/// (`nsUnicharUtils.cpp`:175-196). So `e` + U+0301 reads as `e`.
///
/// Firefox's set is every character with a nonzero canonical combining class except a few
/// (`base_chars.py`:18-26): kana voicing marks, viramas and classes 91/129/130/132, which is Hebrew
/// points, Arabic harakat and Thai vowels among others. This is the Latin range U+0300-U+036F, where
/// every accented letter a page in our languages writes, and the range the spec pins; marks elsewhere
/// are matched as ordinary characters. See PORTING.md.
pub(crate) fn is_combining_diacritic(c: char) -> bool {
    matches!(c, '\u{300}'..='\u{36f}')
}

/// The character a comparison actually uses: `folded_case` then `naked`, in that order, each skipped when
/// the option says the difference is significant. Both sides of a comparison go through this, which is what
/// lets an accented or capital query character find its plain page counterpart.
pub(crate) fn normalize(c: char, options: Options) -> char {
    let c = if options.match_case { c } else { folded_case(c) };
    if options.match_diacritics { c } else { naked(c) }
}

/// Precomposed Latin letters above ASCII and their base letter: the two blocks a Latin page writes
/// accents in, Latin-1 Supplement and Latin Extended-A (U+00A0-U+017F).
///
/// Every row is a character whose canonical decomposition is one base letter followed by combining
/// diacritical marks, mapped to that base, produced by the rule Firefox's own table generator uses
/// (`intl/unicharutil/util/base_chars.py`:45-63 @33abb90`): decompose, keep it when the first character is
/// not a mark and the second is. `İ` -> `I` is the one row that looks odd (its decomposition is `I` +
/// U+0307 DOT ABOVE), and it is right: with accents ignored, `İ` is an `I`.
///
/// Firefox's table covers all of Unicode (same rule, every plane); `naked` above returns the character
/// itself for anything not in this table, so Greek `ά` folds to `ά` here and to `α` in Firefox.
/// Unicode data, not code from either browser, and the source of a table is the numbers themselves --
/// see PORTING.md for the licence note.
const BASE_LETTERS: &[(char, char)] = &[
    ('À', 'A'), ('Á', 'A'), ('Â', 'A'), ('Ã', 'A'),
    ('Ä', 'A'), ('Å', 'A'), ('Ç', 'C'), ('È', 'E'),
    ('É', 'E'), ('Ê', 'E'), ('Ë', 'E'), ('Ì', 'I'),
    ('Í', 'I'), ('Î', 'I'), ('Ï', 'I'), ('Ñ', 'N'),
    ('Ò', 'O'), ('Ó', 'O'), ('Ô', 'O'), ('Õ', 'O'),
    ('Ö', 'O'), ('Ù', 'U'), ('Ú', 'U'), ('Û', 'U'),
    ('Ü', 'U'), ('Ý', 'Y'), ('à', 'a'), ('á', 'a'),
    ('â', 'a'), ('ã', 'a'), ('ä', 'a'), ('å', 'a'),
    ('ç', 'c'), ('è', 'e'), ('é', 'e'), ('ê', 'e'),
    ('ë', 'e'), ('ì', 'i'), ('í', 'i'), ('î', 'i'),
    ('ï', 'i'), ('ñ', 'n'), ('ò', 'o'), ('ó', 'o'),
    ('ô', 'o'), ('õ', 'o'), ('ö', 'o'), ('ù', 'u'),
    ('ú', 'u'), ('û', 'u'), ('ü', 'u'), ('ý', 'y'),
    ('ÿ', 'y'), ('Ā', 'A'), ('ā', 'a'), ('Ă', 'A'),
    ('ă', 'a'), ('Ą', 'A'), ('ą', 'a'), ('Ć', 'C'),
    ('ć', 'c'), ('Ĉ', 'C'), ('ĉ', 'c'), ('Ċ', 'C'),
    ('ċ', 'c'), ('Č', 'C'), ('č', 'c'), ('Ď', 'D'),
    ('ď', 'd'), ('Ē', 'E'), ('ē', 'e'), ('Ĕ', 'E'),
    ('ĕ', 'e'), ('Ė', 'E'), ('ė', 'e'), ('Ę', 'E'),
    ('ę', 'e'), ('Ě', 'E'), ('ě', 'e'), ('Ĝ', 'G'),
    ('ĝ', 'g'), ('Ğ', 'G'), ('ğ', 'g'), ('Ġ', 'G'),
    ('ġ', 'g'), ('Ģ', 'G'), ('ģ', 'g'), ('Ĥ', 'H'),
    ('ĥ', 'h'), ('Ĩ', 'I'), ('ĩ', 'i'), ('Ī', 'I'),
    ('ī', 'i'), ('Ĭ', 'I'), ('ĭ', 'i'), ('Į', 'I'),
    ('į', 'i'), ('İ', 'I'), ('Ĵ', 'J'), ('ĵ', 'j'),
    ('Ķ', 'K'), ('ķ', 'k'), ('Ĺ', 'L'), ('ĺ', 'l'),
    ('Ļ', 'L'), ('ļ', 'l'), ('Ľ', 'L'), ('ľ', 'l'),
    ('Ń', 'N'), ('ń', 'n'), ('Ņ', 'N'), ('ņ', 'n'),
    ('Ň', 'N'), ('ň', 'n'), ('Ō', 'O'), ('ō', 'o'),
    ('Ŏ', 'O'), ('ŏ', 'o'), ('Ő', 'O'), ('ő', 'o'),
    ('Ŕ', 'R'), ('ŕ', 'r'), ('Ŗ', 'R'), ('ŗ', 'r'),
    ('Ř', 'R'), ('ř', 'r'), ('Ś', 'S'), ('ś', 's'),
    ('Ŝ', 'S'), ('ŝ', 's'), ('Ş', 'S'), ('ş', 's'),
    ('Š', 'S'), ('š', 's'), ('Ţ', 'T'), ('ţ', 't'),
    ('Ť', 'T'), ('ť', 't'), ('Ũ', 'U'), ('ũ', 'u'),
    ('Ū', 'U'), ('ū', 'u'), ('Ŭ', 'U'), ('ŭ', 'u'),
    ('Ů', 'U'), ('ů', 'u'), ('Ű', 'U'), ('ű', 'u'),
    ('Ų', 'U'), ('ų', 'u'), ('Ŵ', 'W'), ('ŵ', 'w'),
    ('Ŷ', 'Y'), ('ŷ', 'y'), ('Ÿ', 'Y'), ('Ź', 'Z'),
    ('ź', 'z'), ('Ż', 'Z'), ('ż', 'z'), ('Ž', 'Z'),
    ('ž', 'z'),
];

#[cfg(test)]
mod tests {
    use super::*;

    const CASE_ON_DIA_ON: Options =
        Options { match_case: true, whole_word: false, match_diacritics: true };
    const CASE_OFF_DIA_ON: Options =
        Options { match_case: false, whole_word: false, match_diacritics: true };

    /// One character of a query compared with one character of the page, the way the matcher in
    /// [`crate::search`] does it: both sides through [`normalize`], then equality.
    fn same(page: char, query: char, options: Options) -> bool {
        normalize(page, options) == normalize(query, options)
    }

    /// The table has to be in character order for `binary_search` to work, and no character may appear
    /// twice. Hand-checked against `base_chars.py`'s rule on the rows: 161 of them, U+00C0 to U+017E.
    #[test]
    fn base_letter_table_is_sorted_and_unique() {
        let mut previous: Option<char> = None;
        for (from, to) in BASE_LETTERS {
            if let Some(before) = previous {
                assert!(before < *from, "{before} is not before {from}");
            }
            previous = Some(*from);
            assert_ne!(*from, *to, "{from} maps to itself");
        }
        assert_eq!(BASE_LETTERS.len(), 161);
    }

    /// Every `char` folds to one character, the way Firefox's one-code-point table does.
    #[test]
    fn folding_is_one_character_per_character() {
        // `ß` is neither `s` nor the two letters `ss`: simple folding does not expand it.
        assert!(!same('ß', 's', Options::default()));
        assert!(!same('ß', 's', CASE_ON_DIA_ON));
        // `ǅ` (U+01C5) folds to the lowercase digraph `ǆ` (U+01C6), not to `dž`, so it is not `d` either.
        assert!(same('\u{1c5}', '\u{1c6}', CASE_OFF_DIA_ON));
        assert!(!same('\u{1c5}', 'd', CASE_OFF_DIA_ON));
        // Astral characters fold inside their own plane: U+1E900 ADLAM CAPITAL ALIF is U+1E922.
        assert!(same('\u{1e900}', '\u{1e922}', CASE_OFF_DIA_ON));
        assert!(!same('\u{1e900}', '\u{1e922}', CASE_ON_DIA_ON));
        // Characters that are their own lowercase, including a default-ignorable one and the last code point.
        assert!(same('\u{e0001}', '\u{e0001}', Options::default()));
        assert!(same('\u{10ffff}', '\u{10ffff}', Options::default()));
    }

    #[test]
    fn dotted_and_dotless_i_fold_to_plain_i() {
        assert!(same('ı', 'i', CASE_OFF_DIA_ON)); // U+0131, Firefox: nsUnicodeProperties.h:109-111
        assert!(same('İ', 'i', CASE_OFF_DIA_ON)); // U+0130
        assert!(!same('ı', 'i', CASE_ON_DIA_ON));
    }

    #[test]
    fn accents_are_stripped_only_when_asked() {
        assert!(same('é', 'e', Options::default()));
        assert!(!same('é', 'e', CASE_ON_DIA_ON));
        assert!(!same('é', 'e', CASE_OFF_DIA_ON)); // match_diacritics on: the accent is the difference
        assert!(!same('ø', 'o', Options::default())); // `ø` has no base + mark decomposition, so it stays `ø`
        assert!(same('ø', 'ø', Options::default()));
    }
}
