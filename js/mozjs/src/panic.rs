/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};

// LucinaNet: the pending panic lives in `mozjs_sys::engine::poison`, one slot per thread shared by the two producers: a
// Servo callback's panic caught here by `wrap_panic`, and an engine panic caught at an engine shim (`#[engine_shim]`,
// docs/edge-wrap.md), which also poisons the thread's context. `maybe_resume_unwind` re-raises either in the caller's Rust
// frame, so the script thread unwinds into Servo's own panic handling.

/// If there is a pending panic, resume unwinding.
pub fn maybe_resume_unwind() {
    if let Some(error) = mozjs_sys::engine::poison::take_payload() {
        resume_unwind(error);
    }
}

/// Generic wrapper for JS engine callbacks panic-catching
// https://github.com/servo/servo/issues/26585
#[inline(never)]
pub fn wrap_panic(function: &mut dyn FnMut()) {
    match catch_unwind(AssertUnwindSafe(function)) {
        Ok(()) => {}
        // LucinaNet: a second payload while one waits is no longer an `assert!` (that panicked inside the catch arm, under
        // the callback's `extern "C"` frame: an abort). The first payload is the root cause and is kept.
        Err(payload) => mozjs_sys::engine::poison::store_payload(payload),
    }
}
