/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Strings. The original is the string part of `jsapi.cpp` and `js/src/vm/StringType.cpp` in mozjs_sys 153.6.0-1.
//!
//! `JSString*` is a Brimstone `StringValue` (flat, or a concatenation). SpiderMonkey hands out a string's chars only for a
//! "linear" string and flattens ropes in place; Brimstone does too (a flattened concat string forwards to its flat copy at the
//! same address), but its flat copy is allocated on the GC heap, so flattening can collect and move other strings. Callers that
//! keep a raw `JSString*` across `JS_GetLatin1StringCharsAndLength` & co. (mozjs `jsstr_to_string` does) are only safe for flat
//! strings until the fork can read a rope's chars without allocating (requested of the fork).
//!
//! External strings are copied: Brimstone has no string kind that borrows foreign chars. The finalizer runs at once, which is
//! when the copy stops needing them.

use brimstone_core::runtime::embed::api::{self, StringChars};
use brimstone_core::runtime::gc::{HandleScope, HeapPtr};
use brimstone_core::runtime::{Handle, Value as BsValue};

use super::cx::cx_of;
use super::util::fail;
use crate::boundary::bounds;
use crate::jsapi::*;

#[inline]
fn string_ptr(s: *mut JSString) -> HeapPtr<api::StringValue> {
    HeapPtr::from_ptr(s.cast())
}

/// A new string, made inside its own handle scope; null with a pending exception on failure.
unsafe fn make(
    cx: *mut JSContext,
    f: impl FnOnce(brimstone_core::runtime::Context) -> brimstone_core::runtime::EvalResult<Handle<api::FlatString>>,
) -> *mut JSString {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = match f(bs) {
        Ok(s) => (*s).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}

/// The flat string behind `s`, flattening a concatenation (see the module comment).
unsafe fn flat(cx: *mut JSContext, s: *mut JSString) -> Option<HeapPtr<api::FlatString>> {
    let p = string_ptr(s);
    if p.is_flat() {
        return Some(p.as_flat());
    }
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let h = BsValue::string(p).to_handle(bs).as_string();
    let r = h.flatten().ok();
    scope.exit();
    r
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewStringCopyN(cx: *mut JSContext, s: *const ::std::os::raw::c_char, n: usize) -> *mut JSString {
    // SpiderMonkey reads these bytes as Latin-1.
    let bytes = unsafe { ::std::slice::from_raw_parts(s.cast::<u8>(), n) };
    unsafe { make(cx, |bs| api::string_from_latin1(bs, bytes)) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewStringCopyZ(cx: *mut JSContext, s: *const ::std::os::raw::c_char) -> *mut JSString {
    if s.is_null() {
        return unsafe { JS_GetEmptyString(cx) };
    }
    let bytes = unsafe { ::std::ffi::CStr::from_ptr(s) }.to_bytes();
    unsafe { make(cx, |bs| api::string_from_latin1(bs, bytes)) }
}

/// UTF-8 in. SpiderMonkey throws on malformed UTF-8 here; so do we (a `TypeError`, SpiderMonkey's JSMSG_MALFORMED_UTF8_CHAR).
unsafe fn from_utf8(cx: *mut JSContext, bytes: &[u8]) -> *mut JSString {
    match ::std::str::from_utf8(bytes) {
        Ok(text) => unsafe { make(cx, |bs| api::string_from_utf8(bs, text)) },
        Err(_) => unsafe {
            make(cx, |bs| {
                let e = api::new_type_error(bs, brimstone_core::runtime::embed::current_realm(bs), "malformed UTF-8 character sequence")?;
                Err(brimstone_core::runtime::embed::throw_value(e.as_value()))
            })
        },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewStringCopyUTF8N(cx: *mut JSContext, s: *const JS::UTF8Chars) -> *mut JSString {
    let r = unsafe { &(*s)._base };
    let start = r.mStart.mPtr;
    let len = unsafe { r.mEnd.mPtr.offset_from(start) } as usize;
    let bytes = if len == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(start.cast_const(), len) } };
    unsafe { from_utf8(cx, bytes) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewStringCopyUTF8Z(cx: *mut JSContext, s: JS::ConstUTF8CharsZ) -> *mut JSString {
    if s.data_.is_null() {
        return unsafe { JS_GetEmptyString(cx) };
    }
    let bytes = unsafe { ::std::ffi::CStr::from_ptr(s.data_) }.to_bytes();
    unsafe { from_utf8(cx, bytes) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewUCStringCopyN(cx: *mut JSContext, s: *const u16, n: usize) -> *mut JSString {
    let units = if n == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(s, n) } };
    unsafe { make(cx, |bs| api::string_from_code_units(bs, units)) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewUCStringCopyZ(cx: *mut JSContext, s: *const u16) -> *mut JSString {
    if s.is_null() {
        return unsafe { JS_GetEmptyString(cx) };
    }
    let mut n = 0;
    while unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    unsafe { JS_NewUCStringCopyN(cx, s, n) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetEmptyString(cx: *mut JSContext) -> *mut JSString {
    unsafe { make(cx, |bs| api::string_from_latin1(bs, b"")) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetStringLength(s: *mut JSString) -> usize {
    string_ptr(s).len() as usize
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DeprecatedStringHasLatin1Chars(s: *mut JSString) -> bool {
    string_ptr(s).width() == api::StringWidth::OneByte
}

/// `api::flat_string_chars`: a flat string's characters, borrowed from the GC heap. The fork is making it an `unsafe fn`
/// (the slice dangles once a GC moves the string); this wrapper compiles with either.
///
/// # Safety
/// No GC may run while the returned chars are used: no allocation on the GC heap, no call into JS or into Servo.
#[allow(unused_unsafe)] // redundant until the fork's fn is `unsafe`
#[inline]
pub(crate) unsafe fn flat_chars<'a>(f: HeapPtr<api::FlatString>) -> StringChars<'a> {
    unsafe { api::flat_string_chars(f) }
}

#[super::poison::engine_shim(failure = unsafe { *length = 0; ::std::ptr::NonNull::<JS::Latin1Char>::dangling().as_ptr() })]
pub unsafe extern "C" fn JS_GetLatin1StringCharsAndLength(
    cx: *mut JSContext,
    _nogc: *const JS::AutoRequireNoGC,
    s: *mut JSString,
    length: *mut usize,
) -> *const JS::Latin1Char {
    let Some(f) = (unsafe { flat(cx, s) }) else { return ::std::ptr::null() };
    // SAFETY: the caller holds a `JS::AutoRequireNoGC` (`_nogc`) for as long as it uses the chars, as SpiderMonkey requires.
    match unsafe { flat_chars(f) } {
        StringChars::Latin1(b) => {
            unsafe { *length = b.len() };
            b.as_ptr()
        },
        StringChars::TwoByte(_) => panic!("JS_GetLatin1StringCharsAndLength on a two-byte string"),
    }
}

#[super::poison::engine_shim(failure = unsafe { *length = 0; ::std::ptr::NonNull::<u16>::dangling().as_ptr() })]
pub unsafe extern "C" fn JS_GetTwoByteStringCharsAndLength(
    cx: *mut JSContext,
    _nogc: *const JS::AutoRequireNoGC,
    s: *mut JSString,
    length: *mut usize,
) -> *const u16 {
    let Some(f) = (unsafe { flat(cx, s) }) else { return ::std::ptr::null() };
    // SAFETY: the caller holds a `JS::AutoRequireNoGC` (`_nogc`) for as long as it uses the chars, as SpiderMonkey requires.
    match unsafe { flat_chars(f) } {
        StringChars::TwoByte(u) => {
            unsafe { *length = u.len() };
            u.as_ptr()
        },
        StringChars::Latin1(_) => panic!("JS_GetTwoByteStringCharsAndLength on a Latin-1 string"),
    }
}

/// The string as UTF-16 code units (copies; for comparisons and encoders).
unsafe fn units(cx: *mut JSContext, s: *mut JSString) -> Option<Vec<u16>> {
    let f = unsafe { flat(cx, s) }?;
    // SAFETY: the chars are copied out at once; nothing in the match allocates.
    Some(match unsafe { flat_chars(f) } {
        StringChars::Latin1(b) => b.iter().map(|&c| c as u16).collect(),
        StringChars::TwoByte(u) => u.to_vec(),
    })
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetStringCharAt(cx: *mut JSContext, s: *mut JSString, index: usize, res: *mut u16) -> bool {
    let Some(f) = (unsafe { flat(cx, s) }) else { return false };
    let i = match bounds::char_index(f.len() as usize, index) {
        Some(i) => i,
        None => return false,
    };
    // SAFETY: one char is read at once; nothing in the match allocates.
    unsafe {
        *res = match flat_chars(f) {
            StringChars::Latin1(b) => b[i] as u16,
            StringChars::TwoByte(u) => u[i],
        }
    };
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_CopyStringChars(cx: *mut JSContext, dest: *const mozilla::Range<u16>, s: *mut JSString) -> bool {
    let Some(u) = (unsafe { units(cx, s) }) else { return false };
    let start = unsafe { (*dest).mStart.mPtr };
    let dest_len = unsafe { (*dest).mEnd.mPtr.offset_from(start) } as usize;
    let n = bounds::copy_count(u.len(), dest_len);
    unsafe { ::std::ptr::copy_nonoverlapping(u.as_ptr(), start, n) };
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_StringEqualsAscii(
    cx: *mut JSContext,
    s: *mut JSString,
    ascii: *const ::std::os::raw::c_char,
    matched: *mut bool,
) -> bool {
    let a = unsafe { ::std::ffi::CStr::from_ptr(ascii) }.to_bytes();
    unsafe { JS_StringEqualsAscii1(cx, s, ascii, a.len(), matched) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_StringEqualsAscii1(
    cx: *mut JSContext,
    s: *mut JSString,
    ascii: *const ::std::os::raw::c_char,
    length: usize,
    matched: *mut bool,
) -> bool {
    let Some(u) = (unsafe { units(cx, s) }) else { return false };
    let a = unsafe { ::std::slice::from_raw_parts(ascii.cast::<u8>(), length) };
    unsafe { *matched = u.len() == a.len() && u.iter().zip(a).all(|(&x, &y)| x == y as u16) };
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_CompareStrings(cx: *mut JSContext, a: *mut JSString, b: *mut JSString, result: *mut i32) -> bool {
    let (Some(x), Some(y)) = (unsafe { units(cx, a) }, unsafe { units(cx, b) }) else { return false };
    // SpiderMonkey compares code units and returns their difference sign, as here.
    unsafe {
        *result = match x.cmp(&y) {
            ::std::cmp::Ordering::Less => -1,
            ::std::cmp::Ordering::Equal => 0,
            ::std::cmp::Ordering::Greater => 1,
        }
    };
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ConcatStrings(
    cx: *mut JSContext,
    left: JS::Handle<*mut JSString>,
    right: JS::Handle<*mut JSString>,
) -> *mut JSString {
    let (l, r) = unsafe { (*left.ptr, *right.ptr) };
    let (Some(mut x), Some(y)) = (unsafe { units(cx, l) }, unsafe { units(cx, r) }) else { return ::std::ptr::null_mut() };
    x.extend_from_slice(&y);
    unsafe { JS_NewUCStringCopyN(cx, x.as_ptr(), x.len()) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_EnsureLinearString(cx: *mut JSContext, s: *mut JSString) -> *mut JSLinearString {
    match unsafe { flat(cx, s) } {
        Some(f) => f.as_ptr().cast(),
        None => ::std::ptr::null_mut(),
    }
}

/// UTF-8 length of the string, lone surrogates counting as U+FFFD (3 bytes), as `JS_EncodeStringToUTF8` writes it.
#[super::poison::engine_shim(failure = usize::MAX)]
pub unsafe extern "C" fn JS_GetStringEncodingLength(cx: *mut JSContext, s: *mut JSString) -> usize {
    match unsafe { units(cx, s) } {
        Some(u) => String::from_utf16_lossy(&u).len(),
        None => usize::MAX,
    }
}

/// Latin-1 bytes of the string into `buffer` (chars above U+00FF are truncated to their low byte, as SpiderMonkey's lossy copy).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_EncodeStringToBuffer(
    cx: *mut JSContext,
    s: *mut JSString,
    buffer: *mut ::std::os::raw::c_char,
    length: usize,
) -> bool {
    let Some(u) = (unsafe { units(cx, s) }) else { return false };
    let n = bounds::copy_count(u.len(), length);
    for (i, &c) in u.iter().take(n).enumerate() {
        unsafe { *buffer.add(i) = c as u8 as ::std::os::raw::c_char };
    }
    true
}

/// mozjs's `EncodeStringToUTF8`: calls `cb` with a NUL-terminated UTF-8 copy (lone surrogates become U+FFFD).
#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__EncodeStringToUTF8(cx: *mut JSContext, s: JS::HandleString, cb: crate::glue::EncodedStringCallback) {
    let Some(u) = (unsafe { units(cx, *s.ptr) }) else { return };
    let mut text = String::from_utf16_lossy(&u).into_bytes();
    // An interior NUL would cut the callback's C string short, as with SpiderMonkey's UniqueChars.
    text.push(0);
    unsafe { cb(text.as_ptr().cast()) };
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ToStringSlow(cx: *mut JSContext, v: JS::HandleValue) -> *mut JSString {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let h = super::util::handle_of(bs, unsafe { (*v.ptr).asBits_ });
    let r = match api::to_string(bs, h) {
        Ok(s) => (*s).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    r
}

/// External strings: our own callbacks object, holding mozjs's traps.
#[repr(C)]
struct ExternalCallbacks {
    traps: crate::glue::JSExternalStringCallbacksTraps,
    private: *mut ::std::os::raw::c_void,
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__CreateJSExternalStringCallbacks(
    traps: *const crate::glue::JSExternalStringCallbacksTraps,
    private: *mut ::std::os::raw::c_void,
) -> *mut JSExternalStringCallbacks {
    Box::into_raw(Box::new(ExternalCallbacks { traps: unsafe { *traps }, private })).cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewExternalStringLatin1(
    cx: *mut JSContext,
    chars: *const JS::Latin1Char,
    length: usize,
    callbacks: *const JSExternalStringCallbacks,
) -> *mut JSString {
    let bytes = if length == 0 { &[][..] } else { unsafe { ::std::slice::from_raw_parts(chars, length) } };
    let s = unsafe { make(cx, |bs| api::string_from_latin1(bs, bytes)) };
    if !s.is_null() {
        let cb = callbacks.cast::<ExternalCallbacks>();
        if let Some(fin) = unsafe { (*cb).traps.latin1Finalize } {
            unsafe { fin((*cb).private, chars.cast_mut()) };
        }
    }
    s
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewExternalUCString(
    cx: *mut JSContext,
    chars: *const u16,
    length: usize,
    callbacks: *const JSExternalStringCallbacks,
) -> *mut JSString {
    let s = unsafe { JS_NewUCStringCopyN(cx, chars, length) };
    if !s.is_null() {
        let cb = callbacks.cast::<ExternalCallbacks>();
        if let Some(fin) = unsafe { (*cb).traps.utf16Finalize } {
            unsafe { fin((*cb).private, chars.cast_mut()) };
        }
    }
    s
}
