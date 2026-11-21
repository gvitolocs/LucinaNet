/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! What Enter does with the text in the address bar: open an address, search, or run a keyword search.

use std::net::Ipv6Addr;

use url::Url;

use crate::fixup::fix_scheme_typos;

/// A search engine: a name, an optional keyword that selects it ("w rust"), and a URL template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchEngine {
    /// Display name.
    pub name: String,
    /// Typed first word that selects this engine, matched case-insensitively.
    pub keyword: Option<String>,
    /// URL with `{searchTerms}` (Chromium) or `%s` (Firefox) where the percent-encoded terms go.
    pub template: String,
}

impl SearchEngine {
    /// The search URL for `terms`. Terms are encoded like `application/x-www-form-urlencoded` (space as `+`,
    /// `&` as `%26`). A template that does not parse after filling falls back to the DuckDuckGo URL, so the result
    /// is always a usable address.
    pub fn url_for(&self, terms: &str) -> Url {
        let encoded: String = url::form_urlencoded::byte_serialize(terms.as_bytes()).collect();
        let filled = if self.template.contains("{searchTerms}") {
            self.template.replace("{searchTerms}", &encoded)
        } else {
            self.template.replace("%s", &encoded)
        };
        match Url::parse(&filled) {
            Ok(url) => url,
            Err(_) => fallback_search_url(&encoded),
        }
    }

    /// DuckDuckGo's HTML page, the address bar's search engine today.
    pub fn duckduckgo_html() -> SearchEngine {
        SearchEngine {
            name: "DuckDuckGo".to_owned(),
            keyword: None,
            template: "https://duckduckgo.com/html/?q={searchTerms}".to_owned(),
        }
    }
}

fn fallback_search_url(encoded: &str) -> Url {
    // `encoded` holds only unreserved characters and %XX, so this literal-prefixed URL always parses.
    Url::parse(&format!("https://duckduckgo.com/html/?q={encoded}")).expect("static URL with encoded query")
}

/// What `classify` needs besides the typed text.
pub struct Context<'a> {
    /// Engine for plain searches.
    pub default_engine: &'a SearchEngine,
    /// Engines that can be selected by a keyword.
    pub keywords: &'a [SearchEngine],
    /// Whether a single label such as `intranet` is a host rather than a search (default false).
    pub allow_single_label_hosts: bool,
}

/// What Enter does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Blank text: nothing to do.
    Empty,
    /// Open this address. `typed_scheme` is false when we added the scheme ourselves (the wiring may try http after a
    /// failed https load).
    Navigate { url: Url, typed_scheme: bool },
    /// Search with the default engine.
    Search { engine: SearchEngine, terms: String, url: Url },
    /// "keyword rest": a search with the engine named by the keyword.
    Keyword { engine: SearchEngine, terms: String, url: Url },
}

// Firefox browser/components/urlbar/UrlbarProviderHeuristicFallback.sys.mjs @33abb90 and docshell/base/URIFixup.sys.mjs
// @33abb90; Chromium components/omnibox/browser/autocomplete_input.cc @e9a4673 Parse(): the order of the tests is
// restrict/keyword, scheme, file path, host-like, search.
const NAVIGABLE_SCHEMES: [&str; 8] = ["http", "https", "file", "about", "view-source", "data", "ftp", "blob"];
/// Schemes whose URLs have an authority; without `://` after the name they are host:port or a search, not a URL.
const AUTHORITY_SCHEMES: [&str; 3] = ["http", "https", "ftp"];

/// Decides what Enter does with `typed`.
///
/// Order of tests: (a) a leading `?` forces a search of the rest; (b) a first word equal to an engine keyword with
/// text after it runs that engine (a lone keyword is a plain search); (c) text with a navigable scheme opens as typed
/// after [`fix_scheme_typos`], except `javascript:` which is searched; (d) an absolute path becomes a `file://` URL;
/// (e) host-like text gets `https://` (`http://` for `localhost` and IP literals); (f) everything else is a search.
pub fn classify(typed: &str, cx: &Context<'_>) -> Action {
    classify_at(typed, cx, 0)
}

/// Today's `to_url`: the address `classify` would open, `None` for blank text.
pub fn to_url(typed: &str, cx: &Context<'_>) -> Option<Url> {
    match classify(typed, cx) {
        Action::Empty => None,
        Action::Navigate { url, .. } | Action::Search { url, .. } | Action::Keyword { url, .. } => Some(url),
    }
}

fn classify_at(typed: &str, cx: &Context<'_>, depth: u8) -> Action {
    // Firefox URIFixup strips embedded newlines (single-line fields can still receive them from a paste).
    let cleaned: String = typed.chars().filter(|c| *c != '\r' && *c != '\n').collect();
    let text = cleaned.trim();
    if text.is_empty() {
        return Action::Empty;
    }
    if let Some(rest) = text.strip_prefix('?') {
        let terms = rest.trim();
        if terms.is_empty() {
            return Action::Empty;
        }
        return search(cx, terms);
    }
    if depth == 0
        && let Some(action) = keyword_action(text, cx)
    {
        return action;
    }
    let candidate = fix_scheme_typos(text);
    let candidate = candidate.as_deref().unwrap_or(text);
    if let Some(action) = scheme_action(candidate, text, cx, depth) {
        return action;
    }
    if text.starts_with('/')
        && !text.starts_with("//")
        && let Ok(mut url) = Url::parse("file:///")
    {
        url.set_path(text);
        return Action::Navigate { url, typed_scheme: true };
    }
    if let Some(action) = host_action(text, cx) {
        return action;
    }
    search(cx, text)
}

fn search(cx: &Context<'_>, terms: &str) -> Action {
    let engine = cx.default_engine.clone();
    let url = engine.url_for(terms);
    Action::Search { engine, terms: terms.to_owned(), url }
}

fn keyword_action(text: &str, cx: &Context<'_>) -> Option<Action> {
    let first_end = text.find(char::is_whitespace).unwrap_or(text.len());
    let first = text[..first_end].to_lowercase();
    let engine = cx.keywords.iter().find(|engine| {
        engine
            .keyword
            .as_deref()
            .is_some_and(|keyword| !keyword.is_empty() && keyword.to_lowercase() == first)
    })?;
    let terms = text[first_end..].trim();
    if terms.is_empty() {
        return Some(search(cx, text));
    }
    let url = engine.url_for(terms);
    Some(Action::Keyword { engine: engine.clone(), terms: terms.to_owned(), url })
}

/// `candidate` is the text after scheme-typo repair, `original` the text as typed (searched when the scheme is refused).
fn scheme_action(candidate: &str, original: &str, cx: &Context<'_>, depth: u8) -> Option<Action> {
    let name_end = candidate.find(':')?;
    let name = &candidate[..name_end];
    let mut chars = name.chars();
    if !chars.next()?.is_ascii_alphabetic() || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    let scheme = name.to_ascii_lowercase();
    // Chromium autocomplete_input.cc: javascript: is never run from the bar; Firefox strips it.
    if scheme == "javascript" {
        return Some(search(cx, original));
    }
    if !NAVIGABLE_SCHEMES.contains(&scheme.as_str()) {
        return None;
    }
    if AUTHORITY_SCHEMES.contains(&scheme.as_str()) && !candidate[name_end..].starts_with("://") {
        return None;
    }
    if scheme == "view-source" {
        return Some(view_source(candidate, name_end, original, cx, depth));
    }
    Some(match Url::parse(candidate) {
        Ok(url) => Action::Navigate { url, typed_scheme: true },
        Err(_) => search(cx, original),
    })
}

/// Chromium url_fixer.cc FixupURLInternal: fix the part after `view-source:` and put the prefix back; no recursion.
fn view_source(candidate: &str, name_end: usize, original: &str, cx: &Context<'_>, depth: u8) -> Action {
    let inner = candidate[name_end + 1..].trim();
    if depth > 0 || inner.to_ascii_lowercase().starts_with("view-source:") {
        return search(cx, original);
    }
    match classify_at(inner, cx, depth + 1) {
        Action::Navigate { url, typed_scheme } => match Url::parse(&format!("view-source:{url}")) {
            Ok(url) => Action::Navigate { url, typed_scheme },
            Err(_) => search(cx, original),
        },
        _ => search(cx, original),
    }
}

fn host_action(text: &str, cx: &Context<'_>) -> Option<Action> {
    // Firefox URIFixup: "//shorthand.example.com" is an address with a duff protocol.
    let body = text.strip_prefix("//").unwrap_or(text);
    let authority_end = body.find(['/', '?', '#']).unwrap_or(body.len());
    let (authority, tail) = body.split_at(authority_end);
    // Chromium Parse(): user@host without a scheme is more likely an email address than an HTTP login.
    if authority.is_empty() || authority.contains(char::is_whitespace) || authority.contains('@') {
        return None;
    }
    let (host, port) = split_host_port(authority)?;
    let plain_http = host_kind(host, port.is_some(), tail, cx)?;
    let scheme = if plain_http { "http" } else { "https" };
    let url = Url::parse(&format!("{scheme}://{body}")).ok()?;
    Some(Action::Navigate { url, typed_scheme: false })
}

/// `Some(true)` for a host that is opened over plain http (localhost, IP literals), `Some(false)` for a name that gets
/// https, `None` when the text is not host-like.
fn host_kind(host: &str, has_port: bool, tail: &str, cx: &Context<'_>) -> Option<bool> {
    if let Some(inner) = host.strip_prefix('[') {
        let inner = inner.strip_suffix(']')?;
        return inner.parse::<Ipv6Addr>().ok().map(|_| true);
    }
    let lower = host.to_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") {
        return Some(true);
    }
    if !lower.is_empty() && lower.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return numeric_host(&lower, tail);
    }
    let labels: Vec<&str> = lower.split('.').collect();
    if !labels.iter().all(|label| valid_label(label)) {
        return None;
    }
    if labels.len() >= 2 {
        return labels.last().is_some_and(|tld| valid_tld(tld)).then_some(false);
    }
    // Chromium Parse(): a single word is a search unless a port or a trailing slash says "address".
    (cx.allow_single_label_hosts || has_port || tail.ends_with('/')).then_some(false)
}

/// Chromium Parse(): four distinct components are an IP; two or three ("13.5", "3.14") are a query unless the text
/// ends in a slash; one is a number.
fn numeric_host(host: &str, tail: &str) -> Option<bool> {
    let parts: Vec<&str> = host.split('.').collect();
    let octets = |parts: &[&str]| parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok());
    match parts.len() {
        4 => octets(&parts).then_some(true),
        2 | 3 => (tail.ends_with('/') && octets(&parts)).then_some(true),
        _ => None,
    }
}

fn split_host_port(authority: &str) -> Option<(&str, Option<&str>)> {
    if authority.starts_with('[') {
        let close = authority.find(']')?;
        let host = &authority[..=close];
        return match &authority[close + 1..] {
            "" => Some((host, None)),
            after => valid_port(after.strip_prefix(':')?).then_some((host, Some(&after[1..]))),
        };
    }
    match authority.rfind(':') {
        None => Some((authority, None)),
        Some(i) => {
            let (host, port) = (&authority[..i], &authority[i + 1..]);
            (!host.contains(':') && valid_port(port)).then_some((host, Some(port)))
        }
    }
}

fn valid_port(port: &str) -> bool {
    !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u32>().is_ok_and(|p| p <= 65535)
}

/// A DNS label: letters, digits, `-`, `_`, not starting or ending with `-`, at most 63 characters. Non-ASCII letters
/// pass; the `url` crate turns them into punycode.
fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.chars().count() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// Stands in for Chromium's public-suffix lookup, which this crate has no list for: two or more letters, or a punycode
/// label. This is what keeps `3.14` and `file.v2` out of the host rule.
fn valid_tld(tld: &str) -> bool {
    tld.starts_with("xn--") || (tld.chars().count() >= 2 && tld.chars().all(char::is_alphabetic))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engines() -> (SearchEngine, Vec<SearchEngine>) {
        let wiki = SearchEngine {
            name: "Wikipedia".to_owned(),
            keyword: Some("w".to_owned()),
            template: "https://en.wikipedia.org/w/index.php?search={searchTerms}".to_owned(),
        };
        (SearchEngine::duckduckgo_html(), vec![wiki])
    }

    fn run(typed: &str) -> Action {
        let (default_engine, keywords) = engines();
        classify(typed, &Context { default_engine: &default_engine, keywords: &keywords, allow_single_label_hosts: false })
    }

    fn run_intranet(typed: &str) -> Action {
        let (default_engine, keywords) = engines();
        classify(typed, &Context { default_engine: &default_engine, keywords: &keywords, allow_single_label_hosts: true })
    }

    fn url_of(typed: &str) -> String {
        let (default_engine, keywords) = engines();
        let cx = Context { default_engine: &default_engine, keywords: &keywords, allow_single_label_hosts: false };
        to_url(typed, &cx).map(|u| u.to_string()).unwrap_or_default()
    }

    fn navigates(typed: &str, expected: &str, typed_scheme: bool) {
        match run(typed) {
            Action::Navigate { url, typed_scheme: got } => {
                assert_eq!(url.as_str(), expected, "{typed}");
                assert_eq!(got, typed_scheme, "{typed}");
            }
            other => panic!("{typed}: expected Navigate, got {other:?}"),
        }
    }

    fn searches(typed: &str, expected_terms: &str) {
        match run(typed) {
            Action::Search { terms, .. } => assert_eq!(terms, expected_terms, "{typed}"),
            other => panic!("{typed}: expected Search, got {other:?}"),
        }
    }

    #[test]
    fn empty_and_blank_are_empty() {
        for text in ["", "   ", "\t", "\n", " \r\n "] {
            assert_eq!(run(text), Action::Empty, "{text:?}");
        }
    }

    #[test]
    fn bare_host_gets_https_and_untyped_scheme() {
        navigates("example.com", "https://example.com/", false);
        navigates("  Example.COM  ", "https://example.com/", false);
        navigates("www.example.co.uk", "https://www.example.co.uk/", false);
        navigates("//shorthand.example.com", "https://shorthand.example.com/", false);
    }

    #[test]
    fn to_url_matches_todays_behaviour_host() {
        assert_eq!(url_of("example.com"), "https://example.com/");
    }

    #[test]
    fn to_url_matches_todays_behaviour_scheme_url() {
        assert_eq!(url_of("https://example.com/a"), "https://example.com/a");
        assert_eq!(url_of("https://a.b/c"), "https://a.b/c");
    }

    #[test]
    fn to_url_matches_todays_behaviour_about() {
        assert_eq!(url_of("about:blank"), "about:blank");
        navigates("about:blank", "about:blank", true);
    }

    #[test]
    fn to_url_matches_todays_behaviour_search() {
        assert_eq!(url_of("lucina os"), "https://duckduckgo.com/html/?q=lucina+os");
        assert_eq!(url_of("foo bar"), "https://duckduckgo.com/html/?q=foo+bar");
        let (default_engine, keywords) = engines();
        let cx = Context { default_engine: &default_engine, keywords: &keywords, allow_single_label_hosts: false };
        assert!(to_url("   ", &cx).is_none());
    }

    #[test]
    fn localhost_and_ports_use_http() {
        navigates("localhost", "http://localhost/", false);
        navigates("localhost:8080", "http://localhost:8080/", false);
        navigates("localhost:3000/app?x=1", "http://localhost:3000/app?x=1", false);
        navigates("app.localhost", "http://app.localhost/", false);
        navigates("example.com:8443", "https://example.com:8443/", false);
        searches("example.com:99999", "example.com:99999");
        searches("example.com:", "example.com:");
    }

    #[test]
    fn ipv4_and_ipv6_literals_navigate() {
        navigates("192.168.1.1", "http://192.168.1.1/", false);
        navigates("10.0.0.1:8080/x", "http://10.0.0.1:8080/x", false);
        navigates("[::1]", "http://[::1]/", false);
        navigates("[::1]:3000", "http://[::1]:3000/", false);
        searches("999.1.1.1", "999.1.1.1");
        searches("[nonsense]", "[nonsense]");
    }

    #[test]
    fn host_with_path_query_fragment() {
        navigates("example.com/a/b?x=1#frag", "https://example.com/a/b?x=1#frag", false);
        navigates("example.com?q=1", "https://example.com/?q=1", false);
        navigates("example.com#top", "https://example.com/#top", false);
    }

    #[test]
    fn numbers_with_a_dot_are_searches() {
        searches("3.14", "3.14");
        searches("1.5", "1.5");
        searches("v1.2.3", "v1.2.3");
        // A trailing slash is an explicit "address" (Chromium Parse()).
        navigates("13.5/", "http://13.0.0.5/", false);
    }

    #[test]
    fn trailing_dot_and_double_dot_are_not_hosts() {
        for text in ["example.com.", "a..b", ".com", "hello.", "a.-b.com", "a. b"] {
            assert!(matches!(run(text), Action::Search { .. }), "{text}");
        }
    }

    #[test]
    fn single_label_is_search_unless_allowed() {
        searches("intranet", "intranet");
        match run_intranet("intranet") {
            Action::Navigate { url, typed_scheme } => {
                assert_eq!(url.as_str(), "https://intranet/");
                assert!(!typed_scheme);
            }
            other => panic!("{other:?}"),
        }
        // Chromium Parse(): a trailing slash or a port forces the address reading.
        navigates("intranet/", "https://intranet/", false);
        navigates("intranet:8080", "https://intranet:8080/", false);
    }

    #[test]
    fn spaces_make_a_search_even_with_a_dot() {
        searches("foo.com bar", "foo.com bar");
        searches("what is example.com", "what is example.com");
        searches("user@example.com", "user@example.com");
    }

    #[test]
    fn scheme_urls_navigate_as_typed() {
        navigates("https://a.b/c", "https://a.b/c", true);
        navigates("HTTP://Example.com/Path", "http://example.com/Path", true);
        navigates("ftp://files.example.com/x", "ftp://files.example.com/x", true);
        navigates("data:text/plain,hello", "data:text/plain,hello", true);
        navigates("view-source:example.com", "view-source:https://example.com/", false);
        navigates("file:///etc/hosts", "file:///etc/hosts", true);
        searches("https://", "https://");
        searches("view-source:view-source:example.com", "view-source:view-source:example.com");
        // A scheme the bar does not know ("site:") is a search operator, not an address.
        searches("site:example.com rust", "site:example.com rust");
    }

    #[test]
    fn scheme_typos_are_fixed() {
        navigates("http:/example.com", "http://example.com/", true);
        navigates("http:example.com", "http://example.com/", true);
        navigates("https//x.org", "https://x.org/", true);
        navigates("htps://x.org/a", "https://x.org/a", true);
    }

    #[test]
    fn javascript_is_not_navigated() {
        searches("javascript:alert(1)", "javascript:alert(1)");
        searches("JavaScript:void(0)", "JavaScript:void(0)");
    }

    #[test]
    fn file_paths_become_file_urls() {
        navigates("/usr/share", "file:///usr/share", true);
        navigates("/home/me/my docs/a#b.txt", "file:///home/me/my%20docs/a%23b.txt", true);
        // No home directory is known here, so "~/x" is a search.
        searches("~/x", "~/x");
    }

    #[test]
    fn question_mark_forces_search() {
        searches("?example.com", "example.com");
        searches("? foo bar", "foo bar");
        searches("?localhost", "localhost");
        assert_eq!(run("?"), Action::Empty);
    }

    #[test]
    fn keyword_runs_keyword_search() {
        match run("w rust lang") {
            Action::Keyword { engine, terms, url } => {
                assert_eq!(engine.name, "Wikipedia");
                assert_eq!(terms, "rust lang");
                assert_eq!(url.as_str(), "https://en.wikipedia.org/w/index.php?search=rust+lang");
            }
            other => panic!("{other:?}"),
        }
        // The keyword wins over the host rule for its rest.
        assert!(matches!(run("w example.com"), Action::Keyword { .. }));
    }

    #[test]
    fn lone_keyword_is_plain_search() {
        searches("w", "w");
        searches("w   ", "w");
    }

    #[test]
    fn keyword_is_case_insensitive() {
        match run("W Rust") {
            Action::Keyword { terms, .. } => assert_eq!(terms, "Rust"),
            other => panic!("{other:?}"),
        }
        // Only a whole first word counts.
        searches("wx rust", "wx rust");
    }

    #[test]
    fn unicode_host_is_idna_encoded() {
        navigates("münchen.de", "https://xn--mnchen-3ya.de/", false);
        navigates("münchen.de/straße", "https://xn--mnchen-3ya.de/stra%C3%9Fe", false);
    }

    #[test]
    fn search_url_encodes_terms() {
        match run("a&b c") {
            Action::Search { url, terms, .. } => {
                assert_eq!(terms, "a&b c");
                assert_eq!(url.as_str(), "https://duckduckgo.com/html/?q=a%26b+c");
            }
            other => panic!("{other:?}"),
        }
        searches("  padded  terms  ", "padded  terms");
    }

    #[test]
    fn engine_template_supports_both_placeholders() {
        let chromium = SearchEngine { name: "c".into(), keyword: None, template: "https://s.example/?q={searchTerms}&x=1".into() };
        let firefox = SearchEngine { name: "f".into(), keyword: None, template: "https://s.example/?q=%s&x=1".into() };
        assert_eq!(chromium.url_for("x y").as_str(), "https://s.example/?q=x+y&x=1");
        assert_eq!(firefox.url_for("x y").as_str(), "https://s.example/?q=x+y&x=1");
        // "%s" inside the terms is encoded, never taken for a placeholder.
        assert_eq!(firefox.url_for("100%s").as_str(), "https://s.example/?q=100%25s&x=1");
        let broken = SearchEngine { name: "b".into(), keyword: None, template: "not a url {searchTerms}".into() };
        assert_eq!(broken.url_for("a b").as_str(), "https://duckduckgo.com/html/?q=a+b");
    }
}
