/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Raw-API tests for `glue::NewExternalArrayBuffer` (`mozjs-sys/src/engine/external_buffer.rs`): an ArrayBuffer over memory the
// caller owns, and the free callback that hands that memory back. Slice H2 of the WebAssembly design (§5.3); the page chain it
// unblocks is `GPUBuffer.getMappedRange` → `GPUDataBlock::view` (`servo/components/script_webgpu/datablock.rs:98`).
//
// What they pin is the engine's `ExternalOwner` contract (Brimstone `runtime/embed/api.rs:301-333`) read through the five-argument
// C ABI, and the parts of it only the glue can get wrong:
//
// * the buffer *is* the caller's memory — a write on one side is what the other side reads, with no copy in between;
// * `freeFunc(contents, freeUserData)` runs EXACTLY ONCE: never while the buffer is attached, once after the collection that frees
//   it, once for a buffer still attached when the context is destroyed, and never a second time at teardown after a collection has
//   already paid. Two buffers over one block share one `freeUserData` and each pay once, the first without taking the bytes the
//   second still names;
// * a refused call — null `contents`, misaligned `contents`, or a length over the engine's maximum — answers null with a pending
//   exception and does NOT run the callback: the caller keeps its memory, and the tests below give it back by hand, which a second
//   call would corrupt;
// * a null `freeFunc` means no callback at all: the caller owns the bytes until it detaches the buffer itself;
// * a zero-length buffer is accepted, stores the engine's own pointer rather than the caller's, and — callback or none — pays the
//   callback once and only once when it is a `Free` buffer;
// * a transfer copies the bytes into a heap buffer and detaches the original without touching the memory or ending the loan early.
//
// mozjs allows one `JSEngine` per process, so `.cargo/config.toml` runs the tests one at a time. Every test here ends by dropping
// its runtime, because that teardown is one of the two moments the engine may run a free callback.

use std::cell::Cell;
use std::ffi::{CStr, c_void};
use std::ptr;

use mozjs::context::JSContext;
use mozjs::jsapi::{self as jsapi, JSObject, OnNewGlobalHookOption};
use mozjs::jsval::{ObjectValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{
    CompileOptionsWrapper, HandleObject, JSEngine, MutableHandleValue, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS,
    evaluate_script,
};

/// The bytes a block is filled with before the engine is handed it, and which every read of the buffer must find.
const PATTERN: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// One caller-owned block and the record its free callback updates. This is Servo's shape: `DataBlock::view` leaks an `Arc` and
/// passes it as `freeUserData`, so what the callback reaches outlives the callback — here the `Block` record stays leaked, which is
/// what lets a test read the count after the teardown that ran the callback reclaimed the block.
struct Block {
    /// The block: `words` 8-byte words, leaked. The address the engine is allowed to name.
    data: *mut u8,
    words: usize,
    /// How many times the engine has called `free_func` for this block.
    frees: Cell<usize>,
    /// The `contents` the last call brought in, as the caller's callback saw it.
    contents: Cell<*mut c_void>,
}

/// The `BufferContentsFreeFunc` a caller registers. It runs inside a collection or at context teardown, so it reaches no engine
/// state: one counter update and the reclaim of the leaked block, nothing else.
unsafe extern "C" fn free_func(contents: *mut c_void, user: *mut c_void) {
    // SAFETY: `user` is the leaked `Block` this file made for exactly these bytes; one buffer's callback runs at most once, so
    // nothing else reads the record while this runs.
    let block = unsafe { &*(user.cast::<Block>()) };
    block.frees.set(block.frees.get() + 1);
    block.contents.set(contents);
    // SAFETY: `contents` is that block's `data`, from `Box::leak` of a boxed slice of `block.words` words, and this is its only
    // reclaim: a second call would be a double free, which the allocator stops here.
    drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(contents.cast::<u64>(), block.words)) });
}

/// A callback that only counts, for the case where one block backs two buffers: a caller that keeps those bytes behind a
/// refcount (Servo's `Arc`) returns them when the LAST owner goes, not in each callback, so here the words stay with the
/// harness and only the count moves.
unsafe extern "C" fn counting_free_func(_contents: *mut c_void, user: *mut c_void) {
    // SAFETY: `user` is the leaked `Block` this file made for these bytes; the record outlives every callback.
    let block = unsafe { &*(user.cast::<Block>()) };
    block.frees.set(block.frees.get() + 1);
}

/// A leaked block of `len` bytes — a whole number of words, so its address is 8-aligned as the engine requires — and its record.
fn new_block(len: usize) -> *mut Block {
    assert_eq!(len % 8, 0, "a block is leaked as words, so its length must be a whole number of them");
    // SAFETY: the bytes written are this block's own, and no buffer names them yet.
    let data = Box::leak(vec![0u64; len / 8].into_boxed_slice()).as_mut_ptr().cast::<u8>();
    unsafe { ptr::copy_nonoverlapping(PATTERN.as_ptr(), data, len.min(PATTERN.len())) };
    Box::into_raw(Box::new(Block { data, words: len / 8, frees: Cell::new(0), contents: Cell::new(ptr::null_mut()) }))
}

/// The `contents` pointer this block's free callback was called with, or null if it has not run.
fn received(block: *const Block) -> *mut c_void {
    // SAFETY: a `Block` this file made with `new_block` and has not given back.
    unsafe { (*block).contents.get() }
}

/// How many times the engine has run this block's free callback. The record is leaked, so a test may read it whenever it is not
/// itself inside an engine call.
fn frees(block: *const Block) -> usize {
    // SAFETY: a `Block` this file made with `new_block` and has not given back.
    unsafe { (*block).frees.get() }
}

/// The block's bytes, read as its owner reads them. Only legal while no engine code runs: the contract lends the same memory to the
/// buffer, and it is the caller that keeps the two uses apart.
fn bytes_of(block: *const Block, len: usize) -> Vec<u8> {
    // SAFETY: `data` is a leaked block of at least `len` bytes this file owns and the engine has not freed (its callback has not
    // run), and no JS is running that could have moved or written anything the contract keeps to itself.
    unsafe { ::std::slice::from_raw_parts((*block).data, len).to_vec() }
}

/// Gives a block back when the engine refused the buffer, or when the caller owns the memory: the reclaim a free callback would
/// otherwise have made. Only a test that knows the callback never ran may call this — if it ran, the words are already gone.
unsafe fn reclaim(block: *mut Block) {
    // SAFETY: the caller's precondition is that no free callback ran for this block.
    unsafe {
        let words = (*block).words;
        let data = (*block).data;
        drop(Box::from_raw(ptr::slice_from_raw_parts_mut(data.cast::<u64>(), words)));
        drop(Box::from_raw(block));
    }
}

/// A runtime, a global and its realm as `$cx` (`&mut AutoRealm`) and `$global` (a rooted global) — and, unlike the harness the other
/// test files use, the runtime is dropped before the macro answers: context teardown is one of the two moments a free callback may
/// run, so every test here passes through it. The body's value comes back out.
macro_rules! with_runtime {
    ($cx:ident, $global:ident, $body:block) => {{
        let engine = JSEngine::init().unwrap();
        let mut runtime = Runtime::new(engine.handle());
        let context = runtime.cx();
        let out = {
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
        };
        drop(runtime);
        drop(engine);
        out
    }};
}

fn eval(cx: &mut JSContext, global: HandleObject, src: &str, rval: MutableHandleValue) {
    let options = CompileOptionsWrapper::new(cx, c"brimstone_external_buffer".to_owned(), 1);
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
    assert!(jsapi::JS_SetProperty(cx.raw_cx(), global.into(), name.as_ptr(), v.handle().into()));
}

/// Takes the pending exception, checks it is an instance of `class` (a global constructor's name) and clears it.
unsafe fn take_exception(cx: &mut JSContext, global: HandleObject, class: &str) {
    assert!(jsapi::JS_IsExceptionPending(cx.raw_cx()), "no exception pending, expected a {class}");
    rooted!(&in(cx) let mut e = UndefinedValue());
    assert!(jsapi::JS_GetPendingException(cx.raw_cx(), e.handle_mut().into()));
    jsapi::JS_ClearPendingException(cx.raw_cx());
    assert!(e.is_object(), "the pending exception is not an object");
    unsafe { set_global(cx, global, c"__e", e.to_object()) };
    check(cx, global, &format!("__e instanceof {class}"));
}

/// `PATTERN` as the comma-separated text `Array.from(new Uint8Array(b)).join()` answers with.
fn csv(bytes: &[u8]) -> String {
    bytes.iter().map(u8::to_string).collect::<Vec<_>>().join(",")
}

/// The length and the data pointer the glue reports for a buffer object — re-read after anything that could have moved it.
fn length_and_data(obj: *mut JSObject) -> (usize, *mut u8) {
    let mut length = 0;
    let mut shared = true;
    let mut data = ptr::null_mut();
    // SAFETY: `obj` is the caller's live buffer; the three out-params are this function's own storage.
    unsafe { jsapi::GetArrayBufferLengthAndData(obj, &mut length, &mut shared, &mut data) };
    assert!(!shared, "an external buffer reported shared memory");
    (length, data)
}

/// `glue::NewExternalArrayBuffer` with the caller's block and its callback, as Servo calls it.
unsafe fn external(
    cx: &mut JSContext,
    block: *mut Block,
    len: usize,
    contents: *mut c_void,
    free: jsapi::BufferContentsFreeFunc,
) -> *mut JSObject {
    // SAFETY: the caller's — every test hands this a block it leaked and has not freed.
    unsafe { jsapi::NewExternalArrayBuffer(cx.raw_cx(), len, contents, free, block.cast()) }
}

// ---- The bytes are the caller's: shared, not copied. ----

#[test]
fn the_callers_bytes_are_the_buffers_both_ways() {
    let block = with_runtime!(cx, global, {
        let len = PATTERN.len();
        let block = new_block(len);
        let raw = unsafe { external(cx, block, len, (*block).data.cast(), Some(free_func)) };
        assert!(!raw.is_null(), "a caller-owned block was refused");
        rooted!(&in(cx) let keep = raw);

        // The engine names the caller's own address, not a copy of it.
        assert_eq!(unsafe { length_and_data(keep.get()) }, (len, (*block).data));

        // Written in Rust, read in script.
        unsafe { set_global(cx, global.handle(), c"b", keep.get()) };
        check(
            cx,
            global.handle(),
            &format!("b.byteLength === {len} && Array.from(new Uint8Array(b)).join() === '{}'", csv(&PATTERN)),
        );

        // Written in script, read in Rust: the same bytes, with no copy of them anywhere in between.
        check(cx, global.handle(), "var v = new Uint8Array(b); v[0] = 200; v[15] = 99; v[8] = 1; v[0] === 200");
        let mut written = bytes_of(block, len);
        assert_eq!((written[0], written[15], written[8]), (200, 99, 1), "a script write did not reach the caller's block");

        // And a write through the caller's own slice is what the script reads back.
        written[0] = 7;
        // SAFETY: no JS is running, and the block is this test's for as long as its callback has not run.
        unsafe { ptr::copy_nonoverlapping(written.as_ptr(), (*block).data, len) };
        check(cx, global.handle(), &format!("Array.from(new Uint8Array(b)).join() === '{}'", csv(&written)));

        assert_eq!(frees(block), 0, "the callback ran while the buffer was alive");
        block
    });
    assert_eq!(frees(block), 1, "a buffer still attached at teardown did not hand its memory back exactly once");
}

// ---- The free callback, once. ----

#[test]
fn the_free_callback_runs_once_when_the_buffer_is_collected_and_never_again() {
    let block = with_runtime!(cx, global, {
        let len = PATTERN.len();
        let block = new_block(len);
        let raw = unsafe { external(cx, block, len, (*block).data.cast(), Some(free_func)) };
        assert!(!raw.is_null());

        {
            // The only root the buffer has, so leaving this block is what makes it collectable.
            rooted!(&in(cx) let keep = raw);
            // Alive across a collection: the callback is not a weak reference, it is a payment the buffer still owes.
            unsafe { jsapi::JS_GC(cx.raw_cx(), jsapi::GCReason::API) };
            assert_eq!(frees(block), 0, "freed while the buffer is rooted and alive");
            // Re-read through the root: a collector may have moved the object, its bytes stay where the caller put them.
            assert_eq!(
                unsafe { length_and_data(keep.get()) },
                (len, (*block).data),
                "a collection moved the buffer off the caller's block"
            );
        }

        // Unrooted: the next collection takes the buffer, and the owner cell that carries the callback with it. From here the
        // block is gone, so the test only ever reads its record.
        unsafe { jsapi::JS_GC(cx.raw_cx(), jsapi::GCReason::API) };
        assert_eq!(frees(block), 1, "the collection that freed the buffer did not pay for it");

        // A second collection, and the teardown after it: still one call. If the engine had paid twice, the reclaim inside the
        // callback was a double free and this process would not have reached this line.
        unsafe { jsapi::JS_GC(cx.raw_cx(), jsapi::GCReason::API) };
        assert_eq!(frees(block), 1, "a second collection ran the callback again");
        block
    });
    assert_eq!(frees(block), 1, "context teardown ran the callback of a buffer whose memory was already paid for");
}

/// Servo's `DataBlock::clear_views` (`datablock.rs:56-62`) detaches every view's buffer and `GPUBuffer::unmap` calls
/// `consume` right after it (`gpubuffer.rs:261-262`), whose `expect("No view should be alive")` (`datablock.rs:123`) holds
/// only if the detach has already given the block back: Servo is written for SpiderMonkey, which runs the free callback
/// *at* the detach. This engine does not — a detach drops the buffer→owner edge and the payment lands at the next
/// collection — so what follows pins the engine's current timing, not a contract: moving to SpiderMonkey's would invert
/// these two counts on purpose. What has to hold under either timing is that the payment is neither lost nor doubled.
#[test]
fn a_detached_buffer_pays_at_the_next_collection() {
    let block = with_runtime!(cx, global, {
        let len = PATTERN.len();
        let block = new_block(len);
        let raw = unsafe { external(cx, block, len, (*block).data.cast(), Some(free_func)) };
        assert!(!raw.is_null());
        rooted!(&in(cx) let keep = raw);

        assert!(unsafe { jsapi::DetachArrayBuffer(cx.raw_cx(), keep.handle().into()) });
        assert!(unsafe { jsapi::IsDetachedArrayBufferObject(keep.get()) });
        // The engine's current timing: SpiderMonkey would have paid by now, at the detach itself.
        assert_eq!(frees(block), 0, "the detach itself ran the free callback");
        assert_eq!(bytes_of(block, len), PATTERN, "detaching wrote over or gave back the caller's block");

        unsafe { jsapi::JS_GC(cx.raw_cx(), jsapi::GCReason::API) };
        assert_eq!(frees(block), 1, "a detached buffer's memory came back only at teardown");
        block
    });
    assert_eq!(frees(block), 1, "the teardown after a detach ran the callback a second time");
}

/// One callback per buffer, in a map the engine keys by owner cell: two live buffers must not pay for each other.
#[test]
fn two_buffers_of_one_context_each_pay_once() {
    let (a, b) = with_runtime!(cx, global, {
        let len = PATTERN.len();
        let a = new_block(len);
        let b = new_block(len);
        let raw_a = unsafe { external(cx, a, len, (*a).data.cast(), Some(free_func)) };
        let raw_b = unsafe { external(cx, b, len, (*b).data.cast(), Some(free_func)) };
        assert!(!raw_a.is_null() && !raw_b.is_null());
        // A root for `b` that outlives the inner block, so only `a` is collectable there (roots of one kind leave in the order
        // they arrived).
        rooted!(&in(cx) let keep_b = raw_b);
        {
            rooted!(&in(cx) let _keep_a = raw_a);
            unsafe { jsapi::JS_GC(cx.raw_cx(), jsapi::GCReason::API) };
            assert_eq!((frees(a), frees(b)), (0, 0), "a live buffer paid");
        }
        unsafe { jsapi::JS_GC(cx.raw_cx(), jsapi::GCReason::API) };
        assert_eq!((frees(a), frees(b)), (1, 0), "the collected buffer did not pay, or the live one paid for it");
        (a, b)
    });
    assert_eq!((frees(a), frees(b)), (1, 1), "teardown did not pay the surviving buffer, or paid the other one twice");
}

/// One mapped WebGPU block, two views of it: `GPUDataBlock::view` hands the SAME `freeUserData` — one `Arc` of the shared
/// memory — to a buffer per range, and the ranges never overlap (`datablock.rs:70-76`). Each buffer owes exactly one payment
/// of its own, and the first must not take the bytes the second still names.
#[test]
fn two_buffers_over_one_block_pay_once_each() {
    let block = with_runtime!(cx, global, {
        let len = PATTERN.len();
        let half = len / 2;
        let block = new_block(len);
        let raw_low = unsafe { external(cx, block, half, (*block).data.cast(), Some(counting_free_func)) };
        let raw_high = unsafe { external(cx, block, half, (*block).data.add(half).cast(), Some(counting_free_func)) };
        assert!(!raw_low.is_null() && !raw_high.is_null(), "a second buffer over another part of one block was refused");
        // Each names its own half of the caller's allocation, not a copy and not each other's range.
        assert_eq!(unsafe { length_and_data(raw_low) }, (half, (*block).data));
        assert_eq!(unsafe { length_and_data(raw_high) }, (half, (*block).data.add(half)));

        rooted!(&in(cx) let keep_high = raw_high);
        {
            rooted!(&in(cx) let _keep_low = raw_low);
            unsafe { set_global(cx, global.handle(), c"lo", _keep_low.get()) };
            unsafe { set_global(cx, global.handle(), c"hi", keep_high.get()) };
            check(
                cx,
                global.handle(),
                &format!(
                    "Array.from(new Uint8Array(lo)).join() === '{}' && new Uint8Array(hi).length === {half}",
                    csv(&PATTERN[..half])
                ),
            );
            // A write to the second buffer's last byte is a write to the caller's last byte.
            check(cx, global.handle(), "(new Uint8Array(hi)[7] = 250) === 250");
            assert_eq!(bytes_of(block, len)[len - 1], 250, "the second buffer did not share the caller's bytes");
            assert_eq!(frees(block), 0, "a callback ran while both buffers were alive");
            check(cx, global.handle(), "(lo = null) === null");
        }

        unsafe { jsapi::JS_GC(cx.raw_cx(), jsapi::GCReason::API) };
        assert_eq!(frees(block), 1, "the buffer that died did not pay once, or the live one paid for it");

        // The survivor is untouched by that payment: still its half of the same block, contents and all.
        let mut high = PATTERN[half..].to_vec();
        high[half - 1] = 250;
        assert_eq!(bytes_of(block, len)[half..], high, "the first payment gave back bytes the other buffer still names");
        check(cx, global.handle(), &format!("Array.from(new Uint8Array(hi)).join() === '{}'", csv(&high)));
        unsafe { jsapi::JS_GC(cx.raw_cx(), jsapi::GCReason::API) };
        assert_eq!(frees(block), 1, "a collection paid the live buffer's callback early");
        block
    });
    assert_eq!(frees(block), 2, "teardown did not pay the second buffer once, or paid the first one twice");
    // Both callbacks only counted, so the block is still here and this test gives it back itself.
    unsafe { reclaim(block) };
}

// ---- A transfer copies: it must not end the loan early. ----

/// `ArrayBuffer.prototype.transfer` on a buffer whose memory the engine was going to free: the spec's CloneArrayBuffer-then-detach,
/// so the copy is a heap buffer of the caller's bytes and the source ends detached. Where the free callback goes then is the
/// engine's timing again — SpiderMonkey's detach runs it at once, this engine pays at the next collection — so the count
/// below pins today's timing, not a contract, and would invert with SpiderMonkey's. That the transfer copies rather than
/// moves is what has to hold either way.
#[test]
fn a_transfer_copies_the_bytes_and_does_not_free_the_original_early() {
    // One script transfers and reads the copy back, so nothing but the transfer can collect in between: the count is still zero
    // when the test looks, and the caller's block is still the caller's.
    let block = with_runtime!(cx, global, {
        let len = PATTERN.len();
        let block = new_block(len);
        let raw = unsafe { external(cx, block, len, (*block).data.cast(), Some(free_func)) };
        assert!(!raw.is_null());
        rooted!(&in(cx) let keep = raw);
        unsafe { set_global(cx, global.handle(), c"b", keep.get()) };
        check(
            cx,
            global.handle(),
            &format!(
                "var t = b.transfer(); t.byteLength === {len} && b.detached === true && \
                 Array.from(new Uint8Array(t)).join() === '{}'",
                csv(&PATTERN)
            ),
        );
        // The engine's current timing, as above: under SpiderMonkey's, the detach this transfer performed has already paid.
        assert_eq!(frees(block), 0, "a transfer ran the free callback of the buffer it detached from");
        assert_eq!(bytes_of(block, len), PATTERN, "a transfer took the caller's memory instead of copying it");
        block
    });
    // The buffer the test left attached-to-nothing pays at the teardown it never got to a collection for, once.
    assert_eq!(frees(block), 1, "a transferred buffer did not pay for its memory exactly once");
}

// ---- Who owns the memory when there is no callback. ----

/// SpiderMonkey's rule, which `ExternalOwner::None` is: with a null `freeFunc` the caller keeps the bytes until it detaches the
/// buffer itself, and the engine frees nothing — not at a collection, not at teardown.
#[test]
fn a_null_free_callback_leaves_the_memory_with_the_caller() {
    with_runtime!(cx, global, {
        let len = PATTERN.len();
        let block = new_block(len);
        let raw = unsafe { external(cx, block, len, (*block).data.cast(), None) };
        assert!(!raw.is_null(), "a caller-owned buffer without a callback was refused");
        rooted!(&in(cx) let keep = raw);
        unsafe { set_global(cx, global.handle(), c"b", keep.get()) };
        check(cx, global.handle(), &format!("b.byteLength === {len} && new Uint8Array(b).join() === '{}'", csv(&PATTERN)));

        unsafe { jsapi::JS_GC(cx.raw_cx(), jsapi::GCReason::API) };
        assert_eq!(bytes_of(block, len), PATTERN, "a collection took memory that had no callback behind it");

        // Detach, collect, and only then take the block back: the order the API asks the caller for. `reclaim` is what a second
        // engine-side free would corrupt, so the fact that it runs at all is the assertion.
        assert!(unsafe { jsapi::DetachArrayBuffer(cx.raw_cx(), keep.handle().into()) });
        unsafe { jsapi::JS_GC(cx.raw_cx(), jsapi::GCReason::API) };
        assert_eq!(bytes_of(block, len), PATTERN, "the detach of a caller-owned buffer freed what no one gave it");
        unsafe { reclaim(block) };
    });
}

/// `JS::NewArrayBufferWithUserOwnedContents` is the same object with the same rule and no callback in its signature at all
/// (`mozjs/examples/wasm.rs:98` builds a `WebAssembly.Module` over bytes of that kind).
#[test]
fn user_owned_contents_are_an_external_buffer_the_caller_keeps() {
    with_runtime!(cx, global, {
        let len = PATTERN.len();
        let block = new_block(len);
        let raw = unsafe { jsapi::NewArrayBufferWithUserOwnedContents(cx.raw_cx(), len, (*block).data.cast()) };
        assert!(!raw.is_null(), "user-owned contents were refused");
        rooted!(&in(cx) let keep = raw);
        assert_eq!(unsafe { length_and_data(keep.get()) }, (len, (*block).data), "the buffer copied the caller's bytes");
        unsafe { set_global(cx, global.handle(), c"b", keep.get()) };
        check(cx, global.handle(), &format!("new Uint8Array(b).join() === '{}'", csv(&PATTERN)));
        check(cx, global.handle(), "new Uint8Array(b)[1] = 5; new Uint8Array(b)[1] === 5");
        assert_eq!(bytes_of(block, len)[1], 5, "a script write did not reach the user-owned block");
        unsafe { jsapi::JS_GC(cx.raw_cx(), jsapi::GCReason::API) };
        assert_eq!(bytes_of(block, len)[1], 5, "a collection took the user's memory");
        assert!(unsafe { jsapi::DetachArrayBuffer(cx.raw_cx(), keep.handle().into()) });
        unsafe { reclaim(block) };
    });
}

// ---- What the glue refuses, and what it must not do when it does. ----

#[test]
fn null_contents_are_refused_without_running_the_callback() {
    with_runtime!(cx, global, {
        let len = PATTERN.len();
        let block = new_block(len);
        // Null contents with a length: the engine's TypeError, SpiderMonkey's null return and a pending exception — and the closure
        // dropped uncalled, so the block is still the caller's.
        let raw = unsafe { external(cx, block, len, ptr::null_mut(), Some(free_func)) };
        assert!(raw.is_null(), "a null contents pointer made a buffer");
        assert_eq!(frees(block), 0, "a refused call ran the free callback");
        unsafe { take_exception(cx, global.handle(), "TypeError") };
        // The context still works: the refusal is an exception, not a broken state.
        check(cx, global.handle(), "1 + 1 === 2");
        unsafe { reclaim(block) };
    });
}

#[test]
fn misaligned_contents_are_refused_without_running_the_callback() {
    with_runtime!(cx, global, {
        let len = PATTERN.len();
        let block = new_block(len);
        let misaligned = unsafe { (*block).data.add(1) };
        let raw = unsafe { external(cx, block, len, misaligned.cast(), Some(free_func)) };
        assert!(raw.is_null(), "misaligned contents made a buffer");
        assert_eq!(frees(block), 0, "a refused call ran the free callback");
        unsafe { take_exception(cx, global.handle(), "TypeError") };
        // What a caller must do after a refusal: the memory is still its own to give back.
        unsafe { reclaim(block) };
    });
}

/// The refusal by length, which the two above do not reach: the engine tests `byte_length > MAX_ARRAY_BUFFER_SIZE`
/// (`1 << 32`, `array_buffer_object.rs:167`) before it looks at the pointer, and it tests it *after* the `Free` owner cell
/// exists — so this is the branch where a live closure is dropped uncalled (`forget_external_free`).
#[test]
fn an_over_long_buffer_is_refused_without_running_the_callback() {
    with_runtime!(cx, global, {
        let len = PATTERN.len();
        let block = new_block(len);
        // The caller offers its own 16 aligned bytes and claims more of them than the engine allows; nothing beyond the
        // block is ever named, because the length is refused first.
        let raw = unsafe { external(cx, block, (1usize << 32) + len, (*block).data.cast(), Some(free_func)) };
        assert!(raw.is_null(), "a buffer longer than the engine's maximum was made");
        assert_eq!(frees(block), 0, "a refusal by length ran the free callback, which would have freed a 16-byte block as 4 GiB");
        unsafe { take_exception(cx, global.handle(), "RangeError") };
        check(cx, global.handle(), "1 + 1 === 2");
        // The dropped closure left the memory where it was: with the caller.
        assert_eq!(bytes_of(block, len), PATTERN, "the refused call touched the caller's block");
        unsafe { reclaim(block) };
    });
}

/// The engine's own rule (`array_buffer_new_external`): a zero-length buffer never touches its pointer, so null contents are
/// accepted, and the empty buffer is a real ArrayBuffer an embedder may hand to a page.
#[test]
fn a_zero_length_buffer_needs_no_contents() {
    with_runtime!(cx, global, {
        let block = new_block(8);
        let raw = unsafe { external(cx, block, 0, ptr::null_mut(), None) };
        assert!(!raw.is_null(), "an empty external buffer was refused");
        rooted!(&in(cx) let keep = raw);
        unsafe { set_global(cx, global.handle(), c"b", keep.get()) };
        check(cx, global.handle(), "b instanceof ArrayBuffer && b.byteLength === 0 && new Uint8Array(b).length === 0");
        assert_eq!(unsafe { length_and_data(keep.get()) }.0, 0);
        unsafe { reclaim(block) };
    });
}

/// The other half of that rule, with a callback attached: a zero-length buffer takes no bytes, so the engine stores an
/// aligned dangling pointer of its own — and the pointer that comes back to the caller when it dies is still the one the
/// caller handed the glue, paid exactly once and not again at teardown.
#[test]
fn a_zero_length_buffer_with_a_callback_pays_once() {
    let block = with_runtime!(cx, global, {
        let len = PATTERN.len();
        let block = new_block(len);
        let raw = unsafe { external(cx, block, 0, (*block).data.cast(), Some(free_func)) };
        assert!(!raw.is_null(), "a zero-length buffer with a free callback was refused");
        {
            rooted!(&in(cx) let keep = raw);
            unsafe { set_global(cx, global.handle(), c"b", keep.get()) };
            check(cx, global.handle(), "b instanceof ArrayBuffer && b.byteLength === 0 && new Uint8Array(b).length === 0");
            assert_eq!(frees(block), 0, "a live empty buffer paid");
            // The engine's own pointer for an empty external buffer, not the caller's: the callback is what has to remember
            // whose memory this was.
            assert_ne!(unsafe { length_and_data(keep.get()) }.1, (*block).data, "an empty buffer kept the caller's address");
            check(cx, global.handle(), "(b = null) === null");
        }
        unsafe { jsapi::JS_GC(cx.raw_cx(), jsapi::GCReason::API) };
        assert_eq!(frees(block), 1, "the collection that took the empty buffer did not pay for it");
        assert_eq!(received(block), (*block).data.cast(), "the callback was handed a pointer other than the caller's own");
        // The reclaim inside the callback gave the block back at that moment, so nothing may read its bytes from here.
        unsafe { jsapi::JS_GC(cx.raw_cx(), jsapi::GCReason::API) };
        assert_eq!(frees(block), 1, "a second collection ran the empty buffer's callback again");
        block
    });
    assert_eq!(frees(block), 1, "teardown paid the empty buffer a second time");
}
