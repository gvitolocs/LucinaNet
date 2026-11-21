//! Replays a swgl call trace (`LUCINANET_GLTRACE`, format in `swgl/TRACE.md`) on one thread through swgl's own
//! dispatch functions, frame by frame.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use swgl::replay::{Pixels, Replayer, Target};
use swgl::trace::{Op, TraceReader};

pub mod info;
pub mod perf_table;

pub struct Options {
    /// Where `frame-<n>.png` go; `None` writes none.
    pub out: Option<PathBuf>,
    /// Frames to report, by marker number (frame `n` is everything up to the `n`-th `FrameMarker`, from 0).
    pub frames: Range<u64>,
    pub per_draw: bool,
    /// Skip `DrawElementsInstanced`, so that a backend without shaders and one with them replay the same calls.
    pub no_draws: bool,
    /// Print the per-frame lines and per-draw hashes (off for the timing-only passes of `--repeat`).
    pub verbose: bool,
}

pub struct FrameStat {
    pub frame: u64,
    pub draws: u64,
    /// Time spent in replayed calls since the previous marker.
    pub time: Duration,
}

/// What a frame callback gets: the replayer as it stands at the marker.
pub struct FrameEnd<'a> {
    pub frame: u64,
    pub replayer: &'a Replayer,
    /// Whether the default framebuffer is top row first: SwCompositor composites into it top row first, GL
    /// drawing leaves it bottom row first (`SoftContext::top_down` in `lucinanet-servo/src/soft.rs`). Known
    /// once the trace has shown a `Composite`.
    pub top_down: bool,
}

/// Replays `path` from the start until the end of `opts.frames`, calling `on_frame` at every marker in range.
pub fn replay(
    path: &Path,
    opts: &Options,
    on_frame: &mut dyn FnMut(&FrameEnd) -> Result<(), String>,
) -> Result<Vec<FrameStat>, String> {
    let mut reader = TraceReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut replayer = Replayer::new();
    let mut stats = Vec::new();
    let (mut frame, mut draws, mut time) = (0u64, 0u64, Duration::ZERO);
    let mut top_down = false;
    let mut hash = 0u64;
    let dump: Option<u64> = std::env::var("GLTRACE_DUMP").ok().and_then(|v| v.parse().ok());
    let mut index = 0u64;
    let mut errors = 0u64;
    while frame < opts.frames.end {
        let Some(rec) = reader.next_record().map_err(|e| format!("{}: {e}", path.display()))? else {
            if reader.truncated {
                eprintln!("gltrace: the trace ends inside a record; replayed up to it");
            }
            break;
        };
        let i = index;
        index += 1;
        let Some(op) = Op::from_u16(rec.op) else {
            return Err(format!("record {i}: unknown op {}", rec.op));
        };
        if op == Op::FrameMarker {
            if opts.frames.contains(&frame) {
                on_frame(&FrameEnd { frame, replayer: &replayer, top_down })?;
                if opts.verbose {
                    println!("frame {frame} draws {draws} ms {:.3}", time.as_secs_f64() * 1e3);
                }
                stats.push(FrameStat { frame, draws, time });
            }
            frame += 1;
            draws = 0;
            time = Duration::ZERO;
            continue;
        }
        if opts.no_draws && op == Op::DrawElementsInstanced {
            continue;
        }
        let start = Instant::now();
        let res = replayer.step(op, &rec.payload);
        time += start.elapsed();
        if let Err(e) = res {
            errors += 1;
            if errors <= 10 {
                eprintln!("gltrace: record {i} {}: {e}; skipped", op.name());
            }
            continue;
        }
        match op {
            Op::DrawElementsInstanced => draws += 1,
            Op::Composite => top_down = true,
            _ => {}
        }
        if opts.per_draw && opts.frames.contains(&frame) {
            if let Some(t) = replayer.written_target(op, &rec.payload) {
                let h = replayer.target_pixels(t).map_or(0, |p| {
                    // GLTRACE_DUMP=<call>: the target's rows after that call, to <out>/dump-<call>.raw (width, height
                    // as two little-endian u32, then the pixel bytes of each row), to find the first differing pixel.
                    if dump == Some(i) {
                        let row = row_bytes(&p);
                        let mut bytes = Vec::with_capacity(8 + row * p.height);
                        bytes.extend_from_slice(&(p.width as u32).to_le_bytes());
                        bytes.extend_from_slice(&(p.height as u32).to_le_bytes());
                        for y in 0..p.height {
                            bytes.extend_from_slice(&p.data[y * p.stride..y * p.stride + row]);
                        }
                        let path = opts.out.clone().unwrap_or_default().join(format!("dump-{i}.raw"));
                        if let Err(e) = std::fs::write(&path, bytes) {
                            eprintln!("gltrace: {}: {e}", path.display());
                        }
                    }
                    hash_pixels(&p)
                });
                hash = mix(hash ^ h);
                if opts.verbose {
                    let fb = match t {
                        Target::Framebuffer(fb) => fb.to_string(),
                        Target::Resource(r) => format!("res:{r:x}"),
                        Target::Texture { name: 0, bound: true, .. } => "tex:bound".to_string(),
                        Target::Texture { name, .. } => format!("tex:{name}"),
                    };
                    println!("{i} {} fb {fb} hash {hash:016x} own {h:016x}", op.name());
                }
            }
        }
    }
    if errors > 10 {
        eprintln!("gltrace: {errors} records could not be replayed");
    }
    if replayer.warnings() > 0 {
        eprintln!("gltrace: {} results differed from the recording", replayer.warnings());
    }
    Ok(stats)
}

/// The default framebuffer as RGBA rows, top row first.
pub fn frame_rgba(end: &FrameEnd) -> Option<(u32, u32, Vec<u8>)> {
    let p = end.replayer.color_buffer(0)?;
    let mut out = Vec::with_capacity(p.width * p.height * 4);
    for y in 0..p.height {
        let row = if end.top_down { y } else { p.height - 1 - y };
        let line = &p.data[row * p.stride..row * p.stride + p.width * 4];
        for px in line.as_chunks::<4>().0 {
            out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    Some((p.width as u32, p.height as u32, out))
}

pub fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut w = enc.write_header().map_err(|e| format!("{}: {e}", path.display()))?;
    w.write_image_data(rgba).map_err(|e| format!("{}: {e}", path.display()))
}

/// The bytes of each row that hold pixels, without the padding swgl adds to a 4-byte stride: the widest
/// pixel size (16, 4, 2, 1 bytes) whose aligned row is the stride.
fn row_bytes(p: &Pixels) -> usize {
    if let Some(row) = p.row_bytes {
        return row;
    }
    [16, 4, 2, 1]
        .into_iter()
        .map(|bpp| p.width * bpp)
        .find(|&row| (row + 3) & !3 == p.stride)
        .unwrap_or(p.stride)
}

fn mix(mut h: u64) -> u64 {
    h = h.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^ (h >> 29)
}

/// A 64-bit hash of a color buffer's pixel bytes. Every step is a bijection of the running value, so a buffer
/// that differs in one word always hashes differently.
pub fn hash_pixels(p: &Pixels) -> u64 {
    let row = row_bytes(p);
    let mut h = (p.width as u64) << 32 | p.height as u64;
    for y in 0..p.height {
        let line = &p.data[y * p.stride..y * p.stride + row];
        let (words, rest) = line.as_chunks::<8>();
        for w in words {
            h = mix(h ^ u64::from_le_bytes(*w));
        }
        let mut tail = [0u8; 8];
        tail[..rest.len()].copy_from_slice(rest);
        h = mix(h ^ u64::from_le_bytes(tail) ^ (rest.len() as u64) << 56);
    }
    h
}
