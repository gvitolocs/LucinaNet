/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Raw-API tests for the Brimstone engine's property and function
// implementations (mozjs-sys/src/engine/property.rs and function.rs).

use std::ptr;

use mozjs::jsapi::{
    jsid, JSFunctionSpec, JSNativeWrapper, JSITER_HIDDEN, JSITER_OWNONLY, JSITER_SYMBOLS,
    JSPROP_ENUMERATE, JSPROP_PERMANENT, JSPROP_READONLY, JSJitInfo, JSObject, JSString,
    OnNewGlobalHookOption, JS_AlreadyHasOwnProperty, JS_CallFunctionName, JS_CallFunctionValue,
    JS_DefineElement, JS_DefineFunction, JS_DefineFunctions, JS_DefineProperty, JS_DefineProperty1,
    JS_DefinePropertyById2, JS_DefineUCProperty, JS_DeleteElement, JS_DeleteProperty,
    JS_FreezeObject, JS_GetElement, JS_GetFunctionId, JS_GetFunctionLength,
    JS_GetOwnPropertyDescriptor, JS_GetProperty, JS_GetPropertyById, JS_GetPropertyDescriptor,
    JS_GetPrototype, JS_GetUCProperty, JS_HasOwnProperty, JS_HasProperty, JS_HasPropertyById,
    JS_IdToValue, JS_IsExceptionPending, JS_IsExtensible, JS_NewFunction, JS_StringEqualsAscii, JS_ObjectIsFunction, JS_PreventExtensions,
    JS_SetElement, JS_SetProperty, JS_SetPropertyById, JS_SetPrototype, JS_SetUCProperty,
    JS_StringToId, JS_ValueToId, JS::HandleValueArray, JS::Value,
};
use mozjs::jsval::{Int32Value, ObjectValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{
    GetPropertyKeys, JS_NewGlobalObject, JS_NewPlainObject,
};
use mozjs::rust::{
    evaluate_script, CompileOptionsWrapper, IdVector, JSEngine, RealmOptions, Runtime,
    SIMPLE_GLOBAL_CLASS,
};

unsafe extern "C" fn answer_native(
    _cx: *mut mozjs::context::RawJSContext,
    _argc: u32,
    vp: *mut Value,
) -> bool {
    *vp = Int32Value(42).into();
    true
}

unsafe extern "C" fn add_native(
    _cx: *mut mozjs::context::RawJSContext,
    argc: u32,
    vp: *mut Value,
) -> bool {
    let args = mozjs::jsapi::CallArgs::from_vp(vp, argc);
    let a = args.get(0).to_int32();
    let b = if argc > 1 { args.get(1).to_int32() } else { 0 };
    args.rval().set(Int32Value(a + b).into());
    true
}

#[test]
fn define_and_get_property_by_name() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        rooted!(&in(context) let v = Int32Value(7));
        assert!(JS_DefineProperty(
            context.raw_cx(),
            obj.handle().into(),
            c"x".as_ptr(),
            v.handle().into(),
            JSPROP_ENUMERATE as u32,
        ));
        let mut found = false;
        assert!(JS_HasProperty(context.raw_cx(), obj.handle().into(), c"x".as_ptr(), &mut found));
        assert!(found);
        rooted!(&in(context) let mut rval = UndefinedValue());
        assert!(JS_GetProperty(context.raw_cx(), obj.handle().into(), c"x".as_ptr(), rval.handle_mut().into()));
        assert_eq!(rval.get().to_int32(), 7);
        rooted!(&in(context) let nv = Int32Value(9));
        assert!(JS_SetProperty(
            context.raw_cx(),
            obj.handle().into(),
            c"x".as_ptr(),
            nv.handle().into()
        ));
        assert!(JS_GetProperty(context.raw_cx(), obj.handle().into(), c"x".as_ptr(), rval.handle_mut().into()));
        assert_eq!(rval.get().to_int32(), 9);
    }
}

#[test]
fn define_and_get_property_by_id() {
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
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        rooted!(&in(context) let mut rval = UndefinedValue());
        let options = CompileOptionsWrapper::new(&context, c"t".to_owned(), 1);
        assert!(evaluate_script(
            context,
            global.handle(),
            "'myprop'",
    rval.handle_mut(),
            options
        ).is_ok());
        rooted!(&in(context) let s = rval.get().to_string());
        rooted!(&in(context) let mut id: jsid = std::mem::zeroed());
        assert!(JS_StringToId(context.raw_cx(), s.handle().into(), id.handle_mut().into()));
        rooted!(&in(context) let v = Int32Value(11));
        assert!(JS_DefinePropertyById2(
            context.raw_cx(),
            obj.handle().into(),
            id.handle().into(),
            v.handle().into(),
            JSPROP_ENUMERATE as u32,
        ));
        let mut found = false;
        assert!(JS_HasPropertyById(context.raw_cx(), obj.handle().into(), id.handle().into(), &mut found));
        assert!(found);
        rooted!(&in(context) let mut out = UndefinedValue());
        assert!(JS_GetPropertyById(context.raw_cx(), obj.handle().into(), id.handle().into(), out.handle_mut().into()));
        assert_eq!(out.get().to_int32(), 11);
        rooted!(&in(context) let nv = Int32Value(12));
        assert!(JS_SetPropertyById(
            context.raw_cx(),
            obj.handle().into(),
            id.handle().into(),
            nv.handle().into()
        ));
        assert!(JS_GetPropertyById(context.raw_cx(), obj.handle().into(), id.handle().into(), out.handle_mut().into()));
        assert_eq!(out.get().to_int32(), 12);
    }
}

#[test]
fn element_define_get_set_delete() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        rooted!(&in(context) let v = Int32Value(100));
        assert!(JS_DefineElement(
            context.raw_cx(),
            obj.handle().into(),
            3,
            v.handle().into(),
            JSPROP_ENUMERATE as u32,
        ));
        rooted!(&in(context) let mut rval = UndefinedValue());
        assert!(JS_GetElement(context.raw_cx(), obj.handle().into(), 3, rval.handle_mut().into()));
        assert_eq!(rval.get().to_int32(), 100);
        rooted!(&in(context) let nv = Int32Value(101));
        assert!(JS_SetElement(context.raw_cx(), obj.handle().into(), 3, nv.handle().into()));
        assert!(JS_GetElement(context.raw_cx(), obj.handle().into(), 3, rval.handle_mut().into()));
        assert_eq!(rval.get().to_int32(), 101);
        let mut op = mozjs::jsapi::JS::ObjectOpResult::default();
        assert!(JS_DeleteElement(context.raw_cx(), obj.handle().into(), 3, &mut op));
        let mut found = false;
        assert!(JS_HasProperty(context.raw_cx(), obj.handle().into(), c"3".as_ptr(), &mut found));
        assert!(!found);
    }
}

#[test]
fn uc_property_roundtrip() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        let name: [u16; 4] = ['u' as u16, 'c' as u16, 'p' as u16, 0];
        rooted!(&in(context) let v = Int32Value(5));
        let mut desc = mozjs::jsapi::JS::PropertyDescriptor::default();
        desc.set_hasValue_(true);
        desc.value_ = v.handle().get();
        desc.set_hasEnumerable_(true);
        desc.set_enumerable_(true);
        desc.set_hasWritable_(true);
        desc.set_writable_(true);
        desc.set_hasConfigurable_(true);
        desc.set_configurable_(true);
        let mut op = mozjs::jsapi::JS::ObjectOpResult::default();
        assert!(JS_DefineUCProperty(
            context.raw_cx(),
            obj.handle().into(),
            name.as_ptr(),
            name.len() - 1,
            mozjs::jsapi::JS::Handle::from_marked_location(&desc),
            &mut op,
        ));
        rooted!(&in(context) let mut rval = UndefinedValue());
        assert!(JS_GetUCProperty(
            context.raw_cx(),
            obj.handle().into(),
            name.as_ptr(),
            name.len() - 1,
            rval.handle_mut().into()
        ));
        assert_eq!(rval.get().to_int32(), 5);
        rooted!(&in(context) let nv = Int32Value(6));
        assert!(JS_SetUCProperty(
            context.raw_cx(),
            obj.handle().into(),
            name.as_ptr(),
            name.len() - 1,
            nv.handle().into()
        ));
        assert!(JS_GetUCProperty(
            context.raw_cx(),
            obj.handle().into(),
            name.as_ptr(),
            name.len() - 1,
            rval.handle_mut().into()
        ));
        assert_eq!(rval.get().to_int32(), 6);
    }
}

#[test]
fn has_own_and_already_has_own() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        rooted!(&in(context) let v = Int32Value(1));
        assert!(JS_DefineProperty(
            context.raw_cx(),
            obj.handle().into(),
            c"own".as_ptr(),
            v.handle().into(),
            0,
        ));
        let mut found = false;
        assert!(JS_HasOwnProperty(context.raw_cx(), obj.handle().into(), c"own".as_ptr(), &mut found));
        assert!(found);
        assert!(JS_AlreadyHasOwnProperty(
            context.raw_cx(),
            obj.handle().into(),
            c"own".as_ptr(),
            &mut found
        ));
        assert!(found);
        assert!(JS_AlreadyHasOwnProperty(
            context.raw_cx(),
            obj.handle().into(),
            c"missing".as_ptr(),
            &mut found
        ));
        assert!(!found);
    }
}

#[test]
fn delete_property_reports_op_result() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        rooted!(&in(context) let v = Int32Value(1));
        assert!(JS_DefineProperty(
            context.raw_cx(),
            obj.handle().into(),
            c"d".as_ptr(),
            v.handle().into(),
            JSPROP_PERMANENT as u32,
        ));
        let mut op = mozjs::jsapi::JS::ObjectOpResult::default();
        // Permanent property: delete reports failure but the call succeeds.
        assert!(JS_DeleteProperty(context.raw_cx(), obj.handle().into(), c"d".as_ptr(), &mut op));
        assert!(op.code_ != 0);
        let mut found = false;
        assert!(JS_HasProperty(context.raw_cx(), obj.handle().into(), c"d".as_ptr(), &mut found));
        assert!(found);
    }
}

#[test]
fn own_property_descriptor_flags_and_value() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        rooted!(&in(context) let v = Int32Value(32));
        let attrs = (JSPROP_ENUMERATE | JSPROP_PERMANENT | JSPROP_READONLY) as u32;
        assert!(JS_DefineProperty(
            context.raw_cx(),
            obj.handle().into(),
            c"p".as_ptr(),
            v.handle().into(),
            attrs,
        ));
        rooted!(&in(context) let mut desc: mozjs::jsapi::JS::PropertyDescriptor);
        let mut is_none = true;
        assert!(JS_GetOwnPropertyDescriptor(
            context.raw_cx(),
            obj.handle().into(),
            c"p".as_ptr(),
            desc.handle_mut().into(),
            &mut is_none,
        ));
        assert!(!is_none);
        let d = desc.get();
        assert!(d.enumerable_());
        assert!(!d.configurable_());
        assert!(!d.writable_());
        assert_eq!(d.value_.to_int32(), 32);
    }
}

#[test]
fn get_property_descriptor_with_holder() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        rooted!(&in(context) let v = Int32Value(8));
        assert!(JS_DefineProperty(
            context.raw_cx(),
            obj.handle().into(),
            c"q".as_ptr(),
            v.handle().into(),
            JSPROP_ENUMERATE as u32,
        ));
        rooted!(&in(context) let mut desc: mozjs::jsapi::JS::PropertyDescriptor);
        rooted!(&in(context) let mut holder: *mut JSObject = ptr::null_mut());
        let mut is_none = true;
        assert!(JS_GetPropertyDescriptor(
            context.raw_cx(),
            obj.handle().into(),
            c"q".as_ptr(),
            desc.handle_mut().into(),
            holder.handle_mut().into(),
            &mut is_none,
        ));
        assert!(!is_none);
        assert_eq!(desc.get().value_.to_int32(), 8);
    }
}

#[test]
fn get_property_keys_with_flags() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        rooted!(&in(context) let a = Int32Value(1));
        assert!(JS_DefineProperty(context.raw_cx(), obj.handle().into(), c"k".as_ptr(), a.handle().into(), 0));
        let mut ids = IdVector::new(context);
        assert!(GetPropertyKeys(
            context,
            obj.handle().into(),
            JSITER_OWNONLY | JSITER_HIDDEN | JSITER_SYMBOLS,
            ids.handle_mut(),
        ));
        assert_eq!(ids.len(), 1);
        rooted!(&in(context) let id = ids[0]);
        assert!(id.is_string());
    }
}

#[test]
fn high_index_id_roundtrips_as_string() {
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
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        // Index >= 2^31 must come back out of the engine as a string id.
        rooted!(&in(context) let v = Int32Value(77));
        assert!(JS_DefineElement(
            context.raw_cx(),
            obj.handle().into(),
            3_000_000_000,
            v.handle().into(),
            JSPROP_ENUMERATE as u32,
        ));
        let mut ids = IdVector::new(context);
        assert!(GetPropertyKeys(context, obj.handle().into(), JSITER_OWNONLY, ids.handle_mut()));
        assert_eq!(ids.len(), 1);
        rooted!(&in(context) let id = ids[0]);
        assert!(id.is_string());
        rooted!(&in(context) let mut idval = UndefinedValue());
        assert!(JS_IdToValue(context.raw_cx(), id.get(), idval.handle_mut().into()));
        assert!(idval.get().is_string());
        rooted!(&in(context) let s = idval.get().to_string());
        let mut eq = false;
        assert!(JS_StringEqualsAscii(context.raw_cx(), s.get(), c"3000000000".as_ptr(), &mut eq));
        assert!(eq);

        // A string value that is a canonical high index converts to an index id.
        rooted!(&in(context) let mut rval = UndefinedValue());
        let options = CompileOptionsWrapper::new(&context, c"t".to_owned(), 1);
        assert!(evaluate_script(
            context,
            global.handle(),
            "'4294967295'",
    rval.handle_mut(),
            options
        ).is_ok());
        rooted!(&in(context) let mut id2: jsid = std::mem::zeroed());
        assert!(JS_ValueToId(context.raw_cx(), rval.handle().into(), id2.handle_mut().into()));
        rooted!(&in(context) let mut back = UndefinedValue());
        assert!(JS_IdToValue(context.raw_cx(), id2.get(), back.handle_mut().into()));
        assert!(back.get().is_string());
    }
}

#[test]
fn getter_native_is_called() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        assert!(JS_DefineProperty1(
            context.raw_cx(),
            obj.handle().into(),
            c"answer".as_ptr(),
            Some(answer_native),
            None,
            JSPROP_ENUMERATE as u32,
        ));
        rooted!(&in(context) let mut rval = UndefinedValue());
        assert!(JS_GetProperty(
            context.raw_cx(),
            obj.handle().into(),
            c"answer".as_ptr(),
            rval.handle_mut().into()
        ));
        assert_eq!(rval.get().to_int32(), 42);
    }
}

#[test]
fn prototype_get_and_set() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        rooted!(&in(context) let proto = JS_NewPlainObject(context));
        assert!(JS_SetPrototype(context.raw_cx(), obj.handle().into(), proto.handle().into()));
        rooted!(&in(context) let mut out: *mut JSObject = ptr::null_mut());
        assert!(JS_GetPrototype(context.raw_cx(), obj.handle().into(), out.handle_mut().into()));
        assert_eq!(out.get(), proto.get());
    }
}

#[test]
fn extensibility_and_freeze() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        let mut extensible = false;
        assert!(JS_IsExtensible(context.raw_cx(), obj.handle().into(), &mut extensible));
        assert!(extensible);
        let mut op = mozjs::jsapi::JS::ObjectOpResult::default();
        assert!(JS_PreventExtensions(context.raw_cx(), obj.handle().into(), &mut op));
        assert!(JS_IsExtensible(context.raw_cx(), obj.handle().into(), &mut extensible));
        assert!(!extensible);
        assert!(JS_FreezeObject(context.raw_cx(), obj.handle().into()));
    }
}

#[test]
fn new_function_and_call_function_value() {
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
        rooted!(&in(context) let fun = JS_NewFunction(context.raw_cx(), Some(add_native), 2, 0, c"add".as_ptr()));
        assert!(!fun.get().is_null());
        assert!(JS_ObjectIsFunction(fun.get() as *mut JSObject));
        rooted!(&in(context) let fv = ObjectValue(fun.get() as *mut JSObject));
        rooted!(&in(context) let a = Int32Value(20));
        rooted!(&in(context) let b = Int32Value(22));
        let args = [a.handle().get(), b.handle().get()];
        let argv = HandleValueArray { length_: 2, elements_: args.as_ptr() };
        rooted!(&in(context) let mut rval = UndefinedValue());
        assert!(JS_CallFunctionValue(context.raw_cx(), global.handle().into(), fv.handle().into(), &argv, rval.handle_mut().into()));
        assert_eq!(rval.get().to_int32(), 42);
    }
}

#[test]
fn define_function_and_call_by_name() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        let fun = JS_DefineFunction(context.raw_cx(), obj.handle().into(), c"add".as_ptr(), Some(add_native), 2, 0);
        assert!(!fun.is_null());
        rooted!(&in(context) let a = Int32Value(1));
        rooted!(&in(context) let b = Int32Value(2));
        let args = [a.handle().get(), b.handle().get()];
        let argv = HandleValueArray { length_: 2, elements_: args.as_ptr() };
        rooted!(&in(context) let mut rval = UndefinedValue());
        assert!(JS_CallFunctionName(context.raw_cx(), obj.handle().into(), c"add".as_ptr(), &argv, rval.handle_mut().into()));
        assert_eq!(rval.get().to_int32(), 3);
    }
}

#[test]
fn define_functions_with_jit_info_spec() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        // A spec whose call.info is non-null is a JIT-info spec; the engine
        // must create a plain native that calls call.op.
        let info = Box::leak(Box::new(std::mem::zeroed::<JSJitInfo>()));
        let specs = [
            JSFunctionSpec {
                name: mozjs::jsapi::JSPropertySpec_Name { string_: c"jitfn".as_ptr() },
                call: JSNativeWrapper { op: Some(answer_native), info: info as *const JSJitInfo },
                nargs: 0,
                flags: 0,
                selfHostedName: ptr::null(),
            },
            JSFunctionSpec {
                name: mozjs::jsapi::JSPropertySpec_Name { string_: ptr::null() },
                call: JSNativeWrapper { op: None, info: ptr::null() },
                nargs: 0,
                flags: 0,
                selfHostedName: ptr::null(),
            },
        ];
        assert!(JS_DefineFunctions(context.raw_cx(), obj.handle().into(), specs.as_ptr()));
        let argv = HandleValueArray { length_: 0, elements_: ptr::null() };
        rooted!(&in(context) let mut rval = UndefinedValue());
        assert!(JS_CallFunctionName(context.raw_cx(), obj.handle().into(), c"jitfn".as_ptr(), &argv, rval.handle_mut().into()));
        assert_eq!(rval.get().to_int32(), 42);
    }
}

#[test]
fn function_inspection() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let fun = JS_NewFunction(context.raw_cx(), Some(add_native), 2, 0, c"myfn".as_ptr()));
        let mut len: u16 = 0;
        assert!(JS_GetFunctionLength(context.raw_cx(), fun.handle().into(), &mut len));
        assert_eq!(len, 2);
        rooted!(&in(context) let mut name: *mut JSString = ptr::null_mut());
        assert!(JS_GetFunctionId(context.raw_cx(), fun.handle().into(), name.handle_mut().into()));
        assert!(!name.get().is_null());
    }
}

#[test]
fn array_object_length() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let arr = mozjs::jsapi::NewArrayObject(
            context.raw_cx(),
            &HandleValueArray { length_: 0, elements_: ptr::null() },
        ));
        let mut len: u32 = 99;
        assert!(mozjs::jsapi::GetArrayLength(context.raw_cx(), arr.handle().into(), &mut len));
        assert_eq!(len, 0);
        assert!(mozjs::jsapi::SetArrayLength(context.raw_cx(), arr.handle().into(), 5));
        assert!(mozjs::jsapi::GetArrayLength(context.raw_cx(), arr.handle().into(), &mut len));
        assert_eq!(len, 5);
    }
}

#[test]
fn strict_assignment_to_nonwritable_fails() {
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
        rooted!(&in(context) let v = Int32Value(1));
        assert!(JS_DefineProperty(
            context.raw_cx(),
            global.handle().into(),
            c"ro".as_ptr(),
            v.handle().into(),
            JSPROP_READONLY as u32,
        ));
        rooted!(&in(context) let mut rval = UndefinedValue());
        let options = CompileOptionsWrapper::new(&context, c"strict".to_owned(), 1);
        let res = evaluate_script(
            context,
            global.handle(),
            "'use strict'; ro = 2;",
            rval.handle_mut(),
            options,
        );
        assert!(res.is_err());
    }
}


/// Count how many of `ids` are the string `name`.
unsafe fn count_key(context: &mut mozjs::context::JSContext, ids: &IdVector, name: &std::ffi::CStr) -> usize {
    let mut n = 0;
    for i in 0..ids.len() {
        rooted!(&in(context) let id = ids[i]);
        rooted!(&in(context) let mut v = UndefinedValue());
        assert!(JS_IdToValue(context.raw_cx(), id.get(), v.handle_mut().into()));
        if v.get().is_string() {
            rooted!(&in(context) let st = v.get().to_string());
            let mut eq = false;
            assert!(JS_StringEqualsAscii(context.raw_cx(), st.get(), name.as_ptr(), &mut eq));
            if eq {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn enumeration_error_propagates() {
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
        // A proxy whose ownKeys trap throws must make GetPropertyKeys fail with the
        // exception pending, not swallow it.
        rooted!(&in(context) let mut rval = UndefinedValue());
        let options = CompileOptionsWrapper::new(&context, c"t".to_owned(), 1);
        assert!(evaluate_script(
            context,
            global.handle(),
            "new Proxy({}, { ownKeys() { throw new Error(\"boom\"); } })",
            rval.handle_mut(),
            options,
        )
        .is_ok());
        rooted!(&in(context) let proxy = rval.get().to_object());
        let mut ids = IdVector::new(context);
        assert!(!GetPropertyKeys(
            context,
            proxy.handle().into(),
            JSITER_OWNONLY,
            ids.handle_mut(),
        ));
        assert!(JS_IsExceptionPending(context.raw_cx()));
    }
}

#[test]
fn non_enumerable_keys_hidden_without_hidden_flag() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        rooted!(&in(context) let a = Int32Value(1));
        rooted!(&in(context) let b = Int32Value(2));
        assert!(JS_DefineProperty(context.raw_cx(), obj.handle().into(), c"a".as_ptr(), a.handle().into(), JSPROP_ENUMERATE as u32));
        assert!(JS_DefineProperty(context.raw_cx(), obj.handle().into(), c"b".as_ptr(), b.handle().into(), 0));
        let mut ids = IdVector::new(context);
        assert!(GetPropertyKeys(context, obj.handle().into(), JSITER_OWNONLY, ids.handle_mut()));
        assert_eq!(ids.len(), 1);
        assert_eq!(count_key(context, &ids, c"a"), 1);
        let mut ids2 = IdVector::new(context);
        assert!(GetPropertyKeys(context, obj.handle().into(), JSITER_OWNONLY | JSITER_HIDDEN, ids2.handle_mut()));
        assert_eq!(ids2.len(), 2);
    }
}

#[test]
fn shadowed_prototype_key_listed_once() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        rooted!(&in(context) let proto = JS_NewPlainObject(context));
        rooted!(&in(context) let v = Int32Value(1));
        assert!(JS_DefineProperty(context.raw_cx(), proto.handle().into(), c"s".as_ptr(), v.handle().into(), JSPROP_ENUMERATE as u32));
        assert!(JS_DefineProperty(context.raw_cx(), obj.handle().into(), c"s".as_ptr(), v.handle().into(), JSPROP_ENUMERATE as u32));
        assert!(JS_SetPrototype(context.raw_cx(), obj.handle().into(), proto.handle().into()));
        let mut ids = IdVector::new(context);
        assert!(GetPropertyKeys(context, obj.handle().into(), 0, ids.handle_mut()));
        assert_eq!(ids.len(), 1);
        assert_eq!(count_key(context, &ids, c"s"), 1);
    }
}

#[test]
fn non_enumerable_own_key_shadows_enumerable_prototype_key() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let _global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, _global.handle());
        let context = &mut realm;
        rooted!(&in(context) let obj = JS_NewPlainObject(context));
        rooted!(&in(context) let proto = JS_NewPlainObject(context));
        rooted!(&in(context) let v = Int32Value(1));
        assert!(JS_DefineProperty(context.raw_cx(), proto.handle().into(), c"s".as_ptr(), v.handle().into(), JSPROP_ENUMERATE as u32));
        assert!(JS_DefineProperty(context.raw_cx(), obj.handle().into(), c"s".as_ptr(), v.handle().into(), 0));
        assert!(JS_SetPrototype(context.raw_cx(), obj.handle().into(), proto.handle().into()));
        // Without JSITER_HIDDEN the non-enumerable own key shadows the prototype's:
        // "s" is not reported at all.
        let mut ids = IdVector::new(context);
        assert!(GetPropertyKeys(context, obj.handle().into(), 0, ids.handle_mut()));
        assert_eq!(ids.len(), 0);
        // With JSITER_HIDDEN it appears exactly once (the walk also reaches
        // Object.prototype's hidden keys, so only count "s").
        let mut ids2 = IdVector::new(context);
        assert!(GetPropertyKeys(context, obj.handle().into(), JSITER_HIDDEN, ids2.handle_mut()));
        assert_eq!(count_key(context, &ids2, c"s"), 1);
    }
}
