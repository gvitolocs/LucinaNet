/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Raw-API tests for the Brimstone engine's ArrayBuffer, typed array and DataView implementation
// (mozjs-sys/src/engine/typed.rs).
//
// mozjs allows one `JSEngine` per process: run the tests one at a time (`--exact <name>`), as the other mozjs tests are.

use std::ffi::CStr;
use std::ptr;

use mozjs::context::JSContext;
use mozjs::jsapi::{self, JSObject, OnNewGlobalHookOption, Type};
use mozjs::jsval::{ObjectValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{
    evaluate_script, CompileOptionsWrapper, HandleObject, JSEngine, MutableHandleValue,
    RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS,
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
    let options = CompileOptionsWrapper::new(cx, c"brimstone_typed_api".to_owned(), 1);
    assert!(evaluate_script(cx, global, src, rval, options).is_ok(), "script failed: {src}");
}

/// Evaluates a script that must produce `true`.
fn check(cx: &mut JSContext, global: HandleObject, src: &str) {
    rooted!(&in(cx) let mut rval = UndefinedValue());
    eval(cx, global, src, rval.handle_mut());
    assert!(rval.is_boolean() && rval.to_boolean(), "not true: {src}");
}

unsafe fn set_global(cx: &mut JSContext, global: HandleObject, name: &CStr, obj: *mut JSObject) {
    rooted!(&in(cx) let v = ObjectValue(obj));
    assert!(jsapi::JS_SetProperty(cx.raw_cx(), global.into(), name.as_ptr(), v.handle().into()));
}

/// Takes the pending exception and checks it is an instance of `class` (a global constructor's name).
unsafe fn take_exception(cx: &mut JSContext, global: HandleObject, class: &str) {
    assert!(jsapi::JS_IsExceptionPending(cx.raw_cx()), "no exception pending, expected a {class}");
    rooted!(&in(cx) let mut e = UndefinedValue());
    assert!(jsapi::JS_GetPendingException(cx.raw_cx(), e.handle_mut().into()));
    jsapi::JS_ClearPendingException(cx.raw_cx());
    assert!(e.is_object());
    set_global(cx, global, c"__e", e.to_object());
    check(cx, global, &format!("__e instanceof {class}"));
}

/// Each element type: a buffer made in Rust, viewed and written by a script, read and written in Rust, read back by the script.
macro_rules! round_trip {
    ($test:ident, $class:literal, $elem:ty, $get_as:ident, $get_data:ident, $unwrap:ident, $ty:expr,
     [$a:expr, $b:expr, $c:expr], [$ja:literal, $jb:literal, $jc:literal], $write_c:literal) => {
        #[test]
        fn $test() {
            with_global!(cx, global, {
                let size = std::mem::size_of::<$elem>();
                rooted!(&in(cx) let buffer = jsapi::NewArrayBuffer(cx.raw_cx(), 3 * size));
                assert!(!buffer.get().is_null());
                set_global(cx, global.handle(), c"b", buffer.get());

                rooted!(&in(cx) let mut rval = UndefinedValue());
                let src = concat!("var t = new ", $class, "(b); t[0] = ", $ja, "; t[1] = ", $jb, "; t");
                eval(cx, global.handle(), src, rval.handle_mut());
                rooted!(&in(cx) let array = rval.to_object());

                assert_eq!(jsapi::$unwrap(array.get()), array.get());
                assert!(jsapi::JS_IsTypedArrayObject(array.get()));
                assert_eq!(jsapi::JS_GetArrayBufferViewType(array.get()), $ty);
                assert_eq!(jsapi::JS_GetTypedArrayLength(array.get()), 3);
                assert_eq!(jsapi::JS_GetTypedArrayByteLength(array.get()), 3 * size);

                let mut len = 0;
                let mut shared = true;
                let mut data: *mut $elem = ptr::null_mut();
                assert_eq!(jsapi::$get_as(array.get(), &mut len, &mut shared, &mut data), array.get());
                assert_eq!(len, 3);
                assert!(!shared);
                let values = std::slice::from_raw_parts_mut(data, len);
                assert_eq!(values[0], $a);
                assert_eq!(values[1], $b);
                values[2] = $c;

                // The view and the buffer share their bytes.
                let mut shared = true;
                let bytes = jsapi::GetArrayBufferData(buffer.get(), &mut shared, ptr::null());
                assert_eq!(bytes.cast::<$elem>(), data);
                let mut shared = true;
                assert_eq!(jsapi::$get_data(array.get(), &mut shared, ptr::null()), data);

                check(cx, global.handle(), concat!("t[2] === ", $jc));
                // A script's write after Rust's read is seen by Rust (re-read: the data may have moved).
                eval(cx, global.handle(), concat!("t[0] = ", $write_c, "; undefined"), rval.handle_mut());
                let mut shared = true;
                let data = jsapi::$get_data(array.get(), &mut shared, ptr::null());
                assert_eq!(*data, $c);
            });
        }
    };
}

round_trip!(int8_round_trip, "Int8Array", i8, JS_GetObjectAsInt8Array, JS_GetInt8ArrayData, UnwrapInt8Array,
    Type::Int8, [-128, -56, 99], ["-128", "200", "99"], "99");
round_trip!(uint8_round_trip, "Uint8Array", u8, JS_GetObjectAsUint8Array, JS_GetUint8ArrayData, UnwrapUint8Array,
    Type::Uint8, [255, 4, 7], ["-1", "260", "7"], "7");
round_trip!(uint8_clamped_round_trip, "Uint8ClampedArray", u8, JS_GetObjectAsUint8ClampedArray,
    JS_GetUint8ClampedArrayData, UnwrapUint8ClampedArray, Type::Uint8Clamped, [255, 0, 128], ["300", "-5", "128"], "128");
round_trip!(int16_round_trip, "Int16Array", i16, JS_GetObjectAsInt16Array, JS_GetInt16ArrayData, UnwrapInt16Array,
    Type::Int16, [-32768, 1, -2], ["-32768", "65537", "-2"], "-2");
round_trip!(uint16_round_trip, "Uint16Array", u16, JS_GetObjectAsUint16Array, JS_GetUint16ArrayData, UnwrapUint16Array,
    Type::Uint16, [65535, 3, 40000], ["-1", "65539", "40000"], "40000");
round_trip!(int32_round_trip, "Int32Array", i32, JS_GetObjectAsInt32Array, JS_GetInt32ArrayData, UnwrapInt32Array,
    Type::Int32, [i32::MIN, 1, -123456], ["2147483648", "4294967297", "-123456"], "-123456");
round_trip!(uint32_round_trip, "Uint32Array", u32, JS_GetObjectAsUint32Array, JS_GetUint32ArrayData, UnwrapUint32Array,
    Type::Uint32, [u32::MAX, 5, 3000000000], ["-1", "5", "3000000000"], "3000000000");
// Float16 elements are the IEEE half-precision bits: 1.5 = 0x3E00, -2 = 0xC000, 0.5 = 0x3800.
round_trip!(float16_round_trip, "Float16Array", u16, JS_GetObjectAsFloat16Array, JS_GetFloat16ArrayData,
    UnwrapFloat16Array, Type::Float16, [0x3E00, 0xC000, 0x3800], ["1.5", "-2", "0.5"], "0.5");
round_trip!(float32_round_trip, "Float32Array", f32, JS_GetObjectAsFloat32Array, JS_GetFloat32ArrayData,
    UnwrapFloat32Array, Type::Float32, [0.25, -1.5e10, 3.5], ["0.25", "-1.5e10", "3.5"], "3.5");
round_trip!(float64_round_trip, "Float64Array", f64, JS_GetObjectAsFloat64Array, JS_GetFloat64ArrayData,
    UnwrapFloat64Array, Type::Float64, [0.1, -1e300, 2.5], ["0.1", "-1e300", "2.5"], "2.5");
round_trip!(bigint64_round_trip, "BigInt64Array", i64, JS_GetObjectAsBigInt64Array, JS_GetBigInt64ArrayData,
    UnwrapBigInt64Array, Type::BigInt64, [i64::MIN, -1, 1 << 40], ["-(2n ** 63n)", "2n ** 64n - 1n", "2n ** 40n"],
    "2n ** 40n");
round_trip!(biguint64_round_trip, "BigUint64Array", u64, JS_GetObjectAsBigUint64Array, JS_GetBigUint64ArrayData,
    UnwrapBigUint64Array, Type::BigUint64, [u64::MAX, 1, 1 << 63], ["-1n", "2n ** 64n + 1n", "2n ** 63n"], "2n ** 63n");

#[test]
fn arrays_made_in_rust() {
    with_global!(cx, global, {
        rooted!(&in(cx) let a = jsapi::JS_NewFloat64Array(cx.raw_cx(), 4));
        assert_eq!(jsapi::JS_GetTypedArrayLength(a.get()), 4);
        assert_eq!(jsapi::JS_GetArrayBufferViewByteLength(a.get()), 32);
        set_global(cx, global.handle(), c"a", a.get());
        check(cx, global.handle(), "a instanceof Float64Array && a.length === 4 && a.every(x => x === 0)");

        // From an array: values converted per element type.
        rooted!(&in(cx) let mut rval = UndefinedValue());
        eval(cx, global.handle(), "[1, 2.7, -1, 'x']", rval.handle_mut());
        rooted!(&in(cx) let src = rval.to_object());
        rooted!(&in(cx) let u = jsapi::JS_NewUint8ArrayFromArray(cx.raw_cx(), src.handle().into()));
        let mut len = 0;
        let mut shared = false;
        let mut data = ptr::null_mut();
        assert!(!jsapi::JS_GetObjectAsUint8Array(u.get(), &mut len, &mut shared, &mut data).is_null());
        assert_eq!(std::slice::from_raw_parts(data, len), &[1, 2, 255, 0]);

        // A view of the wrong element type is refused.
        assert!(jsapi::UnwrapUint16Array(u.get()).is_null());
        assert!(jsapi::JS_GetUint16ArrayData(u.get(), &mut shared, ptr::null()).is_null());
        assert!(jsapi::JS_GetObjectAsInt8Array(u.get(), &mut len, &mut shared, &mut ptr::null_mut()).is_null());

        // Too many elements for any buffer.
        assert!(jsapi::JS_NewFloat64Array(cx.raw_cx(), usize::MAX / 4).is_null());
        take_exception(cx, global.handle(), "RangeError");
    });
}

#[test]
fn views_with_offsets() {
    with_global!(cx, global, {
        rooted!(&in(cx) let buffer = jsapi::NewArrayBuffer(cx.raw_cx(), 16));
        let mut shared = true;
        let base = jsapi::GetArrayBufferData(buffer.get(), &mut shared, ptr::null());
        for i in 0..16 {
            *base.add(i) = i as u8;
        }

        rooted!(&in(cx) let view = jsapi::JS_NewUint16ArrayWithBuffer(cx.raw_cx(), buffer.handle().into(), 4, 3));
        assert!(!view.get().is_null());
        assert_eq!(jsapi::JS_GetTypedArrayLength(view.get()), 3);
        assert_eq!(jsapi::JS_GetTypedArrayByteOffset(view.get()), 4);
        assert_eq!(jsapi::JS_GetTypedArrayByteLength(view.get()), 6);
        let base = jsapi::GetArrayBufferData(buffer.get(), &mut shared, ptr::null());
        let data = jsapi::JS_GetUint16ArrayData(view.get(), &mut shared, ptr::null());
        assert_eq!(data.cast::<u8>(), base.add(4));
        assert_eq!(*data, u16::from_ne_bytes([4, 5]));

        // A negative length views the rest of the buffer.
        rooted!(&in(cx) let rest = jsapi::JS_NewUint16ArrayWithBuffer(cx.raw_cx(), buffer.handle().into(), 4, -1));
        assert_eq!(jsapi::JS_GetTypedArrayLength(rest.get()), 6);

        // The view's buffer is the buffer.
        let mut shared = true;
        assert_eq!(jsapi::JS_GetArrayBufferViewBuffer(cx.raw_cx(), view.handle().into(), &mut shared), buffer.get());
        assert!(!shared);

        // DataView: bytes 2..10, written by a script, read in Rust.
        rooted!(&in(cx) let dv = jsapi::JS_NewDataView(cx.raw_cx(), buffer.handle().into(), 2, 8));
        assert!(!dv.get().is_null());
        assert!(jsapi::JS_IsArrayBufferViewObject(dv.get()));
        assert!(!jsapi::JS_IsTypedArrayObject(dv.get()));
        assert_eq!(jsapi::JS_GetArrayBufferViewType(dv.get()), Type::MaxTypedArrayViewType);
        assert_eq!(jsapi::JS_GetArrayBufferViewByteOffset(dv.get()), 2);
        assert_eq!(jsapi::JS_GetArrayBufferViewByteLength(dv.get()), 8);
        set_global(cx, global.handle(), c"dv", dv.get());
        check(cx, global.handle(), "dv.setUint32(0, 0xdeadbeef); dv.getUint8(0) === 0xde");
        let mut len = 0;
        let mut data = ptr::null_mut();
        assert_eq!(jsapi::JS_GetObjectAsArrayBufferView(dv.get(), &mut len, &mut shared, &mut data), dv.get());
        assert_eq!(std::slice::from_raw_parts(data, len), &[0xde, 0xad, 0xbe, 0xef, 6, 7, 8, 9]);

        // A small view copied into a caller's buffer.
        let mut fixed = [0u8; 96];
        assert_eq!(jsapi::JS_MaxMovableTypedArraySize(), 96);
        let p = jsapi::JS_GetArrayBufferViewFixedData(view.get(), fixed.as_mut_ptr(), fixed.len());
        assert_eq!(p, fixed.as_mut_ptr());
        // Bytes 4 and 5 hold the DataView's 0xbe 0xef now.
        assert_eq!(&fixed[..6], &[0xbe, 0xef, 6, 7, 8, 9]);
    });
}

#[test]
fn out_of_range_views_refused() {
    with_global!(cx, global, {
        rooted!(&in(cx) let buffer = jsapi::NewArrayBuffer(cx.raw_cx(), 16));
        let h = buffer.handle();

        // Misaligned offset.
        assert!(jsapi::JS_NewUint16ArrayWithBuffer(cx.raw_cx(), h.into(), 3, 1).is_null());
        take_exception(cx, global.handle(), "RangeError");
        // Past the end, by offset and by length.
        assert!(jsapi::JS_NewUint8ArrayWithBuffer(cx.raw_cx(), h.into(), 17, -1).is_null());
        take_exception(cx, global.handle(), "RangeError");
        assert!(jsapi::JS_NewUint32ArrayWithBuffer(cx.raw_cx(), h.into(), 8, 3).is_null());
        take_exception(cx, global.handle(), "RangeError");
        // A rest-of-buffer view whose remaining bytes are not whole elements (16 bytes are; 12 are not).
        assert!(!jsapi::JS_NewFloat64ArrayWithBuffer(cx.raw_cx(), h.into(), 0, -1).is_null());
        rooted!(&in(cx) let odd = jsapi::NewArrayBuffer(cx.raw_cx(), 12));
        assert!(jsapi::JS_NewFloat64ArrayWithBuffer(cx.raw_cx(), odd.handle().into(), 0, -1).is_null());
        take_exception(cx, global.handle(), "RangeError");
        assert!(jsapi::JS_NewDataView(cx.raw_cx(), h.into(), 10, 7).is_null());
        take_exception(cx, global.handle(), "RangeError");

        // Not a buffer.
        rooted!(&in(cx) let plain = jsapi::JS_NewPlainObject(cx.raw_cx()));
        assert!(jsapi::JS_NewInt8ArrayWithBuffer(cx.raw_cx(), plain.handle().into(), 0, -1).is_null());
        take_exception(cx, global.handle(), "TypeError");
        assert!(!jsapi::JS_IsArrayBufferViewObject(plain.get()));
        assert!(!jsapi::IsArrayBufferObject(plain.get()));
        assert_eq!(jsapi::JS_GetArrayBufferViewType(plain.get()), Type::MaxTypedArrayViewType);
        assert_eq!(jsapi::JS_GetTypedArrayLength(plain.get()), 0);
        assert_eq!(jsapi::GetArrayBufferByteLength(plain.get()), 0);
        let mut shared = false;
        assert!(jsapi::GetArrayBufferData(plain.get(), &mut shared, ptr::null()).is_null());
    });
}

#[test]
fn views_of_a_shrunk_resizable_buffer() {
    with_global!(cx, global, {
        rooted!(&in(cx) let mut rval = UndefinedValue());
        eval(cx, global.handle(), "var rb = new ArrayBuffer(8, { maxByteLength: 16 }); rb", rval.handle_mut());
        rooted!(&in(cx) let rb = rval.to_object());
        assert!(jsapi::IsResizableArrayBufferMaybeShared(rb.get()));
        rooted!(&in(cx) let tracking = jsapi::JS_NewUint8ArrayWithBuffer(cx.raw_cx(), rb.handle().into(), 2, -1));
        rooted!(&in(cx) let fixed = jsapi::JS_NewUint8ArrayWithBuffer(cx.raw_cx(), rb.handle().into(), 4, 4));
        assert!(jsapi::IsResizableArrayBufferView(fixed.get()));
        assert_eq!(jsapi::JS_GetTypedArrayLength(tracking.get()), 6);
        assert_eq!(jsapi::JS_GetTypedArrayLength(fixed.get()), 4);

        eval(cx, global.handle(), "rb.resize(6)", rval.handle_mut());
        // The tracking view follows the buffer; the fixed one no longer fits and reads as empty.
        assert_eq!(jsapi::JS_GetTypedArrayLength(tracking.get()), 4);
        assert_eq!(jsapi::JS_GetTypedArrayLength(fixed.get()), 0);
        assert_eq!(jsapi::JS_GetTypedArrayByteOffset(fixed.get()), 0);
        let mut shared = false;
        assert!(jsapi::JS_GetUint8ArrayData(fixed.get(), &mut shared, ptr::null()).is_null());
        assert!(!jsapi::JS_GetUint8ArrayData(tracking.get(), &mut shared, ptr::null()).is_null());
    });
}

#[test]
fn detached_buffers() {
    with_global!(cx, global, {
        rooted!(&in(cx) let buffer = jsapi::NewArrayBuffer(cx.raw_cx(), 8));
        rooted!(&in(cx) let view = jsapi::JS_NewInt32ArrayWithBuffer(cx.raw_cx(), buffer.handle().into(), 0, 2));
        assert!(jsapi::ArrayBufferHasData(buffer.get()));
        assert!(!jsapi::IsDetachedArrayBufferObject(buffer.get()));

        assert!(jsapi::DetachArrayBuffer(cx.raw_cx(), buffer.handle().into()));
        assert!(jsapi::IsDetachedArrayBufferObject(buffer.get()));
        assert!(!jsapi::ArrayBufferHasData(buffer.get()));
        assert_eq!(jsapi::GetArrayBufferByteLength(buffer.get()), 0);
        let mut shared = false;
        assert!(jsapi::GetArrayBufferData(buffer.get(), &mut shared, ptr::null()).is_null());

        // The view is still an Int32Array, of nothing.
        assert_eq!(jsapi::JS_GetTypedArrayLength(view.get()), 0);
        assert_eq!(jsapi::JS_GetArrayBufferViewByteLength(view.get()), 0);
        let mut len = 9;
        let mut data = ptr::null_mut();
        assert_eq!(jsapi::JS_GetObjectAsInt32Array(view.get(), &mut len, &mut shared, &mut data), view.get());
        assert_eq!(len, 0);
        assert!(data.is_null());
        set_global(cx, global.handle(), c"v", view.get());
        check(cx, global.handle(), "v.length === 0 && v[0] === undefined");

        // Refusals: a view of a detached buffer, copying or stealing it, detaching a non-buffer.
        assert!(jsapi::JS_NewUint8ArrayWithBuffer(cx.raw_cx(), buffer.handle().into(), 0, -1).is_null());
        take_exception(cx, global.handle(), "TypeError");
        assert!(jsapi::CopyArrayBuffer(cx.raw_cx(), buffer.handle().into()).is_null());
        take_exception(cx, global.handle(), "TypeError");
        assert!(jsapi::StealArrayBufferContents(cx.raw_cx(), buffer.handle().into()).is_null());
        take_exception(cx, global.handle(), "TypeError");
        assert!(!jsapi::DetachArrayBuffer(cx.raw_cx(), view.handle().into()));
        take_exception(cx, global.handle(), "TypeError");

        // A buffer detached by a script reads the same.
        rooted!(&in(cx) let mut rval = UndefinedValue());
        eval(cx, global.handle(), "var t = new ArrayBuffer(4); t.transfer(); t", rval.handle_mut());
        assert!(jsapi::IsDetachedArrayBufferObject(rval.to_object()));
    });
}

#[test]
fn contents_in_and_out() {
    with_global!(cx, global, {
        let contents = libc::malloc(4).cast::<u8>();
        contents.copy_from_nonoverlapping([1u8, 2, 3, 4].as_ptr(), 4);
        rooted!(&in(cx) let buffer = jsapi::NewArrayBufferWithContents(cx.raw_cx(), 4, contents.cast()));
        assert!(!buffer.get().is_null());
        set_global(cx, global.handle(), c"b", buffer.get());
        check(cx, global.handle(), "new Uint8Array(b).join() === '1,2,3,4'");

        let stolen = jsapi::StealArrayBufferContents(cx.raw_cx(), buffer.handle().into()).cast::<u8>();
        assert!(!stolen.is_null());
        assert_eq!(std::slice::from_raw_parts(stolen, 4), &[1, 2, 3, 4]);
        libc::free(stolen.cast());
        assert!(jsapi::IsDetachedArrayBufferObject(buffer.get()));
        check(cx, global.handle(), "b.byteLength === 0");

        // Null contents make an empty buffer.
        rooted!(&in(cx) let empty = jsapi::NewArrayBufferWithContents(cx.raw_cx(), 0, ptr::null_mut()));
        assert_eq!(jsapi::GetArrayBufferByteLength(empty.get()), 0);
        assert!(!jsapi::IsDetachedArrayBufferObject(empty.get()));
    });
}

#[test]
fn copy_clone_and_copy_data() {
    with_global!(cx, global, {
        rooted!(&in(cx) let mut rval = UndefinedValue());
        eval(cx, global.handle(), "new Uint8Array([10, 20, 30, 40, 50]).buffer", rval.handle_mut());
        rooted!(&in(cx) let src = rval.to_object());

        rooted!(&in(cx) let copy = jsapi::CopyArrayBuffer(cx.raw_cx(), src.handle().into()));
        rooted!(&in(cx) let part = jsapi::ArrayBufferClone(cx.raw_cx(), src.handle().into(), 1, 3));
        let mut len = 0;
        let mut data = ptr::null_mut();
        assert!(!jsapi::GetObjectAsArrayBuffer(copy.get(), &mut len, &mut data).is_null());
        assert_eq!(std::slice::from_raw_parts(data, len), &[10, 20, 30, 40, 50]);
        assert!(!jsapi::GetObjectAsArrayBuffer(part.get(), &mut len, &mut data).is_null());
        assert_eq!(std::slice::from_raw_parts(data, len), &[20, 30, 40]);

        // Overlapping copy inside one buffer, then a range past the end.
        assert!(jsapi::ArrayBufferCopyData(cx.raw_cx(), copy.handle().into(), 1, copy.handle().into(), 0, 4));
        assert!(!jsapi::GetObjectAsArrayBuffer(copy.get(), &mut len, &mut data).is_null());
        assert_eq!(std::slice::from_raw_parts(data, len), &[10, 10, 20, 30, 40]);
        assert!(!jsapi::ArrayBufferCopyData(cx.raw_cx(), part.handle().into(), 1, src.handle().into(), 0, 3));
        take_exception(cx, global.handle(), "RangeError");
        assert!(jsapi::ArrayBufferClone(cx.raw_cx(), src.handle().into(), 4, 2).is_null());
        take_exception(cx, global.handle(), "RangeError");

        // The source is untouched.
        assert!(!jsapi::GetObjectAsArrayBuffer(src.get(), &mut len, &mut data).is_null());
        assert_eq!(std::slice::from_raw_parts(data, len), &[10, 20, 30, 40, 50]);
    });
}

#[test]
fn shared_memory_queries() {
    with_global!(cx, _global, {
        rooted!(&in(cx) let buffer = jsapi::NewArrayBuffer(cx.raw_cx(), 4));
        rooted!(&in(cx) let view = jsapi::JS_NewUint8ArrayWithBuffer(cx.raw_cx(), buffer.handle().into(), 0, -1));
        assert!(!jsapi::IsSharedArrayBufferObject(buffer.get()));
        assert!(jsapi::IsArrayBufferObjectMaybeShared(buffer.get()));
        assert!(!jsapi::JS_GetTypedArraySharedness(view.get()));
        assert!(!jsapi::IsArrayBufferViewShared(view.get()));
        assert!(!jsapi::ContainsSharedArrayBuffer(cx.raw_cx()));
        assert!(jsapi::UnwrapSharedArrayBuffer(buffer.get()).is_null());
        assert_eq!(jsapi::UnwrapArrayBufferMaybeShared(buffer.get()), buffer.get());
        assert_eq!(jsapi::UnwrapArrayBufferView(view.get()), view.get());
        assert!(jsapi::UnwrapArrayBufferView(buffer.get()).is_null());
        assert!(!jsapi::IsLargeArrayBufferMaybeShared(buffer.get()));
        assert!(!jsapi::IsResizableArrayBufferMaybeShared(buffer.get()));
        let mut defined = true;
        assert!(jsapi::HasDefinedArrayBufferDetachKey(cx.raw_cx(), buffer.handle().into(), &mut defined));
        assert!(!defined);
        assert!(jsapi::EnsureNonInlineArrayBufferOrView(cx.raw_cx(), view.get()));
    });
}

#[test]
fn number_conversions() {
    with_global!(cx, global, {
        rooted!(&in(cx) let mut v = UndefinedValue());
        let mut i32_out = 0;
        eval(cx, global.handle(), "'4294967297'", v.handle_mut());
        assert!(jsapi::ToInt32Slow(cx.raw_cx(), v.handle().into(), &mut i32_out));
        assert_eq!(i32_out, 1);
        let mut u8_out = 0;
        eval(cx, global.handle(), "-1.9", v.handle_mut());
        assert!(jsapi::ToUint8Slow(cx.raw_cx(), v.handle().into(), &mut u8_out));
        assert_eq!(u8_out, 255);
        let mut i16_out = 0;
        eval(cx, global.handle(), "({ valueOf() { return 98304; } })", v.handle_mut());
        assert!(jsapi::ToInt16Slow(cx.raw_cx(), v.handle().into(), &mut i16_out));
        assert_eq!(i16_out, -32768);
        let mut u32_out = 0;
        eval(cx, global.handle(), "({ valueOf() { throw new TypeError('no'); } })", v.handle_mut());
        assert!(!jsapi::ToUint32Slow(cx.raw_cx(), v.handle().into(), &mut u32_out));
        take_exception(cx, global.handle(), "TypeError");

        eval(cx, global.handle(), "-1n", v.handle_mut());
        assert_eq!(jsapi::ToBigInt64(v.to_bigint()), -1);
        assert_eq!(jsapi::ToBigUint64(v.to_bigint()), u64::MAX);
        eval(cx, global.handle(), "2n ** 64n + 5n", v.handle_mut());
        assert_eq!(jsapi::ToBigUint64(v.to_bigint()), 5);
        eval(cx, global.handle(), "2n ** 63n", v.handle_mut());
        assert_eq!(jsapi::ToBigInt64(v.to_bigint()), i64::MIN);

        let mut i = 0;
        assert!(jsapi::JS_DoubleIsInt32(-7.0, &mut i) && i == -7);
        assert!(!jsapi::JS_DoubleIsInt32(-0.0, &mut i));
        assert!(!jsapi::JS_DoubleIsInt32(0.5, &mut i));
        assert!(!jsapi::JS_DoubleIsInt32(2147483648.0, &mut i));
    });
}
