/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// SpiderMonkey's module API on Brimstone (mozjs-sys/src/engine/module.rs), driven as Servo's module loader drives it
// (components/script/scripting/module_loading.rs): compile, LoadRequestedModules with a Rust load hook answering through
// FinishLoadingImportedModule, link, evaluate, read the namespace. Each test inits the engine, so run them one per process
// (`--exact`), as js-engine/bin/mozjs-tests.sh does.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::ptr::{self, NonNull};

use mozjs::conversions::jsstr_to_string;
use mozjs::gc::Traceable;
use mozjs::glue::JS_GetModulePrivate;
use mozjs::jsapi::JS::{
    self, CompileJsonModule1, CompileModule1, CreateDefaultExportSyntheticModule, FinishLoadingDynamicImportedModule,
    FinishLoadingImportedModule, FinishLoadingImportedModuleFailedWithPendingException, GetModuleForNamespace,
    GetModuleNamespace, GetModuleObject, GetModuleRequestSpecifier, GetModuleRequestType, GetModuleScript,
    GetPromiseState, GetRequestedModuleType, IsPromiseObject, LoadRequestedModules1, ModuleEvaluate,
    ModuleErrorBehaviour, ModuleLink, ModuleType, PromiseState, ResolvePromise, SetModuleLoadHook,
    SetModuleMetadataHook, SetModulePrivate, ThrowOnModuleEvaluationFailure,
};
use mozjs::jsapi::{
    js, Heap, JSContext, JSObject, JSScript, JSTracer, JS_AddExtraGCRootsTracer, JS_ClearPendingException,
    JS_DefineProperty, JS_GetPendingException, JS_GetProperty, JS_GetRuntime, JS_IsExceptionPending,
    OnNewGlobalHookOption, Value, JSPROP_ENUMERATE, JS_GC,
};
use mozjs::jsval::{Int32Value, ObjectValue, PrivateValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{
    evaluate_script, transform_str_to_source_text, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime,
    SIMPLE_GLOBAL_CLASS,
};

/// A load the test finishes later (the hook's arguments, kept traced as Servo keeps its `ImportRequest`).
struct Pending {
    specifier: String,
    referrer: Box<Heap<*mut JSScript>>,
    request: Box<Heap<*mut JSObject>>,
    payload: Box<Heap<Value>>,
}

#[derive(Default)]
struct Loader {
    sources: HashMap<String, String>,
    /// One record per specifier, as Servo's module map keeps them.
    records: Vec<(String, Box<Heap<*mut JSObject>>)>,
    /// Answer from the hook (false) or queue the load (true).
    deferred: bool,
    pending: Vec<Pending>,
    /// Specifiers the hook was asked for, in order.
    asked: Vec<String>,
    meta_calls: usize,
    /// Collect inside the hook before answering, as Servo's fetch path may (it allocates heavily there).
    gc_in_hook: bool,
    /// The records are traced only by the global's class trace hook, as Servo's module map is (Window -> Document).
    records_in_global: bool,
}

thread_local! {
    static LOADER: RefCell<Loader> = RefCell::new(Loader::default());
}

unsafe extern "C" fn trace_loader(trc: *mut JSTracer, _data: *mut c_void) {
    LOADER.with(|l| {
        let l = l.borrow();
        if !l.records_in_global {
            for (_, r) in &l.records {
                unsafe { r.trace(trc) };
            }
        }
        for p in &l.pending {
            unsafe {
                p.referrer.trace(trc);
                p.request.trace(trc);
                p.payload.trace(trc);
            }
        }
    });
}

unsafe fn specifier_of(cx: *mut JSContext, request: JS::HandleObject) -> String {
    let s = unsafe { GetModuleRequestSpecifier(cx, request) };
    let context = unsafe { mozjs::context::JSContext::from_ptr(NonNull::new(cx).unwrap()) };
    unsafe { jsstr_to_string(&context, NonNull::new(s).unwrap()) }
}

/// The record for `specifier`: the cached one, or a new one compiled from the loader's sources (JSON for `type: "json"`).
unsafe fn record_for(cx: *mut JSContext, specifier: &str, ty: ModuleType) -> *mut JSObject {
    if let Some(r) = LOADER.with(|l| l.borrow().records.iter().find(|(s, _)| s == specifier).map(|(_, r)| r.get())) {
        return r;
    }
    let Some(source) = LOADER.with(|l| l.borrow().sources.get(specifier).cloned()) else {
        let msg = format!("no module {specifier}");
        rooted!(in(cx) let s = unsafe { mozjs::jsapi::JS_NewStringCopyN(cx, msg.as_ptr().cast(), msg.len()) });
        rooted!(in(cx) let v = mozjs::jsval::StringValue(unsafe { &*s.get() }));
        unsafe { mozjs::jsapi::JS_SetPendingException(cx, v.handle().into(), mozjs::jsapi::JS::ExceptionStackBehavior::Capture) };
        return ptr::null_mut();
    };
    let file = CString::new(specifier).unwrap();
    let options = unsafe { mozjs::glue::NewCompileOptions(cx, file.as_ptr(), 1) };
    let mut src = transform_str_to_source_text(&source);
    let record = unsafe {
        if ty == ModuleType::JSON {
            CompileJsonModule1(cx, options, &mut src)
        } else {
            CompileModule1(cx, options, &mut src)
        }
    };
    unsafe { mozjs::glue::DeleteCompileOptions(options) };
    if !record.is_null() {
        LOADER.with(|l| l.borrow_mut().records.push((specifier.to_owned(), Heap::boxed(record))));
    }
    record
}

/// Answers a load as Servo's `finish_loading_imported_module` does.
unsafe fn answer(cx: *mut JSContext, referrer: JS::Handle<*mut JSScript>, request: JS::HandleObject, payload: JS::HandleValue) -> bool {
    let specifier = unsafe { specifier_of(cx, request) };
    let ty = unsafe { GetModuleRequestType(cx, request) };
    rooted!(in(cx) let record = unsafe { record_for(cx, &specifier, ty) });
    if record.get().is_null() {
        return unsafe { FinishLoadingImportedModuleFailedWithPendingException(cx, payload) };
    }
    unsafe { FinishLoadingImportedModule(cx, referrer, request, payload, record.handle().into(), true) }
}

unsafe extern "C" fn load_hook(
    cx: *mut JSContext,
    referrer: JS::Handle<*mut JSScript>,
    request: JS::HandleObject,
    _host_defined: JS::HandleValue,
    payload: JS::HandleValue,
    _line: u32,
    _column: JS::ColumnNumberOneOrigin,
) -> bool {
    let specifier = unsafe { specifier_of(cx, request) };
    let deferred = LOADER.with(|l| {
        let mut l = l.borrow_mut();
        l.asked.push(specifier.clone());
        l.deferred
    });
    if deferred {
        let p = Pending {
            specifier,
            referrer: Heap::boxed(unsafe { *referrer.ptr }),
            request: Heap::boxed(unsafe { *request.ptr }),
            payload: Heap::boxed(unsafe { *payload.ptr }),
        };
        LOADER.with(|l| l.borrow_mut().pending.push(p));
        return true;
    }
    if LOADER.with(|l| l.borrow().gc_in_hook) {
        // Servo copies the arguments into traced boxes (`ImportRequest`), then finishes from its fetch callback.
        rooted!(in(cx) let referrer = unsafe { *referrer.ptr });
        rooted!(in(cx) let request = unsafe { *request.ptr });
        rooted!(in(cx) let payload = unsafe { *payload.ptr });
        unsafe { JS_GC(cx, JS::GCReason::API) };
        let ty = unsafe { GetModuleRequestType(cx, request.handle().into()) };
        rooted!(in(cx) let record = unsafe { record_for(cx, &specifier, ty) });
        unsafe { JS_GC(cx, JS::GCReason::API) };
        assert!(!record.get().is_null());
        return unsafe {
            FinishLoadingImportedModule(cx, referrer.handle().into(), request.handle().into(), payload.handle().into(), record.handle().into(), true)
        };
    }
    unsafe { answer(cx, referrer, request, payload) }
}

/// Finishes the first queued load (static loads with FinishLoadingImportedModule).
unsafe fn finish_next(cx: *mut JSContext) -> String {
    let p = LOADER.with(|l| l.borrow_mut().pending.remove(0));
    rooted!(in(cx) let referrer = p.referrer.get());
    rooted!(in(cx) let request = p.request.get());
    rooted!(in(cx) let payload = p.payload.get());
    assert!(unsafe { answer(cx, referrer.handle().into(), request.handle().into(), payload.handle().into()) });
    p.specifier
}

unsafe extern "C" fn meta_hook(cx: *mut JSContext, module: JS::HandleObject, meta: JS::HandleObject) -> bool {
    LOADER.with(|l| l.borrow_mut().meta_calls += 1);
    // import.meta.url is the module's private, as Servo derives it from its BaseScript.
    rooted!(in(cx) let mut private = UndefinedValue());
    unsafe { JS_GetModulePrivate(*module.ptr, private.handle_mut().into()) };
    let n = if private.get().is_undefined() { -1 } else { private.get().to_private() as usize as i32 };
    rooted!(in(cx) let v = Int32Value(n));
    unsafe { JS_DefineProperty(cx, meta, c"privateNumber".as_ptr(), v.handle().into(), JSPROP_ENUMERATE as u32) }
}

fn setup(sources: &[(&str, &str)]) -> (JSEngine, Runtime) {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    LOADER.with(|l| {
        let mut l = l.borrow_mut();
        *l = Loader::default();
        for (k, v) in sources {
            l.sources.insert((*k).to_owned(), (*v).to_owned());
        }
    });
    let cx = unsafe { runtime.cx().raw_cx() };
    unsafe {
        SetModuleLoadHook(JS_GetRuntime(cx), Some(load_hook));
        SetModuleMetadataHook(JS_GetRuntime(cx), Some(meta_hook));
        JS_AddExtraGCRootsTracer(cx, Some(trace_loader), ptr::null_mut());
    }
    (engine, runtime)
}

/// What Servo's Window trace (`Window_Binding::_trace`) does for the module map: trace the document's records.
unsafe extern "C" fn trace_window_fields(trc: *mut JSTracer, _global: *mut JSObject) {
    LOADER.with(|l| {
        for (_, r) in &l.borrow().records {
            unsafe { r.trace(trc) };
        }
    });
}

/// Servo's Window class: the class trace hook is `JS_GlobalObjectTraceHook`, and the Window's own trace is the realm's
/// `RealmCreationOptions::traceGlobal_` (script_bindings/interface.rs `create_global_object`).
static SERVO_WINDOW_OPS: mozjs::jsapi::JSClassOps = mozjs::jsapi::JSClassOps {
    addProperty: None,
    delProperty: None,
    enumerate: None,
    newEnumerate: None,
    resolve: None,
    mayResolve: None,
    finalize: None,
    call: None,
    construct: None,
    trace: Some(mozjs::jsapi::JS_GlobalObjectTraceHook),
};

static SERVO_WINDOW: mozjs::jsapi::JSClass = mozjs::jsapi::JSClass {
    name: c"Window".as_ptr(),
    flags: mozjs::jsapi::JSCLASS_IS_GLOBAL
        | ((mozjs::jsapi::JSCLASS_GLOBAL_SLOT_COUNT & mozjs::jsapi::JSCLASS_RESERVED_SLOTS_MASK)
            << mozjs::jsapi::JSCLASS_RESERVED_SLOTS_SHIFT),
    cOps: &SERVO_WINDOW_OPS,
    spec: ptr::null(),
    ext: ptr::null(),
    oOps: ptr::null(),
};

unsafe extern "C" fn trace_module_map_global(trc: *mut JSTracer, global: *mut JSObject) {
    unsafe { mozjs::jsapi::JS_GlobalObjectTraceHook(trc, global) };
    LOADER.with(|l| {
        for (_, r) in &l.borrow().records {
            unsafe { r.trace(trc) };
        }
    });
}

static MODULE_MAP_GLOBAL_OPS: mozjs::jsapi::JSClassOps = mozjs::jsapi::JSClassOps {
    addProperty: None,
    delProperty: None,
    enumerate: None,
    newEnumerate: None,
    resolve: None,
    mayResolve: None,
    finalize: None,
    call: None,
    construct: None,
    trace: Some(trace_module_map_global),
};

static MODULE_MAP_GLOBAL: mozjs::jsapi::JSClass = mozjs::jsapi::JSClass {
    name: c"ModuleMapGlobal".as_ptr(),
    flags: mozjs::jsapi::JSCLASS_IS_GLOBAL
        | ((mozjs::jsapi::JSCLASS_GLOBAL_SLOT_COUNT & mozjs::jsapi::JSCLASS_RESERVED_SLOTS_MASK)
            << mozjs::jsapi::JSCLASS_RESERVED_SLOTS_SHIFT),
    cOps: &MODULE_MAP_GLOBAL_OPS,
    spec: ptr::null(),
    ext: ptr::null(),
    oOps: ptr::null(),
};

/// A fresh global and realm for `$body`, with `$cx` the raw context and `$global` the global's handle.
macro_rules! in_global {
    ($runtime:expr, |$cx:ident, $context:ident, $global:ident| $body:block) => {
        in_global!($runtime, &SIMPLE_GLOBAL_CLASS, |$cx, $context, $global| $body)
    };
    ($runtime:expr, $class:expr, |$cx:ident, $context:ident, $global:ident| $body:block) => {{
        let context = $runtime.cx();
        rooted!(&in(context) let global_root = unsafe { JS_NewGlobalObject(
            context,
            $class,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ) });
        let mut realm = AutoRealm::new_from_handle(context, global_root.handle());
        let $context = &mut *realm;
        let $cx = unsafe { $context.raw_cx() };
        let $global = global_root.handle();
        $body
    }};
}

unsafe fn compile(cx: *mut JSContext, name: &str, source: &str) -> *mut JSObject {
    let file = CString::new(name).unwrap();
    let options = unsafe { mozjs::glue::NewCompileOptions(cx, file.as_ptr(), 1) };
    let mut src = transform_str_to_source_text(source);
    let m = unsafe { CompileModule1(cx, options, &mut src) };
    unsafe { mozjs::glue::DeleteCompileOptions(options) };
    m
}

/// LoadRequestedModules1 and the state of its promise right after the call.
unsafe fn load(cx: *mut JSContext, module: JS::HandleObject) -> (*mut JSObject, PromiseState) {
    rooted!(in(cx) let host_defined = UndefinedValue());
    rooted!(in(cx) let mut promise = ptr::null_mut::<JSObject>());
    assert!(unsafe { LoadRequestedModules1(cx, module, host_defined.handle().into(), promise.handle_mut().into()) });
    let state = unsafe { GetPromiseState(promise.handle().into()) };
    (promise.get(), state)
}

/// Link and evaluate; returns the evaluation promise.
unsafe fn link_and_evaluate(cx: *mut JSContext, module: JS::HandleObject) -> *mut JSObject {
    assert!(unsafe { ModuleLink(cx, module) }, "link failed");
    rooted!(in(cx) let mut rval = UndefinedValue());
    assert!(unsafe { ModuleEvaluate(cx, module, rval.handle_mut().into()) });
    assert!(rval.get().is_object());
    rval.get().to_object()
}

fn eval_i32(context: &mut mozjs::context::JSContext, global: mozjs::rust::HandleObject, src: &str) -> i32 {
    rooted!(&in(context) let mut rval = UndefinedValue());
    let options = CompileOptionsWrapper::new(context, c"check.js".to_owned(), 1);
    assert!(evaluate_script(context, global, src, rval.handle_mut(), options).is_ok(), "{src} threw");
    rval.get().to_int32()
}

unsafe fn pending_message(cx: *mut JSContext, context: &mut mozjs::context::JSContext, global: mozjs::rust::HandleObject) -> String {
    assert!(unsafe { JS_IsExceptionPending(cx) });
    rooted!(in(cx) let mut e = UndefinedValue());
    assert!(unsafe { JS_GetPendingException(cx, e.handle_mut().into()) });
    unsafe { JS_ClearPendingException(cx) };
    // Read `e.message` through a property get.
    rooted!(in(cx) let obj = e.get().to_object());
    rooted!(in(cx) let mut msg = UndefinedValue());
    assert!(unsafe { JS_GetProperty(cx, obj.handle().into(), c"message".as_ptr(), msg.handle_mut().into()) });
    let _ = (context, global);
    unsafe { jsstr_to_string(&mozjs::context::JSContext::from_ptr(NonNull::new(cx).unwrap()), NonNull::new(msg.get().to_string()).unwrap()) }
}

#[test]
fn static_import_resolved_by_the_load_hook() {
    let (_e, mut runtime) = setup(&[("dep.js", "export const x = 40; export function add(a) { return a + 2; }")]);
    in_global!(runtime, |cx, context, global| {
        unsafe {
            rooted!(in(cx) let root = compile(cx, "root.js", "import { x, add } from 'dep.js'; globalThis.result = add(x);"));
            assert!(!root.get().is_null());
            let (_, state) = load(cx, root.handle().into());
            assert_eq!(state, PromiseState::Fulfilled);
            assert_eq!(LOADER.with(|l| l.borrow().asked.clone()), vec!["dep.js".to_owned()]);
            rooted!(in(cx) let p = link_and_evaluate(cx, root.handle().into()));
            assert_eq!(GetPromiseState(p.handle().into()), PromiseState::Fulfilled);
            assert!(ThrowOnModuleEvaluationFailure(cx, p.handle().into(), ModuleErrorBehaviour::ThrowModuleErrorsSync));
        }
        assert_eq!(eval_i32(context, global, "result"), 42);
    });
}

#[test]
fn deferred_loads_finish_later_and_records_keep_their_address() {
    let (_e, mut runtime) = setup(&[("a.js", "import b from 'b.js'; export default b + 1;"), ("b.js", "export default 10;")]);
    LOADER.with(|l| l.borrow_mut().deferred = true);
    in_global!(runtime, |cx, context, global| {
        unsafe {
            rooted!(in(cx) let root = compile(cx, "root.js", "import a from 'a.js'; globalThis.result = a;"));
            let address = root.get();
            let (promise, state) = load(cx, root.handle().into());
            rooted!(in(cx) let promise = promise);
            assert_eq!(state, PromiseState::Pending);
            // A GC while Servo fetches: the record and the queued referrer survive, the record does not move.
            JS_GC(cx, JS::GCReason::API);
            assert_eq!(root.get(), address);
            assert_eq!(finish_next(cx), "a.js");
            assert_eq!(GetPromiseState(promise.handle().into()), PromiseState::Pending);
            JS_GC(cx, JS::GCReason::API);
            assert_eq!(finish_next(cx), "b.js");
            assert_eq!(GetPromiseState(promise.handle().into()), PromiseState::Fulfilled);
            link_and_evaluate(cx, root.handle().into());
        }
        assert_eq!(eval_i32(context, global, "result"), 11);
    });
}

#[test]
fn a_cycle_of_two_modules() {
    let (_e, mut runtime) = setup(&[
        ("even.js", "import { odd } from 'odd.js'; export function even(n) { return n == 0 ? true : odd(n - 1); }"),
        ("odd.js", "import { even } from 'even.js'; export function odd(n) { return n == 0 ? false : even(n - 1); }"),
    ]);
    in_global!(runtime, |cx, context, global| {
        unsafe {
            rooted!(in(cx) let root = record_for(cx, "even.js", ModuleType::JavaScript));
            let (_, state) = load(cx, root.handle().into());
            assert_eq!(state, PromiseState::Fulfilled);
            // odd.js asks for even.js again; the module map answers with the same record, and the walk stops there.
            assert_eq!(LOADER.with(|l| l.borrow().asked.clone()), vec!["odd.js".to_owned(), "even.js".to_owned()]);
            link_and_evaluate(cx, root.handle().into());
            rooted!(in(cx) let ns = GetModuleNamespace(cx, root.handle().into()));
            assert!(!ns.get().is_null());
            rooted!(in(cx) let nsv = ObjectValue(ns.get()));
            assert!(JS_DefineProperty(cx, global.into(), c"ns".as_ptr(), nsv.handle().into(), JSPROP_ENUMERATE as u32));
        }
        assert_eq!(eval_i32(context, global, "(ns.even(10) ? 1 : 0) + (ns.even(7) ? 10 : 0)"), 1);
    });
}

#[test]
fn top_level_await_settles_the_evaluation_promise() {
    let (_e, mut runtime) = setup(&[]);
    in_global!(runtime, |cx, context, global| {
        unsafe {
            rooted!(in(cx) let root = compile(cx, "tla.js", "globalThis.step = 1; await Promise.resolve(); globalThis.step = 2;"));
            load(cx, root.handle().into());
            rooted!(in(cx) let p = link_and_evaluate(cx, root.handle().into()));
            assert_eq!(GetPromiseState(p.handle().into()), PromiseState::Pending);
            js::RunJobs(cx);
            assert_eq!(GetPromiseState(p.handle().into()), PromiseState::Fulfilled);
        }
        assert_eq!(eval_i32(context, global, "step"), 2);
    });
}

#[test]
fn evaluation_error_rejects_and_throws_sync() {
    let (_e, mut runtime) = setup(&[]);
    in_global!(runtime, |cx, context, global| {
        unsafe {
            rooted!(in(cx) let root = compile(cx, "throws.js", "throw new RangeError('boom');"));
            load(cx, root.handle().into());
            rooted!(in(cx) let p = link_and_evaluate(cx, root.handle().into()));
            assert_eq!(GetPromiseState(p.handle().into()), PromiseState::Rejected);
            assert!(!ThrowOnModuleEvaluationFailure(cx, p.handle().into(), ModuleErrorBehaviour::ThrowModuleErrorsSync));
            assert_eq!(pending_message(cx, context, global), "boom");
        }
    });
}

#[test]
fn syntax_error_is_a_pending_exception() {
    let (_e, mut runtime) = setup(&[]);
    in_global!(runtime, |cx, context, global| {
        unsafe {
            let m = compile(cx, "bad.js", "export let = ;");
            assert!(m.is_null());
            assert!(JS_IsExceptionPending(cx));
            rooted!(in(cx) let mut e = UndefinedValue());
            assert!(JS_GetPendingException(cx, e.handle_mut().into()));
            rooted!(in(cx) let ev = e.get());
            assert!(JS_DefineProperty(cx, global.into(), c"err".as_ptr(), ev.handle().into(), JSPROP_ENUMERATE as u32));
            JS_ClearPendingException(cx);
        }
        assert_eq!(eval_i32(context, global, "err instanceof SyntaxError ? 1 : 0"), 1);
    });
}

#[test]
fn link_error_for_a_missing_export() {
    let (_e, mut runtime) = setup(&[("dep.js", "export const there = 1;")]);
    in_global!(runtime, |cx, context, global| {
        unsafe {
            rooted!(in(cx) let root = compile(cx, "root.js", "import { notThere } from 'dep.js'; notThere;"));
            load(cx, root.handle().into());
            assert!(!ModuleLink(cx, root.handle().into()));
            rooted!(in(cx) let mut e = UndefinedValue());
            assert!(JS_GetPendingException(cx, e.handle_mut().into()));
            rooted!(in(cx) let ev = e.get());
            assert!(JS_DefineProperty(cx, global.into(), c"err".as_ptr(), ev.handle().into(), JSPROP_ENUMERATE as u32));
            JS_ClearPendingException(cx);
        }
        assert_eq!(eval_i32(context, global, "err instanceof SyntaxError ? 1 : 0"), 1);
    });
}

#[test]
fn failed_load_rejects_the_loading_promise() {
    let (_e, mut runtime) = setup(&[]);
    in_global!(runtime, |cx, _context, _global| {
        unsafe {
            rooted!(in(cx) let root = compile(cx, "root.js", "import 'missing.js';"));
            let (promise, state) = load(cx, root.handle().into());
            rooted!(in(cx) let promise = promise);
            assert_eq!(state, PromiseState::Rejected);
            assert!(!JS_IsExceptionPending(cx));
            // The rejection reason is what the hook threw.
            rooted!(in(cx) let mut reason = UndefinedValue());
            mozjs::glue::JS_GetPromiseResult(promise.handle().into(), reason.handle_mut().into());
            let context2 = mozjs::context::JSContext::from_ptr(NonNull::new(cx).unwrap());
            assert_eq!(jsstr_to_string(&context2, NonNull::new(reason.get().to_string()).unwrap()), "no module missing.js");
            // Linking a graph that never loaded is an error, not a file load.
            assert!(!ModuleLink(cx, root.handle().into()));
            JS_ClearPendingException(cx);
        }
    });
}

#[test]
fn dynamic_import_finished_later_by_the_host() {
    let (_e, mut runtime) = setup(&[("lazy.js", "export const value = 5;")]);
    LOADER.with(|l| l.borrow_mut().deferred = true);
    in_global!(runtime, |cx, context, global| {
        unsafe {
            rooted!(in(cx) let root = compile(cx, "root.js", "globalThis.got = 0; import('lazy.js').then(ns => { globalThis.got = ns.value; });"));
            load(cx, root.handle().into());
            link_and_evaluate(cx, root.handle().into());
            let p = LOADER.with(|l| l.borrow_mut().pending.remove(0));
            assert_eq!(p.specifier, "lazy.js");
            rooted!(in(cx) let referrer = p.referrer.get());
            rooted!(in(cx) let request = p.request.get());
            rooted!(in(cx) let payload = p.payload.get());
            // The payload is the import() promise, so Servo takes the dynamic path.
            rooted!(in(cx) let promise = payload.get().to_object());
            assert!(IsPromiseObject(promise.handle().into()));
            // The referrer is the importing module's script.
            assert_eq!(GetModuleObject(referrer.handle().into()), root.get());
            JS_GC(cx, JS::GCReason::API);
            rooted!(in(cx) let module = record_for(cx, "lazy.js", ModuleType::JavaScript));
            assert!(FinishLoadingDynamicImportedModule(cx, referrer.handle().into(), request.handle().into(), payload.handle().into(), module.handle().into()));
            // Servo's continue_dynamic_import: load, link, evaluate, resolve with the namespace.
            load(cx, module.handle().into());
            link_and_evaluate(cx, module.handle().into());
            rooted!(in(cx) let ns = GetModuleNamespace(cx, module.handle().into()));
            rooted!(in(cx) let nsv = ObjectValue(ns.get()));
            assert!(ResolvePromise(cx, promise.handle().into(), nsv.handle().into()));
            js::RunJobs(cx);
        }
        assert_eq!(eval_i32(context, global, "got"), 5);
    });
}

#[test]
fn dynamic_import_finished_by_finish_loading_imported_module() {
    let (_e, mut runtime) = setup(&[("lazy.js", "import { base } from 'base.js'; export const value = base * 2;"), ("base.js", "export const base = 21;")]);
    LOADER.with(|l| l.borrow_mut().deferred = true);
    in_global!(runtime, |cx, context, global| {
        unsafe {
            rooted!(in(cx) let root = compile(cx, "root.js", "globalThis.got = 0; import('lazy.js').then(ns => { globalThis.got = ns.value; }, e => { globalThis.got = -1; });"));
            load(cx, root.handle().into());
            link_and_evaluate(cx, root.handle().into());
            let p = LOADER.with(|l| l.borrow_mut().pending.remove(0));
            // The host loads the dependencies first (SpiderMonkey's usePromise path expects them loaded).
            LOADER.with(|l| l.borrow_mut().deferred = false);
            rooted!(in(cx) let module = record_for(cx, "lazy.js", ModuleType::JavaScript));
            load(cx, module.handle().into());
            rooted!(in(cx) let referrer = p.referrer.get());
            rooted!(in(cx) let request = p.request.get());
            rooted!(in(cx) let payload = p.payload.get());
            assert!(FinishLoadingImportedModule(cx, referrer.handle().into(), request.handle().into(), payload.handle().into(), module.handle().into(), true));
            js::RunJobs(cx);
        }
        assert_eq!(eval_i32(context, global, "got"), 42);
    });
}

#[test]
fn module_private_round_trip_and_script() {
    let (_e, mut runtime) = setup(&[]);
    in_global!(runtime, |cx, _context, _global| {
        unsafe {
            rooted!(in(cx) let m = compile(cx, "p.js", "export {};"));
            rooted!(in(cx) let mut private = UndefinedValue());
            JS_GetModulePrivate(m.get(), private.handle_mut().into());
            assert!(private.get().is_undefined());
            let v = PrivateValue(0x1230 as *const c_void);
            SetModulePrivate(m.get(), &v);
            JS_GC(cx, JS::GCReason::API);
            JS_GetModulePrivate(m.get(), private.handle_mut().into());
            assert_eq!(private.get().to_private(), 0x1230 as *const c_void);
            // Record -> script -> record.
            rooted!(in(cx) let script = GetModuleScript(m.handle().into()));
            assert!(!script.get().is_null());
            assert_eq!(GetModuleObject(script.handle().into()), m.get());
            assert_eq!(mozjs::jsapi::JS_GetGlobalFromScript(script.get()), mozjs::jsapi::CurrentGlobalOrNull(cx));
        }
    });
}

#[test]
fn namespace_object_exports() {
    let (_e, mut runtime) = setup(&[]);
    in_global!(runtime, |cx, context, global| {
        unsafe {
            rooted!(in(cx) let m = compile(cx, "ns.js", "export const a = 1; export let b = 2; export default 3; export function f() { b = 20; }"));
            load(cx, m.handle().into());
            link_and_evaluate(cx, m.handle().into());
            rooted!(in(cx) let ns = GetModuleNamespace(cx, m.handle().into()));
            assert!(!ns.get().is_null());
            assert_eq!(GetModuleForNamespace(cx, ns.handle().into()), m.get());
            rooted!(in(cx) let nsv = ObjectValue(ns.get()));
            assert!(JS_DefineProperty(cx, global.into(), c"ns".as_ptr(), nsv.handle().into(), JSPROP_ENUMERATE as u32));
        }
        assert_eq!(eval_i32(context, global, "ns.f(); ns.a * 1000 + ns.b * 10 + ns.default"), 1203);
        assert_eq!(eval_i32(context, global, "Object.keys(ns).join() === 'a,b,default,f' ? 1 : 0"), 1);
        assert_eq!(eval_i32(context, global, "Object.isFrozen(ns) || Reflect.set(ns, 'a', 5) ? 0 : 1"), 1);
    });
}

#[test]
fn json_module_and_synthetic_default_export() {
    let (_e, mut runtime) = setup(&[("data.json", "{\"n\": 7, \"list\": [1, 2]}")]);
    in_global!(runtime, |cx, context, global| {
        unsafe {
            rooted!(in(cx) let root = compile(cx, "root.js", "import data from 'data.json' with { type: 'json' }; import text from 'text'; globalThis.result = data.n + data.list.length + text.length;"));
            assert_eq!(GetRequestedModuleType(cx, root.handle().into(), 0), ModuleType::JSON);
            assert_eq!(GetRequestedModuleType(cx, root.handle().into(), 1), ModuleType::JavaScript);
            // A synthetic module made from a value, as Servo makes text and CSS modules.
            rooted!(in(cx) let s = mozjs::jsapi::JS_NewStringCopyN(cx, c"abcd".as_ptr(), 4));
            rooted!(in(cx) let sv = mozjs::jsval::StringValue(&*s.get()));
            rooted!(in(cx) let text = CreateDefaultExportSyntheticModule(cx, sv.handle().into()));
            assert!(!text.get().is_null());
            assert!(GetModuleScript(text.handle().into()).is_null());
            LOADER.with(|l| l.borrow_mut().records.push(("text".to_owned(), Heap::boxed(text.get()))));
            let (_, state) = load(cx, root.handle().into());
            assert_eq!(state, PromiseState::Fulfilled);
            link_and_evaluate(cx, root.handle().into());
            // Invalid JSON is JSON.parse's SyntaxError.
            let options = mozjs::glue::NewCompileOptions(cx, c"bad.json".as_ptr(), 1);
            let mut src = transform_str_to_source_text("{n: 1}");
            assert!(CompileJsonModule1(cx, options, &mut src).is_null());
            mozjs::glue::DeleteCompileOptions(options);
            assert!(JS_IsExceptionPending(cx));
            JS_ClearPendingException(cx);
        }
        assert_eq!(eval_i32(context, global, "result"), 13);
    });
}

#[test]
fn import_meta_through_the_metadata_hook() {
    let (_e, mut runtime) = setup(&[]);
    in_global!(runtime, |cx, context, global| {
        unsafe {
            rooted!(in(cx) let m = compile(cx, "meta.js", "globalThis.result = import.meta.privateNumber + (import.meta === import.meta ? 1000 : 0);"));
            let v = PrivateValue(77 as *const c_void);
            SetModulePrivate(m.get(), &v);
            load(cx, m.handle().into());
            link_and_evaluate(cx, m.handle().into());
        }
        assert_eq!(eval_i32(context, global, "result"), 1077);
        assert_eq!(LOADER.with(|l| l.borrow().meta_calls), 1);
    });
}

#[test]
fn referrer_private_is_visible_to_the_load_hook() {
    // Servo's hook reads the referrer's private with JS_GetScriptPrivate to find the base URL.
    unsafe extern "C" fn hook(
        cx: *mut JSContext,
        referrer: JS::Handle<*mut JSScript>,
        request: JS::HandleObject,
        _hd: JS::HandleValue,
        payload: JS::HandleValue,
        _l: u32,
        _c: JS::ColumnNumberOneOrigin,
    ) -> bool {
        rooted!(in(cx) let mut private = UndefinedValue());
        unsafe { mozjs::glue::JS_GetScriptPrivate(*referrer.ptr, private.handle_mut().into()) };
        let n = private.get().to_private() as usize;
        LOADER.with(|l| l.borrow_mut().meta_calls = n);
        unsafe { answer(cx, referrer, request, payload) }
    }
    let (_e, mut runtime) = setup(&[("dep.js", "export {};")]);
    in_global!(runtime, |cx, _context, _global| {
        unsafe {
            SetModuleLoadHook(JS_GetRuntime(cx), Some(hook));
            rooted!(in(cx) let m = compile(cx, "root.js", "import 'dep.js';"));
            let v = PrivateValue(4242 as *const c_void);
            SetModulePrivate(m.get(), &v);
            let (_, state) = load(cx, m.handle().into());
            assert_eq!(state, PromiseState::Fulfilled);
        }
        assert_eq!(LOADER.with(|l| l.borrow().meta_calls), 4242);
    });
}

#[test]
fn synchronous_finish_from_the_hook_with_gcs_and_a_shared_dependency() {
    let (_e, mut runtime) = setup(&[
        ("a.js", "import { s } from 'shared.js'; export const a = s + 1;"),
        ("b.js", "import { s } from 'shared.js'; import { a } from 'a.js'; export const b = s + a;"),
        ("shared.js", "import { leaf } from 'leaf.js'; export const s = leaf * 10;"),
        ("leaf.js", "export const leaf = 2;"),
    ]);
    LOADER.with(|l| l.borrow_mut().gc_in_hook = true);
    in_global!(runtime, |cx, context, global| {
        unsafe {
            rooted!(in(cx) let root = compile(cx, "root.js", "import { a } from 'a.js'; import { b } from 'b.js'; globalThis.result = a * 100 + b;"));
            JS_GC(cx, JS::GCReason::API);
            let (promise, state) = load(cx, root.handle().into());
            rooted!(in(cx) let promise = promise);
            assert_eq!(state, PromiseState::Fulfilled);
            JS_GC(cx, JS::GCReason::API);
            let _ = promise.get();
            link_and_evaluate(cx, root.handle().into());
            // A second graph over the same records, all already in the module map.
            rooted!(in(cx) let again = compile(cx, "again.js", "import { b } from 'b.js'; globalThis.again = b;"));
            let (_, state) = load(cx, again.handle().into());
            assert_eq!(state, PromiseState::Fulfilled);
            link_and_evaluate(cx, again.handle().into());
        }
        assert_eq!(eval_i32(context, global, "result"), 21 * 100 + 41);
        assert_eq!(eval_i32(context, global, "again"), 41);
    });
}

#[test]
fn records_traced_only_by_the_global_survive_gcs() {
    let (_e, mut runtime) = setup(&[
        ("a.js", "import { s } from 'shared.js'; export const a = s + 1;"),
        ("b.js", "import { s } from 'shared.js'; export const b = s + 2;"),
        ("shared.js", "export const s = 10;"),
    ]);
    LOADER.with(|l| {
        let mut l = l.borrow_mut();
        l.gc_in_hook = true;
        l.records_in_global = true;
    });
    in_global!(runtime, &MODULE_MAP_GLOBAL, |cx, context, global| {
        unsafe {
            rooted!(in(cx) let first = compile(cx, "first.js", "import { a } from 'a.js'; globalThis.first = a;"));
            let (_, state) = load(cx, first.handle().into());
            assert_eq!(state, PromiseState::Fulfilled);
            link_and_evaluate(cx, first.handle().into());
            // Only the global's trace hook holds a.js, b.js and shared.js now.
            JS_GC(cx, JS::GCReason::API);
            JS_GC(cx, JS::GCReason::API);
            rooted!(in(cx) let second = compile(cx, "second.js", "import { a } from 'a.js'; import { b } from 'b.js'; globalThis.second = a + b;"));
            let (_, state) = load(cx, second.handle().into());
            assert_eq!(state, PromiseState::Fulfilled);
            link_and_evaluate(cx, second.handle().into());
        }
        assert_eq!(eval_i32(context, global, "first"), 11);
        assert_eq!(eval_i32(context, global, "second"), 23);
    });
}

/// GitHub's crash (2026-11-05): Servo's module map is reachable only through the Window's realm trace hook. A GC that does
/// not run it collects every module object Servo still holds, and the next FinishLoadingImportedModule reads a freed one.
#[test]
fn module_map_traced_through_the_realm_trace_hook_survives_gcs() {
    let (_e, mut runtime) = setup(&[
        ("a.js", "import { s } from 'shared.js'; export const a = s + 1;"),
        ("b.js", "import { s } from 'shared.js'; export const b = s + 2;"),
        ("shared.js", "import { leaf } from 'leaf.js'; export const s = leaf * 10;"),
        ("leaf.js", "export const leaf = 1;"),
    ]);
    LOADER.with(|l| {
        let mut l = l.borrow_mut();
        l.gc_in_hook = true;
        l.records_in_global = true;
    });
    let context = runtime.cx();
    let mut options = RealmOptions::default();
    options.creationOptions_.traceGlobal_ = Some(trace_window_fields);
    rooted!(&in(context) let global = unsafe {
        JS_NewGlobalObject(context, &SERVO_WINDOW, ptr::null_mut(), OnNewGlobalHookOption::FireOnNewGlobalHook, &*options)
    });
    let mut realm = AutoRealm::new_from_handle(context, global.handle());
    let context = &mut *realm;
    let cx = unsafe { context.raw_cx() };
    unsafe {
        {
            // Evaluated and dropped, as Servo drops a top-level module script after running it.
            rooted!(in(cx) let first = compile(cx, "first.js", "import { a } from 'a.js'; globalThis.first = a;"));
            let (_, state) = load(cx, first.handle().into());
            assert_eq!(state, PromiseState::Fulfilled);
            link_and_evaluate(cx, first.handle().into());
        }
        JS_GC(cx, JS::GCReason::API);
        // Allocate enough to need more collections while only the realm trace hook holds the records.
        let options = CompileOptionsWrapper::new(context, c"churn.js".to_owned(), 1);
        rooted!(&in(context) let mut rval = UndefinedValue());
        let churn = "let keep = []; for (let i = 0; i < 200000; i++) { keep.push({ i, s: 'x' + i }); if (keep.length > 1000) keep = []; } 0";
        assert!(evaluate_script(context, global.handle(), churn, rval.handle_mut(), options).is_ok());
        JS_GC(cx, JS::GCReason::API);
        // New module objects of the same size reuse whatever the GC freed in the host space.
        rooted!(in(cx) let filler = Int32Value(0));
        for _ in 0..64 {
            assert!(!CreateDefaultExportSyntheticModule(cx, filler.handle().into()).is_null());
        }
        // Every record is still the module Servo stored: a live, linked source text module.
        let records: Vec<*mut JSObject> = LOADER.with(|l| l.borrow().records.iter().map(|(_, r)| r.get()).collect());
        assert_eq!(records.len(), 3);
        for r in records {
            rooted!(in(cx) let r = r);
            assert!(mozjs::jsapi::JS::IsCyclicModule(r.get()));
            assert!(mozjs::jsapi::JS::ModuleIsLinked(r.get()));
        }
        rooted!(in(cx) let second = compile(cx, "second.js", "import { a } from 'a.js'; import { b } from 'b.js'; globalThis.second = a + b;"));
        let (_, state) = load(cx, second.handle().into());
        assert_eq!(state, PromiseState::Fulfilled);
        link_and_evaluate(cx, second.handle().into());
    }
    assert_eq!(eval_i32(context, global.handle(), "first"), 11);
    assert_eq!(eval_i32(context, global.handle(), "second"), 23);
}

static PROXY_FINALIZED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A DOM proxy's `trace` trap, as Servo's HTMLDocument reflector (a proxy, for its named getter) traces its Document,
/// which holds the module map.
unsafe extern "C" fn document_proxy_trace(trc: *mut JSTracer, _proxy: *mut JSObject) {
    LOADER.with(|l| {
        for (_, r) in &l.borrow().records {
            unsafe { r.trace(trc) };
        }
    });
}

unsafe extern "C" fn document_proxy_finalize(_gcx: *mut JS::GCContext, _proxy: *mut JSObject) {
    PROXY_FINALIZED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// GitHub and Discord (2026-11-05, after the realm trace fix): Servo's Document reflector is a proxy, and proxies' trace
/// and finalize traps were never called, so the Document's module map (and its FontFaceSet) were collected.
#[test]
fn module_map_traced_through_a_dom_proxy_trace_trap_survives_gcs() {
    let (_e, mut runtime) = setup(&[
        ("a.js", "import { s } from 'shared.js'; export const a = s + 1;"),
        ("b.js", "import { s } from 'shared.js'; export const b = s + 2;"),
        ("shared.js", "export const s = 10;"),
    ]);
    LOADER.with(|l| {
        let mut l = l.borrow_mut();
        l.gc_in_hook = true;
        l.records_in_global = true;
    });
    in_global!(runtime, |cx, context, global| {
        unsafe {
            let mut traps: mozjs::glue::ProxyTraps = std::mem::zeroed();
            traps.trace = Some(document_proxy_trace);
            traps.finalize = Some(document_proxy_finalize);
            let handler = mozjs::glue::CreateProxyHandler(&traps, ptr::null());
            rooted!(in(cx) let private = UndefinedValue());
            rooted!(in(cx) let document = mozjs::glue::NewProxyObject(cx, handler, private.handle().into(), ptr::null_mut(), ptr::null(), false));
            assert!(!document.get().is_null());
            {
                rooted!(in(cx) let first = compile(cx, "first.js", "import { a } from 'a.js'; globalThis.first = a;"));
                let (_, state) = load(cx, first.handle().into());
                assert_eq!(state, PromiseState::Fulfilled);
                link_and_evaluate(cx, first.handle().into());
            }
            JS_GC(cx, JS::GCReason::API);
            rooted!(in(cx) let filler = Int32Value(0));
            for _ in 0..64 {
                assert!(!CreateDefaultExportSyntheticModule(cx, filler.handle().into()).is_null());
            }
            JS_GC(cx, JS::GCReason::API);
            let records: Vec<*mut JSObject> = LOADER.with(|l| l.borrow().records.iter().map(|(_, r)| r.get()).collect());
            assert_eq!(records.len(), 2);
            for r in records {
                assert!(mozjs::jsapi::JS::IsCyclicModule(r));
                assert!(mozjs::jsapi::JS::ModuleIsLinked(r));
            }
            rooted!(in(cx) let second = compile(cx, "second.js", "import { a } from 'a.js'; import { b } from 'b.js'; globalThis.second = a + b;"));
            let (_, state) = load(cx, second.handle().into());
            assert_eq!(state, PromiseState::Fulfilled);
            link_and_evaluate(cx, second.handle().into());
            // The live proxy was not finalized; a dropped one is.
            assert_eq!(PROXY_FINALIZED.load(std::sync::atomic::Ordering::SeqCst), 0);
            let _ = mozjs::glue::NewProxyObject(cx, handler, private.handle().into(), ptr::null_mut(), ptr::null(), false);
            JS_GC(cx, JS::GCReason::API);
            assert_eq!(PROXY_FINALIZED.load(std::sync::atomic::Ordering::SeqCst), 1);
            let _ = document.get();
        }
        assert_eq!(eval_i32(context, global, "first"), 11);
        assert_eq!(eval_i32(context, global, "second"), 23);
    });
}
