//! From a key press to what the window does about it: `lucinanet-shortcuts` names the command, [`plan`] turns the
//! command into one of the few things the window can do today (the shortcuts/zoom wiring spec). Pure, so the
//! mapping is tested without a window or an engine.

use lucina_app::keys::Press;
use lucinanet_shortcuts::{Command, Context, Mods, Profile};

use crate::ZoomStep;
use crate::TabInfo;

/// Whose defaults the keys follow. Firefox: its zoom table (20..500 %) is the one `lucinanet-zoom` is read against
/// in the porting notes, and its Linux key table is a superset of what the strip already did (Ctrl+T/W/Tab).
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
    /// The find bar goes up (Ctrl+F).
    FindOpen,
    /// The next match of what the find bar holds.
    FindNext,
    /// The match before the current one.
    FindPrev,
    /// The find bar goes down: Esc was the bar's own key, not the load's or the page's.
    FindClose,
    /// The downloads panel goes up over the page, or down if it was already up (Ctrl+Shift+Y). What it shows is
    /// what the chrome already holds, so nothing is asked of the engine.
    ShowDownloads,
    /// The downloads panel goes down: Esc was the panel's own key, not the load's or the page's. The history panel
    /// goes down the same way (one of the two is up at a time), so the one Esc closes whichever is.
    CloseDownloads,
    /// The history panel goes up over the page, or down if it was already up (Ctrl+H). The list is the engine's: the
    /// window asks for it when the panel goes up.
    ShowHistory,
    /// The bookmarks panel goes up over the page, or down if it was already up (Ctrl+Shift+O, Ctrl+B). The list is the
    /// engine's: the window asks for it when the panel goes up.
    ShowBookmarks,
    /// End the browser. Not a way out of its own: the desktop host already exits on this flag for a window close.
    Quit,
    /// The browser owns the shortcut but has no feature behind it yet: the press is taken and nothing happens.
    Unwired,
    /// Not the browser's now: the press goes on to the page. Only Esc while a load runs and no overlay of the
    /// window's own is up, until a stop exists.
    PassToPage,
}

/// The command a press is, with the context the window is in: a load in flight, the find bar up or down, a text
/// field holding the keys, the downloads panel up or down. `None` means the press is not a shortcut.
pub fn command(press: Press, loading: bool, find_bar_open: bool, text_field_focused: bool, downloads_open: bool) -> Option<Command> {
    let mods = Mods {
        ctrl: press.mods.ctrl,
        shift: press.mods.shift,
        alt: press.mods.alt,
        meta: press.mods.meta,
    };
    let found = lucinanet_shortcuts::lookup(PROFILE, press.code, mods, context(loading, find_bar_open, text_field_focused));
    // Esc with the panel up is the browser's own even with no load in flight, which the table cannot say: it names
    // the key `Stop` and enables it only while a load runs, and a panel opened on demand has nothing to stop.
    // Chromium's `IDC_CLOSE_FIND_OR_STOP` is the precedent for the shape of it - the command is enabled by
    // `CanCloseFind(...) || stop enabled`, so an open overlay makes the key the browser's on its own. The command
    // is the same one the bar's state is read against; `plan` decides between the two overlays.
    if found.is_none() && downloads_open && press.code == lucinanet_shortcuts::code::ESC && mods == Mods::default() {
        return Some(Command::Stop);
    }
    found
}

/// What the window is doing, as the matcher sees it. The find bar's state changes no answer of the table - it has
/// no find-bar rows in either profile, and `lucinanet-shortcuts` tests that - but [`plan`] reads the same state to
/// give Esc to the bar rather than to the load, so the window hands its real state over rather than a pinned false.
pub fn context(loading: bool, find_bar_open: bool, text_field_focused: bool) -> Context {
    Context { loading, find_bar_open, text_field_focused }
}

/// What the window does for `command`: the find bar and the downloads panel up or down, `tabs` in strip order, the
/// shown tab's id, and whether it can go back and forward (the buttons are greyed out the same way).
///
/// Every command the table can name appears here, so adding a feature means changing one arm. The ones with no
/// feature behind them today are [`UNWIRED`].
pub fn plan(command: Command, find_bar_open: bool, downloads_open: bool, tabs: &[TabInfo], active: u32, back: bool, forward: bool) -> Action {
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
        // Ctrl+F opens the find bar, the other two move through its matches. The bar and the matches are the next
        // change; the keys stop being the unwired bucket here.
        Command::Find => Action::FindOpen,
        Command::FindNext => Action::FindNext,
        Command::FindPrevious => Action::FindPrev,
        // The downloads panel: the key puts the chrome's own rows up over the page and, once up, the same key is
        // the way out. It asks the engine for nothing, so no state but the window's own decides it.
        Command::ShowDownloads => Action::ShowDownloads,
        // The history panel is the same kind of view, with a list the engine reads from its store when it goes up.
        Command::ShowHistory => Action::ShowHistory,
        // And the bookmarks panel: the same view over the bookmarks the engine lists from its local store.
        Command::ShowBookmarks => Action::ShowBookmarks,
        // The browser's own end. Closing the window is not this: one window, and its close is the same exit already.
        Command::Quit => Action::Quit,
        // Esc with the bar up is the bar's close before it is anything else: Chromium's `IDC_CLOSE_FIND_OR_STOP`
        // runs its `CanCloseFind` branch ahead of the stop, and Firefox's findbar handles the key in its widget
        // rather than in a `<key>`. The table cannot say it, having no find-bar row, so the state says it here.
        Command::Stop if find_bar_open => Action::FindClose,
        // Then the panel's, in the order Chromium's `ExecuteCloseFindOrStop` tries the two: `CanCloseFind` ahead of
        // `CanCloseDownloads`. Both states are the window's, neither table having a row for either overlay.
        Command::Stop if downloads_open => Action::CloseDownloads,
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
        let c = |code, ctrl, shift, alt| command(press(code, ctrl, shift, alt), false, false, false, false);
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
        assert_eq!(command(meta, false, false, false, false), None);
        // Esc is the browser's only while a load runs (the downloads panel, tested below, is the other thing that
        // makes it the browser's with no load in flight).
        assert_eq!(command(press(code::ESC, false, false, false), false, false, false, false), None);
        assert_eq!(command(press(code::ESC, false, false, false), true, false, false, false), Some(Command::Stop));
    }

    /// What `tabs::tab_key` answered before the table took over (its test is gone with it): the same presses give the
    /// same four tab commands, and the presses it refused are still not those commands.
    #[test]
    fn the_old_tab_keys_mean_the_same() {
        let c = |code, ctrl, shift, alt| command(press(code, ctrl, shift, alt), false, false, false, false);
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
        assert_eq!(command(meta, false, false, false, false), None);
        // Ctrl+Shift+T and Ctrl+Shift+W were nothing before; they are the browsers' reopen-tab and close-window now,
        // neither of them new/close tab.
        assert_eq!(c(code::T, true, true, false), Some(Command::ReopenClosedTab));
        assert_eq!(c(code::W, true, true, false), Some(Command::CloseWindow));
    }

    #[test]
    fn tab_commands_pick_tabs_by_position() {
        let t = tabs(3);
        let go = |command, active| plan(command, false, false, &t, active, false, false);
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
        assert_eq!(plan(Command::SelectTab(2), false, false, &shuffled, 7, false, false), Action::SelectTab(4));
        assert_eq!(plan(Command::NextTab, false, false, &shuffled, 9, false, false), Action::SelectTab(7));
        assert_eq!(plan(Command::SelectLastTab, false, false, &shuffled, 7, false, false), Action::SelectTab(9));
        // An active id that is not in the strip (the news has not arrived) selects nothing.
        assert_eq!(plan(Command::NextTab, false, false, &shuffled, 1, false, false), Action::Unwired);
        assert_eq!(plan(Command::SelectLastTab, false, false, &[], 1, false, false), Action::Unwired);
    }

    #[test]
    fn navigation_follows_the_buttons() {
        let t = tabs(1);
        assert_eq!(plan(Command::Back, false, false, &t, 1, false, true), Action::Unwired);
        assert_eq!(plan(Command::Back, false, false, &t, 1, true, false), Action::Back);
        assert_eq!(plan(Command::Forward, false, false, &t, 1, true, false), Action::Unwired);
        assert_eq!(plan(Command::Forward, false, false, &t, 1, false, true), Action::Forward);
        assert_eq!(plan(Command::Reload, false, false, &t, 1, false, false), Action::Reload);
        assert_eq!(plan(Command::ReloadBypassCache, false, false, &t, 1, false, false), Action::Reload);
        assert_eq!(plan(Command::FocusSearch, false, false, &t, 1, false, false), Action::FocusAddress);
        assert_eq!(plan(Command::FocusAddressBar, false, false, &t, 1, false, false), Action::FocusAddress);
    }

    #[test]
    fn ctrl_d_is_the_star() {
        let c = |code, ctrl, shift, alt| command(press(code, ctrl, shift, alt), false, false, false, false);
        assert_eq!(c(code::D, true, false, false), Some(Command::BookmarkPage));
        assert_eq!(plan(Command::BookmarkPage, false, false, &tabs(1), 1, false, false), Action::ToggleBookmark);
        assert!(!UNWIRED.contains(&Command::BookmarkPage), "wired, so not listed as doing nothing");
        // The bookmarks list is wired now; its test is `the_show_bookmarks_command_opens_the_panel`.
        assert!(!UNWIRED.contains(&Command::ShowBookmarks));
    }

    #[test]
    fn zoom_commands_become_steps() {
        let t = tabs(1);
        assert_eq!(plan(Command::ZoomIn, false, false, &t, 1, false, false), Action::Zoom(ZoomStep::In));
        assert_eq!(plan(Command::ZoomOut, false, false, &t, 1, false, false), Action::Zoom(ZoomStep::Out));
        assert_eq!(plan(Command::ZoomReset, false, false, &t, 1, false, false), Action::Zoom(ZoomStep::Reset));
    }

    /// The find keys are the browser's and something is behind them: each plans a find action, and none is left on
    /// the list of commands that are taken and do nothing.
    #[test]
    fn find_commands_are_wired_not_unwired() {
        let t = tabs(1);
        for command in [Command::Find, Command::FindNext, Command::FindPrevious] {
            assert_ne!(plan(command, false, false, &t, 1, false, false), Action::Unwired, "{command:?} plans nothing at all");
            assert!(!UNWIRED.contains(&command), "{command:?} is listed as taken and doing nothing");
        }
    }

    /// The downloads panel is wired the way the find bar is: the command plans the view rather than nothing, so it
    /// leaves the list of commands that are taken and do nothing, while the two list commands that have no view yet
    /// stay on it. Esc is the panel's own close with no load in flight, which the table cannot say, and the find bar
    /// keeps the key ahead of it.
    #[test]
    fn the_show_downloads_command_opens_and_closes_the_panel() {
        let t = tabs(1);
        let go = |command| plan(command, false, false, &t, 1, false, false);
        assert_eq!(go(Command::ShowDownloads), Action::ShowDownloads, "the command opens the view");
        assert!(!UNWIRED.contains(&Command::ShowDownloads), "wired, so not listed as taken and doing nothing");
        // Commands with no view yet stay taken and empty (the bookmarks one is wired; reopen-closed-tab is
        // the example that is left).
        for command in [Command::ReopenClosedTab] {
            assert!(UNWIRED.contains(&command), "{command:?} has no view to open yet");
            assert_eq!(go(command), Action::Unwired, "{command:?} still does nothing");
        }

        // Ctrl+Shift+Y is the downloads key of this profile (Firefox's `key_openDownloads`; its Ctrl+J is the search
        // bar, which is the address field here).
        assert_eq!(
            command(press(code::Y, true, true, false), false, false, false, false),
            Some(Command::ShowDownloads),
            "the panel key is the browser's with nothing loading"
        );

        // The panel's state reaches both reads of a press, so Esc is the panel's own close whether or not a load is
        // in flight - the table names Esc a Stop only while one is, and a panel opened on demand has nothing to stop.
        let esc = |loading, find_bar_open, downloads_open| {
            let found = command(press(code::ESC, false, false, false), loading, find_bar_open, false, downloads_open);
            found.map(|command| plan(command, find_bar_open, downloads_open, &t, 1, false, false))
        };
        assert_eq!(esc(false, false, true), Some(Action::CloseDownloads), "the open panel takes Esc with no load");
        assert_eq!(esc(true, false, true), Some(Action::CloseDownloads), "and with a load in flight");
        assert_eq!(esc(true, true, true), Some(Action::FindClose), "the bar is closed before the panel");
        assert_eq!(esc(false, false, false), None, "with neither up and nothing loading the page keeps Esc");
        assert_eq!(esc(true, false, false), Some(Action::PassToPage), "a load with no overlay keeps the old answer");
        // Only the bare key: a chord the table does not name is not the panel's either.
        assert_eq!(command(press(code::ESC, true, false, false), false, false, false, true), None, "Ctrl+Esc");
    }

    /// the history panel is wired the way the downloads panel is: the command plans the view rather than
    /// nothing and leaves the list of commands that are taken and do nothing; the bookmarks list stays on it. Ctrl+H
    /// is the history key of the profile; the open panel takes Esc whether or not a load is in flight.
    #[test]
    fn the_show_history_command_opens_the_panel() {
        let t = tabs(1);
        let go = |command| plan(command, false, false, &t, 1, false, false);
        assert_eq!(go(Command::ShowHistory), Action::ShowHistory, "the command opens the view");
        assert!(!UNWIRED.contains(&Command::ShowHistory), "wired, so not listed as taken and doing nothing");
        assert!(!UNWIRED.contains(&Command::ShowBookmarks), "the bookmarks view is wired too");
        assert_eq!(
            command(press(code::H, true, false, false), false, false, false, false),
            Some(Command::ShowHistory),
            "Ctrl+H is the history key"
        );
        // The window hands the one flag for "a panel is up" to both reads, so Esc closes the history panel too.
        let esc = command(press(code::ESC, false, false, false), false, false, false, true).unwrap();
        assert_eq!(plan(esc, false, true, &t, 1, false, false), Action::CloseDownloads, "Esc closes the panel that is up");
    }

    /// the bookmarks panel is wired like the history one. The Firefox profile binds Ctrl+B and Ctrl+Shift+O to it;
    /// Esc closes it through the same one flag for "a panel is up".
    #[test]
    fn the_show_bookmarks_command_opens_the_panel() {
        let t = tabs(1);
        let go = |command| plan(command, false, false, &t, 1, false, false);
        assert_eq!(go(Command::ShowBookmarks), Action::ShowBookmarks, "the command opens the view");
        assert!(!UNWIRED.contains(&Command::ShowBookmarks), "wired, so not listed as taken and doing nothing");
        for (code, ctrl, shift) in [(code::B, true, false), (code::O, true, true)] {
            assert_eq!(
                command(press(code, ctrl, shift, false), false, false, false, false),
                Some(Command::ShowBookmarks),
                "{code:?} shift {shift}"
            );
        }
        let esc = command(press(code::ESC, false, false, false), false, false, false, true).unwrap();
        assert_eq!(plan(esc, false, true, &t, 1, false, false), Action::CloseDownloads, "Esc closes the panel that is up");
    }

    /// The find bar's state reaches the matcher's context and the plan instead of being pinned to closed. The
    /// table answers the same press either way - `lucinanet-shortcuts` has no find-bar rows and tests that the
    /// flag changes nothing in `lookup` - so the flag decides what the *window* does with that answer: with the
    /// bar up Esc is the bar's own close, before it is the load's stop.
    #[test]
    fn command_find_bar_flag_reaches_context() {
        let t = tabs(1);
        let esc = |find_bar_open| command(press(code::ESC, false, false, false), true, find_bar_open, false, false);
        assert_eq!(esc(true), esc(false), "the same command with the bar up or down");
        assert_eq!(esc(true), Some(Command::Stop));
        let go = |find_bar_open| plan(Command::Stop, find_bar_open, false, &t, 1, false, false);
        assert_eq!(go(true), Action::FindClose, "the open bar takes Esc");
        assert_eq!(go(false), Action::PassToPage, "with no bar the page keeps it");

        // The context a press is answered in carries each flag in its own place; the bar's state is the window's,
        // not a pinned false.
        let cx = context(true, true, true);
        assert!(cx.loading && cx.find_bar_open && cx.text_field_focused, "{cx:?} lost a flag");
        assert!(context(true, true, false).find_bar_open, "up when the window says it is up");
        assert!(!context(true, false, true).find_bar_open, "down when the window says it is down");

        // The repeat is the browser's and a repeat with the bar up or down, and the caret in the bar's own field
        // does not take it away; neither does Ctrl+F stop being the open.
        for find_bar_open in [false, true] {
            let field = find_bar_open;
            let c = |key, shift| command(press(key, false, shift, false), false, find_bar_open, field, false);
            assert_eq!(c(code::F3, false), Some(Command::FindNext), "F3, bar {find_bar_open}");
            assert_eq!(c(code::F3, true), Some(Command::FindPrevious), "Shift+F3, bar {find_bar_open}");
            assert_eq!(
                command(press(code::G, true, false, false), false, find_bar_open, field, false),
                Some(Command::FindNext),
                "Ctrl+G, bar {find_bar_open}"
            );
            assert_eq!(
                command(press(code::F, true, false, false), false, find_bar_open, field, false),
                Some(Command::Find),
                "Ctrl+F, bar {find_bar_open}"
            );
            let plan_with = |command| plan(command, find_bar_open, false, &t, 1, false, false);
            assert_eq!(plan_with(Command::Find), Action::FindOpen);
            assert_eq!(plan_with(Command::FindNext), Action::FindNext);
            assert_eq!(plan_with(Command::FindPrevious), Action::FindPrev);
        }
    }

    /// Ctrl+Q is the browser's own end; closing a window is not, and stays a press that does nothing.
    #[test]
    fn ctrl_q_quits_and_a_window_close_still_does_nothing() {
        assert_eq!(PROFILE, Profile::Firefox);
        let c = |code, ctrl, shift, alt| command(press(code, ctrl, shift, alt), false, false, false, false);
        assert_eq!(c(code::Q, true, false, false), Some(Command::Quit));
        let t = tabs(1);
        let go = |command| plan(command, false, false, &t, 1, false, false);
        assert_eq!(go(Command::Quit), Action::Quit);
        assert!(!UNWIRED.contains(&Command::Quit), "wired, so not listed");
        assert_eq!(c(code::W, true, true, false), Some(Command::CloseWindow));
        assert!(UNWIRED.contains(&Command::CloseWindow));
        assert_eq!(go(Command::CloseWindow), Action::Unwired);
    }

    /// Every command the firefox table can produce has a place: wired, or on the unwired list, and the two lists
    /// do not overlap. A command added to the crate later fails here until someone decides what it does. The find
    /// bar's and the panel's two states are both read, so a command that only does something with one of them up is
    /// not silently counted as doing nothing.
    #[test]
    fn every_command_in_the_table_is_wired_or_listed_unwired() {
        let t = tabs(2);
        for (find_bar_open, downloads_open) in [(false, false), (false, true), (true, false), (true, true)] {
            for binding in lucinanet_shortcuts::bindings(PROFILE) {
                let action = plan(binding.command, find_bar_open, downloads_open, &t, 1, true, true);
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
}
