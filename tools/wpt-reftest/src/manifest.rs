//! Reads the vendored WPT `MANIFEST.json` (version 9) and selects tests from it.
//!
//! The file is a nested trie: `items[type][suite][...path segments...][file] = [hash, [timeout, refs,
//! extra]]`. It is ~40 MB and 166k leaves, of which only ~63k are types this harness can run, so the
//! deserializer drops the content hashes and skips the unrunnable types with `IgnoredAny` instead of
//! building a `serde_json::Value` for the whole document.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::de::{self, Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use crate::compare::Fuzzy;

/// Types that are never runnable by `--render`: they need a human, a WebDriver session, a spec runner,
/// or are not tests at all. Skipping them here is most of the memory saving.
const UNRUNNABLE: &[&str] = &[
    "aamtest",
    "manual",
    "print-reftest",
    "support",
    "test262",
    "visual",
    "wdspec",
];

/// The types this harness knows how to score, in the order a selection walks them.
pub const RUNNABLE: &[&str] = &["reftest", "testharness", "crashtest"];

enum Node {
    /// Every payload after the content hash. A file usually has one, `[url_or_null, refs-or-meta,
    /// extra?]`, but 42 reftest files and 2389 testharness files carry several: query-string variants,
    /// or the `.any.js` -> `.any.html` / `.any.worker.html` expansion. Each is a separate runnable test.
    /// The hash itself is dropped on purpose.
    Leaf(Vec<Vec<Value>>),
    Dir(BTreeMap<String, Node>),
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Node;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a WPT manifest directory object or test leaf array")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
                let mut dir = BTreeMap::new();
                while let Some((k, v)) = map.next_entry::<String, Node>()? {
                    dir.insert(k, v);
                }
                Ok(Node::Dir(dir))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
                // [hash, payload, payload, ...]: the hash is 40 hex chars we never score against. The
                // sequence must be drained to the end or serde_json reports trailing characters.
                let _hash: IgnoredAny = seq.next_element()?.ok_or_else(|| de::Error::custom("empty leaf"))?;
                let mut payloads = Vec::new();
                while let Some(p) = seq.next_element::<Vec<Value>>()? {
                    payloads.push(p);
                }
                Ok(Node::Leaf(payloads))
            }
        }
        d.deserialize_any(V)
    }
}

pub struct Manifest {
    pub version: u64,
    pub url_base: String,
    items: BTreeMap<String, Node>,
}

impl<'de> Deserialize<'de> for Manifest {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Manifest;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a WPT manifest object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Manifest, A::Error> {
                let (mut version, mut url_base, mut items) = (0, "/".to_string(), BTreeMap::new());
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "version" => version = map.next_value()?,
                        "url_base" => url_base = map.next_value()?,
                        "items" => items = map.next_value::<Items>()?.0,
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        },
                    }
                }
                Ok(Manifest { version, url_base, items })
            }
        }
        struct Items(BTreeMap<String, Node>);
        impl<'de> Deserialize<'de> for Items {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct IV;
                impl<'de> Visitor<'de> for IV {
                    type Value = Items;
                    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                        f.write_str("the manifest's items object")
                    }
                    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Items, A::Error> {
                        let mut out = BTreeMap::new();
                        while let Some(kind) = map.next_key::<String>()? {
                            if UNRUNNABLE.contains(&kind.as_str()) {
                                map.next_value::<IgnoredAny>()?;
                            } else {
                                out.insert(kind, map.next_value::<Node>()?);
                            }
                        }
                        Ok(Items(out))
                    }
                }
                d.deserialize_map(IV)
            }
        }
        d.deserialize_map(V)
    }
}

/// One `==`/`!=` reference of a reftest, as the manifest records it.
#[derive(Clone, Debug)]
pub struct Relation {
    /// The manifest's reference URL, usually root-relative (`/css/...-ref.html`).
    pub url: String,
    pub op: String,
}

#[derive(Clone, Debug)]
pub struct Test {
    /// Path relative to the WPT root, no leading slash: `css/css-masking/clip/...html`.
    pub path: String,
    pub kind: String,
    pub refs: Vec<Relation>,
    /// `(scope, tolerance)`. A `None` scope applies to every reference; a `Some(url)` scope to that one.
    pub fuzzy: Vec<(Option<String>, Fuzzy)>,
}

impl Test {
    /// The file part of the path, without a variant's query string.
    pub fn file(&self) -> &str {
        self.path.split(['?', '#']).next().unwrap_or(&self.path)
    }

    /// A filesystem-safe, collision-free stem for this test's PNG and log. Variants of one file get a
    /// hash of their query appended so they cannot overwrite each other.
    pub fn artifact(&self) -> String {
        match self.path.find(['?', '#']) {
            Some(i) => format!("{}-{:08x}", &self.path[..i], crate::fnv(&self.path[i..]) as u32),
            None => self.path.clone(),
        }
    }

    /// The tolerance for one reference: a scope that names it wins over the unscoped entry, and if
    /// several match the most permissive is used. Our subsets have no scoped entries at all.
    pub fn fuzzy_for(&self, ref_url: &str) -> Fuzzy {
        let mut best = None;
        let mut scoped = false;
        for (scope, f) in &self.fuzzy {
            let matches = match scope {
                None => true,
                Some(s) => s == ref_url || s.trim_start_matches('/') == ref_url.trim_start_matches('/'),
            };
            if !matches {
                continue;
            }
            let is_scoped = scope.is_some();
            if best.is_none() || (is_scoped && !scoped) {
                best = Some(*f);
                scoped = is_scoped;
            } else if is_scoped == scoped {
                let b = best.unwrap();
                best = Some(Fuzzy {
                    max_difference: b.max_difference.max(f.max_difference),
                    max_pixels: b.max_pixels.max(f.max_pixels),
                });
            }
        }
        best.unwrap_or(Fuzzy::EXACT)
    }

    /// The directory this test is counted under: the include prefix it matched, else its top-level suite.
    pub fn dir_under(&self, includes: &[String]) -> String {
        for inc in includes {
            let inc = inc.trim_start_matches('/').trim_end_matches('/');
            if !inc.is_empty() && (self.path == inc || self.path.starts_with(&format!("{inc}/"))) {
                return inc.to_string();
            }
        }
        self.path.split('/').next().unwrap_or(&self.path).to_string()
    }
}

pub fn load(path: &Path) -> Result<Manifest, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let m: Manifest = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if m.version != 9 {
        return Err(format!("{}: manifest version {} (expected 9)", path.display(), m.version));
    }
    Ok(m)
}

/// Default exclusions: variants the localhost-only, http-only server cannot serve at all. `.sub.` is
/// deliberately NOT excluded -- `wpt serve` does server-side substitution.
pub const DEFAULT_EXCLUDES: &[&str] = &[".https.", ".h2.", ".webtransport.", ".http2."];

pub struct Select<'a> {
    pub types: &'a [String],
    pub includes: &'a [String],
    pub excludes: &'a [String],
    pub limit: Option<usize>,
}

/// Walk the selected types in a fixed order and the paths sorted, so `--limit` is reproducible.
pub fn select(m: &Manifest, sel: &Select) -> Vec<Test> {
    let mut out = Vec::new();
    for kind in sel.types {
        let Some(root) = m.items.get(kind) else { continue };
        let mut path = String::new();
        walk(root, &mut path, kind, sel, &mut out);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path).then(a.kind.cmp(&b.kind)));
    if let Some(n) = sel.limit {
        out.truncate(n);
    }
    out
}

fn walk(node: &Node, path: &mut String, kind: &str, sel: &Select, out: &mut Vec<Test>) {
    if let Some(n) = sel.limit {
        if out.len() >= n {
            return;
        }
    }
    match node {
        Node::Dir(dir) => {
            for (name, child) in dir {
                let len = path.len();
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(name);
                walk(child, path, kind, sel, out);
                path.truncate(len);
            }
        },
        Node::Leaf(payloads) => {
            if !wanted(path, sel) {
                return;
            }
            for payload in payloads {
                if sel.limit.is_some_and(|n| out.len() >= n) {
                    return;
                }
                if let Some(t) = leaf(path, kind, payload) {
                    out.push(t);
                }
            }
        },
    }
}

fn wanted(path: &str, sel: &Select) -> bool {
    if sel.excludes.iter().any(|e| path.contains(e.as_str())) {
        return false;
    }
    if sel.includes.is_empty() {
        return true;
    }
    sel.includes.iter().any(|inc| {
        let inc = inc.trim_start_matches('/').trim_end_matches('/');
        inc.is_empty() || path == inc || path.starts_with(&format!("{inc}/"))
    })
}

/// One payload: `[url_or_null, refs, extra]` for a reftest, `[url_or_null, meta]` for testharness and
/// crashtest. A null first element means "the file itself"; a string is the variant's own URL, which is
/// how `.any.js` becomes `.any.html` and `.any.worker.html`, and how query-string variants are named.
fn leaf(file_path: &str, kind: &str, payload: &[Value]) -> Option<Test> {
    let path = payload.first().and_then(Value::as_str).unwrap_or(file_path).to_string();
    let mut refs = Vec::new();
    if kind == "reftest" {
        if let Some(list) = payload.get(1).and_then(Value::as_array) {
            for r in list {
                let pair = r.as_array()?;
                refs.push(Relation {
                    url: pair.first()?.as_str()?.to_string(),
                    op: pair.get(1).and_then(Value::as_str).unwrap_or("==").to_string(),
                });
            }
        }
    }
    let mut fuzzy = Vec::new();
    if let Some(entries) = payload.get(2).and_then(|e| e.get("fuzzy")).and_then(Value::as_array) {
        for entry in entries {
            let Some(pair) = entry.as_array() else { continue };
            let scope = pair.first().and_then(Value::as_str).map(str::to_string);
            if let Some(tol) = pair.get(1).and_then(Value::as_array).and_then(|a| Fuzzy::from_manifest(a)) {
                fuzzy.push((scope, tol));
            }
        }
    }
    Some(Test { path, kind: kind.to_string(), refs, fuzzy })
}