/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Pure helpers used to bake the resolved page style into the serialised copy of
//! an inline `<svg>`. The serialised copy is rasterised in isolation, so the
//! page's stylesheets and the inherited properties of its HTML ancestors never
//! reach it unless we write them into the copy.

use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// A property that is baked into the serialised SVG.
pub(crate) struct BakedProperty {
    pub(crate) name: &'static str,
    /// Inherited properties are skipped on a child when its value equals the
    /// parent's, because the copy inherits it anyway.
    pub(crate) inherited: bool,
    /// Whether the property may be baked on the root `<svg>`. Non inherited
    /// properties on the root are box effects that HTML layout already applies
    /// to the replaced element, so baking them would apply them twice.
    pub(crate) on_root: bool,
}

const fn inh(name: &'static str) -> BakedProperty {
    BakedProperty {
        name,
        inherited: true,
        on_root: true,
    }
}

const fn own(name: &'static str) -> BakedProperty {
    BakedProperty {
        name,
        inherited: false,
        on_root: false,
    }
}

pub(crate) const BAKED_PROPERTIES: &[BakedProperty] = &[
    own("opacity"),
    inh("fill"),
    inh("fill-opacity"),
    inh("fill-rule"),
    inh("stroke"),
    inh("stroke-width"),
    inh("stroke-opacity"),
    inh("stroke-linecap"),
    inh("stroke-linejoin"),
    inh("stroke-miterlimit"),
    inh("stroke-dasharray"),
    inh("stroke-dashoffset"),
    inh("color"),
    inh("visibility"),
    inh("clip-rule"),
    own("clip-path"),
    own("mask"),
    own("filter"),
    inh("font-family"),
    inh("font-size"),
    inh("font-weight"),
    inh("font-style"),
    inh("letter-spacing"),
    inh("text-anchor"),
    own("dominant-baseline"),
    own("transform"),
    own("display"),
];

/// Whether `value` is the initial value of `prop` for an SVG element, in the
/// form the resolved style serialises it. Font family and size have no usable
/// initial value for the isolated image, so they are never initial.
pub(crate) fn is_initial(prop: &str, value: &str) -> bool {
    let value = value.trim();
    match prop {
        "opacity" | "fill-opacity" | "stroke-opacity" => value == "1",
        "fill" | "color" => matches!(value, "rgb(0, 0, 0)" | "black" | "canvastext"),
        "fill-rule" | "clip-rule" => value == "nonzero",
        "stroke" | "stroke-dasharray" | "clip-path" | "mask" | "filter" | "transform" => {
            value == "none"
        },
        "stroke-width" => value == "1px" || value == "1",
        "stroke-linecap" => value == "butt",
        "stroke-linejoin" => value == "miter",
        "stroke-miterlimit" => value == "4",
        "stroke-dashoffset" => value == "0px" || value == "0",
        "visibility" => value == "visible",
        "font-weight" => value == "400" || value == "normal",
        "font-style" | "letter-spacing" => value == "normal",
        "text-anchor" => value == "start",
        "dominant-baseline" => value == "auto",
        // `display` is only baked when it resolves to `none`.
        "display" => value != "none",
        _ => false,
    }
}

/// A value that can be written into a `style` attribute without breaking the
/// declaration list.
fn is_safe_value(value: &str) -> bool {
    !value.is_empty() &&
        !value
            .chars()
            .any(|c| matches!(c, ';' | '{' | '}' | '<' | '\n' | '\r' | '\\'))
}

/// Merge `baked` declarations after the `existing` ones so they win. Exactly
/// one declaration per property remains, `;`-separated.
pub(crate) fn bake_style(existing: &str, baked: &[(&str, String)]) -> String {
    let mut out: Vec<(String, String)> = Vec::new();
    for decl in existing.split(';') {
        let Some((name, value)) = decl.split_once(':') else {
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() || value.is_empty() {
            continue;
        }
        push_decl(&mut out, name, value);
    }
    for (name, value) in baked {
        if !is_safe_value(value) {
            continue;
        }
        push_decl(&mut out, name, value.trim());
    }
    out.iter()
        .map(|(name, value)| format!("{name}:{value}"))
        .collect::<Vec<_>>()
        .join(";")
}

fn push_decl(out: &mut Vec<(String, String)>, name: &str, value: &str) {
    let name = name.to_ascii_lowercase();
    out.retain(|(existing, _)| *existing != name);
    out.push((name, value.to_owned()));
}

/// Rewrite the `url(...)` references of a resolved value so they point at the
/// fragment inside the serialised copy (`url(#id)`). Returns `None` when a
/// reference points at an id that is not part of the subtree, because it cannot
/// resolve in isolation.
pub(crate) fn localize_url_refs(value: &str, ids: &HashSet<String>) -> Option<String> {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("url(") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        let end = after.find(')')?;
        let inner = after[..end].trim().trim_matches(|c| c == '"' || c == '\'');
        let (_, fragment) = inner.rsplit_once('#')?;
        if !ids.contains(fragment) {
            return None;
        }
        out.push_str("url(#");
        out.push_str(fragment);
        out.push(')');
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// A fingerprint of the baked declarations of a whole subtree, in preorder.
pub(crate) fn fingerprint(baked: &[Vec<(&str, String)>]) -> u64 {
    let mut hasher = DefaultHasher::new();
    baked.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baked_values_go_after_existing_and_win() {
        let merged = bake_style(
            "fill: red; opacity: 0.5",
            &[("fill", "rgb(0, 128, 0)".to_owned())],
        );
        assert_eq!(merged, "opacity:0.5;fill:rgb(0, 128, 0)");
    }

    #[test]
    fn no_duplicate_properties() {
        let merged = bake_style(
            "fill:red;fill:blue",
            &[
                ("opacity", "0".to_owned()),
                ("opacity", "0.2".to_owned()),
            ],
        );
        assert_eq!(merged, "fill:blue;opacity:0.2");
    }

    #[test]
    fn unsafe_values_are_dropped() {
        let merged = bake_style(
            "",
            &[
                ("font-family", "a; fill: red".to_owned()),
                ("color", "rgb(0, 0, 255)".to_owned()),
                ("fill", String::new()),
            ],
        );
        assert_eq!(merged, "color:rgb(0, 0, 255)");
    }

    #[test]
    fn empty_inputs_give_empty_style() {
        assert_eq!(bake_style("", &[]), "");
        assert_eq!(bake_style(" ; ;", &[]), "");
    }

    #[test]
    fn initial_values_are_skipped() {
        assert!(is_initial("opacity", "1"));
        assert!(!is_initial("opacity", "0"));
        assert!(is_initial("fill", "rgb(0, 0, 0)"));
        assert!(!is_initial("fill", "rgb(0, 128, 0)"));
        assert!(is_initial("stroke", "none"));
        assert!(is_initial("visibility", "visible"));
        assert!(!is_initial("visibility", "hidden"));
        assert!(is_initial("display", "inline"));
        assert!(!is_initial("display", "none"));
        assert!(!is_initial("font-size", "16px"));
        assert!(!is_initial("unknown", "x"));
    }

    #[test]
    fn url_refs_are_localized() {
        let ids: HashSet<String> = ["g".to_owned()].into();
        assert_eq!(
            localize_url_refs("url(\"https://a.test/p#g\") none", &ids).as_deref(),
            Some("url(#g) none")
        );
        assert_eq!(localize_url_refs("url(\"https://a.test/p#other\")", &ids), None);
        assert_eq!(localize_url_refs("rgb(1, 2, 3)", &ids).as_deref(), Some("rgb(1, 2, 3)"));
    }

    #[test]
    fn fingerprint_changes_with_style() {
        let a = vec![vec![("opacity", "0".to_owned())]];
        let b = vec![vec![("opacity", "1".to_owned())]];
        assert_eq!(fingerprint(&a), fingerprint(&a.clone()));
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }
}
