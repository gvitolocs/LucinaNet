/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `JS::EnvironmentChain` and `JS::CompileFunction*`, as Servo's inline event handlers use them
//! (`EventTarget::get_compiled_event_handler`): scope objects appended document, form owner, element, the element
//! looked up first; the handler text compiled as a sloppy function body with `event` (or the five `onerror` names) as
//! parameters. These used to be `missing()` stubs, which aborted the browser on the first `onclick="..."`.

use std::ffi::CStr;
use std::ptr::{self, NonNull};

use mozjs::context::JSContext;
use mozjs::gc::HandleObject;
use mozjs::jsapi::{JSFunction, JSObject, JS_GetFunctionObject, OnNewGlobalHookOption, SupportUnscopables, JSPROP_ENUMERATE};
use mozjs::jsapi::GCReason;
use mozjs::jsval::{ObjectValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{
    CompileFunction, CompileFunction1, CompileFunctionUtf8, JS_ClearPendingException, JS_DefineProperty, JS_GetPendingException,
    JS_IsExceptionPending, JS_NewGlobalObject, JS_GC,
};
use mozjs::rust::{
    transform_str_to_source_text, transform_u16_to_source_text, CompileOptionsWrapper, EnvironmentChain, JSEngine, RealmOptions,
    Runtime, SIMPLE_GLOBAL_CLASS,
};

const EVENT: &[&CStr] = &[c"event"];
const ONERROR: &[&CStr] = &[c"event", c"source", c"lineno", c"colno", c"error"];

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

/// The same, with the collector moving every object after each KiB allocated and the heap verified around each collection.
fn with_global_gc_stress(f: impl FnOnce(&mut JSContext, HandleObject)) {
    // The context reads these once, when it is made; the tests run one at a time (RUST_TEST_THREADS=1).
    std::env::set_var("LUCINANET_JS_GC_STRESS_KB", "1");
    std::env::set_var("LUCINANET_JS_GC_VERIFY", "1");
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    std::env::remove_var("LUCINANET_JS_GC_STRESS_KB");
    std::env::remove_var("LUCINANET_JS_GC_VERIFY");
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

/// Evaluates `src` and returns its completion value as a string (`String(value)`); panics if it throws.
fn eval_str(cx: &mut JSContext, global: HandleObject, src: &str) -> String {
    rooted!(&in(cx) let mut rval = UndefinedValue());
    run(cx, global, src, rval.handle_mut());
    assert!(unsafe { JS_DefineProperty(cx, global, c"__v".as_ptr(), rval.handle(), JSPROP_ENUMERATE as u32) });
    run(cx, global, "String(__v)", rval.handle_mut());
    unsafe { mozjs::conversions::jsstr_to_string(cx, NonNull::new(rval.get().to_string()).unwrap()) }
}

fn run(cx: &mut JSContext, global: HandleObject, src: &str, rval: mozjs::gc::MutableHandleValue) {
    let options = CompileOptionsWrapper::new(cx, c"test.js".to_owned(), 1);
    if mozjs::rust::evaluate_script(cx, global, src, rval, options).is_err() {
        unsafe { JS_ClearPendingException(cx) };
        panic!("script threw: {src}");
    }
}

/// Evaluates `src` (an object expression) and returns the object; the caller roots it.
fn eval_obj(cx: &mut JSContext, global: HandleObject, src: &str) -> *mut JSObject {
    let options = CompileOptionsWrapper::new(cx, c"obj.js".to_owned(), 1);
    rooted!(&in(cx) let mut rval = UndefinedValue());
    mozjs::rust::evaluate_script(cx, global, src, rval.handle_mut(), options).expect("object script threw");
    rval.get().to_object()
}

#[derive(Clone, Copy)]
enum Src {
    Utf16,
    Utf8Text,
    Utf8Buf,
}

/// `JS::CompileFunction` (one of the three source forms) of `body` named `name` with `args`, file `handler.html` line `line`.
fn compile(
    cx: &mut JSContext,
    chain: &EnvironmentChain,
    name: &CStr,
    args: &[&CStr],
    body: &str,
    line: u32,
    src: Src,
) -> *mut JSFunction {
    let options = CompileOptionsWrapper::new(cx, c"handler.html".to_owned(), line);
    let argv: Vec<*const std::os::raw::c_char> = args.iter().map(|a| a.as_ptr()).collect();
    unsafe {
        match src {
            Src::Utf16 => {
                let units: Vec<u16> = body.encode_utf16().collect();
                CompileFunction(
                    cx,
                    chain.get(),
                    options.ptr,
                    name.as_ptr(),
                    argv.len() as u32,
                    argv.as_ptr(),
                    &mut transform_u16_to_source_text(&units),
                )
            },
            Src::Utf8Buf => CompileFunction1(
                cx,
                chain.get(),
                options.ptr,
                name.as_ptr(),
                argv.len() as u32,
                argv.as_ptr(),
                &mut transform_str_to_source_text(body),
            ),
            Src::Utf8Text => CompileFunctionUtf8(
                cx,
                chain.get(),
                options.ptr,
                name.as_ptr(),
                argv.len() as u32,
                argv.as_ptr(),
                body.as_ptr().cast(),
                body.len(),
            ),
        }
    }
}

/// Defines the compiled function as the global property `name` (through `JS_GetFunctionObject`, as Servo does).
fn install(cx: &mut JSContext, global: HandleObject, name: &CStr, f: *mut JSFunction) {
    assert!(!f.is_null(), "compile failed");
    let obj = unsafe { JS_GetFunctionObject(f) };
    assert!(!obj.is_null());
    rooted!(&in(cx) let v = ObjectValue(obj));
    assert!(unsafe { JS_DefineProperty(cx, global, name.as_ptr(), v.handle(), JSPROP_ENUMERATE as u32) });
}

/// The pending exception's `name: message`, cleared; panics if none is pending.
fn take_exception(cx: &mut JSContext, global: HandleObject) -> String {
    assert!(unsafe { JS_IsExceptionPending(cx) }, "no pending exception");
    rooted!(&in(cx) let mut exc = UndefinedValue());
    assert!(unsafe { JS_GetPendingException(cx, exc.handle_mut()) });
    unsafe { JS_ClearPendingException(cx) };
    assert!(exc.get().is_object(), "exception is not an object");
    rooted!(&in(cx) let ev = exc.get());
    assert!(unsafe { JS_DefineProperty(cx, global, c"__exc".as_ptr(), ev.handle(), JSPROP_ENUMERATE as u32) });
    eval_str(cx, global, "__exc.name + ': ' + __exc.message")
}

#[test]
fn no_chain_basic_function() {
    for src in [Src::Utf16, Src::Utf8Text, Src::Utf8Buf] {
        with_global(|cx, global| {
            let chain = EnvironmentChain::new(cx, SupportUnscopables::No);
            let f = compile(cx, &chain, c"f", EVENT, "return 1+1", 1, src);
            install(cx, global, c"f", f);
            assert_eq!(eval_str(cx, global, "f()"), "2");
            assert_eq!(eval_str(cx, global, "f.name"), "f");
            assert_eq!(eval_str(cx, global, "f.length"), "1");
            assert_eq!(eval_str(cx, global, "typeof f"), "function");

            let g = compile(cx, &chain, c"g", EVENT, "return [this.tag, event.type].join(',')", 1, src);
            install(cx, global, c"g", g);
            assert_eq!(eval_str(cx, global, "g.call({tag: 'T'}, {type: 'click'})"), "T,click");
        });
    }
}

/// The handler's own name is not a binding inside it (SpiderMonkey compiles a function statement, not a named
/// expression): `onclick` inside an `onclick` handler is the element's property or the global's.
#[test]
fn name_is_not_bound_inside() {
    with_global(|cx, global| {
        eval_str(cx, global, "var onclick = 'global onclick'; 0");
        let chain = EnvironmentChain::new(cx, SupportUnscopables::Yes);
        let f = compile(cx, &chain, c"onclick", EVENT, "return String(onclick)", 1, Src::Utf16);
        install(cx, global, c"h", f);
        assert_eq!(eval_str(cx, global, "h()"), "global onclick");
        assert_eq!(eval_str(cx, global, "h.name"), "onclick");
    });
}

#[test]
fn scope_chain_html_order() {
    with_global(|cx, global| {
        rooted!(&in(cx) let doc = eval_obj(cx, global, "globalThis.docObj = ({d: 1, q: 'doc'})"));
        rooted!(&in(cx) let form = eval_obj(cx, global, "globalThis.formObj = ({f: 2, q: 'form'})"));
        rooted!(&in(cx) let elem = eval_obj(cx, global, "globalThis.elemObj = ({q: 'elem', tag: 'input'})"));
        eval_str(cx, global, "var g = 'glob'; 0");
        let chain = EnvironmentChain::new(cx, SupportUnscopables::Yes);
        // Servo's order: document, form owner, element.
        chain.append(doc.get());
        chain.append(form.get());
        chain.append(elem.get());
        let body = "return q + '|' + f + '|' + d + '|' + (typeof document) + '|' + g + '|' + tag";
        let f = compile(cx, &chain, c"onclick", EVENT, body, 1, Src::Utf16);
        install(cx, global, c"h", f);
        assert_eq!(eval_str(cx, global, "h()"), "elem|2|1|undefined|glob|input");
        // Live objects, not copies: later changes are seen, and a property removed from the element falls through.
        assert_eq!(eval_str(cx, global, "elemObj.tag = 'button'; formObj.f = 22; h()"), "elem|22|1|undefined|glob|button");
        assert_eq!(eval_str(cx, global, "delete elemObj.q; h()"), "form|22|1|undefined|glob|button");
        assert_eq!(eval_str(cx, global, "delete formObj.q; h()"), "doc|22|1|undefined|glob|button");
        // Assignment to a name an object has writes that object.
        let s = compile(cx, &chain, c"onchange", EVENT, "tag = 'set'; d = 5; return 0", 1, Src::Utf8Text);
        install(cx, global, c"s", s);
        assert_eq!(eval_str(cx, global, "s(); elemObj.tag + docObj.d + (typeof globalThis.tag)"), "set5undefined");
    });
}

/// With an empty chain the scope is the global only, and a missing name is a ReferenceError.
#[test]
fn empty_chain_reference_error() {
    with_global(|cx, global| {
        let chain = EnvironmentChain::new(cx, SupportUnscopables::Yes);
        let f = compile(cx, &chain, c"onload", EVENT, "return nothingHere", 1, Src::Utf16);
        install(cx, global, c"h", f);
        assert_eq!(eval_str(cx, global, "try { h(); 'no' } catch (e) { e.name }"), "ReferenceError");
    });
}

#[test]
fn chain_objects_survive_gc() {
    with_global_gc_stress(|cx, global| {
        let chain = EnvironmentChain::new(cx, SupportUnscopables::Yes);
        // Only the chain keeps these objects alive between append and compile.
        {
            rooted!(&in(cx) let doc = eval_obj(cx, global, "({d: 'D'.repeat(3), q: 'doc'})"));
            chain.append(doc.get());
        }
        {
            rooted!(&in(cx) let form = eval_obj(cx, global, "({f: [1, 2, 3], q: 'form'})"));
            chain.append(form.get());
        }
        {
            rooted!(&in(cx) let elem = eval_obj(cx, global, "({q: 'elem', tag: {name: 'input'}})"));
            chain.append(elem.get());
        }
        eval_str(cx, global, "var junk = []; for (var i = 0; i < 2000; i++) junk.push({i: i, s: 'x' + i}); junk = null; 0");
        unsafe { JS_GC(cx, GCReason::API) };
        let body = "var parts = []; for (var k = 0; k < 3; k++) parts.push({k: k}); return q + '|' + f.join('') + '|' + d + '|' + tag.name";
        let f = compile(cx, &chain, c"onclick", EVENT, body, 1, Src::Utf16);
        rooted!(&in(cx) let fobj = unsafe { JS_GetFunctionObject(f) });
        unsafe { JS_GC(cx, GCReason::API) };
        eval_str(cx, global, "var junk2 = []; for (var i = 0; i < 2000; i++) junk2.push({i: i}); junk2 = null; 0");
        unsafe { JS_GC(cx, GCReason::API) };
        rooted!(&in(cx) let v = ObjectValue(fobj.get()));
        assert!(unsafe { JS_DefineProperty(cx, global, c"h".as_ptr(), v.handle(), JSPROP_ENUMERATE as u32) });
        for _ in 0..3 {
            assert_eq!(eval_str(cx, global, "h()"), "elem|123|DDD|input");
            unsafe { JS_GC(cx, GCReason::API) };
        }
        // Appending after a compile, compiling again: the chain is still current after the moves.
        {
            rooted!(&in(cx) let extra = eval_obj(cx, global, "({q: 'extra'})"));
            chain.append(extra.get());
        }
        unsafe { JS_GC(cx, GCReason::API) };
        let f2 = compile(cx, &chain, c"onclick", EVENT, "return q + '|' + d", 1, Src::Utf8Buf);
        install(cx, global, c"h2", f2);
        unsafe { JS_GC(cx, GCReason::API) };
        assert_eq!(eval_str(cx, global, "h2()"), "extra|DDD");
        drop(chain);
        unsafe { JS_GC(cx, GCReason::API) };
        // The handlers keep their scope objects alive themselves once the chain is gone.
        assert_eq!(eval_str(cx, global, "h() + ' ' + h2()"), "elem|123|DDD|input extra|DDD");
    });
}

#[test]
fn delete_chains_under_gc_stress() {
    with_global_gc_stress(|cx, global| {
        let empty = EnvironmentChain::new(cx, SupportUnscopables::No);
        drop(empty);
        let kept = EnvironmentChain::new(cx, SupportUnscopables::No);
        {
            rooted!(&in(cx) let a = eval_obj(cx, global, "({kept: 'yes'})"));
            kept.append(a.get());
        }
        for i in 0..20 {
            let chain = EnvironmentChain::new(cx, SupportUnscopables::Yes);
            {
                rooted!(&in(cx) let o = eval_obj(cx, global, &format!("({{n: {i}}})")));
                chain.append(o.get());
                chain.append(o.get());
            }
            unsafe { JS_GC(cx, GCReason::API) };
            drop(chain);
            unsafe { JS_GC(cx, GCReason::API) };
        }
        let f = compile(cx, &kept, c"f", EVENT, "return kept", 1, Src::Utf16);
        install(cx, global, c"h", f);
        assert_eq!(eval_str(cx, global, "h()"), "yes");
    });
}

#[test]
fn syntax_error_is_pending_not_abort() {
    with_global(|cx, global| {
        let chain = EnvironmentChain::new(cx, SupportUnscopables::Yes);
        let f = compile(cx, &chain, c"onclick", EVENT, "return (1 +", 1, Src::Utf16);
        assert!(f.is_null());
        assert!(take_exception(cx, global).starts_with("SyntaxError"));
        assert!(!unsafe { JS_IsExceptionPending(cx) });
    });
}

/// Text that would close the wrapper's function early is rejected as a body: it never runs outside the handler.
#[test]
fn body_cannot_escape_the_wrapper() {
    for body in [
        "}); throw 'escaped'; (function(){",
        "}}}}); throw 'escaped'; (function(){",
        "} throw 'escaped'; function x() {",
        "*/ throw 'escaped'; /*",
        "}) ; globalThis.escaped = 1; ((",
    ] {
        with_global(|cx, global| {
            let chain = EnvironmentChain::new(cx, SupportUnscopables::Yes);
            rooted!(&in(cx) let elem = eval_obj(cx, global, "({q: 1})"));
            chain.append(elem.get());
            let f = compile(cx, &chain, c"onclick", EVENT, body, 1, Src::Utf16);
            assert!(f.is_null(), "body compiled: {body}");
            let e = take_exception(cx, global);
            assert!(e.starts_with("SyntaxError"), "{body}: {e}");
            assert_eq!(eval_str(cx, global, "typeof escaped"), "undefined");
        });
    }
    // A body ending in a line comment, or holding braces in strings and comments, is fine.
    with_global(|cx, global| {
        let chain = EnvironmentChain::new(cx, SupportUnscopables::Yes);
        let f = compile(cx, &chain, c"onclick", EVENT, "return '})' /* }) */ // })", 1, Src::Utf16);
        install(cx, global, c"h", f);
        assert_eq!(eval_str(cx, global, "h()"), "})");
    });
}

#[test]
fn bad_parameter_name_is_a_syntax_error() {
    with_global(|cx, global| {
        let chain = EnvironmentChain::new(cx, SupportUnscopables::Yes);
        let f = compile(cx, &chain, c"f", &[c"a){throw 1}; (function(b"], "return 1", 1, Src::Utf16);
        assert!(f.is_null());
        assert!(take_exception(cx, global).starts_with("SyntaxError"));
    });
}

#[test]
fn handler_semantics() {
    with_global(|cx, global| {
        rooted!(&in(cx) let elem = eval_obj(cx, global, "({q: 'elem'})"));
        let chain = EnvironmentChain::new(cx, SupportUnscopables::Yes);
        chain.append(elem.get());

        let f = compile(cx, &chain, c"onsubmit", EVENT, "return false", 1, Src::Utf16);
        install(cx, global, c"ret", f);
        assert_eq!(eval_str(cx, global, "ret() === false"), "true");

        let f = compile(cx, &chain, c"onclick", EVENT, "function inner(x) { return x * 2 }\nreturn inner(21) + (function () { return q })()", 1, Src::Utf16);
        install(cx, global, c"nested", f);
        assert_eq!(eval_str(cx, global, "nested()"), "42elem");

        let f = compile(cx, &chain, c"onclick", EVENT, "var leaked = 1; leaked2 = 2; return typeof leaked", 1, Src::Utf16);
        install(cx, global, c"vars", f);
        assert_eq!(eval_str(cx, global, "vars() + typeof leaked + leaked2"), "numberundefined2");

        let f = compile(cx, &chain, c"onclick", EVENT, "return arguments.length + ':' + arguments[1]", 1, Src::Utf16);
        install(cx, global, c"args", f);
        assert_eq!(eval_str(cx, global, "args('e', 'second', 3)"), "3:second");

        let f = compile(cx, &chain, c"onclick", EVENT, "'use strict'; return (function () { return this })() === undefined", 1, Src::Utf16);
        install(cx, global, c"strict", f);
        assert_eq!(eval_str(cx, global, "strict()"), "true");

        let f = compile(cx, &chain, c"onclick", EVENT, "return (function () { return this })() === globalThis", 1, Src::Utf16);
        install(cx, global, c"sloppy", f);
        assert_eq!(eval_str(cx, global, "sloppy()"), "true");

        let f = compile(cx, &chain, c"onclick", EVENT, "'use strict'; undeclaredStrict = 1", 1, Src::Utf16);
        install(cx, global, c"strict2", f);
        assert_eq!(eval_str(cx, global, "try { strict2(); 'no' } catch (e) { e.name }"), "ReferenceError");

        // Wrapper internals are not visible to the handler.
        let f = compile(cx, &chain, c"onclick", EVENT, "return typeof __chain + typeof __env", 1, Src::Utf16);
        install(cx, global, c"internals", f);
        assert_eq!(eval_str(cx, global, "internals()"), "undefinedundefined");
    });
}

#[test]
fn onerror_five_arguments() {
    with_global(|cx, global| {
        let chain = EnvironmentChain::new(cx, SupportUnscopables::Yes);
        let f = compile(cx, &chain, c"onerror", ONERROR, "return [event, source, lineno, colno, error].join(',')", 1, Src::Utf16);
        install(cx, global, c"h", f);
        assert_eq!(eval_str(cx, global, "h.length"), "5");
        assert_eq!(eval_str(cx, global, "h('msg', 'a.js', 3, 4, 'E')"), "msg,a.js,3,4,E");
    });
}

/// The handler's line numbers are the attribute's: a throw on the body's second line reports `line + 1`.
#[test]
fn line_numbers_follow_options() {
    for line in [1u32, 7] {
        with_global(|cx, global| {
            let chain = EnvironmentChain::new(cx, SupportUnscopables::Yes);
            rooted!(&in(cx) let elem = eval_obj(cx, global, "({q: 1})"));
            chain.append(elem.get());
            let f = compile(cx, &chain, c"onclick", EVENT, "var a = 1;\nthrow new Error('x')", line, Src::Utf16);
            install(cx, global, c"h", f);
            let got = eval_str(cx, global, "try { h(); '' } catch (e) { e.stack }");
            assert!(got.contains(&format!("handler.html:{}", line + 1)), "line {line}: {got}");
        });
    }
}

/// `SupportUnscopables::Yes` (what Servo passes): `@@unscopables` hides a scope object's property, as in Firefox.
#[test]
fn unscopables_honoured_with_yes() {
    with_global(|cx, global| {
        eval_str(cx, global, "var hidden = 'global'; 0");
        rooted!(&in(cx) let elem = eval_obj(cx, global, "({hidden: 'elem', shown: 'elem', [Symbol.unscopables]: {hidden: true}})"));
        let chain = EnvironmentChain::new(cx, SupportUnscopables::Yes);
        chain.append(elem.get());
        let f = compile(cx, &chain, c"onclick", EVENT, "return hidden + '|' + shown", 1, Src::Utf16);
        install(cx, global, c"h", f);
        assert_eq!(eval_str(cx, global, "h()"), "global|elem");
    });
}

/// Known gap: with `SupportUnscopables::No` SpiderMonkey ignores `@@unscopables` (it would answer "elem|elem"); the
/// engine's `with` environments always honour it, so this answers as `Yes` does. No Servo caller passes `No`.
#[test]
fn unscopables_gap_with_no() {
    with_global(|cx, global| {
        eval_str(cx, global, "var hidden = 'global'; 0");
        rooted!(&in(cx) let elem = eval_obj(cx, global, "({hidden: 'elem', shown: 'elem', [Symbol.unscopables]: {hidden: true}})"));
        let chain = EnvironmentChain::new(cx, SupportUnscopables::No);
        chain.append(elem.get());
        let f = compile(cx, &chain, c"onclick", EVENT, "return hidden + '|' + shown", 1, Src::Utf16);
        install(cx, global, c"h", f);
        assert_eq!(eval_str(cx, global, "h()"), "global|elem");
    });
}
