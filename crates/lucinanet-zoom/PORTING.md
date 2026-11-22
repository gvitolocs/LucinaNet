# PORTING.md — lucinanet-zoom

Page zoom: the level tables, the Ctrl +/-/0 step rules and the per-site zoom memory. Behaviour ported,
structure Rust-first, nothing pasted but tables of numbers.

Read at the pinned revisions, all of which answered HTTP 200 at those shas:

- Firefox `mozilla-firefox/firefox` @ `33abb900fc1646fef2e5ffed2d6b22e806ae10c1` (MPL-2.0)
- Chromium `chromium/chromium` @ `e9a46735de970a8e31217f2233efa3a47a1c337d` (BSD-3-Clause, see `NOTICE`)

`lucinanet-zoom` is MPL-2.0 like both Firefox and LucinaNet; the Chromium-derived parts are the preset
level table and the host-keyed store rules, and the notice for them is in `NOTICE`.

## Records

| Source file | Verdict | What we took |
|---|---|---|
| Firefox source: `modules/libpref/init/all.js` @33abb90 | adopt | Line 3028, `toolkit.zoomManager.zoomValues` — the Firefox level table, as whole percent (`FIREFOX_LEVELS`, 26 rows, 20 to 500). Numbers only; MPL like ours. |
| Firefox source: `modules/libpref/init/StaticPrefList.yaml` @33abb90 | adopt | `zoom.minPercent` = 20 (line 22022) and `zoom.maxPercent` = 500 (line 22013, 400 under `#ifdef ANDROID`). These are the ends of the range, and they cut nothing off the pref table on desktop. |
| Firefox source: `toolkit/content/viewZoomOverlay.js` @33abb90 | adapt | `ZoomManager`'s rules, rewritten in Rust integer arithmetic: `snap` (the first level at or above the value, back to the predecessor only when that is strictly nearer — so midpoints go **up**), `enlarge`/`reduce` as `zoomValues.indexOf(snap(zoom)) ± 1` with no move past either end, `MIN`/`MAX` as the intersection of the pref table with the two percent prefs, and `setZoomForBrowser` refusing a value outside `MIN..MAX` (which is why `clamp` exists here). `reset` sets 1, which is our `DEFAULT`. The `zoomValues` fallback string in the same file (which starts at `.3`) is only what is used when the pref is missing, so it is not the shipped table. |
| Firefox source: `browser/components/tabbrowser/content/browser-fullZoom.js` @33abb90 | reference | The per-site memory's shape: pref name `browser.content.full-zoom`, gated by `browser.zoom.siteSpecific`, `_ensureValid` clamping a stored value into `MIN..MAX`, `_onContentPrefChanged` applying a changed global value only to sites that have no row, and `reset` removing the site's row (`_removePref`). The rows themselves are grouped by base domain, not host, so they are not the key we use (deviation 2). |
| Chromium `third_party/blink/common/page/page_zoom.cc` @e9a4673 | adopt | `kPresetBrowserZoomFactors` (17 factors) as `CHROMIUM_LEVELS` in whole percent, and `kMinimumBrowserZoomFactor` 0.25 / `kMaximumBrowserZoomFactor` 5.0 as its ends. The 1.2 multiplier of `kTextSizeMultiplierRatio` and the float level conversions are not needed once zoom is whole percent (deviation 8). |
| Chromium `components/zoom/page_zoom.cc` @e9a4673 | adapt | `PageZoom::Zoom`: for a value between levels, the step up is the first preset strictly above the current (`std::upper_bound`), the step down the last strictly below, and nothing happens at either end (`ZoomPlus_AtMaximum` / `ZoomMinus_AtMinimum`). That is `zoom_in`/`zoom_out` for `Profile::Chromium`. `PAGE_ZOOM_RESET` sets the *default* level, not 1.0 (deviation 6). The epsilon of `ZoomValuesEqual` (0.001 in level space, about a tenth of a percent) only matters for float levels, so it cannot decide a step between integer percents and is not carried over. |
| Chromium `content/browser/host_zoom_map_impl.cc` @e9a4673 | adapt | Zoom keyed by host and stored in one map, so a host's level holds across scheme and port (`SetZoomLevelForWebContents` → `SetZoomLevelForHost(net::GetHostOrSpecFromURL(url))`), and `SetZoomLevelForHostInternal` **erases** the host when the new level equals the default — the rule behind `SiteZoom::set`. `ClearZoomLevels` and the temporary/per-frame levels are browsing-context plumbing, not store rules, and are rejected. |

Rust crates: `url` (adopt — host extraction, IDNA and case folding are its job, not ours), `serde` +
`serde_json` (adopt — the store's file form). Servo is not touched: `WebView::set_page_zoom` already
applies a factor, and `factor()` is the bridge to it.

## Where the spec differs from the originals

The original wins; each of these says which way we went.

1. **Firefox's bottom level.** The spec says Firefox steps are "30..500 %". The shipped pref table
   (`all.js:3028`) starts at `.2`, and `zoom.minPercent` is 20, so the trim in the `zoomValues` getter
   removes nothing: **20..500, and we followed the original.** 30 is where the *fallback* string inside
   `viewZoomOverlay.js` starts, which Firefox uses only if the pref is absent. The test
   `firefox_table_is_ascending_and_matches_pref` parses the pref string itself, so a wrong table fails.
2. **The key a site is stored under.** The spec says "keyed by origin". Chromium keys by host
   (`host_zoom_map_impl.cc`), so `http://example.org` and `https://example.org:8443` share a level.
   Firefox keys *coarser still*: content prefs group by base domain, so `www.example.org` shares a row
   with `example.org`. One key rule for both profiles, the **host as the spec says, which is Chromium's**,
   and `site_key` says so: no scheme, no port, no `www.` folding. `site_key_keeps_www_distinct` is
   therefore Chromium's behaviour and deliberately not Firefox's.
3. **Ties in `snap`.** The spec's `snap` sends an exact midpoint to the lower level; Firefox's own `snap`
   keeps the higher one (`aVal - values[i-1] < values[i] - aVal` must be strictly less). Both are kept,
   because they answer different questions: the public `snap` is the toolbar's "where are we" helper and
   follows the spec, while the Firefox steppers call the original's snap internally (`firefox_snap`) since
   the spec says to follow `viewZoomOverlay.js` for `Profile::Firefox`. It is observable — from 115 %
   Firefox's Ctrl + goes to 130 (snap up to 120, then one step), where Chromium goes to 125 — and
   `firefox_step_snaps_midpoints_up` pins it.
4. **Chromium's table is not quite static.** `PresetZoomValues` inserts the page's default level into the
   table when it is not one of the presets, so with a default of 115 % Chromium stops there on the way.
   `levels(profile)` returns a fixed table and cannot express it, so **we don't**: with a default that is
   not a level, expect one more stop in Chromium than here. Nothing else in the step rules depends on it.
5. **Rows that equal the default.** The spec (and Chromium) removes such a row; Firefox at this revision
   writes the current zoom on every step (`_applyZoomToPref` compares nothing) and only drops the row on
   Ctrl 0 (`reset` → `_removePref`). **We followed Chromium**, so `set` at the default level forgets the
   site, which keeps stale 100 % rows out of the file. `set_default` purges nothing, matching both
   browsers: a changed default applies to the sites without a row only.
6. **What Ctrl 0 means.** `reset(profile)` returns 100 as the spec signs it, but neither browser resets to
   100 — Firefox reads the global value (`ZoomUI.getGlobalValue()`), Chromium sets `default_zoom_level`.
   So the wiring must reset through `SiteZoom::default_percent()` and use `reset()` only as the unset
   value.
7. **Pages that remember nothing.** The spec's list — `about:`, `data:`, `file:`, `blob:` and empty hosts,
   all `None` — is ours, not either original's exactly. Firefox has no domain to group those under, so it
   cannot store them; Chromium's `GetHostOrSpecFromURL` falls back to the whole URL spec, so `file://`
   pages do get rows there, keyed by path. **We follow the spec's list**, which is Firefox's intent.
8. **Whole percent, desktop, full-page.** Levels are `u16` percent, so Chromium's factor/level
   conversions (`log`, the 1.2 multiplier) and its `ZoomValuesEqual` epsilon have no counterpart and were
   rejected: the two cannot disagree about which level a whole percent is nearest. Platform limits we do
   not model: Chromium's Android range (0.425..6.0) and Firefox's Android `maxPercent` of 400. And Firefox
   separates text zoom from full zoom (`browser.zoom.full`); LucinaNet has whole-page zoom only, because
   that is what `WebView::set_page_zoom` does.

## Not in this crate

The command itself (`Cmd::Zoom`), the toolbar label beyond `label()`, the profile file beyond
`to_json()`/`from_json()`, and pinch-zoom interaction: the wiring pieces listed in the spec.

## Provable properties, left available

`zoom_in`, `zoom_out`, `snap`, `clamp` and the Firefox snap are plain integer arithmetic on `u16` with
`while` loops over the table — no floats, no iterators — so they can go into the Verus list
(`crates/lucinanet-media/src/rules/*.rs` is the model) without a rewrite. What should hold, and what the
tests already walk: the result of a step is always a level of its own table; `zoom_in` never goes down
and `zoom_out` never goes up; a step from a level is the next level of the table, each level reachable
from `min` in `levels.len() - 1` steps; `clamp` is the identity exactly on `min..=max`; `snap` is inside
the table and `clamp`ed. Verus was **not** run here (optional for this crate, and not run), nor were mutants.
