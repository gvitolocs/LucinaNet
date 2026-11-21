/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Values, conversions and collections: `JS::Value` conversions and comparisons, JSON, BigInt, symbols, Date, RegExp,
//! Map/Set/WeakMap, the remaining string entry points, rooted vectors and the allocation functions.
//! The originals are `js/src/jsapi.cpp`, `builtin/JSON.cpp`, `builtin/MapObject.cpp`, `vm/BigIntType.cpp`,
//! `jsdate.cpp`, `builtin/RegExp.cpp`, `vm/StringType.cpp` and `vm/CharacterEncoding.cpp` in mozjs_sys 153.6.0-1.

use ::std::ffi::c_void;
use ::std::os::raw::{c_char, c_uint};

use brimstone_core::runtime::embed::{self, api};
use brimstone_core::runtime::eval_result::EvalError;
use brimstone_core::runtime::gc::{HandleScope, HeapPtr};
use brimstone_core::runtime::intrinsics::bigint_object::BigIntObject;
use brimstone_core::runtime::intrinsics::error_object::ErrorObject;
use brimstone_core::runtime::intrinsics::map_object::MapObject;
use brimstone_core::runtime::intrinsics::regexp_object::RegExpObject;
use brimstone_core::runtime::intrinsics::set_object::SetObject;
use brimstone_core::runtime::intrinsics::weak_map_object::WeakMapObject;
use brimstone_core::runtime::{Context as Bs, Handle, PropertyKey, Value as BsValue};

use super::cx::cx_of;
use super::func::non_zero;
use super::property::id_bits_of;
use super::realm::object_ptr;
use super::util::{fail, finish, handle_of};
use crate::boundary::id_bits;
use crate::jsapi::*;

// ---- Shared shapes ----

#[inline]
fn bs_of(cx: *mut JSContext) -> Bs {
    unsafe { cx_of(cx) }.bs
}

#[inline]
unsafe fn val(cx: *mut JSContext, h: JS::HandleValue) -> Handle<BsValue> {
    handle_of(bs_of(cx), unsafe { (*h.ptr).asBits_ })
}

#[inline]
unsafe fn obj_val(cx: *mut JSContext, p: *mut JSObject) -> Handle<BsValue> {
    BsValue::object(object_ptr(p)).to_handle(bs_of(cx))
}

#[inline]
unsafe fn obj_h(cx: *mut JSContext, p: *mut JSObject) -> Handle<api::ObjectValue> {
    unsafe { obj_val(cx, p) }.as_object()
}

#[inline]
unsafe fn str_ptr(s: *mut JSString) -> HeapPtr<api::StringValue> {
    HeapPtr::from_ptr(s.cast())
}

/// The string's code units as a Rust `Vec<u16>` (copies; flattens ropes).
unsafe fn units_of(cx: *mut JSContext, s: *mut JSString) -> Option<Vec<u16>> {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let h = BsValue::string(str_ptr(s)).to_handle(bs).as_string();
    // SAFETY: the chars are copied out at once (after the flatten, which may GC); nothing in the match allocates.
    let r = h.flatten_to_handle().ok().map(|f| match unsafe { super::string::flat_chars(*f) } {
        api::StringChars::Latin1(b) => b.iter().map(|&c| c as u16).collect(),
        api::StringChars::TwoByte(u) => u.to_vec(),
    });
    scope.exit();
    r
}

/// Two strings' contents are equal.
unsafe fn string_eq(bs: Bs, a: HeapPtr<api::StringValue>, b: HeapPtr<api::StringValue>) -> bool {
    match (a.to_handle().flatten_to_handle(), b.to_handle().flatten_to_handle()) {
        (Ok(x), Ok(y)) => {
            // SAFETY: both flattens (which may GC) are done; the comparison below only reads the chars and allocates nothing.
            let (cx, cy) = unsafe { (super::string::flat_chars(*x), super::string::flat_chars(*y)) };
            match (cx, cy) {
                (api::StringChars::Latin1(xb), api::StringChars::Latin1(yb)) => xb == yb,
                (api::StringChars::TwoByte(xu), api::StringChars::TwoByte(yu)) => xu == yu,
                (api::StringChars::Latin1(xb), api::StringChars::TwoByte(yu)) => {
                    xb.len() == yu.len() && xb.iter().zip(yu).all(|(&x, &y)| x as u16 == y)
                },
                (api::StringChars::TwoByte(xu), api::StringChars::Latin1(yb)) => {
                    xu.len() == yb.len() && xu.iter().zip(yb).all(|(&x, &y)| x == y as u16)
                },
            }
        },
        _ => false,
    }
}

/// The `JSType` of a value.
fn type_of(bs: Bs, v: BsValue) -> JSType {
    if v.is_undefined() {
        JSType::JSTYPE_UNDEFINED
    } else if v.is_object() {
        // ECMA-262 B.3.6: an object with the [[IsHTMLDDA]] slot reports "undefined", even when it is callable.
        if v.is_htmldda() {
            JSType::JSTYPE_UNDEFINED
        } else if api::is_callable(v.to_handle(bs)) {
            JSType::JSTYPE_FUNCTION
        } else {
            JSType::JSTYPE_OBJECT
        }
    } else if v.is_string() {
        JSType::JSTYPE_STRING
    } else if v.is_number() {
        JSType::JSTYPE_NUMBER
    } else if v.is_bool() {
        JSType::JSTYPE_BOOLEAN
    } else if v.is_symbol() {
        JSType::JSTYPE_SYMBOL
    } else {
        JSType::JSTYPE_BIGINT
    }
}

/// ECMA-262 Strict Equality Comparison on Brimstone values.
unsafe fn strict_equal(bs: Bs, a: Handle<BsValue>, b: Handle<BsValue>) -> bool {
    let (x, y) = (*a, *b);
    if x.is_number() || y.is_number() {
        return x.is_number() && y.is_number() && x.as_number() == y.as_number();
    }
    if x.is_string() && y.is_string() {
        return unsafe { string_eq(bs, x.as_string(), y.as_string()) };
    }
    if x.is_bigint() && y.is_bigint() {
        return x.as_bigint().bigint() == y.as_bigint().bigint();
    }
    x.as_raw_bits() == y.as_raw_bits()
}

/// ECMA-262 IsLooselyEqual.
unsafe fn loose_equal(bs: Bs, a: Handle<BsValue>, b: Handle<BsValue>) -> Result<bool, EvalError> {
    let (x, y) = (*a, *b);
    // ECMA-262 B.3.6: an object with [[IsHTMLDDA]] equals null and undefined. Before the typeof compare, because
    // `type_of` reports exactly those objects as undefined and would otherwise send the pair to strict equality.
    if (y.is_null() || y.is_undefined()) && x.is_htmldda() {
        return Ok(true);
    }
    if (x.is_null() || x.is_undefined()) && y.is_htmldda() {
        return Ok(true);
    }
    if type_of(bs, x) as u32 == type_of(bs, y) as u32 {
        return Ok(unsafe { strict_equal(bs, a, b) });
    }
    if (x.is_null() || x.is_undefined()) && (y.is_null() || y.is_undefined()) {
        return Ok(true);
    }
    if x.is_number() && y.is_string() {
        let n = api::to_number(bs, b)?;
        return Ok(x.as_number() == n.as_number());
    }
    if x.is_string() && y.is_number() {
        let n = api::to_number(bs, a)?;
        return Ok(n.as_number() == y.as_number());
    }
    if x.is_bigint() && y.is_string() {
        return Ok(parse_bigint_opt(bs, y.as_string()).map_or(false, |bi| bi == x.as_bigint().bigint()));
    }
    if x.is_string() && y.is_bigint() {
        return Ok(parse_bigint_opt(bs, x.as_string()).map_or(false, |bi| bi == y.as_bigint().bigint()));
    }
    if x.is_bool() {
        let n = BsValue::number(if x.as_bool() { 1.0 } else { 0.0 }).to_handle(bs);
        return unsafe { loose_equal(bs, n, b) };
    }
    if y.is_bool() {
        let n = BsValue::number(if y.as_bool() { 1.0 } else { 0.0 }).to_handle(bs);
        return unsafe { loose_equal(bs, a, n) };
    }
    let y_prim = y.is_object() && (x.is_number() || x.is_string() || x.is_bigint() || x.is_symbol());
    let x_prim = x.is_object() && (y.is_number() || y.is_string() || y.is_bigint() || y.is_symbol());
    if y_prim {
        let p = api::to_primitive(bs, b, api::ToPrimitivePreferredType::None)?;
        return unsafe { loose_equal(bs, a, p) };
    }
    if x_prim {
        let p = api::to_primitive(bs, a, api::ToPrimitivePreferredType::None)?;
        return unsafe { loose_equal(bs, p, b) };
    }
    Ok(false)
}

/// Parse a string as a BigInt literal (ECMA `StringToBigInt`), None if not a valid literal.
fn parse_bigint_opt(bs: Bs, s: HeapPtr<api::StringValue>) -> Option<api::BigInt> {
    let text = api::string_to_rust_string(s.to_handle()).ok()?;
    parse_bigint_text(&text)
}

/// `StringToBigInt` on the text: optional whitespace, sign, then decimal or 0x/0o/0b digits.
fn parse_bigint_text(text: &str) -> Option<api::BigInt> {
    let t = text.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
    if t.is_empty() {
        return None;
    }
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if t.is_empty() {
        return None;
    }
    let (radix, digits) = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        (16, h)
    } else if let Some(o) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        (8, o)
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        (2, b)
    } else {
        (10, t)
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let bi = api::BigInt::parse_bytes(digits.as_bytes(), radix)?;
    Some(if neg { -bi } else { bi })
}

/// ECMA ToInt64 of a number (modulo 2^64).
fn to_int64(n: f64) -> i64 {
    if n.is_nan() || n.is_infinite() || n == 0.0 {
        return 0;
    }
    let t = n.trunc();
    if t >= -9223372036854775808.0 && t < 9223372036854775808.0 {
        return t as i64;
    }
    let m = t.rem_euclid(18446744073709551616.0);
    (m as u64) as i64
}

/// ECMA ToUint64 of a number (modulo 2^64).
fn to_uint64(n: f64) -> u64 {
    if n.is_nan() || n == 0.0 {
        return 0;
    }
    let m = n.trunc().rem_euclid(18446744073709551616.0);
    m as u64
}

// ---- Conversions and comparisons ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ToNumberSlow(cx: *mut JSContext, v: JS::HandleValue, dp: *mut f64) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let h = unsafe { val(cx, v) };
    let ok = match api::to_number(bs, h) {
        Ok(n) => {
            unsafe { *dp = n.as_number() };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ToBooleanSlow(v: JS::HandleValue) -> bool {
    api::to_boolean(BsValue::from_raw_bits(non_zero(unsafe { (*v.ptr).asBits_ })))
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ToInt64Slow(cx: *mut JSContext, v: JS::HandleValue, out: *mut i64) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let h = unsafe { val(cx, v) };
    let ok = match api::to_number(bs, h) {
        Ok(n) => {
            unsafe { *out = to_int64(n.as_number()) };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ToUint64Slow(cx: *mut JSContext, v: JS::HandleValue, out: *mut u64) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let h = unsafe { val(cx, v) };
    let ok = match api::to_number(bs, h) {
        Ok(n) => {
            unsafe { *out = to_uint64(n.as_number()) };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ToObjectSlow(cx: *mut JSContext, v: JS::HandleValue, _reportScanStack: bool) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let h = unsafe { val(cx, v) };
    let r = match api::to_object(bs, h) {
        Ok(o) => (*o).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ToPrimitive(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    hint: JSType,
    vp: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let preferred = match hint {
        JSType::JSTYPE_NUMBER => api::ToPrimitivePreferredType::Number,
        JSType::JSTYPE_STRING => api::ToPrimitivePreferredType::String,
        _ => api::ToPrimitivePreferredType::None,
    };
    let o = unsafe { obj_val(cx, *obj.ptr) };
    let r = api::to_primitive(bs, o, preferred);
    let ok = unsafe { finish(cx, r, vp.ptr) };
    scope.exit();
    ok
}

/// `JS::OrdinaryToPrimitive`: call `toString`/`valueOf` in hint order; the first non-object result wins.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__OrdinaryToPrimitive(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    type_: JSType,
    vp: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let methods: [&str; 2] = if type_ == JSType::JSTYPE_STRING { ["toString", "valueOf"] } else { ["valueOf", "toString"] };
    let r: Result<Handle<BsValue>, EvalError> = (|| {
        for name in methods {
            let key = api::property_key_from_string(bs, name)?;
            let f = api::get(bs, o, key)?;
            if api::is_callable(f) {
                let res = embed::call(bs, f, o.as_value(), &[])?;
                if !res.is_object() {
                    return Ok(res);
                }
            }
        }
        brimstone_core::runtime::error::type_error(bs, "Cannot convert object to primitive value")
    })();
    let ok = unsafe { finish(cx, r, vp.ptr) };
    scope.exit();
    ok
}

/// `JS::ToBigInt`: the BigInt conversion of a value, null with a pending exception on failure.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ToBigInt(cx: *mut JSContext, val_: JS::Handle<JS::Value>) -> *mut JS::BigInt {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let h = unsafe { val(cx, val_) };
    let v = *h;
    let r: Result<Handle<api::BigIntValue>, EvalError> = (|| {
        if v.is_bigint() {
            return Ok(v.as_bigint().to_handle());
        }
        if v.is_bool() {
            return api::bigint_from_i64(bs, if v.as_bool() { 1 } else { 0 }).map_err(|_| embed::uncatchable());
        }
        if v.is_number() {
            return match api::bigint_from_f64(bs, v.as_number()) {
                Ok(Some(b)) => Ok(b),
                _ => brimstone_core::runtime::error::range_error(bs, "can't convert non-integer number to BigInt"),
            };
        }
        if v.is_string() {
            return match parse_bigint_opt(bs, v.as_string()) {
                Some(bi) => api::BigIntValue::new(bs, bi).map_err(|_| embed::uncatchable()),
                None => brimstone_core::runtime::error::syntax_error(bs, "can't convert string to BigInt"),
            };
        }
        brimstone_core::runtime::error::type_error(bs, "can't convert value to BigInt")
    })();
    let r = match r {
        Ok(b) => (*b).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SameValue(
    cx: *mut JSContext,
    v1: JS::Handle<JS::Value>,
    v2: JS::Handle<JS::Value>,
    same: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let a = unsafe { val(cx, v1) };
    let b = unsafe { val(cx, v2) };
    let ok = match api::same_value(a, b) {
        Ok(s) => {
            unsafe { *same = s };
            true
        },
        Err(_) => unsafe { fail(cx, embed::uncatchable()) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__StrictlyEqual(
    cx: *mut JSContext,
    v1: JS::Handle<JS::Value>,
    v2: JS::Handle<JS::Value>,
    equal: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let a = unsafe { val(cx, v1) };
    let b = unsafe { val(cx, v2) };
    unsafe { *equal = strict_equal(bs, a, b) };
    scope.exit();
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__LooselyEqual(
    cx: *mut JSContext,
    v1: JS::Handle<JS::Value>,
    v2: JS::Handle<JS::Value>,
    equal: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let a = unsafe { val(cx, v1) };
    let b = unsafe { val(cx, v2) };
    let ok = match unsafe { loose_equal(bs, a, b) } {
        Ok(e) => {
            unsafe { *equal = e };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_TypeOfValue(cx: *mut JSContext, v: JS::Handle<JS::Value>) -> JSType {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let h = unsafe { val(cx, v) };
    let t = type_of(bs, *h);
    scope.exit();
    t
}

/// The predicate SpiderMonkey calls `js::IsHTMLDDA` / `js::EmulatesUndefined`: does this value carry the
/// [[IsHTMLDDA]] slot (`JSCLASS_EMULATES_UNDEFINED`)? It is not the same question as "is this value undefined":
/// an emulating object is an object, so `??`, `?.` and the bindings' missing-value tests must keep answering that
/// it is present. Neither name appears in `bindgen/jsapi.rs`, so the generator has nothing to re-export and this is
/// the Rust-side form; the C entry points that report type and truthiness (`JS_TypeOfValue`, `js__ToBooleanSlow`,
/// `JS__LooselyEqual`) agree with it because they read the same slot.
pub fn emulates_undefined(v: JS::Value) -> bool {
    let value = BsValue::from_raw_bits(non_zero(v.asBits_));
    value.is_object() && value.is_htmldda()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ValueToObject(
    cx: *mut JSContext,
    v: JS::HandleValue,
    objp: JS::MutableHandleObject,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let h = unsafe { val(cx, v) };
    let ok = match api::to_object(bs, h) {
        Ok(o) => {
            unsafe { (*objp.ptr) = (*o).as_ptr().cast() };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

/// Escape string code units into `out` as a JS string literal body (without the quotes).
fn push_escaped(out: &mut String, units: &[u16]) {
    for &u in units {
        match u {
            0x22 => out.push_str("\\\""),
            0x5C => out.push_str("\\\\"),
            0x08 => out.push_str("\\b"),
            0x0C => out.push_str("\\f"),
            0x0A => out.push_str("\\n"),
            0x0D => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x0B => out.push_str("\\v"),
            c if c < 0x20 => out.push_str(&format!("\\u{c:04x}")),
            c => out.push(char::from_u32(c as u32).unwrap_or('\u{FFFD}')),
        }
    }
}

/// `JS_ValueToSource`: a string value is quoted and escaped, anything else is stringified.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ValueToSource(cx: *mut JSContext, v: JS::Handle<JS::Value>) -> *mut JSString {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let h = unsafe { val(cx, v) };
    let r: Result<Handle<api::FlatString>, EvalError> = (|| {
        if (*h).is_string() {
            let mut out = String::from("\"");
            push_escaped(&mut out, &unsafe { units_of(cx, (*h).as_string().as_ptr().cast()) }.unwrap_or_default());
            out.push('"');
            return api::string_from_utf8(bs, &out);
        }
        Ok(api::to_string(bs, h)?.flatten_to_handle()?)
    })();
    let r = match r {
        Ok(s) => (*s).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}


/// Whether the object is an arguments exotic object (mapped or unmapped).
fn is_arguments(o: &Handle<api::ObjectValue>) -> bool {
    o.is_arguments_object()
}

/// `JS::GetBuiltinClass`: the `ESClass` of an object, false with a TypeError for non-objects.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetBuiltinClass(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    cls: *mut js::ESClass,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let p = unsafe { *obj.ptr };
    let ok = if p.is_null() {
        unsafe { super::property::fail_type(cx, "GetBuiltinClass called on null") }
    } else {
        let o = unsafe { obj_h(cx, p) };
        use js::ESClass as E;
        let class = if o.as_opt::<api::ArrayObject>().is_some() {
            E::Array
        } else if is_arguments(&o) {
            E::Arguments
        } else if o.as_opt::<api::DateObject>().is_some() {
            E::Date
        } else if o.as_opt::<RegExpObject>().is_some() {
            E::RegExp
        } else if o.as_opt::<MapObject>().is_some() {
            E::Map
        } else if o.as_opt::<SetObject>().is_some() {
            E::Set
        } else if o.as_opt::<api::PromiseObject>().is_some() {
            E::Promise
        } else if o.as_opt::<ErrorObject>().is_some() {
            E::Error
        } else if o.as_opt::<BigIntObject>().is_some() {
            E::BigInt
        } else if o.is_callable() {
            E::Function
        } else {
            E::Other
        };
        unsafe { *cls = class };
        true
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__IsArgumentsObject(obj: JS::HandleObject) -> bool {
    let bs = bs_of(super::cx::current_raw());
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    let o = BsValue::object(object_ptr(unsafe { *obj.ptr })).to_handle(bs).as_object();
    is_arguments(&o)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsArray(cx: *mut JSContext, obj: JS::Handle<*mut JSObject>, isArray: *mut bool) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_val(cx, *obj.ptr) };
    let ok = match api::is_array(bs, o) {
        Ok(b) => {
            unsafe { *isArray = b };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsArray1(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    answer: *mut JS::IsArrayAnswer,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_val(cx, *obj.ptr) };
    let ok = match api::is_array(bs, o) {
        Ok(true) => {
            unsafe { *answer = JS::IsArrayAnswer::Array };
            true
        },
        Ok(false) => {
            unsafe { *answer = JS::IsArrayAnswer::NotArray };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

/// `JS::IsArrayObject`: a real `ArrayObject`, not a proxy.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsArrayObject1(cx: *mut JSContext, obj: JS::Handle<*mut JSObject>, isArray: *mut bool) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    unsafe { *isArray = o.as_opt::<api::ArrayObject>().is_some() };
    scope.exit();
    true
}

/// `JS_IdToProtoKey`: a string id naming a standard class maps to its `JSProtoKey`.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_IdToProtoKey(cx: *mut JSContext, id: JS::HandleId) -> JSProtoKey {
    use JSProtoKey as P;
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let bits = id_bits_of(id);
    let key = if id_bits::is_gcthing(bits) {
        let v = BsValue::from_raw_bits(bits);
        if v.is_string() {
            let h = v.to_handle(bs).as_string();
            // SAFETY: the chars are copied into a String at once; nothing in the match allocates on the GC heap.
            h.flatten_to_handle().ok().and_then(|f| match unsafe { super::string::flat_chars(*f) } {
                api::StringChars::Latin1(b) => ::std::str::from_utf8(b).ok().map(|s| s.to_string()),
                api::StringChars::TwoByte(u) => Some(String::from_utf16_lossy(u)),
            })
        } else {
            None
        }
    } else {
        None
    };
    scope.exit();
    match key.as_deref() {
        Some("Object") => P::JSProto_Object,
        Some("Function") => P::JSProto_Function,
        Some("Array") => P::JSProto_Array,
        Some("Boolean") => P::JSProto_Boolean,
        Some("JSON") => P::JSProto_JSON,
        Some("Date") => P::JSProto_Date,
        Some("Math") => P::JSProto_Math,
        Some("Number") => P::JSProto_Number,
        Some("String") => P::JSProto_String,
        Some("RegExp") => P::JSProto_RegExp,
        Some("Error") => P::JSProto_Error,
        Some("AggregateError") => P::JSProto_AggregateError,
        Some("EvalError") => P::JSProto_EvalError,
        Some("RangeError") => P::JSProto_RangeError,
        Some("ReferenceError") => P::JSProto_ReferenceError,
        Some("SuppressedError") => P::JSProto_SuppressedError,
        Some("SyntaxError") => P::JSProto_SyntaxError,
        Some("TypeError") => P::JSProto_TypeError,
        Some("URIError") => P::JSProto_URIError,
        Some("ArrayBuffer") => P::JSProto_ArrayBuffer,
        Some("Int8Array") => P::JSProto_Int8Array,
        Some("Uint8Array") => P::JSProto_Uint8Array,
        Some("Int16Array") => P::JSProto_Int16Array,
        Some("Uint16Array") => P::JSProto_Uint16Array,
        Some("Int32Array") => P::JSProto_Int32Array,
        Some("Uint32Array") => P::JSProto_Uint32Array,
        Some("Float32Array") => P::JSProto_Float32Array,
        Some("Float64Array") => P::JSProto_Float64Array,
        Some("Uint8ClampedArray") => P::JSProto_Uint8ClampedArray,
        Some("BigInt64Array") => P::JSProto_BigInt64Array,
        Some("BigUint64Array") => P::JSProto_BigUint64Array,
        Some("Float16Array") => P::JSProto_Float16Array,
        Some("Map") => P::JSProto_Map,
        Some("Set") => P::JSProto_Set,
        Some("WeakMap") => P::JSProto_WeakMap,
        Some("WeakSet") => P::JSProto_WeakSet,
        Some("Promise") => P::JSProto_Promise,
        Some("Proxy") => P::JSProto_Proxy,
        Some("BigInt") => P::JSProto_BigInt,
        _ => P::JSProto_Null,
    }
}

/// `js::StringIsArrayIndex` on a linear string: a canonical array-index spelling becomes the index.
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__StringIsArrayIndex(str_: *const JSLinearString, indexp: *mut u32) -> bool {
    let p = HeapPtr::<api::FlatString>::from_ptr(str_ as *mut api::FlatString);
    // SAFETY: the index is parsed from the chars at once; nothing in the match allocates.
    let r = match unsafe { super::string::flat_chars(p) } {
        api::StringChars::Latin1(b) => id_bits::parse_index_latin1(b),
        api::StringChars::TwoByte(u) => id_bits::parse_index_utf16(u),
    };
    match r {
        Some(i) => {
            unsafe { *indexp = i };
            true
        },
        None => false,
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__StringIsArrayIndex1(str_: *const u16, length: u32, indexp: *mut u32) -> bool {
    let s = unsafe { ::std::slice::from_raw_parts(str_, length as usize) };
    match id_bits::parse_index_utf16(s) {
        Some(i) => {
            unsafe { *indexp = i };
            true
        },
        None => false,
    }
}

// ---- JSON ----

/// Call a method of the realm's `JSON` object.
unsafe fn call_json_method(cx: *mut JSContext, name: &str, args: &[Handle<BsValue>]) -> Result<Handle<BsValue>, EvalError> {
    let bs = unsafe { cx_of(cx) }.bs;
    let realm = embed::current_realm(bs);
    let json = api::intrinsic(realm, api::Intrinsic::JSON);
    let key = api::property_key_from_string(bs, name)?;
    let f = api::get(bs, json, key)?;
    embed::call(bs, f, json.as_value(), args)
}

/// Feed a stringified result to the `JSONWriteCallback` (once, with the whole text, as SpiderMonkey may chunk it).
unsafe fn feed_callback(callback: JSONWriteCallback, data: *mut c_void, s: Handle<BsValue>) -> bool {
    let Some(cb) = callback else { return true };
    if !(*s).is_string() {
        return true;
    }
    let h = (*s).as_string();
    match h.to_handle().flatten_to_handle() {
        Ok(f) => {
            // SAFETY: the chars are copied out before Servo's callback runs: the callback could reach the engine and GC.
            let units: Vec<u16> = match unsafe { super::string::flat_chars(*f) } {
                api::StringChars::Latin1(b) => b.iter().map(|&c| c as u16).collect(),
                api::StringChars::TwoByte(u) => u.to_vec(),
            };
            unsafe { cb(units.as_ptr(), units.len() as u32, data) }
        },
        Err(_) => false, // out of memory: the caller fails with nothing pending, as SpiderMonkey
    }
}

unsafe fn parse_json_string(cx: *mut JSContext, s: Handle<api::StringValue>, reviver: Option<Handle<BsValue>>, vp: JS::MutableHandle<JS::Value>) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let sv = s.as_value();
    let mut args = vec![sv];
    if let Some(r) = reviver {
        args.push(r);
    }
    let r = call_json_method(cx, "parse", &args);
    let ok = unsafe { finish(cx, r, vp.ptr) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ParseJSON(cx: *mut JSContext, chars: *const u16, len: u32, vp: JS::MutableHandle<JS::Value>) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let units = if len == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(chars, len as usize) } };
    let r = api::string_from_code_units(bs, units).map(|s| s.as_string());
    let ok = match r {
        Ok(s) => unsafe { parse_json_string(cx, s, None, vp) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ParseJSON1(cx: *mut JSContext, str_: JS::Handle<*mut JSString>, vp: JS::MutableHandle<JS::Value>) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let s = BsValue::string(str_ptr(unsafe { *str_.ptr })).to_handle(bs).as_string();
    let ok = unsafe { parse_json_string(cx, s, None, vp) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ParseJSON2(cx: *mut JSContext, chars: *const JS::Latin1Char, len: u32, vp: JS::MutableHandle<JS::Value>) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let bytes = if len == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(chars, len as usize) } };
    let r = api::string_from_latin1(bs, bytes).map(|s| s.as_string());
    let ok = match r {
        Ok(s) => unsafe { parse_json_string(cx, s, None, vp) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ParseJSONWithReviver(
    cx: *mut JSContext,
    chars: *const u16,
    len: u32,
    reviver: JS::Handle<JS::Value>,
    vp: JS::MutableHandle<JS::Value>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let units = if len == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(chars, len as usize) } };
    let r = api::string_from_code_units(bs, units).map(|s| s.as_string());
    let ok = match r {
        Ok(s) => unsafe { parse_json_string(cx, s, Some(val(cx, reviver)), vp) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ParseJSONWithReviver1(
    cx: *mut JSContext,
    str_: JS::Handle<*mut JSString>,
    reviver: JS::Handle<JS::Value>,
    vp: JS::MutableHandle<JS::Value>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let s = BsValue::string(str_ptr(unsafe { *str_.ptr })).to_handle(bs).as_string();
    let ok = unsafe { parse_json_string(cx, s, Some(val(cx, reviver)), vp) };
    scope.exit();
    ok
}

unsafe fn stringify_impl(
    cx: *mut JSContext,
    value: JS::MutableHandle<JS::Value>,
    replacer: JS::Handle<*mut JSObject>,
    space: JS::Handle<JS::Value>,
    callback: JSONWriteCallback,
    data: *mut c_void,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let mut args = vec![unsafe { val(cx, JS::Handle { _phantom_0: Default::default(), ptr: value.ptr }) }];
    if !replacer.ptr.is_null() {
        let rp = unsafe { *replacer.ptr };
        if !rp.is_null() {
            args.push(unsafe { obj_val(cx, rp) });
        }
    }
    if !space.ptr.is_null() {
        let sp = unsafe { (*space.ptr).asBits_ };
        if sp != 0 {
            args.push(unsafe { val(cx, space) });
        }
    }
    let r = call_json_method(cx, "stringify", &args);
    let ok = match r {
        Ok(v) => {
            let fed = unsafe { feed_callback(callback, data, v) };
            unsafe { (*value.ptr).asBits_ = v.as_raw_bits() };
            fed
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_Stringify(
    cx: *mut JSContext,
    value: JS::MutableHandle<JS::Value>,
    replacer: JS::Handle<*mut JSObject>,
    space: JS::Handle<JS::Value>,
    callback: JSONWriteCallback,
    data: *mut c_void,
) -> bool {
    unsafe { stringify_impl(cx, value, replacer, space, callback, data) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_StringifyWithLengthHint(
    cx: *mut JSContext,
    value: JS::MutableHandle<JS::Value>,
    replacer: JS::Handle<*mut JSObject>,
    space: JS::Handle<JS::Value>,
    callback: JSONWriteCallback,
    data: *mut c_void,
    _lengthHint: usize,
) -> bool {
    unsafe { stringify_impl(cx, value, replacer, space, callback, data) }
}

/// `JS::ToJSON`: call the value's `toJSON` if it has one, then serialize with the callback.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ToJSON(
    cx: *mut JSContext,
    value: JS::Handle<JS::Value>,
    replacer: JS::Handle<*mut JSObject>,
    space: JS::Handle<JS::Value>,
    callback: JSONWriteCallback,
    data: *mut c_void,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let h = unsafe { val(cx, value) };
    let r: Result<Handle<BsValue>, EvalError> = (|| {
        let mut v = h;
        if (*v).is_object() {
            let key = api::property_key_from_string(bs, "toJSON")?;
            let f = api::get(bs, (*v).as_object().to_handle(), key)?;
            if api::is_callable(f) {
                let empty = api::string_from_latin1(bs, b"")?.as_value();
                v = embed::call(bs, f, v, &[empty])?;
            }
        }
        let mut args = vec![v];
        let rp = unsafe { *replacer.ptr };
        if !rp.is_null() {
            args.push(unsafe { obj_val(cx, rp) });
        }
        let sp = unsafe { (*space.ptr).asBits_ };
        if sp != 0 {
            args.push(unsafe { val(cx, space) });
        }
        call_json_method(cx, "stringify", &args)
    })();
    let ok = match r {
        Ok(v) => unsafe { feed_callback(callback, data, v) },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

/// `JS::ToJSONMaybeSafely`: serialize a plain object or array without running any user code
/// (no `toJSON`, no getters): data properties only, as SpiderMonkey's safe serializer does.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ToJSONMaybeSafely(
    cx: *mut JSContext,
    input: JS::Handle<*mut JSObject>,
    callback: JSONWriteCallback,
    data: *mut c_void,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *input.ptr) };
    let r: Result<String, EvalError> = (|| {
        let mut out = String::new();
        safe_serialize(bs, o.as_value(), 0, &mut out)?;
        Ok(out)
    })();
    let ok = match r {
        Ok(text) => {
            let units: Vec<u16> = text.encode_utf16().collect();
            match callback {
                Some(cb) => unsafe { cb(units.as_ptr(), units.len() as u32, data) },
                None => true,
            }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

unsafe fn safe_serialize(bs: Bs, v: Handle<BsValue>, depth: usize, out: &mut String) -> Result<(), EvalError> {
    if depth > 100 {
        return brimstone_core::runtime::error::type_error(bs, "too much recursion in ToJSONMaybeSafely");
    }
    let x = *v;
    if x.is_null() {
        out.push_str("null");
    } else if x.is_bool() {
        out.push_str(if x.as_bool() { "true" } else { "false" });
    } else if x.is_number() {
        let n = x.as_number();
        if n.is_finite() {
            let s = api::to_string(bs, v)?;
            out.push_str(&api::string_to_rust_string(s)?);
        } else {
            out.push_str("null");
        }
    } else if x.is_string() {
        let s = api::to_string(bs, v)?;
        let text = api::string_to_rust_string(s)?;
        out.push('"');
        push_escaped(out, &text.encode_utf16().collect::<Vec<u16>>());
        out.push('"');
    } else if x.is_object() {
        let o = x.as_object();
        let is_array = api::is_array(bs, v)?;
        if is_array {
            let len = api::length_of_array_like(bs, o.to_handle())?;
            out.push('[');
            for i in 0..len {
                if i > 0 {
                    out.push(',');
                }
                let key = api::property_key_from_index(bs, i as u32).map_err(|_| embed::uncatchable())?;
                let val = api::get(bs, o.to_handle(), key)?;
                if val.is_undefined() || val.is_symbol() || (val.is_object() && val.as_object().is_callable()) {
                    out.push_str("null");
                } else {
                    safe_serialize(bs, val, depth + 1, out)?;
                }
            }
            out.push(']');
        } else {
            out.push('{');
            let keys = api::own_property_keys(bs, o.to_handle())?;
            let mut first = true;
            for k in keys {
                let key = super::property::id_in(bs, (*k).as_raw_bits());
                if key.is_symbol() {
                    continue;
                }
                let desc = api::get_own_property_descriptor(bs, o.to_handle(), key)?;
                let Some(d) = desc else { continue };
                if !d.is_enumerable.unwrap_or(false) || d.value.is_none() {
                    continue;
                }
                if !first {
                    out.push(',');
                }
                first = false;
                let name = api::string_to_rust_string(api::to_string(bs, k.cast())?)?;
                out.push('"');
                push_escaped(out, &name.encode_utf16().collect::<Vec<u16>>());
                out.push_str(":\"");
                out.truncate(out.len() - 2);
                out.push('"');
                out.push(':');
                safe_serialize(bs, d.value.unwrap(), depth + 1, out)?;
            }
            out.push('}');
        }
    } else {
        // BigInt throws; undefined/symbol/function have no JSON representation (callers map those to null/omission).
        if x.is_bigint() {
            return brimstone_core::runtime::error::type_error(bs, "BigInt is not serializable to JSON");
        }
        out.push_str("null");
    }
    Ok(())
}

// ---- BigInt ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__detail__BigIntFromInt64(cx: *mut JSContext, num: i64) -> *mut JS::BigInt {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = match api::bigint_from_i64(bs, num) {
        Ok(b) => (*b).as_ptr().cast(),
        Err(_) => {
            unsafe { fail(cx, embed::uncatchable()) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__detail__BigIntFromUint64(cx: *mut JSContext, num: u64) -> *mut JS::BigInt {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = match api::bigint_from_u64(bs, num) {
        Ok(b) => (*b).as_ptr().cast(),
        Err(_) => {
            unsafe { fail(cx, embed::uncatchable()) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__detail__BigIntFromBool(cx: *mut JSContext, b: bool) -> *mut JS::BigInt {
    unsafe { JS__detail__BigIntFromInt64(cx, if b { 1 } else { 0 }) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NumberToBigInt(cx: *mut JSContext, num: f64) -> *mut JS::BigInt {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = match api::bigint_from_f64(bs, num) {
        Ok(Some(b)) => (*b).as_ptr().cast(),
        Ok(None) => {
            if let Err(e) = brimstone_core::runtime::error::range_error::<()>(bs, "can't convert non-integer number to BigInt") {
                unsafe { fail(cx, e) };
            }
            ::std::ptr::null_mut()
        },
        Err(_) => {
            unsafe { fail(cx, embed::uncatchable()) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}

unsafe fn string_to_bigint(cx: *mut JSContext, s: Handle<api::StringValue>) -> *mut JS::BigInt {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = match parse_bigint_opt(bs, *s) {
        Some(bi) => match api::BigIntValue::new(bs, bi) {
            Ok(b) => (*b).as_ptr().cast(),
            Err(_) => {
                unsafe { fail(cx, embed::uncatchable()) };
                ::std::ptr::null_mut()
            },
        },
        None => {
            if let Err(e) = brimstone_core::runtime::error::syntax_error::<()>(bs, "can't convert string to BigInt") {
                unsafe { fail(cx, e) };
            }
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__StringToBigInt(cx: *mut JSContext, chars: *const mozilla::Range<JS::Latin1Char>) -> *mut JS::BigInt {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = unsafe { &(*chars) };
    let start = r.mStart.mPtr;
    let len = unsafe { r.mEnd.mPtr.offset_from(start) } as usize;
    let bytes = if len == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(start, len) } };
    let s = match api::string_from_latin1(bs, bytes) {
        Ok(s) => s.as_string(),
        Err(e) => {
            unsafe { fail(cx, e) };
            scope.exit();
            return ::std::ptr::null_mut();
        },
    };
    let out = unsafe { string_to_bigint(cx, s) };
    scope.exit();
    out
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__StringToBigInt1(cx: *mut JSContext, chars: *const mozilla::Range<u16>) -> *mut JS::BigInt {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = unsafe { &(*chars) };
    let start = r.mStart.mPtr;
    let len = unsafe { r.mEnd.mPtr.offset_from(start) } as usize;
    let units = if len == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(start, len) } };
    let s = match api::string_from_code_units(bs, units) {
        Ok(s) => s.as_string(),
        Err(e) => {
            unsafe { fail(cx, e) };
            scope.exit();
            return ::std::ptr::null_mut();
        },
    };
    let out = unsafe { string_to_bigint(cx, s) };
    scope.exit();
    out
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__BigIntToString(cx: *mut JSContext, bi: JS::Handle<*mut JS::BigInt>, radix: u8) -> *mut JSString {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let b = BsValue::bigint(HeapPtr::from_ptr(unsafe { *bi.ptr }.cast())).to_handle(bs).as_bigint();
    let r = match api::bigint_to_string(bs, b, radix as u32) {
        Ok(s) => (*s).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}

// ---- Symbols ----

fn well_known(code: JS::SymbolCode) -> Option<api::WellKnownSymbol> {
    use api::WellKnownSymbol as W;
    match code as u32 {
        0 => Some(W::IsConcatSpreadable),
        1 => Some(W::Iterator),
        2 => Some(W::Match),
        3 => Some(W::Replace),
        4 => Some(W::Search),
        5 => Some(W::Species),
        6 => Some(W::HasInstance),
        7 => Some(W::Split),
        8 => Some(W::ToPrimitive),
        9 => Some(W::ToStringTag),
        10 => Some(W::Unscopables),
        11 => Some(W::AsyncIterator),
        12 => Some(W::MatchAll),
        _ => None,
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetWellKnownSymbol(cx: *mut JSContext, which: JS::SymbolCode) -> *mut JS::Symbol {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = match well_known(which) {
        Some(w) => api::well_known_symbol(bs, w).as_ptr().cast(),
        None => ::std::ptr::null_mut(),
    };
    scope.exit();
    r
}

#[super::poison::engine_shim(failure = JS::PropertyKey { asBits_: id_bits::void_id() as usize })]
pub unsafe extern "C" fn JS__GetWellKnownSymbolKey(cx: *mut JSContext, which: JS::SymbolCode) -> JS::PropertyKey {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = match well_known(which) {
        Some(w) => {
            let sym = api::well_known_symbol(bs, w);
            JS::PropertyKey { asBits_: PropertyKey::symbol(sym).to_raw_value().as_raw_bits() as usize }
        },
        None => JS::PropertyKey { asBits_: 0 },
    };
    scope.exit();
    r
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewSymbol(cx: *mut JSContext, description: JS::Handle<*mut JSString>) -> *mut JS::Symbol {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let dp = unsafe { *description.ptr };
    let desc = if dp.is_null() {
        None
    } else {
        unsafe { units_of(cx, dp) }.map(|u| String::from_utf16_lossy(&u))
    };
    let r = match api::new_symbol(bs, desc.as_deref()) {
        Ok(s) => (*s).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetSymbolFor(cx: *mut JSContext, key: JS::Handle<*mut JSString>) -> *mut JS::Symbol {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let kp = unsafe { *key.ptr };
    let r = match unsafe { units_of(cx, kp) } {
        Some(u) => match api::symbol_for(bs, &String::from_utf16_lossy(&u)) {
            Ok(s) => (*s).as_ptr().cast(),
            Err(e) => {
                unsafe { fail(cx, e) };
                ::std::ptr::null_mut()
            },
        },
        None => ::std::ptr::null_mut(),
    };
    scope.exit();
    r
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetSymbolDescription(symbol: JS::Handle<*mut JS::Symbol>) -> *mut JSString {
    let sp = unsafe { *symbol.ptr };
    if sp.is_null() {
        return ::std::ptr::null_mut();
    }
    let bs = embed::context_of(sp.cast());
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    let sym = BsValue::symbol(HeapPtr::from_ptr(sp.cast())).to_handle(bs).as_symbol();
    match api::symbol_description(sym) {
        Some(d) => (*d).as_ptr().cast(),
        None => ::std::ptr::null_mut(),
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetSymbolCode(symbol: JS::Handle<*mut JS::Symbol>) -> JS::SymbolCode {
    use JS::SymbolCode as S;
    let sp = unsafe { *symbol.ptr };
    if sp.is_null() {
        return S::UniqueSymbol;
    }
    let bs = embed::context_of(sp.cast());
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    let sym = BsValue::symbol(HeapPtr::from_ptr(sp.cast())).to_handle(bs).as_symbol();
    for code in 0..13u32 {
        let Some(w) = well_known(unsafe { ::std::mem::transmute(code) }) else { continue };
        if api::well_known_symbol(bs, w).ptr_eq(&sym) {
            return unsafe { ::std::mem::transmute(code) };
        }
    }
    if api::symbol_key_for(bs, sym).is_some() {
        S::InSymbolRegistry
    } else {
        S::UniqueSymbol
    }
}

// ---- Date ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewDateObject(cx: *mut JSContext, time: JS::ClippedTime) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let r = match api::new_date(bs, realm, time.t) {
        Ok(d) => (*d).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}

/// Days from 1970-01-01 to the given civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `new Date(year, mon, mday, hour, min, sec)`: the components as milliseconds since the epoch.
fn date_parts_to_ms(year: i32, mon: i32, mday: i32, hour: i32, min: i32, sec: i32) -> f64 {
    let year = if (0..=99).contains(&year) { 1900 + year } else { year } as i64;
    let month = mon as i64; // 0-based, may be out of range; normalize
    let y = year + month.div_euclid(12);
    let m = month.rem_euclid(12) + 1;
    let days = days_from_civil(y, m, mday as i64);
    (days * 86_400_000 + hour as i64 * 3_600_000 + min as i64 * 60_000 + sec as i64 * 1_000) as f64
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewDateObject1(
    cx: *mut JSContext,
    year: ::std::os::raw::c_int,
    mon: ::std::os::raw::c_int,
    mday: ::std::os::raw::c_int,
    hour: ::std::os::raw::c_int,
    min: ::std::os::raw::c_int,
    sec: ::std::os::raw::c_int,
) -> *mut JSObject {
    unsafe { JS__NewDateObject(cx, JS::ClippedTime { t: date_parts_to_ms(year, mon, mday, hour, min, sec) }) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ObjectIsDate(cx: *mut JSContext, obj: JS::Handle<*mut JSObject>, isDate: *mut bool) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    unsafe { *isDate = api::date_value(o).is_some() };
    scope.exit();
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__DateGetMsecSinceEpoch(cx: *mut JSContext, obj: JS::HandleObject, msecSinceEpoch: *mut f64) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let ok = match api::date_value(o) {
        Some(ms) => {
            unsafe { *msecSinceEpoch = ms };
            true
        },
        None => unsafe { super::property::fail_type(cx, "DateGetMsecSinceEpoch called on non-Date") },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__DateIsValid(cx: *mut JSContext, obj: JS::HandleObject, isValid: *mut bool) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let ok = match api::date_value(o) {
        Some(ms) => {
            unsafe { *isValid = !ms.is_nan() };
            true
        },
        None => unsafe { super::property::fail_type(cx, "DateIsValid called on non-Date") },
    };
    scope.exit();
    ok
}

/// `JS::IsISOStyleDate`: the string is an ISO-8601 date (optionally with time and zone).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsISOStyleDate(_cx: *mut JSContext, str_: *const JS::Latin1Chars) -> bool {
    let r = unsafe { &(*str_) };
    let start = r._base.mStart.mPtr;
    let len = unsafe { r._base.mEnd.mPtr.offset_from(start) } as usize;
    let bytes = if len == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(start, len) } };
    is_iso_style_date(bytes)
}

fn is_iso_style_date(b: &[u8]) -> bool {
    fn digits(b: &[u8], n: usize) -> bool {
        b.len() >= n && b[..n].iter().all(|c| c.is_ascii_digit())
    }
    // YYYY
    if !digits(b, 4) {
        return false;
    }
    let mut i = 4;
    if b.len() == i {
        return true;
    }
    if b[i] != b'-' || !digits(&b[i + 1..], 2) {
        return false;
    }
    i += 3;
    if b.len() == i {
        return true;
    }
    if b[i] != b'-' || !digits(&b[i + 1..], 2) {
        return false;
    }
    i += 3;
    if b.len() == i {
        return true;
    }
    // Time part: T HH : mm [ : ss [ . sss ] ] [ zone ]
    if b[i] != b'T' || !digits(&b[i + 1..], 2) {
        return false;
    }
    i += 3;
    if b.get(i) != Some(&b':') || !digits(&b[i + 1..], 2) {
        return false;
    }
    i += 3;
    if b.len() == i {
        return true;
    }
    if b[i] == b':' {
        if !digits(&b[i + 1..], 2) {
            return false;
        }
        i += 3;
        if b.len() == i {
            return true;
        }
        if b[i] == b'.' {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j == i + 1 {
                return false;
            }
            i = j;
            if b.len() == i {
                return true;
            }
        }
    }
    // Zone: Z or +-HH[:]mm
    match b[i] {
        b'Z' => b.len() == i + 1,
        b'+' | b'-' => {
            if !digits(&b[i + 1..], 2) {
                return false;
            }
            i += 3;
            if b.len() == i {
                return true;
            }
            if b[i] == b':' {
                i += 1;
            }
            digits(&b[i..], 2) && b.len() == i + 2
        },
        _ => false,
    }
}

// ---- RegExp ----

fn flags_to_string(flags: JS::RegExpFlags) -> String {
    let f = flags.flags_;
    let mut s = String::new();
    if f & JS::RegExpFlag_IgnoreCase != 0 {
        s.push('i');
    }
    if f & JS::RegExpFlag_Global != 0 {
        s.push('g');
    }
    if f & JS::RegExpFlag_Multiline != 0 {
        s.push('m');
    }
    if f & JS::RegExpFlag_Sticky != 0 {
        s.push('y');
    }
    if f & JS::RegExpFlag_Unicode != 0 {
        s.push('u');
    }
    if f & JS::RegExpFlag_DotAll != 0 {
        s.push('s');
    }
    if f & JS::RegExpFlag_UnicodeSets != 0 {
        s.push('v');
    }
    s
}

unsafe fn new_regexp(cx: *mut JSContext, pattern: Handle<api::StringValue>, flags: JS::RegExpFlags) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let ctor = api::intrinsic(realm, api::Intrinsic::RegExpConstructor);
    let r = api::string_from_latin1(bs, flags_to_string(flags).as_bytes())
        .map(|s| s.as_value())
        .and_then(|fs| api::construct(bs, ctor.cast(), &[pattern.cast(), fs], Some(ctor)));
    let r = match r {
        Ok(o) => (*o).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewRegExpObject(
    cx: *mut JSContext,
    bytes: *const c_char,
    length: usize,
    flags: JS::RegExpFlags,
) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let b = if length == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(bytes.cast::<u8>(), length) } };
    let r = api::string_from_latin1(bs, b).map(|s| s.as_string());
    let out = match r {
        Ok(s) => unsafe { new_regexp(cx, s, flags) },
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    out
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewUCRegExpObject(
    cx: *mut JSContext,
    chars: *const u16,
    length: usize,
    flags: JS::RegExpFlags,
) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let u = if length == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(chars, length) } };
    let r = api::string_from_code_units(bs, u).map(|s| s.as_string());
    let out = match r {
        Ok(s) => unsafe { new_regexp(cx, s, flags) },
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    out
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ObjectIsRegExp(cx: *mut JSContext, obj: JS::Handle<*mut JSObject>, isRegExp: *mut bool) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    unsafe { *isRegExp = o.as_opt::<RegExpObject>().is_some() };
    scope.exit();
    true
}

/// `JS::ExecuteRegExp` / `ExecuteRegExpNoStatics`: run `reobj.exec(input)` starting at `*indexp`.
unsafe fn execute_regexp(
    cx: *mut JSContext,
    reobj: JS::Handle<*mut JSObject>,
    chars: *const u16,
    length: usize,
    indexp: *mut usize,
    test: bool,
    rval: JS::MutableHandle<JS::Value>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let ro = unsafe { obj_h(cx, *reobj.ptr) };
    let u = if length == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(chars, length) } };
    let r: Result<bool, EvalError> = (|| {
        let input = api::string_from_code_units(bs, u)?.as_value();
        let li_key = api::property_key_from_string(bs, "lastIndex")?;
        let start = if indexp.is_null() { 0 } else { unsafe { *indexp } };
        api::set(bs, ro, li_key, BsValue::number(start as f64).to_handle(bs), true)?;
        let exec_key = api::property_key_from_string(bs, "exec")?;
        let exec = api::get(bs, ro, exec_key)?;
        let result = embed::call(bs, exec, ro.as_value(), &[input])?;
        if !indexp.is_null() {
            // SpiderMonkey reports the match start index, not lastIndex.
            if result.is_object() {
                let idx_key = api::property_key_from_string(bs, "index")?;
                if let Ok(iv) = api::get(bs, result.as_object(), idx_key) {
                    if iv.is_number() {
                        unsafe { *indexp = iv.as_number().max(0.0) as usize };
                    }
                }
            }
        }
        if test {
            unsafe { (*rval.ptr).asBits_ = BsValue::bool(!result.is_null()).as_raw_bits() };
        } else {
            unsafe { (*rval.ptr).asBits_ = result.as_raw_bits() };
        }
        Ok(true)
    })();
    let ok = match r {
        Ok(b) => b,
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ExecuteRegExp(
    cx: *mut JSContext,
    _obj: JS::Handle<*mut JSObject>,
    reobj: JS::Handle<*mut JSObject>,
    chars: *const u16,
    length: usize,
    indexp: *mut usize,
    test: bool,
    rval: JS::MutableHandle<JS::Value>,
) -> bool {
    unsafe { execute_regexp(cx, reobj, chars, length, indexp, test, rval) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ExecuteRegExpNoStatics(
    cx: *mut JSContext,
    reobj: JS::Handle<*mut JSObject>,
    chars: *const u16,
    length: usize,
    indexp: *mut usize,
    test: bool,
    rval: JS::MutableHandle<JS::Value>,
) -> bool {
    unsafe { execute_regexp(cx, reobj, chars, length, indexp, test, rval) }
}

/// `JS::CheckRegExpSyntax`: compile the pattern; on a syntax error, set `error` to the exception and return true.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CheckRegExpSyntax(
    cx: *mut JSContext,
    chars: *const u16,
    length: usize,
    flags: JS::RegExpFlags,
    error: JS::MutableHandle<JS::Value>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let u = if length == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(chars, length) } };
    let r = api::string_from_code_units(bs, u).map(|s| s.as_string());
    let ok = match r {
        Ok(s) => {
            let realm = embed::current_realm(bs);
            let ctor = api::intrinsic(realm, api::Intrinsic::RegExpConstructor);
            let fs = match api::string_from_latin1(bs, flags_to_string(flags).as_bytes()) {
                Ok(f) => f.as_value(),
                Err(e) => {
                    scope.exit();
                    return unsafe { fail(cx, e) };
                },
            };
            match api::construct(bs, ctor.cast(), &[s.cast(), fs], Some(ctor)) {
                Ok(_) => true,
                Err(e) => match embed::exception_value_opt(e) {
                    Some(v) => {
                        unsafe { (*error.ptr).asBits_ = v.as_raw_bits() };
                        false
                    },
                    None => {
                        scope.exit();
                        return unsafe { fail(cx, e) };
                    },
                },
            }
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetRegExpSource(cx: *mut JSContext, obj: JS::Handle<*mut JSObject>) -> *mut JSString {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let r = match o.as_opt::<RegExpObject>() {
        Some(re) => (*re.escaped_pattern_source()).as_ptr().cast(),
        None => ::std::ptr::null_mut(),
    };
    scope.exit();
    r
}

/// `JS_GetRegExpFlags`: the object's flags as `JS::RegExpFlags` bits.
#[super::poison::engine_shim(failure = unsafe { (*flags).flags_ = 0 })]
pub unsafe extern "C" fn glue__JS_GetRegExpFlags(cx: *mut JSContext, obj: JS::HandleObject, flags: *mut JS::RegExpFlags) {
    use brimstone_core::parser::regexp::RegExpFlags as F;
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let mut out = 0u8;
    if let Some(re) = o.as_opt::<RegExpObject>() {
        let f = re.flags();
        if f.contains(F::IGNORE_CASE) {
            out |= JS::RegExpFlag_IgnoreCase;
        }
        if f.contains(F::GLOBAL) {
            out |= JS::RegExpFlag_Global;
        }
        if f.contains(F::MULTILINE) {
            out |= JS::RegExpFlag_Multiline;
        }
        if f.contains(F::STICKY) {
            out |= JS::RegExpFlag_Sticky;
        }
        if f.contains(F::UNICODE_AWARE) {
            out |= JS::RegExpFlag_Unicode;
        }
        if f.contains(F::DOT_ALL) {
            out |= JS::RegExpFlag_DotAll;
        }
        if f.contains(F::UNICODE_SETS) {
            out |= JS::RegExpFlag_UnicodeSets;
        }
    }
    unsafe { (*flags).flags_ = out };
    scope.exit();
}

/// Brimstone keeps no cross-call RegExp statics, so there is nothing to clear.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ClearRegExpStatics(_cx: *mut JSContext, _obj: JS::Handle<*mut JSObject>) -> bool {
    true
}

/// Brimstone has no legacy `RegExp.input` static; accepted and ignored.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetRegExpInput(
    _cx: *mut JSContext,
    _obj: JS::Handle<*mut JSObject>,
    _input: JS::Handle<*mut JSString>,
) -> bool {
    true
}

// ---- Map, Set, WeakMap ----

/// Call a method of a collection object with itself as `this`.
unsafe fn call_method(
    cx: *mut JSContext,
    obj: Handle<api::ObjectValue>,
    name: &str,
    args: &[Handle<BsValue>],
) -> Result<Handle<BsValue>, EvalError> {
    let bs = bs_of(cx);
    let key = api::property_key_from_string(bs, name)?;
    let f = api::get(bs, obj, key)?;
    embed::call(bs, f, obj.as_value(), args)
}

unsafe fn new_collection(cx: *mut JSContext, intrinsic: api::Intrinsic) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let ctor = api::intrinsic(realm, intrinsic);
    let r = match api::construct(bs, ctor.cast(), &[], Some(ctor)) {
        Ok(o) => (*o).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewMapObject(cx: *mut JSContext) -> *mut JSObject {
    unsafe { new_collection(cx, api::Intrinsic::MapConstructor) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewSetObject(cx: *mut JSContext) -> *mut JSObject {
    unsafe { new_collection(cx, api::Intrinsic::SetConstructor) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewWeakMapObject(cx: *mut JSContext) -> *mut JSObject {
    unsafe { new_collection(cx, api::Intrinsic::WeakMapConstructor) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsMapObject(cx: *mut JSContext, obj: JS::HandleObject, isMap: *mut bool) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    unsafe { *isMap = o.as_opt::<MapObject>().is_some() };
    scope.exit();
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsSetObject(cx: *mut JSContext, obj: JS::HandleObject, isSet: *mut bool) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    unsafe { *isSet = o.as_opt::<SetObject>().is_some() };
    scope.exit();
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsWeakMapObject(obj: *mut JSObject) -> bool {
    if obj.is_null() {
        return false;
    }
    let bs = embed::context_of(obj.cast());
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    BsValue::object(object_ptr(obj)).to_handle(bs).as_object().as_opt::<WeakMapObject>().is_some()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__MapSize(cx: *mut JSContext, obj: JS::HandleObject) -> u32 {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let n = o.as_opt::<MapObject>().map(|m| m.map_data_inner().iter_gc_safe().count() as u32).unwrap_or(0);
    scope.exit();
    n
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetSize(cx: *mut JSContext, obj: JS::HandleObject) -> u32 {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let n = o.as_opt::<SetObject>().map(|s| s.set_data_inner().iter_gc_safe().count() as u32).unwrap_or(0);
    scope.exit();
    n
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__MapKeys(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    rval: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let r = call_method(cx, o, "keys", &[]);
    let ok = unsafe { finish(cx, r, rval.ptr) };
    scope.exit();
    ok
}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__MapValues(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    rval: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let r = call_method(cx, o, "values", &[]);
    let ok = unsafe { finish(cx, r, rval.ptr) };
    scope.exit();
    ok
}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__MapEntries(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    rval: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let r = call_method(cx, o, "entries", &[]);
    let ok = unsafe { finish(cx, r, rval.ptr) };
    scope.exit();
    ok
}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetKeys(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    rval: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let r = call_method(cx, o, "keys", &[]);
    let ok = unsafe { finish(cx, r, rval.ptr) };
    scope.exit();
    ok
}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetValues(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    rval: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let r = call_method(cx, o, "values", &[]);
    let ok = unsafe { finish(cx, r, rval.ptr) };
    scope.exit();
    ok
}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetEntries(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    rval: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let r = call_method(cx, o, "entries", &[]);
    let ok = unsafe { finish(cx, r, rval.ptr) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__MapHas(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    key: JS::HandleValue,
    rval: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let k = unsafe { val(cx, key) };
    let r = call_method(cx, o, "has", &[k]).map(|v| api::to_boolean(*v));
    let ok = match r {
        Ok(b) => {
            unsafe { *rval = b };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__MapDelete(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    key: JS::HandleValue,
    rval: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let k = unsafe { val(cx, key) };
    let r = call_method(cx, o, "delete", &[k]).map(|v| api::to_boolean(*v));
    let ok = match r {
        Ok(b) => {
            unsafe { *rval = b };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetHas(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    key: JS::HandleValue,
    rval: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let k = unsafe { val(cx, key) };
    let r = call_method(cx, o, "has", &[k]).map(|v| api::to_boolean(*v));
    let ok = match r {
        Ok(b) => {
            unsafe { *rval = b };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetDelete(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    key: JS::HandleValue,
    rval: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let k = unsafe { val(cx, key) };
    let r = call_method(cx, o, "delete", &[k]).map(|v| api::to_boolean(*v));
    let ok = match r {
        Ok(b) => {
            unsafe { *rval = b };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__MapGet(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    key: JS::HandleValue,
    rval: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let k = unsafe { val(cx, key) };
    let r = call_method(cx, o, "get", &[k]);
    let ok = unsafe { finish(cx, r, rval.ptr) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__MapGetOrInsert(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    key: JS::HandleValue,
    val_: JS::HandleValue,
    rval: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let k = unsafe { val(cx, key) };
    let v = unsafe { val(cx, val_) };
    let r = call_method(cx, o, "getOrInsert", &[k, v]);
    let ok = unsafe { finish(cx, r, rval.ptr) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__MapSet(cx: *mut JSContext, obj: JS::HandleObject, key: JS::HandleValue, val_: JS::HandleValue) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let k = unsafe { val(cx, key) };
    let v = unsafe { val(cx, val_) };
    let r = call_method(cx, o, "set", &[k, v]).map(|_| true);
    let ok = match r {
        Ok(b) => b,
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetAdd(cx: *mut JSContext, obj: JS::HandleObject, key: JS::HandleValue) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let k = unsafe { val(cx, key) };
    let r = call_method(cx, o, "add", &[k]).map(|_| true);
    let ok = match r {
        Ok(b) => b,
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__MapClear(cx: *mut JSContext, obj: JS::HandleObject) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let r = call_method(cx, o, "clear", &[]).map(|_| true);
    let ok = match r {
        Ok(b) => b,
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetClear(cx: *mut JSContext, obj: JS::HandleObject) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let r = call_method(cx, o, "clear", &[]).map(|_| true);
    let ok = match r {
        Ok(b) => b,
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__MapForEach(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    callbackFn: JS::HandleValue,
    thisVal: JS::HandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let cb = unsafe { val(cx, callbackFn) };
    let this = unsafe { val(cx, thisVal) };
    let r = call_method(cx, o, "forEach", &[cb, this]).map(|_| true);
    let ok = match r {
        Ok(b) => b,
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetForEach(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    callbackFn: JS::HandleValue,
    thisVal: JS::HandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let cb = unsafe { val(cx, callbackFn) };
    let this = unsafe { val(cx, thisVal) };
    let r = call_method(cx, o, "forEach", &[cb, this]).map(|_| true);
    let ok = match r {
        Ok(b) => b,
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetWeakMapEntry(
    cx: *mut JSContext,
    mapObj: JS::HandleObject,
    key: JS::HandleValue,
    val_: JS::MutableHandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *mapObj.ptr) };
    let k = unsafe { val(cx, key) };
    let r = call_method(cx, o, "get", &[k]);
    let ok = unsafe { finish(cx, r, val_.ptr) };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetWeakMapEntry(
    cx: *mut JSContext,
    mapObj: JS::HandleObject,
    key: JS::HandleValue,
    val_: JS::HandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *mapObj.ptr) };
    let k = unsafe { val(cx, key) };
    let v = unsafe { val(cx, val_) };
    let r = call_method(cx, o, "set", &[k, v]).map(|_| true);
    let ok = match r {
        Ok(b) => b,
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

/// The out-vector of `js::GetSetObjectKeys` and `js::GetMapObjectKeysAndValuesInterleaved` (a C++ `MutableHandleIdVector` /
/// `MutableHandleValueVector`) reaches us as a `u8`: the generated binding has no type for it, and a `u8` cannot hold an
/// address, so writing through it would write to a made-up pointer. Until the binding passes a real pointer, both report the
/// call as failed with a plain Error pending (a TypeError would claim the argument was wrong) and never touch the parameter.
unsafe fn out_vector_unsupported(cx: *mut JSContext, bs: Bs, name: &str) -> bool {
    let realm = embed::current_realm(bs);
    let text = format!("{name} is not supported: the binding passes its out-vector as a u8, which cannot hold an address");
    match api::new_error(bs, realm, &text) {
        Ok(e) => unsafe { cx_of(cx) }.set_pending(e.as_value().as_raw_bits()),
        Err(e) => unsafe {
            fail(cx, e);
        },
    }
    false
}

/// `js::GetSetObjectKeys`: not a Set is a TypeError; for a Set, see `out_vector_unsupported`.
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__GetSetObjectKeys(cx: *mut JSContext, obj: JS::HandleObject, _keys: u8) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let ok = match o.as_opt::<SetObject>() {
        Some(_) => unsafe { out_vector_unsupported(cx, bs, "js::GetSetObjectKeys") },
        None => match brimstone_core::runtime::error::type_error::<()>(bs, "not a Set") {
            Err(e) => unsafe { fail(cx, e) },
            Ok(()) => false,
        },
    };
    scope.exit();
    ok
}

/// `js::GetMapObjectKeysAndValuesInterleaved`: not a Map is a TypeError; for a Map, see `out_vector_unsupported`.
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__GetMapObjectKeysAndValuesInterleaved(cx: *mut JSContext, obj: JS::HandleObject, _entries: u8) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let ok = match o.as_opt::<MapObject>() {
        Some(_) => unsafe { out_vector_unsupported(cx, bs, "js::GetMapObjectKeysAndValuesInterleaved") },
        None => match brimstone_core::runtime::error::type_error::<()>(bs, "not a Map") {
            Err(e) => unsafe { fail(cx, e) },
            Ok(()) => false,
        },
    };
    scope.exit();
    ok
}

/// `JS_NondeterministicGetWeakMapKeys` / `...WeakSetKeys`: a fresh array of the weak collection's (live) keys.
unsafe fn weak_keys_impl(cx: *mut JSContext, obj: JS::HandleObject, ret: JS::MutableHandleObject, is_map: bool) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { obj_h(cx, *obj.ptr) };
    let r: Result<Handle<api::ObjectValue>, EvalError> = (|| {
        let keys: Vec<Handle<BsValue>> = if is_map {
            let Some(map) = o.as_opt::<WeakMapObject>() else { return brimstone_core::runtime::error::type_error(bs, "not a WeakMap") };
            map.weak_map_data().iter_gc_unsafe().map(|(k, _)| k.get().to_handle(bs)).collect()
        } else {
            let Some(set) = o.as_opt::<brimstone_core::runtime::intrinsics::weak_set_object::WeakSetObject>() else { return brimstone_core::runtime::error::type_error(bs, "not a WeakSet") };
            { let mut d = set.weak_set_data(); d.iter_mut_gc_unsafe().map(|k| k.get().to_handle(bs)).collect() }
        };
        Ok(api::create_array_from_list(bs, &keys)?.as_object())
    })();
    let ok = match r {
        Ok(a) => {
            unsafe { (*ret.ptr) = (*a).as_ptr().cast() };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NondeterministicGetWeakMapKeys(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    ret: JS::MutableHandleObject,
) -> bool {
    unsafe { weak_keys_impl(cx, obj, ret, true) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NondeterministicGetWeakSetKeys(
    cx: *mut JSContext,
    obj: JS::HandleObject,
    ret: JS::MutableHandleObject,
) -> bool {
    unsafe { weak_keys_impl(cx, obj, ret, false) }
}

// ---- Strings ----

/// The chars of a `StringBuffer` (they follow the header in the same allocation).
unsafe fn buffer_chars<T>(buffer: *mut mozilla::StringBuffer) -> *const T {
    unsafe { (buffer as *const u8).add(::std::mem::size_of::<mozilla::StringBuffer>()).cast() }
}

unsafe fn new_string_from_buffer(cx: *mut JSContext, ptr: *mut mozilla::StringBuffer, length: usize, two_byte: bool, utf8: bool) -> *mut JSString {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r: Result<Handle<api::FlatString>, EvalError> = if utf8 {
        let bytes = unsafe { ::std::slice::from_raw_parts(buffer_chars::<u8>(ptr), length) };
        match ::std::str::from_utf8(bytes) {
            Ok(text) => api::string_from_utf8(bs, text),
            Err(_) => brimstone_core::runtime::error::type_error(bs, "malformed UTF-8 character sequence"),
        }
    } else if two_byte {
        let units = unsafe { ::std::slice::from_raw_parts(buffer_chars::<u16>(ptr), length) };
        api::string_from_code_units(bs, units)
    } else {
        let bytes = unsafe { ::std::slice::from_raw_parts(buffer_chars::<u8>(ptr), length) };
        api::string_from_latin1(bs, bytes)
    };
    let out = match r {
        Ok(s) => (*s).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    out
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewStringFromLatin1Buffer(cx: *mut JSContext, buffer: RefPtr<mozilla::StringBuffer>, length: usize) -> *mut JSString {
    unsafe { new_string_from_buffer(cx, buffer.mRawPtr, length, false, false) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewStringFromTwoByteBuffer(cx: *mut JSContext, buffer: RefPtr<mozilla::StringBuffer>, length: usize) -> *mut JSString {
    unsafe { new_string_from_buffer(cx, buffer.mRawPtr, length, true, false) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewStringFromUTF8Buffer(cx: *mut JSContext, buffer: RefPtr<mozilla::StringBuffer>, length: usize) -> *mut JSString {
    unsafe { new_string_from_buffer(cx, buffer.mRawPtr, length, false, true) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewStringFromKnownLiveLatin1Buffer(cx: *mut JSContext, buffer: *mut mozilla::StringBuffer, length: usize) -> *mut JSString {
    unsafe { new_string_from_buffer(cx, buffer, length, false, false) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewStringFromKnownLiveTwoByteBuffer(cx: *mut JSContext, buffer: *mut mozilla::StringBuffer, length: usize) -> *mut JSString {
    unsafe { new_string_from_buffer(cx, buffer, length, true, false) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewStringFromKnownLiveUTF8Buffer(cx: *mut JSContext, buffer: *mut mozilla::StringBuffer, length: usize) -> *mut JSString {
    unsafe { new_string_from_buffer(cx, buffer, length, false, true) }
}

/// Decode UTF-8 into a NUL-terminated buffer (Latin-1 or UTF-16), allocated with `JS_string_malloc`.
unsafe fn utf8_to_chars_z(cx: *mut JSContext, utf8: *const JS::UTF8Chars, outlen: *mut usize, lossy: bool, two_byte: bool) -> *mut c_void {
    let r = unsafe { &(*utf8) };
    let start = r._base.mStart.mPtr;
    let len = unsafe { r._base.mEnd.mPtr.offset_from(start) } as usize;
    let bytes = if len == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(start, len) } };
    let text = if lossy {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        match ::std::str::from_utf8(bytes) {
            Ok(t) => t.to_string(),
            Err(_) => {
                let bs = unsafe { cx_of(cx) }.bs;
                let scope = HandleScope::enter(bs);
                if let Err(e) = brimstone_core::runtime::error::type_error::<()>(bs, "malformed UTF-8 character sequence") {
                    unsafe { fail(cx, e) };
                }
                scope.exit();
                return ::std::ptr::null_mut();
            },
        }
    };
    let units: Vec<u16> = text.encode_utf16().collect();
    if two_byte {
        let n = units.len();
        let buf = unsafe { JS_string_malloc(cx, (n + 1) * 2) };
        if buf.is_null() {
            return ::std::ptr::null_mut();
        }
        unsafe {
            ::std::ptr::copy_nonoverlapping(units.as_ptr(), buf.cast::<u16>(), n);
            *buf.cast::<u16>().add(n) = 0;
            if !outlen.is_null() {
                *outlen = n;
            }
        }
        buf
    } else {
        // Latin-1: every code point must fit in a byte (lossy replaces with '?').
        let bytes: Vec<u8> = units.iter().map(|&u| if u <= 0xFF { u as u8 } else { b'?' }).collect();
        let n = bytes.len();
        let buf = unsafe { JS_string_malloc(cx, n + 1) };
        if buf.is_null() {
            return ::std::ptr::null_mut();
        }
        unsafe {
            ::std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast::<u8>(), n);
            *buf.cast::<u8>().add(n) = 0;
            if !outlen.is_null() {
                *outlen = n;
            }
        }
        buf
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__UTF8CharsToNewLatin1CharsZ(
    cx: *mut JSContext,
    utf8: *const JS::UTF8Chars,
    outlen: *mut usize,
    _destArenaId: arena_id_t,
) -> JS::Latin1CharsZ {
    let p = unsafe { utf8_to_chars_z(cx, utf8, outlen, false, false) };
    JS::Latin1CharsZ { _base: mozilla::RangedPtr { _phantom_0: ::std::marker::PhantomData, mPtr: p.cast() } }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__UTF8CharsToNewTwoByteCharsZ(
    cx: *mut JSContext,
    utf8: *const JS::UTF8Chars,
    outlen: *mut usize,
    _destArenaId: arena_id_t,
) -> JS::TwoByteCharsZ {
    let p = unsafe { utf8_to_chars_z(cx, utf8, outlen, false, true) };
    JS::TwoByteCharsZ { _base: mozilla::RangedPtr { _phantom_0: ::std::marker::PhantomData, mPtr: p.cast() } }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__LossyUTF8CharsToNewTwoByteCharsZ(
    cx: *mut JSContext,
    utf8: *const JS::UTF8Chars,
    outlen: *mut usize,
    _destArenaId: arena_id_t,
) -> JS::TwoByteCharsZ {
    let p = unsafe { utf8_to_chars_z(cx, utf8, outlen, true, true) };
    JS::TwoByteCharsZ { _base: mozilla::RangedPtr { _phantom_0: ::std::marker::PhantomData, mPtr: p.cast() } }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__LossyTwoByteCharsToNewLatin1CharsZ(
    cx: *mut JSContext,
    tbchars: *const mozilla::Range<u16>,
) -> JS::Latin1CharsZ {
    let r = unsafe { &(*tbchars) };
    let start = r.mStart.mPtr;
    let len = unsafe { r.mEnd.mPtr.offset_from(start) } as usize;
    let units = if len == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(start, len) } };
    let bytes: Vec<u8> = units.iter().map(|&u| (u & 0xFF) as u8).collect();
    let n = bytes.len();
    let buf = unsafe { JS_string_malloc(cx, n + 1) };
    let p = if buf.is_null() {
        ::std::ptr::null_mut()
    } else {
        unsafe {
            ::std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast::<u8>(), n);
            *buf.cast::<u8>().add(n) = 0;
        }
        buf
    };
    JS::Latin1CharsZ { _base: mozilla::RangedPtr { _phantom_0: ::std::marker::PhantomData, mPtr: p.cast() } }
}

/// UTF-8 C string to a NUL-terminated buffer (UTF-16 or Latin-1).
unsafe fn encode_utf8(cx: *mut JSContext, chars: *const c_char, two_byte: bool) -> *mut c_void {
    let bytes = unsafe { ::std::ffi::CStr::from_ptr(chars) }.to_bytes();
    let text = String::from_utf8_lossy(bytes);
    let units: Vec<u16> = text.encode_utf16().collect();
    if two_byte {
        let n = units.len();
        let buf = unsafe { JS_string_malloc(cx, (n + 1) * 2) };
        if !buf.is_null() {
            unsafe {
                ::std::ptr::copy_nonoverlapping(units.as_ptr(), buf.cast::<u16>(), n);
                *buf.cast::<u16>().add(n) = 0;
            }
        }
        buf
    } else {
        let bytes: Vec<u8> = units.iter().map(|&u| if u <= 0xFF { u as u8 } else { b'?' }).collect();
        let n = bytes.len();
        let buf = unsafe { JS_string_malloc(cx, n + 1) };
        if !buf.is_null() {
            unsafe {
                ::std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast::<u8>(), n);
                *buf.cast::<u8>().add(n) = 0;
            }
        }
        buf
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__EncodeUtf8ToWide(cx: *mut JSContext, chars: *const c_char) -> JS::UniqueWideChars {
    unsafe { encode_utf8(cx, chars, true) as JS::UniqueWideChars }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__EncodeUtf8ToNarrow(cx: *mut JSContext, chars: *const c_char) -> JS::UniqueChars {
    unsafe { encode_utf8(cx, chars, false) as JS::UniqueChars }
}

/// Latin-1 C string to UTF-8.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__EncodeNarrowToUtf8(cx: *mut JSContext, chars: *const c_char) -> JS::UniqueChars {
    let bytes = unsafe { ::std::ffi::CStr::from_ptr(chars) }.to_bytes();
    let text: String = bytes.iter().map(|&b| b as char).collect();
    let out = text.into_bytes();
    let n = out.len();
    let buf = unsafe { JS_string_malloc(cx, n + 1) };
    if !buf.is_null() {
        unsafe {
            ::std::ptr::copy_nonoverlapping(out.as_ptr(), buf.cast::<u8>(), n);
            *buf.cast::<u8>().add(n) = 0;
        }
    }
    buf as JS::UniqueChars
}

/// `JS_DecodeBytes`: decode UTF-8 bytes into `dst`; sets `*dstlenp` to the length needed.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DecodeBytes(
    _cx: *mut JSContext,
    src: *const c_char,
    srclen: usize,
    dst: *mut u16,
    dstlenp: *mut usize,
) -> bool {
    let bytes = if srclen == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(src.cast::<u8>(), srclen) } };
    let text = match ::std::str::from_utf8(bytes) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let units: Vec<u16> = text.encode_utf16().collect();
    let needed = units.len();
    let cap = if dstlenp.is_null() { needed } else { unsafe { *dstlenp } };
    unsafe {
        if !dstlenp.is_null() {
            *dstlenp = needed;
        }
    }
    if dst.is_null() {
        return true;
    }
    let n = needed.min(cap);
    unsafe { ::std::ptr::copy_nonoverlapping(units.as_ptr(), dst, n) };
    true
}

/// The string's chars as a NUL-terminated UTF-16 copy.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_CopyStringCharsZ(cx: *mut JSContext, str_: *mut JSString) -> JS::UniqueTwoByteChars {
    let Some(u) = (unsafe { units_of(cx, str_) }) else { return 0 as JS::UniqueTwoByteChars };
    let n = u.len();
    let buf = unsafe { JS_string_malloc(cx, (n + 1) * 2) };
    if buf.is_null() {
        return 0 as JS::UniqueTwoByteChars;
    }
    unsafe {
        ::std::ptr::copy_nonoverlapping(u.as_ptr(), buf.cast::<u16>(), n);
        *buf.cast::<u16>().add(n) = 0;
    }
    buf as usize as JS::UniqueTwoByteChars
}

unsafe fn encode_string(cx: *mut JSContext, s: *mut JSString, mode: u8) -> JS::UniqueChars {
    // mode: 0 = UTF-8 (lossy), 1 = Latin-1 (lossy), 2 = ASCII (non-ASCII becomes '?')
    let Some(u) = (unsafe { units_of(cx, s) }) else { return 0 as JS::UniqueChars };
    let bytes: Vec<u8> = match mode {
        0 => String::from_utf16_lossy(&u).into_bytes(),
        1 => u.iter().map(|&c| if c <= 0xFF { c as u8 } else { b'?' }).collect(),
        _ => u.iter().map(|&c| if c <= 0x7F { c as u8 } else { b'?' }).collect(),
    };
    let n = bytes.len();
    let buf = unsafe { JS_string_malloc(cx, n + 1) };
    if buf.is_null() {
        return 0 as JS::UniqueChars;
    }
    unsafe {
        ::std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast::<u8>(), n);
        *buf.cast::<u8>().add(n) = 0;
    }
    buf as JS::UniqueChars
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_EncodeStringToUTF8(cx: *mut JSContext, str_: JS::Handle<*mut JSString>) -> JS::UniqueChars {
    unsafe { encode_string(cx, *str_.ptr, 0) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_EncodeStringToLatin1(cx: *mut JSContext, str_: *mut JSString) -> JS::UniqueChars {
    unsafe { encode_string(cx, str_, 1) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_EncodeStringToASCII(cx: *mut JSContext, str_: *mut JSString) -> JS::UniqueChars {
    unsafe { encode_string(cx, str_, 2) }
}

/// `JS_PutEscapedString`: write the string escaped into `buffer` (at most `size` bytes, NUL-terminated);
/// returns the number of bytes the full escaped string needs.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_PutEscapedString(
    cx: *mut JSContext,
    buffer: *mut c_char,
    size: usize,
    str_: *mut JSString,
    quote: c_char,
) -> usize {
    let Some(u) = (unsafe { units_of(cx, str_) }) else { return 0 };
    let q = quote as u8;
    let mut out: Vec<u8> = Vec::new();
    for &c in &u {
        match c {
            c if c as u8 == q => {
                out.push(b'\\');
                out.push(q);
            },
            0x5C => out.extend_from_slice(b"\\\\"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0C => out.extend_from_slice(b"\\f"),
            0x0A => out.extend_from_slice(b"\\n"),
            0x0D => out.extend_from_slice(b"\\r"),
            0x09 => out.extend_from_slice(b"\\t"),
            c if c < 0x20 => out.extend_from_slice(format!("\\u{c:04x}").as_bytes()),
            c if c <= 0xFF => out.push(c as u8),
            c => {
                let ch = char::from_u32(c as u32).unwrap_or('\u{FFFD}');
                let mut tmp = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
            },
        }
    }
    let needed = out.len();
    if !buffer.is_null() && size > 0 {
        let n = needed.min(size - 1);
        unsafe {
            ::std::ptr::copy_nonoverlapping(out.as_ptr(), buffer.cast::<u8>(), n);
            *buffer.add(n) = 0;
        }
    }
    needed
}

/// `JS_NewDependentString`: a substring of `str_` (copied, as Brimstone has no dependent strings).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewDependentString(
    cx: *mut JSContext,
    str_: JS::Handle<*mut JSString>,
    start: usize,
    length: usize,
) -> *mut JSString {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = (|| -> Result<Handle<api::FlatString>, EvalError> {
        let Some(u) = (unsafe { units_of(cx, *str_.ptr) }) else { return brimstone_core::runtime::error::type_error(bs, "string flattening failed") };
        let s = start.min(u.len());
        let e = (s + length).min(u.len());
        api::string_from_code_units(bs, &u[s..e])
    })();
    let out = match r {
        Ok(s) => (*s).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    out
}

/// Maybe-external strings: Brimstone has no external strings, so the chars are copied and
/// `allocatedExternal` is set to false (the caller keeps ownership of its buffer).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewMaybeExternalStringLatin1(
    cx: *mut JSContext,
    chars: *const JS::Latin1Char,
    length: usize,
    _callbacks: *const JSExternalStringCallbacks,
    allocatedExternal: *mut bool,
) -> *mut JSString {
    if !allocatedExternal.is_null() {
        unsafe { *allocatedExternal = false };
    }
    let bytes = if length == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(chars, length) } };
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = match api::string_from_latin1(bs, bytes) {
        Ok(s) => (*s).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewMaybeExternalUCString(
    cx: *mut JSContext,
    chars: *const u16,
    length: usize,
    _callbacks: *const JSExternalStringCallbacks,
    allocatedExternal: *mut bool,
) -> *mut JSString {
    if !allocatedExternal.is_null() {
        unsafe { *allocatedExternal = false };
    }
    let units = if length == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(chars, length) } };
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = match api::string_from_code_units(bs, units) {
        Ok(s) => (*s).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewMaybeExternalStringUTF8(
    cx: *mut JSContext,
    utf8: *const JS::UTF8Chars,
    _callbacks: *const JSExternalStringCallbacks,
    allocatedExternal: *mut bool,
) -> *mut JSString {
    if !allocatedExternal.is_null() {
        unsafe { *allocatedExternal = false };
    }
    let r = unsafe { &(*utf8) };
    let start = r._base.mStart.mPtr;
    let len = unsafe { r._base.mEnd.mPtr.offset_from(start) } as usize;
    let bytes = if len == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(start, len) } };
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let out = match ::std::str::from_utf8(bytes) {
        Ok(text) => match api::string_from_utf8(bs, text) {
            Ok(s) => (*s).as_ptr().cast(),
            Err(e) => {
                unsafe { fail(cx, e) };
                ::std::ptr::null_mut()
            },
        },
        Err(_) => {
            if let Err(e) = brimstone_core::runtime::error::type_error::<()>(bs, "malformed UTF-8 character sequence") {
                unsafe { fail(cx, e) };
            }
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    out
}

/// Brimstone's copying GC moves strings, so no string is ever pinned.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_StringHasBeenPinned(_cx: *mut JSContext, _str_: *mut JSString) -> bool {
    false
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__detail__StringToLinearStringSlow(cx: *mut JSContext, str_: *mut JSString) -> *mut JSLinearString {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let h = BsValue::string(str_ptr(str_)).to_handle(bs).as_string();
    let r = match h.to_handle().flatten_to_handle() {
        Ok(f) => f.as_ptr().cast(),
        Err(_) => ::std::ptr::null_mut(),
    };
    scope.exit();
    r
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__GetLinearStringLength(s: *mut JSLinearString) -> usize {
    HeapPtr::<api::FlatString>::from_ptr(s.cast()).len() as usize
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__GetLinearStringCharAt(s: *mut JSLinearString, idx: usize) -> u16 {
    let p = HeapPtr::<api::FlatString>::from_ptr(s.cast());
    // SAFETY: one char is read at once; nothing in the match allocates.
    match unsafe { super::string::flat_chars(p) } {
        api::StringChars::Latin1(b) => b.get(idx).map(|&c| c as u16).unwrap_or(0),
        api::StringChars::TwoByte(u) => u.get(idx).copied().unwrap_or(0),
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__AtomToLinearString(atom: *mut JSAtom) -> *mut JSLinearString {
    // Atoms are interned flat strings.
    atom.cast()
}

#[super::poison::engine_shim(failure = unsafe { (*dest).asBits_ = BsValue::undefined().as_raw_bits() })]
pub unsafe extern "C" fn glue__JS_GetEmptyStringValue(cx: *mut JSContext, dest: *mut JS::Value) {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    match api::string_from_latin1(bs, b"") {
        Ok(s) => unsafe { (*dest).asBits_ = s.as_value().as_raw_bits() },
        Err(_) => unsafe { (*dest).asBits_ = BsValue::undefined().as_raw_bits() },
    }
    scope.exit();
}

#[super::poison::engine_shim(failure = unsafe { (*dest).asBits_ = BsValue::number(f64::NAN).as_raw_bits() })]
pub unsafe extern "C" fn glue__JS_GetNaNValue(_cx: *mut JSContext, dest: *mut JS::Value) {
    unsafe { (*dest).asBits_ = BsValue::number(f64::NAN).as_raw_bits() };
}

#[super::poison::engine_shim(failure = unsafe { (*dest).asBits_ = BsValue::number(f64::INFINITY).as_raw_bits() })]
pub unsafe extern "C" fn glue__JS_GetPositiveInfinityValue(_cx: *mut JSContext, dest: *mut JS::Value) {
    unsafe { (*dest).asBits_ = BsValue::number(f64::INFINITY).as_raw_bits() };
}

/// `JS_IsIdentifier`: identifier syntax, approximated with Rust's char classes plus `_` and `$`
/// (exact on the ASCII range, which is what Servo checks).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_IsIdentifier(cx: *mut JSContext, str_: JS::HandleString, isIdentifier: *mut bool) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let ok = match unsafe { units_of(cx, *str_.ptr) } {
        Some(u) => {
            let valid = !u.is_empty()
                && u.iter().enumerate().all(|(i, &c)| {
                    let ch = char::from_u32(c as u32).unwrap_or('\u{FFFD}');
                    if i == 0 {
                        ch == '_' || ch == '$' || ch.is_alphabetic()
                    } else {
                        ch == '_' || ch == '$' || ch.is_alphanumeric()
                    }
                });
            unsafe { *isIdentifier = valid };
            true
        },
        None => unsafe { fail(cx, embed::uncatchable()) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_IndexToId(_cx: *mut JSContext, index: u32, arg1: JS::MutableHandleId) -> bool {
    unsafe { (*arg1.ptr).asBits_ = id_bits::index_id(index) as usize };
    true
}

// ---- Vectors, roots and memory ----

/// A `StackGCVector` handle points at a boxed `Vec` of the element type.
#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__StackGCVectorValueLength(
    vec: JS::Handle<JS::StackGCVector<JS::Value, js::TempAllocPolicy>>,
) -> u32 {
    let v = unsafe { &*(vec.ptr as *const Vec<JS::Value>) };
    v.len() as u32
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__StackGCVectorValueAtIndex(
    vec: JS::Handle<JS::StackGCVector<JS::Value, js::TempAllocPolicy>>,
    index: u32,
) -> *const JS::Value {
    let v = unsafe { &*(vec.ptr as *const Vec<JS::Value>) };
    v.get(index as usize).map(|p| p as *const JS::Value).unwrap_or(::std::ptr::null())
}
#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__StackGCVectorStringLength(
    vec: JS::Handle<JS::StackGCVector<*mut JSString, js::TempAllocPolicy>>,
) -> u32 {
    let v = unsafe { &*(vec.ptr as *const Vec<*mut JSString>) };
    v.len() as u32
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__StackGCVectorStringAtIndex(
    vec: JS::Handle<JS::StackGCVector<*mut JSString, js::TempAllocPolicy>>,
    index: u32,
) -> *const *mut JSString {
    let v = unsafe { &*(vec.ptr as *const Vec<*mut JSString>) };
    v.get(index as usize).map(|p| p as *const *mut JSString).unwrap_or(::std::ptr::null())
}

/// Traces a rooted object vector's objects; registered in `Cx::extra_roots`.
#[super::poison::engine_shim(gc)]
unsafe extern "C" fn trace_obj_vec(trc: *mut JSTracer, data: *mut c_void) {
    let v = unsafe { &mut *(data as *mut Vec<*mut JSObject>) };
    for p in v.iter_mut() {
        unsafe { super::gc::trace_cell_slot(trc, p as *mut *mut JSObject as *mut *mut c_void) };
    }
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn glue__CreateRootedObjectVector(cx: *mut JSContext) -> *mut JS::PersistentRootedObjectVector {
    let v = Box::into_raw(Box::new(Vec::<*mut JSObject>::new()));
    unsafe { cx_of(cx) }.extra_roots.push((Some(trace_obj_vec), v as *mut c_void));
    v as *mut JS::PersistentRootedObjectVector
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__AppendToRootedObjectVector(v: *mut JS::PersistentRootedObjectVector, obj: *mut JSObject) -> bool {
    let vec = unsafe { &mut *(v as *mut Vec<*mut JSObject>) };
    vec.push(obj);
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__GetObjectVectorAddress(v: *mut JS::PersistentRootedObjectVector) -> *mut c_void {
    unsafe { (*(v as *mut Vec<*mut JSObject>)).as_mut_ptr().cast() }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__DeleteRootedObjectVector(v: *mut JS::PersistentRootedObjectVector) {
    let raw = v as *mut Vec<*mut JSObject>;
    let cx = super::cx::current_raw();
    let roots = &mut unsafe { cx_of(cx) }.extra_roots;
    if let Some(i) = roots.iter().position(|&(_, d)| d == raw as *mut c_void) {
        roots.remove(i);
    }
    unsafe { drop(Box::from_raw(raw)) };
}

/// Traces a raw value slot (for `js::AddRawValueRoot`).
#[super::poison::engine_shim(gc)]
unsafe extern "C" fn trace_raw_value(trc: *mut JSTracer, data: *mut c_void) {
    unsafe { super::gc::trace_value_slot(trc, data.cast::<u64>()) };
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__AddRawValueRoot(cx: *mut JSContext, vp: *mut JS::Value, _name: *const c_char) -> bool {
    unsafe { cx_of(cx) }.extra_roots.push((Some(trace_raw_value), vp as *mut c_void));
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__RemoveRawValueRoot(cx: *mut JSContext, vp: *mut JS::Value) {
    let roots = &mut unsafe { cx_of(cx) }.extra_roots;
    if let Some(i) = roots.iter().position(|&(_, d)| d == vp as *mut c_void) {
        roots.remove(i);
    }
}

/// Associated-memory accounting only: Brimstone sizes its own heap, so there is nothing to report.
/// `JS::AddAssociatedMemory`: Servo reports a DOM object's Rust size (and canvas / WebGL stores) as owned by its reflector.
/// Counted toward a collection as SpiderMonkey's zone malloc counter counts it (`embed::add_associated_memory`). The reflector
/// lives on this thread's context, which a finalizer also runs on.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__AddAssociatedMemory(obj: *mut JSObject, nbytes: usize, _use_: JS::MemoryUse) {
    let cx = super::cx::current_raw();
    if obj.is_null() || nbytes == 0 || cx.is_null() {
        return;
    }
    // Short place access through the raw pointer: this may run inside a finalizer.
    let bs = unsafe { (*super::cx::current_ptr()).bs };
    embed::add_associated_memory(bs, nbytes);
}

/// `JS::RemoveAssociatedMemory`: the matching removal, usually from a finalizer during a collection.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__RemoveAssociatedMemory(obj: *mut JSObject, nbytes: usize, _use_: JS::MemoryUse) {
    let cx = super::cx::current_raw();
    if obj.is_null() || nbytes == 0 || cx.is_null() {
        return;
    }
    let bs = unsafe { (*super::cx::current_ptr()).bs };
    embed::remove_associated_memory(bs, nbytes);
}

/// `JS::GCTraceKindToAscii`.
#[super::poison::engine_shim(failure = ::std::ptr::null())]
pub unsafe extern "C" fn JS__GCTraceKindToAscii(kind: JS::TraceKind) -> *const c_char {
    static NAMES: [&[u8]; 13] = [
        b"Object\0", b"BigInt\0", b"String\0", b"GetterSetter\0", b"Symbol\0", b"Shape\0", b"Null\0",
        b"BaseShape\0", b"JitCode\0", b"Script\0", b"Scope\0", b"RegExpShared\0", b"PrivateScriptData\0",
    ];
    let i = kind as usize;
    if i < NAMES.len() {
        NAMES[i].as_ptr().cast()
    } else {
        b"None\0".as_ptr().cast()
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_malloc(_cx: *mut JSContext, nbytes: usize) -> *mut c_void {
    unsafe { libc::malloc(nbytes.max(1)) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_realloc(_cx: *mut JSContext, p: *mut c_void, _oldBytes: usize, newBytes: usize) -> *mut c_void {
    unsafe { libc::realloc(p, newBytes.max(1)) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_free(_cx: *mut JSContext, p: *mut c_void) {
    unsafe { libc::free(p) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_string_malloc(cx: *mut JSContext, nbytes: usize) -> *mut c_void {
    unsafe { JS_malloc(cx, nbytes) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_string_realloc(cx: *mut JSContext, p: *mut c_void, oldBytes: usize, newBytes: usize) -> *mut c_void {
    unsafe { JS_realloc(cx, p, oldBytes, newBytes) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_string_free(cx: *mut JSContext, p: *mut c_void) {
    unsafe { JS_free(cx, p) }
}
