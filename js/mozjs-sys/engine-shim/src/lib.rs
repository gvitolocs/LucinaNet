/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `#[engine_shim]`: the one wrap of every engine `extern "C"` entry point in `mozjs_sys::engine` (docs/edge-wrap.md).
//!
//! The attribute keeps the function's signature, ABI, visibility and attributes as written and replaces only the body:
//!
//! ```ignore
//! #[engine_shim]
//! pub unsafe extern "C" fn JS_GC(cx: *mut JSContext, reason: JS::GCReason) { body }
//! // becomes
//! pub unsafe extern "C" fn JS_GC(cx: *mut JSContext, reason: JS::GCReason) {
//!     static SITE: Site = Site { name: "JS_GC", file: file!(), line: line!() };
//!     crate::engine::poison::run_gated::<(), _, _>(&SITE, <() as ShimFailure>::failure, move || -> () { body })
//! }
//! ```
//!
//! Forms:
//! * `#[engine_shim]`: gated. A poisoned thread gets the failure value without the body running; a panic in the body
//!   poisons the thread's context and returns the failure value (`poison::run_gated`).
//! * `#[engine_shim(failure = EXPR)]`: as above with this failure value instead of the type's `ShimFailure` one.
//! * `#[engine_shim(gc)]`: for code the engine needs while a collection or a barrier is in progress (tracers, barriers):
//!   never gated (a skipped tracer would free live objects under an engine frame that is still running), and a panic is
//!   recorded and then aborts the process on purpose (`poison::run_gc`).
//! * `#[engine_shim(exempt = "<reason>")]`: the function is left as written. Only for the few functions on the coverage
//!   test's `EXEMPT` list (a signal handler, a cross-thread interrupt request, ...); the test refuses any other.
//!
//! The expansion refers to `crate::engine::poison`, so the attribute only works inside `mozjs_sys`.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{Expr, ItemFn, Meta, ReturnType, Token};

enum Mode {
    Gated(Option<Expr>),
    Gc,
    Exempt,
}

fn parse_args(attr: TokenStream) -> syn::Result<Mode> {
    let metas = Punctuated::<Meta, Token![,]>::parse_terminated.parse(attr)?;
    let mut gc = false;
    let mut failure = None;
    let mut exempt = None;
    let n = metas.len();
    for meta in metas {
        match meta {
            Meta::Path(p) if p.is_ident("gc") => gc = true,
            Meta::NameValue(nv) if nv.path.is_ident("failure") => failure = Some(nv.value),
            Meta::NameValue(nv) if nv.path.is_ident("exempt") => match &nv.value {
                Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) if !s.value().trim().is_empty() => exempt = Some(()),
                other => return Err(syn::Error::new_spanned(other, "engine_shim: `exempt = \"<reason>\"` needs a reason")),
            },
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "engine_shim: the arguments are `gc`, `failure = <expr>` or `exempt = \"<reason>\"` (docs/edge-wrap.md)",
                ))
            },
        }
    }
    if exempt.is_some() {
        return if n == 1 {
            Ok(Mode::Exempt)
        } else {
            Err(syn::Error::new(
                Span::call_site(),
                "engine_shim: `exempt` takes no other argument",
            ))
        };
    }
    match (gc, failure) {
        (true, Some(f)) => Err(syn::Error::new_spanned(
            f,
            "engine_shim: `gc` shims have no failure value: a panic aborts",
        )),
        (true, None) => Ok(Mode::Gc),
        (false, f) => Ok(Mode::Gated(f)),
    }
}

fn check_signature(f: &ItemFn) -> syn::Result<()> {
    let sig = &f.sig;
    let abi_ok = sig
        .abi
        .as_ref()
        .and_then(|a| a.name.as_ref())
        .is_some_and(|n| n.value() == "C");
    if !abi_ok {
        return Err(syn::Error::new_spanned(
            &sig.fn_token,
            "engine_shim: only for `extern \"C\"` functions (not C-unwind: docs/edge-wrap.md section a)",
        ));
    }
    if let Some(v) = &sig.variadic {
        return Err(syn::Error::new_spanned(
            v,
            "engine_shim: variadic functions are not supported",
        ));
    }
    if sig.asyncness.is_some() || sig.constness.is_some() {
        return Err(syn::Error::new_spanned(
            &sig.fn_token,
            "engine_shim: not for async or const functions",
        ));
    }
    if !sig.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &sig.generics,
            "engine_shim: not for generic functions",
        ));
    }
    if let Some(syn::FnArg::Receiver(r)) = sig.inputs.first() {
        return Err(syn::Error::new_spanned(r, "engine_shim: not for methods"));
    }
    Ok(())
}

fn expand(mode: Mode, f: ItemFn) -> TokenStream2 {
    let failure = match mode {
        // Named, not suppressed: the function stays as written; the coverage test only allows the names on its list.
        Mode::Exempt => return quote!(#f),
        Mode::Gc => None,
        Mode::Gated(failure) => Some(failure),
    };
    let ItemFn { attrs, vis, sig, block } = f;
    let name = sig.ident.to_string();
    let ret: TokenStream2 = match &sig.output {
        ReturnType::Default => quote!(()),
        ReturnType::Type(_, t) => quote!(#t),
    };
    let site = syn::Ident::new("__LUCINANET_SHIM_SITE", Span::mixed_site());
    let prologue = quote! {
        static #site: crate::engine::poison::Site =
            crate::engine::poison::Site { name: #name, file: ::core::file!(), line: ::core::line!() };
    };
    let body = quote! {
        move || -> #ret {
            #[cfg(feature = "lucinanet-failpoints")]
            crate::engine::poison::failpoint::enter(#site.name);
            #block
        }
    };
    let call = match failure {
        Some(None) => quote! {
            crate::engine::poison::run_gated::<#ret, _, _>(
                &#site,
                <#ret as crate::engine::poison::ShimFailure>::failure,
                #body,
            )
        },
        Some(Some(expr)) => quote! {
            crate::engine::poison::run_gated::<#ret, _, _>(&#site, || -> #ret { #expr }, #body)
        },
        None => quote! {
            crate::engine::poison::run_gc::<#ret, _>(&#site, #body)
        },
    };
    quote! {
        #(#attrs)*
        #vis #sig {
            #prologue
            #call
        }
    }
}

/// See the crate documentation.
#[proc_macro_attribute]
pub fn engine_shim(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mode = match parse_args(attr) {
        Ok(m) => m,
        Err(e) => return e.to_compile_error().into(),
    };
    let f = match syn::parse::<ItemFn>(item) {
        Ok(f) => f,
        Err(e) => return e.to_compile_error().into(),
    };
    if let Err(e) = check_signature(&f) {
        return e.to_compile_error().into();
    }
    expand(mode, f).into()
}
