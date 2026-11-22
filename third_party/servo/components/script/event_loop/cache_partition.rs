/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! LucinaNet: the partition of Brimstone's function cache for a new Window global.
//!
//! Brimstone keeps the bytecode of functions it has compiled and reuses it when a page compiles the same function again (on
//! the next visit, across a player's releases). A cache shared across sites lets one site tell from timing whether another
//! has been loaded, so, as browsers key their code caches on the top-level site, the engine reuses functions only among
//! globals of the same partition, and a global without one gets no cache at all (LucinaNet-js `cache_partition.rs`).
//!
//! The partition is the top-level site: scheme and registrable domain, the HTML "obtain a site" steps (`Site::from_origin`).
//! Only top-level documents get one. Servo does not track an iframe's top-level creation URL yet (its TODO 37417 in
//! `script_thread.rs`: the iframe's own URL is passed), and partitioning an iframe by its own site would share entries
//! across the sites that embed it, so iframes and opaque origins stay uncached. Without the `brimstone` feature
//! (SpiderMonkey) nothing here does anything.

#[cfg(feature = "brimstone")]
use net_traits::pub_domains::Site;
#[cfg(feature = "brimstone")]
use servo_url::{Host, ImmutableOrigin};

/// The partition of a top-level document of `origin`: `scheme://registrable-domain` (or `scheme://host` where the host has
/// no registrable domain: an IP address, `localhost`). None for an opaque origin.
#[cfg(feature = "brimstone")]
pub(crate) fn top_level_partition(origin: &ImmutableOrigin) -> Option<String> {
    match Site::from_origin(origin) {
        Site::Opaque(_) => None,
        Site::Tuple { scheme, host } => {
            let host = match host {
                Host::Domain(domain) => domain,
                Host::Ipv4(address) => address.to_string(),
                Host::Ipv6(address) => format!("[{address}]"),
            };
            Some(format!("{scheme}://{host}"))
        },
    }
}

/// Gives the realm of `global` its function cache partition: `site`, or none (no cache) when `site` is None.
///
/// # Safety
/// `cx` is this script thread's live context, and `global` a live Window global of it.
#[cfg(feature = "brimstone")]
pub(crate) unsafe fn set(
    cx: *mut mozjs_sys::jsapi::JSContext,
    global: *mut mozjs_sys::jsapi::JSObject,
    site: Option<&str>,
) {
    let site = site.unwrap_or("");
    // SAFETY: the caller's contract for `cx` and `global`; `site` is `len` readable bytes for the call, and the engine copies
    // them.
    unsafe {
        mozjs_sys::engine::cache_partition::lucinanet_js_set_cache_partition(cx, global, site.as_ptr(), site.len())
    };
}

#[cfg(all(test, feature = "brimstone"))]
mod tests {
    use std::path::PathBuf;

    use embedder_traits::resources::{Resource, ResourceReaderMethods};
    use servo_url::ServoUrl;

    use super::top_level_partition;

    /// The registrable domain (`Site::from_origin`) reads the public suffix list through the embedder's resource reader,
    /// which panics when none is registered. A script unit test has no embedder, so it registers this one, serving the real
    /// list Servo ships (`components/default-resources`): the partition logic itself is the production code. (Depending on
    /// `servo-default-resources` instead would change the dependency set the gate's `--locked` build is checked against.)
    struct TestResourceReader;

    impl ResourceReaderMethods for TestResourceReader {
        fn read(&self, res: Resource) -> Vec<u8> {
            match res {
                Resource::DomainList => {
                    include_bytes!("../../default-resources/resources/public_domains.txt").to_vec()
                },
                other => panic!("the cache partition tests do not read {}", other.filename()),
            }
        }
        fn sandbox_access_files(&self) -> Vec<PathBuf> {
            vec![]
        }
        fn sandbox_access_files_dirs(&self) -> Vec<PathBuf> {
            vec![]
        }
    }

    embedder_traits::submit_resource_reader!(&TestResourceReader);

    fn partition(url: &str) -> Option<String> {
        top_level_partition(&ServoUrl::parse(url).unwrap().origin())
    }

    #[test]
    fn the_partition_is_scheme_and_registrable_domain() {
        assert_eq!(partition("https://www.youtube.com/watch?v=x").as_deref(), Some("https://youtube.com"));
        assert_eq!(partition("https://m.youtube.com/").as_deref(), Some("https://youtube.com"));
        assert_eq!(partition("http://www.youtube.com/").as_deref(), Some("http://youtube.com"));
        assert_eq!(partition("https://news.bbc.co.uk/").as_deref(), Some("https://bbc.co.uk"));
        assert_eq!(partition("http://127.0.0.1:8000/").as_deref(), Some("http://127.0.0.1"));
    }

    #[test]
    fn an_opaque_origin_has_no_partition() {
        assert_eq!(partition("data:text/html,hi"), None);
    }
}
