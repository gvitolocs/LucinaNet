/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use mozjs::jsapi::{
    self as js, CompilationType, Handle, HandleString, HandleValue, JSContext, JSSecurityCallbacks,
    JSString, OnNewGlobalHookOption, RuntimeCode,
};
use mozjs::jsval::{Int32Value, JSVal, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rust::{
    evaluate_script, wrappers2::JS_NewGlobalObject, CompileOptionsWrapper, Handle as SafeHandle,
    JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS,
};
use mozjs::{conversions::jsstr_to_string, gc::StackGCVector, rooted};
use std::{
    ptr::{self, NonNull},
    sync::atomic::{AtomicUsize, Ordering},
};

static MODE: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicUsize = AtomicUsize::new(0);
static KINDS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn policy(
    cx: *mut JSContext,
    runtime: RuntimeCode,
    _code: HandleString,
    kind: CompilationType,
    strings: Handle<StackGCVector<*mut JSString>>,
    body: HandleString,
    args: Handle<StackGCVector<JSVal>>,
    body_arg: HandleValue,
    allowed: *mut bool,
) -> bool {
    assert_eq!(runtime, RuntimeCode::JS);
    CALLS.fetch_add(1, Ordering::SeqCst);
    KINDS.fetch_or(1 << kind as usize, Ordering::SeqCst);
    match MODE.load(Ordering::SeqCst) {
        1 => {
            *allowed = false;
            true
        }
        2 => {
            let exception = Int32Value(73);
            js::JS_SetPendingException(
                cx,
                Handle {
                    ptr: &exception,
                    _phantom_0: Default::default(),
                },
                js::ExceptionStackBehavior::DoNotCapture,
            );
            false
        }
        3 => {
            assert_eq!(kind, CompilationType::Function);
            // Force all callback-side string/object snapshots to move before reading them.
            js::JS_GC(cx, js::GCReason::API);
            let mut context = mozjs::context::JSContext::from_ptr(NonNull::new(cx).unwrap());
            let strings = SafeHandle::from_raw(strings);
            let args = SafeHandle::from_raw(args);
            assert_eq!(strings.len(), 1);
            assert_eq!(args.len(), 1);
            assert!(args.at(0).unwrap().is_object());
            assert!((*body_arg.ptr).is_object());
            let parameter = *strings.at(0).unwrap();
            assert_eq!(
                jsstr_to_string(&mut context, NonNull::new(parameter).unwrap()),
                "a"
            );
            assert_eq!(
                jsstr_to_string(&mut context, NonNull::new(*body.ptr).unwrap()),
                "return a"
            );
            *allowed = true;
            true
        }
        4 => false, // No pending exception: uncatchable termination.
        5 => {
            assert_eq!(kind, CompilationType::Function);
            assert!((*body.ptr).is_null());
            assert!((*body_arg.ptr).is_undefined());
            assert_eq!(SafeHandle::from_raw(strings).len(), 0);
            assert_eq!(SafeHandle::from_raw(args).len(), 0);
            *allowed = true;
            true
        }
        _ => {
            *allowed = true;
            true
        }
    }
}

unsafe extern "C" fn branded_source(
    cx: *mut JSContext,
    _object: js::HandleObject,
    out: js::MutableHandleString,
) -> bool {
    // This hook models Servo's TrustedScript extraction. Its output is rooted during GC.
    *out.ptr = js::JS_NewStringCopyZ(cx, c"40 + 2".as_ptr());
    if (*out.ptr).is_null() {
        return false;
    }
    js::JS_GC(cx, js::GCReason::API);
    true
}

static CALLBACKS: JSSecurityCallbacks = JSSecurityCallbacks {
    contentSecurityPolicyAllows: Some(policy),
    codeForEvalGets: None,
    subsumes: None,
};
static BRANDED: JSSecurityCallbacks = JSSecurityCallbacks {
    contentSecurityPolicyAllows: Some(policy),
    codeForEvalGets: Some(branded_source),
    subsumes: None,
};

#[test]
fn lucinanet_dynamic_code_policy() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(context, &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(), OnNewGlobalHookOption::FireOnNewGlobalHook, &*RealmOptions::default()));
        let mut realm = AutoRealm::new_from_handle(context, global.handle());
        let context = &mut realm;
        rooted!(&in(context) let mut result = UndefinedValue());
        macro_rules! run {
            ($source:expr) => {{
                let options = CompileOptionsWrapper::new(context, c"policy.js".to_owned(), 1);
                assert!(
                    evaluate_script(
                        context,
                        global.handle(),
                        $source,
                        result.handle_mut(),
                        options
                    )
                    .is_ok(),
                    "{}",
                    $source
                );
                assert_eq!(result.get().to_int32(), 1, "{}", $source);
            }};
        }
        js::JS_SetSecurityCallbacks(context.raw_cx(), &CALLBACKS);
        run!("Number(eval('2 + 3') === 5 && (0, eval)('6') === 6)");
        assert_eq!(KINDS.load(Ordering::SeqCst), 3);
        MODE.store(1, Ordering::SeqCst);
        run!(
            r#"(() => {
            let constructors = [Function, (async function(){}).constructor,
                (function*(){}).constructor, (async function*(){}).constructor];
            let blocked = 0;
            for (let C of constructors) { try { C('return 1'); } catch(e) { if(e instanceof EvalError) blocked++; } }
            try { eval('1'); } catch(e) { if(e instanceof EvalError) blocked++; }
            try { (0, eval)('1'); } catch(e) { if(e instanceof EvalError) blocked++; }
            return Number(blocked === 6);
        })()"#
        );
        let before = CALLS.load(Ordering::SeqCst);
        run!("Number(eval(17) === 17)");
        assert_eq!(CALLS.load(Ordering::SeqCst), before);
        MODE.store(2, Ordering::SeqCst);
        run!("(() => { try { eval('1'); } catch(e) { return Number(e === 73); } })()");
        run!("(() => { try { Function('return 1'); } catch(e) { return Number(e === 73); } })()");
        MODE.store(3, Ordering::SeqCst);
        run!(
            r#"(() => {
            let conversions = 0;
            const parameter = {toString() { conversions++; return 'a'; }};
            const body = {toString() { conversions++; return 'return a'; }};
            const f = Function(parameter, body);
            return Number(f(9) === 9 && conversions === 2);
        })()"#
        );
        MODE.store(5, Ordering::SeqCst);
        run!("Number(Function()() === undefined)");
        MODE.store(4, Ordering::SeqCst);
        let options = CompileOptionsWrapper::new(context, c"policy.js".to_owned(), 1);
        assert!(evaluate_script(
            context,
            global.handle(),
            "try { eval('1') } catch(e) { 1 }",
            result.handle_mut(),
            options
        )
        .is_err());
        assert!(!js::JS_IsExceptionPending(context.raw_cx()));
        MODE.store(0, Ordering::SeqCst);
        js::JS_SetSecurityCallbacks(context.raw_cx(), &BRANDED);
        run!("Number(eval({toString(){throw 'must not coerce'}}) === 42)");
        js::JS_SetSecurityCallbacks(context.raw_cx(), ptr::null());
        run!("(() => { const object={}; return Number(eval(object) === object); })()");
    }
}
