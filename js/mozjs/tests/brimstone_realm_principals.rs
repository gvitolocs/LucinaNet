//! A global created with principals has them: Servo passes the origin's principals to JS_NewGlobalObject and calls
//! SetRealmPrincipals only once the window has a document, and a cross-realm same-origin check in between (an iframe's initial
//! global) reads the realm's principals (the BBC crash: it read null).
use std::ptr;

use mozjs::glue::{CreateRustJSPrincipals, JSPrincipalsCallbacks};
use mozjs::jsapi::{GetObjectRealmOrNull, GetRealmPrincipals, OnNewGlobalHookOption};
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};

#[test]
fn a_global_has_the_principals_it_was_created_with() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let context = runtime.cx();
    unsafe {
        let callbacks: JSPrincipalsCallbacks = std::mem::zeroed();
        let principals = CreateRustJSPrincipals(&callbacks, ptr::null_mut());
        assert!(!principals.is_null());
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            principals,
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        assert!(!global.get().is_null());
        let realm = GetObjectRealmOrNull(global.get());
        assert!(!realm.is_null());
        assert_eq!(GetRealmPrincipals(realm), principals, "the realm has no principals");

        // A global created without principals has none.
        rooted!(&in(context) let plain = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        assert!(GetRealmPrincipals(GetObjectRealmOrNull(plain.get())).is_null());
    }
}
