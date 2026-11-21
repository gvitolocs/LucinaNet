//! From a key press to what the window does about it: `lucinanet-shortcuts` names the command, [`plan`] turns the
//! command into one of the few things the window can do today (docs/specs/wiring-shortcuts-zoom.md). Pure, so the
//! mapping is tested without a window or an engine.

use lucina_app::keys::Press;
use lucinanet_shortcuts::{Command, Context, Mods, Profile};

use crate::ZoomStep;
use crate::TabInfo;

/// Whose defaults the keys follow. Firefox: its zoom table (20..500 %) is the one `lucinanet-zoom` is read against
/// in the batch notes, and its Linux key table is a superset of what the strip already did (Ctrl+T/W/Tab).
pub const PROFILE: Profile = Profile::Firefox;

/// What the window does for a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Select the address and give it the keys.
    FocusAddress,
    NewTab,
    CloseTab(u32),
    SelectTab(u32),
    Back,
    Forward,
    Reload,
    Zoom(ZoomStep),
    /// Bookmark the shown page, or remove its bookmarks (Ctrl+D, and the star).
    ToggleBookmark,
    /// End the browser. Not a way out of its own: the desktop host already exits on this flag for a window close.
    Quit,
    /// The browser owns the shortcut but has no feature behind it yet: the press is taken and nothing happens.
    Unwired,
    /// Not the browser's now: the press goes on to the page. Only Esc while loading, until a stop exists.
    PassToPage,
}

/// The command a press is, with the context the window is in. `None` means the press is not a shortcut.
pub fn command(press: Press, loading: bool, text_field_focused: bool) -> Option<Command> {
    let mods = Mods {
        ctrl: press.mods.ctrl,
        shift: press.mods.shift,
        alt: press.mods.alt,
        meta: press.mods.meta,
    };
    let cx = Context { loading, find_bar_open: false, text_field_focused };
    lucinanet_shortcuts::lookup(PROFILE, press.code, mods, cx)
}

/// What the window does for `command`: `tabs` in strip order, the shown tab's id, and whether it can go back
/// and forward (the buttons are greyed out the same way).
///
/// Every command the table can name appears here, so adding a feature means changing one arm. The ones with no
/// feature behind them today are [`UNWIRED`].
pub fn plan(command: Command, tabs: &[TabInfo], active: u32, back: bool, forward: bool) -> Action {
    match command {
        Command::NewTab => Action::NewTab,
        Command::CloseTab => Action::CloseTab(active),
        Command::NextTab | Command::PreviousTab => {
            let step = if command == Command::NextTab { 1 } else { -1 };
            match neighbour(tabs, active, step) {
                Some(id) => Action::SelectTab(id),
                None => Action::Unwired,
            }
        },
        Command::SelectTab(n) => match tabs.get(usize::from(n).wrapping_sub(1)) {
            Some(tab) => Action::SelectTab(tab.id),
            // Fewer tabs than the number: nothing to select, as in both browsers.
            None => Action::Unwired,
        },
        Command::SelectLastTab => match tabs.last() {
            Some(tab) => Action::SelectTab(tab.id),
            None => Action::Unwired,
        },
        // Chromium's Ctrl+K and Firefox's Ctrl+J/K go to a search field; here the address bar searches, so it is
        // the same place (PORTING.md of the wiring, adapt).
        Command::FocusAddressBar | Command::FocusSearch => Action::FocusAddress,
        Command::Back if back => Action::Back,
        Command::Forward if forward => Action::Forward,
        Command::Back | Command::Forward => Action::Unwired,
        // The engine reloads from the cache rules it has; there is no bypass switch to flip yet.
        Command::Reload | Command::ReloadBypassCache => Action::Reload,
        Command::ZoomIn => Action::Zoom(ZoomStep::In),
        Command::ZoomOut => Action::Zoom(ZoomStep::Out),
        Command::ZoomReset => Action::Zoom(ZoomStep::Reset),
        // Both browsers add a new page and, on a bookmarked one, offer its removal; here the key is a toggle like the star.
        Command::BookmarkPage => Action::ToggleBookmark,
        // The browser's own end. Closing the window is not this: one window, and its close is the same exit already.
        Command::Quit => Action::Quit,
        // Esc stops a load in both browsers; the engine has no stop, so the page keeps the key.
        Command::Stop => Action::PassToPage,
        _ => Action::Unwired,
    }
}

/// The commands that are matched and taken but do nothing yet, for the document and its test.
pub const UNWIRED: &[Command] = &[
    Command::ReopenClosedTab,
    Command::NewWindow,
    Command::NewPrivateWindow,
    Command::CloseWindow,
    Command::Find,
    Command::FindNext,
    Command::FindPrevious,
    Command::ShowBookmarks,
    Command::ShowHistory,
    Command::ShowDownloads,
    Command::ClearBrowsingData,
    Command::Home,
    Command::Fullscreen,
    Command::Print,
    Command::SavePage,
    Command::ViewSource,
    Command::DevTools,
];

fn neighbour(tabs: &[TabInfo], active: u32, step: i32) -> Option<u32> {
    let at = tabs.iter().position(|t| t.id == active)? as i32;
    let n = tabs.len() as i32;
    Some(tabs[(at + step).rem_euclid(n) as usize].id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_app::keys::{self, Mods as KeyMods};
    use lucinanet_shortcuts::code;

    fn tabs(n: u32) -> Vec<TabInfo> {
        (1..=n)
            .map(|id| TabInfo { id, title: String::new(), url: String::new(), loading: false, registrable_domain: None })
            .collect()
    }

    fn press(code: u16, ctrl: bool, shift: bool, alt: bool) -> Press {
        Press { code, mods: KeyMods { ctrl, shift, alt, ..KeyMods::default() } }
    }

    /// The codes `lucinanet-shortcuts` and `lucina_app::keys` (at the pinned rev) both name must agree: the first is
    /// what the tables are written in, the second is what the toolkit and the desktop window hand over. The rest of
    /// the shortcut codes are held to the kernel header in `lucinanet-shortcuts`' own test.
    #[test]
    fn the_shortcut_codes_are_the_toolkits() {
        let pairs: &[(&str, u16, u16)] = &[
            ("ESC", code::ESC, keys::KEY_ESC),
            ("1", code::N1, keys::KEY_1),
            ("2", code::N2, keys::KEY_2),
            ("3", code::N3, keys::KEY_3),
            ("BACKSPACE", code::BACKSPACE, keys::KEY_BACKSPACE),
            ("TAB", code::TAB, keys::KEY_TAB),
            ("ENTER", code::ENTER, keys::KEY_ENTER),
            ("A", code::A, keys::KEY_A),
            ("X", code::X, keys::KEY_X),
            ("C", code::C, keys::KEY_C),
            ("V", code::V, keys::KEY_V),
            ("SPACE", code::SPACE, keys::KEY_SPACE),
            ("F1", code::F1, keys::KEY_F1),
            ("F2", code::F2, keys::KEY_F2),
            ("F3", code::F3, keys::KEY_F3),
            ("HOME", code::HOME, keys::KEY_HOME),
            ("UP", code::UP, keys::KEY_UP),
            ("PAGEUP", code::PAGE_UP, keys::KEY_PAGEUP),
            ("LEFT", code::LEFT, keys::KEY_LEFT),
            ("RIGHT", code::RIGHT, keys::KEY_RIGHT),
            ("END", code::END, keys::KEY_END),
            ("DOWN", code::DOWN, keys::KEY_DOWN),
            ("PAGEDOWN", code::PAGE_DOWN, keys::KEY_PAGEDOWN),
            ("DELETE", code::DELETE, keys::KEY_DELETE),
        ];
        for &(name, ours, theirs) in pairs {
            assert_eq!(ours, theirs, "KEY_{name}");
        }
    }

    #[test]
    fn presses_become_commands_through_the_table() {
        let c = |code, ctrl, shift, alt| command(press(code, ctrl, shift, alt), false, false);
        assert_eq!(c(code::T, true, false, false), Some(Command::NewTab));
        assert_eq!(c(code::T, true, true, false), Some(Command::ReopenClosedTab));
        assert_eq!(c(code::L, true, false, false), Some(Command::FocusAddressBar));
        assert_eq!(c(code::D, false, false, true), Some(Command::FocusAddressBar));
        assert_eq!(c(code::EQUAL, true, false, false), Some(Command::ZoomIn));
        assert_eq!(c(code::KP_MINUS, true, false, false), Some(Command::ZoomOut));
        assert_eq!(c(code::N0, true, false, false), Some(Command::ZoomReset));
        assert_eq!(c(code::LEFT, false, false, true), Some(Command::Back));
        // A bare letter, Ctrl+C and Ctrl+Alt+T are the page's or the field's.
        assert_eq!(c(code::T, false, false, false), None);
        assert_eq!(c(code::C, true, false, false), None);
        assert_eq!(c(code::T, true, false, true), None);
        // Super is the desktop's.
        let mut meta = press(code::T, true, false, false);
        meta.mods.meta = true;
        assert_eq!(command(meta, false, false), None);
        // Esc is the browser's only while a load runs.
        assert_eq!(command(press(code::ESC, false, false, false), false, false), None);
        assert_eq!(command(press(code::ESC, false, false, false), true, false), Some(Command::Stop));
    }

    /// What `tabs::tab_key` answered before the table took over (its test is gone with it): the same presses give the
    /// same four tab commands, and the presses it refused are still not those commands.
    #[test]
    fn the_old_tab_keys_mean_the_same() {
        let c = |code, ctrl, shift, alt| command(press(code, ctrl, shift, alt), false, false);
        assert_eq!(c(code::T, true, false, false), Some(Command::NewTab));
        assert_eq!(c(code::W, true, false, false), Some(Command::CloseTab));
        assert_eq!(c(code::TAB, true, false, false), Some(Command::NextTab));
        assert_eq!(c(code::TAB, true, true, false), Some(Command::PreviousTab));
        assert_eq!(c(code::T, false, false, false), None);
        assert_eq!(c(code::TAB, false, false, false), None);
        assert_eq!(c(code::TAB, false, true, false), None);
        assert_eq!(c(code::T, true, false, true), None);
        let mut meta = press(code::T, true, false, false);
        meta.mods.meta = true;
        assert_eq!(command(meta, false, false), None);
        // Ctrl+Shift+T and Ctrl+Shift+W were nothing before; they are the browsers' reopen-tab and close-window now,
        // neither of them new/close tab.
        assert_eq!(c(code::T, true, true, false), Some(Command::ReopenClosedTab));
        assert_eq!(c(code::W, true, true, false), Some(Command::CloseWindow));
    }

    #[test]
    fn tab_commands_pick_tabs_by_position() {
        let t = tabs(3);
        let go = |command, active| plan(command, &t, active, false, false);
        assert_eq!(go(Command::NewTab, 1), Action::NewTab);
        assert_eq!(go(Command::CloseTab, 2), Action::CloseTab(2));
        assert_eq!(go(Command::NextTab, 3), Action::SelectTab(1));
        assert_eq!(go(Command::PreviousTab, 1), Action::SelectTab(3));
        assert_eq!(go(Command::SelectTab(2), 1), Action::SelectTab(2));
        assert_eq!(go(Command::SelectTab(8), 1), Action::Unwired);
        assert_eq!(go(Command::SelectLastTab, 1), Action::SelectTab(3));
        // The strip's order, not the ids: ids are never reused, so they are not positions.
        let mut shuffled = tabs(3);
        shuffled[0].id = 7;
        shuffled[1].id = 4;
        shuffled[2].id = 9;
        assert_eq!(plan(Command::SelectTab(2), &shuffled, 7, false, false), Action::SelectTab(4));
        assert_eq!(plan(Command::NextTab, &shuffled, 9, false, false), Action::SelectTab(7));
        assert_eq!(plan(Command::SelectLastTab, &shuffled, 7, false, false), Action::SelectTab(9));
        // An active id that is not in the strip (the news has not arrived) selects nothing.
        assert_eq!(plan(Command::NextTab, &shuffled, 1, false, false), Action::Unwired);
        assert_eq!(plan(Command::SelectLastTab, &[], 1, false, false), Action::Unwired);
    }

    #[test]
    fn navigation_follows_the_buttons() {
        let t = tabs(1);
        assert_eq!(plan(Command::Back, &t, 1, false, true), Action::Unwired);
        assert_eq!(plan(Command::Back, &t, 1, true, false), Action::Back);
        assert_eq!(plan(Command::Forward, &t, 1, true, false), Action::Unwired);
        assert_eq!(plan(Command::Forward, &t, 1, false, true), Action::Forward);
        assert_eq!(plan(Command::Reload, &t, 1, false, false), Action::Reload);
        assert_eq!(plan(Command::ReloadBypassCache, &t, 1, false, false), Action::Reload);
        assert_eq!(plan(Command::FocusSearch, &t, 1, false, false), Action::FocusAddress);
        assert_eq!(plan(Command::FocusAddressBar, &t, 1, false, false), Action::FocusAddress);
    }

    #[test]
    fn ctrl_d_is_the_star() {
        let c = |code, ctrl, shift, alt| command(press(code, ctrl, shift, alt), false, false);
        assert_eq!(c(code::D, true, false, false), Some(Command::BookmarkPage));
        assert_eq!(plan(Command::BookmarkPage, &tabs(1), 1, false, false), Action::ToggleBookmark);
        assert!(!UNWIRED.contains(&Command::BookmarkPage), "wired, so not listed as doing nothing");
        // The bookmarks list is still a page that does not exist.
        assert!(UNWIRED.contains(&Command::ShowBookmarks));
        assert_eq!(plan(Command::ShowBookmarks, &tabs(1), 1, false, false), Action::Unwired);
    }

    #[test]
    fn zoom_commands_become_steps() {
        let t = tabs(1);
        assert_eq!(plan(Command::ZoomIn, &t, 1, false, false), Action::Zoom(ZoomStep::In));
        assert_eq!(plan(Command::ZoomOut, &t, 1, false, false), Action::Zoom(ZoomStep::Out));
        assert_eq!(plan(Command::ZoomReset, &t, 1, false, false), Action::Zoom(ZoomStep::Reset));
    }

    /// Ctrl+Q is the browser's own end; closing a window is not, and stays a press that does nothing.
    #[test]
    fn ctrl_q_quits_and_a_window_close_still_does_nothing() {
        assert_eq!(PROFILE, Profile::Firefox);
        let c = |code, ctrl, shift, alt| command(press(code, ctrl, shift, alt), false, false);
        assert_eq!(c(code::Q, true, false, false), Some(Command::Quit));
        let t = tabs(1);
        let go = |command| plan(command, &t, 1, false, false);
        assert_eq!(go(Command::Quit), Action::Quit);
        assert!(!UNWIRED.contains(&Command::Quit), "wired, so not listed");
        assert_eq!(c(code::W, true, true, false), Some(Command::CloseWindow));
        assert!(UNWIRED.contains(&Command::CloseWindow));
        assert_eq!(go(Command::CloseWindow), Action::Unwired);
    }

    /// Every command the firefox table can produce has a place: wired, or on the unwired list, and the two lists
    /// do not overlap. A command added to the crate later fails here until someone decides what it does.
    #[test]
    fn every_command_in_the_table_is_wired_or_listed_unwired() {
        let t = tabs(2);
        for binding in lucinanet_shortcuts::bindings(PROFILE) {
            let action = plan(binding.command, &t, 1, true, true);
            let listed = UNWIRED.contains(&binding.command);
            match action {
                Action::Unwired => {
                    // Unwired by state (no such tab, nothing to go back to) or by list; never silently by accident.
                    let by_state = matches!(
                        binding.command,
                        Command::SelectTab(_) | Command::SelectLastTab | Command::NextTab | Command::PreviousTab
                    );
                    assert!(listed || by_state, "{:?} does nothing and is not listed", binding.command);
                },
                _ => assert!(!listed, "{:?} is wired and listed unwired", binding.command),
            }
        }
    }
}
