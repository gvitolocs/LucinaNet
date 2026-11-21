// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Verus proof of the SHIPPED code, not a model (the rule): this file only names the source file, and Verus
// checks `crates/lucina-fontsan/src/bounds.rs` itself, with the attributes in it. What is proved is in each function's
// `ensures` in that file. Nothing is trusted.
// Only seen by Verus (nightly): lets the `verus_spec` loop attribute in the shipped file sit on the `while` expression.
#![cfg_attr(verus_keep_ghost, feature(proc_macro_hygiene))]
#[path = "../crates/lucina-fontsan/src/bounds.rs"]
pub mod bounds;

fn main() {}
