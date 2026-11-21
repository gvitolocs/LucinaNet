/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Functions: creation (natives via `embed::new_native_function`), definition on objects, inspection, call/construct,
//! `instanceof`, and arrays. The original is `JS_NewFunction`/`JS_DefineFunction`/`JS_CallFunction*` in mozjs_sys 153.6.0-1's
//! `jsapi.cpp` and `js/src/vm/JSFunction.cpp`.

use ::std::cell::RefCell;
use ::std::collections::HashMap;
use ::std::ffi::c_void;
use ::std::os::raw::c_char;

use brimstone_core::runtime::embed::{self, api};
use brimstone_core::runtime::gc::HandleScope;
use brimstone_core::runtime::{Context as Bs, Handle, PropertyKey, Value as BsValue};

use super::cx::cx_of;
use super::property::{fail_type, id_bits_of, id_in, name_key, uc_name_key, spec_name_key};
use super::realm::object_ptr;
use super::util::{c_str, fail, finish};
use crate::jsapi::*;

#[inline]
fn bs_of(cx: *mut JSContext) -> Bs {
    unsafe { cx_of(cx) }.bs
}

#[inline]
unsafe fn obj_h(cx: *mut JSContext, obj: JS::Handle<*mut JSObject>) -> Handle<api::ObjectValue> {
    BsValue::object(object_ptr(unsafe { *obj.ptr })).to_handle(bs_of(cx)).as_object()
}

#[inline]
unsafe fn value_h(cx: *mut JSContext, v: JS::HandleValue) -> Handle<BsValue> {
    super::util::handle_of(bs_of(cx), unsafe { (*v.ptr).asBits_ })
}

/// The arguments of a `JS::HandleValueArray` as Brimstone handles.
unsafe fn args_of(cx: *mut JSContext, args: *const JS::HandleValueArray) -> Vec<Handle<BsValue>> {
    if args.is_null() {
        return Vec::new();
    }
    let a = unsafe { &*args };
    let mut out = Vec::with_capacity(a.length_);
    for i in 0..a.length_ {
        let v = unsafe { *a.elements_.add(i) };
        out.push(super::util::handle_of(bs_of(cx), v.asBits_));
    }
    out
}

/// Create a native function. `name` is the function's name as a property key (None: the empty string).
unsafe fn new_function_impl(
    cx: *mut JSContext,
    call: JSNative,
    info: *const JSJitInfo,
    nargs: ::std::os::raw::c_uint,
    flags: ::std::os::raw::c_uint,
    name: Option<Handle<PropertyKey>>,
    proto: Option<JS::Handle<*mut JSObject>>,
    reserved: u8,
) -> *mut JSObject {
    let Some(call) = call else { return ::std::ptr::null_mut() };
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let constructor = flags & JSFUN_CONSTRUCTOR != 0;
    // Natives from specs carry Servo's JSJitInfo; its generic natives read it back from the callee on every DOM call.
    let r = embed::new_native_function_with_jit_info(bs, realm, call as *const c_void, info.cast(), nargs, name, constructor, reserved);
    let r = match (r, proto) {
        (Ok(f), Some(p)) => {
            // A custom prototype: create with Function.prototype, then set it.
            let proto_obj = BsValue::object(object_ptr(unsafe { *p.ptr })).to_handle(bs).as_object();
            match api::set_prototype_of(bs, f, Some(proto_obj)) {
                Ok(_) => Ok(f),
                Err(_) => Err(()),
            }
        },
        (r, None) => r.map_err(|_| ()),
        (Err(_), Some(_)) => Err(()),
    };
    scope.exit();
    match r {
        Ok(f) => (*f).as_ptr().cast(),
        Err(_) => ::std::ptr::null_mut(),
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewFunction(
    cx: *mut JSContext,
    call: JSNative,
    nargs: ::std::os::raw::c_uint,
    flags: ::std::os::raw::c_uint,
    name: *const c_char,
) -> *mut JSFunction {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let name_h = if name.is_null() {
        None
    } else {
        unsafe { name_key(cx, name) }.ok()
    };
    let f = unsafe { new_function_impl(cx, call, ::std::ptr::null(), nargs, flags, name_h, None, 0) };
    scope.exit();
    f as *mut JSFunction
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__NewFunctionWithReserved(
    cx: *mut JSContext,
    call: JSNative,
    nargs: ::std::os::raw::c_uint,
    flags: ::std::os::raw::c_uint,
    name: *const c_char,
) -> *mut JSFunction {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let name_h = if name.is_null() {
        None
    } else {
        unsafe { name_key(cx, name) }.ok()
    };
    let f = unsafe { new_function_impl(cx, call, ::std::ptr::null(), nargs, flags, name_h, None, 2) };
    scope.exit();
    f as *mut JSFunction
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__NewFunctionByIdWithReserved(
    cx: *mut JSContext,
    native: JSNative,
    nargs: ::std::os::raw::c_uint,
    flags: ::std::os::raw::c_uint,
    id: jsid,
) -> *mut JSFunction {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let name_h = unsafe { id_in(bs, id.asBits_ as u64) };
    let f = unsafe { new_function_impl(cx, native, ::std::ptr::null(), nargs, flags, Some(name_h), None, 2) };
    scope.exit();
    f as *mut JSFunction
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__NewFunctionByIdWithReservedAndProto(
    cx: *mut JSContext,
    native: JSNative,
    proto: JS::Handle<*mut JSObject>,
    nargs: ::std::os::raw::c_uint,
    flags: ::std::os::raw::c_uint,
    id: jsid,
) -> *mut JSFunction {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let name_h = unsafe { id_in(bs, id.asBits_ as u64) };
    let f = unsafe { new_function_impl(cx, native, ::std::ptr::null(), nargs, flags, Some(name_h), Some(proto), 2) };
    scope.exit();
    f as *mut JSFunction
}

// Storage for the `JS::Value` a `GetFunctionNativeReserved` pointer refers to (SpiderMonkey returns a pointer into the
// function's reserved slots; we keep one boxed value per (function, slot)).
thread_local! {
    static RESERVED_SLOTS: RefCell<HashMap<(usize, usize), Box<JS::Value>>> = RefCell::new(HashMap::new());
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__GetFunctionNativeReserved(fun: *mut JSObject, which: usize) -> *const JS::Value {
    let bs = unsafe { cx_of(super::cx::current_raw()) }.bs;
    let scope = HandleScope::enter(bs);
    let f = BsValue::object(object_ptr(fun)).to_handle(bs).as_object();
    let v = embed::function_reserved(bs, f, which);
    scope.exit();
    RESERVED_SLOTS.with(|m| {
        let mut m = m.borrow_mut();
        let e = m.entry((fun as usize, which)).or_insert_with(|| Box::new(JS::Value { asBits_: 0 }));
        e.asBits_ = v.as_raw_bits();
        &**e as *const JS::Value
    })
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__SetFunctionNativeReserved(fun: *mut JSObject, which: usize, val: *const JS::Value) {
    let bs = unsafe { cx_of(super::cx::current_raw()) }.bs;
    let scope = HandleScope::enter(bs);
    let f = BsValue::object(object_ptr(fun)).to_handle(bs).as_object();
    let v = super::util::handle_of(bs, unsafe { (*val).asBits_ });
    let _ = embed::set_function_reserved(bs, f, which, v);
    scope.exit();
}

/// Define a function as a property of `obj` with the given attributes; returns the function or null.
unsafe fn define_function_impl(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    key: Handle<PropertyKey>,
    call: JSNative,
    info: *const JSJitInfo,
    nargs: ::std::os::raw::c_uint,
    attrs: ::std::os::raw::c_uint,
) -> *mut JSFunction {
    unsafe { define_function_reserved(cx, obj, key, call, info, nargs, attrs, 0) }
}

/// `define_function_impl` with `reserved` native reserved slots (`js::DefineFunctionWithReserved` uses 2).
#[allow(clippy::too_many_arguments)]
unsafe fn define_function_reserved(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    key: Handle<PropertyKey>,
    call: JSNative,
    info: *const JSJitInfo,
    nargs: ::std::os::raw::c_uint,
    attrs: ::std::os::raw::c_uint,
    reserved: u8,
) -> *mut JSFunction {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let f = unsafe { new_function_impl(cx, call, info, nargs, attrs, Some(key), None, reserved) };
    let out = if f.is_null() {
        ::std::ptr::null_mut()
    } else {
        let fv = BsValue::object(object_ptr(f)).to_handle(bs);
        let d = api::PropertyDescriptor::data(fv, super::property::attrs_to_flags(attrs));
        let mut o = unsafe { obj_h(cx, obj) };
        let r = o.define_own_property(bs, key, d);
        match r {
            Ok(true) => f as *mut JSFunction,
            _ => ::std::ptr::null_mut(),
        }
    };
    scope.exit();
    out
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineFunction(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    call: JSNative,
    nargs: ::std::os::raw::c_uint,
    attrs: ::std::os::raw::c_uint,
) -> *mut JSFunction {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let out = match key {
        Ok(k) => unsafe { define_function_impl(cx, obj, k, call, ::std::ptr::null(), nargs, attrs) },
        Err(_) => ::std::ptr::null_mut(),
    };
    scope.exit();
    out
}

/// `js::DefineFunctionWithReserved` (jsfriendapi): `JS_DefineFunction` whose function has the two native reserved slots of
/// `NewFunctionWithReserved` (Servo's `import.meta.resolve` keeps its module there).
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__DefineFunctionWithReserved(
    cx: *mut JSContext,
    obj: *mut JSObject,
    name: *const c_char,
    call: JSNative,
    nargs: ::std::os::raw::c_uint,
    attrs: ::std::os::raw::c_uint,
) -> *mut JSFunction {
    let c = unsafe { cx_of(cx) };
    let scope = HandleScope::enter(c.bs);
    let key = unsafe { name_key(cx, name) };
    let handle = JS::Handle { _phantom_0: Default::default(), ptr: &obj };
    let out = match key {
        Ok(k) => unsafe { define_function_reserved(cx, handle, k, call, ::std::ptr::null(), nargs, attrs, 2) },
        Err(_) => ::std::ptr::null_mut(),
    };
    scope.exit();
    out
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineFunctionById(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    call: JSNative,
    nargs: ::std::os::raw::c_uint,
    attrs: ::std::os::raw::c_uint,
) -> *mut JSFunction {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let out = unsafe { define_function_impl(cx, obj, key, call, ::std::ptr::null(), nargs, attrs) };
    scope.exit();
    out
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineUCFunction(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const u16,
    namelen: usize,
    call: JSNative,
    nargs: ::std::os::raw::c_uint,
    attrs: ::std::os::raw::c_uint,
) -> *mut JSFunction {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let out = match key {
        Ok(k) => unsafe { define_function_impl(cx, obj, k, call, ::std::ptr::null(), nargs, attrs) },
        Err(_) => ::std::ptr::null_mut(),
    };
    scope.exit();
    out
}

/// `JS_DefineFunctions` over a null-terminated `JSFunctionSpec` list. A spec with a `selfHostedName` fails with a TypeError
/// naming it (Brimstone's builtins are Rust, there is no self-hosted function to install). JIT-info specs are created as
/// plain natives calling `call.op`.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineFunctions(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    fs: *const JSFunctionSpec,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let mut p = fs;
    let mut ok = true;
    while !p.is_null() && !unsafe { (*p).name.string_ }.is_null() {
        let spec = unsafe { &*p };
        if !spec.selfHostedName.is_null() {
            let name = unsafe { c_str(spec.selfHostedName) };
            match unsafe { define_self_hosted(cx, obj, spec, &name) } {
                Some(true) => {
                    p = unsafe { p.add(1) };
                    continue;
                },
                Some(false) => {
                    ok = false;
                    break;
                },
                None => {
                    ok = unsafe { fail_type(cx, &format!("self-hosted function {name} is not available")) };
                    break;
                },
            }
        }
        let key = unsafe { spec_name_key(cx, spec.name.string_) };
        match key {
            Ok(k) => {
                let f = unsafe { define_function_impl(cx, obj, k, spec.call.op, spec.call.info, spec.nargs as u32, spec.flags as u32) };
                if f.is_null() {
                    ok = false;
                    break;
                }
            },
            Err(e) => {
                ok = unsafe { fail(cx, e) };
                break;
            },
        }
        p = unsafe { p.add(1) };
    }
    scope.exit();
    ok
}

// ---- Inspection ----

/// `JS_GetFunctionObject`: the function object itself.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetFunctionObject(fun: *mut JSFunction) -> *mut JSObject {
    fun as *mut JSObject
}

/// `JS_GetObjectFunction`: the object as a function (identity; callers check callability).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetObjectFunction(obj: *mut JSObject) -> *mut JSFunction {
    obj as *mut JSFunction
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ObjectIsFunction(obj: *mut JSObject) -> bool {
    let bs = embed::context_of(obj.cast());
    // Every entry point that makes handles releases them: without a scope of its own they went to the outermost one, which
    // never exits (the playback handle leak, ~1,400 handles a second, each keeping its object alive).
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    api::is_callable(BsValue::object(object_ptr(obj)).to_handle(bs))
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsCallable(obj: *mut JSObject) -> bool {
    let bs = embed::context_of(obj.cast());
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    api::is_callable(BsValue::object(object_ptr(obj)).to_handle(bs))
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsConstructor(obj: *mut JSObject) -> bool {
    let bs = embed::context_of(obj.cast());
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    api::is_constructor_value(BsValue::object(object_ptr(obj)).to_handle(bs))
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_IsNativeFunction(funobj: *mut JSObject, call: JSNative) -> bool {
    let bs = embed::context_of(funobj.cast());
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    let f = BsValue::object(object_ptr(funobj)).to_handle(bs).as_object();
    embed::native_of(bs, f) == call.map(|c| c as *const c_void)
}

/// The function's name as a string, or null.
unsafe fn function_name_ptr(cx: *mut JSContext, fun: *mut JSObject) -> *mut JSString {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let f = BsValue::object(object_ptr(fun)).to_handle(bs).as_object();
    let r = api::function_name(bs, f);
    scope.exit();
    match r {
        Ok(Some(s)) => (*s).as_ptr().cast(),
        _ => ::std::ptr::null_mut(),
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetFunctionId(
    cx: *mut JSContext,
    fun: JS::Handle<*mut JSFunction>,
    name: JS::MutableHandle<*mut JSString>,
) -> bool {
    let p = unsafe { function_name_ptr(cx, (*fun.ptr).cast()) };
    unsafe { (*name.ptr) = p };
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetMaybePartialFunctionId(fun: *mut JSFunction) -> *mut JSString {
    unsafe { function_name_ptr(super::cx::current_raw(), fun as *mut JSObject) }
}

/// `JS_GetFunctionDisplayId`, as `mozjs-sys/bindgen/jsapi.rs:18923` documents it:
///
/// ```text
/// Return a function's display name as `name` out-parameter.
///
/// This is the defined name if one was given where the function was defined, or it could be an
/// inferred name by the JS engine in the case that the function was defined to be anonymous.
///
/// This can still return nullptr as `name` out-parameter if a useful display name could not be
/// inferred.
///
/// This function returns false if any error happens while generating the function name string for
/// a function with lazy name.
/// ```
///
/// The fork keeps one name per function and has no lazily generated names: the display name is read from the same `name`
/// property as `JS_GetFunctionId` above, so the two agree and a display name is never a partial one. That property is filled
/// in at creation with the name given where the function was defined, the empty string when there was none
/// (`runtime/bytecode/function.rs:279-285` in Brimstone), and the engine infers the name of the binding an anonymous
/// function is assigned to, which is the `inferred name` the doc text means; `api::function_name` (`runtime/embed/api.rs:525`)
/// only answers that property, so a null out-parameter here is a `name` that reads `undefined`.
///
/// Reading a property rather than a compile-time atom is where this differs from the C++:
///
/// - an anonymous function with nothing to infer a name from answers the empty string, where SpiderMonkey's display atom is
///   null and the out-parameter is null. Nothing is displayed either way, so a console message is the same;
/// - `name` is a script-visible configurable own property (`runtime/common_shapes.rs:103-113`): deleting it answers the
///   `Function.prototype` name, which is the empty string (`runtime/intrinsics/function_prototype.rs:84-90`), and a function
///   renamed by script answers its new name, where SpiderMonkey sticks to the name it was defined with;
/// - the doc text's error case exists here as a `name` accessor that throws: the read fails, the exception is left pending
///   and this returns false. There is no other way the name generation can fail, so a plain function always answers true.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetFunctionDisplayId(
    cx: *mut JSContext,
    fun: JS::Handle<*mut JSFunction>,
    name: JS::MutableHandle<*mut JSString>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let f = BsValue::object(object_ptr(unsafe { (*fun.ptr).cast() })).to_handle(bs).as_object();
    let r = api::function_name(bs, f);
    scope.exit();
    let mut out = ::std::ptr::null_mut();
    let ok = match r {
        Ok(Some(s)) => {
            out = (*s).as_ptr().cast();
            true
        },
        Ok(None) => true,
        Err(e) => unsafe { fail(cx, e) },
    };
    unsafe { (*name.ptr) = out };
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetFunctionLength(
    cx: *mut JSContext,
    fun: JS::HandleFunction,
    length: *mut u16,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let f = BsValue::object(object_ptr(unsafe { *fun.ptr } as *mut JSObject)).to_handle(bs).as_object();
    let r = api::function_length(bs, f);
    scope.exit();
    match r {
        Ok(Some(l)) => {
            if !length.is_null() {
                unsafe { *length = l as u16 };
            }
            true
        },
        Ok(None) => {
            if !length.is_null() {
                unsafe { *length = 0 };
            }
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetFunctionArity(fun: *mut JSFunction) -> u16 {
    let bs = unsafe { cx_of(super::cx::current_raw()) }.bs;
    let scope = HandleScope::enter(bs);
    let f = BsValue::object(object_ptr(fun as *mut JSObject)).to_handle(bs).as_object();
    let r = api::function_length(bs, f);
    scope.exit();
    match r {
        Ok(Some(l)) => l as u16,
        _ => 0,
    }
}

// ==== PART 2 ====
// ---- Call and construct ----

unsafe fn call_impl(
    cx: *mut JSContext,
    fun: Handle<BsValue>,
    this: Handle<BsValue>,
    args: *const JS::HandleValueArray,
    rval: JS::MutableHandle<JS::Value>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let argv = unsafe { args_of(cx, args) };
    let r = embed::call(bs, fun, this, &argv);
    let ok = unsafe { finish(cx, r, rval.ptr) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_CallFunctionValue(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    fval: JS::Handle<JS::Value>,
    args: *const JS::HandleValueArray,
    rval: JS::MutableHandle<JS::Value>,
) -> bool {
    let bs = bs_of(cx);
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    let this = BsValue::object(object_ptr(unsafe { *obj.ptr })).to_handle(bs);
    let f = unsafe { value_h(cx, fval) };
    unsafe { call_impl(cx, f, this, args, rval) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_CallFunction(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    fun: JS::Handle<*mut JSFunction>,
    args: *const JS::HandleValueArray,
    rval: JS::MutableHandle<JS::Value>,
) -> bool {
    let bs = bs_of(cx);
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    let this = BsValue::object(object_ptr(unsafe { *obj.ptr })).to_handle(bs);
    let f = BsValue::object(object_ptr(unsafe { *fun.ptr } as *mut JSObject)).to_handle(bs);
    unsafe { call_impl(cx, f, this, args, rval) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_CallFunctionName(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    args: *const JS::HandleValueArray,
    rval: JS::MutableHandle<JS::Value>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => {
            let o = unsafe { obj_h(cx, obj) };
            let f = match o.get(bs, k, o.as_value()) {
                Ok(f) => f,
                Err(e) => {
                    scope.exit();
                    return unsafe { fail(cx, e) };
                },
            };
            let this = o.as_value();
            drop(scope);
            unsafe { call_impl(cx, f, this, args, rval) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__Call(
    cx: *mut JSContext,
    thisv: JS::Handle<JS::Value>,
    fun: JS::Handle<JS::Value>,
    args: *const JS::HandleValueArray,
    rval: JS::MutableHandle<JS::Value>,
) -> bool {
    // Servo calls every callback (functions, event listeners, observers) through here: the handles must not outlive the call.
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs_of(cx));
    let this = unsafe { value_h(cx, thisv) };
    let f = unsafe { value_h(cx, fun) };
    unsafe { call_impl(cx, f, this, args, rval) }
}

unsafe fn construct_impl(
    cx: *mut JSContext,
    fun: Handle<BsValue>,
    new_target: Option<Handle<api::ObjectValue>>,
    args: *const JS::HandleValueArray,
    objp: JS::MutableHandle<*mut JSObject>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let argv = unsafe { args_of(cx, args) };
    let r = embed::construct(bs, fun, &argv, new_target);
    scope.exit();
    match r {
        Ok(o) => {
            unsafe { (*objp.ptr) = (*o).as_ptr().cast() };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__Construct(
    cx: *mut JSContext,
    fun: JS::Handle<JS::Value>,
    newTarget: JS::Handle<*mut JSObject>,
    args: *const JS::HandleValueArray,
    objp: JS::MutableHandle<*mut JSObject>,
) -> bool {
    let bs = bs_of(cx);
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    let f = unsafe { value_h(cx, fun) };
    let nt = BsValue::object(object_ptr(unsafe { *newTarget.ptr })).to_handle(bs).as_object();
    unsafe { construct_impl(cx, f, Some(nt), args, objp) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__Construct1(
    cx: *mut JSContext,
    fun: JS::Handle<JS::Value>,
    args: *const JS::HandleValueArray,
    objp: JS::MutableHandle<*mut JSObject>,
) -> bool {
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs_of(cx));
    let f = unsafe { value_h(cx, fun) };
    unsafe { construct_impl(cx, f, None, args, objp) }
}

// ---- instanceof ----

/// `InstanceOf`: the `Symbol.hasInstance` method if present, else `OrdinaryHasInstance`.
unsafe fn has_instance_impl(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    v: Handle<BsValue>,
    bp: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, obj) };
    let has_instance_key = bs.symbols.has_instance();
    let r = (|| -> Result<bool, brimstone_core::runtime::eval_result::EvalError> {
        let method = api::get(bs, o, has_instance_key)?;
        if api::is_callable(method) {
            let r = embed::call(bs, method, o.as_value(), &[v])?;
            return Ok(api::to_boolean(*r));
        }
        brimstone_core::runtime::abstract_operations::ordinary_has_instance(bs, o.as_value(), v)
    })();
    scope.exit();
    match r {
        Ok(b) => {
            if !bp.is_null() {
                unsafe { *bp = b };
            }
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_HasInstance(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    v: JS::Handle<JS::Value>,
    bp: *mut bool,
) -> bool {
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs_of(cx));
    let val = unsafe { value_h(cx, v) };
    unsafe { has_instance_impl(cx, obj, val, bp) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_InstanceOf(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    _clasp: *const JSClass,
    args: *mut JS::CallArgs,
) -> bool {
    let bs = bs_of(cx);
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    let v = if args.is_null() || unsafe { (*args).argv_ }.is_null() {
        bs_of(cx).undefined()
    } else {
        super::util::handle_of(bs_of(cx), unsafe { *(*args).argv_ }.asBits_)
    };
    let mut bp = false;
    let ok = unsafe { has_instance_impl(cx, obj, v, &mut bp) };
    if ok && !args.is_null() {
        // CallArgs' return slot is argv[-1] in SpiderMonkey; mozjs reads the boolean from `bp` here, so nothing to write.
        let _ = bp;
    }
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__OrdinaryHasInstance(
    cx: *mut JSContext,
    objArg: JS::HandleObject,
    v: JS::HandleValue,
    bp: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, objArg) };
    let val = unsafe { value_h(cx, v) };
    let r = brimstone_core::runtime::abstract_operations::ordinary_has_instance(bs, o.as_value(), val);
    scope.exit();
    match r {
        Ok(b) => {
            if !bp.is_null() {
                unsafe { *bp = b };
            }
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

// ---- Value conversions ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ValueToFunction(cx: *mut JSContext, v: JS::HandleValue) -> *mut JSFunction {
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs_of(cx));
    let f = unsafe { value_h(cx, v) };
    if api::is_callable(f) {
        f.as_object().as_ptr().cast()
    } else {
        ::std::ptr::null_mut()
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ValueToConstructor(cx: *mut JSContext, v: JS::HandleValue) -> *mut JSFunction {
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs_of(cx));
    let f = unsafe { value_h(cx, v) };
    if api::is_constructor_value(f) {
        f.as_object().as_ptr().cast()
    } else {
        ::std::ptr::null_mut()
    }
}

// ---- Arrays ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewArrayObject(
    cx: *mut JSContext,
    contents: *const JS::HandleValueArray,
) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let argv = unsafe { args_of(cx, contents) };
    let r = api::create_array_from_list(bs, &argv);
    scope.exit();
    match r {
        Ok(a) => (*a).as_ptr().cast(),
        Err(_) => ::std::ptr::null_mut(),
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewArrayObject1(cx: *mut JSContext, length: usize) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let r = api::array_with_length(bs, realm, length as u64);
    scope.exit();
    match r {
        Ok(a) => (*a).as_ptr().cast(),
        Err(_) => ::std::ptr::null_mut(),
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsArrayObject(
    cx: *mut JSContext,
    value: JS::Handle<JS::Value>,
    isArray: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let v = unsafe { value_h(cx, value) };
    let r = api::is_array(bs, v);
    scope.exit();
    match r {
        Ok(b) => {
            if !isArray.is_null() {
                unsafe { *isArray = b };
            }
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetArrayLength(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    lengthp: *mut u32,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, obj) };
    let r = api::length_of_array_like(bs, o);
    scope.exit();
    match r {
        Ok(l) => {
            if !lengthp.is_null() {
                unsafe { *lengthp = l as u32 };
            }
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetArrayLength(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    length: u32,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let mut o = unsafe { obj_h(cx, obj) };
    let key = match api::property_key_from_string(bs, "length") {
        Ok(k) => k,
        Err(e) => {
            scope.exit();
            return unsafe { fail(cx, e) };
        },
    };
    // Array length: writable, non-enumerable, non-configurable.
    let flags = api::PropertyFlags::empty().writable();
    let d = api::PropertyDescriptor::data(bs.smi(length as i32), flags);
    let r = o.define_own_property(bs, key, d);
    scope.exit();
    match r {
        Ok(true) => true,
        Ok(false) => {
            unsafe { fail_type(cx, "cannot set array length") }
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

/// SpiderMonkey's self-hosted functions that Servo's bindings name (codegen.py: `$ArrayValues`, `ArrayKeys`, `ArrayEntries`,
/// `ArrayForEach`, for indexed collections' iterators and `forEach`) are the realm's own `Array.prototype` methods, which
/// Brimstone has natively: the spec installs that function under its own name and flags. `None` for any other name.
unsafe fn define_self_hosted(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    spec: &JSFunctionSpec,
    self_hosted: &str,
) -> Option<bool> {
    let method = match self_hosted.trim_start_matches('$') {
        "ArrayValues" => "values",
        "ArrayKeys" => "keys",
        "ArrayEntries" => "entries",
        "ArrayForEach" => "forEach",
        _ => return None,
    };
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = (|| -> Result<bool, brimstone_core::runtime::eval_result::EvalError> {
        let realm = brimstone_core::runtime::embed::current_realm(bs);
        let array_proto = api::intrinsic(realm, api::Intrinsic::ArrayPrototype);
        let k = api::property_key_from_string(bs, method)?;
        let f = api::get(bs, array_proto, k)?;
        let key = unsafe { spec_name_key(cx, spec.name.string_) }?;
        let d = api::PropertyDescriptor::data(f, super::property::attrs_to_flags(spec.flags as u32));
        let mut o = unsafe { obj_h(cx, obj) };
        o.define_own_property(bs, key, d)
    })();
    let out = match r {
        Ok(defined) => Some(defined),
        Err(e) => {
            unsafe { fail(cx, e) };
            Some(false)
        },
    };
    scope.exit();
    out
}

/// `FUNCTION_VALUE_TO_JITINFO`: the JSJitInfo a native made from a spec carries; null for anything else (never panics).
#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__RUST_FUNCTION_VALUE_TO_JITINFO(v: JS::Value) -> *const JSJitInfo {
    let bits = super::func::non_zero(v.asBits_);
    if !crate::boundary::value_bits::is_traced(bits) {
        return ::std::ptr::null();
    }
    let bs = embed::context_of(bits as usize as *const c_void);
    embed::native_jit_info(bs, BsValue::from_raw_bits(bits)).map_or(::std::ptr::null(), |p| p.cast())
}
