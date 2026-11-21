/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Structured clone and nesting depth. The clone writer and reader of
// mozjs-sys/src/engine/clone.rs recurse once per nesting level of the value, and a page builds any depth
// with a loop, so an unbounded depth is an abort of the whole process (`fatal runtime error: stack
// overflow`, SIGABRT) reachable from structuredClone / postMessage / history state. The guard is the
// context's native stack limit, which is what bounds a recursive walk: `too_deep` compares the address of
// a local against `embed::native_stack_limit(cx)` plus a red zone and refuses through the clone error
// path. `MAX_CLONE_DEPTH` (128) is only the fallback for a context with no quota, where there is no
// address to compare with. Both are tested, and separately:
//
//   * noquota  `JS_SetNativeStackQuota(cx, 0, 0, 0)`, so `native_stack_limit` is 0 and the 128 fallback is
//     the cap. Every kind gets a record at 128 (reads), at 129 (refused) and at 100,000 (refused), so each
//     `descend`/`write_value` arm is the one that refuses. The crafted Error and typed-array records are
//     the only ones that reach the `TAG_ERROR` and `TAG_TYPEDARRAY` arms as the deepest level: without
//     those `descend` calls the suite would stay green.
//   * quota    the clone runs on a thread with a 2 MiB stack (a Servo dedicated/shared worker's, the
//     smallest stack this code gets) and `JS_SetNativeStackQuota(cx, QUOTA_BYTES, ...)` is called near the
//     top of it. A value deeper than the fallback cap then clones, and 100,000 levels are refused by the
//     stack guard, well above the end of that thread.
//
// Every case runs in a CHILD process: the test re-invokes its own binary on the `child` test below with the
// case in CLONE_DEPTH_CASE, and the parent asserts that the child exited 0 and printed the result it
// expects. On code without the guard the child dies with signal 6 and the parent test FAILS cleanly instead
// of taking the test runner down with it.
//
// A level is one object (an object, an array, a Map, a Set, an Error with a cause or a typed array) nested
// in the one above; the root is level 1. `MAX_DEPTH` is the fallback cap of clone.rs (`MAX_CLONE_DEPTH`).
//
// The writer and the reader are tested separately. The writer through a round trip of a value built in
// JS; the reader through a record built here byte for byte (the format of clone.rs: header word, then
// tagged records), which reaches a depth the writer would already have refused.

use std::process::Command;
use std::ptr;

use mozjs::context::JSContext;
use mozjs::jsapi::{self, JS, OnNewGlobalHookOption};
use mozjs::jsval::UndefinedValue;
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{
    evaluate_script, CompileOptionsWrapper, HandleObject, JSEngine, MutableHandleValue, RealmOptions, Runtime,
    SIMPLE_GLOBAL_CLASS,
};

/// The fallback cap of `mozjs-sys/src/engine/clone.rs` (`MAX_CLONE_DEPTH`), in force when the context has no
/// native stack quota.
const MAX_DEPTH: usize = 128;

/// A depth no page needs and every page can build.
const HUGE: usize = 100_000;

/// The stack of the thread the `quota` cases clone on: Rust's default, and what Servo's dedicated and shared
/// workers get (`thread::Builder::new()` with no `stack_size`).
const QUOTA_STACK: usize = 2 * 1024 * 1024;

/// The quota those cases install, measured from near the top of that thread. The guard then refuses about 512
/// KiB above the end of the stack, so a refusal never runs off the end; the walk gets `QUOTA_BYTES` minus the
/// red zone of clone.rs (64 KiB), i.e. ~1.4 MiB.
const QUOTA_BYTES: usize = 1_536 * 1024;

/// A depth over `MAX_DEPTH` that the guard, and not the fallback cap, decides. Measured on that thread in a debug
/// build (the largest frames), one clone level costs up to 4,976 bytes (the reader of an `Error` chain; an object
/// chain costs 1,952 writing and 3,888 reading), so `UNDER_QUOTA` levels is ~1.0 MiB of the ~1.4 MiB the guard
/// leaves, and the refusal of a depth no stack holds comes years later.
const UNDER_QUOTA: usize = 200;

const KINDS: [&str; 6] = ["obj", "arr", "map", "set", "err", "mixed"];

/// The kinds the child builds a record for byte by byte. `err` and `ta` are the two whose only path to the
/// cap is their own `descend` arm: an `Error`'s cause and a typed array's buffer.
const CRAFTED_KINDS: [&str; 7] = ["obj", "arr", "map", "set", "mixed", "err", "ta"];

// ---- The child ----

fn eval(cx: &mut JSContext, global: HandleObject, src: &str, rval: MutableHandleValue) {
    let options = CompileOptionsWrapper::new(cx, c"brimstone_clone_depth".to_owned(), 1);
    assert!(evaluate_script(cx, global, src, rval, options).is_ok(), "script failed: {src}");
}

fn check(cx: &mut JSContext, global: HandleObject, src: &str) -> bool {
    rooted!(&in(cx) let mut rval = UndefinedValue());
    eval(cx, global, src, rval.handle_mut());
    rval.is_boolean() && rval.to_boolean()
}

/// The JS helpers: build a chain of `n` levels of `kind` (iteratively), and walk a chain checking it.
const HELPERS: &str = r#"
var __mixed = ['obj', 'arr', 'map', 'set'];
function __kindAt(kind, i) { return kind === 'mixed' ? __mixed[i % 4] : kind; }
function __mk(k) {
  switch (k) { case 'obj': return {}; case 'arr': return []; case 'map': return new Map();
               case 'set': return new Set(); case 'err': return new Error('e'); }
}
function __link(p, c, k) {
  switch (k) { case 'obj': p.c = c; break; case 'arr': p.push(c); break; case 'map': p.set('k', c); break;
               case 'set': p.add(c); break; case 'err': p.cause = c; break; }
}
function __next(p, k) {
  switch (k) { case 'obj': return p.c; case 'arr': return p[0]; case 'map': return p.get('k');
               case 'set': return p.values().next().value; case 'err': return p.cause; }
}
function __is(p, k) {
  switch (k) {
    case 'obj': return p !== null && typeof p === 'object' && Object.getPrototypeOf(p) === Object.prototype;
    case 'arr': return Array.isArray(p) && p.length === 1;
    case 'map': return p instanceof Map && p.size === 1;
    case 'set': return p instanceof Set && p.size === 1;
    case 'err': return p instanceof Error;
    case 'ta': return p instanceof Uint8Array && p.byteLength === 0;
  }
}
function __build(kind, n) {
  var root = __mk(__kindAt(kind, 0)), cur = root;
  for (var i = 1; i < n; i++) { var x = __mk(__kindAt(kind, i)); __link(cur, x, __kindAt(kind, i - 1)); cur = x; }
  __link(cur, 'leaf', __kindAt(kind, n - 1));
  return root;
}
function __verifyTa(root, n) {
  var cur = root;
  for (var i = 0; i < n - 1; i++) {
    if (!__is(cur, 'obj')) { return false; }
    cur = __next(cur, 'obj');
  }
  return __is(cur, 'ta');
}
function __verify(root, kind, n) {
  if (kind === 'ta') { return __verifyTa(root, n); }
  var cur = root;
  for (var i = 0; i < n; i++) {
    if (!__is(cur, __kindAt(kind, i))) { return false; }
    cur = __next(cur, __kindAt(kind, i));
  }
  return cur === 'leaf';
}
"#;

unsafe fn set_global_val(cx: &mut JSContext, global: HandleObject, name: &std::ffi::CStr, v: mozjs::rust::Handle<jsapi::Value>) {
    assert!(jsapi::JS_SetProperty(cx.raw_cx(), global.into(), name.as_ptr(), v.into()));
}

/// Takes the pending exception, if any, into the global `__e`; true when it is the depth refusal.
unsafe fn take_refusal(cx: &mut JSContext, global: HandleObject) -> bool {
    let raw = cx.raw_cx();
    if !jsapi::JS_IsExceptionPending(raw) {
        return false;
    }
    rooted!(&in(cx) let mut e = UndefinedValue());
    if !jsapi::JS_GetPendingException(raw, e.handle_mut().into()) {
        return false;
    }
    jsapi::JS_ClearPendingException(raw);
    unsafe { set_global_val(cx, global, c"__e", e.handle()) };
    check(cx, global, "__e instanceof Error && __e.message.indexOf('nested too deeply') >= 0")
}

/// The header word of clone.rs: magic "LCNE" in the high half, format version 1 in the low half.
fn header() -> Vec<u8> {
    ((0x4C43_4E45u64 << 32) | 1).to_le_bytes().to_vec()
}

fn tag(buf: &mut Vec<u8>, t: u32) {
    buf.extend_from_slice(&t.to_le_bytes());
}

fn u8(buf: &mut Vec<u8>, b: u8) {
    buf.push(b);
}

fn u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

/// A string's code units with no tag and no length prefix, as `write_string_units` writes them (clone.rs):
/// a `u32` count then the units. An `Error` record's name and message use this, not a string record.
fn units(buf: &mut Vec<u8>, s: &str) {
    tag(buf, s.len() as u32);
    for c in s.bytes() {
        buf.extend_from_slice(&(c as u16).to_le_bytes());
    }
}

/// A string record: `TAG_STRING`, then its units.
fn string(buf: &mut Vec<u8>, s: &str) {
    tag(buf, 6);
    units(buf, s);
}

/// A record of `n` levels of `kind` ('obj', 'arr', 'map', 'set', 'mixed', 'err' or 'ta') ending in the string
/// "leaf" (for 'ta', in the typed array itself).
fn crafted(kind: &str, n: usize) -> Vec<u8> {
    let mut buf = header();
    let mut suffixes: Vec<bool> = Vec::new();
    for i in 0..n {
        let k = if kind == "mixed" { ["obj", "arr", "map", "set"][i % 4] } else { kind };
        if k == "ta" {
            // The last level of a 'ta' chain is the typed array; the levels above it are plain objects.
            if i + 1 != n {
                tag(&mut buf, 15);
                tag(&mut buf, 1);
                tag(&mut buf, 1);
                buf.extend_from_slice(&(b'c' as u16).to_le_bytes());
                suffixes.push(false);
                continue;
            }
            // `write_typed_array`: TAG_TYPEDARRAY, the element kind, the viewed buffer as a value record,
            // then the byte offset and the byte length. The reader descends for the typed array and reads the
            // buffer through `read_value`, whose `TAG_ARRAYBUFFER` arm does not descend: the typed array is
            // the level that meets the cap.
            tag(&mut buf, 20);
            u8(&mut buf, 1); // Uint8
            tag(&mut buf, 18); // TAG_ARRAYBUFFER
            u64(&mut buf, 0); // its byte length, so no bytes follow
            u64(&mut buf, 0); // byte offset
            u64(&mut buf, 0); // byte length
            continue;
        }
        match k {
            "obj" => {
                tag(&mut buf, 15);
                tag(&mut buf, 1);
                tag(&mut buf, 1);
                buf.extend_from_slice(&(b'c' as u16).to_le_bytes());
                suffixes.push(false);
            },
            "arr" => {
                tag(&mut buf, 14);
                u64(&mut buf, 1);
                u8(&mut buf, 1);
                suffixes.push(true);
            },
            "map" => {
                tag(&mut buf, 16);
                tag(&mut buf, 1);
                string(&mut buf, "k");
                suffixes.push(false);
            },
            "set" => {
                tag(&mut buf, 17);
                tag(&mut buf, 1);
                suffixes.push(false);
            },
            // `write_error`: TAG_ERROR, then the name and the message as bare unit runs, a byte for a
            // present stack, a byte for a present cause, and the cause as a value record. An `Error`'s cause
            // is its only nested value, so this chain is the only thing that reaches the `TAG_ERROR` arm of
            // `read_value` at the cap.
            "err" => {
                tag(&mut buf, 22);
                units(&mut buf, "Error");
                units(&mut buf, "e");
                u8(&mut buf, 0); // no stack
                u8(&mut buf, 1); // a cause follows
                suffixes.push(false);
            },
            other => panic!("no crafted kind {other}"),
        }
    }
    if kind != "ta" {
        string(&mut buf, "leaf");
    }
    for arr in suffixes.into_iter().rev() {
        if arr {
            tag(&mut buf, 0); // an array's count of extra named properties
        }
    }
    buf
}

/// One case, on the thread that runs it. `quota` is what all three categories of
/// `JS_SetNativeStackQuota` get, straight after the runtime is made (so it is measured from near the top of
/// this thread's stack): 0 leaves the context with no native limit, and clone.rs then falls back to
/// `MAX_DEPTH`; `QUOTA_BYTES` is the guard case.
unsafe fn child_body(mode: &str, kind: &str, depth: usize, quota: usize, context: &mut JSContext) {
    jsapi::JS_SetNativeStackQuota(context.raw_cx(), quota, quota, quota);
    // SAFETY: a fresh runtime for this process's single `JSEngine`, a global made from it and its realm
    // entered before any JSAPI call below.
    unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, global.handle());
        let cx = &mut realm;
        rooted!(&in(cx) let mut ignore = UndefinedValue());
        eval(cx, global.handle(), HELPERS, ignore.handle_mut());
        let raw = cx.raw_cx();
        let scope = JS::StructuredCloneScope::SameProcess;
        let callbacks = ptr::null();
        let buf = mozjs::glue::NewJSAutoStructuredCloneBuffer(scope, callbacks);
        assert!(!buf.is_null());
        let data = &mut (*buf).data_;
        let version = (*buf).version_;
        rooted!(&in(cx) let undef = UndefinedValue());
        if mode == "roundtrip" || mode == "write" {
            rooted!(&in(cx) let mut v = UndefinedValue());
            eval(cx, global.handle(), &format!("__build({kind:?}, {depth})"), v.handle_mut());
            let ok = jsapi::JS_WriteStructuredClone(raw, v.handle().into(), data, scope, ptr::null(), callbacks, ptr::null_mut(), undef.handle().into());
            if !ok {
                let refusal = take_refusal(cx, global.handle());
                println!("RESULT write=err refusal={refusal}");
                mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf);
                return;
            }
            if mode == "write" {
                println!("RESULT write=ok");
                mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf);
                return;
            }
        } else {
            let bytes = crafted(kind, depth);
            assert!(mozjs::glue::WriteBytesToJSStructuredCloneData(bytes.as_ptr(), bytes.len(), data));
        }
        rooted!(&in(cx) let mut out = UndefinedValue());
        let ok = jsapi::JS_ReadStructuredClone(raw, data, version, scope, out.handle_mut().into(), ptr::null(), callbacks, ptr::null_mut());
        if !ok {
            let refusal = take_refusal(cx, global.handle());
            println!("RESULT write=ok read=err refusal={refusal}");
            mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf);
            return;
        }
        set_global_val(cx, global.handle(), c"__c", out.handle());
        let verified = check(cx, global.handle(), &format!("__verify(__c, {kind:?}, {depth})"));
        println!("RESULT write=ok read=ok verified={verified}");
        mozjs::glue::DeleteJSAutoStructuredCloneBuffer(buf);
    }
}

/// The child: runs `CLONE_DEPTH_CASE` = "<mode>:<kind>:<depth>:<stack>" and prints one RESULT line. A process
/// that dies instead of printing is the failure the parent looks for.
#[test]
fn child() {
    let Ok(case) = std::env::var("CLONE_DEPTH_CASE") else {
        return; // run as an ordinary test: nothing to do
    };
    let parts: Vec<&str> = case.split(':').collect();
    let (mode, kind, depth, stack) =
        (parts[0].to_string(), parts[1].to_string(), parts[2].parse::<usize>().unwrap(), parts[3].to_string());
    match stack.as_str() {
        "noquota" => {
            let engine = JSEngine::init().unwrap();
            let mut runtime = Runtime::new(engine.handle());
            let cx = runtime.cx();
            unsafe { child_body(&mode, &kind, depth, 0, cx) };
        },
        // The engine and the runtime are made on the 2 MiB thread itself: `Runtime::new` sets its quota from
        // the stack of the thread that calls it, and the case is about that thread's limit.
        "quota" => {
            let (mode, kind) = (mode.clone(), kind.clone());
            let child = std::thread::Builder::new()
                .stack_size(QUOTA_STACK)
                .spawn(move || unsafe {
                    let engine = JSEngine::init().unwrap();
                    let mut runtime = Runtime::new(engine.handle());
                    let cx = runtime.cx();
                    child_body(&mode, &kind, depth, QUOTA_BYTES, cx);
                })
                .expect("spawn the 2 MiB thread");
            child.join().expect("the quota thread panicked");
        },
        other => panic!("no stack mode {other}"),
    }
}

// ---- The parent ----

/// Runs the case in a child process of this very binary; returns (exited 0, stdout, status text).
fn run_case(mode: &str, kind: &str, depth: usize, stack: &str) -> (bool, String, String) {
    let exe = std::env::current_exe().expect("current_exe");
    let out = Command::new(exe)
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env("CLONE_DEPTH_CASE", format!("{mode}:{kind}:{depth}:{stack}"))
        .output()
        .expect("spawn child");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let status = format!("{:?}; stderr tail: {}", out.status, stderr.lines().rev().take(3).collect::<Vec<_>>().join(" | "));
    (out.status.success(), stdout, status)
}

fn assert_result(mode: &str, kind: &str, depth: usize, stack: &str, want: &str) {
    let (ok, stdout, status) = run_case(mode, kind, depth, stack);
    assert!(ok, "{mode} {kind} depth {depth} {stack}: the child process died ({status})");
    assert!(
        stdout.lines().any(|l| l.ends_with(want)),
        "{mode} {kind} depth {depth} {stack}: wanted `{want}`, child printed:\n{stdout}"
    );
}

const ROUND_TRIPS: &str = "RESULT write=ok read=ok verified=true";
const WRITER_REFUSES: &str = "RESULT write=err refusal=true";
const READER_REFUSES: &str = "RESULT write=ok read=err refusal=true";

#[test]
fn the_writer_and_reader_round_trip_a_value_of_exactly_the_cap() {
    for kind in KINDS {
        assert_result("roundtrip", kind, MAX_DEPTH, "noquota", ROUND_TRIPS);
    }
}

#[test]
fn a_shallow_value_round_trips() {
    for kind in KINDS {
        assert_result("roundtrip", kind, 10, "noquota", ROUND_TRIPS);
    }
}

#[test]
fn the_writer_refuses_the_cap_plus_one_levels_and_the_process_survives() {
    for kind in KINDS {
        assert_result("roundtrip", kind, MAX_DEPTH + 1, "noquota", WRITER_REFUSES);
    }
}

#[test]
fn the_writer_refuses_a_hundred_thousand_levels_and_the_process_survives() {
    for kind in KINDS {
        assert_result("roundtrip", kind, HUGE, "noquota", WRITER_REFUSES);
    }
}

#[test]
fn the_reader_reads_a_record_of_exactly_the_cap() {
    for kind in CRAFTED_KINDS {
        assert_result("crafted", kind, MAX_DEPTH, "noquota", ROUND_TRIPS);
    }
}

#[test]
fn the_reader_refuses_a_record_of_the_cap_plus_one_levels_and_the_process_survives() {
    for kind in CRAFTED_KINDS {
        assert_result("crafted", kind, MAX_DEPTH + 1, "noquota", READER_REFUSES);
    }
}

#[test]
fn the_reader_refuses_a_record_of_a_hundred_thousand_levels_and_the_process_survives() {
    for kind in CRAFTED_KINDS {
        assert_result("crafted", kind, HUGE, "noquota", READER_REFUSES);
    }
}

/// The `TAG_ERROR` arm: an `Error` record's cause is its only nested value, so a chain of these is refused by
/// that arm alone. Clone.rs without `r.descend` on `TAG_ERROR` reads the 129-level record here and says so.
#[test]
fn an_error_record_is_what_the_reader_refuses_at_the_cap_and_past_it() {
    assert_result("crafted", "err", MAX_DEPTH, "noquota", ROUND_TRIPS);
    assert_result("crafted", "err", MAX_DEPTH + 1, "noquota", READER_REFUSES);
    assert_result("crafted", "err", HUGE, "noquota", READER_REFUSES);
}

/// The `TAG_TYPEDARRAY` arm, and the reader's `TAG_ARRAYBUFFER` arm, which does not descend at all: the typed
/// array itself is the level over the cap here.
#[test]
fn a_typed_array_record_is_what_the_reader_refuses_at_the_cap_and_past_it() {
    assert_result("crafted", "ta", MAX_DEPTH, "noquota", ROUND_TRIPS);
    assert_result("crafted", "ta", MAX_DEPTH + 1, "noquota", READER_REFUSES);
    assert_result("crafted", "ta", HUGE, "noquota", READER_REFUSES);
}

/// With a quota set the cap is the stack, not 128: a value deeper than the fallback clones on a 2 MiB thread.
#[test]
fn a_quota_lets_a_value_deeper_than_the_fallback_cap_clone() {
    for kind in KINDS {
        assert_result("roundtrip", kind, UNDER_QUOTA, "quota", ROUND_TRIPS);
    }
    for kind in CRAFTED_KINDS {
        assert_result("crafted", kind, UNDER_QUOTA, "quota", ROUND_TRIPS);
    }
}

/// And the same guard, not the counter, is what refuses a depth no thread holds.
#[test]
fn a_quota_refuses_a_hundred_thousand_levels_from_the_stack_and_the_process_survives() {
    for kind in KINDS {
        assert_result("roundtrip", kind, HUGE, "quota", WRITER_REFUSES);
    }
    for kind in ["obj", "err", "ta", "mixed"] {
        assert_result("crafted", kind, HUGE, "quota", READER_REFUSES);
    }
}
