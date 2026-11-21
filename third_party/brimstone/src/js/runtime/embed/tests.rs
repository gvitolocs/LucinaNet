//! Tests for the embedding API, using a host that records every call.
//!
//! With the `gc_stress_test` feature every test runs with a GC on every allocation.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::c_void,
    rc::Rc,
};

use crate::{
    common::{constants::DEFAULT_MIN_HEAP_SIZE, options::OptionsBuilder},
    handle_scope_guard,
    runtime::{
        Context, ContextBuilder, EvalResult, Handle, HeapPtr, PropertyDescriptor, PropertyFlags,
        PropertyKey, Realm, Value,
        abstract_operations::{define_property_or_throw, get},
        embed::{
            self, GcHookCx, Host, HostFlags, HostObjectRef, NativeCall, RootVisitor, WeakUpdater,
        },
        gc::Heap,
        intrinsics::intrinsics::Intrinsic,
        object_value::ObjectValue,
        ordinary_object::{ordinary_get, ordinary_object_create},
        type_utilities::to_string,
    },
};

/// Everything the test host saw, shared between the test and the installed host.
#[derive(Default)]
struct Recorder {
    /// Root slots in the value encoding, reported by `trace_roots`.
    value_roots: RefCell<Vec<u64>>,
    /// Root slots holding raw cell pointers, reported by `trace_roots`.
    cell_roots: RefCell<Vec<*mut c_void>>,
    /// Extra fields reported by `trace_host_object`, by host object address.
    traced_fields: RefCell<HashMap<usize, Vec<u64>>>,
    /// Number of `trace_host_object` calls.
    trace_calls: Cell<usize>,
    /// Weak slots updated by `after_gc_weak`.
    weak_cells: RefCell<Vec<*mut c_void>>,
    /// Finalized host objects: (address, raw bits of reserved slot 0).
    finalized: RefCell<Vec<(usize, u64)>>,
    /// Promise jobs handed to the host, in order. Rooted as cell roots until run.
    jobs: RefCell<Vec<*mut c_void>>,
    /// Number of resolve hook calls, by key.
    resolve_calls: RefCell<HashMap<String, usize>>,
    /// Log of proxy hook calls.
    proxy_calls: RefCell<Vec<String>>,
    /// Realm observed by the last native call.
    native_realm: Cell<Option<HeapPtr<Realm>>>,
    /// Number of `interrupt` calls.
    interrupt_calls: Cell<usize>,
    /// What `interrupt` answers.
    continue_on_interrupt: Cell<bool>,
    /// Module sources by specifier, compiled on demand by `resolve_module`.
    module_sources: RefCell<HashMap<String, String>>,
    /// Compiled module records by specifier, rooted as cells.
    module_records: RefCell<HashMap<String, *mut c_void>>,
    /// Specifiers passed to `resolve_module`, in order.
    resolved_specifiers: RefCell<Vec<String>>,
    /// Whether `dynamic_import` takes over imports.
    defer_dynamic_imports: Cell<bool>,
    /// Dynamic imports taken over: (specifier, rooted capability cell).
    pending_imports: RefCell<Vec<(String, *mut c_void)>>,
    /// Whether `can_compile_strings` blocks compilation.
    block_string_compilation: Cell<bool>,
    /// `can_compile_strings` calls as "kind|code|parameters|body".
    compile_string_calls: RefCell<Vec<String>>,
    /// `promise_rejection` calls as "operation reason".
    rejections: RefCell<Vec<String>>,
    /// Whether `finalize_host_object` asks for a GC.
    gc_in_finalizer: Cell<bool>,
    /// Whether `finalize_host_object` allocates on the JS heap.
    alloc_in_finalizer: Cell<bool>,
    /// Whether `finalize_host_object` creates a host object.
    host_object_in_finalizer: Cell<bool>,
}

struct TestHost(Rc<Recorder>);

/// A native implemented by the test host. Returns Err(value) to throw.
/// Returns Err(Some(value)) to throw and Err(None) to terminate uncatchably.
type TestNative = fn(Context, &mut NativeCall) -> Result<(), Option<Value>>;

const PROXY_HANDLER: *const c_void = 0x1000 as *const c_void;
const TEST_CLASS: *const c_void = 0x2000 as *const c_void;

impl Host for TestHost {
    fn trace_roots(&self, _: GcHookCx<'_>, visitor: &mut dyn RootVisitor) {
        for slot in self.0.value_roots.borrow_mut().iter_mut() {
            visitor.value(slot);
        }

        for slot in self.0.cell_roots.borrow_mut().iter_mut() {
            visitor.cell(slot);
        }

        for slot in self.0.jobs.borrow_mut().iter_mut() {
            visitor.cell(slot);
        }

        for slot in self.0.module_records.borrow_mut().values_mut() {
            visitor.cell(slot);
        }

        for (_, slot) in self.0.pending_imports.borrow_mut().iter_mut() {
            visitor.cell(slot);
        }
    }

    fn trace_host_object(
        &self,
        _: GcHookCx<'_>,
        object: HostObjectRef,
        visitor: &mut dyn RootVisitor,
    ) {
        self.0.trace_calls.set(self.0.trace_calls.get() + 1);

        if let Some(fields) = self
            .0
            .traced_fields
            .borrow_mut()
            .get_mut(&(object.as_ptr() as usize))
        {
            for field in fields {
                visitor.value(field);
            }
        }
    }

    fn finalize_host_object(&self, _: GcHookCx<'_>, object: HostObjectRef) {
        let slot_0 = object.reserved_slot(0);
        self.0
            .finalized
            .borrow_mut()
            .push((object.as_ptr() as usize, slot_0.as_raw_bits()));

        // The switches below test the run-time refusals for a host that breaks the rule anyway. The
        // hook's `GcHookCx` has no way to the allocator, so such a host has to recover a `Context`
        // from a pointer it holds, as a host with a stored context would (docs/gc-hooks-cannot-allocate.md).
        let cx = embed::context_of(object.as_ptr());

        if self.0.gc_in_finalizer.get() {
            embed::gc(cx);
        }

        if self.0.alloc_in_finalizer.get() {
            // No handle is created and the memory is never read. With `gc_stress_test` the
            // allocation asks for a GC.
            let small = Heap::alloc_uninit_with_size::<u64>(cx, 64);
            assert!(small.is_ok());

            // Does not fit in the semispace, so it would need a (growing) GC
            #[cfg(feature = "alloc_error")]
            {
                let size = embed::heap_stats(cx).semispace_capacity_bytes + 8;
                assert!(Heap::alloc_uninit_with_size::<u64>(cx, size).is_err());
            }
        }

        if self.0.host_object_in_finalizer.get() {
            // Refused: an error with `alloc_error`, else a panic
            let realm = cx.initial_realm();
            let result = embed::new_host_object(
                cx,
                realm,
                TEST_CLASS,
                HostFlags::empty(),
                None,
                0,
                std::ptr::null(),
                cx.undefined(),
            );
            assert!(result.is_err());
        }
    }

    fn after_gc_weak(&self, _: GcHookCx<'_>, updater: &mut dyn WeakUpdater) {
        for slot in self.0.weak_cells.borrow_mut().iter_mut() {
            updater.update(slot);
        }
    }

    fn call_native(
        &self,
        cx: Context,
        native: *const c_void,
        call: &mut NativeCall,
    ) -> EvalResult<()> {
        self.0.native_realm.set(Some(cx.current_realm_ptr()));

        // SAFETY: Every native created by these tests is a `TestNative`.
        let native = unsafe { std::mem::transmute::<*const c_void, TestNative>(native) };
        native(cx, call).map_err(|error| match error {
            Some(value) => embed::throw_value(value.to_handle(cx)),
            None => embed::uncatchable(),
        })
    }

    fn call_host_object(
        &self,
        cx: Context,
        _: Handle<ObjectValue>,
        _: Handle<Value>,
        arguments: &[Handle<Value>],
        _: Option<Handle<ObjectValue>>,
    ) -> EvalResult<Handle<Value>> {
        Ok(cx.number(arguments.len() as f64 * 10.0))
    }

    fn resolve(
        &self,
        cx: Context,
        object: Handle<ObjectValue>,
        key: Handle<PropertyKey>,
    ) -> EvalResult<bool> {
        let name = key_name(cx, key);
        *self
            .0
            .resolve_calls
            .borrow_mut()
            .entry(name.clone())
            .or_default() += 1;

        if name == "lazyValue" {
            let desc = PropertyDescriptor::data(
                cx.number(99.0),
                PropertyFlags::from_data_attributes(true, true, true),
            );
            define_property_or_throw(cx, object, key, desc)?;
            return Ok(true);
        }

        Ok(false)
    }

    fn resolve_module(
        &self,
        cx: Context,
        _: Option<Handle<embed::SourceTextModule>>,
        _: Handle<crate::runtime::string_value::FlatString>,
        specifier: Handle<crate::runtime::string_value::FlatString>,
        _: Option<Handle<embed::ImportAttributes>>,
    ) -> EvalResult<Option<Handle<embed::SourceTextModule>>> {
        let specifier = specifier.to_string();
        self.0
            .resolved_specifiers
            .borrow_mut()
            .push(specifier.clone());
        test_module(cx, &self.0, &specifier).map(Some)
    }

    fn dynamic_import(
        &self,
        _: Context,
        _: Option<Handle<embed::SourceTextModule>>,
        _: Handle<crate::runtime::string_value::FlatString>,
        specifier: Handle<crate::runtime::string_value::FlatString>,
        _: Option<Handle<embed::ImportAttributes>>,
        import: embed::DynamicImport,
    ) -> EvalResult<bool> {
        if !self.0.defer_dynamic_imports.get() {
            return Ok(false);
        }

        self.0
            .pending_imports
            .borrow_mut()
            .push((specifier.to_string(), import.cell_ptr()));
        Ok(true)
    }

    fn populate_import_meta(
        &self,
        mut cx: Context,
        module: Handle<embed::SourceTextModule>,
        meta: Handle<ObjectValue>,
    ) -> EvalResult<()> {
        let url = embed::module_file_name(module).as_string();
        let url_key = key(cx, "url");
        let _ = &mut cx;
        define_property_or_throw(
            cx,
            meta,
            url_key,
            PropertyDescriptor::data(
                url.as_value(),
                PropertyFlags::from_data_attributes(true, true, true),
            ),
        )
    }

    fn can_compile_strings(
        &self,
        _: Context,
        kind: embed::CompileStringsKind,
        code: Handle<crate::runtime::string_value::StringValue>,
        parameters: &[Handle<crate::runtime::string_value::StringValue>],
        body: Option<Handle<crate::runtime::string_value::StringValue>>,
        _parameter_args: &[Handle<Value>],
        _body_arg: Handle<Value>,
    ) -> EvalResult<bool> {
        let parameters: Vec<String> = parameters
            .iter()
            .map(|parameter| parameter.format().unwrap())
            .collect();
        self.0.compile_string_calls.borrow_mut().push(format!(
            "{kind:?}|{}|{}|{}",
            code.format().unwrap(),
            parameters.join(","),
            body.map(|s| s.format().unwrap()).unwrap_or_default()
        ));
        Ok(!self.0.block_string_compilation.get())
    }

    fn promise_rejection(
        &self,
        _: Context,
        promise: HeapPtr<embed::PromiseObject>,
        operation: embed::PromiseRejectionOperation,
    ) {
        // Must not allocate: only read the rejection reason, a small integer in these tests
        let reason = promise
            .rejected_value()
            .map_or(-1, |reason| reason.as_smi());
        self.0
            .rejections
            .borrow_mut()
            .push(format!("{operation:?} {reason}"));
    }

    fn interrupt(&self, _: Context) -> bool {
        self.0.interrupt_calls.set(self.0.interrupt_calls.get() + 1);
        self.0.continue_on_interrupt.get()
    }

    fn enqueue_job(
        &self,
        _: Context,
        job: HeapPtr<ObjectValue>,
        _: Option<HeapPtr<ObjectValue>>,
        _: Option<HeapPtr<ObjectValue>>,
    ) -> bool {
        self.0.jobs.borrow_mut().push(job.as_ptr().cast());
        true
    }

    fn proxy_get_own_property(
        &self,
        cx: Context,
        handler: *const c_void,
        _: Handle<ObjectValue>,
        key: Handle<PropertyKey>,
    ) -> EvalResult<Option<PropertyDescriptor>> {
        assert_eq!(handler, PROXY_HANDLER);
        let name = key_name(cx, key);
        self.0
            .proxy_calls
            .borrow_mut()
            .push(format!("getOwnProperty {name}"));

        if name == "magic" {
            let flags = PropertyFlags::from_data_attributes(false, true, true);
            return Ok(Some(PropertyDescriptor::data(cx.number(42.0), flags)));
        }

        Ok(None)
    }

    fn proxy_define_property(
        &self,
        cx: Context,
        _: *const c_void,
        _: Handle<ObjectValue>,
        key: Handle<PropertyKey>,
        _: PropertyDescriptor,
    ) -> EvalResult<bool> {
        let name = key_name(cx, key);
        self.0
            .proxy_calls
            .borrow_mut()
            .push(format!("defineProperty {name}"));
        Ok(false)
    }

    fn proxy_own_keys(
        &self,
        mut cx: Context,
        _: *const c_void,
        _: Handle<ObjectValue>,
    ) -> EvalResult<Vec<Handle<Value>>> {
        self.0.proxy_calls.borrow_mut().push("ownKeys".to_owned());
        Ok(vec![cx.alloc_string("magic")?.as_value()])
    }

    fn proxy_get(
        &self,
        cx: Context,
        handler: *const c_void,
        object: Handle<ObjectValue>,
        key: Handle<PropertyKey>,
        receiver: Handle<Value>,
    ) -> EvalResult<Handle<Value>> {
        let name = key_name(cx, key);
        self.0.proxy_calls.borrow_mut().push(format!("get {name}"));

        if name == "magic" {
            return Ok(cx.number(42.0));
        }

        assert_eq!(handler, PROXY_HANDLER);
        ordinary_get(cx, object, key, receiver)
    }
}

fn key_name(cx: Context, key: Handle<PropertyKey>) -> String {
    let value = key.to_value(cx).unwrap();
    if value.is_string() {
        value.as_string().format().unwrap()
    } else {
        String::from("<symbol>")
    }
}

fn new_context() -> (Context, Rc<Recorder>) {
    new_context_with_heap_size(DEFAULT_MIN_HEAP_SIZE)
}

fn new_context_with_heap_size(min_heap_size: usize) -> (Context, Rc<Recorder>) {
    new_context_with_options(OptionsBuilder::new().min_heap_size(min_heap_size))
}

fn new_context_with_options(options: OptionsBuilder) -> (Context, Rc<Recorder>) {
    let recorder = Rc::new(Recorder::default());

    let options = options.build().unwrap();

    #[allow(unused_mut)]
    let mut cx = ContextBuilder::new()
        .set_options(Rc::new(options))
        .build()
        .unwrap();
    embed::install_host(cx, Box::new(TestHost(recorder.clone()))).unwrap();

    #[cfg(feature = "gc_stress_test")]
    cx.enable_gc_stress_test();

    (cx, recorder)
}

fn key(mut cx: Context, name: &str) -> Handle<PropertyKey> {
    let string = cx.alloc_string(name).unwrap();
    PropertyKey::string_handle(cx, string).unwrap()
}

fn eval(cx: Context, realm: Handle<Realm>, source: &str) -> Handle<Value> {
    match embed::evaluate_script(cx, realm, source, "embed_test.js", 1) {
        Ok(value) => value,
        Err(error) => {
            let error = embed::exception_value(error);
            let message = embed::with_realm(cx, realm, |cx| to_string(cx, error).unwrap().format());
            panic!("script threw: {}", message.unwrap())
        }
    }
}

fn eval_string(cx: Context, realm: Handle<Realm>, source: &str) -> String {
    let value = eval(cx, realm, source);
    embed::with_realm(cx, realm, |cx| to_string(cx, value).unwrap().format().unwrap())
}

fn get_number(cx: Context, object: Handle<ObjectValue>, name: &str) -> f64 {
    let key = key(cx, name);
    get(cx, object, key).unwrap().as_number()
}

fn define_global(cx: Context, realm: Handle<Realm>, name: &str, value: Handle<Value>) {
    let global = realm.global_object().as_object();
    let key = key(cx, name);
    let desc =
        PropertyDescriptor::data(value, PropertyFlags::from_data_attributes(true, true, true));
    define_property_or_throw(cx, global, key, desc).unwrap();
}

fn new_test_host_object(
    cx: Context,
    flags: HostFlags,
    num_reserved_slots: u32,
) -> Handle<ObjectValue> {
    let realm = cx.initial_realm();
    let proto = realm.get_intrinsic(Intrinsic::ObjectPrototype);
    embed::new_host_object(
        cx,
        realm,
        TEST_CLASS,
        flags,
        Some(proto),
        num_reserved_slots,
        std::ptr::null(),
        cx.undefined(),
    )
    .unwrap()
}

/// Create an ordinary object `{ marker: <marker> }` and return its raw value. The only handle to
/// it is released before returning.
fn new_marker_object(cx: Context, marker: f64) -> Value {
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    embed::with_realm(cx, realm, |cx| {
        let object = ordinary_object_create(cx).unwrap();
        let key = key(cx, "marker");
        let desc = PropertyDescriptor::data(
            cx.number(marker),
            PropertyFlags::from_data_attributes(true, true, true),
        );
        define_property_or_throw(cx, object, key, desc).unwrap();
        *object.as_value()
    })
}

fn marker_of(cx: Context, value: Value) -> f64 {
    handle_scope_guard!(cx);
    get_number(cx, value.as_object().to_handle(), "marker")
}

#[test]
fn host_object_does_not_move_but_its_slot_target_does() {
    let (cx, recorder) = new_context();

    let host_object = {
        handle_scope_guard!(cx);
        let host_object = new_test_host_object(cx, HostFlags::empty(), 1);
        *host_object
    };
    let host_address = host_object.as_ptr() as usize;

    // The host object is only reachable through a host root, the ordinary object only through the
    // host object's reserved slot.
    recorder
        .cell_roots
        .borrow_mut()
        .push(host_object.as_ptr().cast());
    embed::set_reserved_slot(host_object, 0, new_marker_object(cx, 7.0));

    let mut previous_slot_address = embed::reserved_slot(host_object, 0).as_raw_bits();

    for _ in 0..1000 {
        embed::gc(cx);

        assert_eq!(recorder.cell_roots.borrow()[0] as usize, host_address);
        assert!(embed::is_host_object(host_object));

        let slot = embed::reserved_slot(host_object, 0);
        assert_ne!(slot.as_raw_bits(), previous_slot_address, "slot target was not moved");
        assert!(!embed::is_host_object(slot.as_object()));
        previous_slot_address = slot.as_raw_bits();
    }

    assert_eq!(marker_of(cx, embed::reserved_slot(host_object, 0)), 7.0);
    assert!(recorder.finalized.borrow().is_empty());

    cx.drop();
}

/// A finalizer that asks for a GC (directly or by allocating) runs inside the GC that finalizes it.
/// That GC must not nest: a nested one would not see the outer GC's to-space, so it would finalize
/// and free (poison with 0xdb in debug builds) a host object reachable only through a JS object.
fn check_gc_from_finalizer(configure: impl Fn(&Recorder)) {
    let (cx, recorder) = new_context();
    configure(&recorder);

    let (dead_address, survivor) = {
        handle_scope_guard!(cx);
        let realm = cx.initial_realm();

        let dead = new_test_host_object(cx, HostFlags::HAS_FINALIZE, 1);
        embed::set_reserved_slot(*dead, 0, Value::smi(1));

        // Reachable only through a property of the global object
        let survivor = new_test_host_object(cx, HostFlags::HAS_FINALIZE, 1);
        embed::set_reserved_slot(*survivor, 0, Value::smi(4321));
        define_global(cx, realm, "survivor", survivor.as_value());

        (dead.as_ptr() as usize, *survivor)
    };

    embed::gc(cx);
    embed::gc(cx);

    let finalized = recorder.finalized.borrow().clone();
    assert_eq!(finalized, vec![(dead_address, Value::smi(1).as_raw_bits())]);
    assert_eq!(embed::reserved_slot(survivor, 0).as_smi(), 4321);
    assert!(embed::realm_of_object(cx, survivor).ptr_eq(&cx.initial_realm_ptr()));

    {
        handle_scope_guard!(cx);
        let realm = cx.initial_realm();
        let global = eval(cx, realm, "survivor");
        assert_eq!(global.as_object().as_ptr(), survivor.as_ptr());
    }

    // The heap is still consistent: JS runs and allocates, and later GCs collect normally
    {
        handle_scope_guard!(cx);
        let realm = cx.initial_realm();
        let result =
            eval(cx, realm, "let a = []; for (let i = 0; i < 10000; i++) a.push({ i }); a.length");
        assert_eq!(result.as_number(), 10000.0);
    }
    embed::gc(cx);
    assert_eq!(embed::reserved_slot(survivor, 0).as_smi(), 4321);

    cx.drop();
}

#[test]
fn gc_from_finalizer_does_not_nest() {
    check_gc_from_finalizer(|recorder| recorder.gc_in_finalizer.set(true));
}

#[test]
fn allocation_in_finalizer_does_not_collect() {
    // Obsolete name: an allocation on the JS heap from a GC hook is fatal, not merely
    // non-collecting. See `allocation_in_finalizer_aborts_naming_the_hook` below.
}

#[test]
fn finalizer_runs_once_for_dead_host_objects_only() {
    let (cx, recorder) = new_context();

    let flags = HostFlags::HAS_FINALIZE;
    let (dead_address, live_object) = {
        handle_scope_guard!(cx);

        let dead = new_test_host_object(cx, flags, 2);
        embed::set_reserved_slot(*dead, 0, Value::smi(1234));
        embed::set_reserved_slot(*dead, 1, new_marker_object(cx, 1.0));

        let live = new_test_host_object(cx, flags, 1);
        embed::set_reserved_slot(*live, 0, Value::smi(5678));

        (dead.as_ptr() as usize, *live)
    };

    recorder
        .cell_roots
        .borrow_mut()
        .push(live_object.as_ptr().cast());

    for _ in 0..5 {
        embed::gc(cx);
    }

    let finalized = recorder.finalized.borrow().clone();
    assert_eq!(finalized, vec![(dead_address, Value::smi(1234).as_raw_bits())]);
    assert_eq!(embed::host_object_count(cx), 1);
    assert_eq!(embed::reserved_slot(live_object, 0).as_smi(), 5678);

    // Destroying the context finalizes the remaining host objects
    let live_address = live_object.as_ptr() as usize;
    cx.drop();

    let finalized = recorder.finalized.borrow().clone();
    assert_eq!(finalized.len(), 2);
    assert_eq!(finalized[1], (live_address, Value::smi(5678).as_raw_bits()));
}

#[test]
fn root_slots_keep_objects_alive_and_are_updated() {
    let (cx, recorder) = new_context();

    // Each object is rooted right after it is created, since any allocation may run a GC
    let value = new_marker_object(cx, 11.0);
    recorder.value_roots.borrow_mut().push(value.as_raw_bits());
    recorder.value_roots.borrow_mut().push(0); // Uninitialized slots are skipped
    recorder
        .value_roots
        .borrow_mut()
        .push(Value::smi(3).as_raw_bits());
    let cell = new_marker_object(cx, 22.0);
    recorder
        .cell_roots
        .borrow_mut()
        .push(cell.as_pointer().as_ptr().cast());

    let before = (recorder.value_roots.borrow()[0], recorder.cell_roots.borrow()[0] as usize);
    embed::gc(cx);

    let value = Value::from_raw_bits(recorder.value_roots.borrow()[0]);
    let cell_ptr = recorder.cell_roots.borrow()[0];
    assert_ne!(value.as_raw_bits(), before.0);
    assert_ne!(cell_ptr as usize, before.1);
    assert_eq!(recorder.value_roots.borrow()[1], 0);
    assert_eq!(recorder.value_roots.borrow()[2], Value::smi(3).as_raw_bits());

    assert_eq!(marker_of(cx, value), 11.0);
    let cell_value = Value::heap_item(HeapPtr::from_ptr(cell_ptr.cast()));
    assert_eq!(marker_of(cx, cell_value), 22.0);

    cx.drop();
}

#[test]
fn trace_hook_reports_host_object_fields() {
    let (cx, recorder) = new_context();

    let host_object = {
        handle_scope_guard!(cx);
        *new_test_host_object(cx, HostFlags::HAS_TRACE, 0)
    };
    recorder
        .cell_roots
        .borrow_mut()
        .push(host_object.as_ptr().cast());

    let field = new_marker_object(cx, 33.0);
    recorder
        .traced_fields
        .borrow_mut()
        .insert(host_object.as_ptr() as usize, vec![field.as_raw_bits()]);

    for _ in 0..3 {
        embed::gc(cx);
    }

    assert!(recorder.trace_calls.get() >= 3);
    let field = recorder.traced_fields.borrow()[&(host_object.as_ptr() as usize)][0];
    assert_eq!(marker_of(cx, Value::from_raw_bits(field)), 33.0);

    cx.drop();
}

#[test]
fn weak_pointers_are_cleared_moved_or_kept() {
    let (cx, recorder) = new_context();

    // Each object is registered right after it is created, since any allocation may run a GC.
    // The live ordinary object and the live host object also get strong roots.
    let dead = new_marker_object(cx, 1.0);
    recorder
        .weak_cells
        .borrow_mut()
        .push(dead.as_pointer().as_ptr().cast());

    let live = new_marker_object(cx, 2.0);
    recorder.value_roots.borrow_mut().push(live.as_raw_bits());
    recorder
        .weak_cells
        .borrow_mut()
        .push(live.as_pointer().as_ptr().cast());

    let (host_object, dead_host_address) = {
        handle_scope_guard!(cx);
        let host_object = *new_test_host_object(cx, HostFlags::empty(), 0);
        recorder
            .cell_roots
            .borrow_mut()
            .push(host_object.as_ptr().cast());
        let dead_host_object = *new_test_host_object(cx, HostFlags::HAS_FINALIZE, 1);
        (host_object, dead_host_object.as_ptr() as usize)
    };

    recorder.weak_cells.borrow_mut().extend([
        host_object.as_ptr().cast(),
        dead_host_address as *mut c_void,
        std::ptr::null_mut(),
    ]);

    let live_before = recorder.value_roots.borrow()[0];
    embed::gc(cx);

    let weak_cells = recorder.weak_cells.borrow().clone();
    let live_now = Value::from_raw_bits(recorder.value_roots.borrow()[0]);

    assert!(weak_cells[0].is_null(), "dead ordinary object was not cleared");
    assert_eq!(weak_cells[1], live_now.as_pointer().as_ptr().cast());
    assert_ne!(live_now.as_raw_bits(), live_before, "live object did not move");
    assert_eq!(weak_cells[2], host_object.as_ptr().cast());
    assert!(weak_cells[3].is_null(), "dead host object was not cleared");
    assert!(weak_cells[4].is_null());

    assert_eq!(recorder.finalized.borrow().len(), 1);
    assert_eq!(recorder.finalized.borrow()[0].0, dead_host_address);
    assert_eq!(marker_of(cx, live_now), 2.0);

    cx.drop();
}

fn native_sum(cx: Context, call: &mut NativeCall) -> Result<(), Option<Value>> {
    handle_scope_guard!(cx);

    let callee = call.callee().as_object().to_handle();
    let mut sum = embed::function_reserved(cx, callee, 0).as_number();

    let this = call.this();
    if this.is_object() {
        sum += get_number(cx, this.as_object().to_handle(), "tag");
    }

    for index in 0..call.argc() {
        sum += call.arg(index).as_number();
    }

    call.set_return_value(Value::number(sum));
    Ok(())
}

fn native_throw(_: Context, call: &mut NativeCall) -> Result<(), Option<Value>> {
    Err(Some(Value::smi(call.argc() as i32 + 6)))
}

fn native_construct(cx: Context, call: &mut NativeCall) -> Result<(), Option<Value>> {
    handle_scope_guard!(cx);

    if !call.is_constructing() {
        call.set_return_value(Value::smi(-1));
        return Ok(());
    }

    let object = ordinary_object_create(cx).unwrap();
    let value = call.arg(0).to_handle(cx);
    let key = key(cx, "value");
    define_property_or_throw(
        cx,
        object,
        key,
        PropertyDescriptor::data(value, PropertyFlags::from_data_attributes(true, true, true)),
    )
    .unwrap();

    call.set_return_value(*object.as_value());
    Ok(())
}

fn new_test_native(
    cx: Context,
    realm: Handle<Realm>,
    native: TestNative,
    name: &str,
    constructor: bool,
) -> Handle<ObjectValue> {
    let name = key(cx, name);
    embed::new_native_function(cx, realm, native as *const c_void, 1, Some(name), constructor, 2)
        .unwrap()
}

#[test]
fn natives_are_callable_throw_and_construct() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();

    let sum = new_test_native(cx, realm, native_sum, "sum", false);
    embed::set_function_reserved(cx, sum, 0, cx.number(1000.0)).unwrap();
    let thrower = new_test_native(cx, realm, native_throw, "thrower", false);
    let ctor = new_test_native(cx, realm, native_construct, "Ctor", true);

    assert_eq!(embed::native_of(cx, sum), Some(native_sum as *const c_void));
    assert_eq!(embed::native_of(cx, realm.get_intrinsic(Intrinsic::ObjectPrototype)), None);

    define_global(cx, realm, "sum", sum.as_value());
    define_global(cx, realm, "thrower", thrower.as_value());
    define_global(cx, realm, "Ctor", ctor.as_value());

    let result = eval_string(
        cx,
        realm,
        r#"
        var out = [];
        out.push(typeof sum, sum.name, sum.length);
        out.push(sum.call({ tag: 100 }, 1, 2, 3));
        out.push(sum(5));
        try { thrower(1); out.push("no throw"); } catch (e) { out.push("caught " + e); }
        var o = new Ctor(5);
        out.push(o.value, Ctor(5));
        try { new sum(); out.push("no throw"); } catch (e) { out.push(e instanceof TypeError); }
        out.join(",");
        "#,
    );

    assert_eq!(result, "function,sum,1,1106,1005,caught 7,5,-1,true");
    assert!(recorder.native_realm.get().is_some());

    cx.drop();
}

#[test]
fn proxy_host_object_answers_internal_methods() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();

    let proxy = embed::new_host_object(
        cx,
        realm,
        TEST_CLASS,
        HostFlags::empty(),
        Some(realm.get_intrinsic(Intrinsic::ObjectPrototype)),
        1,
        PROXY_HANDLER,
        cx.number(5.0),
    )
    .unwrap();

    assert_eq!(embed::host_handler(*proxy), Some(PROXY_HANDLER));
    assert_eq!(embed::host_class(*proxy), Some(TEST_CLASS));
    assert_eq!(embed::proxy_private(*proxy).as_number(), 5.0);

    define_global(cx, realm, "p", proxy.as_value());

    let result = eval_string(
        cx,
        realm,
        r#"
        "use strict";
        var out = [];
        out.push(Object.keys(p).join("+"));
        out.push(p.magic);
        out.push("magic" in p, "other" in p, "toString" in p);
        try { p.x = 1; out.push("no throw"); } catch (e) { out.push(e instanceof TypeError); }
        out.push(p.x);
        out.push(typeof p);
        out.join(",");
        "#,
    );

    assert_eq!(result, "magic,42,true,false,true,true,,object");

    let calls = recorder.proxy_calls.borrow();
    assert!(calls.contains(&"ownKeys".to_owned()));
    assert!(calls.contains(&"get magic".to_owned()));
    assert!(calls.contains(&"defineProperty x".to_owned()));

    drop(calls);
    cx.drop();
}

#[test]
fn callable_host_objects_are_functions() {
    let (cx, _) = new_context();
    let realm = cx.initial_realm();

    let callable = embed::new_host_object(
        cx,
        realm,
        TEST_CLASS,
        HostFlags::CALLABLE | HostFlags::CONSTRUCTOR,
        Some(realm.get_intrinsic(Intrinsic::FunctionPrototype)),
        0,
        std::ptr::null(),
        cx.undefined(),
    )
    .unwrap();
    let plain = new_test_host_object(cx, HostFlags::empty(), 0);

    define_global(cx, realm, "callable", callable.as_value());
    define_global(cx, realm, "plain", plain.as_value());

    let result = eval_string(
        cx,
        realm,
        r#"
        var out = [typeof callable, typeof plain, callable(1, 2), callable.call(null, 1)];
        try { new callable(); } catch (e) { out.push(e instanceof TypeError); }
        try { plain(); } catch (e) { out.push(e instanceof TypeError); }
        out.join(",");
        "#,
    );

    // `new` must produce an object, the test host returns a number so it throws
    assert_eq!(result, "function,object,20,10,true,true");

    cx.drop();
}

/// An `IS_HTMLDDA` host object is what a glue layer needs for `document.all` (ECMA-262 B.3.6).
#[test]
fn htmldda_host_objects_emulate_undefined() {
    let (cx, _) = new_context();
    let realm = cx.initial_realm();

    let htmldda = new_test_host_object(cx, HostFlags::IS_HTMLDDA, 0);
    let plain = new_test_host_object(cx, HostFlags::empty(), 0);

    assert!(
        embed::host_flags(*htmldda)
            .unwrap()
            .contains(HostFlags::IS_HTMLDDA)
    );
    assert!(htmldda.is_htmldda());
    assert!(!plain.is_htmldda(), "only the flagged object carries the slot");

    define_global(cx, realm, "all", htmldda.as_value());
    define_global(cx, realm, "plain", plain.as_value());

    let result = eval_string(
        cx,
        realm,
        r#"
        var out = [typeof all, typeof plain, String(all == null), String(all == undefined),
                   String(all === undefined), String(Boolean(all)), String(!all),
                   String(all ?? "fallback"), String(all || "fallback"), String(plain == null)];
        out.join("|");
        "#,
    );
    assert_eq!(
        result,
        "undefined|object|true|true|false|false|true|[object Object]|fallback|false"
    );

    cx.drop();
}

#[test]
fn resolve_hook_defines_global_property_lazily() {
    let (cx, recorder) = new_context();

    let (realm, global) =
        embed::new_global(cx, TEST_CLASS, HostFlags::HAS_RESOLVE | HostFlags::HAS_FINALIZE, 1)
            .unwrap();
    assert!(embed::is_host_object(*global));
    assert_eq!(embed::host_class(*global), Some(TEST_CLASS));
    assert!(
        embed::host_flags(*global)
            .unwrap()
            .contains(HostFlags::IS_GLOBAL)
    );
    assert!(embed::global_of_realm(*realm).ptr_eq(&global));
    assert!(embed::realm_of_object(cx, *global).ptr_eq(&realm));

    let global_address = global.as_ptr() as usize;

    let result = eval_string(
        cx,
        realm,
        r#"
        var first = lazyValue;
        var second = lazyValue;
        var third = globalThis.lazyValue;
        [first, second, third, typeof missingValue, typeof Object].join(",");
        "#,
    );

    assert_eq!(result, "99,99,99,undefined,function");
    assert_eq!(recorder.resolve_calls.borrow()["lazyValue"], 1);
    assert!(recorder.resolve_calls.borrow()["missingValue"] >= 1);

    // The global never moves
    embed::gc(cx);
    assert_eq!(realm.global_object().as_ptr() as usize, global_address);

    cx.drop();
}

#[test]
fn resolve_hook_on_host_object() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();

    let object = new_test_host_object(cx, HostFlags::HAS_RESOLVE, 0);
    define_global(cx, realm, "lazyObject", object.as_value());

    let result = eval_string(
        cx,
        realm,
        "[lazyObject.lazyValue, lazyObject.lazyValue, 'lazyValue' in lazyObject].join(',')",
    );

    assert_eq!(result, "99,99,true");
    assert_eq!(recorder.resolve_calls.borrow()["lazyValue"], 1);

    cx.drop();
}

const ASYNC_SCRIPT: &str = r#"
    var log = [];
    Promise.resolve(1).then(v => log.push("a" + v)).then(() => log.push("a2"));
    (async function () {
        log.push("b");
        await null;
        log.push("c");
        await Promise.resolve(2);
        log.push("d");
    })();
    Promise.reject(3).catch(e => log.push("e" + e)).finally(() => log.push("f"));
    ({ then(resolve) { log.push("thenable"); resolve(4); } });
    Promise.resolve({ then(resolve) { log.push("t"); resolve(5); } }).then(v => log.push("t" + v));
    log.push("sync");
"#;

#[test]
fn promise_jobs_go_to_the_host_in_order() {
    // Expected output from the engine's own task queue, without a host
    let expected = {
        let mut cx = ContextBuilder::new().build().unwrap();
        let realm = cx.initial_realm();
        embed::with_realm(cx, realm, |cx| {
            embed::evaluate_script(cx, realm, ASYNC_SCRIPT, "async.js", 1).unwrap();
        });
        cx.run_all_tasks().unwrap();
        let log = eval_string(cx, realm, "log.join(',')");
        cx.drop();
        log
    };

    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();
    eval(cx, realm, ASYNC_SCRIPT);

    assert!(!recorder.jobs.borrow().is_empty());
    assert_eq!(eval_string(cx, realm, "log.join(',')"), "b,sync");

    // Run jobs in FIFO order, like the host's microtask checkpoint. Running a job may enqueue more.
    let mut num_jobs = 0;
    loop {
        let job = {
            let mut jobs = recorder.jobs.borrow_mut();
            if jobs.is_empty() {
                break;
            }
            jobs.remove(0)
        };

        handle_scope_guard!(cx);
        let job = HeapPtr::<ObjectValue>::from_ptr(job.cast()).to_handle();
        assert!(job.is_callable());
        embed::call(cx, job.as_value(), cx.undefined(), &[]).unwrap();
        num_jobs += 1;

        // Jobs that already ran do nothing
        embed::call(cx, job.as_value(), cx.undefined(), &[]).unwrap();
        embed::gc(cx);
    }

    assert!(num_jobs >= 8);
    assert_eq!(embed::pending_host_jobs(cx), 0);
    assert_eq!(eval_string(cx, realm, "log.join(',')"), expected);

    cx.drop();
}

#[test]
fn unrun_jobs_are_dropped_with_their_job_object() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();

    eval(cx, realm, "Promise.resolve().then(() => {});");
    assert_eq!(embed::pending_host_jobs(cx), 1);

    // The host drops the job without running it
    recorder.jobs.borrow_mut().clear();
    embed::gc(cx);

    assert_eq!(embed::pending_host_jobs(cx), 0);

    cx.drop();
}

#[test]
fn entered_realm_and_callee_realm() {
    let (mut cx, recorder) = new_context();
    let initial_realm = cx.initial_realm();
    let (other_realm, _) = embed::new_global(cx, TEST_CLASS, HostFlags::empty(), 0).unwrap();

    // A function from the initial realm that creates an object and records the native's realm
    let record = new_test_native(cx, initial_realm, native_throw, "record", false);
    let make_object = eval(cx, initial_realm, "(function () { return {}; })");

    embed::enter_realm(cx, *other_realm);
    {
        handle_scope_guard!(cx);
        assert!(embed::current_realm(cx).ptr_eq(&other_realm));
        assert_eq!(embed::entered_realm_depth(cx), 1);

        // Objects created while the realm is entered belong to it
        let object = ordinary_object_create(cx).unwrap();
        let proto = object.prototype().unwrap();
        assert!(proto.ptr_eq(&other_realm.get_intrinsic_ptr(Intrinsic::ObjectPrototype)));

        let host_object = embed::new_host_object(
            cx,
            embed::current_realm(cx),
            TEST_CLASS,
            HostFlags::empty(),
            None,
            0,
            std::ptr::null(),
            cx.undefined(),
        )
        .unwrap();
        assert!(embed::realm_of_object(cx, *host_object).ptr_eq(&other_realm));

        // A JS call from the entered realm runs in the callee's realm
        let result = embed::call(cx, make_object, cx.undefined(), &[]).unwrap();
        let proto = result.as_object().prototype().unwrap();
        assert!(proto.ptr_eq(&initial_realm.get_intrinsic_ptr(Intrinsic::ObjectPrototype)));

        let _ = embed::call(cx, record.as_value(), cx.undefined(), &[]);
        assert!(recorder.native_realm.get().unwrap().ptr_eq(&initial_realm));

        // Nested entries: the newest wins
        embed::enter_realm(cx, *initial_realm);
        assert!(embed::current_realm(cx).ptr_eq(&initial_realm));
        embed::leave_realm(cx, *initial_realm);

        assert!(embed::current_realm(cx).ptr_eq(&other_realm));
    }
    embed::leave_realm(cx, *other_realm);

    assert_eq!(embed::entered_realm_depth(cx), 0);
    assert!(cx.vm().is_stack_empty());

    cx.drop();
}

#[test]
fn context_of_host_objects_and_ordinary_objects() {
    let (cx, _) = new_context();

    let host_object = new_test_host_object(cx, HostFlags::empty(), 0);
    let ordinary =
        embed::with_realm(cx, cx.initial_realm(), |cx| ordinary_object_create(cx).unwrap());

    let host_cx = embed::context_of(host_object.as_ptr().cast());
    let ordinary_cx = embed::context_of(ordinary.as_ptr().cast());
    assert!(std::ptr::eq(&*host_cx, &*cx));
    assert!(std::ptr::eq(&*ordinary_cx, &*cx));

    // Handles to host objects can be created without a context
    let handle = host_object.as_ptr();
    let handle = HeapPtr::<ObjectValue>::from_ptr(handle).to_handle();
    assert!(handle.ptr_eq(&host_object));

    cx.drop();
}

#[test]
fn weak_map_with_host_object_keys() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();

    let key_object = {
        handle_scope_guard!(cx);
        *new_test_host_object(cx, HostFlags::empty(), 0)
    };
    recorder
        .cell_roots
        .borrow_mut()
        .push(key_object.as_ptr().cast());

    {
        handle_scope_guard!(cx);
        define_global(cx, realm, "hostKey", key_object.to_handle().as_value());
    }

    eval(
        cx,
        realm,
        r#"
        var map = new WeakMap();
        map.set(hostKey, { value: 17 });
        var ref = new WeakRef(hostKey);
        delete globalThis.hostKey;
        "#,
    );

    embed::gc(cx);
    assert_eq!(eval_string(cx, realm, "String(ref.deref() !== undefined)"), "true");

    let result = {
        handle_scope_guard!(cx);
        let map = eval(cx, realm, "map");
        let get_fn = eval(cx, realm, "WeakMap.prototype.get");
        let value = embed::call(cx, get_fn, map, &[key_object.to_handle().as_value()]).unwrap();
        get_number(cx, value.as_object(), "value")
    };
    assert_eq!(result, 17.0);

    // Once the host drops its root, the key dies along with its ephemeron value
    recorder.cell_roots.borrow_mut().clear();
    embed::gc(cx);
    embed::gc(cx);
    assert_eq!(eval_string(cx, realm, "String(ref.deref())"), "undefined");
    assert_eq!(embed::host_object_count(cx), 0);

    cx.drop();
}

// Allocating enough to grow the heap takes too long with a GC on every allocation
#[cfg(not(feature = "gc_stress_test"))]
#[test]
fn growing_gc_keeps_host_objects_in_place() {
    use crate::common::constants::MIN_HEAP_SIZE;

    let (cx, recorder) = new_context_with_heap_size(MIN_HEAP_SIZE);
    let realm = cx.initial_realm();

    let host_object = {
        handle_scope_guard!(cx);
        *new_test_host_object(cx, HostFlags::empty(), 1)
    };
    recorder
        .cell_roots
        .borrow_mut()
        .push(host_object.as_ptr().cast());
    embed::set_reserved_slot(host_object, 0, new_marker_object(cx, 3.0));

    let heap_size_before = cx.heap.heap_size();

    // Allocate enough live data to force the heap to grow
    eval(cx, realm, "var keep = []; for (var i = 0; i < 200000; i++) keep.push({ i });");

    assert!(cx.heap.heap_size() > heap_size_before, "heap did not grow");
    assert_eq!(recorder.cell_roots.borrow()[0], host_object.as_ptr().cast());
    assert_eq!(marker_of(cx, embed::reserved_slot(host_object, 0)), 3.0);

    // Handles and contexts are still found from host object pointers after growing
    let handle = host_object.to_handle();
    assert!(embed::context_of(handle.as_ptr().cast()).heap.heap_size() > heap_size_before);

    cx.drop();
}

#[test]
fn failed_evaluation_returns_the_exception_and_unwinds() {
    let (mut cx, _) = new_context();
    let realm = cx.initial_realm();

    let error = embed::evaluate_script(cx, realm, "var x = 1;\nthrow { code: 42 };", "throw.js", 1)
        .err()
        .expect("script did not throw");
    let exception = embed::exception_value(error);
    assert_eq!(get_number(cx, exception.as_object(), "code"), 42.0);

    let syntax_error = embed::evaluate_script(cx, realm, "var = ;", "syntax.js", 1)
        .err()
        .expect("script did not throw");
    let syntax_error = embed::exception_value(syntax_error);
    let message = embed::with_realm(cx, realm, |cx| to_string(cx, syntax_error).unwrap().format());
    assert!(message.unwrap().starts_with("SyntaxError"));

    assert_eq!(embed::entered_realm_depth(cx), 0);
    assert!(cx.vm().is_stack_empty());

    // Line numbers start at the given line
    let stack = eval_string(cx, realm, "try { null.x } catch (e) { e.stack }");
    assert!(stack.contains("embed_test.js:1"), "{stack}");
    let value =
        embed::evaluate_script(cx, realm, "\n\nnew Error('here').stack", "lines.js", 40).unwrap();
    let stack = embed::with_realm(cx, realm, |cx| to_string(cx, value).unwrap().format().unwrap());
    assert!(stack.contains("lines.js:42"), "{stack}");

    cx.drop();
}

#[test]
fn compile_function_from_source() {
    let (cx, _) = new_context();
    let realm = cx.initial_realm();

    let function = embed::compile_function(
        cx,
        realm,
        Some("add"),
        &["a", "b"],
        "return a + b;",
        "compiled.js",
        10,
    )
    .unwrap();

    let result =
        embed::call(cx, function.as_value(), cx.undefined(), &[cx.number(2.0), cx.number(3.0)])
            .unwrap();
    assert_eq!(result.as_number(), 5.0);

    define_global(cx, realm, "add", function.as_value());
    assert_eq!(eval_string(cx, realm, "add.name + add.length"), "add2");

    let thrower =
        embed::compile_function(cx, realm, None, &[], "\nthrow new Error('x');", "compiled.js", 10)
            .unwrap();
    let error = embed::call(cx, thrower.as_value(), cx.undefined(), &[])
        .err()
        .unwrap();
    let stack = {
        let error = embed::exception_value(error);
        let stack = get(cx, error.as_object(), key(cx, "stack")).unwrap();
        embed::with_realm(cx, realm, |cx| to_string(cx, stack).unwrap().format().unwrap())
    };
    assert!(stack.contains("compiled.js:11"), "{stack}");

    cx.drop();
}

fn native_terminate(_: Context, _: &mut NativeCall) -> Result<(), Option<Value>> {
    Err(None)
}

/// Calls its first argument and propagates how the call ended.
fn native_call_arg(cx: Context, call: &mut NativeCall) -> Result<(), Option<Value>> {
    handle_scope_guard!(cx);
    let function = call.arg(0).to_handle(cx);
    match embed::call(cx, function, cx.undefined(), &[]) {
        Ok(value) => {
            call.set_return_value(*value);
            Ok(())
        }
        Err(error) => Err(embed::exception_value_opt(error).map(|value| *value)),
    }
}

fn define_test_natives(cx: Context, realm: Handle<Realm>) {
    let terminate = new_test_native(cx, realm, native_terminate, "terminate", false);
    define_global(cx, realm, "terminate", terminate.as_value());
    let call_arg = new_test_native(cx, realm, native_call_arg, "callArg", false);
    define_global(cx, realm, "callArg", call_arg.as_value());
}

#[test]
fn uncatchable_termination_skips_catch_and_finally() {
    let (mut cx, _) = new_context();
    let realm = cx.initial_realm();
    define_test_natives(cx, realm);
    eval(cx, realm, "var marks = []; function mark(x) { marks.push(x); }");

    let scripts = [
        "try { terminate(); mark('after'); } catch (e) { mark('catch'); } finally { mark('finally'); }",
        // Through nested JS frames, each with its own handlers
        "function inner() { try { terminate(); } finally { mark('inner'); } }
         function outer() { try { inner(); } catch (e) { mark('outer'); } }
         try { outer(); } finally { mark('top'); }",
        // Through a native that calls back into JS
        "try { callArg(function () { try { terminate(); } catch (e) { mark('cb'); } }); }
         catch (e) { mark('native caller'); } finally { mark('native finally'); }",
        // Through iterator closing
        "var it = { [Symbol.iterator]() { return { next() { return { value: 1 } },
                                                    return() { mark('return'); return {}; } }; } };
         for (var x of it) { terminate(); }",
    ];

    for script in scripts {
        let result = embed::evaluate_script(cx, realm, script, "terminate.js", 1);
        let error = result.err().expect("script was not terminated");
        assert!(embed::is_uncatchable(&error));
        assert!(matches!(embed::failure(error), embed::Failure::Uncatchable));
        assert!(embed::exception_value_opt(error).is_none());

        assert!(cx.vm().is_stack_empty());
        assert_eq!(embed::entered_realm_depth(cx), 0);
    }

    assert_eq!(eval_string(cx, realm, "marks.join(',')"), "");

    // Calls and constructs from the host report it the same way
    let function = eval(cx, realm, "(function () { try { terminate(); } finally { mark('f'); } })");
    let error = embed::call(cx, function, cx.undefined(), &[])
        .err()
        .unwrap();
    assert!(embed::is_uncatchable(&error));
    let error = embed::construct(cx, function, &[], None).err().unwrap();
    assert!(embed::is_uncatchable(&error));
    assert_eq!(eval_string(cx, realm, "marks.join(',')"), "");

    // The context keeps working afterwards and thrown values are still catchable
    assert_eq!(eval_string(cx, realm, "try { throw 1 } catch (e) { 'caught' + e }"), "caught1");

    cx.drop();
}

#[test]
fn uncatchable_termination_in_a_promise_job() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();
    define_test_natives(cx, realm);

    eval(
        cx,
        realm,
        "var marks = []; Promise.resolve().then(() => { try { terminate(); } finally { marks.push(1); } })",
    );

    let job = recorder.jobs.borrow_mut().remove(0);
    let job = HeapPtr::<ObjectValue>::from_ptr(job.cast()).to_handle();
    let error = embed::call(cx, job.as_value(), cx.undefined(), &[])
        .err()
        .unwrap();
    assert!(embed::is_uncatchable(&error));
    assert_eq!(eval_string(cx, realm, "marks.length"), "0");

    cx.drop();
}

#[test]
fn interrupt_from_another_thread_stops_an_infinite_loop() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();

    let handle = embed::interrupt_handle(cx);
    let requester = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        handle.request();
    });

    let started = std::time::Instant::now();
    let error = embed::evaluate_script(cx, realm, "while (true) {}", "loop.js", 1)
        .err()
        .expect("loop was not stopped");
    requester.join().unwrap();

    assert!(embed::is_uncatchable(&error));
    assert!(started.elapsed() >= std::time::Duration::from_millis(50));
    assert_eq!(recorder.interrupt_calls.get(), 1);
    assert!(!embed::interrupt_handle(cx).is_requested());

    // A host that answers true lets the script continue
    recorder.continue_on_interrupt.set(true);
    embed::interrupt_handle(cx).request();
    let result = eval_string(
        cx,
        realm,
        "var n = 0; function step() { n++; } for (var i = 0; i < 100; i++) step(); n",
    );
    assert_eq!(result, "100");
    assert_eq!(recorder.interrupt_calls.get(), 2);

    cx.drop();
}

#[test]
fn stack_limit_throws_catchable_range_error() {
    // Natives re-entering the VM use the native stack, so run on a thread with a script thread's
    // stack size rather than the 2 MiB default of test threads.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(stack_limit_test_body)
        .unwrap()
        .join()
        .unwrap();
}

fn stack_limit_test_body() {
    let (cx, _) = new_context();
    let realm = cx.initial_realm();
    define_test_natives(cx, realm);

    let full_limit = embed::stack_limit(cx);
    assert!(full_limit > 0);

    let deep = "function depth(n) { return n == 0 ? 0 : 1 + depth(n - 1); }
                try { String(depth(2000)) } catch (e) { e.constructor.name }";
    assert_eq!(eval_string(cx, realm, deep), "2000");

    embed::set_stack_limit(cx, 16 * 1024);
    assert_eq!(embed::stack_limit(cx), 16 * 1024);
    assert_eq!(eval_string(cx, realm, deep), "RangeError");

    // Natives that call back into JS hit the limit too, and it stays catchable in JS
    let through_native = "function again() { return callArg(again); }
                          try { again(); 'no error' } catch (e) { e.constructor.name }";
    assert_eq!(eval_string(cx, realm, through_native), "RangeError");

    embed::set_stack_limit(cx, usize::MAX);
    assert_eq!(embed::stack_limit(cx), full_limit);
    assert_eq!(eval_string(cx, realm, deep), "2000");

    cx.drop();
}

#[test]
fn native_reentry_limit_fits_small_native_stacks() {
    // Runs on the default 2 MiB test thread
    let (cx, _) = new_context();
    let realm = cx.initial_realm();
    define_test_natives(cx, realm);

    let default_limit = embed::native_reentry_limit(cx);
    embed::set_native_reentry_limit(cx, 6);
    assert_eq!(embed::native_reentry_limit(cx), 6.min(default_limit));

    let result = eval_string(
        cx,
        realm,
        "function f() { f.apply() }
         function g() { return callArg(g); }
         var out = [];
         try { f(); } catch (e) { out.push(e.constructor.name); }
         try { g(); } catch (e) { out.push(e.constructor.name); }
         out.join(',')",
    );
    assert_eq!(result, "RangeError,RangeError");

    embed::set_native_reentry_limit(cx, usize::MAX);
    assert_eq!(embed::native_reentry_limit(cx), default_limit);

    cx.drop();
}

#[test]
fn native_stack_quota_stops_deep_apply_recursion_on_a_small_thread() {
    const THREAD_STACK: usize = 512 * 1024;

    let result = std::thread::Builder::new()
        .stack_size(THREAD_STACK)
        .spawn(|| {
            let (cx, _) = new_context();
            let realm = cx.initial_realm();
            define_test_natives(cx, realm);

            // Leave a margin for the deepest frame between two checks and for creating the error
            embed::set_native_stack_quota(cx, THREAD_STACK - 192 * 1024);
            assert_ne!(embed::native_stack_limit(cx), 0);

            let result = eval_string(
                cx,
                realm,
                "function f() { f.apply() }
                 function g() { return callArg(g); }
                 var out = [];
                 try { f(); } catch (e) { out.push(e.constructor.name + ': ' + e.message); }
                 try { g(); } catch (e) { out.push(e.constructor.name); }
                 out.join(',')",
            );

            embed::set_native_stack_quota(cx, 0);
            assert_eq!(embed::native_stack_limit(cx), 0);
            cx.drop();
            result
        })
        .unwrap()
        .join()
        .unwrap();

    assert_eq!(result, "RangeError: too much recursion,RangeError");
}

#[test]
fn compile_without_running_then_execute_twice() {
    let (cx, _) = new_context();
    let realm = cx.initial_realm();

    // Syntax errors are reported at compile time, as SyntaxError objects
    let error = embed::compile_script(cx, realm, "var = 1", "bad.js", 1)
        .err()
        .unwrap();
    let error = embed::exception_value(error);
    let message = embed::with_realm(cx, realm, |cx| to_string(cx, error).unwrap().format());
    assert!(message.unwrap().starts_with("SyntaxError"));

    let script = embed::compile_script(
        cx,
        realm,
        "var runs = (typeof runs === 'number' ? runs : 0) + 1; runs",
        "counter.js",
        1,
    )
    .unwrap();

    // Compiling ran nothing
    assert_eq!(eval_string(cx, realm, "typeof runs"), "undefined");
    assert!(script.realm().ptr_eq(&realm));

    embed::gc(cx);
    assert_eq!(embed::execute_compiled(cx, script).unwrap().as_number(), 1.0);
    embed::gc(cx);
    assert_eq!(embed::execute_compiled(cx, script).unwrap().as_number(), 2.0);

    cx.drop();
}

#[test]
fn parse_off_thread_then_compile_on_the_context_thread() {
    let parsed = std::thread::spawn(|| {
        embed::parse_script_off_thread("var offThread = 6 * 7;\nnew Error('x').stack", "bg.js", 10)
    })
    .join()
    .unwrap()
    .unwrap();

    let syntax_error = std::thread::spawn(|| embed::parse_script_off_thread("if (", "bad.js", 1))
        .join()
        .unwrap()
        .err()
        .unwrap();
    assert!(!syntax_error.message.is_empty());

    let (cx, _) = new_context();
    let realm = cx.initial_realm();

    let script = embed::compile_parsed_script(cx, realm, parsed).unwrap();
    let stack = embed::execute_compiled(cx, script).unwrap();
    let stack = embed::with_realm(cx, realm, |cx| to_string(cx, stack).unwrap().format().unwrap());
    assert!(stack.contains("bg.js:11"), "{stack}");
    assert_eq!(eval_string(cx, realm, "offThread"), "42");

    cx.drop();
}

/// The module record for a test specifier, compiled from `module_sources` once and cached.
fn test_module(
    cx: Context,
    recorder: &Recorder,
    specifier: &str,
) -> EvalResult<Handle<embed::SourceTextModule>> {
    if let Some(record) = recorder.module_records.borrow().get(specifier) {
        return Ok(HeapPtr::<embed::SourceTextModule>::from_ptr(record.cast()).to_handle());
    }

    let Some(source) = recorder.module_sources.borrow().get(specifier).cloned() else {
        let mut cx = cx;
        let message = cx.alloc_string(&format!("no module {specifier}"))?;
        return Err(embed::throw_value(message.as_value()));
    };

    let module = embed::compile_module(cx, cx.initial_realm(), &source, specifier, 1)?;
    recorder
        .module_records
        .borrow_mut()
        .insert(specifier.to_owned(), module.as_ptr().cast());
    Ok(module)
}

/// Run every job handed to the host, in order, until none are left.
fn run_host_jobs(cx: Context, recorder: &Recorder) {
    loop {
        let job = {
            let mut jobs = recorder.jobs.borrow_mut();
            if jobs.is_empty() {
                return;
            }
            jobs.remove(0)
        };

        handle_scope_guard!(cx);
        let job = HeapPtr::<ObjectValue>::from_ptr(job.cast()).to_handle();
        embed::call(cx, job.as_value(), cx.undefined(), &[]).unwrap();
    }
}

fn add_module_source(recorder: &Recorder, specifier: &str, source: &str) {
    recorder
        .module_sources
        .borrow_mut()
        .insert(specifier.to_owned(), source.to_owned());
}

#[test]
fn host_driven_module_graph() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();

    add_module_source(
        &recorder,
        "./lazy.js",
        "export const y = 2; await null; export const late = 'tla';",
    );

    let main = embed::compile_module(
        cx,
        realm,
        "import { x } from './dep.js';
         import { y, late } from './lazy.js';
         export const sum = x + y;
         export const lateValue = late;
         export const url = import.meta.url;",
        "main.js",
        1,
    )
    .unwrap();

    let requests = embed::module_requests(main);
    let specifiers: Vec<String> = requests.iter().map(|r| r.specifier.to_string()).collect();
    assert_eq!(specifiers, ["./dep.js", "./lazy.js"]);
    assert!(requests[0].attributes.is_empty());

    // One dependency supplied up front, the other through the resolve hook while linking
    let dep = embed::compile_module(cx, realm, "export const x = 1;", "dep.js", 1).unwrap();
    embed::set_requested_module(main, 0, dep);
    assert!(embed::requested_module(main, 0).unwrap().ptr_eq(&dep));
    assert!(embed::requested_module(main, 1).is_none());

    embed::set_module_private(main, Value::smi(77));
    assert_eq!(embed::module_private(main).as_smi(), 77);
    assert_eq!(embed::module_state(main), embed::ModuleState::New);

    embed::module_link(cx, main).unwrap();
    assert_eq!(*recorder.resolved_specifiers.borrow(), ["./lazy.js"]);
    assert_eq!(embed::module_state(main), embed::ModuleState::Linked);
    assert!(embed::module_realm(main).ptr_eq(&realm));

    let promise = embed::module_evaluate(cx, main).unwrap();
    embed::gc(cx);
    run_host_jobs(cx, &recorder);

    assert_eq!(embed::api::promise_state(promise), embed::api::PromiseStatus::Fulfilled);

    let namespace = embed::module_namespace(cx, main).unwrap();
    assert_eq!(get_number(cx, namespace, "sum"), 3.0);
    let late = get(cx, namespace, key(cx, "lateValue")).unwrap();
    assert_eq!(late.as_string().format().unwrap(), "tla");
    let url = get(cx, namespace, key(cx, "url")).unwrap();
    assert_eq!(url.as_string().format().unwrap(), "main.js");
    assert_eq!(embed::module_private(main).as_smi(), 77);

    cx.drop();
}

#[test]
fn module_resolution_link_and_evaluation_errors() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();

    // Unknown specifier: the host's resolve hook throws
    let missing = embed::compile_module(cx, realm, "import './nowhere.js';", "a.js", 1).unwrap();
    let error = embed::exception_value(embed::module_link(cx, missing).err().unwrap());
    assert_eq!(error.as_string().format().unwrap(), "no module ./nowhere.js");

    // Missing export: a link-time SyntaxError
    add_module_source(&recorder, "./empty.js", "export const other = 1;");
    let bad_import =
        embed::compile_module(cx, realm, "import { absent } from './empty.js';", "b.js", 1)
            .unwrap();
    let error = embed::exception_value(embed::module_link(cx, bad_import).err().unwrap());
    let message =
        embed::with_realm(cx, realm, |cx| to_string(cx, error).unwrap().format().unwrap());
    assert!(message.starts_with("SyntaxError"), "{message}");

    // Compile-time syntax error
    let error = embed::compile_module(cx, realm, "export {", "c.js", 1)
        .err()
        .unwrap();
    let error = embed::exception_value(error);
    let message =
        embed::with_realm(cx, realm, |cx| to_string(cx, error).unwrap().format().unwrap());
    assert!(message.starts_with("SyntaxError"), "{message}");

    // Evaluation error rejects the evaluation promise
    let throws = embed::compile_module(cx, realm, "throw 'boom';", "d.js", 1).unwrap();
    embed::module_link(cx, throws).unwrap();
    let promise = embed::module_evaluate(cx, throws).unwrap();
    run_host_jobs(cx, &recorder);
    assert_eq!(embed::api::promise_state(promise), embed::api::PromiseStatus::Rejected);
    let error = embed::module_evaluation_error(cx, throws).unwrap();
    assert_eq!(error.as_string().format().unwrap(), "boom");

    cx.drop();
}

#[test]
fn reevaluating_a_module_that_failed_does_not_abort() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();

    // A module that throws is evaluated, then evaluated again (as when the same module is
    // imported twice, e.g. by a second import() after a failed first one).
    let throws = embed::compile_module(cx, realm, "throw 'boom';", "e.js", 1).unwrap();
    embed::module_link(cx, throws).unwrap();
    let first = embed::module_evaluate(cx, throws).unwrap();
    run_host_jobs(cx, &recorder);
    assert_eq!(embed::api::promise_state(first), embed::api::PromiseStatus::Rejected);

    let second = embed::module_evaluate(cx, throws).unwrap();
    run_host_jobs(cx, &recorder);
    assert_eq!(embed::api::promise_state(second), embed::api::PromiseStatus::Rejected);
    let error = embed::module_evaluation_error(cx, throws).unwrap();
    assert_eq!(error.as_string().format().unwrap(), "boom");

    // A failed dependency is evaluated again through a second importer
    add_module_source(&recorder, "./bad.js", "throw 'dep boom';");
    for name in ["a.js", "b.js"] {
        let importer = embed::compile_module(cx, realm, "import './bad.js';", name, 1).unwrap();
        embed::module_link(cx, importer).unwrap();
        let promise = embed::module_evaluate(cx, importer).unwrap();
        run_host_jobs(cx, &recorder);
        assert_eq!(embed::api::promise_state(promise), embed::api::PromiseStatus::Rejected);
    }

    cx.drop();
}

#[test]
fn dynamic_import_completed_later_by_the_host() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();
    recorder.defer_dynamic_imports.set(true);

    eval(
        cx,
        realm,
        "var log = [];
         import('./later.js').then(ns => log.push('v=' + ns.v));
         import('./fails.js').catch(e => log.push('error=' + e));",
    );

    run_host_jobs(cx, &recorder);
    assert_eq!(eval_string(cx, realm, "log.join(',')"), "");

    let pending = recorder.pending_imports.borrow().clone();
    let specifiers: Vec<&str> = pending.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(specifiers, ["./later.js", "./fails.js"]);

    // GC while the host holds the pending imports
    embed::gc(cx);

    add_module_source(
        &recorder,
        "./later.js",
        "import { w } from './dep2.js'; export const v = w * 5;",
    );
    add_module_source(&recorder, "./dep2.js", "export const w = 1;");

    // The host's rooted copies are updated by every GC, so read them right before each use
    let module = test_module(cx, &recorder, "./later.js").unwrap();
    let later = recorder.pending_imports.borrow()[0].1;
    embed::finish_dynamic_import(cx, embed::DynamicImport::from_cell_ptr(later), Ok(module))
        .unwrap();

    let mut cx_mut = cx;
    let reason = cx_mut.alloc_string("not found").unwrap().as_value();
    let fails = recorder.pending_imports.borrow()[1].1;
    embed::finish_dynamic_import(cx, embed::DynamicImport::from_cell_ptr(fails), Err(reason))
        .unwrap();
    recorder.pending_imports.borrow_mut().clear();

    run_host_jobs(cx, &recorder);
    let log = eval_string(cx, realm, "log.sort().join(',')");
    assert_eq!(log, "error=not found,v=5");

    // Imports the host does not take over are resolved right away through resolve_module
    recorder.defer_dynamic_imports.set(false);
    eval(cx, realm, "import('./dep2.js').then(ns => log.push('w=' + ns.w));");
    run_host_jobs(cx, &recorder);
    assert!(eval_string(cx, realm, "log.join(',')").contains("w=1"));

    cx.drop();
}

fn native_capture_stack(cx: Context, call: &mut NativeCall) -> Result<(), Option<Value>> {
    handle_scope_guard!(cx);
    let frames = embed::capture_stack(cx, 0).unwrap();
    let text = frames
        .iter()
        .map(|frame| {
            format!(
                "{}@{}:{}:{}{}",
                frame.function_name.as_deref().unwrap_or("?"),
                frame.file_name.as_deref().unwrap_or("-"),
                frame.line.map_or("-".to_owned(), |line| line.to_string()),
                frame
                    .column
                    .map_or("-".to_owned(), |column| column.to_string()),
                if frame.is_async_function {
                    " async"
                } else {
                    ""
                },
            )
        })
        .collect::<Vec<_>>()
        .join(" < ");
    let mut cx = cx;
    call.set_return_value(*cx.alloc_string(&text).unwrap().as_value());
    Ok(())
}

#[test]
fn capture_stack_and_error_stack_frames() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();

    let capture = new_test_native(cx, realm, native_capture_stack, "capture", false);
    define_global(cx, realm, "capture", capture.as_value());

    assert!(embed::capture_stack(cx, 0).unwrap().is_empty());

    let stack = embed::evaluate_script(
        cx,
        realm,
        "function inner() { return capture(); }\nfunction outer() { return inner(); }\nouter()",
        "stack.js",
        5,
    )
    .unwrap();
    let stack = stack.as_string().format().unwrap();
    assert_eq!(
        stack,
        "capture@-:-:- < inner@stack.js:5:27 < outer@stack.js:6:27 < ?@stack.js:7:1"
    );

    // Async functions resumed from a job are marked
    eval(
        cx,
        realm,
        "var asyncStack; (async function later() { await null; asyncStack = capture(); })();",
    );
    run_host_jobs(cx, &recorder);
    let async_stack = eval_string(cx, realm, "asyncStack");
    assert_eq!(async_stack, "capture@-:-:- < later@embed_test.js:1:68 async");

    // The frames of an Error, also after its `stack` was formatted
    let error = eval(cx, realm, "function thrower() { return new Error('x'); }\nthrower()");
    let frames = embed::error_stack_frames(cx, error.as_object(), 0)
        .unwrap()
        .unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].function_name.as_deref(), Some("thrower"));
    assert_eq!(frames[0].line, Some(1));
    assert_eq!(frames[1].function_name, None);
    assert_eq!(frames[1].line, Some(2));

    define_global(cx, realm, "err", error);
    eval(cx, realm, "err.stack");
    let frames_after = embed::error_stack_frames(cx, error.as_object(), 1)
        .unwrap()
        .unwrap();
    assert_eq!(frames_after, frames[..1]);

    let plain = eval(cx, realm, "({})");
    assert!(
        embed::error_stack_frames(cx, plain.as_object(), 0)
            .unwrap()
            .is_none()
    );

    cx.drop();
}

#[test]
fn csp_hook_for_eval_and_function_constructors() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();

    // Allowed: the host sees what is compiled
    assert_eq!(eval_string(cx, realm, "String(eval('1 + 1')) + (0, eval)('2')"), "22");
    assert_eq!(eval_string(cx, realm, "new Function('a', 'b', 'return a + b')(2, 3)"), "5");
    assert_eq!(eval_string(cx, realm, "eval(42)"), "42");
    {
        let calls = recorder.compile_string_calls.borrow();
        assert_eq!(calls[0], "Eval|1 + 1||1 + 1");
        assert_eq!(calls[1], "IndirectEval|2||2");
        assert!(calls[2].starts_with("Function|function anonymous(a,b"), "{}", calls[2]);
        assert!(calls[2].ends_with("|a,b|return a + b"), "{}", calls[2]);
        // Non-string eval arguments are not compiled, so not checked
        assert_eq!(calls.len(), 3);
    }

    recorder.block_string_compilation.set(true);
    let result = eval_string(
        cx,
        realm,
        "var out = [];
         function attempt(f) {
             try { f(); out.push('ran'); }
             catch (e) { out.push(e instanceof EvalError ? e.message : 'other ' + e); }
         }
         attempt(() => eval('1'));
         attempt(() => (0, eval)('1'));
         attempt(() => Function('return 1'));
         attempt(() => (async function () {}).constructor('return 1'));
         attempt(() => (function* () {}).constructor('yield 1'));
         attempt(() => (async function* () {}).constructor('yield 1'));
         out.join('; ')",
    );
    assert_eq!(
        result,
        "call to eval() blocked by CSP; call to eval() blocked by CSP; \
         call to Function() blocked by CSP; call to Function() blocked by CSP; \
         call to Function() blocked by CSP; call to Function() blocked by CSP"
    );

    cx.drop();
}

#[test]
fn heap_stats_report_semispace_and_host_space() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();

    let before = embed::heap_stats(cx);
    assert!(before.semispace_used_bytes > 0);
    assert!(before.semispace_used_bytes <= before.semispace_capacity_bytes);
    assert!(before.heap_size_bytes >= 2 * before.semispace_capacity_bytes);
    assert!(before.max_heap_size_bytes >= before.heap_size_bytes);
    assert_eq!(before.host_object_count, 0);
    assert_eq!(before.host_space_used_bytes, 0);

    let host_object = {
        handle_scope_guard!(cx);
        *new_test_host_object(cx, HostFlags::empty(), 4)
    };
    recorder
        .cell_roots
        .borrow_mut()
        .push(host_object.as_ptr().cast());
    {
        handle_scope_guard!(cx);
        new_test_host_object(cx, HostFlags::empty(), 0);
    }

    let with_objects = embed::heap_stats(cx);
    assert_eq!(with_objects.host_object_count, 2);
    assert!(with_objects.host_space_used_bytes >= 4 * 8);
    assert!(with_objects.host_space_reserved_bytes >= with_objects.host_space_used_bytes);

    // Garbage leaves the semispace and the unrooted host object on a GC
    eval(cx, realm, "for (var i = 0; i < 1000; i++) ({ i });");
    let before_gc = embed::heap_stats(cx);
    embed::gc(cx);
    let after_gc = embed::heap_stats(cx);
    // With a GC on every allocation the garbage is already gone before the explicit GC
    assert!(after_gc.semispace_used_bytes <= before_gc.semispace_used_bytes);
    #[cfg(not(feature = "gc_stress_test"))]
    assert!(after_gc.semispace_used_bytes < before_gc.semispace_used_bytes);
    assert_eq!(after_gc.host_object_count, 1);
    assert!(after_gc.host_space_used_bytes < with_objects.host_space_used_bytes);

    cx.drop();
}

/// Returns the jit info of its callee as a number, as Servo's generic natives read it.
fn native_report_jit_info(cx: Context, call: &mut NativeCall) -> Result<(), Option<Value>> {
    let info = embed::native_jit_info(cx, call.callee()).map_or(-1.0, |info| info as usize as f64);
    call.set_return_value(Value::number(info));
    Ok(())
}

#[test]
fn natives_carry_their_own_jit_info() {
    let (cx, _) = new_context();
    let realm = cx.initial_realm();

    let native = native_report_jit_info as TestNative as *const c_void;
    let make = |info: usize, name: &str| {
        let name = key(cx, name);
        embed::new_native_function_with_jit_info(
            cx,
            realm,
            native,
            info as *const c_void,
            0,
            Some(name),
            false,
            1,
        )
        .unwrap()
    };

    let first = make(0x1110, "first");
    let second = make(0x2220, "second");
    let plain = new_test_native(cx, realm, native_report_jit_info, "plain", false);
    define_global(cx, realm, "first", first.as_value());
    define_global(cx, realm, "second", second.as_value());
    define_global(cx, realm, "plain", plain.as_value());

    for _ in 0..5 {
        embed::gc(cx);
        assert_eq!(embed::native_jit_info(cx, *first.as_value()), Some(0x1110 as *const c_void));
        assert_eq!(embed::native_jit_info(cx, *second.as_value()), Some(0x2220 as *const c_void));
        assert_eq!(embed::native_of(cx, first), Some(native));
        assert_eq!(embed::native_of(cx, second), Some(native));
    }

    // Read back from the callee on every call
    let result = eval_string(cx, realm, "[first(), second(), plain(), first()].join(',')");
    assert_eq!(result, format!("{},{},-1,{}", 0x1110, 0x2220, 0x1110));

    // Never panics, None for anything that is not a native with info
    let ordinary = eval(cx, realm, "(function () {})");
    let host_object = new_test_host_object(cx, HostFlags::CALLABLE, 0);
    // Handles, since evaluating the later entries may move the earlier ones
    let values = [
        plain.as_value(),
        ordinary,
        host_object.as_value(),
        cx.smi(3),
        cx.undefined(),
        eval(cx, realm, "'str'"),
        eval(cx, realm, "Math.max"),
    ];
    for value in values {
        assert_eq!(embed::native_jit_info(cx, *value), None);
    }

    assert!(embed::set_native_jit_info(cx, plain, 0x3330 as *const c_void));
    assert_eq!(embed::native_jit_info(cx, *plain.as_value()), Some(0x3330 as *const c_void));
    assert!(!embed::set_native_jit_info(cx, ordinary.as_object(), 0x1 as *const c_void));

    cx.drop();
}

#[test]
fn function_reserved_opt_never_panics() {
    let (cx, _) = new_context();
    let realm = cx.initial_realm();

    let one_slot = {
        let name = key(cx, "oneSlot");
        embed::new_native_function(
            cx,
            realm,
            native_throw as TestNative as *const c_void,
            0,
            Some(name),
            false,
            1,
        )
        .unwrap()
    };
    embed::set_function_reserved(cx, one_slot, 0, cx.number(12.0)).unwrap();
    embed::gc(cx);

    assert_eq!(
        embed::function_reserved_opt(cx, one_slot.as_value(), 0).map(|v| v.as_number()),
        Some(12.0)
    );
    // Slot 1 was not reserved, slot 5 is out of range
    assert!(embed::function_reserved_opt(cx, one_slot.as_value(), 1).is_none());
    assert!(embed::function_reserved_opt(cx, one_slot.as_value(), 5).is_none());

    // Non-natives
    assert!(embed::function_reserved_opt(cx, eval(cx, realm, "(function () {})"), 0).is_none());
    assert!(embed::function_reserved_opt(cx, cx.number(1.0), 0).is_none());
    assert!(embed::function_reserved_opt(cx, eval(cx, realm, "({})"), 0).is_none());

    cx.drop();
}

#[test]
fn promise_rejection_tracking() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();

    eval(
        cx,
        realm,
        "var p = Promise.reject(1);
         Promise.resolve().then(() => { p.catch(() => {}); });
         Promise.reject(2).catch(() => {});
         var reject;
         var handledFirst = new Promise((_, r) => { reject = r; });
         handledFirst.catch(() => {});
         reject(3);
         (async function () { try { await Promise.reject(4); } catch (e) {} })();",
    );

    // Await on an already rejected promise counts as a handler added after the rejection
    assert_eq!(
        *recorder.rejections.borrow(),
        ["Reject 1", "Reject 2", "Handle 2", "Reject 4", "Handle 4"]
    );

    // The handler for `p` is added in a later job
    run_host_jobs(cx, &recorder);
    assert_eq!(recorder.rejections.borrow().last().unwrap(), "Handle 1");
    assert_eq!(recorder.rejections.borrow().len(), 6);

    cx.drop();
}

#[test]
fn jobs_keep_the_realm_they_run_in() {
    let (cx, recorder) = new_context();
    let initial_realm = cx.initial_realm();
    let (other_realm, _) = embed::new_global(cx, TEST_CLASS, HostFlags::empty(), 0).unwrap();

    let job_realms = |recorder: &Recorder| -> Vec<HeapPtr<Realm>> {
        recorder
            .jobs
            .borrow()
            .iter()
            .map(|job| embed::job_realm(HeapPtr::from_ptr(job.cast())).unwrap())
            .collect()
    };

    // A reaction job, an await continuation and a thenable job, all created in the other realm
    eval(
        cx,
        other_realm,
        "Promise.resolve().then(() => 1);
         (async function () { await null; })();
         Promise.resolve({ then(resolve) { resolve(2); } });",
    );
    let realms = job_realms(&recorder);
    assert_eq!(realms.len(), 3);
    assert!(realms.iter().all(|realm| realm.ptr_eq(&other_realm)));

    // A handler from the initial realm attached in the other realm runs in the initial realm
    recorder.jobs.borrow_mut().clear();
    let handler = eval(cx, initial_realm, "(function () {})");
    define_global(cx, other_realm, "initialHandler", handler);
    eval(cx, other_realm, "Promise.resolve().then(initialHandler);");
    let realms = job_realms(&recorder);
    assert_eq!(realms.len(), 1);
    assert!(realms[0].ptr_eq(&initial_realm));

    // Not jobs
    assert!(embed::job_realm(*new_test_host_object(cx, HostFlags::empty(), 0)).is_none());
    assert!(embed::job_realm(*eval(cx, initial_realm, "({})").as_object()).is_none());

    recorder.jobs.borrow_mut().clear();
    embed::gc(cx);
    cx.drop();
}

#[test]
fn promises_never_move() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();

    let script_promise = eval(
        cx,
        realm,
        "var log = [];
         var p = new Promise(r => { globalThis.resolveP = r; });
         var chained = p.then(v => { log.push('p=' + v); return v * 2; });
         var rejected = Promise.reject(7);
         rejected.catch(e => log.push('rejected=' + e));
         p",
    );
    let chained = eval(cx, realm, "chained");
    let api_promise = embed::api::new_promise(cx, realm).unwrap();
    let api_chained = embed::api::promise_then(
        cx,
        api_promise,
        Some(eval(cx, realm, "(v => log.push('api=' + v))").as_object()),
        None,
    )
    .unwrap();

    let promises = [script_promise, chained, api_promise.as_value(), api_chained];
    let addresses: Vec<usize> = promises
        .iter()
        .map(|p| p.as_pointer().as_ptr() as usize)
        .collect();

    for promise in &promises {
        let object = promise.as_object();
        assert!(embed::is_pinned(*object));
        assert!(!embed::is_host_object(*object));
        assert!(embed::host_class(*object).is_none());
        assert!(embed::realm_of_object(cx, *object).ptr_eq(&realm));
        embed::assert_stable(cx, object.as_ptr().cast());
    }

    for i in 0..1000 {
        embed::gc(cx);

        for (promise, address) in promises.iter().zip(&addresses) {
            assert_eq!(promise.as_pointer().as_ptr() as usize, *address, "promise moved");
        }

        match i {
            10 => {
                eval(cx, realm, "resolveP(21)");
            }
            20 => embed::api::promise_resolve(cx, api_promise, cx.number(5.0)).unwrap(),
            30 => run_host_jobs(cx, &recorder),
            _ => {}
        }
    }

    run_host_jobs(cx, &recorder);
    assert_eq!(eval_string(cx, realm, "log.sort().join(',')"), "api=5,p=21,rejected=7");
    let chained_promise = embed::api::as_promise(chained).unwrap();
    assert_eq!(
        embed::api::promise_result(cx, chained_promise)
            .unwrap()
            .as_number(),
        42.0
    );
    assert_eq!(
        embed::api::promise_state(embed::api::as_promise(eval(cx, realm, "rejected")).unwrap()),
        embed::api::PromiseStatus::Rejected
    );

    cx.drop();
}

#[test]
fn dead_pinned_promises_are_swept_and_weakly_referenced() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();

    let baseline = embed::host_object_count(cx);
    eval(
        cx,
        realm,
        "var keep = Promise.resolve('kept');
         var weakKept = new WeakRef(keep);
         var map = new WeakMap();
         map.set(keep, 'value');
         (function () {
             var dead = new Promise(() => {});
             globalThis.weakDead = new WeakRef(dead);
             map.set(dead, 'gone');
         })();",
    );
    run_host_jobs(cx, &recorder);
    assert!(embed::host_object_count(cx) >= baseline + 2);

    embed::gc(cx);
    embed::gc(cx);

    let result = eval_string(
        cx,
        realm,
        "[weakDead.deref() === undefined, weakKept.deref() === keep, map.get(keep)].join(',')",
    );
    assert_eq!(result, "true,true,value");

    // Only the kept promise is left among the promises
    let before = embed::host_object_count(cx);
    eval(cx, realm, "keep = undefined; weakKept = undefined;");
    embed::gc(cx);
    embed::gc(cx);
    assert!(embed::host_object_count(cx) < before);

    cx.drop();
}

// Allocating enough to grow the heap takes too long with a GC on every allocation
#[cfg(not(feature = "gc_stress_test"))]
#[test]
fn heap_growth_keeps_pinned_promises_in_place() {
    use crate::common::constants::MIN_HEAP_SIZE;

    let (cx, recorder) = new_context_with_heap_size(MIN_HEAP_SIZE);
    let realm = cx.initial_realm();

    let promises = eval(
        cx,
        realm,
        "var promises = []; for (var i = 0; i < 200; i++) promises.push(Promise.resolve(i)); promises",
    );
    let first = get(cx, promises.as_object(), key(cx, "0")).unwrap();
    let address = first.as_pointer().as_ptr() as usize;
    let heap_size_before = cx.heap.heap_size();

    eval(
        cx,
        realm,
        "var keepAlive = []; for (var i = 0; i < 200000; i++) keepAlive.push({ i });",
    );
    assert!(cx.heap.heap_size() > heap_size_before, "heap did not grow");

    let first_after = get(cx, promises.as_object(), key(cx, "0")).unwrap();
    assert_eq!(first_after.as_pointer().as_ptr() as usize, address);

    eval(cx, realm, "var sum = 0; for (var q of promises) q.then(v => { sum += v; });");
    run_host_jobs(cx, &recorder);
    assert_eq!(eval_string(cx, realm, "sum"), "19900");

    cx.drop();
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "points into the moving heap")]
fn assert_stable_rejects_semispace_pointers() {
    let (cx, _) = new_context();
    let object = eval(cx, cx.initial_realm(), "({})");
    embed::assert_stable(cx, object.as_pointer().as_ptr().cast());
}

#[test]
fn assert_stable_accepts_host_space_and_permanent_pointers() {
    let (cx, _) = new_context();
    let host_object = new_test_host_object(cx, HostFlags::empty(), 0);
    embed::assert_stable(cx, host_object.as_ptr().cast());
    let promise = eval(cx, cx.initial_realm(), "Promise.resolve()");
    embed::assert_stable(cx, promise.as_pointer().as_ptr().cast());
    cx.drop();
}

#[test]
fn compile_stats_count_lazy_functions() {
    let (cx, _recorder) = new_context();
    let realm = cx.initial_realm();

    // Nested functions are stubs until called. `g` only becomes a stub when `f` is generated.
    let result = eval_string(
        cx,
        realm,
        "function f() { function g() { return 1; } return g() + 1; } function unused() {} String(f())",
    );
    assert_eq!(result, "2");

    let stats = embed::compile_stats(cx);
    assert_eq!(stats.stub_functions, 3);
    assert_eq!(stats.lazy_generated_functions, 2);
    assert_eq!(stats.pending_stub_functions, 1);
    assert!(stats.eager_functions >= 1);
    assert!(stats.stub_source_bytes > stats.lazy_generated_source_bytes);

    // A stub that is no longer referenced is dropped from the registry by a GC
    {
        handle_scope_guard!(cx);
        eval(cx, realm, "var h = function () {}; h = null;");
    }
    assert_eq!(embed::compile_stats(cx).pending_stub_functions, 2);
    embed::gc(cx);
    assert_eq!(embed::compile_stats(cx).pending_stub_functions, 1);

    // The remaining stub still works after the GC moved it
    assert_eq!(eval_string(cx, realm, "String(unused())"), "undefined");
    assert_eq!(embed::compile_stats(cx).pending_stub_functions, 0);
    cx.drop();
}

#[test]
#[should_panic(expected = "not a host space object")]
fn host_object_ref_from_ordinary_object_panics() {
    let (cx, _) = new_context();
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    let object = embed::with_realm(cx, realm, |cx| ordinary_object_create(cx).unwrap());
    HostObjectRef::from_object(*object);
}

#[test]
fn host_object_ref_slots_and_header_copies() {
    let (cx, _) = new_context();
    {
        handle_scope_guard!(cx);
        let object = new_test_host_object(cx, HostFlags::empty(), 2);
        let object_ref = HostObjectRef::from_object(*object);

        // Two copies of the header pointer may be used interleaved
        let (a, b) = (object_ref.header(), object_ref.header());
        a.set_reserved_slot(0, Value::smi(1));
        b.set_reserved_slot(0, Value::smi(2));
        assert_eq!(a.reserved_slot(0).as_smi(), 2);
        object_ref.set_reserved_slot(1, Value::smi(3));
        assert_eq!(embed::reserved_slot(*object, 1).as_smi(), 3);
        assert_eq!(b.class(), TEST_CLASS);
    }
    cx.drop();
}

/// A host object created during a GC would be pre-marked but never traced, so creating one from a
/// GC hook is refused.
#[test]
#[cfg_attr(
    not(feature = "alloc_error"),
    should_panic(expected = "host object created during a garbage collection")
)]
fn host_object_creation_in_finalizer_is_refused() {
    let (cx, recorder) = new_context();
    recorder.host_object_in_finalizer.set(true);
    {
        handle_scope_guard!(cx);
        new_test_host_object(cx, HostFlags::HAS_FINALIZE, 1);
    }
    embed::gc(cx);
    assert_eq!(recorder.finalized.borrow().len(), 1);
    recorder.host_object_in_finalizer.set(false);
    cx.drop();
}

#[test]
fn compile_stats_without_lazy_functions() {
    let options = OptionsBuilder::new().lazy_functions(false).build().unwrap();
    let cx = ContextBuilder::new()
        .set_options(Rc::new(options))
        .build()
        .unwrap();
    let realm = cx.initial_realm();

    let result = eval_string(cx, realm, "function f() { return () => 1; } String(f()())");
    assert_eq!(result, "1");

    let stats = embed::compile_stats(cx);
    assert_eq!(stats.stub_functions, 0);
    assert_eq!(stats.lazy_generated_functions, 0);
    assert_eq!(stats.eager_functions, 3);

    cx.drop();
}

#[test]
fn swap_host_objects_swaps_contents_and_keeps_identity_hashes() {
    let (cx, _recorder) = new_context();
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    let a = new_test_host_object(cx, HostFlags::empty(), 2);
    let b = new_test_host_object(cx, HostFlags::empty(), 2);
    let c = new_test_host_object(cx, HostFlags::empty(), 3);
    embed::set_reserved_slot(*a, 0, Value::smi(1));
    embed::set_reserved_slot(*b, 0, Value::smi(2));
    embed::with_realm(cx, realm, |cx| {
        let flags = PropertyFlags::from_data_attributes(true, true, true);
        define_property_or_throw(
            cx,
            a,
            key(cx, "which"),
            PropertyDescriptor::data(cx.number(10.0), flags),
        )
        .unwrap();
        define_property_or_throw(
            cx,
            b,
            key(cx, "which"),
            PropertyDescriptor::data(cx.number(20.0), flags),
        )
        .unwrap();
        define_property_or_throw(
            cx,
            b,
            key(cx, "only_b"),
            PropertyDescriptor::data(cx.number(3.0), flags),
        )
        .unwrap();
    });
    let (mut ha, mut hb) = (*a, *b);
    let (hash_a, hash_b) = (ha.hash_code(), hb.hash_code());

    // Different layouts are refused and left alone.
    assert!(!embed::swap_host_objects(*a, *c));
    assert_eq!(get_number(cx, a, "which"), 10.0);

    assert!(embed::swap_host_objects(*a, *b));
    assert_eq!(get_number(cx, a, "which"), 20.0);
    assert_eq!(get_number(cx, a, "only_b"), 3.0);
    assert_eq!(get_number(cx, b, "which"), 10.0);
    assert_eq!(embed::reserved_slot(*a, 0).as_smi(), 2);
    assert_eq!(embed::reserved_slot(*b, 0).as_smi(), 1);
    let (mut ha, mut hb) = (*a, *b);
    assert_eq!(ha.hash_code(), hash_a);
    assert_eq!(hb.hash_code(), hash_b);

    // Both survive a GC with their swapped contents.
    embed::gc(cx);
    assert_eq!(get_number(cx, a, "which"), 20.0);
    assert_eq!(get_number(cx, b, "which"), 10.0);
}

#[test]
fn host_globals_have_the_optional_annex_b_methods() {
    let (cx, _recorder) = new_context();
    handle_scope_guard!(cx);
    let (realm, _) = embed::new_global(cx, TEST_CLASS, HostFlags::empty(), 0).unwrap();
    let expected = if cx.options.annex_b {
        "function function function bc"
    } else {
        "undefined undefined undefined"
    };
    let source = if cx.options.annex_b {
        "[typeof ''.substr, typeof escape, typeof new Date().getYear, 'abcd'.substr(1, 2)].join(' ')"
    } else {
        "[typeof ''.substr, typeof escape, typeof new Date().getYear].join(' ')"
    };
    assert_eq!(eval_string(cx, realm, source), expected);
}

#[test]
fn trim_lazy_programs_frees_parse_arenas_and_keeps_functions_working() {
    // Trimming by generation is what a context without re-parsing does
    let (cx, _recorder) = new_context_with_options(OptionsBuilder::new().lazy_reparse(false));
    handle_scope_guard!(cx);
    if !cx.options.lazy_functions {
        return;
    }
    let realm = cx.initial_realm();
    // Nested functions stay lazy stubs until called; `f` is called once to show generated and pending stubs mix.
    eval(
        cx,
        realm,
        "var lib = (function () { function f(x) { return x + 1; } function g(x) { function h(y) { return y * 2; } return h(x) + 1; } \
         return { f: f, g: g }; })(); lib.f(1);",
    );
    let before = embed::retained_parse_bytes(cx);
    assert!(before > 0, "lazy stubs keep their program alive");

    // A deadline that has already passed changes nothing.
    let past = std::time::Instant::now();
    let r = embed::trim_lazy_programs(cx, 0, past).unwrap();
    assert_eq!(r.generated, 0);
    assert!(!r.within_budget);
    assert_eq!(embed::retained_parse_bytes(cx), before);

    // A budget the program fits in changes nothing either.
    let later = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let r = embed::trim_lazy_programs(cx, before, later).unwrap();
    assert_eq!(r.generated, 0);
    assert!(r.within_budget);

    // Budget 0: every pending stub is generated and the arenas are freed.
    let r = embed::trim_lazy_programs(cx, 0, later).unwrap();
    assert!(r.within_budget);
    assert!(r.generated >= 2, "g and h at least: {r:?}");
    assert!(r.freed_programs >= 1);
    assert_eq!(r.retained_bytes, 0);
    assert_eq!(embed::retained_parse_bytes(cx), 0);
    assert_eq!(cx.lazy_functions.num_pending(), 0);

    // The generated functions behave as before.
    assert_eq!(eval_string(cx, realm, "String(lib.f(1) + lib.g(3))"), "9");
}

#[test]
fn function_cache_reuses_renamed_functions_and_verifies() {
    let (cx, _) = new_context();
    embed::set_function_cache_enabled(cx, true);
    embed::set_realm_cache_partition(cx, cx.initial_realm(), b"https://example.test");
    embed::set_function_cache_verify(cx, true);
    let realm = cx.initial_realm();

    let first = "function f1(a, b) { var c = a + b; return function (d) { return c * d; }; } \
                 String(f1(2, 3)(4))";
    let renamed = "function g1(x, y) { var z = x + y; return function (w) { return z * w; }; } \
                   String(g1(5, 6)(2))";
    assert_eq!(eval_string(cx, realm, first), "20");
    let before = embed::function_cache_stats(cx);
    assert_eq!(before.hits, 0);
    assert!(before.misses >= 2);

    // The same structure under other names: both functions are cache hits, and verify mode
    // finds no difference to generating them.
    assert_eq!(eval_string(cx, realm, renamed), "22");
    let after = embed::function_cache_stats(cx);
    assert!(after.hits >= 2, "{after:?}");
    assert_eq!(after.verify_mismatches, 0);

    // A different structure is a miss.
    assert_eq!(
        eval_string(cx, realm, "function h1(a, b) { return a - b; } String(h1(5, 3))"),
        "2"
    );
    assert!(embed::function_cache_stats(cx).misses > after.misses);
    cx.drop();
}

#[test]
fn function_cache_disk_store_serves_a_second_context() {
    let dir = std::env::temp_dir().join(format!("fncache-embed-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let script = "function k1(a, b) { var s = 0; for (var i = a; i < b; i++) { s += i; } return s; } \
                  String(k1(1, 5))";
    let renamed = "function m1(p, q) { var t = 0; for (var j = p; j < q; j++) { t += j; } return t; } \
                   String(m1(1, 5))";

    let (cx, _) = new_context();
    embed::set_function_cache_enabled(cx, true);
    embed::set_realm_cache_partition(cx, cx.initial_realm(), b"https://example.test");
    embed::set_function_cache_dir(cx, Some(dir.clone()));
    assert_eq!(eval_string(cx, cx.initial_realm(), script), "10");
    embed::function_cache_flush(cx);
    cx.drop();

    let (cx, _) = new_context();
    embed::set_function_cache_enabled(cx, true);
    embed::set_realm_cache_partition(cx, cx.initial_realm(), b"https://example.test");
    embed::set_function_cache_verify(cx, true);
    embed::set_function_cache_dir(cx, Some(dir.clone()));
    assert_eq!(eval_string(cx, cx.initial_realm(), renamed), "10");
    let stats = embed::function_cache_stats(cx);
    assert!(stats.hits >= 1, "{stats:?}");
    assert_eq!(stats.verify_mismatches, 0);
    cx.drop();

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn heap_size_follows_the_live_data() {
    const MIN_HEAP: usize = 8 * 1024 * 1024;
    let (cx, _recorder) = new_context_with_heap_size(MIN_HEAP);
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();

    // Garbage with a small live set: the heap collects instead of growing (it used to double on
    // every full semispace up to the maximum).
    eval(cx, realm, "for (let i = 0; i < 400000; i++) { var o = { a: i, b: [i, i] }; }");
    let churn = embed::heap_stats(cx);
    assert!(churn.gc_count > 0, "{churn:?}");
    assert_eq!(churn.gc_grow_count, 0, "{churn:?}");
    assert_eq!(churn.heap_size_bytes, MIN_HEAP, "{churn:?}");
    #[cfg(target_os = "linux")]
    assert!(churn.released_bytes > 0, "{churn:?}");

    // A growing live set grows the heap, keeping at least half of the semispace free.
    eval(
        cx,
        realm,
        "globalThis.keep = []; for (let i = 0; i < 300000; i++) keep.push({ a: i, b: [i] });",
    );
    let grown = embed::heap_stats(cx);
    assert!(grown.gc_grow_count > 0, "{grown:?}");
    assert!(grown.heap_size_bytes > MIN_HEAP, "{grown:?}");
    assert!(grown.peak_live_bytes > MIN_HEAP / 2, "{grown:?}");
    assert_eq!(eval_string(cx, realm, "String(keep.length + keep[299999].a)"), "599999");

    // When the live set is dropped the heap shrinks back as garbage keeps coming.
    eval(
        cx,
        realm,
        "keep = null; for (let i = 0; i < 2000000; i++) { var o = { a: i, b: [i, i] }; }",
    );
    let shrunk = embed::heap_stats(cx);
    assert!(shrunk.gc_shrink_count > 0, "{shrunk:?}");
    assert!(shrunk.heap_size_bytes < grown.heap_size_bytes, "{shrunk:?}");
    assert!(shrunk.last_live_bytes * 8 < shrunk.semispace_capacity_bytes, "{shrunk:?}");
}

#[test]
fn census_counts_live_items_by_kind() {
    let options = OptionsBuilder::new().gc_census(true).build().unwrap();
    let cx = ContextBuilder::new()
        .set_options(Rc::new(options))
        .build()
        .unwrap();
    embed::install_host(cx, Box::new(TestHost(Rc::new(Recorder::default())))).unwrap();
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    assert!(embed::heap_census(cx).is_empty());

    eval(
        cx,
        realm,
        "globalThis.keep = []; for (let i = 0; i < 1000; i++) keep.push([i]);",
    );
    embed::gc(cx);
    let census = embed::heap_census(cx);
    let total: usize = census.iter().map(|entry| entry.bytes).sum();
    assert_eq!(total, embed::heap_stats(cx).last_live_bytes);
    assert!(census.windows(2).all(|pair| pair[0].bytes >= pair[1].bytes));
    let arrays = census
        .iter()
        .find(|entry| entry.kind == "ArrayObject")
        .expect("arrays counted");
    assert!(arrays.count >= 1001, "{census:?}");
}

#[test]
fn rejecting_a_settled_or_already_resolved_promise_does_nothing() {
    let (cx, _recorder) = new_context();
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    embed::with_realm(cx, realm, |cx| {
        let realm = cx.current_realm();
        // Settled: fulfilled, then an embedder reject (Servo's abort_fetch after the fetch finished).
        let fulfilled = embed::api::new_promise(cx, realm).unwrap();
        embed::api::promise_resolve(cx, fulfilled, cx.number(1.0)).unwrap();
        assert!(!embed::api::promise_reject(cx, fulfilled, cx.number(2.0)));
        assert_eq!(embed::api::promise_state(fulfilled), embed::api::PromiseStatus::Fulfilled);

        // Rejected twice: the first reason stays.
        let rejected = embed::api::new_promise(cx, realm).unwrap();
        assert!(embed::api::promise_reject(cx, rejected, cx.number(3.0)));
        assert!(!embed::api::promise_reject(cx, rejected, cx.number(4.0)));
        assert_eq!(
            embed::api::promise_result(cx, rejected)
                .unwrap()
                .as_number(),
            3.0
        );

        // Pending but already resolved to another (pending) promise: still pending afterwards.
        let inner = embed::api::new_promise(cx, realm).unwrap();
        let outer = embed::api::new_promise(cx, realm).unwrap();
        embed::api::promise_resolve(cx, outer, inner.as_object().as_value()).unwrap();
        assert!(!embed::api::promise_reject(cx, outer, cx.number(5.0)));
        assert_eq!(embed::api::promise_state(outer), embed::api::PromiseStatus::Pending);
    });
}

#[test]
fn compile_and_lazy_generation_times_are_recorded() {
    let (cx, _recorder) = new_context();
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    let source = "function f(x) { return x * 2; } var y = 0; for (let i = 0; i < 100; i++) y += i;";
    let script = embed::compile_script(cx, realm, source, "times.js", 1).unwrap();
    let times = embed::last_compile_times();
    assert!(
        times.parse > std::time::Duration::ZERO && times.generate > std::time::Duration::ZERO,
        "{times:?}"
    );
    let before = embed::lazy_generate_time();
    embed::execute_compiled(cx, script).unwrap();
    assert_eq!(eval_string(cx, realm, "String(f(21))"), "42");
    let after = embed::lazy_generate_time();
    assert!(
        after.functions > before.functions,
        "f's stub was generated on its first call: {after:?}"
    );

    let parsed = embed::parse_script_off_thread("var z = 1;", "off.js", 1).unwrap();
    embed::compile_parsed_script(cx, realm, parsed).unwrap();
    assert!(embed::last_compile_times().parse > std::time::Duration::ZERO);
}

/// A grow driven by a large allocation sizes the heap for the live data as well: with 10 MB live in a 64 MB heap, a 60 MB
/// ArrayBuffer needs a semispace of more than 70 MB. It used to get 128 MB (semispace about 64 MB) and run out of heap memory.
#[test]
fn large_allocation_grows_the_heap_for_live_data_too() {
    const MIN_HEAP: usize = 64 * 1024 * 1024;
    let (cx, _recorder) = new_context_with_heap_size(MIN_HEAP);
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();

    eval(
        cx,
        realm,
        "globalThis.keep = []; for (let i = 0; i < 80000; i++) keep.push({ a: i, b: [i] });",
    );
    embed::gc(cx);
    let before = embed::heap_stats(cx);
    assert_eq!(before.heap_size_bytes, MIN_HEAP, "{before:?}");
    assert!(before.last_live_bytes > 8 * 1024 * 1024, "{before:?}");
    assert!(before.last_live_bytes < 16 * 1024 * 1024, "{before:?}");

    assert_eq!(
        eval_string(
            cx,
            realm,
            "globalThis.big = new ArrayBuffer(60 * 1024 * 1024); String(big.byteLength)"
        ),
        "62914560"
    );
    let after = embed::heap_stats(cx);
    assert!(after.heap_size_bytes > MIN_HEAP, "{after:?}");
    assert!(
        after.semispace_capacity_bytes >= before.last_live_bytes + 60 * 1024 * 1024,
        "{after:?}"
    );
    assert_eq!(eval_string(cx, realm, "String(keep.length + keep[80000 - 1].a)"), "159999");
}

#[test]
fn function_cache_store_with_another_secret_gets_no_hits() {
    let dir = std::env::temp_dir().join(format!("fncache-secret-embed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let script = "function k1(a, b) { var s = 0; for (var i = a; i < b; i++) { s += i; } return s; } \
                  String(k1(1, 5))";

    let (cx, _) = new_context();
    embed::set_function_cache_enabled(cx, true);
    embed::set_realm_cache_partition(cx, cx.initial_realm(), b"https://example.test");
    embed::set_function_cache_dir(cx, Some(dir.clone()));
    assert_eq!(eval_string(cx, cx.initial_realm(), script), "10");
    embed::function_cache_flush(cx);
    cx.drop();

    // The same store under its own secret hits.
    let (cx, _) = new_context();
    embed::set_function_cache_enabled(cx, true);
    embed::set_realm_cache_partition(cx, cx.initial_realm(), b"https://example.test");
    embed::set_function_cache_dir(cx, Some(dir.clone()));
    assert_eq!(eval_string(cx, cx.initial_realm(), script), "10");
    assert!(embed::function_cache_stats(cx).hits >= 1);
    cx.drop();

    // Losing the secret file means a new secret: the old records never hit.
    std::fs::remove_file(dir.join("secret.key")).unwrap();
    let (cx, _) = new_context();
    embed::set_function_cache_enabled(cx, true);
    embed::set_realm_cache_partition(cx, cx.initial_realm(), b"https://example.test");
    embed::set_function_cache_dir(cx, Some(dir.clone()));
    assert_eq!(eval_string(cx, cx.initial_realm(), script), "10");
    let stats = embed::function_cache_stats(cx);
    assert_eq!(stats.hits, 0, "{stats:?}");
    assert!(stats.misses >= 1);
    cx.drop();

    let _ = std::fs::remove_dir_all(&dir);
}

/// A second realm in the context, with the same optional globals as the initial one.
fn second_realm(cx: Context) -> Handle<Realm> {
    let mut realm = Realm::new(cx).unwrap();
    realm.install_optional_globals(cx).unwrap();
    realm
}

const PARTITION_SCRIPT: &str = "function k1(a, b) { var s = 0; for (var i = a; i < b; i++) { s += i; } return s; } \
                                String(k1(1, 5))";
const PARTITION_RENAMED: &str = "function m1(p, q) { var t = 0; for (var j = p; j < q; j++) { t += j; } return t; } \
                                 String(m1(1, 5))";

#[test]
fn function_cache_has_no_hits_across_partitions() {
    let (cx, _) = new_context();
    embed::set_function_cache_enabled(cx, true);
    let first = cx.initial_realm();
    let other = second_realm(cx);
    embed::set_realm_cache_partition(cx, first, b"https://a.example");
    embed::set_realm_cache_partition(cx, other, b"https://b.example");

    assert_eq!(eval_string(cx, first, PARTITION_SCRIPT), "10");
    let filled = embed::function_cache_stats(cx);
    assert!(filled.misses >= 1);

    // The same structure from another site: no hit, whatever the first site has cached.
    assert_eq!(eval_string(cx, other, PARTITION_RENAMED), "10");
    let cross = embed::function_cache_stats(cx);
    assert_eq!(cross.hits, 0, "{cross:?}");

    // The first site's own reuse still works.
    assert_eq!(eval_string(cx, first, PARTITION_RENAMED), "10");
    assert!(embed::function_cache_stats(cx).hits >= 1);
    cx.drop();
}

#[test]
fn function_cache_hits_within_one_partition() {
    let (cx, _) = new_context();
    embed::set_function_cache_enabled(cx, true);
    embed::set_function_cache_verify(cx, true);
    let first = cx.initial_realm();
    let other = second_realm(cx);
    embed::set_realm_cache_partition(cx, first, b"https://a.example");
    embed::set_realm_cache_partition(cx, other, b"https://a.example");

    assert_eq!(eval_string(cx, first, PARTITION_SCRIPT), "10");
    assert_eq!(eval_string(cx, other, PARTITION_RENAMED), "10");
    let stats = embed::function_cache_stats(cx);
    assert!(stats.hits >= 1, "{stats:?}");
    assert_eq!(stats.verify_mismatches, 0);
    cx.drop();
}

#[test]
fn function_cache_realm_without_partition_neither_hits_nor_stores() {
    let (cx, _) = new_context();
    embed::set_function_cache_enabled(cx, true);
    let realm = cx.initial_realm();
    assert_eq!(eval_string(cx, realm, PARTITION_SCRIPT), "10");
    assert_eq!(eval_string(cx, realm, PARTITION_RENAMED), "10");
    let stats = embed::function_cache_stats(cx);
    assert_eq!(stats.lookups, 0, "{stats:?}");
    assert_eq!(stats.bytes_stored, 0);

    // An empty partition is no partition.
    embed::set_realm_cache_partition(cx, realm, b"");
    assert_eq!(eval_string(cx, realm, "function z1(a) { return a * 2; } String(z1(4))"), "8");
    let stats = embed::function_cache_stats(cx);
    assert_eq!(stats.lookups, 0, "{stats:?}");
    assert_eq!(stats.bytes_stored, 0);
    cx.drop();
}

#[test]
fn function_cache_disk_store_reopened_under_another_partition_gets_no_hits() {
    let dir = std::env::temp_dir().join(format!("fncache-partition-embed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let run = |partition: &[u8], script: &str| {
        let (cx, _) = new_context();
        embed::set_function_cache_enabled(cx, true);
        embed::set_function_cache_dir(cx, Some(dir.clone()));
        embed::set_realm_cache_partition(cx, cx.initial_realm(), partition);
        assert_eq!(eval_string(cx, cx.initial_realm(), script), "10");
        embed::function_cache_flush(cx);
        let stats = embed::function_cache_stats(cx);
        cx.drop();
        stats
    };

    run(b"https://a.example", PARTITION_SCRIPT);
    let same = run(b"https://a.example", PARTITION_RENAMED);
    assert!(same.hits >= 1, "{same:?}");
    let other = run(b"https://b.example", PARTITION_RENAMED);
    assert_eq!(other.hits, 0, "{other:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Functions of many kinds whose bodies first run after their script has finished, so that they
/// are parsed again on their own. `REPARSE_EXPECTED` is what `REPARSE_CALLS` evaluates to.
const REPARSE_SCRIPT: &str = r#"
var lib = (function (outerParam) {
    'use strict';
    let counter = 0;
    const base = 10;
    function decl(a, b = base) { counter++; return a + b + outerParam; }
    var expr = function (x) { return x * 2 + counter; };
    var named = function fact(n) { return n <= 1 ? 1 : n * fact(n - 1); };
    var arrow = (x, ...rest) => x + rest.length + base;
    var asyncArrow = async (x) => x + 1;
    function makeCounter() { let n = 0; return { inc() { return ++n; }, get value() { return n; }, set value(v) { n = v; } }; }
    function* gen(limit) { for (let i = 0; i < limit; i++) yield i * base; }
    function usesArguments() { return arguments.length + [].slice.call(arguments).join('-'); }
    function withArrowThis() { return [1, 2].map(v => this.k * v + arguments[0]); }
    function newTarget() { return new.target === undefined ? 'call' : 'new'; }
    function arrowNewTarget() { return (() => new.target === undefined)(); }
    function destructure({ a, b: [c, d = 4] = [] }, [e] = [5]) { return a + c + d + e; }
    function tdz() { try { return late; } catch (e) { return e.constructor.name; } finally { } let late = 1; }
    function labels() { outer: for (var i = 0; i < 3; i++) { for (var j = 0; j < 3; j++) { if (j == 1) continue outer; if (i == 2) break outer; } } return i * 10 + j; }
    function deep() { function a() { function b() { return () => () => counter + base; } return b()()(); } return a(); }
    function closures() { var fns = []; for (let i = 0; i < 3; i++) fns.push(() => i); return fns.map(f => f()).join(''); }
    class Point {
        #secret = 7;
        static count = 0;
        constructor(x) { this.x = x; Point.count++; }
        get double() { return this.x * 2; }
        static make(x) { return new Point(x); }
        reveal() { return this.#secret + this.x; }
        #hidden() { return 'h' + this.x; }
        callHidden() { return this.#hidden(); }
        arrowField = () => this.x + 100;
        ['comp' + 'uted']() { return 'computed' + this.x; }
        async asyncMethod() { return this.x; }
        *genMethod() { yield this.x; }
    }
    class Point3 extends Point {
        constructor(x, z) { super(x); this.z = z; }
        sum() { return super.reveal() + this.z; }
        get double() { return super.double + 1; }
    }
    var proto = { greet() { return 'proto'; } };
    var child = { __proto__: proto, greet() { return super.greet() + '+child'; } };
    return { decl, expr, named, arrow, asyncArrow, makeCounter, gen, usesArguments, withArrowThis, newTarget,
             arrowNewTarget, destructure, tdz, labels, deep, closures, Point, Point3, child, k: 3 };
})(1);
function sloppy() { function inner() { return typeof this; } return inner(); }
function withEval(x) { var y = 2; return eval('x + y'); }
function withWith() { var o = { p: 5 }; with (o) { return p + 1; } }
function hoisted() { return later(); function later() { return 'later'; } }
var obj = { method() { return 'method'; }, get g() { return 'getter'; }, set s(v) { this._s = v; }, 'quoted key'() { return 'quoted'; }, 42() { return 'numeric'; }, async am() {}, *gm() { yield 1; } };
var anon = function () { return anon.name; };
var arrowName = () => arrowName.name;
"#;

const REPARSE_CALLS: &str = r#"
var c = lib.makeCounter(); c.inc(); c.inc(); c.value = 5;
var p = lib.Point.make(4); var p3 = new lib.Point3(2, 3);
[lib.decl(1), lib.expr(2), lib.named(5), lib.arrow(1, 2, 3), c.value, c.inc(), [...lib.gen(3)].join(','),
 lib.usesArguments(1, 2), lib.withArrowThis.call({ k: 3 }, 1).join(','), lib.newTarget(), new lib.newTarget() instanceof Object,
 lib.arrowNewTarget(), lib.destructure({ a: 1, b: [2] }), lib.tdz(), lib.labels(), lib.deep(), lib.closures(),
 p.double, p.reveal(), p.callHidden(), p.arrowField(), p.computed(), [...p.genMethod()].join(), p3.sum(), p3.double,
 lib.child.greet(), sloppy(), withEval(1), withWith(), hoisted(), obj.method(), obj.g, (obj.s = 9, obj._s),
 obj['quoted key'](), obj[42](), [...obj.gm()].join(), anon(), arrowName(), lib.Point.count, lib.named.name, lib.expr.name].join('|')
"#;

const REPARSE_EXPECTED: &str = "12|5|120|13|5|6|0,10,20|21-2|4,7|call|true|true|12|ReferenceError|20|11|012|8|11|h4|104|computed4|4|12|5|proto+child|object|3|6|later|method|getter|9|quoted|numeric|1|anon|arrowName|2|fact|expr";

#[test]
fn reparse_parses_functions_again_after_the_script_ran() {
    let (cx, _recorder) = new_context_with_options(OptionsBuilder::new().lazy_reparse(true));
    handle_scope_guard!(cx);
    if !cx.options.lazy_functions {
        return;
    }
    let realm = cx.initial_realm();
    eval(cx, realm, REPARSE_SCRIPT);

    // The script has run: its AST is gone, its scope tree stays
    let after_script = embed::reparse_stats(cx);
    assert!(after_script.programs_dropped >= 1, "{after_script:?}");
    assert!(after_script.detached > 10, "{after_script:?}");
    assert_eq!(embed::retained_parse_bytes(cx), 0);
    assert!(embed::retained_scope_bytes(cx) > 0);

    assert_eq!(eval_string(cx, realm, REPARSE_CALLS), REPARSE_EXPECTED);
    let stats = embed::reparse_stats(cx);
    assert!(stats.reparsed > 20, "{stats:?}");
    assert_eq!(stats.reparse_failed, 0, "{stats:?}");

    // A GC in between keeps detached stubs working
    embed::gc(cx);
    assert_eq!(eval_string(cx, realm, "String(lib.decl(2) + lib.deep())"), "25");
}

#[test]
fn reparse_verify_mode_finds_no_difference_to_the_original_ast() {
    let options = OptionsBuilder::new()
        .lazy_reparse(true)
        .lazy_reparse_verify(true);
    let (cx, _recorder) = new_context_with_options(options);
    handle_scope_guard!(cx);
    if !cx.options.lazy_functions {
        return;
    }
    let realm = cx.initial_realm();
    eval(cx, realm, REPARSE_SCRIPT);
    assert_eq!(eval_string(cx, realm, REPARSE_CALLS), REPARSE_EXPECTED);

    // Every function of both programs, called or not, generated by re-parsing and compared
    embed::generate_all_lazy_functions(cx).unwrap();
    let stats = embed::reparse_stats(cx);
    assert!(stats.reparsed > 40, "{stats:?}");
    assert_eq!(stats.verify_mismatches, 0, "{stats:?}");
    assert_eq!(stats.reparse_failed, 0, "{stats:?}");
    assert_eq!(cx.lazy_functions.num_pending(), 0);
}

#[test]
fn without_reparse_the_same_script_gives_the_same_results() {
    let (cx, _recorder) = new_context_with_options(OptionsBuilder::new().lazy_reparse(false));
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    eval(cx, realm, REPARSE_SCRIPT);
    assert_eq!(eval_string(cx, realm, REPARSE_CALLS), REPARSE_EXPECTED);
    assert_eq!(embed::reparse_stats(cx).reparsed, 0);
}

#[test]
fn source_text_lives_outside_the_heap_and_survives_collections() {
    let (cx, _recorder) = new_context();
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    eval(
        cx,
        realm,
        "function answer(a, b) {\n  return a + b; // the text\n}\nfunction thrower() {\n  throw new Error('x');\n}",
    );
    assert!(cx.source_texts.len() >= 1);
    let text_bytes = cx.source_texts.text_bytes();
    assert!(text_bytes > 50, "{text_bytes}");

    for _ in 0..3 {
        embed::gc(cx);
    }
    assert_eq!(
        eval_string(cx, realm, "String(answer)"),
        "function answer(a, b) {\n  return a + b; // the text\n}"
    );
    // The stack trace reads line and column from the text
    let stack = eval_string(cx, realm, "try { thrower(); } catch (e) { e.stack }");
    assert!(stack.contains(":5:"), "{stack}");

    // The heap holds the file, not its text
    let live = embed::heap_stats(cx).last_live_bytes;
    embed::gc(cx);
    assert!(embed::heap_stats(cx).last_live_bytes <= live + 4096);
}

#[test]
fn gc_stress_window_collects_often_and_keeps_values_intact() {
    // 64 KiB windows: a few MB of allocation must collect dozens of times, with the from-space poisoned each time, and
    // every value the script keeps must still read back right.
    let (cx, _recorder) =
        new_context_with_options(OptionsBuilder::new().gc_stress_window(Some(64 << 10)));
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    let before = embed::heap_stats(cx).gc_count;
    let source = "var keep = []; for (var i = 0; i < 20000; i++) { var o = { i: i, s: 'v' + i, a: [i, i + 1] }; \
                  if (i % 100 == 0) keep.push(o); } \
                  var ok = keep.every(function (o, k) { return o.i == k * 100 && o.s == 'v' + o.i && o.a[1] == o.i + 1; }); \
                  String(ok) + ' ' + keep.length";
    assert_eq!(eval_string(cx, realm, source), "true 200");
    let collections = embed::heap_stats(cx).gc_count - before;
    assert!(collections >= 20, "only {collections} collections");
}

#[test]
fn gc_verify_finds_no_bad_pointers_in_a_busy_heap() {
    // The verifier aborts the process on a bad pointer, so passing means every collection (before and after) checked clean.
    let options = OptionsBuilder::new()
        .gc_stress_window(Some(32 << 10))
        .gc_verify(true);
    let (cx, _recorder) = new_context_with_options(options);
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    let before = embed::heap_stats(cx).gc_count;
    let source = "var out = [];\
                  for (let i = 0; i < 3000; i++) {\
                    let o = { get g() { return i; }, set s(v) { this.v = v; } }; o.s = i;\
                    let m = new Map([[i, 'x' + i]]); let w = new WeakMap(); w.set(o, m);\
                    class C { #p = i; get p() { return this.#p; } } let c = new C();\
                    let r = /a(b+)c/g.exec('xxabbbc' + i); let j = JSON.parse(JSON.stringify({ a: [i, { b: 'q' + i }] }));\
                    let f = (function (k) { return function () { return k + o.g; }; })(i);\
                    if (i % 300 == 0) out.push([o, m, c, r, j, f, new WeakRef(o)]);\
                  }\
                  out.every(function (e) { return e[0].v === e[0].g && e[2].p === e[0].g && e[3][1] === 'bbb' && e[5]() === 2 * e[0].g; })";
    assert_eq!(eval_string(cx, realm, source), "true");
    assert!(embed::heap_stats(cx).gc_count - before >= 20);
}

/// The collector repairs a weak edge only along the structure it can name by hand: the transition
/// tree from its roots, the one interned strings set the context field points at. Everything else
/// that a collection moves has to be caught by the pass over the new semispace.
#[test]
fn gc_verify_repairs_the_weak_edges_of_holders_the_named_walks_miss() {
    // The verifier aborts the process on a bad pointer, so passing means every collection (before and after) checked clean.
    let options = OptionsBuilder::new()
        .gc_stress_window(Some(32 << 10))
        .gc_verify(true);
    let (cx, _recorder) = new_context_with_options(options);
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    let before = embed::heap_stats(cx).gc_count;

    // Objects on a private [[IsHTMLDDA]] shape: the clone is taken out of the transition tree, so
    // neither its own edge nor the chain it transitions into is anywhere the tree walk reaches.
    let mut names = String::new();
    for i in 0..12 {
        let object = embed::with_realm(cx, realm, |cx| {
            let mut object = ordinary_object_create(cx).unwrap();
            object.init_htmldda(cx).unwrap();
            object
        });
        define_global(cx, realm, &format!("all{i}"), object.as_value());
        if i > 0 {
            names.push_str(", ");
        }
        names.push_str(&format!("all{i}"));
    }

    // The churn allocates enough to collect constantly, so a collection lands between every
    // property add and every delete, and the new property names grow the interned strings set the
    // same way while the old one is still rooted by the handle of the code that grew it.
    let source = format!(
        "var alls = [{names}];\
         var churn = [];\
         for (var i = 0; i < 300; i++) {{\
           for (var j = 0; j < alls.length; j++) alls[j]['p' + i + '_' + j] = i;\
           var o = {{}};\
           for (var k = 0; k < 40; k++) o['c' + k + i] = [k, 'v' + k + i];\
           churn.push(o);\
           if (i % 5 == 4) for (var j = 0; j < alls.length; j++) delete alls[j]['p' + (i - 4) + '_' + j];\
         }}\
         var ok = churn.length == 300;\
         for (var j = 0; j < alls.length; j++) {{\
           ok = ok && typeof alls[j] == 'undefined';\
           for (var n = 0; n < 300; n++) {{\
             var name = 'p' + n + '_' + j;\
             ok = ok && (name in alls[j]) == (n % 5 != 0);\
             if (n % 5 != 0) ok = ok && alls[j][name] == n;\
           }}\
         }}\
         ok",
        names = names
    );
    assert_eq!(eval_string(cx, realm, &source), "true");
    assert!(embed::heap_stats(cx).gc_count - before >= 20);
}

/// The collector repairs the weak slots of a WeakRef, WeakSet, WeakMap, FinalizationRegistry,
/// prototype children vec or inline cache only through the intrusive lists, and an item enters a
/// list only when a collection moves it (`copy_or_fix_pointer`). A permanent item is never moved by
/// a normal collection, so one of these kinds in the permanent space would keep a pointer into the
/// vacated semispace after the first collection once the mutator wrote a semispace item into it: an
/// inline cache hit on a recycled shape address. That is safe only because the permanent space
/// holds none of them, which is what this checks (the serialized heap is built by the same
/// initialization, `brimstone_serialized_heap/build.rs`).
#[test]
fn gc_verify_the_permanent_space_holds_no_item_whose_weak_slots_only_a_list_repairs() {
    use crate::runtime::gc::{HeapItemKind, verify::items_of_space};

    let (cx, _recorder) = new_context_with_options(OptionsBuilder::new());
    let items = items_of_space(cx.heap.permanent_heap_bounds());
    let shapes = items
        .iter()
        .filter(|(_, kind)| *kind == HeapItemKind::Shape)
        .count();
    assert!(shapes > 100, "the walk of the permanent space found only {shapes} shapes");

    let list_kinds = [
        HeapItemKind::WeakRefObject,
        HeapItemKind::WeakSetObject,
        HeapItemKind::WeakValueSet,
        HeapItemKind::WeakMapObject,
        HeapItemKind::WeakValueMap,
        HeapItemKind::FinalizationRegistryObject,
        HeapItemKind::FinalizationRegistryCells,
        HeapItemKind::PrototypeObjectChildrenShapesVec,
        HeapItemKind::CacheArray,
        HeapItemKind::PolymorphicCacheArray,
    ];
    for (item, kind) in &items {
        assert!(
            !list_kinds.contains(kind),
            "{kind:?} at {:#x} in the permanent space: no collection repairs its weak slots",
            item.as_ptr() as usize
        );
    }
}

/// A prototype object shape is cut out of the transition tree when it is made
/// (`clone_as_prototype_object_shape`) and every transition on it changes it in place, so it never
/// holds a next-shape edge or a transition vec. The verifier checks that at every collection; this
/// makes prototype shapes, then adds, deletes, re-parents and freezes their objects across
/// collections, and counts the prototype shapes so the check is not vacuous.
#[test]
fn gc_verify_prototype_object_shapes_never_hold_a_transition_edge() {
    use crate::runtime::{
        gc::{HeapItemKind, verify::items_of_space},
        shape::Shape,
    };

    let options = OptionsBuilder::new()
        .gc_stress_window(Some(32 << 10))
        .gc_verify(true);
    let (cx, _recorder) = new_context_with_options(options);
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    let before = embed::heap_stats(cx).gc_count;

    // `twin` has the shape P starts with and transitions out of it, so the shape P's prototype shape
    // is cloned from holds an edge. Each `get` is freshly compiled, so its inline caches stay
    // monomorphic and ask for a validity guard on P, which is what converts P's shape.
    let source = "var protos = [];\
         for (var i = 0; i < 200; i++) {\
           var P = { m: function () { return 1; } };\
           var twin = { m: 0 };\
           twin['t' + i] = i;\
           var g = Object.create(P);\
           var get = new Function('o', 'return o.m() + o.toString.length;');\
           for (var k = 0; k < 4; k++) get(g);\
           P['a' + i] = i;\
           P['b' + i] = i;\
           delete P['a' + i];\
           Object.setPrototypeOf(P, i % 2 ? Array.prototype : Object.prototype);\
           if (i % 7 == 0) Object.preventExtensions(P);\
           for (var k = 0; k < 4; k++) get(g);\
           protos.push([g, twin]);\
           if (protos.length > 40) protos.shift();\
           var junk = [];\
           for (var k = 0; k < 30; k++) junk.push({ k: k, s: 'x' + k + i });\
         }\
         protos.length";
    assert_eq!(eval_string(cx, realm, source), "40");
    assert!(embed::heap_stats(cx).gc_count - before >= 20);

    let current = cx.heap.current_heap_bounds();
    let current = current.start..unsafe { current.start.add(cx.heap.bytes_allocated()) };
    let mut prototype_shapes = 0;
    for (item, kind) in items_of_space(current) {
        if kind != HeapItemKind::Shape {
            continue;
        }
        let shape = item.cast::<Shape>();
        if shape.is_prototype_object() {
            prototype_shapes += 1;
            assert!(shape.transitions_or_next_shape().is_none());
        }
    }
    assert!(
        prototype_shapes >= 10,
        "only {prototype_shapes} prototype object shapes were made"
    );
}

/// Run `setup`, root the container `container` reads off its result with a handle of this scope,
/// the way the code that grows a container keeps the one it replaces (`maybe_grow_for_insertion`,
/// `maybe_grow_for_push`), make `grow` outgrow it, then collect. The outgrown container is moved
/// because the handle names it, and its weak slots still point at what `setup` put in it, which the
/// collection moves too: the verifier (it aborts the process) checks that they were repaired.
fn collect_with_an_outgrown_container(
    setup: &str,
    container: impl Fn(Handle<Value>) -> HeapPtr<crate::runtime::gc::AnyHeapItem>,
    grow: &str,
) {
    let options = OptionsBuilder::new().gc_verify(true);
    let (cx, _recorder) = new_context_with_options(options);
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    let value = eval(cx, realm, setup);
    let outgrown = container(value).to_handle();
    eval(cx, realm, grow);
    assert!(!container(value).ptr_eq(&outgrown), "the container was not outgrown");
    let before = embed::heap_stats(cx).gc_count;
    embed::gc(cx);
    embed::gc(cx);
    assert!(embed::heap_stats(cx).gc_count - before >= 2);
}

#[test]
fn gc_verify_repairs_an_outgrown_weak_set_container() {
    use crate::runtime::intrinsics::weak_set_object::WeakSetObject;
    collect_with_an_outgrown_container(
        "var keep = [{}]; var ws = new WeakSet(); ws.add(keep[0]); ws",
        |ws| {
            ws.as_object()
                .cast::<WeakSetObject>()
                .weak_set_data()
                .cast()
        },
        "for (var i = 0; i < 64; i++) { var o = {}; keep.push(o); ws.add(o); }",
    );
}

#[test]
fn gc_verify_repairs_an_outgrown_weak_map_container() {
    use crate::runtime::intrinsics::weak_map_object::WeakMapObject;
    collect_with_an_outgrown_container(
        "var keep = [{}]; var wm = new WeakMap(); wm.set(keep[0], { v: 0 }); wm",
        |wm| {
            wm.as_object()
                .cast::<WeakMapObject>()
                .weak_map_data()
                .cast()
        },
        "for (var i = 0; i < 64; i++) { var o = {}; keep.push(o); wm.set(o, { v: i }); }",
    );
}

#[test]
fn gc_verify_repairs_outgrown_finalization_registry_cells() {
    use crate::runtime::intrinsics::finalization_registry_object::FinalizationRegistryObject;
    collect_with_an_outgrown_container(
        "var keep = [{}]; var fr = new FinalizationRegistry(function () {});\
         fr.register(keep[0], 0, keep[0]); fr",
        |fr| {
            fr.as_object()
                .cast::<FinalizationRegistryObject>()
                .cells()
                .cast()
        },
        "for (var i = 0; i < 64; i++) { var o = {}; keep.push(o); fr.register(o, i, o); }",
    );
}

#[test]
fn gc_verify_repairs_an_outgrown_transition_vec() {
    // `a` and `b` leave the root shape of `{}` with two transitions, so it holds a transition vec;
    // the loop adds a transition per new property name until the vec is replaced.
    collect_with_an_outgrown_container(
        "var keep = []; var a = {}; a.x0 = 1; var b = {}; b.y0 = 1; a",
        |a| {
            let root = a.as_object().shape().parent_shape_ptr().unwrap();
            assert!(root.has_transitions_vec());
            root.transitions_or_next_shape().unwrap()
        },
        "for (var i = 0; i < 64; i++) { var o = {}; o['z' + i] = i; keep.push(o); }",
    );
}

#[test]
fn gc_verify_repairs_an_outgrown_transition_vec_in_the_semispace() {
    // The root shape of `{}` is in the permanent space; one for a prototype made at run time, and
    // so its transition vecs, are in the semispace.
    collect_with_an_outgrown_container(
        "var keep = []; var P = {}; var a = Object.create(P); a.x0 = 1;\
         var b = Object.create(P); b.y0 = 1; a",
        |a| {
            let root = a.as_object().shape().parent_shape_ptr().unwrap();
            assert!(root.has_transitions_vec());
            root.transitions_or_next_shape().unwrap()
        },
        "for (var i = 0; i < 64; i++) { var o = Object.create(P); o['z' + i] = i; keep.push(o); }",
    );
}

/// Messages of the one-reflector-per-host-object check for two finalizable host objects whose
/// reserved slot 0 holds the given raw bits.
fn duplicate_slot_messages_for(first: u64, second: u64) -> Vec<String> {
    let options = OptionsBuilder::new().gc_verify(true);
    let (cx, _recorder) = new_context_with_options(options);
    handle_scope_guard!(cx);
    let a = new_test_host_object(cx, HostFlags::HAS_FINALIZE, 1);
    embed::set_reserved_slot(*a, 0, Value::from_raw_bits(first));
    let b = new_test_host_object(cx, HostFlags::HAS_FINALIZE, 1);
    embed::set_reserved_slot(*b, 0, Value::from_raw_bits(second));
    let headers = embed::host_headers_for_verify(cx);
    crate::runtime::gc::verify::duplicate_host_slot_messages(&headers)
}

/// The glue's `PrivateValue(nullptr)`: the double +0.0 in the negated-double encoding.
const NULL_PRIVATE_BITS: u64 = u64::MAX;

#[test]
fn gc_verify_two_host_objects_with_a_null_private_slot_are_legal() {
    // Servo writes PrivateValue(nullptr) into window proxies at creation and after a transplant;
    // two of them alive at one collection is a legal state.
    let messages = duplicate_slot_messages_for(NULL_PRIVATE_BITS, NULL_PRIVATE_BITS);
    assert!(messages.is_empty(), "{messages:?}");
}

#[test]
fn gc_verify_two_host_objects_with_one_non_null_private_are_reported() {
    let messages = duplicate_slot_messages_for(!0x7f00_dead_b000u64, !0x7f00_dead_b000u64);
    assert_eq!(messages.len(), 1, "{messages:?}");
}

#[test]
fn gc_verify_host_objects_with_different_non_null_privates_are_not_reported() {
    let messages = duplicate_slot_messages_for(!0x7f00_dead_b000u64, !0x7f00_dead_b010u64);
    assert!(messages.is_empty(), "{messages:?}");
}

#[test]
fn gc_verify_a_null_private_and_a_non_null_private_are_not_reported() {
    let messages = duplicate_slot_messages_for(NULL_PRIVATE_BITS, !0x7f00_dead_b000u64);
    assert!(messages.is_empty(), "{messages:?}");
}

#[test]
fn associated_memory_makes_a_collection_due() {
    // SpiderMonkey's rule: a collection is due once the associated bytes reach max(retained, 38 MiB) x 1.5 (57 MiB) at first.
    let (cx, _recorder) = new_context();
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    let start = embed::heap_stats(cx).gc_count;
    let threshold = cx.heap.accounting().associated_threshold;
    assert_eq!(threshold, (38 << 20) * 15 / 10);

    // Below the threshold nothing changes; a small allocation does not collect.
    embed::add_associated_memory(cx, threshold - 1);
    eval(cx, realm, "var a = [1, 2, 3];");
    assert_eq!(embed::heap_stats(cx).gc_count, start);

    // Reaching it makes the next allocation collect.
    embed::add_associated_memory(cx, 1);
    eval(cx, realm, "var b = { x: 1 };");
    assert_eq!(embed::heap_stats(cx).gc_count, start + 1);

    // After the collection the threshold follows what is retained: max(57 MiB retained, 38 MiB) x 1.5 at low frequency or the
    // first collection's high frequency factor (3.0 below 100 MiB).
    let retained = cx.heap.accounting().associated_bytes;
    assert_eq!(retained, threshold);
    let after = cx.heap.accounting().associated_threshold;
    assert!(
        after == retained * 15 / 10 || after == retained * 30 / 10,
        "{after} for {retained}"
    );

    // Removals stop at zero.
    embed::remove_associated_memory(cx, usize::MAX);
    assert_eq!(cx.heap.accounting().associated_bytes, 0);
}

#[test]
fn host_space_bytes_make_a_collection_due() {
    // Host objects are GC cells in SpiderMonkey: with a 1 MiB allocation threshold (1.5 MiB at first), filling the host space
    // past it makes the next allocation collect.
    let (cx, _recorder) = new_context();
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    assert!(embed::set_gc_parameter(cx, embed::GcParam::AllocationThresholdMb, 1));
    // Thresholds take a new value from the next collection on.
    embed::gc(cx);
    let start = embed::heap_stats(cx).gc_count;
    let threshold = cx.heap.accounting().host_threshold;
    assert!(threshold >= 1 << 20 && threshold <= 3 << 20, "{threshold}");
    let mut kept = vec![];
    while embed::try_state(cx).unwrap().space.used_bytes() < threshold {
        kept.push(new_test_host_object(cx, HostFlags::empty(), 8));
    }
    eval(cx, realm, "var c = [4, 5, 6];");
    assert!(
        embed::heap_stats(cx).gc_count > start,
        "no collection after {} host objects",
        kept.len()
    );
}

#[test]
fn gc_parameters_are_checked_as_spidermonkey_checks_them() {
    let (cx, _recorder) = new_context();
    handle_scope_guard!(cx);
    use embed::GcParam::*;
    // Factors below 1.0 or above 10.0 are refused, and so is a small-heap factor below the large-heap one.
    assert!(!embed::set_gc_parameter(cx, LowFrequencyHeapGrowth, 50));
    assert!(!embed::set_gc_parameter(cx, LowFrequencyHeapGrowth, 1500));
    assert!(!embed::set_gc_parameter(cx, HighFrequencySmallHeapGrowth, 120));
    assert!(embed::set_gc_parameter(cx, HighFrequencySmallHeapGrowth, 300));
    // The small-heap size must stay below the large-heap size.
    assert!(!embed::set_gc_parameter(cx, SmallHeapSizeMaxMb, 600));
    assert!(embed::set_gc_parameter(cx, SmallHeapSizeMaxMb, 50));
    assert_eq!(cx.heap.accounting().tunables.small_heap, 50 << 20);
    // JSGC_MAX_BYTES caps the heap's growth (never below the current heap).
    assert!(embed::set_gc_parameter(cx, MaxBytes, 1 << 20));
    assert_eq!(crate::runtime::gc::Heap::effective_max_heap(cx), cx.heap.heap_size());
    assert!(embed::set_gc_parameter(cx, MaxBytes, u32::MAX));
    assert_eq!(crate::runtime::gc::Heap::effective_max_heap(cx), cx.options.max_heap_size);
}

/// Contexts for the Annex B.3.2 tests: eager functions, lazy functions, and lazy functions whose
/// bodies are parsed again on their own (checked against the full parse).
fn annex_b_contexts() -> Vec<(Context, Rc<Recorder>)> {
    vec![
        new_context_with_options(OptionsBuilder::new().annex_b(true).lazy_functions(false)),
        new_context_with_options(OptionsBuilder::new().annex_b(true).lazy_functions(true)),
        new_context_with_options(
            OptionsBuilder::new()
                .annex_b(true)
                .lazy_functions(true)
                .lazy_reparse_verify(true),
        ),
    ]
}

#[test]
fn annex_b_block_function_sets_the_function_var_when_evaluated() {
    for (cx, _recorder) in annex_b_contexts() {
        handle_scope_guard!(cx);
        let realm = cx.initial_realm();
        // FunctionDeclarationInstantiation step 32.a.i.2.b: undefined before the block,
        // 32.a.i.2.c: the block's function after the declaration is evaluated
        let source = "(function () { var before = typeof f; { function f() { return 1; } } \
                      return before + ' ' + typeof f + ' ' + f(); })()";
        assert_eq!(eval_string(cx, realm, source), "undefined function 1");
        // The copy happens at the declaration, not at block entry, and the block binding stays
        // independent of the var
        let source = "(function () { var seen; { seen = typeof f; f = 2; function f() {} f = 3; } \
                      return seen + ' ' + f; })()";
        assert_eq!(eval_string(cx, realm, source), "function 2");
        // A closure after the block sees the var; switch cases and if clauses count as blocks
        let source = "(function () { switch (1) { case 1: function g() { return 'g'; } } \
                      if (true) function h() { return 'h'; } return (function () { return g() + h(); })(); })()";
        assert_eq!(eval_string(cx, realm, source), "gh");
        // A declaration that is not evaluated leaves the var undefined
        let source = "(function () { if (false) { function f() {} } return String(f); })()";
        assert_eq!(eval_string(cx, realm, source), "undefined");
    }
}

#[test]
fn annex_b_block_function_is_skipped_when_a_var_would_be_an_early_error() {
    for (cx, _recorder) in annex_b_contexts() {
        handle_scope_guard!(cx);
        let realm = cx.initial_realm();
        // An enclosing let, a later top level let, a for-let and a destructured catch parameter
        let source = "(function () { var r = []; \
                      { let f = 1; { function f() {} } r.push(f); } \
                      r.push(typeof f); \
                      for (let k of [0]) { function k() {} } r.push(typeof k); \
                      try { throw {}; } catch ({ c }) { { function c() {} } } r.push(typeof c); \
                      return r.join(); })()";
        assert_eq!(eval_string(cx, realm, source), "1,undefined,undefined,undefined");
        let source = "(function () { { function f() {} } let f = 2; return f; })()";
        assert_eq!(eval_string(cx, realm, source), "2");
        // A parameter keeps its value (32.a.i.2: paramNames does not contain funcName)
        let source = "(function (f) { { function f() {} } return f; })(5)";
        assert_eq!(eval_string(cx, realm, source), "5");
        let source = "(function (f = 6) { { function f() {} } return f; })()";
        assert_eq!(eval_string(cx, realm, source), "6");
        // `arguments` stays the arguments object
        let source = "(function () { { function arguments() {} } return typeof arguments; })()";
        assert_eq!(eval_string(cx, realm, source), "object");
    }
}

#[test]
fn annex_b_block_function_passes_a_simple_catch_parameter_and_allows_duplicates() {
    for (cx, _recorder) in annex_b_contexts() {
        handle_scope_guard!(cx);
        let realm = cx.initial_realm();
        // B.3.4: a var may share the name of a BindingIdentifier catch parameter
        let source = "(function () { try { throw 1; } catch (f) { { function f() { return 7; } } } \
                      return f(); })()";
        assert_eq!(eval_string(cx, realm, source), "7");
        // 14.2.1 / 14.12.1: duplicate FunctionDeclarations in sloppy code; the last one wins, and
        // no var is created as one of them would conflict with it
        let source = "(function () { var r; { function a() { return 1; } function a() { return 2; } r = a(); } \
                      return r + ' ' + typeof a; })()";
        assert_eq!(eval_string(cx, realm, source), "2 undefined");
        let source = "(function () { switch (0) { case 0: function a() { return 1; } default: function a() { return 2; } } \
                      return 'ok'; })()";
        assert_eq!(eval_string(cx, realm, source), "ok");
    }
}

#[test]
fn annex_b_block_function_in_script_and_eval_code() {
    for (cx, _recorder) in annex_b_contexts() {
        handle_scope_guard!(cx);
        let realm = cx.initial_realm();
        // GlobalDeclarationInstantiation step 12: a non-configurable global var
        eval(
            cx,
            realm,
            "var annexBBefore = typeof annexBGlobal; { function annexBGlobal() { return 'g'; } }",
        );
        let source = "annexBBefore + ' ' + annexBGlobal() + ' ' + \
                      Object.getOwnPropertyDescriptor(globalThis, 'annexBGlobal').configurable";
        assert_eq!(eval_string(cx, realm, source), "undefined g false");
        // A global lexical declaration of another script: no var and no copy
        eval(cx, realm, "let annexBLexical = 1;");
        eval(cx, realm, "{ function annexBLexical() {} }");
        assert_eq!(
            eval_string(cx, realm, "annexBLexical + ' ' + ('annexBLexical' in globalThis)"),
            "1 false"
        );
        // EvalDeclarationInstantiation step 13: in a function, through the block's own binding
        // and a catch parameter of the eval code
        let source = "(function () { eval('{ function f() { f = 1; return 2; } } var r = f();'); \
                      eval('try { throw 0; } catch (g) { { function g() { return 3; } } }'); \
                      return typeof f + ' ' + r + ' ' + g(); })()";
        assert_eq!(eval_string(cx, realm, source), "function 2 3");
        // A lexical binding of the caller between the eval and its var scope: no var, no copy
        let source = "(function () { { let f = 1; eval('{ function f() {} }'); return f + ' ' + typeof f; } })()";
        assert_eq!(eval_string(cx, realm, source), "1 number");
        let source =
            "(function () { { let f = 1; eval('{ function f() {} }'); } return typeof f; })()";
        assert_eq!(eval_string(cx, realm, source), "undefined");
        // B.3.4: an eval in a catch block may declare the catch parameter's name
        let source = "(function () { try { throw 0; } catch (e) { eval('var e = 4; function e2() {}'); } return e; })()";
        assert_eq!(eval_string(cx, realm, source), "undefined");
    }
}

#[test]
fn block_function_has_no_var_without_annex_b() {
    let (cx, _recorder) = new_context_with_options(OptionsBuilder::new().annex_b(false));
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    let source = "(function () { { function f() {} } return typeof f; })()";
    assert_eq!(eval_string(cx, realm, source), "undefined");
}

/// The code must be a SyntaxError, reported as a SyntaxError object.
fn expect_syntax_error(cx: Context, realm: Handle<Realm>, source: &str) {
    let result = embed::evaluate_script(cx, realm, source, "embed_test.js", 1);
    let error = match result {
        Ok(_) => panic!("`{source}` parsed"),
        Err(error) => error,
    };
    let error = embed::exception_value(error);
    let message =
        embed::with_realm(cx, realm, |cx| to_string(cx, error).unwrap().format()).unwrap();
    assert!(message.starts_with("SyntaxError"), "`{source}`: {message}");
}

#[test]
fn arrow_function_parameters_keep_the_enclosing_await_context() {
    let (cx, _) = new_context();
    let realm = cx.initial_realm();

    // ArrowParameters[?Yield, ?Await] inherits the enclosing Await goal, so `await` is still the
    // keyword inside the parameter list of a non-async arrow that sits in an async context. The
    // parameter list of a class static block and of an async arrow are both async contexts.
    for source in [
        "class C { static { (await => 0); } }",
        "class C { static { ((x = await) => 0); } }",
        "async(a = await => {}) => {};",
        "async(a = (await) => {}) => {};",
        "async(a = (...await) => {}) => {};",
        "async() => { (a = await/r/g) => {} };",
    ] {
        expect_syntax_error(cx, realm, source);
    }

    // An async arrow's own parameters are an async context too, and a nested arrow does not reset
    // the goal until its body.
    expect_syntax_error(cx, realm, "async (await) => {}");
    expect_syntax_error(cx, realm, "async function f(a = await => {}) {}");

    // Crossing into a nested function body does reset it: `await` is an ordinary identifier there.
    assert_eq!(
        eval_string(
            cx,
            realm,
            "class C { static { (() => { class await {}; return typeof await })() } }; typeof C"
        ),
        "function"
    );
    assert_eq!(
        eval_string(
            cx,
            realm,
            "(async(a = (x) => typeof await) => 1)() instanceof Promise ? 'ok' : 'bad'"
        ),
        "ok"
    );
    assert_eq!(
        eval_string(cx, realm, "(a => typeof await)() === 'undefined' ? 'ok' : 'bad'"),
        "ok"
    );
}

#[test]
fn super_property_references_take_their_base_before_converting_the_key() {
    let (cx, _) = new_context();
    let realm = cx.initial_realm();

    let prefix = r#"
        var key = { toString() { Object.setPrototypeOf(obj, proto2); return "p"; } };
    "#;

    // GetValue, PutValue, compound assignment and the increment operator all resolve
    // GetSuperBase before ToPropertyKey of the reference's name.
    let cases = [
        (
            r#"var proto = { p: "ok" }; var proto2 = { p: "bad" };
               var obj = { __proto__: proto, m() { return super[key]; } };"#,
            "String(obj.m())",
            "ok",
        ),
        (
            r#"var result;
               var proto = { set p(v) { result = "ok"; } };
               var proto2 = { set p(v) { result = "bad"; } };
               var obj = { __proto__: proto, m() { super[key] = 10; } };"#,
            "obj.m(), String(result)",
            "ok",
        ),
        (
            r#"var proto = { p: 1 }; var proto2 = { p: -1 };
               var obj = { __proto__: proto, m() { return super[key] += 1; } };"#,
            "String(obj.m())",
            "2",
        ),
        (
            r#"var proto = { p: 1 }; var proto2 = { p: -1 };
               var obj = { __proto__: proto, m() { return ++super[key]; } };"#,
            "String(obj.m())",
            "2",
        ),
    ];

    for (setup, expression, expected) in cases {
        assert_eq!(
            eval_string(cx, realm, &format!("{setup}{prefix}{expression}")),
            expected,
            "{expression}"
        );
    }
}

#[test]
fn delete_super_property_checks_this_before_the_key() {
    let (cx, _) = new_context();
    let realm = cx.initial_realm();

    // `super [ Expression ]` performs GetThisBinding before evaluating the key expression.
    assert_eq!(
        eval_string(
            cx,
            realm,
            r#"
                var baseCalled = false;
                class Base { constructor() { baseCalled = true; throw new Error("base"); } }
                class Derived extends Base { constructor() { delete super[(super(), 0)]; } }
                var error;
                try { new Derived() } catch (e) { error = e }
                String(!baseCalled && error instanceof ReferenceError)
            "#
        ),
        "true"
    );
}

#[test]
fn computed_property_names_become_keys_before_the_value() {
    let (cx, _) = new_context();
    let realm = cx.initial_realm();

    assert_eq!(
        eval_string(
            cx,
            realm,
            r#"
                var value = "bad";
                var key = { toString() { value = "ok"; return "p"; } };
                String({ [key]: value }.p)
            "#
        ),
        "ok"
    );

    // A destructuring assignment converts the name of its source property when the pattern is
    // evaluated, but the target's key only when it is stored to.
    assert_eq!(
        eval_string(
            cx,
            realm,
            r#"
                var log = [];
                function source() { log.push("source"); return { get p() { log.push("get"); } }; }
                function target() { log.push("target"); return { set q(v) { log.push("set"); } }; }
                function sourceKey() {
                    log.push("source-key");
                    return { toString() { log.push("source-key-tostring"); return "p"; } };
                }
                function targetKey() {
                    log.push("target-key");
                    return { toString() { log.push("target-key-tostring"); return "q"; } };
                }
                ({ [sourceKey()]: target()[targetKey()] } = source());
                log.join(",")
            "#
        ),
        "source,source-key,source-key-tostring,target,target-key,get,target-key-tostring,set"
    );
}

#[test]
fn private_elements_need_an_extensible_object() {
    let (cx, _) = new_context();
    let realm = cx.initial_realm();

    let cases = [
        "class Base { constructor() { Object.preventExtensions(this); } }
         class Sub extends Base { #val; constructor() { super(); this.#val = 42; } }
         new Sub();",
        "class Base { constructor() { Object.preventExtensions(this); } }
         class Sub extends Base { constructor() { super(); } #m() {} }
         new Sub();",
        "class Base { constructor() { Object.preventExtensions(this); } }
         class Sub extends Base { constructor() { super(); } get #a() { return 1; } }
         new Sub();",
        "class Direct { #g = (Object.preventExtensions(this), 1); }
         new Direct();",
    ];

    for source in cases {
        assert_eq!(
            eval_string(
                cx,
                realm,
                &format!(
                    "var error; try {{ {source} }} catch (e) {{ error = e }} String(error instanceof TypeError)"
                )
            ),
            "true",
            "{source}"
        );
    }

    // Extensible objects are unaffected.
    assert_eq!(
        eval_string(
            cx,
            realm,
            "class Ok { #val = 1; get val() { return this.#val } }; String(new Ok().val)"
        ),
        "1"
    );
}

/// the dynamic reference instructions read their operands before the scope lookup, which can run
/// script that collects. With the from-space poisoned, an operand read after the lookup came from the old
/// bytecode and corrupted the frame, so this crashes without the `gc_stress_test` feature when it regresses.
#[test]
fn dynamic_reference_instructions_survive_a_collection_in_the_lookup() {
    let options = OptionsBuilder::new().gc_stress_window(Some(16 << 10)).gc_verify(true);
    let (cx, _recorder) = new_context_with_options(options);
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    let before = embed::heap_stats(cx).gc_count;
    let source = "function churn() { var k = []; for (var i = 0; i < 500; i++) k.push({ a: i, s: \"s\" + i }); return k.length; }\
                  var o = { x: 4, y: null, get [Symbol.unscopables]() { churn(); return {}; } };\
                  var r = [];\
                  with (o) { x += 1; r.push(x++); r.push(++x); y ??= 3; x **= 2; }\
                  var p = new Proxy({ z: 1 }, { has(t, k) { churn(); return k in t; } });\
                  with (p) { z += 2; z++; }\
                  r.join() + \" \" + o.x + \" \" + o.y + \" \" + p.z";
    assert_eq!(eval_string(cx, realm, source), "5,7 49 3 4");
    assert!(embed::heap_stats(cx).gc_count - before >= 5);
}

/// dynamic-ref-operands, MakeDynamicRef: only a simple assignment to a name resolved at run time emits it, and it read its
/// dest operand after the lookup that can collect.
#[test]
fn simple_assignment_through_with_survives_a_collection_in_the_lookup() {
    let options = OptionsBuilder::new().gc_stress_window(Some(16 << 10)).gc_verify(true);
    let (cx, _recorder) = new_context_with_options(options);
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    let before = embed::heap_stats(cx).gc_count;
    let source = "function churn() { var k = []; for (var i = 0; i < 500; i++) k.push({ a: i, s: \"s\" + i }); return k.length; }\
                  var o = { x: 1, get [Symbol.unscopables]() { churn(); return {}; } };\
                  with (o) { x = 5; }\
                  var t = { x: 1 };\
                  var p = new Proxy(t, { has(t, k) { churn(); return k in t; } });\
                  with (p) { x = 7; }\
                  var gz = 1;\
                  with (o) { gz = 9; undeclaredByVm01 = 3; }\
                  function g() { var q = { x: 1, get [Symbol.unscopables]() { churn(); return {}; } }; with (q) { x = (churn(), 2); } return q.x; }\
                  [o.x, t.x, gz, undeclaredByVm01, g(), g()].join()";
    assert_eq!(eval_string(cx, realm, source), "5,7,9,3,2,2");
    assert!(embed::heap_stats(cx).gc_count - before >= 5);
}

// ---------------------------------------------------------------------------------------------
// an engine task runs after it has left every root set.
//
// `run_all_tasks` pops a task off the queue and `run_job` removes it from the host job map before
// executing it, so while the task runs its raw `Value`/`HeapPtr` fields are in no root set. These
// tests give each kind of task a callback that forces moving collections (allocation under
// `gc_stress_window`, the heap verifier on, and an explicit `gc()`) while the task is running, and
// then check that everything the task reads after the callback (the result capability, the settled
// value, the generator, the realm) is intact. Each runs through both task paths: the engine queue
// without a host (`run_all_tasks`) and the host job path (`run_job`).
// ---------------------------------------------------------------------------------------------

/// Garbage-heavy helpers shared by the re-entrancy scripts. `churn` allocates enough to cross several
/// stress windows and then runs an explicit collection.
const REENT01_PRELUDE: &str = r#"
var log = [];
var sink;
function gc() { $262.gc(); }
function churn(n) {
    n = Math.max(60, Math.floor(n / SCALE));
    var a = [];
    for (var i = 0; i < n; i++) a.push({ i: i, s: "x" + i, a: [i, i + 1] });
    gc();
    sink = a.length;
    for (var i = 0; i < n; i += 97) if (a[i].s !== "x" + i || a[i].a[1] !== i + 1) throw new Error("corrupt churn data");
    return a.length;
}
"#;

fn reent01_options() -> OptionsBuilder {
    OptionsBuilder::new()
        .expose_test_262(true)
        .gc_stress_window(Some(32 << 10))
        .gc_verify(true)
}

/// Run `body` after the prelude and drain all tasks, either through the engine queue or through the
/// host job path, then return the log the script wrote.
fn reent01_run(body: &str, through_host: bool) -> String {
    // With the gc_stress_test feature every allocation collects, so the garbage per churn is cut down
    let scale = if cfg!(feature = "gc_stress_test") {
        30
    } else {
        1
    };
    let source = format!("var SCALE = {scale};\n{REENT01_PRELUDE}\n{body}");

    if through_host {
        let (cx, recorder) = new_context_with_options(reent01_options());
        let log = {
            handle_scope_guard!(cx);
            let realm = cx.initial_realm();
            eval(cx, realm, &source);
            run_host_jobs(cx, &recorder);
            eval_string(cx, realm, "log.join(\",\")")
        };
        cx.drop();
        log
    } else {
        #[allow(unused_mut)]
        let mut cx = ContextBuilder::new()
            .set_options(Rc::new(reent01_options().build().unwrap()))
            .build()
            .unwrap();
        #[cfg(feature = "gc_stress_test")]
        cx.enable_gc_stress_test();
        let log = {
            handle_scope_guard!(cx);
            let realm = cx.initial_realm();
            embed::with_realm(cx, realm, |cx| {
                embed::evaluate_script(cx, realm, &source, "reent01.js", 1).unwrap();
            });
            let mut cx = cx;
            cx.run_all_tasks().unwrap();
            embed::with_realm(cx, realm, |cx| {
                let value =
                    embed::evaluate_script(cx, realm, "log.join(\",\")", "log.js", 1).unwrap();
                to_string(cx, value).unwrap().format().unwrap()
            })
        };
        cx.drop();
        log
    }
}

fn reent01_check(body: &str, expected: &str) {
    for through_host in [false, true] {
        assert_eq!(reent01_run(body, through_host), expected, "through_host = {through_host}");
    }
}

#[test]
fn reent01_promise_fulfill_reaction_survives_collections_in_its_handler() {
    // PromiseThenReactionTask (Fulfill): the handler closure is referenced only by the task, and the
    // capability that receives its result is read after the handler ran.
    reent01_check(
        r#"(function () {
            var tag = { t: "k1" };
            Promise.resolve(7)
                .then(function (v) { churn(3000); log.push("a:" + v + ":" + tag.t); return { w: v + 1 }; })
                .then(function (o) { churn(3000); log.push("b:" + o.w); return o.w * 2; })
                .then(function (v) { churn(3000); log.push("c:" + v); });
        })();"#,
        "a:7:k1,b:8,c:16",
    );
}

#[test]
fn reent01_promise_reject_reaction_survives_collections_in_its_handler() {
    // PromiseThenReactionTask (Reject), also a reaction without a handler (pass-through).
    reent01_check(
        r#"(function () {
            Promise.reject({ why: "r" })
                .then(function () { log.push("no"); })
                .catch(function (e) { churn(3000); log.push("a:" + e.why); throw { again: e.why + "2" }; })
                .then(null, function (e) { churn(3000); log.push("b:" + e.again); return "done"; })
                .then(function (v) { log.push("c:" + v); });
        })();"#,
        "a:r,b:r2,c:done",
    );
}

#[test]
fn reent01_thenable_job_survives_collections_in_the_then_function() {
    // PromiseThenSettleTask: `then_function`, `resolution`, `promise`, `realm` are all raw.
    reent01_check(
        r#"(function () {
            var thenable = { v: 5, then: function (resolve) { churn(3000); log.push("t:" + this.v); resolve(this.v + 1); } };
            Promise.resolve(thenable).then(function (v) { churn(3000); log.push("a:" + v); });
            new Promise(function (resolve) { resolve({ then: function (r) { churn(3000); r("n"); } }); })
                .then(function (v) { log.push("b:" + v); });
        })();"#,
        "t:5,a:6,b:n",
    );
}

#[test]
fn reent01_async_function_await_continuation_survives_collections() {
    // AwaitResumeTask on a GeneratorObject: generator and result are raw until resumed.
    reent01_check(
        r#"(async function () {
            var local = { n: 1 };
            var v = await Promise.resolve(10);
            churn(3000);
            log.push("a:" + v + ":" + local.n);
            var w = await { then: function (r) { churn(3000); r(20); } };
            churn(3000);
            log.push("b:" + w);
            try { await Promise.reject({ e: 3 }); } catch (e) { churn(3000); log.push("c:" + e.e); }
            return 99;
        })().then(function (v) { log.push("d:" + v); });"#,
        "a:10:1,b:20,c:3,d:99",
    );
}

#[test]
fn reent01_async_generator_and_for_await_continuations_survive_collections() {
    // AwaitResumeTask on an AsyncGeneratorObject and on a BuiltinGenerator (async-from-sync iterator).
    reent01_check(
        r#"(async function () {
            async function* g() { var x = await 1; churn(3000); yield x; yield await Promise.resolve(2); churn(3000); return 3; }
            for await (var v of g()) { churn(3000); log.push("g:" + v); }
            for await (var u of [Promise.resolve("s1"), "s2"]) { churn(3000); log.push("s:" + u); }
            log.push("end");
        })();"#,
        "g:1,g:2,s:s1,s:s2,end",
    );
}

#[test]
fn reent01_promise_combinator_element_functions_survive_collections() {
    // Reactions whose handlers are builtin element functions (Promise.all / allSettled / any / race),
    // run while earlier reactions churn the heap.
    reent01_check(
        r#"(function () {
            var ps = [1, 2, 3].map(function (i) { return Promise.resolve(i).then(function (v) { churn(2000); return v * 10; }); });
            Promise.all(ps).then(function (a) { churn(2000); log.push("all:" + a.join("+")); });
            Promise.allSettled([Promise.reject(1), Promise.resolve(2)]).then(function (r) { churn(2000); log.push("settled:" + r.map(function (x) { return x.status; }).join("+")); });
            Promise.any([Promise.reject(1), Promise.resolve("ok")]).then(function (v) { churn(2000); log.push("any:" + v); });
            Promise.race([new Promise(function () {}), Promise.resolve("first")]).then(function (v) { churn(2000); log.push("race:" + v); });
        })();"#,
        "settled:rejected+fulfilled,any:ok,race:first,all:10+20+30",
    );
}

#[test]
fn reent01_finalization_registry_cleanup_task_survives_collections() {
    // Callback1Task: `func` and `arg` are raw; the callback is the registry cleanup callback and the
    // held value is an object only the task refers to.
    reent01_check(
        r#"(function () {
            var registry = new FinalizationRegistry(function (held) { churn(3000); log.push("cleanup:" + held.id); });
            (function () { registry.register({}, { id: 41 }); })();
            gc(); gc();
            globalThis.keepRegistry = registry;
        })();
        Promise.resolve().then(function () { churn(500); log.push("tick"); });"#,
        "cleanup:41,tick",
    );
}

/// link a module whose dependency cannot be loaded, again and again. Every call must fail with the
/// host error; none may abort (linker.rs `loaded_modules[i].unwrap()` on a request that never loaded).
#[test]
fn glue01_link_module_with_unloadable_dependency_twice() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();
    // "./missing.js" has no source: the test host's resolve_module throws, like a 404.
    let importer = embed::compile_module(cx, realm, "import './missing.js';", "imp.js", 1).unwrap();
    assert!(embed::module_link(cx, importer).is_err());
    assert!(embed::module_link(cx, importer).is_err());
    assert!(embed::module_link(cx, importer).is_err());
    run_host_jobs(cx, &recorder);
    cx.drop();
}

/// same, with a dependency whose source fails to parse.
#[test]
fn glue03_link_module_with_dependency_that_fails_to_parse_twice() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();
    add_module_source(&recorder, "./syntax.js", "let let let;");
    let importer = embed::compile_module(cx, realm, "import './syntax.js';", "imp.js", 1).unwrap();
    assert!(embed::module_link(cx, importer).is_err());
    assert!(embed::module_link(cx, importer).is_err());
    run_host_jobs(cx, &recorder);
    cx.drop();
}

/// two import() of the same broken graph started in the same tick.
#[test]
fn glue03_two_dynamic_imports_of_a_graph_with_unloadable_dependency_in_one_tick() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();
    add_module_source(&recorder, "./imp.js", "import './missing.js';");
    eval(
        cx,
        realm,
        "var log = [];
         Promise.allSettled([import('./imp.js'), import('./imp.js')]).then(r => log.push(r.map(x => x.status).join()));",
    );
    run_host_jobs(cx, &recorder);
    assert_eq!(eval_string(cx, realm, "log.join(';')"), "rejected,rejected");

    // and a later one, after the first two settled
    eval(cx, realm, "import('./imp.js').catch(() => log.push('third rejected'));");
    run_host_jobs(cx, &recorder);
    assert_eq!(eval_string(cx, realm, "log.join(';')"), "rejected,rejected;third rejected");
    cx.drop();
}

/// the second load goes through a different importer of the module whose dependency failed.
#[test]
fn glue03_second_load_through_a_different_importer() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();
    add_module_source(&recorder, "./imp.js", "import './missing.js';");
    let first = embed::compile_module(cx, realm, "import './imp.js';", "first.js", 1).unwrap();
    assert!(embed::module_link(cx, first).is_err());
    let second = embed::compile_module(cx, realm, "import './imp.js';", "second.js", 1).unwrap();
    assert!(embed::module_link(cx, second).is_err());
    run_host_jobs(cx, &recorder);
    cx.drop();
}

/// the dependency becomes loadable before the second load: the second load succeeds and the module runs.
#[test]
fn glue03_dependency_that_becomes_loadable_on_the_second_load() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();
    let importer = embed::compile_module(
        cx,
        realm,
        "import { v } from './late.js'; globalThis.ran = v;",
        "imp.js",
        1,
    )
    .unwrap();
    assert!(embed::module_link(cx, importer).is_err());

    add_module_source(&recorder, "./late.js", "export const v = 7;");
    embed::module_link(cx, importer).unwrap();
    let promise = embed::module_evaluate(cx, importer).unwrap();
    run_host_jobs(cx, &recorder);
    assert_eq!(embed::api::promise_state(promise), embed::api::PromiseStatus::Fulfilled);
    assert_eq!(eval_string(cx, realm, "String(globalThis.ran)"), "7");
    cx.drop();
}

/// a graph with one good and one bad dependency, deeper than one level: the retry reaches the missing request
/// below an already loaded module, then fails again while the dependency is still missing.
#[test]
fn glue03_retry_reaches_missing_request_below_loaded_modules() {
    let (cx, recorder) = new_context();
    let realm = cx.initial_realm();
    add_module_source(&recorder, "./good.js", "export const g = 1;");
    add_module_source(&recorder, "./mid.js", "import './good.js'; import './deep.js'; export const m = 2;");
    let root = embed::compile_module(cx, realm, "import './mid.js'; import './good.js';", "root.js", 1).unwrap();
    assert!(embed::module_link(cx, root).is_err());
    assert!(embed::module_link(cx, root).is_err());

    add_module_source(&recorder, "./deep.js", "export const d = 3;");
    embed::module_link(cx, root).unwrap();
    let promise = embed::module_evaluate(cx, root).unwrap();
    run_host_jobs(cx, &recorder);
    assert_eq!(embed::api::promise_state(promise), embed::api::PromiseStatus::Fulfilled);
    cx.drop();
}

// =============================================================================================
// an earlier change containment: an allocation on the JS heap made from a garbage collector
// hook aborts the process, and the message names the hook that made it.
//
// `docs/engine-invariants.md` row 6d states the rule ("GC hooks (`trace_roots`, `trace_host_object`,
// `after_gc_weak`, `finalize_host_object`) must not allocate on the JS heap, create handles or run
// JS"); `docs/embedding.md` states it to the embedder. Before this containment the rule was prose
// plus an after-the-fact report (`gc/verify.rs`, only with `Options::gc_verify`), so an item
// allocated from a sweep hook was handed back as a `HeapPtr` into the from-space or into the
// old heap buffer of a resizing collection, which the collection then frees (a use-after-free).
// The interleavings behind this containment are described in the engine notes.
//
// Every scenario runs in a CHILD PROCESS: the containment is `std::process::abort`, and on the
// unfixed engine the same scenario ends in a `debug_assert`, in the verifier's abort or in a
// SIGSEGV. A child contains all of those, and the parent asserts on the exit status and on stderr.
// What the unfixed engine does with these seven scenarios (debug build, `annex_b,alloc_error`,
// recorded in the engine notes):
//   * `after_gc_weak`, `finalize_host_object`, `trace_host_object`, `trace_roots` and the test
//     host's own finalizer allocation: the child EXITS 0 and the marker reads back intact - a silent
//     survivor, which is the hook-allocation-lost hazard the verifier exists to catch.
//   * `after_gc_weak` with `gc_verify`: the child is aborted by `verify_heap` with "N allocations
//     during the collection (into the from-space)", which names neither the hook nor the rule.
//   * `after_gc_weak` during a resizing collection: `alloc_string_ptr` fails with `EvalError::Alloc`
//     (debug + `alloc_error`); in release the same item is a use-after-free of the old heap buffer
//     and the read-back SIGSEGVs (row gc02 of the reproduction results).
// The control scenario, in which no hook allocates, exits 0 both before and after the containment.
//
// Teardown is the one interval that is not a collection: `embed::teardown` sets `gc_in_progress` to
// stop a nested GC and runs the host finalizers, but it never closes the allocation window, so a
// finalizer that allocates there is served as long as the item fits the semispace (see
// `gc_alloc_in_teardown_finalizer_is_served`).
// =============================================================================================

/// Content of the string the containment host allocates from a hook, stored in a live host object's
/// reserved slot so that a surviving item can be told from a dead one.
const GC_CONTAINMENT_MARKER: &str = "GC-CONTAINMENT-SENTINEL-0123456789";

/// Environment variable that makes a copy of this test binary run one containment scenario in
/// process, instead of only spawning the child.
const GC_CHILD_ENV: &str = "BS_GC_ALLOC_CONTAINMENT_CHILD";

/// Which `Host` hook the containment host allocates from. `NoHook` is the control: a host that
/// obeys row 6d, which the containment must leave alone.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum ContainmentHook {
    #[default]
    NoHook,
    TraceRoots,
    TraceHostObject,
    AfterGcWeak,
    FinalizeHostObject,
}

impl ContainmentHook {
    /// The name the abort message is expected to print, which is also the scenario's `mode` string.
    fn name(self) -> &'static str {
        match self {
            ContainmentHook::NoHook => "no_hook",
            ContainmentHook::TraceRoots => "trace_roots",
            ContainmentHook::TraceHostObject => "trace_host_object",
            ContainmentHook::AfterGcWeak => "after_gc_weak",
            ContainmentHook::FinalizeHostObject => "finalize_host_object",
        }
    }
}

/// What one containment scenario does: which hook allocates, whether the collection is a resizing
/// one, and whether the heap verifier is on.
#[derive(Clone, Copy)]
struct ContainmentScenario {
    hook: ContainmentHook,
    /// Drive the collection with `GcType::Grow`, so the illegal item would land in the heap buffer
    /// the collection frees rather than in a semispace that stays mapped.
    resizing: bool,
    /// `Options::gc_verify`, off by default: the containment must not need the verifier.
    verify: bool,
    /// Run the existing finalizer scenario (`check_gc_from_finalizer` with `alloc_in_finalizer`)
    /// rather than the containment host: the hook is reached through the test host's recorder.
    via_test_host: bool,
}

/// Shared between a containment scenario and the host it installs.
#[derive(Default)]
struct ContainmentState {
    /// Which hook allocates.
    hook: Cell<ContainmentHook>,
    /// Only the armed collection allocates; the warm-up collections must stay clean.
    armed: Cell<bool>,
    /// Set once the hook has allocated, so it does so exactly once.
    sabotaged: Cell<bool>,
    /// Allocate only from the host call that follows the weak pass, which with `gc_verify` on is the
    /// verification walk *after* the collection: `after_gc_weak` comes between it and the two trace
    /// calls before it (the walk before the collection and the root trace).
    after_weak_only: Cell<bool>,
    /// Set by `after_gc_weak` while armed, consumed by the next hook that may allocate.
    weak_done: Cell<bool>,
    /// A live host object the marker's `Value` is stored in, kept alive by `sink_root`. Host
    /// objects never move, so the slot survives the collection while the item in it may not.
    sink: Cell<Option<HeapPtr<ObjectValue>>>,
    /// The sink, reported to the collector by `trace_roots`.
    sink_root: RefCell<Vec<*mut c_void>>,
    /// The context, stored at install time as the glue stores its own. The hooks receive a
    /// `GcHookCx`, which has no way to the allocator, so this host breaks the rule only by
    /// reaching around the type: the run-time abort is what stops it (docs/gc-hooks-cannot-allocate.md).
    cx: Cell<Option<Context>>,
}

impl ContainmentState {
    /// On the first armed call from the watched hook, allocate the marker string on the JS heap and
    /// store it in the sink's reserved slot 0. This is exactly hook-allocation-lost and the item lands in
    /// the from-space or in the old heap buffer, and the slot it is written to was visited already,
    /// so nothing forwards it. With `after_weak_only` the call is taken only after the weak pass,
    /// which puts it in the verification walk that follows the collection.
    fn maybe_allocate(&self, from: ContainmentHook) {
        if !self.armed.get() || self.sabotaged.get() || self.hook.get() != from {
            return;
        }
        if self.after_weak_only.get() {
            // Wait for the weak pass: everything the collector calls before it is a call of the
            // collection proper, and this scenario is about the walk that comes after it.
            if !self.weak_done.get() {
                return;
            }
            self.weak_done.set(false);
        }
        let Some(sink) = self.sink.get() else { return };
        self.sabotaged.set(true);

        let mut cx = self.cx.get().expect("context stored at install");
        let string = cx.alloc_string_ptr(GC_CONTAINMENT_MARKER).unwrap();
        embed::set_reserved_slot(sink, 0, Value::from(string.as_string()));
    }
}

struct ContainmentHost(Rc<ContainmentState>);

impl Host for ContainmentHost {
    fn trace_roots(&self, _: GcHookCx<'_>, visitor: &mut dyn RootVisitor) {
        for slot in self.0.sink_root.borrow_mut().iter_mut() {
            visitor.cell(slot);
        }
        self.0.maybe_allocate(ContainmentHook::TraceRoots);
    }

    fn trace_host_object(&self, _: GcHookCx<'_>, _: HostObjectRef, _: &mut dyn RootVisitor) {
        self.0.maybe_allocate(ContainmentHook::TraceHostObject);
    }

    fn after_gc_weak(&self, _: GcHookCx<'_>, _: &mut dyn WeakUpdater) {
        if self.0.armed.get() {
            self.0.weak_done.set(true);
        }
        self.0.maybe_allocate(ContainmentHook::AfterGcWeak);
    }

    fn finalize_host_object(&self, _: GcHookCx<'_>, _: HostObjectRef) {
        self.0.maybe_allocate(ContainmentHook::FinalizeHostObject);
    }
}

fn new_containment_context(options: OptionsBuilder) -> (Context, Rc<ContainmentState>) {
    let state = Rc::new(ContainmentState::default());
    let options = options.build().unwrap();

    #[allow(unused_mut)]
    let mut cx = ContextBuilder::new()
        .set_options(Rc::new(options))
        .build()
        .unwrap();
    embed::install_host(cx, Box::new(ContainmentHost(state.clone()))).unwrap();
    state.cx.set(Some(cx));

    #[cfg(feature = "gc_stress_test")]
    cx.enable_gc_stress_test();

    (cx, state)
}

/// A live host object with one reserved slot, rooted through the state so it survives the
/// collection. `HAS_TRACE` is needed for `trace_host_object` to be called for it.
fn new_containment_sink(
    cx: Context,
    state: &Rc<ContainmentState>,
    flags: HostFlags,
) -> HeapPtr<ObjectValue> {
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    let proto = realm.get_intrinsic(Intrinsic::ObjectPrototype);
    let sink = embed::new_host_object(
        cx,
        realm,
        TEST_CLASS,
        flags,
        Some(proto),
        1,
        std::ptr::null(),
        cx.undefined(),
    )
    .unwrap();
    let sink = *sink;
    state.sink.set(Some(sink));
    state.sink_root.borrow_mut().push(sink.as_ptr().cast());
    sink
}

/// A host object with a finalizer and no remaining root: the next collection frees it and calls
/// `finalize_host_object`, which is the hook the scenario watches.
fn new_dying_containment_object(cx: Context) {
    handle_scope_guard!(cx);
    let realm = cx.initial_realm();
    let proto = realm.get_intrinsic(Intrinsic::ObjectPrototype);
    let _dying = embed::new_host_object(
        cx,
        realm,
        TEST_CLASS,
        HostFlags::HAS_FINALIZE,
        Some(proto),
        0,
        std::ptr::null(),
        cx.undefined(),
    )
    .unwrap();
}

/// Read the sink's reserved slot 0 back as a string: the marker if the item survived, anything else
/// if it died with the collection.
fn read_containment_marker(cx: Context, sink: HeapPtr<ObjectValue>) -> String {
    let slot = embed::reserved_slot(sink, 0);
    handle_scope_guard!(cx);
    slot.as_string().to_handle().format().unwrap()
}

/// The containment scenario, run either in the child (`child_gc_alloc_during_collection`) or, when
/// a test does not go through a child, never.
fn run_containment_scenario(scenario: &ContainmentScenario) {
    if scenario.via_test_host {
        // The engine's own finalizer scenario: `finalize_host_object` of the test host allocates 64
        // bytes on the JS heap. That is the `finalize_host_object` hook, reached through the
        // recorder instead of a dedicated host.
        check_gc_from_finalizer(|recorder| recorder.alloc_in_finalizer.set(true));
        return;
    }

    // A heap built at run time (`serialized_heap(None)`), so the heap size is exactly what the
    // scenario asks for.
    let mut options = OptionsBuilder::new()
        .serialized_heap(None)
        .gc_verify(scenario.verify);
    if scenario.resizing {
        options = options.min_heap_size(2 << 20).max_heap_size(128 << 20);
    }
    let (cx, state) = new_containment_context(options);
    let flags = if scenario.hook == ContainmentHook::TraceHostObject {
        HostFlags::HAS_TRACE
    } else {
        HostFlags::empty()
    };
    let sink = new_containment_sink(cx, &state, flags);

    // Clean collections, so the sink is stably reachable and the armed one is the only suspect.
    embed::gc(cx);
    embed::gc(cx);

    if scenario.hook == ContainmentHook::FinalizeHostObject {
        new_dying_containment_object(cx);
    }

    state.hook.set(scenario.hook);
    if scenario.resizing {
        // One allocation of a whole semispace, which cannot fit the window. The checks below are
        // `sizing::grow_before_collect` (gc/sizing.rs:433-446) on these numbers: heap below the
        // maximum, a collection counted, and room needed (`needs_room` at gc/sizing.rs:416-421 is
        // `grown(last_live) + trigger > semispace`, true on its second term alone because the trigger
        // is a whole semispace and `grown(last_live)` is positive). So the allocator answers the
        // trigger with a Grow, and its sweep calls the hook while the OLD heap buffer is still the
        // heap. There is deliberately no JS warm-up: with `feature = "gc_stress_test"` every
        // allocation runs a collection (gc/heap.rs:387-389), so a loop of reachable objects would be
        // a collection per object here, and the trigger is what makes this a resizing case.
        let before = embed::heap_stats(cx);
        let trigger = before.semispace_capacity_bytes;
        assert!(before.last_live_bytes > 0, "no live data after the warm collection");
        assert!(
            before.heap_size_bytes < before.max_heap_size_bytes && before.gc_count > 0,
            "the trigger would not be answered with a Grow: {before:?}"
        );
        eprintln!(
            "gc02 armed for a Grow: heap_size={} max_heap={} semispace={} last_live={} gc_count={} grow_count={}",
            before.heap_size_bytes,
            before.max_heap_size_bytes,
            before.semispace_capacity_bytes,
            before.last_live_bytes,
            before.gc_count,
            before.gc_grow_count,
        );

        // Armed only around the trigger: the warm-up collections above run with the hook silent, so
        // the armed collection can only be the one this allocation starts. That is what makes this
        // the resizing case rather than "some collection aborted".
        state.armed.set(true);
        let _ = Heap::alloc_uninit_with_size::<u64>(cx, trigger).unwrap();
        state.armed.set(false);

        // Only an engine that lets a hook allocation through reaches this: with the containment on,
        // the abort happens inside the call above. These two say what the trigger did, and the
        // child's stderr shows where the containment stopped it.
        let after = embed::heap_stats(cx);
        assert!(
            after.gc_grow_count > before.gc_grow_count,
            "the collection did not resize the heap"
        );
        assert!(
            after.heap_size_bytes > before.heap_size_bytes,
            "the heap size did not grow, so the collection was not a resizing one: {before:?} -> {after:?}"
        );
    } else {
        state.armed.set(true);
        embed::gc(cx);
    }
    state.armed.set(false);

    if scenario.hook == ContainmentHook::NoHook {
        // The control: the collections ran, nothing was aborted, and JS still works.
        assert!(!state.sabotaged.get(), "the control host allocated");
        assert!(embed::heap_stats(cx).gc_count >= 3, "the control ran no collection");
        {
            handle_scope_guard!(cx);
            let realm = cx.initial_realm();
            assert_eq!(eval(cx, realm, "1+1").as_number(), 2.0);
        }
        cx.drop();
        return;
    }

    // Reaching this line means no abort happened: on the unfixed engine this is the silent case.
    assert!(state.sabotaged.get(), "the {} hook never allocated", scenario.hook.name());
    assert_eq!(
        read_containment_marker(cx, sink),
        GC_CONTAINMENT_MARKER,
        "an item allocated by the {} hook during a collection survived it, so the collection did not reject it",
        scenario.hook.name(),
    );
    cx.drop();
}

/// Teardown is not a collection: `embed::teardown` sets `gc_in_progress` so that no GC nests, and runs
/// the host finalizers, but leaves the allocation window as it found it. A finalizer that allocates
/// there is therefore served, as long as the item fits the semispace, and dies with the context. This
/// scenario reaches the allocator's slow path on purpose (`request_collection`, the way a nearly full
/// semispace does) so that it tests `Heap::collect_for_alloc`, which is where the containment lives:
/// the abort keys off the collection, not off `gc_in_progress` alone. Nothing is aborted here, so the
/// child runs to completion and exits 0.
fn run_teardown_alloc_scenario() {
    let (mut cx, state) = new_containment_context(OptionsBuilder::new().serialized_heap(None));
    let _sink = new_containment_sink(cx, &state, HostFlags::empty());
    new_dying_containment_object(cx);
    state.hook.set(ContainmentHook::FinalizeHostObject);
    state.armed.set(true);

    cx.heap.request_collection();

    // `embed::teardown` finalizes the dying object, and its `finalize_host_object` allocates.
    cx.drop();

    assert!(state.sabotaged.get(), "the teardown finalizer never allocated");
    println!("TEARDOWN-ALLOC-SERVED");
}

/// The verification walk that runs after the collection (`verify_heap(cx, "after")`, hunt builds with
/// `Options::gc_verify`) calls the host trace hooks as well, and it is still part of the collection:
/// the allocation window stays closed until `reset_alloc_limit`, which comes after it. So an
/// allocation from one of its hooks is refused like one from the collection itself, instead of served
/// because the copy is already over. `after_weak_only` picks that call out of the three the collection
/// makes, by the weak pass that comes between it and the trace calls before it.
fn run_after_verify_trace_scenario() {
    let (cx, state) =
        new_containment_context(OptionsBuilder::new().serialized_heap(None).gc_verify(true));
    let _sink = new_containment_sink(cx, &state, HostFlags::empty());

    // Clean collections, so the armed one is the only suspect.
    embed::gc(cx);
    embed::gc(cx);

    state.hook.set(ContainmentHook::TraceRoots);
    state.after_weak_only.set(true);
    state.armed.set(true);
    embed::gc(cx);
    state.armed.set(false);

    // Reached only by an engine that serves the after-verification allocation: the window was already
    // reopened, the item fits the space the collection moved into, and so the child exits 0.
    assert!(state.sabotaged.get(), "the trace hook of the after-verification walk never ran");
    cx.drop();
}

/// The scenarios themselves. Without `GC_CHILD_ENV` this test does nothing: it only runs as a child.
#[test]
fn child_gc_alloc_during_collection() {
    let Ok(mode) = std::env::var(GC_CHILD_ENV) else {
        return;
    };
    let (hook, verify) = match mode.as_str() {
        "no_hook" => (ContainmentHook::NoHook, false),
        "trace_roots" => (ContainmentHook::TraceRoots, false),
        "trace_host_object" => (ContainmentHook::TraceHostObject, false),
        "after_gc_weak" => (ContainmentHook::AfterGcWeak, false),
        "finalize_host_object" => (ContainmentHook::FinalizeHostObject, false),
        "after_gc_weak_verify" => (ContainmentHook::AfterGcWeak, true),
        "after_gc_weak_resize" => {
            run_containment_scenario(&ContainmentScenario {
                hook: ContainmentHook::AfterGcWeak,
                resizing: true,
                verify: false,
                via_test_host: false,
            });
            return;
        }
        "allocation_in_finalizer" => {
            run_containment_scenario(&ContainmentScenario {
                hook: ContainmentHook::FinalizeHostObject,
                resizing: false,
                verify: false,
                via_test_host: true,
            });
            return;
        }
        "teardown_alloc" => {
            run_teardown_alloc_scenario();
            return;
        }
        "trace_roots_after_weak_verify" => {
            run_after_verify_trace_scenario();
            return;
        }
        other => panic!("unknown containment scenario {other}"),
    };
    run_containment_scenario(&ContainmentScenario {
        hook,
        resizing: false,
        verify,
        via_test_host: false,
    });
}

/// Whether the child was killed by `std::process::abort` (SIGABRT on unix).
fn child_aborted(status: &std::process::ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        status.signal() == Some(6)
    }
    #[cfg(not(unix))]
    {
        !status.success()
    }
}

/// Run one containment scenario in a child copy of this test binary and assert that it died in the
/// named abort: status SIGABRT, and a stderr that says both `during a garbage collection` and which
/// hook did it.
fn assert_child_aborts_naming(scenario: &ContainmentScenario) {
    let mode = if scenario.via_test_host {
        "allocation_in_finalizer".to_string()
    } else if scenario.resizing {
        "after_gc_weak_resize".to_string()
    } else if scenario.verify {
        format!("{}_verify", scenario.hook.name())
    } else {
        scenario.hook.name().to_string()
    };
    let exe = std::env::current_exe().unwrap();
    let child = std::process::Command::new(exe)
        .args([
            "--exact",
            "runtime::embed::tests::child_gc_alloc_during_collection",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(GC_CHILD_ENV, &mode)
        .output()
        .expect("failed to spawn the child containment process");
    let stdout = String::from_utf8_lossy(&child.stdout);
    let stderr = String::from_utf8_lossy(&child.stderr);
    let hook = scenario.hook.name();
    assert!(
        child_aborted(&child.status),
        "the child was not aborted by the containment: status={:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        child.status,
    );
    assert!(
        stderr.contains("during a garbage collection"),
        "the child did not abort with the containment message: status={:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        child.status,
    );
    assert!(
        stderr.contains(hook),
        "the abort message does not name the hook {hook}: status={:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        child.status,
    );
}

/// hook-allocation-lost, `after_gc_weak`: without the containment the item lands in the from-space, the collection
/// releases those pages and the read-back is silent corruption (child exit 0 on the unfixed engine).
#[test]
fn gc_alloc_in_after_gc_weak_aborts_naming_the_hook() {
    assert_child_aborts_naming(&ContainmentScenario {
        hook: ContainmentHook::AfterGcWeak,
        resizing: false,
        verify: false,
        via_test_host: false,
    });
}

/// hook-allocation-lost, `finalize_host_object`, reached through the test host's own finalizer allocation: the
/// scenario the engine used to document as allowed ("allocation in a finalizer does not collect").
#[test]
fn allocation_in_finalizer_aborts_naming_the_hook() {
    assert_child_aborts_naming(&ContainmentScenario {
        hook: ContainmentHook::FinalizeHostObject,
        resizing: false,
        verify: false,
        via_test_host: true,
    });
}

/// hook-allocation-lost, `finalize_host_object` through the containment host.
#[test]
fn gc_alloc_in_finalize_host_object_aborts_naming_the_hook() {
    assert_child_aborts_naming(&ContainmentScenario {
        hook: ContainmentHook::FinalizeHostObject,
        resizing: false,
        verify: false,
        via_test_host: false,
    });
}

/// `trace_host_object` runs during the live copy, where the unfixed engine happened to place the
/// item in the destination space and let it survive (repro results, row `gc01_trace_host_object`):
/// still a hook, still forbidden by row 6d, so it must abort as loudly as the sweep hooks.
#[test]
fn gc_alloc_in_trace_host_object_aborts_naming_the_hook() {
    assert_child_aborts_naming(&ContainmentScenario {
        hook: ContainmentHook::TraceHostObject,
        resizing: false,
        verify: false,
        via_test_host: false,
    });
}

/// `trace_roots`, the root-tracing hook of the same trait.
#[test]
fn gc_alloc_in_trace_roots_aborts_naming_the_hook() {
    assert_child_aborts_naming(&ContainmentScenario {
        hook: ContainmentHook::TraceRoots,
        resizing: false,
        verify: false,
        via_test_host: false,
    });
}

/// the same hook during a resizing collection, without the verifier. On the unfixed engine
/// the item is in the freed heap buffer, and the read-back is a use-after-free (SIGSEGV in release,
/// `heap.rs`'s debug path in a debug build). This is the case the containment has to catch with no
/// verifier compiled in.
#[test]
fn gc_alloc_in_a_resizing_collection_aborts_naming_the_hook() {
    assert_child_aborts_naming(&ContainmentScenario {
        hook: ContainmentHook::AfterGcWeak,
        resizing: true,
        verify: false,
        via_test_host: false,
    });
}

/// With `Options::gc_verify` the collector also walks the heap and counts during-collection
/// allocations; the containment must be the one that speaks first and name the hook.
#[test]
fn gc_alloc_during_gc_aborts_with_the_verifier_on_too() {
    assert_child_aborts_naming(&ContainmentScenario {
        hook: ContainmentHook::AfterGcWeak,
        resizing: false,
        verify: true,
        via_test_host: false,
    });
}

/// Review item 3: the verification walk that runs *after* the collection calls the host trace hooks
/// too, and it is inside the closed window, so an allocation from it is refused and names the hook
/// like any other. Before, the window was reopened first and the walk ran with it open: an item that
/// fit the post-collection window was served, and the walk that is supposed to catch a bad allocation
/// was the one that made it. The child is the `trace_roots_after_weak_verify` scenario.
#[test]
fn gc_alloc_in_the_after_verify_walk_aborts_naming_the_hook() {
    let exe = std::env::current_exe().unwrap();
    let child = std::process::Command::new(exe)
        .args([
            "--exact",
            "runtime::embed::tests::child_gc_alloc_during_collection",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(GC_CHILD_ENV, "trace_roots_after_weak_verify")
        .output()
        .expect("failed to spawn the child after-verification process");
    let stdout = String::from_utf8_lossy(&child.stdout);
    let stderr = String::from_utf8_lossy(&child.stderr);
    assert!(
        child_aborted(&child.status),
        "the after-verification allocation was not aborted: status={:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        child.status,
    );
    assert!(
        stderr.contains("during a garbage collection"),
        "the child did not abort with the containment message: status={:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        child.status,
    );
    assert!(
        stderr.contains("trace_roots"),
        "the abort message does not name the hook trace_roots: status={:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        child.status,
    );
}

/// The control: a host that traces, weakly updates and finalizes without allocating on the JS heap
/// must run to completion. Guards against a containment that fires on a legitimate path.
#[test]
fn gc_hook_that_does_not_allocate_is_not_aborted() {
    let exe = std::env::current_exe().unwrap();
    let child = std::process::Command::new(exe)
        .args([
            "--exact",
            "runtime::embed::tests::child_gc_alloc_during_collection",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(GC_CHILD_ENV, "no_hook")
        .output()
        .expect("failed to spawn the child control process");
    let stdout = String::from_utf8_lossy(&child.stdout);
    let stderr = String::from_utf8_lossy(&child.stderr);
    assert!(
        child.status.success(),
        "a host hook that does not allocate did not exit cleanly: status={:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        child.status,
    );
}

/// The other case that must not abort: the same `finalize_host_object` hook, but at context drop.
/// Teardown sets `gc_in_progress` without collecting, so the containment, which is armed for the
/// duration of a collection, must leave the allocation there alone: the child is served the item (and
/// prints `TEARDOWN-ALLOC-SERVED`) and exits 0, as it did before an earlier change.
#[test]
fn gc_alloc_in_teardown_finalizer_is_served() {
    let exe = std::env::current_exe().unwrap();
    let child = std::process::Command::new(exe)
        .args([
            "--exact",
            "runtime::embed::tests::child_gc_alloc_during_collection",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(GC_CHILD_ENV, "teardown_alloc")
        .output()
        .expect("failed to spawn the child teardown process");
    let stdout = String::from_utf8_lossy(&child.stdout);
    let stderr = String::from_utf8_lossy(&child.stderr);
    assert!(
        child.status.success(),
        "the containment aborted an allocation at teardown, where no collection runs: status={:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        child.status,
    );
    assert!(
        stdout.contains("TEARDOWN-ALLOC-SERVED"),
        "the teardown finalizer did not report an allocation served: status={:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        child.status,
    );
}
