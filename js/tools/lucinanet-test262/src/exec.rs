//! Worker-side test execution against the mozjs crate API.
//!
//! One `JSEngine` per process; each test variant gets a fresh `Runtime`,
//! global object and realm.

use std::cell::RefCell;
use std::ffi::CString;
use std::path::PathBuf;
use std::ptr::{self, NonNull};

use mozjs::jsapi::{self, JS_CallArgsFromVp, OnNewGlobalHookOption, JS_GC};
use mozjs::jsval::{JSVal, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::*;
use mozjs::rust::{
    evaluate_script, new_is_htmldda, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime,
    SIMPLE_GLOBAL_CLASS,
};
use serde::{Deserialize, Serialize};

use crate::meta::{parse_front_matter, variants_for, FrontMatter, Negative, Variant};

thread_local! {
    static PRINT_LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Job {
    pub path: String,
    pub variant: String,
    pub root: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestResult {
    pub path: String,
    pub variant: String,
    pub result: String,
    pub message: String,
    #[serde(default)]
    pub features: Vec<String>,
}

fn jsval_to_string(cx: &mut mozjs::context::JSContext, v: JSVal) -> String {
    unsafe {
        rooted!(&in(cx) let value = v);
        let s = mozjs::rust::ToString(cx, value.handle());
        if s.is_null() {
            return "<unconvertible>".to_string();
        }
        let raw = cx.raw_cx();
        if jsapi::JS_DeprecatedStringHasLatin1Chars(s) {
            let mut len = 0usize;
            let chars = jsapi::JS_GetLatin1StringCharsAndLength(raw, ptr::null(), s, &mut len);
            if chars.is_null() {
                return "<unconvertible>".to_string();
            }
            std::slice::from_raw_parts(chars, len)
                .iter()
                .map(|&c| c as char)
                .collect()
        } else {
            let mut len = 0usize;
            let chars = jsapi::JS_GetTwoByteStringCharsAndLength(raw, ptr::null(), s, &mut len);
            if chars.is_null() {
                return "<unconvertible>".to_string();
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(chars, len))
        }
    }
}

unsafe extern "C" fn native_print(
    cx: *mut jsapi::JSContext,
    argc: u32,
    vp: *mut jsapi::JS::Value,
) -> bool {
    let args = JS_CallArgsFromVp(argc, vp);
    let mut cx_box = mozjs::context::JSContext::from_ptr(NonNull::new(cx).unwrap());
    let mut parts = Vec::new();
    for i in 0..argc {
        parts.push(jsval_to_string(&mut cx_box, *args.get(i)));
    }
    PRINT_LOG.with(|log| log.borrow_mut().push(parts.join(" ")));
    args.rval().set(UndefinedValue());
    true
}

unsafe extern "C" fn native_eval_script(
    cx: *mut jsapi::JSContext,
    argc: u32,
    vp: *mut jsapi::JS::Value,
) -> bool {
    let args = JS_CallArgsFromVp(argc, vp);
    let mut cx_box = mozjs::context::JSContext::from_ptr(NonNull::new(cx).unwrap());
    let src = if argc > 0 {
        jsval_to_string(&mut cx_box, *args.get(0))
    } else {
        String::new()
    };
    // evalScript belongs to the realm in which this native was created, even
    // when a test calls another realm's $262.evalScript from its original realm.
    let global = jsapi::JS::GetNonCCWObjectGlobal(args.callee());
    if global.is_null() {
        return false;
    }
    let filename = CString::new("evalScript").unwrap();
    let options = CompileOptionsWrapper::new(&cx_box, filename, 1);
    rooted!(&in(cx_box) let global_obj = global);
    rooted!(&in(cx_box) let mut rval = UndefinedValue());
    let ok = evaluate_script(
        &mut cx_box,
        global_obj.handle(),
        &src,
        rval.handle_mut(),
        options,
    )
    .is_ok();
    args.rval().set(rval.get());
    ok
}

unsafe extern "C" fn native_create_realm(
    cx: *mut jsapi::JSContext,
    _argc: u32,
    vp: *mut jsapi::JS::Value,
) -> bool {
    let args = JS_CallArgsFromVp(_argc, vp);
    let mut cx_box = mozjs::context::JSContext::from_ptr(NonNull::new(cx).unwrap());
    let options = RealmOptions::default();
    rooted!(&in(cx_box) let global = JS_NewGlobalObject(
        &mut cx_box,
        &SIMPLE_GLOBAL_CLASS,
        ptr::null_mut(),
        OnNewGlobalHookOption::FireOnNewGlobalHook,
        &*options,
    ));
    let mut realm = AutoRealm::new(&mut cx_box, NonNull::new(global.handle().get()).unwrap());
    let cx = &mut realm;
    define_262(cx, global.handle());
    rooted!(&in(cx) let mut d262_v = UndefinedValue());
    JS_GetProperty(cx, global.handle(), c"$262".as_ptr(), d262_v.handle_mut());
    args.rval().set(d262_v.get());
    drop(realm);
    true
}

unsafe extern "C" fn native_detach_array_buffer(
    cx: *mut jsapi::JSContext,
    argc: u32,
    vp: *mut jsapi::JS::Value,
) -> bool {
    let args = JS_CallArgsFromVp(argc, vp);
    let mut cx_box = mozjs::context::JSContext::from_ptr(NonNull::new(cx).unwrap());
    let mut obj = args.get(0).to_object_or_null();
    let handle = mozjs::rust::HandleObject::from_marked_location(&mut obj as *mut _);
    let ok = DetachArrayBuffer(&mut cx_box, handle);
    args.rval().set(UndefinedValue());
    ok
}

unsafe extern "C" fn native_gc(
    cx: *mut jsapi::JSContext,
    _argc: u32,
    vp: *mut jsapi::JS::Value,
) -> bool {
    JS_GC(cx, jsapi::JS::GCReason::API);
    let args = JS_CallArgsFromVp(_argc, vp);
    args.rval().set(UndefinedValue());
    true
}

/// Define `print` and the `$262` object on `global`.
fn define_262(
    cx: &mut mozjs::context::JSContext,
    global: mozjs::rust::Handle<*mut jsapi::JSObject>,
) {
    unsafe {
        rooted!(&in(cx) let d262 = JS_NewPlainObject(cx));
        rooted!(&in(cx) let agent = JS_NewPlainObject(cx));

        JS_DefineFunction(cx, global, c"print".as_ptr(), Some(native_print), 1, 0);
        JS_DefineFunction(
            cx,
            d262.handle(),
            c"evalScript".as_ptr(),
            Some(native_eval_script),
            1,
            0,
        );
        JS_DefineFunction(
            cx,
            d262.handle(),
            c"createRealm".as_ptr(),
            Some(native_create_realm),
            0,
            0,
        );
        JS_DefineFunction(
            cx,
            d262.handle(),
            c"detachArrayBuffer".as_ptr(),
            Some(native_detach_array_buffer),
            1,
            0,
        );
        JS_DefineFunction(cx, d262.handle(), c"gc".as_ptr(), Some(native_gc), 0, 0);

        // ECMA-262 B.3.6 / test262 INTERPRETING.md: `$262.IsHTMLDDA`, a callable object carrying the
        // [[IsHTMLDDA]] internal slot. The `emulates-undefined` tests bind `var IsHTMLDDA = $262.IsHTMLDDA`,
        // then define properties on it and call it, so it must be a real object; `new_is_htmldda` builds one
        // per realm here, and `native_create_realm` calls `define_262` on each `$262.createRealm()` root too.
        rooted!(&in(cx) let htmldda = new_is_htmldda(cx));
        let mut htmldda_val = mozjs::jsval::ObjectValue(htmldda.handle().get());
        let ihandle =
            mozjs::rust::HandleValue::from_marked_location(&mut htmldda_val as *mut JSVal);
        JS_SetProperty(cx, d262.handle(), c"IsHTMLDDA".as_ptr(), ihandle);

        let mut global_val = mozjs::jsval::ObjectValue(global.get());
        let ghandle = mozjs::rust::HandleValue::from_marked_location(&mut global_val as *mut JSVal);
        JS_SetProperty(cx, d262.handle(), c"global".as_ptr(), ghandle);
        let mut agent_val = mozjs::jsval::ObjectValue(agent.handle().get());
        let ahandle = mozjs::rust::HandleValue::from_marked_location(&mut agent_val as *mut JSVal);
        JS_SetProperty(cx, d262.handle(), c"agent".as_ptr(), ahandle);

        let mut d262_val = mozjs::jsval::ObjectValue(d262.handle().get());
        let dhandle = mozjs::rust::HandleValue::from_marked_location(&mut d262_val as *mut JSVal);
        JS_SetProperty(cx, global, c"$262".as_ptr(), dhandle);
    }
}

struct ErrorInfo {
    name: String,
    message: String,
    phase: &'static str,
    /// The thrown value is an instance of the realm's `Test262Error`; see `thrown_is_test262_error`.
    is_test262_error: bool,
}

fn take_exception(cx: &mut mozjs::context::JSContext) -> Option<ErrorInfo> {
    unsafe {
        if !JS_IsExceptionPending(cx) {
            return None;
        }
        rooted!(&in(cx) let mut exc = UndefinedValue());
        JS_GetPendingException(cx, exc.handle_mut());
        let info = if exc.get().is_object() {
            let mut obj = exc.get().to_object();
            let handle = mozjs::rust::HandleObject::from_marked_location(&mut obj as *mut _);
            rooted!(&in(cx) let mut name_v = UndefinedValue());
            rooted!(&in(cx) let mut msg_v = UndefinedValue());
            JS_GetProperty(cx, handle, c"name".as_ptr(), name_v.handle_mut());
            JS_GetProperty(cx, handle, c"message".as_ptr(), msg_v.handle_mut());
            ErrorInfo {
                phase: "runtime",
                name: jsval_to_string(cx, name_v.get()),
                message: jsval_to_string(cx, msg_v.get()),
                is_test262_error: thrown_is_test262_error(cx, exc.handle()),
            }
        } else {
            ErrorInfo {
                phase: "runtime",
                name: "Error".to_string(),
                message: jsval_to_string(cx, exc.get()),
                is_test262_error: false,
            }
        };
        JS_ClearPendingException(cx);
        Some(info)
    }
}

/// Is the thrown value an instance of the realm's `Test262Error`? A test of `negative: { phase: runtime, type:
/// Test262Error }` signals success by throwing one (`test/language/line-terminators/comment-single-cr.js`), and the harness
/// class is a plain function that sets only `message` and a `toString` (`harness/sta.js`), so it has no `name` to compare and
/// a name match can never hold for it. `instanceof` decides instead: 7.3.21 OrdinaryHasInstance step 4 takes
/// `Get(Test262Error, "prototype")` and steps 6a-6c walk the thrown value's prototype chain to it, so the value is asked
/// only for its prototype and no property is added to it.
///
/// Safety: `cx` is a live context used on the thread that owns it and `exc` is a rooted value that outlives the call, the same
/// precondition `take_exception`, the only caller, already meets when it reads `name` and `message` off the thrown value. The
/// object handles are taken from local pointers of rooted or immediately-preceding `to_object` results, as elsewhere here.
unsafe fn thrown_is_test262_error(
    cx: &mut mozjs::context::JSContext,
    exc: mozjs::rust::HandleValue,
) -> bool {
    let mut exc_obj = exc.get().to_object();
    if exc_obj.is_null() {
        return false;
    }
    // The realm that owns the thrown value answers: `$262.createRealm` tests throw that realm's `Test262Error`.
    let exc_handle = mozjs::rust::HandleObject::from_marked_location(&mut exc_obj as *mut _);
    let mut realm = AutoRealm::new_from_handle(cx, exc_handle);
    let cx = &mut realm;
    let global = jsapi::JS::CurrentGlobalOrNull(cx.raw_cx());
    if global.is_null() {
        return false;
    }
    rooted!(&in(cx) let global_obj = global);
    rooted!(&in(cx) let mut ctor = UndefinedValue());
    if !JS_GetProperty(
        cx,
        global_obj.handle(),
        c"Test262Error".as_ptr(),
        ctor.handle_mut(),
    ) || !ctor.get().is_object()
    {
        return false;
    }
    let mut ctor_obj = ctor.get().to_object();
    let ctor_handle = mozjs::rust::HandleObject::from_marked_location(&mut ctor_obj as *mut _);
    let mut is_instance = false;
    OrdinaryHasInstance(cx, ctor_handle, exc, &mut is_instance) && is_instance
}

fn eval_part(
    cx: &mut mozjs::context::JSContext,
    global: mozjs::rust::Handle<*mut jsapi::JSObject>,
    source: &str,
    filename: &str,
) -> Result<(), &'static str> {
    let filename = CString::new(filename).unwrap_or_else(|_| CString::new("test.js").unwrap());
    let options = CompileOptionsWrapper::new(cx, filename, 1);
    rooted!(&in(cx) let mut rval = UndefinedValue());
    let mut realm = AutoRealm::new_from_handle(cx, global);
    let cx = &mut realm;
    let mut text = mozjs::rust::transform_str_to_source_text(source);
    rooted!(&in(cx) let script = unsafe { jsapi::JS::Compile1(cx.raw_cx(), options.ptr, &mut text) });
    if script.get().is_null() {
        return Err("compile");
    }
    if unsafe { JS_ExecuteScript(cx, script.handle(), rval.handle_mut()) } {
        Ok(())
    } else {
        Err("runtime")
    }
}

fn read_file(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Set the pending exception to `new Error(message)`, for a failure the engine reported by neither throwing nor
/// rejecting (a module whose evaluation promise never settles).
unsafe fn throw_host_error(cx: &mut mozjs::context::JSContext, message: &str) {
    let src = format!(
        "throw new Error({})",
        serde_json::to_string(message).unwrap_or_default()
    );
    let global = jsapi::JS::CurrentGlobalOrNull(cx.raw_cx());
    rooted!(&in(cx) let global_obj = global);
    rooted!(&in(cx) let mut rval = UndefinedValue());
    let options = CompileOptionsWrapper::new(cx, CString::new("module-evaluation").unwrap(), 1);
    let _ = evaluate_script(cx, global_obj.handle(), &src, rval.handle_mut(), options);
}

/// Rounds of the job queue to drain while a module with top-level await stays unevaluated. A module that outlives this
/// waits on something no job delivers, and is reported rather than passed.
const MODULE_AWAIT_ROUNDS: usize = 1_000;

/// Evaluate `source` as the module the test file `path` is (test262 `flags: [module]`), in the realm of `global`:
/// 16.2.1.5 ParseModule (`JS::CompileModule1`), 16.2.1.7 InnerModuleLoading (the load hook answers for every request),
/// 16.2.2.2 ModuleDeclarationInstantiation (`JS::ModuleLink`), 16.2.1.9 Evaluate (`JS::ModuleEvaluate`, whose promise a
/// top-level await settles from the job queue).
///
/// Like `eval_part` it leaves the exception pending and names the phase: `compile` for a parse, resolution or link
/// failure — test262 reports all three as a compile-time negative — and `runtime` for an evaluation failure.
fn eval_module(
    cx: &mut mozjs::context::JSContext,
    global: mozjs::rust::Handle<*mut jsapi::JSObject>,
    source: &str,
    path: &str,
) -> Result<(), &'static str> {
    unsafe {
        let raw = cx.raw_cx();
        let key = module_key(std::path::Path::new(path));
        let filename =
            CString::new(key.clone()).unwrap_or_else(|_| CString::new("test.js").unwrap());
        let options = CompileOptionsWrapper::new(cx, filename, 1);
        let mut text = mozjs::rust::transform_str_to_source_text(source);
        rooted!(&in(cx) let module = jsapi::JS::CompileModule1(raw, options.ptr, &mut text));
        if module.get().is_null() {
            return Err("compile");
        }
        // 16.2.1.7 step 2: the host resolves this module's requests against its own file, so the path is its private
        // and its key in the loader's cache (a fixture that imports the test must get this record back).
        let js_path = jsapi::JS_NewStringCopyN(raw, key.as_ptr() as *const _, key.len());
        if js_path.is_null() {
            return Err("compile");
        }
        let pv = mozjs::jsval::StringValue(&*js_path);
        jsapi::JS::SetModulePrivate(module.get(), &pv);
        let cache = module_cache(cx, global);
        if cache.is_null() {
            return Err("compile");
        }
        rooted!(&in(cx) let cache_obj = cache);
        let record = mozjs::jsval::ObjectValue(module.get());
        if !cache_module(cx, cache_obj.handle(), &key, record) {
            return Err("compile");
        }

        // 16.2.1.7: load the graph. The hook answers every request before this call returns, so its promise is
        // settled here: a rejected one carries the resolution error.
        rooted!(&in(cx) let host_defined = UndefinedValue());
        rooted!(&in(cx) let mut load_promise: *mut jsapi::JSObject = ptr::null_mut());
        if !jsapi::JS::LoadRequestedModules1(
            raw,
            module.handle().into(),
            host_defined.handle().into(),
            load_promise.handle_mut().into(),
        ) {
            return Err("compile");
        }
        if load_promise.get().is_null() {
            return Err("compile");
        }
        rooted!(&in(cx) let load = load_promise.get());
        if jsapi::JS::GetPromiseState(load.handle().into()) == jsapi::JS::PromiseState::Rejected {
            rooted!(&in(cx) let mut error = UndefinedValue());
            mozjs_sys::glue::JS_GetPromiseResult(load.handle().into(), error.handle_mut().into());
            jsapi::JS_SetPendingException(
                raw,
                error.handle().into(),
                jsapi::JS::ExceptionStackBehavior::DoNotCapture,
            );
            return Err("compile");
        }

        // 16.2.2.2 ModuleDeclarationInstantiation: an ambiguous or missing export is a SyntaxError before any code runs.
        if !jsapi::JS::ModuleLink(raw, module.handle().into()) {
            return Err("compile");
        }

        rooted!(&in(cx) let mut rval = UndefinedValue());
        if !jsapi::JS::ModuleEvaluate(raw, module.handle().into(), rval.handle_mut().into()) {
            return Err("runtime");
        }
        if !rval.get().is_object() {
            return Err("runtime");
        }
        rooted!(&in(cx) let evaluation = rval.get().to_object());
        // 16.2.1.9: a module with top-level await is `evaluating-async`; its promise settles once the awaited jobs run.
        let mut rounds = 0;
        while jsapi::JS::GetPromiseState(evaluation.handle().into())
            == jsapi::JS::PromiseState::Pending
        {
            if rounds >= MODULE_AWAIT_ROUNDS || !jsapi::JS::HasRegularMicroTasks(raw) {
                throw_host_error(cx, "module evaluation did not settle");
                return Err("runtime");
            }
            jsapi::js::RunJobs(raw);
            rounds += 1;
        }
        // The evaluation promise is handled here: a rejection becomes the pending exception.
        if !jsapi::JS::ThrowOnModuleEvaluationFailure(
            raw,
            evaluation.handle().into(),
            jsapi::JS::ModuleErrorBehaviour::ThrowModuleErrorsSync,
        ) {
            return Err("runtime");
        }
        Ok(())
    }
}

// SpiderMonkey requires an embedder job queue before `js::RunJobs` can drain
// the microtask queue. Install a RustJobQueue whose `runJobs` trap drains
// regular microtasks.
unsafe extern "C" fn drain_run_jobs(cx: *mut jsapi::JSContext) {
    loop {
        if !jsapi::JS::HasRegularMicroTasks(cx) {
            break;
        }
        let task = jsapi::JS::DequeueNextRegularMicroTask(cx);
        let mut mt = jsapi::JS::ToUnwrappedJSMicroTask(&task);
        let mh = jsapi::JS::Handle::<*mut jsapi::JS::JSMicroTask>::from_marked_location(
            &mut mt as *mut _,
        );
        jsapi::JS::RunJSMicroTask(cx, mh);
    }
}

unsafe extern "C" fn noop_host_data(
    _cx: *mut jsapi::JSContext,
    _g: jsapi::JS::MutableHandle<*mut jsapi::JSObject>,
    _d: jsapi::JS::MutableHandle<*mut jsapi::JSObject>,
) -> bool {
    true
}

unsafe extern "C" fn noop_host_global(
    _cx: *mut jsapi::JSContext,
    _d: jsapi::JS::MutableHandle<*mut jsapi::JSObject>,
) -> bool {
    true
}

unsafe extern "C" fn noop_trace(_trc: *mut jsapi::JSTracer, _v: *mut jsapi::JS::Value) {}

fn install_job_queue(cx: &mut mozjs::context::JSContext) {
    unsafe {
        let traps = mozjs_sys::glue::JobQueueTraps {
            getHostDefinedData: Some(noop_host_data),
            getHostDefinedGlobal: Some(noop_host_global),
            runJobs: Some(drain_run_jobs),
            traceNonGCThingMicroTask: Some(noop_trace),
        };
        let queue = mozjs_sys::glue::CreateJobQueue(&traps);
        jsapi::JS::SetJobQueue(cx.raw_cx(), queue as *mut _);
    }
}

thread_local! {
    /// Directory of the test file being run: the base for `import()` from a script (which has no module referrer).
    static TEST_DIR: RefCell<PathBuf> = const { RefCell::new(PathBuf::new()) };
}

/// Resolve `specifier` against the directory of `base_file` (a file path) or `base_dir`.
pub fn resolve_specifier(base_dir: &std::path::Path, specifier: &str) -> PathBuf {
    let p = std::path::Path::new(specifier);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    }
}

/// The loader's key for a module file: its canonical path, so that `./x.js`, `x.js` and a `../` hop all name one
/// record (a module that imports the file it was imported from must see itself). A file that does not exist keeps the
/// path it was asked for, so the load fails with that name.
fn module_key(path: &std::path::Path) -> String {
    std::fs::canonicalize(path)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// The map from module key to compiled record that the global keeps for the loader, created on first use. It is a
/// non-enumerable property (`%lcModuleCache`) so no test sees it. Null when it could not be created.
unsafe fn module_cache(
    cx: &mut mozjs::context::JSContext,
    global: mozjs::rust::Handle<*mut jsapi::JSObject>,
) -> *mut jsapi::JSObject {
    rooted!(&in(cx) let mut cache_v = UndefinedValue());
    JS_GetProperty(cx, global, c"%lcModuleCache".as_ptr(), cache_v.handle_mut());
    if cache_v.get().is_object() {
        return cache_v.get().to_object();
    }
    rooted!(&in(cx) let fresh = JS_NewPlainObject(cx));
    if fresh.get().is_null() {
        return ptr::null_mut();
    }
    let mut v = mozjs::jsval::ObjectValue(fresh.get());
    let h = mozjs::rust::HandleValue::from_marked_location(&mut v as *mut JSVal);
    JS_DefineProperty(cx, global, c"%lcModuleCache".as_ptr(), h, 0);
    fresh.get()
}

/// Record `module` under `key` in `cache`, so a later request for the same file answers with the same record.
unsafe fn cache_module(
    cx: &mut mozjs::context::JSContext,
    cache: mozjs::rust::Handle<*mut jsapi::JSObject>,
    key: &str,
    module: mozjs::jsval::JSVal,
) -> bool {
    let ckey = match CString::new(key) {
        Ok(k) => k,
        Err(_) => return false,
    };
    let mut mv = module;
    let mh = mozjs::rust::HandleValue::from_marked_location(&mut mv as *mut JSVal);
    JS_DefineProperty(cx, cache, ckey.as_ptr(), mh, 0)
}

/// Throw-free helper: finish the pending `import()` with a fresh `Error(message)`.
unsafe fn hook_fail(
    cx: &mut mozjs::context::JSContext,
    payload: jsapi::JS::Handle<JSVal>,
    message: &str,
) -> bool {
    let src = format!(
        "new Error({})",
        serde_json::to_string(message).unwrap_or_default()
    );
    let global = jsapi::JS::CurrentGlobalOrNull(cx.raw_cx());
    rooted!(&in(cx) let global_obj = global);
    rooted!(&in(cx) let mut err = UndefinedValue());
    let options = CompileOptionsWrapper::new(cx, CString::new("module-load").unwrap(), 1);
    let _ = evaluate_script(cx, global_obj.handle(), &src, err.handle_mut(), options);
    jsapi::JS::FinishLoadingImportedModuleFailed(cx.raw_cx(), payload, err.handle().into())
}

/// `JS::ModuleLoadHook`: test262's host for `import()`. Reads the specifier's file (relative to the referrer module's
/// file, or to the test file for scripts), compiles it as a JavaScript module or, when the request asks for
/// `type: "json"`, as a JSON module, and hands it back; a module is compiled once per global, file and type, so
/// repeated imports see the same namespace.
unsafe extern "C" fn module_load_hook(
    raw: *mut jsapi::JSContext,
    referrer: jsapi::JS::Handle<*mut jsapi::JSScript>,
    request: jsapi::JS::Handle<*mut jsapi::JSObject>,
    _host_defined: jsapi::JS::Handle<JSVal>,
    payload: jsapi::JS::Handle<JSVal>,
    _line: u32,
    _col: jsapi::JS::ColumnNumberOneOrigin,
) -> bool {
    let mut cx_box = mozjs::context::JSContext::from_ptr(NonNull::new(raw).unwrap());
    let cx = &mut cx_box;

    let spec_str = jsapi::JS::GetModuleRequestSpecifier(raw, request);
    if spec_str.is_null() {
        return hook_fail(cx, payload, "module request has no specifier");
    }
    rooted!(&in(cx) let spec_root = spec_str);
    let specifier = jsval_to_string(cx, mozjs::jsval::StringValue(&*spec_root.get()));

    // Base directory: the referrer module's file (its private), else the test's directory.
    let mut base = TEST_DIR.with(|d| d.borrow().clone());
    if !referrer.get().is_null() {
        rooted!(&in(cx) let mut private = UndefinedValue());
        mozjs_sys::glue::JS_GetScriptPrivate(referrer.get(), private.handle_mut().into());
        if private.get().is_string() {
            let file = PathBuf::from(jsval_to_string(cx, private.get()));
            if let Some(dir) = file.parent() {
                base = dir.to_path_buf();
            }
        }
    }
    let path = resolve_specifier(&base, &specifier);
    // 16.2.1.7 step 3 and 27.3.4.2 step 5: `with { type: "json" }` asks for a JSON module, a synthetic record
    // identified by the file *and* that type — the same file imported as JavaScript is a different record.
    let json = jsapi::JS::GetModuleRequestType(raw, request) == jsapi::JS::ModuleType::JSON;
    // The record of a file is one per global (see `module_key`). The engine links and evaluates whatever the hook hands
    // back (`mozjs-sys/src/engine/module.rs`, `continue_dynamic_import`), and its linker asserts a record is not already
    // evaluating, so `import()` of a module whose own evaluation is still running aborts the process.
    let named = module_key(&path);
    let key = if json {
        format!("{named}?type=json")
    } else {
        named
    };

    let global = jsapi::JS::CurrentGlobalOrNull(raw);
    rooted!(&in(cx) let global_obj = global);
    let cache = module_cache(cx, global_obj.handle());
    if cache.is_null() {
        return hook_fail(cx, payload, "cannot reach the module cache");
    }
    rooted!(&in(cx) let cache_obj = cache);
    let ckey = match CString::new(key.clone()) {
        Ok(k) => k,
        Err(_) => return hook_fail(cx, payload, "invalid module path"),
    };

    rooted!(&in(cx) let mut module_v = UndefinedValue());
    JS_GetProperty(cx, cache_obj.handle(), ckey.as_ptr(), module_v.handle_mut());
    if !module_v.get().is_object() {
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => return hook_fail(cx, payload, &format!("cannot load module {key}: {e}")),
        };
        let options = CompileOptionsWrapper::new(cx, ckey.clone(), 1);
        let mut text = mozjs::rust::transform_str_to_source_text(&source);
        rooted!(&in(cx) let module = if json {
            // 16.2.2.2: a JSON module's source is parsed as JSON, and the record is synthetic.
            jsapi::JS::CompileJsonModule1(raw, options.ptr, &mut text)
        } else {
            jsapi::JS::CompileModule1(raw, options.ptr, &mut text)
        });
        if module.get().is_null() {
            return jsapi::JS::FinishLoadingImportedModuleFailedWithPendingException(raw, payload);
        }
        if !json {
            // The module's private is its file path, read back when it is a referrer or fills its own `import.meta`.
            // A synthetic (JSON) module object has no such slot, so only a JavaScript record gets it.
            let js_path = jsapi::JS_NewStringCopyN(raw, key.as_ptr() as *const _, key.len());
            if js_path.is_null() {
                return jsapi::JS::FinishLoadingImportedModuleFailedWithPendingException(
                    raw, payload,
                );
            }
            let pv = mozjs::jsval::StringValue(&*js_path);
            jsapi::JS::SetModulePrivate(module.get(), &pv);
        }
        let record = mozjs::jsval::ObjectValue(module.get());
        if !cache_module(cx, cache_obj.handle(), &key, record) {
            return jsapi::JS::FinishLoadingImportedModuleFailedWithPendingException(raw, payload);
        }
        module_v.set(record);
    }
    rooted!(&in(cx) let module = module_v.get().to_object());
    let is_import_promise = payload.get().is_object() && {
        rooted!(&in(cx) let p = payload.get().to_object());
        jsapi::JS::IsPromiseObject(p.handle().into())
    };
    if is_import_promise {
        // `import()`: the module's own graph must be loaded before it is linked; the promise is finished from the callbacks.
        rooted!(&in(cx) let host = JS_NewPlainObject(cx));
        for (name, value) in [(c"p", payload.get()), (c"m", module_v.get())] {
            let mut v = value;
            let h = mozjs::rust::HandleValue::from_marked_location(&mut v as *mut JSVal);
            JS_DefineProperty(cx, host.handle(), name.as_ptr(), h, 0);
        }
        let mut host_v = mozjs::jsval::ObjectValue(host.get());
        let host_h = mozjs::rust::HandleValue::from_marked_location(&mut host_v as *mut JSVal);
        return jsapi::JS::LoadRequestedModules(
            raw,
            module.handle().into(),
            host_h.into(),
            Some(import_graph_loaded),
            Some(import_graph_failed),
        );
    }
    jsapi::JS::FinishLoadingImportedModule(
        raw,
        referrer,
        request,
        payload,
        module.handle().into(),
        true,
    )
}

/// `LoadRequestedModules` resolved for an `import()`: hand the module to the engine, which links and evaluates it.
unsafe extern "C" fn import_graph_loaded(
    raw: *mut jsapi::JSContext,
    host: jsapi::JS::Handle<JSVal>,
) -> bool {
    let mut cx_box = mozjs::context::JSContext::from_ptr(NonNull::new(raw).unwrap());
    let cx = &mut cx_box;
    rooted!(&in(cx) let host_obj = host.get().to_object());
    rooted!(&in(cx) let mut promise = UndefinedValue());
    rooted!(&in(cx) let mut module_v = UndefinedValue());
    JS_GetProperty(cx, host_obj.handle(), c"p".as_ptr(), promise.handle_mut());
    JS_GetProperty(cx, host_obj.handle(), c"m".as_ptr(), module_v.handle_mut());
    rooted!(&in(cx) let module = module_v.get().to_object());
    rooted!(&in(cx) let no_script: *mut jsapi::JSScript = ptr::null_mut());
    rooted!(&in(cx) let no_request: *mut jsapi::JSObject = ptr::null_mut());
    jsapi::JS::FinishLoadingImportedModule(
        raw,
        no_script.handle().into(),
        no_request.handle().into(),
        promise.handle().into(),
        module.handle().into(),
        true,
    )
}

/// `LoadRequestedModules` rejected for an `import()`: reject its promise with the error.
unsafe extern "C" fn import_graph_failed(
    raw: *mut jsapi::JSContext,
    host: jsapi::JS::Handle<JSVal>,
    error: jsapi::JS::Handle<JSVal>,
) -> bool {
    let mut cx_box = mozjs::context::JSContext::from_ptr(NonNull::new(raw).unwrap());
    let cx = &mut cx_box;
    rooted!(&in(cx) let host_obj = host.get().to_object());
    rooted!(&in(cx) let mut promise = UndefinedValue());
    JS_GetProperty(cx, host_obj.handle(), c"p".as_ptr(), promise.handle_mut());
    jsapi::JS::FinishLoadingImportedModuleFailed(raw, promise.handle().into(), error)
}

/// `JS::ModuleMetadataHook`: `import.meta` of one module (16.2.1.4 `HostGetImportMetaProperties`). Its `url` is the file
/// URL of the module's private, its path — the same string the load hook resolves the module's own requests against, so
/// every module of a graph gets its own.
unsafe extern "C" fn module_meta_hook(
    raw: *mut jsapi::JSContext,
    module: jsapi::JS::Handle<*mut jsapi::JSObject>,
    meta: jsapi::JS::Handle<*mut jsapi::JSObject>,
) -> bool {
    let mut cx_box = mozjs::context::JSContext::from_ptr(NonNull::new(raw).unwrap());
    let cx = &mut cx_box;
    rooted!(&in(cx) let mut private = UndefinedValue());
    mozjs_sys::glue::JS_GetModulePrivate(module.get(), private.handle_mut().into());
    if !private.get().is_string() {
        return true;
    }
    let url = format!("file://{}", jsval_to_string(cx, private.get()));
    rooted!(&in(cx) let s = jsapi::JS_NewStringCopyN(raw, url.as_ptr() as *const _, url.len()));
    if s.get().is_null() {
        return false;
    }
    rooted!(&in(cx) let meta_obj = meta.get());
    let mut v = mozjs::jsval::StringValue(&*s.get());
    let h = mozjs::rust::HandleValue::from_marked_location(&mut v as *mut JSVal);
    JS_DefineProperty(
        cx,
        meta_obj.handle(),
        c"url".as_ptr(),
        h,
        jsapi::JSPROP_ENUMERATE as u32,
    )
}

fn install_module_hooks(rt: &mut Runtime) {
    unsafe {
        let raw = rt.cx().raw_cx();
        let runtime = jsapi::JS_GetRuntime(raw);
        jsapi::JS::SetModuleLoadHook(runtime, Some(module_load_hook));
        jsapi::JS::SetModuleMetadataHook(runtime, Some(module_meta_hook));
    }
}

/// test262 `features:` of module syntax the engine has no production for: the parser rejects `import defer` and
/// `import source`, `using` declarations, and the loader has no bytes or text module. Their tests are skipped, never
/// silently failed.
const UNSUPPORTED_MODULE_FEATURES: [&str; 5] = [
    "import-defer",
    "source-phase-imports",
    "import-bytes",
    "import-text",
    "explicit-resource-management",
];

/// Run one test variant. `engine` is the process-wide engine.
pub fn run_job(engine: &JSEngine, job: &Job) -> TestResult {
    let path = PathBuf::from(&job.path);
    let root = PathBuf::from(&job.root);
    let result = |result: &str, message: String, features: Vec<String>| TestResult {
        path: job.path.clone(),
        variant: job.variant.clone(),
        result: result.to_string(),
        message,
        features,
    };

    let source = match read_file(&path) {
        Some(s) => s,
        None => return result("fail", "cannot read test file".to_string(), vec![]),
    };
    let fm: FrontMatter = parse_front_matter(&source).unwrap_or_default();
    let features = fm.features.clone();

    let variant = match job.variant.as_str() {
        "strict" => Variant::Strict,
        "sloppy" => Variant::Sloppy,
        "module" => Variant::Module,
        "raw" => Variant::Raw,
        other => return result("fail", format!("unknown variant {other}"), features),
    };
    if !variants_for(&fm).contains(&variant) {
        return result("skipped", "variant-not-applicable".to_string(), features);
    }
    if variant == Variant::Module {
        // A module test of a feature the engine has no syntax or loader for runs nowhere: it is disclosed, not
        // executed. Only the module variant is gated, never a script one.
        if let Some(f) = fm
            .features
            .iter()
            .find(|f| UNSUPPORTED_MODULE_FEATURES.contains(&f.as_str()))
        {
            return result("skipped", format!("feature-not-supported: {f}"), features);
        }
    }

    let is_async = fm.flags.iter().any(|f| f == "async");

    // Assemble the script: harness files, then the test source.
    let mut parts: Vec<(String, String)> = Vec::new();
    if variant != Variant::Raw {
        let harness = root.join("harness");
        let mut files = vec!["assert.js".to_string(), "sta.js".to_string()];
        if is_async {
            files.push("doneprintHandle.js".to_string());
        }
        files.extend(fm.includes.iter().cloned());
        for f in files {
            let p = harness.join(&f);
            match read_file(&p) {
                Some(src) => parts.push((src, format!("harness/{f}"))),
                None => return result("fail", format!("missing harness file {f}"), features),
            }
        }
    }
    let test_src = match variant {
        Variant::Strict => format!("\"use strict\";\n{source}"),
        _ => source.clone(),
    };
    let test_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "test.js".to_string());
    parts.push((test_src, test_name));

    PRINT_LOG.with(|log| log.borrow_mut().clear());

    let mut rt = Runtime::new(engine.handle());
    install_job_queue(rt.cx());
    install_module_hooks(&mut rt);
    TEST_DIR.with(|d| *d.borrow_mut() = path.parent().map(|p| p.to_path_buf()).unwrap_or_default());

    // Phase 1: create realm, install host bindings, evaluate everything.
    let test_error: Option<ErrorInfo> = {
        let cx = rt.cx();
        rooted!(&in(cx) let global = unsafe {
            JS_NewGlobalObject(
                cx,
                &SIMPLE_GLOBAL_CLASS,
                ptr::null_mut(),
                OnNewGlobalHookOption::FireOnNewGlobalHook,
                &*RealmOptions::default(),
            )
        });
        let mut realm = AutoRealm::new(cx, NonNull::new(global.handle().get()).unwrap());
        let cx = &mut realm;
        define_262(cx, global.handle());

        let n = parts.len();
        let mut test_error = None;
        for (i, (src, name)) in parts.iter().enumerate() {
            let is_test = i == n - 1;
            // A `flags: [module]` test runs as test262's hosts run it: the harness stays a script of the global realm
            // (so `assert` and `Test262Error` are globals, visible to the fixtures too) and only the test source is
            // the module. It gets no `"use strict"` prelude: a module is always strict (16.2.1.5).
            let outcome = if is_test && variant == Variant::Module {
                eval_module(cx, global.handle(), src, &job.path)
            } else {
                eval_part(cx, global.handle(), src, name)
            };
            if let Err(phase) = outcome {
                let mut err = take_exception(cx).unwrap_or(ErrorInfo {
                    phase,
                    name: "Error".to_string(),
                    message: "unknown".to_string(),
                    is_test262_error: false,
                });
                err.phase = phase;
                if is_test {
                    test_error = Some(err);
                } else {
                    return result(
                        "fail",
                        format!("harness {name} threw {}: {}", err.name, err.message),
                        features,
                    );
                }
                break;
            }
        }
        unsafe { jsapi::js::RunJobs(cx.raw_cx()) };
        test_error
    };

    // Phase 2: judge.
    let cx = rt.cx();
    let job_error = unsafe { JS_IsExceptionPending(cx) }.then(|| {
        take_exception(cx).unwrap_or(ErrorInfo {
            phase: "runtime",
            name: "Error".to_string(),
            message: "unknown".to_string(),
            is_test262_error: false,
        })
    });
    let err = test_error.or(job_error);

    if let Some(err) = err {
        return judge_negative(&fm, &err, is_async, &result, features);
    }

    if let Some(neg) = &fm.negative {
        return result(
            "fail",
            format!(
                "expected {} error ({}), test passed",
                neg.phase, neg.error_type
            ),
            features,
        );
    }

    if is_async {
        let log = PRINT_LOG.with(|log| log.borrow().clone());
        if let Some(fail) = log.iter().find(|l| l.contains("Test262:AsyncTestFailure")) {
            return result("fail", fail.clone(), features);
        }
        if log
            .iter()
            .filter(|l| l.as_str() == "Test262:AsyncTestComplete")
            .count()
            == 1
        {
            return result("pass", String::new(), features);
        }
        return result("fail", "async test did not complete".to_string(), features);
    }

    result("pass", String::new(), features)
}

/// Does a thrown error satisfy the test's `negative` clause? The phase as `phase_matches` maps it, the type by the `name`
/// property — except for `Test262Error`, whose class sets no `name` and which is matched by the instanceof flag instead.
fn negative_satisfied_by(neg: &Negative, err: &ErrorInfo) -> bool {
    if !phase_matches(&neg.phase, err.phase) {
        return false;
    }
    match neg.error_type.as_str() {
        "Test262Error" => err.is_test262_error,
        expected => err.name == expected,
    }
}

fn judge_negative(
    fm: &FrontMatter,
    err: &ErrorInfo,
    is_async: bool,
    result: &dyn Fn(&str, String, Vec<String>) -> TestResult,
    features: Vec<String>,
) -> TestResult {
    if is_async {
        let log = PRINT_LOG.with(|log| log.borrow().clone());
        if log.iter().any(|l| l.contains("Test262:AsyncTestFailure")) {
            if let Some(neg) = &fm.negative {
                if negative_satisfied_by(neg, err) {
                    return result("pass", String::new(), features);
                }
            }
            return result("fail", "async failure".to_string(), features);
        }
    }
    match &fm.negative {
        Some(neg) if negative_satisfied_by(neg, err) => result("pass", String::new(), features),
        Some(neg) => result(
            "fail",
            format!(
                "expected {} ({}), got {} {}: {}",
                neg.phase, neg.error_type, err.phase, err.name, err.message
            ),
            features,
        ),
        None => result(
            "fail",
            format!("uncaught {}: {}", err.name, err.message),
            features,
        ),
    }
}

// JSAPI combines parsing and early-error validation in its compile operation. A module's resolution and instantiation
// failures are the same kind of negative: `eval_module` reports both phases as `compile`, before any code has run.
fn phase_matches(expected: &str, actual: &str) -> bool {
    match expected {
        "parse" | "early" | "resolution" => actual == "compile",
        "runtime" => actual == "runtime",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_specifier;
    use std::path::Path;

    #[test]
    fn resolves_relative_to_base() {
        assert_eq!(
            resolve_specifier(Path::new("/t/dir"), "./x_FIXTURE.js"),
            Path::new("/t/dir/./x_FIXTURE.js")
        );
    }

    #[test]
    fn absolute_specifier_is_kept() {
        assert_eq!(
            resolve_specifier(Path::new("/t"), "/a/b.js"),
            Path::new("/a/b.js")
        );
    }
}
