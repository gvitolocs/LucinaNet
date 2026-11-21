// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Behaviour derived from OTS (BSD-3-Clause, see LICENSES/BSD-3-Clause-OTS.txt).

//! sfnt/TTC directory processing and output serialization, mirroring
//! ProcessTTF / ProcessTTC / ProcessGeneric in ots.cc.

use crate::bounds;
use crate::buf::Buf;
use crate::tables::{self, FontInfo, Verdict};
use crate::Error;

#[derive(Clone, Copy)]
pub struct TableEntry {
    pub tag: u32,
    pub offset: u32,
    pub length: u32,
    pub uncompressed_length: u32,
}

pub const fn tag(b: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*b)
}

fn valid_version_tag(version: u32) -> Option<u32> {
    match version {
        0x0001_0000 | 0x4F54_544F => Some(version), // 0x00010000, 'OTTO'
        0x7472_7565 => Some(0x0001_0000),           // 'true' -> 0x00010000
        _ => None,
    }
}

/// ots.cc ProcessTTF.
pub fn process_ttf(data: &[u8], offset: usize) -> Result<Vec<u8>, Error> {
    let length = data.len() as u64;
    if offset as u64 > length {
        return Err(Error::InvalidFont("offset beyond end of file"));
    }
    if !bounds::file_size_ok(length) {
        return Err(Error::InvalidFont("file exceeds 1GB"));
    }
    let mut f = Buf::new(&data[offset..]);
    let version_raw = f
        .read_u32()
        .ok_or(Error::InvalidFont("error reading sfntVersion"))?;
    let version =
        valid_version_tag(version_raw).ok_or(Error::InvalidFont("invalid sfntVersion"))?;
    let num_tables = f.read_u16().ok_or(Error::InvalidFont(
        "error reading table directory search header",
    ))?;
    let _search_range = f.read_u16().ok_or(Error::InvalidFont(
        "error reading table directory search header",
    ))?;
    let _entry_selector = f.read_u16().ok_or(Error::InvalidFont(
        "error reading table directory search header",
    ))?;
    let _range_shift = f.read_u16().ok_or(Error::InvalidFont(
        "error reading table directory search header",
    ))?;
    // OTS tolerates and fixes bad searchRange/entrySelector/rangeShift; the
    // values are recomputed on output anyway.
    if !bounds::num_tables_ok(num_tables) {
        return Err(Error::InvalidFont("excessive (or zero) number of tables"));
    }
    let mut tables = Vec::with_capacity(num_tables as usize);
    for _ in 0..num_tables {
        let t = TableEntry {
            tag: f
                .read_u32()
                .ok_or(Error::InvalidFont("error reading table directory"))?,
            offset: {
                let _chksum = f
                    .read_u32()
                    .ok_or(Error::InvalidFont("error reading table directory"))?;
                f.read_u32()
                    .ok_or(Error::InvalidFont("error reading table directory"))?
            },
            length: f
                .read_u32()
                .ok_or(Error::InvalidFont("error reading table directory"))?,
            uncompressed_length: 0,
        };
        let mut t = t;
        t.uncompressed_length = t.length;
        tables.push(t);
    }
    process_generic(data, version, &tables, false)
}

/// ots.cc ProcessTTC (index == -1: process every face, write a collection).
pub fn process_ttc(data: &[u8], _index: Option<u32>) -> Result<Vec<u8>, Error> {
    let length = data.len() as u64;
    if !bounds::file_size_ok(length) {
        return Err(Error::InvalidFont("file exceeds 1GB"));
    }
    let mut f = Buf::new(data);
    if f.read_u32() != Some(tag(b"ttcf")) {
        return Err(Error::InvalidFont("invalid TTC tag"));
    }
    let ttc_version = f
        .read_u32()
        .ok_or(Error::InvalidFont("error reading TTC version"))?;
    if ttc_version != 0x0001_0000 && ttc_version != 0x0002_0000 {
        return Err(Error::InvalidFont("invalid TTC version"));
    }
    let num_fonts = f
        .read_u32()
        .ok_or(Error::InvalidFont("error reading number of TTC fonts"))?;
    if num_fonts > bounds::MAX_TTC_FONTS {
        return Err(Error::InvalidFont("too many fonts in TTC"));
    }
    let mut offsets = Vec::with_capacity(num_fonts as usize);
    for _ in 0..num_fonts {
        offsets.push(
            f.read_u32()
                .ok_or(Error::InvalidFont("error reading offset to OffsetTable"))?,
        );
    }
    if ttc_version == 0x0002_0000 && !f.skip(12) {
        return Err(Error::InvalidFont(
            "error reading DSIG offset and length in TTC font",
        ));
    }
    // Process every face; each face is sanitized independently and the
    // collection is rebuilt (ots.cc ProcessTTC index == -1).
    let mut faces = Vec::with_capacity(num_fonts as usize);
    for &off in &offsets {
        faces.push(process_ttf(data, off as usize)?);
    }
    // Assemble a ttcf 1.0 header pointing at the sanitized faces.
    let mut out = Vec::new();
    let header_len = 12 + 4 * num_fonts as usize;
    let mut face_offsets = Vec::with_capacity(faces.len());
    let mut cursor = header_len;
    for face in &faces {
        face_offsets.push(cursor as u32);
        cursor = match bounds::round4(cursor as u64 + face.len() as u64) {
            Some(v) if v <= u32::MAX as u64 => v as usize,
            _ => return Err(Error::TooLarge("ttcf face offset overflow")),
        };
    }
    if !bounds::output_size_ok(cursor as u64) {
        return Err(Error::TooLarge("output exceeds 300MB"));
    }
    out.extend_from_slice(b"ttcf");
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&num_fonts.to_be_bytes());
    for o in &face_offsets {
        out.extend_from_slice(&o.to_be_bytes());
    }
    for face in &faces {
        out.extend_from_slice(face);
        let pad = (4 - (out.len() & 3)) % 4;
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    Ok(out)
}

/// ots.cc ProcessGeneric: per-table directory checks, table whitelist,
/// validation, and output serialization.
fn process_generic(
    data: &[u8],
    version: u32,
    tables: &[TableEntry],
    woff: bool,
) -> Result<Vec<u8>, Error> {
    let length = data.len() as u64;
    let data_offset = 12 + 16 * tables.len() as u64;

    let mut uncompressed_sum: u64 = 0;
    for t in tables {
        // OTS only warns on unsorted directories and bad tags.
        if !bounds::offset_aligned(t.offset) {
            return Err(Error::InvalidFont("misaligned table"));
        }
        if !bounds::offset_in_file(t.offset, data_offset, length) {
            return Err(Error::InvalidFont("invalid table offset"));
        }
        if !bounds::table_length_ok(t.length) {
            return Err(Error::InvalidFont("zero-length or >1GB table"));
        }
        if !bounds::uncompressed_ge_compressed(t.uncompressed_length, t.length) {
            return Err(Error::InvalidFont("invalid compressed table"));
        }
        if t.uncompressed_length > t.length {
            if !bounds::decompressed_table_size_ok(t.uncompressed_length) {
                return Err(Error::InvalidFont(
                    "decompressed table length exceeds 150MB",
                ));
            }
            uncompressed_sum = bounds::decompressed_sum_ok(uncompressed_sum, t.uncompressed_length)
                .ok_or(Error::InvalidFont("decompressed sum exceeds 300MB"))?;
        }
        if !bounds::table_end_in_file(t.offset, t.length, woff, length) {
            return Err(Error::InvalidFont("table overruns end of file"));
        }
    }
    if uncompressed_sum > bounds::MAX_DECOMPRESSED_FILE_SIZE {
        return Err(Error::InvalidFont("decompressed sum exceeds 300MB"));
    }

    // Overlap check (ots.cc ProcessGeneric "overlapping tables").
    let mut events: Vec<(u32, i8)> = Vec::with_capacity(tables.len() * 2);
    for t in tables {
        events.push((t.offset, 1));
        events.push((t.offset.wrapping_add(t.length), -1));
    }
    events.sort();
    let mut count: i32 = 0;
    for (_, d) in &events {
        count += *d as i32;
        if count > 1 {
            return Err(Error::InvalidFont("overlapping tables"));
        }
    }

    // Last directory entry wins for duplicate tags (std::map assignment).
    let mut table_map: Vec<(u32, TableEntry)> = Vec::with_capacity(tables.len());
    for t in tables {
        if let Some(slot) = table_map.iter_mut().find(|(tag, _)| *tag == t.tag) {
            slot.1 = *t;
        } else {
            table_map.push((t.tag, *t));
        }
    }

    let mut info = FontInfo::default();
    let mut kept: Vec<(u32, Vec<u8>)> = Vec::new();

    // Parse supported tables first, in OTS's supported_tables order.
    for &(t, required) in tables::SUPPORTED_TABLES {
        match table_map.iter().find(|(tag, _)| *tag == t) {
            None => {
                if required {
                    return Err(Error::InvalidFont("missing required table"));
                }
            }
            Some((_, entry)) => {
                let bytes =
                    &data[entry.offset as usize..entry.offset as usize + entry.length as usize];
                match tables::validate(t, bytes, &mut info, required)? {
                    Verdict::Keep => kept.push((t, bytes.to_vec())),
                    Verdict::Drop => {}
                }
            }
        }
    }

    // glyf+loca vs CFF/CFF2 rule (ots.cc ProcessGeneric).
    let has_glyf = kept.iter().any(|(t, _)| *t == tag(b"glyf"));
    let has_loca = kept.iter().any(|(t, _)| *t == tag(b"loca"));
    let has_cff = kept.iter().any(|(t, _)| *t == tag(b"CFF "))
        || kept.iter().any(|(t, _)| *t == tag(b"CFF2"));
    if has_glyf && has_loca {
        if !info.maxp_version_1 {
            return Err(Error::InvalidFont("wrong maxp version for glyph data"));
        }
        kept.retain(|(t, _)| *t != tag(b"CFF ") && *t != tag(b"CFF2"));
    } else if has_cff {
        kept.retain(|(t, _)| *t != tag(b"glyf") && *t != tag(b"loca"));
    } else {
        return Err(Error::InvalidFont(
            "no supported glyph data table(s) present",
        ));
    }

    write_sfnt(version, &kept)
}

/// Serialize the sanitized sfnt exactly as ots.cc ProcessGeneric writes it:
/// directory sorted by tag, recomputed search fields, 4-byte padded tables,
/// recomputed checksums and head.checkSumAdjustment.
fn write_sfnt(version: u32, kept: &[(u32, Vec<u8>)]) -> Result<Vec<u8>, Error> {
    let mut kept: Vec<(u32, &[u8])> = kept.iter().map(|(t, b)| (*t, b.as_slice())).collect();
    kept.sort_by_key(|(t, _)| *t);
    let n = kept.len();
    if n == 0 || n >= bounds::MAX_NUM_TABLES as usize {
        return Err(Error::InvalidFont("bad output table count"));
    }
    let (search_range, entry_selector, range_shift) = bounds::search_fields(n as u16);

    let mut offsets = Vec::with_capacity(n);
    let mut cursor = (12 + 16 * n) as u64;
    for (_, b) in &kept {
        offsets.push(cursor as u32);
        cursor = bounds::round4(cursor + b.len() as u64)
            .ok_or(Error::TooLarge("output offset overflow"))?;
    }
    if !bounds::output_size_ok(cursor) {
        return Err(Error::TooLarge("output exceeds 300MB"));
    }

    let mut out = Vec::with_capacity(cursor as usize);
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());
    let mut checksums = Vec::with_capacity(n);
    for (i, (t, b)) in kept.iter().enumerate() {
        // head.cc Serialize writes checkSumAdjustment as 0, so the head
        // directory checksum is computed with those 4 bytes zeroed.
        if *t == tag(b"head") && b.len() >= 12 {
            let mut z = b.to_vec();
            z[8..12].copy_from_slice(&[0, 0, 0, 0]);
            checksums.push(checksum(&z));
        } else {
            checksums.push(checksum(b));
        }
        out.extend_from_slice(&t.to_be_bytes());
        out.extend_from_slice(&checksums[i].to_be_bytes());
        out.extend_from_slice(&offsets[i].to_be_bytes());
        out.extend_from_slice(&(b.len() as u32).to_be_bytes());
    }
    let mut head_dir_index = None;
    for (i, (t, _)) in kept.iter().enumerate() {
        if *t == tag(b"head") {
            head_dir_index = Some(i);
        }
    }
    for (_, b) in kept.iter() {
        out.extend_from_slice(b);
        let pad = (4 - (out.len() & 3)) % 4;
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    // head.checkSumAdjustment = 0xB1B0AFBA - checksum(whole file, adjustment=0)
    if let Some(i) = head_dir_index {
        let head_off = offsets[i] as usize;
        // ots.cc: zero checkSumAdjustment before the whole-file checksum.
        out[head_off + 8..head_off + 12].copy_from_slice(&[0, 0, 0, 0]);
        let adj = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
        out[head_off + 8..head_off + 12].copy_from_slice(&adj.to_be_bytes());
    }
    Ok(out)
}

/// Table checksum: sum of big-endian u32s over the 4-byte padded data
/// (ots.h ots_checksum / wuff compute_checksum).
pub fn checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    let (chunks, rem) = data.as_chunks::<4>();
    for c in chunks {
        sum = sum.wrapping_add(u32::from_be_bytes(*c));
    }
    let mut tail = [0u8; 4];
    tail[..rem.len()].copy_from_slice(rem);
    sum = sum.wrapping_add(u32::from_be_bytes(tail));
    sum
}
