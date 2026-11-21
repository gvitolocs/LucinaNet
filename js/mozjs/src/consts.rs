/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

pub const default_heapsize: u32 = 32_u32 * 1024_u32 * 1024_u32;

pub use crate::jsapi::JSCLASS_IS_DOMJSCLASS;
pub use crate::jsapi::JSCLASS_USERBIT1;

pub use crate::jsapi::JSCLASS_RESERVED_SLOTS_MASK;

pub use crate::jsapi::JSCLASS_HIGH_FLAGS_SHIFT;

pub use crate::jsapi::JSCLASS_IS_GLOBAL;

pub use crate::jsapi::JSCLASS_IS_PROXY;

pub use crate::jsapi::JSCLASS_GLOBAL_SLOT_COUNT;

// The class flag behind `document.all` (ECMA-262 B.3.6's [[IsHTMLDDA]] slot). A binding that sets it on its JSClass
// gets the Annex B semantics; see mozjs_sys's `engine::realm::host_flags_of`.
pub use crate::jsapi::JSCLASS_EMULATES_UNDEFINED;
