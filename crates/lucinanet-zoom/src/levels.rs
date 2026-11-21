/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The zoom level tables and the Ctrl +/-/0 step rules, for one browser profile's behaviour.
//!
//! The tables are the originals' own, read at the pinned revisions (PORTING.md): Firefox's is the
//! `toolkit.zoomManager.zoomValues` pref cut by `zoom.minPercent`/`zoom.maxPercent`, Chromium's is
//! `kPresetBrowserZoomFactors` written in whole percent. Steps are whole percent too, so the step
//! functions here are plain integer arithmetic on `u16` - no floats and no iterators - which leaves
//! them available to a later Verus check of the properties this module states (the result is always
//! a level, `zoom_in` never goes down, `zoom_out` never goes up, and the step is minimal).

/// A zoom level: whole percent of the page's unzoomed size. 100 is no zoom.
pub type Percent = u16;

/// An unzoomed page, and what `reset` returns.
pub const DEFAULT: Percent = 100;

/// Which browser's table and step rules to follow. LucinaNet matches one of them exactly so a page
/// looks the way it did in that browser at the same keystrokes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Profile {
    /// `toolkit/content/viewZoomOverlay.js` (`ZoomManager`) at 33abb90.
    Firefox,
    /// `third_party/blink/common/page/page_zoom.cc` and `components/zoom/page_zoom.cc` at e9a4673.
    Chromium,
}

// Firefox modules/libpref/init/all.js:3028 @33abb90, the `toolkit.zoomManager.zoomValues` pref:
// ".2,.3,.4,.5,.6,.7,.8,.9,1,1.1,1.2,1.3,1.4,1.5,1.6,1.7,1.8,1.9,2,2.2,2.4,2.6,2.8,3,4,5" in whole
// percent. Sorted, then cut to `zoom.minPercent`..=`zoom.maxPercent` by `ZoomManager`'s pref getter;
// those are 20 and 500 on desktop (modules/libpref/init/StaticPrefList.yaml:22022 and :22013
// @33abb90), so the cut removes nothing and the shipped table starts at 20 %.
const FIREFOX_LEVELS: [Percent; 26] = [
    20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180, 190, 200, 220, 240,
    260, 280, 300, 400, 500,
];

// Chromium third_party/blink/common/page/page_zoom.cc @e9a4673, `kPresetBrowserZoomFactors`
// {0.25, 1/3, 0.5, 2/3, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0} rounded
// to whole percent. The same file's `kMinimumBrowserZoomFactor` 0.25 and `kMaximumBrowserZoomFactor`
// 5.0 are the ends of it, so like Firefox nothing is cut.
const CHROMIUM_LEVELS: [Percent; 17] = [
    25, 33, 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300, 400, 500,
];

/// The profile's level table, ascending and without duplicates.
pub fn levels(profile: Profile) -> &'static [Percent] {
    match profile {
        Profile::Firefox => &FIREFOX_LEVELS,
        Profile::Chromium => &CHROMIUM_LEVELS,
    }
}

/// The lowest level the profile allows: the first of [`levels`].
pub fn min(profile: Profile) -> Percent {
    match profile {
        Profile::Firefox => FIREFOX_LEVELS[0],
        Profile::Chromium => CHROMIUM_LEVELS[0],
    }
}

/// The highest level the profile allows: the last of [`levels`].
pub fn max(profile: Profile) -> Percent {
    match profile {
        Profile::Firefox => FIREFOX_LEVELS[FIREFOX_LEVELS.len() - 1],
        Profile::Chromium => CHROMIUM_LEVELS[CHROMIUM_LEVELS.len() - 1],
    }
}

/// Into `min..=max`, without moving a value that is already inside: what the profile's zoom setter
/// accepts. Chromium clamps the level on set (`content::ClampZoomLevel`), Firefox refuses a value
/// outside `MIN`..=`MAX` (`viewZoomOverlay.js` `setZoomForBrowser`), and `SiteZoom` clamps what it
/// reads back.
pub fn clamp(profile: Profile, current: Percent) -> Percent {
    if current < min(profile) {
        min(profile)
    } else if current > max(profile) {
        max(profile)
    } else {
        current
    }
}

/// The level nearest `current`, ties to the lower level; out-of-range values give the nearer end.
///
/// This is the toolbar's "where are we" helper, not a step: the Firefox steppers use the original's
/// own snap, which sends an exact midpoint to the *higher* level (`firefox_snap` below, and
/// PORTING.md on why the two differ).
pub fn snap(profile: Profile, current: Percent) -> Percent {
    let values = levels(profile);
    let value = clamp(profile, current);
    let mut i = 0;
    while i < values.len() {
        if values[i] >= value {
            // The first level at or above `value`; its predecessor is strictly below it, so both
            // differences are non-negative here.
            if i > 0 && value - values[i - 1] <= values[i] - value {
                return values[i - 1];
            }
            return values[i];
        }
        i += 1;
    }
    // Only reachable if the table were empty; `clamp` already put `value` at or below the last level.
    values[values.len() - 1]
}

/// Firefox `ZoomManager.snap` (toolkit/content/viewZoomOverlay.js @33abb90): the first level at or
/// above the value, back to the predecessor only if that one is strictly nearer - so a value exactly
/// between two levels goes up. Below the table it gives the first level, above it the last.
fn firefox_snap(profile: Profile, current: Percent) -> Percent {
    let values = levels(profile);
    let mut i = 0;
    while i < values.len() {
        if values[i] >= current {
            if i > 0 && current - values[i - 1] < values[i] - current {
                return values[i - 1];
            }
            return values[i];
        }
        i += 1;
    }
    values[values.len() - 1]
}

/// Ctrl +: the smallest level above `current`; at the top it stays at [`max`]. `current` need not be
/// a level - a pinch leaves 137, a level saved under another profile lands here too.
///
/// Firefox's `enlargeForBrowser` steps from the snapped level (`zoomValues.indexOf(snap(zoom)) + 1`),
/// which one level past a value that snapped up; Chromium's `PageZoom::Zoom` takes the first preset
/// strictly above the current level (`std::upper_bound`). Both leave the page at [`max`] once there is
/// no step up, and a value past the top of the table comes back as [`max`] (Chromium clamps on set,
/// Firefox would refuse the value outright).
pub fn zoom_in(profile: Profile, current: Percent) -> Percent {
    let values = levels(profile);
    match profile {
        Profile::Firefox => {
            let snapped = firefox_snap(profile, current);
            let mut i = 0;
            while i + 1 < values.len() {
                if values[i] == snapped {
                    return values[i + 1];
                }
                i += 1;
            }
            values[values.len() - 1]
        }
        Profile::Chromium => {
            let mut i = 0;
            while i < values.len() {
                if values[i] > current {
                    return values[i];
                }
                i += 1;
            }
            values[values.len() - 1]
        }
    }
}

/// Ctrl -: the largest level below `current`; at the bottom it stays at [`min`]. The same rules as
/// [`zoom_in`], mirrored: Firefox steps from the snapped level, Chromium takes the last preset
/// strictly below the current one.
pub fn zoom_out(profile: Profile, current: Percent) -> Percent {
    let values = levels(profile);
    match profile {
        Profile::Firefox => {
            let snapped = firefox_snap(profile, current);
            let mut i = 0;
            while i + 1 < values.len() {
                if values[i + 1] == snapped {
                    return values[i];
                }
                i += 1;
            }
            values[0]
        }
        Profile::Chromium => {
            let mut best = values[0];
            let mut i = 0;
            while i < values.len() {
                if values[i] >= current {
                    break;
                }
                best = values[i];
                i += 1;
            }
            best
        }
    }
}

/// Ctrl 0: the unzoomed page. The wiring calls this through `SiteZoom::default_percent`, which is the
/// user's "default zoom" setting and may itself be something other than 100; `reset` is the unset value.
pub fn reset(_profile: Profile) -> Percent {
    DEFAULT
}

/// The multiplier for `WebView::set_page_zoom`: percent over 100.
pub fn factor(percent: Percent) -> f32 {
    percent as f32 / 100.0
}

/// The level as the toolbar shows it, e.g. `125%`.
pub fn label(percent: Percent) -> String {
    format!("{percent}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pref string of `toolkit.zoomManager.zoomValues` read out of Firefox's
    /// modules/libpref/init/all.js:3028 @33abb90, verbatim.
    const FIREFOX_PREF: &str =
        ".2,.3,.4,.5,.6,.7,.8,.9,1,1.1,1.2,1.3,1.4,1.5,1.6,1.7,1.8,1.9,2,2.2,2.4,2.6,2.8,3,4,5";

    /// That string as whole percent, by integer arithmetic: ".2" is 20, "1.1" is 110, "2" is 200.
    fn pref_percents(text: &str) -> Vec<Percent> {
        let mut out = Vec::new();
        for token in text.split(',') {
            let (whole, frac) = match token.split_once('.') {
                Some((w, f)) => (w, f),
                None => (token, ""),
            };
            let mut percent: Percent = 0;
            for digit in whole.bytes() {
                percent = percent * 10 + (digit - b'0') as Percent;
            }
            percent *= 100;
            // Two decimal digits, short ones scaled up: ".2" -> 20, "1.1" -> 110.
            let mut place: Percent = 10;
            for digit in frac.bytes() {
                percent += (digit - b'0') as Percent * place;
                place /= 10;
            }
            out.push(percent);
        }
        out
    }

    #[test]
    fn firefox_table_is_ascending_and_matches_pref() {
        // The whole expected table, from the pref string above (all.js:3028 @33abb90), and the min
        // and max prefs that would have cut it (StaticPrefList.yaml:22022 minPercent 20,
        // :22013 maxPercent 500) - so nothing is cut.
        let expected: Vec<Percent> = pref_percents(FIREFOX_PREF);
        assert_eq!(expected.len(), 26);
        assert_eq!(levels(Profile::Firefox), expected.as_slice());
        assert_eq!(
            levels(Profile::Firefox),
            [20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180, 190, 200,
             220, 240, 260, 280, 300, 400, 500]
        );
        assert_eq!(min(Profile::Firefox), 20);
        assert_eq!(max(Profile::Firefox), 500);
    }

    #[test]
    fn chromium_table_matches_preset_factors() {
        // kPresetBrowserZoomFactors (blink/common/page/page_zoom.cc @e9a4673) rounded to whole
        // percent: 0.25 25, 1/3 33.33->33, 0.5 50, 2/3 66.67->67, 0.75 75, 0.8 80, 0.9 90, 1.0 100,
        // 1.1 110, 1.25 125, 1.5 150, 1.75 175, 2.0 200, 2.5 250, 3.0 300, 4.0 400, 5.0 500.
        assert_eq!(
            levels(Profile::Chromium),
            [25, 33, 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300, 400, 500]
        );
        assert_eq!(min(Profile::Chromium), 25); // kMinimumBrowserZoomFactor 0.25
        assert_eq!(max(Profile::Chromium), 500); // kMaximumBrowserZoomFactor 5.0
    }

    #[test]
    fn tables_are_ascending_without_duplicates() {
        for profile in [Profile::Firefox, Profile::Chromium] {
            let values = levels(profile);
            assert!(!values.is_empty());
            let mut i = 1;
            while i < values.len() {
                assert!(values[i - 1] < values[i], "{profile:?} not ascending at {i}");
                i += 1;
            }
        }
    }

    #[test]
    fn zoom_in_steps_through_every_level_firefox() {
        let values = levels(Profile::Firefox);
        let mut current = min(Profile::Firefox);
        let mut visited = 1;
        while current != max(Profile::Firefox) {
            let next = zoom_in(Profile::Firefox, current);
            assert_eq!(next, values[visited], "Ctrl + from {current}");
            assert!(next > current);
            current = next;
            visited += 1;
        }
        assert_eq!(visited, values.len()); // every level, none skipped
    }

    #[test]
    fn zoom_in_steps_through_every_level_chromium() {
        let values = levels(Profile::Chromium);
        let mut current = min(Profile::Chromium);
        let mut visited = 1;
        while current != max(Profile::Chromium) {
            let next = zoom_in(Profile::Chromium, current);
            assert_eq!(next, values[visited], "Ctrl + from {current}");
            assert!(next > current);
            current = next;
            visited += 1;
        }
        assert_eq!(visited, values.len());
    }

    #[test]
    fn zoom_out_steps_through_every_level_firefox() {
        let values = levels(Profile::Firefox);
        let mut current = max(Profile::Firefox);
        let mut left = values.len() - 1; // the index `current` is at
        while current != min(Profile::Firefox) {
            let next = zoom_out(Profile::Firefox, current);
            assert_eq!(next, values[left - 1], "Ctrl - from {current}");
            assert!(next < current);
            current = next;
            left -= 1;
        }
        assert_eq!(left, 0); // the walk ended on the first level, and skipped none
    }

    #[test]
    fn zoom_out_steps_through_every_level_chromium() {
        let values = levels(Profile::Chromium);
        let mut current = max(Profile::Chromium);
        let mut left = values.len() - 1;
        while current != min(Profile::Chromium) {
            let next = zoom_out(Profile::Chromium, current);
            assert_eq!(next, values[left - 1], "Ctrl - from {current}");
            assert!(next < current);
            current = next;
            left -= 1;
        }
        assert_eq!(left, 0);
    }

    #[test]
    fn zoom_in_stops_at_max() {
        assert_eq!(zoom_in(Profile::Firefox, 500), 500);
        assert_eq!(zoom_in(Profile::Chromium, 500), 500);
        // Past the table there is no legal level left to go to; the top of it is the answer.
        assert_eq!(zoom_in(Profile::Firefox, 600), 500);
        assert_eq!(zoom_in(Profile::Chromium, 9999), 500);
    }

    #[test]
    fn zoom_out_stops_at_min() {
        assert_eq!(zoom_out(Profile::Firefox, 20), 20);
        assert_eq!(zoom_out(Profile::Chromium, 25), 25);
        // Below the table, the bottom of it, which is where a further step down would end up.
        assert_eq!(zoom_out(Profile::Firefox, 10), 20);
        assert_eq!(zoom_out(Profile::Chromium, 1), 25);
    }

    #[test]
    fn zoom_in_from_off_table_value() {
        // A pinch leaves 137 %. Firefox: snap(137) - the first level at or above it is 140, and the
        // predecessor 130 is not nearer (7 against 3) - then one step up from 140, so 150.
        assert_eq!(zoom_in(Profile::Firefox, 137), 150);
        // Chromium: the first preset strictly above 137, which is 150 (125 is below it).
        assert_eq!(zoom_in(Profile::Chromium, 137), 150);
        // 95 %: Firefox snaps up to 100 and steps to 110; Chromium's next preset above 95 is 100.
        assert_eq!(zoom_in(Profile::Firefox, 95), 110);
        assert_eq!(zoom_in(Profile::Chromium, 95), 100);
    }

    #[test]
    fn zoom_out_from_off_table_value() {
        // 137 %: Firefox steps down from the snapped 140, so 130; Chromium takes the last preset
        // strictly below 137, so 125.
        assert_eq!(zoom_out(Profile::Firefox, 137), 130);
        assert_eq!(zoom_out(Profile::Chromium, 137), 125);
        // Chromium's 1/3 and 2/3 rows: 60 % steps down to 50 (67 is above it), and up to 67.
        assert_eq!(zoom_out(Profile::Chromium, 60), 50);
        assert_eq!(zoom_in(Profile::Chromium, 60), 67);
    }

    #[test]
    fn firefox_step_snaps_midpoints_up() {
        // 115 is exactly between 110 and 120. viewZoomOverlay.js keeps the higher one for the snap,
        // so Ctrl + goes one past it to 130 and Ctrl - lands on 110; the plain "strictly above/below"
        // rule of Chromium would answer 120 and 110.
        assert_eq!(firefox_snap(Profile::Firefox, 115), 120);
        assert_eq!(zoom_in(Profile::Firefox, 115), 130);
        assert_eq!(zoom_out(Profile::Firefox, 115), 110);
        assert_eq!(zoom_in(Profile::Chromium, 115), 125);
        assert_eq!(zoom_out(Profile::Chromium, 115), 110);
    }

    #[test]
    fn reset_is_100() {
        assert_eq!(reset(Profile::Firefox), 100);
        assert_eq!(reset(Profile::Chromium), DEFAULT);
        assert_eq!(DEFAULT, 100);
        assert!(levels(Profile::Firefox).contains(&DEFAULT));
        assert!(levels(Profile::Chromium).contains(&DEFAULT));
    }

    #[test]
    fn snap_picks_nearest_and_ties_go_down() {
        // Nearest level: 137 is 3 from 140 and 7 from 130 (Firefox's table).
        assert_eq!(snap(Profile::Firefox, 137), 140);
        assert_eq!(snap(Profile::Firefox, 132), 130);
        // A level is itself: 240.
        assert_eq!(snap(Profile::Firefox, 240), 240);
        // Exact midpoints go down: 25 between 20 and 30, 115 between 110 and 120, 275 between 250
        // and 300 (Chromium).
        assert_eq!(snap(Profile::Firefox, 25), 20);
        assert_eq!(snap(Profile::Firefox, 115), 110);
        assert_eq!(snap(Profile::Chromium, 275), 250);
        assert_eq!(snap(Profile::Chromium, 95), 90);
        // Out of range gives the nearer end of the table.
        assert_eq!(snap(Profile::Firefox, 5), 20);
        assert_eq!(snap(Profile::Chromium, 4000), 500);
    }

    #[test]
    fn clamp_limits_without_snapping() {
        // Inside the range nothing moves, level or not.
        assert_eq!(clamp(Profile::Firefox, 137), 137);
        assert_eq!(clamp(Profile::Chromium, 33), 33);
        assert_eq!(clamp(Profile::Firefox, 20), 20);
        assert_eq!(clamp(Profile::Firefox, 500), 500);
        // Outside it, the nearer end - and no snap on the way.
        assert_eq!(clamp(Profile::Firefox, 19), 20);
        assert_eq!(clamp(Profile::Firefox, 501), 500);
        assert_eq!(clamp(Profile::Chromium, 0), 25);
        assert_eq!(clamp(Profile::Chromium, 9999), 500);
    }

    #[test]
    fn factor_is_percent_over_100() {
        assert_eq!(factor(125), 1.25);
        assert_eq!(factor(100), 1.0);
        assert_eq!(factor(200), 2.0);
        assert_eq!(factor(50), 0.5);
        // 1/3 and the other repeating rows are not exact in binary; what matters is the percent.
        assert!((factor(33) - 0.33).abs() < 1e-6);
        assert!((factor(67) - 0.67).abs() < 1e-6);
    }

    #[test]
    fn label_formats_percent() {
        assert_eq!(label(125), "125%");
        assert_eq!(label(100), "100%");
        assert_eq!(label(20), "20%");
        assert_eq!(label(500), "500%");
    }
}
