# lucinanet-shortcuts — porting record

The browser's own keyboard shortcuts for Linux, as data, plus the match from a key press to a command. The two
tables are Firefox's and Chromium's defaults, read from their sources at the pinned revisions; nothing here is
written from memory of what a browser "usually" does.

Base: LucinaNet `main` at `58acc9f`. Toolchain 1.99.1, edition 2024. Our licence: MPL-2.0.

## Revisions read

| Upstream | Where read | Sha / branch |
|---|---|---|
| Firefox | `raw.githubusercontent.com/mozilla-firefox/firefox` | `33abb900fc1646fef2e5ffed2d6b22e806ae10c1` |
| Chromium | `raw.githubusercontent.com/chromium/chromium` | `e9a46735de970a8e31217f2233efa3a47a1c337d` |
| Linux kernel | `raw.githubusercontent.com/torvalds/linux` | `master`, fetched 2026-11-11 |
| Servo (servoshell) | local `/home/user/Projects/Lucina/LucinaNet-servo` | `b26a7d92` pin |

Both pinned shas answered HTTP 200 for every path the brief listed, with one exception: the brief's
`browser/locales/en-US/browser/menu.ftl`-style location of the shortcut *characters* has moved. At `33abb90` a
`<key>` element carries `data-l10n-id` and no `key` attribute, so the character of almost every Firefox shortcut
comes from `browser/locales/en-US/browser/browserSets.ftl`, which is what `browser-sets.inc.xhtml` refers to.
I read that file too (it is where `Ctrl+Shift+Y` for downloads and `Ctrl+J` for the search bar come from, both
Linux-only). `browser-menubar.inc.xhtml` at this sha only names the `<key>` ids, so it gave the menu references,
not the chords.

## Records, one per source

| Source | Record | Licence | What I took |
|---|---|---|---|
| `browser/base/content/browser-sets.inc.xhtml` @33abb90 | adapt | MPL-2.0 | The `<key>` elements and their `modifiers`, the Linux branches (`#ifndef XP_MACOSX`, `#ifdef XP_UNIX`, `XP_GNOME` → `NUM_SELECT_TAB_MODIFIER alt`, `key_openDownloads` accel,shift). Rewritten as `static FIREFOX: &[Binding]` with our own `Command` enum; no XUL, no chrome, no l10n id. |
| `browser/locales/en-US/browser/browserSets.ftl` @33abb90 | adapt | MPL-2.0 | The `.key`/`.keycode` each `data-l10n-id` supplies, with the `PLATFORM()` `[linux]` values (`search-focus-shortcut-alt` "J", `downloads-shortcut` "Y"), and the zoom set (`+`, `=`, empty; `-`, `_`, empty; `0`). |
| `toolkit/modules/ShortcutUtils.sys.mjs` @33abb90 | adapt | MPL-2.0 | `getSystemActionForEvent`: Ctrl+Tab (CYCLE_TABS, direction on Shift), Ctrl+PageUp/PageDown (PREVIOUS/NEXT_TAB), Ctrl+F4 (CLOSE_TAB), and the `ctrlOnly`/`ctrlShift` definitions that make Ctrl+Alt+T nothing. Also `validate()`'s rule that Shift alone only goes with a function key, which `no_binding_is_a_bare_letter_or_digit_or_shifted_one` holds. The modifier *names* in order for `label` come from `getModifierString`. |
| `browser/base/content/browser-menubar.inc.xhtml` @33abb90 | reference | MPL-2.0 | Which `<key>` each menu row advertises — the reason a command's first table entry is the menu's chord. No text taken. |
| `toolkit/content/editMenuKeys.inc.xhtml` @33abb90 | reject | MPL-2.0 | Ctrl+Z/X/C/V/A and Delete are editing commands, and the file's own header says the nodes are "here only for show" (the real bindings are in `platformHTMLBindings.xml`). They are not browser commands, so they are not in the enum and not in the table: the page and the focused field keep them. |
| `devtools/startup/DevToolsStartup.sys.mjs` + `devtools/startup/locales/en-US/key-shortcuts.ftl` @33abb90 | adapt | MPL-2.0 | The toolbox toggle: `VK_F12` with no modifiers, and "I" with `accel,shift` on non-Mac. The brief's `key_toggleToolbox` is not a `<key>` in `browser-sets.inc.xhtml` at this sha; these two files are where the chords live. |
| `browser/app/profile/firefox.js` @33abb90 | reference | MPL-2.0 | `browser.backspace_action = 2` (Backspace goes neither back nor anywhere: "and other values, nothing"), so Backspace is not bound; `browser.ctrlTab.sortByRecentlyUsed = false`. |
| Chromium `chrome/browser/ui/accelerator_table.cc` @e9a4673 | adapt | BSD-3-Clause | The whole Chromium table: `kAcceleratorMap`, `kDevToolsAcceleratorMap`, `kDesktopOnlyAcceleratorMap`, `kNonChromeOSAcceleratorMap`, `kLinuxAndChromeOSAcceleratorMap`, `kLinuxAndWinAcceleratorMap`, `kAltTabSelectAcceleratorMap`, `kLinuxAcceleratorMap`, `kTabGroupAcceleratorMap`, with the `#if BUILDFLAG` branches resolved for Linux (no Mac, no ChromeOS, no Android). `ui::VKEY_*` → our evdev codes, `EF_PLATFORM_ACCELERATOR` → Ctrl, `EF_*` → `Mods`. See `NOTICE`. |
| Chromium `chrome/browser/ui/browser_command_controller.cc` @e9a4673 | adapt | BSD-3-Clause | What the `IDC_*` do, which decided the shape of two entries: `IDC_CYCLE_TO_NEXT_TAB` vs `IDC_SELECT_NEXT_TAB` (both end on another tab → `NextTab`, and Ctrl+Shift+PageDown is `IDC_MOVE_TAB_NEXT`, which *moves* it and so is not our command), and `IDC_CLOSE_FIND_OR_STOP` (`CanCloseFind` first, then stop; enabled on `CanCloseFind \|\| stop enabled`, which is why `Context::loading` gates Esc). `IDC_EXIT` has no desktop accelerator. |
| Chromium `LICENSE` @e9a4673 | — | BSD-3-Clause | The copyright line and the three clauses, copied into `NOTICE`. |
| `include/uapi/linux/input-event-codes.h` (torvalds/linux master) | reference | GPL-2.0 with the syscall note | Only the numeric value of each `KEY_*` (a table of numbers is fact, not expression). They are the same numbers `lucina_app::keys` and `lucinanet-ui` already hardcode, and `table_keys_use_the_evdev_codes_of_lucinanet_ui` pins the eight the brief names. |
| `ports/servoshell/desktop/headed_window.rs` (LucinaNet-servo `b26a7d92`) | reference | MPL-2.0 | Cross-check only: Servo's own shell cycles tabs on Ctrl+PageDown/PageUp and reloads on F5, which is what both tables say. Its toolkit is not ours and nothing was taken. |
| `crates/lucinanet-ui/src/tabs.rs`, `crates/lucinanet-ui/src/browser.rs` | reference | MPL-2.0 | Today's `tab_key` and the `KEY_L` branch, which rule 6 requires the tables to reproduce (`existing_tab_keys_unchanged`, `ctrl_l_focuses_address_bar_only_without_extra_modifiers`). |
| `crates/lucinanet-media/Cargo.toml` | reference | MPL-2.0 | The crate header shape (the brief's "Crate shape" rule). |

## The two character-vs-keycode rules the sources forced

Firefox binds a **character** (`key="+"`), Chromium a **keycode** (`VKEY_OEM_PLUS`). Since this crate matches a
physical code with exact modifiers, one Firefox character can be several codes:

- "+" is Shift+"=" on a US layout, so `EQUAL` with Ctrl (Firefox's `=`, Chromium's `VKEY_OEM_PLUS`) and `EQUAL`
  with Ctrl+Shift (Firefox's `+`, Chromium's shift variant) are both `ZoomIn`; `KP_PLUS` (Firefox types "+",
  Chromium's `VKEY_ADD`) too. `MINUS`/`KP_MINUS` the same way for `ZoomOut` ("-" and its Shift twin "_"), and
  `N0`/`KP_0` for `ZoomReset`. This is rule 2, and the `key_fullZoomEnlarge`/`Reduce`/`Reset` `.ftl` values are
  `+`/`=`, `-`/`_`, `0` — the third variant in each group is an empty `.key`, i.e. unbound.
- The digits Firefox binds for tab selection are typed by the keypad too, so `KP_1..KP_9` are tabled beside
  `N1..N9` in both profiles (Chromium names the `VKEY_NUMPAD*` keys itself, so it is not an assumption there).
  `KP_EQUAL` is not tabled: no source names it.

## Where the brief and the sources differ, and which one the code follows

The original wins in every case, as `docs/porting-guidelines.md` §1 requires.

1. **`ShowDownloads`, Firefox.** Brief: Ctrl+J. Source: `key_openDownloads` is `modifiers="accel,shift"` under
   `#ifdef XP_GNOME` and `downloads-shortcut` is `[linux] Y` → **Ctrl+Shift+Y**. Followed the source.
2. **`FocusSearch`, Firefox.** Brief: Ctrl+K and Ctrl+E. Source: `key_search2` takes `search-focus-shortcut-alt`,
   which is `[linux] J` ("E" is the `[other]` value) — and the comment in `browser-sets.inc` says it outright:
   "Unix: Ctrl+K, Ctrl+J (in case of emacs Ctrl+K conflict)". → **Ctrl+K and Ctrl+J**. Followed the source. The
   two together are why downloads moved to Ctrl+Shift+Y, and they also explain rule 6's need for Ctrl+J to stay
   free of a downloads binding in the Firefox profile.
3. **`FocusAddressBar`, F6.** Brief lists F6 for both profiles. Chromium's `VKEY_F6` is `IDC_FOCUS_NEXT_PANE` /
   `IDC_FOCUS_PREVIOUS_PANE` / (with Ctrl) `IDC_FOCUS_WEB_CONTENTS_PANE` — pane cycling, not the address bar — and
   `browser-sets.inc.xhtml` carries no F6 element at this sha (I searched the keyset; the toolkit's own focus
   cycling, if any, is not a browser command either way). **Not tabled**; `FocusAddressBar` stays Ctrl+L and Alt+D
   in both profiles.
4. **`Back`/`Forward`, Backspace.** The brief's own guess is confirmed by the source: Firefox still ships the
   `VK_BACK` keys (`cmd_handleBackspace`, `cmd_handleShiftBackspace`), but they are `reserved="false"` and
   `browser.backspace_action` defaults to 2, whose comment is "and other values, nothing". **Never bound** in
   either profile, and `backspace_is_never_bound` says so. Chromium's clipboard and selection chords (Ctrl+X/C/V,
   Ctrl+A) are not accelerators either: `accelerator_table.cc:458` says outright "The standard Ctrl-X, Ctrl-V and
   Ctrl-C are not defined as accelerators anywhere else", and they live in
   `GetStandardAcceleratorForCommandId`, which `GetAcceleratorForCommandId` consults only to label a menu item.
   Neither browser gives `Back`/`Forward` to anything but the mouse-button gesture, so `None` is right and the
   brief's "unless the source says otherwise" is not triggered; `alt_left_right_navigate_history` asserts Alt+Left
   and Alt+Right and the Backspace answer. Firefox's Linux `goBackKb2`/`goForwardKb2` —
   **Ctrl+[ and Ctrl+]**, which the brief does not list — **are** tabled, since the source has them on XP_UNIX.
5. **Tab digits, Firefox.** `#ifdef XP_GNOME` makes the modifier `alt`, so Linux is **Alt+1..8 and Alt+9 only**.
   The `accel` branch is the Windows/macOS one, so **Ctrl+digit is not bound** in the Firefox profile. That is the
   answer rule 5's `select_tab_digits_firefox_linux_uses_alt` asked me to verify.
6. **`ReopenClosedTab`, Firefox.** `key_undoCloseTab` exists but carries no `key`/`modifiers` at this sha; the
   chord is `key_restoreLastClosedTabOrWindowOrSession` (`accel,shift` + tab-new-shortcut "T"). Same Ctrl+Shift+T
   the brief expects, different element — recorded so the next reader does not look for the wrong id.
7. **Ctrl+Shift+Tab, Firefox.** `key_showAllTabs` (`VK_TAB`, `control,shift`, **no command**) is what the View
   menu advertises as "Show all tabs", while `ShortcutUtils.getSystemActionForEvent` returns CYCLE_TABS for
   Ctrl+Tab with Shift meaning backwards, and our `tab_key` already gives Previous. Tabled as **PreviousTab**,
   which is rule 6 and the ShortcutUtils behaviour; the commandless menu element is the one thing overruled.
8. **`Quit`, Chromium — the one absent command.** `IDC_EXIT` is a real command (`UpdateCommandEnabled(..., true)`)
   but appears in no Linux table; `VKEY_Q` with Ctrl is in `kAndroidAcceleratorMap` only. So the Chromium profile
   has **no Quit binding**, and it is the single entry of the allow-list in
   `every_command_bound_in_both_profiles_or_listed_absent`. Firefox quits on Ctrl+Q.
9. **`CloseWindow`, Chromium.** The brief lists Ctrl+Shift+W; the source also carries `VKEY_F4` with `EF_ALT_DOWN`
   → **Alt+F4 tabled** too (after the menu chord, so `binding_for` still says Ctrl+Shift+W).
10. **Chromium commands with a Linux chord and no `Command::` variant** are left out on purpose, all read from
    the Linux-active tables of `accelerator_table.cc`: Ctrl+Shift+L (`IDC_TOGGLE_VERTICAL_TABS_COLLAPSE`, line 172
    — the important one, since it is why `Ctrl+Shift+L` must answer None), Ctrl+Shift+A (`IDC_TAB_SEARCH`, 63),
    Ctrl+Shift+P (`IDC_BASIC_PRINT`, print preview, 175; our `Print` is `IDC_PRINT`, 66), Ctrl+Shift+M (avatar
    menu, 170), Ctrl+Shift+B (bookmark bar, 101), Ctrl+Shift+D (bookmark all tabs, 57), Ctrl+Shift+{J,C} (the
    console and the inspector panels, 160-163), Ctrl+O (open file, 70), Alt+E and Alt+F (the app menu, 147-148),
    Alt+Shift+T/B/A/R (toolbar, bookmarks view, the accessibility focus popup, the reading list),
    Ctrl+Shift+PageUp/PageDown (`IDC_MOVE_TAB_*`, 250-252, which *moves* a tab, see the
    `browser_command_controller.cc` record), Alt+Shift+N (split view, 253), the tab-group Alt+Shift+{C,P,X,Z,W}
    (`kTabGroupAcceleratorMap`, 317-321, active on Linux), F1/F7/F10 (help, caret browsing, menu bar — plus the
    Menu/LMenu/RMENU keys themselves, 182-184), F6 pane focus (see 3), Shift+Esc (task manager, 193) and the media
    keys (`VKEY_BROWSER_BACK/FORWARD/HOME/REFRESH/CLOSE/NEW` from `kLinuxAndChromeOSAcceleratorMap`,
    `VKEY_BROWSER_SEARCH → IDC_FOCUS_SEARCH` from `kNonChromeOSAcceleratorMap`, `VKEY_BROWSER_FAVORITES` from
    `kLinuxAcceleratorMap`) — chords on rows that are not rows of a keyboard, so they belong to the wiring's key
    device and not to this matcher. Nothing that exists only under another `BUILDFLAG` is counted: Ctrl+/ for
    help, Ctrl+Alt+N, Search+Esc and Search+Z are `kChromeOSAcceleratorMap`, Ctrl+Shift+Backspace for the clear
    dialog too, and `IDC_TAB_SEARCH`'s Alt+Shift twin and `IDC_EXIT` (Ctrl+Q, see 8) are
    `kAndroidAcceleratorMap`.
11. **Firefox commands with a Linux chord and no `Command::` variant**, same reason, all from
    `browser-sets.inc.xhtml`: Ctrl+Shift+N (`History:UndoCloseWindow` — the *window*, where our
    `ReopenClosedTab` is the tab), Ctrl+Shift+F (`Tools:Addons`), Ctrl+Shift+S (`Browser:Screenshot`, whose
    `screenshot-shortcut` `.key` is not in `browserSets.ftl` and which I did not chase, the command not being in
    the enum either way),
    Ctrl+Shift+A (`Browser:SearchTabs`), Ctrl+Shift+X (`cmd_switchTextDirection`), Ctrl+Shift+D (bookmark all
    tabs), Ctrl+B and Ctrl+Shift+B (bookmarks sidebar and toolbar), Ctrl+I (`View:PageInfo`), Ctrl+O
    (`Browser:OpenFile`), Ctrl+M (`cmd_toggleMute` — `key_toggleMute` carries `modifiers="control"`, so it is
    Ctrl+M, not the Ctrl+Shift+Alt+M a Windows Chrome user expects), Shift+Esc (`View:AboutProcesses`),
    Ctrl+Alt+R (reader mode; `VK_F9` is its `[windows]` variant), Ctrl+Alt+{U,X,Z} (the three sidebars), plain F7
    (`TOGGLE_CARET_BROWSING`, `ShortcutUtils.sys.mjs:401`, while Shift+F7 is the DevTools Style Editor) and
    Ctrl+Shift+PageUp/PageDown (move tab, `ShortcutUtils.sys.mjs`). Also left out because no key chord reaches
    them: `cmd_CustomizeToolbars`, `Browser:SearchBookmarks`, `Profiles:*`, `cmd_translate`,
    `cmd_toggleOfflineStatus`, the split-view commands and the gesture commands.

## Rule 4, what the sources actually say about a focused text field

Nothing filters on it, as the brief expected. In Firefox the keys that would collide — Ctrl+Z, Ctrl+X, Ctrl+C,
Ctrl+V, Ctrl+A, Delete, Backspace — are either `internal="true"` in `browser-sets.inc.xhtml` or in
`editMenuKeys.inc.xhtml`, whose header says they exist "only for show" because the real bindings are the
platform's HTML input bindings; none of them is a browser command. In Chromium Ctrl+X/C/V appear only in
`GetStandardAcceleratorForCommandId`, which exists to label the context menu and is not part of
`GetAcceleratorList()`. Every other entry in both tables needs Ctrl or Alt, and no bare letter, digit or
Shift-only chord is in either table (`no_binding_is_a_bare_letter_or_digit_or_shifted_one`), so a caret in a
field changes no answer. `Context::text_field_focused` is accepted and ignored, and
`text_field_focus_flag_changes_nothing_unless_source_says_so` walks both whole tables to keep that honest. The
wiring still needs the flag: it decides *what a command does* (Ctrl+A in the address bar selects all its text,
Find opens the bar without taking the page's selection), which is not this crate's question.

## Not ported, on purpose

The tables are the default chords of the two profiles and nothing else: no user `about:config`/`Shortcuts`
rebinding, no extension keysets, no locale-dependent `.ftl` overrides (the `[linux]` values are for an en-US
build; a localized build can move a letter, which is a wiring question about `Mods`+layout, not a second table),
no Windows/macOS branches of either source, and no execution of a command — that is `browser.rs`/`desktop.rs`,
which will replace `tab_key` and the `KEY_L` branch and run these.
