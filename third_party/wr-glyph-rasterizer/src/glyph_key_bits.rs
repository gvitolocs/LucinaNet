/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The bit layout of `GlyphKey`: glyph index in bits 0..26, x subpixel offset in 26..28, y subpixel offset in
//! 28..30, subpixel direction in 30..32. Pure functions on integers, so Verus proves them in this very file
//! (the rule; `verification/glyph_key.rs` names it with `#[path]`): for an index < 2^26 and fields < 4,
//! unpacking gives every field back, and `pack` is injective, so the glyph cache cannot confuse two glyphs or two
//! subpixel positions. The attributes are only seen by Verus.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg(verus_keep_ghost)]
verus! {

/// What `pack` computes, as a spec.
pub open spec fn pack_bits(index: u32, sox: u8, soy: u8, dir: u8) -> u32 {
    index | (sox as u32) << 26u32 | (soy as u32) << 28u32 | (dir as u32) << 30u32
}

/// The domain the glyph key is defined on.
pub open spec fn in_domain(index: u32, sox: u8, soy: u8, dir: u8) -> bool {
    index < 0x0400_0000 && sox < 4 && soy < 4 && dir < 4
}

/// Distinct inputs on the domain give distinct keys.
pub proof fn pack_injective(i1: u32, x1: u8, y1: u8, d1: u8, i2: u32, x2: u8, y2: u8, d2: u8)
    requires
        in_domain(i1, x1, y1, d1),
        in_domain(i2, x2, y2, d2),
        pack_bits(i1, x1, y1, d1) == pack_bits(i2, x2, y2, d2),
    ensures i1 == i2 && x1 == x2 && y1 == y2 && d1 == d2,
{
    pack_round_trip(i1, x1, y1, d1);
    pack_round_trip(i2, x2, y2, d2);
}

/// Unpacking a packed key on the domain gives every field back.
pub proof fn pack_round_trip(index: u32, sox: u8, soy: u8, dir: u8)
    requires in_domain(index, sox, soy, dir),
    ensures
        pack_bits(index, sox, soy, dir) & 0x03FF_FFFF == index,
        ((pack_bits(index, sox, soy, dir) >> 26u32) as u8) & 3 == sox,
        ((pack_bits(index, sox, soy, dir) >> 28u32) as u8) & 3 == soy,
        ((pack_bits(index, sox, soy, dir) >> 30u32) as u8) & 3 == dir,
{
    let k = pack_bits(index, sox, soy, dir);
    let x = sox as u32;
    let y = soy as u32;
    let d = dir as u32;
    assert(k == index | x << 26u32 | y << 28u32 | d << 30u32);
    assert(index < 0x0400_0000 && x < 4 && y < 4 && d < 4 ==> {
        let k = index | x << 26u32 | y << 28u32 | d << 30u32;
        k & 0x03FF_FFFF == index && (k >> 26u32) & 3 == x && (k >> 28u32) & 3 == y && (k >> 30u32) & 3 == d
    }) by(bit_vector);
    assert(((k >> 26u32) as u8) & 3 == ((k >> 26u32) & 3) as u8) by(bit_vector);
    assert(((k >> 28u32) as u8) & 3 == ((k >> 28u32) & 3) as u8) by(bit_vector);
    assert(((k >> 30u32) as u8) & 3 == ((k >> 30u32) & 3) as u8) by(bit_vector);
}

} // verus!

/// Pack a glyph key. Callers check `index < 2^26` first (`GlyphKey::new` asserts it); the fields are enum
/// discriminants < 4.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == pack_bits(index, sox, soy, dir),
        in_domain(index, sox, soy, dir) ==> index_of(r) == index && sox_of(r) == sox && soy_of(r) == soy
            && dir_of(r) == dir,
))]
pub fn pack(index: u32, sox: u8, soy: u8, dir: u8) -> u32 {
    #[cfg(verus_keep_ghost)]
    proof! {
        if in_domain(index, sox, soy, dir) {
            pack_round_trip(index, sox, soy, dir);
        }
    }
    index | (sox as u32) << 26 | (soy as u32) << 28 | (dir as u32) << 30
}

/// The glyph index, bits 0..26.
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == k & 0x03FF_FFFF, r < 0x0400_0000))]
#[cfg_attr(verus_keep_ghost, verifier::when_used_as_spec(index_of_spec))]
pub fn index_of(k: u32) -> u32 {
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(k & 0x03FF_FFFF < 0x0400_0000) by(bit_vector);
    }
    k & 0x03FF_FFFF
}

/// The x subpixel offset, bits 26..28: always < 4.
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == ((k >> 26u32) as u8) & 3, r < 4))]
#[cfg_attr(verus_keep_ghost, verifier::when_used_as_spec(sox_of_spec))]
pub fn sox_of(k: u32) -> u8 {
    #[cfg_attr(verus_keep_ghost, verifier::truncate)]
    let x = (k >> 26) as u8;
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(x & 3 < 4) by(bit_vector);
    }
    x & 3
}

/// The y subpixel offset, bits 28..30: always < 4.
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == ((k >> 28u32) as u8) & 3, r < 4))]
#[cfg_attr(verus_keep_ghost, verifier::when_used_as_spec(soy_of_spec))]
pub fn soy_of(k: u32) -> u8 {
    #[cfg_attr(verus_keep_ghost, verifier::truncate)]
    let y = (k >> 28) as u8;
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(y & 3 < 4) by(bit_vector);
    }
    y & 3
}

/// The subpixel direction, bits 30..32: always < 4.
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == ((k >> 30u32) as u8) & 3, r < 4))]
#[cfg_attr(verus_keep_ghost, verifier::when_used_as_spec(dir_of_spec))]
pub fn dir_of(k: u32) -> u8 {
    #[cfg_attr(verus_keep_ghost, verifier::truncate)]
    let d = (k >> 30) as u8;
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(d & 3 < 4) by(bit_vector);
    }
    d & 3
}

#[cfg(verus_keep_ghost)]
verus! {

pub open spec fn index_of_spec(k: u32) -> u32 { k & 0x03FF_FFFF }
pub open spec fn sox_of_spec(k: u32) -> u8 { ((k >> 26u32) as u8) & 3 }
pub open spec fn soy_of_spec(k: u32) -> u8 { ((k >> 28u32) as u8) & 3 }
pub open spec fn dir_of_spec(k: u32) -> u8 { ((k >> 30u32) as u8) & 3 }

} // verus!
