// Verus proof of the SHIPPED code, not a model (the rule): this file only names the source file, and Verus
// checks `src/glyph_key_bits.rs` itself, with the attributes in it. What is proved is in each function's `ensures`
// and in `pack_round_trip` / `pack_injective`. Nothing is trusted.
#![cfg_attr(verus_keep_ghost, feature(proc_macro_hygiene))]
#[path = "../src/glyph_key_bits.rs"]
pub mod glyph_key_bits;

fn main() {}
