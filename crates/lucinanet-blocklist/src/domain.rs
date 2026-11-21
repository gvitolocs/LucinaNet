/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Host helpers that work on URL strings without a URL parser, and the third-party test.

/// Two-label public suffixes common enough to matter for a heuristic eTLD+1; a real public-suffix list is the caller's
/// job (it can pass `Request::third_party` instead).
const TWO_LABEL_SUFFIXES: &[&str] = &[
    "co.uk", "org.uk", "ac.uk", "gov.uk", "me.uk", "ltd.uk", "plc.uk", "net.uk", "sch.uk", "com.au", "net.au", "org.au",
    "edu.au", "gov.au", "co.nz", "org.nz", "net.nz", "govt.nz", "co.jp", "ne.jp", "or.jp", "ac.jp", "go.jp", "com.br",
    "net.br", "org.br", "gov.br", "com.cn", "net.cn", "org.cn", "gov.cn", "edu.cn", "co.in", "net.in", "org.in",
    "gov.in", "ac.in", "co.za", "org.za", "com.mx", "org.mx", "gob.mx", "com.tr", "org.tr", "gov.tr", "com.ar",
    "org.ar", "gob.ar", "com.sg", "com.hk", "com.tw", "co.kr", "or.kr", "co.id", "co.il", "com.ua", "com.pl",
];

/// Byte range of the host inside an http/https/ws/wss URL (no userinfo, no port, IPv6 brackets kept).
pub(crate) fn host_range(url: &str) -> Option<(usize, usize)> {
    let b = url.as_bytes();
    let after_scheme = ["http://", "https://", "ws://", "wss://"]
        .iter()
        .find(|p| b.len() >= p.len() && b[..p.len()].eq_ignore_ascii_case(p.as_bytes()))
        .map(|p| p.len())?;
    let auth_end = b[after_scheme..]
        .iter()
        .position(|&c| matches!(c, b'/' | b'?' | b'#'))
        .map_or(b.len(), |i| after_scheme + i);
    let host_start = b[after_scheme..auth_end]
        .iter()
        .rposition(|&c| c == b'@')
        .map_or(after_scheme, |i| after_scheme + i + 1);
    let host_end = if b.get(host_start) == Some(&b'[') {
        let close = b[host_start..auth_end].iter().position(|&c| c == b']')?;
        host_start + close + 1
    } else {
        b[host_start..auth_end]
            .iter()
            .rposition(|&c| c == b':')
            .map_or(auth_end, |i| host_start + i)
    };
    (host_end > host_start).then_some((host_start, host_end))
}

/// Host of a URL string without allocating a URL parser: no port, no userinfo, IPv6 brackets kept. `None` if the URL is not
/// http/https/ws/wss. The slice is returned as written (a `&str` cannot be lower-cased in place); [`crate::Engine::check`]
/// lower-cases the URL before it asks, and callers that compare hosts should do the same.
pub fn host_of(url: &str) -> Option<&str> {
    let (s, e) = host_range(url)?;
    url.get(s..e)
}

fn is_ip_like(host: &str) -> bool {
    host.starts_with('[') || host.contains(':') || (host.bytes().all(|c| c.is_ascii_digit() || c == b'.') && !host.is_empty())
}

/// Heuristic registrable domain (eTLD+1): the last two labels, or three when the last two form a common two-label public
/// suffix such as `co.uk`. IP addresses and single-label hosts come back whole.
pub fn registrable_domain(host: &str) -> &str {
    let host = host.strip_suffix('.').unwrap_or(host);
    if is_ip_like(host) {
        return host;
    }
    let Some(d1) = host.rfind('.') else { return host };
    let Some(d2) = host[..d1].rfind('.') else { return host };
    let pair = &host[d2 + 1..];
    if TWO_LABEL_SUFFIXES.iter().any(|s| s.eq_ignore_ascii_case(pair)) {
        match host[..d2].rfind('.') {
            Some(d3) => &host[d3 + 1..],
            None => host,
        }
    } else {
        pair
    }
}

/// True when the two hosts have different registrable domains (case-insensitive).
pub fn is_third_party(request_host: &str, source_host: &str) -> bool {
    !registrable_domain(request_host).eq_ignore_ascii_case(registrable_domain(source_host))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn userinfo_and_port_in_host_of() {
        assert_eq!(host_of("https://user:pw@example.com:8443/a?b#c"), Some("example.com"));
        assert_eq!(host_of("http://example.com"), Some("example.com"));
        assert_eq!(host_of("http://example.com?x=1"), Some("example.com"));
        assert_eq!(host_of("ws://[::1]:9000/x"), Some("[::1]"));
        assert_eq!(host_of("wss://a@b@c.org/"), Some("c.org"));
        assert_eq!(host_of("http://192.168.0.1:80/"), Some("192.168.0.1"));
        assert_eq!(host_of("HTTP://x.org/"), Some("x.org"));
    }

    #[test]
    fn host_of_rejects_non_http() {
        for u in ["ftp://x.org/", "data:text/html,hi", "example.com", "//x.org/", "https://", "http://:80/", "http://[::1/", ""] {
            assert_eq!(host_of(u), None, "{u}");
        }
    }

    #[test]
    fn registrable_domain_cases() {
        assert_eq!(registrable_domain("example.com"), "example.com");
        assert_eq!(registrable_domain("a.b.example.com"), "example.com");
        assert_eq!(registrable_domain("www.example.co.uk"), "example.co.uk");
        assert_eq!(registrable_domain("example.co.uk"), "example.co.uk");
        assert_eq!(registrable_domain("co.uk"), "co.uk");
        assert_eq!(registrable_domain("localhost"), "localhost");
        assert_eq!(registrable_domain("192.168.0.1"), "192.168.0.1");
        assert_eq!(registrable_domain("[::1]"), "[::1]");
        assert_eq!(registrable_domain("x.example.com."), "example.com");
        assert_eq!(registrable_domain("a.shop.com.au"), "shop.com.au");
    }

    #[test]
    fn is_third_party_cases() {
        assert!(!is_third_party("example.com", "cdn.example.com"));
        assert!(is_third_party("example.co.uk", "other.co.uk"));
        assert!(!is_third_party("a.example.co.uk", "b.example.co.uk"));
        assert!(is_third_party("example.com", "example.org"));
        assert!(is_third_party("10.0.0.1", "10.0.0.2"));
        assert!(!is_third_party("10.0.0.1", "10.0.0.1"));
        assert!(!is_third_party("Example.COM", "www.example.com"));
        assert!(!is_third_party("localhost", "localhost"));
    }
}
