// The name cache's rules: how long an entry lives, when a lookup starts, how many entries are kept.
// Verus checks this very file (`//@` lines); see verify.sh and mutants.sh.
#![allow(clippy::needless_return)] //@omit

/// How long a found address lives, ms. Chromium: kCacheEntryTTLSeconds = 60 for the system resolver
/// (net/dns/host_resolver_manager_job.cc); Firefox: network.dnsCacheExpiration = 60 "in the absence of OS TTLs".
pub const POSITIVE_TTL_MS: u64 = 60_000;
/// How long a failure lives, ms. OUR number: Chromium keeps none (kNegativeCacheEntryTTLSeconds = 0), Firefox keeps 60 s
/// (NEGATIVE_RECORD_LIFETIME = 60, nsHostResolver.cpp). Five seconds stops a page that asks 40 times for a dead name from
/// asking the system 40 times, and lets a fixed network recover within a few seconds.
pub const NEGATIVE_TTL_MS: u64 = 5_000;
/// Entries kept: Firefox network.dnsCacheEntries = 800 (StaticPrefList.yaml). Chromium's cache size is set by the embedder.
pub const MAX_ENTRIES: usize = 800;

/// What the cache holds for a name.
pub enum Slot {
    /// Nothing.
    Absent,
    /// A lookup is running.
    InFlight,
    /// An answer stored `age_ms` ago; `failure` if it is a failed lookup.
    Ready { age_ms: u64, failure: bool },
}

/// What an ask for a name does.
pub enum Action {
    /// Answer from the cache.
    Hit,
    /// Wait for the lookup that is running.
    Attach,
    /// Start a lookup.
    Start,
}

/// The lifetime of an entry.
pub fn ttl_ms(failure: bool) -> u64 //@ret
//@ ensures failure ==> ret == 5000, !failure ==> ret == 60000,
{
    if failure {
        return NEGATIVE_TTL_MS;
    }
    return POSITIVE_TTL_MS;
}

/// An entry is usable for strictly less than its lifetime, so a zero age is fresh and the lifetime itself is not.
pub fn is_fresh(age_ms: u64, failure: bool) -> bool //@ret
//@ ensures ret == (age_ms < if failure { 5000u64 } else { 60000u64 }),
{
    return age_ms < ttl_ms(failure);
}

/// What an ask does given what the cache holds. Single-flight: while a lookup runs, every ask attaches to it.
pub fn decide(slot: &Slot) -> Action //@ret
//@ ensures slot is Absent ==> ret is Start, slot is InFlight ==> ret is Attach, slot is Ready ==> (ret is Hit <==> slot->Ready_age_ms < if slot->Ready_failure { 5000u64 } else { 60000u64 }), slot is Ready ==> !(ret is Attach),
{
    match slot {
        Slot::Absent => Action::Start,
        Slot::InFlight => Action::Attach,
        Slot::Ready { age_ms, failure } => {
            if is_fresh(*age_ms, *failure) {
                Action::Hit
            } else {
                Action::Start
            }
        },
    }
}

/// Entries to drop before storing one more so that the cache never holds more than `cap`: none while there is room.
pub fn evict_count(len: usize, cap: usize) -> usize //@ret
//@ requires cap >= 1, ensures len < cap ==> ret == 0, len >= cap ==> ret == len - cap + 1, len >= cap ==> len - ret + 1 == cap,
{
    if len < cap {
        return 0;
    }
    return len - cap + 1;
}
