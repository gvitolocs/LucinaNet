/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Sampling JavaScript profiler. Enabled by setting `LUCINANET_JS_PROFILE` to a path prefix; a
//! `js-profiler` thread then requests an interrupt every `LUCINANET_JS_PROFILE_US` microseconds (default
//! 1000) and the engine thread's interrupt hook calls `take_sample`, which captures the stack and folds it.
//! `flush` writes `<prefix>.<pid>.folded` (flamegraph.pl input) and `<prefix>.<pid>.txt` (a summary).

#[path = "profile_folded.rs"]
mod folded;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use brimstone_core::runtime::embed::{self, InterruptHandle};
use brimstone_core::runtime::Context;

/// Set by the profiler thread when the next interrupt should take a sample.
static SAMPLE_DUE: AtomicBool = AtomicBool::new(false);

/// Set by `stop`: the profiler thread exits at its next tick.
static STOPPED: AtomicBool = AtomicBool::new(false);

/// All profiler state; `None` until `start` enables it and after `stop`.
static PROFILE: Mutex<Option<State>> = Mutex::new(None);

struct State {
    /// The engine thread being profiled; samples and `stop` from other threads (workers) are ignored.
    thread: std::thread::ThreadId,
    prefix: String,
    interval_us: u64,
    started: Instant,
    last_request: Instant,
    samples: u64,
    last_flush: Instant,
    /// The engine compile stats at the last sample (lazy stubs, functions generated lazily, bytes deferred).
    compile_stats: String,
    folded: folded::Folded,
}

/// Start profiling if `LUCINANET_JS_PROFILE` names a path prefix. Only the first call in the process does
/// anything (the script thread); calls from other contexts (worker threads) no-op.
pub fn start(handle: InterruptHandle) {
    let Ok(prefix) = std::env::var("LUCINANET_JS_PROFILE") else {
        return;
    };
    if prefix.is_empty() {
        return;
    }
    let interval_us = std::env::var("LUCINANET_JS_PROFILE_US")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&us| us > 0)
        .unwrap_or(1000);

    let mut guard = PROFILE.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_some() {
        return;
    }

    let interval = Duration::from_micros(interval_us);
    let thread_handle = handle.clone();
    let spawned = std::thread::Builder::new()
        .name("js-profiler".to_string())
        .spawn(move || loop {
            std::thread::sleep(interval);
            if STOPPED.load(Ordering::Acquire) {
                return;
            }
            // A sample still pending keeps its first request time, so the time until the engine reaches an interrupt point
            // (spent outside JS) is weighed and marked `(delayed)`.
            if SAMPLE_DUE.load(Ordering::Acquire) {
                continue;
            }
            if let Ok(mut guard) = PROFILE.lock() {
                if let Some(state) = guard.as_mut() {
                    state.last_request = Instant::now();
                }
            }
            SAMPLE_DUE.store(true, Ordering::Release);
            thread_handle.request();
        });
    if spawned.is_err() {
        return;
    }

    // Page renders often end with `exit` and never destroy the context: write the profile at process exit too.
    #[crate::engine::poison::engine_shim(exempt = "an atexit handler: runs after main returned, with no context")]
    extern "C" fn flush_at_exit() {
        if let Ok(guard) = PROFILE.try_lock() {
            if let Some(state) = guard.as_ref() {
                flush_locked(state);
            }
        }
    }
    unsafe { libc::atexit(flush_at_exit) };

    *guard = Some(State {
        thread: std::thread::current().id(),
        prefix,
        interval_us,
        started: Instant::now(),
        last_request: Instant::now(),
        samples: 0,
        last_flush: Instant::now(),
        compile_stats: String::new(),
        folded: folded::Folded::new(),
    });
}

/// Whether the interrupt hook should take a sample. Cheap: one atomic load.
pub fn sample_due() -> bool {
    SAMPLE_DUE.load(Ordering::Acquire)
}

/// Take a sample if one is due and this is the profiled thread (the interrupt hook calls it on every engine thread).
pub fn take_sample(bs: Context) {
    let mut guard = PROFILE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(state) = guard.as_mut() else {
        return;
    };
    if state.thread != std::thread::current().id() || !SAMPLE_DUE.swap(false, Ordering::AcqRel) {
        return;
    }
    let weight_us = state.last_request.elapsed().as_micros() as u64;
    let h = embed::heap_stats(bs);
    state.compile_stats = format!(
        "{:?}\nmemory: heap {:.0} MB (semispace {:.0} of {:.0} MB used, permanent {:.0} MB, host objects {:.0} MB), retained parse trees {:.0} MB\ngc: {} collections ({} grew, {} shrank), live after the last {:.0} MB, peak live {:.0} MB, released {:.0} MB",
        embed::compile_stats(bs),
        h.heap_size_bytes as f64 / 1e6,
        h.semispace_used_bytes as f64 / 1e6,
        h.semispace_capacity_bytes as f64 / 1e6,
        h.permanent_bytes as f64 / 1e6,
        h.host_space_used_bytes as f64 / 1e6,
        embed::retained_parse_bytes(bs) as f64 / 1e6,
        h.gc_count,
        h.gc_grow_count,
        h.gc_shrink_count,
        h.last_live_bytes as f64 / 1e6,
        h.peak_live_bytes as f64 / 1e6,
        h.released_bytes as f64 / 1e6,
    );
    let census = embed::heap_census(bs);
    if !census.is_empty() {
        state.compile_stats.push_str("
census (live after the last collection):");
        for entry in census.iter().take(16) {
            state.compile_stats.push_str(&format!(
                "
  {:<28} {:>9} items {:>8.1} MB",
                entry.kind,
                entry.count,
                entry.bytes as f64 / 1e6
            ));
        }
    }
    let delayed = weight_us > 3 * state.interval_us;

    let Ok(captured) = embed::capture_stack(bs, 64) else {
        return;
    };
    let frames: Vec<folded::Frame> = captured
        .into_iter()
        .map(|f| folded::Frame {
            name: f.function_name,
            file: f.file_name,
            line: f.line,
            native: f.is_native,
        })
        .collect();
    state.folded.add(&frames, weight_us, delayed);
    state.samples += 1;

    // Write every 500 samples or 200 ms, whichever comes first: a page render can exit without destroying the context or
    // running exit handlers, so the files on disk must already be close to complete.
    if state.samples % 500 == 0 || state.last_flush.elapsed() >= Duration::from_millis(200) {
        state.last_flush = Instant::now();
        flush_locked(state);
    }
}

/// Write the folded stacks and the summary to `<prefix>.<pid>.folded` / `<prefix>.<pid>.txt`.
pub fn flush() {
    let mut guard = PROFILE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = guard.as_ref() {
        flush_locked(state);
    }
}

/// Stop profiling and write the final files. Called from `JS_DestroyContext`.
pub fn stop() {
    let mut guard = PROFILE.lock().unwrap_or_else(|e| e.into_inner());
    if guard.as_ref().is_some_and(|state| state.thread == std::thread::current().id()) {
        STOPPED.store(true, Ordering::Release);
        if let Some(state) = guard.take() {
            flush_locked(&state);
        }
    }
}

fn flush_locked(state: &State) {
    let pid = std::process::id();
    let folded_path = format!("{}.{pid}.folded", state.prefix);
    let summary_path = format!("{}.{pid}.txt", state.prefix);
    let _ = std::fs::write(&folded_path, state.folded.folded_text());
    let wall_ms = state.started.elapsed().as_millis() as u64;
    let _ = std::fs::write(&summary_path, format!("{}compile: {}\n", state.folded.summary(wall_ms), state.compile_stats));
}
