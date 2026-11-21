// Where a field may lie in a record, and how long a record may grow (record.rs). Verus checks this very file: `//@` lines hold the
// proof, see LucinaNet scripts/verify-shipped.sh.
//
// Proved: a span returned lies inside the record and starts where the field does, a length prefix that says more than is left is
// refused, as is one above MAX_FIELD, and no sum here overflows. Not proved: the byte reads and copies around these calls (record.rs
// slices at exactly the returned bounds), or what a field means.

/// A fixed field: 8 bytes, little-endian.
pub const FIXED: usize = 8;
/// The length in front of a variable field: 4 bytes, little-endian.
pub const LEN_PREFIX: usize = 4;
/// The longest variable field: 1 GiB. An IndexedDB value or a cached response larger than this is refused, not cut.
pub const MAX_FIELD: u32 = 1073741824;
/// The longest record: 2 GiB, below redb's own limit on a value.
pub const MAX_RECORD: usize = 2147483648;

/// Where a fixed field that starts at `at` ends, in a record of `len` bytes.
pub fn fixed_span(len: usize, at: usize) -> Option<usize> //@ret
//@ ensures match ret {
//@     Some(end) => at <= len && end == at + FIXED && end <= len,
//@     None => at > len || len - at < FIXED,
//@ },
{
    if at > len || len - at < FIXED {
        None
    } else {
        Some(at + FIXED)
    }
}

/// Where the bytes of a variable field lie, the field starting at `at` in a record of `len` bytes, with `n` in its length prefix.
pub fn field_span(len: usize, at: usize, n: u32) -> Option<(usize, usize)> //@ret
//@ ensures match ret {
//@     Some((start, end)) => at <= len && start == at + LEN_PREFIX && end == start + n && end <= len && n <= MAX_FIELD,
//@     None => at > len || len - at < LEN_PREFIX || n > MAX_FIELD || len - at - LEN_PREFIX < n,
//@ },
{
    if at > len || len - at < LEN_PREFIX || n > MAX_FIELD {
        return None;
    }
    let room = len - at - LEN_PREFIX;
    if room < n as usize {
        return None;
    }
    Some((at + LEN_PREFIX, at + LEN_PREFIX + n as usize))
}

/// Whether a record of `len` bytes may take `add` more.
pub fn record_may_grow(len: usize, add: usize) -> bool //@ret
//@ ensures ret == (len + add <= MAX_RECORD),
{
    len <= MAX_RECORD && add <= MAX_RECORD - len
}
