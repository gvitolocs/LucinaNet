/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// The A/V clock: where a player's sound is at the speaker, which the picture follows (audio is master, as in GStreamer's pipeline
// clock from the audio sink and in the Video app's `lucina_video::Clock`). Verus checks this very file (scripts/verify-shipped.sh).
//
// Juno's sound device takes one program and one stream, so LucinaNet mixes every player and AudioContext into it. Each mix block
// of `len` device frames carries up to `len` of a stream's frames, first in the block, then silence for it. The device says how many
// frames it has played; a stream's frames at the speaker follow from the block that position falls in.
//
// Proved: the stream's position never leaves the block's span; before the block it is the block's start, past the stream's part of it
// the end; it never goes back as the device plays on (monotonic in `played`); time from frames is exact integer division and does not
// overflow for any stream shorter than five years at 48 kHz, at any rate from 8 to 192 kHz.

/// Frames above this are refused as a clock: 2^43, over five years at 48 kHz, and 2^43 * 10^6 fits in a u64.
pub const MAX_FRAMES: u64 = 8796093022208;
/// The lowest and highest rates the mixer is ever told of.
pub const MIN_RATE: u64 = 8000;
pub const MAX_RATE: u64 = 192000;
/// Presentation times are kept under this, about 290 years in microseconds, so a sum of two never overflows an i64.
pub const MAX_PTS: i64 = 9223372036854775;

//@ pub open spec fn spec_frames_at(played: u64, dev_start: u64, stream_start: u64, taken: u64) -> u64 {
//@     if played <= dev_start { stream_start }
//@     else if played - dev_start >= taken { (stream_start + taken) as u64 }
//@     else { (stream_start + (played - dev_start)) as u64 }
//@ }

/// The stream's frames at the speaker when the device has played `played` frames, inside a block that starts at device frame
/// `dev_start` and carried the stream's frames from `stream_start`, `taken` of them.
pub fn frames_at(played: u64, dev_start: u64, stream_start: u64, taken: u64) -> u64 //@ret
//@ requires stream_start + taken <= u64::MAX,
//@ ensures ret == spec_frames_at(played, dev_start, stream_start, taken),
//@     stream_start <= ret, ret <= stream_start + taken,
//@     played <= dev_start ==> ret == stream_start,
//@     played >= dev_start + taken ==> ret == stream_start + taken,
{
    if played <= dev_start {
        stream_start
    } else if played - dev_start >= taken {
        stream_start + taken
    } else {
        stream_start + (played - dev_start)
    }
}

//@ proof fn frames_at_monotonic(p1: u64, p2: u64, dev_start: u64, stream_start: u64, taken: u64)
//@     requires p1 <= p2, stream_start + taken <= u64::MAX,
//@     ensures spec_frames_at(p1, dev_start, stream_start, taken) <= spec_frames_at(p2, dev_start, stream_start, taken),
//@ {}

/// Microseconds in `frames` at `rate`.
pub fn frames_to_us(frames: u64, rate: u64) -> i64 //@ret
//@ requires frames <= MAX_FRAMES, MIN_RATE <= rate, rate <= MAX_RATE,
//@ ensures ret as int == (frames as int * 1000000) / (rate as int), 0 <= ret, ret <= MAX_PTS,
{
    //@ assert(frames as int * 1000000 <= 8796093022208000000) by(nonlinear_arith) requires frames <= MAX_FRAMES;
    //@ assert((frames as int * 1000000) / (rate as int) <= (frames as int * 1000000) / 8000) by(nonlinear_arith) requires rate >= 8000, frames >= 0;
    //@ assert((frames as int * 1000000) / 8000 <= 1099511627776000) by(nonlinear_arith) requires frames <= MAX_FRAMES;
    ((frames * 1000000) / rate) as i64
}

/// The presentation time a stream has reached: where it started plus what has played of it.
pub fn position_us(start_pts: i64, frames: u64, rate: u64) -> i64 //@ret
//@ requires -MAX_PTS <= start_pts, start_pts <= MAX_PTS, frames <= MAX_FRAMES, MIN_RATE <= rate, rate <= MAX_RATE,
//@ ensures ret >= start_pts, ret as int == start_pts as int + (frames as int * 1000000) / (rate as int),
{
    start_pts + frames_to_us(frames, rate)
}
