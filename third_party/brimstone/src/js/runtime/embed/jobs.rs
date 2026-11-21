//! Promise jobs handed to the host.
//!
//! When a host is installed every task the engine enqueues (promise reactions, await
//! continuations, thenable jobs, FinalizationRegistry cleanups) is wrapped in a callable job
//! object and given to `Host::enqueue_job`. Calling the job object runs the task once.
//!
//! Job objects live in the host space and use a shape created at install time, so wrapping a task
//! never allocates on the moving heap. This matters because tasks are enqueued from places that
//! hold raw heap pointers.

use crate::{
    handle_scope,
    runtime::{
        Context, EvalResult, Handle, Value,
        embed::{
            host,
            host_object::alloc_host_object,
            host_space::{HostAllocation, HostFlags},
            state,
        },
        object_value::ObjectValue,
    },
};

/// Wrap every task in the engine's task queue into a job object and hand it to the host. Does
/// nothing during a GC (the GC calls this again when it finishes).
pub(crate) fn hand_tasks_to_host(mut cx: Context) {
    if state(cx).in_gc {
        return;
    }

    // The state is re-borrowed for each use: allocating the job object takes the state, and the
    // host may re-enter the engine, so no borrow of it may be live across either call.
    while let Some(task) = cx.task_queue_mut().pop_front() {
        let (promise, realm) = task.host_job_info();
        // Tasks without a realm of their own (a reaction with no handler) run in the current realm
        let realm = realm.unwrap_or_else(|| {
            if cx.vm().is_stack_empty() {
                cx.initial_realm_ptr()
            } else {
                cx.current_realm_ptr()
            }
        });

        let job_shape = state(cx).job_shape;
        let job = alloc_host_object::<ObjectValue>(
            cx,
            job_shape,
            HostAllocation {
                object_size: job_shape.object_byte_size(),
                num_reserved_slots: 0,
                flags: HostFlags::INTERNAL_JOB,
                class: std::ptr::null(),
                handler: std::ptr::null(),
                proxy_private: Value::undefined(),
                realm,
            },
        );

        let key = job.as_ptr() as usize;
        state(cx).jobs.insert(key, task);

        let incumbent_global = Some(realm.global_object_ptr().as_object());
        if !host(cx).enqueue_job(cx, job, promise, incumbent_global) {
            // The host refused the job. Put the task back so `run_all_tasks` can run it, and stop
            // handing over tasks until the next enqueue.
            let task = state(cx).jobs.remove(&key).unwrap();
            cx.task_queue_mut().push_front(task);
            return;
        }
    }
}

/// Run the task wrapped by a job object. Running a job a second time does nothing.
pub(crate) fn run_job(cx: Context, job: Handle<ObjectValue>) -> EvalResult<Handle<Value>> {
    let Some(task) = state(cx).jobs.remove(&(job.as_ptr() as usize)) else {
        return Ok(cx.undefined());
    };

    handle_scope!(cx, task.execute(cx))?;

    Ok(cx.undefined())
}
