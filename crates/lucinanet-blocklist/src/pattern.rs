/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! One URL pattern (`||host^path*x|`): parsing and matching. No regular expressions and no backtracking: the pattern is
//! split at `*` into segments and each segment is matched leftmost, which is exact for wildcards and linear per segment.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Anchor {
    None,
    /// `|` at the start: the pattern begins at the start of the URL.
    Start,
    /// `||`: the pattern begins at the start of the host or right after a dot inside it.
    Domain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum El {
    Lit(u8),
    /// `^`: a separator character, or the end of the URL.
    Sep,
}

#[derive(Clone, Debug)]
pub(crate) struct Pattern {
    pub anchor: Anchor,
    pub end_anchor: bool,
    /// The text began with `*`: the first segment may start anywhere (so a start anchor means nothing).
    pub left_open: bool,
    pub right_open: bool,
    /// Non-empty segments between wildcards.
    pub segs: Vec<Vec<El>>,
}

/// A separator is an ASCII character that is not a letter, digit, `_`, `-`, `.` or `%`.
pub(crate) fn is_sep(b: u8) -> bool {
    b.is_ascii() && !(b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'%'))
}

impl Pattern {
    /// `p` is the pattern text without `@@` and without options. With `match_case` false the literals are lower-cased and the
    /// text given to `is_match` must be lower-cased too.
    pub fn parse(p: &str, match_case: bool) -> Pattern {
        let mut s = p.as_bytes();
        let mut anchor = Anchor::None;
        if let Some(rest) = s.strip_prefix(b"||") {
            anchor = Anchor::Domain;
            s = rest;
        } else if let Some(rest) = s.strip_prefix(b"|") {
            anchor = Anchor::Start;
            s = rest;
        }
        let mut end_anchor = false;
        if let Some(rest) = s.strip_suffix(b"|") {
            end_anchor = true;
            s = rest;
        }
        let left_open = s.first() == Some(&b'*');
        let right_open = s.last() == Some(&b'*');
        if left_open {
            anchor = Anchor::None;
        }
        if right_open {
            end_anchor = false;
        }
        let segs = s
            .split(|&b| b == b'*')
            .filter(|seg| !seg.is_empty())
            .map(|seg| {
                seg.iter()
                    .map(|&b| match b {
                        b'^' => El::Sep,
                        _ if match_case => El::Lit(b),
                        _ => El::Lit(b.to_ascii_lowercase()),
                    })
                    .collect()
            })
            .collect();
        Pattern { anchor, end_anchor, left_open, right_open, segs }
    }

    /// True when the pattern has no literal or separator at all (it matches every URL).
    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    /// `host` is the byte range of the host inside `text`, needed by `||`.
    pub fn is_match(&self, text: &[u8], host: Option<(usize, usize)>) -> bool {
        let n = self.segs.len();
        if n == 0 {
            return true;
        }
        let len = text.len();
        let mut pos;
        match self.anchor {
            Anchor::Start => match seg_at(&self.segs[0], text, 0) {
                Some(e) if n > 1 || !self.end_anchor || e == len => pos = e,
                _ => return false,
            },
            Anchor::Domain => {
                let Some((hs, he)) = host else { return false };
                let mut found = None;
                let starts = std::iter::once(hs).chain((hs..he).filter(|&k| text.get(k) == Some(&b'.')).map(|k| k + 1));
                for s in starts {
                    if let Some(e) = seg_at(&self.segs[0], text, s)
                        && (n > 1 || !self.end_anchor || e == len)
                    {
                        found = Some(e);
                        break;
                    }
                }
                match found {
                    Some(e) => pos = e,
                    None => return false,
                }
            }
            Anchor::None => {
                if n == 1 {
                    return match_last(&self.segs[0], text, 0, self.end_anchor);
                }
                match find_leftmost(&self.segs[0], text, 0) {
                    Some((_, e)) => pos = e,
                    None => return false,
                }
            }
        }
        if n == 1 {
            return true;
        }
        for seg in &self.segs[1..n - 1] {
            match find_leftmost(seg, text, pos) {
                Some((_, e)) => pos = e,
                None => return false,
            }
        }
        match_last(&self.segs[n - 1], text, pos, self.end_anchor)
    }
}

/// Match `seg` starting exactly at `i`; returns where it ends. `^` also matches the end of the text (consuming nothing).
fn seg_at(seg: &[El], text: &[u8], mut i: usize) -> Option<usize> {
    for el in seg {
        match *el {
            El::Lit(b) => {
                if text.get(i) != Some(&b) {
                    return None;
                }
                i += 1;
            }
            El::Sep => {
                if i == text.len() {
                    continue;
                }
                if !is_sep(text[i]) {
                    return None;
                }
                i += 1;
            }
        }
    }
    Some(i)
}

fn find_leftmost(seg: &[El], text: &[u8], from: usize) -> Option<(usize, usize)> {
    (from..=text.len()).find_map(|s| seg_at(seg, text, s).map(|e| (s, e)))
}

/// The final segment: anywhere at or after `from`, or ending exactly at the end of the text when the pattern has `|` there.
fn match_last(seg: &[El], text: &[u8], from: usize, end_anchor: bool) -> bool {
    if !end_anchor {
        return find_leftmost(seg, text, from).is_some();
    }
    let len = text.len();
    (len.saturating_sub(seg.len()).max(from)..=len).any(|s| seg_at(seg, text, s) == Some(len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, url: &str) -> bool {
        let host = crate::domain::host_range(url);
        Pattern::parse(p, false).is_match(url.as_bytes(), host)
    }

    #[test]
    fn anchors_and_wildcards() {
        assert!(m("||a.test^", "https://a.test/"));
        assert!(m("|https://a|", "https://a"));
        assert!(!m("|https://a|", "https://ab"));
        assert!(m("a*b*c", "xxaxxbxxc"));
        assert!(!m("a*b*c", "xxaxxcxxb"));
        assert!(m("*", "anything"));
        assert!(m("", "anything"));
        assert!(m("a^", "xa"));
        assert!(!m("a^", "xab"));
    }

    #[test]
    fn end_anchor_with_multiple_segments() {
        assert!(m("a*b|", "a--b"));
        assert!(!m("a*b|", "a--bx"));
        assert!(m("a*^|", "a--"));
    }
}
