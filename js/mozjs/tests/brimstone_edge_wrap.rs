/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// The engine edge wrap (docs/edge-wrap.md, panic containment): a panic below an engine `extern "C"` shim poisons that thread's
// context instead of aborting the process. Needs the test-only feature `lucinanet-failpoints`:
//   cargo test -p mozjs --features lucinanet-failpoints --test brimstone_edge_wrap
// The out-of-memory and the GC-shim cases run in a child process (this binary again, one `#[ignore]`d test, by name).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::Command;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use mozjs::context::RawJSContext;
use mozjs::jsapi::{CallArgs, GCReason, JSObject, OnNewGlobalHookOption, Value, JS};
use mozjs::jsval::{Int32Value, UndefinedValue};
use mozjs::poison::{poison_records, thread_poisoned, PoisonInfo, PoisonReason, OUT_OF_MEMORY_MESSAGE};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{JS_DefineFunction, JS_NewGlobalObject};
use mozjs::rust::{evaluate_script, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};
use mozjs_sys::engine::poison::{failpoint, run_gated, run_gc, skipped, Site};

/// This thread's latest record (the process keeps every test's).
fn my_record() -> PoisonInfo {
    let me = std::thread::current().name().unwrap_or("<unnamed>").to_owned();
    poison_records()
        .into_iter()
        .rev()
        .find(|r| r.thread == me)
        .expect("a poison record for this thread")
}

fn payload_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_default()
}

/// Runs `script` in a fresh global of `runtime`, giving it the natives `poison` and `mark`. The mozjs wrapper re-raises a
/// pending panic (`maybe_resume_unwind`) when the evaluation fails, as Servo's script runner does: the result is that panic.
/// On an ALREADY-poisoned thread it stops BEFORE its first engine call: the global creation itself is a gated edge from
/// slice S6 on (`JS_NewGlobalObject` and the `RealmOptions` it takes), and those edges fail with values mozjs's own
/// wrappers assert against (a null `RealmOptions` trips the `Deref` at mozjs/src/rust.rs:135, a null global trips
/// `AutoRealm::new_from_handle`'s `unwrap` at mozjs/src/realm.rs:94). So the helper refuses at the first gated call that
/// needs no global — `JS_NewPlainObject` must answer null (docs/edge-wrap.md a) — and reports "did not run" as
/// `Ok(Err(()))` without calling anything further.
fn run_script(runtime: &mut Runtime, script: &str) -> Result<Result<(), ()>, Box<dyn std::any::Any + Send>> {
    let context = runtime.cx();
    unsafe {
        if thread_poisoned().is_some() {
            let raw = context.raw_cx();
            assert!(
                mozjs::jsapi::JS_NewPlainObject(raw).is_null(),
                "a gated shim must answer null on a poisoned thread"
            );
            return Ok(Err(())); // "the script did not run", and nothing of the engine is called again
        }
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, global.handle());
        let poison_def = JS_DefineFunction(
            &mut realm,
            global.handle().into(),
            c"poison".as_ptr(),
            Some(poison_native),
            0,
            0,
        );
        if thread_poisoned().is_some() {
            assert!(
                poison_def.is_null(),
                "`JS_DefineFunction` must be refused on a poisoned thread"
            );
            return Ok(Err(())); // "the script did not run", and nothing of the engine is called again
        }
        assert!(!poison_def.is_null());
        assert!(!JS_DefineFunction(
            &mut realm,
            global.handle().into(),
            c"mark".as_ptr(),
            Some(mark_native),
            0,
            0
        )
        .is_null());
        rooted!(&in(&mut realm) let mut rval = UndefinedValue());
        let options = CompileOptionsWrapper::new(&mut realm, c"edge_wrap.js".to_owned(), 1);
        catch_unwind(AssertUnwindSafe(|| {
            evaluate_script(&mut realm, global.handle(), script, rval.handle_mut(), options)
        }))
    }
}

static MARKS: AtomicUsize = AtomicUsize::new(0);

/// A native whose engine call fails (the failpoint panics inside `JS_NewPlainObject`) and that, like a careless binding,
/// ignores the failure and reports success.
unsafe extern "C" fn poison_native(cx: *mut RawJSContext, argc: u32, vp: *mut Value) -> bool {
    failpoint::arm("JS_NewPlainObject");
    let o = unsafe { mozjs::jsapi::JS_NewPlainObject(cx) };
    assert!(o.is_null(), "the shim that caught the panic returns null");
    let args = unsafe { CallArgs::from_vp(vp, argc) };
    args.rval().set(UndefinedValue());
    true
}

unsafe extern "C" fn mark_native(_cx: *mut RawJSContext, argc: u32, vp: *mut Value) -> bool {
    MARKS.fetch_add(1, Ordering::SeqCst);
    let args = unsafe { CallArgs::from_vp(vp, argc) };
    args.rval().set(UndefinedValue());
    true
}

/// A shim made to panic returns its failure value; the thread is poisoned with the crash record; later gated calls return
/// failure without entering their bodies; the panic is re-raised once in a Rust frame; a second context is unaffected.
#[test]
fn panicking_shim_poisons_its_context_only() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    // A clean context first: the wrap changes nothing.
    assert!(matches!(run_script(&mut runtime, "1 + 1"), Ok(Ok(()))));
    assert_eq!(thread_poisoned(), None);

    let raw = unsafe { runtime.cx().raw_cx() };
    let gc_before = failpoint::entered("JS_GC");
    let pending_before = failpoint::entered("JS_IsExceptionPending");
    failpoint::arm("JS_NewPlainObject");
    let entered_before = failpoint::entered("JS_NewPlainObject");
    let o = unsafe { mozjs::jsapi::JS_NewPlainObject(raw) };
    assert!(o.is_null(), "a pointer shim fails with null");
    assert_eq!(failpoint::entered("JS_NewPlainObject"), entered_before + 1);
    assert_eq!(thread_poisoned(), Some(PoisonReason::Panic));
    let info = my_record();
    assert_eq!(info.shim, "JS_NewPlainObject");
    assert_eq!(info.reason, PoisonReason::Panic);
    assert_eq!(info.message, failpoint::MESSAGE);
    assert!(
        info.location.contains("poison.rs"),
        "the panic's own file:line: {}",
        info.location
    );
    assert!(!info.fatal);
    assert!(info.backtrace.is_some(), "the glue's hook saw the panic");
    // Nothing script-visible: no pending exception, the uncatchable shape.
    assert!(unsafe { mozjs_sys::engine::cx::cx_of(raw) }.exception.is_none());

    // Later calls: the failure value, and no body runs.
    let skipped_before = skipped();
    assert!(!unsafe { mozjs::jsapi::JS_IsExceptionPending(raw) });
    unsafe { mozjs::jsapi::JS_GC(raw, GCReason::API) };
    assert!(unsafe { mozjs::jsapi::JS_NewPlainObject(raw) }.is_null());
    assert_eq!(failpoint::entered("JS_GC"), gc_before, "JS_GC's body never ran");
    assert_eq!(failpoint::entered("JS_IsExceptionPending"), pending_before);
    assert_eq!(failpoint::entered("JS_NewPlainObject"), entered_before + 1);
    assert_eq!(skipped(), skipped_before + 3);

    // The pending panic is re-raised once, in this Rust frame, by the mozjs helper Servo already calls.
    let resumed = catch_unwind(mozjs::panic::maybe_resume_unwind).expect_err("the engine panic is re-raised");
    assert_eq!(payload_text(&*resumed), failpoint::MESSAGE);
    mozjs::panic::maybe_resume_unwind(); // nothing left: no second panic

    // A second context on another thread is untouched.
    let parent = runtime.prepare_for_new_child();
    std::thread::Builder::new()
        .name("edge-wrap-second-context".into())
        .spawn(move || {
            let mut second = unsafe { Runtime::create_with_parent(parent) };
            assert_eq!(thread_poisoned(), None);
            // (JS_NewPlainObject needs an entered realm, so the object is made by script inside run_script's realm.)
            assert!(matches!(
                run_script(&mut second, "let o = { a: 1 }; o.a + 1"),
                Ok(Ok(()))
            ));
            assert_eq!(thread_poisoned(), None);
        })
        .unwrap()
        .join()
        .unwrap();
    // The poisoned runtime drops here. JS_DestroyContext is a gated shim, so on this poisoned thread its body never runs:
    // the heap is leaked, not finalized (review change 8a). The leak is the point of the containment: the poisoned context
    // is discarded as a unit, so no finalizer ever touches the half-updated engine state a panic left behind.
    let destroy_before = failpoint::entered("JS_DestroyContext");
    let refused_before = skipped();
    drop(runtime);
    assert_eq!(
        failpoint::entered("JS_DestroyContext"),
        destroy_before,
        "JS_DestroyContext's body must not run on the poisoned thread: the heap is leaked, not finalized"
    );
    assert!(
        skipped() > refused_before,
        "dropping the poisoned runtime must be refused at the gate (a gated call counted as skipped)"
    );
}

/// Nested: the panic is caught by the innermost shim (JS_NewPlainObject, called by a native, called by script, called by
/// JS__Evaluate2). The native ignores the failure; the engine still refuses to call any further native (`mark` never runs,
/// not even from `finally`), and the outer shim returns failure although its body completed.
#[test]
fn nested_panic_stops_the_outer_script() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    MARKS.store(0, Ordering::SeqCst);
    let evaluated_before = failpoint::entered("JS__Evaluate2");
    let r = run_script(&mut runtime, "try { poison(); mark(); } finally { mark(); }");
    let resumed = r.expect_err("the failed evaluation re-raises the engine panic");
    assert_eq!(payload_text(&*resumed), failpoint::MESSAGE);
    assert_eq!(MARKS.load(Ordering::SeqCst), 0, "no native ran on the poisoned context");
    assert_eq!(failpoint::entered("JS__Evaluate2"), evaluated_before + 1);
    assert_eq!(thread_poisoned(), Some(PoisonReason::Panic));
    assert_eq!(my_record().shim, "JS_NewPlainObject");
    // The next script on this context does not run at all.
    let skipped_before = skipped();
    assert!(matches!(run_script(&mut runtime, "mark()"), Ok(Err(()))));
    assert_eq!(MARKS.load(Ordering::SeqCst), 0);
    assert_eq!(failpoint::entered("JS__Evaluate2"), evaluated_before + 1);
    assert!(skipped() > skipped_before);
}

fn run_child(test: &str, env: &[(&str, &str)]) -> std::process::Output {
    let exe = std::env::current_exe().unwrap();
    let mut cmd = Command::new(exe);
    cmd.args(["--exact", test, "--ignored", "--nocapture", "--test-threads", "1"]);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

/// Heap exhaustion poisons with its own reason, `OutOfMemory`; the process and a second context live.
#[test]
fn heap_exhaustion_is_contained() {
    let out = run_child("oom_child", &[("LUCINANET_JS_MAX_HEAP_MB", "16")]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "child failed: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains("edge-wrap-oom-child-ok"),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("lucinanet-js: context poisoned: OutOfMemory in JS__Evaluate2"),
        "stderr:\n{stderr}"
    );
}

#[test]
#[ignore]
fn oom_child() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let r = run_script(
        &mut runtime,
        "let a = []; for (;;) a.push({ x: [1, 2, 3, 4], y: 'z' + a.length });",
    );
    let resumed = r.expect_err("the engine panic is re-raised in the caller's Rust frame");
    assert_eq!(payload_text(&*resumed), OUT_OF_MEMORY_MESSAGE);
    assert_eq!(thread_poisoned(), Some(PoisonReason::OutOfMemory));
    let info = mozjs::poison::poisoned().expect("the crash record");
    assert_eq!(info.reason, PoisonReason::OutOfMemory);
    assert_eq!(info.shim, "JS__Evaluate2");
    assert_eq!(info.message, OUT_OF_MEMORY_MESSAGE);
    assert!(info.location.contains("heap.rs"), "{}", info.location);
    // The poisoned context refuses at once.
    let raw = unsafe { runtime.cx().raw_cx() };
    let skipped_before = skipped();
    assert!(unsafe { mozjs::jsapi::JS_NewPlainObject(raw) }.is_null());
    assert_eq!(skipped(), skipped_before + 1);
    // A second context lives (with the same 16 MiB cap).
    let parent = runtime.prepare_for_new_child();
    std::thread::spawn(move || {
        let mut second = unsafe { Runtime::create_with_parent(parent) };
        assert!(matches!(
            run_script(&mut second, "let s = 0; for (let i = 0; i < 1000; i++) s += i; s"),
            Ok(Ok(()))
        ));
        assert_eq!(thread_poisoned(), None);
    })
    .join()
    .unwrap();
    println!("edge-wrap-oom-child-ok");
}

/// A `gc` shim (tracers) is never gated and a panic in it aborts the process on purpose, after the record line.
#[test]
fn gc_shim_panic_aborts_with_a_record() {
    let out = run_child("gc_child", &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(out.status.signal(), Some(libc::SIGABRT), "stderr:\n{stderr}");
    }
    assert!(
        stderr.contains("lucinanet-js: fatal panic in a GC shim, aborting: Panic in glue__CallValueTracer"),
        "{stderr}"
    );
}

#[test]
#[ignore]
fn gc_child() {
    failpoint::arm("glue__CallValueTracer");
    unsafe { mozjs_sys::engine::gc::glue__CallValueTracer(ptr::null_mut(), ptr::null_mut(), ptr::null()) };
    unreachable!("a gc shim's panic aborts");
}

/// A `gc` shim is never gated: entered on an already-poisoned thread it still runs its body, so a live collection keeps
/// tracing (review change 8b, docs/edge-wrap.md section d). And because the poisoning predates this body, `run_gc` returns
/// the body's value rather than aborting — change 4 aborts only on a poisoning that arises DURING a body. Exercises
/// `poison::run_gated` / `poison::run_gc` directly, so no heap is involved.
#[test]
fn gc_shim_enters_its_body_on_a_poisoned_thread() {
    static GATED: Site = Site { name: "test-gated-shim", file: file!(), line: line!() };
    static GC: Site = Site { name: "test-gc-shim", file: file!(), line: line!() };

    // Poison this thread with a gated shim whose body panics.
    let failed = run_gated::<i32, _, _>(&GATED, || -1, || panic!("poison on purpose"));
    assert_eq!(failed, -1, "a panicked gated shim returns its failure value");
    assert_eq!(thread_poisoned(), Some(PoisonReason::Panic));

    // A gc shim entered afterwards still runs its body: the GC must trace even on a poisoned thread. Its value comes
    // back, not an abort, because this thread was already poisoned before the body started (the poisoning predates it).
    let mut traced = false;
    let value = run_gc(&GC, || {
        traced = true;
        42
    });
    assert!(traced, "a gc shim ENTERS its body on an already-poisoned thread");
    assert_eq!(value, 42, "and returns the body's value: no abort for a poisoning that predates this body");
}

/// With change 4, a `gc` shim whose body poisons the thread (a gated shim nested inside it catches a panic) aborts, after
/// writing the crash record: a collection that lost a tracer cannot be contained, and must not return a value computed
/// over a heap the engine no longer trusts.
#[test]
fn gc_shim_aborts_on_a_poisoning_arising_during_its_body() {
    let out = run_child("gc_poison_during_body_child", &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(out.status.signal(), Some(libc::SIGABRT), "stderr:\n{stderr}");
    }
    assert!(
        stderr.contains("lucinanet-js: fatal panic in a GC shim, aborting: Panic in test-gc-shim"),
        "the fatal record names the gc shim that observed the poisoning arising\n{stderr}"
    );
}

#[test]
#[ignore]
fn gc_poison_during_body_child() {
    static OUTER_GC: Site = Site { name: "test-gc-shim", file: file!(), line: line!() };
    static INNER_GATED: Site = Site { name: "test-inner-gated", file: file!(), line: line!() };
    // The gc body is clean at entry, then poisons the thread through a nested gated shim and returns normally. run_gc's
    // epilogue sees that poisoning arose DURING the body and aborts.
    let v = run_gc::<i32, _>(&OUTER_GC, || {
        run_gated::<i32, _, _>(&INNER_GATED, || -1, || panic!("poisoning arising inside a gc body"))
    });
    unreachable!("run_gc aborts before returning a value over a heap it no longer trusts; it returned {v}");
}

/// Change 6: a payload stored for `take_payload` but never taken must NOT have its own destructor run when its thread
/// exits. Dropping it at teardown would run foreign `Drop` code outside any `catch_unwind` — the very abort the wrap
/// exists to prevent. `PAYLOAD`'s `Undropped` forgets any payload still held, so one that outlives its thread is leaked
/// (the thread is going away), never dropped.
#[test]
fn payload_survives_its_thread_undropped() {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    // A payload whose `Drop` flips `dropped`: if the slot drops it at thread exit, the flag goes up.
    struct DropWatch(Arc<AtomicBool>);
    impl Drop for DropWatch {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    static SITE: Site = Site { name: "test-payload-drop-watch", file: file!(), line: line!() };

    let dropped = Arc::new(AtomicBool::new(false));
    let watch = dropped.clone();
    std::thread::spawn(move || {
        // A gated shim whose body panics with the watched payload: `on_panic` stores it for `take_payload` (the
        // non-fatal path). The thread then exits WITHOUT taking it, so only `Undropped`'s Drop can run here.
        let w = watch;
        run_gated::<i32, _, _>(&SITE, || -1, || std::panic::panic_any(DropWatch(w)));
        assert_eq!(thread_poisoned(), Some(PoisonReason::Panic), "the shim poisoned as usual");
        // No take_payload: the payload is still in the slot when the thread tears down.
    })
    .join()
    .unwrap();

    assert!(
        !dropped.load(Ordering::SeqCst),
        "an untaken payload must be forgotten at thread exit, not dropped (review change 6)"
    );
}

/// Poison the calling thread through `poison::run_gated` directly, the way `gc_shim_enters_its_body_on_a_poisoned_thread`
/// does: no context and no heap involved, so the three tests below check only the value a refused gate hands back. Each
/// runs on its own thread, since `.cargo/config.toml` pins `RUST_TEST_THREADS=1` and one poisoned test thread would carry
/// into every later test of this file.
fn poison_this_thread() {
    static SITE: Site = Site { name: "test-refused-failure-value", file: file!(), line: line!() };
    let refused = run_gated::<i32, _, _>(&SITE, || -1, || panic!("poison on purpose"));
    assert_eq!(refused, -1, "the panicking gated shim returns its failure value");
    assert_eq!(thread_poisoned(), Some(PoisonReason::Panic), "and poisons this thread");
}

/// Review change (1): the refused `JS__GetWellKnownSymbolKey` answers the void id THIS tree uses (`id_bits::void_id`,
/// Brimstone's empty value), not the bindgen `PropertyKey_VoidTypeTag` (2). In the NaN-box a word with the top 16 bits
/// clear and a nonzero payload is a heap pointer, so `2` reads as a GC thing: `jsid::is_void` and
/// `glue__RUST_JSID_IS_VOID` do not accept it and a later trace follows it.
#[test]
fn refused_well_known_symbol_key_is_the_void_id() {
    std::thread::Builder::new()
        .name("edge-wrap-void-key".into())
        .spawn(|| {
            poison_this_thread();
            let key =
                unsafe { mozjs_sys::engine::values::JS__GetWellKnownSymbolKey(ptr::null_mut(), JS::SymbolCode::UniqueSymbol) };
            assert_eq!(
                key.asBits_,
                mozjs_sys::jsid::VoidId().asBits_,
                "a refused PropertyKey must be JSID_VOID, the word `jsid::is_void` and `glue__RUST_JSID_IS_VOID` test"
            );
            assert!(key.is_void(), "the refused key must read as \"no key\", asBits_ {:#x}", key.asBits_);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Review changes (2), (3) and (4): `glue__RUST_INTERNED_STRING_TO_JSID`, `glue__RUST_SYMBOL_TO_JSID` and
/// `glue__int_to_jsid` return `()` and write nothing, so their ONLY result is the out-param. A refused call used to leave
/// the caller's word there: a caller that had zeroed the id (mozjs/tests/brimstone_proxy.rs:69-71) kept `asBits_ == 0`,
/// which `is_void` rejects, and a caller that had not kept the id it put in and goes on using it — there is no bool to
/// ignore. Each now writes the same void id as change (1).
#[test]
fn refused_id_shims_write_the_void_id() {
    std::thread::Builder::new()
        .name("edge-wrap-void-id".into())
        .spawn(|| {
            poison_this_thread();
            let void_bits = mozjs_sys::jsid::VoidId().asBits_;
            let mut id = mozjs_sys::jsid::IntId(7); // a live int id: a value the refusal left behind would show
            let mh: JS::MutableHandleId = JS::MutableHandle { _phantom_0: Default::default(), ptr: &mut id };
            unsafe { mozjs_sys::engine::property::glue__RUST_INTERNED_STRING_TO_JSID(ptr::null_mut(), ptr::null_mut(), mh) };
            assert_eq!(id.asBits_, void_bits, "a refused `glue__RUST_INTERNED_STRING_TO_JSID` writes the void id to `id`");

            let mut id = mozjs_sys::jsid::IntId(7);
            let mh: JS::MutableHandleId = JS::MutableHandle { _phantom_0: Default::default(), ptr: &mut id };
            unsafe { mozjs_sys::engine::property::glue__RUST_SYMBOL_TO_JSID(ptr::null_mut(), mh) };
            assert_eq!(id.asBits_, void_bits, "a refused `glue__RUST_SYMBOL_TO_JSID` writes the void id");

            let mut id = mozjs_sys::jsid::IntId(7);
            let mh: JS::MutableHandleId = JS::MutableHandle { _phantom_0: Default::default(), ptr: &mut id };
            unsafe { mozjs_sys::engine::property::glue__int_to_jsid(7, mh) };
            assert_eq!(id.asBits_, void_bits, "a refused `glue__int_to_jsid` writes the void id");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Review changes (5), (6), (7) and (8): `()` shims whose only result is a value out-param write UNDEFINED on the refusal
/// — the word each body writes on its own miss path (`promise.rs:296` a non-settled promise, `module.rs:1066` a non-module
/// object, `script.rs:288` a missing private entry, `jobs.rs:177` an empty queue) — instead of leaving the caller's stale
/// value, which the wrappers read unconditionally. `glue__JS_DequeueNextMicroTask` still does not pop the queue: the pop
/// is a side effect and the handle is the only result.
#[test]
fn refused_value_out_param_shims_write_undefined() {
    std::thread::Builder::new()
        .name("edge-wrap-undefined-out-param".into())
        .spawn(|| {
            poison_this_thread();
            let undefined = UndefinedValue().asBits_;
            let mut value = Int32Value(7); // a stale word: a refusal that writes nothing leaves this visible
            let obj: *mut JSObject = ptr::null_mut();
            let promise: JS::HandleObject = JS::Handle { _phantom_0: Default::default(), ptr: &obj };
            let dest = JS::MutableHandle { _phantom_0: Default::default(), ptr: &mut value };
            unsafe { mozjs_sys::engine::promise::glue__JS_GetPromiseResult(promise, dest) };
            assert_eq!(value.asBits_, undefined, "a refused `glue__JS_GetPromiseResult` writes undefined to `dest`");

            let mut value = Int32Value(7);
            let dest = JS::MutableHandle { _phantom_0: Default::default(), ptr: &mut value };
            unsafe { mozjs_sys::engine::module::glue__JS_GetModulePrivate(ptr::null_mut(), dest) };
            assert_eq!(value.asBits_, undefined, "a refused `glue__JS_GetModulePrivate` writes undefined to `dest`");

            let mut value = Int32Value(7);
            let dest = JS::MutableHandle { _phantom_0: Default::default(), ptr: &mut value };
            unsafe { mozjs_sys::engine::script::glue__JS_GetScriptPrivate(ptr::null_mut(), dest) };
            assert_eq!(value.asBits_, undefined, "a refused `glue__JS_GetScriptPrivate` writes undefined to `dest`");

            let mut value = Int32Value(7);
            let dest = JS::MutableHandle { _phantom_0: Default::default(), ptr: &mut value };
            unsafe { mozjs_sys::engine::jobs::glue__JS_DequeueNextMicroTask(ptr::null_mut(), dest) };
            assert_eq!(value.asBits_, undefined, "a refused `glue__JS_DequeueNextMicroTask` writes undefined to `result`");
        })
        .unwrap()
        .join()
        .unwrap();
}
