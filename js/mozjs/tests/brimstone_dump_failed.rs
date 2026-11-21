/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `LUCINANET_JS_DUMP_FAILED=<dir>`: the source of a script the glue compiles or runs that fails with a SyntaxError is written
//! to `<dir>/<pid>-<n>.js`, with a report of what the glue knows about it in `<dir>/<pid>-<n>.txt` beside it.
//!
//! The variable is read when the context is created, so `with_global` sets (or removes) it before `Runtime::new`; this file is
//! its own process and its tests run one at a time (`.cargo/config.toml`), so one test's variable cannot reach another's.

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::Arc;

use mozjs::context::JSContext;
use mozjs::gc::HandleObject;
use mozjs::jsapi::{OnNewGlobalHookOption, JSPROP_ENUMERATE};
use mozjs::jsval::UndefinedValue;
use mozjs::offthread::{compile_to_stencil_offthread, CompilationResult};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{
    Compile1, ConvertFrontendErrorsToRuntimeErrors, JS_ClearPendingException, JS_DefineProperty, JS_ExecuteScript,
    JS_GetPendingException, JS_IsExceptionPending, JS_NewGlobalObject,
};
use mozjs::rust::{transform_str_to_source_text, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};

/// Redeclares the global `a` when it runs: the failure the flag was added for, found at global declaration instantiation.
const REDECLARE: &str = "let a = 1;";
/// Wrong as it is parsed.
const BAD_SYNTAX: &str = "let = ;";

/// Runs `f` in a fresh engine with a simple global, its realm entered, and returns what it returned with the dump directory it
/// used. `dump` is whether `LUCINANET_JS_DUMP_FAILED` names that directory: the variable is set or removed before the context
/// is created, since that is when it is read. The directory is empty beforehand and, with the variable off, never made.
fn with_global<R>(tag: &str, dump: bool, f: impl FnOnce(&mut JSContext, HandleObject) -> R) -> (PathBuf, R) {
    let dir = std::env::temp_dir().join(format!("lucina-dump-failed-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    match dump {
        true => std::env::set_var("LUCINANET_JS_DUMP_FAILED", &dir),
        false => std::env::remove_var("LUCINANET_JS_DUMP_FAILED"),
    }
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let out = unsafe {
        let context = runtime.cx();
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            std::ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, global.handle());
        f(&mut realm, global.handle())
    };
    drop(runtime);
    drop(engine);
    std::env::remove_var("LUCINANET_JS_DUMP_FAILED");
    (dir, out)
}

/// Evaluates `src` as its own classic script from `file`, and returns whether it ran; a thrown error is left pending.
fn eval(cx: &mut JSContext, global: HandleObject, src: &str, file: &str, line: u32) -> bool {
    let file = CString::new(file).unwrap();
    let options = CompileOptionsWrapper::new(cx, file, line);
    rooted!(&in(cx) let mut rval = UndefinedValue());
    mozjs::rust::evaluate_script(cx, global, src, rval.handle_mut(), options).is_ok()
}

/// Evaluates `src` and returns its completion value as a string; panics if it throws.
fn eval_str(cx: &mut JSContext, global: HandleObject, src: &str) -> String {
    let options = CompileOptionsWrapper::new(cx, c"read.js".to_owned(), 1);
    rooted!(&in(cx) let mut rval = UndefinedValue());
    assert!(mozjs::rust::evaluate_script(cx, global, src, rval.handle_mut(), options).is_ok(), "script threw: {src}");
    assert!(unsafe { JS_DefineProperty(cx, global, c"__v".as_ptr(), rval.handle(), JSPROP_ENUMERATE as u32) });
    let options = CompileOptionsWrapper::new(cx, c"read.js".to_owned(), 1);
    rooted!(&in(cx) let mut v = UndefinedValue());
    assert!(mozjs::rust::evaluate_script(cx, global, "String(__v)", v.handle_mut(), options).is_ok(), "String(__v) threw");
    unsafe { mozjs::conversions::jsstr_to_string(cx, NonNull::new(v.get().to_string()).unwrap()) }
}

/// The pending exception as `name: message`, then cleared.
fn take_exception(cx: &mut JSContext, global: HandleObject) -> String {
    assert!(unsafe { JS_IsExceptionPending(cx) }, "no pending exception");
    rooted!(&in(cx) let mut exc = UndefinedValue());
    assert!(unsafe { JS_GetPendingException(cx, exc.handle_mut()) });
    unsafe { JS_ClearPendingException(cx) };
    assert!(exc.get().is_object(), "exception is not an object");
    rooted!(&in(cx) let ev = exc.get());
    assert!(unsafe { JS_DefineProperty(cx, global, c"__exc".as_ptr(), ev.handle(), JSPROP_ENUMERATE as u32) });
    eval_str(cx, global, "__exc.name + ': ' + __exc.message")
}

/// Compiles `src` and runs it as two calls, the way Servo runs a classic script; whether the run succeeded.
fn compile_then_execute(cx: &mut JSContext, src: &str, file: &str) -> bool {
    let cfile = CString::new(file).unwrap();
    let options = CompileOptionsWrapper::new(cx, cfile, 1);
    let mut src_text = transform_str_to_source_text(src);
    unsafe {
        let raw = Compile1(cx, options.ptr, &mut src_text);
        assert!(!raw.is_null(), "{src} should compile");
        rooted!(&in(cx) let script = raw);
        rooted!(&in(cx) let mut rval = UndefinedValue());
        JS_ExecuteScript(cx, script.handle(), rval.handle_mut())
    }
}

/// What the run wrote, as (name, contents), sorted by name.
fn dumped(dir: &Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<(String, String)> = entries
        .filter_map(|e| e.ok())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read_to_string(e.path()).unwrap_or_default()))
        .collect();
    out.sort();
    out
}

/// The one dump in `dir`: the source it wrote and the report beside it.
fn only_dump(dir: &Path) -> (String, String) {
    let files = dumped(dir);
    assert_eq!(files.len(), 2, "expected one .js and one .txt in {}: {files:?}", dir.display());
    let source = files.iter().find(|(name, _)| name.ends_with(".js")).expect("no .js was written");
    let report = files.iter().find(|(name, _)| name.ends_with(".txt")).expect("no .txt was written");
    assert_eq!(stem(&source.0), stem(&report.0), "the report is not beside the source it describes");
    (source.1.clone(), report.1.clone())
}

fn stem(name: &str) -> &str {
    name.trim_end_matches(".js").trim_end_matches(".txt")
}

/// The names carry this process's pid, so the browser's content processes cannot overwrite each other.
fn check_names(dir: &Path) {
    let prefix = format!("{}-", std::process::id());
    for (name, _) in dumped(dir) {
        assert!(name.starts_with(&prefix), "{name} does not start with this pid");
    }
}

#[test]
#[cfg_attr(target_arch = "wasm32", ignore)]
fn a_script_redeclaring_a_global_is_dumped_with_its_error() {
    let (dir, message) = with_global("evaluate", true, |cx, global| {
        assert!(eval(cx, global, REDECLARE, "first.js", 1), "the first script should run");
        assert!(!eval(cx, global, REDECLARE, "second.js", 7), "the redeclaration should fail");
        take_exception(cx, global)
    });
    assert!(message.starts_with("SyntaxError"), "{message}");
    let (source, report) = only_dump(&dir);
    check_names(&dir);
    assert_eq!(source, REDECLARE);
    assert!(report.contains("failure: run"), "{report}");
    assert!(report.contains(&format!("error: {message}")), "{report}");
    assert!(report.contains("file: second.js"), "{report}");
    assert!(report.contains("line: 7"), "{report}");
}

#[test]
#[cfg_attr(target_arch = "wasm32", ignore)]
fn a_script_that_does_not_parse_is_dumped_with_its_error() {
    let (dir, message) = with_global("compile", true, |cx, global| {
        assert!(!eval(cx, global, BAD_SYNTAX, "broken.js", 3), "the script should not parse");
        take_exception(cx, global)
    });
    assert!(message.starts_with("SyntaxError"), "{message}");
    let (source, report) = only_dump(&dir);
    check_names(&dir);
    assert_eq!(source, BAD_SYNTAX);
    assert!(report.contains("failure: compile"), "{report}");
    assert!(report.contains("file: broken.js"), "{report}");
    assert!(report.contains("line: 3"), "{report}");
}

#[test]
#[cfg_attr(target_arch = "wasm32", ignore)]
fn a_script_executed_after_it_was_compiled_is_dumped_too() {
    let (dir, ()) = with_global("execute", true, |cx, global| {
        assert!(compile_then_execute(cx, REDECLARE, "kept1.js"), "the first script should run");
        assert!(!compile_then_execute(cx, REDECLARE, "kept2.js"), "the redeclaration should fail");
        let message = take_exception(cx, global);
        assert!(message.starts_with("SyntaxError"), "{message}");
    });
    let (source, report) = only_dump(&dir);
    assert_eq!(source, REDECLARE);
    assert!(report.contains("failure: run"), "{report}");
    assert!(report.contains("file: kept2.js"), "{report}");
}

#[test]
#[cfg_attr(target_arch = "wasm32", ignore)]
fn a_syntax_error_found_off_the_thread_is_dumped_when_it_becomes_an_error() {
    let (dir, message) = with_global("offthread", true, |cx, global| {
        let options = CompileOptionsWrapper::new(cx, c"stencil.js".to_owned(), 5);
        let token = compile_to_stencil_offthread(options.ptr as *const _, Arc::new(BAD_SYNTAX.to_string()), move |r| Some(r));
        let CompilationResult { stencil, storage: _, fc, compile_options } = token.finish().expect("the off-thread compile finished");
        assert!(stencil.is_none(), "a script that does not parse leaves no stencil");
        assert!(unsafe { ConvertFrontendErrorsToRuntimeErrors(cx, *fc, compile_options.ptr as *const _) });
        take_exception(cx, global)
    });
    assert!(message.starts_with("SyntaxError"), "{message}");
    let (source, report) = only_dump(&dir);
    assert_eq!(source, BAD_SYNTAX);
    assert!(report.contains("failure: compile"), "{report}");
    assert!(report.contains("file: stencil.js"), "{report}");
    assert!(report.contains("line: 5"), "{report}");
}

#[test]
#[cfg_attr(target_arch = "wasm32", ignore)]
fn nothing_is_written_while_the_variable_is_unset() {
    let (dir, ()) = with_global("off", false, |cx, global| {
        assert!(eval(cx, global, REDECLARE, "one.js", 1), "the first script should run");
        assert!(!eval(cx, global, REDECLARE, "two.js", 1), "the redeclaration should still fail");
        let _ = take_exception(cx, global);
        assert!(!eval(cx, global, BAD_SYNTAX, "broken.js", 1), "the script should still not parse");
        let _ = take_exception(cx, global);
    });
    assert!(dumped(&dir).is_empty(), "{} holds dumps although the variable was unset", dir.display());
    assert!(!dir.exists(), "the glue made {} although the variable was unset", dir.display());
}
