/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Servo's side of each GC, reported to Brimstone's collector: what SpiderMonkey's `GCRuntime::traceRuntime` walks (the
//! `Rooted` lists, the extra-roots tracers, our native frames, the pending exception), class trace hooks, finalizers and the
//! weak-pointer callbacks. Brimstone moves ordinary objects, so every slot is reported by address and may be rewritten; host
//! objects never move (the fork's host space).

use ::std::ffi::{c_char, c_void};

use brimstone_core::runtime::embed::{RootVisitor, WeakUpdater};

use super::cx::Cx;
use super::store_buffer_rules;
use crate::boundary::{roots::FrameStack, value_bits};
use crate::jsapi::*;
use crate::jsgc::{RootedBase, RootedVFTable};

/// Our `JSTracer`. Servo only passes it back to us (`Call*Tracer`, `JS_UpdateWeakPointerAfterGC`), so the C++ base is never read.
#[repr(C)]
pub struct ShimTracer {
    pub base: JSTracer,
    visitor: Option<*mut dyn RootVisitor>,
    weak: Option<*mut dyn WeakUpdater>,
}

impl ShimTracer {
    /// A tracer for marking. The visitor lives for the GC callback that made this tracer.
    pub fn marking(visitor: &mut dyn RootVisitor) -> Self {
        // SAFETY: erases the visitor's lifetime; the tracer never outlives the callback (it is a local of it).
        let v: *mut (dyn RootVisitor + '_) = visitor;
        let v: *mut (dyn RootVisitor + 'static) = unsafe { ::std::mem::transmute(v) };
        ShimTracer { base: unsafe { ::std::mem::zeroed() }, visitor: Some(v), weak: None }
    }

    pub fn weak(updater: &mut dyn WeakUpdater) -> Self {
        let w: *mut (dyn WeakUpdater + '_) = updater;
        // SAFETY: as in `marking`.
        let w: *mut (dyn WeakUpdater + 'static) = unsafe { ::std::mem::transmute(w) };
        ShimTracer { base: unsafe { ::std::mem::zeroed() }, visitor: None, weak: Some(w) }
    }

    pub fn as_trc(&mut self) -> *mut JSTracer {
        (self as *mut ShimTracer).cast()
    }
}

/// # Safety
/// `trc` was made by `ShimTracer` and its callback is still running.
#[inline]
unsafe fn shim<'a>(trc: *mut JSTracer) -> &'a mut ShimTracer {
    unsafe { &mut *trc.cast::<ShimTracer>() }
}

/// Reports a value slot. Weak tracers (Servo traces through them only by mistake) update it as a weak edge instead.
#[inline]
pub unsafe fn trace_value_slot(trc: *mut JSTracer, slot: *mut u64) {
    let t = unsafe { shim(trc) };
    if let Some(v) = t.visitor {
        unsafe { (*v).value(&mut *slot) };
    } else if let Some(w) = t.weak {
        unsafe { (*w).update_value(&mut *slot) };
    }
}

/// Reports a pointer slot (object, string, symbol, BigInt, script).
#[inline]
pub unsafe fn trace_cell_slot(trc: *mut JSTracer, slot: *mut *mut c_void) {
    let t = unsafe { shim(trc) };
    if let Some(v) = t.visitor {
        unsafe { (*v).cell(&mut *slot) };
    } else if let Some(w) = t.weak {
        unsafe { (*w).update(&mut *slot) };
    }
}

/// A `jsid` slot: only string and symbol ids point into the heap.
#[inline]
pub unsafe fn trace_id_slot(trc: *mut JSTracer, slot: *mut jsid) {
    let bits = unsafe { (*slot).asBits_ } as u64;
    if value_bits::is_traced(bits) {
        unsafe { trace_value_slot(trc, slot.cast::<u64>()) };
    }
}

// `JS::RootKind`, the order of `RootedListHeads`: Object, Script, String, Symbol, BigInt, Id, Value, Traceable... Read from the
// bindings so a new kind upstream fails to compile here rather than going untraced.
const ROOT_KINDS: usize = JS::RootKind::Limit as usize;

/// Every `Rooted` on this context's stack lists.
unsafe fn trace_stack_roots(cx: *mut Cx, trc: *mut JSTracer) {
    let heads: *mut *mut RootedBase = unsafe { ::std::ptr::addr_of_mut!((*cx).rooting.stackRoots_) }.cast();
    for kind in 0..ROOT_KINDS {
        let mut r = unsafe { *heads.add(kind) };
        while !r.is_null() {
            // The rooted thing follows the base, as in `jsgc::Rooted<T>`.
            let data = unsafe { r.add(1) } as *mut c_void;
            unsafe { trace_root(kind as u32, r, data, trc) };
            r = unsafe { (*r).prev };
        }
    }
}

/// `JS::AutoGCRooter` lists (mozjs `CustomAutoRooter`): each node sits 8 bytes into a C++ object whose vtable pointer comes first;
/// `trace` is the vtable's third slot (`jsgc::CustomAutoRooterVFTable`). Servo uses only the Custom kind.
unsafe fn trace_auto_rooters(cx: *mut Cx, trc: *mut JSTracer) {
    let heads: *mut *mut JS::AutoGCRooter = unsafe { ::std::ptr::addr_of_mut!((*cx).rooting.autoGCRooters_) }.cast();
    for kind in 0..(JS::AutoGCRooterKind::Limit as usize) {
        let mut r = unsafe { *heads.add(kind) };
        while !r.is_null() {
            if kind == JS::AutoGCRooterKind::Custom as usize {
                let rooter = unsafe { (r as *mut *const crate::jsgc::CustomAutoRooterVFTable).sub(1) };
                let vtable = unsafe { *rooter };
                unsafe { ((*vtable).trace)(rooter.cast(), trc) };
            }
            r = unsafe { (*r).down };
        }
    }
}

unsafe fn trace_root(kind: u32, base: *mut RootedBase, data: *mut c_void, trc: *mut JSTracer) {
    use JS::RootKind as K;
    const OBJECT: u32 = K::Object as u32;
    const SCRIPT: u32 = K::Script as u32;
    const STRING: u32 = K::String as u32;
    const SYMBOL: u32 = K::Symbol as u32;
    const BIGINT: u32 = K::BigInt as u32;
    const ID: u32 = K::Id as u32;
    const VALUE: u32 = K::Value as u32;
    const TRACEABLE: u32 = K::Traceable as u32;
    match kind {
        OBJECT | SCRIPT | STRING | SYMBOL | BIGINT => unsafe { trace_cell_slot(trc, data.cast()) },
        ID => unsafe { trace_id_slot(trc, data.cast()) },
        VALUE => unsafe { trace_value_slot(trc, data.cast()) },
        TRACEABLE => {
            // `Rooted<T>` for a traceable T starts with its vtable pointer, then the base.
            let rooted = unsafe { (base as *mut *const RootedVFTable).sub(1) };
            let vtable = unsafe { *rooted };
            unsafe { ((*vtable).trace)(rooted.cast(), trc, c"root".as_ptr() as *const c_char) };
        },
        _ => {},
    }
}

/// All of Servo's roots: called by Brimstone at the start of every GC.
///
/// `cx` stays a raw pointer: the GC runs inside an allocation or a native, whose callers may still hold the frame values
/// (`vp`) and the context, and the trace hooks below are Servo's. Each access is a short place access through `cx`, and no
/// reference into the context is held across a hook.
pub unsafe fn trace_all_roots(cx: *mut Cx, visitor: &mut dyn RootVisitor) {
    let mut shim = ShimTracer::marking(visitor);
    let trc = shim.as_trc();
    unsafe { trace_stack_roots(cx, trc) };
    unsafe { trace_auto_rooters(cx, trc) };
    // Native frames: `vp` arrays of running natives, through the array's raw base (`FrameValues`), as the natives reach them.
    let (n, base) = unsafe { ((*cx).frames.walk_len(), (*cx).frame_values.base()) };
    for i in 0..n {
        unsafe { trace_value_slot(trc, base.add(i)) };
    }
    if let Some(e) = unsafe { (*cx).exception.as_mut() } {
        unsafe { trace_value_slot(trc, e) };
    }
    let descs = unsafe { (*cx).desc_roots.clone() };
    for &d in descs.iter() {
        unsafe { glue__CallPropertyDescriptorTracer(trc, d) };
    }
    // Servo's trap traces its own entries; the queue is out of the context meanwhile.
    let mut tasks = ::std::mem::take(unsafe { &mut (*cx).microtasks });
    let job_queue = unsafe { (*cx).job_queue };
    unsafe { tasks.trace(job_queue, trc) };
    unsafe { (*cx).microtasks = tasks };
    for e in unsafe { (*cx).script_privates.iter_mut() } {
        unsafe { trace_value_slot(trc, &mut e.0) };
        unsafe { trace_value_slot(trc, &mut e.1) };
    }
    // The scripted caller's two tables are keyed by script address too, and must follow it as it moves.
    for e in unsafe { (*cx).script_files.iter_mut() } {
        unsafe { trace_value_slot(trc, &mut e.0) };
    }
    for e in unsafe { (*cx).running_scripts.iter_mut() } {
        if let Some(bits) = &mut e.script {
            unsafe { trace_value_slot(trc, bits) };
        }
    }
    for slot in unsafe { (*cx).extra_values.iter_mut() } {
        unsafe { trace_value_slot(trc, slot) };
    }
    // The tracers can add or remove tracers (they don't in Servo); iterate over a copy.
    let extra = unsafe { (*cx).extra_roots.clone() };
    for (op, data) in extra {
        if let Some(op) = op {
            unsafe { op(trc, data) };
        }
    }
}

/// A host object's class trace hook (`JSClassOps::trace`).
pub unsafe fn trace_host_object(class: *const JSClass, obj: *mut JSObject, visitor: &mut dyn RootVisitor) {
    let ops = unsafe { (*class).cOps };
    if ops.is_null() {
        return;
    }
    if let Some(trace) = unsafe { (*ops).trace } {
        let mut shim = ShimTracer::marking(visitor);
        unsafe { trace(shim.as_trc(), obj) };
    }
}

/// The weak-pointer callbacks Servo registered, after liveness is final.
pub unsafe fn update_weak_pointers(cx: *mut Cx, updater: &mut dyn WeakUpdater) {
    // Heap<T> fields recorded by the post-write barriers (the store buffer) first: they are plain memory, no callbacks.
    update_store_buffer(updater);
    let mut shim = ShimTracer::weak(updater);
    let trc = shim.as_trc();
    // Copies, taken through `cx` before Servo's callbacks run: no reference into the context is live while they do.
    let zones = unsafe { (*cx).weak_zone_callbacks.clone() };
    for (cb, data) in zones {
        if let Some(cb) = cb {
            unsafe { cb(trc, data) };
        }
    }
    let compartments = unsafe { (*cx).weak_compartment_callbacks.clone() };
    for (cb, data) in compartments {
        if let Some(cb) = cb {
            // One compartment per context here; SpiderMonkey calls this per compartment.
            unsafe { cb(trc, ::std::ptr::null_mut(), data) };
        }
    }
}

/// The fixed array natives' `vp` live in, sized for `FrameStack`. Never reallocated (see `boundary::roots`).
///
/// Held as the raw pointer `Box::into_raw` gave, never as a `Box` or a slice: a native keeps its `vp` into the array while a
/// nested native or a GC writes other slots, and a fresh `&mut [u64]` over the array (a `Box` reborrow) would invalidate that
/// `vp`. Every access goes through `base()`, so all pointers into the array derive from the one raw pointer.
pub struct FrameValues {
    base: *mut u64,
    len: usize,
}

impl FrameValues {
    #[inline]
    pub fn base(&self) -> *mut u64 {
        self.base
    }
}

impl Drop for FrameValues {
    fn drop(&mut self) {
        // SAFETY: `base` and `len` came from `Box::<[u64]>::into_raw` in `new_frame_values`, and this is the only owner.
        unsafe { drop(Box::from_raw(::std::ptr::slice_from_raw_parts_mut(self.base, self.len))) };
    }
}

pub fn new_frame_values(capacity: usize) -> (FrameStack, FrameValues) {
    let boxed = vec![value_bits::undefined(); capacity].into_boxed_slice();
    let base = Box::into_raw(boxed).cast::<u64>();
    (FrameStack::new(capacity), FrameValues { base, len: capacity })
}

// ---- The glue's tracer functions (mozjs_sys jsglue.cpp `Call*Tracer`) ----

#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn glue__CallValueTracer(trc: *mut JSTracer, valuep: *mut JS::Heap<JS::Value>, _name: *const c_char) {
    unsafe { trace_value_slot(trc, valuep.cast()) }
}

#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn glue__CallIdTracer(trc: *mut JSTracer, idp: *mut JS::Heap<jsid>, _name: *const c_char) {
    unsafe { trace_id_slot(trc, idp.cast()) }
}

#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn glue__CallObjectTracer(trc: *mut JSTracer, objp: *mut JS::Heap<*mut JSObject>, _name: *const c_char) {
    unsafe { trace_cell_slot(trc, objp.cast()) }
}

#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn glue__CallStringTracer(trc: *mut JSTracer, strp: *mut JS::Heap<*mut JSString>, _name: *const c_char) {
    unsafe { trace_cell_slot(trc, strp.cast()) }
}

#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn glue__CallSymbolTracer(trc: *mut JSTracer, bip: *mut JS::Heap<*mut JS::Symbol>, _name: *const c_char) {
    unsafe { trace_cell_slot(trc, bip.cast()) }
}

#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn glue__CallBigIntTracer(trc: *mut JSTracer, bip: *mut JS::Heap<*mut JS::BigInt>, _name: *const c_char) {
    unsafe { trace_cell_slot(trc, bip.cast()) }
}

#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn glue__CallScriptTracer(trc: *mut JSTracer, scriptp: *mut JS::Heap<*mut JSScript>, _name: *const c_char) {
    unsafe { trace_cell_slot(trc, scriptp.cast()) }
}

#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn glue__CallFunctionTracer(trc: *mut JSTracer, funp: *mut JS::Heap<*mut JSFunction>, _name: *const c_char) {
    unsafe { trace_cell_slot(trc, funp.cast()) }
}

#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn glue__CallUnbarrieredObjectTracer(trc: *mut JSTracer, objp: *mut *mut JSObject, _name: *const c_char) {
    unsafe { trace_cell_slot(trc, objp.cast()) }
}

#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn glue__CallObjectRootTracer(trc: *mut JSTracer, objp: *mut *mut JSObject, _name: *const c_char) {
    unsafe { trace_cell_slot(trc, objp.cast()) }
}

#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn glue__CallValueRootTracer(trc: *mut JSTracer, valp: *mut JS::Value, _name: *const c_char) {
    unsafe { trace_value_slot(trc, valp.cast()) }
}

#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn glue__CallPropertyDescriptorTracer(trc: *mut JSTracer, desc: *mut JS::PropertyDescriptor) {
    unsafe {
        trace_cell_slot(trc, ::std::ptr::addr_of_mut!((*desc).getter_).cast());
        trace_cell_slot(trc, ::std::ptr::addr_of_mut!((*desc).setter_).cast());
        trace_value_slot(trc, ::std::ptr::addr_of_mut!((*desc).value_).cast());
    }
}

#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn JS_UpdateWeakPointerAfterGC(trc: *mut JSTracer, objp: *mut JS::Heap<*mut JSObject>) -> bool {
    unsafe { trace_cell_slot(trc, objp.cast()) };
    !unsafe { *objp.cast::<*mut JSObject>() }.is_null()
}

#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn JS_UpdateWeakPointerAfterGCUnbarriered(trc: *mut JSTracer, objp: *mut *mut JSObject) -> bool {
    unsafe { trace_cell_slot(trc, objp.cast()) };
    !unsafe { *objp }.is_null()
}

// ---- Barriers: the store buffer ----
// SpiderMonkey's post-write barriers on `Heap<T>` record, in the nursery's store buffer, every heap slot that points at an
// object that may move, and a minor GC updates those slots. Servo relies on it: a `Heap<*mut JSObject>` field written with
// `set` but reached by no trace hook (a timer's callback function, kept alive by a separate raw root on another field) is
// still updated when its object moves. Brimstone's copying GC moves every ordinary object at every collection, so the glue
// keeps the same buffer: each barrier adds the slot when the new value is a heap pointer and drops it otherwise (`Heap`'s
// `Drop` writes the initial value through the barrier, so freed slots leave the buffer), and `update_weak_pointers` moves
// every recorded slot after a GC. A slot whose object died is cleared (null, or undefined for a value), as for weak pointers;
// SpiderMonkey would leave it dangling. Without this a YouTube watch page crashed in a timer's callback after a GC (2026-11-07).

thread_local! {
    /// Slots holding a cell pointer (`JSObject*`, `JSString*`, `BigInt*`, `JSScript*`).
    static CELL_SLOTS: ::std::cell::RefCell<::std::collections::HashSet<usize>> =
        ::std::cell::RefCell::new(::std::collections::HashSet::new());
    /// Slots holding a `JS::Value`.
    static VALUE_SLOTS: ::std::cell::RefCell<::std::collections::HashSet<usize>> =
        ::std::cell::RefCell::new(::std::collections::HashSet::new());
}

#[inline]
fn record_cell_slot(slot: *mut c_void, next: *mut c_void) {
    let recorded = store_buffer_rules::cell_recorded(next as usize);
    // `try_with`: a Heap dropped during thread teardown, after the buffer itself, has nothing left to update.
    let _ = CELL_SLOTS.try_with(|set| {
        let mut set = set.borrow_mut();
        if recorded {
            set.insert(slot as usize);
        } else {
            set.remove(&(slot as usize));
        }
    });
}

#[inline]
fn record_value_slot(slot: *mut JS::Value, next: u64) {
    let is_pointer = store_buffer_rules::value_recorded(next);
    let _ = VALUE_SLOTS.try_with(|set| {
        let mut set = set.borrow_mut();
        if is_pointer {
            set.insert(slot as usize);
        } else {
            set.remove(&(slot as usize));
        }
    });
}

/// After a GC moved the heap: every slot the barriers recorded gets its object's new address (or null / undefined if it
/// died). Slots whose content no longer points into the heap leave the buffer.
///
/// A GC during thread teardown (a runtime dropped by a thread-local destructor) may run after the buffer's own thread-locals
/// are gone: then every `Heap` that recorded into them has already been dropped too, or will never be read again, so there is
/// nothing to update. `with` would panic there, and a panic in a thread-local destructor aborts the process (seen on Juno,
/// 2026-11-07).
pub fn update_store_buffer(updater: &mut dyn WeakUpdater) {
    let _ = CELL_SLOTS.try_with(|set| {
        set.borrow_mut().retain(|&slot| {
            let p = slot as *mut *mut c_void;
            // SAFETY: a recorded slot is a live `Heap<T>` field: its barrier added it on a write and its `Drop` removes it.
            unsafe { updater.update(&mut *p) };
            store_buffer_rules::cell_recorded(unsafe { *p } as usize)
        });
    });
    let _ = VALUE_SLOTS.try_with(|set| {
        set.borrow_mut().retain(|&slot| {
            let p = slot as *mut u64;
            // SAFETY: as above, for a `Heap<JS::Value>` field.
            unsafe { updater.update_value(&mut *p) };
            let bits = unsafe { *p };
            store_buffer_rules::value_recorded(bits)
        });
    });
}

/// Number of slots in the store buffer (cell slots, value slots), for tests and diagnostics.
pub fn store_buffer_len() -> (usize, usize) {
    (
        CELL_SLOTS.try_with(|s| s.borrow().len()).unwrap_or(0),
        VALUE_SLOTS.try_with(|s| s.borrow().len()).unwrap_or(0),
    )
}

#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn JS__HeapObjectWriteBarriers(objp: *mut *mut JSObject, _prev: *mut JSObject, next: *mut JSObject) {
    record_cell_slot(objp.cast(), next.cast());
}
#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn JS__HeapValueWriteBarriers(valuep: *mut JS::Value, _prev: *const JS::Value, next: *const JS::Value) {
    let bits = if next.is_null() { 0 } else { unsafe { (*next).asBits_ } };
    record_value_slot(valuep, bits);
}
#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn JS__HeapStringWriteBarriers(strp: *mut *mut JSString, _prev: *mut JSString, next: *mut JSString) {
    record_cell_slot(strp.cast(), next.cast());
}
#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn JS__HeapBigIntWriteBarriers(bip: *mut *mut JS::BigInt, _prev: *mut JS::BigInt, next: *mut JS::BigInt) {
    record_cell_slot(bip.cast(), next.cast());
}
#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn JS__HeapScriptWriteBarriers(scriptp: *mut *mut JSScript, _prev: *mut JSScript, next: *mut JSScript) {
    record_cell_slot(scriptp.cast(), next.cast());
}

// ---- Constant handles (`JS::NullHandleValue` & co.) ----
// SpiderMonkey points them at static constant values. The bits are value_bits::null() etc., written out because statics need
// constants; `tests::constant_handles` checks them against value_bits.

static NULL_VALUE: JS::Value = JS::Value { asBits_: 0x8004 << 48 };
static UNDEFINED_VALUE: JS::Value = JS::Value { asBits_: 0x8006 << 48 };
static TRUE_VALUE: JS::Value = JS::Value { asBits_: (0x8003 << 48) | 1 };
static FALSE_VALUE: JS::Value = JS::Value { asBits_: 0x8003 << 48 };

const fn constant_handle(v: &'static JS::Value) -> JS::HandleValue {
    JS::Handle { _phantom_0: ::std::marker::PhantomData, ptr: v }
}

pub static mut JS__NullHandleValue: JS::HandleValue = constant_handle(&NULL_VALUE);
pub static mut JS__UndefinedHandleValue: JS::HandleValue = constant_handle(&UNDEFINED_VALUE);
pub static mut JS__TrueHandleValue: JS::HandleValue = constant_handle(&TRUE_VALUE);
pub static mut JS__FalseHandleValue: JS::HandleValue = constant_handle(&FALSE_VALUE);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_handles() {
        assert_eq!(NULL_VALUE.asBits_, value_bits::null());
        assert_eq!(UNDEFINED_VALUE.asBits_, value_bits::undefined());
        assert_eq!(TRUE_VALUE.asBits_, value_bits::boolean(true));
        assert_eq!(FALSE_VALUE.asBits_, value_bits::boolean(false));
        unsafe {
            assert!((*(&raw const JS__NullHandleValue)).ptr == &NULL_VALUE as *const _);
        }
    }
}
