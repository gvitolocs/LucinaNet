/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Structured clone of ordinary objects whose prototype is NOT the realm's %Object.prototype%: class
// instances, `Object.create(x)` objects and objects made in another realm. Servo's
// history.replaceState/pushState run the engine's structured clone over whatever the page handed them,
// so the record branch of `write_object` (mozjs-sys/src/engine/clone.rs:286-337) must follow the HTML
// Standard's StructuredSerializeInternal, which has no prototype check at all.
//
// The WPT corpus (`tests/wpt/tests/html/webappapis/structured-clone/` of a Servo checkout) has no spec
// text, so the behaviour asserted here is cited by WPT test name:
//   structured-clone-battery-of-tests.js:478 'Object with property on prototype' — a `new Foo()` whose
//       Foo.prototype = {'foo':'bar'} clones, and the copy has no 'foo' (:473 check_absent_property).
//   :484  'Object with non-enumerable property' — own non-enumerable properties are dropped.
//   :497  'Object with non-writable property', :510 'Object with non-configurable property' — the copy's
//       own properties are ordinary writable and configurable data properties.
//   :516  'Object with a getter that throws' — the exception the getter threw is the one the caller sees.
//   :630  'ObjectPrototype must lose its exotic-ness when cloned' — %Object.prototype% itself clones as an
//       ordinary record that can be made some other object's prototype.
//   :671  'A subclass instance will deserialize as its closest serializable superclass' — `class extends Map`
//       comes back as a Map.
//   :459-471 'Array/Object with identical property values', :451 'Object with a circular reference' — shared
//       and cyclic references keep their identity.
//   structured-clone-cross-realm-method.html:12-17 — an instance of another realm's Object/Array/Date/RegExp
//       cloned through that other realm's structuredClone comes back `instanceof` the CALLER's builtins.
//   The corpus has no refusal test for a proxy, a primitive symbol or a module namespace object, so those
//       cases below follow the algorithm's own slot/exotic-object conditions and this glue's existing
//       refusals, which mozjs/tests/brimstone_clone_api.rs:238 already pins down.
//
// mozjs allows one `JSEngine` per process; .cargo/config.toml sets RUST_TEST_THREADS=1, as the other
// mozjs tests rely on.

use std::ffi::CStr;
use std::ptr;

use mozjs::context::JSContext;
use mozjs::jsapi::{self, JS, OnNewGlobalHookOption, Value};
use mozjs::jsval::UndefinedValue;
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{
    evaluate_script, CompileOptionsWrapper, HandleObject, JSEngine, MutableHandleValue, Runtime, RealmOptions,
    SIMPLE_GLOBAL_CLASS,
};

/// A runtime, a global and its realm, as `$cx` (`&mut AutoRealm`) and `$global` (a rooted global).
macro_rules! with_global {
    ($cx:ident, $global:ident, $body:block) => {{
        let engine = JSEngine::init().unwrap();
        let mut runtime = Runtime::new(engine.handle());
        let context = runtime.cx();
        #[allow(unused_unsafe)]
        unsafe {
            rooted!(&in(context) let $global = JS_NewGlobalObject(
                context,
                &SIMPLE_GLOBAL_CLASS,
                ptr::null_mut(),
                OnNewGlobalHookOption::FireOnNewGlobalHook,
                &*RealmOptions::default(),
            ));
            let mut realm = AutoRealm::new_from_handle(context, $global.handle());
            let $cx = &mut realm;
            $body
        }
    }};
}

fn eval(cx: &mut JSContext, global: HandleObject, src: &str, rval: MutableHandleValue) {
    let options = CompileOptionsWrapper::new(cx, c"brimstone_clone_ordinary".to_owned(), 1);
    assert!(evaluate_script(cx, global, src, rval, options).is_ok(), "script failed: {src}");
}

/// Evaluates a script that must produce `true`.
fn check(cx: &mut JSContext, global: HandleObject, src: &str) {
    rooted!(&in(cx) let mut rval = UndefinedValue());
    eval(cx, global, src, rval.handle_mut());
    assert!(rval.is_boolean() && rval.to_boolean(), "not true: {src}");
}

/// Evaluates a script and reports whether its value is truthy, for the cases whose builtin may not be
/// compiled into this engine.
unsafe fn truthy(cx: &mut JSContext, global: HandleObject, src: &str) -> bool {
    rooted!(&in(cx) let mut rval = UndefinedValue());
    eval(cx, global, src, rval.handle_mut());
    rval.to_boolean()
}

unsafe fn set_global_val(cx: &mut JSContext, global: HandleObject, name: &CStr, v: mozjs::rust::Handle<Value>) {
    assert!(jsapi::JS_SetProperty(cx.raw_cx(), global.into(), name.as_ptr(), v.into()));
}

/// Writes an already-evaluated value with `JS_WriteStructuredClone`, reads it back with
/// `JS_ReadStructuredClone` and leaves the clone as the global `__c`.
unsafe fn round_trip_value(
    cx: &mut JSContext,
    global: HandleObject,
    v: JS::HandleValue,
    callbacks: *const jsapi::JSStructuredCloneCallbacks,
) {
    let raw = cx.raw_cx();
    let scope = jsapi::JS::StructuredCloneScope::SameProcess;
    let buf = mozjs::glue::NewJSAutoStructuredCloneBuffer(scope, callbacks);
    assert!(!buf.is_null());
    let data = unsafe { &mut (*buf).data_ };
    let version = unsafe { (*buf).version_ };
    rooted!(&in(cx) let undef = UndefinedValue());
    assert!(
        jsapi::JS_WriteStructuredClone(raw, v, data, scope, ptr::null(), callbacks, ptr::null_mut(), undef.handle().into()),
        "write failed"
    );
    rooted!(&in(cx) let mut out = UndefinedValue());
    assert!(
        jsapi::JS_ReadStructuredClone(raw, data, version, scope, out.handle_mut().into(), ptr::null(), callbacks, ptr::null_mut()),
        "read failed"
    );
    set_global_val(cx, global, c"__c", out.handle());
    mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf);
}

/// `src`'s value, cloned.
unsafe fn round_trip(cx: &mut JSContext, global: HandleObject, src: &str) {
    rooted!(&in(cx) let mut v = UndefinedValue());
    eval(cx, global, src, v.handle_mut());
    round_trip_value(cx, global, v.handle().into(), ptr::null());
}

/// The refusal this glue raises for an object the algorithm cannot serialise: `data_clone_error`
/// (mozjs-sys/src/engine/clone.rs:204), an Error reading "The object could not be cloned.".
const REFUSAL: &str = "The object could not be cloned.";

/// Writes `src`'s value and asserts the refusal above.
unsafe fn expect_refusal(cx: &mut JSContext, global: HandleObject, src: &str) {
    rooted!(&in(cx) let mut v = UndefinedValue());
    eval(cx, global, src, v.handle_mut());
    expect_refusal_value(cx, global, v.handle().into(), src);
}

/// Asserts that writing `v` fails with the refusal above.
unsafe fn expect_refusal_value(cx: &mut JSContext, global: HandleObject, v: JS::HandleValue, what: &str) {
    let raw = cx.raw_cx();
    let scope = jsapi::JS::StructuredCloneScope::SameProcess;
    let buf = mozjs::glue::NewJSAutoStructuredCloneBuffer(scope, ptr::null());
    assert!(!buf.is_null());
    let data = unsafe { &mut (*buf).data_ };
    rooted!(&in(cx) let undef = UndefinedValue());
    assert!(
        !jsapi::JS_WriteStructuredClone(raw, v, data, scope, ptr::null(), ptr::null(), ptr::null_mut(), undef.handle().into()),
        "not refused: {what}"
    );
    assert!(jsapi::JS_IsExceptionPending(raw), "no pending exception for: {what}");
    rooted!(&in(cx) let mut e = UndefinedValue());
    assert!(jsapi::JS_GetPendingException(raw, e.handle_mut().into()));
    jsapi::JS_ClearPendingException(raw);
    assert!(e.is_object(), "the refusal of {what} is not an object");
    set_global_val(cx, global, c"__e", e.handle());
    check(cx, global, &format!("__e instanceof Error && Object.getPrototypeOf(__e) === Error.prototype && __e.message === {REFUSAL:?}"));
    mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf);
}

// ---- (a) an instance of a class: own properties only, and the target realm's %Object.prototype% ----

#[test]
fn class_instance_clones_own_properties_and_loses_its_prototype() {
    with_global!(cx, global, {
        unsafe {
            round_trip(
                cx,
                global.handle(),
                "class Foo { constructor() { this.a = 1; this.b = 'x'; } get g() { return 9; } }; new Foo()",
            );
            check(cx, global.handle(), "__c.a === 1 && __c.b === 'x'");
            // WPT :478 'Object with property on prototype': an inherited accessor is not an own property of
            // the original, so it is not in the record either.
            check(cx, global.handle(), "Object.getOwnPropertyNames(__c).join() === 'a,b'");
            check(cx, global.handle(), "!Object.prototype.hasOwnProperty.call(__c, 'g') && __c.g === undefined");
            // The copy is a fresh ordinary object of the realm the clone was read in.
            check(cx, global.handle(), "Object.getPrototypeOf(__c) === Object.prototype");
            check(cx, global.handle(), "Object.prototype.toString.call(__c) === '[object Object]'");
            check(cx, global.handle(), "!(__c instanceof Foo)");
        }
    });
}

// ---- (b) Object.create(x): inherited properties are not copied ----

#[test]
fn object_create_copies_own_properties_and_drops_inherited() {
    with_global!(cx, global, {
        unsafe {
            round_trip(cx, global.handle(), "var base = { inherited: 1 }; var o = Object.create(base); o.own = 2; o");
            check(cx, global.handle(), "__c.own === 2 && Object.getOwnPropertyNames(__c).join() === 'own'");
            check(cx, global.handle(), "!('inherited' in __c) && __c.inherited === undefined");
            check(cx, global.handle(), "Object.getPrototypeOf(__c) === Object.prototype");
        }
    });
}

// ---- (c) an object of another realm, cloned with this realm current ----

#[test]
fn an_object_of_another_realm_clones_into_the_current_realm() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let other = JS_NewGlobalObject(
            context, &SIMPLE_GLOBAL_CLASS, ptr::null_mut(), OnNewGlobalHookOption::FireOnNewGlobalHook, &*RealmOptions::default(),
        ));
        rooted!(&in(context) let home = JS_NewGlobalObject(
            context, &SIMPLE_GLOBAL_CLASS, ptr::null_mut(), OnNewGlobalHookOption::FireOnNewGlobalHook, &*RealmOptions::default(),
        ));
        assert!(!other.get().is_null() && !home.get().is_null());
        rooted!(&in(context) let mut oproto = UndefinedValue());
        rooted!(&in(context) let mut record = UndefinedValue());
        rooted!(&in(context) let mut instance = UndefinedValue());
        rooted!(&in(context) let mut cycle = UndefinedValue());
        rooted!(&in(context) let mut string = UndefinedValue());
        {
            // Built in `other`, whose realm is current while it is built.
            let mut realm = AutoRealm::new_from_handle(context, other.handle());
            let cx = &mut realm;
            eval(cx, other.handle(), "Object.prototype", oproto.handle_mut());
            eval(cx, other.handle(), "({ k: 1, nested: { z: 2 }, arr: [7, { deep: 'x' }] })", record.handle_mut());
            eval(
                cx,
                other.handle(),
                "class Q { constructor() { this.q = 1; } }; var proto = { inherited: 'no' }; var i = new Q(); Object.setPrototypeOf(i, proto); i",
                instance.handle_mut(),
            );
            // An object that refers to itself, made in the other realm: the identity map is pointer
            // identity and does not look at the realm.
            eval(cx, other.handle(), "var c = { n: 1 }; c.again = c; c.list = [c]; c", cycle.handle_mut());
            eval(cx, other.handle(), "new String('xy')", string.handle_mut());
        }
        {
            // Cloned and read with `home`'s realm current: the state a page's replaceState reaches.
            let mut realm = AutoRealm::new_from_handle(context, home.handle());
            let cx = &mut realm;
            set_global_val(cx, home.handle(), c"__otherObjectPrototype", oproto.handle());
            round_trip_value(cx, home.handle(), record.handle().into(), ptr::null());
            check(cx, home.handle(), "__c.k === 1 && __c.nested.z === 2");
            // WPT cross-realm-method.html:12-17: the builtins the copy answers to are the caller's.
            check(cx, home.handle(), "Object.getPrototypeOf(__c) === Object.prototype && __c instanceof Object");
            check(cx, home.handle(), "Object.getPrototypeOf(__c) !== __otherObjectPrototype");
            // The object from another realm whose own property is an array.
            check(cx, home.handle(), "Array.isArray(__c.arr) && __c.arr.length === 2 && __c.arr[1].deep === 'x'");
            check(cx, home.handle(), "Object.getPrototypeOf(__c.arr) === Array.prototype && Object.getPrototypeOf(__c.nested) === Object.prototype");

            round_trip_value(cx, home.handle(), instance.handle().into(), ptr::null());
            check(cx, home.handle(), "__c.q === 1 && Object.getOwnPropertyNames(__c).join() === 'q'");
            check(cx, home.handle(), "Object.getPrototypeOf(__c) === Object.prototype && !('inherited' in __c)");

            // A cross-realm cycle: both back-references land on this realm's copy.
            round_trip_value(cx, home.handle(), cycle.handle().into(), ptr::null());
            check(cx, home.handle(), "__c.n === 1 && __c.again === __c && __c.list[0] === __c");
            check(cx, home.handle(), "Object.getPrototypeOf(__c) === Object.prototype && Object.getPrototypeOf(__c.list) === Array.prototype");

            // A String object of the other realm is serialised from its [[StringData]] too.
            round_trip_value(cx, home.handle(), string.handle().into(), ptr::null());
            check(cx, home.handle(), "__c instanceof String && Object.getPrototypeOf(__c) === String.prototype && String.prototype.valueOf.call(__c) === 'xy'");
            check(cx, home.handle(), "Object.prototype.toString.call(__c) === '[object String]' && __c.length === 2");
        }
    }
}

// ---- (d) a null-prototype object: already serialisable today, must stay so ----

#[test]
fn null_prototype_object_clones_and_the_copy_is_ordinary() {
    with_global!(cx, global, {
        unsafe {
            round_trip(cx, global.handle(), "var o = Object.create(null); o.a = 1; o.b = 'n'; o");
            check(cx, global.handle(), "__c.a === 1 && __c.b === 'n' && Object.getOwnPropertyNames(__c).join() === 'a,b'");
            // The copy is an ordinary object with the target realm's %Object.prototype%, not a null-prototype one.
            check(cx, global.handle(), "Object.getPrototypeOf(__c) === Object.prototype");
        }
    });
}

#[test]
fn object_prototype_itself_clones_as_an_ordinary_record() {
    with_global!(cx, global, {
        unsafe {
            // WPT :630 'ObjectPrototype must lose its exotic-ness when cloned'.
            round_trip(cx, global.handle(), "Object.prototype");
            check(cx, global.handle(), "__c instanceof Object && !(__c instanceof Array) && __c !== Object.prototype");
            check(cx, global.handle(), "Object.keys(__c).length === 0 && Object.getPrototypeOf(__c) === Object.prototype");
            check(cx, global.handle(), "var np = { some: 'proto' }; Object.setPrototypeOf(__c, np); Object.getPrototypeOf(__c) === np");
        }
    });
}

// ---- String objects: the [[StringData]] slot, not the prototype (review finding 1) ----

#[test]
fn string_objects_clone_from_their_string_data_whatever_their_prototype() {
    with_global!(cx, global, {
        unsafe {
            // The String record is the wrapped value, which the object holds in [[StringData]]
            // whatever its prototype and whichever realm made it: %String.prototype% itself (the empty
            // string), a reparented `new String(s)`, a String subclass instance, another realm's
            // String object (that one in the cross-realm test above).
            round_trip(cx, global.handle(), "String.prototype");
            check(cx, global.handle(), "Object.prototype.toString.call(__c) === '[object String]' && String.prototype.valueOf.call(__c) === ''");
            check(cx, global.handle(), "__c instanceof String && Object.getPrototypeOf(__c) === String.prototype && __c.length === 0");

            round_trip(cx, global.handle(), "Object.setPrototypeOf(new String('ab'), null)");
            check(cx, global.handle(), "Object.prototype.toString.call(__c) === '[object String]' && String.prototype.valueOf.call(__c) === 'ab' && __c.length === 2");

            round_trip(cx, global.handle(), "Object.setPrototypeOf(new String('cd'), Object.prototype)");
            check(cx, global.handle(), "Object.prototype.toString.call(__c) === '[object String]' && String.prototype.valueOf.call(__c) === 'cd'");

            // WPT :671 'A subclass instance will deserialize as its closest serializable superclass':
            // a String subclass comes back as a String, and the record is the wrapped value alone, so
            // its own extra properties are dropped exactly as with the Map and Error subclasses.
            round_trip(cx, global.handle(), "class S extends String { constructor() { super('zz'); this.extra = 1; } }; new S()");
            check(cx, global.handle(), "__c instanceof String && String.prototype.valueOf.call(__c) === 'zz'");
            check(cx, global.handle(), "__c.extra === undefined && !Object.prototype.hasOwnProperty.call(__c, 'extra')");
        }
    });
}

// ---- (e) own accessors: read with [[Get]], stored as values ----

#[test]
fn own_getter_is_invoked_and_stored_as_a_data_property() {
    with_global!(cx, global, {
        unsafe {
            round_trip(
                cx,
                global.handle(),
                "var calls = 0; class T { constructor() { this.fixed = 1; } }; var t = new T(); Object.defineProperty(t, 'lazy', { get: function () { calls += 1; return 'v' + calls; }, enumerable: true, configurable: true }); t",
            );
            check(cx, global.handle(), "__c.lazy === 'v1' && __c.fixed === 1");
            // The getter was run once, and what the copy holds is a value, not an accessor.
            check(cx, global.handle(), "calls === 1");
            check(
                cx,
                global.handle(),
                "var d = Object.getOwnPropertyDescriptor(__c, 'lazy'); d.get === undefined && d.set === undefined && d.value === 'v1' && d.writable === true && d.enumerable === true && d.configurable === true",
            );
            check(cx, global.handle(), "__c.lazy === 'v1' && calls === 1");

            // The same on a plain object, which already worked: unchanged behaviour.
            round_trip(
                cx,
                global.handle(),
                "var p = 0; var o = {}; Object.defineProperty(o, 'q', { get: function () { p += 1; return p; }, enumerable: true }); o",
            );
            check(cx, global.handle(), "__c.q === 1 && p === 1");
        }
    });
}

#[test]
fn a_getter_that_throws_propagates_and_nothing_is_cloned() {
    with_global!(cx, global, {
        unsafe {
            // WPT :516 'Object with a getter that throws': the exception the getter threw is the caller's.
            let raw = cx.raw_cx();
            rooted!(&in(cx) let mut v = UndefinedValue());
            eval(
                cx,
                global.handle(),
                "var boom = new RangeError('getter-boom'); class E { constructor() { this.own = 1; } }; var e = new E(); Object.defineProperty(e, 'bad', { get: function () { throw boom; }, enumerable: true }); e",
                v.handle_mut(),
            );
            let scope = jsapi::JS::StructuredCloneScope::SameProcess;
            let buf = mozjs::glue::NewJSAutoStructuredCloneBuffer(scope, ptr::null());
            let data = unsafe { &mut (*buf).data_ };
            let version = unsafe { (*buf).version_ };
            rooted!(&in(cx) let undef = UndefinedValue());
            assert!(
                !jsapi::JS_WriteStructuredClone(raw, v.handle().into(), data, scope, ptr::null(), ptr::null(), ptr::null_mut(), undef.handle().into()),
                "the throwing getter was swallowed"
            );
            rooted!(&in(cx) let mut e = UndefinedValue());
            assert!(jsapi::JS_GetPendingException(raw, e.handle_mut().into()));
            jsapi::JS_ClearPendingException(raw);
            set_global_val(cx, global.handle(), c"__e", e.handle());
            check(cx, global.handle(), "__e === boom && __e instanceof RangeError && __e.message === 'getter-boom'");

            // Nothing usable was written: reading it back is refused too, so no state entry exists.
            rooted!(&in(cx) let mut out = UndefinedValue());
            assert!(
                !jsapi::JS_ReadStructuredClone(raw, data, version, scope, out.handle_mut().into(), ptr::null(), ptr::null(), ptr::null_mut()),
                "a partial clone read back as a value"
            );
            assert!(jsapi::JS_IsExceptionPending(raw));
            jsapi::JS_ClearPendingException(raw);
            mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf);
        }
    });
}

// ---- a getter that re-shapes the object while it is being cloned ----

#[test]
fn a_getter_running_during_the_clone_can_delete_a_later_key_or_add_one() {
    with_global!(cx, global, {
        unsafe {
            // The key list is taken before any value is read (clone.rs: enumerable_string_props calls
            // own_property_keys once), so an own accessor that deletes a later key makes that key leave the
            // clone instead of being cloned as undefined, and a key the accessor adds never enters it.
            round_trip(
                cx,
                global.handle(),
                "var ran = []; var d = { get a() { ran.push('a'); delete d.c; return 1; }, b: 2, c: 3 }; d",
            );
            check(cx, global.handle(), "Object.keys(__c).join() === 'a,b' && __c.a === 1 && __c.b === 2");
            check(cx, global.handle(), "ran.join() === 'a' && !('c' in __c)");

            // Growing the object from the accessor: neither an assignment nor defineProperty puts the new
            // key in this clone, but it is there on the source for the next one.
            round_trip(
                cx,
                global.handle(),
                "var g = { get a() { g.z = 9; Object.defineProperty(g, 'q', { value: 1, enumerable: true, writable: true, configurable: true }); return 1; }, b: 2 }; g",
            );
            check(cx, global.handle(), "Object.keys(__c).join() === 'a,b' && __c.b === 2");
            check(cx, global.handle(), "!('z' in __c) && !('q' in __c) && g.z === 9 && g.q === 1");
        }
    });
}

// ---- (f) which keys are copied, and in which order ----

#[test]
fn symbol_and_non_enumerable_own_properties_are_dropped_and_key_order_is_own_keys() {
    with_global!(cx, global, {
        unsafe {
            round_trip(
                cx,
                global.handle(),
                "class K { constructor() { this.z = 1; } }; var k = new K(); k[Symbol.iterator] = 'sym'; k.s = 'str'; Object.defineProperty(k, 'hidden', { value: 1, enumerable: false, writable: true, configurable: true }); k[42] = 'forty-two'; k.y = 2; k",
            );
            check(cx, global.handle(), "Object.getOwnPropertySymbols(__c).length === 0 && __c[Symbol.iterator] === undefined");
            check(cx, global.handle(), "!('hidden' in __c) && !Object.prototype.hasOwnProperty.call(__c, 'hidden')");
            // OrdinaryOwnPropertyKeys: array indices in ascending numeric order, then the strings in
            // insertion order (ECMA-262 10.7.15); the record keeps that order.
            check(cx, global.handle(), "Object.keys(__c).join() === '42,z,s,y'");
            check(cx, global.handle(), "__c[42] === 'forty-two' && __c.z === 1 && __c.s === 'str' && __c.y === 2");

            // A non-writable, non-configurable own enumerable property becomes an ordinary data property
            // (WPT :497 and :510).
            round_trip(
                cx,
                global.handle(),
                "var f = Object.create(null); Object.defineProperty(f, 'ro', { value: 'v', enumerable: true, writable: false, configurable: false }); f",
            );
            check(
                cx,
                global.handle(),
                "(function () { var d = Object.getOwnPropertyDescriptor(__c, 'ro'); return __c.ro === 'v' && d.writable === true && d.configurable === true; })()",
            );
            check(cx, global.handle(), "delete __c.ro && !('ro' in __c)");

            // Indices are ordered by value, not by the order they were assigned (OrdinaryOwnPropertyKeys);
            // a previous version of the writer walked the raw shape list and kept insertion order.
            round_trip(cx, global.handle(), "var q = {}; q.b = 1; q.a = 2; q[7] = 3; q[3] = 4; q");
            check(cx, global.handle(), "Object.keys(__c).join() === '3,7,b,a'");
        }
    });
}

// ---- (g) references: cycles and repeats among objects of any prototype ----

#[test]
fn cycles_and_shared_references_survive_in_class_instances() {
    with_global!(cx, global, {
        unsafe {
            // WPT :451 'Object with a circular reference', :468 'Object with identical property values'.
            round_trip(
                cx,
                global.handle(),
                "class Node { constructor(name) { this.name = name; } }; var o = new Node('o'); o.self = o; var shared = new Node('shared'); ({ o: o, a: [shared, shared], b: { again: shared } })",
            );
            check(cx, global.handle(), "__c.o.self === __c.o && __c.o.name === 'o'");
            check(cx, global.handle(), "__c.a[0] === __c.a[1] && __c.a[0] === __c.b.again && __c.a[0].name === 'shared'");
            check(cx, global.handle(), "Object.getPrototypeOf(__c.o) === Object.prototype && Object.getPrototypeOf(__c.a[0]) === Object.prototype");
        }
    });
}

// ---- (h) class instances nested in the builtins the algorithm names ----

#[test]
fn class_instances_nested_in_arrays_maps_and_sets_clone() {
    with_global!(cx, global, {
        unsafe {
            round_trip(
                cx,
                global.handle(),
                "class Item { constructor(id) { this.id = id; } }; var inner = new Item(3); [ new Item(1), { m: new Map([['k', new Item(2)]]), s: new Set([inner, inner]) }, inner ]",
            );
            check(cx, global.handle(), "Array.isArray(__c) && __c.length === 3");
            check(cx, global.handle(), "__c[0].id === 1 && Object.getPrototypeOf(__c[0]) === Object.prototype");
            check(cx, global.handle(), "__c[1].m instanceof Map && __c[1].m.get('k').id === 2 && __c[1].m.size === 1");
            check(cx, global.handle(), "__c[1].s instanceof Set && __c[1].s.size === 1");
            check(cx, global.handle(), "__c[1].s.has(__c[2]) && __c[2].id === 3");

            // WPT :671 'A subclass instance will deserialize as its closest serializable superclass':
            // `class extends Map` keeps the Map serialisation, entries and all.
            round_trip(
                cx,
                global.handle(),
                "class M extends Map { get extra() { return 1; } }; var m = new M(); m.set('k', 1); m.set({ o: 2 }, 'v'); m.tag = 'own'; m",
            );
            check(cx, global.handle(), "__c instanceof Map && __c.size === 2 && __c.get('k') === 1 && __c.get({ o: 2 }) === undefined");
            // The Map record carries its entries and nothing else: the own property is not copied.
            check(cx, global.handle(), "!('tag' in __c)");
            check(cx, global.handle(), "Object.getPrototypeOf(__c) === Map.prototype");
        }
    });
}

// ---- (i) the refusals the spec still requires ----

#[test]
fn callables_are_refused() {
    with_global!(cx, global, {
        unsafe {
            expect_refusal(cx, global.handle(), "(function f() {})");
            expect_refusal(cx, global.handle(), "(class Bar { })");
            expect_refusal(cx, global.handle(), "(function*(){ yield 1; })");
            // An async function and an async generator are callables too; %Function.prototype% is the
            // function object you reach from any of them, and it is refused like the rest.
            expect_refusal(cx, global.handle(), "(async function () {})");
            expect_refusal(cx, global.handle(), "(async function*(){ yield 1; })");
            expect_refusal(cx, global.handle(), "Function.prototype");
            // A bound function has [[BoundTargetFunction]]: an internal slot the algorithm does not name,
            // and it is callable besides.
            expect_refusal(cx, global.handle(), "(function () {}).bind(null)");
            expect_refusal(cx, global.handle(), "Math.max");
            expect_refusal(cx, global.handle(), "({ f: function () {} })");
        }
    });
}

#[test]
fn proxies_are_refused() {
    with_global!(cx, global, {
        unsafe {
            // A proxy is an exotic object with [[ProxyHandler]] and [[ProxyTarget]]: the algorithm names
            // only the ordinary-object case, so both of these are DataCloneError even though the target's
            // own prototype would have been serialisable.
            expect_refusal(cx, global.handle(), "new Proxy({ a: 1 }, {})");
            expect_refusal(cx, global.handle(), "new Proxy(function () {}, {})");
            expect_refusal(cx, global.handle(), "new Proxy({ a: 1 }, { getPrototypeOf: function () { return Object.prototype; } })");
            expect_refusal(cx, global.handle(), "({ p: new Proxy([], {}) })");
        }
    });
}

#[test]
fn objects_with_other_internal_slots_are_refused() {
    with_global!(cx, global, {
        unsafe {
            expect_refusal(cx, global.handle(), "Promise.resolve(1)");
            expect_refusal(cx, global.handle(), "new WeakMap()");
            expect_refusal(cx, global.handle(), "new WeakSet()");
            expect_refusal(cx, global.handle(), "new WeakRef({})");
            expect_refusal(cx, global.handle(), "new FinalizationRegistry(function () {})");
            // A generator object ([[GeneratorState]]) and an iterator helper, which inherits from
            // %IteratorPrototype% rather than %Object.prototype%.
            expect_refusal(cx, global.handle(), "(function*(){ yield 1; })()");
            expect_refusal(cx, global.handle(), "[1, 2][Symbol.iterator]().map(function (x) { return x; })");
            // An arguments object is an exotic object the algorithm does not name: it has [[ArgsData]]
            // whether or not the function is strict, and its prototype is %Object.prototype% either way.
            expect_refusal(cx, global.handle(), "(function () { return arguments; })(1, 2)");
            expect_refusal(cx, global.handle(), "(function () { 'use strict'; return arguments; })(1, 2)");
            // The object an async generator call returns ([[AsyncGeneratorState]], on
            // %AsyncGeneratorPrototype%).
            expect_refusal(cx, global.handle(), "(async function*(){ yield 1; })()");
            // A JSON.rawJSON object carries [[RawJSONData]]. The builtin is behind a build option, so the
            // case is only checked where the engine has it.
            if truthy(cx, global.handle(), "typeof JSON.rawJSON === 'function'") {
                expect_refusal(cx, global.handle(), "JSON.rawJSON('1')");
            } else {
                // `JSON.rawJSON` is not compiled into this engine, so this refusal is not tested here.
            }
            expect_refusal(cx, global.handle(), "({ w: new WeakMap() })");
        }
    });
}

#[test]
fn a_module_namespace_object_is_refused() {
    with_global!(cx, global, {
        unsafe {
            // A module namespace object is exotic and carries [[Namespace]], which the algorithm does
            // not name. The harness can build one without a load hook: a synthetic module has nothing to
            // link (mozjs-sys/src/engine/module.rs:920) and its namespace is made on request (:1128-1131).
            let raw = cx.raw_cx();
            rooted!(&in(cx) let s = jsapi::JS_NewStringCopyN(raw, c"abcd".as_ptr(), 4));
            assert!(!s.get().is_null());
            rooted!(&in(cx) let sv = mozjs::jsval::StringValue(&*s.get()));
            rooted!(&in(cx) let m = JS::CreateDefaultExportSyntheticModule(raw, sv.handle().into()));
            assert!(!m.get().is_null(), "no synthetic module");
            assert!(JS::ModuleLink(raw, m.handle().into()), "the synthetic module did not link");
            rooted!(&in(cx) let ns = JS::GetModuleNamespace(raw, m.handle().into()));
            assert!(!ns.get().is_null(), "no module namespace object");
            rooted!(&in(cx) let nsv = mozjs::jsval::ObjectValue(ns.get()));
            expect_refusal_value(cx, global.handle(), nsv.handle().into(), "a module namespace object");
        }
    });
}

#[test]
fn symbols_are_refused() {
    with_global!(cx, global, {
        unsafe {
            // A primitive symbol value (write_value's own branch) and, separately, a symbol wrapper object:
            // an object with [[SymbolData]] is not an ordinary object either, so it is refused with the
            // standard message like every other named refusal.
            expect_refusal(cx, global.handle(), "Symbol('s')");
            expect_refusal(cx, global.handle(), "({ s: Symbol('nested') })");
            expect_refusal(cx, global.handle(), "Object(Symbol('wrap'))");
            expect_refusal(cx, global.handle(), "({ s: Object(Symbol('nested')) })");
        }
    });
}

#[test]
fn the_builtin_records_still_round_trip() {
    with_global!(cx, global, {
        unsafe {
            // The branches ahead of the record branch are untouched: each of these keeps its own tag.
            round_trip(cx, global.handle(), "({ d: new Date(123456789), r: /ab+c/gi, e: new TypeError('boom'), a: [1, , 3], b: new ArrayBuffer(4) })");
            check(cx, global.handle(), "__c.d.getTime() === 123456789 && __c.r.source === 'ab+c' && __c.e instanceof TypeError");
            check(cx, global.handle(), "Array.isArray(__c.a) && __c.a.length === 3 && !(1 in __c.a) && __c.b instanceof ArrayBuffer && __c.b.byteLength === 4");
            round_trip(
                cx,
                global.handle(),
                "({ bo: new Boolean(true), no: new Number(3.5), so: new String('wrap'), bi: Object(10n) })",
            );
            check(cx, global.handle(), "__c.bo.valueOf() === true && __c.no.valueOf() === 3.5 && __c.so.valueOf() === 'wrap' && __c.bi.valueOf() === 10n");
            check(cx, global.handle(), "__c.bo instanceof Boolean && __c.so instanceof String");
        }
    });
}
