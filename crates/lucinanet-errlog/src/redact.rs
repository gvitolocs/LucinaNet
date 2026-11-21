/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! `redact`: what a URL and a message may be turned into before the log keeps them. These three
//! functions are where the privacy of the whole file comes from, so everything else in the crate
//! assumes they ran first.
//!
//! A URL that arrives here was written by a page, so it may be relative, empty, full of spaces, or a
//! `data:` whose body is the page's own text. We read them by hand rather than with a URL parser
//! because a parser *repairs* what it is given — it guesses a scheme, completes a path, punycodes a
//! host — and a guess about someone's address is a string we would then be keeping about a page that
//! never said it. What we do instead is read at most two pieces of it and drop the rest:
//!
//! - the host: the authority without its user info and its port, lower-cased, and only for `http`
//!   and `https`, which are the schemes that have one;
//! - the file: the last segment of the path without its query or fragment, at most
//!   [`FILE_MAX_CHARS`] characters, and only for `http`, `https` and `file` — the last segment of a
//!   `data:` URL is the end of someone's script.
//!
//! A non-ASCII host is kept as it was written, not punycoded. The log holds no IDNA table, and
//! `bücher.example` and `xn--bcher-kva.example` grouping apart is a worse mistake than a host shown
//! the way the address bar showed it.

use crate::model::FILE_MAX_CHARS;

/// The host a URL names: lower-case, no user info, no port, no path, no query, no trailing dot.
/// Empty for anything that is not an `http` or `https` URL — including `data:` and `blob:`, whose
/// bodies are pages — and for text that is not a URL at all.
pub fn host_of(url: &str) -> String {
    let Some((Scheme::Http | Scheme::Https, rest)) = scheme_of(url) else {
        return String::new();
    };
    let Some(body) = rest.strip_prefix("//") else {
        return String::new(); // `https:example.com` is not an address.
    };
    let authority = body.split(['/', '\\', '?', '#']).next().unwrap_or("");
    // The user info runs up to the last `@` before the host, so `a@b@host.example` reads as
    // `host.example` and not as `b@host.example`.
    let host_and_port = authority.rsplit('@').next().unwrap_or("");
    let host = match host_and_port.strip_prefix('[') {
        // A bracketed literal is a host in its own right: `[::1]`, and nothing after the `]` but a
        // port. An unclosed bracket is not an address, and neither is a bracket around a name.
        Some(bracketed) => {
            let Some((literal, after)) = bracketed.split_once(']') else {
                return String::new();
            };
            // After the closing bracket there is nothing, or a port and nothing else.
            let after_is_a_port = match after.strip_prefix(':') {
                Some(digits) => !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
                None => after.is_empty(),
            };
            if literal.contains(':') && after_is_a_port {
                return format!("[{}]", literal.to_lowercase());
            }
            return String::new();
        }
        None => host_and_port.split(':').next().unwrap_or(""),
    };
    let host = host.trim_end_matches('.');
    // Real host names only: letters, digits, `.`, `-`, `_` (the last turns up in LAN names written by
    // hand) and non-ASCII labels; no empty label, no hyphen at either end of a label, no label over
    // the 63 characters a name may have, and nothing so long that it is somebody's text rather than
    // a host.
    if host.is_empty()
        || host.len() > 253
        || !host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_') || b >= 0x80)
        || host.split('.').any(|label| label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-'))
    {
        return String::new();
    }
    host.to_lowercase()
}

/// The last segment of a URL's path — the file, without its query or fragment, at most
/// [`FILE_MAX_CHARS`] characters, whitespace and control characters gone. Empty when there is no
/// name there (`https://x.example/pages/`), when the scheme's body is a page rather than a location
/// (`data:`, `blob:`, `javascript:`), or when the text is not a URL.
///
/// A URL with no scheme is read as a path, because that is what a stack report gives: `app.js`,
/// `./js/app.min.js` and `/assets/js/app.min.js` all name `app.min.js`.
pub fn file_of(url: &str) -> String {
    let (scheme, rest) = match scheme_of(url) {
        Some(found) => found,
        // No scheme, or a colon that is inside a path: the whole string is then the path.
        None => {
            let url = url.trim();
            if url.contains('/') || url.contains('\\') {
                (Scheme::None, url)
            } else {
                return String::new(); // A word, not a location.
            }
        }
    };
    if !matches!(scheme, Scheme::None | Scheme::Http | Scheme::Https | Scheme::File) {
        return String::new();
    }
    // With an authority the path is what follows it; `scheme://host` alone names no file.
    let path = match rest.strip_prefix("//") {
        Some(body) => match body.find(['/', '\\']) {
            Some(at) => &body[at..],
            None => return String::new(),
        },
        None if matches!(scheme, Scheme::None) => rest,
        None => return String::new(), // `file:x` and every opaque body.
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    let last = path.rsplit('/').next().unwrap_or("");
    if last.is_empty() {
        return String::new();
    }
    // Cut the raw name before cleaning it, so that a path whose last segment is somebody's megabyte
    // of text costs one bounded copy and not the whole of it.
    let last = truncate(last, 3 * FILE_MAX_CHARS);
    truncate(&clean(&last), FILE_MAX_CHARS)
}

/// What a detail may hold. Redaction on a message, which is text a page wrote and which can
/// therefore contain anything the page can see:
///
/// - in a token that looks like a URL, everything from the first `?` or `#` on goes, and so does
///   `user:pass@` in front of the host;
/// - a bare e-mail becomes `<email>`;
/// - a run of 24 characters or more that could be an identifier — hex, or base64's alphabet with a
///   digit or a sign in it — becomes `<id>`, which is what a token, a session key and a SHA look
///   like;
/// - control characters go and runs of spaces collapse, and the result has no space at either end,
///   because a line of the file is one line.
///
/// A plain long word is left alone: the names in a `TypeError` are the reason to keep the message at
/// all. Redacting an already redacted detail changes nothing, which lets the wiring redact a line it
/// read back out of an older file.
pub fn redact_detail(s: &str) -> String {
    // Control characters first, as spaces: a message arrives with the page's own line breaks in it,
    // and `\u{1}` is not whitespace but is still a character that must not reach a line of the file.
    let flattened: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let mut out = String::with_capacity(s.len());
    for (index, token) in flattened.split_whitespace().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        out.push_str(&redact_token(token));
    }
    out
}

/// One non-whitespace run. Wrapping punctuation (`(https://x/a?b)`, `<user@mail.com>,`) is peeled
/// off and put back, so the sentence still reads and it is the wrapped thing that gets redacted.
fn redact_token(token: &str) -> String {
    let open = token.len() - token.trim_start_matches(['(', '[', '{', '<', '\'', '"', '`', '*', '_']).len();
    let close =
        token.len() - token.trim_end_matches([')', ']', '}', '>', '\'', '"', '`', '.', ',', ';', ':', '!', '?', '*']).len();
    if open + close >= token.len() {
        return redact_ids(token);
    }
    let (before, suffix) = token.split_at(token.len() - close);
    let (prefix, core) = before.split_at(open);
    let mut redacted = if core.contains("://") || core.starts_with("//") {
        redact_url(core)
    } else if is_email(core) {
        "<email>".to_string()
    } else if let Some(host) = credential_at(core) {
        host
    } else {
        redact_ids(core)
    };
    // An e-mail written between angle brackets is replaced by a placeholder that is itself bracketed:
    // `<a.person@corp.example>` would otherwise come back as `<<email>>`. The brackets go with the
    // thing they quoted.
    if prefix.ends_with('<') && suffix.starts_with('>') && redacted.starts_with('<') && redacted.ends_with('>') {
        redacted = redacted[1..redacted.len() - 1].to_string();
    }
    format!("{prefix}{redacted}{suffix}")
}

/// A URL-shaped token: cut at its query and its fragment, and drop the user info from its authority.
fn redact_url(core: &str) -> String {
    let without_tail = core.split(['?', '#']).next().unwrap_or("");
    let Some(at) = without_tail.find("://") else {
        return redact_ids(without_tail); // Scheme-relative: there is no authority to strip.
    };
    let (scheme, rest) = without_tail.split_at(at + 3);
    let authority_end = rest.find(['/', '\\']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    match authority.rfind('@') {
        Some(at) => format!("{scheme}{}{path}", &authority[at + 1..]),
        None => without_tail.to_string(),
    }
}

/// `user:pass@host.example/path` with no scheme in front of it, the way a `Failed to fetch` message
/// quotes it: the part after the last `@`, query and fragment gone. `None` unless there is a colon
/// in front of the `@`, so an ordinary word is left to [`redact_ids`].
fn credential_at(core: &str) -> Option<String> {
    let without_tail = core.split(['?', '#']).next()?;
    let at = without_tail.rfind('@')?;
    let (userinfo, host_and_path) = without_tail.split_at(at + 1);
    if !userinfo[..userinfo.len() - 1].contains(':') || !host_and_path.contains('.') {
        return None;
    }
    Some(host_and_path.to_string())
}

/// Mask the identifier-looking runs inside a token and keep the rest of it.
fn redact_ids(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    // The empty final round is what flushes a run that reaches the end of the token: nothing is
    // appended for it, so a redacted detail gains no characters of its own.
    for (c, is_last) in s.chars().map(|c| (c, false)).chain(std::iter::once((' ', true))) {
        if !is_last && (c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-')) {
            run.push(c);
            continue;
        }
        out.push_str(if looks_like_id(&run) { "<id>" } else { run.as_str() });
        run.clear();
        if !is_last {
            out.push(c);
        }
    }
    out
}

/// 24 characters or more of the identifier alphabet, carrying a digit or a sign — or a run of hex,
/// which is what a UUID without its dashes and a git object id look like. Long words are names.
fn looks_like_id(run: &str) -> bool {
    if run.chars().count() < 24 {
        return false;
    }
    let (digit_or_sign, hex) = run.chars().fold((false, true), |(digit_or_sign, hex), c| {
        (digit_or_sign || c.is_ascii_digit() || matches!(c, '+' | '/' | '='), hex && c.is_ascii_hexdigit())
    });
    digit_or_sign || hex
}

/// A bare `someone@somewhere.example`. A URL's user info is not an e-mail, and [`redact_url`] and
/// [`credential_at`] deal with that instead.
fn is_email(core: &str) -> bool {
    let Some((local, domain)) = core.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && core.matches('@').count() == 1
        && !core.contains(['/', '\\', ':'])
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.starts_with('-')
        && !domain.ends_with('.')
}

/// The schemes a log cares about, and the part of the URL after the `:`. Anything with a colon in
/// front of its first separator has no scheme, and gives `None` so that the caller can read the
/// whole string as a path.
enum Scheme {
    None,
    Http,
    Https,
    File,
    Other,
}

fn scheme_of(url: &str) -> Option<(Scheme, &str)> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let at = url.find(':')?;
    let name = &url[..at];
    if at == 0 || name.contains('/') || name.contains('\\') || !is_scheme(name) {
        return None;
    }
    let scheme = if name.eq_ignore_ascii_case("http") {
        Scheme::Http
    } else if name.eq_ignore_ascii_case("https") {
        Scheme::Https
    } else if name.eq_ignore_ascii_case("file") {
        Scheme::File
    } else {
        Scheme::Other
    };
    Some((scheme, &url[at + 1..]))
}

fn is_scheme(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

/// Whitespace and control characters out of a name read off a URL.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect()
}

/// First `max` characters, on a character boundary.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((at, _)) => s[..at].to_string(),
        None => s.to_string(),
    }
}
