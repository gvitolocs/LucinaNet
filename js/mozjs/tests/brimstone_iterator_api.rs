/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Host iteration must match SpiderMonkey 153's vm/ForOfIterator.cpp,
// including its AllowNonIterable API contract (only undefined is allowed).
use mozjs::jsapi::{
    JS_ClearPendingException, JS_IsExceptionPending, OnNewGlobalHookOption, RootedObject,
    RootedValue, JS, JS_GC,
};
use mozjs::jsval::{Int32Value, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{InitRealmStandardClasses, JS_NewGlobalObject};
use mozjs::rust::{
    evaluate_script, for_of, CompileOptionsWrapper, ForOfIterationFailure, JSEngine, RealmOptions,
    Runtime, SIMPLE_GLOBAL_CLASS,
};
use std::{ops::ControlFlow, ptr};

// Roots must be removed even when an assertion fails.
struct IteratorRoots<'a>(&'a mut JS::ForOfIterator);
impl Drop for IteratorRoots<'_> {
    fn drop(&mut self) {
        unsafe {
            self.0.nextMethod.remove_from_root_stack();
            self.0.iterator.remove_from_root_stack();
        }
    }
}

#[test]
fn host_iterator_protocol() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context, &SIMPLE_GLOBAL_CLASS, ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook, &*RealmOptions::default()));
        let mut realm = AutoRealm::new_from_handle(context, global.handle());
        let context = &mut realm;
        assert!(InitRealmStandardClasses(context));
        rooted!(&in(context) let mut value = UndefinedValue());

        macro_rules! eval {
            ($source:expr) => {{
                let options = CompileOptionsWrapper::new(context, c"iterator.js".to_owned(), 1);
                assert!(evaluate_script(
                    context,
                    global.handle(),
                    $source,
                    value.handle_mut(),
                    options
                )
                .is_ok())
            }};
        }

        // Nullish values fail ToObject even when non-iterables are allowed.
        // A null @@iterator is also an error in this SpiderMonkey host API.
        for source in ["null", "undefined", "({[Symbol.iterator]: null})",
            "({[Symbol.iterator]: 7})", "({[Symbol.iterator]() { return 7; }})",
            "({[Symbol.iterator]() { throw new Error('iterator'); }})",
            "({[Symbol.iterator]() { return {next() { return 7; }}; }})",
            "({[Symbol.iterator]() { return {next() { throw new Error('next'); }}; }})",
            "({[Symbol.iterator]() { return {next() { return {get done() { throw 7; }}; }}; }})",
            "({[Symbol.iterator]() { return {next() { return {done:false, get value() { throw 7; }}; }}; }})"] {
            eval!(source);
            let result = for_of(context, value.handle(), |_, _| -> Result<_, ForOfIterationFailure<()>> {
                panic!("invalid iterable yielded a value");
            });
            assert!(matches!(result, Err(ForOfIterationFailure::JSFailed)), "{source}: {result:?}");
            assert!(JS_IsExceptionPending(context.raw_cx()), "{source}");
            JS_ClearPendingException(context.raw_cx());
        }
        for source in ["({})", "({[Symbol.iterator]: undefined})", "42"] {
            eval!(source);
            let result = for_of(
                context,
                value.handle(),
                |_, _| -> Result<_, ForOfIterationFailure<()>> {
                    panic!("non-iterable yielded a value");
                },
            );
            assert!(matches!(
                result,
                Err(ForOfIterationFailure::ValueIsNotIterable)
            ));
            assert!(!JS_IsExceptionPending(context.raw_cx()));
        }

        // The @@iterator getter and method both receive the original primitive,
        // not its temporary wrapper. next is fetched once, then cached.
        eval!(
            r#"Object.defineProperty(Number.prototype, Symbol.iterator, {
            configurable:true,
            get() { 'use strict';
                if (this !== 42) throw new Error('boxed getter receiver');
                return function() { 'use strict';
                    if (this !== 42) throw new Error('boxed method receiver');
                    let n = 0;
                    return { get next() {
                        if (n !== 0) throw new Error('next fetched again');
                        return function() { return n < 2 ? {value:17 + n++} : {done:true}; };
                    }};
                };
            }
        }); 42"#
        );
        let mut seen = Vec::new();
        let result = for_of(
            context,
            value.handle(),
            |cx, v| -> Result<_, ForOfIterationFailure<()>> {
                // The iterator and cached next function are movable engine objects.
                JS_GC(cx.raw_cx(), JS::GCReason::API);
                seen.push(v.get().to_int32());
                Ok(ControlFlow::Continue(()))
            },
        );
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(seen, [17, 18]);

        // next(done=true) clears the output and never reads result.value.
        eval!("({[Symbol.iterator]() { return {next() { return {done:1, get value() { throw 7; }}; }}; }})");
        let raw = context.raw_cx();
        let mut it = JS::ForOfIterator {
            cx_: raw,
            iterator: RootedObject::new_unrooted(ptr::null_mut()),
            nextMethod: RootedValue::new_unrooted(UndefinedValue()),
            index: u32::MAX,
            ..std::mem::zeroed()
        };
        RootedObject::add_to_root_stack(&raw mut it.iterator, raw);
        RootedValue::add_to_root_stack(&raw mut it.nextMethod, raw);
        let roots = IteratorRoots(&mut it);
        let it = &mut *roots.0;
        assert!(it.init(
            value.handle().into(),
            JS::ForOfIterator_NonIterableBehavior::ThrowOnNonIterable
        ));
        {
            rooted!(&in(context) let mut output = Int32Value(99));
            let mut done = false;
            assert!(it.next(output.handle_mut().into(), &mut done));
            assert!(done);
            assert!(
                output.get().is_undefined(),
                "completed iterator retained its previous output"
            );
        }
    }
}
