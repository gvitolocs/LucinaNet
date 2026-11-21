/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Lazy re-parse through the glue: once a script has run its AST is dropped (nothing left for lazy_trim), and its functions
// that had not run yet parse their own text again on their first call, nested ones included, with the same results.

use std::ptr;

use mozjs::jsapi::OnNewGlobalHookOption;
use mozjs::jsval::UndefinedValue;
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::jsapi::{JS, JS_GC};
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{evaluate_script, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};
use mozjs_sys::engine::lazy_trim;

#[test]
fn functions_parse_again_after_their_script_ran() {
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
        let context = &mut realm;
        let mut run = |src: &str| -> i32 {
            rooted!(&in(context) let mut rval = UndefinedValue());
            let options = CompileOptionsWrapper::new(context, c"reparse.js".to_owned(), 1);
            assert!(evaluate_script(context, global.handle(), src, rval.handle_mut(), options).is_ok(), "{src} threw");
            rval.get().to_int32()
        };
        let lib = "var lib = (function () { var base = 10; function f(x) { return x + base; } \
                   function g(x) { function h(y) { return y * 2 + base; } return h(x) + 1; } \
                   var o = { m(a) { return a * 3; }, get v() { return base * 4; } }; \
                   return { f: f, g: g, o: o, a: (y) => y - base }; })(); 0";
        run(lib);
        let raw = mozjs::rust::Runtime::get().unwrap().as_ptr();
        assert_eq!(lazy_trim::retained_parse_bytes(raw), 0, "the AST is dropped once the script has run");

        // First calls parse each function again, a collection in between
        assert_eq!(run("lib.f(1)"), 11);
        JS_GC(raw, JS::GCReason::API);
        assert_eq!(run("lib.g(3) + lib.o.m(2) + lib.o.v + lib.a(15)"), 17 + 6 + 40 + 5);
        assert!(!lazy_trim::idle_trim(raw, std::time::Duration::from_millis(5)));
    }
}
