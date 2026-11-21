/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! LucinaNet: idle-time trimming of the parse trees Brimstone keeps for lazily compiled functions.
//!
//! Brimstone compiles a function on its first call and keeps the script's parse tree until then. Most of a big app's
//! functions are never called: on YouTube's watch page that kept about 250 MB alive (RSS 684 MB against SpiderMonkey's 396 MB,
//! 2026-11-07). After a page's load event the script thread asks for a trim; then, whenever it would block waiting for a
//! message, it runs one short engine slice instead, until the engine reports the retained trees within its budget
//! (`LUCINANET_JS_PARSE_BUDGET_MB`, default 64). A message that arrives is handled first, so the trim never delays input,
//! timers or rendering by more than one slice. Without the `brimstone` feature (SpiderMonkey) nothing here does anything.

use std::cell::Cell;
#[cfg(feature = "brimstone")]
use std::time::Duration;

/// One slice of engine work between two looks at the message queue: under one frame at 60 Hz.
#[cfg(feature = "brimstone")]
const SLICE: Duration = Duration::from_millis(12);

thread_local! {
    static PENDING: Cell<bool> = const { Cell::new(false) };
}

/// A document of this thread finished loading: trim when the thread is next idle. `LUCINANET_JS_IDLE_TRIM=0` turns the
/// idle trim off, to compare.
pub(crate) fn request() {
    if cfg!(feature = "brimstone") && std::env::var("LUCINANET_JS_IDLE_TRIM").as_deref() != Ok("0") {
        PENDING.with(|pending| pending.set(true));
    }
}

/// Whether a trim is still wanted.
#[cfg_attr(not(feature = "brimstone"), expect(dead_code))]
pub(crate) fn pending() -> bool {
    PENDING.with(Cell::get)
}

/// Runs one slice. Returns whether more work is left; when none is, the request is done.
///
/// # Safety
/// `cx` is this thread's live JS context, and no script is running on it (the script thread is between tasks).
#[cfg(feature = "brimstone")]
pub(crate) unsafe fn run_slice(cx: *mut mozjs_sys::jsapi::JSContext) -> bool {
    // SAFETY: the caller's contract: this thread's context, between tasks. idle_trim runs no script and leaves no
    // exception pending.
    let more = unsafe { mozjs_sys::engine::lazy_trim::idle_trim(cx, SLICE) };
    if !more {
        PENDING.with(|pending| pending.set(false));
        if let Ok(value) = std::env::var("LUCINANET_TIMING") {
            if !value.is_empty() {
                // SAFETY: as above; this only reads a counter.
                let left = unsafe { mozjs_sys::engine::lazy_trim::retained_parse_bytes(cx) };
                eprintln!("lucinanet: js idle trim done, {} KiB of parse trees kept", left / 1024);
            }
        }
    }
    more
}
