// `JS::Value` on Brimstone: the 64 bits Servo holds are Brimstone's own NaN-boxed `Value`, so nothing is converted at the
// boundary. This file is the only place that knows the encoding on our side; `jsval.rs` calls it. Verus checks this very file
// (`scripts/verify-shipped.sh`): `//@` lines hold the proof.
//
// The encoding, from brimstone `src/js/runtime/value.rs` at 64d6698 (the original):
//   - heap pointers as they are: top 16 bits zero;
//   - doubles bitwise negated, NaN canonical (one quiet NaN);
//   - undefined, null, empty, bool, int32 ("smi") negated tags in the top 16 bits, payload in the low bits.
// tests/value_bits_vs_brimstone.rs compares every constructor here with Brimstone's own `Value` on the same inputs.
//
// Proved here: every constructor gives a value of the kind it claims, decoding gives back what went in, a double or a private
// pointer can never look like a heap pointer (so the GC never traces a Rust pointer or a number), and `kind` is a function of
// the bits, so the kinds are disjoint. Not proved: that Brimstone's own code uses the same encoding (the test above checks it),
// and the integral-double-to-int32 rule of `Value::number` (float arithmetic; jsval.rs, tested against Brimstone).

pub const TAG_SHIFT: u64 = 48;
pub const NAN_TAG: u64 = 0x7FF8;
/// `!(x | NAN_TAG)` on 16 bits, Brimstone's tag constants.
pub const UNDEFINED_TAG: u64 = 0x8006;
pub const NULL_TAG: u64 = 0x8004;
pub const EMPTY_TAG: u64 = 0x8005;
pub const BOOL_TAG: u64 = 0x8003;
pub const INT32_TAG: u64 = 0x8001;
/// `!(NAN_TAG << 48)`: the one NaN a value may hold.
pub const CANONICAL_NAN: u64 = 0x8007_FFFF_FFFF_FFFF;
/// Heap pointers and Rust pointers given to `PrivateValue` are below this.
pub const POINTER_LIMIT: u64 = 0x1_0000_0000_0000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// An all-zero word: an uninitialized slot. Never traced, never given to the engine.
    Zero,
    Pointer,
    Undefined,
    Null,
    Empty,
    Bool,
    Int32,
    Double,
}

//@ pub open spec fn tag_of(bits: u64) -> u64 { bits >> 48u64 }
//@ pub open spec fn is_double_bits(b: u64) -> bool {
//@     b != 0 && b >> 48u64 != 0 && b >> 48u64 != 0x8001 && b >> 48u64 != 0x8003 && b >> 48u64 != 0x8004
//@     && b >> 48u64 != 0x8005 && b >> 48u64 != 0x8006
//@ }

/// The top 16 bits.
#[inline]
pub fn tag(bits: u64) -> u64 //@ret
//@ ensures ret == tag_of(bits), ret < 0x10000,
{
    //@ assert(bits >> 48u64 < 0x10000) by (bit_vector);
    bits >> TAG_SHIFT
}

/// What the bits hold. A double is anything that is not one of the tagged kinds, as in Brimstone's `is_double`.
pub fn kind(bits: u64) -> Kind //@ret
//@ ensures
//@     (ret == Kind::Zero) == (bits == 0),
//@     (ret == Kind::Pointer) == (bits != 0 && tag_of(bits) == 0),
//@     (ret == Kind::Undefined) == (tag_of(bits) == 0x8006),
//@     (ret == Kind::Null) == (tag_of(bits) == 0x8004),
//@     (ret == Kind::Empty) == (tag_of(bits) == 0x8005),
//@     (ret == Kind::Bool) == (tag_of(bits) == 0x8003),
//@     (ret == Kind::Int32) == (tag_of(bits) == 0x8001),
//@     (ret == Kind::Double) == is_double_bits(bits),
{
    //@ assert(bits == 0 ==> bits >> 48u64 == 0) by (bit_vector);
    if bits == 0 {
        return Kind::Zero;
    }
    let t = tag(bits);
    if t == 0 {
        Kind::Pointer
    } else if t == UNDEFINED_TAG {
        Kind::Undefined
    } else if t == NULL_TAG {
        Kind::Null
    } else if t == EMPTY_TAG {
        Kind::Empty
    } else if t == BOOL_TAG {
        Kind::Bool
    } else if t == INT32_TAG {
        Kind::Int32
    } else {
        Kind::Double
    }
}

#[inline]
pub fn undefined() -> u64 //@ret
//@ ensures tag_of(ret) == 0x8006,
{
    //@ assert((0x8006u64 << 48u64) >> 48u64 == 0x8006) by (bit_vector);
    UNDEFINED_TAG << TAG_SHIFT
}

#[inline]
pub fn null() -> u64 //@ret
//@ ensures tag_of(ret) == 0x8004,
{
    //@ assert((0x8004u64 << 48u64) >> 48u64 == 0x8004) by (bit_vector);
    NULL_TAG << TAG_SHIFT
}

#[inline]
pub fn empty() -> u64 //@ret
//@ ensures tag_of(ret) == 0x8005,
{
    //@ assert((0x8005u64 << 48u64) >> 48u64 == 0x8005) by (bit_vector);
    EMPTY_TAG << TAG_SHIFT
}

#[inline]
pub fn boolean(b: bool) -> u64 //@ret
//@ ensures tag_of(ret) == 0x8003, (ret & 1 == 1) == b,
{
    let p: u64 = if b { 1 } else { 0 };
    //@ assert(p <= 1 ==> ((0x8003u64 << 48u64) | p) >> 48u64 == 0x8003) by (bit_vector);
    //@ assert(p <= 1 ==> (((0x8003u64 << 48u64) | p) & 1 == 1) == (p == 1)) by (bit_vector);
    (BOOL_TAG << TAG_SHIFT) | p
}

#[inline]
pub fn to_boolean(bits: u64) -> bool {
    bits & 1 != 0
}

/// An int32 ("smi"): the payload is the 32 bits of `i` as they are.
#[inline]
pub fn int32(i: i32) -> u64 //@ret
//@ ensures tag_of(ret) == 0x8001, low32(ret) == (if i >= 0 { i as int } else { i as int + 0x1_0000_0000 }),
{
    // `i as u32 as u64`, written so that each cast is in range.
    let p: u64 = if i >= 0 { i as u64 } else { (i as i64 + 0x1_0000_0000i64) as u64 };
    //@ assert(p < 0x1_0000_0000 ==> ((0x8001u64 << 48u64) | p) >> 48u64 == 0x8001) by (bit_vector);
    //@ assert(p < 0x1_0000_0000 ==> ((0x8001u64 << 48u64) | p) & 0xFFFF_FFFF == p) by (bit_vector);
    (INT32_TAG << TAG_SHIFT) | p
}

//@ pub open spec fn low32(bits: u64) -> int { (bits & 0xFFFF_FFFF) as int }

/// The int32 an int32 value holds: its low 32 bits as a signed number, as Brimstone's `as_smi`.
#[inline]
pub fn to_int32(bits: u64) -> i32 //@ret
//@ ensures ret as int == (if low32(bits) < 0x8000_0000 { low32(bits) } else { low32(bits) - 0x1_0000_0000 }),
{
    //@ assert(bits & 0xFFFF_FFFF <= 0xFFFF_FFFF) by (bit_vector);
    let lo = bits & 0xFFFF_FFFF;
    if lo < 0x8000_0000 { lo as i32 } else { (lo as i64 - 0x1_0000_0000i64) as i32 }
}

// Decoding an int32 gives back the int32: the two `ensures` above compose.
//@ pub proof fn lemma_int32_round_trip(i: i32, bits: u64)
//@     requires low32(bits) == (if i >= 0 { i as int } else { i as int + 0x1_0000_0000 }),
//@     ensures (if low32(bits) < 0x8000_0000 { low32(bits) } else { low32(bits) - 0x1_0000_0000 }) == i as int,
//@ {
//@ }

//@ pub open spec fn is_nan_bits(raw: u64) -> bool { (raw >> 52u64) & 0x7FF == 0x7FF && raw & 0xF_FFFF_FFFF_FFFF != 0 }

/// The value for the IEEE bits `raw` of a double, kept a double (no int32 rule here, see jsval.rs). Every NaN becomes the
/// canonical one, so no payload chosen by a script reaches the tag space.
#[inline]
pub fn double(raw: u64) -> u64 //@ret
//@ ensures
//@     is_double_bits(ret),
//@     is_nan_bits(raw) ==> ret == 0x8007_FFFF_FFFF_FFFFu64,
//@     !is_nan_bits(raw) ==> !ret == raw,
{
    let nan = (raw >> 52) & 0x7FF == 0x7FF && raw & 0xF_FFFF_FFFF_FFFF != 0;
    //@ assert(nan == is_nan_bits(raw));
    if nan {
        //@ assert(0x8007_FFFF_FFFF_FFFFu64 >> 48u64 == 0x8007) by (bit_vector);
        CANONICAL_NAN
    } else {
        // Not a NaN, so the top 16 bits of `!raw` are none of the tags (nor 0, which would make it a pointer).
        //@ assert(!((raw >> 52u64) & 0x7FF == 0x7FF && raw & 0xF_FFFF_FFFF_FFFF != 0) ==>
        //@     (!raw != 0 && (!raw) >> 48u64 != 0 && (!raw) >> 48u64 != 0x8001 && (!raw) >> 48u64 != 0x8003
        //@      && (!raw) >> 48u64 != 0x8004 && (!raw) >> 48u64 != 0x8005 && (!raw) >> 48u64 != 0x8006)) by (bit_vector);
        //@ assert(!(!raw) == raw) by (bit_vector);
        !raw
    }
}

/// The IEEE bits of a double value.
#[inline]
pub fn to_double_bits(bits: u64) -> u64 //@ret
//@ ensures !ret == bits,
{
    //@ assert(!(!bits) == bits) by (bit_vector);
    !bits
}

/// A heap pointer as a value. The engine gives us only pointers below 2^48, 8-byte aligned and not null.
#[inline]
pub fn pointer(p: u64) -> u64 //@ret
//@ requires p != 0, p < 0x1_0000_0000_0000,
//@ ensures ret != 0 && tag_of(ret) == 0, ret == p,
{
    //@ assert(p != 0 && p < 0x1_0000_0000_0000 ==> p >> 48u64 == 0) by (bit_vector);
    p
}

/// `JS::PrivateValue`: a Rust pointer kept in a slot, encoded exactly like the double whose IEEE bits are `p`, so the GC sees
/// a number and never follows it. Below 2^48 those bits are never a NaN.
#[inline]
pub fn private(p: u64) -> u64 //@ret
//@ requires p < 0x1_0000_0000_0000,
//@ ensures is_double_bits(ret), !ret == p,
{
    //@ assert(p < 0x1_0000_0000_0000 ==> !((p >> 52u64) & 0x7FF == 0x7FF && p & 0xF_FFFF_FFFF_FFFF != 0)) by (bit_vector);
    double(p)
}

#[inline]
pub fn to_private(bits: u64) -> u64 //@ret
//@ ensures !ret == bits,
{
    to_double_bits(bits)
}

/// Whether the GC must visit these bits: a heap pointer, nothing else.
#[inline]
pub fn is_traced(bits: u64) -> bool //@ret
//@ ensures ret == (bits != 0 && tag_of(bits) == 0), ret ==> bits < 0x1_0000_0000_0000,
{
    //@ assert(bits >> 48u64 == 0 ==> bits < 0x1_0000_0000_0000) by (bit_vector);
    bits != 0 && tag(bits) == 0
}
