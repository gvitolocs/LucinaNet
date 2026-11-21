/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `JS::GetScriptedCallerPrivate` and `JS_GetFunctionDisplayId`, which were `missing()` stubs
//! (mozjs-sys/src/engine/caller_private.rs, mozjs-sys/src/engine/function.rs) and so aborted the browser process on the
//! first page that reached them.
//!
//! The private is how Servo finds the script a `setTimeout`/`setInterval` string came from, to run the callback with that
//! script's base URL and fetch options (`components/script/timers.rs`, `active_script_fetch_info`): it stores an
//! `Rc<BaseScript>` as a `PrivateValue` with `JS::SetScriptPrivate` after `JS::Compile` and reads it back from the native
//! the timer runs, which is the path these tests take. The display name is how Servo names a function in a console
//! message (`components/script/dom/console.rs:505`).
//!
//! Each test inits the engine, so they run one at a time (`.cargo/config.toml` pins RUST_TEST_THREADS=1). Where the fork
//! gives the glue no way to know what the C++ would know — the script a frame belongs to, a display atom rather than a `name`
//! property — the test says so and asserts what this engine answers, so that the difference recorded in
//! `mozjs-sys/src/engine/caller_private.rs` and `mozjs-sys/src/engine/function.rs` is a measurement and not a claim.

use std::cell::RefCell;
use std::ffi::{c_void, CString};
use std::ptr::{self, NonNull};

use mozjs::conversions::jsstr_to_string;
use mozjs::context::{JSContext, RawJSContext};
use mozjs::gc::HandleObject;
use mozjs::jsapi::JS;
use mozjs::jsapi::{GCReason, JSString, OnNewGlobalHookOption, Value, JSPROP_ENUMERATE};
use mozjs::jsval::{Int32Value, PrivateValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{
    Compile1, JS_ClearPendingException, JS_DefineFunction, JS_DefineProperty, JS_ExecuteScript, JS_GetFunctionDisplayId,
    JS_GetFunctionId, JS_GetPendingException, JS_GetScriptedCallerPrivate, JS_IsExceptionPending, JS_NewGlobalObject,
    JS_ValueToFunction, JS_GC,
};
use mozjs::rust::{transform_str_to_source_text, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};

thread_local! {
    /// What each `callerPrivate()` call answered, in order.
    static SEEN: RefCell<Vec<String>> = RefCell::new(Vec::new());
}

fn seen() -> Vec<String> {
    SEEN.with(|s| s.borrow().clone())
}

/// A value as a string, so that a test can tell 101 from `undefined` from a private pointer.
fn describe(v: &Value) -> String {
    if v.is_undefined() {
        "undefined".to_owned()
    } else if v.is_null() {
        "null".to_owned()
    } else if v.is_int32() {
        format!("int({})", v.to_int32())
    } else if v.is_double() {
        // A `PrivateValue` is double-tagged here, which is the shape Servo stores its `Rc<BaseScript>` in.
        format!("private({:#x})", v.to_private() as usize)
    } else {
        format!("other({:x})", v.asBits_)
    }
}

/// What a timer callback is: a native that asks the engine whose script is running.
unsafe extern "C" fn caller_private(cx: *mut RawJSContext, _argc: u32, vp: *mut Value) -> bool {
    rooted!(in(cx) let mut dest = UndefinedValue());
    unsafe { mozjs::glue::JS_GetScriptedCallerPrivate(cx, dest.handle_mut().into()) };
    let answer = describe(&dest.get());
    SEEN.with(|seen| seen.borrow_mut().push(answer));
    unsafe { *vp = dest.get() };
    true
}

/// Runs `f` in a fresh engine with a simple global, its realm entered and `callerPrivate` defined on it.
fn with_global(f: impl FnOnce(&mut JSContext, HandleObject)) {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    SEEN.with(|s| s.borrow_mut().clear());
    unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, global.handle());
        let cx = &mut realm;
        assert!(
            !JS_DefineFunction(cx, global.handle(), c"callerPrivate".as_ptr(), Some(caller_private), 0, 0).is_null(),
            "callerPrivate was not defined"
        );
        f(cx, global.handle());
    }
}

/// Servo's path for a classic script of `file`: `JS::Compile`, `JS::SetScriptPrivate`, `JS_ExecuteScript`.
fn servo_run(cx: &mut JSContext, file: &str, src: &str, private: Option<Value>) {
    let options = CompileOptionsWrapper::new(cx, CString::new(file).unwrap(), 1);
    let mut text = transform_str_to_source_text(src);
    unsafe {
        let raw = Compile1(cx, options.ptr, &mut text);
        assert!(!raw.is_null(), "{file} did not compile");
        rooted!(&in(cx) let script = raw);
        if let Some(p) = private {
            JS::SetScriptPrivate(script.get(), &p);
        }
        rooted!(&in(cx) let mut rval = UndefinedValue());
        assert!(JS_ExecuteScript(cx, script.handle(), rval.handle_mut()), "{file} threw: {src}");
    }
}

/// `src` as its own `JS::Evaluate` script (what a bookmarklet, or Servo's `evaluate_script`, does).
fn eval(cx: &mut JSContext, global: HandleObject, src: &str) {
    assert!(eval_value(cx, global, src).is_some(), "{src} threw");
}

/// `src` evaluated as a one-step script of `file` — a bookmarklet, or Servo's `evaluate_script`, which names the document —
/// its completion value described; `None` if it threw.
fn eval_at(cx: &mut JSContext, global: HandleObject, file: &str, src: &str) -> Option<String> {
    let options = CompileOptionsWrapper::new(cx, CString::new(file).unwrap(), 1);
    rooted!(&in(cx) let mut rval = UndefinedValue());
    if mozjs::rust::evaluate_script(cx, global, src, rval.handle_mut(), options).is_err() {
        return None;
    }
    Some(describe(&rval.get()))
}

/// `src` evaluated, its completion value described; `None` if it threw.
fn eval_value(cx: &mut JSContext, global: HandleObject, src: &str) -> Option<String> {
    eval_at(cx, global, "check.js", src)
}

fn eval_bool(cx: &mut JSContext, global: HandleObject, src: &str) -> bool {
    let options = CompileOptionsWrapper::new(cx, c"check.js".to_owned(), 1);
    rooted!(&in(cx) let mut rval = UndefinedValue());
    assert!(mozjs::rust::evaluate_script(cx, global, src, rval.handle_mut(), options).is_ok(), "{src} threw");
    rval.get().to_boolean()
}

fn eval_str(cx: &mut JSContext, global: HandleObject, src: &str) -> String {
    let options = CompileOptionsWrapper::new(cx, c"check.js".to_owned(), 1);
    rooted!(&in(cx) let mut rval = UndefinedValue());
    assert!(mozjs::rust::evaluate_script(cx, global, src, rval.handle_mut(), options).is_ok(), "{src} threw");
    assert!(!rval.get().to_string().is_null(), "{src} did not yield a string");
    unsafe { jsstr_to_string(cx, NonNull::new(rval.get().to_string()).unwrap()) }
}

/// The `JS_GetFunctionDisplayId` of the function `path` holds (`JS_GetFunctionId`, with `display` false). A null
/// out-parameter reads back as `"(null)"`.
fn name_of(cx: &mut JSContext, global: HandleObject, path: &str, display: bool) -> String {
    let options = CompileOptionsWrapper::new(cx, c"fun.js".to_owned(), 1);
    rooted!(&in(cx) let mut v = UndefinedValue());
    assert!(mozjs::rust::evaluate_script(cx, global, path, v.handle_mut(), options).is_ok(), "{path} threw");
    rooted!(&in(cx) let fun = unsafe { JS_ValueToFunction(cx, v.handle()) });
    assert!(!fun.get().is_null(), "{path} does not hold a function: {}", describe(&v.get()));
    rooted!(&in(cx) let mut name = ptr::null_mut::<JSString>());
    let ok = if display {
        unsafe { JS_GetFunctionDisplayId(cx, fun.handle(), name.handle_mut()) }
    } else {
        unsafe { JS_GetFunctionId(cx, fun.handle(), name.handle_mut()) }
    };
    assert!(ok, "the name of {path} could not be read");
    if name.get().is_null() {
        "(null)".to_owned()
    } else {
        unsafe { jsstr_to_string(cx, NonNull::new(name.get()).unwrap()) }
    }
}

/// The display name, the `JS_GetFunctionId` name and the `name` property of `path`: all three from one source, and equal
/// to `want`.
fn check_name(cx: &mut JSContext, global: HandleObject, program: &str, path: &str, want: &str) {
    eval(cx, global, program);
    assert_eq!(name_of(cx, global, path, true), want, "display name: {program}");
    assert_eq!(name_of(cx, global, path, false), want, "function id: {program}");
    assert_eq!(eval_str(cx, global, &format!("String({path}.name)")), want, "name property: {program}");
}

// ---- JS::GetScriptedCallerPrivate ----

#[test]
fn the_private_of_the_running_script_reaches_the_native() {
    with_global(|cx, global| {
        servo_run(cx, "page.html", "globalThis.seen = callerPrivate();", Some(Int32Value(101)));
        assert_eq!(seen(), ["int(101)"]);
        assert!(eval_bool(cx, global, "seen === 101"), "the value did not survive the trip into the script");
    });
}

#[test]
fn it_is_the_pointer_servo_stored() {
    // Servo stores `Rc::into_raw(Rc<BaseScript>))` as a `PrivateValue`; the timer reads the base URL and fetch options out
    // of that pointer, so it has to come back unchanged.
    let base_script = Box::leak(Box::new(0x5bd5_c0de_usize)) as *const usize;
    with_global(move |cx, _global| {
        let private = PrivateValue(base_script as *const c_void);
        servo_run(cx, "page.html", "callerPrivate();", Some(private));
        assert_eq!(seen(), [format!("private({:#x})", base_script as usize)]);
    });
}

#[test]
fn a_script_that_was_never_given_a_private_answers_undefined() {
    with_global(|cx, global| {
        servo_run(cx, "plain.html", "globalThis.seen = callerPrivate();", None);
        assert_eq!(seen(), ["undefined"]);
        assert!(eval_bool(cx, global, "seen === undefined"));
    });
}

#[test]
fn with_no_script_on_the_stack_it_answers_undefined() {
    // Servo calls it from the host too, where there is no frame to read; the C++ writes `UndefinedValue()` there.
    with_global(|cx, _global| {
        rooted!(&in(cx) let mut dest = UndefinedValue());
        unsafe { JS_GetScriptedCallerPrivate(cx, dest.handle_mut()) };
        assert_eq!(describe(&dest.get()), "undefined");
    });
}

#[test]
fn a_nested_call_answers_with_the_topmost_scripted_frame() {
    with_global(|cx, _global| {
        // b.js defines the callee and a.js calls it. `NonBuiltinFrameIter` stops at b.js's frame, so the answer is b.js's
        // private and not the private of a.js, whose script is the one running.
        servo_run(cx, "b.js", "function fromB() { return callerPrivate(); }", Some(Int32Value(303)));
        servo_run(cx, "a.js", "fromB();", Some(Int32Value(101)));
        assert_eq!(seen(), ["int(303)"]);

        // The same the other way round: the caller has a private, the script the called function lives in has none.
        servo_run(cx, "d.js", "function fromD() { return callerPrivate(); }", None);
        servo_run(cx, "c.js", "fromD();", Some(Int32Value(404)));
        assert_eq!(seen(), ["int(303)", "undefined"]);

        // A function defined and called in the one script answers with that script's own private.
        servo_run(cx, "e.js", "function inner() { return callerPrivate(); } inner();", Some(Int32Value(505)));
        assert_eq!(seen(), ["int(303)", "undefined", "int(505)"]);
    });
}

#[test]
fn three_deep_calls_answer_with_the_innermost_script() {
    with_global(|cx, _global| {
        servo_run(cx, "h.js", "function fromH() { return callerPrivate(); }", Some(Int32Value(808)));
        servo_run(cx, "g.js", "function viaG() { return fromH(); }", Some(Int32Value(707)));
        servo_run(cx, "f.js", "viaG();", Some(Int32Value(606)));
        assert_eq!(seen(), ["int(808)"]);
    });
}

#[test]
fn the_private_follows_the_script_when_the_collector_moves_it() {
    // The table is keyed by the script address and Brimstone moves every heap item at each collection, so the key is
    // traced with it.
    with_global(|cx, _global| {
        let options = CompileOptionsWrapper::new(cx, c"moved.js".to_owned(), 1);
        let mut text = transform_str_to_source_text("callerPrivate();");
        unsafe {
            let raw = Compile1(cx, options.ptr, &mut text);
            assert!(!raw.is_null(), "moved.js did not compile");
            rooted!(&in(cx) let script = raw);
            JS::SetScriptPrivate(script.get(), &Int32Value(707));
            JS_GC(cx, GCReason::API);
            rooted!(&in(cx) let mut rval = UndefinedValue());
            assert!(JS_ExecuteScript(cx, script.handle(), rval.handle_mut()), "moved.js threw");
        }
        assert_eq!(seen(), ["int(707)"]);
    });
}

#[test]
fn two_scripts_of_one_document_answer_for_the_one_that_is_running() {
    // The inline `<script>` elements of a document share one URL, which is why the private is looked up by script and not
    // by file name: the second must not answer with the first's, nor the first with the second's.
    with_global(|cx, _global| {
        servo_run(cx, "doc.html", "globalThis.one = callerPrivate();", Some(Int32Value(111)));
        servo_run(cx, "doc.html", "globalThis.two = callerPrivate();", Some(Int32Value(222)));
        assert_eq!(seen(), ["int(111)", "int(222)"]);
    });
}

#[test]
fn code_from_a_string_answers_undefined_as_the_cpp_does() {
    // The eval script and the `new Function` script have no private in the C++ either, and the fork gives their frames the
    // source name `<eval>` (Brimstone `parser/source.rs:71`, reached from `runtime/eval/eval.rs:70` and
    // `runtime/eval/create_dynamic_function.rs:136`; a frame's file is that name, `runtime/stack_trace.rs:238-240`), which is
    // no file any script here was compiled from. So neither the glue nor the C++ finds a script to read: the same answer, for
    // the same reason.
    with_global(|cx, _global| {
        servo_run(cx, "page.html", "eval('callerPrivate();');", Some(Int32Value(101)));
        assert_eq!(seen(), ["undefined"]);
        servo_run(cx, "page.html", "new Function('return callerPrivate();')();", Some(Int32Value(101)));
        assert_eq!(seen(), ["undefined", "undefined"]);
        // A function *defined* by the eval answers the same way: its frames are the eval script's. Parenthesised, because a
        // source that opens `function (` threw out of this engine's `eval`, and the parenthesised form is the expression
        // whose value the eval has to yield anyway.
        servo_run(
            cx,
            "page.html",
            "globalThis.fromEval = eval('(function () { return callerPrivate(); })'); fromEval();",
            Some(Int32Value(101)),
        );
        assert_eq!(seen(), ["undefined", "undefined", "undefined"]);
    });
}

#[test]
fn a_host_evaluated_script_of_the_pages_own_url_answers_undefined() {
    // `JS::Evaluate` compiles and runs a script in one step, so nobody can have given that script a private. When it is
    // handed the document's own URL — what a bookmarklet and Servo's `evaluate_script` do — the same file is also a compiled
    // script's, and the engine gives the glue no way to tell the two frames apart. It answers for the one-step script, which
    // is what the C++ answers here; `a_page_function_called_from_a_host_evaluated_script_of_one_url_answers_undefined` is the
    // case where that guess costs the page function's private.
    with_global(|cx, global| {
        servo_run(cx, "page.html", "globalThis.marker = 1;", Some(Int32Value(101)));
        assert_eq!(eval_at(cx, global, "page.html", "callerPrivate()"), Some("undefined".to_owned()));
        assert_eq!(seen(), ["undefined"]);
    });
}

#[test]
fn a_page_function_called_from_a_host_evaluated_script_of_one_url_answers_undefined() {
    // The counterpart of `a_page_function_called_from_a_host_evaluated_script_answers_its_own_private`, where the eval is of
    // `check.js` and the differing file names let the glue find `holder`'s script. Here the eval is of `page.html` itself, so
    // `holder`'s frame — the script that was given 101, and the one the C++ answers with — is indistinguishable from the
    // eval's own code, and the answer is `UndefinedValue()`. Servo then runs the timer with the default fetch options, whose
    // base URL is the same document.
    with_global(|cx, global| {
        servo_run(cx, "page.html", "globalThis.holder = function () { return callerPrivate(); };", Some(Int32Value(101)));
        assert_eq!(eval_at(cx, global, "page.html", "holder()"), Some("undefined".to_owned()));
        assert_eq!(seen(), ["undefined"]);
    });
}

#[test]
fn a_script_the_host_evaluates_has_no_private_of_its_own() {
    // `JS::Evaluate` (a bookmarklet, Servo's `evaluate_script`) compiles a script and runs it in one step; no one can set a
    // private on that script, and the C++ answers `UndefinedValue()` for it.
    with_global(|cx, global| {
        assert_eq!(eval_value(cx, global, "callerPrivate()"), Some("undefined".to_owned()));
        assert_eq!(seen(), ["undefined"]);
    });
}

#[test]
fn a_page_function_called_from_a_host_evaluated_script_answers_its_own_private() {
    // The innermost scripted frame is `holder`'s, in the script that was given 101, and the eval underneath it is not what
    // the C++ looks at.
    with_global(|cx, global| {
        servo_run(cx, "page.html", "globalThis.holder = function () { return callerPrivate(); };", Some(Int32Value(101)));
        assert!(seen().is_empty(), "the page script does not call the native itself");
        assert_eq!(eval_value(cx, global, "holder()"), Some("int(101)".to_owned()));
        assert_eq!(seen(), ["int(101)"]);
    });
}

// ---- JS_GetFunctionDisplayId ----

#[test]
fn display_name_of_a_named_function() {
    with_global(|cx, global| {
        check_name(cx, global, "function greet(who) { return 'hello ' + who; }", "greet", "greet");
    });
}

#[test]
fn display_name_of_an_anonymous_function_expression_assigned_to_a_variable() {
    // The engine infers the name of the binding it is assigned to, exactly as it does for the `name` property — and exactly
    // as SpiderMonkey, which stores the inferred name in the very slot its display atom is read from (vendored
    // `js/src/vm/JSFunction.cpp:2066`, `js/src/vm/JSFunction.h:421-427`).
    with_global(|cx, global| {
        check_name(cx, global, "const made = function (n) { return n; };", "made", "made");
    });
}

#[test]
fn display_name_of_an_anonymous_function_with_nothing_to_infer_from() {
    // An element of an array literal is nothing to infer a name from. The fork gives every function an own `name` at
    // creation, the empty string when no name was written (`bytecode/function.rs:279-285` in Brimstone), so all three reads
    // answer `""`; SpiderMonkey's display atom stays null there, so its `JS_GetFunctionDisplayId` answers a null
    // out-parameter while its `name` property is `""` as well. Nothing is shown either way — the first difference recorded in
    // mozjs-sys/src/engine/function.rs.
    with_global(|cx, global| {
        check_name(cx, global, "globalThis.pair = [function (n) { return n; }];", "pair[0]", "");
    });
}

#[test]
fn display_name_of_an_arrow_function() {
    with_global(|cx, global| {
        check_name(cx, global, "const twice = (n) => n * 2;", "twice", "twice");
    });
}

#[test]
fn display_name_of_a_method() {
    with_global(|cx, global| {
        check_name(cx, global, "const holder = { speak() { return 'hi'; } };", "holder.speak", "speak");
    });
}

#[test]
fn display_name_of_a_bound_function() {
    // `Function.prototype.bind` names the result after its target, which is what a console message wants to see.
    with_global(|cx, global| {
        check_name(cx, global, "function speak2() {} const bound = speak2.bind(null);", "bound", "bound speak2");
    });
}

#[test]
fn display_name_of_a_native_function() {
    with_global(|cx, global| {
        check_name(cx, global, "", "callerPrivate", "callerPrivate");
    });
}

#[test]
fn the_display_name_follows_the_name_property_a_script_changed() {
    // The other difference recorded in mozjs-sys/src/engine/function.rs: the fork's name is the script-visible configurable
    // own property (`common_shapes.rs:103-113` in Brimstone) read by `api::function_name`, so it moves with what script does
    // to it. SpiderMonkey answers the atom the function was compiled or inferred with, `greet` and `original` here, and is
    // undisturbed by either edit. After the delete the fork reads on up to `Function.prototype`'s own name, the empty string
    // (`intrinsics/function_prototype.rs:84-90`).
    with_global(|cx, global| {
        eval(cx, global, "const greet = function greet() {}; delete greet.name;");
        assert_eq!(name_of(cx, global, "greet", true), "", "a deleted name reads the prototype's");
        assert_eq!(name_of(cx, global, "greet", false), "", "and `JS_GetFunctionId` is the same read");
        assert_eq!(eval_str(cx, global, "String(greet.name)"), "");

        eval(
            cx,
            global,
            "const renamed = function original() {}; Object.defineProperty(renamed, 'name', { value: 'changed', configurable: true });",
        );
        assert_eq!(name_of(cx, global, "renamed", true), "changed");
        assert_eq!(eval_str(cx, global, "String(renamed.name)"), "changed");
    });
}

#[test]
fn the_nullptr_out_parameter_is_a_name_that_reads_undefined() {
    // The nullptr the declaration allows for a function with no useful name to display, and what `console.rs:505` skips. The
    // fork answers None from `api::function_name` only when the read `name` comes out undefined
    // (`embed/api.rs:529-532`), so that is the shape that reaches it here; SpiderMonkey reaches it from a null display atom
    // instead (see the two differences above), and for this very function would answer the atom `f`.
    with_global(|cx, global| {
        eval(
            cx,
            global,
            "const f = function () {}; Object.defineProperty(f, 'name', { value: undefined, configurable: true });",
        );
        assert_eq!(name_of(cx, global, "f", true), "(null)");
        assert_eq!(name_of(cx, global, "f", false), "(null)");
        assert_eq!(eval_str(cx, global, "String(f.name)"), "undefined");
    });
}

#[test]
fn a_name_accessor_that_throws_fails_the_call_and_leaves_the_exception() {
    // The doc text's last paragraph, "returns false if any error happens while generating the function name string": here the
    // error is a script's own `name` getter throwing, `fail` leaves its exception pending for the caller exactly as
    // `JS_GetFunctionLength` does with the same read (mozjs-sys/src/engine/function.rs:479, util.rs:28), and the
    // out-parameter stays null.
    with_global(|cx, global| {
        eval(
            cx,
            global,
            "const f = function () {}; Object.defineProperty(f, 'name', { get() { throw new RangeError('no name'); }, configurable: true });",
        );
        let options = CompileOptionsWrapper::new(cx, c"fun.js".to_owned(), 1);
        rooted!(&in(cx) let mut v = UndefinedValue());
        assert!(mozjs::rust::evaluate_script(cx, global, "f", v.handle_mut(), options).is_ok(), "reading f threw");
        rooted!(&in(cx) let fun = unsafe { JS_ValueToFunction(cx, v.handle()) });
        assert!(!fun.get().is_null(), "f does not hold a function: {}", describe(&v.get()));
        rooted!(&in(cx) let mut name = ptr::null_mut::<JSString>());
        assert!(!unsafe { JS_GetFunctionDisplayId(cx, fun.handle(), name.handle_mut()) }, "a failing name read answered true");
        assert!(name.get().is_null(), "the out-parameter was not null after the failure");
        assert!(unsafe { JS_IsExceptionPending(cx) }, "the getter's exception was not left pending");
        rooted!(&in(cx) let mut exc = UndefinedValue());
        assert!(unsafe { JS_GetPendingException(cx, exc.handle_mut()) }, "the pending exception could not be read");
        unsafe { JS_ClearPendingException(cx) };
        rooted!(&in(cx) let ev = exc.get());
        assert!(
            unsafe { JS_DefineProperty(cx, global, c"__exc".as_ptr(), ev.handle(), JSPROP_ENUMERATE as u32) },
            "the exception could not be stashed for script to read"
        );
        assert_eq!(eval_str(cx, global, "__exc.name + ': ' + __exc.message"), "RangeError: no name");
    });
}
