/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Structured clone: `JS_WriteStructuredClone`, `JS_ReadStructuredClone`, `JS_StructuredClone` and the
//! `JSAutoStructuredCloneBuffer` / `JSStructuredCloneData` glue. The original is mozjs_sys 153.6.0-1's
//! `js/src/vm/StructuredClone.cpp`, `js/public/StructuredClone.h` and the glue in `src/jsglue.cpp`.
//!
//! The byte format is ours (Servo only reads what this engine wrote), but it is versioned and every
//! read is bounds-checked: a header word with a magic and a format version, then a stream of tagged
//! records. Malformed or truncated data (it may come from disk via IndexedDB) produces an error,
//! never a panic or an out-of-bounds read. All reading lives in `Reader`, one function family a later
//! Verus pass can check.
//!
//! `JSAutoStructuredCloneBuffer` and `JSStructuredCloneData` are opaque to Servo: they are allocated
//! here as `Box`es, and the clone bytes live in a `CloneDataInner` whose pointer is stored in the
//! data's opaque `bufList_` word.

use ::std::ffi::{c_void, CString};
use ::std::ptr;

use brimstone_core::runtime::embed::{self, api};
use brimstone_core::runtime::error::type_error;
use brimstone_core::runtime::eval_result::EvalError;
use brimstone_core::runtime::gc::{HandleScope, HeapPtr};
use brimstone_core::runtime::intrinsics::array_buffer_object::ArrayBufferObject;
use brimstone_core::runtime::intrinsics::bigint_object::BigIntObject;
use brimstone_core::runtime::intrinsics::boolean_object::BooleanObject;
use brimstone_core::runtime::intrinsics::date_object::DateObject;
use brimstone_core::runtime::intrinsics::error_object::ErrorObject;
use brimstone_core::runtime::intrinsics::map_object::MapObject;
use brimstone_core::runtime::intrinsics::number_object::NumberObject;
use brimstone_core::runtime::intrinsics::object_prototype_object::ObjectPrototypeObject;
use brimstone_core::runtime::intrinsics::regexp_object::RegExpObject;
use brimstone_core::runtime::intrinsics::set_object::SetObject;
use brimstone_core::runtime::ordinary_object::OrdinaryObject;
use brimstone_core::runtime::{Context as Bs, Handle, PropertyKey, Value as BsValue};

use super::cx::cx_of;
use super::func::with_slots;
use super::realm::object_ptr;
use super::util::{fail, handle_of};
use crate::boundary::value_bits;
use crate::jsapi::*;

// ---- Byte format ----

/// The header word's magic ("LCNE") and the format version it carries.
const HEADER_MAGIC: u64 = 0x4C43_4E45;
const FORMAT_VERSION: u32 = 1;

/// SpiderMonkey's `JS_SCTAG_USER_MIN`: host-object tags are at or above this.
const SCTAG_USER_MIN: u32 = 0xFFFF_8000;

// Record tags. Host callbacks write their own tags (>= SCTAG_USER_MIN) straight into the stream.
const TAG_BACKREF: u32 = 1;
const TAG_UNDEFINED: u32 = 2;
const TAG_NULL: u32 = 3;
const TAG_BOOLEAN: u32 = 4;
const TAG_NUMBER: u32 = 5;
const TAG_STRING: u32 = 6;
const TAG_BIGINT: u32 = 7;
const TAG_BOOLEAN_OBJ: u32 = 8;
const TAG_NUMBER_OBJ: u32 = 9;
const TAG_STRING_OBJ: u32 = 10;
const TAG_BIGINT_OBJ: u32 = 11;
const TAG_DATE: u32 = 12;
const TAG_REGEXP: u32 = 13;
const TAG_ARRAY: u32 = 14;
const TAG_OBJECT: u32 = 15;
const TAG_MAP: u32 = 16;
const TAG_SET: u32 = 17;
const TAG_ARRAYBUFFER: u32 = 18;
const TAG_TRANSFER: u32 = 19;
const TAG_TYPEDARRAY: u32 = 20;
const TAG_DATAVIEW: u32 = 21;
const TAG_ERROR: u32 = 22;

/// The fallback depth cap, used only when the context has no native stack quota. The root object is level 1, an
/// object held by a property, element, entry or cause of the level-n object is level n + 1. A deeper value is
/// refused as "nested too deeply" (a DataCloneError through the host's `reportError`), never run into the stack.
///
/// `write_value` and `Reader::descend` recurse once per level and a page builds any depth with a loop,
/// so the real bound is the stack and not a number: `too_deep` compares the stack pointer against the context's
/// native stack limit, the way SpiderMonkey guards the recursion in its own `StructuredClone.cpp` with
/// `AutoCheckRecursionLimit`. `JS_SetNativeStackQuota` (cx.rs) is what installs that limit, so on a runtime set up
/// like Servo's this constant is never consulted. It is the cap for a context with no quota --
/// `JS_SetNativeStackQuota(cx, 0, 0, 0)`, or one that never went through a quota call -- where there is no address
/// to compare against.
///
/// 128 is not a margin claim. The smallest stack that runs this code is a worker's: Servo spawns script threads
/// with 8 MiB (components/script/event_loop/script_thread.rs:501) but dedicated and shared workers with
/// `thread::Builder::new()` and no `stack_size` (dedicatedworkerglobalscope.rs:455, sharedworkerglobalscope.rs:402),
/// Rust's 2 MiB default. A debug clone frame is about 3.9 KiB (see `CLONE_STACK_RED_ZONE`), so ~500 reader levels
/// fit and 128 fits with them -- but that is Servo's quota stopping the walk about 1 MiB above the end of that
/// thread, not a property of this counter, which also resets on every re-entrant `JS_WriteStructuredClone`. Raise
/// the quota toward the real stack, or enter the clone from Rust already past it, and 128 frames walk off the end;
/// `too_deep` is what covers that. Real data (JSON, settings, history state) is a few levels deep.
const MAX_CLONE_DEPTH: usize = 128;

/// The stack one more clone frame still needs after `too_deep` last said there was room: a level of the recursion
/// plus the refusal's error path on that frame. Measured in a debug build on a 2 MiB thread by printing the
/// address of a `black_box`'d local at two nested calls of the real recursion (temporary instrumentation of
/// `write_value` and `Reader::descend`): the object chain costs 1,952 bytes per writer level and 3,888 per reader
/// level, and the largest frame of any kind is the reader of an `Error` chain at 4,976. 64 KiB is 13 of those,
/// and the error path starts close to the check (a `clone_error` frame's local measured 336 bytes below the
/// refusal in `descend`), so what is left of the red zone is several times the walk into `api::new_error`.
const CLONE_STACK_RED_ZONE: usize = 64 * 1024;

/// Whether a nested object may still be walked: by the stack when the context has a native stack quota (the same
/// comparison the VM makes at native entry, `bytecode/vm.rs` `is_native_stack_exhausted`), by `MAX_CLONE_DEPTH`
/// when it has none (`embed::native_stack_limit` is then 0).
#[inline]
fn too_deep(bs: Bs, depth: usize) -> bool {
    let limit = embed::native_stack_limit(bs);
    if limit == 0 {
        return depth >= MAX_CLONE_DEPTH;
    }
    let marker = 0u8;
    let stack_pointer = ::std::hint::black_box(&marker) as *const u8 as usize;
    stack_pointer < limit.saturating_add(CLONE_STACK_RED_ZONE)
}

const DEPTH_MESSAGE: &str = "The object is nested too deeply to be cloned.";

fn header_word() -> u64 {
    (HEADER_MAGIC << 32) | FORMAT_VERSION as u64
}

fn write_u8(buf: &mut Vec<u8>, v: u8) {
    buf.push(v);
}

fn write_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_f64(buf: &mut Vec<u8>, v: f64) {
    write_u64(buf, v.to_bits());
}

fn write_bytes(buf: &mut Vec<u8>, b: &[u8]) {
    buf.extend_from_slice(b);
}

/// A string as a length-prefixed run of UTF-16 code units (lone surrogates survive).
fn write_string_units(buf: &mut Vec<u8>, units: &[u16]) {
    write_u32(buf, units.len() as u32);
    for &u in units {
        buf.extend_from_slice(&u.to_le_bytes());
    }
}

// ---- The clone data ----

/// The bytes behind a `JSStructuredCloneData` pointer (stored in its opaque `bufList_` word).
struct CloneDataInner {
    bytes: Vec<u8>,
    has_transferables: bool,
}

unsafe fn inner_of(data: *mut JSStructuredCloneData) -> Option<&'static mut CloneDataInner> {
    if data.is_null() {
        return None;
    }
    let p = unsafe { (*data).bufList_.0[0] } as *mut CloneDataInner;
    if p.is_null() { None } else { Some(unsafe { &mut *p }) }
}

fn new_clone_data() -> JSStructuredCloneData {
    JSStructuredCloneData {
        bufList_: __BindgenOpaqueArray([0; 9]),
        scope_: JS::StructuredCloneScope::SameProcess,
        callbacks_: ptr::null(),
        closure_: ptr::null_mut(),
        ownTransferables_: OwnTransferablePolicy::NoTransferables,
        refsHeld_: js::SharedArrayRawBufferRefs { refs_: __BindgenOpaqueArray([0; 3]) },
        stringBufferRefsHeld_: __BindgenOpaqueArray([0; 7]),
    }
}

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
fn hobj(p: &*mut JSObject) -> JS::HandleObject {
    JS::Handle { _phantom_0: Default::default(), ptr: p }
}

#[inline]
fn hval(slot: *mut u64) -> JS::HandleValue {
    JS::Handle { _phantom_0: Default::default(), ptr: slot.cast() }
}

#[inline]
fn mobj(slot: *mut *mut JSObject) -> JS::MutableHandleObject {
    JS::MutableHandle { _phantom_0: Default::default(), ptr: slot }
}

/// The string's code units as a Rust `Vec<u16>` (copies; flattens ropes).
fn units_of(_bs: Bs, s: Handle<api::StringValue>) -> Option<Vec<u16>> {
    let f = s.flatten_to_handle().ok()?;
    Some(match unsafe { super::string::flat_chars(*f) } {
        api::StringChars::Latin1(b) => b.iter().map(|&c| c as u16).collect(),
        api::StringChars::TwoByte(u) => u.to_vec(),
    })
}

/// A pending Error for a clone failure, reported through `reportError` when the callbacks have one.
unsafe fn clone_error(cx: *mut JSContext, callbacks: *const JSStructuredCloneCallbacks, closure: *mut c_void, msg: &str) -> EvalError {
    let bs = bs_of(cx);
    if !callbacks.is_null() {
        if let Some(cb) = unsafe { (*callbacks).reportError } {
            if let Ok(c) = CString::new(msg) {
                unsafe { cb(cx, 0, closure, c.as_ptr()) };
            }
        }
    }
    if unsafe { cx_of(cx) }.exception.is_some() {
        // The reportError callback reported; take it back as the pending exception.
        let bits = unsafe { cx_of(cx) }.take_pending().unwrap_or(value_bits::undefined());
        return embed::throw_value(BsValue::from_raw_bits(bits).to_handle(bs));
    }
    let realm = embed::current_realm(bs);
    match api::new_error(bs, realm, msg) {
        Ok(e) => embed::throw_value(e.as_value()),
        Err(e) => e,
    }
}

/// A clone failure as an `EvalError` (no reporting; the caller reports once at the top).
fn read_error(bs: Bs, msg: &str) -> EvalError {
    let realm = embed::current_realm(bs);
    match api::new_error(bs, realm, msg) {
        Ok(e) => embed::throw_value(e.as_value()),
        Err(e) => e,
    }
}

fn data_clone_error(bs: Bs) -> EvalError {
    read_error(bs, "The object could not be cloned.")
}

// ---- Writer ----

struct Writer {
    cx: *mut JSContext,
    scope: JS::StructuredCloneScope,
    callbacks: *const JSStructuredCloneCallbacks,
    closure: *mut c_void,
    policy: *const JS::CloneDataPolicy,
    data: *mut JSStructuredCloneData,
    /// Objects already written, in write order; the index is the back-reference id.
    memory: Vec<Handle<BsValue>>,
    /// The objects in the transfer list.
    transferables: Vec<Handle<BsValue>>,
    /// The nesting level of the object being written; counts levels only for the no-quota fallback (`too_deep`).
    depth: usize,
}

impl Writer {
    fn buf(&mut self) -> &mut Vec<u8> {
        &mut unsafe { inner_of(self.data) }.expect("clone data").bytes
    }

    fn memory_index(&self, obj: HeapPtr<api::ObjectValue>) -> Option<u32> {
        self.memory.iter().position(|h| h.is_object() && (*h).as_object().ptr_eq(&obj)).map(|i| i as u32)
    }

    fn register(&mut self, v: Handle<BsValue>) -> u32 {
        self.memory.push(v);
        (self.memory.len() - 1) as u32
    }

    fn is_transferable(&self, obj: HeapPtr<api::ObjectValue>) -> bool {
        self.transferables.iter().any(|h| h.is_object() && (*h).as_object().ptr_eq(&obj))
    }
}

fn write_value(w: &mut Writer, v: Handle<BsValue>) -> Result<(), EvalError> {
    let bs = bs_of(w.cx);
    let d = *v;
    if d.is_undefined() {
        write_u32(w.buf(), TAG_UNDEFINED);
    } else if d.is_null() {
        write_u32(w.buf(), TAG_NULL);
    } else if d.is_bool() {
        write_u32(w.buf(), TAG_BOOLEAN);
        write_u8(w.buf(), d.is_true() as u8);
    } else if d.is_number() {
        write_u32(w.buf(), TAG_NUMBER);
        write_f64(w.buf(), d.as_number());
    } else if d.is_string() {
        write_u32(w.buf(), TAG_STRING);
        let units = units_of(bs, d.as_string().to_handle()).ok_or_else(|| data_clone_error(bs))?;
        write_string_units(w.buf(), &units);
    } else if d.is_bigint() {
        write_u32(w.buf(), TAG_BIGINT);
        write_bigint(w.buf(), bs, d.as_bigint().to_handle())?;
    } else if d.is_symbol() {
        return Err(data_clone_error(bs));
    } else {
        let obj = d.as_object();
        if let Some(i) = w.memory_index(obj) {
            write_u32(w.buf(), TAG_BACKREF);
            write_u32(w.buf(), i);
            return Ok(());
        }
        if too_deep(bs, w.depth) {
            return Err(unsafe { clone_error(w.cx, w.callbacks, w.closure, DEPTH_MESSAGE) });
        }
        w.register(v);
        let o = obj.to_handle().as_object();
        w.depth += 1;
        let r = write_object(w, v, o);
        w.depth -= 1;
        r?;
    }
    Ok(())
}

fn write_bigint(buf: &mut Vec<u8>, bs: Bs, bi: Handle<api::BigIntValue>) -> Result<(), EvalError> {
    let s = api::bigint_to_string(bs, bi, 10)?;
    let text = api::string_to_rust_string(s)?;
    write_u32(buf, text.len() as u32);
    write_bytes(buf, text.as_bytes());
    Ok(())
}

fn write_object(w: &mut Writer, v: Handle<BsValue>, o: Handle<api::ObjectValue>) -> Result<(), EvalError> {
    let bs = bs_of(w.cx);
    if o.as_opt::<api::ArrayObject>().is_some() {
        write_array(w, o)?;
    } else if let Some(b) = o.as_opt::<BooleanObject>() {
        write_u32(w.buf(), TAG_BOOLEAN_OBJ);
        write_u8(w.buf(), b.boolean_data() as u8);
    } else if let Some(n) = o.as_opt::<NumberObject>() {
        write_u32(w.buf(), TAG_NUMBER_OBJ);
        write_f64(w.buf(), n.number_data());
    } else if let Some(s) = api::string_object_data(o) {
        // StructuredSerializeInternal reads the [[StringData]] slot (ECMA-262 10.5), so this is the
        // object's kind and not its prototype: %String.prototype% itself, a `new String(s)` whose
        // prototype was replaced and a String object of another realm all serialise as Strings.
        write_u32(w.buf(), TAG_STRING_OBJ);
        let units = units_of(bs, s).ok_or_else(|| data_clone_error(bs))?;
        write_string_units(w.buf(), &units);
    } else if let Some(b) = o.as_opt::<BigIntObject>() {
        write_u32(w.buf(), TAG_BIGINT_OBJ);
        write_bigint(w.buf(), bs, b.bigint_data())?;
    } else if let Some(d) = o.as_opt::<DateObject>() {
        write_u32(w.buf(), TAG_DATE);
        write_f64(w.buf(), d.date_value());
    } else if let Some(r) = o.as_opt::<RegExpObject>() {
        write_u32(w.buf(), TAG_REGEXP);
        write_u8(w.buf(), js_flags_of(r.flags()));
        let units = units_of(bs, r.escaped_pattern_source()).ok_or_else(|| data_clone_error(bs))?;
        write_string_units(w.buf(), &units);
    } else if o.as_opt::<MapObject>().is_some() {
        write_map(w, o)?;
    } else if o.as_opt::<SetObject>().is_some() {
        write_set(w, o)?;
    } else if let Some(buf) = o.as_opt::<ArrayBufferObject>() {
        if w.is_transferable(*o.as_object()) {
            write_transfer(w, v, buf)?;
        } else {
            write_array_buffer(w, buf)?;
        }
    } else if api::is_typed_array(v) {
        write_typed_array(w, o)?;
    } else if let Some(dv) = api::as_data_view(o) {
        write_data_view(w, dv)?;
    } else if o.as_opt::<ErrorObject>().is_some() {
        write_error(w, o)?;
    } else if embed::is_host_object(*o.as_object()) {
        write_host(w, v, o)?;
    } else if is_serialisable_object(o) {
        // StructuredSerializeInternal steps 3 and 5-6: a record of the own enumerable string-keyed
        // properties, read with [[Get]] in the original's realm.
        write_u32(w.buf(), TAG_OBJECT);
        write_object_props(w, o)?;
    } else {
        return Err(data_clone_error(bs));
    }
    Ok(())
}

/// Brimstone's regexp flag bits to SpiderMonkey's `JS::RegExpFlag_*` bits.
fn js_flags_of(f: brimstone_core::parser::regexp::RegExpFlags) -> u8 {
    use brimstone_core::parser::regexp::RegExpFlags as F;
    let mut out = 0u8;
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
    if f.contains(F::HAS_INDICES) {
        out |= JS::RegExpFlag_HasIndices;
    }
    if f.contains(F::UNICODE_SETS) {
        out |= JS::RegExpFlag_UnicodeSets;
    }
    out
}

fn flags_string(f: u8) -> String {
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
    if f & JS::RegExpFlag_HasIndices != 0 {
        s.push('d');
    }
    s
}

fn key_of_string(bs: Bs, units: &[u16]) -> Result<Handle<PropertyKey>, EvalError> {
    let s = api::string_from_code_units(bs, units)?;
    Ok(PropertyKey::string_handle(bs, s.as_string())?)
}

/// The object's own enumerable string-keyed properties, in the order the algorithm wants them:
/// OrdinaryOwnPropertyKeys, array indices in ascending numeric order then the strings in own-property
/// order (ECMA-262 10.7.15). An array index is a String key (1.1 Property Keys), so it is written under
/// its decimal name; a symbol key is not copied. With `array`, the indices and "length" are left out:
/// `write_array` has already written the elements it indexes and stores the length itself.
fn enumerable_string_props(bs: Bs, o: Handle<api::ObjectValue>, array: bool) -> Result<Vec<(Vec<u16>, Handle<BsValue>)>, EvalError> {
    let keys = api::own_property_keys(bs, o)?;
    let mut out = Vec::new();
    for k in keys {
        let kd = *k;
        if kd.is_symbol() {
            continue;
        }
        // An index-like key comes back from `own_property_keys` as the string it was written with, so the
        // lookup key is built the way the engine builds one (brimstone src/js/runtime/property_key.rs:38):
        // "42" becomes array index 42, which is where the property actually lives.
        let key = if kd.is_string() {
            PropertyKey::string_handle(bs, kd.as_string().to_handle())?
        } else {
            PropertyKey::from_raw_value(kd).to_handle(bs)
        };
        if array {
            if key.is_array_index() {
                continue;
            }
            if key.is_string() {
                let units = units_of(bs, key.as_string()).unwrap_or_default();
                if units == b"length".iter().map(|&c| c as u16).collect::<Vec<u16>>() {
                    continue;
                }
            }
        }
        match api::get_own_property_descriptor(bs, o, key)? {
            Some(desc) if desc.is_enumerable() => {},
            _ => continue,
        }
        // StructuredSerializeInternal step 6: the value is read with [[Get]], so an own accessor runs.
        let value = api::get(bs, o, key)?;
        let units = if kd.is_string() {
            units_of(bs, kd.as_string().to_handle()).ok_or_else(|| data_clone_error(bs))?
        } else {
            key.as_array_index().to_string().chars().map(|c| c as u16).collect::<Vec<u16>>()
        };
        out.push((units, value));
    }
    Ok(out)
}

/// An object the algorithm serialises as a record of its own enumerable string-keyed properties: an
/// ordinary object, with no internal slot beyond [[Prototype]] and [[Extensible]], not callable and not
/// a proxy (HTML Standard 2.7.3 StructuredSerializeInternal, "if value has any internal slot other than
/// [[Prototype]] and [[Extensible]]" and "if value is an exotic object and its type is not Object").
/// The engine tells an object's kind by its GC shape tag, not by its prototype, so the test is the tag:
/// a class instance, an `Object.create(x)` object and an object made in another realm are all ordinary
/// here, while everything the algorithm does not name (a function, a proxy, a promise, a weak
/// collection, a generator, an arguments object, a module namespace, a raw JSON object) is tagged
/// otherwise and reaches the refusal below. %Object.prototype% carries its own tag (ECMA-262 20.1.2) but
/// is an ordinary object, and the WPT battery clones it ('ObjectPrototype must lose its exotic-ness when
/// cloned'). Two tag reads and at most three more in `is_callable` (brimstone
/// src/js/runtime/type_utilities.rs:691): O(1) per object, no allocation.
fn is_serialisable_object(o: Handle<api::ObjectValue>) -> bool {
    (o.is::<OrdinaryObject>() || o.is::<ObjectPrototypeObject>()) && !o.is_callable()
}

fn write_object_props(w: &mut Writer, o: Handle<api::ObjectValue>) -> Result<(), EvalError> {
    let bs = bs_of(w.cx);
    let props = enumerable_string_props(bs, o, false)?;
    write_u32(w.buf(), props.len() as u32);
    for (units, value) in props {
        write_string_units(w.buf(), &units);
        write_value(w, value)?;
    }
    Ok(())
}

fn write_array(w: &mut Writer, o: Handle<api::ObjectValue>) -> Result<(), EvalError> {
    let bs = bs_of(w.cx);
    write_u32(w.buf(), TAG_ARRAY);
    let len = api::length_of_array_like(bs, o)?;
    write_u64(w.buf(), len);
    for i in 0..len {
        let key = PropertyKey::array_index_handle(bs, i as u32)?;
        match api::get_own_property_descriptor(bs, o, key)? {
            Some(_) => {
                write_u8(w.buf(), 1);
                let v = api::get(bs, o, key)?;
                write_value(w, v)?;
            },
            None => write_u8(w.buf(), 0),
        }
    }
    let extras = enumerable_string_props(bs, o, true)?;
    write_u32(w.buf(), extras.len() as u32);
    for (units, value) in extras {
        write_string_units(w.buf(), &units);
        write_value(w, value)?;
    }
    Ok(())
}

fn write_map(w: &mut Writer, o: Handle<api::ObjectValue>) -> Result<(), EvalError> {
    let bs = bs_of(w.cx);
    write_u32(w.buf(), TAG_MAP);
    let m = o.as_opt::<MapObject>().expect("map");
    let entries: Vec<(Handle<BsValue>, Handle<BsValue>)> = m
        .map_data_inner()
        .iter_gc_safe()
        .map(|(k, v)| (k.get().to_handle(bs), v.to_handle(bs)))
        .collect();
    write_u32(w.buf(), entries.len() as u32);
    for (k, v) in entries {
        write_value(w, k)?;
        write_value(w, v)?;
    }
    Ok(())
}

fn write_set(w: &mut Writer, o: Handle<api::ObjectValue>) -> Result<(), EvalError> {
    let bs = bs_of(w.cx);
    write_u32(w.buf(), TAG_SET);
    let s = o.as_opt::<SetObject>().expect("set");
    let items: Vec<Handle<BsValue>> = s.set_data_inner().iter_gc_safe().map(|(k, _)| k.get().to_handle(bs)).collect();
    write_u32(w.buf(), items.len() as u32);
    for v in items {
        write_value(w, v)?;
    }
    Ok(())
}

fn write_array_buffer(w: &mut Writer, buf: Handle<ArrayBufferObject>) -> Result<(), EvalError> {
    let bs = bs_of(w.cx);
    write_u32(w.buf(), TAG_ARRAYBUFFER);
    if api::array_buffer_is_detached(buf) {
        return Err(read_error(bs, "Cannot clone a detached ArrayBuffer."));
    }
    let len = api::array_buffer_byte_length(buf);
    let data = api::array_buffer_data_ptr(buf).ok_or_else(|| read_error(bs, "Cannot clone a detached ArrayBuffer."))?;
    write_u64(w.buf(), len as u64);
    // SAFETY: the pointer covers `len` bytes and no GC runs while copying.
    write_bytes(w.buf(), unsafe { ::std::slice::from_raw_parts(data, len) });
    Ok(())
}

fn write_transfer(w: &mut Writer, v: Handle<BsValue>, buf: Handle<ArrayBufferObject>) -> Result<(), EvalError> {
    let bs = bs_of(w.cx);
    let cx = w.cx;
    // Refused before a byte is written or the host is asked: the bytes belong to the memory, and a transfer would detach it.
    if api::array_buffer_detach_key(buf) == api::DetachKey::WasmMemory {
        return type_error(bs, "cannot transfer the buffer of a WebAssembly.Memory");
    }
    let mut tag: u32 = 0;
    let mut ownership = JS::TransferableOwnership::SCTAG_TMO_UNOWNED;
    let mut content: *mut c_void = ptr::null_mut();
    let mut extra: u64 = 0;
    let mut ok = false;
    if !w.callbacks.is_null() {
        if let Some(cb) = unsafe { (*w.callbacks).writeTransfer } {
            let objp: *mut JSObject = (*v).as_object().as_ptr().cast();
            let r = unsafe {
                with_slots(cx_of(cx), 1, |slots| {
                    *slots = v.as_raw_bits();
                    cb(cx, hobj(&objp), w.closure, &mut tag, &mut ownership, &mut content, &mut extra)
                })
            };
            ok = r.unwrap_or(false);
        }
    }
    if !ok {
        // No transfer callback: clone the bytes like a plain buffer.
        return write_array_buffer(w, buf);
    }
    write_u32(w.buf(), TAG_TRANSFER);
    write_u32(w.buf(), tag);
    write_u32(w.buf(), ownership as u32);
    write_u64(w.buf(), extra);
    let len = api::array_buffer_byte_length(buf);
    let data = api::array_buffer_data_ptr(buf).unwrap_or(ptr::null_mut());
    write_u64(w.buf(), len as u64);
    if !data.is_null() {
        // SAFETY: the pointer covers `len` bytes and no GC runs while copying.
        write_bytes(w.buf(), unsafe { ::std::slice::from_raw_parts(data, len) });
    }
    if let Some(inner) = unsafe { inner_of(w.data) } {
        inner.has_transferables = true;
    }
    api::array_buffer_detach(buf);
    if !w.callbacks.is_null() {
        if let Some(cb) = unsafe { (*w.callbacks).freeTransfer } {
            unsafe { cb(tag, ownership, content, extra, w.closure) };
        }
    }
    Ok(())
}

fn write_typed_array(w: &mut Writer, o: Handle<api::ObjectValue>) -> Result<(), EvalError> {
    let bs = bs_of(w.cx);
    write_u32(w.buf(), TAG_TYPEDARRAY);
    let kind = api::typed_array_kind(o).ok_or_else(|| data_clone_error(bs))?;
    write_u8(w.buf(), kind as u8);
    let ta = o.as_typed_array();
    let buffer = ta.viewed_array_buffer_ptr().to_handle();
    write_value(w, buffer.as_value())?;
    write_u64(w.buf(), api::typed_array_byte_offset(o).unwrap_or(0) as u64);
    write_u64(w.buf(), api::typed_array_byte_length(o).unwrap_or(0) as u64);
    Ok(())
}

fn write_data_view(w: &mut Writer, dv: Handle<api::DataViewObject>) -> Result<(), EvalError> {
    write_u32(w.buf(), TAG_DATAVIEW);
    let buffer = dv.viewed_array_buffer().to_handle();
    write_value(w, buffer.as_value())?;
    write_u64(w.buf(), api::data_view_byte_offset(dv) as u64);
    write_u64(w.buf(), api::data_view_byte_length(dv) as u64);
    Ok(())
}

fn write_error(w: &mut Writer, o: Handle<api::ObjectValue>) -> Result<(), EvalError> {
    let bs = bs_of(w.cx);
    write_u32(w.buf(), TAG_ERROR);
    let name = get_string_prop(bs, o, "name")?;
    let message = get_string_prop(bs, o, "message")?;
    let stack = get_string_prop_opt(bs, o, "stack")?;
    let cause_key = key_of_string(bs, &b"cause".iter().map(|&c| c as u16).collect::<Vec<u16>>())?;
    let has_cause = api::has_own_property(bs, o, cause_key)?;
    write_string_units(w.buf(), &name);
    write_string_units(w.buf(), &message);
    match stack {
        Some(s) => {
            write_u8(w.buf(), 1);
            write_string_units(w.buf(), &s);
        },
        None => write_u8(w.buf(), 0),
    }
    if has_cause {
        write_u8(w.buf(), 1);
        let cause = api::get(bs, o, cause_key)?;
        write_value(w, cause)?;
    } else {
        write_u8(w.buf(), 0);
    }
    Ok(())
}

fn get_string_prop(bs: Bs, o: Handle<api::ObjectValue>, name: &str) -> Result<Vec<u16>, EvalError> {
    Ok(get_string_prop_opt(bs, o, name)?.unwrap_or_default())
}

fn get_string_prop_opt(bs: Bs, o: Handle<api::ObjectValue>, name: &str) -> Result<Option<Vec<u16>>, EvalError> {
    let key = key_of_string(bs, &name.bytes().map(|c| c as u16).collect::<Vec<u16>>())?;
    let v = api::get(bs, o, key)?;
    if v.is_undefined() {
        return Ok(None);
    }
    let s = api::to_string(bs, v)?;
    Ok(units_of(bs, s))
}

fn write_host(w: &mut Writer, v: Handle<BsValue>, o: Handle<api::ObjectValue>) -> Result<(), EvalError> {
    let cx = w.cx;
    let bs = bs_of(cx);
    if w.callbacks.is_null() {
        return Err(data_clone_error(bs));
    }
    let Some(cb) = (unsafe { (*w.callbacks).write }) else {
        return Err(data_clone_error(bs));
    };
    let wptr = w as *mut Writer as *mut JSStructuredCloneWriter;
    let objp: *mut JSObject = (*o.as_object()).as_ptr().cast();
    let mut same_process = false;
    let r = unsafe {
        with_slots(cx_of(cx), 1, |slots| {
            *slots = v.as_raw_bits();
            cb(cx, wptr, hobj(&objp), &mut same_process, w.closure)
        })
    };
    match r {
        Some(true) => Ok(()),
        _ => Err(unsafe { clone_error(cx, w.callbacks, w.closure, "The host object could not be cloned.") }),
    }
}

// ---- Reader ----

/// Every read goes through here: bounds-checked, and any failure is an error, never a panic.
struct Reader<'a> {
    cx: *mut JSContext,
    buf: &'a [u8],
    pos: usize,
    callbacks: *const JSStructuredCloneCallbacks,
    closure: *mut c_void,
    policy: *const JS::CloneDataPolicy,
    /// Objects already read, in read order; the index is the back-reference id.
    objects: Vec<Handle<BsValue>>,
    /// The nesting level of the object being read; counts levels only for the no-quota fallback (`too_deep`).
    depth: usize,
}

impl<'a> Reader<'a> {
    /// Reads one nested object record with `f`, one level deeper; refuses past the stack guard (`too_deep`).
    fn descend(&mut self, f: impl FnOnce(&mut Self) -> Result<Handle<BsValue>, EvalError>) -> Result<Handle<BsValue>, EvalError> {
        if too_deep(bs_of(self.cx), self.depth) {
            return Err(unsafe { clone_error(self.cx, self.callbacks, self.closure, DEPTH_MESSAGE) });
        }
        self.depth += 1;
        let out = f(self);
        self.depth -= 1;
        out
    }

    fn err(&self) -> EvalError {
        read_error(bs_of(self.cx), "The structured clone data is truncated or corrupt.")
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], EvalError> {
        if self.buf.len() - self.pos < n {
            return Err(self.err());
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, EvalError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, EvalError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, EvalError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn f64(&mut self) -> Result<f64, EvalError> {
        Ok(f64::from_bits(self.u64()?))
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], EvalError> {
        self.take(n)
    }

    fn string_units(&mut self) -> Result<Vec<u16>, EvalError> {
        let len = self.u32()? as usize;
        let raw = self.take(len.checked_mul(2).ok_or_else(|| self.err())?)?;
        Ok(raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect())
    }

    fn register(&mut self, v: Handle<BsValue>) -> u32 {
        self.objects.push(v);
        (self.objects.len() - 1) as u32
    }

    fn placeholder(&mut self, bs: Bs) -> u32 {
        let v = BsValue::undefined().to_handle(bs);
        self.register(v)
    }

    fn set_at(&mut self, i: u32, v: Handle<BsValue>) {
        if let Some(slot) = self.objects.get_mut(i as usize) {
            *slot = v;
        }
    }
}

fn read_value(r: &mut Reader) -> Result<Handle<BsValue>, EvalError> {
    let bs = bs_of(r.cx);
    let tag = r.u32()?;
    match tag {
        TAG_UNDEFINED => Ok(BsValue::undefined().to_handle(bs)),
        TAG_NULL => Ok(BsValue::null().to_handle(bs)),
        TAG_BOOLEAN => Ok(BsValue::bool(r.u8()? != 0).to_handle(bs)),
        TAG_NUMBER => Ok(BsValue::number(r.f64()?).to_handle(bs)),
        TAG_STRING => {
            let units = r.string_units()?;
            let s = api::string_from_code_units(bs, &units)?;
            Ok(s.as_value())
        },
        TAG_BIGINT => {
            let len = r.u32()? as usize;
            let raw = r.bytes(len)?;
            let text = ::std::str::from_utf8(raw).map_err(|_| r.err())?;
            let bi: api::BigInt = text.parse().map_err(|_| r.err())?;
            let v = api::BigIntValue::new(bs, bi)?;
            Ok(BsValue::bigint(HeapPtr::from_ptr((*v).as_ptr())).to_handle(bs))
        },
        TAG_BACKREF => {
            let i = r.u32()?;
            r.objects.get(i as usize).copied().ok_or_else(|| r.err())
        },
        TAG_BOOLEAN_OBJ => {
            let b = r.u8()? != 0;
            let realm = embed::current_realm(bs);
            let ctor = api::intrinsic(realm, api::Intrinsic::BooleanConstructor);
            let arg = BsValue::bool(b).to_handle(bs);
            let o = api::construct(bs, ctor.as_value(), &[arg], Some(ctor))?;
            let id = r.placeholder(bs);
            let v = o.as_value();
            r.set_at(id, v);
            Ok(v)
        },
        TAG_NUMBER_OBJ => {
            let n = r.f64()?;
            let realm = embed::current_realm(bs);
            let ctor = api::intrinsic(realm, api::Intrinsic::NumberConstructor);
            let arg = BsValue::number(n).to_handle(bs);
            let o = api::construct(bs, ctor.as_value(), &[arg], Some(ctor))?;
            let id = r.placeholder(bs);
            let v = o.as_value();
            r.set_at(id, v);
            Ok(v)
        },
        TAG_STRING_OBJ => {
            let units = r.string_units()?;
            let s = api::string_from_code_units(bs, &units)?;
            let realm = embed::current_realm(bs);
            let ctor = api::intrinsic(realm, api::Intrinsic::StringConstructor);
            let o = api::construct(bs, ctor.as_value(), &[s.as_value()], Some(ctor))?;
            let id = r.placeholder(bs);
            let v = o.as_value();
            r.set_at(id, v);
            Ok(v)
        },
        TAG_BIGINT_OBJ => {
            let len = r.u32()? as usize;
            let raw = r.bytes(len)?;
            let text = ::std::str::from_utf8(raw).map_err(|_| r.err())?;
            let bi: api::BigInt = text.parse().map_err(|_| r.err())?;
            let bv = api::BigIntValue::new(bs, bi)?;
            let o = BigIntObject::new_from_value(bs, embed::current_realm(bs), bv)?;
            let id = r.placeholder(bs);
            let v = o.as_value();
            r.set_at(id, v);
            Ok(v)
        },
        TAG_DATE => {
            let ms = r.f64()?;
            let realm = embed::current_realm(bs);
            let d = api::new_date(bs, realm, ms)?;
            let id = r.placeholder(bs);
            let v = d.as_value();
            r.set_at(id, v);
            Ok(v)
        },
        TAG_REGEXP => {
            let flags = r.u8()?;
            let units = r.string_units()?;
            let pattern = api::string_from_code_units(bs, &units)?;
            let fs = api::string_from_utf8(bs, &flags_string(flags))?;
            let realm = embed::current_realm(bs);
            let ctor = api::intrinsic(realm, api::Intrinsic::RegExpConstructor);
            let o = api::construct(bs, ctor.as_value(), &[pattern.as_value(), fs.as_value()], Some(ctor))?;
            let id = r.placeholder(bs);
            let v = o.as_value();
            r.set_at(id, v);
            Ok(v)
        },
        TAG_ARRAY => r.descend(read_array),
        TAG_OBJECT => r.descend(read_object),
        TAG_MAP => r.descend(read_map),
        TAG_SET => r.descend(read_set),
        TAG_ARRAYBUFFER => read_array_buffer(r),
        TAG_TRANSFER => read_transfer(r),
        TAG_TYPEDARRAY => r.descend(read_typed_array),
        TAG_DATAVIEW => r.descend(read_data_view),
        TAG_ERROR => r.descend(read_error_object),
        t if t >= SCTAG_USER_MIN => r.descend(|r| read_host(r, t)),
        _ => Err(r.err()),
    }
}

fn read_array(r: &mut Reader) -> Result<Handle<BsValue>, EvalError> {
    let bs = bs_of(r.cx);
    let len = r.u64()?;
    let realm = embed::current_realm(bs);
    let arr = api::array_with_length(bs, realm, len)?;
    let id = r.placeholder(bs);
    let av = arr.as_value();
    r.set_at(id, av);
    for i in 0..len {
        let present = r.u8()?;
        if present != 0 {
            let v = read_value(r)?;
            let key = PropertyKey::array_index_handle(bs, i as u32)?;
            api::create_data_property(bs, arr.as_object(), key, v)?;
        }
    }
    let count = r.u32()?;
    for _ in 0..count {
        let units = r.string_units()?;
        let v = read_value(r)?;
        let key = key_of_string(bs, &units)?;
        api::create_data_property(bs, arr.as_object(), key, v)?;
    }
    Ok(av)
}

fn read_object(r: &mut Reader) -> Result<Handle<BsValue>, EvalError> {
    let bs = bs_of(r.cx);
    let o = api::new_object(bs)?;
    let id = r.placeholder(bs);
    let ov = o.as_value();
    r.set_at(id, ov);
    let count = r.u32()?;
    for _ in 0..count {
        let units = r.string_units()?;
        let v = read_value(r)?;
        let key = key_of_string(bs, &units)?;
        api::create_data_property(bs, o, key, v)?;
    }
    Ok(ov)
}

fn read_map(r: &mut Reader) -> Result<Handle<BsValue>, EvalError> {
    let bs = bs_of(r.cx);
    let realm = embed::current_realm(bs);
    let ctor = api::intrinsic(realm, api::Intrinsic::MapConstructor);
    let m = MapObject::new_from_constructor(bs, ctor)?;
    let id = r.placeholder(bs);
    let mv = m.as_value();
    r.set_at(id, mv);
    let count = r.u32()?;
    for _ in 0..count {
        let k = read_value(r)?;
        let v = read_value(r)?;
        m.insert(bs, k, v)?;
    }
    Ok(mv)
}

fn read_set(r: &mut Reader) -> Result<Handle<BsValue>, EvalError> {
    let bs = bs_of(r.cx);
    let realm = embed::current_realm(bs);
    let ctor = api::intrinsic(realm, api::Intrinsic::SetConstructor);
    let s = SetObject::new_from_constructor(bs, ctor)?;
    let id = r.placeholder(bs);
    let sv = s.as_value();
    r.set_at(id, sv);
    let count = r.u32()?;
    for _ in 0..count {
        let v = read_value(r)?;
        s.insert(bs, v)?;
    }
    Ok(sv)
}

fn read_array_buffer(r: &mut Reader) -> Result<Handle<BsValue>, EvalError> {
    let bs = bs_of(r.cx);
    let len = r.u64()? as usize;
    let raw = r.bytes(len)?;
    let realm = embed::current_realm(bs);
    let buf = api::array_buffer_new(bs, realm, len)?;
    let id = r.placeholder(bs);
    let bv = buf.as_value();
    r.set_at(id, bv);
    if let Some(data) = api::array_buffer_data_ptr(buf) {
        // SAFETY: the pointer covers `len` bytes and no GC runs while copying.
        unsafe { ptr::copy_nonoverlapping(raw.as_ptr(), data, len) };
    }
    Ok(bv)
}

fn read_transfer(r: &mut Reader) -> Result<Handle<BsValue>, EvalError> {
    let bs = bs_of(r.cx);
    let cx = r.cx;
    let tag = r.u32()?;
    let ownership = r.u32()?;
    let extra = r.u64()?;
    let len = r.u64()? as usize;
    let raw = r.bytes(len)?;
    let _ = raw;
    if r.callbacks.is_null() {
        return Err(read_error(bs, "No readTransfer callback for a transferred object."));
    }
    let Some(cb) = (unsafe { (*r.callbacks).readTransfer }) else {
        return Err(read_error(bs, "No readTransfer callback for a transferred object."));
    };
    let rptr = r as *mut Reader as *mut JSStructuredCloneReader;
    let content: *mut c_void = ptr::null_mut();
    let ownership = match ownership {
        0 => JS::TransferableOwnership::SCTAG_TMO_UNFILLED,
        1 => JS::TransferableOwnership::SCTAG_TMO_UNOWNED,
        2 => JS::TransferableOwnership::SCTAG_TMO_FIRST_OWNED,
        3 => JS::TransferableOwnership::SCTAG_TMO_MAPPED_DATA,
        4 => JS::TransferableOwnership::SCTAG_TMO_CUSTOM,
        _ => JS::TransferableOwnership::SCTAG_TMO_USER_MIN,
    };
    let _ = ownership;
    let r2 = unsafe {
        with_slots(cx_of(cx), 1, |slots| {
            let mut out: *mut JSObject = ptr::null_mut();
            let ok = cb(cx, rptr, r.policy, tag, content, extra, r.closure, mobj(&mut out));
            (ok, out, slots)
        })
    };
    match r2 {
        Some((true, out, _)) if !out.is_null() => {
            let v = BsValue::object(object_ptr(out)).to_handle(bs);
            let id = r.placeholder(bs);
            r.set_at(id, v);
            Ok(v)
        },
        _ => Err(unsafe { clone_error(cx, r.callbacks, r.closure, "The transferred object could not be read.") }),
    }
}

fn read_typed_array(r: &mut Reader) -> Result<Handle<BsValue>, EvalError> {
    let bs = bs_of(r.cx);
    let kind_u8 = r.u8()?;
    let kind = match kind_u8 {
        0 => api::TypedArrayKind::Int8,
        1 => api::TypedArrayKind::UInt8,
        2 => api::TypedArrayKind::UInt8Clamped,
        3 => api::TypedArrayKind::Int16,
        4 => api::TypedArrayKind::UInt16,
        5 => api::TypedArrayKind::Int32,
        6 => api::TypedArrayKind::UInt32,
        7 => api::TypedArrayKind::BigInt64,
        8 => api::TypedArrayKind::BigUInt64,
        9 => api::TypedArrayKind::Float16,
        10 => api::TypedArrayKind::Float32,
        _ => api::TypedArrayKind::Float64,
    };
    let id = r.placeholder(bs);
    let buffer_v = read_value(r)?;
    let byte_offset = r.u64()? as usize;
    let byte_length = r.u64()? as usize;
    let buffer = buffer_v.as_object().as_opt::<ArrayBufferObject>().ok_or_else(|| r.err())?;
    let realm = embed::current_realm(bs);
    let elem_size = match kind {
        api::TypedArrayKind::Int8 | api::TypedArrayKind::UInt8 | api::TypedArrayKind::UInt8Clamped => 1,
        api::TypedArrayKind::Int16 | api::TypedArrayKind::UInt16 | api::TypedArrayKind::Float16 => 2,
        api::TypedArrayKind::Int32 | api::TypedArrayKind::UInt32 | api::TypedArrayKind::Float32 => 4,
        _ => 8,
    };
    let length = if elem_size > 0 { byte_length / elem_size } else { 0 };
    let ta = api::create_typed_array(bs, realm, kind, buffer, byte_offset, Some(length))?;
    let v = ta.as_value();
    r.set_at(id, v);
    Ok(v)
}

fn read_data_view(r: &mut Reader) -> Result<Handle<BsValue>, EvalError> {
    let bs = bs_of(r.cx);
    let id = r.placeholder(bs);
    let buffer_v = read_value(r)?;
    let byte_offset = r.u64()? as usize;
    let byte_length = r.u64()? as usize;
    let buffer = buffer_v.as_object().as_opt::<ArrayBufferObject>().ok_or_else(|| r.err())?;
    let realm = embed::current_realm(bs);
    let dv = api::create_data_view(bs, realm, buffer, byte_offset, Some(byte_length))?;
    let v = dv.as_value();
    r.set_at(id, v);
    Ok(v)
}

fn read_error_object(r: &mut Reader) -> Result<Handle<BsValue>, EvalError> {
    let bs = bs_of(r.cx);
    let name = r.string_units()?;
    let message = r.string_units()?;
    let has_stack = r.u8()?;
    let stack = if has_stack != 0 { Some(r.string_units()?) } else { None };
    let has_cause = r.u8()?;
    let id = r.placeholder(bs);
    let name_text = String::from_utf16_lossy(&name);
    let message_text = String::from_utf16_lossy(&message);
    let intrinsic = match name_text.as_str() {
        "EvalError" => api::Intrinsic::EvalErrorConstructor,
        "RangeError" => api::Intrinsic::RangeErrorConstructor,
        "ReferenceError" => api::Intrinsic::ReferenceErrorConstructor,
        "SyntaxError" => api::Intrinsic::SyntaxErrorConstructor,
        "TypeError" => api::Intrinsic::TypeErrorConstructor,
        "URIError" => api::Intrinsic::URIErrorConstructor,
        _ => api::Intrinsic::ErrorConstructor,
    };
    let realm = embed::current_realm(bs);
    let ctor = api::intrinsic(realm, intrinsic);
    let msg = api::string_from_utf8(bs, &message_text)?;
    let e = api::construct(bs, ctor.as_value(), &[msg.as_value()], Some(ctor))?;
    if let Some(stack) = stack {
        let s = api::string_from_code_units(bs, &stack)?;
        let key = key_of_string(bs, &b"stack".iter().map(|&c| c as u16).collect::<Vec<u16>>())?;
        api::create_data_property(bs, e, key, s.as_value())?;
    }
    if has_cause != 0 {
        let cause = read_value(r)?;
        let key = key_of_string(bs, &b"cause".iter().map(|&c| c as u16).collect::<Vec<u16>>())?;
        api::create_data_property(bs, e, key, cause)?;
    }
    let v = e.as_value();
    r.set_at(id, v);
    Ok(v)
}

fn read_host(r: &mut Reader, tag: u32) -> Result<Handle<BsValue>, EvalError> {
    let bs = bs_of(r.cx);
    let cx = r.cx;
    let data = r.u32()?;
    if r.callbacks.is_null() {
        return Err(read_error(bs, "No read callback for a host object."));
    }
    let Some(cb) = (unsafe { (*r.callbacks).read }) else {
        return Err(read_error(bs, "No read callback for a host object."));
    };
    let rptr = r as *mut Reader as *mut JSStructuredCloneReader;
    let out = unsafe { cb(cx, rptr, r.policy, tag, data, r.closure) };
    if out.is_null() {
        return Err(unsafe { clone_error(cx, r.callbacks, r.closure, "The host object could not be read.") });
    }
    let v = BsValue::object(object_ptr(out)).to_handle(bs);
    let id = r.placeholder(bs);
    r.set_at(id, v);
    Ok(v)
}

// ---- The API ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__NewJSAutoStructuredCloneBuffer(
    scope: JS::StructuredCloneScope,
    callbacks: *const JSStructuredCloneCallbacks,
) -> *mut JSAutoStructuredCloneBuffer {
    let inner = Box::new(CloneDataInner { bytes: Vec::new(), has_transferables: false });
    let mut data = new_clone_data();
    data.bufList_.0[0] = Box::into_raw(inner) as u64;
    data.scope_ = scope;
    data.callbacks_ = callbacks;
    let buf = Box::new(JSAutoStructuredCloneBuffer { data_: data, version_: crate::jsapi::JS_STRUCTURED_CLONE_VERSION });
    Box::into_raw(buf)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__DeleteJSAutoStructuredCloneBuffer(buf: *mut JSAutoStructuredCloneBuffer) {
    if buf.is_null() {
        return;
    }
    let b = unsafe { Box::from_raw(buf) };
    let p = b.data_.bufList_.0[0] as *mut CloneDataInner;
    if !p.is_null() {
        drop(unsafe { Box::from_raw(p) });
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__GetLengthOfJSStructuredCloneData(data: *mut JSStructuredCloneData) -> usize {
    unsafe { inner_of(data) }.map(|i| i.bytes.len()).unwrap_or(0)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__CopyJSStructuredCloneData(src: *mut JSStructuredCloneData, dest: *mut u8) {
    if let Some(inner) = unsafe { inner_of(src) } {
        if !dest.is_null() {
            // SAFETY: Servo provides a buffer of GetLengthOfJSStructuredCloneData bytes.
            unsafe { ptr::copy_nonoverlapping(inner.bytes.as_ptr(), dest, inner.bytes.len()) };
        }
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__WriteBytesToJSStructuredCloneData(
    src: *const u8,
    len: usize,
    dest: *mut JSStructuredCloneData,
) -> bool {
    let Some(inner) = (unsafe { inner_of(dest) }) else {
        return false;
    };
    if !src.is_null() {
        // SAFETY: Servo provides `len` readable bytes.
        inner.bytes.extend_from_slice(unsafe { ::std::slice::from_raw_parts(src, len) });
    }
    true
}

/// The objects of a transfer list (an array) as handles, filtered through `canTransfer` when given.
unsafe fn transfer_list(
    cx: *mut JSContext,
    transferable: JS::HandleValue,
    callbacks: *const JSStructuredCloneCallbacks,
    closure: *mut c_void,
) -> Vec<Handle<BsValue>> {
    let bs = bs_of(cx);
    let v = unsafe { val(cx, transferable) };
    let d = *v;
    let mut out = Vec::new();
    if d.is_object() {
        let o = d.as_object().to_handle().as_object();
        if let Ok(len) = api::length_of_array_like(bs, o) {
            for i in 0..len {
                if let Ok(key) = PropertyKey::array_index_handle(bs, i as u32) {
                    if let Ok(item) = api::get(bs, o, key) {
                        if !item.is_object() {
                            continue;
                        }
                        if !callbacks.is_null() {
                            if let Some(cb) = unsafe { (*callbacks).canTransfer } {
                                let objp: *mut JSObject = (*item).as_object().as_ptr().cast();
                                let mut same_process = false;
                                let ok = unsafe {
                                    with_slots(cx_of(cx), 1, |slots| {
                                        *slots = item.as_raw_bits();
                                        cb(cx, hobj(&objp), &mut same_process, closure)
                                    })
                                };
                                if !ok.unwrap_or(false) {
                                    continue;
                                }
                            }
                        }
                        out.push(item);
                    }
                }
            }
        }
    }
    out
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_WriteStructuredClone(
    cx: *mut JSContext,
    v: JS::HandleValue,
    data: *mut JSStructuredCloneData,
    scope: JS::StructuredCloneScope,
    cloneDataPolicy: *const JS::CloneDataPolicy,
    optionalCallbacks: *const JSStructuredCloneCallbacks,
    closure: *mut c_void,
    transferable: JS::HandleValue,
) -> bool {
    let bs = bs_of(cx);
    let scope_h = HandleScope::enter(bs);
    if !data.is_null() {
        unsafe {
            (*data).scope_ = scope;
            (*data).callbacks_ = optionalCallbacks;
            (*data).closure_ = closure;
        }
    }
    let inner = unsafe { inner_of(data) };
    let Some(inner) = inner else {
        scope_h.exit();
        return unsafe { fail(cx, read_error(bs, "Invalid structured clone data.")) };
    };
    inner.bytes.clear();
    inner.has_transferables = false;
    write_u64(&mut inner.bytes, header_word());
    let mut w = Writer {
        cx,
        scope,
        callbacks: optionalCallbacks,
        closure,
        policy: cloneDataPolicy,
        data,
        memory: Vec::new(),
        transferables: unsafe { transfer_list(cx, transferable, optionalCallbacks, closure) },
        depth: 0,
    };
    let vh = unsafe { val(cx, v) };
    let r = write_value(&mut w, vh);
    scope_h.exit();
    match r {
        Ok(()) => true,
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ReadStructuredClone(
    cx: *mut JSContext,
    data: *const JSStructuredCloneData,
    version: u32,
    scope: JS::StructuredCloneScope,
    vp: JS::MutableHandleValue,
    cloneDataPolicy: *const JS::CloneDataPolicy,
    optionalCallbacks: *const JSStructuredCloneCallbacks,
    closure: *mut c_void,
) -> bool {
    let bs = bs_of(cx);
    let scope_h = HandleScope::enter(bs);
    let bytes: &[u8] = match unsafe { inner_of(data as *mut JSStructuredCloneData) } {
        Some(inner) => &inner.bytes,
        None => {
            scope_h.exit();
            return unsafe { fail(cx, read_error(bs, "Invalid structured clone data.")) };
        },
    };
    let mut r = Reader {
        cx,
        buf: bytes,
        pos: 0,
        callbacks: optionalCallbacks,
        closure,
        policy: cloneDataPolicy,
        objects: Vec::new(),
        depth: 0,
    };
    let _ = scope;
    let result = (|| {
        let header = r.u64()?;
        // `version` is SpiderMonkey's API version the caller says the data was written with (Servo passes
        // JS_STRUCTURED_CLONE_VERSION): like SpiderMonkey, refuse only data newer than this engine. Our own byte format
        // version lives in the header word.
        if header >> 32 != HEADER_MAGIC
            || (header & 0xFFFF_FFFF) as u32 != FORMAT_VERSION
            || version > crate::jsapi::JS_STRUCTURED_CLONE_VERSION
        {
            return Err(read_error(bs, "Unsupported structured clone data version."));
        }
        read_value(&mut r)
    })();
    match result {
        Ok(v) => {
            if !vp.ptr.is_null() {
                unsafe { (*vp.ptr).asBits_ = v.as_raw_bits() };
            }
            scope_h.exit();
            true
        },
        Err(e) => {
            scope_h.exit();
            unsafe { fail(cx, e) }
        },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_StructuredClone(
    cx: *mut JSContext,
    v: JS::HandleValue,
    vp: JS::MutableHandleValue,
    optionalCallbacks: *const JSStructuredCloneCallbacks,
    closure: *mut c_void,
) -> bool {
    let scope = JS::StructuredCloneScope::SameProcess;
    let buf = unsafe { glue__NewJSAutoStructuredCloneBuffer(scope, optionalCallbacks) };
    if buf.is_null() {
        return false;
    }
    let data = unsafe { &mut (*buf).data_ };
    let version = unsafe { (*buf).version_ };
    let ok = unsafe {
        with_slots(cx_of(cx), 1, |slots| {
            *slots = value_bits::undefined();
            let undef = hval(slots);
            let w = JS_WriteStructuredClone(cx, v, data, scope, ptr::null(), optionalCallbacks, closure, undef);
            if !w {
                return false;
            }
            JS_ReadStructuredClone(cx, data, version, scope, vp, ptr::null(), optionalCallbacks, closure)
        })
    };
    unsafe { glue__DeleteJSAutoStructuredCloneBuffer(buf) };
    ok.unwrap_or(false)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_StructuredCloneHasTransferables(
    data: *mut JSStructuredCloneData,
    hasTransferable: *mut bool,
) -> bool {
    if let Some(inner) = unsafe { inner_of(data) } {
        if !hasTransferable.is_null() {
            unsafe { *hasTransferable = inner.has_transferables };
        }
        true
    } else {
        false
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetStructuredCloneScope(w: *mut JSStructuredCloneWriter) -> JS::StructuredCloneScope {
    if w.is_null() {
        return JS::StructuredCloneScope::SameProcess;
    }
    unsafe { (*(w as *mut Writer)).scope }
}

// ---- Reader/writer primitives used by Servo's callbacks ----

unsafe fn reader_of(r: *mut JSStructuredCloneReader) -> Option<&'static mut Reader<'static>> {
    if r.is_null() { None } else { Some(unsafe { &mut *(r as *mut Reader<'static>) }) }
}

unsafe fn writer_of(w: *mut JSStructuredCloneWriter) -> Option<&'static mut Writer> {
    if w.is_null() { None } else { Some(unsafe { &mut *(w as *mut Writer) }) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ReadUint32Pair(
    r: *mut JSStructuredCloneReader,
    p1: *mut u32,
    p2: *mut u32,
) -> bool {
    let Some(rr) = (unsafe { reader_of(r) }) else { return false };
    match (rr.u32(), rr.u32()) {
        (Ok(a), Ok(b)) => {
            if !p1.is_null() {
                unsafe { *p1 = a };
            }
            if !p2.is_null() {
                unsafe { *p2 = b };
            }
            true
        },
        _ => false,
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_WriteUint32PairUnchecked(
    w: *mut JSStructuredCloneWriter,
    tag: u32,
    data: u32,
) -> bool {
    let Some(ww) = (unsafe { writer_of(w) }) else { return false };
    write_u32(ww.buf(), tag);
    write_u32(ww.buf(), data);
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ReadBytes(
    r: *mut JSStructuredCloneReader,
    p: *mut c_void,
    len: usize,
) -> bool {
    let Some(rr) = (unsafe { reader_of(r) }) else { return false };
    match rr.bytes(len) {
        Ok(b) => {
            if !p.is_null() {
                // SAFETY: the caller provides `len` writable bytes.
                unsafe { ptr::copy_nonoverlapping(b.as_ptr(), p as *mut u8, len) };
            }
            true
        },
        Err(_) => false,
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_WriteBytes(
    w: *mut JSStructuredCloneWriter,
    p: *const c_void,
    len: usize,
) -> bool {
    let Some(ww) = (unsafe { writer_of(w) }) else { return false };
    if !p.is_null() {
        // SAFETY: the caller provides `len` readable bytes.
        write_bytes(ww.buf(), unsafe { ::std::slice::from_raw_parts(p as *const u8, len) });
    }
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ReadDouble(r: *mut JSStructuredCloneReader, v: *mut f64) -> bool {
    let Some(rr) = (unsafe { reader_of(r) }) else { return false };
    match rr.f64() {
        Ok(d) => {
            if !v.is_null() {
                unsafe { *v = d };
            }
            true
        },
        Err(_) => false,
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_WriteDouble(w: *mut JSStructuredCloneWriter, v: f64) -> bool {
    let Some(ww) = (unsafe { writer_of(w) }) else { return false };
    write_f64(ww.buf(), v);
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ReadString(
    r: *mut JSStructuredCloneReader,
    str_: JS::MutableHandleString,
) -> bool {
    let Some(rr) = (unsafe { reader_of(r) }) else { return false };
    let bs = bs_of(rr.cx);
    match rr.string_units() {
        Ok(units) => match api::string_from_code_units(bs, &units) {
            Ok(s) => {
                if !str_.ptr.is_null() {
                    unsafe { *str_.ptr = (*s.as_string()).as_ptr().cast() };
                }
                true
            },
            Err(_) => false,
        },
        Err(_) => false,
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_WriteString(
    w: *mut JSStructuredCloneWriter,
    str_: JS::HandleString,
) -> bool {
    let Some(ww) = (unsafe { writer_of(w) }) else { return false };
    let bs = bs_of(ww.cx);
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    if str_.ptr.is_null() || unsafe { (*str_.ptr).is_null() } {
        return false;
    }
    let s = BsValue::string(HeapPtr::from_ptr(unsafe { *str_.ptr }.cast())).to_handle(bs).as_string();
    let Some(units) = units_of(bs, s) else { return false };
    write_string_units(ww.buf(), &units);
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ReadTypedArray(
    r: *mut JSStructuredCloneReader,
    vp: JS::MutableHandleValue,
) -> bool {
    let Some(rr) = (unsafe { reader_of(r) }) else { return false };
    // The host's read callback re-enters the walk here: `read_typed_array` is called with no `descend`, and the
    // buffer it then reads is a `TAG_ARRAYBUFFER`, whose arm does not descend either. This entry is the only place
    // the stack and the depth get checked on that path.
    if too_deep(bs_of(rr.cx), rr.depth) {
        let _ = unsafe { clone_error(rr.cx, rr.callbacks, rr.closure, DEPTH_MESSAGE) };
        return false;
    }
    match read_typed_array(rr) {
        Ok(v) => {
            if !vp.ptr.is_null() {
                unsafe { (*vp.ptr).asBits_ = v.as_raw_bits() };
            }
            true
        },
        Err(_) => false,
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_WriteTypedArray(
    w: *mut JSStructuredCloneWriter,
    v: JS::HandleValue,
) -> bool {
    let Some(ww) = (unsafe { writer_of(w) }) else { return false };
    let bs = bs_of(ww.cx);
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    let hv = handle_of(bs, unsafe { (*v.ptr).asBits_ });
    if !api::is_typed_array(hv) {
        return false;
    }
    // As in `JS_ReadTypedArray`: the host's write callback enters here, past `write_value`, so the view's own
    // frame is checked on this entry and only on this entry.
    if too_deep(bs, ww.depth) {
        let _ = unsafe { clone_error(ww.cx, ww.callbacks, ww.closure, DEPTH_MESSAGE) };
        return false;
    }
    match write_typed_array(ww, hv.as_object()) {
        Ok(()) => true,
        Err(_) => false,
    }
}
