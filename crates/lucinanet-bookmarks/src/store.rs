/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The tree and its operations.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashSet};

use url::Url;

use crate::{Error, Id, Kind, MENU, Node, OTHER, ROOT, TOOLBAR};

/// Titles are cut here. Firefox: `toolkit/components/places/PlacesUtils.sys.mjs` @33abb90 bounds titles by the DB column; Chromium has no bound,
/// so the spec's 4096 bytes is ours.
const MAX_TITLE_BYTES: usize = 4096;
/// Firefox `Bookmarks.sys.mjs` @33abb90 `MAX_TAG_LENGTH: 100`; longer tags are dropped, not truncated.
const MAX_TAG_CHARS: usize = 100;
/// Firefox `PlacesUtils.sys.mjs` @33abb90 `DB_URL_LENGTH_MAX = 65536`.
const MAX_URL_BYTES: usize = 65536;

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) node: Node,
    pub(crate) children: Vec<Id>,
}

/// The bookmark tree: [`ROOT`] with the three built-in folders, and whatever the user adds below them.
#[derive(Clone, Debug)]
pub struct Bookmarks {
    pub(crate) entries: BTreeMap<Id, Entry>,
    pub(crate) next_id: Id,
}

impl Default for Bookmarks {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn clip_title(s: &str) -> String {
    if s.len() <= MAX_TITLE_BYTES {
        return s.to_string();
    }
    let mut end = MAX_TITLE_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Trim, then parse; the stored form is `Url::as_str`.
pub(crate) fn normalize_url(text: &str) -> Result<String, Error> {
    let t = text.trim();
    if t.is_empty() {
        return Err(Error::EmptyUrl);
    }
    if t.len() > MAX_URL_BYTES {
        return Err(Error::InvalidUrl(clip_title(t)));
    }
    Url::parse(t)
        .map(|u| u.as_str().to_string())
        .map_err(|_| Error::InvalidUrl(t.to_string()))
}

pub(crate) fn normalize_tags<'a>(tags: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = tags
        .into_iter()
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty() && t.chars().count() <= MAX_TAG_CHARS)
        .collect();
    out.sort();
    out.dedup();
    out
}

pub(crate) fn normalize_keyword(k: Option<&str>) -> Option<String> {
    k.map(str::trim).filter(|k| !k.is_empty()).map(str::to_string)
}

fn builtin_node(id: Id, title: &str) -> Entry {
    Entry {
        node: Node {
            id,
            parent: if id == ROOT { None } else { Some(ROOT) },
            kind: Kind::Folder,
            title: title.to_string(),
            added_us: 0,
            modified_us: 0,
            tags: Vec::new(),
            keyword: None,
        },
        children: Vec::new(),
    }
}

impl Bookmarks {
    /// [`ROOT`] and the three built-in folders, ids `0..=3`, next id 4.
    pub fn new() -> Bookmarks {
        let mut entries = BTreeMap::new();
        let mut root = builtin_node(ROOT, "");
        root.children = vec![TOOLBAR, MENU, OTHER];
        entries.insert(ROOT, root);
        entries.insert(TOOLBAR, builtin_node(TOOLBAR, "Bookmarks Toolbar"));
        entries.insert(MENU, builtin_node(MENU, "Bookmarks Menu"));
        entries.insert(OTHER, builtin_node(OTHER, "Other Bookmarks"));
        Bookmarks { entries, next_id: 4 }
    }

    pub fn get(&self, id: Id) -> Option<&Node> {
        self.entries.get(&id).map(|e| &e.node)
    }

    /// Children of a folder in order.
    pub fn children(&self, folder: Id) -> Result<&[Id], Error> {
        let e = self.entries.get(&folder).ok_or(Error::NoSuchNode(folder))?;
        match e.node.kind {
            Kind::Folder => Ok(&e.children),
            _ => Err(Error::NotAFolder(folder)),
        }
    }

    /// Bookmarks only (not folders or separators).
    pub fn len_bookmarks(&self) -> usize {
        self.entries
            .values()
            .filter(|e| matches!(e.node.kind, Kind::Bookmark { .. }))
            .count()
    }

    /// Check that `parent` can take children and return its current child count.
    fn insert_slot(&self, parent: Id, index: Option<usize>) -> Result<usize, Error> {
        let e = self.entries.get(&parent).ok_or(Error::NoSuchNode(parent))?;
        if parent == ROOT {
            return Err(Error::ProtectedNode(ROOT));
        }
        if !matches!(e.node.kind, Kind::Folder) {
            return Err(Error::NotAFolder(parent));
        }
        let len = e.children.len();
        let idx = index.unwrap_or(len);
        if idx > len {
            return Err(Error::IndexOutOfRange);
        }
        Ok(idx)
    }

    /// The one place a node is created; importers use it to keep their own dates, tags and keyword. It leaves
    /// the parent's `modified_us` alone (callers decide).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_raw(
        &mut self,
        parent: Id,
        index: Option<usize>,
        kind: Kind,
        title: &str,
        added_us: i64,
        modified_us: i64,
        tags: Vec<String>,
        keyword: Option<String>,
    ) -> Result<Id, Error> {
        let idx = self.insert_slot(parent, index)?;
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let title = if matches!(kind, Kind::Separator) {
            String::new()
        } else {
            clip_title(title)
        };
        self.entries.insert(
            id,
            Entry {
                node: Node {
                    id,
                    parent: Some(parent),
                    kind,
                    title,
                    added_us,
                    modified_us,
                    tags,
                    keyword,
                },
                children: Vec::new(),
            },
        );
        if let Some(p) = self.entries.get_mut(&parent) {
            p.children.insert(idx, id);
        }
        Ok(id)
    }

    fn touch(&mut self, id: Id, now_us: i64) {
        if let Some(e) = self.entries.get_mut(&id) {
            e.node.modified_us = now_us;
        }
    }

    /// `index = None` appends; `Some(i)` inserts at `i` (`i == len` is valid, larger is `IndexOutOfRange`).
    /// The URL is trimmed and stored in normalised form.
    pub fn add_bookmark(&mut self, parent: Id, index: Option<usize>, title: &str, url: &str, now_us: i64) -> Result<Id, Error> {
        let url = normalize_url(url)?;
        let id = self.insert_raw(parent, index, Kind::Bookmark { url }, title, now_us, now_us, Vec::new(), None)?;
        self.touch(parent, now_us);
        Ok(id)
    }

    pub fn add_folder(&mut self, parent: Id, index: Option<usize>, title: &str, now_us: i64) -> Result<Id, Error> {
        let id = self.insert_raw(parent, index, Kind::Folder, title, now_us, now_us, Vec::new(), None)?;
        self.touch(parent, now_us);
        Ok(id)
    }

    pub fn add_separator(&mut self, parent: Id, index: Option<usize>, now_us: i64) -> Result<Id, Error> {
        let id = self.insert_raw(parent, index, Kind::Separator, "", now_us, now_us, Vec::new(), None)?;
        self.touch(parent, now_us);
        Ok(id)
    }

    /// Descendants of `from` (not `from` itself) in tree order with their depth below `from` (children are 0).
    /// Iterative, so deep trees cannot overflow the stack.
    fn descendants(&self, from: Id) -> Vec<(usize, Id)> {
        let mut out = Vec::new();
        let mut stack: Vec<(usize, Id)> = Vec::new();
        if let Some(e) = self.entries.get(&from) {
            stack.extend(e.children.iter().rev().map(|c| (0, *c)));
        }
        while let Some((depth, id)) = stack.pop() {
            out.push((depth, id));
            if let Some(e) = self.entries.get(&id) {
                stack.extend(e.children.iter().rev().map(|c| (depth + 1, *c)));
            }
        }
        out
    }

    /// Removes the subtree; returns how many nodes went. Built-in folders and ROOT are protected.
    pub fn remove(&mut self, id: Id) -> Result<usize, Error> {
        let node = self.get(id).ok_or(Error::NoSuchNode(id))?;
        if id <= OTHER {
            return Err(Error::ProtectedNode(id));
        }
        let parent = node.parent;
        let mut doomed = vec![id];
        doomed.extend(self.descendants(id).into_iter().map(|(_, c)| c));
        if let Some(p) = parent.and_then(|p| self.entries.get_mut(&p)) {
            p.children.retain(|c| *c != id);
        }
        for d in &doomed {
            self.entries.remove(d);
        }
        Ok(doomed.len())
    }

    /// Move to `new_parent` at `index` with Chromium's `BookmarkModel::Move` semantics: `index` is a slot in the
    /// destination's child list *before* the node leaves it (`0..=len`; `None` is `len`). Moving inside one parent to the
    /// slot it already occupies (`old` or `old + 1`) changes nothing; to a later slot the removal shifts the target down by
    /// one. Chromium `components/bookmarks/browser/bookmark_model.cc` `BookmarkModel::Move` @e9a4673.
    pub fn move_to(&mut self, id: Id, new_parent: Id, index: Option<usize>, now_us: i64) -> Result<(), Error> {
        let node = self.get(id).ok_or(Error::NoSuchNode(id))?;
        if id <= OTHER {
            return Err(Error::ProtectedNode(id));
        }
        let old_parent = node.parent.ok_or(Error::NoSuchNode(id))?;
        let mut idx = self.insert_slot(new_parent, index)?;
        // Walk up from the target: meeting `id` means the target is the node or inside it.
        let mut cur = Some(new_parent);
        while let Some(c) = cur {
            if c == id {
                return Err(Error::Cycle);
            }
            cur = self.get(c).and_then(|n| n.parent);
        }
        let old_index = self
            .entries
            .get(&old_parent)
            .and_then(|p| p.children.iter().position(|c| *c == id))
            .ok_or(Error::NoSuchNode(id))?;
        if old_parent == new_parent {
            if idx == old_index || idx == old_index + 1 {
                return Ok(());
            }
            if idx > old_index {
                idx -= 1;
            }
        }
        if let Some(p) = self.entries.get_mut(&old_parent) {
            p.children.remove(old_index);
        }
        if let Some(p) = self.entries.get_mut(&new_parent) {
            p.children.insert(idx, id);
        }
        if let Some(e) = self.entries.get_mut(&id) {
            e.node.parent = Some(new_parent);
            e.node.modified_us = now_us;
        }
        self.touch(old_parent, now_us);
        self.touch(new_parent, now_us);
        Ok(())
    }

    /// Rename a bookmark or folder. Separators have no title.
    pub fn rename(&mut self, id: Id, title: &str, now_us: i64) -> Result<(), Error> {
        let e = self.entries.get_mut(&id).ok_or(Error::NoSuchNode(id))?;
        if id <= OTHER {
            return Err(Error::ProtectedNode(id));
        }
        if matches!(e.node.kind, Kind::Separator) {
            return Err(Error::Format("separators have no title".into()));
        }
        e.node.title = clip_title(title);
        e.node.modified_us = now_us;
        Ok(())
    }

    pub fn set_url(&mut self, id: Id, url: &str, now_us: i64) -> Result<(), Error> {
        let url = normalize_url(url)?;
        let e = self.entries.get_mut(&id).ok_or(Error::NoSuchNode(id))?;
        match &mut e.node.kind {
            Kind::Bookmark { url: u } => *u = url,
            _ => return Err(Error::InvalidUrl("not a bookmark".into())),
        }
        e.node.modified_us = now_us;
        Ok(())
    }

    /// Replaces the tags: lower-cased, trimmed, sorted, deduplicated; empty and over-long (100 chars) tags are dropped.
    pub fn set_tags(&mut self, id: Id, tags: &[&str], now_us: i64) -> Result<(), Error> {
        let e = self.entries.get_mut(&id).ok_or(Error::NoSuchNode(id))?;
        if !matches!(e.node.kind, Kind::Bookmark { .. }) {
            return Err(Error::Format("only bookmarks have tags".into()));
        }
        e.node.tags = normalize_tags(tags.iter().copied());
        e.node.modified_us = now_us;
        Ok(())
    }

    pub fn set_keyword(&mut self, id: Id, keyword: Option<&str>, now_us: i64) -> Result<(), Error> {
        let e = self.entries.get_mut(&id).ok_or(Error::NoSuchNode(id))?;
        if !matches!(e.node.kind, Kind::Bookmark { .. }) {
            return Err(Error::Format("only bookmarks have a keyword".into()));
        }
        e.node.keyword = normalize_keyword(keyword);
        e.node.modified_us = now_us;
        Ok(())
    }

    /// Ids of bookmarks whose URL is the same URL as `url` (compared after `url::Url::parse` normalisation; unparsable text
    /// compares as typed), in tree order.
    pub fn find_by_url(&self, url: &str) -> Vec<Id> {
        let key = normalize_url(url).unwrap_or_else(|_| url.trim().to_string());
        self.descendants(ROOT)
            .into_iter()
            .filter_map(|(_, id)| match self.get(id).map(|n| &n.kind) {
                Some(Kind::Bookmark { url }) if *url == key => Some(id),
                _ => None,
            })
            .collect()
    }

    pub fn is_bookmarked(&self, url: &str) -> bool {
        !self.find_by_url(url).is_empty()
    }

    /// Bookmarks whose title, URL, tags or keyword contain every whitespace-separated word of `query` (case-insensitive
    /// substring), best first, at most `limit`. Each word scores 3 in the title, 2 in tags or keyword, 1 only in the URL
    /// (Chromium `titled_url_index.cc` @e9a4673 also matches words against title and URL, without a rank; the rank is ours);
    /// ties keep tree order.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Id> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut hits: Vec<(u32, Id)> = Vec::new();
        for (_, id) in self.descendants(ROOT) {
            let Some(node) = self.get(id) else { continue };
            let Kind::Bookmark { url } = &node.kind else { continue };
            let title = node.title.to_lowercase();
            let url = url.to_lowercase();
            let tags = node.tags.join(" ");
            let keyword = node.keyword.as_deref().unwrap_or("").to_lowercase();
            let mut score = 0u32;
            let mut all = true;
            for w in &words {
                if title.contains(w.as_str()) {
                    score += 3;
                } else if tags.contains(w.as_str()) || keyword.contains(w.as_str()) {
                    score += 2;
                } else if url.contains(w.as_str()) {
                    score += 1;
                } else {
                    all = false;
                    break;
                }
            }
            if all {
                hits.push((score, id));
            }
        }
        hits.sort_by_key(|(score, _)| Reverse(*score));
        hits.into_iter().take(limit).map(|(_, id)| id).collect()
    }

    /// Remove duplicates inside `folder` (and below when `recursive`): of several bookmarks with the same URL in the same
    /// folder, the first by position stays. Nothing else moves; folders and separators are untouched. Returns how many went.
    pub fn dedupe(&mut self, folder: Id, recursive: bool) -> usize {
        let Some(root) = self.entries.get(&folder) else { return 0 };
        if !matches!(root.node.kind, Kind::Folder) {
            return 0;
        }
        let mut folders = vec![folder];
        if recursive {
            folders.extend(
                self.descendants(folder)
                    .into_iter()
                    .map(|(_, id)| id)
                    .filter(|id| matches!(self.get(*id).map(|n| &n.kind), Some(Kind::Folder))),
            );
        }
        let mut doomed: Vec<Id> = Vec::new();
        for f in folders {
            let Some(e) = self.entries.get(&f) else { continue };
            let mut seen: HashSet<&str> = HashSet::new();
            for child in &e.children {
                if let Some(Kind::Bookmark { url }) = self.get(*child).map(|n| &n.kind)
                    && !seen.insert(url.as_str())
                {
                    doomed.push(*child);
                }
            }
        }
        let mut removed = 0;
        for id in doomed {
            if let Ok(n) = self.remove(id) {
                removed += n;
            }
        }
        removed
    }

    /// Everything in tree order (depth first, children in order), for a bookmarks bar or a manager. The built-in folders
    /// are depth 0; ROOT is not listed.
    pub fn walk(&self) -> Vec<(usize, &Node)> {
        self.descendants(ROOT)
            .into_iter()
            .filter_map(|(d, id)| self.get(id).map(|n| (d, n)))
            .collect()
    }
}
