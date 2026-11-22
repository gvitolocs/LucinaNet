/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// `mozjs_sys::engine::cx::set_function_cache_config` is the way to turn the function cache on in a normal build: process-wide,
// set once, before the first context. A second call, or a call after a context exists, is refused with an error and changes nothing.
#![cfg(not(feature = "env-config"))]

use std::ptr;

use mozjs::jsapi::OnNewGlobalHookOption;
use mozjs::jsval::UndefinedValue;
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{evaluate_script, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};
use mozjs_sys::engine::cache_partition::{cache_counters, lucinanet_js_set_cache_partition};
use mozjs_sys::engine::cx::{set_function_cache_config, FunctionCacheConfig, FunctionCacheConfigError};

const SCRIPT: &str = "function add(a, b) { return a + b; } function twice(f) { return function (x) { return f(f(x)); }; } twice(function (v) { return add(v, 1); })(1)";

fn run_once(engine: &JSEngine) -> u64 {
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
        let options = CompileOptionsWrapper::new(context, c"api.js".to_owned(), 1);
        assert!(evaluate_script(context, global.handle(), SCRIPT, rval.handle_mut(), options).is_ok());
        assert_eq!(rval.get().to_int32(), 3);
    }
    unsafe { cache_counters(raw) }.0
}

fn record_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).map(|d| d.filter_map(Result::ok).filter(|e| e.path().is_file()).count()).unwrap_or(0)
}

#[test]
fn api_turns_the_function_cache_on_once_before_the_first_context() {
    let dir = std::env::temp_dir().join(format!("lucinanet-fncache-api-{}", std::process::id()));
    let other = dir.join("other");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&other).unwrap();
    let on = dir.join("on");
    std::fs::create_dir_all(&on).unwrap();

    let config = FunctionCacheConfig { dir: on.clone(), max_mb: 8, enabled: true };
    assert_eq!(set_function_cache_config(config), Ok(()));

    // A second call is refused and changes nothing.
    let again = FunctionCacheConfig { dir: other.clone(), max_mb: 1, enabled: true };
    assert_eq!(set_function_cache_config(again), Err(FunctionCacheConfigError::AlreadySet));

    let engine = JSEngine::init().unwrap();
    assert!(run_once(&engine) > 0, "the cache did lookups");
    assert!(record_count(&on) > 0, "the disk store of the configured directory got records");
    assert_eq!(record_count(&other), 0, "the refused configuration was not used");

    // A call after a context was created is refused too.
    let late = FunctionCacheConfig { dir: other.clone(), max_mb: 1, enabled: true };
    assert!(matches!(
        set_function_cache_config(late),
        Err(FunctionCacheConfigError::AlreadySet | FunctionCacheConfigError::ContextExists)
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

