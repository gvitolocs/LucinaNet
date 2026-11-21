// How many pages LucinaNet keeps alive behind Back and Forward. Pure integer rules: inputs are the machine's total memory, what is
// available, a pressure level and the ages of the entries, so a system-provided pressure signal can replace the polling later
// (docs/specs/history-depth.md). Verus checks this very file: `//@` lines hold the proof, see raster/verify.sh and
// crates/lucinanet-history/mutants.sh. Numbers and their sources are in the spec.
#![allow(clippy::needless_return)] //@omit

/// Most pages kept alive in all: Firefox caps the viewers at 8 (nsSHistory.cpp CalcMaxTotalViewers, "Cap it off at 8 max").
pub const MAX_ENTRIES: u32 = 8;
/// Firefox keeps viewers only within this many entries of the current one (nsSHistory.h VIEWER_WINDOW = 3), so it is also
/// the most per side: Servo's depth is per side.
pub const VIEWER_WINDOW: u32 = 3;
/// Chromium's cache limit under moderate memory pressure for the foreground tab (back_forward_cache_impl.cc,
/// foreground_cache_size_on_moderate_pressure = 3); under critical pressure it is 0, as Firefox's memory-pressure eviction.
pub const LOW_PRESSURE_ENTRIES: u32 = 3;
/// What one kept page costs, MiB. Measured on the build host: 115 MiB (example.com) to 560 MiB (YouTube) peak RSS; a middling page.
pub const PAGE_COST_MIB: u64 = 250;
/// Memory always left to the current page and the system, MiB; the engine frees its caches below this (`FREE_CACHES_MIB`).
pub const RESERVE_MIN_MIB: u64 = 500;
/// Pressure levels: what the OS (later) or a sample taken at a navigation (now) says.
pub const PRESSURE_NORMAL: u8 = 0;
pub const PRESSURE_LOW: u8 = 1;
pub const PRESSURE_CRITICAL: u8 = 2;
/// Thresholds for a sample, MiB available (the engine's `FREE_CACHES_MIB` and `CLOSE_PAGE_MIB`).
pub const LOW_BELOW_MIB: u64 = 500;
pub const CRITICAL_BELOW_MIB: u64 = 300;
/// A kept page expires after this many seconds: Chromium's 600 (kDefaultTimeToLiveInBackForwardCacheInSeconds) on small
/// machines, Firefox's 1800 (CONTENT_VIEWER_TIMEOUT_SECONDS_DEFAULT) from `LARGE_MIB`.
pub const TTL_SMALL_SECS: u64 = 600;
pub const TTL_LARGE_SECS: u64 = 1800;
/// From this much RAM (8 GiB) the long time to live applies.
pub const LARGE_MIB: u64 = 8192;

/// Firefox's DocumentViewer count by RAM (nsSHistory.cpp CalcMaxTotalViewers, desktop): with x = log2(KiB) - 14, none for
/// x <= 0, else floor(x*x - x + 2.001) / 4, at most 8. The steps of that formula in MiB, a table, no logarithm (the test
/// compares it with the formula for every MiB up to 64 GiB).
pub fn viewers_for_ram(total_mib: u64) -> u32 //@ret
//@ ensures ret <= 8, total_mib < 64 ==> ret == 0,
{
    if total_mib < 64 {
        return 0;
    }
    if total_mib < 128 {
        return 1;
    }
    if total_mib < 209 {
        return 2;
    }
    if total_mib < 310 {
        return 3;
    }
    if total_mib < 438 {
        return 4;
    }
    if total_mib < 596 {
        return 5;
    }
    if total_mib < 789 {
        return 6;
    }
    if total_mib < 1024 {
        return 7;
    }
    return 8;
}

/// What the memory left makes of the pressure level: what a system service would give.
pub fn pressure_from_available(available_mib: u64) -> u8 //@ret
//@ ensures ret <= 2, available_mib < 300 ==> ret == 2, 300 <= available_mib < 500 ==> ret == 1, available_mib >= 500 ==> ret == 0,
{
    if available_mib < CRITICAL_BELOW_MIB {
        return PRESSURE_CRITICAL;
    }
    if available_mib < LOW_BELOW_MIB {
        return PRESSURE_LOW;
    }
    return PRESSURE_NORMAL;
}

/// Memory kept free: an eighth of the machine, never under `RESERVE_MIN_MIB`.
pub fn reserve_mib(total_mib: u64) -> u64 //@ret
//@ ensures ret >= 500, ret >= total_mib / 8,
{
    let eighth = total_mib / 8;
    if eighth > RESERVE_MIN_MIB {
        return eighth;
    }
    return RESERVE_MIN_MIB;
}

/// Seconds a kept page lives.
pub fn ttl_secs(total_mib: u64) -> u64 {
    if total_mib >= LARGE_MIB {
        return TTL_LARGE_SECS;
    }
    return TTL_SMALL_SECS;
}

/// Pages to keep alive in all. Never above Firefox's count for this RAM, never more than the memory above the reserve holds
/// (and a quarter of the machine at most), three under low pressure, none under critical pressure.
pub fn max_entries(total_mib: u64, available_mib: u64, pressure: u8) -> u32 //@ret
//@ ensures ret <= 8, pressure >= 2 ==> ret == 0, pressure == 1 ==> ret <= 3, available_mib <= 500 ==> ret == 0, total_mib < 64 ==> ret == 0,
//@     ret > 0 ==> ret * 250 + 500 <= available_mib && ret * 250 + total_mib / 8 <= available_mib,
{
    if pressure >= PRESSURE_CRITICAL {
        return 0;
    }
    let ram = viewers_for_ram(total_mib);
    let reserve = reserve_mib(total_mib);
    if available_mib <= reserve {
        return 0;
    }
    let usable = available_mib - reserve;
    let share = total_mib / 4;
    let budget = if usable < share { usable } else { share };
    let by_memory = budget / PAGE_COST_MIB;
    let n = if by_memory < ram as u64 { by_memory as u32 } else { ram };
    if pressure == PRESSURE_LOW && n > LOW_PRESSURE_ENTRIES {
        return LOW_PRESSURE_ENTRIES;
    }
    return n;
}

/// What one of `tabs` open tabs may count on: the reserve stays whole, the memory above it is shared equally. Every tab
/// keeps its own pages, so the pages alive in all stay inside what one tab alone would have been given.
pub fn shared_available(total_mib: u64, available_mib: u64, tabs: u64) -> u64 //@ret
//@ ensures ret <= available_mib,
{
    let reserve = reserve_mib(total_mib);
    if tabs <= 1 || available_mib <= reserve {
        return available_mib;
    }
    return reserve + (available_mib - reserve) / tabs;
}

/// The depth for Servo, which counts per side: the pages in all, at most `VIEWER_WINDOW`.
pub fn side_depth(total_mib: u64, available_mib: u64, pressure: u8) -> u32 //@ret
//@ ensures ret <= 3, pressure >= 2 ==> ret == 0, available_mib <= 500 ==> ret == 0,
{
    let n = max_entries(total_mib, available_mib, pressure);
    if n > VIEWER_WINDOW {
        return VIEWER_WINDOW;
    }
    return n;
}

/// How many of the newest entries (`ages` oldest first, seconds since each was left) are younger than `ttl`.
pub fn fresh_entries(ages: &[u64], ttl: u64) -> usize //@ret
//@ ensures ret <= ages.len(), forall|j: int| ages.len() - ret <= j < ages.len() ==> ages[j] <= ttl,
{
    let mut fresh: usize = 0;
    while fresh < ages.len() && ages[ages.len() - 1 - fresh] <= ttl
        //@ invariant fresh <= ages.len(), forall|j: int| ages.len() - fresh <= j < ages.len() ==> ages[j] <= ttl,
        //@ decreases ages.len() - fresh,
    {
        fresh = fresh + 1;
    }
    return fresh;
}

/// What to do with the entries of one side: keep the newest `keep`, drop the oldest `evict`.
pub struct Plan {
    pub keep: usize,
    pub evict: usize,
}

/// The newest entries that are within the time to live, at most `max_entries`; the rest, oldest first, goes.
pub fn plan(total_mib: u64, available_mib: u64, pressure: u8, ages: &[u64]) -> Plan //@ret
//@ ensures ret.keep + ret.evict == ages.len(), ret.keep <= 8, ret.keep <= ages.len(), pressure >= 2 ==> ret.keep == 0,
//@     available_mib <= 500 ==> ret.keep == 0,
{
    let cap = max_entries(total_mib, available_mib, pressure) as usize;
    let fresh = fresh_entries(ages, ttl_secs(total_mib));
    let keep = if cap < fresh { cap } else { fresh };
    return Plan { keep, evict: ages.len() - keep };
}
