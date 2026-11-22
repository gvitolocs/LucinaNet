/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// The shortcut tables, read from the two browsers' own sources at the pinned revisions (PORTING.md holds the
// records, and every entry says where it comes from). One group per command, in the order of `Command`; a
// command's first entry is the one its menu shows, so `binding_for` gives the shortcut the user expects.
//
// Two rules the sources force on a physical-key matcher, both explained by the entries that follow:
// Firefox names the *character* of a key (`key="+"`) and Chromium names its *keycode* (`VKEY_OEM_PLUS`), so a
// character that a shifted key and a keypad key both produce is tabled under every physical code that produces
// it on a US layout. Modifier sets match exactly, so every variant the sources carry is its own entry.

use crate::code::*;
use crate::{Binding, Command, Mods, Profile};

/// No modifier held: F5, F11, F12, F3, Esc.
const NONE: Mods = Mods { ctrl: false, shift: false, alt: false, meta: false };
/// Shift alone, which only ever appears with a function key (Firefox rejects Shift+letter as a shortcut).
const SHIFT: Mods = Mods { ctrl: false, shift: true, alt: false, meta: false };
const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false, meta: false };
const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, alt: false, meta: false };
const ALT: Mods = Mods { ctrl: false, shift: false, alt: true, meta: false };

use Command::*;

/// Firefox's Linux defaults. `browser-sets.inc.xhtml` gives the `<key>` elements with their `modifiers` and the
/// Linux platform branches; the characters they name come from `browserSets.ftl` (Firefox moved them there),
/// and the tab-cycling and Ctrl+F4 chords from `ShortcutUtils.sys.mjs`, which is where they are matched today.
static FIREFOX: &[Binding] = &[
    // browser/base/content/browser-sets.inc.xhtml @33abb90 <keyset id="mainKeyset">, with the `.key` values of
    // browser/locales/en-US/browser/browserSets.ftl @33abb90. `modifiers="accel"` is Ctrl on Linux.
    Binding { code: T, mods: CTRL, command: NewTab }, // key_newNavigatorTab, tab-new-shortcut "T"
    Binding { code: W, mods: CTRL, command: CloseTab }, // key_close, close-shortcut "W"
    // toolkit/modules/ShortcutUtils.sys.mjs @33abb90 getSystemActionForEvent: DOM_VK_F4 with Ctrl alone.
    Binding { code: F4, mods: CTRL, command: CloseTab },
    // key_restoreLastClosedTabOrWindowOrSession, accel,shift + tab-new-shortcut "T" (key_undoCloseTab itself
    // carries no key in the XUL anymore; this is the one that fires).
    Binding { code: T, mods: CTRL_SHIFT, command: ReopenClosedTab },
    // ShortcutUtils: DOM_VK_TAB with Ctrl (and no Alt/Meta) is CYCLE_TABS, forwards or backwards on Shift;
    // DOM_VK_PAGE_DOWN/PAGE_UP with Ctrl alone are NEXT_TAB/PREVIOUS_TAB.
    Binding { code: TAB, mods: CTRL, command: NextTab },
    Binding { code: PAGE_DOWN, mods: CTRL, command: NextTab },
    Binding { code: TAB, mods: CTRL_SHIFT, command: PreviousTab },
    Binding { code: PAGE_UP, mods: CTRL, command: PreviousTab },
    // key_selectTab1..8 and key_selectLastTab, whose modifier is `alt` on Linux (XP_GNOME defines
    // NUM_SELECT_TAB_MODIFIER as alt; `accel` there is the Windows/macOS branch only), for the digit characters
    // "1".."9", which the keypad types too.
    Binding { code: N1, mods: ALT, command: SelectTab(1) },
    Binding { code: KP_1, mods: ALT, command: SelectTab(1) },
    Binding { code: N2, mods: ALT, command: SelectTab(2) },
    Binding { code: KP_2, mods: ALT, command: SelectTab(2) },
    Binding { code: N3, mods: ALT, command: SelectTab(3) },
    Binding { code: KP_3, mods: ALT, command: SelectTab(3) },
    Binding { code: N4, mods: ALT, command: SelectTab(4) },
    Binding { code: KP_4, mods: ALT, command: SelectTab(4) },
    Binding { code: N5, mods: ALT, command: SelectTab(5) },
    Binding { code: KP_5, mods: ALT, command: SelectTab(5) },
    Binding { code: N6, mods: ALT, command: SelectTab(6) },
    Binding { code: KP_6, mods: ALT, command: SelectTab(6) },
    Binding { code: N7, mods: ALT, command: SelectTab(7) },
    Binding { code: KP_7, mods: ALT, command: SelectTab(7) },
    Binding { code: N8, mods: ALT, command: SelectTab(8) },
    Binding { code: KP_8, mods: ALT, command: SelectTab(8) },
    Binding { code: N9, mods: ALT, command: SelectLastTab }, // key_selectLastTab, "9"
    Binding { code: KP_9, mods: ALT, command: SelectLastTab },
    Binding { code: N, mods: CTRL, command: NewWindow }, // key_newNavigator, window-new-shortcut "N"
    // key_privatebrowsing, accel,shift + private-browsing-shortcut "P".
    Binding { code: P, mods: CTRL_SHIFT, command: NewPrivateWindow },
    Binding { code: W, mods: CTRL_SHIFT, command: CloseWindow }, // key_closeWindow, close-shortcut "W"
    Binding { code: Q, mods: CTRL, command: Quit }, // key_quitApplication, quit-app-shortcut "Q" (accel on Linux)
    Binding { code: L, mods: CTRL, command: FocusAddressBar }, // focusURLBar, location-open-shortcut "L"
    Binding { code: D, mods: ALT, command: FocusAddressBar }, // focusURLBar2 (not macOS), "D"
    // key_search (search-focus-shortcut "K") and key_search2, whose character is "J" on Linux
    // (search-focus-shortcut-alt selects [linux] J, "E" elsewhere; the comment in browser-sets.inc explains the
    // emacs Ctrl+K clash).
    Binding { code: K, mods: CTRL, command: FocusSearch },
    Binding { code: J, mods: CTRL, command: FocusSearch },
    Binding { code: F, mods: CTRL, command: Find }, // key_find, find-shortcut "F"
    // key_findAgain (accel, "G") and key_findAgain2 (search-find-again-shortcut-alt = VK_F3, no modifiers).
    Binding { code: G, mods: CTRL, command: FindNext },
    Binding { code: F3, mods: NONE, command: FindNext },
    Binding { code: G, mods: CTRL_SHIFT, command: FindPrevious }, // key_findPrevious, accel,shift "G"
    Binding { code: F3, mods: SHIFT, command: FindPrevious }, // key_findPrevious2, VK_F3 with shift
    Binding { code: D, mods: CTRL, command: BookmarkPage }, // addBookmarkAsKb, bookmark-this-page-shortcut "D"
    // viewBookmarksSidebarKb (accel, "B") and manBookmarkKb (accel,shift, "O": the library).
    Binding { code: B, mods: CTRL, command: ShowBookmarks },
    Binding { code: O, mods: CTRL_SHIFT, command: ShowBookmarks },
    // key_gotoHistory (accel, history-sidebar-shortcut "H") and showAllHistoryKb (accel,shift, "H").
    Binding { code: H, mods: CTRL, command: ShowHistory },
    Binding { code: H, mods: CTRL_SHIFT, command: ShowHistory },
    // key_openDownloads: on GNOME the modifiers are accel,shift and downloads-shortcut is [linux] "Y", so
    // Ctrl+Shift+Y. Ctrl+J there is the search bar, not downloads.
    Binding { code: Y, mods: CTRL_SHIFT, command: ShowDownloads },
    Binding { code: DELETE, mods: CTRL_SHIFT, command: ClearBrowsingData }, // key_sanitize, VK_DELETE accel,shift
    // key_reload (accel, "R") and key_reload2 (VK_F5, no modifiers).
    Binding { code: R, mods: CTRL, command: Reload },
    Binding { code: F5, mods: NONE, command: Reload },
    // key_reload_skip_cache (accel,shift, "R") and key_reload_skip_cache2 (VK_F5 with accel).
    Binding { code: R, mods: CTRL_SHIFT, command: ReloadBypassCache },
    Binding { code: F5, mods: CTRL, command: ReloadBypassCache },
    // key_stop, VK_ESCAPE with no modifiers. `Browser:Stop` is only enabled while a load is in flight, which is
    // what `lookup` gates on through `Context::loading`.
    Binding { code: ESC, mods: NONE, command: Stop },
    // goBackKb/goForwardKb (VK_LEFT/VK_RIGHT with alt, not macOS) and goBackKb2/goForwardKb2 (XP_UNIX: the
    // characters "[" and "]" with accel).
    Binding { code: LEFT, mods: ALT, command: Back },
    Binding { code: BRACKETLEFT, mods: CTRL, command: Back },
    Binding { code: RIGHT, mods: ALT, command: Forward },
    Binding { code: BRACKETRIGHT, mods: CTRL, command: Forward },
    Binding { code: HOME, mods: ALT, command: Home }, // goHome, VK_HOME with alt
    // key_fullZoomEnlarge: the characters "+" and "=" with accel, so Ctrl+= and, since "+" is Shift+"=" on a
    // US layout, Ctrl+Shift+=; the keypad + types "+" too. The third variant in the .ftl is empty.
    Binding { code: EQUAL, mods: CTRL, command: ZoomIn },
    Binding { code: EQUAL, mods: CTRL_SHIFT, command: ZoomIn },
    Binding { code: KP_PLUS, mods: CTRL, command: ZoomIn },
    // key_fullZoomReduce: "-" and "_" ("_" is Shift+"-") with accel; the keypad - types "-".
    Binding { code: MINUS, mods: CTRL, command: ZoomOut },
    Binding { code: MINUS, mods: CTRL_SHIFT, command: ZoomOut },
    Binding { code: KP_MINUS, mods: CTRL, command: ZoomOut },
    // key_fullZoomReset: the character "0" with accel, which the keypad 0 types too.
    Binding { code: N0, mods: CTRL, command: ZoomReset },
    Binding { code: KP_0, mods: CTRL, command: ZoomReset },
    Binding { code: F11, mods: NONE, command: Fullscreen }, // key_enterFullScreen, VK_F11
    Binding { code: P, mods: CTRL, command: Print }, // printKb, print-shortcut "P"
    Binding { code: S, mods: CTRL, command: SavePage }, // key_savePage, save-page-shortcut "S"
    Binding { code: U, mods: CTRL, command: ViewSource }, // key_viewSource, page-source-shortcut "U"
    // devtools/startup/DevToolsStartup.sys.mjs @33abb90 with devtools/startup/locales/en-US/key-shortcuts.ftl
    // @33abb90: the toolbox toggles on VK_F12 with no modifiers and on "I" with accel,shift.
    Binding { code: F12, mods: NONE, command: DevTools },
    Binding { code: I, mods: CTRL_SHIFT, command: DevTools },
];

/// Chromium's Linux defaults, from `chrome/browser/ui/accelerator_table.cc`: `ui::VKEY_*` plus modifiers, with
/// the `#if BUILDFLAG(...)` branches that hold on Linux kept and the macOS/ChromeOS-only ones dropped.
/// `EF_PLATFORM_ACCELERATOR` is Ctrl there, `VKEY_NEXT`/`VKEY_PRIOR` are PageDown/PageUp, `VKEY_OEM_PLUS`/
/// `VKEY_OEM_MINUS` are the `=`/`-` keys and `VKEY_ADD`/`VKEY_SUBTRACT` the keypad ones.
static CHROMIUM: &[Binding] = &[
    Binding { code: T, mods: CTRL, command: NewTab }, // IDC_NEW_TAB
    Binding { code: W, mods: CTRL, command: CloseTab }, // IDC_CLOSE_TAB
    Binding { code: F4, mods: CTRL, command: CloseTab }, // IDC_CLOSE_TAB
    Binding { code: T, mods: CTRL_SHIFT, command: ReopenClosedTab }, // IDC_RESTORE_TAB
    // Ctrl+Tab is IDC_CYCLE_TO_NEXT_TAB and Ctrl+PageDown IDC_SELECT_NEXT_TAB (browser_command_controller.cc:
    // the cycle one honours the recently-used order when that pref is on, the select one does not); both end at
    // another tab, so both are NextTab here.
    Binding { code: TAB, mods: CTRL, command: NextTab },
    Binding { code: PAGE_DOWN, mods: CTRL, command: NextTab },
    Binding { code: TAB, mods: CTRL_SHIFT, command: PreviousTab }, // IDC_CYCLE_TO_PREV_TAB
    Binding { code: PAGE_UP, mods: CTRL, command: PreviousTab }, // IDC_SELECT_PREVIOUS_TAB
    // IDC_SELECT_TAB_0..7 for VKEY_1..8 and their keypad twins, under the platform accelerator and, on Linux,
    // under Alt too (kAltTabSelectAcceleratorMap).
    Binding { code: N1, mods: CTRL, command: SelectTab(1) },
    Binding { code: KP_1, mods: CTRL, command: SelectTab(1) },
    Binding { code: N1, mods: ALT, command: SelectTab(1) },
    Binding { code: KP_1, mods: ALT, command: SelectTab(1) },
    Binding { code: N2, mods: CTRL, command: SelectTab(2) },
    Binding { code: KP_2, mods: CTRL, command: SelectTab(2) },
    Binding { code: N2, mods: ALT, command: SelectTab(2) },
    Binding { code: KP_2, mods: ALT, command: SelectTab(2) },
    Binding { code: N3, mods: CTRL, command: SelectTab(3) },
    Binding { code: KP_3, mods: CTRL, command: SelectTab(3) },
    Binding { code: N3, mods: ALT, command: SelectTab(3) },
    Binding { code: KP_3, mods: ALT, command: SelectTab(3) },
    Binding { code: N4, mods: CTRL, command: SelectTab(4) },
    Binding { code: KP_4, mods: CTRL, command: SelectTab(4) },
    Binding { code: N4, mods: ALT, command: SelectTab(4) },
    Binding { code: KP_4, mods: ALT, command: SelectTab(4) },
    Binding { code: N5, mods: CTRL, command: SelectTab(5) },
    Binding { code: KP_5, mods: CTRL, command: SelectTab(5) },
    Binding { code: N5, mods: ALT, command: SelectTab(5) },
    Binding { code: KP_5, mods: ALT, command: SelectTab(5) },
    Binding { code: N6, mods: CTRL, command: SelectTab(6) },
    Binding { code: KP_6, mods: CTRL, command: SelectTab(6) },
    Binding { code: N6, mods: ALT, command: SelectTab(6) },
    Binding { code: KP_6, mods: ALT, command: SelectTab(6) },
    Binding { code: N7, mods: CTRL, command: SelectTab(7) },
    Binding { code: KP_7, mods: CTRL, command: SelectTab(7) },
    Binding { code: N7, mods: ALT, command: SelectTab(7) },
    Binding { code: KP_7, mods: ALT, command: SelectTab(7) },
    Binding { code: N8, mods: CTRL, command: SelectTab(8) },
    Binding { code: KP_8, mods: CTRL, command: SelectTab(8) },
    Binding { code: N8, mods: ALT, command: SelectTab(8) },
    Binding { code: KP_8, mods: ALT, command: SelectTab(8) },
    Binding { code: N9, mods: CTRL, command: SelectLastTab }, // IDC_SELECT_LAST_TAB
    Binding { code: KP_9, mods: CTRL, command: SelectLastTab },
    Binding { code: N9, mods: ALT, command: SelectLastTab },
    Binding { code: KP_9, mods: ALT, command: SelectLastTab },
    Binding { code: N, mods: CTRL, command: NewWindow }, // IDC_NEW_WINDOW
    Binding { code: N, mods: CTRL_SHIFT, command: NewPrivateWindow }, // IDC_NEW_INCOGNITO_WINDOW
    Binding { code: W, mods: CTRL_SHIFT, command: CloseWindow }, // IDC_CLOSE_WINDOW
    Binding { code: F4, mods: ALT, command: CloseWindow }, // IDC_CLOSE_WINDOW
    Binding { code: L, mods: CTRL, command: FocusAddressBar }, // IDC_FOCUS_LOCATION
    Binding { code: D, mods: ALT, command: FocusAddressBar }, // IDC_FOCUS_LOCATION
    Binding { code: K, mods: CTRL, command: FocusSearch }, // IDC_FOCUS_SEARCH
    Binding { code: E, mods: CTRL, command: FocusSearch }, // IDC_FOCUS_SEARCH
    Binding { code: F, mods: CTRL, command: Find }, // IDC_FIND
    Binding { code: G, mods: CTRL, command: FindNext }, // IDC_FIND_NEXT
    Binding { code: F3, mods: NONE, command: FindNext }, // IDC_FIND_NEXT
    Binding { code: G, mods: CTRL_SHIFT, command: FindPrevious }, // IDC_FIND_PREVIOUS
    Binding { code: F3, mods: SHIFT, command: FindPrevious }, // IDC_FIND_PREVIOUS
    Binding { code: D, mods: CTRL, command: BookmarkPage }, // IDC_BOOKMARK_THIS_TAB
    Binding { code: O, mods: CTRL_SHIFT, command: ShowBookmarks }, // IDC_SHOW_BOOKMARK_MANAGER
    Binding { code: H, mods: CTRL, command: ShowHistory }, // IDC_SHOW_HISTORY
    Binding { code: J, mods: CTRL, command: ShowDownloads }, // IDC_SHOW_DOWNLOADS
    Binding { code: DELETE, mods: CTRL_SHIFT, command: ClearBrowsingData }, // IDC_CLEAR_BROWSING_DATA
    Binding { code: R, mods: CTRL, command: Reload }, // IDC_RELOAD
    Binding { code: F5, mods: NONE, command: Reload }, // IDC_RELOAD
    Binding { code: R, mods: CTRL_SHIFT, command: ReloadBypassCache }, // IDC_RELOAD_BYPASSING_CACHE
    Binding { code: F5, mods: CTRL, command: ReloadBypassCache }, // IDC_RELOAD_BYPASSING_CACHE
    Binding { code: F5, mods: SHIFT, command: ReloadBypassCache }, // IDC_RELOAD_BYPASSING_CACHE
    // IDC_CLOSE_FIND_OR_STOP: the find bubble first if it is open, else stop (browser_command_controller.cc
    // UpdateCloseFindOrStop enables it on `CanCloseFind(browser) || stop enabled`). The find bar closing itself
    // is not a command here, so the entry is the stop half, which needs a load in flight.
    Binding { code: ESC, mods: NONE, command: Stop },
    Binding { code: LEFT, mods: ALT, command: Back }, // IDC_BACK (the AltGr variant is Ctrl+Alt, not modelled)
    Binding { code: RIGHT, mods: ALT, command: Forward }, // IDC_FORWARD
    Binding { code: HOME, mods: ALT, command: Home }, // IDC_HOME
    Binding { code: EQUAL, mods: CTRL, command: ZoomIn }, // IDC_ZOOM_PLUS, VKEY_OEM_PLUS
    Binding { code: EQUAL, mods: CTRL_SHIFT, command: ZoomIn }, // IDC_ZOOM_PLUS with shift
    Binding { code: KP_PLUS, mods: CTRL, command: ZoomIn }, // IDC_ZOOM_PLUS, VKEY_ADD
    Binding { code: MINUS, mods: CTRL, command: ZoomOut }, // IDC_ZOOM_MINUS, VKEY_OEM_MINUS
    Binding { code: MINUS, mods: CTRL_SHIFT, command: ZoomOut }, // IDC_ZOOM_MINUS with shift
    Binding { code: KP_MINUS, mods: CTRL, command: ZoomOut }, // IDC_ZOOM_MINUS, VKEY_SUBTRACT
    Binding { code: N0, mods: CTRL, command: ZoomReset }, // IDC_ZOOM_NORMAL
    Binding { code: KP_0, mods: CTRL, command: ZoomReset }, // IDC_ZOOM_NORMAL, VKEY_NUMPAD0
    Binding { code: F11, mods: NONE, command: Fullscreen }, // IDC_FULLSCREEN
    Binding { code: P, mods: CTRL, command: Print }, // IDC_PRINT
    Binding { code: S, mods: CTRL, command: SavePage }, // IDC_SAVE_PAGE
    Binding { code: U, mods: CTRL, command: ViewSource }, // IDC_VIEW_SOURCE
    Binding { code: F12, mods: NONE, command: DevTools }, // IDC_DEV_TOOLS_TOGGLE
    Binding { code: I, mods: CTRL_SHIFT, command: DevTools }, // IDC_DEV_TOOLS
];

/// The defaults of `profile` for Linux, in the fixed order of the tables above.
pub fn bindings(profile: Profile) -> &'static [Binding] {
    match profile {
        Profile::Firefox => FIREFOX,
        Profile::Chromium => CHROMIUM,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant of `Command`, so the coverage test cannot quietly miss a new one.
    const EVERY_COMMAND: &[Command] = &[
        NewTab,
        CloseTab,
        ReopenClosedTab,
        NextTab,
        PreviousTab,
        SelectLastTab,
        NewWindow,
        NewPrivateWindow,
        CloseWindow,
        Quit,
        FocusAddressBar,
        FocusSearch,
        Find,
        FindNext,
        FindPrevious,
        BookmarkPage,
        ShowBookmarks,
        ShowHistory,
        ShowDownloads,
        ClearBrowsingData,
        Reload,
        ReloadBypassCache,
        Stop,
        Back,
        Forward,
        Home,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        Fullscreen,
        Print,
        SavePage,
        ViewSource,
        DevTools,
    ];

    /// Commands with no binding in a profile, because the source read above has none: Chromium's `IDC_EXIT` is a
    /// command with no Linux accelerator (`VKEY_Q` sits in the Android-only table), and Firefox quits on Ctrl+Q.
    const ABSENT: &[(Profile, Command)] = &[(Profile::Chromium, Quit)];

    fn duplicates(profile: Profile) -> Vec<(u16, Mods, Command, Command)> {
        let table = bindings(profile);
        let mut found = Vec::new();
        for (i, a) in table.iter().enumerate() {
            for b in &table[i + 1..] {
                if a.code == b.code && a.mods == b.mods {
                    found.push((a.code, a.mods, a.command, b.command));
                }
            }
        }
        found
    }

    #[test]
    fn firefox_table_has_no_duplicate_chords() {
        assert_eq!(duplicates(Profile::Firefox), Vec::new());
    }

    #[test]
    fn chromium_table_has_no_duplicate_chords() {
        assert_eq!(duplicates(Profile::Chromium), Vec::new());
    }

    #[test]
    fn every_command_bound_in_both_profiles_or_listed_absent() {
        let mut commands: Vec<Command> = EVERY_COMMAND.to_vec();
        for n in 1..=8 {
            commands.push(SelectTab(n));
        }
        for profile in [Profile::Firefox, Profile::Chromium] {
            for command in &commands {
                let bound = bindings(profile).iter().any(|b| b.command == *command);
                let given_up = ABSENT.contains(&(profile, *command));
                assert!(bound || given_up, "{profile:?} {command:?}: bound {bound}, listed {given_up}");
                if !bound {
                    assert!(given_up, "{profile:?} {command:?} is bound in neither the table nor the absent list");
                }
            }
        }
        // The list itself covers every variant; Command grows, this does not.
        assert_eq!(commands.len(), 34 + 8);
    }

    #[test]
    fn no_table_entry_holds_the_super_key() {
        // Rule 1: Linux browsers leave Meta to the desktop, so a chord with it is never in a table and
        // `lookup`'s rejection of it loses nothing.
        for profile in [Profile::Firefox, Profile::Chromium] {
            for b in bindings(profile) {
                assert!(!b.mods.meta, "{profile:?} {b:?}");
            }
        }
    }

    #[test]
    fn no_binding_is_a_bare_letter_or_digit_or_shifted_one() {
        // A bare or Shift-only character key belongs to the page and to text editing; the only keys matched
        // without a control modifier are function keys and Esc.
        for profile in [Profile::Firefox, Profile::Chromium] {
            for b in bindings(profile) {
                if b.mods.ctrl || b.mods.alt || b.mods.meta {
                    continue;
                }
                let named = [F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, ESC];
                assert!(named.contains(&b.code), "{profile:?} {b:?}");
            }
        }
    }

    #[test]
    fn table_keys_use_the_evdev_codes_of_lucinanet_ui() {
        // crates/lucinanet-ui/src/tabs.rs: KEY_T = 20, KEY_W = 17, KEY_TAB = 15; browser.rs: KEY_L = 38. The
        // rest are the same header (linux/input-event-codes.h) that lucina_app::keys is read from.
        assert_eq!(T, 20);
        assert_eq!(W, 17);
        assert_eq!(TAB, 15);
        assert_eq!(L, 38);
        assert_eq!(D, 32);
        assert_eq!(F, 33);
        assert_eq!(H, 35);
        assert_eq!(J, 36);
        // The page-up/down and keypad codes the tables lean on.
        assert_eq!(PAGE_UP, 104);
        assert_eq!(PAGE_DOWN, 109);
        assert_eq!(KP_PLUS, 78);
        assert_eq!(KP_MINUS, 74);
        assert_eq!(KP_0, 82);
        assert_eq!(ESC, 1);
        assert_eq!(DELETE, 111);
    }
}
