//! Embedding API: lets a host (e.g. a browser DOM) install hooks into the runtime.
//!
//! See `docs/embedding.md` for the design. In short:
//! - A [`Host`] is installed once per [`Context`] with [`install_host`].
//! - Host objects are allocated in a non-moving host space (see `host_space.rs`). They are marked
//!   and swept by the otherwise copying garbage collector, so their address never changes.
//! - The host reports its roots, the fields of its host objects, weak pointers and finalization
//!   through the [`Host`] trait during every GC.
//! - Natives, proxies, resolve hooks, promise jobs and realm entry are routed through the host.
//!
//! When no host is installed none of this code runs and the runtime behaves exactly as upstream.

pub mod api;
#[cfg(test)]
mod cell_tests;
mod compiled;
mod host_object;
mod host_space;
mod jobs;
mod modules;
mod natives;
#[cfg(test)]
mod tests;

use std::{
    collections::HashMap,
    ffi::c_void,
    marker::PhantomData,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub use compiled::{
    CompileTimes, CompiledScript, OffThreadSyntaxError, ParsedScript, compile_parsed_script,
    compile_script, execute_compiled, last_compile_times, parse_script_off_thread,
};
pub use host_object::HostObject;
pub use host_space::{HostFlags, HostHeader, HostHeaderPtr};
pub(crate) use host_space::{HostRegionBases, HostSpace};
pub(crate) use jobs::hand_tasks_to_host;
pub use modules::{
    DynamicImport, ImportAttributes, ModuleRequestInfo, ModuleState, SourceTextModule,
    compile_module, finish_dynamic_import, module_evaluate, module_evaluation_error,
    module_file_name, module_link, module_namespace, module_private, module_realm, module_requests,
    module_state, requested_module, set_module_private, set_requested_module,
};
pub(crate) use natives::EmbedNatives;
pub use natives::{
    NativeCall, function_reserved, function_reserved_opt, native_jit_info, native_of,
    new_native_function, new_native_function_with_jit_info, set_function_reserved,
    set_native_jit_info,
};

pub use crate::runtime::bytecode::lazy::{LazyGenerateTime, lazy_generate_time};
pub use crate::runtime::promise_object::{PromiseObject, PromiseRejectionOperation};
use crate::runtime::{
    Context, EvalResult, Handle, HeapItemKind, HeapPtr, PropertyKey, Realm, SymbolValue, Value,
    abstract_operations::{call_object, construct as construct_object},
    alloc_error::AllocResult,
    bytecode::function::ClosureObject,
    error::type_error,
    eval_result::EvalError,
    function::set_function_name,
    gc::{AnyHeapItem, GarbageCollector, GcType, Heap, HeapInfo, HeapVisitor},
    intrinsics::intrinsics::Intrinsic,
    object_value::ObjectValue,
    ordinary_object::{
        ordinary_define_own_property, ordinary_delete, ordinary_get, ordinary_get_own_property,
        ordinary_has_property, ordinary_own_property_keys, ordinary_set,
    },
    property_descriptor::PropertyDescriptor,
    shape::Shape,
    shape_registry::ShapeRegistry,
    string_value::{FlatString, StringValue},
    tasks::Task,
};

/// Visitor for strong roots and traced fields. Every slot may be updated by a moving GC.
pub trait RootVisitor {
    /// A slot holding a value in the engine's own NaN-boxed encoding (`Value::as_raw_bits`). A slot
    /// holding 0 is treated as uninitialized and skipped.
    fn value(&mut self, bits: &mut u64);

    /// A slot holding a raw pointer to a heap item (object, string, symbol, BigInt, ...). Null is
    /// skipped.
    fn cell(&mut self, ptr: &mut *mut c_void);
}

/// Updater for weak slots, used after liveness of the heap is final.
pub trait WeakUpdater {
    /// Set the slot to null if the pointee is dead, otherwise to its (possibly new) address.
    fn update(&mut self, ptr: &mut *mut c_void);

    /// Same as `update` for a slot in the NaN-boxed value encoding. Dead pointers become undefined.
    /// Non-pointer values and 0 are left unchanged.
    fn update_value(&mut self, bits: &mut u64);
}

/// What a hook the garbage collector calls receives in place of a [`Context`]: `Host::trace_roots`,
/// `Host::trace_host_object`, `Host::after_gc_weak`, `Host::finalize_host_object` and the engine-cell
/// hooks ([`EngineCellHooks`]). See `docs/gc-hooks-cannot-allocate.md`.
///
/// These hooks run while the collector is moving the heap (or while the context is being dropped),
/// so they must not allocate on the JS heap, create handles, run JS or ask for a GC. Every one of
/// those needs a `Context`, and this type is not one: it has no `Deref` to the context, no conversion
/// to it and no public method, so a hook written against it cannot reach the allocator through its
/// arguments. What a hook may do is in its other arguments: the [`RootVisitor`] and [`WeakUpdater`]
/// slots, and the header and reserved slots of a [`HostObjectRef`].
///
/// The lifetime is the hook call: the value cannot be kept after the hook returns.
///
/// A host that obtained a `Context` some other way (stored at install time, `context_of`) is not
/// stopped by the type; an allocation through it during a collection aborts the process naming the
/// hook (`Heap::abort_alloc_during_gc`).
#[derive(Clone, Copy)]
pub struct GcHookCx<'a> {
    cx: Context,
    _call: PhantomData<&'a ()>,
}

impl<'a> GcHookCx<'a> {
    /// Made only by the engine, for one hook call.
    fn new(cx: Context) -> Self {
        GcHookCx { cx, _call: PhantomData }
    }

    /// The free callback registered for `cell` by `register_external_free`, removed so that it runs
    /// at most once. For the finalizer of `EXTERNAL_FREE_CELL_HOOKS` only. Allocates nothing.
    fn take_external_free(self, cell: HeapPtr<ObjectValue>) -> Option<Box<dyn FnOnce()>> {
        state(self.cx)
            .external_frees
            .remove(&(cell.as_ptr() as usize))
    }
}

/// A reference to a host object, valid for as long as the object is alive. Host objects never
/// move, so this is safe to hold across a GC (but not past the object's finalizer).
///
/// Invariant: the pointer is the object pointer of a host space allocation. Like a `HeapPtr`, it
/// must not be used after the object died.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HostObjectRef(NonNull<c_void>);

impl HostObjectRef {
    /// Panics if the object does not live in the host space.
    pub fn from_object(object: HeapPtr<ObjectValue>) -> Self {
        assert!(is_in_host_space(object.as_ptr().cast()), "not a host space object");
        HostObjectRef(NonNull::new(object.as_ptr().cast()).unwrap())
    }

    /// # Safety
    /// `object` must be the object pointer of a host space allocation.
    pub(crate) unsafe fn from_host_space_object(object: HeapPtr<AnyHeapItem>) -> Self {
        HostObjectRef(NonNull::new(object.as_ptr().cast()).unwrap())
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.0.as_ptr()
    }

    pub fn object(&self) -> HeapPtr<ObjectValue> {
        HeapPtr::from_ptr(self.0.as_ptr().cast())
    }

    pub fn header(&self) -> HostHeaderPtr {
        // SAFETY: The pointer is the object of a host space allocation (type invariant). The
        // allocation is not freed while the object is alive, and like a `HeapPtr` a `HostObjectRef`
        // is only used while its object is alive.
        unsafe { HostHeaderPtr::from_object_ptr(self.0.as_ptr()) }
    }

    pub fn class(&self) -> *const c_void {
        self.header().class()
    }

    pub fn flags(&self) -> HostFlags {
        self.header().flags()
    }

    /// Read a reserved slot. Panics if the index is out of range.
    pub fn reserved_slot(&self, index: u32) -> Value {
        self.header().reserved_slot(index)
    }

    /// Write a reserved slot. Panics if the index is out of range.
    pub fn set_reserved_slot(&self, index: u32, value: Value) {
        self.header().set_reserved_slot(index, value);
    }

    /// Mutable access to the reserved slots, e.g. to report them from a trace hook.
    ///
    /// # Safety
    /// For all of `'a` the slice must be the only access to this object's reserved slots: no other
    /// slice from this method may be used, and nothing may read or write the slots in another way
    /// (`reserved_slot`, `set_reserved_slot`, `embed::set_reserved_slot`, ...). The object must stay
    /// alive during `'a`.
    pub unsafe fn reserved_slots_mut<'a>(&self) -> &'a mut [Value] {
        // SAFETY: The caller's guarantees are those of `HostHeaderPtr::reserved_slots_mut`.
        unsafe { self.header().reserved_slots_mut() }
    }
}

/// The host environment. Installed once per Context, all methods are called on the Context's
/// thread. Methods take `&self` since the host is called re-entrantly (a native may run JS that
/// calls another native), so hosts keep their mutable state in `Cell`/`RefCell`.
///
/// Methods called during a GC (`trace_roots`, `trace_host_object`, `after_gc_weak`,
/// `finalize_host_object`) must not allocate on the JS heap, create handles or run JS. They receive
/// a [`GcHookCx`] instead of a `Context`, so they cannot reach the allocator through their arguments.
///
/// **No ArrayBuffer bytes in callbacks that run without JS on the stack** (WebAssembly design
/// §3.2): `trace_roots`, `trace_host_object`, `after_gc_weak`, `finalize_host_object`,
/// `promise_rejection` and `enqueue_job`, like the engine-cell hooks (`EngineCellHooks`) and the
/// free callback of an external buffer (`api::ExternalOwner::Free`), must not read or write the
/// bytes of any ArrayBuffer or view, and must not keep a pointer to them. Some of them run inside
/// a GC, and the bytes of an external buffer (a wasm memory) are only guaranteed to match the
/// buffer at the crossings where engine code runs JS.
#[allow(unused_variables)]
pub trait Host: 'static {
    /// Report all embedder roots. Called at the start of every GC. Must not touch ArrayBuffer
    /// bytes (see the trait documentation).
    fn trace_roots(&self, cx: GcHookCx<'_>, visitor: &mut dyn RootVisitor) {}

    /// Report the fields of a live host object whose flags contain `HAS_TRACE`. Must not touch
    /// ArrayBuffer bytes.
    fn trace_host_object(
        &self,
        cx: GcHookCx<'_>,
        object: HostObjectRef,
        visitor: &mut dyn RootVisitor,
    ) {
    }

    /// A host object with `HAS_FINALIZE` died (or the context is being destroyed). Its header and
    /// reserved slots are still readable. Called exactly once per object. Must not touch
    /// ArrayBuffer bytes. Work that needs the engine is queued and done after the collection.
    fn finalize_host_object(&self, cx: GcHookCx<'_>, object: HostObjectRef) {}

    /// Update weak slots after liveness is final. Must not touch ArrayBuffer bytes.
    fn after_gc_weak(&self, cx: GcHookCx<'_>, updater: &mut dyn WeakUpdater) {}

    /// Call a native function created with `new_native_function`. The return value is read from
    /// `call.vp()[0]`. Throw by returning `Err` (see [`throw_value`]).
    fn call_native(
        &self,
        cx: Context,
        native: *const c_void,
        call: &mut NativeCall,
    ) -> EvalResult<()> {
        type_error(cx, "host does not implement natives")
    }

    /// Call or construct a callable host object (flags `CALLABLE` / `CONSTRUCTOR`). `new_target`
    /// is set for `new`, and the result must then be an object.
    fn call_host_object(
        &self,
        cx: Context,
        object: Handle<ObjectValue>,
        this: Handle<Value>,
        arguments: &[Handle<Value>],
        new_target: Option<Handle<ObjectValue>>,
    ) -> EvalResult<Handle<Value>> {
        type_error(cx, "host object is not callable")
    }

    /// Resolve hook (flags `HAS_RESOLVE`): called on an own property miss before the prototype
    /// walk. Return true if a property was defined, in which case the lookup is retried once.
    fn resolve(
        &self,
        cx: Context,
        object: Handle<ObjectValue>,
        key: Handle<PropertyKey>,
    ) -> EvalResult<bool> {
        Ok(false)
    }

    /// An interrupt was requested with `InterruptHandle::request`. Called on the context's thread at
    /// the next loop back edge or JS function entry. Return false to terminate the running script
    /// uncatchably, true to continue. May run JS. A context with no host answers every interrupt
    /// with termination.
    fn interrupt(&self, cx: Context) -> bool {
        true
    }

    /// Supply the module record for a static import while linking (SpiderMonkey's module resolve
    /// / load hook). `referrer` is the importing module, `referrer_file` its file name. Return
    /// `Ok(None)` to let the engine load the file itself (the default).
    fn resolve_module(
        &self,
        cx: Context,
        referrer: Option<Handle<SourceTextModule>>,
        referrer_file: Handle<FlatString>,
        specifier: Handle<FlatString>,
        attributes: Option<Handle<ImportAttributes>>,
    ) -> EvalResult<Option<Handle<SourceTextModule>>> {
        Ok(None)
    }

    /// Dynamic `import()` (SpiderMonkey's dynamic import hook). `referrer` is the importing module,
    /// None for scripts. Return true to take over the import and complete it later with
    /// `finish_dynamic_import`; false (the default) lets the engine resolve it right away through
    /// `resolve_module` or its own file loading.
    fn dynamic_import(
        &self,
        cx: Context,
        referrer: Option<Handle<SourceTextModule>>,
        referrer_file: Handle<FlatString>,
        specifier: Handle<FlatString>,
        attributes: Option<Handle<ImportAttributes>>,
        import: DynamicImport,
    ) -> EvalResult<bool> {
        Ok(false)
    }

    /// Fill in a module's `import.meta` object when it is first created (SpiderMonkey's
    /// `import.meta` hook).
    fn populate_import_meta(
        &self,
        cx: Context,
        module: Handle<SourceTextModule>,
        meta: Handle<ObjectValue>,
    ) -> EvalResult<()> {
        Ok(())
    }

    /// HostGetCodeForEval: branded objects (e.g. TrustedScript) may supply source.
    /// None leaves a non-string eval argument unchanged, without invoking ToString.
    fn code_for_eval(
        &self,
        cx: Context,
        object: Handle<ObjectValue>,
    ) -> EvalResult<Option<Handle<StringValue>>> {
        Ok(None)
    }

    /// Content Security Policy check before compiling strings (HostEnsureCanCompileStrings): called
    /// for direct and indirect `eval` of a string and for the `Function`, `AsyncFunction`,
    /// `GeneratorFunction` and `AsyncGeneratorFunction` constructors. `code` is the whole source,
    /// `parameters` the parameter strings (empty for eval) and `body` the body (the code for eval).
    /// Original arguments are kept separately from their string conversions, so a host can
    /// apply dynamic-code brand checks without losing object identity or repeating ToString.
    /// Return false to block compilation; the caller then throws an EvalError.
    fn can_compile_strings(
        &self,
        cx: Context,
        kind: CompileStringsKind,
        code: Handle<StringValue>,
        parameters: &[Handle<StringValue>],
        body: Option<Handle<StringValue>>,
        parameter_args: &[Handle<Value>],
        body_arg: Handle<Value>,
    ) -> EvalResult<bool> {
        Ok(true)
    }

    /// HostPromiseRejectionTracker (SpiderMonkey's PromiseRejectionTrackerCallback): a promise was
    /// rejected with no handler (`Reject`), or a handler was added to a rejected promise that had
    /// none (`Handle`). Must not allocate on the JS heap or run JS (record the promise and report
    /// it later); the promise is reported while the engine holds raw pointers. Must not touch
    /// ArrayBuffer bytes.
    fn promise_rejection(
        &self,
        cx: Context,
        promise: HeapPtr<PromiseObject>,
        operation: PromiseRejectionOperation,
    ) {
    }

    /// Called with each promise job (a callable object) when the engine would enqueue it. The host
    /// calls it later, exactly once. Return false to leave the job in the engine's own queue (then
    /// `Context::run_all_tasks` runs it).
    ///
    /// Must not run JS, allocate on the JS heap or touch ArrayBuffer bytes: the job is handed over
    /// while the engine holds raw pointers, possibly at the end of a GC. Record it and run it later.
    fn enqueue_job(
        &self,
        cx: Context,
        job: HeapPtr<ObjectValue>,
        promise: Option<HeapPtr<ObjectValue>>,
        incumbent_global: Option<HeapPtr<ObjectValue>>,
    ) -> bool {
        false
    }

    /// Streaming WebAssembly compilation (`WebAssembly.compileStreaming` and
    /// `instantiateStreaming`, SpiderMonkey's `ConsumeStreamCallback`): take over `response` (the
    /// value the promise resolved to) and feed its bytes to the engine's stream `stream_id` later.
    /// Return false (the default) to refuse: the engine then rejects the promise with a TypeError.
    fn consume_stream(
        &self,
        cx: Context,
        response: Handle<Value>,
        stream_id: u32,
    ) -> EvalResult<bool> {
        Ok(false)
    }

    // Proxy internal methods, called for host objects created with a proxy handler. The defaults
    // behave like an ordinary object.

    fn proxy_get_own_property(
        &self,
        cx: Context,
        handler: *const c_void,
        object: Handle<ObjectValue>,
        key: Handle<PropertyKey>,
    ) -> EvalResult<Option<PropertyDescriptor>> {
        Ok(ordinary_get_own_property(cx, object, key)
            .map(|property| property.to_property_descriptor()))
    }

    fn proxy_define_property(
        &self,
        cx: Context,
        handler: *const c_void,
        object: Handle<ObjectValue>,
        key: Handle<PropertyKey>,
        desc: PropertyDescriptor,
    ) -> EvalResult<bool> {
        ordinary_define_own_property(cx, object, key, desc)
    }

    fn proxy_own_keys(
        &self,
        cx: Context,
        handler: *const c_void,
        object: Handle<ObjectValue>,
    ) -> EvalResult<Vec<Handle<Value>>> {
        Ok(ordinary_own_property_keys(cx, object)?)
    }

    fn proxy_delete(
        &self,
        cx: Context,
        handler: *const c_void,
        object: Handle<ObjectValue>,
        key: Handle<PropertyKey>,
    ) -> EvalResult<bool> {
        ordinary_delete(cx, object, key)
    }

    fn proxy_has(
        &self,
        cx: Context,
        handler: *const c_void,
        object: Handle<ObjectValue>,
        key: Handle<PropertyKey>,
    ) -> EvalResult<bool> {
        ordinary_has_property(cx, object, key)
    }

    fn proxy_get(
        &self,
        cx: Context,
        handler: *const c_void,
        object: Handle<ObjectValue>,
        key: Handle<PropertyKey>,
        receiver: Handle<Value>,
    ) -> EvalResult<Handle<Value>> {
        ordinary_get(cx, object, key, receiver)
    }

    fn proxy_set(
        &self,
        cx: Context,
        handler: *const c_void,
        object: Handle<ObjectValue>,
        key: Handle<PropertyKey>,
        value: Handle<Value>,
        receiver: Handle<Value>,
    ) -> EvalResult<bool> {
        ordinary_set(cx, object, key, value, receiver)
    }

    fn proxy_prevent_extensions(
        &self,
        cx: Context,
        handler: *const c_void,
        object: Handle<ObjectValue>,
    ) -> EvalResult<bool> {
        let mut object = object;
        object.ordinary_prevent_extensions(cx)
    }

    fn proxy_is_extensible(
        &self,
        cx: Context,
        handler: *const c_void,
        object: Handle<ObjectValue>,
    ) -> EvalResult<bool> {
        Ok(object.shape_ptr().is_extensible())
    }

    /// [[GetPrototypeOf]]. Hosts implementing `getPrototypeIfOrdinary` can answer it here too.
    fn proxy_get_prototype(
        &self,
        cx: Context,
        handler: *const c_void,
        object: Handle<ObjectValue>,
    ) -> EvalResult<Option<Handle<ObjectValue>>> {
        Ok(object.prototype().map(|proto| proto.to_handle()))
    }

    fn proxy_set_prototype(
        &self,
        cx: Context,
        handler: *const c_void,
        object: Handle<ObjectValue>,
        prototype: Option<Handle<ObjectValue>>,
    ) -> EvalResult<bool> {
        let mut object = object;
        object.ordinary_set_prototype_of(cx, prototype)
    }
}

/// What is being compiled from a string, for `Host::can_compile_strings`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompileStringsKind {
    /// Direct eval.
    Eval,
    /// Indirect eval, including (0, eval)(source).
    IndirectEval,
    /// One of the `Function` constructors (`Function`, `AsyncFunction`, `GeneratorFunction`,
    /// `AsyncGeneratorFunction`).
    Function,
    /// WebAssembly compilation (`WebAssembly.compile`, `instantiate`, `validate`'s siblings and the
    /// `Module` constructor), CSP's `wasm-unsafe-eval`. `code` is the empty string, there are no
    /// parameters and no body. Blocking it makes the WebAssembly API throw a `CompileError`.
    Wasm,
}

/// Ask the host whether strings may be compiled, throwing an EvalError if not (SpiderMonkey's
/// `JSMSG_CSP_BLOCKED_EVAL` / `JSMSG_CSP_BLOCKED_FUNCTION`). Always allowed without a host.
pub(crate) fn ensure_can_compile_strings(
    cx: Context,
    kind: CompileStringsKind,
    code: Handle<StringValue>,
    parameters: &[Handle<StringValue>],
    body: Option<Handle<StringValue>>,
    parameter_args: &[Handle<Value>],
    body_arg: Handle<Value>,
) -> EvalResult<()> {
    if !has_host(cx)
        || host(cx).can_compile_strings(
            cx,
            kind,
            code,
            parameters,
            body,
            parameter_args,
            body_arg,
        )?
    {
        return Ok(());
    }

    let message = match kind {
        CompileStringsKind::Eval | CompileStringsKind::IndirectEval => {
            "call to eval() blocked by CSP"
        }
        CompileStringsKind::Function => "call to Function() blocked by CSP",
        CompileStringsKind::Wasm => "call to WebAssembly compilation blocked by CSP",
    };
    crate::runtime::error::eval_error(cx, message)
}

/// Ask the host whether WebAssembly may be compiled (`CompileStringsKind::Wasm`). Always allowed
/// without a host. The WebAssembly API throws its own `CompileError` when this is false.
#[allow(dead_code)]
pub(crate) fn can_compile_wasm(cx: Context) -> EvalResult<bool> {
    if !has_host(cx) {
        return Ok(true);
    }

    let code = cx.names.empty_string().as_string();
    host(cx).can_compile_strings(cx, CompileStringsKind::Wasm, code, &[], None, &[], cx.undefined())
}

/// Whether the WebAssembly namespace is enabled for this context (the embedder's switch, e.g.
/// Servo's `ContextOptions::wasm`). Off without a host; on by default once a host is installed.
pub fn wasm_enabled(cx: Context) -> bool {
    try_state(cx).is_some_and(|state| state.wasm_enabled)
}

/// Turn the WebAssembly namespace on or off for realms created from now on. Panics if no host is
/// installed.
pub fn set_wasm_enabled(cx: Context, enabled: bool) {
    state(cx).wasm_enabled = enabled;
}

/// A realm entered by the host without a JS frame.
struct RealmEntry {
    realm: HeapPtr<Realm>,
    /// The VM frame pointer of the dummy frame pushed for this entry.
    frame_pointer: *const (),
}

/// Parameters for the global object of the next realm created by `new_global`.
#[derive(Clone, Copy)]
pub(crate) struct PendingGlobal {
    class: *const c_void,
    flags: HostFlags,
    num_reserved_slots: u32,
}

/// All embedding state of a Context. Only exists once a host is installed.
pub struct EmbedState {
    host: Box<dyn Host>,
    pub(crate) space: HostSpace,
    realm_stack: Vec<RealmEntry>,
    /// Engine tasks wrapped in job objects that have not run yet, keyed by job object address.
    jobs: HashMap<usize, Task>,
    /// Argument buffers of the natives currently executing. Rooted and updated by the GC.
    native_vps: Vec<Vec<Value>>,
    /// Shape used for job objects. Strong root.
    job_shape: HeapPtr<Shape>,
    /// Private symbols holding the reserved slots of native functions.
    native_reserved_keys: [HeapPtr<SymbolValue>; natives::MAX_NATIVE_RESERVED_SLOTS],
    pending_global: Option<PendingGlobal>,
    /// The (object, key) pairs whose resolve hook is currently running. A hook usually defines the
    /// property it resolves, which looks up the own property again; the hook is not re-entered for
    /// the same pair.
    resolving: Vec<(usize, Handle<PropertyKey>)>,
    /// Set while a GC is running. Tasks enqueued during a GC are handed to the host afterwards.
    pub(crate) in_gc: bool,
    /// The engine cells of the calls currently running, innermost last (the active-call roots of
    /// the WebAssembly design §2.4, invariant A1). Strong roots, visited and updated by every GC.
    /// Pushed and popped only by `push_cell_call_root` and `pop_cell_call_root`.
    cell_call_roots: Vec<HeapPtr<ObjectValue>>,
    /// The hooks of every engine cell not yet finalized, by cell address (cells never move).
    cell_hooks: HashMap<usize, &'static EngineCellHooks>,
    /// The free callbacks of external buffers created with `ExternalOwner::Free`, by the address
    /// of the engine cell that owns the memory. Run by that cell's finalizer.
    external_frees: HashMap<usize, Box<dyn FnOnce()>>,
    /// Whether the WebAssembly namespace is enabled (`set_wasm_enabled`).
    wasm_enabled: bool,
}

impl EmbedState {
    fn visit_roots(&mut self, visitor: &mut impl HeapVisitor) {
        for entry in &mut self.realm_stack {
            visitor.visit_pointer(&mut entry.realm);
        }

        for cell in &mut self.cell_call_roots {
            visitor.visit_pointer(cell);
        }

        for task in self.jobs.values_mut() {
            task.visit_roots(visitor);
        }

        for vp in &mut self.native_vps {
            for value in vp {
                visitor.visit_value(value);
            }
        }

        visitor.visit_pointer(&mut self.job_shape);
        for key in &mut self.native_reserved_keys {
            visitor.visit_pointer(key);
        }
    }
}

/// Embedding state of a context. Panics if no host is installed.
///
/// The reference is detached from `cx` (see `try_state`). Use it within one statement or a short
/// block: it must not be kept across a call into the host, nor across anything else that takes the
/// state again (allocating a host object, `is_in_host_space`, ...), since that creates another
/// `&mut EmbedState`. Re-borrow after such a call instead.
#[inline]
pub(crate) fn state<'a>(cx: Context) -> &'a mut EmbedState {
    try_state(cx).expect("no host installed on this context")
}

#[inline]
pub(crate) fn try_state<'a>(mut cx: Context) -> Option<&'a mut EmbedState> {
    let state = cx.embed.as_deref_mut()?;
    // SAFETY: The state is boxed and lives until the context is dropped. Like `Heap::info`, the
    // reference is detached from the context pointer so it can be used alongside `cx`.
    Some(unsafe { &mut *(state as *mut EmbedState) })
}

/// The installed host. Panics if no host is installed.
#[inline]
pub(crate) fn host<'a>(cx: Context) -> &'a dyn Host {
    // Only a shared borrow of the state, which ends here: the host is called while other code
    // re-borrows the state mutably.
    let state = cx
        .embed
        .as_deref()
        .expect("no host installed on this context");
    let host: *const dyn Host = &*state.host;
    // SAFETY: The host has its own boxed allocation, which lives until the state is dropped at the
    // end of `teardown`, after the last call into the host, and is never written or moved while it
    // lives (`EmbedState::host` is never replaced). All its methods take `&self`, so shared
    // references to it may coexist with mutable borrows of the rest of the state.
    unsafe { &*host }
}

/// Install the host on a context. Must be called once, before any other embedding function.
pub fn install_host(mut cx: Context, host: Box<dyn Host>) -> AllocResult<()> {
    assert!(cx.embed.is_none(), "a host is already installed on this context");

    // Handles created here are released with the caller's handle scope
    let job_shape = ShapeRegistry::get_root_object_shape(cx, HeapItemKind::HostObject, None, 0)?;
    let reserved_0 = natives::new_private_symbol(cx, "embed reserved 0")?;
    let reserved_1 = natives::new_private_symbol(cx, "embed reserved 1")?;

    cx.embed = Some(Box::new(EmbedState {
        host,
        space: HostSpace::new(),
        realm_stack: vec![],
        jobs: HashMap::new(),
        native_vps: vec![],
        job_shape: *job_shape,
        native_reserved_keys: [*reserved_0, *reserved_1],
        pending_global: None,
        resolving: vec![],
        in_gc: false,
        cell_call_roots: vec![],
        cell_hooks: HashMap::new(),
        external_frees: HashMap::new(),
        wasm_enabled: true,
    }));

    cx.task_queue_mut().set_has_host();

    // Hand over any tasks that were enqueued before the host was installed
    jobs::hand_tasks_to_host(cx);

    Ok(())
}

/// Whether a host is installed on this context.
pub fn has_host(cx: Context) -> bool {
    cx.embed.is_some()
}

/// Create a new host object in the host space. Fails during a GC (from a GC hook).
///
/// `proxy_handler` is null for ordinary host objects. A non-null handler makes every internal
/// method call the matching `Host::proxy_*` method, and `proxy_private` is stored with the object.
pub fn new_host_object(
    cx: Context,
    realm: Handle<Realm>,
    class: *const c_void,
    flags: HostFlags,
    proto: Option<Handle<ObjectValue>>,
    num_reserved_slots: u32,
    proxy_handler: *const c_void,
    proxy_private: Handle<Value>,
) -> AllocResult<Handle<ObjectValue>> {
    debug_assert!(!flags.intersects(HostFlags::INTERNAL_JOB | HostFlags::INTERNAL_CELL));

    // Host objects cannot be created during a GC (see `alloc_host_object`), which fails as out of
    // memory: an error with the `alloc_error` feature, else the panic in `alloc_host_object`.
    #[cfg(feature = "alloc_error")]
    if state(cx).in_gc {
        return Err(crate::runtime::alloc_error::AllocError::oom());
    }

    // May allocate, so look up the shape before allocating in the host space
    let root_shape = ShapeRegistry::get_root_object_shape(cx, HeapItemKind::HostObject, proto, 0)?;

    // An `IS_HTMLDDA` class (SpiderMonkey's `JSCLASS_EMULATES_UNDEFINED`, e.g. for `document.all`)
    // gets the [[IsHTMLDDA]] internal slot (ECMA-262 B.3.6) on a private copy of the shape. Still
    // looked up before the host space allocation, which cannot tolerate a collection after it.
    let shape = if flags.contains(HostFlags::IS_HTMLDDA) {
        root_shape.clone_with_htmldda(cx)?
    } else {
        *root_shape
    };

    let object = host_object::alloc_host_object::<ObjectValue>(
        cx,
        shape,
        host_space::HostAllocation {
            object_size: shape.object_byte_size(),
            num_reserved_slots,
            flags: flags & !HostFlags::IS_GLOBAL,
            class,
            handler: proxy_handler,
            proxy_private: *proxy_private,
            realm: *realm,
        },
    );

    Ok(object.to_handle())
}

/// Create a new realm whose global object is a host object with the given class, flags and
/// reserved slots. Only `HAS_TRACE`, `HAS_FINALIZE` and `HAS_RESOLVE` are honored for globals.
pub fn new_global(
    cx: Context,
    class: *const c_void,
    flags: HostFlags,
    num_reserved_slots: u32,
) -> AllocResult<(Handle<Realm>, Handle<ObjectValue>)> {
    state(cx).pending_global = Some(PendingGlobal {
        class,
        flags: (flags & (HostFlags::HAS_TRACE | HostFlags::HAS_FINALIZE | HostFlags::HAS_RESOLVE))
            | HostFlags::IS_GLOBAL,
        num_reserved_slots,
    });

    let realm = Realm::new(cx);

    // Clear the request even if realm creation failed
    let pending = state(cx).pending_global.take();
    debug_assert!(pending.is_none() || realm.is_err(), "global object was not created");

    let mut realm = realm?;
    // As for the initial realm and the shell's realms: Annex B methods (String.prototype.substr, escape, Date's getYear,
    // RegExp's compile...) and the other optional globals the context's options ask for. Without this every host global
    // (Servo's Window) lacked them, and YouTube's player failed on `substr`.
    realm.install_optional_globals(cx)?;
    let global = realm.global_object().as_object();

    Ok((realm, global))
}

/// Called when a realm's global object is created. Return the pending host global request, if any.
pub(crate) fn take_pending_global(cx: Context) -> Option<PendingGlobal> {
    try_state(cx)?.pending_global.take()
}

/// Allocate the global object of a new realm in the host space.
pub(crate) fn alloc_host_global(
    cx: Context,
    realm: Handle<Realm>,
    shape: HeapPtr<Shape>,
    pending: PendingGlobal,
) -> HeapPtr<ObjectValue> {
    host_object::alloc_host_object(
        cx,
        shape,
        host_space::HostAllocation {
            object_size: shape.object_byte_size(),
            num_reserved_slots: pending.num_reserved_slots,
            flags: pending.flags,
            class: pending.class,
            handler: std::ptr::null(),
            proxy_private: Value::undefined(),
            realm: *realm,
        },
    )
}

/// Whether the object lives in the host space (created by `new_host_object` or `new_global`, or a
/// job object).
pub fn is_host_object(object: HeapPtr<ObjectValue>) -> bool {
    host_header(object).is_some()
}

/// Whether a heap item lives in the host space, so it never moves: host objects, job objects and
/// pinned engine objects (promises).
pub fn is_in_host_space(ptr: *const c_void) -> bool {
    let cx = context_of(ptr);
    match try_state(cx) {
        Some(state) => state.space.contains(ptr.cast()),
        None => false,
    }
}

/// The header of any object in the host space, pinned engine objects included.
#[inline]
fn space_header(object: HeapPtr<ObjectValue>) -> Option<HostHeaderPtr> {
    if is_in_host_space(object.as_ptr().cast()) {
        // SAFETY: `object` is a live object (the caller's `HeapPtr` premise) and lies in the host
        // space, and every object in the host space is the object of a `HostSpace::alloc`
        // allocation, so it is directly preceded by its header.
        Some(unsafe { HostHeaderPtr::from_object_ptr(object.as_ptr()) })
    } else {
        None
    }
}

/// The header of a host object. Pinned engine objects are not host objects.
#[inline]
fn host_header(object: HeapPtr<ObjectValue>) -> Option<HostHeaderPtr> {
    space_header(object).filter(|header| !header.flags().contains(HostFlags::PINNED))
}

/// Swap the contents of two host objects of the same layout, so that each object (its address, its identity) now has the
/// other one's class, proxy handler and private, realm, prototype, properties and reserved slots. This is SpiderMonkey's
/// same-compartment `JSObject::swap`, which `JS_TransplantObject` uses when Servo retargets a WindowProxy at a new Window
/// while keeping the proxy's identity. Identity hash codes stay with their address, so maps keyed by either object still
/// find it. Returns false, changing nothing, unless both are (unpinned) host objects with the same allocation size and
/// reserved slot count. Must not be called during a GC.
pub fn swap_host_objects(mut a: HeapPtr<ObjectValue>, mut b: HeapPtr<ObjectValue>) -> bool {
    if a.ptr_eq(&b) {
        return true;
    }
    if host_header(a).is_none() || host_header(b).is_none() {
        return false;
    }
    let (hash_a, hash_b) = (a.raw_hash_code(), b.raw_hash_code());
    // SAFETY: both are live host objects (checked above), the caller is not inside a GC, and no reference into either
    // allocation is live across this call.
    let swapped = unsafe { host_space::swap_allocations(a.as_ptr().cast(), b.as_ptr().cast()) };
    if swapped {
        a.set_raw_hash_code(hash_a);
        b.set_raw_hash_code(hash_b);
    }
    swapped
}

/// Whether an object is an engine object pinned in the host space (every promise, when a host is
/// installed).
pub fn is_pinned(object: HeapPtr<ObjectValue>) -> bool {
    space_header(object).is_some_and(|header| header.flags().contains(HostFlags::PINNED))
}

/// Debug check that a heap pointer will not move: panics in debug builds if `ptr` points into
/// the moving heap's current semispace (it must be in the host space or the permanent space).
/// Does nothing in release builds.
#[inline]
pub fn assert_stable(cx: Context, ptr: *const c_void) {
    if cfg!(debug_assertions) {
        let bounds = cx.heap.current_heap_bounds();
        let ptr = ptr.cast::<u8>();
        assert!(
            !bounds.contains(&ptr),
            "{ptr:?} points into the moving heap and will move at the next GC"
        );
    }
}

/// Allocate an engine object that must never move (e.g. a promise, whose object Servo uses as a
/// reflector) in the host space. Does not allocate on the moving heap. The object belongs to the
/// current realm.
pub(crate) fn alloc_pinned_object<T>(cx: Context, shape: HeapPtr<Shape>) -> HeapPtr<T> {
    host_object::alloc_host_object(
        cx,
        shape,
        host_space::HostAllocation {
            object_size: shape.object_byte_size(),
            num_reserved_slots: 0,
            flags: HostFlags::PINNED,
            class: std::ptr::null(),
            handler: std::ptr::null(),
            proxy_private: Value::undefined(),
            realm: cx.current_realm_ptr(),
        },
    )
}

// Engine cells (WebAssembly design §2.2).

/// The trace hook of an engine cell (see `EngineCellHooks::trace`).
pub type EngineCellTrace = fn(GcHookCx<'_>, HeapPtr<ObjectValue>, &mut dyn RootVisitor);

/// The hooks of an engine cell: an engine-owned object in the host space (it never moves) with
/// its own trace and finalize hooks, dispatched by the engine and never by the host. The cell is
/// not a host object to the embedding API (`is_host_object`, `host_class`, `host_flags` and
/// `swap_host_objects` do not see it).
///
/// The hooks run like the host's GC hooks, without JS on the stack, so they must not allocate on
/// the JS heap, create handles, run JS, take a GC, or read or write ArrayBuffer bytes (§3.2). They
/// receive a [`GcHookCx`], not a `Context`, for that reason.
pub struct EngineCellHooks {
    /// Report the strong edges the cell holds outside its own fields and reserved slots. Called
    /// once per GC for every live cell, and by `gc::verify`. Every slot may be updated (the GC
    /// moves objects).
    pub trace: Option<EngineCellTrace>,
    /// The cell died, or the context is being destroyed. Called exactly once per cell, from the GC
    /// sweep or from teardown; the cell's header and reserved slots are still readable.
    pub finalize: fn(GcHookCx<'_>, HeapPtr<ObjectValue>),
}

/// Allocate an engine cell with `hooks` in `realm`, with `num_reserved_slots` undefined reserved
/// slots (traced like a host object's). The cell keeps `realm` alive through its header. Does not
/// allocate on the moving heap, so it cannot trigger a GC, and `shape` must be looked up before.
/// Panics during a GC, like every host space allocation, and if no host is installed.
pub(crate) fn alloc_engine_cell(
    cx: Context,
    shape: HeapPtr<Shape>,
    hooks: &'static EngineCellHooks,
    realm: HeapPtr<Realm>,
    num_reserved_slots: u32,
) -> HeapPtr<ObjectValue> {
    let mut flags = HostFlags::PINNED | HostFlags::INTERNAL_CELL;
    if hooks.trace.is_some() {
        // So that `gc::verify` reaches the trace hook too: it dispatches on HAS_TRACE through
        // `trace_host_object_for_verify`, which knows about cells.
        flags |= HostFlags::HAS_TRACE;
    }

    let cell: HeapPtr<ObjectValue> = host_object::alloc_host_object(
        cx,
        shape,
        host_space::HostAllocation {
            object_size: shape.object_byte_size(),
            num_reserved_slots,
            flags,
            // Opaque to everyone: the hooks are looked up in `cell_hooks`, never through the class
            class: (hooks as *const EngineCellHooks).cast(),
            handler: std::ptr::null(),
            proxy_private: Value::undefined(),
            realm,
        },
    );

    // Host space objects never move, so the address is the cell's key until it is finalized
    state(cx).cell_hooks.insert(cell.as_ptr() as usize, hooks);

    cell
}

/// Allocate an engine cell with the engine's plain cell shape (a host-space object kind with no
/// prototype and no properties). See `alloc_engine_cell`.
pub(crate) fn new_engine_cell(
    cx: Context,
    hooks: &'static EngineCellHooks,
    realm: HeapPtr<Realm>,
    num_reserved_slots: u32,
) -> Handle<ObjectValue> {
    // The job shape is exactly that shape, and a strong root of the state.
    let shape = state(cx).job_shape;
    alloc_engine_cell(cx, shape, hooks, realm, num_reserved_slots).to_handle()
}

/// The header of an engine cell, or None if the object is not one.
#[inline]
fn cell_header(object: HeapPtr<ObjectValue>) -> Option<HostHeaderPtr> {
    space_header(object).filter(|header| header.flags().contains(HostFlags::INTERNAL_CELL))
}

/// Whether the object is an engine cell.
pub(crate) fn is_engine_cell(object: HeapPtr<ObjectValue>) -> bool {
    cell_header(object).is_some()
}

/// The hooks of a live engine cell, or None if the object is not one. Lets a caller check what kind
/// of cell it holds by comparing the hooks' address.
#[allow(dead_code)] // used by the WebAssembly scaffold (slice W0a)
pub(crate) fn engine_cell_hooks(object: HeapPtr<ObjectValue>) -> Option<&'static EngineCellHooks> {
    if !is_engine_cell(object) {
        return None;
    }

    state(context_of(object.as_ptr().cast()))
        .cell_hooks
        .get(&(object.as_ptr() as usize))
        .copied()
}

/// Read a reserved slot of an engine cell. Panics if not a cell or out of range.
#[allow(dead_code)] // used by the WebAssembly scaffold (slice W0a)
pub(crate) fn engine_cell_slot(cell: HeapPtr<ObjectValue>, index: u32) -> Value {
    cell_header(cell)
        .expect("not an engine cell")
        .reserved_slot(index)
}

/// Write a reserved slot of an engine cell. Panics if not a cell or out of range.
#[allow(dead_code)] // used by the WebAssembly scaffold (slice W0a)
pub(crate) fn set_engine_cell_slot(cell: HeapPtr<ObjectValue>, index: u32, value: Value) {
    cell_header(cell)
        .expect("not an engine cell")
        .set_reserved_slot(index, value);
}

/// Run the trace hook of an engine cell the GC marked (a no-op for a cell without a trace hook).
pub(crate) fn trace_engine_cell(
    cx: Context,
    object: HeapPtr<AnyHeapItem>,
    gc: &mut GarbageCollector,
) {
    let hooks = state(cx)
        .cell_hooks
        .get(&(object.as_ptr() as usize))
        .copied();
    // A marked cell is live, and only `finalize_engine_cell` (on a dead cell) removes its entry
    debug_assert!(hooks.is_some(), "live engine cell has no hook table entry");
    if let Some(trace) = hooks.and_then(|hooks| hooks.trace) {
        let _label = GcHookLabel::new(cx, "trace_engine_cell");
        trace(GcHookCx::new(cx), object.cast(), &mut GcRootVisitor { gc });
    }
}

/// Run the finalize hook of a dead (or torn down) engine cell, exactly once: its entry is removed
/// from the hook table before the hook runs.
fn finalize_engine_cell(cx: Context, object: HeapPtr<AnyHeapItem>) {
    let hooks = state(cx).cell_hooks.remove(&(object.as_ptr() as usize));
    if let Some(hooks) = hooks {
        let _label = GcHookLabel::new(cx, "finalize_engine_cell");
        (hooks.finalize)(GcHookCx::new(cx), object.cast());
    }
}

/// Root `cell` for the duration of a call that uses it (design §2.4, invariant A1): it cannot be
/// finalized until the matching `pop_cell_call_root`. Returns the depth to pass to the pop. Calls
/// nest: pops must come in the reverse order of the pushes. Panics if no host is installed.
#[allow(dead_code)] // used by the WebAssembly scaffold (slice W0a)
pub(crate) fn push_cell_call_root(cx: Context, cell: HeapPtr<ObjectValue>) -> usize {
    debug_assert!(is_engine_cell(cell));
    let roots = &mut state(cx).cell_call_roots;
    roots.push(cell);
    roots.len() - 1
}

/// Remove the root pushed by `push_cell_call_root` that returned `depth`. A mismatch (not the
/// innermost root, or not that cell) is an engine bug that would leave a cell unrooted or rooted
/// forever, so it aborts the process (it does not panic, so an unwind cannot skip the pop).
#[allow(dead_code)] // used by the WebAssembly scaffold (slice W0a)
pub(crate) fn pop_cell_call_root(cx: Context, depth: usize, cell: HeapPtr<ObjectValue>) {
    let roots = &mut state(cx).cell_call_roots;
    let matches = roots.len() == depth + 1 && roots[depth].ptr_eq(&cell);
    if !matches {
        eprintln!(
            "brimstone: engine cell call roots out of order (depth {depth} of {})",
            roots.len()
        );
        std::process::abort();
    }
    roots.pop();
}

/// Number of cell call roots, i.e. calls using an engine cell currently on the stack.
#[allow(dead_code)] // used by the WebAssembly scaffold (slice W0a)
pub(crate) fn cell_call_root_count(cx: Context) -> usize {
    try_state(cx).map_or(0, |state| state.cell_call_roots.len())
}

/// Record the free callback of an external buffer's memory, owned by `cell`: the cell's finalizer
/// runs it.
pub(crate) fn register_external_free(
    cx: Context,
    cell: HeapPtr<ObjectValue>,
    free: Box<dyn FnOnce()>,
) {
    state(cx)
        .external_frees
        .insert(cell.as_ptr() as usize, free);
}

/// Drop the free callback registered for `cell` without calling it (the buffer was not created).
pub(crate) fn forget_external_free(cx: Context, cell: HeapPtr<ObjectValue>) {
    let free = state(cx).external_frees.remove(&(cell.as_ptr() as usize));
    drop(free);
}

/// The finalizer of the cells that own external buffers created with `ExternalOwner::Free`.
fn finalize_external_free_cell(cx: GcHookCx<'_>, cell: HeapPtr<ObjectValue>) {
    // The entry is removed before the callback runs, so it runs exactly once
    let free = cx.take_external_free(cell);
    if let Some(free) = free {
        free();
    }
}

/// The hooks of the cells that free an external buffer's memory (`ExternalOwner::Free`).
pub(crate) static EXTERNAL_FREE_CELL_HOOKS: EngineCellHooks =
    EngineCellHooks { trace: None, finalize: finalize_external_free_cell };

fn expect_host_header(object: HeapPtr<ObjectValue>) -> HostHeaderPtr {
    host_header(object).expect("not a host object")
}

/// The class pointer of a host object, or None if the object is not a host object.
pub fn host_class(object: HeapPtr<ObjectValue>) -> Option<*const c_void> {
    host_header(object).map(|header| header.class())
}

/// The flags of a host object, or None if the object is not a host object.
pub fn host_flags(object: HeapPtr<ObjectValue>) -> Option<HostFlags> {
    host_header(object).map(|header| header.flags())
}

/// The proxy handler of a host proxy, or None if the object is not a host proxy.
pub fn host_handler(object: HeapPtr<ObjectValue>) -> Option<*const c_void> {
    host_header(object)
        .filter(|header| header.is_proxy())
        .map(|header| header.handler())
}

/// Number of reserved slots of a host object. Panics if not a host object.
pub fn num_reserved_slots(object: HeapPtr<ObjectValue>) -> u32 {
    expect_host_header(object).num_reserved_slots()
}

/// Read a reserved slot of a host object. Panics if not a host object or out of range.
pub fn reserved_slot(object: HeapPtr<ObjectValue>, index: u32) -> Value {
    expect_host_header(object).reserved_slot(index)
}

/// Write a reserved slot of a host object. Panics if not a host object or out of range.
pub fn set_reserved_slot(object: HeapPtr<ObjectValue>, index: u32, value: Value) {
    expect_host_header(object).set_reserved_slot(index, value);
}

/// The private value of a host proxy (undefined for other host objects). Panics if not a host
/// object.
pub fn proxy_private(object: HeapPtr<ObjectValue>) -> Value {
    expect_host_header(object).proxy_private()
}

/// Set the private value of a host object. Panics if not a host object.
pub fn set_proxy_private(object: HeapPtr<ObjectValue>, value: Value) {
    expect_host_header(object).set_proxy_private(value);
}

/// Enter a realm without a JS frame. Objects created until the matching `leave_realm` belong to
/// this realm, unless a newer JS frame (e.g. a called function) has its own realm.
///
/// Implemented by pushing the realm's dummy frame onto the VM stack, so entries nest correctly
/// with JS frames: the newest entry wins over older frames and newer frames win over it. Works with
/// or without an installed host (nesting is only checked when a host is installed).
pub fn enter_realm(mut cx: Context, realm: HeapPtr<Realm>) {
    let was_empty = cx.vm().is_stack_empty();

    let push_result = cx.vm().push_initial_realm_stack_frame(realm);
    assert!(push_result.is_ok(), "stack overflow while entering realm");

    if was_empty {
        cx.vm().mark_stack_trace_top();
    }

    let frame_pointer = cx.vm().frame_pointer();
    if let Some(state) = try_state(cx) {
        state.realm_stack.push(RealmEntry { realm, frame_pointer });
    }
}

/// Leave the realm entered by the matching `enter_realm`. Panics if entries are not nested
/// correctly with VM frames.
pub fn leave_realm(mut cx: Context, realm: HeapPtr<Realm>) {
    if let Some(state) = try_state(cx) {
        let entry = state
            .realm_stack
            .pop()
            .expect("leave_realm without enter_realm");

        assert!(entry.realm.ptr_eq(&realm), "leave_realm called with a different realm");
        assert!(
            entry.frame_pointer == cx.vm().frame_pointer(),
            "leave_realm called while frames pushed after enter_realm are still active"
        );
    } else {
        assert!(!cx.vm().is_stack_empty(), "leave_realm without enter_realm");
        assert!(
            cx.vm().closure().ptr_eq(&realm.empty_function_ptr()),
            "leave_realm called while frames pushed after enter_realm are still active"
        );
    }

    cx.vm().pop_initial_realm_stack_frame();
}

/// Number of realms currently entered with `enter_realm`.
pub fn entered_realm_depth(cx: Context) -> usize {
    try_state(cx).map_or(0, |state| state.realm_stack.len())
}

/// The current realm: the realm of the newest JS frame or entered realm.
pub fn current_realm(cx: Context) -> Handle<Realm> {
    cx.current_realm()
}

/// Run `f` with `realm` entered.
pub fn with_realm<R>(cx: Context, realm: Handle<Realm>, f: impl FnOnce(Context) -> R) -> R {
    enter_realm(cx, *realm);
    let result = f(cx);
    leave_realm(cx, *realm);
    result
}

/// Run `f` in the current realm, or in the initial realm if the VM stack is empty.
pub(crate) fn with_any_realm<R>(mut cx: Context, f: impl FnOnce(Context) -> R) -> R {
    if cx.vm().is_stack_empty() {
        let realm = cx.initial_realm();
        with_realm(cx, realm, f)
    } else {
        f(cx)
    }
}

/// The realm an object belongs to: the creation realm of host objects, the function realm of
/// functions, and otherwise the current realm (ordinary objects do not record their realm).
pub fn realm_of_object(cx: Context, object: HeapPtr<ObjectValue>) -> HeapPtr<Realm> {
    // Host objects and pinned objects record their creation realm
    if let Some(header) = space_header(object) {
        return header.realm();
    }

    if let Some(closure) = object.as_opt::<ClosureObject>() {
        return closure.function_ptr().realm_ptr();
    }

    cx.current_realm_ptr()
}

/// The realm a job object handed to `Host::enqueue_job` runs in (its execution realm: the realm of
/// the reaction handler, of the resumed async function, or of the thenable job's `then`), or None
/// if the object is not a job. The same realm is reported as the job's `incumbent_global`'s realm.
pub fn job_realm(job: HeapPtr<ObjectValue>) -> Option<HeapPtr<Realm>> {
    let header = host_header(job)?;
    if header.flags().contains(HostFlags::INTERNAL_JOB) {
        Some(header.realm())
    } else {
        None
    }
}

/// The global object of a realm.
pub fn global_of_realm(realm: HeapPtr<Realm>) -> HeapPtr<ObjectValue> {
    realm.global_object_ptr().as_object()
}

/// Find the Context from any heap pointer, including pointers to host objects.
pub fn context_of(ptr: *const c_void) -> Context {
    HeapInfo::from_raw_heap_ptr(ptr).cx()
}

/// Requests an interrupt of a context from any thread (SpiderMonkey's `JS_RequestInterruptCallback`).
#[derive(Clone)]
pub struct InterruptHandle(Arc<AtomicBool>);

impl InterruptHandle {
    /// Request an interrupt. The context calls `Host::interrupt` at its next loop back edge or JS
    /// function entry; with no host installed the running script terminates uncatchably.
    pub fn request(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether an interrupt is requested and not yet handled.
    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// A handle that requests interrupts of this context. Can be sent to other threads.
pub fn interrupt_handle(mut cx: Context) -> InterruptHandle {
    InterruptHandle(cx.vm().interrupt_flag())
}

/// Number of bytes per VM stack slot, the unit of the stack limit.
pub const STACK_SLOT_BYTES: usize = 8;

/// Limit the depth of JS recursion. `bytes` is the VM stack size to allow (clamped to the VM's
/// stack, which is the default). Recursion past it, from JS->JS calls and from natives that call
/// back into JS, throws a catchable RangeError. Native re-entry is additionally limited by a fixed
/// nesting depth of the dispatch loop.
pub fn set_stack_limit(mut cx: Context, bytes: usize) {
    cx.vm().set_stack_limit_slots(bytes / STACK_SLOT_BYTES);
}

/// Limit how deeply natives and builtins may re-enter JS (e.g. `f.apply` recursion). Each level
/// uses native stack, so a host running scripts on threads with small stacks (debug builds use
/// the most) lowers this. Clamped to the engine's default (32 in debug builds, 2048 in release
/// builds). Past it a catchable RangeError is thrown.
pub fn set_native_reentry_limit(mut cx: Context, depth: usize) {
    cx.vm().set_max_dispatch_depth(depth);
}

/// Limit native stack use like SpiderMonkey's `JS_SetNativeStackQuota`: the VM may use `bytes` of
/// native stack below the caller's current stack pointer (call this near the base of the thread
/// that runs scripts, with the thread's stack size minus a safety margin). Every dispatch loop
/// entry and every builtin or native call compares the stack pointer against the limit and throws
/// a catchable RangeError ("too much recursion") past it. The re-entry depth limit stays as a
/// second guard. Pass 0 to remove the quota.
#[inline(never)]
pub fn set_native_stack_quota(mut cx: Context, bytes: usize) {
    if bytes == 0 {
        cx.vm().set_native_stack_limit(0);
        return;
    }

    let marker = 0u8;
    let stack_pointer = std::hint::black_box(&marker) as *const u8 as usize;
    cx.vm()
        .set_native_stack_limit(stack_pointer.saturating_sub(bytes).max(1));
}

/// The lowest native stack address the VM may be re-entered at, or 0 if no quota is set.
pub fn native_stack_limit(mut cx: Context) -> usize {
    cx.vm().native_stack_limit()
}

/// The current native re-entry limit.
pub fn native_reentry_limit(mut cx: Context) -> usize {
    cx.vm().max_dispatch_depth()
}

/// The current VM stack limit in bytes.
pub fn stack_limit(mut cx: Context) -> usize {
    cx.vm().stack_limit_slots() * STACK_SLOT_BYTES
}

pub use crate::runtime::stack_trace::CapturedStackFrame;

/// The current JS stack, newest frame first, up to `max_frames` frames (0 means all), as
/// SpiderMonkey's `JS::CaptureCurrentStack` sees it. Empty when no JS is running.
pub fn capture_stack(cx: Context, max_frames: usize) -> AllocResult<Vec<CapturedStackFrame>> {
    crate::runtime::stack_trace::capture_current_stack(cx, max_frames)
}

/// The stack captured when an Error object was created, newest frame first (0 means all frames).
/// None if the object is not an Error.
pub fn error_stack_frames(
    cx: Context,
    error: Handle<ObjectValue>,
    max_frames: usize,
) -> AllocResult<Option<Vec<CapturedStackFrame>>> {
    let Some(error) = error.as_opt::<crate::runtime::intrinsics::error_object::ErrorObject>()
    else {
        return Ok(None);
    };
    let Some(stack_frame_info) = error.stack_frame_info() else {
        return Ok(Some(vec![]));
    };

    let frames = crate::runtime::stack_trace::describe_stack_frames(
        cx,
        stack_frame_info.to_handle(),
        max_frames,
    )?;
    Ok(Some(frames))
}

/// Memory statistics of a context, e.g. for a memory reporter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapStats {
    /// Bytes allocated in the current semispace (live objects plus garbage since the last GC).
    pub semispace_used_bytes: usize,
    /// Capacity of the current semispace in bytes.
    pub semispace_capacity_bytes: usize,
    /// Bytes in the permanent space (builtins that are never collected).
    pub permanent_bytes: usize,
    /// Total size of the moving heap (both semispaces, the permanent space and the header).
    pub heap_size_bytes: usize,
    /// The heap size limit; the heap grows up to this size.
    pub max_heap_size_bytes: usize,
    /// Bytes used by host objects (live, or dead but not yet swept).
    pub host_space_used_bytes: usize,
    /// Bytes of address space reserved for the host space.
    pub host_space_reserved_bytes: usize,
    /// Number of host objects.
    pub host_object_count: usize,
    /// Number of collections, and how many of them grew or shrank the heap.
    pub gc_count: u64,
    pub gc_grow_count: u64,
    pub gc_shrink_count: u64,
    /// Live bytes in the semispace right after the last collection, and the most seen.
    pub last_live_bytes: usize,
    pub peak_live_bytes: usize,
    /// Bytes of free semispace handed back to the operating system, in total.
    pub released_bytes: u64,
}

/// Memory statistics of a context.
pub fn heap_stats(cx: Context) -> HeapStats {
    let bounds = cx.heap.current_heap_bounds();
    let gc_stats = cx.heap.gc_stats();
    let (host_space_used_bytes, host_space_reserved_bytes, host_object_count) = match try_state(cx)
    {
        Some(state) => {
            (state.space.used_bytes(), state.space.region_bytes(), state.space.num_objects())
        }
        None => (0, 0, 0),
    };

    HeapStats {
        semispace_used_bytes: cx.heap.bytes_allocated(),
        semispace_capacity_bytes: bounds.end as usize - bounds.start as usize,
        permanent_bytes: cx.heap.permanent_bytes_allocated(),
        heap_size_bytes: cx.heap.heap_size(),
        max_heap_size_bytes: cx.options.max_heap_size,
        host_space_used_bytes,
        host_space_reserved_bytes,
        host_object_count,
        gc_count: gc_stats.gc_count,
        gc_grow_count: gc_stats.grow_count,
        gc_shrink_count: gc_stats.shrink_count,
        last_live_bytes: gc_stats.last_live_bytes,
        peak_live_bytes: gc_stats.peak_live_bytes,
        released_bytes: gc_stats.released_bytes,
    }
}

pub use crate::runtime::bytecode::lazy::CompileStats;

/// Counts of the functions the context has compiled since it was created, with the source bytes
/// they span: generated with their script, created as lazy stubs, generated lazily on their first
/// call, and stubs not generated (nor collected) yet.
/// Bytes of parse arenas (AST and scope tree) kept alive by lazy stubs that have not been generated yet.
pub fn retained_parse_bytes(cx: Context) -> usize {
    if crate::runtime::bytecode::lazy::reparse_enabled(cx) {
        cx.lazy_functions.retained_ast_bytes()
    } else {
        cx.lazy_functions.retained_parse_bytes()
    }
}

/// Generate pending lazy stubs, largest program first, until the parse arenas they keep alive fit in
/// `budget_bytes` or `deadline` passes. For the host's idle time after a page load; see
/// `bytecode::lazy::trim_retained_programs`.
pub fn trim_lazy_programs(
    cx: Context,
    budget_bytes: usize,
    deadline: std::time::Instant,
) -> EvalResult<crate::runtime::bytecode::lazy::TrimResult> {
    crate::runtime::bytecode::lazy::trim_retained_programs(cx, budget_bytes, deadline)
}

pub fn compile_stats(cx: Context) -> CompileStats {
    cx.lazy_functions.compile_stats()
}

/// Drop the AST of every program with pending lazy stubs, as when their scripts have run (a
/// no-op unless the context re-parses). For tools and tests.
pub fn drop_lazy_asts(cx: Context) -> EvalResult<()> {
    for program in cx.lazy_functions.programs_with_ast() {
        if let Err(error) = crate::runtime::bytecode::lazy::detach_program(cx, program) {
            return crate::runtime::error::syntax_error(cx, &error.to_string());
        }
    }
    Ok(())
}

/// Generate the stubs pending now, but not the stubs that doing so creates. For tools.
pub fn generate_pending_round(cx: Context) -> EvalResult<()> {
    crate::runtime::bytecode::lazy::generate_pending_round(cx)
}

/// Generate every pending lazy stub, including the stubs created while doing so. For tools and
/// tests.
pub fn generate_all_lazy_functions(cx: Context) -> EvalResult<()> {
    crate::runtime::bytecode::lazy::generate_all_lazy_functions(cx)
}

/// What dropping ASTs and re-parsing lazy functions did so far (`Options::lazy_reparse`).
pub fn reparse_stats(cx: Context) -> crate::runtime::bytecode::lazy::ReparseStats {
    cx.lazy_functions.reparse
}

/// Bytes of the scope trees that pending lazy stubs keep alive (they outlive the ASTs).
pub fn retained_scope_bytes(cx: Context) -> usize {
    cx.lazy_functions.retained_scope_bytes()
}

/// Run a full (non-growing) garbage collection.
///
/// Does nothing when called while a GC is already running (from a GC hook such as
/// `Host::finalize_host_object`) or while host objects are finalized at context teardown: that GC
/// collects, and a nested one would free live host objects.
pub fn gc(cx: Context) {
    Heap::run_gc(cx, GcType::Normal);
}

/// A shrinking collection (SpiderMonkey's GCOptions::Shrink): collect, then hand the free semispace back to the system whatever
/// the collection frequency (`gc::accounting::should_release`).
pub fn gc_shrinking(mut cx: Context) {
    Heap::run_gc(cx, GcType::Normal);
    if cx.options.release_free_semispace
        && cx.options.gc_stress_window.is_none()
        && crate::runtime::gc::accounting::should_release(cx.heap.gc_stats().high_frequency, true)
    {
        cx.heap.release_next_semispace();
    }
}

/// Turn a value into an error result, e.g. to throw from a native.
pub fn throw_value(value: Handle<Value>) -> EvalError {
    EvalError::new_value(value)
}

/// An uncatchable termination, e.g. for a native that fails without an exception (SpiderMonkey's
/// `return false` with nothing pending). Returned from `Host::call_native` or `Host::interrupt`, it
/// unwinds every JS frame without running `catch` or `finally` blocks, and the outermost evaluate,
/// call or construct of the host fails with it.
pub fn uncatchable() -> EvalError {
    EvalError::Uncatchable
}

/// Whether a failed evaluation was an uncatchable termination.
pub fn is_uncatchable(error: &EvalError) -> bool {
    matches!(error, EvalError::Uncatchable)
}

/// How an evaluation failed.
#[derive(Clone, Copy)]
pub enum Failure {
    /// A value was thrown and not caught.
    Thrown(Handle<Value>),
    /// The evaluation was terminated uncatchably. There is no exception value.
    Uncatchable,
}

/// Classify a failed evaluation. Panics on an allocation error (only possible with the
/// `alloc_error` feature).
pub fn failure(error: EvalError) -> Failure {
    match error {
        EvalError::Value(value) => Failure::Thrown(value),
        EvalError::Uncatchable => Failure::Uncatchable,
        #[cfg(feature = "alloc_error")]
        EvalError::Alloc(_) => panic!("allocation error"),
    }
}

/// The thrown value of a failed evaluation, or None for an uncatchable termination.
pub fn exception_value_opt(error: EvalError) -> Option<Handle<Value>> {
    match failure(error) {
        Failure::Thrown(value) => Some(value),
        Failure::Uncatchable => None,
    }
}

/// The thrown value of a failed evaluation. Panics on an uncatchable termination (use
/// `exception_value_opt` or `failure` where that is possible) and on an allocation error.
pub fn exception_value(error: EvalError) -> Handle<Value> {
    exception_value_opt(error).expect("uncatchable termination has no exception value")
}

/// Evaluate a script in a realm and return its completion value. `line` is the line number of the
/// first line of `source` (1-based).
pub fn evaluate_script(
    cx: Context,
    realm: Handle<Realm>,
    source: &str,
    file_name: &str,
    line: u32,
) -> EvalResult<Handle<Value>> {
    let script = compile_script(cx, realm, source, file_name, line)?;
    execute_compiled(cx, script)
}

/// Compile a function from its parameter names and body in a realm's global scope. `line` is the
/// line number of the first line of `body`.
pub fn compile_function(
    cx: Context,
    realm: Handle<Realm>,
    name: Option<&str>,
    params: &[&str],
    body: &str,
    file_name: &str,
    line: u32,
) -> EvalResult<Handle<ObjectValue>> {
    let text = format!("(function({}) {{\n{}\n}})", params.join(", "), body);
    let function = evaluate_script(cx, realm, &text, file_name, line.saturating_sub(1).max(1))?;

    if !function.is_object() {
        return type_error(cx, "function compilation did not produce a function");
    }

    let function = function.as_object();

    if let Some(name) = name {
        let mut cx = cx;
        let name = cx.alloc_string(name)?;
        let name_key = PropertyKey::string_handle(cx, name)?;
        set_function_name(cx, function, name_key, None)?;
    }

    Ok(function)
}

/// Call a value with a receiver and arguments. Enters the initial realm if no realm is active.
pub fn call(
    cx: Context,
    function: Handle<Value>,
    this: Handle<Value>,
    arguments: &[Handle<Value>],
) -> EvalResult<Handle<Value>> {
    if !function.is_object() {
        return type_error(cx, "value is not a function");
    }

    with_any_realm(cx, |cx| call_object(cx, function.as_object(), this, arguments))
}

/// Construct a value with arguments and an optional new.target. Enters the initial realm if no
/// realm is active.
pub fn construct(
    cx: Context,
    function: Handle<Value>,
    arguments: &[Handle<Value>],
    new_target: Option<Handle<ObjectValue>>,
) -> EvalResult<Handle<ObjectValue>> {
    if !function.is_object() || !function.as_object().is_constructor() {
        return type_error(cx, "value is not a constructor");
    }

    with_any_realm(cx, |cx| construct_object(cx, function.as_object(), arguments, new_target))
}

/// Run every task left in the engine's own queue (tasks the host refused, or all tasks when no
/// host is installed).
pub fn run_engine_tasks(mut cx: Context) -> EvalResult<()> {
    cx.run_all_tasks()
}

/// Number of jobs handed to the host that have not run yet.
pub fn pending_host_jobs(cx: Context) -> usize {
    state(cx).jobs.len()
}

/// Number of objects in the host space (live, or dead but not yet swept).
pub fn host_object_count(cx: Context) -> usize {
    state(cx).space.num_objects()
}

/// The Function.prototype intrinsic of a realm, a common prototype for callable host objects.
pub fn function_prototype(realm: Handle<Realm>) -> Handle<ObjectValue> {
    realm.get_intrinsic(Intrinsic::FunctionPrototype)
}

/// The Object.prototype intrinsic of a realm.
pub fn object_prototype(realm: Handle<Realm>) -> Handle<ObjectValue> {
    realm.get_intrinsic(Intrinsic::ObjectPrototype)
}

// GC integration. Only called by the garbage collector when a host is installed.

/// Names the host hook the collector is inside, for the message of the abort that rejects an
/// allocation on the moving heap during a collection (`Heap::abort_alloc_during_gc`). Labels nest:
/// a hook that re-enters the collector sees the inner name, then restores the outer one.
struct GcHookLabel {
    cx: Context,
    prev: &'static str,
}

impl GcHookLabel {
    fn new(mut cx: Context, hook: &'static str) -> Self {
        let prev = cx.gc_alloc_hook;
        cx.gc_alloc_hook = hook;
        Self { cx, prev }
    }
}

impl Drop for GcHookLabel {
    fn drop(&mut self) {
        self.cx.gc_alloc_hook = self.prev;
    }
}

/// Visit the roots owned by the embedding state and the host's roots.
pub(crate) fn visit_roots_for_gc(cx: Context, gc: &mut GarbageCollector) {
    state(cx).visit_roots(gc);
    let _label = GcHookLabel::new(cx, "trace_roots");
    host(cx).trace_roots(GcHookCx::new(cx), &mut GcRootVisitor { gc });
}

/// Report the fields of a marked host object through the host's trace hook.
pub(crate) fn trace_host_object(
    cx: Context,
    object: HeapPtr<AnyHeapItem>,
    gc: &mut GarbageCollector,
) {
    debug_assert!(!is_engine_cell(object.cast()), "engine cells are traced by trace_engine_cell");
    // SAFETY: The GC only traces objects from its host gray stack, which are host space objects.
    let object = unsafe { HostObjectRef::from_host_space_object(object) };
    let _label = GcHookLabel::new(cx, "trace_host_object");
    host(cx).trace_host_object(GcHookCx::new(cx), object, &mut GcRootVisitor { gc });
}

/// Run the host's weak pass, then finalize and free every unmarked host object. Must be called
/// after liveness is final and before the from-space is released.
pub(crate) fn sweep_after_gc(cx: Context, gc: &mut GarbageCollector) {
    {
        let _label = GcHookLabel::new(cx, "after_gc_weak");
        host(cx).after_gc_weak(GcHookCx::new(cx), &mut GcWeakUpdater { gc });
    }

    // The state is re-borrowed for each use: the finalizer may re-enter the engine and take the
    // state again, so no borrow of it (or of a dead header) may be live across that call.
    let dead = state(cx).space.take_unmarked();

    for header in &dead {
        let (flags, object) = {
            // SAFETY: Dead headers are still allocated until `free` is called below. The
            // reference ends before the host is called.
            let header_ref = unsafe { &*header.as_ptr() };
            (header_ref.flags(), header_ref.object())
        };

        if flags.contains(HostFlags::INTERNAL_JOB) {
            // The host dropped a job without running it
            state(cx).jobs.remove(&(object.as_ptr() as usize));
        } else if flags.contains(HostFlags::INTERNAL_CELL) {
            // An engine cell: its own finalize hook, never the host's
            finalize_engine_cell(cx, object);
        } else if flags.contains(HostFlags::HAS_FINALIZE) {
            // SAFETY: `object` is a host space object, still allocated until `free` below.
            let object = unsafe { HostObjectRef::from_host_space_object(object) };
            let _label = GcHookLabel::new(cx, "finalize_host_object");
            host(cx).finalize_host_object(GcHookCx::new(cx), object);
        }
    }

    state(cx).space.free(dead);
}

/// Called at the very end of a GC.
pub(crate) fn after_gc(cx: Context) {
    state(cx).in_gc = false;

    // Tasks enqueued during the GC (e.g. FinalizationRegistry cleanups) can now be wrapped
    jobs::hand_tasks_to_host(cx);
}

/// Finalize every host object and drop the embedding state. Called when the context is dropped.
pub(crate) fn teardown(mut cx: Context) {
    let Some(state) = try_state(cx) else {
        return;
    };

    // Dropping the context needs its owner, which no frame inside a call using an engine cell can
    // be: a cell call root left at teardown is a host misuse (design §2.4). Finalizing the cell
    // under that frame would free what it uses, so this is fatal.
    if !state.cell_call_roots.is_empty() {
        eprintln!(
            "brimstone: context destroyed while {} call(s) using engine cells are on the stack",
            state.cell_call_roots.len()
        );
        std::process::abort();
    }

    // The borrow of the state ends here, before the host is called
    let headers = state.space.take_all();

    // A finalizer that asks for a GC must not get one: the GC would trace and weakly update host
    // objects that were already finalized.
    cx.gc_in_progress = true;

    for header in headers {
        let (flags, object) = {
            // SAFETY: The host space is still allocated, it is dropped with the state below. The
            // reference ends before the host is called.
            let header_ref = unsafe { &*header.as_ptr() };
            (header_ref.flags(), header_ref.object())
        };
        if flags.contains(HostFlags::INTERNAL_CELL) {
            finalize_engine_cell(cx, object);
        } else if flags.contains(HostFlags::HAS_FINALIZE)
            && !flags.contains(HostFlags::INTERNAL_JOB)
        {
            // SAFETY: The object is a host space object; the space is dropped with the state below.
            let object = unsafe { HostObjectRef::from_host_space_object(object) };
            let _label = GcHookLabel::new(cx, "finalize_host_object");
            host(cx).finalize_host_object(GcHookCx::new(cx), object);
        }
    }

    cx.embed = None;
    cx.gc_in_progress = false;
}

/// Adapter from the host's root visitor interface to the garbage collector.
struct GcRootVisitor<'a> {
    gc: &'a mut GarbageCollector,
}

impl RootVisitor for GcRootVisitor<'_> {
    fn value(&mut self, bits: &mut u64) {
        if *bits == 0 {
            return;
        }

        let mut value = Value::from_raw_bits(*bits);
        self.gc.visit_value(&mut value);
        *bits = value.as_raw_bits();
    }

    fn cell(&mut self, ptr: &mut *mut c_void) {
        if ptr.is_null() {
            return;
        }

        let mut heap_ptr = HeapPtr::<AnyHeapItem>::from_ptr(ptr.cast());
        self.gc.visit(&mut heap_ptr);
        *ptr = heap_ptr.as_ptr().cast();
    }
}

/// Adapter from the host's weak updater interface to the garbage collector.
struct GcWeakUpdater<'a> {
    gc: &'a mut GarbageCollector,
}

impl WeakUpdater for GcWeakUpdater<'_> {
    fn update(&mut self, ptr: &mut *mut c_void) {
        if ptr.is_null() {
            return;
        }

        *ptr = match self.gc.resolve_weak_pointer(HeapPtr::from_ptr(ptr.cast())) {
            Some(new_location) => new_location.as_ptr().cast(),
            None => std::ptr::null_mut(),
        };
    }

    fn update_value(&mut self, bits: &mut u64) {
        if *bits == 0 {
            return;
        }

        let value = Value::from_raw_bits(*bits);
        if !value.is_pointer() {
            return;
        }

        *bits = match self.gc.resolve_weak_pointer(value.as_pointer()) {
            Some(new_location) => Value::heap_item(new_location).as_raw_bits(),
            None => Value::undefined().as_raw_bits(),
        };
    }
}

/// Look up a property on a resolve-hook host object after an own property miss. Return whether
/// the hook defined a property (the caller then retries its lookup once).
pub(crate) fn resolve_own_property_miss(
    cx: Context,
    object: Handle<ObjectValue>,
    key: Handle<PropertyKey>,
) -> EvalResult<bool> {
    if cx.embed.is_none() {
        return Ok(false);
    }

    match host_header(*object) {
        Some(header) if header.flags().contains(HostFlags::HAS_RESOLVE) => {
            call_resolve_hook(cx, object, key)
        }
        _ => Ok(false),
    }
}

/// Call the host's resolve hook unless it is already running for this object and key.
pub(crate) fn call_resolve_hook(
    cx: Context,
    object: Handle<ObjectValue>,
    key: Handle<PropertyKey>,
) -> EvalResult<bool> {
    let address = object.as_ptr() as usize;
    let is_resolving = state(cx)
        .resolving
        .iter()
        .any(|(resolving_address, resolving_key)| {
            *resolving_address == address && **resolving_key == *key
        });
    if is_resolving {
        return Ok(false);
    }

    state(cx).resolving.push((address, key));
    let result = host(cx).resolve(cx, object, key);
    state(cx).resolving.pop();

    result
}

/// Set the directory of the on-disk structural function cache. `None` disables the disk store,
/// leaving only the in-memory cache. Existing files in the directory are indexed lazily.
pub fn set_function_cache_dir(mut cx: Context, dir: Option<std::path::PathBuf>) {
    cx.fn_cache.set_dir(dir);
}

/// Set the maximum size in bytes of the on-disk function cache. The bound is enforced by LRU
/// eviction on last use. Defaults to 64 MB.
pub fn set_function_cache_max_disk_bytes(mut cx: Context, max_bytes: u64) {
    cx.fn_cache.set_max_disk_bytes(max_bytes);
}

/// Enable or disable the structural function cache. When disabled no keys are computed and nothing
/// is stored or looked up. Disabled by default.
pub fn set_function_cache_enabled(mut cx: Context, enabled: bool) {
    cx.fn_cache.enabled = enabled;
}

/// Share the function cache's templates with the other contexts of the process that did the same.
/// For embedders that run many short-lived contexts.
pub fn set_function_cache_shared(mut cx: Context, shared: bool) {
    cx.fn_cache.shared = shared;
}

/// Enable or disable verify mode: on every cache hit the function is also generated normally and
/// the two results are compared in full. A mismatch is counted in the stats, reported to stderr,
/// and the generated function is used. Disabled by default.
pub fn set_function_cache_verify(mut cx: Context, verify: bool) {
    cx.fn_cache.verify = verify;
}

/// Statistics of the structural function cache.
pub fn function_cache_stats(cx: Context) -> crate::runtime::bytecode::fncache::FunctionCacheStats {
    cx.fn_cache.stats.clone()
}

/// Set the function cache partition of a realm: the top-level site (scheme and registrable
/// domain) of the page the realm belongs to. Functions are cached and reused only between realms
/// with the same partition, in memory and on disk, so one site cannot tell from timing what
/// another has loaded. A realm without a partition (the default, or an empty `partition`) gets no
/// function cache at all.
pub fn set_realm_cache_partition(mut cx: Context, mut realm: Handle<Realm>, partition: &[u8]) {
    let id = cx.fn_cache.partition_id(partition);
    realm.set_cache_partition(id);
}

/// Block until all queued writes to the on-disk function cache have been performed.
pub fn function_cache_flush(cx: Context) {
    cx.fn_cache.flush();
}

/// One kind of heap item in a census.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CensusEntry {
    pub kind: String,
    pub count: usize,
    pub bytes: usize,
}

/// The live heap items by kind after the last collection, largest first. Empty unless the context's
/// options enable the census (`OptionsBuilder::gc_census`) and a collection has run.
pub fn heap_census(cx: Context) -> Vec<CensusEntry> {
    let mut entries: Vec<CensusEntry> = cx
        .heap
        .census()
        .into_iter()
        .map(|(kind, count, bytes)| CensusEntry { kind, count, bytes })
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.bytes));
    entries
}

/// Heap verification (`gc::verify`): the embedding state's roots and the host's roots.
pub(crate) fn visit_roots_for_verify<V: HeapVisitor>(
    cx: Context,
    visitor: &mut V,
    mut label: impl FnMut(&mut V, &'static str),
) {
    if try_state(cx).is_none() {
        return;
    }
    label(visitor, "embedding state root (realm stack, jobs, native frames)");
    state(cx).visit_roots(visitor);
    label(visitor, "host root (Servo's roots)");
    let _label = GcHookLabel::new(cx, "trace_roots");
    host(cx).trace_roots(GcHookCx::new(cx), &mut AnyRootVisitor { visitor });
}

/// Heap verification: the headers of every host object not yet freed (a copy, so no borrow of the
/// state is held across calls into the host).
pub(crate) fn host_headers_for_verify(cx: Context) -> Vec<std::ptr::NonNull<HostHeader>> {
    match try_state(cx) {
        Some(state) => state.space.headers().to_vec(),
        None => vec![],
    }
}

/// Heap verification: whether an address is in the host space.
pub(crate) fn host_space_contains(cx: Context, ptr: usize) -> bool {
    try_state(cx).is_some_and(|state| state.space.contains(ptr as *const u8))
}

/// Heap verification: a host object's fields through the host's trace hook.
pub(crate) fn trace_host_object_for_verify(
    cx: Context,
    object: HeapPtr<AnyHeapItem>,
    visitor: &mut impl HeapVisitor,
) {
    // An engine cell (it has HAS_TRACE when it has a trace hook): its own hook, not the host's
    if is_engine_cell(object.cast()) {
        let hooks = state(cx)
            .cell_hooks
            .get(&(object.as_ptr() as usize))
            .copied();
        // The verifier walks only objects not yet freed, and an entry is removed only when the
        // cell is finalized, right before it is freed
        debug_assert!(hooks.is_some(), "live engine cell has no hook table entry");
        if let Some(trace) = hooks.and_then(|hooks| hooks.trace) {
            let _label = GcHookLabel::new(cx, "trace_engine_cell");
            trace(GcHookCx::new(cx), object.cast(), &mut AnyRootVisitor { visitor });
        }
        return;
    }

    // SAFETY: the verifier passes only objects of the host space's object list.
    let object = unsafe { HostObjectRef::from_host_space_object(object) };
    let _label = GcHookLabel::new(cx, "trace_host_object");
    host(cx).trace_host_object(GcHookCx::new(cx), object, &mut AnyRootVisitor { visitor });
}

/// Adapter from the host's root visitor interface to any heap visitor (verification only: slots
/// are read, never updated).
struct AnyRootVisitor<'a, V: HeapVisitor> {
    visitor: &'a mut V,
}

impl<V: HeapVisitor> RootVisitor for AnyRootVisitor<'_, V> {
    fn value(&mut self, bits: &mut u64) {
        if *bits == 0 {
            return;
        }
        let mut value = Value::from_raw_bits(*bits);
        self.visitor.visit_value(&mut value);
    }

    fn cell(&mut self, ptr: &mut *mut c_void) {
        if ptr.is_null() {
            return;
        }
        let mut heap_ptr = HeapPtr::<AnyHeapItem>::from_ptr(ptr.cast());
        self.visitor.visit(&mut heap_ptr);
    }
}

/// JS::AddAssociatedMemory: `n` bytes of embedder memory owned by a JS object (Servo: a DOM object's Rust size, canvas and
/// WebGL stores). Counted as SpiderMonkey's zone malloc counter counts them (gc/ZoneAllocator.h:70-81, jsapi.cpp:1281-1292): a
/// collection is due once the count reaches its threshold (`gc::accounting`).
pub fn add_associated_memory(mut cx: Context, n: usize) {
    if n == 0 {
        return;
    }
    let a = cx.heap.accounting_mut();
    a.associated_bytes = crate::runtime::gc::accounting::charge(a.associated_bytes, n);
    let due = crate::runtime::gc::accounting::due(a.associated_bytes, a.associated_threshold);
    if due && !cx.gc_in_progress {
        cx.heap.request_collection();
    }
}

/// JS::RemoveAssociatedMemory: the embedder freed `n` associated bytes (also from finalizers during a collection, which is
/// how the retained count follows the survivors, jsapi.cpp:1294-1304).
pub fn remove_associated_memory(mut cx: Context, n: usize) {
    let a = cx.heap.accounting_mut();
    a.associated_bytes = crate::runtime::gc::accounting::uncharge(a.associated_bytes, n);
}

/// SpiderMonkey's GC parameters (JSGCParamKey) that LucinaJS honours, in SpiderMonkey's units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GcParam {
    /// JSGC_MAX_BYTES, bytes.
    MaxBytes,
    /// JSGC_MALLOC_THRESHOLD_BASE, MiB.
    MallocThresholdBaseMb,
    /// JSGC_ALLOCATION_THRESHOLD, MiB.
    AllocationThresholdMb,
    /// JSGC_SMALL_HEAP_SIZE_MAX and JSGC_LARGE_HEAP_SIZE_MIN, MiB.
    SmallHeapSizeMaxMb,
    LargeHeapSizeMinMb,
    /// JSGC_HIGH_FREQUENCY_SMALL_HEAP_GROWTH, _LARGE_HEAP_GROWTH and JSGC_LOW_FREQUENCY_HEAP_GROWTH, percent (300 = 3.0).
    HighFrequencySmallHeapGrowth,
    HighFrequencyLargeHeapGrowth,
    LowFrequencyHeapGrowth,
    /// JSGC_HIGH_FREQUENCY_TIME_LIMIT, ms.
    HighFrequencyTimeLimitMs,
}

/// JS_SetGCParameter for the parameters LucinaJS honours. A value SpiderMonkey would refuse (a factor below 1.0 or above 10.0,
/// a small-heap size not below the large-heap size, sizes past 1 PiB) is ignored and false returned, as SpiderMonkey's
/// setParameter returns false; the thresholds take a new value from the next collection on.
pub fn set_gc_parameter(mut cx: Context, param: GcParam, value: u32) -> bool {
    use crate::runtime::gc::accounting::{MAX_SIZE, MAX_TENTHS};
    let mut t = cx.heap.accounting().tunables;
    let mb = |v: u32| (v as usize).checked_mul(1 << 20).filter(|b| *b <= MAX_SIZE);
    let tenths = |v: u32| Some(v as usize / 10).filter(|f| (10..=MAX_TENTHS).contains(f));
    let ok = match param {
        GcParam::MaxBytes => {
            t.max_bytes = if value == u32::MAX {
                usize::MAX
            } else {
                value as usize
            };
            true
        }
        GcParam::MallocThresholdBaseMb => mb(value).map(|b| t.malloc_base = b).is_some(),
        GcParam::AllocationThresholdMb => mb(value).map(|b| t.allocation_threshold = b).is_some(),
        GcParam::SmallHeapSizeMaxMb => mb(value)
            .filter(|b| *b < t.large_heap)
            .map(|b| t.small_heap = b)
            .is_some(),
        GcParam::LargeHeapSizeMinMb => mb(value)
            .filter(|b| *b > t.small_heap)
            .map(|b| t.large_heap = b)
            .is_some(),
        GcParam::HighFrequencySmallHeapGrowth => tenths(value)
            .filter(|f| *f >= t.high_large_tenths)
            .map(|f| t.high_small_tenths = f)
            .is_some(),
        GcParam::HighFrequencyLargeHeapGrowth => tenths(value)
            .filter(|f| *f <= t.high_small_tenths)
            .map(|f| t.high_large_tenths = f)
            .is_some(),
        GcParam::LowFrequencyHeapGrowth => tenths(value).map(|f| t.low_tenths = f).is_some(),
        GcParam::HighFrequencyTimeLimitMs => {
            t.high_frequency_ms = value as u64;
            true
        }
    };
    if ok {
        cx.heap.accounting_mut().tunables = t;
    }
    ok
}
