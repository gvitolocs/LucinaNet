/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// A SourceBuffer's buffered ranges and its eviction (mse.rs), as the MSE specification has them (3.5.8 and the `buffered`
// attribute): each track buffer's frames as presentation ranges, the SourceBuffer's ranges as the intersection of its tracks', and
// coded frame eviction that takes only what lies well behind the playback position. Verus checks this very file
// (scripts/verify-shipped.sh).
//
// Proved:
// - `join`: a track's ranges are normalised as a TimeRanges must be (each range not empty, in order, none overlapping or touching);
//   every frame lies inside one of them; the first starts at the first frame (gaps shorter than the next frame are joined, Chromium's
//   fudge). No overflow, for any time under about 290 years (`span` clamps what a stream says to that).
// - `intersect`: normalised in, normalised out, and a time is in the result exactly when it is in both: so `buffered` after any
//   append, removal or eviction is what both tracks hold, never more.
// - `fits`: the quota check without overflow. `evict_before` and `evict_cut`: eviction ends at a random access point, the last one
//   more than ten seconds before the playback position, never at or after it.
//
// Not proved: that the spans given to `join` are the track buffer's frames (mse.rs builds them from its BTreeMap and sorts them),
// the byte count of a track buffer (mse.rs adds and subtracts each frame's length), and which frames depend on a removed one.

/// Presentation times a stream gives are clamped to this, about 290 years in microseconds (as rules/clock.rs).
pub const MAX_PTS: i64 = 9223372036854775;
/// A frame's end is under this: a start and a duration, each at most MAX_PTS.
pub const MAX_END: i64 = 18446744073709550;

//@ pub open spec fn wf(r: Seq<(i64, i64)>) -> bool {
//@     &&& forall|k: int| 0 <= k < r.len() ==> (#[trigger] r[k]).0 < r[k].1
//@     &&& forall|k: int, m: int| 0 <= k < m < r.len() ==> (#[trigger] r[k]).1 < (#[trigger] r[m]).0
//@ }

//@ pub open spec fn covers(r: Seq<(i64, i64)>, t: int) -> bool {
//@     exists|k: int| 0 <= k < r.len() && (#[trigger] r[k]).0 <= t && t < r[k].1
//@ }

//@ pub open spec fn inside(r: Seq<(i64, i64)>, s: (i64, i64)) -> bool {
//@     exists|m: int| 0 <= m < r.len() && (#[trigger] r[m]).0 <= s.0 && s.1 <= r[m].1
//@ }

//@ pub open spec fn hi(x: i64, y: i64) -> int {
//@     if x > y { x as int } else { y as int }
//@ }

/// A frame's presentation span: from its time for its duration, at least one microsecond.
pub fn span(pts: i64, duration: i64) -> (i64, i64) //@ret
//@ ensures
//@     -MAX_PTS <= ret.0, ret.0 < ret.1, ret.1 <= MAX_END,
//@     -MAX_PTS <= pts <= MAX_PTS && 1 <= duration <= MAX_PTS ==> ret.0 == pts && ret.1 == pts + duration,
//@     -MAX_PTS <= pts <= MAX_PTS && duration < 1 ==> ret.0 == pts && ret.1 == pts + 1,
{
    let s = if pts < -MAX_PTS { -MAX_PTS } else if pts > MAX_PTS { MAX_PTS } else { pts };
    let d = if duration < 1 { 1 } else if duration > MAX_PTS { MAX_PTS } else { duration };
    (s, s + d)
}

//@ proof fn join_witnesses(spans: Seq<(i64, i64)>, out: Seq<(i64, i64)>, owner: Seq<int>)
//@     requires
//@         owner.len() == spans.len(),
//@         forall|x: int| 0 <= x < spans.len() ==> 0 <= #[trigger] owner[x] < out.len() && out[owner[x]].0 <= spans[x].0 && spans[x].1 <= out[owner[x]].1,
//@     ensures
//@         forall|k: int| 0 <= k < spans.len() ==> inside(out, #[trigger] spans[k]),
//@ {
//@     assert forall|k: int| 0 <= k < spans.len() implies inside(out, #[trigger] spans[k]) by {
//@         let m = owner[k];
//@         assert(out[m].0 <= spans[k].0 && spans[k].1 <= out[m].1);
//@     }
//@ }

/// A track's buffered ranges from its frames' spans, sorted by start: spans that overlap, or have a gap before them shorter than
/// themselves, join the range before.
pub fn join(spans: &Vec<(i64, i64)>) -> Vec<(i64, i64)> //@ret
//@ requires
//@     forall|k: int| 0 <= k < spans@.len() ==> -MAX_PTS <= (#[trigger] spans@[k]).0 && spans@[k].0 < spans@[k].1 && spans@[k].1 <= MAX_END,
//@     forall|k: int, m: int| 0 <= k < m < spans@.len() ==> (#[trigger] spans@[k]).0 <= (#[trigger] spans@[m]).0,
//@ ensures
//@     wf(ret@),
//@     forall|k: int| 0 <= k < spans@.len() ==> inside(ret@, #[trigger] spans@[k]),
//@     forall|m: int| 0 <= m < ret@.len() ==> -MAX_PTS <= (#[trigger] ret@[m]).0 && ret@[m].1 <= MAX_END,
//@     spans@.len() > 0 ==> ret@.len() > 0 && ret@[0].0 == spans@[0].0,
{
    let mut out: Vec<(i64, i64)> = Vec::new();
    //@ let ghost mut owner: Seq<int> = Seq::empty();
    let mut k: usize = 0;
    while k < spans.len()
        //@ invariant
        //@     k <= spans@.len(), wf(out@), owner.len() == k,
        //@     forall|x: int| 0 <= x < spans@.len() ==> -MAX_PTS <= (#[trigger] spans@[x]).0 && spans@[x].0 < spans@[x].1 && spans@[x].1 <= MAX_END,
        //@     forall|x: int, y: int| 0 <= x < y < spans@.len() ==> (#[trigger] spans@[x]).0 <= (#[trigger] spans@[y]).0,
        //@     forall|x: int| 0 <= x < k ==> 0 <= #[trigger] owner[x] < out@.len() && out@[owner[x]].0 <= spans@[x].0 && spans@[x].1 <= out@[owner[x]].1,
        //@     forall|m: int| 0 <= m < out@.len() ==> -MAX_PTS <= (#[trigger] out@[m]).0 && out@[m].1 <= MAX_END,
        //@     k > 0 ==> out@.len() > 0 && out@[0].0 == spans@[0].0,
        //@     k == 0 ==> out@.len() == 0,
        //@     k < spans@.len() && out@.len() > 0 ==> out@[out@.len() - 1].0 <= spans@[k as int].0,
        //@ decreases spans@.len() - k,
    {
        let s = spans[k].0;
        let e = spans[k].1;
        let n = out.len();
        //@ let ghost out0 = out@;
        if n > 0 && s <= out[n - 1].1 + (e - s) {
            let ls = out[n - 1].0;
            let le = out[n - 1].1;
            out.pop();
            out.push((ls, if e > le { e } else { le }));
            //@ proof {
            //@     owner = owner.push(n - 1);
            //@     assert(out@[n - 1] == (ls, if e > le { e } else { le }));
            //@     assert forall|m: int| 0 <= m < n - 1 implies out@[m] == out0[m] by {}
            //@     assert forall|x: int| 0 <= x < k + 1 implies 0 <= #[trigger] owner[x] < out@.len() && out@[owner[x]].0 <= spans@[x].0 && spans@[x].1 <= out@[owner[x]].1 by {
            //@         if x < k { assert(out0[owner[x]].0 <= spans@[x].0); }
            //@     }
            //@     assert forall|a: int, b: int| 0 <= a < b < out@.len() implies (#[trigger] out@[a]).1 < (#[trigger] out@[b]).0 by {
            //@         assert(out0[a].1 < out0[b].0);
            //@     }
            //@     if k + 1 < spans@.len() { assert(spans@[k as int].0 <= spans@[k + 1].0); }
            //@ }
        } else {
            out.push((s, e));
            //@ proof {
            //@     owner = owner.push(n as int);
            //@     assert forall|x: int| 0 <= x < k + 1 implies 0 <= #[trigger] owner[x] < out@.len() && out@[owner[x]].0 <= spans@[x].0 && spans@[x].1 <= out@[owner[x]].1 by {
            //@         if x < k { assert(out0[owner[x]].0 <= spans@[x].0); }
            //@     }
            //@     assert forall|a: int, b: int| 0 <= a < b < out@.len() implies (#[trigger] out@[a]).1 < (#[trigger] out@[b]).0 by {
            //@         if b < n { assert(out0[a].1 < out0[b].0); } else if a < n - 1 { assert(out0[a].1 < out0[n - 1].0); }
            //@     }
            //@     if k + 1 < spans@.len() { assert(spans@[k as int].0 <= spans@[k + 1].0); }
            //@ }
        }
        k = k + 1;
    }
    //@ proof { join_witnesses(spans@, out@, owner); }
    out
}

/// The intersection of two normalised range lists (the SourceBuffer's `buffered` over its tracks).
pub fn intersect(a: &Vec<(i64, i64)>, b: &Vec<(i64, i64)>) -> Vec<(i64, i64)> //@ret
//@ requires wf(a@), wf(b@),
//@ ensures wf(ret@), forall|t: int| #[trigger] covers(ret@, t) <==> (covers(a@, t) && covers(b@, t)),
{
    let mut out: Vec<(i64, i64)> = Vec::new();
    let mut i: usize = 0;
    let mut j: usize = 0;
    while i < a.len() && j < b.len()
        //@ invariant
        //@     i <= a@.len(), j <= b@.len(), wf(a@), wf(b@), wf(out@),
        //@     forall|t: int| #[trigger] covers(out@, t) ==> covers(a@, t) && covers(b@, t),
        //@     forall|t: int| covers(a@, t) && covers(b@, t) && (i < a@.len() && j < b@.len() ==> t < hi(a@[i as int].0, b@[j as int].0)) ==> #[trigger] covers(out@, t),
        //@     out@.len() > 0 && i < a@.len() && j < b@.len() ==> out@[out@.len() - 1].1 < hi(a@[i as int].0, b@[j as int].0),
        //@ decreases a@.len() + b@.len() - i - j,
    {
        //@ let ghost out0 = out@;
        //@ let ghost i0 = i as int;
        //@ let ghost j0 = j as int;
        let s = if a[i].0 > b[j].0 { a[i].0 } else { b[j].0 };
        let e = if a[i].1 < b[j].1 { a[i].1 } else { b[j].1 };
        if s < e {
            out.push((s, e));
            //@ assert forall|x: int, y: int| 0 <= x < y < out@.len() implies (#[trigger] out@[x]).1 < (#[trigger] out@[y]).0 by {
            //@     if y < out0.len() { assert(out0[x].1 < out0[y].0); } else if x < out0.len() - 1 { assert(out0[x].1 < out0[out0.len() - 1].0); }
            //@ }
        }
        if a[i].1 < b[j].1 { i = i + 1 } else { j = j + 1 }
        //@ proof {
        //@     assert forall|t: int| #[trigger] covers(out@, t) implies covers(a@, t) && covers(b@, t) by {
        //@         let k = choose|k: int| 0 <= k < out@.len() && (#[trigger] out@[k]).0 <= t && t < out@[k].1;
        //@         if k < out0.len() {
        //@             assert(out0[k].0 <= t && t < out0[k].1);
        //@             assert(covers(out0, t));
        //@         } else {
        //@             assert(a@[i0].0 <= t && t < a@[i0].1);
        //@             assert(b@[j0].0 <= t && t < b@[j0].1);
        //@         }
        //@     }
        //@     assert forall|t: int| covers(a@, t) && covers(b@, t) && (i < a@.len() && j < b@.len() ==> t < hi(a@[i as int].0, b@[j as int].0)) implies #[trigger] covers(out@, t) by {
        //@         if t < hi(a@[i0].0, b@[j0].0) {
        //@             assert(covers(out0, t));
        //@             let k = choose|k: int| 0 <= k < out0.len() && (#[trigger] out0[k]).0 <= t && t < out0[k].1;
        //@             assert(out@[k] == out0[k]);
        //@         } else {
        //@             let x = choose|x: int| 0 <= x < a@.len() && (#[trigger] a@[x]).0 <= t && t < a@[x].1;
        //@             let y = choose|y: int| 0 <= y < b@.len() && (#[trigger] b@[y]).0 <= t && t < b@[y].1;
        //@             if x < i0 { assert(a@[x].1 < a@[i0].0); }
        //@             if y < j0 { assert(b@[y].1 < b@[j0].0); }
        //@             if x > i0 {
        //@                 assert(a@[i0].1 < a@[x].0);
        //@                 if i == i0 + 1 && x > i { assert(a@[i as int].1 < a@[x].0); }
        //@                 if j == j0 + 1 && y > j0 && y > j { assert(b@[j as int].1 < b@[y].0); }
        //@             }
        //@             if y > j0 {
        //@                 assert(b@[j0].1 < b@[y].0);
        //@                 if j == j0 + 1 && y > j { assert(b@[j as int].1 < b@[y].0); }
        //@                 if i == i0 + 1 && x > i0 && x > i { assert(a@[i as int].1 < a@[x].0); }
        //@             }
        //@             assert(x == i0 && y == j0);
        //@             assert(out@[out@.len() - 1] == (s, e));
        //@         }
        //@     }
        //@     if out@.len() > 0 && i < a@.len() && j < b@.len() {
        //@         if i == i0 + 1 { assert(a@[i0].1 < a@[i as int].0); } else { assert(b@[j0].1 < b@[j as int].0); }
        //@     }
        //@ }
    }
    out
}

/// Whether `incoming` more bytes fit in `quota` beside the `bytes` held.
pub fn fits(bytes: usize, incoming: usize, quota: usize) -> bool //@ret
//@ ensures ret == (bytes as int + incoming as int <= quota as int),
{
    bytes <= quota && incoming <= quota - bytes
}

/// Coded frame eviction takes only what is before this: ten seconds before the playback position, as Chromium.
pub fn evict_before(current_us: i64) -> i64 //@ret
//@ ensures
//@     ret <= current_us,
//@     current_us >= i64::MIN + 10000000 ==> ret == current_us - 10000000,
//@     current_us < i64::MIN + 10000000 ==> ret == i64::MIN,
{
    if current_us < i64::MIN + 10_000_000 { i64::MIN } else { current_us - 10_000_000 }
}

/// Where eviction stops, given the presentation times of a track's random access points: the last one before `until`, so what
/// is left starts at a random access point. `None`: nothing to evict.
pub fn evict_cut(keys: &Vec<i64>, until: i64) -> Option<i64> //@ret
//@ ensures
//@     ret matches Some(c) ==> c < until && keys@.contains(c) && forall|k: int| 0 <= k < keys@.len() && #[trigger] keys@[k] < until ==> keys@[k] <= c,
//@     ret is None ==> forall|k: int| 0 <= k < keys@.len() ==> #[trigger] keys@[k] >= until,
{
    let mut best: Option<i64> = None;
    let mut k: usize = 0;
    while k < keys.len()
        //@ invariant
        //@     k <= keys@.len(),
        //@     best matches Some(c) ==> c < until && keys@.contains(c) && forall|x: int| 0 <= x < k && #[trigger] keys@[x] < until ==> keys@[x] <= c,
        //@     best is None ==> forall|x: int| 0 <= x < k ==> #[trigger] keys@[x] >= until,
        //@ decreases keys@.len() - k,
    {
        let p = keys[k];
        if p < until {
            best = match best {
                Some(c) => if c >= p { Some(c) } else { Some(p) },
                None => Some(p),
            };
            //@ assert(keys@[k as int] == p);
        }
        k = k + 1;
    }
    best
}
