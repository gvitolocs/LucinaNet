/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Realms and globals. The original is `js/src/vm/Realm.cpp` and `JS_NewGlobalObject`/`JS::EnterRealm` in `jsapi.cpp`.
//!
//! `JS::Realm*` (and `JS::Compartment*`) is the realm's **global object**: SpiderMonkey's realms never move and Servo keeps
//! realm pointers across GCs (`JSAutoRealm` holds the old one), but Brimstone's `Realm` is an ordinary heap item that the
//! collector copies. Globals are host objects, which never move, and a realm has exactly one, so the global is a stable
//! name for its realm. `JS::Zone*` is the context: one heap per context here.

use ::std::ffi::c_void;

use brimstone_core::runtime::embed::{self, HostFlags};
use brimstone_core::runtime::gc::{HandleScope, HeapPtr};
use brimstone_core::runtime::object_value::ObjectValue;

use super::cx::cx_of;
use crate::boundary::bounds;
use crate::jsapi::*;

/// The `JS::Realm*` of a global.
#[inline]
pub fn realm_name(global: HeapPtr<ObjectValue>) -> *mut JS::Realm {
    global.as_ptr().cast()
}

/// The global a `JS::Realm*` names.
#[inline]
pub fn global_of(realm: *mut JS::Realm) -> HeapPtr<ObjectValue> {
    HeapPtr::from_ptr(realm.cast())
}

#[inline]
pub fn object_ptr(obj: *mut JSObject) -> HeapPtr<ObjectValue> {
    HeapPtr::from_ptr(obj.cast())
}

/// The host flags for objects of a SpiderMonkey class.
pub unsafe fn host_flags_of(class: *const JSClass) -> HostFlags {
    let mut flags = HostFlags::empty();
    let ops = unsafe { (*class).cOps };
    if !ops.is_null() {
        let ops = unsafe { &*ops };
        if ops.trace.is_some() {
            flags |= HostFlags::HAS_TRACE;
        }
        if ops.finalize.is_some() {
            flags |= HostFlags::HAS_FINALIZE;
        }
        // Brimstone defines the standard classes eagerly, so SpiderMonkey's lazy resolver has nothing to do here.
        let standard = JS_ResolveStandardClass as usize;
        if ops.resolve.is_some_and(|r| r as usize != standard) {
            flags |= HostFlags::HAS_RESOLVE;
        }
        if ops.call.is_some() {
            flags |= HostFlags::CALLABLE;
        }
        if ops.construct.is_some() {
            flags |= HostFlags::CONSTRUCTOR;
        }
    }
    // SpiderMonkey spells ECMA-262 B.3.6's [[IsHTMLDDA]] slot as a class flag (`JSCLASS_EMULATES_UNDEFINED`, what
    // Servo sets on HTMLAllCollection for `document.all`); Brimstone keeps it on the object's shape.
    if unsafe { (*class).flags } & JSCLASS_EMULATES_UNDEFINED != 0 {
        flags |= HostFlags::IS_HTMLDDA;
    }
    flags
}

thread_local! {
    /// Set while `JS_NewGlobalObject` builds a global (checked by `object::call_class_resolve`).
    pub static CREATING_GLOBAL: ::std::cell::Cell<bool> = const { ::std::cell::Cell::new(false) };
    /// `RealmCreationOptions::traceGlobal_` of each global, by address (globals never move): what `JS_GlobalObjectTraceHook`
    /// calls. Servo's Window class has `JS_GlobalObjectTraceHook` as its trace hook and its own trace here
    /// (`script_bindings::interface::create_global_object`). A global made later at a freed global's address replaces it.
    static GLOBAL_TRACE: ::std::cell::RefCell<::std::collections::HashMap<usize, JSTraceOp>> =
        ::std::cell::RefCell::new(::std::collections::HashMap::new());
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_NewGlobalObject(
    cx: *mut JSContext,
    clasp: *const JSClass,
    principals: *mut JSPrincipals,
    _hook: JS::OnNewGlobalHookOption,
    options: *const JS::RealmOptions,
) -> *mut JSObject {
    let bs = unsafe { cx_of(cx) }.bs;
    // The realm the engine would install WebAssembly in is about to exist: take the option's value as of now.
    unsafe { super::cx::sync_wasm_enabled(cx_of(cx)) };
    let trace = if options.is_null() { None } else { unsafe { (*options).creationOptions_.traceGlobal_ } };
    let flags = unsafe { host_flags_of(clasp) };
    let slots = bounds::reserved_slot_count(unsafe { (*clasp).flags });
    let scope = HandleScope::enter(bs);
    // SpiderMonkey runs no resolve hook while it builds a global: Servo's `resolve_window` reads the reserved slots it fills in
    // only after JS_NewGlobalObject returns.
    let was = CREATING_GLOBAL.replace(true);
    let global: *mut JSObject = match embed::new_global(bs, clasp.cast(), flags, slots) {
        Ok((_realm, global)) => (*global).as_ptr().cast(),
        Err(_) => ::std::ptr::null_mut(),
    };
    CREATING_GLOBAL.set(was);
    scope.exit();
    // The realm takes the principals it is created with (SpiderMonkey's Realm constructor holds them): Servo passes the origin's
    // principals here and calls SetRealmPrincipals only once the window has its document, so a cross-realm check in between
    // (an iframe's initial about:blank global, BBC and other pages with ad frames) read no principals and dereferenced null in
    // is_platform_object_same_origin (SIGSEGV on every run of `--render https://www.bbc.com/news`).
    if !global.is_null() && !principals.is_null() {
        unsafe { super::compartment::JS__SetRealmPrincipals(global.cast(), principals) };
    }
    if !global.is_null() {
        GLOBAL_TRACE.with(|t| {
            let mut t = t.borrow_mut();
            match trace {
                Some(_) => t.insert(global as usize, trace),
                None => t.remove(&(global as usize)),
            }
        });
    }
    global
}

/// Enters the realm of `target`'s global; returns the realm that was current (null if none), for `LeaveRealm`.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__EnterRealm(cx: *mut JSContext, target: *mut JSObject) -> *mut JS::Realm {
    let bs = unsafe { cx_of(cx) }.bs;
    let old = unsafe { cx_of(cx) }.rooting.realm_;
    let realm = embed::realm_of_object(bs, object_ptr(target));
    let global = embed::global_of_realm(realm);
    embed::enter_realm(bs, realm);
    unsafe { cx_of(cx) }.realm_stack.push(global.as_ptr().cast());
    unsafe { cx_of(cx) }.rooting.realm_ = realm_name(global);
    old
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__LeaveRealm(cx: *mut JSContext, old: *mut JS::Realm) {
    let bs = unsafe { cx_of(cx) }.bs;
    let current = unsafe { cx_of(cx) }.realm_stack.pop().expect("LeaveRealm without EnterRealm");
    let realm = embed::realm_of_object(bs, HeapPtr::from_ptr(current.cast()));
    embed::leave_realm(bs, realm);
    unsafe { cx_of(cx) }.rooting.realm_ = old;
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetObjectRealmOrNull(obj: *mut JSObject) -> *mut JS::Realm {
    if obj.is_null() {
        return ::std::ptr::null_mut();
    }
    let bs = embed::context_of(obj.cast());
    realm_name(embed::global_of_realm(embed::realm_of_object(bs, object_ptr(obj))))
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetRealmGlobalOrNull(realm: *mut JS::Realm) -> *mut JSObject {
    realm.cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CurrentGlobalOrNull(cx: *mut JSContext) -> *mut JSObject {
    unsafe { cx_of(cx) }.rooting.realm_.cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetNonCCWObjectGlobal(obj: *mut JSObject) -> *mut JSObject {
    unsafe { JS__GetObjectRealmOrNull(obj) }.cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__InitRealmStandardClasses(_cx: *mut JSContext) -> bool {
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_ResolveStandardClass(
    _cx: *mut JSContext,
    _obj: JS::HandleObject,
    _id: JS::HandleId,
    resolved: *mut bool,
) -> bool {
    unsafe { *resolved = false };
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_MayResolveStandardClass(_names: *const JSAtomState, _id: jsid, _maybe: *mut JSObject) -> bool {
    false
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_EnumerateStandardClasses(_cx: *mut JSContext, _obj: JS::HandleObject) -> bool {
    true
}

/// SpiderMonkey traces the global's own slots and realm data here (Brimstone traces a host object's slots and its realm
/// itself), then calls the realm's `RealmCreationOptions::traceGlobal_`, which is how Servo's Window traces its fields.
#[super::poison::engine_shim(gc)]
pub unsafe extern "C" fn JS_GlobalObjectTraceHook(trc: *mut JSTracer, global: *mut JSObject) {
    let trace = GLOBAL_TRACE.with(|t| t.borrow().get(&(global as usize)).copied().flatten());
    if let Some(trace) = trace {
        unsafe { trace(trc, global) };
    }
}

/// Debugger hook; there is no debugger.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS_FireOnNewGlobalObject(_cx: *mut JSContext, _global: JS::HandleObject) {}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__JS_NewRealmOptions() -> *mut JS::RealmOptions {
    Box::into_raw(Box::new(unsafe { ::std::mem::zeroed::<JS::RealmOptions>() }))
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__DeleteRealmOptions(options: *mut JS::RealmOptions) {
    if !options.is_null() {
        unsafe { drop(Box::from_raw(options)) };
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__SetRealmPrivate(realm: *mut JS::Realm, data: *mut c_void) {
    let bs = embed::context_of(realm.cast());
    let _ = bs;
    unsafe { super::cx::current() }.realm_private.insert(realm as usize, data);
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__GetRealmPrivate(realm: *mut JS::Realm) -> *mut c_void {
    unsafe { super::cx::current() }.realm_private.get(&(realm as usize)).copied().unwrap_or(::std::ptr::null_mut())
}

/// A handle to the current global: `RootingContext::realm_` holds it, since our `JS::Realm*` is the global's address.
#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CurrentGlobal(cx: *mut JSContext) -> *const *mut JSObject {
    (&raw const unsafe { cx_of(cx) }.rooting.realm_).cast()
}
