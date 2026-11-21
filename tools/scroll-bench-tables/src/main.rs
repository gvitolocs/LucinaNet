//! Regenerate the six scroll-bench findings tables from the run directories and the job logs the runs
//! went through, byte for byte as the Python analysis printed them.
//!
//!     scroll-bench-tables --base $WORK_DIR/lucinanet/scroll-bench m1 m2 m3 m4 \
//!         logs/run-matrix.log logs/run-matrix2.log logs/run-matrix3.log logs/run-matrix4.log
//!
//! Every argument that ends in `.log` is a job log and every other one a matrix directory; a relative
//! one is read under `--base`, which stands in for the path the Python had written into it. Each run's
//! load and wall time come from the block of a log whose clock window holds the time its json was
//! written, so a run with no log beside it still prints, with `-` in those two columns.

mod fmt;
mod joblog;
mod run;
mod stats;
mod tables;

use std::path::{Path, PathBuf};

use run::Run;

const USAGE: &str = "\
scroll-bench-tables - the scroll-bench findings tables, from the runs that made them

USAGE:
    scroll-bench-tables [OPTIONS] [INPUT...]

INPUTS:
    Any number of matrix directories and job logs, in any order. An argument ending in `.log`
    is a job log; any other is a directory of runs, either the matrices' own layout of one
    directory per run with a `json/` inside it, or a single directory holding a `json/` of its
    own - which is what a run on the Mac leaves behind.

OPTIONS:
    --base DIR    Read a relative input from here instead of the current directory.
    -h, --help    Print this text.

The tables go to stdout: the count of runs read, then sections 1 to 6 of the findings. A run
whose json was written outside the clock window of any job log gets `-` in its load and wall
columns, and the list of them is the only thing this writes to stderr apart from an error.
";

fn main() {
    if let Err(error) = run(std::env::args().skip(1)) {
        eprintln!("scroll-bench-tables: {error}");
        std::process::exit(1);
    }
}

fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut base = PathBuf::from(".");
    let mut inputs: Vec<String> = Vec::new();
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--base" => {
                base = PathBuf::from(args.next().ok_or_else(|| "--base wants a directory".to_string())?);
            }
            _ if arg.starts_with("--base=") => {
                base = PathBuf::from(arg.trim_start_matches("--base=").to_string());
            }
            _ => inputs.push(arg),
        }
    }

    let mut blocks = Vec::new();
    for log in inputs.iter().filter(|a| a.ends_with(".log")) {
        let path = resolve(&base, log);
        if !path.is_file() {
            return Err(format!("no job log at {}", path.display()));
        }
        blocks.extend(joblog::load_log(&path));
    }

    let mut runs: Vec<Run> = Vec::new();
    for dir in inputs.iter().filter(|a| !a.ends_with(".log")) {
        let path = resolve(&base, dir);
        if !path.is_dir() {
            return Err(format!("no run directory at {}", path.display()));
        }
        for (json, name, src) in listing(&path)? {
            runs.push(Run::read(&json, &name, &src).map_err(|e| format!("{dir}: {e}"))?);
        }
    }
    if runs.is_empty() {
        println!("no json under {}", inputs.join(" "));
        return Ok(());
    }

    let found = tables::report(&mut runs, &blocks);
    if !found.unmatched.is_empty() {
        // Once, and on stderr: the tables themselves are the output, and a reader comparing this to
        // the Python's own file must not have a differing line count to explain.
        let mut names = found.unmatched.iter().take(4).cloned().collect::<Vec<String>>().join(", ");
        let rest = found.unmatched.len().saturating_sub(4);
        if rest > 0 {
            names.push_str(&format!(", and {rest} more"));
        }
        eprintln!(
            "scroll-bench-tables: {} of {} runs have no load line, so their load and wall columns read -: \
             {names}",
            found.unmatched.len(),
            runs.len()
        );
    }
    Ok(())
}

fn resolve(base: &Path, arg: &str) -> PathBuf {
    let path = Path::new(arg);
    if path.is_absolute() { path.to_path_buf() } else { base.join(path) }
}

/// The runs of one directory, as the paths they were found under: sorted by the whole path, which is
/// the order the analysis' glob gave them and the order that decides which run of a cell is the first
/// one section 4 quotes.
///
/// Both layouts are read, and the name a run is known by comes from the directory it sits in rather
/// than the file: the m1 directories were renamed after the runs to carry the rasterizer and the pace,
/// and their json files were left with the old names.
fn listing(dir: &Path) -> Result<Vec<(PathBuf, String, String)>, String> {
    let src = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
    let mut found: Vec<(String, PathBuf, String, String)> = Vec::new();
    let add = |json: PathBuf, name: String, found: &mut Vec<(String, PathBuf, String, String)>| {
        found.push((json.to_string_lossy().into_owned(), json, name, src.clone()));
    };
    for entry in read_dir(dir)? {
        let json = entry.join("json");
        if json.is_dir() {
            let name = entry.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
            for file in read_dir(&json)? {
                if file.extension().is_some_and(|e| e == "json") {
                    add(file, name.clone(), &mut found);
                }
            }
        }
    }
    // A directory that is itself one run: the name is the json file's own, minus the suffix.
    let flat = dir.join("json");
    if flat.is_dir() {
        for file in read_dir(&flat)? {
            if file.extension().is_some_and(|e| e == "json") {
                let name = file.file_stem().and_then(|n| n.to_str()).unwrap_or_default().to_string();
                add(file, name, &mut found);
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found.into_iter().map(|(_, json, name, src)| (json, name, src)).collect())
}

fn read_dir(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        out.push(entry.path());
    }
    Ok(out)
}
