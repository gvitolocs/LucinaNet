/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The names of the function cache's environment variables are in a build only with the `env-config` feature: a normal build has
//! no way to read the directory of the function cache from the environment. The test scans its own binary, which links the engine.

use std::hint::black_box;

#[test]
fn environment_names_are_in_the_binary_only_with_env_config() {
    // Pull the engine's context creation (which configures the cache) into this binary.
    black_box(mozjs_sys::engine::cx::JS_NewContext as usize);
    let bytes = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    // The name written backwards, so this test's own constants do not contain it.
    let needle: String = "EHCAC_NF_SJ_TENANICUL".chars().rev().collect();
    let found = bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
    assert_eq!(found, cfg!(feature = "env-config"), "names present in the binary: {found}");
}
