/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Partitioning the function cache by top-level site (LucinaNet; no SpiderMonkey counterpart). A shared cache would let a
//! page tell from timing whether another site has loaded a function, so browsers key code caches on the top-frame site.
//! Servo calls `lucinanet_js_set_cache_partition` once after creating a Window global, with the site of the top-level
//! document (scheme and registrable domain). Functions are then cached and reused only among globals with the same site,
//! in memory and in the on-disk store. A global that never gets a partition gets no function cache at all.

use brimstone_core::runtime::embed;

use super::cx::cx_of;
use super::realm::object_ptr;
use crate::jsapi::{JSContext, JSObject};

/// Sets the function cache partition of `global`'s realm to the `len` bytes at `site` (for example `b"https://example.org"`).
/// An empty or null `site` leaves the realm without a partition.
///
/// # Safety
/// `cx` is a live context of this crate, used on its own thread; `global` is a live global object of it; `site` points to
/// `len` readable bytes, or is null.
#[no_mangle]
#[super::poison::engine_shim]
pub unsafe extern "C" fn lucinanet_js_set_cache_partition(
    cx: *mut JSContext,
    global: *mut JSObject,
    site: *const u8,
    len: usize,
) {
    let bs = unsafe { cx_of(cx) }.bs;
    let _scope = brimstone_core::runtime::gc::HandleScopeGuard::new(bs);
    let site: &[u8] = if site.is_null() || len == 0 { &[] } else { unsafe { ::std::slice::from_raw_parts(site, len) } };
    let realm = embed::realm_of_object(bs, object_ptr(global)).to_handle();
    embed::set_realm_cache_partition(bs, realm, site);
}

/// The function cache's counters of a context: (lookups, hits, misses). For tests and profiling.
///
/// # Safety
/// `cx` is a live context of this crate, used on its own thread.
pub unsafe fn cache_counters(cx: *mut JSContext) -> (u64, u64, u64) {
    let stats = embed::function_cache_stats(unsafe { cx_of(cx) }.bs);
    (stats.lookups, stats.hits, stats.misses)
}
