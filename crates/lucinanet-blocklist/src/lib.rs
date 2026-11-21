/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Content blocking: a matcher for the network-filter subset of the Adblock-style syntax. It parses filter lines and
//! answers "block this request?" without scanning every rule. No list is bundled or fetched, and there is no cosmetic
//! filtering: the caller supplies the lines and acts on the verdict.

#![forbid(unsafe_code)]

pub mod domain;
mod engine;
mod parse;
mod pattern;

pub use engine::Engine;

/// What a request loads, as the filter options name it. The discriminant is the bit position used by type options.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResourceType {
    Document,
    Subdocument,
    Stylesheet,
    Script,
    Image,
    Font,
    Media,
    XmlHttpRequest,
    Ping,
    WebSocket,
    Object,
    Other,
}

/// One request to judge.
pub struct Request<'a> {
    /// Absolute http(s)/ws(s) URL, as the engine requests it.
    pub url: &'a str,
    /// Host of the page (top-level document or frame) that makes the request; `None` for the page load itself.
    pub source_host: Option<&'a str>,
    pub resource: ResourceType,
    /// Set when the caller already knows (it has a public-suffix list); `None` lets the engine decide with
    /// [`domain::is_third_party`].
    pub third_party: Option<bool>,
}

/// The answer to [`Engine::check`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// No filter matched.
    Allow,
    /// Cancel the request; `rule` indexes [`Engine::rules`].
    Block { rule: usize },
    /// A block filter matched but the exception filter `rule` (`@@`) overrides block filter `overridden`.
    Exception { rule: usize, overridden: usize },
}

/// What loading a list found. Line numbers are 1-based.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParseReport {
    /// Filters added to the engine.
    pub accepted: usize,
    pub comments: usize,
    /// Cosmetic, scriptlet and HTML filters: not an error, just not this crate's job.
    pub cosmetic_skipped: usize,
    /// Filters left out because they use syntax this matcher does not implement (the reason names the option, or `regex`).
    pub unsupported: Vec<(usize, String)>,
    /// Lines that are malformed.
    pub invalid: Vec<(usize, String)>,
}
