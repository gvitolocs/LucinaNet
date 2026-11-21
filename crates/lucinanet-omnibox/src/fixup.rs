/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! URL fix-ups that need no network and no file system.

/// Repairs a mistyped scheme separator or scheme name, returning the corrected text, or `None` when nothing applies.
///
/// Handled (the rest of the text is kept as typed):
/// - `http:/x`, `http:x`, `http//x`, `http;//x` become `http://x`, and the same for `https` and `ftp`;
/// - `file:/x` becomes `file:///x`, `file//x` and `file;//x` become `file://x`;
/// - the scheme typos Firefox repairs (`ttp`, `htp` for http; `ttps`, `tps`, `ps`, `htps` for https; `ile`, `le` for
///   file), only when `//` follows, because `ps:aux` or `le:foo` are far more likely searches than typos.
///
/// Text that already reads `scheme://x`, `http:8080` (a host and port) and text without a scheme give `None`.
pub fn fix_scheme_typos(typed: &str) -> Option<String> {
    let text = typed.trim();
    let name_len = text.bytes().take_while(|b| b.is_ascii_alphabetic()).count();
    if name_len == 0 {
        return None;
    }
    let (name, after) = text.split_at(name_len);
    // Firefox docshell/base/URIFixup.sys.mjs @33abb90 extractScheme: the typo table.
    let (canon, is_typo): (&str, bool) = match name.to_ascii_lowercase().as_str() {
        "http" => ("http", false),
        "https" => ("https", false),
        "ftp" => ("ftp", false),
        "file" => ("file", false),
        "ttp" | "htp" => ("http", true),
        "ttps" | "tps" | "ps" | "htps" => ("https", true),
        "ile" | "le" => ("file", true),
        _ => return None,
    };
    let (separator, tail) = match after.chars().next() {
        Some(c @ (':' | ';')) => (Some(c), &after[1..]),
        _ => (None, after),
    };
    let slashes = tail.bytes().take_while(|b| *b == b'/').count();
    let rest = &tail[slashes..];
    if rest.is_empty() {
        return None;
    }
    let needs_fix = if is_typo {
        slashes == 2
    } else if canon == "file" {
        if separator.is_some() && slashes == 1 {
            return Some(format!("file:///{rest}"));
        }
        slashes == 2 && separator != Some(':')
    } else {
        match slashes {
            // Chromium components/url_formatter/url_fixer.cc @e9a4673 GetValidScheme: "www:123/" has a port, not a scheme.
            0 => separator.is_some() && !starts_with_port(rest),
            1 => separator.is_some(),
            2 => separator != Some(':'),
            _ => false,
        }
    };
    needs_fix.then(|| format!("{canon}://{rest}"))
}

/// `8080`, `8080/path`, `8080?x`: one to five digits, then the end of the authority.
fn starts_with_port(rest: &str) -> bool {
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    (1..=5).contains(&digits)
        && matches!(rest.as_bytes().get(digits), None | Some(b'/' | b'?' | b'#'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_slash_and_missing_slashes_are_repaired() {
        assert_eq!(fix_scheme_typos("http:/example.com").as_deref(), Some("http://example.com"));
        assert_eq!(fix_scheme_typos("http:example.com").as_deref(), Some("http://example.com"));
        assert_eq!(fix_scheme_typos("https//x.org").as_deref(), Some("https://x.org"));
        assert_eq!(fix_scheme_typos("HTTPS:/x.org/a?b").as_deref(), Some("https://x.org/a?b"));
        assert_eq!(fix_scheme_typos("http;//x.org").as_deref(), Some("http://x.org"));
        assert_eq!(fix_scheme_typos("ftp:files.org").as_deref(), Some("ftp://files.org"));
    }

    #[test]
    fn file_forms() {
        assert_eq!(fix_scheme_typos("file:/etc/hosts").as_deref(), Some("file:///etc/hosts"));
        assert_eq!(fix_scheme_typos("file//etc/hosts").as_deref(), Some("file://etc/hosts"));
        assert_eq!(fix_scheme_typos("file:///etc/hosts"), None);
    }

    #[test]
    fn scheme_name_typos_need_double_slash() {
        assert_eq!(fix_scheme_typos("ttp://a.b").as_deref(), Some("http://a.b"));
        assert_eq!(fix_scheme_typos("htp://a.b").as_deref(), Some("http://a.b"));
        assert_eq!(fix_scheme_typos("htps://a.b/x").as_deref(), Some("https://a.b/x"));
        assert_eq!(fix_scheme_typos("tps://a.b").as_deref(), Some("https://a.b"));
        assert_eq!(fix_scheme_typos("ps://a.b").as_deref(), Some("https://a.b"));
        assert_eq!(fix_scheme_typos("ile://a").as_deref(), Some("file://a"));
        assert_eq!(fix_scheme_typos("ps:aux"), None);
    }

    #[test]
    fn nothing_to_fix() {
        for text in ["http://ok.com", "example.com", "http:8080", "http:8080/x", "http", "http://", "", "  ", "mailto:a@b.c", "1http://x"] {
            assert_eq!(fix_scheme_typos(text), None, "{text}");
        }
    }
}
