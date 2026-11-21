/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `jsid` on Brimstone: the bits are Brimstone's `PropertyKey` (an index as an int32 value, an interned string or a symbol as a
//! heap pointer), so ids cross the engine seam unchanged. The rules are in `boundary::id_bits` (Verus-checked).
//!
//! SpiderMonkey's int ids stop at 2^31-1. Indices from 2^31 to 2^32-2 are therefore never handed to Servo as they are: the engine
//! turns them into string ids on the way out (`engine::id::to_servo`) and parses string ids back (`id_bits::parse_index_*`) on the
//! way in, so every id Servo holds is void, an int, a string or a symbol, as with SpiderMonkey.

#![allow(non_snake_case)]

use crate::boundary::{id_bits, value_bits};
use crate::jsapi::JS::{Symbol, TraceKind};
use crate::jsapi::{jsid, JSString};
use libc::c_void;

#[deprecated]
pub const JSID_VOID: jsid = VoidId();

#[inline(always)]
const fn AsPropertyKey(bits: u64) -> jsid {
    jsid { asBits_: bits as usize }
}

/// Brimstone's empty value (`id_bits::void_id`, checked by the test below), written out because this must be a `const fn`.
#[inline(always)]
pub const fn VoidId() -> jsid {
    AsPropertyKey(value_bits::EMPTY_TAG << value_bits::TAG_SHIFT)
}

#[inline(always)]
pub fn IntId(i: i32) -> jsid {
    assert!(i >= 0);
    AsPropertyKey(id_bits::int_id(i))
}

#[inline(always)]
pub fn SymbolId(symbol: *mut Symbol) -> jsid {
    let p = symbol as usize as u64;
    assert!(p != 0 && p < value_bits::POINTER_LIMIT && p & 7 == 0);
    AsPropertyKey(value_bits::pointer(p))
}

/// A string id. The string must not be an array index's canonical spelling: those ids are ints in Brimstone, and the engine's
/// `JS_StringToId`/`AtomToId` parse them; this constructor is for interned strings the engine gave out.
#[inline(always)]
pub fn StringId(s: *mut JSString) -> jsid {
    let p = s as usize as u64;
    assert!(p != 0 && p < value_bits::POINTER_LIMIT && p & 7 == 0);
    AsPropertyKey(value_bits::pointer(p))
}

impl jsid {
    #[inline(always)]
    fn asBits(&self) -> u64 {
        self.asBits_ as u64
    }

    #[inline(always)]
    pub fn is_void(&self) -> bool {
        value_bits::tag(self.asBits()) == value_bits::EMPTY_TAG
    }

    #[inline(always)]
    pub fn is_int(&self) -> bool {
        debug_assert!(!id_bits::is_high_index(self.asBits()), "an index above 2^31-1 reached Servo as an int id");
        id_bits::is_int(self.asBits())
    }

    #[inline(always)]
    pub fn is_string(&self) -> bool {
        id_bits::is_gcthing(self.asBits()) && crate::engine::trace_kind_of(self.asBits()) == TraceKind::String
    }

    #[inline(always)]
    pub fn is_symbol(&self) -> bool {
        id_bits::is_gcthing(self.asBits()) && crate::engine::trace_kind_of(self.asBits()) == TraceKind::Symbol
    }

    #[inline(always)]
    pub fn is_gcthing(&self) -> bool {
        id_bits::is_gcthing(self.asBits())
    }

    #[inline(always)]
    pub fn to_int(&self) -> i32 {
        assert!(self.is_int());
        id_bits::to_int(self.asBits())
    }

    #[inline(always)]
    pub fn to_string(&self) -> *mut JSString {
        assert!(self.is_string());
        self.asBits() as usize as *mut JSString
    }

    #[inline(always)]
    pub fn to_symbol(&self) -> *mut Symbol {
        assert!(self.is_symbol());
        self.asBits() as usize as *mut Symbol
    }

    #[inline(always)]
    pub fn to_gcthing(&self) -> *mut c_void {
        assert!(self.is_gcthing());
        self.asBits() as usize as *mut c_void
    }
}

#[test]
fn test_representation() {
    let id = VoidId();
    assert!(id.is_void());
    assert_eq!(id.asBits(), id_bits::void_id());

    let id = IntId(0);
    assert!(id.is_int() && !id.is_void() && !id.is_gcthing());
    assert_eq!(id.to_int(), 0);

    let id = IntId(i32::MAX);
    assert!(id.is_int());
    assert_eq!(id.to_int(), i32::MAX);

    assert!(id_bits::is_high_index(id_bits::index_id(0x8000_0000)));
}
