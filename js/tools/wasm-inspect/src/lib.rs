/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Read a WebAssembly binary and say what it is and what it needs.
//!
//! The library is separate from the binary so the verdict can be tested on
//! modules built in the tests. Nothing here can run a module: there is no
//! engine in this crate's dependency list, only `wasmparser`.

pub mod inspect;
pub mod render;
pub mod verdict;
