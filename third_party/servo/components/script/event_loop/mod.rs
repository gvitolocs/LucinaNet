/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#[cfg_attr(feature = "brimstone", expect(unsafe_code))]
pub(crate) mod cache_partition;
#[cfg(feature = "devtools")]
mod devtools;
pub(crate) mod document_collection;
pub(crate) mod document_loader;
#[cfg_attr(feature = "brimstone", expect(unsafe_code))]
pub(crate) mod lazy_trim_idle;
mod script_mutation_observers;
#[expect(unsafe_code)]
pub(crate) mod script_thread;
pub(crate) mod script_window_proxies;
mod svg_font;
pub(crate) mod timers;
pub(crate) mod webdriver_handlers;
