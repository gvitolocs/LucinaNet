// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Behaviour derived from OTS (BSD-3-Clause, see LICENSES/BSD-3-Clause-OTS.txt).

//! Hostile corpus generator + smoke test. Generates hostile files into
//! samples/hostile/ (a temporary directory, rebuilt by the generator) and checks that lucina-fontsan never
//! panics on them. Accept/reject agreement with OTS is checked by
//! tests/oracle.rs, which also scans samples/hostile/.

use std::path::PathBuf;

/// Directory with the sample fonts: the env var LUCINANET_FONT_SAMPLES (the fonts are not in this repo).
fn samples_dir() -> PathBuf {
    std::env::var_os("LUCINANET_FONT_SAMPLES")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// True when the sample fonts are present; tests return early (printing a note) when they are not.
fn samples_available(test: &str) -> bool {
    let dir = samples_dir();
    if dir.as_os_str().is_empty() || !dir.is_dir() {
        eprintln!("{test}: skipped, set LUCINANET_FONT_SAMPLES to a directory of sample fonts");
        return false;
    }
    true
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}
fn be16(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}

/// Minimal sfnt builder from (tag, data) pairs.
fn build_sfnt(version: u32, tables: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let n = tables.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&be32(version));
    out.extend_from_slice(&be16(n));
    out.extend_from_slice(&be16(16));
    out.extend_from_slice(&be16(0));
    out.extend_from_slice(&be16(16 * n - 16));
    let mut cursor = (12 + 16 * tables.len()) as u32;
    let mut dir = Vec::new();
    let mut body = Vec::new();
    for (t, d) in tables {
        dir.extend_from_slice(&be32(*t));
        dir.extend_from_slice(&be32(0));
        dir.extend_from_slice(&be32(cursor));
        dir.extend_from_slice(&be32(d.len() as u32));
        body.extend_from_slice(d);
        let pad = (4 - (body.len() & 3)) % 4;
        body.extend(std::iter::repeat_n(0u8, pad));
        cursor += d.len() as u32 + pad as u32;
    }
    out.extend_from_slice(&dir);
    out.extend_from_slice(&body);
    out
}

fn tag(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

/// Build a minimal valid-ish TTF skeleton (head/maxp/hhea/hmtx/cmap/name/OS-2/post/loca/glyf).
fn base_font() -> Vec<u8> {
    let head = {
        let mut v = Vec::new();
        v.extend_from_slice(&be32(0x00010000));
        v.extend_from_slice(&be32(0x00010000));
        v.extend_from_slice(&be32(0));
        v.extend_from_slice(&be32(0x5F0F3CF5));
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&be16(1000));
        v.extend_from_slice(&[0u8; 16]);
        v.extend_from_slice(&be16(0)); // xmin
        v.extend_from_slice(&be16(0)); // ymin
        v.extend_from_slice(&be16(100)); // xmax
        v.extend_from_slice(&be16(100)); // ymax
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&be16(2));
        v.extend_from_slice(&be16(1)); // indexToLocFormat long
        v.extend_from_slice(&be16(0));
        v
    };
    let maxp = {
        let mut v = Vec::new();
        v.extend_from_slice(&be32(0x00010000));
        v.extend_from_slice(&be16(2)); // numGlyphs
        v.extend(std::iter::repeat_n(0u8, 26));
        v
    };
    let hhea = {
        let mut v = Vec::new();
        v.extend_from_slice(&be32(0x00010000));
        v.extend_from_slice(&be16(800));
        v.extend_from_slice(&be16((-200i16) as u16));
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&be16(1000));
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&be16(1000));
        v.extend_from_slice(&be16(1));
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&[0u8; 8]);
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&be16(1)); // numberOfHMetrics
        v
    };
    let hmtx = {
        let mut v = Vec::new();
        v.extend_from_slice(&be16(500));
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&be16(0));
        v
    };
    let cmap = {
        let mut v = Vec::new();
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&be16(1));
        v.extend_from_slice(&be16(3));
        v.extend_from_slice(&be16(1));
        v.extend_from_slice(&be32(12));
        // format 4 subtable: one segment 0x41-0x41 -> glyph 1, terminator
        let mut s = Vec::new();
        s.extend_from_slice(&be16(4));
        s.extend_from_slice(&be16(16 + 8 + 8 + 2 + 8 + 8)); // length
        s.extend_from_slice(&be16(0));
        s.extend_from_slice(&be16(2 * 2)); // segCountX2
        s.extend_from_slice(&be16(4)); // searchRange
        s.extend_from_slice(&be16(1)); // entrySelector
        s.extend_from_slice(&be16(0)); // rangeShift
        s.extend_from_slice(&be16(0x41)); // endCode[0]
        s.extend_from_slice(&be16(0xFFFF)); // endCode[1]
        s.extend_from_slice(&be16(0)); // pad
        s.extend_from_slice(&be16(0x41)); // startCode[0]
        s.extend_from_slice(&be16(0xFFFF)); // startCode[1]
        s.extend_from_slice(&be16((1i32 - 0x41) as u16)); // idDelta[0]
        s.extend_from_slice(&be16(1)); // idDelta[1] (0xFFFF+1=0)
        s.extend_from_slice(&be16(0));
        s.extend_from_slice(&be16(0));
        v.extend_from_slice(&s);
        v
    };
    let name = {
        let mut v = Vec::new();
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&be16(6));
        v
    };
    let os2 = {
        let mut v = vec![0u8; 78];
        v[0..2].copy_from_slice(&be16(0));
        v
    };
    let post = {
        let mut v = Vec::new();
        v.extend_from_slice(&be32(0x00030000));
        v.extend_from_slice(&be32(0));
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&be16(1));
        v.extend_from_slice(&be32(0));
        v.extend(std::iter::repeat_n(0u8, 16));
        v
    };
    let loca = {
        let mut v = Vec::new();
        v.extend_from_slice(&be32(0));
        v.extend_from_slice(&be32(0));
        v.extend_from_slice(&be32(0));
        v
    };
    let glyf = vec![0u8; 4];
    build_sfnt(
        0x00010000,
        &[
            (tag(b"OS/2"), os2),
            (tag(b"cmap"), cmap),
            (tag(b"glyf"), glyf),
            (tag(b"head"), head),
            (tag(b"hhea"), hhea),
            (tag(b"hmtx"), hmtx),
            (tag(b"loca"), loca),
            (tag(b"maxp"), maxp),
            (tag(b"name"), name),
            (tag(b"post"), post),
        ],
    )
}

/// Wrap an sfnt as a WOFF1 file (tables stored uncompressed).
fn wrap_woff(sfnt: &[u8], total_sfnt_size_override: Option<u32>) -> Vec<u8> {
    let n = u16::from_be_bytes([sfnt[4], sfnt[5]]) as usize;
    let mut out = Vec::new();
    out.extend_from_slice(b"wOFF");
    out.extend_from_slice(&be32(0x00010000));
    let dir_end = 44 + 20 * n;
    let mut total = (12 + 16 * n) as u32;
    let mut entries: Vec<(u32, u32, u32)> = Vec::new();
    for i in 0..n {
        let off = 12 + 16 * i;
        let t = u32::from_be_bytes([sfnt[off], sfnt[off + 1], sfnt[off + 2], sfnt[off + 3]]);
        let toff =
            u32::from_be_bytes([sfnt[off + 8], sfnt[off + 9], sfnt[off + 10], sfnt[off + 11]]);
        let tlen = u32::from_be_bytes([
            sfnt[off + 12],
            sfnt[off + 13],
            sfnt[off + 14],
            sfnt[off + 15],
        ]);
        entries.push((t, toff, tlen));
        total += (tlen + 3) & !3;
    }
    let total = total_sfnt_size_override.unwrap_or(total);
    out.extend_from_slice(&be32(0)); // length patched later
    out.extend_from_slice(&be16(n as u16));
    out.extend_from_slice(&be16(0));
    out.extend_from_slice(&be32(total));
    out.extend_from_slice(&be16(1));
    out.extend_from_slice(&be16(0));
    out.extend_from_slice(&be32(0));
    out.extend_from_slice(&be32(0));
    out.extend_from_slice(&be32(0));
    out.extend_from_slice(&be32(0));
    out.extend_from_slice(&be32(0));
    let mut cursor = dir_end as u32;
    for (t, _toff, tlen) in &entries {
        out.extend_from_slice(&be32(*t));
        out.extend_from_slice(&be32(cursor));
        out.extend_from_slice(&be32(*tlen));
        out.extend_from_slice(&be32(*tlen));
        out.extend_from_slice(&be32(0));
        cursor += (tlen + 3) & !3;
    }
    for (_, toff, tlen) in &entries {
        out.extend_from_slice(&sfnt[*toff as usize..*toff as usize + *tlen as usize]);
        let pad = (4 - (out.len() & 3)) % 4;
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    let len = out.len() as u32;
    out[8..12].copy_from_slice(&be32(len));
    out
}

fn write(dir: &std::path::Path, name: &str, data: &[u8]) {
    std::fs::write(dir.join(name), data).expect("write hostile sample");
}

fn generate() -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let base = base_font();
    let dejavu = std::fs::read(samples_dir().join("DejaVuSans.ttf")).expect("dejavu");

    // truncated files at many lengths
    for &len in &[0usize, 1, 3, 4, 11, 12, 13, 27, 28, 100, 1000, 5000] {
        if len <= dejavu.len() {
            out.push((format!("trunc-{}.ttf", len), dejavu[..len].to_vec()));
        }
    }
    // huge numTables
    let mut f = dejavu.clone();
    f[4..6].copy_from_slice(&be16(0xFFFF));
    out.push(("huge-numtables.ttf".into(), f));
    // duplicate tags
    let mut f = base.clone();
    // copy the head directory entry over the maxp entry
    let n = u16::from_be_bytes([f[4], f[5]]) as usize;
    let head_off = 12
        + 16 * (0..n)
            .find(|&i| &f[12 + 16 * i..12 + 16 * i + 4] == b"head")
            .unwrap();
    let head_tag = f[head_off..head_off + 4].to_vec();
    f[12 + 16..12 + 20].copy_from_slice(&head_tag);
    out.push(("dup-tag.ttf".into(), f));
    // overlapping table offsets
    let mut f = base.clone();
    let n = u16::from_be_bytes([f[4], f[5]]) as usize;
    let second = 12 + 16;
    let first_off = u32::from_be_bytes([f[12 + 8], f[12 + 9], f[12 + 10], f[12 + 11]]);
    f[second + 8..second + 12].copy_from_slice(&be32(first_off));
    let _ = n;
    out.push(("overlap.ttf".into(), f));
    // out-of-range table offset
    let mut f = base.clone();
    f[12 + 8..12 + 12].copy_from_slice(&be32(0xFFFFFFF0));
    out.push(("bad-offset.ttf".into(), f));
    // misaligned table offset
    let mut f = base.clone();
    f[12 + 8..12 + 12].copy_from_slice(&be32(13));
    out.push(("misaligned.ttf".into(), f));
    // loca pointing outside glyf
    let mut f = base.clone();
    let n = u16::from_be_bytes([f[4], f[5]]) as usize;
    let loca_i = (0..n)
        .find(|&i| &f[12 + 16 * i..12 + 16 * i + 4] == b"loca")
        .unwrap();
    let loca_off = u32::from_be_bytes([
        f[12 + 16 * loca_i + 8],
        f[12 + 16 * loca_i + 9],
        f[12 + 16 * loca_i + 10],
        f[12 + 16 * loca_i + 11],
    ]) as usize;
    f[loca_off + 4..loca_off + 8].copy_from_slice(&be32(0x00100000));
    out.push(("loca-outside-glyf.ttf".into(), f));
    // WOFF with totalSfntSize near u32::MAX
    out.push((
        "woff-huge-totalsfnt.woff".into(),
        wrap_woff(&base, Some(0xFFFFFFF0)),
    ));
    // WOFF with table length near u32::MAX
    let mut w = wrap_woff(&base, None);
    w[44 + 12..44 + 16].copy_from_slice(&be32(0xFFFFFFF0)); // first table compLength
    out.push(("woff-huge-tablelen.woff".into(), w));
    // WOFF zlib bomb: one table declared 100 MiB of zeros, compressed small
    let zeros = vec![0u8; 100 * 1024 * 1024];
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    use std::io::Write;
    enc.write_all(&zeros).unwrap();
    let compressed = enc.finish().unwrap();
    let mut w = Vec::new();
    let n = 1u16;
    w.extend_from_slice(b"wOFF");
    w.extend_from_slice(&be32(0x00010000));
    w.extend_from_slice(&be32(0)); // patched
    w.extend_from_slice(&be16(n));
    w.extend_from_slice(&be16(0));
    let total_sfnt = (12 + 16 + (zeros.len() as u32 + 3)) & !3;
    w.extend_from_slice(&be32(total_sfnt));
    w.extend_from_slice(&be16(1));
    w.extend_from_slice(&be16(0));
    w.extend_from_slice(&be32(0));
    w.extend_from_slice(&be32(0));
    w.extend_from_slice(&be32(0));
    w.extend_from_slice(&be32(0));
    w.extend_from_slice(&be32(0));
    w.extend_from_slice(&be32(tag(b"glyf")));
    w.extend_from_slice(&be32(44 + 20));
    w.extend_from_slice(&be32(compressed.len() as u32));
    w.extend_from_slice(&be32(zeros.len() as u32));
    w.extend_from_slice(&be32(0));
    w.extend_from_slice(&compressed);
    let pad = (4 - (w.len() & 3)) % 4;
    w.extend(std::iter::repeat_n(0u8, pad));
    let l = w.len() as u32;
    w[8..12].copy_from_slice(&be32(l));
    out.push(("woff-zlib-bomb.woff".into(), w));
    // cmap format 12 with huge group count
    let mut cmap = Vec::new();
    cmap.extend_from_slice(&be16(0));
    cmap.extend_from_slice(&be16(1));
    cmap.extend_from_slice(&be16(3));
    cmap.extend_from_slice(&be16(10));
    cmap.extend_from_slice(&be32(12));
    cmap.extend_from_slice(&be16(12));
    cmap.extend_from_slice(&be16(0));
    cmap.extend_from_slice(&be32(16));
    cmap.extend_from_slice(&be32(0));
    cmap.extend_from_slice(&be32(0xFFFFFFF0)); // numGroups huge
    let mut f = base.clone();
    let n = u16::from_be_bytes([f[4], f[5]]) as usize;
    let cmap_i = (0..n)
        .find(|&i| &f[12 + 16 * i..12 + 16 * i + 4] == b"cmap")
        .unwrap();
    let cmap_off = u32::from_be_bytes([
        f[12 + 16 * cmap_i + 8],
        f[12 + 16 * cmap_i + 9],
        f[12 + 16 * cmap_i + 10],
        f[12 + 16 * cmap_i + 11],
    ]) as usize;
    let cmap_len = u32::from_be_bytes([
        f[12 + 16 * cmap_i + 12],
        f[12 + 16 * cmap_i + 13],
        f[12 + 16 * cmap_i + 14],
        f[12 + 16 * cmap_i + 15],
    ]) as usize;
    f.splice(cmap_off..cmap_off + cmap_len, cmap.iter().cloned());
    out.push(("cmap12-huge-groups.ttf".into(), f));
    // fvar with 65535 axes
    let mut fvar = Vec::new();
    fvar.extend_from_slice(&be16(1));
    fvar.extend_from_slice(&be16(0));
    fvar.extend_from_slice(&be16(16));
    fvar.extend_from_slice(&be16(2));
    fvar.extend_from_slice(&be16(0xFFFF));
    fvar.extend_from_slice(&be16(20));
    fvar.extend_from_slice(&be16(0));
    fvar.extend_from_slice(&be16((4u32 * 0xFFFF + 4) as u16));
    out.push(("fvar-65535-axes.bin".into(), fvar));
    // ttcf with thousands of faces
    let mut ttc = Vec::new();
    ttc.extend_from_slice(b"ttcf");
    ttc.extend_from_slice(&be32(0x00010000));
    ttc.extend_from_slice(&be32(5000));
    ttc.extend(std::iter::repeat_n(0u8, 5000 * 4));
    out.push(("ttcf-5000-faces.ttc".into(), ttc));
    // composite glyph cycle: glyph 1 references itself
    let mut glyf = vec![0u8; 30];
    // glyph 0: empty; glyph 1: composite referencing glyph 1
    glyf[0..2].copy_from_slice(&be16(0)); // nc=0 empty
    let c = 10usize;
    glyf[c..c + 2].copy_from_slice(&be16(0xFFFF)); // composite
    glyf[c + 2..c + 10].copy_from_slice(&[0u8; 8]);
    glyf[c + 10..c + 12].copy_from_slice(&be16(0x0020)); // MORE_COMPONENTS? no: flags
    glyf[c + 10..c + 12].copy_from_slice(&be16(0x0000));
    glyf[c + 12..c + 14].copy_from_slice(&be16(1)); // gid 1 -> self
    glyf[c + 14..c + 16].copy_from_slice(&be16(0));
    glyf[c + 16..c + 18].copy_from_slice(&be16(0));
    let loca = {
        let mut v = Vec::new();
        v.extend_from_slice(&be32(0));
        v.extend_from_slice(&be32(10));
        v.extend_from_slice(&be32(30));
        v
    };
    let f = build_sfnt(
        0x00010000,
        &[
            (tag(b"OS/2"), vec![0u8; 78]),
            (tag(b"cmap"), {
                let mut v = Vec::new();
                v.extend_from_slice(&be16(0));
                v.extend_from_slice(&be16(0));
                v
            }),
            (tag(b"glyf"), glyf),
            (tag(b"head"), {
                let mut v = Vec::new();
                v.extend_from_slice(&be32(0x00010000));
                v.extend_from_slice(&be32(0x00010000));
                v.extend_from_slice(&be32(0));
                v.extend_from_slice(&be32(0x5F0F3CF5));
                v.extend_from_slice(&be16(0));
                v.extend_from_slice(&be16(1000));
                v.extend_from_slice(&[0u8; 16]);
                v.extend_from_slice(&be16(0));
                v.extend_from_slice(&be16(0));
                v.extend_from_slice(&be16(100));
                v.extend_from_slice(&be16(100));
                v.extend_from_slice(&be16(0));
                v.extend_from_slice(&be16(0));
                v.extend_from_slice(&be16(2));
                v.extend_from_slice(&be16(1));
                v.extend_from_slice(&be16(0));
                v
            }),
            (tag(b"hhea"), {
                let mut v = Vec::new();
                v.extend_from_slice(&be32(0x00010000));
                v.extend(std::iter::repeat_n(0u8, 32));
                v.extend_from_slice(&be16(1));
                v
            }),
            (tag(b"hmtx"), {
                let mut v = Vec::new();
                v.extend_from_slice(&be16(500));
                v.extend_from_slice(&be16(0));
                v.extend_from_slice(&be16(0));
                v
            }),
            (tag(b"loca"), loca),
            (tag(b"maxp"), {
                let mut v = Vec::new();
                v.extend_from_slice(&be32(0x00010000));
                v.extend_from_slice(&be16(2));
                v.extend(std::iter::repeat_n(0u8, 26));
                v
            }),
            (tag(b"name"), {
                let mut v = Vec::new();
                v.extend_from_slice(&be16(0));
                v.extend_from_slice(&be16(0));
                v.extend_from_slice(&be16(6));
                v
            }),
            (tag(b"post"), {
                let mut v = Vec::new();
                v.extend_from_slice(&be32(0x00030000));
                v.extend(std::iter::repeat_n(0u8, 28));
                v
            }),
        ],
    );
    out.push(("composite-cycle.ttf".into(), f));
    out
}

#[test]
fn hostile_corpus_no_panic() {
    if !samples_available("hostile_corpus_no_panic") {
        return;
    }
    let dir = samples_dir().join("hostile");
    std::fs::create_dir_all(&dir).expect("create hostile dir");
    let files = generate();
    for (name, data) in &files {
        write(&dir, name, data);
        let r = lucina_fontsan::process(data);
        eprintln!(
            "hostile {}: {:?}",
            name,
            r.as_ref().map(|v| v.len()).map_err(|e| format!("{:?}", e))
        );
    }
    // also run the oracle on the hostile corpus
    for (name, data) in &files {
        let ours = lucina_fontsan::process(data);
        let ots = fontsan::process(data);
        if ours.is_ok() != ots.is_ok() {
            eprintln!(
                "DISAGREEMENT {}: ours={:?} ots={:?}",
                name,
                ours.as_ref().map(|v| v.len()),
                ots.as_ref()
                    .map(|v| v.len())
                    .map_err(|e| format!("{:?}", e))
            );
        }
    }
}
