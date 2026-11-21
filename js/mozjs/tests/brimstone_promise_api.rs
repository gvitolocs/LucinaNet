/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Raw-API tests for the Brimstone engine's promise and microtask
// implementations (mozjs-sys/src/engine/promise.rs and jobs.rs).

use std::ptr;
use std::sync::Mutex;

use mozjs::jsapi::js;
use mozjs::jsapi::JS::{
    self, AddPromiseReactions, CallOriginalPromiseReject, CallOriginalPromiseResolve,
    CallOriginalPromiseThen, DequeueNextDebuggerMicroTask, DequeueNextRegularMicroTask,
    EnqueueDebugMicroTask, GetPromiseConstructor, GetPromiseID, GetPromiseIsHandled,
    GetPromisePrototype, GetPromiseState, GetWaitForAllPromise, HasDebuggerMicroTasks,
    HasRegularMicroTasks, IsPromiseObject, NewPromiseObject, PeekNextMicroTask, PrependMicroTask,
    PromiseRejectionHandlingState, PromiseState, PromiseUserInputEventHandlingState,
    RejectPromise, ResolvePromise, RestoreMicroTaskQueue, SaveMicroTaskQueue,
    SetAnyPromiseIsHandled, SetPromiseRejectionTrackerCallback,
    SetPromiseUserInputEventHandlingState, SetSettledPromiseIsHandled,
};
use mozjs::jsapi::{
    JSContext, JSObject, JS_NewFunction, JS_NewGlobalObject, JS_NewPlainObject, OnNewGlobalHookOption,
    Value,
};
use mozjs::glue::JS_GetPromiseResult;
use mozjs::jsval::{Int32Value, ObjectValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject as NewGlobal;
use mozjs::rust::{JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};

unsafe extern "C" fn do_nothing(
    _cx: *mut mozjs::context::RawJSContext,
    _argc: u32,
    _vp: *mut Value,
) -> bool {
    true
}

static HANDLER_CALLS: Mutex<Vec<i32>> = Mutex::new(Vec::new());
static TRACKER_CALLS: Mutex<Vec<(bool, i32)>> = Mutex::new(Vec::new());

unsafe extern "C" fn record_handler(
    _cx: *mut mozjs::context::RawJSContext,
    argc: u32,
    vp: *mut Value,
) -> bool {
    let args = mozjs::jsapi::CallArgs::from_vp(vp, argc);
    let v = if argc > 0 { args.get(0).to_int32() } else { -1 };
    HANDLER_CALLS.lock().unwrap().push(v);
    true
}

unsafe extern "C" fn tracker(
    _cx: *mut mozjs::context::RawJSContext,
    muted: bool,
    promise: JS::HandleObject,
    state: PromiseRejectionHandlingState,
    _data: *mut std::ffi::c_void,
) {
    let id = unsafe { GetPromiseID(promise) };
    TRACKER_CALLS.lock().unwrap().push((muted, state as i32));
    let _ = id;
}

fn setup() -> (JSEngine, Runtime) {
    let engine = JSEngine::init().unwrap();
    let runtime = Runtime::new(engine.handle());
    (engine, runtime)
}

/// A global, a realm and a do-nothing executor function, as Servo's `Promise::create_js_promise` makes them.
macro_rules! with_realm {
    ($runtime:expr, |$cx:ident| $body:block) => {{
        let context = $runtime.cx();
        unsafe {
            rooted!(&in(context) let global = NewGlobal(
                context,
                &SIMPLE_GLOBAL_CLASS,
                ptr::null_mut(),
                OnNewGlobalHookOption::FireOnNewGlobalHook,
                &*RealmOptions::default(),
            ));
            let mut realm = AutoRealm::new_from_handle(context, global.handle());
            let $cx = &mut realm;
            $body
        }
    }};
}

#[test]
fn new_promise_object_and_state() {
    let (_e, mut runtime) = setup();
    with_realm!(runtime, |context| {
        rooted!(&in(context) let executor = JS_NewFunction(context.raw_cx(), Some(do_nothing), 2, 0, ptr::null()));
        rooted!(&in(context) let executor_obj = unsafe { mozjs::jsapi::JS_GetFunctionObject(executor.get()) });
        rooted!(&in(context) let mut promise: *mut JSObject = ptr::null_mut());
        unsafe {
            promise.set(NewPromiseObject(context.raw_cx(), executor_obj.handle().into()));
        }
        assert!(!promise.get().is_null());
        unsafe {
            assert!(IsPromiseObject(promise.handle().into()));
            assert_eq!(GetPromiseState(promise.handle().into()), PromiseState::Pending);
            assert!(!GetPromiseIsHandled(promise.handle().into()));
            // The id is stable for one promise and distinct between two.
            let id1 = GetPromiseID(promise.handle().into());
            assert_eq!(id1, GetPromiseID(promise.handle().into()));
            rooted!(&in(context) let mut p2: *mut JSObject = ptr::null_mut());
            p2.set(NewPromiseObject(context.raw_cx(), executor_obj.handle().into()));
            assert_ne!(id1, GetPromiseID(p2.handle().into()));
            // A plain object is not a promise and reports Pending, as SpiderMonkey's maybeUnwrapIf does.
            rooted!(&in(context) let plain = JS_NewPlainObject(context.raw_cx()));
            assert!(!IsPromiseObject(plain.handle().into()));
            assert_eq!(GetPromiseState(plain.handle().into()), PromiseState::Pending);
        }
    });
}

#[test]
fn resolve_promise_and_read_result() {
    let (_e, mut runtime) = setup();
    with_realm!(runtime, |context| {
        rooted!(&in(context) let executor = JS_NewFunction(context.raw_cx(), Some(do_nothing), 2, 0, ptr::null()));
        rooted!(&in(context) let executor_obj = unsafe { mozjs::jsapi::JS_GetFunctionObject(executor.get()) });
        rooted!(&in(context) let mut promise: *mut JSObject = ptr::null_mut());
        unsafe { promise.set(NewPromiseObject(context.raw_cx(), executor_obj.handle().into())) };
        rooted!(&in(context) let v = Int32Value(42));
        unsafe {
            assert!(ResolvePromise(context.raw_cx(), promise.handle().into(), v.handle().into()));
            assert_eq!(GetPromiseState(promise.handle().into()), PromiseState::Fulfilled);
            rooted!(&in(context) let mut result = UndefinedValue());
            JS_GetPromiseResult(promise.handle().into(), result.handle_mut().into());
            assert_eq!(result.get().to_int32(), 42);
        }
    });
}

#[test]
fn reject_promise_handled_and_tracker() {
    let (_e, mut runtime) = setup();
    with_realm!(runtime, |context| {
        unsafe { SetPromiseRejectionTrackerCallback(context.raw_cx(), Some(tracker), ptr::null_mut()) };
        rooted!(&in(context) let executor = JS_NewFunction(context.raw_cx(), Some(do_nothing), 2, 0, ptr::null()));
        rooted!(&in(context) let executor_obj = unsafe { mozjs::jsapi::JS_GetFunctionObject(executor.get()) });
        rooted!(&in(context) let mut promise: *mut JSObject = ptr::null_mut());
        unsafe { promise.set(NewPromiseObject(context.raw_cx(), executor_obj.handle().into())) };
        rooted!(&in(context) let reason = Int32Value(7));
        unsafe {
            assert!(RejectPromise(context.raw_cx(), promise.handle().into(), reason.handle().into()));
            assert_eq!(GetPromiseState(promise.handle().into()), PromiseState::Rejected);
            rooted!(&in(context) let mut result = UndefinedValue());
            JS_GetPromiseResult(promise.handle().into(), result.handle_mut().into());
            assert_eq!(result.get().to_int32(), 7);
            // Rejected without a handler: not handled, and the tracker heard Unhandled.
            assert!(!GetPromiseIsHandled(promise.handle().into()));
            assert!(TRACKER_CALLS.lock().unwrap().contains(&(false, PromiseRejectionHandlingState::Unhandled as i32)));
            // Marking it handled reports Handled to the tracker.
            assert!(SetAnyPromiseIsHandled(context.raw_cx(), promise.handle().into()));
            assert!(GetPromiseIsHandled(promise.handle().into()));
            assert!(TRACKER_CALLS.lock().unwrap().contains(&(false, PromiseRejectionHandlingState::Handled as i32)));
            assert!(SetSettledPromiseIsHandled(context.raw_cx(), promise.handle().into()));
        }
    });
}

#[test]
fn original_resolve_reject_and_then() {
    let (_e, mut runtime) = setup();
    with_realm!(runtime, |context| {
        rooted!(&in(context) let v = Int32Value(9));
        rooted!(&in(context) let mut p: *mut JSObject = ptr::null_mut());
        unsafe {
            p.set(CallOriginalPromiseResolve(context.raw_cx(), v.handle().into()));
            assert!(!p.get().is_null());
            assert!(IsPromiseObject(p.handle().into()));
            assert_eq!(GetPromiseState(p.handle().into()), PromiseState::Fulfilled);
            rooted!(&in(context) let mut result = UndefinedValue());
            JS_GetPromiseResult(p.handle().into(), result.handle_mut().into());
            assert_eq!(result.get().to_int32(), 9);

            rooted!(&in(context) let r = Int32Value(3));
            rooted!(&in(context) let mut rp: *mut JSObject = ptr::null_mut());
            rp.set(CallOriginalPromiseReject(context.raw_cx(), r.handle().into()));
            assert_eq!(GetPromiseState(rp.handle().into()), PromiseState::Rejected);

            // then on the fulfilled promise returns a new promise and runs the handler as a job.
            rooted!(&in(context) let handler = JS_NewFunction(context.raw_cx(), Some(record_handler), 1, 0, ptr::null()));
            rooted!(&in(context) let handler_obj = mozjs::jsapi::JS_GetFunctionObject(handler.get()));
            rooted!(&in(context) let mut derived: *mut JSObject = ptr::null_mut());
            derived.set(CallOriginalPromiseThen(context.raw_cx(), p.handle().into(), handler_obj.handle().into(), handler_obj.handle().into()));
            assert!(!derived.get().is_null());
            assert!(IsPromiseObject(derived.handle().into()));
            js::RunJobs(context.raw_cx());
            assert!(HANDLER_CALLS.lock().unwrap().contains(&9));
        }
    });
}

#[test]
fn add_promise_reactions_runs_handler() {
    let (_e, mut runtime) = setup();
    with_realm!(runtime, |context| {
        rooted!(&in(context) let executor = JS_NewFunction(context.raw_cx(), Some(do_nothing), 2, 0, ptr::null()));
        rooted!(&in(context) let executor_obj = unsafe { mozjs::jsapi::JS_GetFunctionObject(executor.get()) });
        rooted!(&in(context) let mut promise: *mut JSObject = ptr::null_mut());
        unsafe { promise.set(NewPromiseObject(context.raw_cx(), executor_obj.handle().into())) };
        rooted!(&in(context) let handler = JS_NewFunction(context.raw_cx(), Some(record_handler), 1, 0, ptr::null()));
        rooted!(&in(context) let handler_obj = unsafe { mozjs::jsapi::JS_GetFunctionObject(handler.get()) });
        unsafe {
            assert!(AddPromiseReactions(context.raw_cx(), promise.handle().into(), handler_obj.handle().into(), handler_obj.handle().into()));
            rooted!(&in(context) let v = Int32Value(5));
            assert!(ResolvePromise(context.raw_cx(), promise.handle().into(), v.handle().into()));
            js::RunJobs(context.raw_cx());
            assert!(HANDLER_CALLS.lock().unwrap().contains(&5));
        }
    });
}

#[test]
fn promise_constructor_prototype_and_wait_for_all() {
    let (_e, mut runtime) = setup();
    with_realm!(runtime, |context| {
        unsafe {
            let ctor = GetPromiseConstructor(context.raw_cx());
            let proto = GetPromisePrototype(context.raw_cx());
            assert!(!ctor.is_null());
            assert!(!proto.is_null());
            rooted!(&in(context) let v = Int32Value(1));
            rooted!(&in(context) let mut p: *mut JSObject = ptr::null_mut());
            p.set(CallOriginalPromiseResolve(context.raw_cx(), v.handle().into()));
            // GetWaitForAllPromise over a one-element vector: a promise fulfilled with [1].
            // (The vector's layout is SpiderMonkey's: elements pointer, then length.)
            #[repr(C)]
            struct RawVector {
                elements: *mut *mut JSObject,
                length: usize,
            }
            let mut elems = [p.get()];
            let raw = RawVector { elements: elems.as_mut_ptr(), length: 1 };
            let hv = JS::HandleObjectVector { ptr: &raw as *const RawVector as *mut std::ffi::c_void };
            rooted!(&in(context) let mut all: *mut JSObject = ptr::null_mut());
            all.set(GetWaitForAllPromise(context.raw_cx(), hv));
            assert!(!all.get().is_null());
            assert!(IsPromiseObject(all.handle().into()));
            js::RunJobs(context.raw_cx());
            assert_eq!(GetPromiseState(all.handle().into()), PromiseState::Fulfilled);
        }
    });
}

#[test]
fn user_input_event_handling_state() {
    let (_e, mut runtime) = setup();
    with_realm!(runtime, |context| {
        rooted!(&in(context) let executor = JS_NewFunction(context.raw_cx(), Some(do_nothing), 2, 0, ptr::null()));
        rooted!(&in(context) let executor_obj = unsafe { mozjs::jsapi::JS_GetFunctionObject(executor.get()) });
        rooted!(&in(context) let mut promise: *mut JSObject = ptr::null_mut());
        unsafe {
            promise.set(NewPromiseObject(context.raw_cx(), executor_obj.handle().into()));
            assert!(SetPromiseUserInputEventHandlingState(
                promise.handle().into(),
                PromiseUserInputEventHandlingState::HadUserInteractionAtCreation
            ));
            // A non-promise refuses the state, as SpiderMonkey's maybeUnwrapIf does.
            rooted!(&in(context) let plain = JS_NewPlainObject(context.raw_cx()));
            assert!(!SetPromiseUserInputEventHandlingState(
                plain.handle().into(),
                PromiseUserInputEventHandlingState::DontCare
            ));
        }
    });
}

#[test]
fn microtask_queue_variants() {
    let (_e, mut runtime) = setup();
    with_realm!(runtime, |context| {
        unsafe {
            let cx = context.raw_cx();
            assert!(!HasRegularMicroTasks(cx));
            assert!(!HasDebuggerMicroTasks(cx));
            // Enqueue two regular tasks, then peek and prepend.
            let mut t1 = Value::from(Int32Value(1));
            let mut t2 = Value::from(Int32Value(2));
            let mut t0 = Value::from(Int32Value(0));
            assert!(mozjs::jsapi::JS::EnqueueMicroTask(cx, &t1));
            assert!(mozjs::jsapi::JS::EnqueueMicroTask(cx, &t2));
            assert!(HasRegularMicroTasks(cx));
            assert_eq!(PeekNextMicroTask(cx).asBits_, t1.asBits_);
            assert!(PrependMicroTask(cx, &t0));
            assert_eq!(PeekNextMicroTask(cx).asBits_, t0.asBits_);
            assert_eq!(DequeueNextRegularMicroTask(cx).asBits_, t0.asBits_);
            assert_eq!(DequeueNextRegularMicroTask(cx).asBits_, t1.asBits_);
            assert_eq!(DequeueNextRegularMicroTask(cx).asBits_, t2.asBits_);
            // Empty dequeues give null, as SpiderMonkey's DequeueNextRegularMicroTask does.
            assert_eq!(DequeueNextRegularMicroTask(cx).asBits_, mozjs::jsval::NullValue().asBits_);
            assert!(!HasRegularMicroTasks(cx));
            // The debugger queue is separate.
            let mut d = Value::from(Int32Value(9));
            assert!(EnqueueDebugMicroTask(cx, &d));
            assert!(HasDebuggerMicroTasks(cx));
            assert!(!HasRegularMicroTasks(cx));
            assert_eq!(DequeueNextDebuggerMicroTask(cx).asBits_, d.asBits_);
            assert!(!HasDebuggerMicroTasks(cx));
            // Save/restore swaps the regular queue.
            assert!(mozjs::jsapi::JS::EnqueueMicroTask(cx, &t1));
            let saved = SaveMicroTaskQueue(cx);
            assert!(saved >= 1);
            assert!(!HasRegularMicroTasks(cx));
            assert!(mozjs::jsapi::JS::EnqueueMicroTask(cx, &t2));
            RestoreMicroTaskQueue(cx, saved);
            assert!(HasRegularMicroTasks(cx));
            assert_eq!(DequeueNextRegularMicroTask(cx).asBits_, t1.asBits_);
            assert!(!HasRegularMicroTasks(cx));
            let _ = &mut t0;
        }
    });
}

static HOST_DATA_TRAP_RAN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `getHostDefinedData` that puts a fresh object in its out-handle (JSAPI lets it: the handle names a rooted location),
/// marks it with `mark: 7`, also keeps it as the global's `hd` (to find where the GC moved it), and forces a GC before it
/// returns: the out-handle's slot must be updated by that GC like any root.
unsafe extern "C" fn fresh_host_data(
    cx: *mut JSContext,
    _incumbent: JS::MutableHandle<*mut JSObject>,
    data: JS::MutableHandle<*mut JSObject>,
) -> bool {
    unsafe {
        *data.ptr = JS_NewPlainObject(cx);
        let obj = JS::Handle { _phantom_0: Default::default(), ptr: data.ptr as *const *mut JSObject };
        let seven: Value = Int32Value(7).into();
        let mut ok = mozjs::jsapi::JS_DefineProperty(
            cx,
            obj,
            c"mark".as_ptr(),
            JS::Handle { _phantom_0: Default::default(), ptr: &seven },
            mozjs::jsapi::JSPROP_ENUMERATE as u32,
        );
        let global = JS::CurrentGlobalOrNull(cx);
        // On Brimstone an object value's bits are its address, so the out-handle's slot reads as the value too.
        ok &= mozjs::jsapi::JS_DefineProperty(
            cx,
            JS::Handle { _phantom_0: Default::default(), ptr: &global },
            c"hd".as_ptr(),
            JS::Handle { _phantom_0: Default::default(), ptr: data.ptr as *const Value },
            mozjs::jsapi::JSPROP_ENUMERATE as u32,
        );
        mozjs::jsapi::JS_GC(cx, JS::GCReason::API);
        HOST_DATA_TRAP_RAN.store(true, std::sync::atomic::Ordering::SeqCst);
        ok
    }
}

/// enqueue_job with a GC inside it (in the host-data trap): the task must hold the moved object, still marked.
#[test]
fn enqueue_job_keeps_host_data_across_gc() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let global = NewGlobal(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, global.handle());
        let context = &mut realm;
        let cx = context.raw_cx();
        let traps = mozjs::glue::JobQueueTraps {
            getHostDefinedData: Some(fresh_host_data),
            getHostDefinedGlobal: None,
            runJobs: None,
            traceNonGCThingMicroTask: None,
        };
        let queue = mozjs::glue::CreateJobQueue(&traps);
        JS::SetJobQueue(cx, queue.cast());
        let options = mozjs::rust::CompileOptionsWrapper::new(&context, c"t".to_owned(), 1);
        rooted!(&in(context) let mut rval = UndefinedValue());
        let src = "var junk = []; for (var i = 0; i < 1000; i++) junk.push({ i: i }); Promise.resolve(1).then(function () {}); 1";
        assert!(mozjs::rust::evaluate_script(context, global.handle(), src, rval.handle_mut(), options).is_ok());
        assert!(HOST_DATA_TRAP_RAN.load(std::sync::atomic::Ordering::SeqCst));
        let mut entry: JS::GenericMicroTask = UndefinedValue();
        mozjs::glue::JS_DequeueNextMicroTask(cx, JS::MutableHandle { _phantom_0: Default::default(), ptr: &mut entry });
        assert!(JS::IsJSMicroTask(&entry));
        let task = JS::ToMaybeWrappedJSMicroTask(&entry);
        assert!(!task.is_null());
        rooted!(&in(context) let mut incumbent = ptr::null_mut::<JSObject>());
        rooted!(&in(context) let mut data = ptr::null_mut::<JSObject>());
        assert!(JS::MaybeGetHostDefinedDataFromJSMicroTask(task, incumbent.handle_mut().into(), data.handle_mut().into()));
        assert!(!data.get().is_null());
        rooted!(&in(context) let mut mark = UndefinedValue());
        assert!(mozjs::jsapi::JS_GetProperty(cx, data.handle().into(), c"mark".as_ptr(), mark.handle_mut().into()));
        assert_eq!(mark.get().to_int32(), 7);
        // The very object the global holds, at its address after the GC (a stale pre-GC address would differ).
        rooted!(&in(context) let mut hd = UndefinedValue());
        assert!(mozjs::jsapi::JS_GetProperty(cx, global.handle().into(), c"hd".as_ptr(), hd.handle_mut().into()));
        assert!(hd.get().is_object());
        assert_eq!(hd.get().to_object(), data.get());
        JS::SetJobQueue(cx, ptr::null_mut());
        mozjs::glue::DeleteJobQueue(queue);
    }
}
