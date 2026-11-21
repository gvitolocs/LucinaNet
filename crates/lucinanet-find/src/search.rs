/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Which of the page's text a query matches, and where those matches sit.
//!
//! The page arrives as blocks of runs ([`Block`] of [`Run`]), which is what the engine side will hand us once
//! Servo has a text API: a run is one text node, a block is one block-level box. That is the whole contract,
//! and it is the shape [`find_all`] needs, because a match may continue from one run to the next (`foo<b>bar</b>`
//! holds `foobar`) but never from one block to the next. Firefox gets the same rule out of the tree walk: the
//! search stops at a forced break between blocks (`nsFind.cpp`:906-914) and keeps going across inline elements
//! (:540-548, "for abc should match a\<b\>b\</b\>c"). Chromium builds one flat string per block and stops at the
//! block boundary (`find_buffer.cc`:548-550) -- same rule, different machinery.
//!
//! Offsets are UTF-16 code units (`Pos`), because that is what a DOM Range offset counts, and a match has to be
//! handed back as a range the engine can draw. Characters outside the BMP (`😀`) are two units, which is how
//! Firefox reads them too (`DecodeChar`, `nsFind.cpp`:550-564).
//!
//! A match is found by walking the block's characters, not by building an index: a page is searched once per
//! keystroke, and the walk is the algorithm both browsers use.

use crate::fold;

/// One piece of text inside a block-level element (a text node). `node` is the caller's id for it (opaque here).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub node: u32,
    pub text: String,
}

/// The runs of one block (paragraph, list item, table cell...) in document order. A match may continue across
/// runs of one block (`foo<b>bar</b>` matches `foobar`) but never across blocks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub runs: Vec<Run>,
}

/// A position: run index in the block and an offset in UTF-16 code units inside that run's text (what DOM Range
/// offsets use). Ordering is document order: runs first, then the offset in the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub run: usize,
    pub offset: usize,
}

/// One found occurrence of the query. `end` is exclusive: `[start, end)` is the range to highlight, and it
/// covers everything between the two characters that matched, including a whitespace run and any character
/// the folding skipped over (`nsFind.cpp`:1161-1177 builds the range from the anchor to the last character
/// compared).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub block: usize,
    pub start: Pos,
    pub end: Pos,
}

/// How literal the comparison is. All three are off by default, which is what Firefox's find bar starts with:
/// `findbar.entireword` false and `findbar.matchdiacritics` 0 (`modules/libpref/init/all.js`:493, :499) and case
/// sensitivity 0 = never (`all.js`:476, `accessibility.typeaheadfind.casesensitive`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Options {
    pub match_case: bool,
    pub whole_word: bool,
    pub match_diacritics: bool,
}

/// Every match, in document order, at most `limit`. `truncated` is true when the count stopped at `limit`, so
/// more may exist -- see [`find_all`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub matches: Vec<Match>,
    pub truncated: bool,
}

/// What one token of the page asks for: one folded character, or a run of whitespace of that many characters.
#[derive(Clone, Copy, Debug)]
enum Kind {
    Ch(char),
    Ws(usize),
}

/// One character of a block's text as the matcher sees it, with the UTF-16 range it stands for.
///
/// Two consecutive whitespace characters are one `Ws` token because one space in the query matches a run of
/// them (`nsFind.cpp`:1031-1032, :1196-1204), and the run is the unit the highlight covers.
#[derive(Clone, Copy, Debug)]
struct Tok {
    kind: Kind,
    /// Whether this token continues a word, for the whole-word test. A whitespace run does not.
    word: bool,
    start: Pos,
    end: Pos,
}

/// One character of the query, whitespace runs collapsed the same way, with how many spaces they ask for.
#[derive(Clone, Copy, Debug)]
enum Qtok {
    Ch(char),
    Ws(usize),
}

/// Every match, in document order, at most `limit`. `truncated` is true when more existed.
///
/// Matching per block: case and diacritics folded by [`crate::fold`], whitespace runs collapsed against one
/// space in the query, and each match starting where the previous one ended, so `aa` in `aaaa` gives two
/// matches and never two overlapping ones (`FinderIterator.sys.mjs`:759-761 restarts at the end of the range it
/// just yielded; Chromium restarts the search at the end of the match, `text_finder.cc`:469-472).
///
/// `limit` is a stop, not a filter: as soon as `limit` matches are in hand the scan ends and `truncated` is set,
/// whether or not the page holds more. That is Firefox's counting exactly -- it stops at the limit and reports
/// `total == limit` as "more than" (`Finder.sys.mjs`:598-600), which is why the counter says *More than 1000
/// matches* for a page with precisely 1000. `limit` of 0 finds nothing and is not truncated: with
/// `matchesCountLimit` 0 Firefox does not count at all (`Finder.sys.mjs`:617-622).
pub fn find_all(blocks: &[Block], query: &str, options: Options, limit: usize) -> Found {
    let mut matches: Vec<Match> = Vec::new();
    // A query of nothing, or of nothing but whitespace, is an empty find bar: clear rather than search (spec
    // rule 5; Firefox would match the whitespace run, `nsFind.cpp`:1031 -- see PORTING.md).
    if query.trim().is_empty() || limit == 0 {
        return Found { matches, truncated: false };
    }
    let Some(q) = query_tokens(query, options) else {
        return Found { matches, truncated: false };
    };

    for (block, toks) in blocks.iter().enumerate().map(|(b, blk)| (b, view(blk, options))) {
        let mut at = 0;
        while at < toks.len() {
            match match_from(&toks, &q, at, options.whole_word) {
                // The next attempt starts where this match ended, so matches never overlap.
                Some((next, start, end)) => {
                    matches.push(Match { block, start, end });
                    if matches.len() >= limit {
                        return Found { matches, truncated: true };
                    }
                    at = next;
                }
                None => at += 1,
            }
        }
    }
    Found { matches, truncated: false }
}

/// The block's text as the matcher reads it: folded, invisible characters gone, whitespace runs collapsed.
///
/// Dropped, in Firefox's order: the soft hyphen always (`nsFind.cpp`:1003-1006) and combining diacritical marks
/// when the accents are not matched (:998-1001). Both leave the run they were in but take no token, so a match
/// spanning them still highlights them, which is what the range from anchor to last character does in Firefox.
fn view(block: &Block, options: Options) -> Vec<Tok> {
    let mut toks: Vec<Tok> = Vec::new();
    // The whitespace run being collected, if any: where it started, where it has reached, how many characters.
    let mut ws: Option<(Pos, Pos, usize)> = None;

    for (run, node) in block.runs.iter().enumerate() {
        let mut offset = 0;
        for c in node.text.chars() {
            let start = Pos { run, offset };
            offset += u16_units(c);
            let end = Pos { run, offset };
            if c == fold::SOFT_HYPHEN || (!options.match_diacritics && fold::is_combining_diacritic(c)) {
                continue;
            }
            if is_space(c) {
                ws = match ws {
                    // Still the same run of whitespace: extend it, its start stays where it was.
                    Some((from, _, count)) => Some((from, end, count + 1)),
                    None => Some((start, end, 1)),
                };
                continue;
            }
            if let Some((from, to, count)) = ws.take() {
                toks.push(Tok { kind: Kind::Ws(count), word: false, start: from, end: to });
            }
            toks.push(Tok {
                kind: Kind::Ch(fold::normalize(c, options)),
                word: is_word_char(c),
                start,
                end,
            });
        }
    }
    if let Some((from, to, count)) = ws.take() {
        toks.push(Tok { kind: Kind::Ws(count), word: false, start: from, end: to });
    }
    toks
}

/// The query as tokens, or `None` when nothing is left of it after the folding (a query of one combining mark,
/// with the marks dropped, is no query at all -- Firefox early-exits on the empty pattern, `nsFind.cpp`:811-815).
fn query_tokens(query: &str, options: Options) -> Option<Vec<Qtok>> {
    let mut q: Vec<Qtok> = Vec::new();
    for c in query.chars() {
        if c == fold::SOFT_HYPHEN || (!options.match_diacritics && fold::is_combining_diacritic(c)) {
            continue;
        }
        if is_space(c) {
            // Consecutive spaces in the query ask for that many whitespace characters, and the last one takes
            // whatever is left of the page's run (`nsFind.cpp`:1199-1204 advances over a space when the next
            // query character is a space too).
            match q.last_mut() {
                Some(Qtok::Ws(count)) => *count += 1,
                _ => q.push(Qtok::Ws(1)),
            }
            continue;
        }
        q.push(Qtok::Ch(fold::normalize(c, options)));
    }
    if q.is_empty() { None } else { Some(q) }
}

/// Whether the query matches at token `at`: the token index one past the match and the UTF-16 span of what it
/// used, or `None`.
///
/// With `whole_word`, the characters on both sides of the match must not be word characters, and the edge of a
/// block counts as one (`nsFind.cpp`:1071-1076 for the start, :1147-1152 for the end; a `0` from `PeekNextChar`
/// means the block ran out, and the match is kept).
fn match_from(toks: &[Tok], q: &[Qtok], at: usize, whole_word: bool) -> Option<(usize, Pos, Pos)> {
    if whole_word && !boundary_before(toks, at) {
        return None;
    }
    let mut i = at;
    for qt in q {
        let tok = toks.get(i)?;
        match (qt, tok.kind) {
            (Qtok::Ch(c), Kind::Ch(t)) if t == *c => {}
            (Qtok::Ws(count), Kind::Ws(have)) if have >= *count => {}
            _ => return None,
        }
        i += 1;
    }
    if whole_word && !boundary_after(toks, i) {
        return None;
    }
    // `q` is never empty (see `query_tokens`), so `i` is at least `at` + 1 and both tokens exist.
    Some((i, toks.get(at)?.start, toks.get(i - 1)?.end))
}

/// A word boundary before token `at`: the token before it is not a word character, or there is none.
fn boundary_before(toks: &[Tok], at: usize) -> bool {
    let before = at.checked_sub(1).and_then(|j| toks.get(j));
    !before.is_some_and(|tok| tok.word)
}

/// A word boundary after the match: the token at `after` is not a word character, or the block ended.
fn boundary_after(toks: &[Tok], after: usize) -> bool {
    !toks.get(after).is_some_and(|tok| tok.word)
}

/// Whitespace as Firefox's matcher counts it: the four ASCII whitespace characters (`nsCRTGlue.h`:115-117, via
/// `nsCRT::IsAsciiSpace`, `nsCRT.h`:109-111) plus NBSP (`nsFind.cpp`:718-727). Vertical tab and form feed are
/// *not* in that set, and characters above U+FFFF are never whitespace (`nsFind.cpp`:725-727).
fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\r' | '\n' | '\t' | '\u{a0}')
}

/// Whether `c` continues a word, for `whole_word`. Firefox asks its ICU word segmenter (`BreakInBetween`,
/// `nsFind.cpp`:634-642); UAX #29 puts no break between a letter and a letter, a letter and a digit, or either
/// and `_` (ExtendNumLet), which is what this says in one line. `char::is_alphanumeric` is Alphabetic + Nd +
/// Nl + No, so accented and non-Latin letters count as word characters too.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// UTF-16 code units `c` occupies: two above the BMP, one otherwise (`nsFind.cpp`:550-564 reads a surrogate
/// pair as one character whose offsets take two units).
fn u16_units(c: char) -> usize {
    usize::from(c as u32 > 0xFFFF) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_OPTIONS: Options = Options { match_case: false, whole_word: false, match_diacritics: false };
    const CASE: Options = Options { match_case: true, whole_word: false, match_diacritics: false };
    const WORD: Options = Options { match_case: false, whole_word: true, match_diacritics: false };
    const DIA: Options = Options { match_case: false, whole_word: false, match_diacritics: true };

    /// One block, one run per string, `node` = the index in the list.
    fn block(runs: &[&str]) -> Block {
        Block {
            runs: runs
                .iter()
                .enumerate()
                .map(|(i, text)| Run { node: i as u32, text: (*text).to_string() })
                .collect(),
        }
    }

    /// One block per string.
    fn page(blocks: &[&[&str]]) -> Vec<Block> {
        blocks.iter().map(|runs| block(runs)).collect()
    }

    /// One block of one run, no limit.
    fn find(run: &str, query: &str) -> Found {
        find_all(&[block(&[run])], query, NO_OPTIONS, usize::MAX)
    }

    fn m(block: usize, start: usize, end: usize) -> Match {
        Match {
            block,
            start: Pos { run: 0, offset: start },
            end: Pos { run: 0, offset: end },
        }
    }

    #[test]
    fn finds_plain_word_in_order() {
        // "cat and cat": c0 a1 t2 sp3 a4 n5 d6 sp7 c8 a9 t10 -> 0..3 and 8..11.
        // "another cat": a0 n1 o2 t3 h4 e5 r6 sp7 c8 a9 t10 -> block 1, 8..11.
        let found = find_all(&page(&[&["cat and cat"], &["another cat"]]), "cat", NO_OPTIONS, usize::MAX);
        assert_eq!(found.matches, vec![m(0, 0, 3), m(0, 8, 11), m(1, 8, 11)]);
        assert!(!found.truncated);
    }

    /// The next search starts after the end of the previous match: `aa` in `aaaa` gives 2, not 3
    /// (spec rule 1; `FinderIterator.sys.mjs`:759-761 collapses the restart point to the range's end).
    #[test]
    fn overlapping_text_gives_non_overlapping_matches() {
        assert_eq!(find("aaaa", "aa").matches, vec![m(0, 0, 2), m(0, 2, 4)]);
        // `aaa` in `aaaaa`: 0..3, then the rest is `aa`, too short -> one match.
        assert_eq!(find("aaaaa", "aaa").matches, vec![m(0, 0, 3)]);
    }

    #[test]
    fn case_insensitive_by_default() {
        // "Cat CAT cat" -> 0..3, 4..7, 8..11.
        assert_eq!(
            find("Cat CAT cat", "cat").matches,
            vec![m(0, 0, 3), m(0, 4, 7), m(0, 8, 11)]
        );
    }

    #[test]
    fn match_case_respects_case() {
        assert_eq!(
            find_all(&[block(&["Cat CAT cat"])], "cat", CASE, usize::MAX).matches,
            vec![m(0, 8, 11)]
        );
    }

    #[test]
    fn whole_word_rejects_inside_word() {
        // "category cat": `cat` inside `category` starts at a word boundary but does not end at one -> rejected.
        // Offsets: c0 a1 t2 e3 g4 o5 r6 y7 sp8 c9 a10 t11 -> only 9..12.
        let found = find_all(&[block(&["category cat"])], "cat", WORD, usize::MAX);
        assert_eq!(found.matches, vec![m(0, 9, 12)]);
    }

    #[test]
    fn whole_word_accepts_punctuation_neighbours() {
        // "cat, cat. (cat)": 0..3, 5..8, 11..14 -- `,`, `.`, `(` and `)` are not word characters.
        let found = find_all(&[block(&["cat, cat. (cat)"])], "cat", WORD, usize::MAX);
        assert_eq!(found.matches, vec![m(0, 0, 3), m(0, 5, 8), m(0, 11, 14)]);
    }

    #[test]
    fn whole_word_treats_underscore_and_digits_as_word_chars() {
        // "cat_ cat1 1cat cat": 0..3 followed by `_` -> no; 5..8 followed by `1` -> no;
        // 11..14 preceded by `1` -> no; 15..18 at the end of the block -> yes.
        let found = find_all(&[block(&["cat_ cat1 1cat cat"])], "cat", WORD, usize::MAX);
        assert_eq!(found.matches, vec![m(0, 15, 18)]);
    }

    /// A match may continue across runs of one block: `foo<b>bar</b>` holds `foobar`, starting in run 0 and
    /// ending in run 1 (spec rule 1; the comment at `nsFind.cpp`:540-543 is about precisely this case).
    #[test]
    fn match_spans_inline_runs() {
        let found = find_all(&[block(&["foo", "bar"])], "foobar", NO_OPTIONS, usize::MAX);
        assert_eq!(
            found.matches,
            vec![Match {
                block: 0,
                start: Pos { run: 0, offset: 0 },
                end: Pos { run: 1, offset: 3 },
            }]
        );
    }

    #[test]
    fn match_does_not_span_blocks() {
        let blocks = page(&[&["foo"], &["bar"]]);
        assert!(find_all(&blocks, "foobar", NO_OPTIONS, usize::MAX).matches.is_empty());
        // The second block still matches on its own: block 1, 0..3.
        assert_eq!(find_all(&blocks, "bar", NO_OPTIONS, usize::MAX).matches, vec![m(1, 0, 3)]);
    }

    #[test]
    fn diacritics_ignored_by_default() {
        // "resume" finds the precomposed form: r0 é1 s2 u3 m4 e5, one UTF-16 unit per character -> 0..6.
        assert_eq!(find("résumé", "resume").matches, vec![m(0, 0, 6)]);
        // "re\u{301}sume\u{301}": r0 e1 ́2 s3 u4 m5 e6 ́7. The marks are invisible to the comparison, so the
        // match runs 0..7: the mark between `e` and `s` is inside the span, the trailing one after the last
        // matched character (`e` at 6) is not (`nsFind.cpp`:1176 ends the range at the last character + 1).
        assert_eq!(find("re\u{301}sume\u{301}", "resume").matches, vec![m(0, 0, 7)]);
        // The other direction, per spec rule 2: an accented query matches the plain base too.
        assert_eq!(find("resume", "résumé").matches, vec![m(0, 0, 6)]);
    }

    #[test]
    fn diacritics_matter_when_asked() {
        assert!(find_all(&[block(&["résumé"])], "resume", DIA, usize::MAX).matches.is_empty());
        assert_eq!(find_all(&[block(&["résumé"])], "résumé", DIA, usize::MAX).matches, vec![m(0, 0, 6)]);
        // With diacritics matched, a combining mark is a character like any other: `e\u{301}` is not `e`.
        assert!(find_all(&[block(&["re\u{301}sumé"])], "resume", DIA, usize::MAX).matches.is_empty());
    }

    /// One space in the query matches a whole run of whitespace in the page, and the span covers the run
    /// (spec rule 4; `nsFind.cpp`:1031-1032 enters whitespace mode, :1196-1204 does not advance the query over
    /// more page whitespace, :1154-1158 keeps going while the next character is whitespace too).
    #[test]
    fn whitespace_runs_match_one_space() {
        // "a   b": a0 sp1 sp2 sp3 b4 -> one match over the whole run, 0..5.
        assert_eq!(find("a   b", "a b").matches, vec![m(0, 0, 5)]);
        // NBSP is whitespace: a0 NBSP1 b2 -> 0..3.
        assert_eq!(find("a\u{a0}b", "a b").matches, vec![m(0, 0, 3)]);
        // Tab and newline too, and a run across an inline element boundary.
        assert_eq!(find("a\t\nb", "a b").matches, vec![m(0, 0, 4)]);
        assert_eq!(
            find_all(&[block(&["a ", " b"])], "a b", NO_OPTIONS, usize::MAX).matches,
            vec![Match {
                block: 0,
                start: Pos { run: 0, offset: 0 },
                end: Pos { run: 1, offset: 2 },
            }]
        );
        // Two spaces in the query ask for two whitespace characters: Firefox advances the query over the first
        // because the next query character is a space as well (`nsFind.cpp`:1199-1204), so `a  b` does not
        // match `a b` but does match `a   b`.
        assert!(find("a b", "a  b").matches.is_empty());
        assert_eq!(find("a   b", "a  b").matches, vec![m(0, 0, 5)]);
        // A query space matches a page space that is not a run of its own (the run here is one character).
        assert_eq!(find("ab", "a b").matches, vec![]);
    }

    #[test]
    fn offsets_are_utf16_units() {
        // "😀cat": the emoji is one character and two UTF-16 units, so `cat` starts at 2, not at 4 (the byte
        // count in UTF-8) and not at 1 (the character count).
        assert_eq!(find("\u{1f600}cat", "cat").matches, vec![m(0, 2, 5)]);
        // Two runs: the emoji is in run 0, `cat` starts run 1 at offset 0 -- offsets are per run.
        assert_eq!(
            find_all(&[block(&["\u{1f600}", "cat"])], "cat", NO_OPTIONS, usize::MAX).matches,
            vec![Match {
                block: 0,
                start: Pos { run: 1, offset: 0 },
                end: Pos { run: 1, offset: 3 },
            }]
        );
        // A match that starts on an astral character spans its two units.
        assert_eq!(find("a\u{1f600}", "\u{1f600}").matches, vec![m(0, 1, 3)]);
    }

    #[test]
    fn empty_and_blank_query_find_nothing() {
        for query in ["", " ", "   ", "\u{a0}", "\t\n", " \u{a0} "] {
            let found = find("a cat sat", query);
            assert!(found.matches.is_empty(), "{query:?} found {:?}", found.matches);
            assert!(!found.truncated);
        }
        // A query that is nothing but a combining mark disappears when the marks are folded away.
        assert!(find_all(&[block(&["e\u{301}"])], "\u{301}", NO_OPTIONS, usize::MAX).matches.is_empty());
    }

    #[test]
    fn limit_truncates_and_flags() {
        // "cat cat cat cat" has 4 matches at offsets 0..3, 4..7, 8..11, 12..15.
        let found = find_all(&[block(&["cat cat cat cat"])], "cat", NO_OPTIONS, 2);
        assert_eq!(found.matches, vec![m(0, 0, 3), m(0, 4, 7)]);
        assert!(found.truncated);
        // Exactly `limit` matches is truncated as well: Firefox stops counting at the limit and calls the
        // total -1, so the bar reads "More than ..." (`Finder.sys.mjs`:598-600).
        let exact = find_all(&[block(&["cat cat"])], "cat", NO_OPTIONS, 2);
        assert_eq!(exact.matches.len(), 2);
        assert!(exact.truncated);
        assert!(!find_all(&[block(&["cat cat"])], "cat", NO_OPTIONS, 3).truncated);
        // A limit of 0 does not count, and is not reported as truncated.
        assert_eq!(find_all(&[block(&["cat"])], "cat", NO_OPTIONS, 0), Found { matches: vec![], truncated: false });
    }

    #[test]
    fn unicode_case_folding_non_ascii() {
        // `É` finds `é` with diacritics on and case off: folding is Unicode, not ASCII-only.
        assert_eq!(find_all(&[block(&["é"])], "É", DIA, usize::MAX).matches, vec![m(0, 0, 1)]);
        assert_eq!(find_all(&[block(&["É"])], "é", DIA, usize::MAX).matches, vec![m(0, 0, 1)]);
        // With case matched as well they are different characters.
        assert!(find_all(&[block(&["é"])], "É", Options { match_case: true, match_diacritics: true, whole_word: false }, usize::MAX)
            .matches
            .is_empty());
        // And the astral plane folds too: U+1E900 ADLAM CAPITAL LETTER ALIF is U+1E922 ADLAM SMALL
        // LETTER ALIF, and one astral character is two UTF-16 units, so the span is 0..2.
        assert_eq!(
            find_all(&[block(&["\u{1e922}"])], "\u{1e900}", DIA, usize::MAX).matches,
            vec![m(0, 0, 2)]
        );
    }

    /// Firefox ignores soft hyphens in the page and strips them from the query (`nsFind.cpp`:805-807, :1003-1006).
    #[test]
    fn soft_hyphens_are_ignored() {
        // "for\xadmatted" reads "formatted" to the matcher: f0 o1 r2 SHY3 m4 a5 t6 t7 e8 d9.
        assert_eq!(find("for\u{ad}matted", "formatted").matches, vec![m(0, 0, 10)]);
        assert_eq!(find("formatted", "for\u{ad}matted").matches, vec![m(0, 0, 9)]);
    }

    /// An empty run (an empty text node) contributes nothing and does not shift the offsets, which are per run.
    #[test]
    fn empty_runs_are_skipped() {
        assert_eq!(
            find_all(&[block(&["", "cat", ""])], "cat", NO_OPTIONS, usize::MAX).matches,
            vec![Match {
                block: 0,
                start: Pos { run: 1, offset: 0 },
                end: Pos { run: 1, offset: 3 },
            }]
        );
    }
}
