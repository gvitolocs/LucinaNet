// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Verus proof of the SHIPPED code, not a model (the rule): this file only names the source file, and Verus
// checks `crates/lucina-ftcompat/src/lib.rs` itself, with the attributes in it. What is proved is in each function's
// `ensures` in that file. Trusted, not proved (`verifier::external_body`): `char_size` and `ppem_f32_for_scale`, which
// are IEEE float arithmetic Verus does not model — their cast/rounding semantics are trusted to match the C code
// Servo and skrifa used.
// Only seen by Verus (nightly): lets the `verus_spec` loop attribute in the shipped file sit on the `while` expression.
#![cfg_attr(verus_keep_ghost, feature(proc_macro_hygiene))]
#[path = "../crates/lucina-ftcompat/src/lib.rs"]
pub mod ftcompat;

fn main() {}
