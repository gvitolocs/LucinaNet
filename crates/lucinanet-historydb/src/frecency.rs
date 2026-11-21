/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Frecency as Firefox computes it today: a half-life decayed, weighted mean of the newest visits, stored as the day number
//! after which the score would drop to 1. Ported from the SQL's meaning, not its text.

use crate::db::{Source, Transition, Visit, VisitId};

const DAY_US: i64 = 86_400_000_000;

/// Weights and sampling of `places.frecency.pages.*`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrecencyParams {
    pub half_life_days: u32,
    pub num_sampled_visits: usize,
    pub very_high: u32,
    pub high: u32,
    pub medium: u32,
    pub low: u32,
}

impl Default for FrecencyParams {
    fn default() -> Self {
        // Firefox modules/libpref/init/StaticPrefList.yaml places.frecency.pages.* @33abb90
        FrecencyParams { half_life_days: 30, num_sampled_visits: 10, very_high: 200, high: 100, medium: 50, low: 20 }
    }
}

/// Lookups the formula needs about visits that may live on other pages (a redirect source and its target are different pages).
pub(crate) struct Links<'a> {
    pub type_of: &'a dyn Fn(VisitId) -> Option<Transition>,
    pub has_redirect_child: &'a dyn Fn(VisitId) -> bool,
}

fn is_redirect_type(t: Transition) -> bool {
    matches!(t, Transition::RedirectPermanent | Transition::RedirectTemporary)
}

/// Score of one page. `visits` are the page's own visits; `from` links and redirect children are looked up inside `visits`
/// only (the database uses [`frecency_linked`] to look across pages). A page without countable samples scores from its
/// bookmark date when it has one, else 0.
pub fn frecency(
    visits: &[Visit],
    visit_count: u32,
    bookmarked_at_us: Option<i64>,
    is_redirect: bool,
    p: &FrecencyParams,
) -> i64 {
    let type_of = |id: VisitId| visits.iter().find(|v| v.id == id).map(|v| v.transition);
    let has_child = |id: VisitId| visits.iter().any(|v| v.from == Some(id) && is_redirect_type(v.transition));
    frecency_linked(visits, visit_count, bookmarked_at_us, is_redirect, p, &Links { type_of: &type_of, has_redirect_child: &has_child })
}

/// Frecency of a URL: `place:` queries always score 0, everything else is [`frecency`].
pub fn url_frecency(
    url: &str,
    visits: &[Visit],
    visit_count: u32,
    bookmarked_at_us: Option<i64>,
    is_redirect: bool,
    p: &FrecencyParams,
) -> i64 {
    if url.starts_with("place:") { 0 } else { frecency(visits, visit_count, bookmarked_at_us, is_redirect, p) }
}

pub(crate) fn frecency_linked(
    visits: &[Visit],
    visit_count: u32,
    bookmarked_at_us: Option<i64>,
    is_redirect: bool,
    p: &FrecencyParams,
    links: &Links<'_>,
) -> i64 {
    // Firefox toolkit/components/places/SQLFunctions.cpp CalculateFrecencyFunction::BuildFrecencySQL @33abb90
    let mut recent: Vec<&Visit> = visits
        .iter()
        // Downloads, framed links and reloads are not sampled.
        .filter(|v| !matches!(v.transition, Transition::Download | Transition::FramedLink | Transition::Reload))
        .collect();
    recent.sort_by_key(|v| std::cmp::Reverse(v.time_us));
    recent.truncate(p.num_sampled_visits);

    let mut samples: Vec<(i64, u32)> = recent
        .iter()
        .map(|v| (v.time_us / DAY_US, weight(v, is_redirect, p, links)))
        .collect();
    if samples.is_empty() {
        match bookmarked_at_us {
            Some(t) => samples.push((t / DAY_US, p.high)),
            None => return 0,
        }
    }
    let reference = samples.iter().map(|s| s.0).max().unwrap_or(0);
    let lambda = std::f64::consts::LN_2 / f64::from(p.half_life_days.max(1));
    let n = samples.len();
    let sum: f64 = samples.iter().map(|&(d, w)| f64::from(w) * (-lambda * (reference - d) as f64).exp()).sum();
    let score = sum / n as f64 * f64::from(visit_count).max(n as f64);
    // CAST(... AS INTEGER) truncates toward zero, as `as i64` does.
    reference + (score.ln() / lambda) as i64
}

fn weight(v: &Visit, is_redirect: bool, p: &FrecencyParams, links: &Links<'_>) -> u32 {
    // A redirect target counts as the visit it came from.
    let effective = match (v.transition, v.from) {
        (t, Some(f)) if is_redirect_type(t) => (links.type_of)(f).unwrap_or(t),
        (t, _) => t,
    };
    let plain = !(links.has_redirect_child)(v.id) && !is_redirect;
    let lifted = effective == Transition::Bookmark
        || v.source == Source::Bookmark
        || (effective == Transition::Typed && !matches!(v.source, Source::Search | Source::Sponsored) && plain);
    if lifted {
        if v.interesting { p.very_high } else { p.high }
    } else if plain
        && !matches!(effective, Transition::Embed | Transition::FramedLink | Transition::Reload)
        && v.source != Source::Sponsored
    {
        if v.interesting { p.high } else { p.medium }
    } else {
        p.low
    }
}
