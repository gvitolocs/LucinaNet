/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! `page`: the two renderings of what [`crate::read_sessions`] returns — HTML for `about:errors` and
//! plain text for a terminal, a file, or the log's own dump.
//!
//! Both are pure functions of the data handed in. Neither reads a file, a clock or the environment,
//! so the wiring can build the page for a session that has not finished loading, a test can compare
//! output without a date in it, and the same numbers can be shown in a dialog instead.
//!
//! The HTML is self-contained: its own styles, no script of any kind, nothing loaded from anywhere.
//! Every byte that came out of the log goes through [`escape`] on its way in, and that includes the
//! text a page wrote itself — a detail that says `<script>` says it as the eight characters it is.
//! The page is a browser showing a person their own machine, and it is the one place in LucinaNet
//! where the input is our own file: escaping it is not enough to be safe, it is enough to be honest.

use crate::log::Target;
use crate::model::Kind;
use crate::read::{group_by_host, DetailCount, SessionErrors};

/// One `Clear` control on the page: the host group it belongs to and where the click goes. The
/// wiring mints the link — with its token, its confirmation step and all the rest of what makes a
/// state-changing click safe — and the library only draws it beside the right row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClearLink {
    pub page_host: String,
    pub href: String,
}

/// Where the lines on the page came from. The footer has to say it, and the three answers are
/// different sentences: a profile's file names itself, a run with no profile says what dies with the
/// process, and a rendering that was never told — [`render_html`], a test, a dump — claims neither.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lines<'a> {
    #[default]
    Unnamed,
    /// `<dir>/errors.jsonl` and its rolled-away neighbours.
    File(&'a str),
    /// No profile directory: this session's lines, in memory.
    Memory,
}

/// What the page may do besides show the lines. Every field is `None`/empty for a page that is a
/// picture of the file and nothing else — the text rendering, a test, a run that cannot write.
#[derive(Clone, Debug, Default)]
pub struct Page<'a> {
    /// The per-host `Clear` links, matched to the table's rows by `page_host`.
    pub clear_hosts: &'a [ClearLink],
    /// The `Clear all` link. `None` is no controls on the page at all.
    pub clear_all: Option<&'a str>,
    /// What to say above the tables: what the last control did, or why it did nothing.
    pub notice: Option<&'a str>,
    /// Where the lines are, so the footer can be true about it.
    pub lines: Lines<'a>,
}

impl<'a> Page<'a> {
    /// A page with nothing to click and nothing new to say: what [`render_html`] draws.
    pub fn plain() -> Page<'static> {
        Page::default()
    }

    /// Whether the table has a column of controls.
    fn controls(&self) -> bool {
        self.clear_all.is_some() || !self.clear_hosts.is_empty()
    }
}

/// `about:errors`: one table per session, newest session first, with no controls.
pub fn render_html(sessions: &[SessionErrors]) -> String {
    render_html_with(sessions, &Page::plain())
}

/// `about:errors` as the browser serves it: the same tables, plus the controls that empty them and
/// the line that says what happened when the last one was used.
pub fn render_html_with(sessions: &[SessionErrors], page: &Page) -> String {
    let mut out = String::with_capacity(4096);
    head(&mut out, "LucinaNet — Session errors");
    out.push_str("<h1>Session errors</h1>\n");
    out.push_str(PRIVACY_NOTE_HTML);
    if let Some(notice) = page.notice {
        out.push_str(&format!("<p class=\"notice\">{}</p>\n", escape(notice)));
    }
    if let Some(href) = page.clear_all {
        out.push_str(&format!("<p class=\"actions\"><a href=\"{}\">Clear all</a></p>\n", escape(href)));
    }
    if sessions.is_empty() {
        out.push_str("<p class=\"meta\">Nothing has gone wrong yet, or LucinaNet has not been pointed at a profile directory.</p>\n");
    }
    for session in sessions {
        render_session_html(&mut out, session, page);
    }
    out.push_str(&foot_html(page));
    out.push_str("</body>\n</html>\n");
    out
}

/// The whole style of both pages, in one place: the ask page and the list page are the same document
/// family and must not read as two. Nothing here loads from anywhere, and neither does the page.
const STYLE: &str = "body{margin:2rem auto;max-width:56rem;padding:0 1rem;font:14px/1.5 system-ui,sans-serif;color:#191919;background:#fff}\n\
h1{font-size:1.4rem;margin:0 0 .25rem}h2{font-size:1.05rem;margin:1.75rem 0 .25rem}\n\
p{margin:.3rem 0}.note,.meta{color:#575757;font-size:.95rem}\n\
table{border-collapse:collapse;width:100%;margin:.5rem 0 1.25rem}\n\
th,td{border-bottom:1px solid #d8d8d8;padding:.4rem .5rem;text-align:left;vertical-align:top}\n\
th{font-weight:600;font-size:.9rem;text-transform:uppercase;letter-spacing:.02em;color:#575757}\n\
td.n,th.n{text-align:right;font-variant-numeric:tabular-nums;white-space:nowrap}\n\
ul{margin:0;padding-left:1.1rem}li{margin:.15rem 0}\n\
.notice{margin:.6rem 0;padding:.5rem .7rem;background:#f0f4f8;border-left:3px solid #3b6ea5}\n\
.actions{margin:.7rem 0}\n";

fn head(out: &mut String, title: &str) {
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str(&format!("<title>{}</title>\n<style>\n{STYLE}</style>\n</head>\n<body>\n", escape(title)));
}

/// The same sessions as text: for a terminal, a saved file, or `lucinanet --errors`.
pub fn render_text(sessions: &[SessionErrors]) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str("LucinaNet - session errors\n");
    out.push_str(
        "Hosts, file names, line numbers and counts only: no page text, no query, no cookies, \
         nothing you typed.\n\n",
    );
    if sessions.is_empty() {
        out.push_str("Nothing has gone wrong yet, or LucinaNet has not been pointed at a profile directory.\n");
    }
    for session in sessions {
        render_session_text(&mut out, session);
    }
    out
}

const PRIVACY_NOTE_HTML: &str = "<p class=\"note\">What went wrong while browsing, newest session first. This log keeps hosts, file \
    names, line numbers and counts and nothing else: not page text, not an address with its query, \
    not cookies, not anything typed into a form.</p>\n";

/// The last paragraph: where the lines are, how to stop them, and how to take them away. It used to
/// point at a settings page that did not exist; this is the true version of what that sentence was
/// reaching for, and it changes with the run it describes, because a run with no profile has no file
/// to delete and a run with no controls has nothing to click.
fn foot_html(page: &Page) -> String {
    let mut body = String::with_capacity(512);
    match page.lines {
        Lines::File(dir) => body.push_str(&format!(
            "These lines are <code>{}/errors.jsonl</code>, one JSON object per event, with the older \
             <code>errors.1.jsonl</code> and beyond kept beside them while they last. ",
            escape(dir)
        )),
        Lines::Memory => body.push_str("This run has no profile directory, so these lines are in memory and die with it. "),
        Lines::Unnamed => body.push_str(
            "These lines are <code>errors.jsonl</code> in the profile directory, one JSON object per \
             event, with the older <code>errors.1.jsonl</code> and beyond kept beside them while they \
             last. ",
        ),
    }
    if page.controls() {
        body.push_str(
            "<code>Clear</code> beside a host takes that host's lines out of every file the log \
             keeps, and <code>Clear all</code> takes the whole log; both ask first, and neither \
             touches the site, the history or the cookies.",
        );
    } else {
        body.push_str("Deleting <code>errors.jsonl</code> and its <code>errors.N.jsonl</code> neighbours takes this page back to empty.");
    }
    body.push_str(
        " To stop LucinaNet writing it, set <code>error_log</code> to <code>false</code> in the \
         <code>privacy</code> part of the profile's <code>prefs.json</code>, or start it with \
         <code>LUCINANET_ERRLOG=0</code> — the environment wins over the file, and either one leaves \
         the lines already there until you clear them.",
    );
    format!("<p class=\"note\">{body}</p>\n")
}

/// `about:errors` asking before it takes anything away. One click away from the list, and one more
/// from the removal: the link here was minted for this render and is good for one use, which is what
/// keeps a page on the network from wiping the log it cannot see.
pub fn render_clear_ask(target: &Target, confirm_href: &str, back_href: &str) -> String {
    let heading = match target {
        Target::All => "Clear the whole error log?".to_string(),
        Target::Host(page_host) => format!("Clear {}?", host_label(page_host)),
    };
    let mut out = String::with_capacity(2048);
    head(&mut out, "LucinaNet — Clear the session error log?");
    out.push_str(&format!("<h1>{}</h1>\n", escape(&heading)));
    out.push_str(&format!("<p>LucinaNet will take <strong>{}</strong> out of the log.</p>\n", target_words(target)));
    out.push_str(
        "<p class=\"note\">It asks nothing of the site and forgets nothing else: not the history, not \
         the cookies, not the zoom. The errors stay errors — the next time one of them happens it is \
         written again from one.</p>\n",
    );
    out.push_str(&format!(
        "<p class=\"actions\"><a href=\"{}\">Yes, clear it</a> &nbsp; <a href=\"{}\">No, keep the log</a></p>\n",
        escape(confirm_href),
        escape(back_href)
    ));
    out.push_str(&foot_html(&Page::plain()));
    out.push_str("</body>\n</html>\n");
    out
}

/// What a [`Target`] takes away, in the words the page uses. The row's own label and this sentence
/// come from the same field, so a host with an odd character in it is escaped once and reads the same
/// in both places.
pub fn target_words(target: &Target) -> String {
    match target {
        Target::All => "every line of it, in every session and in the generations rolled away".to_string(),
        Target::Host(page_host) if page_host.is_empty() => {
            "every line of the pages that have no host — an <code>about:</code>, a <code>data:</code>, a new tab"
                .to_string()
        },
        Target::Host(page_host) => format!("every line of <code>{}</code>", escape(page_host)),
    }
}

fn render_session_html(out: &mut String, session: &SessionErrors, page: &Page) {
    out.push_str("<section>\n");
    out.push_str(&format!("<h2>Session {}</h2>\n", session.session));
    out.push_str(&format!("<p class=\"meta\">{}</p>\n", escape(&meta_line(session))));
    out.push_str("<table>\n");
    out.push_str("<thead><tr><th>Host</th><th>What happened</th><th>Most frequent</th><th class=\"n\">Times</th>");
    if page.controls() {
        out.push_str("<th>Remove</th>");
    }
    out.push_str("</tr></thead>\n<tbody>\n");
    for group in group_by_host(session) {
        out.push_str("<tr><td>");
        out.push_str(&escape(&host_label(&group.page_host)));
        out.push_str("</td><td>");
        out.push_str(&escape(&kinds_line(&group.kinds)));
        out.push_str("</td><td>");
        if group.details.is_empty() {
            out.push('-');
        } else {
            out.push_str("<ul>");
            for detail in &group.details {
                out.push_str("<li>");
                out.push_str(&escape(&format!("{}: {}", detail.kind.label(), detail.detail)));
                let from = where_from(detail);
                if !from.is_empty() {
                    out.push_str(&escape(&format!(" - {from}")));
                }
                out.push_str(&format!(" ({} times)", detail.count));
                out.push_str("</li>");
            }
            out.push_str("</ul>");
        }
        out.push_str(&format!("</td><td class=\"n\">{}</td>", group.total));
        match page.clear_hosts.iter().find(|link| link.page_host == group.page_host) {
            Some(link) => out.push_str(&format!("<td><a href=\"{}\">Clear</a></td></tr>\n", escape(&link.href))),
            None => out.push_str(if page.controls() { "<td></td></tr>\n" } else { "</tr>\n" }),
        }
    }
    out.push_str("</tbody>\n</table>\n");
    if session.truncated() {
        out.push_str("<p class=\"note\">This session reached the limit of what its log keeps. What happened after that is not here.</p>\n");
    }
    out.push_str("</section>\n");
}

fn render_session_text(out: &mut String, session: &SessionErrors) {
    out.push_str(&format!("Session {}\n", session.session));
    out.push_str(&format!("  {}\n", meta_line(session)));
    for group in group_by_host(session) {
        out.push_str(&format!("  {:<30}{:>7} times  {:>5} distinct\n", host_label(&group.page_host), group.total, group.distinct));
        out.push_str(&format!("    {}\n", kinds_line(&group.kinds)));
        for detail in &group.details {
            let from = where_from(detail);
            let from = if from.is_empty() { String::new() } else { format!(" - {from}") };
            out.push_str(&format!("    {:<15}{:>6}  {}{from}\n", detail.kind.label(), detail.count, one_line(&detail.detail)));
        }
    }
    if session.truncated() {
        out.push_str("  This session reached the limit of what its log keeps; what happened after is not here.\n");
    }
    out.push('\n');
}

/// A detail on one line. The log writes one line per event, so a redacted detail has no newlines in
/// it already; this is for a file that was edited by hand, where a table row must not be split.
fn one_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// The line under a session's heading, in both renderings: how many lines, when they are from, and
/// how much of the session could not be read.
fn meta_line(session: &SessionErrors) -> String {
    let span = match (session.first_t_ms(), session.last_t_ms()) {
        (Some(first), Some(last)) if first == last => stamp(first),
        (Some(first), Some(last)) => format!("{} to {}", stamp(first), stamp(last)),
        _ => "no readable lines".to_string(),
    };
    let corrupt = match session.corrupt {
        0 => String::new(),
        1 => ", 1 line unreadable".to_string(),
        n => format!(", {n} lines unreadable"),
    };
    format!("{} lines, {span}{corrupt}", session.events.len())
}

/// A page host, or what to call the pages that have none.
fn host_label(page_host: &str) -> String {
    if page_host.is_empty() { "(no host)" } else { page_host }.to_string()
}

/// "script error 4, failed request 2", the kinds in the order the model declares them.
fn kinds_line(kinds: &[(Kind, u32)]) -> String {
    if kinds.is_empty() {
        return "-".to_string();
    }
    kinds.iter().map(|(kind, count)| format!("{} {count}", kind.label())).collect::<Vec<_>>().join(", ")
}

/// Where a detail came from, as far as the log is allowed to know: a file name, a line, a host.
fn where_from(detail: &DetailCount) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !detail.file.is_empty() {
        parts.push(if detail.line > 0 { format!("{}:{}", detail.file, detail.line) } else { detail.file.clone() });
    }
    if !detail.host.is_empty() {
        parts.push(detail.host.clone());
    }
    parts.join(" ")
}

/// Text from the log, safe to put in a page. Everything that did not come from this crate's own
/// strings goes through here, including a message a page wrote.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Milliseconds since the Unix epoch as a UTC stamp, without a locale, a timezone or a date crate: a
/// diagnostics page that needs a dependency to print a date is a diagnostics page that cannot load.
pub fn stamp(t_ms: u64) -> String {
    let secs = i64::try_from(t_ms / 1000).unwrap_or(i64::MAX);
    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC", time_of_day / 3600, (time_of_day / 60) % 60, time_of_day % 60)
}

/// The proleptic Gregorian civil date of a number of days since 1970-01-01 - Howard Hinnant's
/// `civil_from_days`, the arithmetic every date library uses for exactly this.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 { shifted } else { shifted - 146_096 } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64; // [0, 146096]
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let shifted_month = (5 * day_of_year + 2) / 153; // [0, 11], March first
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32; // [1, 31]
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}
