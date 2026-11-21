/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Objects: creation, classes and reserved slots. The original is `JS_NewObject` & co. in `jsapi.cpp` and
//! `js/src/vm/JSObject.cpp`.
//!
//! An object made with a `JSClass` (Servo's DOM reflectors, globals, its own classes) is a Brimstone host object: it never
//! moves, carries the class pointer and its reserved slots, and the class's trace, finalize and resolve hooks run through
//! `cx::ServoHost`. Every other object reports one of the static classes below, which Servo only compares against its own.

use brimstone_core::runtime::embed::{self, api};
use brimstone_core::runtime::gc::HandleScope;
use brimstone_core::runtime::{Handle, Value as BsValue};

use super::cx::cx_of;
use super::realm::{host_flags_of, object_ptr};
use crate::boundary::bounds;
use crate::jsapi::*;

const fn plain_class(name: &'static ::std::ffi::CStr) -> JSClass {
    JSClass {
        name: name.as_ptr(),
        flags: 0,
        cOps: ::std::ptr::null(),
        spec: ::std::ptr::null(),
        ext: ::std::ptr::null(),
        oOps: ::std::ptr::null(),
    }
}

/// What `JS::GetClass` reports for objects that are not host objects.
pub static PLAIN_OBJECT_CLASS: JSClass = plain_class(c"Object");
pub static FUNCTION_CLASS: JSClass = plain_class(c"Function");
pub static ARRAY_CLASS: JSClass = plain_class(c"Array");

/// `JS::GetClass(obj)`.
pub unsafe fn class_of(obj: *mut JSObject) -> *const JSClass {
    let o = object_ptr(obj);
    if let Some(class) = embed::host_class(o) {
        return class.cast();
    }
    let v = BsValue::object(o);
    // Called directly by Servo (JS::GetClass, wrapping values): the handle must not outlive the call.
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(embed::context_of(obj.cast()));
    if api::is_callable(v.to_handle(embed::context_of(obj.cast()))) {
        &FUNCTION_CLASS
    } else if o.is::<api::ArrayObject>() {
        &ARRAY_CLASS
    } else {
        &PLAIN_OBJECT_CLASS
    }
}

/// A new object of `clasp` (null: a plain object) with prototype `proto` (None: `Object.prototype` of the current realm).
pub unsafe fn new_object(cx: *mut JSContext, clasp: *const JSClass, proto: Option<*mut JSObject>) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let proto: Handle<api::ObjectValue> = match proto {
        Some(p) if !p.is_null() => BsValue::object(object_ptr(p)).to_handle(bs).as_object(),
        _ => embed::object_prototype(realm),
    };
    let result = if clasp.is_null() || clasp == &PLAIN_OBJECT_CLASS as *const JSClass {
        match api::new_object(bs) {
            Ok(o) => {
                let _ = api::set_prototype_of(bs, o, Some(proto));
                Ok((*o).as_ptr())
            },
            Err(e) => Err(e),
        }
    } else {
        let flags = unsafe { host_flags_of(clasp) };
        let slots = bounds::reserved_slot_count(unsafe { (*clasp).flags });
        embed::new_host_object(
            bs,
            realm,
            clasp.cast(),
            flags,
            Some(proto),
            slots,
            ::std::ptr::null(),
            bs.undefined(),
        )
        .map(|o| (*o).as_ptr())
    };
    scope.exit();
    match result {
        Ok(p) => p.cast(),
        Err(_) => ::std::ptr::null_mut(),
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewObject(cx: *mut JSContext, clasp: *const JSClass) -> *mut JSObject {
    unsafe { new_object(cx, clasp, None) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewPlainObject(cx: *mut JSContext) -> *mut JSObject {
    unsafe { new_object(cx, ::std::ptr::null(), None) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewObjectWithGivenProto(
    cx: *mut JSContext,
    clasp: *const JSClass,
    proto: JS::Handle<*mut JSObject>,
) -> *mut JSObject {
    let p = unsafe { *proto.ptr };
    if p.is_null() {
        // A null prototype, not the default one.
        return unsafe { new_object_null_proto(cx, clasp) };
    }
    unsafe { new_object(cx, clasp, Some(p)) }
}

unsafe fn new_object_null_proto(cx: *mut JSContext, clasp: *const JSClass) -> *mut JSObject {
    let obj = unsafe { new_object(cx, clasp, None) };
    if obj.is_null() {
        return obj;
    }
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = BsValue::object(object_ptr(obj)).to_handle(bs).as_object();
    let _ = api::set_prototype_of(bs, o, None);
    scope.exit();
    obj
}

/// Reserved slot `index`, through the Verus-checked bound: out of range reads undefined (SpiderMonkey asserts).
#[super::poison::engine_shim(failure = unsafe { (*dest).asBits_ = crate::boundary::value_bits::undefined() })]
pub unsafe extern "C" fn glue__JS_GetReservedSlot(obj: *mut JSObject, index: u32, dest: *mut JS::Value) {
    let o = object_ptr(obj);
    let v = match bounds::reserved_slot(embed::num_reserved_slots(o), index) {
        Some(i) => embed::reserved_slot(o, i as u32).as_raw_bits(),
        None => crate::boundary::value_bits::undefined(),
    };
    unsafe { (*dest).asBits_ = v };
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetReservedSlot(obj: *mut JSObject, index: u32, v: *const JS::Value) {
    let o = object_ptr(obj);
    let i = bounds::reserved_slot(embed::num_reserved_slots(o), index)
        .unwrap_or_else(|| panic!("JS_SetReservedSlot: slot {index} out of range"));
    let bits = super::func::non_zero(unsafe { (*v).asBits_ });
    embed::set_reserved_slot(o, i as u32, BsValue::from_raw_bits(bits));
}

/// The class's resolve hook after an own-property miss, as SpiderMonkey's `CallResolveOp` (`js/src/vm/NativeObject-inl.h`)
/// before the prototype walk: Servo's Window global defines its DOM interface objects lazily this way (`resolve_global`).
/// True when the hook says it resolved the id; the engine then looks the property up again.
///
/// `cx` stays a raw pointer: the hook is Servo's and re-enters the engine (it defines properties, and may GC).
pub unsafe fn call_class_resolve(
    cx: *mut super::cx::Cx,
    bs: brimstone_core::runtime::Context,
    object: Handle<api::ObjectValue>,
    key: Handle<brimstone_core::runtime::PropertyKey>,
) -> brimstone_core::runtime::EvalResult<bool> {
    if super::realm::CREATING_GLOBAL.get() {
        return Ok(false);
    }
    let obj: *mut JSObject = (*object).as_ptr().cast();
    let class = unsafe { class_of(obj) };
    let ops = unsafe { (*class).cOps };
    let Some(resolve) = (if ops.is_null() { None } else { unsafe { (*ops).resolve } }) else {
        return Ok(false);
    };
    let id = unsafe { super::property::id_out(bs, key) };
    // The id lives in a traced frame slot while the hook runs (it may GC); host objects never move, so `obj` stays valid.
    let Some(start) = (unsafe { (*cx).frames.push(1) }) else {
        return brimstone_core::runtime::error::range_error(bs, "too much recursion");
    };
    let slot = unsafe { (*cx).frame_values.base().add(start) };
    unsafe { *slot = id };
    let raw_cx: *mut JSContext = cx.cast();
    let mut resolved = false;
    let ok = unsafe {
        resolve(
            raw_cx,
            JS::Handle { _phantom_0: Default::default(), ptr: &obj },
            JS::Handle { _phantom_0: Default::default(), ptr: slot.cast() },
            &mut resolved,
        )
    };
    let popped = unsafe { (*cx).frames.pop() };
    debug_assert_eq!(popped, Some(start));
    if ok {
        return Ok(resolved);
    }
    match unsafe { (*cx).take_pending() } {
        Some(bits) => Err(embed::throw_value(super::util::handle_of(bs, bits))),
        None => Err(super::uncatchable(bs)),
    }
}

/// Calling or constructing a host object whose class has `JSClassOps::call` / `construct` (Servo's DOM interface objects,
/// `NonCallbackInterfaceObjectClass`), as SpiderMonkey's `CallJSNative` with the class hook: the SpiderMonkey `vp` layout
/// (callee, this or the constructing magic, arguments, new.target) in a traced frame. Without a hook it is a TypeError, as
/// SpiderMonkey's "is not a function" / "is not a constructor".
///
/// `cx` is a raw pointer, reborrowed only for the frame push and pop: the hook re-enters the engine through `cx_of`.
pub unsafe fn call_class_hook(
    cx: *mut super::cx::Cx,
    bs: brimstone_core::runtime::Context,
    object: Handle<api::ObjectValue>,
    this: Handle<BsValue>,
    arguments: &[Handle<BsValue>],
    new_target: Option<Handle<api::ObjectValue>>,
) -> brimstone_core::runtime::EvalResult<Handle<BsValue>> {
    use crate::boundary::value_bits;
    let obj: *mut JSObject = (*object).as_ptr().cast();
    let class = unsafe { class_of(obj) };
    let ops = unsafe { (*class).cOps };
    let constructing = new_target.is_some();
    let hook = if ops.is_null() {
        None
    } else if constructing {
        unsafe { (*ops).construct }
    } else {
        unsafe { (*ops).call }
    };
    let Some(hook) = hook else {
        let what = if constructing { "not a constructor" } else { "not a function" };
        return brimstone_core::runtime::error::type_error(bs, &format!("host object is {what}"));
    };
    let argc = arguments.len() as u32;
    let Some(len) = bounds::vp_len(argc + constructing as u32, 0) else {
        return brimstone_core::runtime::error::range_error(bs, "too many arguments");
    };
    let Some(start) = (unsafe { (*cx).frames.push(len) }) else {
        return brimstone_core::runtime::error::range_error(bs, "too much recursion");
    };
    let vp = unsafe { (*cx).frame_values.base().add(start) };
    unsafe {
        *vp = BsValue::object(*object).as_raw_bits();
        *vp.add(1) = if constructing { value_bits::empty() } else { this.as_raw_bits() };
        for (i, a) in arguments.iter().enumerate() {
            *vp.add(2 + i) = a.as_raw_bits();
        }
        if let Some(nt) = new_target {
            *vp.add(2 + argc as usize) = BsValue::object(*nt).as_raw_bits();
        }
    }
    let ok = unsafe { hook(cx.cast(), argc, vp.cast()) };
    let rval = super::func::non_zero(unsafe { *vp });
    let popped = unsafe { (*cx).frames.pop() };
    debug_assert_eq!(popped, Some(start));
    if ok {
        return Ok(BsValue::from_raw_bits(rval).to_handle(bs));
    }
    match unsafe { (*cx).take_pending() } {
        Some(bits) => Err(embed::throw_value(super::util::handle_of(bs, bits))),
        None => Err(super::uncatchable(bs)),
    }
}
