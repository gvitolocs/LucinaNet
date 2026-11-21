/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// LUCINANET_JS_FN_CACHE_DIR turns on the engine's structural function cache with its disk store; destroying the context
// flushes it, so the directory holds records afterwards. LUCINANET_JS_FN_CACHE=0 keeps it off.

use std::ptr;

use mozjs::jsapi::OnNewGlobalHookOption;
use mozjs::jsval::UndefinedValue;
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{evaluate_script, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};

const SCRIPT: &str = "function add(a, b) { return a + b; } function twice(f) { return function (x) { return f(f(x)); }; } twice(function (v) { return add(v, 1); })(1)";

fn run_once(engine: &JSEngine) {
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
        rooted!(&in(context) let mut rval = UndefinedValue());
        let options = CompileOptionsWrapper::new(context, c"fnc.js".to_owned(), 1);
        assert!(evaluate_script(context, global.handle(), SCRIPT, rval.handle_mut(), options).is_ok());
        assert_eq!(rval.get().to_int32(), 3);
    }
}

fn record_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).map(|d| d.filter_map(Result::ok).filter(|e| e.path().is_file()).count()).unwrap_or(0)
}

#[test]
fn cache_dir_from_the_environment_gets_records_and_the_switch_keeps_it_off() {
    let base = std::env::temp_dir().join(format!("lucinanet-fncache-test-{}", std::process::id()));
    let on = base.join("on");
    let off = base.join("off");
    std::fs::create_dir_all(&on).unwrap();
    std::fs::create_dir_all(&off).unwrap();
    let engine = JSEngine::init().unwrap();

    // SAFETY: this test binary runs one test, and no other thread reads the environment meanwhile.
    unsafe {
        std::env::set_var("LUCINANET_JS_FN_CACHE_DIR", &on);
        std::env::remove_var("LUCINANET_JS_FN_CACHE");
    }
    run_once(&engine);
    assert!(record_count(&on) > 0, "the disk store got records after the context was destroyed");

    unsafe {
        std::env::set_var("LUCINANET_JS_FN_CACHE_DIR", &off);
        std::env::set_var("LUCINANET_JS_FN_CACHE", "0");
    }
    run_once(&engine);
    assert_eq!(record_count(&off), 0, "LUCINANET_JS_FN_CACHE=0 keeps the cache off");

    let _ = std::fs::remove_dir_all(&base);
}
