/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! One filter line to a [`Filter`]. Anything the matcher cannot honour exactly is reported as unsupported and left out:
//! an option that is not understood must never turn into "block everything".

use crate::ResourceType;
use crate::pattern::Pattern;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Party {
    Any,
    Third,
    First,
}

#[derive(Clone, Debug)]
pub(crate) struct Filter {
    pub text: String,
    pub exception: bool,
    pub important: bool,
    pub match_case: bool,
    pub pattern: Pattern,
    /// Bit set of `type_bit`; zero means "no type option".
    pub include: u16,
    pub exclude: u16,
    pub party: Party,
    pub dom_inc: Vec<String>,
    pub dom_exc: Vec<String>,
}

pub(crate) enum Parsed {
    Blank,
    Comment,
    Cosmetic,
    Unsupported(String),
    Invalid(String),
    Filter(Box<Filter>),
}

pub(crate) fn type_bit(t: ResourceType) -> u16 {
    1 << (t as u16)
}

const DOCUMENT_BIT: u16 = 1 << (ResourceType::Document as u16);

fn type_by_name(name: &str) -> Option<ResourceType> {
    Some(match name {
        "script" => ResourceType::Script,
        "image" => ResourceType::Image,
        "stylesheet" | "css" => ResourceType::Stylesheet,
        "font" => ResourceType::Font,
        "media" => ResourceType::Media,
        "xmlhttprequest" | "xhr" => ResourceType::XmlHttpRequest,
        "subdocument" | "frame" => ResourceType::Subdocument,
        "document" | "doc" => ResourceType::Document,
        "ping" => ResourceType::Ping,
        "websocket" => ResourceType::WebSocket,
        "object" => ResourceType::Object,
        "other" => ResourceType::Other,
        _ => return None,
    })
}

impl Filter {
    pub fn type_ok(&self, t: ResourceType) -> bool {
        let b = type_bit(t);
        if self.include != 0 {
            self.include & b != 0 && self.exclude & b == 0
        } else {
            b != DOCUMENT_BIT && self.exclude & b == 0
        }
    }

    pub fn party_ok(&self, third_party: bool) -> bool {
        match self.party {
            Party::Any => true,
            Party::Third => third_party,
            Party::First => !third_party,
        }
    }

    /// `source` is the lower-cased page host. A request with no page can only satisfy an exclude-only list.
    pub fn domains_ok(&self, source: Option<&str>) -> bool {
        if self.dom_inc.is_empty() && self.dom_exc.is_empty() {
            return true;
        }
        let Some(h) = source else { return self.dom_inc.is_empty() };
        if self.dom_exc.iter().any(|d| covers(d, h)) {
            return false;
        }
        self.dom_inc.is_empty() || self.dom_inc.iter().any(|d| covers(d, h))
    }

    /// An `@@` filter with `$document`: it exempts the whole page, see `Engine::check`.
    pub fn is_page_exemption(&self) -> bool {
        self.exception && self.include & DOCUMENT_BIT != 0
    }
}

/// `domain` is `host` or a parent of it.
fn covers(domain: &str, host: &str) -> bool {
    host == domain
        || (host.len() > domain.len() && host.ends_with(domain) && host.as_bytes()[host.len() - domain.len() - 1] == b'.')
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

enum OptErr {
    Unsupported(String),
    Invalid(String),
}

struct Options {
    include: u16,
    exclude: u16,
    party: Party,
    dom_inc: Vec<String>,
    dom_exc: Vec<String>,
    match_case: bool,
    important: bool,
}

fn parse_options(text: &str) -> Result<Options, OptErr> {
    let mut o = Options {
        include: 0,
        exclude: 0,
        party: Party::Any,
        dom_inc: Vec::new(),
        dom_exc: Vec::new(),
        match_case: false,
        important: false,
    };
    fn set_party(o: &mut Options, p: Party) -> Result<(), OptErr> {
        if o.party != Party::Any && o.party != p {
            return Err(OptErr::Invalid("conflicting party options".into()));
        }
        o.party = p;
        Ok(())
    }
    for raw in text.split(',') {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let (neg, raw) = match raw.strip_prefix('~') {
            Some(r) => (true, r),
            None => (false, raw),
        };
        let (name, value) = match raw.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (raw, None),
        };
        let name = name.to_ascii_lowercase();
        if value.is_none() {
            if let Some(t) = type_by_name(&name) {
                if neg {
                    o.exclude |= type_bit(t);
                } else {
                    o.include |= type_bit(t);
                }
                continue;
            }
            match name.as_str() {
                "third-party" | "3p" => {
                    set_party(&mut o, if neg { Party::First } else { Party::Third })?;
                    continue;
                }
                "first-party" | "1p" => {
                    set_party(&mut o, if neg { Party::Third } else { Party::First })?;
                    continue;
                }
                "match-case" if !neg => {
                    o.match_case = true;
                    continue;
                }
                "important" if !neg => {
                    o.important = true;
                    continue;
                }
                _ => {}
            }
        } else if name == "domain" && !neg {
            for d in value.unwrap_or("").split('|') {
                let d = d.trim().to_ascii_lowercase();
                let (neg, host) = match d.strip_prefix('~') {
                    Some(x) => (true, x),
                    None => (false, d.as_str()),
                };
                if host.is_empty() {
                    return Err(OptErr::Invalid("empty domain in domain=".into()));
                }
                if neg {
                    o.dom_exc.push(host.to_string());
                } else {
                    o.dom_inc.push(host.to_string());
                }
            }
            continue;
        }
        return Err(OptErr::Unsupported(name));
    }
    Ok(o)
}

pub(crate) fn parse_line(line: &str) -> Parsed {
    let line = line.trim();
    if line.is_empty() {
        return Parsed::Blank;
    }
    if line.starts_with('!') || starts_with_ci(line, "[adblock") || line.starts_with("# ") || line == "#" {
        return Parsed::Comment;
    }
    if ["##", "#@#", "#?#", "#$#", "#%#", "$$"].iter().any(|m| line.contains(m)) {
        return Parsed::Cosmetic;
    }
    let (exception, rest) = match line.strip_prefix("@@") {
        Some(r) => (true, r),
        None => (false, line),
    };
    let (pat_text, opts_text) = match rest.rfind('$') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    let is_regex = |s: &str| s.len() >= 2 && s.starts_with('/') && s.ends_with('/');
    if is_regex(pat_text) || is_regex(rest) {
        return Parsed::Unsupported("regex".into());
    }
    let opts_text = opts_text.map(str::trim).filter(|s| !s.is_empty());
    let opts = match opts_text.map(parse_options).transpose() {
        Ok(o) => o,
        Err(OptErr::Unsupported(n)) => return Parsed::Unsupported(n),
        Err(OptErr::Invalid(m)) => return Parsed::Invalid(m),
    };
    let (include, exclude, party, dom_inc, dom_exc, match_case, important) = match opts {
        Some(o) => (o.include, o.exclude, o.party, o.dom_inc, o.dom_exc, o.match_case, o.important),
        None => (0, 0, Party::Any, Vec::new(), Vec::new(), false, false),
    };
    let pattern = Pattern::parse(pat_text, match_case);
    if pattern.is_empty() && pat_text.bytes().any(|b| b != b'*') && pat_text.bytes().all(|b| b == b'|') {
        return Parsed::Invalid("empty pattern after anchor".into());
    }
    if pat_text.is_empty() && opts_text.is_none() {
        return Parsed::Invalid("empty filter".into());
    }
    Parsed::Filter(Box::new(Filter {
        text: line.to_string(),
        exception,
        important,
        match_case,
        pattern,
        include,
        exclude,
        party,
        dom_inc,
        dom_exc,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;

    fn filter(line: &str) -> Filter {
        match parse_line(line) {
            Parsed::Filter(f) => *f,
            _ => panic!("not a filter: {line}"),
        }
    }

    fn bit(t: ResourceType) -> u16 {
        type_bit(t)
    }

    #[test]
    fn comments_headers_and_blank_lines() {
        let (e, r) = Engine::from_text("[Adblock Plus 2.0]\n! comment\n# note\n\n   \r\n||a.test^\n");
        assert_eq!((r.comments, r.accepted, e.len()), (3, 1, 1));
        assert!(r.unsupported.is_empty() && r.invalid.is_empty());
    }

    #[test]
    fn cosmetic_filters_are_counted_not_errors() {
        let (e, r) = Engine::from_text("a.test##.ad\na.test#@#.ad\n#?#div\n#$#body{}\n#%#//x\nfoo$$script[x]\n");
        assert_eq!(r.cosmetic_skipped, 6);
        assert!(e.is_empty() && r.invalid.is_empty() && r.unsupported.is_empty());
    }

    #[test]
    fn regex_filter_is_unsupported() {
        let (e, r) = Engine::from_text("/ad[0-9]+/\n@@/ok.*/$script\n");
        assert!(e.is_empty());
        assert_eq!(r.unsupported, vec![(1, "regex".to_string()), (2, "regex".to_string())]);
    }

    #[test]
    fn unknown_option_is_unsupported_not_blocking_everything() {
        let (e, r) = Engine::from_text("||a.test^$frobnicate\n$popup\n||b.test^$all\n");
        assert!(e.is_empty());
        assert_eq!(r.unsupported.len(), 3);
        assert_eq!(r.unsupported[0].1, "frobnicate");
        let q = crate::Request { url: "https://a.test/", source_host: None, resource: ResourceType::Script, third_party: None };
        assert_eq!(e.check(&q), crate::Verdict::Allow);
    }

    #[test]
    fn redirect_and_removeparam_unsupported() {
        let (e, r) = Engine::from_text("||a.test^$redirect=noop.js\n||b.test^$removeparam=x\n||c.test^$csp=default-src 'none'\n");
        assert!(e.is_empty());
        let names: Vec<_> = r.unsupported.iter().map(|u| u.1.as_str()).collect();
        assert_eq!(names, ["redirect", "removeparam", "csp"]);
    }

    #[test]
    fn options_parse_aliases() {
        let f = filter("||a^$3p,css,xhr,frame");
        assert_eq!(f.party, Party::Third);
        assert_eq!(f.include, bit(ResourceType::Stylesheet) | bit(ResourceType::XmlHttpRequest) | bit(ResourceType::Subdocument));
        assert_eq!(filter("||a^$1p").party, Party::First);
        assert_eq!(filter("||a^$first-party").party, Party::First);
        assert_eq!(filter("||a^$~third-party").party, Party::First);
        assert_eq!(filter("||a^$~1p").party, Party::Third);
        assert_eq!(filter("||a^$doc").include, bit(ResourceType::Document));
    }

    #[test]
    fn negated_types() {
        let f = filter("||a^$~script,~image");
        assert_eq!(f.include, 0);
        assert_eq!(f.exclude, bit(ResourceType::Script) | bit(ResourceType::Image));
    }

    #[test]
    fn domain_option_include_and_exclude() {
        let f = filter("||a^$domain=x.com|~y.x.com|Z.org");
        assert_eq!(f.dom_inc, ["x.com", "z.org"]);
        assert_eq!(f.dom_exc, ["y.x.com"]);
        assert!(matches!(parse_line("||a^$domain="), Parsed::Invalid(_)));
        assert!(matches!(parse_line("||a^$domain=a.com||b.com"), Parsed::Invalid(_)));
    }

    #[test]
    fn empty_pattern_with_options() {
        let f = filter("$script,domain=a.com");
        assert!(f.pattern.is_empty());
        assert_eq!(f.dom_inc, ["a.com"]);
        assert!(matches!(parse_line("$"), Parsed::Invalid(_)));
        assert!(matches!(parse_line("@@"), Parsed::Invalid(_)));
        assert!(matches!(parse_line("||"), Parsed::Invalid(_)));
    }

    #[test]
    fn exception_prefix() {
        assert!(filter("@@||a.test^").exception);
        assert!(!filter("||a.test^").exception);
        assert_eq!(filter("@@||a.test^").text, "@@||a.test^");
    }

    #[test]
    fn important_option() {
        assert!(filter("||a.test^$important").important);
        assert!(!filter("||a.test^").important);
    }

    #[test]
    fn match_case_option() {
        let f = filter("/AdX$match-case");
        assert!(f.match_case);
        assert!(f.pattern.segs[0].contains(&crate::pattern::El::Lit(b'A')));
        let g = filter("/AdX");
        assert!(g.pattern.segs[0].contains(&crate::pattern::El::Lit(b'a')));
    }

    #[test]
    fn trailing_options_split_at_last_dollar() {
        let f = filter("/a$b/*$script");
        assert_eq!(f.include, bit(ResourceType::Script));
        assert!(f.pattern.segs[0].contains(&crate::pattern::El::Lit(b'$')));
        assert_eq!(f.pattern.segs[0].len(), 5); // "/a$b/"
    }

    #[test]
    fn report_line_numbers_are_one_based() {
        let (_, r) = Engine::from_lines(["! c", "||a^$foo", "/x/", "", "||b^$"]);
        assert_eq!(r.unsupported, vec![(2, "foo".to_string()), (3, "regex".to_string())]);
        assert_eq!(r.accepted, 1);
        let (_, r) = Engine::from_lines(["", "$"]);
        assert_eq!(r.invalid.len(), 1);
        assert_eq!(r.invalid[0].0, 2);
    }
}
