/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Raw-API tests for the Brimstone engine's proxy implementation
// (mozjs-sys/src/engine/proxy.rs): Base, Forwarding and WindowProxy handlers
// seen from JS through the mozjs API.

use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

use mozjs::glue::{
    CreateForwardingProxyHandler, CreateProxyHandler, GetProxyPrivate, GetProxyReservedSlot,
    ProxyTraps, RUST_JSID_IS_INT, RUST_JSID_IS_STRING, RUST_JSID_TO_INT, RUST_JSID_TO_STRING,
    SetProxyPrivate, SetProxyReservedSlot, int_to_jsid,
};
use mozjs::jsapi::{
    JSContext, JSObject, JSString, JSPROP_ENUMERATE, JS_DefineProperty, JS_GC, JS_StringEqualsAscii,
    OnNewGlobalHookOption, jsid, JS,
};
use mozjs::jsval::{Int32Value, ObjectValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{JS_NewGlobalObject, JS_NewPlainObject, NewProxyObject, NewWindowProxy};
use mozjs::rust::{CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS, evaluate_script};

/// A Base handler whose `getOwnPropertyDescriptor` reports indices 0 and 1 (values 42 and 43).
unsafe extern "C" fn indexed_gopd(
    _cx: *mut JSContext,
    _proxy: JS::HandleObject,
    id: JS::HandleId,
    desc: JS::MutableHandle<JS::PropertyDescriptor>,
    is_none: *mut bool,
) -> bool {
    let d = unsafe { &mut *desc.ptr };
    if unsafe { RUST_JSID_IS_INT(id) } {
        let i = unsafe { RUST_JSID_TO_INT(id) };
        let val = match i {
            0 => 42,
            1 => 43,
            _ => {
                unsafe { *is_none = true };
                return true;
            },
        };
        d.set_hasValue_(true);
        d.value_ = Int32Value(val).into();
        d.set_hasWritable_(true);
        d.set_writable_(true);
        d.set_hasEnumerable_(true);
        d.set_enumerable_(true);
        d.set_hasConfigurable_(true);
        d.set_configurable_(true);
        unsafe { *is_none = false };
        return true;
    }
    unsafe { *is_none = true };
    true
}

/// `ownPropertyKeys` reporting "0" and "1", in that order.
unsafe extern "C" fn indexed_keys(
    _cx: *mut JSContext,
    _proxy: JS::HandleObject,
    props: JS::MutableHandleIdVector,
) -> bool {
    let vec = unsafe { &mut *(props.ptr as *mut Vec<u64>) };
    for i in 0..2i32 {
        let mut id: jsid = unsafe { std::mem::zeroed() };
        unsafe { int_to_jsid(i, JS::MutableHandle { _phantom_0: Default::default(), ptr: &mut id }) };
        vec.push(id.asBits_ as u64);
    }
    true
}

/// A `defineProperty` trap that refuses everything.
unsafe extern "C" fn refusing_define(
    _cx: *mut JSContext,
    _proxy: JS::HandleObject,
    _id: JS::HandleId,
    _desc: JS::Handle<JS::PropertyDescriptor>,
    result: *mut JS::ObjectOpResult,
) -> bool {
    unsafe { (*result).code_ = 1 };
    true
}

/// Makes a Base proxy with the given traps and defines it as the global `p`.
unsafe fn make_base_proxy(
    context: &mut mozjs::context::JSContext,
    global: mozjs::gc::HandleObject,
    traps: &ProxyTraps,
) -> *mut JSObject {
    let handler = unsafe { CreateProxyHandler(traps, ptr::null()) };
    rooted!(&in(context) let privv = UndefinedValue());
    let proxy = unsafe { NewProxyObject(context, handler, privv.handle(), ptr::null_mut(), ptr::null(), false) };
    assert!(!proxy.is_null());
    rooted!(&in(context) let pv = ObjectValue(proxy));
    assert!(unsafe {
        JS_DefineProperty(
            context.raw_cx(),
            global.into(),
            c"p".as_ptr(),
            pv.handle().into(),
            JSPROP_ENUMERATE as u32,
        )
    });
    proxy
}

unsafe fn eval(context: &mut mozjs::context::JSContext, global: mozjs::gc::HandleObject, src: &str) -> Result<i32, ()> {
    let options = CompileOptionsWrapper::new(&context, c"t".to_owned(), 1);
    rooted!(&in(context) let mut rval = UndefinedValue());
    unsafe { evaluate_script(context, global, src, rval.handle_mut(), options) }?;
    Ok(rval.get().to_int32())
}



#[test]
fn base_proxy_indexed_get() {
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
        let traps = ProxyTraps { getOwnPropertyDescriptor: Some(indexed_gopd), ..Default::default() };
        make_base_proxy(context, global.handle(), &traps);
        assert_eq!(eval(context, global.handle(), "p[0]").unwrap(), 42);
        assert_eq!(eval(context, global.handle(), "p[1]").unwrap(), 43);
        assert_eq!(eval(context, global.handle(), "p[5] === undefined ? 1 : 0").unwrap(), 1);
    }
}

#[test]
fn base_proxy_has_in() {
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
        let traps = ProxyTraps { getOwnPropertyDescriptor: Some(indexed_gopd), ..Default::default() };
        make_base_proxy(context, global.handle(), &traps);
        assert_eq!(eval(context, global.handle(), "0 in p ? 1 : 0").unwrap(), 1);
        assert_eq!(eval(context, global.handle(), "1 in p ? 1 : 0").unwrap(), 1);
        assert_eq!(eval(context, global.handle(), "5 in p ? 1 : 0").unwrap(), 0);
    }
}

#[test]
fn base_proxy_object_keys() {
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
        let traps = ProxyTraps {
            getOwnPropertyDescriptor: Some(indexed_gopd),
            ownPropertyKeys: Some(indexed_keys),
            ..Default::default()
        };
        make_base_proxy(context, global.handle(), &traps);
        assert_eq!(eval(context, global.handle(), "Object.keys(p).join(',') === '0,1' ? 1 : 0").unwrap(), 1);
    }
}

#[test]
fn base_proxy_for_in() {
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
        let traps = ProxyTraps {
            getOwnPropertyDescriptor: Some(indexed_gopd),
            ownPropertyKeys: Some(indexed_keys),
            ..Default::default()
        };
        make_base_proxy(context, global.handle(), &traps);
        assert_eq!(eval(context, global.handle(), "var ks = []; for (var k in p) ks.push(k); ks.join(',') === '0,1' ? 1 : 0").unwrap(), 1);
    }
}

#[test]
fn base_proxy_define_refused_in_strict() {
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
        let traps = ProxyTraps {
            getOwnPropertyDescriptor: Some(indexed_gopd),
            defineProperty: Some(refusing_define),
            ..Default::default()
        };
        make_base_proxy(context, global.handle(), &traps);
        // Object.defineProperty always throws when the trap refuses.
        assert!(eval(context, global.handle(), "Object.defineProperty(p, '0', { value: 1 })").is_err());
        // Strict assignment throws too.
        assert!(eval(context, global.handle(), "'use strict'; p[0] = 5;").is_err());
        // The value is unchanged.
        assert_eq!(eval(context, global.handle(), "p[0]").unwrap(), 42);
    }
}

static GETTER: AtomicU64 = AtomicU64::new(0);

/// `getOwnPropertyDescriptor` reporting an accessor for "x" whose getter is the function in `GETTER`.
unsafe extern "C" fn getter_gopd(
    _cx: *mut JSContext,
    _proxy: JS::HandleObject,
    id: JS::HandleId,
    desc: JS::MutableHandle<JS::PropertyDescriptor>,
    is_none: *mut bool,
) -> bool {
    let d = unsafe { &mut *desc.ptr };
    if unsafe { RUST_JSID_IS_STRING(id) } {
        let s = unsafe { RUST_JSID_TO_STRING(id) };
        let mut eq = false;
        unsafe { JS_StringEqualsAscii(_cx, s, c"x".as_ptr(), &mut eq) };
        if eq {
            d.set_hasGetter_(true);
            d.getter_ = GETTER.load(Ordering::SeqCst) as *mut JSObject;
            d.set_hasEnumerable_(true);
            d.set_enumerable_(true);
            d.set_hasConfigurable_(true);
            d.set_configurable_(true);
            unsafe { *is_none = false };
            return true;
        }
    }
    unsafe { *is_none = true };
    true
}

#[test]
fn base_proxy_get_derived_from_descriptor_getter() {
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
        // The getter function.
        let options = CompileOptionsWrapper::new(&context, c"t".to_owned(), 1);
        rooted!(&in(context) let mut rval = UndefinedValue());
        assert!(evaluate_script(context, global.handle(), "(function () { return 7; })", rval.handle_mut(), options).is_ok());
        GETTER.store(rval.get().to_object() as u64, Ordering::SeqCst);
        // No `get` trap: Base derives [[Get]] from the descriptor and calls the getter.
        let traps = ProxyTraps { getOwnPropertyDescriptor: Some(getter_gopd), ..Default::default() };
        make_base_proxy(context, global.handle(), &traps);
        assert_eq!(eval(context, global.handle(), "p.x").unwrap(), 7);
    }
}

static DELETED: AtomicI32 = AtomicI32::new(-1);

unsafe extern "C" fn recording_delete(
    _cx: *mut JSContext,
    _proxy: JS::HandleObject,
    id: JS::HandleId,
    result: *mut JS::ObjectOpResult,
) -> bool {
    if unsafe { RUST_JSID_IS_INT(id) } {
        DELETED.store(unsafe { RUST_JSID_TO_INT(id) }, Ordering::SeqCst);
    }
    unsafe { (*result).code_ = 0 };
    true
}

#[test]
fn base_proxy_delete_trap() {
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
        let traps = ProxyTraps {
            getOwnPropertyDescriptor: Some(indexed_gopd),
            delete_: Some(recording_delete),
            ..Default::default()
        };
        make_base_proxy(context, global.handle(), &traps);
        assert_eq!(eval(context, global.handle(), "delete p[1] ? 1 : 0").unwrap(), 1);
        assert_eq!(DELETED.load(Ordering::SeqCst), 1);
    }
}

static SET_VALUE: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" fn recording_set(
    _cx: *mut JSContext,
    _proxy: JS::HandleObject,
    _id: JS::HandleId,
    v: JS::HandleValue,
    _receiver: JS::HandleValue,
    result: *mut JS::ObjectOpResult,
) -> bool {
    SET_VALUE.store(unsafe { (*v.ptr).asBits_ }, Ordering::SeqCst);
    unsafe { (*result).code_ = 0 };
    true
}

#[test]
fn base_proxy_set_trap() {
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
        let traps = ProxyTraps {
            getOwnPropertyDescriptor: Some(indexed_gopd),
            set: Some(recording_set),
            ..Default::default()
        };
        make_base_proxy(context, global.handle(), &traps);
        assert_eq!(eval(context, global.handle(), "'use strict'; p[0] = 9; 1").unwrap(), 1);
        let raw: mozjs::jsapi::JS::Value = Int32Value(9).into();
        assert_eq!(SET_VALUE.load(Ordering::SeqCst), raw.asBits_);
    }
}

#[test]
fn window_proxy_forwards_get_and_set() {
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
        // A Forwarding handler with no traps forwards everything to the target (the global).
        let traps = ProxyTraps::default();
        let handler = CreateForwardingProxyHandler(&traps, ptr::null());
        let wp = NewWindowProxy(context, global.handle(), handler);
        assert!(!wp.is_null());
        rooted!(&in(context) let wpv = ObjectValue(wp));
        assert!(JS_DefineProperty(
            context.raw_cx(),
            global.handle().into(),
            c"wp".as_ptr(),
            wpv.handle().into(),
            JSPROP_ENUMERATE as u32,
        ));
        assert_eq!(eval(context, global.handle(), "var foo = 9; wp.foo").unwrap(), 9);
        assert_eq!(eval(context, global.handle(), "wp.bar = 11; bar").unwrap(), 11);
        assert_eq!(eval(context, global.handle(), "'bar' in wp ? 1 : 0").unwrap(), 1);
    }
}

#[test]
fn to_window_if_window_proxy() {
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
        let traps = ProxyTraps::default();
        let handler = CreateForwardingProxyHandler(&traps, ptr::null());
        let wp = NewWindowProxy(context, global.handle(), handler);
        assert!(mozjs::jsapi::js::IsWindowProxy(wp));
        assert_eq!(mozjs::jsapi::js::ToWindowIfWindowProxy(wp), global.get());
        // A plain object is its own window.
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        assert!(!mozjs::jsapi::js::IsWindowProxy(obj.get()));
        assert_eq!(mozjs::jsapi::js::ToWindowIfWindowProxy(obj.get()), obj.get());
    }
}

static GC_ID_OK: AtomicBool = AtomicBool::new(false);

/// A trap that collects inside, then checks the id string still reads "movable_key".
unsafe extern "C" fn gc_gopd(
    cx: *mut JSContext,
    _proxy: JS::HandleObject,
    id: JS::HandleId,
    _desc: JS::MutableHandle<JS::PropertyDescriptor>,
    is_none: *mut bool,
) -> bool {
    unsafe {
        JS_GC(cx, JS::GCReason::API);
        let ok = RUST_JSID_IS_STRING(id);
        if ok {
            let s: *mut JSString = RUST_JSID_TO_STRING(id);
            let mut eq = false;
            JS_StringEqualsAscii(cx, s, c"movable_key".as_ptr(), &mut eq);
            GC_ID_OK.store(eq, Ordering::SeqCst);
        } else {
            GC_ID_OK.store(false, Ordering::SeqCst);
        }
        *is_none = true;
    }
    true
}

#[test]
fn gc_inside_trap_keeps_id() {
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
        let traps = ProxyTraps { getOwnPropertyDescriptor: Some(gc_gopd), ..Default::default() };
        make_base_proxy(context, global.handle(), &traps);
        // Allocate some garbage so the GC actually moves the id string.
        assert!(eval(context, global.handle(), "var junk = []; for (var i = 0; i < 1000; i++) junk.push({ i: i }); 1").is_ok());
        assert_eq!(eval(context, global.handle(), "p.movable_key === undefined ? 1 : 0").unwrap(), 1);
        assert!(GC_ID_OK.load(Ordering::SeqCst));
    }
}

#[test]
fn proxy_private_round_trip() {
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
        let traps = ProxyTraps { getOwnPropertyDescriptor: Some(indexed_gopd), ..Default::default() };
        let proxy = make_base_proxy(context, global.handle(), &traps);
        let raw: JS::Value = Int32Value(123).into();
        SetProxyPrivate(proxy, &raw);
        let mut out: JS::Value = JS::Value { asBits_: 0 };
        GetProxyPrivate(proxy, &mut out);
        assert_eq!(out.asBits_, raw.asBits_);
    }
}

#[test]
fn proxy_reserved_slots_round_trip() {
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
        let traps = ProxyTraps { getOwnPropertyDescriptor: Some(indexed_gopd), ..Default::default() };
        let proxy = make_base_proxy(context, global.handle(), &traps);
        for slot in 0..2u32 {
            let raw: JS::Value = Int32Value(100 + slot as i32).into();
            SetProxyReservedSlot(proxy, slot, &raw);
            let mut out: JS::Value = JS::Value { asBits_: 0 };
            GetProxyReservedSlot(proxy, slot, &mut out);
            assert_eq!(out.asBits_, raw.asBits_);
        }
    }
}

unsafe extern "C" fn call_trap(
    _cx: *mut JSContext,
    _proxy: JS::HandleObject,
    args: *const JS::CallArgs,
) -> bool {
    unsafe {
        let argc = (*args).argc_;
        let ca = mozjs::jsapi::CallArgs::from_vp((*args).argv_.sub(2), argc);
        let a = ca.get(0).to_int32();
        ca.rval().set(Int32Value(a + 1).into());
    }
    true
}

#[test]
fn callable_proxy_call_trap() {
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
        let options = CompileOptionsWrapper::new(&context, c"t".to_owned(), 1);
        rooted!(&in(context) let mut rval = UndefinedValue());
        assert!(evaluate_script(context, global.handle(), "(function (a) { return a; })", rval.handle_mut(), options).is_ok());
        // The private value is a callable, so the proxy is callable and its `call` trap runs.
        let traps = ProxyTraps { call: Some(call_trap), ..Default::default() };
        let handler = CreateProxyHandler(&traps, ptr::null());
        rooted!(&in(context) let privv = ObjectValue(rval.get().to_object()));
        let proxy = NewProxyObject(context, handler, privv.handle(), ptr::null_mut(), ptr::null(), false);
        assert!(!proxy.is_null());
        rooted!(&in(context) let pv = ObjectValue(proxy));
        assert!(JS_DefineProperty(
            context.raw_cx(),
            global.handle().into(),
            c"p".as_ptr(),
            pv.handle().into(),
            JSPROP_ENUMERATE as u32,
        ));
        assert_eq!(eval(context, global.handle(), "p(41)").unwrap(), 42);
    }
}

#[test]
fn forwarding_proxy_has_and_get_on_target() {
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
        rooted!(&in(context) let target = JS_NewPlainObject(context));
        rooted!(&in(context) let v = Int32Value(77));
        assert!(JS_DefineProperty(
            context.raw_cx(),
            target.handle().into(),
            c"x".as_ptr(),
            v.handle().into(),
            JSPROP_ENUMERATE as u32,
        ));
        let traps = ProxyTraps::default();
        let handler = CreateForwardingProxyHandler(&traps, ptr::null());
        rooted!(&in(context) let tv = ObjectValue(target.get()));
        let proxy = NewProxyObject(context, handler, tv.handle(), ptr::null_mut(), ptr::null(), false);
        rooted!(&in(context) let pv = ObjectValue(proxy));
        assert!(JS_DefineProperty(
            context.raw_cx(),
            global.handle().into(),
            c"fp".as_ptr(),
            pv.handle().into(),
            JSPROP_ENUMERATE as u32,
        ));
        assert_eq!(eval(context, global.handle(), "fp.x").unwrap(), 77);
        assert_eq!(eval(context, global.handle(), "'x' in fp ? 1 : 0").unwrap(), 1);
        assert_eq!(eval(context, global.handle(), "'y' in fp ? 1 : 0").unwrap(), 0);
    }
}

/// A `getOwnPropertyDescriptor` trap that re-enters the API before it returns: for "x" it puts a fresh object in the
/// descriptor (rooted only there), defines `y: 7` on it through JS_DefineProperty and forces a GC, then reports the object.
unsafe extern "C" fn reentrant_gopd(
    cx: *mut JSContext,
    _proxy: JS::HandleObject,
    id: JS::HandleId,
    desc: JS::MutableHandle<JS::PropertyDescriptor>,
    is_none: *mut bool,
) -> bool {
    unsafe {
        let mut is_x = false;
        if RUST_JSID_IS_STRING(id) {
            JS_StringEqualsAscii(cx, RUST_JSID_TO_STRING(id), c"x".as_ptr(), &mut is_x);
        }
        if !is_x {
            *is_none = true;
            return true;
        }
        let d = desc.ptr;
        (*d).value_ = ObjectValue(mozjs::jsapi::JS_NewPlainObject(cx)).into();
        // The descriptor is the object's only root; a handle on its value slot is a handle on the object.
        let obj = JS::Handle { _phantom_0: Default::default(), ptr: ptr::addr_of!((*d).value_).cast::<*mut JSObject>() };
        let seven: JS::Value = Int32Value(7).into();
        let ok = JS_DefineProperty(cx, obj, c"y".as_ptr(), JS::Handle { _phantom_0: Default::default(), ptr: &seven }, JSPROP_ENUMERATE as u32);
        JS_GC(cx, JS::GCReason::API);
        (*d).set_hasValue_(true);
        (*d).set_hasWritable_(true);
        (*d).set_writable_(true);
        (*d).set_hasEnumerable_(true);
        (*d).set_enumerable_(true);
        (*d).set_hasConfigurable_(true);
        (*d).set_configurable_(true);
        *is_none = false;
        ok
    }
}

/// The trap re-enters the engine (a define and a GC) while the outer `[[GetOwnProperty]]` holds its frame and descriptor;
/// the outer call then reads the descriptor the trap filled, with the object moved by the GC.
#[test]
fn reentrant_trap_result_read_after_gc() {
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
        let traps = ProxyTraps { getOwnPropertyDescriptor: Some(reentrant_gopd), ..Default::default() };
        make_base_proxy(context, global.handle(), &traps);
        assert!(eval(context, global.handle(), "var junk = []; for (var i = 0; i < 1000; i++) junk.push({ i: i }); 1").is_ok());
        assert_eq!(eval(context, global.handle(), "p.x.y").unwrap(), 7);
        assert_eq!(eval(context, global.handle(), "var o = Object.getOwnPropertyDescriptor(p, 'x'); junk = null; o.value.y + 1").unwrap(), 8);
    }
}

#[test]
fn transplant_window_proxy_keeps_identity_and_retargets() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let g1 = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        rooted!(&in(context) let g2 = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let traps = ProxyTraps::default();
        let handler = CreateForwardingProxyHandler(&traps, ptr::null());
        {
            let mut realm = AutoRealm::new_from_handle(context, g2.handle());
            let context = &mut realm;
            assert_eq!(eval(context, g2.handle(), "var which = 2; which").unwrap(), 2);
        }
        let mut realm = AutoRealm::new_from_handle(context, g1.handle());
        let context = &mut realm;
        assert_eq!(eval(context, g1.handle(), "var which = 1; which").unwrap(), 1);
        // The old proxy targets g1 and is what page code holds (`wp`); the new one targets g2.
        rooted!(&in(context) let old = NewWindowProxy(context, g1.handle(), handler));
        rooted!(&in(context) let fresh = NewWindowProxy(context, g2.handle(), handler));
        rooted!(&in(context) let oldv = ObjectValue(old.get()));
        assert!(JS_DefineProperty(context.raw_cx(), g1.handle().into(), c"wp".as_ptr(), oldv.handle().into(), JSPROP_ENUMERATE as u32));
        assert_eq!(eval(context, g1.handle(), "var held = wp; wp.which").unwrap(), 1);

        let result = mozjs::jsapi::JS_TransplantObject(context.raw_cx(), old.handle().into(), fresh.handle().into());
        // Same identity, new target.
        assert_eq!(result, old.get());
        assert_eq!(eval(context, g1.handle(), "wp.which").unwrap(), 2);
        assert_eq!(eval(context, g1.handle(), "held === wp ? 1 : 0").unwrap(), 1);
        JS_GC(context.raw_cx(), JS::GCReason::API);
        assert_eq!(eval(context, g1.handle(), "held.which").unwrap(), 2);
    }
}
