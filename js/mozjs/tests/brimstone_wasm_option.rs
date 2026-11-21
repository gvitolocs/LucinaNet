/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// `JS::ContextOptions::wasm` in the glue (WebAssembly design §2.1, slice H1): the option is the one source of truth for the
// engine's WebAssembly namespace, and the glue copies it at both points Servo can still have written it — when the context is
// made, and when a realm is about to exist.
//
// mozjs allows one `JSEngine` per process, so `.cargo/config.toml` runs each test on its own.

use std::ptr;

use mozjs::jsapi::OnNewGlobalHookOption;
use mozjs::rooted;
use mozjs::rust::wrappers2::{ContextOptionsRef, JS_NewGlobalObject};
use mozjs::rust::{JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};
use mozjs_sys::embed::wasm_enabled;
use mozjs_sys::engine::cx::Cx;

#[test]
fn the_wasm_option_decides_the_engines_webassembly_switch() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    // SAFETY: `context` is the live `JSContext` of this thread's runtime, and a glue context is a `Cx` (`engine/cx.rs`).
    let bs = unsafe { (*context.raw_cx().cast::<Cx>()).bs };

    // The engine's own default is on; the glue's answer to a realm built without an option is the option's, which is off.
    assert!(!wasm_enabled(bs), "WebAssembly was enabled before anyone asked for it");

    unsafe {
        let options = ContextOptionsRef(context);
        assert!(!(*options).wasm_(), "the option read as set when it had never been written");
        // Servo writes the option after `JS_NewContext` returns and before its first global; the next realm takes that value.
        (*options).set_wasm_(true);
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        assert!(wasm_enabled(bs), "the realm did not take the option");

        // The switch follows the option, not the realm: a second global made with the option off is enabled off again.
        (*options).set_wasm_(false);
        rooted!(&in(context) let second = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        assert!(!wasm_enabled(bs), "the second realm kept the first one's value");
        assert!(!global.get().is_null() && !second.get().is_null());
    }
}
