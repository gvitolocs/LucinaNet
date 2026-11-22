/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// A call to `set_function_cache_config` after a context exists is refused with `ContextExists` and the cache stays off.
#![cfg(not(feature = "env-config"))]

use mozjs::rust::{JSEngine, Runtime};
use mozjs_sys::engine::cx::{set_function_cache_config, FunctionCacheConfig, FunctionCacheConfigError};

#[test]
fn config_after_a_context_exists_is_refused() {
    let engine = JSEngine::init().unwrap();
    let _runtime = Runtime::new(engine.handle());
    let dir = std::env::temp_dir().join(format!("lucinanet-fncache-late-{}", std::process::id()));
    let config = FunctionCacheConfig { dir: dir.clone(), max_mb: 1, enabled: true };
    assert_eq!(set_function_cache_config(config), Err(FunctionCacheConfigError::ContextExists));
    assert!(!dir.exists());
}
