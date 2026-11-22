# Porting notes — `lucinanet-settings`

Typed settings schema, the defaults we ship, the JSON file format, the version chain,
and what changed between two settings. Nothing in this crate reads a file, the
environment, the clock or the network; the wiring owns `prefs.json` in the profile
directory and this crate only moves between a string and a [`Settings`].

Sources were fetched with `curl` into a scratch directory outside this repo,
never into the repo:

| Scratch file | Upstream path | Revision | Licence |
| --- | --- | --- | --- |
| `ff-firefox.js` | `browser/app/profile/firefox.js` | `@33abb90` | MPL-2.0 |
| `ff-all.js` | `modules/libpref/init/all.js` | `@33abb90` | MPL-2.0 |
| `ff-StaticPrefList.yaml` | `modules/libpref/init/StaticPrefList.yaml` | `@33abb90` | MPL-2.0 |
| `ch-pref_names.h` | `chrome/common/pref_names.h` | `@e9a4673` | BSD-3 |
| `ch-browser_prefs.cc` | `chrome/browser/prefs/browser_prefs.cc` | `@e9a4673` | BSD-3 |
| `ch-download_prefs.cc` | `chrome/browser/download/download_prefs.cc` | `@e9a4673` | BSD-3 |
| `ch-bookmark_pref_names.h` | `components/bookmarks/common/bookmark_pref_names.h` | `@e9a4673` | BSD-3 |
| `ch-session_startup_pref.cc` | `chrome/browser/prefs/session_startup_pref.cc` | `@e9a4673` | BSD-3 |
| `ch-pref_registry_simple.h` | `components/prefs/pref_registry_simple.h` | `@e9a4673` | BSD-3 |
| `ch-page_zoom.h` | `third_party/blink/public/common/page/page_zoom.h` | `@e9a4673` | BSD-3 |
| `ch-page_zoom.cc` | `third_party/blink/common/page/page_zoom.cc` | `@e9a4673` | BSD-3 |
| `ch-page_zoom_constants.h` | `components/zoom/page_zoom_constants.h` | `@e9a4673` | BSD-3 |

The paths in this table are the ones each scratch file was checked against: `curl` of the file at
`https://raw.githubusercontent.com/chromium/chromium/e9a46735de970a8e31217f2233efa3a47a1c337d/<path>` returns the
same bytes as the copy in `src/st/` (compared with `diff`), and the Firefox paths the same way against
`https://raw.githubusercontent.com/mozilla-firefox/firefox/33abb900fc1646fef2e5ffed2d6b22e806ae10c1/<path>`.

An earlier draft of this table named
`components/download/internal/common/download_prefs.cc`, `chrome/browser/android/preferences/browser_prefs.cc`,
`chrome/browser/session_startup_pref.cc`, `chrome/browser/bookmarks/bookmark_pref_names.h`,
`components/prefs/content/public/browser/pref_registry_simple.h` and `third_party/blink/common/page/page_zoom.h`:
those are wrong (404 at that revision, or another file's guard), and the rows above are the corrected paths.

### Reading a cite in the source

`src/*.rs` writes a source as `(@firefox.js:340)`. The short name stands for the full path at the pinned revision,
in the form the rules ask for:

| Short cite | Full source |
| --- | --- |
| `@firefox.js:N` | Firefox source: `browser/app/profile/firefox.js` @33abb90, line N |
| `@all.js:N` | Firefox source: `modules/libpref/init/all.js` @33abb90, line N |
| `@StaticPrefList.yaml:N` | Firefox source: `modules/libpref/init/StaticPrefList.yaml` @33abb90, line N |
| `@download_prefs.cc:N` | Chromium `chrome/browser/download/download_prefs.cc` @e9a4673, line N |
| `@page_zoom.cc:N` | Chromium `third_party/blink/common/page/page_zoom.cc` @e9a4673, line N |
| `@page_zoom.h:N` | Chromium `third_party/blink/public/common/page/page_zoom.h` @e9a4673, line N |

Every line number in the source and in this file was checked against the fetched copy with `sed -n 'Np'` — the
values quoted are what those lines say.

Firefox's values are the defaults. Chromium was read only to check that a value means
the same thing there; no Chromium text is in this crate, so no NOTICE entry is needed.

## Table: setting → pref → value → source

The value column is the value the original ships and the value `src/schema.rs` uses, except in the rows marked
*LucinaNet*, where the row says what we chose and why. `PORTING.md` deviations are listed after the table.

| Setting (field) | Pref name | Value | Source file:line |
| --- | --- | --- | --- |
| `general.startup` = `home_page` | `browser.startup.page` | 1 (homepage); **LucinaNet**: the preference is stored but startup does not read it yet, a window with no address starts on `about:blank` (`start_url`, `crates/lucinanet-ui/src/address.rs`) | `firefox.js:340` |
| `general.home_page` | `browser.startup.homepage` | Firefox `"about:home"`; **LucinaNet** `"about:blank"` | `firefox.js:341` |
| `general.accept_languages` | `intl.accept_languages` | static value `"und"`, read through `Services.locale.acceptLanguages`; **LucinaNet** ships the en-US expansion `["en-US","en"]` | `all.js:1457-1459` |
| `search.default_engine` = `duckduckgo` | — | **LucinaNet**: the DuckDuckGo endpoint built in `crates/lucinanet-settings/src/schema.rs` (`Engine::builtin`), the only place the code names an engine | this crate |
| `search.suggest_remote` | `browser.search.suggest.enabled` (+ `browser.urlbar.suggest.searches`) | **LucinaNet**: false (Firefox: true; deviation 1) | `all.js:3441`, `firefox.js:500` |
| `search.suggest_history` | `browser.urlbar.suggest.history` | true | `firefox.js:497` |
| `search.suggest_bookmarks` | `browser.urlbar.suggest.bookmark` | true | `firefox.js:495` |
| `privacy.do_not_track` = false | `privacy.donottrackheader.enabled` | false | `StaticPrefList.yaml:18630` |
| `privacy.cookies` = `block_third_party_trackers` | `network.cookie.cookieBehavior` | 5 | `firefox.js:2703` |
| `privacy.content_blocking` = false | — | **LucinaNet**: no blocklist ships, so there is nothing to apply. Firefox's `permissions.policy.*` and uBlock's switch are not read here; there is no privacy spec to point at | this crate |
| `privacy.clear_on_exit.*` = false | `privacy.sanitize.sanitizeOnShutdown` | false | `StaticPrefList.yaml:18905` |
| `downloads.directory` = null | `browser.download.dir` | no directory shipped; the wiring resolves one | the downloads spec, step 4 |
| `downloads.ask_where` = false | `browser.download.useDownloadDir` | true (take the folder, do not ask); Chromium: `download.prompt_for_download` false | `firefox.js:936`, `download_prefs.cc:356` |
| `appearance.theme` = `auto` | `layout.css.prefers-color-scheme.content-override` | 2 = auto (system) | `StaticPrefList.yaml:11534-11539` |
| `appearance.default_zoom_percent` = 100 | the `1` step of `toolkit.zoomManager.zoomValues`; Chromium's `kPresetBrowserZoomFactors` holds the same `1.0` | 100 % | `all.js:3028`, `page_zoom.cc:13-17` |
| `appearance.minimum_font_size` = 0 | `font.minimum-size.*` | 0 for all 33 script entries (`ar` at `:1899` through `x-math`); only `th` is raised to 10 or 13 in three locale blocks (`:2178`, `:2457`, `:2709`) | `all.js:1899` |
| `appearance.show_bookmarks_bar` = `new_tab_only` | `browser.toolbars.bookmarks.visibility` | `"newtab"` | `firefox.js:3192` |
| `tabs.open_links_in_new_tab` = true | `browser.link.open_newwindow` | 3 = new tab | `firefox.js:1110` |
| `tabs.confirm_close_multiple` = false | `browser.tabs.warnOnClose` | false | `firefox.js:1153` |
| `tabs.new_tab_page` = `blank` | — | **LucinaNet**: no new-tab page exists to show | this crate |

### The zoom range

`appearance.default_zoom_percent` is clamped to **20..=500**, and the clamp is written at
`src/schema.rs:ZOOM_MIN`/`ZOOM_MAX`.

| Source | What it gives | Line |
| --- | --- | --- |
| Firefox `toolkit.zoomManager.zoomValues` | `.2,.3,.4,.5,.6,.7,.8,.9,1,1.1,1.2,1.3,1.4,1.5,1.6,1.7,1.8,1.9,2,2.2,2.4,2.6,2.8,3,4,5` — the steps a zoom control walks: 20 % to 500 % | `all.js:3028` |
| Firefox's zoom UI code | **not reachable at this revision.** The spec names `toolkit/modules/ZoomManager.jsm`; that path and `toolkit/components/zoom/ZoomManager.jsm`, `toolkit/components/zoom/content/ZoomManager.sys.mjs`, `browser/extensions/zoom/ZoomManager.jsm`, `toolkit/modules/ZoomUI.jsm` and `toolkit/browser/views/controls/zoom/main.js` all return 404 at `33abb900fc16…`. What the menu offers is the pref above, so the pref is what is cited, and nothing about a menu range is claimed. | — |
| Chromium `kMinimumBrowserZoomFactor`, `kMaximumBrowserZoomFactor` | 0.25 and 5.0 on desktop; 0.425 and 6.0 on Android, where the comment says the wider end absorbs the OS font scale | `third_party/blink/common/page/page_zoom.cc:25-26`, `:28-33` |
| Chromium `kPresetBrowserZoomFactors` | 0.25, 1/3, 0.5, 2/3, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0 | `third_party/blink/common/page/page_zoom.cc:13-17` |

| Firefox `zoom.minPercent`, `zoom.maxPercent` | 20 and 500 (400 under `#ifdef ANDROID`): the ends of the range the zoom manager accepts; they cut nothing off the desktop table above | `StaticPrefList.yaml:22022`, `:22013` |

**Which end was used and why.** The clamp is **20..=500**, Firefox's own table ends, the same ends `lucinanet-zoom`
uses (see `crates/lucinanet-zoom/PORTING.md`, deviation 1), so a stored default is always a level its stepper can
reach.

* **Bottom, 20.** The first row of `zoomValues` (`.2`) and `zoom.minPercent` = 20. The spec said 30, "the union of the
  browsers' table ends", because Firefox's menu was believed to start there; 30 is only the start of the fallback
  string inside `viewZoomOverlay.js`, used when the pref is missing, so the original says 20 and the original wins.
  Chromium's desktop floor is 25 % (`page_zoom.cc:25`), so a stored 20 is below what Chromium would honour; that is
  accepted because Firefox is the source for defaults and the wiring clamps again to whatever profile is active.
* **Top, 500.** Where both tables end: `5` in Firefox's `zoomValues`, `kMaximumBrowserZoomFactor = 5.0` in
  Chromium's desktop build. Chromium's Android 6.0 is deliberately not taken — it exists to absorb the OS font scale
  (`page_zoom.cc:28-31`), which a page that stores one integer percentage does not model.

The warning the clamp writes (`Clamped`, with the value it found) is what tells the person their file said something odd.

## Per-file record

| File | Kind | Licence | What was taken |
| --- | --- | --- | --- |
| Firefox source: `browser/app/profile/firefox.js` | **adopt** (values) | MPL-2.0 | The shipped values behind startup, homepage, urlbar suggestions, download behaviour, cookie behaviour, bookmarks-bar visibility, new-window behaviour and `warnOnClose`. Numbers and strings only — no code, no comment text — with `file:line` next to each use. |
| Firefox source: `modules/libpref/init/all.js` | **adopt** (values) | MPL-2.0 | `intl.accept_languages` and its comment's meaning (the pref is a placeholder, the locale supplies the value), `font.minimum-size.*` 0, `browser.search.suggest.enabled`, the zoom table. |
| Firefox source: `modules/libpref/init/StaticPrefList.yaml` | **adopt** (values) | MPL-2.0 | `privacy.donottrackheader.enabled` false, `privacy.sanitize.sanitizeOnShutdown` false, `layout.css.prefers-color-scheme.content-override` 2. |
| `components/bookmarks/common/bookmark_pref_names.h` | **reference** | BSD-3 | The name `kShowBookmarkBar` = `"bookmark_bar.show_on_all_tabs"` (`:41`), to check that a bookmarks bar is a boolean there and a three-way choice here. No value taken. |
| `chrome/browser/prefs/session_startup_pref.cc` | **reference** | BSD-3 | The startup choices (homepage / continue where you left off / new tab page) to check `Startup` covers the same ground. No value taken. |
| `chrome/browser/prefs/browser_prefs.cc`, `chrome/common/pref_names.h`, `components/prefs/pref_registry_simple.h` | **reference** | BSD-3 | How a default is registered, to check that a false boolean there means "off in a fresh profile". No text taken. |
| `chrome/browser/download/download_prefs.cc` | **reference** | BSD-3 | `prefs::kPromptForDownload` registered false (`:355-357`), cross-check for `downloads.ask_where`. |
| `third_party/blink/public/common/page/page_zoom.h` | **reference** | BSD-3 | The *names* of the zoom bounds (`:21-23`); the file declares them `extern`, so the numbers are in the `.cc` below. |
| `third_party/blink/common/page/page_zoom.cc` | **reference** | BSD-3 | The zoom bounds 0.25/5.0 desktop and 0.425/6.0 Android (`:25-26`, `:32-33`) and the preset factor list (`:13-17`), used only to check the range in "the zoom range" above. No list copied: `ZOOM_MIN`/`ZOOM_MAX` are two numbers of our own. |
| `components/zoom/page_zoom_constants.h` | **reference** | BSD-3 | That the preset list reaches the UI through `GetPresetZoomFactorsAsJSON()` and nothing else — the header holds that one declaration, so the numbers come from `page_zoom.cc` above. `components/zoom/page_zoom_constants.cc` was checked only to exist at this revision (`curl` 200), not read. |
| uBlock Origin, EasyList, Adblock Plus | **reject** | GPL-2.0 / AGPL-3.0 / (list licences) | Nothing. `privacy.content_blocking` is a boolean we ship off; no list text, no filter syntax, no code. |
| Firefox `Sanitizer.jsm`, `nsICookiePermission`, permission manager | **reject** | MPL-2.0 | The per-site permission model and the sanitize *dialog* shape: this crate stores four booleans and nothing else; those belong to the wiring. |
| Chromium `PrefChangeRegistrar`, `PrefRegistrySyncable` | **reject** | BSD-3 | Listener registration and sync metadata. [`Settings::changed_keys`] is the only diff mechanism here, and the caller owns what listens. |

## Deviations from the spec, and from the original

The rule is that the original wins and the difference is written down. These are those differences.

1. **`search.suggest_remote` is `false`; Firefox ships `true`.** Firefox has `browser.search.suggest.enabled` true
   (`all.js:3441`) with `browser.urlbar.suggest.searches` true behind the same checkbox (`firefox.js:500`). LucinaNet
   follows the rule that no request leaves the machine that the person did not cause, and a request to the
   engine on every keystroke is one. An explicit `true` in a settings file still loads.
2. **`tabs.confirm_close_multiple` is `false`, not `true`.** Same reasoning: `browser.tabs.warnOnClose` is false
   (`firefox.js:1153`). Chromium warns; Firefox is the source, and the difference is also in the table above.
3. **`general.home_page` is `"about:blank"`, Firefox's is `"about:home"`** (`firefox.js:341`). The spec names this
   as a LucinaNet choice (there is no branded start page to show), so it is followed, and the original's value is
   recorded in the table.
4. **The zoom clamp is 20..=500, not the spec's 30..=500.** The spec's bottom came from reading Firefox's menu as
   starting at 30; the shipped table and `zoom.minPercent` start at 20 (`all.js:3028`, `StaticPrefList.yaml:22022`),
   and `lucinanet-zoom` uses 20..500 at the same revision. The original wins; decided above under "the zoom range".
5. **An earlier draft of these notes said Chromium's zoom bounds were unreachable at `@e9a4673`** because
   `third_party/blink/common/page/page_zoom.cc` had been fetched under the wrong path and came back 404. The file is
   there; `curl` of the path in the table above returns it, and its numbers (0.25/5.0, 0.425/6.0) are now what the
   zoom section cites. The claim was wrong and is corrected here rather than left standing.
6. **`toolkit/modules/ZoomManager.jsm`, the file the spec names for the zoom table, is not in this revision.** Six
   candidate paths 404 (listed in "the zoom range"). The same fact — the list of steps a zoom control walks — is in
   the `toolkit.zoomManager.zoomValues` pref at `all.js:3028`, and that is what is cited. No menu range is claimed
   anywhere in this crate, because no file that would show one could be read.
7. **`browser.zoom.full` is not a default here.** It is Firefox's toggle for zooming text only versus the whole
   page; this crate holds one number (`default_zoom_percent`) and the wiring that would honour a text-only mode does
   not exist yet, so no field pretends at it.
8. **`%s` becomes `{searchTerms}` in the v0 migration, and the derived engine is `custom`/"Custom".** The spec
   asks for that; the name is ours, because a v0 file recorded a URL and nothing else, and a settings page needs a
   word to show. Firefox's own OpenSearch reader accepts `%s` in the same position, which is why the migration does
   the replacement rather than rejecting the URL.
9. **An empty `keyword` is no keyword.** `""` is not a shortcut a person can type, and a v0 or hand-written file
   uses `""` for "none" more often than `null`, so `""` reads as `None` before validation instead of failing the
   lower-case/space rules.
10. **`accept_languages` deduplicates case-insensitively.** `"en-US"` and `"en-us"` are the same offer to a server;
   the first spelling wins and the second is reported. A case-sensitive set would keep both and send a duplicate.
11. **A `version` written as a numeric string (`"1"`) is taken as that number; a string that is not (`"one"`) is
    reported and read as 0**, so the file is upgraded and rewritten correctly instead of being refused over one odd
    key. Absent and `null` are version 0 silently.
12. **`search.engines` with no usable engine left is reported at `search.engines`, and the built-in comes back.**
    The spec says "none left → built-in"; the warning names the list rather than each dropped item twice.
13. **`unknown` is one `changed_keys` path.** The spec asks for dotted names and the return type is
    `&'static str`, so a key that only exists in the file cannot be named; the map reports itself and the caller
    re-reads it.

## Format

`"version"` is first and the sections follow in this order: `general`, `search`, `privacy`, `downloads`,
`appearance`, `tabs`, then the unknown keys in sorted order. The order is fixed by `Settings::to_json` writing a
`Node` tree, not by `serde_json::Map`, which sorts its keys and would otherwise put `appearance` first.

```json
{
  "version": 1,
  "general": {
    "startup": "home_page",
    "home_page": "about:blank",
    "accept_languages": ["en-US", "en"]
  },
  "search": {
    "default_engine": "duckduckgo",
    "engines": [
      {
        "id": "duckduckgo",
        "name": "DuckDuckGo",
        "keyword": "ddg",
        "template": "https://duckduckgo.com/html/?q={searchTerms}",
        "suggest_url": "https://duckduckgo.com/ac/?q={searchTerms}&type=list"
      }
    ],
    "suggest_remote": false,
    "suggest_history": true,
    "suggest_bookmarks": true
  },
  "privacy": {
    "do_not_track": false,
    "cookies": "block_third_party_trackers",
    "content_blocking": false,
    "clear_on_exit": {
      "history": false,
      "cookies": false,
      "cache": false,
      "downloads_list": false
    }
  },
  "downloads": {
    "directory": null,
    "ask_where": false
  },
  "appearance": {
    "theme": "auto",
    "default_zoom_percent": 100,
    "minimum_font_size": 0,
    "show_bookmarks_bar": "new_tab_only"
  },
  "tabs": {
    "open_links_in_new_tab": true,
    "confirm_close_multiple": false,
    "new_tab_page": "blank"
  }
}
```

This is what `Settings::default().to_json()` writes, and `default_json_matches_the_documented_shape` asserts it.

Version 0 is the flat shape that was never published:

```json
{
  "home": "https://example.org/start",
  "search_engine": "https://search.example.org/?q=%s",
  "dnt": true,
  "zoom": 120,
  "theme": "dark"
}
```

No `version` key means 0. `migrate` puts `home` in `general.home_page`, `zoom` and `theme` in `appearance`, `dnt` in
`privacy.do_not_track`; a `search_engine` that is `ddg`/`duckduckgo` becomes `default_engine`, one that is a http(s)
URL with `{searchTerms}` or `%s` becomes the `custom` engine, and anything else stays where it is so the loader keeps
it as an unknown top-level key. Keys the chain does not know are never dropped.

## Checks

```
export CARGO_TARGET_DIR=$WORK_DIR/lucinanet/target
cargo test -p lucinanet-settings --offline
cargo clippy -p lucinanet-settings --offline --all-targets -- -D warnings
```

34 tests (every test name the spec lists is among them), no Verus, no mutants.
