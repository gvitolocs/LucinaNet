/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! ArrayBuffers, typed arrays and DataViews. The original is mozjs_sys 153.6.0-1's `js/src/vm/ArrayBufferObject.cpp`,
//! `TypedArrayObject.cpp`, `ArrayBufferViewObject.cpp`, `builtin/DataViewObject.cpp` and the typed-array glue in
//! `src/jsglue.cpp` and `src/jsapi.cpp`.
//!
//! Brimstone keeps the bytes of a heap buffer in a `ByteArray` on the moving heap, so a data pointer handed out here is valid
//! until the next GC, which is what SpiderMonkey's `JS::AutoRequireNoGC` argument asks of the caller anyway. The bytes of an
//! external buffer (a `WebAssembly.Memory`'s, design §3.1) belong to the embedder: they never move, but they go stale the moment
//! the buffer is detached, which is what a memory grow does to the buffer it replaces. Either way the pointer is read at the
//! moment of the call and never cached. Every byte range handed out comes from `bounds::view_range`; a view that does not fit
//! its buffer (or a detached buffer) reads as null and empty. There is no SharedArrayBuffer, no immutable, mapped or pinned
//! buffer and no cross-compartment wrapper: the shared-memory queries answer "not shared" and unwrapping is a type check.

use ::std::ffi::c_void;
use ::std::ptr;

use brimstone_core::runtime::embed::{self, api};
use brimstone_core::runtime::error::{range_error, type_error};
use brimstone_core::runtime::eval_result::EvalError;
use brimstone_core::runtime::gc::{HandleScope, HeapPtr};
use brimstone_core::runtime::{Context as Bs, Handle, Value as BsValue};

use super::cx::cx_of;
use super::realm::object_ptr;
use super::util::{fail, handle_of};
use crate::boundary::bounds;
use crate::jsapi::JS::Scalar::Type as Scalar;
use crate::jsapi::*;

// --- Element types ---

fn scalar_of(kind: &api::TypedArrayKind) -> Scalar {
    use api::TypedArrayKind as K;
    match kind {
        K::Int8 => Scalar::Int8,
        K::UInt8 => Scalar::Uint8,
        K::UInt8Clamped => Scalar::Uint8Clamped,
        K::Int16 => Scalar::Int16,
        K::UInt16 => Scalar::Uint16,
        K::Int32 => Scalar::Int32,
        K::UInt32 => Scalar::Uint32,
        K::BigInt64 => Scalar::BigInt64,
        K::BigUInt64 => Scalar::BigUint64,
        K::Float16 => Scalar::Float16,
        K::Float32 => Scalar::Float32,
        K::Float64 => Scalar::Float64,
    }
}

fn kind_of(t: Scalar) -> api::TypedArrayKind {
    use api::TypedArrayKind as K;
    match t {
        Scalar::Int8 => K::Int8,
        Scalar::Uint8 => K::UInt8,
        Scalar::Uint8Clamped => K::UInt8Clamped,
        Scalar::Int16 => K::Int16,
        Scalar::Uint16 => K::UInt16,
        Scalar::Int32 => K::Int32,
        Scalar::Uint32 => K::UInt32,
        Scalar::BigInt64 => K::BigInt64,
        Scalar::BigUint64 => K::BigUInt64,
        Scalar::Float16 => K::Float16,
        Scalar::Float32 => K::Float32,
        Scalar::Float64 => K::Float64,
        _ => unreachable!("not a typed array element type: {t:?}"),
    }
}

fn constructor_of(t: Scalar) -> api::Intrinsic {
    use api::Intrinsic as I;
    match t {
        Scalar::Int8 => I::Int8ArrayConstructor,
        Scalar::Uint8 => I::UInt8ArrayConstructor,
        Scalar::Uint8Clamped => I::UInt8ClampedArrayConstructor,
        Scalar::Int16 => I::Int16ArrayConstructor,
        Scalar::Uint16 => I::UInt16ArrayConstructor,
        Scalar::Int32 => I::Int32ArrayConstructor,
        Scalar::Uint32 => I::UInt32ArrayConstructor,
        Scalar::BigInt64 => I::BigInt64ArrayConstructor,
        Scalar::BigUint64 => I::BigUInt64ArrayConstructor,
        Scalar::Float16 => I::Float16ArrayConstructor,
        Scalar::Float32 => I::Float32ArrayConstructor,
        Scalar::Float64 => I::Float64ArrayConstructor,
        _ => unreachable!("not a typed array element type: {t:?}"),
    }
}

fn element_size(t: Scalar) -> usize {
    match t {
        Scalar::Int8 | Scalar::Uint8 | Scalar::Uint8Clamped => 1,
        Scalar::Int16 | Scalar::Uint16 | Scalar::Float16 => 2,
        Scalar::Int32 | Scalar::Uint32 | Scalar::Float32 => 4,
        Scalar::BigInt64 | Scalar::BigUint64 | Scalar::Float64 => 8,
        _ => unreachable!("not a typed array element type: {t:?}"),
    }
}

/// `Scalar::name`, for SpiderMonkey's error messages.
fn element_name(t: Scalar) -> &'static str {
    match t {
        Scalar::Int8 => "Int8",
        Scalar::Uint8 => "Uint8",
        Scalar::Uint8Clamped => "Uint8Clamped",
        Scalar::Int16 => "Int16",
        Scalar::Uint16 => "Uint16",
        Scalar::Int32 => "Int32",
        Scalar::Uint32 => "Uint32",
        Scalar::BigInt64 => "BigInt64",
        Scalar::BigUint64 => "BigUint64",
        Scalar::Float16 => "Float16",
        Scalar::Float32 => "Float32",
        Scalar::Float64 => "Float64",
        _ => unreachable!("not a typed array element type: {t:?}"),
    }
}

// --- Shared shapes ---

/// Runs `f` on the object behind `obj` inside a handle scope of the context that owns it; `None` for a null object.
unsafe fn with_object<R>(obj: *mut JSObject, f: impl FnOnce(Bs, Handle<api::ObjectValue>) -> R) -> Option<R> {
    if obj.is_null() {
        return None;
    }
    let bs = embed::context_of(obj.cast());
    let scope = HandleScope::enter(bs);
    let o = BsValue::object(object_ptr(obj)).to_handle(bs).as_object();
    let r = f(bs, o);
    scope.exit();
    Some(r)
}

#[inline]
fn raw_of(o: Handle<api::ObjectValue>) -> *mut JSObject {
    (*o).as_ptr().cast()
}

#[inline]
unsafe fn handle_obj(bs: Bs, obj: JS::Handle<*mut JSObject>) -> Option<Handle<api::ObjectValue>> {
    let p = unsafe { *obj.ptr };
    if p.is_null() { None } else { Some(BsValue::object(object_ptr(p)).to_handle(bs).as_object()) }
}

/// A buffer's bytes: (data, byte length); null and 0 when detached.
fn buffer_bytes(buffer: Handle<api::ArrayBufferObject>) -> (*mut u8, usize) {
    let Some(base) = api::array_buffer_data_ptr(buffer) else {
        return (ptr::null_mut(), 0);
    };
    let len = api::array_buffer_byte_length(buffer);
    match bounds::view_range(len, 0, len, 1) {
        Some((s, e)) => (unsafe { base.add(s) }, e - s),
        None => (ptr::null_mut(), 0),
    }
}

/// What a typed array or DataView covers right now.
struct View {
    /// The element type; `MaxTypedArrayViewType` for a DataView, as `JS_GetArrayBufferViewType` reports.
    scalar: Scalar,
    data: *mut u8,
    /// In elements (bytes for a DataView).
    length: usize,
    byte_length: usize,
    byte_offset: usize,
    buffer: Handle<api::ArrayBufferObject>,
}

/// The view behind an object, or `None` if it is neither a typed array nor a DataView. A detached buffer or a view that no
/// longer fits its (resized) buffer is out of bounds: null data, zero lengths and offset, as SpiderMonkey's `valueOr(0)`.
fn view_of(o: Handle<api::ObjectValue>) -> Option<View> {
    let (buffer, offset, fixed, size, scalar) = if o.is_typed_array() {
        let ta = o.as_typed_array();
        (ta.viewed_array_buffer(), ta.byte_offset(), ta.array_length(), ta.element_size(), scalar_of(&ta.kind()))
    } else if let Some(dv) = api::as_data_view(o) {
        (dv.viewed_array_buffer(), dv.byte_offset(), dv.byte_length(), 1, Scalar::MaxTypedArrayViewType)
    } else {
        return None;
    };
    let empty = View { scalar, data: ptr::null_mut(), length: 0, byte_length: 0, byte_offset: 0, buffer };
    let Some(base) = api::array_buffer_data_ptr(buffer) else {
        return Some(empty);
    };
    let buffer_len = api::array_buffer_byte_length(buffer);
    // A length-tracking view covers whole elements up to the end of the buffer.
    let length = match fixed {
        Some(n) => n,
        None => match buffer_len.checked_sub(offset) {
            Some(room) => room / size,
            None => return Some(empty),
        },
    };
    match bounds::view_range(buffer_len, offset, length, size) {
        Some((s, e)) => Some(View {
            scalar,
            data: unsafe { base.add(s) },
            length,
            byte_length: e - s,
            byte_offset: offset,
            buffer,
        }),
        None => Some(empty),
    }
}

/// The view behind an object if it is a typed array of element type `t`.
fn typed_view_of(o: Handle<api::ObjectValue>, t: Scalar) -> Option<View> {
    view_of(o).filter(|v| v.scalar == t)
}

unsafe fn fail_with(cx: *mut JSContext, r: Result<(), EvalError>) -> bool {
    match r {
        Ok(()) => true,
        Err(e) => unsafe { fail(cx, e) },
    }
}

/// `ReportAccessDenied`: a plain Error, as SpiderMonkey's JSMSG_ACCESS_DENIED... which Brimstone only reaches for an object
/// of the wrong kind, since it has no wrappers to deny.
fn access_denied(bs: Bs) -> EvalError {
    let realm = embed::current_realm(bs);
    match api::new_error(bs, realm, "Permission denied to access object") {
        Ok(e) => embed::throw_value(e.as_value()),
        Err(e) => e,
    }
}

/// `UnwrapOrReportArrayBuffer`.
fn array_buffer_or_report(bs: Bs, o: Option<Handle<api::ObjectValue>>) -> Result<Handle<api::ArrayBufferObject>, EvalError> {
    match o.and_then(|o| o.as_opt::<api::ArrayBufferObject>()) {
        Some(b) => Ok(b),
        None => type_error(bs, "ArrayBuffer object required"),
    }
}

/// A new ArrayBuffer holding a copy of `len` bytes at `src` (null: zero-filled).
fn new_buffer_from(bs: Bs, src: *const u8, len: usize) -> Result<Handle<api::ArrayBufferObject>, EvalError> {
    let buffer = api::array_buffer_new(bs, embed::current_realm(bs), len)?;
    let (dst, dst_len) = buffer_bytes(buffer);
    if !src.is_null() && !dst.is_null() {
        unsafe { ptr::copy_nonoverlapping(src, dst, dst_len.min(len)) };
    }
    Ok(buffer)
}

// --- ArrayBuffers ---

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewArrayBuffer(cx: *mut JSContext, nbytes: usize) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = api::array_buffer_new(bs, embed::current_realm(bs), nbytes);
    let out = match r {
        Ok(b) => raw_of(b.as_object()),
        Err(e) => {
            unsafe { fail(cx, e) };
            ptr::null_mut()
        },
    };
    scope.exit();
    out
}

/// mozjs's glue: `JS::NewArrayBufferWithContents` with `JS::FreePolicy`, which owns `contents` from the call on. Brimstone
/// cannot adopt foreign memory, so the bytes are copied into a new buffer and `contents` is freed with `js_free` (libc
/// `free`), as the glue's `UniquePtr` would on either outcome.
#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__NewArrayBufferWithContents(
    cx: *mut JSContext,
    nbytes: usize,
    contents: *mut c_void,
) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    // Null contents make an empty buffer, whatever `nbytes` says.
    let len = if contents.is_null() { 0 } else { nbytes };
    let r = new_buffer_from(bs, contents.cast(), len);
    scope.exit();
    if !contents.is_null() {
        unsafe { libc::free(contents) };
    }
    match r {
        Ok(b) => raw_of(b.as_object()),
        Err(e) => {
            unsafe { fail(cx, e) };
            ptr::null_mut()
        },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CopyArrayBuffer(cx: *mut JSContext, maybeArrayBuffer: JS::Handle<*mut JSObject>) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = (|| {
        let source = array_buffer_or_report(bs, unsafe { handle_obj(bs, maybeArrayBuffer) })?;
        if api::array_buffer_is_detached(source) {
            return type_error(bs, "attempting to access detached ArrayBuffer");
        }
        let len = api::array_buffer_byte_length(source);
        let copy = api::array_buffer_new(bs, embed::current_realm(bs), len)?;
        // The source may have moved during the allocation: read its bytes only now.
        let (src, src_len) = buffer_bytes(source);
        let (dst, dst_len) = buffer_bytes(copy);
        if !src.is_null() && !dst.is_null() {
            unsafe { ptr::copy_nonoverlapping(src, dst, src_len.min(dst_len)) };
        }
        Ok(raw_of(copy.as_object()))
    })();
    scope.exit();
    match r {
        Ok(p) => p,
        Err(e) => {
            unsafe { fail(cx, e) };
            ptr::null_mut()
        },
    }
}

/// CopyDataBlockBytes between two buffers, refusing a range outside either (JSMSG_ARRAYBUFFER_COPY_RANGE).
fn copy_data(
    bs: Bs,
    to: Handle<api::ArrayBufferObject>,
    to_index: usize,
    from: Handle<api::ArrayBufferObject>,
    from_index: usize,
    count: usize,
) -> Result<(), EvalError> {
    let (dst, dst_len) = buffer_bytes(to);
    let (src, src_len) = buffer_bytes(from);
    let (Some((d, _)), Some((s, _))) =
        (bounds::view_range(dst_len, to_index, count, 1), bounds::view_range(src_len, from_index, count, 1))
    else {
        return range_error(bs, "ArrayBuffer range incorrect for copying");
    };
    if count > 0 {
        // Both ranges are inside their buffers, so neither pointer is null; the buffers may be the same one.
        unsafe { ptr::copy(src.add(s), dst.add(d), count) };
    }
    Ok(())
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ArrayBufferCopyData(
    cx: *mut JSContext,
    toBlock: JS::Handle<*mut JSObject>,
    toIndex: usize,
    fromBlock: JS::Handle<*mut JSObject>,
    fromIndex: usize,
    count: usize,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = (|| {
        let buffer = |h| unsafe { handle_obj(bs, h) }.and_then(|o| o.as_opt::<api::ArrayBufferObject>());
        let Some(to) = buffer(toBlock) else { return Err(access_denied(bs)) };
        let Some(from) = buffer(fromBlock) else { return Err(access_denied(bs)) };
        copy_data(bs, to, toIndex, from, fromIndex, count)
    })();
    scope.exit();
    unsafe { fail_with(cx, r) }
}

/// CloneArrayBuffer with %ArrayBuffer% as the constructor.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ArrayBufferClone(
    cx: *mut JSContext,
    srcBuffer: JS::Handle<*mut JSObject>,
    srcByteOffset: usize,
    srcLength: usize,
) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = (|| {
        let source = array_buffer_or_report(bs, unsafe { handle_obj(bs, srcBuffer) })?;
        if api::array_buffer_is_detached(source) {
            return type_error(bs, "attempting to access detached ArrayBuffer");
        }
        let target = api::array_buffer_new(bs, embed::current_realm(bs), srcLength)?;
        copy_data(bs, target, 0, source, srcByteOffset, srcLength)?;
        Ok(raw_of(target.as_object()))
    })();
    scope.exit();
    match r {
        Ok(p) => p,
        Err(e) => {
            unsafe { fail(cx, e) };
            ptr::null_mut()
        },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__DetachArrayBuffer(cx: *mut JSContext, obj: JS::Handle<*mut JSObject>) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    // Nothing else refuses a detach here: no buffer is immutable or has a pinned length, and the only detach key the engine
    // knows is a wasm memory's, which its owner detaches and an embedder may not.
    let r = array_buffer_or_report(bs, unsafe { handle_obj(bs, obj) }).and_then(|buffer| {
        if api::array_buffer_detach(buffer) {
            Ok(())
        } else {
            type_error(bs, "cannot detach the buffer of a WebAssembly.Memory")
        }
    });
    scope.exit();
    unsafe { fail_with(cx, r) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__HasDefinedArrayBufferDetachKey(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    isDefined: *mut bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = array_buffer_or_report(bs, unsafe { handle_obj(bs, obj) }).map(|buffer| {
        // SAFETY: `isDefined` is the caller's writable out-param, written exactly as SpiderMonkey's version writes it and only
        // for a buffer this function has just taken from a live handle.
        unsafe { *isDefined = api::array_buffer_detach_key(buffer) == api::DetachKey::WasmMemory }
    });
    scope.exit();
    unsafe { fail_with(cx, r) }
}

/// Detaches the buffer and hands its bytes to the caller in memory from `malloc`, which the caller frees with `JS_free`.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__StealArrayBufferContents(cx: *mut JSContext, obj: JS::Handle<*mut JSObject>) -> *mut c_void {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = (|| {
        let buffer = array_buffer_or_report(bs, unsafe { handle_obj(bs, obj) })?;
        if api::array_buffer_is_detached(buffer) {
            return type_error(bs, "attempting to access detached ArrayBuffer");
        }
        if api::array_buffer_detach_key(buffer) == api::DetachKey::WasmMemory {
            return type_error(bs, "cannot steal the buffer of a WebAssembly.Memory");
        }
        let (src, len) = buffer_bytes(buffer);
        // A zero-length buffer still yields a pointer the caller can free.
        let out = unsafe { libc::malloc(len.max(1)) };
        if out.is_null() {
            return range_error(bs, "out of memory");
        }
        if len > 0 {
            // SAFETY: this is the byte length of an attached buffer, which the two checks above established and which
            // `buffer_bytes` only reports with a non-null source; `len` bytes are read from it into the block malloc'd above.
            unsafe { ptr::copy_nonoverlapping(src, out.cast::<u8>(), len) };
        }
        api::array_buffer_detach(buffer);
        Ok(out)
    })();
    scope.exit();
    match r {
        Ok(p) => p,
        Err(e) => {
            unsafe { fail(cx, e) };
            ptr::null_mut()
        },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsArrayBufferObject(obj: *mut JSObject) -> bool {
    unsafe { with_object(obj, |_, o| o.is::<api::ArrayBufferObject>()) }.unwrap_or(false)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsArrayBufferObjectMaybeShared(obj: *mut JSObject) -> bool {
    unsafe { JS__IsArrayBufferObject(obj) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsSharedArrayBufferObject(_obj: *mut JSObject) -> bool {
    false
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ContainsSharedArrayBuffer(_cx: *mut JSContext) -> bool {
    false
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsDetachedArrayBufferObject(obj: *mut JSObject) -> bool {
    unsafe { with_object(obj, |_, o| o.as_opt::<api::ArrayBufferObject>().is_some_and(api::array_buffer_is_detached)) }
        .unwrap_or(false)
}

/// `obj` must be an ArrayBuffer (SpiderMonkey's `unwrapAs` asserts); anything else has no data.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ArrayBufferHasData(obj: *mut JSObject) -> bool {
    unsafe {
        with_object(obj, |_, o| o.as_opt::<api::ArrayBufferObject>().is_some_and(|b| !api::array_buffer_is_detached(b)))
    }
    .unwrap_or(false)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsMappedArrayBufferObject(_obj: *mut JSObject) -> bool {
    false
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsImmutableArrayBufferMaybeShared(_obj: *mut JSObject) -> bool {
    false
}

/// SpiderMonkey's `ByteLengthLimitForSmallBuffer`.
const SMALL_BUFFER_LIMIT: usize = i32::MAX as usize;

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsLargeArrayBufferMaybeShared(obj: *mut JSObject) -> bool {
    unsafe {
        with_object(obj, |_, o| {
            o.as_opt::<api::ArrayBufferObject>().is_some_and(|b| api::array_buffer_byte_length(b) > SMALL_BUFFER_LIMIT)
        })
    }
    .unwrap_or(false)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsResizableArrayBufferMaybeShared(obj: *mut JSObject) -> bool {
    unsafe { with_object(obj, |_, o| o.as_opt::<api::ArrayBufferObject>().is_some_and(|b| !b.is_fixed_length())) }
        .unwrap_or(false)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__UnwrapArrayBuffer(obj: *mut JSObject) -> *mut JSObject {
    if unsafe { JS__IsArrayBufferObject(obj) } { obj } else { ptr::null_mut() }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__UnwrapArrayBufferMaybeShared(obj: *mut JSObject) -> *mut JSObject {
    unsafe { JS__UnwrapArrayBuffer(obj) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__UnwrapSharedArrayBuffer(_obj: *mut JSObject) -> *mut JSObject {
    ptr::null_mut()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetArrayBufferByteLength(obj: *mut JSObject) -> usize {
    unsafe { with_object(obj, |_, o| o.as_opt::<api::ArrayBufferObject>().map_or(0, |b| buffer_bytes(b).1)) }
        .unwrap_or(0)
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS__GetArrayBufferData(
    obj: *mut JSObject,
    isSharedMemory: *mut bool,
    _nogc: *const JS::AutoRequireNoGC,
) -> *mut u8 {
    let data = unsafe { with_object(obj, |_, o| o.as_opt::<api::ArrayBufferObject>().map(|b| buffer_bytes(b).0)) };
    match data.flatten() {
        Some(p) => {
            unsafe { *isSharedMemory = false };
            p
        },
        None => ptr::null_mut(),
    }
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS__GetArrayBufferMaybeSharedData(
    obj: *mut JSObject,
    isSharedMemory: *mut bool,
    nogc: *const JS::AutoRequireNoGC,
) -> *mut u8 {
    unsafe { JS__GetArrayBufferData(obj, isSharedMemory, nogc) }
}

/// `obj` must be an ArrayBuffer (SpiderMonkey asserts); anything else reads as empty.
#[super::poison::engine_shim(failure = unsafe { *length = 0; *isSharedMemory = false; *data = ::std::ptr::null_mut() })]
pub unsafe extern "C" fn JS__GetArrayBufferLengthAndData(
    obj: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut u8,
) {
    let (p, len) = unsafe { with_object(obj, |_, o| o.as_opt::<api::ArrayBufferObject>().map(buffer_bytes)) }
        .flatten()
        .unwrap_or((ptr::null_mut(), 0));
    unsafe {
        *length = len;
        *data = p;
        *isSharedMemory = false;
    }
}

#[super::poison::engine_shim(failure = unsafe { *length = 0; *isSharedMemory = false; *data = ::std::ptr::null_mut() })]
pub unsafe extern "C" fn JS__GetArrayBufferMaybeSharedLengthAndData(
    obj: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut u8,
) {
    unsafe { JS__GetArrayBufferLengthAndData(obj, length, isSharedMemory, data) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetObjectAsArrayBuffer(obj: *mut JSObject, length: *mut usize, data: *mut *mut u8) -> *mut JSObject {
    let bytes = unsafe { with_object(obj, |_, o| o.as_opt::<api::ArrayBufferObject>().map(buffer_bytes)) }.flatten();
    match bytes {
        Some((p, len)) => {
            unsafe {
                *length = len;
                *data = p;
            }
            obj
        },
        None => ptr::null_mut(),
    }
}

// SharedArrayBuffers do not exist here, so nothing is one: these are only ever called on one.

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetSharedArrayBufferByteLength(_obj: *mut JSObject) -> usize {
    0
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS__GetSharedArrayBufferData(
    _obj: *mut JSObject,
    _isSharedMemory: *mut bool,
    _nogc: *const JS::AutoRequireNoGC,
) -> *mut u8 {
    ptr::null_mut()
}

#[super::poison::engine_shim(failure = unsafe { *length = 0; *isSharedMemory = false; *data = ::std::ptr::null_mut() })]
pub unsafe extern "C" fn JS__GetSharedArrayBufferLengthAndData(
    _obj: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut u8,
) {
    unsafe {
        *length = 0;
        *isSharedMemory = false;
        *data = ptr::null_mut();
    }
}

/// Buffer contents are never inline in an object here (they live in a separate `ByteArray`), so there is nothing to do for
/// a buffer or a view; anything else is refused as SpiderMonkey does.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__EnsureNonInlineArrayBufferOrView(cx: *mut JSContext, obj: *mut JSObject) -> bool {
    let ok = unsafe { with_object(obj, |_, o| o.is::<api::ArrayBufferObject>() || view_of(o).is_some()) }.unwrap_or(false);
    if ok {
        return true;
    }
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let e = match api::new_error(bs, realm, "unhandled type") {
        Ok(e) => embed::throw_value(e.as_value()),
        Err(e) => e,
    };
    scope.exit();
    unsafe { fail(cx, e) }
}

// --- Views: typed arrays and DataViews ---

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_IsArrayBufferViewObject(obj: *mut JSObject) -> bool {
    unsafe { with_object(obj, |_, o| o.is_typed_array() || api::as_data_view(o).is_some()) }.unwrap_or(false)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__UnwrapArrayBufferView(obj: *mut JSObject) -> *mut JSObject {
    if unsafe { JS_IsArrayBufferViewObject(obj) } { obj } else { ptr::null_mut() }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_IsTypedArrayObject(obj: *mut JSObject) -> bool {
    unsafe { with_object(obj, |_, o| o.is_typed_array()) }.unwrap_or(false)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetArrayBufferViewType(obj: *mut JSObject) -> Scalar {
    unsafe { with_object(obj, |_, o| view_of(o).map(|v| v.scalar)) }
        .flatten()
        .unwrap_or(Scalar::MaxTypedArrayViewType)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetArrayBufferViewData(
    obj: *mut JSObject,
    isSharedMemory: *mut bool,
    _nogc: *const JS::AutoRequireNoGC,
) -> *mut c_void {
    match unsafe { with_object(obj, |_, o| view_of(o).map(|v| v.data)) }.flatten() {
        Some(p) => {
            unsafe { *isSharedMemory = false };
            p.cast()
        },
        None => ptr::null_mut(),
    }
}

/// Copies the view's bytes into `buffer` when they fit, the case SpiderMonkey copies for (inline data, which moves with
/// its object; here all data can move); a larger view gets its data pointer, null if it does not fit its buffer.
#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS_GetArrayBufferViewFixedData(obj: *mut JSObject, buffer: *mut u8, bufSize: usize) -> *mut u8 {
    let view = unsafe { with_object(obj, |_, o| view_of(o).map(|v| (v.data, v.byte_length))) }.flatten();
    let Some((data, len)) = view else {
        return ptr::null_mut();
    };
    if data.is_null() || len > bufSize || buffer.is_null() {
        return data;
    }
    unsafe { ptr::copy_nonoverlapping(data, buffer, len) };
    buffer
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetArrayBufferViewByteLength(obj: *mut JSObject) -> usize {
    unsafe { with_object(obj, |_, o| view_of(o).map_or(0, |v| v.byte_length)) }.unwrap_or(0)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetArrayBufferViewByteOffset(obj: *mut JSObject) -> usize {
    unsafe { with_object(obj, |_, o| view_of(o).map_or(0, |v| v.byte_offset)) }.unwrap_or(0)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetArrayBufferViewBuffer(
    cx: *mut JSContext,
    obj: JS::Handle<*mut JSObject>,
    isSharedMemory: *mut bool,
) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let buffer = unsafe { handle_obj(bs, obj) }.and_then(view_of).map(|v| raw_of(v.buffer.as_object()));
    let r = match buffer {
        Some(b) => Ok(b),
        None => Err(access_denied(bs)),
    };
    scope.exit();
    match r {
        Ok(b) => {
            unsafe { *isSharedMemory = false };
            b
        },
        Err(e) => {
            unsafe { fail(cx, e) };
            ptr::null_mut()
        },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetObjectAsArrayBufferView(
    obj: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut u8,
) -> *mut JSObject {
    match unsafe { with_object(obj, |_, o| view_of(o).map(|v| (v.data, v.byte_length))) }.flatten() {
        Some((p, len)) => {
            unsafe {
                *length = len;
                *isSharedMemory = false;
                *data = p;
            }
            obj
        },
        None => ptr::null_mut(),
    }
}

/// `obj` must be a view (SpiderMonkey asserts); anything else reads as empty.
#[super::poison::engine_shim(failure = unsafe { *length = 0; *isSharedMemory = false; *data = ::std::ptr::null_mut() })]
pub unsafe extern "C" fn js__GetArrayBufferViewLengthAndData(
    obj: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut u8,
) {
    let (p, len) = unsafe { with_object(obj, |_, o| view_of(o).map(|v| (v.data, v.byte_length))) }
        .flatten()
        .unwrap_or((ptr::null_mut(), 0));
    unsafe {
        *length = len;
        *isSharedMemory = false;
        *data = p;
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsArrayBufferViewShared(_obj: *mut JSObject) -> bool {
    false
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsLargeArrayBufferView(obj: *mut JSObject) -> bool {
    unsafe { with_object(obj, |_, o| view_of(o).is_some_and(|v| v.byte_length > SMALL_BUFFER_LIMIT)) }.unwrap_or(false)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsResizableArrayBufferView(obj: *mut JSObject) -> bool {
    unsafe { with_object(obj, |_, o| view_of(o).is_some_and(|v| !v.buffer.is_fixed_length())) }.unwrap_or(false)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsImmutableArrayBufferView(_obj: *mut JSObject) -> bool {
    false
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetTypedArrayLength(obj: *mut JSObject) -> usize {
    unsafe { with_object(obj, |_, o| if o.is_typed_array() { view_of(o).map_or(0, |v| v.length) } else { 0 }) }
        .unwrap_or(0)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetTypedArrayByteOffset(obj: *mut JSObject) -> usize {
    unsafe { with_object(obj, |_, o| if o.is_typed_array() { view_of(o).map_or(0, |v| v.byte_offset) } else { 0 }) }
        .unwrap_or(0)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetTypedArrayByteLength(obj: *mut JSObject) -> usize {
    unsafe { with_object(obj, |_, o| if o.is_typed_array() { view_of(o).map_or(0, |v| v.byte_length) } else { 0 }) }
        .unwrap_or(0)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetTypedArraySharedness(_obj: *mut JSObject) -> bool {
    false
}

/// SpiderMonkey's `FixedLengthTypedArrayObject::INLINE_BUFFER_LIMIT`: (MAX_FIXED_SLOTS 16 - 4 reserved slots) * 8 bytes.
/// Callers size the buffer they pass to `JS_GetArrayBufferViewFixedData` by it.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_MaxMovableTypedArraySize() -> usize {
    (16 - 4) * ::std::mem::size_of::<JS::Value>()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewDataView(
    cx: *mut JSContext,
    buffer: JS::Handle<*mut JSObject>,
    byteOffset: usize,
    byteLength: usize,
) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    // `new DataView(buffer, byteOffset, byteLength)`, as SpiderMonkey does it.
    let r = (|| {
        let ctor = api::intrinsic(embed::current_realm(bs), api::Intrinsic::DataViewConstructor);
        let buffer = handle_of(bs, BsValue::object(object_ptr(unsafe { *buffer.ptr })).as_raw_bits());
        let offset = BsValue::number(byteOffset as f64).to_handle(bs);
        let length = BsValue::number(byteLength as f64).to_handle(bs);
        embed::construct(bs, ctor.as_value(), &[buffer, offset, length], Some(ctor)).map(raw_of)
    })();
    scope.exit();
    match r {
        Ok(p) => p,
        Err(e) => {
            unsafe { fail(cx, e) };
            ptr::null_mut()
        },
    }
}

// --- Typed arrays of one element type ---

/// `JS_New<T>Array`: a zero-filled array of `nelements` in a new buffer.
unsafe fn new_typed(cx: *mut JSContext, t: Scalar, nelements: usize) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = (|| {
        let Some(bytes) = nelements.checked_mul(element_size(t)) else {
            return range_error(bs, "invalid array length");
        };
        let realm = embed::current_realm(bs);
        let buffer = api::array_buffer_new(bs, realm, bytes)?;
        api::create_typed_array(bs, realm, kind_of(t), buffer, 0, Some(nelements)).map(raw_of)
    })();
    scope.exit();
    match r {
        Ok(p) => p,
        Err(e) => {
            unsafe { fail(cx, e) };
            ptr::null_mut()
        },
    }
}

/// `JS_New<T>ArrayFromArray`: `new <T>Array(array)`, which copies a typed array or reads an iterable or array-like.
unsafe fn new_typed_from_array(cx: *mut JSContext, t: Scalar, array: JS::Handle<*mut JSObject>) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = (|| {
        let ctor = api::intrinsic(embed::current_realm(bs), constructor_of(t));
        let source = handle_of(bs, BsValue::object(object_ptr(unsafe { *array.ptr })).as_raw_bits());
        embed::construct(bs, ctor.as_value(), &[source], Some(ctor)).map(raw_of)
    })();
    scope.exit();
    match r {
        Ok(p) => p,
        Err(e) => {
            unsafe { fail(cx, e) };
            ptr::null_mut()
        },
    }
}

/// `JS_New<T>ArrayWithBuffer`: a view of `length` elements (negative: the rest of the buffer) from `byteOffset`.
unsafe fn new_typed_with_buffer(
    cx: *mut JSContext,
    t: Scalar,
    buffer: JS::Handle<*mut JSObject>,
    byte_offset: usize,
    length: i64,
) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = (|| {
        let size = element_size(t);
        if byte_offset % size != 0 {
            let msg = format!("start offset of {}Array should be a multiple of {size}", element_name(t));
            return range_error(bs, &msg);
        }
        let Some(buffer) = unsafe { handle_obj(bs, buffer) }.and_then(|o| o.as_opt::<api::ArrayBufferObject>()) else {
            return type_error(bs, "invalid arguments");
        };
        let length = usize::try_from(length).ok();
        // The constructor refuses a detached buffer (TypeError) and a range outside the buffer (RangeError).
        api::create_typed_array(bs, embed::current_realm(bs), kind_of(t), buffer, byte_offset, length).map(raw_of)
    })();
    scope.exit();
    match r {
        Ok(p) => p,
        Err(e) => {
            unsafe { fail(cx, e) };
            ptr::null_mut()
        },
    }
}

/// `js::Unwrap<T>Array`.
unsafe fn unwrap_typed(obj: *mut JSObject, t: Scalar) -> *mut JSObject {
    let is = unsafe { with_object(obj, |_, o| o.is_typed_array() && scalar_of(&o.as_typed_array().kind()) == t) };
    if is.unwrap_or(false) { obj } else { ptr::null_mut() }
}

/// `JS_Get<T>ArrayData`: null if `obj` is not a `<T>Array`.
unsafe fn typed_data<E>(obj: *mut JSObject, t: Scalar, isSharedMemory: *mut bool) -> *mut E {
    match unsafe { with_object(obj, |_, o| typed_view_of(o, t).map(|v| v.data)) }.flatten() {
        Some(p) => {
            unsafe { *isSharedMemory = false };
            p.cast()
        },
        None => ptr::null_mut(),
    }
}

/// `JS_GetObjectAs<T>Array`: null if `obj` is not a `<T>Array`, else `obj` with its element length and data.
unsafe fn object_as_typed<E>(
    obj: *mut JSObject,
    t: Scalar,
    length: *mut usize,
    isShared: *mut bool,
    data: *mut *mut E,
) -> *mut JSObject {
    match unsafe { with_object(obj, |_, o| typed_view_of(o, t).map(|v| (v.data, v.length))) }.flatten() {
        Some((p, len)) => {
            unsafe {
                *length = len;
                *isShared = false;
                *data = p.cast();
            }
            obj
        },
        None => ptr::null_mut(),
    }
}

/// `js::Get<T>ArrayLengthAndData`: `obj` must be a `<T>Array` (SpiderMonkey asserts); anything else reads as empty.
unsafe fn typed_length_and_data<E>(
    obj: *mut JSObject,
    t: Scalar,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut E,
) {
    let (p, len) = unsafe { with_object(obj, |_, o| typed_view_of(o, t).map(|v| (v.data, v.length))) }
        .flatten()
        .unwrap_or((ptr::null_mut(), 0));
    unsafe {
        *length = len;
        *isSharedMemory = false;
        *data = p.cast();
    }
}

// --- Numbers ---

/// `mozilla::NumberIsInt32`: an int32 exactly, and not -0.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DoubleIsInt32(d: f64, ip: *mut i32) -> bool {
    let i = d as i32;
    if i as f64 == d && !(d == 0.0 && d.is_sign_negative()) {
        unsafe { *ip = i };
        true
    } else {
        false
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__JS_ValueIsInt32(value: *const JS::Value) -> bool {
    let bits = unsafe { (*value).asBits_ };
    matches!(crate::boundary::value_bits::kind(bits), crate::boundary::value_bits::Kind::Int32)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__JS_ValueToInt32(value: *const JS::Value) -> i32 {
    crate::boundary::value_bits::to_int32(unsafe { (*value).asBits_ })
}

#[super::poison::engine_shim(failure = unsafe { (*value).asBits_ = crate::boundary::value_bits::int32(x) })]
pub unsafe extern "C" fn glue__JS_ValueSetInt32(value: *mut JS::Value, x: i32) {
    unsafe { (*value).asBits_ = crate::boundary::value_bits::int32(x) };
}

/// ToInt32 / ToUint32 of a value, through ToNumber (which may run script and throw).
unsafe fn to_int32_of(cx: *mut JSContext, v: JS::HandleValue) -> Result<i32, EvalError> {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = api::to_int32(bs, handle_of(bs, unsafe { (*v.ptr).asBits_ }));
    scope.exit();
    r
}

unsafe fn to_uint32_of(cx: *mut JSContext, v: JS::HandleValue) -> Result<u32, EvalError> {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r = api::to_uint32(bs, handle_of(bs, unsafe { (*v.ptr).asBits_ }));
    scope.exit();
    r
}

/// Stores a modular conversion's result, or records its exception.
unsafe fn store<T>(cx: *mut JSContext, r: Result<T, EvalError>, out: *mut T) -> bool {
    match r {
        Ok(x) => {
            unsafe { *out = x };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ToInt32Slow(cx: *mut JSContext, v: JS::HandleValue, out: *mut i32) -> bool {
    unsafe { store(cx, to_int32_of(cx, v), out) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ToUint32Slow(cx: *mut JSContext, v: JS::HandleValue, out: *mut u32) -> bool {
    unsafe { store(cx, to_uint32_of(cx, v), out) }
}

// ToInt16, ToInt8, ToUint16 and ToUint8 are ToNumber modulo 2^16 or 2^8, which is ToInt32's result truncated, since 2^32 is
// a multiple of both.

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ToInt16Slow(cx: *mut JSContext, v: JS::HandleValue, out: *mut i16) -> bool {
    unsafe { store(cx, to_int32_of(cx, v).map(|i| i as i16), out) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ToUint16Slow(cx: *mut JSContext, v: JS::HandleValue, out: *mut u16) -> bool {
    unsafe { store(cx, to_int32_of(cx, v).map(|i| i as u16), out) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ToInt8Slow(cx: *mut JSContext, v: JS::HandleValue, out: *mut i8) -> bool {
    unsafe { store(cx, to_int32_of(cx, v).map(|i| i as i8), out) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__ToUint8Slow(cx: *mut JSContext, v: JS::HandleValue, out: *mut u8) -> bool {
    unsafe { store(cx, to_int32_of(cx, v).map(|i| i as u8), out) }
}

/// The low 64 bits of a BigInt in two's complement (BigInt.asUintN(64)).
unsafe fn bigint_low64(bi: *const JS::BigInt) -> u64 {
    let b: HeapPtr<api::BigIntValue> = HeapPtr::from_ptr(bi as *mut _);
    let value = b.bigint();
    let low = value.iter_u64_digits().next().unwrap_or(0);
    if value < api::BigInt::from(0) { low.wrapping_neg() } else { low }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ToBigInt64(bi: *const JS::BigInt) -> i64 {
    unsafe { bigint_low64(bi) as i64 }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ToBigUint64(bi: *const JS::BigInt) -> u64 {
    unsafe { bigint_low64(bi) }
}

// --- Per element type ---
// Int8Array

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewInt8Array(cx: *mut JSContext, nelements: usize) -> *mut JSObject {
    unsafe { new_typed(cx, Scalar::Int8, nelements) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewInt8ArrayFromArray(cx: *mut JSContext, array: JS::Handle<*mut JSObject>) -> *mut JSObject {
    unsafe { new_typed_from_array(cx, Scalar::Int8, array) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewInt8ArrayWithBuffer(
    cx: *mut JSContext,
    arrayBuffer: JS::Handle<*mut JSObject>,
    byteOffset: usize,
    length: i64,
) -> *mut JSObject {
    unsafe { new_typed_with_buffer(cx, Scalar::Int8, arrayBuffer, byteOffset, length) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__UnwrapInt8Array(maybeWrapped: *mut JSObject) -> *mut JSObject {
    unsafe { unwrap_typed(maybeWrapped, Scalar::Int8) }
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS_GetInt8ArrayData(
    maybeWrapped: *mut JSObject,
    isSharedMemory: *mut bool,
    _nogc: *const JS::AutoRequireNoGC,
) -> *mut i8 {
    unsafe { typed_data(maybeWrapped, Scalar::Int8, isSharedMemory) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetObjectAsInt8Array(
    maybeWrapped: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut i8,
) -> *mut JSObject {
    unsafe { object_as_typed(maybeWrapped, Scalar::Int8, length, isSharedMemory, data) }
}

#[super::poison::engine_shim(failure = unsafe { *length = 0; *isSharedMemory = false; *data = ::std::ptr::null_mut() })]
pub unsafe extern "C" fn glue__GetInt8ArrayLengthAndData(
    obj: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut i8,
) {
    unsafe { typed_length_and_data(obj, Scalar::Int8, length, isSharedMemory, data) }
}

// Uint8Array

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewUint8Array(cx: *mut JSContext, nelements: usize) -> *mut JSObject {
    unsafe { new_typed(cx, Scalar::Uint8, nelements) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewUint8ArrayFromArray(cx: *mut JSContext, array: JS::Handle<*mut JSObject>) -> *mut JSObject {
    unsafe { new_typed_from_array(cx, Scalar::Uint8, array) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewUint8ArrayWithBuffer(
    cx: *mut JSContext,
    arrayBuffer: JS::Handle<*mut JSObject>,
    byteOffset: usize,
    length: i64,
) -> *mut JSObject {
    unsafe { new_typed_with_buffer(cx, Scalar::Uint8, arrayBuffer, byteOffset, length) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__UnwrapUint8Array(maybeWrapped: *mut JSObject) -> *mut JSObject {
    unsafe { unwrap_typed(maybeWrapped, Scalar::Uint8) }
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS_GetUint8ArrayData(
    maybeWrapped: *mut JSObject,
    isSharedMemory: *mut bool,
    _nogc: *const JS::AutoRequireNoGC,
) -> *mut u8 {
    unsafe { typed_data(maybeWrapped, Scalar::Uint8, isSharedMemory) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetObjectAsUint8Array(
    maybeWrapped: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut u8,
) -> *mut JSObject {
    unsafe { object_as_typed(maybeWrapped, Scalar::Uint8, length, isSharedMemory, data) }
}

#[super::poison::engine_shim(failure = unsafe { *length = 0; *isSharedMemory = false; *data = ::std::ptr::null_mut() })]
pub unsafe extern "C" fn glue__GetUint8ArrayLengthAndData(
    obj: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut u8,
) {
    unsafe { typed_length_and_data(obj, Scalar::Uint8, length, isSharedMemory, data) }
}

// Uint8ClampedArray

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewUint8ClampedArray(cx: *mut JSContext, nelements: usize) -> *mut JSObject {
    unsafe { new_typed(cx, Scalar::Uint8Clamped, nelements) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewUint8ClampedArrayFromArray(cx: *mut JSContext, array: JS::Handle<*mut JSObject>) -> *mut JSObject {
    unsafe { new_typed_from_array(cx, Scalar::Uint8Clamped, array) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewUint8ClampedArrayWithBuffer(
    cx: *mut JSContext,
    arrayBuffer: JS::Handle<*mut JSObject>,
    byteOffset: usize,
    length: i64,
) -> *mut JSObject {
    unsafe { new_typed_with_buffer(cx, Scalar::Uint8Clamped, arrayBuffer, byteOffset, length) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__UnwrapUint8ClampedArray(maybeWrapped: *mut JSObject) -> *mut JSObject {
    unsafe { unwrap_typed(maybeWrapped, Scalar::Uint8Clamped) }
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS_GetUint8ClampedArrayData(
    maybeWrapped: *mut JSObject,
    isSharedMemory: *mut bool,
    _nogc: *const JS::AutoRequireNoGC,
) -> *mut u8 {
    unsafe { typed_data(maybeWrapped, Scalar::Uint8Clamped, isSharedMemory) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetObjectAsUint8ClampedArray(
    maybeWrapped: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut u8,
) -> *mut JSObject {
    unsafe { object_as_typed(maybeWrapped, Scalar::Uint8Clamped, length, isSharedMemory, data) }
}

#[super::poison::engine_shim(failure = unsafe { *length = 0; *isSharedMemory = false; *data = ::std::ptr::null_mut() })]
pub unsafe extern "C" fn glue__GetUint8ClampedArrayLengthAndData(
    obj: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut u8,
) {
    unsafe { typed_length_and_data(obj, Scalar::Uint8Clamped, length, isSharedMemory, data) }
}

// Int16Array

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewInt16Array(cx: *mut JSContext, nelements: usize) -> *mut JSObject {
    unsafe { new_typed(cx, Scalar::Int16, nelements) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewInt16ArrayFromArray(cx: *mut JSContext, array: JS::Handle<*mut JSObject>) -> *mut JSObject {
    unsafe { new_typed_from_array(cx, Scalar::Int16, array) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewInt16ArrayWithBuffer(
    cx: *mut JSContext,
    arrayBuffer: JS::Handle<*mut JSObject>,
    byteOffset: usize,
    length: i64,
) -> *mut JSObject {
    unsafe { new_typed_with_buffer(cx, Scalar::Int16, arrayBuffer, byteOffset, length) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__UnwrapInt16Array(maybeWrapped: *mut JSObject) -> *mut JSObject {
    unsafe { unwrap_typed(maybeWrapped, Scalar::Int16) }
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS_GetInt16ArrayData(
    maybeWrapped: *mut JSObject,
    isSharedMemory: *mut bool,
    _nogc: *const JS::AutoRequireNoGC,
) -> *mut i16 {
    unsafe { typed_data(maybeWrapped, Scalar::Int16, isSharedMemory) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetObjectAsInt16Array(
    maybeWrapped: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut i16,
) -> *mut JSObject {
    unsafe { object_as_typed(maybeWrapped, Scalar::Int16, length, isSharedMemory, data) }
}

#[super::poison::engine_shim(failure = unsafe { *length = 0; *isSharedMemory = false; *data = ::std::ptr::null_mut() })]
pub unsafe extern "C" fn glue__GetInt16ArrayLengthAndData(
    obj: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut i16,
) {
    unsafe { typed_length_and_data(obj, Scalar::Int16, length, isSharedMemory, data) }
}

// Uint16Array

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewUint16Array(cx: *mut JSContext, nelements: usize) -> *mut JSObject {
    unsafe { new_typed(cx, Scalar::Uint16, nelements) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewUint16ArrayFromArray(cx: *mut JSContext, array: JS::Handle<*mut JSObject>) -> *mut JSObject {
    unsafe { new_typed_from_array(cx, Scalar::Uint16, array) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewUint16ArrayWithBuffer(
    cx: *mut JSContext,
    arrayBuffer: JS::Handle<*mut JSObject>,
    byteOffset: usize,
    length: i64,
) -> *mut JSObject {
    unsafe { new_typed_with_buffer(cx, Scalar::Uint16, arrayBuffer, byteOffset, length) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__UnwrapUint16Array(maybeWrapped: *mut JSObject) -> *mut JSObject {
    unsafe { unwrap_typed(maybeWrapped, Scalar::Uint16) }
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS_GetUint16ArrayData(
    maybeWrapped: *mut JSObject,
    isSharedMemory: *mut bool,
    _nogc: *const JS::AutoRequireNoGC,
) -> *mut u16 {
    unsafe { typed_data(maybeWrapped, Scalar::Uint16, isSharedMemory) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetObjectAsUint16Array(
    maybeWrapped: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut u16,
) -> *mut JSObject {
    unsafe { object_as_typed(maybeWrapped, Scalar::Uint16, length, isSharedMemory, data) }
}

#[super::poison::engine_shim(failure = unsafe { *length = 0; *isSharedMemory = false; *data = ::std::ptr::null_mut() })]
pub unsafe extern "C" fn glue__GetUint16ArrayLengthAndData(
    obj: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut u16,
) {
    unsafe { typed_length_and_data(obj, Scalar::Uint16, length, isSharedMemory, data) }
}

// Int32Array

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewInt32Array(cx: *mut JSContext, nelements: usize) -> *mut JSObject {
    unsafe { new_typed(cx, Scalar::Int32, nelements) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewInt32ArrayFromArray(cx: *mut JSContext, array: JS::Handle<*mut JSObject>) -> *mut JSObject {
    unsafe { new_typed_from_array(cx, Scalar::Int32, array) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewInt32ArrayWithBuffer(
    cx: *mut JSContext,
    arrayBuffer: JS::Handle<*mut JSObject>,
    byteOffset: usize,
    length: i64,
) -> *mut JSObject {
    unsafe { new_typed_with_buffer(cx, Scalar::Int32, arrayBuffer, byteOffset, length) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__UnwrapInt32Array(maybeWrapped: *mut JSObject) -> *mut JSObject {
    unsafe { unwrap_typed(maybeWrapped, Scalar::Int32) }
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS_GetInt32ArrayData(
    maybeWrapped: *mut JSObject,
    isSharedMemory: *mut bool,
    _nogc: *const JS::AutoRequireNoGC,
) -> *mut i32 {
    unsafe { typed_data(maybeWrapped, Scalar::Int32, isSharedMemory) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetObjectAsInt32Array(
    maybeWrapped: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut i32,
) -> *mut JSObject {
    unsafe { object_as_typed(maybeWrapped, Scalar::Int32, length, isSharedMemory, data) }
}

#[super::poison::engine_shim(failure = unsafe { *length = 0; *isSharedMemory = false; *data = ::std::ptr::null_mut() })]
pub unsafe extern "C" fn glue__GetInt32ArrayLengthAndData(
    obj: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut i32,
) {
    unsafe { typed_length_and_data(obj, Scalar::Int32, length, isSharedMemory, data) }
}

// Uint32Array

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewUint32Array(cx: *mut JSContext, nelements: usize) -> *mut JSObject {
    unsafe { new_typed(cx, Scalar::Uint32, nelements) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewUint32ArrayFromArray(cx: *mut JSContext, array: JS::Handle<*mut JSObject>) -> *mut JSObject {
    unsafe { new_typed_from_array(cx, Scalar::Uint32, array) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewUint32ArrayWithBuffer(
    cx: *mut JSContext,
    arrayBuffer: JS::Handle<*mut JSObject>,
    byteOffset: usize,
    length: i64,
) -> *mut JSObject {
    unsafe { new_typed_with_buffer(cx, Scalar::Uint32, arrayBuffer, byteOffset, length) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__UnwrapUint32Array(maybeWrapped: *mut JSObject) -> *mut JSObject {
    unsafe { unwrap_typed(maybeWrapped, Scalar::Uint32) }
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS_GetUint32ArrayData(
    maybeWrapped: *mut JSObject,
    isSharedMemory: *mut bool,
    _nogc: *const JS::AutoRequireNoGC,
) -> *mut u32 {
    unsafe { typed_data(maybeWrapped, Scalar::Uint32, isSharedMemory) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetObjectAsUint32Array(
    maybeWrapped: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut u32,
) -> *mut JSObject {
    unsafe { object_as_typed(maybeWrapped, Scalar::Uint32, length, isSharedMemory, data) }
}

#[super::poison::engine_shim(failure = unsafe { *length = 0; *isSharedMemory = false; *data = ::std::ptr::null_mut() })]
pub unsafe extern "C" fn glue__GetUint32ArrayLengthAndData(
    obj: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut u32,
) {
    unsafe { typed_length_and_data(obj, Scalar::Uint32, length, isSharedMemory, data) }
}

// Float16Array

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewFloat16Array(cx: *mut JSContext, nelements: usize) -> *mut JSObject {
    unsafe { new_typed(cx, Scalar::Float16, nelements) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewFloat16ArrayFromArray(cx: *mut JSContext, array: JS::Handle<*mut JSObject>) -> *mut JSObject {
    unsafe { new_typed_from_array(cx, Scalar::Float16, array) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewFloat16ArrayWithBuffer(
    cx: *mut JSContext,
    arrayBuffer: JS::Handle<*mut JSObject>,
    byteOffset: usize,
    length: i64,
) -> *mut JSObject {
    unsafe { new_typed_with_buffer(cx, Scalar::Float16, arrayBuffer, byteOffset, length) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__UnwrapFloat16Array(maybeWrapped: *mut JSObject) -> *mut JSObject {
    unsafe { unwrap_typed(maybeWrapped, Scalar::Float16) }
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS_GetFloat16ArrayData(
    maybeWrapped: *mut JSObject,
    isSharedMemory: *mut bool,
    _nogc: *const JS::AutoRequireNoGC,
) -> *mut u16 {
    unsafe { typed_data(maybeWrapped, Scalar::Float16, isSharedMemory) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetObjectAsFloat16Array(
    maybeWrapped: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut u16,
) -> *mut JSObject {
    unsafe { object_as_typed(maybeWrapped, Scalar::Float16, length, isSharedMemory, data) }
}

// Float32Array

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewFloat32Array(cx: *mut JSContext, nelements: usize) -> *mut JSObject {
    unsafe { new_typed(cx, Scalar::Float32, nelements) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewFloat32ArrayFromArray(cx: *mut JSContext, array: JS::Handle<*mut JSObject>) -> *mut JSObject {
    unsafe { new_typed_from_array(cx, Scalar::Float32, array) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewFloat32ArrayWithBuffer(
    cx: *mut JSContext,
    arrayBuffer: JS::Handle<*mut JSObject>,
    byteOffset: usize,
    length: i64,
) -> *mut JSObject {
    unsafe { new_typed_with_buffer(cx, Scalar::Float32, arrayBuffer, byteOffset, length) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__UnwrapFloat32Array(maybeWrapped: *mut JSObject) -> *mut JSObject {
    unsafe { unwrap_typed(maybeWrapped, Scalar::Float32) }
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS_GetFloat32ArrayData(
    maybeWrapped: *mut JSObject,
    isSharedMemory: *mut bool,
    _nogc: *const JS::AutoRequireNoGC,
) -> *mut f32 {
    unsafe { typed_data(maybeWrapped, Scalar::Float32, isSharedMemory) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetObjectAsFloat32Array(
    maybeWrapped: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut f32,
) -> *mut JSObject {
    unsafe { object_as_typed(maybeWrapped, Scalar::Float32, length, isSharedMemory, data) }
}

#[super::poison::engine_shim(failure = unsafe { *length = 0; *isSharedMemory = false; *data = ::std::ptr::null_mut() })]
pub unsafe extern "C" fn glue__GetFloat32ArrayLengthAndData(
    obj: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut f32,
) {
    unsafe { typed_length_and_data(obj, Scalar::Float32, length, isSharedMemory, data) }
}

// Float64Array

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewFloat64Array(cx: *mut JSContext, nelements: usize) -> *mut JSObject {
    unsafe { new_typed(cx, Scalar::Float64, nelements) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewFloat64ArrayFromArray(cx: *mut JSContext, array: JS::Handle<*mut JSObject>) -> *mut JSObject {
    unsafe { new_typed_from_array(cx, Scalar::Float64, array) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewFloat64ArrayWithBuffer(
    cx: *mut JSContext,
    arrayBuffer: JS::Handle<*mut JSObject>,
    byteOffset: usize,
    length: i64,
) -> *mut JSObject {
    unsafe { new_typed_with_buffer(cx, Scalar::Float64, arrayBuffer, byteOffset, length) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__UnwrapFloat64Array(maybeWrapped: *mut JSObject) -> *mut JSObject {
    unsafe { unwrap_typed(maybeWrapped, Scalar::Float64) }
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS_GetFloat64ArrayData(
    maybeWrapped: *mut JSObject,
    isSharedMemory: *mut bool,
    _nogc: *const JS::AutoRequireNoGC,
) -> *mut f64 {
    unsafe { typed_data(maybeWrapped, Scalar::Float64, isSharedMemory) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetObjectAsFloat64Array(
    maybeWrapped: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut f64,
) -> *mut JSObject {
    unsafe { object_as_typed(maybeWrapped, Scalar::Float64, length, isSharedMemory, data) }
}

#[super::poison::engine_shim(failure = unsafe { *length = 0; *isSharedMemory = false; *data = ::std::ptr::null_mut() })]
pub unsafe extern "C" fn glue__GetFloat64ArrayLengthAndData(
    obj: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut f64,
) {
    unsafe { typed_length_and_data(obj, Scalar::Float64, length, isSharedMemory, data) }
}

// BigInt64Array

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewBigInt64Array(cx: *mut JSContext, nelements: usize) -> *mut JSObject {
    unsafe { new_typed(cx, Scalar::BigInt64, nelements) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewBigInt64ArrayFromArray(cx: *mut JSContext, array: JS::Handle<*mut JSObject>) -> *mut JSObject {
    unsafe { new_typed_from_array(cx, Scalar::BigInt64, array) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewBigInt64ArrayWithBuffer(
    cx: *mut JSContext,
    arrayBuffer: JS::Handle<*mut JSObject>,
    byteOffset: usize,
    length: i64,
) -> *mut JSObject {
    unsafe { new_typed_with_buffer(cx, Scalar::BigInt64, arrayBuffer, byteOffset, length) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__UnwrapBigInt64Array(maybeWrapped: *mut JSObject) -> *mut JSObject {
    unsafe { unwrap_typed(maybeWrapped, Scalar::BigInt64) }
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS_GetBigInt64ArrayData(
    maybeWrapped: *mut JSObject,
    isSharedMemory: *mut bool,
    _nogc: *const JS::AutoRequireNoGC,
) -> *mut i64 {
    unsafe { typed_data(maybeWrapped, Scalar::BigInt64, isSharedMemory) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetObjectAsBigInt64Array(
    maybeWrapped: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut i64,
) -> *mut JSObject {
    unsafe { object_as_typed(maybeWrapped, Scalar::BigInt64, length, isSharedMemory, data) }
}

// BigUint64Array

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewBigUint64Array(cx: *mut JSContext, nelements: usize) -> *mut JSObject {
    unsafe { new_typed(cx, Scalar::BigUint64, nelements) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewBigUint64ArrayFromArray(cx: *mut JSContext, array: JS::Handle<*mut JSObject>) -> *mut JSObject {
    unsafe { new_typed_from_array(cx, Scalar::BigUint64, array) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewBigUint64ArrayWithBuffer(
    cx: *mut JSContext,
    arrayBuffer: JS::Handle<*mut JSObject>,
    byteOffset: usize,
    length: i64,
) -> *mut JSObject {
    unsafe { new_typed_with_buffer(cx, Scalar::BigUint64, arrayBuffer, byteOffset, length) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__UnwrapBigUint64Array(maybeWrapped: *mut JSObject) -> *mut JSObject {
    unsafe { unwrap_typed(maybeWrapped, Scalar::BigUint64) }
}

#[super::poison::engine_shim(failure = ::std::ptr::null_mut())]
pub unsafe extern "C" fn JS_GetBigUint64ArrayData(
    maybeWrapped: *mut JSObject,
    isSharedMemory: *mut bool,
    _nogc: *const JS::AutoRequireNoGC,
) -> *mut u64 {
    unsafe { typed_data(maybeWrapped, Scalar::BigUint64, isSharedMemory) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetObjectAsBigUint64Array(
    maybeWrapped: *mut JSObject,
    length: *mut usize,
    isSharedMemory: *mut bool,
    data: *mut *mut u64,
) -> *mut JSObject {
    unsafe { object_as_typed(maybeWrapped, Scalar::BigUint64, length, isSharedMemory, data) }
}
