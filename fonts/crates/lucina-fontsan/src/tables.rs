// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Behaviour derived from OTS (BSD-3-Clause, see LICENSES/BSD-3-Clause-OTS.txt).

//! Per-table validation. Deep checks mirror the OTS parsers in
//! fontsan-0.7.0/src/deps/ots/src/*.cc; structural validation of layout,
//! colour and remaining tables uses read-fonts 0.46 typed parsing.
//! Required tables reject the font on failure; optional tables are dropped,
//! matching OTS's per-table Drop/Error decisions.

use crate::bounds;
use crate::buf::Buf;
use crate::Error;
use read_fonts::FontData;
use read_fonts::FontRead;

pub const fn tag(b: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*b)
}

/// ots.cc supported_tables, in OTS's parse order.
pub const SUPPORTED_TABLES: &[(u32, bool)] = &[
    (tag(b"maxp"), true),
    (tag(b"head"), true),
    (tag(b"OS/2"), true),
    (tag(b"cmap"), true),
    (tag(b"hhea"), true),
    (tag(b"hmtx"), true),
    (tag(b"name"), true),
    (tag(b"post"), true),
    (tag(b"loca"), false),
    (tag(b"glyf"), false),
    (tag(b"CFF "), false),
    (tag(b"VDMX"), false),
    (tag(b"hdmx"), false),
    (tag(b"gasp"), false),
    (tag(b"cvt "), false),
    (tag(b"fpgm"), false),
    (tag(b"prep"), false),
    (tag(b"LTSH"), false),
    (tag(b"VORG"), false),
    (tag(b"kern"), false),
    (tag(b"fvar"), false),
    (tag(b"avar"), false),
    (tag(b"cvar"), false),
    (tag(b"gvar"), false),
    (tag(b"HVAR"), false),
    (tag(b"MVAR"), false),
    (tag(b"STAT"), false),
    (tag(b"VVAR"), false),
    (tag(b"CFF2"), false),
    (tag(b"CPAL"), false),
    (tag(b"COLR"), false),
    (tag(b"GDEF"), false),
    (tag(b"GPOS"), false),
    (tag(b"GSUB"), false),
    (tag(b"vhea"), false),
    (tag(b"vmtx"), false),
    (tag(b"MATH"), false),
];

macro_rules! typed {
    ($ty:ty, $data:expr) => {
        <$ty>::read(FontData::new($data))
            .map(|_| ())
            .map_err(|_| "table failed read-fonts validation")
    };
}

pub enum Verdict {
    Keep,
    Drop,
}

/// Parsed facts from required tables that other validators need
/// (head/maxp/hhea in OTS).
#[derive(Default)]
pub struct FontInfo {
    pub num_glyphs: u16,
    pub maxp_version_1: bool,
    pub index_to_loc_format: i16,
    pub num_h_metrics: u16,
    /// Set by the loca validator (loca parses before glyf in OTS's order).
    pub loca_offsets: Option<Vec<u32>>,
}

fn invalid(msg: &'static str) -> Error {
    Error::InvalidFont(msg)
}

/// Validate one table. `required` tables return Err on failure (OTS rejects
/// the font); optional tables return Ok(Verdict::Drop) where OTS drops.
pub fn validate(
    t: u32,
    data: &[u8],
    info: &mut FontInfo,
    required: bool,
) -> Result<Verdict, Error> {
    let res: Result<Verdict, &'static str> = match t {
        x if x == tag(b"head") => parse_head(data, info).map(|_| Verdict::Keep),
        x if x == tag(b"maxp") => parse_maxp(data, info).map(|_| Verdict::Keep),
        x if x == tag(b"hhea") => parse_hhea(data, info).map(|_| Verdict::Keep),
        x if x == tag(b"hmtx") => parse_hmtx(data, info).map(|_| Verdict::Keep),
        x if x == tag(b"loca") => parse_loca(data, info).map(|offsets| {
            info.loca_offsets = Some(offsets);
            Verdict::Keep
        }),
        x if x == tag(b"glyf") => parse_glyf(data, info).map(|_| Verdict::Keep),
        x if x == tag(b"cmap") => parse_cmap(data, info).map(|_| Verdict::Keep),
        x if x == tag(b"CFF ") => parse_cff(data, info, false).map(|_| Verdict::Keep),
        x if x == tag(b"CFF2") => parse_cff(data, info, true).map(|_| Verdict::Keep),
        x if x == tag(b"name") => parse_name(data).map(|_| Verdict::Keep),
        x if x == tag(b"OS/2") => parse_os2(data).map(|_| Verdict::Keep),
        x if x == tag(b"post") => parse_post(data, info).map(|_| Verdict::Keep),
        x if x == tag(b"fvar") => parse_fvar(data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"avar") => typed!(read_fonts::tables::avar::Avar, data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"gvar") => parse_gvar(data, info)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"cvar") => typed!(read_fonts::tables::cvar::Cvar, data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"HVAR") => typed!(read_fonts::tables::hvar::Hvar, data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"MVAR") => parse_mvar(data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"STAT") => typed!(read_fonts::tables::stat::Stat, data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"VVAR") => typed!(read_fonts::tables::vvar::Vvar, data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"GDEF") => typed!(read_fonts::tables::gdef::Gdef, data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"GSUB") => typed!(read_fonts::tables::gsub::Gsub, data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"GPOS") => typed!(read_fonts::tables::gpos::Gpos, data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"kern") => typed!(read_fonts::tables::kern::Kern, data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"COLR") => typed!(read_fonts::tables::colr::Colr, data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"CPAL") => typed!(read_fonts::tables::cpal::Cpal, data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"MATH") => parse_math(data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"VDMX") => parse_vdmx(data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"hdmx") => parse_hdmx(data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"gasp") => typed!(read_fonts::tables::gasp::Gasp, data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"LTSH") => parse_ltsh(data, info)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"VORG") => typed!(read_fonts::tables::vorg::Vorg, data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"vhea") => parse_vhea(data, info)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"vmtx") => parse_vmtx(data, info)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"cvt ") => parse_bytecode_table(data)
            .map(|_| Verdict::Keep)
            .or(Ok(Verdict::Drop)),
        x if x == tag(b"fpgm") => Ok(Verdict::Keep),
        x if x == tag(b"prep") => Ok(Verdict::Keep),
        _ => Ok(Verdict::Drop), // unknown tags are dropped (TABLE_ACTION_DROP)
    };
    match res {
        Ok(v) => Ok(v),
        Err(msg) => {
            if required {
                Err(invalid(msg))
            } else {
                Ok(Verdict::Drop)
            }
        }
    }
}

// ---------------------------------------------------------------- head (head.cc)

fn parse_head(data: &[u8], info: &mut FontInfo) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    let version = t.read_u32().ok_or("failed to read table header")?;
    if version >> 16 != 1 {
        return Err("unsupported head majorVersion");
    }
    let _revision = t.read_u32().ok_or("failed to read table header")?;
    let _checksum = t.read_u32().ok_or("failed to read checksum")?;
    let magic = t.read_u32().ok_or("failed to read magicNumber")?;
    if magic != 0x5F0F_3CF5 {
        return Err("incorrect head magicNumber");
    }
    let _flags = t.read_u16().ok_or("failed to read flags")?;
    let upem = t.read_u16().ok_or("failed to read unitsPerEm")?;
    if !(16..=16384).contains(&upem) {
        return Err("unitsPerEm not in the range [16, 16384]");
    }
    if !t.skip(16) {
        return Err("can't read font dates");
    }
    let xmin = t.read_s16().ok_or("failed to read font bounding box")?;
    let ymin = t.read_s16().ok_or("failed to read font bounding box")?;
    let xmax = t.read_s16().ok_or("failed to read font bounding box")?;
    let ymax = t.read_s16().ok_or("failed to read font bounding box")?;
    if xmin > xmax || ymin > ymax {
        return Err("bad font bounding box");
    }
    let _mac_style = t.read_u16().ok_or("failed to read macStyle")?;
    let _min_ppem = t.read_u16().ok_or("failed to read lowestRecPPEM")?;
    if !t.skip(2) {
        return Err("failed to read fontDirectionHint");
    }
    let itlf = t.read_s16().ok_or("failed to read indexToLocFormat")?;
    if !(0..=1).contains(&itlf) {
        return Err("bad indexToLocFormat");
    }
    info.index_to_loc_format = itlf;
    let gdf = t.read_s16().ok_or("failed to read glyphDataFormat")?;
    if gdf != 0 {
        return Err("bad glyphDataFormat");
    }
    Ok(())
}

// ---------------------------------------------------------------- maxp (maxp.cc)

fn parse_maxp(data: &[u8], info: &mut FontInfo) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    let version = t.read_u32().ok_or("failed to read table version")?;
    if version >> 16 > 1 {
        return Err("unsupported maxp version");
    }
    let num_glyphs = t.read_u16().ok_or("failed to read numGlyphs")?;
    if num_glyphs == 0 {
        return Err("numGlyphs is 0");
    }
    info.num_glyphs = num_glyphs;
    info.maxp_version_1 = false;
    if version == 0x0000_5000 {
        return Ok(());
    }
    // version 1.0 fields; OTS downgrades to 0.5 if they don't fit.
    let mut v = [0u16; 13];
    for slot in v.iter_mut() {
        match t.read_u16() {
            Some(x) => *slot = x,
            None => return Ok(()),
        }
    }
    info.maxp_version_1 = true;
    let _ = v;
    Ok(())
}

// ------------------------------------------------------- hhea/vhea (metrics.cc)

fn parse_metrics_header(
    data: &[u8],
    info: &mut FontInfo,
    num_metrics_out: &mut u16,
) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    let version = t.read_u32().ok_or("failed to read table version")?;
    if version >> 16 != 1 {
        return Err("unsupported metrics header majorVersion");
    }
    if !t.skip(2 * 10) {
        return Err("failed to read table");
    }
    if !t.skip(8) {
        return Err("failed to read reserved bytes");
    }
    let data_format = t.read_s16().ok_or("failed to read metricDataFormat")?;
    if data_format != 0 {
        return Err("unsupported metricDataFormat");
    }
    let num_metrics = t.read_u16().ok_or("failed to read number of metrics")?;
    if num_metrics > info.num_glyphs {
        return Err("bad number of metrics");
    }
    *num_metrics_out = num_metrics;
    Ok(())
}

fn parse_hhea(data: &[u8], info: &mut FontInfo) -> Result<(), &'static str> {
    let mut nm = 0;
    parse_metrics_header(data, info, &mut nm)?;
    info.num_h_metrics = nm;
    Ok(())
}

fn parse_vhea(data: &[u8], _info: &mut FontInfo) -> Result<(), &'static str> {
    let mut nm = 0;
    parse_metrics_header(data, _info, &mut nm)
}

// ------------------------------------------------------- hmtx/vmtx (metrics.cc)

fn parse_hmtx(data: &[u8], info: &mut FontInfo) -> Result<(), &'static str> {
    let num_metrics = info.num_h_metrics as u64;
    let num_glyphs = info.num_glyphs as u64;
    if num_metrics > num_glyphs {
        return Err("bad number of metrics");
    }
    if num_metrics == 0 {
        return Err("no metrics");
    }
    let needed = 4 * num_metrics + 2 * (num_glyphs - num_metrics);
    if data.len() as u64 != needed {
        return Err("hmtx length mismatch");
    }
    Ok(())
}

fn parse_vmtx(data: &[u8], info: &mut FontInfo) -> Result<(), &'static str> {
    // vhea numberOfMetrics is not tracked separately; vmtx uses the same
    // layout rule against vhea's numberOfHMetrics. OTS shares the header
    // parse; we bound-check against numGlyphs conservatively.
    let num_glyphs = info.num_glyphs as u64;
    if (data.len() as u64) < 2 * num_glyphs {
        return Err("vmtx too short");
    }
    Ok(())
}

// ---------------------------------------------------------------- loca (loca.cc)

fn parse_loca(data: &[u8], info: &FontInfo) -> Result<Vec<u32>, &'static str> {
    let num_glyphs = info.num_glyphs as usize;
    let mut t = Buf::new(data);
    let mut offsets = Vec::with_capacity(num_glyphs + 1);
    let mut last: u32 = 0;
    if info.index_to_loc_format == 0 {
        for _ in 0..=num_glyphs {
            let o = t.read_u16().ok_or("failed to read loca offset")? as u32 * 2;
            if o < last {
                return Err("out of order loca offset");
            }
            last = o;
            offsets.push(o);
        }
    } else {
        for _ in 0..=num_glyphs {
            let o = t.read_u32().ok_or("failed to read loca offset")?;
            if o < last {
                return Err("out of order loca offset");
            }
            last = o;
            offsets.push(o);
        }
    }
    Ok(offsets)
}

// ---------------------------------------------------------------- glyf (glyf.cc)

fn parse_glyf(data: &[u8], info: &mut FontInfo) -> Result<(), &'static str> {
    let offsets = parse_loca_for_glyf(info)?;
    let num_glyphs = info.num_glyphs as usize;
    if offsets.len() != num_glyphs + 1 {
        return Err("invalid glyph offsets size");
    }
    let table_len = data.len() as u64;
    for gid in 0..num_glyphs {
        let start = offsets[gid] as u64;
        let end = offsets[gid + 1] as u64;
        let len = end - start;
        if len == 0 {
            continue;
        }
        if !bounds::glyph_section_in_table(start as u32, len as u32, table_len) {
            return Err("glyph offset/length outside glyf table");
        }
        let g = &data[start as usize..end as usize];
        let mut gb = Buf::new(g);
        let num_contours = gb.read_s16().ok_or("can't read glyph header")?;
        let _xmin = gb.read_s16().ok_or("can't read glyph header")?;
        let _ymin = gb.read_s16().ok_or("can't read glyph header")?;
        let _xmax = gb.read_s16().ok_or("can't read glyph header")?;
        let _ymax = gb.read_s16().ok_or("can't read glyph header")?;
        if num_contours <= -2 {
            return Err("bad number of contours in glyph");
        }
        if num_contours == 0 {
            continue;
        }
        if num_contours > 0 {
            parse_simple_glyph(&mut gb, num_contours)?;
        } else {
            parse_composite_glyph(&g[10..], &offsets, num_glyphs, gid, data)?;
        }
    }
    Ok(())
}

fn parse_loca_for_glyf(info: &FontInfo) -> Result<Vec<u32>, &'static str> {
    // loca was validated before glyf (OTS parse order); its offsets are
    // stored in FontInfo by the loca validator.
    info.loca_offsets
        .clone()
        .ok_or("loca not parsed before glyf")
}

fn parse_simple_glyph(g: &mut Buf, num_contours: i16) -> Result<(), &'static str> {
    let mut num_flags: u32 = 0;
    for i in 0..num_contours {
        let tmp = g.read_u16().ok_or("can't read contour index")?;
        if tmp == 0xFFFF {
            return Err("bad contour index");
        }
        if i > 0 && (tmp as u32) < num_flags {
            return Err("decreasing contour index");
        }
        num_flags = tmp as u32 + 1;
    }
    let bytecode_length = g.read_u16().ok_or("can't read bytecode length")? as usize;
    if !g.skip(bytecode_length) {
        return Err("can't read glyph bytecode");
    }
    // Read flags into a vector (bounded by num_flags <= 65536), with repeat
    // expansion, tracking the total coordinate byte length as OTS does.
    let mut flags = Vec::with_capacity(num_flags as usize);
    let mut coordinates_length: u32 = 0;
    while (flags.len() as u32) < num_flags {
        let flag = g.read_u8().ok_or("can't read flag")?;
        let mut delta: u32 = 0;
        if flag & (1 << 1) != 0 {
            delta += 1;
        } else if flag & (1 << 4) == 0 {
            delta += 2;
        }
        if flag & (1 << 2) != 0 {
            delta += 1;
        } else if flag & (1 << 5) == 0 {
            delta += 2;
        }
        if flag & (1 << 6) != 0 && !flags.is_empty() {
            return Err("bad glyph flag bit 6");
        }
        flags.push(flag & !(1 << 3));
        if flag & (1 << 3) != 0 {
            let repeat = g.read_u8().ok_or("can't read repeat value")?;
            if repeat == 0 {
                return Err("zero repeat");
            }
            delta += delta * repeat as u32;
            if flags.len() as u32 + repeat as u32 > num_flags {
                return Err("count too high");
            }
            for _ in 0..repeat {
                flags.push(flag & !(1 << 3));
            }
        }
        if flag & (1 << 7) != 0 {
            return Err("bad glyph flag reserved bit 7");
        }
        coordinates_length = coordinates_length.saturating_add(delta);
        if (g.len() - g.offset()) < coordinates_length as usize {
            return Err("glyph coordinates length bigger than glyph length");
        }
    }
    for &flag in &flags {
        if flag & (1 << 1) != 0 {
            if g.read_u8().is_none() {
                return Err("glyph too short");
            }
        } else if flag & (1 << 4) == 0 && g.read_s16().is_none() {
            return Err("glyph too short");
        }
    }
    for &flag in &flags {
        if flag & (1 << 2) != 0 {
            if g.read_u8().is_none() {
                return Err("glyph too short");
            }
        } else if flag & (1 << 5) == 0 && g.read_s16().is_none() {
            return Err("glyph too short");
        }
    }
    Ok(())
}

fn parse_composite_glyph(
    g: &[u8],
    offsets: &[u32],
    num_glyphs: usize,
    base_gid: usize,
    glyf_data: &[u8],
) -> Result<(), &'static str> {
    let mut gb = Buf::new(g);
    // first component set
    let mut stack: Vec<(u16, u32)> = Vec::new();
    let mut last_flags: u16;
    loop {
        let flags = gb.read_u16().ok_or("can't read composite flags")?;
        last_flags = flags;
        let gid = gb.read_u16().ok_or("can't read composite glyphIndex")?;
        if gid as usize >= num_glyphs {
            return Err("invalid glyph id in composite glyph");
        }
        if flags & 1 != 0 {
            if gb.read_s16().is_none() || gb.read_s16().is_none() {
                return Err("can't read arguments");
            }
        } else if gb.read_u8().is_none() || gb.read_u8().is_none() {
            return Err("can't read arguments");
        }
        if flags & (1 << 3) != 0 {
            if gb.read_s16().is_none() {
                return Err("can't read scale");
            }
        } else if flags & (1 << 6) != 0 {
            if gb.read_s16().is_none() || gb.read_s16().is_none() {
                return Err("can't read x/y scale");
            }
        } else if flags & (1 << 7) != 0 {
            for _ in 0..4 {
                if gb.read_s16().is_none() {
                    return Err("can't read transform");
                }
            }
        }
        stack.push((gid, 1));
        if flags & (1 << 5) == 0 {
            break;
        }
    }
    if last_flags & (1 << 8) != 0 {
        // WE_HAVE_INSTRUCTIONS
        let bytecode_length = gb.read_u16().ok_or("can't read instructions size")? as usize;
        if !gb.skip(bytecode_length) {
            return Err("can't read composite bytecode");
        }
    }
    let mut acc_points: u32 = 0;
    while let Some((gid, level)) = stack.pop() {
        if level > 0xFFFF {
            return Err("illegal component depth");
        }
        let start = offsets[gid as usize] as u64;
        let end = offsets[gid as usize + 1] as u64;
        let len = end - start;
        if len == 0 {
            continue;
        }
        if !bounds::glyph_section_in_table(start as u32, len as u32, glyf_data.len() as u64) {
            return Err("component glyph outside glyf table");
        }
        let cg = &glyf_data[start as usize..end as usize];
        let mut cb = Buf::new(cg);
        let nc = cb.read_s16().ok_or("can't read glyph header")?;
        if !cb.skip(8) {
            return Err("can't read glyph header");
        }
        if nc <= -2 {
            return Err("bad number of contours in component");
        }
        if nc == 0 {
            continue;
        }
        if nc > 0 {
            let mut num_points: u32 = 0;
            for _ in 0..nc {
                let tmp = cb.read_u16().ok_or("can't read contour index")? as u32;
                num_points = tmp + 1;
            }
            acc_points = acc_points.saturating_add(num_points);
            if acc_points > 0xFFFF {
                return Err("illegal composite points value");
            }
        } else {
            loop {
                let flags = cb.read_u16().ok_or("can't read composite flags")?;
                let cgid = cb.read_u16().ok_or("can't read composite glyphIndex")?;
                let mut skip = if flags & 1 != 0 { 4 } else { 2 };
                if flags & (1 << 3) != 0 {
                    skip += 2;
                } else if flags & (1 << 6) != 0 {
                    skip += 4;
                } else if flags & (1 << 7) != 0 {
                    skip += 8;
                }
                if !cb.skip(skip) {
                    return Err("failed to parse component glyph");
                }
                if cgid as usize >= num_glyphs {
                    return Err("invalid glyph id in composite glyph");
                }
                stack.push((cgid, level + 1));
                if flags & (1 << 5) == 0 {
                    break;
                }
            }
        }
    }
    let _ = base_gid;
    Ok(())
}

// ---------------------------------------------------------------- cmap (cmap.cc)

const UNICODE_UPPER_LIMIT: u32 = 0x10FFFF;
const MAX_CMAP_SELECTOR_RECORDS: u32 = 259;

fn parse_cmap(data: &[u8], info: &FontInfo) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    let version = t.read_u16().ok_or("can't read structure of cmap")?;
    let num_tables = t.read_u16().ok_or("can't read structure of cmap")?;
    if version != 0 {
        return Err("non zero cmap version");
    }
    if num_tables == 0 {
        return Err("no subtables in cmap");
    }
    struct Sub {
        platform: u16,
        encoding: u16,
        offset: u32,
        format: u16,
        length: u32,
    }
    let mut subs: Vec<Sub> = Vec::with_capacity(num_tables as usize);
    for _ in 0..num_tables {
        let platform = t.read_u16().ok_or("can't read subtable information")?;
        let encoding = t.read_u16().ok_or("can't read subtable information")?;
        let offset = t.read_u32().ok_or("can't read subtable information")?;
        subs.push(Sub {
            platform,
            encoding,
            offset,
            format: 0,
            length: 0,
        });
    }
    let data_offset = t.offset() as u64;
    let length = data.len() as u64;
    for s in subs.iter_mut() {
        if s.offset as u64 > bounds::MAX_FILE_SIZE {
            return Err("bad subtable offset in cmap");
        }
        if (s.offset as u64) < data_offset || (s.offset as u64) >= length {
            return Err("bad subtable offset in cmap");
        }
    }
    for s in subs.iter_mut() {
        let mut st = Buf::new(&data[s.offset as usize..]);
        s.format = st.read_u16().ok_or("can't read cmap subtable format")?;
        match s.format {
            0 | 4 => {
                let len = st.read_u16().ok_or("can't read cmap subtable length")? as u32;
                let _lang = st.read_u16().ok_or("can't read cmap subtable language")?;
                s.length = len;
            }
            12 | 13 => {
                if !st.skip(2) {
                    return Err("bad cmap subtable structure");
                }
                s.length = st.read_u32().ok_or("can't read cmap subtable length")?;
                let _lang = st.read_u32().ok_or("can't read cmap subtable language")?;
            }
            14 => {
                s.length = st.read_u32().ok_or("can't read cmap subtable length")?;
            }
            _ => {}
        }
    }
    for s in &subs {
        if s.length == 0 {
            continue;
        }
        if s.length as u64 > bounds::MAX_FILE_SIZE {
            return Err("bad cmap subtable length");
        }
        if s.offset as u64 + s.length as u64 > length {
            return Err("over long cmap subtable");
        }
    }
    // overlap check (identical (offset,end) pairs allowed)
    let mut events: Vec<(u32, i8)> = Vec::new();
    let mut seen: Vec<(u32, u32)> = Vec::new();
    for s in &subs {
        let end = s.offset.wrapping_add(s.length);
        if seen.contains(&(s.offset, end)) {
            continue;
        }
        seen.push((s.offset, end));
        events.push((s.offset, 1));
        events.push((end, -1));
    }
    events.sort();
    let mut count: i32 = 0;
    for (_, d) in &events {
        count += *d as i32;
        if count > 1 {
            return Err("excessive overlap in cmap subtables");
        }
    }
    let num_glyphs = info.num_glyphs;
    for s in &subs {
        let sub = &data[s.offset as usize..];
        if s.platform == 0 {
            // OTS lists encodings 0 and 1, then 3, as separate branches with the same parser; merged here.
            if (s.encoding == 0 || s.encoding == 1 || s.encoding == 3) && s.format == 4 {
                parse_format4(sub, num_glyphs)?;
            } else if (s.encoding == 3 || s.encoding == 4) && s.format == 12 {
                parse_format_12_13(sub, num_glyphs, false)?;
            } else if s.encoding == 5 && s.format == 14 {
                parse_format14(sub)?;
            }
        } else if s.platform == 1 {
            if s.encoding == 0 && s.format == 0 {
                parse_format0(sub)?;
            }
        } else if s.platform == 3 {
            match s.encoding {
                0 | 1 => {
                    if s.format == 4 {
                        parse_format4(sub, num_glyphs)?;
                    }
                }
                10 => {
                    if s.format == 12 {
                        parse_format_12_13(sub, num_glyphs, false)?;
                    } else if s.format == 13 {
                        parse_format_12_13(sub, num_glyphs, true)?;
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn parse_format4(data: &[u8], num_glyphs: u16) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    if !t.skip(4) {
        return Err("can't read start of cmap format 4 subtable");
    }
    let language = t.read_u16().ok_or("can't read language")?;
    if language != 0 {
        return Err("cmap format 4 language should be 0");
    }
    let segcountx2 = t.read_u16().ok_or("failed to read subcmap structure")?;
    let search_range = t.read_u16().ok_or("failed to read subcmap structure")?;
    let entry_selector = t.read_u16().ok_or("failed to read subcmap structure")?;
    let range_shift = t.read_u16().ok_or("failed to read subcmap structure")?;
    if segcountx2 & 1 != 0 || search_range & 1 != 0 {
        return Err("bad subcmap structure");
    }
    let segcount = segcountx2 >> 1;
    if segcount < 1 {
        return Err("segcount < 1");
    }
    let mut log2 = 0u16;
    while (1u32 << (log2 as u32 + 1)) <= segcount as u32 {
        log2 += 1;
    }
    if 2 * (1u16 << log2) != search_range {
        return Err("expected search range != search range");
    }
    if entry_selector != log2 {
        return Err("entry selector != log2(segcount)");
    }
    if range_shift != segcountx2 - search_range {
        return Err("unexpected range shift");
    }
    let mut end_range = Vec::with_capacity(segcount as usize);
    for _ in 0..segcount {
        end_range.push(t.read_u16().ok_or("failed to read segment")?);
    }
    let padding = t.read_u16().ok_or("failed to read segment padding")?;
    if padding != 0 {
        return Err("non zero cmap segment padding");
    }
    let mut start_range = Vec::with_capacity(segcount as usize);
    for _ in 0..segcount {
        start_range.push(t.read_u16().ok_or("failed to read segment start")?);
    }
    let mut id_delta = Vec::with_capacity(segcount as usize);
    for _ in 0..segcount {
        id_delta.push(t.read_s16().ok_or("failed to read segment delta")?);
    }
    let mut id_range_offset = Vec::with_capacity(segcount as usize);
    let mut id_range_offset_pos = Vec::with_capacity(segcount as usize);
    for i in 0..segcount as usize {
        id_range_offset_pos.push(t.offset() as u32);
        let mut v = t.read_u16().ok_or("failed to read segment range offset")?;
        if v & 1 != 0 {
            if i == segcount as usize - 1 {
                v = 0; // tolerated on the final segment
            } else {
                return Err("bad segment offset");
            }
        }
        id_range_offset.push(v);
    }
    for i in 1..segcount as usize {
        if i == segcount as usize - 1
            && start_range[i - 1] == 0xFFFF
            && end_range[i - 1] == 0xFFFF
            && start_range[i] == 0xFFFF
            && end_range[i] == 0xFFFF
        {
            continue; // multiple 0xffff terminators tolerated
        }
        if end_range[i] <= end_range[i - 1] {
            return Err("out of order end range");
        }
        if start_range[i] <= end_range[i - 1] {
            return Err("out of order start range");
        }
    }
    if start_range[segcount as usize - 1] != 0xFFFF || end_range[segcount as usize - 1] != 0xFFFF {
        return Err("final segment must be 0xFFFF-0xFFFF");
    }
    let length = data.len() as u64;
    for i in 0..segcount as usize {
        let mut cp = start_range[i] as u32;
        while cp <= end_range[i] as u32 {
            let code_point = cp as u16;
            if id_range_offset[i] == 0 {
                let glyph = code_point.wrapping_add(id_delta[i] as u16);
                if glyph as u32 >= num_glyphs as u32 {
                    return Err("range glyph reference too high");
                }
            } else {
                let range_delta = (code_point - start_range[i]) as u32;
                let glyph_id_offset = id_range_offset_pos[i] as u64
                    + id_range_offset[i] as u64
                    + range_delta as u64 * 2;
                if glyph_id_offset + 1 >= length {
                    return Err("bad glyph id offset");
                }
                let bytes = &data[glyph_id_offset as usize..glyph_id_offset as usize + 2];
                let glyph = u16::from_be_bytes([bytes[0], bytes[1]]);
                if glyph as u32 >= num_glyphs as u32 {
                    return Err("range glyph reference too high");
                }
            }
            if cp == 0xFFFF {
                break;
            }
            cp += 1;
        }
    }
    Ok(())
}

fn parse_format_12_13(data: &[u8], num_glyphs: u16, is_13: bool) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    if !t.skip(8) {
        return Err("failed to skip format 12/13 header");
    }
    let language = t.read_u32().ok_or("can't read language")?;
    if language != 0 {
        return Err("format 12/13 language should be zero");
    }
    let num_groups = t.read_u32().ok_or("can't read group count")?;
    if num_groups == 0 || (t.remaining() / 12) < num_groups as usize {
        return Err("bad group count");
    }
    let mut prev_start: u32 = 0;
    let mut prev_end: u32 = 0;
    for i in 0..num_groups {
        let start = t.read_u32().ok_or("can't read group")?;
        let end = t.read_u32().ok_or("can't read group")?;
        let gid = t.read_u32().ok_or("can't read group")?;
        if start > UNICODE_UPPER_LIMIT || end > UNICODE_UPPER_LIMIT || gid > 0xFFFF {
            return Err("bad format 12/13 group");
        }
        if !is_13 {
            if end < start {
                return Err("group end before start");
            }
            if (end - start) + gid > num_glyphs as u32 {
                return Err("bad group startGlyphID");
            }
        } else if gid >= num_glyphs as u32 {
            return Err("subrange starting glyph id too high");
        }
        if i > 0 {
            if start <= prev_start {
                return Err("out of order groups");
            }
            if start <= prev_end {
                return Err("overlapping groups");
            }
        }
        prev_start = start;
        prev_end = end;
    }
    Ok(())
}

fn parse_format14(data: &[u8]) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    if !t.skip(6) {
        return Err("can't read start of format 14 subtable");
    }
    let num_records = t.read_u32().ok_or("can't read record count")?;
    if num_records == 0 || num_records > MAX_CMAP_SELECTOR_RECORDS {
        return Err("bad format 14 record count");
    }
    let length = data.len() as u64;
    let mut records: Vec<(u32, u32, u32)> = Vec::with_capacity(num_records as usize);
    for i in 0..num_records {
        let b0 = t.read_u8().ok_or("can't read record")? as u32;
        let b1 = t.read_u8().ok_or("can't read record")? as u32;
        let b2 = t.read_u8().ok_or("can't read record")? as u32;
        let var_selector = (b0 << 16) | (b1 << 8) | b2;
        let default_offset = t.read_u32().ok_or("can't read record")?;
        let non_default_offset = t.read_u32().ok_or("can't read record")?;
        let in_range = (0x180B..=0x180D).contains(&var_selector)
            || (0xFE00..=0xFE0F).contains(&var_selector)
            || (0xE0100..=0xE01EF).contains(&var_selector);
        if !in_range {
            return Err("bad variation selector");
        }
        if i > 0 && records[(i - 1) as usize].0 >= var_selector {
            return Err("out of order variation selector");
        }
        if default_offset == 0 && non_default_offset == 0 {
            return Err("no default offset in variation selector record");
        }
        if default_offset != 0 && (default_offset as u64) >= length {
            return Err("default offset too high");
        }
        if non_default_offset != 0 && (non_default_offset as u64) >= length {
            return Err("non default offset too high");
        }
        records.push((var_selector, default_offset, non_default_offset));
    }
    for (_, def, nondef) in &records {
        if *def != 0 {
            let mut st = Buf::new(&data[*def as usize..]);
            let num_ranges = st.read_u32().ok_or("can't read ranges")?;
            if num_ranges == 0 || st.remaining() / 4 < num_ranges as usize {
                return Err("bad number of ranges");
            }
            let mut last: u32 = 0;
            for _ in 0..num_ranges {
                let b0 = st.read_u8().ok_or("can't read range")? as u32;
                let b1 = st.read_u8().ok_or("can't read range")? as u32;
                let b2 = st.read_u8().ok_or("can't read range")? as u32;
                let uv = (b0 << 16) | (b1 << 8) | b2;
                let add = st.read_u8().ok_or("can't read range")? as u32;
                let check = uv + add;
                if uv == 0
                    || uv > UNICODE_UPPER_LIMIT
                    || check > 0xFFFFFF
                    || (last != 0 && uv <= last)
                {
                    return Err("bad unicode value in variation range");
                }
                last = check;
            }
        }
        if *nondef != 0 {
            let mut st = Buf::new(&data[*nondef as usize..]);
            let num_mappings = st.read_u32().ok_or("can't read mappings")?;
            if num_mappings == 0 || st.remaining() / 5 < num_mappings as usize {
                return Err("bad number of mappings");
            }
            let mut last: u32 = 0;
            for _ in 0..num_mappings {
                let b0 = st.read_u8().ok_or("can't read mapping")? as u32;
                let b1 = st.read_u8().ok_or("can't read mapping")? as u32;
                let b2 = st.read_u8().ok_or("can't read mapping")? as u32;
                let uv = (b0 << 16) | (b1 << 8) | b2;
                let gid = st.read_u16().ok_or("can't read mapping")?;
                if gid == 0 || uv == 0 {
                    return Err("bad mapping");
                }
                if uv > UNICODE_UPPER_LIMIT {
                    return Err("invalid unicode value in mapping");
                }
                if last != 0 && uv <= last {
                    return Err("out of order unicode value in mapping");
                }
                last = uv;
            }
        }
    }
    Ok(())
}

fn parse_format0(data: &[u8]) -> Result<(), &'static str> {
    if data.len() < 6 + 256 {
        return Err("format 0 cmap too short");
    }
    Ok(())
}

// ---------------------------------------------------------------- CFF/CFF2 (cff.cc)

fn parse_cff(data: &[u8], info: &FontInfo, is_cff2: bool) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    let major = t.read_u8().ok_or("failed to read CFF header")?;
    if major != 1 && !(is_cff2 && major == 2) {
        return Err("unsupported CFF major version");
    }
    let minor = t.read_u8().ok_or("failed to read CFF header")?;
    let hdr_size = t.read_u8().ok_or("failed to read CFF header")? as usize;
    if is_cff2 {
        // cff.cc OpenTypeCFF2::Parse: header is major/minor/hdrSize/topDictLength.
        if minor != 0 {
            return Err("unsupported CFF2 minor version");
        }
        let top_dict_size = t.read_u16().ok_or("failed to read CFF2 header")? as usize;
        if hdr_size >= data.len() || top_dict_size == 0 || hdr_size + top_dict_size > data.len() {
            return Err("bad CFF2 header sizes");
        }
        // Global Subrs INDEX follows the Top DICT; each CFF2 INDEX carries its
        // own offSize. FDArray INDEX follows; charstrings location requires
        // Top DICT parsing, so we bound-check the remaining INDEXes instead.
        t.set_offset(hdr_size + top_dict_size);
        read_cff2_index(&mut t)?; // Global Subrs
        let fdarray = read_cff2_index(&mut t)?; // FDArray
        if fdarray == 0 {
            return Err("CFF2 FDArray INDEX empty");
        }
        return Ok(());
    }
    let _minor = minor;
    let off_size = t.read_u8().ok_or("failed to read CFF header")?;
    if !(1..=4).contains(&off_size) {
        return Err("bad CFF offSize");
    }
    if hdr_size != 4 || hdr_size >= data.len() {
        return Err("bad CFF hdrSize");
    }
    let _ = off_size;
    t.set_offset(hdr_size);
    // Name INDEX
    let (name_count, _) = read_cff_index(&mut t)?;
    if name_count == 0 {
        return Err("CFF Name INDEX empty");
    }
    // Top DICT INDEX
    let (top_count, _) = read_cff_index(&mut t)?;
    if top_count == 0 {
        return Err("CFF Top DICT INDEX empty");
    }
    // String INDEX
    let _ = read_cff_index(&mut t)?;
    // Global Subr INDEX
    let _ = read_cff_index(&mut t)?;
    // CharStrings INDEX is located via the Top DICT (operator 17).
    let mut td = Buf::new(data);
    td.set_offset(hdr_size);
    let (_, _) = read_cff_index(&mut td)?; // Name INDEX
    let (_, top_offsets) = read_cff_index(&mut td)?; // Top DICT INDEX
    if top_offsets.len() < 2 {
        return Err("bad CFF Top DICT INDEX");
    }
    let dict = &data[top_offsets[0]..top_offsets[1]];
    let cs_off =
        parse_cff_dict_charstrings(dict).ok_or("CFF Top DICT has no CharStrings")? as usize;
    if cs_off >= data.len() {
        return Err("CFF CharStrings offset out of bounds");
    }
    let mut cs = Buf::new(&data[cs_off..]);
    let (cs_count, _) = read_cff_index(&mut cs)?;
    if cs_count as u64 != info.num_glyphs as u64 {
        return Err("CFF charstrings count != numGlyphs");
    }
    Ok(())
}

/// Minimal CFF1 DICT parser: returns the operand of the CharStrings
/// operator (op 17) in a Top DICT.
fn parse_cff_dict_charstrings(dict: &[u8]) -> Option<u32> {
    let mut i = 0usize;
    let mut operands: Vec<i64> = Vec::new();
    while i < dict.len() {
        let b = dict[i];
        if b <= 21 {
            // operator
            if b == 17 && !operands.is_empty() {
                return operands.last().map(|&v| v as u32);
            }
            operands.clear();
            i += 1;
            if b == 12 {
                i += 1; // two-byte operator
            }
        } else if b == 28 {
            if i + 3 > dict.len() {
                return None;
            }
            operands.push(i16::from_be_bytes([dict[i + 1], dict[i + 2]]) as i64);
            i += 3;
        } else if b == 29 {
            if i + 5 > dict.len() {
                return None;
            }
            operands.push(
                i32::from_be_bytes([dict[i + 1], dict[i + 2], dict[i + 3], dict[i + 4]]) as i64,
            );
            i += 5;
        } else if b == 30 {
            // real number: skip to terminator nibble
            i += 1;
            loop {
                if i >= dict.len() {
                    return None;
                }
                let byte = dict[i];
                i += 1;
                if (byte & 0x0F) == 0x0F || (byte >> 4) == 0x0F {
                    break;
                }
            }
            operands.push(0);
        } else if (32..=246).contains(&b) {
            operands.push(b as i64 - 139);
            i += 1;
        } else if (247..=250).contains(&b) {
            if i + 2 > dict.len() {
                return None;
            }
            operands.push(108 + ((b as i64 - 247) << 8) + dict[i + 1] as i64);
            i += 2;
        } else if (251..=254).contains(&b) {
            if i + 2 > dict.len() {
                return None;
            }
            operands.push(-108 - ((b as i64 - 251) << 8) - dict[i + 1] as i64);
            i += 2;
        } else if b == 255 {
            if i + 5 > dict.len() {
                return None;
            }
            operands.push(
                i32::from_be_bytes([dict[i + 1], dict[i + 2], dict[i + 3], dict[i + 4]]) as i64,
            );
            i += 5;
        } else {
            return None;
        }
    }
    None
}

fn read_cff_index_count(t: &mut Buf) -> Result<u32, &'static str> {
    t.read_u16()
        .map(|v| v as u32)
        .ok_or("failed to read CFF INDEX count")
}

fn read_cff_index(t: &mut Buf) -> Result<(u32, Vec<usize>), &'static str> {
    // cff.cc ParseIndex: count u16, offSize u8, then count+1 offsets of
    // offSize bytes, 1-based relative to the object data that follows.
    let count = read_cff_index_count(t)?;
    if count == 0 {
        return Ok((0, Vec::new())); // empty INDEX
    }
    let off_size = t.read_u8().ok_or("failed to read CFF INDEX offSize")?;
    if !(1..=4).contains(&off_size) {
        return Err("bad CFF INDEX offSize");
    }
    read_cff_index_body(t, off_size, count)
}

fn read_cff_index_body(
    t: &mut Buf,
    off_size: u8,
    count: u32,
) -> Result<(u32, Vec<usize>), &'static str> {
    let mut rel = Vec::with_capacity(count as usize + 1);
    for i in 0..=count {
        let mut v: u64 = 0;
        for _ in 0..off_size {
            v = (v << 8) | t.read_u8().ok_or("failed to read CFF INDEX offset")? as u64;
        }
        if v < 1 || (i == 0 && v != 1) {
            return Err("bad CFF INDEX offset");
        }
        if v > t.len() as u64 {
            return Err("CFF INDEX offset out of bounds");
        }
        rel.push(v);
    }
    let data_start = t.offset() as u64;
    for i in 1..rel.len() {
        if rel[i] < rel[i - 1] {
            return Err("out of order CFF INDEX offset");
        }
    }
    let end = data_start + rel[count as usize] - 1;
    if end >= t.len() as u64 {
        return Err("CFF INDEX data out of bounds");
    }
    // Skip the object data so the next INDEX starts after it.
    t.set_offset(end as usize);
    Ok((
        count,
        rel.iter().map(|&r| (data_start + r - 1) as usize).collect(),
    ))
}

/// CFF2 INDEX: count u32, offSize u8, offsets (cff.cc ParseIndex cff2=true).
fn read_cff2_index(t: &mut Buf) -> Result<u32, &'static str> {
    let count = t.read_u32().ok_or("failed to read CFF2 INDEX count")?;
    if count == 0 {
        return Ok(0);
    }
    let off_size = t.read_u8().ok_or("failed to read CFF2 INDEX offSize")?;
    if !(1..=4).contains(&off_size) {
        return Err("bad CFF2 INDEX offSize");
    }
    let (c, _) = read_cff_index_body(t, off_size, count)?;
    Ok(c)
}

// ---------------------------------------------------------------- name (name.cc)

fn parse_name(data: &[u8]) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    let format = t.read_u16().ok_or("failed to read name format")?;
    if format > 1 {
        return Err("bad name format");
    }
    let count = t.read_u16().ok_or("failed to read name count")?;
    let string_offset = t.read_u16().ok_or("failed to read stringOffset")? as usize;
    if string_offset > data.len() {
        return Err("bad stringOffset");
    }
    for _ in 0..count {
        let platform = t.read_u16().ok_or("failed to read name entry")?;
        let encoding = t.read_u16().ok_or("failed to read name entry")?;
        let _language = t.read_u16().ok_or("failed to read name entry")?;
        let _name_id = t.read_u16().ok_or("failed to read name entry")?;
        let name_length = t.read_u16().ok_or("failed to read name entry")? as usize;
        let name_offset = t.read_u16().ok_or("failed to read name entry")? as usize;
        match platform {
            0 if encoding <= 6 => {}
            1 if encoding <= 32 => {}
            2 if encoding <= 2 => {}
            3 if encoding <= 6 || encoding == 10 => {}
            4 if encoding <= 255 => {}
            _ => continue, // unknown platform/encoding: record discarded
        }
        let name_end = string_offset + name_offset + name_length;
        if name_end > data.len() {
            continue; // out-of-bounds record discarded
        }
    }
    if format == 1 {
        let lang_tag_count = t.read_u16().ok_or("failed to read langTagCount")?;
        for _ in 0..lang_tag_count {
            let tag_length = t.read_u16().ok_or("failed to read langTagRecord")? as usize;
            let tag_offset = t.read_u16().ok_or("failed to read langTagRecord")? as usize;
            let tag_end = string_offset + tag_offset + tag_length;
            if tag_end > data.len() {
                return Err("bad end of langTagRecord");
            }
            if tag_length > 200 {
                return Err("too long language tag");
            }
        }
    }
    if t.offset() > string_offset {
        return Err("bad name table offset");
    }
    Ok(())
}

// ---------------------------------------------------------------- OS/2 (os2.cc)

fn parse_os2(data: &[u8]) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    let version = t.read_u16().ok_or("error reading OS/2 version")?;
    if version > 5 {
        return Err("unsupported OS/2 table version");
    }
    // minimum lengths per version (os2.cc offsetof checks)
    let min_len: usize = match version {
        0 => 78,
        1 => 86,
        2..=4 => 96,
        _ => 100,
    };
    if data.len() < min_len {
        // OTS downgrades the version when fields are missing; accept as the
        // lower version instead of rejecting.
        if data.len() >= 78 {
            return Ok(());
        }
        return Err("OS/2 table too short");
    }
    Ok(())
}

// ---------------------------------------------------------------- post (post.cc)

fn parse_post(data: &[u8], info: &FontInfo) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    let version = t.read_u32().ok_or("failed to read post version")?;
    if version != 0x0001_0000 && version != 0x0002_0000 && version != 0x0003_0000 {
        return Err("unsupported post table version");
    }
    if !t.skip(4 + 2 + 2 + 4 + 16) {
        return Err("failed to read post header");
    }
    if version != 0x0002_0000 {
        return Ok(());
    }
    let num_glyphs = t.read_u16().ok_or("failed to read numberOfGlyphs")?;
    if num_glyphs == 0 {
        if info.num_glyphs > 258 {
            return Err("no glyphs in post table but more than 258 in font");
        }
        return Ok(());
    }
    if num_glyphs != info.num_glyphs {
        return Err("bad number of glyphs in post");
    }
    for _ in 0..num_glyphs {
        let idx = t.read_u16().ok_or("failed to read glyph name index")?;
        let _ = idx;
    }
    // Pascal strings
    let strings = t.rest();
    let mut i = 0usize;
    let mut names = 0usize;
    while i < strings.len() {
        let l = strings[i] as usize;
        if i + 1 + l > strings.len() {
            return Err("bad post string length");
        }
        if strings[i + 1..i + 1 + l].contains(&0) {
            return Err("bad post string");
        }
        i += 1 + l;
        names += 1;
    }
    let _ = names;
    Ok(())
}

// ---------------------------------------------------------------- fvar (fvar.cc)

fn parse_fvar(data: &[u8]) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    let major = t.read_u16().ok_or("failed to read fvar header")?;
    let _minor = t.read_u16().ok_or("failed to read fvar header")?;
    let axes_array_offset = t.read_u16().ok_or("failed to read fvar header")? as usize;
    let reserved = t.read_u16().ok_or("failed to read fvar header")?;
    let axis_count = t.read_u16().ok_or("failed to read fvar header")?;
    let axis_size = t.read_u16().ok_or("failed to read fvar header")?;
    let instance_count = t.read_u16().ok_or("failed to read fvar header")?;
    let instance_size = t.read_u16().ok_or("failed to read fvar header")?;
    if major != 1 {
        return Err("unknown fvar version");
    }
    if axes_array_offset > data.len() || axes_array_offset < t.offset() {
        return Err("bad axesArrayOffset");
    }
    if reserved != 2 {
        // OTS only warns and fixes; accept.
    }
    if axis_count == 0 {
        return Err("no variation axes");
    }
    if axis_size != 20 {
        return Err("invalid fvar axisSize");
    }
    if instance_size as u64 != axis_count as u64 * 4 + 6
        && instance_size as u64 != axis_count as u64 * 4 + 4
    {
        return Err("invalid fvar instanceSize");
    }
    t.set_offset(axes_array_offset);
    for _ in 0..axis_count {
        let axis_tag = t.read_u32().ok_or("failed to read axis record")?;
        let min = t.read_s32().ok_or("failed to read axis record")?;
        let default = t.read_s32().ok_or("failed to read axis record")?;
        let max = t.read_s32().ok_or("failed to read axis record")?;
        let _flags = t.read_u16().ok_or("failed to read axis record")?;
        let _name_id = t.read_u16().ok_or("failed to read axis record")?;
        if !check_tag(axis_tag) {
            return Err("bad axis tag");
        }
        if !(min <= default && default <= max) {
            return Err("bad axis value range");
        }
    }
    for _ in 0..instance_count {
        let _subfamily = t.read_u16().ok_or("failed to read instance record")?;
        let _flags = t.read_u16().ok_or("failed to read instance record")?;
        for _ in 0..axis_count {
            let _coord = t.read_s32().ok_or("failed to read instance coordinates")?;
        }
        if instance_size as u64 == axis_count as u64 * 4 + 6 {
            let _ps = t.read_u16().ok_or("failed to read instance psname ID")?;
        }
    }
    Ok(())
}

// ------------------------------------------------- vdmx/ltsh/cvt (vdmx.cc etc.)

fn parse_vdmx(data: &[u8]) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    let version = t.read_u16().ok_or("failed to read vdmx version")?;
    if version != 1 {
        return Err("unsupported vdmx version");
    }
    let num_records = t.read_u16().ok_or("failed to read vdmx record count")? as usize;
    let num_ratios = t.read_u16().ok_or("failed to read vdmx ratio count")? as usize;
    if num_records as u64 * 24 > data.len() as u64 || num_ratios == 0 {
        return Err("bad vdmx record count");
    }
    Ok(())
}

fn parse_ltsh(data: &[u8], info: &FontInfo) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    let num_glyphs = t.read_u16().ok_or("failed to read LTSH numGlyphs")?;
    if num_glyphs != info.num_glyphs {
        return Err("LTSH numGlyphs != maxp numGlyphs");
    }
    if data.len() < 4 + 2 * num_glyphs as usize {
        return Err("LTSH too short");
    }
    Ok(())
}

fn parse_math(data: &[u8]) -> Result<(), &'static str> {
    // math_.cc: version 1.0, then constant/variation offsets within the table.
    let mut t = Buf::new(data);
    let version = t.read_u32().ok_or("failed to read MATH version")?;
    if version >> 16 != 1 {
        return Err("unsupported MATH version");
    }
    let len = data.len() as u64;
    for _ in 0..3 {
        let off = t.read_u16().ok_or("failed to read MATH offset")? as u64;
        if off > len {
            return Err("MATH offset out of bounds");
        }
    }
    Ok(())
}

fn parse_hdmx(data: &[u8]) -> Result<(), &'static str> {
    // hdmx.cc: version u16, numRecords u16, sizeDeviceRecord u32, then records
    // of (u8 pad, u8 maxWidth, u8 widths[numGlyphs]). Bounded by table length.
    let mut t = Buf::new(data);
    let version = t.read_u16().ok_or("failed to read hdmx version")?;
    if version != 2 {
        return Err("unsupported hdmx version");
    }
    let num_records = t.read_u16().ok_or("failed to read hdmx numRecords")? as usize;
    let size_device_record = t.read_u32().ok_or("failed to read hdmx sizeDeviceRecord")? as usize;
    if size_device_record < 2 {
        return Err("bad hdmx sizeDeviceRecord");
    }
    if num_records as u64 * size_device_record as u64 > data.len() as u64 {
        return Err("hdmx records out of bounds");
    }
    Ok(())
}

fn parse_bytecode_table(data: &[u8]) -> Result<(), &'static str> {
    // cvt is an array of u16 (cvt.cc reads u16s until the end).
    if !data.len().is_multiple_of(2) {
        return Err("odd-length cvt table");
    }
    Ok(())
}

fn check_tag(tag_value: u32) -> bool {
    let mut v = tag_value;
    for _ in 0..4 {
        let c = v & 0xFF;
        if !(32..=126).contains(&c) {
            return false;
        }
        v >>= 8;
    }
    true
}

// ------------------------------------------------- gvar/hvar/mvar (gvar.cc etc.)

fn parse_gvar(data: &[u8], info: &FontInfo) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    let major = t.read_u16().ok_or("failed to read gvar header")?;
    if major != 1 {
        return Err("unknown gvar version");
    }
    let _minor = t.read_u16().ok_or("failed to read gvar header")?;
    let axis_count = t.read_u16().ok_or("failed to read gvar header")?;
    let shared_tuple_count = t.read_u16().ok_or("failed to read gvar header")? as u32;
    let shared_tuples_offset = t.read_u32().ok_or("failed to read gvar header")? as u64;
    let glyph_count = t.read_u16().ok_or("failed to read gvar header")?;
    let flags = t.read_u16().ok_or("failed to read gvar header")?;
    let glyph_variation_data_array_offset =
        t.read_u32().ok_or("failed to read gvar header")? as u64;
    if glyph_count != info.num_glyphs {
        return Err("gvar glyphCount != numGlyphs");
    }
    let len = data.len() as u64;
    if shared_tuples_offset > len || glyph_variation_data_array_offset > len {
        return Err("gvar offset out of bounds");
    }
    // offsets array: glyph_count+1 entries of u16/u32 depending on flags bit 0
    let long_offsets = flags & 1 != 0;
    let entry_size = if long_offsets { 4 } else { 2 };
    let needed = (glyph_count as u64 + 1) * entry_size;
    if (t.remaining() as u64) < needed {
        return Err("gvar offsets array out of bounds");
    }
    let _ = (axis_count, shared_tuple_count);
    Ok(())
}

fn parse_mvar(data: &[u8]) -> Result<(), &'static str> {
    let mut t = Buf::new(data);
    // mvar.cc: majorVersion u16, minorVersion u16, reserved u16,
    // valueRecordSize u16, valueRecordCount u16, itemVariationStoreOffset u16.
    let version = t.read_u32().ok_or("failed to read MVAR header")?;
    if version != 0x0001_0000 {
        return Err("unknown MVAR version");
    }
    let _reserved = t.read_u16().ok_or("failed to read MVAR header")?;
    let value_record_size = t.read_u16().ok_or("failed to read MVAR header")? as u64;
    let value_record_count = t.read_u16().ok_or("failed to read MVAR header")? as u64;
    let store_offset = t.read_u16().ok_or("failed to read MVAR header")? as u64;
    let len = data.len() as u64;
    if value_record_size < 8 && value_record_count != 0 {
        return Err("bad MVAR valueRecordSize");
    }
    if value_record_count == 0 && store_offset != 0 {
        return Err("bad MVAR itemVariationStoreOffset");
    }
    if store_offset != 0 {
        if store_offset < 12 || store_offset > len {
            return Err("MVAR itemVariationStoreOffset out of bounds");
        }
        typed!(
            read_fonts::tables::variations::ItemVariationStore,
            &data[store_offset as usize..]
        )
        .map_err(|_| "bad MVAR ItemVariationStore")?;
    }
    let records_end = 12 + value_record_count * value_record_size;
    if records_end > len {
        return Err("MVAR value records out of bounds");
    }
    Ok(())
}
