//! fncache-check: the correctness oracle for the structural per-function bytecode cache.
//!
//! Subcommands:
//!
//!   self <file.js> [--disk DIR]
//!       Compile the file (lazy on) and generate every function stub, filling the cache; then
//!       compile the same file again with verify mode on and generate all of its functions.
//!       Expect 100% hits and 0 mismatches. With --disk the check runs in a second context that
//!       only has the on-disk store.
//!
//!   pair <A.js> <B.js> [--disk DIR]
//!       Fill the cache from A, then compile B with verify mode on and generate all of B's
//!       functions; print hits/misses/uncacheable/mismatches (mismatches must be 0). Also time
//!       compiling B with the cache cold (fresh context, empty cache) vs warm (cache filled from
//!       A), verify off, median of 5. With --disk the check runs in a second context that only
//!       has the on-disk store.
//!
//!   fill <A.js> --disk DIR
//!       One process: fill the on-disk store from A and flush.
//!
//!   check <B.js> --disk DIR
//!       A second process: compile B against the on-disk store with verify mode on.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use brimstone_core::common::options::OptionsBuilder;
use brimstone_core::runtime::bytecode::fncache::FunctionCacheStats;
use brimstone_core::runtime::bytecode::fncache::key::UncacheableReason;
use brimstone_core::runtime::bytecode::lazy::generate_all_lazy_functions;
use brimstone_core::runtime::embed;
use brimstone_core::runtime::gc::HandleScope;
use brimstone_core::runtime::{Context, ContextBuilder};

fn new_context(disk: Option<&Path>) -> Context {
    let options = Rc::new(OptionsBuilder::new().lazy_functions(true).build().unwrap());
    let cx = ContextBuilder::new()
        .set_options(options)
        .build()
        .expect("context");
    embed::set_function_cache_enabled(cx, true);
    embed::set_realm_cache_partition(cx, cx.initial_realm(), b"https://oracle.test");
    if let Some(dir) = disk {
        embed::set_function_cache_dir(cx, Some(dir.to_path_buf()));
    }
    cx
}

/// Compile a script (lazy on) and generate the bytecode of every function stub, so that every
/// function of the file goes through the cache. Returns the wall time of both steps.
fn compile_and_generate_all(cx: Context, path: &str, text: &str) -> Duration {
    let realm = cx.initial_realm();
    let scope = HandleScope::enter(cx);
    let start = Instant::now();
    let script = embed::compile_script(cx, realm, text, path, 1).expect("compile failed");
    let _ = script;
    if let Err(e) = generate_all_lazy_functions(cx) {
        panic!("lazy generation failed: {e:?}");
    }
    let elapsed = start.elapsed();
    scope.exit();
    elapsed
}

fn stats(cx: Context) -> FunctionCacheStats {
    embed::function_cache_stats(cx)
}

fn delta(before: &FunctionCacheStats, after: &FunctionCacheStats) -> FunctionCacheStats {
    FunctionCacheStats {
        lookups: after.lookups - before.lookups,
        hits: after.hits - before.hits,
        misses: after.misses - before.misses,
        uncacheable: std::array::from_fn(|i| after.uncacheable[i] - before.uncacheable[i]),
        verify_mismatches: after.verify_mismatches - before.verify_mismatches,
        bytes_stored: after.bytes_stored - before.bytes_stored,
        key_time_ns: after.key_time_ns - before.key_time_ns,
        instantiate_time_ns: after.instantiate_time_ns - before.instantiate_time_ns,
    }
}

fn print_stats(label: &str, d: &FunctionCacheStats) {
    let hit_rate = if d.lookups > 0 {
        d.hits as f64 * 100.0 / d.lookups as f64
    } else {
        0.0
    };
    println!(
        "{label}: lookups {} | hits {} ({:.1}%) | misses {} | uncacheable {} | verify mismatches {} | bytes stored {:.1} MB | key {:.1} ms | instantiate {:.1} ms",
        d.lookups,
        d.hits,
        hit_rate,
        d.misses,
        d.uncacheable.iter().sum::<u64>(),
        d.verify_mismatches,
        d.bytes_stored as f64 / 1e6,
        d.key_time_ns as f64 / 1e6,
        d.instantiate_time_ns as f64 / 1e6,
    );
    for reason in UncacheableReason::ALL {
        let n = d.uncacheable[reason.index()];
        if n > 0 {
            println!("    uncacheable[{}]: {}", reason.name(), n);
        }
    }
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn short_name(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .chars()
        .take(48)
        .collect()
}

/// Fill the cache from `a` in `cx`, then check `b` in `cx` (or in a fresh context with the same
/// disk dir) with verify mode on. Prints the stats of the check phase.
fn fill_then_check(a: &str, b: &str, disk: Option<&Path>) {
    let a_text = std::fs::read_to_string(a).expect("read A");
    let b_text = std::fs::read_to_string(b).expect("read B");

    let cx = new_context(disk);
    let t_fill = compile_and_generate_all(cx, a, &a_text);
    let before_fill = stats(cx);
    if disk.is_some() {
        embed::function_cache_flush(cx);
    }
    println!(
        "fill from {} ({:.1} MB): {:.1} ms, {} functions cached",
        short_name(a),
        a_text.len() as f64 / 1e6,
        t_fill.as_secs_f64() * 1000.0,
        before_fill.lookups - before_fill.hits,
    );
    print_stats("  fill phase", &delta(&FunctionCacheStats::default(), &before_fill));

    // Check B: same context, or a fresh one that only has the disk store.
    let cx_check = if disk.is_some() {
        new_context(disk)
    } else {
        cx
    };
    embed::set_function_cache_verify(cx_check, true);
    let before = stats(cx_check);
    let t_check = compile_and_generate_all(cx_check, b, &b_text);
    let after = stats(cx_check);
    println!(
        "check {} ({:.1} MB) verify on: {:.1} ms",
        short_name(b),
        b_text.len() as f64 / 1e6,
        t_check.as_secs_f64() * 1000.0,
    );
    print_stats("  check phase", &delta(&before, &after));
    let d = delta(&before, &after);
    if d.verify_mismatches != 0 {
        println!("  FAIL: {} verify mismatches", d.verify_mismatches);
        std::process::exit(1);
    }
}

/// Time compiling B with the cache cold (fresh context each time) vs warm (cache filled from A),
/// verify off, median of 5.
fn time_cold_vs_warm(a: &str, b: &str) {
    let a_text = std::fs::read_to_string(a).expect("read A");
    let b_text = std::fs::read_to_string(b).expect("read B");

    let mut cold = Vec::new();
    for _ in 0..5 {
        let cx = new_context(None);
        cold.push(compile_and_generate_all(cx, b, &b_text));
    }

    let cx = new_context(None);
    compile_and_generate_all(cx, a, &a_text);
    let mut warm = Vec::new();
    for _ in 0..5 {
        warm.push(compile_and_generate_all(cx, b, &b_text));
    }

    let cold = median(cold);
    let warm = median(warm);
    println!(
        "compile+generate of {} ({:.1} MB): cold {:.1} ms | warm {:.1} ms ({:.1}%)",
        short_name(b),
        b_text.len() as f64 / 1e6,
        cold.as_secs_f64() * 1000.0,
        warm.as_secs_f64() * 1000.0,
        warm.as_secs_f64() * 100.0 / cold.as_secs_f64(),
    );
}

/// Split the arguments into positional arguments and the --disk directory.
fn parse_args(args: &[String]) -> (Vec<&String>, Option<PathBuf>) {
    let mut positional = Vec::new();
    let mut disk = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--disk" {
            disk = Some(PathBuf::from(it.next().expect("--disk needs a directory")));
        } else {
            positional.push(a);
        }
    }
    (positional, disk)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (positional, disk) = parse_args(&args);

    match positional.first().map(|s| s.as_str()) {
        Some("self") => {
            let file = positional
                .get(1)
                .expect("usage: self <file.js> [--disk DIR]");
            fill_then_check(file, file, disk.as_deref());
        }
        Some("pair") => {
            let a = positional
                .get(1)
                .expect("usage: pair <A.js> <B.js> [--disk DIR]");
            let b = positional
                .get(2)
                .expect("usage: pair <A.js> <B.js> [--disk DIR]");
            fill_then_check(a, b, disk.as_deref());
            time_cold_vs_warm(a, b);
        }
        Some("fill") => {
            let file = positional.get(1).expect("usage: fill <A.js> --disk DIR");
            let disk = disk.clone().expect("fill needs --disk DIR");
            let text = std::fs::read_to_string(file).expect("read A");
            let cx = new_context(Some(&disk));
            let t = compile_and_generate_all(cx, file, &text);
            embed::function_cache_flush(cx);
            println!(
                "filled disk store {} from {} ({:.1} MB) in {:.1} ms",
                disk.display(),
                short_name(file),
                text.len() as f64 / 1e6,
                t.as_secs_f64() * 1000.0,
            );
            print_stats("  fill phase", &stats(cx));
        }
        Some("check") => {
            let file = positional.get(1).expect("usage: check <B.js> --disk DIR");
            let disk = disk.clone().expect("check needs --disk DIR");
            let text = std::fs::read_to_string(file).expect("read B");
            let cx = new_context(Some(&disk));
            embed::set_function_cache_verify(cx, true);
            let before = stats(cx);
            let t = compile_and_generate_all(cx, file, &text);
            let after = stats(cx);
            println!(
                "checked {} ({:.1} MB) against disk store {} in {:.1} ms",
                short_name(file),
                text.len() as f64 / 1e6,
                disk.display(),
                t.as_secs_f64() * 1000.0,
            );
            print_stats("  check phase", &delta(&before, &after));
            let d = delta(&before, &after);
            if d.verify_mismatches != 0 {
                println!("  FAIL: {} verify mismatches", d.verify_mismatches);
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!(
                "usage: fncache-check <self <file.js> | pair <A.js> <B.js> | fill <A.js> --disk DIR | check <B.js> --disk DIR> [--disk DIR]"
            );
            std::process::exit(2);
        }
    }
}
