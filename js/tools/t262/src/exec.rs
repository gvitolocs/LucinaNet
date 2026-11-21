//! Worker-side test execution against the mozjs crate API.
//!
//! One `JSEngine` per process; each test variant gets a fresh `Runtime`,
//! global object and realm.

use std::cell::{Cell, RefCell};
use std::ffi::CString;
use std::path::PathBuf;
use std::ptr::{self, NonNull};

use mozjs::jsapi::{self, JS_CallArgsFromVp, JS_GC, OnNewGlobalHookOption};
use mozjs::jsval::{JSVal, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::*;
use mozjs::rust::{
    evaluate_script, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS,
};
use serde::{Deserialize, Serialize};

use crate::meta::{parse_front_matter, variants_for, FrontMatter, Variant};

thread_local! {
    static PRINT_LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static CURRENT_GLOBAL: Cell<*mut jsapi::JSObject> = const { Cell::new(ptr::null_mut()) };
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
        let mut v = v;
        let handle = mozjs::rust::HandleValue::from_marked_location(&mut v as *mut JSVal);
        let s = mozjs::rust::ToString(cx, handle);
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
    let global = CURRENT_GLOBAL.with(|g| g.get());
    if global.is_null() {
        return false;
    }
    let filename = CString::new("evalScript").unwrap();
    let options = CompileOptionsWrapper::new(&cx_box, filename, 1);
    rooted!(&in(cx_box) let global_obj = global);
    rooted!(&in(cx_box) let mut rval = UndefinedValue());
    let ok = evaluate_script(&mut cx_box, global_obj.handle(), &src, rval.handle_mut(), options).is_ok();
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
fn define_262(cx: &mut mozjs::context::JSContext, global: mozjs::rust::Handle<*mut jsapi::JSObject>) {
    unsafe {
        rooted!(&in(cx) let d262 = JS_NewPlainObject(cx));
        rooted!(&in(cx) let agent = JS_NewPlainObject(cx));

        JS_DefineFunction(cx, global, c"print".as_ptr(), Some(native_print), 1, 0);
        JS_DefineFunction(cx, d262.handle(), c"evalScript".as_ptr(), Some(native_eval_script), 1, 0);
        JS_DefineFunction(cx, d262.handle(), c"createRealm".as_ptr(), Some(native_create_realm), 0, 0);
        JS_DefineFunction(cx, d262.handle(), c"detachArrayBuffer".as_ptr(), Some(native_detach_array_buffer), 1, 0);
        JS_DefineFunction(cx, d262.handle(), c"gc".as_ptr(), Some(native_gc), 0, 0);

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
                name: jsval_to_string(cx, name_v.get()),
                message: jsval_to_string(cx, msg_v.get()),
            }
        } else {
            ErrorInfo {
                name: "Error".to_string(),
                message: jsval_to_string(cx, exc.get()),
            }
        };
        JS_ClearPendingException(cx);
        Some(info)
    }
}

fn eval_part(
    cx: &mut mozjs::context::JSContext,
    global: mozjs::rust::Handle<*mut jsapi::JSObject>,
    source: &str,
    filename: &str,
) -> Result<(), ()> {
    let filename = CString::new(filename).unwrap_or_else(|_| CString::new("test.js").unwrap());
    let options = CompileOptionsWrapper::new(cx, filename, 1);
    rooted!(&in(cx) let mut rval = UndefinedValue());
    evaluate_script(cx, global, source, rval.handle_mut(), options)
}

fn read_file(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
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
        let mh =
            jsapi::JS::Handle::<*mut jsapi::JS::JSMicroTask>::from_marked_location(&mut mt as *mut _);
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
    if variant == Variant::Module {
        return result("skipped", "module-unsupported".to_string(), features);
    }
    if !variants_for(&fm).contains(&variant) {
        return result("skipped", "variant-not-applicable".to_string(), features);
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
        CURRENT_GLOBAL.with(|g| g.set(global.handle().get()));

        let n = parts.len();
        let mut test_error = None;
        for (i, (src, name)) in parts.iter().enumerate() {
            if eval_part(cx, global.handle(), src, name).is_err() {
                let err = take_exception(cx).unwrap_or(ErrorInfo {
                    name: "Error".to_string(),
                    message: "unknown".to_string(),
                });
                if i == n - 1 {
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
            name: "Error".to_string(),
            message: "unknown".to_string(),
        })
    });
    let err = test_error.or(job_error);

    if let Some(err) = err {
        return judge_negative(&fm, &err, is_async, &result, features);
    }

    if is_async {
        let log = PRINT_LOG.with(|log| log.borrow().clone());
        if log.iter().any(|l| l.contains("Test262:AsyncTestComplete")) {
            return result("pass", String::new(), features);
        }
        if let Some(fail) = log.iter().find(|l| l.contains("Test262:AsyncTestFailure")) {
            return result("fail", fail.clone(), features);
        }
        return result("fail", "async test did not complete".to_string(), features);
    }

    if let Some(neg) = &fm.negative {
        return result(
            "fail",
            format!("expected {} error ({}), test passed", neg.phase, neg.error_type),
            features,
        );
    }
    result("pass", String::new(), features)
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
                if err.name == neg.error_type {
                    return result("pass", String::new(), features);
                }
            }
            return result("fail", "async failure".to_string(), features);
        }
    }
    match &fm.negative {
        Some(neg) if err.name == neg.error_type => result("pass", String::new(), features),
        Some(neg) => result(
            "fail",
            format!(
                "expected {} ({}), got {}: {}",
                neg.phase, neg.error_type, err.name, err.message
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
