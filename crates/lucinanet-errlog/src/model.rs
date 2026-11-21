/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The event: one line of `errors.jsonl`, and what each [`Kind`] is allowed to put in it.
//!
//! The fields are the whole privacy story, so they are worth reading once. There is no field for a
//! path, a query, a cookie, a form value, a search box or a piece of page text; there are two hosts,
//! one file name, one line number, one short message, and two numbers. The constructors are what
//! keep it that way — they run every URL through [`crate::host_of`] and [`crate::file_of`] and every
//! piece of text through [`crate::redact_detail`] before it lands, so the wiring cannot leak by
//! passing the wrong thing, and a person reading the file sees no more than the address bar and a
//! status line already told them.
//!
//! The line format is JSON, one object per line, written and read with the `serde_json` the
//! workspace already uses — written by hand from the fields rather than derived, so that the key set
//! is this crate's decision and a field a future build adds cannot silently appear in an old one's
//! file. Keys come out in name order, which makes two files from two builds diffable.

use serde_json::{Value, json};

use crate::redact::{file_of, host_of, redact_detail, truncate};

/// How much of a message is kept. A stack trace and a page's worth of text both arrive in an
/// exception message; the first two hundred characters say what broke, and the rest is the page.
pub const DETAIL_MAX_CHARS: usize = 200;

/// How long a file name may be. Names are short; a 300-character one is a content hash, which
/// identifies a build of a page rather than the page.
pub const FILE_MAX_CHARS: usize = 80;

/// What happened. The five things the browser reports, plus [`Kind::Truncated`], which the log
/// writes about itself when a session has filled its share of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// A JavaScript exception: the engine's own message, the script and the line in it.
    ScriptException,
    /// A Web API the page reached for that this build does not have (`Intl`,
    /// `requestIdleCallback`), and how often it reached.
    MissingApi,
    /// A subresource — script, stylesheet, image, fetch — that never arrived, and why.
    ResourceFailed,
    /// A panic caught in one of the browser's threads while the page was loading.
    EnginePanic,
    /// A page that started and never finished, left for more than its budget of seconds.
    PageNeverLoaded,
    /// Not a page error. The log's own line saying it stopped writing this session.
    Truncated,
}

impl Kind {
    /// The name in the file. Stable: it is what an older or a newer build reads back, and an unknown
    /// name costs the line rather than the file.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::ScriptException => "ScriptException",
            Kind::MissingApi => "MissingApi",
            Kind::ResourceFailed => "ResourceFailed",
            Kind::EnginePanic => "EnginePanic",
            Kind::PageNeverLoaded => "PageNeverLoaded",
            Kind::Truncated => "Truncated",
        }
    }

    /// The name `about:errors` shows.
    pub fn label(self) -> &'static str {
        match self {
            Kind::ScriptException => "script error",
            Kind::MissingApi => "missing API",
            Kind::ResourceFailed => "failed request",
            Kind::EnginePanic => "LucinaNet fault",
            Kind::PageNeverLoaded => "page never loaded",
            Kind::Truncated => "cut off",
        }
    }

    /// The reverse of [`Kind::as_str`].
    pub fn parse(name: &str) -> Option<Kind> {
        Some(match name {
            "ScriptException" => Kind::ScriptException,
            "MissingApi" => Kind::MissingApi,
            "ResourceFailed" => Kind::ResourceFailed,
            "EnginePanic" => Kind::EnginePanic,
            "PageNeverLoaded" => Kind::PageNeverLoaded,
            "Truncated" => Kind::Truncated,
            _ => return None,
        })
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One recorded error.
///
/// Build one with the per-kind functions — [`Event::script`], [`Event::missing_api`],
/// [`Event::resource_failed`], [`Event::engine_panic`], [`Event::page_never_loaded`] — rather than a
/// literal. Those are where the redaction happens, and a literal is where a leak would come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Milliseconds since the Unix epoch. The clock belongs to the wiring; this crate never reads it.
    pub t_ms: u64,
    /// Which browser session wrote it, counting from 1.
    pub session: u32,
    /// Which tab, so that two tabs of one site are not read as one page. Not part of an error's
    /// identity: the same failure in two tabs is one line, counted twice.
    pub tab: u32,
    /// The host of the page it happened on, empty when the page has none (`about:`, `data:`, a new
    /// tab). This is what `about:errors` groups by.
    pub page_host: String,
    pub kind: Kind,
    /// Per kind: the message, the API name, the status or error class, the seconds waited. Redacted
    /// and at most [`DETAIL_MAX_CHARS`] characters.
    pub detail: String,
    /// The host of the script or resource the error is about, empty when it is about the page itself
    /// or about us.
    pub host: String,
    /// The name of that script, or the last two segments of a LucinaNet source path. Never a
    /// directory, never a query.
    pub file: String,
    /// Line within `file`, 0 when unknown.
    pub line: u32,
    /// How often this exact error had happened by the time this line was written. A repeat is
    /// written again only when the count reaches the next power of two, so the newest line for an
    /// error is its running total.
    pub count: u32,
}

impl Event {
    /// A JavaScript exception. `message` is the engine's own; `page_url` and `script_url` are kept
    /// only as a host and a name.
    pub fn script(
        t_ms: u64,
        session: u32,
        tab: u32,
        page_url: &str,
        script_url: &str,
        line: u32,
        message: &str,
    ) -> Event {
        Event {
            t_ms,
            session,
            tab,
            page_host: host_of(page_url),
            kind: Kind::ScriptException,
            detail: detail(message),
            host: host_of(script_url),
            file: file_of(script_url),
            line,
            count: 1,
        }
    }

    /// A call to an API this build does not have. `api` is the name the page reached for and `count`
    /// how many calls the wiring had already counted for it — the one number a missing API is worth
    /// recording, since the page went on without it.
    pub fn missing_api(t_ms: u64, session: u32, tab: u32, page_url: &str, api: &str, count: u32) -> Event {
        Event {
            t_ms,
            session,
            tab,
            page_host: host_of(page_url),
            kind: Kind::MissingApi,
            detail: detail(api),
            host: String::new(),
            file: String::new(),
            line: 0,
            count: count.max(1),
        }
    }

    /// A subresource that never arrived. `reason` is the HTTP status as text ("404", "503") or the
    /// error class ("dns", "tls", "reset"), never a server's message body. The resource is kept as a
    /// host only: the path of a failed image says more about what the person was looking at than a
    /// log should.
    pub fn resource_failed(t_ms: u64, session: u32, tab: u32, page_url: &str, resource_url: &str, reason: &str) -> Event {
        Event {
            t_ms,
            session,
            tab,
            page_host: host_of(page_url),
            kind: Kind::ResourceFailed,
            detail: detail(reason),
            host: host_of(resource_url),
            file: String::new(),
            line: 0,
            count: 1,
        }
    }

    /// A panic inside the browser. `path` is the source file it came from, kept as at most its last
    /// two segments: `/home/you/Projects/LucinaNet/crates/x/src/engine.rs` becomes `src/engine.rs`,
    /// which names the code without naming the person's directory or their machine.
    pub fn engine_panic(t_ms: u64, session: u32, tab: u32, page_url: &str, path: &str, message: &str) -> Event {
        Event {
            t_ms,
            session,
            tab,
            page_host: host_of(page_url),
            kind: Kind::EnginePanic,
            detail: detail(message),
            host: String::new(),
            file: last_segments(path),
            line: 0,
            count: 1,
        }
    }

    /// A page that was still loading when its budget ran out. `seconds` is the budget, not the wait:
    /// the number a person would have to be told.
    pub fn page_never_loaded(t_ms: u64, session: u32, tab: u32, page_url: &str, seconds: u32) -> Event {
        Event {
            t_ms,
            session,
            tab,
            page_host: host_of(page_url),
            kind: Kind::PageNeverLoaded,
            detail: format!("{seconds}s"),
            host: String::new(),
            file: String::new(),
            line: 0,
            count: 1,
        }
    }

    /// The whole event as one line of JSON, no newline at the end.
    pub fn to_line(&self) -> String {
        let value = json!({
            "t_ms": self.t_ms,
            "session": self.session,
            "tab": self.tab,
            "page_host": self.page_host,
            "kind": self.kind.as_str(),
            "detail": self.detail,
            "host": self.host,
            "file": self.file,
            "line": self.line,
            "count": self.count,
        });
        // Only a key that is not a string could fail here, and every key above is a literal.
        serde_json::to_string(&value).unwrap_or_default()
    }

    /// One line of the file back into an event. `None` is a line to skip and count: not JSON, not an
    /// object, no kind we know, no time on it. Everything else is read leniently — a missing field
    /// costs that field and not the line, because the rest of the session is still worth showing.
    pub fn from_line(line: &str) -> Option<Event> {
        let value: Value = serde_json::from_str(line).ok()?;
        let object = value.as_object()?;
        let kind = Kind::parse(object.get("kind")?.as_str()?)?;
        let t_ms = object.get("t_ms")?.as_u64()?;
        let text = |name: &str| object.get(name).and_then(Value::as_str).unwrap_or("").to_string();
        let number = |name: &str| object.get(name).and_then(Value::as_u64).unwrap_or(0) as u32;
        Some(Event {
            t_ms,
            session: number("session"),
            tab: number("tab"),
            page_host: text("page_host"),
            kind,
            detail: text("detail"),
            host: text("host"),
            file: text("file"),
            line: number("line"),
            // A line with no count, or a count of none, happened once.
            count: object.get("count").and_then(Value::as_u64).unwrap_or(1).clamp(1, u64::from(u32::MAX)) as u32,
        })
    }
}

/// A detail from raw text: redacted, then cut to [`DETAIL_MAX_CHARS`]. In that order, because the
/// thing that gets cut off is the tail of a message and the thing that gets redacted is inside it.
fn detail(text: &str) -> String {
    truncate(&redact_detail(text), DETAIL_MAX_CHARS)
}

/// The last two segments of a source path, with either kind of separator. Shorter paths keep what
/// they have; a path with no separator in it is a file name already, and a name of none is nothing.
fn last_segments(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|part| !part.is_empty()).collect();
    let kept = match parts.len() {
        0 => String::new(),
        1 | 2 => parts.join("/"),
        _ => parts[parts.len() - 2..].join("/"),
    };
    truncate(&redact_detail(&kept), FILE_MAX_CHARS)
}
