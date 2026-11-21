/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The lenient reader: what a file says becomes what the browser acts on, and every step of that which needed a
//! change is a [`Warning`].
//!
//! Three rules hold this together.
//!
//! - A key that is missing is the default for that key and no comment on it: a partial file is a person who
//!   changed one thing, not one who lost the rest.
//! - A key whose type is wrong costs that key alone. Sections are read independently and the fixing runs field by
//!   field, so nothing here throws away the settings beside the unreadable one.
//! - A `version` above this build's is refused outright ([`crate::SettingsError::TooNew`]), because writing our
//!   own defaults over it would erase the settings only that newer build knows how to read.
//!
//! The shape is read out of a [`serde_json::Value`] by hand — no derive, no mirror struct — so the types in
//! [`crate::schema`] stay the only statement of what a setting is.

use serde_json::Value;

use crate::migrate;
use crate::schema::{
    Appearance, BookmarksBar, ClearOnExit, CookiePolicy, Downloads, Engine, FONT_MAX, FONT_MIN, General, NewTabPage,
    Privacy, Search, Settings, Startup, Tabs, Theme, ZOOM_MAX, ZOOM_MIN, limit_to,
};
use crate::{Object, SettingsError, Warning};

/// The sections of the current format, in the order the file writes them.
const SECTIONS: [&str; 6] = ["general", "search", "privacy", "downloads", "appearance", "tabs"];

impl Settings {
    /// Read a file: never panics, never loses a setting that was readable, and reports what it had to change.
    ///
    /// A file from an older version is migrated first, so what is read below is one shape. `Ok` carries the
    /// settings and the warnings in the order the sections are read. `Err` means the settings this build holds
    /// are the defaults and the file stays as it is.
    pub fn from_json(text: &str) -> Result<(Settings, Vec<Warning>), SettingsError> {
        let value =
            serde_json::from_str::<Value>(text).map_err(|error| SettingsError::Syntax(error.to_string()))?;
        let (value, mut warnings) = migrate::migrate(value)?;
        Ok((Settings::load(&value, &mut warnings), warnings))
    }

    /// Read a value that is already at this build's version.
    pub(crate) fn load(value: &Value, warnings: &mut Vec<Warning>) -> Settings {
        let mut settings = Settings::default();
        // `migrate` refused anything that is not an object; a caller that skips it gets the defaults, not a panic.
        let Value::Object(root) = value else {
            return settings;
        };
        if let Some(section) = section(root, "general", "general", warnings) {
            settings.general = General::load(section, warnings);
        }
        if let Some(section) = section(root, "search", "search", warnings) {
            settings.search = Search::load(section, warnings);
        }
        if let Some(section) = section(root, "privacy", "privacy", warnings) {
            settings.privacy = Privacy::load(section, warnings);
        }
        if let Some(section) = section(root, "downloads", "downloads", warnings) {
            settings.downloads = Downloads::load(section, warnings);
        }
        if let Some(section) = section(root, "appearance", "appearance", warnings) {
            settings.appearance = Appearance::load(section, warnings);
        }
        if let Some(section) = section(root, "tabs", "tabs", warnings) {
            settings.tabs = Tabs::load(section, warnings);
        }
        settings.fix(warnings);
        // Keys we have no field for, in the sorted order `serde_json`'s map gives, so the warnings come the same
        // way every read. Inside a known section an unknown key is dropped instead: that section's meaning is
        // fixed here, and half of a newer one is not a setting this build can honour.
        for (key, value) in root.iter() {
            if key == "version" || SECTIONS.contains(&key.as_str()) {
                continue;
            }
            warnings.push(Warning::UnknownKeyKept(key.clone()));
            settings.unknown.insert(key.clone(), value.clone());
        }
        settings
    }
}

impl General {
    pub(crate) fn load(object: &Object, warnings: &mut Vec<Warning>) -> General {
        let mut general = General::default();
        if let Some(found) = enum_field(object, "startup", "general.startup", Startup::NAMES, warnings) {
            general.startup = found;
        }
        if let Some(found) = string_field(object, "home_page", "general.home_page", warnings) {
            general.home_page = found;
        }
        if let Some(found) = string_list_field(object, "accept_languages", "general.accept_languages", warnings) {
            general.accept_languages = found;
        }
        general
    }
}

impl Search {
    pub(crate) fn load(object: &Object, warnings: &mut Vec<Warning>) -> Search {
        let mut search = Search::default();
        if let Some(found) = string_field(object, "default_engine", "search.default_engine", warnings) {
            search.default_engine = found;
        }
        if let Some(found) = engine_list_field(object, warnings) {
            search.engines = found;
        }
        if let Some(found) = bool_field(object, "suggest_remote", "search.suggest_remote", warnings) {
            search.suggest_remote = found;
        }
        if let Some(found) = bool_field(object, "suggest_history", "search.suggest_history", warnings) {
            search.suggest_history = found;
        }
        if let Some(found) = bool_field(object, "suggest_bookmarks", "search.suggest_bookmarks", warnings) {
            search.suggest_bookmarks = found;
        }
        search
    }
}

impl Privacy {
    pub(crate) fn load(object: &Object, warnings: &mut Vec<Warning>) -> Privacy {
        let mut privacy = Privacy::default();
        if let Some(found) = bool_field(object, "do_not_track", "privacy.do_not_track", warnings) {
            privacy.do_not_track = found;
        }
        if let Some(found) = enum_field(object, "cookies", "privacy.cookies", CookiePolicy::NAMES, warnings) {
            privacy.cookies = found;
        }
        if let Some(found) = bool_field(object, "content_blocking", "privacy.content_blocking", warnings) {
            privacy.content_blocking = found;
        }
        if let Some(found) = bool_field(object, "error_log", "privacy.error_log", warnings) {
            privacy.error_log = found;
        }
        if let Some(section) = section(object, "clear_on_exit", "privacy.clear_on_exit", warnings) {
            privacy.clear_on_exit = ClearOnExit::load(section, warnings);
        }
        privacy
    }
}

impl ClearOnExit {
    pub(crate) fn load(object: &Object, warnings: &mut Vec<Warning>) -> ClearOnExit {
        let mut clear = ClearOnExit::default();
        if let Some(found) = bool_field(object, "history", "privacy.clear_on_exit.history", warnings) {
            clear.history = found;
        }
        if let Some(found) = bool_field(object, "cookies", "privacy.clear_on_exit.cookies", warnings) {
            clear.cookies = found;
        }
        if let Some(found) = bool_field(object, "cache", "privacy.clear_on_exit.cache", warnings) {
            clear.cache = found;
        }
        if let Some(found) = bool_field(object, "downloads_list", "privacy.clear_on_exit.downloads_list", warnings)
        {
            clear.downloads_list = found;
        }
        clear
    }
}

impl Downloads {
    pub(crate) fn load(object: &Object, warnings: &mut Vec<Warning>) -> Downloads {
        let mut downloads = Downloads::default();
        if let Some(found) = optional_string_field(object, "directory", "downloads.directory", warnings) {
            downloads.directory = found;
        }
        if let Some(found) = bool_field(object, "ask_where", "downloads.ask_where", warnings) {
            downloads.ask_where = found;
        }
        downloads
    }
}

impl Appearance {
    pub(crate) fn load(object: &Object, warnings: &mut Vec<Warning>) -> Appearance {
        let mut appearance = Appearance::default();
        if let Some(found) = enum_field(object, "theme", "appearance.theme", Theme::NAMES, warnings) {
            appearance.theme = found;
        }
        if let Some(found) =
            number_field(object, "default_zoom_percent", "appearance.default_zoom_percent", warnings)
        {
            appearance.default_zoom_percent =
                limit_to(found, "appearance.default_zoom_percent", ZOOM_MIN, ZOOM_MAX, warnings) as u16;
        }
        if let Some(found) = number_field(object, "minimum_font_size", "appearance.minimum_font_size", warnings) {
            appearance.minimum_font_size =
                limit_to(found, "appearance.minimum_font_size", FONT_MIN, FONT_MAX, warnings) as u8;
        }
        if let Some(found) = enum_field(
            object,
            "show_bookmarks_bar",
            "appearance.show_bookmarks_bar",
            BookmarksBar::NAMES,
            warnings,
        ) {
            appearance.show_bookmarks_bar = found;
        }
        appearance
    }
}

impl Tabs {
    pub(crate) fn load(object: &Object, warnings: &mut Vec<Warning>) -> Tabs {
        let mut tabs = Tabs::default();
        if let Some(found) = bool_field(object, "open_links_in_new_tab", "tabs.open_links_in_new_tab", warnings) {
            tabs.open_links_in_new_tab = found;
        }
        if let Some(found) = bool_field(object, "confirm_close_multiple", "tabs.confirm_close_multiple", warnings) {
            tabs.confirm_close_multiple = found;
        }
        if let Some(found) = enum_field(object, "new_tab_page", "tabs.new_tab_page", NewTabPage::NAMES, warnings) {
            tabs.new_tab_page = found;
        }
        tabs
    }
}

/// A section that is there and is an object. `None` covers both the section that is missing — defaults, said
/// nowhere — and the one holding something else, which is said.
fn section<'a>(object: &'a Object, key: &str, path: &str, warnings: &mut Vec<Warning>) -> Option<&'a Object> {
    match object.get(key) {
        None => None,
        Some(Value::Object(found)) => Some(found),
        Some(found) => {
            warnings.push(unexpected(path, "an object", found));
            None
        }
    }
}

fn string_field(object: &Object, key: &str, path: &str, warnings: &mut Vec<Warning>) -> Option<String> {
    match object.get(key) {
        None => None,
        Some(Value::String(found)) => Some(found.clone()),
        Some(found) => {
            warnings.push(unexpected(path, "a string", found));
            None
        }
    }
}

/// A string or `null`: `None` for the key being absent, `Some(None)` for it saying it holds nothing.
fn optional_string_field(
    object: &Object,
    key: &str,
    path: &str,
    warnings: &mut Vec<Warning>,
) -> Option<Option<String>> {
    match object.get(key) {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(found)) => Some(Some(found.clone())),
        Some(found) => {
            warnings.push(unexpected(path, "a string or null", found));
            None
        }
    }
}

fn bool_field(object: &Object, key: &str, path: &str, warnings: &mut Vec<Warning>) -> Option<bool> {
    match object.get(key) {
        None => None,
        Some(Value::Bool(found)) => Some(*found),
        Some(found) => {
            warnings.push(unexpected(path, "a boolean", found));
            None
        }
    }
}

/// A number as an `i128`, so a file written by another program (a zoom of 1.0, or one no control could write)
/// still reads; whether the value fits the setting is the field's own fixing's business.
fn number_field(object: &Object, key: &str, path: &str, warnings: &mut Vec<Warning>) -> Option<i128> {
    let found = object.get(key)?;
    let Value::Number(number) = found else {
        warnings.push(unexpected(path, "a number", found));
        return None;
    };
    if let Some(integer) = number.as_i64() {
        return Some(i128::from(integer));
    }
    if let Some(positive) = number.as_u64() {
        return Some(i128::from(positive));
    }
    match number.as_f64() {
        Some(real) if real.is_finite() => Some(real.round() as i128),
        _ => {
            warnings.push(Warning::Dropped {
                path: path.to_string(),
                reason: "not a number this setting can hold".to_string(),
            });
            None
        }
    }
}

/// One of an enum's file names; anything else costs the field and says which names there were.
fn enum_field<T: Copy>(
    object: &Object,
    key: &str,
    path: &str,
    names: &[(&str, T)],
    warnings: &mut Vec<Warning>,
) -> Option<T> {
    let found = object.get(key)?;
    let Value::String(text) = found else {
        warnings.push(unexpected(path, "a string", found));
        return None;
    };
    for (name, value) in names {
        if *name == text.as_str() {
            return Some(*value);
        }
    }
    let listed = names
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<&str>>()
        .join(", ");
    warnings.push(Warning::Dropped {
        path: path.to_string(),
        reason: format!("{text} is none of {listed}"),
    });
    None
}

fn string_list_field(object: &Object, key: &str, path: &str, warnings: &mut Vec<Warning>) -> Option<Vec<String>> {
    let found = object.get(key)?;
    let Value::Array(items) = found else {
        warnings.push(unexpected(path, "a list", found));
        return None;
    };
    let mut list: Vec<String> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match item {
            Value::String(text) => list.push(text.clone()),
            other => warnings.push(unexpected(&format!("{path}[{index}]"), "a string", other)),
        }
    }
    Some(list)
}

/// The engines, each read as far as it goes. An engine whose parts do not add up is dropped by the fixing that
/// follows, which says so with its index; reading the fields leniently here is what keeps one broken engine from
/// costing the list.
fn engine_list_field(object: &Object, warnings: &mut Vec<Warning>) -> Option<Vec<Engine>> {
    let items = match object.get("engines") {
        None => return None,
        Some(Value::Array(items)) => items,
        Some(found) => {
            warnings.push(unexpected("search.engines", "a list", found));
            return None;
        }
    };
    let mut engines: Vec<Engine> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let base = format!("search.engines[{index}]");
        let Value::Object(fields) = item else {
            warnings.push(unexpected(&base, "an object", item));
            continue;
        };
        let engine = Engine {
            id: string_field(fields, "id", &format!("{base}.id"), warnings).unwrap_or_default(),
            name: string_field(fields, "name", &format!("{base}.name"), warnings).unwrap_or_default(),
            keyword: optional_string_field(fields, "keyword", &format!("{base}.keyword"), warnings).flatten(),
            template: string_field(fields, "template", &format!("{base}.template"), warnings).unwrap_or_default(),
            suggest_url: optional_string_field(fields, "suggest_url", &format!("{base}.suggest_url"), warnings)
                .flatten(),
        };
        engines.push(engine);
    }
    Some(engines)
}

/// The warning for a value of the wrong shape, naming what was wanted and what was there.
fn unexpected(path: &str, wanted: &str, found: &Value) -> Warning {
    Warning::Dropped {
        path: path.to_string(),
        reason: format!("expected {wanted}, found {}", kind(found)),
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "nothing",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::HOME_PAGE_MAX;

    /// The dotted path a warning is about, for the assertions that do not care how the reason is worded.
    fn path(warning: &Warning) -> &str {
        match warning {
            Warning::Dropped { path, .. } | Warning::Clamped { path, .. } => path,
            Warning::UnknownKeyKept(key) => key,
            Warning::Migrated { .. } => "version",
        }
    }

    /// The reason a warning gives, for the assertions that do not care about the whole variant.
    fn reason(warning: &Warning) -> &str {
        match warning {
            Warning::Dropped { reason, .. } => reason,
            Warning::Clamped { .. } => "clamped",
            Warning::UnknownKeyKept(_) => "kept",
            Warning::Migrated { .. } => "migrated",
        }
    }

    /// `{}` writes no `version`, so it is version 0 and the chain has one step to report. Nothing is said about
    /// values — there are none — and what comes out is the shipped defaults.
    #[test]
    fn empty_object_gives_defaults_without_warnings() {
        let (settings, warnings) = Settings::from_json("{}").expect("an empty object reads");
        assert_eq!(settings, Settings::default());
        assert_eq!(warnings, vec![Warning::Migrated { from: 0, to: 1 }]);
    }

    /// A file that names one setting is a person who changed one setting; the rest are defaults, said nowhere.
    #[test]
    fn missing_keys_use_defaults() {
        let text = r#"{"version":1,"general":{"startup":"blank"}}"#;
        let (settings, warnings) = Settings::from_json(text).expect("a partial file reads");
        assert_eq!(warnings, Vec::new());
        assert_eq!(settings.general.startup, Startup::Blank);
        assert_eq!(settings.general.home_page, "about:blank");
        assert_eq!(
            settings.general.accept_languages,
            vec!["en-US".to_string(), "en".to_string()]
        );
        // The sections that were not in the file at all keep every default.
        assert_eq!(settings.search, Search::default());
        assert_eq!(settings.privacy, Privacy::default());
        assert_eq!(settings.downloads, Downloads::default());
        assert_eq!(settings.appearance, Appearance::default());
        assert_eq!(settings.tabs, Tabs::default());
    }

    /// The error log is the one setting whose default is on with no Firefox behind it, and the one the
    /// about:errors page points a person at: a file that says `false` must reach them as `false`.
    #[test]
    fn the_error_log_switch_is_on_by_default_and_off_when_the_file_says_so() {
        assert!(Privacy::default().error_log, "a browser that keeps no record cannot be told about it");

        let (settings, warnings) = Settings::from_json(r#"{"version":1,"privacy":{"error_log":false}}"#)
            .expect("a file that turns the log off reads");
        assert_eq!(warnings, Vec::new());
        assert!(!settings.privacy.error_log);
        assert!(settings.privacy.do_not_track == Privacy::default().do_not_track, "the rest of the section held");

        // A value of the wrong shape costs the key, and the default — on — comes back with the warning.
        let (settings, warnings) =
            Settings::from_json(r#"{"version":1,"privacy":{"error_log":"nope"}}"#).expect("a mistyped file reads");
        assert_eq!(
            warnings,
            vec![Warning::Dropped {
                path: "privacy.error_log".to_string(),
                reason: "expected a boolean, found a string".to_string()
            }]
        );
        assert!(settings.privacy.error_log, "a bad value never silences the log on its own");
    }

    /// A value of the wrong shape costs its own key, and nothing else in the section beside it.
    #[test]
    fn wrong_type_drops_only_that_key() {
        let text = r#"{"version":1,"general":{"startup":3,"home_page":"https://ln.example/"}}"#;
        let (settings, warnings) = Settings::from_json(text).expect("a mistyped file reads");
        assert_eq!(
            warnings,
            vec![Warning::Dropped {
                path: "general.startup".to_string(),
                reason: "expected a string, found a number".to_string()
            }]
        );
        assert_eq!(settings.general.startup, Startup::HomePage);
        assert_eq!(settings.general.home_page, "https://ln.example/");
    }

    /// A whole section of the wrong shape costs that section; the sections around it are read as usual.
    #[test]
    fn wrong_type_section_drops_only_that_section() {
        let text = r#"{"version":1,"search":"duckduckgo","tabs":{"new_tab_page":"home_page"}}"#;
        let (settings, warnings) = Settings::from_json(text).expect("a mistyped section reads");
        assert_eq!(
            warnings,
            vec![Warning::Dropped {
                path: "search".to_string(),
                reason: "expected an object, found a string".to_string()
            }]
        );
        assert_eq!(settings.search, Search::default());
        assert_eq!(settings.tabs.new_tab_page, NewTabPage::HomePage);
    }

    /// An enum name the file does not know costs that field alone.
    #[test]
    fn bad_enum_value_defaults_with_warning() {
        let text = r#"{"version":1,"privacy":{"cookies":"block_everything","do_not_track":true}}"#;
        let (settings, warnings) = Settings::from_json(text).expect("an unknown enum name reads");
        assert_eq!(
            warnings,
            vec![Warning::Dropped {
                path: "privacy.cookies".to_string(),
                reason: "block_everything is none of allow_all, block_third_party_trackers, block_third_party, block_all".to_string()
            }]
        );
        assert_eq!(settings.privacy.cookies, CookiePolicy::BlockThirdPartyTrackers);
        // The other value in the same section survived.
        assert!(settings.privacy.do_not_track);
    }

    /// The zoom ends are the two browsers' menu ends, and the person is told which one they passed.
    #[test]
    fn zoom_clamped_with_warning() {
        let (settings, warnings) =
            Settings::from_json(r#"{"version":1,"appearance":{"default_zoom_percent":5000}}"#).expect("reads");
        assert_eq!(settings.appearance.default_zoom_percent, 500);
        assert_eq!(
            warnings,
            vec![Warning::Clamped {
                path: "appearance.default_zoom_percent".to_string(),
                from: "5000".to_string(),
                to: "500".to_string()
            }]
        );
        let (settings, warnings) =
            Settings::from_json(r#"{"version":1,"appearance":{"default_zoom_percent":5}}"#).expect("reads");
        assert_eq!(settings.appearance.default_zoom_percent, 20);
        assert_eq!(
            warnings,
            vec![Warning::Clamped {
                path: "appearance.default_zoom_percent".to_string(),
                from: "5".to_string(),
                to: "20".to_string()
            }]
        );
        // The ends themselves are inside the range and say nothing.
        let (settings, warnings) =
            Settings::from_json(r#"{"version":1,"appearance":{"default_zoom_percent":20}}"#).expect("reads");
        assert_eq!(settings.appearance.default_zoom_percent, 20);
        assert_eq!(warnings, Vec::new());
    }

    /// A minimum font size over 72 px is a page nobody is reading any more, so it stops there.
    #[test]
    fn font_size_clamped() {
        let (settings, warnings) =
            Settings::from_json(r#"{"version":1,"appearance":{"minimum_font_size":200}}"#).expect("reads");
        assert_eq!(settings.appearance.minimum_font_size, 72);
        assert_eq!(
            warnings,
            vec![Warning::Clamped {
                path: "appearance.minimum_font_size".to_string(),
                from: "200".to_string(),
                to: "72".to_string()
            }]
        );
        // A negative size, and one far too large for the field's own type, both land on an end instead of wrapping.
        let (settings, warnings) =
            Settings::from_json(r#"{"version":1,"appearance":{"minimum_font_size":-3}}"#).expect("reads");
        assert_eq!(settings.appearance.minimum_font_size, 0);
        assert_eq!(warnings.len(), 1);
        let (settings, warnings) =
            Settings::from_json(r#"{"version":1,"appearance":{"minimum_font_size":40000000000}}"#).expect("reads");
        assert_eq!(settings.appearance.minimum_font_size, 72);
        assert_eq!(warnings.len(), 1, "one warning for one value: {warnings:?}");
    }

    /// The home page is trimmed; an empty one is no page; one longer than 2048 bytes is a pasted document.
    #[test]
    fn home_page_empty_and_too_long() {
        let (settings, warnings) =
            Settings::from_json(r#"{"version":1,"general":{"home_page":"  https://ln.example/  "}}"#).expect("reads");
        assert_eq!(settings.general.home_page, "https://ln.example/");
        assert_eq!(warnings, Vec::new(), "trimming is not worth a warning");

        let (settings, warnings) =
            Settings::from_json(r#"{"version":1,"general":{"home_page":"   "}}"#).expect("reads");
        assert_eq!(settings.general.home_page, "about:blank");
        assert_eq!(
            warnings,
            vec![Warning::Dropped {
                path: "general.home_page".to_string(),
                reason: "no page given".to_string()
            }]
        );

        // Hand-worked: exactly 2048 bytes fit (the limit is inclusive), 2049 do not.
        let at_the_end = "h".to_string() + &"t".repeat(HOME_PAGE_MAX - 1);
        assert_eq!(at_the_end.len(), HOME_PAGE_MAX);
        let text = format!(r#"{{"version":1,"general":{{"home_page":"{at_the_end}"}}}}"#);
        let (settings, warnings) = Settings::from_json(&text).expect("reads");
        assert_eq!(settings.general.home_page, at_the_end);
        assert_eq!(warnings, Vec::new());

        let too_long = "t".repeat(HOME_PAGE_MAX + 1);
        let text = format!(r#"{{"version":1,"general":{{"home_page":"{too_long}"}}}}"#);
        let (settings, warnings) = Settings::from_json(&text).expect("reads");
        assert_eq!(settings.general.home_page, "about:blank");
        assert_eq!(
            warnings,
            vec![Warning::Dropped {
                path: "general.home_page".to_string(),
                reason: "longer than 2048 bytes".to_string()
            }]
        );
        // A typed page of any other text is accepted: it is the wiring that decides whether it parses.
        let (settings, warnings) =
            Settings::from_json(r#"{"version":1,"general":{"home_page":"not a url at all"}}"#).expect("reads");
        assert_eq!(settings.general.home_page, "not a url at all");
        assert_eq!(warnings, Vec::new());
    }

    /// A bad tag costs its own entry, a repeat of one already kept goes quietly, and the eleventh onward is not
    /// offered at all.
    #[test]
    fn accept_languages_validation_and_dedupe() {
        let text = r#"{"version":1,"general":{"accept_languages":["en-US"," en-us ","en","x","English","de-DE-1901","qaa-zz","","fi"]}}"#;
        let (settings, warnings) = Settings::from_json(text).expect("reads");
        // "x" is one letter, "English" seven, "" none: each costs its own entry. "en-us" repeats "en-US" (BCP 47
        // tags ignore case), and "de-DE-1901" and "qaa-zz" are well formed.
        assert_eq!(
            settings.general.accept_languages,
            vec![
                "en-US".to_string(),
                "en".to_string(),
                "de-DE-1901".to_string(),
                "qaa-zz".to_string(),
                "fi".to_string()
            ]
        );
        assert_eq!(warnings.len(), 3, "one per bad entry: {warnings:?}");
        assert_eq!(path(&warnings[0]), "general.accept_languages[3]");
        assert_eq!(path(&warnings[1]), "general.accept_languages[4]");
        assert_eq!(path(&warnings[2]), "general.accept_languages[7]");

        // Twelve well-formed tags: the first ten win and the last two are said.
        let twelve: Vec<String> = ["aa", "ab", "ac", "ad", "ae", "af", "ag", "ah", "ai", "aj", "ak", "al"]
            .iter()
            .map(|tag| (*tag).to_string())
            .collect();
        let text = format!(
            r#"{{"version":1,"general":{{"accept_languages":{}}}}}"#,
            Value::Array(twelve.iter().map(|tag| Value::String(tag.clone())).collect())
        );
        let (settings, warnings) = Settings::from_json(&text).expect("reads");
        assert_eq!(settings.general.accept_languages, twelve[..10].to_vec());
        assert_eq!(warnings.len(), 2, "the eleventh and twelfth: {warnings:?}");
        assert_eq!(path(&warnings[0]), "general.accept_languages[10]");
        assert_eq!(path(&warnings[1]), "general.accept_languages[11]");

        // An empty list would offer nothing, so the default list comes back.
        let (settings, warnings) =
            Settings::from_json(r#"{"version":1,"general":{"accept_languages":[]}}"#).expect("reads");
        assert_eq!(
            settings.general.accept_languages,
            vec!["en-US".to_string(), "en".to_string()]
        );
        assert_eq!(
            warnings,
            vec![Warning::Dropped {
                path: "general.accept_languages".to_string(),
                reason: "no usable language tag".to_string()
            }]
        );
    }

    /// Two engines cannot share an id: the second is dropped and the first is what a search uses.
    #[test]
    fn engines_duplicate_ids_dropped() {
        let text = r#"{"version":1,"search":{"engines":[
            {"id":"ddg","name":"DuckDuckGo","keyword":"ddg","template":"https://duckduckgo.com/html/?q={searchTerms}","suggest_url":null},
            {"id":"ddg","name":"Copy","keyword":null,"template":"https://other.example/?q={searchTerms}","suggest_url":null}
        ]}}"#;
        let (settings, warnings) = Settings::from_json(text).expect("reads");
        assert_eq!(settings.search.engines.len(), 1);
        assert_eq!(settings.search.engines[0].name, "DuckDuckGo");
        // Hand-worked: two complaints, not one. The second engine goes, and the file
        // never named a default, so the shipped "duckduckgo" names nothing and the
        // first kept engine ("ddg") takes over.
        assert_eq!(settings.search.default_engine, "ddg");
        assert_eq!(
            warnings,
            vec![
                Warning::Dropped {
                    path: "search.engines[1]".to_string(),
                    reason: "another engine already has this id".to_string()
                },
                Warning::Dropped {
                    path: "search.default_engine".to_string(),
                    reason: "no engine with id duckduckgo".to_string()
                },
            ]
        );
    }

    /// A keyword that repeats is the same mistake in the other direction, and a template with nowhere for the
    /// typed words to go searches nothing.
    #[test]
    fn engine_template_without_placeholder_dropped() {
        let text = r#"{"version":1,"search":{"engines":[
            {"id":"wiki","name":"Wikipedia","keyword":null,"template":"https://wikipedia.org/w/index.php","suggest_url":null}
        ]}}"#;
        let (settings, warnings) = Settings::from_json(text).expect("reads");
        // Nothing usable was left, so the built-in engine came back.
        assert_eq!(settings.search.engines, vec![Engine::builtin()]);
        assert_eq!(
            warnings,
            vec![
                Warning::Dropped {
                    path: "search.engines[0]".to_string(),
                    reason: "template has no {searchTerms}".to_string()
                },
                Warning::Dropped {
                    path: "search.engines".to_string(),
                    reason: "no usable engine left".to_string()
                },
            ]
        );
        // The keyword belongs to another engine.
        let text = r#"{"version":1,"search":{"engines":[
            {"id":"one","name":"One","keyword":"ddg","template":"https://one.example/?q={searchTerms}","suggest_url":null},
            {"id":"two","name":"Two","keyword":"ddg","template":"https://two.example/?q={searchTerms}","suggest_url":null}
        ]}}"#;
        let (settings, warnings) = Settings::from_json(text).expect("reads");
        assert_eq!(settings.search.engines.len(), 1);
        assert_eq!(reason(&warnings[0]), "another engine already has this keyword");
    }

    /// Only a plain http(s) URL is a search: `ftp://`, `javascript:` and a relative path are refused.
    #[test]
    fn engine_template_must_be_http_or_https() {
        for template in [
            "ftp://search.example/?q={searchTerms}",
            "javascript:search({searchTerms})",
            "/relative/?q={searchTerms}",
        ] {
            let text = format!(
                r#"{{"version":1,"search":{{"engines":[{{"id":"one","name":"One","keyword":null,"template":"{template}","suggest_url":null}}]}}}}"#
            );
            let (settings, warnings) = Settings::from_json(&text).expect("reads");
            assert_eq!(settings.search.engines[0].id, "duckduckgo", "{template} was taken");
            assert_eq!(warnings[0], Warning::Dropped {
                path: "search.engines[0]".to_string(),
                reason: "template is not a http(s) URL".to_string()
            });
        }
        // Both of the schemes that pass.
        for template in [
            "http://intranet.example/?q={searchTerms}",
            "https://duckduckgo.com/html/?q={searchTerms}",
        ] {
            let text = format!(
                r#"{{"version":1,"search":{{"engines":[{{"id":"one","name":"One","keyword":null,"template":"{template}","suggest_url":null}}]}}}}"#
            );
            let (settings, warnings) = Settings::from_json(&text).expect("reads");
            assert_eq!(settings.search.engines[0].id, "one", "{template} was refused");
            // Hand-worked: the engine is kept, but the file named no default, so the
            // shipped "duckduckgo" is not on the list and the kept engine takes over.
            assert_eq!(settings.search.default_engine, "one");
            assert_eq!(
                warnings,
                vec![Warning::Dropped {
                    path: "search.default_engine".to_string(),
                    reason: "no engine with id duckduckgo".to_string()
                }]
            );
        }
    }

    /// An empty engine list, and a list of nothing but broken engines, both still search.
    #[test]
    fn no_valid_engines_restores_builtin() {
        let (settings, warnings) = Settings::from_json(r#"{"version":1,"search":{"engines":[]}}"#).expect("reads");
        assert_eq!(settings.search.engines, vec![Engine::builtin()]);
        assert_eq!(settings.search.default_engine, "duckduckgo");
        assert_eq!(
            warnings,
            vec![Warning::Dropped {
                path: "search.engines".to_string(),
                reason: "no usable engine left".to_string()
            }]
        );
        // An id with capital letters is not an id: `[a-z0-9_-]` only.
        let text = r#"{"version":1,"search":{"engines":[{"id":"UPPER","name":"Broken","keyword":null,"template":"https://x.example/?q={searchTerms}","suggest_url":null}]}}"#;
        let (settings, warnings) = Settings::from_json(text).expect("reads");
        assert_eq!(settings.search.engines, vec![Engine::builtin()]);
        assert_eq!(warnings.len(), 2, "the engine, then the list it left: {warnings:?}");
        assert_eq!(path(&warnings[0]), "search.engines[0]");
        assert_eq!(path(&warnings[1]), "search.engines");
    }

    /// A `default_engine` that names nothing becomes the first engine, with the reason said.
    #[test]
    fn unknown_default_engine_falls_back_to_first() {
        let text = r#"{"version":1,"search":{"default_engine":"google","engines":[
            {"id":"one","name":"One","keyword":null,"template":"https://one.example/?q={searchTerms}","suggest_url":null},
            {"id":"two","name":"Two","keyword":null,"template":"https://two.example/?q={searchTerms}","suggest_url":null}
        ]}}"#;
        let (settings, warnings) = Settings::from_json(text).expect("reads");
        assert_eq!(settings.search.default_engine, "one");
        assert_eq!(settings.default_engine().name, "One");
        assert_eq!(
            warnings,
            vec![Warning::Dropped {
                path: "search.default_engine".to_string(),
                reason: "no engine with id google".to_string()
            }]
        );
    }

    /// A key a newer build wrote survives the read, the warning, the write and the read after that.
    #[test]
    fn unknown_top_level_keys_kept_and_written_back() {
        let text = r#"{"version":1,"sync_device":"juno-1","general":{"home_page":"https://ln.example/"}}"#;
        let (settings, warnings) = Settings::from_json(text).expect("reads");
        assert_eq!(
            settings.unknown.get("sync_device"),
            Some(&Value::String("juno-1".to_string()))
        );
        assert_eq!(warnings, vec![Warning::UnknownKeyKept("sync_device".to_string())]);
        assert_eq!(settings.general.home_page, "https://ln.example/");
        let written = settings.to_json();
        assert!(written.contains("\"sync_device\": \"juno-1\""), "{written}");
        let (again, warnings) = Settings::from_json(&written).expect("the file we wrote reads back");
        assert_eq!(again, settings);
        assert_eq!(warnings, vec![Warning::UnknownKeyKept("sync_device".to_string())]);
    }

    /// An unknown key inside a known section belongs to a section whose shape this build has decided: keeping it
    /// would promise a setting that cannot exist, so it goes, quietly.
    #[test]
    fn unknown_keys_inside_sections_dropped() {
        let text = r#"{"version":1,"general":{"colour":"blue","startup":"blank"},"downloads":{"nothing_here":[]}}"#;
        let (settings, warnings) = Settings::from_json(text).expect("reads");
        assert_eq!(warnings, Vec::new());
        assert!(settings.unknown.is_empty());
        assert_eq!(settings.general.startup, Startup::Blank);
    }

    /// Load, save, load: the second file says what the first became, and the third changes nothing.
    #[test]
    fn settings_json_survives_load_save_load() {
        let messy = r#"{"home":"https://ln.example/","search_engine":"https://s.example/?q=%s","dnt":true,"zoom":900,"theme":"sepia","extra":{"keep":true},"general":{"startup":"restore_session"}}"#;
        let (first, first_warnings) = Settings::from_json(messy).expect("a v0 file reads");
        assert!(
            first_warnings
                .iter()
                .any(|warning| matches!(warning, Warning::Clamped { .. })),
            "900 % zoom is clamped: {first_warnings:?}"
        );
        assert!(
            first_warnings.iter().any(|warning| reason(warning) == "sepia is none of auto, light, dark"),
            "the theme name is refused: {first_warnings:?}"
        );
        assert_eq!(first.general.startup, Startup::RestoreSession, "the nested section survived the step");
        let written = first.to_json();
        let (second, second_warnings) = Settings::from_json(&written).expect("our own file reads");
        // All the second read has to say is that the newer build's key is still there: the first read fixed every
        // value, so nothing is dropped or moved any more.
        assert_eq!(second_warnings, vec![Warning::UnknownKeyKept("extra".to_string())]);
        assert_eq!(second, first);
        assert_eq!(second.to_json(), written, "saving an unchanged file writes the same bytes");
        let (third, _) = Settings::from_json(&second.to_json()).expect("reads again");
        assert_eq!(third, second);
    }
}
