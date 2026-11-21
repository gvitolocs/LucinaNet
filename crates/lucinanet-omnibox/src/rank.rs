/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The suggestion list under the address bar: the heuristic first item, then history, bookmarks and remote search
//! suggestions in descending score, and the inline completion of the typed text.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use url::Url;

use crate::classify::{Action, Context, classify};
use crate::history_score::{find_all, fold, fold_str, frequency, score_with, terms_of, topicality, word_starts};

/// A visited page, as the history store reports it (the wiring converts its records into this).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    /// The page address.
    pub url: String,
    /// The page title, possibly empty.
    pub title: String,
    /// Number of visits.
    pub visit_count: u32,
    /// How many of the visits came from typing the address.
    pub typed_count: u32,
    /// Time of the last visit, microseconds since the Unix epoch.
    pub last_visit_us: i64,
}

/// A bookmark, as the bookmark store reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookmarkEntry {
    /// The bookmarked address.
    pub url: String,
    /// The bookmark title.
    pub title: String,
    /// "Toolbar/News", for display; not used for ranking.
    pub folder_path: String,
}

/// Where a suggestion comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// What Enter does with the typed text; always the first item.
    Heuristic,
    /// A visited page.
    History,
    /// A bookmark that is not in the history.
    Bookmark,
    /// A search suggestion from the default engine.
    RemoteSearch,
    /// The plain default-engine search of the whole typed text, shown next to a keyword search (Chromium keeps both).
    KeywordSearch,
}

/// One row of the dropdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// Where it comes from.
    pub kind: Kind,
    /// What goes into the bar when chosen: a URL for pages, the terms for searches.
    pub text: String,
    /// What Enter opens.
    pub url: Url,
    /// Row title.
    pub title: String,
    /// Higher first; the heuristic item is always first regardless of score.
    pub score: i32,
    /// The tail to append to the typed text and select; only ever on the first item.
    pub inline_completion: Option<String>,
    /// A history item that is also a bookmark gets the star.
    pub bookmarked: bool,
}

/// Everything `suggest` reads.
pub struct RankInput<'a> {
    /// The text in the bar.
    pub typed: &'a str,
    /// Inline completion only when the caret is at the end ...
    pub cursor_at_end: bool,
    /// ... and the last edit was not a Backspace/Delete.
    pub deleting: bool,
    /// Current time, microseconds since the Unix epoch.
    pub now_us: i64,
    /// Visited pages (any number; non-matching ones are ignored).
    pub history: &'a [HistoryEntry],
    /// Bookmarks.
    pub bookmarks: &'a [BookmarkEntry],
    /// Search suggestions from the default engine, in the engine's order.
    pub remote: &'a [String],
    /// Engines and options, as for `classify`.
    pub cx: &'a Context<'a>,
}

/// Rows in the dropdown, the heuristic item included. Chromium's desktop omnibox shows 8
/// (autocomplete_result.cc GetMaxMatches @e9a4673); Firefox's `browser.urlbar.maxRichResults` is 10
/// (browser/components/urlbar/UrlbarPrefs.sys.mjs @33abb90). We keep 8 because the scores here are Chromium's.
pub const MAX_SUGGESTIONS: usize = 8;

// Chromium components/omnibox/browser/search_provider.cc and search_suggestion_parser.cc @e9a4673.
/// SuggestResult::CalculateRelevance for a query: 600, plus how many results follow it.
const REMOTE_BASE_QUERY: i32 = 600;
/// The same when the typed text looks like a URL.
const REMOTE_BASE_URL: i32 = 300;
/// SearchProvider::CalculateRelevanceForVerbatim when a keyword provider is active.
const VERBATIM_WITH_KEYWORD: i32 = 250;
// Chromium bookmark_provider.cc CalculateBookmarkMatchRelevance @e9a4673.
const BOOKMARK_BASE: i32 = 900;
const BOOKMARK_MAX: i32 = 1199;
const BOOKMARKLET_BASE: i32 = 400;
const BOOKMARKLET_MAX: i32 = 799;
const BOOKMARK_URL_COUNT_BOOST: [i32; 4] = [0, 75, 125, 150];
/// omnibox_field_trial.cc HQPBookmarkValue default: a bookmarked page counts as 10 visits for inline completion rank.
const BOOKMARK_VISIT_VALUE: f64 = 10.0;
/// Chromium url_prefix.cc @e9a4673 GetURLPrefixes(): longest first; the empty prefix covers "the person typed the scheme".
const INLINE_PREFIXES: [&str; 7] = ["http://www.", "https://www.", "ftp://www.", "http://", "https://", "ftp://", ""];

struct Candidate {
    suggestion: Suggestion,
    recency: i64,
    /// The text starts like the typed text (URL after scheme and `www.`, or title), which remote suggestions must not outrank.
    prefix_match: bool,
}

/// Ranks the dropdown for `input`: empty when `classify` says Empty, otherwise the heuristic item followed by at most
/// `MAX_SUGGESTIONS - 1` more, in descending score, ties by recency then URL.
///
/// Remote suggestions are listed when the heuristic item is a search, or when the typed text has a space and is not a
/// keyword search (the suggestions come from the default engine, not from the keyword's). For text that reads as an
/// address only history and bookmarks are shown; Chromium also lists search suggestions there, at 300 instead of 600,
/// which this does for address-like text that contains a space.
pub fn suggest(input: &RankInput<'_>) -> Vec<Suggestion> {
    let action = classify(input.typed, input.cx);
    let Some(mut first) = heuristic(&action) else {
        return Vec::new();
    };
    let heuristic_key = dedupe_key(first.url.as_str());
    let terms = terms_of(input.typed);
    let typed_lower = input.typed.trim().to_lowercase();
    let typed_stripped = strip_scheme_and_www(&typed_lower);

    let mut candidates: BTreeMap<String, Candidate> = BTreeMap::new();
    let tops: Vec<f64> = input.history.iter().map(|entry| topicality(entry, &terms)).collect();
    let matching_pages = tops.iter().filter(|top| **top > 0.0).count();
    for (entry, top) in input.history.iter().zip(&tops) {
        if *top <= 0.0 {
            continue;
        }
        let Ok(url) = Url::parse(&entry.url) else { continue };
        let key = dedupe_key(url.as_str());
        let score = score_with(entry, *top, input.now_us, matching_pages);
        let prefix_match = starts_like_typed(&key, &entry.title, typed_stripped, &typed_lower);
        add(
            &mut candidates,
            key,
            Candidate {
                suggestion: Suggestion {
                    kind: Kind::History,
                    text: entry.url.clone(),
                    url,
                    title: entry.title.clone(),
                    score,
                    inline_completion: None,
                    bookmarked: false,
                },
                recency: entry.last_visit_us,
                prefix_match,
            },
        );
    }
    for bookmark in input.bookmarks {
        let same_url = input.bookmarks.iter().filter(|other| other.url == bookmark.url).count();
        let score = bookmark_relevance(bookmark, &terms, same_url);
        if score == 0 {
            continue;
        }
        let Ok(url) = Url::parse(&bookmark.url) else { continue };
        let key = dedupe_key(url.as_str());
        let prefix_match = starts_like_typed(&key, &bookmark.title, typed_stripped, &typed_lower);
        add(
            &mut candidates,
            key,
            Candidate {
                suggestion: Suggestion {
                    kind: Kind::Bookmark,
                    text: bookmark.url.clone(),
                    url,
                    title: bookmark.title.clone(),
                    score,
                    inline_completion: None,
                    bookmarked: true,
                },
                recency: i64::MIN,
                prefix_match,
            },
        );
    }
    // The heuristic item is the page Enter opens: listing it again would be a duplicate row.
    candidates.remove(&heuristic_key);

    let prefix_floor = candidates.values().filter(|c| c.prefix_match).map(|c| c.suggestion.score).min();
    let mut rows: Vec<Candidate> = candidates.into_values().collect();

    let remote_allowed = match &action {
        Action::Search { .. } => true,
        Action::Navigate { .. } => input.typed.trim().contains(char::is_whitespace),
        Action::Keyword { .. } | Action::Empty => false,
    };
    if remote_allowed {
        rows.extend(remote_rows(input, matches!(action, Action::Search { .. }), &typed_lower, prefix_floor));
    }
    if matches!(action, Action::Keyword { .. }) {
        let terms = input.typed.trim();
        rows.push(Candidate {
            suggestion: Suggestion {
                kind: Kind::KeywordSearch,
                text: terms.to_owned(),
                url: input.cx.default_engine.url_for(terms),
                title: format!("{terms} \u{2014} {}", input.cx.default_engine.name),
                score: VERBATIM_WITH_KEYWORD,
                inline_completion: None,
                bookmarked: false,
            },
            recency: i64::MIN,
            prefix_match: false,
        });
    }

    rows.sort_by(order);
    first.inline_completion = inline_completion(input);
    let mut out = Vec::with_capacity(MAX_SUGGESTIONS);
    out.push(first);
    out.extend(rows.into_iter().take(MAX_SUGGESTIONS - 1).map(|row| row.suggestion));
    out
}

/// Score descending, then recency descending, then URL ascending.
fn order(a: &Candidate, b: &Candidate) -> Ordering {
    b.suggestion
        .score
        .cmp(&a.suggestion.score)
        .then(b.recency.cmp(&a.recency))
        .then_with(|| a.suggestion.url.as_str().cmp(b.suggestion.url.as_str()))
}

fn heuristic(action: &Action) -> Option<Suggestion> {
    let (text, title, url) = match action {
        Action::Empty => return None,
        Action::Navigate { url, .. } => (url.to_string(), url.to_string(), url.clone()),
        Action::Search { terms, url, .. } => (terms.clone(), format!("{terms} \u{2014} search"), url.clone()),
        Action::Keyword { engine, terms, url } => (terms.clone(), format!("{terms} \u{2014} {}", engine.name), url.clone()),
    };
    Some(Suggestion { kind: Kind::Heuristic, text, url, title, score: i32::MAX, inline_completion: None, bookmarked: false })
}

/// Adds a candidate, merging with one for the same destination: the better score wins; a history page that is also a
/// bookmark stays a `History` item with the star and the larger of the two scores.
fn add(map: &mut BTreeMap<String, Candidate>, key: String, new: Candidate) {
    match map.remove(&key) {
        None => {
            map.insert(key, new);
        }
        Some(old) => {
            map.insert(key, merge(old, new));
        }
    }
}

fn merge(old: Candidate, new: Candidate) -> Candidate {
    let bookmarked = old.suggestion.bookmarked || new.suggestion.bookmarked;
    let score = old.suggestion.score.max(new.suggestion.score);
    let any_history = old.suggestion.kind == Kind::History || new.suggestion.kind == Kind::History;
    let (mut primary, other) = match (old.suggestion.kind == Kind::History, new.suggestion.kind == Kind::History) {
        (true, false) => (old, new),
        (false, true) => (new, old),
        _ if order(&old, &new) != Ordering::Greater => (old, new),
        _ => (new, old),
    };
    primary.suggestion.score = score;
    primary.suggestion.bookmarked = bookmarked;
    primary.suggestion.kind = if any_history { Kind::History } else { Kind::Bookmark };
    if primary.suggestion.title.is_empty() {
        primary.suggestion.title = other.suggestion.title;
    }
    primary.prefix_match |= other.prefix_match;
    primary
}

/// Remote suggestions with Chromium's relevance (600 + number of later results), dropping the typed text itself and
/// repeats, and never above the weakest history or bookmark item that matches the typed text by prefix.
fn remote_rows(input: &RankInput<'_>, query_like: bool, typed_lower: &str, prefix_floor: Option<i32>) -> Vec<Candidate> {
    let base = if query_like { REMOTE_BASE_QUERY } else { REMOTE_BASE_URL };
    let count = i32::try_from(input.remote.len()).unwrap_or(i32::MAX);
    let mut seen = BTreeSet::new();
    let mut previous = i32::MAX;
    let mut rows = Vec::new();
    for (i, suggestion) in input.remote.iter().enumerate() {
        let text = suggestion.trim();
        let lower = text.to_lowercase();
        if text.is_empty() || lower == typed_lower || !seen.insert(lower) {
            continue;
        }
        let later = count - 1 - i32::try_from(i).unwrap_or(i32::MAX);
        let mut score = base.saturating_add(later);
        if let Some(floor) = prefix_floor {
            score = score.min(floor - 1);
        }
        score = score.min(previous - 1).max(0);
        previous = score;
        rows.push(Candidate {
            suggestion: Suggestion {
                kind: Kind::RemoteSearch,
                text: text.to_owned(),
                url: input.cx.default_engine.url_for(text),
                title: text.to_owned(),
                score,
                inline_completion: None,
                bookmarked: false,
            },
            recency: i64::MIN,
            prefix_match: false,
        });
    }
    rows
}

fn strip_scheme_and_www(lower: &str) -> &str {
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")).unwrap_or(lower);
    rest.strip_prefix("www.").unwrap_or(rest)
}

fn starts_like_typed(key: &str, title: &str, typed_stripped: &str, typed_lower: &str) -> bool {
    !typed_stripped.is_empty() && (key.to_lowercase().starts_with(typed_stripped) || title.to_lowercase().starts_with(typed_lower))
}

/// The destination with the scheme (http and https count as one), `www.` and a trailing slash left out, for deduplication
/// (Chromium autocomplete_match.cc GetStrippedURL idea, @e9a4673).
pub(crate) fn dedupe_key(raw: &str) -> String {
    let Ok(url) = Url::parse(raw) else {
        return raw.trim().trim_end_matches('/').to_lowercase();
    };
    let mut key = String::new();
    match url.host_str() {
        Some(host) if matches!(url.scheme(), "http" | "https") => {
            key.push_str(host.strip_prefix("www.").unwrap_or(host));
            if let Some(port) = url.port() {
                key.push(':');
                key.push_str(&port.to_string());
            }
            key.push_str(url.path());
            if let Some(query) = url.query() {
                key.push('?');
                key.push_str(query);
            }
            if let Some(fragment) = url.fragment() {
                key.push('#');
                key.push_str(fragment);
            }
        }
        _ => key.push_str(url.as_str()),
    }
    if key.ends_with('/') {
        key.pop();
    }
    key
}

/// Chromium bookmark_provider.cc CalculateBookmarkMatchRelevance: every typed word must start a word in the title or
/// the URL; each match adds (matched characters) x (how early it starts) via ScoringFunctor; the sum divided by the
/// title length plus 10 (capped at 1) picks a point between 900 and 1199, and a URL that several bookmarks share is
/// boosted. 0 when a word matches nowhere.
fn bookmark_relevance(bookmark: &BookmarkEntry, terms: &[Vec<char>], same_url_count: usize) -> i32 {
    if terms.is_empty() {
        return 0;
    }
    let title = fold_str(&bookmark.title);
    let url_len = bookmark.url.chars().count();
    let url_text = fold_str(strip_scheme_and_www(&bookmark.url.to_lowercase()));
    let title_words = word_starts(&title);
    let url_words = word_starts(&url_text);
    let mut title_hits = Vec::new();
    let mut url_hits = Vec::new();
    for term in terms {
        let in_title = first_at_word_start(term, &title, &title_words);
        let in_url = first_at_word_start(term, &url_text, &url_words);
        if in_title.is_none() && in_url.is_none() {
            return 0;
        }
        title_hits.extend(in_title);
        url_hits.extend(in_url);
    }
    title_hits.sort_unstable();
    title_hits.dedup();
    url_hits.sort_unstable();
    url_hits.dedup();
    // scoring_functor.cc: matched length x (length - start) / length.
    let factor = |hits: &[(usize, usize)], length: usize| -> f64 {
        if length == 0 {
            return 0.0;
        }
        hits.iter().map(|(start, end)| (end - start) as f64 * length.saturating_sub(*start) as f64 / length as f64).sum()
    };
    let title_strength = factor(&title_hits, title.len());
    let summed = title_strength + factor(&url_hits, url_len);
    let length = if title.is_empty() { url_len } else { title.len() };
    let normalized = (summed / (length as f64 + 10.0)).min(1.0);
    let bookmarklet = bookmark.url.to_ascii_lowercase().starts_with("javascript:") && title_strength == 0.0;
    let (base, max) = if bookmarklet { (BOOKMARKLET_BASE, BOOKMARKLET_MAX) } else { (BOOKMARK_BASE, BOOKMARK_MAX) };
    let relevance = (normalized * f64::from(max - base)) as i32 + base;
    if relevance >= max {
        return relevance;
    }
    let boost = BOOKMARK_URL_COUNT_BOOST[same_url_count.clamp(1, BOOKMARK_URL_COUNT_BOOST.len()) - 1];
    (relevance + boost).min(max)
}

fn first_at_word_start(term: &[char], text: &[char], starts: &[usize]) -> Option<(usize, usize)> {
    find_all(term, text).into_iter().find(|at| starts.binary_search(at).is_ok()).map(|at| (at, at + term.len()))
}

/// The tail to append to the typed text. Mirrors Firefox UrlbarProviderAutofill.sys.mjs @33abb90 (origins first; a URL
/// only when the typed text already holds a `/`; never with a space) and Chromium url_prefix.cc @e9a4673 (the typed
/// text may start right after `http://`, `https://www.` and so on, so a person who typed no scheme gets none).
fn inline_completion(input: &RankInput<'_>) -> Option<String> {
    let typed = input.typed;
    if !input.cursor_at_end || input.deleting || typed.is_empty() || typed.chars().any(char::is_whitespace) {
        return None;
    }
    if strip_scheme_and_www(&typed.to_lowercase()).is_empty() {
        return None;
    }
    let typed_folded = fold_str(typed);
    let mut pages: Vec<(&str, f64)> = Vec::new();
    for entry in input.history {
        pages.push((entry.url.as_str(), frequency(entry, input.now_us)));
    }
    for bookmark in input.bookmarks {
        pages.push((bookmark.url.as_str(), BOOKMARK_VISIT_VALUE));
    }

    // Candidates with a rank: the rank of an origin is the sum over its pages.
    let mut ranked: BTreeMap<String, f64> = BTreeMap::new();
    let origin_mode = !typed.contains('/');
    for (raw, rank) in pages {
        let candidate = if origin_mode {
            match origin_of(raw) {
                Some(origin) => origin,
                None => continue,
            }
        } else {
            raw.to_owned()
        };
        let slot = ranked.entry(candidate).or_insert(0.0);
        if origin_mode {
            *slot += rank;
        } else if rank > *slot {
            *slot = rank;
        }
    }
    let mut best: Option<(f64, &str, String)> = None;
    for (candidate, rank) in &ranked {
        let Some(tail) = completion_after(candidate, &typed_folded) else { continue };
        let better = match &best {
            None => true,
            Some((best_rank, best_candidate, _)) => outranks(*rank, candidate, *best_rank, best_candidate),
        };
        if better {
            best = Some((*rank, candidate.as_str(), tail));
        }
    }
    best.map(|(_, _, tail)| tail)
}

/// Higher rank wins; then the shorter text; then the alphabetically smaller one, so the choice is deterministic.
fn outranks(rank: f64, candidate: &str, best_rank: f64, best_candidate: &str) -> bool {
    rank.total_cmp(&best_rank).then(best_candidate.len().cmp(&candidate.len())).then_with(|| best_candidate.cmp(candidate)).is_gt()
}

/// `https://www.example.com:8080/` for an http, https or ftp page.
fn origin_of(raw: &str) -> Option<String> {
    let url = Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https" | "ftp") || url.host_str().is_none() {
        return None;
    }
    Some(format!("{}/", url.origin().ascii_serialization()))
}

/// The text of `candidate` after `typed`, when `typed` starts the candidate directly or right after one of the URL
/// prefixes; `None` when there is nothing left to complete.
fn completion_after(candidate: &str, typed_folded: &[char]) -> Option<String> {
    let original: Vec<char> = candidate.chars().collect();
    let folded: Vec<char> = original.iter().map(|c| fold(*c)).collect();
    for prefix in INLINE_PREFIXES {
        let prefix: Vec<char> = prefix.chars().collect();
        let end = prefix.len() + typed_folded.len();
        if folded.len() >= end && folded[..prefix.len()] == prefix[..] && folded[prefix.len()..end] == *typed_folded {
            let tail: String = original[end..].iter().collect();
            return (!tail.is_empty()).then_some(tail);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::SearchEngine;

    const NOW: i64 = 1_800_000_000_000_000;
    const DAY: i64 = 86_400_000_000;

    fn page(url: &str, title: &str, visits: u32, typed: u32, days_ago: i64) -> HistoryEntry {
        HistoryEntry { url: url.to_owned(), title: title.to_owned(), visit_count: visits, typed_count: typed, last_visit_us: NOW - days_ago * DAY }
    }

    fn bookmark(url: &str, title: &str) -> BookmarkEntry {
        BookmarkEntry { url: url.to_owned(), title: title.to_owned(), folder_path: "Toolbar".to_owned() }
    }

    struct World {
        default_engine: SearchEngine,
        keywords: Vec<SearchEngine>,
    }

    impl World {
        fn new() -> World {
            let wiki = SearchEngine {
                name: "Wikipedia".to_owned(),
                keyword: Some("w".to_owned()),
                template: "https://en.wikipedia.org/w/index.php?search={searchTerms}".to_owned(),
            };
            World { default_engine: SearchEngine::duckduckgo_html(), keywords: vec![wiki] }
        }

        fn run(&self, typed: &str, history: &[HistoryEntry], bookmarks: &[BookmarkEntry], remote: &[&str]) -> Vec<Suggestion> {
            self.run_with(typed, true, false, history, bookmarks, remote)
        }

        fn run_with(
            &self,
            typed: &str,
            cursor_at_end: bool,
            deleting: bool,
            history: &[HistoryEntry],
            bookmarks: &[BookmarkEntry],
            remote: &[&str],
        ) -> Vec<Suggestion> {
            let cx = Context { default_engine: &self.default_engine, keywords: &self.keywords, allow_single_label_hosts: false };
            let remote: Vec<String> = remote.iter().map(|s| (*s).to_owned()).collect();
            suggest(&RankInput { typed, cursor_at_end, deleting, now_us: NOW, history, bookmarks, remote: &remote, cx: &cx })
        }
    }

    fn urls(list: &[Suggestion]) -> Vec<&str> {
        list.iter().map(|s| s.url.as_str()).collect()
    }

    #[test]
    fn suggest_empty_input_gives_nothing() {
        let world = World::new();
        let history = [page("https://example.com/", "Example", 3, 0, 0)];
        assert!(world.run("", &history, &[], &["x"]).is_empty());
        assert!(world.run("   ", &history, &[], &["x"]).is_empty());
        assert!(world.run("?", &history, &[], &[]).is_empty());
    }

    #[test]
    fn suggest_first_is_heuristic() {
        let world = World::new();
        let history = [page("https://example.com/", "Example", 3, 0, 0)];
        let out = world.run("exam", &history, &[], &[]);
        assert_eq!(out[0].kind, Kind::Heuristic);
        assert_eq!(out[0].score, i32::MAX);
        assert_eq!(out[0].url.as_str(), "https://duckduckgo.com/html/?q=exam");
        assert_eq!(out[0].text, "exam");
        assert_eq!(out[0].title, "exam \u{2014} search");
        assert_eq!(out[1].kind, Kind::History);
        assert!(out[1..].iter().all(|s| s.score < i32::MAX && s.inline_completion.is_none()));

        let nav = world.run("example.com/x", &history, &[], &[]);
        assert_eq!(nav[0].kind, Kind::Heuristic);
        assert_eq!(nav[0].url.as_str(), "https://example.com/x");
        assert_eq!(nav[0].text, "https://example.com/x");
    }

    #[test]
    fn suggest_orders_by_score_then_recency_then_url() {
        let world = World::new();
        // Equal scores (one visit within four days, same shape), so recency decides, then the URL.
        let history = [
            page("https://site.org/a", "", 1, 0, 2),
            page("https://site.org/b", "", 1, 0, 1),
            page("https://site.org/c", "", 1, 0, 2),
            page("https://site.org/hot", "", 9, 0, 3),
        ];
        let out = world.run("site", &history, &[], &[]);
        assert_eq!(
            urls(&out[1..]),
            ["https://site.org/hot", "https://site.org/b", "https://site.org/a", "https://site.org/c"]
        );
        assert!(out[1].score > out[2].score);
        assert_eq!(out[2].score, out[3].score);
        assert_eq!(out[3].score, out[4].score);
    }

    #[test]
    fn suggest_caps_at_max() {
        let world = World::new();
        let history: Vec<HistoryEntry> = (0..20).map(|i| page(&format!("https://site.org/p{i:02}"), "", 1, 0, 1)).collect();
        let out = world.run("site", &history, &[], &[]);
        assert_eq!(out.len(), MAX_SUGGESTIONS);
        let remote: Vec<String> = (0..30).map(|i| format!("site {i}")).collect();
        let remote: Vec<&str> = remote.iter().map(String::as_str).collect();
        assert_eq!(world.run("site", &history, &[], &remote).len(), MAX_SUGGESTIONS);
    }

    #[test]
    fn suggest_dedupes_scheme_www_and_slash() {
        let world = World::new();
        let history = [
            page("http://www.example.com/page/", "", 1, 0, 1),
            page("https://example.com/page", "", 3, 0, 1),
            page("https://example.com/other", "", 1, 0, 1),
        ];
        let out = world.run("exam", &history, &[], &[]);
        let pages: Vec<&Suggestion> = out.iter().filter(|s| s.kind == Kind::History).collect();
        assert_eq!(pages.len(), 2);
        // The better-scored duplicate (3 visits) is the one kept.
        assert_eq!(pages[0].url.as_str(), "https://example.com/page");
        assert_eq!(pages[1].url.as_str(), "https://example.com/other");
    }

    #[test]
    fn suggest_bookmark_and_history_merge_with_star() {
        let world = World::new();
        let history = [page("https://example.com/", "Example Domain", 1, 0, 0)];
        let marks = [bookmark("https://example.com/", "Example Domain")];
        let out = world.run("exam", &history, &marks, &[]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].kind, Kind::History);
        assert!(out[1].bookmarked);
        // Bookmark: title match 4*14/14 = 4, URL match 4*20/20 = 4; 8 / (14 + 10) = 0.3333; 0.3333 * 299 = 99.67 -> 99;
        // 900 + 99 = 999. History: 18 points = 1.574, frequency 1.0, one matching page x3 = 4.72 -> 625 + 84.375 * 3.72 = 939.
        // The merged item takes the larger one.
        assert_eq!(out[1].score, 999);

        // Without the history page the bookmark stays a bookmark.
        let alone = world.run("exam", &[], &marks, &[]);
        assert_eq!(alone[1].kind, Kind::Bookmark);
        assert!(alone[1].bookmarked);
        assert_eq!(alone[1].score, 999);

        // The same URL bookmarked twice adds Chromium's boost of 75: 999 + 75.
        let twice = [bookmark("https://example.com/", "Example Domain"), bookmark("https://example.com/", "Example Domain")];
        let boosted = world.run("exam", &[], &twice, &[]);
        assert_eq!(boosted.len(), 2);
        assert_eq!(boosted[1].score, 1074);

        // A word that starts no word in title or URL is not a match.
        assert_eq!(world.run("xample", &[], &marks, &[]).len(), 1);
    }

    #[test]
    fn suggest_drops_item_equal_to_heuristic() {
        let world = World::new();
        let history = [
            page("https://example.com/", "Example", 5, 5, 0),
            page("http://www.example.com", "Example", 5, 0, 0),
            page("https://example.com/other", "Other", 2, 0, 0),
        ];
        let out = world.run("example.com", &history, &[bookmark("https://example.com/", "Example")], &[]);
        assert_eq!(out[0].url.as_str(), "https://example.com/");
        assert_eq!(urls(&out[1..]), ["https://example.com/other"]);
    }

    #[test]
    fn suggest_remote_keeps_engine_order_and_skips_verbatim() {
        let world = World::new();
        let out = world.run("rust", &[], &[], &["rust lang", "Rust", "rustup", "rust lang"]);
        let remote: Vec<(&str, i32)> = out.iter().filter(|s| s.kind == Kind::RemoteSearch).map(|s| (s.text.as_str(), s.score)).collect();
        // 600 plus the number of results after it (4 in the engine's list): 603, then 601 for the third.
        assert_eq!(remote, [("rust lang", 603), ("rustup", 601)]);
        assert_eq!(out[1].url.as_str(), "https://duckduckgo.com/html/?q=rust+lang");
    }

    #[test]
    fn suggest_remote_not_shown_for_url_input() {
        // Choice: for text that reads as an address, search suggestions are left out (Chromium lists them at 300).
        // Address-like text with a space is a different case and shows them at the lower base.
        let world = World::new();
        let out = world.run("example.com", &[], &[], &["example.com login"]);
        assert_eq!(out.len(), 1);
        let spaced = world.run("example.com/a b", &[], &[], &["example.com login"]);
        assert_eq!(spaced[0].kind, Kind::Heuristic);
        assert_eq!((spaced[1].kind, spaced[1].score), (Kind::RemoteSearch, 300));
        // Keyword searches never list the default engine's suggestions.
        assert!(world.run("w rust", &[], &[], &["rust lang"]).iter().all(|s| s.kind != Kind::RemoteSearch));
    }

    #[test]
    fn suggest_remote_never_above_prefix_matching_history() {
        let world = World::new();
        // 100 days old: weight 0.19; x3 for the single match: 0.57 -> 550 + 75 * 0.57 = 592.
        let history = [page("https://rust-lang.org/", "", 1, 0, 100)];
        let out = world.run("rust", &history, &[], &["rust lang", "rustup"]);
        let kinds: Vec<Kind> = out.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [Kind::Heuristic, Kind::History, Kind::RemoteSearch, Kind::RemoteSearch]);
        assert_eq!(out[1].score, 592);
        // Capped one under the history item, still in the engine's order.
        assert_eq!((out[2].score, out[3].score), (591, 590));
    }

    #[test]
    fn suggest_keyword_search_adds_the_plain_search() {
        let world = World::new();
        let out = world.run("w rust lang", &[], &[], &[]);
        assert_eq!(out[0].kind, Kind::Heuristic);
        assert_eq!(out[0].url.as_str(), "https://en.wikipedia.org/w/index.php?search=rust+lang");
        assert_eq!(out[0].title, "rust lang \u{2014} Wikipedia");
        assert_eq!((out[1].kind, out[1].score), (Kind::KeywordSearch, 250));
        assert_eq!(out[1].url.as_str(), "https://duckduckgo.com/html/?q=w+rust+lang");
    }

    #[test]
    fn suggest_inline_completes_origin() {
        let world = World::new();
        let history = [page("https://example.com/page", "", 3, 0, 0)];
        let out = world.run("exa", &history, &[], &[]);
        assert_eq!(out[0].inline_completion.as_deref(), Some("mple.com/"));
        // The heuristic item is still what the typed text alone does.
        assert_eq!(out[0].url.as_str(), "https://duckduckgo.com/html/?q=exa");
        // Only the first item carries it.
        assert!(out[1..].iter().all(|s| s.inline_completion.is_none()));
        // The most visited origin wins.
        let two = [page("https://alpha.org/", "", 1, 0, 0), page("https://alps.org/x", "", 9, 0, 0), page("https://alps.org/y", "", 9, 0, 0)];
        assert_eq!(world.run("al", &two, &[], &[])[0].inline_completion.as_deref(), Some("ps.org/"));
        // Bookmarks complete too; the typed case is kept and the candidate's case follows.
        let marks = [bookmark("https://Docs.Rs/crate", "docs")];
        assert_eq!(world.run("DOC", &[], &marks, &[])[0].inline_completion.as_deref(), Some("s.rs/"));
        // A typed slash switches to whole URLs (Firefox), and a scheme alone completes nothing.
        assert_eq!(world.run("example.com/", &history, &[], &[])[0].inline_completion.as_deref(), Some("page"));
        assert_eq!(world.run("https://", &history, &[], &[])[0].inline_completion, None);
    }

    #[test]
    fn suggest_inline_completes_full_url_when_typed_has_slash() {
        let world = World::new();
        let history = [page("https://example.com/page", "", 3, 0, 0), page("https://example.com/path/deep", "", 1, 0, 0)];
        let out = world.run("example.com/pa", &history, &[], &[]);
        assert_eq!(out[0].inline_completion.as_deref(), Some("ge"));
        // With the scheme typed, the scheme stays part of the shown text.
        let out = world.run("https://example.com/pat", &history, &[], &[]);
        assert_eq!(out[0].inline_completion.as_deref(), Some("h/deep"));
    }

    #[test]
    fn suggest_inline_ignores_scheme_and_www() {
        let world = World::new();
        let cases = [
            ("exa", "http://www.example.com/x", "mple.com/"),
            ("exa", "https://example.com/x", "mple.com/"),
            ("www.exa", "https://www.example.com/x", "mple.com/"),
            ("EXA", "https://example.com/x", "mple.com/"),
            ("example.com/x", "http://www.example.com/xyz", "yz"),
        ];
        for (typed, url, tail) in cases {
            let out = world.run(typed, &[page(url, "", 2, 0, 0)], &[], &[]);
            assert_eq!(out[0].inline_completion.as_deref(), Some(tail), "{typed} / {url}");
        }
        // Not a prefix of the host: no completion.
        let out = world.run("ample", &[page("https://example.com/", "", 2, 0, 0)], &[], &[]);
        assert_eq!(out[0].inline_completion, None);
    }

    #[test]
    fn suggest_no_inline_when_deleting() {
        let world = World::new();
        let history = [page("https://example.com/page", "", 3, 0, 0)];
        let out = world.run_with("exa", true, true, &history, &[], &[]);
        assert_eq!(out[0].inline_completion, None);
        assert_eq!(world.run_with("exa", true, false, &history, &[], &[])[0].inline_completion.as_deref(), Some("mple.com/"));
    }

    #[test]
    fn suggest_no_inline_with_space_or_cursor_not_at_end() {
        let world = World::new();
        let history = [page("https://example.com/page", "", 3, 0, 0)];
        assert_eq!(world.run_with("exa", false, false, &history, &[], &[])[0].inline_completion, None);
        assert_eq!(world.run("exa ", &history, &[], &[])[0].inline_completion, None);
        assert_eq!(world.run("exa mple", &history, &[], &[])[0].inline_completion, None);
    }
}
