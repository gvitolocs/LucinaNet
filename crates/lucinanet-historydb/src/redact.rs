/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Credentials in recorded addresses (the omnibox/history wiring spec, "Credentials in addresses").
//!
//! An OAuth redirect lands on `https://app/cb?code=...&state=...` or, in the implicit flow, on `https://app/cb#access_token=...`.
//! The address is a place the person may visit again, so it is recorded; but `history.json` is a cleartext file, so what is
//! recorded must not carry the secret. Firefox and Chromium record such addresses as they are (their stores sit in the
//! profile, behind the OS account); this is our adaptation for a cleartext store on `/data`.
//!
//! [`redact_url`] is pure, linear in the input, never panics, and idempotent. The live navigation never goes through it:
//! only what the history stores, indexes or looks up does.

use std::borrow::Cow;

/// Parameter names (compared whole, case-insensitively, after percent-decoding) that mark a query or fragment as holding a
/// credential. `code` and `state` travel together in the authorization-code flow, and `code` alone already triggers, so
/// the "state together with code" rule needs no entry of its own: `state` by itself is not a credential and does not trigger.
pub const SECRET_PARAMS: [&str; 26] = [
    "code",
    "token",
    "access_token",
    "id_token",
    "refresh_token",
    "oauth_token",
    "oauth_verifier",
    "code_verifier",
    "auth",
    "authorization",
    "session",
    "sessionid",
    "session_id",
    "sid",
    "key",
    "api_key",
    "apikey",
    "password",
    "secret",
    "client_secret",
    // Google sign-in flow state (accounts.google.com/v3/signin): one-time flow, form and session tokens.
    "ifkv",
    "dsh",
    "tl",
    "rapt",
    "gxf",
    "osid",
];

/// Path fragments (lower case) of pages that are themselves a credential flow: sign-in, login, OAuth and SSO pages. On such
/// a page the whole query and fragment go, whatever the parameters are called (Google's sign-in page carries `ifkv`, `dsh`,
/// `TL`, `continue` and more, and the set changes without notice). Host and path stay, so the place is still recorded.
pub const FLOW_PATHS: [&str; 6] = ["/signin/", "/login", "/oauth", "/authorize", "/auth/", "/sso"];

/// Parameters whose value is an address to go to next. If that address would itself be redacted the parameter is dropped
/// (the rest of the query stays): `continue=https%3A%2F%2Fapp%2Fcb%3Fcode%3Dx` carries the code of its inner address.
pub const URL_PARAMS: [&str; 8] = ["continue", "redirect_uri", "redirect", "url", "next", "return_to", "returnurl", "return_url"];

/// How deep a URL parameter may nest another one; deeper than this the parameter is dropped if it holds an address.
const MAX_DEPTH: u8 = 3;

/// The address as it may be recorded: for an `http`/`https` address (or one that has no scheme and so cannot be parsed),
/// the whole query and the fragment are dropped when the query names a credential or the path is a sign-in flow, the
/// fragment is dropped when the fragment names a credential, and a URL-valued parameter is dropped when its address would be
/// redacted; any other address is returned unchanged.
pub fn redact_url(url: &str) -> String {
    redact(url).into_owned()
}

/// [`redact_url`] without a copy when nothing changes.
pub(crate) fn redact(url: &str) -> Cow<'_, str> {
    redact_at(url, 0)
}

fn redact_at(url: &str, depth: u8) -> Cow<'_, str> {
    if !url.contains(['?', '#']) || !(is_web(url) || !has_scheme(url)) {
        return Cow::Borrowed(url);
    }
    let (before_fragment, fragment) = match url.find('#') {
        Some(i) => (&url[..i], Some(&url[i + 1..])),
        None => (url, None),
    };
    let (base, query) = match before_fragment.find('?') {
        Some(i) => (&before_fragment[..i], Some(&before_fragment[i + 1..])),
        None => (before_fragment, None),
    };
    if is_flow_path(base) || query.is_some_and(|q| has_secret(q, &['&'])) {
        return Cow::Borrowed(base);
    }
    let fragment_secret = fragment
        .is_some_and(|f| has_secret(f, &['&', '#']) || f.split_once('?').is_some_and(|(_, q)| has_secret(q, &['&', '#'])));
    let kept_fragment = if fragment_secret { None } else { fragment };
    let cleaned_query = query.and_then(|q| drop_url_params(q, depth));
    if cleaned_query.is_none() && !fragment_secret {
        return Cow::Borrowed(url);
    }
    let mut out = String::from(base);
    let q_out = cleaned_query.as_deref().or(query);
    if let Some(q) = q_out
        && !(cleaned_query.is_some() && q.is_empty())
    {
        out.push('?');
        out.push_str(q);
    }
    if let Some(f) = kept_fragment {
        out.push('#');
        out.push_str(f);
    }
    Cow::Owned(out)
}

/// The path part of an address (after the authority, before the query), lower-cased, holds a [`FLOW_PATHS`] entry.
fn is_flow_path(base: &str) -> bool {
    let path = match base.find("//") {
        Some(i) if base[..i].ends_with(':') || i == 0 => {
            let rest = &base[i + 2..];
            rest.find('/').map_or("", |j| &rest[j..])
        }
        _ => base,
    };
    let lower = path.to_ascii_lowercase();
    FLOW_PATHS.iter().any(|f| lower.contains(f))
}

/// The query without its URL-valued parameters whose address would be redacted; None when none is dropped.
fn drop_url_params(query: &str, depth: u8) -> Option<String> {
    let mut dropped = false;
    let mut kept: Vec<&str> = Vec::new();
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if value.len() > 8 && is_url_param(name) {
            let inner = decode(value);
            if is_web(&inner) && (depth >= MAX_DEPTH || *redact_at(&inner, depth + 1) != *inner) {
                dropped = true;
                continue;
            }
        }
        kept.push(pair);
    }
    dropped.then(|| kept.join("&"))
}

fn is_url_param(name: &str) -> bool {
    let name = decode(name);
    URL_PARAMS.iter().any(|n| name.eq_ignore_ascii_case(n))
}

/// `http:` or `https:` in any case.
fn is_web(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else { return false };
    scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
}

/// Whether the text starts with a syntactically valid scheme (`alpha *(alnum / + - .)` then `:`).
fn has_scheme(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else { return false };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
}

/// Whether any `name[=value]` pair of an `a=b&c=d` string has a name in [`SECRET_PARAMS`].
fn has_secret(params: &str, separators: &[char]) -> bool {
    params.split(separators).any(|pair| {
        let name = pair.split_once('=').map_or(pair, |(n, _)| n);
        is_secret_name(name)
    })
}

fn is_secret_name(name: &str) -> bool {
    // Longer than any name, even with every byte percent-encoded: cannot match.
    let longest = SECRET_PARAMS.iter().map(|n| n.len()).max().unwrap_or(0);
    if name.len() > longest * 3 {
        return false;
    }
    let decoded = decode(name);
    SECRET_PARAMS.iter().any(|n| decoded.eq_ignore_ascii_case(n))
}

/// Percent-decode (`%63` is `c`), leaving a malformed escape as it is.
fn decode(s: &str) -> Cow<'_, str> {
    if !s.contains('%') {
        return Cow::Borrowed(s);
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Cow::Owned(String::from_utf8_lossy(&out).into_owned())
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: &str = "https://app.example/cb";

    #[test]
    fn each_name_in_the_query_drops_query_and_fragment() {
        for name in SECRET_PARAMS {
            for case in [name.to_string(), name.to_uppercase(), format!("{}{}", &name[..1].to_uppercase(), &name[1..])] {
                assert_eq!(redact_url(&format!("{B}?{case}=v1&x=1#frag")), B, "{case}");
                assert_eq!(redact_url(&format!("{B}?x=1&{case}=v1")), B, "{case}");
                assert_eq!(redact_url(&format!("{B}?{case}")), B, "{case} without value");
                assert_eq!(redact_url(&format!("{B}?{case}=")), B, "{case} empty value");
            }
        }
    }

    #[test]
    fn each_name_in_the_fragment_drops_the_fragment_only() {
        for name in SECRET_PARAMS {
            for case in [name.to_string(), name.to_uppercase()] {
                assert_eq!(redact_url(&format!("{B}?x=1#{case}=zzz&token_type=bearer")), format!("{B}?x=1"), "{case}");
                assert_eq!(redact_url(&format!("{B}#{case}=zzz")), B, "{case}");
                assert_eq!(redact_url(&format!("{B}#x=1&{case}=zzz")), B, "{case}");
                assert_eq!(redact_url(&format!("{B}#{case}")), B, "{case} without value");
            }
        }
    }

    #[test]
    fn percent_encoded_names_are_decoded() {
        assert_eq!(redact_url(&format!("{B}?%63ode=abc&x=1")), B);
        assert_eq!(redact_url(&format!("{B}?%43ODE=abc")), B);
        assert_eq!(redact_url(&format!("{B}?access%5Ftoken=abc")), B);
        assert_eq!(redact_url(&format!("{B}#%61ccess_token=abc&x=1")), B);
        // A malformed escape is left as it is and the name does not match.
        assert_eq!(redact_url(&format!("{B}?%6=abc")), format!("{B}?%6=abc"));
        assert_eq!(redact_url(&format!("{B}?cod%65%=abc")), format!("{B}?cod%65%=abc"));
    }

    #[test]
    fn names_are_matched_whole() {
        for u in [
            format!("{B}?codeX=1"),
            format!("{B}?xcode=1"),
            format!("{B}?my_code=1"),
            format!("{B}?tokens=1"),
            format!("{B}?keyboard=1"),
            format!("{B}?state=1&x=2"),
            format!("{B}?a=code&b=token"),
            format!("{B}?x=1&y=2#codeX=1"),
            format!("{B}?q=a%26code%3D1"),
            format!("{B}#sidebar=1"),
        ] {
            assert_eq!(redact_url(&u), u);
        }
    }

    #[test]
    fn other_addresses_are_unchanged() {
        for u in [
            "https://example.com/",
            "https://example.com/a/b?x=1&y=2",
            "https://example.com/#/settings",
            "https://example.com/app#/settings/profile",
            "https://example.com/?",
            "https://example.com/#",
            "https://example.com/?#",
            "https://example.com/?x=1#top",
            "https://例え.jp/パス?検索=値#節",
            "https://example.com/?a=1&&b=2",
            "file:///tmp/a.html?code=1",
            "ftp://h/f?token=1",
            "",
            "about:blank",
        ] {
            assert_eq!(redact_url(u), u);
        }
    }

    #[test]
    fn shapes_of_query_and_fragment() {
        assert_eq!(redact_url("https://a.test/p?a=1&code=2"), "https://a.test/p");
        assert_eq!(redact_url("https://a.test/p?code&a=1"), "https://a.test/p");
        assert_eq!(redact_url("https://a.test/p?a=1&code=2&b=3#x"), "https://a.test/p");
        assert_eq!(redact_url("https://a.test/p?&&code=2"), "https://a.test/p");
        assert_eq!(redact_url("https://a.test/p?a=b=c&code=2"), "https://a.test/p");
        assert_eq!(redact_url("https://a.test/p#access_token=zzz&x=1"), "https://a.test/p");
        assert_eq!(redact_url("https://a.test/p?a=1#access_token=zzz&x=1"), "https://a.test/p?a=1");
        assert_eq!(redact_url("https://a.test/p?a=1#/cb?code=9"), "https://a.test/p?a=1");
        assert_eq!(redact_url("HTTPS://A.TEST/p?CODE=1#x"), "HTTPS://A.TEST/p");
        // A second '?' or '#' belongs to the query or fragment.
        assert_eq!(redact_url("https://a.test/p?x=?&code=1"), "https://a.test/p");
        assert_eq!(redact_url("https://a.test/p#a#access_token=1"), "https://a.test/p");
        // Empty query and fragment with nothing secret stay.
        assert_eq!(redact_url("https://a.test/p?#frag"), "https://a.test/p?#frag");
    }

    #[test]
    fn unparseable_text_is_cut_only_when_suspicious() {
        assert_eq!(redact_url("not a url?code=1#x"), "not a url");
        assert_eq!(redact_url("//host/p?token=1"), "//host/p");
        assert_eq!(redact_url("host/p#id_token=1"), "host/p");
        assert_eq!(redact_url("not a url?x=1#y"), "not a url?x=1#y");
        assert_eq!(redact_url("https:?code=1"), "https:");
        assert_eq!(redact_url("https://"), "https://");
        assert_eq!(redact_url("???&&&###"), "???&&&###");
        assert_eq!(redact_url("http://\u{0}\u{ffff}?code=\u{1f600}"), "http://\u{0}\u{ffff}");
    }

    #[test]
    fn it_is_idempotent() {
        for u in [
            "https://a.test/p?a=1&code=2#x",
            "https://a.test/p?a=1#access_token=1",
            "https://a.test/p?%63ode=1",
            "junk?token",
            "https://a.test/#/x",
        ] {
            let once = redact_url(u);
            assert_eq!(redact_url(&once), once);
        }
    }

    #[test]
    fn huge_input_is_linear() {
        let t = std::time::Instant::now();
        let many = format!("{B}?{}", "a=1&".repeat(2_000_000));
        assert_eq!(redact_url(&many), many);
        let many_secret = format!("{B}?{}code=1", "a=1&".repeat(2_000_000));
        assert_eq!(redact_url(&many_secret), B);
        let escapes = format!("{B}?{}=1", "%41".repeat(3_000_000));
        assert_eq!(redact_url(&escapes), escapes);
        let hashes = format!("{B}{}", "#".repeat(3_000_000));
        assert_eq!(redact_url(&hashes), hashes);
        let amps = format!("{B}#{}", "&".repeat(3_000_000));
        assert_eq!(redact_url(&amps), amps);
        let qs = format!("{B}{}", "?".repeat(3_000_000));
        assert_eq!(redact_url(&qs), qs);
        assert!(t.elapsed().as_secs() < 10, "{:?}", t.elapsed());
    }

    #[test]
    fn the_list_is_what_the_spec_says() {
        for n in [
            "code", "token", "access_token", "id_token", "refresh_token", "auth", "authorization", "session", "sessionid",
            "sid", "key", "api_key", "apikey", "password", "secret",
        ] {
            assert!(SECRET_PARAMS.contains(&n), "{n}");
        }
        assert!(SECRET_PARAMS.iter().all(|n| n.is_ascii() && *n == n.to_ascii_lowercase()));
    }

    #[test]
    fn google_sign_in_urls_keep_host_and_path_only() {
        let g = "https://accounts.google.com/v3/signin/identifier";
        for u in [
            format!("{g}?continue=https%3A%2F%2Fwww.youtube.com%2Fsignin%3Faction_handle_signin%3Dtrue&ec=65620&hl=da&flowName=GlifWebSignIn&flowEntry=ServiceLogin&service=youtube&pstMsg=1&ifkv=AaSxoQabc&dsh=S-123456&TL=AM3xyz&cid=1"),
            format!("{g}?flowName=GlifWebSignIn&flowEntry=ServiceLogin#anything"),
            format!("{g}?"),
        ] {
            let r = redact_url(&u);
            assert!(r == g || r == format!("{g}?"), "{r}");
        }
        // The names alone also trigger, outside a sign-in path.
        for n in ["ifkv", "dsh", "TL", "rapt", "gxf", "osid"] {
            assert_eq!(redact_url(&format!("https://x.test/p?hl=da&{n}=v&b=1")), "https://x.test/p");
        }
        assert_eq!(redact_url("https://x.test/p?hl=da&cid=1&authuser=0&flowName=Glif"), "https://x.test/p?hl=da&cid=1&authuser=0&flowName=Glif");
    }

    #[test]
    fn sign_in_paths_drop_the_query() {
        for path in ["/signin/x", "/v3/SignIn/identifier", "/login", "/login.php", "/user/login/step", "/oauth2/v2/auth", "/oauth", "/authorize", "/o/authorize", "/auth/callback", "/sso", "/sso/saml", "/a/b/sso?"] {
            let u = format!("https://h.test{path}");
            let want = u.trim_end_matches('?').to_string();
            assert_eq!(redact_url(&format!("{u}?hl=da&x=1#top")).trim_end_matches('?'), want, "{path}");
        }
        // Not a flow path: a host or query text does not count, nor a near miss.
        for u in [
            "https://login.example.com/home?x=1",
            "https://h.test/p?next=/login",
            "https://h.test/signing/x?x=1",
            "https://h.test/blog/author?x=1",
            "https://h.test/#/login",
            "https://h.test/p#/signin/x",
        ] {
            assert_eq!(redact_url(u), u, "{u}");
        }
        // Without a query or fragment the address is unchanged.
        assert_eq!(redact_url("https://h.test/login"), "https://h.test/login");
    }

    #[test]
    fn url_valued_parameters_are_checked_recursively() {
        let b = "https://h.test/p";
        // The inner address carries a credential: the parameter goes, the rest stays.
        assert_eq!(redact_url(&format!("{b}?a=1&continue=https%3A%2F%2Fapp%2Fcb%3Fcode%3Dx&b=2")), format!("{b}?a=1&b=2"));
        assert_eq!(redact_url(&format!("{b}?redirect_uri=https%3A%2F%2Fapp%2Fcb%23access_token%3Dx")), b);
        assert_eq!(redact_url(&format!("{b}?next=https://app/cb?token=x&z=1")), format!("{b}?z=1"));
        assert_eq!(redact_url(&format!("{b}?URL=https%3A%2F%2Fapp%2Flogin%3Fx%3D1&z=1")), format!("{b}?z=1"));
        assert_eq!(redact_url(&format!("{b}?return_to=https%3A%2F%2Fapp%2Fcb%3Fcode%3Dx#frag")), format!("{b}#frag"));
        // Twice encoded and nested.
        assert_eq!(
            redact_url(&format!("{b}?continue=https%3A%2F%2Fa.test%2Fp%3Fnext%3Dhttps%253A%252F%252Fb.test%252Fcb%253Fcode%253Dx&z=1")),
            format!("{b}?z=1")
        );
        // Harmless inner addresses, relative values and non-URL values stay.
        for u in [
            format!("{b}?continue=https%3A%2F%2Fwww.youtube.com%2Fsignin%3Faction_handle_signin%3Dtrue&z=1"),
            format!("{b}?next=/account/home&z=1"),
            format!("{b}?url=https%3A%2F%2Fexample.com%2Fa%3Fq%3D1"),
            format!("{b}?continue=&z=1"),
            format!("{b}?continue"),
        ] {
            assert_eq!(redact_url(&u), u, "{u}");
        }
        // Idempotent.
        let once = redact_url(&format!("{b}?a=1&continue=https%3A%2F%2Fapp%2Fcb%3Fcode%3Dx"));
        assert_eq!(redact_url(&once), once);
    }
}
