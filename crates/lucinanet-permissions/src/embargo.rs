/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Chromium's embargo: repeated dismissed or ignored prompts make the browser stop asking for a while.
//! Counters belong to one (origin, feature) and persist; they are cleared only by an explicit allow or deny.

/// Chromium components/permissions/permission_decision_auto_blocker.cc @e9a4673: `kDefaultDismissalsBeforeBlock`.
pub(crate) const DISMISSALS_BEFORE_BLOCK: u32 = 3;
/// Chromium permission_decision_auto_blocker.cc @e9a4673: `kDefaultIgnoresBeforeBlock`.
pub(crate) const IGNORES_BEFORE_BLOCK: u32 = 4;
/// Chromium permission_decision_auto_blocker.cc @e9a4673: `kDefaultEmbargoDays`.
pub(crate) const EMBARGO_DAYS: i64 = 7;
const DAY_US: i64 = 86_400_000_000;

/// Prompt history of one (origin, feature).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counters {
    pub dismissals: u32,
    pub ignores: u32,
    /// End of the embargo in force or last in force (microseconds since the epoch).
    pub until: Option<i64>,
}

impl Counters {
    pub fn is_empty(&self) -> bool {
        self.dismissals == 0 && self.ignores == 0 && self.until.is_none()
    }

    /// The embargo end if it lies in the future of `now_us`.
    pub fn active_until(&self, now_us: i64) -> Option<i64> {
        self.until.filter(|&t| now_us < t)
    }

    /// A prompt was dismissed. Like Chromium, the count is not reset when the embargo starts: once it has reached the
    /// limit, every further dismissal (after the embargo ran out) starts a new one.
    pub fn dismissed(&mut self, now_us: i64) {
        self.dismissals = self.dismissals.saturating_add(1);
        if self.dismissals >= DISMISSALS_BEFORE_BLOCK {
            self.start(now_us);
        }
    }

    /// A prompt went unanswered (tab closed or navigated away). Same persistence as [`Counters::dismissed`].
    pub fn ignored(&mut self, now_us: i64) {
        self.ignores = self.ignores.saturating_add(1);
        if self.ignores >= IGNORES_BEFORE_BLOCK {
            self.start(now_us);
        }
    }

    fn start(&mut self, now_us: i64) {
        // Chromium keeps one start time per cause, each lasting seven days; "under embargo" is true while any is in
        // force, which is the later end.
        let end = now_us.saturating_add(EMBARGO_DAYS * DAY_US);
        self.until = Some(self.until.map_or(end, |u| u.max(end)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_start_embargo_at_the_limits() {
        let mut c = Counters::default();
        c.dismissed(10);
        c.dismissed(20);
        assert_eq!(c.active_until(30), None);
        c.dismissed(30);
        assert_eq!(c.active_until(31), Some(30 + 7 * DAY_US));
        let mut i = Counters::default();
        for t in 0..3 {
            i.ignored(t);
        }
        assert!(i.until.is_none());
        i.ignored(3);
        assert_eq!(i.until, Some(3 + 7 * DAY_US));
    }

    #[test]
    fn counters_saturate_at_the_end_of_time() {
        let mut c = Counters { dismissals: u32::MAX, ignores: u32::MAX, until: None };
        c.dismissed(i64::MAX);
        c.ignored(i64::MAX);
        assert_eq!(c.until, Some(i64::MAX));
        assert_eq!(c.dismissals, u32::MAX);
    }
}
