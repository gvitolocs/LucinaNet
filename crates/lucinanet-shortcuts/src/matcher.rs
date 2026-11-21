/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// The match from a key press to a command, and the two reads a menu needs. Matching is one equality over
// (physical key, modifiers) in the profile's table: the exactness is what Firefox's `KeyEventHandler` and
// Chromium's `Accelerator::Matches` both do, and it is what keeps Ctrl+T from firing when the person means
// Ctrl+Alt+T.

use crate::bindings;
use crate::code::name;
use crate::{Binding, Command, Context, Mods, Profile};

/// The command for a key press, or None, which means the press goes on to the page.
///
/// The modifiers must match exactly: Ctrl+T is not Ctrl+Shift+T and not Ctrl+Alt+T. Meta is never part of a
/// match, because the Linux desktop owns the Super key and neither browser binds a chord with it.
///
/// Of `Context` only `loading` changes the answer, for Esc: it stops a load and is the page's key otherwise.
/// `find_bar_open` belongs to the find bar - neither table has a find-bar Esc entry, so the wiring hands the key
/// to the bar before asking (Chromium's `IDC_CLOSE_FIND_OR_STOP` does the same thing in one command) - and
/// `text_field_focused` filters nothing in either source - see PORTING.md.
pub fn lookup(profile: Profile, code: u16, mods: Mods, cx: Context) -> Option<Command> {
    if mods.meta {
        return None;
    }

    let binding = bindings(profile).iter().find(|b| b.code == code && b.mods == mods)?;

    // Firefox's `Browser:Stop` and Chromium's `IDC_CLOSE_FIND_OR_STOP` are enabled only while a load is in
    // flight, so an idle page keeps its Esc (dialogs, fullscreen, the page's own handlers).
    if binding.command == Command::Stop && !cx.loading {
        return None;
    }

    Some(binding.command)
}

/// The shortcut text to show in a menu for `binding`: "Ctrl+Shift+T", "Alt+Left", "F5", "Ctrl+KP +".
///
/// Modifiers come in the order Ctrl, Shift, Alt, as Firefox's `ShortcutUtils::getModifierString` and Chromium's
/// `Accelerator::GetShortcutText` list them on Linux. Meta is not written out: no table entry holds one, and a
/// menu would be advertising a chord [`lookup`] never answers to.
pub fn label(binding: &Binding) -> String {
    let mut text = String::new();
    if binding.mods.ctrl {
        text.push_str("Ctrl+");
    }
    if binding.mods.shift {
        text.push_str("Shift+");
    }
    if binding.mods.alt {
        text.push_str("Alt+");
    }
    text.push_str(name(binding.code));
    text
}

/// The first binding listed for `command`, which is the one its menu shows; None if the profile has none.
pub fn binding_for(profile: Profile, command: Command) -> Option<&'static Binding> {
    bindings(profile).iter().find(|b| b.command == command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code;

    const NONE: Mods = Mods { ctrl: false, shift: false, alt: false, meta: false };
    const SHIFT: Mods = Mods { ctrl: false, shift: true, alt: false, meta: false };
    const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false, meta: false };
    const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, alt: false, meta: false };
    const CTRL_ALT: Mods = Mods { ctrl: true, shift: false, alt: true, meta: false };
    const CTRL_META: Mods = Mods { ctrl: true, shift: false, alt: false, meta: true };
    const ALT: Mods = Mods { ctrl: false, shift: false, alt: true, meta: false };
    const META: Mods = Mods { ctrl: false, shift: false, alt: false, meta: true };
    const SHIFT_ALT: Mods = Mods { ctrl: false, shift: true, alt: true, meta: false };

    const PROFILES: [Profile; 2] = [Profile::Firefox, Profile::Chromium];

    /// The digit row and its keypad twins, in the order 1..=9.
    const DIGITS: [u16; 9] = [code::N1, code::N2, code::N3, code::N4, code::N5, code::N6, code::N7, code::N8, code::N9];
    const KP_DIGITS: [u16; 9] = [
        code::KP_1,
        code::KP_2,
        code::KP_3,
        code::KP_4,
        code::KP_5,
        code::KP_6,
        code::KP_7,
        code::KP_8,
        code::KP_9,
    ];
    const LETTERS: [u16; 26] = [
        code::A, code::B, code::C, code::D, code::E, code::F, code::G, code::H, code::I, code::J, code::K, code::L,
        code::M, code::N, code::O, code::P, code::Q, code::R, code::S, code::T, code::U, code::V, code::W, code::X,
        code::Y, code::Z,
    ];

    /// The label of the menu binding of `command` in `profile`.
    fn first_label(profile: Profile, command: Command) -> String {
        label(binding_for(profile, command).expect("the command has a menu binding"))
    }

    /// The label of the table entry with exactly this key and these modifiers.
    fn label_of(profile: Profile, key: u16, mods: Mods) -> String {
        let found = bindings(profile)
            .iter()
            .find(|b| b.code == key && b.mods == mods)
            .expect("the table has this entry");
        label(found)
    }

    #[test]
    fn existing_tab_keys_unchanged() {
        // Rule 6: what crates/lucinanet-ui/src/tabs.rs `tab_key` and browser.rs answer today must not move.
        for profile in PROFILES {
            assert_eq!(lookup(profile, code::T, CTRL, Context::default()), Some(Command::NewTab), "{profile:?}");
            assert_eq!(lookup(profile, code::W, CTRL, Context::default()), Some(Command::CloseTab), "{profile:?}");
            assert_eq!(lookup(profile, code::TAB, CTRL, Context::default()), Some(Command::NextTab), "{profile:?}");
            assert_eq!(
                lookup(profile, code::TAB, CTRL_SHIFT, Context::default()),
                Some(Command::PreviousTab),
                "{profile:?}"
            );
            assert_eq!(
                lookup(profile, code::L, CTRL, Context::default()),
                Some(Command::FocusAddressBar),
                "{profile:?}"
            );
            // `tab_key` hands back None as soon as Alt or Meta is held, and Ctrl+Shift+T is its own shortcut.
            assert_eq!(lookup(profile, code::T, CTRL_ALT, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::T, CTRL_META, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::T, NONE, Context::default()), None, "{profile:?}");
            assert_eq!(
                lookup(profile, code::T, CTRL_SHIFT, Context::default()),
                Some(Command::ReopenClosedTab),
                "{profile:?}"
            );
            assert_eq!(lookup(profile, code::W, CTRL_SHIFT, Context::default()), Some(Command::CloseWindow));
        }
    }

    #[test]
    fn ctrl_l_focuses_address_bar_only_without_extra_modifiers() {
        // browser.rs's `shortcuts`: Ctrl and not Alt and not Meta on KEY_L. Shift is the extra condition the
        // sources add - Ctrl+Shift+L is Chromium's collapse-the-tab-strip (not a command) and nothing in Firefox.
        for profile in PROFILES {
            assert_eq!(
                lookup(profile, code::L, CTRL, Context::default()),
                Some(Command::FocusAddressBar),
                "{profile:?}"
            );
            assert_eq!(lookup(profile, code::L, CTRL_SHIFT, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::L, CTRL_ALT, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::L, CTRL_META, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::L, ALT, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::L, SHIFT, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::L, NONE, Context::default()), None, "{profile:?}");
            // Alt+D is the second binding for the same command in both browsers.
            assert_eq!(
                lookup(profile, code::D, ALT, Context::default()),
                Some(Command::FocusAddressBar),
                "{profile:?}"
            );
        }
    }

    #[test]
    fn ctrl_shift_t_reopens_tab_ctrl_t_opens() {
        for profile in PROFILES {
            assert_eq!(lookup(profile, code::T, CTRL, Context::default()), Some(Command::NewTab), "{profile:?}");
            assert_eq!(
                lookup(profile, code::T, CTRL_SHIFT, Context::default()),
                Some(Command::ReopenClosedTab),
                "{profile:?}"
            );
        }
    }

    #[test]
    fn exact_modifiers_required() {
        // Each of these is a chord the sources give to something else, or to nothing at all.
        assert_eq!(lookup(Profile::Firefox, code::T, CTRL_ALT, Context::default()), None);
        assert_eq!(lookup(Profile::Chromium, code::T, CTRL_ALT, Context::default()), None);
        // Firefox binds Ctrl+Alt+U (chat sidebar) and Ctrl+Alt+R (reader mode): not browser commands of ours.
        assert_eq!(lookup(Profile::Firefox, code::U, CTRL_ALT, Context::default()), None);
        assert_eq!(lookup(Profile::Firefox, code::R, CTRL_ALT, Context::default()), None);
        // Zoom differs by Shift in one direction only, and Ctrl+Shift+0 is bound in neither browser.
        assert_eq!(lookup(Profile::Firefox, code::MINUS, CTRL, Context::default()), Some(Command::ZoomOut));
        assert_eq!(lookup(Profile::Firefox, code::N0, CTRL_SHIFT, Context::default()), None);
        assert_eq!(lookup(Profile::Chromium, code::N0, CTRL_SHIFT, Context::default()), None);
        // Ctrl+Shift+PageUp/PageDown *moves* the tab (Chromium IDC_MOVE_TAB_NEXT/PREVIOUS, Firefox
        // MOVE_TAB_FORWARD/BACKWARD), which is not a command in this enum.
        assert_eq!(lookup(Profile::Chromium, code::PAGE_DOWN, CTRL_SHIFT, Context::default()), None);
        assert_eq!(lookup(Profile::Firefox, code::PAGE_DOWN, CTRL_SHIFT, Context::default()), None);
        assert_eq!(lookup(Profile::Chromium, code::PAGE_UP, CTRL_SHIFT, Context::default()), None);
        // Alt+Shift+T is Chromium's focus-the-toolbar.
        assert_eq!(lookup(Profile::Chromium, code::T, SHIFT_ALT, Context::default()), None);
        // The pairs that differ only by which browser they come from: Ctrl+J and Ctrl+Shift+Y.
        assert_eq!(lookup(Profile::Firefox, code::J, CTRL, Context::default()), Some(Command::FocusSearch));
        assert_eq!(lookup(Profile::Chromium, code::J, CTRL, Context::default()), Some(Command::ShowDownloads));
        assert_eq!(lookup(Profile::Firefox, code::Y, CTRL_SHIFT, Context::default()), Some(Command::ShowDownloads));
        assert_eq!(lookup(Profile::Chromium, code::Y, CTRL_SHIFT, Context::default()), None);
    }

    #[test]
    fn meta_never_matches() {
        // Rule 1: Super is the desktop's. Held with anything at all, it answers to nothing, so the press goes
        // to the window manager instead of the browser.
        for profile in PROFILES {
            for binding in bindings(profile) {
                let with_meta =
                    Mods { ctrl: binding.mods.ctrl, shift: binding.mods.shift, alt: binding.mods.alt, meta: true };
                assert_eq!(lookup(profile, binding.code, with_meta, Context::default()), None, "{profile:?} {binding:?}");
                let loading = Context { loading: true, ..Context::default() };
                assert_eq!(lookup(profile, binding.code, with_meta, loading), None, "{profile:?} {binding:?}");
            }
            assert_eq!(lookup(profile, code::T, META, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::ESC, META, Context::default()), None, "{profile:?}");
        }
    }

    #[test]
    fn tab_cycle_both_directions_and_pageup_pagedown() {
        for profile in PROFILES {
            assert_eq!(lookup(profile, code::TAB, CTRL, Context::default()), Some(Command::NextTab), "{profile:?}");
            assert_eq!(
                lookup(profile, code::TAB, CTRL_SHIFT, Context::default()),
                Some(Command::PreviousTab),
                "{profile:?}"
            );
            assert_eq!(
                lookup(profile, code::PAGE_DOWN, CTRL, Context::default()),
                Some(Command::NextTab),
                "{profile:?}"
            );
            assert_eq!(
                lookup(profile, code::PAGE_UP, CTRL, Context::default()),
                Some(Command::PreviousTab),
                "{profile:?}"
            );
            // Without Ctrl the page keeps them: bare Tab walks the focus, PageUp/PageDown scroll, and Shift+Tab
            // is the page's reverse focus walk.
            assert_eq!(lookup(profile, code::PAGE_DOWN, NONE, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::PAGE_UP, NONE, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::TAB, NONE, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::TAB, SHIFT, Context::default()), None, "{profile:?}");
        }
    }

    #[test]
    fn select_tab_digits_firefox_linux_uses_alt() {
        // browser-sets.inc: `#ifdef XP_GNOME #define NUM_SELECT_TAB_MODIFIER alt`, so on Linux the digits are
        // Alt+1..8 and Alt+9. The `accel` branch there is the Windows/macOS one, so Ctrl+digit - which the brief
        // listed as the other possibility - is not bound. The keypad types the same characters, and Firefox
        // matches on the character, so it selects tabs too.
        for (i, key) in DIGITS.iter().enumerate() {
            let n = i as u8 + 1;
            let want = if n <= 8 { Command::SelectTab(n) } else { Command::SelectLastTab };
            assert_eq!(lookup(Profile::Firefox, *key, ALT, Context::default()), Some(want), "Alt+{n}");
            assert_eq!(lookup(Profile::Firefox, KP_DIGITS[i], ALT, Context::default()), Some(want), "Alt+KP {n}");
            assert_eq!(lookup(Profile::Firefox, *key, CTRL, Context::default()), None, "Ctrl+{n}");
            assert_eq!(lookup(Profile::Firefox, *key, CTRL_SHIFT, Context::default()), None, "Ctrl+Shift+{n}");
            assert_eq!(lookup(Profile::Firefox, *key, NONE, Context::default()), None, "{n} alone");
        }
    }

    #[test]
    fn select_tab_digits_chromium_ctrl_and_alt_1_to_9() {
        // accelerator_table.cc: IDC_SELECT_TAB_0..7 and IDC_SELECT_LAST_TAB on VKEY_1..9 and their NUMPAD twins,
        // under the platform accelerator (Ctrl) and again under Alt (kAltTabSelectAcceleratorMap, Linux).
        for (i, key) in DIGITS.iter().enumerate() {
            let n = i as u8 + 1;
            let want = if n <= 8 { Command::SelectTab(n) } else { Command::SelectLastTab };
            assert_eq!(lookup(Profile::Chromium, *key, CTRL, Context::default()), Some(want), "Ctrl+{n}");
            assert_eq!(lookup(Profile::Chromium, *key, ALT, Context::default()), Some(want), "Alt+{n}");
            assert_eq!(lookup(Profile::Chromium, KP_DIGITS[i], CTRL, Context::default()), Some(want), "Ctrl+KP {n}");
            assert_eq!(lookup(Profile::Chromium, KP_DIGITS[i], ALT, Context::default()), Some(want), "Alt+KP {n}");
            assert_eq!(lookup(Profile::Chromium, *key, SHIFT, Context::default()), None, "Shift+{n}");
            assert_eq!(lookup(Profile::Chromium, *key, CTRL_ALT, Context::default()), None, "Ctrl+Alt+{n}");
        }
    }

    #[test]
    fn zoom_in_on_equal_with_and_without_shift() {
        // Rule 2: Firefox binds the characters "+" and "=", Chromium VKEY_OEM_PLUS with and without
        // EF_SHIFT_DOWN. "+" is Shift+"=" on a US layout, so both chords are ZoomIn.
        for profile in PROFILES {
            assert_eq!(lookup(profile, code::EQUAL, CTRL, Context::default()), Some(Command::ZoomIn), "{profile:?}");
            assert_eq!(
                lookup(profile, code::EQUAL, CTRL_SHIFT, Context::default()),
                Some(Command::ZoomIn),
                "{profile:?}"
            );
            assert_eq!(lookup(profile, code::EQUAL, NONE, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::EQUAL, SHIFT, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::EQUAL, ALT, Context::default()), None, "{profile:?}");
        }
    }

    #[test]
    fn zoom_in_on_keypad_plus() {
        // The keypad + types the "+" Firefox binds; Chromium names VKEY_ADD for the same command.
        for profile in PROFILES {
            assert_eq!(lookup(profile, code::KP_PLUS, CTRL, Context::default()), Some(Command::ZoomIn), "{profile:?}");
            assert_eq!(lookup(profile, code::KP_PLUS, NONE, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::KP_PLUS, ALT, Context::default()), None, "{profile:?}");
        }
    }

    #[test]
    fn zoom_out_minus_and_keypad_minus() {
        // Firefox binds "-" and its Shift twin "_"; Chromium VKEY_OEM_MINUS with and without shift, and
        // VKEY_SUBTRACT for the keypad.
        for profile in PROFILES {
            assert_eq!(lookup(profile, code::MINUS, CTRL, Context::default()), Some(Command::ZoomOut), "{profile:?}");
            assert_eq!(
                lookup(profile, code::MINUS, CTRL_SHIFT, Context::default()),
                Some(Command::ZoomOut),
                "{profile:?}"
            );
            assert_eq!(
                lookup(profile, code::KP_MINUS, CTRL, Context::default()),
                Some(Command::ZoomOut),
                "{profile:?}"
            );
            assert_eq!(lookup(profile, code::MINUS, NONE, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::MINUS, ALT, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::KP_MINUS, CTRL_SHIFT, Context::default()), None, "{profile:?}");
        }
    }

    #[test]
    fn zoom_reset_zero_and_keypad_zero() {
        for profile in PROFILES {
            assert_eq!(lookup(profile, code::N0, CTRL, Context::default()), Some(Command::ZoomReset), "{profile:?}");
            assert_eq!(
                lookup(profile, code::KP_0, CTRL, Context::default()),
                Some(Command::ZoomReset),
                "{profile:?}"
            );
            assert_eq!(lookup(profile, code::N0, NONE, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::N0, ALT, Context::default()), None, "{profile:?}");
        }
    }

    #[test]
    fn esc_stops_only_while_loading() {
        let loading = Context { loading: true, ..Context::default() };
        for profile in PROFILES {
            assert_eq!(lookup(profile, code::ESC, NONE, loading), Some(Command::Stop), "{profile:?}");
            // Rule 3: an open find bar is not in the table, so it changes nothing in the answer - the wiring
            // lets the find bar take the key before asking, exactly as Chromium's CanCloseFind branch does.
            let find_and_loading = Context { find_bar_open: true, ..loading };
            assert_eq!(lookup(profile, code::ESC, NONE, find_and_loading), Some(Command::Stop), "{profile:?}");
            assert_eq!(lookup(profile, code::ESC, SHIFT, loading), None, "{profile:?}");
            assert_eq!(lookup(profile, code::ESC, CTRL, loading), None, "{profile:?}");
            assert_eq!(lookup(profile, code::ESC, ALT, loading), None, "{profile:?}");
        }
    }

    #[test]
    fn esc_without_loading_goes_to_page() {
        // Rule 3: idle, Esc is the page's and the window's (dialogs, fullscreen), never a table hit.
        for profile in PROFILES {
            assert_eq!(lookup(profile, code::ESC, NONE, Context::default()), None, "{profile:?}");
            let find_bar = Context { find_bar_open: true, ..Context::default() };
            assert_eq!(lookup(profile, code::ESC, NONE, find_bar), None, "{profile:?}");
            let in_field = Context { text_field_focused: true, ..Context::default() };
            assert_eq!(lookup(profile, code::ESC, NONE, in_field), None, "{profile:?}");
        }
    }

    #[test]
    fn f5_and_ctrl_r_reload_shift_variants_bypass_cache() {
        for profile in PROFILES {
            assert_eq!(lookup(profile, code::R, CTRL, Context::default()), Some(Command::Reload), "{profile:?}");
            assert_eq!(lookup(profile, code::F5, NONE, Context::default()), Some(Command::Reload), "{profile:?}");
            assert_eq!(
                lookup(profile, code::R, CTRL_SHIFT, Context::default()),
                Some(Command::ReloadBypassCache),
                "{profile:?}"
            );
            assert_eq!(
                lookup(profile, code::F5, CTRL, Context::default()),
                Some(Command::ReloadBypassCache),
                "{profile:?}"
            );
        }
        // Chromium also takes Shift+F5 for the bypass (VKEY_F5 with EF_SHIFT_DOWN); Firefox has no such entry.
        assert_eq!(lookup(Profile::Chromium, code::F5, SHIFT, Context::default()), Some(Command::ReloadBypassCache));
        assert_eq!(lookup(Profile::Firefox, code::F5, SHIFT, Context::default()), None);
        // A bare R is a typed letter; Ctrl+Alt+R is Firefox's reader mode.
        assert_eq!(lookup(Profile::Firefox, code::R, NONE, Context::default()), None);
        assert_eq!(lookup(Profile::Chromium, code::R, CTRL_ALT, Context::default()), None);
    }

    #[test]
    fn f3_and_ctrl_g_find_next_and_shift_previous() {
        for profile in PROFILES {
            assert_eq!(lookup(profile, code::F, CTRL, Context::default()), Some(Command::Find), "{profile:?}");
            assert_eq!(lookup(profile, code::G, CTRL, Context::default()), Some(Command::FindNext), "{profile:?}");
            assert_eq!(
                lookup(profile, code::F3, NONE, Context::default()),
                Some(Command::FindNext),
                "{profile:?}"
            );
            assert_eq!(
                lookup(profile, code::G, CTRL_SHIFT, Context::default()),
                Some(Command::FindPrevious),
                "{profile:?}"
            );
            assert_eq!(
                lookup(profile, code::F3, SHIFT, Context::default()),
                Some(Command::FindPrevious),
                "{profile:?}"
            );
            // Rule 3: F3 and Ctrl+G answer with the find bar open too.
            let open = Context { find_bar_open: true, ..Context::default() };
            assert_eq!(lookup(profile, code::F3, NONE, open), Some(Command::FindNext), "{profile:?}");
            assert_eq!(lookup(profile, code::F3, CTRL, open), None, "{profile:?}");
            assert_eq!(lookup(profile, code::F, ALT, Context::default()), None, "{profile:?}");
        }
        // Ctrl+Shift+F is Firefox's add-ons, Chromium's tab search is Ctrl+Shift+A: not commands of ours.
        assert_eq!(lookup(Profile::Firefox, code::F, CTRL_SHIFT, Context::default()), None);
        assert_eq!(lookup(Profile::Chromium, code::A, CTRL_SHIFT, Context::default()), None);
    }

    #[test]
    fn alt_left_right_navigate_history() {
        for profile in PROFILES {
            assert_eq!(lookup(profile, code::LEFT, ALT, Context::default()), Some(Command::Back), "{profile:?}");
            assert_eq!(lookup(profile, code::RIGHT, ALT, Context::default()), Some(Command::Forward), "{profile:?}");
            assert_eq!(lookup(profile, code::LEFT, NONE, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::RIGHT, CTRL, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::LEFT, CTRL_SHIFT, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::HOME, ALT, Context::default()), Some(Command::Home), "{profile:?}");
            // Alt+Home is the browser's; Ctrl+Home and bare Home are the page's, so they are not tabled at all.
            assert_eq!(lookup(profile, code::HOME, CTRL, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::HOME, NONE, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::END, ALT, Context::default()), None, "{profile:?}");
        }
        // Firefox's Linux-only twins of the Alt arrows, on the bracket keys; Chromium has no chord for them.
        assert_eq!(lookup(Profile::Firefox, code::BRACKETLEFT, CTRL, Context::default()), Some(Command::Back));
        assert_eq!(lookup(Profile::Firefox, code::BRACKETRIGHT, CTRL, Context::default()), Some(Command::Forward));
        assert_eq!(lookup(Profile::Chromium, code::BRACKETLEFT, CTRL, Context::default()), None);
        // Backspace: Firefox still tables VK_BACK, but cmd_handleBackspace only goes back when
        // browser.backspace_action is set to go-back behaviour, which is not the default. So it is never bound.
        assert_eq!(lookup(Profile::Firefox, code::BACKSPACE, NONE, Context::default()), None);
        assert_eq!(lookup(Profile::Firefox, code::BACKSPACE, SHIFT, Context::default()), None);
        assert_eq!(lookup(Profile::Chromium, code::BACKSPACE, CTRL_ALT, Context::default()), None);
    }

    #[test]
    fn f11_fullscreen() {
        for profile in PROFILES {
            assert_eq!(lookup(profile, code::F11, NONE, Context::default()), Some(Command::Fullscreen));
            // Firefox's Ctrl+Cmd+F branch is the macOS one; Chromium has no shifted or controlled F11.
            assert_eq!(lookup(profile, code::F11, CTRL, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::F11, CTRL_SHIFT, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::F, CTRL_ALT, Context::default()), None, "{profile:?}");
        }
    }

    #[test]
    fn f12_and_ctrl_shift_i_devtools() {
        for profile in PROFILES {
            assert_eq!(lookup(profile, code::F12, NONE, Context::default()), Some(Command::DevTools), "{profile:?}");
            assert_eq!(
                lookup(profile, code::I, CTRL_SHIFT, Context::default()),
                Some(Command::DevTools),
                "{profile:?}"
            );
            // The other devtools chords (web console, inspector, browser console) are not commands of ours.
            assert_eq!(lookup(profile, code::J, CTRL_SHIFT, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::C, CTRL_SHIFT, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::F12, SHIFT, Context::default()), None, "{profile:?}");
        }
    }

    #[test]
    fn ctrl_shift_delete_clears_data() {
        for profile in PROFILES {
            assert_eq!(
                lookup(profile, code::DELETE, CTRL_SHIFT, Context::default()),
                Some(Command::ClearBrowsingData),
                "{profile:?}"
            );
            assert_eq!(lookup(profile, code::DELETE, CTRL, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::DELETE, NONE, Context::default()), None, "{profile:?}");
            // ChromeOS clears the data on Ctrl+Shift+Backspace because its keyboards have no Delete; Linux does.
            assert_eq!(lookup(profile, code::BACKSPACE, CTRL_SHIFT, Context::default()), None, "{profile:?}");
        }
    }

    #[test]
    fn plain_letters_and_shift_letters_are_never_bound() {
        // Rule 4: without Ctrl or Alt only function keys are browser shortcuts, so every letter and digit -
        // Shift or not - reaches the page and every text field.
        for profile in PROFILES {
            for key in LETTERS.iter().chain(DIGITS.iter()) {
                assert_eq!(lookup(profile, *key, NONE, Context::default()), None, "{profile:?} code {key}");
                assert_eq!(lookup(profile, *key, SHIFT, Context::default()), None, "{profile:?} shift {key}");
            }
            // Space and Enter are the page's and the focused control's, never a browser command.
            assert_eq!(lookup(profile, code::SPACE, NONE, Context::default()), None, "{profile:?}");
            assert_eq!(lookup(profile, code::ENTER, NONE, Context::default()), None, "{profile:?}");
        }
        // Alt+letter reaches the table only for Alt+D, the address bar in both browsers; the other Alt chords
        // are arrows, Home and the digits.
        assert_eq!(lookup(Profile::Firefox, code::D, ALT, Context::default()), Some(Command::FocusAddressBar));
        assert_eq!(lookup(Profile::Chromium, code::D, ALT, Context::default()), Some(Command::FocusAddressBar));
        assert_eq!(lookup(Profile::Firefox, code::E, ALT, Context::default()), None);
        assert_eq!(lookup(Profile::Chromium, code::E, ALT, Context::default()), None);
    }

    #[test]
    fn text_field_focus_flag_changes_nothing_unless_source_says_so() {
        // Rule 4, and what the sources say: nothing. Firefox's editing keys are `internal="true"` or page
        // commands (toolkit/content/editMenuKeys.inc.xhtml: Ctrl+X, C, V, Z, A, Backspace, Delete) and never
        // reach a browser command; Chromium's cut/copy/paste are the menu-only "standard" accelerators. So no
        // binding of either profile is filtered by the caret, and Esc (the one loading-gated entry) is not
        // either: Firefox's and Chromium's stop commands do not look at focus.
        let loading = Context { loading: true, ..Context::default() };
        let busy_field = Context { loading: true, find_bar_open: true, text_field_focused: true };
        for profile in PROFILES {
            for binding in bindings(profile) {
                let idle = Context::default();
                let plain = lookup(profile, binding.code, binding.mods, idle);
                let in_field =
                    lookup(profile, binding.code, binding.mods, Context { text_field_focused: true, ..idle });
                let find_bar = lookup(profile, binding.code, binding.mods, Context { find_bar_open: true, ..idle });
                assert_eq!(in_field, plain, "{profile:?} {binding:?} differs in a text field");
                assert_eq!(find_bar, plain, "{profile:?} {binding:?} differs with the find bar open");

                // Every entry answers for its command, and only the Esc one needs the load.
                let want = Some(binding.command);
                assert_eq!(plain, if binding.command == Command::Stop { None } else { want }, "{profile:?} {binding:?}");
                assert_eq!(lookup(profile, binding.code, binding.mods, loading), want, "{profile:?} {binding:?}");
                assert_eq!(
                    lookup(profile, binding.code, binding.mods, busy_field),
                    lookup(profile, binding.code, binding.mods, loading),
                    "{profile:?} {binding:?} differs once the flags are set"
                );
            }
        }
    }

    #[test]
    fn labels_are_readable() {
        assert_eq!(first_label(Profile::Firefox, Command::ReopenClosedTab), "Ctrl+Shift+T");
        assert_eq!(first_label(Profile::Chromium, Command::ReopenClosedTab), "Ctrl+Shift+T");
        assert_eq!(first_label(Profile::Chromium, Command::NextTab), "Ctrl+Tab");
        assert_eq!(first_label(Profile::Firefox, Command::PreviousTab), "Ctrl+Shift+Tab");
        assert_eq!(first_label(Profile::Firefox, Command::Back), "Alt+Left");
        assert_eq!(first_label(Profile::Chromium, Command::Forward), "Alt+Right");
        assert_eq!(first_label(Profile::Firefox, Command::ClearBrowsingData), "Ctrl+Shift+Delete");
        assert_eq!(first_label(Profile::Firefox, Command::ZoomIn), "Ctrl+=");
        assert_eq!(first_label(Profile::Chromium, Command::ZoomOut), "Ctrl+-");
        // A key with no modifiers is just its name.
        assert_eq!(label_of(Profile::Firefox, code::F5, NONE), "F5");
        assert_eq!(label_of(Profile::Chromium, code::F11, NONE), "F11");
        assert_eq!(label_of(Profile::Firefox, code::ESC, NONE), "Esc");
        assert_eq!(label_of(Profile::Firefox, code::F3, SHIFT), "Shift+F3");
        assert_eq!(label_of(Profile::Chromium, code::KP_PLUS, CTRL), "Ctrl+KP +");
        assert_eq!(label_of(Profile::Chromium, code::KP_9, ALT), "Alt+KP 9");
        assert_eq!(label_of(Profile::Firefox, code::KP_MINUS, CTRL), "Ctrl+KP -");
        assert_eq!(label_of(Profile::Firefox, code::BRACKETRIGHT, CTRL), "Ctrl+]");
        // The order is Ctrl, Shift, Alt, whatever the table entry's fields look like.
        let odd = Binding { code: code::TAB, mods: Mods { ctrl: true, shift: true, alt: true, meta: false }, command: Command::Find };
        assert_eq!(label(&odd), "Ctrl+Shift+Alt+Tab");
    }

    #[test]
    fn binding_for_returns_first_listed() {
        for profile in PROFILES {
            for command in [
                Command::NewTab,
                Command::CloseTab,
                Command::NextTab,
                Command::PreviousTab,
                Command::SelectLastTab,
                Command::FocusAddressBar,
                Command::FocusSearch,
                Command::Find,
                Command::ShowBookmarks,
                Command::Reload,
                Command::ReloadBypassCache,
                Command::Stop,
                Command::ZoomIn,
                Command::DevTools,
            ] {
                let first = bindings(profile).iter().position(|b| b.command == command);
                let got = binding_for(profile, command);
                match first {
                    Some(i) => {
                        assert_eq!(got, bindings(profile).get(i), "{profile:?} {command:?}");
                        assert_eq!(got.map(label), Some(first_label(profile, command)), "{profile:?}");
                    }
                    None => assert_eq!(got, None, "{profile:?} {command:?}"),
                }
            }

            // The rows the menus show, which is what the order of the tables is for.
            assert_eq!(first_label(profile, Command::FocusSearch), "Ctrl+K");
            assert_eq!(first_label(profile, Command::Find), "Ctrl+F");
            assert_eq!(first_label(profile, Command::Print), "Ctrl+P");
            assert_eq!(first_label(profile, Command::Home), "Alt+Home");
            assert_eq!(first_label(profile, Command::Stop), "Esc");
        }
        // Where the two browsers part: the digit modifier, downloads, and the bookmark and history rows.
        assert_eq!(first_label(Profile::Firefox, Command::SelectTab(1)), "Alt+1");
        assert_eq!(first_label(Profile::Chromium, Command::SelectTab(1)), "Ctrl+1");
        assert_eq!(first_label(Profile::Firefox, Command::ShowDownloads), "Ctrl+Shift+Y");
        assert_eq!(first_label(Profile::Chromium, Command::ShowDownloads), "Ctrl+J");
        assert_eq!(first_label(Profile::Firefox, Command::ShowBookmarks), "Ctrl+B");
        assert_eq!(first_label(Profile::Chromium, Command::ShowBookmarks), "Ctrl+Shift+O");
        assert_eq!(first_label(Profile::Firefox, Command::NewPrivateWindow), "Ctrl+Shift+P");
        assert_eq!(first_label(Profile::Chromium, Command::NewPrivateWindow), "Ctrl+Shift+N");
        assert_eq!(first_label(Profile::Chromium, Command::CloseWindow), "Ctrl+Shift+W");
        // The one command with no binding in one profile: Chromium's IDC_EXIT has no Linux accelerator.
        assert_eq!(binding_for(Profile::Chromium, Command::Quit), None);
        assert_eq!(first_label(Profile::Firefox, Command::Quit), "Ctrl+Q");
    }
}
