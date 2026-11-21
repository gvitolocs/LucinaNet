// The size bound of the on-disk function cache, as rules over numbers: which records are in the store, how many bytes they take,
// which ones go when a new record needs the room. `DiskStore`'s writer thread calls these and keeps no second list or total.
// Verus checks this very file (the method of LucinaOS `scripts/verify-shipped.sh`): `//@` lines hold the proof.
// 
// Proved, for every sequence of the operations below (each one requires and ensures `inv`):
// - the counted bytes `total` are exactly the sum of the sizes of the records in the list, and no key is in the list twice;
// - after `insert` and after `from_existing` the bytes counted never exceed `max`, and an `insert` evicts only while they do;
// - eviction takes records from the front of the list, the least recently used first, in order, and nothing else;
// - a record larger than `max` is refused (`accepts`) so that it is never written, and the newly stored record is never
//   the one evicted;
// - `touch` (a lookup) moves a record to the back and changes neither the set of records nor the total.
// 
// Not proved (trusted): that the files in the directory are the records in the list. The writer unlinks each key `insert`,
// `evict_to` and `from_existing` return and writes each record it passes to `insert`, and no one else touches the directory.

/// The records, least recently used first, with their sizes, and the bytes they take together.
pub struct Lru {
    pub entries: Vec<(u128, u64)>,
    pub total: u64,
    pub max: u64,
}

//@ pub open spec fn sum(s: Seq<(u128, u64)>) -> int
//@ decreases s.len(),
//@ {
//@     if s.len() == 0 { 0 } else { sum(s.drop_last()) + s.last().1 as int }
//@ }

//@ pub open spec fn keys_unique(s: Seq<(u128, u64)>) -> bool {
//@     forall|i: int, j: int| 0 <= i < j < s.len() ==> (#[trigger] s[i]).0 != (#[trigger] s[j]).0
//@ }

//@ pub open spec fn has_key(s: Seq<(u128, u64)>, key: u128) -> bool { exists|i: int| 0 <= i < s.len() && (#[trigger] s[i]).0 == key }

//@ /// The sum splits over a concatenation.
//@ proof fn sum_add(a: Seq<(u128, u64)>, b: Seq<(u128, u64)>)
//@     ensures sum(a + b) == sum(a) + sum(b),
//@     decreases b.len(),
//@ {
//@     if b.len() > 0 {
//@         assert((a + b).drop_last() =~= a + b.drop_last());
//@         sum_add(a, b.drop_last());
//@     }
//@ }

//@ proof fn sum_remove(s: Seq<(u128, u64)>, i: int)
//@     requires 0 <= i < s.len(),
//@     ensures sum(s.remove(i)) == sum(s) - s[i].1,
//@ {
//@     assert(s == s.take(i) + s.subrange(i, s.len() as int));
//@     assert(s.remove(i) =~= s.take(i) + s.subrange(i + 1, s.len() as int));
//@     assert(s.subrange(i, s.len() as int) =~= seq![s[i]] + s.subrange(i + 1, s.len() as int));
//@     sum_add(s.take(i), s.subrange(i + 1, s.len() as int));
//@     sum_add(s.take(i), seq![s[i]] + s.subrange(i + 1, s.len() as int));
//@     sum_add(seq![s[i]], s.subrange(i + 1, s.len() as int));
//@     sum_single(s[i]);
//@ }

//@ proof fn sum_single(x: (u128, u64))
//@     ensures sum(seq![x]) == x.1 as int,
//@ {
//@     assert(seq![x].drop_last() =~= Seq::<(u128, u64)>::empty());
//@     reveal_with_fuel(sum, 2);
//@ }

//@ proof fn sum_nonneg(s: Seq<(u128, u64)>)
//@     ensures sum(s) >= 0,
//@     decreases s.len(),
//@ {
//@     if s.len() > 0 { sum_nonneg(s.drop_last()); }
//@ }

//@ /// Moving a record to the back keeps the records, their keys and the uniqueness of the keys.
//@ proof fn move_to_back(s: Seq<(u128, u64)>, i: int)
//@     requires 0 <= i < s.len(),
//@     ensures
//@         s.remove(i).push(s[i]).len() == s.len(),
//@         forall|k: u128| has_key(s.remove(i).push(s[i]), k) <==> has_key(s, k),
//@         keys_unique(s) ==> keys_unique(s.remove(i).push(s[i])),
//@ {
//@     let t = s.remove(i).push(s[i]);
//@     assert(forall|idx: int| 0 <= idx < i ==> s.remove(i)[idx] == s[idx]);
//@     assert(forall|idx: int| i <= idx < s.len() - 1 ==> s.remove(i)[idx] == s[idx + 1]);
//@     assert forall|k: u128| has_key(t, k) <==> has_key(s, k) by {
//@         if has_key(t, k) {
//@             let idx = choose|idx: int| 0 <= idx < t.len() && t[idx].0 == k;
//@             if idx < t.len() - 1 {
//@                 if idx < i { assert(s[idx].0 == k); } else { assert(s[idx + 1].0 == k); }
//@             } else {
//@                 assert(s[i].0 == k);
//@             }
//@         }
//@         if has_key(s, k) {
//@             let j = choose|j: int| 0 <= j < s.len() && s[j].0 == k;
//@             if j == i { assert(t[t.len() - 1].0 == k); }
//@             else if j < i { assert(t[j].0 == k); }
//@             else { assert(t[j - 1].0 == k); }
//@         }
//@     }
//@     if keys_unique(s) {
//@         assert forall|a: int, b: int| 0 <= a < b < t.len() implies (#[trigger] t[a]).0 != (#[trigger] t[b]).0 by {
//@             if b == t.len() - 1 {
//@                 if a < i { assert(t[a] == s[a]); } else { assert(t[a] == s[a + 1]); }
//@             } else {
//@                 if a < i { assert(t[a] == s[a]); } else { assert(t[a] == s[a + 1]); }
//@                 if b < i { assert(t[b] == s[b]); } else { assert(t[b] == s[b + 1]); }
//@             }
//@         }
//@     }
//@ }

//@ /// Taking the record at `i` out keeps the uniqueness, loses its key and no other.
//@ proof fn remove_props(s: Seq<(u128, u64)>, i: int)
//@     requires 0 <= i < s.len(), keys_unique(s),
//@     ensures
//@         keys_unique(s.remove(i)),
//@         !has_key(s.remove(i), s[i].0),
//@         forall|k: u128| has_key(s.remove(i), k) ==> has_key(s, k),
//@ {
//@     assert(forall|idx: int| 0 <= idx < i ==> s.remove(i)[idx] == s[idx]);
//@     assert(forall|idx: int| i <= idx < s.len() - 1 ==> s.remove(i)[idx] == s[idx + 1]);
//@     assert forall|a: int, b: int| 0 <= a < b < s.remove(i).len() implies (#[trigger] s.remove(i)[a]).0 != (#[trigger] s.remove(i)[b]).0 by {
//@         if a < i { assert(s.remove(i)[a] == s[a]); } else { assert(s.remove(i)[a] == s[a + 1]); }
//@         if b < i { assert(s.remove(i)[b] == s[b]); } else { assert(s.remove(i)[b] == s[b + 1]); }
//@     }
//@     assert forall|k: u128| has_key(s.remove(i), k) implies has_key(s, k) by {
//@         let idx = choose|idx: int| 0 <= idx < s.remove(i).len() && s.remove(i)[idx].0 == k;
//@         if idx < i { assert(s[idx].0 == k); } else { assert(s[idx + 1].0 == k); }
//@     }
//@     assert forall|idx: int| 0 <= idx < s.remove(i).len() implies (#[trigger] s.remove(i)[idx]).0 != s[i].0 by {
//@         if idx < i { assert(s.remove(i)[idx] == s[idx]); } else { assert(s.remove(i)[idx] == s[idx + 1]); }
//@     }
//@ }

//@ proof fn sum_push(s: Seq<(u128, u64)>, x: (u128, u64))
//@     ensures sum(s.push(x)) == sum(s) + x.1,
//@ {
//@     assert(s.push(x).drop_last() =~= s);
//@ }

/// Whether a record of `size` bytes may be stored at all: a record larger than the bound is never written.
pub fn accepts(size: u64, max: u64) -> bool //@ret
//@ ensures ret == (size <= max),
{
    size <= max
}

impl Lru {
    //@ pub open spec fn wf(&self) -> bool { self.total as int == sum(self.entries@) && keys_unique(self.entries@) }
    //@ /// The invariant of the store at rest: consistent accounting, within the bound.
    //@ pub open spec fn inv(&self) -> bool { self.wf() && self.total <= self.max }

    /// An empty store with the bound `max`.
    pub fn new(max: u64) -> Lru //@ret
    //@ ensures ret.inv(), ret.max == max, ret.entries@.len() == 0,
    {
        Lru { entries: Vec::new(), total: 0, max }
    }

    /// The index of the record of `key`.
    pub fn position(&self, key: u128) -> Option<usize> //@ret
    //@ ensures match ret {
    //@     Some(i) => i < self.entries@.len() && self.entries@[i as int].0 == key,
    //@     None => !has_key(self.entries@, key),
    //@ },
    {
        let mut i = 0;
        while i < self.entries.len()
        //@ invariant 0 <= i <= self.entries@.len(), forall|j: int| 0 <= j < i ==> (#[trigger] self.entries@[j]).0 != key,
        //@ decreases self.entries@.len() - i,
        {
            if self.entries[i].0 == key {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Take the records of an existing directory into the store, least recently used first, then evict down to `max`.
    /// A record whose size would overflow the count is left out (it is not in the list, so nothing counts it). Returns the keys
    /// to unlink: the evicted ones, and the ones left out.
    pub fn from_existing(existing: &[(u128, u64)], max: u64) -> (Lru, Vec<u128>) //@ret
    //@ requires keys_unique(existing@),
    //@ ensures ret.0.inv(), ret.0.max == max,
//@     forall|k: u128| has_key(existing@, k) ==> has_key(ret.0.entries@, k) || ret.1@.contains(k),
//@     forall|k: u128| ret.1@.contains(k) ==> !has_key(ret.0.entries@, k) && has_key(existing@, k),
    {
        let mut lru = Lru { entries: Vec::new(), total: 0, max };
        let mut dropped: Vec<u128> = Vec::new();
        let mut i = 0;
        while i < existing.len()
        //@ invariant 0 <= i <= existing@.len(), lru.wf(), lru.max == max, keys_unique(existing@),
        //@     forall|j: int| 0 <= j < lru.entries@.len() ==> exists|k: int| 0 <= k < i && existing@[k].0 == (#[trigger] lru.entries@[j]).0,
//@     forall|j: int| 0 <= j < i ==> has_key(lru.entries@, (#[trigger] existing@[j]).0) || dropped@.contains(existing@[j].0),
//@     forall|k: u128| dropped@.contains(k) ==> !has_key(lru.entries@, k) && exists|j: int| 0 <= j < i && existing@[j].0 == k,
        //@ decreases existing@.len() - i,
        {
            let (key, size) = existing[i];
            //@ let ghost old_entries = lru.entries@;
            //@ let ghost old_dropped = dropped@;
            //@ assert forall|j: int| 0 <= j < lru.entries@.len() implies #[trigger] lru.entries@[j].0 != key by {
            //@     let m = choose|m: int| 0 <= m < i && existing@[m].0 == lru.entries@[j].0;
            //@     assert(existing@[m].0 != existing@[i as int].0);
            //@ }
            //@ assert(!has_key(lru.entries@, key));
            if size > u64::MAX - lru.total {
                dropped.push(key);
                //@ assert(dropped@[dropped@.len() - 1] == key);
                //@ assert(dropped@.contains(key));
                //@ assert forall|k: u128| old_dropped.contains(k) implies dropped@.contains(k) by {
                //@     let m = choose|m: int| 0 <= m < old_dropped.len() && old_dropped[m] == k;
                //@     assert(dropped@[m] == k);
                //@ }
                //@ assert forall|k: u128| dropped@.contains(k) implies !has_key(lru.entries@, k) by {
                //@     let m = choose|m: int| 0 <= m < dropped@.len() && dropped@[m] == k;
                //@     if m < old_dropped.len() { assert(old_dropped[m] == k); assert(old_dropped.contains(k)); }
                //@     else { assert(k == key); }
                //@ }
            } else {
                //@ proof { sum_push(lru.entries@, (key, size)); }
                lru.entries.push((key, size));
                lru.total += size;
                //@ assert(lru.entries@[lru.entries@.len() - 1].0 == key);
                //@ assert(has_key(lru.entries@, key));
                //@ assert forall|j: int| 0 <= j < i implies has_key(lru.entries@, existing@[j].0) || dropped@.contains(existing@[j].0) by {
                //@     if has_key(old_entries, existing@[j].0) {
                //@         let m = choose|m: int| 0 <= m < old_entries.len() && old_entries[m].0 == existing@[j].0;
                //@         assert(lru.entries@[m] == old_entries[m]);
                //@     }
                //@ }
                //@ assert forall|k: u128| dropped@.contains(k) implies !has_key(lru.entries@, k) by {
                //@     let j = choose|j: int| 0 <= j < i && existing@[j].0 == k;
                //@     assert(existing@[j].0 != existing@[i as int].0);
                //@ }
            }
            //@ assert forall|k: u128| dropped@.contains(k) implies exists|j: int| 0 <= j < i + 1 && existing@[j].0 == k by {
            //@     if k != key { let m = choose|m: int| 0 <= m < dropped@.len() && dropped@[m] == k; assert(old_dropped.contains(k)); }
            //@ }
            i += 1;
        }
        //@ let ghost before = lru.entries@;
        //@ let ghost dropped_pre = dropped@;
        let mut evicted = lru.evict_to();
        //@ let ghost evicted_keys = evicted@;
        dropped.append(&mut evicted);
        //@ assert(dropped@ =~= dropped_pre + evicted_keys);
        //@ assert(lru.entries@ =~= before.skip(evicted_keys.len() as int));
        //@ assert forall|k: u128| has_key(existing@, k) implies has_key(lru.entries@, k) || dropped@.contains(k) by {
        //@     let j = choose|j: int| 0 <= j < existing@.len() && existing@[j].0 == k;
        //@     if dropped_pre.contains(k) {
        //@         let m = choose|m: int| 0 <= m < dropped_pre.len() && dropped_pre[m] == k;
        //@         assert(dropped@[m] == k);
        //@     } else {
        //@         assert(has_key(before, k));
        //@         let m = choose|m: int| 0 <= m < before.len() && before[m].0 == k;
        //@         if m >= evicted_keys.len() { assert(lru.entries@[m - evicted_keys.len()] == before[m]); }
        //@         else { assert(evicted_keys[m] == k); assert(dropped@[dropped_pre.len() + m] == k); }
        //@     }
        //@ }
        //@ assert forall|k: u128| dropped@.contains(k) implies !has_key(lru.entries@, k) && has_key(existing@, k) by {
        //@     let m = choose|m: int| 0 <= m < dropped@.len() && dropped@[m] == k;
        //@     if m < dropped_pre.len() {
        //@         assert(dropped_pre.contains(k));
        //@         let jj = choose|jj: int| 0 <= jj < existing@.len() && existing@[jj].0 == k;
        //@         assert(has_key(existing@, k));
        //@         if has_key(lru.entries@, k) {
        //@             let q = choose|q: int| 0 <= q < lru.entries@.len() && lru.entries@[q].0 == k;
        //@             assert(before[q + evicted_keys.len()] == lru.entries@[q]);
        //@             assert(has_key(before, k));
        //@         }
        //@     } else {
        //@         let e = m - dropped_pre.len();
        //@         assert(evicted_keys[e] == k);
        //@         assert(before[e].0 == k);
        //@         assert(exists|q: int| 0 <= q < existing@.len() && existing@[q].0 == before[e].0);
        //@         assert(has_key(existing@, k));
        //@     }
        //@ }
        (lru, dropped)
    }

    /// Evict from the front, least recently used first, while the count plus `extra` bytes is over `max`. Returns the evicted
    /// keys in order. With `extra = 0` this brings a store down to its bound.
    pub fn evict_for(&mut self, extra: u64) -> Vec<u128> //@ret
    //@ requires old(self).wf(), extra <= old(self).max,
    //@ ensures final(self).wf(), final(self).total as int + extra as int <= final(self).max as int, final(self).max == old(self).max,
    //@     ret@.len() <= old(self).entries@.len(),
    //@     final(self).entries@ =~= old(self).entries@.skip(ret@.len() as int),
    //@     forall|i: int| 0 <= i < ret@.len() ==> ret@[i] == (#[trigger] old(self).entries@[i]).0,
    //@     ret@.len() == 0 || final(self).total as int + old(self).entries@[ret@.len() - 1].1 as int + extra as int > final(self).max as int,
    //@     (old(self).total as int + extra as int <= old(self).max as int) ==> ret@.len() == 0,
//@     keys_unique(old(self).entries@) ==> forall|i: int| 0 <= i < ret@.len() ==> !has_key(final(self).entries@, ret@[i]),
    {
        let mut out: Vec<u128> = Vec::new();
        //@ let ghost old_entries = self.entries@;
        while self.total > self.max || extra > self.max - self.total
        //@ invariant self.wf(), self.max == old(self).max, out@.len() <= old_entries.len(),
        //@     self.entries@ =~= old_entries.skip(out@.len() as int),
        //@     forall|i: int| 0 <= i < out@.len() ==> out@[i] == (#[trigger] old_entries[i]).0,
        //@     out@.len() == 0 || self.total as int + old_entries[out@.len() - 1].1 as int + extra as int > self.max as int,
        //@     old_entries == old(self).entries@, extra <= self.max, out@.len() == 0 ==> self.total == old(self).total,
        //@     out@.len() > 0 ==> old(self).total as int + extra as int > old(self).max as int,
        //@ decreases self.entries@.len(),
        {
            //@ proof { if self.entries@.len() == 0 { assert(sum(self.entries@) == 0); } sum_nonneg(self.entries@.remove(0)); }
            //@ proof { sum_remove(self.entries@, 0); }
            let (key, size) = self.entries.remove(0);
            self.total -= size;
            out.push(key);
            //@ assert(self.entries@ =~= old_entries.skip(out@.len() as int));
        }
        //@ assert forall|i: int| 0 <= i < out@.len() implies !has_key(self.entries@, out@[i]) by {
        //@     if has_key(self.entries@, out@[i]) {
        //@         let j = choose|j: int| 0 <= j < self.entries@.len() && self.entries@[j].0 == out@[i];
        //@         assert(self.entries@[j] == old_entries[j + out@.len()]);
        //@         assert(old_entries[i].0 == out@[i]);
        //@     }
        //@ }
        out
    }

    /// Evict down to the bound.
    pub fn evict_to(&mut self) -> Vec<u128> //@ret
    //@ requires old(self).wf(),
    //@ ensures final(self).wf(), final(self).total <= final(self).max, final(self).max == old(self).max,
    //@     ret@.len() <= old(self).entries@.len(),
    //@     final(self).entries@ =~= old(self).entries@.skip(ret@.len() as int),
    //@     forall|i: int| 0 <= i < ret@.len() ==> ret@[i] == (#[trigger] old(self).entries@[i]).0,
    //@     old(self).total <= old(self).max ==> ret@.len() == 0,
    //@     keys_unique(old(self).entries@) ==> forall|i: int| 0 <= i < ret@.len() ==> !has_key(final(self).entries@, ret@[i]),
    //@     forall|i: int| 0 <= i < ret@.len() ==> has_key(old(self).entries@, ret@[i]),
    {
        let r = self.evict_for(0);
        //@ assert forall|i: int| 0 <= i < r@.len() implies has_key(old(self).entries@, r@[i]) by { assert(old(self).entries@[i].0 == r@[i]); }
        r
    }

    /// A lookup used the record of `key`: it becomes the most recently used. The set of records and the total do not change.
    pub fn touch(&mut self, key: u128)
    //@ requires old(self).inv(),
    //@ ensures final(self).inv(), final(self).max == old(self).max, final(self).total == old(self).total,
    //@     forall|k: u128| has_key(final(self).entries@, k) <==> has_key(old(self).entries@, k),
    //@     final(self).entries@.len() == old(self).entries@.len(),
    {
        match self.position(key) {
            Some(i) => {
                //@ proof { sum_remove(self.entries@, i as int); sum_nonneg(self.entries@.remove(i as int)); }
                let entry = self.entries.remove(i);
                //@ proof { sum_push(self.entries@, entry); }
                self.entries.push(entry);
                //@ proof { move_to_back(old(self).entries@, i as int); assert(entry == old(self).entries@[i as int]); }
            }
            None => {}
        }
    }

    /// Store a record of `size` bytes under `key` (already written): it replaces a record of the same key, becomes the most
    /// recently used, and the least recently used records go until the count is within `max`. Returns the keys to unlink.
    pub fn insert(&mut self, key: u128, size: u64) -> Vec<u128> //@ret
    //@ requires old(self).inv(), size <= old(self).max,
    //@ ensures final(self).inv(), final(self).max == old(self).max,
    //@     final(self).entries@.len() > 0, final(self).entries@[final(self).entries@.len() - 1] == (key, size),
    //@     !ret@.contains(key),
    //@     forall|i: int| 0 <= i < ret@.len() ==> has_key(old(self).entries@, ret@[i]),
//@     forall|i: int| 0 <= i < ret@.len() ==> !has_key(final(self).entries@, ret@[i]),
    {
        match self.position(key) {
            Some(i) => {
                //@ proof { sum_remove(self.entries@, i as int); sum_nonneg(self.entries@.remove(i as int)); remove_props(self.entries@, i as int); }
                let (_, old_size) = self.entries.remove(i);
                self.total -= old_size;
            }
            None => {}
        }
        //@ let ghost mid = self.entries@;
        //@ assert(!has_key(mid, key) && (forall|k: u128| has_key(mid, k) ==> has_key(old(self).entries@, k)));
        let evicted = self.evict_for(size);
        //@ assert forall|i: int| 0 <= i < evicted@.len() implies has_key(old(self).entries@, evicted@[i]) && evicted@[i] != key by {
        //@     assert(mid[i].0 == evicted@[i]);
        //@     assert(has_key(mid, evicted@[i]));
        //@ }
        //@ assert(!evicted@.contains(key));
        //@ assert(forall|j: int| 0 <= j < self.entries@.len() ==> self.entries@[j].0 != key);
        //@ proof { sum_push(self.entries@, (key, size)); }
        self.entries.push((key, size));
        self.total += size;
        //@ assert(keys_unique(self.entries@));
        evicted
    }
}
