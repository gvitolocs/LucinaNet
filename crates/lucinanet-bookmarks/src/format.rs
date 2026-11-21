/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Our own file format (version 1): nested by folder, children in order, ids stored, fixed field order.

use std::collections::HashSet;

use serde::Deserialize;

use crate::store::{Bookmarks, Entry, clip_title, normalize_keyword, normalize_tags, normalize_url};
use crate::{Error, Id, Kind, MENU, Node, OTHER, ROOT, TOOLBAR};

const VERSION: u32 = 1;

pub(crate) fn js(s: &str) -> String {
    // A `&str` always serialises; the fallback only keeps the function total.
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

#[derive(Deserialize)]
struct Doc {
    version: u32,
    next_id: u64,
    nodes: Vec<JNode>,
}

#[derive(Deserialize)]
struct JNode {
    id: u64,
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    added: i64,
    #[serde(default)]
    modified: i64,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    keyword: Option<String>,
    #[serde(default)]
    children: Vec<JNode>,
}

fn bad(msg: &str) -> Error {
    Error::Format(msg.to_string())
}

impl Bookmarks {
    fn write_json_node(&self, id: Id, out: &mut String) {
        let Some(e) = self.entries.get(&id) else { return };
        let n = &e.node;
        match &n.kind {
            Kind::Folder => {
                out.push_str(&format!(
                    "{{\"id\":{},\"title\":{},\"type\":\"folder\",\"added\":{},\"modified\":{},\"children\":[",
                    n.id,
                    js(&n.title),
                    n.added_us,
                    n.modified_us
                ));
                for (i, c) in e.children.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    self.write_json_node(*c, out);
                }
                out.push_str("]}");
            }
            Kind::Bookmark { url } => {
                out.push_str(&format!(
                    "{{\"id\":{},\"type\":\"bookmark\",\"title\":{},\"url\":{},\"added\":{},\"modified\":{}",
                    n.id,
                    js(&n.title),
                    js(url),
                    n.added_us,
                    n.modified_us
                ));
                if !n.tags.is_empty() {
                    let tags: Vec<String> = n.tags.iter().map(|t| js(t)).collect();
                    out.push_str(&format!(",\"tags\":[{}]", tags.join(",")));
                }
                if let Some(k) = &n.keyword {
                    out.push_str(&format!(",\"keyword\":{}", js(k)));
                }
                out.push('}');
            }
            Kind::Separator => {
                out.push_str(&format!(
                    "{{\"id\":{},\"type\":\"separator\",\"added\":{},\"modified\":{}}}",
                    n.id, n.added_us, n.modified_us
                ));
            }
        }
    }

    /// Own format, version 1. Deterministic: same tree, same bytes.
    pub fn to_json(&self) -> String {
        let mut out = format!("{{\"version\":{VERSION},\"next_id\":{},\"nodes\":[", self.next_id);
        if let Some(root) = self.entries.get(&ROOT) {
            for (i, c) in root.children.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                self.write_json_node(*c, &mut out);
            }
        }
        out.push_str("]}");
        out
    }

    fn load_json_node(&mut self, parent: Id, j: &JNode, seen: &mut HashSet<Id>) -> Result<(), Error> {
        if !seen.insert(j.id) {
            return Err(Error::Format(format!("duplicate id {}", j.id)));
        }
        let kind = match j.ty.as_str() {
            "folder" => Kind::Folder,
            "separator" => Kind::Separator,
            "bookmark" => {
                let raw = j.url.as_deref().ok_or_else(|| bad("bookmark without url"))?;
                let url = normalize_url(raw).map_err(|_| Error::Format(format!("bad url for id {}", j.id)))?;
                Kind::Bookmark { url }
            }
            other => return Err(Error::Format(format!("unknown node type {other}"))),
        };
        let is_bookmark = matches!(kind, Kind::Bookmark { .. });
        let is_folder = matches!(kind, Kind::Folder);
        let title = if matches!(kind, Kind::Separator) { String::new() } else { clip_title(&j.title) };
        self.entries.insert(
            j.id,
            Entry {
                node: Node {
                    id: j.id,
                    parent: Some(parent),
                    kind,
                    title,
                    added_us: j.added,
                    modified_us: j.modified,
                    tags: if is_bookmark { normalize_tags(j.tags.iter().map(String::as_str)) } else { Vec::new() },
                    keyword: if is_bookmark { normalize_keyword(j.keyword.as_deref()) } else { None },
                },
                children: Vec::new(),
            },
        );
        if let Some(p) = self.entries.get_mut(&parent) {
            p.children.push(j.id);
        }
        if is_folder {
            for c in &j.children {
                self.load_json_node(j.id, c, seen)?;
            }
        }
        Ok(())
    }

    /// Reads our own format. Unknown extra fields are ignored. Rejects an unknown `version`, duplicate ids, a `next_id`
    /// that is not above every id, a missing or misplaced built-in folder, and a bookmark with a missing, empty or
    /// unparsable URL.
    pub fn from_json(text: &str) -> Result<Bookmarks, Error> {
        let doc: Doc = serde_json::from_str(text).map_err(|e| Error::Format(e.to_string()))?;
        if doc.version != VERSION {
            return Err(Error::Format(format!("unsupported version {}", doc.version)));
        }
        let builtins = [TOOLBAR, MENU, OTHER];
        if doc.nodes.len() != builtins.len()
            || doc.nodes.iter().zip(builtins).any(|(n, id)| n.id != id || n.ty != "folder")
        {
            return Err(bad("built-in folder missing or not a folder"));
        }
        let mut bm = Bookmarks::new();
        let mut seen: HashSet<Id> = HashSet::from([ROOT, TOOLBAR, MENU, OTHER]);
        for (top, id) in doc.nodes.iter().zip(builtins) {
            if let Some(e) = bm.entries.get_mut(&id) {
                e.node.added_us = top.added;
                e.node.modified_us = top.modified;
            }
            for c in &top.children {
                bm.load_json_node(id, c, &mut seen)?;
            }
        }
        let max = seen.iter().copied().max().unwrap_or(OTHER);
        if doc.next_id <= max {
            return Err(bad("next_id is not above every id"));
        }
        bm.next_id = doc.next_id;
        Ok(bm)
    }
}
