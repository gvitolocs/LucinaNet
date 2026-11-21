// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Behaviour derived from OTS (BSD-3-Clause, see LICENSES/BSD-3-Clause-OTS.txt).

//! Deterministic mutation fuzzer (#[ignore]; run with
//! `cargo test --release -- --ignored`). For every mutated input:
//! `process` must not panic, must stay within the allocation budget
//! (peak < 64 x input size + 64 MiB, tracked with a counting global
//! allocator), and any Ok output must re-process to identical bytes
//! (idempotent) and open cleanly in skrifa + harfrust.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let cur = ALLOCATED.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            PEAK.fetch_max(cur, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        ALLOCATED.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
}
#[global_allocator]
static A: Counting = Counting;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

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

fn collect_inputs() -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    let dir = samples_dir();
    let mut stack = vec![dir.clone()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.is_file()
                    && p.extension()
                        .map(|e| e != "md" && e != "css" && e != "zip")
                        .unwrap_or(true)
                {
                    if let Ok(data) = std::fs::read(&p) {
                        if data.len() <= 4 * 1024 * 1024 {
                            v.push(data);
                        }
                    }
                }
            }
        }
    }
    v
}

fn mutate(rng: &mut Rng, data: &[u8]) -> Vec<u8> {
    let mut d = data.to_vec();
    let nops = 1 + rng.below(8);
    for _ in 0..nops {
        if d.is_empty() {
            d.push(rng.next() as u8);
            continue;
        }
        match rng.below(4) {
            0 => {
                let i = rng.below(d.len());
                d[i] ^= 1 << (rng.below(8));
            }
            1 => {
                let i = rng.below(d.len());
                d[i] = rng.next() as u8;
            }
            2 => {
                let len = rng.below(d.len() + 1);
                d.truncate(len);
            }
            _ => {
                let i = rng.below(d.len());
                let j = rng.below(d.len());
                d.swap(i, j);
            }
        }
    }
    d
}

struct NoopPen;
impl skrifa::outline::OutlinePen for NoopPen {
    fn move_to(&mut self, _x: f32, _y: f32) {}
    fn line_to(&mut self, _x: f32, _y: f32) {}
    fn quad_to(&mut self, _cx: f32, _cy: f32, _x: f32, _y: f32) {}
    fn curve_to(&mut self, _c1x: f32, _c1y: f32, _c2x: f32, _c2y: f32, _x: f32, _y: f32) {}
    fn close(&mut self) {}
}

fn smoke_ok(data: &[u8]) -> bool {
    use read_fonts::{FontRef, TableProvider};
    use skrifa::MetadataProvider;
    let Ok(font) = FontRef::new(data) else {
        return false;
    };
    let _ = font.maxp();
    let _ = font.cmap();
    let _ = font.hmtx();
    let outlines = font.outline_glyphs();
    let n = font
        .maxp()
        .map(|m| m.num_glyphs() as usize)
        .unwrap_or(0)
        .min(64);
    for gid in 0..n {
        if let Some(g) = outlines.get(read_fonts::types::GlyphId::new(gid as u32)) {
            let settings = skrifa::outline::DrawSettings::unhinted(
                skrifa::prelude::Size::unscaled(),
                skrifa::instance::LocationRef::default(),
            );
            let mut pen = NoopPen;
            let _ = g.draw(settings, &mut pen);
        }
    }
    if let Some(font) = harfrust::Font::new(data.to_vec(), 0) {
        let shaper = harfrust::ShaperFont::new(&font);
        let mut buf = harfrust::Buffer::new();
        buf.push_str("Hello, عالم 世界");
        let _ = harfrust::shape(&shaper, &mut buf, harfrust::ShapeOptions::default());
    }
    true
}

#[test]
#[ignore]
fn mutation_fuzz() {
    if !samples_available("mutation_fuzz") {
        return;
    }
    let iters: usize = std::env::var("LUCINA_FONTSAN_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200_000);
    let inputs = collect_inputs();
    assert!(!inputs.is_empty(), "no fuzz inputs");
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let fail_dir = std::env::temp_dir().join("lucina-fontsan-fuzz-fail");
    let mut ok_count = 0usize;
    for i in 0..iters {
        let base = &inputs[rng.below(inputs.len())];
        let input = mutate(&mut rng, base);
        let budget = 64 * input.len() + 64 * 1024 * 1024;
        PEAK.store(0, Ordering::Relaxed);
        let r = lucina_fontsan::process(&input);
        let peak = PEAK.load(Ordering::Relaxed);
        if peak >= budget {
            let dir = fail_dir.as_path();
            std::fs::create_dir_all(dir).ok();
            std::fs::write(dir.join(format!("iter{}-budget-input.bin", i)), &input).ok();
            panic!(
                "iter {}: allocation budget exceeded: peak {} >= {} (input {} bytes)",
                i,
                peak,
                budget,
                input.len()
            );
        }
        if let Ok(out) = r {
            ok_count += 1;
            let again = lucina_fontsan::process(&out).expect("idempotent re-process must succeed");
            if again != out {
                let dir = fail_dir.as_path();
                std::fs::create_dir_all(dir).ok();
                std::fs::write(dir.join(format!("iter{}-input.bin", i)), &input).ok();
                std::fs::write(dir.join(format!("iter{}-out1.bin", i)), &out).ok();
                std::fs::write(dir.join(format!("iter{}-out2.bin", i)), &again).ok();
                panic!("iter {}: output not idempotent (saved to fuzz-fail/)", i);
            }
            assert!(
                smoke_ok(&out),
                "iter {}: skrifa/harfrust failed on output",
                i
            );
        }
        if i % 100_000 == 0 {
            eprintln!("fuzz iter {}/{} ({} accepted)", i, iters, ok_count);
        }
    }
    eprintln!(
        "fuzz done: {} iterations, {} accepted outputs",
        iters, ok_count
    );
}
