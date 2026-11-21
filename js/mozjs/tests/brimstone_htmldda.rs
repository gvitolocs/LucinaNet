/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The two glue halves of `document.all`. A `JSClass` flagged `JSCLASS_EMULATES_UNDEFINED` reaches Brimstone's
//! `HostFlags::IS_HTMLDDA` (ECMA-262 B.3.6's [[IsHTMLDDA]] slot), and a proxy whose handler provides the `call` trap is
//! callable over its own private rather than only over a callable private (a DOM `legacycaller`). The last test puts
//! both on one object, which is what HTMLAllCollection is.
//!
//! The set the annex changes is narrow, so every test reads both directions: `typeof` says "undefined", `ToBoolean`
//! answers false and `==` answers true against null and undefined, while `===`, `Object.is`, `??` and the missing-value
//! test a binding makes still see an object.
//!
//! The last two tests walk the same object through collections. Brimstone keeps the slot on the object's *private* shape,
//! which is detached from the transition tree, and a detached shape caches its next shapes in a weak edge that no tree
//! walk reaches; the collector has to repair that edge when it moves the shape, and the same once the cache has become
//! a vec and that vec is replaced by a larger copy. The heap verifier is what sees the stale address, so both tests
//! print the `LUCINANET_JS_GC_VERIFY` / `LUCINANET_JS_GC_STRESS_KB` they were run under.

use std::ffi::CStr;
use std::ptr;
use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};

use mozjs::context::JSContext;
use mozjs::gc::HandleObject;
use mozjs::glue::{CreateProxyHandler, ProxyTraps, RUST_JSID_IS_INT, RUST_JSID_TO_INT};
use mozjs::jsapi::{
    js, JS, CallArgs, JSClass, JSClassOps, JSContext as RawJSContext, JS_NewObject, JS_NewPlainObject, JS_TypeOfValue,
    JSObject, JSCLASS_EMULATES_UNDEFINED, JSCLASS_IS_PROXY, JSCLASS_RESERVED_SLOTS_SHIFT, JSPROP_ENUMERATE, JSType,
    OnNewGlobalHookOption,
};
use mozjs::jsval::{Int32Value, NullValue, ObjectValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{
    JS_ClearPendingException, JS_DefineProperty, JS_NewGlobalObject, JS_GC, NewProxyObject,
};
use mozjs::rust::{CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS, evaluate_script};
use mozjs_sys::engine::values::emulates_undefined;

// ___________________________________________________________________________
// classes

/// A class call hook answering `10 * argc + the first argument`, to tell the call sites apart.
unsafe extern "C" fn class_call_hook(cx: *mut RawJSContext, argc: u32, vp: *mut JS::Value) -> bool {
    let _ = cx;
    let args = unsafe { CallArgs::from_vp(vp, argc) };
    assert!(!args.is_constructing());
    let first = if argc > 0 { args.get(0).to_int32() } else { 0 };
    args.rval().set(Int32Value(10 * argc as i32 + first));
    true
}

static OPS_NONE: JSClassOps = JSClassOps {
    addProperty: None,
    delProperty: None,
    enumerate: None,
    newEnumerate: None,
    resolve: None,
    mayResolve: None,
    finalize: None,
    call: None,
    construct: None,
    trace: None,
};

static OPS_CALL: JSClassOps = JSClassOps { call: Some(class_call_hook), ..OPS_NONE };

/// What a binding makes for `document.all`: the emulated-undefined flag and nothing else.
static HTMLDDA: JSClass = JSClass {
    name: c"HTMLDDA".as_ptr(),
    flags: JSCLASS_EMULATES_UNDEFINED,
    cOps: &OPS_NONE,
    spec: ptr::null(),
    ext: ptr::null(),
    oOps: ptr::null(),
};

/// The same class without the flag: the control.
static PLAIN_DOM: JSClass = JSClass {
    name: c"PlainDom".as_ptr(),
    flags: 0,
    cOps: &OPS_NONE,
    spec: ptr::null(),
    ext: ptr::null(),
    oOps: ptr::null(),
};

/// Callable, unflagged, and a host object so it never moves: used as a proxy private.
static CALLABLE_DOM: JSClass = JSClass {
    name: c"CallableDom".as_ptr(),
    flags: 0,
    cOps: &OPS_CALL,
    spec: ptr::null(),
    ext: ptr::null(),
    oOps: ptr::null(),
};

/// Flagged and callable at once: the slot wins for `typeof`, the hook stays reachable.
static HTMLDDA_CALLABLE: JSClass = JSClass {
    name: c"HTMLDDACallable".as_ptr(),
    flags: JSCLASS_EMULATES_UNDEFINED,
    cOps: &OPS_CALL,
    spec: ptr::null(),
    ext: ptr::null(),
    oOps: ptr::null(),
};

/// A proxy class carrying the flag, the way `HTMLAllCollection` is bound.
static HTMLDDA_PROXY: JSClass = JSClass {
    name: c"HTMLAllCollection".as_ptr(),
    flags: JSCLASS_IS_PROXY | JSCLASS_EMULATES_UNDEFINED | (2 << JSCLASS_RESERVED_SLOTS_SHIFT),
    cOps: ptr::null(),
    spec: ptr::null(),
    ext: ptr::null(),
    oOps: ptr::null(),
};

// ___________________________________________________________________________
// traps, and what they recorded

static CALLS: AtomicI32 = AtomicI32::new(0);
static CONSTRUCTS: AtomicI32 = AtomicI32::new(0);
static LAST_ARGC: AtomicI32 = AtomicI32::new(-1);
static LAST_FIRST: AtomicI32 = AtomicI32::new(-1);
static LAST_CONSTRUCTING: AtomicI32 = AtomicI32::new(-1);
static LAST_PROXY_MATCHED: AtomicI32 = AtomicI32::new(-1);
static PROXY: AtomicI64 = AtomicI64::new(0);

fn reset() {
    CALLS.store(0, Ordering::SeqCst);
    CONSTRUCTS.store(0, Ordering::SeqCst);
    LAST_ARGC.store(-1, Ordering::SeqCst);
    LAST_FIRST.store(-1, Ordering::SeqCst);
    LAST_CONSTRUCTING.store(-1, Ordering::SeqCst);
    LAST_PROXY_MATCHED.store(-1, Ordering::SeqCst);
    PROXY.store(0, Ordering::SeqCst);
}

/// The first argument's kind: 0 int32, 1 string, 2 absent, 3 something else.
fn first_kind(args: &CallArgs) -> i32 {
    let first = args.get(0).get();
    if first.is_int32() {
        0
    } else if first.is_string() {
        1
    } else if first.is_undefined() {
        2
    } else {
        3
    }
}

/// What every trap notes: the argument count, the first argument's kind, whether the frame reads as constructing, and
/// that the handler got the proxy itself. A trap is an `extern "C"` frame, where a panic aborts the process: everything
/// is recorded for the test body to assert on instead.
unsafe fn record(args: &CallArgs, proxy: JS::HandleObject) {
    LAST_ARGC.store(args.argc_ as i32, Ordering::SeqCst);
    LAST_FIRST.store(if args.argc_ == 0 { 2 } else { first_kind(args) }, Ordering::SeqCst);
    LAST_CONSTRUCTING.store(args.is_constructing() as i32, Ordering::SeqCst);
    let given = unsafe { *proxy.ptr } as i64;
    LAST_PROXY_MATCHED.store((given == PROXY.load(Ordering::SeqCst)) as i32, Ordering::SeqCst);
}

/// A `call` trap: `f(i)` is 1000 + i, `f("s")` 2000, `f(o)` 3000.
unsafe extern "C" fn call_trap(cx: *mut RawJSContext, proxy: JS::HandleObject, args: *const CallArgs) -> bool {
    let _ = cx;
    let args = unsafe { &*args };
    CALLS.fetch_add(1, Ordering::SeqCst);
    unsafe { record(args, proxy) };
    let r = match args.argc_ {
        0 => -1,
        _ => match first_kind(args) {
            0 => 1000 + args.get(0).to_int32(),
            1 => 2000,
            _ => 3000,
        },
    };
    args.rval().set(Int32Value(r));
    true
}

/// A `construct` trap: hands back a fresh object.
unsafe extern "C" fn construct_trap(cx: *mut RawJSContext, proxy: JS::HandleObject, args: *const CallArgs) -> bool {
    let args = unsafe { &*args };
    CONSTRUCTS.fetch_add(1, Ordering::SeqCst);
    unsafe { record(args, proxy) };
    let obj = unsafe { JS_NewPlainObject(cx) };
    assert!(!obj.is_null(), "JS_NewPlainObject failed inside the construct trap");
    args.rval().set(ObjectValue(obj));
    true
}

/// A Base handler's `getOwnPropertyDescriptor`: index 0 is 42, index 1 is 43, anything else is absent.
unsafe extern "C" fn indexed_gopd(
    _cx: *mut RawJSContext,
    _proxy: JS::HandleObject,
    id: JS::HandleId,
    desc: JS::MutableHandle<JS::PropertyDescriptor>,
    is_none: *mut bool,
) -> bool {
    let d = unsafe { &mut *desc.ptr };
    if unsafe { RUST_JSID_IS_INT(id) } {
        let val = match unsafe { RUST_JSID_TO_INT(id) } {
            0 => 42,
            1 => 43,
            _ => {
                unsafe { *is_none = true };
                return true;
            },
        };
        d.set_hasValue_(true);
        d.value_ = Int32Value(val).into();
        d.set_hasWritable_(true);
        d.set_writable_(true);
        d.set_hasEnumerable_(true);
        d.set_enumerable_(true);
        d.set_hasConfigurable_(true);
        d.set_configurable_(true);
        unsafe { *is_none = false };
        return true;
    }
    unsafe { *is_none = true };
    true
}

/// The `[[Call]]` of HTMLAllCollection: no argument is null, a number selects the indexed element, a name the named one.
unsafe extern "C" fn all_call(cx: *mut RawJSContext, proxy: JS::HandleObject, args: *const CallArgs) -> bool {
    let _ = cx;
    let args = unsafe { &*args };
    unsafe { record(args, proxy) };
    let r = match args.argc_ {
        0 => NullValue(),
        _ => match args.get(0).get() {
            v if v.is_int32() => Int32Value(42 + v.to_int32()),
            v if v.is_string() => Int32Value(7),
            _ => NullValue(),
        },
    };
    CALLS.fetch_add(1, Ordering::SeqCst);
    args.rval().set(r);
    true
}

// ___________________________________________________________________________
// harness

/// Runs `f` in a fresh engine with a simple global, its realm entered.
fn with_global(f: impl FnOnce(&mut JSContext, HandleObject)) {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, global.handle());
        f(&mut realm, global.handle());
    }
}

/// Evaluates `src` and returns its completion value as an int32; panics if it throws.
fn eval(context: &mut JSContext, global: HandleObject, src: &str) -> i32 {
    rooted!(&in(context) let mut rval = UndefinedValue());
    let options = CompileOptionsWrapper::new(context, c"htmldda.js".to_owned(), 1);
    if evaluate_script(context, global, src, rval.handle_mut(), options).is_err() {
        unsafe { JS_ClearPendingException(context) };
        panic!("script threw: {src}");
    }
    rval.get().to_int32()
}

/// Whether evaluating `src` throws.
fn throws(context: &mut JSContext, global: HandleObject, src: &str) -> bool {
    rooted!(&in(context) let mut rval = UndefinedValue());
    let options = CompileOptionsWrapper::new(context, c"htmldda.js".to_owned(), 1);
    let threw = evaluate_script(context, global, src, rval.handle_mut(), options).is_err();
    if threw {
        unsafe { JS_ClearPendingException(context) };
    }
    threw
}

unsafe fn define(context: &mut JSContext, global: HandleObject, name: &CStr, object: *mut JSObject) {
    assert!(!object.is_null(), "{name:?}: the object was not created");
    rooted!(&in(context) let value = ObjectValue(object));
    assert!(JS_DefineProperty(context, global, name.as_ptr(), value.handle(), JSPROP_ENUMERATE as u32));
}

/// Makes a Base proxy with `traps` (of `class`, if given; private `target`, if one is needed), defines it as `name` and
/// leaves its address for the traps to compare against. The caller keeps `target` alive.
unsafe fn proxy(
    context: &mut JSContext,
    global: HandleObject,
    name: &CStr,
    traps: &ProxyTraps,
    class: *const JSClass,
    target: *mut JSObject,
) -> *mut JSObject {
    let handler = CreateProxyHandler(traps, ptr::null());
    assert!(!handler.is_null());
    rooted!(&in(context) let privv = if target.is_null() { UndefinedValue() } else { ObjectValue(target) });
    let object = NewProxyObject(context, handler, privv.handle(), ptr::null_mut(), class, false);
    assert!(!object.is_null(), "{name:?}: the proxy was not created");
    PROXY.store(object as i64, Ordering::SeqCst);
    define(context, global, name, object);
    object
}

/// `typeof` through the C entry point.
fn type_of(context: &mut JSContext, object: *mut JSObject) -> JSType {
    rooted!(&in(context) let value = ObjectValue(object));
    unsafe { JS_TypeOfValue(context.raw_cx(), value.handle().into()) }
}

// ___________________________________________________________________________
// (b) JSCLASS_EMULATES_UNDEFINED

#[test]
fn a_flagged_class_emulates_undefined() {
    with_global(|context, global| unsafe {
        let x = JS_NewObject(context.raw_cx(), &HTMLDDA);
        define(context, global, c"x", x);
        let y = JS_NewObject(context.raw_cx(), &PLAIN_DOM);
        define(context, global, c"y", y);

        // The three operations ECMA-262 B.3.6 changes.
        assert_eq!(eval(context, global, "typeof x === 'undefined' ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "typeof x == 'undefined' ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "x ? 1 : 0"), 0);
        assert_eq!(eval(context, global, "!x ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "Boolean(x) ? 1 : 0"), 0);
        assert_eq!(eval(context, global, "let n = 0; if (x) n = 1; n"), 0, "the coerced conditional jump");
        assert_eq!(eval(context, global, "x == null ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "x == undefined ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "null == x ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "undefined == x ? 1 : 0"), 1);

        // Everything the annex leaves alone: it stays an object.
        assert_eq!(eval(context, global, "x !== undefined ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "x !== null ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "x === x ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "x != x ? 1 : 0"), 0);
        assert_eq!(eval(context, global, "Object.is(x, undefined) ? 1 : 0"), 0);
        assert_eq!(eval(context, global, "x instanceof Object ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "x.toString === undefined ? 1 : 0"), 0, "still has its prototype");
        assert_eq!(eval(context, global, "(x ?? 'd') === x ? 1 : 0"), 1, "?? does not see the slot");
        assert_eq!(eval(context, global, "x.a = 1; (x?.a) === 1 ? 1 : 0"), 1, "?. reads through it");
        assert_eq!(eval(context, global, "(function (v = 'd') { return v === x ? 1 : 0 })(x)"), 1);
        assert_eq!(eval(context, global, "[x].includes(undefined) ? 1 : 0"), 0);

        // The unflagged class is unaffected.
        assert_eq!(eval(context, global, "typeof y === 'object' ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "y ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "y == null ? 1 : 0"), 0);

        // The C entry points agree with the script.
        assert_eq!(type_of(context, x) as u32, JSType::JSTYPE_UNDEFINED as u32);
        assert_eq!(type_of(context, y) as u32, JSType::JSTYPE_OBJECT as u32);
        rooted!(&in(context) let xv = ObjectValue(x));
        rooted!(&in(context) let yv = ObjectValue(y));
        rooted!(&in(context) let undef = UndefinedValue());
        rooted!(&in(context) let nullv = NullValue());
        assert!(!js::ToBooleanSlow(xv.handle().into()));
        assert!(js::ToBooleanSlow(yv.handle().into()));
        let mut eq = true;
        assert!(JS::StrictlyEqual(context.raw_cx(), xv.handle().into(), undef.handle().into(), &mut eq));
        assert!(!eq, "x === undefined must stay false");
        assert!(JS::StrictlyEqual(context.raw_cx(), xv.handle().into(), xv.handle().into(), &mut eq));
        assert!(eq, "x === x");
        assert!(JS::LooselyEqual(context.raw_cx(), xv.handle().into(), undef.handle().into(), &mut eq));
        assert!(eq, "x == undefined");
        assert!(JS::LooselyEqual(context.raw_cx(), nullv.handle().into(), xv.handle().into(), &mut eq));
        assert!(eq, "null == x");
        eq = true;
        assert!(JS::LooselyEqual(context.raw_cx(), xv.handle().into(), yv.handle().into(), &mut eq));
        assert!(!eq, "and two host objects are still not equal to each other");

        // The predicate a binding asks, and that it is not the question "is this value missing".
        assert!(emulates_undefined(xv.get()));
        assert!(!emulates_undefined(yv.get()));
        assert!(!emulates_undefined(undef.get()), "undefined itself does not emulate undefined");
        assert!(!xv.get().is_undefined(), "the object is not undefined");
        assert!(!xv.get().is_null_or_undefined(), "nor nullish: the bindings' missing-value test still sees it");
    });
}

#[test]
fn the_flag_survives_shape_changes() {
    /// The slot still answers: `typeof`, `!` and `== null` together.
    fn emulates(context: &mut JSContext, global: HandleObject) {
        assert_eq!(eval(context, global, "typeof x === 'undefined' ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "!x ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "x == null ? 1 : 0"), 1);
    }

    with_global(|context, global| unsafe {
        let x = JS_NewObject(context.raw_cx(), &HTMLDDA);
        define(context, global, c"x", x);
        emulates(context, global);

        // Property adds move the shape; the flag rides on the object's own copy of it.
        assert_eq!(eval(context, global, "x.a = 1; x.b = 2; x.a + x.b"), 3);
        emulates(context, global);
        assert_eq!(eval(context, global, "Object.defineProperty(x, 'c', {value: 3}).c"), 3);
        emulates(context, global);
        // Four hundred more properties switch the shape to dictionary mode.
        assert_eq!(eval(context, global, "for (let i = 0; i < 400; i++) x['k' + i] = i; x.k399"), 399);
        emulates(context, global);
        assert_eq!(eval(context, global, "delete x.k0; delete x.a; ('a' in x || 'k0' in x) ? 1 : 0"), 0);
        emulates(context, global);

        // The prototype goes and comes back; the object stops being extensible.
        assert!(!throws(context, global, "Object.setPrototypeOf(x, null)"));
        emulates(context, global);
        assert_eq!(eval(context, global, "x instanceof Object ? 1 : 0"), 0, "with no prototype, and still emulating");
        assert!(!throws(context, global, "Object.setPrototypeOf(x, Array.prototype)"));
        emulates(context, global);
        assert_eq!(eval(context, global, "x instanceof Array ? 1 : 0"), 1);
        assert!(!throws(context, global, "Object.preventExtensions(x)"));
        emulates(context, global);

        // A second object of the same class is its own object, not a shape shared with the first.
        let z = JS_NewObject(context.raw_cx(), &HTMLDDA);
        define(context, global, c"z", z);
        assert_eq!(eval(context, global, "z.q = 1; (z.q === 1 && x.q === undefined) ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "z !== x ? 1 : 0"), 1);
        rooted!(&in(context) let zv = ObjectValue(z));
        assert!(!zv.get().is_undefined());
        assert!(emulates_undefined(zv.get()));
        emulates(context, global);
    });
}

#[test]
fn a_flagged_class_with_a_call_hook_stays_callable() {
    with_global(|context, global| unsafe {
        let f = JS_NewObject(context.raw_cx(), &HTMLDDA_CALLABLE);
        define(context, global, c"f", f);
        // The slot outranks callability.
        assert_eq!(eval(context, global, "typeof f === 'undefined' ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "!f ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "f == null ? 1 : 0"), 1);
        assert_eq!(type_of(context, f) as u32, JSType::JSTYPE_UNDEFINED as u32);
        // The call hook still runs.
        assert_eq!(eval(context, global, "f()"), 0);
        assert_eq!(eval(context, global, "f(3)"), 13);
        assert_eq!(eval(context, global, "f(7, 8)"), 27);
        rooted!(&in(context) let fv = ObjectValue(f));
        assert!(!js::ToBooleanSlow(fv.handle().into()));
        assert!(emulates_undefined(fv.get()));
    });
}

// ___________________________________________________________________________
// (b2) the proxy call trap

#[test]
fn a_proxy_with_a_call_trap_is_callable() {
    reset();
    with_global(|context, global| unsafe {
        let traps = ProxyTraps { call: Some(call_trap), ..Default::default() };
        let f = proxy(context, global, c"f", &traps, ptr::null(), ptr::null_mut());
        assert_eq!(eval(context, global, "typeof f === 'function' ? 1 : 0"), 1, "the trap makes it callable");
        assert_eq!(type_of(context, f) as u32, JSType::JSTYPE_FUNCTION as u32);
        assert_eq!(eval(context, global, "f(0)"), 1000);
        assert_eq!(eval(context, global, "f(5)"), 1005);
        assert_eq!(eval(context, global, "f('id')"), 2000);
        assert_eq!(eval(context, global, "f()"), -1);
        assert_eq!(CALLS.load(Ordering::SeqCst), 4);
        assert_eq!(LAST_ARGC.load(Ordering::SeqCst), 0, "f() arrived with nothing");
        assert_eq!(LAST_CONSTRUCTING.load(Ordering::SeqCst), 0, "a call is not a construct");
        assert_eq!(LAST_PROXY_MATCHED.load(Ordering::SeqCst), 1, "the trap was handed the proxy");
        // An object argument, and a this that is not the proxy.
        assert_eq!(eval(context, global, "(function () { return f(this) })({})"), 3000);
        assert_eq!(LAST_FIRST.load(Ordering::SeqCst), 3);
        // Function.prototype.call and .apply drive it with an explicit this; the trap sees the arguments.
        assert_eq!(eval(context, global, "Function.prototype.call.call(f, null, 4)"), 1004);
        assert_eq!(LAST_ARGC.load(Ordering::SeqCst), 1);
        assert_eq!(eval(context, global, "Function.prototype.apply.call(f, null, [6, 7])"), 1006);
        assert_eq!(LAST_ARGC.load(Ordering::SeqCst), 2);
        assert_eq!(eval(context, global, "Reflect.apply(f, undefined, [])"), -1);
        // No construct hook, so `new` is a TypeError and the construct trap is never reached.
        assert!(throws(context, global, "new f(1)"));
        assert_eq!(CONSTRUCTS.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn a_proxy_with_a_construct_trap_constructs() {
    reset();
    with_global(|context, global| unsafe {
        let traps = ProxyTraps { call: Some(call_trap), construct: Some(construct_trap), ..Default::default() };
        proxy(context, global, c"f", &traps, ptr::null(), ptr::null_mut());
        assert_eq!(eval(context, global, "typeof f === 'function' ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "f(2)"), 1002);
        assert_eq!(CONSTRUCTS.load(Ordering::SeqCst), 0, "a call must not reach the construct trap");
        assert_eq!(LAST_CONSTRUCTING.load(Ordering::SeqCst), 0, "nor does its frame read as constructing");
        assert_eq!(eval(context, global, "(new f(1, 2)) !== null ? 1 : 0"), 1);
        assert_eq!(LAST_CONSTRUCTING.load(Ordering::SeqCst), 1, "the construct trap sees a construct");
        assert_eq!(eval(context, global, "typeof (new f()) === 'object' ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "let o = new f(9); o === f ? 1 : 0"), 0, "fresh, not the proxy itself");
        assert_eq!(CONSTRUCTS.load(Ordering::SeqCst), 3);
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(LAST_ARGC.load(Ordering::SeqCst), 1, "new f(9) kept its argument");
        assert_eq!(LAST_PROXY_MATCHED.load(Ordering::SeqCst), 1, "the construct trap got the proxy too");
    });
}

#[test]
fn a_proxy_without_a_call_trap_is_unchanged() {
    reset();
    with_global(|context, global| unsafe {
        // Only a property trap, over an undefined private: an object, and calling it throws.
        let traps = ProxyTraps { getOwnPropertyDescriptor: Some(indexed_gopd), ..Default::default() };
        let p = proxy(context, global, c"p", &traps, ptr::null(), ptr::null_mut());
        assert_eq!(eval(context, global, "p[0]"), 42);
        assert_eq!(eval(context, global, "typeof p === 'object' ? 1 : 0"), 1);
        assert_eq!(type_of(context, p) as u32, JSType::JSTYPE_OBJECT as u32);
        assert!(throws(context, global, "p(0)"));
        assert!(throws(context, global, "new p()"));
        assert_eq!(CALLS.load(Ordering::SeqCst), 0);

        // The old rule stands beside the new one: a callable private makes the proxy callable, trap or no trap.
        let target = JS_NewObject(context.raw_cx(), &CALLABLE_DOM);
        rooted!(&in(context) let tv = ObjectValue(target));
        let _g = proxy(context, global, c"g", &traps, ptr::null(), target);
        assert_eq!(eval(context, global, "typeof g === 'function' ? 1 : 0"), 1, "callable through its private");
        assert_eq!(eval(context, global, "g(3)"), 13, "the call reaches the target, not a trap");
        assert_eq!(eval(context, global, "Function.prototype.call.call(g, null, 4)"), 14);
        assert_eq!(CALLS.load(Ordering::SeqCst), 0);
        assert!(tv.get().is_object());
    });
}

#[test]
fn a_flagged_callable_proxy_is_document_all() {
    reset();
    with_global(|context, global| unsafe {
        let traps =
            ProxyTraps { getOwnPropertyDescriptor: Some(indexed_gopd), call: Some(all_call), ..Default::default() };
        let all = proxy(context, global, c"all", &traps, &HTMLDDA_PROXY, ptr::null_mut());
        // Both halves at once, the slot winning the way the engine's own typeof does.
        assert_eq!(eval(context, global, "typeof all === 'undefined' ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "!all ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "all == null ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "all == undefined ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "all !== undefined ? 1 : 0"), 1);
        assert_eq!(eval(context, global, "(all ?? 'd') === all ? 1 : 0"), 1);
        // The indexed getter and the legacy caller both work on it.
        assert_eq!(eval(context, global, "all[0]"), 42);
        assert_eq!(eval(context, global, "all[1]"), 43);
        assert_eq!(eval(context, global, "all(0)"), 42);
        assert_eq!(eval(context, global, "all(2)"), 44);
        assert_eq!(eval(context, global, "all('link')"), 7);
        assert_eq!(eval(context, global, "all() === null ? 1 : 0"), 1);
        assert_eq!(CALLS.load(Ordering::SeqCst), 4);
        assert_eq!(LAST_ARGC.load(Ordering::SeqCst), 0, "the no-argument call arrived empty");
        assert_eq!(LAST_CONSTRUCTING.load(Ordering::SeqCst), 0, "and as a call, not a construct");
        assert_eq!(LAST_PROXY_MATCHED.load(Ordering::SeqCst), 1);
        assert!(throws(context, global, "new all(0)"), "HTMLAllCollection has no [[Construct]]");
        rooted!(&in(context) let av = ObjectValue(all));
        assert_eq!(JS_TypeOfValue(context.raw_cx(), av.handle().into()) as u32, JSType::JSTYPE_UNDEFINED as u32);
        assert!(!js::ToBooleanSlow(av.handle().into()));
        assert!(emulates_undefined(av.get()));
        assert!(!av.get().is_null_or_undefined());
    });
}

// ___________________________________________________________________________
// (c) the emulating object under the collector

#[test]
fn an_emulating_object_keeps_its_shape_transitions_across_collections() {
    // The engine reads the two crash-hunt flags once, when its context is made, so the same binary answers both
    // questions: plain, and with the verifier and a stress window from the environment.
    println!(
        "configuration: LUCINANET_JS_GC_VERIFY={} LUCINANET_JS_GC_STRESS_KB={}",
        std::env::var("LUCINANET_JS_GC_VERIFY").unwrap_or_else(|_| "(unset)".to_string()),
        std::env::var("LUCINANET_JS_GC_STRESS_KB").unwrap_or_else(|_| "(unset)".to_string()),
    );

    /// What B.3.6 changes on the object, and the one thing it leaves alone, read after a step `after`.
    fn emulates(context: &mut JSContext, global: HandleObject, after: &str) {
        assert_eq!(eval(context, global, "typeof x === 'undefined' ? 1 : 0"), 1, "`typeof x` after {after}");
        assert_eq!(eval(context, global, "!x ? 1 : 0"), 1, "`!x` after {after}");
        assert_eq!(eval(context, global, "x == null ? 1 : 0"), 1, "`x == null` after {after}");
        assert_eq!(eval(context, global, "x !== undefined ? 1 : 0"), 1, "`x !== undefined` after {after}");
    }

    /// One collection through the glue's GC entry point.
    fn collect(context: &mut JSContext) {
        unsafe { JS_GC(context, JS::GCReason::API) };
    }

    /// Allocation the collector has to reclaim, so the space a stale edge points into gets handed out again for real.
    fn churn(context: &mut JSContext, global: HandleObject) {
        let src = concat!(
            "for (var j = 0; j < 64; j++) {",
            " var o = {a: j, b: 's' + j, c: [j, j + 1]};",
            " if (o.a + o.c[1] !== j * 2 + 1 || o.b !== 's' + j) throw new Error('churn read wrong');",
            "} 1",
        );
        assert_eq!(eval(context, global, src), 1, "the churn read its own allocations wrong");
    }

    /// `x`'s `p{i}`, read back: `v` negative means the property is gone.
    fn expect_prop(context: &mut JSContext, global: HandleObject, i: usize, v: i64) {
        let (src, what) = if v < 0 {
            (format!("x['p{i}'] === undefined && !('p{i}' in x) ? 1 : 0"), format!("p{i} is absent"))
        } else {
            (format!("x['p{i}'] === {v} ? 1 : 0"), format!("p{i} reads {v}"))
        };
        assert_eq!(eval(context, global, &src), 1, "{what}");
    }

    /// `x['p{i}'] = v`, and what it reads back in the same script.
    fn set_prop(context: &mut JSContext, global: HandleObject, i: usize, v: i64) {
        assert_eq!(eval(context, global, &format!("x['p{i}'] = {v}; x['p{i}']")), v as i32, "p{i} set to {v}");
    }

    with_global(|context, global| unsafe {
        let x = JS_NewObject(context.raw_cx(), &HTMLDDA);
        define(context, global, c"x", x);
        // The object moves at every collection; the root keeps it reachable and keeps its address current.
        rooted!(&in(context) let xv = ObjectValue(x));
        emulates(context, global, "the object was created");

        // What the object holds, mirrored round by round so the last pass can ask for all of it. -1 is absent.
        let mut have = vec![-1i64; 80];
        let mut collections = 0;

        // One property added per round, a collection after each. Every add clones a new shape off the one the object
        // has, and the shape it left caches the edge to the new one: the detached private shape's weak edge, which is
        // what the collector has to repair when it moves that shape.
        for i in 0..24 {
            set_prop(context, global, i, i as i64);
            have[i] = i as i64;
            churn(context, global);
            collect(context);
            collections += 1;
            expect_prop(context, global, i, i as i64);
            if i % 6 == 0 {
                emulates(context, global, &format!("{i} adds each followed by a collection"));
            }
        }

        // A prototype change is a transition as well.
        assert!(!throws(context, global, "var parent = {inherited: 1}; Object.setPrototypeOf(x, parent)"));
        collect(context);
        collections += 1;
        assert_eq!(eval(context, global, "x.inherited"), 1, "the new prototype is reachable after a collection");

        // Past the array-mode limit the object goes to map mode and keeps cloning private shapes. Every fourth add
        // from 28 on deletes the property four rounds back, which is what turns one cached edge into a vec of them.
        for i in 24..80 {
            set_prop(context, global, i, i as i64);
            have[i] = i as i64;
            if i >= 28 && i % 4 == 0 {
                assert_eq!(eval(context, global, &format!("delete x['p{}']; 1", i - 4)), 1, "p{} deleted", i - 4);
                have[i - 4] = -1;
            }
            collect(context);
            collections += 1;
        }
        for i in 24..80 {
            expect_prop(context, global, i, have[i]);
        }
        emulates(context, global, "fifty-six add-and-delete rounds each closed by a collection");

        // Re-add: every deleted slot comes back with a new value, with a collection between the add and the read.
        let mut readded = 0;
        for i in 24..80 {
            if have[i] >= 0 {
                continue;
            }
            let v = 1000 + i as i64;
            set_prop(context, global, i, v);
            have[i] = v;
            collect(context);
            collections += 1;
            expect_prop(context, global, i, v);
            readded += 1;
        }
        assert_eq!(readded, 13, "the deletes above took thirteen properties away");

        // And the same three slots go again, with churn between, so the freed space is reallocated before the read.
        for i in [24usize, 44, 64] {
            assert_eq!(eval(context, global, &format!("delete x['p{i}']; 1")), 1, "p{i} re-deleted");
            have[i] = -1;
            churn(context, global);
            collect(context);
            collections += 1;
        }

        // A freeze is a shape change of its own, and then the whole set is read once more.
        assert!(!throws(context, global, "Object.freeze(x)"));
        collect(context);
        collections += 1;
        for i in 0..80 {
            expect_prop(context, global, i, have[i]);
        }
        emulates(context, global, "freeze and the ninety-plus collections around it");

        // The C entry points, on the root the collector had to keep current.
        assert_eq!(JS_TypeOfValue(context.raw_cx(), xv.handle().into()) as u32, JSType::JSTYPE_UNDEFINED as u32);
        assert!(!js::ToBooleanSlow(xv.handle().into()));
        assert!(emulates_undefined(xv.get()));
        assert!(!xv.get().is_null_or_undefined(), "the slot is not the missing-value test");
        rooted!(&in(context) let nullv = NullValue());
        let mut eq = false;
        assert!(JS::LooselyEqual(context.raw_cx(), xv.handle().into(), nullv.handle().into(), &mut eq));
        assert!(eq, "x == null after every collection");

        // An object made from the emulating one is ordinary: the shape it left behind is unbroken.
        assert_eq!(eval(context, global, "typeof Object.create(x) === 'object' ? 1 : 0"), 1);
        println!("collections driven: {collections}");
    });
}

// ___________________________________________________________________________
// (d) a transition vec replaced by a larger one, under the collector

/// A shape caches its outgoing transitions as a direct edge, then as a vec that is replaced by a larger copy every
/// time it fills (`TransitionVec` starts at four and doubles). The superseded copy is live only while the code that
/// grew it holds it, so a collection moves it and has to repair the shapes its entries name; the same is true of the
/// vec of a detached shape, which no transition-tree walk reaches. Both are driven here through the glue: distinct
/// names added and deleted again on one emulating object's shape, and one new name per fresh plain object.
#[test]
fn a_transition_vec_outgrown_across_collections_keeps_its_properties() {
    // The engine reads the crash-hunt flags once, when its context is made, so the same binary answers both
    // questions: plain, and with the verifier, a stress window and a small heap from the environment.
    println!(
        "configuration: LUCINANET_JS_GC_VERIFY={} LUCINANET_JS_GC_STRESS_KB={} LUCINANET_JS_MIN_WINDOW_MB={} \
         LUCINANET_JS_MIN_HEAP_MB={}",
        std::env::var("LUCINANET_JS_GC_VERIFY").unwrap_or_else(|_| "(unset)".to_string()),
        std::env::var("LUCINANET_JS_GC_STRESS_KB").unwrap_or_else(|_| "(unset)".to_string()),
        std::env::var("LUCINANET_JS_MIN_WINDOW_MB").unwrap_or_else(|_| "(unset)".to_string()),
        std::env::var("LUCINANET_JS_MIN_HEAP_MB").unwrap_or_else(|_| "(unset)".to_string()),
    );

    /// What B.3.6 changes on the object, read after a step `after`.
    fn emulates(context: &mut JSContext, global: HandleObject, after: &str) {
        assert_eq!(eval(context, global, "typeof x === 'undefined' ? 1 : 0"), 1, "`typeof x` after {after}");
        assert_eq!(eval(context, global, "!x ? 1 : 0"), 1, "`!x` after {after}");
        assert_eq!(eval(context, global, "x == null ? 1 : 0"), 1, "`x == null` after {after}");
        assert_eq!(eval(context, global, "x !== undefined ? 1 : 0"), 1, "`x !== undefined` after {after}");
    }

    /// One collection through the glue's GC entry point.
    fn collect(context: &mut JSContext) {
        unsafe { JS_GC(context, JS::GCReason::API) };
    }

    /// Allocation the collector has to reclaim, so the space a superseded vec left gets handed out again.
    fn churn(context: &mut JSContext, global: HandleObject) {
        let src = concat!(
            "for (var j = 0; j < 64; j++) {",
            " var o = {a: j, b: 's' + j, c: [j, j + 1]};",
            " if (o.a + o.c[1] !== j * 2 + 1 || o.b !== 's' + j) throw new Error('churn read wrong');",
            "} 1",
        );
        assert_eq!(eval(context, global, src), 1, "the churn read its own allocations wrong");
    }

    /// `x['p{i}']`, read back: `v` negative means the property is gone.
    fn expect_prop(context: &mut JSContext, global: HandleObject, i: usize, v: i64) {
        let (src, what) = if v < 0 {
            (format!("x['p{i}'] === undefined && !('p{i}' in x) ? 1 : 0"), format!("p{i} is absent"))
        } else {
            (format!("x['p{i}'] === {v} ? 1 : 0"), format!("p{i} reads {v}"))
        };
        assert_eq!(eval(context, global, &src), 1, "{what}");
    }

    with_global(|context, global| unsafe {
        let x = JS_NewObject(context.raw_cx(), &HTMLDDA);
        define(context, global, c"x", x);
        rooted!(&in(context) let xv = ObjectValue(x));
        emulates(context, global, "the object was created");
        let mut collections = 0;

        // (a) One shape and twelve names, each added and then deleted again. `x` comes back to the shape it started
        // the round on, so that shape caches one more transition per round: the direct edge first, a vec from the
        // second name on, and a larger vec replacing it at the fifth push and the ninth.
        for i in 0..12 {
            let src = format!("x['t{i}'] = {i}; x['t{i}']");
            assert_eq!(eval(context, global, &src), i as i32, "t{i} written and read back in one script");
            assert_eq!(eval(context, global, &format!("delete x['t{i}']; 1")), 1, "t{i} deleted again");
            churn(context, global);
            collect(context);
            collections += 1;
            if i % 4 == 3 {
                emulates(context, global, &format!("{} add-and-delete rounds, each closed by a collection", i + 1));
            }
        }

        // (b) The shape a plain `{}` starts from, which the context made before any script ran: one new name per
        // object, every object kept so the shape it made stays live. Some kept arrays widen the live set, so a
        // collection has room to resize and move the whole permanent space along with the vec outgrown in it.
        assert!(!throws(context, global, "var keep = []; var ballast = [];"));
        for i in 0..24 {
            let src = format!("var o = {{}}; o['n{i}'] = {i}; keep.push(o); o['n{i}']");
            assert_eq!(eval(context, global, &src), i as i32, "n{i} written on a fresh plain object and read back");
            if i % 6 == 5 {
                // Half a MiB of live numbers per third round, so that with a 1 MiB allocation window the
                // collector has to grow the heap and move the whole permanent space, its outgrown vecs with it.
                let widen = concat!(
                    "var a = [];",
                    " for (var k = 0; k < 65536; k++) { a[k] = 7; }",
                    " ballast.push(a); 1",
                );
                assert!(!throws(context, global, widen));
                collect(context);
                collections += 1;
            }
        }
        assert_eq!(
            eval(context, global, "keep.every(function (o, i) { return o['n' + i] === i; }) ? 1 : 0"),
            1,
            "all twenty-four kept objects still read the property they were made with"
        );
        assert_eq!(
            eval(
                context,
                global,
                "ballast.every(function (a) { return a.length === 65536 && a[0] === 7 && a[65535] === 7; }) ? 1 : 0"
            ),
            1,
            "the ballast arrays survived the same collections"
        );
        emulates(context, global, "twenty-four fresh plain objects, one new property name each");

        // (c) Properties kept on the emulating object: eight added with a collection after each, four of them — not
        // the last ones — deleted with churn in between, and those four re-added with new values.
        let mut have = vec![-1i64; 8];
        for i in 0..8 {
            let v = 100 + i as i64;
            assert_eq!(eval(context, global, &format!("x['p{i}'] = {v}; x['p{i}']")), v as i32, "p{i} set to {v}");
            have[i] = v;
            collect(context);
            collections += 1;
        }
        for i in [1usize, 3, 5, 7] {
            assert_eq!(eval(context, global, &format!("delete x['p{i}']; 1")), 1, "p{i} deleted");
            have[i] = -1;
            churn(context, global);
            collect(context);
            collections += 1;
        }
        for i in [1usize, 3, 5, 7] {
            let v = 200 + i as i64;
            assert_eq!(eval(context, global, &format!("x['p{i}'] = {v}; x['p{i}']")), v as i32, "p{i} re-added as {v}");
            have[i] = v;
            collect(context);
            collections += 1;
        }
        for i in 0..8 {
            expect_prop(context, global, i, have[i]);
        }
        emulates(context, global, "eight properties, four of them deleted and re-added across collections");

        // The C entry points, on the root the collector had to keep current.
        assert_eq!(JS_TypeOfValue(context.raw_cx(), xv.handle().into()) as u32, JSType::JSTYPE_UNDEFINED as u32);
        assert!(!js::ToBooleanSlow(xv.handle().into()));
        assert!(emulates_undefined(xv.get()));
        assert!(!xv.get().is_null_or_undefined(), "the slot is not the missing-value test");
        rooted!(&in(context) let nullv = NullValue());
        let mut eq = false;
        assert!(JS::LooselyEqual(context.raw_cx(), xv.handle().into(), nullv.handle().into(), &mut eq));
        assert!(eq, "x == null after every collection");

        // An object made from the emulating one is ordinary: the shape it left behind is unbroken.
        assert_eq!(eval(context, global, "typeof Object.create(x) === 'object' ? 1 : 0"), 1);
        println!("collections driven: {collections}");
    });
}
