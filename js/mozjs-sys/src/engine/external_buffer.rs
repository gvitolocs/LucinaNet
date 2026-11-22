/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `glue::NewExternalArrayBuffer` and `JS::NewArrayBufferWithUserOwnedContents` — an ArrayBuffer over memory the caller
//! owns (mozjs_sys 153.6.0-1's `js/public/ArrayBuffer.h`, `js/src/vm/ArrayBufferObject.cpp`). WebAssembly design §5.3,
//! slice H2. Today one page path reaches the first: `GPUBuffer.getMappedRange` → `GPUDataBlock::view`
//! (`servo/components/script_webgpu/datablock.rs:98`), which passes a `free_func` and an `Arc` of its shared memory as
//! `freeUserData`. The second has no Servo caller; `mozjs/examples/wasm.rs:98` is what it is there for.
//!
//! **Ownership** is Brimstone's `ExternalOwner` (`runtime/embed/api.rs:301-333`), which says who gives the bytes back:
//!
//! * `Free(closure)` — SpiderMonkey's `NewExternalArrayBuffer` with a `freeFunc`: the engine owns the memory from the call
//!   on. The closure is the caller's `(freeFunc, contents, freeUserData)` triple, so `freeFunc(contents, freeUserData)`
//!   runs once, after the buffer has been detached or has died and been collected, or when the context is destroyed.
//! * `None` — `NewArrayBufferWithUserOwnedContents`, and `NewExternalArrayBuffer` with a null `freeFunc`: the caller keeps
//!   the memory, touches none of it while the buffer is attached, and frees it only after it has detached the buffer.
//!   Nothing on this side frees those bytes, at a collection or at teardown.
//!
//! **Where "exactly once" comes from.** The engine keeps a `Free` closure in an internal cell (`embed/mod.rs:1133-1160`):
//! the buffer holds that cell while it is attached, a detach only drops the edge, the cell's finalizer *takes* the closure
//! before it runs it, and `embed::teardown` finalizes the cells still alive when the context dies. So a buffer the
//! collector never reached still pays, once, and no path pays twice. When the buffer cannot be made the engine drops the
//! closure uncalled (`forget_external_free`) and the memory stays with the caller, as SpiderMonkey's
//! `BufferContentsDeleter` leaves it on the same path — which is why this glue never calls `freeFunc` itself: not on a
//! refusal, not when the shim's wrap catches a panic, not at teardown.
//!
//! **The closure runs inside a collection or at teardown**, with no JS on the stack. It is the caller's function pointer
//! and the caller's `freeUserData`, and this file calls only that: it allocates nothing, enters no engine state and
//! touches no ArrayBuffer's bytes, which is the engine's rule for this closure (`api.rs:311-315`) and what makes a call
//! from the collector safe. The caller's `freeFunc` is held to the same rule: it must not call back into the engine.
//! The closure is given the `contents` the caller handed us, not the pointer the engine stored: the two differ only for
//! a zero-length `Free` buffer, where the engine keeps an aligned dangling pointer of its own.
//!
//! **Refusals** are the engine's and answer like the other buffer constructors of this glue — null with the exception
//! state. `contents` null or not 8-byte aligned with a non-zero `nbytes` is a TypeError (`EXTERNAL_ALIGNMENT`), a length
//! over the engine's maximum a RangeError; a zero length ignores `contents` and is accepted.

use ::std::ffi::c_void;
use ::std::ptr;

use brimstone_core::runtime::embed::{self, api};
use brimstone_core::runtime::gc::HandleScope;
use brimstone_core::runtime::Handle;

use super::cx::cx_of;
use super::util::fail;
use crate::jsapi::*;

/// `JS::NewExternalArrayBuffer(cx, nbytes, contents, freeFunc, freeUserData)`: an ArrayBuffer over the caller's `contents`.
/// With a `freeFunc` the engine owns those bytes from the call on and runs that callback when the buffer is done with them;
/// without one, the caller owns them and detaches the buffer before freeing them.
#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__NewExternalArrayBuffer(
    cx: *mut JSContext,
    nbytes: usize,
    contents: *mut c_void,
    freeFunc: JS::BufferContentsFreeFunc,
    freeUserData: *mut c_void,
) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let owner = match freeFunc {
        None => api::ExternalOwner::None,
        Some(free) => api::ExternalOwner::Free(Box::new(move || {
            // Runs inside a collection or at teardown: `freeFunc` and nothing else. The caller's `freeFunc` must not
            // call back into the engine either, for the same reason — it runs without JS on the stack.
            unsafe { free(contents, freeUserData) };
        })),
    };
    // SAFETY: the caller's contract for `contents` and `nbytes` — the C++ header's, restated above — is
    // `array_buffer_new_external`'s, and the `Free` closure is the caller's own deleter, which the engine runs at most
    // once and only after the buffer has died and been collected or detached or the context has been destroyed. The
    // caller's `freeFunc` must not call back into the engine when that happens: it runs without JS on the stack,
    // possibly inside a collection or at teardown.
    let r = unsafe { api::array_buffer_new_external(bs, embed::current_realm(bs), contents.cast(), nbytes, owner) };
    let out = match r {
        Ok(b) => raw_of(b.as_object()),
        Err(e) => {
            unsafe { fail(cx, e) };
            ptr::null_mut()
        },
    };
    scope.exit();
    out
}

/// `JS::NewArrayBufferWithUserOwnedContents(cx, nbytes, contents)`: an external ArrayBuffer whose bytes stay the caller's,
/// which keeps them valid and untouched until it detaches the buffer, and frees them itself afterwards.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewArrayBufferWithUserOwnedContents(
    cx: *mut JSContext,
    nbytes: usize,
    contents: *mut c_void,
) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    // SAFETY: the caller keeps `contents` allocated and to itself while the buffer is attached and detaches before freeing
    // it; no closure is involved, so nothing on the engine's side ever gives the caller's memory away.
    let r = unsafe {
        api::array_buffer_new_external(bs, embed::current_realm(bs), contents.cast(), nbytes, api::ExternalOwner::None)
    };
    let out = match r {
        Ok(b) => raw_of(b.as_object()),
        Err(e) => {
            unsafe { fail(cx, e) };
            ptr::null_mut()
        },
    };
    scope.exit();
    out
}

#[inline]
fn raw_of(o: Handle<api::ObjectValue>) -> *mut JSObject {
    (*o).as_ptr().cast()
}
