//! Worker side: one WPT `.any.js` test in the Brimstone engine used as a JS shell.
//!
//! The engine is created the way `tools/lucinanet-test262` creates it: one process-wide
//! `JSEngine`, a fresh `Runtime` and global object per test, the Rust job queue that
//! SpiderMonkey needs before it will drain microtasks, and scripts evaluated as plain
//! global code. What is added here is the shell environment `testharness.js` selects
//! when the global is neither a document nor a worker, and a driver loop that runs jobs
//! and timers until the harness reports completion or the wall clock runs out.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::{Duration, Instant};

use mozjs::context::JSContext;
use mozjs::jsapi::{self, JS_CallArgsFromVp, OnNewGlobalHookOption};
use mozjs::jsval::{JSVal, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::*;
use mozjs::rust::{CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};
use serde_json::{json, Value};

use crate::meta::Meta;

/// Evaluated before `testharness.js`: the globals it reads to choose its environment.
const SHELL_ENV: &str = include_str!("shell_env.js");
/// Evaluated after `testharness.js`: the `add_*_callback` hooks that collect results.
const SHELL_CALLBACKS: &str = include_str!("shell_callbacks.js");

/// `Test.statuses` in `testharness.js:2779-2785`, in code order.
pub const STATUS_NAMES: [&str; 5] = ["PASS", "FAIL", "TIMEOUT", "NOTRUN", "PRECONDITION_FAILED"];

/// `TestsStatus.statuses` in `testharness.js:3602-3607`, in code order.
const HARNESS_NAMES: [&str; 4] = ["OK", "ERROR", "TIMEOUT", "PRECONDITION_FAILED"];

/// One test file as the runner saw it.
pub struct Outcome {
    /// `ran` (the harness reported completion), `timeout` (the wall clock ended the run)
    /// or `harness-error` (a harness or `script=` file failed, so nothing could run).
    pub result: &'static str,
    /// Why, when the result is not `ran`.
    pub reason: String,
    /// The harness status name, or `NONE` when completion never arrived.
    pub harness: String,
    /// Its message, when the harness reported one.
    pub harness_message: String,
    pub secs: f64,
}

/// The harness pair for a run that never reached `add_completion_callback`.
fn no_harness() -> (String, String) {
    ("NONE".to_string(), String::new())
}

thread_local! {
    /// Path of the test being run, relative to the WPT root: stamped on every record.
    static REL: RefCell<String> = const { RefCell::new(String::new()) };
    /// Subtest records by harness index; the last report for an index wins.
    static SUBTESTS: RefCell<BTreeMap<usize, Value>> = const { RefCell::new(BTreeMap::new()) };
    /// The payload of `add_completion_callback`, when it fired.
    static COMPLETION: RefCell<Option<Value>> = const { RefCell::new(None) };
    /// Runner-side faults that are not test results: a timer callback that threw.
    static ERRORS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// Set by `__wptComplete`: the driver loop stops on it.
    static DONE: Cell<bool> = const { Cell::new(false) };
    /// Index source for records that carry no harness index.
    static SPARE: Cell<usize> = const { Cell::new(1_000_000) };
}

fn jsval_to_string(cx: &mut JSContext, v: JSVal) -> String {
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

/// The `message` of a record: the harness message, else its stack, else nothing.
fn clip(text: &str) -> String {
    const LIMIT: usize = 400;
    let one = text.trim();
    if one.len() <= LIMIT {
        return one.to_string();
    }
    let end = one
        .char_indices()
        .nth(LIMIT)
        .map(|(i, _)| i)
        .unwrap_or(one.len());
    format!("{}…", &one[..end])
}

unsafe extern "C" fn native_log(
    cx: *mut jsapi::JSContext,
    argc: u32,
    vp: *mut jsapi::JS::Value,
) -> bool {
    let args = JS_CallArgsFromVp(argc, vp);
    let mut cx_box = JSContext::from_ptr(std::ptr::NonNull::new(cx).unwrap());
    let level = if argc > 0 {
        jsval_to_string(&mut cx_box, *args.get(0))
    } else {
        "log".to_string()
    };
    let text = if argc > 1 {
        jsval_to_string(&mut cx_box, *args.get(1))
    } else {
        String::new()
    };
    // Test output goes to stderr: stdout carries only the JSONL the parent relays.
    eprintln!("[{level}] {text}");
    args.rval().set(UndefinedValue());
    true
}

/// `__wptResult(json)`: one subtest as the harness reported it.
unsafe extern "C" fn native_result(
    cx: *mut jsapi::JSContext,
    argc: u32,
    vp: *mut jsapi::JS::Value,
) -> bool {
    let args = JS_CallArgsFromVp(argc, vp);
    let mut cx_box = JSContext::from_ptr(std::ptr::NonNull::new(cx).unwrap());
    let text = if argc > 0 {
        jsval_to_string(&mut cx_box, *args.get(0))
    } else {
        String::new()
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => record(&value),
        Err(e) => ERRORS.with(|e2| e2.borrow_mut().push(format!("bad result payload: {e}"))),
    }
    args.rval().set(UndefinedValue());
    true
}

/// `__wptComplete(json)`: the harness is done; the driver loop takes this as its cue.
unsafe extern "C" fn native_complete(
    cx: *mut jsapi::JSContext,
    argc: u32,
    vp: *mut jsapi::JS::Value,
) -> bool {
    let args = JS_CallArgsFromVp(argc, vp);
    let mut cx_box = JSContext::from_ptr(std::ptr::NonNull::new(cx).unwrap());
    let text = if argc > 0 {
        jsval_to_string(&mut cx_box, *args.get(0))
    } else {
        String::new()
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => {
            for test in value.get("tests").and_then(Value::as_array).into_iter().flatten() {
                record(test);
            }
            COMPLETION.with(|c| *c.borrow_mut() = Some(value));
            DONE.with(|d| d.set(true));
        }
        Err(e) => ERRORS.with(|e2| e2.borrow_mut().push(format!("bad completion payload: {e}"))),
    }
    args.rval().set(UndefinedValue());
    true
}

/// `__wptError(text)`: a runner fault, recorded against the file and never a subtest.
unsafe extern "C" fn native_error(
    cx: *mut jsapi::JSContext,
    argc: u32,
    vp: *mut jsapi::JS::Value,
) -> bool {
    let args = JS_CallArgsFromVp(argc, vp);
    let mut cx_box = JSContext::from_ptr(std::ptr::NonNull::new(cx).unwrap());
    let text = if argc > 0 {
        jsval_to_string(&mut cx_box, *args.get(0))
    } else {
        String::new()
    };
    ERRORS.with(|e| e.borrow_mut().push(clip(&text)));
    args.rval().set(UndefinedValue());
    true
}

/// Store one subtest record from the JSON the harness glue sent.
fn record(test: &Value) {
    let index = match test.get("index").and_then(Value::as_u64) {
        Some(i) => i as usize,
        None => SPARE.with(|s| {
            let i = s.get();
            s.set(i + 1);
            i
        }),
    };
    let status = test.get("status").and_then(Value::as_i64);
    let phase = test.get("phase").and_then(Value::as_i64);
    let message = match test.get("message") {
        Some(Value::Null) | None => String::new(),
        Some(v) => clip(v.as_str().unwrap_or_default()),
    };
    SUBTESTS.with(|map| {
        map.borrow_mut().insert(
            index,
            json!({
                "name": test.get("name").and_then(Value::as_str).unwrap_or(""),
                "status": status_name(status, phase),
                "message": message,
            }),
        )
    });
}

/// `Test.statuses`, with the WPT convention for a test that was started but never
/// given a result: the run ended under it, which is a TIMEOUT, while a test that never
/// started at all stays NOTRUN.
fn status_name(status: Option<i64>, phase: Option<i64>) -> &'static str {
    // `Test` is created with status NOTRUN (testharness.js:2733) and only a result
    // changes it, so NOTRUN alone does not say whether the test ran. A phase past
    // STARTED that is not HAS_RESULT means it started and never settled: that is the
    // TIMEOUT the wall clock caused. `complete` force-finishes a test that never started
    // by moving it straight to HAS_RESULT with NOTRUN left in place, which is why
    // HAS_RESULT is not on the list.
    if status.unwrap_or(3) == 3 {
        return match phase {
            Some(1) | Some(3) => "TIMEOUT",
            _ => "NOTRUN",
        };
    }
    match status {
        Some(c) if (0..STATUS_NAMES.len() as i64).contains(&c) => STATUS_NAMES[c as usize],
        _ => "NOTRUN",
    }
}

fn harness_name(status: Option<i64>) -> &'static str {
    match status {
        Some(c) if (0..HARNESS_NAMES.len() as i64).contains(&c) => HARNESS_NAMES[c as usize],
        _ => "UNKNOWN",
    }
}

struct ErrorInfo {
    name: String,
    message: String,
}

fn take_exception(cx: &mut JSContext) -> Option<ErrorInfo> {
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

/// `Name: message` for an exception, or `name` alone when there is no message.
fn error_text(error: &ErrorInfo) -> String {
    if error.message.is_empty() {
        error.name.clone()
    } else {
        clip(&format!("{}: {}", error.name, error.message))
    }
}

/// Why a part of a run did not complete. Whether the engine refused to compile the file
/// or the file threw decides if anything is left to wait for: a file that never parsed
/// registered no tests, so its run is over the moment it is reported.
enum Failure {
    Parse(String),
    Runtime(String),
}

impl Failure {
    fn text(&self) -> &str {
        match self {
            Failure::Parse(text) | Failure::Runtime(text) => text,
        }
    }
}

/// Compile `source` as global code in the realm of `global`, run it, and return its
/// completion value as text - `None` when the value is `undefined`, which is what every
/// script part of a test evaluates to. The filename is what the harness records in the
/// stacks it captures, so a failure names the file that actually threw.
fn exec_script(
    cx: &mut JSContext,
    global: mozjs::rust::Handle<*mut jsapi::JSObject>,
    source: &str,
    filename: &str,
) -> Result<Option<String>, Failure> {
    let c_name = CString::new(filename).unwrap_or_else(|_| CString::new("test.js").unwrap());
    let options = CompileOptionsWrapper::new(cx, c_name, 1);
    let mut realm = AutoRealm::new_from_handle(cx, global);
    let cx = &mut realm;
    rooted!(&in(cx) let mut rval = UndefinedValue());
    let mut text = mozjs::rust::transform_str_to_source_text(source);
    rooted!(&in(cx) let script =
        unsafe { jsapi::JS::Compile1(cx.raw_cx(), options.ptr, &mut text) });
    if script.get().is_null() {
        return Err(Failure::Parse(format!("{filename}: {}", exception_text(cx))));
    }
    if !unsafe { JS_ExecuteScript(cx, script.handle(), rval.handle_mut()) } {
        return Err(Failure::Runtime(format!("{filename}: {}", exception_text(cx))));
    }
    if rval.get().is_undefined() {
        return Ok(None);
    }
    Ok(Some(jsval_to_string(cx, rval.get())))
}

/// `Name: message` of the pending exception, which is cleared either way.
fn exception_text(cx: &mut JSContext) -> String {
    take_exception(cx)
        .map(|error| error_text(&error))
        .unwrap_or_else(|| "no exception was reported".to_string())
}

/// The number an expression produced, or -1 when it did not produce one. A driver
/// expression that throws is a runner fault, and -1 makes the loop fall back to
/// polling instead of stopping.
fn driver_number(
    cx: &mut JSContext,
    global: mozjs::rust::Handle<*mut jsapi::JSObject>,
    source: &str,
) -> f64 {
    match exec_script(cx, global, source, "driver") {
        Ok(Some(text)) => text.trim().parse().unwrap_or(-1.0),
        _ => -1.0,
    }
}

/// The JSON an expression produced, or nothing when it did not produce a string.
fn driver_json(
    cx: &mut JSContext,
    global: mozjs::rust::Handle<*mut jsapi::JSObject>,
    source: &str,
) -> Option<Value> {
    match exec_script(cx, global, source, "driver") {
        Ok(Some(text)) => serde_json::from_str(&text).ok(),
        _ => None,
    }
}


// SpiderMonkey needs an embedder job queue before `js::RunJobs` will drain the
// microtask queue; the trap below drains regular microtasks.
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

fn install_job_queue(cx: &mut JSContext) {
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

/// The test's `script=` entries, in order, each resolved against the WPT root when it
/// starts with `/` and against the test's directory otherwise.
pub fn script_paths(wpt: &Path, test: &Path, meta: &Meta) -> Vec<PathBuf> {
    let dir = test.parent().map(Path::to_path_buf).unwrap_or_default();
    meta.scripts
        .iter()
        .map(|s| match s.strip_prefix('/') {
            Some(rel) => wpt.join(rel),
            None => dir.join(s),
        })
        .collect()
}

/// `META_TITLE` and `location`, the two globals `get_title()` reads
/// (`testharness.js:5013-5035`). Without a location a test that calls `test(fn)` with
/// no name would be reported as `Untitled` instead of after its file.
fn title_prelude(rel: &str, meta: &Meta) -> String {
    let title = meta.title.clone().unwrap_or_default();
    let pathname = format!("/{rel}");
    let href = format!("file://{pathname}");
    format!(
        "self.META_TITLE = {};\n\
         Object.defineProperty(self, \"location\", {{\n\
        \x20 value: {{ pathname: {}, href: {}, protocol: \"file:\", search: \"\", hash: \"\" }},\n\
        \x20 writable: true, configurable: true, enumerable: true,\n\
         }});\n",
        json!(title),
        json!(pathname),
        json!(href),
    )
}

/// Run one jsshell test file. Returns the subtest records (in harness index order) and
/// the file's outcome; the caller turns them into JSONL.
pub fn run_file(
    engine: &JSEngine,
    wpt: &Path,
    test: &Path,
    meta: &Meta,
    timeout: Duration,
) -> (Vec<(usize, Value)>, Outcome) {
    let rel = test
        .strip_prefix(wpt)
        .unwrap_or(test)
        .to_string_lossy()
        .replace('\\', "/");
    REL.with(|r| *r.borrow_mut() = rel.clone());
    SUBTESTS.with(|s| s.borrow_mut().clear());
    COMPLETION.with(|c| *c.borrow_mut() = None);
    ERRORS.with(|e| e.borrow_mut().clear());
    DONE.with(|d| d.set(false));
    SPARE.with(|s| s.set(1_000_000));

    let outcome = |result: &'static str, reason: String, harness: (String, String), secs: f64| {
        Outcome {
            result,
            reason,
            harness: harness.0,
            harness_message: harness.1,
            secs,
        }
    };

    let source = match std::fs::read_to_string(test) {
        Ok(s) => s,
        Err(e) => {
            return (
                vec![],
                outcome("harness-error", format!("cannot read test: {e}"), no_harness(), 0.0),
            )
        }
    };

    // The harness every WPT test needs, then the `script=` files, then the test.
    let harness_path = wpt.join("resources/testharness.js");
    let harness = match std::fs::read_to_string(&harness_path) {
        Ok(s) => s,
        Err(e) => {
            return (
                vec![],
                outcome(
                    "harness-error",
                    format!("cannot read {}: {e}", harness_path.display()),
                    no_harness(),
                    0.0,
                ),
            )
        }
    };
    let mut parts: Vec<(&str, String, String)> = vec![
        ("shell environment", SHELL_ENV.to_string(), "shell_env.js".to_string()),
        ("metadata", title_prelude(&rel, meta), "shell_title.js".to_string()),
        ("testharness.js", harness, "resources/testharness.js".to_string()),
        ("result hooks", SHELL_CALLBACKS.to_string(), "shell_callbacks.js".to_string()),
    ];
    for path in script_paths(wpt, test, meta) {
        let name = path.strip_prefix(wpt).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        match std::fs::read_to_string(&path) {
            Ok(src) => parts.push(("script", src, name)),
            Err(e) => {
                return (
                    vec![],
                    outcome(
                        "harness-error",
                        format!("cannot read script {name}: {e}"),
                        no_harness(),
                        0.0,
                    ),
                )
            }
        }
    }
    parts.push(("test", source, rel.clone()));

    let start = Instant::now();
    let mut rt = Runtime::new(engine.handle());
    install_job_queue(rt.cx());

    let (file_error, timed_out) = {
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
        let mut realm = AutoRealm::new(cx, std::ptr::NonNull::new(global.handle().get()).unwrap());
        let cx = &mut realm;
        define_shell_natives(cx, global.handle());

        let mut file_error = None;
        let mut keep_driving = true;
        for (kind, src, name) in parts.iter() {
            let Err(failure) = exec_script(cx, global.handle(), src, name) else {
                continue;
            };
            let text = failure.text().to_string();
            if *kind != "script" && *kind != "test" {
                return (
                    records(),
                    outcome(
                        "harness-error",
                        format!("{kind} {name} threw {text}"),
                        harness_status(),
                        start.elapsed().as_secs_f64(),
                    ),
                );
            }
            // One of the test's own files failed. The harness is intact, so every
            // subtest registered before this point keeps its result, and the reason
            // says which file the engine could not run - an engine gap, not a fault
            // here. Nothing after it is evaluated: the test body of a file whose
            // helpers are missing would only fail again.
            file_error = Some(if *kind == "script" {
                format!("script {name} threw {text}")
            } else {
                text
            });
            // A file that did not parse registered no tests, so there is no completion
            // to wait for and the driver loop would only burn the timeout.
            keep_driving = !matches!(failure, Failure::Parse(_));
            break;
        }

        let mut timed_out = false;
        let mut wait = keep_driving;
        if wait && file_error.is_some() {
            // A file that failed gets asked whether anything is left before the loop is
            // run at all: a parse error or a throwing `script=` leaves no test
            // registered, and spending the whole timeout waiting for a completion that
            // cannot arrive is not information. The queue is drained once first, because
            // a test file may register its tests from a promise continuation.
            unsafe { jsapi::js::RunJobs(cx.raw_cx()) };
            wait = registered(cx, global.handle());
        }
        if wait {
            drive(cx, global.handle(), timeout, start);
            timed_out = !DONE.with(|d| d.get());
            if timed_out {
                // The run ended without the harness reporting: ask it what it knows. This
                // is the only way a test that started but never finished becomes a TIMEOUT
                // instead of disappearing from the results.
                if let Some(value) = snapshot(cx, global.handle()) {
                    for test in value
                        .get("tests")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        record(test);
                    }
                }
            }
            if let Some(error) = take_exception(cx) {
                ERRORS.with(|e| e.borrow_mut().push(error_text(&error)));
            }
        }
        (file_error, timed_out)
    };

    let secs = start.elapsed().as_secs_f64();
    let result = if file_error.is_some() {
        "error"
    } else if timed_out {
        "timeout"
    } else {
        "ran"
    };
    let reason = file_error.unwrap_or_else(|| {
        if timed_out {
            format!("no completion after {} s", timeout.as_secs())
        } else {
            String::new()
        }
    });
    (
        records(),
        outcome(result, reason, harness_status(), secs),
    )
}

fn records() -> Vec<(usize, Value)> {
    SUBTESTS.with(|s| s.borrow().iter().map(|(k, v)| (*k, v.clone())).collect())
}

/// Runner faults collected during the file: text for the file record, never a subtest.
pub fn runner_errors() -> Vec<String> {
    ERRORS.with(|e| e.borrow().clone())
}

/// Status name and message of the harness itself, taken from `add_completion_callback`.
/// A harness `ERROR` carries the reason - duplicate test names, for instance - that no
/// subtest record would otherwise explain.
fn harness_status() -> (String, String) {
    let text = COMPLETION.with(|c| c.borrow().clone());
    let harness = text
        .as_ref()
        .and_then(|v| v.get("harness"))
        .map(|harness| harness.clone());
    let Some(harness) = harness else {
        return ("NONE".to_string(), String::new());
    };
    let name = harness_name(harness.get("status").and_then(Value::as_i64)).to_string();
    let message = match harness.get("message") {
        Some(Value::Null) | None => String::new(),
        Some(v) => clip(v.as_str().unwrap_or_default()),
    };
    (name, message)
}

/// The natives the shell glue calls. `print`/`console` are defined in JS on top of
/// `__wptLog`, so they can format their arguments there.
fn define_shell_natives(cx: &mut JSContext, global: mozjs::rust::Handle<*mut jsapi::JSObject>) {
    unsafe {
        JS_DefineFunction(cx, global, c"__wptLog".as_ptr(), Some(native_log), 2, 0);
        JS_DefineFunction(
            cx,
            global,
            c"__wptResult".as_ptr(),
            Some(native_result),
            1,
            0,
        );
        JS_DefineFunction(
            cx,
            global,
            c"__wptComplete".as_ptr(),
            Some(native_complete),
            1,
            0,
        );
        JS_DefineFunction(cx, global, c"__wptError".as_ptr(), Some(native_error), 1, 0);
    }
}

/// Run jobs and timers until the harness reports completion or `timeout` is spent.
fn drive(
    cx: &mut JSContext,
    global: mozjs::rust::Handle<*mut jsapi::JSObject>,
    timeout: Duration,
    start: Instant,
) {
    loop {
        unsafe { jsapi::js::RunJobs(cx.raw_cx()) };
        if DONE.with(|d| d.get()) {
            return;
        }
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return;
        }
        let ran = driver_number(cx, global, "__wptRunTimers()");
        if ran > 0.0 {
            continue;
        }
        let next = driver_number(cx, global, "__wptNextTimer()");
        let nap = if next < 0.0 {
            Duration::from_millis(5)
        } else {
            Duration::from_millis(next.max(0.0) as u64).min(Duration::from_millis(200))
        };
        let left = timeout.saturating_sub(elapsed);
        if nap < left {
            std::thread::sleep(nap);
        } else {
            std::thread::sleep(left);
        }
    }
}

/// Whether the harness has a test registered, which is what decides whether there is a
/// completion worth waiting for.
fn registered(
    cx: &mut JSContext,
    global: mozjs::rust::Handle<*mut jsapi::JSObject>,
) -> bool {
    snapshot(cx, global)
        .and_then(|value| {
            value
                .get("tests")
                .and_then(Value::as_array)
                .map(|tests| !tests.is_empty())
        })
        .unwrap_or(false)
}

/// The list of tests the harness knows about, for the case where it never completed.
pub fn snapshot(
    cx: &mut JSContext,
    global: mozjs::rust::Handle<*mut jsapi::JSObject>,
) -> Option<Value> {
    driver_json(cx, global, "__wptSnapshot()")
}

/// Status counts of a set of subtest records, in `STATUS_NAMES` order.
pub fn counts(subtests: &[(usize, Value)]) -> Vec<usize> {
    let mut counts = vec![0usize; STATUS_NAMES.len()];
    for (_, record) in subtests {
        if let Some(pos) = record
            .get("status")
            .and_then(Value::as_str)
            .and_then(|s| STATUS_NAMES.iter().position(|n| *n == s))
        {
            counts[pos] += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::status_name;

    #[test]
    fn a_reported_status_is_the_status() {
        assert_eq!(status_name(Some(0), Some(4)), "PASS");
        assert_eq!(status_name(Some(1), Some(2)), "FAIL");
        assert_eq!(status_name(Some(2), Some(1)), "TIMEOUT");
        assert_eq!(status_name(Some(4), Some(0)), "PRECONDITION_FAILED");
        assert_eq!(status_name(Some(9), Some(0)), "NOTRUN");
    }

    #[test]
    fn notrun_separates_a_hang_from_a_test_that_was_never_reached() {
        // Started, never settled: the wall clock ended the run under it.
        assert_eq!(status_name(Some(3), Some(1)), "TIMEOUT");
        assert_eq!(status_name(Some(3), Some(3)), "TIMEOUT");
        // `complete` moves an unstarted test to HAS_RESULT with NOTRUN in place, and a
        // test that was never reached stays INITIAL; neither is a timeout.
        assert_eq!(status_name(Some(3), Some(0)), "NOTRUN");
        assert_eq!(status_name(Some(3), Some(2)), "NOTRUN");
        assert_eq!(status_name(Some(3), Some(4)), "NOTRUN");
        assert_eq!(status_name(None, None), "NOTRUN");
    }
}
