//! gltrace replay <trace.bin> --out <dir> [--frames A..B] [--per-draw] [--repeat N] [--backend cxx|rust] [--threads N] [--no-draws]
//! gltrace info <trace.bin>
//! gltrace perf-table <cxx.log> <rust.log> [--page NAME]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use gltrace::{frame_rgba, replay, write_png, FrameStat, Options};

const USAGE: &str = "usage: gltrace replay <trace.bin> --out <dir> [--frames A..B] [--per-draw] [--repeat N] [--backend cxx|rust] [--threads N] [--no-draws]
       gltrace info <trace.bin>
       gltrace perf-table <cxx.log> <rust.log> [--page NAME]

Frames are numbered from 0 by the FrameMarker that ends them; A..B is half open, A.. runs to the end.
--no-draws skips DrawElementsInstanced (to compare the other calls between backends without shaders).
--backend picks the rasterizer (cxx is swgl's C++, rust is lucinanet-raster). Without it gltrace follows the library
default, which is rust; LUCINANET_RASTER=swgl|rust does the same as --backend.
--threads N splits the rust backend's large draws across N threads by 32-row stripes (1: one thread; default the
cores, at most 8); LUCINANET_RASTER_THREADS does the same.";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gltrace: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some("info") => {
            let path = args.next().ok_or(USAGE)?;
            gltrace::info::info(path.as_ref())
        }
        Some("replay") => replay_cmd(args),
        Some("perf-table") => gltrace::perf_table::run(&args.collect::<Vec<_>>()),
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(USAGE.into()),
    }
}

fn replay_cmd(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut trace = None;
    let mut opts = Options { out: None, frames: 0..u64::MAX, per_draw: false, no_draws: false, verbose: true };
    let mut repeat = 1u32;
    let mut backend = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => opts.out = Some(PathBuf::from(args.next().ok_or("--out needs a directory")?)),
            "--frames" => opts.frames = parse_range(&args.next().ok_or("--frames needs A..B")?)?,
            "--per-draw" => opts.per_draw = true,
            "--no-draws" => opts.no_draws = true,
            "--backend" => {
                let b = match args.next().as_deref() {
                    Some("cxx" | "swgl") => swgl::Backend::Cxx,
                    Some("rust") => swgl::Backend::Rust,
                    other => return Err(format!("--backend takes cxx or rust, not {other:?}")),
                };
                backend = Some(b);
            }
            "--threads" => {
                let n = args.next().ok_or("--threads needs a count")?;
                let t: usize = n.parse().ok().filter(|&t| t > 0).ok_or(format!("bad --threads {n}"))?;
                // Read once, by the first context this replay makes (`lucinanet_raster::pool::default_threads`).
                std::env::set_var("LUCINANET_RASTER_THREADS", t.to_string());
            }
            "--repeat" => {
                let n = args.next().ok_or("--repeat needs a count")?;
                repeat = n.parse().ok().filter(|&n| n > 0).ok_or(format!("bad --repeat {n}"))?;
            }
            _ if trace.is_none() && !a.starts_with("--") => trace = Some(PathBuf::from(a)),
            _ => return Err(format!("unknown argument {a}\n{USAGE}")),
        }
    }
    let trace = trace.ok_or(USAGE)?;
    // Without --backend, LUCINANET_RASTER decides at the first context (default rust, as in the library).
    if let Some(b) = backend {
        swgl::select_backend(b)?;
    }
    if let Some(out) = &opts.out {
        std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    }

    // The first pass writes the PNGs and prints; with --repeat, the others only time.
    let out = opts.out.clone();
    let mut write_frame = |end: &gltrace::FrameEnd| -> Result<(), String> {
        let Some(out) = &out else { return Ok(()) };
        let Some((w, h, rgba)) = frame_rgba(end) else {
            eprintln!("gltrace: frame {}: no default framebuffer", end.frame);
            return Ok(());
        };
        write_png(&out.join(format!("frame-{}.png", end.frame)), w, h, &rgba)
    };
    let mut passes: Vec<Vec<FrameStat>> = vec![replay(&trace, &opts, &mut write_frame)?];
    if repeat > 1 {
        let quiet = Options { out: None, verbose: false, per_draw: false, ..opts };
        for _ in 1..repeat {
            passes.push(replay(&trace, &quiet, &mut |_| Ok(()))?);
        }
        println!("over {repeat} passes, ms per frame:");
        for (i, f) in passes[0].iter().enumerate() {
            let mut times: Vec<Duration> = passes.iter().filter_map(|p| p.get(i)).map(|s| s.time).collect();
            times.sort();
            let ms = |d: Duration| d.as_secs_f64() * 1e3;
            println!("frame {} min {:.3} median {:.3}", f.frame, ms(times[0]), ms(times[times.len() / 2]));
        }
    }
    Ok(())
}

fn parse_range(s: &str) -> Result<std::ops::Range<u64>, String> {
    let bad = || format!("bad frame range {s}: want A..B or A..");
    let (a, b) = s.split_once("..").ok_or_else(bad)?;
    let a = if a.is_empty() { 0 } else { a.parse().map_err(|_| bad())? };
    let b = if b.is_empty() { u64::MAX } else { b.parse().map_err(|_| bad())? };
    if a > b {
        return Err(bad());
    }
    Ok(a..b)
}
