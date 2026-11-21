// Which entries of the HTTP disk cache go when it is over its size (net/disk_cache.rs). Verus checks this very file: `//@` lines hold
// the proof, see LucinaNet scripts/verify-shipped.sh.
//
// Servo's loop (`delete_until_cache_size`) popped entries while the total was over the limit: with no entries left and a total still
// over (a size read back from the file that disagreed with the list) it never ended, and `size -= entry.size` could underflow. This
// one always ends, never underflows, and when the total is the sum of the sizes it drops the fewest oldest entries that make the rest
// fit. Servo dropped the newest (`pop_back` of a list it appends to), so a full cache threw away what it had just stored; this drops
// the oldest, as a cache should (Chromium and Firefox evict the least recently used).
//
// Not proved: that the sizes are the sizes of the records in the file (disk_cache.rs keeps them together under one lock).

//@ pub open spec fn suffix_sum(s: Seq<u64>, i: int) -> int
//@     decreases s.len() - i,
//@ {
//@     if i < 0 || i >= s.len() { 0 } else { s[i] as int + suffix_sum(s, i + 1) }
//@ }

//@ proof fn suffix_sum_nonneg(s: Seq<u64>, i: int)
//@     ensures suffix_sum(s, i) >= 0,
//@     decreases s.len() - i,
//@ {
//@     if i >= 0 && i < s.len() { suffix_sum_nonneg(s, i + 1); }
//@ }

/// How many of the oldest entries to drop (`sizes` oldest first, their total `total`) so that the rest fits in `max`, and the total
/// left after dropping them.
pub fn evict(sizes: &Vec<u64>, total: u64, max: u64) -> (usize, u64) //@ret
//@ ensures
//@     ret.0 <= sizes.len(),
//@     ret.1 <= total,
//@     total as int == suffix_sum(sizes@, 0) ==> (
//@         ret.1 as int == suffix_sum(sizes@, ret.0 as int)
//@         && (ret.1 <= max || ret.0 == sizes.len())
//@         && forall|j: int| 0 <= j < ret.0 ==> suffix_sum(sizes@, j) > max
//@     ),
{
    let mut dropped: usize = 0;
    let mut left: u64 = total;
    while dropped < sizes.len() && left > max
        //@ invariant
        //@     dropped <= sizes.len(),
        //@     left <= total,
        //@     total as int == suffix_sum(sizes@, 0) ==> (
        //@         left as int == suffix_sum(sizes@, dropped as int)
        //@         && forall|j: int| 0 <= j < dropped ==> suffix_sum(sizes@, j) > max
        //@     ),
        //@ decreases sizes.len() - dropped,
    {
        //@ proof { suffix_sum_nonneg(sizes@, dropped as int + 1); }
        let size = sizes[dropped];
        left = if left >= size { left - size } else { 0 };
        dropped = dropped + 1;
    }
    (dropped, left)
}
