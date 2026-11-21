/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// mozjs_sys::engine::lazy_trim: idle slices give back the parse trees lazy stubs keep, and the functions still work. With
// re-parsing off (LUCINANET_JS_REPARSE=0): by default a script's AST is dropped once it has run, so there is nothing to trim
// (tests/brimstone_lazy_reparse.rs).

use std::ptr;
use std::time::Duration;

use mozjs::jsapi::OnNewGlobalHookOption;
use mozjs::jsval::UndefinedValue;
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{evaluate_script, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};
use mozjs_sys::engine::lazy_trim;

#[test]
fn idle_trim_frees_retained_parse_trees() {
    // Budget 0: everything over it is trimmed. Set before the engine starts; read at each slice.
    unsafe { std::env::set_var("LUCINANET_JS_PARSE_BUDGET_MB", "0") };
    unsafe { std::env::set_var("LUCINANET_JS_REPARSE", "0") };
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
            let options = CompileOptionsWrapper::new(context, c"trim.js".to_owned(), 1);
            assert!(evaluate_script(context, global.handle(), src, rval.handle_mut(), options).is_ok(), "{src} threw");
            rval.get().to_int32()
        };
        let lib = "var lib = (function () { function f(x) { return x + 1; } \
                   function g(x) { function h(y) { return y * 2; } return h(x) + 1; } return { f: f, g: g }; })(); 0";
        run(lib);
        let raw = mozjs::rust::Runtime::get().unwrap().as_ptr();
        assert!(lazy_trim::retained_parse_bytes(raw) > 0);
        let mut slices = 0;
        while lazy_trim::idle_trim(raw, Duration::from_millis(5)) {
            slices += 1;
            assert!(slices < 1000, "trim never finished");
        }
        assert_eq!(lazy_trim::retained_parse_bytes(raw), 0);
        assert_eq!(run("lib.f(1) + lib.g(3)"), 9);
        // Nothing over the budget: a slice does nothing and says so.
        assert!(!lazy_trim::idle_trim(raw, Duration::from_millis(5)));
    }
}
