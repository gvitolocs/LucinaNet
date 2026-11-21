/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Bounding the memory lazy bytecode generation keeps (LucinaNet; no SpiderMonkey counterpart, whose lazy functions keep
//! only their source). A lazy stub keeps its script's whole parse tree alive until it is generated: on a YouTube watch
//! page about 250 MB for 13 MB of source, 87% of it for functions never called. `idle_trim` generates pending stubs,
//! largest script first, until the retained parse trees fit in `LUCINANET_JS_PARSE_BUDGET_MB` (default 64), in slices the
//! host runs when idle after a page load, so the load itself does not pay. The budget may be exceeded during the load.

use ::std::time::{Duration, Instant};

use brimstone_core::runtime::embed;
use brimstone_core::runtime::gc::HandleScope;

use super::cx::cx_of;
use crate::jsapi::JSContext;

/// Retained parse trees allowed when idle, in MB, unless `LUCINANET_JS_PARSE_BUDGET_MB` says otherwise.
pub const DEFAULT_PARSE_BUDGET_MB: usize = 64;

/// The budget in bytes.
pub fn parse_budget_bytes() -> usize {
    let mb = ::std::env::var("LUCINANET_JS_PARSE_BUDGET_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_PARSE_BUDGET_MB);
    mb.saturating_mul(1 << 20)
}

/// Parse-tree bytes kept alive by pending lazy stubs.
///
/// # Safety
/// `cx` is a live context of this crate, used on its own thread.
pub unsafe fn retained_parse_bytes(cx: *mut JSContext) -> usize {
    embed::retained_parse_bytes(unsafe { cx_of(cx) }.bs)
}

/// One slice of trimming, at most about `slice` long: for the host's idle time after a page load (a
/// requestIdleCallback-style deadline). Returns true while more work is left over the budget, so the host can schedule
/// another slice. Calls into no script and leaves no exception pending.
///
/// # Safety
/// `cx` is a live context of this crate, used on its own thread, with no script running.
pub unsafe fn idle_trim(cx: *mut JSContext, slice: Duration) -> bool {
    let bs = unsafe { cx_of(cx) }.bs;
    let budget = parse_budget_bytes();
    if embed::retained_parse_bytes(bs) <= budget {
        return false;
    }
    let scope = HandleScope::enter(bs);
    let result = embed::trim_lazy_programs(bs, budget, Instant::now() + slice);
    scope.exit();
    match result {
        Ok(r) => !r.within_budget,
        // Generation errors stay with their stubs (reported on first call); nothing more to do now.
        Err(_) => false,
    }
}
