/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Implementation of [microtasks](https://html.spec.whatwg.org/multipage/#microtask) and
//! microtask queues. It is up to implementations of event loops to store a queue and
//! perform checkpoints at appropriate times, as well as enqueue microtasks as required.

use std::cell::Cell;
use std::ffi::c_void;
use std::ptr::NonNull;

use js::context::JSContext;
use js::glue::{CreateJobQueue, DeleteJobQueue, JobQueueTraps, RustJobQueue};
use js::jsapi::{
    GetExecutionGlobalFromJSMicroTask, GetPromiseUserInputEventHandlingState, IsJSMicroTask,
    JSContext as RawJSContext, JSTracer, MaybeGetPromiseFromJSMicroTask, MutableHandleObject,
    PromiseUserInputEventHandlingState, ToMaybeWrappedJSMicroTask,
};
use js::jsval::{JSVal, PrivateValue};
use js::panic::wrap_panic;
use js::realm::AutoRealm;
use js::rust::wrappers2::{
    EnqueueMicroTask, HasAnyMicroTasks, JS_DequeueNextMicroTask, JobQueueIsEmpty,
    JobQueueMayNotBeEmpty, MaybeGetHostDefinedDataFromJSMicroTask, RunJSMicroTask, SetJobQueue,
};
use malloc_size_of::MallocSizeOf;
use script_bindings::callback::TracedCallback;
use script_bindings::reflector::DomObject as _;
use script_bindings::root::Dom;
use script_bindings::settings_stack::{run_a_callback, run_a_script};

use crate::dom::bindings::callback::ExceptionHandling;
use crate::dom::bindings::codegen::Bindings::VoidFunctionBinding::VoidFunction;
use crate::dom::bindings::root::DomRoot;
use crate::dom::globalscope::GlobalScope;
use crate::event_loop::script_thread::ScriptThread;
use crate::realms::enter_auto_realm;
use crate::runtime::script_runtime::notify_about_rejected_promises;
use crate::{DomTypeHolder, JSTraceable};

pub(crate) static JOB_QUEUE_TRAPS: JobQueueTraps = JobQueueTraps {
    getHostDefinedData: Some(get_host_defined_data),
    getHostDefinedGlobal: Some(get_host_defined_global),
    runJobs: Some(run_jobs),
    traceNonGCThingMicroTask: Some(trace_non_gc_things_micro_task),
};

pub(crate) struct JobQueue(*mut RustJobQueue);

// LucinaNet: the glue's numbers: its allocation was 32 bytes while Servo read
// offset 40. Servo no longer reads `draining` (own thread-local); this stops the structs drifting.
const _: () = {
    assert!(std::mem::size_of::<RustJobQueue>() == 48);
    assert!(std::mem::align_of::<RustJobQueue>() == 8);
    assert!(std::mem::offset_of!(RustJobQueue, vtable_) == 0);
    assert!(std::mem::offset_of!(RustJobQueue, mTraps) == 8);
    assert!(std::mem::offset_of!(RustJobQueue, draining) == 40);
};

#[expect(unsafe_code)]
unsafe impl JSTraceable for JobQueue {
    unsafe fn trace(&self, _trc: *mut JSTracer) {
        // RustJobQueue does not contain any GC things.
    }
}

impl MallocSizeOf for JobQueue {
    fn size_of(&self, _ops: &mut malloc_size_of::MallocSizeOfOps) -> usize {
        // TODO: measure size of all enqueued microtasks
        0
    }
}

impl JobQueue {
    #[expect(unsafe_code)]
    pub(crate) fn new() -> Self {
        JobQueue(unsafe { CreateJobQueue(&JOB_QUEUE_TRAPS) })
    }

    #[expect(unsafe_code)]
    pub(crate) fn set_on_context(&self, cx: &JSContext) {
        unsafe { SetJobQueue(cx, self.0 as *mut _) };
    }
}

impl Drop for JobQueue {
    #[expect(unsafe_code)]
    fn drop(&mut self) {
        unsafe {
            DeleteJobQueue(self.0);
        }
    }
}

/// <https://searchfox.org/firefox-main/rev/446c6e609dbd7c355c2fb27209dfe4833211991f/xpcom/base/CycleCollectedJSContext.cpp#229>
#[expect(unsafe_code)]
unsafe extern "C" fn get_host_defined_data(
    cx: *mut RawJSContext,
    incumbent_global: MutableHandleObject,
    data: MutableHandleObject,
) -> bool {
    incumbent_global.set(std::ptr::null_mut());
    data.set(std::ptr::null_mut());
    if !unsafe { get_host_defined_global(cx, incumbent_global) } {
        return false;
    }

    if incumbent_global.is_null() {
        return true;
    }

    // we have no schedulingState

    true
}

#[allow(unsafe_code)]
/// <https://searchfox.org/firefox-main/rev/446c6e609dbd7c355c2fb27209dfe4833211991f/xpcom/base/CycleCollectedJSContext.cpp#199>
unsafe extern "C" fn get_host_defined_global(
    _cx: *mut RawJSContext,
    out: MutableHandleObject,
) -> bool {
    // LucinaNet: a caught panic answers the hook's failure, `false` with nothing
    // pending (the uncatchable exception), not `true`: the glue enqueues the job on `true`.
    let mut wrap_result = false;
    wrap_panic(&mut || {
        wrap_result = (|| -> bool {
            let Some(incumbent_global) = GlobalScope::incumbent() else {
                return true;
            };

            out.set(incumbent_global.reflector().get_jsobject().get());
            true
        })()
    });

    wrap_result
}

#[expect(unsafe_code)]
unsafe extern "C" fn run_jobs(cx: *mut RawJSContext) {
    // LucinaNet: the whole hook body is under the wrap, including the context
    // construction, which used to be able to panic outside of it.
    wrap_panic(&mut || {
        let mut cx = unsafe {
            // SAFETY: We are in SM hook
            JSContext::from_ptr(NonNull::new(cx).expect("JSContext should not be null in SM hook"))
        };
        // TODO: run Promise- and User-variant Microtasks, and do #notify-about-rejected-promises.
        // Those will require real `globalscopes` values.
        job_queue_microtask_checkpoint(&mut cx, vec![]);
    });
}

#[derive(JSTraceable, MallocSizeOf)]
pub struct NotifyMutationObserversMicrotask;

impl NotifyMutationObserversMicrotask {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl MicrotaskRunnable for NotifyMutationObserversMicrotask {
    fn handler(&self, cx: &mut JSContext) {
        ScriptThread::mutation_observers().notify_mutation_observers(cx);
    }
}

#[derive(JSTraceable, MallocSizeOf)]
pub struct CustomElementReactionMicrotask;

impl CustomElementReactionMicrotask {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl MicrotaskRunnable for CustomElementReactionMicrotask {
    fn handler(&self, cx: &mut JSContext) {
        ScriptThread::invoke_backup_element_queue(cx);
    }
}

pub(crate) trait MicrotaskRunnable: JSTraceable + MallocSizeOf {
    // must also take care of entering the realm
    fn handler(&self, _cx: &mut JSContext) {}
}

/// A microtask that comes from a queueMicrotask() Javascript call
#[derive(JSTraceable, MallocSizeOf)]
#[cfg_attr(crown, crown::unrooted_must_root_lint::must_root)]
pub(crate) struct UserMicrotask {
    pub(crate) callback: TracedCallback<VoidFunction>,
    pub(crate) global: Dom<GlobalScope>,
}

impl MicrotaskRunnable for UserMicrotask {
    fn handler(&self, cx: &mut JSContext) {
        let mut realm = enter_auto_realm(cx, &*self.global);
        let cx = &mut realm;
        let _ = self
            .callback
            .Call_(cx, &*self.global, ExceptionHandling::Report);
    }
}

fn microtask_from_jsval(val: JSVal) -> *mut Box<dyn MicrotaskRunnable> {
    val.to_private() as *const Box<dyn MicrotaskRunnable> as *mut Box<dyn MicrotaskRunnable>
}

/// Add a new microtask to this queue. It will be invoked as part of the next
/// microtask checkpoint.
#[expect(unsafe_code)]
pub(crate) fn enqueue(cx: &JSContext, task: Box<dyn MicrotaskRunnable>) {
    let task = Box::new(task);
    let raw = Box::into_raw(task);
    unsafe { JobQueueMayNotBeEmpty(cx) };
    // LucinaNet: a poisoned or terminated engine refuses the task and answers false
    // without taking it, so the box is still ours. Reclaiming it is the whole of the failure: the
    // caller's checkpoint finds the queue empty, and the resume point ends this thread.
    if !unsafe { EnqueueMicroTask(cx, &PrivateValue(raw as *const c_void)) } {
        unsafe { drop(Box::from_raw(raw)) };
    }
}

/// <https://html.spec.whatwg.org/multipage/#perform-a-microtask-checkpoint>
/// Perform a microtask checkpoint, executing all queued microtasks until the queue is empty.
#[expect(unsafe_code)]
pub(crate) fn job_queue_microtask_checkpoint(
    cx: &mut JSContext,
    globalscopes: Vec<DomRoot<GlobalScope>>,
) {
    // LucinaNet: the flag is this thread's, not `(*GetJobQueue(cx)).draining`. The
    // glue's job queue is only its traps table (glue `mozjs-sys/src/engine/jobs.rs`, `JobQueue`),
    // so `RustJobQueue::draining` (offset 40) was a read and write past that 32-byte allocation:
    // whether microtasks ran depended on the heap layout. On a poisoned context `GetJobQueue`
    // also answered null, which was dereferenced. One context per thread, so a thread-local is
    // the event loop's flag.
    // Step 1. If the event loop's performing a microtask checkpoint is true, then return.
    if DrainingFlag::is_set() {
        return;
    }

    // Step 2. Set the event loop's performing a microtask checkpoint to true.
    // LucinaNet: through a guard, so step 7 also runs when the checkpoint unwinds (a
    // re-raised panic in a job, or `run_jobs`' wrap catching one): a flag left true would make
    // every later checkpoint return at step 1 and run nothing.
    let draining = DrainingFlag::set();

    debug!("Now performing a microtask checkpoint");

    rooted!(&in(cx) let mut generic_task: js::jsapi::GenericMicroTask);
    rooted!(&in(cx) let mut js_micro_task: *mut js::jsapi::JSMicroTask);
    rooted!(&in(cx) let mut execution_global: *mut js::jsapi::JSObject);
    rooted!(&in(cx) let mut incumbent_global: *mut js::jsapi::JSObject);
    rooted!(&in(cx) let mut data: *mut js::jsapi::JSObject);

    // Step 3. While the event loop's microtask queue is not empty:
    // based on https://spidermonkey.dev/blog/2026/01/15/job-responsibility.html#running-micro-tasks
    // and https://searchfox.org/firefox-main/rev/7ae92e67d094086cd3e09918ec94b6278a948535/xpcom/base/CycleCollectedJSContext.cpp#1176
    // and its helper functions
    while unsafe { HasAnyMicroTasks(cx) } {
        unsafe { JS_DequeueNextMicroTask(cx, generic_task.handle_mut()) };

        // Notify the JS engine if the queue is now empty, enabling optimizations
        // like skipping await microtask creation for resolved promises.
        if !unsafe { HasAnyMicroTasks(cx) } {
            unsafe { JobQueueIsEmpty(cx) };
        }

        // https://searchfox.org/firefox-main/rev/50691777d300fffc7d1f7844b59769109bc76f3e/xpcom/base/CycleCollectedJSContext.cpp#916
        if !unsafe { IsJSMicroTask(generic_task.as_ptr()) } {
            rooted!(&in(cx) let task = unsafe {
                Box::from_raw(
                    microtask_from_jsval(*generic_task),
                )
            });
            task.handler(cx);
            continue;
        }

        js_micro_task.set(unsafe { ToMaybeWrappedJSMicroTask(generic_task.as_ptr()) });
        execution_global.set(unsafe { GetExecutionGlobalFromJSMicroTask(js_micro_task.get()) });
        if execution_global.get().is_null() {
            continue;
        }
        if !unsafe {
            MaybeGetHostDefinedDataFromJSMicroTask(
                js_micro_task.get(),
                incumbent_global.handle_mut(),
                data.handle_mut(),
            )
        } {
            continue;
        }

        let interaction = if let Some(promise) =
            NonNull::new(unsafe { MaybeGetPromiseFromJSMicroTask(js_micro_task.get()) })
        {
            unsafe { GetPromiseUserInputEventHandlingState(promise.as_ptr()) }
        } else {
            PromiseUserInputEventHandlingState::DontCare
        };
        let _maybe_user_interacting_guard =
            if interaction == PromiseUserInputEventHandlingState::HadUserInteractionAtCreation {
                Some(ScriptThread::user_interacting_guard())
            } else {
                None
            };
        let global_scope = unsafe { GlobalScope::from_object(execution_global.get()) };
        run_a_script::<DomTypeHolder, _, _>(cx, &global_scope, |cx| {
            let mut r = || {
                let mut realm = AutoRealm::new_from_handle(cx, execution_global.handle());
                let _ = unsafe { RunJSMicroTask(&mut realm, js_micro_task.handle()) };
            };
            if incumbent_global.get().is_null() {
                r();
            } else {
                let global_scope = unsafe { GlobalScope::from_object(incumbent_global.get()) };
                run_a_callback::<DomTypeHolder, _>(&global_scope, r);
            }
        });
    }

    // Step 4. For each environment settings object settingsObject whose responsible
    // event loop is this event loop, notify about rejected promises given
    // settingsObject's global object.
    for global in &globalscopes {
        notify_about_rejected_promises(cx, global);
    }

    // https://html.spec.whatwg.org/multipage/#perform-a-microtask-checkpoint
    // Step 5. Cleanup Indexed Database transactions.
    // https://w3c.github.io/IndexedDB/#cleanup-indexed-database-transactions
    // “These steps are invoked by [HTML]. They ensure that transactions created by a script call
    // to transaction() are deactivated once the task that invoked the script has completed.”
    for global in globalscopes.iter() {
        if let Some(factory) = global.indexeddb_factory() {
            let _ = factory.cleanup_indexeddb_transactions(cx);
        }
    }

    // Step 6. Perform ClearKeptObjects().
    unsafe { js::rust::wrappers2::ClearKeptObjects(cx) };

    // Step 7. Set the event loop's performing a microtask checkpoint to false.
    drop(draining);
    // TODO: Step 8. Record timing info for microtask checkpoint.
}

thread_local! {
    /// LucinaNet: this thread's "performing a microtask checkpoint" flag (one context
    /// per thread). See `job_queue_microtask_checkpoint` for why it is not the glue's.
    static DRAINING: Cell<bool> = const { Cell::new(false) };
}

/// LucinaNet: the microtask checkpoint's "performing a microtask checkpoint" flag, set
/// for the guard's life and cleared when it drops, on return and on unwind alike.
struct DrainingFlag(());

impl DrainingFlag {
    /// Whether a checkpoint is running on this thread.
    fn is_set() -> bool {
        DRAINING.with(Cell::get)
    }

    /// Sets the flag until the guard drops.
    fn set() -> Self {
        DRAINING.with(|draining| draining.set(true));
        DrainingFlag(())
    }
}

impl Drop for DrainingFlag {
    fn drop(&mut self) {
        // `try_with`: a guard dropped during thread teardown must not panic in `Drop`.
        let _ = DRAINING.try_with(|draining| draining.set(false));
    }
}

#[cfg(test)]
mod tests {
    use super::DrainingFlag;

    #[test]
    fn draining_flag_is_cleared_when_the_checkpoint_unwinds() {
        let unwound = std::panic::catch_unwind(|| {
            let _draining = DrainingFlag::set();
            assert!(DrainingFlag::is_set());
            panic!("a job panicked inside the checkpoint");
        });
        assert!(unwound.is_err());
        assert!(
            !DrainingFlag::is_set(),
            "the flag stayed set after the unwind"
        );
    }

    #[test]
    fn draining_flag_is_cleared_on_return() {
        {
            let _draining = DrainingFlag::set();
            assert!(DrainingFlag::is_set());
        }
        assert!(!DrainingFlag::is_set());
    }
}

#[expect(unsafe_code)]
pub(crate) fn job_queue_clear(cx: &JSContext) {
    rooted!(&in(cx) let mut generic_task: js::jsapi::GenericMicroTask);
    while unsafe { HasAnyMicroTasks(cx) } {
        unsafe { JS_DequeueNextMicroTask(cx, generic_task.handle_mut()) };
        if !unsafe { IsJSMicroTask(generic_task.as_ptr()) } {
            let task = unsafe { Box::from_raw(microtask_from_jsval(*generic_task)) };
            drop(task);
        }
    }
}

#[expect(unsafe_code)]
unsafe extern "C" fn trace_non_gc_things_micro_task(trc: *mut JSTracer, val: *mut JSVal) {
    // LucinaNet: a tracer, so deliberately NOT under `wrap_panic` (glue
    // `docs/edge-wrap.md` section d): a caught panic would return as if the microtask had been
    // traced and the collection would free what it holds. A panic here cannot unwind out of this
    // `extern "C"` function, so it aborts the process, which is the designed answer for a
    // GC-time failure, the same as Servo's other tracers and finalizers.
    let task = microtask_from_jsval(unsafe { *val });
    unsafe { (**task).trace(trc) };
}
