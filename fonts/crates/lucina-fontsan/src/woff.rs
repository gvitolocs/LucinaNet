// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Behaviour derived from OTS (BSD-3-Clause, see LICENSES/BSD-3-Clause-OTS.txt).

//! WOFF 1.0 handling: header/directory checks mirroring ProcessWOFF in
//! ots.cc, then whole-file decompression with wuff (pure-Rust flate2).

use crate::bounds;
use crate::buf::Buf;
use crate::{font, Error};

/// WOFF header + table directory, validated per ots.cc ProcessWOFF before any
/// decompression happens, so a hostile header cannot make wuff allocate
/// gigabytes.
struct WoffHeader {
    flavor: u32,
    num_tables: u16,
}

fn parse_woff_header(data: &[u8]) -> Result<WoffHeader, Error> {
    let length = data.len() as u64;
    // ots.cc ProcessWOFF: "file exceeds 1GB"
    if !bounds::file_size_ok(length) {
        return Err(Error::InvalidFont("file exceeds 1GB"));
    }
    let mut f = Buf::new(data);
    if f.read_u32() != Some(u32::from_be_bytes(*b"wOFF")) {
        return Err(Error::InvalidFont("invalid WOFF marker"));
    }
    let flavor = f
        .read_u32()
        .ok_or(Error::InvalidFont("error reading sfntVersion"))?;
    match flavor {
        0x0001_0000 | 0x4F54_544F | 0x7472_7565 => {}
        _ => return Err(Error::InvalidFont("invalid sfntVersion")),
    }
    let reported_length = f
        .read_u32()
        .ok_or(Error::InvalidFont("error reading file length"))?;
    if reported_length as u64 != length {
        return Err(Error::InvalidFont("incorrect file size in WOFF header"));
    }
    let num_tables = f
        .read_u16()
        .ok_or(Error::InvalidFont("error reading number of tables"))?;
    if num_tables == 0 {
        return Err(Error::InvalidFont("error reading number of tables"));
    }
    let reserved = f
        .read_u16()
        .ok_or(Error::InvalidFont("error in reserved field of WOFF header"))?;
    if reserved != 0 {
        return Err(Error::InvalidFont("error in reserved field of WOFF header"));
    }
    let total_sfnt_size = f
        .read_u32()
        .ok_or(Error::InvalidFont("error reading total sfnt size"))?;
    // majorVersion/minorVersion are not checked by OTS.
    if !f.skip(4) {
        return Err(Error::InvalidFont("failed to read version fields"));
    }
    let meta_offset = f
        .read_u32()
        .ok_or(Error::InvalidFont("failed to read metadata block fields"))?;
    let meta_length = f
        .read_u32()
        .ok_or(Error::InvalidFont("failed to read metadata block fields"))?;
    let _meta_length_orig = f
        .read_u32()
        .ok_or(Error::InvalidFont("failed to read metadata block fields"))?;
    if meta_offset != 0
        && ((meta_offset as u64) >= length || length - (meta_offset as u64) < (meta_length as u64))
    {
        return Err(Error::InvalidFont(
            "invalid metadata block offset or length",
        ));
    }
    let priv_offset = f
        .read_u32()
        .ok_or(Error::InvalidFont("failed to read private block fields"))?;
    let priv_length = f
        .read_u32()
        .ok_or(Error::InvalidFont("failed to read private block fields"))?;
    if priv_offset != 0
        && ((priv_offset as u64) >= length || length - (priv_offset as u64) < (priv_length as u64))
    {
        return Err(Error::InvalidFont("invalid private block offset or length"));
    }

    // Table directory.
    let mut total: u64 = 12 + 16 * num_tables as u64;
    let mut first_offset = u32::MAX;
    let mut last_offset = 0u32;
    let mut last_length = 0u32;
    for _ in 0..num_tables {
        let _tag = f
            .read_u32()
            .ok_or(Error::InvalidFont("error reading table directory"))?;
        let offset = f
            .read_u32()
            .ok_or(Error::InvalidFont("error reading table directory"))?;
        let comp_length = f
            .read_u32()
            .ok_or(Error::InvalidFont("error reading table directory"))?;
        let orig_length = f
            .read_u32()
            .ok_or(Error::InvalidFont("error reading table directory"))?;
        let _chksum = f
            .read_u32()
            .ok_or(Error::InvalidFont("error reading table directory"))?;
        // ots.cc: origLength >= compLength rule and 150 MiB decompressed cap
        // are re-checked per table in ProcessGeneric; apply them here too so
        // wuff never sees a hostile directory.
        if !bounds::uncompressed_ge_compressed(orig_length, comp_length) {
            return Err(Error::InvalidFont("invalid compressed table"));
        }
        if orig_length > comp_length && !bounds::decompressed_table_size_ok(orig_length) {
            return Err(Error::InvalidFont(
                "decompressed table length exceeds 150MB",
            ));
        }
        total = bounds::woff_total_sfnt_size_ok(
            total,
            bounds::round4(orig_length as u64).unwrap_or(u64::MAX),
        )
        .ok_or(Error::InvalidFont("sfnt size overflow"))?;
        if offset < first_offset {
            first_offset = offset;
        }
        if offset >= last_offset {
            last_offset = offset;
            last_length = comp_length;
        }
    }
    if total_sfnt_size as u64 != total {
        return Err(Error::InvalidFont("uncompressed sfnt size mismatch"));
    }
    // Table data must follow immediately after the header.
    let dir_end = bounds::round4(f.offset() as u64)
        .ok_or(Error::InvalidFont("junk before tables in WOFF file"))?;
    if first_offset as u64 != dir_end {
        return Err(Error::InvalidFont("junk before tables in WOFF file"));
    }
    if (last_offset as u64) >= length || length - (last_offset as u64) < (last_length as u64) {
        return Err(Error::InvalidFont("invalid table location/size"));
    }
    let mut block_end = bounds::round4(last_offset as u64 + last_length as u64)
        .ok_or(Error::InvalidFont("invalid table location/size"))?;
    if block_end > u32::MAX as u64 {
        return Err(Error::InvalidFont("invalid table location/size"));
    }
    if meta_offset != 0 {
        if block_end != meta_offset as u64 {
            return Err(Error::InvalidFont("invalid metadata block offset"));
        }
        block_end = bounds::round4(meta_offset as u64 + meta_length as u64)
            .ok_or(Error::InvalidFont("invalid metadata block length"))?;
        if block_end > u32::MAX as u64 {
            return Err(Error::InvalidFont("invalid metadata block length"));
        }
    }
    if priv_offset != 0 {
        if block_end != priv_offset as u64 {
            return Err(Error::InvalidFont("invalid private block offset"));
        }
        block_end = bounds::round4(priv_offset as u64 + priv_length as u64)
            .ok_or(Error::InvalidFont("invalid private block length"))?;
        if block_end > u32::MAX as u64 {
            return Err(Error::InvalidFont("invalid private block length"));
        }
    }
    if block_end != bounds::round4(length).unwrap_or(u64::MAX) {
        return Err(Error::InvalidFont("file length mismatch (trailing junk?)"));
    }
    // The decompressed sfnt must fit the 300 MiB limit (ots.cc ProcessWOFF2
    // applies the same cap to WOFF2; WOFF1 goes through the same output
    // stream limit in ProcessGeneric).
    if (total_sfnt_size as u64) > bounds::MAX_DECOMPRESSED_FILE_SIZE {
        return Err(Error::InvalidFont("decompressed sum exceeds 300MB"));
    }
    if !bounds::declared_size_proportionate(total_sfnt_size as u64, data.len() as u64) {
        return Err(Error::InvalidFont(
            "declared sfnt size disproportionate to input",
        ));
    }
    Ok(WoffHeader { flavor, num_tables })
}

pub fn process_woff(data: &[u8]) -> Result<Vec<u8>, Error> {
    let hdr = parse_woff_header(data)?;
    let _ = hdr.num_tables;
    // Decompress the whole WOFF into an sfnt with wuff (flate2/miniz_oxide).
    // All OTS header limits were applied above, so wuff's allocation is
    // bounded by totalSfntSize <= 300 MiB.
    let sfnt = wuff::decompress_woff1(data)
        .map_err(|_| Error::InvalidFont("failed to decompress WOFF"))?;
    if (sfnt.len() as u64) > bounds::MAX_DECOMPRESSED_FILE_SIZE {
        return Err(Error::InvalidFont("decompressed sum exceeds 300MB"));
    }
    let _ = hdr.flavor;
    // The decompressed data is a plain sfnt (or ttcf); run the normal path.
    if sfnt.len() >= 4 && &sfnt[0..4] == b"ttcf" {
        font::process_ttc(&sfnt, None)
    } else {
        font::process_ttf(&sfnt, 0)
    }
}
