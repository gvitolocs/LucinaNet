/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use crate::dom::bindings::str::DOMString;

#[expect(non_snake_case)]
pub(crate) fn Product() -> DOMString {
    DOMString::from_static("Gecko")
}

#[expect(non_snake_case)]
pub(crate) fn ProductSub() -> DOMString {
    DOMString::from_static("20100101")
}

#[expect(non_snake_case)]
pub(crate) fn Vendor() -> DOMString {
    DOMString::new()
}

#[expect(non_snake_case)]
pub(crate) fn VendorSub() -> DOMString {
    DOMString::new()
}

#[expect(non_snake_case)]
pub(crate) fn TaintEnabled() -> bool {
    false
}

#[expect(non_snake_case)]
pub(crate) fn AppName() -> DOMString {
    DOMString::from_static("Netscape") // Like Gecko/Webkit
}

#[expect(non_snake_case)]
pub(crate) fn AppCodeName() -> DOMString {
    DOMString::from_static("Mozilla")
}

/// `navigator.platform`: the embedder's `navigator_platform` preference when set, else the build target's default.
#[expect(non_snake_case)]
pub(crate) fn Platform(configured: &str) -> DOMString {
    if configured.is_empty() {
        DOMString::from_static(target_platform())
    } else {
        DOMString::from(configured)
    }
}

#[cfg(target_os = "windows")]
const fn target_platform() -> &'static str {
    "Win32"
}

#[cfg(any(target_os = "android", target_os = "linux", target_os = "freebsd"))]
const fn target_platform() -> &'static str {
    "Linux"
}

#[cfg(target_os = "macos")]
const fn target_platform() -> &'static str {
    "Mac"
}

#[cfg(target_os = "ios")]
const fn target_platform() -> &'static str {
    "iOS"
}

#[expect(non_snake_case)]
pub(crate) fn UserAgent(user_agent: &str) -> DOMString {
    DOMString::from(user_agent)
}

#[expect(non_snake_case)]
pub(crate) fn AppVersion() -> DOMString {
    DOMString::from_static("4.0")
}

#[expect(non_snake_case)]
pub(crate) fn Language() -> DOMString {
    DOMString::from(net_traits::get_current_locale().0.clone())
}

#[cfg(test)]
mod tests {
    use super::{Platform, target_platform};

    #[test]
    fn configured_platform_wins_and_empty_falls_back_to_the_target() {
        assert_eq!(&*Platform("MacIntel"), "MacIntel");
        assert_eq!(&*Platform(""), target_platform());
    }
}
