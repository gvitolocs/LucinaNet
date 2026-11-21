// `jsid` (SpiderMonkey's `PropertyKey`) on Brimstone: the bits are Brimstone's `PropertyKey`, itself a `Value`: an array index
// 0..=2^32-2 as an int32 holding the index's 32 bits, a string (interned, never an array index) or a symbol as a heap pointer.
// SpiderMonkey's int ids only go to 2^31-1, so indices from 2^31 have no int form for Servo: `jsid.rs` hands them out as
// strings and brings strings back through `parse_index_*`, the same rule Brimstone's `PropertyKey::string` applies.
// Verus checks this very file (`scripts/verify-shipped.sh`).
//
// Proved: the kinds are disjoint and cover every key; an int id and an index decode to what made them; the parsers accept
// exactly the canonical decimal spellings of 0..=2^32-2 (ECMA-262 "array index": CanonicalNumericString and < 2^32-1), for any
// input, and never overflow. Not proved: that Brimstone's own parser agrees (tests/id_bits_vs_brimstone.rs compares them).

use super::value_bits;

/// The largest array index, 2^32 - 2.
pub const MAX_INDEX: u32 = 0xFFFF_FFFE;
/// The largest int id SpiderMonkey has (`PropertyKey::IntMax`).
pub const MAX_INT_ID: i32 = 0x7FFF_FFFF;

/// `JSID_VOID`: Brimstone's empty value, which no property key is.
pub fn void_id() -> u64 //@ret
//@ ensures value_bits::tag_of(ret) == 0x8005,
{
    value_bits::empty()
}

/// The key of array index `i`.
pub fn index_id(i: u32) -> u64 //@ret
//@ requires i <= 0xFFFF_FFFE,
//@ ensures value_bits::tag_of(ret) == 0x8001, value_bits::low32(ret) == i as int,
{
    let as_i32: i32 = if i < 0x8000_0000 { i as i32 } else { (i as i64 - 0x1_0000_0000i64) as i32 };
    value_bits::int32(as_i32)
}

/// `PropertyKey::Int(i)`, `i` >= 0.
pub fn int_id(i: i32) -> u64 //@ret
//@ requires i >= 0,
//@ ensures value_bits::tag_of(ret) == 0x8001, value_bits::low32(ret) == i as int, is_int_spec(ret),
{
    value_bits::int32(i)
}

//@ pub open spec fn is_index_spec(bits: u64) -> bool { value_bits::tag_of(bits) == 0x8001 }
//@ pub open spec fn is_int_spec(bits: u64) -> bool { is_index_spec(bits) && value_bits::low32(bits) < 0x8000_0000 }

/// An array index of any size.
pub fn is_index(bits: u64) -> bool //@ret
//@ ensures ret == is_index_spec(bits),
{
    value_bits::tag(bits) == value_bits::INT32_TAG
}

/// An index Servo may see as an int id (0..=2^31-1).
pub fn is_int(bits: u64) -> bool //@ret
//@ ensures ret == is_int_spec(bits),
{
    //@ assert(bits & 0xFFFF_FFFF <= 0xFFFF_FFFF) by (bit_vector);
    is_index(bits) && (bits & 0xFFFF_FFFF) < 0x8000_0000
}

/// An index above SpiderMonkey's int range (2^31..=2^32-2): Servo sees it as a string.
pub fn is_high_index(bits: u64) -> bool //@ret
//@ ensures ret == (is_index_spec(bits) && !is_int_spec(bits)),
{
    //@ assert(bits & 0xFFFF_FFFF <= 0xFFFF_FFFF) by (bit_vector);
    is_index(bits) && (bits & 0xFFFF_FFFF) >= 0x8000_0000
}

/// The index an index key holds.
pub fn to_index(bits: u64) -> u32 //@ret
//@ requires is_index_spec(bits),
//@ ensures ret as int == value_bits::low32(bits),
{
    //@ assert(bits & 0xFFFF_FFFF <= 0xFFFF_FFFF) by (bit_vector);
    (bits & 0xFFFF_FFFF) as u32
}

/// The int id an int key holds.
pub fn to_int(bits: u64) -> i32 //@ret
//@ requires is_int_spec(bits),
//@ ensures ret as int == value_bits::low32(bits), ret >= 0,
{
    //@ assert(bits & 0xFFFF_FFFF <= 0xFFFF_FFFF) by (bit_vector);
    ((bits & 0xFFFF_FFFF) as u32) as i32
}

/// A string or symbol key: which of the two is in the heap item, not in the bits.
pub fn is_gcthing(bits: u64) -> bool //@ret
//@ ensures ret == (bits != 0 && value_bits::tag_of(bits) == 0),
{
    value_bits::is_traced(bits)
}

// ---- Canonical array-index strings, from untrusted text (property names a page chooses) ----

//@ pub open spec fn is_digit(c: int) -> bool { 0x30 <= c && c <= 0x39 }
//@ pub open spec fn all_digits(s: Seq<int>) -> bool { forall|k: int| 0 <= k < s.len() ==> is_digit(#[trigger] s[k]) }
//@ pub open spec fn value_of(s: Seq<int>) -> int
//@     decreases s.len(),
//@ {
//@     if s.len() == 0 { 0 } else { value_of(s.drop_last()) * 10 + (s.last() - 0x30) }
//@ }
//@ /// ECMA-262 array index: the canonical decimal spelling of a number below 2^32 - 1.
//@ pub open spec fn canonical_index(s: Seq<int>) -> bool {
//@     s.len() >= 1 && all_digits(s) && (s[0] != 0x30 || s.len() == 1) && value_of(s) < 0xFFFF_FFFF
//@ }
//@ pub open spec fn units8(s: Seq<u8>, n: int) -> Seq<int> { Seq::new(n as nat, |k: int| s[k] as int) }
//@ pub open spec fn units16(s: Seq<u16>, n: int) -> Seq<int> { Seq::new(n as nat, |k: int| s[k] as int) }
//@
//@ pub proof fn lemma_value_nonneg(s: Seq<int>)
//@     requires all_digits(s),
//@     ensures value_of(s) >= 0,
//@     decreases s.len(),
//@ {
//@     if s.len() > 0 {
//@         assert(all_digits(s.drop_last()));
//@         lemma_value_nonneg(s.drop_last());
//@         assert(is_digit(s[s.len() - 1]));
//@     }
//@ }
//@
//@ /// A longer all-digit prefix is worth at least as much as a shorter one.
//@ pub proof fn lemma_value_grows(s: Seq<int>, k: int)
//@     requires all_digits(s), 0 <= k <= s.len(),
//@     ensures value_of(s) >= value_of(s.subrange(0, k)),
//@     decreases s.len() - k,
//@ {
//@     if k < s.len() {
//@         let t = s.subrange(0, k + 1);
//@         assert(t.drop_last() =~= s.subrange(0, k));
//@         assert(all_digits(s.subrange(0, k)));
//@         lemma_value_nonneg(s.subrange(0, k));
//@         assert(is_digit(t[k]));
//@         lemma_value_grows(s, k + 1);
//@     } else {
//@         assert(s.subrange(0, k) =~= s);
//@     }
//@ }

/// `"123"` -> `Some(123)`; anything that is not an array index's canonical spelling -> `None` (`""`, `"01"`, `"-1"`, `"1.0"`,
/// `"4294967295"` and above).
pub fn parse_index_latin1(s: &[u8]) -> Option<u32> //@ret
//@ ensures match ret {
//@     Some(n) => canonical_index(units8(s@, s@.len() as int)) && value_of(units8(s@, s@.len() as int)) == n as int,
//@     None => !canonical_index(units8(s@, s@.len() as int)),
//@ },
{
    let len = s.len();
    if len == 0 {
        return None;
    }
    if s[0] == 0x30 && len > 1 {
        //@ assert(units8(s@, len as int)[0] == 0x30);
        return None;
    }
    let mut acc: u64 = 0;
    let mut i: usize = 0;
    while i < len
        //@ invariant
        //@     0 <= i <= len, len == s@.len(), len >= 1, !(s@[0] == 0x30 && len > 1),
        //@     all_digits(units8(s@, i as int)),
        //@     acc as int == value_of(units8(s@, i as int)),
        //@     acc < 0xFFFF_FFFF,
        //@ decreases len - i,
    {
        let c = s[i];
        if c < 0x30 || c > 0x39 {
            //@ assert(!is_digit(units8(s@, len as int)[i as int]));
            return None;
        }
        //@ assert(units8(s@, i as int + 1).drop_last() =~= units8(s@, i as int));
        //@ assert(all_digits(units8(s@, i as int + 1))) by {
        //@     let t = units8(s@, i as int + 1);
        //@     assert forall|k: int| 0 <= k < t.len() implies is_digit(#[trigger] t[k]) by {
        //@         if k < i as int { assert(t[k] == units8(s@, i as int)[k]); } else { assert(t[k] == c as int); }
        //@     }
        //@ }
        acc = acc * 10 + (c - 0x30) as u64;
        i = i + 1;
        if acc >= 0xFFFF_FFFF {
            //@ proof {
            //@     let whole = units8(s@, len as int);
            //@     if all_digits(whole) {
            //@         assert(whole.subrange(0, i as int) =~= units8(s@, i as int));
            //@         lemma_value_grows(whole, i as int);
            //@     }
            //@ }
            return None;
        }
    }
    //@ assert(units8(s@, i as int) =~= units8(s@, len as int));
    //@ assert(units8(s@, len as int)[0] == s@[0] as int);
    Some(acc as u32)
}

/// The same for two-byte strings.
pub fn parse_index_utf16(s: &[u16]) -> Option<u32> //@ret
//@ ensures match ret {
//@     Some(n) => canonical_index(units16(s@, s@.len() as int)) && value_of(units16(s@, s@.len() as int)) == n as int,
//@     None => !canonical_index(units16(s@, s@.len() as int)),
//@ },
{
    let len = s.len();
    if len == 0 {
        return None;
    }
    if s[0] == 0x30 && len > 1 {
        //@ assert(units16(s@, len as int)[0] == 0x30);
        return None;
    }
    let mut acc: u64 = 0;
    let mut i: usize = 0;
    while i < len
        //@ invariant
        //@     0 <= i <= len, len == s@.len(), len >= 1, !(s@[0] == 0x30 && len > 1),
        //@     all_digits(units16(s@, i as int)),
        //@     acc as int == value_of(units16(s@, i as int)),
        //@     acc < 0xFFFF_FFFF,
        //@ decreases len - i,
    {
        let c = s[i];
        if c < 0x30 || c > 0x39 {
            //@ assert(!is_digit(units16(s@, len as int)[i as int]));
            return None;
        }
        //@ assert(units16(s@, i as int + 1).drop_last() =~= units16(s@, i as int));
        //@ assert(all_digits(units16(s@, i as int + 1))) by {
        //@     let t = units16(s@, i as int + 1);
        //@     assert forall|k: int| 0 <= k < t.len() implies is_digit(#[trigger] t[k]) by {
        //@         if k < i as int { assert(t[k] == units16(s@, i as int)[k]); } else { assert(t[k] == c as int); }
        //@     }
        //@ }
        acc = acc * 10 + (c - 0x30) as u64;
        i = i + 1;
        if acc >= 0xFFFF_FFFF {
            //@ proof {
            //@     let whole = units16(s@, len as int);
            //@     if all_digits(whole) {
            //@         assert(whole.subrange(0, i as int) =~= units16(s@, i as int));
            //@         lemma_value_grows(whole, i as int);
            //@     }
            //@ }
            return None;
        }
    }
    //@ assert(units16(s@, i as int) =~= units16(s@, len as int));
    //@ assert(units16(s@, len as int)[0] == s@[0] as int);
    Some(acc as u32)
}
