/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Compartments, wrappers, principals and the DOM hooks Servo installs at startup. The original is `js/src/vm/Compartment.cpp`
//! (`Compartment::wrap`), `js/src/proxy/Wrapper.cpp` (unwrapping), `js/src/vm/JSPrincipals.cpp`, `jsfriendapi.cpp`, and
//! mozjs_sys's `jsglue.cpp` (`RustJSPrincipals`).
//!
//! Model: one compartment for the whole context. Every object is "in the current compartment", so wrapping into it is the
//! identity, except for SpiderMonkey's rule that a Window global is never exposed to script: `wrap` turns it into its
//! WindowProxy (`ToWindowProxyIfWindow`), and that rule holds here through the table `js::SetWindowProxy` fills.
//! Cross-origin wrappers are Servo's own proxies (handler kind Wrapper); unwrapping walks those to their targets.
//! Not modelled: separate compartments per origin and their security checks (`CheckedUnwrap` refusing): a page and an iframe of
//! another origin share one compartment here, and Servo's cross-origin checks in its proxy handlers are what remains.

use ::std::ffi::c_void;

use brimstone_core::runtime::embed::{self, api};
use brimstone_core::runtime::gc::HandleScope;

use super::cx::{current, current_ptr, cx_of};
use super::realm::{global_of, object_ptr};
use crate::boundary::value_bits;
use crate::jsapi::*;

// ---- Window and WindowProxy ----

/// The WindowProxy of `obj` if it is a Window (a global with a proxy registered by `js::SetWindowProxy`), else `obj`.
fn outerize(obj: *mut JSObject) -> *mut JSObject {
    if obj.is_null() {
        return obj;
    }
    // SAFETY: a short reborrow; nothing here re-enters the engine.
    match unsafe { current() }.window_proxies.get(&(obj as usize)) {
        Some(&p) => p as *mut JSObject,
        None => obj,
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__detail__ToWindowProxyIfWindowSlow(obj: *mut JSObject) -> *mut JSObject {
    outerize(obj)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__detail__IsWindowSlow(obj: *mut JSObject) -> bool {
    unsafe { current() }.window_proxies.contains_key(&(obj as usize))
}

// ---- Wrapping into the current compartment ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_WrapObject(_cx: *mut JSContext, objp: JS::MutableHandleObject) -> bool {
    unsafe { *objp.ptr = outerize(*objp.ptr) };
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_WrapValue(_cx: *mut JSContext, vp: JS::MutableHandleValue) -> bool {
    let bits = unsafe { (*vp.ptr).asBits_ };
    if value_bits::is_traced(bits) && super::trace_kind_of(bits) == JS::TraceKind::Object {
        let o = outerize(bits as usize as *mut JSObject);
        unsafe { (*vp.ptr).asBits_ = o as usize as u64 };
    }
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_WrapPropertyDescriptor(cx: *mut JSContext, desc: JS::MutableHandle<JS::PropertyDescriptor>) -> bool {
    let d = unsafe { &mut *desc.ptr };
    let mut v = d.value_;
    let vh = JS::MutableHandle { _phantom_0: Default::default(), ptr: &mut v };
    unsafe { JS_WrapValue(cx, vh) };
    d.value_ = v;
    d.getter_ = outerize(d.getter_);
    d.setter_ = outerize(d.setter_);
    true
}

/// One compartment: nothing to refresh.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_RefreshCrossCompartmentWrappers(_cx: *mut JSContext, _obj: JS::Handle<*mut JSObject>) -> bool {
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__RecomputeWrappers(
    _cx: *mut JSContext,
    _source: *const js::CompartmentFilter,
    _target: *const js::CompartmentFilter,
) -> bool {
    true
}

// ---- Unwrapping ----

fn is_wrapper(obj: *mut JSObject) -> bool {
    unsafe { super::proxy::glue__IsWrapper(obj) }
}

/// The target a wrapper forwards to (its private value).
fn target(obj: *mut JSObject) -> *mut JSObject {
    let p = embed::proxy_private(object_ptr(obj));
    if p.is_object() { p.as_object().as_ptr().cast() } else { ::std::ptr::null_mut() }
}

fn unwrap_all(mut obj: *mut JSObject, stop_at_window_proxy: bool) -> *mut JSObject {
    while !obj.is_null() && is_wrapper(obj) {
        if stop_at_window_proxy && unsafe { super::proxy::js__IsWindowProxy(obj) } {
            break;
        }
        let t = target(obj);
        if t.is_null() {
            break;
        }
        obj = t;
    }
    obj
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__UncheckedUnwrapObject(obj: *mut JSObject, stop_at_window_proxy: bool) -> *mut JSObject {
    unwrap_all(obj, stop_at_window_proxy)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__UnwrapObjectStatic(obj: *mut JSObject) -> *mut JSObject {
    unwrap_all(obj, true)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__UnwrapObjectDynamic(obj: *mut JSObject, _cx: *mut JSContext, stop: bool) -> *mut JSObject {
    unwrap_all(obj, stop)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__CheckedUnwrapStatic(obj: *mut JSObject) -> *mut JSObject {
    unwrap_all(obj, true)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__CheckedUnwrapDynamic(obj: *mut JSObject, _cx: *mut JSContext, stop: bool) -> *mut JSObject {
    unwrap_all(obj, stop)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__UnwrapOneCheckedDynamic(obj: JS::HandleObject, _cx: *mut JSContext, stop: bool) -> *mut JSObject {
    let o = unsafe { *obj.ptr };
    if is_wrapper(o) && !(stop && unsafe { super::proxy::js__IsWindowProxy(o) }) {
        let t = target(o);
        if !t.is_null() {
            return t;
        }
    }
    o
}

// ---- Compartment queries ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__IsSystemCompartment(_comp: *mut JS::Compartment) -> bool {
    false
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__IsSharableCompartment(_comp: *mut JS::Compartment) -> bool {
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__AssertSameCompartment(_cx: *mut JSContext, _obj: *mut JSObject) {}
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__AssertSameCompartment1(_cx: *mut JSContext, _v: JS::HandleValue) {}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__IsObjectInContextCompartment(_obj: *mut JSObject, _cx: *const JSContext) -> bool {
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_IsGlobalObject(obj: *mut JSObject) -> bool {
    embed::host_flags(object_ptr(obj)).is_some_and(|f| f.contains(embed::HostFlags::IS_GLOBAL))
}

/// Dead wrappers are what a nuked cross-compartment wrapper becomes; with one compartment there are none.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_IsDeadWrapper(_obj: *mut JSObject) -> bool {
    false
}

// ---- DOM hooks Servo installs ----

/// SpiderMonkey's JIT uses these to optimise property access on DOM proxies; the interpreter only keeps them. Process-wide, as in
/// SpiderMonkey: Servo sets them once while it builds the browser, before any context exists.
pub struct DomProxyInformation {
    pub family: *const c_void,
    pub shadows: JS::DOMProxyShadowsCheck,
    pub remote_family: *const c_void,
}
// SAFETY: written once at startup by Servo, read-only after; the pointers are Servo's statics.
unsafe impl Send for DomProxyInformation {}
unsafe impl Sync for DomProxyInformation {}
pub static DOM_PROXY_INFORMATION: ::std::sync::RwLock<DomProxyInformation> = ::std::sync::RwLock::new(DomProxyInformation {
    family: ::std::ptr::null(),
    shadows: None,
    remote_family: ::std::ptr::null(),
});

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetDOMProxyInformation(
    family: *const c_void,
    shadows: JS::DOMProxyShadowsCheck,
    remote_family: *const c_void,
) {
    *DOM_PROXY_INFORMATION.write().unwrap() = DomProxyInformation { family, shadows, remote_family };
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__SetDOMCallbacks(cx: *mut JSContext, callbacks: *const js::DOMCallbacks) {
    unsafe { cx_of(cx) }.dom_callbacks = callbacks;
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__GetDOMCallbacks(cx: *mut JSContext) -> *const js::DOMCallbacks {
    unsafe { cx_of(cx) }.dom_callbacks
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetWrapObjectCallbacks(cx: *mut JSContext, callbacks: *const JSWrapObjectCallbacks) {
    unsafe { cx_of(cx) }.wrap_callbacks = callbacks;
}

/// SpiderMonkey keeps a DOM object's reflector alive while its wrapper may be "preserved"; Brimstone traces reflectors through
/// the DOM objects' own trace hooks every GC, so the callbacks are kept and never needed.
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__SetPreserveWrapperCallbacks(
    cx: *mut JSContext,
    preserve: js::PreserveWrapperCallback,
    released: js::HasReleasedWrapperCallback,
) {
    unsafe { cx_of(cx) }.preserve_wrapper = preserve;
    unsafe { cx_of(cx) }.has_released_wrapper = released;
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__CommitPendingWrapperPreservations(_cx: *mut JSContext) {}

// ---- Principals (mozjs_sys jsglue.cpp RustJSPrincipals) ----

#[repr(C)]
struct RustPrincipals {
    base: JSPrincipals,
    callbacks: crate::glue::JSPrincipalsCallbacks,
    private: *mut c_void,
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__CreateRustJSPrincipals(
    callbacks: *const crate::glue::JSPrincipalsCallbacks,
    private: *mut c_void,
) -> *mut JSPrincipals {
    let p = Box::new(RustPrincipals {
        base: JSPrincipals { vtable_: ::std::ptr::null(), refcount: 0 },
        callbacks: unsafe { *callbacks },
        private,
    });
    Box::into_raw(p).cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__GetRustJSPrincipalsPrivate(principals: *mut JSPrincipals) -> *mut c_void {
    if principals.is_null() { ::std::ptr::null_mut() } else { unsafe { (*principals.cast::<RustPrincipals>()).private } }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__DestroyRustJSPrincipals(principals: *mut JSPrincipals) {
    if !principals.is_null() {
        unsafe { drop(Box::from_raw(principals.cast::<RustPrincipals>())) };
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_HoldPrincipals(principals: *mut JSPrincipals) {
    if !principals.is_null() {
        unsafe { (*principals).refcount += 1 };
    }
}

/// Drops one reference; at zero the destroy callback Servo registered frees it, as `JS_DropPrincipals`.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DropPrincipals(cx: *mut JSContext, principals: *mut JSPrincipals) {
    if principals.is_null() {
        return;
    }
    let rc = unsafe { &mut (*principals).refcount };
    debug_assert!(*rc > 0, "JS_DropPrincipals on principals with no references");
    *rc = rc.saturating_sub(1);
    if *rc == 0 {
        if let Some(destroy) = unsafe { cx_of(cx) }.destroy_principals {
            unsafe { destroy(principals) };
        }
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_InitDestroyPrincipalsCallback(cx: *mut JSContext, op: JSDestroyPrincipalsOp) {
    unsafe { cx_of(cx) }.destroy_principals = op;
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_InitReadPrincipalsCallback(cx: *mut JSContext, op: JSReadPrincipalsOp) {
    unsafe { cx_of(cx) }.read_principals = op;
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetTrustedPrincipals(cx: *mut JSContext, principals: *mut JSPrincipals) {
    unsafe { cx_of(cx) }.trusted_principals = principals;
}

/// Each realm's principals, by its global (globals never move). A realm holds one reference, as SpiderMonkey's does.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetRealmPrincipals(realm: *mut JS::Realm) -> *mut JSPrincipals {
    unsafe { current() }.realm_principals.get(&(realm as usize)).copied().unwrap_or(::std::ptr::null_mut())
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetRealmPrincipals(realm: *mut JS::Realm, principals: *mut JSPrincipals) {
    let c = current_ptr();
    let raw: *mut JSContext = c.cast();
    if !principals.is_null() {
        unsafe { JS_HoldPrincipals(principals) };
    }
    // A short access: JS_DropPrincipals may call Servo's destroy hook, which gets the context.
    let old = unsafe { (*c).realm_principals.insert(realm as usize, principals) };
    if let Some(old) = old {
        unsafe { JS_DropPrincipals(raw, old) };
    }
}

// ---- Realm accessors ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetCurrentRealmOrNull(cx: *mut JSContext) -> *mut JS::Realm {
    unsafe { cx_of(cx) }.rooting.realm_
}

/// The realm of a function: its global's (bound functions and proxies: their own global here, as one compartment).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetFunctionRealm(_cx: *mut JSContext, obj: JS::HandleObject) -> *mut JS::Realm {
    unsafe { super::realm::JS__GetObjectRealmOrNull(*obj.ptr) }
}

unsafe fn realm_intrinsic(cx: *mut JSContext, which: api::Intrinsic) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let o = (*api::intrinsic(realm, which)).as_ptr().cast();
    scope.exit();
    o
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetRealmObjectPrototype(cx: *mut JSContext) -> *mut JSObject {
    unsafe { realm_intrinsic(cx, api::Intrinsic::ObjectPrototype) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetRealmFunctionPrototype(cx: *mut JSContext) -> *mut JSObject {
    unsafe { realm_intrinsic(cx, api::Intrinsic::FunctionPrototype) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetRealmErrorPrototype(cx: *mut JSContext) -> *mut JSObject {
    unsafe { realm_intrinsic(cx, api::Intrinsic::ErrorPrototype) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetRealmIteratorPrototype(cx: *mut JSContext) -> *mut JSObject {
    unsafe { realm_intrinsic(cx, api::Intrinsic::IteratorPrototype) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetRealmArrayPrototype(cx: *mut JSContext) -> *mut JSObject {
    unsafe { realm_intrinsic(cx, api::Intrinsic::ArrayPrototype) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetRealmAsyncIteratorPrototype(cx: *mut JSContext) -> *mut JSObject {
    unsafe { realm_intrinsic(cx, api::Intrinsic::AsyncIteratorPrototype) }
}

/// The realm's global stands for its "key object" (SpiderMonkey: the global, for realms without a separate key).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetRealmKeyObject(cx: *mut JSContext) -> *mut JSObject {
    let g = unsafe { cx_of(cx) }.rooting.realm_;
    global_of(g).as_ptr().cast()
}

// ---- JIT-info calls (jsglue.cpp CallJit*Op): Servo's DOM getters, setters and methods ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__CallJitGetterOp(
    info: *const JSJitInfo,
    cx: *mut JSContext,
    this: JS::HandleObject,
    specialized_this: *mut c_void,
    _argc: ::std::os::raw::c_uint,
    vp: *mut JS::Value,
) -> bool {
    let getter = unsafe { (*info).__bindgen_anon_1.getter }.expect("JSJitInfo without getter");
    let args = JSJitGetterCallArgs { _base: JS::MutableHandle { _phantom_0: Default::default(), ptr: vp } };
    unsafe { getter(cx, this, specialized_this, args) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__CallJitSetterOp(
    info: *const JSJitInfo,
    cx: *mut JSContext,
    this: JS::HandleObject,
    specialized_this: *mut c_void,
    _argc: ::std::os::raw::c_uint,
    vp: *mut JS::Value,
) -> bool {
    let setter = unsafe { (*info).__bindgen_anon_1.setter }.expect("JSJitInfo without setter");
    // The setter reads its value from args[0], vp[2] (`JSJitSetterCallArgs(args)` in jsglue.cpp).
    let args = JSJitSetterCallArgs { _base: JS::MutableHandle { _phantom_0: Default::default(), ptr: unsafe { vp.add(2) } } };
    unsafe { setter(cx, this, specialized_this, args) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__CallJitMethodOp(
    info: *const JSJitInfo,
    cx: *mut JSContext,
    this: JS::HandleObject,
    specialized_this: *mut c_void,
    argc: u32,
    vp: *mut JS::Value,
) -> bool {
    let method = unsafe { (*info).__bindgen_anon_1.method }.expect("JSJitInfo without method");
    let args = JSJitMethodCallArgs { argv_: unsafe { vp.add(2) }, argc_: argc, ..unsafe { ::std::mem::zeroed() } };
    unsafe { method(cx, this, specialized_this, &args) }
}

/// `JS_TransplantObject` in its same-compartment form, which is how Servo retargets a WindowProxy at a new Window on
/// navigation (`windowproxy.rs` `set_window`): `origobj` keeps its identity (its address, so every reference to it) and takes
/// the contents of `target` (class, handler, private, realm, slots), as SpiderMonkey's `JSObject::swap`; `target` gets the old
/// contents and is dropped by Servo. Returns `origobj`. Both must be host objects (proxies) of the same layout; anything else
/// is refused with a pending TypeError, since a cross-compartment transplant has no counterpart here.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_TransplantObject(
    cx: *mut JSContext,
    origobj: JS::HandleObject,
    target: JS::HandleObject,
) -> *mut JSObject {
    let orig = unsafe { *origobj.ptr };
    let tgt = unsafe { *target.ptr };
    if orig == tgt {
        return orig;
    }
    if embed::swap_host_objects(object_ptr(orig), object_ptr(tgt)) {
        return orig;
    }
    unsafe { super::property::fail_type(cx, "JS_TransplantObject: only host objects of the same class layout can be transplanted") };
    ::std::ptr::null_mut()
}
