/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! What changed between two [`Settings`], as the dotted paths the wiring listens to.
//!
//! The paths are the file's own keys, so a listener can match on the string it would
//! have written in the JSON. Everything is compared, including the unknown keys, which
//! arrive here as one path: [`Settings::unknown`] is a map of values and the return
//! type is `&'static str`, so a name built out of a key found in a file cannot be
//! returned. A change inside it is reported as `"unknown"`, and the caller re-reads the
//! map itself.

use super::*;

impl Settings {
    /// The dotted paths whose values differ between `self` and `other`, sorted.
    ///
    /// The comparison is symmetric, so the answer does not depend on which settings are
    /// `self`; the list is sorted so a caller can show or log it without sorting again.
    pub fn changed_keys(&self, other: &Settings) -> Vec<&'static str> {
        macro_rules! note {
            ($out:ident, $path:literal, $left:expr, $right:expr) => {
                if $left != $right {
                    $out.push($path);
                }
            };
        }

        let mut changed: Vec<&'static str> = Vec::new();

        note!(changed, "general.startup", self.general.startup, other.general.startup);
        note!(changed, "general.home_page", self.general.home_page, other.general.home_page);
        note!(changed,
            "general.accept_languages",
            self.general.accept_languages,
            other.general.accept_languages
        );

        note!(changed,
            "search.default_engine",
            self.search.default_engine,
            other.search.default_engine
        );
        note!(changed, "search.engines", self.search.engines, other.search.engines);
        note!(changed,
            "search.suggest_remote",
            self.search.suggest_remote,
            other.search.suggest_remote
        );
        note!(changed,
            "search.suggest_history",
            self.search.suggest_history,
            other.search.suggest_history
        );
        note!(changed,
            "search.suggest_bookmarks",
            self.search.suggest_bookmarks,
            other.search.suggest_bookmarks
        );

        note!(changed,
            "privacy.do_not_track",
            self.privacy.do_not_track,
            other.privacy.do_not_track
        );
        note!(changed, "privacy.cookies", self.privacy.cookies, other.privacy.cookies);
        note!(changed,
            "privacy.content_blocking",
            self.privacy.content_blocking,
            other.privacy.content_blocking
        );
        note!(changed,
            "privacy.clear_on_exit.history",
            self.privacy.clear_on_exit.history,
            other.privacy.clear_on_exit.history
        );
        note!(changed,
            "privacy.clear_on_exit.cookies",
            self.privacy.clear_on_exit.cookies,
            other.privacy.clear_on_exit.cookies
        );
        note!(changed,
            "privacy.clear_on_exit.cache",
            self.privacy.clear_on_exit.cache,
            other.privacy.clear_on_exit.cache
        );
        note!(changed,
            "privacy.clear_on_exit.downloads_list",
            self.privacy.clear_on_exit.downloads_list,
            other.privacy.clear_on_exit.downloads_list
        );

        note!(changed,
            "downloads.directory",
            self.downloads.directory,
            other.downloads.directory
        );
        note!(changed, "downloads.ask_where", self.downloads.ask_where, other.downloads.ask_where);

        note!(changed, "appearance.theme", self.appearance.theme, other.appearance.theme);
        note!(changed,
            "appearance.default_zoom_percent",
            self.appearance.default_zoom_percent,
            other.appearance.default_zoom_percent
        );
        note!(changed,
            "appearance.minimum_font_size",
            self.appearance.minimum_font_size,
            other.appearance.minimum_font_size
        );
        note!(changed,
            "appearance.show_bookmarks_bar",
            self.appearance.show_bookmarks_bar,
            other.appearance.show_bookmarks_bar
        );

        note!(changed,
            "tabs.open_links_in_new_tab",
            self.tabs.open_links_in_new_tab,
            other.tabs.open_links_in_new_tab
        );
        note!(changed,
            "tabs.confirm_close_multiple",
            self.tabs.confirm_close_multiple,
            other.tabs.confirm_close_multiple
        );
        note!(changed, "tabs.new_tab_page", self.tabs.new_tab_page, other.tabs.new_tab_page);

        // One path for the whole map, for the reason in the module docs.
        note!(changed, "unknown", self.unknown, other.unknown);

        changed.sort_unstable();
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn changed_keys_reports_dotted_names_sorted() {
        let before = Settings::default();
        let mut after = Settings::default();
        after.general.home_page = "https://example.org/home".to_string();
        after.search.engines.push(Engine {
            id: "wiki".to_string(),
            name: "Wikipedia".to_string(),
            keyword: Some("wiki".to_string()),
            template: "https://en.wikipedia.org/w/index.php?search={searchTerms}".to_string(),
            suggest_url: None,
        });
        after.privacy.cookies = CookiePolicy::BlockAll;
        after.appearance.default_zoom_percent = 150;
        after.unknown.insert("sync_device".to_string(), Value::from("juno-1"));

        // Hand-worked: five differences, sorted by path — a, g, p, s, u.
        assert_eq!(
            before.changed_keys(&after),
            vec![
                "appearance.default_zoom_percent",
                "general.home_page",
                "privacy.cookies",
                "search.engines",
                "unknown",
            ]
        );

        // The same pair read the other way gives the same list.
        assert_eq!(after.changed_keys(&before), before.changed_keys(&after));

        // A clear-on-exit switch is its own path, not the whole privacy section.
        let mut cleared = Settings::default();
        cleared.privacy.clear_on_exit.cache = true;
        assert_eq!(before.changed_keys(&cleared), vec!["privacy.clear_on_exit.cache"]);

        // Fields noticed in section order still come out sorted, and an engine that
        // only changes its keyword is still a changed `search.engines`.
        let mut many = Settings::default();
        many.tabs.new_tab_page = NewTabPage::HomePage;
        many.general.startup = Startup::Blank;
        many.appearance.theme = Theme::Dark;
        many.downloads.ask_where = true;
        assert_eq!(
            before.changed_keys(&many),
            vec![
                "appearance.theme",
                "downloads.ask_where",
                "general.startup",
                "tabs.new_tab_page",
            ]
        );

        let mut keyword = Settings::default();
        keyword.search.engines[0].keyword = Some("dd".to_string());
        assert_eq!(before.changed_keys(&keyword), vec!["search.engines"]);
    }

    #[test]
    fn changed_keys_empty_for_equal() {
        let settings = Settings::default();
        assert!(settings.changed_keys(&settings.clone()).is_empty());
        assert!(Settings::default().changed_keys(&Settings::default()).is_empty());

        // Two files that differ only in the order their unknown keys were written in
        // are the same settings: the map holds the keys either way.
        let mut a = Settings::default();
        a.unknown.insert("zebra".to_string(), Value::from(1));
        a.unknown.insert("yak".to_string(), Value::from(2));
        let mut b = Settings::default();
        b.unknown.insert("yak".to_string(), Value::from(2));
        b.unknown.insert("zebra".to_string(), Value::from(1));
        assert!(a.changed_keys(&b).is_empty(), "{:?}", a.changed_keys(&b));

        // A value inside an unknown key is a change, reported by the one path.
        b.unknown.insert("yak".to_string(), Value::from(3));
        assert_eq!(a.changed_keys(&b), vec!["unknown"]);

        // A round trip through the file reports nothing.
        let text = settings.to_json();
        let (loaded, _) = Settings::from_json(&text).expect("the default file reads");
        assert!(settings.changed_keys(&loaded).is_empty());
    }
}
