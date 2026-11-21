# Porting notes — `lucinanet-find`

Find-in-page logic: which text a query matches, which of those matches is current, what the counter says. Ctrl+F
without a DOM: the page arrives as `Block`s of `Run`s handed over by the engine side, and the matches go back out as
`(block, Pos)` ranges.

Revisions read, both `main` of 2026-11-11:

- **Firefox** `mozilla-firefox/firefox` @`33abb900fc1646fef2e5ffed2d6b22e806ae10c1` — MPL-2.0
- **Chromium** `chromium/chromium` @`e9a46735de970a8e31217f2233efa3a47a1c337d` — BSD-3-Clause

Sources were fetched with curl into `<work-dir>/lucinanet/batch1/src/` (outside the repository). Verdicts below:

| verdict | meaning |
| --- | --- |
| adopt | the rule, value or string is taken as the original has it |
| adapt | the original's behaviour, re-expressed for a text-only Rust API |
| reference | read to settle a question; nothing taken |
| reject | read and deliberately not ported, reason given |

## Firefox sources (MPL-2.0)

| File | Verdict | What it decided |
| --- | --- | --- |
| `toolkit/components/find/nsFind.cpp` | adapt | the matcher. Per-character comparison with case folding then the accent off (`:1034-1039`); the same folding applied to the query (`:798-803`); soft hyphen stripped from the query (`:805-807`, `CH_SHY` `:43-47`) and skipped in the page (`:1003-1006`); combining marks skipped unless diacritics are matched (`:998-1001`); the whitespace state machine (`:1031-1032` enter, `:1147-1158` accumulate a page run, `:1196-1204` one query space absorbs the whole run); word boundaries by `BreakInBetween` (`:634-642`, applied `:1071-1076` and `:1147-1152`); surrogate pairs and UTF-16 indices (`:550-564`); the matched range spans anchor to last matched character (`:1161-1177`); a fragment stops at a block boundary (`:906-914`); NBSP counts as space (`:718-727`) |
| `toolkit/modules/Finder.sys.mjs` | adapt | searching starts from the current selection (`:211-216`, `fastFind` always `FIND_INITIAL`); *find again* continues from it (`:254-263`); a wrap is a result code, not a move (`:278-279` plays the wrap sound for it); the matches count is `total == limit → -1` (`:597-599`) and an empty or not-found search counts as 0/0 (`:612-622`) |
| `toolkit/modules/FinderIterator.sys.mjs` | adapt | highlight-all restarts the search at the end of the range it just yielded, so matches never overlap (`:759-761`); the limit stops the counting loop (`:535`, `:561-562`) |
| `toolkit/content/widgets/findbar.js` | adapt | the counter and the status message are two elements (`found-matches` `:57`, `find-status` `:59`) and a wrap writes only the status (`:1077-1083`), leaving the counter on screen; *Phrase not found* hides the counter (`:1084-1090`) and disables both step buttons (`:1113-1119`); an empty field hides the counter (`:1324-1328`) and the input handler searches whatever the field holds, including blanks (`:176-184`) |
| `toolkit/locales/en-US/toolkit/main-window/findbar.ftl` | adopt | the strings, copied verbatim: `findbar-not-found` (`:38`), `findbar-wrapped-to-top` (`:40`), `findbar-wrapped-to-bottom` (`:41`), `findbar-found-matches` with its `[one]`/`*[other]` plural on `$total` (`:60-65`), `findbar-found-matches-count-limit` (`:69-74`), and the option labels (`:15-36`) that name the three `Options` fields |
| `modules/libpref/init/all.js` | adopt | `accessibility.typeaheadfind.matchesCountLimit` 1000 → `MATCH_LIMIT` (`:491`); the defaults behind `Options::default()`: case sensitivity 0 = never (`:476`), `findbar.entireword` false (`:493`), `findbar.matchdiacritics` 0 (`:499`) |
| `intl/unicharutil/util/nsUnicharUtils.cpp` | adapt | `ToFoldedCase` (`:152-166`) and `ToNaked` (`:168-173`): one character in, one character out, surrogate pairs folded as one. `ToNaked(nsAString&)` (`:175-196`) is where the marks vanish from a whole string, skipping any character `IsCombiningDiacritic` accepts (`nsUnicodeProperties.cpp`:180-182, a generated bitset) |
| `intl/unicharutil/operators/nsUnicodeProperties.h` | adapt | `GetFoldedcase` gives plain `i` for both U+0130 `İ` and U+0131 `ı` (`:106-112`) — the one special case in `folded_case`, and the reason `İ` is not in the base-letter table's way |
| `intl/unicharutil/util/base_chars.py` | reference | the rule that generates Gecko's naked-character table (canonically decompose; keep the row when the first character is not a combining diacritic and the second is, `:45-67`), and the combining-diacritic predicate with its excluded classes (`:9-27`). `BASE_LETTERS` in `fold.rs` is that rule run over U+00A0-U+017F |
| `intl/unicharutil/util/is_combining_diacritic.py` | reference | the same predicate and its exclusion list (`:14-24`), i.e. which marks Firefox ignores and which it does not |
| `xpcom/ds/nsCRT.h`, `xpcom/ds/nsCRTGlue.h` | adopt | `nsCRT::IsAsciiSpace` is exactly space, CR, LF, TAB (`nsCRT.h`:109-111, `nsCRTGlue.h`:115-117) — no vertical tab, no form feed |
| `toolkit/components/find/nsFind.h` | reject | the `nsIFind` interface, its `State` struct and the iterator callbacks: they exist to walk a content tree, which this crate does not own |
| `toolkit/modules/Finder.jsm` selection/highlight code, `nsTypeAheadFind` | reject | selection painting, `FIND_LINKS`/typeahead modes, sub-frame cycling, the search cache, sounds, telemetry. UI and engine plumbing on both sides of the boundary this crate sits at |
| `toolkit/components/find/nsFind.cpp` CJK newline (`:1042-1051`), default-ignorables (`:1053-1056`), math/music exception (`:998-1001`) | reject | see "Not ported" |

## Chromium sources (BSD-3-Clause)

| File | Verdict | What it decided |
| --- | --- | --- |
| `third_party/blink/renderer/core/editing/finder/find_buffer.cc` | reference | the block model: text is concatenated per *uninterrupted block*, and a node whose layout block flow differs from the starting one breaks the run (`:365-403`, `:545-550`) — the same "a match crosses inline elements but never a block boundary" rule Firefox gets from its tree walk. Offsets are UTF-16 code units in that flat buffer |
| `third_party/blink/renderer/core/editing/finder/find_results.cc` | reference | the result iteration: each searcher is advanced only when its match is the one just reported (`:117-129`), so consecutive matches never overlap; `IsInvalidMatch` (`find_buffer.cc`:260-276) drops matches that do not resolve to real text positions |
| `third_party/blink/renderer/core/editing/iterators/text_searcher_icu.cc` | reference, reject | where Chromium's matching actually happens, and why none of it is portable here: matching is ICU `usearch` at a collation strength, so case and accent insensitivity are one dial, not two (`:286-296`); whole word means "the match is exactly one ICU word", found by walking word boundaries backwards from the match end (`:127-149`), with a CJK special case that calls any match starting at an ideograph whole-word (`:136-138`); overlapping matches are an option (`:182`) rather than the default. Rejected in favour of Firefox's per-character model |
| `third_party/blink/renderer/core/editing/finder/find_options.h` | reference | the option shape at this revision: case, whole word, word-bounded start/end, overlap (`:64-88`) — no independent "match diacritics" flag; that is a runtime feature inside the searcher |
| `third_party/blink/renderer/core/editing/finder/text_finder.cc` | reference | ordinals for the counter: the reported index is `active_match_index_ + 1`, `0` when nothing is selected, and `-1` is the empty state (`:417-427`, `:738-745`); stepping past the end wraps by modular arithmetic on the match count (`:417-423`) |
| `third_party/blink/renderer/core/editing/finder/unicode_utilities.cc` | reference | Chromium also neutralises the soft hyphen, and additionally folds six typographic quote marks to ASCII (`:60-87`); its separator notion is an ICU P/Z/Cf table (`:89-126`), not a four-character list |
| `components/find_in_page/find_tab_helper.cc`, `find_in_page/find_notification_details.h` | reference | the two numbers that reach the UI: `number_of_matches_` ("How many matches were found", `:53`) and `active_match_ordinal_` ("The ordinal of the currently selected match", `:55`), both `-1` in the default-constructed details (`:26-28`), and `find_tab_helper.cc` passing the tab's own counts through. Ordinal and total are 1-based, `0` meaning nothing is selected |
| `chrome/browser/ui/views/find_bar_view.cc` | reference | the counter's rendering: `IDS_FIND_IN_PAGE_COUNT` filled with `FormatNumber(active_match_ordinal())` then `FormatNumber(number_of_matches())` (`:141-144`) — the `3/12` shape `CounterStyle::Chromium` copies, with the numbers in that order and no words around them |
| `third_party/blink/renderer/core/frame/...` node walking, mark-all, rects, scroll-into-view, `async_find_buffer.cc`, extension search | reject | the rest of Blink's find: it needs a layout tree |

`find_buffer.cc`:548-550 and `text_finder.cc`:469-472 are the two Chromium lines the crate's comments lean on most; both
were read at the pinned sha. Nothing else was taken from Chromium — no table, no formula, no constant — which is why
there is no `NOTICE` file (see Licences).

### Paths that did not resolve, and where the rule was read instead

- The spec points at `find_buffer.cc` for whole word "via `IsWordBoundary`". No symbol of that name exists at either
  pinned sha: at `e9a46735` the whole-word test is `IsWholeWordMatch`, `FindWordStartBoundary` and
  `FindWordEndBoundary`, called from `ShouldSkipCurrentMatch` in
  `third_party/blink/renderer/core/editing/iterators/text_searcher_icu.cc` (note `iterators/`, not `finder/` — the
  first fetch under `finder/` returned 404, and the directory listing at the pinned sha shows the file is not there).
  Read from the correct path at the pinned sha, so nothing had to be taken from `main`.
- `toolkit/components/find/IsCombiningDiacritic.h` and `BaseChars.h` are generated at build time and are not in the
  Firefox tree (`nsUnicodeProperties.cpp`:10-11 includes them); the tables' *rules* were read from their generators,
  `intl/unicharutil/util/base_chars.py` and `intl/unicharutil/util/is_combining_diacritic.py`, both at `33abb90`.
  `ch-find_bar_view_linux.cc`, `ch-find_bubble_view.cc` and `ch-find_in_page_switches.h` came back as 404 bodies:
  those paths exist only as BUILD.gn entries at this revision. None of them was needed.

## The ten rules, and what settled each

1. **Runs join inside a block, UTF-16 offsets, non-overlapping.** `Pos.offset` counts UTF-16 code units, so
   astral characters cost 2 (`nsFind.cpp`:550-564, `DecodeChar`; Chromium's flat buffer indices are UTF-16 too).
   The next scan starts one past the end of a match: Firefox collapses the restart point to the range's end
   (`FinderIterator.sys.mjs`:759-761) and Chromium advances only the searcher that produced the reported match
   (`find_results.cc`:117-129). `aa` in `aaaa` therefore gives 2 matches, not 3.
2. **Case.** Simple case folding, one character at a time, per Gecko's `GetFoldedcase` (`nsUnicharUtils.cpp`:161-173),
   which is a one-code-point table: `ß` matches neither `s` nor `ss`, and the digraph `ǅ` folds to `ǆ` and so matches
   neither `d` nor `z`. Rust's `char::to_lowercase` is simple casing as well, so it is used directly, with U+0130 and
   U+0131 special-cased to `i` (`nsUnicodeProperties.h`:106-112).
3. **Diacritics.** Case first, then the base letter, in that order, each step skipped when its option is on
   (`nsFind.cpp`:798-803 for the query, `:1034-1039` for the page) — which is why `É` and `é` and `e` are one
   comparison with both options off. Marks are dropped from the page text (`:998-1001`) and from the query (the same
   folding), so `re\u{301}sume\u{301}` matches `resume` and, with accents off, `résumé` matches `resume`.
   Soft hyphens disappear from both sides (`:805-807`, `:1003-1006`), so `for\xadmatted` is `formatted`.
4. **Whole word.** The characters on either side of the match must not be word characters, and the edge of a block
   counts as a boundary (`nsFind.cpp`:1071-1076, `:1147-1152` treat a ran-out-of-fragment as a boundary). The
   character class is the spec's (`char::is_alphanumeric() || '_'`); both browsers use a word segmenter instead —
   see the deviation below.
5. **Whitespace.** A maximal run of page whitespace is one token, and a run of query spaces is one token asking for
   that many; a page run of at least as many characters matches, and the range covers the whole run
   (`nsFind.cpp`:1031-1032, `:1147-1158`, `:1161-1177`, `:1196-1204`). Whitespace is space, CR, LF, TAB, NBSP
   (`nsCRTGlue.h`:115-117 + `nsFind.cpp`:718-727); nothing above U+FFFF is ever whitespace (`:725-727`). A consequence
   of the model, and of Firefox's matcher: a match never begins or ends inside a whitespace run, because a run that
   is only partly consumed leaves either a page space with no query space to absorb it or a query space with no page
   space left.
6. **Limit.** `find_all` stops after `limit` matches and sets `truncated`; `Finder` passes `MATCH_LIMIT`, 1000
   (`all.js`:491). Firefox reports `total == limit` as `-1` and the bar then says *More than 1000 matches*
   (`Finder.sys.mjs`:597-599, `findbar.ftl`:69-74), so a page with exactly 1000 matches reads as "more than" there as
   well as here.
7. **Navigation.** `next` past the last match goes to the first and reports `ToTop`; `previous` before the first goes
   to the last with `ToBottom`; any other step clears `wrapped`; `search` clears it unless its own `from` ran off the
   bottom. With no matches a step changes nothing (the bar disables the buttons, `findbar.js`:1113-1119). With one
   match a step stays on it and still reports the wrap: Firefox answers `FIND_WRAPPED` whenever the search went round
   the document, whether or not the selection moved (`Finder.sys.mjs`:278-279, and `findbar.js`:1080-1082 picks the
   message from the direction alone).
8. **`from`.** The current match after a `search` is the first one starting at or after `from`, so a caller that
   passes the start of its current match keeps it while the query grows a letter. Firefox gets the same effect from
   the selection it is sitting on (`Finder.sys.mjs`:211-216); here the caller says where it was, because there is no
   selection to read.
9. **`refresh`.** First match at or after the old current `(block, start)`, else the last match, else nothing. No
   browser does this — see the deviation below.
10. **Counter text.** `findbar.ftl`'s English, exactly, including the `[one]` singular chosen by `$total` and the
    `$limit` form for truncation; Chromium's `"{ordinal}/{total}"` with `0/0` for an empty or fruitless search
    (`text_finder.cc`:417-427, `find_tab_helper.cc`, `find_bar_view.cc`).

## Where the spec and the originals differ, and which one this crate follows

1. **Blank query.** *Spec rule 5 wins.* Firefox has no such rule: `findbar.js`:176-184 calls the engine with whatever
   the field holds, and `nsFind.cpp` has no empty-pattern guard on the whitespace path, so a query of `"  "` really
   does find double spaces on the page. Here, `search` and `find_all` treat a query whose `trim()` is empty as no
   search at all: matches cleared, `current = None`, `wrapped = None`, counter empty (Firefox's own
   `Finder.requestMatchesCount` already reports 0/0 for the empty string, `:612-622`, which is as far as the original
   agrees).
2. **Wrap note "instead of" the counter.** *The original wins*, which is what the spec's rule 10 tells us to do when
   the UI shows both. `findbar.js` has two elements: the wrap message goes to `find-status` (`:59`, filled at
   `:1077-1083`, `:1105-1106`) and the counter stays in `found-matches` (`:57`), which only *not found* and an empty
   count hide (`:1088`, `:1324-1328`). So `counter_text` returns both, counter first, one space between:
   `"3 of 12 matches Reached end of page, continued from top"`. A paraphrase would have been the spec's literal
   reading; the `.ftl` strings are kept verbatim either way.
3. **Whole-word character class vs a word segmenter.** *Spec rule 3 wins*, and the difference is real. Firefox asks
   ICU's `BreakInBetween` (`nsFind.cpp`:634-642) and Chromium asks whether the match is exactly one ICU word
   (`text_searcher_icu.cc`:127-149); the crate's rule is `is_alphanumeric() || '_'` on the neighbour. Divergences this
   produces: a combining mark is not alphanumeric here, so with *Match Diacritics* on, `e\u{301}` cuts a word in
   Firefox's reading but not in ours; UAX #29 keeps `1,000` and `don't` inside one word (comma and apostrophe are
   mid-number/mid-letter, not boundaries) while `,` and `'` are boundaries for us; Chromium waves through any match
   that starts at a CJK ideograph (`:136-138`) and we do not look at scripts at all. `_` agreeing with word-ness is
   the `ExtendNumLet` rule, which both segmenters and this crate share.
4. **The base-letter table's reach.** *Spec rule 2 wins.* Gecko's `GetNaked` table covers all of Unicode and is built
   by `base_chars.py` from canonical decompositions *plus* an ASCII-fallback table (`:71-87`); the crate ships the
   same rule restricted to Latin-1 Supplement and Latin Extended-A, 161 rows, U+00C0-U+017E. Consequences: Greek `ά`
   finds `α` in Firefox and not here; `ø`, `ß`, `æ`, `đ`, `ł` have no base+mark decomposition and stay themselves
   here, and Firefox keeps them as themselves for the same reason (its fallback table only contributes one-character
   mappings, and none of those letters has one in the table `base_chars.py` reads) — the crate's test
   `accents_are_stripped_only_when_asked` pins `ø` ≠ `o`.
5. **Which marks count as combining diacritics.** *Spec rule 2 wins*: the range U+0300-U+036F. Firefox's set is every
   character with a nonzero canonical combining class except 8, 9, 91, 129, 130, 132 (`base_chars.py`:9-27), i.e. it
   also covers Hebrew points, Arabic harakat, Thai vowels and Devanagari matras, and it deliberately excludes the kana
   voicing marks and viramas. Marks outside U+0300-U+036F are ordinary characters here.
6. **How many query spaces a page run needs.** *The original wins*, and rule 4 is compatible with it: one query space
   absorbs a page run of any length (`nsFind.cpp`:1196-1204), and a query with two spaces needs two page characters,
   so `a  b` does not find `a b` while `a b` finds `a   b`. The required test
   `whitespace_runs_match_one_space` is written against that.
7. **`truncated` when the page has exactly `limit` matches.** *Both, deliberately.* `find_all` sets `truncated` only
   when it stopped at the limit; the counter says *More than 1000 matches* whenever `truncated`, which is Firefox's
   visible behaviour because it also folds `total == limit` into "more than" (`Finder.sys.mjs`:597-599). Neither
   browser can tell the caller about a page with exactly 1000 matches, and neither can this crate.
8. **`refresh`'s nearest-match rule.** *Ours, as the spec says (rule 9).* Firefox re-runs the search from the current
   selection and lets it land where it lands (`Finder.sys.mjs`:254-263); Chromium re-searches and picks the match
   nearest the viewport (`text_finder.cc`:437-500), which needs rects. The `(block, start)` rule is the closest a
   text-only crate can get to "keep the user's place", and it is stable: the same page and query reproduce the same
   current match.
9. **Chromium's counter for a truncated search.** The spec says truncation is not shown in Chromium and `0/0` covers
   an empty search; `find_tab_helper.cc`/`find_notification_details.h` confirm the numbers are plain counts with `-1`
   for unknown, so `CounterStyle::Chromium` prints `1000/1000` where the Firefox style prints *More than 1000
   matches*. The original wins; there is no `find_limitExceeded` handling in this style.
10. **`match_diacritics` as an independent option.** *Firefox wins.* Chromium cannot express "case-insensitive but
    accent-sensitive" except behind a runtime feature flag that changes the collation strength to secondary with a
    wildcard pattern weight (`text_searcher_icu.cc`:286-296); Firefox has three separate booleans
    (`nsFind.cpp`'s `mCaseSensitive`, `mMatchDiacritics`, `mEntireWord`) and so does `Options`.
11. **API shape.** The spec's signatures, not the browsers': `find_all` takes a limit instead of reading a pref,
    `search` takes an explicit `from` instead of reading a selection, `State` carries `wrapped` instead of returning a
    result code, and `Pos` names a run index instead of a DOM node. The `CounterStyle` enum exists because two
    wordings were specified; `Firefox` is the one the browser actually shows.

## Not ported, and why

- **Firefox's other "ignore this character" rules.** A `\n` between two CJK characters (`nsFind.cpp`:1042-1051),
  default-ignorable characters such as ZWSP and variation selectors (`:1053-1056`), and the exception that keeps a
  mark after a math or music symbol (`:998-1001`). Each needs a Unicode property table std does not carry
  (Default_Ignorable, CJK classes, Sm/So) — the same reason `is_combining_diacritic` is a range here. None of them
  changes where a Latin word is found; they are listed so a later crate can add them without re-deriving the rules.
- **Firefox's** backward search inside one fragment (`mFindBackward` — direction lives in `Finder::step` here, and the
  ranges are the same), the content-node walker and its point-and-item iterators, `nsFindContentIterator`, sub-frame
  and shadow-tree traversal, the word-boundary cache (`:630-632`), quick find and links-only mode, typeahead find,
  `findEvents`, sounds, the 1-second `_findFailedString` reset, and scroll/highlight painting.
- **Chromium's** quote-mark folding (`unicode_utilities.cc`:75-87), its ICU separator table, the "kana workaround"
  that discards collation-only matches (`text_searcher_icu.cc:204-208, :251-254`), overlapping matches as an option
  (`:182`), search inside collapsed and off-screen content, `async_find_buffer.cc` chunking, mark-all with rects,
  "expand then find", and the per-frame restart in `DoPhaseFindInFrame`.
- **Everything that needs a clock, a file or a tree.** No timing, no I/O, no globals: `find_all` and `Finder` are pure
  functions of their arguments, which is what makes the tests hand-computable.

## Licences

- The crate is MPL-2.0, the same licence as the Firefox sources it ports, so each `.rs` carries the MPL header and no
  additional notice is needed for them. The four user-visible strings are from `findbar.ftl` (MPL-2.0, The Mozilla
  Foundation) and are reproduced verbatim, with the attribution `findbar.ftl` @33abb90 at each definition.
- Chromium is BSD-3-Clause and only its *behaviour* is reflected here: no Chromium table, formula, constant or string
  is reproduced (the one numeric constant in the crate, 1000, comes from Firefox's pref). The spec asks for a `NOTICE`
  when a table is ported; no table was, so the crate has no `NOTICE`. `ch-LICENSE` was read anyway and holds the
  standard "Copyright 2015 The Chromium Authors" line, in case the wiring patch later copies a number from Chromium.
- `BASE_LETTERS` is Unicode data, produced by running `base_chars.py`'s rule (MPL-2.0, same licence as this crate) over
  canonical decompositions from the UCD; the rows are code point pairs, which is also how they would have to be written
  from any source.
- No GPL/AGPL or unlicensed third-party code (uBlock Origin, EasyList, Adblock Plus) was read for this crate, and none
  of it is find-in-page logic.
