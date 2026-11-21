/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! What the address field shows when it is not being edited, and where it is scrolled when it is.
//!
//! The field's text is a URL; its *display* is the part that says which page this is. An unfocused field
//! shows the start of the URL - scheme handling, host, then the path - with the tail elided, never its
//! end. On a Google sign-in page a 200-character address used to read
//! `...&flowName=GlifWebSignIn&hl=da&pstMsg=1&service=youtube`: the person could not see which site he
//! was on.
//!
//! Both functions are pure: no UI types, no font metrics, no floating point. The caller measures its
//! glyphs and passes the number of Unicode scalar values that fit; everything here counts characters and
//! never splits one. Which part of the host is *the domain* is likewise an argument, worked out by the
//! caller from the Public Suffix List: this crate embeds no list and never guesses at one. The pinned
//! revisions the rules come from, and the places we deliberately differ, are in `PORTING.md`.

use std::ops::Range;

use url::{Host, Position, Url};

/// The elision marker: one character, as in Chromium's `kEllipsis` (`components/url_formatter/elide_url.cc`
/// @e9a4673), not three full stops.
const ELLIPSIS: char = '\u{2026}';

/// The one trivial subdomain that is dropped. Chromium's `kFormatUrlOmitTrivialSubdomains` @e9a4673 knows
/// more of them; see `PORTING.md`.
const TRIVIAL_SUBDOMAIN: &str = "www";

/// What an unfocused address field shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Display {
    /// The text to draw: scheme and trivial-prefix handling applied, the host's strong-colour part
    /// located in it, and the tail elided with `…` if it does not fit `max_chars`.
    pub text: String,
    /// Byte range inside [`Display::text`] of the part a UI draws in the strong colour and that is never
    /// elided: the caller's registrable domain where it is this host's, the whole host where it is not.
    /// `None` for a URL with no host and for text that does not parse as one.
    pub domain: Option<Range<usize>>,
    /// True when the display is shortened: tail elided, host elided, or both.
    pub elided: bool,
}

/// The unfocused display of `url`, shortened to at most `max_chars` Unicode scalar values.
///
/// `registrable_domain` is the host's registrable domain (eTLD+1, e.g. "google.com", "example.co.uk",
/// "me.github.io") as computed by the caller from the Public Suffix List, or `None` when there is none
/// (IP literal, localhost, single-label host, a host the list cannot place). It is used only if it is a
/// suffix of the host at a label boundary; anything else is treated as `None`, which highlights the whole
/// host rather than a guess.
///
/// - `https://` is dropped; `http://` is kept as the marker of an insecure page (there is no lock in this
///   UI yet), and any other scheme (`file://`, `ftp://`, `about:`, `data:` ...) is always shown;
/// - a trivial `www.` prefix is dropped, and userinfo (`user:pass@`) is never shown;
/// - the host, its port, then the path, query and fragment follow, with the tail replaced by `…`;
/// - the highlighted part is never cut: if the host does not fit, its subdomain part is cut from the
///   left, and if even that part does not fit, its start is shown with `…`;
/// - an IDN host is shown in Unicode only when no label mixes two scripts, otherwise as `xn--` punycode;
///   a `registrable_domain` in punycode is carried over to the host as displayed;
/// - a trailing root dot is not shown, being neither a label nor part of any domain;
/// - text that does not parse as a URL is returned as it was typed, truncated to `max_chars`.
///
/// `max_chars` is a character count, not a byte count, and the result never holds more than that.
pub fn unfocused_display(url: &str, registrable_domain: Option<&str>, max_chars: usize) -> Display {
    match formatted(url, registrable_domain) {
        None => {
            let (text, elided) = truncate(url, max_chars);
            Display {
                text,
                domain: None,
                elided,
            }
        }
        Some(parts) => parts.fit(max_chars),
    }
}

/// Where a focused field of `max_chars` visible characters is scrolled, in characters from the start of
/// its text.
///
/// 0 when the text fits, and when the caret is already inside the visible part (a click into the middle
/// of a URL must not jump the view); otherwise the view moves just far enough to keep the caret visible
/// at its end. A click that enters the field selects everything and leaves the caret at 0, so the start
/// of the URL is what the person sees; typing or `End` puts the caret at the end and scrolls there.
pub fn focused_scroll(text_chars: usize, caret_chars: usize, max_chars: usize) -> usize {
    caret_chars.min(text_chars).saturating_sub(max_chars)
}

/// The URL as it would be drawn with infinite space, in the pieces shortening works on. Each piece is a
/// whole number of characters of the display text, so a piece can only be cut at its ends.
struct Formatted {
    /// Scheme marker: empty for https, `http://` for http, `scheme://` for anything else with a host.
    pre: String,
    /// Labels of the host before the highlighted part, with their trailing dots; empty when there is
    /// nothing before it. This is the part that goes first.
    sub: String,
    /// What is highlighted and protected: the caller's registrable domain, or the whole host when it did
    /// not give one that fits this host. Never cut unless nothing else can be.
    dom: String,
    /// `:port`, glued to the domain because dropping it would name another server.
    port: String,
    /// Path, query and fragment. Elided from the right, before any of the host is touched.
    tail: String,
}

impl Formatted {
    /// The whole display, unelided. Only the property tests need it: callers get a shortened display.
    #[cfg(test)]
    fn head(&self) -> String {
        let mut text = String::with_capacity(
            self.pre.len() + self.sub.len() + self.dom.len() + self.port.len() + self.tail.len(),
        );
        text.push_str(&self.pre);
        text.push_str(&self.sub);
        text.push_str(&self.dom);
        text.push_str(&self.port);
        text.push_str(&self.tail);
        text
    }

    /// Appends the domain and returns its range in `text`.
    fn push_domain(&self, text: &mut String) -> Range<usize> {
        let start = text.len();
        text.push_str(&self.dom);
        start..text.len()
    }

    /// Shorten the display to `max` characters, protecting the domain and its port.
    fn fit(&self, max: usize) -> Display {
        let pre = count(&self.pre);
        // The domain and its port are one protected unit: a display that split them would lie about which
        // server the page is on.
        let dom = count(&self.dom) + count(&self.port);
        let host = pre + count(&self.sub) + dom;
        let total = host + count(&self.tail);

        let mut text = String::new();
        let mut domain: Option<Range<usize>> = None;
        let mut elided = true;
        if total <= max {
            text.push_str(&self.pre);
            text.push_str(&self.sub);
            domain = Some(self.push_domain(&mut text));
            text.push_str(&self.port);
            text.push_str(&self.tail);
            elided = false;
        } else if host <= max {
            // The whole host fits, so only the tail is cut: as much of it as is left, then the marker.
            let room = max - host;
            text.push_str(&self.pre);
            text.push_str(&self.sub);
            domain = Some(self.push_domain(&mut text));
            text.push_str(&self.port);
            if room > 1 {
                text.push_str(head_chars(&self.tail, room - 1));
            }
            // With no room at all for a marker the host wins; the caller still reads `elided`.
            if room > 0 {
                text.push(ELLIPSIS);
            }
        } else if !self.sub.is_empty() && pre + 1 + dom <= max {
            // No room for the host: cut its subdomain from the left, keeping as much of its end as fits.
            let room = max - pre - 1 - dom;
            text.push_str(&self.pre);
            text.push(ELLIPSIS);
            text.push_str(last_chars(&self.sub, room));
            domain = Some(self.push_domain(&mut text));
            text.push_str(&self.port);
        } else if pre + dom <= max {
            // Even the marker will not fit beside the host: show the host alone, shortened to its domain.
            text.push_str(&self.pre);
            domain = Some(self.push_domain(&mut text));
            text.push_str(&self.port);
        } else if max > dom {
            // A field narrower than the scheme marker and the domain together: the marker gives way, but
            // there is still room for the domain and its marker.
            domain = Some(self.push_domain(&mut text));
            text.push_str(&self.port);
            text.push(ELLIPSIS);
        } else if max > 1 {
            // The domain itself does not fit: its start, as far as it goes.
            let shown = head_chars(&self.dom, max - 1);
            let start = text.len();
            text.push_str(shown);
            domain = Some(start..text.len());
            text.push(ELLIPSIS);
        } else if max == 1 {
            // One character of a URL that does not fit says nothing about it; it is all there is room for.
            text.push(ELLIPSIS);
        }

        Display {
            text,
            domain,
            elided,
        }
    }
}

/// The display pieces of `url`, or `None` when it does not parse as a URL.
///
/// `registrable_domain` is believed only where it describes this host, see [`given_domain`]. A host that
/// is not a domain name (an IP literal) has no registrable domain, so a value for it is not looked at.
fn formatted(url: &str, registrable_domain: Option<&str>) -> Option<Formatted> {
    let parsed = Url::parse(url).ok()?;
    let ser = parsed.as_str();
    // URLs with no host to protect (`about:blank`, `data:`, `blob:`, `file:///path`) show their scheme
    // and everything after it; only the tail can be elided.
    let host_ascii = parsed.host_str().filter(|host| !host.is_empty())?;
    let after_host = parsed[..Position::AfterHost].len();

    let mut parts = Formatted {
        pre: prefix(&parsed),
        sub: String::new(),
        dom: String::new(),
        port: String::new(),
        tail: String::new(),
    };
    let (port, tail) = split_port(&ser[after_host..]);
    parts.port = port.to_owned();
    // A bare host needs no slash; Firefox drops it the same way (`trimURL` in
    // browser/modules/BrowserUIUtils.sys.mjs @33abb90).
    parts.tail = if tail == "/" {
        String::new()
    } else {
        tail.to_owned()
    };
    let shown = unicode_host(url, host_ascii).unwrap_or_else(|| host_ascii.to_owned());
    // The root dot is neither a label nor part of any domain, so it is not shown.
    let host = strip_root_dot(&shown).to_owned();
    // An IP literal has no registrable domain, so a value for it is not even looked at.
    let given = match parsed.host() {
        Some(Host::Domain(_)) => given_domain(host_ascii, &host, registrable_domain),
        _ => None,
    };
    if let Some(at) = given {
        parts.sub = drop_trivial(&host[..at], &host[at..]);
        parts.dom = host[at..].to_owned();
    } else {
        parts.dom = host;
    }
    Some(parts)
}

/// The scheme part of the display. Only `https://` disappears: `http://` stays as the marker of an
/// insecure page until there is a lock to show in its place, and any other scheme is always shown.
fn prefix(parsed: &Url) -> String {
    match parsed.scheme() {
        "https" => String::new(),
        scheme => format!("{scheme}://"),
    }
}

/// Splits what follows the host into its `:port` and the rest.
fn split_port(after_host: &str) -> (&str, &str) {
    let end = after_host.find(['/', '?', '#']).unwrap_or(after_host.len());
    let (maybe_port, rest) = after_host.split_at(end);
    let is_port = maybe_port.len() > 1
        && maybe_port.starts_with(':')
        && maybe_port[1..].bytes().all(|byte| byte.is_ascii_digit());
    if is_port {
        (maybe_port, rest)
    } else {
        ("", after_host)
    }
}

/// The byte offset in `shown` at which the caller's `registrable_domain` starts, `None` when the value is
/// not this host's and must be treated as absent.
///
/// The value has to end where the host ends and begin at a label boundary, so that `evil.com` says nothing
/// about `accounts.google.com` and `github.io` cannot be read off `me.github.io`. It arrives the way the
/// `url` crate's `host_str` does, punycode for an international host; when the host is shown in Unicode the
/// value is matched against the canonical host and carried over by label count, punycode being a per-label
/// encoding.
fn given_domain(ascii: &str, shown: &str, registrable_domain: Option<&str>) -> Option<usize> {
    let given = strip_root_dot(registrable_domain?);
    let labels = trailing_labels(shown, given).or_else(|| trailing_labels(ascii, given))?;
    Some(tail_offset(shown, labels))
}

/// The number of labels of `host` that `given` covers, when it covers the last ones exactly.
fn trailing_labels(host: &str, given: &str) -> Option<usize> {
    let host_labels: Vec<&str> = host.split('.').collect();
    let given_labels: Vec<&str> = given.split('.').collect();
    if given_labels.is_empty() || given_labels.len() > host_labels.len() {
        return None;
    }
    let off = host_labels.len() - given_labels.len();
    host_labels[off..]
        .iter()
        .zip(&given_labels)
        .all(|(label, want)| same_label(label, want))
        .then_some(given_labels.len())
}

/// The two forms of one label may differ only in case: the caller is not obliged to have folded it.
fn same_label(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase()
}

/// The byte offset of the last `labels` labels of `host`, which is `0` when that is the whole host.
fn tail_offset(host: &str, labels: usize) -> usize {
    let pass = host.matches('.').count().saturating_sub(labels - 1);
    let mut seen = 0;
    for (at, byte) in host.bytes().enumerate() {
        if byte == b'.' {
            seen += 1;
            if seen == pass {
                return at + 1;
            }
        }
    }
    0
}

/// `host` without its trailing root dot, if it has one.
fn strip_root_dot(host: &str) -> &str {
    host.strip_suffix('.').unwrap_or(host)
}

/// The labels before the registrable domain, as they go on screen: the trivial `www.` is dropped, as in
/// Chromium's `kWwwPrefix` (`elide_url.cc:473-481` @e9a4673), but only when it is a whole label of its own
/// and the domain left behind has two labels - `www.com` is a host, not a `www.` on top of `com`. The
/// caller has to say where the domain begins for that to be knowable, which is why nothing is dropped when
/// it did not give a domain.
fn drop_trivial(sub: &str, dom: &str) -> String {
    // A `www.` that is a whole label of its own, and only with a domain of two labels left behind.
    let Some((first, rest)) = sub.split_once('.') else {
        return sub.to_owned();
    };
    if !first.eq_ignore_ascii_case(TRIVIAL_SUBDOMAIN) || dom.split('.').count() < 2 {
        return sub.to_owned();
    }
    rest.to_owned()
}

/// The host as it was typed, when it is Unicode and showing it is safe; `None` means show the canonical
/// form the `url` crate parsed, which is punycode for a non-ASCII host.
///
/// This is the case worth showing in Unicode: the parser folded what it could, so a host that still needs
/// `xn--` in canonical form is one whose real characters are non-ASCII. Showing them is what Firefox does
/// (its `losslessDecodeURL`, browser/components/urlbar/content/UrlbarInputBase.mjs @33abb90). The safety
/// check is the one Chromium's IDN handling boils down to: no label may mix two scripts, because a Latin
/// host with one Cyrillic `а` reads as the host it is not. See `PORTING.md`.
fn unicode_host(typed: &str, ascii_host: &str) -> Option<String> {
    // A host the parser left in ASCII needs no rescue: fullwidth and uppercase letters are already folded.
    if !ascii_host.contains("xn--") {
        return None;
    }
    let scheme_end = typed.find("://")?;
    let authority = &typed[scheme_end + 3..];
    let authority = &authority[..authority.find(['/', '?', '#']).unwrap_or(authority.len())];
    // Everything up to the last `@` of the authority is userinfo, so the host starts after it.
    let host = &authority[authority.rfind('@').map_or(0, |at| at + 1)..];
    let host = &host[..host.rfind(':').unwrap_or(host.len())];
    if host.is_ascii() {
        return None;
    }
    let candidate = host.to_lowercase();
    // Prove that the slice is this URL's host - re-parsed, it must give the canonical host back - before
    // putting characters the parser never confirmed into the display.
    let reparsed = Url::parse(&format!("http://{candidate}/")).ok()?;
    (reparsed.host_str() == Some(ascii_host) && labels_have_one_script(&candidate)).then_some(candidate)
}

/// True when no label of `host` mixes two scripts. Script-less characters (digits, `-`, `_`, `.` and
/// punctuation, emoji) mix with anything, and one label may be Latin while another is Greek: that is an
/// ordinary international host, `παράδειγμα.gr`.
fn labels_have_one_script(host: &str) -> bool {
    host.split('.').all(|label| {
        let mut seen = None;
        for c in label.chars() {
            match (seen, Script::of(c)) {
                (_, None) => {}
                (None, Some(script)) => seen = Some(script),
                (Some(known), Some(script)) if known == script => {}
                (Some(_), Some(_)) => return false,
            }
        }
        true
    })
}

/// The scripts the display policy tells apart, as the spec lists them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Script {
    Latin,
    Greek,
    Cyrillic,
    /// Han and Kana together: a Japanese host mixes them in one label.
    EastAsian,
    Hangul,
    Arabic,
    Hebrew,
    Thai,
    Devanagari,
}

impl Script {
    /// The script of one character, `None` when the character carries no script.
    ///
    /// ASCII letters are Latin, as in Unicode's own `Script` property: that is what makes `аpple` with a
    /// Cyrillic `а` a mix. Each script is its block or blocks, not the full property, so a handful of
    /// characters outside them (a Latin letter in the Phonetic Extensions block, say) count as
    /// script-less. That errs towards showing the host, never towards hiding a mix.
    fn of(c: char) -> Option<Script> {
        let cp = c as u32;
        if matches!(cp, 0x0041..=0x005A | 0x0061..=0x007A | 0x00C0..=0x024F | 0x1E00..=0x1EFF | 0x2C60..=0x2C7F | 0xA720..=0xA7FF)
        {
            return Some(Script::Latin);
        }
        if matches!(cp, 0x0370..=0x03FF | 0x1F00..=0x1FFF) {
            return Some(Script::Greek);
        }
        if matches!(cp, 0x0400..=0x052F) {
            return Some(Script::Cyrillic);
        }
        if matches!(cp, 0x3005..=0x3007 | 0x3021..=0x3029 | 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
        {
            return Some(Script::EastAsian);
        }
        if matches!(cp, 0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xAC00..=0xD7AF) {
            return Some(Script::Hangul);
        }
        if matches!(cp, 0x0600..=0x07BF | 0x0860..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF) {
            return Some(Script::Arabic);
        }
        if matches!(cp, 0x0590..=0x05FF | 0xFB1D..=0xFB4F) {
            return Some(Script::Hebrew);
        }
        if matches!(cp, 0x0E00..=0x0E7F) {
            return Some(Script::Thai);
        }
        if matches!(cp, 0x0900..=0x097F) {
            return Some(Script::Devanagari);
        }
        None
    }
}

/// `text` cut to at most `max` characters, with the marker when anything was dropped.
fn truncate(text: &str, max: usize) -> (String, bool) {
    if count(text) <= max {
        return (text.to_owned(), false);
    }
    if max == 0 {
        return (String::new(), true);
    }
    let mut out = String::new();
    out.push_str(head_chars(text, max - 1));
    out.push(ELLIPSIS);
    (out, true)
}

/// The first `n` characters of `s`, a whole number of code points.
fn head_chars(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or(s, |(at, _)| &s[..at])
}

/// The last `n` characters of `s`, a whole number of code points.
fn last_chars(s: &str, n: usize) -> &str {
    let total = count(s);
    if n >= total {
        return s;
    }
    let at = s.char_indices().nth(total - n).map_or(s.len(), |(i, _)| i);
    &s[at..]
}

/// Character count, which is the unit of `max_chars`.
fn count(s: &str) -> usize {
    s.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGN_IN: &str = "https://accounts.google.com/v3/signin/identifier?continue=https%3A%2F%2Fwww.youtube.com%2Fsignin%3Faction_handle_signin%3Dtrue&ec=65620&hl=da&flowName=GlifWebSignIn&flowEntry=ServiceLogin&service=youtube&pstMsg=1";

    #[test]
    fn long_sign_in_url_shows_host_first() {
        let shown = unfocused_display(SIGN_IN, Some("google.com"), 60);
        assert!(
            shown.text.starts_with("accounts.google.com/"),
            "the field showed the end of the URL instead of its start: {:?}",
            shown.text
        );
        assert!(shown.text.ends_with(ELLIPSIS), "{:?}", shown.text);
        assert_eq!(count(&shown.text), 60, "{:?}", shown.text);
        assert!(shown.elided);
    }

    #[test]
    fn domain_range_is_google_com() {
        let shown = unfocused_display(SIGN_IN, Some("google.com"), 60);
        let domain = shown.domain.expect("the caller says google.com is the registrable domain");
        assert_eq!(&shown.text[domain], "google.com");
    }

    #[test]
    fn https_scheme_is_dropped() {
        let shown = unfocused_display("https://example.com/a/b?c=1", Some("example.com"), 60);
        assert_eq!(shown.text, "example.com/a/b?c=1");
        assert_eq!(shown.domain, Some(0..11));
        assert!(!shown.elided);
        // The fragment is part of the address the person is looking at, so it stays.
        assert_eq!(
            unfocused_display("https://example.com/a#b", Some("example.com"), 60).text,
            "example.com/a#b"
        );
        // So does the port: without it the text names a different server.
        assert_eq!(
            unfocused_display("https://example.com:8443/a", Some("example.com"), 60).text,
            "example.com:8443/a"
        );
        // A secure URL that fits keeps everything; a bare host keeps no trailing slash.
        assert_eq!(
            unfocused_display("https://example.com", Some("example.com"), 60).text,
            "example.com"
        );
        assert_eq!(
            unfocused_display("https://example.com/", Some("example.com"), 60).text,
            "example.com"
        );
        assert_eq!(
            unfocused_display("https://example.com//", Some("example.com"), 60).text,
            "example.com//"
        );
    }

    #[test]
    fn http_scheme_is_kept() {
        let shown = unfocused_display("http://example.com/a", Some("example.com"), 60);
        assert_eq!(shown.text, "http://example.com/a");
        assert_eq!(shown.domain, Some(7..18));
        // The marker costs a character, and the host still wins.
        let narrow = unfocused_display("http://example.com/abcdefghij", Some("example.com"), 18);
        assert_eq!(narrow.text, "http://example.com");
        assert_eq!(narrow.domain, Some(7..18));
        assert!(narrow.elided);
    }

    #[test]
    fn other_schemes_are_kept() {
        for (url, text) in [
            ("file:///home/user/notes.md", "file:///home/user/notes.md"),
            (
                "ftp://ftp.example.com/pub/readme.txt",
                "ftp://ftp.example.com/pub/readme.txt",
            ),
            ("about:blank", "about:blank"),
            ("about:preferences", "about:preferences"),
            ("data:text/plain,hello", "data:text/plain,hello"),
            (
                "view-source:https://example.com/",
                "view-source:https://example.com/",
            ),
            ("myapp://open/thing", "myapp://open/thing"),
            ("ws://socket.example.com/live", "ws://socket.example.com/live"),
            ("mailto:someone@example.com", "mailto:someone@example.com"),
        ] {
            // None of these depends on where the domain starts, so the value is not given.
            assert_eq!(unfocused_display(url, None, 60).text, text, "for {url}");
        }
        // A long hostless URL is shortened from the tail, and its scheme survives.
        let shown = unfocused_display("data:text/plain,", None, 10);
        assert_eq!(shown.text, "data:text\u{2026}");
        assert!(shown.elided);
        assert_eq!(shown.domain, None);
        // Hostless URLs report no domain, even when they name a host further along.
        assert_eq!(
            unfocused_display("blob:https://example.com/5c6a", Some("example.com"), 60).domain,
            None
        );
        let narrow = unfocused_display("file:///home/user/notes.md", None, 12);
        assert_eq!(narrow.text, "file:///hom\u{2026}");
    }

    #[test]
    fn www_is_dropped() {
        let shown = unfocused_display("https://www.example.com/x", Some("example.com"), 60);
        assert_eq!(shown.text, "example.com/x");
        assert_eq!(shown.domain, Some(0..11));
        // A host that would be left with one label keeps it, and so does a label that only starts with it.
        assert_eq!(
            unfocused_display("https://www.com/x", Some("www.com"), 60).text,
            "www.com/x"
        );
        assert_eq!(
            unfocused_display("https://wwwx.example.com/x", Some("example.com"), 60).text,
            "wwwx.example.com/x"
        );
        assert_eq!(
            unfocused_display("https://xwww.example.com/x", Some("example.com"), 60).text,
            "xwww.example.com/x"
        );
        // `www.` is trivial only as the first label: `a.www.example.com` keeps `a.www.`.
        assert_eq!(
            unfocused_display("https://a.www.example.com/x", Some("example.com"), 60).text,
            "a.www.example.com/x"
        );
        // Dropped from an insecure URL too, after the kept scheme.
        assert_eq!(
            unfocused_display("http://www.example.com", Some("example.com"), 60).text,
            "http://example.com"
        );
        // `www.` on a multi-label suffix keeps the whole registrable domain.
        let uk = unfocused_display("https://www.example.co.uk/", Some("example.co.uk"), 60);
        assert_eq!(uk.text, "example.co.uk");
        // A host that is nothing but the trivial prefix and a public suffix keeps both labels.
        assert_eq!(
            unfocused_display("https://www.co.uk/x", Some("www.co.uk"), 60).text,
            "www.co.uk/x"
        );
        // With no value from the caller the boundary is unknown, so nothing is dropped: `www.` may itself
        // be the start of the registrable domain.
        assert_eq!(
            unfocused_display("https://www.example.com/x", None, 60).text,
            "www.example.com/x"
        );
        assert_eq!(
            unfocused_display("https://www.example.com/x", None, 60).domain,
            Some(0..15)
        );
    }

    #[test]
    fn given_domain_co_uk_range() {
        // What `multi_label_suffix_co_uk` used to check by guessing: now the caller's value decides it.
        let shown = unfocused_display("https://shop.example.co.uk/x", Some("example.co.uk"), 60);
        assert_eq!(shown.text, "shop.example.co.uk/x");
        let domain = shown.domain.expect("example.co.uk is the registrable domain");
        assert_eq!(&shown.text[domain.clone()], "example.co.uk");
        assert_eq!(domain, 5..18);
        // Deeper labels do not move the domain.
        let deep = unfocused_display("https://a.b.c.d.example.co.uk/x", Some("example.co.uk"), 60);
        assert_eq!(&deep.text[deep.domain.expect("domain")], "example.co.uk");
        // A zone no list entry claims: the caller's value decides it, and the range follows.
        let other = unfocused_display("https://shop.example.zz9/x", Some("example.zz9"), 60);
        assert_eq!(&other.text[other.domain.expect("domain")], "example.zz9");
        // A host that is only a public suffix is its own highlighted part.
        let plain = unfocused_display("https://co.uk/", Some("co.uk"), 60);
        assert_eq!(plain.domain, Some(0..5));
        // The caller's case does not matter.
        let upper = unfocused_display("https://shop.example.co.uk/x", Some("EXAMPLE.CO.UK"), 60);
        assert_eq!(upper.domain, Some(5..18));
    }

    #[test]
    fn registrable_under_hosting_suffix() {
        // The case the embedded list got wrong: `github.io` is a public suffix, so the page is on
        // `me.github.io` and that is what the strong colour must cover.
        let shown = unfocused_display("https://me.github.io/", Some("me.github.io"), 60);
        assert_eq!(shown.text, "me.github.io");
        let domain = shown.domain.expect("the caller's value");
        assert_eq!(&shown.text[domain.clone()], "me.github.io");
        assert_eq!(
            domain,
            0..12,
            "the range covers the whole host, not the github.io suffix"
        );
        // A label above it stays visible and out of the highlighted part.
        let deep = unfocused_display("https://a.me.github.io/x", Some("me.github.io"), 60);
        assert_eq!(deep.text, "a.me.github.io/x");
        assert_eq!(&deep.text[deep.domain.expect("domain")], "me.github.io");
        // With nothing from the caller the whole host is highlighted; the suffix is never guessed.
        let none = unfocused_display("https://me.github.io/", None, 60);
        assert_eq!(none.domain, Some(0..12));
    }

    #[test]
    fn registrable_under_pages_dev() {
        let shown = unfocused_display("https://luke.pages.dev/", Some("luke.pages.dev"), 60);
        assert_eq!(shown.text, "luke.pages.dev");
        assert_eq!(&shown.text[shown.domain.expect("domain")], "luke.pages.dev");
        // The trivial prefix goes, the three-label domain stays.
        let www = unfocused_display("https://www.luke.pages.dev/", Some("luke.pages.dev"), 60);
        assert_eq!(www.text, "luke.pages.dev");
        assert_eq!(www.domain, Some(0..14));
        let deep = unfocused_display("https://a.b.luke.pages.dev/x", Some("luke.pages.dev"), 60);
        assert_eq!(deep.text, "a.b.luke.pages.dev/x");
        assert_eq!(&deep.text[deep.domain.expect("domain")], "luke.pages.dev");
    }

    #[test]
    fn co_jp_and_com_br_fixed_values() {
        // Zones the old embedded list carried, now as the fixed values the engine side would pass.
        for (url, given) in [
            ("https://a.example.co.jp/x", "example.co.jp"),
            ("https://a.example.com.br/x", "example.com.br"),
            ("https://a.example.com.au/x", "example.com.au"),
            ("https://a.example.com.tr/x", "example.com.tr"),
            ("https://a.example.ac.uk/x", "example.ac.uk"),
            ("https://a.b.example.com.cn/x", "example.com.cn"),
        ] {
            let shown = unfocused_display(url, Some(given), 60);
            let host = url["https://".len()..].split('/').next().unwrap();
            assert_eq!(&shown.text[shown.domain.expect(url)], given, "for {url}");
            assert!(shown.text.starts_with("a."), "for {url}: {:?}", shown.text);
            // With no value the whole host is highlighted; the suffix is never guessed from it.
            let none = unfocused_display(url, None, 60);
            assert_eq!(none.text, format!("{host}/x"), "for {url}");
            assert_eq!(&none.text[none.domain.expect(url)], host, "for {url}");
        }
    }

    #[test]
    fn none_highlights_whole_host() {
        let shown = unfocused_display("https://shop.example.co.uk/x", None, 60);
        assert_eq!(shown.text, "shop.example.co.uk/x");
        let domain = shown.domain.expect("no value given, so the whole host");
        assert_eq!(&shown.text[domain], "shop.example.co.uk");
        // A single-label host is its own highlighted part, as is one the list cannot place.
        let single = unfocused_display("https://myhost/x", None, 60);
        assert_eq!(single.domain, Some(0..6));
        assert_eq!(&single.text[single.domain.expect("host")], "myhost");
        // The protected unit is that whole host: the tail goes before any of it is cut.
        let narrow = unfocused_display("https://shop.example.co.uk/x", None, 18);
        assert_eq!(narrow.text, "shop.example.co.uk");
        assert_eq!(&narrow.text[narrow.domain.expect("host")], "shop.example.co.uk");
        assert!(narrow.elided);
    }

    #[test]
    fn ipv4_literal_whole_host() {
        let shown = unfocused_display("http://192.168.1.1/admin/index.html", None, 60);
        assert!(
            shown.text.starts_with("http://192.168.1.1/"),
            "{:?}",
            shown.text
        );
        let domain = shown.domain.expect("an address has no registrable domain, so the host is it");
        assert_eq!(&shown.text[domain], "192.168.1.1");
        // The whole host is the part that matters, so nothing of it is cut even in a narrow field.
        let narrow = unfocused_display("http://192.168.1.1/admin", None, 18);
        assert_eq!(narrow.text, "http://192.168.1.1");
        assert_eq!(narrow.domain, Some(7..18));
        assert!(narrow.elided);
        // A value for an address is not a registrable domain of this host and is ignored.
        let given = unfocused_display("http://192.168.1.1/admin", Some("1.1"), 18);
        assert_eq!(given.domain, Some(7..18));
        assert_eq!(&given.text[given.domain.expect("host")], "192.168.1.1");
    }

    #[test]
    fn ipv6_literal_whole_host() {
        let shown = unfocused_display("http://[::1]:8080/x", None, 60);
        assert_eq!(shown.text, "http://[::1]:8080/x");
        assert_eq!(shown.domain, Some(7..12));
        let secure = unfocused_display("https://[2001:db8::1]/a/b", None, 60);
        assert_eq!(secure.text, "[2001:db8::1]/a/b");
        assert_eq!(&secure.text[secure.domain.expect("host")], "[2001:db8::1]");
        // The brackets and the address are one piece, so a narrow field cuts the tail and nothing else.
        let narrow = unfocused_display("https://[2001:db8::1]/a/b", None, 14);
        assert_eq!(narrow.text, "[2001:db8::1]\u{2026}");
        assert_eq!(narrow.domain, Some(0..13));
        assert!(narrow.elided);
    }

    #[test]
    fn localhost_with_port_whole_host() {
        let shown = unfocused_display("https://localhost:8080/x", None, 60);
        assert_eq!(shown.text, "localhost:8080/x");
        assert_eq!(shown.domain, Some(0..9));
        let dev = unfocused_display("http://localhost:3000/", None, 60);
        assert_eq!(dev.text, "http://localhost:3000");
        assert_eq!(&dev.text[dev.domain.expect("host")], "localhost");
        // The port is glued on but is not part of the highlighted host, and it is never cut from it.
        let narrow = unfocused_display("https://localhost:3000/a/b/c", None, 14);
        assert_eq!(narrow.text, "localhost:3000");
        assert_eq!(narrow.domain, Some(0..9));
        assert!(narrow.elided);
        // A single-label host is its own registrable domain, which is the same answer as giving nothing.
        assert_eq!(
            unfocused_display("http://localhost:3000/", Some("localhost"), 60).domain,
            Some(7..16)
        );
    }

    #[test]
    fn userinfo_is_never_shown() {
        let shown = unfocused_display("https://user:pass@example.com/vault", Some("example.com"), 60);
        assert_eq!(shown.text, "example.com/vault");
        assert!(!shown.text.contains('@'), "{:?}", shown.text);
        assert!(!shown.text.contains("pass"), "{:?}", shown.text);
        assert_eq!(shown.domain, Some(0..11));
        let secure = unfocused_display("http://bob@10.0.0.7/status", None, 60);
        assert_eq!(secure.text, "http://10.0.0.7/status");
        assert_eq!(&secure.text[secure.domain.expect("address")], "10.0.0.7");
        // The last `@` before the path ends the userinfo, as it does for the parser.
        let odd = unfocused_display("https://x:y@z@example.com/x", Some("example.com"), 60);
        assert_eq!(odd.text, "example.com/x", "{:?}", odd.text);
        assert!(!odd.text.contains('@'), "{:?}", odd.text);
        // A host with a percent-escaped `@` in it is not a host at all: the parser refuses it, so the text
        // is shown as typed rather than guessed at.
        let escaped = unfocused_display("https://a%40b@c%40d.example/x", Some("example.com"), 60);
        assert_eq!(escaped.text, "https://a%40b@c%40d.example/x");
        assert_eq!(escaped.domain, None);
    }

    #[test]
    fn userinfo_trick_shows_real_host() {
        // Everything before the `@` is userinfo: this page is on evil.example, and the display must say so
        // rather than repeat the host the trick was built to imitate.
        let shown = unfocused_display("https://accounts.google.com@evil.example/path", None, 60);
        assert!(shown.text.starts_with("evil.example/"), "{:?}", shown.text);
        assert!(!shown.text.contains("google"), "{:?}", shown.text);
        let domain = shown.domain.expect("evil.example");
        assert_eq!(&shown.text[domain], "evil.example");
        // A field as narrow as the real host is: the tail goes, the host stays whole.
        let narrow = unfocused_display("https://accounts.google.com@evil.example/path", None, 13);
        assert_eq!(narrow.text, "evil.example\u{2026}");
        assert_eq!(narrow.domain, Some(0..12));
    }

    #[test]
    fn userinfo_trick_with_given_domain() {
        let shown =
            unfocused_display("https://accounts.google.com@evil.example/", Some("evil.example"), 60);
        assert!(shown.text.starts_with("evil.example"), "{:?}", shown.text);
        assert!(!shown.text.contains("google"), "{:?}", shown.text);
        assert_eq!(shown.domain, Some(0..12));
        // A subdomain on the real host is cut from the left, and the host of the trick stays out of it.
        let deeper = unfocused_display(
            "https://accounts.google.com@mail.evil.example/path",
            Some("evil.example"),
            13,
        );
        assert_eq!(deeper.text, "\u{2026}evil.example");
        // `Display::domain` is a byte range: the marker is three bytes of its own.
        assert_eq!(deeper.domain, Some(3..15));
        assert_eq!(&deeper.text[deeper.domain.unwrap()], "evil.example");
        // Without the caller's value the subdomain is not separable, so the whole host is cut from its
        // start instead - never the imitated host.
        let none = unfocused_display("https://accounts.google.com@mail.evil.example/path", None, 13);
        assert_eq!(none.text, "mail.evil.ex\u{2026}");
        assert_eq!(&none.text[none.domain.expect("host")], "mail.evil.ex");
        assert!(!none.text.contains("google"), "{:?}", none.text);
    }

    #[test]
    fn subdomain_before_domain_kept() {
        // Knowing where the domain starts does not mean hiding what came before it.
        let shown = unfocused_display("https://mail.google.com/inbox", Some("google.com"), 60);
        assert_eq!(shown.text, "mail.google.com/inbox");
        assert_eq!(shown.domain, Some(5..15));
        assert_eq!(&shown.text[shown.domain.expect("domain")], "google.com");
        // Only the tail goes first; at the width of the host alone the subdomain still shows.
        let tight = unfocused_display("https://mail.google.com/inbox", Some("google.com"), 15);
        assert_eq!(tight.text, "mail.google.com");
        assert_eq!(tight.domain, Some(5..15));
        // Too narrow for both: the subdomain goes from the left, the domain stays whole behind a marker.
        let narrow = unfocused_display("https://mail.google.com/inbox", Some("google.com"), 11);
        assert_eq!(narrow.text, "\u{2026}google.com");
        assert_eq!(narrow.domain, Some(3..13));
        // One character less and the marker has no room: the domain alone.
        let alone = unfocused_display("https://mail.google.com/inbox", Some("google.com"), 10);
        assert_eq!(alone.text, "google.com");
        assert_eq!(alone.domain, Some(0..10));
    }

    #[test]
    fn trailing_dot_host() {
        // The root dot is not part of the registrable domain, so it is not shown: the parser keeps it in
        // `host_str` ("example.com."), and the display drops it.
        let shown = unfocused_display("https://example.com./", Some("example.com"), 60);
        assert_eq!(shown.text, "example.com", "{:?}", shown.text);
        assert_eq!(shown.domain, Some(0..11));
        // Without a value the whole displayed host is the highlighted part, dot gone either way.
        let none = unfocused_display("https://example.com./", None, 60);
        assert_eq!(none.text, "example.com");
        assert_eq!(none.domain, Some(0..11));
        // With labels above it, and a value that carries the dot itself: the same answer.
        let deep = unfocused_display("https://shop.example.co.uk./x", Some("example.co.uk."), 60);
        assert_eq!(deep.text, "shop.example.co.uk/x", "{:?}", deep.text);
        let domain = deep.domain.expect("domain");
        assert_eq!(&deep.text[domain.clone()], "example.co.uk");
        assert_eq!(domain, 5..18);
    }

    #[test]
    fn invalid_caller_value_treated_as_none() {
        // A value that is not a suffix of this host at a label boundary says nothing about it.
        let shown = unfocused_display("https://accounts.google.com/x", Some("evil.com"), 60);
        assert_eq!(shown.text, "accounts.google.com/x");
        assert_eq!(&shown.text[shown.domain.expect("host")], "accounts.google.com");
        // Nor is one that starts in the middle of a label, one longer than the host, or an empty one.
        for given in ["ple.com", "a.b.example.com", "", "."] {
            let shown = unfocused_display("https://example.com/x", Some(given), 60);
            assert_eq!(shown.domain, Some(0..11), "for {given:?}");
        }
        // A value that is a single label of the host is the caller's answer, believed: this crate does not
        // know which labels are public suffixes. A dot on the end of the value says nothing either.
        for given in ["com", "com."] {
            assert_eq!(
                unfocused_display("https://example.com/x", Some(given), 60).domain,
                Some(8..11),
                "for {given:?}"
            );
        }
    }

    #[test]
    fn path_containing_other_host_name() {
        let shown = unfocused_display(
            "https://evil.example/https://accounts.google.com/",
            Some("evil.example"),
            60,
        );
        assert_eq!(shown.text, "evil.example/https://accounts.google.com/");
        let domain = shown.domain.expect("evil.example");
        assert_eq!(&shown.text[domain], "evil.example");
        // Even when the tail is elided, the domain shown is the real one.
        let narrow = unfocused_display(
            "https://evil.example/https://accounts.google.com/",
            Some("evil.example"),
            20,
        );
        assert_eq!(narrow.text, "evil.example/https:\u{2026}");
        assert_eq!(&narrow.text[narrow.domain.expect("domain")], "evil.example");
    }

    #[test]
    fn very_long_query_string_is_elided() {
        let query = "x".repeat(5_000);
        let url = format!("https://example.com/search?q={query}");
        let shown = unfocused_display(&url, Some("example.com"), 40);
        assert_eq!(count(&shown.text), 40);
        assert!(shown.text.ends_with(ELLIPSIS));
        assert!(shown.text.starts_with("example.com/search?q="));
        assert_eq!(shown.domain, Some(0..11));
        assert!(shown.elided);
        // Only what does not fit is cut: at the full width everything, including the 5 000-character
        // query, is there and nothing is marked.
        let full = unfocused_display(&url, Some("example.com"), 6_000);
        assert_eq!(full.text, format!("example.com/search?q={query}"));
        assert!(!full.elided);
    }

    #[test]
    fn max_chars_smaller_than_host() {
        let url = "https://mail.inbox.example.co.uk/a/b/c";
        let shown = unfocused_display(url, Some("example.co.uk"), 18);
        assert_eq!(shown.text, "\u{2026}box.example.co.uk");
        assert_eq!(
            &shown.text[shown.domain.expect("domain")],
            "example.co.uk",
            "the registrable domain is never cut"
        );
        assert!(shown.elided);
        // No room left for any of the subdomain: the domain alone, with its marker.
        let tight = unfocused_display(url, Some("example.co.uk"), 14);
        assert_eq!(tight.text, "\u{2026}example.co.uk");
        assert_eq!(&tight.text[tight.domain.expect("domain")], "example.co.uk");
        // No room for the marker either: the domain alone.
        let exact = unfocused_display(url, Some("example.co.uk"), 13);
        assert_eq!(exact.text, "example.co.uk");
        assert_eq!(exact.domain, Some(0..13));
        // Narrower still: the scheme marker gives way, then the domain is cut from its start.
        // 17 characters is not enough for the scheme marker and the domain together, so the marker goes.
        let insecure = unfocused_display("http://mail.inbox.example.co.uk/a", Some("example.co.uk"), 17);
        assert_eq!(insecure.text, "example.co.uk\u{2026}");
        assert_eq!(insecure.domain, Some(0..13));
        let tiny = unfocused_display(url, Some("example.co.uk"), 8);
        assert_eq!(tiny.text, "example\u{2026}");
        assert_eq!(&tiny.text[tiny.domain.expect("domain")], "example");
        let http_tiny = unfocused_display("http://mail.inbox.example.co.uk/a", Some("example.co.uk"), 8);
        assert_eq!(http_tiny.text, "example\u{2026}");
        assert_eq!(count(&http_tiny.text), 8);
        assert_eq!(http_tiny.domain, Some(0..7));
    }

    #[test]
    fn max_chars_zero_and_one() {
        let url = "https://example.com/a/b?c=d";
        let none = unfocused_display(url, Some("example.com"), 0);
        assert_eq!(none.text, "");
        assert!(none.elided);
        assert_eq!(none.domain, None);
        let one = unfocused_display(url, Some("example.com"), 1);
        assert_eq!(one.text, "\u{2026}");
        assert!(one.elided);
        assert_eq!(one.domain, None);
        // Two characters: the first of the domain and the marker.
        let two = unfocused_display(url, Some("example.com"), 2);
        assert_eq!(two.text, "e\u{2026}");
        assert_eq!(two.domain, Some(0..1));
        // A URL that already fits is untouched at a width that costs nothing.
        assert_eq!(unfocused_display("https://x.y/1", None, 0).text, "");
        assert!(!unfocused_display("", None, 0).elided);
        assert_eq!(unfocused_display("", None, 5).text, "");
        assert!(!unfocused_display("", None, 5).elided);
    }

    #[test]
    fn idn_single_script_unicode_shown() {
        // The caller passes the form the parser gives it, punycode; the display carries it over to the
        // labels as shown.
        let shown = unfocused_display("https://münchen.de/", Some("xn--mnchen-3ya.de"), 60);
        assert_eq!(shown.text, "münchen.de");
        assert_eq!(&shown.text[shown.domain.expect("domain")], "münchen.de");
        // A Greek label under a Latin one is an ordinary international host, not a mix. With no value
        // given the whole host is the highlighted part, which here is the same string.
        let greek = unfocused_display("https://παράδειγμα.gr/α", None, 60);
        assert_eq!(greek.text, "παράδειγμα.gr/%CE%B1");
        assert_eq!(&greek.text[greek.domain.expect("domain")], "παράδειγμα.gr");
        // Digits, hyphens and underscores carry no script; Han and Kana are one group.
        let hyphen = unfocused_display("https://ma-01.münchen.de/x", Some("xn--mnchen-3ya.de"), 60);
        assert_eq!(hyphen.text, "ma-01.münchen.de/x");
        assert_eq!(&hyphen.text[hyphen.domain.expect("domain")], "münchen.de");
        let japanese = unfocused_display("https://日本へん.jp/x", None, 60);
        assert_eq!(japanese.text, "日本へん.jp/x");
        let korean = unfocused_display("https://한글.kr/x", None, 60);
        assert_eq!(korean.text, "한글.kr/x");
        let devanagari = unfocused_display("https://उदाहरण.भारत/x", None, 60);
        assert_eq!(devanagari.text, "उदाहरण.भारत/x");
        // The typed case is not kept: the parser folds it, and so do we.
        assert_eq!(
            unfocused_display("https://M\u{00dc}NCHEN.de/", Some("xn--mnchen-3ya.de"), 60).text,
            "m\u{00fc}nchen.de"
        );
        // A host typed as punycode stays punycode: the safe form is the one we have.
        let puny = unfocused_display("https://xn--mnchen-3ya.de/", Some("xn--mnchen-3ya.de"), 60);
        assert_eq!(puny.text, "xn--mnchen-3ya.de");
        assert_eq!(&puny.text[puny.domain.expect("domain")], "xn--mnchen-3ya.de");
        // Its `www.` still goes.
        assert_eq!(
            unfocused_display("https://www.xn--mnchen-3ya.de/", Some("xn--mnchen-3ya.de"), 60).text,
            "xn--mnchen-3ya.de"
        );
    }

    #[test]
    fn idn_mixed_script_shows_punycode() {
        // A Cyrillic `а` (U+0430) in a Latin host: every character reads as a Latin one, so the only
        // honest display is the punycode the parser made of it.
        let shown = unfocused_display("https://\u{430}pple.com/x", Some("xn--pple-43d.com"), 60);
        assert_eq!(shown.text, "xn--pple-43d.com/x");
        assert_eq!(&shown.text[shown.domain.expect("domain")], "xn--pple-43d.com");
        // Latin with Greek is a mix as well.
        let greek = unfocused_display("https://l\u{3b1}tin.example.com/", Some("example.com"), 60);
        assert_eq!(greek.text, "xn--ltin-0ld.example.com");
        // Cyrillic with Greek too: the rule is per label, not per host.
        let two = unfocused_display("https://\u{430}\u{3b1}.example.com/", Some("example.com"), 60);
        assert_eq!(two.text, "xn--mxa7w.example.com");
        // Hangul is its own script: Han with Hangul does not make one label.
        let han = unfocused_display("https://\u{65e5}\u{d55c}.kr/", Some("xn--wgv449v.kr"), 60);
        assert_eq!(han.text, "xn--wgv449v.kr");
        // A mixed host keeps the scheme marker, and is shortened like any other.
        let insecure = unfocused_display("http://\u{430}pple.com/x", Some("xn--pple-43d.com"), 60);
        assert_eq!(insecure.text, "http://xn--pple-43d.com/x");
        assert_eq!(
            &insecure.text[insecure.domain.expect("domain")],
            "xn--pple-43d.com"
        );
    }

    #[test]
    fn fullwidth_and_emoji_hosts_do_not_panic() {
        // Fullwidth Latin is folded by the parser before we see it, so the display is the folded ASCII
        // host; nothing about the typed form survives to be mistaken for it.
        let folded = unfocused_display(
            "https://\u{ff21}\u{ff22}\u{ff23}.example.com/x",
            Some("example.com"),
            60,
        );
        assert_eq!(folded.text, "abc.example.com/x");
        assert_eq!(&folded.text[folded.domain.expect("domain")], "example.com");
        for url in [
            "https://\u{1f600}.example.com/",
            "https://\u{1f600}\u{200d}\u{1f600}.example.com/",
            "https://\u{2764}.example.com/x",
            "https://\u{fe0f}.example.com/x",
            "https://\u{202e}example.com/x",
            "https://\u{2066}example.com/x",
            "https://\u{0301}.example.com/x",
            "https://\u{130a1}.example.com/x",
            "https://\u{10ffff}.example.com/x",
            "https://\u{00ad}example.com/x",
            "https://\u{0c89}.example.com/in",
            "http://\u{6d4b}\u{8bd5}.\u{4e2d}\u{56fd}/x",
            "\u{200e}https://example.com/",
            "https://xn--e28h.example.com/",
            "https://\u{1f600}.\u{1f600}/\u{1f600}",
        ] {
            for max in [0usize, 1, 2, 5, 13, 40, 100] {
                let shown = unfocused_display(url, None, max);
                assert!(
                    count(&shown.text) <= max,
                    "{url} at {max}: {:?}",
                    shown.text
                );
                assert!(shown.text.is_char_boundary(shown.text.len()));
                assert!(!shown.text.contains('\u{fffd}'), "{url} put a replacement in");
                if let Some(domain) = &shown.domain {
                    assert!(
                        shown.text.get(domain.clone()).is_some(),
                        "{url} at {max}: domain out of {:?}",
                        shown.text
                    );
                }
            }
        }
    }

    #[test]
    fn never_splits_a_code_point() {
        let corpus = [
            (SIGN_IN, Some("google.com")),
            ("https://m\u{00fc}nchen.de/x", Some("xn--mnchen-3ya.de")),
            ("https://\u{430}pple.com/x/\u{1f600}", Some("xn--pple-43d.com")),
            (
                "https://example.com/\u{65e5}\u{672c}\u{3078}\u{3093}",
                Some("example.com"),
            ),
            (
                "https://example.com/\u{d55c}\u{adfc}?q=\u{1f4a9}",
                Some("example.com"),
            ),
            ("https://example.com/#\u{2764}\u{fe0f}", Some("example.com")),
            ("https://example.com/a\u{301}b", Some("example.com")),
            (
                "https://\u{1f600}.\u{1f600}.example.com/x",
                Some("example.com"),
            ),
            ("https://user\u{2024}name@example.com/x", Some("example.com")),
            (
                "http://\u{43c}\u{43e}\u{441}\u{43a}\u{432}\u{430}.\u{440}\u{444}/\u{43f}",
                Some("\u{440}\u{444}"),
            ),
            ("file:///tmp/\u{1f4c1}/a.md", None),
            ("not a url \u{1f600} at all \u{1f4a9}", None),
            ("", None),
            ("\u{1f600}", None),
            ("https://\u{ff03}.example.com/x", Some("example.com")),
            (
                "https://\u{10d0}\u{10d1}\u{10d2}.example.com/x",
                Some("example.com"),
            ),
            ("https://\u{624b}\u{514c}.jp/\u{1f44c}", None),
        ];
        for (url, given) in corpus {
            // The display the URL would have if it all fitted, which is what anything shown must be made
            // of: characters of it, in order, cut only at character boundaries.
            let full = match formatted(url, given) {
                Some(parts) => parts.head(),
                None => url.to_owned(),
            };
            for max in 0..=80usize {
                let shown = unfocused_display(url, given, max);
                assert!(
                    count(&shown.text) <= max,
                    "{url} at {max}: {:?}",
                    shown.text
                );
                assert!(shown.text.is_char_boundary(shown.text.len()));
                assert!(!shown.text.contains('\u{fffd}'), "{:?} for {url}", shown.text);
                let mut rest = full.chars();
                for c in shown.text.chars().filter(|c| *c != ELLIPSIS) {
                    assert!(
                        rest.by_ref().any(|next| next == c),
                        "{:?} is not a run of the display of {url} ({full:?})",
                        shown.text
                    );
                }
                assert_eq!(shown.elided, count(&full) > max, "{url} at {max}");
                if let Some(domain) = &shown.domain {
                    let text = shown.text.get(domain.clone()).expect("range inside text");
                    assert!(full.contains(text), "{text:?} is not a part of {full:?}");
                    assert!(!text.contains('\u{fffd}'));
                }
            }
        }
    }

    #[test]
    fn elided_text_at_most_max_chars() {
        let urls = [
            (SIGN_IN, Some("google.com")),
            ("https://example.com", Some("example.com")),
            ("http://example.com/a?b=c#d", Some("example.com")),
            (
                "https://www.sub.sub.sub.example.co.uk/x",
                Some("example.co.uk"),
            ),
            ("https://m\u{00fc}nchen.de/\u{1f600}", Some("xn--mnchen-3ya.de")),
            ("http://[::1]:8080/a/b/c/d/e/f", None),
            ("about:config", None),
            ("file:///very/long/path/to/a/file.txt", None),
            ("https://\u{430}pple.com/x", Some("xn--pple-43d.com")),
            (
                "garbage that is not a url at all \u{1f600}\u{1f600}",
                Some("example.com"),
            ),
            (
                "https://\u{430}pple.com@shop.example.co.uk/a?b=\u{1f600}",
                Some("example.co.uk"),
            ),
        ];
        for (url, given) in urls {
            for max in [0, 1, 2, 3, 7, 8, 11, 12, 13, 19, 20, 33, 64, 1_000] {
                let shown = unfocused_display(url, given, max);
                assert!(
                    count(&shown.text) <= max,
                    "{url} at {max} gave {:?}",
                    shown.text
                );
                assert!(!shown.text.contains('\u{fffd}'));
                if shown.elided {
                    // Either the marker shows, or there was no room for it and the protected host part is
                    // exactly as wide as the field.
                    assert!(
                        shown.text.contains(ELLIPSIS) || count(&shown.text) == max,
                        "{url} at {max} hid something without saying so: {:?}",
                        shown.text
                    );
                }
                if let Some(domain) = &shown.domain {
                    assert!(shown.text.get(domain.clone()).is_some());
                }
            }
        }
    }

    #[test]
    fn unparseable_input_truncated() {
        let text = "hello world \u{1f600} this is not a url";
        let shown = unfocused_display(text, None, 40);
        assert_eq!(shown.text, text);
        assert!(!shown.elided);
        assert_eq!(shown.domain, None);
        let short = unfocused_display(text, Some("example.com"), 12);
        assert_eq!(count(&short.text), 12);
        assert!(short.text.ends_with(ELLIPSIS));
        assert!(short.elided);
        assert_eq!(short.text, "hello world\u{2026}");
        // Text the parser refuses is shown as typed, whatever it looks like.
        for bad in [
            "http://",
            "https://exa mple.com",
            ":::",
            "\u{1f600}",
            "https://",
            "//example.com/x",
        ] {
            let shown = unfocused_display(bad, Some("example.com"), 60);
            assert_eq!(shown.text, bad, "for {bad}");
            assert_eq!(shown.domain, None, "for {bad}");
        }
    }

    #[test]
    fn focused_scroll_caret_at_end() {
        // Typing to the end of a long URL: the view moves just far enough to keep the caret visible.
        assert_eq!(focused_scroll(100, 100, 60), 40);
        assert_eq!(focused_scroll(200, 200, 40), 160);
        assert_eq!(
            focused_scroll(count(SIGN_IN), count(SIGN_IN), 60),
            count(SIGN_IN) - 60,
        );
    }

    #[test]
    fn focused_scroll_short_text_is_zero() {
        assert_eq!(focused_scroll(10, 10, 60), 0);
        assert_eq!(focused_scroll(60, 60, 60), 0);
        // A click into the field selects everything and leaves the caret at 0, so the start is shown.
        assert_eq!(focused_scroll(100, 0, 60), 0);
        assert_eq!(focused_scroll(0, 0, 0), 0);
    }

    #[test]
    fn focused_scroll_caret_inside() {
        assert_eq!(focused_scroll(100, 30, 60), 0);
        assert_eq!(focused_scroll(100, 59, 60), 0);
        assert_eq!(focused_scroll(100, 60, 60), 0);
        assert_eq!(focused_scroll(100, 61, 60), 1);
        assert_eq!(focused_scroll(100, 80, 60), 20);
        // A stale caret past the end of the text is clamped, never scrolled off it.
        assert_eq!(focused_scroll(100, 500, 60), 40);
    }
}
