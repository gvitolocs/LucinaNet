/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The `Intl` globals and the message shapes a page touches at module load, evaluated through the glue exactly as the page
//! writes them: example-site.com/marketplace runs `new Intl.Collator` while its module is evaluating, and its tables sort with
//! `Intl.Collator.prototype.compare` and print with `Intl.NumberFormat.prototype.format`. The fork's `Intl` is ICU4X with baked
//! data for `en`, `da` and `it` only, so a locale outside those is reported unsupported rather than falling back quietly.
//! `style: "currency"` is in the baked data since the fork's Intl.NumberFormat part B, and `notation: "scientific"` and
//! `style: "unit"` still are not: those two throw a RangeError, loudly, by design.
//!
//! The TypeError and ReferenceError messages are the browser-style ones the fork added (`x.y is not a function`, `Cannot read
//! properties of undefined (reading 'z')`): a page that logs `e.message` to its own console gets what a browser would have said.

use std::ptr::NonNull;

use mozjs::context::JSContext;
use mozjs::gc::HandleObject;
use mozjs::jsapi::{OnNewGlobalHookOption, JSPROP_ENUMERATE};
use mozjs::jsval::UndefinedValue;
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{JS_ClearPendingException, JS_DefineProperty, JS_GetPendingException, JS_IsExceptionPending, JS_NewGlobalObject};
use mozjs::rust::{CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};

/// The message the engine gives for a call of a name that was never declared, from the fork at `707ec0fd6d`.
const UNDECLARED_CALL: &str = "ReferenceError: `undefined_fn` is not defined";

/// Runs `f` in a fresh engine with a simple global, its realm entered.
fn with_global(f: impl FnOnce(&mut JSContext, HandleObject)) {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            std::ptr::null_mut(),
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

/// Evaluates `src` and returns whether its completion value is true.
fn eval_true(cx: &mut JSContext, global: HandleObject, src: &str) -> bool {
    rooted!(&in(cx) let mut rval = UndefinedValue());
    run(cx, global, src, rval.handle_mut());
    rval.get().to_boolean()
}

fn run(cx: &mut JSContext, global: HandleObject, src: &str, rval: mozjs::gc::MutableHandleValue) {
    let options = CompileOptionsWrapper::new(cx, c"page.js".to_owned(), 1);
    if mozjs::rust::evaluate_script(cx, global, src, rval, options).is_err() {
        unsafe { JS_ClearPendingException(cx) };
        panic!("script threw: {src}");
    }
}

/// Evaluates `src`, which must throw, and returns the thrown value as `name: message`, read off the pending exception.
fn eval_throw(cx: &mut JSContext, global: HandleObject, src: &str) -> String {
    let options = CompileOptionsWrapper::new(cx, c"page.js".to_owned(), 1);
    rooted!(&in(cx) let mut rval = UndefinedValue());
    assert!(mozjs::rust::evaluate_script(cx, global, src, rval.handle_mut(), options).is_err(), "script did not throw: {src}");
    assert!(unsafe { JS_IsExceptionPending(cx) }, "no pending exception after: {src}");
    rooted!(&in(cx) let mut exc = UndefinedValue());
    assert!(unsafe { JS_GetPendingException(cx, exc.handle_mut()) });
    unsafe { JS_ClearPendingException(cx) };
    assert!(exc.get().is_object(), "exception is not an object: {src}");
    rooted!(&in(cx) let ev = exc.get());
    assert!(unsafe { JS_DefineProperty(cx, global, c"__exc".as_ptr(), ev.handle(), JSPROP_ENUMERATE as u32) });
    eval_str(cx, global, "__exc.name + ': ' + __exc.message")
}

/// The same read from inside the page: `try { src } catch (e) { e }`, as a bundle that reports its own failures sees it.
fn caught(cx: &mut JSContext, global: HandleObject, src: &str) -> String {
    eval_str(cx, global, &format!("try {{ {src}; 'no throw' }} catch (e) {{ e.name + ': ' + e.message }}"))
}

#[test]
fn intl_global_and_collator() {
    with_global(|cx, global| {
        assert_eq!(eval_str(cx, global, "typeof Intl"), "object");
        assert_eq!(eval_str(cx, global, "typeof Intl.Collator"), "function");
        // The page's `new Intl.Collator` at module load: no locale, no options.
        assert!(eval_true(cx, global, "new Intl.Collator() instanceof Intl.Collator"));
        assert_eq!(eval_str(cx, global, "typeof new Intl.Collator().compare"), "function");
    });
}

#[test]
fn collator_compare_and_sort() {
    with_global(|cx, global| {
        // `numeric: true` orders the digit run as a number, so a2 sorts before a10.
        assert!(eval_true(cx, global, r#"new Intl.Collator("en", {numeric: true}).compare("a2", "a10") < 0"#));
        // Sorting with the collator's own `compare` (the callback is the unbound method, as pages pass it).
        assert_eq!(eval_str(cx, global, r#"["b", "a", "C"].sort(new Intl.Collator().compare).join(",")"#), "a,b,C");
    });
}

#[test]
fn locale_compare_on_strings() {
    with_global(|cx, global| {
        assert!(eval_true(cx, global, r#""a".localeCompare("b") < 0"#));
        assert!(eval_true(cx, global, r#""b".localeCompare("a") > 0"#));
        assert!(eval_true(cx, global, r#""a".localeCompare("a") === 0"#));
    });
}

#[test]
fn number_format_and_to_locale_string() {
    with_global(|cx, global| {
        assert_eq!(
            eval_str(cx, global, r#"new Intl.NumberFormat("en-US", {useGrouping: false, maximumFractionDigits: 2}).format(1234.5678)"#),
            "1234.57"
        );
        assert_eq!(eval_str(cx, global, r#"(1234567.891).toLocaleString("en-US")"#), "1,234,567.891");
        // Danish: `.` groups, `,` is the decimal separator.
        assert_eq!(eval_str(cx, global, r#"new Intl.NumberFormat("da").format(1234567.891)"#), "1.234.567,891");
        // Percent style is in the build too.
        assert_eq!(eval_str(cx, global, r#"new Intl.NumberFormat("en-US", {style: "percent"}).format(0.25)"#), "25%");
    });
}

#[test]
fn supported_locales_are_the_baked_ones() {
    with_global(|cx, global| {
        // Only `en`, `da` and `it` have data: `de` and `zh` are dropped rather than silently falling back.
        assert_eq!(
            eval_str(cx, global, r#"Intl.NumberFormat.supportedLocalesOf(["da-DK", "de", "en-GB", "it", "zh"]).join()"#),
            "da-DK,en-GB,it"
        );
    });
}

/// `Error.prototype.stack` is assignable: a page that installs its own error reporting overwrites it and reads it back.
#[test]
fn error_stack_is_assignable() {
    with_global(|cx, global| {
        assert_eq!(
            eval_str(cx, global, r#"var e = new Error("m"); e.stack = "x"; String(e.stack)"#),
            "x"
        );
        assert!(eval_true(cx, global, r#"typeof new Error("m").stack === "string""#));
    });
}

/// The call of an undeclared name and the call of a missing method, as the page's own `catch` reports them.
#[test]
fn browser_style_error_messages() {
    with_global(|cx, global| {
        assert_eq!(caught(cx, global, "undefined_fn()"), UNDECLARED_CALL);
        assert_eq!(eval_throw(cx, global, "undefined_fn()"), UNDECLARED_CALL);
        let msg = caught(cx, global, "({}).f()");
        assert!(msg.contains("f is not a function"), "message was {msg:?}");
        assert!(msg.starts_with("TypeError"), "message was {msg:?}");
        assert_eq!(
            caught(cx, global, "undefined.z"),
            "TypeError: Cannot read properties of undefined (reading 'z')"
        );
    });
}

/// `style: "currency"` has baked data since the fork's Intl.NumberFormat part B, so it formats. The strings are V8's, checked
/// with `node -e` (node v22.23.4) and identical to the fork's own `number_format_currency.js` expectations. What the
/// fork still refuses stays refused: `notation: "scientific"` and `style: "unit"` have no data.
#[test]
fn currency_number_format_formats_usd() {
    with_global(|cx, global| {
        let usd = r#"new Intl.NumberFormat("en-US", {style: "currency", currency: "USD"})"#;
        assert_eq!(eval_str(cx, global, &format!("{usd}.format(1234.5)")), "$1,234.50");
        assert_eq!(eval_str(cx, global, &format!("{usd}.format(-5)")), "-$5.00");
        assert_eq!(
            eval_str(
                cx,
                global,
                r#"new Intl.NumberFormat("en-US", {style: "currency", currency: "USD", currencySign: "accounting"}).format(-5)"#
            ),
            "($5.00)"
        );
        assert_eq!(
            eval_str(
                cx,
                global,
                r#"new Intl.NumberFormat("en-US", {style: "currency", currency: "USD", useGrouping: false, maximumFractionDigits: 0}).format(1234.5)"#
            ),
            "$1235"
        );
        // The page that asks for these still gets a loud RangeError, as before.
        for src in [
            r#"new Intl.NumberFormat("en-US", {style: "currency", currency: "USD", notation: "scientific"})"#,
            r#"new Intl.NumberFormat("en-US", {style: "unit", unit: "kilometer"})"#,
        ] {
            let as_page = caught(cx, global, src);
            assert!(as_page.starts_with("RangeError"), "a page catching it saw {as_page:?}");
            let pending = eval_throw(cx, global, src);
            assert!(pending.starts_with("RangeError"), "the pending exception was {pending:?}");
        }
    });
}
