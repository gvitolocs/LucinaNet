// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Behaviour derived from OTS (BSD-3-Clause, see LICENSES/BSD-3-Clause-OTS.txt).

//! Pure checked-arithmetic helpers for every offset/length/alignment/size rule
//! applied to untrusted font data. No allocation, no I/O, plain integer types.
//! Each function documents the OTS rule it mirrors (file and line in
//! `fontsan-0.7.0/src/deps/ots/src/ots.cc` unless noted otherwise).
//!
//! Verus proves every function here in this very file (the rule; `verification/fontsan_bounds.rs` names it
//! with `#[path]`): no overflow and no panic for any input (search_fields: for the `num_tables_ok` counts its one
//! caller passes), and the rule each one documents, as its `ensures`. The attributes are only seen by Verus.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Maximum accepted input file size: OTS rejects files > 1 GiB
/// (ots.cc, ProcessTTF/ProcessTTC/ProcessWOFF: "file exceeds 1GB").
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_FILE_SIZE: u64 = 1024 * 1024 * 1024;

/// Maximum decompressed font size: OTS_MAX_DECOMPRESSED_FILE_SIZE
/// (ots.h:235, 300 MiB; also the output stream size in ots_glue.cc).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_DECOMPRESSED_FILE_SIZE: u64 = 300 * 1024 * 1024;

/// Maximum decompressed single table size: OTS_MAX_DECOMPRESSED_TABLE_SIZE
/// (ots.h:236, 150 MiB).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_DECOMPRESSED_TABLE_SIZE: u64 = 150 * 1024 * 1024;

/// Maximum number of tables in an sfnt directory: num_tables must be in
/// [1, 4096) (ots.cc ProcessTTF: "excessive (or zero) number of tables").
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_NUM_TABLES: u16 = 4096;

/// Maximum number of faces in a TTC: num_fonts <= 0x10000
/// (ots.cc ProcessTTC: "Too many fonts in TTC").
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_TTC_FONTS: u32 = 0x10000;

/// Round `v` up to the next multiple of 4, returning None on overflow.
/// Mirrors `ots::Round4` (ots.h).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r is None <==> v as int + 3 > u64::MAX as int,
        r matches Some(x) ==> x as int % 4 == 0 && v <= x && x as int <= v as int + 3,
))]
#[allow(clippy::manual_map)] // the `match` is deliberate, for Verus (see the comment in the body)
pub fn round4(v: u64) -> Option<u64> {
    // A `match` rather than `.map(|x| x & !3)`, for Verus; the same value for every input.
    match v.checked_add(3) {
        Some(x) => {
            #[cfg(verus_keep_ghost)]
            proof! {
                assert(x & !3u64 == x - x % 4) by(bit_vector);
            }
            Some(x & !3)
        }
        None => None,
    }
}

/// Rule: input file must not exceed 1 GiB (ots.cc ProcessTTF ~line 205).
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == (length <= 1073741824)))]
pub fn file_size_ok(length: u64) -> bool {
    length <= MAX_FILE_SIZE
}

/// Rule: num_tables must be in [1, 4096) (ots.cc ProcessTTF ~line 232).
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == (1 <= num_tables && num_tables < 4096)))]
#[allow(clippy::manual_range_contains)] // keeps the shape the Verus proof and verification/mutants.sh use
pub fn num_tables_ok(num_tables: u16) -> bool {
    num_tables >= 1 && num_tables < MAX_NUM_TABLES
}

/// Compute the (searchRange, entrySelector, rangeShift) triple OTS expects for
/// a directory with `num_tables` entries (ots.cc ProcessTTF ~lines 236-262).
/// `num_tables` must already satisfy `num_tables_ok`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires 1 <= num_tables && num_tables < 4096,
    ensures
        // entrySelector = floor(log2(num_tables)), searchRange = 16 * 2^entrySelector,
        // rangeShift = 16 * num_tables - searchRange.
        r.1 <= 11,
        (1u32 << r.1 as u32) <= num_tables as u32 && (num_tables as u32) < (1u32 << (r.1 as u32 + 1)),
        r.0 as int == 16 * (1u32 << r.1 as u32) as int,
        r.2 as int == 16 * num_tables as int - r.0 as int,
))]
pub fn search_fields(num_tables: u16) -> (u16, u16, u16) {
    let mut max_pow2: u16 = 0;
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(1u32 << 0u32 == 1u32) by(bit_vector);
    }
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            1 <= num_tables && num_tables < 4096,
            max_pow2 <= 11,
            (1u32 << max_pow2 as u32) <= num_tables as u32,
        decreases 12 - max_pow2,
    ))]
    while (1u32 << (max_pow2 as u32 + 1)) <= num_tables as u32 {
        #[cfg(verus_keep_ghost)]
        proof! {
            let m = max_pow2 as u32;
            let n = num_tables as u32;
            assert(m <= 11 && n < 4096 && (1u32 << (m + 1)) <= n ==> m + 1 <= 11) by(bit_vector);
        }
        max_pow2 += 1;
    }
    #[cfg(verus_keep_ghost)]
    proof! {
        let m = max_pow2 as u32;
        let n = num_tables as u32;
        assert(m <= 11 ==> ((1u16 << m as u16) << 4u16) as int == 16 * (1u32 << m) as int) by(bit_vector);
        assert(m <= 11 && (1u32 << m) <= n && n < 4096 ==> 16 * (1u32 << m) as int <= 16 * n as int) by(bit_vector);
    }
    let search_range = (1u16 << max_pow2) << 4;
    let range_shift = 16 * num_tables - search_range;
    (search_range, max_pow2, range_shift)
}

/// Rule: a table offset must be 4-byte aligned (ots.cc ProcessGeneric:
/// "misaligned table").
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == (offset % 4 == 0)))]
pub fn offset_aligned(offset: u32) -> bool {
    #[cfg(verus_keep_ghost)]
    proof! {
        assert((offset & 3 == 0) == (offset % 4 == 0)) by(bit_vector);
    }
    offset & 3 == 0
}

/// Rule: a table offset must be at or after the end of the directory and
/// strictly before the end of the file (ots.cc ProcessGeneric:
/// "invalid table offset").
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == (data_offset <= offset as u64 && (offset as u64) < length)))]
pub fn offset_in_file(offset: u32, data_offset: u64, length: u64) -> bool {
    (offset as u64) >= data_offset && (offset as u64) < length
}

/// Rule: table length must be in [1, 1 GiB] (ots.cc ProcessGeneric:
/// "zero-length table" / "table length exceeds 1GB").
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == (1 <= length && length <= 1073741824)))]
pub fn table_length_ok(length: u32) -> bool {
    length >= 1 && (length as u64) <= MAX_FILE_SIZE
}

/// Rule: uncompressed_length must be >= compressed length (ots.cc
/// ProcessGeneric: "invalid compressed table").
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == (uncompressed >= compressed)))]
pub fn uncompressed_ge_compressed(uncompressed: u32, compressed: u32) -> bool {
    uncompressed >= compressed
}

/// Rule: a decompressed table must not exceed 150 MiB (ots.cc ProcessGeneric:
/// "decompressed table length exceeds ...").
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == (uncompressed <= 157286400)))]
pub fn decompressed_table_size_ok(uncompressed: u32) -> bool {
    (uncompressed as u64) <= MAX_DECOMPRESSED_TABLE_SIZE
}

/// Rule: the end of a table (rounded up to 4 for WOFF) must not overrun the
/// file (ots.cc ProcessGeneric: "table overruns end of file").
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        // offset + length (rounded up to 4 for WOFF; it cannot overflow) is a nonzero end within the file.
        r == ({
            let e = offset as int + length as int;
            let e = if woff { (e + 3) / 4 * 4 } else { e };
            e != 0 && e <= file_len as int
        }),
        r ==> offset as int + length as int <= file_len as int,
))]
pub fn table_end_in_file(offset: u32, length: u32, woff: bool, file_len: u64) -> bool {
    let end = offset as u64 + length as u64;
    let end = if woff {
        round4(end).unwrap_or(u64::MAX)
    } else {
        end
    };
    #[cfg(verus_keep_ghost)]
    proof! {
        let e = offset as int + length as int;
        assert((e + 3) / 4 * 4 >= e && (e + 3) / 4 * 4 <= e + 3);
    }
    end != 0 && end <= file_len
}

/// Rule: the running sum of decompressed table sizes must not overflow and
/// must stay <= 300 MiB (ots.cc ProcessGeneric: "overflow of decompressed
/// sum" / "decompressed sum exceeds ...").
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r is Some <==> sum as int + add as int <= 314572800,
        r matches Some(n) ==> n as int == sum as int + add as int,
))]
pub fn decompressed_sum_ok(sum: u64, add: u32) -> Option<u64> {
    let new = sum.checked_add(add as u64)?;
    if new > MAX_DECOMPRESSED_FILE_SIZE {
        return None;
    }
    Some(new)
}

/// Rule: WOFF totalSfntSize accumulates 12 + 16*numTables + Round4 of each
/// uncompressed length and must not exceed u32::MAX (ots.cc ProcessWOFF:
/// "sfnt size overflow").
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        // The accumulation never wraps: Some is the exact sum, and only while it fits in a u32.
        r is Some <==> acc as int + add as int <= 4294967295,
        r matches Some(n) ==> n as int == acc as int + add as int,
))]
pub fn woff_total_sfnt_size_ok(acc: u64, add: u64) -> Option<u64> {
    let new = acc.checked_add(add)?;
    if new > u32::MAX as u64 {
        return None;
    }
    Some(new)
}

/// Rule: a glyph section [gly_offset, gly_offset+gly_length) must lie inside
/// the glyf table (glyf.cc GetGlyphBufferSection).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == (gly_length == 0 || gly_offset as int + gly_length as int <= table_len as int)))]
pub fn glyph_section_in_table(gly_offset: u32, gly_length: u32, table_len: u64) -> bool {
    if gly_length == 0 {
        return true; // empty glyph: no data needed
    }
    let off = gly_offset as u64;
    if off >= table_len {
        return false;
    }
    let end = off.checked_add(gly_length as u64);
    match end {
        Some(e) => e <= table_len,
        None => false,
    }
}

/// Rule: output file must not exceed the 300 MiB stream size
/// (ots_glue.cc RustOTSStream::size).
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == (size <= 314572800)))]
pub fn output_size_ok(size: u64) -> bool {
    size <= MAX_DECOMPRESSED_FILE_SIZE
}

/// Rule: a declared decompressed size must be proportionate to the compressed
/// input, so a small hostile WOFF/WOFF2 header cannot make the decompressor
/// preallocate gigabytes. OTS streams output instead of preallocating, so it
/// never allocates more than the real decompressed size; wuff preallocates
/// totalSfntSize, so we cap the declared size at 64x input + 64 MiB.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == (declared as int <= 64 * input_len as int + 67108864)))]
pub fn declared_size_proportionate(declared: u64, input_len: u64) -> bool {
    // In u128 so the cap cannot overflow (`64 * input_len` in u64 panicked in debug builds for input_len >= 2^58 and
    // wrapped in release); the same result for every input where the u64 form did not overflow.
    (declared as u128) <= 64 * (input_len as u128) + 64 * 1024 * 1024
}
