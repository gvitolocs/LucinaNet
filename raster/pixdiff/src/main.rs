use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use image::ImageReader;

struct Args {
    dir_a: PathBuf,
    dir_b: PathBuf,
    heat: Option<PathBuf>,
    tolerance: u8,
}

fn parse_args() -> Result<Args, String> {
    let mut positional: Vec<PathBuf> = Vec::new();
    let mut heat: Option<PathBuf> = None;
    let mut tolerance: u8 = 0;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--heat" => {
                let out = args.next().ok_or("--heat requires an output directory")?;
                heat = Some(PathBuf::from(out));
            }
            "--tolerance" => {
                let val = args.next().ok_or("--tolerance requires a value")?;
                tolerance = val
                    .parse::<u8>()
                    .map_err(|_| format!("invalid tolerance value: {val}"))?;
            }
            _ if arg.starts_with("--") => {
                return Err(format!("unknown option: {arg}"));
            }
            _ => positional.push(PathBuf::from(arg)),
        }
    }
    if positional.len() != 2 {
        return Err("usage: pixdiff <dir_a> <dir_b> [--heat <out_dir>] [--tolerance N]".to_string());
    }
    Ok(Args {
        dir_a: positional.remove(0),
        dir_b: positional.remove(0),
        heat,
        tolerance,
    })
}

fn frame_number(name: &str) -> Option<u64> {
    name.strip_prefix("frame-")?
        .strip_suffix(".png")?
        .parse::<u64>()
        .ok()
}

fn list_frames(dir: &Path) -> Result<Vec<(u64, String)>, String> {
    let mut frames: Vec<(u64, String)> = Vec::new();
    let entries = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(n) = frame_number(&name) {
            frames.push((n, name));
        }
    }
    frames.sort_by_key(|(n, _)| *n);
    Ok(frames)
}

fn load_rgba(path: &Path) -> Result<image::RgbaImage, String> {
    let reader = ImageReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let img = reader
        .decode()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(img.to_rgba8())
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("pixdiff: {e}");
            return ExitCode::from(2);
        }
    };

    let frames_a = match list_frames(&args.dir_a) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("pixdiff: {e}");
            return ExitCode::from(2);
        }
    };
    let frames_b = match list_frames(&args.dir_b) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("pixdiff: {e}");
            return ExitCode::from(2);
        }
    };

    if let Some(heat_dir) = &args.heat {
        if let Err(e) = fs::create_dir_all(heat_dir) {
            eprintln!("pixdiff: cannot create {}: {e}", heat_dir.display());
            return ExitCode::from(2);
        }
    }

    let names_a: Vec<&str> = frames_a.iter().map(|(_, n)| n.as_str()).collect();
    let names_b: Vec<&str> = frames_b.iter().map(|(_, n)| n.as_str()).collect();

    let mut ok = true;
    for name in &names_a {
        if !names_b.contains(name) {
            println!("only in a: {name}");
            ok = false;
        }
    }
    for name in &names_b {
        if !names_a.contains(name) {
            println!("only in b: {name}");
            ok = false;
        }
    }

    let mut total = 0usize;
    let mut identical = 0usize;
    let mut within = 0usize;
    let mut worse = 0usize;

    for (_, name) in &frames_a {
        if !names_b.contains(&name.as_str()) {
            continue;
        }
        total += 1;
        let path_a = args.dir_a.join(name);
        let path_b = args.dir_b.join(name);
        let img_a = match load_rgba(&path_a) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("pixdiff: {e}");
                ok = false;
                continue;
            }
        };
        let img_b = match load_rgba(&path_b) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("pixdiff: {e}");
                ok = false;
                continue;
            }
        };
        let (wa, ha) = img_a.dimensions();
        let (wb, hb) = img_b.dimensions();
        if wa != wb || ha != hb {
            println!("size differs: {name} {wa}x{ha} vs {wb}x{hb}");
            ok = false;
            worse += 1;
            continue;
        }

        let pa = img_a.as_raw();
        let pb = img_b.as_raw();
        let total_px = (wa * ha) as usize;
        let mut d0 = 0usize;
        let mut d1 = 0usize;
        let mut d2 = 0usize;
        let mut d8 = 0usize;
        let mut d32 = 0usize;
        let mut max_d: u8 = 0;
        let mut first: Option<(u32, u32)> = None;
        for i in 0..total_px {
            let a = &pa[i * 4..i * 4 + 4];
            let b = &pb[i * 4..i * 4 + 4];
            let d = a
                .iter()
                .zip(b.iter())
                .map(|(x, y)| x.abs_diff(*y))
                .max()
                .unwrap_or(0);
            if d > 0 {
                d0 += 1;
                if first.is_none() {
                    first = Some(((i as u32) % wa, (i as u32) / wa));
                }
                if d > 1 {
                    d1 += 1;
                }
                if d > 2 {
                    d2 += 1;
                }
                if d > 8 {
                    d8 += 1;
                }
                if d > 32 {
                    d32 += 1;
                }
                if d > max_d {
                    max_d = d;
                }
            }
        }
        let first_str = match first {
            Some((x, y)) => format!("{x},{y}"),
            None => "-".to_string(),
        };
        println!(
            "{name} px {total_px} d>0 {d0} d>1 {d1} d>2 {d2} d>8 {d8} d>32 {d32} max {max_d} first {first_str}",
        );

        if let Some(heat_dir) = &args.heat {
            if max_d > 0 {
                let mut heat = image::RgbaImage::new(wa, ha);
                for i in 0..total_px {
                    let a = &pa[i * 4..i * 4 + 4];
                    let b = &pb[i * 4..i * 4 + 4];
                    let d = a
                        .iter()
                        .zip(b.iter())
                        .map(|(x, y)| x.abs_diff(*y))
                        .max()
                        .unwrap_or(0);
                    let v = if d == 0 { 0 } else { (d as u16 * 16).min(255) as u8 };
                    heat.put_pixel((i as u32) % wa, (i as u32) / wa, image::Rgba([v, v, v, 255]));
                }
                if let Err(e) = heat.save(heat_dir.join(name)) {
                    eprintln!("pixdiff: cannot write heat map for {name}: {e}");
                    ok = false;
                }
            }
        }

        if max_d == 0 {
            identical += 1;
        } else if max_d <= args.tolerance {
            within += 1;
        } else {
            worse += 1;
            ok = false;
        }
    }

    println!(
        "frames {total} identical {identical} within {} {within} worse {worse}",
        args.tolerance
    );

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
