/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Proxies: `js::NewProxyObject` and the three handlers of mozjs_sys's `jsglue.cpp` (`BaseProxyHandler`,
//! `ForwardingProxyHandler`, `WrapperProxyHandler`), which Servo's DOM proxies (WindowProxy, Location, named and indexed
//! property objects) fill with `ProxyTraps`.
//!
//! A proxy is a Brimstone host object with a handler pointer and a private value; every internal method comes to
//! `ServoHost::proxy_*` and then here. A trap Servo set is called; a missing one does what the C++ base class does
//! (`js/src/proxy/BaseProxyHandler.cpp`, `Wrapper.cpp`): the Base handler derives `has`, `get`, `set` and friends from
//! `getOwnPropertyDescriptor` and the prototype; Forwarding and Wrapper handlers forward to the target (the private value).
//!
//! Handles given to traps point into a native frame (`boundary::roots`), so the GC sees and updates ids and values while a trap
//! runs; descriptors go on `Cx::desc_roots`. Proxies themselves are host objects and never move.

use ::std::ffi::c_void;

use brimstone_core::runtime::embed::{self, api, HostFlags};
use brimstone_core::runtime::eval_result::EvalError;
use brimstone_core::runtime::gc::{HandleScope, HeapPtr};
use brimstone_core::runtime::object_value::ObjectValue;
use brimstone_core::runtime::{Context as Bs, EvalResult, Handle, PropertyKey, Value as BsValue};

use super::cx::{current_ptr, cx_of, Cx};
use super::func::with_slots;
use super::realm::object_ptr;
use super::util::{fail, handle_of};
use crate::boundary::{bounds, value_bits};
use crate::glue::ProxyTraps;
use crate::jsapi::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HandlerKind {
    /// `BaseProxyHandler`: the mandatory traps are Servo's, the rest derive from them.
    Base,
    /// `ForwardingProxyHandler` and `WrapperProxyHandler`: missing traps forward to the target.
    Forwarding,
    Wrapper,
}

/// What the `Create*ProxyHandler` glue returns.
pub struct Handler {
    pub traps: ProxyTraps,
    pub extra: *const c_void,
    pub kind: HandlerKind,
}

/// The two families of jsglue.cpp; Servo compares `GetProxyHandlerFamily()` against what it got.
static BASE_FAMILY: u8 = 0;
static FORWARDING_FAMILY: u8 = 0;

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__GetProxyHandlerFamily() -> *const c_void {
    (&raw const BASE_FAMILY).cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__CreateProxyHandler(traps: *const ProxyTraps, extra: *const c_void) -> *const c_void {
    Box::into_raw(Box::new(Handler { traps: unsafe { *traps }, extra, kind: HandlerKind::Base })).cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__CreateForwardingProxyHandler(traps: *const ProxyTraps, extra: *const c_void) -> *const c_void {
    Box::into_raw(Box::new(Handler { traps: unsafe { *traps }, extra, kind: HandlerKind::Forwarding })).cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__CreateWrapperProxyHandler(traps: *const ProxyTraps) -> *const c_void {
    Box::into_raw(Box::new(Handler { traps: unsafe { *traps }, extra: ::std::ptr::null(), kind: HandlerKind::Wrapper })).cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__DeleteWrapperProxyHandler(handler: *const c_void) {
    if !handler.is_null() {
        unsafe { drop(Box::from_raw(handler as *mut Handler)) };
    }
}

fn handler_of(obj: *mut JSObject) -> Option<&'static Handler> {
    embed::host_handler(object_ptr(obj)).filter(|h| !h.is_null()).map(|h| unsafe { &*(h as *const Handler) })
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__GetProxyHandler(obj: *mut JSObject) -> *const c_void {
    handler_of(obj).map_or(::std::ptr::null(), |h| (h as *const Handler).cast())
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__GetProxyHandlerExtra(obj: *mut JSObject) -> *const c_void {
    handler_of(obj).map_or(::std::ptr::null(), |h| h.extra)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__IsProxyHandlerFamily(obj: *mut JSObject) -> bool {
    handler_of(obj).is_some_and(|h| h.kind != HandlerKind::Wrapper)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__IsWrapper(obj: *mut JSObject) -> bool {
    handler_of(obj).is_some_and(|h| h.kind == HandlerKind::Wrapper)
}

#[super::poison::engine_shim(failure = unsafe { (*out).asBits_ = value_bits::undefined() })]
pub unsafe extern "C" fn glue__GetProxyPrivate(obj: *mut JSObject, out: *mut JS::Value) {
    unsafe { (*out).asBits_ = embed::proxy_private(object_ptr(obj)).as_raw_bits() };
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__SetProxyPrivate(obj: *mut JSObject, v: *const JS::Value) {
    let bits = super::func::non_zero(unsafe { (*v).asBits_ });
    embed::set_proxy_private(object_ptr(obj), BsValue::from_raw_bits(bits));
}

#[super::poison::engine_shim(failure = unsafe { (*out).asBits_ = value_bits::undefined() })]
pub unsafe extern "C" fn glue__GetProxyReservedSlot(obj: *mut JSObject, slot: u32, out: *mut JS::Value) {
    let o = object_ptr(obj);
    let bits = match bounds::reserved_slot(embed::num_reserved_slots(o), slot) {
        Some(i) => embed::reserved_slot(o, i as u32).as_raw_bits(),
        None => value_bits::undefined(),
    };
    unsafe { (*out).asBits_ = bits };
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__SetProxyReservedSlot(obj: *mut JSObject, slot: u32, v: *const JS::Value) {
    let o = object_ptr(obj);
    let i = bounds::reserved_slot(embed::num_reserved_slots(o), slot)
        .unwrap_or_else(|| panic!("SetProxyReservedSlot: slot {slot} out of range"));
    embed::set_reserved_slot(o, i as u32, BsValue::from_raw_bits(super::func::non_zero(unsafe { (*v).asBits_ })));
}

/// `js::SwappableProxyReservedSlots` (2) and `JSCLASS_IS_PROXY`, as jsglue.cpp's classes; no hooks of their own.
const fn proxy_class(name: &'static ::std::ffi::CStr) -> JSClass {
    JSClass {
        name: name.as_ptr(),
        flags: JSCLASS_IS_PROXY | (2 << JSCLASS_RESERVED_SLOTS_SHIFT),
        cOps: ::std::ptr::null(),
        spec: ::std::ptr::null(),
        ext: ::std::ptr::null(),
        oOps: ::std::ptr::null(),
    }
}

pub static PROXY_CLASS: JSClass = proxy_class(c"Proxy");
pub static WINDOW_PROXY_CLASS: JSClass = proxy_class(c"Proxy");

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__GetWindowProxyClass() -> *const JSClass {
    &WINDOW_PROXY_CLASS
}

unsafe fn new_proxy(
    cx: *mut JSContext,
    handler: *const c_void,
    private: u64,
    proto: *mut JSObject,
    class: *const JSClass,
) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let proto_h = if proto.is_null() { None } else { Some(BsValue::object(object_ptr(proto)).to_handle(bs).as_object()) };
    let class = if class.is_null() { &PROXY_CLASS as *const JSClass } else { class };
    let slots = bounds::reserved_slot_count(unsafe { (*class).flags });
    let mut flags = HostFlags::empty();
    // `ProxyObject::trace` / `finalize` call the handler's traps (jsglue.cpp): Servo's DOM proxies (HTMLDocument, Location,
    // DOMStringMap...) trace and drop their Rust object there. Without these flags the GC never asks, and everything only
    // the proxy's Rust object holds (a Document's module map, FontFaceSet, timeline...) is collected under Servo.
    if let Some(h) = handler_of_raw(handler) {
        if h.traps.trace.is_some() {
            flags |= HostFlags::HAS_TRACE;
        }
        if h.traps.finalize.is_some() {
            flags |= HostFlags::HAS_FINALIZE;
        }
        // A handler that provides the `call`/`construct` trap gives the proxy that internal method over its own
        // private: this is how a DOM object with a legacy caller (HTMLAllCollection, `document.all`) is callable.
        if h.traps.call.is_some() {
            flags |= HostFlags::CALLABLE;
        }
        if h.traps.construct.is_some() {
            flags |= HostFlags::CONSTRUCTOR;
        }
    }
    // Callable proxies (Servo's are not, but WrapperNew of a function must stay callable).
    if !private.eq(&0) && value_bits::is_traced(private) && api::is_callable(handle_of(bs, private)) {
        flags |= HostFlags::CALLABLE;
    }
    // The proxy's DOM class can carry the emulated-undefined flag, as SpiderMonkey lets a proxy class do.
    if unsafe { (*class).flags } & JSCLASS_EMULATES_UNDEFINED != 0 {
        flags |= HostFlags::IS_HTMLDDA;
    }
    let r = embed::new_host_object(bs, realm, class.cast(), flags, proto_h, slots, handler, handle_of(bs, private));
    let out = match r {
        Ok(o) => (*o).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e.into()) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    out
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__NewProxyObject(
    cx: *mut JSContext,
    handler: *const c_void,
    private: JS::HandleValue,
    proto: *mut JSObject,
    class: *const JSClass,
    _lazy_proto: bool,
) -> *mut JSObject {
    unsafe { new_proxy(cx, handler, (*private.ptr).asBits_, proto, class) }
}

/// A wrapper of `obj`: one realm per compartment and no cross-compartment wrappers are needed, so this is the proxy the
/// handler asks for, with `obj` as target.
#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__WrapperNew(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    handler: *const c_void,
    class: *const JSClass,
) -> *mut JSObject {
    let target = unsafe { *obj.ptr };
    let proto = match handler_of_raw(handler) {
        // A wrapper's prototype is the target's (Wrapper::getPrototype forwards); lazily, as SpiderMonkey's TaggedProto::LazyProto.
        _ => ::std::ptr::null_mut(),
    };
    unsafe { new_proxy(cx, handler, BsValue::object(object_ptr(target)).as_raw_bits(), proto, class) }
}

/// A live proxy's `trace` trap, from the GC (`cx::ServoHost::trace_host_object`).
pub unsafe fn trace_proxy(handler: *const c_void, obj: *mut JSObject, visitor: &mut dyn brimstone_core::runtime::embed::RootVisitor) {
    if let Some(trace) = handler_of_raw(handler).and_then(|h| h.traps.trace) {
        let mut shim = super::gc::ShimTracer::marking(visitor);
        unsafe { trace(shim.as_trc(), obj) };
    }
}

/// A dead proxy's `finalize` trap, from the GC (`cx::ServoHost::finalize_host_object`).
pub unsafe fn finalize_proxy(handler: *const c_void, gcx: *mut JS::GCContext, obj: *mut JSObject) {
    if let Some(finalize) = handler_of_raw(handler).and_then(|h| h.traps.finalize) {
        unsafe { finalize(gcx, obj) };
    }
}

fn handler_of_raw(h: *const c_void) -> Option<&'static Handler> {
    if h.is_null() { None } else { Some(unsafe { &*(h as *const Handler) }) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__NewWindowProxy(cx: *mut JSContext, obj: JS::HandleObject, handler: *const c_void) -> *mut JSObject {
    unsafe { glue__WrapperNew(cx, obj, handler, &WINDOW_PROXY_CLASS) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__IsWindowProxy(obj: *mut JSObject) -> bool {
    embed::host_class(object_ptr(obj)) == Some((&WINDOW_PROXY_CLASS as *const JSClass).cast())
}

/// The window behind a WindowProxy (its target), or the object itself.
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ToWindowIfWindowProxy(obj: *mut JSObject) -> *mut JSObject {
    if unsafe { js__IsWindowProxy(obj) } {
        let p = embed::proxy_private(object_ptr(obj));
        if p.is_object() {
            return p.as_object().as_ptr().cast();
        }
    }
    obj
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__SetWindowProxyClass(cx: *mut JSContext, class: *const JSClass) {
    unsafe { cx_of(cx) }.window_proxy_class = class;
}

/// `js::SetWindowProxy(cx, global, windowProxy)`: kept so `JS_GetWindowProxy`-style lookups and `this` at the top level can
/// answer the proxy for the global.
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__SetWindowProxy(cx: *mut JSContext, global: JS::HandleObject, proxy: JS::HandleObject) {
    let (g, p) = unsafe { (*global.ptr, *proxy.ptr) };
    unsafe { cx_of(cx) }.window_proxies.insert(g as usize, p as usize);
}

// ---- Calling traps ----

#[inline]
fn hobj(p: &*mut JSObject) -> JS::HandleObject {
    JS::Handle { _phantom_0: Default::default(), ptr: p }
}

#[inline]
fn hid(slot: *mut u64) -> JS::HandleId {
    JS::Handle { _phantom_0: Default::default(), ptr: slot.cast() }
}

#[inline]
fn hval(slot: *mut u64) -> JS::HandleValue {
    JS::Handle { _phantom_0: Default::default(), ptr: slot.cast() }
}

#[inline]
fn mval(slot: *mut u64) -> JS::MutableHandleValue {
    JS::MutableHandle { _phantom_0: Default::default(), ptr: slot.cast() }
}

/// The id Servo sees for a key (indices above 2^31 as strings: `jsid.rs`).
unsafe fn key_bits(bs: Bs, key: Handle<PropertyKey>) -> u64 {
    unsafe { super::property::id_out(bs, key) }
}

/// The result of a trap: true, or the pending exception as Brimstone's error.
unsafe fn trap_result(cx: *mut JSContext, bs: Bs, ok: bool) -> EvalResult<()> {
    if ok {
        return Ok(());
    }
    match unsafe { cx_of(cx) }.take_pending() {
        Some(bits) => Err(embed::throw_value(handle_of(bs, bits))),
        None => Err(super::uncatchable(bs)),
    }
}

/// `JS::ObjectOpResult` as it comes back from a trap: ok, or a failure code SpiderMonkey would turn into a TypeError in strict
/// code. Brimstone's internal methods return the boolean and throw in strict code themselves.
#[inline]
fn op_ok(r: &JS::ObjectOpResult) -> bool {
    r.code_ == 0
}

fn new_op_result() -> JS::ObjectOpResult {
    // `ObjectOpResult::Uninitialized`, as SpiderMonkey starts one.
    JS::ObjectOpResult { code_: usize::MAX }
}

/// Brimstone's descriptor from SpiderMonkey's.
pub unsafe fn desc_from_js(bs: Bs, d: &JS::PropertyDescriptor) -> api::PropertyDescriptor {
    let mut out = api::PropertyDescriptor {
        value: None,
        is_writable: None,
        is_enumerable: None,
        is_configurable: None,
        has_get: false,
        has_set: false,
        get: None,
        set: None,
    };
    if d.hasConfigurable_() {
        out.is_configurable = Some(d.configurable_());
    }
    if d.hasEnumerable_() {
        out.is_enumerable = Some(d.enumerable_());
    }
    if d.hasWritable_() {
        out.is_writable = Some(d.writable_());
    }
    if d.hasValue_() {
        out.value = Some(handle_of(bs, d.value_.asBits_));
    }
    if d.hasGetter_() {
        out.has_get = true;
        out.get = (!d.getter_.is_null()).then(|| BsValue::object(object_ptr(d.getter_)).to_handle(bs).as_object());
    }
    if d.hasSetter_() {
        out.has_set = true;
        out.set = (!d.setter_.is_null()).then(|| BsValue::object(object_ptr(d.setter_)).to_handle(bs).as_object());
    }
    out
}

/// SpiderMonkey's descriptor from Brimstone's.
pub unsafe fn desc_to_js(d: &api::PropertyDescriptor, out: &mut JS::PropertyDescriptor) {
    *out = unsafe { ::std::mem::zeroed() };
    if let Some(c) = d.is_configurable {
        out.set_hasConfigurable_(true);
        out.set_configurable_(c);
    }
    if let Some(e) = d.is_enumerable {
        out.set_hasEnumerable_(true);
        out.set_enumerable_(e);
    }
    if let Some(w) = d.is_writable {
        out.set_hasWritable_(true);
        out.set_writable_(w);
    }
    match d.value {
        Some(v) => {
            out.set_hasValue_(true);
            out.value_.asBits_ = v.as_raw_bits();
        },
        None => out.value_.asBits_ = value_bits::undefined(),
    }
    if d.has_get {
        out.set_hasGetter_(true);
        out.getter_ = d.get.map_or(::std::ptr::null_mut(), |g| (*g).as_ptr().cast());
    }
    if d.has_set {
        out.set_hasSetter_(true);
        out.setter_ = d.set.map_or(::std::ptr::null_mut(), |s| (*s).as_ptr().cast());
    }
}

/// A SpiderMonkey descriptor rooted for the duration of `f` (`Cx::desc_roots`, traced each GC).
///
/// `f` gets the descriptor as a raw pointer, the one the GC writes through: `f` hands it to a trap that may GC, so a
/// `&mut` to it must not be live across the trap. `cx` is reached only for the push and the pop (`f` re-enters the engine).
unsafe fn with_desc<R>(cx: *mut Cx, f: impl FnOnce(*mut JS::PropertyDescriptor) -> R) -> R {
    let mut d: Box<JS::PropertyDescriptor> = Box::new(unsafe { ::std::mem::zeroed() });
    d.value_.asBits_ = value_bits::undefined();
    let p: *mut JS::PropertyDescriptor = Box::into_raw(d);
    unsafe { (*cx).desc_roots.push(p) };
    let r = f(p);
    let popped = unsafe { (*cx).desc_roots.pop() };
    debug_assert_eq!(popped, Some(p));
    // SAFETY: `p` came from Box::into_raw above and is off the root list.
    unsafe { drop(Box::from_raw(p)) };
    r
}

// ---- Internal methods (ServoHost::proxy_* forwards here) ----

/// The proxy's target, for Forwarding and Wrapper handlers.
fn target_of(bs: Bs, proxy: Handle<ObjectValue>) -> Option<Handle<ObjectValue>> {
    let p = embed::proxy_private(*proxy);
    p.is_object().then(|| p.as_object().to_handle())
}

pub unsafe fn get_own_property(
    bs: Bs,
    handler: *const c_void,
    proxy: Handle<ObjectValue>,
    key: Handle<PropertyKey>,
) -> EvalResult<Option<api::PropertyDescriptor>> {
    let h = handler_of_raw(handler).expect("proxy without handler");
    let cx = current_ptr();
    let raw_cx: *mut JSContext = cx.cast();
    match h.traps.getOwnPropertyDescriptor {
        Some(trap) => {
            let obj: *mut JSObject = (*proxy).as_ptr().cast();
            let id = unsafe { key_bits(bs, key) };
            let r = unsafe {
                with_slots(cx, 1, |slots| {
                    *slots = id;
                    with_desc(cx, |d| {
                        let mut is_none = true;
                        let ok = trap(
                            raw_cx,
                            hobj(&obj),
                            hid(slots),
                            JS::MutableHandle { _phantom_0: Default::default(), ptr: d },
                            &mut is_none,
                        );
                        // The trap is done: no GC while this short borrow reads it (handles do not allocate on the heap).
                        (ok, if is_none { None } else { Some(desc_from_js(bs, &*d)) })
                    })
                })
            };
            let Some((ok, desc)) = r else { return brimstone_core::runtime::error::range_error(bs, "too much recursion") };
            unsafe { trap_result(raw_cx, bs, ok) }?;
            Ok(desc)
        },
        None => match target_of(bs, proxy) {
            Some(t) => api::get_own_property_descriptor(bs, t, key),
            None => Ok(None),
        },
    }
}

/// Traces an id vector a trap fills, while it is registered in `Cx::extra_roots`.
#[super::poison::engine_shim(gc)]
unsafe extern "C" fn trace_id_vec(trc: *mut JSTracer, data: *mut c_void) {
    let v = unsafe { &mut *(data as *mut Vec<u64>) };
    for bits in v.iter_mut() {
        unsafe { super::gc::trace_value_slot(trc, bits) };
    }
}

/// `[[DefineOwnProperty]]`: the `defineProperty` trap with an `ObjectOpResult`; without one, defines on the target
/// (the Base handler requires the trap, so a Base proxy without one and without a target refuses).
pub unsafe fn define_property(
    bs: Bs,
    handler: *const c_void,
    proxy: Handle<ObjectValue>,
    key: Handle<PropertyKey>,
    desc: api::PropertyDescriptor,
) -> EvalResult<bool> {
    let h = handler_of_raw(handler).expect("proxy without handler");
    let cx = current_ptr();
    let raw_cx: *mut JSContext = cx.cast();
    match h.traps.defineProperty {
        Some(trap) => {
            let obj: *mut JSObject = (*proxy).as_ptr().cast();
            let id = unsafe { key_bits(bs, key) };
            let r = unsafe {
                with_slots(cx, 1, |slots| {
                    *slots = id;
                    with_desc(cx, |d| {
                        desc_to_js(&desc, &mut *d);
                        let mut result = new_op_result();
                        let ok = trap(
                            raw_cx,
                            hobj(&obj),
                            hid(slots),
                            JS::Handle { _phantom_0: Default::default(), ptr: d },
                            &mut result,
                        );
                        (ok, result)
                    })
                })
            };
            let Some((ok, result)) = r else { return brimstone_core::runtime::error::range_error(bs, "too much recursion") };
            unsafe { trap_result(raw_cx, bs, ok) }?;
            Ok(op_ok(&result))
        },
        None => match target_of(bs, proxy) {
            Some(t) => api::define_property(bs, t, key, desc),
            None => Ok(false),
        },
    }
}

/// A key as `own_property_keys` returns it: an array index as its decimal string (the engine's convention), else the id value.
unsafe fn key_value(bs: Bs, bits: u64) -> EvalResult<Handle<BsValue>> {
    let key = unsafe { super::property::id_in(bs, bits) };
    let raw = key.to_raw_value();
    if raw.is_smi() {
        let s = api::string_from_utf8(bs, &raw.as_smi().to_string())?;
        Ok(s.as_value())
    } else {
        Ok(raw.to_handle(bs))
    }
}

/// `[[OwnPropertyKeys]]`: the `ownPropertyKeys` trap, result order kept, ids converted back with `id_in`.
pub unsafe fn own_keys(
    bs: Bs,
    handler: *const c_void,
    proxy: Handle<ObjectValue>,
) -> EvalResult<Vec<Handle<BsValue>>> {
    let h = handler_of_raw(handler).expect("proxy without handler");
    let cx = current_ptr();
    let raw_cx: *mut JSContext = cx.cast();
    match h.traps.ownPropertyKeys {
        Some(trap) => {
            let obj: *mut JSObject = (*proxy).as_ptr().cast();
            let mut vec = Box::new(Vec::<u64>::new());
            let data = (&mut *vec as *mut Vec<u64>).cast::<c_void>();
            unsafe { (*cx).extra_roots.push((Some(trace_id_vec), data)) };
            let ok = unsafe { trap(raw_cx, hobj(&obj), JS::MutableHandleIdVector { ptr: data }) };
            let roots = unsafe { &mut (*cx).extra_roots };
            if let Some(i) = roots.iter().position(|&(_, d)| d == data) {
                roots.remove(i);
            }
            unsafe { trap_result(raw_cx, bs, ok) }?;
            let mut out = Vec::with_capacity(vec.len());
            for &bits in vec.iter() {
                out.push(unsafe { key_value(bs, bits) }?);
            }
            Ok(out)
        },
        None => match target_of(bs, proxy) {
            Some(t) => api::own_property_keys(bs, t),
            None => Ok(Vec::new()),
        },
    }
}

/// `[[Delete]]`: the `delete_` trap with an `ObjectOpResult`; without one, deletes on the target.
pub unsafe fn delete(
    bs: Bs,
    handler: *const c_void,
    proxy: Handle<ObjectValue>,
    key: Handle<PropertyKey>,
) -> EvalResult<bool> {
    let h = handler_of_raw(handler).expect("proxy without handler");
    let cx = current_ptr();
    let raw_cx: *mut JSContext = cx.cast();
    match h.traps.delete_ {
        Some(trap) => {
            let obj: *mut JSObject = (*proxy).as_ptr().cast();
            let id = unsafe { key_bits(bs, key) };
            let r = unsafe {
                with_slots(cx, 1, |slots| {
                    *slots = id;
                    let mut result = new_op_result();
                    let ok = trap(raw_cx, hobj(&obj), hid(slots), &mut result);
                    (ok, result)
                })
            };
            let Some((ok, result)) = r else { return brimstone_core::runtime::error::range_error(bs, "too much recursion") };
            unsafe { trap_result(raw_cx, bs, ok) }?;
            Ok(op_ok(&result))
        },
        None => match target_of(bs, proxy) {
            Some(t) => api::delete_property(bs, t, key),
            None => Ok(false),
        },
    }
}

/// Reads a `MutableHandleObject` out-slot filled by a prototype trap: a nullable raw `JSObject*`.
/// Null (bits 0) is "no prototype"; the pointer bits of a non-null object are valid object-tagged `BsValue` bits.
fn proto_from_slot(bits: u64) -> Option<Handle<ObjectValue>> {
    if bits == 0 {
        return None;
    }
    // SAFETY: non-null, so a live `JSObject*` the trap stored (rooted by the caller's slot until here); the pointer-tagged bits are the object Value.
    let v = BsValue::from_raw_bits(bits);
    v.is_object().then(|| v.as_object().to_handle())
}

/// The Base handler's prototype: the `getPrototypeIfOrdinary` trap, else the object's own stored prototype.
unsafe fn base_prototype(
    bs: Bs,
    handler: *const c_void,
    proxy: Handle<ObjectValue>,
) -> EvalResult<Option<Handle<ObjectValue>>> {
    let h = handler_of_raw(handler).expect("proxy without handler");
    let cx = current_ptr();
    let raw_cx: *mut JSContext = cx.cast();
    match h.traps.getPrototypeIfOrdinary {
        Some(trap) => {
            let obj: *mut JSObject = (*proxy).as_ptr().cast();
            let r = unsafe {
                with_slots(cx, 1, |slots| {
                    // The out-param is a `MutableHandleObject`: a nullable raw `JSObject*`, not a Value. Null means "no prototype".
                    *slots = 0;
                    let mut is_ordinary = false;
                    let ok = trap(
                        raw_cx,
                        hobj(&obj),
                        &mut is_ordinary,
                        JS::MutableHandle { _phantom_0: Default::default(), ptr: slots.cast() },
                    );
                    (ok, is_ordinary, *slots)
                })
            };
            let Some((ok, is_ordinary, bits)) = r else { return brimstone_core::runtime::error::range_error(bs, "too much recursion") };
            unsafe { trap_result(raw_cx, bs, ok) }?;
            if is_ordinary {
                // As SpiderMonkey's `Proxy::getPrototypeIfOrdinary`: when ordinary, the (possibly null) result is the prototype.
                return Ok(proto_from_slot(bits));
            }
            Ok(proxy.prototype().map(|p| p.to_handle()))
        },
        None => Ok(proxy.prototype().map(|p| p.to_handle())),
    }
}

/// `[[HasProperty]]`: the `has` trap; else Base derives it from `hasOwn`/the own descriptor and the prototype,
/// Forwarding and Wrapper forward `has` to the target.
pub unsafe fn has(
    bs: Bs,
    handler: *const c_void,
    proxy: Handle<ObjectValue>,
    key: Handle<PropertyKey>,
) -> EvalResult<bool> {
    let h = handler_of_raw(handler).expect("proxy without handler");
    let cx = current_ptr();
    let raw_cx: *mut JSContext = cx.cast();
    if let Some(trap) = h.traps.has {
        let obj: *mut JSObject = (*proxy).as_ptr().cast();
        let id = unsafe { key_bits(bs, key) };
        let r = unsafe {
            with_slots(cx, 1, |slots| {
                *slots = id;
                let mut bp = false;
                let ok = trap(raw_cx, hobj(&obj), hid(slots), &mut bp);
                (ok, bp)
            })
        };
        let Some((ok, bp)) = r else { return brimstone_core::runtime::error::range_error(bs, "too much recursion") };
        unsafe { trap_result(raw_cx, bs, ok) }?;
        return Ok(bp);
    }
    match h.kind {
        HandlerKind::Base => {
            let bp = match h.traps.hasOwn {
                Some(trap) => {
                    let obj: *mut JSObject = (*proxy).as_ptr().cast();
                    let id = unsafe { key_bits(bs, key) };
                    let r = unsafe {
                        with_slots(cx, 1, |slots| {
                            *slots = id;
                            let mut bp = false;
                            let ok = trap(raw_cx, hobj(&obj), hid(slots), &mut bp);
                            (ok, bp)
                        })
                    };
                    let Some((ok, bp)) = r else { return brimstone_core::runtime::error::range_error(bs, "too much recursion") };
                    unsafe { trap_result(raw_cx, bs, ok) }?;
                    bp
                },
                None => unsafe { get_own_property(bs, handler, proxy, key) }?.is_some(),
            };
            if bp {
                return Ok(true);
            }
            match unsafe { base_prototype(bs, handler, proxy) }? {
                Some(proto) => api::has_property(bs, proto, key),
                None => Ok(false),
            }
        },
        _ => match target_of(bs, proxy) {
            Some(t) => api::has_property(bs, t, key),
            None => Ok(false),
        },
    }
}

/// `[[Get]]`: the `get` trap; else Base's `BaseProxyHandler::get` (own descriptor or the prototype chain, data -> value,
/// accessor -> getter called with the receiver), Forwarding and Wrapper get on the target with the same receiver.
pub unsafe fn get(
    bs: Bs,
    handler: *const c_void,
    proxy: Handle<ObjectValue>,
    key: Handle<PropertyKey>,
    receiver: Handle<BsValue>,
) -> EvalResult<Handle<BsValue>> {
    let h = handler_of_raw(handler).expect("proxy without handler");
    let cx = current_ptr();
    let raw_cx: *mut JSContext = cx.cast();
    if let Some(trap) = h.traps.get {
        let obj: *mut JSObject = (*proxy).as_ptr().cast();
        let id = unsafe { key_bits(bs, key) };
        let r = unsafe {
            with_slots(cx, 3, |slots| {
                *slots = id;
                *slots.add(1) = receiver.as_raw_bits();
                *slots.add(2) = value_bits::undefined();
                let ok = trap(raw_cx, hobj(&obj), hval(slots.add(1)), hid(slots), mval(slots.add(2)));
                (ok, *slots.add(2))
            })
        };
        let Some((ok, out)) = r else { return brimstone_core::runtime::error::range_error(bs, "too much recursion") };
        unsafe { trap_result(raw_cx, bs, ok) }?;
        return Ok(handle_of(bs, out));
    }
    match h.kind {
        HandlerKind::Base => {
            match unsafe { get_own_property(bs, handler, proxy, key) }? {
                Some(d) => {
                    if let Some(v) = d.value {
                        Ok(v)
                    } else if d.has_get {
                        match d.get {
                            Some(g) => {
                                let f = BsValue::object(*g).to_handle(bs);
                                embed::call(bs, f, receiver, &[])
                            },
                            None => Ok(bs.undefined()),
                        }
                    } else {
                        Ok(bs.undefined())
                    }
                },
                None => match unsafe { base_prototype(bs, handler, proxy) }? {
                    Some(proto) => proto.get(bs, key, receiver),
                    None => Ok(bs.undefined()),
                },
            }
        },
        _ => match target_of(bs, proxy) {
            Some(t) => t.get(bs, key, receiver),
            None => Ok(bs.undefined()),
        },
    }
}

/// `[[Set]]`: the `set` trap; else `BaseProxyHandler::set` (`js::SetPropertyIgnoringNamedGetter`: own descriptor, then the
/// prototype, then define on the receiver as OrdinarySet does), Forwarding and Wrapper set on the target.
pub unsafe fn set(
    bs: Bs,
    handler: *const c_void,
    proxy: Handle<ObjectValue>,
    key: Handle<PropertyKey>,
    value: Handle<BsValue>,
    receiver: Handle<BsValue>,
) -> EvalResult<bool> {
    let h = handler_of_raw(handler).expect("proxy without handler");
    let cx = current_ptr();
    let raw_cx: *mut JSContext = cx.cast();
    if let Some(trap) = h.traps.set {
        let obj: *mut JSObject = (*proxy).as_ptr().cast();
        let id = unsafe { key_bits(bs, key) };
        let r = unsafe {
            with_slots(cx, 3, |slots| {
                *slots = id;
                *slots.add(1) = value.as_raw_bits();
                *slots.add(2) = receiver.as_raw_bits();
                let mut result = new_op_result();
                let ok = trap(raw_cx, hobj(&obj), hid(slots), hval(slots.add(1)), hval(slots.add(2)), &mut result);
                (ok, result)
            })
        };
        let Some((ok, result)) = r else { return brimstone_core::runtime::error::range_error(bs, "too much recursion") };
        unsafe { trap_result(raw_cx, bs, ok) }?;
        return Ok(op_ok(&result));
    }
    match h.kind {
        HandlerKind::Base => {
            let own = match unsafe { get_own_property(bs, handler, proxy, key) }? {
                Some(d) => d,
                None => match unsafe { base_prototype(bs, handler, proxy) }? {
                    Some(mut proto) => return proto.set(bs, key, value, receiver),
                    None => api::PropertyDescriptor::data(
                        bs.undefined(),
                        api::PropertyFlags::empty().writable().enumerable().configurable(),
                    ),
                },
            };
            if own.value.is_some() {
                if own.is_writable == Some(false) {
                    return Ok(false);
                }
                if !receiver.is_object() {
                    return Ok(false);
                }
                let recv = receiver.as_object();
                let existing = api::get_own_property_descriptor(bs, recv, key)?;
                if let Some(ex) = &existing {
                    if ex.has_get || ex.has_set {
                        return Ok(false);
                    }
                    if ex.is_writable == Some(false) {
                        return Ok(false);
                    }
                }
                let d = match existing {
                    Some(_) => api::PropertyDescriptor::data_value_only(value),
                    None => api::PropertyDescriptor::data(
                        value,
                        api::PropertyFlags::empty().writable().enumerable().configurable(),
                    ),
                };
                api::define_property(bs, recv, key, d)
            } else {
                match own.set {
                    Some(s) => {
                        let f = BsValue::object(*s).to_handle(bs);
                        embed::call(bs, f, receiver, &[value])?;
                        Ok(true)
                    },
                    None => Ok(false),
                }
            }
        },
        _ => match target_of(bs, proxy) {
            Some(t) => api::set_property(bs, t, key, value, receiver),
            None => Ok(false),
        },
    }
}

/// `[[PreventExtensions]]`: the `preventExtensions` trap; without one, on the target (else the proxy itself).
pub unsafe fn prevent_extensions(
    bs: Bs,
    handler: *const c_void,
    proxy: Handle<ObjectValue>,
) -> EvalResult<bool> {
    let h = handler_of_raw(handler).expect("proxy without handler");
    let cx = current_ptr();
    let raw_cx: *mut JSContext = cx.cast();
    match h.traps.preventExtensions {
        Some(trap) => {
            let obj: *mut JSObject = (*proxy).as_ptr().cast();
            let mut result = new_op_result();
            let ok = unsafe { trap(raw_cx, hobj(&obj), &mut result) };
            unsafe { trap_result(raw_cx, bs, ok) }?;
            Ok(op_ok(&result))
        },
        None => match target_of(bs, proxy) {
            Some(t) => api::prevent_extensions(bs, t),
            None => {
                let mut p = proxy;
                p.ordinary_prevent_extensions(bs)
            },
        },
    }
}

/// `[[IsExtensible]]`: the `isExtensible` trap; without one, on the target (else the proxy's own shape).
pub unsafe fn is_extensible(
    bs: Bs,
    handler: *const c_void,
    proxy: Handle<ObjectValue>,
) -> EvalResult<bool> {
    let h = handler_of_raw(handler).expect("proxy without handler");
    let cx = current_ptr();
    let raw_cx: *mut JSContext = cx.cast();
    match h.traps.isExtensible {
        Some(trap) => {
            let obj: *mut JSObject = (*proxy).as_ptr().cast();
            let mut succeeded = false;
            let ok = unsafe { trap(raw_cx, hobj(&obj), &mut succeeded) };
            unsafe { trap_result(raw_cx, bs, ok) }?;
            Ok(succeeded)
        },
        None => match target_of(bs, proxy) {
            Some(t) => api::is_extensible(bs, t),
            None => Ok(proxy.shape_ptr().is_extensible()),
        },
    }
}

/// `[[GetPrototypeOf]]`: for Base, `getPrototypeIfOrdinary` first (the object's own stored prototype when ordinary);
/// the others use the `getPrototype` trap, else the target's prototype.
pub unsafe fn get_prototype(
    bs: Bs,
    handler: *const c_void,
    proxy: Handle<ObjectValue>,
) -> EvalResult<Option<Handle<ObjectValue>>> {
    let h = handler_of_raw(handler).expect("proxy without handler");
    let cx = current_ptr();
    let raw_cx: *mut JSContext = cx.cast();
    match h.kind {
        HandlerKind::Base => unsafe { base_prototype(bs, handler, proxy) },
        _ => match h.traps.getPrototype {
            Some(trap) => {
                let obj: *mut JSObject = (*proxy).as_ptr().cast();
                let r = unsafe {
                    with_slots(cx, 1, |slots| {
                        // Nullable raw `JSObject*` out-slot (see `proto_from_slot`).
                        *slots = 0;
                        let ok = trap(raw_cx, hobj(&obj), JS::MutableHandle { _phantom_0: Default::default(), ptr: slots.cast() });
                        (ok, *slots)
                    })
                };
                let Some((ok, bits)) = r else { return brimstone_core::runtime::error::range_error(bs, "too much recursion") };
                unsafe { trap_result(raw_cx, bs, ok) }?;
                Ok(proto_from_slot(bits))
            },
            None => match target_of(bs, proxy) {
                Some(t) => api::get_prototype_of(bs, t),
                None => Ok(proxy.prototype().map(|p| p.to_handle())),
            },
        },
    }
}

/// `[[SetPrototypeOf]]`: the `setPrototype` trap; Base without one refuses immutable prototypes per
/// `setImmutablePrototype`, the others set the target's prototype.
pub unsafe fn set_prototype(
    bs: Bs,
    handler: *const c_void,
    proxy: Handle<ObjectValue>,
    proto: Option<Handle<ObjectValue>>,
) -> EvalResult<bool> {
    let h = handler_of_raw(handler).expect("proxy without handler");
    let cx = current_ptr();
    let raw_cx: *mut JSContext = cx.cast();
    match h.traps.setPrototype {
        Some(trap) => {
            let obj: *mut JSObject = (*proxy).as_ptr().cast();
            let r = unsafe {
                with_slots(cx, 1, |slots| {
                    *slots = match proto {
                        Some(p) => BsValue::object(*p).as_raw_bits(),
                        // A `HandleObject` for "no prototype" holds a null pointer, never a Value's bits.
                        None => 0,
                    };
                    let mut result = new_op_result();
                    let ok = trap(raw_cx, hobj(&obj), hobj(&*(slots as *mut *mut JSObject)), &mut result);
                    (ok, result)
                })
            };
            let Some((ok, result)) = r else { return brimstone_core::runtime::error::range_error(bs, "too much recursion") };
            unsafe { trap_result(raw_cx, bs, ok) }?;
            Ok(op_ok(&result))
        },
        None => match h.kind {
            HandlerKind::Base => match h.traps.setImmutablePrototype {
                Some(trap) => {
                    let obj: *mut JSObject = (*proxy).as_ptr().cast();
                    let mut succeeded = false;
                    let ok = unsafe { trap(raw_cx, hobj(&obj), &mut succeeded) };
                    unsafe { trap_result(raw_cx, bs, ok) }?;
                    Ok(succeeded)
                },
                None => Ok(false),
            },
            _ => match target_of(bs, proxy) {
                Some(t) => api::set_prototype_of(bs, t, proto),
                None => Ok(false),
            },
        },
    }
}

/// `[[Call]]`/`[[Construct]]` of a callable proxy: the `call`/`construct` trap over a `JS::CallArgs` laid out on a native
/// frame as `func::call_native` does; without one, call/construct the target.
pub unsafe fn call_host_object(
    bs: Bs,
    handler: *const c_void,
    proxy: Handle<ObjectValue>,
    this: Handle<BsValue>,
    arguments: &[Handle<BsValue>],
    new_target: Option<Handle<ObjectValue>>,
) -> EvalResult<Handle<BsValue>> {
    let h = handler_of_raw(handler).expect("proxy without handler");
    let constructing = new_target.is_some();
    let trap = if constructing { h.traps.construct } else { h.traps.call };
    let cx = current_ptr();
    let raw_cx: *mut JSContext = cx.cast();
    match trap {
        Some(trap) => {
            let argc = arguments.len() as u32;
            let Some(len) = bounds::vp_len(argc + constructing as u32, 0) else {
                return brimstone_core::runtime::error::range_error(bs, "too many arguments");
            };
            let obj: *mut JSObject = (*proxy).as_ptr().cast();
            let r = unsafe {
                with_slots(cx, len, |vp| {
                    *vp = BsValue::object(*proxy).as_raw_bits();
                    // As in `call_class_hook`: SpiderMonkey's `CallArgs::is_constructing` reads the magic in the this slot,
                    // so a construct trap has to see it there (the bitfield below is what the C++ class really carries).
                    *vp.add(1) = if constructing { value_bits::empty() } else { this.as_raw_bits() };
                    for (i, a) in arguments.iter().enumerate() {
                        *vp.add(2 + i) = a.as_raw_bits();
                    }
                    if let Some(nt) = new_target {
                        *vp.add(2 + argc as usize) = BsValue::object(*nt).as_raw_bits();
                    }
                    let args = JS::CallArgs {
                        argv_: vp.add(2).cast(),
                        argc_: argc,
                        _bitfield_align_1: [],
                        _bitfield_1: JS::CallArgs::new_bitfield_1(constructing, false),
                        __bindgen_padding_0: [0; 3],
                    };
                    let ok = trap(raw_cx, hobj(&obj), &args);
                    (ok, *vp)
                })
            };
            let Some((ok, rval)) = r else { return brimstone_core::runtime::error::range_error(bs, "too much recursion") };
            unsafe { trap_result(raw_cx, bs, ok) }?;
            Ok(handle_of(bs, rval))
        },
        None => match target_of(bs, proxy) {
            Some(t) => {
                let f = BsValue::object(*t).to_handle(bs);
                match new_target {
                    Some(nt) => Ok(BsValue::object(*embed::construct(bs, f, arguments, Some(nt))?).to_handle(bs)),
                    None => embed::call(bs, f, this, arguments),
                }
            },
            None => brimstone_core::runtime::error::type_error(bs, "proxy is not callable"),
        },
    }
}

// SpiderMonkey's constant proxy class parts (`js::ProxyClassOps` & co.), which Servo puts in its own proxy classes
// (`window_named_properties.rs`). Their hooks are SpiderMonkey's internals; here a proxy's behaviour comes from its handler,
// so they are empty, and immutable as in C++ (Servo takes `&ProxyClassOps`).
pub static js__ProxyClassOps: JSClassOps = unsafe { ::std::mem::zeroed() };
pub static js__ProxyClassExtension: js::ClassExtension = unsafe { ::std::mem::zeroed() };
pub static js__ProxyObjectOps: js::ObjectOps = unsafe { ::std::mem::zeroed() };
