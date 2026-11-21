//! Tests for external ArrayBuffers (WebAssembly design §3.1-3.3; §3.5 items 4-7 and the callout
//! rows of item 10), with a fake owner standing in for a wasm linear memory.
//!
//! The fake memory is a list of blocks. `grow(n)` from JS moves it the way wasmi's `Vec` moves on
//! a grow: a new, larger block gets a copy of the bytes, the old block is retired, and the current
//! buffer is detached and replaced (what `sync_buffers` and `Memory.prototype.grow` do, §3.2). The
//! retired block is not freed but filled with 0xEE and kept, so a view that still read or wrote it
//! would be seen by the tests instead of being undefined behavior.
//!
//! With the `gc_stress_test` feature every test runs with a GC on every allocation.

use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    rc::Rc,
};

use crate::{
    common::options::OptionsBuilder,
    handle_scope_guard,
    runtime::{
        Context, ContextBuilder, EvalResult, Handle, HeapPtr, PropertyDescriptor, PropertyFlags,
        PropertyKey, Value,
        abstract_operations::define_property_or_throw,
        embed::{self, EngineCellHooks, GcHookCx, Host, NativeCall, RootVisitor, api},
        intrinsics::array_buffer_object::{ArrayBufferObject, DetachKey},
        object_value::ObjectValue,
        type_utilities::to_string,
    },
};

/// Bytes added by `grow(1)`.
const PAGE: usize = 32;
/// The byte retired blocks are filled with.
const POISON: u8 = 0xee;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// `ExternalOwner::None`: no detach key.
    Embedder,
    /// `ExternalOwner::Cell`: the `WasmMemory` key, the buffer holds the cell.
    Cell,
}

/// A block of the fake memory: `len` bytes at an 8-aligned address inside `storage`, as
/// `array_buffer_new_external` requires (a boxed `[u8]` is only byte-aligned).
struct Block {
    storage: Box<[u8]>,
    offset: usize,
    len: usize,
}

impl Block {
    fn new(bytes: &[u8]) -> Block {
        let mut storage = vec![0u8; bytes.len() + 7].into_boxed_slice();
        let address = storage.as_ptr() as usize;
        let offset = (8 - address % 8) % 8;
        storage[offset..offset + bytes.len()].copy_from_slice(bytes);
        Block { storage, offset, len: bytes.len() }
    }
}

impl std::ops::Deref for Block {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.storage[self.offset..self.offset + self.len]
    }
}

impl std::ops::DerefMut for Block {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.storage[self.offset..self.offset + self.len]
    }
}

thread_local! {
    /// Every block of the fake memory, the current one last. Retired blocks stay allocated.
    static BLOCKS: RefCell<Vec<Block>> = const { RefCell::new(Vec::new()) };
    /// The current buffer, as raw value bits: a host root (the memory's record).
    static CURRENT: Cell<u64> = const { Cell::new(0) };
    static OWNER: Cell<Option<Owner>> = const { Cell::new(None) };
    /// Negative control only: `grow` leaves the old buffer attached (the bug a grow that detached
    /// the wrong object would have), so the tests can be shown to catch it.
    static SKIP_DETACH: Cell<bool> = const { Cell::new(false) };
    /// Finalized owner cells.
    static CELLS_FINALIZED: Cell<usize> = const { Cell::new(0) };
}

fn finalize_owner_cell(_: GcHookCx<'_>, _: HeapPtr<ObjectValue>) {
    CELLS_FINALIZED.with(|count| count.set(count.get() + 1));
}

static OWNER_CELL_HOOKS: EngineCellHooks =
    EngineCellHooks { trace: None, finalize: finalize_owner_cell };

type TestNative = fn(Context, &mut NativeCall) -> Result<(), Value>;

struct MemoryHost;

impl Host for MemoryHost {
    fn trace_roots(&self, _: GcHookCx<'_>, visitor: &mut dyn RootVisitor) {
        CURRENT.with(|current| {
            let mut bits = current.get();
            visitor.value(&mut bits);
            current.set(bits);
        });
    }

    fn call_native(
        &self,
        cx: Context,
        native: *const c_void,
        call: &mut NativeCall,
    ) -> EvalResult<()> {
        // SAFETY: every native these tests create is a `TestNative`.
        let native = unsafe { std::mem::transmute::<*const c_void, TestNative>(native) };
        native(cx, call).map_err(|value| embed::throw_value(value.to_handle(cx)))
    }
}

fn new_context_with_options(options: OptionsBuilder) -> Context {
    // Fresh per-thread state: tests share threads
    BLOCKS.with(|blocks| blocks.borrow_mut().clear());
    CURRENT.with(|current| current.set(0));
    OWNER.with(|owner| owner.set(None));
    SKIP_DETACH.with(|skip| skip.set(false));
    CELLS_FINALIZED.with(|count| count.set(0));

    #[allow(unused_mut)]
    let mut cx = ContextBuilder::new()
        .set_options(Rc::new(options.build().unwrap()))
        .build()
        .unwrap();
    embed::install_host(cx, Box::new(MemoryHost)).unwrap();

    #[cfg(feature = "gc_stress_test")]
    cx.enable_gc_stress_test();

    cx
}

fn new_context() -> Context {
    new_context_with_options(OptionsBuilder::new())
}

/// Destroy the context, then free the fake memory (never before: buffers may still point at it).
fn finish(cx: Context) {
    CURRENT.with(|current| current.set(0));
    cx.drop();
    BLOCKS.with(|blocks| blocks.borrow_mut().clear());
}

fn key(mut cx: Context, name: &str) -> Handle<PropertyKey> {
    let string = cx.alloc_string(name).unwrap();
    PropertyKey::string_handle(cx, string).unwrap()
}

fn define_global(cx: Context, name: &str, value: Handle<Value>) {
    let global = cx.initial_realm().global_object().as_object();
    let key = key(cx, name);
    let desc =
        PropertyDescriptor::data(value, PropertyFlags::from_data_attributes(true, true, true));
    define_property_or_throw(cx, global, key, desc).unwrap();
}

fn define_native(cx: Context, name: &str, native: TestNative) {
    let realm = cx.initial_realm();
    let name_key = key(cx, name);
    let function =
        embed::new_native_function(cx, realm, native as *const c_void, 1, Some(name_key), false, 0)
            .unwrap();
    define_global(cx, name, function.as_value());
}

/// Evaluate in the initial realm and convert the completion value to a string.
fn eval(cx: Context, source: &str) -> String {
    let realm = cx.initial_realm();
    let result = embed::evaluate_script(cx, realm, source, "external_buffer_test.js", 1);
    embed::with_realm(cx, realm, |cx| match result {
        Ok(value) => to_string(cx, value).unwrap().format().unwrap(),
        Err(error) => {
            let error = embed::exception_value(error);
            panic!("script threw: {}", to_string(cx, error).unwrap().format().unwrap())
        }
    })
}

fn current_buffer() -> HeapPtr<ArrayBufferObject> {
    let bits = CURRENT.with(|current| current.get());
    Value::from_raw_bits(bits)
        .as_object()
        .cast::<ArrayBufferObject>()
}

/// Make an external buffer over the current block and record it as the current buffer. The owner
/// cell of a `Cell` memory is `owner`, or a new one.
fn attach_current_block(
    cx: Context,
    owner: Option<Handle<ObjectValue>>,
) -> Handle<ArrayBufferObject> {
    let (ptr, len) = BLOCKS.with(|blocks| {
        let mut blocks = blocks.borrow_mut();
        let block = blocks.last_mut().unwrap();
        (block.as_mut_ptr(), block.len())
    });
    let realm = cx.initial_realm();
    let owner = match OWNER.with(|owner| owner.get()).unwrap() {
        Owner::Embedder => api::ExternalOwner::None,
        Owner::Cell => api::ExternalOwner::Cell(owner.unwrap_or_else(|| {
            embed::new_engine_cell(cx, &OWNER_CELL_HOOKS, cx.initial_realm_ptr(), 0)
        })),
    };

    // SAFETY: the block is a live boxed slice of `len` bytes; it stays allocated until `finish`,
    // after the context is gone, and only the engine accesses it while JS runs (the tests read it
    // between engine calls only).
    let buffer = unsafe { api::array_buffer_new_external(cx, realm, ptr, len, owner) }.unwrap();
    CURRENT.with(|current| current.set(buffer.as_value().as_raw_bits()));
    buffer
}

/// Set up a fake memory of `len` bytes holding 0, 1, 2, ... and the natives `mem()` (the current
/// buffer), `grow(n)` and `gc()`.
fn install_memory(cx: Context, owner: Owner, len: usize) -> Handle<ArrayBufferObject> {
    OWNER.with(|o| o.set(Some(owner)));
    let bytes: Vec<u8> = (0..len).map(|i| i as u8).collect();
    BLOCKS.with(|blocks| blocks.borrow_mut().push(Block::new(&bytes)));
    let buffer = attach_current_block(cx, None);
    define_native(cx, "mem", native_mem);
    define_native(cx, "grow", native_grow);
    define_native(cx, "gc", native_gc);
    buffer
}

fn native_mem(_: Context, call: &mut NativeCall) -> Result<(), Value> {
    call.set_return_value(Value::from_raw_bits(CURRENT.with(|current| current.get())));
    Ok(())
}

fn native_gc(cx: Context, _: &mut NativeCall) -> Result<(), Value> {
    embed::gc(cx);
    Ok(())
}

/// Move the memory to a larger block, retire (poison) the old one, detach the old buffer and
/// attach a new one: what a wasm grow followed by `sync_buffers` does.
fn native_grow(cx: Context, call: &mut NativeCall) -> Result<(), Value> {
    let pages = if call.arg(0).is_undefined() {
        1
    } else {
        call.arg(0).as_number() as usize
    };
    BLOCKS.with(|blocks| {
        let mut blocks = blocks.borrow_mut();
        let old = blocks.last_mut().unwrap();
        let mut bytes = old.to_vec();
        bytes.resize(old.len() + pages * PAGE, 0);
        old.fill(POISON);
        blocks.push(Block::new(&bytes));
    });

    handle_scope_guard!(cx);
    // CURRENT is a host root (`MemoryHost::trace_roots`), forwarded by every GC, so this is the
    // buffer object JS holds now, not the address it had when it was attached
    let mut old_buffer = current_buffer().to_handle();
    let owner = old_buffer.external_owner().map(|owner| owner.to_handle());
    if !SKIP_DETACH.with(|skip| skip.get()) {
        old_buffer.detach_external();
    }
    attach_current_block(cx, owner);
    Ok(())
}

/// Whether every retired block is still all POISON: nothing wrote to memory the buffers had let go
/// of.
fn retired_blocks_untouched() -> bool {
    BLOCKS.with(|blocks| {
        let blocks = blocks.borrow();
        blocks[..blocks.len() - 1]
            .iter()
            .all(|block| block.iter().all(|&b| b == POISON))
    })
}

fn assert_retired_blocks_untouched() {
    assert!(retired_blocks_untouched(), "a retired block was written after it was retired");
}

fn current_block() -> Vec<u8> {
    BLOCKS.with(|blocks| blocks.borrow().last().unwrap().to_vec())
}

// --- The bytes are shared with the owner ---

/// Every kind of access reads and writes the owner's bytes in place: element access (the VM fast
/// path and the generic path), DataView, slice, copies into heap buffers and from them, and the
/// embedder pointers.
#[test]
fn external_bytes_are_the_owners_bytes() {
    let cx = new_context();
    {
        handle_scope_guard!(cx);
        let buffer = install_memory(cx, Owner::Embedder, 64);

        let ptr = BLOCKS.with(|blocks| blocks.borrow_mut().last_mut().unwrap().as_mut_ptr());
        assert_eq!(api::array_buffer_data_ptr(buffer), Some(ptr));
        assert!(api::array_buffer_is_external(buffer));
        assert!(!api::array_buffer_is_detached(buffer));
        assert_eq!(api::array_buffer_detach_key(buffer), DetachKey::None);

        let source = "var b = mem(); var u8 = new Uint8Array(b); var i32 = new Int32Array(b, 8, 4);\
                      var dv = new DataView(b, 16);\
                      var out = [b.byteLength, b.resizable, b.maxByteLength, b.detached, u8[5], u8['6'], i32[0],\
                                 dv.getUint8(0), dv.getInt32(0, true)];\
                      u8[0] = 200; i32[1] = -2; dv.setInt16(4, 0x1234, true); u8.fill(7, 60, 62);\
                      var s = b.slice(2, 6); out.push(s.byteLength, new Uint8Array(s).join('|'));\
                      var copy = new Uint8Array(u8.subarray(1, 4)); out.push(copy.join('|'));\
                      var heap = new Uint8Array([9, 8, 7]); u8.set(heap, 40); u8.copyWithin(44, 40, 43);\
                      out.push(Array.from(u8.subarray(40, 47)).join('|'));\
                      out.join()";
        assert_eq!(
            eval(cx, source),
            "64,false,64,false,5,6,185207048,16,319951120,4,2|3|4|5,1|2|3,9|8|7|43|9|8|7"
        );

        let block = current_block();
        assert_eq!(block[0], 200);
        assert_eq!(&block[12..16], &(-2i32).to_le_bytes());
        assert_eq!(&block[20..22], &0x1234u16.to_le_bytes());
        assert_eq!(&block[60..62], &[7, 7]);
        assert_eq!(&block[40..47], &[9, 8, 7, 43, 9, 8, 7]);

        // The embedder pointers of a view and a DataView over the external bytes
        let realm = cx.initial_realm();
        let view =
            api::create_typed_array(cx, realm, api::TypedArrayKind::UInt8, buffer, 3, Some(4))
                .unwrap();
        assert_eq!(api::typed_array_data_ptr(view), Some(ptr.wrapping_add(3)));
        let data_view = api::create_data_view(cx, realm, buffer, 5, None).unwrap();
        let data_view = api::as_data_view(data_view).unwrap();
        assert_eq!(api::data_view_data_ptr(data_view), Some(ptr.wrapping_add(5)));
    }
    finish(cx);
}

/// The VM fast path for `ta[i]` on an external buffer, against a model of the bytes.
#[test]
fn element_fast_path_reads_and_writes_external_bytes() {
    let cx = new_context();
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Cell, 256);
        let source = "var a = new Int32Array(mem()); var f = new Float64Array(mem(), 128); var s = 0;\
                      for (let i = 0; i < 20000; i++) { a[i & 31] = (a[i & 31] + i) | 0; s = (s + a[(i + 3) & 31]) | 0;\
                        f[i & 15] = i / 2; }\
                      s + ',' + f[3] + ',' + a[64] + ',' + f[16]";
        let result = eval(cx, source);

        // The same loop on a model
        let initial: Vec<u8> = (0..256).map(|i| i as u8).collect();
        let mut a: Vec<i32> = initial[..128]
            .chunks(4)
            .map(|c| i32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        let mut f = [0f64; 16];
        let mut s: i32 = 0;
        for i in 0..20000i32 {
            let j = (i & 31) as usize;
            a[j] = a[j].wrapping_add(i);
            s = s.wrapping_add(a[((i + 3) & 31) as usize]);
            f[(i & 15) as usize] = i as f64 / 2.0;
        }
        assert_eq!(result, format!("{s},{},undefined,undefined", f[3]));

        let block = current_block();
        for (j, value) in a.iter().enumerate() {
            assert_eq!(&block[j * 4..j * 4 + 4], &value.to_le_bytes());
        }
        for (j, value) in f.iter().enumerate() {
            assert_eq!(&block[128 + j * 8..128 + j * 8 + 8], &value.to_le_bytes());
        }
    }
    finish(cx);
}

// --- Detach, keys, transfer ---

/// An external buffer detached by its embedder takes the spec's detached paths everywhere, and no
/// access reaches its memory again.
#[test]
fn detached_external_buffer_takes_the_detached_paths() {
    let cx = new_context();
    {
        handle_scope_guard!(cx);
        let buffer = install_memory(cx, Owner::Embedder, 32);
        eval(
            cx,
            "var b = mem(); var u8 = new Uint8Array(b); var i32 = new Int32Array(b, 4, 2); var dv = new DataView(b); 1",
        );

        assert!(api::array_buffer_detach(buffer));
        assert!(api::array_buffer_is_detached(buffer));
        assert!(!api::array_buffer_is_external(buffer));
        assert_eq!(api::array_buffer_data_ptr(buffer), None);
        BLOCKS.with(|blocks| blocks.borrow_mut().last_mut().unwrap().fill(POISON));

        let source = "function throws(f) { try { f(); return 'no'; } catch (e) { return e.constructor.name; } }\
                      var out = [b.detached, b.byteLength, u8.length, u8.byteOffset, i32.length, u8[0], i32[1]];\
                      u8[0] = 1; i32[0] = 2; for (let i = 0; i < 100; i++) u8[i & 7] = i;\
                      out.push(u8[0], throws(() => dv.getInt8(0)), throws(() => dv.setInt8(0, 1)),\
                               throws(() => b.slice(0)), throws(() => u8.fill(1)), throws(() => b.transfer()),\
                               Object.keys(u8).length);\
                      out.join()";
        assert_eq!(
            eval(cx, source),
            "true,0,0,0,0,,,,TypeError,TypeError,TypeError,TypeError,TypeError,0"
        );
        assert!(
            current_block().iter().all(|&b| b == POISON),
            "a detached buffer wrote its old memory"
        );

        let view = eval_object(cx, "u8");
        assert_eq!(api::typed_array_data_ptr(view), None);
        let dv = api::as_data_view(eval_object(cx, "dv")).unwrap();
        assert_eq!(api::data_view_data_ptr(dv), None);
    }
    finish(cx);
}

fn eval_object(cx: Context, source: &str) -> Handle<ObjectValue> {
    let realm = cx.initial_realm();
    let value = embed::evaluate_script(cx, realm, source, "external_buffer_test.js", 1)
        .ok()
        .unwrap();
    value.as_object()
}

/// The buffer of a wasm memory (detach key `WasmMemory`) cannot be detached by the embedder or
/// transferred by JS; it is fixed-length; its owner can still detach it.
#[test]
fn wasm_memory_key_refuses_detach_and_transfer() {
    let cx = new_context();
    {
        handle_scope_guard!(cx);
        let buffer = install_memory(cx, Owner::Cell, 16);
        assert_eq!(api::array_buffer_detach_key(buffer), DetachKey::WasmMemory);
        assert!(
            !api::array_buffer_detach(buffer),
            "the embedder detached a wasm memory's buffer"
        );
        assert!(!api::array_buffer_is_detached(buffer));

        let source = "function throws(f) { try { f(); return 'no'; } catch (e) { return e.constructor.name; } }\
                      var b = mem();\
                      [throws(() => b.transfer()), throws(() => b.transfer(32)), throws(() => b.transferToFixedLength()),\
                       throws(() => b.resize(8)), b.detached, b.byteLength, new Uint8Array(b)[15], b.resizable,\
                       b.maxByteLength].join()";
        assert_eq!(
            eval(cx, source),
            "TypeError,TypeError,TypeError,TypeError,false,16,15,false,16"
        );

        // Its owner detaches it (as sync_buffers does)
        let mut buffer = buffer;
        buffer.detach_external();
        assert!(buffer.is_detached());
        assert_eq!(buffer.detach_key(), DetachKey::None);
        assert_eq!(eval(cx, "[b.detached, b.byteLength].join()"), "true,0");
    }
    finish(cx);
}

/// `transfer` and `transferToFixedLength` of a keyless external buffer copy its bytes into a heap
/// buffer, then detach it; the old memory is not touched afterwards.
#[test]
fn transfer_of_a_keyless_external_buffer_copies_then_detaches() {
    let cx = new_context();
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Embedder, 16);
        let source = "var b = mem(); var t = b.transfer(); var tv = new Uint8Array(t);\
                      var out = [b.detached, t.byteLength, tv[15], t.resizable];\
                      tv[0] = 99; out.join()";
        assert_eq!(eval(cx, source), "true,16,15,false");
        let block = current_block();
        assert_eq!(block[0], 0, "the transfer's target aliases the old memory");

        // A longer and a shorter target, from fresh external buffers over the same block
        for (call, expected) in [
            ("transfer(20)", "true,20,15,0,false"),
            ("transferToFixedLength(4)", "true,4,,,false"),
        ] {
            handle_scope_guard!(cx);
            attach_current_block(cx, None);
            let source = format!(
                "var b = mem(); var t = b.{call}; var tv = new Uint8Array(t);\
                 [b.detached, t.byteLength, tv[15], tv[19], t.resizable].join()"
            );
            assert_eq!(eval(cx, &source), expected, "{call}");
        }
    }
    finish(cx);
}

// --- Grow: the buffer is replaced, old views detach (the buffer "generation") ---

/// The ten-generation scenario: views of each buffer, then `grow(1)` and `gc()`. Returns the JS
/// result: whether every old buffer and view is detached, the final length, two copied bytes, a
/// pristine byte, whether `mem()` changed, and whether `mem()` still names the buffer `b` after a
/// GC moved it (the host root is forwarded).
const GENERATIONS: &str = "var gens = []; var same_after_gc = true;\
    for (let g = 0; g < 10; g++) {\
      const b = mem(); const v = new Uint8Array(b); const d = new DataView(b); v[g] = 100 + g;\
      gc(); same_after_gc = same_after_gc && mem() === b;\
      gens.push([b, v, d]); grow(1); gc();\
    }\
    var stale = gens.every(([b, v, d]) => b.detached && b.byteLength === 0 && v.length === 0 &&\
      v[0] === undefined && (() => { try { d.getInt8(0); return false; } catch (e) { return e instanceof TypeError; } })());\
    for (const [b, v] of gens) { v[0] = 1; v[3] = 2; }\
    var now = new Uint8Array(mem());\
    [stale, mem().byteLength, now[0], now[9], now[10], mem() === gens[9][0], same_after_gc].join()";

/// After each grow the previous buffer is detached and every view of it has length 0, while the new
/// buffer has the new length and the old contents. Ten generations, with GCs in between, under the
/// heap verifier (which aborts on a root that still points into from-space) and GC stress.
#[test]
fn grow_detaches_every_view_of_the_previous_buffer() {
    let options = OptionsBuilder::new()
        .gc_stress_window(Some(32 << 10))
        .gc_verify(true);
    let cx = new_context_with_options(options);
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Cell, PAGE);
        assert_eq!(eval(cx, GENERATIONS), format!("true,{},100,109,10,false,true", 11 * PAGE));
        assert_retired_blocks_untouched();
        // One owner cell for the whole memory, carried from buffer to buffer
        assert_eq!(CELLS_FINALIZED.with(|c| c.get()), 0);
    }
    finish(cx);
    assert_eq!(CELLS_FINALIZED.with(|c| c.get()), 1);
}

/// Negative control: the same scenario with a `grow` that leaves the old buffer attached (as a
/// grow that detached some other object would). Both checks of the test above catch it: the JS
/// result says the old views are not detached, and the retired blocks were written through them.
#[test]
fn grow_tests_fail_when_grow_leaves_the_old_buffer_attached() {
    let options = OptionsBuilder::new()
        .gc_stress_window(Some(32 << 10))
        .gc_verify(true);
    let cx = new_context_with_options(options);
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Cell, PAGE);
        SKIP_DETACH.with(|skip| skip.set(true));
        let result = eval(cx, GENERATIONS);
        assert!(result.starts_with("false,"), "the stale-view check did not notice: {result}");
        assert!(!retired_blocks_untouched(), "the retired-block check did not notice");
    }
    finish(cx);
}

// --- GC (§3.5 items 4 and 6) ---

/// Item 4: a GC between creating the buffer and using it, under the heap verifier (which aborts on
/// a bad pointer) and GC stress.
#[test]
fn gc_between_creating_and_using_the_buffer_under_verify() {
    let options = OptionsBuilder::new()
        .gc_stress_window(Some(32 << 10))
        .gc_verify(true);
    let cx = new_context_with_options(options);
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Cell, 4 * PAGE);
        let before = embed::heap_stats(cx).gc_count;
        let source = "var b = mem(); gc(); if (mem() !== b) throw new Error('the memory record lost its buffer');\
                  var views = [];\
                      for (let i = 0; i < 2000; i++) { views.push(new Uint8Array(b, i & 63, 8)); views.push({ i, s: 'x' + i });\
                        if (views.length > 64) views.splice(0, 2); }\
                      gc(); var sum = 0; for (const v of views) if (v instanceof Uint8Array) sum += v[0];\
                      grow(1); gc(); [sum > 0, views[0].length, new Uint8Array(mem())[127]].join()";
        assert_eq!(eval(cx, source), "true,0,127");
        assert!(embed::heap_stats(cx).gc_count - before >= 5);
        assert_retired_blocks_untouched();
    }
    finish(cx);
}

/// Item 6: the buffer is the only thing that reaches its owner cell. Through many GCs the cell
/// lives and the contents stay intact; once the buffer is unreachable the cell dies with it.
#[test]
fn only_the_buffer_keeps_its_owner_cell() {
    let cx = new_context();
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Cell, 2 * PAGE);
        eval(cx, "var keep = mem(); 1");
    }
    // The memory's record is no root any more; only the global `keep` reaches the buffer
    CURRENT.with(|current| current.set(0));
    for _ in 0..20 {
        embed::gc(cx);
    }
    assert_eq!(CELLS_FINALIZED.with(|c| c.get()), 0);
    {
        handle_scope_guard!(cx);
        assert_eq!(eval(cx, "var v = new Uint8Array(keep); v[0] + v[63] + keep.byteLength"), "127");
        eval(cx, "keep = undefined; v = undefined; 1");
    }
    embed::gc(cx);
    embed::gc(cx);
    assert_eq!(CELLS_FINALIZED.with(|c| c.get()), 1);
    finish(cx);
}

// --- Callouts (§3.3 rows with a callout; §3.5 item 10) ---

const THROWS: &str =
    "function throws(f) { try { f(); return 'no'; } catch (e) { return e.constructor.name; } }";

/// `ArrayBuffer.prototype.slice` whose species constructor grows the memory: the re-check after
/// user code sees the detached buffer and throws.
#[test]
fn callout_array_buffer_slice_species_grows_memory() {
    let cx = new_context();
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Cell, PAGE);
        let source = format!(
            "{THROWS} var b = mem(); b.constructor = {{ [Symbol.species]: function (n) {{ grow(1); return new ArrayBuffer(n); }} }};\
             [throws(() => b.slice(0, 8)), b.detached].join()"
        );
        assert_eq!(eval(cx, &source), "TypeError,true");
        assert_retired_blocks_untouched();
    }
    finish(cx);
}

/// `TypedArray.prototype.set` from an array-like whose getter grows the memory: every element after
/// the grow is skipped (re-validation in `write_element_value_unchecked`), nothing reaches the old
/// block, and the new memory holds what was written before the grow.
#[test]
fn callout_typed_array_set_from_array_like_grows_memory() {
    let cx = new_context();
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Cell, PAGE);
        let source = "var v = new Uint8Array(mem());\
                      var src = { length: 4, get 0() { return 50; }, get 1() { grow(1); return 51; }, get 2() { return 52; }, 3: 53 };\
                      v.set(src); var now = new Uint8Array(mem());\
                      [v.length, now[0], now[1], now[2], now[3], now.length].join()";
        assert_eq!(eval(cx, source), format!("0,50,1,2,3,{}", 2 * PAGE));
        assert_retired_blocks_untouched();
    }
    finish(cx);
}

/// `copyWithin` whose argument coercion grows the memory: re-validated, throws.
#[test]
fn callout_typed_array_copy_within_coercion_grows_memory() {
    let cx = new_context();
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Cell, PAGE);
        let source = format!(
            "{THROWS} var v = new Uint8Array(mem());\
             [throws(() => v.copyWithin(0, {{ valueOf() {{ grow(1); return 4; }} }})), v.length].join()"
        );
        assert_eq!(eval(cx, &source), "TypeError,0");
        assert_retired_blocks_untouched();
    }
    finish(cx);
}

/// `TypedArray.prototype.slice` whose species constructor grows the memory: re-validated, throws.
#[test]
fn callout_typed_array_slice_species_grows_memory() {
    let cx = new_context();
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Cell, PAGE);
        let source = format!(
            "{THROWS} var v = new Uint8Array(mem());\
             v.constructor = {{ [Symbol.species]: function (n) {{ grow(1); return new Uint8Array(n); }} }};\
             [throws(() => v.slice(0, 4)), v.length].join()"
        );
        assert_eq!(eval(cx, &source), "TypeError,0");
        assert_retired_blocks_untouched();
    }
    finish(cx);
}

/// `ta[i] = x` and `defineProperty` whose value coercion grows the memory: the write is dropped
/// (the buffer is detached after the coercion), no exception, nothing reaches the old block.
#[test]
fn callout_element_set_and_define_coercion_grows_memory() {
    let cx = new_context();
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Cell, PAGE);
        let source = "var v = new Uint8Array(mem()); var w = new Int32Array(mem());\
                      v[0] = { valueOf() { grow(1); return 9; } };\
                      var w2 = new Int32Array(mem()); var x = w2;\
                      Object.defineProperty(x, '1', { value: { valueOf() { grow(1); return 7; } } });\
                      var now = new Uint8Array(mem());\
                      [v.length, w.length, x.length, now[0], now[4], now.length].join()";
        assert_eq!(eval(cx, source), format!("0,0,0,0,4,{}", 3 * PAGE));
        assert_retired_blocks_untouched();
    }
    finish(cx);
}

/// `DataView.prototype.setInt32` whose value coercion grows the memory: the view is re-checked
/// after the conversion and throws; `getInt32` with a growing index coercion too.
#[test]
fn callout_data_view_coercion_grows_memory() {
    let cx = new_context();
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Cell, PAGE);
        let source = format!(
            "{THROWS} var d = new DataView(mem());\
             var a = throws(() => d.setInt32(0, {{ valueOf() {{ grow(1); return 7; }} }}));\
             var e = new DataView(mem());\
             var b = throws(() => e.getInt32({{ valueOf() {{ grow(1); return 0; }} }}));\
             [a, b, new Uint8Array(mem())[0]].join()"
        );
        assert_eq!(eval(cx, &source), "TypeError,TypeError,0");
        assert_retired_blocks_untouched();
    }
    finish(cx);
}

/// `toBase64` and `setFromBase64` whose option getters grow the memory: re-validated after the
/// options, throw.
#[test]
fn callout_base64_options_grow_memory() {
    let cx = new_context();
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Cell, PAGE);
        let source = format!(
            "{THROWS} var v = new Uint8Array(mem());\
             var a = throws(() => v.toBase64({{ get alphabet() {{ grow(1); return 'base64'; }} }}));\
             var w = new Uint8Array(mem());\
             var b = throws(() => w.setFromBase64('AAAA', {{ get alphabet() {{ grow(1); return 'base64'; }} }}));\
             var u = new Uint8Array(mem()); var c = u.subarray(0, 3).toBase64();\
             [a, b, c, new Uint8Array(mem()).length].join()"
        );
        assert_eq!(eval(cx, &source), format!("TypeError,TypeError,AAEC,{}", 3 * PAGE));
        assert_retired_blocks_untouched();
    }
    finish(cx);
}

/// `TypedArray.prototype.set` from an external typed array of another element type: the
/// conversion loop may allocate (GC) between elements; the bytes do not move and the values are
/// right. Under GC stress and the verifier.
#[test]
fn set_from_an_external_typed_array_converts_across_gcs() {
    let options = OptionsBuilder::new()
        .gc_stress_window(Some(16 << 10))
        .gc_verify(true);
    let cx = new_context_with_options(options);
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Cell, 4 * PAGE);
        let source = "var src = new Int32Array(mem()); for (let i = 0; i < src.length; i++) src[i] = i * 1000003;\
                      var keep = []; var ok = true;\
                      for (let r = 0; r < 200; r++) { var dst = new Float64Array(src.length); dst.set(src);\
                        var big = new BigInt64Array(2); keep.push(dst, 'y' + r); if (keep.length > 20) keep.splice(0, 2);\
                        for (let i = 0; i < src.length; i++) if (dst[i] !== i * 1000003) ok = false; }\
                      var back = new Int32Array(mem(), 0, 4); back.set(new Float64Array([1.5, -2.5, 3, 4]));\
                      [ok, back.join('|')].join()";
        assert_eq!(eval(cx, source), "true,1|-2|3|4");
    }
    finish(cx);
}

/// `new Uint8Array(externalView)` copies through CloneArrayBuffer-style allocation first, then the
/// bytes: the copy is a heap buffer with the same bytes.
#[test]
fn typed_array_constructor_copies_an_external_view() {
    let cx = new_context();
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Embedder, PAGE);
        let source = "var v = new Uint8Array(mem(), 4, 4); var c = new Uint8Array(v); var d = new Int16Array(v);\
                      c[0] = 77; [c.join('|'), d.join('|'), v[0]].join()";
        assert_eq!(eval(cx, source), "77|5|6|7,4|5|6|7,4");
    }
    finish(cx);
}

/// `$262.detachArrayBuffer` honours the detach key: a wasm memory's buffer throws a TypeError and
/// stays attached; a keyless external buffer and a heap buffer detach.
#[test]
fn test262_detach_honours_the_detach_key() {
    let cx = new_context_with_options(OptionsBuilder::new().expose_test_262(true));
    {
        handle_scope_guard!(cx);
        install_memory(cx, Owner::Cell, 16);
        let source = format!(
            "{THROWS} var b = mem(); var h = new ArrayBuffer(8);\
             [throws(() => $262.detachArrayBuffer(b)), b.detached, new Uint8Array(b)[15],\
              throws(() => $262.detachArrayBuffer(h)), h.detached].join()"
        );
        assert_eq!(eval(cx, &source), "TypeError,false,15,no,true");

        attach_keyless_buffer_over_current_block(cx);
        assert_eq!(
            eval(
                cx,
                "var k = mem(); $262.detachArrayBuffer(k); [k.detached, k.byteLength].join()"
            ),
            "true,0"
        );
    }
    finish(cx);
}

/// Detach the current buffer, then attach the current block again as a keyless
/// (`ExternalOwner::None`) buffer: never two attached buffers over the same bytes.
fn attach_keyless_buffer_over_current_block(cx: Context) {
    current_buffer().detach_external();
    OWNER.with(|owner| owner.set(Some(Owner::Embedder)));
    attach_current_block(cx, None);
}

/// `typed_array_data_ptr` returns None for a view that is out of bounds because its resizable
/// buffer shrank, instead of slicing past the end.
#[test]
fn typed_array_data_ptr_is_none_out_of_bounds() {
    let cx = new_context();
    {
        handle_scope_guard!(cx);
        let realm = cx.initial_realm();
        let view = embed::evaluate_script(
            cx,
            realm,
            "var rab = new ArrayBuffer(16, { maxByteLength: 16 }); var v = new Uint8Array(rab, 8, 8); v",
            "external_buffer_test.js",
            1,
        )
        .ok()
        .unwrap()
        .as_object();
        assert!(api::typed_array_data_ptr(view).is_some());
        eval(cx, "rab.resize(12); 1");
        assert_eq!(api::typed_array_data_ptr(view), None, "a view past the shrunk end");
        eval(cx, "rab.resize(4); 1");
        assert_eq!(api::typed_array_data_ptr(view), None, "a view whose offset is past the end");
        eval(cx, "rab.resize(16); 1");
        assert!(api::typed_array_data_ptr(view).is_some(), "back in bounds");
    }
    finish(cx);
}
