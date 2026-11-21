/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Properties: define/get/set/has/delete, descriptors, enumeration, prototypes, extensibility, ids and atoms.
//! The original is `js/src/jsapi.cpp` (`JS_DefineProperty` & co.) and `js/src/vm/JSObject.cpp` in mozjs_sys 153.6.0-1.
//!
//! Ids cross the seam as Brimstone `PropertyKey` bits, with one conversion each way (see `boundary::id_bits`): a string id
//! whose text is a canonical array index becomes the index key on the way in, and an index key at or above 2^31 becomes a
//! string id on the way out.

use ::std::collections::HashSet;
use ::std::ffi::c_void;
use ::std::os::raw::c_char;

use brimstone_core::runtime::embed::{self, api};
use brimstone_core::runtime::eval_result::EvalError;
use brimstone_core::runtime::gc::{HandleScope, HeapPtr};
use brimstone_core::runtime::{Context as Bs, Handle, PropertyKey, Value as BsValue};

use super::cx::cx_of;
use super::func::non_zero;
use super::realm::object_ptr;
use super::util::{c_str, fail, finish};
use crate::boundary::{id_bits, value_bits};
use crate::jsapi::*;

// ---- Ids ----

/// A `jsid` Servo hands us, as a Brimstone property key. A string id whose text is a canonical array index ("0", "17", ...
/// up to 4294967294) becomes the index key; everything else keeps its bits.
pub unsafe fn id_in(bs: Bs, bits: u64) -> Handle<PropertyKey> {
    let bits = non_zero(bits);
    if id_bits::is_gcthing(bits) {
        let v = BsValue::from_raw_bits(bits);
        if v.is_string() {
            let h = v.to_handle(bs).as_string();
            if let Ok(flat) = h.flatten_to_handle() {
                // SAFETY: the index is parsed from the chars at once (after the flatten); nothing in the match allocates.
                let index = match unsafe { super::string::flat_chars(*flat) } {
                    api::StringChars::Latin1(bytes) => id_bits::parse_index_latin1(bytes),
                    api::StringChars::TwoByte(units) => id_bits::parse_index_utf16(units),
                };
                if let Some(i) = index {
                    if let Ok(k) = api::property_key_from_index(bs, i) {
                        return k;
                    }
                }
                if let Ok(k) = PropertyKey::string_handle(bs, flat.as_string()) {
                    return k;
                }
            }
        }
    }
    PropertyKey::from_raw_value(BsValue::from_raw_bits(bits)).to_handle(bs)
}

/// A Brimstone property key as a `jsid` for Servo. An index at or above 2^31 (`id_bits::is_high_index`) becomes a string id
/// (the index as decimal text, interned); everything else keeps its bits.
pub unsafe fn id_out(bs: Bs, key: Handle<PropertyKey>) -> u64 {
    let bits = key.to_raw_value().as_raw_bits();
    if id_bits::is_high_index(bits) {
        let i = id_bits::to_index(bits);
        let mut bs = bs;
        // property_key_from_string would normalize the decimal text back to an index key;
        // intern it as a plain string instead.
        if let Ok(s) = bs.alloc_string(&i.to_string()) {
            if let Ok(k) = PropertyKey::string_not_array_index_handle(bs, s) {
                return k.to_raw_value().as_raw_bits();
            }
        }
    }
    bits
}

/// The bits of a `HandleId`.
#[inline]
pub unsafe fn id_bits_of(id: JS::HandleId) -> u64 {
    non_zero(unsafe { (*id.ptr).asBits_ } as u64)
}

// ---- Shared shapes ----

#[inline]
fn bs_of(cx: *mut JSContext) -> Bs {
    unsafe { cx_of(cx) }.bs
}

/// A failed operation with a TypeError, as SpiderMonkey reports plain failures.
pub unsafe fn fail_type(cx: *mut JSContext, msg: &str) -> bool {
    match brimstone_core::runtime::error::type_error::<bool>(bs_of(cx), msg) {
        Err(e) => unsafe { fail(cx, e) },
        Ok(_) => false,
    }
}

#[inline]
unsafe fn obj_h(cx: *mut JSContext, obj: JS::Handle<*mut JSObject>) -> Handle<api::ObjectValue> {
    BsValue::object(object_ptr(unsafe { *obj.ptr })).to_handle(bs_of(cx)).as_object()
}

#[inline]
unsafe fn value_h(cx: *mut JSContext, v: JS::HandleValue) -> Handle<BsValue> {
    super::util::handle_of(bs_of(cx), unsafe { (*v.ptr).asBits_ })
}

/// A Latin-1 name as a property key (array-index spellings become index keys, as in SpiderMonkey).
pub unsafe fn name_key(cx: *mut JSContext, name: *const c_char) -> Result<Handle<PropertyKey>, EvalError> {
    let s = unsafe { c_str(name) };
    api::property_key_from_string(bs_of(cx), &s)
}

/// The key a `JSPropertySpec`/`JSFunctionSpec` name stands for. SpiderMonkey packs well-known symbols into the name pointer as
/// `code + 1` (`JS::PropertySpecNameIsSymbol`, js/public/Symbol.h: `name != 0 && name - 1 < WellKnownSymbolLimit`), so a small
/// integer here is `@@toStringTag` and friends, not a C string.
pub unsafe fn spec_name_key(cx: *mut JSContext, name: *const c_char) -> Result<Handle<PropertyKey>, EvalError> {
    let bits = name as usize;
    if bits != 0 && bits - 1 < JS::SymbolCode::Limit as usize {
        use api::WellKnownSymbol as W;
        // SpiderMonkey's SymbolCode order; Brimstone has no dispose / asyncDispose yet.
        const CODES: [Option<W>; 15] = [
            Some(W::IsConcatSpreadable), Some(W::Iterator), Some(W::Match), Some(W::Replace), Some(W::Search),
            Some(W::Species), Some(W::HasInstance), Some(W::Split), Some(W::ToPrimitive), Some(W::ToStringTag),
            Some(W::Unscopables), Some(W::AsyncIterator), Some(W::MatchAll), None, None,
        ];
        let bs = bs_of(cx);
        return match CODES[bits - 1] {
            Some(w) => Ok(PropertyKey::symbol(api::well_known_symbol(bs, w))),
            None => {
                let realm = brimstone_core::runtime::embed::current_realm(bs);
                let e = api::new_type_error(bs, realm, "Symbol.dispose and Symbol.asyncDispose are not supported yet")?;
                Err(brimstone_core::runtime::embed::throw_value(e.as_value()))
            },
        };
    }
    unsafe { name_key(cx, name) }
}

/// A two-byte name as a property key.
pub unsafe fn uc_name_key(cx: *mut JSContext, name: *const u16, namelen: usize) -> Result<Handle<PropertyKey>, EvalError> {
    let bs = bs_of(cx);
    let units = unsafe { ::std::slice::from_raw_parts(name, namelen) };
    let s = api::string_from_code_units(bs, units)?;
    Ok(PropertyKey::string_handle(bs, s.as_string())?)
}

/// `JSPROP_*` attributes as Brimstone property flags: absent `JSPROP_READONLY` means writable, absent
/// `JSPROP_PERMANENT` means configurable, `JSPROP_ENUMERATE` means enumerable.
pub fn attrs_to_flags(attrs: ::std::os::raw::c_uint) -> api::PropertyFlags {
    let mut f = api::PropertyFlags::empty();
    if attrs & JSPROP_READONLY as u32 == 0 {
        f = f.writable();
    }
    if attrs & JSPROP_ENUMERATE as u32 != 0 {
        f = f.enumerable();
    }
    if attrs & JSPROP_PERMANENT as u32 == 0 {
        f = f.configurable();
    }
    f
}

/// A data descriptor from a value and attributes.
fn data_desc(value: Handle<BsValue>, attrs: ::std::os::raw::c_uint) -> api::PropertyDescriptor {
    api::PropertyDescriptor::data(value, attrs_to_flags(attrs))
}

/// An accessor descriptor from getter/setter objects and attributes.
unsafe fn accessor_desc(
    cx: *mut JSContext,
    getter: *mut JSObject,
    setter: *mut JSObject,
    attrs: ::std::os::raw::c_uint,
) -> api::PropertyDescriptor {
    let bs = bs_of(cx);
    let g = if getter.is_null() { None } else { Some(BsValue::object(object_ptr(getter)).to_handle(bs).as_object()) };
    let s = if setter.is_null() { None } else { Some(BsValue::object(object_ptr(setter)).to_handle(bs).as_object()) };
    api::PropertyDescriptor::accessor(g, s, attrs_to_flags(attrs))
}

/// An accessor descriptor from getter/setter handles.
fn accessor_desc_h(
    g: Option<Handle<api::ObjectValue>>,
    s: Option<Handle<api::ObjectValue>>,
    attrs: ::std::os::raw::c_uint,
) -> api::PropertyDescriptor {
    api::PropertyDescriptor::accessor(g, s, attrs_to_flags(attrs))
}

/// A `JSNative` wrapped as a function object, for the getter/setter-native overloads. A handle, not a raw pointer: the setter's
/// function is allocated after the getter's, and that allocation can collect and move the getter (a raw pointer held across
/// it built accessors with a stale getter: the GitHub/BBC crash under GC stress, caught by LUCINANET_JS_GC_VERIFY).
unsafe fn native_obj(cx: *mut JSContext, native: JSNative, info: *const JSJitInfo) -> Option<Handle<api::ObjectValue>> {
    let native = native?;
    let bs = unsafe { cx_of(cx) }.bs;
    let realm = embed::current_realm(bs);
    embed::new_native_function_with_jit_info(bs, realm, native as *const c_void, info.cast(), 0, None, false, 0).ok()
}

/// `[[DefineOwnProperty]]`. With `result`, a plain failure is reported through its code and the call still returns true
/// (SpiderMonkey only returns false when an exception is pending); without it, a failure throws.
unsafe fn define_impl(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    key: Handle<PropertyKey>,
    desc: api::PropertyDescriptor,
    result: *mut JS::ObjectOpResult,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let mut o = unsafe { obj_h(cx, obj) };
    let r = o.define_own_property(bs, key, desc);
    scope.exit();
    match r {
        Ok(true) => {
            if !result.is_null() {
                unsafe { (*result).code_ = JS::ObjectOpResult_SpecialCodes::OkCode as usize };
            }
            true
        },
        Ok(false) => {
            if result.is_null() {
                unsafe { fail_type(cx, "cannot define property") }
            } else {
                unsafe { (*result).code_ = 1 };
                true
            }
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

/// A `JS::PropertyDescriptor` as Brimstone's.
unsafe fn desc_from_js(cx: *mut JSContext, desc: JS::Handle<JS::PropertyDescriptor>) -> api::PropertyDescriptor {
    let d = unsafe { &*desc.ptr };
    let bs = bs_of(cx);
    let mut out = api::PropertyDescriptor::default();
    if d.hasWritable_() {
        out.is_writable = Some(d.writable_());
    }
    if d.hasEnumerable_() {
        out.is_enumerable = Some(d.enumerable_());
    }
    if d.hasConfigurable_() {
        out.is_configurable = Some(d.configurable_());
    }
    if d.hasGetter_() {
        out.has_get = true;
        out.get = if d.getter_.is_null() { None } else { Some(BsValue::object(object_ptr(d.getter_)).to_handle(bs).as_object()) };
    }
    if d.hasSetter_() {
        out.has_set = true;
        out.set = if d.setter_.is_null() { None } else { Some(BsValue::object(object_ptr(d.setter_)).to_handle(bs).as_object()) };
    }
    if d.hasValue_() {
        out.value = Some(super::util::handle_of(bs, d.value_.asBits_));
    }
    out
}

/// Write a Brimstone descriptor into a `JS::PropertyDescriptor`, exactly as SpiderMonkey fills it.
unsafe fn desc_to_js(desc: *mut JS::PropertyDescriptor, d: api::PropertyDescriptor) {
    let d_ = unsafe { &mut *desc };
    d_.set_hasWritable_(d.is_writable.is_some());
    d_.set_writable_(d.is_writable.unwrap_or(false));
    d_.set_hasEnumerable_(d.is_enumerable.is_some());
    d_.set_enumerable_(d.is_enumerable.unwrap_or(false));
    d_.set_hasConfigurable_(d.is_configurable.is_some());
    d_.set_configurable_(d.is_configurable.unwrap_or(false));
    d_.set_hasGetter_(d.has_get);
    
    d_.getter_ = d.get.map(|o| (*o).as_ptr().cast()).unwrap_or(::std::ptr::null_mut());
    d_.set_hasSetter_(d.has_set);
    
    d_.setter_ = d.set.map(|o| (*o).as_ptr().cast()).unwrap_or(::std::ptr::null_mut());
    d_.set_hasValue_(d.value.is_some());
    d_.value_ = JS::Value { asBits_: d.value.map(|v| v.as_raw_bits()).unwrap_or(0) };
}

// ---- Define ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineProperty(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    value: JS::Handle<JS::Value>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => {
            let v = unsafe { value_h(cx, value) };
            unsafe { define_impl(cx, obj, k, data_desc(v, attrs), ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineProperty1(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    getter: JSNative,
    setter: JSNative,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => {
            let g = unsafe { native_obj(cx, getter, ::std::ptr::null()) };
            let s = unsafe { native_obj(cx, setter, ::std::ptr::null()) };
            let d = accessor_desc_h(g, s, attrs);
            unsafe { define_impl(cx, obj, k, d, ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineProperty2(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    getter: JS::Handle<*mut JSObject>,
    setter: JS::Handle<*mut JSObject>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => {
            let d = unsafe { accessor_desc(cx, *getter.ptr, *setter.ptr, attrs) };
            unsafe { define_impl(cx, obj, k, d, ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineProperty3(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    value: JS::Handle<*mut JSObject>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => {
            let v = BsValue::object(object_ptr(unsafe { *value.ptr })).to_handle(bs);
            unsafe { define_impl(cx, obj, k, data_desc(v, attrs), ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineProperty4(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    value: JS::Handle<*mut JSString>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => {
            let v = BsValue::string(HeapPtr::from_ptr(unsafe { *value.ptr }.cast())).to_handle(bs);
            unsafe { define_impl(cx, obj, k, data_desc(v, attrs), ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineProperty5(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    value: i32,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => {
            let v = bs.smi(value);
            unsafe { define_impl(cx, obj, k, data_desc(v, attrs), ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineProperty6(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    value: u32,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => {
            let v = BsValue::number(value as f64).to_handle(bs);
            unsafe { define_impl(cx, obj, k, data_desc(v, attrs), ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineProperty7(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    value: f64,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => {
            let v = BsValue::number(value).to_handle(bs);
            unsafe { define_impl(cx, obj, k, data_desc(v, attrs), ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

// ---- DefinePropertyById ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefinePropertyById(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    desc: JS::Handle<JS::PropertyDescriptor>,
    result: *mut JS::ObjectOpResult,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let d = unsafe { desc_from_js(cx, desc) };
    let ok = unsafe { define_impl(cx, obj, key, d, result) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefinePropertyById1(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    desc: JS::Handle<JS::PropertyDescriptor>,
) -> bool {
    JS_DefinePropertyById(cx, obj, id, desc, ::std::ptr::null_mut())
}


#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefinePropertyById3(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    getter: JSNative,
    setter: JSNative,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let g = unsafe { native_obj(cx, getter, ::std::ptr::null()) };
    let s = unsafe { native_obj(cx, setter, ::std::ptr::null()) };
    let d = accessor_desc_h(g, s, attrs);
    let ok = unsafe { define_impl(cx, obj, key, d, ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefinePropertyById4(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    getter: JS::Handle<*mut JSObject>,
    setter: JS::Handle<*mut JSObject>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let d = unsafe { accessor_desc(cx, *getter.ptr, *setter.ptr, attrs) };
    let ok = unsafe { define_impl(cx, obj, key, d, ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefinePropertyById5(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    value: JS::Handle<*mut JSObject>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let v = BsValue::object(object_ptr(unsafe { *value.ptr })).to_handle(bs);
    let ok = unsafe { define_impl(cx, obj, key, data_desc(v, attrs), ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefinePropertyById6(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    value: JS::Handle<*mut JSString>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let v = BsValue::string(HeapPtr::from_ptr(unsafe { *value.ptr }.cast())).to_handle(bs);
    let ok = unsafe { define_impl(cx, obj, key, data_desc(v, attrs), ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefinePropertyById7(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    value: i32,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let v = bs.smi(value);
    let ok = unsafe { define_impl(cx, obj, key, data_desc(v, attrs), ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefinePropertyById8(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    value: u32,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let v = BsValue::number(value as f64).to_handle(bs);
    let ok = unsafe { define_impl(cx, obj, key, data_desc(v, attrs), ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefinePropertyById9(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    value: f64,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let v = BsValue::number(value).to_handle(bs);
    let ok = unsafe { define_impl(cx, obj, key, data_desc(v, attrs), ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

// ==== PART 2 ====
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefinePropertyById2(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    value: JS::Handle<JS::Value>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let v = unsafe { value_h(cx, value) };
    let ok = unsafe { define_impl(cx, obj, key, data_desc(v, attrs), ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

// ---- DefineUCProperty ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineUCProperty(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const u16,
    namelen: usize,
    desc: JS::Handle<JS::PropertyDescriptor>,
    result: *mut JS::ObjectOpResult,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let ok = match key {
        Ok(k) => {
            let d = unsafe { desc_from_js(cx, desc) };
            unsafe { define_impl(cx, obj, k, d, result) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineUCProperty1(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const u16,
    namelen: usize,
    desc: JS::Handle<JS::PropertyDescriptor>,
) -> bool {
    JS_DefineUCProperty(cx, obj, name, namelen, desc, ::std::ptr::null_mut())
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineUCProperty2(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const u16,
    namelen: usize,
    value: JS::Handle<JS::Value>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let ok = match key {
        Ok(k) => {
            let v = unsafe { value_h(cx, value) };
            unsafe { define_impl(cx, obj, k, data_desc(v, attrs), ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineUCProperty3(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const u16,
    namelen: usize,
    getter: JS::Handle<*mut JSObject>,
    setter: JS::Handle<*mut JSObject>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let ok = match key {
        Ok(k) => {
            let d = unsafe { accessor_desc(cx, *getter.ptr, *setter.ptr, attrs) };
            unsafe { define_impl(cx, obj, k, d, ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineUCProperty4(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const u16,
    namelen: usize,
    value: JS::Handle<*mut JSObject>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let ok = match key {
        Ok(k) => {
            let v = BsValue::object(object_ptr(unsafe { *value.ptr })).to_handle(bs);
            unsafe { define_impl(cx, obj, k, data_desc(v, attrs), ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineUCProperty5(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const u16,
    namelen: usize,
    value: JS::Handle<*mut JSString>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let ok = match key {
        Ok(k) => {
            let v = BsValue::string(HeapPtr::from_ptr(unsafe { *value.ptr }.cast())).to_handle(bs);
            unsafe { define_impl(cx, obj, k, data_desc(v, attrs), ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineUCProperty6(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const u16,
    namelen: usize,
    value: i32,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let ok = match key {
        Ok(k) => {
            let v = bs.smi(value);
            unsafe { define_impl(cx, obj, k, data_desc(v, attrs), ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineUCProperty7(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const u16,
    namelen: usize,
    value: u32,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let ok = match key {
        Ok(k) => {
            let v = BsValue::number(value as f64).to_handle(bs);
            unsafe { define_impl(cx, obj, k, data_desc(v, attrs), ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineUCProperty8(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const u16,
    namelen: usize,
    value: f64,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let ok = match key {
        Ok(k) => {
            let v = BsValue::number(value).to_handle(bs);
            unsafe { define_impl(cx, obj, k, data_desc(v, attrs), ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

// ---- DefineElement ----


#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineElement1(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    getter: JS::Handle<*mut JSObject>,
    setter: JS::Handle<*mut JSObject>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(e) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let d = unsafe { accessor_desc(cx, *getter.ptr, *setter.ptr, attrs) };
    let ok = unsafe { define_impl(cx, obj, key, d, ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

/// `JS_DefineProperties` over a null-terminated `JSPropertySpec` list.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineProperties(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    ps: *const JSPropertySpec,
) -> bool {
    let mut bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let mut p = ps;
    let mut ok = true;
    while !p.is_null() && !unsafe { (*p).name.string_ }.is_null() {
        let spec = unsafe { &*p };
        let key = unsafe { spec_name_key(cx, spec.name.string_) };
        let attrs = spec.attributes_ as ::std::os::raw::c_uint;
        match key {
            Ok(k) => {
                let d = match spec.kind_ {
                    JSPropertySpec_Kind::Value => {
                        let vw = unsafe { &spec.u.value };
                        let v: Handle<BsValue> = match vw.type_ {
                            JSPropertySpec_ValueWrapper_Type::String => {
                                let s = unsafe { c_str(vw.__bindgen_anon_1.string) };
                                match bs.alloc_string(&s) {
                                    Ok(s) => s.as_value(),
                                    Err(e) => {
                                        ok = unsafe { fail(cx, e) };
                                        break;
                                    },
                                }
                            },
                            JSPropertySpec_ValueWrapper_Type::Int32 => bs.smi(unsafe { vw.__bindgen_anon_1.int32 }),
                            JSPropertySpec_ValueWrapper_Type::Double => BsValue::number(unsafe { vw.__bindgen_anon_1.double_ }).to_handle(bs),
                        };
                        data_desc(v, attrs)
                    },
                    JSPropertySpec_Kind::NativeAccessor => {
                        let acc = unsafe { &spec.u.accessors };
                        // Handles, not raw pointers: creating the setter's function can collect and move the getter.
                        let g = unsafe { native_obj(cx, acc.getter.native.op, acc.getter.native.info) };
                        let s = unsafe { native_obj(cx, acc.setter.native.op, acc.setter.native.info) };
                        accessor_desc_h(g, s, attrs)
                    },
                    JSPropertySpec_Kind::SelfHostedAccessor => {
                        // Brimstone's builtins are Rust; there is no self-hosted accessor to call.
                        let funname = unsafe { c_str(spec.u.accessors.getter.selfHosted.funname) };
                        ok = unsafe { fail_type(cx, &format!("self-hosted accessor {funname} is not available")) };
                        break;
                    },
                };
                if !unsafe { define_impl(cx, obj, k, d, ::std::ptr::null_mut()) } {
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

// ---- Get ----

unsafe fn get_impl(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    key: Handle<PropertyKey>,
    receiver: Option<Handle<BsValue>>,
    vp: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let mut o = unsafe { obj_h(cx, obj) };
    let recv = receiver.unwrap_or_else(|| o.as_value());
    let r = o.get(bs, key, recv);
    let ok = unsafe { finish(cx, r, vp.ptr) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetProperty(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    vp: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => unsafe { get_impl(cx, obj, k, None, vp) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetPropertyById(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    vp: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let ok = unsafe { get_impl(cx, obj, key, None, vp) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetUCProperty(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const u16,
    namelen: usize,
    vp: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let ok = match key {
        Ok(k) => unsafe { get_impl(cx, obj, k, None, vp) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetElement(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    vp: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(e) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let ok = unsafe { get_impl(cx, obj, key, None, vp) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ForwardGetPropertyTo(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    receiver: JS::HandleValue,
    vp: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let recv = unsafe { value_h(cx, receiver) };
    let ok = unsafe { get_impl(cx, obj, key, Some(recv), vp) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ForwardGetElementTo(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    receiver: JS::Handle<*mut JSObject>,
    vp: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(e) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let recv = BsValue::object(object_ptr(unsafe { *receiver.ptr })).to_handle(bs);
    let ok = unsafe { get_impl(cx, obj, key, Some(recv), vp) };
    scope.exit();
    ok
}

// ---- Set ----

unsafe fn set_impl(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    key: Handle<PropertyKey>,
    v: Handle<BsValue>,
    receiver: Option<Handle<BsValue>>,
    result: *mut JS::ObjectOpResult,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let mut o = unsafe { obj_h(cx, obj) };
    let recv = receiver.unwrap_or_else(|| o.as_value());
    let r = if result.is_null() {
        // JS_SetProperty & co. throw on failure.
        api::set(bs, o, key, v, true).map(|_| true)
    } else {
        api::set_property(bs, o, key, v, recv).map(|b| {
            unsafe { (*result).code_ = if b { JS::ObjectOpResult_SpecialCodes::OkCode as usize } else { 1 } };
            true
        })
    };
    let ok = match r {
        Ok(b) => b,
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetProperty(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    v: JS::HandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => {
            let val = unsafe { value_h(cx, v) };
            unsafe { set_impl(cx, obj, k, val, None, ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetPropertyById(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    v: JS::HandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let val = unsafe { value_h(cx, v) };
    let ok = unsafe { set_impl(cx, obj, key, val, None, ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetUCProperty(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const u16,
    namelen: usize,
    v: JS::HandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let ok = match key {
        Ok(k) => {
            let val = unsafe { value_h(cx, v) };
            unsafe { set_impl(cx, obj, k, val, None, ::std::ptr::null_mut()) }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}


#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ForwardSetPropertyTo(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    v: JS::HandleValue,
    receiver: JS::HandleValue,
    result: *mut JS::ObjectOpResult,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let val = unsafe { value_h(cx, v) };
    let recv = unsafe { value_h(cx, receiver) };
    let ok = unsafe { set_impl(cx, obj, key, val, Some(recv), result) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineElement(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    value: JS::Handle<JS::Value>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(_) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let v: Handle<BsValue> = unsafe { value_h(cx, value) };
    let ok = unsafe { define_impl(cx, obj, key, data_desc(v, attrs), ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineElement2(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    value: JS::Handle<*mut JSObject>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(_) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let v: Handle<BsValue> = BsValue::object(object_ptr(unsafe { *value.ptr })).to_handle(bs_of(cx));
    let ok = unsafe { define_impl(cx, obj, key, data_desc(v, attrs), ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineElement3(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    value: JS::Handle<*mut JSString>,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(_) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let v: Handle<BsValue> = BsValue::string(HeapPtr::from_ptr(unsafe { *value.ptr }.cast())).to_handle(bs_of(cx));
    let ok = unsafe { define_impl(cx, obj, key, data_desc(v, attrs), ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineElement4(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    value: i32,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(_) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let v: Handle<BsValue> = unsafe { cx_of(cx) }.bs.smi(value);
    let ok = unsafe { define_impl(cx, obj, key, data_desc(v, attrs), ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineElement5(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    value: u32,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(_) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let v: Handle<BsValue> = BsValue::number(value as f64).to_handle(bs_of(cx));
    let ok = unsafe { define_impl(cx, obj, key, data_desc(v, attrs), ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DefineElement6(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    value: f64,
    attrs: ::std::os::raw::c_uint,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(_) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let v: Handle<BsValue> = BsValue::number(value).to_handle(bs_of(cx));
    let ok = unsafe { define_impl(cx, obj, key, data_desc(v, attrs), ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

// ==== PART 3 ====
// ---- Has ----

unsafe fn has_impl(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    key: Handle<PropertyKey>,
    own: bool,
    foundp: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, obj) };
    let r = if own { api::has_own_property(bs, o, key) } else { api::has_property(bs, o, key) };
    scope.exit();
    match r {
        Ok(b) => {
            if !foundp.is_null() {
                unsafe { *foundp = b };
            }
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_HasProperty(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    foundp: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => unsafe { has_impl(cx, obj, k, false, foundp) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_HasPropertyById(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    foundp: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let ok = unsafe { has_impl(cx, obj, key, false, foundp) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_HasOwnProperty(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    foundp: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => unsafe { has_impl(cx, obj, k, true, foundp) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_HasOwnPropertyById(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    foundp: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let ok = unsafe { has_impl(cx, obj, key, true, foundp) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_HasUCProperty(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const u16,
    namelen: usize,
    vp: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let ok = match key {
        Ok(k) => unsafe { has_impl(cx, obj, k, false, vp) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_HasElement(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    foundp: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(e) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let ok = unsafe { has_impl(cx, obj, key, false, foundp) };
    scope.exit();
    ok
}

// `JS_AlreadyHasOwn*` is the same own lookup as `JS_HasOwn*`.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AlreadyHasOwnProperty(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    foundp: *mut bool,
) -> bool {
    JS_HasOwnProperty(cx, obj, name, foundp)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AlreadyHasOwnPropertyById(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    foundp: *mut bool,
) -> bool {
    JS_HasOwnPropertyById(cx, obj, id, foundp)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AlreadyHasOwnUCProperty(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const u16,
    namelen: usize,
    foundp: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let ok = match key {
        Ok(k) => unsafe { has_impl(cx, obj, k, true, foundp) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AlreadyHasOwnElement(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    foundp: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(e) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let ok = unsafe { has_impl(cx, obj, key, true, foundp) };
    scope.exit();
    ok
}

// ---- Delete ----

unsafe fn delete_impl(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    key: Handle<PropertyKey>,
    result: *mut JS::ObjectOpResult,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let mut o = unsafe { obj_h(cx, obj) };
    let r = o.delete(bs, key);
    scope.exit();
    match r {
        Ok(b) => {
            if result.is_null() {
                // The throwing form.
                if !b {
                    return unsafe { fail_type(cx, "cannot delete property") };
                }
            } else {
                unsafe { (*result).code_ = if b { JS::ObjectOpResult_SpecialCodes::OkCode as usize } else { 1 } };
            }
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DeleteProperty(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    result: *mut JS::ObjectOpResult,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => unsafe { delete_impl(cx, obj, k, result) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DeleteProperty1(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
) -> bool {
    JS_DeleteProperty(cx, obj, name, ::std::ptr::null_mut())
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DeletePropertyById(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
    result: *mut JS::ObjectOpResult,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let ok = unsafe { delete_impl(cx, obj, key, result) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DeletePropertyById1(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    id: JS::HandleId,
) -> bool {
    JS_DeletePropertyById(cx, obj, id, ::std::ptr::null_mut())
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DeleteUCProperty(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const u16,
    namelen: usize,
    result: *mut JS::ObjectOpResult,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let ok = match key {
        Ok(k) => unsafe { delete_impl(cx, obj, k, result) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DeleteElement(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    result: *mut JS::ObjectOpResult,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(e) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let ok = unsafe { delete_impl(cx, obj, key, result) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DeleteElement1(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
) -> bool {
    JS_DeleteElement(cx, obj, index, ::std::ptr::null_mut())
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetElement(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    v: JS::Handle<JS::Value>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(_) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let val: Handle<BsValue> = unsafe { value_h(cx, v) };
    let ok = unsafe { set_impl(cx, obj, key, val, None, ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetElement1(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    v: JS::Handle<*mut JSObject>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(_) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let val: Handle<BsValue> = BsValue::object(object_ptr(unsafe { *v.ptr })).to_handle(bs_of(cx));
    let ok = unsafe { set_impl(cx, obj, key, val, None, ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetElement2(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    v: JS::Handle<*mut JSString>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(_) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let val: Handle<BsValue> = BsValue::string(HeapPtr::from_ptr(unsafe { *v.ptr }.cast())).to_handle(bs_of(cx));
    let ok = unsafe { set_impl(cx, obj, key, val, None, ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetElement3(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    v: i32,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(_) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let val: Handle<BsValue> = unsafe { cx_of(cx) }.bs.smi(v);
    let ok = unsafe { set_impl(cx, obj, key, val, None, ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetElement4(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    v: u32,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(_) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let val: Handle<BsValue> = BsValue::number(v as f64).to_handle(bs_of(cx));
    let ok = unsafe { set_impl(cx, obj, key, val, None, ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetElement5(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    index: u32,
    v: f64,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = match api::property_key_from_index(bs, index) {
        Ok(k) => k,
        Err(_) => {
            scope.exit();
            return unsafe { fail_type(cx, "allocation failed") };
        },
    };
    let val: Handle<BsValue> = BsValue::number(v).to_handle(bs_of(cx));
    let ok = unsafe { set_impl(cx, obj, key, val, None, ::std::ptr::null_mut()) };
    scope.exit();
    ok
}

// ==== PART 4 ====
// ---- Descriptors ----

unsafe fn own_desc_impl(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    key: Handle<PropertyKey>,
    desc: JS::MutableHandle<JS::PropertyDescriptor>,
    isNone: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, obj) };
    let r = api::get_own_property_descriptor(bs, o, key);
    scope.exit();
    match r {
        Ok(Some(d)) => {
            unsafe {
                desc_to_js(desc.ptr, d);
                if !isNone.is_null() {
                    *isNone = false;
                }
            }
            true
        },
        Ok(None) => {
            if !isNone.is_null() {
                unsafe { *isNone = true };
            }
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__JS_GetOwnPropertyDescriptor(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    name: *const c_char,
    desc: JS::MutableHandle<JS::PropertyDescriptor>,
    isNone: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => unsafe { own_desc_impl(cx, obj, k, desc, isNone) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__JS_GetOwnPropertyDescriptorById(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    id: JS::HandleId,
    desc: JS::MutableHandle<JS::PropertyDescriptor>,
    isNone: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let ok = unsafe { own_desc_impl(cx, obj, key, desc, isNone) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__JS_GetOwnUCPropertyDescriptor(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    name: *const u16,
    namelen: usize,
    desc: JS::MutableHandle<JS::PropertyDescriptor>,
    isNone: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let ok = match key {
        Ok(k) => unsafe { own_desc_impl(cx, obj, k, desc, isNone) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

/// The prototype-walking descriptor lookup: fills `desc` and `holder`, or sets `isNone`.
unsafe fn desc_walk_impl(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    key: Handle<PropertyKey>,
    desc: JS::MutableHandle<JS::PropertyDescriptor>,
    holder: JS::MutableHandleObject,
    isNone: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let mut current = unsafe { obj_h(cx, obj) };
    let r = loop {
        match api::get_own_property_descriptor(bs, current, key) {
            Ok(Some(d)) => break Ok((Some(d), current)),
            Ok(None) => match api::get_prototype_of(bs, current) {
                Ok(Some(proto)) => current = proto,
                Ok(None) => break Ok((None, current)),
                Err(e) => break Err(e),
            },
            Err(e) => break Err(e),
        }
    };
    scope.exit();
    match r {
        Ok((Some(d), h)) => {
            unsafe {
                desc_to_js(desc.ptr, d);
                (*holder.ptr) = (*h).as_ptr().cast();
                if !isNone.is_null() {
                    *isNone = false;
                }
            }
            true
        },
        Ok((None, _)) => {
            if !isNone.is_null() {
                unsafe { *isNone = true };
            }
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__JS_GetPropertyDescriptor(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    name: *const c_char,
    desc: JS::MutableHandle<JS::PropertyDescriptor>,
    holder: JS::MutableHandleObject,
    isNone: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { name_key(cx, name) };
    let ok = match key {
        Ok(k) => unsafe { desc_walk_impl(cx, obj, k, desc, holder, isNone) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__JS_GetPropertyDescriptorById(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    id: JS::HandleId,
    desc: JS::MutableHandle<JS::PropertyDescriptor>,
    holder: JS::MutableHandleObject,
    isNone: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { id_in(bs, id_bits_of(id)) };
    let ok = unsafe { desc_walk_impl(cx, obj, key, desc, holder, isNone) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__JS_GetUCPropertyDescriptor(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    name: *const u16,
    namelen: usize,
    desc: JS::MutableHandle<JS::PropertyDescriptor>,
    holder: JS::MutableHandleObject,
    isNone: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let key = unsafe { uc_name_key(cx, name, namelen) };
    let ok = match key {
        Ok(k) => unsafe { desc_walk_impl(cx, obj, k, desc, holder, isNone) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

/// `JS::ObjectToCompletePropertyDescriptor`: reads value/get/set/writable/enumerable/configurable off a descriptor object.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ToCompletePropertyDescriptor(
    cx: *mut JSContext,
    descriptor: JS::Handle<JS::Value>,
    desc: JS::MutableHandle<JS::PropertyDescriptor>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let v = unsafe { value_h(cx, descriptor) };
    let r = (|| -> Result<api::PropertyDescriptor, EvalError> {
        let obj = api::to_object(bs, v)?;
        let mut out = api::PropertyDescriptor::default();
        let fields: [&str; 6] = ["value", "get", "set", "writable", "enumerable", "configurable"];
        for (i, name) in fields.iter().enumerate() {
            let key = api::property_key_from_string(bs, name)?;
            if !api::has_property(bs, obj, key)? {
                continue;
            }
            let val = api::get(bs, obj, key)?;
            match i {
                0 => out.value = Some(val),
                1 => {
                    out.has_get = true;
                    out.get = if val.is_object() { Some(val.as_object()) } else { None };
                },
                2 => {
                    out.has_set = true;
                    out.set = if val.is_object() { Some(val.as_object()) } else { None };
                },
                3 => out.is_writable = Some(api::to_boolean(*val)),
                4 => out.is_enumerable = Some(api::to_boolean(*val)),
                _ => out.is_configurable = Some(api::to_boolean(*val)),
            }
        }
        Ok(out)
    })();
    let ok = match r {
        Ok(d) => {
            unsafe { desc_to_js(desc.ptr, d) };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

// ---- Enumeration ----

/// The `Vec<u64>` an `IdVector` handle points at (see the glue functions below).
#[inline]
unsafe fn id_vec_of(props: *mut c_void) -> &'static mut Vec<u64> {
    unsafe { &mut *(props as *mut Vec<u64>) }
}

/// `js::GetPropertyKeys` / `JS_Enumerate`, following Snapshot in js/src/vm/Iteration.cpp:
/// own keys first, then the prototype chain unless JSITER_OWNONLY; a seen set deduplicates
/// keys across the walk (a nearer key shadows, even a non-enumerable one); without
/// JSITER_HIDDEN only enumerable keys are kept (checked via each key's own descriptor);
/// symbols need JSITER_SYMBOLS, JSITER_SYMBOLSONLY keeps only symbols. Any error returns
/// false immediately with the exception pending.
unsafe fn keys_impl(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    flags: ::std::os::raw::c_uint,
    props: JS::MutableHandleIdVector,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, obj) };
    let own_only = flags & JSITER_OWNONLY as u32 != 0;
    let mut out: Vec<u64> = Vec::new();
    let mut seen: HashSet<u64> = HashSet::new();
    let r: Result<(), EvalError> = (|| {
        let mut cur = o;
        loop {
            let keys = api::own_property_keys(bs, cur)?;
            for k in keys {
                // Normalize like an incoming id: a string that is a canonical array index
                // is the index key (high indices come back out as strings via id_out).
                let key = unsafe { id_in(bs, (*k).as_raw_bits()) };
                let bits = key.to_raw_value().as_raw_bits();
                if !own_only && !seen.insert(bits) {
                    continue;
                }
                if key.is_symbol() {
                    if flags & JSITER_SYMBOLS as u32 == 0 {
                        continue;
                    }
                } else if flags & JSITER_SYMBOLSONLY as u32 != 0 {
                    continue;
                }
                if flags & JSITER_HIDDEN as u32 == 0 {
                    let desc = api::get_own_property_descriptor(bs, cur, key)?;
                    match desc {
                        Some(d) if d.is_enumerable.unwrap_or(false) => {},
                        _ => continue,
                    }
                }
                out.push(unsafe { id_out(bs, key) });
            }
            if own_only {
                break;
            }
            match api::get_prototype_of(bs, cur)? {
                Some(p) => cur = p,
                None => break,
            }
        }
        Ok(())
    })();
    let ok = match r {
        Ok(()) => {
            let vec = unsafe { id_vec_of(props.ptr as *mut c_void) };
            vec.clear();
            vec.extend(out);
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_Enumerate(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    props: JS::MutableHandle<JS::IdVector>,
) -> bool {
    unsafe { keys_impl(cx, obj, JSITER_OWNONLY, JS::MutableHandleIdVector { ptr: props.ptr as *mut _ }) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__GetPropertyKeys(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    flags: ::std::os::raw::c_uint,
    props: JS::MutableHandleIdVector,
) -> bool {
    unsafe { keys_impl(cx, obj, flags, props) }
}

// ---- IdVector glue ----

/// Traces a rooted id vector's ids; registered in `Cx::extra_roots`, so `gc::trace_all_roots` keeps them alive and current.
#[super::poison::engine_shim(gc)]
unsafe extern "C" fn trace_id_vec(trc: *mut JSTracer, data: *mut c_void) {
    let v = unsafe { &mut *(data as *mut Vec<u64>) };
    for bits in v.iter_mut() {
        unsafe { super::gc::trace_value_slot(trc, bits) };
    }
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn glue__CreateRootedIdVector(cx: *mut JSContext) -> *mut JS::PersistentRootedIdVector {
    let v = Box::into_raw(Box::new(Vec::<u64>::new()));
    unsafe { cx_of(cx) }.extra_roots.push((Some(trace_id_vec), v as *mut c_void));
    v as *mut JS::PersistentRootedIdVector
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__GetIdVectorAddress(v: *mut JS::PersistentRootedIdVector) -> *mut c_void {
    v as *mut c_void
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__AppendToIdVector(v: JS::MutableHandleIdVector, id: JS::HandleId) -> bool {
    let vec = unsafe { id_vec_of(v.ptr as *mut c_void) };
    vec.push(unsafe { id_bits_of(id) });
    true
}

#[super::poison::engine_shim(failure = ::std::ptr::NonNull::<jsid>::dangling().as_ptr())]
pub unsafe extern "C" fn glue__SliceRootedIdVector(
    v: *const JS::PersistentRootedIdVector,
    length: *mut usize,
) -> *const jsid {
    let vec = unsafe { &*(v as *const Vec<u64>) };
    if !length.is_null() {
        unsafe { *length = vec.len() };
    }
    vec.as_ptr() as *const jsid
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__DestroyRootedIdVector(v: *mut JS::PersistentRootedIdVector) {
    if v.is_null() {
        return;
    }
    let data = v as *mut c_void;
    let roots = &mut unsafe { cx_of(super::cx::current_raw()) }.extra_roots;
    if let Some(i) = roots.iter().position(|&(_, d)| d == data) {
        roots.remove(i);
    }
    unsafe { drop(Box::from_raw(v as *mut Vec<u64>)) };
}

// ==== PART 5 ====
// ---- Prototype and extensibility ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetPrototype(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    result: JS::MutableHandleObject,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, obj) };
    let r = api::get_prototype_of(bs, o);
    scope.exit();
    match r {
        Ok(Some(p)) => {
            unsafe { (*result.ptr) = (*p).as_ptr().cast() };
            true
        },
        Ok(None) => {
            unsafe { (*result.ptr) = ::std::ptr::null_mut() };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetPrototypeIfOrdinary(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    isOrdinary: *mut bool,
    result: JS::MutableHandleObject,
) -> bool {
    if !isOrdinary.is_null() {
        unsafe { *isOrdinary = true };
    }
    JS_GetPrototype(cx, obj, result)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetPrototype(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    proto: JS::HandleObject,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, obj) };
    let p = unsafe { *proto.ptr };
    let proto_h = if p.is_null() { None } else { Some(BsValue::object(object_ptr(p)).to_handle(bs).as_object()) };
    let r = api::set_prototype_of(bs, o, proto_h);
    scope.exit();
    match r {
        Ok(true) => true,
        Ok(false) => {
            unsafe { fail_type(cx, "cannot set prototype") }
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_IsExtensible(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    extensible: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, obj) };
    let r = api::is_extensible(bs, o);
    scope.exit();
    match r {
        Ok(b) => {
            if !extensible.is_null() {
                unsafe { *extensible = b };
            }
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_PreventExtensions(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    result: *mut JS::ObjectOpResult,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, obj) };
    let r = api::prevent_extensions(bs, o);
    scope.exit();
    match r {
        Ok(b) => {
            if !result.is_null() {
                unsafe { (*result).code_ = if b { JS::ObjectOpResult_SpecialCodes::OkCode as usize } else { 1 } };
            }
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

/// Freeze: non-extensible, and every own property non-configurable and (for data properties) non-writable.
unsafe fn freeze_impl(cx: *mut JSContext, obj: JS::Handle<*mut JSObject>, deep: bool) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let mut o = unsafe { obj_h(cx, obj) };
    let mut r = api::prevent_extensions(bs, o);
    if matches!(r, Ok(true)) {
        let keys = api::own_property_keys(bs, o);
        match keys {
            Ok(keys) => {
                for k in keys {
                    let key = PropertyKey::from_raw_value(*k).to_handle(bs);
                    match api::get_own_property_descriptor(bs, o, key) {
                        Ok(Some(mut d)) => {
                            if deep {
                                if let Some(v) = d.value {
                                    if v.is_object() {
                                        let sub_ptr = (*v.as_object()).as_ptr().cast::<JSObject>();
                                        let sub_h: JS::Handle<*mut JSObject> = JS::Handle {
                                            _phantom_0: ::std::marker::PhantomData,
                                            ptr: &sub_ptr as *const *mut JSObject,
                                        };
                                        if !unsafe { freeze_impl(cx, sub_h, true) } {
                                            r = Ok(false);
                                            break;
                                        }
                                    }
                                }
                            }
                            d.is_configurable = Some(false);
                            if d.value.is_some() {
                                d.is_writable = Some(false);
                            }
                            match o.define_own_property(bs, key, d) {
                                Ok(true) => {},
                                Ok(false) => {
                                    r = Ok(false);
                                    break;
                                },
                                Err(e) => {
                                    r = Err(e);
                                    break;
                                },
                            }
                        },
                        Ok(None) => {},
                        Err(e) => {
                            r = Err(e);
                            break;
                        },
                    }
                }
            },
            Err(e) => r = Err(e),
        }
    }
    scope.exit();
    match r {
        Ok(_) => true,
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_FreezeObject(cx: *mut JSContext, obj: JS::Handle<*mut JSObject>) -> bool {
    unsafe { freeze_impl(cx, obj, false) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DeepFreezeObject(cx: *mut JSContext, obj: JS::Handle<*mut JSObject>) -> bool {
    unsafe { freeze_impl(cx, obj, true) }
}

// ---- Ids and atoms ----

/// The bits of a string as a `jsid`: a canonical array-index spelling becomes the index id, else the string id.
unsafe fn string_id_bits(bs: Bs, s: Handle<api::StringValue>) -> Result<u64, EvalError> {
    let flat = s.flatten_to_handle()?;
    // SAFETY: the index is parsed from the chars at once (after the flatten); nothing in the match allocates.
    let index = match unsafe { super::string::flat_chars(*flat) } {
        api::StringChars::Latin1(bytes) => id_bits::parse_index_latin1(bytes),
        api::StringChars::TwoByte(units) => id_bits::parse_index_utf16(units),
    };
    if let Some(i) = index {
        return Ok(id_bits::index_id(i));
    }
    Ok(PropertyKey::string_handle(bs, flat.as_string())?.to_raw_value().as_raw_bits())
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_StringToId(
    cx: *mut JSContext,
    s: JS::HandleString,
    idp: JS::MutableHandleId,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let str_h = BsValue::string(HeapPtr::from_ptr(unsafe { *s.ptr }.cast())).to_handle(bs).as_string();
    let r = unsafe { string_id_bits(bs, str_h) };
    let ok = match r {
        Ok(bits) => {
            unsafe { (*idp.ptr).asBits_ = bits as usize };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ValueToId(
    cx: *mut JSContext,
    v: JS::HandleValue,
    idp: JS::MutableHandleId,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let val = unsafe { value_h(cx, v) };
    let r = api::to_property_key(bs, val).map(|k| unsafe { id_out(bs, k) });
    let ok = match r {
        Ok(bits) => {
            unsafe { (*idp.ptr).asBits_ = bits as usize };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_IdToValue(
    _cx: *mut JSContext,
    id: jsid,
    vp: JS::MutableHandle<JS::Value>,
) -> bool {
    let bits = non_zero(id.asBits_ as u64);
    let out = if value_bits::tag(bits) == value_bits::EMPTY_TAG { value_bits::undefined() } else { bits };
    unsafe { (*vp.ptr).asBits_ = out };
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_CharsToId(
    cx: *mut JSContext,
    chars: JS::TwoByteChars,
    arg1: JS::MutableHandleId,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let start = chars._base.mStart.mPtr;
    let end = chars._base.mEnd.mPtr;
    let len = unsafe { end.offset_from(start) } as usize;
    let units = unsafe { ::std::slice::from_raw_parts(start, len) };
    let r = api::string_from_code_units(bs, units).and_then(|s| unsafe { string_id_bits(bs, s.as_string()) });
    let ok = match r {
        Ok(bits) => {
            unsafe { (*arg1.ptr).asBits_ = bits as usize };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

/// Intern a string and return it as a `JSString*`. Interned strings are kept alive by the context.
unsafe fn atomize(bs: Bs, s: Handle<api::StringValue>) -> Result<*mut JSString, EvalError> {
    let flat = s.flatten_to_handle()?;
    let key = PropertyKey::string_not_array_index(bs, flat.as_string())?;
    Ok(key.to_raw_value().as_string().as_ptr().cast())
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AtomizeString(cx: *mut JSContext, s: *const c_char) -> *mut JSString {
    let mut bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let text = unsafe { c_str(s) };
    let r = bs.alloc_string(&text).and_then(|s| unsafe { atomize(bs, s) });
    scope.exit();
    r.unwrap_or(::std::ptr::null_mut())
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AtomizeStringN(cx: *mut JSContext, s: *const c_char, length: usize) -> *mut JSString {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let bytes = unsafe { ::std::slice::from_raw_parts(s.cast::<u8>(), length) };
    let r = api::string_from_latin1(bs, bytes).and_then(|s| unsafe { atomize(bs, s.as_string()) });
    scope.exit();
    r.unwrap_or(::std::ptr::null_mut())
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AtomizeUCString(cx: *mut JSContext, s: *const u16) -> *mut JSString {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let mut len = 0usize;
    unsafe {
        while *s.add(len) != 0 {
            len += 1;
        }
    }
    let units = unsafe { ::std::slice::from_raw_parts(s, len) };
    let r = api::string_from_code_units(bs, units).and_then(|s| unsafe { atomize(bs, s.as_string()) });
    scope.exit();
    r.unwrap_or(::std::ptr::null_mut())
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AtomizeUCStringN(cx: *mut JSContext, s: *const u16, length: usize) -> *mut JSString {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let units = unsafe { ::std::slice::from_raw_parts(s, length) };
    let r = api::string_from_code_units(bs, units).and_then(|s| unsafe { atomize(bs, s.as_string()) });
    scope.exit();
    r.unwrap_or(::std::ptr::null_mut())
}

/// Pinning note: SpiderMonkey's `JS_AtomizeAndPinString` keeps the atom alive forever and its address stable. Brimstone
/// interns the string in the context's atom table, so it is kept alive, but the collector may still move it: a pinned
/// string's address is only stable until the next GC unless it is rooted.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AtomizeAndPinString(cx: *mut JSContext, s: *const c_char) -> *mut JSString {
    JS_AtomizeString(cx, s)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AtomizeAndPinStringN(cx: *mut JSContext, s: *const c_char, length: usize) -> *mut JSString {
    JS_AtomizeStringN(cx, s, length)
}

// ---- jsid glue ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__RUST_JSID_IS_INT(id: JS::HandleId) -> bool {
    id_bits::is_int(unsafe { id_bits_of(id) })
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__RUST_JSID_IS_STRING(id: JS::HandleId) -> bool {
    let bits = unsafe { id_bits_of(id) };
    id_bits::is_gcthing(bits) && BsValue::from_raw_bits(bits).is_string()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__RUST_JSID_IS_VOID(id: JS::HandleId) -> bool {
    value_bits::tag(unsafe { id_bits_of(id) }) == value_bits::EMPTY_TAG
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__RUST_JSID_TO_INT(id: JS::HandleId) -> i32 {
    id_bits::to_int(unsafe { id_bits_of(id) })
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__RUST_JSID_TO_STRING(id: JS::HandleId) -> *mut JSString {
    let bits = unsafe { id_bits_of(id) };
    if id_bits::is_gcthing(bits) {
        BsValue::from_raw_bits(bits).as_string().as_ptr().cast()
    } else {
        ::std::ptr::null_mut()
    }
}

#[super::poison::engine_shim(failure = unsafe { (*id.ptr).asBits_ = id_bits::void_id() as usize })]
pub unsafe extern "C" fn glue__RUST_INTERNED_STRING_TO_JSID(
    _cx: *mut JSContext,
    str_: *mut JSString,
    id: JS::MutableHandleId,
) {
    unsafe { (*id.ptr).asBits_ = value_bits::pointer(str_ as u64) as usize };
}

#[super::poison::engine_shim(failure = unsafe { (*id.ptr).asBits_ = id_bits::void_id() as usize })]
pub unsafe extern "C" fn glue__RUST_SYMBOL_TO_JSID(sym: *mut JS::Symbol, id: JS::MutableHandleId) {
    unsafe { (*id.ptr).asBits_ = value_bits::pointer(sym as u64) as usize };
}

#[super::poison::engine_shim(failure = unsafe { (*id.ptr).asBits_ = id_bits::void_id() as usize })]
pub unsafe extern "C" fn glue__int_to_jsid(i: i32, id: JS::MutableHandleId) {
    unsafe { (*id.ptr).asBits_ = id_bits::int_id(i) as usize };
}

/// `glue__FromPropertyDescriptor`: a `JS::PropertyDescriptor` as a plain JS object with the present fields as properties.
#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__FromPropertyDescriptor(
    cx: *mut JSContext,
    desc_: JS::Handle<JS::PropertyDescriptor>,
    vp: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let d = unsafe { &*desc_.ptr };
    let r = (|| -> Result<Handle<BsValue>, EvalError> {
        let obj = api::new_object(bs)?;
        let mut set_field = |name: &str, val: Handle<BsValue>| -> Result<(), EvalError> {
            let key = api::property_key_from_string(bs, name)?;
            api::create_data_property(bs, obj, key, val)?;
            Ok(())
        };
        if d.hasValue_() {
            set_field("value", super::util::handle_of(bs, d.value_.asBits_))?;
        }
        if d.hasGetter_() {
            let g = if d.getter_.is_null() { bs.undefined() } else { BsValue::object(object_ptr(d.getter_)).to_handle(bs) };
            set_field("get", g)?;
        }
        if d.hasSetter_() {
            let s = if d.setter_.is_null() { bs.undefined() } else { BsValue::object(object_ptr(d.setter_)).to_handle(bs) };
            set_field("set", s)?;
        }
        if d.hasWritable_() {
            set_field("writable", BsValue::bool(d.writable_()).to_handle(bs))?;
        }
        if d.hasEnumerable_() {
            set_field("enumerable", BsValue::bool(d.enumerable_()).to_handle(bs))?;
        }
        if d.hasConfigurable_() {
            set_field("configurable", BsValue::bool(d.configurable_()).to_handle(bs))?;
        }
        Ok(obj.as_value())
    })();
    let ok = unsafe { finish(cx, r, vp.ptr) };
    scope.exit();
    ok
}
