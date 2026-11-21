// The byte layer of the function cache's records: little-endian numbers, length-prefixed byte strings, a bounds-checked reader and
// the record header. `template.rs` (the template codec) and `disk.rs` (the record header) call these and hold no copy of them.
// Verus checks this very file (the method of LucinaOS `scripts/verify-shipped.sh`): `//@` lines hold the proof.
//
// Proved:
//  - every `Writer` method appends exactly the bytes of its encoding (`enc16`, `enc32`, `enc64`, the length-prefixed `bytes`), and
//    every `Reader` method reads exactly the number `dec16`, `dec32`, `dec64` of the bytes it consumes, so reading what was
//    written gives the value back (`rt16`, `rt32`, `rt64`, `rt_i32`, `rt_i64`, `rt_bytes`: decode(encode(x)) == x at any position of
//    a buffer, with anything before and after);
//  - the reader never reads outside its buffer and never overflows: a read that does not fit returns `None` and leaves the position
//    where it was, for any buffer and any position, truncated or corrupt input included (`take`);
//  - the record header: `write_header` appends the magic, the format version, the build id, the key, the partition digest and the
//    payload length; `read_header` returns the key and the partition digest only if the buffer has the magic, the version asked
//    for, the build id asked for, and exactly the payload length it states after the header, and the key and partition are the
//    ones written (`header_roundtrip`), so an old or foreign version is refused.
//
// Not proved: how `template.rs` composes these into the template (its round trip is tested: `template_round_trip`, every truncation,
// every one-byte corruption), and that `Reader::pos <= buf.len()` holds for a reader built by someone who fills the public fields.

//@ global size_of usize == 8;

//@ pub open spec fn enc16(v: u16) -> Seq<u8> { seq![v as u8, (v >> 8u16) as u8] }
//@ pub open spec fn dec16(s: Seq<u8>) -> u16 recommends s.len() == 2 { (s[0] as u16) | ((s[1] as u16) << 8u16) }
//@ pub open spec fn enc32(v: u32) -> Seq<u8> { seq![v as u8, (v >> 8u32) as u8, (v >> 16u32) as u8, (v >> 24u32) as u8] }
//@ pub open spec fn dec32(s: Seq<u8>) -> u32 recommends s.len() == 4 {
//@     (s[0] as u32) | ((s[1] as u32) << 8u32) | ((s[2] as u32) << 16u32) | ((s[3] as u32) << 24u32)
//@ }
//@ pub open spec fn enc64(v: u64) -> Seq<u8> {
//@     seq![v as u8, (v >> 8u64) as u8, (v >> 16u64) as u8, (v >> 24u64) as u8, (v >> 32u64) as u8, (v >> 40u64) as u8, (v >> 48u64) as u8, (v >> 56u64) as u8]
//@ }
//@ pub open spec fn dec64(s: Seq<u8>) -> u64 recommends s.len() == 8 {
//@     (s[0] as u64) | ((s[1] as u64) << 8u64) | ((s[2] as u64) << 16u64) | ((s[3] as u64) << 24u64)
//@         | ((s[4] as u64) << 32u64) | ((s[5] as u64) << 40u64) | ((s[6] as u64) << 48u64) | ((s[7] as u64) << 56u64)
//@ }

//@ pub proof fn rt16(v: u16)
//@     ensures dec16(enc16(v)) == v,
//@ {
//@     assert((v as u8) as u16 | ((((v >> 8u16) as u8) as u16) << 8u16) == v) by (bit_vector);
//@ }
//@ pub proof fn rt32(v: u32)
//@     ensures dec32(enc32(v)) == v,
//@ {
//@     assert((v as u8) as u32 | ((((v >> 8u32) as u8) as u32) << 8u32) | ((((v >> 16u32) as u8) as u32) << 16u32) | ((((v >> 24u32) as u8) as u32) << 24u32) == v) by (bit_vector);
//@ }
//@ pub proof fn rt64(v: u64)
//@     ensures dec64(enc64(v)) == v,
//@ {
//@     assert((v as u8) as u64 | ((((v >> 8u64) as u8) as u64) << 8u64) | ((((v >> 16u64) as u8) as u64) << 16u64) | ((((v >> 24u64) as u8) as u64) << 24u64)
//@         | ((((v >> 32u64) as u8) as u64) << 32u64) | ((((v >> 40u64) as u8) as u64) << 40u64) | ((((v >> 48u64) as u8) as u64) << 48u64) | ((((v >> 56u64) as u8) as u64) << 56u64) == v) by (bit_vector);
//@ }
//@ /// Signed numbers are written as the unsigned number of the same bits.
//@ pub proof fn rt_i32(v: i32)
//@     ensures dec32(enc32(v as u32)) as i32 == v,
//@ {
//@     rt32(v as u32);
//@     assert(((v as u32) as i32) == v) by (bit_vector);
//@ }
//@ pub proof fn rt_i64(v: i64)
//@     ensures dec64(enc64(v as u64)) as i64 == v,
//@ {
//@     rt64(v as u64);
//@     assert(((v as u64) as i64) == v) by (bit_vector);
//@ }

/// Appends little-endian numbers and length-prefixed byte strings to a buffer.
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Writer //@ret
    //@ ensures ret.buf@.len() == 0,
    {
        Writer { buf: Vec::new() }
    }

    pub fn u8(&mut self, v: u8)
    //@ ensures final(self).buf@ == old(self).buf@.push(v),
    {
        self.buf.push(v);
    }

    pub fn u16(&mut self, v: u16)
    //@ ensures final(self).buf@ == old(self).buf@ + enc16(v),
    {
        self.buf.push(v as u8);
        self.buf.push((v >> 8) as u8);
        //@ assert(self.buf@ =~= old(self).buf@ + enc16(v));
    }

    pub fn u32(&mut self, v: u32)
    //@ ensures final(self).buf@ == old(self).buf@ + enc32(v),
    {
        self.buf.push(v as u8);
        self.buf.push((v >> 8) as u8);
        self.buf.push((v >> 16) as u8);
        self.buf.push((v >> 24) as u8);
        //@ assert(self.buf@ =~= old(self).buf@ + enc32(v));
    }

    pub fn u64(&mut self, v: u64)
    //@ ensures final(self).buf@ == old(self).buf@ + enc64(v),
    {
        self.buf.push(v as u8);
        self.buf.push((v >> 8) as u8);
        self.buf.push((v >> 16) as u8);
        self.buf.push((v >> 24) as u8);
        self.buf.push((v >> 32) as u8);
        self.buf.push((v >> 40) as u8);
        self.buf.push((v >> 48) as u8);
        self.buf.push((v >> 56) as u8);
        //@ assert(self.buf@ =~= old(self).buf@ + enc64(v));
    }

    pub fn i32(&mut self, v: i32)
    //@ ensures final(self).buf@ == old(self).buf@ + enc32(v as u32),
    {
        self.u32(v as u32);
    }

    pub fn i64(&mut self, v: i64)
    //@ ensures final(self).buf@ == old(self).buf@ + enc64(v as u64),
    {
        self.u64(v as u64);
    }

    pub fn usize(&mut self, v: usize)
    //@ ensures final(self).buf@ == old(self).buf@ + enc64(v as u64),
    {
        self.u64(v as u64);
    }

    pub fn bool(&mut self, v: bool)
    //@ ensures final(self).buf@ == old(self).buf@.push(if v { 1u8 } else { 0u8 }),
    {
        self.u8(if v { 1 } else { 0 });
    }

    /// The length as a u64, then the bytes.
    pub fn bytes(&mut self, v: &[u8])
    //@ ensures final(self).buf@ == old(self).buf@ + enc64(v@.len() as u64) + v@,
    {
        self.usize(v.len());
        //@ let ghost mid = self.buf@;
        let mut i = 0;
        while i < v.len()
        //@ invariant 0 <= i <= v@.len(), self.buf@ == mid + v@.subrange(0, i as int),
        //@ decreases v@.len() - i,
        {
            self.buf.push(v[i]);
            //@ assert(self.buf@ =~= mid + v@.subrange(0, i as int + 1));
            i += 1;
        }
        //@ assert(v@.subrange(0, v@.len() as int) =~= v@);
    }
}

/// Reads what a `Writer` wrote. Every read is bounds-checked: one that does not fit returns `None` and moves nothing.
pub struct Reader<'a> {
    pub buf: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Reader<'a> //@ret
    //@ ensures ret.buf@ == buf@, ret.pos == 0,
    {
        Reader { buf, pos: 0 }
    }

    /// Move over `n` bytes; the index of the first of them. The buffer is not touched.
    pub fn take(&mut self, n: usize) -> Option<usize> //@ret
    //@ requires old(self).pos <= old(self).buf@.len(),
    //@ ensures final(self).buf@ == old(self).buf@, final(self).pos <= final(self).buf@.len(),
    //@     match ret {
    //@         Some(start) => start == old(self).pos && final(self).pos == old(self).pos + n && final(self).pos <= final(self).buf@.len(),
    //@         None => n > old(self).buf@.len() - old(self).pos && final(self).pos == old(self).pos,
    //@     },
    {
        if n > self.buf.len() - self.pos {
            return None;
        }
        let start = self.pos;
        self.pos += n;
        Some(start)
    }

    pub fn u8(&mut self) -> Option<u8> //@ret
    //@ requires old(self).pos <= old(self).buf@.len(),
    //@ ensures final(self).buf@ == old(self).buf@, final(self).pos <= final(self).buf@.len(),
    //@     match ret {
    //@         Some(v) => final(self).pos == old(self).pos + 1 && v == old(self).buf@[old(self).pos as int],
    //@         None => final(self).pos == old(self).pos && old(self).buf@.len() - old(self).pos < 1,
    //@     },
    {
        match self.take(1) {
            Some(s) => Some(self.buf[s]),
            None => None,
        }
    }

    pub fn u16(&mut self) -> Option<u16> //@ret
    //@ requires old(self).pos <= old(self).buf@.len(),
    //@ ensures final(self).buf@ == old(self).buf@, final(self).pos <= final(self).buf@.len(),
    //@     match ret {
    //@         Some(v) => final(self).pos == old(self).pos + 2 && v == dec16(old(self).buf@.subrange(old(self).pos as int, old(self).pos as int + 2)),
    //@         None => final(self).pos == old(self).pos && old(self).buf@.len() - old(self).pos < 2,
    //@     },
    {
        match self.take(2) {
            Some(s) => {
                //@ assert(self.buf@.subrange(s as int, s as int + 2)[0] == self.buf@[s as int]);
                //@ assert(self.buf@.subrange(s as int, s as int + 2)[1] == self.buf@[s as int + 1]);
                Some((self.buf[s] as u16) | ((self.buf[s + 1] as u16) << 8))
            }
            None => None,
        }
    }

    pub fn u32(&mut self) -> Option<u32> //@ret
    //@ requires old(self).pos <= old(self).buf@.len(),
    //@ ensures final(self).buf@ == old(self).buf@, final(self).pos <= final(self).buf@.len(),
    //@     match ret {
    //@         Some(v) => final(self).pos == old(self).pos + 4 && v == dec32(old(self).buf@.subrange(old(self).pos as int, old(self).pos as int + 4)),
    //@         None => final(self).pos == old(self).pos && old(self).buf@.len() - old(self).pos < 4,
    //@     },
    {
        match self.take(4) {
            Some(s) => {
                //@ let ghost w = self.buf@.subrange(s as int, s as int + 4);
                //@ assert(w[0] == self.buf@[s as int] && w[1] == self.buf@[s as int + 1] && w[2] == self.buf@[s as int + 2] && w[3] == self.buf@[s as int + 3]);
                Some(
                    (self.buf[s] as u32)
                        | ((self.buf[s + 1] as u32) << 8)
                        | ((self.buf[s + 2] as u32) << 16)
                        | ((self.buf[s + 3] as u32) << 24),
                )
            }
            None => None,
        }
    }

    pub fn u64(&mut self) -> Option<u64> //@ret
    //@ requires old(self).pos <= old(self).buf@.len(),
    //@ ensures final(self).buf@ == old(self).buf@, final(self).pos <= final(self).buf@.len(),
    //@     match ret {
    //@         Some(v) => final(self).pos == old(self).pos + 8 && v == dec64(old(self).buf@.subrange(old(self).pos as int, old(self).pos as int + 8)),
    //@         None => final(self).pos == old(self).pos && old(self).buf@.len() - old(self).pos < 8,
    //@     },
    {
        match self.take(8) {
            Some(s) => {
                //@ let ghost w = self.buf@.subrange(s as int, s as int + 8);
                //@ assert(forall|k: int| 0 <= k < 8 ==> w[k] == self.buf@[s as int + k]);
                Some(
                    (self.buf[s] as u64)
                        | ((self.buf[s + 1] as u64) << 8)
                        | ((self.buf[s + 2] as u64) << 16)
                        | ((self.buf[s + 3] as u64) << 24)
                        | ((self.buf[s + 4] as u64) << 32)
                        | ((self.buf[s + 5] as u64) << 40)
                        | ((self.buf[s + 6] as u64) << 48)
                        | ((self.buf[s + 7] as u64) << 56),
                )
            }
            None => None,
        }
    }

    pub fn i32(&mut self) -> Option<i32> //@ret
    //@ requires old(self).pos <= old(self).buf@.len(),
    //@ ensures final(self).buf@ == old(self).buf@, final(self).pos <= final(self).buf@.len(),
    //@     match ret {
    //@         Some(v) => final(self).pos == old(self).pos + 4 && v == dec32(old(self).buf@.subrange(old(self).pos as int, old(self).pos as int + 4)) as i32,
    //@         None => final(self).pos == old(self).pos && old(self).buf@.len() - old(self).pos < 4,
    //@     },
    {
        match self.u32() {
            Some(v) => Some(v as i32),
            None => None,
        }
    }

    pub fn i64(&mut self) -> Option<i64> //@ret
    //@ requires old(self).pos <= old(self).buf@.len(),
    //@ ensures final(self).buf@ == old(self).buf@, final(self).pos <= final(self).buf@.len(),
    //@     match ret {
    //@         Some(v) => final(self).pos == old(self).pos + 8 && v == dec64(old(self).buf@.subrange(old(self).pos as int, old(self).pos as int + 8)) as i64,
    //@         None => final(self).pos == old(self).pos && old(self).buf@.len() - old(self).pos < 8,
    //@     },
    {
        match self.u64() {
            Some(v) => Some(v as i64),
            None => None,
        }
    }

    pub fn usize(&mut self) -> Option<usize> //@ret
    //@ requires old(self).pos <= old(self).buf@.len(),
    //@ ensures final(self).buf@ == old(self).buf@, final(self).pos <= final(self).buf@.len(),
    //@     match ret {
    //@         Some(v) => final(self).pos == old(self).pos + 8 && v as u64 == dec64(old(self).buf@.subrange(old(self).pos as int, old(self).pos as int + 8)),
    //@         None => final(self).pos == old(self).pos && old(self).buf@.len() - old(self).pos < 8,
    //@     },
    {
        match self.u64() {
            Some(v) => Some(v as usize),
            None => None,
        }
    }

    pub fn bool(&mut self) -> Option<bool> //@ret
    //@ requires old(self).pos <= old(self).buf@.len(),
    //@ ensures final(self).buf@ == old(self).buf@, final(self).pos <= final(self).buf@.len(),
    //@     match ret {
    //@         Some(v) => final(self).pos == old(self).pos + 1 && v == (old(self).buf@[old(self).pos as int] != 0),
    //@         None => final(self).pos == old(self).pos && old(self).buf@.len() - old(self).pos < 1,
    //@     },
    {
        match self.u8() {
            Some(v) => Some(v != 0),
            None => None,
        }
    }

    /// A length-prefixed byte string: the range `[start, end)` of the buffer it occupies.
    pub fn bytes_range(&mut self) -> Option<(usize, usize)> //@ret
    //@ requires old(self).pos <= old(self).buf@.len(),
    //@ ensures final(self).buf@ == old(self).buf@, final(self).pos <= final(self).buf@.len(),
    //@     match ret {
    //@         Some((start, end)) => start <= end <= final(self).buf@.len() && start == old(self).pos + 8 && final(self).pos == end
    //@             && (end - start) as u64 == dec64(old(self).buf@.subrange(old(self).pos as int, old(self).pos as int + 8)),
    //@         None => final(self).pos >= old(self).pos && final(self).pos <= final(self).buf@.len(),
    //@     },
    {
        let len = match self.usize() {
            Some(len) => len,
            None => return None,
        };
        match self.take(len) {
            Some(start) => Some((start, start + len)),
            None => None,
        }
    }

    /// Whether all of the buffer has been read.
    pub fn done(&self) -> bool //@ret
    //@ ensures ret == (self.pos == self.buf@.len()),
    {
        self.pos == self.buf.len()
    }
}

//@ /// decode(encode(x)) == x: what a reader at `p` reads of a buffer with the encoding of `v` at `p`, and anything around it.
//@ pub proof fn rt_at(prefix: Seq<u8>, v: u64, suffix: Seq<u8>)
//@     ensures ({
//@         let buf = prefix + enc64(v) + suffix;
//@         buf.subrange(prefix.len() as int, prefix.len() as int + 8) == enc64(v) && dec64(buf.subrange(prefix.len() as int, prefix.len() as int + 8)) == v
//@     }),
//@ {
//@     let buf = prefix + enc64(v) + suffix;
//@     assert(buf.subrange(prefix.len() as int, prefix.len() as int + 8) =~= enc64(v));
//@     rt64(v);
//@ }

//@ /// A length-prefixed byte string written after `prefix` and followed by `suffix` is found again at the same place, with the same
//@ /// length and the same bytes.
//@ pub proof fn rt_bytes(prefix: Seq<u8>, data: Seq<u8>, suffix: Seq<u8>)
//@     ensures ({
//@         let buf = prefix + enc64(data.len() as u64) + data + suffix;
//@         let p = prefix.len() as int;
//@         dec64(buf.subrange(p, p + 8)) == data.len() as u64 && buf.subrange(p + 8, p + 8 + data.len()) == data
//@     }),
//@ {
//@     let buf = prefix + enc64(data.len() as u64) + data + suffix;
//@     let p = prefix.len() as int;
//@     assert(buf.subrange(p, p + 8) =~= enc64(data.len() as u64));
//@     assert(buf.subrange(p + 8, p + 8 + data.len()) =~= data);
//@     rt64(data.len() as u64);
//@ }

/// Number of bytes in a record header: magic (4) + format version (4) + build id (8) + key (16) + partition digest (16) + payload
/// length (4).
pub const HEADER_LEN: usize = 52;

//@ pub open spec fn enc128(v: u128) -> Seq<u8> { enc64(v as u64) + enc64((v >> 64u128) as u64) }
//@ pub open spec fn dec128(s: Seq<u8>) -> u128 recommends s.len() == 16 {
//@     (dec64(s.subrange(0, 8)) as u128) | ((dec64(s.subrange(8, 16)) as u128) << 64u128)
//@ }
//@ pub proof fn rt128(v: u128)
//@     ensures dec128(enc128(v)) == v,
//@ {
//@     let s = enc128(v);
//@     assert(s.subrange(0, 8) =~= enc64(v as u64));
//@     assert(s.subrange(8, 16) =~= enc64((v >> 64u128) as u64));
//@     rt64(v as u64);
//@     rt64((v >> 64u128) as u64);
//@     assert(((v as u64) as u128) | ((((v >> 64u128) as u64) as u128) << 64u128) == v) by (bit_vector);
//@ }

/// The header of a record: the magic, the format version, the build id, the key and the partition digest the record was made for,
/// and the length of the payload that follows.
pub fn write_header(out: &mut Vec<u8>, magic: &[u8], version: u32, build: u64, key: u128, partition: u128, payload_len: u32)
//@ requires magic@.len() == 4,
//@ ensures final(out)@ == old(out)@ + magic@ + enc32(version) + enc64(build) + enc128(key) + enc128(partition) + enc32(payload_len),
{
    let mut w = Writer { buf: Vec::new() };
    let mut i = 0;
    while i < 4
    //@ invariant 0 <= i <= 4, magic@.len() == 4, w.buf@ == magic@.subrange(0, i as int),
    //@ decreases 4 - i,
    {
        w.buf.push(magic[i]);
        //@ assert(w.buf@ =~= magic@.subrange(0, i as int + 1));
        i += 1;
    }
    //@ assert(w.buf@ =~= magic@);
    w.u32(version);
    w.u64(build);
    w.u64(key as u64);
    w.u64((key >> 64) as u64);
    w.u64(partition as u64);
    w.u64((partition >> 64) as u64);
    w.u32(payload_len);
    //@ assert(w.buf@ =~= magic@ + enc32(version) + enc64(build) + enc128(key) + enc128(partition) + enc32(payload_len));
    let mut j = 0;
    while j < w.buf.len()
    //@ invariant 0 <= j <= w.buf@.len(), out@ == old(out)@ + w.buf@.subrange(0, j as int),
    //@ decreases w.buf@.len() - j,
    {
        out.push(w.buf[j]);
        //@ assert(out@ =~= old(out)@ + w.buf@.subrange(0, j as int + 1));
        j += 1;
    }
    //@ assert(w.buf@.subrange(0, w.buf@.len() as int) =~= w.buf@);
}

/// The key and the partition digest of a record of this format: `buf` starts with the magic, the format version asked for and the
/// build id asked for, and it is exactly the header and the payload length the header states. `None` for anything else.
pub fn read_header(buf: &[u8], magic: &[u8], version: u32, build: u64) -> Option<(u128, u128)> //@ret
//@ requires magic@.len() == 4,
//@ ensures match ret {
//@     Some((key, partition)) => buf@.len() >= 52
//@         && buf@.subrange(0, 4) == magic@
//@         && dec32(buf@.subrange(4, 8)) == version
//@         && dec64(buf@.subrange(8, 16)) == build
//@         && key == dec128(buf@.subrange(16, 32))
//@         && partition == dec128(buf@.subrange(32, 48))
//@         && buf@.len() == 52 + dec32(buf@.subrange(48, 52)) as int,
//@     None => !(buf@.len() >= 52
//@         && buf@.subrange(0, 4) == magic@
//@         && dec32(buf@.subrange(4, 8)) == version
//@         && dec64(buf@.subrange(8, 16)) == build
//@         && buf@.len() == 52 + dec32(buf@.subrange(48, 52)) as int),
//@ },
{
    if buf.len() < HEADER_LEN {
        return None;
    }
    let mut i = 0;
    while i < 4
    //@ invariant 0 <= i <= 4, magic@.len() == 4, buf@.len() >= 52, forall|k: int| 0 <= k < i ==> buf@[k] == magic@[k],
    //@ decreases 4 - i,
    {
        if buf[i] != magic[i] {
            //@ assert(buf@.subrange(0, 4) != magic@);
            return None;
        }
        i += 1;
    }
    //@ assert(buf@.subrange(0, 4) =~= magic@);
    let mut r = Reader { buf, pos: 4 };
    let found_version = match r.u32() {
        Some(v) => v,
        None => return None,
    };
    if found_version != version {
        return None;
    }
    let found_build = match r.u64() {
        Some(v) => v,
        None => return None,
    };
    if found_build != build {
        return None;
    }
    let key_lo = match r.u64() {
        Some(v) => v,
        None => return None,
    };
    let key_hi = match r.u64() {
        Some(v) => v,
        None => return None,
    };
    let partition_lo = match r.u64() {
        Some(v) => v,
        None => return None,
    };
    let partition_hi = match r.u64() {
        Some(v) => v,
        None => return None,
    };
    let payload_len = match r.u32() {
        Some(v) => v,
        None => return None,
    };
    if buf.len() != HEADER_LEN + payload_len as usize {
        return None;
    }
    let key = (key_lo as u128) | ((key_hi as u128) << 64);
    let partition = (partition_lo as u128) | ((partition_hi as u128) << 64);
    Some((key, partition))
}

//@ /// A record written with `write_header` and a payload of the length it states is read back with the same key and partition digest,
//@ /// and a different version is refused.
//@ pub proof fn header_roundtrip(magic: Seq<u8>, version: u32, build: u64, key: u128, partition: u128, payload: Seq<u8>, other_version: u32)
//@     requires magic.len() == 4, payload.len() <= 0xFFFF_FFFFnat as int, other_version != version,
//@     ensures ({
//@         let buf = magic + enc32(version) + enc64(build) + enc128(key) + enc128(partition) + enc32(payload.len() as u32) + payload;
//@         buf.len() >= 52 && buf.subrange(0, 4) == magic && dec32(buf.subrange(4, 8)) == version && dec64(buf.subrange(8, 16)) == build
//@             && dec128(buf.subrange(16, 32)) == key && dec128(buf.subrange(32, 48)) == partition
//@             && buf.len() == 52 + dec32(buf.subrange(48, 52)) as int && dec32(buf.subrange(4, 8)) != other_version
//@     }),
//@ {
//@     let buf = magic + enc32(version) + enc64(build) + enc128(key) + enc128(partition) + enc32(payload.len() as u32) + payload;
//@     assert(buf.subrange(0, 4) =~= magic);
//@     assert(buf.subrange(4, 8) =~= enc32(version));
//@     assert(buf.subrange(8, 16) =~= enc64(build));
//@     assert(buf.subrange(16, 32) =~= enc128(key));
//@     assert(buf.subrange(32, 48) =~= enc128(partition));
//@     assert(buf.subrange(48, 52) =~= enc32(payload.len() as u32));
//@     rt32(version);
//@     rt64(build);
//@     rt128(key);
//@     rt128(partition);
//@     rt32(payload.len() as u32);
//@ }
