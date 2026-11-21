/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `wasm-inspect <file.wasm> [--json]`: what a module is, and what it asks of
//! an engine.
//!
//! The only thing this binary does with the bytes is read them with
//! `wasmparser`. There is no engine in this build (no wasmi dependency), so it
//! cannot instantiate, link or run the module it inspects: by construction the
//! file's code is never executed.

use wasm_inspect::{inspect, render};

use std::io::Write;
use std::process::ExitCode;
use std::{env, fs, io};

const USAGE: &str = "\
wasm-inspect <file.wasm> [--json]

Read a WebAssembly binary and print its sections with sizes, its imports and
exports, its memory, table and global limits, its start function, its function
count, the producers of its toolchain, and a feature verdict computed by
scanning every function body's operators.

  --json   emit the same report as one JSON document instead of text
  -h, --help   this text

Inspection only: the module is never instantiated or run.";

fn run() -> Result<(), String> {
    let mut path: Option<String> = None;
    let mut as_json = false;
    let mut help = false;
    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--json" => as_json = true,
            "-h" | "--help" => help = true,
            other if other.starts_with('-') => {
                return Err(format!("unknown option `{other}`\n{USAGE}"));
            }
            other => {
                if path.is_some() {
                    return Err(format!("one file at a time, got a second: `{other}`"));
                }
                path = Some(other.to_string());
            }
        }
    }
    if help {
        println!("{USAGE}");
        return Ok(());
    }
    let path = path.ok_or_else(|| format!("no file given\n{USAGE}"))?;

    let bytes = fs::read(&path).map_err(|error| format!("{path}: {error}"))?;
    let report = inspect::Report::new(&bytes, &path).map_err(|error| format!("{path}: {error}"))?;
    let features = inspect::verdict(&report);

    let stdout = io::stdout();
    let mut out = stdout.lock();
    if as_json {
        serde_json::to_writer_pretty(&mut out, &render::to_json(&report, &features))
            .map_err(|error| format!("writing: {error}"))?;
        writeln!(out).map_err(|error| format!("writing: {error}"))?;
    } else {
        write!(out, "{}", render::to_text(&report, &features))
            .map_err(|error| format!("writing: {error}"))?;
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wasm-inspect: {error}");
            ExitCode::FAILURE
        }
    }
}
