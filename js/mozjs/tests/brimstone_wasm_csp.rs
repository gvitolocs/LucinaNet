/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `CompileStringsKind::Wasm` through the glue's CSP check (`mozjs-sys/src/engine/security.rs`): the callback must see
//! `JS::RuntimeCode::WASM` with `JS::CompilationType::Undefined`, and the answer must come back to the caller.
//!
//! WebAssembly itself is slice W0a, so no script reaches the Wasm kind yet: there is no `WebAssembly` to compile. These tests
//! therefore call the glue's own `can_compile_strings` with the kind, with empty code, no parameters and no body — the question
//! the engine's `can_compile_wasm` puts to its host.

use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicUsize, Ordering};

use mozjs::context::JSContext;
use mozjs::jsapi::{
    self as js, CompilationType, Handle, HandleString, HandleValue, JSSecurityCallbacks, JSString, OnNewGlobalHookOption,
    RuntimeCode,
};
use mozjs::jsval::JSVal;
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{Handle as SafeHandle, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};
use mozjs::{conversions::jsstr_to_string, gc::StackGCVector};

use mozjs_sys::embed::{api, CompileStringsKind, HandleScope};
use mozjs_sys::engine::cx::Cx;
use mozjs_sys::engine::security;

/// 0: allow, 1: refuse, 2: allow after a collection inside the callback.
static MODE: AtomicUsize = AtomicUsize::new(0);
static RUNTIME: AtomicUsize = AtomicUsize::new(usize::MAX);
static COMPILATION: AtomicUsize = AtomicUsize::new(usize::MAX);
/// How often the callback saw the empty wasm question: no parameters, no arguments, no body, an empty code string.
static WASM_SHAPED: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn policy(
    cx: *mut js::JSContext,
    runtime: RuntimeCode,
    code: HandleString,
    kind: CompilationType,
    strings: Handle<StackGCVector<*mut JSString>>,
    body: HandleString,
    args: Handle<StackGCVector<JSVal>>,
    body_arg: HandleValue,
    allowed: *mut bool,
) -> bool {
    RUNTIME.store(runtime as usize, Ordering::SeqCst);
    COMPILATION.store(kind as usize, Ordering::SeqCst);
    if runtime == RuntimeCode::WASM {
        let empty = !(*code.ptr).is_null() && {
            let mut context = JSContext::from_ptr(NonNull::new(cx).unwrap());
            jsstr_to_string(&mut context, NonNull::new(*code.ptr).unwrap()).is_empty()
        };
        if empty && SafeHandle::from_raw(strings).len() == 0 && SafeHandle::from_raw(args).len() == 0 && (*body.ptr).is_null()
            && (*body_arg.ptr).is_undefined()
        {
            WASM_SHAPED.fetch_add(1, Ordering::SeqCst);
        }
    }
    match MODE.load(Ordering::SeqCst) {
        1 => {
            *allowed = false;
            true
        },
        2 => {
            // The code string the callback is handed must live through a collection: the glue traces it.
            js::JS_GC(cx, js::GCReason::API);
            let mut context = JSContext::from_ptr(NonNull::new(cx).unwrap());
            assert_eq!(
                jsstr_to_string(&mut context, NonNull::new(*code.ptr).unwrap()),
                "",
                "the wasm question's code string did not survive the callback's collection"
            );
            *allowed = true;
            true
        },
        _ => {
            *allowed = true;
            true
        },
    }
}

static CALLBACKS: JSSecurityCallbacks = JSSecurityCallbacks {
    contentSecurityPolicyAllows: Some(policy),
    codeForEvalGets: None,
    subsumes: None,
};

/// The engine's question to the CSP, asked here instead of by a compilation.
fn ask(context: &mut JSContext, kind: CompileStringsKind, source: &str) -> Option<bool> {
    let cx = unsafe { context.raw_cx() }.cast::<Cx>();
    let bs = unsafe { (*cx).bs };
    let scope = HandleScope::enter(bs);
    let answer = match api::string_from_utf8(bs, source) {
        Ok(code) => Some(unsafe {
            security::can_compile_strings(cx, bs, kind, code.as_string(), &[], None, &[], bs.undefined())
        }),
        Err(_) => None,
    };
    scope.exit();
    match answer {
        Some(Ok(allowed)) => Some(allowed),
        _ => None,
    }
}

#[test]
fn wasm_kind_is_asked_as_wasm_runtime_code() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, global.handle());
        let context = &mut realm;
        js::JS_SetSecurityCallbacks(context.raw_cx(), &CALLBACKS);

        // An allowed wasm compilation: the callback sees the wasm runtime with the undefined compilation type.
        MODE.store(0, Ordering::SeqCst);
        assert_eq!(ask(context, CompileStringsKind::Wasm, ""), Some(true), "the glue must pass the CSP's true through");
        assert_eq!(RUNTIME.load(Ordering::SeqCst), RuntimeCode::WASM as usize, "the Wasm kind is not JS runtime code");
        assert_eq!(
            COMPILATION.load(Ordering::SeqCst),
            CompilationType::Undefined as usize,
            "a wasm compilation is neither eval nor Function"
        );
        assert_eq!(WASM_SHAPED.load(Ordering::SeqCst), 1, "the wasm question carries no code, no parameters and no body");

        // A refused one: false, and still about the wasm runtime.
        MODE.store(1, Ordering::SeqCst);
        assert_eq!(ask(context, CompileStringsKind::Wasm, ""), Some(false), "a refused wasm question must say false");
        assert_eq!(RUNTIME.load(Ordering::SeqCst), RuntimeCode::WASM as usize);
        assert_eq!(COMPILATION.load(Ordering::SeqCst), CompilationType::Undefined as usize);

        // Its slots live through a collection run by the callback itself.
        MODE.store(2, Ordering::SeqCst);
        assert_eq!(ask(context, CompileStringsKind::Wasm, ""), Some(true));

        // The kinds that existed before the Wasm one keep their own answer.
        MODE.store(0, Ordering::SeqCst);
        assert_eq!(ask(context, CompileStringsKind::Eval, "1"), Some(true));
        assert_eq!(RUNTIME.load(Ordering::SeqCst), RuntimeCode::JS as usize);
        assert_eq!(COMPILATION.load(Ordering::SeqCst), CompilationType::DirectEval as usize);
        assert_eq!(ask(context, CompileStringsKind::IndirectEval, "1"), Some(true));
        assert_eq!(COMPILATION.load(Ordering::SeqCst), CompilationType::IndirectEval as usize);
        assert_eq!(ask(context, CompileStringsKind::Function, "return 1"), Some(true));
        assert_eq!(COMPILATION.load(Ordering::SeqCst), CompilationType::Function as usize);
        assert_eq!(RUNTIME.load(Ordering::SeqCst), RuntimeCode::JS as usize, "Function is still JS runtime code");
        assert_eq!(WASM_SHAPED.load(Ordering::SeqCst), 3, "only the three wasm questions were wasm-shaped");

        js::JS_SetSecurityCallbacks(context.raw_cx(), ptr::null());
        assert_eq!(ask(context, CompileStringsKind::Wasm, ""), Some(true), "no callbacks: wasm is allowed");
    }
}
