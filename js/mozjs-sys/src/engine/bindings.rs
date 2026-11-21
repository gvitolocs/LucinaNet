/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! What Servo's binding layer calls while it builds interface objects, prototypes, reflectors and DOM proxies
//! (`components/script_bindings/{interface,wrap,proxyhandler,settings_stack,utils}.rs`). The original is `jsapi.cpp`,
//! `jsfriendapi.cpp`, `js/src/proxy/BaseProxyHandler.cpp` (`SetPropertyIgnoringNamedGetter`) and mozjs_sys's `jsglue.cpp`.

use brimstone_core::runtime::embed::{self, api};
use brimstone_core::runtime::gc::HandleScope;
use brimstone_core::runtime::{PropertyFlags, Value as BsValue};

use super::cx::cx_of;
use super::realm::object_ptr;
use super::util::{fail, handle_of};
use crate::boundary::value_bits;
use crate::jsapi::*;

/// `ObjectOpResult` failure code. SpiderMonkey stores a JSMSG number; ours are reported through `RUST_js_GetErrorMessage`,
/// which gives one generic message, so any non-zero code is a failure with the same text.
const OP_FAILED: usize = 1;

unsafe fn obj_h(cx: *mut JSContext, o: *mut JSObject) -> brimstone_core::runtime::Handle<api::ObjectValue> {
    let bs = unsafe { cx_of(cx) }.bs;
    BsValue::object(object_ptr(o)).to_handle(bs).as_object()
}

/// `ctor.prototype = proto` (non-writable, non-enumerable, non-configurable) and `proto.constructor = ctor` (writable,
/// non-enumerable, configurable), as `LinkConstructorAndPrototype` in `js/src/vm/GlobalObject.cpp`.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_LinkConstructorAndPrototype(
    cx: *mut JSContext,
    ctor: JS::Handle<*mut JSObject>,
    proto: JS::Handle<*mut JSObject>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let (ct, pr) = unsafe { (obj_h(cx, *ctor.ptr), obj_h(cx, *proto.ptr)) };
    let r = (|| {
        let k = api::property_key_from_string(bs, "prototype")?;
        let d = api::PropertyDescriptor::data(pr.as_value(), PropertyFlags::empty());
        api::define_property_or_throw(bs, ct, k, d)?;
        let k = api::property_key_from_string(bs, "constructor")?;
        let d = api::PropertyDescriptor::data(ct.as_value(), PropertyFlags::IS_WRITABLE | PropertyFlags::IS_CONFIGURABLE);
        api::define_property_or_throw(bs, pr, k, d)
    })();
    let ok = match r {
        Ok(_) => true,
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

/// Allocation metadata is not recorded, so this is `JS_NewObjectWithGivenProto`.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewObjectWithoutMetadata(
    cx: *mut JSContext,
    clasp: *const JSClass,
    proto: JS::Handle<*mut JSObject>,
) -> *mut JSObject {
    unsafe { super::object::JS_NewObjectWithGivenProto(cx, clasp, proto) }
}

/// SpiderMonkey marks the object so a later [[SetPrototypeOf]] to another value fails (the Window and its prototypes are
/// "immutable prototype exotic objects"). Brimstone has no such flag yet: this reports success and does not enforce it, so
/// `Object.setPrototypeOf(window, x)` is not refused (gap, fork follow-up).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetImmutablePrototype(_cx: *mut JSContext, _obj: JS::HandleObject, succeeded: *mut bool) -> bool {
    unsafe { *succeeded = true };
    true
}

/// Copies `src`'s own properties to `dst`, as SpiderMonkey's shape-and-slot copy gives the same own properties.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_InitializePropertiesFromCompatibleNativeObject(
    cx: *mut JSContext,
    dst: JS::HandleObject,
    src: JS::HandleObject,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let (d, s) = unsafe { (obj_h(cx, *dst.ptr), obj_h(cx, *src.ptr)) };
    let r = (|| {
        for k in api::own_property_keys(bs, s)? {
            let key = api::to_property_key(bs, k)?;
            if let Some(desc) = api::get_own_property_descriptor(bs, s, key)? {
                api::define_property_or_throw(bs, d, key, desc)?;
            }
        }
        Ok(())
    })();
    let ok = match r {
        Ok(()) => true,
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

/// One compartment per context: the callback runs once. The compartment pointer is the context (`JS::Compartment*` is opaque
/// to Servo and only passed back to compartment queries, which answer for all).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_IterateCompartments(
    cx: *mut JSContext,
    data: *mut ::std::ffi::c_void,
    callback: JSIterateCompartmentCallback,
) {
    if let Some(cb) = callback {
        unsafe { cb(cx, data, cx.cast()) };
    }
}

/// Brimstone defines the standard classes on a global when it creates it, so there is nothing still unresolved to list.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewEnumerateStandardClasses(
    _cx: *mut JSContext,
    _obj: JS::HandleObject,
    _properties: JS::MutableHandleIdVector,
    _enumerable_only: bool,
) -> bool {
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__NewProxyObject(
    cx: *mut JSContext,
    handler: *const js::BaseProxyHandler,
    private: JS::HandleValue,
    proto: *mut JSObject,
    options: *const js::ProxyOptions,
) -> *mut JSObject {
    let class = if options.is_null() { ::std::ptr::null() } else { unsafe { (*options).clasp_ } };
    let lazy = !options.is_null() && unsafe { (*options).lazyProto_ };
    unsafe { super::proxy::glue__NewProxyObject(cx, handler.cast(), private, proto, class, lazy) }
}

/// The prototype stored in the object's shape, with no [[GetPrototypeOf]] dispatch: Servo's DOM proxies call this from their own
/// getPrototypeIfOrdinary trap, so going through the proxy would recurse (it did: a stack overflow on example.com).
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__GetStaticPrototype(obj: *mut JSObject) -> *mut JSObject {
    object_ptr(obj).prototype().map_or(::std::ptr::null_mut(), |p| p.as_ptr().cast())
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__GetObjectProto(cx: *mut JSContext, obj: JS::HandleObject, proto: JS::MutableHandleObject) -> bool {
    unsafe { *proto.ptr = js__GetStaticPrototype(*obj.ptr) };
    let _ = cx;
    true
}

fn attrs_flags(d: &mut JS::PropertyDescriptor, attrs: u32) {
    d.set_hasConfigurable_(true);
    d.set_configurable_(attrs & JSPROP_PERMANENT as u32 == 0);
    d.set_hasEnumerable_(true);
    d.set_enumerable_(attrs & JSPROP_ENUMERATE as u32 != 0);
}

/// A data descriptor from `JSPROP_*` attributes, as jsglue.cpp's `SetDataPropertyDescriptor`.
#[super::poison::engine_shim(failure = unsafe { *desc.ptr = ::std::mem::zeroed() })]
pub unsafe extern "C" fn glue__SetDataPropertyDescriptor(desc: JS::MutableHandle<JS::PropertyDescriptor>, value: JS::HandleValue, attrs: u32) {
    let d = unsafe { &mut *desc.ptr };
    *d = unsafe { ::std::mem::zeroed() };
    attrs_flags(d, attrs);
    d.set_hasWritable_(true);
    d.set_writable_(attrs & JSPROP_READONLY as u32 == 0);
    d.set_hasValue_(true);
    d.value_.asBits_ = unsafe { (*value.ptr).asBits_ };
}

#[super::poison::engine_shim(failure = unsafe { *desc.ptr = ::std::mem::zeroed() })]
pub unsafe extern "C" fn glue__SetAccessorPropertyDescriptor(
    desc: JS::MutableHandle<JS::PropertyDescriptor>,
    getter: JS::HandleObject,
    setter: JS::HandleObject,
    attrs: u32,
) {
    let d = unsafe { &mut *desc.ptr };
    *d = unsafe { ::std::mem::zeroed() };
    attrs_flags(d, attrs);
    d.set_hasGetter_(true);
    d.getter_ = unsafe { *getter.ptr };
    d.set_hasSetter_(true);
    d.setter_ = unsafe { *setter.ptr };
    d.value_.asBits_ = value_bits::undefined();
}

/// OrdinarySetWithOwnDescriptor with the descriptor a DOM proxy found (or none), skipping its named getter: SpiderMonkey's
/// `js::SetPropertyIgnoringNamedGetter` (BaseProxyHandler.cpp), step by step.
#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__SetPropertyIgnoringNamedGetter(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    id: JS::HandleId,
    v: JS::HandleValue,
    receiver: JS::HandleValue,
    own_desc: *const JS::Handle<JS::PropertyDescriptor>,
    result: *mut JS::ObjectOpResult,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { super::property::id_in(bs, (*id.ptr).asBits_ as u64) };
    let value = handle_of(bs, unsafe { (*v.ptr).asBits_ });
    let recv = handle_of(bs, unsafe { (*receiver.ptr).asBits_ });
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let r = (|| -> Result<bool, brimstone_core::runtime::eval_result::EvalError> {
        // Step 4: no own property: continue on the prototype, or behave as a writable data property.
        let desc = if own_desc.is_null() {
            match api::get_prototype_of(bs, o)? {
                Some(proto) => return api::set_property(bs, proto, key, value, recv),
                None => None,
            }
        } else {
            Some(unsafe { super::proxy::desc_from_js(bs, &*(*own_desc).ptr) })
        };
        let is_accessor = desc.as_ref().is_some_and(|d| d.has_get || d.has_set);
        if !is_accessor {
            // Step 5.
            if desc.as_ref().is_some_and(|d| d.is_writable == Some(false)) {
                return Ok(false);
            }
            if !recv.is_object() {
                return Ok(false);
            }
            let ro = recv.as_object();
            match api::get_own_property_descriptor(bs, ro, key)? {
                Some(existing) => {
                    if existing.has_get || existing.has_set || existing.is_writable == Some(false) {
                        return Ok(false);
                    }
                    let d = api::PropertyDescriptor {
                        value: Some(value),
                        is_writable: None,
                        is_enumerable: None,
                        is_configurable: None,
                        has_get: false,
                        has_set: false,
                        get: None,
                        set: None,
                    };
                    api::define_property_or_throw(bs, ro, key, d).map(|_| true)
                },
                None => api::create_data_property(bs, ro, key, value),
            }
        } else {
            // Step 6: the setter, called with the receiver.
            match desc.and_then(|d| d.set) {
                Some(setter) => embed::call(bs, setter.as_value(), recv, &[value]).map(|_| true),
                None => Ok(false),
            }
        }
    })();
    let ok = match r {
        Ok(done) => {
            unsafe { (*result).code_ = if done { 0 } else { OP_FAILED } };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

// ---- The scripted caller (settings_stack.rs) ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__HideScriptedCaller(cx: *mut JSContext) {
    unsafe { cx_of(cx) }.hidden_scripted_caller += 1;
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__UnhideScriptedCaller(cx: *mut JSContext) {
    unsafe { cx_of(cx) }.hidden_scripted_caller = unsafe { cx_of(cx) }.hidden_scripted_caller.saturating_sub(1);
}

/// SpiderMonkey answers the global of the innermost script frame; here the current realm's global, which is the same while
/// script calls Servo directly (natives run in their caller's realm unless Servo enters another). Null when hidden.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetScriptedCallerGlobal(cx: *mut JSContext) -> *mut JSObject {
    if unsafe { cx_of(cx) }.hidden_scripted_caller > 0 {
        return ::std::ptr::null_mut();
    }
    unsafe { cx_of(cx) }.rooting.realm_.cast()
}

/// The innermost scripted frame's file, line and column, as jsglue.cpp's `DescribeScriptedCaller` over
/// `JS::DescribeScriptedCaller`: false with nothing pending when no script is running or the caller is hidden. The file name
/// is copied as `strncpy` does (no terminator when it fills the buffer); the column is 0-based, as the glue converts it.
#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__DescribeScriptedCaller(
    cx: *mut JSContext,
    buffer: *mut ::std::os::raw::c_char,
    buflen: usize,
    line: *mut u32,
    col: *mut u32,
) -> bool {
    let c = unsafe { cx_of(cx) };
    if c.hidden_scripted_caller > 0 {
        return false;
    }
    let frames = match embed::capture_stack(c.bs, 16) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let Some(frame) = frames.into_iter().find(|f| !f.is_native && f.file_name.is_some()) else {
        return false;
    };
    let name = frame.file_name.unwrap_or_default();
    if !buffer.is_null() && buflen > 0 {
        let bytes = name.as_bytes();
        let n = bytes.len().min(buflen);
        unsafe {
            ::std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast::<u8>(), n);
            if n < buflen {
                ::std::ptr::write_bytes(buffer.add(n), 0, buflen - n);
            }
        }
    }
    unsafe {
        if !line.is_null() {
            *line = frame.line.unwrap_or(0);
        }
        if !col.is_null() {
            *col = frame.column.unwrap_or(1).saturating_sub(1);
        }
    }
    true
}

/// WeakRef targets kept alive until the end of the current job: Brimstone clears its kept set at each job's end itself.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ClearKeptObjects(_cx: *mut JSContext) {}

/// Memory for Servo's about:memory: the engine's own numbers; `gs` (sizes of DOM objects) is not walked yet.
#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__CollectServoSizes(cx: *mut JSContext, sizes: *mut JS::ServoSizes, _gs: crate::glue::GetSize) -> bool {
    let s = embed::heap_stats(unsafe { cx_of(cx) }.bs);
    unsafe {
        (*sizes).gcHeapUsed += s.semispace_used_bytes + s.host_space_used_bytes;
        (*sizes).gcHeapUnused += s.semispace_capacity_bytes.saturating_sub(s.semispace_used_bytes);
        (*sizes).gcHeapAdmin += s.permanent_bytes;
    }
    true
}

/// One generic format for every SpiderMonkey message number Servo reports through (`js_GetErrorMessage`).
struct SyncFormat(JSErrorFormatString);
// SAFETY: the pointers are to static C strings, never written.
unsafe impl Sync for SyncFormat {}
static GENERIC_ERROR: SyncFormat = SyncFormat(JSErrorFormatString {
    name: c"JSMSG".as_ptr(),
    format: c"{0}".as_ptr(),
    argCount: 1,
    exnType: JSExnType::JSEXN_TYPEERR as i16,
});

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__RUST_js_GetErrorMessage(_user: *mut ::std::ffi::c_void, _number: u32) -> *const JSErrorFormatString {
    &GENERIC_ERROR.0
}
