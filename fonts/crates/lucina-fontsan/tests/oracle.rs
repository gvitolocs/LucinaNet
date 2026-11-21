// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Behaviour derived from OTS (BSD-3-Clause, see LICENSES/BSD-3-Clause-OTS.txt).

//! Oracle test: lucina-fontsan must agree with fontsan 0.7 (C++ OTS) on every
//! sample: both accept or both reject; when both accept, the outputs must
//! have the same table tag set, numGlyphs, BMP cmap mappings, hmtx advances
//! and (via skrifa) the same outline points for the first 200 glyphs.

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

fn sample_files() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(samples_dir())
        .expect("samples dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && !p
                    .extension()
                    .map(|e| e == "md" || e == "css")
                    .unwrap_or(false)
                && p.file_name().map(|n| n != "README").unwrap_or(true)
        })
        .collect();
    let hostile = samples_dir().join("hostile");
    if hostile.is_dir() {
        if let Ok(rd) = std::fs::read_dir(&hostile) {
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.is_file() {
                    v.push(p);
                }
            }
        }
    }
    v.sort();
    v
}

fn table_tags(data: &[u8]) -> Vec<u32> {
    use read_fonts::FontRef;
    let Ok(font) = FontRef::new(data) else {
        return Vec::new();
    };
    let mut tags: Vec<u32> = font
        .table_directory()
        .table_records()
        .iter()
        .map(|e| u32::from_be_bytes(e.tag().into_bytes()))
        .collect();
    tags.sort_unstable();
    tags
}

fn num_glyphs(data: &[u8]) -> Option<u16> {
    use read_fonts::{FontRef, TableProvider};
    let font = FontRef::new(data).ok()?;
    font.maxp().ok().map(|m| m.num_glyphs())
}

fn bmp_cmap(data: &[u8]) -> Vec<(u32, u16)> {
    use read_fonts::{FontRef, TableProvider};
    let Ok(font) = FontRef::new(data) else {
        return Vec::new();
    };
    let Ok(cmap) = font.cmap() else {
        return Vec::new();
    };
    let Some((_, _, sub)) = cmap.best_subtable() else {
        return Vec::new();
    };
    let mut v = Vec::new();
    for cp in 0u32..=0xFFFF {
        if let Some(g) = sub.map_codepoint(cp) {
            v.push((cp, g.to_u32() as u16));
        }
    }
    v
}

fn hmtx_advances(data: &[u8]) -> Vec<u16> {
    use read_fonts::{FontRef, TableProvider};
    let Ok(font) = FontRef::new(data) else {
        return Vec::new();
    };
    let Ok(hmtx) = font.hmtx() else {
        return Vec::new();
    };
    let n = num_glyphs(data).unwrap_or(0) as usize;
    (0..n)
        .map(|i| {
            hmtx.advance(read_fonts::types::GlyphId::new(i as u32))
                .unwrap_or(0)
        })
        .collect()
}

type OutlinePoints = (u16, Vec<(f32, f32, bool)>);
fn outline_points(data: &[u8], count: usize) -> Vec<OutlinePoints> {
    use read_fonts::TableProvider;
    use skrifa::outline::DrawSettings;
    use skrifa::{prelude::Size, FontRef, MetadataProvider};
    let Ok(font) = FontRef::new(data) else {
        return Vec::new();
    };
    let n = font
        .maxp()
        .map(|m| m.num_glyphs() as usize)
        .unwrap_or(0)
        .min(count);
    let outlines = font.outline_glyphs();
    let mut out = Vec::new();
    for gid in 0..n {
        let mut pts = Vec::new();
        if let Some(g) = outlines.get(read_fonts::types::GlyphId::new(gid as u32)) {
            let settings =
                DrawSettings::unhinted(Size::unscaled(), skrifa::instance::LocationRef::default());
            let _ = g.draw(settings, &mut PtSink(&mut pts));
        }
        out.push((gid as u16, pts));
    }
    out
}

struct PtSink<'a>(&'a mut Vec<(f32, f32, bool)>);
impl skrifa::outline::OutlinePen for PtSink<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push((x, y, false));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push((x, y, true));
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0.push((cx, cy, false));
        self.0.push((x, y, true));
    }
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.0.push((c1x, c1y, false));
        self.0.push((c2x, c2y, false));
        self.0.push((x, y, true));
    }
    fn close(&mut self) {}
}

#[test]
fn oracle_agreement() {
    if !samples_available("oracle_agreement") {
        return;
    }
    let files = sample_files();
    assert!(!files.is_empty(), "no sample files found");
    let mut disagreements: Vec<String> = Vec::new();
    let mut accepted = 0;
    for f in &files {
        let data = std::fs::read(f).expect("read sample");
        let ours = lucina_fontsan::process(&data);
        let theirs = fontsan::process(&data);
        match (&ours, &theirs) {
            (Ok(a), Ok(b)) => {
                accepted += 1;
                let name = f.file_name().unwrap().to_string_lossy().to_string();
                if table_tags(a) != table_tags(b) {
                    disagreements.push(format!(
                        "{}: table tag sets differ: ours={:?} ots={:?}",
                        name,
                        table_tags(a),
                        table_tags(b)
                    ));
                }
                if num_glyphs(a) != num_glyphs(b) {
                    disagreements.push(format!(
                        "{}: numGlyphs differ: ours={:?} ots={:?}",
                        name,
                        num_glyphs(a),
                        num_glyphs(b)
                    ));
                }
                if bmp_cmap(a) != bmp_cmap(b) {
                    disagreements.push(format!("{}: BMP cmap mappings differ", name));
                }
                if hmtx_advances(a) != hmtx_advances(b) {
                    disagreements.push(format!("{}: hmtx advances differ", name));
                }
                if outline_points(a, 200) != outline_points(b, 200) {
                    disagreements.push(format!(
                        "{}: outline points differ (first 200 glyphs)",
                        name
                    ));
                }
            }
            (Err(_), Err(_)) => {}
            (Ok(_), Err(e)) => disagreements.push(format!(
                "{}: we accept, OTS rejects ({:?})",
                f.file_name().unwrap().to_string_lossy(),
                e
            )),
            (Err(e), Ok(_)) => disagreements.push(format!(
                "{}: OTS accepts, we reject ({:?})",
                f.file_name().unwrap().to_string_lossy(),
                e
            )),
        }
    }
    eprintln!(
        "samples: {} files, {} accepted by both",
        files.len(),
        accepted
    );
    for d in &disagreements {
        eprintln!("DISAGREEMENT: {}", d);
    }
    assert!(
        disagreements.is_empty(),
        "{} disagreements with OTS",
        disagreements.len()
    );
}
