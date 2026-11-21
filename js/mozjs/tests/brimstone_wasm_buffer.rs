/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Raw-API tests for ArrayBuffers whose bytes are not the engine's: the detach key that says who may take them away, and the
// pointers `mozjs-sys/src/engine/typed.rs` and `engine/clone.rs` hand out for them (WebAssembly design §3, slice H1).
//
// One thing no test here can do at this pin is make a buffer keyed to a `WebAssembly.Memory`. The engine gives that key only to a
// buffer owned by an engine cell (`ExternalOwner::Cell`), and the cell API is `pub(crate)` in the fork until WebAssembly's own
// objects arrive with slice W0a. The three refusals written for that key — detach, steal, transfer — are therefore not run by any
// test in this file, only read: the first slice that can build a cell-owned buffer has to assert each of them answers false with a
// pending `TypeError` and leaves `byteLength` alone. What runs here is the other half of each of those functions — a buffer with no
// key, on the heap or external — which must keep exactly the behaviour it had, and which is what a wasm memory's buffer must now
// lose.
//
// mozjs allows one `JSEngine` per process, so `.cargo/config.toml` runs each test on its own.

use std::ffi::{CStr, c_void};
use std::ptr;

use mozjs::context::JSContext;
use mozjs::jsapi::{self as js, JSObject, OnNewGlobalHookOption};
use mozjs::jsval::{ObjectValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{
    evaluate_script, CompileOptionsWrapper, HandleObject, JSEngine, MutableHandleValue, RealmOptions, Runtime,
    SIMPLE_GLOBAL_CLASS,
};
use mozjs::typedarray::Uint8Array;
use mozjs_sys::embed::{current_realm, HandleScope, api};
use mozjs_sys::engine::{cx::Cx, typed, values};

/// The bytes an embedder's block is filled with before the engine is handed it, and which every read of the buffer must find.
const PATTERN: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// A runtime, a global and its realm, as `$cx` (`&mut AutoRealm`) and `$global` (a rooted global).
macro_rules! with_global {
    ($cx:ident, $global:ident, $body:block) => {{
        let engine = JSEngine::init().unwrap();
        let mut runtime = Runtime::new(engine.handle());
        let context = runtime.cx();
        #[allow(unused_unsafe)]
        unsafe {
            rooted!(&in(context) let $global = JS_NewGlobalObject(
                context,
                &SIMPLE_GLOBAL_CLASS,
                ptr::null_mut(),
                OnNewGlobalHookOption::FireOnNewGlobalHook,
                &*RealmOptions::default(),
            ));
            let mut realm = AutoRealm::new_from_handle(context, $global.handle());
            let $cx = &mut realm;
            $body
        }
    }};
}

/// The engine behind `cx`: its `JSContext` as the glue spells it, and its Brimstone context.
macro_rules! engine_of {
    ($cx:expr) => {{
        let raw = unsafe { $cx.raw_cx() };
        // SAFETY: a `JSContext` the glue made is a `Cx` (`engine/cx.rs`), and it lives as long as the runtime does.
        (raw, unsafe { (*raw.cast::<Cx>()).bs })
    }};
}

fn eval(cx: &mut JSContext, global: HandleObject, src: &str, rval: MutableHandleValue) {
    let options = CompileOptionsWrapper::new(cx, c"brimstone_wasm_buffer".to_owned(), 1);
    assert!(evaluate_script(cx, global, src, rval, options).is_ok(), "script failed: {src}");
}

/// Evaluates a script that must produce `true`.
fn check(cx: &mut JSContext, global: HandleObject, src: &str) {
    rooted!(&in(cx) let mut rval = UndefinedValue());
    eval(cx, global, src, rval.handle_mut());
    assert!(rval.is_boolean() && rval.to_boolean(), "not true: {src}");
}

unsafe fn set_global(cx: &mut JSContext, global: HandleObject, name: &CStr, obj: *mut JSObject) {
    rooted!(&in(cx) let v = ObjectValue(obj));
    assert!(js::JS_SetProperty(cx.raw_cx(), global.into(), name.as_ptr(), v.handle().into()));
}

/// A `JS::Handle<JSObject*>` for the object in `slot`. SpiderMonkey's handle is the address of a slot that holds the pointer,
/// so `slot` has to outlive the call: in these tests it is always a local of the test itself.
fn handle_of(slot: &*mut JSObject) -> js::Handle<*mut JSObject> {
    js::Handle { _phantom_0: Default::default(), ptr: slot }
}

/// The embedder's memory for an external buffer: `Vec<u64>`-backed, so aligned as `array_buffer_new_external` requires
/// (`EXTERNAL_ALIGNMENT`), and leaked, because the block must outlive every buffer that names it and the engine frees nothing
/// of a buffer whose owner is `api::ExternalOwner::None`.
fn aligned_block(bytes: &[u8]) -> *mut u8 {
    let mut block = vec![0u64; bytes.len().div_ceil(8)].into_boxed_slice();
    // SAFETY: `bytes.len()` bytes go into a block of at least that many, and no buffer names this memory yet.
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), block.as_mut_ptr().cast::<u8>(), bytes.len()) };
    Box::leak(block).as_mut_ptr().cast::<u8>()
}

/// Reads back `len` bytes of embedder memory, to show the engine neither wrote over them nor freed them.
fn bytes_at(data: *mut u8, len: usize) -> Vec<u8> {
    // SAFETY: `data` is a block of at least `len` bytes that this test allocated and has not freed.
    unsafe { ::std::slice::from_raw_parts(data, len).to_vec() }
}

/// An external buffer of `$len` bytes over the embedder's `block`, as a `WebAssembly.Memory`'s buffer is, with a `Uint8Array`
/// over the whole of it: `(buffer, its object, the view's object)`.
macro_rules! external {
    ($bs:expr, $realm:expr, $block:expr, $len:expr) => {{
        // SAFETY: `block` is `$len` 8-aligned bytes this test owns and has not freed, and `api::ExternalOwner::None` says the
        // engine is never the one to free them; `$realm` is the realm entered for `$bs`.
        let buffer = api::array_buffer_new_external($bs, $realm, $block, $len, api::ExternalOwner::None)
            .expect("an external ArrayBuffer");
        let view = api::create_typed_array($bs, $realm, api::TypedArrayKind::UInt8, buffer, 0, None).expect("a view of it");
        let buffer_object: *mut JSObject = (*buffer.as_object()).as_ptr().cast();
        let view_object: *mut JSObject = (*view).as_ptr().cast();
        (buffer, buffer_object, view_object)
    }};
}

/// A heap buffer of `$len` bytes whose first `PATTERN.len()` are `PATTERN`, with a `Uint8Array` over it: the same three values
/// as `external!`.
macro_rules! heap {
    ($bs:expr, $realm:expr, $len:expr) => {{
        let buffer = api::array_buffer_new($bs, $realm, $len).expect("a heap ArrayBuffer");
        let data = api::array_buffer_data_ptr(buffer).expect("a heap buffer has bytes");
        // SAFETY: `data` is the buffer's own `$len` bytes, and no JS has been given a handle to them yet.
        unsafe { ptr::copy_nonoverlapping(PATTERN.as_ptr(), data, $len.min(PATTERN.len())) };
        let view = api::create_typed_array($bs, $realm, api::TypedArrayKind::UInt8, buffer, 0, None).expect("a view of it");
        let buffer_object: *mut JSObject = (*buffer.as_object()).as_ptr().cast();
        let view_object: *mut JSObject = (*view).as_ptr().cast();
        (buffer, buffer_object, view_object)
    }};
}

#[test]
fn a_heap_buffer_with_no_detach_key_keeps_its_behaviour() {
    with_global!(cx, global, {
        let (raw, bs) = engine_of!(cx);
        let scope = HandleScope::enter(bs);
        let realm = current_realm(bs);
        let (buffer, buffer_object, view_object) = heap!(bs, realm, PATTERN.len());

        // A buffer with no key: what `structuredClone` and `ArrayBuffer.prototype.transfer` ask before taking bytes away.
        let mut defined = true;
        // SAFETY: `raw` is this thread's context and `buffer_object` names a live ArrayBuffer; `defined` is this test's storage.
        assert!(unsafe { typed::JS__HasDefinedArrayBufferDetachKey(raw, handle_of(&buffer_object), &mut defined) });
        assert!(!defined, "a plain buffer reported a detach key");

        let mut shared = true;
        // SAFETY: `view_object` is a live typed array of the buffer made above.
        let data = unsafe { typed::JS_GetArrayBufferViewData(view_object, &mut shared, ptr::null()) };
        assert!(!data.is_null() && !shared);
        // SAFETY: `data` covers the 16 bytes the view extends over.
        assert_eq!(unsafe { bytes_at(data.cast(), PATTERN.len()) }, PATTERN);

        // Detaching succeeds, and the view that named those bytes then names nothing.
        // SAFETY: as above, on the same live buffer.
        assert!(unsafe { typed::JS__DetachArrayBuffer(raw, handle_of(&buffer_object)) });
        assert!(api::array_buffer_is_detached(buffer));
        let mut shared = true;
        // SAFETY: `view_object` is the view of the buffer just detached.
        assert!(unsafe { typed::JS_GetArrayBufferViewData(view_object, &mut shared, ptr::null()) }.is_null());
        // SAFETY: as above.
        assert_eq!(unsafe { typed::JS_GetArrayBufferViewByteLength(view_object) }, 0);
        // Detaching a detached buffer is the no-op SpiderMonkey says it is, not a refusal.
        // SAFETY: as above.
        assert!(unsafe { typed::JS__DetachArrayBuffer(raw, handle_of(&buffer_object)) });

        // Stealing hands over a copy of the bytes and leaves the buffer detached; the caller owns the copy from there.
        let (second, second_object, _) = heap!(bs, realm, PATTERN.len());
        // SAFETY: `second_object` is a live ArrayBuffer holding the bytes written above.
        let stolen = unsafe { typed::JS__StealArrayBufferContents(raw, handle_of(&second_object)) };
        assert!(!stolen.is_null());
        // SAFETY: `stolen` is the 16 bytes the glue `malloc`'d for it.
        assert_eq!(unsafe { bytes_at(stolen.cast(), PATTERN.len()) }, PATTERN);
        assert!(api::array_buffer_is_detached(second));
        // SAFETY: `stolen` came from `malloc`, which `JS_free` matches.
        unsafe { values::JS_free(raw, stolen) };
        // A detached buffer has no contents to steal.
        // SAFETY: `second_object` names the buffer detached above.
        assert!(unsafe { typed::JS__StealArrayBufferContents(raw, handle_of(&second_object)) }.is_null());
        // An attached buffer with no bytes has nothing to copy, but still answers with memory the caller frees.
        let (_empty, empty_object, _) = heap!(bs, realm, 0);
        // SAFETY: `empty_object` names that empty buffer.
        let empty = unsafe { typed::JS__StealArrayBufferContents(raw, handle_of(&empty_object)) };
        assert!(!empty.is_null());
        // SAFETY: `empty` came from `malloc`, which `JS_free` matches.
        unsafe { values::JS_free(raw, empty) };
        scope.exit();
    });
}

#[test]
fn an_external_buffer_with_no_detach_key_keeps_its_behaviour() {
    with_global!(cx, global, {
        let (raw, bs) = engine_of!(cx);
        let scope = HandleScope::enter(bs);
        let realm = current_realm(bs);
        let block = aligned_block(&PATTERN);
        let (buffer, buffer_object, view_object) = external!(bs, realm, block, PATTERN.len());

        // The buffer *is* the block: not a copy of it, and the same answer from the engine and from the glue's view accessor.
        assert!(api::array_buffer_is_external(buffer));
        assert_eq!(api::array_buffer_data_ptr(buffer), Some(block));
        assert_eq!(api::array_buffer_detach_key(buffer), api::DetachKey::None);

        let mut defined = true;
        // SAFETY: `raw` is this thread's context and `buffer_object` names that live buffer.
        assert!(unsafe { typed::JS__HasDefinedArrayBufferDetachKey(raw, handle_of(&buffer_object), &mut defined) });
        assert!(!defined, "an external buffer with no owner reported a detach key");

        let mut shared = true;
        // SAFETY: `view_object` is a live typed array of that buffer.
        let data = unsafe { typed::JS_GetArrayBufferViewData(view_object, &mut shared, ptr::null()) };
        assert_eq!(data.cast::<u8>(), block, "the view does not name the embedder's block");
        // The same bytes from JS.
        // SAFETY: `set_global` roots the buffer's object in the entered realm.
        unsafe { set_global(cx, global.handle(), c"b", buffer_object) };
        check(cx, global.handle(), "b.byteLength === 16 && Array.from(new Uint8Array(b)).join() === '0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15'");

        // Detaching is allowed — the key is none — and it must not touch the memory, which the embedder still owns.
        // SAFETY: `raw` is this thread's context and `buffer_object` names that live buffer.
        assert!(unsafe { typed::JS__DetachArrayBuffer(raw, handle_of(&buffer_object)) });
        assert!(api::array_buffer_is_detached(buffer));
        assert_eq!(bytes_at(block, PATTERN.len()), PATTERN, "detaching wrote over or freed the embedder's block");
        let mut shared = true;
        // SAFETY: `view_object` is the view of that detached buffer.
        assert!(unsafe { typed::JS_GetArrayBufferViewData(view_object, &mut shared, ptr::null()) }.is_null());

        // Stealing a keyless external buffer copies its bytes out, as it does for a heap buffer, and takes nothing from the block.
        let second = aligned_block(&PATTERN);
        let (_, second_object, _) = external!(bs, realm, second, PATTERN.len());
        // SAFETY: `second_object` is a live external ArrayBuffer over the `PATTERN` bytes.
        let stolen = unsafe { typed::JS__StealArrayBufferContents(raw, handle_of(&second_object)) };
        assert!(!stolen.is_null());
        // SAFETY: `stolen` is the 16 bytes the glue `malloc`'d.
        assert_eq!(unsafe { bytes_at(stolen.cast(), PATTERN.len()) }, PATTERN);
        assert_eq!(bytes_at(second, PATTERN.len()), PATTERN, "a steal took the embedder's memory instead of copying it");
        // SAFETY: `stolen` came from `malloc`, which `JS_free` matches.
        unsafe { values::JS_free(raw, stolen) };
        scope.exit();
    });
}

#[test]
fn a_view_of_a_buffer_the_memory_grew_away_reads_as_no_bytes() {
    with_global!(cx, global, {
        let (raw, bs) = engine_of!(cx);
        let scope = HandleScope::enter(bs);
        let realm = current_realm(bs);
        let block = aligned_block(&PATTERN);
        let (buffer, buffer_object, view_object) = external!(bs, realm, block, PATTERN.len());

        let mut shared = true;
        // SAFETY: `view_object` is a live typed array of the buffer made above.
        let stale = unsafe { typed::JS_GetArrayBufferViewData(view_object, &mut shared, ptr::null()) };
        assert_eq!(stale.cast::<u8>(), block);

        // Servo keeps the memory's buffer and its views across a grow, so all four of these must survive the collection
        // around it: rooted, and re-read afterwards, because an engine object moves when the collector copies it.
        // SAFETY: every object named here is live in the entered realm.
        rooted!(&in(cx) let keep_view = ObjectValue(view_object));
        rooted!(&in(cx) let keep_buffer = ObjectValue(buffer_object));

        // The detach that replaces a growed memory's bytes. A memory's own buffer is detached by its owner, whose `detach` ignores
        // the key (design §3.1); this buffer has no key, so the embedder's `array_buffer_detach` is allowed and does the same to a
        // view of it, which is what the host still holds.
        assert!(api::array_buffer_detach(buffer));
        let grown = aligned_block(&[7u8; 32]);
        let (_, grown_object, grown_view) = external!(bs, realm, grown, 32);
        rooted!(&in(cx) let keep_grown_object = ObjectValue(grown_object));
        rooted!(&in(cx) let keep_grown_view = ObjectValue(grown_view));

        // SAFETY: `raw` is this thread's context; a collection runs with the old buffer already detached, as in Servo.
        unsafe { js::JS_GC(raw, js::GCReason::API) };
        let view_object = unsafe { keep_view.get().to_object() };
        let buffer_object = unsafe { keep_buffer.get().to_object() };
        let grown_object = unsafe { keep_grown_object.get().to_object() };
        let grown_view = unsafe { keep_grown_view.get().to_object() };

        // The view the embedder still holds says it has no bytes at all. The pointer taken before the grow is never what the
        // glue hands out afterwards, so nothing can read the superseded block through it.
        let mut shared = true;
        // SAFETY: `view_object` is the rooted view, which the collection may have moved.
        assert!(unsafe { typed::JS_GetArrayBufferViewData(view_object, &mut shared, ptr::null()) }.is_null());
        // SAFETY: as above.
        assert_eq!(unsafe { typed::JS_GetArrayBufferViewByteLength(view_object) }, 0);
        // SAFETY: as above.
        assert_eq!(unsafe { typed::JS_GetArrayBufferViewByteOffset(view_object) }, 0);
        // SAFETY: `buffer_object` is the rooted, now detached buffer.
        assert!(unsafe { typed::JS__IsDetachedArrayBufferObject(buffer_object) });
        // SAFETY: as above.
        assert_eq!(unsafe { typed::JS__GetArrayBufferByteLength(buffer_object) }, 0);
        let mut length = usize::MAX;
        let mut data = usize::MAX as *mut u8;
        let mut shared = true;
        // SAFETY: `buffer_object` is that buffer; the three out-params are this test's own storage.
        unsafe { typed::JS__GetArrayBufferLengthAndData(buffer_object, &mut length, &mut shared, &mut data) };
        assert_eq!(length, 0);
        assert!(data.is_null());

        // The replacement is a live buffer over the new block, which is not the stale pointer.
        assert_ne!(grown, block);
        let mut shared = true;
        // SAFETY: `grown_view` is a live typed array of the replacement buffer.
        assert_eq!(unsafe { typed::JS_GetArrayBufferViewData(grown_view, &mut shared, ptr::null()) }.cast::<u8>(), grown);
        let mut defined = true;
        // SAFETY: `raw` is this thread's context and `grown_object` names that live buffer.
        assert!(unsafe { typed::JS__HasDefinedArrayBufferDetachKey(raw, handle_of(&grown_object), &mut defined) });
        assert!(!defined);
        scope.exit();
    });
}

// ---- The safe wrapper in `mozjs/src/typedarray.rs`, which must never answer from bytes the buffer no longer names. ----

/// A wrapper may not answer a later call from the pointer an earlier one took: between the two the buffer can have been detached —
/// which is what a grow does to the buffer a memory leaves behind, and what a transfer does to the one it takes — and then its bytes
/// are nobody's to read. This is the `TypedArray` API Servo calls, with no `unsafe` in sight.
#[test]
fn a_detached_buffer_leaves_the_safe_wrapper_with_no_slice() {
    with_global!(cx, global, {
        let (raw, bs) = engine_of!(cx);
        let scope = HandleScope::enter(bs);
        let realm = current_realm(bs);
        let block = aligned_block(&PATTERN);
        let (_buffer, buffer_object, view_object) = external!(bs, realm, block, PATTERN.len());
        let array = Uint8Array::from(view_object).expect("a wrapper of the view");
        assert_eq!(array.as_slice_safe(cx), Some(&PATTERN[..]), "the wrapper did not see the bytes to begin with");
        assert_eq!(array.len(), PATTERN.len());

        // That borrow is over here. The detach that follows is the last moment the wrapper may remember anything.
        // SAFETY: `raw` is this thread's context and `buffer_object` names the live buffer the view extends over.
        assert!(unsafe { typed::JS__DetachArrayBuffer(raw, handle_of(&buffer_object)) });
        assert_eq!(array.as_slice_safe(cx), None, "the safe wrapper handed out bytes the detached buffer no longer names");
        assert_eq!(array.len(), 0, "the safe wrapper still counted a detached buffer's length");
        assert_eq!(array.to_vec(), None, "and the owned copy went on answering too");
        assert_eq!(bytes_at(block, PATTERN.len()), PATTERN, "the detached-from read wrote over the embedder's block");
        scope.exit();
    });
}

// ---- The clone transfer of an external buffer, through the host's transfer callbacks. ----

/// The tag and the `extra` word this host gives a transferred buffer.
const HOST_TAG: u32 = 0xFFFF_8002;
const HOST_EXTRA: u64 = 91;
/// The glue's own transfer tag (`engine/clone.rs`'s `TAG_TRANSFER`), which opens the record in the clone.
const GLUE_TRANSFER_TAG: u32 = 19;

/// The bytes and the length a written clone carries for this host's transfer: `TAG_TRANSFER`, the host's tag, the ownership, the
/// host's `extra`, then the byte length and that many bytes. The two calls are the ones Servo uses to move a clone across a thread
/// boundary, so reading them is reading what a receiving host would have to rebuild from.
fn transfer_payload(data: *mut js::JSStructuredCloneData) -> (u64, Vec<u8>) {
    // SAFETY: `data` is this test's own clone buffer; the length is then this vector's, which the copy fills.
    let length = unsafe { mozjs::glue::GetLengthOfJSStructuredCloneData(data) };
    let mut stream = vec![0u8; length];
    // SAFETY: `stream` has `GetLengthOfJSStructuredCloneData` bytes, which is that call's contract.
    unsafe { mozjs::glue::CopyJSStructuredCloneData(data, stream.as_mut_ptr()) };
    let mut start = [0u8; 8];
    start[0..4].copy_from_slice(&GLUE_TRANSFER_TAG.to_le_bytes());
    start[4..8].copy_from_slice(&HOST_TAG.to_le_bytes());
    let at = stream.windows(8).position(|w| w == start).expect("this host's transfer record in the clone");
    let record = &stream[at..];
    let ownership = u32::from_le_bytes(record[8..12].try_into().unwrap());
    let extra = u64::from_le_bytes(record[12..20].try_into().unwrap());
    let serialized = u64::from_le_bytes(record[20..28].try_into().unwrap());
    assert_eq!(ownership, js::JS::TransferableOwnership::SCTAG_TMO_FIRST_OWNED as u32, "the ownership the clone carried");
    assert_eq!(extra, HOST_EXTRA, "the extra word the clone carried");
    let bytes: usize = serialized.try_into().expect("a serialized length that fits");
    assert_eq!(record.len(), 28 + bytes, "the clone ends where the transferred bytes do not");
    (serialized, record[28..].to_vec())
}

unsafe extern "C" fn write_transfer_cb(
    _cx: *mut js::JSContext,
    _obj: js::Handle<*mut JSObject>,
    _closure: *mut c_void,
    tag: *mut u32,
    ownership: *mut js::JS::TransferableOwnership,
    content: *mut *mut c_void,
    extra: *mut u64,
) -> bool {
    // SAFETY: all five are the glue's own writable out-params for this call.
    unsafe {
        *tag = HOST_TAG;
        *ownership = js::JS::TransferableOwnership::SCTAG_TMO_FIRST_OWNED;
        *content = ptr::null_mut();
        *extra = HOST_EXTRA;
        true
    }
}

unsafe extern "C" fn read_transfer_cb(
    cx: *mut js::JSContext,
    _r: *mut js::JSStructuredCloneReader,
    _policy: *const js::JS::CloneDataPolicy,
    tag: u32,
    content: *mut c_void,
    extra: u64,
    closure: *mut c_void,
    return_object: js::JS::MutableHandleObject,
) -> bool {
    assert_eq!(tag, HOST_TAG);
    assert_eq!(extra, HOST_EXTRA);
    assert!(content.is_null(), "a transfer the host owns arrives with no content of the engine's");
    // The bytes the clone carried, which the test read out of the stream before asking for the read: this is the receiving host,
    // rebuilding a buffer of the serialized length out of them.
    let payload = unsafe { &*(closure.cast::<Vec<u8>>()) };
    // SAFETY: `cx` is the reading context and `return_object` is the glue's out-slot for the result.
    unsafe {
        let buffer = typed::JS__NewArrayBuffer(cx, payload.len());
        if buffer.is_null() {
            return false;
        }
        let mut length = 0;
        let mut shared = false;
        let mut data = ptr::null_mut();
        typed::JS__GetArrayBufferLengthAndData(buffer, &mut length, &mut shared, &mut data);
        assert_eq!(length, payload.len(), "the buffer was not built at the serialized length");
        assert!(!shared);
        if data.is_null() {
            return false;
        }
        // SAFETY: `data` covers the `length` bytes of the buffer made above and `payload` holds that many.
        ptr::copy_nonoverlapping(payload.as_ptr(), data, length);
        *return_object.ptr = buffer;
    }
    true
}

unsafe extern "C" fn can_transfer_cb(
    _cx: *mut js::JSContext,
    _obj: js::Handle<*mut JSObject>,
    _same_process: *mut bool,
    _closure: *mut c_void,
) -> bool {
    true
}

static TRANSFER_CALLBACKS: js::JSStructuredCloneCallbacks = js::JSStructuredCloneCallbacks {
    read: None,
    write: None,
    reportError: None,
    readTransfer: Some(read_transfer_cb),
    writeTransfer: Some(write_transfer_cb),
    freeTransfer: None,
    canTransfer: Some(can_transfer_cb),
    sabCloned: None,
};

#[test]
fn transferring_an_external_buffer_copies_and_detaches_it() {
    with_global!(cx, global, {
        let (_raw, bs) = engine_of!(cx);
        let scope = HandleScope::enter(bs);
        let realm = current_realm(bs);
        let block = aligned_block(&PATTERN);
        let (buffer, buffer_object, _view) = external!(bs, realm, block, PATTERN.len());
        // SAFETY: the scripts only name the rooted buffer, and the clone runs with its realm entered.
        unsafe {
            set_global(cx, global.handle(), c"b", buffer_object);
            rooted!(&in(cx) let mut subject = UndefinedValue());
            eval(cx, global.handle(), "b", subject.handle_mut());
            rooted!(&in(cx) let mut transfer = UndefinedValue());
            eval(cx, global.handle(), "[b]", transfer.handle_mut());
            let clone_scope = js::JS::StructuredCloneScope::SameProcess;
            let buf = mozjs::glue::NewJSAutoStructuredCloneBuffer(clone_scope, &TRANSFER_CALLBACKS);
            let data = &mut (*buf).data_;
            let version = (*buf).version_;
            assert!(js::JS_WriteStructuredClone(
                cx.raw_cx(),
                subject.handle().into(),
                data,
                clone_scope,
                ptr::null(),
                &TRANSFER_CALLBACKS,
                ptr::null_mut(),
                transfer.handle().into(),
            ));
            let mut has = false;
            assert!(js::JS_StructuredCloneHasTransferables(data, &mut has));
            assert!(has, "the transfer was not reported");
            // The bytes the source named, and the length the clone carries with them: the copy out of the embedder's block is what
            // a receiving host rebuilds from, and this is the only place that copy is visible.
            let (serialized, payload) = transfer_payload(data);
            assert_eq!(usize::try_from(serialized), Ok(PATTERN.len()), "the clone carried a different length");
            assert_eq!(payload, PATTERN, "the clone did not carry the embedder's bytes");
            // The source is detached, and the block it named — which was only ever on loan — is untouched and still there.
            assert!(api::array_buffer_is_detached(buffer));
            assert_eq!(bytes_at(block, PATTERN.len()), PATTERN, "the transfer moved or freed the embedder's block");
            rooted!(&in(cx) let mut out = UndefinedValue());
            assert!(js::JS_ReadStructuredClone(
                cx.raw_cx(),
                data,
                version,
                clone_scope,
                out.handle_mut().into(),
                ptr::null(),
                &TRANSFER_CALLBACKS,
                &payload as *const Vec<u8> as *mut c_void,
            ));
            set_global(cx, global.handle(), c"__c", out.to_object());
            let bytes = PATTERN.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(",");
            check(cx, global.handle(), "__c instanceof ArrayBuffer");
            check(cx, global.handle(), &format!("__c.byteLength === {}", PATTERN.len()));
            check(cx, global.handle(), &format!("Array.from(new Uint8Array(__c)).join() === '{bytes}'"));
            mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf);
        }
        scope.exit();
    });
}
