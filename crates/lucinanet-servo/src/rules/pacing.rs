/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// Frame pacing in whole microseconds, as the Servo patch's `Pacing` computes it (crates/lucinanet-servo/src/soft.rs).
// Verus checks this very file (scripts/verify-shipped.sh): the `//@` lines hold the proof, and the shipped file
// is this one with the lines removed.
//
// A run is a series of close-together frames (an animation), not a navigation's few frames. The first
// `UNPACED_FRAMES` frames of a run start at once, and after that a frame starts one interval after the last start, the
// interval being `next_frame_us`: `FASTEST_FRAME_US` while input is recent, else at least twice the last paint.
//
// Proved: the run resets to (no wait, run 0) once the frames are farther apart than `RUN_GAP_US`; the first
// `UNPACED_FRAMES` frames of a run never wait; a paced frame never waits longer than the interval and starts exactly
// one interval after the last start; `next_frame_us` is at least `FASTEST_FRAME_US`, exactly it while input is recent,
// and at least twice the last paint otherwise (saturating for a paint over 2^63 us); `input_recent` is false when
// there was no input and cannot overflow for any u64; a new run is `run + 1` saturating, and 0 on reset.

/// Frames this close together count as one run: an animation, not a navigation's few frames.
pub const RUN_GAP_US: u64 = 100000;
/// The first frames of a run start at once; pacing begins after them. A navigation paints two or three
/// frames (the old page's last, the loading state, the new page), and pacing them put the new page's first
/// paint one or two 16 ms slots late on Juno (2026-11-05).
pub const UNPACED_FRAMES: u32 = 3;

/// Animations wait this long after input, so that scrolling paints only what it moved.
pub const INPUT_HOLD_MS: u64 = 300;
/// Frames never come closer together than this, and never slower while input is recent.
pub const FASTEST_FRAME_US: u64 = 16000;

/// How the next frame comes, in microseconds: `FASTEST_FRAME_US` while input is recent, else at least twice the last
/// paint, so at most half the time goes to painting and the timer starts with the paint.
pub fn next_frame_us(input_recent: bool, last_paint_us: u64) -> u64 //@ret
//@ ensures ret >= FASTEST_FRAME_US,
//@     input_recent ==> ret == FASTEST_FRAME_US,
//@     !input_recent && last_paint_us <= u64::MAX / 2 ==> ret >= 2 * last_paint_us,
//@     !input_recent && last_paint_us > u64::MAX / 2 ==> ret == u64::MAX,
{
    if input_recent {
        FASTEST_FRAME_US
    } else {
        FASTEST_FRAME_US.max(last_paint_us.saturating_mul(2))
    }
}

/// Whether input is recent. `last_input_ms` is the milliseconds since `epoch` of the last input, plus one, so zero
/// means no input yet; `now_ms` is the milliseconds since `epoch` now. The test cannot wrap for any u64: input is
/// recent only while `now_ms` is within `INPUT_HOLD_MS` of the stored timestamp.
pub fn input_recent(now_ms: u64, last_input_ms: u64) -> bool //@ret
//@ ensures ret == (last_input_ms != 0 && last_input_ms <= now_ms.saturating_add(1)
//@     && last_input_ms >= now_ms.saturating_sub((INPUT_HOLD_MS - 2) as u64)),
//@     last_input_ms == 0 ==> !ret,
{
    // `now + 1 - last < INPUT_HOLD_MS` with no subtraction that could underflow: `last >= now + 1 - INPUT_HOLD_MS`.
    last_input_ms != 0
        && last_input_ms <= now_ms.saturating_add(1)
        && last_input_ms >= now_ms.saturating_sub(INPUT_HOLD_MS - 2)
}

/// Whether the last frame's start is still within the run gap: a frame this close continues the run.
//@ pub open spec fn spec_in_run(since_last_us: Option<u64>) -> bool {
//@     match since_last_us {
//@         None => false,
//@         Some(elapsed) => elapsed < RUN_GAP_US,
//@     }
//@ }

/// The microseconds since the last frame's start, if there was one within the run gap.
//@ pub open spec fn spec_since_last(since_last_us: Option<u64>) -> u64 {
//@     match since_last_us {
//@         None => 0,
//@         Some(elapsed) => elapsed,
//@     }
//@ }

/// The run after this frame: `run + 1` saturating while the run continues, 0 on reset.
//@ pub open spec fn spec_new_run(since_last_us: Option<u64>, run: u32) -> u32 {
//@     if spec_in_run(since_last_us) { run.saturating_add(1) } else { 0 }
//@ }

/// The wait before this frame starts: nothing before the run is paced, else what is left of the interval,
// counted from the last start.
//@ pub open spec fn spec_wait(since_last_us: Option<u64>, run: u32, next_us: u64) -> u64 {
//@     if spec_in_run(since_last_us) && run.saturating_add(1) >= UNPACED_FRAMES {
//@         next_us.saturating_sub(spec_since_last(since_last_us))
//@     } else {
//@         0
//@     }
//@ }

/// After a frame is asked for: `(wait, new run)`, in microseconds. `since_last_us` is the time since the last
/// frame's start, if there was one; `run`, how many frames in a row came close together before it; `next_us`,
/// [`next_frame_us`]. The wait never overflows: it is at most the interval.
pub fn frame_wait(since_last_us: Option<u64>, run: u32, next_us: u64) -> (u64, u32) //@ret
//@ ensures ret.0 == spec_wait(since_last_us, run, next_us),
//@     ret.1 == spec_new_run(since_last_us, run),
//@     !spec_in_run(since_last_us) ==> ret.0 == 0 && ret.1 == 0,
//@     spec_in_run(since_last_us) ==> ret.1 == run.saturating_add(1),
//@     spec_in_run(since_last_us) && run.saturating_add(1) < UNPACED_FRAMES ==> ret.0 == 0,
//@     spec_in_run(since_last_us) && run.saturating_add(1) >= UNPACED_FRAMES ==> ret.0 <= next_us,
//@     spec_in_run(since_last_us) && run.saturating_add(1) >= UNPACED_FRAMES
//@         && spec_since_last(since_last_us) < next_us ==> spec_since_last(since_last_us) + ret.0 == next_us,
//@     spec_in_run(since_last_us) && run.saturating_add(1) >= UNPACED_FRAMES
//@         && spec_since_last(since_last_us) >= next_us ==> ret.0 == 0,
{
    match since_last_us {
        Some(elapsed) if elapsed < RUN_GAP_US => {
            if run.saturating_add(1) < UNPACED_FRAMES {
                (0, run.saturating_add(1))
            } else {
                (next_us.saturating_sub(elapsed), run.saturating_add(1))
            }
        }
        _ => (0, 0),
    }
}

#[cfg(test)] //@omit
mod tests { //@omit
    use super::*; //@omit

    #[test] //@omit
    fn a_run_resets_when_frames_are_far_apart() { //@omit
        assert_eq!(frame_wait(None, 5, 16000), (0, 0)); //@omit
        assert_eq!(frame_wait(Some(RUN_GAP_US), 5, 16000), (0, 0)); //@omit
        // One microsecond inside the gap: the run continues, and the wait is 0 because the last frame already //@omit
        // ran longer than the interval. //@omit
        assert_eq!(frame_wait(Some(RUN_GAP_US - 1), 2, 16000), (0, 3)); //@omit
    } //@omit

    #[test] //@omit
    fn the_first_frames_of_a_run_start_at_once() { //@omit
        assert_eq!(frame_wait(Some(0), 0, 16000), (0, 1)); //@omit
        assert_eq!(frame_wait(Some(5000), 1, 16000), (0, 2)); //@omit
        // The fourth of a run is paced: 7 ms into a 16 ms interval, wait 9 ms. //@omit
        assert_eq!(frame_wait(Some(7000), 2, 16000), (9000, 3)); //@omit
    } //@omit

    #[test] //@omit
    fn a_paced_frame_starts_one_interval_after_the_last_start() { //@omit
        assert_eq!(frame_wait(Some(7000), 2, 16000), (9000, 3)); // 7000 + 9000 == 16000 //@omit
        assert_eq!(frame_wait(Some(15999), 5, 16000), (1, 6)); // the last microsecond of the interval //@omit
        assert_eq!(frame_wait(Some(20000), 5, 16000), (0, 6)); // the last frame ran long: no wait //@omit
        assert_eq!(frame_wait(Some(0), 5, 16000), (16000, 6)); // the wait is never longer than the interval //@omit
    } //@omit

    #[test] //@omit
    fn the_interval_is_16_ms_while_input_is_recent() { //@omit
        assert_eq!(next_frame_us(true, 0), FASTEST_FRAME_US); //@omit
        assert_eq!(next_frame_us(true, 40000), FASTEST_FRAME_US); //@omit
    } //@omit

    #[test] //@omit
    fn the_interval_is_at_least_twice_the_last_paint() { //@omit
        assert_eq!(next_frame_us(false, 0), FASTEST_FRAME_US); // the 16 ms floor //@omit
        assert_eq!(next_frame_us(false, 8000), FASTEST_FRAME_US); //@omit
        assert_eq!(next_frame_us(false, 8001), 16002); //@omit
        assert_eq!(next_frame_us(false, 20000), 40000); //@omit
    } //@omit

    #[test] //@omit
    fn the_interval_saturates_for_a_huge_paint() { //@omit
        assert_eq!(next_frame_us(false, u64::MAX / 2), u64::MAX / 2 * 2); //@omit
        assert_eq!(next_frame_us(false, u64::MAX / 2 + 1), u64::MAX); //@omit
        assert_eq!(next_frame_us(false, u64::MAX), u64::MAX); //@omit
    } //@omit

    #[test] //@omit
    fn input_recent_needs_input_and_a_recent_timestamp() { //@omit
        assert_eq!(input_recent(1000, 0), false); // no input //@omit
        assert_eq!(input_recent(1000, 1001), true); // 1 ms ago: stored as the input's millisecond plus one //@omit
        assert_eq!(input_recent(1000, 702), true); // 299 ms ago: within the hold //@omit
        assert_eq!(input_recent(1000, 701), false); // 300 ms ago: the hold is over //@omit
        assert_eq!(input_recent(0, 5), false); // a timestamp from the future: not recent, no overflow //@omit
        assert_eq!(input_recent(u64::MAX, 1), false); // the clock rolled past it: no overflow //@omit
        assert_eq!(input_recent(u64::MAX, u64::MAX - 298), true); // the last millisecond of the hold //@omit
        assert_eq!(input_recent(u64::MAX, u64::MAX - 299), false); //@omit
    } //@omit

    #[test] //@omit
    fn the_run_count_saturates() { //@omit
        assert_eq!(frame_wait(Some(0), u32::MAX, 16000), (16000, u32::MAX)); //@omit
    } //@omit
} //@omit
