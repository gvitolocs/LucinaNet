# lucinanet-omnibox: porting record

Per our porting guidelines: the original browsers are the truth for behaviour; this crate copies
behaviour and constants, not structure. Pinned revisions (both are `main` of 2026-11-11):

- Firefox (MPL-2.0): `mozilla-firefox/firefox` @ `33abb900fc1646fef2e5ffed2d6b22e806ae10c1` ("@33abb90" in the code).
- Chromium (BSD-3-Clause): `chromium/chromium` @ `e9a46735de970a8e31217f2233efa3a47a1c337d` ("@e9a4673"). The Chromium
  licence is in `NOTICE`.

All paths below returned HTTP 200 at the pinned revision, except `components/omnibox/browser/history_scoring.cc`, which does not
exist (not needed). Files read: the ones in the table, plus `bookmark_provider.h`, `scoring_functor.{h,cc}`, `search_provider.h`,
`search_suggestion_parser.cc`, `autocomplete_provider.h` (the relevance table in its header comment) and `omnibox_field_trial.cc`.

## Sources

| File (@ pinned revision) | Decision | What was taken |
| --- | --- | --- |
| Chromium `components/omnibox/browser/scored_history_match.cc` | adapt (BSD, see NOTICE) | Topicality: 10/8/5/2 points for host-boundary / title or path / query / host-inside-word matches, raw-score-to-topicality table (linear below 10, `1 + 2.25 log10(0.1 s)` above, cap 29), word-boundary filter with mid-word continuation, first 20 title words, threshold 0.5; recency table by days (100 up to 4 days, 70 at 14, 50 at 31, 30 at 90, 10 at 365, integer arithmetic as in the source); frequency (typed 1.5, at most 10 visits); document specificity table; the final piecewise map `0:550, 1:625, 9:1300, 90:1399`. |
| Chromium `omnibox_field_trial.cc` | adapt | The default values of the field-trial parameters, written next to each constant in `history_score.rs`: HQPBookmarkValue 10, HQPTypedValue 1.5, HQPMaxVisitsToScore 10, HQPAllowMatchInTLD false, HQPAllowMatchInScheme false, HQPNumTitleWordsToAllow 20, topicality threshold 0.5, NumMatchesScores `1:3,2:2.5,3:2,4:1.5`, HQPAlsoDoHUPLikeScoring false. |
| Chromium `bookmark_provider.cc`, `scoring_functor.{h,cc}` | adapt | Bookmark relevance: factor per match = matched length x (length - start) / length, summed over title and URL, divided by (title length + 10), capped at 1, mapped into 900..1199 (bookmarklets 400..799), boosted 0/75/125/150 for 1/2/3/4+ bookmarks of the same URL. |
| Chromium `search_provider.cc`, `search_suggestion_parser.cc` | adapt | Remote suggestion relevance 600 + (results after it) for queries, 300 for URL-like text; 250 for the plain search shown beside a keyword search (`CalculateRelevanceForVerbatim` with a keyword provider). |
| Chromium `history_url_provider.cc` | reference | Read for the what-you-typed (1203) and inline (1413) scores. Not used: the heuristic item here is always first regardless of score (spec rule 3), so those numbers only order items we do not have. |
| Chromium `autocomplete_result.cc` | adapt | `GetMaxMatches`: 8 on desktop (`MAX_SUGGESTIONS`); dedupe by destination, sort by relevance. |
| Chromium `autocomplete_match.cc` | reference | The stripped-URL idea for deduplication (scheme, `www.`, trailing slash). The key here is our own: host without `www.`, port, path, query, fragment, one trailing `/` removed. |
| Chromium `url_prefix.cc` | adapt | The prefix list for inline completion and the rule that the typed text may start right after a prefix. |
| Chromium `autocomplete_input.cc` | adapt | `Parse`: javascript is not a URL; unknown schemes ("site:") are queries; `user@host` is a query; IPv4 needs four components, 2-3 numeric components are a query unless the text ends in `/`; a port or a trailing slash makes a single word an address; `localhost`. |
| Chromium `url_fixer.cc` | adapt | Scheme/separator repair (`;` for `:`, `http:/x`, host:port is not a scheme), file paths from a leading `/`, `view-source:` fixed recursively without recursion. |
| Firefox `docshell/base/URIFixup.sys.mjs` | adapt | The scheme typo table (`ttp htp ttps tps ps htps ile le`), `//host` duff protocol, newline stripping, `localhost`/IP handling, a forced visit by a trailing slash. |
| Firefox `UrlbarProviderHeuristicFallback.sys.mjs` | adapt | What Enter does: URL result unless the text is a search; keyword and `?` restrict handling. |
| Firefox `UrlbarUtils.sys.mjs`, `UrlbarTokenizer.sys.mjs` | reference | Fetched and searched, not read through; nothing taken (the `?` restrict token is taken from the spec text and `autocomplete_input.cc`). |
| Firefox `UrlbarMuxerStandard.sys.mjs` | reference | Fetched; only the `maxRichResults` handling was looked at. Result order and deduplication follow the spec and Chromium's `autocomplete_result.cc`. |
| Firefox `UrlbarProviderAutofill.sys.mjs` | adapt | Origins first, a full URL only when the typed text holds a `/`; no autofill when the text has a space; the typed case is kept and the candidate's case follows; Enter on an autofill navigates to it. |

### Files read for `display.rs` (the unfocused address display)

Two paths the spec names have moved by the pinned revision, and the real ones are cited below instead. Both were
confirmed by a direct fetch, and the file/line numbers here are from those fetches, not from a tree listing (the
recursive tree API truncates and is not authoritative).

- `browser/components/urlbar/UrlbarInput.sys.mjs` no longer exists. It is `browser/components/urlbar/content/UrlbarInputBase.mjs`
  (6 741 lines) with a 12-line `content/UrlbarInput.mjs` subclass.
- `toolkit/modules/URL.sys.mjs` no longer exists (HTTP 404 @33abb90), so there is no `getBaseDomainFromHost` in JS: the formatter
  asks the platform service directly, `Services.eTLD.getBaseDomainFromHost(origin)` (`UrlbarValueFormatter.sys.mjs:490`).

| File (@ pinned revision) | Decision | What was taken |
| --- | --- | --- |
| Chromium `components/omnibox/browser/location_bar_model_impl.cc` | adapt | `GetURLForDisplay` (67-102) is the unfocused display: `kFormatUrlOmitDefaults` plus `kFormatUrlOmitHTTPS` and `kFormatUrlOmitTrivialSubdomains`, and optionally `kFormatUrlTrimAfterHost`. `GetFormattedFullURL` (59-64) is the untrimmed one: `kFormatUrlOmitDefaults` alone. `ShouldPreventElision` (112-115) drops HTTP trimming for people who turn elision off. |
| Chromium `components/url_formatter/url_formatter.h`, `.cc` | adapt | The flag values and what each omits (`url_formatter.h:68-102`, `url_formatter.cc:546-559`): `kFormatUrlOmitDefaults = kFormatUrlOmitUsernamePassword \| kFormatUrlOmitHTTP \| kFormatUrlOmitTrailingSlashOnBareHostname`, and HTTPS/`www.` trimming are deliberately *not* in the defaults. `StripWWW` (817-830) with its comment "Exclude the registry and domain from trivial subdomain stripping" and "If there is no domain and registry ... Leave those alone". IDN: `IDNToUnicodeWithAdjustmentsImpl` (308-347) checks and converts **one label at a time** (`IDNToUnicodeOneComponent`), and `SpoofCheckIDNComponent` (402-408) asks `IDNSpoofChecker::SafeToDisplayAsUnicode(label, tld, tld_unicode)` per label; a label that fails is left in punycode. `FormatViewSourceUrl` (235-258) refuses "destructive elisions" for `view-source:`. |
| Chromium `components/url_formatter/elide_url.cc` | adapt | The order of the elision passes in `ElideUrl`: all of it, then scheme off, then "Subdomain, domain and entire path fits" (196-246); the domain and registry come from `GetDomainAndRegistry(url, EXCLUDE_PRIVATE_REGISTRIES)` and, when there is none (an IP address), the **whole host** becomes the protected part (455-462). When only the domain fits, `…` goes before it: "If a subdomain is present, add an ellipsis before domain", `url_elided_domain = kEllipsisAndSlash[0] + url_domain` (279-286, and 342). The marker is one character (`gfx::kEllipsisUTF16`, used as `kEllipsisAndSlash[0]`, 263-264), and a trivial subdomain means exactly `www.` (`kWwwPrefix`, 473-481). |
| Chromium `chrome/browser/ui/views/omnibox/omnibox_view_views.cc` | reference | `OnBlur` (1978) calls `ElideAndResetScrollPosition` (2017-2038): `SetElideBehavior(gfx::ELIDE_TAIL)` and `SetDisplayOffset(0)`, with the comment that the offset must be set to 0 directly because the text may be bidirectional. This is the "an unfocused field shows the start" rule. `ElideAnimation` is gone; steady-state elision is what the `ShowFullURLs`/unelide paths switch around. |
| Firefox `browser/components/urlbar/content/UrlbarInputBase.mjs` | adapt | `createExposableURI` before showing the value, with the comment "Strip off usernames and passwords for the location bar" (1094-1100): userinfo is never in the display. `losslessDecodeURL` (4255-4258, 6467-6479) with "don't show punycode": the host is shown in its IDN display form. `_on_click` → `#maybeSelectAll` (5189-5198, 5300-5306) selects everything on the click that enters the field, which is what leaves the caret at 0 and the view at the start. |
| Firefox `browser/components/urlbar/UrlbarValueFormatter.sys.mjs` | adapt | The base domain (`Services.eTLD.getBaseDomainFromHost`, 488-502) splits the host into `subDomain` + `baseDomain`; everything before the domain and everything after the host is put in `SELECTION_URLSECONDARY`, i.e. **de-emphasised** (dimmed) while the domain keeps the strong colour (311-314, 472-519), gated on `browser.urlbar.formatting.enabled` (default true, `UrlbarPrefs.sys.mjs:194`). `#getUrlMetaData` returns null while focused (163-166): the metadata, and with it the elision, only applies to the unfocused field. |
| Firefox `UrlbarValueFormatter.sys.mjs` `#scrollHostIntoView` | reference | Replaces the `selectionStart = selectionEnd = 0` / `scrollLeft = 0` the spec names. 110-131: with no host range, `scrollLeft = isRTL ? scrollLeftMax : 0` (the start); with one, it scrolls *minimally* so the host's right edge sits inside the fade. `tests/browser-UrlbarInput/browser_registrableDomainInView.js` guarantees the registrable domain is fully inside the field and clear of both fades, and that nothing more is scrolled than needed. |
| Firefox `browser/modules/BrowserUIUtils.sys.mjs` | adapt | `trimURL` (139-188): one trailing slash off `http(s)/ftp` host-and-slash URLs, then one protocol prefix off the front; `getTrimmedURLPrefix` strips `www.` **only together with** the protocol and only when `trimWww` is on. |
| Firefox `browser/app/profile/firefox.js`, `UrlbarPrefs.sys.mjs` | reference | `trimURLs` true, `trimHttps` false, `trimWww` false (firefox.js:695-697, mirrored in `UrlbarPrefs.sys.mjs:700-708`), but `trimURLProtocol` reads `getScotchBonnetPref("trimHttps")` (`BrowserUIUtils.sys.mjs:144-148`) and `scotchBonnet.enableOverride` is true (firefox.js:511) — so the current Firefox default trims **https**, not http. |

## Choices where the spec text and the originals differ (the original wins unless stated)

1. **Prefix list.** The spec names `https://www.`, `https://`, `http://www.`, `http://`, `www.`. `url_prefix.cc` has `http://www.`,
   `https://www.`, `ftp://www.`, `http://`, `https://`, `ftp://` and the empty prefix. Followed the source; `www.` typed by the
   person is covered by the `https://` prefix followed by their `www.`.
2. **`MAX_SUGGESTIONS`.** Chromium desktop shows 8, Firefox `browser.urlbar.maxRichResults` is 10 (`UrlbarPrefs.sys.mjs`). Kept 8: the scores are Chromium's, and 8 is the spec's own value.
3. **Trailing dot.** Spec rule 1(e) says `example.com.` is not a host (test `trailing_dot_and_double_dot_are_not_hosts`).
   Chromium navigates there (`FixupHost` in `url_fixer.cc` keeps one trailing dot); Firefox's behaviour was not checked. **Followed the spec** because a required
   test name fixes it and "hello." at the end of a sentence is more often a search; `..`, a leading dot and a space after a dot are
   searches in the originals too.
4. **Known TLD.** Chromium `Parse` needs a host with a known public suffix (or a port, a trailing slash, localhost); Firefox asks the
   public suffix list too. This crate has no list (no dependency allowed), so the last label must be two or more letters
   (or a `xn--` label). `3.14`, `file.v2` are searches; `readme.zzz` navigates where Chromium would ask. Spec rule 1(e) says the same.
5. **Multiple terms and the word-boundary rule.** The spec says every term must match at a word boundary. The source only requires
   that each term matches somewhere in URL or title and that the average topicality reaches 0.5: with two terms, one strong
   (1.0) and one filtered out (0), the average is exactly 0.5 and the page still scores. Followed the source.
6. **Heuristic for address-like text.** `Navigate` shows the URL (`text` = the URL we would open). For a `Search`, the title is
   "terms -- search" (em dash) as in the spec; for a keyword search the engine's name replaces "search".
7. **Remote suggestions.** Shown when the heuristic is a search (base 600) and for an address-like input that has a space (base
   300, Chromium's URL base); not for plain address-like input and not for keyword searches (they come from the default engine, not
   the keyword's). Chromium lists them for URLs too, below history; the spec asks for the choice to be written here: chosen as above.
   Capped under the weakest history/bookmark item that starts like the typed text and kept strictly decreasing, so the engine's order
   survives.
8. **`Kind::KeywordSearch`.** Used for the one extra row Chromium shows beside a keyword search: the default-engine search of the whole
   typed text (score 250). The spec leaves the variant unused otherwise.
9. **Scheme typos.** Firefox repairs the typo names (`ttp`, `ps`, `le` ...) even with a lone colon (`ps:aux` becomes `https://aux`). Here they
   need `//` after them, because those are real search words ("le:", "ps:") and the person still sees the result in the bar.
10. **Inline completion rank.** Firefox ranks origins by frecency; the frecency crate does not exist here. Rank = sum over the origin's
    pages of the Chromium frequency (visits weighted by recency, bookmark = 10), then shorter text, then alphabetical. For a full URL
    the best page's own frequency.
11. **Bookmarks of the same URL** are counted by identical `url` strings.
12. **`http://` stays, `https://` goes** (display rule a). Both browsers do the opposite of each part of this: Chromium adds
    `kFormatUrlOmitHTTPS` to the display format while `kFormatUrlOmitHTTP` means http is never printed (`location_bar_model_impl.cc:74-86`,
    `url_formatter.cc:546-559`), and Firefox reads the scotchBonnet `trimHttps` pref, so today's default trims https too
    (`BrowserUIUtils.sys.mjs:144-148`, `firefox.js:511`). Keeping `http://` as the insecure marker is **deliberately stricter than both**:
    this UI has no lock icon yet, so the only place the scheme can be told is the text. Every other scheme is printed in full: the only
    others Chromium elides are `file:` and `mailto:`, each outside the defaults and each with a UI element in its place
    (`url_formatter.h:90-95`, `location_bar_model_impl.cc:85-88` — "the File chip makes the scheme redundant"). Firefox prints
    `file:///tmp/x` with its protocol (`trimURL` knows only http(s) and ftp).
13. **A trivial subdomain is exactly `www.`, and only when it is not the domain** (rule b). `elide_url.cc:473-481` (`kWwwPrefix`) and
    `StripWWW`'s "Exclude the registry and domain from trivial subdomain stripping" / "If there is no domain and registry ... leave those
    alone" (`url_formatter.cc:817-830`) are followed exactly: `www.example.com` gives `example.com`, `www.co.uk` and `www.com` keep their
    labels, and `a.www.example.com` keeps `a.www.` as the subdomain. Whether `www.` is outside the domain is a question about the domain,
    so it is answered only when the caller passed one: with no registrable domain the prefix stays, which is `StripWWW`'s "no domain and
    registry ... leave those alone" branch. Firefox strips `www.` only together with the protocol and only with
    `trimWww` on, which is off by default (`BrowserUIUtils.sys.mjs:161-166`) — the spec asks for it, and Chromium does it, so it is done.
14. **Registrable domain: an argument, not a guess.** Neither browser has a hand-written subset: Chromium asks `GetDomainAndRegistry(url,
    EXCLUDE_PRIVATE_REGISTRIES)` (`elide_url.cc:458-461`) and Firefox `Services.eTLD.getBaseDomainFromHost`
    (`UrlbarValueFormatter.sys.mjs:490`) — both ask the full public suffix list. The first cut of this file stood that up with an
    embedded list of some 190 multi-label suffixes plus a last-two-labels fallback. **Rejected now** (project lead decision, 2026-11-12):
    a partial list shows the suffix *itself* as "the domain" in exactly the cases where it matters — `me.github.io` read `github.io`,
    `shop.example.co.jp` read `co.jp`, a `*.pages.dev` site read `pages.dev` — and the fallback gives that same wrong answer for every
    zone the list did not carry, while naming `example.com` for a host nobody can place. So the caller passes the registrable domain as
    `unfocused_display`'s second argument, computed from the list the engine already embeds: `net_traits::pub_domains::reg_suffix`
    (`components/shared/net/pub_domains.rs:147` of the pinned Servo fork, whose data resource `Resource::DomainList` is
    `components/default-resources/resources/public_domains.txt`). This crate embeds no list at all and its tests feed fixed values.
    A passed value is used only where it is a suffix of the host at a label boundary; where the caller gives none, or one that does not
    fit this host, the **whole host** becomes the protected and highlighted part, which is Chromium's shape for "no domain and registry"
    (455-462) and never a guess at one.
15. **IDN: one script per label** (rule d). Chromium converts and checks **one label at a time** (`IDNToUnicodeWithAdjustmentsImpl`
    308-347, `SpoofCheckIDNComponent` 402-408) and Firefox shows the display form with "don't show punycode"
    (`UrlbarInputBase.mjs:4255-4258`); without `idna`/a spoof checker the same granularity is kept with a script test instead of
    confusables: a label in a single script is shown in Unicode, a mixed-script label stays punycode. The Unicode form is recovered from
    the typed text and only used after re-parsing it gives back the same punycode host, so typed punycode stays punycode and nothing that
    the parser folded (fullwidth, upper case) is resurrected.
16. **The marker and where it goes** (rule e). `…` before the domain is Chromium's, verbatim in intent: "If a subdomain is present, add an
    ellipsis before domain", `url_elided_domain = kEllipsisAndSlash[0] + url_domain` (`elide_url.cc:279-286,342`), one character
    (`kEllipsisAndSlash[0]`, 263-264) and never three dots. When even the domain does not fit, the field shows its start and the tail
    marker, which is the `gfx::ELIDE_TAIL` shape `omnibox_view_views.cc:2017-2038` uses.
17. **Port belongs to the domain.** `url_formatter` puts the port after the host, so `domain` here is the highlighted part **without**
    it (the range stops at `Position::AfterHost`), but elision counts host and port as one unit: `example.co.uk:8443` is never cut in the
    middle of that.
18. **The path and the query are kept** (rule c) until they do not fit. Chromium can drop everything after the host
    (`kFormatUrlTrimAfterHost`), but only when its delegate asks for it, and always on iOS (`location_bar_model_impl.cc:76-80`) — not
    ported: the spec asks for the tail, and the bug this feature fixes was a query string hiding the site name, not a query string being
    too long. An IP address gets no special treatment either: `elide_url.cc:455-462` makes the whole host the protected part there, and
    so does this — the literal is shown in full, `domain` is the range of that whole host (a passed value is ignored, an address has no
    registrable domain), and only tail elision applies.
19. **No userinfo, ever** (rule f). Firefox strips it before display, comment "Strip off usernames and passwords for the location bar"
    (`UrlbarInputBase.mjs:1094-1100`); Chromium sets `kFormatUrlOmitUsernamePassword` in `kFormatUrlOmitDefaults`. The trick case
    (`https://accounts.google.com@evil.example/`) shows `evil.example` because the parser, not this crate, decides what the host is —
    the same reason `path_containing_other_host_name` is safe.
20. **Host-less and unparseable values.** `about:blank`, `data:...`, `file:///path` and `view-source:...` have no host to align on, so the
    serialization is shown as it is and only the length limit applies; `FormatViewSourceUrl` refuses "destructive elisions" for
    `view-source:` for the same reason (`url_formatter.cc:235-258`). Unparseable input (rule g) is returned as typed, truncated: the
    originals never see such a string, so there is nothing to copy, and inventing a layout for text we cannot parse is the unsafe choice.

## Simplifications (and why)

- **Visit list.** `ScoredHistoryMatch` receives each visit with its time and transition. `HistoryEntry` has a count, a typed count and
  the last visit time. All counted visits (at most 10) get the weight of the last visit, and the first `typed_count` of them are
  typed. This overstates the frequency of old, busy pages a little.
- **Document specificity.** Chromium counts the pages the in-memory index returns for the typed words. `history_score` uses 1.0 (its
  "many pages" value); `suggest` counts the history entries with positive topicality and passes that to `history_score_among`.
- **Word boundaries.** Chromium splits words with ICU; here a word is a run of alphanumeric characters (Unicode). Case folding maps a
  character to its lowercase when that is one character.
- **Matching text.** The cleaned URL is: `http(s)://` and `www.` dropped (other schemes kept and never score), the slash of a bare host
  dropped, non-ASCII percent escapes decoded. IDN hosts stay in punycode (no `idna` dependency), so typed `münchen` does not match
  `xn--mnchen-3ya.de`; the query/fragment split is the first `?` or `#`.
- **HUP-like scoring** (`HQPAlsoDoHUPLikeScoring`) is off by default and not ported. The HQP bookmark value (10 per visit) is not used for
  the score: a bookmark is scored by the bookmark rule and merged with the history score by `max`, as the spec says.
- **Bookmark matching.** Chromium's `query_parser` rules for short input (exact word match for all but the last term below 3 characters) are
  replaced by "each typed word starts a word in the title or in the URL without scheme and `www.`"; overlapping matches are only
  deduplicated when identical.
- **File paths.** `~` needs a home directory, which is unknown here, so `~/x` is a search. Paths are not checked against the file system.
- **Display: characters, not pixels.** Chromium measures the text with a font and `gfx::ElideText`, and Firefox reads the rendered host
  range; `max_chars` is the crate's own scalar-value budget, so the cut is exact here and approximate in the browsers. The domain is
  returned as a range and `elided` as a flag so a real field can dim it and add its own fades, the way `SELECTION_URLSECONDARY` and the
  two fades do over there.
- **Display: no percent-decoding** of the path or query. The browsers ask for `base::UnescapeRule::SPACES`
  (`elide_url.cc:150,430,444`; `url_formatter.h:108-109` says use `SPACES` "if you can handle spaces"), so `%20` becomes a real space
  there; here nothing is unescaped, `%2F` stays visible and the byte offsets of the input stay the offsets in the text.
- **Display: no bidi.** Firefox refuses the trim when the result would change direction or the protocol is shown struck out
  (`UrlbarInputBase.mjs:4025-4038`, "Only trim value if the directionality doesn't change to RTL and we're not showing a strikeout https
  protocol" — the strikeout is how it marks http today, the same argument as choice 12 from the other side), and Chromium's
  `ElideAndResetScrollPosition` (2017-2038) notes that with bidirectional text "the start of the text may not be at the left edge".
  There is no direction detection here: the field shows the start, which is what both browsers do for left-to-right text, and an rtl host
  is simply shown from its first character.
- **Display: `focused_scroll` is the caret rule only.** A real field scrolls the minimum that keeps the caret inside the viewport with
  both fades in mind (`#scrollHostIntoView`); this returns the smallest offset that puts the caret in view, with no fade allowance and no
  knowledge of line breaks.
- **No Verus, no mutants** (the spec says none).

## Tests

Expected values in the tests were worked out by hand from the formulas (the arithmetic is in a comment next to each assert). The exact
values asserted: 625 (one visit today, host-boundary match), 962 (5 visits), 1173 (5 typed visits), 564 (100 days old), 793 (one matching
page), 587 (query match), 1317 (10 typed visits with title), 999 and 1074 (bookmark relevance), 592 and 591/590 (remote cap).

`display.rs` asserts whole strings, worked out from the layout branches in `fit`, and passes the registrable domain as the
caller would: fixed values (`Some("example.co.uk")`, `Some("me.github.io")`, `Some("luke.pages.dev")`, the punycode of an IDN host),
`None` where the list has no answer, and values that are not the host's, which must be ignored. `max_chars_smaller_than_host` walks
the field from 18 characters down to 8 on one URL and pins `…box.example.co.uk`, `…example.co.uk`, `example.co.uk`, `example.co.uk…`
(17, http: the scheme marker gives way) and `example…` (8, the marker wins against the scheme and loses);
`userinfo_trick_with_given_domain` pins the byte range, `3..15` for `…evil.example`, IDN tests pin the display form (`münchen.de`,
`παράδειγμα.gr/%CE%B1`) or the punycode one (`xn--pple-43d.com`).
`never_splits_a_code_point` and `elided_text_at_most_max_chars` loop over a 17-URL corpus at every width from 0 to 80 and check the length
budget, that the text is a subsequence of the input (nothing is reordered or invented) and that the cut lands on a character boundary.
