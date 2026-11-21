/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Compiling and running scripts. The original is `js/src/vm/CompilationAndEvaluation.cpp` and mozjs_sys's `jsglue.cpp`
//! (`NewCompileOptions`, `DeleteCompileOptions`).

use brimstone_core::runtime::embed;
use brimstone_core::runtime::gc::HandleScope;

use super::cx::cx_of;
use super::util::{c_str, finish};
use crate::jsapi::*;

/// What `NewCompileOptions` hands out: SpiderMonkey's options struct first (mozjs writes its fields), then what keeps the file
/// name alive.
#[repr(C)]
struct OwnedOptions {
    base: JS::ReadOnlyCompileOptions,
    file: ::std::ffi::CString,
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__NewCompileOptions(
    _cx: *mut JSContext,
    file: *const ::std::os::raw::c_char,
    line: ::std::os::raw::c_uint,
) -> *mut JS::ReadOnlyCompileOptions {
    let file = if file.is_null() { ::std::ffi::CString::default() } else { unsafe { ::std::ffi::CStr::from_ptr(file) }.to_owned() };
    let mut o = Box::new(OwnedOptions { base: unsafe { ::std::mem::zeroed() }, file });
    o.base._base.filename_.data_ = o.file.as_ptr();
    o.base.lineno = line;
    Box::into_raw(o).cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__DeleteCompileOptions(opts: *mut JS::ReadOnlyCompileOptions) {
    if !opts.is_null() {
        unsafe { drop(Box::from_raw(opts.cast::<OwnedOptions>())) };
    }
}

// ---- LUCINANET_JS_SCRIPT_TIMES=1: one stderr line per executed top-level script ----
// `jsscript <file tail> bytes=N parse=P gen=G exec=E ms, lazy +L ms/n during exec, lazy total T ms/m`: parse is parsing plus
// scope analysis (off the script thread for scripts Servo parsed there), gen is bytecode generation of the top level and its
// eager functions, exec the top-level run, and lazy the generation of functions on their first call during that run.

fn script_times_on() -> bool {
    static ON: ::std::sync::OnceLock<bool> = ::std::sync::OnceLock::new();
    *ON.get_or_init(|| ::std::env::var("LUCINANET_JS_SCRIPT_TIMES").is_ok_and(|v| v == "1"))
}

struct CompiledInfo {
    script: usize,
    tail: String,
    bytes: usize,
    times: embed::CompileTimes,
}

thread_local! {
    /// The last script compiled on this thread, matched by address when it is executed (Servo runs a script right after
    /// compiling it; a GC in between moves it, and the line then says so).
    static LAST_COMPILED: ::std::cell::RefCell<Option<CompiledInfo>> = const { ::std::cell::RefCell::new(None) };
}

fn file_tail(file: &str) -> String {
    let tail = file.split(['?', '#']).next().unwrap_or(file).rsplit('/').next().unwrap_or(file);
    let tail = if tail.is_empty() { file } else { tail };
    tail.chars().take(80).collect()
}

fn ms(d: ::std::time::Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn note_compiled(script: *mut JSScript, file: &str, bytes: usize) {
    if !script_times_on() || script.is_null() {
        return;
    }
    let info = CompiledInfo { script: script as usize, tail: file_tail(file), bytes, times: embed::last_compile_times() };
    let _ = LAST_COMPILED.try_with(|last| *last.borrow_mut() = Some(info));
}

fn report_script(tail: &str, bytes: usize, times: Option<embed::CompileTimes>, exec: ::std::time::Duration, lazy_before: embed::LazyGenerateTime) {
    let lazy = embed::lazy_generate_time();
    let (parse, generate) = match times {
        Some(t) => (format!("{:.1}", ms(t.parse)), format!("{:.1}", ms(t.generate))),
        None => ("?".to_owned(), "?".to_owned()),
    };
    eprintln!(
        "jsscript {tail} bytes={bytes} parse={parse} gen={generate} exec={:.1} ms, lazy +{:.1} ms/{} during exec, lazy total {:.1} ms/{}",
        ms(exec),
        ms(lazy.total.saturating_sub(lazy_before.total)),
        lazy.functions - lazy_before.functions,
        ms(lazy.total),
        lazy.functions,
    );
}

pub(super) unsafe fn file_and_line<'a>(options: *const JS::ReadOnlyCompileOptions) -> (::std::borrow::Cow<'a, str>, u32) {
    if options.is_null() {
        return (::std::borrow::Cow::Borrowed(""), 1);
    }
    let o = unsafe { &*options };
    (unsafe { c_str(o._base.filename_.data_) }, o.lineno.max(1))
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__Evaluate2(
    cx: *mut JSContext,
    options: *const JS::ReadOnlyCompileOptions,
    src: *mut JS::SourceText<mozilla::Utf8Unit>,
    rval: JS::MutableHandle<JS::Value>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let bytes = unsafe { ::std::slice::from_raw_parts((*src).units_.cast::<u8>(), (*src).length_ as usize) };
    let text = String::from_utf8_lossy(bytes);
    let at = unsafe { super::dump_failed::at(options) };
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let r = timed_evaluate(bs, realm, &text, &at);
    let ok = unsafe { finish(cx, r, rval.ptr) };
    scope.exit();
    ok
}

/// The two-byte form, which mozjs uses for scripts given as UTF-16.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__Evaluate(
    cx: *mut JSContext,
    options: *const JS::ReadOnlyCompileOptions,
    src: *mut JS::SourceText<u16>,
    rval: JS::MutableHandle<JS::Value>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let units = unsafe { ::std::slice::from_raw_parts((*src).units_, (*src).length_ as usize) };
    let text = String::from_utf16_lossy(units);
    let at = unsafe { super::dump_failed::at(options) };
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let r = timed_evaluate(bs, realm, &text, &at);
    let ok = unsafe { finish(cx, r, rval.ptr) };
    scope.exit();
    ok
}

/// `embed::evaluate_script`: compiled and run in one step, unless the script timing (which measures the two apart) or
/// `LUCINANET_JS_DUMP_FAILED` (which reports which of the two failed) is on.
fn timed_evaluate(
    bs: brimstone_core::runtime::Context,
    realm: brimstone_core::runtime::Handle<brimstone_core::runtime::Realm>,
    text: &str,
    at: &super::dump_failed::Where,
) -> brimstone_core::runtime::EvalResult<brimstone_core::runtime::Handle<brimstone_core::runtime::Value>> {
    let _running = unsafe { super::caller_private::running_eval(&at.file) };
    if !script_times_on() && !super::dump_failed::enabled() {
        return embed::evaluate_script(bs, realm, text, &at.file, at.line);
    }
    let script = match embed::compile_script(bs, realm, text, &at.file, at.line) {
        Ok(s) => s,
        Err(e) => {
            unsafe { super::dump_failed::report(bs, text, at, super::dump_failed::Phase::Compile, e) };
            return Err(e);
        },
    };
    let times = embed::last_compile_times();
    let lazy_before = embed::lazy_generate_time();
    let start = ::std::time::Instant::now();
    let r = embed::execute_compiled(bs, script);
    if let Err(e) = &r {
        unsafe { super::dump_failed::report(bs, text, at, super::dump_failed::Phase::Run, *e) };
    }
    if script_times_on() {
        report_script(&file_tail(&at.file), text.len(), Some(times), start.elapsed(), lazy_before);
    }
    r
}

// ---- Compiled scripts (JS::Compile, JS_ExecuteScript) ----
// `JSScript*` is the fork's `CompiledScript` heap item. It moves with the GC like any engine object; Servo keeps scripts in
// traced `Heap<*mut JSScript>` slots, so that is safe.

unsafe fn compile(cx: *mut JSContext, options: *const JS::ReadOnlyCompileOptions, text: &str) -> *mut JSScript {
    let bs = unsafe { cx_of(cx) }.bs;
    let at = unsafe { super::dump_failed::at(options) };
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let out = match embed::compile_script(bs, realm, text, &at.file, at.line) {
        Ok(s) => (*s).as_ptr().cast(),
        Err(e) => {
            unsafe { super::dump_failed::report(bs, text, &at, super::dump_failed::Phase::Compile, e) };
            unsafe { super::util::fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    unsafe { super::dump_failed::record_compiled(out, text, &at) };
    note_compiled(out, &at.file, text.len());
    unsafe { super::caller_private::note_script(out, &at.file) };
    out
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__Compile1(
    cx: *mut JSContext,
    options: *const JS::ReadOnlyCompileOptions,
    src: *mut JS::SourceText<mozilla::Utf8Unit>,
) -> *mut JSScript {
    let bytes = unsafe { ::std::slice::from_raw_parts((*src).units_.cast::<u8>(), (*src).length_ as usize) };
    let text = String::from_utf8_lossy(bytes);
    unsafe { compile(cx, options, &text) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__Compile(
    cx: *mut JSContext,
    options: *const JS::ReadOnlyCompileOptions,
    src: *mut JS::SourceText<u16>,
) -> *mut JSScript {
    let units = unsafe { ::std::slice::from_raw_parts((*src).units_, (*src).length_ as usize) };
    let text = String::from_utf16_lossy(units);
    unsafe { compile(cx, options, &text) }
}

unsafe fn execute(cx: *mut JSContext, script: *mut JSScript, rval: *mut JS::Value) -> bool {
    let _running = unsafe { super::caller_private::running(script) };
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let s = brimstone_core::runtime::gc::HeapPtr::<embed::CompiledScript>::from_ptr(script.cast()).to_handle();
    if !script_times_on() {
        let r = embed::execute_compiled(bs, s);
        if let Err(e) = &r {
            unsafe { super::dump_failed::report_script(bs, script, super::dump_failed::Phase::Run, *e) };
        }
        let ok = unsafe { finish(cx, r, rval) };
        scope.exit();
        return ok;
    }
    let info = LAST_COMPILED.try_with(|last| last.borrow_mut().take()).ok().flatten();
    let lazy_before = embed::lazy_generate_time();
    let start = ::std::time::Instant::now();
    let r = embed::execute_compiled(bs, s);
    let exec = start.elapsed();
    if let Err(e) = &r {
        unsafe { super::dump_failed::report_script(bs, script, super::dump_failed::Phase::Run, *e) };
    }
    match info.filter(|i| i.script == script as usize) {
        Some(i) => report_script(&i.tail, i.bytes, Some(i.times), exec, lazy_before),
        None => report_script("(script compiled earlier)", 0, None, exec, lazy_before),
    }
    let ok = unsafe { finish(cx, r, rval) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ExecuteScript(
    cx: *mut JSContext,
    script: JS::Handle<*mut JSScript>,
    rval: JS::MutableHandle<JS::Value>,
) -> bool {
    unsafe { execute(cx, *script.ptr, rval.ptr) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ExecuteScript1(cx: *mut JSContext, script: JS::Handle<*mut JSScript>) -> bool {
    unsafe { execute(cx, *script.ptr, ::std::ptr::null_mut()) }
}

/// `JS::SetScriptPrivate`: kept in a table on the context whose entries are traced (both the script, which moves, and the
/// value), since the fork's compiled scripts have no private slot yet. Entries live as long as the context.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetScriptPrivate(script: *mut JSScript, value: *const JS::Value) {
    let c = unsafe { super::cx::current() };
    let key = script as usize as u64;
    let v = super::func::non_zero(unsafe { (*value).asBits_ });
    if let Some(e) = c.script_privates.iter_mut().find(|e| e.0 == key) {
        e.1 = v;
    } else {
        c.script_privates.push((key, v));
    }
}

#[super::poison::engine_shim(failure = unsafe { (*dest.ptr).asBits_ = crate::boundary::value_bits::undefined() })]
pub unsafe extern "C" fn glue__JS_GetScriptPrivate(script: *mut JSScript, dest: JS::MutableHandleValue) {
    let c = unsafe { super::cx::current() };
    let key = script as usize as u64;
    let v = c.script_privates.iter().find(|e| e.0 == key).map_or(crate::boundary::value_bits::undefined(), |e| e.1);
    unsafe { (*dest.ptr).asBits_ = v };
}

// ---- Stencils compiled off the main thread (mozjs offthread, Servo's script_execution.rs) ----
// A stencil is the source checked by the fork's `parse_script_off_thread` (any thread); instantiating compiles it on the
// context's thread from that parse (`compile_parsed_script`).

/// `JS::FrontendContext`: where an off-thread compile leaves its syntax error.
#[derive(Default)]
pub struct FrontendContext {
    error: Option<FrontendError>,
}

/// An off-thread parse error. `text` is the source it was parsing, kept only while `LUCINANET_JS_DUMP_FAILED` is on: the
/// dump wants the source, and this is the one compile whose error reaches the context as a message rather than a value.
struct FrontendError {
    message: String,
    at: super::dump_failed::Where<'static>,
    text: Option<String>,
}

/// `JS::Stencil`.
pub struct Stencil {
    refcount: usize,
    /// The off-thread parse, used by the first instantiation; later ones compile `source` again.
    parsed: Option<embed::ParsedScript>,
    source: String,
    at: super::dump_failed::Where<'static>,
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewFrontendContext() -> *mut JS::FrontendContext {
    Box::into_raw(Box::<FrontendContext>::default()).cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__DestroyFrontendContext(fc: *mut JS::FrontendContext) {
    if !fc.is_null() {
        unsafe { drop(Box::from_raw(fc.cast::<FrontendContext>())) };
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__HadFrontendErrors(fc: *mut JS::FrontendContext) -> bool {
    unsafe { (*fc.cast::<FrontendContext>()).error.is_some() }
}

/// The off-thread syntax error becomes the context's pending SyntaxError, as SpiderMonkey reports it.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ConvertFrontendErrorsToRuntimeErrors(
    cx: *mut JSContext,
    fc: *mut JS::FrontendContext,
    _options: *const JS::ReadOnlyCompileOptions,
) -> bool {
    let Some(fe) = (unsafe { (*fc.cast::<FrontendContext>()).error.take() }) else { return true };
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let message = format!("{} ({}:{})", fe.message, fe.at.file, fe.at.line);
    if let Some(text) = &fe.text {
        super::dump_failed::report_frontend(text, &fe.at.file, fe.at.line, fe.at.column, &message);
    }
    match brimstone_core::runtime::embed::api::new_syntax_error(bs, realm, &message) {
        Ok(e) => unsafe { cx_of(cx) }.set_pending(e.as_value().as_raw_bits()),
        Err(e) => unsafe {
            super::util::fail(cx, e);
        },
    }
    scope.exit();
    true
}

/// Copies of compile options that outlive the caller's (`OwningCompileOptions`): ours own their file name already.
#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__OwningCompileOptions_for_fc(
    _fc: *mut JS::FrontendContext,
    rhs: *const JS::ReadOnlyCompileOptions,
) -> *mut JS::OwningCompileOptions {
    let (file, line) = unsafe { file_and_line(rhs) };
    let file = ::std::ffi::CString::new(file.into_owned()).unwrap_or_default();
    let mut o = Box::new(OwnedOptions { base: unsafe { *rhs }, file });
    o.base._base.filename_.data_ = o.file.as_ptr();
    o.base.lineno = line;
    Box::into_raw(o).cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__DeleteOwningCompileOptions(opts: *mut JS::OwningCompileOptions) {
    if !opts.is_null() {
        unsafe { drop(Box::from_raw(opts.cast::<OwnedOptions>())) };
    }
}

unsafe fn stencil_from(fc: Option<&mut FrontendContext>, options: *const JS::ReadOnlyCompileOptions, text: String) -> *mut JS::Stencil {
    let at = unsafe { super::dump_failed::at(options) };
    match embed::parse_script_off_thread(&text, &at.file, at.line) {
        Ok(p) => Box::into_raw(Box::new(Stencil { refcount: 1, parsed: Some(p), source: text, at: at.owned() })).cast(),
        Err(e) => {
            if let Some(fc) = fc {
                let text = super::dump_failed::enabled().then(|| text.clone());
                fc.error = Some(FrontendError { message: e.message, at: at.owned(), text });
            }
            ::std::ptr::null_mut()
        },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__CompileGlobalScriptToStencil_C(
    fc: *mut JS::FrontendContext,
    options: *const JS::ReadOnlyCompileOptions,
    src: *mut JS::SourceText<mozilla::Utf8Unit>,
    out: *mut already_AddRefed<JS::Stencil>,
) {
    let bytes = unsafe { ::std::slice::from_raw_parts((*src).units_.cast::<u8>(), (*src).length_ as usize) };
    let text = String::from_utf8_lossy(bytes).into_owned();
    let fc = unsafe { fc.cast::<FrontendContext>().as_mut() };
    unsafe { (*out).mRawPtr = stencil_from(fc, options, text) };
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__PrepareForInstantiate(
    _fc: *mut JS::FrontendContext,
    _stencil: *mut JS::Stencil,
    _storage: *mut JS::InstantiationStorage,
) -> bool {
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__InstantiateGlobalStencil(
    cx: *mut JSContext,
    _options: *const JS::InstantiateOptions,
    stencil: *mut JS::Stencil,
    _storage: *mut JS::InstantiationStorage,
) -> *mut JSScript {
    let s = unsafe { &mut *stencil.cast::<Stencil>() };
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let compiled = match s.parsed.take() {
        Some(p) => embed::compile_parsed_script(bs, realm, p),
        None => embed::compile_script(bs, realm, &s.source, &s.at.file, s.at.line),
    };
    let out = match compiled {
        Ok(sc) => (*sc).as_ptr().cast(),
        Err(e) => {
            unsafe { super::dump_failed::report(bs, &s.source, &s.at, super::dump_failed::Phase::Compile, e) };
            unsafe { super::util::fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    unsafe { super::dump_failed::record_compiled(out, &s.source, &s.at) };
    note_compiled(out, &s.at.file, s.source.len());
    unsafe { super::caller_private::note_script(out, &s.at.file) };
    out
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__StencilRelease(stencil: *mut JS::Stencil) {
    if stencil.is_null() {
        return;
    }
    let s = stencil.cast::<Stencil>();
    unsafe {
        (*s).refcount -= 1;
        if (*s).refcount == 0 {
            drop(Box::from_raw(s));
        }
    }
}
