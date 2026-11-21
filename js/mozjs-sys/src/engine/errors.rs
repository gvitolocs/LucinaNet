/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Error reports: what Servo reads when a script throws (`components/script/dom/bindings/error.rs`, mozjs
//! `error_info_from_exception_stack`). The original is mozjs_sys's `jsglue.cpp` (`PendingExceptionStackInfo`), `jsapi.cpp`
//! (`JS_ErrorFromException`) and `js/src/vm/ErrorReporting.cpp` (`ErrorReportBuilder`: the message of an Error is
//! "Name: message"; a thrown non-Error value is reported as "uncaught exception: <value>").
//!
//! An Error's file, line and column are those of the newest frame captured when it was created (the fork's
//! `error_stack_frames`), as SpiderMonkey records them at construction.

use ::std::ffi::{c_void, CString};

use brimstone_core::runtime::embed::{self, api};
use brimstone_core::runtime::gc::HandleScope;
use brimstone_core::runtime::{Context as Bs, Handle};

use super::cx::cx_of;
use super::util::handle_of;
use crate::jsapi::*;

/// What a report says about an exception value.
pub(super) struct Described {
    pub(super) message: String,
    pub(super) file: String,
    pub(super) line: u32,
    pub(super) column: u32,
    pub(super) exn_type: i16,
}

fn string_of(bs: Bs, v: Handle<brimstone_core::runtime::Value>) -> String {
    match api::to_string(bs, v) {
        Ok(s) => api::string_to_rust_string(s).unwrap_or_default(),
        Err(_) => String::from("<unprintable>"),
    }
}

/// SpiderMonkey's `JSExnType` from an Error's `name` (TypeError, RangeError...); plain Error otherwise.
fn exn_type(name: &str) -> i16 {
    use crate::jsapi::JSExnType as E;
    (match name {
        "InternalError" => E::JSEXN_INTERNALERR,
        "AggregateError" => E::JSEXN_AGGREGATEERR,
        "EvalError" => E::JSEXN_EVALERR,
        "RangeError" => E::JSEXN_RANGEERR,
        "ReferenceError" => E::JSEXN_REFERENCEERR,
        "SyntaxError" => E::JSEXN_SYNTAXERR,
        "TypeError" => E::JSEXN_TYPEERR,
        "URIError" => E::JSEXN_URIERR,
        _ => E::JSEXN_ERR,
    }) as i16
}

/// Describes `bits` as ErrorReportBuilder would. `None` for an Error-less value only when `errors_only`.
/// `dump_failed` reads it to tell a SyntaxError from any other failure.
pub(super) fn describe(bs: Bs, bits: u64, errors_only: bool) -> Option<Described> {
    let v = handle_of(bs, bits);
    if v.is_object() {
        let o = v.as_object();
        if let Ok(Some(frames)) = embed::error_stack_frames(bs, o, 1) {
            let key = api::property_key_from_string(bs, "name").ok()?;
            let name = api::get(bs, o, key).map(|n| if n.is_undefined() { "Error".into() } else { string_of(bs, n) }).unwrap_or_default();
            let msg = match api::error_message(bs, o) {
                Ok(Some(m)) => api::string_to_rust_string(m).unwrap_or_default(),
                _ => String::new(),
            };
            let message = if msg.is_empty() { name.clone() } else { format!("{name}: {msg}") };
            let top = frames.into_iter().next();
            return Some(Described {
                message,
                file: top.as_ref().and_then(|f| f.file_name.clone()).unwrap_or_default(),
                line: top.as_ref().and_then(|f| f.line).unwrap_or(0),
                column: top.as_ref().and_then(|f| f.column).unwrap_or(0),
                exn_type: exn_type(&name),
            });
        }
    }
    if errors_only {
        return None;
    }
    Some(Described {
        message: format!("uncaught exception: {}", string_of(bs, v)),
        file: String::new(),
        line: 0,
        column: 0,
        exn_type: crate::jsapi::JSExnType::JSEXN_ERR as i16,
    })
}

/// Takes the pending exception and reports it through `callback` (message, then file name), the line and column, and the
/// value itself in `dest`, as jsglue.cpp's `PendingExceptionStackInfo`. False when nothing is pending. This is the one path on
/// which the glue hands an uncaught script error to the embedder, so it is also where `LUCINANET_JS_MISSING_LOG` records it.
#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__PendingExceptionStackInfo(
    cx: *mut JSContext,
    callback: crate::glue::StringCallback,
    message_target: *mut c_void,
    filename_target: *mut c_void,
    line: *mut u32,
    col: *mut u32,
    dest: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let Some(bits) = unsafe { cx_of(cx) }.take_pending() else { return false };
    let scope = HandleScope::enter(bs);
    let d = describe(bs, bits, false).expect("non-Error values are described");
    super::record_error(&d.message, &d.file, d.line);
    if let Some(cb) = callback {
        unsafe { cb(d.message.as_ptr().cast(), d.message.len(), message_target) };
        if !d.file.is_empty() {
            unsafe { cb(d.file.as_ptr().cast(), d.file.len(), filename_target) };
        }
    }
    unsafe {
        *line = d.line;
        *col = d.column;
        (*dest.ptr).asBits_ = bits;
    }
    scope.exit();
    true
}

/// The report an Error object carries, as `JS_ErrorFromException`: false for anything that is not an Error. The report lives in
/// the context until the next call (Servo reads it inside `borrowed_error_report`'s closure).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ErrorFromException(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    report: *mut JS::BorrowedErrorReport,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let o = unsafe { *obj.ptr };
    let scope = HandleScope::enter(bs);
    let d = describe(bs, o as usize as u64, true);
    scope.exit();
    let Some(d) = d else { return false };
    let mut held = Box::new(HeldReport {
        report: unsafe { ::std::mem::zeroed() },
        message: CString::new(d.message.replace('\0', " ")).unwrap_or_default(),
        file: CString::new(d.file.replace('\0', " ")).unwrap_or_default(),
    });
    held.report._base.message_.data_ = held.message.as_ptr();
    held.report._base.filename.data_ = held.file.as_ptr();
    held.report._base.lineno = d.line;
    held.report._base.column._base = d.column;
    held.report.exnType = d.exn_type;
    unsafe {
        (*report).report_ = &mut held.report;
        (*report).owner_.data = o;
    }
    unsafe { cx_of(cx) }.last_error_report = Some(held);
    true
}

/// A `JSErrorReport` with the strings it points at.
pub struct HeldReport {
    report: JSErrorReport,
    message: CString,
    file: CString,
}

/// SpiderMonkey reports an out-of-memory as an uncatchable "out of memory" error; here the script stops uncatchably.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ReportOutOfMemory(cx: *mut JSContext) {
    unsafe { cx_of(cx) }.exception = None;
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ReportOutOfMemory(cx: *mut JSContext) {
    unsafe { JS_ReportOutOfMemory(cx) }
}

/// A TypeError "<value> is not a function", as `js::ReportIsNotFunction`. Always false.
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ReportIsNotFunction(cx: *mut JSContext, v: JS::HandleValue) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let what = string_of(bs, handle_of(bs, unsafe { (*v.ptr).asBits_ }));
    let realm = embed::current_realm(bs);
    match api::new_type_error(bs, realm, &format!("{what} is not a function")) {
        Ok(e) => unsafe { cx_of(cx) }.set_pending(e.as_value().as_raw_bits()),
        Err(e) => unsafe {
            super::util::fail(cx, e);
        },
    }
    scope.exit();
    false
}

/// Servo's cross-origin refusals: "Permission denied to access object" as SpiderMonkey's JSMSG_ACCESS_DENIED (an Error).
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ReportAccessDenied(cx: *mut JSContext) {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    match api::new_error(bs, realm, "Permission denied to access object") {
        Ok(e) => unsafe { cx_of(cx) }.set_pending(e.as_value().as_raw_bits()),
        Err(e) => unsafe {
            super::util::fail(cx, e);
        },
    }
    scope.exit();
}

/// A plain Error with `message` as the pending exception (mozjs glue `ReportErrorUTF8`/`ASCII`).
#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__ReportErrorUTF8(cx: *mut JSContext, message: *const ::std::os::raw::c_char) {
    let bs = unsafe { cx_of(cx) }.bs;
    let text = unsafe { super::util::c_str(message) };
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    match api::new_error(bs, realm, &text) {
        Ok(e) => unsafe { cx_of(cx) }.set_pending(e.as_value().as_raw_bits()),
        Err(e) => unsafe {
            super::util::fail(cx, e);
        },
    }
    scope.exit();
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__ReportErrorASCII(cx: *mut JSContext, message: *const ::std::os::raw::c_char) {
    unsafe { glue__ReportErrorUTF8(cx, message) }
}

/// `JS::ReportUncatchableException`: nothing pending, the caller returns false and the script stops.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ReportUncatchableException(cx: *mut JSContext) {
    unsafe { cx_of(cx) }.exception = None;
}
