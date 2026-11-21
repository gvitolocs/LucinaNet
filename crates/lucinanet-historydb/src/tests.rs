/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Expected frecency integers are hand calculated with lambda = ln 2 / 30 = 0.0231049 (Firefox's half-life of 30 days):
//! score = sum(weight * decay) / samples * max(visit_count, samples); result = reference_day + trunc(ln(score) / lambda).
//! ln(20)/lambda = 129.66, ln(50)/lambda = 169.31, ln(100)/lambda = 199.32, ln(200)/lambda = 229.31.

use crate::*;

const DAY: i64 = 86_400_000_000;
const D: i64 = 20_000; // reference day number

fn at(day: i64, extra: i64) -> i64 {
    day * DAY + extra
}

fn vis(id: u64, from: Option<u64>, time_us: i64, tr: Transition, src: Source, interesting: bool) -> Visit {
    Visit { id, from, time_us, transition: tr, source: src, interesting }
}

fn link(id: u64, time_us: i64) -> Visit {
    vis(id, None, time_us, Transition::Link, Source::Organic, false)
}

fn nv(url: &str, time_us: i64, transition: Transition) -> NewVisit<'_> {
    NewVisit { url, title: None, time_us, transition, source: Source::Organic, interesting: false, from: None }
}

fn p() -> FrecencyParams {
    FrecencyParams::default()
}

#[test]
fn transition_codes_roundtrip_1_to_9() {
    for code in 1..=9u8 {
        assert_eq!(Transition::from_code(code).map(Transition::code), Some(code));
    }
    assert_eq!(Transition::from_code(0), None);
    assert_eq!(Transition::from_code(10), None);
    assert_eq!(Transition::from_code(7), Some(Transition::Download));
}

#[test]
fn record_visit_creates_page_and_counts() {
    let mut db = HistoryDb::new();
    let (pid, vid) = db.record_visit(NewVisit { title: Some("Hello"), ..nv("https://a.test/", 100, Transition::Link) }).unwrap();
    let (pid2, vid2) = db.record_visit(nv("https://a.test/", 200, Transition::Link)).unwrap();
    assert_eq!(pid, pid2);
    assert_ne!(vid, vid2);
    let page = db.page("https://a.test/").unwrap();
    assert_eq!((page.visit_count, page.typed_count, page.last_visit_us), (2, 0, 200));
    assert_eq!(page.title.as_deref(), Some("Hello"));
    assert_eq!(db.len(), 1);
    assert_eq!(db.page_by_id(pid).map(|x| x.url.as_str()), Some("https://a.test/"));
}

#[test]
fn typed_visit_bumps_typed_count() {
    let mut db = HistoryDb::new();
    db.record_visit(nv("https://a.test/", 1, Transition::Typed));
    db.record_visit(nv("https://a.test/", 2, Transition::Link));
    db.record_visit(nv("https://a.test/", 3, Transition::Typed));
    let page = db.page("https://a.test/").unwrap();
    assert_eq!((page.visit_count, page.typed_count), (3, 2));
}

#[test]
fn reload_and_download_do_not_count() {
    let mut db = HistoryDb::new();
    for (t, tr) in [Transition::Link, Transition::Reload, Transition::Download, Transition::FramedLink, Transition::Embed]
        .into_iter()
        .enumerate()
    {
        db.record_visit(nv("https://a.test/", t as i64 + 1, tr));
    }
    assert_eq!(db.page("https://a.test/").unwrap().visit_count, 1);
    assert_eq!(db.visits(1).len(), 5);
}

#[test]
fn embed_only_page_is_hidden() {
    let mut db = HistoryDb::new();
    db.record_visit(nv("https://a.test/frame", 1, Transition::Embed));
    assert!(db.page("https://a.test/frame").unwrap().hidden);
    assert_eq!(db.page("https://a.test/frame").unwrap().visit_count, 0);
}

#[test]
fn normal_visit_unhides_page() {
    let mut db = HistoryDb::new();
    db.record_visit(nv("https://a.test/", 1, Transition::Embed));
    db.record_visit(nv("https://a.test/", 2, Transition::Link));
    assert!(!db.page("https://a.test/").unwrap().hidden);
}

#[test]
fn redirect_source_is_hidden_once_its_target_is_recorded() {
    let mut db = HistoryDb::new();
    let (_, src) = db.record_visit(nv("http://a.test/", 1, Transition::Typed)).unwrap();
    assert!(!db.page("http://a.test/").unwrap().hidden);
    db.record_visit(NewVisit { from: Some(src), ..nv("https://a.test/", 2, Transition::RedirectPermanent) });
    assert!(db.page("http://a.test/").unwrap().hidden);
    assert!(!db.page("https://a.test/").unwrap().hidden);
}

#[test]
fn about_and_data_urls_are_not_stored() {
    let mut db = HistoryDb::new();
    for url in [
        "",
        "about:blank",
        "data:text/html,hi",
        "javascript:void(0)",
        "view-source:https://a.test/",
        "place:sort=8",
        "chrome://browser/content/",
        "http://user:pw@a.test/",
        "no-scheme",
    ] {
        assert_eq!(db.record_visit(nv(url, 1, Transition::Link)), None, "{url}");
    }
    let long = format!("https://a.test/{}", "x".repeat(2000));
    assert_eq!(db.record_visit(nv(&long, 1, Transition::Link)), None);
    assert!(db.is_empty());
    assert!(db.record_visit(nv("ftp://a.test/f", 1, Transition::Link)).is_some());
}

#[test]
fn title_set_and_empty_ignored() {
    let mut db = HistoryDb::new();
    db.record_visit(nv("https://a.test/", 1, Transition::Link));
    db.set_title("https://a.test/", "First");
    db.set_title("https://a.test/", "");
    assert_eq!(db.page("https://a.test/").unwrap().title.as_deref(), Some("First"));
    db.record_visit(NewVisit { title: Some(""), ..nv("https://a.test/", 2, Transition::Link) });
    assert_eq!(db.page("https://a.test/").unwrap().title.as_deref(), Some("First"));
    db.record_visit(NewVisit { title: Some("Second"), ..nv("https://a.test/", 3, Transition::Link) });
    assert_eq!(db.page("https://a.test/").unwrap().title.as_deref(), Some("Second"));
    db.set_title("https://unknown.test/", "x");
}

#[test]
fn visits_newest_first_and_capped() {
    let mut db = HistoryDb::new();
    let mut pid = 0;
    for t in (1..=105).rev() {
        pid = db.record_visit(nv("https://a.test/", t, Transition::Link)).unwrap().0;
    }
    let v = db.visits(pid);
    assert_eq!(v.len(), MAX_VISITS_PER_PAGE);
    assert_eq!(v[0].time_us, 105);
    assert_eq!(v[99].time_us, 6);
    assert!(v.windows(2).all(|w| w[0].time_us > w[1].time_us));
}

#[test]
fn count_survives_cap() {
    let mut db = HistoryDb::new();
    for t in 1..=105 {
        db.record_visit(nv("https://a.test/", t, Transition::Typed));
    }
    let page = db.page("https://a.test/").unwrap();
    assert_eq!((page.visit_count, page.typed_count), (105, 105));
    assert_eq!(db.visits(page.id).len(), 100);
}

// One Link visit, not interesting: weight medium 50, samples 1 -> score 50 * 1 = 50; ln(50) / lambda = 3.91202 / 0.0231049 = 169.31 -> 169.
#[test]
fn frecency_single_link_visit() {
    assert_eq!(frecency(&[link(1, at(D, 7))], 1, None, false, &p()), D + 169);
}

// Typed, organic, no redirect child: high 100 -> ln(100) / lambda = 199.32 -> 199.
#[test]
fn frecency_typed_beats_link() {
    let typed = vis(1, None, at(D, 0), Transition::Typed, Source::Organic, false);
    assert_eq!(frecency(&[typed], 1, None, false, &p()), D + 199);
}

// Interesting typed visit: very high 200 -> ln(200) / lambda = 229.31 -> 229. An interesting plain link is only high: 199.
#[test]
fn frecency_interesting_visit_very_high() {
    let typed = vis(1, None, at(D, 0), Transition::Typed, Source::Organic, true);
    assert_eq!(frecency(&[typed], 1, None, false, &p()), D + 229);
    let l = vis(1, None, at(D, 0), Transition::Link, Source::Organic, true);
    assert_eq!(frecency(&[l], 1, None, false, &p()), D + 199);
}

// Visit 1 (Link) has a redirect child (visit 2): low 20. Visit 2 (target, effective type Link, no child): medium 50.
// score = (20 + 50) / 2 * max(2, 2) = 70; ln(70) / lambda = 4.24850 / 0.0231049 = 183.88 -> 183.
#[test]
fn frecency_redirect_source_is_low() {
    let src = link(1, at(D, 1));
    let dst = vis(2, Some(1), at(D, 2), Transition::RedirectTemporary, Source::Organic, false);
    assert_eq!(frecency(&[dst, src], 2, None, false, &p()), D + 183);
}

// Visit 1 (Typed) has a redirect child: low 20. Visit 2 is a redirect target whose source was Typed: counts as Typed, high 100.
// score = (100 + 20) / 2 * 2 = 120; ln(120) / lambda = 4.78749 / 0.0231049 = 207.21 -> 207 (as a plain redirect target it would be 183).
#[test]
fn frecency_redirect_target_uses_source_type() {
    let src = vis(1, None, at(D, 1), Transition::Typed, Source::Organic, false);
    let dst = vis(2, Some(1), at(D, 2), Transition::RedirectPermanent, Source::Organic, false);
    assert_eq!(frecency(&[dst, src], 2, None, false, &p()), D + 207);
}

// Only the Link visit is sampled: same as the single-link case, 169.
#[test]
fn frecency_ignores_download_framed_reload_visits() {
    let visits = [
        vis(4, None, at(D, 9), Transition::Reload, Source::Organic, true),
        vis(3, None, at(D + 5, 0), Transition::FramedLink, Source::Organic, true),
        vis(2, None, at(D + 9, 0), Transition::Download, Source::Organic, true),
        link(1, at(D, 1)),
    ];
    assert_eq!(frecency(&visits, 1, None, false, &p()), D + 169);
}

// Eleven visits on day D, visit_count 11. The oldest (a Typed one, weight 100) is outside the newest ten:
// score = 50 * 10 / 10 * max(11, 10) = 550; ln(550) / lambda = 6.30992 / 0.0231049 = 273.09 -> 273.
// (Had it been sampled: (500 + 100) / 11 * 11 = 600 -> 276.)
#[test]
fn frecency_samples_only_newest_ten() {
    let mut visits: Vec<Visit> = (1..=10).rev().map(|i| link(i + 1, at(D, 1000 + i as i64))).collect();
    visits.push(vis(1, None, at(D, 0), Transition::Typed, Source::Organic, false));
    assert_eq!(frecency(&visits, 11, None, false, &p()), D + 273);
}

// One sample of weight 50, visit_count 100: score = 50 / 1 * 100 = 5000; ln(5000) / lambda = 8.51719 / 0.0231049 = 368.62 -> 368.
#[test]
fn frecency_uses_visit_count_when_larger_than_samples() {
    assert_eq!(frecency(&[link(1, at(D, 0))], 100, None, false, &p()), D + 368);
}

// Same weights and counts, last seen 30 days earlier: the reference day is 30 less and the offset is identical.
#[test]
fn frecency_shifts_by_age_in_days() {
    let newer = frecency(&[link(1, at(D, 0))], 1, None, false, &p());
    let older = frecency(&[link(1, at(D - 30, 0))], 1, None, false, &p());
    assert_eq!(newer - older, 30);
    assert_eq!(older, D - 30 + 169);
}

// No visits, bookmarked on day 19000: one `high` sample, visit_count 0 -> max(0, 1) = 1; score 100 -> 19000 + 199.
#[test]
fn frecency_no_visits_bookmark_only() {
    assert_eq!(frecency(&[], 0, Some(at(19_000, 5)), false, &p()), 19_000 + 199);
}

#[test]
fn frecency_no_visits_no_bookmark_is_zero() {
    assert_eq!(frecency(&[], 0, None, false, &p()), 0);
}

#[test]
fn frecency_place_url_is_zero() {
    let v = [link(1, at(D, 0))];
    assert_eq!(url_frecency("place:sort=8", &v, 1, None, false, &p()), 0);
    assert_eq!(url_frecency("https://a.test/", &v, 1, None, false, &p()), D + 169);
}

// First visit: Link on day D, medium -> D + 169. Adding a Typed visit the same day: (100 + 50) / 2 * max(2, 2) = 150;
// ln(150) / lambda = 5.01064 / 0.0231049 = 216.86 -> 216.
#[test]
fn frecency_recomputed_on_each_visit() {
    let mut db = HistoryDb::new();
    db.record_visit(nv("https://a.test/", at(D, 1), Transition::Link));
    assert_eq!(db.page("https://a.test/").unwrap().frecency, D + 169);
    db.record_visit(nv("https://a.test/", at(D, 2), Transition::Typed));
    assert_eq!(db.page("https://a.test/").unwrap().frecency, D + 216);
}

#[test]
fn frecency_of_redirect_source_page_is_lowered_when_target_arrives() {
    let mut db = HistoryDb::new();
    let (_, src) = db.record_visit(nv("http://a.test/", at(D, 1), Transition::Link)).unwrap();
    assert_eq!(db.page("http://a.test/").unwrap().frecency, D + 169);
    db.record_visit(NewVisit { from: Some(src), ..nv("https://a.test/", at(D, 2), Transition::RedirectTemporary) });
    // Source visit is now low 20, one sample: ln(20) / lambda = 129.66 -> 129.
    assert_eq!(db.page("http://a.test/").unwrap().frecency, D + 129);
}

#[test]
fn recompute_frecency_uses_bookmark_date_without_countable_visits() {
    let mut db = HistoryDb::new();
    db.record_visit(nv("https://a.test/", at(D, 1), Transition::Download));
    assert_eq!(db.page("https://a.test/").unwrap().frecency, 0);
    db.recompute_frecency("https://a.test/", Some(at(19_000, 0)), false);
    assert_eq!(db.page("https://a.test/").unwrap().frecency, 19_000 + 199);
}

fn titled(db: &mut HistoryDb, url: &str, title: &str, t: i64, tr: Transition) {
    db.record_visit(NewVisit { title: Some(title), ..nv(url, t, tr) });
}

fn urls(v: Vec<&Page>) -> Vec<&str> {
    v.into_iter().map(|p| p.url.as_str()).collect()
}

fn q(text: &str, behavior: MatchBehavior) -> Query<'_> {
    Query { text, behavior, limit: 50, include_hidden: false, since_us: None }
}

#[test]
fn search_all_tokens_must_match() {
    let mut db = HistoryDb::new();
    titled(&mut db, "https://rust-lang.org/learn", "Learn Rust", 1, Transition::Link);
    titled(&mut db, "https://example.org/", "Rust Example", 2, Transition::Link);
    assert_eq!(urls(db.search(&q("rust learn", MatchBehavior::Anywhere))), ["https://rust-lang.org/learn"]);
    assert_eq!(db.search(&q("RUST", MatchBehavior::Anywhere)).len(), 2);
    assert!(db.search(&q("rust nothing", MatchBehavior::Anywhere)).is_empty());
}

#[test]
fn search_anywhere_substring() {
    let mut db = HistoryDb::new();
    titled(&mut db, "https://a.test/foobar", "T", 1, Transition::Link);
    assert_eq!(db.search(&q("oba", MatchBehavior::Anywhere)).len(), 1);
    assert!(db.search(&q("obz", MatchBehavior::Anywhere)).is_empty());
}

#[test]
fn search_boundary_requires_word_start() {
    let mut db = HistoryDb::new();
    titled(&mut db, "https://a.test/foo-bar", "Thing", 1, Transition::Link);
    titled(&mut db, "https://a.test/foobar", "Other", 2, Transition::Link);
    assert_eq!(urls(db.search(&q("bar", MatchBehavior::Boundary))), ["https://a.test/foo-bar"]);
    assert_eq!(db.search(&q("foo", MatchBehavior::Boundary)).len(), 2);
    assert_eq!(urls(db.search(&q("thing", MatchBehavior::Boundary))), ["https://a.test/foo-bar"]);
}

#[test]
fn search_begin_ignores_scheme_and_www() {
    let mut db = HistoryDb::new();
    titled(&mut db, "https://www.example.org/", "Some Title", 1, Transition::Link);
    assert_eq!(db.search(&q("exa", MatchBehavior::Begin)).len(), 1);
    assert_eq!(db.search(&q("some", MatchBehavior::Begin)).len(), 1);
    assert!(db.search(&q("www", MatchBehavior::Begin)).is_empty());
    assert!(db.search(&q("https", MatchBehavior::Begin)).is_empty());
    assert!(db.search(&q("title", MatchBehavior::Begin)).is_empty());
    assert!(db.search(&q("ample", MatchBehavior::Begin)).is_empty());
}

#[test]
fn search_orders_by_frecency_then_recency_then_url() {
    let mut db = HistoryDb::new();
    titled(&mut db, "https://x.test/", "t", at(D, 5000), Transition::Link);
    titled(&mut db, "https://z.test/", "t", at(D, 9000), Transition::Link);
    titled(&mut db, "https://y.test/", "t", at(D, 9000), Transition::Link);
    titled(&mut db, "https://b.test/", "t", at(D, 100), Transition::Typed);
    // b: typed (D + 199); x, y, z: link on the same day (D + 169): newer first, equal times by URL.
    assert_eq!(
        urls(db.search(&q("t", MatchBehavior::Anywhere))),
        ["https://b.test/", "https://y.test/", "https://z.test/", "https://x.test/"]
    );
    let two = Query { limit: 2, ..q("t", MatchBehavior::Anywhere) };
    assert_eq!(db.search(&two).len(), 2);
}

#[test]
fn search_excludes_hidden_unless_asked() {
    let mut db = HistoryDb::new();
    titled(&mut db, "https://a.test/frame", "t", 1, Transition::Embed);
    titled(&mut db, "https://a.test/page", "t", 2, Transition::Link);
    assert_eq!(urls(db.search(&q("a.test", MatchBehavior::Anywhere))), ["https://a.test/page"]);
    let all = Query { include_hidden: true, ..q("a.test", MatchBehavior::Anywhere) };
    assert_eq!(db.search(&all).len(), 2);
}

#[test]
fn search_since_filters() {
    let mut db = HistoryDb::new();
    titled(&mut db, "https://old.test/", "t", 100, Transition::Link);
    titled(&mut db, "https://new.test/", "t", 200, Transition::Link);
    let since = Query { since_us: Some(200), ..q("test", MatchBehavior::Anywhere) };
    assert_eq!(urls(db.search(&since)), ["https://new.test/"]);
}

#[test]
fn search_empty_text_and_zero_limit_return_nothing() {
    let mut db = HistoryDb::new();
    titled(&mut db, "https://a.test/", "t", 1, Transition::Link);
    assert!(db.search(&q("", MatchBehavior::Anywhere)).is_empty());
    assert!(db.search(&q("   ", MatchBehavior::Anywhere)).is_empty());
    assert!(db.search(&Query { limit: 0, ..q("a", MatchBehavior::Anywhere) }).is_empty());
}

#[test]
fn recent_newest_first() {
    let mut db = HistoryDb::new();
    db.record_visit(nv("https://a.test/", 10, Transition::Link));
    db.record_visit(nv("https://b.test/", 30, Transition::Link));
    db.record_visit(nv("https://c.test/", 20, Transition::Link));
    db.record_visit(nv("https://e.test/", 40, Transition::Embed));
    assert_eq!(urls(db.recent(10, false)), ["https://b.test/", "https://c.test/", "https://a.test/"]);
    assert_eq!(urls(db.recent(2, true)), ["https://e.test/", "https://b.test/"]);
}

#[test]
fn remove_url_removes_visits() {
    let mut db = HistoryDb::new();
    let (pid, _) = db.record_visit(nv("https://a.test/", 1, Transition::Link)).unwrap();
    db.record_visit(nv("https://b.test/", 2, Transition::Link));
    assert!(db.remove_url("https://a.test/"));
    assert!(!db.remove_url("https://a.test/"));
    assert!(db.page("https://a.test/").is_none());
    assert!(db.visits(pid).is_empty());
    assert_eq!(db.len(), 1);
}

#[test]
fn remove_range_half_open_and_drops_empty_pages() {
    let mut db = HistoryDb::new();
    db.record_visit(nv("https://a.test/", 10, Transition::Link));
    db.record_visit(nv("https://a.test/", 20, Transition::Link));
    db.record_visit(nv("https://b.test/", 30, Transition::Link));
    assert_eq!(db.remove_range(10, 20), 0);
    let a = db.page("https://a.test/").unwrap();
    assert_eq!((a.visit_count, a.last_visit_us), (1, 20));
    assert_eq!(db.visits(a.id).len(), 1);
    assert_eq!(db.remove_range(20, 31), 2);
    assert!(db.is_empty());
}

// Visits Link(1), Reload(2), Link(3); removing [1, 3) removes one countable visit (the reload does not count): 2 -> 1,
// which equals the one surviving countable visit.
#[test]
fn remove_range_reduces_counts() {
    let mut db = HistoryDb::new();
    db.record_visit(nv("https://a.test/", 1, Transition::Link));
    db.record_visit(nv("https://a.test/", 2, Transition::Reload));
    db.record_visit(nv("https://a.test/", 3, Transition::Link));
    assert_eq!(db.page("https://a.test/").unwrap().visit_count, 2);
    db.remove_range(1, 3);
    assert_eq!(db.page("https://a.test/").unwrap().visit_count, 1);
    // 105 visits, 100 kept (6..=105), count 105. Removing [0, 50) drops 44 stored ones: 105 - 44 = 61, above the 56 surviving.
    let mut db = HistoryDb::new();
    for t in 1..=105 {
        db.record_visit(nv("https://c.test/", t, Transition::Link));
    }
    db.remove_range(0, 50);
    assert_eq!(db.page("https://c.test/").unwrap().visit_count, 61);
    // Removing [0, 106) minus the newest keeps the floor: remove everything but t=105 -> surviving 1, 61 - 55 = 6 -> 6.
    db.remove_range(0, 105);
    assert_eq!(db.page("https://c.test/").unwrap().visit_count, 6);
}

#[test]
fn clear_empties() {
    let mut db = HistoryDb::new();
    db.record_visit(nv("https://a.test/", 1, Transition::Link));
    db.clear();
    assert!(db.is_empty());
    assert!(db.recent(10, true).is_empty());
    assert_eq!(db.record_visit(nv("https://a.test/", 1, Transition::Link)), Some((1, 1)));
}

fn sample_db() -> HistoryDb {
    let mut db = HistoryDb::new();
    titled(&mut db, "https://a.test/", "Alpha \"quoted\"", at(D, 1), Transition::Typed);
    let (_, src) = db.record_visit(nv("http://b.test/", at(D, 2), Transition::Link)).unwrap();
    db.record_visit(NewVisit { from: Some(src), source: Source::Search, interesting: true, ..nv("https://b.test/", at(D, 3), Transition::RedirectPermanent) });
    db.record_visit(nv("https://c.test/", at(D, 4), Transition::Embed));
    db
}

#[test]
fn json_roundtrip() {
    let db = sample_db();
    let text = db.to_json();
    let back = HistoryDb::from_json(&text).unwrap();
    assert_eq!(back.to_json(), text);
    assert_eq!(back.len(), 4);
    for url in ["https://a.test/", "http://b.test/", "https://b.test/", "https://c.test/"] {
        let (x, y) = (db.page(url), back.page(url));
        assert_eq!(x, y, "{url}");
        if let Some(x) = x {
            assert_eq!(db.visits(x.id), back.visits(x.id));
        }
    }
    // Indexes are rebuilt: the redirect source stays a redirect source, new ids continue.
    let mut back = back;
    assert!(back.page("http://b.test/").unwrap().hidden);
    let (pid, vid) = back.record_visit(nv("https://d.test/", at(D, 9), Transition::Link)).unwrap();
    assert_eq!((pid, vid), (5, 5));
}

#[test]
fn json_is_deterministic() {
    assert_eq!(sample_db().to_json(), sample_db().to_json());
    assert!(sample_db().to_json().starts_with("{\"version\":1,\"next_page\":5,\"next_visit\":5,\"pages\":[{\"id\":1,"));
}

#[test]
fn json_rejects_bad_version() {
    assert_eq!(HistoryDb::from_json("{\"version\":2,\"next_page\":1,\"next_visit\":1,\"pages\":[]}").err(), Some(HistoryError::Version(2)));
    assert!(matches!(HistoryDb::from_json("not json"), Err(HistoryError::Syntax(_))));
    assert!(matches!(HistoryDb::from_json("{\"pages\":[]}"), Err(HistoryError::Syntax(_))));
    assert!(matches!(HistoryDb::from_json("{\"version\":1,\"pages\":5}"), Err(HistoryError::Syntax(_))));
    let bad_code = "{\"version\":1,\"next_page\":2,\"next_visit\":2,\"pages\":[{\"id\":1,\"url\":\"https://a.test/\",\"title\":null,\"visit_count\":1,\"typed_count\":0,\"last_visit_us\":1,\"hidden\":false,\"frecency\":0,\"visits\":[{\"id\":1,\"from\":null,\"time_us\":1,\"transition\":42,\"source\":0,\"interesting\":false}]}]}";
    assert!(matches!(HistoryDb::from_json(bad_code), Err(HistoryError::Syntax(_))));
}

#[test]
fn a_credential_in_the_address_never_reaches_the_store() {
    let mut db = HistoryDb::new();
    let (p1, _) = db.record_visit(nv("https://app.test/cb?code=SECRET1&state=s#frag", at(D, 1), Transition::Link)).unwrap();
    let (p2, _) = db.record_visit(nv("https://app.test/cb?code=SECRET2&state=t", at(D, 2), Transition::Link)).unwrap();
    let (p3, _) = db.record_visit(nv("https://app.test/cb#access_token=SECRET3&token_type=bearer", at(D, 3), Transition::Link)).unwrap();
    // Two logins to the same place are one page.
    assert_eq!((p1, p2, p3), (1, 1, 1));
    assert_eq!(db.len(), 1);
    let page = db.page("https://app.test/cb").unwrap();
    assert_eq!((page.url.as_str(), page.visit_count), ("https://app.test/cb", 3));
    // Lookups by the raw address find the same page; title and removal go through it too.
    assert!(db.page("https://app.test/cb?code=OTHER").is_some());
    db.set_title("https://app.test/cb?code=SECRET4", "Signed in");
    assert_eq!(db.page("https://app.test/cb").unwrap().title.as_deref(), Some("Signed in"));
    let json = db.to_json();
    for s in ["SECRET", "code=", "access_token", "state=", "frag"] {
        assert!(!json.contains(s), "{s}: {json}");
    }
    assert!(db.remove_url("https://app.test/cb?code=zzz"));
    assert!(db.is_empty());
}

#[test]
fn an_old_file_with_credentials_is_cleaned_as_it_loads() {
    let page = |id: u64, url: &str| {
        format!("{{\"id\":{id},\"url\":\"{url}\",\"title\":null,\"visit_count\":1,\"typed_count\":0,\"last_visit_us\":1,\"hidden\":false,\"frecency\":0,\"visits\":[{{\"id\":{id},\"from\":null,\"time_us\":1,\"transition\":1,\"source\":0,\"interesting\":false}}]}}")
    };
    let text = format!(
        "{{\"version\":1,\"next_page\":4,\"next_visit\":4,\"pages\":[{},{},{}]}}",
        page(1, "https://a.test/cb?code=SECRET1"),
        page(2, "https://a.test/cb?code=SECRET2"),
        page(3, "https://b.test/#access_token=SECRET3"),
    );
    let db = HistoryDb::from_json(&text).unwrap();
    assert_eq!(db.len(), 2);
    assert!(db.page("https://a.test/cb").is_some() && db.page("https://b.test/").is_some());
    let out = db.to_json();
    assert!(!out.contains("SECRET") && !out.contains("code="), "{out}");
    // A real duplicate id is still an error.
    let dup = format!("{{\"version\":1,\"next_page\":4,\"next_visit\":4,\"pages\":[{},{}]}}", page(1, "https://a.test/"), page(1, "https://c.test/"));
    assert!(matches!(HistoryDb::from_json(&dup), Err(HistoryError::Syntax(_))));
}

/// One address a person really lands on, what history is allowed to keep from it, and what must never reach the file.
/// `keep: None` is an address the store refuses outright.
struct Case {
    url: &'static str,
    keep: Option<&'static str>,
    secrets: &'static [&'static str],
}

/// The addresses of the flows the rule was written for, taken from what the pages actually put in the bar.
const REAL_ADDRESSES: [Case; 18] = [
    // Google sign-in: the path is the flow, so everything after it goes, `continue` and the one-time tokens with it.
    Case {
        url: "https://accounts.google.com/v3/signin/identifier?continue=https%3A%2F%2Fwww.youtube.com%2Fsignin%3Faction_handle_signin%3Dtrue%26app%3Ddesktop%26next%3D%252F&ec=65620&hl=en&flowName=GlifWebSignIn&flowEntry=ServiceLogin&ifkv=AQDQWYt8pRk7mV2xLd9cFh3nBq6sZ&dsh=S1729684809:1729684901234567&TL=BARxyZ2&authuser=0",
        keep: Some("https://accounts.google.com/v3/signin/identifier"),
        secrets: &["ifkv", "dsh", "TL", "continue", "Glif", "65620"],
    },
    Case {
        url: "https://accounts.google.com/v3/signin/challenge/pwd?TL=AM3QGb7pLcD&continue=https%3A%2F%2Faccounts.google.com%2F&flowName=GlifWebSignIn&flowEntry=AssignEmail",
        keep: Some("https://accounts.google.com/v3/signin/challenge/pwd"),
        secrets: &["TL", "continue", "flowName"],
    },
    Case {
        url: "https://accounts.google.com/o/oauth2/auth?client_id=1234567890-abc.apps.googleusercontent.com&response_type=code&scope=openid%20email&redirect_uri=https%3A%2F%2Fapp.example%2Fcb&state=af0ifjsldkj&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
        keep: Some("https://accounts.google.com/o/oauth2/auth"),
        secrets: &["client_id", "state", "code_challenge", "redirect_uri"],
    },
    // The authorization-code callback: the code names itself.
    Case {
        url: "https://app.example.com/callback?code=SplxlOBeZQQYbYS6WxSbIA&state=af0ifjsldkj",
        keep: Some("https://app.example.com/callback"),
        secrets: &["code=", "state=", "Splxl"],
    },
    // GitHub's token exchange, and the same code arriving in the fragment (implicit flow).
    Case {
        url: "https://github.com/login/oauth/access_token?client_id=Iv23li1C&client_secret=EXAMPLE-fake-client-secret&code=1e637a39f4b2&redirect_uri=https%3A%2F%2Fapp.example%2Fcb",
        keep: Some("https://github.com/login/oauth/access_token"),
        secrets: &["client_secret", "EXAMPLE-fake-client", "code=", "redirect_uri"],
    },
    Case {
        url: "https://app.example.com/cb#access_token=EXAMPLE-fake-access-token-0000&scope=repo%2Cuser&token_type=bearer&state=xyz",
        keep: Some("https://app.example.com/cb"),
        secrets: &["access_token", "EXAMPLE-fake-access", "state="],
    },
    // A fragment token on a page with a harmless query: the query stays, the fragment goes.
    Case {
        url: "https://app.example.com/return?view=home#access_token=EXAMPLE-fake-token-0000&token_type=bearer",
        keep: Some("https://app.example.com/return?view=home"),
        secrets: &["access_token", "EXAMPLE-fake-token", "token_type"],
    },
    // The secret name written with a percent-encoded first letter.
    Case {
        url: "https://httpbin.org/get?%61ccess_token=EXAMPLE-fake-token-0000&limit=20",
        keep: Some("https://httpbin.org/get"),
        secrets: &["%61ccess", "EXAMPLE-fake-token", "token"],
    },
    // An API key, and a session id: both names are on the list.
    Case {
        url: "https://www.googleapis.com/language/translate/v2?key=EXAMPLE-not-a-real-key-0000000000000000&q=hello",
        keep: Some("https://www.googleapis.com/language/translate/v2"),
        secrets: &["key=", "EXAMPLE-not-a-real"],
    },
    Case { url: "https://shop.example.com/checkout?sid=7d4f8a2b1e6c9d0f&coupon=SAVE10", keep: Some("https://shop.example.com/checkout"), secrets: &["sid=", "7d4f8a2b"] },
    // `continue` and `next` holding an address that is itself redacted: that parameter alone goes.
    Case {
        url: "https://accounts.google.com/ServiceLogin?continue=https%3A%2F%2Fa.test%2Fp%3Fnext%3Dhttps%253A%252F%252Fcheckout.example%252Fcb%253Fcode%253Dabc123&hl=en",
        keep: Some("https://accounts.google.com/ServiceLogin?hl=en"),
        secrets: &["continue", "abc123"],
    },
    Case {
        url: "https://app.example.com/start?size=large&redirect_uri=https%3A%2F%2Fapp.example.com%2Fcb%3Fcode%3Dabc123&hl=en",
        keep: Some("https://app.example.com/start?size=large&hl=en"),
        secrets: &["redirect_uri", "abc123"],
    },
    // An identity provider's SSO endpoint: the path is the flow, SAMLRequest and RelayState go with it.
    Case {
        url: "https://idp.corp.example/sso/saml?SAMLRequest=nZLLboMwFIT3%2FhUXLzrGJG3TIlVl%2BhGprZ20kjqIEJQ4asmlcVJL37s8ArzqzOyZs6c&RelayState=https%3A%2F%2Fapp.example%2Fcb%3Fcode%3Dz&SigAlg=http%3A%2F%2Fwww.w3.org%2F2001%2F04%2Fxmldsig%23rsa-sha1",
        keep: Some("https://idp.corp.example/sso/saml"),
        secrets: &["SAMLRequest", "RelayState", "SigAlg"],
    },
    // Credentials in the authority are not a page at all: the store refuses the address and keeps nothing.
    Case {
        url: "https://admin:s3cr3tP4ss@reports.internal.example/q1/dashboard?token=TESTVALUE-not-a-credential-0002",
        keep: None,
        secrets: &["s3cr3tP4ss", "admin@", "token="],
    },
    // Addresses that stay exactly as they are: a search, a video, a search engine, a mail client's own route.
    Case {
        url: "https://www.google.com/search?q=lucinanet+browser&oq=lucinanet&hl=en&source=hp&ei=Z0AdGeJ2S&sourceid=chrome&ie=UTF-8",
        keep: Some("https://www.google.com/search?q=lucinanet+browser&oq=lucinanet&hl=en&source=hp&ei=Z0AdGeJ2S&sourceid=chrome&ie=UTF-8"),
        secrets: &[],
    },
    Case { url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ&pp=ygULcHVsbCB1cA%3D%3D", keep: Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ&pp=ygULcHVsbCB1cA%3D%3D"), secrets: &[] },
    Case { url: "https://duckduckgo.com/?q=oauth+token+redaction&ia=web", keep: Some("https://duckduckgo.com/?q=oauth+token+redaction&ia=web"), secrets: &[] },
    Case { url: "https://mail.google.com/mail/u/0/#inbox", keep: Some("https://mail.google.com/mail/u/0/#inbox"), secrets: &[] },
];

/// Every address in the table, through `redact_url` and through the store: what is kept, what is refused, and that
/// nothing secret reaches the file.
#[test]
fn real_addresses_are_recorded_as_the_table_says() {
    for c in REAL_ADDRESSES {
        let mut db = HistoryDb::new();
        let Some(keep) = c.keep else {
            assert!(db.record_visit(nv(c.url, at(D, 1), Transition::Typed)).is_none(), "{} must not be stored", c.url);
            assert!(db.is_empty(), "{} leaves nothing behind", c.url);
            continue;
        };
        assert_eq!(redact_url(c.url), keep, "redact_url({})", c.url);
        assert_eq!(redact_url(keep), keep, "{} records to itself", keep);
        let (pid, _) = db.record_visit(nv(c.url, at(D, 1), Transition::Typed)).expect("a stored visit");
        assert_eq!(db.page_by_id(pid).unwrap().url, keep, "the address stored for {}", c.url);
        // A later lookup by the raw address finds the same page.
        assert_eq!(db.page(c.url).map(|p| p.url.as_str()), Some(keep), "lookup by the raw address {}", c.url);
        let json = db.to_json();
        assert!(json.contains(keep), "{keep} is missing from {json}");
        for s in c.secrets {
            assert!(!json.contains(s), "{s} reached the file for {}: {json}", c.url);
        }
    }
}
