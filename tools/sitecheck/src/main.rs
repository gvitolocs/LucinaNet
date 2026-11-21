//! `sitecheck`: turn `lucinanet --render` stderr logs into a per-site table,
//! and compare two PNG screenshots with `sitecheck diff`.
//!
//! All parsing and rendering lives in the `sitecheck` library; this file
//! only handles the command line, reading files, and exit codes.

use std::path::Path;
use std::process::ExitCode;

use sitecheck::diff::{diff_files, format_diff, format_diff_json, DEFAULT_THRESHOLD};
use sitecheck::{parse_line, print_json, print_table, DEFAULT_LOAD_MS, DEFAULT_SCROLL_MS, Site};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("diff") {
        return run_diff(&args);
    }
    let mut json = false;
    let mut scroll_ms = DEFAULT_SCROLL_MS;
    let mut load_ms = DEFAULT_LOAD_MS;
    let mut logs: Vec<String> = Vec::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--scroll-ms" => match parse_threshold(&args, &mut i) {
                Some(v) => scroll_ms = v,
                None => return usage(),
            },
            "--load-ms" => match parse_threshold(&args, &mut i) {
                Some(v) => load_ms = v,
                None => return usage(),
            },
            other => logs.push(other.to_string()),
        }
        i += 1;
    }
    if logs.is_empty() {
        return usage();
    }

    let mut sites = Vec::with_capacity(logs.len());
    for log in &logs {
        let text = match std::fs::read_to_string(log) {
            Ok(text) => text,
            Err(err) => {
                eprintln!("sitecheck: cannot read {}: {}", log, err);
                return ExitCode::from(2);
            }
        };
        let mut site = Site::default();
        for line in text.lines() {
            parse_line(&mut site, line);
        }
        // Neither the harness header nor a navigation line gave a url.
        if site.url.is_empty() {
            site.url = log.clone();
        }
        sites.push(site);
    }

    let output = if json {
        print_json(&sites)
    } else {
        print_table(&sites, load_ms, scroll_ms)
    };
    println!("{}", output);
    // The table carries the verdicts, so a complete parse always exits 0.
    ExitCode::SUCCESS
}

/// Read the value following a threshold flag; `i` advances past it.
fn parse_threshold(args: &[String], i: &mut usize) -> Option<u64> {
    let value = args.get(*i + 1)?.parse::<u64>().ok()?;
    *i += 1;
    Some(value)
}

/// `sitecheck diff <a.png> <b.png> [--threshold N] [--json]`
///
/// Differing pixels are the answer, not an error, so the only non-zero exit is
/// a file that cannot be read or decoded (3).
fn run_diff(args: &[String]) -> ExitCode {
    let mut json = false;
    let mut threshold = DEFAULT_THRESHOLD;
    let mut paths: Vec<String> = Vec::new();

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--threshold" => match parse_pixel_threshold(args, &mut i) {
                Some(value) => threshold = value,
                None => return diff_usage(),
            },
            other => paths.push(other.to_string()),
        }
        i += 1;
    }
    if paths.len() != 2 {
        return diff_usage();
    }

    let report = match diff_files(Path::new(&paths[0]), Path::new(&paths[1]), threshold) {
        Ok(report) => report,
        Err((path, reason)) => {
            eprintln!("sitecheck: {} {}", path, reason);
            return ExitCode::from(3);
        }
    };
    if json {
        println!("{}", format_diff_json(&report));
    } else {
        print!("{}", format_diff(&report));
    }
    // A pair that differs by 100% is still a successful comparison.
    ExitCode::SUCCESS
}

/// Read the value following `--threshold`; `i` advances past it.
fn parse_pixel_threshold(args: &[String], i: &mut usize) -> Option<u32> {
    let value = args.get(*i + 1)?.parse::<u32>().ok()?;
    *i += 1;
    Some(value)
}

fn usage() -> ExitCode {
    eprintln!("usage: sitecheck [--json] [--scroll-ms N] [--load-ms N] <log>...");
    eprintln!("       sitecheck diff <a.png> <b.png> [--threshold N] [--json]");
    ExitCode::from(2)
}

fn diff_usage() -> ExitCode {
    eprintln!("usage: sitecheck diff <a.png> <b.png> [--threshold N] [--json]");
    ExitCode::from(2)
}
