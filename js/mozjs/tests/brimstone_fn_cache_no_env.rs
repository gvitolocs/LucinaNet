/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// A normal build takes the function cache's directory only from `mozjs_sys::engine::cx::set_function_cache_config`, never from
// the process environment: the directory is a place bytecode is loaded from and run, and whoever controls a launch environment
// must not be able to name it. (With the `env-config` feature the old environment behaviour is back: brimstone_fn_cache_env.)
#![cfg(not(feature = "env-config"))]

use std::ptr;

use mozjs::jsapi::OnNewGlobalHookOption;
use mozjs::jsval::UndefinedValue;
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{evaluate_script, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};
use mozjs_sys::engine::cache_partition::{cache_counters, lucinanet_js_set_cache_partition};

const SCRIPT: &str = "function add(a, b) { return a + b; } function twice(f) { return function (x) { return f(f(x)); }; } twice(function (v) { return add(v, 1); })(1)";

#[test]
fn environment_does_not_turn_the_function_cache_on() {
    let dir = std::env::temp_dir().join(format!("lucinanet-fncache-noenv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: this test binary runs one test, and no other thread reads the environment meanwhile.
    unsafe {
        std::env::set_var("LUCINANET_JS_FN_CACHE_DIR", &dir);
        std::env::set_var("LUCINANET_JS_FN_CACHE_MB", "1");
        std::env::set_var("LUCINANET_JS_FN_CACHE", "1");
    }
    let engine = JSEngine::init().unwrap();
    let lookups = {
        let mut runtime = Runtime::new(engine.handle());
        let context = runtime.cx();
        let raw = mozjs::rust::Runtime::get().unwrap().as_ptr();
        unsafe {
            rooted!(&in(context) let global = JS_NewGlobalObject(
                context,
                &SIMPLE_GLOBAL_CLASS,
                ptr::null_mut(),
                OnNewGlobalHookOption::FireOnNewGlobalHook,
                &*RealmOptions::default(),
            ));
            lucinanet_js_set_cache_partition(raw, global.get(), b"https://a.test".as_ptr(), 14);
            let mut realm = AutoRealm::new_from_handle(context, global.handle());
            let context = &mut realm;
            rooted!(&in(context) let mut rval = UndefinedValue());
            let options = CompileOptionsWrapper::new(context, c"noenv.js".to_owned(), 1);
            assert!(evaluate_script(context, global.handle(), SCRIPT, rval.handle_mut(), options).is_ok());
            assert_eq!(rval.get().to_int32(), 3);
        }
        unsafe { cache_counters(raw) }.0
    };
    assert_eq!(lookups, 0, "the function cache is off: the environment names no directory in a normal build");
    let entries = std::fs::read_dir(&dir).unwrap().count();
    assert_eq!(entries, 0, "the directory named by the environment is untouched");
    let _ = std::fs::remove_dir_all(&dir);
}
