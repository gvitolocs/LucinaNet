# The engine edge wrap

Every engine `extern "C"` function of `mozjs-sys/src/engine/` catches a panic at its edge,
**poisons** its thread's context and returns a failure value; the script thread then unwinds in a Rust frame of the
embedder into Servo's own panic handling. One tab is lost; the process and the other tabs live. A panic never becomes a
script-visible exception (by the precedent of both reference browsers: a trusted internal failure
is fatal for that content, as a release assert or a `CHECK` is).

Why poisoning and not an exception: after a panic the engine state is not trusted. Handle scopes are entered and exited
by hand, so an unwind skips `scope.exit()` (`engine/module.rs`, `JS__ModuleEvaluate`: `HandleScope::enter` then
`scope.exit()`); the heap may be mid-allocation or mid-collection. The engine's own rule, quoted in
`third_party/brimstone/docs/engine-invariants.md:346-356` from the engine's `docs/embedding.md:511-520`: "A panic that
unwinds out of a GC leaves the heap half moved; the context must not be used afterwards." The same document's
enforcement register lists this very wrap as the missing check (`engine-invariants.md:64`, row (c): "No panic crosses an
`extern "C"` entry point in the glue ... ordered").

Code: `mozjs-sys/engine-shim/src/lib.rs` (the attribute), `mozjs-sys/src/engine/poison.rs` (what it expands to, the
poisoned state, the crash record), `mozjs/src/panic.rs` (re-raise), `mozjs-sys/tests/engine_shim_coverage.rs` (the static
check), `mozjs/tests/brimstone_edge_wrap.rs` (behaviour).

## The mechanism end to end

1. A shim is written `#[super::poison::engine_shim] pub unsafe extern "C" fn X(..) -> R { body }`. The attribute keeps the
   signature and runs `body` as a closure inside `poison::run_gated` (`poison.rs`, `run_gated`).
2. Prologue: if this thread is poisoned, return `R::failure()` at once; the body (engine code) never runs.
3. The body runs under `catch_unwind`. On a panic, `on_panic` sets the thread state (no allocation), then, inside its own
   `catch_unwind`, records `PoisonInfo` (thread, static message, panic `file:line`, backtrace from the glue's chained panic
   hook, reason), writes one `lucinanet-js: context poisoned: ...` line to stderr and the same through `log::error!`, a
   `poison` line to `LUCINANET_JS_MISSING_LOG`, requests an engine interrupt, and keeps the payload in this thread's slot.
4. The shim returns `R::failure()`: no pending exception is set, so for Servo it is SpiderMonkey's uncatchable exception
   (false or null with nothing pending): no `catch` or `finally` in the page runs.
5. Engine frames above a nested shim (script running a native that called the shim) are stopped: natives are refused
   (`func::call_native`), interrupts refuse (`cx::ServoHost::interrupt`), and every outer gated shim returns its failure
   value even if its body completed (epilogue of `run_gated`).
6. The embedder's panic hook already ran at the original panic, before any unwinding: servoshell's hook prints the message
   and calls `error!` while panicking (`third_party/servo/ports/servoshell/panic_hook.rs:52`), which Servo's logger turns into
   `LogEntry::Panic` (`components/constellation/logging.rs:121`), which the constellation handles as a panic of that event
   loop (`constellation.rs:3182-3183` -> `handle_panic` `:3024`): crash page for its webviews, other event loops untouched;
   in `hard_fail` mode the hook raises SIGSEGV (`panic_hook.rs:47`) and the constellation exits 1 (`constellation.rs:3031-3035`):
   the non-zero exit of headless runs. The catch does not hide the panic from any of this.
7. The script thread unwinds when its Rust code calls `mozjs::panic::maybe_resume_unwind()` (`mozjs/src/panic.rs`), which
   now takes the engine's payload too and `resume_unwind`s it in that Rust frame. `resume_unwind` does not run the hook
   again, so there is no second report and no second abort. Servo calls it today after classic scripts
   (`components/script/scripting/script_execution.rs:318`, `globalscope.rs:2649`, generated callbacks `codegen.py:9406`,
   mozjs `rust.rs:435`); the module and job paths need the companion change (open point 1).

The accessor: `mozjs_sys::engine::poison::poisoned() -> Option<PoisonInfo>` (re-exported as `mozjs::poison::poisoned`):
the process's first poisoning (the root cause), readable from any thread. Also `poison_records() -> Vec<PoisonInfo>` (the
first 16), `poison_count() -> usize`, and `thread_poisoned() -> Option<PoisonReason>` for the calling thread.

```rust
pub struct PoisonInfo {
    pub thread: String,          // "Script#1", or "<unnamed>"
    pub message: String,         // the static part only (below)
    pub location: String,        // file:line of the panic, from the glue's hook; else the shim's file:line, marked so
    pub shim: &'static str,      // the shim that caught it
    pub reason: PoisonReason,    // Panic | OutOfMemory
    pub fatal: bool,             // a `gc` shim: the process aborts right after the record
    pub backtrace: Option<String>, // at most 40 frames
}
```

`message` is the panic's static part (`poison::static_message`): a `&'static str` payload (any
`panic!("literal")`, `Option::unwrap` on `None`, Brimstone's "Ran out of heap memory") is kept as is; a formatted payload
(`String`) keeps only what precedes its first `:`, followed by "(arguments withheld)" ("index out of bounds", "already
borrowed", "called `Result::unwrap()` on an `Err` value"); a formatted payload without `:` is withheld whole. No URL is
read anywhere in the wrap.

That truncation is a heuristic, not a privacy guarantee, and the design does not rest on one: what precedes the first `:`
can be page data, as open point 4 says of `panic!("bad key {key}: ...")`, which keeps `bad key <data>`. What bounds the
exposure is not the shape of the message but where it is allowed to go.

**Decision 6 (2026-11-17): the full panic text is local only.** It is allowed in exactly two places, where the
person debugging sits: the local session log, and the crash page (step 6: the embedder's panic hook prints the message and
calls `error!`, which becomes `LogEntry::Panic`, which the constellation turns into the crash page). Anything an agent
reads, or that can leave the machine - a crash summary, a flaw report, a crash-frame extract - carries the shim name, the
`file:line` location and the reason (`Panic` / `OutOfMemory`), and never the message text.

The channels the glue writes today are all local, and each of them carries the message: the stderr line
`lucinanet-js: context poisoned: <reason> in <shim> on thread '<thread>': <message> at <location>` and the `log::error!`
of the same text (`poison.rs`, `record`), which is what reaches the session log; the `poison` line
`<unix_ms>\tpoison\t<shim>: <message>\t<location>` appended to `LUCINANET_JS_MISSING_LOG` (`engine::record_poison`); and
`PoisonInfo.message`, what `engine::poisoned()` and `poison_records()` hand to the embedder and what the crash page
renders. So decision 6 is a rule for the reader of those, not a filter in the writer: an extract destined for an agent or
for off-machine reporting drops `message` and quotes only shim, location and reason. This repository has no
crash-summary, flaw-report or crash-frame-extract tool yet, so the rule binds the one that gets written; the `panic!`
with-arguments lint of open point 4 stays later, as the decision says.

## (a) panic=unwind, the ABI, and unwind safety

* **Build-time assertion.** `poison.rs` has `#[cfg(panic = "abort")] compile_error!(..)`: `mozjs_sys` does not compile
  under `panic=abort`, so a profile that sets it cannot ship a browser where every engine panic kills the process. Today no
  browser profile sets it (`Cargo.toml` profile `juno` and the Servo profiles); the two glue tools that do (`tools/wasm-inspect`, `tools/jit-probe`) do not depend on `mozjs_sys`.
* **`extern "C"`, not `extern "C-unwind"`.** The shims are called from Rust through the generated bindings
  (`jsapi_gen.rs`: `const _: unsafe extern "C" fn(..) = crate::engine::api::X; pub use .. as X;`), and engine code calls
  them back through Servo's `extern "C"` callbacks; a `C-unwind` shim would let a panic travel into Servo frames that are
  `extern "C"` (natives, hooks) and abort there, which is today's crash (`panic_cannot_unwind` below `JS__ModuleEvaluate`).
  Catching at the shim is the only point every path crosses. The macro refuses any ABI but `"C"`; the coverage test
  refuses `extern "C-unwind"` in `engine/`.
* **`catch_unwind(AssertUnwindSafe(..))`.** The assertion is sound because nothing observes the state a panicking body
  leaves: the thread is poisoned before anything else runs, every later gated shim, native and interrupt on it refuses,
  and the context is discarded as a whole: `JS_DestroyContext` is gated, so a poisoned heap is leaked, never finalized
  (one heap per crashed tab; tested by `panicking_shim_poisons_its_context_only`, which asserts `JS_DestroyContext`'s body
  does not run on the poisoned thread). Only the glue's own bookkeeping outside the engine is touched
  after a panic, and it is ordered so the state survives a panic in that bookkeeping: `on_panic` first sets the
  poison byte — a `thread_local` `Cell<u8>` on the already-mapped thread stack, so the write allocates nothing
  and cannot fault — and only then does it allocate the message, take the record mutex, and touch the `thread_local`
  `RefCell` payload and the `lucinanet-failpoints` `RefCell<HashMap>` entry counter. A panic in that second group loses at
  most the record (it runs in its own `catch_unwind`), never the poisoning.
* **Cost.** `catch_unwind` costs nothing on the normal path (landing pads); the gate is one thread-local byte load and a
  branch. The `lucinanet-failpoints` counters exist only in that test feature.

## (b) Failure values

`trait ShimFailure { fn failure() -> Self; }` (`poison.rs`), with
`#[diagnostic::on_unimplemented]`: a shim whose return type has no impl does not compile, and the message says where to
add one. `poison::every_return_type_has_a_failure_value` names every return type of the 848 functions (except the aliased ones below, which state their value), so applying the
attribute never stops on a missing impl.

| Return type | Failure | Why |
|---|---|---|
| `bool` (331 fns) | `false` | SpiderMonkey's failure; with nothing pending, the uncatchable exception |
| `()` (182) | nothing | the call is skipped |
| pointers to GC things, opaque structs, callback tables, `void*` | null | SpiderMonkey's failure; callers check null |
| pointers to element data (`u8 u16 u32 u64 i8 i16 i32 i64 f32 f64`, `Latin1Char`) | `NonNull::dangling()` — but see (b.1) | dangling is only correct where the caller feeds the pointer to `slice::from_raw_parts` together with a length it already knows is 0 (a skipped `()` length shim leaves the caller's pre-zeroed 0): that is the two string char getters `JS_GetLatin1StringCharsAndLength` / `JS_GetTwoByteStringCharsAndLength`, read by `mozjs/src/conversions.rs:589-594, :618-621`, which zeroes the length first and `assert!(!chars.is_null())` — a null there would make mozjs's own assert panic inside the calling native, a second panic for nothing. Where a function's OWN miss path is null and its caller null-checks the return to mean "no data", dangling is wrong (read as a live pointer): those shims override to null, see (b.1) |
| `*const c_char`, `*mut JS::PersistentRootedIdVector`, `*mut JS::PersistentRootedObjectVector` | none by type: the shim must write `#[engine_shim(failure = ::std::ptr::null_mut())]` (or `null()`) | bindgen aliases of element types (`c_char` is `i8` or `u8` by target; the two rooted vectors are `u8`), so by type they would get the dangling element pointer; the coverage test (`ALIASED_RETURNS`) refuses such a return without `failure =` |
| integers (`usize u32 u64 u16 i32 i64 u8`, and bindgen's opaque `UniqueChars`=`u64`, `UniqueTwoByteChars`=`u8`) | 0 | lengths and counts: an empty result; `UniqueChars` 0 is the null owner |
| `Option<fn>` hooks (`WarningReporter`, `ModuleLoadHook`, ...) | `None` | setters return the previous hook |
| `JS::Value` (`GenericMicroTask`) | undefined | not a task |
| `JS::PropertyKey` | the void id | no key |
| `SavedFrameResult` | `AccessDenied` | no frame visible |
| `ModuleType` | `Unknown` | |
| `JSType` | `JSTYPE_UNDEFINED` | |
| `JSProtoKey` | `JSProto_Null` | |
| `SymbolCode` | `UniqueSymbol` | not well-known |
| `StructuredCloneScope` | `DifferentProcess` | the most restrictive scope |
| `PromiseState` | `Pending` | SpiderMonkey's answer for a non-promise (`engine/promise.rs`, `JS__GetPromiseState`) |
| `PromiseUserInputEventHandlingState` | `DontCare` | |
| `HeapState` | `Idle` | |
| `Scalar::Type` | `MaxTypedArrayViewType` | not a view type |
| `TwoByteCharsZ`, `Latin1CharsZ` | zeroed (null `RangedPtr`) | no characters |
| `JS::CallArgs` | none | its only function (`glue__JS_CallArgsFromVp`) is exempt: pure pointer arithmetic |

**Values a caller could take for success.** `0` lengths, `Pending`, `Idle`, `JSTYPE_UNDEFINED`, a dangling data pointer
and `true` for nothing. They are acceptable because a poisoned thread runs no more engine code and no more script: a wrong
but plausible value reaches only Servo's Rust, which calls further shims (which fail) until it reaches a resume point and
unwinds. They are chosen to be the values that make Servo do least (empty data, nothing settles, nothing to trace) and
never undefined behaviour. A shim whose plausible value would make Servo do something harmful takes an explicit
`failure = ...` (none found in the prototype set; the slices check this per file, section f).

**(b.1) Element-data getters whose own miss path is null override the dangling value.** The type
default for an element-pointer return is `NonNull::dangling()`, which is right only for the two string char getters above
(a caller that pairs it with a length it already knows is 0 and rejects null). Every OTHER function that returns element
data directly reports "no data" as **null**, which its caller null-checks; for those the dangling default would be read
as a live pointer, so the wrapping slice must write `#[engine_shim(failure = ::std::ptr::null_mut())]`. The full list of
affected functions (all in `typed.rs`; none is wrapped yet, so this is a rule the `typed.rs` slice obeys, enforced by
`NULL_DATA_RETURNS` in `engine_shim_coverage.rs`, done before the typed-array and string slices): `JS__GetArrayBufferData`,
`JS__GetArrayBufferMaybeSharedData`, `JS__GetSharedArrayBufferData`, `JS_GetArrayBufferViewFixedData`,
`JS_GetInt8ArrayData`, `JS_GetUint8ArrayData`, `JS_GetUint8ClampedArrayData`, `JS_GetInt16ArrayData`,
`JS_GetUint16ArrayData`, `JS_GetInt32ArrayData`, `JS_GetUint32ArrayData`, `JS_GetFloat16ArrayData`,
`JS_GetFloat32ArrayData`, `JS_GetFloat64ArrayData`, `JS_GetBigInt64ArrayData`, `JS_GetBigUint64ArrayData`. The
`*mut c_void` data getter (`JS_GetArrayBufferViewData`) is unaffected: `c_void` already fails with null. A function
returning data only through an out-parameter (`glue__Get*ArrayLengthAndData`, `JS__Get*BufferLengthAndData`) is `()` and
has no failure value; the slice rule (section f) is that its out-params must be written on the skip,
not left stale.

**How script threads learn to unwind.** Not by an exception, and not by a check after every call: the payload waits in
the thread's slot, which `mozjs_sys::engine::poison::{take_payload, store_payload}` own and `mozjs::panic` uses for its
own `wrap_panic` payloads too, so `mozjs::panic::maybe_resume_unwind()` re-raises whichever is waiting. Servo calls it in
pure Rust frames after a failed evaluation; it must never be called under an `extern "C"` frame that has no `wrap_panic`.
The constellation does not depend on the unwind: it was already told by the hook (step 6). The unwind ends the poisoned
script thread so it stops serving tasks (open point 1 adds the resume points it still lacks). `wrap_panic` no longer
`assert!`s that the slot is empty (`mozjs/src/panic.rs`): that assert was a second abort; the first
payload is kept, a later one is forgotten (dropping a payload runs foreign code that may panic).

## (c) Contexts, threads, where the flag lives

* One context per thread (`cx::CURRENT`, set by `JS_NewContext`), so the poisoned state is **per thread**: a
  `thread_local` byte in `poison.rs` (`STATE`), read by the gate without touching `Cx` (an outer frame may hold a
  reference into it; `cx_of`'s contract). The crash record is **process-wide** (`RECORDS`, a mutex taken only after a
  panic), so the embedder's crash reporter reads it from any thread.
* **Shims with a context argument** (most) and **context-free shims** (typed-array and string accessors that take a
  `JSObject*`/`JSString*`, `JS__GetPromiseState`, `JS_GetStringLength`) follow one rule: the gate is the calling thread's
  state. A context-free shim called on a script thread belongs to that thread's context; called on a thread with no
  context (off-thread compilation in `mozjs/src/offthread.rs`, `glue__JS_Init`), a panic poisons only that helper
  thread, which owns no engine state, and returns failure to its caller. No shim poisons another thread.
* **Other threads.** `JS_RequestInterruptCallback` (and `...CanWait`) is called from Servo's hang monitor while the script
  thread runs; it touches only the context's `Interrupt`, by address, under a poison-tolerant lock: exempt, and the wrap
  calls it itself. The profiler (`profile.rs`) samples through the interrupt hook on the script thread. Brimstone collects
  on the context's thread (`engine-invariants.md`, section 6: "all methods are called on the Context's thread").
* **A poisoned thread gets no new context** (`JS_NewContext` is gated: null), and its context is leaked at
  `JS_DestroyContext` (gated).

## (d) Re-entrancy

* **Nested shims.** Shim A runs script; script calls a Servo native; the native calls shim B; B's body panics. The
  innermost catch is B's, so the unwind crosses only Rust frames of the glue and Brimstone below B, never an engine frame
  or a Servo `extern "C"` frame twice. B poisons and returns failure; the native gets the failure. Whatever the native
  returns, the engine cannot go on: the next native call is refused (`call_native`: uncatchable), the next interrupt point
  refuses (B requested one), and A's epilogue returns failure although its body returned normally. Tested:
  `nested_panic_stops_the_outer_script` (a native that ignores the failure and returns true; `mark()` in the same `try`
  and in its `finally` never runs).
* **GC-time shims** (`#[engine_shim(gc)]`: the `glue__Call*Tracer` family, the `trace_*` root tracers, write barriers,
  the weak-pointer updates `JS_UpdateWeakPointerAfterGC*`) are never gated: a collection started in an engine frame above
  one may still be running, and a skipped tracer would free live objects under it. So their body runs even on an already
  poisoned thread (tested: `gc_shim_enters_its_body_on_a_poisoned_thread`), and a poisoning that merely
  *predates* the body is left to where it happened: the value is returned. What must not happen is a poisoning that
  *arises during* the body — a gated shim nested inside a tracer catching a panic — because the collection has since been
  tracing over a heap the engine no longer trusts. `run_gc`'s epilogue sees `STATE` go dirty across its body and aborts
  with a `fatal` record first (tested: `gc_shim_aborts_on_a_poisoning_arising_during_its_body`). A panic
  in the body itself is recorded (`fatal: true`) and likewise aborts (`gc_shim_panic_aborts_with_a_record`).
* **The limit of the GC containment.** Both aborts call `process::abort`, which can land anywhere inside a collection, so
  it may leave a half-traced graph and a torn CC. That is acceptable and deliberate: the containment for a GC-time failure
  is a process kill, not a recoverable error, precisely because a collection that lost a tracer, or traced over a poisoned
  heap, cannot be safely continued. Nothing downstream relies on the heap after the abort — the process is gone — so a torn
  graph is never observed; the poisoned-script path (a tracer returning failure to a live collection) is the case that
  would be unsafe, and that is why these shims abort rather than return failure.
* **A panic while panicking.** The record step runs inside its own `catch_unwind`; the thread state is set before it, so a
  panic while recording loses at most the record, never the poisoning; its payload is forgotten. A panic inside the
  glue's panic hook, or a `Drop` that panics while a panic unwinds, is a double panic: Rust aborts the process at once,
  before any catch (section g).
* **A `Drop` that panics on the normal path** is an ordinary panic below the shim: caught like any other.
* **Payloads** are never dropped by the wrap (stored or forgotten): a payload's `Drop` is foreign code.

## (e) The macro shape, and no `deny` lint

The mechanical change per function is one line above it, nothing else:

```rust
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetProperty(cx: *mut JSContext, ...) -> bool { /* unchanged */ }
```

Forms: `#[super::poison::engine_shim]` (gated), `#[super::poison::engine_shim(failure = <expr>)]` (gated, explicit
failure value: the `c_char` shims), `#[super::poison::engine_shim(gc)]` (tracers, barriers, finalize-time code),
`#[super::poison::engine_shim(exempt = "<reason>")]` (left as written; only the five functions on the coverage test's
`EXEMPT` list, any other use fails the test). The five: `crash_bt.rs` `handler` (signal handler),
`cx.rs` `JS_RequestInterruptCallback` and `JS_RequestInterruptCallbackCanWait` (other threads; the wrap calls it),
`func.rs` `glue__JS_CallArgsFromVp` (pure; `CallArgs` has no failure value), `profile.rs` `flush_at_exit` (atexit). Exceptions are named in the code by this attribute, never by an ast-grep
suppression comment, so the ast-grep rule for an `extern "C"` fn with no `engine_shim` attribute (regex
`^#\[\s*(?:[a-z_]+::)*engine_shim\b`) counts down to 0 and the syn test checks the exemptions by name. The path form needs
no `use` line, so a slice touches no imports. It is an attribute on the function item, not a function-like macro around
it, so the function stays visible to ast-grep and `syn`. The macro refuses: another ABI, variadic, `const`/`async`,
generics, methods, unknown arguments, `gc` with `failure`. A body's `return` returns from the closure, so bodies need no
edit. `#[super::poison::engine_shim]` inside a nested module needs the matching path (`crate::engine::poison::engine_shim`
always works).

No `#![deny(clippy::unwrap_used, expect_used, panic)]` for the files not yet wrapped: the panics that matter are in
Brimstone (heap exhaustion, `must!`, module asserts), which a lint on the glue never sees;
the glue files would need hundreds of allows to pass it; and the wrap makes the lint moot once a file is done. The
register's proposed lint (`engine-invariants.md:64`) is superseded by this wrap plus the coverage test.

## (f) The slice plan

The static check is `mozjs-sys/tests/engine_shim_coverage.rs`, a test rather than a build script: a build script would
run in every Servo build and its failures would stop the browser build for a bookkeeping reason; the test runs in the
glue's own checks and prints the inventory. It parses every `src/engine/**/*.rs` with `syn` and fails on any `extern "C"`
function without the attribute that is not in the ratchet
`src/engine/edge-wrap-pending.txt`. A pending line whose function is wrapped is itself a failure, so each slice must delete
the lines of what it wrapped; **a slice never adds a line** (review: `git diff --numstat <base> -- mozjs-sys/src/engine/edge-wrap-pending.txt`
shows 0 added).

**No pinning of the glue while the ratchet is non-empty.** A pending line is an engine `extern "C"` entry point still running
raw, and a partial wrap is not safe to pin (point 7 below): the unwrapped edge is exactly where a poisoning aborts the process.
So what stays forbidden is pinning the GLUE into the browser while lines remain; bringing glue main into the wrap branch — with
its Brimstone re-pin — is allowed and needed, because the slices start from main and must carry both the wrap and the engine
main is on. The coverage test prints `pending lines in the ratchet: N` (its inventory,
`--nocapture`); the pin gate is that `N` reaching 0 — the 848 functions reduced to the exempt few, no pending lines.

**The count: 848.** The syn scan finds 848 `extern "C"` function items under `src/engine/` (free functions and the one
nested `flush_at_exit`; declarations in `extern "C" { }` blocks and `extern "C" fn(..)` pointer types are not items). That
equals an independent count of the raw lines (850 `extern "C" fn` lines minus the fn-pointer types at `func.rs:21` and
`mod.rs:95`). A rougher grep counted 853, including 8 functions outside `engine/`. 848 = 13 wrapped by this
prototype + 5 exempt (named by attribute) + 830 pending. The ast-grep rule sees all 18 attributed functions, so its
count is 830 today and reaches 0 when the slices are done.

Inventory (`cargo test -p mozjs_sys --test engine_shim_coverage -- --nocapture`), pending per file:

| File | Total | Wrapped | Exempt | Pending |
|---|---|---|---|---|
| bindings.rs | 19 | 0 | 0 | 19 |
| cache_partition.rs | 1 | 0 | 0 | 1 |
| caller_private.rs | 1 | 0 | 0 | 1 |
| clone.rs | 20 | 0 | 0 | 20 |
| compartment.rs | 49 | 0 | 0 | 49 |
| crash_bt.rs | 1 | 0 | 1 | 0 |
| cx.rs | 29 | 4 | 2 | 23 |
| env_chain.rs | 7 | 0 | 0 | 7 |
| errors.rs | 9 | 0 | 0 | 9 |
| func.rs | 1 | 0 | 1 | 0 |
| function.rs | 39 | 0 | 0 | 39 |
| gc.rs | 19 | 1 | 0 | 18 |
| iter.rs | 3 | 0 | 0 | 3 |
| jobs.rs | 18 | 0 | 0 | 18 |
| module.rs | 31 | 1 | 0 | 30 |
| object.rs | 5 | 1 | 0 | 4 |
| profile.rs | 1 | 0 | 1 | 0 |
| promise.rs | 35 | 1 | 0 | 34 |
| property.rs | 109 | 0 | 0 | 109 |
| proxy.rs | 22 | 0 | 0 | 22 |
| realm.rs | 18 | 2 | 0 | 16 |
| runtime.rs | 61 | 0 | 0 | 61 |
| saved_frame.rs | 13 | 0 | 0 | 13 |
| script.rs | 20 | 1 | 0 | 19 |
| security.rs | 1 | 0 | 0 | 1 |
| string.rs | 25 | 1 | 0 | 24 |
| typed.rs | 145 | 0 | 0 | 145 |
| values.rs | 146 | 1 | 0 | 145 |
| **Total** | **848** | **13** | **5** | **830** |

The 13 wrapped: `JS_NewContext`, `JS_DestroyContext`, `JS_GC`, `JS_IsExceptionPending` (cx.rs), `JS__Evaluate2`,
`JS__ModuleEvaluate`, `JS_NewPlainObject`, `JS_GetStringLength`, `JS__GetPromiseState`, `JS_TypeOfValue`,
`JS__EnterRealm`, `JS__LeaveRealm`, and `glue__CallValueTracer` (`gc`). Return kinds covered: `bool`, `()`, GC pointer,
integer, enum, `gc`.

Slices (file-disjoint, a file is never split), in this order (the crash path first):

| Slice | Files (pending) | Count | `gc` candidates to read for |
|---|---|---|---|
| S1 | module.rs 30, promise.rs 34, jobs.rs 18, script.rs 19 | 101 | `finalize_module`, `finalize_loading_state` (module.rs), `trace_tables` (promise.rs) |
| S2 | function.rs 39, runtime.rs 61 | 100 | |
| S3 | property.rs 109 | 109 | `trace_id_vec`; the rooted id vector shims need `failure = null_mut()` |
| S4 | values.rs 145 | 145 | `trace_obj_vec`, `trace_raw_value`; rooted object vector shims need `failure = null_mut()` |
| S5 | typed.rs 145 | 145 | |
| S6 | compartment.rs 49, cx.rs 23, proxy.rs 22, realm.rs 16 | 110 | `trace_id_vec` (proxy.rs); the cx.rs GC-callback setters stay gated |
| S7 | string.rs 24, clone.rs 20, bindings.rs 19, gc.rs 18, saved_frame.rs 13, errors.rs 9, env_chain.rs 7, object.rs 4, iter.rs 3, security.rs 1, cache_partition.rs 1, caller_private.rs 1 | 120 | all `glue__Call*Tracer` (gc.rs), `trace_env_chain`, `trace_policy` |
| | | **830** | |

Each slice is one mechanical piece closed by this spec, file-disjoint so they can run in parallel; S4
and S5 are the largest and may each be split only along file boundaries (they are one file each, so they stay whole).

Each slice, for every function in its files: add the attribute line; `gc` for tracers (`glue__Call*Tracer`, `trace_*`),
write barriers and finalizers (`finalize_*`); `failure = ::std::ptr::null()` for a `c_char` return; read each body for a
return value a caller could take for success and harmful (b), and say so in the slice report; delete its lines from the
pending file. Check per slice: `cargo check -p mozjs_sys`, then
`cargo test -p mozjs_sys --test engine_shim_coverage` and `cargo test -p mozjs --features lucinanet-failpoints --test brimstone_edge_wrap`
(each test `--exact`, one per process: with one test thread libtest runs a test on the process's main thread, so a
poisoned thread would carry into the next test), plus the existing mozjs test files of the slice's area (for example
`brimstone_typed_api` for `typed.rs`). No behaviour may change on a clean context: the existing tests are the proof.

## (g) Not covered, and the cheapest next step

| Failure | Why the wrap does not see it | Cheapest next step |
|---|---|---|
| Stack overflow (native stack exhausted) | SIGSEGV on the guard page, not a panic. Brimstone turns JS recursion into a `RangeError` first (`runtime/error.rs`, "too much recursion"), and natives refuse past the frame-value cap (`func.rs`); deep Rust recursion in the parser or in Servo can still hit the guard | `LUCINANET_JS_CRASH_BT` already prints the frames; add a test that a deeply nested source is refused by the parser's depth limit, and check `JS_SetNativeStackQuota` margins against the script thread's stack size |
| SIGSEGV / SIGBUS (a wild pointer, a use after free) | memory corruption is not a panic | `crash_bt` (`LUCINANET_JS_CRASH_BT=1`) records it; the GC verify and stress modes find the causes |
| Allocation failure of the Rust heap (`handle_alloc_error`, `host_space.rs` host objects) | aborts, not a panic | none in the glue; Servo's allocator and the OOM killer decide. The JS heap's own exhaustion is covered (reason `OutOfMemory`) |
| A double panic (panic in a `Drop` during an unwind; a panic in a panic hook) | Rust aborts before any catch | keep `Drop` impls in the glue panic-free (they are bookkeeping); the glue's hook catches nothing and does little |
| A panic in a `gc` shim | aborted on purpose, after the record (section d) | none: a collection that lost a tracer cannot continue |
| `engine::missing()` stubs | deliberate `process::abort` with a log line (`engine/mod.rs`) | write the stub; the missing log already names it |
| A panic in a Servo callback without `wrap_panic` (`native_handler_callback`, `components/script/dom/promise/promise.rs:439`) | Servo's own `extern "C"` frame: aborts before reaching any shim | Servo companion change (open point 1) |
| The 8 `extern "C"` functions outside `engine/` (`mozjs/src/error.rs`, `gc/custom.rs`, `gc/trace.rs`, `rust.rs`, `mozjs-sys/src/jsgc.rs`, `lib.rs`) | Servo-side trampolines under the `wrap_panic` convention, not engine shims | review them under that convention with open point 1 |

## Open points

1. **Servo companion change (`third_party/servo`, not this repository).** (i) Resume points where the script thread is in
   pure Rust: after each task of the event loop (`components/script/event_loop/script_thread.rs`, the loop at `:1412`)
   and after the microtask checkpoint (`:4694`), the same in the worker loops; each a `js::panic::maybe_resume_unwind()`.
   (ii) `components/script/scripting/module_loading.rs:317`: `assert!(ModuleEvaluate(..))` becomes the failure branch that
   follows it (`:319-322`); today a poisoned `ModuleEvaluate` makes that `assert!` panic inside `native_handler_callback`,
   an `extern "C"` frame: the original abort, one frame higher. (iii) Wrap `native_handler_callback` (`promise.rs:439`) in
   `wrap_panic`. Without (ii) and (iii) the module path still aborts; the wrap must not be pinned into the browser before
   them (crash visibility first).
2. **Pinning.** The wrap is behaviour-neutral on a clean context; pin it into LucinaNet only with the Servo change above.
   The pin moves `Cargo.lock`: a new crate `lucinanet_engine_shim` (proc-macro, `syn` 2 with `full`, already in the lock);
   the tools with their own lock (`tools/lucinanet-test262`, `tools/lucinanet-wpt-jsshell`, `tools/t262`) need
   `cargo update -p mozjs_sys` too.
3. **Aborting on a `gc` shim panic** is a deliberate process abort (section d). The alternative, poisoning and skipping the
   tracer, risks freeing live objects under a running collection: memory unsafety.
4. **The static message rule** keeps the text before the first `:` of a formatted panic message. A message like
   `panic!("bad key {key}: ...")` would keep `bad key <data>`. Brimstone and the glue have no such panic in the paths read;
   a lint for `panic!` with arguments before the first `:` is possible if wanted.
6. **The `glue-panic-in-extern-c` rule cannot reach 0 by itself.** The wrap does not remove the `unwrap`/`expect`/`assert!`
   inside shim bodies; it makes them safe. That rule should skip a function that carries a non-exempt `engine_shim` attribute,
   so its 16 hits fall as the slices land; otherwise each needs a body rewrite that the wrap makes unnecessary.
7. **A partial wrap is not safe to pin.** The prototype proved it: in the out-of-memory test the still-unwrapped
   `JS__LeaveRealm` (mozjs `AutoRealm`'s drop) ran engine code on the poisoned context, the engine panicked ("leave_realm
   called while frames pushed after enter_realm are still active", `embed/mod.rs:1144`) and that panic aborted the process at
   the unwrapped edge. Wrapping the two realm shims fixed it. Every shim Servo calls on its unwind path must be gated before
   the pin: pin only after all seven slices.
8. **Leaking the poisoned heap** (up to 1 GiB per crashed tab, by default) instead of finalizing it. Finalizing runs Servo
   finalizers over a heap that may be half moved; leaking is the safe default. A process that crashes many tabs should
   restart; the record count (`poison_count`) is the signal.
