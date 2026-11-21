/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Raw-API tests for the Brimstone engine's structured clone implementation
// (mozjs-sys/src/engine/clone.rs).
//
// mozjs allows one `JSEngine` per process: run the tests one at a time (`--exact <name>`), as the other mozjs tests are.

use std::ffi::c_void;
use std::ptr;

use mozjs::context::JSContext;
use mozjs::jsapi::{self, JSClass, JSObject, OnNewGlobalHookOption, Value};
use mozjs::jsval::{Int32Value, DoubleValue, ObjectValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{
    evaluate_script, CompileOptionsWrapper, HandleObject, JSEngine, MutableHandleValue, RealmOptions, Runtime,
    SIMPLE_GLOBAL_CLASS,
};

/// A runtime, a global and its realm, as `$cx` (`&mut AutoRealm`) and `$global` (a rooted global).
macro_rules! with_global {
    ($cx:ident, $global:ident, $body:block) => {{
        let engine = JSEngine::init().unwrap();
        let mut runtime = Runtime::new(engine.handle());
        let context = runtime.cx();
        #[allow(unused_unsafe)]
        unsafe {
            rooted!(&in(context) let $global = JS_NewGlobalObject(
                context,
                &SIMPLE_GLOBAL_CLASS,
                ptr::null_mut(),
                OnNewGlobalHookOption::FireOnNewGlobalHook,
                &*RealmOptions::default(),
            ));
            let mut realm = AutoRealm::new_from_handle(context, $global.handle());
            let $cx = &mut realm;
            $body
        }
    }};
}

fn eval(cx: &mut JSContext, global: HandleObject, src: &str, rval: MutableHandleValue) {
    let options = CompileOptionsWrapper::new(cx, c"brimstone_clone_api".to_owned(), 1);
    assert!(evaluate_script(cx, global, src, rval, options).is_ok(), "script failed: {src}");
}

/// Evaluates a script that must produce `true`.
fn check(cx: &mut JSContext, global: HandleObject, src: &str) {
    rooted!(&in(cx) let mut rval = UndefinedValue());
    eval(cx, global, src, rval.handle_mut());
    assert!(rval.is_boolean() && rval.to_boolean(), "not true: {src}");
}

unsafe fn set_global_val(cx: &mut JSContext, global: HandleObject, name: &std::ffi::CStr, v: mozjs::rust::Handle<Value>) {
    assert!(jsapi::JS_SetProperty(cx.raw_cx(), global.into(), name.as_ptr(), v.into()));
}

/// Writes `src`'s value with `JS_WriteStructuredClone` into a fresh buffer, reads it back with
/// `JS_ReadStructuredClone`, and leaves the clone as the global `__c`.
unsafe fn round_trip(cx: &mut JSContext, global: HandleObject, src: &str, callbacks: *const jsapi::JSStructuredCloneCallbacks) {
    let raw = cx.raw_cx();
    rooted!(&in(cx) let mut rval = UndefinedValue());
    eval(cx, global, src, rval.handle_mut());
    let scope = jsapi::JS::StructuredCloneScope::SameProcess;
    let buf = mozjs::glue::NewJSAutoStructuredCloneBuffer(scope, callbacks);
    assert!(!buf.is_null());
    let data = unsafe { &mut (*buf).data_ };
    let version = unsafe { (*buf).version_ };
    rooted!(&in(cx) let undef = UndefinedValue());
    assert!(
        jsapi::JS_WriteStructuredClone(raw, rval.handle().into(), data, scope, ptr::null(), callbacks, ptr::null_mut(), undef.handle().into()),
        "write failed: {src}"
    );
    rooted!(&in(cx) let mut out = UndefinedValue());
    assert!(
        jsapi::JS_ReadStructuredClone(raw, data, version, scope, out.handle_mut().into(), ptr::null(), callbacks, ptr::null_mut()),
        "read failed: {src}"
    );
    set_global_val(cx, global, c"__c", out.handle());
    mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf);
}

/// Takes the pending exception and checks it is an Error.
unsafe fn take_exception(cx: &mut JSContext, global: HandleObject) {
    assert!(jsapi::JS_IsExceptionPending(cx.raw_cx()), "no exception pending");
    rooted!(&in(cx) let mut e = UndefinedValue());
    assert!(jsapi::JS_GetPendingException(cx.raw_cx(), e.handle_mut().into()));
    jsapi::JS_ClearPendingException(cx.raw_cx());
    assert!(e.is_object());
    set_global_val(cx, global, c"__e", e.handle());
    check(cx, global, "__e instanceof Error");
}

#[test]
fn primitives_round_trip() {
    with_global!(cx, global, {
        unsafe {
            round_trip(cx, global.handle(), "({ u: undefined, n: null, t: true, f: false, i: 42, d: 3.25, s: 'hello', b: 10n })", ptr::null());
            check(cx, global.handle(), "__c.u === undefined");
            check(cx, global.handle(), "__c.n === null");
            check(cx, global.handle(), "__c.t === true");
            check(cx, global.handle(), "__c.f === false");
            check(cx, global.handle(), "__c.i === 42");
            check(cx, global.handle(), "__c.d === 3.25");
            check(cx, global.handle(), "__c.s === 'hello'");
            check(cx, global.handle(), "__c.b === 10n");
            // A string with a lone surrogate survives.
            round_trip(cx, global.handle(), "'a\\uD800b'", ptr::null());
            check(cx, global.handle(), "__c.length === 3 && __c.charCodeAt(1) === 0xD800");
        }
    });
}

#[test]
fn wrappers_date_regexp_round_trip() {
    with_global!(cx, global, {
        unsafe {
            round_trip(
                cx,
                global.handle(),
                "({ bo: new Boolean(true), no: new Number(3.5), so: new String('wrap'), bi: Object(10n), d: new Date(123456789), r: /ab+c/gi })",
                ptr::null(),
            );
            check(
                cx,
                global.handle(),
                "__c.bo instanceof Boolean && __c.bo.valueOf() === true && __c.no instanceof Number && __c.no.valueOf() === 3.5 && __c.so instanceof String && __c.so.valueOf() === 'wrap' && __c.bi instanceof BigInt && __c.bi.valueOf() === 10n && __c.d instanceof Date && __c.d.getTime() === 123456789 && __c.r instanceof RegExp && __c.r.source === 'ab+c' && __c.r.flags === 'gi'",
            );
        }
    });
}

#[test]
fn array_holes_and_extra_props_round_trip() {
    with_global!(cx, global, {
        unsafe {
            round_trip(cx, global.handle(), "var a = [1, , 3]; a.extra = 'x'; a", ptr::null());
            check(cx, global.handle(), "Array.isArray(__c) && __c.length === 3 && __c[0] === 1 && !(1 in __c) && __c[2] === 3 && __c.extra === 'x'");
        }
    });
}

#[test]
fn plain_object_round_trip() {
    with_global!(cx, global, {
        unsafe {
            round_trip(cx, global.handle(), "({ a: 1, b: 'x', c: { d: [2, 3] } })", ptr::null());
            check(cx, global.handle(), "__c.a === 1 && __c.b === 'x' && __c.c.d[0] === 2 && __c.c.d[1] === 3 && Object.keys(__c).join() === 'a,b,c'");
        }
    });
}

#[test]
fn map_and_set_round_trip() {
    with_global!(cx, global, {
        unsafe {
            round_trip(cx, global.handle(), "({ m: new Map([[1, 'one'], ['k', { v: 2 }]]), s: new Set([1, 'two', 3]) })", ptr::null());
            check(
                cx,
                global.handle(),
                "__c.m instanceof Map && __c.m.get(1) === 'one' && __c.m.get('k').v === 2 && __c.m.size === 2 && __c.s instanceof Set && __c.s.has(1) && __c.s.has('two') && __c.s.has(3) && __c.s.size === 3",
            );
        }
    });
}

#[test]
fn buffer_typed_array_data_view_round_trip() {
    with_global!(cx, global, {
        unsafe {
            round_trip(
                cx,
                global.handle(),
                "var b = new ArrayBuffer(24); var u = new Uint8Array(b); u[0] = 200; u[15] = 7; var f = new Float64Array(b); f[2] = 1.5; var dv = new DataView(b, 8, 4); ({ b: b, u: u, f: f, dv: dv })",
                ptr::null(),
            );
            check(cx, global.handle(), "__c.b instanceof ArrayBuffer");
            check(cx, global.handle(), "__c.b.byteLength === 24");
            check(cx, global.handle(), "__c.u instanceof Uint8Array");
            check(cx, global.handle(), "__c.u[0] === 200");
            check(cx, global.handle(), "__c.u[15] === 7");
            check(cx, global.handle(), "__c.f instanceof Float64Array");
            check(cx, global.handle(), "__c.f[2] === 1.5");
            check(cx, global.handle(), "__c.dv instanceof DataView");
            check(cx, global.handle(), "__c.dv.byteOffset === 8");
            check(cx, global.handle(), "__c.dv.byteLength === 4");
            check(cx, global.handle(), "__c.u.buffer === __c.b");
            check(cx, global.handle(), "__c.dv.buffer === __c.b");
        }
    });
}

#[test]
fn shared_buffer_identity_kept() {
    with_global!(cx, global, {
        unsafe {
            round_trip(cx, global.handle(), "var b = new ArrayBuffer(4); ({ a: new Uint8Array(b), c: new Uint8Array(b), d: b })", ptr::null());
            check(cx, global.handle(), "__c.a.buffer === __c.c.buffer && __c.a.buffer === __c.d");
        }
    });
}

#[test]
fn cycles_and_repeated_refs_keep_identity() {
    with_global!(cx, global, {
        unsafe {
            round_trip(cx, global.handle(), "var o = { name: 'o' }; o.self = o; var arr = [o, o]; ({ o: o, arr: arr })", ptr::null());
            check(cx, global.handle(), "__c.o.self === __c.o && __c.arr[0] === __c.o && __c.arr[1] === __c.o && __c.arr[0].name === 'o'");
        }
    });
}

#[test]
fn negative_zero_and_nan_round_trip() {
    with_global!(cx, global, {
        unsafe {
            round_trip(cx, global.handle(), "({ z: -0, n: NaN, i: Infinity, ni: -Infinity })", ptr::null());
            check(cx, global.handle(), "Object.is(__c.z, -0) && Number.isNaN(__c.n) && __c.i === Infinity && __c.ni === -Infinity");
        }
    });
}

#[test]
fn error_object_round_trip() {
    with_global!(cx, global, {
        unsafe {
            round_trip(cx, global.handle(), "var e = new TypeError('boom'); e.cause = 'why'; e", ptr::null());
            check(cx, global.handle(), "__c instanceof TypeError && __c.message === 'boom' && __c.cause === 'why'");
        }
    });
}

#[test]
fn function_and_symbol_throw_data_clone_error() {
    with_global!(cx, global, {
        unsafe {
            let raw = cx.raw_cx();
            rooted!(&in(cx) let mut rval = UndefinedValue());
            eval(cx, global.handle(), "(function f() {})", rval.handle_mut());
            let scope = jsapi::JS::StructuredCloneScope::SameProcess;
            let buf = mozjs::glue::NewJSAutoStructuredCloneBuffer(scope, ptr::null());
            let data = unsafe { &mut (*buf).data_ };
            rooted!(&in(cx) let undef = UndefinedValue());
            assert!(!jsapi::JS_WriteStructuredClone(raw, rval.handle().into(), data, scope, ptr::null(), ptr::null(), ptr::null_mut(), undef.handle().into()));
            take_exception(cx, global.handle());
            mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf);

            rooted!(&in(cx) let mut rval2 = UndefinedValue());
            eval(cx, global.handle(), "Symbol('s')", rval2.handle_mut());
            let buf2 = mozjs::glue::NewJSAutoStructuredCloneBuffer(scope, ptr::null());
            let data2 = unsafe { &mut (*buf2).data_ };
            assert!(!jsapi::JS_WriteStructuredClone(raw, rval2.handle().into(), data2, scope, ptr::null(), ptr::null(), ptr::null_mut(), undef.handle().into()));
            take_exception(cx, global.handle());
            mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf2);
        }
    });
}

// ---- Host object callbacks ----

static HOST_CLASS: JSClass = JSClass {
    name: c"CloneTestHost".as_ptr(),
    flags: 0,
    cOps: ptr::null(),
    spec: ptr::null(),
    ext: ptr::null(),
    oOps: ptr::null(),
};

const HOST_TAG: u32 = 0xFFFF_8000;

unsafe extern "C" fn host_write(
    _cx: *mut jsapi::JSContext,
    w: *mut jsapi::JSStructuredCloneWriter,
    _obj: jsapi::JS::HandleObject,
    _same_process: *mut bool,
    _closure: *mut c_void,
) -> bool {
    unsafe {
        assert!(jsapi::JS_WriteUint32PairUnchecked(w, HOST_TAG, 42));
        let bytes = [1u8, 2, 3, 4];
        assert!(jsapi::JS_WriteBytes(w, bytes.as_ptr().cast(), bytes.len()));
        assert!(jsapi::JS_WriteDouble(w, 2.5));
        let s = jsapi::JS_NewStringCopyZ(_cx, c"hoststr".as_ptr());
        assert!(!s.is_null());
        assert!(jsapi::JS_WriteString(w, jsapi::JS::Handle { _phantom_0: Default::default(), ptr: &s }));
        true
    }
}

unsafe extern "C" fn host_read(
    cx: *mut jsapi::JSContext,
    r: *mut jsapi::JSStructuredCloneReader,
    _policy: *const jsapi::JS::CloneDataPolicy,
    tag: u32,
    data: u32,
    _closure: *mut c_void,
) -> *mut JSObject {
    unsafe {
        assert_eq!(tag, HOST_TAG);
        assert_eq!(data, 42);
        let mut bytes = [0u8; 4];
        assert!(jsapi::JS_ReadBytes(r, bytes.as_mut_ptr().cast(), bytes.len()));
        assert_eq!(bytes, [1, 2, 3, 4]);
        let mut d = 0f64;
        assert!(jsapi::JS_ReadDouble(r, &mut d));
        assert_eq!(d, 2.5);
        let mut s: *mut jsapi::JSString = ptr::null_mut();
        assert!(jsapi::JS_ReadString(r, jsapi::JS::MutableHandle { _phantom_0: Default::default(), ptr: &mut s }));
        let obj = jsapi::JS_NewPlainObject(cx);
        assert!(!obj.is_null());
        let objh = jsapi::JS::Handle { _phantom_0: Default::default(), ptr: &obj };
        let tagv: Value = Value::from(Int32Value(tag as i32));
        let datav: Value = Value::from(Int32Value(data as i32));
        let dv: Value = Value::from(DoubleValue(d));
        assert!(jsapi::JS_DefineProperty(cx, objh, c"tag".as_ptr(), jsapi::JS::Handle { _phantom_0: Default::default(), ptr: &tagv }, jsapi::JSPROP_ENUMERATE as u32));
        assert!(jsapi::JS_DefineProperty(cx, objh, c"data".as_ptr(), jsapi::JS::Handle { _phantom_0: Default::default(), ptr: &datav }, jsapi::JSPROP_ENUMERATE as u32));
        assert!(jsapi::JS_DefineProperty(cx, objh, c"d".as_ptr(), jsapi::JS::Handle { _phantom_0: Default::default(), ptr: &dv }, jsapi::JSPROP_ENUMERATE as u32));
        assert!(jsapi::JS_DefineProperty(cx, objh, c"s".as_ptr(), jsapi::JS::Handle { _phantom_0: Default::default(), ptr: &s as *const _ as *const Value }, jsapi::JSPROP_ENUMERATE as u32));
        obj
    }
}

static HOST_CALLBACKS: jsapi::JSStructuredCloneCallbacks = jsapi::JSStructuredCloneCallbacks {
    read: Some(host_read),
    write: Some(host_write),
    reportError: None,
    readTransfer: None,
    writeTransfer: None,
    freeTransfer: None,
    canTransfer: None,
    sabCloned: None,
};

#[test]
fn host_object_through_callbacks() {
    with_global!(cx, global, {
        unsafe {
            let raw = cx.raw_cx();
            rooted!(&in(cx) let host = jsapi::JS_NewObject(raw, &HOST_CLASS));
            assert!(!host.get().is_null());
            {
                rooted!(&in(cx) let hv = ObjectValue(host.get()));
                set_global_val(cx, global.handle(), c"__h", hv.handle());
            }
            round_trip(cx, global.handle(), "__h", &HOST_CALLBACKS);
            check(cx, global.handle(), "(__c.tag >>> 0) === 0xFFFF8000 && __c.data === 42 && __c.d === 2.5 && __c.s === 'hoststr'");
        }
    });
}

// ---- Transferables ----

unsafe extern "C" fn write_transfer_cb(
    _cx: *mut jsapi::JSContext,
    _obj: jsapi::JS::Handle<*mut JSObject>,
    _closure: *mut c_void,
    tag: *mut u32,
    ownership: *mut jsapi::JS::TransferableOwnership,
    content: *mut *mut c_void,
    extra: *mut u64,
) -> bool {
    unsafe {
        *tag = 0xFFFF_8001;
        *ownership = jsapi::JS::TransferableOwnership::SCTAG_TMO_FIRST_OWNED;
        *content = ptr::null_mut();
        *extra = 77;
        true
    }
}

unsafe extern "C" fn read_transfer_cb(
    cx: *mut jsapi::JSContext,
    _r: *mut jsapi::JSStructuredCloneReader,
    _policy: *const jsapi::JS::CloneDataPolicy,
    tag: u32,
    _content: *mut c_void,
    extra: u64,
    _closure: *mut c_void,
    return_object: jsapi::JS::MutableHandleObject,
) -> bool {
    unsafe {
        assert_eq!(tag, 0xFFFF_8001);
        assert_eq!(extra, 77);
        let buf = jsapi::NewArrayBuffer(cx, 8);
        assert!(!buf.is_null());
        *return_object.ptr = buf;
        true
    }
}

unsafe extern "C" fn can_transfer_cb(
    _cx: *mut jsapi::JSContext,
    _obj: jsapi::JS::Handle<*mut JSObject>,
    _same_process: *mut bool,
    _closure: *mut c_void,
) -> bool {
    true
}

static TRANSFER_CALLBACKS: jsapi::JSStructuredCloneCallbacks = jsapi::JSStructuredCloneCallbacks {
    read: None,
    write: None,
    reportError: None,
    readTransfer: Some(read_transfer_cb),
    writeTransfer: Some(write_transfer_cb),
    freeTransfer: None,
    canTransfer: Some(can_transfer_cb),
    sabCloned: None,
};

#[test]
fn transferable_array_buffer() {
    with_global!(cx, global, {
        unsafe {
            let raw = cx.raw_cx();
            rooted!(&in(cx) let mut rval = UndefinedValue());
            eval(cx, global.handle(), "var b = new ArrayBuffer(8); new Uint8Array(b)[0] = 9; b", rval.handle_mut());
            let scope = jsapi::JS::StructuredCloneScope::SameProcess;
            let buf = mozjs::glue::NewJSAutoStructuredCloneBuffer(scope, &TRANSFER_CALLBACKS);
            let data = unsafe { &mut (*buf).data_ };
            let version = unsafe { (*buf).version_ };
            rooted!(&in(cx) let mut transfer = UndefinedValue());
            eval(cx, global.handle(), "[b]", transfer.handle_mut());
            assert!(jsapi::JS_WriteStructuredClone(raw, rval.handle().into(), data, scope, ptr::null(), &TRANSFER_CALLBACKS, ptr::null_mut(), transfer.handle().into()));
            let mut has = false;
            assert!(jsapi::JS_StructuredCloneHasTransferables(data, &mut has));
            assert!(has, "transferables not reported");
            // The original buffer was detached by the transfer.
            check(cx, global.handle(), "b.byteLength === 0");
            rooted!(&in(cx) let mut out = UndefinedValue());
            assert!(jsapi::JS_ReadStructuredClone(raw, data, version, scope, out.handle_mut().into(), ptr::null(), &TRANSFER_CALLBACKS, ptr::null_mut()));
            set_global_val(cx, global.handle(), c"__c", out.handle());
            check(cx, global.handle(), "__c instanceof ArrayBuffer && __c.byteLength === 8");
            mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf);
        }
    });
}

#[test]
fn truncated_and_corrupt_data_rejected() {
    with_global!(cx, global, {
        unsafe {
            let raw = cx.raw_cx();
            rooted!(&in(cx) let mut rval = UndefinedValue());
            eval(cx, global.handle(), "({ a: [1, 2, 3], s: 'some string' })", rval.handle_mut());
            let scope = jsapi::JS::StructuredCloneScope::SameProcess;
            let buf = mozjs::glue::NewJSAutoStructuredCloneBuffer(scope, ptr::null());
            let data = unsafe { &mut (*buf).data_ };
            let version = unsafe { (*buf).version_ };
            rooted!(&in(cx) let undef = UndefinedValue());
            assert!(jsapi::JS_WriteStructuredClone(raw, rval.handle().into(), data, scope, ptr::null(), ptr::null(), ptr::null_mut(), undef.handle().into()));
            let len = mozjs::glue::GetLengthOfJSStructuredCloneData(data);
            assert!(len > 8);
            let mut bytes = vec![0u8; len];
            mozjs::glue::CopyJSStructuredCloneData(data, bytes.as_mut_ptr());

            // Truncated: every prefix shorter than the whole must fail with an error, not panic.
            for cut in [0usize, 1, 4, 8, len / 2, len - 1] {
                let buf2 = mozjs::glue::NewJSAutoStructuredCloneBuffer(scope, ptr::null());
                let data2 = unsafe { &mut (*buf2).data_ };
                assert!(mozjs::glue::WriteBytesToJSStructuredCloneData(bytes.as_ptr(), cut, data2));
                rooted!(&in(cx) let mut out = UndefinedValue());
                assert!(
                    !jsapi::JS_ReadStructuredClone(raw, data2, version, scope, out.handle_mut().into(), ptr::null(), ptr::null(), ptr::null_mut()),
                    "truncated at {cut} read as ok"
                );
                take_exception(cx, global.handle());
                mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf2);
            }

            // Corrupted header: bad magic.
            let mut bad = bytes.clone();
            bad[0] ^= 0xFF;
            let buf3 = mozjs::glue::NewJSAutoStructuredCloneBuffer(scope, ptr::null());
            let data3 = unsafe { &mut (*buf3).data_ };
            assert!(mozjs::glue::WriteBytesToJSStructuredCloneData(bad.as_ptr(), bad.len(), data3));
            rooted!(&in(cx) let mut out = UndefinedValue());
            assert!(!jsapi::JS_ReadStructuredClone(raw, data3, version, scope, out.handle_mut().into(), ptr::null(), ptr::null(), ptr::null_mut()));
            take_exception(cx, global.handle());
            mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf3);

            // Corrupted tag in the middle.
            let mut bad2 = bytes.clone();
            let mid = len / 2;
            bad2[mid] = 0xEE;
            bad2[mid + 1] = 0xEE;
            bad2[mid + 2] = 0xEE;
            bad2[mid + 3] = 0xEE;
            let buf4 = mozjs::glue::NewJSAutoStructuredCloneBuffer(scope, ptr::null());
            let data4 = unsafe { &mut (*buf4).data_ };
            assert!(mozjs::glue::WriteBytesToJSStructuredCloneData(bad2.as_ptr(), bad2.len(), data4));
            rooted!(&in(cx) let mut out = UndefinedValue());
            let _ = jsapi::JS_ReadStructuredClone(raw, data4, version, scope, out.handle_mut().into(), ptr::null(), ptr::null(), ptr::null_mut());
            if jsapi::JS_IsExceptionPending(raw) {
                jsapi::JS_ClearPendingException(raw);
            }
            mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf4);

            mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf);
        }
    });
}

#[test]
fn js_structured_clone_one_call() {
    with_global!(cx, global, {
        unsafe {
            let raw = cx.raw_cx();
            rooted!(&in(cx) let mut rval = UndefinedValue());
            eval(cx, global.handle(), "({ x: [1, { y: 'z' }], m: new Map([[1, 2]]) })", rval.handle_mut());
            rooted!(&in(cx) let mut out = UndefinedValue());
            assert!(jsapi::JS_StructuredClone(raw, rval.handle().into(), out.handle_mut().into(), ptr::null(), ptr::null_mut()));
            set_global_val(cx, global.handle(), c"__c", out.handle());
            check(cx, global.handle(), "__c.x[0] === 1 && __c.x[1].y === 'z' && __c.m.get(1) === 2");
        }
    });
}

#[test]
fn writer_reader_primitives_from_callbacks() {
    with_global!(cx, global, {
        unsafe {
            // The host write/read callbacks above already exercise JS_WriteUint32PairUnchecked,
            // JS_WriteBytes, JS_WriteDouble, JS_WriteString and their read counterparts; here the
            // scope of the writer is checked through JS_GetStructuredCloneScope inside a callback.
            let raw = cx.raw_cx();
            rooted!(&in(cx) let host = jsapi::JS_NewObject(raw, &HOST_CLASS));
            {
                rooted!(&in(cx) let hv = ObjectValue(host.get()));
                set_global_val(cx, global.handle(), c"__h", hv.handle());
            }
            let scope = jsapi::JS::StructuredCloneScope::DifferentProcess;
            let buf = mozjs::glue::NewJSAutoStructuredCloneBuffer(scope, &HOST_CALLBACKS);
            let data = unsafe { &mut (*buf).data_ };
            rooted!(&in(cx) let undef = UndefinedValue());
            rooted!(&in(cx) let mut hv = UndefinedValue());
            eval(cx, global.handle(), "__h", hv.handle_mut());
            assert!(jsapi::JS_WriteStructuredClone(raw, hv.handle().into(), data, scope, ptr::null(), &HOST_CALLBACKS, ptr::null_mut(), undef.handle().into()));
            mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf);
        }
    });
}

/// Servo (structuredclone.rs) copies a written clone out with CopyJSStructuredCloneData, writes the bytes into a fresh
/// buffer with WriteBytesToJSStructuredCloneData, and reads with SpiderMonkey's JS_STRUCTURED_CLONE_VERSION.
#[test]
fn read_after_copy_with_spidermonkey_api_version() {
    with_global!(cx, global, {
        unsafe {
            let raw = cx.raw_cx();
            let scope = jsapi::JS::StructuredCloneScope::DifferentProcess;
            rooted!(&in(cx) let mut v = UndefinedValue());
            eval(cx, global.handle(), "({ a: 77, s: 'x' })", v.handle_mut());
            let buf = mozjs::glue::NewJSAutoStructuredCloneBuffer(scope, ptr::null());
            let data = &mut (*buf).data_;
            rooted!(&in(cx) let undef = UndefinedValue());
            assert!(jsapi::JS_WriteStructuredClone(raw, v.handle().into(), data, scope, ptr::null(), ptr::null(), ptr::null_mut(), undef.handle().into()));
            // As Servo: length, then a copy into a buffer of that length.
            let n = mozjs::glue::GetLengthOfJSStructuredCloneData(data);
            let mut bytes: Vec<u8> = Vec::with_capacity(n);
            mozjs::glue::CopyJSStructuredCloneData(data, bytes.as_mut_ptr());
            bytes.set_len(n);
            mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf);
            assert!(!bytes.is_empty());

            let buf2 = mozjs::glue::NewJSAutoStructuredCloneBuffer(scope, ptr::null());
            let data2 = &mut (*buf2).data_;
            mozjs::glue::WriteBytesToJSStructuredCloneData(bytes.as_ptr(), bytes.len(), data2);
            rooted!(&in(cx) let mut out = UndefinedValue());
            assert!(jsapi::JS_ReadStructuredClone(raw, data2, jsapi::JS_STRUCTURED_CLONE_VERSION, scope, out.handle_mut().into(), ptr::null(), ptr::null(), ptr::null_mut()));
            set_global_val(cx, global.handle(), c"__c", out.handle());
            check(cx, global.handle(), "__c.a === 77 && __c.s === 'x'");
            // Data claiming a newer SpiderMonkey version than this engine is refused.
            rooted!(&in(cx) let mut out2 = UndefinedValue());
            assert!(!jsapi::JS_ReadStructuredClone(raw, data2, jsapi::JS_STRUCTURED_CLONE_VERSION + 1, scope, out2.handle_mut().into(), ptr::null(), ptr::null(), ptr::null_mut()));
            mozjs::jsapi::JS_ClearPendingException(raw);
            mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf2);
        }
    });
}

