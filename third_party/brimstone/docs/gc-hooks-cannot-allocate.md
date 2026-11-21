# GC hooks cannot allocate (fixes hook-allocation-lost and hook-allocation-freed)

Decision note, written before the code. Row 6d of `docs/engine-invariants.md` says the
hooks the collector calls must not allocate on the JS heap, create handles or run JS. Until this change
the rule was prose plus a run-time refusal: every hook received a full `Context`, which is `Copy`,
dereferences to the whole `ContextCell` and is the first argument of every allocating function in the
engine. hook-allocation-lost (an allocation from `after_gc_weak` or `finalize_host_object` lands in the from-space and
is lost) and hook-allocation-freed (the same during a resizing collection is a use after free of the old buffer) are
both "the hook used the `Context` it was handed". The containment of an earlier change (branch
`gc-alloc-during-gc-abort`) turns that into an abort naming the hook. This change removes the
`Context` from the hooks, so the allocating hook no longer compiles.

## Decision

1. Every hook the collector calls receives a `GcHookCx<'_>` instead of a `Context`: `Host::trace_roots`,
   `Host::trace_host_object`, `Host::after_gc_weak`, `Host::finalize_host_object` and the two
   engine-cell hooks (`EngineCellTrace`, `EngineCellHooks::finalize`). It is a token bound to the call
   (a lifetime the hook cannot name, so it cannot be stored), with no `Deref`, no conversion to
   `Context` and no public method that allocates, makes a handle or runs script.
2. What a hook may do is already in its other arguments: the `RootVisitor` (report and update strong
   slots), the `WeakUpdater` (update weak slots), and `HostObjectRef` (header, class, flags, reserved
   slots). `GcHookCx` adds nothing public; it is the place a future allowed operation goes. The engine's
   own cell finalizer reaches its one piece of state through a `pub(crate)` method that does exactly that
   (`take_external_free`), not through a `Context`.
3. Proof that the wrong hook does not compile: `compile_fail` doc-tests with their rustc error codes, in a
   small doc-test-only crate (`tests/gc_hook_types`), next to a positive control that must compile.
   `brimstone_core` has `doctest = false` and no `trybuild`; turning its doc-tests on would add a second
   full build of the crate to every `cargo test -p brimstone_core`, and adding `trybuild` would add
   dependencies to `Cargo.lock`.
4. The containment stays as the release-mode backstop for what the type cannot see: a host that stored a
   `Context` (the glue does: `ServoHost.cx`), `embed::context_of(ptr)` on a pointer a hook can read,
   C finalizers in the glue that call back into the JSAPI shims through `GC_CONTEXT`, and
   `HeapPtr::to_handle`, which needs no context (row 6d's handle clause stays run-time/prose).
   Teardown is unchanged: finalizers at context drop get the same `GcHookCx`, and an allocation that a
   host makes there through a stored `Context` is still served (the containment's review item 1).
5. No deprecated shim. The glue pins the engine by git revision (`mozjs-sys/Cargo.toml`, `rev =`), so it
   keeps compiling against the old engine until the pin moves; the pin bump applies the four-line glue
   change in the last section. A shim would need a second trait (Rust has no overloading) and would keep
   the `Context`-taking hook shape alive, which is the thing being removed.

## Precedent

- **SpiderMonkey.** Trace hooks get only a tracer: `JSTraceOp` is `void (*)(JSTracer*, JSObject*)`
  (`js/public/Class.h`), and weak-pointer callbacks get `JSTracer*` too
  (`JSWeakPointerZonesCallback`, `js/public/GCAPI.h`). Finalizers get a `JS::GCContext*`, not a
  `JSContext*` (`JSFinalizeOp`, `js/public/Class.h`; `js/src/gc/GCContext.h`): the type has free and
  delete operations and no allocation API. Where a raw `JSContext*` is unavoidable, the "cannot GC"
  regions are scoped by `JS::AutoCheckCannotGC` / `JS::AutoAssertNoGC` (`js/public/GCAPI.h`) and
  `AutoSuppressGC` (`js/src/vm/JSContext.h`), and checked statically by the hazard analysis
  (`js/src/devtools/rootAnalysis/`). We follow the type part: a hook's context is not the mutator's
  context.
- **V8.** `DisallowGarbageCollection` (`src/common/assert-scope.h`) scopes the regions; first-pass weak
  callbacks may only reset the handle and may call no other V8 API (`include/v8-weak-callback-info.h`,
  `WeakCallbackInfo`), work that needs the isolate goes to a second-pass callback that runs outside the
  GC; `cppgc` trace methods receive only a `cppgc::Visitor` (`include/cppgc/visitor.h`). Our equivalent
  of the second pass is the job queue: work a finalizer needs to do with the engine is queued and runs
  after the collection, through `enqueue_job` / FinalizationRegistry tasks, never from the hook.

The paths above are cited from the upstream source layouts as known before this run; they were not
re-read for this note.

## The hooks the collector calls (all of them)

Found by reading every call from `gc/garbage_collector.rs`, `gc/verify.rs`, `context.rs` and
`embed/mod.rs` into the host or into a hook table, not from the trait's documentation:

| hook | called from | phase |
|---|---|---|
| `Host::trace_roots` | `embed::visit_roots_for_gc`, `embed::visit_roots_for_verify` | collection, verifier walks |
| `Host::trace_host_object` | `embed::trace_host_object`, `embed::trace_host_object_for_verify` | live trace, verifier walks |
| `Host::after_gc_weak` | `embed::sweep_after_gc` | weak pass |
| `Host::finalize_host_object` | `embed::sweep_after_gc`, `embed::teardown` | sweep, context drop |
| `EngineCellHooks::trace` | `embed::trace_engine_cell`, `embed::trace_host_object_for_verify` | live trace, verifier walks |
| `EngineCellHooks::finalize` | `embed::finalize_engine_cell` (sweep and teardown) | sweep, context drop |
| `ExternalOwner::Free` closure | `finalize_external_free_cell` (an engine-cell finalizer) | sweep, context drop |

The `Free` closure is a `Box<dyn FnOnce()>`: it already receives nothing. `enqueue_job` and
`promise_rejection` are not called during a collection (`hand_tasks_to_host` returns while `in_gc`; jobs
are handed over in `embed::after_gc`, after `gc_in_progress` is cleared), so they keep their `Context`
(row 6b is a separate finding).

## The new signatures

```rust
pub struct GcHookCx<'a> { /* private */ }

pub trait Host: 'static {
    fn trace_roots(&self, cx: GcHookCx<'_>, visitor: &mut dyn RootVisitor) {}
    fn trace_host_object(&self, cx: GcHookCx<'_>, object: HostObjectRef, visitor: &mut dyn RootVisitor) {}
    fn finalize_host_object(&self, cx: GcHookCx<'_>, object: HostObjectRef) {}
    fn after_gc_weak(&self, cx: GcHookCx<'_>, updater: &mut dyn WeakUpdater) {}
    // every other method unchanged, with `Context`
}

pub type EngineCellTrace = fn(GcHookCx<'_>, HeapPtr<ObjectValue>, &mut dyn RootVisitor);
pub struct EngineCellHooks {
    pub trace: Option<EngineCellTrace>,
    pub finalize: fn(GcHookCx<'_>, HeapPtr<ObjectValue>),
}
```

## What the type does not cover (the backstop's job)

- A `Context` the host obtained elsewhere: stored at install time, `embed::context_of`, a thread-local.
  Allocation through it during a collection aborts with the hook's name.
- Handles: `HeapPtr::to_handle()` takes no context. Creating a handle is not a moving-heap allocation
  (`docs/engine-invariants.md` section 2); the rule stays prose plus the proposed `Handle::new` debug
  assertion.
- Running JS from a hook needs a `Context`, so it is covered by the type unless the host escaped it; the
  escaped case allocates and aborts.

## Glue-side change needed at the next pin bump (not applied in this run)

`/home/user/Projects/Lucina/LucinaNet-js`, `mozjs-sys/src/engine/cx.rs`, in `impl Host for ServoHost`
(lines 169-235 at `e5558ef`). The four hooks ignore their context today (`_bs`), so the change is the
parameter type only:

```diff
-    fn trace_roots(&self, _bs: Bs, visitor: &mut dyn RootVisitor) {
+    fn trace_roots(&self, _cx: embed::GcHookCx<'_>, visitor: &mut dyn RootVisitor) {
-    fn trace_host_object(&self, _bs: Bs, object: HostObjectRef, visitor: &mut dyn RootVisitor) {
+    fn trace_host_object(&self, _cx: embed::GcHookCx<'_>, object: HostObjectRef, visitor: &mut dyn RootVisitor) {
-    fn finalize_host_object(&self, _bs: Bs, object: HostObjectRef) {
+    fn finalize_host_object(&self, _cx: embed::GcHookCx<'_>, object: HostObjectRef) {
-    fn after_gc_weak(&self, _bs: Bs, updater: &mut dyn WeakUpdater) {
+    fn after_gc_weak(&self, _cx: embed::GcHookCx<'_>, updater: &mut dyn WeakUpdater) {
```

plus `rev =` in `mozjs-sys/Cargo.toml` and `Cargo.lock`. The bodies stay as they are: they reach Servo
through `self.cx` (a raw `JSContext`) and `GC_CONTEXT`, which the engine type cannot see; that is the
part the containment covers. A grep of the glue for other `EngineCellHooks` or hook implementations
(`rg 'EngineCellHooks|fn trace_roots|fn after_gc_weak|fn finalize_host_object|fn trace_host_object'
mozjs-sys`) found only these four.
