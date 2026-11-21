/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! SpiderMonkey's JSContext::isRuntimeCodeGenEnabled on the LucinaNet engine.
//! The callback may execute JS and collect. Every slot behind its handles must
//! therefore be traced and updated, including the original argument objects.

use super::cx::{Cx, JS_AddExtraGCRootsTracer, JS_RemoveExtraGCRootsTracer};
use crate::jsapi::*;
use ::std::{ffi::c_void, ptr};
use brimstone_core::runtime::embed::{self, api, CompileStringsKind};
use brimstone_core::runtime::{Context, EvalResult, Handle, Value};

struct PolicyRoots {
    eval_object: *mut JSObject,
    code: *mut JSString,
    body: *mut JSString,
    parameter_strings: Vec<*mut JSString>,
    parameter_args: Vec<JS::Value>,
    body_arg: JS::Value,
}

#[super::poison::engine_shim(gc)]
unsafe extern "C" fn trace_policy(trc: *mut JSTracer, data: *mut c_void) {
    let p = data.cast::<PolicyRoots>();
    unsafe {
        super::gc::trace_cell_slot(trc, ptr::addr_of_mut!((*p).eval_object).cast());
        super::gc::trace_cell_slot(trc, ptr::addr_of_mut!((*p).code).cast());
        super::gc::trace_cell_slot(trc, ptr::addr_of_mut!((*p).body).cast());
        for string in &mut (*p).parameter_strings {
            super::gc::trace_cell_slot(trc, ptr::from_mut(string).cast());
        }
        for value in &mut (*p).parameter_args {
            super::gc::trace_value_slot(trc, ptr::addr_of_mut!(value.asBits_));
        }
        super::gc::trace_value_slot(trc, ptr::addr_of_mut!((*p).body_arg.asBits_));
    }
}

struct RootGuard {
    cx: *mut JSContext,
    roots: *mut PolicyRoots,
}

impl Drop for RootGuard {
    fn drop(&mut self) {
        unsafe {
            JS_RemoveExtraGCRootsTracer(self.cx, Some(trace_policy), self.roots.cast());
            drop(Box::from_raw(self.roots));
        }
    }
}

fn handle<T>(ptr: *const T) -> JS::Handle<T> {
    JS::Handle {
        ptr,
        _phantom_0: Default::default(),
    }
}

pub unsafe fn can_compile_strings(
    cx: *mut Cx,
    bs: Context,
    kind: CompileStringsKind,
    code: Handle<api::StringValue>,
    parameters: &[Handle<api::StringValue>],
    body: Option<Handle<api::StringValue>>,
    parameter_args: &[Handle<Value>],
    body_arg: Handle<Value>,
) -> EvalResult<bool> {
    let callbacks = unsafe { (*cx).security_callbacks };
    let callback = if callbacks.is_null() {
        None
    } else {
        unsafe { (*callbacks).contentSecurityPolicyAllows }
    };
    let Some(callback) = callback else {
        return Ok(true);
    };
    let roots = Box::into_raw(Box::new(PolicyRoots {
        eval_object: ptr::null_mut(),
        code: (*code).as_ptr().cast(),
        body: body.map_or(ptr::null_mut(), |s| (*s).as_ptr().cast()),
        parameter_strings: parameters.iter().map(|s| (**s).as_ptr().cast()).collect(),
        parameter_args: parameter_args
            .iter()
            .map(|v| JS::Value {
                asBits_: v.as_raw_bits(),
            })
            .collect(),
        body_arg: JS::Value {
            asBits_: body_arg.as_raw_bits(),
        },
    }));
    let raw = cx.cast::<JSContext>();
    let _guard = RootGuard { cx: raw, roots };
    unsafe { JS_AddExtraGCRootsTracer(raw, Some(trace_policy), roots.cast()) };
    // A wasm compilation is neither eval nor Function: the engine asks with no code, no parameters and no body, so the
    // callback sees only the runtime and the undefined compilation type.
    let (runtime, compilation) = match kind {
        CompileStringsKind::Eval => (JS::RuntimeCode::JS, JS::CompilationType::DirectEval),
        CompileStringsKind::IndirectEval => (JS::RuntimeCode::JS, JS::CompilationType::IndirectEval),
        CompileStringsKind::Function => (JS::RuntimeCode::JS, JS::CompilationType::Function),
        CompileStringsKind::Wasm => (JS::RuntimeCode::WASM, JS::CompilationType::Undefined),
    };
    let mut allowed = false;
    // No borrow of Cx or PolicyRoots survives the callback's re-entry.
    let ok = unsafe {
        callback(
            raw,
            runtime,
            handle(ptr::addr_of!((*roots).code)),
            compilation,
            handle(ptr::addr_of!((*roots).parameter_strings).cast()),
            handle(ptr::addr_of!((*roots).body)),
            handle(ptr::addr_of!((*roots).parameter_args).cast()),
            handle(ptr::addr_of!((*roots).body_arg)),
            &mut allowed,
        )
    };
    if ok {
        return Ok(allowed);
    }
    match unsafe { (*cx).take_pending() } {
        Some(bits) => Err(embed::throw_value(super::util::handle_of(bs, bits))),
        None => Err(super::uncatchable(bs)),
    }
}

/// The TrustedScript extraction hook Servo installs beside its CSP callback.
pub unsafe fn code_for_eval(
    cx: *mut Cx,
    bs: Context,
    object: Handle<api::ObjectValue>,
) -> EvalResult<Option<Handle<api::StringValue>>> {
    let callbacks = unsafe { (*cx).security_callbacks };
    let callback = if callbacks.is_null() {
        None
    } else {
        unsafe { (*callbacks).codeForEvalGets }
    };
    let Some(callback) = callback else {
        return Ok(None);
    };
    let roots = Box::into_raw(Box::new(PolicyRoots {
        eval_object: (*object).as_ptr().cast(),
        code: ptr::null_mut(),
        body: ptr::null_mut(),
        parameter_strings: Vec::new(),
        parameter_args: Vec::new(),
        body_arg: JS::Value {
            asBits_: crate::boundary::value_bits::undefined(),
        },
    }));
    let raw = cx.cast::<JSContext>();
    let _guard = RootGuard { cx: raw, roots };
    unsafe { JS_AddExtraGCRootsTracer(raw, Some(trace_policy), roots.cast()) };
    let ok = unsafe {
        callback(
            raw,
            handle(ptr::addr_of!((*roots).eval_object)),
            JS::MutableHandle {
                ptr: ptr::addr_of_mut!((*roots).code),
                _phantom_0: Default::default(),
            },
        )
    };
    if !ok {
        return match unsafe { (*cx).take_pending() } {
            Some(bits) => Err(embed::throw_value(super::util::handle_of(bs, bits))),
            None => Err(super::uncatchable(bs)),
        };
    }
    let source = unsafe { (*roots).code };
    Ok(if source.is_null() {
        None
    } else {
        Some(
            brimstone_core::runtime::HeapPtr::<api::StringValue>::from_ptr(source.cast())
                .to_handle(),
        )
    })
}
