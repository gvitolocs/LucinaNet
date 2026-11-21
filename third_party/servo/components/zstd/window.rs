/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// What a zstd frame header may ask of the decoder (RFC 8878 section 3.1.1), read before any of the frame is decoded. Verus checks
// this very file: `//@` lines hold the proof, see LucinaNet scripts/verify-shipped.sh.
//
// The limit is RFC 9659's for `Content-Encoding: zstd`: a window of at most 8 MiB, which Chromium and Firefox both enforce
// (windowLogMax 23). libzstd through async-compression, what Servo used, allowed 128 MiB (its default windowLogMax 27), so a page
// could make the decoder hold 128 MiB for one response. A frame that needs a dictionary is refused, as libzstd refuses it with none
// loaded, and so is one with the reserved bit set (RFC 8878: a decoder must not decode it).
//
// Proved: a header accepted needs a window of at most MAX_WINDOW and lies inside the bytes given; the window is the RFC's: from the
// Window_Descriptor, or the Frame_Content_Size of a single segment, never under 1 KiB; an accepted frame has the zstd magic, no
// reserved bit and no dictionary id; a refusal for its window is only ever for a window over MAX_WINDOW; "need more" asks for more
// than was given and never more than MAX_HEADER; no shift or sum overflows. Not proved: the decoding of the frame itself (ruzstd's,
// compared with libzstd in tests/parity.rs), which is told the same limit.

/// RFC 9659: the largest window a `zstd` content coding may use.
pub const MAX_WINDOW: u64 = 8388608;
/// The longest frame header: magic, descriptor, window, a 4-byte dictionary id and an 8-byte content size.
pub const MAX_HEADER: usize = 18;
/// A skippable frame's magic and its 4-byte size.
pub const SKIPPABLE_HEADER: usize = 8;

/// What the start of a frame says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum Header {
    /// A frame of compressed data, its header `len` bytes long, needing a window of `window` bytes.
    Frame { len: usize, window: u64 },
    /// A skippable frame: its 8-byte header and then `data` bytes that are not content.
    Skippable { data: u64 },
    /// The header is longer than the bytes given: at least `need` bytes are needed to read it.
    NeedMore { need: usize },
}

/// Why a frame is refused before it is decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum Refused {
    /// Not the magic of a zstd frame nor of a skippable one.
    Magic,
    /// The reserved bit of the frame header descriptor is set.
    Reserved,
    /// The frame needs a dictionary, and HTTP gives none.
    Dictionary,
    /// The frame needs a window larger than MAX_WINDOW.
    Window,
}

//@ pub open spec fn le32(b0: u8, b1: u8, b2: u8, b3: u8) -> u32 {
//@     (b0 as u32) | ((b1 as u32) << 8u32) | ((b2 as u32) << 16u32) | ((b3 as u32) << 24u32)
//@ }

fn le32_at(buf: &[u8], at: usize) -> u32 //@ret
//@ requires at + 4 <= buf.len(),
//@ ensures ret == le32(buf[at as int], buf[at + 1], buf[at + 2], buf[at + 3]),
{
    (buf[at] as u32) | ((buf[at + 1] as u32) << 8u32) | ((buf[at + 2] as u32) << 16u32) | ((buf[at + 3] as u32) << 24u32)
}

//@ pub open spec fn spec_fcs_len(d: u8) -> int {
//@     if (d >> 6u8) == 0 { if (d & 0x20u8) != 0 { 1 } else { 0 } } else if (d >> 6u8) == 1 { 2 } else if (d >> 6u8) == 2 { 4 } else { 8 }
//@ }

//@ pub open spec fn spec_did_len(d: u8) -> int {
//@     if (d & 3u8) == 0 { 0 } else if (d & 3u8) == 1 { 1 } else if (d & 3u8) == 2 { 2 } else { 4 }
//@ }

/// The size of the frame content field: RFC 8878 Frame_Content_Size_Flag and Single_Segment_Flag.
pub fn fcs_len(descriptor: u8) -> usize //@ret
//@ ensures ret == spec_fcs_len(descriptor), ret == 0 || ret == 1 || ret == 2 || ret == 4 || ret == 8,
//@     (descriptor & 0x20u8) != 0 ==> ret != 0,
{
    let flag = descriptor >> 6u8;
    if flag == 0 {
        if descriptor & 0x20 != 0 { 1 } else { 0 }
    } else if flag == 1 {
        2
    } else if flag == 2 {
        4
    } else {
        8
    }
}

/// The size of the dictionary id field: RFC 8878 Dictionary_ID_Flag.
pub fn did_len(descriptor: u8) -> usize //@ret
//@ ensures ret == spec_did_len(descriptor), ret == 0 || ret == 1 || ret == 2 || ret == 4,
{
    let flag = descriptor & 3;
    if flag == 0 {
        0
    } else if flag == 1 {
        1
    } else if flag == 2 {
        2
    } else {
        4
    }
}

//@ pub open spec fn spec_window(wd: u8) -> int {
//@     (1u64 << (10u64 + (wd >> 3u8) as u64)) as int + ((1u64 << (10u64 + (wd >> 3u8) as u64)) / 8) as int * (wd & 7u8) as int
//@ }

/// RFC 8878 Window_Descriptor: windowLog = 10 + Exponent, Window_Size = windowBase + windowBase / 8 * Mantissa.
pub fn window_size(wd: u8) -> u64 //@ret
//@ ensures ret as int == spec_window(wd), ret <= 4123168604160u64,
{
    let exponent = (wd >> 3u8) as u64;
    let mantissa = (wd & 7u8) as u64;
    //@ assert(wd >> 3u8 <= 31u8) by (bit_vector);
    //@ assert((wd & 7u8) <= 7u8) by (bit_vector);
    let base: u64 = 1u64 << (10 + exponent);
    //@ assert(base <= 2199023255552u64) by (bit_vector) requires exponent <= 31u64, base == 1u64 << (10u64 + exponent);
    //@ assert(mantissa <= 7);
    //@ assert((base / 8) * mantissa <= 1924145348608u64) by (nonlinear_arith) requires base <= 2199023255552u64, mantissa <= 7u64;
    base + (base / 8) * mantissa
}

//@ pub open spec fn spec_fcs(buf: Seq<u8>, at: int, n: int) -> int {
//@     if n == 1 { buf[at] as int }
//@     else if n == 2 { buf[at] as int + 256 * buf[at + 1] as int + 256 }
//@     else if n == 4 { le32(buf[at], buf[at + 1], buf[at + 2], buf[at + 3]) as int }
//@     else { le32(buf[at], buf[at + 1], buf[at + 2], buf[at + 3]) as int + 4294967296 * le32(buf[at + 4], buf[at + 5], buf[at + 6], buf[at + 7]) as int }
//@ }

//@ pub open spec fn spec_wd_len(d: u8) -> int { if (d & 0x20u8) != 0 { 0 } else { 1 } }

//@ pub open spec fn at_least_1k(x: int) -> int { if x < 1024 { 1024 } else { x } }

/// RFC 8878 Frame_Content_Size: `n` little-endian bytes at `at`, with 256 added to a 2-byte one.
pub fn content_size(buf: &[u8], at: usize, n: usize) -> u64 //@ret
//@ requires n == 1 || n == 2 || n == 4 || n == 8, at + n <= buf.len(),
//@ ensures ret as int == spec_fcs(buf@, at as int, n as int),
{
    if n == 1 {
        buf[at] as u64
    } else if n == 2 {
        buf[at] as u64 + 256 * (buf[at + 1] as u64) + 256
    } else if n == 4 {
        le32_at(buf, at) as u64
    } else {
        let lo = le32_at(buf, at) as u64;
        let hi = le32_at(buf, at + 4) as u64;
        //@ assert(hi * 4294967296u64 + lo <= 18446744073709551615u64) by (nonlinear_arith) requires hi <= 4294967295u64, lo <= 4294967295u64;
        hi * 4294967296 + lo
    }
}

/// What a frame starting at `buf[0]` says, from as many of its bytes as are given.
pub fn frame_header(buf: &[u8]) -> Result<Header, Refused> //@ret
//@ ensures match ret {
//@     Ok(Header::Frame { len, window }) => len <= buf.len() && len <= MAX_HEADER && window <= MAX_WINDOW && window > 0
//@         && le32(buf[0], buf[1], buf[2], buf[3]) == 0xFD2FB528u32 && (buf[4] & 0x08u8) == 0
//@         && len == 5 + spec_wd_len(buf[4]) + spec_did_len(buf[4]) + spec_fcs_len(buf[4])
//@         && (forall|j: int| 0 <= j < spec_did_len(buf[4]) ==> #[trigger] buf[5 + spec_wd_len(buf[4]) + j] == 0u8)
//@         && window as int == at_least_1k(if (buf[4] & 0x20u8) != 0 { spec_fcs(buf@, 5 + spec_did_len(buf[4]), spec_fcs_len(buf[4])) }
//@                                         else { spec_window(buf[5]) }),
//@     Ok(Header::NeedMore { need }) => need > buf.len() && need <= MAX_HEADER,
//@     Ok(Header::Skippable { data }) => buf.len() >= SKIPPABLE_HEADER && (le32(buf[0], buf[1], buf[2], buf[3]) & 0xFFFFFFF0u32) == 0x184D2A50u32
//@         && data as int == le32(buf[4], buf[5], buf[6], buf[7]) as int,
//@     Err(Refused::Window) => buf.len() >= 5 && at_least_1k(if (buf[4] & 0x20u8) != 0 { spec_fcs(buf@, 5 + spec_did_len(buf[4]), spec_fcs_len(buf[4])) }
//@                                         else { spec_window(buf[5]) }) > MAX_WINDOW,
//@     Err(_) => true,
//@ },
{
    if buf.len() < 4 {
        return Ok(Header::NeedMore { need: 4 });
    }
    let magic = le32_at(buf, 0);
    if magic & 0xFFFFFFF0u32 == 0x184D2A50u32 {
        if buf.len() < SKIPPABLE_HEADER {
            return Ok(Header::NeedMore { need: SKIPPABLE_HEADER });
        }
        return Ok(Header::Skippable { data: le32_at(buf, 4) as u64 });
    }
    if magic != 0xFD2FB528u32 {
        return Err(Refused::Magic);
    }
    if buf.len() < 5 {
        return Ok(Header::NeedMore { need: 5 });
    }
    let descriptor = buf[4];
    if descriptor & 0x08 != 0 {
        return Err(Refused::Reserved);
    }
    let single = descriptor & 0x20 != 0;
    let wd_len: usize = if single { 0 } else { 1 };
    let did = did_len(descriptor);
    let fcs = fcs_len(descriptor);
    let len = 5 + wd_len + did + fcs;
    if buf.len() < len {
        return Ok(Header::NeedMore { need: len });
    }
    let after_wd = 5 + wd_len;
    // A dictionary id of zero means none (RFC 8878 Dictionary_ID).
    let mut i: usize = 0;
    while i < did
        //@ invariant i <= did, did <= 4, after_wd + did <= buf.len(), after_wd == 5 + wd_len,
        //@     forall|j: int| 0 <= j < i ==> #[trigger] buf[5 + wd_len + j] == 0u8,
        //@ decreases did - i,
    {
        if buf[after_wd + i] != 0 {
            return Err(Refused::Dictionary);
        }
        i = i + 1;
    }
    // The window is the content size in a single segment, which must then be given (fcs_len is never 0 there).
    let window: u64 = if single { content_size(buf, after_wd + did, fcs) } else { window_size(buf[5]) };
    // RFC 8878: no window is under 1 KiB (windowLog 10), whatever a small content size says.
    let window = if window < 1024 { 1024 } else { window };
    if window > MAX_WINDOW {
        return Err(Refused::Window);
    }
    Ok(Header::Frame { len, window })
}
