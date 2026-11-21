/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `JS::EnvironmentChain` (mozjs_sys `jsglue.cpp` `NewEnvironmentChain`, `AppendToEnvironmentChain`,
//! `DeleteEnvironmentChain`) and `JS::CompileFunction` (`js/src/vm/CompilationAndEvaluation.cpp`): what Servo compiles
//! inline event handlers with (`EventTarget::get_compiled_event_handler`).
//!
//! **Scope order.** Servo appends the document, then the form owner, then the element, and the HTML spec looks the element
//! up first ("getting the current value of the event handler": document scope, form owner scope, element scope, each a
//! NewObjectEnvironment around the previous). So here the *first appended object is the outermost* scope just inside the
//! global, and the *last appended is searched first*. (In SpiderMonkey, `CreateObjectsForEnvironmentChain` wraps from the
//! end of the vector, which makes index 0 the innermost, and Gecko appends the element first; we follow the order Servo's
//! caller and the spec need.)
//!
//! **Rooting.** The chain lives on the Rust heap between `Append` and `Compile`, while allocations can collect and the
//! collector moves objects. Each chain registers itself in `Cx::extra_roots` (as `glue__CreateRootedObjectVector` does, the
//! `PersistentRooted` vectors), so `gc::trace_all_roots` reports every object slot by address on each GC, which keeps the
//! objects alive and rewrites the slots when they move. `DeleteEnvironmentChain` unregisters it before freeing. Compile
//! reads each slot only after the call that could have moved it.
//!
//! **Compilation.** No change to the engine: one script, compiled with the caller's file and line, of the shape
//! `(function(){with(this)return function(){with(this)return function(ARGS){BODY\n}}})` (one `with` level per chain
//! object), whose outer functions are then called with each chain object, outermost first, as `this`. `this` is not a name
//! lookup, so no wrapper binding is visible to the handler and no scope object can shadow one; the wrappers' own `arguments`
//! is always shadowed by the handler's. The handler is an anonymous function expression whose `name` is then defined, so
//! its name is not a binding inside it (SpiderMonkey compiles a function statement, the HTML spec's handler has no self
//! binding). Before the wrapper is compiled, the parameters and the body are each parsed alone by the engine's
//! `Function`-constructor parsers (`FormalParameters`, `FunctionBody`), as CreateDynamicFunction does, so text such as
//! `}); evil(); ({` is a SyntaxError and never code outside the handler. The body starts on the options' line (the wrapper
//! prefix shares its first line), so reported line numbers are the attribute's; columns on that first line are off by the
//! prefix length (the options' column is not used). The function is sloppy unless its body says `"use strict"`.
//!
//! **Known gap.** `SupportUnscopables::No` asks for `@@unscopables` to be ignored; the engine's `with` always honours it,
//! so `No` behaves as `Yes`. Servo passes `Yes`.

use ::std::ffi::c_void;
use ::std::os::raw::{c_char, c_uint};
use ::std::rc::Rc;

use brimstone_core::common::wtf_8::Wtf8String;
use brimstone_core::parser::parser::{
    parse_function_body_for_function_constructor, parse_function_params_for_function_constructor,
};
use brimstone_core::parser::source::Source;
use brimstone_core::parser::{LocalizedParseError, ParseContext};
use brimstone_core::runtime::embed::{self, api};
use brimstone_core::runtime::error;
use brimstone_core::runtime::gc::HandleScope;
use brimstone_core::runtime::{Context as Bs, EvalResult, Handle, PropertyFlags, Value as BsValue};

use super::cx::cx_of;
use super::realm::object_ptr;
use super::util::{c_str, fail};
use crate::jsapi::*;

/// What `JS::EnvironmentChain*` points at here.
struct EnvChain {
    /// In append order: outermost first.
    objects: Vec<*mut JSObject>,
    /// Recorded only: see the module comment's known gap.
    _support_unscopables: bool,
}

/// Traces a chain's objects; registered in `Cx::extra_roots` for the chain's life.
#[super::poison::engine_shim(gc)]
unsafe extern "C" fn trace_env_chain(trc: *mut JSTracer, data: *mut c_void) {
    let chain = unsafe { &mut *(data as *mut EnvChain) };
    for p in chain.objects.iter_mut() {
        unsafe { super::gc::trace_cell_slot(trc, p as *mut *mut JSObject as *mut *mut c_void) };
    }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__NewEnvironmentChain(
    cx: *mut JSContext,
    support_unscopables: JS::SupportUnscopables,
) -> *mut JS::EnvironmentChain {
    let chain = Box::into_raw(Box::new(EnvChain {
        objects: Vec::new(),
        _support_unscopables: support_unscopables == JS::SupportUnscopables::Yes,
    }));
    unsafe { cx_of(cx) }.extra_roots.push((Some(trace_env_chain), chain as *mut c_void));
    chain.cast()
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__AppendToEnvironmentChain(chain: *mut JS::EnvironmentChain, obj: *mut JSObject) -> bool {
    if chain.is_null() || obj.is_null() {
        return false;
    }
    unsafe { (*chain.cast::<EnvChain>()).objects.push(obj) };
    true
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn glue__DeleteEnvironmentChain(chain: *mut JS::EnvironmentChain) {
    if chain.is_null() {
        return;
    }
    let data = chain as *mut c_void;
    let cx = super::cx::current_raw();
    if !cx.is_null() {
        let roots = &mut unsafe { cx_of(cx) }.extra_roots;
        if let Some(i) = roots.iter().position(|&(_, d)| d == data) {
            roots.remove(i);
        }
    }
    unsafe { drop(Box::from_raw(chain.cast::<EnvChain>())) };
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CompileFunction(
    cx: *mut JSContext,
    env_chain: *const JS::EnvironmentChain,
    options: *const JS::ReadOnlyCompileOptions,
    name: *const c_char,
    nargs: c_uint,
    argnames: *const *const c_char,
    src: *mut JS::SourceText<u16>,
) -> *mut JSFunction {
    let units = unsafe { ::std::slice::from_raw_parts((*src).units_, (*src).length_ as usize) };
    let body = String::from_utf16_lossy(units);
    unsafe { compile_function(cx, env_chain, options, name, nargs, argnames, &body) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CompileFunction1(
    cx: *mut JSContext,
    env_chain: *const JS::EnvironmentChain,
    options: *const JS::ReadOnlyCompileOptions,
    name: *const c_char,
    nargs: c_uint,
    argnames: *const *const c_char,
    src: *mut JS::SourceText<mozilla::Utf8Unit>,
) -> *mut JSFunction {
    let bytes = unsafe { ::std::slice::from_raw_parts((*src).units_.cast::<u8>(), (*src).length_ as usize) };
    let body = String::from_utf8_lossy(bytes);
    unsafe { compile_function(cx, env_chain, options, name, nargs, argnames, &body) }
}

#[super::poison::engine_shim]
pub unsafe extern "C" fn JS__CompileFunctionUtf8(
    cx: *mut JSContext,
    env_chain: *const JS::EnvironmentChain,
    options: *const JS::ReadOnlyCompileOptions,
    name: *const c_char,
    nargs: c_uint,
    argnames: *const *const c_char,
    utf8: *const c_char,
    length: usize,
) -> *mut JSFunction {
    let bytes = if utf8.is_null() { &[][..] } else { unsafe { ::std::slice::from_raw_parts(utf8.cast::<u8>(), length) } };
    let body = String::from_utf8_lossy(bytes);
    unsafe { compile_function(cx, env_chain, options, name, nargs, argnames, &body) }
}

unsafe fn compile_function(
    cx: *mut JSContext,
    env_chain: *const JS::EnvironmentChain,
    options: *const JS::ReadOnlyCompileOptions,
    name: *const c_char,
    nargs: c_uint,
    argnames: *const *const c_char,
    body: &str,
) -> *mut JSFunction {
    let bs = unsafe { cx_of(cx) }.bs;
    let at = unsafe { super::dump_failed::at(options) };
    let params: Vec<String> =
        (0..nargs as usize).map(|i| unsafe { c_str(*argnames.add(i)) }.into_owned()).collect();
    let name = if name.is_null() { None } else { Some(unsafe { c_str(name) }.into_owned()) };
    let chain = env_chain.cast::<EnvChain>();
    let depth = if chain.is_null() { 0 } else { unsafe { (*chain).objects.len() } };
    let scope = HandleScope::enter(bs);
    let realm = embed::current_realm(bs);
    let out = match unsafe { build(bs, realm, chain, depth, name.as_deref(), &params, body, &at) } {
        Ok(f) => (*f).as_ptr().cast(),
        Err(e) => {
            unsafe { fail(cx, e) };
            ::std::ptr::null_mut()
        },
    };
    scope.exit();
    out
}

/// A SyntaxError (in the current realm) for a part that does not parse alone.
fn part_error<T>(bs: Bs, e: &LocalizedParseError, file: &str, line: u32) -> EvalResult<T> {
    error::syntax_error(bs, &format!("{} ({file}:{line})", e.to_string_without_name()))
}

/// Checks `text` alone with one of the engine's `Function`-constructor part parsers.
fn check_part(
    bs: Bs,
    text: String,
    file: &str,
    line: u32,
    parse: fn(&ParseContext, Rc<brimstone_core::common::options::Options>, bool, bool) -> brimstone_core::parser::ParseResult<()>,
) -> EvalResult<()> {
    let source = match Source::new_for_eval(file.to_owned(), Wtf8String::from_string(text)) {
        Ok(s) => Rc::new(s),
        Err(e) => return part_error(bs, &e, file, line),
    };
    let pcx = ParseContext::new(source);
    match parse(&pcx, bs.options.clone(), false, false) {
        Ok(()) => Ok(()),
        Err(e) => part_error(bs, &e, file, line),
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn build(
    bs: Bs,
    realm: Handle<brimstone_core::runtime::Realm>,
    chain: *const EnvChain,
    depth: usize,
    name: Option<&str>,
    params: &[String],
    body: &str,
    at: &super::dump_failed::Where,
) -> EvalResult<Handle<api::ObjectValue>> {
    let params = params.join(",");
    let mut text = String::with_capacity(body.len() + params.len() + 40 * depth + 32);
    text.push('(');
    for _ in 0..depth {
        text.push_str("function(){with(this)return ");
    }
    text.push_str("function(");
    text.push_str(&params);
    text.push_str("){");
    text.push_str(body);
    text.push_str("\n}");
    for _ in 0..depth {
        text.push('}');
    }
    text.push(')');

    let r = (|| -> EvalResult<Handle<api::ObjectValue>> {
        // Each part alone, as CreateDynamicFunction checks them: a body or a parameter list that would close the wrapper
        // early does not parse by itself.
        check_part(bs, params.clone(), &at.file, at.line, parse_function_params_for_function_constructor)?;
        check_part(bs, format!("\n{body}\n"), &at.file, at.line, parse_function_body_for_function_constructor)?;

        let mut f = embed::evaluate_script(bs, realm, &text, &at.file, at.line)?;
        for i in 0..depth {
            // Read after the previous call: a GC there may have moved the object, and the chain's slot was updated.
            let obj = unsafe { (&(*chain).objects)[i] };
            let this = BsValue::object(object_ptr(obj)).to_handle(bs);
            f = api::call(bs, f, this, &[])?;
        }
        if !f.is_object() {
            return error::type_error(bs, "function compilation did not produce a function");
        }
        let f = f.as_object();
        if let Some(name) = name {
            let key = api::property_key_from_string(bs, "name")?;
            let value = api::string_from_utf8(bs, name)?;
            let d = api::PropertyDescriptor::data(value.as_value(), PropertyFlags::IS_CONFIGURABLE);
            api::define_property(bs, f, key, d)?;
        }
        Ok(f)
    })();
    if let Err(e) = &r {
        // The wrapper the engine was handed, not the attribute's body: that is what failed to compile.
        unsafe { super::dump_failed::report(bs, &text, at, super::dump_failed::Phase::Compile, *e) };
    }
    r
}
