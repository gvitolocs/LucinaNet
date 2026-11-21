/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The origin string permissions are keyed by, and the secure-context test on it.

use url::{Origin, Url};

/// "https://example.org", "http://localhost:3000": scheme://host[:port], default port left out, host lower-case IDNA.
/// `None` for origins that cannot hold a permission: opaque ones (`about:`, `data:`, `blob:`), `file:` (one shared
/// "file://" origin must not hold grants) and every scheme other than http and https.
pub fn origin_of(url: &Url) -> Option<String> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    match url.origin() {
        Origin::Tuple(..) => Some(url.origin().ascii_serialization()),
        Origin::Opaque(_) => None,
    }
}

/// Whether an origin string made by [`origin_of`] is a secure context: https, or http on a loopback host
/// (`localhost`, `*.localhost`, `127.0.0.1`, `[::1]`). Anything else, including text that is not an origin, is not.
pub(crate) fn is_secure_origin(origin: &str) -> bool {
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    match scheme {
        "https" => true,
        "http" => {
            let host = if rest.starts_with('[') {
                rest.split_once(']').map_or(rest, |(h, _)| &rest[..h.len() + 1])
            } else {
                rest.split_once(':').map_or(rest, |(h, _)| h)
            };
            host == "localhost" || host.ends_with(".localhost") || host == "127.0.0.1" || host == "[::1]"
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(s: &str) -> Option<String> {
        origin_of(&Url::parse(s).expect("test url"))
    }

    #[test]
    fn origin_of_strips_default_port_lowercases_idna() {
        assert_eq!(o("https://EXAMPLE.org:443/x"), Some("https://example.org".into()));
        assert_eq!(o("http://example.org:80/"), Some("http://example.org".into()));
        assert_eq!(o("http://localhost:3000/"), Some("http://localhost:3000".into()));
        assert_eq!(o("https://example.org:8443/a?b#c"), Some("https://example.org:8443".into()));
        // "münchen" -> xn--mnchen-3ya (RFC 3492 / IDNA).
        assert_eq!(o("https://münchen.de"), Some("https://xn--mnchen-3ya.de".into()));
    }

    #[test]
    fn origin_of_none_for_opaque_and_file() {
        assert_eq!(o("about:blank"), None);
        assert_eq!(o("data:text/plain,hi"), None);
        assert_eq!(o("blob:https://example.org/1234"), None);
        assert_eq!(o("file:///x"), None);
        assert_eq!(o("ftp://example.org/"), None);
    }

    #[test]
    fn localhost_and_loopback_are_secure() {
        assert!(is_secure_origin("https://example.org"));
        assert!(is_secure_origin("http://localhost"));
        assert!(is_secure_origin("http://localhost:3000"));
        assert!(is_secure_origin("http://app.localhost:8080"));
        assert!(is_secure_origin("http://127.0.0.1:8000"));
        assert!(is_secure_origin("http://[::1]"));
        assert!(is_secure_origin("http://[::1]:9000"));
        assert!(!is_secure_origin("http://example.org"));
        assert!(!is_secure_origin("http://localhost.example.org"));
        assert!(!is_secure_origin("http://128.0.0.1"));
        assert!(!is_secure_origin("file://"));
        assert!(!is_secure_origin("garbage"));
    }
}
