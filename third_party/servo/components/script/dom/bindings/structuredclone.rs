/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! This module implements structured cloning, as defined by [HTML](https://html.spec.whatwg.org/multipage/#safe-passing-of-structured-data).

#![cfg_attr(crown, allow(crown::jscontext_first_arg))]

use std::ffi::CStr;
use std::os::raw;
use std::ptr::{self, NonNull};

use js::context::{JSContext, NoGC};
use js::conversions::{ToJSValConvertible, jsstr_to_string};
use js::glue::{
    CopyJSStructuredCloneData, GetLengthOfJSStructuredCloneData, WriteBytesToJSStructuredCloneData,
};
use js::jsapi::{
    CloneDataPolicy, ESClass, ExceptionStackBehavior, HandleObject as RawHandleObject, Heap,
    JS_ReadUint32Pair, JS_STRUCTURED_CLONE_VERSION, JS_WriteUint32PairUnchecked,
    JSContext as RawJSContext, JSObject, JSStructuredCloneCallbacks, JSStructuredCloneReader,
    JSStructuredCloneWriter, MutableHandleObject as RawMutableHandleObject, StructuredCloneScope,
    TransferableOwnership,
};
use js::jsval::{ObjectValue, UndefinedValue};
use js::realm::CurrentRealm;
use js::rust::wrappers2::{
    CurrentGlobalOrNull, GetBuiltinClass, IsArray, JS_ClearPendingException,
    JS_GetPendingException, JS_GetProperty, JS_ReadStructuredClone, JS_ValueToSource,
    JS_WriteStructuredClone,
};
use js::rust::{
    CustomAutoRooterGuard, HandleValue, IdVector, JSAutoStructuredCloneBufferWrapper,
    MutableHandleValue,
};
use rustc_hash::FxHashMap;
use script_bindings::conversions::IDLInterface;
use script_bindings::conversions::get_dom_class;
use script_bindings::js::{
    GetPropertyKeys, JS_GetPropertyById, JS_GetPrototype, JS_IdToValue, JS_IsExceptionPending,
    JS_SetPendingException,
};
use script_bindings::serializable::Serializable;
use script_bindings::structuredclone::{
    StructuredData, StructuredDataReader, StructuredDataWriter,
};
use servo_base::id::{Index, NamespaceIndex, PipelineNamespaceId};
use servo_constellation_traits::{
    Serializable as SerializableInterface, StructuredSerializedData,
    Transferrable as TransferrableInterface,
};
use strum::IntoEnumIterator;

use crate::dom::bindings::conversions::root_from_object;
use crate::dom::bindings::error::{Error, Fallible};
use crate::dom::bindings::root::DomRoot;
use crate::dom::bindings::serializable::StorageKey;
use crate::dom::bindings::transferable::Transferable;
use crate::dom::blob::Blob;
#[cfg(feature = "subtlecrypto")]
use crate::dom::cryptokey::CryptoKey;
use crate::dom::dompoint::DOMPoint;
use crate::dom::dompointreadonly::DOMPointReadOnly;
use crate::dom::file::File;
use crate::dom::filelist::FileList;
use crate::dom::globalscope::GlobalScope;
use crate::dom::imagebitmap::ImageBitmap;
use crate::dom::imagedata::ImageData;
use crate::dom::messageport::MessagePort;
use crate::dom::offscreencanvas::OffscreenCanvas;
use crate::dom::stream::readablestream::ReadableStream;
use crate::dom::stream::writablestream::WritableStream;
use crate::dom::types::{
    DOMException, DOMMatrix, DOMMatrixReadOnly, DOMQuad, DOMRect, DOMRectReadOnly,
    QuotaExceededError, TransformStream,
};
use crate::realms::enter_auto_realm;

// ---- Structured-clone diagnostics (LUCINANET_CLONE_DEBUG=1) -----------------------------
//
// Ephemeral discovery aid (spec 08): when a clone is refused, print the refused value's
// class, prototype chain, best-effort property path from the root, and the operation. Read
// only; the behaviour is unchanged unless the environment variable is set. The native
// refusal (`mozjs-sys/src/engine/clone.rs` `data_clone_error`) never calls back into the
// embedder, so to learn which sub-value the glue refused we walk the root ourselves here.

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

fn clone_debug_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("LUCINANET_CLONE_DEBUG")
            .map(|v| v == "1")
            .unwrap_or(false)
    })
}

thread_local! {
    static CLONE_OP: RefCell<&'static str> = const { RefCell::new("(unlabeled)") };
    /// Nodes visited by the current diagnostic walk, to bound pathological payloads.
    static CLONE_WALK_VISITS: Cell<usize> = const { Cell::new(0) };
    /// Standalone serializations left in the current probe search.
    static CLONE_PROBE_TRIES: Cell<i32> = const { Cell::new(0) };
}

/// Ask the serializer what it refuses, rather than inferring it. Off by default because it
/// re-enters `JS_WriteStructuredClone` once per candidate.
fn clone_probe_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("LUCINANET_CLONE_PROBE")
            .map(|v| v == "1")
            .unwrap_or(false)
    })
}

/// Search bounds for the diagnostic walker. YouTube's state payload is deep and wide, so the
/// offending sub-value can sit far from the root; these are generous but capped by the visit
/// budget below.
const CLONE_WALK_MAX_DEPTH: usize = 32;
const CLONE_WALK_MAX_KEYS: usize = 128;
const CLONE_WALK_MAX_LINES: usize = 1500;
const CLONE_WALK_MAX_VISITS: usize = 40_000;
/// Bounds for the opt-in probe, which costs one real serialization per candidate.
const CLONE_PROBE_MAX_DEPTH: usize = 8;
const CLONE_PROBE_MAX_TRIES: i32 = 600;

/// Prototypes that `write_object` dispatches on *before* it ever reaches `is_plain_object`
/// (`mozjs-sys/src/engine/clone.rs:285-331`): a value carrying one of these is serialized by tag,
/// not refused by the prototype gate. Matched by prototype constructor name, so it is a filter for
/// the structural hint, not a second implementation of the spec — the probe asks the serializer.
/// Typed arrays are covered by the `Array` suffix at the use site.
const GLUE_TAGGED_PROTOS: [&str; 23] = [
    "Array",
    "Date",
    "RegExp",
    "Map",
    "Set",
    "WeakMap",
    "WeakSet",
    "Error",
    "EvalError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "TypeError",
    "URIError",
    "ArrayBuffer",
    "SharedArrayBuffer",
    "DataView",
    "Boolean",
    "Number",
    "String",
    "BigInt",
    "Function",
    "Promise",
];

/// A reflected DOM object: `write_object` sends it to `embed::is_host_object` -> `write_host`
/// (clone.rs:327) and the engine's own callback, so whatever refuses it is not the prototype gate.
/// `GetBuiltinClass` cannot tell these apart from page objects in this engine (it answers
/// `ESClass::Other` for `new F()` instances too), hence the binding's own class check.
unsafe fn is_dom_host_object(obj: *mut JSObject) -> bool {
    matches!(unsafe { get_dom_class(obj) }, Ok(_))
}

/// Best-effort record of which API triggered the next clone. Diagnostic only.
pub(crate) fn note_clone_operation(op: &'static str) {
    if clone_debug_enabled() {
        CLONE_OP.with(|c| *c.borrow_mut() = op);
    }
}

unsafe fn value_to_string(cx: &mut JSContext, v: HandleValue) -> String {
    match std::ptr::NonNull::new(unsafe { JS_ValueToSource(cx, v) }) {
        Some(s) => unsafe { jsstr_to_string(cx, s) },
        None => String::new(),
    }
}

/// A JS string value's *contents*, unquoted. `JS_ValueToSource` wraps strings in one pair of
/// double quotes; stripping that pair recovers the contents, so comparisons like
/// `ctor == "Object"` and the surfaced DOMException message are exact. Non-string values fall
/// back to their source form.
unsafe fn string_contents(cx: &mut JSContext, v: HandleValue) -> String {
    let src = unsafe { value_to_string(cx, v) };
    if v.is_string() {
        if let Some(inner) = src.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            return inner.to_string();
        }
    }
    src
}

unsafe fn property_name(cx: &mut JSContext, obj: js::rust::HandleObject, name: &[u8]) -> String {
    let cname = name.as_ptr() as *const raw::c_char;
    rooted!(&in(cx) let mut v = UndefinedValue());
    if !JS_GetProperty(cx, obj, cname, v.handle_mut()) {
        return String::new();
    }
    unsafe { string_contents(cx, v.handle()) }
}

/// `obj.constructor.name`, empty if absent.
unsafe fn constructor_name(cx: &mut JSContext, obj: js::rust::HandleObject) -> String {
    rooted!(&in(cx) let mut ctor = UndefinedValue());
    let cname = b"constructor\0".as_ptr() as *const raw::c_char;
    if !JS_GetProperty(cx, obj, cname, ctor.handle_mut()) || !ctor.is_object() {
        return String::new();
    }
    rooted!(&in(cx) let ctor_obj = ctor.get().to_object());
    unsafe { property_name(cx, ctor_obj.handle(), b"name\0") }
}

/// The current realm's `%Object.prototype%`, which is the one value the glue's `is_plain_object`
/// compares every prototype against (`mozjs-sys/src/engine/clone.rs:436`, read-only here). `None`
/// if it cannot be named, in which case callers fall back to a looser test.
unsafe fn realm_object_prototype(cx: &mut JSContext) -> Option<*mut JSObject> {
    let global = unsafe { CurrentGlobalOrNull(cx) };
    if global.is_null() {
        return None;
    }
    rooted!(&in(cx) let gval = ObjectValue(global));
    rooted!(&in(cx) let gobj = gval.get().to_object());
    rooted!(&in(cx) let mut ctor = UndefinedValue());
    let oname = b"Object\0".as_ptr() as *const raw::c_char;
    if !JS_GetProperty(cx, gobj.handle(), oname, ctor.handle_mut()) || !ctor.is_object() {
        return None;
    }
    rooted!(&in(cx) let ctor_obj = ctor.get().to_object());
    rooted!(&in(cx) let mut proto = UndefinedValue());
    let pname = b"prototype\0".as_ptr() as *const raw::c_char;
    if !JS_GetProperty(cx, ctor_obj.handle(), pname, proto.handle_mut()) || !proto.is_object() {
        return None;
    }
    rooted!(&in(cx) let proto_obj = proto.get().to_object());
    Some(proto_obj.handle().get())
}

/// How a value looks to the glue: its builtin class, its own constructor name and the constructor
/// name of its prototype, plus whether that prototype is the realm's `Object.prototype`.
unsafe fn clone_debug_shape(
    cx: &mut JSContext,
    val: HandleValue,
    objproto: Option<*mut JSObject>,
) -> String {
    if val.is_symbol() {
        return "symbol".to_owned();
    }
    if !val.is_object() {
        return format!("primitive `{}`", unsafe { value_to_string(cx, val) });
    }
    rooted!(&in(cx) let o = val.to_object());
    let mut esclass = ESClass::Other;
    let got = GetBuiltinClass(cx, o.handle(), &mut esclass as *mut _);
    let ctor = unsafe { constructor_name(cx, o.handle()) };
    rooted!(&in(cx) let mut proto = ptr::null_mut::<JSObject>());
    let have_proto = JS_GetPrototype(cx, o.handle(), proto.handle_mut()) && !proto.is_null();
    let pctor = if have_proto {
        unsafe { constructor_name(cx, proto.handle()) }
    } else {
        String::new()
    };
    let same = match objproto {
        Some(p) => have_proto && proto.handle().get() == p,
        None => have_proto && pctor == "Object",
    };
    let proto_desc = if !have_proto {
        "null".to_owned()
    } else {
        format!(
            "ctor={pctor}{}",
            if same {
                ", realm Object.prototype"
            } else {
                ", NOT the realm's Object.prototype"
            }
        )
    };
    format!(
        "esclass={:?} ctor={:?} prototype[{}]{}",
        if got { esclass } else { ESClass::Other },
        ctor,
        proto_desc,
        match own_key_count(cx, o.handle()) {
            Some(n) => format!(" ownKeys={n}"),
            None => String::new(),
        }
    )
}

/// Own enumerable string keys of an object, if they can be read.
unsafe fn own_key_count(cx: &mut JSContext, obj: js::rust::HandleObject) -> Option<usize> {
    let mut ids = IdVector::new(cx);
    if !GetPropertyKeys(cx, obj, js::jsapi::JSITER_OWNONLY, ids.handle_mut()) {
        return None;
    }
    Some(ids.len())
}

/// Ask the serializer about one value on its own. `Some(true)` = it refuses this value,
/// `Some(false)` = it serializes it, `None` = not asked (a reflected DOM object, or the budget is
/// spent). Nothing is transferred: the transfer list is empty, so asking copies and detaches
/// nothing the page owns.
unsafe fn clone_probe_verdict(cx: &mut JSContext, val: HandleValue) -> Option<bool> {
    if !val.is_object() || CLONE_PROBE_TRIES.with(|t| t.get()) <= 0 {
        return None;
    }
    rooted!(&in(cx) let o = val.to_object());
    if unsafe { is_dom_host_object(o.handle().get()) } {
        return None;
    }
    CLONE_PROBE_TRIES.with(|t| t.set(t.get() - 1));
    let mut sc_writer = StructuredDataWriter::default();
    let sc_writer_ptr = &mut sc_writer as *mut _;
    let scbuf = JSAutoStructuredCloneBufferWrapper::new(
        StructuredCloneScope::DifferentProcess,
        &STRUCTURED_CLONE_CALLBACKS,
    );
    let scdata = unsafe { &mut ((*scbuf.as_raw_ptr()).data_) };
    let policy = CloneDataPolicy {
        allowIntraClusterClonableSharedObjects_: false,
        allowSharedMemoryObjects_: false,
    };
    rooted!(&in(cx) let mut transfer = UndefinedValue());
    let ok = unsafe {
        JS_WriteStructuredClone(
            cx,
            val,
            scdata,
            StructuredCloneScope::DifferentProcess,
            &policy,
            &STRUCTURED_CLONE_CALLBACKS,
            sc_writer_ptr as *mut raw::c_void,
            transfer.handle(),
        )
    };
    if JS_IsExceptionPending(cx) {
        unsafe { JS_ClearPendingException(cx) };
    }
    Some(!ok)
}

/// Narrow a refused clone down to one value by asking the serializer about each sub-value in turn.
/// Returns true once it has printed the smallest candidate that still fails on its own.
#[cfg_attr(crown, allow(crown::unrooted_must_root_lint::must_root))]
unsafe fn clone_debug_probe(
    cx: &mut JSContext,
    path: &str,
    val: HandleValue,
    depth: usize,
    objproto: Option<*mut JSObject>,
    lines: &mut Vec<String>,
) -> bool {
    let verdict = unsafe { clone_probe_verdict(cx, val) };
    // Descend when this value fails or when it could not be asked; stop at one the serializer
    // accepts, because a refusal inside it would not have been reached in the first place.
    if !matches!(verdict, Some(false)) && depth < CLONE_PROBE_MAX_DEPTH && val.is_object() {
        rooted!(&in(cx) let o = val.to_object());
        let mut ids = IdVector::new(cx);
        if GetPropertyKeys(cx, o.handle(), js::jsapi::JSITER_OWNONLY, ids.handle_mut()) {
            for id in ids.iter().take(CLONE_WALK_MAX_KEYS) {
                if CLONE_PROBE_TRIES.with(|t| t.get()) <= 0 {
                    break;
                }
                rooted!(&in(cx) let id = *id);
                rooted!(&in(cx) let mut keyv = UndefinedValue());
                let key = if JS_IdToValue(cx, id.handle().get(), keyv.handle_mut()) {
                    unsafe { value_to_string(cx, keyv.handle()) }
                } else {
                    String::from("(key?)")
                };
                rooted!(&in(cx) let mut child = UndefinedValue());
                if !JS_GetPropertyById(cx, o.handle(), id.handle(), child.handle_mut()) {
                    if JS_IsExceptionPending(cx) {
                        unsafe { JS_ClearPendingException(cx) };
                    }
                    continue;
                }
                let childpath = format!("{path}.{key}");
                if unsafe {
                    clone_debug_probe(cx, &childpath, child.handle(), depth + 1, objproto, lines)
                } {
                    return true;
                }
            }
        }
    }
    if verdict == Some(true) {
        lines.push(format!(
            "{path}: SERIALIZER REFUSES THIS VALUE ON ITS OWN -- {}",
            unsafe { clone_debug_shape(cx, val, objproto) }
        ));
        return true;
    }
    false
}

#[cfg_attr(crown, allow(crown::unrooted_must_root_lint::must_root))]
unsafe fn clone_debug_walk(
    cx: &mut JSContext,
    path: &str,
    val: HandleValue,
    depth: usize,
    objproto: Option<*mut JSObject>,
    seen: &mut Vec<*mut JSObject>,
    lines: &mut Vec<String>,
) {
    let visits = CLONE_WALK_VISITS.with(|v| {
        let n = v.get() + 1;
        v.set(n);
        n
    });
    if lines.len() >= CLONE_WALK_MAX_LINES || visits > CLONE_WALK_MAX_VISITS {
        return;
    }
    if val.is_symbol() {
        lines.push(format!(
            "{path}: symbol  <-- glue REFUSES (data_clone_error)"
        ));
        return;
    }
    if !val.is_object() {
        return;
    }
    rooted!(&in(cx) let obj = val.to_object());
    let ptr = obj.handle().get();
    if seen.contains(&ptr) {
        lines.push(format!("{path}: [cyclic reference]"));
        return;
    }
    seen.push(ptr);
    let mut esclass = ESClass::Other;
    let got = GetBuiltinClass(cx, obj.handle(), &mut esclass as *mut _);
    if got && esclass == ESClass::Function {
        let name = unsafe { constructor_name(cx, obj.handle()) };
        lines.push(format!(
            "{path}: function {name}  <-- glue REFUSES (data_clone_error)"
        ));
        seen.pop();
        return;
    }
    // The glue's `is_plain_object` (clone.rs:436) accepts a prototype only when it *is* the current
    // realm's `%Object.prototype%`, or when there is none. A constructor name of "Object" is not
    // enough: `Object.create(base)` and an object from an iframe both read
    // `constructor.name === "Object"` and are both refused, so compare identity.
    //
    // `esclass` is no use for spotting the ordinary objects here: this engine reports
    // `ESClass::Other` for values a page constructed with `new F()` — exactly what the gate refuses
    // — and for DOM objects too. So the test runs on any object that is not a DOM reflection and
    // not an array, with the prototypes the glue serializes by tag filtered out by name.
    let mut is_array = false;
    let array_known = IsArray(cx, obj.handle(), &mut is_array);
    let skip = unsafe { is_dom_host_object(ptr) } || (array_known && is_array);
    if !skip {
        rooted!(&in(cx) let mut proto = ptr::null_mut::<JSObject>());
        if JS_GetPrototype(cx, obj.handle(), proto.handle_mut()) && !proto.is_null() {
            let pctor = unsafe { constructor_name(cx, proto.handle()) };
            let refused = match objproto {
                Some(p) => proto.handle().get() != p,
                // No realm prototype to compare with: fall back to the looser name test so the
                // walker does not report every object as an offender.
                None => pctor.is_empty() || pctor != "Object",
            };
            let tagged = GLUE_TAGGED_PROTOS.contains(&pctor.as_str()) || pctor.ends_with("Array");
            if pctor == "Function" {
                // A callable, whatever `GetBuiltinClass` makes of it: spec step 22 refuses it and
                // so does the glue, at the same fall-through.
                let name = unsafe { constructor_name(cx, obj.handle()) };
                lines.push(format!(
                    "{path}: function {name}  <-- glue REFUSES (data_clone_error)"
                ));
            } else if refused && !tagged {
                lines.push(format!(
                    "{path}: ordinary object, prototype ctor={pctor}{}  <-- glue REFUSES (is_plain_object wants the current realm's Object.prototype)",
                    if pctor == "Object" { ", not this realm's" } else { "" }
                ));
            }
        }
    }
    if depth < CLONE_WALK_MAX_DEPTH {
        unsafe { clone_debug_props(cx, path, obj.handle(), depth, objproto, seen, lines) };
    }
    seen.pop();
}

#[cfg_attr(crown, allow(crown::unrooted_must_root_lint::must_root))]
unsafe fn clone_debug_props(
    cx: &mut JSContext,
    path: &str,
    obj: js::rust::HandleObject,
    depth: usize,
    objproto: Option<*mut JSObject>,
    seen: &mut Vec<*mut JSObject>,
    lines: &mut Vec<String>,
) {
    let mut ids = IdVector::new(cx);
    if !GetPropertyKeys(
        cx,
        obj,
        js::jsapi::JSITER_OWNONLY | js::jsapi::JSITER_SYMBOLS,
        ids.handle_mut(),
    ) {
        return;
    }
    for id in ids.iter().take(CLONE_WALK_MAX_KEYS) {
        rooted!(&in(cx) let id = *id);
        if id.is_symbol() {
            rooted!(&in(cx) let mut keyv = UndefinedValue());
            if JS_IdToValue(cx, id.handle().get(), keyv.handle_mut()) {
                let key = unsafe { value_to_string(cx, keyv.handle()) };
                lines.push(format!(
                    "{path}.[{key}] (symbol key)  <-- glue REFUSES symbol keys"
                ));
            }
            continue;
        }
        rooted!(&in(cx) let mut keyv = UndefinedValue());
        let key = if JS_IdToValue(cx, id.handle().get(), keyv.handle_mut()) {
            unsafe { value_to_string(cx, keyv.handle()) }
        } else {
            String::new()
        };
        rooted!(&in(cx) let mut child = UndefinedValue());
        if !JS_GetPropertyById(cx, obj, id.handle(), child.handle_mut()) {
            continue;
        }
        let childpath = format!("{path}.{key}");
        unsafe {
            clone_debug_walk(
                cx,
                &childpath,
                child.handle(),
                depth + 1,
                objproto,
                seen,
                lines,
            )
        };
    }
}

/// Log the refused value tree. Called from `write()` after a refused clone. Whatever exception the
/// serializer left pending is saved and restored around this: reading properties can itself raise
/// (a throwing getter), and a diagnostic must not change what `write()` decides to throw.
unsafe fn clone_debug_refused(cx: &mut JSContext, root: HandleValue, reason: &str) {
    let op = CLONE_OP.with(|c| *c.borrow());
    CLONE_WALK_VISITS.with(|v| v.set(0));
    let had_pending = JS_IsExceptionPending(cx);
    rooted!(&in(cx) let mut saved = UndefinedValue());
    if had_pending {
        JS_GetPendingException(cx, saved.handle_mut());
    }
    // The very value the glue compares prototypes against.
    let objproto = unsafe { realm_object_prototype(cx) };
    if JS_IsExceptionPending(cx) {
        JS_ClearPendingException(cx);
    }

    let mut lines = Vec::new();
    let mut seen = Vec::new();
    unsafe { clone_debug_walk(cx, "root", root, 0, objproto, &mut seen, &mut lines) };
    let visits = CLONE_WALK_VISITS.with(|v| v.get());

    // Ask the serializer about values one by one: it names the offender by verdict, not by guess.
    let probed = clone_probe_enabled();
    if probed {
        CLONE_PROBE_TRIES.with(|t| t.set(CLONE_PROBE_MAX_TRIES));
        unsafe { clone_debug_probe(cx, "root", root, 0, objproto, &mut lines) };
    }
    let probes = CLONE_PROBE_MAX_TRIES - CLONE_PROBE_TRIES.with(|t| t.get());

    // Describe the root itself: a class instance or exotic host object at the top needs no descent.
    let mut root_shape = String::new();
    if root.is_object() {
        rooted!(&in(cx) let o = root.to_object());
        let mut esclass = ESClass::Other;
        let _ = GetBuiltinClass(cx, o.handle(), &mut esclass as *mut _);
        let ctor = unsafe { constructor_name(cx, o.handle()) };
        let ordinary = esclass == ESClass::Object;
        root_shape = format!(
            " esclass_ordinary={ordinary} root_ctor={ctor:?} root_shape={}",
            unsafe { clone_debug_shape(cx, root, objproto) }
        );
    }
    println!(
        "LUCINANET_CLONE: refused op={op} reason={reason} offenders={} nodes={visits}{root_shape} (searched depth<={CLONE_WALK_MAX_DEPTH}, keys<={CLONE_WALK_MAX_KEYS}/level, realm Object.prototype {})",
        lines.len(),
        if objproto.is_some() {
            "read"
        } else {
            "UNREADABLE"
        }
    );
    if probed {
        println!(
            "LUCINANET_CLONE: probe serializations used={probes} (depth<={CLONE_PROBE_MAX_DEPTH})"
        );
    }
    for l in lines {
        println!("LUCINANET_CLONE:   {l}");
    }

    if had_pending {
        JS_SetPendingException(cx, saved.handle(), ExceptionStackBehavior::DoNotCapture);
    } else {
        JS_ClearPendingException(cx);
    }
}

/// The message of a pending native clone-refusal `Error`, if the pending exception is one.
/// A script-thrown error (e.g. a throwing getter) returns `None` so it propagates intact.
unsafe fn pending_clone_refusal_message(cx: &mut JSContext) -> Option<String> {
    rooted!(&in(cx) let mut ex = UndefinedValue());
    if !JS_GetPendingException(cx, ex.handle_mut()) || !ex.is_object() {
        return None;
    }
    rooted!(&in(cx) let ex_obj = ex.get().to_object());
    let msg = unsafe { property_name(cx, ex_obj.handle(), b"message\0") };
    if msg.contains("could not be cloned") {
        Some(msg)
    } else {
        None
    }
}

// TODO: Should we add Min and Max const to https://github.com/servo/rust-mozjs/blob/master/src/consts.rs?
// TODO: Determine for sure which value Min and Max should have.
// NOTE: Current values found at https://dxr.mozilla.org/mozilla-central/
// rev/ff04d410e74b69acfab17ef7e73e7397602d5a68/js/public/StructuredClone.h#323
#[repr(u32)]
pub(super) enum StructuredCloneTags {
    /// To support additional types, add new tags with values incremented from the last one before Max.
    Min = 0xFFFF8000,
    DomBlob = 0xFFFF8001,
    DomFile = 0xFFFF8002,
    DomFileList = 0xFFFF8003,
    MessagePort = 0xFFFF8004,
    Principals = 0xFFFF8005,
    DomPointReadOnly = 0xFFFF8006,
    DomPoint = 0xFFFF8007,
    ReadableStream = 0xFFFF8008,
    DomException = 0xFFFF8009,
    WritableStream = 0xFFFF800A,
    TransformStream = 0xFFFF800B,
    ImageBitmap = 0xFFFF800C,
    OffscreenCanvas = 0xFFFF800D,
    QuotaExceededError = 0xFFFF800E,
    DomRect = 0xFFFF800F,
    DomRectReadOnly = 0xFFFF8010,
    DomQuad = 0xFFFF8011,
    DomMatrix = 0xFFFF8012,
    DomMatrixReadOnly = 0xFFFF8013,
    ImageData = 0xFFFF8014,
    #[cfg(feature = "subtlecrypto")]
    CryptoKey = 0xFFFF8015,
    Max = 0xFFFFFFFF,
}

impl From<SerializableInterface> for StructuredCloneTags {
    fn from(v: SerializableInterface) -> Self {
        match v {
            SerializableInterface::File => StructuredCloneTags::DomFile,
            SerializableInterface::FileList => StructuredCloneTags::DomFileList,
            SerializableInterface::Blob => StructuredCloneTags::DomBlob,
            SerializableInterface::DomPoint => StructuredCloneTags::DomPoint,
            SerializableInterface::DomPointReadOnly => StructuredCloneTags::DomPointReadOnly,
            SerializableInterface::DomRect => StructuredCloneTags::DomRect,
            SerializableInterface::DomRectReadOnly => StructuredCloneTags::DomRectReadOnly,
            SerializableInterface::DomQuad => StructuredCloneTags::DomQuad,
            SerializableInterface::DomMatrix => StructuredCloneTags::DomMatrix,
            SerializableInterface::DomMatrixReadOnly => StructuredCloneTags::DomMatrixReadOnly,
            SerializableInterface::DomException => StructuredCloneTags::DomException,
            SerializableInterface::ImageBitmap => StructuredCloneTags::ImageBitmap,
            SerializableInterface::QuotaExceededError => StructuredCloneTags::QuotaExceededError,
            SerializableInterface::ImageData => StructuredCloneTags::ImageData,
            #[cfg(feature = "subtlecrypto")]
            SerializableInterface::CryptoKey => StructuredCloneTags::CryptoKey,
        }
    }
}

impl From<TransferrableInterface> for StructuredCloneTags {
    fn from(v: TransferrableInterface) -> Self {
        match v {
            TransferrableInterface::ImageBitmap => StructuredCloneTags::ImageBitmap,
            TransferrableInterface::MessagePort => StructuredCloneTags::MessagePort,
            TransferrableInterface::OffscreenCanvas => StructuredCloneTags::OffscreenCanvas,
            TransferrableInterface::ReadableStream => StructuredCloneTags::ReadableStream,
            TransferrableInterface::WritableStream => StructuredCloneTags::WritableStream,
            TransferrableInterface::TransformStream => StructuredCloneTags::TransformStream,
        }
    }
}

fn reader_for_type(
    val: SerializableInterface,
) -> unsafe fn(
    cx: &mut JSContext,
    &GlobalScope,
    *mut JSStructuredCloneReader,
    &mut StructuredDataReader<'_>,
) -> *mut JSObject {
    match val {
        SerializableInterface::File => read_object::<File>,
        SerializableInterface::FileList => read_object::<FileList>,
        SerializableInterface::Blob => read_object::<Blob>,
        SerializableInterface::DomPoint => read_object::<DOMPoint>,
        SerializableInterface::DomPointReadOnly => read_object::<DOMPointReadOnly>,
        SerializableInterface::DomRect => read_object::<DOMRect>,
        SerializableInterface::DomRectReadOnly => read_object::<DOMRectReadOnly>,
        SerializableInterface::DomQuad => read_object::<DOMQuad>,
        SerializableInterface::DomMatrix => read_object::<DOMMatrix>,
        SerializableInterface::DomMatrixReadOnly => read_object::<DOMMatrixReadOnly>,
        SerializableInterface::DomException => read_object::<DOMException>,
        SerializableInterface::ImageBitmap => read_object::<ImageBitmap>,
        SerializableInterface::QuotaExceededError => read_object::<QuotaExceededError>,
        SerializableInterface::ImageData => read_object::<ImageData>,
        #[cfg(feature = "subtlecrypto")]
        SerializableInterface::CryptoKey => read_object::<CryptoKey>,
    }
}

unsafe fn read_object<T: Serializable<crate::DomTypeHolder>>(
    cx: &mut JSContext,
    owner: &GlobalScope,
    r: *mut JSStructuredCloneReader,
    sc_reader: &mut StructuredDataReader<'_>,
) -> *mut JSObject {
    let mut name_space: u32 = 0;
    let mut index: u32 = 0;
    unsafe {
        if !JS_ReadUint32Pair(r, &mut name_space as *mut u32, &mut index as *mut u32) {
            // LucinaNet: a poisoned or terminated engine answers false and leaves the
            // pair unread. That is this reader's own failure, not a broken invariant, and the
            // caller already has the null answer for a clone it could not read.
            return ptr::null_mut();
        }
    }
    let storage_key = StorageKey { index, name_space };

    // 1. Re-build the key for the storage location
    // of the serialized object.
    let id: NamespaceIndex<T::Index> = storage_key.into();

    // 2. Get the transferred object from its storage, using the key.
    let objects = T::serialized_storage(StructuredData::Reader(sc_reader));
    let objects_map = objects
        .as_mut()
        .expect("The SC holder does not have any relevant objects");
    let serialized = objects_map
        .remove(&id)
        .expect("No object to be deserialized found.");
    if objects_map.is_empty() {
        *objects = None;
    }

    if let Ok(obj) = T::deserialize(cx, owner, serialized) {
        let reflector = obj.reflector().get_jsobject().get();
        sc_reader.roots.push(Heap::boxed(reflector));
        return reflector;
    }
    warn!("Reading structured data failed in {:?}.", owner.get_url());
    ptr::null_mut()
}

unsafe fn write_object<T: Serializable<crate::DomTypeHolder>>(
    no_gc: &NoGC,
    interface: SerializableInterface,
    owner: &GlobalScope,
    object: &T,
    w: *mut JSStructuredCloneWriter,
    sc_writer: &mut StructuredDataWriter,
) -> bool {
    if let Ok((new_id, serialized)) = object.serialize(no_gc) {
        let objects = T::serialized_storage(StructuredData::Writer(sc_writer))
            .get_or_insert(FxHashMap::default());
        objects.insert(new_id, serialized);
        let storage_key = StorageKey::new(new_id);

        unsafe {
            // LucinaNet: refused, so nothing was written; `false` is this writer's own
            // failure and the clone reports it, instead of an assert turning a refusal into a panic.
            if !JS_WriteUint32PairUnchecked(w, StructuredCloneTags::from(interface) as u32, 0)
                || !JS_WriteUint32PairUnchecked(w, storage_key.name_space, storage_key.index)
            {
                return false;
            }
        }
        return true;
    }
    warn!("Writing structured data failed in {:?}.", owner.get_url());
    false
}

unsafe extern "C" fn read_callback(
    cx: *mut RawJSContext,
    r: *mut JSStructuredCloneReader,
    _policy: *const CloneDataPolicy,
    tag: u32,
    _data: u32,
    closure: *mut raw::c_void,
) -> *mut JSObject {
    let mut wrap_result = ptr::null_mut();
    js::panic::wrap_panic(&mut || {
        wrap_result = (|| -> *mut JSObject {
            assert!(
                tag < StructuredCloneTags::Max as u32,
                "tag should be lower than StructuredCloneTags::Max"
            );
            assert!(
                tag > StructuredCloneTags::Min as u32,
                "tag should be higher than StructuredCloneTags::Min"
            );

            // SAFETY: it is safe to construct a JSContext from engine hook.
            let mut cx = unsafe { JSContext::from_ptr(NonNull::new(cx).unwrap()) };
            let cx = &mut cx;

            let sc_reader = unsafe { &mut *(closure as *mut StructuredDataReader<'_>) };

            let mut realm = CurrentRealm::assert(cx);
            let global = GlobalScope::from_current_realm(&mut realm);

            for serializable in SerializableInterface::iter() {
                if tag == StructuredCloneTags::from(serializable) as u32 {
                    let reader = reader_for_type(serializable);
                    return unsafe { reader(cx, &global, r, sc_reader) };
                }
            }

            ptr::null_mut()
        })()
    });
    wrap_result
}

enum OperationError {
    InterfaceDoesNotMatch,
    Exception(Error),
}

unsafe fn try_serialize<T: Serializable<crate::DomTypeHolder> + IDLInterface>(
    cx: &mut JSContext,
    val: SerializableInterface,
    object: RawHandleObject,
    global: &GlobalScope,
    w: *mut JSStructuredCloneWriter,
    writer: &mut StructuredDataWriter,
) -> Result<bool, OperationError> {
    let object = unsafe { root_from_object::<T>(cx, *object) };
    if let Ok(obj) = object {
        return unsafe { Ok(write_object(cx.no_gc(), val, global, &*obj, w, writer)) };
    }
    Err(OperationError::InterfaceDoesNotMatch)
}

type SerializeOperation = unsafe fn(
    &mut JSContext,
    SerializableInterface,
    RawHandleObject,
    &GlobalScope,
    *mut JSStructuredCloneWriter,
    &mut StructuredDataWriter,
) -> Result<bool, OperationError>;

fn serialize_for_type(val: SerializableInterface) -> SerializeOperation {
    match val {
        SerializableInterface::File => try_serialize::<File>,
        SerializableInterface::FileList => try_serialize::<FileList>,
        SerializableInterface::Blob => try_serialize::<Blob>,
        SerializableInterface::DomPoint => try_serialize::<DOMPoint>,
        SerializableInterface::DomPointReadOnly => try_serialize::<DOMPointReadOnly>,
        SerializableInterface::DomRect => try_serialize::<DOMRect>,
        SerializableInterface::DomRectReadOnly => try_serialize::<DOMRectReadOnly>,
        SerializableInterface::DomQuad => try_serialize::<DOMQuad>,
        SerializableInterface::DomMatrix => try_serialize::<DOMMatrix>,
        SerializableInterface::DomMatrixReadOnly => try_serialize::<DOMMatrixReadOnly>,
        SerializableInterface::DomException => try_serialize::<DOMException>,
        SerializableInterface::ImageBitmap => try_serialize::<ImageBitmap>,
        SerializableInterface::QuotaExceededError => try_serialize::<QuotaExceededError>,
        SerializableInterface::ImageData => try_serialize::<ImageData>,
        #[cfg(feature = "subtlecrypto")]
        SerializableInterface::CryptoKey => try_serialize::<CryptoKey>,
    }
}

unsafe extern "C" fn write_callback(
    cx: *mut RawJSContext,
    w: *mut JSStructuredCloneWriter,
    obj: RawHandleObject,
    _same_process_scope_required: *mut bool,
    closure: *mut raw::c_void,
) -> bool {
    let mut wrap_result = false;
    js::panic::wrap_panic(&mut || {
        wrap_result = (|| -> bool {
            // SAFETY: it is safe to construct a JSContext from engine hook.
            let mut cx = unsafe { JSContext::from_ptr(NonNull::new(cx).unwrap()) };
            let cx = &mut cx;

            let sc_writer = unsafe { &mut *(closure as *mut StructuredDataWriter) };

            let mut realm = CurrentRealm::assert(cx);
            let global = GlobalScope::from_current_realm(&mut realm);

            for serializable in SerializableInterface::iter() {
                let serializer = serialize_for_type(serializable);
                if let Ok(result) =
                    unsafe { serializer(cx, serializable, obj, &global, w, sc_writer) }
                {
                    return result;
                }
            }
            if clone_debug_enabled() {
                rooted!(&in(cx) let ov = unsafe { obj.get() });
                let name = unsafe { constructor_name(cx, ov.handle()) };
                let op = CLONE_OP.with(|c| *c.borrow());
                println!("LUCINANET_CLONE: write_callback refused host object ctor={name} op={op}");
            }
            false
        })()
    });
    wrap_result
}

fn receiver_for_type(
    val: TransferrableInterface,
) -> fn(
    &mut JSContext,
    &GlobalScope,
    &mut StructuredDataReader<'_>,
    u64,
    RawMutableHandleObject,
) -> Result<(), ()> {
    match val {
        TransferrableInterface::ImageBitmap => receive_object::<ImageBitmap>,
        TransferrableInterface::MessagePort => receive_object::<MessagePort>,
        TransferrableInterface::OffscreenCanvas => receive_object::<OffscreenCanvas>,
        TransferrableInterface::ReadableStream => receive_object::<ReadableStream>,
        TransferrableInterface::WritableStream => receive_object::<WritableStream>,
        TransferrableInterface::TransformStream => receive_object::<TransformStream>,
    }
}

fn receive_object<T: Transferable>(
    cx: &mut JSContext,
    owner: &GlobalScope,
    sc_reader: &mut StructuredDataReader<'_>,
    extra_data: u64,
    return_object: RawMutableHandleObject,
) -> Result<(), ()> {
    // 1. Re-build the key for the storage location
    // of the transferred object.
    let big: [u8; 8] = extra_data.to_ne_bytes();
    let (name_space, index) = big.split_at(4);

    let namespace_id = PipelineNamespaceId(u32::from_ne_bytes(
        name_space
            .try_into()
            .expect("name_space to be a slice of four."),
    ));
    let id: NamespaceIndex<T::Index> = NamespaceIndex {
        namespace_id,
        index: Index::new(u32::from_ne_bytes(
            index.try_into().expect("index to be a slice of four."),
        ))
        .expect("Index to be non-zero"),
    };

    // 2. Get the transferred object from its storage, using the key.
    let storage = T::serialized_storage(StructuredData::Reader(sc_reader));
    let serialized = if let Some(objects) = storage.as_mut() {
        let object = objects.remove(&id).expect("Transferred port to be stored");
        if objects.is_empty() {
            *storage = None;
        }
        object
    } else {
        panic!(
            "An interface was transfer-received, yet the SC holder does not have any serialized objects"
        );
    };

    let Ok(received) = T::transfer_receive(cx, owner, id, serialized) else {
        return Err(());
    };
    return_object.set(received.reflector().rootable().get());
    sc_reader.roots.push(Heap::boxed(return_object.get()));
    Ok(())
}

unsafe extern "C" fn read_transfer_callback(
    cx: *mut RawJSContext,
    _r: *mut JSStructuredCloneReader,
    _policy: *const CloneDataPolicy,
    tag: u32,
    _content: *mut raw::c_void,
    extra_data: u64,
    closure: *mut raw::c_void,
    return_object: RawMutableHandleObject,
) -> bool {
    let mut wrap_result = false;
    js::panic::wrap_panic(&mut || {
        wrap_result = (|| -> bool {
            let sc_reader = unsafe { &mut *(closure as *mut StructuredDataReader<'_>) };
            let mut cx = unsafe {
                // This is safe because we are in SM hook
                JSContext::from_ptr(
                    NonNull::new(cx).expect("JSContext pointer should not be null in SM hook"),
                )
            };
            let mut realm = CurrentRealm::assert(&mut cx);
            let owner = GlobalScope::from_current_realm(&mut realm);

            for transferrable in TransferrableInterface::iter() {
                if tag == StructuredCloneTags::from(transferrable) as u32 {
                    let transfer_receiver = receiver_for_type(transferrable);
                    if transfer_receiver(&mut realm, &owner, sc_reader, extra_data, return_object)
                        .is_ok()
                    {
                        return true;
                    }
                }
            }
            false
        })()
    });
    wrap_result
}

unsafe fn try_transfer<T: Transferable + IDLInterface>(
    interface: TransferrableInterface,
    obj: RawHandleObject,
    cx: &mut JSContext,
    sc_writer: &mut StructuredDataWriter,
    tag: *mut u32,
    ownership: *mut TransferableOwnership,
    extra_data: *mut u64,
) -> Result<(), OperationError> {
    let object = unsafe { root_from_object::<T>(cx, *obj) };
    let Ok(object) = object else {
        return Err(OperationError::InterfaceDoesNotMatch);
    };

    unsafe { *tag = StructuredCloneTags::from(interface) as u32 };
    unsafe { *ownership = TransferableOwnership::SCTAG_TMO_CUSTOM };

    let (id, object) = object.transfer(cx).map_err(OperationError::Exception)?;

    // 2. Store the transferred object at a given key.
    let objects = T::serialized_storage(StructuredData::Writer(sc_writer))
        .get_or_insert(FxHashMap::default());
    objects.insert(id, object);

    let index = id.index.0.get();

    let mut big: [u8; 8] = [0; 8];
    let name_space = id.namespace_id.0.to_ne_bytes();
    let index = index.to_ne_bytes();

    let (left, right) = big.split_at_mut(4);
    left.copy_from_slice(&name_space);
    right.copy_from_slice(&index);

    // 3. Return a u64 representation of the key where the object is stored.
    unsafe { *extra_data = u64::from_ne_bytes(big) };
    Ok(())
}

type TransferOperation = unsafe fn(
    TransferrableInterface,
    RawHandleObject,
    &mut JSContext,
    &mut StructuredDataWriter,
    *mut u32,
    *mut TransferableOwnership,
    *mut u64,
) -> Result<(), OperationError>;

fn transfer_for_type(val: TransferrableInterface) -> TransferOperation {
    match val {
        TransferrableInterface::ImageBitmap => try_transfer::<ImageBitmap>,
        TransferrableInterface::MessagePort => try_transfer::<MessagePort>,
        TransferrableInterface::OffscreenCanvas => try_transfer::<OffscreenCanvas>,
        TransferrableInterface::ReadableStream => try_transfer::<ReadableStream>,
        TransferrableInterface::WritableStream => try_transfer::<WritableStream>,
        TransferrableInterface::TransformStream => try_transfer::<TransformStream>,
    }
}

/// <https://html.spec.whatwg.org/multipage/#structuredserializewithtransfer>
unsafe extern "C" fn write_transfer_callback(
    cx: *mut RawJSContext,
    obj: RawHandleObject,
    closure: *mut raw::c_void,
    tag: *mut u32,
    ownership: *mut TransferableOwnership,
    _content: *mut *mut raw::c_void,
    extra_data: *mut u64,
) -> bool {
    let mut wrap_result = false;
    js::panic::wrap_panic(&mut || {
        wrap_result = (|| -> bool {
            let sc_writer = unsafe { &mut *(closure as *mut StructuredDataWriter) };
            let mut cx = unsafe {
                // This is safe because we are in SM hook
                JSContext::from_ptr(
                    NonNull::new(cx).expect("JSContext pointer should not be null in SM hook"),
                )
            };
            for transferable in TransferrableInterface::iter() {
                let try_transfer = transfer_for_type(transferable);

                let transfer_result = unsafe {
                    try_transfer(
                        transferable,
                        obj,
                        &mut cx,
                        sc_writer,
                        tag,
                        ownership,
                        extra_data,
                    )
                };
                match transfer_result {
                    Err(error) => match error {
                        OperationError::InterfaceDoesNotMatch => {},
                        OperationError::Exception(error) => {
                            sc_writer.error = Some(error);
                            return false;
                        },
                    },
                    Ok(..) => return true,
                }
            }

            false
        })()
    });
    wrap_result
}

unsafe extern "C" fn free_transfer_callback(
    _tag: u32,
    _ownership: TransferableOwnership,
    _content: *mut raw::c_void,
    _extra_data: u64,
    _closure: *mut raw::c_void,
) {
}

unsafe fn can_transfer_for_type(
    cx: &mut JSContext,
    transferable: TransferrableInterface,
    obj: RawHandleObject,
) -> Result<bool, ()> {
    unsafe fn can_transfer<T: Transferable + IDLInterface>(
        cx: &mut JSContext,
        obj: RawHandleObject,
    ) -> Result<bool, ()> {
        unsafe { root_from_object::<T>(cx, *obj).map(|o| Transferable::can_transfer(&*o)) }
    }

    unsafe {
        match transferable {
            TransferrableInterface::ImageBitmap => can_transfer::<ImageBitmap>(cx, obj),
            TransferrableInterface::MessagePort => can_transfer::<MessagePort>(cx, obj),
            TransferrableInterface::OffscreenCanvas => can_transfer::<OffscreenCanvas>(cx, obj),
            TransferrableInterface::ReadableStream => can_transfer::<ReadableStream>(cx, obj),
            TransferrableInterface::WritableStream => can_transfer::<WritableStream>(cx, obj),
            TransferrableInterface::TransformStream => can_transfer::<TransformStream>(cx, obj),
        }
    }
}

unsafe extern "C" fn can_transfer_callback(
    cx: *mut RawJSContext,
    obj: RawHandleObject,
    _same_process_scope_required: *mut bool,
    _closure: *mut raw::c_void,
) -> bool {
    let mut wrap_result = false;
    js::panic::wrap_panic(&mut || {
        wrap_result = (|| -> bool {
            // SAFETY: it is safe to construct a JSContext from engine hook.
            let mut cx = unsafe { JSContext::from_ptr(NonNull::new(cx).unwrap()) };
            let cx = &mut cx;

            for transferable in TransferrableInterface::iter() {
                let can_transfer = unsafe { can_transfer_for_type(cx, transferable, obj) };
                if let Ok(can_transfer) = can_transfer {
                    return can_transfer;
                }
            }
            false
        })()
    });
    wrap_result
}

unsafe extern "C" fn report_error_callback(
    _cx: *mut RawJSContext,
    _errorid: u32,
    closure: *mut raw::c_void,
    error_message: *const ::std::os::raw::c_char,
) {
    js::panic::wrap_panic(&mut || {
        let msg_result = unsafe { CStr::from_ptr(error_message).to_str().map(str::to_string) };

        if let Ok(msg) = msg_result {
            let error = unsafe { &mut *(closure as *mut Option<Error>) };

            if clone_debug_enabled() {
                let op = CLONE_OP.with(|c| *c.borrow());
                println!("LUCINANET_CLONE: report_error msg={msg} op={op}");
            }

            if error.is_none() {
                *error = Some(Error::DataClone(Some(msg)));
            }
        }
    });
}

unsafe extern "C" fn sab_cloned_callback(
    _cx: *mut RawJSContext,
    _receiving: bool,
    _closure: *mut ::std::os::raw::c_void,
) -> bool {
    false
}

static STRUCTURED_CLONE_CALLBACKS: JSStructuredCloneCallbacks = JSStructuredCloneCallbacks {
    read: Some(read_callback),
    write: Some(write_callback),
    reportError: Some(report_error_callback),
    readTransfer: Some(read_transfer_callback),
    writeTransfer: Some(write_transfer_callback),
    freeTransfer: Some(free_transfer_callback),
    canTransfer: Some(can_transfer_callback),
    sabCloned: Some(sab_cloned_callback),
};

/// Writes a structured clone. Returns a `DataClone` error if that fails.
pub(crate) fn write(
    cx: &mut JSContext,
    message: HandleValue,
    transfer: Option<CustomAutoRooterGuard<Vec<*mut JSObject>>>,
) -> Fallible<StructuredSerializedData> {
    unsafe {
        rooted!(&in(cx) let mut val = UndefinedValue());
        if let Some(transfer) = transfer {
            transfer.to_jsval(cx, val.handle_mut());
        }
        let mut sc_writer = StructuredDataWriter::default();
        let sc_writer_ptr = &mut sc_writer as *mut _;

        let scbuf = JSAutoStructuredCloneBufferWrapper::new(
            StructuredCloneScope::DifferentProcess,
            &STRUCTURED_CLONE_CALLBACKS,
        );
        let scdata = &mut ((*scbuf.as_raw_ptr()).data_);
        let policy = CloneDataPolicy {
            allowIntraClusterClonableSharedObjects_: false,
            allowSharedMemoryObjects_: false,
        };
        let result = JS_WriteStructuredClone(
            cx,
            message,
            scdata,
            StructuredCloneScope::DifferentProcess,
            &policy,
            &STRUCTURED_CLONE_CALLBACKS,
            sc_writer_ptr as *mut raw::c_void,
            val.handle(),
        );
        if !result {
            let engine_error = sc_writer.error.take();
            let pending = JS_IsExceptionPending(cx);

            if clone_debug_enabled() {
                let reason = match &engine_error {
                    Some(_) => "engine-refusal",
                    None if pending => "glue-or-script-exception",
                    None => "unknown",
                };
                unsafe { clone_debug_refused(cx, message, reason) };
            }

            // Step 2: every refusal becomes a real "DataCloneError" DOMException (name
            // `DataCloneError`, legacy code 25) at this single site, for every operation
            // routed through here. A genuine JS exception, such as a getter that throws,
            // still propagates unchanged.
            if let Some(err) = engine_error {
                if pending {
                    unsafe { JS_ClearPendingException(cx) };
                }
                return Err(err);
            }
            if pending {
                if let Some(msg) = unsafe { pending_clone_refusal_message(cx) } {
                    unsafe { JS_ClearPendingException(cx) };
                    return Err(Error::DataClone(Some(msg)));
                }
                return Err(Error::JSFailed);
            }
            return Err(Error::DataClone(None));
        }

        let nbytes = GetLengthOfJSStructuredCloneData(scdata);
        let mut data = Vec::with_capacity(nbytes);
        CopyJSStructuredCloneData(scdata, data.as_mut_ptr());
        data.set_len(nbytes);

        let data = StructuredSerializedData {
            serialized: data,
            ports: sc_writer.ports.take(),
            transform_streams: sc_writer.transform_streams_port.take(),
            points: sc_writer.points.take(),
            rects: sc_writer.rects.take(),
            quads: sc_writer.quads.take(),
            matrices: sc_writer.matrices.take(),
            exceptions: sc_writer.exceptions.take(),
            quota_exceeded_errors: sc_writer.quota_exceeded_errors.take(),
            blobs: sc_writer.blobs.take(),
            files: sc_writer.files.take(),
            file_lists: sc_writer.file_lists.take(),
            image_bitmaps: sc_writer.image_bitmaps.take(),
            transferred_image_bitmaps: sc_writer.transferred_image_bitmaps.take(),
            offscreen_canvases: sc_writer.offscreen_canvases.take(),
            image_data: sc_writer.image_data.take(),
            crypto_keys: sc_writer.crypto_keys.take(),
        };

        Ok(data)
    }
}

/// Read structured serialized data, possibly containing transferred objects.
/// Returns a vec of rooted transfer-received ports, or an error.
pub(crate) fn read(
    cx: &mut JSContext,
    global: &GlobalScope,
    mut data: StructuredSerializedData,
    rval: MutableHandleValue,
) -> Fallible<Vec<DomRoot<MessagePort>>> {
    let mut realm = enter_auto_realm(cx, global);
    let cx = &mut realm.current_realm();

    rooted_vec!(let mut roots);
    let mut sc_reader = StructuredDataReader {
        error: None,
        roots,
        port_impls: data.ports.take(),
        transform_streams_port_impls: data.transform_streams.take(),
        blob_impls: data.blobs.take(),
        files: data.files.take(),
        file_lists: data.file_lists.take(),
        points: data.points.take(),
        rects: data.rects.take(),
        quads: data.quads.take(),
        matrices: data.matrices.take(),
        exceptions: data.exceptions.take(),
        quota_exceeded_errors: data.quota_exceeded_errors.take(),
        image_bitmaps: data.image_bitmaps.take(),
        transferred_image_bitmaps: data.transferred_image_bitmaps.take(),
        offscreen_canvases: data.offscreen_canvases.take(),
        image_data: data.image_data.take(),
        crypto_keys: data.crypto_keys.take(),
    };
    let sc_reader_ptr = &mut sc_reader as *mut _;
    unsafe {
        let scbuf = JSAutoStructuredCloneBufferWrapper::new(
            StructuredCloneScope::DifferentProcess,
            &STRUCTURED_CLONE_CALLBACKS,
        );
        let scdata = &mut ((*scbuf.as_raw_ptr()).data_);

        WriteBytesToJSStructuredCloneData(
            data.serialized.as_mut_ptr() as *const u8,
            data.serialized.len(),
            scdata,
        );

        let result = JS_ReadStructuredClone(
            cx,
            scdata,
            JS_STRUCTURED_CLONE_VERSION,
            StructuredCloneScope::DifferentProcess,
            rval,
            &CloneDataPolicy {
                allowIntraClusterClonableSharedObjects_: false,
                allowSharedMemoryObjects_: false,
            },
            &STRUCTURED_CLONE_CALLBACKS,
            sc_reader_ptr as *mut raw::c_void,
        );
        if !result {
            let error = if JS_IsExceptionPending(cx) {
                Error::JSFailed
            } else {
                sc_reader.error.unwrap_or(Error::DataClone(None))
            };

            return Err(error);
        }

        let mut message_ports = vec![];
        for reflector in sc_reader.roots.iter() {
            let Ok(message_port) = root_from_object::<MessagePort>(cx, reflector.get()) else {
                continue;
            };
            message_ports.push(message_port);
        }
        // Any transfer-received port-impls should have been taken out.
        assert!(sc_reader.port_impls.is_none());
        Ok(message_ports)
    }
}
