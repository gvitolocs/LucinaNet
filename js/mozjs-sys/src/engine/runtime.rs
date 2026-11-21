/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The runtime settings Servo applies when it creates a script runtime (`components/script/runtime/script_runtime.rs`): hooks
//! it registers, GC and JIT tuning, interrupt control. The original is `jsapi.cpp`, `jsfriendapi.cpp` and `js/src/gc/GC.cpp`.
//!
//! Three kinds of entry here:
//! - hooks the engine will call once the feature exists on our side (modules, event-loop dispatch, the script environment
//!   preparer, realm destruction, out of memory): stored in `Cx::hooks`;
//! - SpiderMonkey tuning with no Brimstone counterpart (incremental and nursery GC, JIT compiler options, profiling stacks):
//!   accepted and ignored, each saying so;
//! - small queries answered as Brimstone is (no incremental GC, no JIT).

use ::std::ffi::c_void;
use ::std::sync::atomic::Ordering;

use super::cx::cx_of;
use crate::jsapi::*;

/// Hooks kept for features that will use them.
#[derive(Default)]
pub struct Hooks {
    pub gc_slice: JS::GCSliceCallback,
    pub destroy_realm: JS::DestroyRealmCallback,
    pub realm_name: JS::RealmNameCallback,
    pub out_of_memory: Option<(JS::OutOfMemoryCallback, *mut c_void)>,
    pub cleanup_finalization_registry: Option<(JSHostCleanupFinalizationRegistryCallback, *mut c_void)>,
    pub ensure_can_add_private_element: JS::EnsureCanAddPrivateElementOp,
    pub consume_stream: Option<(JS::ConsumeStreamCallback, JS::ReportStreamErrorCallback)>,
    pub event_loop_dispatch: Option<(crate::glue::RustDispatchToEventLoopCallback, *mut c_void)>,
    pub script_preparer: crate::glue::InvokeScriptPreparerHook,
    pub module_load: JS::ModuleLoadHook,
    pub module_metadata: JS::ModuleMetadataHook,
    pub script_private_add: JS::ScriptPrivateReferenceHook,
    pub script_private_release: JS::ScriptPrivateReferenceHook,
    pub stack_format: Option<js::StackFormat>,
    pub gc_params: ::std::collections::HashMap<u32, u32>,
    pub interrupt_enabled: bool,
    pub context_options: Option<Box<JS::ContextOptions>>,
    pub realm_creation_options: Option<Box<JS::RealmCreationOptions>>,
    pub realm_behaviors: Option<Box<JS::RealmBehaviors>>,
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetGCSliceCallback(cx: *mut JSContext, callback: JS::GCSliceCallback) -> JS::GCSliceCallback {
    ::std::mem::replace(&mut unsafe { cx_of(cx) }.hooks.gc_slice, callback)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetDestroyRealmCallback(cx: *mut JSContext, callback: JS::DestroyRealmCallback) {
    unsafe { cx_of(cx) }.hooks.destroy_realm = callback;
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetRealmNameCallback(cx: *mut JSContext, callback: JS::RealmNameCallback) {
    unsafe { cx_of(cx) }.hooks.realm_name = callback;
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetOutOfMemoryCallback(cx: *mut JSContext, cb: JS::OutOfMemoryCallback, data: *mut c_void) {
    unsafe { cx_of(cx) }.hooks.out_of_memory = Some((cb, data));
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetHostCleanupFinalizationRegistryCallback(
    cx: *mut JSContext,
    cb: JSHostCleanupFinalizationRegistryCallback,
    data: *mut c_void,
) {
    unsafe { cx_of(cx) }.hooks.cleanup_finalization_registry = Some((cb, data));
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetHostEnsureCanAddPrivateElementHook(cx: *mut JSContext, op: JS::EnsureCanAddPrivateElementOp) {
    unsafe { cx_of(cx) }.hooks.ensure_can_add_private_element = op;
}

/// Low-memory mode tunes SpiderMonkey's GC heuristics; Brimstone's heap only grows when a collection leaves it full.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetLowMemoryState(_cx: *mut JSContext, _state: bool) {}

/// Incremental GC slice budgets: Brimstone's collections are not incremental.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetCreateGCSliceBudgetCallback(_cx: *mut JSContext, _cb: JS::CreateSliceBudgetCallback) {}

/// The build id keys SpiderMonkey's bytecode caches; Brimstone has none, so the op is kept by Servo and never called.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetProcessBuildIdOp(_op: JS::BuildIdOp) {}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetScriptPrivateReferenceHooks(
    rt: *mut JSRuntime,
    add: JS::ScriptPrivateReferenceHook,
    release: JS::ScriptPrivateReferenceHook,
) {
    let hooks = &mut unsafe { cx_of(rt.cast()) }.hooks;
    hooks.script_private_add = add;
    hooks.script_private_release = release;
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__InitConsumeStreamCallback(
    cx: *mut JSContext,
    consume: JS::ConsumeStreamCallback,
    report: JS::ReportStreamErrorCallback,
) {
    unsafe { cx_of(cx) }.hooks.consume_stream = Some((consume, report));
}

/// Off-thread tasks (Wasm compilation, Atomics.waitAsync) dispatched back to the event loop: Brimstone runs none yet.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__InitAsyncTaskCallbacks(
    _cx: *mut JSContext,
    _dispatch: JS::DispatchToEventLoopCallback,
    _delayed: JS::DelayedDispatchToEventLoopCallback,
    _started: JS::AsyncTaskStartedCallback,
    _finished: JS::AsyncTaskFinishedCallback,
    _closure: *mut c_void,
) {
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__SetUpEventLoopDispatch(
    cx: *mut JSContext,
    callback: crate::glue::RustDispatchToEventLoopCallback,
    _started: JS::AsyncTaskStartedCallback,
    _finished: JS::AsyncTaskFinishedCallback,
    closure: *mut c_void,
) {
    unsafe { cx_of(cx) }.hooks.event_loop_dispatch = Some((callback, closure));
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__RegisterScriptEnvironmentPreparer(cx: *mut JSContext, hook: crate::glue::InvokeScriptPreparerHook) {
    unsafe { cx_of(cx) }.hooks.script_preparer = hook;
}

/// Allocation metadata (devtools' allocation sites): not recorded.
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__SetAllocationMetadataBuilder(_cx: *mut JSContext, _builder: *const js::AllocationMetadataBuilder) {}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__SetStackFormat(cx: *mut JSContext, format: js::StackFormat) {
    unsafe { cx_of(cx) }.hooks.stack_format = Some(format);
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__GetStackFormat(cx: *mut JSContext) -> js::StackFormat {
    unsafe { cx_of(cx) }.hooks.stack_format.unwrap_or(js::StackFormat::SpiderMonkey)
}

/// The Gecko profiler's pseudo-stack: not maintained.
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__SetContextProfilingStack(_cx: *mut JSContext, _stack: *mut crate::jsapi::ProfilingStack) {}
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__EnableContextProfilingStack(_cx: *mut JSContext, _enabled: bool) {}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetDestroyZoneCallback(_cx: *mut JSContext, _cb: JSDestroyZoneCallback) {}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetDestroyCompartmentCallback(_cx: *mut JSContext, _cb: JSDestroyCompartmentCallback) {}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetSizeOfIncludingThisCompartmentCallback(_cx: *mut JSContext, _cb: JSSizeOfIncludingThisCompartmentCallback) {}

/// JIT compiler options: Brimstone is an interpreter.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetGlobalJitCompilerOption(_cx: *mut JSContext, _opt: JSJitCompilerOption, _value: u32) {}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetGlobalJitCompilerOption(_cx: *mut JSContext, _opt: JSJitCompilerOption, value: *mut u32) -> bool {
    unsafe { *value = 0 };
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetOffthreadIonCompilationEnabled(_cx: *mut JSContext, _enabled: bool) {}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetOffthreadBaselineCompilationEnabled(_cx: *mut JSContext, _enabled: bool) {}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__DisableJitBackend() {}

/// GC tuning from the machine's memory size: Brimstone sizes its heap itself.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetGCParametersBasedOnAvailableMemory(_cx: *mut JSContext, _mb: u32) {}

/// `Atomics.wait` on this thread: Brimstone has no SharedArrayBuffer or Atomics yet.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_SetFutexCanWait(_cx: *mut JSContext) {}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetMeasuringExecutionTimeEnabled(_cx: *mut JSContext, _value: bool) {}

/// Brittle mode turns some internal assertions into crashes in SpiderMonkey's fuzzing builds. Returns the previous setting.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetBrittleMode(_cx: *mut JSContext, _setting: bool) -> bool {
    false
}

/// Nursery collections: Brimstone has no nursery, so the callback would never fire; it is accepted.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__AddGCNurseryCollectionCallback(
    _cx: *mut JSContext,
    _cb: JS::GCNurseryCollectionCallback,
    _data: *mut c_void,
) -> bool {
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__RemoveGCNurseryCollectionCallback(_cx: *mut JSContext, _cb: JS::GCNurseryCollectionCallback, _data: *mut c_void) {}

fn same_fn<F: Copy>(a: Option<F>, b: Option<F>) -> bool {
    // Function pointers compared by address, as SpiderMonkey's vector removal does.
    let addr = |f: Option<F>| f.map(|f| unsafe { ::std::mem::transmute_copy::<F, usize>(&f) });
    addr(a) == addr(b)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_RemoveFinalizeCallback(cx: *mut JSContext, cb: JSFinalizeCallback) {
    unsafe { cx_of(cx) }.finalize_callbacks.retain(|&(f, _)| !same_fn(f, cb));
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_RemoveWeakPointerZonesCallback(cx: *mut JSContext, cb: JSWeakPointerZonesCallback) {
    unsafe { cx_of(cx) }.weak_zone_callbacks.retain(|&(f, _)| !same_fn(f, cb));
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_RemoveWeakPointerCompartmentCallback(cx: *mut JSContext, cb: JSWeakPointerCompartmentCallback) {
    unsafe { cx_of(cx) }.weak_compartment_callbacks.retain(|&(f, _)| !same_fn(f, cb));
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetModuleLoadHook(rt: *mut JSRuntime, func: JS::ModuleLoadHook) {
    unsafe { cx_of(rt.cast()) }.hooks.module_load = func;
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetModuleMetadataHook(rt: *mut JSRuntime, func: JS::ModuleMetadataHook) {
    unsafe { cx_of(rt.cast()) }.hooks.module_metadata = func;
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetModuleLoadHook(rt: *mut JSRuntime) -> JS::ModuleLoadHook {
    unsafe { cx_of(rt.cast()) }.hooks.module_load
}

/// The options structs Servo reads and writes through these references: one per context, zeroed (SpiderMonkey's defaults are
/// mostly false; the flags that matter to Brimstone are read where they apply).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ContextOptionsRef(cx: *mut JSContext) -> *mut JS::ContextOptions {
    let h = &mut unsafe { cx_of(cx) }.hooks;
    &mut **h.context_options.get_or_insert_with(|| Box::new(unsafe { ::std::mem::zeroed() }))
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__RealmCreationOptionsRef1(cx: *mut JSContext) -> *const JS::RealmCreationOptions {
    let h = &mut unsafe { cx_of(cx) }.hooks;
    &**h.realm_creation_options.get_or_insert_with(|| Box::new(unsafe { ::std::mem::zeroed() }))
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__RealmBehaviorsRef1(cx: *mut JSContext) -> *const JS::RealmBehaviors {
    let h = &mut unsafe { cx_of(cx) }.hooks;
    &**h.realm_behaviors.get_or_insert_with(|| Box::new(unsafe { ::std::mem::zeroed() }))
}

/// What `JS_SetGCParameter` recorded, else 0 (Brimstone uses none of them).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetGCParameter(cx: *mut JSContext, key: JSGCParamKey) -> u32 {
    unsafe { cx_of(cx) }.hooks.gc_params.get(&(key as u32)).copied().unwrap_or(0)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ResetGCParameter(cx: *mut JSContext, key: JSGCParamKey) {
    unsafe { cx_of(cx) }.hooks.gc_params.remove(&(key as u32));
}

/// SpiderMonkey's rule: the quota is 90% of the thread's stack (`ThreadStackQuotaForSize`, jsapi.cpp).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ThreadStackQuotaForSize(size: usize) -> JS::NativeStackSize {
    (size as f64 * 0.9) as usize
}

/// No-op: the fork's off-thread parser is iterative and has no recursive-descent
/// stack limit, so the native stack quota SpiderMonkey sets here has nothing to apply.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetNativeStackQuota(_fc: *mut JS::FrontendContext, _stackSize: JS::NativeStackSize) {
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ResetInterruptCallback(cx: *mut JSContext, enable: bool) {
    unsafe { cx_of(cx) }.hooks.interrupt_enabled = enable;
}

/// Returns whether interrupts were enabled, and disables them.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_DisableInterruptCallback(cx: *mut JSContext) -> bool {
    ::std::mem::replace(&mut unsafe { cx_of(cx) }.hooks.interrupt_enabled, false)
}

/// Runs the interrupt callbacks now if an interrupt was requested; false means the script must stop.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_CheckForInterrupt(cx: *mut JSContext) -> bool {
    if !unsafe { cx_of(cx) }.interrupt.requested.swap(false, Ordering::SeqCst) {
        return true;
    }
    let mut go_on = true;
    for cb in unsafe { cx_of(cx) }.interrupt_callbacks.clone() {
        if let Some(cb) = cb {
            go_on &= unsafe { cb(cx) };
        }
    }
    go_on
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_AbortIfWrongThread(cx: *mut JSContext) {
    let here = super::cx::current_raw();
    if here != cx {
        eprintln!("mozjs_sys (Rust): JSContext used on the wrong thread");
        ::std::process::abort();
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__RuntimeHeapState() -> JS::HeapState {
    JS::HeapState::Idle
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsIncrementalGCInProgress(_cx: *mut JSContext) -> bool {
    false
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsIncrementalGCEnabled(_cx: *mut JSContext) -> bool {
    false
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsIncrementalBarrierNeeded(_cx: *mut JSContext) -> bool {
    false
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsGCScheduled(_cx: *mut JSContext) -> bool {
    false
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__PrepareForFullGC(_cx: *mut JSContext) {}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NonIncrementalGC(cx: *mut JSContext, _options: JS::GCOptions, reason: JS::GCReason) {
    unsafe { super::cx::JS_GC(cx, reason) }
}

/// Servo's workers ask the engine to keep its own job queue; ours is always the engine's (`jobs.rs`).
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__UseInternalJobQueues(_cx: *mut JSContext) -> bool {
    true
}

/// One runtime per context: its own parent.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetParentRuntime(cx: *mut JSContext) -> *mut JSRuntime {
    cx.cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ShutdownAsyncTasks(_cx: *mut JSContext) {}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CancelAsyncTasks(_cx: *mut JSContext) {}
