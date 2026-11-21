/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Saved frames: `JS::CaptureCurrentStack` and the `JS::GetSavedFrame*` accessors
//! (js/src/vm/Stack.cpp, js/src/vm/SavedStacks.cpp).
//!
//! The fork has no `SavedFrame` class, so each frame of the stack reported by
//! `embed::capture_stack` (innermost first) is wrapped in a host object of
//! SAVED_FRAME_CLASS whose reserved slots hold source, line, column, function
//! display name and parent. The parent slot links the chain, so `parent` is
//! walked back outwards as usual and the GC keeps the outer frames alive.
//!
//! The fork reports 1-based lines and columns; SpiderMonkey's `SavedFrame` line
//! is 0-based, so the line slot stores `line - 1` and the column slot the
//! 1-based column unchanged (`GetSavedFrameColumn` documents 1-based columns).
//!
//! No async stacks exist in the fork, so `GetSavedFrameAsyncCause` and
//! `GetSavedFrameAsyncParent` report "none" (null).

use brimstone_core::runtime::embed::{self, api, CapturedStackFrame, HostFlags};
use brimstone_core::runtime::gc::HandleScopeGuard;
use brimstone_core::runtime::{Handle, Value as BsValue};

use super::cx::cx_of;
use super::realm::object_ptr;
use super::util::fail;
use crate::jsapi::*;

const SOURCE_SLOT: u32 = 0;
const LINE_SLOT: u32 = 1;
const COLUMN_SLOT: u32 = 2;
const NAME_SLOT: u32 = 3;
const PARENT_SLOT: u32 = 4;

static SAVED_FRAME_CLASS: JSClass = JSClass {
    name: c"SavedFrame".as_ptr(),
    flags: 5 << JSCLASS_RESERVED_SLOTS_SHIFT,
    cOps: ::std::ptr::null(),
    spec: ::std::ptr::null(),
    ext: ::std::ptr::null(),
    oOps: ::std::ptr::null(),
};

fn is_saved_frame(obj: *mut JSObject) -> bool {
    !obj.is_null()
        && embed::host_class(object_ptr(obj)) == Some((&SAVED_FRAME_CLASS as *const JSClass).cast())
}

/// The tag byte of the `mozilla::Variant` `StackCapture`: `AllFrames` = 0,
/// `MaxFrames` = 1 (the limit in word 0), `FirstSubsumedFrame` = 2.
fn stack_capture_max(capture: *mut JS::StackCapture) -> usize {
    let words = unsafe { (*capture).0 };
    match words[3] & 0xff {
        1 => words[0] as usize,
        _ => 0, // AllFrames: no limit
    }
}

/// Wrap one reported frame in a SavedFrame host object.
unsafe fn new_frame(
    bs: brimstone_core::runtime::Context,
    frame: &CapturedStackFrame,
    parent: Option<Handle<api::ObjectValue>>,
) -> Result<Handle<api::ObjectValue>, brimstone_core::runtime::eval_result::EvalError> {
    let realm = embed::current_realm(bs);
    let obj = embed::new_host_object(
        bs,
        realm,
        (&SAVED_FRAME_CLASS as *const JSClass).cast(),
        HostFlags::empty(),
        None,
        5,
        ::std::ptr::null(),
        bs.undefined(),
    )?;

    if let Some(name) = &frame.function_name {
        let s = api::string_from_latin1(bs, name.as_bytes())?;
        embed::set_reserved_slot(*obj, NAME_SLOT, *s.as_value());
    }
    if let Some(source) = &frame.file_name {
        let s = api::string_from_latin1(bs, source.as_bytes())?;
        embed::set_reserved_slot(*obj, SOURCE_SLOT, *s.as_value());
    }
    embed::set_reserved_slot(*obj, LINE_SLOT, BsValue::smi(frame.line.map_or(-1, |l| l as i32 - 1)));
    embed::set_reserved_slot(*obj, COLUMN_SLOT, BsValue::smi(frame.column.map_or(-1, |c| c as i32)));
    embed::set_reserved_slot(
        *obj,
        PARENT_SLOT,
        parent.map_or(BsValue::undefined(), |p| *p.as_value()),
    );
    Ok(obj)
}

fn frame_result(savedFrame: *mut JSObject) -> JS::SavedFrameResult {
    if is_saved_frame(savedFrame) {
        JS::SavedFrameResult::Ok
    } else {
        JS::SavedFrameResult::AccessDenied
    }
}

/// Read a string slot, or `None` for undefined.
unsafe fn frame_string(bs: brimstone_core::runtime::Context, obj: *mut JSObject, slot: u32) -> Option<String> {
    let v = embed::reserved_slot(object_ptr(obj), slot);
    if !v.is_string() {
        return None;
    }
    api::string_to_rust_string(v.as_string().to_handle()).ok()
}

/// Fill `stringp` with the string in `slot`, or null for undefined.
unsafe fn frame_string_out(obj: *mut JSObject, slot: u32, out: JS::MutableHandle<*mut JSString>) -> bool {
    let v = embed::reserved_slot(object_ptr(obj), slot);
    if !v.is_string() {
        *out.ptr = ::std::ptr::null_mut();
        return false;
    }
    *out.ptr = v.as_string().as_ptr().cast();
    true
}

// `JS_StackCapture_AllFrames` / `JS_StackCapture_MaxFrames` (SpiderMonkey glue):
// fill the out-param in place as a `mozilla::Variant<AllFrames, MaxFrames,
// FirstSubsumedFrame>` — a 24-byte payload followed by a tag byte (word 3, low
// byte) — which `JS__CaptureCurrentStack` reads back.
#[super::poison::engine_shim(failure = unsafe { (*capture).0 = [0, 0, 0, 0] })]
pub unsafe extern "C" fn glue__JS_StackCapture_AllFrames(capture: *mut JS::StackCapture) {
    (*capture).0 = [0, 0, 0, 0];
}

#[super::poison::engine_shim(failure = unsafe { (*capture).0 = [max as u64, 0, 0, 1] })]
pub unsafe extern "C" fn glue__JS_StackCapture_MaxFrames(max: u32, capture: *mut JS::StackCapture) {
    (*capture).0 = [max as u64, 0, 0, 1];
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CaptureCurrentStack(
    cx: *mut JSContext,
    stackp: JS::MutableHandleObject,
    capture: *mut JS::StackCapture,
    startAfter: JS::HandleObject,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let _scope = HandleScopeGuard::new(bs);
    let _ = startAfter; // not supported: always capture the whole current stack
    let max = stack_capture_max(capture);
    // SpiderMonkey's SavedStacks reports script frames only: a native function being called is not a frame (FrameIter). The
    // limit counts the frames that are kept, so the engine is asked for all of them.
    let frames = match embed::capture_stack(bs, 0) {
        Ok(mut frames) => {
            frames.retain(|f| !f.is_native);
            if max != 0 {
                frames.truncate(max);
            }
            frames
        },
        Err(e) => return unsafe { fail(cx, e.into()) },
    };
    if frames.is_empty() {
        // SpiderMonkey returns a null stack for an empty stack.
        unsafe { *stackp.ptr = ::std::ptr::null_mut() };
        return true;
    }

    // The frames are innermost-first; each frame's parent is the one behind it.
    let mut parent: Option<Handle<api::ObjectValue>> = None;
    for frame in frames.iter().rev() {
        let obj = match unsafe { new_frame(bs, frame, parent) } {
            Ok(obj) => obj,
            Err(e) => return unsafe { fail(cx, e) },
        };
        parent = Some(obj);
    }
    unsafe { *stackp.ptr = (*parent.unwrap()).as_ptr().cast() };
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__IsSavedFrame(obj: *mut JSObject) -> bool {
    is_saved_frame(obj)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__BuildStackString(
    cx: *mut JSContext,
    principals: *mut JSPrincipals,
    stack: JS::HandleObject,
    stringp: JS::MutableHandleString,
    indent: usize,
    stackFormat: js::StackFormat,
) -> bool {
    let _ = (principals, stackFormat); // the SpiderMonkey format is the only one we support
    let bs = unsafe { cx_of(cx) }.bs;
    let _scope = HandleScopeGuard::new(bs);
    let prefix = " ".repeat(indent);

    let mut text = String::new();
    let mut frame = unsafe { *stack.ptr };
    while !frame.is_null() {
        if !is_saved_frame(frame) {
            let realm = embed::current_realm(bs);
            let e = match api::new_type_error(bs, realm, "not a saved frame stack") {
                Ok(e) => e,
                Err(e) => return unsafe { fail(cx, e) },
            };
            return unsafe { fail(cx, embed::throw_value(e.as_value())) };
        }
        let obj = object_ptr(frame);
        let line = embed::reserved_slot(obj, LINE_SLOT).as_smi();
        let column = embed::reserved_slot(obj, COLUMN_SLOT).as_smi();
        let name = unsafe { frame_string(bs, frame, NAME_SLOT) }.unwrap_or_default();
        if line < 0 || column < 0 {
            text.push_str(&format!("{prefix}{name}@[no location info]\n"));
        } else {
            let source = unsafe { frame_string(bs, frame, SOURCE_SLOT) }
                .unwrap_or_else(|| "[no location info]".into());
            text.push_str(&format!("{prefix}{name}@{source}:{line}:{column}\n"));
        }
        let parent = embed::reserved_slot(obj, PARENT_SLOT);
        frame = if parent.is_object() {
            parent.as_object().as_ptr().cast()
        } else {
            ::std::ptr::null_mut()
        };
    }

    let string = match api::string_from_latin1(bs, text.as_bytes()) {
        Ok(string) => string,
        Err(e) => return unsafe { fail(cx, e) },
    };
    unsafe { *stringp.ptr = (*string).as_ptr().cast() };
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetSavedFrameSource(
    _cx: *mut JSContext,
    _principals: *mut JSPrincipals,
    savedFrame: JS::Handle<*mut JSObject>,
    sourcep: JS::MutableHandle<*mut JSString>,
    _selfHosted: JS::SavedFrameSelfHosted,
) -> JS::SavedFrameResult {
    let frame = unsafe { *savedFrame.ptr };
    if !is_saved_frame(frame) {
        return JS::SavedFrameResult::AccessDenied;
    }
    unsafe { frame_string_out(frame, SOURCE_SLOT, sourcep) };
    JS::SavedFrameResult::Ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetSavedFrameSourceId(
    _cx: *mut JSContext,
    _principals: *mut JSPrincipals,
    savedFrame: JS::Handle<*mut JSObject>,
    sourceIdp: *mut u32,
    _selfHosted: JS::SavedFrameSelfHosted,
) -> JS::SavedFrameResult {
    let frame = unsafe { *savedFrame.ptr };
    if !is_saved_frame(frame) {
        return JS::SavedFrameResult::AccessDenied;
    }
    unsafe { *sourceIdp = 0 };
    JS::SavedFrameResult::Ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetSavedFrameLine(
    _cx: *mut JSContext,
    _principals: *mut JSPrincipals,
    savedFrame: JS::Handle<*mut JSObject>,
    linep: *mut u32,
    _selfHosted: JS::SavedFrameSelfHosted,
) -> JS::SavedFrameResult {
    let frame = unsafe { *savedFrame.ptr };
    if !is_saved_frame(frame) {
        return JS::SavedFrameResult::AccessDenied;
    }
    let line = embed::reserved_slot(object_ptr(frame), LINE_SLOT).as_smi();
    unsafe { *linep = line.max(0) as u32 };
    JS::SavedFrameResult::Ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetSavedFrameColumn(
    _cx: *mut JSContext,
    _principals: *mut JSPrincipals,
    savedFrame: JS::Handle<*mut JSObject>,
    columnp: *mut JS::TaggedColumnNumberOneOrigin,
    _selfHosted: JS::SavedFrameSelfHosted,
) -> JS::SavedFrameResult {
    let frame = unsafe { *savedFrame.ptr };
    if !is_saved_frame(frame) {
        return JS::SavedFrameResult::AccessDenied;
    }
    let column = embed::reserved_slot(object_ptr(frame), COLUMN_SLOT).as_smi();
    unsafe { *columnp = JS::TaggedColumnNumberOneOrigin { value_: column.max(0) as u32 } };
    JS::SavedFrameResult::Ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetSavedFrameFunctionDisplayName(
    _cx: *mut JSContext,
    _principals: *mut JSPrincipals,
    savedFrame: JS::Handle<*mut JSObject>,
    namep: JS::MutableHandle<*mut JSString>,
    _selfHosted: JS::SavedFrameSelfHosted,
) -> JS::SavedFrameResult {
    let frame = unsafe { *savedFrame.ptr };
    if !is_saved_frame(frame) {
        return JS::SavedFrameResult::AccessDenied;
    }
    unsafe { frame_string_out(frame, NAME_SLOT, namep) };
    JS::SavedFrameResult::Ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetSavedFrameAsyncCause(
    _cx: *mut JSContext,
    _principals: *mut JSPrincipals,
    savedFrame: JS::Handle<*mut JSObject>,
    asyncCausep: JS::MutableHandle<*mut JSString>,
    _selfHosted: JS::SavedFrameSelfHosted,
) -> JS::SavedFrameResult {
    // The fork has no async stacks.
    let frame = unsafe { *savedFrame.ptr };
    if !is_saved_frame(frame) {
        return JS::SavedFrameResult::AccessDenied;
    }
    unsafe { *asyncCausep.ptr = ::std::ptr::null_mut() };
    JS::SavedFrameResult::Ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetSavedFrameAsyncParent(
    _cx: *mut JSContext,
    _principals: *mut JSPrincipals,
    savedFrame: JS::Handle<*mut JSObject>,
    asyncParentp: JS::MutableHandle<*mut JSObject>,
    _selfHosted: JS::SavedFrameSelfHosted,
) -> JS::SavedFrameResult {
    // The fork has no async stacks.
    let frame = unsafe { *savedFrame.ptr };
    if !is_saved_frame(frame) {
        return JS::SavedFrameResult::AccessDenied;
    }
    unsafe { *asyncParentp.ptr = ::std::ptr::null_mut() };
    JS::SavedFrameResult::Ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetSavedFrameParent(
    _cx: *mut JSContext,
    _principals: *mut JSPrincipals,
    savedFrame: JS::Handle<*mut JSObject>,
    parentp: JS::MutableHandle<*mut JSObject>,
    _selfHosted: JS::SavedFrameSelfHosted,
) -> JS::SavedFrameResult {
    let frame = unsafe { *savedFrame.ptr };
    if !is_saved_frame(frame) {
        return JS::SavedFrameResult::AccessDenied;
    }
    let parent = embed::reserved_slot(object_ptr(frame), PARENT_SLOT);
    let parent = if parent.is_object() {
        parent.as_object().as_ptr().cast()
    } else {
        ::std::ptr::null_mut()
    };
    unsafe { *parentp.ptr = parent };
    JS::SavedFrameResult::Ok
}
