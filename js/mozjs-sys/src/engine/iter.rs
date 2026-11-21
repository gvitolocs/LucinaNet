/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `JS::ForOfIterator`: what Servo's WebIDL sequence and iterable conversions use. The original is
//! `js/src/vm/ForOfIterator.cpp` (SpiderMonkey 153). Its fast path for packed arrays (`index`) is not taken: every value goes
//! through the iteration protocol, which gives the same results more slowly.
//!
//! The iterator object and `next` live in the struct's own `Rooted` fields, which mozjs links into the context's root lists,
//! so the GC sees and updates them between calls.

use brimstone_core::runtime::embed::{self, api};
use brimstone_core::runtime::gc::HandleScope;
use brimstone_core::runtime::{PropertyKey, Value as BsValue};

use super::realm::object_ptr;
use super::util::{fail, handle_of};
use crate::boundary::value_bits;
use crate::jsapi::*;

/// `ForOfIterator::NOT_ARRAY`.
const NOT_ARRAY: u32 = u32::MAX;

unsafe fn type_error(cx: *mut JSContext, msg: &str) -> bool {
    let bs = unsafe { super::cx::cx_of(cx) }.bs;
    let realm = embed::current_realm(bs);
    match api::new_type_error(bs, realm, msg) {
        Ok(e) => unsafe { super::cx::cx_of(cx) }.set_pending(e.as_value().as_raw_bits()),
        Err(e) => unsafe {
            fail(cx, e);
        },
    }
    false
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__JS_ForOfIteratorInit(
    it: *mut JS::ForOfIterator,
    iterable: JS::HandleValue,
    behavior: JS::ForOfIterator_NonIterableBehavior,
) -> bool {
    let cx = unsafe { (*it).cx_ };
    let bs = unsafe { super::cx::cx_of(cx) }.bs;
    unsafe {
        (*it).index = NOT_ARRAY;
        (*it).iterator.data = ::std::ptr::null_mut();
        (*it).nextMethod.data.asBits_ = value_bits::undefined();
    }
    let scope = HandleScope::enter(bs);
    let v = handle_of(bs, unsafe { (*iterable.ptr).asBits_ });
    let allow = behavior == JS::ForOfIterator_NonIterableBehavior::AllowNonIterable;
    let ok = (|| -> bool {
        // SpiderMonkey always performs ToObject, including in AllowNonIterable mode.
        let obj = match api::to_object(bs, v) {
            Ok(o) => o,
            Err(e) => return unsafe { fail(cx, e) },
        };
        let sym = api::well_known_symbol(bs, api::WellKnownSymbol::Iterator);
        let key = PropertyKey::symbol(sym);
        // Preserve the original receiver for accessors on primitive prototypes.
        let method = match obj.get(bs, key, v) {
            Ok(m) => m,
            Err(e) => return unsafe { fail(cx, e) },
        };
        // This host API accepts only undefined in AllowNonIterable mode, not null.
        if allow && method.is_undefined() {
            return true;
        }
        if !api::is_callable(method) {
            return unsafe { type_error(cx, "@@iterator is not a function") };
        }
        let iter = match embed::call(bs, method, v, &[]) {
            Ok(i) => i,
            Err(e) => return unsafe { fail(cx, e) },
        };
        if !iter.is_object() {
            return unsafe { type_error(cx, "iterator is not an object") };
        }
        let iter_obj = iter.as_object();
        let next_key = match api::property_key_from_string(bs, "next") {
            Ok(k) => k,
            Err(e) => return unsafe { fail(cx, e) },
        };
        let next = match api::get(bs, iter_obj, next_key) {
            Ok(n) => n,
            Err(e) => return unsafe { fail(cx, e) },
        };
        unsafe {
            (*it).iterator.data = (*iter_obj).as_ptr().cast();
            (*it).nextMethod.data.asBits_ = next.as_raw_bits();
        }
        true
    })();
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__JS_ForOfIteratorValueIsIterable(it: *const JS::ForOfIterator) -> bool {
    !unsafe { (*it).iterator.data }.is_null()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__JS_ForOfIteratorNext(
    it: *mut JS::ForOfIterator,
    val: JS::MutableHandleValue,
    done: *mut bool,
) -> bool {
    let cx = unsafe { (*it).cx_ };
    let bs = unsafe { super::cx::cx_of(cx) }.bs;
    let iter = unsafe { (*it).iterator.data };
    if iter.is_null() {
        unsafe { *done = true };
        return true;
    }
    let scope = HandleScope::enter(bs);
    let ok = (|| -> bool {
        let next = handle_of(bs, unsafe { (*it).nextMethod.data.asBits_ });
        let this = handle_of(bs, BsValue::object(object_ptr(iter)).as_raw_bits());
        let result = match embed::call(bs, next, this, &[]) {
            Ok(r) => r,
            Err(e) => return unsafe { fail(cx, e) },
        };
        if !result.is_object() {
            return unsafe { type_error(cx, "iterator result is not an object") };
        }
        let r = result.as_object();
        let done_key = match api::property_key_from_string(bs, "done") {
            Ok(k) => k,
            Err(e) => return unsafe { fail(cx, e) },
        };
        let d = match api::get(bs, r, done_key) {
            Ok(d) => api::to_boolean(*d),
            Err(e) => return unsafe { fail(cx, e) },
        };
        unsafe { *done = d };
        if d {
            unsafe { (*val.ptr).asBits_ = value_bits::undefined() };
            return true;
        }
        let value_key = match api::property_key_from_string(bs, "value") {
            Ok(k) => k,
            Err(e) => return unsafe { fail(cx, e) },
        };
        match api::get(bs, r, value_key) {
            Ok(v) => {
                unsafe { (*val.ptr).asBits_ = v.as_raw_bits() };
                true
            },
            Err(e) => unsafe { fail(cx, e) },
        }
    })();
    scope.exit();
    ok
}
