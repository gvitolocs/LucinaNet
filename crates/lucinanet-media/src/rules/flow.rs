/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// When the byte cache takes what script pushes, when it says it has enough, and when it asks for more. Verus checks this very file:
// `//@` lines hold the proof (scripts/verify-shipped.sh).
//
// The original is servo-media's GStreamer backend: appsrc's `enough-data` makes `push_data` answer `PlayerError::EnoughData` and
// sends `PlayerEvent::EnoughData`, after which Servo cancels a seekable fetch ("Backoff"); `need-data` sends `PlayerEvent::NeedData`.
// Here the high and low marks are bytes cached ahead of the slowest reader inside the run the fetch is filling.
//
// Proved: the two signals alternate (no `Enough` twice without a `Need` between, and the reverse); a push is taken only while filling;
// filling turns to full exactly when the bytes ahead reach the high mark, and full turns back exactly when they fall to the low mark;
// a read is served by the running fetch only between where it is and `SLACK` past it; no sum overflows.

/// Bytes ahead of the slowest reader at which the cache stops taking pushes. About 30 s of 1080p H.264 at the server's 2.7 Mbit/s.
pub const HIGH: u64 = 12582912;
/// Bytes ahead at which it asks for more again.
pub const LOW: u64 = 4194304;
/// A read this far past where the fetch has got waits for it, rather than asking for a new one there.
pub const SLACK: u64 = 1048576;

/// Whether the cache takes pushes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum Flow {
    Filling,
    Full,
}

/// What a step tells script, if anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum Signal {
    Quiet,
    /// `PlayerEvent::EnoughData`, and `PlayerError::EnoughData` for the push that found the cache full.
    Enough,
    /// Ask for more: `PlayerEvent::NeedData`, or a new fetch from where the old one stopped.
    Need,
}

/// Whether a push of bytes is taken.
pub fn takes_push(flow: Flow) -> bool //@ret
//@ ensures ret == (flow == Flow::Filling),
{
    match flow {
        Flow::Filling => true,
        Flow::Full => false,
    }
}

/// After a push: `ahead` is what is cached ahead of the slowest reader now.
pub fn after_push(flow: Flow, ahead: u64) -> (Flow, Signal) //@ret
//@ ensures
//@     flow == Flow::Filling && ahead >= HIGH ==> ret.0 == Flow::Full && ret.1 == Signal::Enough,
//@     flow == Flow::Filling && ahead < HIGH ==> ret.0 == Flow::Filling && ret.1 == Signal::Quiet,
//@     flow == Flow::Full ==> ret.0 == Flow::Full && ret.1 == Signal::Quiet,
//@     ret.1 == Signal::Enough ==> flow == Flow::Filling && ret.0 == Flow::Full,
//@     ret.1 != Signal::Need,
{
    match flow {
        Flow::Filling => {
            if ahead >= HIGH {
                (Flow::Full, Signal::Enough)
            } else {
                (Flow::Filling, Signal::Quiet)
            }
        },
        Flow::Full => (Flow::Full, Signal::Quiet),
    }
}

/// After a read or an eviction: `ahead` as for [`after_push`].
pub fn after_read(flow: Flow, ahead: u64) -> (Flow, Signal) //@ret
//@ ensures
//@     flow == Flow::Full && ahead <= LOW ==> ret.0 == Flow::Filling && ret.1 == Signal::Need,
//@     flow == Flow::Full && ahead > LOW ==> ret.0 == Flow::Full && ret.1 == Signal::Quiet,
//@     flow == Flow::Filling ==> ret.0 == Flow::Filling && ret.1 == Signal::Quiet,
//@     ret.1 == Signal::Need ==> flow == Flow::Full && ret.0 == Flow::Filling,
//@     ret.1 != Signal::Enough,
{
    match flow {
        Flow::Full => {
            if ahead <= LOW {
                (Flow::Filling, Signal::Need)
            } else {
                (Flow::Full, Signal::Quiet)
            }
        },
        Flow::Filling => (Flow::Filling, Signal::Quiet),
    }
}

/// Whether a read at `pos`, which the cache does not hold, is for the fetch now writing at `fetch_at` to bring.
pub fn served_by_fetch(pos: u64, fetch_at: u64) -> bool //@ret
//@ ensures ret == (fetch_at <= pos && pos - fetch_at <= SLACK),
{
    fetch_at <= pos && pos - fetch_at <= SLACK
}

/// The bytes of a push that are new: those before `next_cached`, the start of the next cached run after `fetch_at` (or `u64::MAX`).
pub fn new_bytes(fetch_at: u64, len: u64, next_cached: u64) -> u64 //@ret
//@ requires fetch_at <= next_cached,
//@ ensures ret <= len, ret <= next_cached - fetch_at,
//@     fetch_at + len <= next_cached ==> ret == len,
{
    let room = next_cached - fetch_at;
    if len <= room { len } else { room }
}

// Alternation follows from the contracts above: `Enough` leaves the flow `Full`, from which only `after_read` can signal, and only
// `Need`; `Need` leaves it `Filling`, from which only `after_push` can signal, and only `Enough`.
