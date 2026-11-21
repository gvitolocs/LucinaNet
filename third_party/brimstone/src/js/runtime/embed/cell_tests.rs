//! Tests for engine cells (WebAssembly design §2.2, §2.4) and the other engine-core hooks of slice
//! W0b: `Realm::wasm_store`, the cell call roots and their teardown abort, external buffers freed
//! by a cell, `CompileStringsKind::Wasm`, `Host::consume_stream`, `set_wasm_enabled` and the
//! interrupt wrappers.
//!
//! With the `gc_stress_test` feature every test runs with a GC on every allocation.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use crate::{
    common::options::OptionsBuilder,
    handle_scope_guard,
    runtime::{
        Context, ContextBuilder, EvalResult, Handle, HeapPtr, PropertyKey, Realm, Value,
        abstract_operations::get,
        embed::{
            self, EngineCellHooks, GcHookCx, Host, HostFlags, HostObjectRef, RootVisitor, api,
        },
        object_value::ObjectValue,
        string_value::StringValue,
        type_utilities::to_string,
    },
};

// --- A host that records what it sees ---

#[derive(Default)]
struct Recorder {
    /// `trace_host_object` and `finalize_host_object` calls: cells must never reach them.
    host_trace_calls: Cell<usize>,
    host_finalize_calls: Cell<usize>,
    /// `can_compile_strings` calls, as the kind's debug name.
    compile_kinds: RefCell<Vec<String>>,
    block_compilation: Cell<bool>,
    interrupt_calls: Cell<usize>,
    continue_on_interrupt: Cell<bool>,
}

struct CellHost(Rc<Recorder>);

impl Host for CellHost {
    fn trace_host_object(&self, _: GcHookCx<'_>, _: HostObjectRef, _: &mut dyn RootVisitor) {
        self.0
            .host_trace_calls
            .set(self.0.host_trace_calls.get() + 1);
    }

    fn finalize_host_object(&self, _: GcHookCx<'_>, _: HostObjectRef) {
        self.0
            .host_finalize_calls
            .set(self.0.host_finalize_calls.get() + 1);
    }

    fn can_compile_strings(
        &self,
        _: Context,
        kind: embed::CompileStringsKind,
        code: Handle<StringValue>,
        parameters: &[Handle<StringValue>],
        body: Option<Handle<StringValue>>,
        _: &[Handle<Value>],
        _: Handle<Value>,
    ) -> EvalResult<bool> {
        if kind == embed::CompileStringsKind::Wasm {
            assert_eq!(code.format().unwrap(), "");
            assert!(parameters.is_empty() && body.is_none());
        }
        self.0.compile_kinds.borrow_mut().push(format!("{kind:?}"));
        Ok(!self.0.block_compilation.get())
    }

    fn interrupt(&self, _: Context) -> bool {
        self.0.interrupt_calls.set(self.0.interrupt_calls.get() + 1);
        self.0.continue_on_interrupt.get()
    }
}

/// A host that overrides nothing: the defaults.
struct DefaultHost;
impl Host for DefaultHost {}

fn new_context_with_options(options: OptionsBuilder) -> (Context, Rc<Recorder>) {
    let recorder = Rc::new(Recorder::default());
    #[allow(unused_mut)]
    let mut cx = ContextBuilder::new()
        .set_options(Rc::new(options.build().unwrap()))
        .build()
        .unwrap();
    embed::install_host(cx, Box::new(CellHost(recorder.clone()))).unwrap();

    #[cfg(feature = "gc_stress_test")]
    cx.enable_gc_stress_test();

    (cx, recorder)
}

fn new_context() -> (Context, Rc<Recorder>) {
    new_context_with_options(OptionsBuilder::new())
}

fn eval(cx: Context, realm: Handle<Realm>, source: &str) -> Handle<Value> {
    match embed::evaluate_script(cx, realm, source, "cell_test.js", 1) {
        Ok(value) => value,
        Err(error) => {
            let error = embed::exception_value(error);
            let message = embed::with_realm(cx, realm, |cx| to_string(cx, error).unwrap().format());
            panic!("script threw: {}", message.unwrap())
        }
    }
}

fn key(mut cx: Context, name: &str) -> Handle<PropertyKey> {
    let string = cx.alloc_string(name).unwrap();
    PropertyKey::string_handle(cx, string).unwrap()
}

/// A fresh ordinary object `{ marker: n }`, made by JS so it lives on the moving heap.
fn new_marker(cx: Context, n: i32) -> Value {
    let realm = cx.initial_realm();
    *eval(cx, realm, &format!("({{ marker: {n} }})"))
}

fn marker_of(cx: Context, value: Value) -> f64 {
    assert!(value.is_object(), "not an object: the slot was not updated or the object died");
    let key = key(cx, "marker");
    embed::with_realm(cx, cx.initial_realm(), |cx| get(cx, value.to_handle(cx).as_object(), key))
        .unwrap()
        .as_number()
}

// --- Cell hooks under test, with their state per test thread ---

thread_local! {
    /// The values each traced test cell holds, by cell address, in the NaN-boxed encoding.
    static HELD: RefCell<HashMap<usize, Vec<u64>>> = RefCell::new(HashMap::new());
    /// Every cell finalized by `finalize_test_cell`, in order.
    static FINALIZED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    static TRACE_CALLS: Cell<usize> = const { Cell::new(0) };
}

fn trace_test_cell(_: GcHookCx<'_>, cell: HeapPtr<ObjectValue>, visitor: &mut dyn RootVisitor) {
    TRACE_CALLS.with(|calls| calls.set(calls.get() + 1));
    HELD.with(|held| {
        if let Some(values) = held.borrow_mut().get_mut(&(cell.as_ptr() as usize)) {
            for bits in values {
                visitor.value(bits);
            }
        }
    });
}

fn finalize_test_cell(_: GcHookCx<'_>, cell: HeapPtr<ObjectValue>) {
    let address = cell.as_ptr() as usize;
    FINALIZED.with(|finalized| finalized.borrow_mut().push(address));
    HELD.with(|held| held.borrow_mut().remove(&address));
}

static TRACED_HOOKS: EngineCellHooks =
    EngineCellHooks { trace: Some(trace_test_cell), finalize: finalize_test_cell };
static PLAIN_HOOKS: EngineCellHooks = EngineCellHooks { trace: None, finalize: finalize_test_cell };

fn finalized_count(address: usize) -> usize {
    FINALIZED.with(|finalized| finalized.borrow().iter().filter(|&&a| a == address).count())
}

fn hold(cell: HeapPtr<ObjectValue>, value: Value) {
    HELD.with(|held| {
        held.borrow_mut()
            .entry(cell.as_ptr() as usize)
            .or_default()
            .push(value.as_raw_bits())
    });
}

fn held(cell: HeapPtr<ObjectValue>) -> Vec<Value> {
    HELD.with(|held| {
        held.borrow()
            .get(&(cell.as_ptr() as usize))
            .map(|values| {
                values
                    .iter()
                    .map(|&bits| Value::from_raw_bits(bits))
                    .collect()
            })
            .unwrap_or_default()
    })
}

fn new_cell(cx: Context, hooks: &'static EngineCellHooks, slots: u32) -> Handle<ObjectValue> {
    embed::new_engine_cell(cx, hooks, cx.initial_realm_ptr(), slots)
}

// --- Trace, forward, finalize ---

/// Hold a fresh marker object in `cell` only: the handle `eval` made is released before any GC.
fn hold_new_marker(cx: Context, cell: HeapPtr<ObjectValue>, n: i32) {
    handle_scope_guard!(cx);
    let marker = new_marker(cx, n);
    hold(cell, marker);
}

/// A cell reports a JS object through its trace hook. The object is reachable only that way; every
/// GC moves it and the hook's slot is updated to the new address. When the cell dies its finalizer
/// runs exactly once, and the host never sees the cell.
#[test]
fn cell_trace_forwards_its_values_and_finalize_runs_once() {
    let (cx, recorder) = new_context();
    let calls_before = TRACE_CALLS.with(|c| c.get());

    let address = {
        handle_scope_guard!(cx);
        let cell = new_cell(cx, &TRACED_HOOKS, 0);
        hold_new_marker(cx, *cell, 7);

        let mut previous = held(*cell)[0].as_raw_bits();
        for i in 0..50 {
            embed::gc(cx);
            let now = held(*cell)[0];
            assert_ne!(now.as_raw_bits(), previous, "the traced object did not move in GC {i}");
            previous = now.as_raw_bits();
            handle_scope_guard!(cx);
            let _ = new_marker(cx, i);
        }
        assert_eq!(marker_of(cx, held(*cell)[0]), 7.0);
        assert!(TRACE_CALLS.with(|c| c.get()) - calls_before >= 50);
        cell.as_ptr() as usize
    };
    assert_eq!(finalized_count(address), 0);

    // No handle left: dies at the next GC, finalized once, and only by its own hook
    embed::gc(cx);
    embed::gc(cx);
    assert_eq!(finalized_count(address), 1);
    assert_eq!(recorder.host_trace_calls.get(), 0);
    assert_eq!(recorder.host_finalize_calls.get(), 0);

    cx.drop();
    assert_eq!(finalized_count(address), 1, "teardown finalized a dead cell again");
}

/// A cell is not a host object to the embedding API: PINNED|INTERNAL_CELL are filtered out of
/// every host-visible path.
#[test]
fn cell_is_invisible_to_the_host_api() {
    let (cx, _recorder) = new_context();
    {
        handle_scope_guard!(cx);
        let realm = cx.initial_realm();
        let cell = new_cell(cx, &PLAIN_HOOKS, 2);

        assert!(embed::is_engine_cell(*cell));
        assert!(!embed::is_host_object(*cell));
        assert_eq!(embed::host_class(*cell), None);
        assert_eq!(embed::host_flags(*cell), None);
        assert_eq!(embed::host_handler(*cell), None);
        assert!(embed::is_in_host_space(cell.as_ptr().cast()), "cells never move");

        // A host object of the same layout cannot be swapped with it
        let host_object = embed::new_host_object(
            cx,
            realm,
            0x2000 as *const _,
            HostFlags::empty(),
            None,
            2,
            std::ptr::null(),
            cx.undefined(),
        )
        .unwrap();
        assert!(!embed::swap_host_objects(*cell, *host_object));
        assert!(!embed::swap_host_objects(*host_object, *cell));
        assert!(!embed::is_engine_cell(*host_object));

        // Its hooks are the ones it was made with, and its reserved slots work
        assert!(std::ptr::eq(embed::engine_cell_hooks(*cell).unwrap(), &PLAIN_HOOKS));
        assert!(embed::engine_cell_hooks(*host_object).is_none());
        assert!(embed::engine_cell_slot(*cell, 1).is_undefined());
        embed::set_engine_cell_slot(*cell, 1, Value::smi(41));
        embed::gc(cx);
        assert_eq!(embed::engine_cell_slot(*cell, 1).as_smi(), 41);
    }
    cx.drop();
}

/// Reserved slots of a cell are traced and updated like a host object's.
#[test]
fn cell_reserved_slots_keep_their_objects() {
    let (cx, _recorder) = new_context();
    {
        handle_scope_guard!(cx);
        let cell = new_cell(cx, &PLAIN_HOOKS, 1);
        {
            handle_scope_guard!(cx);
            let marker = new_marker(cx, 11);
            embed::set_engine_cell_slot(*cell, 0, marker);
        }
        for _ in 0..20 {
            embed::gc(cx);
        }
        assert_eq!(marker_of(cx, embed::engine_cell_slot(*cell, 0)), 11.0);
    }
    cx.drop();
}

/// The call roots (design §2.4): a cell whose last handle is gone stays alive while a call root
/// names it, nested roots pop in order, and the cell dies after the last pop.
#[test]
fn call_roots_keep_cells_alive_during_a_call() {
    let (cx, _recorder) = new_context();

    let (outer, inner, outer_depth, inner_depth) = {
        handle_scope_guard!(cx);
        let outer = new_cell(cx, &TRACED_HOOKS, 0);
        let inner = new_cell(cx, &TRACED_HOOKS, 0);
        hold_new_marker(cx, *outer, 1);
        hold_new_marker(cx, *inner, 2);
        let outer_depth = embed::push_cell_call_root(cx, *outer);
        let inner_depth = embed::push_cell_call_root(cx, *inner);
        (*outer, *inner, outer_depth, inner_depth)
    };
    assert_eq!((outer_depth, inner_depth), (0, 1));
    assert_eq!(embed::cell_call_root_count(cx), 2);

    // Rooted only by the call roots; GC with allocation in between, as an import that calls gc()
    for i in 0..20 {
        embed::gc(cx);
        handle_scope_guard!(cx);
        let _ = new_marker(cx, i);
    }
    assert_eq!(finalized_count(outer.as_ptr() as usize), 0);
    assert_eq!(finalized_count(inner.as_ptr() as usize), 0);
    assert_eq!(marker_of(cx, held(outer)[0]), 1.0);
    assert_eq!(marker_of(cx, held(inner)[0]), 2.0);

    embed::pop_cell_call_root(cx, inner_depth, inner);
    embed::gc(cx);
    assert_eq!(finalized_count(inner.as_ptr() as usize), 1);
    assert_eq!(finalized_count(outer.as_ptr() as usize), 0);
    assert_eq!(marker_of(cx, held(outer)[0]), 1.0);

    embed::pop_cell_call_root(cx, outer_depth, outer);
    assert_eq!(embed::cell_call_root_count(cx), 0);
    embed::gc(cx);
    assert_eq!(finalized_count(outer.as_ptr() as usize), 1);
    cx.drop();
}

/// The realm edge: a realm keeps its `wasm_store` cell alive, and the cell keeps its realm alive
/// through its header; both die together once neither is reachable.
#[test]
fn realm_and_its_wasm_store_cell_keep_each_other() {
    let (cx, _recorder) = new_context();

    let address = {
        handle_scope_guard!(cx);
        let mut realm = Realm::new(cx).unwrap();

        // The cell's own handle is released at the end of this scope: from then on only the
        // realm's edge holds it
        let address = {
            handle_scope_guard!(cx);
            let cell = embed::new_engine_cell(cx, &TRACED_HOOKS, *realm, 0);
            hold_new_marker(cx, *cell, 5);
            realm.set_wasm_store(Some(*cell));
            cell.as_ptr() as usize
        };

        for i in 0..10 {
            embed::gc(cx);
            handle_scope_guard!(cx);
            let _ = new_marker(cx, i);
        }
        let store = realm.wasm_store().unwrap();
        assert_eq!(store.as_ptr() as usize, address);
        assert_eq!(marker_of(cx, held(store)[0]), 5.0);
        assert!(embed::realm_of_object(cx, store).ptr_eq(&*realm));
        assert_eq!(finalized_count(address), 0);
        address
    };

    // Neither is reachable any more
    embed::gc(cx);
    embed::gc(cx);
    assert_eq!(finalized_count(address), 1);
    cx.drop();
}

/// A cell rooted only by its realm's `wasm_store`, while the realm is rooted only by the cell's
/// header and a call root on the cell: the cycle through the header is traced too.
#[test]
fn a_call_root_on_the_cell_keeps_its_realm() {
    let (cx, _recorder) = new_context();

    let (cell, depth) = {
        handle_scope_guard!(cx);
        let mut realm = Realm::new(cx).unwrap();
        let cell = embed::new_engine_cell(cx, &PLAIN_HOOKS, *realm, 0);
        realm.set_wasm_store(Some(*cell));
        (*cell, embed::push_cell_call_root(cx, *cell))
    };

    for _ in 0..5 {
        embed::gc(cx);
    }
    let realm = embed::realm_of_object(cx, cell);
    assert!(
        realm.wasm_store().unwrap().ptr_eq(&cell),
        "the realm died or moved without its edge"
    );
    assert_eq!(finalized_count(cell.as_ptr() as usize), 0);

    embed::pop_cell_call_root(cx, depth, cell);
    embed::gc(cx);
    assert_eq!(finalized_count(cell.as_ptr() as usize), 1);
    cx.drop();
}

/// Teardown finalizes every live cell exactly once, and a cell that died in a GC is not finalized
/// again.
#[test]
fn teardown_finalizes_live_cells_once() {
    let (cx, recorder) = new_context();
    let (a, b) = {
        handle_scope_guard!(cx);
        let a = new_cell(cx, &TRACED_HOOKS, 0);
        let b = new_cell(cx, &PLAIN_HOOKS, 0);
        hold_new_marker(cx, *a, 3);
        // Rooted through the initial realm, so it survives every GC until teardown
        let mut realm = cx.initial_realm();
        realm.set_wasm_store(Some(*a));
        (a.as_ptr() as usize, b.as_ptr() as usize)
    };
    embed::gc(cx);
    assert_eq!(finalized_count(a), 0);
    assert_eq!(finalized_count(b), 1, "b had no root and died in the GC");

    cx.drop();
    assert_eq!(finalized_count(a), 1);
    assert_eq!(finalized_count(b), 1);
    assert_eq!(recorder.host_finalize_calls.get(), 0);
}

/// Under the heap verifier (which aborts the process on a bad pointer) and GC stress, cells and
/// what their hooks trace check clean before and after every collection.
#[test]
fn cells_check_clean_under_gc_verify() {
    let options = OptionsBuilder::new()
        .gc_stress_window(Some(32 << 10))
        .gc_verify(true);
    let (cx, _recorder) = new_context_with_options(options);
    {
        handle_scope_guard!(cx);
        let realm = cx.initial_realm();
        let before = embed::heap_stats(cx).gc_count;

        let cells: Vec<Handle<ObjectValue>> = (0..8)
            .map(|i| {
                let cell = new_cell(cx, &TRACED_HOOKS, 1);
                hold_new_marker(cx, *cell, i);
                hold_new_marker(cx, *cell, 100 + i);
                embed::set_engine_cell_slot(*cell, 0, Value::smi(i));
                cell
            })
            .collect();
        let depth = embed::push_cell_call_root(cx, *cells[3]);

        let source = "var keep = []; for (let i = 0; i < 4000; i++) { keep.push({ i, s: 'x' + i }); \
                      if (keep.length > 50) keep.shift(); } keep.length";
        assert_eq!(eval(cx, realm, source).as_number(), 50.0);
        assert!(embed::heap_stats(cx).gc_count - before >= 10);

        for (i, cell) in cells.iter().enumerate() {
            let values = held(**cell);
            assert_eq!(marker_of(cx, values[0]), i as f64);
            assert_eq!(marker_of(cx, values[1]), 100.0 + i as f64);
            assert_eq!(embed::engine_cell_slot(**cell, 0).as_smi(), i as i32);
        }
        embed::pop_cell_call_root(cx, depth, *cells[3]);
    }
    cx.drop();
}

// --- Process-fatal misuse, in a child process ---

const CHILD_ENV: &str = "BS_CELL_TESTS_CHILD";

/// Run one test of this module in a child copy of the test binary with `CHILD_ENV` set, and
/// return its exit status.
fn run_child(test_name: &str, mode: &str) -> std::process::ExitStatus {
    let exe = std::env::current_exe().unwrap();
    std::process::Command::new(exe)
        .args([
            "--exact",
            &format!("runtime::embed::cell_tests::{test_name}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, mode)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
}

fn assert_aborted(status: std::process::ExitStatus) {
    assert!(!status.success(), "the child exited normally: {status:?}");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(6), "the child did not abort: {status:?}");
    }
}

/// The misuses themselves. Without `CHILD_ENV` this test does nothing: it is only run as a child.
#[test]
fn child_cell_misuse() {
    let Ok(mode) = std::env::var(CHILD_ENV) else {
        return;
    };
    let (cx, _recorder) = new_context();
    let cell = {
        handle_scope_guard!(cx);
        *new_cell(cx, &PLAIN_HOOKS, 0)
    };
    let depth = embed::push_cell_call_root(cx, cell);
    match mode.as_str() {
        // Destroying the context while a call using the cell is on the stack
        "teardown" => cx.drop(),
        // Popping with the wrong depth
        "pop_depth" => embed::pop_cell_call_root(cx, depth + 1, cell),
        // Popping an outer root while an inner one is still pushed
        "pop_order" => {
            let inner = {
                handle_scope_guard!(cx);
                *new_cell(cx, &PLAIN_HOOKS, 0)
            };
            embed::push_cell_call_root(cx, inner);
            embed::pop_cell_call_root(cx, depth, cell);
        }
        _ => {}
    }
    // Reached only if nothing aborted
    std::process::exit(0);
}

/// Teardown with a call root left is a host misuse: the process aborts (design §2.4).
#[test]
fn teardown_with_a_call_root_aborts() {
    assert_aborted(run_child("child_cell_misuse", "teardown"));
}

/// A pop that does not match the innermost push aborts instead of unrooting the wrong cell.
#[test]
fn mismatched_call_root_pops_abort() {
    assert_aborted(run_child("child_cell_misuse", "pop_depth"));
    assert_aborted(run_child("child_cell_misuse", "pop_order"));
}

/// The child test exits 0 when nothing goes wrong, so a child that never reached the misuse (a
/// wrong test path) would not count as an abort.
#[test]
fn child_runs_and_exits_cleanly_without_a_misuse() {
    let status = run_child("child_cell_misuse", "none");
    assert!(status.success(), "{status:?}");
}

// --- External buffers freed by a cell (ExternalOwner::Free) ---

/// The memory of a `Free` buffer, and a count of free calls.
fn new_free_buffer(
    cx: Context,
    len: usize,
    frees: &Rc<Cell<usize>>,
) -> Handle<api::ArrayBufferObject> {
    // Words, so the memory is 8-aligned as `array_buffer_new_external` requires
    assert_eq!(len % 8, 0);
    let words = len / 8;
    let memory: &'static mut [u64] =
        Box::leak(vec![0x5a5a_5a5a_5a5a_5a5au64; words].into_boxed_slice());
    let words_ptr = memory.as_mut_ptr();
    let frees = frees.clone();
    let free = Box::new(move || {
        frees.set(frees.get() + 1);
        // SAFETY: `words_ptr` came from `Box::leak` of a boxed slice of `words` words above, and
        // the closure runs at most once.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(words_ptr, words)) });
    });
    // SAFETY: the leaked slice is valid and 8-aligned for `len` bytes until the closure frees it,
    // and nothing else references it.
    unsafe {
        api::array_buffer_new_external(
            cx,
            cx.initial_realm(),
            words_ptr.cast(),
            len,
            api::ExternalOwner::Free(free),
        )
    }
    .unwrap()
}

#[test]
fn free_callback_runs_once_after_the_buffer_dies() {
    let (cx, _recorder) = new_context();
    let frees = Rc::new(Cell::new(0));
    {
        handle_scope_guard!(cx);
        let buffer = new_free_buffer(cx, 64, &frees);
        assert!(api::array_buffer_is_external(buffer));
        assert_eq!(api::array_buffer_detach_key(buffer), api::DetachKey::None);
        assert_eq!(api::array_buffer_byte_length(buffer), 64);
        embed::gc(cx);
        assert_eq!(frees.get(), 0, "freed while the buffer is alive");
        let data = api::array_buffer_data_ptr(buffer).unwrap();
        // SAFETY: the buffer is attached and 64 bytes long; no JS runs while it is read.
        assert_eq!(unsafe { *data.add(63) }, 0x5a);
    }
    embed::gc(cx);
    assert_eq!(frees.get(), 1);
    embed::gc(cx);
    cx.drop();
    assert_eq!(frees.get(), 1, "freed twice");
}

#[test]
fn free_callback_runs_once_after_detach_and_at_teardown() {
    let (cx, _recorder) = new_context();
    let detached = Rc::new(Cell::new(0));
    let kept = Rc::new(Cell::new(0));
    {
        handle_scope_guard!(cx);
        let buffer = new_free_buffer(cx, 16, &detached);
        let survivor = new_free_buffer(cx, 16, &kept);

        // Detached: the buffer drops its owner cell, which dies at the next GC
        assert!(api::array_buffer_detach(buffer));
        assert!(api::array_buffer_is_detached(buffer));
        assert!(api::array_buffer_data_ptr(buffer).is_none());
        embed::gc(cx);
        assert_eq!(detached.get(), 1);

        // Still attached at teardown: freed by teardown
        assert!(!api::array_buffer_is_detached(survivor));
        embed::gc(cx);
        assert_eq!(kept.get(), 0);
    }
    cx.drop();
    assert_eq!(kept.get(), 1);
    assert_eq!(detached.get(), 1);
}

#[test]
fn transfer_of_a_free_buffer_copies_and_frees_the_original_once() {
    let (cx, _recorder) = new_context();
    let frees = Rc::new(Cell::new(0));
    {
        handle_scope_guard!(cx);
        let realm = cx.initial_realm();
        let buffer = new_free_buffer(cx, 8, &frees);
        let global = realm.global_object().as_object();
        let name = key(cx, "ext");
        crate::runtime::abstract_operations::set(cx, global, name, buffer.as_value(), true)
            .unwrap();

        let result = eval(
            cx,
            realm,
            "var t = ext.transfer(12); var v = new Uint8Array(t); \
             [ext.detached, ext.byteLength, t.byteLength, v[0], v[7], v[8], v[11], t.resizable].join()",
        );
        let result =
            embed::with_realm(cx, realm, |cx| to_string(cx, result).unwrap().format().unwrap());
        assert_eq!(result, "true,0,12,90,90,0,0,false");
        embed::gc(cx);
        assert_eq!(frees.get(), 1);
    }
    cx.drop();
    assert_eq!(frees.get(), 1);
}

#[test]
fn null_or_misaligned_external_data_is_only_allowed_when_empty() {
    let (cx, _recorder) = new_context();
    {
        handle_scope_guard!(cx);
        let realm = cx.initial_realm();
        // SAFETY: a zero-length buffer never reads its data pointer.
        let empty = unsafe {
            api::array_buffer_new_external(
                cx,
                realm,
                std::ptr::null_mut(),
                0,
                api::ExternalOwner::None,
            )
        }
        .unwrap();
        assert!(!api::array_buffer_is_detached(empty));
        assert_eq!(api::array_buffer_byte_length(empty), 0);
        // SAFETY: refused before the pointer is stored.
        let refused = unsafe {
            api::array_buffer_new_external(
                cx,
                realm,
                std::ptr::null_mut(),
                4,
                api::ExternalOwner::None,
            )
        };
        assert!(refused.is_err());

        // Memory that is not 8-aligned is refused unless the buffer is empty (wasmi's empty memory
        // has the dangling address 1)
        let mut words = [0u64; 4];
        let base = words.as_mut_ptr().cast::<u8>();
        for misalignment in 1..8 {
            // SAFETY: refused before the pointer is stored.
            let refused = unsafe {
                api::array_buffer_new_external(
                    cx,
                    realm,
                    base.wrapping_add(misalignment),
                    8,
                    api::ExternalOwner::None,
                )
            };
            assert!(refused.is_err(), "misalignment {misalignment} accepted");
        }
        // SAFETY: a zero-length buffer never reads its data pointer.
        let empty = unsafe {
            api::array_buffer_new_external(
                cx,
                realm,
                base.wrapping_add(1),
                0,
                api::ExternalOwner::None,
            )
        }
        .unwrap();
        assert_eq!(api::array_buffer_byte_length(empty), 0);
        // SAFETY: `words` is 32 aligned bytes, live until the end of this block, and only this
        // buffer reaches them; it is detached before `words` goes out of scope.
        let aligned = unsafe {
            api::array_buffer_new_external(cx, realm, base, 32, api::ExternalOwner::None)
        }
        .unwrap();
        assert_eq!(api::array_buffer_data_ptr(aligned), Some(base));
        assert!(api::array_buffer_detach(aligned));
    }
    cx.drop();
}

// --- CompileStringsKind::Wasm, consume_stream, set_wasm_enabled, interrupts ---

#[test]
fn csp_is_asked_about_wasm_compilation() {
    let (cx, recorder) = new_context();
    {
        handle_scope_guard!(cx);
        assert!(embed::can_compile_wasm(cx).unwrap());
        recorder.block_compilation.set(true);
        assert!(!embed::can_compile_wasm(cx).unwrap());
        assert_eq!(*recorder.compile_kinds.borrow(), vec!["Wasm", "Wasm"]);

        // The generic check names it in its EvalError
        // Inside a realm: throwing needs a current realm
        let message = embed::with_realm(cx, cx.initial_realm(), |cx| {
            let code = cx.names.empty_string().as_string();
            let error = embed::ensure_can_compile_strings(
                cx,
                embed::CompileStringsKind::Wasm,
                code,
                &[],
                None,
                &[],
                cx.undefined(),
            )
            .unwrap_err();
            to_string(cx, embed::exception_value(error))
                .unwrap()
                .format()
                .unwrap()
        });
        assert!(message.contains("WebAssembly compilation blocked by CSP"), "{message}");
    }
    cx.drop();

    // Without a host, always allowed
    let cx = ContextBuilder::new().build().unwrap();
    assert!(embed::can_compile_wasm(cx).unwrap());
    cx.drop();
}

#[test]
fn consume_stream_rejects_by_default() {
    let cx = ContextBuilder::new().build().unwrap();
    embed::install_host(cx, Box::new(DefaultHost)).unwrap();
    {
        handle_scope_guard!(cx);
        let response = cx.undefined();
        assert!(!embed::host(cx).consume_stream(cx, response, 1).unwrap());
    }
    cx.drop();
}

#[test]
fn wasm_is_enabled_by_default_once_a_host_is_installed() {
    let cx = ContextBuilder::new().build().unwrap();
    assert!(!embed::wasm_enabled(cx), "no host: off");
    embed::install_host(cx, Box::new(DefaultHost)).unwrap();
    assert!(embed::wasm_enabled(cx));
    embed::set_wasm_enabled(cx, false);
    assert!(!embed::wasm_enabled(cx));
    embed::set_wasm_enabled(cx, true);
    assert!(embed::wasm_enabled(cx));
    cx.drop();
}

#[test]
fn interrupt_wrappers_poll_and_run_the_host_hook() {
    let (mut cx, recorder) = new_context();
    {
        handle_scope_guard!(cx);
        let handle = embed::interrupt_handle(cx);
        recorder.continue_on_interrupt.set(true);
        assert!(!cx.vm().interrupt_requested());
        assert!(cx.vm().run_interrupt().is_ok(), "nothing requested still asks the host");

        handle.request();
        assert!(cx.vm().interrupt_requested());
        assert!(cx.vm().run_interrupt().is_ok());
        assert!(!cx.vm().interrupt_requested(), "the request was cleared");

        handle.request();
        recorder.continue_on_interrupt.set(false);
        let result = cx.vm().run_interrupt();
        assert!(matches!(result, Err(error) if embed::is_uncatchable(&error)));
        assert!(!cx.vm().interrupt_requested());
        assert_eq!(recorder.interrupt_calls.get(), 3);
    }
    cx.drop();
}
