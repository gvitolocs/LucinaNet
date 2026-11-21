/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The SpiderMonkey API that the `mozjs` crate binds, implemented in Rust on Boa. `jsgc`, `jsid`, `jsimpls`,
//! `jsval` and `trace` are mozjs_sys's own Rust files; `generated` comes from its bindgen output (tools/gen-jsapi);
//! `engine` is the part that replaces the C++.

#![allow(unused_extern_crates)]
#![cfg_attr(feature = "crown", feature(register_tool))]
#![cfg_attr(feature = "crown", register_tool(crown))]

mod jsimpls;

// NaN-boxed values hold 64-bit pointers; the Verus proofs in `boundary` assume a 64-bit usize too.
const _: () = assert!(usize::BITS == 64);

pub mod boundary;
pub mod engine;
pub mod glue;
pub mod jsgc;
pub mod jsid;
pub mod jsval;
pub mod trace;

pub use crate::generated::root as jsapi;
/// The part of the engine this glue cannot wrap in C: external ArrayBuffers and their detach keys, the CSP's compilation kinds,
/// the realm and wasm queries, and the handle scope a raw engine call needs. The engine crate itself is deliberately not
/// re-exported: its object types carry methods that refuse nothing (`ArrayBufferObject::detach` ignores a detach key), and every
/// guarantee this glue makes lives in the functions below.
pub mod embed {
    pub use brimstone_core::runtime::embed::{api, current_realm, wasm_enabled, CompileStringsKind};
    pub use brimstone_core::runtime::gc::HandleScope;
}

#[doc(hidden)]
#[allow(dead_code, unused_imports, static_mut_refs, clippy::all)]
mod generated {
    #![allow(unnecessary_transmutes)]
    include!("jsapi_gen.rs");
}

#[no_mangle]
pub extern "C" fn install_rust_hooks() {}
