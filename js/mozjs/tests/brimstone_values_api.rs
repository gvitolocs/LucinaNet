/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Raw-API tests for the Brimstone engine's values, conversions and
// collections implementations (mozjs-sys/src/engine/values.rs).

use std::ptr;

use mozjs_sys::glue as glue_gen;
use mozjs::jsapi::{js, JS};
use mozjs::jsapi::{
    JS_ClearPendingException, JS_EncodeStringToUTF8, JS_GetProperty, JS_IdToProtoKey, JS_IndexToId,
    JS_IsExceptionPending, JS_IsIdentifier, JS_NewDependentString, JS_ParseJSON1, JS_Stringify,
    JS_StringToId, JS_TypeOfValue, JS_ValueToObject, JS_free, JS_malloc, JS_realloc, JSProtoKey,
    JSString, JSType, OnNewGlobalHookOption, JSObject, JS::ClippedTime, JS::HandleObject,
    JS::HandleString, JS::MutableHandleId, JS::RegExpFlag_Global, JS::RegExpFlag_IgnoreCase,
    JS::RegExpFlags, JS::SymbolCode, JS::TraceKind, JS::Value,
};
use mozjs::jsval::{BooleanValue, DoubleValue, Int32Value, ObjectValue, StringValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{JS_NewGlobalObject, JS_NewPlainObject, JS_NewStringCopyZ};
use mozjs::rust::{JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};

fn harness() {}

/// The string's text via its UTF-8 encoding (JS_EncodeStringToUTF8).
unsafe fn string_text(cx: *mut mozjs::context::RawJSContext, s: HandleString) -> String {
    let utf8 = JS_EncodeStringToUTF8(cx, s);
    assert!(utf8 != 0);
    let text = std::ffi::CStr::from_ptr(utf8 as *const std::os::raw::c_char).to_str().unwrap().to_string();
    JS_free(cx, utf8 as *mut std::ffi::c_void);
    text
}

#[test]
fn comparisons_and_typeof() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    let mut realm = unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        AutoRealm::new_from_handle(context, global.handle())
    };
    let cx = &mut realm;
    unsafe {
        rooted!(&in(cx) let one = Int32Value(1));
        rooted!(&in(cx) let one_b = Int32Value(1));
        rooted!(&in(cx) let two = Int32Value(2));
        rooted!(&in(cx) let nan = DoubleValue(f64::NAN));
        rooted!(&in(cx) let nan2 = DoubleValue(f64::NAN));
        rooted!(&in(cx) let s1 = JS_NewStringCopyZ(cx, c"1".as_ptr()));
        rooted!(&in(cx) let str_one = StringValue(&*s1.get()));
        let mut same = false;
        assert!(JS::SameValue(cx.raw_cx(), one.handle().into(), one_b.handle().into(), &mut same));
        assert!(same);
        assert!(JS::SameValue(cx.raw_cx(), nan.handle().into(), nan2.handle().into(), &mut same));
        assert!(same); // SameValue: NaN equals NaN
        let mut eq = false;
        assert!(JS::StrictlyEqual(cx.raw_cx(), one.handle().into(), str_one.handle().into(), &mut eq));
        assert!(!eq); // strict: number != string
        assert!(JS::LooselyEqual(cx.raw_cx(), one.handle().into(), str_one.handle().into(), &mut eq));
        assert!(eq); // loose: 1 == "1"
        assert!(JS::StrictlyEqual(cx.raw_cx(), one.handle().into(), two.handle().into(), &mut eq));
        assert!(!eq);
        assert_eq!(JS_TypeOfValue(cx.raw_cx(), one.handle().into()) as u32, JSType::JSTYPE_NUMBER as u32);
        assert_eq!(JS_TypeOfValue(cx.raw_cx(), str_one.handle().into()) as u32, JSType::JSTYPE_STRING as u32);
        rooted!(&in(cx) let undef = UndefinedValue());
        assert_eq!(JS_TypeOfValue(cx.raw_cx(), undef.handle().into()) as u32, JSType::JSTYPE_UNDEFINED as u32);
        // ToNumberSlow / ToBooleanSlow / ToInt64Slow / ToUint64Slow
        rooted!(&in(cx) let s = JS_NewStringCopyZ(cx, c"3.5".as_ptr()));
        rooted!(&in(cx) let sv = StringValue(&*s.get()));
        let mut d = 0.0;
        assert!(js::ToNumberSlow(cx.raw_cx(), sv.handle().into(), &mut d));
        assert_eq!(d, 3.5);
        rooted!(&in(cx) let zero = Int32Value(0));
        rooted!(&in(cx) let one_v = Int32Value(1));
        assert!(!js::ToBooleanSlow(zero.handle().into()));
        assert!(js::ToBooleanSlow(one_v.handle().into()));
        rooted!(&in(cx) let sn = JS_NewStringCopyZ(cx, c"-7".as_ptr()));
        rooted!(&in(cx) let snv = StringValue(&*sn.get()));
        let mut i = 0i64;
        assert!(js::ToInt64Slow(cx.raw_cx(), snv.handle().into(), &mut i));
        assert_eq!(i, -7);
        rooted!(&in(cx) let su = JS_NewStringCopyZ(cx, c"9".as_ptr()));
        rooted!(&in(cx) let suv = StringValue(&*su.get()));
        let mut u = 0u64;
        assert!(js::ToUint64Slow(cx.raw_cx(), suv.handle().into(), &mut u));
        assert_eq!(u, 9);
    }
}

#[test]
fn value_to_object_and_primitive() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    let mut realm = unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        AutoRealm::new_from_handle(context, global.handle())
    };
    let cx = &mut realm;
    unsafe {
        rooted!(&in(cx) let num = Int32Value(42));
        rooted!(&in(cx) let mut out_obj: *mut JSObject = ptr::null_mut());
        assert!(JS_ValueToObject(cx.raw_cx(), num.handle().into(), out_obj.handle_mut().into()));
        let obj = out_obj.get();
        assert!(!obj.is_null());
        rooted!(&in(cx) let obj_h = obj);
        // A Number wrapper has valueOf, so ToPrimitive returns the primitive.
        rooted!(&in(cx) let mut out = UndefinedValue());
        assert!(JS::ToPrimitive(cx.raw_cx(), obj_h.handle().into(), JSType::JSTYPE_NUMBER, out.handle_mut().into()));
        assert_eq!(out.get().to_int32(), 42);
        // OrdinaryToPrimitive on a plain object uses Object.prototype.toString.
        rooted!(&in(cx) let plain = JS_NewPlainObject(cx));
        assert!(JS::OrdinaryToPrimitive(cx.raw_cx(), plain.handle().into(), JSType::JSTYPE_STRING, out.handle_mut().into()));
        assert!(out.get().is_string());
        rooted!(&in(cx) let ts = out.get().to_string());
        assert_eq!(string_text(cx.raw_cx(), ts.handle().into()), "[object Object]");
    }
}

#[test]
fn json_parse_and_stringify() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    let mut realm = unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        AutoRealm::new_from_handle(context, global.handle())
    };
    let cx = &mut realm;
    unsafe {
        rooted!(&in(cx) let json = JS_NewStringCopyZ(cx, c"{\"a\":1}".as_ptr()));
        rooted!(&in(cx) let mut parsed = UndefinedValue());
        assert!(JS_ParseJSON1(cx.raw_cx(), json.handle().into(), parsed.handle_mut().into()));
        assert!(parsed.get().is_object());
        rooted!(&in(cx) let pobj = parsed.get().to_object() as *mut JSObject);
        rooted!(&in(cx) let mut rval = UndefinedValue());
        assert!(JS_GetProperty(cx.raw_cx(), pobj.handle().into(), c"a".as_ptr(), rval.handle_mut().into()));
        assert_eq!(rval.get().to_int32(), 1);
        // Stringify a number.
        rooted!(&in(cx) let mut v = Int32Value(42));
        let null_obj: HandleObject = JS::Handle { _phantom_0: Default::default(), ptr: ptr::null_mut() };
        rooted!(&in(cx) let space = UndefinedValue());
        assert!(JS_Stringify(cx.raw_cx(), v.handle_mut().into(), null_obj, space.handle().into(), None, ptr::null_mut()));
        rooted!(&in(cx) let sv = StringValue(&*v.get().to_string()));
        rooted!(&in(cx) let ts = v.get().to_string());
        assert_eq!(string_text(cx.raw_cx(), ts.handle().into()), "42");
    }
}

#[test]
fn bigint_conversions() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    let mut realm = unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        AutoRealm::new_from_handle(context, global.handle())
    };
    let cx = &mut realm;
    unsafe {
        let bi = JS::detail::BigIntFromInt64(cx.raw_cx(), -5);
        assert!(!bi.is_null());
        rooted!(&in(cx) let bih = bi);
        let s = JS::BigIntToString(cx.raw_cx(), bih.handle().into(), 10);
        assert!(!s.is_null());
        rooted!(&in(cx) let sh = s);
        assert_eq!(string_text(cx.raw_cx(), sh.handle().into()), "-5");
        let bi8 = JS::NumberToBigInt(cx.raw_cx(), 8.0);
        assert!(!bi8.is_null());
        // StringToBigInt on "0x10" (Latin1 range) -> 16
        let digits: Vec<u8> = b"0x10".to_vec();
        let range = mozjs::jsapi::mozilla::Range {
            _phantom_0: Default::default(),
            mStart: mozjs::jsapi::mozilla::RangedPtr { _phantom_0: Default::default(), mPtr: digits.as_ptr() as *mut u8 },
            mEnd: mozjs::jsapi::mozilla::RangedPtr { _phantom_0: Default::default(), mPtr: digits.as_ptr().add(digits.len()) as *mut u8 },
        };
        let bi16 = JS::StringToBigInt(cx.raw_cx(), &range);
        assert!(!bi16.is_null());
        rooted!(&in(cx) let b16h = bi16);
        let s16 = JS::BigIntToString(cx.raw_cx(), b16h.handle().into(), 10);
        rooted!(&in(cx) let s16h = s16);
        assert_eq!(string_text(cx.raw_cx(), s16h.handle().into()), "16");
        // ToBigInt on a boolean
        rooted!(&in(cx) let t = BooleanValue(true));
        let bi1 = JS::ToBigInt(cx.raw_cx(), t.handle().into());
        assert!(!bi1.is_null());
    }
}

#[test]
fn symbols() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    let mut realm = unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        AutoRealm::new_from_handle(context, global.handle())
    };
    let cx = &mut realm;
    unsafe {
        let iter = JS::GetWellKnownSymbol(cx.raw_cx(), SymbolCode::iterator);
        assert!(!iter.is_null());
        let key = JS::GetWellKnownSymbolKey(cx.raw_cx(), SymbolCode::iterator);
        assert!(key.is_symbol());
        rooted!(&in(cx) let desc = JS_NewStringCopyZ(cx, c"mysym".as_ptr()));
        let sym = JS::NewSymbol(cx.raw_cx(), desc.handle().into());
        assert!(!sym.is_null());
        rooted!(&in(cx) let symh = sym);
        let d = JS::GetSymbolDescription(symh.handle().into());
        assert!(!d.is_null());
        rooted!(&in(cx) let dh2 = d);
        assert_eq!(string_text(cx.raw_cx(), dh2.handle().into()), "mysym");
        let reg = JS::GetSymbolFor(cx.raw_cx(), desc.handle().into());
        assert!(!reg.is_null());
    }
}

#[test]
fn date_objects() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    let mut realm = unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        AutoRealm::new_from_handle(context, global.handle())
    };
    let cx = &mut realm;
    unsafe {
        let d = JS::NewDateObject(cx.raw_cx(), ClippedTime { t: 1000.0 });
        assert!(!d.is_null());
        rooted!(&in(cx) let dh = d);
        let mut is_date = false;
        assert!(JS::ObjectIsDate(cx.raw_cx(), dh.handle().into(), &mut is_date));
        assert!(is_date);
        rooted!(&in(cx) let obj = d);
        let mut msec = 0.0;
        assert!(js::DateGetMsecSinceEpoch(cx.raw_cx(), obj.handle().into(), &mut msec));
        assert_eq!(msec, 1000.0);
        let d2 = JS::NewDateObject1(cx.raw_cx(), 2020, 0, 1, 0, 0, 0);
        assert!(!d2.is_null());
    }
}

#[test]
fn regexp_objects() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    let mut realm = unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        AutoRealm::new_from_handle(context, global.handle())
    };
    let cx = &mut realm;
    unsafe {
        let flags = JS::RegExpFlags { flags_: RegExpFlag_Global | RegExpFlag_IgnoreCase };
        let re = JS::NewRegExpObject(cx.raw_cx(), c"a+".as_ptr(), 2, flags);
        assert!(!re.is_null());
        rooted!(&in(cx) let reh = re);
        let mut is_re = false;
        assert!(JS::ObjectIsRegExp(cx.raw_cx(), reh.handle().into(), &mut is_re));
        assert!(is_re);
        let src = JS::GetRegExpSource(cx.raw_cx(), reh.handle().into());
        assert!(!src.is_null());
        rooted!(&in(cx) let srch = src);
        assert_eq!(string_text(cx.raw_cx(), srch.handle().into()), "a+");
        rooted!(&in(cx) let obj = re);
        let mut got = JS::RegExpFlags { flags_: 0 };
        glue_gen::JS_GetRegExpFlags(cx.raw_cx(), obj.handle().into(), &mut got);
        assert_eq!(got.flags_, flags.flags_);
        // Syntax check: "(" is invalid.
        let bad: Vec<u16> = "(".encode_utf16().collect();
        rooted!(&in(cx) let mut err = UndefinedValue());
        assert!(!JS::CheckRegExpSyntax(cx.raw_cx(), bad.as_ptr(), bad.len(), JS::RegExpFlags { flags_: 0 }, err.handle_mut().into()));
        JS_ClearPendingException(cx.raw_cx());
        // Execute against "xaaay": global match at index 1.
        let input: Vec<u16> = "xaaay".encode_utf16().collect();
        rooted!(&in(cx) let mut rval = UndefinedValue());
        let mut index = 0usize;
        assert!(JS::ExecuteRegExp(cx.raw_cx(), reh.handle().into(), reh.handle().into(), input.as_ptr(), input.len(), &mut index, true, rval.handle_mut().into()));
        assert_eq!(index, 1);
    }
}

#[test]
fn map_set_weakmap() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    let mut realm = unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        AutoRealm::new_from_handle(context, global.handle())
    };
    let cx = &mut realm;
    unsafe {
        let map = JS::NewMapObject(cx.raw_cx());
        assert!(!map.is_null());
        rooted!(&in(cx) let mh = map);
        let mut is_map = false;
        assert!(JS::IsMapObject(cx.raw_cx(), mh.handle().into(), &mut is_map));
        assert!(is_map);
        rooted!(&in(cx) let key = Int32Value(1));
        rooted!(&in(cx) let vs = JS_NewStringCopyZ(cx, c"one".as_ptr()));
        rooted!(&in(cx) let val = StringValue(&*vs.get()));
        assert!(JS::MapSet(cx.raw_cx(), mh.handle().into(), key.handle().into(), val.handle().into()));
        assert_eq!(JS::MapSize(cx.raw_cx(), mh.handle().into()), 1);
        let mut has = false;
        assert!(JS::MapHas(cx.raw_cx(), mh.handle().into(), key.handle().into(), &mut has));
        assert!(has);
        rooted!(&in(cx) let mut got = UndefinedValue());
        assert!(JS::MapGet(cx.raw_cx(), mh.handle().into(), key.handle().into(), got.handle_mut().into()));
        assert!(got.get().is_string());
        // Interleaved keys/values vector.
        // The generated binding types the out-vector as `u8`, which cannot carry an address: the engine refuses with a
        // pending Error rather than report success with nothing written.
        assert!(!js::GetMapObjectKeysAndValuesInterleaved(cx.raw_cx(), mh.handle().into(), 0));
        assert!(mozjs::jsapi::JS_IsExceptionPending(cx.raw_cx()));
        mozjs::jsapi::JS_ClearPendingException(cx.raw_cx());

        let set = JS::NewSetObject(cx.raw_cx());
        assert!(!set.is_null());
        rooted!(&in(cx) let sh = set);
        let mut is_set = false;
        assert!(JS::IsSetObject(cx.raw_cx(), sh.handle().into(), &mut is_set));
        assert!(is_set);
        assert!(JS::SetAdd(cx.raw_cx(), sh.handle().into(), key.handle().into()));
        assert_eq!(JS::SetSize(cx.raw_cx(), sh.handle().into()), 1);
        let mut has2 = false;
        assert!(JS::SetHas(cx.raw_cx(), sh.handle().into(), key.handle().into(), &mut has2));
        assert!(has2);
        assert!(!js::GetSetObjectKeys(cx.raw_cx(), sh.handle().into(), 0));
        assert!(mozjs::jsapi::JS_IsExceptionPending(cx.raw_cx()));
        mozjs::jsapi::JS_ClearPendingException(cx.raw_cx());

        let wm = JS::NewWeakMapObject(cx.raw_cx());
        assert!(!wm.is_null());
        rooted!(&in(cx) let wh = wm);
        assert!(JS::IsWeakMapObject(wm));
        rooted!(&in(cx) let wkey = JS_NewPlainObject(cx));
        rooted!(&in(cx) let wkeyv = ObjectValue(wkey.get()));
        assert!(JS::SetWeakMapEntry(cx.raw_cx(), wh.handle().into(), wkeyv.handle().into(), val.handle().into()));
        rooted!(&in(cx) let mut wgot = UndefinedValue());
        assert!(JS::GetWeakMapEntry(cx.raw_cx(), wh.handle().into(), wkeyv.handle().into(), wgot.handle_mut().into()));
        assert!(wgot.get().is_string());
    }
}

#[test]
fn strings_and_ids() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    let mut realm = unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        AutoRealm::new_from_handle(context, global.handle())
    };
    let cx = &mut realm;
    unsafe {
        rooted!(&in(cx) let s = JS_NewStringCopyZ(cx, c"hello".as_ptr()));
        assert_eq!(string_text(cx.raw_cx(), s.handle().into()), "hello");
        let dep = JS_NewDependentString(cx.raw_cx(), s.handle().into(), 1, 3);
        assert!(!dep.is_null());
        rooted!(&in(cx) let deph = dep);
        assert_eq!(string_text(cx.raw_cx(), deph.handle().into()), "ell");
        let mut is_id = false;
        assert!(JS_IsIdentifier(cx.raw_cx(), s.handle().into(), &mut is_id));
        assert!(is_id);
        rooted!(&in(cx) let bad = JS_NewStringCopyZ(cx, c"1a".as_ptr()));
        assert!(JS_IsIdentifier(cx.raw_cx(), bad.handle().into(), &mut is_id));
        assert!(!is_id);
        // Index ids and array-index strings.
        let mut id: mozjs::jsapi::jsid = mozjs::jsapi::jsid { asBits_: 0 };
        let hid = MutableHandleId { _phantom_0: Default::default(), ptr: &mut id };
        assert!(JS_IndexToId(cx.raw_cx(), 17, hid));
        let idx: Vec<u16> = "17".encode_utf16().collect();
        let mut index = 0u32;
        assert!(js::StringIsArrayIndex1(idx.as_ptr(), idx.len() as u32, &mut index));
        assert_eq!(index, 17);
        // IdToProtoKey: the "Array" string id maps to JSProto_Array.
        rooted!(&in(cx) let arr = JS_NewStringCopyZ(cx, c"Array".as_ptr()));
        let mut arr_id: mozjs::jsapi::jsid = mozjs::jsapi::jsid { asBits_: 0 };
        let arr_hid = MutableHandleId { _phantom_0: Default::default(), ptr: &mut arr_id };
        assert!(JS_StringToId(cx.raw_cx(), arr.handle().into(), arr_hid));
        let key = JS_IdToProtoKey(cx.raw_cx(), JS::HandleId { _phantom_0: Default::default(), ptr: &arr_id });
        assert_eq!(key, JSProtoKey::JSProto_Array);
        // Constant values.
        let mut v = JS::Value { asBits_: 0 };
        glue_gen::JS_GetNaNValue(cx.raw_cx(), &mut v);
        assert!(v.to_double().is_nan());
        glue_gen::JS_GetPositiveInfinityValue(cx.raw_cx(), &mut v);
        assert_eq!(v.to_double(), f64::INFINITY);
    }
}

#[test]
fn roots_and_memory() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    let mut realm = unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        AutoRealm::new_from_handle(context, global.handle())
    };
    let cx = &mut realm;
    unsafe {
        rooted!(&in(cx) let mut v = Int32Value(7));
        let mh: JS::MutableHandleValue = v.handle_mut().into();
        assert!(js::AddRawValueRoot(cx.raw_cx(), mh.ptr, c"test".as_ptr()));
        js::RemoveRawValueRoot(cx.raw_cx(), mh.ptr);
        // Associated memory is accounting only; must not crash.
        rooted!(&in(cx) let obj = JS_NewPlainObject(cx));
        // Trace kind names.
        let name = JS::GCTraceKindToAscii(TraceKind::Object);
        assert!(
            !name.is_null(),
            "`JS__GCTraceKindToAscii` answered null: CStr::from_ptr(null) is undefined behaviour in a test"
        );
        assert_eq!(std::ffi::CStr::from_ptr(name).to_str().unwrap(), "Object");
        // malloc/realloc/free.
        let p = JS_malloc(cx.raw_cx(), 16);
        assert!(!p.is_null());
        let p2 = JS_realloc(cx.raw_cx(), p, 16, 32);
        assert!(!p2.is_null());
        JS_free(cx.raw_cx(), p2);
        // Rooted object vector.
        let vec = glue_gen::CreateRootedObjectVector(cx.raw_cx());
        assert!(!vec.is_null());
        assert!(glue_gen::AppendToRootedObjectVector(vec, obj.get()));
        let addr = glue_gen::GetObjectVectorAddress(vec);
        assert!(!addr.is_null());
        let v = std::slice::from_raw_parts(addr as *const *mut JSObject, 1);
        assert_eq!(v[0], obj.get());
        glue_gen::DeleteRootedObjectVector(vec);
    }
}
