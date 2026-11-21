/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Pages and visits, and how recording or removing a visit moves the counts, hidden flag and frecency.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::frecency::{FrecencyParams, Links, frecency_linked};
use crate::redact::redact;

pub type PageId = u64;
pub type VisitId = u64;

/// Visits kept per page, newest first; counts are not reduced when older ones drop.
pub const MAX_VISITS_PER_PAGE: usize = 100;

/// Longest URL stored. Firefox docshell/base/BaseHistory.cpp @33abb90 (`browser.history.maxUrlLength`).
const MAX_URL_LEN: usize = 2000;

/// Schemes Firefox never stores. Firefox docshell/base/BaseHistory.cpp `kDisallowedSchemes` @33abb90, plus `place:` (queries, not pages).
const DISALLOWED_SCHEMES: [&str; 18] = [
    "about", "blob", "cached-favicon", "chrome", "data", "imap", "javascript", "mailbox", "news", "page-icon", "resource",
    "view-source", "moz-extension", "moz-page-thumb", "moz-src", "x-moz-ews", "x-moz-graph", "place",
];

/// Firefox visit types (nsINavHistoryService TRANSITION_*).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Transition {
    Link,
    Typed,
    Bookmark,
    Embed,
    RedirectPermanent,
    RedirectTemporary,
    Download,
    FramedLink,
    Reload,
}

impl Transition {
    /// The nsINavHistoryService code, 1..=9.
    pub fn code(self) -> u8 {
        match self {
            Transition::Link => 1,
            Transition::Typed => 2,
            Transition::Bookmark => 3,
            Transition::Embed => 4,
            Transition::RedirectPermanent => 5,
            Transition::RedirectTemporary => 6,
            Transition::Download => 7,
            Transition::FramedLink => 8,
            Transition::Reload => 9,
        }
    }

    /// Inverse of [`Transition::code`]; `None` outside 1..=9.
    pub fn from_code(code: u8) -> Option<Transition> {
        Some(match code {
            1 => Transition::Link,
            2 => Transition::Typed,
            3 => Transition::Bookmark,
            4 => Transition::Embed,
            5 => Transition::RedirectPermanent,
            6 => Transition::RedirectTemporary,
            7 => Transition::Download,
            8 => Transition::FramedLink,
            9 => Transition::Reload,
            _ => return None,
        })
    }

    fn counts(self) -> bool {
        // Firefox toolkit/components/places/nsPlacesTriggers.h VISIT_COUNT_INC @33abb90
        !matches!(self, Transition::Embed | Transition::Download | Transition::FramedLink | Transition::Reload)
    }

    fn is_redirect(self) -> bool {
        matches!(self, Transition::RedirectPermanent | Transition::RedirectTemporary)
    }
}

/// Where a visit came from (`moz_historyvisits.source`). Stored codes follow nsINavHistoryService:
/// organic 0, sponsored 1, bookmarked 2, searched 3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Organic,
    Search,
    Bookmark,
    Sponsored,
}

impl Source {
    fn code(self) -> u8 {
        match self {
            Source::Organic => 0,
            Source::Sponsored => 1,
            Source::Bookmark => 2,
            Source::Search => 3,
        }
    }

    fn from_code(code: u8) -> Option<Source> {
        Some(match code {
            0 => Source::Organic,
            1 => Source::Sponsored,
            2 => Source::Bookmark,
            3 => Source::Search,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Visit {
    pub id: VisitId,
    /// The visit that led here (redirect chains, link clicks).
    pub from: Option<VisitId>,
    pub time_us: i64,
    pub transition: Transition,
    pub source: Source,
    /// The person spent real time on the page; false when unknown.
    pub interesting: bool,
}

#[derive(Clone, Debug)]
pub struct NewVisit<'a> {
    pub url: &'a str,
    pub title: Option<&'a str>,
    pub time_us: i64,
    pub transition: Transition,
    pub source: Source,
    pub interesting: bool,
    pub from: Option<VisitId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub id: PageId,
    pub url: String,
    pub title: Option<String>,
    /// Total countable visits ever recorded (not capped): every type except Embed, Download, FramedLink, Reload.
    pub visit_count: u32,
    /// Visits with transition Typed.
    pub typed_count: u32,
    pub last_visit_us: i64,
    /// Not shown in lists: every visit is an embed, framed link or redirect source.
    pub hidden: bool,
    /// See `frecency.rs`; -1 = never computed.
    pub frecency: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HistoryError {
    Syntax(String),
    Version(u32),
}

/// The in-memory history. Pages are ordered by id so serialisation is deterministic.
#[derive(Clone)]
pub struct HistoryDb {
    pages: BTreeMap<PageId, Page>,
    by_url: HashMap<String, PageId>,
    visits: HashMap<PageId, Vec<Visit>>,
    visit_page: HashMap<VisitId, PageId>,
    /// Stored redirect visits per `from` visit: tells that a visit is a redirect source.
    redirect_children: HashMap<VisitId, u32>,
    next_page: PageId,
    next_visit: VisitId,
    params: FrecencyParams,
}

impl Default for HistoryDb {
    fn default() -> Self {
        HistoryDb::new()
    }
}

/// Firefox docshell/base/BaseHistory.cpp `CanStore` and History.cpp (no userpass in stored URLs) @33abb90.
fn can_store(url: &str) -> bool {
    if url.is_empty() || url.len() > MAX_URL_LEN {
        return false;
    }
    let Some((scheme, rest)) = url.split_once(':') else { return false };
    let scheme = scheme.to_ascii_lowercase();
    if scheme.is_empty() || DISALLOWED_SCHEMES.contains(&scheme.as_str()) {
        return false;
    }
    if (scheme == "http" || scheme == "https")
        && let Some(auth) = rest.strip_prefix("//")
    {
        let end = auth.find(['/', '?', '#']).unwrap_or(auth.len());
        if auth[..end].contains('@') {
            return false;
        }
    }
    true
}

impl HistoryDb {
    pub fn new() -> HistoryDb {
        HistoryDb {
            pages: BTreeMap::new(),
            by_url: HashMap::new(),
            visits: HashMap::new(),
            visit_page: HashMap::new(),
            redirect_children: HashMap::new(),
            next_page: 1,
            next_visit: 1,
            params: FrecencyParams::default(),
        }
    }

    pub(crate) fn pages(&self) -> impl Iterator<Item = &Page> {
        self.pages.values()
    }

    /// Record a visit; creates the page on first sight. `None` for a URL that is not stored. Recomputes the page's frecency.
    /// The address is stored in its [`redact_url`](crate::redact_url) form, so two visits that differ only in a credential
    /// are one page.
    pub fn record_visit(&mut self, v: NewVisit<'_>) -> Option<(PageId, VisitId)> {
        if !can_store(v.url) {
            return None;
        }
        let redacted = redact(v.url);
        let v = NewVisit { url: &redacted, ..v };
        let pid = match self.by_url.get(v.url) {
            Some(&id) => id,
            None => {
                let id = self.next_page;
                self.next_page += 1;
                self.by_url.insert(v.url.to_owned(), id);
                self.pages.insert(
                    id,
                    Page {
                        id,
                        url: v.url.to_owned(),
                        title: None,
                        visit_count: 0,
                        typed_count: 0,
                        last_visit_us: v.time_us,
                        hidden: false,
                        frecency: -1,
                    },
                );
                id
            }
        };
        let vid = self.next_visit;
        self.next_visit += 1;
        let visit = Visit {
            id: vid,
            from: v.from,
            time_us: v.time_us,
            transition: v.transition,
            source: v.source,
            interesting: v.interesting,
        };
        let list = self.visits.entry(pid).or_default();
        // Newest first; among equal times the latest recorded goes first.
        let at = list.partition_point(|x| x.time_us > v.time_us);
        list.insert(at, visit);
        if v.transition.is_redirect()
            && let Some(f) = v.from
        {
            *self.redirect_children.entry(f).or_insert(0) += 1;
        }
        self.visit_page.insert(vid, pid);
        while list.len() > MAX_VISITS_PER_PAGE {
            if let Some(old) = list.pop() {
                self.visit_page.remove(&old.id);
                if old.transition.is_redirect()
                    && let Some(f) = old.from
                {
                    dec_child(&mut self.redirect_children, f);
                }
            }
        }
        if let Some(page) = self.pages.get_mut(&pid) {
            if v.transition.counts() {
                page.visit_count = page.visit_count.saturating_add(1);
            }
            if v.transition == Transition::Typed {
                page.typed_count = page.typed_count.saturating_add(1);
            }
            page.last_visit_us = page.last_visit_us.max(v.time_us);
            if let Some(t) = v.title
                && !t.is_empty()
            {
                page.title = Some(t.to_owned());
            }
        }
        self.refresh(pid, None, false);
        // The source of a redirect has just become a redirect source: it may turn hidden and its visit weighs less.
        if v.transition.is_redirect()
            && let Some(&src) = v.from.and_then(|f| self.visit_page.get(&f))
            && src != pid
        {
            self.refresh(src, None, false);
        }
        Some((pid, vid))
    }

    pub fn set_title(&mut self, url: &str, title: &str) {
        if title.is_empty() {
            return;
        }
        if let Some(p) = self.by_url.get(&*redact(url)).and_then(|id| self.pages.get_mut(id)) {
            p.title = Some(title.to_owned());
        }
    }

    pub fn page(&self, url: &str) -> Option<&Page> {
        self.by_url.get(&*redact(url)).and_then(|id| self.pages.get(id))
    }

    pub fn page_by_id(&self, id: PageId) -> Option<&Page> {
        self.pages.get(&id)
    }

    /// Newest first, at most [`MAX_VISITS_PER_PAGE`] kept.
    pub fn visits(&self, id: PageId) -> &[Visit] {
        self.visits.get(&id).map_or(&[], Vec::as_slice)
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Recompute one page's frecency, using the bookmark's date when the page has no countable visits.
    pub fn recompute_frecency(&mut self, url: &str, bookmarked_at_us: Option<i64>, is_redirect: bool) {
        if let Some(&id) = self.by_url.get(&*redact(url)) {
            self.refresh(id, bookmarked_at_us, is_redirect);
        }
    }

    pub fn remove_url(&mut self, url: &str) -> bool {
        let Some(id) = self.by_url.remove(&*redact(url)) else { return false };
        self.pages.remove(&id);
        for v in self.visits.remove(&id).unwrap_or_default() {
            self.forget_visit(&v);
        }
        true
    }

    /// Remove visits in `[from_us, to_us)`; pages left with no visits go. Returns the number of pages removed.
    pub fn remove_range(&mut self, from_us: i64, to_us: i64) -> usize {
        let ids: Vec<PageId> = self.pages.keys().copied().collect();
        let mut removed_pages = 0;
        for pid in ids {
            let list = self.visits.entry(pid).or_default();
            let (gone, kept): (Vec<Visit>, Vec<Visit>) =
                std::mem::take(list).into_iter().partition(|v| v.time_us >= from_us && v.time_us < to_us);
            *list = kept;
            if gone.is_empty() {
                continue;
            }
            let removed_countable = gone.iter().filter(|v| v.transition.counts()).count() as u32;
            for v in &gone {
                self.forget_visit(v);
            }
            let list = self.visits.get(&pid).map_or(&[][..], Vec::as_slice);
            if list.is_empty() {
                if let Some(p) = self.pages.remove(&pid) {
                    self.by_url.remove(&p.url);
                }
                self.visits.remove(&pid);
                removed_pages += 1;
                continue;
            }
            let surviving = list.iter().filter(|v| v.transition.counts()).count() as u32;
            let newest = list.iter().map(|v| v.time_us).max().unwrap_or(0);
            if let Some(p) = self.pages.get_mut(&pid) {
                p.visit_count = p.visit_count.saturating_sub(removed_countable).max(surviving);
                p.last_visit_us = newest;
            }
            self.refresh(pid, None, false);
        }
        removed_pages
    }

    pub fn clear(&mut self) {
        let params = self.params.clone();
        *self = HistoryDb::new();
        self.params = params;
    }

    fn forget_visit(&mut self, v: &Visit) {
        self.visit_page.remove(&v.id);
        self.redirect_children.remove(&v.id);
        if v.transition.is_redirect()
            && let Some(f) = v.from
        {
            dec_child(&mut self.redirect_children, f);
        }
    }

    /// Recompute hidden and frecency of one page from its stored visits.
    fn refresh(&mut self, pid: PageId, bookmarked_at_us: Option<i64>, is_redirect: bool) {
        let list = self.visits.get(&pid).map_or(&[][..], Vec::as_slice);
        // Firefox toolkit/components/places/Helpers.cpp GetHiddenState @33abb90: framed link, embed or redirect source.
        let hidden = !list.is_empty()
            && list.iter().all(|v| {
                matches!(v.transition, Transition::Embed | Transition::FramedLink)
                    || self.redirect_children.get(&v.id).is_some_and(|&n| n > 0)
            });
        let Some(url) = self.pages.get(&pid).map(|p| p.url.as_str()) else { return };
        let count = self.pages.get(&pid).map_or(0, |p| p.visit_count);
        let score = if url.starts_with("place:") {
            0
        } else {
            let type_of = |id: VisitId| {
                let page = self.visit_page.get(&id)?;
                self.visits.get(page)?.iter().find(|v| v.id == id).map(|v| v.transition)
            };
            let has_child = |id: VisitId| self.redirect_children.get(&id).is_some_and(|&n| n > 0);
            frecency_linked(list, count, bookmarked_at_us, is_redirect, &self.params, &Links { type_of: &type_of, has_redirect_child: &has_child })
        };
        if let Some(p) = self.pages.get_mut(&pid) {
            p.hidden = hidden;
            p.frecency = score;
        }
    }

    /// Deterministic JSON: `{"version":1,"next_page":N,"next_visit":M,"pages":[...]}` sorted by page id, visits nested.
    pub fn to_json(&self) -> String {
        let pages = self
            .pages
            .values()
            .map(|p| PageJson {
                id: p.id,
                url: p.url.clone(),
                title: p.title.clone(),
                visit_count: p.visit_count,
                typed_count: p.typed_count,
                last_visit_us: p.last_visit_us,
                hidden: p.hidden,
                frecency: p.frecency,
                visits: self
                    .visits(p.id)
                    .iter()
                    .map(|v| VisitJson {
                        id: v.id,
                        from: v.from,
                        time_us: v.time_us,
                        transition: v.transition.code(),
                        source: v.source.code(),
                        interesting: v.interesting,
                    })
                    .collect(),
            })
            .collect();
        let file = FileJson { version: 1, next_page: self.next_page, next_visit: self.next_visit, pages };
        serde_json::to_string(&file).unwrap_or_default()
    }

    /// Parse [`HistoryDb::to_json`] output, rebuilding the indexes; the stored frecency is kept.
    pub fn from_json(text: &str) -> Result<HistoryDb, HistoryError> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| HistoryError::Syntax(e.to_string()))?;
        let version = value.get("version").and_then(serde_json::Value::as_u64);
        match version {
            Some(1) => {}
            Some(n) => return Err(HistoryError::Version(u32::try_from(n).unwrap_or(u32::MAX))),
            None => return Err(HistoryError::Syntax("missing version".into())),
        }
        let file: FileJson = serde_json::from_value(value).map_err(|e| HistoryError::Syntax(e.to_string()))?;
        let mut db = HistoryDb::new();
        db.next_page = file.next_page;
        db.next_visit = file.next_visit;
        for mut pj in file.pages {
            // A file written before addresses were redacted: clean it as it loads. A page whose cleaned address is one
            // already loaded is dropped (the earlier page keeps its own visits).
            let clean = redact(&pj.url).into_owned();
            if clean != pj.url {
                pj.url = clean;
            }
            if db.by_url.contains_key(&pj.url) && !db.pages.contains_key(&pj.id) {
                continue;
            }
            if db.pages.contains_key(&pj.id) || db.by_url.contains_key(&pj.url) {
                return Err(HistoryError::Syntax(format!("duplicate page {}", pj.id)));
            }
            let mut list = Vec::with_capacity(pj.visits.len());
            for vj in pj.visits {
                let transition = Transition::from_code(vj.transition)
                    .ok_or_else(|| HistoryError::Syntax(format!("bad transition {}", vj.transition)))?;
                let source =
                    Source::from_code(vj.source).ok_or_else(|| HistoryError::Syntax(format!("bad source {}", vj.source)))?;
                if db.visit_page.insert(vj.id, pj.id).is_some() {
                    return Err(HistoryError::Syntax(format!("duplicate visit {}", vj.id)));
                }
                if transition.is_redirect()
                    && let Some(f) = vj.from
                {
                    *db.redirect_children.entry(f).or_insert(0) += 1;
                }
                db.next_visit = db.next_visit.max(vj.id.saturating_add(1));
                list.push(Visit { id: vj.id, from: vj.from, time_us: vj.time_us, transition, source, interesting: vj.interesting });
            }
            db.next_page = db.next_page.max(pj.id.saturating_add(1));
            db.by_url.insert(pj.url.clone(), pj.id);
            db.visits.insert(pj.id, list);
            db.pages.insert(
                pj.id,
                Page {
                    id: pj.id,
                    url: pj.url,
                    title: pj.title,
                    visit_count: pj.visit_count,
                    typed_count: pj.typed_count,
                    last_visit_us: pj.last_visit_us,
                    hidden: pj.hidden,
                    frecency: pj.frecency,
                },
            );
        }
        Ok(db)
    }
}

fn dec_child(map: &mut HashMap<VisitId, u32>, from: VisitId) {
    if let Some(n) = map.get_mut(&from) {
        *n = n.saturating_sub(1);
        if *n == 0 {
            map.remove(&from);
        }
    }
}

#[derive(Serialize, Deserialize)]
struct FileJson {
    version: u32,
    next_page: u64,
    next_visit: u64,
    pages: Vec<PageJson>,
}

#[derive(Serialize, Deserialize)]
struct PageJson {
    id: u64,
    url: String,
    title: Option<String>,
    visit_count: u32,
    typed_count: u32,
    last_visit_us: i64,
    hidden: bool,
    frecency: i64,
    visits: Vec<VisitJson>,
}

#[derive(Serialize, Deserialize)]
struct VisitJson {
    id: u64,
    from: Option<u64>,
    time_us: i64,
    transition: u8,
    source: u8,
    interesting: bool,
}
