/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// mozjs_sys::engine::cache_partition: the function cache is shared only among globals with the same top-level site.

use std::ptr;

use mozjs::jsapi::OnNewGlobalHookOption;
use mozjs::jsval::UndefinedValue;
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{evaluate_script, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};
use mozjs_sys::engine::cache_partition::{cache_counters, lucinanet_js_set_cache_partition};

const FIRST: &str = "function k1(a, b) { var s = 0; for (var i = a; i < b; i++) { s += i; } return s; } k1(1, 5)";
const RENAMED: &str = "function m1(p, q) { var t = 0; for (var j = p; j < q; j++) { t += j; } return t; } m1(1, 5)";

#[test]
fn function_cache_is_partitioned_by_site() {
    let dir = std::env::temp_dir().join(format!("lucinanet-partition-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    unsafe { std::env::set_var("LUCINANET_JS_FN_CACHE_DIR", &dir) };

    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    let raw = mozjs::rust::Runtime::get().unwrap().as_ptr();
    unsafe {
        let new_global = |context: &mut mozjs::context::JSContext| {
            JS_NewGlobalObject(
                context,
                &SIMPLE_GLOBAL_CLASS,
                ptr::null_mut(),
                OnNewGlobalHookOption::FireOnNewGlobalHook,
                &*RealmOptions::default(),
            )
        };
        rooted!(&in(context) let a = new_global(context));
        rooted!(&in(context) let b = new_global(context));
        rooted!(&in(context) let c = new_global(context));
        lucinanet_js_set_cache_partition(raw, a.get(), b"https://a.test".as_ptr(), 14);
        lucinanet_js_set_cache_partition(raw, b.get(), b"https://b.test".as_ptr(), 14);
        // `c` gets no partition.

        let mut run = |global: mozjs::gc::HandleObject, src: &str| -> i32 {
            let mut realm = AutoRealm::new_from_handle(context, global);
            let context = &mut realm;
            rooted!(&in(context) let mut rval = UndefinedValue());
            let options = CompileOptionsWrapper::new(context, c"partition.js".to_owned(), 1);
            assert!(evaluate_script(context, global, src, rval.handle_mut(), options).is_ok(), "{src} threw");
            rval.get().to_int32()
        };

        assert_eq!(run(a.handle(), FIRST), 10);
        let (lookups, hits, _) = cache_counters(raw);
        assert!(lookups > 0);
        assert_eq!(hits, 0);

        // Another site: the same structure does not hit.
        assert_eq!(run(b.handle(), RENAMED), 10);
        assert_eq!(cache_counters(raw).1, 0);

        // The same site does.
        assert_eq!(run(a.handle(), RENAMED), 10);
        assert!(cache_counters(raw).1 >= 1);

        // A global without a partition does not use the cache at all.
        let before = cache_counters(raw);
        assert_eq!(run(c.handle(), RENAMED), 10);
        assert_eq!(cache_counters(raw), before);
    }
    drop(runtime);
    let _ = std::fs::remove_dir_all(&dir);
}
