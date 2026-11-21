//! `sweep-score score <out-dir> [--list sites.tsv] [--walls walls.tsv]`
//! `sweep-score diff [<old-results.tsv>] <new-results.tsv> [--history DIR]`
//!
//! `score` reads `<out-dir>/<id>/` and writes `<out-dir>/results.tsv` (rules: see the library docs).
//! `diff` prints what changed between two results files; with one file it compares against
//! `<history>/latest/results.tsv` (history: `--history`, `$SWEEP_HISTORY`, or `$WORK_DIR/lucinanet/sweep-results`,
//! `WORK_DIR` being a scratch directory and the temp dir its default).
//! Exit status of `diff`: 1 when any page went loaded -> broken, 0 otherwise; 2 is an error.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::exit;

use sweep_score::{diff, format_results, parse_results, read_list, score_run, summary, Walls};

const DEFAULT_WALLS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/sweep/walls.tsv");
const DEFAULT_LIST: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/sweep/sites.tsv");

fn usage() -> ! {
    eprintln!("usage: sweep-score score <out-dir> [--list sites.tsv] [--walls walls.tsv]");
    eprintln!("       sweep-score diff [<old-results.tsv>] <new-results.tsv> [--history DIR]");
    exit(2)
}

fn fail(msg: &str) -> ! {
    eprintln!("sweep-score: {msg}");
    exit(2)
}

fn option(args: &mut Vec<String>, name: &str) -> Option<String> {
    let at = args.iter().position(|a| a == name)?;
    if at + 1 >= args.len() {
        usage();
    }
    let v = args.remove(at + 1);
    args.remove(at);
    Some(v)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }
    let cmd = args.remove(0);
    match cmd.as_str() {
        "score" => {
            let list = option(&mut args, "--list").unwrap_or_else(|| DEFAULT_LIST.to_string());
            let walls_path = option(&mut args, "--walls").unwrap_or_else(|| DEFAULT_WALLS.to_string());
            if args.len() != 1 {
                usage();
            }
            let out = PathBuf::from(&args[0]);
            let walls_src = fs::read_to_string(&walls_path).unwrap_or_else(|e| fail(&format!("{walls_path}: {e}")));
            let walls = Walls::parse(&walls_src).unwrap_or_else(|e| fail(&e));
            let categories = fs::read_to_string(&list).map(|s| read_list(&s)).unwrap_or_default();
            let rows = score_run(&out, &walls, &categories).unwrap_or_else(|e| fail(&e));
            let target = out.join("results.tsv");
            fs::write(&target, format_results(&rows)).unwrap_or_else(|e| fail(&format!("{}: {e}", target.display())));
            print!("{}", summary(&rows));
            println!("wrote {}", target.display());
        },
        "diff" => {
            let history = option(&mut args, "--history")
                .or_else(|| std::env::var("SWEEP_HISTORY").ok())
                .unwrap_or_else(|| format!("{}/lucinanet/sweep-results", std::env::var("WORK_DIR").unwrap_or_else(|_| std::env::temp_dir().display().to_string())));
            let (old_path, new_path) = match args.len() {
                1 => (Path::new(&history).join("latest").join("results.tsv"), PathBuf::from(&args[0])),
                2 => (PathBuf::from(&args[0]), PathBuf::from(&args[1])),
                _ => usage(),
            };
            let read = |p: &Path| {
                let src = fs::read_to_string(p).unwrap_or_else(|e| fail(&format!("{}: {e}", p.display())));
                parse_results(&src).unwrap_or_else(|e| fail(&format!("{}: {e}", p.display())))
            };
            let report = diff(&read(&old_path), &read(&new_path));
            print!("{}", report.text);
            exit(if report.regression { 1 } else { 0 });
        },
        _ => usage(),
    }
}
