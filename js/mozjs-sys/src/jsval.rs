/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this file,
 * You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `JS::Value` on Brimstone. The API is mozjs_sys's (same names, same asserts where they still mean something); the bits are
//! Brimstone's own NaN-boxed `Value`, so values cross the engine seam unchanged. The encoding lives in `boundary::value_bits`
//! (Verus-checked); what needs the heap (object, string, symbol or BigInt behind a pointer) asks the engine.
//!
//! Differences from SpiderMonkey a caller can see:
//! - `DoubleValue` of an integral number in int32 range (not -0) gives an int32, as Brimstone's `Value::number` does, because the
//!   engine relies on that form. `is_number`/`to_number` agree either way.
//! - Magic values are Brimstone's empty value; `MagicValue` takes no reason.
//! - `PrivateValue` is encoded as the double with the pointer's bits, never as a heap pointer, so the GC cannot follow it.

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use crate::boundary::value_bits as vb;
use crate::jsapi::JSContext;
use crate::jsapi::JSObject;
use crate::jsapi::JSString;
use crate::jsapi::JS::BigInt;
use crate::jsapi::JS::Symbol;
use crate::jsapi::JS::TraceKind;
use crate::jsapi::JS::Value;

use libc::c_void;
use std::default::Default;

pub type JSVal = Value;

#[inline(always)]
const fn AsJSVal(val: u64) -> JSVal {
    JSVal { asBits_: val }
}

/// What kind of heap item a pointer value holds (the engine reads its shape).
#[inline(always)]
fn heap_kind(bits: u64) -> TraceKind {
    debug_assert!(vb::is_traced(bits));
    crate::engine::trace_kind_of(bits)
}

#[inline(always)]
pub fn NullValue() -> JSVal {
    AsJSVal(vb::null())
}

#[inline(always)]
pub fn UndefinedValue() -> JSVal {
    AsJSVal(vb::undefined())
}

/// SpiderMonkey's magic values; Brimstone has one, "empty".
#[inline(always)]
pub fn MagicValue() -> JSVal {
    AsJSVal(vb::empty())
}

#[inline(always)]
pub fn Int32Value(i: i32) -> JSVal {
    AsJSVal(vb::int32(i))
}

/// A number, in the form Brimstone's `Value::number` gives it: an int32 when it is one, else a double, NaN canonical.
#[inline(always)]
pub fn DoubleValue(f: f64) -> JSVal {
    if f.trunc() == f && (i32::MIN as f64..=i32::MAX as f64).contains(&f) && f.to_bits() != (-0.0f64).to_bits() {
        return Int32Value(f as i32);
    }
    AsJSVal(vb::double(f.to_bits()))
}

#[inline(always)]
pub fn UInt32Value(ui: u32) -> JSVal {
    if ui > 0x7fffffff {
        DoubleValue(ui as f64)
    } else {
        Int32Value(ui as i32)
    }
}

#[inline(always)]
fn PointerValue(p: u64) -> JSVal {
    assert!(p != 0 && p < vb::POINTER_LIMIT && p & 7 == 0);
    AsJSVal(vb::pointer(p))
}

#[inline(always)]
pub fn StringValue(s: &JSString) -> JSVal {
    PointerValue(s as *const JSString as usize as u64)
}

#[inline(always)]
pub fn BooleanValue(b: bool) -> JSVal {
    AsJSVal(vb::boolean(b))
}

#[inline(always)]
pub fn ObjectValue(o: *mut JSObject) -> JSVal {
    PointerValue(o as usize as u64)
}

#[inline(always)]
pub fn ObjectOrNullValue(o: *mut JSObject) -> JSVal {
    if o.is_null() {
        NullValue()
    } else {
        ObjectValue(o)
    }
}

#[inline(always)]
pub fn SymbolValue(s: &Symbol) -> JSVal {
    PointerValue(s as *const Symbol as usize as u64)
}

#[inline(always)]
pub fn BigIntValue(s: &BigInt) -> JSVal {
    PointerValue(s as *const BigInt as usize as u64)
}

#[inline(always)]
pub fn PrivateValue(o: *const c_void) -> JSVal {
    let ptrBits = o as usize as u64;
    assert!(ptrBits < vb::POINTER_LIMIT);
    AsJSVal(vb::private(ptrBits))
}

impl JSVal {
    #[inline(always)]
    fn asBits(&self) -> u64 {
        self.asBits_
    }

    #[inline(always)]
    fn kind(&self) -> vb::Kind {
        vb::kind(self.asBits())
    }

    #[inline(always)]
    pub fn is_undefined(&self) -> bool {
        // An all-zero slot (a `Heap` nobody set) reads as undefined, as a zeroed SpiderMonkey value reads as a double 0 only
        // by accident; nothing sets zero on purpose.
        matches!(self.kind(), vb::Kind::Undefined | vb::Kind::Zero)
    }

    #[inline(always)]
    pub fn is_null(&self) -> bool {
        self.kind() == vb::Kind::Null
    }

    #[inline(always)]
    pub fn is_null_or_undefined(&self) -> bool {
        self.is_null() || self.is_undefined()
    }

    #[inline(always)]
    pub fn is_boolean(&self) -> bool {
        self.kind() == vb::Kind::Bool
    }

    #[inline(always)]
    pub fn is_int32(&self) -> bool {
        self.kind() == vb::Kind::Int32
    }

    #[inline(always)]
    pub fn is_double(&self) -> bool {
        self.kind() == vb::Kind::Double
    }

    #[inline(always)]
    pub fn is_number(&self) -> bool {
        matches!(self.kind(), vb::Kind::Int32 | vb::Kind::Double)
    }

    #[inline(always)]
    pub fn is_primitive(&self) -> bool {
        !self.is_object()
    }

    #[inline(always)]
    pub fn is_string(&self) -> bool {
        vb::is_traced(self.asBits()) && heap_kind(self.asBits()) == TraceKind::String
    }

    #[inline(always)]
    pub fn is_object(&self) -> bool {
        vb::is_traced(self.asBits()) && heap_kind(self.asBits()) == TraceKind::Object
    }

    #[inline(always)]
    pub fn is_object_or_null(&self) -> bool {
        self.is_null() || self.is_object()
    }

    #[inline(always)]
    pub fn is_magic(&self) -> bool {
        self.kind() == vb::Kind::Empty
    }

    #[inline(always)]
    pub fn is_symbol(&self) -> bool {
        vb::is_traced(self.asBits()) && heap_kind(self.asBits()) == TraceKind::Symbol
    }

    #[inline(always)]
    pub fn is_bigint(&self) -> bool {
        vb::is_traced(self.asBits()) && heap_kind(self.asBits()) == TraceKind::BigInt
    }

    /// A pointer to a GC thing. Unlike SpiderMonkey's, null is not one (it has no pointer here).
    #[inline(always)]
    pub fn is_gcthing(&self) -> bool {
        vb::is_traced(self.asBits())
    }

    #[inline(always)]
    pub fn to_boolean(&self) -> bool {
        assert!(self.is_boolean());
        vb::to_boolean(self.asBits())
    }

    #[inline(always)]
    pub fn to_int32(&self) -> i32 {
        assert!(self.is_int32());
        vb::to_int32(self.asBits())
    }

    #[inline(always)]
    pub fn to_double(&self) -> f64 {
        assert!(self.is_double());
        f64::from_bits(vb::to_double_bits(self.asBits()))
    }

    #[inline(always)]
    pub fn to_number(&self) -> f64 {
        assert!(self.is_number());
        if self.is_double() {
            self.to_double()
        } else {
            self.to_int32() as f64
        }
    }

    #[inline(always)]
    pub fn to_string(&self) -> *mut JSString {
        assert!(self.is_string());
        self.asBits() as usize as *mut JSString
    }

    #[inline(always)]
    pub fn to_object(&self) -> *mut JSObject {
        assert!(self.is_object());
        self.asBits() as usize as *mut JSObject
    }

    #[inline(always)]
    pub fn to_object_or_null(&self) -> *mut JSObject {
        assert!(self.is_object_or_null());
        if self.is_null() {
            std::ptr::null_mut()
        } else {
            self.asBits() as usize as *mut JSObject
        }
    }

    #[inline(always)]
    pub fn to_symbol(&self) -> *mut Symbol {
        assert!(self.is_symbol());
        self.asBits() as usize as *mut Symbol
    }

    #[inline(always)]
    pub fn to_bigint(&self) -> *mut BigInt {
        assert!(self.is_bigint());
        self.asBits() as usize as *mut BigInt
    }

    #[inline(always)]
    pub fn to_private(&self) -> *const c_void {
        assert!(self.is_double());
        let p = vb::to_private(self.asBits());
        assert!(p < vb::POINTER_LIMIT);
        p as usize as *const c_void
    }

    #[inline(always)]
    pub fn to_gcthing(&self) -> *mut c_void {
        assert!(self.is_gcthing());
        self.asBits() as usize as *mut c_void
    }

    #[inline(always)]
    pub fn is_markable(&self) -> bool {
        self.is_gcthing()
    }

    #[inline(always)]
    pub fn trace_kind(&self) -> TraceKind {
        assert!(self.is_markable());
        heap_kind(self.asBits())
    }
}

impl Default for JSVal {
    fn default() -> JSVal {
        UndefinedValue()
    }
}

#[inline(always)]
pub unsafe fn JS_ARGV(_cx: *mut JSContext, vp: *mut JSVal) -> *mut JSVal {
    vp.offset(2)
}

#[inline(always)]
pub unsafe fn JS_CALLEE(_cx: *mut JSContext, vp: *mut JSVal) -> JSVal {
    *vp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_take_brimstones_form() {
        assert!(DoubleValue(3.0).is_int32());
        assert_eq!(DoubleValue(3.0).to_int32(), 3);
        assert!(DoubleValue(-0.0).is_double());
        assert_eq!(DoubleValue(-0.0).to_double().to_bits(), (-0.0f64).to_bits());
        assert!(DoubleValue(3.5).is_double());
        assert_eq!(DoubleValue(3.5).to_number(), 3.5);
        assert!(DoubleValue(f64::from_bits(0xFFF8_0000_0000_1234)).is_double());
        assert!(DoubleValue(f64::NAN).to_double().is_nan());
        assert!(DoubleValue(f64::INFINITY).is_double());
        assert!(DoubleValue(2147483648.0).is_double());
        assert_eq!(UInt32Value(0x8000_0000).to_number(), 2147483648.0);
        assert_eq!(Int32Value(-7).to_int32(), -7);
    }

    #[test]
    fn tags() {
        assert!(UndefinedValue().is_undefined() && !UndefinedValue().is_null());
        assert!(NullValue().is_null() && NullValue().is_null_or_undefined() && NullValue().is_object_or_null());
        assert!(BooleanValue(true).to_boolean() && !BooleanValue(false).to_boolean());
        assert!(MagicValue().is_magic());
        assert!(!DoubleValue(1.5).is_gcthing() && !NullValue().is_gcthing());
        assert!(JSVal { asBits_: 0 }.is_undefined() && !JSVal { asBits_: 0 }.is_gcthing());
    }

    #[test]
    fn private_values_are_not_pointers() {
        let x = 0u64;
        let p = &x as *const u64 as *const c_void;
        let v = PrivateValue(p);
        assert!(!v.is_gcthing() && v.is_double());
        assert_eq!(v.to_private(), p);
        let n = PrivateValue(std::ptr::null());
        assert!(!n.is_gcthing() && n.to_private().is_null());
    }
}
