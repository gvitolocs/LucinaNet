/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! A prototype trap that answers "no prototype" by writing a null `JSObject*` into its `MutableHandleObject`
//! (Servo's cross-origin WindowProxy `getPrototype` does) must read as a null prototype. The engine used to read the
//! slot as a Value and dereferenced address 0 (the YouTube back-navigation crash: `instanceof` on a cross-origin proxy).

use std::ptr;

use mozjs::glue::{CreateForwardingProxyHandler, CreateProxyHandler, ProxyTraps};
use mozjs::jsapi::{JSContext, JSObject, JSPROP_ENUMERATE, JS_DefineProperty, OnNewGlobalHookOption, JS};
use mozjs::jsval::{ObjectValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{JS_NewGlobalObject, NewProxyObject, NewWindowProxy};
use mozjs::rust::{CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS, evaluate_script};

/// `getPrototype`: no prototype, as a null pointer.
unsafe extern "C" fn null_proto(_cx: *mut JSContext, _proxy: JS::HandleObject, protop: JS::MutableHandleObject) -> bool {
    unsafe { *protop.ptr = ptr::null_mut() };
    true
}

/// `getPrototypeIfOrdinary`: ordinary, and the prototype is null.
unsafe extern "C" fn ordinary_null_proto(
    _cx: *mut JSContext,
    _proxy: JS::HandleObject,
    is_ordinary: *mut bool,
    protop: JS::MutableHandleObject,
) -> bool {
    unsafe {
        *is_ordinary = true;
        *protop.ptr = ptr::null_mut();
    }
    true
}

unsafe fn eval(context: &mut mozjs::context::JSContext, global: mozjs::gc::HandleObject, src: &str) -> i32 {
    let options = CompileOptionsWrapper::new(&context, c"t".to_owned(), 1);
    rooted!(&in(context) let mut rval = UndefinedValue());
    evaluate_script(context, global, src, rval.handle_mut(), options).expect("script threw");
    rval.get().to_int32()
}

unsafe fn define_p(context: &mut mozjs::context::JSContext, global: mozjs::gc::HandleObject, proxy: *mut JSObject) {
    assert!(!proxy.is_null());
    rooted!(&in(context) let pv = ObjectValue(proxy));
    assert!(unsafe {
        JS_DefineProperty(context.raw_cx(), global.into(), c"p".as_ptr(), pv.handle().into(), JSPROP_ENUMERATE as u32)
    });
}

const CHECKS: &str = "(Object.getPrototypeOf(p) === null ? 1 : 0) \
    + (p instanceof Function ? 100 : 0) + (p instanceof Object ? 1000 : 0) \
    + (Function.prototype.isPrototypeOf(p) ? 10000 : 0)";

/// Base handler, `getPrototypeIfOrdinary` writes null: null prototype, and `instanceof` is simply false.
#[test]
fn null_proto_from_get_prototype_if_ordinary() {
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
        let traps = ProxyTraps { getPrototypeIfOrdinary: Some(ordinary_null_proto), ..Default::default() };
        let handler = CreateProxyHandler(&traps, ptr::null());
        rooted!(&in(context) let privv = UndefinedValue());
        let proxy = NewProxyObject(context, handler, privv.handle(), ptr::null_mut(), ptr::null(), false);
        define_p(context, global.handle(), proxy);
        assert_eq!(eval(context, global.handle(), CHECKS), 1);
    }
}

/// WindowProxy (forwarding handler) whose `getPrototype` writes null, like Servo's cross-origin WindowProxy.
#[test]
fn null_proto_from_get_prototype() {
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
        let traps = ProxyTraps { getPrototype: Some(null_proto), ..Default::default() };
        let handler = CreateForwardingProxyHandler(&traps, ptr::null());
        let wp = NewWindowProxy(context, global.handle(), handler);
        define_p(context, global.handle(), wp);
        assert_eq!(eval(context, global.handle(), CHECKS), 1);
    }
}
