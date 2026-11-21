// The hit decision of the function cache, kept pure and small.
//
// A cached record belongs to one key and one partition (the top-level site of the realm that
// created it). A lookup may use a record only if both are equal. The key already mixes the
// partition in, but the decision never relies on the hash alone.
//
// Verus checks this very file (the method of LucinaOS `scripts/verify-shipped.sh`): `//@` lines hold the proof.
//
// Proved: `may_hit` is true exactly when the entry's key equals the lookup's key and the entry's partition digest equals the lookup's
// partition digest (a hit needs both); so an entry of another partition, or of another key, never hits
// (`other_partition_never_hits`, `other_key_never_hits`); and for a table of entries by key, a lookup that hits returns an entry
// stored under the partition asked for (`lookup_stays_in_partition`).
//
// Not proved (trusted): that the digest of a partition tells two sites apart (it is a hash), and that the caller stores an entry under the
// partition and key of the function it was made from.

//@ /// The decision: the keys are equal and the partition digests are equal.
//@ pub open spec fn hit(entry_key: u128, entry_partition: u128, key: u128, partition: u128) -> bool {
//@     entry_key == key && entry_partition == partition
//@ }

/// Whether the record (`entry_key`, `entry_partition`) may answer a lookup of (`key`, `partition`).
pub fn may_hit(entry_key: u128, entry_partition: u128, key: u128, partition: u128) -> bool //@ret
//@ ensures ret == hit(entry_key, entry_partition, key, partition),
{
    entry_key == key && entry_partition == partition
}

//@ /// A hit needs both: the keys are equal and the partitions are equal.
//@ pub proof fn may_hit_needs_both(entry_key: u128, entry_partition: u128, key: u128, partition: u128)
//@     requires hit(entry_key, entry_partition, key, partition),
//@     ensures entry_key == key, entry_partition == partition,
//@ {
//@ }

//@ pub proof fn other_partition_never_hits(entry_key: u128, entry_partition: u128, key: u128, partition: u128)
//@     requires entry_partition != partition,
//@     ensures !hit(entry_key, entry_partition, key, partition),
//@ {
//@ }

//@ pub proof fn other_key_never_hits(entry_key: u128, entry_partition: u128, key: u128, partition: u128)
//@     requires entry_key != key,
//@     ensures !hit(entry_key, entry_partition, key, partition),
//@ {
//@ }

//@ /// A table from key to the partition its entry was stored under (the entry itself does not matter here).
//@ pub open spec fn lookup(table: Map<u128, u128>, key: u128, partition: u128) -> Option<u128> {
//@     if table.dom().contains(key) && hit(key, table[key], key, partition) { Some(table[key]) } else { None }
//@ }

//@ /// A lookup that hits answers with an entry stored under the partition asked for.
//@ pub proof fn lookup_stays_in_partition(table: Map<u128, u128>, key: u128, partition: u128)
//@     ensures match lookup(table, key, partition) { Some(stored) => stored == partition && table.dom().contains(key), None => true },
//@ {
//@ }

#[cfg(test)] //@omit
mod tests { //@omit
    use super::may_hit; //@omit
 //@omit
    #[test] //@omit
    fn hits_only_when_key_and_partition_are_equal() { //@omit
        assert!(may_hit(1, 2, 1, 2)); //@omit
        assert!(!may_hit(1, 2, 1, 3)); //@omit
        assert!(!may_hit(1, 2, 4, 2)); //@omit
        assert!(!may_hit(1, 2, 4, 3)); //@omit
    } //@omit
} //@omit
