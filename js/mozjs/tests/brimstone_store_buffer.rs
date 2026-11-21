/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// The post-write barriers' store buffer (mozjs_sys engine/gc.rs): a `Heap<*mut JSObject>` field reached by no trace hook
// follows its object when the copying GC moves it, as Servo's callback objects need (the object is kept alive by a raw
// value root on a different field, the way script_bindings' CallbackObject does).

use std::ptr;

use mozjs::jsapi::{Heap, JSObject, JS_GetProperty, OnNewGlobalHookOption, JS};
use mozjs::jsval::{Int32Value, ObjectValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{evaluate_script, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};

#[test]
fn heap_fields_follow_moved_objects() {
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
        let raw = context.raw_cx();

        // A fresh ordinary object { v: 41 }, held in an untraced Heap field and in a raw value root on another field.
        rooted!(&in(context) let mut rval = UndefinedValue());
        let options = CompileOptionsWrapper::new(context, c"sb.js".to_owned(), 1);
        assert!(evaluate_script(context, global.handle(), "({ v: 41 })", rval.handle_mut(), options).is_ok());
        let field: Box<Heap<*mut JSObject>> = Heap::boxed(rval.get().to_object());
        let root: Box<Heap<JS::Value>> = Heap::boxed(ObjectValue(rval.get().to_object()));
        assert!(mozjs::jsapi::js::AddRawValueRoot(raw, root.get_unsafe(), c"sb-root".as_ptr()));
        rval.set(UndefinedValue());
        let before = field.get();

        // Allocate enough to need collections, then collect: the object moves.
        for _ in 0..3 {
            rooted!(&in(context) let mut junk = UndefinedValue());
            let options = CompileOptionsWrapper::new(context, c"sb.js".to_owned(), 1);
            assert!(evaluate_script(context, global.handle(), "var a = []; for (let i = 0; i < 20000; i++) a.push({ i }); a.length", junk.handle_mut(), options).is_ok());
            mozjs::jsapi::JS_GC(raw, JS::GCReason::API);
        }

        let after = field.get();
        assert_eq!(after, root.get().to_object(), "the Heap field must hold the root's (moved) object");
        assert_ne!(after, before, "the copying GC moved the object");
        rooted!(&in(context) let obj = after);
        rooted!(&in(context) let mut v = UndefinedValue());
        assert!(JS_GetProperty(raw, obj.handle().into(), c"v".as_ptr(), v.handle_mut().into()));
        assert_eq!(v.get(), Int32Value(41));

        let (cells, _) = mozjs_sys::engine::gc::store_buffer_len();
        assert!(cells >= 1);
        mozjs::jsapi::js::RemoveRawValueRoot(raw, root.get_unsafe());
        drop(field);
        drop(root);
        let (cells_after, _) = mozjs_sys::engine::gc::store_buffer_len();
        assert!(cells_after < cells, "dropping a Heap removes its slot from the buffer");
    }
}

/// A runtime owned by a thread-local is dropped during thread teardown; a GC then must not touch the store buffer's
/// thread-locals if they were destroyed first (glibc runs thread-local destructors in reverse registration order, so the
/// buffer, first used after the runtime was stored, goes first). Before the fix this aborted the process.
#[test]
fn gc_during_thread_teardown_after_the_store_buffer_is_gone() {
    struct GcOnDrop(Option<Runtime>);
    impl Drop for GcOnDrop {
        fn drop(&mut self) {
            if let Some(runtime) = self.0.as_ref() {
                unsafe { mozjs::jsapi::JS_GC(runtime.cx_no_gc().raw_cx_no_gc(), JS::GCReason::API) };
            }
        }
    }
    thread_local! {
        static OWNER: std::cell::RefCell<GcOnDrop> = const { std::cell::RefCell::new(GcOnDrop(None)) };
    }

    let engine = JSEngine::init().unwrap();
    let handle = engine.handle();
    std::thread::spawn(move || {
        OWNER.with(|owner| owner.borrow_mut().0 = Some(Runtime::new(handle)));
        OWNER.with(|owner| {
            let mut owner = owner.borrow_mut();
            let runtime = owner.0.as_mut().unwrap();
            let context = runtime.cx();
            unsafe {
                rooted!(&in(context) let global = JS_NewGlobalObject(
                    context,
                    &SIMPLE_GLOBAL_CLASS,
                    ptr::null_mut(),
                    OnNewGlobalHookOption::FireOnNewGlobalHook,
                    &*RealmOptions::default(),
                ));
                // First use of the store buffer: a Heap field pointing at an object, recorded and kept past the thread's
                // end (leaked, so its Drop never removes it).
                let field: &'static mut Heap<*mut JSObject> = Box::leak(Heap::boxed(global.get()));
                assert!(!field.get().is_null());
            }
        });
    })
    .join()
    .expect("the thread ends without a panic or abort");
}
