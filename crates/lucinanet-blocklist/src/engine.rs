/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The rule set: loading, the token index and the verdict.

use std::cell::Cell;
use std::collections::HashMap;

use crate::domain;
use crate::parse::{Filter, Parsed, parse_line};
use crate::pattern::{Anchor, El};
use crate::{ParseReport, Request, Verdict};

/// Salt that keeps host-label keys apart from plain token keys.
const LABEL_SALT: u64 = 0x9e37_79b9_7f4a_7c15;

fn hash(bytes: impl IntoIterator<Item = u8>, salt: u64) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ salt;
    for b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'%'
}

/// Index keys a filter can be found by: runs of token characters whose neighbours in the pattern force the same run
/// boundaries in any URL it matches (a separator, a non-token literal, a start or end anchor). A run next to `*` or an
/// unanchored edge could be a piece of a longer run in the URL, so it is not a key. Returns (hash, run length).
fn pattern_keys(f: &Filter) -> Vec<u64> {
    let p = &f.pattern;
    let mut keys = Vec::new();
    for (si, seg) in p.segs.iter().enumerate() {
        let tok = |i: usize| matches!(seg[i], El::Lit(b) if is_token_byte(b.to_ascii_lowercase()));
        let mut i = 0;
        while i < seg.len() {
            if !tok(i) {
                i += 1;
                continue;
            }
            let st = i;
            while i < seg.len() && tok(i) {
                i += 1;
            }
            let left_ok = st > 0 || (si == 0 && !p.left_open && p.anchor != Anchor::None);
            let right_ok = i < seg.len() || (si + 1 == p.segs.len() && !p.right_open && p.end_anchor);
            if !(left_ok && right_ok) {
                continue;
            }
            let run = || {
                seg[st..i].iter().filter_map(|e| match e {
                    El::Lit(b) => Some(b.to_ascii_lowercase()),
                    El::Sep => None,
                })
            };
            if i - st >= 3 {
                keys.push(hash(run(), 0));
            }
            if si == 0 && st == 0 && p.anchor == Anchor::Domain {
                keys.push(hash(run(), LABEL_SALT));
            }
        }
    }
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// A rule set. Build it once with [`Engine::from_text`] or [`Engine::from_lines`]; `check` is then read-only apart from a
/// counter, so one engine is used from one thread at a time.
pub struct Engine {
    rules: Vec<String>,
    filters: Vec<Filter>,
    index: HashMap<u64, Vec<usize>>,
    /// Filters with no usable key.
    always: Vec<usize>,
    /// `@@...$document` filters, which also exempt the pages their pattern names.
    page_exemptions: Vec<usize>,
    candidates: Cell<usize>,
}

impl Engine {
    /// An engine with no rules: everything is allowed.
    pub fn empty() -> Engine {
        Engine {
            rules: Vec::new(),
            filters: Vec::new(),
            index: HashMap::new(),
            always: Vec::new(),
            page_exemptions: Vec::new(),
            candidates: Cell::new(0),
        }
    }

    /// Parses every line (one filter per item) and indexes the accepted ones. Lines that are not network filters are
    /// counted in the report, never an error; rule numbers in verdicts follow the order of the accepted lines.
    pub fn from_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> (Engine, ParseReport) {
        let mut report = ParseReport::default();
        let mut engine = Engine::empty();
        for (n, line) in lines.into_iter().enumerate() {
            let n = n + 1;
            match parse_line(line) {
                Parsed::Blank => {}
                Parsed::Comment => report.comments += 1,
                Parsed::Cosmetic => report.cosmetic_skipped += 1,
                Parsed::Unsupported(reason) => report.unsupported.push((n, reason)),
                Parsed::Invalid(reason) => report.invalid.push((n, reason)),
                Parsed::Filter(f) => {
                    report.accepted += 1;
                    engine.rules.push(f.text.clone());
                    engine.filters.push(*f);
                }
            }
        }
        engine.build_index();
        (engine, report)
    }

    /// Like [`Engine::from_lines`] over a list file: split on `\n`, a trailing `\r` is trimmed.
    pub fn from_text(text: &str) -> (Engine, ParseReport) {
        Engine::from_lines(text.split('\n').map(|l| l.trim_end_matches('\r')))
    }

    /// Each filter goes under its rarest key, so a key shared by thousands of rules (a TLD, "example") is not the one
    /// requests are looked up by.
    fn build_index(&mut self) {
        let keys: Vec<Vec<u64>> = self.filters.iter().map(pattern_keys).collect();
        let mut freq: HashMap<u64, u32> = HashMap::new();
        for k in keys.iter().flatten() {
            *freq.entry(*k).or_insert(0) += 1;
        }
        for (i, ks) in keys.iter().enumerate() {
            if self.filters[i].is_page_exemption() {
                self.page_exemptions.push(i);
            }
            match ks.iter().min_by_key(|k| freq.get(k).copied().unwrap_or(0)) {
                Some(k) => self.index.entry(*k).or_default().push(i),
                None => self.always.push(i),
            }
        }
    }

    /// The filter text of every accepted rule, in input order (for a "blocked by" report).
    pub fn rules(&self) -> &[String] {
        &self.rules
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Number of candidate rules the last `check` examined (for the index test).
    pub fn last_candidates(&self) -> usize {
        self.candidates.get()
    }

    /// Judges one request. Order: a matching `important` block wins; else a matching exception; else a block; else allow.
    /// Among matches of one class the rule listed first is reported. An exception that matches with nothing to override
    /// gives `Allow`.
    ///
    /// A `@@...$document` filter is also a page exemption: when its pattern matches `https://<source_host>/`, every request
    /// made by that page is exempt (type and third-party options of that filter are not applied to those requests; its
    /// `domain=` option still is).
    pub fn check(&self, request: &Request<'_>) -> Verdict {
        let url = request.url.split('#').next().unwrap_or("");
        let lower = url.to_ascii_lowercase();
        let host = domain::host_range(&lower);
        let source = request.source_host.map(str::to_ascii_lowercase);
        let third_party = request.third_party.unwrap_or_else(|| match (host, source.as_deref()) {
            (Some((s, e)), Some(src)) => domain::is_third_party(&lower[s..e], src),
            _ => false,
        });

        let mut cand: Vec<usize> = self.always.clone();
        let mut keys: Vec<u64> = Vec::new();
        let lb = lower.as_bytes();
        let mut i = 0;
        while i < lb.len() {
            if !is_token_byte(lb[i]) {
                i += 1;
                continue;
            }
            let st = i;
            while i < lb.len() && is_token_byte(lb[i]) {
                i += 1;
            }
            if i - st >= 3 {
                keys.push(hash(lb[st..i].iter().copied(), 0));
            }
            if let Some((hs, he)) = host
                && st >= hs
                && st < he
                && (st == hs || lb[st - 1] == b'.')
            {
                keys.push(hash(lb[st..i].iter().copied(), LABEL_SALT));
            }
        }
        keys.sort_unstable();
        keys.dedup();
        for k in keys {
            if let Some(v) = self.index.get(&k) {
                cand.extend_from_slice(v);
            }
        }
        self.candidates.set(cand.len());

        let orig = url.as_bytes();
        let mut important: Option<usize> = None;
        let mut block: Option<usize> = None;
        let mut exception: Option<usize> = None;
        let min = |slot: &mut Option<usize>, i: usize| {
            if slot.is_none_or(|c| i < c) {
                *slot = Some(i);
            }
        };
        for &i in &cand {
            let f = &self.filters[i];
            if !f.type_ok(request.resource) || !f.party_ok(third_party) || !f.domains_ok(source.as_deref()) {
                continue;
            }
            let text = if f.match_case { orig } else { lb };
            if !f.pattern.is_match(text, host) {
                continue;
            }
            match (f.exception, f.important) {
                (true, _) => min(&mut exception, i),
                (false, true) => min(&mut important, i),
                (false, false) => min(&mut block, i),
            }
        }
        if let Some(src) = source.as_deref() {
            let page = format!("https://{src}/");
            let page_host = domain::host_range(&page);
            for &i in &self.page_exemptions {
                let f = &self.filters[i];
                if f.domains_ok(Some(src)) && f.pattern.is_match(page.as_bytes(), page_host) {
                    min(&mut exception, i);
                }
            }
        }

        if let Some(rule) = important {
            return Verdict::Block { rule };
        }
        match (exception, block) {
            (Some(rule), Some(overridden)) => Verdict::Exception { rule, overridden },
            (Some(_), None) => Verdict::Allow,
            (None, Some(rule)) => Verdict::Block { rule },
            (None, None) => Verdict::Allow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResourceType as T;

    fn eng(lines: &[&str]) -> Engine {
        Engine::from_lines(lines.iter().copied()).0
    }

    fn chk(e: &Engine, url: &str, src: Option<&str>, t: T) -> Verdict {
        e.check(&Request { url, source_host: src, resource: t, third_party: None })
    }

    fn url(e: &Engine, url: &str) -> Verdict {
        chk(e, url, None, T::Script)
    }

    const BLOCK0: Verdict = Verdict::Block { rule: 0 };

    #[test]
    fn domain_anchor_matches_host_and_subdomains() {
        let e = eng(&["||ads.com^"]);
        assert_eq!(url(&e, "https://ads.com/"), BLOCK0);
        assert_eq!(url(&e, "https://x.ads.com/y?z=1"), BLOCK0);
        assert_eq!(url(&e, "http://ADS.com"), BLOCK0);
        assert_eq!(url(&e, "https://a.b.ads.com:8080/x"), BLOCK0);
    }

    #[test]
    fn domain_anchor_does_not_match_inside_other_host() {
        let e = eng(&["||ads.com^"]);
        assert_eq!(url(&e, "https://badads.com/"), Verdict::Allow);
        assert_eq!(url(&e, "https://ads.community/"), Verdict::Allow);
    }

    #[test]
    fn domain_anchor_does_not_match_in_path() {
        let e = eng(&["||ads.com^"]);
        assert_eq!(url(&e, "https://example.org/?u=ads.com"), Verdict::Allow);
        assert_eq!(url(&e, "https://example.org/ads.com/x"), Verdict::Allow);
        assert_eq!(url(&e, "https://user:ads.com@example.org/"), Verdict::Allow);
    }

    #[test]
    fn separator_matches_slash_query_and_end() {
        let e = eng(&["x.test/ads^"]);
        for u in ["https://x.test/ads/", "https://x.test/ads?q=1", "https://x.test/ads", "https://x.test/ads:80", "https://x.test/ads="] {
            assert_eq!(url(&e, u), BLOCK0, "{u}");
        }
    }

    #[test]
    fn separator_does_not_match_letters_or_dot() {
        let e = eng(&["x.test/ads^"]);
        for u in ["https://x.test/adsx", "https://x.test/ads.js", "https://x.test/ads-1", "https://x.test/ads_1", "https://x.test/ads%41", "https://x.test/ads7"] {
            assert_eq!(url(&e, u), Verdict::Allow, "{u}");
        }
    }

    #[test]
    fn start_and_end_anchors() {
        let e = eng(&["|https://a.test/x|", "swf|"]);
        assert_eq!(url(&e, "https://a.test/x"), BLOCK0);
        assert_eq!(url(&e, "https://a.test/x?1"), Verdict::Allow);
        assert_eq!(url(&e, "https://a.test/y/x"), Verdict::Allow);
        assert_eq!(url(&e, "https://b.test/a.swf"), Verdict::Block { rule: 1 });
        assert_eq!(url(&e, "https://b.test/a.swf?x"), Verdict::Allow);
        let s = eng(&["|http://"]);
        assert_eq!(url(&s, "http://a.test/"), BLOCK0);
        assert_eq!(url(&s, "https://a.test/?u=http://b"), Verdict::Allow);
    }

    #[test]
    fn wildcard_in_middle() {
        let e = eng(&["/ban*ner.gif"]);
        assert_eq!(url(&e, "https://a.test/ban/zz/ner.gif"), BLOCK0);
        assert_eq!(url(&e, "https://a.test/banner.gif"), BLOCK0);
        assert_eq!(url(&e, "https://a.test/banner.png"), Verdict::Allow);
        assert_eq!(url(&e, "https://a.test/ner.gif/ban"), Verdict::Allow);
    }

    #[test]
    fn wildcards_do_not_blow_up_on_long_input() {
        let e = eng(&["*a*a*a*a*b", "||a.test/*a*a*a*a*a*b|"]);
        let long = format!("https://a.test/{}", "a".repeat(10_000));
        assert_eq!(url(&e, &long), Verdict::Allow);
        let ending_b = format!("{long}b");
        assert_eq!(url(&e, &ending_b), BLOCK0);
    }

    #[test]
    fn plain_substring_matches_anywhere() {
        let e = eng(&["/ads/banner"]);
        assert_eq!(url(&e, "https://a.test/x/ads/banner/1"), BLOCK0);
        assert_eq!(url(&e, "https://a.test/?p=/ads/banner"), BLOCK0);
        assert_eq!(url(&e, "https://a.test/ads/banne"), Verdict::Allow);
    }

    #[test]
    fn case_insensitive_by_default() {
        let e = eng(&["/AdBanner", "||Ads.Test^"]);
        assert_eq!(url(&e, "https://a.test/adbanner"), BLOCK0);
        assert_eq!(url(&e, "https://a.test/ADBANNER"), BLOCK0);
        assert_eq!(url(&e, "https://ADS.test/"), Verdict::Block { rule: 1 });
    }

    #[test]
    fn match_case_is_case_sensitive() {
        let e = eng(&["/AdBanner$match-case"]);
        assert_eq!(url(&e, "https://a.test/AdBanner"), BLOCK0);
        assert_eq!(url(&e, "https://a.test/adbanner"), Verdict::Allow);
        assert_eq!(url(&e, "https://a.test/ADBANNER"), Verdict::Allow);
    }

    #[test]
    fn third_party_option_with_given_flag() {
        let e = eng(&["||a.test^$third-party"]);
        let q = |tp| Request { url: "https://a.test/", source_host: Some("a.test"), resource: T::Script, third_party: Some(tp) };
        assert_eq!(e.check(&q(true)), BLOCK0);
        assert_eq!(e.check(&q(false)), Verdict::Allow);
    }

    #[test]
    fn third_party_decided_by_hosts() {
        let e = eng(&["||example.com^$third-party", "||other.co.uk^$3p"]);
        assert_eq!(chk(&e, "https://example.com/x", Some("cdn.example.com"), T::Script), Verdict::Allow);
        assert_eq!(chk(&e, "https://example.com/x", Some("elsewhere.org"), T::Script), BLOCK0);
        assert_eq!(chk(&e, "https://other.co.uk/x", Some("example.co.uk"), T::Script), Verdict::Block { rule: 1 });
        assert_eq!(chk(&e, "https://www.other.co.uk/x", Some("other.co.uk"), T::Script), Verdict::Allow);
        // The page load itself has no source: first party.
        assert_eq!(chk(&e, "https://example.com/", None, T::Script), Verdict::Allow);
    }

    #[test]
    fn first_party_option() {
        let e = eng(&["||a.test^$~third-party"]);
        assert_eq!(chk(&e, "https://a.test/", Some("www.a.test"), T::Image), BLOCK0);
        assert_eq!(chk(&e, "https://a.test/", Some("b.test"), T::Image), Verdict::Allow);
        assert_eq!(chk(&e, "https://a.test/", None, T::Image), BLOCK0);
    }

    #[test]
    fn type_option_script_only() {
        let e = eng(&["||a.test^$script"]);
        assert_eq!(chk(&e, "https://a.test/", None, T::Script), BLOCK0);
        assert_eq!(chk(&e, "https://a.test/", None, T::Image), Verdict::Allow);
    }

    #[test]
    fn negated_type_excludes() {
        let e = eng(&["||a.test^$~script"]);
        assert_eq!(chk(&e, "https://a.test/", None, T::Script), Verdict::Allow);
        assert_eq!(chk(&e, "https://a.test/", None, T::Image), BLOCK0);
        assert_eq!(chk(&e, "https://a.test/", None, T::Document), Verdict::Allow);
    }

    #[test]
    fn default_types_exclude_document() {
        let e = eng(&["||a.test^", "||b.test^$document"]);
        assert_eq!(chk(&e, "https://a.test/", None, T::Document), Verdict::Allow);
        assert_eq!(chk(&e, "https://a.test/", None, T::Subdocument), BLOCK0);
        assert_eq!(chk(&e, "https://b.test/", None, T::Document), Verdict::Block { rule: 1 });
        assert_eq!(chk(&e, "https://b.test/", None, T::Script), Verdict::Allow);
    }

    #[test]
    fn domain_option_applies_to_source_host_and_parents() {
        let e = eng(&["||a.test^$domain=pub.com|~sub.pub.com"]);
        assert_eq!(chk(&e, "https://a.test/", Some("pub.com"), T::Script), BLOCK0);
        assert_eq!(chk(&e, "https://a.test/", Some("www.pub.com"), T::Script), BLOCK0);
        assert_eq!(chk(&e, "https://a.test/", Some("WWW.Pub.com"), T::Script), BLOCK0);
        assert_eq!(chk(&e, "https://a.test/", Some("sub.pub.com"), T::Script), Verdict::Allow);
        assert_eq!(chk(&e, "https://a.test/", Some("x.sub.pub.com"), T::Script), Verdict::Allow);
        assert_eq!(chk(&e, "https://a.test/", Some("other.com"), T::Script), Verdict::Allow);
        assert_eq!(chk(&e, "https://a.test/", Some("xpub.com"), T::Script), Verdict::Allow);
    }

    #[test]
    fn domain_option_none_source_host() {
        let inc = eng(&["||a.test^$domain=pub.com"]);
        assert_eq!(chk(&inc, "https://a.test/", None, T::Script), Verdict::Allow);
        let exc = eng(&["||a.test^$domain=~pub.com"]);
        assert_eq!(chk(&exc, "https://a.test/", None, T::Script), BLOCK0);
        assert_eq!(chk(&exc, "https://a.test/", Some("pub.com"), T::Script), Verdict::Allow);
    }

    #[test]
    fn exception_overrides_block() {
        let e = eng(&["||a.test^", "@@||a.test/ok^"]);
        assert_eq!(url(&e, "https://a.test/ok/1"), Verdict::Exception { rule: 1, overridden: 0 });
        assert_eq!(url(&e, "https://a.test/bad"), BLOCK0);
    }

    #[test]
    fn exception_alone_is_allow() {
        let e = eng(&["@@||a.test^"]);
        assert_eq!(url(&e, "https://a.test/"), Verdict::Allow);
    }

    #[test]
    fn important_beats_exception() {
        let e = eng(&["||a.test^$important", "@@||a.test^"]);
        assert_eq!(url(&e, "https://a.test/"), BLOCK0);
        let both = eng(&["||a.test^", "||a.test^$important", "@@||a.test^"]);
        assert_eq!(url(&both, "https://a.test/"), Verdict::Block { rule: 1 });
    }

    #[test]
    fn first_listed_rule_is_reported() {
        let e = eng(&["||a.test/x", "||a.test^", "/x"]);
        assert_eq!(url(&e, "https://a.test/x"), BLOCK0);
        let e = eng(&["/x", "||a.test^", "||a.test/x"]);
        assert_eq!(url(&e, "https://a.test/x"), BLOCK0);
        let e = eng(&["||a.test^", "@@/x", "@@||a.test/x", "||a.test/x"]);
        assert_eq!(url(&e, "https://a.test/x"), Verdict::Exception { rule: 1, overridden: 0 });
    }

    #[test]
    fn document_exception_exempts_page() {
        let e = eng(&["||ads.test^", "@@||pub.com^$document"]);
        let ex = Verdict::Exception { rule: 1, overridden: 0 };
        assert_eq!(chk(&e, "https://ads.test/a.js", Some("pub.com"), T::Script), ex);
        assert_eq!(chk(&e, "https://ads.test/a.png", Some("www.pub.com"), T::Image), ex);
        assert_eq!(chk(&e, "https://ads.test/a.js", Some("other.com"), T::Script), BLOCK0);
        assert_eq!(chk(&e, "https://ads.test/a.js", Some("xpub.com"), T::Script), BLOCK0);
        assert_eq!(chk(&e, "https://ads.test/a.js", None, T::Script), BLOCK0);
        // The page load itself: the exception names pub.com, and rule 0 does not apply to documents.
        assert_eq!(chk(&e, "https://pub.com/", None, T::Document), Verdict::Allow);
        // `domain=` of the exemption still applies.
        let d = eng(&["||ads.test^", "@@||pub.com^$document,domain=only.org"]);
        assert_eq!(chk(&d, "https://ads.test/a.js", Some("pub.com"), T::Script), BLOCK0);
    }

    #[test]
    fn rules_returns_filter_text_in_order() {
        let (e, _) = Engine::from_text("! c\n  ||a.test^$script  \r\n\n@@||b.test^\n##.x\n/c/*");
        assert_eq!(e.rules(), ["||a.test^$script", "@@||b.test^", "/c/*"]);
        assert_eq!(e.len(), 3);
        assert!(!e.is_empty());
        assert!(Engine::empty().is_empty());
    }

    #[test]
    fn index_examines_few_candidates() {
        let lines: Vec<String> = (0..50_000).map(|i| format!("||{i}.example.com^")).collect();
        let (e, r) = Engine::from_lines(lines.iter().map(String::as_str));
        assert_eq!(r.accepted, 50_000);
        for i in [0usize, 7, 31_337, 49_999] {
            let u = format!("https://sub.{i}.example.com/a/b.js?x=1");
            assert_eq!(url(&e, &u), Verdict::Block { rule: i }, "{u}");
            assert!(e.last_candidates() < 100, "{} candidates for {u}", e.last_candidates());
        }
        assert_eq!(url(&e, "https://50000.example.com/"), Verdict::Allow);
        assert!(e.last_candidates() < 100);
        assert_eq!(url(&e, "https://www.wikipedia.org/wiki/Rust"), Verdict::Allow);
        assert!(e.last_candidates() < 100);

        let words: Vec<String> = (0..50_000).map(|i| format!("/zq{i}track/*.gif")).collect();
        let (w, _) = Engine::from_lines(words.iter().map(String::as_str));
        assert_eq!(url(&w, "https://a.test/zq4242track/p.gif"), Verdict::Block { rule: 4242 });
        assert!(w.last_candidates() < 100, "{}", w.last_candidates());
    }

    #[test]
    fn ip_host_and_port_requests() {
        let e = eng(&["||192.168.0.1^", "||10.0.0.5:8080^"]);
        assert_eq!(url(&e, "http://192.168.0.1:8080/x"), BLOCK0);
        assert_eq!(url(&e, "http://192.168.0.1/"), BLOCK0);
        assert_eq!(url(&e, "http://192.168.0.10/"), Verdict::Allow);
        assert_eq!(url(&e, "http://[::1]:3000/"), Verdict::Allow);
        assert_eq!(url(&e, "http://10.0.0.5:8080/p"), Verdict::Block { rule: 1 });
        let v6 = eng(&["||[::1]^"]);
        assert_eq!(url(&v6, "http://[::1]:3000/"), BLOCK0);
        // Third-party by IP: whole address compared.
        let t = eng(&["||10.0.0.5^$third-party"]);
        assert_eq!(chk(&t, "http://10.0.0.5/", Some("10.0.0.6"), T::Script), BLOCK0);
        assert_eq!(chk(&t, "http://10.0.0.5/", Some("10.0.0.5"), T::Script), Verdict::Allow);
    }

    #[test]
    fn fragment_is_ignored() {
        let e = eng(&["/ads|", "/frag"]);
        assert_eq!(url(&e, "https://a.test/ads#x"), BLOCK0);
        assert_eq!(url(&e, "https://a.test/#/frag"), Verdict::Allow);
    }

    #[test]
    fn no_panics_on_odd_inputs() {
        let odd = [
            "||", "|", "$", "@@", "@@@@", "^^^", "*", "**", "|*|", "||*", "||^", "%", "$$$$", "$domain=", "$domain=~", "$,,,", "~", "~$~",
            "||a.com^$~", "!", "[", "# ", "#", "##", "ünï||cødé^*", "||ünï.test^", "\u{0}", "\t||a.com^\t", "/", "//", "/*/", "@@/$script",
            "||a.com^$domain=a|b|", "$important,match-case,script,~script",
        ];
        let long = "a".repeat(10_000);
        let long_wild = "*a".repeat(5_000);
        let mut lines: Vec<&str> = odd.to_vec();
        lines.push(&long);
        lines.push(&long_wild);
        let mut seed = 0x1234_5678_9abc_def0u64;
        let alphabet: Vec<char> = "|$@^*/.-_%~=,#!aAz09 :éÿ".chars().collect();
        let generated: Vec<String> = (0..2_000)
            .map(|_| {
                (0..12)
                    .map(|_| {
                        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                        alphabet[(seed >> 33) as usize % alphabet.len()]
                    })
                    .collect()
            })
            .collect();
        lines.extend(generated.iter().map(String::as_str));
        let (e, _) = Engine::from_lines(lines);
        let urls = [
            "", "http://", "https://a.com", "https://a.com/%", "https://a.com/%zz?%", "http://[", "http://[::1", "ws://x@", "https://ü.test/ï?ÿ#é",
            "HTTPS://A.COM/A", "about:blank", "https://a.com/a#", "https://@/", "https://:@:/", "https://a.com:/",
        ];
        let long_url = format!("https://a.com/{}", "ab%".repeat(4_000));
        for u in urls.iter().copied().chain(std::iter::once(long_url.as_str())) {
            for src in [None, Some(""), Some("a.com"), Some("[::1]"), Some("ünï.test")] {
                for t in [T::Document, T::Script, T::Other] {
                    let _ = chk(&e, u, src, t);
                }
            }
        }
    }
}
