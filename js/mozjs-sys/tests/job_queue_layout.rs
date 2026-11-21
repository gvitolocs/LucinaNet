/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The job queue is one object with two views: the glue's `JobQueue`, which `MicroTasks::trace` reads, and bindgen's
//! `RustJobQueue`, the type `CreateJobQueue` is declared to hand back and the type the host side reads fields of. Both views
//! name the same bytes, so the object must be as large as a `RustJobQueue` and put every field where a `RustJobQueue` says it
//! is. Here: the shape of the two types, then the object a call to `CreateJobQueue` actually returns.

use std::alloc::{GlobalAlloc, Layout, System};
use std::mem::{align_of, offset_of, size_of};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use mozjs_sys::engine::jobs::JobQueue;
use mozjs_sys::glue::{CreateJobQueue, DeleteJobQueue, JobQueueTraps, RustJobQueue};

/// Every block allocated while `WATCHING` is set, so a test can ask which block an object sits in and how big it is.
/// Recording is two atomic stores, so a window can be as short as one call.
const WATCH_SLOTS: usize = 256;
static WATCHING: AtomicBool = AtomicBool::new(false);
static WATCH_ADDR: [AtomicUsize; WATCH_SLOTS] = [const { AtomicUsize::new(0) }; WATCH_SLOTS];
static WATCH_SIZE: [AtomicUsize; WATCH_SLOTS] = [const { AtomicUsize::new(0) }; WATCH_SLOTS];
static WATCH_NEXT: AtomicUsize = AtomicUsize::new(0);

struct Watched;

unsafe impl GlobalAlloc for Watched {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() && WATCHING.load(Ordering::Relaxed) {
            let i = WATCH_NEXT.fetch_add(1, Ordering::Relaxed) % WATCH_SLOTS;
            WATCH_ADDR[i].store(p as usize, Ordering::Relaxed);
            WATCH_SIZE[i].store(layout.size(), Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Watched = Watched;

/// `f`, plus the blocks allocated while it ran.
fn watched<T>(f: impl FnOnce() -> T) -> (T, Vec<(usize, usize)>) {
    for i in 0..WATCH_SLOTS {
        WATCH_ADDR[i].store(0, Ordering::Relaxed);
        WATCH_SIZE[i].store(0, Ordering::Relaxed);
    }
    WATCH_NEXT.store(0, Ordering::Relaxed);
    WATCHING.store(true, Ordering::Relaxed);
    let value = f();
    WATCHING.store(false, Ordering::Relaxed);
    let blocks = (0..WATCH_SLOTS)
        .filter_map(|i| match (WATCH_ADDR[i].load(Ordering::Relaxed), WATCH_SIZE[i].load(Ordering::Relaxed)) {
            (0, _) => None,
            (addr, size) => Some((addr, size)),
        })
        .collect();
    (value, blocks)
}

/// The block a pointer sits in, and its size.
fn holder_of(blocks: &[(usize, usize)], p: *const u8) -> Option<(usize, usize)> {
    let at = p as usize;
    blocks.iter().copied().find(|(addr, size)| *addr <= at && at < *addr + *size)
}

unsafe extern "C" fn runs(_cx: *mut mozjs_sys::jsapi::JSContext) {}
unsafe extern "C" fn traced(_trc: *mut mozjs_sys::jsapi::JSTracer, _v: *mut mozjs_sys::jsapi::JS::Value) {}

fn traps() -> JobQueueTraps {
    JobQueueTraps {
        getHostDefinedData: None,
        getHostDefinedGlobal: None,
        runJobs: Some(runs),
        traceNonGCThingMicroTask: Some(traced),
    }
}

/// The two views agree on where everything is and on how big the object is.
#[test]
fn job_queue_and_rust_job_queue_have_the_same_shape() {
    assert_eq!(
        align_of::<JobQueue>(),
        align_of::<RustJobQueue>(),
        "JobQueue aligns {:#x}, RustJobQueue aligns {:#x}",
        align_of::<JobQueue>(),
        align_of::<RustJobQueue>()
    );
    assert_eq!(
        offset_of!(JobQueue, traps),
        offset_of!(RustJobQueue, mTraps),
        "the traps sit at {:#x} for the glue and {:#x} for the host",
        offset_of!(JobQueue, traps),
        offset_of!(RustJobQueue, mTraps)
    );
    assert!(
        size_of::<JobQueue>() >= size_of::<RustJobQueue>(),
        "JobQueue is {:#x} bytes and cannot carry a RustJobQueue of {:#x}",
        size_of::<JobQueue>(),
        size_of::<RustJobQueue>()
    );
    assert!(
        offset_of!(JobQueue, traps) + size_of::<JobQueueTraps>() <= offset_of!(RustJobQueue, draining),
        "the traps reach {:#x}, where the host reads draining at {:#x}",
        offset_of!(JobQueue, traps) + size_of::<JobQueueTraps>(),
        offset_of!(RustJobQueue, draining)
    );
    assert!(
        size_of::<JobQueue>() >= offset_of!(RustJobQueue, draining) + size_of::<bool>(),
        "draining lives at {:#x}, past a JobQueue of {:#x} bytes",
        offset_of!(RustJobQueue, draining),
        size_of::<JobQueue>()
    );
    assert_eq!(
        size_of::<JobQueue>(),
        size_of::<RustJobQueue>(),
        "the two views are one object, so one size: {:#x} against {:#x}",
        size_of::<JobQueue>(),
        size_of::<RustJobQueue>()
    );
    assert_eq!(
        offset_of!(JobQueue, vtable_),
        offset_of!(RustJobQueue, vtable_),
        "the object starts at {:#x} for the glue and {:#x} for the host",
        offset_of!(JobQueue, vtable_),
        offset_of!(RustJobQueue, vtable_)
    );
    assert_eq!(
        offset_of!(JobQueue, draining),
        offset_of!(RustJobQueue, draining),
        "draining sits at {:#x} for the glue and {:#x} for the host",
        offset_of!(JobQueue, draining),
        offset_of!(RustJobQueue, draining)
    );
}

/// What one `CreateJobQueue` call returns: a block that carries the whole `RustJobQueue`, traps where both views read them,
/// and a `draining` that starts false and keeps what the host writes there. The block is settled first, because the field
/// reads below are only meaningful inside it.
#[test]
fn a_created_queue_carries_every_rust_job_queue_field() {
    let want = traps();
    let (queue, blocks) = watched(|| unsafe { CreateJobQueue(&want) });
    assert!(!queue.is_null(), "CreateJobQueue returned null");
    let (addr, size) = holder_of(&blocks, queue as *const u8).expect("the queue is not in a block this call allocated");
    let end = queue as usize + size_of::<RustJobQueue>();
    assert!(
        end <= addr + size,
        "a RustJobQueue spans {:#x}..{:#x}, but its block is {addr:#x}..{:#x}",
        queue as usize,
        end,
        addr + size
    );
    unsafe {
        assert_eq!((*queue).mTraps, want, "the host view must read the traps it was given");
        assert_eq!((*queue.cast::<JobQueue>()).traps, want, "both views must read the same traps");
        assert!(!(*queue).draining, "draining must start false");
        (*queue).draining = true;
        assert!((*queue).draining, "draining must keep what the host wrote");
        assert_eq!((*queue).mTraps, want, "a write to draining leaves the traps where they were");
        (*queue).draining = false;
        assert!(!(*queue).draining, "draining is back to its documented value");
        DeleteJobQueue(queue);
    }
}
