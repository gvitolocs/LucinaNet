/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `JSContext` (and `JSRuntime`, the same object here: one context per runtime, as Servo uses them): one Brimstone `Context` and
//! what SpiderMonkey keeps per context. The original is `js/src/vm/JSContext.cpp` and `jsapi.cpp` in mozjs_sys 153.6.0-1.

use ::std::cell::Cell;
use ::std::ffi::c_void;
use ::std::sync::atomic::{AtomicBool, Ordering};
use ::std::sync::{Arc, LazyLock, Mutex};

use brimstone_core::runtime::embed::{self, api, Host, HostObjectRef, RootVisitor, WeakUpdater};
use brimstone_core::runtime::eval_result::EvalResult;
use brimstone_core::runtime::{Context as Bs, ContextBuilder, Handle, PropertyKey, Value};

use super::gc;
use crate::boundary::roots::FrameStack;
use crate::jsapi::*;

thread_local! {
    static CURRENT: Cell<*mut Cx> = const { Cell::new(::std::ptr::null_mut()) };
}

/// How many values running natives may hold in their `vp` arrays at once (8 MiB). Deeper than any real stack: the native
/// stack quota stops recursion first. A full array throws "too much recursion" (`boundary::roots`).
pub const FRAME_VALUES: usize = 1 << 20;

#[repr(C)]
pub struct Cx {
    /// First, because mozjs casts `JSContext*` to `JS::RootingContext*`, as C++ does (`Rooted` links itself into `stackRoots_`).
    pub rooting: JS::RootingContext,
    pub bs: Bs,
    /// The pending exception, in value bits.
    pub exception: Option<u64>,
    pub frames: FrameStack,
    pub frame_values: gc::FrameValues,
    /// Values the engine itself keeps alive between calls (none are borrowed by Servo).
    pub extra_values: Vec<u64>,
    pub extra_roots: Vec<(JSTraceDataOp, *mut c_void)>,
    pub weak_zone_callbacks: Vec<(JSWeakPointerZonesCallback, *mut c_void)>,
    pub weak_compartment_callbacks: Vec<(JSWeakPointerCompartmentCallback, *mut c_void)>,
    pub gc_callback: Option<(JSGCCallback, *mut c_void)>,
    pub finalize_callbacks: Vec<(JSFinalizeCallback, *mut c_void)>,
    pub interrupt_callbacks: Vec<JSInterruptCallback>,
    /// What `JS_RequestInterruptCallback` sets from any thread; that thread reaches it through `INTERRUPTS`, never through
    /// the context.
    pub interrupt: Arc<Interrupt>,
    pub private: *mut c_void,
    pub native_stack_quota: usize,
    pub job_queue: Option<*mut JS::JobQueue>,
    /// Globals of the realms entered with `JS::EnterRealm`, innermost last.
    pub realm_stack: Vec<*mut c_void>,
    /// `JS::SetRealmPrivate`, by global address (globals never move).
    pub realm_private: ::std::collections::HashMap<usize, *mut c_void>,
    pub warning_reporter: JS::WarningReporter,
    pub security_callbacks: *const JSSecurityCallbacks,
    pub microtasks: super::jobs::MicroTasks,
    /// SpiderMonkey descriptors handed to proxy traps, rooted while the trap runs.
    pub desc_roots: Vec<*mut JS::PropertyDescriptor>,
    pub window_proxy_class: *const JSClass,
    /// `js::SetWindowProxy`: global address to its WindowProxy (both never move).
    pub window_proxies: ::std::collections::HashMap<usize, usize>,
    pub dom_callbacks: *const js::DOMCallbacks,
    pub wrap_callbacks: *const JSWrapObjectCallbacks,
    pub preserve_wrapper: js::PreserveWrapperCallback,
    pub has_released_wrapper: js::HasReleasedWrapperCallback,
    pub destroy_principals: JSDestroyPrincipalsOp,
    pub read_principals: JSReadPrincipalsOp,
    pub trusted_principals: *mut JSPrincipals,
    /// `JS::SetRealmPrincipals`, by global address.
    pub realm_principals: ::std::collections::HashMap<usize, *mut JSPrincipals>,
    pub hooks: super::runtime::Hooks,
    pub hidden_scripted_caller: u32,
    /// `JS::SetScriptPrivate`: (script, value) bits, both traced every GC.
    pub script_privates: Vec<(u64, u64)>,
    /// (script, the file it was compiled from), for the scripted caller of `engine/caller_private.rs`: the engine reports a
    /// running frame by file name and never tells which script it belongs to. Traced every GC like `script_privates`.
    pub script_files: Vec<(u64, String)>,
    /// What the glue is running innermost: `JS_ExecuteScript` pushes the script it was given, `JS::Evaluate` the file of the
    /// script it compiled for itself. Traced every GC like `script_privates`.
    pub running_scripts: Vec<super::caller_private::RunningScript>,
    /// The report `JS_ErrorFromException` handed out last (Servo reads it before the next call).
    pub last_error_report: Option<Box<super::errors::HeldReport>>,
}

/// The part of a context another thread may touch: Servo's hang monitor calls `JS_RequestInterruptCallback` while the
/// script thread runs, maybe inside a native with the context in use. It lives outside `Cx`, reached from that thread
/// through `INTERRUPTS` by the context's address, so no other thread reads `Cx` memory.
pub struct Interrupt {
    /// Set by `JS_RequestInterruptCallback`; the script thread takes it where it checks for interrupts.
    pub requested: AtomicBool,
    /// Brimstone's interrupt flag, so the running script stops at its next interrupt point.
    pub handle: embed::InterruptHandle,
}

/// Each live context's `Interrupt`, by the context's address: registered by `JS_NewContext`, removed by `JS_DestroyContext`.
static INTERRUPTS: LazyLock<Mutex<::std::collections::HashMap<usize, Arc<Interrupt>>>> = LazyLock::new(Default::default);

/// The `Cx` behind a `JSContext*`.
///
/// # Safety
/// `cx` came from `JS_NewContext` and has not been destroyed, and the returned reference is used as a short reborrow: for the
/// whole of `'a` no other reference to the context is used, and `'a` ends before any call that can re-enter the engine (JS, a
/// GC, a Servo callback or hook, a proxy trap), since those reach the context through their own pointer. Copy out what is
/// needed (`cx_of(cx).bs`) or reborrow again after such a call; use a raw `*mut Cx` where the context is needed across one.
#[inline]
pub unsafe fn cx_of<'a>(cx: *mut JSContext) -> &'a mut Cx {
    unsafe { &mut *cx.cast::<Cx>() }
}

/// This thread's context, for engine callbacks that only get a Brimstone `Context`.
///
/// # Safety
/// As `cx_of`: no other reference to the context is used for the whole of `'a`, and `'a` ends before anything that can
/// re-enter the engine. Code that needs the context across such a call takes `current_ptr()`.
#[inline]
pub unsafe fn current<'a>() -> &'a mut Cx {
    // SAFETY: set by JS_NewContext for the life of this thread's context, cleared by JS_DestroyContext; uniqueness is the
    // caller's.
    unsafe { &mut *current_ptr() }
}

/// This thread's context as a raw pointer, for code that calls back into Servo or the engine while it still needs the context
/// (proxy traps, hooks, natives): access it by short place expressions (`(*cx).field`), never through a reference that is live
/// across such a call.
#[inline]
pub fn current_ptr() -> *mut Cx {
    let p = CURRENT.with(Cell::get);
    assert!(!p.is_null(), "mozjs_sys (Rust): no JSContext on this thread");
    p
}

#[inline]
pub fn current_raw() -> *mut JSContext {
    CURRENT.with(Cell::get).cast()
}

/// What Brimstone calls back into: the GC hooks and natives. Holds no state of its own; it all lives in `Cx`.
struct ServoHost {
    cx: *mut Cx,
}

/// SpiderMonkey's finalizers get a `GCContext`; Servo's never read it.
static mut GC_CONTEXT: ::std::mem::MaybeUninit<JS::GCContext> = ::std::mem::MaybeUninit::zeroed();

impl Host for ServoHost {
    fn code_for_eval(
        &self, bs: Bs, object: Handle<api::ObjectValue>,
    ) -> EvalResult<Option<Handle<api::StringValue>>> {
        unsafe { super::security::code_for_eval(self.cx, bs, object) }
    }

    fn can_compile_strings(
        &self,
        bs: Bs,
        kind: embed::CompileStringsKind,
        code: Handle<api::StringValue>,
        parameters: &[Handle<api::StringValue>],
        body: Option<Handle<api::StringValue>>,
        parameter_args: &[Handle<Value>],
        body_arg: Handle<Value>,
    ) -> EvalResult<bool> {
        unsafe { super::security::can_compile_strings(
            self.cx, bs, kind, code, parameters, body, parameter_args, body_arg,
        ) }
    }

    fn trace_roots(&self, _cx: embed::GcHookCx<'_>, visitor: &mut dyn RootVisitor) {
        // SAFETY: the context outlives its Brimstone context; GCs run on its thread. Passed raw: a GC runs inside natives and
        // allocations whose callers still reach the context (see `func::call_native`).
        unsafe { gc::trace_all_roots(self.cx, visitor) }
    }

    fn trace_host_object(&self, _cx: embed::GcHookCx<'_>, object: HostObjectRef, visitor: &mut dyn RootVisitor) {
        let class = object.class() as *const JSClass;
        if !class.is_null() {
            unsafe { gc::trace_host_object(class, object.as_ptr().cast(), visitor) }
        }
        // Proxies: the handler's trace trap, as SpiderMonkey's ProxyObject::trace calls handler->trace.
        if let Some(handler) = embed::host_handler(object.object()).filter(|h| !h.is_null()) {
            unsafe { super::proxy::trace_proxy(handler, object.as_ptr().cast(), visitor) }
        }
    }

    fn finalize_host_object(&self, _cx: embed::GcHookCx<'_>, object: HostObjectRef) {
        // Proxies: the handler's finalize trap (ProxyObject::finalize), which drops Servo's DOM object.
        if let Some(handler) = embed::host_handler(object.object()).filter(|h| !h.is_null()) {
            unsafe { super::proxy::finalize_proxy(handler, ::std::ptr::addr_of_mut!(GC_CONTEXT).cast(), object.as_ptr().cast()) };
        }
        let class = object.class() as *const JSClass;
        if class.is_null() {
            return;
        }
        let ops = unsafe { (*class).cOps };
        if ops.is_null() {
            return;
        }
        if let Some(finalize) = unsafe { (*ops).finalize } {
            unsafe { finalize(::std::ptr::addr_of_mut!(GC_CONTEXT).cast(), object.as_ptr().cast()) };
        }
    }

    /// SpiderMonkey's interrupt: every callback runs; any false stops the script uncatchably (Servo's slow-script stop).
    /// The profiler (`profile.rs`) also requests interrupts; Servo's callbacks run only when Servo asked, as in
    /// `JS_CheckForInterrupt`.
    fn interrupt(&self, bs: Bs) -> bool {
        // A poisoned context stops its script uncatchably and calls nothing of Servo's (docs/edge-wrap.md section d).
        if super::poison::thread_poisoned().is_some() {
            return false;
        }
        if super::profile::sample_due() {
            super::profile::take_sample(bs);
        }
        // Short place accesses through the raw pointer: the interrupt comes inside running script, maybe inside a native, and
        // Servo's callbacks get the context.
        let interrupt: &Interrupt = unsafe { &(*self.cx).interrupt };
        if !interrupt.requested.swap(false, Ordering::SeqCst) {
            return true;
        }
        let raw: *mut JSContext = self.cx.cast();
        let mut go_on = true;
        let callbacks = unsafe { (*self.cx).interrupt_callbacks.clone() };
        for cb in callbacks {
            if let Some(cb) = cb {
                go_on &= unsafe { cb(raw) };
            }
        }
        go_on
    }

    /// `JSClassOps::resolve` on an own-property miss (flag `HAS_RESOLVE`, set by `host_flags_of`).
    fn resolve(&self, bs: Bs, object: Handle<embed::api::ObjectValue>, key: Handle<PropertyKey>) -> EvalResult<bool> {
        unsafe { super::object::call_class_resolve(self.cx, bs, object, key) }
    }

    fn after_gc_weak(&self, _cx: embed::GcHookCx<'_>, updater: &mut dyn WeakUpdater) {
        unsafe { gc::update_weak_pointers(self.cx, updater) }
    }

    fn enqueue_job(
        &self,
        bs: Bs,
        job: brimstone_core::runtime::gc::HeapPtr<brimstone_core::runtime::object_value::ObjectValue>,
        promise: Option<brimstone_core::runtime::gc::HeapPtr<brimstone_core::runtime::object_value::ObjectValue>>,
        incumbent_global: Option<brimstone_core::runtime::gc::HeapPtr<brimstone_core::runtime::object_value::ObjectValue>>,
    ) -> bool {
        unsafe { super::jobs::enqueue_job(self.cx, bs, job, promise, incumbent_global) }
    }

    fn call_native(&self, bs: Bs, native: *const c_void, call: &mut embed::NativeCall) -> brimstone_core::runtime::EvalResult<()> {
        unsafe { super::func::call_native(self.cx, bs, native, call) }
    }

    fn call_host_object(
        &self,
        bs: Bs,
        object: Handle<embed::api::ObjectValue>,
        this: Handle<Value>,
        arguments: &[Handle<Value>],
        new_target: Option<Handle<embed::api::ObjectValue>>,
    ) -> EvalResult<Handle<Value>> {
        match embed::host_handler(*object) {
            Some(h) if !h.is_null() => unsafe { super::proxy::call_host_object(bs, h, object, this, arguments, new_target) },
            _ => unsafe { super::object::call_class_hook(self.cx, bs, object, this, arguments, new_target) },
        }
    }

    fn proxy_get_own_property(
        &self,
        bs: Bs,
        handler: *const c_void,
        object: Handle<embed::api::ObjectValue>,
        key: Handle<PropertyKey>,
    ) -> EvalResult<Option<api::PropertyDescriptor>> {
        unsafe { super::proxy::get_own_property(bs, handler, object, key) }
    }

    fn proxy_define_property(
        &self,
        bs: Bs,
        handler: *const c_void,
        object: Handle<embed::api::ObjectValue>,
        key: Handle<PropertyKey>,
        desc: api::PropertyDescriptor,
    ) -> EvalResult<bool> {
        unsafe { super::proxy::define_property(bs, handler, object, key, desc) }
    }

    fn proxy_own_keys(
        &self,
        bs: Bs,
        handler: *const c_void,
        object: Handle<embed::api::ObjectValue>,
    ) -> EvalResult<Vec<Handle<Value>>> {
        unsafe { super::proxy::own_keys(bs, handler, object) }
    }

    fn proxy_delete(
        &self,
        bs: Bs,
        handler: *const c_void,
        object: Handle<embed::api::ObjectValue>,
        key: Handle<PropertyKey>,
    ) -> EvalResult<bool> {
        unsafe { super::proxy::delete(bs, handler, object, key) }
    }

    fn proxy_has(
        &self,
        bs: Bs,
        handler: *const c_void,
        object: Handle<embed::api::ObjectValue>,
        key: Handle<PropertyKey>,
    ) -> EvalResult<bool> {
        unsafe { super::proxy::has(bs, handler, object, key) }
    }

    fn proxy_get(
        &self,
        bs: Bs,
        handler: *const c_void,
        object: Handle<embed::api::ObjectValue>,
        key: Handle<PropertyKey>,
        receiver: Handle<Value>,
    ) -> EvalResult<Handle<Value>> {
        unsafe { super::proxy::get(bs, handler, object, key, receiver) }
    }

    fn proxy_set(
        &self,
        bs: Bs,
        handler: *const c_void,
        object: Handle<embed::api::ObjectValue>,
        key: Handle<PropertyKey>,
        value: Handle<Value>,
        receiver: Handle<Value>,
    ) -> EvalResult<bool> {
        unsafe { super::proxy::set(bs, handler, object, key, value, receiver) }
    }

    fn proxy_prevent_extensions(
        &self,
        bs: Bs,
        handler: *const c_void,
        object: Handle<embed::api::ObjectValue>,
    ) -> EvalResult<bool> {
        unsafe { super::proxy::prevent_extensions(bs, handler, object) }
    }

    fn proxy_is_extensible(
        &self,
        bs: Bs,
        handler: *const c_void,
        object: Handle<embed::api::ObjectValue>,
    ) -> EvalResult<bool> {
        unsafe { super::proxy::is_extensible(bs, handler, object) }
    }

    fn proxy_get_prototype(
        &self,
        bs: Bs,
        handler: *const c_void,
        object: Handle<embed::api::ObjectValue>,
    ) -> EvalResult<Option<Handle<embed::api::ObjectValue>>> {
        unsafe { super::proxy::get_prototype(bs, handler, object) }
    }

    fn proxy_set_prototype(
        &self,
        bs: Bs,
        handler: *const c_void,
        object: Handle<embed::api::ObjectValue>,
        prototype: Option<Handle<embed::api::ObjectValue>>,
    ) -> EvalResult<bool> {
        unsafe { super::proxy::set_prototype(bs, handler, object, prototype) }
    }

    // Modules (`module.rs`): Servo's module load and metadata hooks.
    fn resolve_module(
        &self,
        bs: Bs,
        _referrer: Option<Handle<embed::SourceTextModule>>,
        _referrer_file: Handle<embed::api::FlatString>,
        specifier: Handle<embed::api::FlatString>,
        _attributes: Option<Handle<embed::ImportAttributes>>,
    ) -> EvalResult<Option<Handle<embed::SourceTextModule>>> {
        super::module::resolve_module(bs, specifier)
    }

    fn dynamic_import(
        &self,
        bs: Bs,
        referrer: Option<Handle<embed::SourceTextModule>>,
        _referrer_file: Handle<embed::api::FlatString>,
        specifier: Handle<embed::api::FlatString>,
        attributes: Option<Handle<embed::ImportAttributes>>,
        import: embed::DynamicImport,
    ) -> EvalResult<bool> {
        super::module::dynamic_import(bs, referrer, specifier, attributes, import)
    }

    fn populate_import_meta(
        &self,
        bs: Bs,
        module: Handle<embed::SourceTextModule>,
        meta: Handle<embed::api::ObjectValue>,
    ) -> EvalResult<()> {
        super::module::populate_import_meta(bs, module, meta)
    }
}

/// Gated: a thread whose context was poisoned gets no new one (null); it is meant to unwind (docs/edge-wrap.md).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewContext(_maxbytes: u32, parent: *mut JSRuntime) -> *mut JSContext {
    super::poison::install_hook();
    super::crash_bt::maybe_install();
    if !parent.is_null() {
        // Worker runtimes: each gets its own context and heap, as with SpiderMonkey; nothing is shared with the parent.
    }
    // Engine options from the environment, for A/B runs:
    // - LUCINANET_JS_MIN_HEAP_MB: start with a semispace this large, so a page load runs with few or no collections;
    // - LUCINANET_JS_LAZY=0: generate every function's bytecode when its script is compiled (lazy generation keeps each
    //   script's parse tree alive for its uncalled functions: less compile time, more memory);
    // - LUCINANET_JS_GC_RELEASE=0: keep the free semispace's pages after each collection instead of handing them back to
    //   the operating system (madvise MADV_DONTNEED; on by default);
    // - LUCINANET_JS_CENSUS=1: count the live heap items by kind at each collection (the profile summary prints them);
    // - LUCINANET_JS_GC_LOG=1: one stderr line per collection (`jsgc ...`: pause, heap and live sizes, parse arenas kept),
    //   and a `jsreparse ...` summary when the context goes;
    // - LUCINANET_JS_REPARSE=0: keep each script's AST for its lazy functions instead of dropping it once the script has
    //   run (then a lazy function parses its own text again on its first call, as SpiderMonkey does); `verify` keeps the
    //   AST and generates every re-parsed function from both, reporting differences (`lazy reparse: verify mismatch`);
    // - LUCINANET_JS_DUMP_FAILED=<dir>: write the source of every script that fails with a SyntaxError, with what the glue
    //   knows of it beside it (`dump_failed`); unset keeps it off.
    super::dump_failed::configure();
    let mut builder = ContextBuilder::new();
    let min_heap_mb = ::std::env::var("LUCINANET_JS_MIN_HEAP_MB").ok().and_then(|v| v.parse::<usize>().ok());
    // LUCINANET_JS_MAX_HEAP_MB: cap the heap (default 1 GiB), so heap exhaustion can be reached quickly (the edge wrap's
    // out-of-memory test, docs/edge-wrap.md).
    let max_heap_mb = ::std::env::var("LUCINANET_JS_MAX_HEAP_MB").ok().and_then(|v| v.parse::<usize>().ok());
    let lazy_off = ::std::env::var("LUCINANET_JS_LAZY").is_ok_and(|v| v == "0");
    let release_off = ::std::env::var("LUCINANET_JS_GC_RELEASE").is_ok_and(|v| v == "0");
    let census_on = ::std::env::var("LUCINANET_JS_CENSUS").is_ok_and(|v| v == "1");
    let gc_log_on = ::std::env::var("LUCINANET_JS_GC_LOG").is_ok_and(|v| v == "1");
    // LUCINANET_JS_MIN_WINDOW_MB: the smallest allocation window, when it should not be the semispace of the minimum heap
    // (with a large LUCINANET_JS_MIN_HEAP_MB: address space reserved, a small window, no copies to grow the heap).
    let min_window_mb = ::std::env::var("LUCINANET_JS_MIN_WINDOW_MB").ok().and_then(|v| v.parse::<usize>().ok());
    // LUCINANET_JS_GC_STRESS_KB=N: crash hunts. Collect after every N KiB allocated and poison the from-space
    // (0x01), so a pointer that missed a move reads garbage. Slow.
    let stress_kb = ::std::env::var("LUCINANET_JS_GC_STRESS_KB").ok().and_then(|v| v.parse::<usize>().ok());
    // LUCINANET_JS_GC_VERIFY=1: crash hunts. Check every pointer of the heap before and after each collection; abort with
    // a report (holder kind, field offset) on a bad one. Slow.
    let verify_on = ::std::env::var("LUCINANET_JS_GC_VERIFY").is_ok_and(|v| v == "1");
    let reparse = ::std::env::var("LUCINANET_JS_REPARSE").ok();
    let reparse_off = reparse.as_deref() == Some("0");
    let reparse_verify = reparse.as_deref() == Some("verify");
    if min_heap_mb.is_some()
        || max_heap_mb.is_some()
        || min_window_mb.is_some()
        || lazy_off
        || release_off
        || census_on
        || gc_log_on
        || stress_kb.is_some()
        || verify_on
        || reparse_off
        || reparse_verify
    {
        let mut options = brimstone_core::common::options::OptionsBuilder::new();
        if let Some(mb) = min_heap_mb {
            let bytes = mb.next_power_of_two().clamp(4, 4096) << 20;
            options = options.min_heap_size(bytes).max_heap_size(bytes.max(1 << 30));
        }
        if let Some(mb) = max_heap_mb {
            // Below the default minimum (64 MiB) the minimum follows it down, or the options would be refused.
            let bytes = mb.next_power_of_two().clamp(4, 4096) << 20;
            let min = min_heap_mb.map_or(64 << 20, |m| m.next_power_of_two().clamp(4, 4096) << 20).min(bytes);
            options = options.min_heap_size(min).max_heap_size(bytes);
        }
        if lazy_off {
            options = options.lazy_functions(false);
        }
        if release_off {
            options = options.release_free_semispace(false);
        }
        if census_on {
            options = options.gc_census(true);
        }
        if gc_log_on {
            options = options.gc_log(true);
        }
        if let Some(mb) = min_window_mb {
            options = options.min_alloc_window(Some(mb << 20));
        }
        if let Some(kb) = stress_kb {
            options = options.gc_stress_window(Some(kb.max(1) << 10));
        }
        if verify_on {
            options = options.gc_verify(true);
        }
        if reparse_off {
            options = options.lazy_reparse(false);
        }
        if reparse_verify {
            options = options.lazy_reparse_verify(true);
        }
        if let Ok(options) = options.build() {
            builder = builder.set_options(::std::rc::Rc::new(options));
        }
    }
    let bs = match builder.build() {
        Ok(bs) => bs,
        Err(_) => return ::std::ptr::null_mut(),
    };
    configure_function_cache(bs);
    let (frames, frame_values) = gc::new_frame_values(FRAME_VALUES);
    let interrupt = Arc::new(Interrupt { requested: AtomicBool::new(false), handle: embed::interrupt_handle(bs) });
    let cx = Box::into_raw(Box::new(Cx {
        rooting: unsafe { ::std::mem::zeroed() },
        bs,
        exception: None,
        frames,
        frame_values,
        extra_values: Vec::new(),
        extra_roots: Vec::new(),
        weak_zone_callbacks: Vec::new(),
        weak_compartment_callbacks: Vec::new(),
        gc_callback: None,
        finalize_callbacks: Vec::new(),
        interrupt_callbacks: Vec::new(),
        interrupt: interrupt.clone(),
        private: ::std::ptr::null_mut(),
        native_stack_quota: 0,
        job_queue: None,
        realm_stack: Vec::new(),
        realm_private: ::std::collections::HashMap::new(),
        warning_reporter: None,
        security_callbacks: ::std::ptr::null(),
        microtasks: Default::default(),
        desc_roots: Vec::new(),
        window_proxy_class: ::std::ptr::null(),
        window_proxies: ::std::collections::HashMap::new(),
        dom_callbacks: ::std::ptr::null(),
        wrap_callbacks: ::std::ptr::null(),
        preserve_wrapper: None,
        has_released_wrapper: None,
        destroy_principals: None,
        read_principals: None,
        trusted_principals: ::std::ptr::null_mut(),
        realm_principals: ::std::collections::HashMap::new(),
        hooks: super::runtime::Hooks { interrupt_enabled: true, ..Default::default() },
        hidden_scripted_caller: 0,
        script_privates: Vec::new(),
        script_files: Vec::new(),
        running_scripts: Vec::new(),
        last_error_report: None,
    }));
    if embed::install_host(bs, Box::new(ServoHost { cx })).is_err() {
        unsafe { drop(Box::from_raw(cx)) };
        bs.drop();
        return ::std::ptr::null_mut();
    }
    // `set_wasm_enabled` is the engine's host state, so the option can only be copied once the host exists.
    unsafe { sync_wasm_enabled(cx) };
    INTERRUPTS.lock().unwrap_or_else(|e| e.into_inner()).insert(cx as usize, interrupt);
    super::profile::start(embed::interrupt_handle(bs));
    CURRENT.with(|c| c.set(cx));
    cx.cast()
}

/// `JS::ContextOptions::wasm` is the only source of truth for the WebAssembly namespace (design §2.1); an option that was never
/// written is false. The engine's switch decides it for the realms created from now on, and Servo writes the option after
/// `JS_NewContext` returns and before its first global, so this runs at both points.
pub(super) unsafe fn sync_wasm_enabled(cx: *mut Cx) {
    // SAFETY: `cx` is a context this glue made and has not freed — the caller holds it as its own `*mut JSContext` — and an option
    // box it points at is the one `JS__ContextOptionsRef` handed out and never moved.
    let enabled = unsafe { (*cx).hooks.context_options.as_deref() }.is_some_and(|o| o.wasm_());
    // SAFETY: as above; `bs` is the engine context the `Cx` owns for its whole life, so it is valid whenever the `Cx` is.
    embed::set_wasm_enabled(unsafe { (*cx).bs }, enabled);
}

/// The structural per-function bytecode cache (fork `bytecode/fncache`), from the environment:
/// - LUCINANET_JS_FN_CACHE_DIR=<dir>: turn the cache on with its disk store in <dir> (LucinaNet sets <profile>/js-fn-cache);
/// - LUCINANET_JS_FN_CACHE_MB=<n>: the disk store's size cap (default 64 MB);
/// - LUCINANET_JS_FN_CACHE=0: keep it off even when a directory is set (A/B runs).
/// Debug builds also run the verify mode: every hit is generated again and compared.
fn configure_function_cache(bs: brimstone_core::runtime::Context) {
    let Some(dir) = ::std::env::var_os("LUCINANET_JS_FN_CACHE_DIR").filter(|d| !d.is_empty()) else {
        return;
    };
    if ::std::env::var("LUCINANET_JS_FN_CACHE").is_ok_and(|v| v == "0") {
        return;
    }
    let mb = ::std::env::var("LUCINANET_JS_FN_CACHE_MB").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(64);
    embed::set_function_cache_enabled(bs, true);
    embed::set_function_cache_dir(bs, Some(::std::path::PathBuf::from(dir)));
    embed::set_function_cache_max_disk_bytes(bs, mb.saturating_mul(1 << 20));
    embed::set_function_cache_verify(bs, cfg!(debug_assertions));
}

/// Gated: a poisoned context is leaked, not torn down. Its heap may be half moved or mid-allocation and its handle scopes
/// unbalanced; running its finalizers would hand that state to Servo (docs/embedding.md:511-520 in the engine: "the
/// context must not be used afterwards"). One leaked heap per crashed tab.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DestroyContext(cx: *mut JSContext) {
    let raw = cx.cast::<Cx>();
    let bs = unsafe { (*raw).bs };
    // Queued writes of the function cache reach the disk before the context goes.
    embed::function_cache_flush(bs);
    if bs.options.gc_log {
        let r = embed::reparse_stats(bs);
        eprintln!(
            "jsreparse detached={} generated_before_drop={} programs_dropped={} reparsed={} reparse_ms={:.1} failed={} verify_mismatches={}",
            r.detached,
            r.generated_before_drop,
            r.programs_dropped,
            r.reparsed,
            r.reparse_ns as f64 / 1e6,
            r.reparse_failed,
            r.verify_mismatches
        );
    }
    INTERRUPTS.lock().unwrap_or_else(|e| e.into_inner()).remove(&(raw as usize));
    super::profile::stop();
    // Brimstone finalizes the remaining host objects as it goes; their finalizers may still look at the context.
    bs.drop();
    CURRENT.with(|c| {
        if c.get() == raw {
            c.set(::std::ptr::null_mut())
        }
    });
    unsafe { drop(Box::from_raw(raw)) };
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__JS_Init() -> bool {
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ShutDown() {}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetRuntime(cx: *mut JSContext) -> *mut JSRuntime {
    cx.cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetContextPrivate(cx: *mut JSContext) -> *mut c_void {
    unsafe { cx_of(cx) }.private
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetContextPrivate(cx: *mut JSContext, data: *mut c_void) {
    unsafe { cx_of(cx) }.private = data;
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetNativeStackQuota(
    cx: *mut JSContext,
    system: JS::NativeStackSize,
    trusted: JS::NativeStackSize,
    untrusted: JS::NativeStackSize,
) {
    let bs = unsafe { cx_of(cx) }.bs;
    unsafe { cx_of(cx) }.native_stack_quota = system;
    // Brimstone has one native limit, so use the most restrictive enabled quota.
    // VM slots and the Rust/native stack are different resources: guard both.
    let quota = [system, trusted, untrusted].into_iter().filter(|n| *n != 0).min().unwrap_or(0);
    embed::set_stack_limit(bs, if quota == 0 { usize::MAX } else { quota });
    embed::set_native_stack_quota(bs, quota);
}

/// SpiderMonkey's GC parameters: the ones LucinaJS has a rule for (heap maximum, the accounting triggers' bases, growth factors
/// and heap-size limits, the high-frequency time; `embed::set_gc_parameter`) take effect; the rest (incremental and per-zone GC,
/// slices, nursery) have no counterpart in a non-incremental copying heap. All are recorded for `JS_GetGCParameter`.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetGCParameter(cx: *mut JSContext, key: JSGCParamKey, value: u32) {
    use embed::GcParam;
    unsafe { cx_of(cx) }.hooks.gc_params.insert(key as u32, value);
    let param = match key {
        JSGCParamKey::JSGC_MAX_BYTES => Some(GcParam::MaxBytes),
        JSGCParamKey::JSGC_MALLOC_THRESHOLD_BASE => Some(GcParam::MallocThresholdBaseMb),
        JSGCParamKey::JSGC_ALLOCATION_THRESHOLD => Some(GcParam::AllocationThresholdMb),
        JSGCParamKey::JSGC_SMALL_HEAP_SIZE_MAX => Some(GcParam::SmallHeapSizeMaxMb),
        JSGCParamKey::JSGC_LARGE_HEAP_SIZE_MIN => Some(GcParam::LargeHeapSizeMinMb),
        JSGCParamKey::JSGC_HIGH_FREQUENCY_SMALL_HEAP_GROWTH => Some(GcParam::HighFrequencySmallHeapGrowth),
        JSGCParamKey::JSGC_HIGH_FREQUENCY_LARGE_HEAP_GROWTH => Some(GcParam::HighFrequencyLargeHeapGrowth),
        JSGCParamKey::JSGC_LOW_FREQUENCY_HEAP_GROWTH => Some(GcParam::LowFrequencyHeapGrowth),
        JSGCParamKey::JSGC_HIGH_FREQUENCY_TIME_LIMIT => Some(GcParam::HighFrequencyTimeLimitMs),
        _ => None,
    };
    if let Some(param) = param {
        let bs = unsafe { cx_of(cx) }.bs;
        embed::set_gc_parameter(bs, param, value);
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AddExtraGCRootsTracer(cx: *mut JSContext, op: JSTraceDataOp, data: *mut c_void) -> bool {
    unsafe { cx_of(cx) }.extra_roots.push((op, data));
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_RemoveExtraGCRootsTracer(cx: *mut JSContext, op: JSTraceDataOp, data: *mut c_void) {
    let roots = &mut unsafe { cx_of(cx) }.extra_roots;
    if let Some(i) = roots.iter().position(|&(o, d)| o.map(|f| f as usize) == op.map(|f| f as usize) && d == data) {
        roots.remove(i);
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GC(cx: *mut JSContext, _reason: JS::GCReason) {
    let bs = unsafe { cx_of(cx) }.bs;
    if let Some((Some(cb), data)) = unsafe { cx_of(cx) }.gc_callback {
        unsafe { cb(cx, JSGCStatus::JSGC_BEGIN, JS::GCReason::API, data) };
    }
    embed::gc(bs);
    for (cb, data) in unsafe { cx_of(cx) }.finalize_callbacks.clone() {
        if let Some(cb) = cb {
            unsafe { cb(::std::ptr::addr_of_mut!(GC_CONTEXT).cast(), JSFinalizeStatus::JSFINALIZE_COLLECTION_END, data) };
        }
    }
    if let Some((Some(cb), data)) = unsafe { cx_of(cx) }.gc_callback {
        unsafe { cb(cx, JSGCStatus::JSGC_END, JS::GCReason::API, data) };
    }
}

/// Brimstone collects when its heap is full; there is nothing to do between.
/// Servo calls this when a window is suspended (navigated away): a good time to give back the parse trees lazy stubs keep
/// (`lazy_trim`), in one bounded slice.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_MaybeGC(cx: *mut JSContext) {
    let _ = unsafe { super::lazy_trim::idle_trim(cx, ::std::time::Duration::from_millis(50)) };
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetGCCallback(cx: *mut JSContext, cb: JSGCCallback, data: *mut c_void) {
    unsafe { cx_of(cx) }.gc_callback = Some((cb, data));
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AddFinalizeCallback(cx: *mut JSContext, cb: JSFinalizeCallback, data: *mut c_void) -> bool {
    unsafe { cx_of(cx) }.finalize_callbacks.push((cb, data));
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AddWeakPointerZonesCallback(cx: *mut JSContext, cb: JSWeakPointerZonesCallback, data: *mut c_void) -> bool {
    unsafe { cx_of(cx) }.weak_zone_callbacks.push((cb, data));
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AddWeakPointerCompartmentCallback(
    cx: *mut JSContext,
    cb: JSWeakPointerCompartmentCallback,
    data: *mut c_void,
) -> bool {
    unsafe { cx_of(cx) }.weak_compartment_callbacks.push((cb, data));
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AddInterruptCallback(cx: *mut JSContext, cb: JSInterruptCallback) -> bool {
    unsafe { cx_of(cx) }.interrupt_callbacks.push(cb);
    true
}

/// May be called from any thread (Servo's hang monitor): it touches only the context's `Interrupt`, found by address, never
/// the `Cx` itself, which the script thread may be using.
#[super::poison::engine_shim(exempt = "other threads call it; the wrap itself calls it on a poisoned context")]
pub unsafe extern "C" fn JS_RequestInterruptCallback(cx: *mut JSContext) {
    let interrupt = INTERRUPTS.lock().unwrap_or_else(|e| e.into_inner()).get(&(cx as usize)).cloned();
    if let Some(i) = interrupt {
        i.requested.store(true, Ordering::SeqCst);
        i.handle.request();
    }
}

#[super::poison::engine_shim(exempt = "as JS_RequestInterruptCallback, which it calls")]
pub unsafe extern "C" fn JS_RequestInterruptCallbackCanWait(cx: *mut JSContext) {
    unsafe { JS_RequestInterruptCallback(cx) }
}

// ---- The pending exception ----

impl Cx {
    pub fn set_pending(&mut self, bits: u64) {
        self.exception = Some(bits);
    }

    pub fn take_pending(&mut self) -> Option<u64> {
        self.exception.take()
    }
}

/// Gated: a poisoned context has nothing pending (false), so a failed call reads as SpiderMonkey's uncatchable exception.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_IsExceptionPending(cx: *mut JSContext) -> bool {
    unsafe { cx_of(cx) }.exception.is_some()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetPendingException(cx: *mut JSContext, vp: JS::MutableHandleValue) -> bool {
    match unsafe { cx_of(cx) }.exception {
        Some(bits) => {
            unsafe { (*vp.ptr).asBits_ = bits };
            true
        },
        None => false,
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetPendingException(cx: *mut JSContext, v: JS::HandleValue, _behavior: JS::ExceptionStackBehavior) {
    let bits = unsafe { (*v.ptr).asBits_ };
    unsafe { cx_of(cx) }.set_pending(bits);
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ClearPendingException(cx: *mut JSContext) {
    unsafe { cx_of(cx) }.exception = None;
}

/// SpiderMonkey compiles its self-hosted JS here; Brimstone's builtins are Rust, so there is nothing to load.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__InitSelfHostedCode(
    _cx: *mut JSContext,
    _cache: JS::SelfHostedCache,
    _writer: JS::SelfHostedWriter,
) -> bool {
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetWarningReporter(cx: *mut JSContext, reporter: JS::WarningReporter) -> JS::WarningReporter {
    ::std::mem::replace(&mut unsafe { cx_of(cx) }.warning_reporter, reporter)
}

/// Servo's CSP checks for eval and Function constructors, called by ServoHost before compilation.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetSecurityCallbacks(cx: *mut JSContext, callbacks: *const JSSecurityCallbacks) {
    unsafe { cx_of(cx) }.security_callbacks = callbacks;
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetSecurityCallbacks(cx: *mut JSContext) -> *const JSSecurityCallbacks {
    unsafe { cx_of(cx) }.security_callbacks
}

/// Servo's memory reporter hook (`WantToMeasure`): there is no SpiderMonkey heap census to feed it, so it is kept and unused.
#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__InitializeMemoryReporter(_wtm: crate::glue::WantToMeasure) {}
