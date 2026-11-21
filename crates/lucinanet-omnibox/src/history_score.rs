/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The Chromium-style history score: how well typed words match a page's URL and title (topicality), how often and
//! how recently the page was visited (frequency), mapped through a piecewise function to 0..=1399.
//!
//! Chromium components/omnibox/browser/scored_history_match.cc @e9a4673 with the default values of the field-trial
//! parameters read in omnibox_field_trial.cc @e9a4673 (listed next to each constant). The simplifications are in PORTING.md.

use url::Url;

use crate::rank::HistoryEntry;

const DAY_US: i64 = 86_400_000_000;
/// scored_history_match.cc kMaxRawTermScore: raw term scores above this share the last bucket.
const MAX_RAW_TERM_SCORE: i32 = 30;
/// scored_history_match.cc kDaysToPrecomputeRecencyScoresFor (366): older visits count as the oldest bucket.
const MAX_RECENCY_DAY: i64 = 365;
/// omnibox_field_trial.cc HQPMaxVisitsToScore default.
const MAX_VISITS_TO_SCORE: u32 = 10;
/// omnibox_field_trial.cc HQPTypedValue default: a typed visit is worth 1.5 plain visits.
const TYPED_VALUE: f64 = 1.5;
/// omnibox_field_trial.cc HQPExperimentalTopicalityThreshold default.
const TOPICALITY_THRESHOLD: f64 = 0.5;
/// omnibox_field_trial.cc HQPNumTitleWordsToAllow default: only matches in the first 20 title words count.
const NUM_TITLE_WORDS: usize = 20;
/// scored_history_match.cc kDefaultHQPBuckets "0.0:550,1:625,9.0:1300,90.0:1399".
const BUCKETS: [(f64, f64); 4] = [(0.0, 550.0), (1.0, 625.0), (9.0, 1300.0), (90.0, 1399.0)];
/// omnibox_field_trial.cc kDefaultNumMatchesScores "1:3,2:2.5,3:2,4:1.5".
const SPECIFICITY: [(usize, f64); 4] = [(1, 3.0), (2, 2.5), (3, 2.0), (4, 1.5)];

/// The history score of `entry` for `typed` at time `now_us`, 0..=1399; 0 when the typed words do not match.
///
/// Words are the whitespace-separated words of `typed`, case-folded. Each must occur in the URL (scheme, `www.` and a
/// bare trailing slash left out) or the title, and the matches must come at word boundaries (a match in the host
/// excepted) strongly enough for the average topicality to reach 0.5. The document-specificity factor is 1.0, which
/// is Chromium's value when many pages match; [`history_score_among`] takes the number of matching pages.
pub fn history_score(entry: &HistoryEntry, typed: &str, now_us: i64) -> i32 {
    history_score_among(entry, typed, now_us, usize::MAX)
}

/// Like [`history_score`], with the number of history pages that match `typed`: few matches raise the score
/// (1 page: x3, 2: x2.5, 3: x2, 4: x1.5, more: x1), as Chromium's document specificity does.
pub fn history_score_among(entry: &HistoryEntry, typed: &str, now_us: i64, num_matching_pages: usize) -> i32 {
    let terms = terms_of(typed);
    let topicality = topicality(entry, &terms);
    score_with(entry, topicality, now_us, num_matching_pages)
}

/// Final relevance from an already computed topicality; 0 when the topicality is 0.
pub(crate) fn score_with(entry: &HistoryEntry, topicality: f64, now_us: i64, num_matching_pages: usize) -> i32 {
    if topicality <= 0.0 {
        return 0;
    }
    let intermediate = topicality * frequency(entry, now_us) * specificity(num_matching_pages);
    final_relevancy(intermediate)
}

pub(crate) fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(one), None) => one,
        _ => c,
    }
}

pub(crate) fn fold_str(text: &str) -> Vec<char> {
    text.chars().map(fold).collect()
}

pub(crate) fn terms_of(typed: &str) -> Vec<Vec<char>> {
    typed.split_whitespace().map(fold_str).collect()
}

/// Indices where a word starts: an alphanumeric character not preceded by one.
pub(crate) fn word_starts(text: &[char]) -> Vec<usize> {
    let mut starts = Vec::new();
    for (i, c) in text.iter().enumerate() {
        if c.is_alphanumeric() && (i == 0 || !text[i - 1].is_alphanumeric()) {
            starts.push(i);
        }
    }
    starts
}

/// Non-overlapping occurrences of `term` in `hay`, left to right.
pub(crate) fn find_all(term: &[char], hay: &[char]) -> Vec<usize> {
    let mut found = Vec::new();
    if term.is_empty() || hay.len() < term.len() {
        return found;
    }
    let mut i = 0;
    while i + term.len() <= hay.len() {
        if hay[i..i + term.len()] == *term {
            found.push(i);
            i += term.len();
        } else {
            i += 1;
        }
    }
    found
}

/// The URL as matched: case-folded, without `http(s)://`, a leading `www.` and the slash of a bare host, with
/// non-ASCII percent-escapes decoded. Offsets mark where the host, path and query/fragment start.
pub(crate) struct Cleaned {
    pub text: Vec<char>,
    pub host_pos: usize,
    pub path_pos: usize,
    pub query_pos: usize,
    /// Index of the dot before the last host label, `usize::MAX` for a host without a dot.
    pub last_dot: usize,
}

pub(crate) fn clean_url(raw: &str) -> Cleaned {
    let (flat, host_pos, path_pos) = flatten(raw);
    let text = fold_str(&flat);
    let query_pos = text.iter().skip(path_pos).position(|c| matches!(c, '?' | '#')).map_or(text.len(), |i| i + path_pos);
    let last_dot = if path_pos > host_pos {
        text[host_pos..path_pos].iter().rposition(|c| *c == '.').map_or(usize::MAX, |i| i + host_pos)
    } else {
        usize::MAX
    };
    Cleaned { text, host_pos, path_pos, query_pos, last_dot }
}

fn flatten(raw: &str) -> (String, usize, usize) {
    if let Ok(url) = Url::parse(raw)
        && let Some(host) = url.host_str()
    {
        let mut out = String::new();
        if !matches!(url.scheme(), "http" | "https") {
            out.push_str(url.scheme());
            out.push_str("://");
        }
        let host_pos = out.chars().count();
        out.push_str(host.strip_prefix("www.").unwrap_or(host));
        if let Some(port) = url.port() {
            out.push(':');
            out.push_str(&port.to_string());
        }
        let path_pos = out.chars().count();
        let bare_host = url.path() == "/" && url.query().is_none() && url.fragment().is_none();
        if !bare_host {
            out.push_str(&unescape(url.path()));
        }
        if let Some(query) = url.query() {
            out.push('?');
            out.push_str(&unescape(query));
        }
        if let Some(fragment) = url.fragment() {
            out.push('#');
            out.push_str(&unescape(fragment));
        }
        (out, host_pos, path_pos)
    } else {
        // Without a host (about:, data:, mailto:) everything counts as path.
        (raw.trim().to_owned(), 0, 0)
    }
}

/// Decodes `%XX` runs that form non-ASCII UTF-8, so typed text can match them; ASCII escapes stay (a decoded `%2F`
/// would invent a path separator).
fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = hex_pair(bytes.get(i + 1).copied(), bytes.get(i + 2).copied())
            && byte >= 0x80
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_pair(hi: Option<u8>, lo: Option<u8>) -> Option<u8> {
    let digit = |b: u8| char::from(b).to_digit(16);
    Some((digit(hi?)? * 16 + digit(lo?)?) as u8)
}

/// Where in `text` the word part of `term` starts: `/foo` has its word at 1; a term with no word returns its length.
fn word_offset(term: &[char]) -> usize {
    term.iter().position(|c| c.is_alphanumeric()).unwrap_or(term.len())
}

/// Chromium FilterTermMatchesByWordStarts: a match survives when it is before `keep_before` (the host), starts a
/// word, holds no word at all, or continues where the previous kept match ended ("stack overflow" in "stackoverflow").
fn filter_matches(matches: &[(usize, usize)], terms: &[Vec<char>], word_starts: &[usize], keep_before: usize) -> Vec<(usize, usize)> {
    let mut kept = Vec::new();
    let mut last_end = 0;
    for &(offset, term_num) in matches {
        let term = &terms[term_num];
        let in_term = word_offset(term);
        if offset < keep_before
            || word_starts.binary_search(&(offset + in_term)).is_ok()
            || in_term == term.len()
            || offset == last_end
        {
            last_end = offset + term.len();
            kept.push((offset, term_num));
        }
    }
    kept
}

/// Average topicality of the terms in the entry, 0 when a term matches nowhere or the average is under 0.5.
pub(crate) fn topicality(entry: &HistoryEntry, terms: &[Vec<char>]) -> f64 {
    if terms.is_empty() {
        return 0.0;
    }
    let url = clean_url(&entry.url);
    let title = fold_str(&entry.title);
    let url_words = word_starts(&url.text);
    let title_words = word_starts(&title);
    let mut url_matches = Vec::new();
    let mut title_matches = Vec::new();
    for (term_num, term) in terms.iter().enumerate() {
        let in_url = find_all(term, &url.text);
        let in_title = find_all(term, &title);
        if in_url.is_empty() && in_title.is_empty() {
            return 0.0;
        }
        url_matches.extend(in_url.into_iter().map(|offset| (offset, term_num)));
        title_matches.extend(in_title.into_iter().map(|offset| (offset, term_num)));
    }
    url_matches.sort_unstable();
    title_matches.sort_unstable();

    let mut term_scores = vec![0i32; terms.len()];
    // The first filter keeps host matches wherever they fall; later matches must start a word. There is no scheme to
    // filter: it was cleaned away.
    for (offset, term_num) in filter_matches(&url_matches, terms, &url_words, url.path_pos) {
        let word_pos = offset + word_offset(&terms[term_num]);
        let at_boundary = url_words.binary_search(&word_pos).is_ok();
        term_scores[term_num] += if word_pos >= url.query_pos {
            5
        } else if word_pos >= url.path_pos {
            8
        } else if word_pos >= url.host_pos {
            // Matches in the last host label (the TLD) score nothing: HQPAllowMatchInTLD defaults to false.
            if word_pos < url.last_dot {
                if at_boundary { 10 } else { 2 }
            } else {
                0
            }
        } else {
            // Scheme: HQPAllowMatchInScheme defaults to false.
            0
        };
    }
    for (offset, term_num) in filter_matches(&title_matches, terms, &title_words, 0) {
        let word_pos = offset + word_offset(&terms[term_num]);
        let word_num = title_words.partition_point(|start| *start < word_pos);
        if word_num >= NUM_TITLE_WORDS {
            break;
        }
        term_scores[term_num] += 8;
    }

    let total: f64 = term_scores.iter().map(|score| topicality_of_term_score(*score)).sum();
    let average = total / terms.len() as f64;
    if average < TOPICALITY_THRESHOLD { 0.0 } else { average }
}

/// scored_history_match.cc InitRawTermScoreToTopicalityScoreArray: linear below 10 points, logarithmic above, so
/// 10 points (a host-boundary match) is 1.0 and 29 points about 2.05.
fn topicality_of_term_score(raw: i32) -> f64 {
    let raw = raw.clamp(0, MAX_RAW_TERM_SCORE - 1);
    if raw < 10 {
        0.1 * f64::from(raw)
    } else {
        1.0 + 2.25 * (0.1 * f64::from(raw)).log10()
    }
}

/// scored_history_match.cc InitDaysAgoToRecencyScoreArray, integer arithmetic included.
fn recency_weight(days_ago: i64) -> f64 {
    let d = days_ago.clamp(0, MAX_RECENCY_DAY) as i32;
    let unnormalized = if d <= 4 {
        100
    } else if d <= 14 {
        70 + (14 - d) * (100 - 70) / (14 - 4)
    } else if d <= 31 {
        50 + (31 - d) * (70 - 50) / (31 - 14)
    } else if d <= 90 {
        30 + (90 - d) * (50 - 30) / (90 - 30)
    } else {
        10 + (365 - d) * (20 - 10) / (365 - 90)
    };
    f64::from(unnormalized) / 100.0
}

/// scored_history_match.cc GetFrequency: the sum over the latest (at most 10) visits of the visit's worth (typed 1.5,
/// otherwise 1) times its recency weight. Only the last visit's time is known, so all counted visits get its weight.
pub(crate) fn frequency(entry: &HistoryEntry, now_us: i64) -> f64 {
    let counted = entry.visit_count.min(MAX_VISITS_TO_SCORE);
    let typed = entry.typed_count.min(counted);
    let days_ago = now_us.saturating_sub(entry.last_visit_us) / DAY_US;
    let weight = recency_weight(days_ago);
    (f64::from(typed) * TYPED_VALUE + f64::from(counted - typed)) * weight
}

/// scored_history_match.cc GetDocumentSpecificityScore: first table row whose count is at least `n`, else 1.0.
fn specificity(num_matching_pages: usize) -> f64 {
    SPECIFICITY.iter().find(|(count, _)| *count >= num_matching_pages).map_or(1.0, |(_, value)| *value)
}

/// scored_history_match.cc GetFinalRelevancyScore: linear interpolation between the bucket points, flat at the end.
fn final_relevancy(intermediate: f64) -> i32 {
    for pair in BUCKETS.windows(2) {
        let (low, high) = (pair[0], pair[1]);
        if intermediate < high.0 {
            let slope = (high.1 - low.1) / (high.0 - low.0);
            return (low.1 + slope * (intermediate - low.0)) as i32;
        }
    }
    BUCKETS[BUCKETS.len() - 1].1 as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000_000;

    fn entry(url: &str, title: &str, visits: u32, typed: u32, days_ago: i64) -> HistoryEntry {
        HistoryEntry { url: url.to_owned(), title: title.to_owned(), visit_count: visits, typed_count: typed, last_visit_us: NOW - days_ago * DAY_US }
    }

    #[test]
    fn history_score_zero_without_boundary_match() {
        // "xample" sits inside the host label: 2 points, topicality 0.2 < 0.5.
        assert_eq!(history_score(&entry("https://example.com/", "", 1, 0, 0), "xample", NOW), 0);
        // Inside a path word: dropped by the word-boundary filter.
        assert_eq!(history_score(&entry("https://a.com/reexample", "", 1, 0, 0), "example", NOW), 0);
        // Only in the TLD: no points by default.
        assert_eq!(history_score(&entry("https://example.com/", "", 1, 0, 0), "com", NOW), 0);
        assert_eq!(history_score(&entry("https://example.com/", "", 1, 0, 0), "   ", NOW), 0);
    }

    #[test]
    fn history_score_exact_values_by_hand() {
        let now = NOW;
        // Host-boundary match: 10 points -> topicality 1.0. One visit today -> frequency 1.0. Intermediate 1.0 is the
        // bucket point (1, 625).
        assert_eq!(history_score(&entry("https://example.com/", "", 1, 0, 0), "example", now), 625);
        // 5 visits: frequency 5.0 -> 625 + (1300-625)/(9-1) * (5-1) = 625 + 337.5 -> 962.
        assert_eq!(history_score(&entry("https://example.com/", "", 5, 0, 0), "example", now), 962);
        // 5 typed visits: 5 * 1.5 = 7.5 -> 625 + 84.375 * 6.5 = 1173.4 -> 1173.
        assert_eq!(history_score(&entry("https://example.com/", "", 5, 5, 0), "example", now), 1173);
        // 100 days: recency 10 + 265*10/275 = 19 -> 0.19 -> 550 + 75 * 0.19 = 564.25 -> 564.
        assert_eq!(history_score(&entry("https://example.com/", "", 1, 0, 100), "example", now), 564);
        // With one matching page the specificity factor is 3: 625 + 84.375 * 2 = 793.75 -> 793.
        assert_eq!(history_score_among(&entry("https://example.com/", "", 1, 0, 0), "example", now, 1), 793);
    }

    #[test]
    fn history_score_more_visits_scores_higher() {
        let few = history_score(&entry("https://example.com/", "", 1, 0, 0), "example", NOW);
        let many = history_score(&entry("https://example.com/", "", 5, 0, 0), "example", NOW);
        let lots = history_score(&entry("https://example.com/", "", 50, 0, 0), "example", NOW);
        assert!(few < many && many < lots, "{few} {many} {lots}");
    }

    #[test]
    fn history_score_typed_visits_score_higher() {
        let plain = history_score(&entry("https://example.com/", "", 3, 0, 0), "example", NOW);
        let typed = history_score(&entry("https://example.com/", "", 3, 3, 0), "example", NOW);
        assert!(typed > plain, "{typed} {plain}");
    }

    #[test]
    fn history_score_recent_beats_old() {
        let recent = history_score(&entry("https://example.com/", "", 1, 0, 4), "example", NOW);
        let old = history_score(&entry("https://example.com/", "", 1, 0, 100), "example", NOW);
        assert!(recent > old, "{recent} {old}");
        assert_eq!(recent, 625);
    }

    #[test]
    fn history_score_all_terms_must_match() {
        let page = entry("https://example.com/", "Example Domain", 1, 0, 0);
        assert!(history_score(&page, "example domain", NOW) > 0);
        assert_eq!(history_score(&page, "example nothere", NOW), 0);
        assert_eq!(history_score(&page, "nothere example", NOW), 0);
    }

    #[test]
    fn history_score_host_match_beats_path_match() {
        let host = history_score(&entry("https://docs.site.org/", "", 1, 0, 0), "docs", NOW);
        let path = history_score(&entry("https://site.org/docs", "", 1, 0, 0), "docs", NOW);
        // Host boundary 10 points = 1.0 -> 625. Path boundary 8 points = 0.8 -> about 550 + 75 * 0.8 = 610.
        assert_eq!(host, 625);
        assert!((609..=610).contains(&path), "{path}");
        let query = history_score(&entry("https://site.org/?docs=1", "", 1, 0, 0), "docs", NOW);
        // Query 5 points = 0.5 = the threshold, kept: 550 + 37.5 -> 587.
        assert_eq!(query, 587);
    }

    #[test]
    fn history_score_title_and_continuation() {
        // Title word boundary alone: 8 points -> 0.8 -> about 610.
        let title_only = history_score(&entry("https://a.example/", "Best Pizza Ever", 1, 0, 0), "pizza", NOW);
        assert!((609..=610).contains(&title_only), "{title_only}");
        // "stack overflow" continues into "stackoverflow" (second match starts where the first ended).
        assert!(history_score(&entry("https://stackoverflow.com/q", "", 1, 0, 0), "stack overflow", NOW) > 0);
    }

    #[test]
    fn history_score_unicode_and_percent_escapes() {
        let page = entry("https://example.org/Stra%C3%9Fe", "", 1, 0, 0);
        assert!(history_score(&page, "straße", NOW) > 0);
    }

    #[test]
    fn history_score_is_deterministic_and_bounded() {
        let pages = [
            entry("https://example.com/", "Example Domain", u32::MAX, u32::MAX, 0),
            entry("https://example.com/a?b=c#d", "x", 0, 0, 100_000),
            entry("about:blank", "", 3, 1, -5),
            entry("not a url", "not a url", 7, 7, 30),
            entry("https://example.com/", "", 1, 0, 0),
        ];
        for page in &pages {
            for typed in ["example", "e", "a b", "not", "blank", "ex am", "", "%"] {
                let first = history_score(page, typed, NOW);
                assert_eq!(first, history_score(page, typed, NOW));
                assert!((0..=1400).contains(&first), "{first}");
                for among in [1, 2, 5, usize::MAX] {
                    assert!((0..=1400).contains(&history_score_among(page, typed, NOW, among)));
                }
            }
        }
        // 10 typed visits today: 15.0. URL 10 + title 8 = 18 points: 1 + 2.25 * log10(1.8) = 1.5744. Product 23.615 sits
        // between the points (9, 1300) and (90, 1399): 1300 + 99/81 * 14.615 = 1317.86 -> 1317.
        assert_eq!(history_score(&pages[0], "example", NOW), 1317);
    }

    #[test]
    fn recency_table_matches_chromium_points() {
        assert_eq!(recency_weight(0), 1.0);
        assert_eq!(recency_weight(4), 1.0);
        assert_eq!(recency_weight(14), 0.70);
        assert_eq!(recency_weight(31), 0.50);
        assert_eq!(recency_weight(90), 0.30);
        assert_eq!(recency_weight(365), 0.10);
        assert_eq!(recency_weight(10_000), 0.10);
        assert_eq!(recency_weight(-3), 1.0);
    }
}
