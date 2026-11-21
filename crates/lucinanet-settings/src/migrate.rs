// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The version chain: an older shape of the file is rewritten into the current one.
//!
//! A step only moves values between keys. It never judges them: a moved value that
//! turns out to be a bad zoom, a stale theme name or a broken engine is left exactly
//! as it was found, and the loader in [`crate::load`] reports it as a `Dropped` or
//! `Clamped` warning there. That keeps one rule for what counts as a valid value —
//! the loader's — and keeps the steps short.
//!
//! Version 0 is the flat shape the browser never shipped (`home`, `search_engine`,
//! `dnt`, `zoom`, `theme`). A file with no `version` key is a version 0 file, so a
//! step that has never run in production still has to read correctly.

use super::*;
use crate::schema::Engine;
use serde_json::Value;

/// One upgrade: rewrite a file of version `from` so that it is version `to`.
pub(crate) struct Step {
    pub(crate) from: u32,
    pub(crate) to: u32,
    pub(crate) run: fn(Value) -> Value,
}

/// The whole chain. `CURRENT_VERSION` is the top of it, so a v1 -> v2 step is added
/// here when the format changes: a new step, a raised `CURRENT_VERSION`, and the tests
/// below then cover the ordering.
pub(crate) const CHAIN: &[Step] = &[Step {
    from: 0,
    to: 1,
    run: v0_to_v1,
}];

/// Upgrade a parsed file to the current version. See the module docs.
pub fn migrate(value: Value) -> Result<(Value, Vec<Warning>), SettingsError> {
    migrate_with(value, CHAIN)
}

/// The chain as data, so a test can run a chain of its own without waiting for a
/// second real version to exist.
///
/// A file newer than the chain's top is refused rather than read: downgrading would
/// silently lose the keys the newer file holds, and a settings file is easier to
/// rewrite by hand than a profile to restore.
pub(crate) fn migrate_with(
    value: Value,
    chain: &[Step],
) -> Result<(Value, Vec<Warning>), SettingsError> {
    let mut warnings = Vec::new();
    let mut root = match value {
        Value::Object(root) => root,
        _ => return Err(SettingsError::NotAnObject),
    };
    let found = version_of(&root, &mut warnings);
    let supported = chain.last().map_or(0, |step| step.to);
    if found > supported {
        return Err(SettingsError::TooNew {
            found,
            supported,
        });
    }
    let mut at = found;
    for step in chain.iter().filter(|step| step.from >= found) {
        let next = (step.run)(Value::Object(root));
        root = match next {
            Value::Object(root) => root,
            _ => return Err(SettingsError::NotAnObject),
        };
        at = step.to;
        warnings.push(Warning::Migrated {
            from: step.from,
            to: step.to,
        });
    }
    root.insert("version".to_string(), Value::from(at as u64));
    Ok((Value::Object(root), warnings))
}

/// The `version` of a file. Missing and `null` both mean 0; anything else that is not
/// a usable number is reported and read as 0, because refusing a file over a typo in
/// one key is worse than the typo.
fn version_of(root: &Object, warnings: &mut Vec<Warning>) -> u32 {
    match root.get("version") {
        None | Some(Value::Null) => 0,
        Some(Value::Number(number)) => {
            // Negative and fractional versions are the writer's mistake; 0 is the
            // oldest thing a reader can honestly upgrade from.
            if number.is_f64() {
                match number.as_f64() {
                    Some(found) if found.is_finite() && found >= 0.0 => {
                        u32::try_from(found as i128).unwrap_or(u32::MAX)
                    }
                    _ => 0,
                }
            } else if number.is_i64() {
                number
                    .as_i64()
                    .map_or(0, |found| u32::try_from(found.max(0)).unwrap_or(u32::MAX))
            } else {
                number
                    .as_u64()
                    .map_or(0, |found| u32::try_from(found).unwrap_or(u32::MAX))
            }
        }
        Some(Value::String(text)) => match text.trim().parse::<u32>() {
            // A version written as "1" is the same number, so it is taken quietly.
            Ok(found) => found,
            // "one" is not: the file cannot be placed in the chain, so it is read as
            // the oldest version and upgraded, which is where a hand-written file ends
            // up looking right. The complaint is what tells the caller it was odd.
            Err(_) => {
                warnings.push(Warning::Dropped {
                    path: "version".to_string(),
                    reason: "expected a number, found a string".to_string(),
                });
                0
            }
        },
        Some(found) => {
            warnings.push(Warning::Dropped {
                path: "version".to_string(),
                reason: format!("expected a number, found {}", kind_of(found)),
            });
            0
        }
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "nothing",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// v0 (flat) -> v1 (nested).
///
/// The five v0 keys are moved into their section; the other keys of the file are left
/// where they are, so the loader can keep them as unknown top-level keys.
fn v0_to_v1(value: Value) -> Value {
    let mut root = match value {
        Value::Object(root) => root,
        found => return found,
    };

    let mut general = Object::new();
    if let Some(home) = root.remove("home") {
        general.insert("home_page".to_string(), home);
    }
    merge_section(&mut root, "general", general);

    let mut appearance = Object::new();
    if let Some(zoom) = root.remove("zoom") {
        appearance.insert("default_zoom_percent".to_string(), zoom);
    }
    if let Some(theme) = root.remove("theme") {
        appearance.insert("theme".to_string(), theme);
    }
    merge_section(&mut root, "appearance", appearance);

    let mut privacy = Object::new();
    if let Some(dnt) = root.remove("dnt") {
        privacy.insert("do_not_track".to_string(), dnt);
    }
    merge_section(&mut root, "privacy", privacy);

    if let Some(engine) = root.remove("search_engine") {
        match v0_engine(&engine) {
            V0Engine::Builtin => {
                let mut search = Object::new();
                search.insert("default_engine".to_string(), Value::from("duckduckgo"));
                merge_section(&mut root, "search", search);
            }
            V0Engine::Custom(template) => {
                let mut search = Object::new();
                search.insert("default_engine".to_string(), Value::from("custom"));
                search.insert(
                    "engines".to_string(),
                    Value::Array(vec![Value::Object(custom_engine(&template))]),
                );
                merge_section(&mut root, "search", search);
            }
            V0Engine::Unreadable => {
                // Not a search engine, but still the person's text: put it back where
                // it was, so the loader keeps it as an unknown top-level key instead of
                // the file losing a value on the way through the chain.
                root.insert("search_engine".to_string(), engine);
            }
        }
    }

    Value::Object(root)
}

/// What a v0 `search_engine` string meant.
enum V0Engine {
    /// One of the names of the built-in engine.
    Builtin,
    /// A search URL to keep as a custom engine.
    Custom(String),
    /// Neither: the value stays where it is and the loader keeps it as unknown.
    Unreadable,
}

fn v0_engine(value: &Value) -> V0Engine {
    let Some(text) = value.as_str() else {
        return V0Engine::Unreadable;
    };
    let lowered = text.trim().to_lowercase();
    if lowered == "ddg" || lowered == "duckduckgo" {
        return V0Engine::Builtin;
    }
    if !crate::schema::is_http_url(text.trim()) {
        return V0Engine::Unreadable;
    }
    let template = text.trim().replace("%s", Engine::PLACEHOLDER);
    if template.contains(Engine::PLACEHOLDER) {
        V0Engine::Custom(template)
    } else {
        V0Engine::Unreadable
    }
}

/// The built-in engine is the default and needs no record of its own; a v0 URL
/// becomes one, under the only id a hand-written file can be expected to use.
fn custom_engine(template: &str) -> Object {
    let mut engine = Object::new();
    engine.insert("id".to_string(), Value::from("custom"));
    engine.insert("name".to_string(), Value::from("Custom"));
    engine.insert("keyword".to_string(), Value::Null);
    engine.insert("template".to_string(), Value::from(template));
    engine.insert("suggest_url".to_string(), Value::Null);
    engine
}

/// Put a built section into the file: the moved keys win over keys that were already
/// in the section, because the flat keys are what the user of a v0 file set.
fn merge_section(root: &mut Object, name: &str, built: Object) {
    if built.is_empty() {
        return;
    }
    let taken = root.remove(name);
    let mut merged = match taken {
        Some(Value::Object(existing)) => existing,
        _ => Object::new(),
    };
    merged.extend(built);
    root.insert(name.to_string(), Value::Object(merged));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> Value {
        serde_json::from_str(text).expect("the test text is valid json")
    }

    fn section<'a>(root: &'a Value, name: &str, key: &str) -> Option<&'a Value> {
        root.get(name)?.get(key)
    }

    #[test]
    fn v0_flat_file_migrates() {
        let text = r#"{
            "home": "https://example.org/start",
            "search_engine": "ddg",
            "dnt": true,
            "zoom": 120,
            "theme": "dark",
            "sync_device": "juno-1"
        }"#;
        let (root, warnings) = migrate(parsed(text)).expect("v0 is upgradeable");

        assert_eq!(root.get("version"), Some(&Value::from(1)));
        assert_eq!(
            section(&root, "general", "home_page"),
            Some(&Value::from("https://example.org/start"))
        );
        assert_eq!(section(&root, "search", "default_engine"), Some(&Value::from("duckduckgo")));
        assert_eq!(section(&root, "privacy", "do_not_track"), Some(&Value::from(true)));
        assert_eq!(
            section(&root, "appearance", "default_zoom_percent"),
            Some(&Value::from(120))
        );
        assert_eq!(section(&root, "appearance", "theme"), Some(&Value::from("dark")));

        // The flat keys are gone, and a key the chain does not know stays at top level
        // for the loader to keep as unknown.
        for gone in ["home", "search_engine", "dnt", "zoom", "theme"] {
            assert!(root.get(gone).is_none(), "{gone} stayed at top level: {root}");
        }
        assert_eq!(root.get("sync_device"), Some(&Value::from("juno-1")));

        // Hand-worked: one step record, then the unknown key the loader is told about.
        // The chain itself does not report unknown keys, so this is the single warning.
        assert_eq!(warnings, vec![Warning::Migrated { from: 0, to: 1 }]);

        // Reading the migrated file gives the same values; the key the chain did not
        // know is the one thing the loader still has to mention.
        let (settings, read_warnings) =
            Settings::from_json(&root.to_string()).expect("the upgraded file reads");
        assert_eq!(settings.general.home_page, "https://example.org/start");
        assert_eq!(settings.search.default_engine, "duckduckgo");
        assert!(settings.privacy.do_not_track);
        assert_eq!(settings.appearance.default_zoom_percent, 120);
        assert_eq!(settings.appearance.theme, Theme::Dark);
        assert_eq!(settings.unknown.get("sync_device"), Some(&Value::from("juno-1")));
        assert_eq!(read_warnings, vec![Warning::UnknownKeyKept("sync_device".to_string())]);
    }

    #[test]
    fn v0_search_engine_url_becomes_custom_engine() {
        let text = r#"{"search_engine": "https://search.example.org/?q=%s&hl=en"}"#;
        let (root, _) = migrate(parsed(text)).expect("a url is upgradeable");

        assert_eq!(section(&root, "search", "default_engine"), Some(&Value::from("custom")));
        let engines = root.get("search").and_then(|search| search.get("engines"));
        let Value::Array(engines) = engines.expect("an engine list was built") else {
            panic!("engines is not a list: {engines:?}");
        };
        assert_eq!(engines.len(), 1);
        let engine = &engines[0];
        assert_eq!(engine.get("id"), Some(&Value::from("custom")));
        assert_eq!(engine.get("name"), Some(&Value::from("Custom")));
        assert_eq!(engine.get("keyword"), Some(&Value::Null));
        // Hand-worked: "%s" is replaced by "{searchTerms}", the rest is untouched.
        assert_eq!(
            engine.get("template"),
            Some(&Value::from("https://search.example.org/?q={searchTerms}&hl=en"))
        );
        assert_eq!(engine.get("suggest_url"), Some(&Value::Null));

        // A name that is neither the built-in nor a search url is left for the loader,
        // which keeps it as an unknown key instead of inventing an engine.
        let (root, warnings) = migrate(parsed(r#"{"search_engine": "bing"}"#)).expect("reads");
        assert_eq!(root.get("search"), None);
        assert_eq!(root.get("search_engine"), Some(&Value::from("bing")));
        assert_eq!(warnings, vec![Warning::Migrated { from: 0, to: 1 }]);
        let (settings, _) = Settings::from_json(&root.to_string()).expect("reads again");
        assert_eq!(settings.unknown.get("search_engine"), Some(&Value::from("bing")));
        assert_eq!(settings.search.default_engine, "duckduckgo");

        // A url that cannot be a search url is unreadable for the same reason: it is
        // kept, not turned into an engine that would never return results.
        let (root, _) =
            migrate(parsed(r#"{"search_engine": "https://search.example.org/static"}"#)).expect("reads");
        assert_eq!(root.get("search"), None);
        assert_eq!(root.get("search_engine"), Some(&Value::from("https://search.example.org/static")));
    }

    #[test]
    fn v0_missing_version_is_version_0() {
        let (root, warnings) = migrate(parsed(r#"{"zoom": 110}"#)).expect("a plain file is v0");
        assert_eq!(root.get("version"), Some(&Value::from(1)));
        assert_eq!(warnings, vec![Warning::Migrated { from: 0, to: 1 }]);

        // An explicit null says the same thing as no key at all, silently.
        let (root, warnings) = migrate(parsed(r#"{"version": null, "zoom": 110}"#)).expect("reads");
        assert_eq!(root.get("version"), Some(&Value::from(1)));
        assert_eq!(warnings, vec![Warning::Migrated { from: 0, to: 1 }]);

        // A version that is neither is reported, then read as 0 so nothing is refused.
        let (root, warnings) = migrate(parsed(r#"{"version": "one", "zoom": 110}"#)).expect("reads");
        assert_eq!(root.get("version"), Some(&Value::from(1)));
        assert_eq!(
            warnings,
            vec![
                Warning::Dropped {
                    path: "version".to_string(),
                    reason: "expected a number, found a string".to_string()
                },
                Warning::Migrated { from: 0, to: 1 }
            ]
        );
    }

    #[test]
    fn future_version_is_too_new_error() {
        // Hand-worked: the chain ends at CURRENT_VERSION (1), so 2 and 200 are both
        // above it and each reports its own number.
        assert_eq!(
            migrate(parsed(r#"{"version":2}"#)).expect_err("a newer file is refused"),
            SettingsError::TooNew { found: 2, supported: CURRENT_VERSION }
        );
        assert_eq!(
            migrate(parsed(r#"{"version":200,"theme":"dark"}"#)).expect_err("still refused"),
            SettingsError::TooNew { found: 200, supported: CURRENT_VERSION }
        );
        // Refusing is not reading: no warning survives an error.
        assert!(migrate(parsed(r#"{"version":2,"zoom":110}"#)).map(|(_, warnings)| warnings).is_err());
        // A number too large for a u32 is still newer than the chain.
        assert_eq!(
            migrate(parsed(r#"{"version":4294967296}"#)).expect_err("refused"),
            SettingsError::TooNew { found: u32::MAX, supported: CURRENT_VERSION }
        );
        // Version 1 exactly is the current file: no step, no warning, no error.
        let (root, warnings) = migrate(parsed(r#"{"version":1,"theme":"dark"}"#)).expect("reads");
        assert_eq!(root.get("version"), Some(&Value::from(1)));
        assert_eq!(warnings, Vec::new());
    }

    #[test]
    fn non_object_is_error() {
        for text in [r#"[1,2]"#, r#""zoom""#, r#"42"#, "null"] {
            let value = parsed(text);
            assert_eq!(migrate(value).expect_err("not a settings object"), SettingsError::NotAnObject, "accepted {text}");
            assert_eq!(
                Settings::from_json(text).expect_err("not a settings object"),
                SettingsError::NotAnObject,
                "loader accepted {text}"
            );
        }
    }

    #[test]
    fn garbage_is_syntax_error() {
        // Hand-worked: the syntax error is raised before the chain runs, so the whole
        // text of the mistake is what the caller gets and no warning is produced.
        for text in ["", "   ", "{", r#"{"zoom": }"#, r#"{"a": 1,}"#, "not json at all"] {
            match Settings::from_json(text) {
                Err(SettingsError::Syntax(message)) => assert!(!message.is_empty(), "empty message for {text:?}"),
                other => panic!("expected a syntax error for {text:?}, got {other:?}"),
            }
        }
    }

    /// A step that records that the 0 -> 1 link ran.
    fn tag_first(value: Value) -> Value {
        let mut root = match value {
            Value::Object(root) => root,
            found => return found,
        };
        root.insert("first".to_string(), Value::from(true));
        Value::Object(root)
    }

    /// A step that can only be true if it ran after `tag_first`.
    fn tag_second(value: Value) -> Value {
        let mut root = match value {
            Value::Object(root) => root,
            found => return found,
        };
        root.insert(
            "second".to_string(),
            Value::from(root.get("first") == Some(&Value::from(true))),
        );
        Value::Object(root)
    }

    /// A chain of two steps, 0 -> 1 -> 2, used to test the order without waiting for
    /// a second real version of the file.
    const TEST_CHAIN: &[Step] = &[
        Step {
            from: 0,
            to: 1,
            run: tag_first,
        },
        Step {
            from: 1,
            to: 2,
            run: tag_second,
        },
    ];

    #[test]
    fn migration_chain_runs_steps_in_order() {
        let (root, warnings) = migrate_with(parsed(r#"{"version":0}"#), TEST_CHAIN).expect("upgrades");
        assert_eq!(root.get("version"), Some(&Value::from(2)));
        assert_eq!(root.get("first"), Some(&Value::from(true)));
        // Hand-worked: the second step saw the key the first one wrote, so the order held.
        assert_eq!(root.get("second"), Some(&Value::from(true)));
        assert_eq!(
            warnings,
            vec![
                Warning::Migrated { from: 0, to: 1 },
                Warning::Migrated { from: 1, to: 2 },
            ]
        );

        // A file already at the middle of the chain gets only the steps above it.
        let (root, warnings) = migrate_with(parsed(r#"{"version":1}"#), TEST_CHAIN).expect("upgrades");
        assert_eq!(root.get("version"), Some(&Value::from(2)));
        assert_eq!(root.get("first"), None, "the 0 -> 1 step ran twice");
        assert_eq!(warnings, vec![Warning::Migrated { from: 1, to: 2 }]);

        // A file at the top gets none, and a step that does not return an object is a
        // file that was never settings.
        let (_, warnings) = migrate_with(parsed(r#"{"version":2}"#), TEST_CHAIN).expect("current");
        assert_eq!(warnings, Vec::new());
        fn breaks(value: Value) -> Value {
            match value {
                Value::Object(_) => Value::Array(Vec::new()),
                found => found,
            }
        }
        const BROKEN: &[Step] = &[Step {
            from: 0,
            to: 1,
            run: breaks,
        }];
        assert_eq!(
            migrate_with(parsed(r#"{"zoom":110}"#), BROKEN).expect_err("a step that loses the object"),
            SettingsError::NotAnObject
        );
    }

    #[test]
    fn chain_ends_at_the_current_version() {
        // If this fails, CURRENT_VERSION was raised without adding the step that
        // reaches it, and v0 files would be refused as too new.
        assert_eq!(CHAIN.last().expect("at least one step").to, CURRENT_VERSION);
        assert_eq!(CHAIN.first().expect("at least one step").from, 0);
        for step in CHAIN.windows(2) {
            assert_eq!(step[0].to, step[1].from, "the chain has a gap");
        }
    }
}
