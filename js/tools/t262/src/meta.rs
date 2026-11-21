//! YAML front-matter parsing for test262 tests.

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct FrontMatter {
    pub includes: Vec<String>,
    pub flags: Vec<String>,
    pub negative: Option<Negative>,
    pub features: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Negative {
    pub phase: String,
    #[serde(rename = "type")]
    pub error_type: String,
}

/// Extract and parse the YAML front matter between `/*---` and `---*/`.
/// Returns `None` if the test has no front matter.
pub fn parse_front_matter(source: &str) -> Option<FrontMatter> {
    let start = source.find("/*---")? + "/*---".len();
    let end = source[start..].find("---*/")? + start;
    let yaml = &source[start..end];
    match serde_yaml::from_str::<FrontMatter>(yaml) {
        Ok(fm) => Some(fm),
        Err(_) => {
            // Fall back to a lenient parse: keep only what we understand.
            let value: serde_yaml::Value = serde_yaml::from_str(yaml).ok()?;
            Some(FrontMatter {
                includes: string_list(&value, "includes"),
                flags: string_list(&value, "flags"),
                negative: value.get("negative").and_then(|n| {
                    Some(Negative {
                        phase: n.get("phase")?.as_str()?.to_string(),
                        error_type: n.get("type")?.as_str()?.to_string(),
                    })
                }),
                features: string_list(&value, "features"),
            })
        }
    }
}

fn string_list(value: &serde_yaml::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Strict,
    Sloppy,
    Module,
    Raw,
}

impl Variant {
    pub fn as_str(&self) -> &'static str {
        match self {
            Variant::Strict => "strict",
            Variant::Sloppy => "sloppy",
            Variant::Module => "module",
            Variant::Raw => "raw",
        }
    }
}

/// Decide which variants a test must run in, per INTERPRETING.md.
pub fn variants_for(fm: &FrontMatter) -> Vec<Variant> {
    let has = |f: &str| fm.flags.iter().any(|x| x == f);
    if has("raw") {
        vec![Variant::Raw]
    } else if has("module") {
        vec![Variant::Module]
    } else if has("onlyStrict") {
        vec![Variant::Strict]
    } else if has("noStrict") {
        vec![Variant::Sloppy]
    } else {
        vec![Variant::Sloppy, Variant::Strict]
    }
}
