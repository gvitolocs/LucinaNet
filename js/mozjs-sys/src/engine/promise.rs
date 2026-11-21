/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Promises and the microtask queue variants. The original is `js/src/builtin/Promise.cpp` and the promise and
//! microtask functions of `js/src/jsapi.cpp` and `js/src/vm/JSContext.cpp` in mozjs_sys 153.6.0-1.
//!
//! Brimstone's `PromiseObject` carries the state, the result and the reactions, but not SpiderMonkey's extra
//! per-promise fields (the id, the handled flag, the user-input state, the allocation/resolution sites). Those
//! live here in a side table keyed by the promise's value bits; the table is an extra GC root, so the keys are
//! traced and follow the moving collector. The rejection tracker callback is stored the same way, and the
//! unhandled-rejection bookkeeping runs where SpiderMonkey runs it: when a promise is rejected without a
//! handler, and when a handler is added to an already-rejected promise (plus a sweep after each dequeued job,
//! which is where rejections made by the engine itself become visible).

use ::std::cell::RefCell;
use ::std::collections::{HashMap, VecDeque};
use ::std::ffi::c_void;

use brimstone_core::runtime::embed::{self, api};
use brimstone_core::runtime::gc::HandleScope;
use brimstone_core::runtime::{Context as Bs, Handle, Value as BsValue};

use super::cx::{current_raw, cx_of};
use super::realm::object_ptr;
use super::util::{fail, handle_of};
use crate::boundary::value_bits;
use crate::jsapi::*;

/// SpiderMonkey's per-promise fields we keep on the side.
struct PromiseInfo {
    /// The promise's value bits; traced so the key follows the moving GC.
    promise: u64,
    id: u64,
    handled: bool,
    /// Whether the rejection tracker was already told this promise is unhandled.
    notified_unhandled: bool,
    user_input: i32,
}

/// Per-context promise state: the side table, the tracker callback and the debugger microtask queue.
struct PromiseTable {
    next_id: u64,
    entries: Vec<PromiseInfo>,
    tracker: Option<(JS::PromiseRejectionTrackerCallback, *mut c_void)>,
    debug_queue: VecDeque<u64>,
    /// Stack of queues saved by `JS::SaveMicroTaskQueue`; the API hands the position back as a `u8`.
    saved_queues: Vec<VecDeque<u64>>,
    use_internal_job_queues: bool,
    stop_draining: bool,
}

impl PromiseTable {
    fn new() -> Self {
        PromiseTable {
            next_id: 1,
            entries: Vec::new(),
            tracker: None,
            debug_queue: VecDeque::new(),
            saved_queues: Vec::new(),
            use_internal_job_queues: false,
            stop_draining: false,
        }
    }

    fn entry(&mut self, bits: u64) -> &mut PromiseInfo {
        if let Some(i) = self.entries.iter().position(|e| e.promise == bits) {
            return &mut self.entries[i];
        }
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(PromiseInfo {
            promise: bits,
            id,
            handled: false,
            notified_unhandled: false,
            user_input: JS::PromiseUserInputEventHandlingState::DontCare as i32,
        });
        self.entries.last_mut().unwrap()
    }
}

thread_local! {
    static TABLES: RefCell<HashMap<usize, PromiseTable>> = RefCell::new(HashMap::new());
}

/// GC: the side table's promise keys and the debugger queue's entries move with the heap.
#[super::poison::engine_shim(gc)]
unsafe extern "C" fn trace_tables(trc: *mut JSTracer, data: *mut c_void) {
    let key = data as usize;
    TABLES.with(|t| {
        let mut t = t.borrow_mut();
        if let Some(table) = t.get_mut(&key) {
            for e in table.entries.iter_mut() {
                unsafe { super::gc::trace_value_slot(trc, &mut e.promise) };
            }
            for bits in table.debug_queue.iter_mut() {
                if value_bits::is_traced(*bits) {
                    unsafe { super::gc::trace_value_slot(trc, bits) };
                }
            }
            for q in table.saved_queues.iter_mut() {
                for bits in q.iter_mut() {
                    if value_bits::is_traced(*bits) {
                        unsafe { super::gc::trace_value_slot(trc, bits) };
                    }
                }
            }
        }
    });
}

/// The table for `cx`, registering the GC tracer the first time.
fn ensure_table(cx: *mut JSContext) {
    let key = cx as usize;
    let mut registered = false;
    TABLES.with(|t| {
        let mut t = t.borrow_mut();
        if !t.contains_key(&key) {
            t.insert(key, PromiseTable::new());
            registered = true;
        }
    });
    if registered {
        unsafe { super::cx::JS_AddExtraGCRootsTracer(cx, Some(trace_tables), cx.cast()) };
    }
}

fn with_table<R>(cx: *mut JSContext, f: impl FnOnce(&mut PromiseTable) -> R) -> R {
    ensure_table(cx);
    TABLES.with(|t| {
        let mut t = t.borrow_mut();
        f(t.get_mut(&(cx as usize)).expect("table just ensured"))
    })
}

/// The promise behind `obj`, if it is one.
fn promise_of(bs: Bs, obj: *mut JSObject) -> Option<Handle<api::PromiseObject>> {
    if obj.is_null() {
        return None;
    }
    BsValue::object(object_ptr(obj)).to_handle(bs).as_opt::<api::PromiseObject>()
}

fn obj_bits(obj: *mut JSObject) -> u64 {
    BsValue::object(object_ptr(obj)).as_raw_bits()
}

/// Calls the rejection tracker, as SpiderMonkey's `HostPromiseRejectionTracker` does.
unsafe fn fire_tracker(cx: *mut JSContext, promise: *mut JSObject, state: JS::PromiseRejectionHandlingState) {
    let cb = with_table(cx, |t| t.tracker);
    let Some((cb, data)) = cb else { return };
    let Some(cb) = cb else { return };
    let mut p = promise;
    let h = JS::HandleObject { _phantom_0: Default::default(), ptr: &mut p as *mut _ };
    unsafe { cb(cx, false, h, state, data) };
}

/// A rejected promise with no handler is reported as unhandled, once.
unsafe fn note_rejection(cx: *mut JSContext, bs: Bs, obj: *mut JSObject) {
    let Some(p) = promise_of(bs, obj) else { return };
    if api::promise_state(p) != api::PromiseStatus::Rejected {
        return;
    }
    let bits = obj_bits(obj);
    let fire = with_table(cx, |t| {
        let e = t.entry(bits);
        if e.handled || e.notified_unhandled {
            false
        } else {
            e.notified_unhandled = true;
            true
        }
    });
    if fire {
        unsafe { fire_tracker(cx, obj, JS::PromiseRejectionHandlingState::Unhandled) };
    }
}

/// A handler was added to `obj`: it is handled now, and the tracker hears so if it was rejected.
unsafe fn note_handled(cx: *mut JSContext, bs: Bs, obj: *mut JSObject) {
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    let Some(p) = promise_of(bs, obj) else { return };
    let rejected = api::promise_state(p) == api::PromiseStatus::Rejected;
    let bits = obj_bits(obj);
    let fire = with_table(cx, |t| {
        let e = t.entry(bits);
        let was = e.handled;
        e.handled = true;
        rejected && !was
    });
    if fire {
        unsafe { fire_tracker(cx, obj, JS::PromiseRejectionHandlingState::Handled) };
    }
}

/// Reports every rejected, unhandled promise in the table (rejections the engine made itself).
unsafe fn sweep_unhandled(cx: *mut JSContext) {
    let bs = unsafe { cx_of(cx) }.bs;
    let pending: Vec<u64> = with_table(cx, |t| {
        t.entries
            .iter()
            .filter(|e| !e.handled && !e.notified_unhandled)
            .map(|e| e.promise)
            .collect()
    });
    for bits in pending {
        let obj = (bits as usize as *mut crate::jsapi::JSObject).cast();
        // SAFETY: the bits are a traced object value from the table.
        unsafe { note_rejection(cx, bs, obj) };
    }
}

// ---- Promises ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__NewPromiseObject(cx: *mut JSContext, executor: JS::HandleObject) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let result = if (*executor.ptr).is_null() {
        api::new_promise(bs, realm).map(|p| (*p.as_object()).as_ptr()).map_err(|e| e.into())
    } else {
        let ctor = api::intrinsic(realm, api::Intrinsic::PromiseConstructor);
        let ex = BsValue::object(object_ptr(*executor.ptr)).to_handle(bs);
        embed::construct(bs, ctor.as_value(), &[ex], None).map(|o| (*o).as_ptr())
    };
    scope.exit();
    match result {
        Ok(p) => p.cast(),
        Err(_) => ::std::ptr::null_mut(),
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsPromiseObject(obj: JS::HandleObject) -> bool {
    let o = unsafe { *obj.ptr };
    if o.is_null() {
        return false;
    }
    let bs = embed::context_of(o.cast());
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    api::is_promise(BsValue::object(object_ptr(o)).to_handle(bs))
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetPromiseConstructor(cx: *mut JSContext) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let o = (*api::intrinsic(realm, api::Intrinsic::PromiseConstructor)).as_ptr().cast();
    scope.exit();
    o
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetPromisePrototype(cx: *mut JSContext) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let o = (*api::intrinsic(realm, api::Intrinsic::PromisePrototype)).as_ptr().cast();
    scope.exit();
    o
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetPromiseState(promise: JS::HandleObject) -> JS::PromiseState {
    let o = unsafe { *promise.ptr };
    // The script thread asks this in its main loop: the handle must not outlive the call.
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(embed::context_of(o.cast()));
    let Some(p) = promise_of(embed::context_of(o.cast()), o) else {
        // SpiderMonkey: not a promise unwraps to nothing and reports Pending.
        return JS::PromiseState::Pending;
    };
    match api::promise_state(p) {
        api::PromiseStatus::Pending => JS::PromiseState::Pending,
        api::PromiseStatus::Fulfilled => JS::PromiseState::Fulfilled,
        api::PromiseStatus::Rejected => JS::PromiseState::Rejected,
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetPromiseID(promise: JS::HandleObject) -> u64 {
    let o = unsafe { *promise.ptr };
    let bits = obj_bits(o);
    with_table(current_raw(), |t| t.entry(bits).id)
}

#[super::poison::engine_shim(failure = unsafe { (*dest.ptr).asBits_ = value_bits::undefined() })]
pub unsafe extern "C" fn glue__JS_GetPromiseResult(promise: JS::HandleObject, dest: JS::MutableHandleValue) {
    let o = unsafe { *promise.ptr };
    let bs = embed::context_of(o.cast());
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    let bits = match promise_of(bs, o).and_then(|p| api::promise_result(bs, p)) {
        Some(r) => r.as_raw_bits(),
        None => value_bits::undefined(),
    };
    unsafe { (*dest.ptr).asBits_ = bits };
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetPromiseIsHandled(promise: JS::HandleObject) -> bool {
    let o = unsafe { *promise.ptr };
    with_table(current_raw(), |t| t.entry(obj_bits(o)).handled)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetSettledPromiseIsHandled(cx: *mut JSContext, promise: JS::HandleObject) -> bool {
    let o = unsafe { *promise.ptr };
    let bs = unsafe { cx_of(cx) }.bs;
    unsafe { note_handled(cx, bs, o) };
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetAnyPromiseIsHandled(cx: *mut JSContext, promise: JS::HandleObject) -> bool {
    let o = unsafe { *promise.ptr };
    let bs = unsafe { cx_of(cx) }.bs;
    unsafe { note_handled(cx, bs, o) };
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetPromiseAllocationSite(promise: JS::HandleObject) -> *mut JSObject {
    // No stack capture on Brimstone: SpiderMonkey reports null when there is no site.
    let _ = promise;
    ::std::ptr::null_mut()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetPromiseResolutionSite(promise: JS::HandleObject) -> *mut JSObject {
    let _ = promise;
    ::std::ptr::null_mut()
}

/// `Promise.resolve` with the original constructor (`PromiseObject::unforgeableResolve`).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CallOriginalPromiseResolve(
    cx: *mut JSContext,
    resolutionValue: JS::HandleValue,
) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let ctor = api::intrinsic(realm, api::Intrinsic::PromiseConstructor);
    let key = api::property_key_from_string(bs, "resolve");
    let result = key.and_then(|k| api::get(bs, ctor, k)).and_then(|resolve| {
        let v = handle_of(bs, unsafe { (*resolutionValue.ptr).asBits_ });
        // PromiseResolve calls the constructor's own `resolve` with the constructor as `this`.
        embed::call(bs, resolve, ctor.as_value(), &[v]).map(|r| {
            if r.is_object() { (*r.as_object()).as_ptr().cast() } else { ::std::ptr::null_mut() }
        })
    });
    scope.exit();
    match result {
        Ok(p) => p,
        Err(_) => ::std::ptr::null_mut(),
    }
}

/// `Promise.reject` with the original constructor (`PromiseObject::unforgeableReject`).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CallOriginalPromiseReject(
    cx: *mut JSContext,
    rejectionValue: JS::HandleValue,
) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let result = api::new_promise(bs, realm).map(|p| {
        let v = handle_of(bs, unsafe { (*rejectionValue.ptr).asBits_ });
        api::promise_reject(bs, p, v);
        (*p.as_object()).as_ptr().cast()
    });
    scope.exit();
    match result {
        Ok(p) => p,
        Err(_) => ::std::ptr::null_mut(),
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ResolvePromise(
    cx: *mut JSContext,
    promiseObj: JS::HandleObject,
    resolutionValue: JS::HandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { *promiseObj.ptr };
    let v = handle_of(bs, unsafe { (*resolutionValue.ptr).asBits_ });
    let r = match promise_of(bs, o) {
        Some(p) => api::promise_resolve(bs, p, v),
        None => {
            let realm = embed::current_realm(bs);
            match api::new_type_error(bs, realm, "ResolvePromise called on a non-Promise object") {
                Ok(e) => Err(embed::throw_value(e.as_value())),
                Err(e) => Err(e),
            }
        },
    };
    scope.exit();
    match r {
        Ok(()) => true,
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__RejectPromise(
    cx: *mut JSContext,
    promiseObj: JS::HandleObject,
    rejectionValue: JS::HandleValue,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { *promiseObj.ptr };
    let v = handle_of(bs, unsafe { (*rejectionValue.ptr).asBits_ });
    // SpiderMonkey's PromiseObject::reject: a settled (or already resolved) promise is left alone and the call succeeds;
    // only a promise this call rejected goes to the rejection tracker.
    let rejected = match promise_of(bs, o) {
        Some(p) => api::promise_reject(bs, p, v),
        None => {
            let realm = embed::current_realm(bs);
            match api::new_type_error(bs, realm, "RejectPromise called on a non-Promise object") {
                Ok(e) => return unsafe { fail(cx, embed::throw_value(e.as_value())) },
                Err(e) => return unsafe { fail(cx, e) },
            }
        },
    };
    scope.exit();
    if rejected {
        unsafe { note_rejection(cx, bs, o) };
    }
    true
}

/// The original `%Promise.prototype.then%` (`OriginalPromiseThen`).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CallOriginalPromiseThen(
    cx: *mut JSContext,
    promise: JS::HandleObject,
    onFulfilled: JS::HandleObject,
    onRejected: JS::HandleObject,
) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let proto = api::intrinsic(realm, api::Intrinsic::PromisePrototype);
    let key = api::property_key_from_string(bs, "then");
    let result = key
        .and_then(|k| api::get(bs, proto, k))
        .and_then(|then| {
            let p = BsValue::object(object_ptr(*promise.ptr)).to_handle(bs);
            let f = if (*onFulfilled.ptr).is_null() { bs.null() } else { BsValue::object(object_ptr(*onFulfilled.ptr)).to_handle(bs) };
            let r = if (*onRejected.ptr).is_null() { bs.null() } else { BsValue::object(object_ptr(*onRejected.ptr)).to_handle(bs) };
            embed::call(bs, then, p, &[f, r]).map(|v| {
                if v.is_object() { (*v.as_object()).as_ptr().cast() } else { ::std::ptr::null_mut() }
            })
        });
    scope.exit();
    match result {
        Ok(p) => p,
        Err(_) => ::std::ptr::null_mut(),
    }
}

unsafe fn add_reactions(
    cx: *mut JSContext,
    promiseObj: JS::HandleObject,
    onFulfilled: JS::HandleObject,
    onRejected: JS::HandleObject,
    track_handled: bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let o = unsafe { *promiseObj.ptr };
    let f = if (*onFulfilled.ptr).is_null() { None } else { Some(BsValue::object(object_ptr(*onFulfilled.ptr)).to_handle(bs).as_object()) };
    let r = if (*onRejected.ptr).is_null() { None } else { Some(BsValue::object(object_ptr(*onRejected.ptr)).to_handle(bs).as_object()) };
    let result = match promise_of(bs, o) {
        Some(mut p) => p.add_then_reaction(bs, f, r, None).map(|_| ()).map_err(|e| e.into()),
        None => {
            let realm = embed::current_realm(bs);
            match api::new_type_error(bs, realm, "Promise reactions added to a non-Promise object") {
                Ok(e) => Err(embed::throw_value(e.as_value())),
                Err(e) => Err(e),
            }
        },
    };
    scope.exit();
    match result {
        Ok(()) => {
            if track_handled && !(*onRejected.ptr).is_null() {
                unsafe { note_handled(cx, bs, o) };
            }
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__AddPromiseReactions(
    cx: *mut JSContext,
    promise: JS::HandleObject,
    onFulfilled: JS::HandleObject,
    onRejected: JS::HandleObject,
) -> bool {
    unsafe { add_reactions(cx, promise, onFulfilled, onRejected, true) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__AddPromiseReactionsIgnoringUnhandledRejection(
    cx: *mut JSContext,
    promise: JS::HandleObject,
    onFulfilled: JS::HandleObject,
    onRejected: JS::HandleObject,
) -> bool {
    unsafe { add_reactions(cx, promise, onFulfilled, onRejected, false) }
}

/// `Promise.all` over a dense vector of promises (`js::GetWaitForAllPromise`).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetWaitForAllPromise(
    cx: *mut JSContext,
    promises: JS::HandleObjectVector,
) -> *mut JSObject {
    // mozjs's `GetObjectVectorAddress` hands the address of the rooted vector: { elements, length, capacity }.
    #[repr(C)]
    struct RawVector {
        elements: *mut *mut JSObject,
        length: usize,
    }
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let raw = unsafe { &*(promises.ptr as *const RawVector) };
    let result = (|| {
        let arr = api::array_with_length(bs, realm, raw.length as u64)?;
        for i in 0..raw.length {
            let o = unsafe { *raw.elements.add(i) };
            let key = api::property_key_from_index(bs, i as u32)?;
            let v = BsValue::object(object_ptr(o)).to_handle(bs);
            api::create_data_property(bs, arr.as_object(), key, v)?;
        }
        let ctor = api::intrinsic(realm, api::Intrinsic::PromiseConstructor);
        let key = api::property_key_from_string(bs, "all")?;
        let all = api::get(bs, ctor, key)?;
        // `Promise.all` requires the constructor as `this`.
        embed::call(bs, all, ctor.as_value(), &[arr.as_value()]).map(|v| {
            if v.is_object() { (*v.as_object()).as_ptr().cast() } else { ::std::ptr::null_mut() }
        })
    })();
    scope.exit();
    match result {
        Ok(p) => p,
        Err(_) => ::std::ptr::null_mut(),
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetPromiseUserInputEventHandlingState(
    promise: JS::HandleObject,
    state: JS::PromiseUserInputEventHandlingState,
) -> bool {
    let o = unsafe { *promise.ptr };
    if o.is_null() || !unsafe { JS__IsPromiseObject(promise) } {
        return false;
    }
    with_table(current_raw(), |t| t.entry(obj_bits(o)).user_input = state as i32);
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetPromiseRejectionTrackerCallback(
    cx: *mut JSContext,
    callback: JS::PromiseRejectionTrackerCallback,
    data: *mut c_void,
) {
    with_table(cx, |t| t.tracker = Some((callback, data)));
}

// ---- Microtask queue variants ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__PeekNextMicroTask(cx: *mut JSContext) -> JS::GenericMicroTask {
    let bits = unsafe { cx_of(cx) }.microtasks.queue.front().copied().unwrap_or(value_bits::null());
    JS::Value { asBits_: bits }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__PrependMicroTask(cx: *mut JSContext, entry: *const JS::GenericMicroTask) -> bool {
    unsafe { cx_of(cx) }.microtasks.queue.push_front(unsafe { (*entry).asBits_ });
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__DequeueNextRegularMicroTask(cx: *mut JSContext) -> JS::GenericMicroTask {
    let bits = unsafe { cx_of(cx) }.microtasks.queue.pop_front().unwrap_or(value_bits::null());
    JS::Value { asBits_: bits }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__DequeueNextDebuggerMicroTask(cx: *mut JSContext) -> JS::GenericMicroTask {
    let bits = with_table(cx, |t| t.debug_queue.pop_front()).unwrap_or(value_bits::null());
    JS::Value { asBits_: bits }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__EnqueueDebugMicroTask(cx: *mut JSContext, entry: *const JS::GenericMicroTask) -> bool {
    with_table(cx, |t| t.debug_queue.push_back(unsafe { (*entry).asBits_ }));
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__HasRegularMicroTasks(cx: *mut JSContext) -> bool {
    !unsafe { cx_of(cx) }.microtasks.queue.is_empty()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__HasDebuggerMicroTasks(cx: *mut JSContext) -> bool {
    with_table(cx, |t| !t.debug_queue.is_empty())
}

/// Swaps the regular queue for a fresh one and returns its 1-based position on the save stack (0: none).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SaveMicroTaskQueue(cx: *mut JSContext) -> u8 {
    let saved = ::std::mem::take(&mut unsafe { cx_of(cx) }.microtasks.queue);
    with_table(cx, |t| {
        t.saved_queues.push(saved);
        t.saved_queues.len() as u8
    })
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__RestoreMicroTaskQueue(cx: *mut JSContext, savedQueue: u8) {
    with_table(cx, |t| {
        if savedQueue >= 1 && (savedQueue as usize) <= t.saved_queues.len() {
            let saved = t.saved_queues.remove(savedQueue as usize - 1);
            unsafe { cx_of(cx) }.microtasks.queue = saved;
        }
    });
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__MaybeGetAllocationSiteFromJSMicroTask(
    entry: *mut JS::JSMicroTask,
    _out: JS::MutableHandleObject,
) -> bool {
    // No stack capture on Brimstone: no microtask has an allocation site.
    let _ = entry;
    false
}

/// `glue__JS_DequeueNextMicroTask` and `js__UseInternalJobQueues` live in jobs.rs and runtime.rs.

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__StopDrainingJobQueue(cx: *mut JSContext) {
    with_table(cx, |t| t.stop_draining = true);
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn js__RestartDrainingJobQueue(cx: *mut JSContext) {
    with_table(cx, |t| t.stop_draining = false);
}
