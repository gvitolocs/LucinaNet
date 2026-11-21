/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// A global class's resolve hook defines properties lazily on an own-property miss, as Servo's Window does for its DOM
// interface objects (`resolve_global`). SpiderMonkey calls the hook from every lookup path: bare identifiers, `typeof`,
// member access, `in`, strict code.

use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use mozjs::jsapi::{
    JSClass, JSClassOps, JSContext, JSObject, JS_DefineProperty, JS_GlobalObjectTraceHook, JS_StringEqualsAscii,
    OnNewGlobalHookOption, JSCLASS_GLOBAL_SLOT_COUNT, JSCLASS_IS_GLOBAL, JSCLASS_RESERVED_SLOTS_MASK,
    JSCLASS_RESERVED_SLOTS_SHIFT, JSPROP_ENUMERATE, JS,
};
use mozjs::jsval::{Int32Value, UndefinedValue};
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{evaluate_script, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime};

static CALLS: AtomicUsize = AtomicUsize::new(0);
/// Set while JS_NewGlobalObject runs: SpiderMonkey calls no resolve hook then (Servo's reads slots filled in afterwards).
static IN_NEW_GLOBAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Defines `Lazy = 7` the first time it is looked up; anything else stays unresolved.
unsafe extern "C" fn resolve(cx: *mut JSContext, obj: JS::HandleObject, id: JS::HandleId, resolved: *mut bool) -> bool {
    assert!(!IN_NEW_GLOBAL.load(Ordering::SeqCst), "resolve hook ran inside JS_NewGlobalObject");
    unsafe { *resolved = false };
    let id = unsafe { *id.ptr };
    if !id.is_string() {
        return true;
    }
    let mut eq = false;
    if !unsafe { JS_StringEqualsAscii(cx, id.to_string(), c"Lazy".as_ptr(), &mut eq) } {
        return false;
    }
    if !eq {
        return true;
    }
    CALLS.fetch_add(1, Ordering::SeqCst);
    let v = Int32Value(7);
    let ok = unsafe {
        JS_DefineProperty(
            cx,
            obj,
            c"Lazy".as_ptr(),
            JS::Handle { _phantom_0: Default::default(), ptr: &v },
            JSPROP_ENUMERATE as u32,
        )
    };
    unsafe { *resolved = ok };
    ok
}

static OPS: JSClassOps = JSClassOps {
    addProperty: None,
    delProperty: None,
    enumerate: None,
    newEnumerate: None,
    resolve: Some(resolve),
    mayResolve: None,
    finalize: None,
    call: None,
    construct: None,
    trace: Some(JS_GlobalObjectTraceHook),
};

static LAZY_GLOBAL: JSClass = JSClass {
    name: c"LazyGlobal".as_ptr(),
    flags: JSCLASS_IS_GLOBAL
        | ((JSCLASS_GLOBAL_SLOT_COUNT & JSCLASS_RESERVED_SLOTS_MASK) << JSCLASS_RESERVED_SLOTS_SHIFT),
    cOps: &OPS,
    spec: ptr::null(),
    ext: ptr::null(),
    oOps: ptr::null(),
};

/// Runs `src` in a fresh LazyGlobal and returns its int32 result.
fn run(runtime: &mut Runtime, src: &str) -> i32 {
    let context = runtime.cx();
    unsafe {
IN_NEW_GLOBAL.store(true, Ordering::SeqCst);
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &LAZY_GLOBAL,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        IN_NEW_GLOBAL.store(false, Ordering::SeqCst);
        rooted!(&in(context) let mut rval = UndefinedValue());
        let options = CompileOptionsWrapper::new(&context, c"resolve".to_owned(), 1);
        assert!(evaluate_script(context, global.handle(), src, rval.handle_mut(), options).is_ok(), "{src} threw");
        let _: *mut JSObject = global.get();
        rval.get().to_int32()
    }
}

// One test: JSEngine::init works once per process.
#[test]
fn resolve_hook_runs_on_every_lookup_path() {
    let engine = JSEngine::init().unwrap();
    let mut rt = Runtime::new(engine.handle());
    let rt = &mut rt;
    assert_eq!(run(rt, "Lazy"), 7);
    assert_eq!(run(rt, "typeof Lazy === 'number' ? 1 : 0"), 1);
    assert_eq!(run(rt, "this.Lazy"), 7);
    assert_eq!(run(rt, "'Lazy' in this ? 1 : 0"), 1);
    assert_eq!(run(rt, "(function(){ 'use strict'; return Lazy; })()"), 7);
    assert_eq!(run(rt, "(() => globalThis['Lazy'])()"), 7);
    // Each fresh global resolves once; the defined property answers every later lookup.
    assert_eq!(run(rt, "Lazy + Lazy + Lazy"), 21);
    assert_eq!(CALLS.load(Ordering::SeqCst), 7);
    // Names the hook does not know stay missing.
    assert_eq!(run(rt, "typeof NotThere === 'undefined' ? 1 : 0"), 1);
    assert_eq!(run(rt, "try { NotThere; 0 } catch (e) { e instanceof ReferenceError ? 1 : 2 }"), 1);
}
