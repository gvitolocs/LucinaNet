/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Natives: Servo's `JSNative`s called from JS. The original is SpiderMonkey's `CallJSNative` (`js/src/vm/Interpreter.cpp`) and
//! `JS::CallArgs` (`js/public/CallArgs.h`): `vp[0]` callee and return value, `vp[1]` this (the magic value when constructing),
//! `vp[2..2+argc]` the arguments, and `vp[2+argc]` new.target when constructing.
//!
//! The fork's trampoline hands us its own buffer; we copy it into a frame of `Cx::frame_values`, whose bounds and nesting are
//! the Verus-checked `boundary::roots::FrameStack`, so the layout is exactly SpiderMonkey's whatever the fork does.

use ::std::ffi::c_void;

use brimstone_core::runtime::embed::{self, NativeCall};
use brimstone_core::runtime::{Context as Bs, EvalResult, Value as BsValue};

use super::cx::Cx;
use crate::boundary::{bounds, value_bits};
use crate::jsapi::*;

type Native = unsafe extern "C" fn(*mut JSContext, u32, *mut JS::Value) -> bool;

/// Pops the native's frame however the call ends. Holds the context by raw pointer: the native re-enters the engine, which
/// reaches the same `Cx` through `ServoHost.cx`, so no `&mut Cx` may stay live across the call.
struct FrameGuard {
    cx: *mut Cx,
}

impl Drop for FrameGuard {
    fn drop(&mut self) {
        // SAFETY: the context outlives every native call, and no reference to it is live here.
        let popped = unsafe { (*self.cx).frames.pop() };
        debug_assert!(popped.is_some());
    }
}

/// Called by Brimstone's trampoline for every native.
///
/// # Safety
/// `cx` is this thread's live context. No reference to it may be live across this call: the native gets `cx` and re-enters.
pub unsafe fn call_native(cx: *mut Cx, bs: Bs, native: *const c_void, call: &mut NativeCall) -> EvalResult<()> {
    // A poisoned context calls nothing of Servo's: the engine is still unwinding script in the frames above the shim that
    // caught the panic, on state nobody trusts. Uncatchable, so no catch or finally runs (docs/edge-wrap.md section d).
    if super::poison::thread_poisoned().is_some() {
        return Err(super::uncatchable(bs));
    }
    let argc = call.argc();
    let constructing = call.is_constructing();
    let Some(len) = bounds::vp_len(argc + constructing as u32, 0) else {
        return brimstone_core::runtime::error::range_error(bs, "too many arguments");
    };
    let Some(start) = (unsafe { (*cx).frames.push(len) }) else {
        return brimstone_core::runtime::error::range_error(bs, "too much recursion");
    };
    // From the array's raw base (`gc::FrameValues`), as the GC and nested natives reach it: no reborrow invalidates `vp`.
    let vp = unsafe { (*cx).frame_values.base().add(start) };
    unsafe {
        *vp = call.callee().as_raw_bits();
        *vp.add(1) = if constructing { value_bits::empty() } else { call.this().as_raw_bits() };
        for i in 0..argc {
            *vp.add(2 + i as usize) = call.arg(i).as_raw_bits();
        }
        if let Some(nt) = call.new_target() {
            *vp.add(2 + argc as usize) = nt.as_value().as_raw_bits();
        }
    }
    let raw_cx: *mut JSContext = cx.cast();
    let guard = FrameGuard { cx };
    // SAFETY: `native` came from Servo through JS_NewFunction & co. and has the JSNative signature.
    let f: Native = unsafe { ::std::mem::transmute(native) };
    let ok = unsafe { f(raw_cx, argc, vp.cast()) };
    let rval = unsafe { *vp };
    drop(guard);
    if ok {
        call.set_return_value(BsValue::from_raw_bits(non_zero(rval)));
        return Ok(());
    }
    match unsafe { (*cx).take_pending() } {
        Some(bits) => Err(embed::throw_value(BsValue::from_raw_bits(non_zero(bits)).to_handle(bs))),
        // A native returned false with nothing pending: SpiderMonkey's uncatchable exception (script termination).
        None => Err(super::uncatchable(bs)),
    }
}

/// A frame of `n` value slots (undefined to start), traced and updated by the GC while `f` runs; None when the frame stack is
/// full. For handles given to Servo's traps and hooks.
///
/// # Safety
/// `cx` is this thread's live context. `f` may re-enter the engine, so `cx` is reached only for the push and the pop, and no
/// reference to it may be live across this call.
pub unsafe fn with_slots<R>(cx: *mut Cx, n: usize, f: impl FnOnce(*mut u64) -> R) -> Option<R> {
    let start = unsafe { (*cx).frames.push(n) }?;
    let slots = unsafe { (*cx).frame_values.base().add(start) };
    for i in 0..n {
        unsafe { *slots.add(i) = value_bits::undefined() };
    }
    let r = f(slots);
    let popped = unsafe { (*cx).frames.pop() };
    debug_assert_eq!(popped, Some(start));
    Some(r)
}

/// Brimstone's `Value` is never zero; a zero slot is undefined to us (`jsval`).
#[inline]
pub fn non_zero(bits: u64) -> u64 {
    if bits == 0 { value_bits::undefined() } else { bits }
}

/// SpiderMonkey's inline CallArgsFromVp, exported by jsglue for embedders.
/// The native frame already has the same callee/this/arguments layout.
#[super::poison::engine_shim(exempt = "pure pointer arithmetic; JS::CallArgs has no failure value")]
pub unsafe extern "C" fn glue__JS_CallArgsFromVp(argc: u32, vp: *mut JS::Value) -> JS::CallArgs {
    unsafe { JS::CallArgs::from_vp(vp, argc) }
}
