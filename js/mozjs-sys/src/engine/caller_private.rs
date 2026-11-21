/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `JS_GetScriptedCallerPrivate`, which Servo's `active_script_fetch_info`
//! (`LucinaNet-servo/components/script/event_loop/timers.rs:1122`) calls when `setTimeout` or `setInterval` is handed a
//! *string*: the private value of the script that is running, in which Servo keeps the initiating script's fetch options and
//! base URL (a `Rc<BaseScript>` as a `PrivateValue`, set with `JS::SetScriptPrivate`).
//!
//! **What the C++ does.** The glue declaration (`mozjs-sys/bindgen/gluebindings.rs:1326`, `JS_GetScriptedCallerPrivate(cx,
//! dest)`) carries no doc text, so here is the function it is a wrapper for, `JS::GetScriptedCallerPrivate`, from
//! `vendored mozjs/js/src/jsapi.cpp:2825`:
//!
//! ```text
//! JS::Value JS::GetScriptedCallerPrivate(JSContext* cx) {
//!   NonBuiltinFrameIter iter(cx, cx->realm()->principals());
//!   if (iter.done() || !iter.hasScript()) { return UndefinedValue(); }
//!   return iter.script()->sourceObject()->getPrivate();
//! }
//! ```
//!
//! That is: the innermost frame that is running bytecode; the private of the source object that frame's script was compiled
//! from; `UndefinedValue()` when no scripted frame is running, and also when that script has no private. Since the private
//! lives on the source object, every function compiled from one script's source shares that script's private, whatever frame
//! the code is running in.
//!
//! **What is implemented here.** The engine keeps no private on a compiled script, so `Cx::script_privates` is the table
//! (`engine/script.rs`), keyed by the script's address. What was missing is the other half: which script is running. The
//! fork reports a frame's *file name*, `line`, `column` and function name (`embed::capture_stack`), never its script, and a
//! `CompiledScript` shows an embedder only its realm. So the running script is found in two steps, the same way the engine
//! itself finds it when it cannot:
//!
//! - the file of the innermost scripted frame, taken from `embed::capture_stack` as `glue__DescribeScriptedCaller`
//!   (`engine/bindings.rs:274`) does, skipping native frames (Servo's own hooks and the glue's natives are the engine's
//!   `is_native` frames, which have no source file);
//! - the script that file was compiled from, taken from `Cx::script_files`, which every script compile fills in
//!   (`engine/script.rs`).
//!
//! Because several scripts share a file name (every inline `<script>` of a document names that document, as does every
//! `javascript:` URL), the *identity* of the running script is kept where the glue can see it: `Cx::running_scripts`,
//! innermost last, holds either the script `JS_ExecuteScript` is running or, for the script `JS::Evaluate` compiles and runs
//! in one step, its file (`RunningScript`, pushed by a `Running` guard that pops it again). A frame of that one-step script is
//! answered with `UndefinedValue()` as the C++ answers it, since its script is never handed out and so no one can set a
//! private on it; a frame of the innermost compiled script, whose file matches, is answered by identity. The file name alone
//! is consulted only for a frame the glue can name no other way.
//!
//! **Known differences** from the C++, all of them where the engine gives no script to ask about:
//!
//! - a function of another script that is running inside it (script A calls a function defined in script B, B's private
//!   wanted) is found by file name, so if two scripts with a private were compiled from one file the newest of them answers,
//!   which is the one A's call most likely meant;
//! - an inline event handler compiled by `JS::CompileFunction` (`engine/env_chain.rs`) is given the *caller's* file, so it
//!   answers like the script it was compiled inside. Its own script has no private, so the C++ answers `UndefinedValue()`;
//! - a `JS::Evaluate` of a page's own URL answers `UndefinedValue()` for every frame of that file while it runs, the page
//!   functions it calls included, whose script the C++ would find and whose private it would give;
//! - `JS::HideScriptedCaller` does not hide this: `JS::GetScriptedCallerPrivate` reads the frame iterator, not the
//!   `ScriptedCaller` that the hiding applies to, which the glue's `JS__GetScriptedCallerGlobal` and
//!   `glue__DescribeScriptedCaller` do honour.
//!
//! `eval` and `new Function` are **not** a difference, although the code they run comes from a string and not from a file of
//! its own: the fork names the source of both `<eval>` (`parser/source.rs:71` in Brimstone, from `runtime/eval/eval.rs:70`
//! and `runtime/eval/create_dynamic_function.rs:136`), and a frame's file is that name (`runtime/stack_trace.rs:238-240`,
//! `runtime/source_file.rs:110-111`), so no entry of `Cx::script_files` matches and the answer is `UndefinedValue()` — which
//! is what the C++ answers for an eval script, whose source object carries no private either.

use brimstone_core::runtime::embed;
use brimstone_core::runtime::Context as Bs;

use super::cx::{cx_of, Cx};
use crate::boundary::value_bits;
use crate::jsapi::*;

/// How many frames the search for the innermost scripted frame looks through, as `glue__DescribeScriptedCaller` does. The
/// frame above it is the native that asked, so a real chain of nested natives does not reach this depth.
const CALLER_FRAMES: usize = 16;

#[super::poison::engine_shim(failure = unsafe { (*dest.ptr).asBits_ = value_bits::undefined() })]
pub unsafe extern "C" fn glue__JS_GetScriptedCallerPrivate(cx: *mut JSContext, dest: JS::MutableHandleValue) {
    // Collecting the frames allocates (the names, the line offsets) and so may collect, which moves every script: ask the
    // engine for the caller before any script address is read.
    let file = scripted_caller_file(unsafe { cx_of(cx) }.bs);
    let c = unsafe { cx_of(cx) };
    let script = match (file.as_deref(), c.running_scripts.last()) {
        // `iter.done()`: no scripted frame is running, or none of the last frames has a source file.
        (None, _) => None,
        // The frame belongs to the script `JS::Evaluate` compiled and ran in one step, which no one can give a private.
        (Some(file), Some(r)) if r.script.is_none() && r.eval_file.as_deref() == Some(file) => None,
        (Some(file), Some(r)) => match r.script.filter(|bits| script_file(c, *bits) == Some(file)) {
            // The innermost script the glue ran is compiled from the frame's file: that script, by identity.
            Some(bits) => Some(bits),
            // A function defined in another script is running, or the engine ran the code itself (a module, a job, a
            // promise callback): the file the frame reports is all there is to look the script up by.
            None => c.script_files.iter().rev().find(|e| e.1 == file).map(|e| e.0),
        },
        (Some(_), None) => None,
    };
    let out = script.map_or_else(value_bits::undefined, |bits| private_of(c, bits));
    unsafe { (*dest.ptr).asBits_ = out };
}

/// The file of the innermost scripted frame, `None` when no script is running or the frame has no file.
fn scripted_caller_file(bs: Bs) -> Option<String> {
    let frames = embed::capture_stack(bs, CALLER_FRAMES).ok()?;
    frames.into_iter().find(|f| !f.is_native && f.file_name.is_some()).and_then(|f| f.file_name)
}

/// The file `bits` was compiled from, as `note_script` recorded it.
fn script_file(c: &Cx, bits: u64) -> Option<&str> {
    c.script_files.iter().find(|e| e.0 == bits).map(|e| e.1.as_str())
}

/// The private of the script at `bits`, `UndefinedValue()` bits when it has none.
fn private_of(c: &Cx, bits: u64) -> u64 {
    c.script_privates.iter().find(|e| e.0 == bits).map_or_else(value_bits::undefined, |e| e.1)
}

/// Records which file a compiled script came from, the link between the frame the engine reports and the script Servo
/// holds. `script` is the address the compile returned; a collection moves it and rewrites this entry with it, as it does
/// `Cx::script_privates`.
pub(super) unsafe fn note_script(script: *mut JSScript, file: &str) {
    if script.is_null() {
        return;
    }
    let c = unsafe { super::cx::current() };
    let bits = script as usize as u64;
    match c.script_files.iter_mut().find(|e| e.0 == bits) {
        Some(e) => e.1 = file.to_owned(),
        None => c.script_files.push((bits, file.to_owned())),
    }
}

/// What one glue call has running innermost, kept on the context until the `Running` guard of that call is dropped.
#[derive(Debug)]
pub struct RunningScript {
    /// The script `JS_ExecuteScript` was given, `None` for code compiled and run in one step (`JS::Evaluate`), whose
    /// script the fork never hands out and so no one can give a private.
    pub script: Option<u64>,
    /// For that second case, the file the one-step compile was told it was reading: the frame of the script it made shows
    /// the same file, which is how it is told apart from a script of some other file running underneath.
    eval_file: Option<String>,
}

/// The guard that keeps a `RunningScript` on the context: dropping it pops the entry it pushed.
#[must_use]
pub struct Running;

impl Running {
    fn enter(script: Option<u64>, eval_file: Option<String>) -> Running {
        unsafe { super::cx::current() }.running_scripts.push(RunningScript { script, eval_file });
        Running
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        unsafe { super::cx::current() }.running_scripts.pop();
    }
}

/// `script` is the innermost script running until the returned guard is dropped.
pub(super) unsafe fn running(script: *mut JSScript) -> Running {
    Running::enter((!script.is_null()).then(|| script as usize as u64), None)
}

/// Code compiled and run in one step (`JS::Evaluate`) is the innermost running code until the returned guard is dropped.
pub(super) unsafe fn running_eval(file: &str) -> Running {
    Running::enter(None, Some(file.to_owned()))
}
