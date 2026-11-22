# Engine invariants

The one document every change spec for **engine core** must cite. Engine core is
`src/js/runtime/bytecode/**` (generator and VM), `src/js/runtime/gc/**`, `src/js/runtime/embed/**`,
the parser's scope analysis and re-parse code (`src/js/parser/analyze.rs`, `scope_tree.rs`,
`reparse.rs`, `reparse_rules.rs`), and `mozjs-sys/src/engine/**` in the glue workspace (`js/`).

Rules are quoted verbatim from code, comments or `docs/embedding.md`, with `file:line` (lines of
the tree this file was written against; re-check after edits; only common indentation removed). A
rule with no comment of its own is marked **Derived**: it is stated here in our words and the code
that shows it is cited.

The machine-readable list of engine-core paths (one glob per line, used by the review-record check) is
`docs/engine-core-paths.txt`.

## Enforcement register

A rule an agent or contributor can break by not reading it is not yet a rule. This table says, for
each invariant of this document, what refuses or detects its breakage **today** (checked against the
tree this file was written against: workflow job names, script lines, `file:line`), and what is
planned. It is ranked by how easily an ordinary change breaks the invariant, most easily first.

**States.** `enforced`: a machine refuses it. `partly`: some paths are refused or detected, named in
the second column. `detected`: found afterwards, not refused. `ordered`: nothing enforces it today
and a change with an id is in work. `prose`: nothing checks it; the second column says `nothing` and
the last column proposes the check. Order of preference for a check: impossible by types, refused at
the tool, refused before it lands (a check, a lint), detected afterwards, a comment (which counts as prose).

**The checks that exist.** (1) The public workflow `.github/workflows/ci.yml`: job `lint`
(`cargo bfmt --install --check`, `cargo clippy -- -D warnings`, ci.yml:22-25), job `cargo-tests`
(`cargo test`, which includes the embed tests with their own verifier and cache-verify runs, and
`gc_stress_test` when that feature is on, embed/tests.rs:3), and job `integration-tests-gc-stress-test`
(`cargo brimstone-test --release --features gc_stress_test`, the whole conformance suite,
ci.yml:69-81). It runs on `pull_request`; whether a job blocks a merge depends on repository settings,
not on the file, and was not verified. (2) The verification steps that run on a change before it
lands: a check that refuses rewritten tests, golden files and ignore lists, a `cargo` run over the
touched crates, the proof-ledger checks, the permanent-space census `scripts/gate-census.sh`, and
`scripts/gate-gc-verify.sh` (section 7), run when a change touches `src/js/`. (3) The stress feature
and switches of section 4.

**What a lint can and cannot do here.** Clippy has no data-flow analysis. It cannot see "a raw
`HeapPtr` or `Value` is held across a call that can allocate", "an operand is read after a
GC point" or "this hook allocates". Nothing in the workspace lint table (`Cargo.toml:109-118`: only
`allow` entries) or a `clippy.toml` (none exists) says anything about these rules today. What a lint
can do, cheaply: `disallowed-methods` / `disallowed-types` to forbid a named constructor or type
outside listed modules (for example a raw-pointer constructor in handler files, or `Handle::new`
outside the handle and scope helpers); `unwrap_used`, `expect_used` and `panic` denied by an
attribute on one module (for example the glue's `extern "C"` shims, until a wrap macro exists); and
`must_use` (already used, handle.rs:128). What only a type design can do: make `instr` unreachable
after the first allocation, for example by decoding every operand of an instruction into a plain
`Copy` struct by value that `define_instructions!` emits, so a handler takes the struct and not the
reference (`instruction.rs:77-` builds `$instr_name_camel<W>` as a reference-like view over the bytes
today). What only a run can do: the stress feature, run-time stress and the verifier (section 4),
which find a stale pointer only if a test reaches the code.

| Invariant | Enforced today by | State | What will enforce it (change) | Ease of breaking |
| --- | --- | --- | --- | --- |
| 1a. Operands: read every operand into a local before the first call that can allocate; never touch `instr` after it (section 1) | A comment (`vm.rs:900-903`, `vm.rs:2976-2978`). Detected afterwards, only where a test reaches it, by the public job `integration-tests-gc-stress-test`; the verification step also runs `language/statements/with`, `language/eval-code` and the first-party test `language/scope/dynamic_ref_allocating_lookup` under `gc_stress_test` (section 7). Broken once: dynamic-ref-operands, commit 228e8cc9, found a day later | partly | Types: operands by value in a `Copy` struct emitted by `define_instructions!` (proposed, no id). Stress subsets in the verification step: an earlier change (done with the targeted stress steps; the public job stays) | high: any handler edit that adds a call before the last operand read compiles and passes ordinary tests |
| 1b. PC: publish before a call that can allocate or throw; reload the local PC after (section 1) | Comments (`vm.rs:939-944`, `vm.rs:171-174`); the macro `set_local_pc_checking_interrupt!` (vm.rs:1040-1051) does it for jump targets only. Same stress detection as 1a | detected | A VM-side macro or wrapper for every call that can allocate (proposed, no id); stress subsets: an earlier change | high: same reason as 1a; a new handler must remember it |
| 2a. A raw `HeapPtr` or an unrooted `Value` is valid only until the next GC point; keep a `Handle` across one (section 2) | Nothing static: `HeapPtr` is a plain pointer type (pointer.rs:8-9 is a comment). Detected by poisoning where a test reaches it: public job `integration-tests-gc-stress-test`; the verification step `gate-gc-verify.sh` runs the run-time stress window and from-space fill over seven test subsets only (IsHTMLDDA, regression, WeakMap, WeakSet, WeakRef, FinalizationRegistry, `Object/defineProperty`) | partly | an earlier change adds `with` and `eval-code`; a data-flow check is not available from clippy (proposed: a custom lint or a stress run per area, no id) | high: the classic engine bug; compiles and often passes unless a GC lands in the window |
| 3. GC-point checklist (section 3): property operations, conversions, calls, `alloc`/`make_*`, scope lookups, iterators, promises, host calls are GC points | Nothing: the list is prose; a missing entry is found by a stress run or not at all | prose | Not expressible in clippy (no data flow); the stress runs of 1a and 2a are the check; a marker attribute on allocating functions plus a custom lint is a proposal, no id | high: a helper that starts to allocate silently changes every caller |
| 5a. Lazy and cached bytecode: a cache hit must give bytecode identical to fresh generation; a new instruction, operand or scope-analysis output changes the structural key and `fncache/template.rs` (section 5) | Embed tests turn `set_function_cache_verify` on (embed/tests.rs:2568, 2616, 2829) and run under `cargo test` (public job `cargo-tests`; the touched-crate verification step). The harness flag `--function-cache-verify` and the tool `tools/fncache-check` exist (harness main.rs:76,117; fncache-check/src/main.rs:138); no workflow or check script runs them | partly | Run `--function-cache-verify` over the conformance suite in the public workflow (proposed, no id) | high: any new operand or analysis output; the key is easy to forget |
| 5c. Parser scope analysis and re-parse: a re-parsed function must give the bytecode of full-AST generation; a binding's VM location change changes keys (section 5) | Embed tests with `lazy_reparse_verify(true)` (embed/tests.rs:2988, 3405) under `cargo test`; the harness flag `--lazy-reparse-verify` (main.rs:85,121; lazy.rs:507) is run by no workflow or check script | partly | Run `--lazy-reparse-verify` over the conformance suite in the public workflow (proposed, no id) | high: any analyzer change that moves a binding |
| (d) Engine-core changes need an old-against-new review by a model of another family; a change that is not spec-closed also goes to the strongest available model (section 7) | Nothing: prose. By a count of 2026-11-17 (not rechecked here), 115 of 128 core commits since the upstream base had no such review | prose | A check that every change touching a path in `docs/engine-core-paths.txt` has a review record naming a model of another family; it is being built, report-only first (ordered with that change; no id known) | high: nothing stops a core change from merging without a review |
| (c) No panic crosses an `extern "C"` entry point in the glue | Nothing. The glue is a separate workspace (`js/`); this crate has no `extern "C"` function (grep over `src`: none), so no rule here can cover it. Finding panic containment | ordered | In the glue: a wrap macro that catches panics at every `extern "C"` shim (ordered, glue workspace; no id known). Until it lands: a lint scoped to the shim module that denies `unwrap_used`, `expect_used` and `panic` (proposed) | high: any shim edit can add an `unwrap` |
| 2b. Handle loop rule: a loop that makes handles needs its own `handle_scope!`; `Handle::replace` changes all copies; `escape` copies out before allocating (section 2) | Nothing. The job `integration-tests-handle-tracking` builds with `handle_stats` (counters printed by `src/main.rs:77-78`; no assertion on the counts was found) | prose | A lint forbidding `Handle::new` outside the handle helpers can be written; a bound on handle growth per loop needs the counters turned into a test (proposed, no id) | medium: a leak shows up as memory growth, `replace` aliasing as wrong values |
| 2c. A `HandleScope` is exited, not dropped (section 2) | `#[must_use]` (handle.rs:128) plus `cargo clippy -- -D warnings` in the public job `lint` (ci.yml:25); the verification step runs `cargo` on the touched crates, whether that denies warnings was not verified | partly | Run the same `-D warnings` in the verification step (proposed, no id) | medium: the type forces `exit` or a warning |
| 5b. A new instruction needs its definition, emission, handler, dump and a stress run (section 5) | `define_instructions!` (instruction.rs:77-) generates the struct and accessors from one definition; the rest (emission, handler, dump, stress run) is prose. Whether a missing handler fails to compile was not verified | prose | A test that walks every defined instruction and checks it has an emitter, a handler and a dump (proposed, no id) | medium: the compiler may catch some omissions, not a wrong or untested handler |
| 6a. `Host::interrupt` must not allocate on the GC heap or run script unless the engine roots its raw arguments across it (a known gap) | Nothing. `push_stack_frame_with_generic_call_args` (vm.rs:3110-3121) calls `handle_interrupt` (vm.rs:641-656, which calls `Host::interrupt`) and then uses the raw `closure` and `receiver` it took by value; the trait doc says the hook "May run JS" (embed/mod.rs:230-233), so the contract allows what the code cannot survive. Ledger interrupt-allocation, candidate | prose | Fix: root `closure` and `receiver` across the poll, or poll before they are read; a stress run with a host whose `interrupt` allocates (proposed) | medium: only a host that allocates in the hook triggers it; any VM edit near the poll can reintroduce it |
| 6b. A host whose `enqueue_job` allocates while the engine holds raw promise and incumbent-global pointers | A doc comment: "Must not run JS, allocate on the JS heap or touch ArrayBuffer bytes" (embed/mod.rs:324-327); the engine passes raw `job`, `promise` and `incumbent_global` (embed/jobs.rs:63). Nothing checks a host. The host contract names this point | prose | The engine roots the three values across the call, or the trait takes `Handle`s (proposed); a test host whose `enqueue_job` allocates, under stress | medium: needs a host that breaks the documented rule |
| 6c. Interrupt polling discipline: poll only at loop back edges and JS function entry; publish PC before the hook and reload after; builtins are not interrupted (section 6) | Tests `interrupt_from_another_thread_stops_an_infinite_loop` (embed/tests.rs:1419) and `interrupt_wrappers_poll_and_run_the_host_hook` (embed/cell_tests.rs:755); the macro at vm.rs:1040-1051 encodes the back-edge case | partly | A test per looping instruction that the poll fires and the PC is valid afterwards under stress (proposed, no id) | medium: a new looping or entry instruction must remember the poll |
| 6f. A new embed entry point takes and returns `Handle`s, documents its GC points, keeps no `embed::state` borrow across a host call, and passes `EvalError::Uncatchable` on with `?` (section 6) | Nothing; the rules are in docs/embedding.md:388-395, 521-524 | prose | A `disallowed-types` lint on a raw `HeapPtr` in a public `embed` signature (host objects excepted by a short allow list) and the review of (d); the borrow rule needs a type or a test (proposed, no id) | medium: each new entry point is a chance to take a raw pointer |
| 5d. `gc/`: every pointer the collector can reach (roots, item fields, host object fields, cell trace hooks) is visited; a new heap item kind needs its visitor and size and a verifier run (section 5) | The verifier `gc::verify` aborts on a bad pointer; its unit tests (embed/tests.rs:3062, 3087, 3149) run in the verification step `cargo test -p brimstone_core gc_verify` and the permanent-space census check; the subsets of 2a run it over real programs. A field nobody visits that no test reaches is not found | partly | A per-kind visitor and size consistency test over every heap item kind (proposed, no id) | medium: a new field in an item is easy to leave unvisited |
| (e) The stress subsets: `scripts/gate-gc-verify.sh` runs the `BS_GC_VERIFY` subsets and, since an earlier change, also `language/statements/with`, `language/eval-code` and `language/scope/dynamic_ref_allocating_lookup` under `gc_stress_test`; the full feature run is a job of the public workflow only | As stated: `gate-gc-verify.sh` (section 7) and `ci.yml:69-81`. The verification step runs the feature only for those three filters | partly | an earlier change (done); the full feature run as a required check of the public workflow (repository setting, not verified) | medium: a check that runs less than it claims proves nothing, so the guards in the script matter |
| 6d. GC hooks (`trace_roots`, `trace_host_object`, `after_gc_weak`, `finalize_host_object`, and the engine-cell trace and finalize hooks) must not allocate on the JS heap, create handles or run JS (section 6) | **By type** since the hooks receive a `GcHookCx<'_>` (embed/mod.rs), not a `Context`; it has no `Deref`, no conversion to `Context` and no public method, and cannot outlive the call, so a hook that allocates, asks for a GC, reaches a realm or keeps the token does not compile: compile-fail doc-tests with their rustc error codes and a compiling control in `tests/gc_hook_types` (`cargo test -p brimstone_gc_hook_types --doc`; docs/gc-hooks-cannot-allocate.md). The type does not see a `Context` the host kept or recovered (`context_of`), nor `HeapPtr::to_handle`. For those, the backstop: the JS-heap half is refused at run time since an earlier change, for the duration of a **collection** only: `GarbageCollector::run` marks the interval (`ContextCell::gc_collecting`, gc/garbage_collector.rs:150) and closes the allocation window with it (:151), shuts the window the swap opened again for the `gc_verify` after-verification walk (:247) and reopens it once, at `reset_alloc_limit` after the last host call (:253); a moving-heap allocation inside that interval reaches `Heap::abort_alloc_during_gc` (gc/heap.rs:442 → :512), which prints the size and the hook (`ContextCell::gc_alloc_hook`, set by `embed::GcHookLabel` at each of the ten host call sites) and aborts the process. Teardown is not a collection: it sets `gc_in_progress` without closing the window, and a finalizer that allocates there is served as it always was — `cover_alloc` if it fits the semispace, out of memory if it does not. Eight abort tests (`gc_alloc_in_*`, `allocation_in_finalizer_aborts_naming_the_hook`; embed/tests.rs:4869-4992), the control `gc_hook_that_does_not_allocate_is_not_aborted` (:4994) and the teardown case `gc_alloc_in_teardown_finalizer_is_served` (:5020). A nested GC does not run; creating a host object in a GC is refused. Not covered: creating a handle, or running JS from a hook through a `Context` the host kept or recovered — a stored `Context`, `context_of`, or the glue's C finalizers, since a hook that runs script through the `GcHookCx` token does not compile — and allocations from `enqueue_job` | enforced | A debug assertion in `Handle::new` while a GC runs, and the same refusal for running JS from a hook (proposed, no id) | low: only host and engine-cell authors touch the hooks |
| 6e. Engine cell hooks follow the same rules, and no ArrayBuffer bytes are read or written in callbacks that run without JS on the stack (section 6, WebAssembly design 3.2) | Nothing for the bytes rule (doc only, embed/mod.rs:262-271, 887-893); the hook allocation rule is covered by 6d | prose | A debug assertion that no ArrayBuffer data pointer is dereferenced while a GC or a hook is on the stack (proposed, no id) | low: few callbacks, rarely edited |
| 6g. External ArrayBuffers: the memory stays valid and unshared while attached; a buffer with the `WasmMemory` detach key is not detached, transferred or stolen by an embedder (section 6) | Rust `unsafe` on `array_buffer_new_external` (api.rs:337) forces an `unsafe` block at the call site but not the contract; `array_buffer_detach` returns false for the key (api.rs:853-858). No test of the refusal was found in `src/js/runtime/embed` by grep | partly | A test that detaching a wasm memory buffer fails and changes nothing (proposed, no id) | low: one function, few callers |
| 4. From-space poisoning and stress: the stress feature, the run-time switches and the verifier stay on and run (section 4) | The feature `gc_stress_test` runs in the public workflow job only (whether that job is a required check was not verified); the verification step sets `BS_GC_VERIFY` and `BS_GC_STRESS=4096` for seven subsets and runs the verifier unit tests (`gate-gc-verify.sh`, guarded against a vacuous run) | partly | `language/statements/with`, `language/eval-code` and `language/scope/dynamic_ref_allocating_lookup` under `gc_stress_test` in the verification step: an earlier change (done) | low: the guards in the script catch a missing harness switch or test checkout |
| 5e. `gc/`: the embedder updates weak pointers while the from-space is intact (garbage_collector.rs:190-191); the weak-edge repair decisions are pure (`weak_rules.rs`) | The verifier unit tests and the weak-edge subsets above | partly | none beyond the above | low: one place, rarely edited |
| 5f. `embed/`: host objects live in the non-moving host space and are marked and swept, never moved (section 5) | A design property; the verifier checks host-object pointers and the census check checks the permanent space. Nothing checks that a new host-space type is traced | partly | A test per host-space type that its fields are visited (proposed, no id) | low: rarely edited |
| 7a. No change edits, deletes or regenerates a conformance test, golden file or ignore list to pass (section 7) | the verification step refuses rewritten tests, golden files and ignore lists | enforced | | low: refused before it lands |

**Counts (24 rows):** enforced 1, partly 11, detected 3, ordered 1, prose 8. The two rows that already
caused a page-reachable memory bug or a near miss are 1a (found a day after the merge) and 6a
(a candidate finding); every `prose` row names the check to add.

## 1. The VM rules: `instr` is a raw reference into moving bytecode

The dispatch loop hands each handler `instr`, a reference into the running function's bytecode,
which is a heap item that the collector copies.

`src/js/runtime/bytecode/vm.rs:900-903`
> /// Invariants:
> /// - By the time that a throw is possible, PC must be updated to point to the next instruction
> /// - References to the instruction cannot be held over any allocations, since the instruction
> ///   points into the managed heap and may be moved by a GC.

Where lazy generation can allocate in the middle of a handler, the code repeats it:

`src/js/runtime/bytecode/vm.rs:2976-2978`
> // Generate the bytecode of a lazy function on its first call. This may allocate, so the
> // receiver is read again from its register afterwards. The instruction itself must not be
> // read again since a GC may have moved the bytecode.

The program counter follows the same rule (the GC rewrites the VM's PC field):

`src/js/runtime/bytecode/vm.rs:939-944`
> // Invariants:
> // - At the top of each iteration the local PC holds the address of the next opcode. The
> //   published PC may be stale, so it must be written before any call that can allocate or
> //   throw.
> // - After any call that can allocate or throw, the local PC must be reloaded from the
> //   published PC, since the GC may have moved the bytecode and rewritten the VM's PC field.

`src/js/runtime/bytecode/vm.rs:171-174`
> /// The published program counter. Externally visible and is updated by the GC.
> ///
> /// The dispatch loop uses a local copy of the PC but must ensure that the local copy is
> /// published to this field before any operation that can allocate, throw, or enter the runtime.

**Derived rule (operands).** Read every operand a handler needs (`instr.dest()`,
`instr.name_index()`, `instr.reference()`, ...) into a local **before** the first call that can
allocate (section 3), and never touch `instr` after it. Writing a register after an allocation is
fine when the register number was read first: `write_register` addresses the VM stack, not the
bytecode.

**Incident dynamic-ref-operands (commit 228e8cc9).** `execute_load_dynamic_ref` and `execute_make_dynamic_ref`
ran a scope lookup that can run a getter (so collect) and only then read `dest` and `reference`
from `instr`: stale bytes once the bytecode had moved.

## 2. Rooting

`src/js/runtime/gc/pointer.rs:8-9`
> /// For direct references to heap pointers, such as references to other heap items stored within a
> /// heap item. May not be held on stack during a GC (which can occur during any heap allocation).

`src/js/runtime/gc/handle.rs:24-26`
> /// Handles hold a value or heap pointer behind a pointer. Handles are safe to store on the stack
> /// during a GC, since the handle's pointer does not change but the address of the heap item
> /// behind the pointer may be updated.

Related handle rules:

`src/js/runtime/gc/handle.rs:80-81`
> /// Replace the value stored behind this handle with a new value. Note that all copies of this
> /// handle will also be changed.

`src/js/runtime/gc/handle.rs:595-604`
> /// Trait for items that can escape (be returned from) a handle scope. The item must be copied into
> /// the parent handle scope.
> pub trait Escapable {
>     /// Copy this item into the current handle scope. Called from the parent's handle scope so that
>     /// this item can escape the destroyed child handle scope.
>     ///
>     /// This is called after the handle scope containing the escaped item has been destroyed. This
>     /// means that allocating a handle may overwrite the handles in this item. If multiple handles
>     /// must be moved to the parent scope then be sure to copy out all the values before allocating
>     /// any new handles, to avoid overwriting the old handles.

`src/js/runtime/bytecode/generator.rs:368`
> // Escape emit result into current handle scope immediately, before allocation occurs

**Derived** (`src/js/runtime/gc/handle.rs`):

- **`Handle::new` is not a GC point.** It writes one word into the current block (handle.rs:46-47).
  When the block is full, `push_block` (handle.rs:416-438) takes a block from the free list or
  allocates one with `Box::new(HandleBlock { .. })` (handle.rs:355): a Rust allocation, not a JS
  heap allocation. So creating a handle never moves an object and never invalidates a `HeapPtr`
  you hold. It does consume a slot: a loop that makes handles needs its own `handle_scope!`.
- **A scope is exited, not dropped:** `HandleScope` is `#[must_use = "HandleScopes must be
  explicitly exited with a call to exit"]` (handle.rs:128). A value leaves a scope only through
  `escape`.
- **A raw `HeapPtr` or an unrooted `Value` that holds a heap item is valid only until the next
  GC point.** Keep a `Handle` across one and read the pointer again afterwards.

## 3. What can allocate or run script (GC points)

**Derived.** The collector runs inside a JS-heap allocation (`Heap::alloc_uninit_with_size`:
`collect_for_alloc`, heap.rs:395; in stress mode a GC on every call, heap.rs:379-383), so a GC
point is any call that can allocate on the JS heap or run script (script allocates). Checklist:

- [ ] property get, set, define, delete, `has`, `own_keys` (getters, setters, proxy traps, host
  `resolve` hooks);
- [ ] every conversion: `to_string`, `to_number`, `to_primitive`, `to_object`, `to_property_key`,
  `to_numeric`, `to_length` (user `valueOf`/`toString`; string and BigInt results allocate);
- [ ] every call and construct, native and host calls, and lazy generation of a function on its
  first call;
- [ ] every `alloc`, `make_*`, `create_*`, `new_*`, `*::new` that takes a `Context`: closures,
  arrays, errors, iterator results, regexps, shapes and growing property tables, string
  concatenation and flattening;
- [ ] scope lookups (`lookup_reference`, `load_from_reference`, with-object `has` and
  `@@unscopables`, global object access); the code says so where it matters:

`src/js/runtime/bytecode/vm.rs:4515-4516`
> // The value may be produced by a getter that collects, so the reference is only read
> // back from its handle after the load.

- [ ] iterator `next`/`close`, promise operations, job enqueueing, module load and link, `eval`,
  the parser, the bytecode generator;
- [ ] anything that reaches the embedder (every `Host` method except the GC hooks of section 6) and
  `embed::gc`.

Not GC points: `Handle::new`/`replace`, reading a register, `write_register`, host-space
allocation (`alloc_host_object`: "Does not allocate on the moving heap, so cannot trigger a GC",
embed/host_object.rs:73-74; same for `alloc_engine_cell`, embed/mod.rs:905-906).

## 4. The collector: objects and bytecode move

`docs/embedding.md:267-269`
> Brimstone's collector is a Cheney semispace copier: every live object moves on every GC. Servo needs
> DOM reflectors, globals and DOM proxies to keep their address for life, so host objects live in a
> separate **host space** (`embed/host_space.rs`) that the copier never moves.

Every live heap item moves on every collection, `BytecodeFunction` bytecode included; only the host
space and the permanent space stay. There is no incremental or generational mode.

**From-space poisoning.** With the `gc_stress_test` Cargo feature (`src/js/runtime/context.rs:782-785`,
`Context::enable_gc_stress_test`; the harness turns it on at `tests/harness/src/runner.rs:363-366`)
every allocation collects and the old space is overwritten:

`src/js/runtime/gc/heap.rs:379`
> // Run a GC on every allocation in stress test mode

`src/js/runtime/gc/garbage_collector.rs:197-198`
> // In GC stress test mode, overwrite the old heap with 0x01 bytes to try to catch reads from
> // pointers to the old heap.

**Run-time switches** (`src/js/common/options.rs`), set by the harness from the environment
(`tests/harness/src/runner.rs:323-324`): `BS_GC_VERIFY` (set to any value) and `BS_GC_STRESS=<bytes>`.

`src/js/common/options.rs:140-142`
> /// Stress mode for crash hunts: collect after every this many bytes allocated (or sooner, when
> /// the sizing says so), fill the from-space with 0x01 after each normal collection and keep its
> /// pages, so that a stale pointer into it reads garbage instead of the moved object. Slow.

`src/js/common/options.rs:145-146`
> /// Verify the heap before and after every collection (`gc::verify`): every pointer must point
> /// at a heap item or a live host object, or the process aborts with a report. Slow.

`src/js/runtime/gc/verify.rs:3-8`
> //! Before and after each collection, every pointer held by a root, a heap item or a host object
> //! must point at the start of a heap item in the permanent space or the current semispace, or at a
> //! host object that has not been freed. A bad pointer is reported with what holds it (item kind,
> //! address and field offset, or "root"); a walk that runs off an item boundary is reported as a
> //! desync. Any failure aborts the process, so the log and a debugger show the state at that point.
> //! Costs two walks of the heap per collection: for hunts only.

## 5. Per-directory rules

### `src/js/runtime/gc/`
`mod.rs` has no module documentation. See sections 2 to 4 and:

`src/js/runtime/gc/weak_rules.rs:1`
> //! The pure decisions of the weak-edge repair.

`src/js/runtime/gc/garbage_collector.rs:190-191`
> // Let the embedder update its weak pointers, then finalize and free dead host objects. Must
> // happen while the from-space is still intact.

`src/js/runtime/gc/heap.rs:385`
> // Ensure that this direct reference to heap is not used after a GC or allocation

**Derived:** every pointer the collector can reach (roots, item fields, host object fields, cell
trace hooks) must be visited; an unvisited heap pointer dangles after the next collection, and
`gc::verify` is what reports it. A new heap item kind needs its visitor and size (`heap_item.rs`,
`sizing.rs`) and a verifier run.

### `src/js/runtime/bytecode/`
`mod.rs` has no module documentation.

`src/js/runtime/bytecode/lazy.rs:1-6`
> //! Lazy bytecode generation.
> //!
> //! Nested functions of a script or module get a stub `BytecodeFunction` when their parent is
> //! generated. A stub carries everything needed before the function first runs (flags, name,
> //! length, realm, source range) but no bytecode. Its body is generated from the AST on the first
> //! call, and the stub then forwards to the generated function.

`src/js/runtime/bytecode/fncache/key.rs:1`
> //! Structural key computation for the per-function bytecode cache.

**Derived:** a cache hit must give bytecode identical to fresh generation, so anything that changes
a function's bytecode (a new instruction or operand, a new scope-analysis output) is a change to
the structural key (`fncache/key.rs:1-11`) and to `fncache/template.rs`. A new instruction needs
its definition, its generator emission, its handler (operands first, section 1), its dump in
`instruction.rs`, and a `gc_stress_test` run (section 7).

### `src/js/runtime/embed/`

`src/js/runtime/embed/mod.rs:1-3`
> //! Embedding API: lets a host (e.g. a browser DOM) install hooks into the runtime.
> //!
> //! See `docs/embedding.md` for the design. In short:

`src/js/runtime/embed/compiled.rs:1`
> //! Compiling scripts without running them, and parsing them off the context's thread.

`src/js/runtime/embed/host_space.rs:378-380`
> /// The non-moving space for host objects. The copying collector never moves objects in this space.
> /// Instead they are marked when first reached, have their fields visited, and are swept (finalized
> /// and freed) when unmarked at the end of a GC.

### `src/js/parser/` (scope analysis, re-parse)

`src/js/parser/reparse.rs:4-11`
> //! A lazy function (a stub that has not run yet) used to keep the whole program's AST alive. Once
> //! its script has run, the AST is dropped and the stub keeps only its source range and the id of
> //! its scope node in the program's scope tree, which stays alive in its own arena. On the first
> //! call the function's own text is parsed again into a temporary arena, with the parser and the
> //! analyzer put back into the state they were in when they reached the function in the full parse
> //! (recorded here, in the function's scope node). Names the function uses from outside resolve
> //! through the kept scope tree, whose bindings already carry their final analysis (captured,
> //! TDZ checks, VM locations), so the bytecode is the same as generation from the full AST.

`src/js/parser/scope_tree.rs:20-21`
> /// Indexed by scope node id. None for a node removed by `compact_into` (no lazy function
> /// that has not run yet is nested in it).

`reparse_rules.rs` is the pure part of what the analyzer keeps of its function stack
(`reparse_rules.rs:6-19`): the kept chain answers the three questions the analyzer asks of the
stack ("the top entry", "the nearest entry from the top that is not an arrow function", "whether
it is empty") as the full stack does, and a chain longer than the snapshot holds is refused (the
function is then generated before its AST is dropped).

**Derived:** a change to analysis that moves a binding's VM location changes bytecode and cache
keys, and a re-parsed function must produce the bytecode of the full-AST generation (reparse.rs:10-11).
The harness flag `--lazy-reparse-verify` compares the two.

## 6. The embedding boundary

`src/js/runtime/embed/mod.rs:206-220`
> /// The host environment. Installed once per Context, all methods are called on the Context's
> /// thread. Methods take `&self` since the host is called re-entrantly (a native may run JS that
> /// calls another native), so hosts keep their mutable state in `Cell`/`RefCell`.
> ///
> /// Methods called during a GC (`trace_roots`, `trace_host_object`, `after_gc_weak`,
> /// `finalize_host_object`) must not allocate on the JS heap, create handles or run JS. They receive
> /// a [`GcHookCx`] instead of a `Context`, so they cannot reach the allocator through their arguments.
> ///
> /// **No ArrayBuffer bytes in callbacks that run without JS on the stack** (WebAssembly design
> /// §3.2): `trace_roots`, `trace_host_object`, `after_gc_weak`, `finalize_host_object`,
> /// `promise_rejection` and `enqueue_job`, like the engine-cell hooks (`EngineCellHooks`) and the
> /// free callback of an external buffer (`api::ExternalOwner::Free`), must not read or write the
> /// bytes of any ArrayBuffer or view, and must not keep a pointer to them. Some of them run inside
> /// a GC, and the bytes of an external buffer (a wasm memory) are only guaranteed to match the
> /// buffer at the crossings where engine code runs JS.

`docs/embedding.md:511-530`
> - **GC hooks** (`trace_*`, `after_gc_weak`, `finalize_host_object`) and `enqueue_job` must not
>   allocate on the JS heap. The GC hooks and the engine-cell hooks receive a `GcHookCx` instead of a
>   `Context` (no allocator, no handles from the context, no script, no GC), so a hook that allocates
>   through its arguments does not compile (`tests/gc_hook_types`); what follows is the run-time
>   backstop for a host that reaches around the type with a `Context` it kept or recovered
>   (`context_of`). A GC never nests: a GC asked for while one runs (by `embed::gc`, also from
>   finalizers at teardown) does not run. An allocation on the moving heap from a hook is fatal: the
>   collector closes the allocation window for the duration of a collection, so any such allocation
>   reaches `Heap::abort_alloc_during_gc`, which prints the size and the name of the hook and calls
>   `std::process::abort` (not a panic: unwinding out of a GC leaves the heap half moved, and the
>   context unusable). Before that the item was handed back and died with the collection — released
>   from-space pages after a `Normal` GC, a freed heap buffer after a resizing one, so reading it is a
>   use after free — and only `Options::gc_verify` reported it, as a count at the end of the
>   collection. The window covers every host call of the collection, the verification walk that
>   `gc_verify` runs after the copy included. Teardown is not a collection, and its window stays open:
>   a finalizer that allocates there gets an item that is dropped with the context, and if it does not
>   fit the allocation fails as out of memory — no nested collection runs, as it never did. Creating a
>   host object during a GC is refused differently
>   (`new_host_object` fails; internal host space allocations panic): the GC has already traced the host
>   space, so the new object's fields would never be visited.

`docs/embedding.md:228-229`
> - `unsafe flat_string_chars(flat) -> StringChars::{Latin1, TwoByte}` borrow a flat string's chars.
>   The borrow points into the moving heap: nothing may allocate on the JS heap or run a GC while it is used.

`docs/embedding.md:242`
> Arrays, buffers, views (data pointers point into the moving heap: valid until the next GC)

**Interrupts.**

`src/js/runtime/bytecode/vm.rs:209-211`
> /// Set from any thread to request an interrupt. Checked at loop back edges and JS function
> /// entry, then handled by the embedder host (which terminates the script by refusing, as a
> /// context with no host does).

`src/js/runtime/bytecode/vm.rs:1038-1039`
> // Set the local PC to a jump target. Backward jumps (loop back edges) check for a
> // requested interrupt, which is a single relaxed load when none is pending.

`src/js/runtime/bytecode/vm.rs:3118`
> // Function entry from a JS call checks for a requested interrupt

`docs/embedding.md:399-401`
> The VM owns an `Arc<AtomicBool>`; `InterruptHandle::request` sets it from any thread. Backward jumps
> (including fused compare-and-jump) and JS-to-JS function entry do one relaxed load of it; when set,
> the VM clears it and calls `Host::interrupt` with the PC published, so the hook may run JS. The VM

**Derived:** a new looping instruction or entry path keeps the poll only at back edges and JS
function entry, publishes the PC before the hook (it may run JS) and reloads it afterwards
(`set_local_pc_checking_interrupt!`: `publish_pc`, `handle_interrupt`, `reload_pc!`). Builtins are
not interrupted (docs/embedding.md:545-546).

**Engine cells:**

`src/js/runtime/embed/mod.rs:958-965`
> /// The hooks of an engine cell: an engine-owned object in the host space (it never moves) with
> /// its own trace and finalize hooks, dispatched by the engine and never by the host. The cell is
> /// not a host object to the embedding API (`is_host_object`, `host_class`, `host_flags` and
> /// `swap_host_objects` do not see it).
> ///
> /// The hooks run like the host's GC hooks, without JS on the stack, so they must not allocate on
> /// the JS heap, create handles, run JS, take a GC, or read or write ArrayBuffer bytes (§3.2). They
> /// receive a [`GcHookCx`], not a `Context`, for that reason.

**External ArrayBuffers and detach keys:**

`src/js/runtime/embed/api.rs:330-336`
> /// # Safety
> /// While the buffer is attached, the `byte_length` bytes at `data` must be readable and writable,
> /// and nothing else may read or write them while the engine runs: no Rust reference to them, and
> /// no other attached ArrayBuffer covering any of them. For `ExternalOwner::None` the embedder keeps
> /// the memory allocated until it detaches the buffer; for `Free` until the closure runs; for `Cell`
> /// the owner of the memory keeps it allocated and matching the buffer (invariant B1) until it
> /// detaches the buffer.

`src/js/runtime/embed/api.rs:380-382`
> /// The detach key of an ArrayBuffer: `WasmMemory` for the buffer of a `WebAssembly.Memory`,
> /// `None` otherwise (and once detached). Embedders refuse to transfer, steal or detach a buffer
> /// with the `WasmMemory` key (structured-clone transfer, `JS::StealArrayBufferContents`).

`src/js/runtime/embed/api.rs:853-856`
> /// Detach an ArrayBuffer, dropping its contents (for an external buffer: dropping the reference to
> /// its memory). Returns false, changing nothing, if the buffer has the `WasmMemory` detach key: only
> /// its memory may detach it, and callers then throw a TypeError as the spec's DetachArrayBuffer
> /// does on a key mismatch.

**Derived:** a new embed entry point takes and returns `Handle`s (raw `HeapPtr` only for host
objects), documents which of its calls are GC points, keeps no `embed::state` borrow across a call
into the host (docs/embedding.md:531-534), and passes `EvalError::Uncatchable` on with `?`
(docs/embedding.md:388-395).

## 7. How to check a change

After the dynamic-ref-operands correction lands, the check for VM handler and generator changes is the conformance
directories that resolve names dynamically, with the GC stress feature (release build):

```
cargo btest --release --features gc_stress_test -- language/statements/with
cargo btest --release --features gc_stress_test -- language/eval-code
```

(The filter is a substring of the test path; without `-s` it matches in every suite that has it.)

**`scripts/gate-gc-verify.sh`, as it is today.** Debug build, from the repo root. In order:

1. Guard: fails unless `tests/harness/src/runner.rs` reads `BS_GC_VERIFY`.
2. Guard: fails unless the pinned conformance-suite checkout exists at the commit named in
   `tests/test262/install_test262.sh`; then links it at `tests/test262/test262`.
3. `BS_GC_VERIFY=1 cargo test --quiet -p brimstone_core gc_verify` (the verifier's unit tests).
4. Seven runs of `BS_GC_VERIFY=1 BS_GC_STRESS=4096 cargo btest -- <filter>` (the first with
   `--reindex`) over `built-ins/IsHTMLDDA`, `regression`, `built-ins/WeakMap`, `built-ins/WeakSet`,
   `built-ins/WeakRef`, `built-ins/FinalizationRegistry`, `built-ins/Object/defineProperty`.
5. Three release runs of `cargo btest --release --features gc_stress_test -- <filter>` over
   `language/statements/with`, `language/eval-code` and (first-party suite only, `-s integration`)
   `language/scope/dynamic_ref_allocating_lookup`.

That is the verifier on, a 4096-byte allocation window and the run-time stress mode (it poisons the
from-space too), and the `gc_stress_test` feature for those three filters only; the full feature run
is the public job. It covers weak edges, shape transitions and
first-party regressions; the VM rules of section 1 are checked by the commands above.

Other checks: `--lazy-reparse-verify` and `--function-cache-verify` on the harness for re-parse and
cache changes; `cargo test -p brimstone_core embed` for embed changes (the embed tests enable
`gc_stress_test` when it is on, embed/tests.rs:3).

**Review rule.** A change to engine core gets an old-against-new review by a model family other
than the one that wrote it; its brief is this file, the diff and the output of the commands above.
A change that is not spec-closed (a new instruction, a new GC path such as a heap item kind, weak
edge or root, a new embed entry point) also goes to the strongest available model for
specification and review. No change edits, deletes or regenerates a conformance test, golden file
or ignore list to pass.
