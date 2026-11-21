//! The registrable domain (eTLD+1) of an address, for the address field to emphasise: of `news.bbc.co.uk`, the part that
//! says which site this is is `bbc.co.uk`.
//!
//! The list is the engine's own - Servo's Public Suffix List (`net_traits::pub_domains`), baked into this binary by the
//! `bundled` feature of the pinned fork - and the UI keeps no copy of it, so the answer travels to the window on
//! [`lucinanet_ui::TabInfo`] and the field draws what it is told. Where the list cannot place a host (`localhost`, a single
//! label, a bare suffix, an IP literal) the answer is `None` and the field emphasises the whole host, which is what it did
//! before there was a list. `docs/specs/wiring-omnibox-history.md`, "Unfocused address display".

use url::{Host, Url};

/// The pure half: what a host's registrable domain is, given a function from a host to its public suffix and its
/// registrable domain as a list sees them. The one rule on top of the list is here rather than in the list: a host whose
/// registrable domain *is* its public suffix is a suffix and not a site (`com`, `github.io`, `localhost`), and there is
/// nothing to emphasise inside it. Servo's pair is what the engine passes (see [`psl`]); a test passes a table.
pub fn registrable_domain(host: &str, suffixes_of: impl Fn(&str) -> (&str, &str)) -> Option<String> {
    let (public, registrable) = suffixes_of(host);
    (registrable != public).then(|| registrable.to_owned())
}

/// A host's (public suffix, registrable domain) by Servo's list, compiled into this binary.
fn psl(host: &str) -> (&str, &str) {
    (net_traits::pub_domains::pub_suffix(host), net_traits::pub_domains::reg_suffix(host))
}

/// The registrable domain of `address`, or `None` when there is nothing to emphasise inside its host: no host at all
/// (`about:blank`, a page that has not committed), an IP literal, a host the list cannot place, or text that is not an
/// address.
///
/// The host comes from the parsed URL and not from the list, because the list answers a numeric host as if it were dotted
/// labels - `reg_suffix("127.0.0.1")` is "0.1", which names no site. A `Host::Domain` is the only host a registrable
/// domain can be taken from; the display crate would ignore such a value too (`display.rs`, `formatted`), and the field is
/// better served by not being handed one.
pub fn registrable_domain_of(address: &str) -> Option<String> {
    let Ok(url) = Url::parse(address) else { return None };
    let Some(Host::Domain(host)) = url.host() else { return None };
    registrable_domain(host, psl)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a list answers, as (public suffix, registrable domain), in the shapes the real list has: a one-label suffix,
    /// a suffix of two labels, a suffix that takes one more label, a wildcard that swallows the label before it, the
    /// exception that gives that label back, and a host no list knows.
    const LIST: &[(&str, (&str, &str))] = &[
        ("example.com", ("com", "example.com")),
        ("www.example.com", ("com", "example.com")),
        ("a.b.c.example.net", ("net", "example.net")),
        ("com", ("com", "com")),
        ("news.bbc.co.uk", ("co.uk", "bbc.co.uk")),
        ("example.co.uk", ("co.uk", "example.co.uk")),
        ("co.uk", ("co.uk", "co.uk")),
        ("me.github.io", ("github.io", "me.github.io")),
        ("github.io", ("github.io", "github.io")),
        ("a.ck", ("a.ck", "a.ck")),
        ("x.a.ck", ("a.ck", "x.a.ck")),
        ("www.ck", ("ck", "www.ck")),
        ("localhost", ("localhost", "localhost")),
        ("intranet", ("intranet", "intranet")),
        ("", ("", "")),
    ];

    fn listed(host: &str) -> (&str, &str) {
        if let Some((_, pair)) = LIST.iter().find(|(h, _)| *h == host) {
            return *pair;
        }
        // What Servo's walk does with a host no list knows (`suffix_pair_without_trailing_dot`): it ends on the last dot,
        // so the last label becomes the suffix and the registrable domain is that label plus the one before it - not the
        // whole host. A single label is its own suffix, and so its own registrable domain, which the rule then drops.
        match host.rsplit_once('.') {
            Some((before, last)) => (last, &host[before.rfind('.').map(|i| i + 1).unwrap_or(0)..]),
            None => (host, host),
        }
    }

    /// One row per case the rule has to get right. A host that *is* a suffix gives nothing, which is the difference
    /// between `me.github.io` and `github.io` and between `bbc.co.uk` and a bare `co.uk`; a wildcard's host gives nothing
    /// either, and its exception gives the label back.
    #[test]
    fn the_list_and_the_rule_together_answer_for_each_shape_of_host() {
        for (host, want) in [
            ("example.com", Some("example.com")),
            ("www.example.com", Some("example.com")),
            ("a.b.c.example.net", Some("example.net")),
            ("com", None),
            ("news.bbc.co.uk", Some("bbc.co.uk")),
            ("example.co.uk", Some("example.co.uk")),
            ("co.uk", None),
            ("me.github.io", Some("me.github.io")),
            ("github.io", None),
            ("a.ck", None),
            ("x.a.ck", Some("x.a.ck")),
            ("www.ck", Some("www.ck")),
            ("localhost", None),
            ("intranet", None),
            ("", None),
            ("unlisted.example", Some("unlisted.example")),
            ("a.b.c.unlisted", Some("c.unlisted")),
        ] {
            assert_eq!(registrable_domain(host, listed).as_deref(), want, "{host}");
        }
    }

    /// The engine's real list, host by host. Two rows are worth reading twice: `pages.dev` is not in the pinned revision,
    /// so the answer is the registrable domain under the suffix `dev` rather than the Cloudflare Pages site, and a trailing
    /// dot stays in the answer, because the list keeps `example.com.` and `example.com` distinct hosts. The `invalid` row
    /// is the one the fake table above cannot vouch for by itself: a host whose last label is no suffix at all, which the
    /// walk ends on, so the answer is the last two labels rather than the whole host.
    #[test]
    fn the_engine_list_answers_for_the_hosts_the_field_sees() {
        for (address, want) in [
            ("https://google.com/", Some("google.com")),
            ("https://www.google.com/search?q=lu", Some("google.com")),
            ("https://news.bbc.co.uk/", Some("bbc.co.uk")),
            ("https://me.github.io/path", Some("me.github.io")),
            ("https://example.pages.dev/", Some("pages.dev")),
            ("https://www.google.com./", Some("google.com.")),
            ("https://a.b.c.invalid/", Some("c.invalid")),
            ("https://xn--80ak6aa92e.com/", Some("xn--80ak6aa92e.com")),
            ("https://www.xn--80axh.xn--p1ai/", Some("xn--80axh.xn--p1ai")),
            ("https://localhost:8080/", None),
            ("https://intranet/", None),
            ("http://127.0.0.1:8080/", None),
            ("http://[::1]:8080/", None),
            ("about:blank", None),
            ("", None),
            ("not a url", None),
        ] {
            assert_eq!(registrable_domain_of(address).as_deref(), want, "{address}");
        }
    }

    /// The two IP literals above are `None` because a parsed host that is not a domain is not asked of the list, not
    /// because the list has no answer for it: given the digits, it places them. This is what
    /// [`registrable_domain_of`]'s `Host::Domain` is for.
    #[test]
    fn the_list_alone_cannot_tell_an_ip_from_a_domain() {
        assert_eq!(registrable_domain("127.0.0.1", psl).as_deref(), Some("0.1"));
        assert_eq!(registrable_domain("::1", psl), None);
    }
}
