/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! ES modules: SpiderMonkey's module API (`js/public/Modules.h`, `js/src/vm/Modules.cpp` in mozjs_sys 153.6.0-1) on the
//! fork's host-driven modules (`embed::compile_module`, `module_link`, `module_evaluate`, ...).
//!
//! What Servo holds and what it is here:
//! - **A module record** (`JSObject*`, SpiderMonkey's `ModuleObject`): a host object of `MODULE_CLASS`, so it keeps its
//!   address and has the module's realm. Servo keeps records traced (`ModuleTree::record`, a `Heap<*mut JSObject>`), but it
//!   also puts them in values (`import.meta.resolve`'s reserved slot) and asks for their global (`GlobalScope::from_object`),
//!   so they must be real objects. Slot 0 holds the engine's record (a `SourceTextModule` or `SyntheticModule` heap item),
//!   slot 1 Servo's private (`JS::SetModulePrivate`). A source text record points back at its object through its
//!   `[[HostDefined]]`, so the two live and die together.
//! - **A module's script** (`JSScript*`, `JS::GetModuleScript`, the load hook's referrer): the `SourceTextModule` itself.
//!   It may move; Servo keeps referrers in traced `Heap<*mut JSScript>` slots, as it does compiled scripts.
//! - **A module request** (the load hook's `moduleRequest`): a host object of `MODULE_REQUEST_CLASS` with the specifier,
//!   the module type and the index of the request in its module (-1 for `import()`).
//! - **The payload**: a GraphLoadingState host object (`GRAPH_LOADING_STATE_CLASS`) for static imports, the `import()`
//!   promise for dynamic ones (promises are pinned by the fork, so the address is stable too).
//!
//! Static imports load as SpiderMonkey's `InnerModuleLoading` / `ContinueModuleLoading` do: here, on our side, calling
//! `Hooks::module_load` for every request that has no record yet; Servo answers later with `JS::FinishLoadingImportedModule*`.
//! When the graph is complete the records are all in place, so the engine's `module_link` never needs to ask for one.

use ::std::cell::RefCell;
use ::std::collections::{HashMap, HashSet};

use brimstone_core::runtime::embed::{self, api, DynamicImport, HostFlags};
use brimstone_core::runtime::eval_result::EvalError;
use brimstone_core::runtime::gc::{HandleScope, HeapPtr};
use brimstone_core::runtime::module::module::{Module, ModuleEnum};
use brimstone_core::runtime::module::module_namespace_object::ModuleNamespaceObject;
use brimstone_core::runtime::module::source_text_module::{ModuleState, SourceTextModule};
use brimstone_core::runtime::module::synthetic_module::SyntheticModule;
use brimstone_core::runtime::module::import_attributes::ImportAttributes;
use brimstone_core::runtime::string_value::FlatString;
use brimstone_core::runtime::{Context as Bs, EvalResult, Handle, Value as BsValue};

use super::cx::{current_raw, cx_of};
use super::func::non_zero;
use super::realm::object_ptr;
use super::util::fail;
use crate::boundary::value_bits;
use crate::jsapi::*;

// ---- The objects Servo sees ----

const fn host_class(name: &'static ::std::ffi::CStr, slots: u32, ops: *const JSClassOps) -> JSClass {
    JSClass {
        name: name.as_ptr(),
        flags: slots << JSCLASS_RESERVED_SLOTS_SHIFT,
        cOps: ops,
        spec: ::std::ptr::null(),
        ext: ::std::ptr::null(),
        oOps: ::std::ptr::null(),
    }
}

const fn finalize_ops(finalize: JSFinalizeOp) -> JSClassOps {
    JSClassOps {
        addProperty: None,
        delProperty: None,
        enumerate: None,
        newEnumerate: None,
        resolve: None,
        mayResolve: None,
        finalize,
        call: None,
        construct: None,
        trace: None,
    }
}

const RECORD_SLOT: u32 = 0;
const PRIVATE_SLOT: u32 = 1;
static MODULE_OPS: JSClassOps = finalize_ops(Some(finalize_module));
static MODULE_CLASS: JSClass = host_class(c"ModuleObject", 2, &MODULE_OPS);

const SPECIFIER_SLOT: u32 = 0;
const TYPE_SLOT: u32 = 1;
const INDEX_SLOT: u32 = 2;
static MODULE_REQUEST_CLASS: JSClass = host_class(c"ModuleRequestObject", 3, ::std::ptr::null());

const HOST_DEFINED_SLOT: u32 = 0;
const PROMISE_SLOT: u32 = 1;
static GRAPH_LOADING_STATE_OPS: JSClassOps = finalize_ops(Some(finalize_loading_state));
static GRAPH_LOADING_STATE_CLASS: JSClass = host_class(c"GraphLoadingStateRecordObject", 2, &GRAPH_LOADING_STATE_OPS);

/// GraphLoadingState's fields that are not values (those live in the state object's slots and are traced there).
struct LoadingState {
    is_loading: bool,
    pending: usize,
    /// `[[Visited]]`, by module id (records move, ids do not).
    visited: HashSet<usize>,
    /// `JS::LoadRequestedModules` with callbacks instead of a promise.
    callbacks: Option<(JS::LoadModuleResolvedCallback, JS::LoadModuleRejectedCallback)>,
}

thread_local! {
    /// Loading states by state object address (host objects never move); dropped when the object is finalized.
    static STATES: RefCell<HashMap<usize, LoadingState>> = RefCell::new(HashMap::new());
    /// Every live module object, for `JS::GetModuleForNamespace`.
    static MODULES: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

#[super::poison::engine_shim(gc)]
unsafe extern "C" fn finalize_module(_gcx: *mut JS::GCContext, obj: *mut JSObject) {
    MODULES.with(|m| m.borrow_mut().remove(&(obj as usize)));
    // SpiderMonkey releases the private when the module's ScriptSourceObject is finalized.
    let private = embed::reserved_slot(object_ptr(obj), PRIVATE_SLOT);
    unsafe { release_private(private.as_raw_bits()) };
}

#[super::poison::engine_shim(gc)]
unsafe extern "C" fn finalize_loading_state(_gcx: *mut JS::GCContext, obj: *mut JSObject) {
    STATES.with(|s| s.borrow_mut().remove(&(obj as usize)));
}

unsafe fn release_private(bits: u64) {
    let cx = current_raw();
    if bits == value_bits::undefined() || cx.is_null() {
        return;
    }
    if let Some(release) = unsafe { cx_of(cx) }.hooks.script_private_release {
        let v = JS::Value { asBits_: bits };
        unsafe { release(&v) };
    }
}

/// An engine module record.
#[derive(Clone, Copy)]
enum Record {
    Source(Handle<SourceTextModule>),
    Synthetic(Handle<SyntheticModule>),
}

impl Record {
    fn from_enum(m: ModuleEnum) -> Record {
        match m {
            ModuleEnum::SourceText(m) => Record::Source(m),
            ModuleEnum::Synthetic(m) => Record::Synthetic(m),
        }
    }

    fn bits(self) -> u64 {
        match self {
            Record::Source(m) => (*m).as_ptr() as u64,
            Record::Synthetic(m) => (*m).as_ptr() as u64,
        }
    }
}

/// The engine record behind a module object.
unsafe fn record_of(obj: *mut JSObject) -> Record {
    let v = embed::reserved_slot(object_ptr(obj), RECORD_SLOT);
    Record::from_enum(ModuleEnum::from_any(v.as_pointer()))
}

/// A `JSObject*` given to us as a module object.
fn is_module_object(obj: *mut JSObject) -> bool {
    !obj.is_null() && embed::host_class(object_ptr(obj)) == Some((&MODULE_CLASS as *const JSClass).cast())
}

/// The module object of a source text record (made with the record, kept in its `[[HostDefined]]`).
fn object_of_source(m: Handle<SourceTextModule>) -> *mut JSObject {
    let v = embed::module_private(m);
    if v.is_object() { v.as_object().as_ptr().cast() } else { ::std::ptr::null_mut() }
}

/// The realm a record belongs to.
fn realm_of(bs: Bs, obj: *mut JSObject) -> Handle<api::Realm> {
    embed::realm_of_object(bs, object_ptr(obj)).to_handle()
}

/// Wraps a new engine record in its module object.
fn new_module_object(bs: Bs, realm: Handle<api::Realm>, record: Record) -> EvalResult<*mut JSObject> {
    let obj = embed::new_host_object(
        bs,
        realm,
        (&MODULE_CLASS as *const JSClass).cast(),
        HostFlags::HAS_FINALIZE,
        None,
        2,
        ::std::ptr::null(),
        bs.undefined(),
    )?;
    embed::set_reserved_slot(*obj, RECORD_SLOT, BsValue::from_raw_bits(record.bits()));
    embed::set_reserved_slot(*obj, PRIVATE_SLOT, BsValue::undefined());
    if let Record::Source(m) = record {
        embed::set_module_private(m, *obj.as_value());
    }
    let p: *mut JSObject = (*obj).as_ptr().cast();
    MODULES.with(|m| m.borrow_mut().insert(p as usize));
    Ok(p)
}

// ---- Rooting for what we hand to Servo's hooks ----

/// `n` value slots of `Cx::frame_values` (traced and updated by every GC) while `f` runs; None when the frame stack is full.
unsafe fn with_slots<R>(n: usize, f: impl FnOnce(*mut u64) -> R) -> Option<R> {
    unsafe { super::func::with_slots(super::cx::current_ptr(), n, f) }
}

#[inline]
fn handle<T>(slot: *mut u64) -> JS::Handle<T> {
    JS::Handle { _phantom_0: Default::default(), ptr: slot.cast() }
}

fn too_much_recursion(bs: Bs) -> EvalError {
    match brimstone_core::runtime::error::range_error::<()>(bs, "too much recursion") {
        Err(e) => e,
        Ok(()) => embed::uncatchable(),
    }
}

/// Takes the pending exception as an engine error (an uncatchable stop when nothing is pending).
fn take_pending_error(bs: Bs, cx: *mut JSContext) -> EvalError {
    match unsafe { cx_of(cx) }.take_pending() {
        Some(bits) => embed::throw_value(BsValue::from_raw_bits(non_zero(bits)).to_handle(bs)),
        None => embed::uncatchable(),
    }
}

// ---- Module requests ----

fn module_type(attributes: &[(Handle<FlatString>, Handle<FlatString>)]) -> JS::ModuleType {
    for (key, value) in attributes {
        if key.to_string() != "type" {
            continue;
        }
        return match value.to_string().as_str() {
            "json" => JS::ModuleType::JSON,
            "css" => JS::ModuleType::CSS,
            "bytes" => JS::ModuleType::Bytes,
            "text" => JS::ModuleType::Text,
            _ => JS::ModuleType::Unknown,
        };
    }
    JS::ModuleType::JavaScript
}

/// The first attribute key SpiderMonkey does not support (it knows only `type`).
fn unsupported_attribute(attributes: &[(Handle<FlatString>, Handle<FlatString>)]) -> Option<String> {
    attributes.iter().map(|(k, _)| k.to_string()).find(|k| k != "type")
}

fn attribute_pairs(attributes: Option<Handle<ImportAttributes>>) -> Vec<(Handle<FlatString>, Handle<FlatString>)> {
    attributes.map(|a| a.pairs()).unwrap_or_default()
}

fn new_request(
    bs: Bs,
    specifier: Handle<FlatString>,
    ty: JS::ModuleType,
    index: i32,
) -> EvalResult<*mut JSObject> {
    let realm = embed::current_realm(bs);
    let obj = embed::new_host_object(
        bs,
        realm,
        (&MODULE_REQUEST_CLASS as *const JSClass).cast(),
        HostFlags::empty(),
        None,
        3,
        ::std::ptr::null(),
        bs.undefined(),
    )?;
    embed::set_reserved_slot(*obj, SPECIFIER_SLOT, *specifier.as_value());
    embed::set_reserved_slot(*obj, TYPE_SLOT, BsValue::smi(ty as i32));
    embed::set_reserved_slot(*obj, INDEX_SLOT, BsValue::smi(index));
    Ok((*obj).as_ptr().cast())
}

fn request_index(obj: *mut JSObject) -> Option<usize> {
    let i = embed::reserved_slot(object_ptr(obj), INDEX_SLOT).as_smi();
    if i < 0 { None } else { Some(i as usize) }
}

// ---- HostLoadImportedModule ----

/// `js::HostLoadImportedModule`: hands one request to Servo's load hook. A hook that fails finishes the load with its error.
unsafe fn host_load_imported_module(
    bs: Bs,
    referrer: Option<Handle<SourceTextModule>>,
    request: *mut JSObject,
    host_defined: Handle<BsValue>,
    payload: *mut JSObject,
) -> bool {
    let cx = current_raw();
    let Some(hook) = unsafe { cx_of(cx) }.hooks.module_load else {
        let realm = embed::current_realm(bs);
        return match api::new_error(bs, realm, "Module load hook not set") {
            Ok(e) => unsafe { finish_failed(bs, payload, e.as_value()) },
            Err(e) => unsafe { fail(cx, e) },
        };
    };
    let referrer_bits = referrer.map_or(0, |m| (*m).as_ptr() as u64);
    // Servo reads the referrer's private with `JS_GetScriptPrivate`, which looks in `Cx::script_privates`: the module's
    // private is put there for the hook's duration.
    let private = referrer
        .map(object_of_source)
        .filter(|o| !o.is_null())
        .map(|o| embed::reserved_slot(object_ptr(o), PRIVATE_SLOT).as_raw_bits())
        .filter(|&b| b != value_bits::undefined());
    let r = unsafe {
        with_slots(4, |s| {
            let null_script: *mut JSScript = ::std::ptr::null_mut();
            *s = if referrer_bits == 0 { value_bits::undefined() } else { referrer_bits };
            *s.add(1) = request as u64;
            *s.add(2) = host_defined.as_raw_bits();
            *s.add(3) = payload as u64;
            let referrer_handle: JS::Handle<*mut JSScript> = if referrer_bits == 0 {
                JS::Handle { _phantom_0: Default::default(), ptr: &null_script }
            } else {
                handle(s)
            };
            let entry = private.map(|p| {
                let c = cx_of(cx);
                c.script_privates.push((referrer_bits, p));
                c.script_privates.len() - 1
            });
            let ok = hook(cx, referrer_handle, handle(s.add(1)), handle(s.add(2)), handle(s.add(3)), 0, JS::ColumnNumberOneOrigin { _base: 1 });
            if let Some(i) = entry {
                let c = cx_of(cx);
                if i < c.script_privates.len() {
                    c.script_privates.remove(i);
                }
            }
            ok
        })
    };
    match r {
        Some(true) => true,
        Some(false) => {
            if unsafe { cx_of(cx) }.exception.is_some() {
                unsafe { finish_failed_pending(bs, payload) }
            } else {
                unsafe { finish_failed(bs, payload, bs.undefined()) }
            }
        },
        None => unsafe { fail(cx, too_much_recursion(bs)) },
    }
}

// ---- GraphLoadingState: InnerModuleLoading and ContinueModuleLoading ----

fn is_loading(state: *mut JSObject) -> bool {
    STATES.with(|s| s.borrow().get(&(state as usize)).is_some_and(|s| s.is_loading))
}

fn is_loading_state(obj: *mut JSObject) -> bool {
    embed::host_class(object_ptr(obj)) == Some((&GRAPH_LOADING_STATE_CLASS as *const JSClass).cast())
}

fn state_slot(bs: Bs, state: *mut JSObject, slot: u32) -> Handle<BsValue> {
    embed::reserved_slot(object_ptr(state), slot).to_handle(bs)
}

/// The end of a load: `[[PromiseCapability]]` (or the callbacks) settles.
unsafe fn settle(bs: Bs, state: *mut JSObject, error: Option<Handle<BsValue>>) -> bool {
    let cx = current_raw();
    let callbacks = STATES.with(|s| s.borrow().get(&(state as usize)).and_then(|s| s.callbacks));
    let host_defined = state_slot(bs, state, HOST_DEFINED_SLOT);
    let r = unsafe {
        with_slots(3, |s| {
            *s = host_defined.as_raw_bits();
            *s.add(1) = error.map_or(value_bits::undefined(), |e| e.as_raw_bits());
            match callbacks {
                Some((resolved, rejected)) => match error {
                    None => resolved.is_none_or(|f| f(cx, handle(s))),
                    Some(_) => rejected.is_none_or(|f| f(cx, handle(s), handle(s.add(1)))),
                },
                None => {
                    *s.add(2) = state_slot(bs, state, PROMISE_SLOT).as_raw_bits();
                    match error {
                        None => super::promise::JS__ResolvePromise(cx, handle(s.add(2)), handle(s.add(1))),
                        Some(_) => super::promise::JS__RejectPromise(cx, handle(s.add(2)), handle(s.add(1))),
                    }
                },
            }
        })
    };
    match r {
        Some(ok) => ok,
        None => unsafe { fail(cx, too_much_recursion(bs)) },
    }
}

/// `InnerModuleLoading(state, module, recursive-load)`.
unsafe fn inner_module_loading(bs: Bs, state: *mut JSObject, module: Record) -> bool {
    let cx = current_raw();
    if let Record::Source(m) = module {
        let first_visit = embed::module_state(m) == ModuleState::New
            && STATES.with(|s| s.borrow_mut().get_mut(&(state as usize)).is_some_and(|s| s.visited.insert(m.id())));
        if first_visit {
            let requests = embed::module_requests(m);
            STATES.with(|s| {
                if let Some(s) = s.borrow_mut().get_mut(&(state as usize)) {
                    s.pending += requests.len();
                }
            });
            for (index, request) in requests.iter().enumerate() {
                let ok = if let Some(key) = unsupported_attribute(&request.attributes) {
                    let realm = embed::current_realm(bs);
                    let text = format!("Unsupported import attribute: {key}");
                    match api::new_syntax_error(bs, realm, &text) {
                        Ok(e) => unsafe { continue_module_loading(bs, state, Err(e.as_value())) },
                        Err(e) => unsafe { fail(cx, e) },
                    }
                } else if let Some(dep) = loaded_module(m, index) {
                    unsafe { inner_module_loading(bs, state, dep) }
                } else {
                    let ty = module_type(&request.attributes);
                    match new_request(bs, request.specifier, ty, index as i32) {
                        Ok(req) => {
                            let host_defined = state_slot(bs, state, HOST_DEFINED_SLOT);
                            unsafe { host_load_imported_module(bs, Some(m), req, host_defined, state) }
                        },
                        Err(e) => unsafe { fail(cx, e) },
                    }
                };
                if !ok {
                    return false;
                }
                if !is_loading(state) {
                    return true;
                }
            }
        }
    }
    let done = STATES.with(|s| {
        let mut s = s.borrow_mut();
        let Some(s) = s.get_mut(&(state as usize)) else { return false };
        s.pending = s.pending.saturating_sub(1);
        if s.pending == 0 {
            s.is_loading = false;
            true
        } else {
            false
        }
    });
    // The engine moves the visited records from new to unlinked itself when `JS::ModuleLink` walks the (now complete) graph.
    if done { unsafe { settle(bs, state, None) } } else { true }
}

/// `ContinueModuleLoading(state, moduleCompletion)`.
unsafe fn continue_module_loading(bs: Bs, state: *mut JSObject, completion: Result<Record, Handle<BsValue>>) -> bool {
    if !is_loading(state) {
        return true;
    }
    match completion {
        Ok(module) => unsafe { inner_module_loading(bs, state, module) },
        Err(error) => {
            STATES.with(|s| {
                if let Some(s) = s.borrow_mut().get_mut(&(state as usize)) {
                    s.is_loading = false;
                }
            });
            unsafe { settle(bs, state, Some(error)) }
        },
    }
}

fn loaded_module(m: Handle<SourceTextModule>, index: usize) -> Option<Record> {
    if !m.has_loaded_module_at(index) {
        return None;
    }
    Some(Record::from_enum(ModuleEnum::from_any(*m.get_loaded_module_at(index).as_any())))
}

/// `js::LoadRequestedModules`, with either a promise or the two callbacks.
unsafe fn load_requested_modules(
    cx: *mut JSContext,
    module: *mut JSObject,
    host_defined: u64,
    callbacks: Option<(JS::LoadModuleResolvedCallback, JS::LoadModuleRejectedCallback)>,
) -> Option<*mut JSObject> {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let record = unsafe { record_of(module) };
    let host_defined = BsValue::from_raw_bits(non_zero(host_defined)).to_handle(bs);
    let promise = if callbacks.is_none() {
        match api::new_promise(bs, realm) {
            Ok(p) => Some(p.as_object()),
            Err(e) => {
                unsafe { fail(cx, e.into()) };
                scope.exit();
                return None;
            },
        }
    } else {
        None
    };
    let out = match record {
        // Step 1 for synthetic modules: nothing to load.
        Record::Synthetic(_) => {
            let ok = match (callbacks, promise) {
                (Some((resolved, _)), _) => unsafe {
                    with_slots(1, |s| {
                        *s = host_defined.as_raw_bits();
                        resolved.is_none_or(|f| f(cx, handle(s)))
                    })
                }
                .unwrap_or(false),
                (None, Some(p)) => api::promise_resolve(bs, p.as_opt::<api::PromiseObject>().unwrap(), bs.undefined())
                    .map_err(|e| unsafe { fail(cx, e) })
                    .is_ok(),
                (None, None) => true,
            };
            ok.then(|| promise.map_or(::std::ptr::null_mut(), |p| (*p).as_ptr().cast()))
        },
        Record::Source(_) => {
            let state = embed::new_host_object(
                bs,
                realm,
                (&GRAPH_LOADING_STATE_CLASS as *const JSClass).cast(),
                HostFlags::HAS_FINALIZE,
                None,
                2,
                ::std::ptr::null(),
                bs.undefined(),
            );
            match state {
                Ok(state) => {
                    embed::set_reserved_slot(*state, HOST_DEFINED_SLOT, *host_defined);
                    embed::set_reserved_slot(*state, PROMISE_SLOT, promise.map_or(BsValue::undefined(), |p| *p.as_value()));
                    let state: *mut JSObject = (*state).as_ptr().cast();
                    STATES.with(|s| {
                        s.borrow_mut().insert(
                            state as usize,
                            LoadingState { is_loading: true, pending: 1, visited: HashSet::new(), callbacks },
                        )
                    });
                    if unsafe { inner_module_loading(bs, state, record) } {
                        Some(promise.map_or(::std::ptr::null_mut(), |p| (*p).as_ptr().cast()))
                    } else {
                        // Abandoned without settling (SpiderMonkey: only out of memory gets here).
                        STATES.with(|s| {
                            if let Some(s) = s.borrow_mut().get_mut(&(state as usize)) {
                                s.is_loading = false;
                            }
                        });
                        None
                    }
                },
                Err(e) => {
                    unsafe { fail(cx, e.into()) };
                    None
                },
            }
        },
    };
    scope.exit();
    out
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__LoadRequestedModules(
    cx: *mut JSContext,
    module: JS::Handle<*mut JSObject>,
    hostDefined: JS::Handle<JS::Value>,
    resolved: JS::LoadModuleResolvedCallback,
    rejected: JS::LoadModuleRejectedCallback,
) -> bool {
    let hd = unsafe { (*hostDefined.ptr).asBits_ };
    unsafe { load_requested_modules(cx, *module.ptr, hd, Some((resolved, rejected))) }.is_some()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__LoadRequestedModules1(
    cx: *mut JSContext,
    module: JS::Handle<*mut JSObject>,
    hostDefined: JS::Handle<JS::Value>,
    promiseOut: JS::MutableHandle<*mut JSObject>,
) -> bool {
    let hd = unsafe { (*hostDefined.ptr).asBits_ };
    match unsafe { load_requested_modules(cx, *module.ptr, hd, None) } {
        Some(p) => {
            unsafe { *promiseOut.ptr = p };
            true
        },
        None => false,
    }
}

// ---- FinishLoadingImportedModule ----

/// Step 1: records `result` as the referrer's module for the request (static requests only: Brimstone keeps
/// `[[LoadedModules]]` per requested module).
unsafe fn record_loaded(referrer: *mut JSScript, request: *mut JSObject, result: Record) {
    if referrer.is_null() || request.is_null() {
        return;
    }
    let Some(index) = request_index(request) else { return };
    let Some(mut m) = BsValue::from_raw_bits(referrer as u64).as_opt::<SourceTextModule>() else { return };
    if index < embed::module_requests(m.to_handle()).len() && !m.has_loaded_module_at(index) {
        let dyn_module = match result {
            Record::Source(r) => r.as_dyn_module(),
            Record::Synthetic(r) => r.as_dyn_module(),
        };
        m.set_loaded_module_at(index, dyn_module);
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__FinishLoadingImportedModule(
    cx: *mut JSContext,
    referrer: JS::Handle<*mut JSScript>,
    moduleRequest: JS::Handle<*mut JSObject>,
    payload: JS::Handle<JS::Value>,
    result: JS::Handle<*mut JSObject>,
    _usePromise: bool,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let record = unsafe { record_of(*result.ptr) };
    unsafe { record_loaded(*referrer.ptr, *moduleRequest.ptr, record) };
    let payload = BsValue::from_raw_bits(non_zero(unsafe { (*payload.ptr).asBits_ }));
    let ok = if payload.is_object() && is_loading_state(payload.as_object().as_ptr().cast()) {
        unsafe { continue_module_loading(bs, payload.as_object().as_ptr().cast(), Ok(record)) }
    } else if let Some(promise) = api::as_promise(payload.to_handle(bs)) {
        unsafe { continue_dynamic_import(bs, promise, *result.ptr, record) }
    } else {
        unsafe { super::property::fail_type(cx, "FinishLoadingImportedModule: the payload is neither a loading state nor a promise") }
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__FinishLoadingDynamicImportedModule(
    cx: *mut JSContext,
    referrer: JS::Handle<*mut JSScript>,
    moduleRequest: JS::Handle<*mut JSObject>,
    _payload: JS::Handle<JS::Value>,
    result: JS::Handle<*mut JSObject>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let record = unsafe { record_of(*result.ptr) };
    unsafe { record_loaded(*referrer.ptr, *moduleRequest.ptr, record) };
    scope.exit();
    true
}

/// The failure paths: a loading state stops and rejects, an `import()` promise is rejected.
unsafe fn finish_failed(bs: Bs, payload: *mut JSObject, error: Handle<BsValue>) -> bool {
    if is_loading_state(payload) {
        return unsafe { continue_module_loading(bs, payload, Err(error)) };
    }
    let cx = current_raw();
    unsafe {
        with_slots(2, |s| {
            *s = payload as u64;
            *s.add(1) = error.as_raw_bits();
            super::promise::JS__RejectPromise(cx, handle(s), handle(s.add(1)))
        })
    }
    .unwrap_or(false)
}

unsafe fn finish_failed_pending(bs: Bs, payload: *mut JSObject) -> bool {
    let bits = unsafe { cx_of(current_raw()) }.take_pending().unwrap_or(value_bits::undefined());
    let error = BsValue::from_raw_bits(non_zero(bits)).to_handle(bs);
    unsafe { finish_failed(bs, payload, error) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__FinishLoadingImportedModuleFailed(
    cx: *mut JSContext,
    payload: JS::Handle<JS::Value>,
    error: JS::Handle<JS::Value>,
) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let p = BsValue::from_raw_bits(non_zero(unsafe { (*payload.ptr).asBits_ }));
    let e = BsValue::from_raw_bits(non_zero(unsafe { (*error.ptr).asBits_ })).to_handle(bs);
    let ok = if p.is_object() {
        unsafe { finish_failed(bs, p.as_object().as_ptr().cast(), e) }
    } else {
        unsafe { super::property::fail_type(cx, "FinishLoadingImportedModuleFailed: the payload is not an object") }
    };
    scope.exit();
    ok
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__FinishLoadingImportedModuleFailedWithPendingException(
    cx: *mut JSContext,
    payload: JS::Handle<JS::Value>,
) -> bool {
    let bits = unsafe { cx_of(cx) }.take_pending().unwrap_or(value_bits::undefined());
    let error = JS::Value { asBits_: bits };
    unsafe { JS__FinishLoadingImportedModuleFailed(cx, payload, JS::Handle { _phantom_0: Default::default(), ptr: &error }) }
}

// ---- Dynamic import ----

/// `ContinueDynamicImport` once the record and its dependencies are loaded: link, evaluate, and resolve the `import()`
/// promise with the namespace when evaluation finishes (or reject it with the link or evaluation error).
unsafe fn continue_dynamic_import(bs: Bs, promise: Handle<api::PromiseObject>, module: *mut JSObject, record: Record) -> bool {
    let cx = current_raw();
    let realm = realm_of(bs, module);
    let r: EvalResult<Handle<BsValue>> = (|| {
        let (namespace, evaluation) = match record {
            Record::Source(m) => {
                embed::module_link(bs, m)?;
                let evaluation = embed::module_evaluate(bs, m)?;
                (embed::module_namespace(bs, m)?, evaluation)
            },
            Record::Synthetic(mut m) => embed::with_realm(bs, realm, |cx| -> EvalResult<_> {
                let evaluation = m.evaluate(cx)?;
                Ok((m.get_namespace_object(cx)?.as_object().to_handle(), evaluation))
            })?,
        };
        // `fulfilledClosure`: a function that returns the namespace.
        let make = api::compile_function(bs, realm, None, &["ns"], "return function () { return ns; };", "", 1)?;
        let fulfilled = embed::call(bs, make.as_value(), bs.undefined(), &[namespace.as_value()])?;
        api::promise_then(bs, evaluation, Some(fulfilled.as_object()), None)
    })();
    let resolution = match r {
        Ok(v) => Ok(v),
        Err(e) => match embed::exception_value_opt(e) {
            Some(v) => Err(v),
            None => return false,
        },
    };
    unsafe {
        with_slots(2, |s| {
            *s = (*promise.as_object()).as_ptr() as u64;
            match resolution {
                Ok(v) => {
                    *s.add(1) = v.as_raw_bits();
                    super::promise::JS__ResolvePromise(cx, handle(s), handle(s.add(1)))
                },
                Err(e) => {
                    *s.add(1) = e.as_raw_bits();
                    super::promise::JS__RejectPromise(cx, handle(s), handle(s.add(1)))
                },
            }
        })
    }
    .unwrap_or(false)
}

/// `Host::dynamic_import` (from `cx::ServoHost`): SpiderMonkey's `TryStartDynamicModuleImport`. Servo's load hook gets the
/// request with the `import()` promise as payload and settles it later; we always take the import over.
pub fn dynamic_import(
    bs: Bs,
    referrer: Option<Handle<SourceTextModule>>,
    specifier: Handle<FlatString>,
    attributes: Option<Handle<ImportAttributes>>,
    import: DynamicImport,
) -> EvalResult<bool> {
    let promise: *mut JSObject = (*import.promise()).as_ptr().cast();
    let pairs = attribute_pairs(attributes);
    if let Some(key) = unsupported_attribute(&pairs) {
        let realm = embed::current_realm(bs);
        let e = api::new_type_error(bs, realm, &format!("Unsupported import attribute: {key}"))?;
        unsafe { finish_failed(bs, promise, e.as_value()) };
        return Ok(true);
    }
    let request = new_request(bs, specifier, module_type(&pairs), -1)?;
    unsafe { host_load_imported_module(bs, referrer, request, bs.undefined(), promise) };
    Ok(true)
}

/// `Host::resolve_module` (from `cx::ServoHost`). Records come from `JS::LoadRequestedModules` and Servo's
/// `FinishLoadingImportedModule`; linking a graph that was not loaded is an error, as in SpiderMonkey.
pub fn resolve_module(bs: Bs, specifier: Handle<FlatString>) -> EvalResult<Option<Handle<SourceTextModule>>> {
    let realm = embed::current_realm(bs);
    let text = format!("module '{specifier}' is not loaded (JS::LoadRequestedModules must load the graph before JS::ModuleLink)");
    let e = api::new_error(bs, realm, &text)?;
    Err(embed::throw_value(e.as_value()))
}

/// `Host::populate_import_meta` (from `cx::ServoHost`): Servo's `ModuleMetadataHook`.
pub fn populate_import_meta(bs: Bs, module: Handle<SourceTextModule>, meta: Handle<api::ObjectValue>) -> EvalResult<()> {
    let cx = current_raw();
    let Some(hook) = unsafe { cx_of(cx) }.hooks.module_metadata else {
        let realm = embed::current_realm(bs);
        let e = api::new_error(bs, realm, "Module metadata hook not set")?;
        return Err(embed::throw_value(e.as_value()));
    };
    let obj = object_of_source(module);
    if obj.is_null() {
        return Ok(());
    }
    let r = unsafe {
        with_slots(2, |s| {
            *s = obj as u64;
            *s.add(1) = (*meta).as_ptr() as u64;
            hook(cx, handle(s), handle(s.add(1)))
        })
    };
    match r {
        Some(true) => Ok(()),
        Some(false) => Err(take_pending_error(bs, cx)),
        None => Err(too_much_recursion(bs)),
    }
}

// ---- Compiling ----

unsafe fn utf8_text<'a>(src: *mut JS::SourceText<mozilla::Utf8Unit>) -> ::std::borrow::Cow<'a, str> {
    let len = unsafe { (*src).length_ } as usize;
    if len == 0 {
        return ::std::borrow::Cow::Borrowed("");
    }
    String::from_utf8_lossy(unsafe { ::std::slice::from_raw_parts((*src).units_.cast::<u8>(), len) })
}

unsafe fn utf16_text(src: *mut JS::SourceText<u16>) -> String {
    let len = unsafe { (*src).length_ } as usize;
    if len == 0 {
        return String::new();
    }
    String::from_utf16_lossy(unsafe { ::std::slice::from_raw_parts((*src).units_, len) })
}

/// Runs `f` in a handle scope and turns its error into the pending exception and null.
unsafe fn object_or_null(cx: *mut JSContext, f: impl FnOnce(Bs) -> EvalResult<*mut JSObject>) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let out = match f(bs) {
        Ok(p) => p,
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    out
}

unsafe fn compile_module(cx: *mut JSContext, options: *const JS::ReadOnlyCompileOptions, text: &str) -> *mut JSObject {
    let at = unsafe { super::dump_failed::at(options) };
    unsafe {
        object_or_null(cx, |bs| {
            let realm = embed::current_realm(bs);
            let m = match embed::compile_module(bs, realm, text, &at.file, at.line) {
                Ok(m) => m,
                Err(e) => {
                    super::dump_failed::report(bs, text, &at, super::dump_failed::Phase::Compile, e);
                    return Err(e);
                },
            };
            new_module_object(bs, realm, Record::Source(m))
        })
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CompileModule1(
    cx: *mut JSContext,
    options: *const JS::ReadOnlyCompileOptions,
    srcBuf: *mut JS::SourceText<mozilla::Utf8Unit>,
) -> *mut JSObject {
    let text = unsafe { utf8_text(srcBuf) };
    unsafe { compile_module(cx, options, &text) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CompileModule(
    cx: *mut JSContext,
    options: *const JS::ReadOnlyCompileOptions,
    srcBuf: *mut JS::SourceText<u16>,
) -> *mut JSObject {
    let text = unsafe { utf16_text(srcBuf) };
    unsafe { compile_module(cx, options, &text) }
}

/// A JSON module: the parsed value as the default export of a synthetic module. A syntax error is JSON.parse's SyntaxError.
unsafe fn compile_json_module(cx: *mut JSContext, text: &str) -> *mut JSObject {
    unsafe {
        object_or_null(cx, |bs| {
            let realm = embed::current_realm(bs);
            let value = api::json_parse(bs, realm, text)?;
            let m = SyntheticModule::new_default_export(bs, realm, value)?;
            new_module_object(bs, realm, Record::Synthetic(m))
        })
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CompileJsonModule1(
    cx: *mut JSContext,
    _options: *const JS::ReadOnlyCompileOptions,
    srcBuf: *mut JS::SourceText<mozilla::Utf8Unit>,
) -> *mut JSObject {
    let text = unsafe { utf8_text(srcBuf) };
    unsafe { compile_json_module(cx, &text) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CompileJsonModule(
    cx: *mut JSContext,
    _options: *const JS::ReadOnlyCompileOptions,
    srcBuf: *mut JS::SourceText<u16>,
) -> *mut JSObject {
    let text = unsafe { utf16_text(srcBuf) };
    unsafe { compile_json_module(cx, &text) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CreateDefaultExportSyntheticModule(
    cx: *mut JSContext,
    defaultExport: JS::Handle<JS::Value>,
) -> *mut JSObject {
    let bits = non_zero(unsafe { (*defaultExport.ptr).asBits_ });
    unsafe {
        object_or_null(cx, |bs| {
            let realm = embed::current_realm(bs);
            let m = SyntheticModule::new_default_export(bs, realm, BsValue::from_raw_bits(bits).to_handle(bs))?;
            new_module_object(bs, realm, Record::Synthetic(m))
        })
    }
}

// ---- Link and evaluate ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ModuleLink(cx: *mut JSContext, moduleRecord: JS::Handle<*mut JSObject>) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let ok = match unsafe { record_of(*moduleRecord.ptr) } {
        Record::Source(m) => match embed::module_link(bs, m) {
            Ok(()) => true,
            Err(e) => unsafe { fail(cx, e) },
        },
        // Not a cyclic module: nothing to link.
        Record::Synthetic(_) => true,
    };
    scope.exit();
    ok
}

fn status_name(s: ModuleState) -> &'static str {
    match s {
        ModuleState::New => "New",
        ModuleState::Unlinked => "Unlinked",
        ModuleState::Linking => "Linking",
        ModuleState::Linked => "Linked",
        ModuleState::Evaluating => "Evaluating",
        ModuleState::EvaluatingAsync => "EvaluatingAsync",
        ModuleState::Evaluated => "Evaluated",
    }
}

/// `JS::ModuleEvaluate`: `rval` is the evaluation promise; evaluation errors reject it (including after top-level await).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ModuleEvaluate(
    cx: *mut JSContext,
    moduleRecord: JS::Handle<*mut JSObject>,
    rval: JS::MutableHandleValue,
) -> bool {
    let module = unsafe { *moduleRecord.ptr };
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let r: EvalResult<Handle<api::PromiseObject>> = match unsafe { record_of(module) } {
        Record::Source(m) => {
            let state = embed::module_state(m);
            if matches!(state, ModuleState::Linked | ModuleState::EvaluatingAsync | ModuleState::Evaluated) {
                embed::module_evaluate(bs, m)
            } else {
                // JSMSG_BAD_MODULE_STATUS, an InternalError in SpiderMonkey.
                let realm = embed::current_realm(bs);
                let text = format!("module record has unexpected status: {}", status_name(state));
                api::new_error(bs, realm, &text).map_err(EvalError::from).and_then(|e| Err(embed::throw_value(e.as_value())))
            }
        },
        Record::Synthetic(m) => {
            let realm = realm_of(bs, module);
            embed::with_realm(bs, realm, |cx| m.evaluate(cx).map_err(EvalError::from))
        },
    };
    let ok = match r {
        Ok(p) => {
            unsafe { (*rval.ptr).asBits_ = (*p.as_object()).as_ptr() as u64 };
            true
        },
        Err(e) => unsafe { fail(cx, e) },
    };
    scope.exit();
    ok
}

/// `JS::ThrowOnModuleEvaluationFailure`. Synchronous errors (what Servo asks for) become the pending exception. For
/// `ReportModuleErrorsAsync` the promise is left without a handler, so a later rejection reaches the host's rejection
/// tracker (SpiderMonkey reports it through the script environment preparer instead).
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ThrowOnModuleEvaluationFailure(
    cx: *mut JSContext,
    evaluationPromise: JS::Handle<*mut JSObject>,
    errorBehaviour: JS::ModuleErrorBehaviour,
) -> bool {
    if unsafe { (*evaluationPromise.ptr).is_null() } {
        return false;
    }
    if errorBehaviour != JS::ModuleErrorBehaviour::ThrowModuleErrorsSync {
        return true;
    }
    let state = unsafe { super::promise::JS__GetPromiseState(evaluationPromise) };
    unsafe { super::promise::JS__SetSettledPromiseIsHandled(cx, evaluationPromise) };
    if state != JS::PromiseState::Rejected {
        return true;
    }
    let mut error = JS::Value { asBits_: value_bits::undefined() };
    unsafe { super::promise::glue__JS_GetPromiseResult(evaluationPromise, JS::MutableHandle { _phantom_0: Default::default(), ptr: &mut error }) };
    unsafe { cx_of(cx) }.set_pending(error.asBits_);
    false
}

// ---- Records, requests and namespaces ----

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetModulePrivate(module: *mut JSObject, value: *const JS::Value) {
    let o = object_ptr(module);
    let old = embed::reserved_slot(o, PRIVATE_SLOT).as_raw_bits();
    let new = non_zero(unsafe { (*value).asBits_ });
    // As ScriptSourceObject::setPrivate: release the old value, store, add a reference to the new one.
    unsafe { release_private(old) };
    embed::set_reserved_slot(o, PRIVATE_SLOT, BsValue::from_raw_bits(new));
    let cx = current_raw();
    if new != value_bits::undefined() && !cx.is_null() {
        if let Some(add) = unsafe { cx_of(cx) }.hooks.script_private_add {
            let v = JS::Value { asBits_: new };
            unsafe { add(&v) };
        }
    }
}

/// As ScriptSourceObject::clearPrivate: the release hook runs for the old value.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ClearModulePrivate(module: *mut JSObject) {
    let o = object_ptr(module);
    let old = embed::reserved_slot(o, PRIVATE_SLOT).as_raw_bits();
    embed::set_reserved_slot(o, PRIVATE_SLOT, BsValue::undefined());
    unsafe { release_private(old) };
}

/// Source text modules are cyclic module records, synthetic ones are not.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__IsCyclicModule(module: *mut JSObject) -> bool {
    embed::reserved_slot(object_ptr(module), RECORD_SLOT).is::<SourceTextModule>()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ModuleIsLinked(moduleObj: *mut JSObject) -> bool {
    let bs = embed::context_of(moduleObj.cast());
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    match embed::reserved_slot(object_ptr(moduleObj), RECORD_SLOT).as_opt::<SourceTextModule>() {
        Some(m) => !matches!(embed::module_state(m.to_handle()), ModuleState::New | ModuleState::Unlinked),
        // SpiderMonkey gives synthetic modules the evaluated status when they are created.
        None => true,
    }
}

#[super::poison::engine_shim(failure = unsafe { (*dest.ptr).asBits_ = value_bits::undefined() })]
pub unsafe extern "C" fn glue__JS_GetModulePrivate(module: *mut JSObject, dest: JS::MutableHandleValue) {
    let bits = if is_module_object(module) {
        embed::reserved_slot(object_ptr(module), PRIVATE_SLOT).as_raw_bits()
    } else {
        value_bits::undefined()
    };
    unsafe { (*dest.ptr).asBits_ = bits };
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetModuleMetadataHook(rt: *mut JSRuntime) -> JS::ModuleMetadataHook {
    unsafe { cx_of(rt.cast()) }.hooks.module_metadata
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetModuleRequestSpecifier(
    _cx: *mut JSContext,
    moduleRequestArg: JS::Handle<*mut JSObject>,
) -> *mut JSString {
    embed::reserved_slot(object_ptr(unsafe { *moduleRequestArg.ptr }), SPECIFIER_SLOT).as_raw_bits() as usize as *mut JSString
}

fn module_type_from_int(i: i32) -> JS::ModuleType {
    use JS::ModuleType as T;
    [T::Unknown, T::JavaScript, T::JSON, T::CSS, T::Bytes, T::Text].into_iter().find(|t| *t as i32 == i).unwrap_or(T::Unknown)
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetModuleRequestType(_cx: *mut JSContext, moduleRequestArg: JS::Handle<*mut JSObject>) -> JS::ModuleType {
    module_type_from_int(embed::reserved_slot(object_ptr(unsafe { *moduleRequestArg.ptr }), TYPE_SLOT).as_smi())
}

/// Brimstone parses no source phase imports (`import source x from ...`), so no request is one.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__ModuleRequestIsSourcePhase(_cx: *mut JSContext, _moduleRequestArg: JS::Handle<*mut JSObject>) -> bool {
    false
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetRequestedModuleType(
    cx: *mut JSContext,
    moduleRecord: JS::Handle<*mut JSObject>,
    index: u32,
) -> JS::ModuleType {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let ty = match unsafe { record_of(*moduleRecord.ptr) } {
        Record::Source(m) => {
            let requests = embed::module_requests(m);
            match requests.get(index as usize) {
                Some(r) => module_type(&r.attributes),
                None => panic!("JS::GetRequestedModuleType: request {index} out of range"),
            }
        },
        Record::Synthetic(_) => panic!("JS::GetRequestedModuleType: a synthetic module has no requests"),
    };
    scope.exit();
    ty
}

/// A source text module's script is its record; synthetic modules have none.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetModuleScript(moduleRecord: JS::Handle<*mut JSObject>) -> *mut JSScript {
    let obj = unsafe { *moduleRecord.ptr };
    let v = embed::reserved_slot(object_ptr(obj), RECORD_SLOT);
    if v.is::<SourceTextModule>() { v.as_raw_bits() as usize as *mut JSScript } else { ::std::ptr::null_mut() }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetModuleObject(moduleScript: JS::Handle<*mut JSScript>) -> *mut JSObject {
    let s = unsafe { *moduleScript.ptr };
    let bs = embed::context_of((s as *const u8).cast());
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    match BsValue::from_raw_bits(s as u64).as_opt::<SourceTextModule>() {
        Some(m) => object_of_source(m.to_handle()),
        None => panic!("JS::GetModuleObject: not a module script"),
    }
}

/// The global of a script: a module script's realm or a compiled script's.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_GetGlobalFromScript(script: *mut JSScript) -> *mut JSObject {
    let bs = embed::context_of(script.cast());
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    let v = BsValue::from_raw_bits(script as u64);
    let realm = if let Some(m) = v.as_opt::<SourceTextModule>() {
        embed::module_realm(m.to_handle())
    } else {
        HeapPtr::<embed::CompiledScript>::from_ptr(script.cast()).realm()
    };
    embed::global_of_realm(*realm).as_ptr().cast()
}

unsafe fn namespace_of(bs: Bs, module: *mut JSObject, record: Record) -> EvalResult<*mut JSObject> {
    let ns = match record {
        Record::Source(m) => embed::module_namespace(bs, m)?,
        Record::Synthetic(mut m) => {
            let realm = realm_of(bs, module);
            embed::with_realm(bs, realm, |cx| m.get_namespace_object(cx))?.as_object().to_handle()
        },
    };
    Ok((*ns).as_ptr().cast())
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetModuleNamespace(cx: *mut JSContext, moduleRecord: JS::Handle<*mut JSObject>) -> *mut JSObject {
    let module = unsafe { *moduleRecord.ptr };
    unsafe {
        object_or_null(cx, |bs| {
            let record = record_of(module);
            if let Record::Source(m) = record {
                if matches!(embed::module_state(m), ModuleState::New | ModuleState::Unlinked | ModuleState::Linking) {
                    let realm = embed::current_realm(bs);
                    let e = api::new_error(bs, realm, "module namespace requested before the module was linked")?;
                    return Err(embed::throw_value(e.as_value()));
                }
            }
            namespace_of(bs, module, record)
        })
    }
}

/// The module object whose namespace this is. Namespaces exist only for linked records, so only those are asked.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetModuleForNamespace(cx: *mut JSContext, moduleNamespace: JS::Handle<*mut JSObject>) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    let scope = HandleScope::enter(bs);
    let ns = BsValue::object(object_ptr(unsafe { *moduleNamespace.ptr }));
    if !ns.is::<ModuleNamespaceObject>() {
        scope.exit();
        panic!("JS::GetModuleForNamespace: not a module namespace");
    }
    let candidates: Vec<usize> = MODULES.with(|m| m.borrow().iter().copied().collect());
    let mut found = ::std::ptr::null_mut();
    for addr in candidates {
        let module = addr as *mut JSObject;
        let record = unsafe { record_of(module) };
        if let Record::Source(m) = record {
            if matches!(embed::module_state(m), ModuleState::New | ModuleState::Unlinked | ModuleState::Linking) {
                continue;
            }
        }
        if let Ok(p) = unsafe { namespace_of(bs, module, record) } {
            if p == unsafe { *moduleNamespace.ptr } {
                found = module;
                break;
            }
        }
    }
    scope.exit();
    found
}
