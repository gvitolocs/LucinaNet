/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! the embedder half of the engine edge wrap (glue `docs/edge-wrap.md`).
//!
//! Two producers leave a panic waiting in this thread's slot: the glue's engine shims (which also
//! poison the thread's context, so every later gated shim refuses) and Servo's own `wrap_panic`
//! around its `extern "C"` callbacks (which does not poison: the engine stays trusted, but the
//! panic must still end the thread). Both are re-raised by `js::panic::maybe_resume_unwind`, which
//! may only run in a pure Rust frame of an event loop, never under an `extern "C"` frame and never
//! while that frame still holds a `JSAutoRealm` (on a poisoned thread `LeaveRealm` refuses, so a
//! realm dropped by the unwind is never popped).
//!
//! The choke point is [`engine_turn`]: every event-loop site that may enter the engine runs that
//! work as one turn (the script thread's task dispatch, its gather loop, its rendering and GC tail,
//! its idle trim; each worker task; each worklet message, control batch and collection). A turn is
//! gated before it starts and resumed after it ends, after every realm the turn entered has been
//! dropped, because the turn's realms live inside the closure. Nested pumps that run while engine
//! frames are on the stack (synchronous XHR's `process_event`, the devtools pause loop) use
//! [`nested_entry_allowed`] instead: they refuse, and leave the re-raise to the enclosing turn.

use std::panic::{catch_unwind, resume_unwind};

use js::poison::PoisonReason;

/// The gate in front of a turn. Re-raises a panic still waiting on this thread (the caller is a
/// pure Rust frame of the event loop), then answers `Some(reason)` when the thread's context is
/// poisoned with nothing left to re-raise: the caller must then end its loop without entering the
/// engine. `None`: the turn may run.
pub(crate) fn engine_entry_refused() -> Option<PoisonReason> {
    js::panic::maybe_resume_unwind();
    let reason = js::poison::thread_poisoned();
    if reason.is_some() {
        error!("lucinanet-js: engine entry refused on a poisoned thread");
    }
    reason
}

/// Runs `turn`, a piece of event-loop work that may enter the engine, behind the gate and with a
/// resume after it. `Err(reason)`: the gate refused and `turn` did not run; the caller ends its
/// loop. The resume runs after `turn` returned, so any realm `turn` entered has already been left.
/// When `turn` itself unwinds (a resume deeper down, or a plain Rust panic) nothing here runs.
pub(crate) fn engine_turn<R>(turn: impl FnOnce() -> R) -> Result<R, PoisonReason> {
    if let Some(reason) = engine_entry_refused() {
        return Err(reason);
    }
    let result = turn();
    js::panic::maybe_resume_unwind();
    Ok(result)
}

/// Whether a nested pump may run one more task: false when the thread is poisoned or a panic waits
/// in its slot. Never re-raises (the caller has engine and `extern "C"` frames below it); the
/// enclosing [`engine_turn`] does that once those frames are gone.
pub(crate) fn nested_entry_allowed() -> bool {
    js::poison::thread_poisoned().is_none() && !payload_pending()
}

/// Whether a panic waits in this thread's slot. The slot's owner (`mozjs_sys::engine::poison`) is
/// only reachable from here through `mozjs::panic`, so the payload is taken by catching the
/// re-raise and put back through `wrap_panic`, which stores it again: the slot was just emptied, so
/// it is kept. `resume_unwind` does not run the panic hook, so neither step reports anything.
fn payload_pending() -> bool {
    match catch_unwind(js::panic::maybe_resume_unwind) {
        Ok(()) => false,
        Err(payload) => {
            let mut payload = Some(payload);
            js::panic::wrap_panic(&mut || {
                if let Some(payload) = payload.take() {
                    resume_unwind(payload);
                }
            });
            true
        },
    }
}
