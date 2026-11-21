/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Microtasks and the job queue. The original is SpiderMonkey 153's microtask queue (`js/src/vm/Runtime.cpp`, the
//! `JS::EnqueueMicroTask`/`JS_DequeueNextMicroTask`/`JS::RunJSMicroTask` family in `jsapi.cpp`) and `RustJobQueue` in
//! mozjs_sys's `jsglue.cpp`.
//!
//! As in SpiderMonkey 153 the engine owns the queue and Servo drains it: entries are values, either a JS microtask (a promise
//! job from Brimstone, wrapped here in a small host object holding the job function, its promise, the incumbent global and the
//! host-defined data) or Servo's own task as a `PrivateValue`, which Servo traces through `traceNonGCThingMicroTask`.
//! Without a job queue set (mozjs's own tests), Brimstone keeps its jobs and `js::RunJobs` runs them.

use ::std::collections::VecDeque;

use brimstone_core::runtime::embed::{self, HostFlags};
use brimstone_core::runtime::gc::{HandleScope, HeapPtr};
use brimstone_core::runtime::object_value::ObjectValue;
use brimstone_core::runtime::{Context as Bs, Value as BsValue};

use super::cx::{cx_of, Cx};
use super::func::with_slots;
use super::realm::object_ptr;
use super::util::{fail, handle_of};
use crate::boundary::value_bits;
use crate::glue::JobQueueTraps;
use crate::jsapi::*;

/// The class of our JS microtask objects (no hooks: the GC traces their reserved slots).
pub static MICROTASK_CLASS: JSClass = JSClass {
    name: c"MicroTask".as_ptr(),
    flags: 4 << 8,
    cOps: ::std::ptr::null(),
    spec: ::std::ptr::null(),
    ext: ::std::ptr::null(),
    oOps: ::std::ptr::null(),
};
const SLOT_JOB: u32 = 0;
const SLOT_PROMISE: u32 = 1;
const SLOT_INCUMBENT: u32 = 2;
const SLOT_HOST_DATA: u32 = 3;

/// What `CreateJobQueue` returns. The host side reads the object through `crate::glue::RustJobQueue` and writes `draining`
/// there, and `MicroTasks::trace` reads it back through this type, so the two are one layout: the fields of
/// `RustJobQueue`, in its order, with the assertions below holding against it.
#[repr(C)]
pub struct JobQueue {
    /// `RustJobQueue`'s vtable slot. Nothing here reads it; it is what puts the traps at their offset.
    pub vtable_: *const crate::glue::RustJobQueue__bindgen_vtable,
    pub traps: JobQueueTraps,
    /// The host's microtask-checkpoint re-entrancy flag.
    pub draining: bool,
}

const _: () = assert!(::std::mem::size_of::<JobQueue>() == ::std::mem::size_of::<crate::glue::RustJobQueue>());
const _: () = assert!(::std::mem::align_of::<JobQueue>() == ::std::mem::align_of::<crate::glue::RustJobQueue>());
const _: () = assert!(
    ::std::mem::offset_of!(JobQueue, vtable_) == ::std::mem::offset_of!(crate::glue::RustJobQueue, vtable_),
    "JobQueue::vtable_ and RustJobQueue::vtable_ must start the object"
);
const _: () = assert!(
    ::std::mem::offset_of!(JobQueue, traps) == ::std::mem::offset_of!(crate::glue::RustJobQueue, mTraps),
    "the traps must be where a RustJobQueue reads them"
);
const _: () = assert!(
    ::std::mem::offset_of!(JobQueue, draining) == ::std::mem::offset_of!(crate::glue::RustJobQueue, draining),
    "draining must be where the host reads and writes it"
);

/// The queue, per context: value bits, traced every GC.
#[derive(Default)]
pub struct MicroTasks {
    pub queue: VecDeque<u64>,
}

impl MicroTasks {
    /// GC: object entries move with the heap; Servo's private entries are traced by its trap.
    pub unsafe fn trace(&mut self, job_queue: Option<*mut JS::JobQueue>, trc: *mut JSTracer) {
        let traps = job_queue.map(|q| unsafe { (*q.cast::<JobQueue>()).traps });
        for bits in self.queue.iter_mut() {
            if value_bits::is_traced(*bits) {
                unsafe { super::gc::trace_value_slot(trc, bits) };
            } else if let Some(t) = traps.and_then(|t| t.traceNonGCThingMicroTask) {
                unsafe { t(trc, (bits as *mut u64).cast()) };
            }
        }
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__CreateJobQueue(traps: *const JobQueueTraps) -> *mut crate::glue::RustJobQueue {
    let queue = JobQueue { vtable_: ::std::ptr::null(), traps: unsafe { *traps }, draining: false };
    Box::into_raw(Box::new(queue)).cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__DeleteJobQueue(queue: *mut crate::glue::RustJobQueue) {
    if !queue.is_null() {
        unsafe { drop(Box::from_raw(queue.cast::<JobQueue>())) };
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetJobQueue(cx: *mut JSContext, queue: *mut JS::JobQueue) {
    unsafe { cx_of(cx) }.job_queue = if queue.is_null() { None } else { Some(queue) };
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetJobQueue(cx: *mut JSContext) -> *mut JS::JobQueue {
    unsafe { cx_of(cx) }.job_queue.unwrap_or(::std::ptr::null_mut())
}

/// Called by Brimstone for each promise job when a job queue is set; false leaves it to Brimstone's own queue.
///
/// `cx` stays a raw pointer: Servo's `getHostDefinedData` trap gets the context, and the allocation below may GC.
pub unsafe fn enqueue_job(
    cx: *mut Cx,
    bs: Bs,
    job: HeapPtr<ObjectValue>,
    promise: Option<HeapPtr<ObjectValue>>,
    incumbent: Option<HeapPtr<ObjectValue>>,
) -> bool {
    let Some(queue) = (unsafe { (*cx).job_queue }) else { return false };
    let raw_cx: *mut JSContext = cx.cast();
    let scope = HandleScope::enter(bs);
    let job_h = BsValue::object(job).to_handle(bs).as_object();
    // The promise, the incumbent global and the host-defined data live in traced frame slots from here until they are in the
    // task: the trap may leave a fresh object only in its out-handle (JSAPI hands it a Rooted), and the allocation below may
    // GC and move or free whatever nothing roots. Object value bits are the object's address, so a slot is also the
    // `JSObject*` location the trap's MutableHandle names; null is 0, which the GC skips.
    let ok = unsafe {
        with_slots(cx, 3, |s| {
            *s = incumbent.map_or(0, |g| g.as_ptr() as u64);
            *s.add(1) = 0;
            *s.add(2) = promise.map_or(value_bits::undefined(), |p| BsValue::object(p).as_raw_bits());
            // SpiderMonkey asks the host for the incumbent global and its data when it creates the job.
            if let Some(get) = (*queue.cast::<JobQueue>()).traps.getHostDefinedData {
                let ok = get(
                    raw_cx,
                    JS::MutableHandle { _phantom_0: Default::default(), ptr: s.cast::<*mut JSObject>() },
                    JS::MutableHandle { _phantom_0: Default::default(), ptr: s.add(1).cast::<*mut JSObject>() },
                );
                if !ok {
                    return false;
                }
            }
            let realm = embed::current_realm(bs);
            let task = embed::new_host_object(
                bs,
                realm,
                (&MICROTASK_CLASS as *const JSClass).cast(),
                HostFlags::empty(),
                None,
                4,
                ::std::ptr::null(),
                bs.undefined(),
            );
            match task {
                Ok(t) => {
                    let o = *t;
                    // Read back after the allocation: the GC has updated the slots.
                    let (incumbent_obj, host_data, promise_bits) = (*s as *mut JSObject, *s.add(1) as *mut JSObject, *s.add(2));
                    embed::set_reserved_slot(o, SLOT_JOB, *job_h.as_value());
                    embed::set_reserved_slot(o, SLOT_PROMISE, BsValue::from_raw_bits(promise_bits));
                    let g = if incumbent_obj.is_null() { BsValue::null() } else { BsValue::object(object_ptr(incumbent_obj)) };
                    embed::set_reserved_slot(o, SLOT_INCUMBENT, g);
                    let d = if host_data.is_null() { BsValue::null() } else { BsValue::object(object_ptr(host_data)) };
                    embed::set_reserved_slot(o, SLOT_HOST_DATA, d);
                    (*cx).microtasks.queue.push_back(BsValue::object(o).as_raw_bits());
                    true
                },
                Err(_) => false,
            }
        })
    };
    scope.exit();
    // A full frame stack (None) leaves the job to Brimstone's queue, as a refused job does.
    ok.unwrap_or(false)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__EnqueueMicroTask(cx: *mut JSContext, entry: *const JS::GenericMicroTask) -> bool {
    unsafe { cx_of(cx) }.microtasks.queue.push_back(unsafe { (*entry).asBits_ });
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__HasAnyMicroTasks(cx: *mut JSContext) -> bool {
    !unsafe { cx_of(cx) }.microtasks.queue.is_empty()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetRegularMicroTaskCount(cx: *mut JSContext) -> usize {
    unsafe { cx_of(cx) }.microtasks.queue.len()
}

#[super::poison::engine_shim(failure = unsafe { (*result.ptr).asBits_ = value_bits::undefined() })]
pub unsafe extern "C" fn glue__JS_DequeueNextMicroTask(cx: *mut JSContext, result: JS::MutableHandle<JS::GenericMicroTask>) {
    let bits = unsafe { cx_of(cx) }.microtasks.queue.pop_front().unwrap_or(value_bits::undefined());
    unsafe { (*result.ptr).asBits_ = bits };
}

fn task_of(bits: u64) -> Option<HeapPtr<ObjectValue>> {
    if !value_bits::is_traced(bits) {
        return None;
    }
    let o: HeapPtr<ObjectValue> = HeapPtr::from_ptr(bits as usize as *mut ObjectValue);
    (embed::host_class(o) == Some((&MICROTASK_CLASS as *const JSClass).cast())).then_some(o)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsJSMicroTask(hv: *const JS::GenericMicroTask) -> bool {
    task_of(unsafe { (*hv).asBits_ }).is_some()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ToMaybeWrappedJSMicroTask(t: *const JS::GenericMicroTask) -> *mut JS::JSMicroTask {
    task_of(unsafe { (*t).asBits_ }).map_or(::std::ptr::null_mut(), |o| o.as_ptr().cast())
}

fn slot_object(task: *mut JS::JSMicroTask, slot: u32) -> *mut JSObject {
    let v = embed::reserved_slot(object_ptr(task), slot);
    if v.is_object() { v.as_object().as_ptr().cast() } else { ::std::ptr::null_mut() }
}

/// The global of the job function's realm: where SpiderMonkey runs the job.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetExecutionGlobalFromJSMicroTask(entry: *mut JS::JSMicroTask) -> *mut JSObject {
    let job = slot_object(entry, SLOT_JOB);
    if job.is_null() {
        return ::std::ptr::null_mut();
    }
    unsafe { super::realm::JS__GetObjectRealmOrNull(job) }.cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__MaybeGetPromiseFromJSMicroTask(entry: *mut JS::JSMicroTask) -> *mut JSObject {
    slot_object(entry, SLOT_PROMISE)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__MaybeGetHostDefinedDataFromJSMicroTask(
    entry: *mut JS::JSMicroTask,
    incumbent: JS::MutableHandleObject,
    data: JS::MutableHandleObject,
) -> bool {
    unsafe {
        *incumbent.ptr = slot_object(entry, SLOT_INCUMBENT);
        *data.ptr = slot_object(entry, SLOT_HOST_DATA);
    }
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__RunJSMicroTask(cx: *mut JSContext, entry: JS::Handle<*mut JS::JSMicroTask>) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let job = slot_object(unsafe { *entry.ptr }, SLOT_JOB);
    if job.is_null() {
        return true;
    }
    let scope = HandleScope::enter(bs);
    let f = handle_of(bs, BsValue::object(object_ptr(job)).as_raw_bits());
    let r = embed::call(bs, f, bs.undefined(), &[]);
    let ok = match r {
        Ok(_) => true,
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__JobQueueIsEmpty(_cx: *mut JSContext) {}
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__JobQueueMayNotBeEmpty(_cx: *mut JSContext) {}

/// No user-input tracking for promises.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetPromiseUserInputEventHandlingState(
    _promise: *mut JSObject,
) -> JS::PromiseUserInputEventHandlingState {
    JS::PromiseUserInputEventHandlingState::DontCare
}

/// `js::RunJobs`: Brimstone's own queue (no job queue set), then ours.
#[super::poison::engine_shim]
pub unsafe extern "C" fn js__RunJobs(cx: *mut JSContext) {
    let bs = unsafe { cx_of(cx) }.bs;
    let _ = embed::run_engine_tasks(bs);
    while let Some(bits) = unsafe { cx_of(cx) }.microtasks.queue.pop_front() {
        if let Some(t) = task_of(bits) {
            let mut p: *mut JS::JSMicroTask = t.as_ptr().cast();
            let h = JS::Handle { _phantom_0: Default::default(), ptr: &mut p as *const _ };
            if !unsafe { JS__RunJSMicroTask(cx, h) } {
                break;
            }
        }
    }
}
