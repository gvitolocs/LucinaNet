# Embedding Brimstone

`brimstone_core::runtime::embed` lets a host such as a browser DOM (Servo, through a `mozjs_sys`
compatible layer) drive the engine the way SpiderMonkey embedders do: host objects that never
move, embedder roots, class trace hooks, finalizers, weak pointers, natives, proxies, resolve hooks,
a host-run job queue and realm entry without a JS frame.

When no host is installed none of the embedding code runs and the engine behaves as upstream.

## Quick start

```rust
use brimstone_core::runtime::{ContextBuilder, embed::{self, Host, HostFlags}};

struct MyHost; // keep mutable state in Cell / RefCell, the host is called re-entrantly
impl Host for MyHost { /* override the hooks you need, all have defaults */ }

let cx = ContextBuilder::new().build()?;
embed::install_host(cx, Box::new(MyHost))?;

let (realm, global) = embed::new_global(cx, MY_GLOBAL_CLASS, HostFlags::HAS_RESOLVE, 1)?;
let value = embed::evaluate_script(cx, realm, "1 + 1", "file.js", 1)?;
cx.drop(); // finalizes all remaining host objects
```

## API

### Host trait

All methods take `&self`: the host is called re-entrantly (a native runs JS that calls another
native), so hosts keep mutable state in `Cell`/`RefCell`. Every method has a default.

```rust
pub trait Host: 'static {
    // GC hooks: no JS-heap allocation, handles or JS; they get a `GcHookCx`, which has no allocator.
    fn trace_roots(&self, cx: GcHookCx<'_>, visitor: &mut dyn RootVisitor);
    fn trace_host_object(&self, cx: GcHookCx<'_>, object: HostObjectRef, visitor: &mut dyn RootVisitor);
    fn finalize_host_object(&self, cx: GcHookCx<'_>, object: HostObjectRef);
    fn after_gc_weak(&self, cx: GcHookCx<'_>, updater: &mut dyn WeakUpdater);

    fn call_native(&self, cx: Context, native: *const c_void, call: &mut NativeCall) -> EvalResult<()>;
    fn call_host_object(&self, cx: Context, object: Handle<ObjectValue>, this: Handle<Value>,
                        arguments: &[Handle<Value>], new_target: Option<Handle<ObjectValue>>)
                        -> EvalResult<Handle<Value>>;
    fn resolve(&self, cx: Context, object: Handle<ObjectValue>, key: Handle<PropertyKey>) -> EvalResult<bool>;
    fn enqueue_job(&self, cx: Context, job: HeapPtr<ObjectValue>, promise: Option<HeapPtr<ObjectValue>>,
                   incumbent_global: Option<HeapPtr<ObjectValue>>) -> bool;
    fn interrupt(&self, cx: Context) -> bool;   // false = terminate uncatchably

    // Modules
    fn resolve_module(&self, cx: Context, referrer: Option<Handle<SourceTextModule>>,
                      referrer_file: Handle<FlatString>, specifier: Handle<FlatString>,
                      attributes: Option<Handle<ImportAttributes>>)
                      -> EvalResult<Option<Handle<SourceTextModule>>>;   // None = engine loads the file
    fn dynamic_import(&self, cx: Context, referrer: Option<Handle<SourceTextModule>>,
                      referrer_file: Handle<FlatString>, specifier: Handle<FlatString>,
                      attributes: Option<Handle<ImportAttributes>>, import: DynamicImport)
                      -> EvalResult<bool>;   // true = host finishes it later
    fn populate_import_meta(&self, cx: Context, module: Handle<SourceTextModule>,
                            meta: Handle<ObjectValue>) -> EvalResult<()>;

    // HostPromiseRejectionTracker; must not allocate on the JS heap or run JS
    fn promise_rejection(&self, cx: Context, promise: HeapPtr<PromiseObject>,
                         operation: PromiseRejectionOperation);   // Reject | Handle

    // TrustedScript-style source extraction; None leaves object eval arguments unchanged.
    fn code_for_eval(&self, cx: Context, object: Handle<ObjectValue>)
                     -> EvalResult<Option<Handle<StringValue>>>;

    // CSP before parsing. Original arguments remain rooted and retain object identity.
    fn can_compile_strings(&self, cx: Context, kind: CompileStringsKind, code: Handle<StringValue>,
                           parameters: &[Handle<StringValue>], body: Option<Handle<StringValue>>,
                           parameter_args: &[Handle<Value>], body_arg: Handle<Value>)
                           -> EvalResult<bool>;

    // Proxy internal methods (host objects created with a non-null handler). Defaults are ordinary.
    fn proxy_get_own_property(&self, cx, handler: *const c_void, object, key) -> EvalResult<Option<PropertyDescriptor>>;
    fn proxy_define_property(&self, cx, handler, object, key, desc: PropertyDescriptor) -> EvalResult<bool>;
    fn proxy_own_keys(&self, cx, handler, object) -> EvalResult<Vec<Handle<Value>>>;
    fn proxy_delete(&self, cx, handler, object, key) -> EvalResult<bool>;
    fn proxy_has(&self, cx, handler, object, key) -> EvalResult<bool>;
    fn proxy_get(&self, cx, handler, object, key, receiver: Handle<Value>) -> EvalResult<Handle<Value>>;
    fn proxy_set(&self, cx, handler, object, key, value: Handle<Value>, receiver: Handle<Value>) -> EvalResult<bool>;
    fn proxy_prevent_extensions(&self, cx, handler, object) -> EvalResult<bool>;
    fn proxy_is_extensible(&self, cx, handler, object) -> EvalResult<bool>;
    fn proxy_get_prototype(&self, cx, handler, object) -> EvalResult<Option<Handle<ObjectValue>>>;
    fn proxy_set_prototype(&self, cx, handler, object, prototype: Option<Handle<ObjectValue>>) -> EvalResult<bool>;
}

pub trait RootVisitor {
    fn value(&mut self, bits: &mut u64);          // NaN-boxed Value::as_raw_bits, 0 = uninitialized, skipped
    fn cell(&mut self, ptr: &mut *mut c_void);    // raw heap item pointer, null skipped
}

pub trait WeakUpdater {
    fn update(&mut self, ptr: &mut *mut c_void);  // null if dead, else the (possibly new) address
    fn update_value(&mut self, bits: &mut u64);   // same for a value slot, dead -> undefined
}
```

`HostObjectRef` is a copyable reference to a host object (stable address): `object()`, `class()`,
`flags()`, `reserved_slot(i)`, `set_reserved_slot(i, v)`, `header()` (a copyable `HostHeaderPtr` whose
accessors copy each field in or out and never hand out references), and `unsafe reserved_slots_mut()`
(a `&mut [Value]` the caller must keep as the only access to the slots while it is used).
`HostObjectRef::from_object` panics for an object outside the host space.

`NativeCall` follows the SpiderMonkey `vp` layout: `vp()` returns `*mut u64` with `vp[0]` = callee,
`vp[1]` = this (undefined when constructing), `vp[2..2 + argc]` = arguments; the return value is
written to `vp[0]`. The buffer is rooted and updated by the GC for the duration of the call. Also
`argc()`, `callee()`, `this()`, `arg(i)`, `set_return_value(v)`, `is_constructing()`, `new_target()`.
Throw by returning `Err(embed::throw_value(value))`.

### Functions of `embed`

One line each; see the doc comments for details. `Handle`/`HeapPtr` arguments follow the engine's
rules: handles survive a GC, raw `HeapPtr`s only for host objects (which never move).

Host and host objects
- `install_host(cx, host)` install the host; once per context, before any other call.
- `has_host(cx)` whether a host is installed.
- `new_host_object(cx, realm, class, flags, proto, num_reserved_slots, proxy_handler, proxy_private)` new object in the non-moving host space; a non-null handler makes it a host proxy.
- `new_global(cx, class, flags, num_reserved_slots) -> (realm, global)` new realm whose global is a host object.
- `is_host_object(object)` whether the object lives in the host space.
- `host_class(object)` / `host_flags(object)` / `host_handler(object)` class pointer, flags, proxy handler (None if not a host object / proxy).
- `num_reserved_slots(object)`, `reserved_slot(object, i)`, `set_reserved_slot(object, i, value)` reserved slots (panic out of range).
- `proxy_private(object)`, `set_proxy_private(object, value)` the private value of a host proxy.
- `host_object_count(cx)` number of objects in the host space (pinned engine objects and jobs included).
- `is_pinned(object)` whether an engine object (a promise) is pinned in the host space.
- `is_in_host_space(ptr)` whether any heap pointer is in the host space, so it never moves.
- `assert_stable(cx, ptr)` debug-build check that a pointer is not in the moving semispace (no-op in release).
- `HostObjectRef` (stable reference given to hooks): `object`, `class`, `flags`, `reserved_slot`, `set_reserved_slot`, `header` (`HostHeaderPtr`), `unsafe reserved_slots_mut`.

Natives
- `new_native_function(cx, realm, native, nargs, name, constructor, reserved_slots)` builtin closure calling `Host::call_native` (at most 2 reserved slots).
- `new_native_function_with_jit_info(cx, realm, native, jit_info, nargs, name, constructor, reserved_slots)` the same with a `JSJitInfo` pointer.
- `native_of(cx, function)` the native pointer of such a function, None otherwise.
- `native_jit_info(cx, value) -> Option<*const c_void>` the jit info (`FUNCTION_VALUE_TO_JITINFO`); None for anything that is not a native with info. A field read.
- `set_native_jit_info(cx, function, info)` set or clear (null) it; false if not a native.
- `function_reserved(cx, function, i)` / `set_function_reserved(cx, function, i, value)` its reserved slots (panic for non-natives / missing slots).
- `function_reserved_opt(cx, value, i) -> Option<Value>` the same without panicking.
- `NativeCall`: `vp`, `argc`, `callee`, `this`, `arg`, `set_return_value`, `is_constructing`, `new_target`.

Realms
- `enter_realm(cx, realm)` / `leave_realm(cx, realm)` enter and leave a realm without a JS frame (nest like frames).
- `with_realm(cx, realm, f)` run `f` with the realm entered.
- `entered_realm_depth(cx)` number of entered realms.
- `current_realm(cx)` realm of the newest frame or entry.
- `realm_of_object(cx, object)` creation realm of host objects, function realm of functions, else the current realm.
- `global_of_realm(realm)` the realm's global.
- `object_prototype(realm)` / `function_prototype(realm)` intrinsics.
- `context_of(ptr)` the Context of any heap pointer, host objects included.

Running code and failures
- `evaluate_script(cx, realm, source, file_name, line)` run a script, return its completion value (compile + execute).
- `compile_script(cx, realm, source, file_name, line) -> Handle<CompiledScript>` parse and generate bytecode without running; syntax errors are thrown here.
- `execute_compiled(cx, script)` run a compiled script in its realm, any number of times; `CompiledScript::realm()`.
- `parse_script_off_thread(source, file_name, line) -> Result<ParsedScript, OffThreadSyntaxError>` parse and analyze without a Context, on any thread (`ParsedScript: Send`).
- `compile_parsed_script(cx, realm, parsed)` generate bytecode for an off-thread parse on the context's thread.
- `compile_function(cx, realm, name, params, body, file_name, line)` function from parameter names and body.
- `call(cx, function, this, args)` / `construct(cx, function, args, new_target)` call or construct a value.
- `throw_value(value)` error result that throws `value` (from natives and hooks).
- `uncatchable()` error result that terminates uncatchably (no catch/finally runs).
- `is_uncatchable(&error)`, `failure(error) -> Failure::{Thrown(value), Uncatchable}`, `exception_value_opt(error)` how an evaluation failed.
- `exception_value(error)` the thrown value (panics on an uncatchable termination).

Modules
- `compile_module(cx, realm, source, file_name, line) -> Handle<SourceTextModule>` compile a module (`JS::CompileModule`).
- `module_requests(module) -> Vec<ModuleRequestInfo { specifier, attributes }>` requested specifiers in source order.
- `set_requested_module(module, index, dependency)` / `requested_module(module, index)` supply or read the record for a request.
- `module_link(cx, module)` link the graph, asking `Host::resolve_module` for missing records (`JS::ModuleLink`).
- `module_evaluate(cx, module) -> Handle<PromiseObject>` evaluate; the promise settles after top-level await (`JS::ModuleEvaluate`).
- `module_namespace(cx, module)` the namespace object (`JS::GetModuleNamespace`).
- `module_state(module)`, `module_realm(module)`, `module_file_name(module)`, `module_evaluation_error(cx, module)` record information.
- `set_module_private(module, value)` / `module_private(module)` a traced value on the record (`JS::SetModulePrivate`/`GetModulePrivate`).
- `DynamicImport`: `cell_ptr()` / `from_cell_ptr(ptr)` (root it from `trace_roots`), `promise()`.
- `finish_dynamic_import(cx, import, Ok(module) | Err(value))` complete or fail an import the host took over.

Stacks
- `capture_stack(cx, max_frames) -> Vec<CapturedStackFrame>` the current JS stack, newest first (`JS::CaptureCurrentStack`).
- `error_stack_frames(cx, error, max_frames)` the frames captured when an Error was created (None if not an Error).
- `CapturedStackFrame { function_name, file_name, line, column, is_async_function, is_native }`.

Jobs, interrupts, limits, GC
- `run_engine_tasks(cx)` run tasks left in the engine's queue (refused by the host, or no host).
- `pending_host_jobs(cx)` jobs handed to the host and not run yet.
- `job_realm(job) -> Option<HeapPtr<Realm>>` the realm a job runs in (None if not a job).
- `PromiseObject::is_handled()` `[[PromiseIsHandled]]`; `PromiseRejectionOperation::{Reject, Handle}`.
- `interrupt_handle(cx) -> InterruptHandle` (`Send + Sync + Clone`; `request()`, `is_requested()`).
- `set_stack_limit(cx, bytes)` / `stack_limit(cx)` VM stack limit; exceeding it throws a catchable RangeError.
- `set_native_reentry_limit(cx, depth)` / `native_reentry_limit(cx)` how deeply natives may re-enter JS.
- `set_native_stack_quota(cx, bytes)` / `native_stack_limit(cx)` native stack limit by address (`JS_SetNativeStackQuota`); 0 removes it.
- `heap_stats(cx) -> HeapStats` semispace used/capacity, permanent bytes, heap size and limit, host space used/reserved bytes and object count.
- `gc(cx)` full collection.
- `compile_stats(cx) -> CompileStats` functions generated with their script, created as lazy stubs, generated lazily on their first call, and stubs still pending, with the source bytes of each. Nested functions of scripts and modules are generated on their first call unless `Options::lazy_functions` is off.
- `gc(cx)` full collection. Does nothing while a GC is running (called from a GC hook) or while host
  objects are finalized at teardown.

### Functions of `embed::api`

Re-exports: `Context, EvalResult, Handle, HeapPtr, PropertyDescriptor, PropertyFlags, PropertyKey,
Realm, Value, ObjectValue, AllocResult, BuiltinSymbols, StringWidth, BigInt, FlatString,
StringValue, SymbolValue, BigIntValue, ArrayObject, ByteArray, ArrayBufferObject, DynTypedArray,
TypedArray, TypedArrayKind, PromiseObject, ErrorObject, DataViewObject, DateObject, Intrinsic`;
`get, set, create_data_property, define_property_or_throw, delete_property_or_throw, has_property,
has_own_property, is_extensible, length_of_array_like` (abstract operations);
`to_string, to_number, to_property_key, to_object, to_boolean, to_int32, to_uint32, to_primitive,
ToPrimitivePreferredType, is_array, is_callable, is_constructor_value, same_value,
same_value_zero_non_allocating` (conversions); `create_array_from_list`; and from `embed`:
`evaluate_script, compile_function, call, construct, global_of_realm`.

Objects and properties
- `new_object(cx)` ordinary object with Object.prototype of the current realm.
- `set_property(cx, object, key, value, receiver)` [[Set]], returns success.
- `define_property(cx, object, key, desc)` [[DefineOwnProperty]], returns success.
- `delete_property(cx, object, key)` [[Delete]], returns success.
- `get_own_property_descriptor(cx, object, key)` full descriptor (value/writable or get/set, enumerable, configurable).
- `own_property_keys(cx, object)` own keys as values.
- `get_prototype_of(cx, object)` / `set_prototype_of(cx, object, proto)` prototype.
- `prevent_extensions(cx, object)` make non-extensible (`is_extensible` re-exported).
- `object_identity_hash(object)` stable hash across moves.
- `property_key_from_string(cx, s)`, `property_key_from_symbol(symbol)`, `property_key_from_index(cx, i)`, `property_key_from_atom(cx, atom)` property keys.
- `intrinsic(realm, Intrinsic::...)` any intrinsic of a realm by name.
- `function_name(cx, f)` / `function_length(cx, f)` a function's name and length.

Strings
- `string_from_latin1(cx, bytes)`, `string_from_code_units(cx, units)`, `string_from_utf8(cx, s)` create strings.
- `string_length(s)`, `string_is_one_byte(s)` length in code units, width.
- `unsafe flat_string_chars(flat) -> StringChars::{Latin1, TwoByte}` borrow a flat string's chars.
  The borrow points into the moving heap: nothing may allocate on the JS heap or run a GC while it is used.
- `unsafe with_string_chars_no_gc(string, f)` chars of any string without allocating on the JS heap (ropes
  copied to a Rust buffer). `f` must not allocate on the JS heap or run a GC.
- `flatten_string(s)` flatten a rope (may GC).
- `atomize(cx, s)` / `atomize_str(cx, s)` interned string (equal atoms are the same pointer).
- `string_to_rust_string(s)` lossy Rust copy.

Symbols, BigInt
- `new_symbol(cx, description)`, `symbol_description(symbol)` new symbol, description.
- `symbol_for(cx, key)` / `symbol_key_for(cx, symbol)` registry (`Symbol.for` / `Symbol.keyFor`).
- `well_known_symbol(cx, WellKnownSymbol::...)`, `WellKnownSymbol::from_code(code)` well-known symbols, numbered like `JS::SymbolCode`.
- `bigint_from_i64/u64/f64(cx, v)`, `bigint_to_i64/u64/f64(b)`, `bigint_to_string(cx, b, radix)`.

Arrays, buffers, views (data pointers point into the moving heap: valid until the next GC)
- `array_with_length(cx, realm, len)` new array.
- `array_buffer_new(cx, realm, len)`, `array_buffer_byte_length(b)`, `array_buffer_is_detached(b)`, `array_buffer_data_ptr(b)`, `array_buffer_detach(b)`.
- `create_typed_array(cx, realm, kind, buffer, byte_offset, length)`, `is_typed_array(v)`, `typed_array_kind/element_length/byte_offset/byte_length/data_ptr(array)`.
- `create_data_view(cx, realm, buffer, byte_offset, byte_length)`, `as_data_view(object)`, `data_view_byte_offset/byte_length/data_ptr(view)`.

Promises, errors, JSON, dates
- `new_promise(cx, realm)`, `promise_resolve(cx, p, v)`, `promise_reject(cx, p, v)`, `promise_then(cx, p, on_fulfilled, on_rejected)`, `promise_state(p) -> PromiseStatus`, `promise_result(cx, p)`, `is_promise(v)`.
- `new_error / new_type_error / new_range_error / new_syntax_error(cx, realm, message)` error objects.
- `new_error_with_location(cx, realm, message, file, line)`, `new_error_with_position(..., line, column)` with `fileName`/`lineNumber`/`columnNumber`.
- `error_message(cx, error)`, `error_stack(cx, error)` read an error's message and stack.
- `json_parse(cx, realm, text)`, `json_stringify(cx, realm, value)`.
- `new_date(cx, realm, ms)`, `date_value(object)` Date objects.

Also changed outside the module, for embedders: `Property::to_property_descriptor`,
`Handle<ObjectValue>::{call_virtual, construct_virtual, is_constructor, ordinary_prevent_extensions}`,
`VM::{is_stack_empty, frame_pointer, interrupt_flag, set_stack_limit_slots, set_max_dispatch_depth}`,
`EvalError::Uncatchable`, `Debug for EvalError`, `HeapPtr<StringValue>::{as_flat_no_gc,
for_each_flat_piece_no_gc}`, `Context::task_queue` (now a `TaskEnqueuer`) / `task_queue_mut`, and
`BytecodeProgramGenerator::generate_from_parse_script_result_with_completion_value`.

## Design

### Host space

Brimstone's collector is a Cheney semispace copier: every live object moves on every GC. Servo needs
DOM reflectors, globals and DOM proxies to keep their address for life, so host objects live in a
separate **host space** (`embed/host_space.rs`) that the copier never moves.

- **Regions.** The host space is a list of regions, each a 16 MiB (or larger, for big objects)
  allocation aligned to the heap alignment (4 GiB). The start of each region holds a `HeapInfo`
  copy pointing to the same `Context` and the same handle context as the main heap, so
  `HeapInfo::from_raw_heap_ptr`, `HeapPtr::to_handle` and `embed::context_of` work on host object
  pointers. For this the `HandleContext` is now boxed and `HeapInfo` holds a pointer to it (it used to
  be stored inline); a resizing GC copies the pointer, so nothing in the host space needs updating
  when the heap grows. Membership is tested by masking a pointer with the alignment and comparing it
  against the region bases.
- **Allocation layout.** `| reserved slots | HostHeader | object |`. The object pointer is what the
  rest of the engine sees; it starts with the shape like every heap item. The `HostHeader` holds the
  allocation size, reserved slot count, `HostFlags`, the mark bit, the class pointer, the proxy
  handler, the proxy private value and the creation realm. Freed allocations go onto free lists by
  exact size and are reused; in debug builds freed memory is poisoned with `0xdb`.
- **Kinds.** Ordinary host objects, proxies and callable host objects use the new heap item kind
  `HostObject` (an exotic kind, so inline caches never cache through it). Host globals keep the
  `GlobalObject` kind, because the VM has fast paths for globals; they are just allocated in the
  host space. Host-ness is decided by address, not by kind.

### GC

`GarbageCollector` gets a copy of the host space's region bases (exact for the whole GC, since no host
object can be created during one) and a gray stack when a host is installed:

1. `copy_or_fix_pointer`: a pointer outside the moved spaces that lies in the host space is
   **marked**; if it was unmarked it is pushed on the gray stack. Without a host this branch is never
   taken (the original debug assertion still applies).
2. Roots: `Context::visit_roots_for_gc` additionally visits the embedding state (realm entries,
   pending jobs, native `vp` buffers, the job shape and private keys) and calls
   `Host::trace_roots` through an adapter that turns `RootVisitor::value`/`cell` into GC visits.
3. `move_all_live_items` alternates the Cheney scan with draining the gray stack until both are
   empty. Draining a host object visits its object fields by kind (shape, named and array
   properties, inline properties, and `GlobalObject`'s property map), its header (reserved slots,
   proxy private, realm), and calls `Host::trace_host_object` if `HAS_TRACE` is set.
4. Ephemerons (`WeakMap`, `FinalizationRegistry`): a host-space key is live iff marked; values of
   live host keys are visited and only counted as newly live if they were not yet visited, so the
   fixpoint terminates.
5. Weak resolution (`resolve_weak`): moved space -> forwarding pointer or dead; host space -> mark
   bit (alive at the same address) or dead; anything else (permanent space) -> alive and unmoved.
   This covers WeakRefs, WeakMaps/WeakSets, transition trees, inline caches and interned strings.
6. After the engine's own weak processing, `embed::sweep_after_gc` calls `Host::after_gc_weak` with
   an updater built on `resolve_weak`, then takes every unmarked host object out of the space, calls
   `Host::finalize_host_object` for those with `HAS_FINALIZE` (header and reserved slots intact),
   drops the tasks of dead job objects and puts the memory on the free lists. Survivors are unmarked.
   This runs before the gc_stress overwrite and before from-space is released.
7. The resize path (`GcType::Grow`, `move_permanent_heap`) only moves the semispaces and the
   permanent space; host objects are marked and visited like in a normal GC.

When the context is dropped, every remaining host object with `HAS_FINALIZE` is finalized, then the
host space and the host are dropped.

### Host objects as objects

`impl VirtualObject for Handle<HostObject>`:
- With a proxy handler, every internal method calls the matching `Host::proxy_*` method. The
  non-virtual internal methods in `object_value.rs` (`get_prototype_of`, `set_prototype_of`,
  `is_extensible`, `prevent_extensions`) also dispatch to the host for host proxies, and
  `OrdinarySetPrototypeOf`'s cycle check stops at host proxies like at proxies.
- With `HAS_RESOLVE`, an own property miss calls `Host::resolve` before the prototype walk and
  retries the lookup once if it returns true. `GlobalObject::get_own_property` does the same for
  host globals. A hook is not re-entered for the same object and key while it runs (it usually
  defines the property it resolves, which looks it up again).
- Otherwise the object behaves as an ordinary object.
- `CALLABLE`/`CONSTRUCTOR` host objects are callable: `is_callable_object` and
  `is_constructor_object_value` know them (so `typeof` is "function"), the VM's call paths dispatch
  callable exotic objects (proxies and host objects) through the virtual `[[Call]]`/`[[Construct]]`,
  `GetFunctionRealm` returns the creation realm, and bound functions inherit constructor-ness.
- With `IS_HTMLDDA` the object has the [[IsHTMLDDA]] internal slot of ECMA-262 B.3.6, which is what
  `document.all` needs (SpiderMonkey's `JSCLASS_EMULATES_UNDEFINED`): `typeof` says "undefined",
  `ToBoolean` says false and `==` compares equal to `null` and `undefined`. Nothing else changes:
  `===`, `Object.is`, `??`, `?.` and default parameters still see an object. The slot is one bit on
  a private copy of the shape taken when the object is created, so host objects that were made
  without the flag keep the shared shape and pay nothing for it. `ObjectValue::is_htmldda` is the
  predicate; `ObjectValue::init_htmldda` gives the slot to an object created without it, which is
  how the test262 hook `$262.IsHTMLDDA` (a callable) gets it.

### Natives

A native is a builtin closure whose Rust runtime function is the single trampoline
`EmbedNatives::call`, registered in the runtime function table like every builtin (so heap
serialization keeps working). Each native has its own `BytecodeFunction`; runtime functions have
no bytecode, so its 16 data bytes (where bytecode would be) hold the native pointer and the
`JSJitInfo` pointer. They are opaque to the GC, move with the function and are read with a field
access. Up to two reserved slots are stored on the function under private symbols created at
install time: invisible to JS, updated by the GC. The trampoline builds the rooted `vp` buffer, calls `Host::call_native`, and requires an
object result when constructing.

### Jobs

`Context::task_queue()` returns a `TaskEnqueuer`. With a host installed, each enqueued `Task` is
wrapped in a **job object**: a callable host object (internal flag) created with a shape made at
install time, so wrapping never allocates on the moving heap (tasks are enqueued from places that hold
raw heap pointers). The task is kept in a side table keyed by the job object's address and rooted
from there; `Host::enqueue_job` receives the job, the settled promise when known, and the realm's
global as incumbent global. Calling the job runs the task once (later calls do nothing). If the host
drops a job without running it, the job object dies and its task is dropped. Tasks enqueued during a
GC (FinalizationRegistry cleanups) are handed over when the GC finishes. If `enqueue_job` returns
false the task stays in the engine queue and `run_all_tasks` runs it. Without a host
`run_all_tasks` works as before.

### Realms

`enter_realm` pushes the realm's dummy frame (the one `with_initial_realm_stack_frame` uses) onto the
VM stack and records the frame pointer; `leave_realm` checks that the newest entry and frame match
and pops it. Because entries are VM frames, `cx.current_realm()` naturally prefers the newest entry
over older JS frames, and newer JS frames (a called function, a native) over the entry.
`with_initial_realm_stack_frame` now also works when frames are already on the stack (it pushes the
realm frame on top), which happens when a host runs a job or evaluates a script from inside a native.

### Scripts

`evaluate_script` parses and runs a script in a realm and returns its completion value; the bytecode
generator has an opt-in to track statement completions for scripts as it does for eval. The start line
is applied by prefixing `line - 1` newlines. `compile_function` evaluates
`(function(params) {\nbody\n})` with the line adjusted so the body starts at `line`, then sets `name`.

### Uncatchable termination

`EvalError::Uncatchable` (`embed::uncatchable()`) is what a native returns to fail without an
exception (SpiderMonkey's `return false` with nothing pending), and what a refused interrupt
produces. When the VM sees it from a runtime call it unwinds frames up to and including the nearest
frame called from Rust, without consulting any exception handler, so no `catch` or `finally` runs.
The Rust caller (a native, a job, `call_from_rust`) passes it on with `?`, and the
`completion_value!` macro, `must!` and `IteratorClose` propagate it instead of turning it into a
completion. The host's outermost `evaluate_script`/`call`/`construct` returns it; `failure()` tells
it apart from a thrown value.

### Interrupts and limits

The VM owns an `Arc<AtomicBool>`; `InterruptHandle::request` sets it from any thread. Backward jumps
(including fused compare-and-jump) and JS-to-JS function entry do one relaxed load of it; when set,
the VM clears it and calls `Host::interrupt` with the PC published, so the hook may run JS. The VM
stack limit (`set_stack_limit`) moves the lowest address frames may use; the existing stack-overflow
RangeError is thrown past it. `set_native_reentry_limit` lowers the existing cap on nested dispatch
loops, which bounds native stack use when natives and builtins call back into JS.

### Native stack quota

`set_native_stack_quota(cx, bytes)` records `limit = address of a local in the caller - bytes`
(the stack grows down on x86-64 and aarch64). Every dispatch loop entry and every builtin or native
call (`call_rust_runtime`) compares the address of a local against it, like SpiderMonkey's
`AutoCheckRecursionLimit`, and throws a catchable RangeError "too much recursion" past it. The
dispatch depth cap stays as a second guard. Leave a margin for the deepest native frame between two
checks (debug builds use the most); the test uses 512 KiB threads with a 320 KiB quota.

### Compiled scripts and off-thread parsing

`CompiledScript` is a GC heap item holding the script's program `BytecodeFunction` and its
`GlobalNames`; executing it runs GlobalDeclarationInstantiation and the program each time. The
parser and the analyzer need no Context: `ParsedScript` owns the AST arena together with the
`Rc<Source>`, the `Rc<Options>` and every `Cell` the AST uses, none of which is reachable from
outside, so the bundle is `Send` (not `Sync`) and bytecode is generated on the context's thread.
Off-thread parsing uses the default Options; if the context's Annex B setting differs, the script is
parsed again on the context's thread.

### Modules

The engine's module machinery (`GraphLoader`, linker, evaluator) is unchanged; only
`HostLoadImportedModule` and `import()` consult the host. With a host whose `resolve_module`
answers, the engine neither loads files nor uses its module cache, so the host decides identity
(return the same record for the same resolved URL). Loading is synchronous: records are supplied
up front with `set_requested_module` or synchronously from `resolve_module` while `module_link`
runs. Dynamic `import()` gives the host a `DynamicImport` (the promise capability) to finish later;
`finish_dynamic_import` then loads the record's missing dependencies, links, evaluates and
resolves the import's promise with the namespace through the job queue.

### Pinned engine objects (promises)

With a host installed, every `PromiseObject` (from script, from `Promise.resolve`/`then`/async
functions, and from `api::new_promise`) is allocated in the host space by
`ObjectBuilder::build_pinned`, because Servo uses the JS promise object itself as the reflector of
its `Promise` DOM type and does not trace that reflector. A pinned object has its normal kind,
shape and behaviour; its host header carries the internal `PINNED` flag, no class, no reserved
slots and its creation realm. The GC treats it like any host-space object: marked when reached,
fields visited, live for weak references iff marked, swept (no finalizer) when unmarked, never
copied by a normal or growing GC. The embedding API does not treat pinned objects as host objects
(`is_host_object`, `host_class`, `host_flags`, reserved slots: no), but `realm_of_object` returns
their creation realm. Promise reactions and jobs stay in the semispace. Without a host, promises
are ordinary semispace objects as upstream.

Other objects an embedder might hold untraced, checked against Servo (`components/script`, all
`init_reflector*` / `set_jsobject` call sites):

| Object | Held how in Servo | Decision |
| --- | --- | --- |
| DOM reflectors (`reflect_dom_object`) | `Reflector`, untraced | Created with a DOM `JSClass`, so mozjs-sys makes them host objects: stable. |
| Globals | `Reflector`, untraced | Host objects (`new_global`): stable. |
| `WindowProxy` | `Reflector`, untraced | Host proxy object: stable. |
| `Promise` DOM objects | `Reflector` set to the JS promise (`new_with_js_promise`) | Every promise pinned (this section). |
| Prototype / interface objects | `ProtoOrIfaceArray`, traced by the global's trace hook | May move. |
| Callbacks (`CallbackObject`), scripts, module records | `Heap<*mut ...>`, traced | May move. |
| Typed arrays / ArrayBuffers | `TypedArray` wrappers rooted, `HeapBufferSource` traced | Objects may move; data pointers are only used under `AutoCheckCannotGC`. |
| Plain objects (`JS_NewPlainObject`, no class) | Rooted or traced | Semispace, may move. |

`assert_stable` lets the glue check the reflectors it hands to Servo in debug builds.

### Promise rejection tracking

`PromiseObject` records `[[PromiseIsHandled]]`. `RejectPromise` reports `Reject` for an unhandled
promise, and `PerformPromiseThen` (also used by `await`) reports `Handle` when it adds the first
reaction to an already rejected promise, as SpiderMonkey's `PromiseRejectionTrackerCallback`.
Nothing is reported without a host.

### Job realms

Each job object records the realm its task runs in: the handler's function realm for reactions
(the current realm when there is no handler), the resumed function's realm for `await`
continuations, the `then` function's realm for thenable jobs, and the callback's realm for
FinalizationRegistry cleanups. `enqueue_job`'s `incumbent_global` is that realm's global, and
`job_realm` returns it.

### Stack capture and CSP

Captured frames come from the same frame walk as Error stacks (dummy realm frames are skipped).
Error objects keep their captured frames after `stack` is formatted. `Host::can_compile_strings` is
the HostEnsureCanCompileStrings hook, called before any parsing. Direct and indirect eval
are distinguished. Function constructors supply both their once-converted parameter/body strings
and the original rooted values; zero-argument Function has no body string and an undefined original
body argument, matching SpiderMonkey. HostGetCodeForEval is available through `code_for_eval` so
Servo can extract TrustedScript data without coercing ordinary eval objects.

The LucinaNet browser bridge now invokes both hooks. Its independent acceptance test compares
against SpiderMonkey and forces collections during both callbacks. The ordinary engine, without
a host, keeps the default ECMAScript behavior. ICU/Temporal dependencies are pinned at their tested
versions in Cargo.toml because an embedding workspace does not inherit this repository's lockfile.

## Not done / limitations

- **Heap serialization with a host installed** is not supported: the serializer does not know the host
  space. Serializing and deserializing contexts without a host works as before.
- **`mayResolve`** is not modelled; the resolve hook is consulted on every own property miss of a
  resolve object (string and symbol keys and array indices alike for host objects; named keys only for
  host globals). There is no enumerate hook (`for-in` / `Object.keys` do not ask the host).
- **Proxy hooks on globals**: globals support trace, finalize and resolve, not proxy handlers
  (WindowProxy is a separate host proxy object). `Array.isArray` returns false for host proxies.
  The host proxy hooks are not checked against the proxy invariants of the spec.
- **`getPrototypeIfOrdinary`** has no separate hook; hosts answer it in `proxy_get_prototype`.
- **Ordinary objects do not record their realm**: `realm_of_object` returns the creation realm for host
  objects, the function realm for functions, and the current realm otherwise.
- **Line numbers** are produced with leading newlines, so column numbers on the first line and
  `Function.prototype.toString` are unaffected but the source text contains the padding.
- **GC hooks** (`trace_*`, `after_gc_weak`, `finalize_host_object`) and `enqueue_job` must not
  allocate on the JS heap. The GC hooks and the engine-cell hooks receive a `GcHookCx` instead of a
  `Context` (no allocator, no handles from the context, no script, no GC), so a hook that allocates
  through its arguments does not compile (`tests/gc_hook_types`); what follows is the run-time
  backstop for a host that reaches around the type with a `Context` it kept or recovered
  (`context_of`). A GC never nests: a GC asked for while one runs (by `embed::gc`, also from
  finalizers at teardown) does not run. An allocation on the moving heap from a hook is fatal: the
  collector closes the allocation window for the duration of a collection, so any such allocation
  reaches `Heap::abort_alloc_during_gc`, which prints the size and the name of the hook and calls
  `std::process::abort` (not a panic: unwinding out of a GC leaves the heap half moved, and the
  context unusable). Before that the item was handed back and died with the collection — released
  from-space pages after a `Normal` GC, a freed heap buffer after a resizing one, so reading it is a
  use after free — and only `Options::gc_verify` reported it, as a count at the end of the
  collection. The window covers every host call of the collection, the verification walk that
  `gc_verify` runs after the copy included. Teardown is not a collection, and its window stays open:
  a finalizer that allocates there gets an item that is dropped with the context, and if it does not
  fit the allocation fails as out of memory — no nested collection runs, as it never did. Creating a
  host object during a GC is refused differently
  (`new_host_object` fails; internal host space allocations panic): the GC has already traced the host
  space, so the new object's fields would never be visited.
- **Borrows of the embedding state** (`embed::state` / `try_state`) are `&mut EmbedState` detached
  from the context, so the type system does not stop two of them from overlapping. The rule (see
  `state`) is to re-borrow for each use and never keep one across a call into the host or anything
  else that takes the state; `sweep_after_gc`, `teardown` and `hand_tasks_to_host` follow it, `host()`
  takes only a shared borrow, and the GC tests host-space membership on its own copy of the region
  bases. Making overlap impossible by construction (the state behind a raw pointer or `UnsafeCell`
  with accessors that copy in and out) would touch every use of the state and is not done.
- **Host-space memory** is never returned to the OS before the context is dropped (free lists by exact
  size, no compaction of the host space).
- **Incremental / generational collection** does not exist in Brimstone; every GC is a full GC.
- The host is called with `&self`; mutable host state needs interior mutability.
- **Uncatchable termination** inside module evaluation and async generator steps (which return
  `AllocResult` internally) is reported as a throw of `undefined` there, i.e. it rejects the module
  or generator promise instead of propagating.
- **Interrupts** are checked on backward jumps and JS-to-JS calls only; a long-running builtin (e.g.
  sorting a huge array) is not interrupted until it returns to bytecode.
- **Stack limit messages** are the engine's existing "Stack Overflow" RangeError, not SpiderMonkey's
  "too much recursion" InternalError. Native re-entry uses native stack (most in debug builds), so a
  host on threads smaller than 8 MiB should lower `set_native_reentry_limit`.
- **Async stacks**: Brimstone resumes an awaiting function from the job queue without its callers,
  so a captured stack ends at the async frame (`is_async_function` marks it); there is no
  `asyncCause` chain to the code that started the async function.
- **Modules**: no asynchronous static loading (SpiderMonkey's `ModuleLoadHook` that finishes
  loading later); static dependencies must be available synchronously. No separate module
  `JSScript` (`module_file_name` and `module_realm` cover what it is used for). `module_requests`
  reports attributes as strings; `with { type: "json" }` modules must be built by the host
  (the engine's JSON module support is only used by its own file loader). The engine's module
  cache is not consulted when the host resolves modules.
- **Off-thread parsing** covers classic scripts only, not modules or functions.
- **CSP** reports `CompileStringsKind::Function` for all four function constructors.
- **Pinned promises cost more to allocate** than semispace objects (free-list allocation and a
  sweep entry per promise); a page creating very many promises pays for it.
- **Snapshot tests** (`cargo test -p brimstone_tests`): `bytecode_snapshot_tests` and
  `error_snapshot_tests` fail on the base commit 64d6698 as well (a parse error in
  `call_assignment.js` and an out-of-memory panic); unchanged by this work.
