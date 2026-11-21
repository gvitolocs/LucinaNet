// Memory the copying heap does not see, counted the way SpiderMonkey counts it, and when it makes a collection due. The rules are
// SpiderMonkey's (the original, mozjs 153; audit in LucinaNet-js docs/lucinajs-memory-audit.md):
//  - Associated memory: what the embedder reports with JS::AddAssociatedMemory / RemoveAssociatedMemory (Servo: each DOM object's
//    Rust size, canvas and WebGL stores). SpiderMonkey keeps it in the zone's malloc counter (gc/ZoneAllocator.h:70-94) and asks
//    for a collection when the counter reaches max(retained, 38 MiB) x growth (gc/ZoneAllocator.h:155-168, gc/Scheduling.cpp:578-596).
//  - Host-space bytes: host objects (Servo's DOM reflectors, and every promise) live outside the copying heap. In SpiderMonkey they
//    are GC cells, counted in the zone's GC heap, which collects at max(retained, 27 MiB) x growth (gc/Scheduling.cpp:501-509).
//  - growth is SpiderMonkey's computeZoneHeapGrowthFactorForHeapSize (gc/Scheduling.cpp:467-498): the low-frequency factor below
//    1 MiB retained or when collections are infrequent; while frequent, the small-heap factor up to the small-heap size, the
//    large-heap factor from the large-heap size, linear in between. The factors and sizes are SpiderMonkey's tunables, which
//    Servo sets with JS_SetGCParameter (its defaults: Scheduling.h:374-422, Servo's prefs.rs:555-570).
//  - The heap's maximum (JSGC_MAX_BYTES) caps how far the copying heap may grow.
// Retained means what is left after a collection, finalizers included (they remove their associated memory first), as
// SpiderMonkey's retainedBytes (Scheduling.h:653-670).
//
// Verus checks this very file (`//@` lines, as in sizing.rs; verification/gc-accounting/run.sh). Trusted, not proved: that the
// constants are SpiderMonkey's values, and that the callers pass the counters and tunables they hold.

//@ global size_of usize == 8;

/// One MiB: below this much retained, SpiderMonkey always uses the low-frequency factor.
pub const ONE_MIB: usize = 1 << 20;
/// JSGC_MALLOC_THRESHOLD_BASE default: 38 MiB (Scheduling.h:430-431).
pub const MALLOC_BASE: usize = 38 << 20;
/// JSGC_ALLOCATION_THRESHOLD default: 27 MiB (Scheduling.h:363-364), the base of the GC-heap trigger.
pub const HOST_BASE: usize = 27 << 20;
/// JSGC_SMALL_HEAP_SIZE_MAX and JSGC_LARGE_HEAP_SIZE_MIN defaults: 100 and 500 MiB (Scheduling.h:374-377).
pub const SMALL_HEAP: usize = 100 << 20;
pub const LARGE_HEAP: usize = 500 << 20;
/// The growth factors in tenths: high frequency 3.0 (small heap) and 1.5 (large heap), low frequency 1.5 (Scheduling.h:409-422).
pub const HIGH_SMALL_TENTHS: usize = 30;
pub const HIGH_LARGE_TENTHS: usize = 15;
pub const LOW_TENTHS: usize = 15;
/// The largest factor a tunable may set, in tenths (SpiderMonkey accepts up to 10.0 for heap growth).
pub const MAX_TENTHS: usize = 100;
/// The largest heap size a tunable may name: 1 PiB, so that every product below fits in a usize.
pub const MAX_SIZE: usize = 1 << 50;

//@ pub open spec fn imax(a: int, b: int) -> int { if a > b { a } else { b } }

//@ /// The tunables are sane: factors of at least 1.0, the small-heap factor at least the large-heap one, sizes in order.
//@ pub open spec fn tunables_ok(low: int, high_small: int, high_large: int, small: int, large: int) -> bool {
//@     10 <= low <= MAX_TENTHS as int && 10 <= high_large <= high_small <= MAX_TENTHS as int
//@         && 0 <= small < large <= MAX_SIZE as int
//@ }

//@ /// SpiderMonkey's growth factor in tenths (computeZoneHeapGrowthFactorForHeapSize), rounded down.
//@ pub open spec fn growth_spec(r: int, hf: bool, low: int, high_small: int, high_large: int, small: int, large: int) -> int {
//@     if !hf || r < ONE_MIB as int { low }
//@     else if r <= small { high_small }
//@     else if r >= large { high_large }
//@     else { high_small - ((high_small - high_large) * (r - small)) / (large - small) }
//@ }

//@ /// When the counter makes a collection due: max(retained, base) times the factor, saturated.
//@ pub open spec fn threshold_spec(r: int, base: int, f: int) -> int {
//@     if imax(r, base) > usize::MAX as int / MAX_TENTHS as int { usize::MAX as int } else { imax(r, base) * f / 10 }
//@ }

//@ proof fn consts()
//@     ensures ONE_MIB == 1048576, MALLOC_BASE == 39845888, HOST_BASE == 28311552, SMALL_HEAP == 104857600,
//@         LARGE_HEAP == 524288000, HIGH_SMALL_TENTHS == 30, HIGH_LARGE_TENTHS == 15, LOW_TENTHS == 15, MAX_TENTHS == 100,
//@         MAX_SIZE == 1125899906842624,
//@ {
//@     assert(1usize << 20 == 1048576usize) by (bit_vector);
//@     assert(38usize << 20 == 39845888usize) by (bit_vector);
//@     assert(27usize << 20 == 28311552usize) by (bit_vector);
//@     assert(100usize << 20 == 104857600usize) by (bit_vector);
//@     assert(500usize << 20 == 524288000usize) by (bit_vector);
//@     assert(1usize << 50 == 1125899906842624usize) by (bit_vector);
//@ }

//@ proof fn interp_bounds(r: int, hs: int, hl: int, small: int, large: int)
//@     requires 0 <= small < r < large, 10 <= hl <= hs <= 100, large <= 1125899906842624,
//@     ensures 0 <= (hs - hl) * (r - small) <= 100 * 1125899906842624,
//@         0 <= ((hs - hl) * (r - small)) / (large - small) <= hs - hl,
//@ {
//@     assert(0 <= (hs - hl) * (r - small)) by (nonlinear_arith) requires hs >= hl, r > small;
//@     assert((hs - hl) * (r - small) <= 100 * 1125899906842624) by (nonlinear_arith)
//@         requires hs - hl <= 100, 0 < r - small < 1125899906842624, hs >= hl;
//@     assert((hs - hl) * (r - small) <= (hs - hl) * (large - small)) by (nonlinear_arith)
//@         requires hs >= hl, r - small <= large - small;
//@     vstd::arithmetic::div_mod::lemma_div_is_ordered((hs - hl) * (r - small), (hs - hl) * (large - small), large - small);
//@     vstd::arithmetic::div_mod::lemma_div_multiples_vanish(hs - hl, large - small);
//@     vstd::arithmetic::div_mod::lemma_div_pos_is_pos((hs - hl) * (r - small), large - small);
//@ }

/// SpiderMonkey's heap growth factor in tenths for `retained` bytes (computeZoneHeapGrowthFactorForHeapSize).
pub fn growth_tenths(
    retained: usize,
    high_frequency: bool,
    low: usize,
    high_small: usize,
    high_large: usize,
    small: usize,
    large: usize,
) -> usize //@ret
//@ requires tunables_ok(low as int, high_small as int, high_large as int, small as int, large as int),
//@ ensures ret as int == growth_spec(retained as int, high_frequency, low as int, high_small as int, high_large as int,
//@         small as int, large as int),
//@     10 <= ret <= MAX_TENTHS,
//@     !high_frequency ==> ret == low,
//@     high_frequency && retained >= ONE_MIB ==> high_large <= ret <= high_small,
{
    //@ proof { consts(); if high_frequency && retained >= ONE_MIB && retained > small && retained < large {
    //@     interp_bounds(retained as int, high_small as int, high_large as int, small as int, large as int); } }
    if !high_frequency || retained < ONE_MIB {
        low
    } else if retained <= small {
        high_small
    } else if retained >= large {
        high_large
    } else {
        high_small - ((high_small - high_large) * (retained - small)) / (large - small)
    }
}

/// The counter value at which a collection is due: max(retained, base) x factor / 10, saturated (MallocHeapThreshold /
/// GCHeapThreshold::computeZoneTriggerBytes).
pub fn threshold(retained: usize, base: usize, factor_tenths: usize) -> usize //@ret
//@ requires 10 <= factor_tenths <= MAX_TENTHS,
//@ ensures ret as int == threshold_spec(retained as int, base as int, factor_tenths as int), ret >= retained, ret >= base,
{
    //@ proof { consts(); }
    let b = if retained > base { retained } else { base };
    //@ proof {
    //@     if b <= usize::MAX / MAX_TENTHS {
    //@         assert(b * factor_tenths <= b * 100) by (nonlinear_arith) requires factor_tenths <= 100, b >= 0;
    //@         assert(b * 100 <= usize::MAX) by (nonlinear_arith) requires b <= usize::MAX / 100;
    //@         assert(b * factor_tenths >= 10 * b) by (nonlinear_arith) requires factor_tenths >= 10, b >= 0;
    //@         vstd::arithmetic::div_mod::lemma_div_is_ordered(10 * b as int, b * factor_tenths as int, 10);
    //@         vstd::arithmetic::div_mod::lemma_div_multiples_vanish(b as int, 10);
    //@     }
    //@ }
    if b > usize::MAX / MAX_TENTHS { usize::MAX } else { b * factor_tenths / 10 }
}

/// Whether the counter has reached its threshold (checkHeapThreshold: used >= threshold, gc/GC.cpp:2187-2200).
pub fn due(bytes: usize, threshold: usize) -> bool //@ret
//@ ensures ret <==> bytes >= threshold,
{
    bytes >= threshold
}

/// The counter after the embedder reports `n` more bytes (saturating: a counter never wraps).
pub fn charge(count: usize, n: usize) -> usize //@ret
//@ ensures ret as int == if count as int + n as int > usize::MAX as int { usize::MAX as int } else { count as int + n as int },
//@     ret >= count,
{
    if count > usize::MAX - n { usize::MAX } else { count + n }
}

/// The counter after the embedder removes `n` bytes; never below zero, whatever the embedder reports.
pub fn uncharge(count: usize, n: usize) -> usize //@ret
//@ ensures ret as int == if n > count { 0 } else { count as int - n as int }, ret <= count,
{
    if n > count { 0 } else { count - n }
}

//@ /// n is 1, 2, 4, 8, ...
//@ pub open spec fn is_pow2(n: nat) -> bool
//@ decreases n,
//@ {
//@     if n == 1 { true } else if n >= 2 && n % 2 == 0 { is_pow2((n / 2) as nat) } else { false }
//@ }

//@ proof fn pow2_double(p: nat)
//@     requires is_pow2(p),
//@     ensures is_pow2(p * 2),
//@ {
//@     assert((p * 2) / 2 == p);
//@     assert((p * 2) % 2 == 0);
//@     reveal_with_fuel(is_pow2, 2);
//@ }

/// The largest heap the copying heap may grow to under a JSGC_MAX_BYTES of `max_bytes`: the largest power of two from
/// `min_heap` up that is at most `max_bytes` and at most `options_max` (the heap's sizes are powers of two), and never below
/// `min_heap`, the heap's starting size.
pub fn max_heap_for(max_bytes: usize, min_heap: usize, options_max: usize) -> usize //@ret
//@ requires is_pow2(min_heap as nat), min_heap <= options_max, options_max <= usize::MAX / 2,
//@ ensures is_pow2(ret as nat), min_heap <= ret <= options_max, ret == min_heap || ret <= max_bytes,
//@     ret * 2 > options_max || ret * 2 > max_bytes,
{
    let mut p = min_heap;
    while p <= options_max / 2 && p * 2 <= max_bytes
    //@ invariant is_pow2(p as nat), min_heap <= p <= options_max, p == min_heap || p <= max_bytes, options_max <= usize::MAX / 2,
    //@ decreases options_max - p,
    {
        //@ proof { pow2_double(p as nat); }
        p = p * 2;
    }
    p
}

/// Whether a collection hands its free memory back to the system now, as SpiderMonkey's GCRuntime::shouldDecommit
/// (gc/GC.cpp:2281-2294): a shrinking collection always does; a normal one only when collections are infrequent, "so that we do
/// not compete with the mutator" (a program allocating heavily would fault the memory back in after every collection).
pub fn should_release(high_frequency: bool, shrinking: bool) -> bool //@ret
//@ ensures ret <==> (shrinking || !high_frequency),
{
    if shrinking { true } else { !high_frequency }
}
