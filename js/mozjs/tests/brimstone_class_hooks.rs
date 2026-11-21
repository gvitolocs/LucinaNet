/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Calling and constructing an object whose JSClass has call / construct hooks (Servo's DOM interface objects), and
// glue::DescribeScriptedCaller from inside such a hook.

use std::ffi::CStr;
use std::ptr;
use std::sync::Mutex;

use mozjs::glue::DescribeScriptedCaller;
use mozjs::jsapi::{
    CallArgs, JSClass, JSClassOps, JSContext, JS_DefineProperty, JS_NewObject, JS_NewPlainObject, OnNewGlobalHookOption,
    Value, JSPROP_ENUMERATE,
};
use mozjs::jsval::{Int32Value, ObjectValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{evaluate_script, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};

static CALLER: Mutex<Option<(String, u32)>> = Mutex::new(None);

/// Returns 10 * argc + the first argument; records the scripted caller.
unsafe extern "C" fn call_hook(cx: *mut JSContext, argc: u32, vp: *mut Value) -> bool {
    let args = unsafe { CallArgs::from_vp(vp, argc) };
    assert!(!args.is_constructing());
    let mut buf = [0 as std::os::raw::c_char; 64];
    let (mut line, mut col) = (0u32, 0u32);
    if unsafe { DescribeScriptedCaller(cx, buf.as_mut_ptr(), buf.len(), &mut line, &mut col) } {
        let name = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned();
        *CALLER.lock().unwrap() = Some((name, line));
    }
    let first = if argc > 0 { args.get(0).to_int32() } else { 0 };
    args.rval().set(Int32Value(10 * argc as i32 + first));
    true
}

/// `new Ctor(...)` returns a plain object; it must arrive flagged as constructing.
unsafe extern "C" fn construct_hook(cx: *mut JSContext, argc: u32, vp: *mut Value) -> bool {
    let args = unsafe { CallArgs::from_vp(vp, argc) };
    assert!(args.is_constructing());
    let obj = unsafe { JS_NewPlainObject(cx) };
    args.rval().set(ObjectValue(obj));
    true
}

static CALLABLE_OPS: JSClassOps = JSClassOps {
    addProperty: None,
    delProperty: None,
    enumerate: None,
    newEnumerate: None,
    resolve: None,
    mayResolve: None,
    finalize: None,
    call: Some(call_hook),
    construct: Some(construct_hook),
    trace: None,
};

static CALLABLE: JSClass = JSClass {
    name: c"Callable".as_ptr(),
    flags: 0,
    cOps: &CALLABLE_OPS,
    spec: ptr::null(),
    ext: ptr::null(),
    oOps: ptr::null(),
};

#[test]
fn class_call_and_construct_hooks() {
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
        rooted!(&in(context) let ctor = JS_NewObject(context.raw_cx(), &CALLABLE));
        rooted!(&in(context) let ctor_value = ObjectValue(ctor.get()));
        assert!(JS_DefineProperty(
            context.raw_cx(),
            global.handle().into(),
            c"Ctor".as_ptr(),
            ctor_value.handle().into(),
            JSPROP_ENUMERATE as u32,
        ));
        let mut run = |src: &str| -> i32 {
            rooted!(&in(context) let mut rval = UndefinedValue());
            let options = CompileOptionsWrapper::new(context, c"hooks.js".to_owned(), 1);
            assert!(evaluate_script(context, global.handle(), src, rval.handle_mut(), options).is_ok(), "{src} threw");
            rval.get().to_int32()
        };
        assert_eq!(run("Ctor()"), 0);
        assert_eq!(run("Ctor(7, 8)"), 27);
        assert_eq!(run("typeof new Ctor() === 'object' ? 1 : 0"), 1);
        assert_eq!(run("\n\n  Ctor(1)"), 11);
        let caller = CALLER.lock().unwrap().clone().expect("DescribeScriptedCaller answered inside the hook");
        assert_eq!(caller, ("hooks.js".to_string(), 3));
        // No script running: no scripted caller, nothing pending.
        let mut buf = [0 as std::os::raw::c_char; 8];
        let (mut l, mut c) = (0, 0);
        assert!(!DescribeScriptedCaller(context.raw_cx(), buf.as_mut_ptr(), buf.len(), &mut l, &mut c));

        // js::DefineFunctionWithReserved: a callable property whose function has native reserved slots.
        let f = mozjs::jsapi::js::DefineFunctionWithReserved(
            context.raw_cx(),
            global.get(),
            c"withSlots".as_ptr(),
            Some(call_hook),
            1,
            JSPROP_ENUMERATE as u32,
        );
        assert!(!f.is_null());
        let fobj = mozjs::jsapi::JS_GetFunctionObject(f);
        let v = Int32Value(42);
        mozjs::jsapi::js::SetFunctionNativeReserved(fobj, 1, &v);
        assert_eq!((*mozjs::jsapi::js::GetFunctionNativeReserved(fobj, 1)).to_int32(), 42);
        rooted!(&in(context) let mut rval = UndefinedValue());
        let options = CompileOptionsWrapper::new(context, c"hooks.js".to_owned(), 1);
        assert!(evaluate_script(context, global.handle(), "withSlots(5)", rval.handle_mut(), options).is_ok());
        assert_eq!(rval.get().to_int32(), 15);
    }
}
