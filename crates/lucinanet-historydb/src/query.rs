/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Token matching as the Firefox address bar does it over places: every whitespace separated token must match the URL or the title.

use crate::db::{HistoryDb, Page};

/// mozIPlacesAutoComplete MATCH_ANYWHERE / MATCH_BOUNDARY / MATCH_BEGINNING.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchBehavior {
    Anywhere,
    Boundary,
    Begin,
}

/// A history search. Empty `text` or `limit == 0` give no results.
pub struct Query<'a> {
    pub text: &'a str,
    pub behavior: MatchBehavior,
    pub limit: usize,
    pub include_hidden: bool,
    /// Only pages whose last visit is at or after this.
    pub since_us: Option<i64>,
}

fn strip_scheme_and_www(url: &str) -> &str {
    let rest = match url.find("://") {
        Some(i) if url[..i].bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')) => &url[i + 3..],
        _ => url,
    };
    rest.strip_prefix("www.").unwrap_or(rest)
}

fn boundary_match(hay: &str, token: &str) -> bool {
    hay.match_indices(token).any(|(i, _)| i == 0 || hay[..i].chars().next_back().is_some_and(|c| !c.is_alphanumeric()))
}

fn field_matches(hay: &str, token: &str, b: MatchBehavior, is_url: bool) -> bool {
    match b {
        MatchBehavior::Anywhere => hay.contains(token),
        MatchBehavior::Boundary => boundary_match(hay, token),
        MatchBehavior::Begin => {
            if is_url { strip_scheme_and_www(hay).starts_with(token) } else { hay.starts_with(token) }
        }
    }
}

fn page_matches(p: &Page, tokens: &[String], b: MatchBehavior) -> bool {
    let url = p.url.to_lowercase();
    let title = p.title.as_deref().map(str::to_lowercase).unwrap_or_default();
    tokens.iter().all(|t| field_matches(&url, t, b, true) || field_matches(&title, t, b, false))
}

impl HistoryDb {
    /// Best first: frecency descending, then last visit descending, then URL ascending.
    pub fn search(&self, q: &Query<'_>) -> Vec<&Page> {
        let tokens: Vec<String> = q.text.split_whitespace().map(str::to_lowercase).collect();
        if tokens.is_empty() || q.limit == 0 {
            return Vec::new();
        }
        let mut hits: Vec<&Page> = self
            .pages()
            .filter(|p| (q.include_hidden || !p.hidden) && q.since_us.is_none_or(|s| p.last_visit_us >= s))
            .filter(|p| page_matches(p, &tokens, q.behavior))
            .collect();
        hits.sort_by(|a, b| {
            b.frecency.cmp(&a.frecency).then(b.last_visit_us.cmp(&a.last_visit_us)).then_with(|| a.url.cmp(&b.url))
        });
        hits.truncate(q.limit);
        hits
    }

    /// Pages by last visit, newest first (ties: URL ascending).
    pub fn recent(&self, limit: usize, include_hidden: bool) -> Vec<&Page> {
        let mut all: Vec<&Page> = self.pages().filter(|p| include_hidden || !p.hidden).collect();
        all.sort_by(|a, b| b.last_visit_us.cmp(&a.last_visit_us).then_with(|| a.url.cmp(&b.url)));
        all.truncate(limit);
        all
    }
}
