/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The shapes every API function shares: values in and out of Brimstone, and SpiderMonkey's "false and a pending exception"
//! for a failed operation.

use brimstone_core::runtime::embed;
use brimstone_core::runtime::eval_result::EvalError;
use brimstone_core::runtime::{Context as Bs, Handle, Value as BsValue};

use super::cx::cx_of;
use super::func::non_zero;
use crate::jsapi::*;

/// A Brimstone handle for value bits Servo gave us (a zero slot is undefined).
#[inline]
pub fn handle_of(bs: Bs, bits: u64) -> Handle<BsValue> {
    BsValue::from_raw_bits(non_zero(bits)).to_handle(bs)
}

#[inline]
pub fn bits_of(h: Handle<BsValue>) -> u64 {
    h.as_raw_bits()
}

/// Records a failed operation as SpiderMonkey does: the exception becomes the context's pending exception, the call returns false.
pub unsafe fn fail(cx: *mut JSContext, e: EvalError) -> bool {
    // An uncatchable termination leaves nothing pending, as SpiderMonkey's does.
    if let Some(v) = embed::exception_value_opt(e) {
        unsafe { cx_of(cx) }.set_pending(v.as_raw_bits());
    }
    false
}

/// `Ok` writes the value to `out`, `Err` records the exception.
pub unsafe fn finish(cx: *mut JSContext, r: Result<Handle<BsValue>, EvalError>, out: *mut JS::Value) -> bool {
    match r {
        Ok(v) => {
            if !out.is_null() {
                unsafe { (*out).asBits_ = v.as_raw_bits() };
            }
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

/// A C string as `&str` (lossy: SpiderMonkey's file names and identifiers are UTF-8, Latin-1 bytes come out replaced).
pub unsafe fn c_str<'a>(p: *const ::std::os::raw::c_char) -> ::std::borrow::Cow<'a, str> {
    if p.is_null() {
        return ::std::borrow::Cow::Borrowed("");
    }
    unsafe { ::std::ffi::CStr::from_ptr(p) }.to_string_lossy()
}
