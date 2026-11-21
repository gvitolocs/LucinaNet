# lucinanet-historydb porting record

Firefox sources, MPL-2.0 (same licence as LucinaNet), read at `mozilla-firefox/firefox` @ `33abb900fc1646fef2e5ffed2d6b22e806ae10c1`. All paths existed at the pinned sha.

| File | Decision | Taken |
| --- | --- | --- |
| `toolkit/components/places/SQLFunctions.cpp` | adapt | `CalculateFrecencyFunction::BuildFrecencySQL`: sampling, weights, half-life decay, date-like result. The SQL text is not copied. `CalculateAltFrecencyFunction` rejected (feature gated, off). |
| `modules/libpref/init/StaticPrefList.yaml` | adopt | `places.frecency.pages.*`: half-life 30, 10 sampled visits, weights 200/100/50/20. `.interactions.*` and `.alternative.*` ignored (no view time is recorded; `interesting` is the caller's flag). |
| `toolkit/components/places/nsPlacesTriggers.h` | adapt | `VISIT_COUNT_INC`: Embed, Download, FramedLink, Reload do not count. Redirect types do count. On removal the count falls by the removed countable visits. |
| `toolkit/components/places/Helpers.cpp` | adapt | `GetHiddenState`: hidden = framed link, embed or redirect source. |
| `toolkit/components/places/History.cpp` | adapt | typed flag, unhide when a non-hidden visit arrives, no URLs with userpass stored. |
| `docshell/base/BaseHistory.cpp` | adapt | `CanStore`: disallowed scheme list, URL length cap 2000 (`browser.history.maxUrlLength`). |
| `toolkit/components/places/nsNavHistory.cpp` | reference | `CanAddURI` only delegates to `BaseHistory::CanStore`. |
| `toolkit/components/places/nsINavHistoryService.idl` | adopt | TRANSITION_* codes 1..9, VISIT_SOURCE_* codes. |
| `browser/components/urlbar/UrlbarProviderPlaces.sys.mjs` | adapt | Default match behaviour is `MATCH_BOUNDARY`, falling back to `MATCH_ANYWHERE` when too few results. There is no `browser.urlbar.matchBehavior` pref at this sha. |
| `browser/components/urlbar/UrlbarTokenizer.sys.mjs` | adapt | Tokens split on whitespace. Restriction tokens are the omnibox's concern. |
| `Database.cpp`, `PlacesFrecencyRecalculator.sys.mjs` | reference | Schema meaning only; we recompute on every `record_visit`/`recompute_frecency`/removal. |

## Where the spec and the original differ (the original wins)

1. **Source codes.** The spec says 1 = search, 3 = sponsored. `nsINavHistoryService.idl` says SPONSORED = 1, BOOKMARKED = 2, SEARCHED = 3. JSON stores the original codes; the `Source` enum keeps the spec's variant names. In the frecency SQL `source NOT IN (1, 3)` therefore excludes sponsored and search from the typed boost, and `source <> 1` excludes only sponsored from the medium/high weight (spec said sponsored for both; matches).
2. **Hidden.** The spec's doc comment lists downloads as hidden. `GetHiddenState` hides only framed links, embeds and redirect sources, so downloads are visible.
3. **Redirect source.** A page becomes a redirect source (hidden, low weight) when a stored Redirect* visit names one of its visits in `from`; Firefox gets this from the `REDIRECT_SOURCE` flag at visit time. `record_visit` therefore never passes `is_redirect = true`; the wiring may call `recompute_frecency(.., is_redirect = true)`.
4. **Cross-page lookups.** In SQL the `from` visit and the redirect child live on other pages. `frecency()` looks them up inside the slice it is given; the database looks them up across pages.
5. **`place:` URLs** score 0 (`url_frecency`) and are not stored (spec rule 1); embed visits are stored here (Firefox only notifies), as the spec requires.
6. **`typed_count`** is a count here (Firefox keeps a flag) and is not reduced by `remove_range`; `last_visit_us` is recomputed from the surviving visits as the delete trigger does.
7. **Boundary** matching uses "start or after a non-alphanumeric char" per the spec, not Firefox's SQL `AUTOCOMPLETE_MATCH` word-boundary function.
