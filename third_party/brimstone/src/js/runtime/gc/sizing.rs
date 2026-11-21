// How big the heap is, decided from plain numbers: when a full semispace collects, when it grows the heap, when it shrinks it, and
// to what size. `Heap::collect_for_alloc` and `GarbageCollector::calculate_new_heap_size` call these functions and hold no copy of
// the rules. Verus checks this very file: `//@` lines hold the proof (the method of LucinaOS `scripts/verify-shipped.sh`; the
// mechanical steps are in `verification/gc-sizing/mutants.sh`).
//
// The policy is SpiderMonkey's, with smaller factors (see `MAX_FACTOR_TENTHS`). After a collection that left `live` bytes, bump
// allocation may go on up to `live` times a growth factor (2.0 up to 100 MiB, 1.5 from 500 MiB, linear in between), at least
// the semispace of the minimum heap, at most the
// semispace. The heap grows when the live data plus the allocation do not fit in the semispace (before a collection: when the
// grown live data plus the allocation do not), and halves when the grown live data plus the allocation fit in the semispace of
// the halved heap. Sizes are powers of two between the minimum and the maximum heap size, and every sum saturates and every
// product is bounded, so nothing here can overflow.
//
// Trusted, not proved:
//  - The sizes of the semispaces are the nominal ones, `semispace_of(heap, overhead)` = (heap - overhead) / 2. The real layout
//    (`Heap::init_semispaces`) rounds each semispace down to the 8-byte item alignment, and `overhead` (permanent space + HeapInfo
//    + one alignment unit) is taken as large enough to cover what that rounding and the header take. The grown live data is at
//    least 1.5 times the live data, so the room left by a shrink is not tight to the byte.
//  - The callers pass the heap's own numbers: the heap size is a power of two between the minimum and the maximum heap size
//    (`Options` checks the two limits and the heap starts at the minimum), and live data fits in the semispace.
//    A semispace is below the largest number (it is under half a heap size): needs_room, grow_before_collect and
//    resize_after_collect ask for it, so that a saturated sum is still above it.
//  - The callers' own loop (run a collection, look at the heap again), the collections themselves and the allocation of the new heap.
//  - That SMALL_HEAP, LARGE_HEAP and the factors 30 and 15 are SpiderMonkey's values; the proofs state what they are, not why.

//@ global size_of usize == 8;

/// What a full semispace does after the normal collection that made room: nothing more, grow the heap, or halve it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum Resize {
    Normal,
    Grow,
    Shrink,
}

//@ /// n is 1, 2, 4, 8, ...
//@ pub open spec fn is_pow2(n: nat) -> bool
//@ decreases n,
//@ {
//@     if n == 1 { true } else if n >= 2 && n % 2 == 0 { is_pow2((n / 2) as nat) } else { false }
//@ }

//@ /// One semispace of a heap of size `h` of which `o` bytes are the header, the permanent space and the alignment.
//@ pub open spec fn semispace_of(h: int, o: int) -> int { if h > o { (h - o) / 2 } else { 0 } }

//@ pub open spec fn imin(a: int, b: int) -> int { if a < b { a } else { b } }
//@ pub open spec fn imax(a: int, b: int) -> int { if a > b { a } else { b } }

//@ /// How far the factor has fallen below its largest value at the live size `l`, between `SMALL_HEAP` and `LARGE_HEAP`.
//@ pub open spec fn fall_spec(l: int) -> int {
//@     ((MAX_FACTOR_TENTHS as int - MIN_FACTOR_TENTHS as int) * (l - SMALL_HEAP as int))
//@         / (LARGE_HEAP as int - SMALL_HEAP as int)
//@ }

//@ /// The growth factor in tenths: the largest up to `SMALL_HEAP`, the smallest from `LARGE_HEAP`, linear in between (rounded down).
//@ pub open spec fn factor_spec(l: int) -> int {
//@     if l <= SMALL_HEAP as int {
//@         MAX_FACTOR_TENTHS as int
//@     } else if l >= LARGE_HEAP as int {
//@         MIN_FACTOR_TENTHS as int
//@     } else {
//@         MAX_FACTOR_TENTHS as int - fall_spec(l)
//@     }
//@ }

//@ /// The live data times the growth factor, the largest number when `live * MAX_FACTOR_TENTHS` does not fit.
//@ pub open spec fn grown_spec(l: int) -> int {
//@     if l > usize::MAX as int / MAX_FACTOR_TENTHS as int { usize::MAX as int } else { l * factor_spec(l) / 10 }
//@ }

//@ /// The grown live data, and at least the minimum window `mw`.
//@ pub open spec fn window_spec(l: int, mw: int) -> int { imax(grown_spec(l), mw) }

//@ /// The live data times `LOW_FACTOR_TENTHS`, the largest number when `live * LOW_FACTOR_TENTHS` does not fit.
//@ pub open spec fn low_grown_spec(l: int) -> int {
//@     if l > usize::MAX as int / LOW_FACTOR_TENTHS as int { usize::MAX as int } else { l * LOW_FACTOR_TENTHS as int / 10 }
//@ }

//@ /// The window when collections are infrequent: the low-frequency grown live data, and at least the minimum window `mw`.
//@ pub open spec fn low_window_spec(l: int, mw: int) -> int { imax(low_grown_spec(l), mw) }

//@ /// The window (`window_spec` while collections are frequent, `low_window_spec` otherwise), at least the live data plus the
//@ /// allocation (saturated), at most the semispace `s`.
//@ pub open spec fn alloc_limit_spec(l: int, a: int, s: int, mw: int, hf: bool) -> int {
//@     imin(imax(if hf { window_spec(l, mw) } else { low_window_spec(l, mw) }, imin(l + a, usize::MAX as int)), s)
//@ }

//@ /// The live data plus the allocation leave no room in the semispace `s`: the heap must grow.
//@ pub open spec fn grow_cond(live: int, alloc: int, s: int) -> bool { live + alloc > s }

//@ /// The grown live data plus the allocation do not fit in the semispace `s`: the next collection must grow the heap.
//@ pub open spec fn needs_room_cond(live: int, alloc: int, s: int) -> bool { grown_spec(live) + alloc > s }

//@ /// The heap `h` can halve and stay at `min` or more, and the semispace after halving holds the grown live data plus the
//@ /// allocation.
//@ pub open spec fn shrink_cond(live: int, alloc: int, h: int, o: int, min: int) -> bool {
//@     h / 2 >= min && grown_spec(live) + alloc <= semispace_of(h / 2, o)
//@ }

//@ proof fn pow2_pos(n: nat)
//@     requires is_pow2(n),
//@     ensures n >= 1,
//@     decreases n,
//@ {
//@     if n != 1 { pow2_pos((n / 2) as nat); }
//@ }

//@ proof fn pow2_half(n: nat)
//@     requires is_pow2(n), n > 1,
//@     ensures n % 2 == 0, is_pow2((n / 2) as nat),
//@ {
//@ }

//@ proof fn pow2_double(n: nat)
//@     requires is_pow2(n),
//@     ensures is_pow2((2 * n) as nat),
//@ {
//@     pow2_pos(n);
//@     assert((2 * n) / 2 == n) by (nonlinear_arith) requires n >= 1;
//@ }

//@ /// A power of two below another is at most half of it.
//@ proof fn pow2_lt_double(a: nat, b: nat)
//@     requires is_pow2(a), is_pow2(b), a < b,
//@     ensures 2 * a <= b,
//@     decreases b,
//@ {
//@     pow2_pos(a);
//@     pow2_half(b);
//@     if a > 1 {
//@         pow2_half(a);
//@         pow2_lt_double((a / 2) as nat, (b / 2) as nat);
//@     }
//@ }

//@ /// The largest number is not a power of two, so a power of two that fits a `usize` is below it.
//@ proof fn pow2_below_max(n: nat)
//@     requires is_pow2(n), n <= usize::MAX as nat,
//@     ensures n < usize::MAX as nat,
//@ {
//@     assert(!is_pow2(usize::MAX as nat));
//@ }

/// `a + b`, or the largest number when that does not fit.
pub fn sat_add(a: usize, b: usize) -> usize //@ret
//@ ensures ret as int == (if a + b <= usize::MAX { a + b } else { usize::MAX as int }),
{
    if a > usize::MAX - b { usize::MAX } else { a + b }
}

/// `a - b`, or 0 when `b` is the larger.
pub fn sat_sub(a: usize, b: usize) -> usize //@ret
//@ ensures ret as int == (if a >= b { a - b } else { 0 }),
{
    if a >= b { a - b } else { 0 }
}

/// The smallest power of two that is at least `n` and at most `cap`, a power of two that is at least `n`.
#[allow(unused_variables)] //@omit
pub fn pow2_at_least(n: usize, cap: usize) -> usize //@ret
//@ requires is_pow2(cap as nat), n <= cap,
//@ ensures is_pow2(ret as nat), n <= ret <= cap, ret == 1 || ret / 2 < n,
{
    let mut p: usize = 1;
    //@ proof { pow2_pos(cap as nat); }
    while p < n
    //@ invariant is_pow2(p as nat), is_pow2(cap as nat), n <= cap, 1 <= p <= cap, p == 1 || p / 2 < n,
    //@ decreases cap - p,
    {
        //@ proof { pow2_double(p as nat); pow2_lt_double(p as nat, cap as nat); }
        p = p * 2;
    }
    p
}

/// Live data up to which SpiderMonkey grows its tenured heap by the largest factor
/// (`gcHighFrequencySmallHeapSize`), and from which by the smallest (`gcHighFrequencyLargeHeapSize`).
pub const SMALL_HEAP: usize = 100 << 20;
pub const LARGE_HEAP: usize = 500 << 20;

/// The largest and the smallest growth factor in tenths: 2.0 and 1.5. SpiderMonkey uses 3.0
/// (`gcHighFrequencyHeapGrowthMax`) and 1.5 (`gcHighFrequencyHeapGrowthMin`) for its tenured heap,
/// which only holds what survived the nursery; this heap has no nursery, so its live data includes
/// the young objects too, and 3.0 let the YouTube watch page fill a 370 MB window for 127 MB of
/// live data (2.0: 250 MB, about the semispace-full trigger it replaces, without its doubled
/// copies). The proofs are written over these names; they only ask for
/// `10 <= MIN_FACTOR_TENTHS <= MAX_FACTOR_TENTHS <= 1000` (`factor_consts`).
pub const MAX_FACTOR_TENTHS: usize = 20;
pub const MIN_FACTOR_TENTHS: usize = 15;

//@ /// The two heap limits are the numbers they are spelled as.
//@ proof fn heap_consts()
//@     ensures SMALL_HEAP == 104857600usize, LARGE_HEAP == 524288000usize,
//@ {
//@     assert(100usize << 20 == 104857600usize) by (compute_only);
//@     assert(500usize << 20 == 524288000usize) by (compute_only);
//@ }

//@ /// The factors are ordered, not below 1.0 (so the grown live data is not below the live data) and not above 100.0 (so the
//@ /// interpolation cannot overflow). This is the one place that checks the two values.
//@ proof fn factor_consts()
//@     ensures 10 <= MIN_FACTOR_TENTHS, MIN_FACTOR_TENTHS <= MAX_FACTOR_TENTHS, MAX_FACTOR_TENTHS <= 1000,
//@ {
//@     assert(10 <= MIN_FACTOR_TENTHS && MIN_FACTOR_TENTHS <= MAX_FACTOR_TENTHS && MAX_FACTOR_TENTHS <= 1000) by (compute_only);
//@ }

//@ /// The low-frequency factor is not below 1.0 and not above the smallest high-frequency factor, so the low-frequency window is
//@ /// never above the high-frequency one. This is the one place that checks the value.
//@ proof fn low_consts()
//@     ensures 10 <= LOW_FACTOR_TENTHS, LOW_FACTOR_TENTHS <= MIN_FACTOR_TENTHS,
//@ {
//@     assert(10 <= LOW_FACTOR_TENTHS && LOW_FACTOR_TENTHS <= MIN_FACTOR_TENTHS) by (compute_only);
//@ }

//@ /// Between the two heap limits the fall of the factor is between 0 and the whole range, and its numerator fits a `usize`.
//@ proof fn interp_bounds(l: int)
//@     requires (SMALL_HEAP as int) < l, l < (LARGE_HEAP as int),
//@     ensures 0 <= fall_spec(l) <= MAX_FACTOR_TENTHS as int - MIN_FACTOR_TENTHS as int,
//@         0 <= (MAX_FACTOR_TENTHS as int - MIN_FACTOR_TENTHS as int) * (l - SMALL_HEAP as int) <= usize::MAX as int,
//@ {
//@     heap_consts();
//@     factor_consts();
//@     let d = MAX_FACTOR_TENTHS as int - MIN_FACTOR_TENTHS as int;
//@     let s = SMALL_HEAP as int;
//@     let dd = LARGE_HEAP as int - s;
//@     assert(d * (l - s) <= d * dd) by (nonlinear_arith) requires d >= 0, l - s <= dd, l - s >= 0;
//@     assert(d * (l - s) >= 0) by (nonlinear_arith) requires d >= 0, l - s >= 0;
//@     assert(d * dd <= 1000 * dd) by (nonlinear_arith) requires d <= 1000, dd >= 0;
//@     vstd::arithmetic::div_mod::lemma_div_is_ordered(d * (l - s), d * dd, dd);
//@     vstd::arithmetic::div_mod::lemma_div_multiples_vanish(d, dd);
//@ }

//@ /// The factor stays between the smallest and the largest, and takes the two ends outside the interpolated stretch.
//@ proof fn factor_bounds(l: int)
//@     ensures MIN_FACTOR_TENTHS as int <= factor_spec(l) <= MAX_FACTOR_TENTHS as int,
//@         l <= SMALL_HEAP as int ==> factor_spec(l) == MAX_FACTOR_TENTHS as int,
//@         l >= LARGE_HEAP as int ==> factor_spec(l) == MIN_FACTOR_TENTHS as int,
//@ {
//@     heap_consts();
//@     factor_consts();
//@     if (SMALL_HEAP as int) < l && l < (LARGE_HEAP as int) { interp_bounds(l); }
//@ }

//@ /// A bigger heap never gets a bigger factor.
//@ pub proof fn factor_antitone(a: int, b: int)
//@     requires a <= b,
//@     ensures factor_spec(a) >= factor_spec(b),
//@ {
//@     heap_consts();
//@     factor_consts();
//@     factor_bounds(a);
//@     factor_bounds(b);
//@     if a > SMALL_HEAP as int && b < LARGE_HEAP as int {
//@         let d = MAX_FACTOR_TENTHS as int - MIN_FACTOR_TENTHS as int;
//@         let s = SMALL_HEAP as int;
//@         assert(d * (a - s) <= d * (b - s)) by (nonlinear_arith) requires d >= 0, a <= b;
//@         vstd::arithmetic::div_mod::lemma_div_is_ordered(d * (a - s), d * (b - s), LARGE_HEAP as int - s);
//@     }
//@ }

//@ /// A live size and a factor that keep `live * factor` in a `usize`.
//@ proof fn mul_fits(l: int, f: int)
//@     requires 0 <= l <= usize::MAX as int / MAX_FACTOR_TENTHS as int, 0 <= f <= MAX_FACTOR_TENTHS as int,
//@     ensures l * f <= usize::MAX as int,
//@ {
//@     factor_consts();
//@     let m = MAX_FACTOR_TENTHS as int;
//@     let q = usize::MAX as int / m;
//@     vstd::arithmetic::div_mod::lemma_fundamental_div_mod(usize::MAX as int, m);
//@     vstd::arithmetic::div_mod::lemma_mod_bound(usize::MAX as int, m);
//@     assert(l * f <= l * m) by (nonlinear_arith) requires f <= m, l >= 0;
//@     assert(l * m <= q * m) by (nonlinear_arith) requires l <= q, m >= 0;
//@ }

//@ /// A number up to the largest number divided by `m` times `m` fits a `usize`.
//@ proof fn times_fits(l: int, m: int)
//@     requires m >= 1, 0 <= l <= usize::MAX as int / m,
//@     ensures l * m <= usize::MAX as int,
//@ {
//@     let q = usize::MAX as int / m;
//@     vstd::arithmetic::div_mod::lemma_fundamental_div_mod(usize::MAX as int, m);
//@     vstd::arithmetic::div_mod::lemma_mod_bound(usize::MAX as int, m);
//@     let r = usize::MAX as int % m;
//@     assert(q * m <= usize::MAX as int) by (nonlinear_arith) requires m * q + r == usize::MAX as int, r >= 0;
//@     assert(l * m <= q * m) by (nonlinear_arith) requires l <= q, m >= 0;
//@ }

//@ /// A product that fits a `usize` has a first factor up to the largest number divided by the second.
//@ proof fn fits_times(l: int, m: int)
//@     requires m >= 1, 0 <= l, l * m <= usize::MAX as int,
//@     ensures l <= usize::MAX as int / m,
//@ {
//@     vstd::arithmetic::div_mod::lemma_div_is_ordered(l * m, usize::MAX as int, m);
//@     vstd::arithmetic::div_mod::lemma_div_multiples_vanish(l, m);
//@     assert(l * m == m * l) by (nonlinear_arith);
//@ }

//@ /// The grown live data is at least the live data and fits in a `usize`.
//@ pub proof fn grown_bounds(l: int)
//@     requires 0 <= l <= usize::MAX as int,
//@     ensures l <= grown_spec(l) <= usize::MAX as int,
//@ {
//@     factor_bounds(l);
//@     factor_consts();
//@     if l <= usize::MAX as int / MAX_FACTOR_TENTHS as int {
//@         let f = factor_spec(l);
//@         mul_fits(l, f);
//@         assert(l * f >= 10 * l) by (nonlinear_arith) requires f >= 10, l >= 0;
//@         vstd::arithmetic::div_mod::lemma_div_is_ordered(10 * l, l * f, 10);
//@         vstd::arithmetic::div_mod::lemma_div_multiples_vanish(l, 10);
//@     }
//@ }

/// SpiderMonkey's heap growth factor in tenths: `MAX_FACTOR_TENTHS` for live data up to `SMALL_HEAP`, `MIN_FACTOR_TENTHS`
/// from `LARGE_HEAP`, linear in between.
pub fn growth_factor_tenths(live: usize) -> usize //@ret
//@ ensures ret as int == factor_spec(live as int), MIN_FACTOR_TENTHS <= ret <= MAX_FACTOR_TENTHS,
//@     live <= SMALL_HEAP ==> ret == MAX_FACTOR_TENTHS,
//@     live >= LARGE_HEAP ==> ret == MIN_FACTOR_TENTHS,
{
    //@ proof { heap_consts(); factor_consts(); if live > SMALL_HEAP && live < LARGE_HEAP { interp_bounds(live as int); } }
    if live <= SMALL_HEAP {
        MAX_FACTOR_TENTHS
    } else if live >= LARGE_HEAP {
        MIN_FACTOR_TENTHS
    } else {
        MAX_FACTOR_TENTHS - ((MAX_FACTOR_TENTHS - MIN_FACTOR_TENTHS) * (live - SMALL_HEAP)) / (LARGE_HEAP - SMALL_HEAP)
    }
}

/// The semispace of the minimum heap: the smallest allocation window.
pub fn min_window(min_heap: usize, overhead: usize) -> usize //@ret
//@ ensures ret as int == semispace_of(min_heap as int, overhead as int),
{
    sat_sub(min_heap, overhead) / 2
}

/// The live data times the growth factor: how big the heap's used part may get before the next collection, as SpiderMonkey
/// sizes its heap.
pub fn grown(live: usize) -> usize //@ret
//@ ensures ret as int == grown_spec(live as int), live <= ret,
{
    //@ proof {
    //@     factor_consts();
    //@     factor_bounds(live as int);
    //@     grown_bounds(live as int);
    //@     if live <= usize::MAX / MAX_FACTOR_TENTHS { mul_fits(live as int, factor_spec(live as int)); }
    //@ }
    if live > usize::MAX / MAX_FACTOR_TENTHS { usize::MAX } else { live * growth_factor_tenths(live) / 10 }
}

/// How far the semispace may fill before the next collection: `grown`, and at least `min_window`, so that a small heap does not
/// collect more often than a full semispace would.
pub fn window(live: usize, min_window: usize) -> usize //@ret
//@ ensures ret as int == window_spec(live as int, min_window as int), ret >= min_window, ret >= grown_spec(live as int),
{
    let g = grown(live);
    if g > min_window { g } else { min_window }
}

/// SpiderMonkey's growth factor in tenths when collections are infrequent (`gcLowFrequencyHeapGrowth`, 1.5).
pub const LOW_FACTOR_TENTHS: usize = 15;

/// Collections less than this far apart are frequent (`gcHighFrequencyTime`, 1 s): the program is building up its data (a page
/// load), and the window grows by `growth_factor_tenths`; otherwise by `LOW_FACTOR_TENTHS`, so a steady heap stays smaller.
pub const HIGH_FREQUENCY_MS: u64 = 1000;

/// The window when collections are infrequent: the live data times `LOW_FACTOR_TENTHS`, and at least `min_window`.
pub fn low_frequency_window(live: usize, min_window: usize) -> usize //@ret
//@ ensures ret as int == low_window_spec(live as int, min_window as int), ret >= min_window, ret >= live,
//@     ret as int <= window_spec(live as int, min_window as int),
{
    //@ proof {
    //@     low_consts();
    //@     factor_consts();
    //@     factor_bounds(live as int);
    //@     grown_bounds(live as int);
    //@     if live <= usize::MAX / LOW_FACTOR_TENTHS {
    //@         times_fits(live as int, LOW_FACTOR_TENTHS as int);
    //@         assert(live * LOW_FACTOR_TENTHS >= 10 * live) by (nonlinear_arith) requires LOW_FACTOR_TENTHS >= 10, live >= 0;
    //@         vstd::arithmetic::div_mod::lemma_div_is_ordered(10 * live as int, live * LOW_FACTOR_TENTHS as int, 10);
    //@         vstd::arithmetic::div_mod::lemma_div_multiples_vanish(live as int, 10);
    //@         if live <= usize::MAX / MAX_FACTOR_TENTHS {
    //@             let f = factor_spec(live as int);
    //@             times_fits(live as int, MAX_FACTOR_TENTHS as int);
    //@             assert(live * LOW_FACTOR_TENTHS <= live * f) by (nonlinear_arith) requires LOW_FACTOR_TENTHS <= f, live >= 0;
    //@             vstd::arithmetic::div_mod::lemma_div_is_ordered(live * LOW_FACTOR_TENTHS as int, live * f, 10);
    //@         }
    //@     } else {
    //@         if live <= usize::MAX / MAX_FACTOR_TENTHS {
    //@             times_fits(live as int, MAX_FACTOR_TENTHS as int);
    //@             assert(live * LOW_FACTOR_TENTHS <= live * MAX_FACTOR_TENTHS) by (nonlinear_arith) requires LOW_FACTOR_TENTHS <= MAX_FACTOR_TENTHS, live >= 0;
    //@             fits_times(live as int, LOW_FACTOR_TENTHS as int);
    //@         }
    //@     }
    //@ }
    let g = if live > usize::MAX / LOW_FACTOR_TENTHS { usize::MAX } else { live * LOW_FACTOR_TENTHS / 10 };
    if g > min_window { g } else { min_window }
}

/// Where bump allocation stops after a collection that left `live` bytes, before an allocation of `alloc` bytes, in bytes from
/// the start of the semispace: the window (`window` while collections are frequent, `low_frequency_window` otherwise), but at
/// least what the live data and the allocation take, and at most the semispace.
pub fn alloc_limit(live: usize, alloc: usize, semispace: usize, min_window: usize, high_frequency: bool) -> usize //@ret
//@ ensures ret as int == alloc_limit_spec(live as int, alloc as int, semispace as int, min_window as int, high_frequency),
//@     ret <= semispace,
//@     live as int + alloc as int <= semispace as int ==> ret as int >= live as int + alloc as int,
{
    let w = if high_frequency { window(live, min_window) } else { low_frequency_window(live, min_window) };
    let need = sat_add(live, alloc);
    let limit = if w > need { w } else { need };
    if limit > semispace { semispace } else { limit }
}

/// The semispace cannot hold the grown live data plus the allocation.
pub fn needs_room(live: usize, alloc: usize, semispace: usize) -> bool //@ret
//@ requires semispace < usize::MAX,
//@ ensures ret <==> needs_room_cond(live as int, alloc as int, semispace as int),
{
    sat_add(grown(live), alloc) > semispace
}

/// The size of one semispace after halving the heap (the permanent space moves along unchanged); `overhead` is the permanent
/// space, the header and one alignment unit.
pub fn shrunk_semispace(heap_size: usize, overhead: usize) -> usize //@ret
//@ ensures ret as int == semispace_of((heap_size / 2) as int, overhead as int),
{
    sat_sub(heap_size / 2, overhead) / 2
}

/// Before collecting for an allocation of `alloc` bytes: grow in this very collection, because the live size after the last
/// collection already says the heap is too small? (Else collect normally and look again with `resize_after_collect`.)
pub fn grow_before_collect(
    heap_size: usize,
    max_heap: usize,
    collected_before: bool,
    last_live: usize,
    alloc: usize,
    semispace: usize,
) -> bool //@ret
//@ requires semispace < usize::MAX,
//@ ensures ret <==> (heap_size < max_heap && collected_before
//@     && needs_room_cond(last_live as int, alloc as int, semispace as int)),
{
    heap_size < max_heap && collected_before && needs_room(last_live, alloc, semispace)
}

/// After a normal collection left `live` bytes: grow now only when the allocation does not fit (a window that does not fit
/// grows at the next collection, through `grow_before_collect`, so that growing copies the heap once); shrink when the window
/// and the allocation fit in the semispace of half the heap (with the live data grown, not the minimum window, so that the
/// heap can shrink back to the minimum); else leave the heap.
pub fn resize_after_collect(
    heap_size: usize,
    max_heap: usize,
    min_heap: usize,
    live: usize,
    alloc: usize,
    semispace: usize,
    overhead: usize,
) -> Resize //@ret
//@ requires semispace < usize::MAX,
//@ ensures ret == Resize::Grow <==> (heap_size < max_heap && grow_cond(live as int, alloc as int, semispace as int)),
//@     ret == Resize::Shrink <==> (!(heap_size < max_heap && grow_cond(live as int, alloc as int, semispace as int))
//@         && shrink_cond(live as int, alloc as int, heap_size as int, overhead as int, min_heap as int)),
{
    let can_grow = heap_size < max_heap;
    if can_grow && sat_add(live, alloc) > semispace {
        Resize::Grow
    } else if heap_size / 2 >= min_heap
        && sat_add(grown(live), alloc) <= shrunk_semispace(heap_size, overhead)
    {
        Resize::Shrink
    } else {
        Resize::Normal
    }
}

/// The first answer for the heap size of a growing collection that was asked for after an allocation of `alloc` bytes: double, or
/// when the allocation is more than half of the heap the next power of two above twice the allocation; never above `max_heap`.
/// It looks at the allocation only; `grow_size` makes room for the live data too.
pub fn grow_base(heap_size: usize, alloc: Option<usize>, max_heap: usize) -> usize //@ret
//@ requires is_pow2(heap_size as nat), is_pow2(max_heap as nat), heap_size <= max_heap,
//@ ensures is_pow2(ret as nat), heap_size <= ret <= max_heap,
//@     ret >= (if heap_size < max_heap { 2 * heap_size } else { heap_size as int }),
//@     (match alloc { Some(a) => 2 * (a as int) <= heap_size, None => true })
//@         ==> ret as int == (if heap_size < max_heap { 2 * heap_size } else { heap_size as int }),
//@     match alloc {
//@         Some(a) => 2 * (a as int) > heap_size ==> ret / 2 < 2 * (a as int)
//@             && ret >= (if 2 * (a as int) <= max_heap { 2 * (a as int) } else { max_heap as int }),
//@         None => true,
//@     },
{
    let doubled = if heap_size <= max_heap / 2 { heap_size * 2 } else { max_heap };
    //@ proof { if heap_size < max_heap { pow2_lt_double(heap_size as nat, max_heap as nat); } }
    match alloc {
        Some(a) => {
            let twice = sat_add(a, a);
            if twice > heap_size {
                if twice > max_heap {
                    max_heap
                } else {
                    //@ proof { pow2_pos(max_heap as nat); }
                    let rounded = pow2_at_least(twice, max_heap);
                    //@ proof { pow2_lt_double(heap_size as nat, rounded as nat); }
                    rounded
                }
            } else {
                doubled
            }
        }
        None => doubled,
    }
}

//@ /// The bytes of the allocation, 0 when none is named.
//@ pub open spec fn alloc_of(alloc: Option<usize>) -> int { match alloc { Some(a) => a as int, None => 0 } }

/// The heap size of a growing collection after an allocation of `alloc` bytes with `live` bytes live: the smallest power of two
/// that is at least `grow_base`'s answer and whose semispace, `(size - overhead) / 2`, holds the grown live data and the
/// allocation; `max_heap` when even that is too small.
pub fn grow_size(
    heap_size: usize,
    live: usize,
    alloc: Option<usize>,
    overhead: usize,
    max_heap: usize,
) -> usize //@ret
//@ requires is_pow2(heap_size as nat), is_pow2(max_heap as nat), heap_size <= max_heap,
//@ ensures is_pow2(ret as nat), heap_size <= ret <= max_heap,
//@     ret >= (if heap_size < max_heap { 2 * heap_size } else { heap_size as int }),
//@     (match alloc { Some(a) => 2 * (a as int) > heap_size, None => false })
//@         ==> ret >= (if 2 * alloc_of(alloc) <= max_heap { 2 * alloc_of(alloc) } else { max_heap as int }),
//@     semispace_of(max_heap as int, overhead as int) >= grown_spec(live as int) + alloc_of(alloc)
//@         ==> semispace_of(ret as int, overhead as int) >= grown_spec(live as int) + alloc_of(alloc),
//@     ret == (if heap_size < max_heap { 2 * heap_size } else { heap_size as int })
//@         || ret / 2 < 2 * alloc_of(alloc)
//@         || semispace_of((ret / 2) as int, overhead as int) < grown_spec(live as int) + alloc_of(alloc),
{
    let base = grow_base(heap_size, alloc, max_heap);
    let held = grown(live);
    let need = match alloc {
        Some(a) => sat_add(held, a),
        None => held,
    };
    if need == 0 {
        return base;
    }
    // A semispace of `(size - overhead) / 2` holds `need` bytes when `size >= overhead + 2 * need`.
    let target = sat_add(overhead, sat_add(need, need));
    //@ proof { pow2_below_max(max_heap as nat); }
    if target <= base {
        base
    } else if target > max_heap {
        max_heap
    } else {
        pow2_at_least(target, max_heap)
    }
}

/// The heap size of a shrinking collection: half of it, not below `min_heap`; `None` when that is not smaller.
pub fn shrink_size(heap_size: usize, min_heap: usize) -> Option<usize> //@ret
//@ requires is_pow2(heap_size as nat), is_pow2(min_heap as nat), min_heap <= heap_size,
//@ ensures match ret {
//@     Some(n) => is_pow2(n as nat) && min_heap <= n && n < heap_size && n == (if heap_size / 2 >= min_heap { heap_size / 2 } else { min_heap }),
//@     None => heap_size == min_heap,
//@ },
//@     heap_size / 2 >= min_heap ==> ret == Some((heap_size / 2) as usize),
{
    let n = if heap_size / 2 >= min_heap { heap_size / 2 } else { min_heap };
    //@ proof { pow2_pos(min_heap as nat); if heap_size / 2 >= min_heap { pow2_half(heap_size as nat); } }
    if n < heap_size { Some(n) } else { None }
}

//@ /// A grow decided because the live data plus the allocation do not fit is also one the grown live data do not fit in.
//@ pub proof fn grow_cond_needs_room(live: int, alloc: int, s: int)
//@     requires 0 <= live <= usize::MAX as int, grow_cond(live, alloc, s),
//@     ensures needs_room_cond(live, alloc, s),
//@ {
//@     grown_bounds(live);
//@ }

//@ /// After a growing collection the shrink condition is false for the same live data and allocation. `s` is the semispace the
//@ /// grow was decided on (`needs_room_cond`, or `grow_cond` through `grow_cond_needs_room`); `ret` is what `grow_size`
//@ /// answered (its last ensures) for a heap `h` below the maximum `mx`.
//@ pub proof fn no_shrink_right_after_grow(live: int, alloc: int, s: int, h: int, mx: int, o: int, min: int, ret: int)
//@     requires 0 <= live <= usize::MAX as int, alloc >= 0, o >= 0, h >= 1, h < mx, ret >= h, s == semispace_of(h, o), needs_room_cond(live, alloc, s),
//@         ret == (if h < mx { 2 * h } else { h }) || ret / 2 < 2 * alloc || semispace_of(ret / 2, o) < grown_spec(live) + alloc,
//@     ensures !shrink_cond(live, alloc, ret, o, min),
//@ {
//@     grown_bounds(live);
//@ }

//@ /// After a shrinking collection neither the grow condition nor the one that grows before the next collection holds for the
//@ /// same live data and allocation, on the semispace the shrink made.
//@ pub proof fn no_grow_right_after_shrink(live: int, alloc: int, h: int, o: int, min: int)
//@     requires 0 <= live <= usize::MAX as int, alloc >= 0, shrink_cond(live, alloc, h, o, min),
//@     ensures !grow_cond(live, alloc, semispace_of(h / 2, o)), !needs_room_cond(live, alloc, semispace_of(h / 2, o)),
//@ {
//@     grown_bounds(live);
//@ }

//@ /// A shrink leaves a semispace that holds the grown live data plus the allocation (so the copy fits, and so does the next
//@ /// allocation limit, which is the least of the window and that semispace and at least the live data plus the allocation).
//@ pub proof fn shrink_fits(live: int, alloc: int, h: int, o: int, min: int, mw: int, hf: bool)
//@     requires 0 <= live <= usize::MAX as int, alloc >= 0, o >= 0, h <= usize::MAX as int, shrink_cond(live, alloc, h, o, min),
//@     ensures semispace_of(h / 2, o) >= grown_spec(live) + alloc, semispace_of(h / 2, o) >= live + alloc, h / 2 >= min,
//@         alloc_limit_spec(live, alloc, semispace_of(h / 2, o), mw, hf) >= live + alloc,
//@         alloc_limit_spec(live, alloc, semispace_of(h / 2, o), mw, hf) <= semispace_of(h / 2, o),
//@ {
//@     grown_bounds(live);
//@ }

//@ /// A bigger heap has a bigger semispace.
//@ pub proof fn grow_fits(h: int, o: int, ret: int)
//@     requires o >= 0, 0 <= h <= ret,
//@     ensures semispace_of(ret, o) >= semispace_of(h, o),
//@ {
//@ }
