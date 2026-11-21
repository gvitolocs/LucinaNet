/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The SpiderMonkey C++ API on Brimstone (being written; see docs/brimstone-backend.md).

pub mod api;
pub mod bindings;
pub mod compartment;
pub mod errors;
pub mod cx;
pub mod func;
pub mod iter;
pub mod jobs;
pub mod object;
pub mod property;
pub mod proxy;
pub mod function;
pub mod typed;
pub mod gc;
pub mod store_buffer_rules;
pub mod realm;
pub mod runtime;
pub mod script;
pub mod dump_failed;
pub mod security;
pub mod string;
pub mod util;

use crate::jsapi::{JSClass, JSContext, JSErrorFormatString, JSObject, JS};
use std::io::Write;
use std::os::raw::{c_char, c_uint, c_void};

// ---- LUCINANET_JS_MISSING_LOG=<file>: the run-time site log of docs/missing.md section 3 ----
// One tab-separated line per event, so a run over the site list says what pages actually reach instead
// of stopping at the first abort. `<unix_ms>\tmissing\t<name>` for a stub, and
// `<unix_ms>\terror\t<message>\t<file>:<line>` for an uncaught script error, at most ERROR_LOG_CAP of
// the latter per process. The target is read once, so the unset case costs one atomic load.

/// Where the lines go, from `LUCINANET_JS_MISSING_LOG`, read once per process. Unset or empty is off.
fn missing_log_target() -> Option<&'static std::path::Path> {
    static TARGET: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    TARGET
        .get_or_init(|| ::std::env::var_os("LUCINANET_JS_MISSING_LOG").filter(|v| !v.is_empty()).map(std::path::PathBuf::from))
        .as_deref()
}

fn unix_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// Appends `<unix_ms>\t<kind>\t<text>`, plus `\t<at>` when `at` is not empty. A log that cannot be
/// opened or written leaves the run untouched: it is evidence, not behaviour.
fn missing_log_line(kind: &str, text: &str, at: &str) {
    let Some(path) = missing_log_target() else { return };
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else { return };
    let _ = if at.is_empty() { writeln!(file, "{}\t{kind}\t{}", unix_ms(), text) } else { writeln!(file, "{}\t{kind}\t{}\t{}", unix_ms(), text, at) };
    let _ = file.flush();
}

/// The `<unix_ms>\tmissing\t<name>` line of the stub `name`. `missing()` writes it just before it aborts;
/// it is a function of its own because `missing()` never returns.
pub fn record_missing(name: &str) {
    missing_log_line("missing", name, "");
}

/// The `<unix_ms>\tpoison\t<shim>: <message>\t<location>` line of a poisoned context (`poison.rs`): only the panic's
/// static part and its `file:line`, never page data.
pub fn record_poison(shim: &str, message: &str, location: &str) {
    missing_log_line("poison", &format!("{shim}: {message}"), location);
}

/// Uncaught script errors logged per process: a page that throws in a frame loop would otherwise grow
/// the file without bound.
const ERROR_LOG_CAP: usize = 500;

/// The line for an uncaught script error the glue reports to the embedder. `message` is what mozjs sees,
/// `Name: text` for an Error and SpiderMonkey's `uncaught exception: <value>` for anything else, so the
/// same text the site's stderr console line carries and the two channels group into one row. A value
/// thrown from nowhere has no frame, and so no location field.
pub fn record_error(message: &str, file: &str, line: u32) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static ERRORS: AtomicUsize = AtomicUsize::new(0);
    if missing_log_target().is_none() || ERRORS.fetch_add(1, Ordering::Relaxed) >= ERROR_LOG_CAP {
        return;
    }
    missing_log_line("error", message, &if file.is_empty() { String::new() } else { format!("{file}:{line}") });
}

/// Every C++ function nobody has written yet ends up here, so a run names what to do next.
#[cold]
pub fn missing(name: &str) -> ! {
    eprintln!("mozjs_sys (Rust): {name} is not implemented");
    record_missing(name);
    std::process::abort()
}

/// What mozjs error.rs needs from JS_ReportErrorNumberUTF8.
pub unsafe fn report_error_number(
    cx: *mut JSContext,
    callback: unsafe extern "C" fn(*mut c_void, c_uint) -> *const JSErrorFormatString,
    number: c_uint,
    message: *const c_char,
) {
    use brimstone_core::runtime::embed::{self, api};
    let format = unsafe { callback(::std::ptr::null_mut(), number) };
    let arg = unsafe { util::c_str(message) };
    // SpiderMonkey substitutes `{N}` with argument N; mozjs only uses "{0}".
    let text = if format.is_null() { arg.into_owned() } else { unsafe { util::c_str((*format).format) }.replace("{0}", &arg) };
    let kind = if format.is_null() { crate::jsapi::JSExnType::JSEXN_ERR as i16 } else { unsafe { (*format).exnType } };
    let bs = unsafe { cx::cx_of(cx) }.bs;
    let scope = brimstone_core::runtime::gc::HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    use crate::jsapi::JSExnType as E;
    let made = if kind == E::JSEXN_TYPEERR as i16 {
        api::new_type_error(bs, realm, &text)
    } else if kind == E::JSEXN_RANGEERR as i16 {
        api::new_range_error(bs, realm, &text)
    } else if kind == E::JSEXN_SYNTAXERR as i16 {
        api::new_syntax_error(bs, realm, &text)
    } else {
        api::new_error(bs, realm, &text)
    };
    match made {
        Ok(e) => unsafe { cx::cx_of(cx) }.set_pending(e.as_value().as_raw_bits()),
        Err(e) => unsafe {
            util::fail(cx, e);
        },
    }
    scope.exit();
}

/// `mozjs::rust::get_object_class`, which reads SpiderMonkey's object layout directly.
#[inline]
pub unsafe fn object_class(obj: *mut JSObject) -> *const JSClass {
    unsafe { object::class_of(obj) }
}

/// `mozjs::rust::get_object_realm`, for the same reason.
#[inline]
pub unsafe fn object_realm(obj: *mut JSObject) -> *mut JS::Realm {
    unsafe { realm::JS__GetObjectRealmOrNull(obj) }
}

/// The kind of heap item behind a pointer value or id (Brimstone keeps it in the item's shape, not in the bits).
#[inline]
pub fn trace_kind_of(bits: u64) -> JS::TraceKind {
    debug_assert!(crate::boundary::value_bits::is_traced(bits));
    let v = brimstone_core::runtime::Value::from_raw_bits(bits);
    if v.is_object() {
        JS::TraceKind::Object
    } else if v.is_string() {
        JS::TraceKind::String
    } else if v.is_symbol() {
        JS::TraceKind::Symbol
    } else if v.is_bigint() {
        JS::TraceKind::BigInt
    } else {
        // Bytecode, scopes, shapes...: SpiderMonkey's scripts and shapes are GC things too, never values Servo inspects.
        JS::TraceKind::Script
    }
}

/// SpiderMonkey's uncatchable exception (a native returned false with nothing pending): no catch or finally runs.
pub fn uncatchable(_bs: brimstone_core::runtime::Context) -> brimstone_core::runtime::eval_result::EvalError {
    brimstone_core::runtime::embed::uncatchable()
}
pub mod promise;
pub mod clone;
pub mod values;
pub mod module;
pub mod profile;
pub mod lazy_trim;
pub mod cache_partition;
pub mod saved_frame;
pub mod env_chain;
pub mod caller_private;

pub mod crash_bt;
pub mod poison;
