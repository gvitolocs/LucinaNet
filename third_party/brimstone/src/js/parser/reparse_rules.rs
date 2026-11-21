// What the analyzer keeps of its function stack for parsing a lazy function again on its own (`parser::reparse`), and how one
// stack entry is packed. `Analyzer::record_reparse_snapshot` and `analyze_reparsed_function` call these functions and hold no
// copy of the rules. Verus checks this very file: `//@` lines hold the proof (the method of LucinaOS `scripts/verify-shipped.sh`;
// the mechanical steps are in `verification/reparse/mutants.sh`).
//
// The analyzer asks its function stack three things: the top entry (`current_function`, and the `last()` read for `this` in
// direct eval), the nearest entry from the top that is not an arrow function (`enclosing_non_arrow_function`), and whether it is
// empty (`is_at_top_level`, with the class stack). The kept chain is the top of the stack down to and including that nearest
// non-arrow entry (the whole stack if every entry is an arrow). Proved: the chain answers all three questions as the full stack
// does, and a chain longer than the snapshot holds is refused (the function is then generated before its AST is dropped, never
// re-parsed with a cut chain). A packed entry gives back each of its six flags.
//
// Trusted, not proved:
//  - The three questions are the only reads of the function stack while the analyzer works (checked by reading analyze.rs:
//    `function_stack` is read in `enclosing_non_arrow_function`, `current_function`, `is_at_top_level` and one `last()` in the
//    direct eval case, which is `current_function`; the rest are pushes and pops of the analyzer's own entries).
//  - The re-parsed function pushes and pops its own entry exactly as the full analysis did, on top of the chain pushed back.
//  - `record_reparse_snapshot` copies the entries of the kept chain in order, each through `entry_bits`, and
//    `analyze_reparsed_function` pushes them back in order, each through `entry_flag`: a loop over a slice, not proved here.

//@ /// The index of the nearest non-arrow entry among the first `i + 1` entries of `arrows` (true = arrow), searching from
//@ /// index `i` down.
//@ pub open spec fn nearest_from(arrows: Seq<bool>, i: int) -> Option<int>
//@     decreases i + 1,
//@ {
//@     if i < 0 { None } else if !arrows[i] { Some(i) } else { nearest_from(arrows, i - 1) }
//@ }

//@ pub open spec fn nearest(arrows: Seq<bool>) -> Option<int> { nearest_from(arrows, arrows.len() - 1) }

//@ /// The search result is in range, is a non-arrow entry, and every entry above it (or every entry, if none) is an arrow.
//@ proof fn nearest_in_range(arrows: Seq<bool>, i: int)
//@     requires i < arrows.len(),
//@     ensures
//@         match nearest_from(arrows, i) { Some(j) => 0 <= j <= i && !arrows[j], None => true },
//@         forall|k: int| #![auto] (match nearest_from(arrows, i) { Some(j) => j < k <= i, None => 0 <= k <= i }) ==> arrows[k],
//@     decreases i + 1,
//@ {
//@     if i >= 0 && arrows[i] { nearest_in_range(arrows, i - 1); }
//@ }

//@ /// The search in the suffix starting at `s` finds the same entry, shifted by `s`, when the entry is in the suffix.
//@ proof fn nearest_in_suffix(arrows: Seq<bool>, s: int, i: int)
//@     requires 0 <= s <= arrows.len(), s - 1 <= i < arrows.len(),
//@         match nearest_from(arrows, i) { Some(j) => j >= s, None => s == 0 },
//@     ensures nearest_from(arrows.subrange(s, arrows.len() as int), i - s)
//@         == (match nearest_from(arrows, i) { Some(j) => Some(j - s), None => None::<int> }),
//@     decreases i + 1,
//@ {
//@     nearest_in_range(arrows, i);
//@     if i >= s {
//@         let sub = arrows.subrange(s, arrows.len() as int);
//@         assert(sub[i - s] == arrows[i]);
//@         if arrows[i] { nearest_in_suffix(arrows, s, i - 1); }
//@     }
//@ }

/// Most function stack entries a snapshot holds (`reparse::MAX_FUNCTION_CHAIN`).
pub const MAX_CHAIN: usize = 8;

/// Where the kept chain starts in a function stack whose entries are arrow functions or not (`arrows`, bottom first): the
/// nearest non-arrow entry from the top, or 0 if there is none. None if the chain would be longer than `MAX_CHAIN`.
pub fn chain_start(arrows: &[bool]) -> Option<usize> //@ret
//@ ensures match ret {
//@     Some(s) => s <= arrows.len() && arrows.len() - s <= MAX_CHAIN
//@         && (match nearest(arrows@) { Some(j) => s == j, None => s == 0 }),
//@     None => (match nearest(arrows@) { Some(j) => arrows.len() - j > MAX_CHAIN, None => arrows.len() > MAX_CHAIN }),
//@ },
{
    let mut i = arrows.len();
    while i > 0 && arrows[i - 1]
    //@ invariant i <= arrows.len(),
    //@     nearest(arrows@) == nearest_from(arrows@, i - 1),
    //@ decreases i,
    {
        i = i - 1;
    }
    let start = if i > 0 { i - 1 } else { 0 };
    if arrows.len() - start > MAX_CHAIN {
        None
    } else {
        Some(start)
    }
}

//@ /// The chain kept from `arrows` answers the analyzer's three questions as the whole stack does: the top entry is the same
//@ /// entry, the nearest non-arrow entry from the top is the same entry (shifted), and the chain is empty exactly when the
//@ /// stack is.
//@ proof fn chain_answers_like_the_stack(arrows: Seq<bool>, s: int)
//@     requires 0 <= s <= arrows.len(),
//@         match nearest(arrows) { Some(j) => s == j, None => s == 0 },
//@     ensures ({
//@         let chain = arrows.subrange(s, arrows.len() as int);
//@         &&& (chain.len() == 0) == (arrows.len() == 0)
//@         &&& (arrows.len() > 0 ==> chain[chain.len() - 1] == arrows[arrows.len() - 1])
//@         &&& nearest(chain) == (match nearest(arrows) { Some(j) => Some(j - s), None => None::<int> })
//@     }),
//@ {
//@     if arrows.len() > 0 { nearest_in_range(arrows, arrows.len() - 1); }
//@     nearest_in_suffix(arrows, s, arrows.len() - 1);
//@ }

//@ /// Flag `i` of a packed entry.
//@ pub open spec fn bit_of(b: u8, i: u8) -> bool { (b >> i) & 1u8 == 1u8 }

/// One analyzer function stack entry as bits: arrow, method, static, derived constructor, static initializer, class field
/// initializer, from bit 0 up.
pub fn entry_bits(f0: bool, f1: bool, f2: bool, f3: bool, f4: bool, f5: bool) -> u8 //@ret
//@ ensures ret < 64,
//@     bit_of(ret, 0) == f0, bit_of(ret, 1) == f1, bit_of(ret, 2) == f2,
//@     bit_of(ret, 3) == f3, bit_of(ret, 4) == f4, bit_of(ret, 5) == f5,
{
    let b0: u8 = if f0 { 1 } else { 0 };
    let b1: u8 = if f1 { 1 } else { 0 };
    let b2: u8 = if f2 { 1 } else { 0 };
    let b3: u8 = if f3 { 1 } else { 0 };
    let b4: u8 = if f4 { 1 } else { 0 };
    let b5: u8 = if f5 { 1 } else { 0 };
    let b = b0 | b1 << 1u8 | b2 << 2u8 | b3 << 3u8 | b4 << 4u8 | b5 << 5u8;
    //@ assert(b < 64 && (b >> 0u8) & 1u8 == b0 && (b >> 1u8) & 1u8 == b1 && (b >> 2u8) & 1u8 == b2
    //@     && (b >> 3u8) & 1u8 == b3 && (b >> 4u8) & 1u8 == b4 && (b >> 5u8) & 1u8 == b5) by (bit_vector)
    //@     requires b0 < 2, b1 < 2, b2 < 2, b3 < 2, b4 < 2, b5 < 2,
    //@         b == b0 | b1 << 1u8 | b2 << 2u8 | b3 << 3u8 | b4 << 4u8 | b5 << 5u8;
    b
}

/// Flag `i` (as numbered in `entry_bits`) of a packed entry.
pub fn entry_flag(bits: u8, i: u8) -> bool //@ret
//@ requires i < 8,
//@ ensures ret == bit_of(bits, i),
{
    (bits >> i) & 1 == 1
}

//@ /// Packing six flags and reading flag `k` back gives flag `k`, for every `k` below 6.
//@ proof fn flags_round_trip(f0: bool, f1: bool, f2: bool, f3: bool, f4: bool, f5: bool, bits: u8, k: u8)
//@     requires bit_of(bits, 0) == f0, bit_of(bits, 1) == f1, bit_of(bits, 2) == f2,
//@         bit_of(bits, 3) == f3, bit_of(bits, 4) == f4, bit_of(bits, 5) == f5, k < 6,
//@     ensures bit_of(bits, k) == (if k == 0 { f0 } else if k == 1 { f1 } else if k == 2 { f2 } else if k == 3 { f3 }
//@         else if k == 4 { f4 } else { f5 }),
//@ {
//@ }
