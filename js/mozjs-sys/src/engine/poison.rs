/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The engine edge wrap (docs/edge-wrap.md, panic containment): what `#[engine_shim]` expands to, the poisoned state of a
//! context and the crash record.
//!
//! A panic below an engine `extern "C"` shim is caught at that shim. The thread's context is then **poisoned**: the
//! shim returns its failure value (`ShimFailure`), every later gated shim on the thread returns the failure value without
//! running engine code, natives and interrupts refuse (`func::call_native`, `cx::ServoHost::interrupt`), and the panic
//! payload waits in this thread's slot until the embedder's Rust code re-raises it (`take_payload`, which
//! `mozjs::panic::maybe_resume_unwind` calls), so the script thread unwinds into the embedder's own panic handling: one
//! tab is lost, the process and the other tabs live. A panic is never turned into a script-visible exception.
//!
//! One context per thread (`cx::CURRENT`), so the poisoned state is per thread; the crash record (`poisoned`) is
//! process-wide so the embedder can read it from any thread.

use ::std::any::Any;
use ::std::cell::{Cell, RefCell};
use ::std::panic::{catch_unwind, AssertUnwindSafe};
use ::std::sync::atomic::{AtomicUsize, Ordering};
use ::std::sync::{Mutex, Once};

pub use lucinanet_engine_shim::engine_shim;

// (docs/edge-wrap.md section a) With panic=abort a panic never reaches the catch: every engine panic would kill the
// whole browser. Refuse that build instead of shipping it.
#[cfg(panic = "abort")]
compile_error!(
    "mozjs_sys: the engine edge wrap (docs/edge-wrap.md) needs panic=unwind; with panic=abort any engine panic kills the \
     whole process instead of one tab"
);

/// Where a shim is: written by `#[engine_shim]` into each shim as a `static`.
pub struct Site {
    pub name: &'static str,
    pub file: &'static str,
    pub line: u32,
}

/// Why a context was poisoned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PoisonReason {
    /// A panic: a broken engine or glue invariant (the release assert / CHECK class).
    Panic,
    /// The engine's heap is exhausted (`panic!("Ran out of heap memory")`, Brimstone `gc/heap.rs:408` at pin 9c1f266f). Not an
    /// invariant break; kept apart so crash reports and telemetry can tell the two apart.
    OutOfMemory,
}

/// One poisoning, as the crash record carries it. No URL and no page text: `message` is the panic's static part only
/// (`static_message`), `location` the `file:line` of the panic (or of the shim when the panic hook did not see it).
#[derive(Clone, Debug)]
pub struct PoisonInfo {
    /// The thread's name (`Script#1`, ...), or `<unnamed>`.
    pub thread: String,
    pub message: String,
    pub location: String,
    /// The shim that caught the panic.
    pub shim: &'static str,
    pub reason: PoisonReason,
    /// The panic came from a `#[engine_shim(gc)]` shim: the process aborts right after recording it.
    pub fatal: bool,
    /// The panicking thread's backtrace (at most 40 frames), when the glue's panic hook saw the panic.
    pub backtrace: Option<String>,
}

/// The exact message of Brimstone's heap exhaustion (`gc/heap.rs:408`, `alloc_error` feature off as the glue builds it).
pub const OUT_OF_MEMORY_MESSAGE: &str = "Ran out of heap memory";

const CLEAN: u8 = 0;
const POISONED_PANIC: u8 = 1;
const POISONED_OOM: u8 = 2;

/// Records kept for `poison_records` (the first ones: the root causes); `poison_count` counts them all.
const RECORD_CAP: usize = 16;

struct HookSeen {
    location: String,
    backtrace: String,
}

/// A panic payload held for `take_payload`, whose `Drop` never runs the payload's own `Drop`. A payload that nobody
/// re-raises stays in this thread's slot until the thread exits; letting the slot drop it there would run foreign code
/// (the payload's `Drop`) outside any `catch_unwind`, at thread teardown — precisely what the wrap promises never to do
/// (docs/edge-wrap.md section d: "Payloads are never dropped by the wrap (stored or forgotten)"). So
/// the destructor `forget`s it: a payload that outlives its thread is leaked, which is safe (the thread is going away,
/// its context is already poisoned, and the allocation is a single boxed panic message) and is never a double panic.
struct Undropped(Option<Box<dyn Any + Send>>);

impl Drop for Undropped {
    fn drop(&mut self) {
        // Move the payload out and forget it: `mem::forget` runs no `Drop`, so the payload's own destructor never fires.
        ::std::mem::forget(self.0.take());
    }
}

thread_local! {
    /// This thread's context state: CLEAN or the first poisoning's reason. Never goes back to CLEAN.
    static STATE: Cell<u8> = const { Cell::new(CLEAN) };
    /// How many shims are running on this thread: the panic hook only records panics below a shim.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    /// Gated calls refused on this thread because it is poisoned.
    static SKIPPED: Cell<u64> = const { Cell::new(0) };
    /// What the glue's panic hook saw of this thread's latest panic below a shim.
    static LAST_HOOK: RefCell<Option<HookSeen>> = const { RefCell::new(None) };
    /// The panic waiting to be re-raised in a Rust frame of the embedder (`take_payload`). Also used by `mozjs::panic`.
    /// Its `Drop` forgets any payload still held rather than running it (`Undropped`).
    static PAYLOAD: RefCell<Undropped> = const { RefCell::new(Undropped(None)) };
}

static RECORDS: Mutex<Vec<PoisonInfo>> = Mutex::new(Vec::new());
static TOTAL: AtomicUsize = AtomicUsize::new(0);

/// The process's first poisoning (the root cause), for the crash record. `None` while no context was ever poisoned.
pub fn poisoned() -> Option<PoisonInfo> {
    RECORDS.lock().unwrap_or_else(|e| e.into_inner()).first().cloned()
}

/// The first [`RECORD_CAP`] poisonings of the process, in order (one per crashed context).
pub fn poison_records() -> Vec<PoisonInfo> {
    RECORDS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// How many contexts were poisoned in this process.
pub fn poison_count() -> usize {
    TOTAL.load(Ordering::Relaxed)
}

/// Whether this thread's context is poisoned, and why. The embedder's script thread may check it at its task boundary.
pub fn thread_poisoned() -> Option<PoisonReason> {
    match STATE.with(Cell::get) {
        CLEAN => None,
        POISONED_OOM => Some(PoisonReason::OutOfMemory),
        _ => Some(PoisonReason::Panic),
    }
}

/// Gated calls refused on this thread since it was poisoned (the proof that they ran no engine code).
pub fn skipped() -> u64 {
    SKIPPED.with(Cell::get)
}

/// Take the panic waiting on this thread, to `resume_unwind` it in a Rust frame of the embedder (never inside an
/// `extern "C"` frame). `mozjs::panic::maybe_resume_unwind` does exactly that.
pub fn take_payload() -> Option<Box<dyn Any + Send>> {
    PAYLOAD.try_with(|p| p.borrow_mut().0.take()).ok().flatten()
}

/// Keep `payload` for `take_payload`. The first one waiting wins: it is the root cause. A later one is forgotten, not
/// dropped, since dropping a payload runs foreign code that may panic again (an abort outside a catch). A payload that
/// is never taken is forgotten at thread exit too, by `Undropped`'s `Drop`, never dropped.
pub fn store_payload(payload: Box<dyn Any + Send>) {
    let rest = PAYLOAD.try_with(|p| match p.try_borrow_mut() {
        Ok(mut slot) if slot.0.is_none() => {
            slot.0 = Some(payload);
            None
        }
        _ => Some(payload),
    });
    match rest {
        Ok(Some(p)) => ::std::mem::forget(p),
        Ok(None) => {}
        // The slot is gone (thread teardown): the closure was not called and `payload` went with it.
        Err(_) => {}
    }
}

/// Chain the glue's panic hook once per process: it records where a panic below a shim happened and the backtrace, which
/// the payload alone does not carry. The embedder's hook (set before the first context) keeps running first, so its
/// output and its crash reporting (servoshell: `log::error!` while panicking reaches the constellation) are unchanged.
pub fn install_hook() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = ::std::panic::take_hook();
        ::std::panic::set_hook(Box::new(move |info| {
            previous(info);
            if DEPTH.try_with(Cell::get).unwrap_or(0) == 0 {
                return;
            }
            let location = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_else(|| "<unknown>".to_owned());
            let backtrace = super::crash_bt::truncate_frames(
                &::std::backtrace::Backtrace::force_capture().to_string(),
                super::crash_bt::MAX_PANIC_FRAMES,
            );
            let _ = LAST_HOOK.try_with(|h| {
                if let Ok(mut h) = h.try_borrow_mut() {
                    *h = Some(HookSeen { location, backtrace });
                }
            });
        }));
    });
}

/// What a value-returning shim returns when it fails without running (poisoned) or after a caught panic. Mirrors
/// SpiderMonkey's failure conventions: `false`, null, `None`, zero; see docs/edge-wrap.md section b for every choice.
#[diagnostic::on_unimplemented(
    message = "`{Self}` has no failure value for an engine shim",
    note = "add a `ShimFailure` impl in mozjs-sys/src/engine/poison.rs with its reason (docs/edge-wrap.md section b), or \
            write `#[engine_shim(failure = <expr>)]` on this shim"
)]
pub trait ShimFailure: Sized {
    fn failure() -> Self;
}

impl ShimFailure for () {
    #[inline(always)]
    fn failure() {}
}

impl ShimFailure for bool {
    /// SpiderMonkey's failure; with nothing pending it is the uncatchable exception (script termination).
    #[inline(always)]
    fn failure() -> bool {
        false
    }
}

/// Lengths, counts, sizes and the opaque `UniqueChars` family (bindgen makes those plain integers): zero, so that the
/// slice a caller builds from it is empty.
macro_rules! zero_failure {
    ($($t:ty),*) => {$(
        impl ShimFailure for $t {
            #[inline(always)]
            fn failure() -> $t {
                0
            }
        }
    )*};
}
zero_failure!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);

/// Hook setters return the previous hook: none.
impl<T> ShimFailure for Option<T> {
    #[inline(always)]
    fn failure() -> Option<T> {
        None
    }
}

/// GC things, opaque structures, callbacks tables, `void*` (malloc and private pointers): null, SpiderMonkey's failure.
macro_rules! null_failure {
    ($($t:ty),*) => {$(
        impl ShimFailure for *mut $t {
            #[inline(always)]
            fn failure() -> *mut $t {
                ::std::ptr::null_mut()
            }
        }
        impl ShimFailure for *const $t {
            #[inline(always)]
            fn failure() -> *const $t {
                ::std::ptr::null()
            }
        }
    )*};
}
use crate::jsapi::{js, JS};
null_failure!(
    crate::jsapi::JSObject,
    crate::jsapi::JSString,
    crate::jsapi::JSFunction,
    crate::jsapi::JSScript,
    crate::jsapi::JSLinearString,
    crate::jsapi::JSRuntime,
    crate::jsapi::JSPrincipals,
    crate::jsapi::JSContext,
    crate::jsapi::JSAutoStructuredCloneBuffer,
    crate::jsapi::JSExternalStringCallbacks,
    crate::jsapi::JSSecurityCallbacks,
    crate::jsapi::JSJitInfo,
    crate::jsapi::JSErrorFormatString,
    crate::jsapi::JSClass,
    JS::BigInt,
    JS::Symbol,
    JS::Realm,
    JS::RealmOptions,
    JS::RealmCreationOptions,
    JS::RealmBehaviors,
    JS::ReadOnlyCompileOptions,
    JS::OwningCompileOptions,
    JS::JobQueue,
    JS::FrontendContext,
    JS::EnvironmentChain,
    JS::ContextOptions,
    JS::PropertyKey,
    JS::Value,
    js::DOMCallbacks,
    crate::glue::RustJobQueue,
    ::std::ffi::c_void,
    *mut crate::jsapi::JSString,
    *mut crate::jsapi::JSObject
);

/// Element data (typed arrays, ArrayBuffers, string characters): a dangling, aligned, non-null pointer. Callers pair it
/// with a length from the same call or another shim, which is zero on failure, and Rust's `slice::from_raw_parts` needs a
/// non-null aligned pointer even for an empty slice (null there is undefined behaviour, and a precondition abort in debug
/// builds). Some handle types are bindgen aliases of these element types and must not get the dangling pointer:
/// `*const c_char` is `*const i8` or `*const u8` by target, and `JS::PersistentRootedIdVector` /
/// `JS::PersistentRootedObjectVector` are `u8`. Those shims state their failure value (`failure = ::std::ptr::null_mut()`);
/// the coverage test refuses such a return type without one (`ALIASED_RETURNS` there).
macro_rules! dangling_failure {
    ($($t:ty),*) => {$(
        impl ShimFailure for *mut $t {
            #[inline(always)]
            fn failure() -> *mut $t {
                ::std::ptr::NonNull::<$t>::dangling().as_ptr()
            }
        }
        impl ShimFailure for *const $t {
            #[inline(always)]
            fn failure() -> *const $t {
                ::std::ptr::NonNull::<$t>::dangling().as_ptr()
            }
        }
    )*};
}
dangling_failure!(u8, u16, u32, u64, i8, i16, i32, i64, f32, f64);

/// Enums and small structs returned by value: the answer that claims least.
macro_rules! value_failure {
    ($($t:ty => $v:expr;)*) => {$(
        impl ShimFailure for $t {
            #[inline(always)]
            fn failure() -> $t {
                $v
            }
        }
    )*};
}
value_failure! {
    // No frame is visible: the caller sees no stack.
    JS::SavedFrameResult => JS::SavedFrameResult::AccessDenied;
    JS::ModuleType => JS::ModuleType::Unknown;
    js::StackFormat => js::StackFormat::Default;
    crate::jsapi::JSType => crate::jsapi::JSType::JSTYPE_UNDEFINED;
    crate::jsapi::JSProtoKey => crate::jsapi::JSProtoKey::JSProto_Null;
    // Not a well-known symbol.
    JS::SymbolCode => JS::SymbolCode::UniqueSymbol;
    // The most restrictive scope: nothing process-local (shared memory, pointers) may cross it.
    JS::StructuredCloneScope => JS::StructuredCloneScope::DifferentProcess;
    JS::PromiseUserInputEventHandlingState => JS::PromiseUserInputEventHandlingState::DontCare;
    // SpiderMonkey's answer for "not a promise" (engine/promise.rs JS__GetPromiseState): nothing settles.
    JS::PromiseState => JS::PromiseState::Pending;
    JS::HeapState => JS::HeapState::Idle;
    JS::Scalar::Type => JS::Scalar::Type::MaxTypedArrayViewType;
    // The void id (PropertyKey::VoidTypeTag).
    JS::PropertyKey => JS::PropertyKey { asBits_: JS::PropertyKey_VoidTypeTag };
    // `GenericMicroTask` is a `JS::Value`: undefined, not a task.
    JS::Value => crate::jsval::UndefinedValue();
    // A null `RangedPtr`: no characters.
    JS::TwoByteCharsZ => unsafe { ::std::mem::zeroed() };
    JS::Latin1CharsZ => unsafe { ::std::mem::zeroed() };
}

/// The static part of a panic payload, for the log and the crash record: a `&'static str` payload is the program's own
/// text; a formatted one (`String`) keeps only what precedes its first `:` (std's own formats put their fixed head there:
/// "index out of bounds", "already borrowed", "called `Result::unwrap()` on an `Err` value"), since its arguments may be
/// page data. A formatted message without a `:` is withheld.
pub fn static_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        return (*s).to_owned();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return match s.split_once(':') {
            Some((head, _)) => format!("{} (arguments withheld)", head.chars().take(120).collect::<String>()),
            None => "<formatted message withheld>".to_owned(),
        };
    }
    "<non-string panic payload>".to_owned()
}

/// A gated shim: refuse at once on a poisoned thread; run the body under `catch_unwind`; on a panic poison the thread and
/// return the failure value. A body that returned normally but during which the thread was poisoned (a nested shim caught
/// a panic) also returns the failure value: its result was computed on state that is no longer trusted.
#[inline(always)]
pub fn run_gated<R, D: FnOnce() -> R, F: FnOnce() -> R>(site: &'static Site, failure: D, body: F) -> R {
    if STATE.with(Cell::get) != CLEAN {
        SKIPPED.with(|s| s.set(s.get() + 1));
        return failure();
    }
    DEPTH.with(|d| d.set(d.get() + 1));
    // SAFETY (unwind safety, not memory safety): after a panic nothing observes the engine state the body may have left
    // half-updated: the thread is poisoned below before anything else runs, and every later gated call, native and
    // interrupt on it refuses; the context is discarded as a whole (leaked at JS_DestroyContext). docs/edge-wrap.md (a).
    let result = catch_unwind(AssertUnwindSafe(body));
    DEPTH.with(|d| d.set(d.get() - 1));
    match result {
        Ok(v) if STATE.with(Cell::get) == CLEAN => v,
        Ok(_) => failure(),
        Err(payload) => {
            on_panic(site, payload, false);
            failure()
        }
    }
}

/// A `gc` shim (tracers, barriers): never gated, since the engine may still be running a collection in a frame above;
/// a panic is recorded and then aborts the process, because a collection that lost a tracer cannot be contained.
#[inline(always)]
pub fn run_gc<R, F: FnOnce() -> R>(site: &'static Site, body: F) -> R {
    // A `gc` shim is never gated: a collection started in an engine frame above may still be running, and a skipped
    // tracer would free live objects under it. So the body runs even on an already-poisoned thread (it must still
    // trace); a thread poisoned before this body started gets its value returned, since that poisoning
    // predates this shim and is handled where it happened. What must not happen is a poisoning that arose DURING this
    // body: see the epilogue below.
    let clean_at_entry = STATE.with(|s| s.get()) == CLEAN;
    DEPTH.with(|d| d.set(d.get() + 1));
    let result = catch_unwind(AssertUnwindSafe(body));
    DEPTH.with(|d| d.set(d.get() - 1));
    match result {
        Ok(v) => {
            // A gated shim nested inside this body caught a panic, poisoned the thread and returned its failure value;
            // the collection has since been tracing over a heap the engine no longer trusts. Continue only by aborting,
            // the same as a panic in the body itself: the record is written before the abort
            // (docs/edge-wrap.md section d).
            if clean_at_entry && STATE.with(|s| s.get()) != CLEAN {
                gc_poisoned_abort(site);
            }
            v
        },
        Err(payload) => {
            on_panic(site, payload, true);
            ::std::process::abort()
        }
    }
}

/// Abort on purpose after a `gc` shim observed a poisoning arise during its body: write the crash record first (with
/// `fatal`, so it reads as a GC abort), then take the process down exactly as the panic-in-a-`gc`-body path does. A
/// panic while recording is contained and its payload forgotten; the abort follows regardless.
#[cold]
#[inline(never)]
fn gc_poisoned_abort(site: &'static Site) -> ! {
    let message = "poisoning observed during a GC shim".to_owned();
    if let Err(inner) = catch_unwind(AssertUnwindSafe(|| record(site, message, PoisonReason::Panic, true))) {
        ::std::mem::forget(inner);
    }
    ::std::process::abort()
}

#[cold]
#[inline(never)]
fn on_panic(site: &'static Site, payload: Box<dyn Any + Send>, fatal: bool) {
    // Poison the thread FIRST, before anything that allocates or can panic. The message is computed
    // below and `static_message` allocates; a panic from that allocation would otherwise unwind out of this
    // `extern "C"` frame with the flag still `CLEAN` and abort the process. `STATE.with` on an alive thread-local only
    // reads and writes a `Cell<u8>`, so setting it here is the last step that allocates nothing. The reason is refined
    // once the message is known; only the first poisoning writes the byte, so a later panic keeps the root cause's.
    let first = STATE.with(|s| {
        let was_clean = s.get() == CLEAN;
        if was_clean {
            s.set(POISONED_PANIC);
        }
        was_clean
    });
    let message = static_message(&*payload);
    let reason = if message == OUT_OF_MEMORY_MESSAGE {
        PoisonReason::OutOfMemory
    } else {
        PoisonReason::Panic
    };
    if first && reason == PoisonReason::OutOfMemory {
        STATE.with(|s| s.set(POISONED_OOM));
    }
    // The rest allocates, logs and locks; a panic in it must not leave this `extern "C"` frame (an abort), and its own
    // payload is forgotten rather than dropped.
    if let Err(inner) = catch_unwind(AssertUnwindSafe(|| record(site, message, reason, fatal))) {
        ::std::mem::forget(inner);
    }
    if fatal {
        ::std::mem::forget(payload);
    } else {
        store_payload(payload);
    }
}

fn record(site: &'static Site, message: String, reason: PoisonReason, fatal: bool) {
    let seen = LAST_HOOK.try_with(|h| h.borrow_mut().take()).ok().flatten();
    let (location, backtrace) = match seen {
        Some(s) => (s.location, Some(s.backtrace)),
        None => (
            format!(
                "{}:{} (the shim; the panic hook did not see the panic)",
                site.file, site.line
            ),
            None,
        ),
    };
    let thread = ::std::thread::current().name().unwrap_or("<unnamed>").to_owned();
    let what = if fatal {
        "fatal panic in a GC shim, aborting"
    } else {
        "context poisoned"
    };
    // One stderr line even without a logger (tests, tools); the embedder's logger gets the same text.
    eprintln!(
        "lucinanet-js: {what}: {reason:?} in {} on thread '{thread}': {message} at {location}",
        site.name
    );
    log::error!(
        "lucinanet-js: {what}: {reason:?} in {} on thread '{thread}': {message} at {location}",
        site.name
    );
    super::record_poison(site.name, &message, &location);
    let info = PoisonInfo {
        thread,
        message,
        location,
        shim: site.name,
        reason,
        fatal,
        backtrace,
    };
    TOTAL.fetch_add(1, Ordering::Relaxed);
    let mut records = RECORDS.lock().unwrap_or_else(|e| e.into_inner());
    if records.len() < RECORD_CAP {
        records.push(info);
    }
    drop(records);
    if !fatal {
        // The engine may still be running script in a frame above the nested shim that caught the panic: stop it at its
        // next interrupt point, where `ServoHost::interrupt` refuses for a poisoned thread (uncatchable termination).
        let raw = super::cx::current_raw();
        if !raw.is_null() {
            // SAFETY: this thread's live context; the call touches only its `Interrupt`, by address.
            unsafe { super::cx::JS_RequestInterruptCallback(raw) };
        }
    }
}

/// Test-only switches (cargo feature `lucinanet-failpoints`, never in a browser build): make a named shim panic on its next
/// entry, and count how often each shim's body was entered on this thread.
#[cfg(feature = "lucinanet-failpoints")]
pub mod failpoint {
    use ::std::cell::{Cell, RefCell};
    use ::std::collections::HashMap;

    /// The message a failpoint panics with.
    pub const MESSAGE: &str = "lucinanet failpoint";

    thread_local! {
        static ARMED: Cell<Option<&'static str>> = const { Cell::new(None) };
        static ENTERED: RefCell<HashMap<&'static str, u64>> = RefCell::new(HashMap::new());
    }

    /// The next entry into the body of shim `name` on this thread panics with [`MESSAGE`].
    pub fn arm(name: &'static str) {
        ARMED.with(|a| a.set(Some(name)));
    }

    /// How many times the body of shim `name` was entered on this thread.
    pub fn entered(name: &str) -> u64 {
        ENTERED.with(|e| e.borrow().get(name).copied().unwrap_or(0))
    }

    /// Called by `#[engine_shim]` at the start of every body.
    pub fn enter(name: &'static str) {
        ENTERED.with(|e| *e.borrow_mut().entry(name).or_insert(0) += 1);
        if ARMED.with(|a| a.get()) == Some(name) {
            ARMED.with(|a| a.set(None));
            panic!("lucinanet failpoint");
        }
    }
}

/// Every return type of an engine `extern "C"` fn (the 848 of `engine/**/*.rs`, listed by the coverage test) has a failure
/// value, so applying `#[engine_shim]` never stops on a missing impl. The aliased handle types (`*const c_char`, the rooted
/// vectors) are the exception by design: they compile, with the element-data value, and the coverage test demands
/// `failure = ...` for them.
#[allow(dead_code)]
fn every_return_type_has_a_failure_value() {
    use crate::jsapi::*;
    use ::std::ffi::c_void;
    fn has<T: ShimFailure>() {}
    has::<()>();
    has::<bool>();
    has::<usize>();
    has::<u8>();
    has::<u16>();
    has::<u32>();
    has::<u64>();
    has::<i32>();
    has::<i64>();
    has::<*mut JSObject>();
    has::<*mut JSString>();
    has::<*mut JSFunction>();
    has::<*mut c_void>();
    has::<*const c_void>();
    has::<*mut u8>();
    has::<*mut u16>();
    has::<*mut u32>();
    has::<*mut u64>();
    has::<*mut i8>();
    has::<*mut i16>();
    has::<*mut i32>();
    has::<*mut i64>();
    has::<*mut f32>();
    has::<*mut f64>();
    has::<*const u16>();
    has::<*const JS::Latin1Char>();
    has::<*const JS::Value>();
    has::<*const jsid>();
    has::<*const *mut JSString>();
    has::<*const *mut JSObject>();
    has::<*mut JSScript>();
    has::<*mut JS::BigInt>();
    has::<*mut JS::Symbol>();
    has::<*mut JS::Realm>();
    has::<*mut JSLinearString>();
    has::<*mut JSRuntime>();
    has::<*mut JSPrincipals>();
    has::<*mut JSContext>();
    has::<*mut JSAutoStructuredCloneBuffer>();
    has::<*mut JS::RealmOptions>();
    has::<*mut JS::ReadOnlyCompileOptions>();
    has::<*mut JS::OwningCompileOptions>();
    has::<*mut JS::JobQueue>();
    has::<*mut JS::JSMicroTask>();
    has::<*mut JS::FrontendContext>();
    has::<*mut JS::EnvironmentChain>();
    has::<*mut JS::ContextOptions>();
    has::<*mut crate::glue::RustJobQueue>();
    has::<*mut JSExternalStringCallbacks>();
    has::<*const js::DOMCallbacks>();
    has::<*const JSSecurityCallbacks>();
    has::<*const JSJitInfo>();
    has::<*const JSErrorFormatString>();
    has::<*const JSClass>();
    has::<*const JS::RealmCreationOptions>();
    has::<*const JS::RealmBehaviors>();
    has::<JS::SavedFrameResult>();
    has::<JS::UniqueChars>();
    has::<JS::UniqueWideChars>();
    has::<JS::UniqueTwoByteChars>();
    has::<JS::GenericMicroTask>();
    has::<JS::ModuleType>();
    has::<JS::TwoByteCharsZ>();
    has::<JS::Latin1CharsZ>();
    has::<js::StackFormat>();
    has::<JS::Scalar::Type>();
    has::<JSType>();
    has::<JSProtoKey>();
    has::<JS::WarningReporter>();
    has::<JS::SymbolCode>();
    has::<JS::StructuredCloneScope>();
    has::<JS::PropertyKey>();
    has::<JS::PromiseUserInputEventHandlingState>();
    has::<JS::PromiseState>();
    has::<JS::NativeStackSize>();
    has::<JS::ModuleMetadataHook>();
    has::<JS::ModuleLoadHook>();
    has::<JS::HeapState>();
    has::<JS::GCSliceCallback>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_message_keeps_only_the_fixed_part() {
        let s: Box<dyn Any + Send> = Box::new("Ran out of heap memory");
        assert_eq!(static_message(&*s), OUT_OF_MEMORY_MESSAGE);
        let f: Box<dyn Any + Send> = Box::new(format!("index out of bounds: the len is {} but the index is {}", 3, 5));
        assert_eq!(static_message(&*f), "index out of bounds (arguments withheld)");
        let page: Box<dyn Any + Send> = Box::new(format!("{}", "https://example.org/secret page text"));
        assert_eq!(static_message(&*page), "https (arguments withheld)");
        let bare: Box<dyn Any + Send> = Box::new(format!("{}", "page text"));
        assert_eq!(static_message(&*bare), "<formatted message withheld>");
        let other: Box<dyn Any + Send> = Box::new(7u32);
        assert_eq!(static_message(&*other), "<non-string panic payload>");
    }

    #[test]
    fn failure_values() {
        assert!(!<bool as ShimFailure>::failure());
        assert!(<*mut crate::jsapi::JSObject as ShimFailure>::failure().is_null());
        let data = <*mut u8 as ShimFailure>::failure();
        assert!(!data.is_null());
        // An empty slice from the failure pointer is valid.
        assert!(unsafe { ::std::slice::from_raw_parts(data, 0) }.is_empty());
        assert_eq!(<usize as ShimFailure>::failure(), 0);
        assert!(<crate::jsapi::JS::WarningReporter as ShimFailure>::failure().is_none());
    }
}
