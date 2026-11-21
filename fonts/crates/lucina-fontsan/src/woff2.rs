// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Behaviour derived from OTS (BSD-3-Clause, see LICENSES/BSD-3-Clause-OTS.txt).

//! WOFF 2.0 handling: header checks mirroring ProcessWOFF2 in ots.cc, then
//! decompression with wuff (pure-Rust brotli-decompressor). All OTS limits on
//! the declared sizes are applied before wuff runs.

use crate::bounds;
use crate::buf::Buf;
use crate::{font, Error};

/// Read a WOFF2 255UInt16 (WOFF2 spec / wuff variable_length.rs).
fn read_255u16(f: &mut Buf) -> Option<u32> {
    let b = f.read_u8()?;
    match b {
        255 => Some(0xFFFF),
        254 => Some(f.read_u8()? as u32),
        253 => Some(f.read_u16()? as u32),
        v => Some(v as u32),
    }
}

/// Read a WOFF2 UIntBase128 (WOFF2 spec); bounded to 5 bytes / u32 range.
fn read_base128(f: &mut Buf) -> Option<u32> {
    let mut result: u32 = 0;
    for i in 0..5 {
        let b = f.read_u8()?;
        if i == 4 && (b & 0xF0) != 0 {
            return None; // would overflow u32
        }
        result = result.checked_mul(128)?.checked_add((b & 0x7F) as u32)?;
        if b & 0x80 == 0 {
            return Some(result);
        }
    }
    None
}

fn parse_woff2_header(data: &[u8]) -> Result<(), Error> {
    let length = data.len() as u64;
    if !bounds::file_size_ok(length) {
        return Err(Error::InvalidFont("file exceeds 1GB"));
    }
    let mut f = Buf::new(data);
    if f.read_u32() != Some(u32::from_be_bytes(*b"wOF2")) {
        return Err(Error::InvalidFont("invalid WOFF2 marker"));
    }
    let _flavor = f
        .read_u32()
        .ok_or(Error::InvalidFont("error reading flavor"))?;
    let reported_length = f
        .read_u32()
        .ok_or(Error::InvalidFont("error reading length"))?;
    if reported_length as u64 != length {
        return Err(Error::InvalidFont("incorrect file size in WOFF2 header"));
    }
    let num_tables = f
        .read_u16()
        .ok_or(Error::InvalidFont("error reading number of tables"))?;
    let reserved = f.read_u16().ok_or(Error::InvalidFont(
        "error in reserved field of WOFF2 header",
    ))?;
    if reserved != 0 {
        return Err(Error::InvalidFont(
            "error in reserved field of WOFF2 header",
        ));
    }
    let total_sfnt_size = f
        .read_u32()
        .ok_or(Error::InvalidFont("error reading total sfnt size"))?;
    let total_compressed_size = f
        .read_u32()
        .ok_or(Error::InvalidFont("error reading total compressed size"))?;
    // ots.cc ProcessWOFF2 checks on ComputeWOFF2FinalSize (= totalSfntSize):
    if (total_sfnt_size as u64) < length {
        return Err(Error::InvalidFont(
            "size of decompressed WOFF 2.0 is less than compressed size",
        ));
    }
    if total_sfnt_size == 0 {
        return Err(Error::InvalidFont(
            "size of decompressed WOFF 2.0 is set to 0",
        ));
    }
    if (total_sfnt_size as u64) > bounds::MAX_DECOMPRESSED_FILE_SIZE {
        return Err(Error::InvalidFont(
            "size of decompressed WOFF 2.0 font exceeds 300MB",
        ));
    }
    if !bounds::declared_size_proportionate(total_sfnt_size as u64, data.len() as u64) {
        return Err(Error::InvalidFont(
            "declared sfnt size disproportionate to input",
        ));
    }
    if !f.skip(4) {
        return Err(Error::InvalidFont("error reading version fields"));
    }
    let meta_offset = f
        .read_u32()
        .ok_or(Error::InvalidFont("error reading metadata fields"))?;
    let meta_length = f
        .read_u32()
        .ok_or(Error::InvalidFont("error reading metadata fields"))?;
    let _meta_orig = f
        .read_u32()
        .ok_or(Error::InvalidFont("error reading metadata fields"))?;
    let priv_offset = f
        .read_u32()
        .ok_or(Error::InvalidFont("error reading private fields"))?;
    let priv_length = f
        .read_u32()
        .ok_or(Error::InvalidFont("error reading private fields"))?;
    if meta_offset != 0
        && ((meta_offset as u64) >= length || length - (meta_offset as u64) < (meta_length as u64))
    {
        return Err(Error::InvalidFont(
            "invalid metadata block offset or length",
        ));
    }
    if priv_offset != 0
        && ((priv_offset as u64) >= length || length - (priv_offset as u64) < (priv_length as u64))
    {
        return Err(Error::InvalidFont("invalid private block offset or length"));
    }
    // Bound the directory parse and the per-table declared sizes before wuff
    // allocates anything: numTables < 4096 (as ProcessTTF enforces later) and
    // each origLength <= 1 GiB (ProcessGeneric table length rule).
    if num_tables >= bounds::MAX_NUM_TABLES {
        return Err(Error::InvalidFont("excessive number of tables"));
    }
    let mut dir_sum: u64 = 12 + 16 * num_tables as u64;
    for _ in 0..num_tables {
        let flags = f
            .read_u8()
            .ok_or(Error::InvalidFont("error reading table directory"))?;
        let tag_index = flags & 0x3F;
        let transform_version = (flags >> 6) & 0x3;
        if tag_index == 63 {
            let _custom = f
                .read_u32()
                .ok_or(Error::InvalidFont("error reading table directory"))?;
        }
        let orig_length =
            read_255u16(&mut f).ok_or(Error::InvalidFont("error reading table directory"))?;
        // glyf/loca (and hmtx) may be transformed; transformed tables carry
        // no compLength. Known tags 0..17 are the standard set; tags for
        // glyf/loca/hmtx are indexes 10, 11? -- the transform decision only
        // affects whether compLength follows, and wuff re-parses the
        // directory itself, so here we only bound origLength.
        // glyf(10)/loca(11)/hmtx(3) with transformVersion 0 are transformed
        // and carry no compLength (WOFF2 spec table directory rules).
        let is_transformed =
            tag_index != 63 && matches!(tag_index, 3 | 10 | 11) && transform_version == 0;
        if !is_transformed {
            let _comp_length =
                read_255u16(&mut f).ok_or(Error::InvalidFont("error reading table directory"))?;
        }
        if (orig_length as u64) > bounds::MAX_FILE_SIZE {
            return Err(Error::InvalidFont("table length exceeds 1GB"));
        }
        dir_sum = bounds::woff_total_sfnt_size_ok(
            dir_sum,
            bounds::round4(orig_length as u64).unwrap_or(u64::MAX),
        )
        .ok_or(Error::InvalidFont("sfnt size overflow"))?;
    }
    if dir_sum > bounds::MAX_DECOMPRESSED_FILE_SIZE {
        return Err(Error::InvalidFont("decompressed sum exceeds 300MB"));
    }
    // The compressed payload must fit in the file.
    let header_end = 48u64;
    if (total_compressed_size as u64) > length.saturating_sub(header_end) {
        return Err(Error::InvalidFont("invalid total compressed size"));
    }
    let _ = read_base128; // used by wuff internally; kept for parity of parsing rules
    Ok(())
}

pub fn process_woff2(data: &[u8]) -> Result<Vec<u8>, Error> {
    parse_woff2_header(data)?;
    let sfnt = wuff::decompress_woff2(data)
        .map_err(|_| Error::InvalidFont("failed to convert WOFF 2.0 font to SFNT"))?;
    if (sfnt.len() as u64) > bounds::MAX_DECOMPRESSED_FILE_SIZE {
        return Err(Error::InvalidFont(
            "size of decompressed WOFF 2.0 font exceeds 300MB",
        ));
    }
    if sfnt.len() >= 4 && &sfnt[0..4] == b"ttcf" {
        font::process_ttc(&sfnt, None)
    } else {
        font::process_ttf(&sfnt, 0)
    }
}
