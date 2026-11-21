/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Netscape bookmark HTML: a tolerant tokenizer (not an HTML parser) for import, Firefox's writer for export.
//! Firefox `toolkit/components/places/BookmarkHTMLUtils.sys.mjs` @33abb90.

use std::collections::{HashMap, HashSet};

use crate::store::{Bookmarks, normalize_keyword, normalize_tags, normalize_url};
use crate::{Error, Id, ImportOptions, ImportReport, Kind, MENU, OTHER, ROOT, TOOLBAR};

/// Folders nested deeper than this inside an import are filled into the deepest allowed folder, so a hostile file
/// cannot build a tree that later recursion (export, own format) would overflow the stack on.
const MAX_DEPTH: usize = 48;
/// `EXPORT_INDENT` in BookmarkHTMLUtils.sys.mjs @33abb90.
const INDENT: &str = "    ";

#[derive(Debug)]
enum Tok<'a> {
    Open { name: String, attrs: Vec<(String, String)> },
    Close(String),
    Text(&'a str),
}

/// Decode `&amp; &lt; &gt; &quot; &apos; &#39; &#x27; &#NN;`. Anything else is left as written.
pub(crate) fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        // An entity is short; look for the `;` close by.
        let end = rest.char_indices().take(12).find(|(_, c)| *c == ';').map(|(i, _)| i);
        let decoded = end.and_then(|e| {
            let body = &rest[1..e];
            let ch = match body {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => {
                    let num = body.strip_prefix('#')?;
                    let code = match num.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok()?,
                        None => num.parse::<u32>().ok()?,
                    };
                    char::from_u32(code)
                }
            }?;
            Some((ch, e + 1))
        });
        match decoded {
            Some((ch, used)) => {
                out.push(ch);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parse attributes starting at `j` (just after the tag name). Returns them and the index after the closing `>`.
fn parse_attrs(html: &str, mut j: usize) -> (Vec<(String, String)>, usize) {
    let b = html.as_bytes();
    let mut attrs = Vec::new();
    loop {
        while j < b.len() && (b[j].is_ascii_whitespace() || b[j] == b'/') {
            j += 1;
        }
        if j >= b.len() {
            return (attrs, b.len());
        }
        if b[j] == b'>' {
            return (attrs, j + 1);
        }
        let start = j;
        while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'=' && b[j] != b'>' {
            j += 1;
        }
        if j == start {
            // A stray `=`: skip it so the loop always advances.
            j += 1;
            continue;
        }
        let name = html[start..j].to_ascii_lowercase();
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        let mut value = String::new();
        if j < b.len() && b[j] == b'=' {
            j += 1;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < b.len() && (b[j] == b'"' || b[j] == b'\'') {
                let q = b[j];
                j += 1;
                let vs = j;
                while j < b.len() && b[j] != q {
                    j += 1;
                }
                value = decode_entities(&html[vs..j]);
                if j < b.len() {
                    j += 1;
                }
            } else {
                let vs = j;
                while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                    j += 1;
                }
                value = decode_entities(&html[vs..j]);
            }
        }
        attrs.push((name, value));
    }
}

fn tokenize(html: &str) -> Vec<Tok<'_>> {
    let b = html.as_bytes();
    // ASCII lower-casing keeps byte offsets and char boundaries identical to `html`.
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut i = 0;
    let mut text_start = 0;
    while i < b.len() {
        if b[i] != b'<' || i + 1 >= b.len() {
            i += 1;
            continue;
        }
        let n = b[i + 1];
        let closing = n == b'/';
        let is_tag = n.is_ascii_alphabetic() || (closing && b.get(i + 2).is_some_and(u8::is_ascii_alphabetic));
        if !(is_tag || n == b'!' || n == b'?') {
            i += 1;
            continue;
        }
        if text_start < i {
            out.push(Tok::Text(&html[text_start..i]));
        }
        if lower[i..].starts_with("<!--") {
            i = lower[i + 4..].find("-->").map_or(b.len(), |p| i + 4 + p + 3);
            text_start = i;
            continue;
        }
        if !is_tag {
            i = lower[i..].find('>').map_or(b.len(), |p| i + p + 1);
            text_start = i;
            continue;
        }
        let name_start = i + 1 + usize::from(closing);
        let mut j = name_start;
        while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' && b[j] != b'/' {
            j += 1;
        }
        let name = lower[name_start..j].to_string();
        let (attrs, end) = parse_attrs(html, j);
        i = end;
        if closing {
            out.push(Tok::Close(name));
        } else {
            let raw = name == "script" || name == "style";
            out.push(Tok::Open { name: name.clone(), attrs });
            if raw {
                // Contents are code, never bookmarks.
                i = lower[i..].find(&format!("</{name}")).map_or(b.len(), |p| i + p);
            }
        }
        text_start = i;
    }
    if text_start < b.len() {
        out.push(Tok::Text(&html[text_start..]));
    }
    out
}

/// `parseInt` as Firefox's `_convertImportedDateToInternalDate` uses it: leading integer, anything after is ignored.
fn parse_seconds(v: Option<&str>) -> Option<i64> {
    let t = v?.trim();
    let digits_from = usize::from(t.starts_with(['+', '-']));
    let end = t[digits_from..]
        .find(|c: char| !c.is_ascii_digit())
        .map_or(t.len(), |p| digits_from + p);
    if end == digits_from {
        return None;
    }
    t[..end].parse::<i64>().ok().map(|s| s.saturating_mul(1_000_000))
}

/// Both missing: `now`. One missing: the other (Firefox sets `dateAdded = lastModified` when only the latter exists).
/// Modified before added is raised to added, which the Bookmarks API would otherwise reject.
pub(crate) fn resolve_dates(added: Option<i64>, modified: Option<i64>, now_us: i64) -> (i64, i64) {
    match (added, modified) {
        (Some(a), Some(m)) => (a, m.max(a)),
        (Some(a), None) => (a, a),
        (None, Some(m)) => (m, m),
        (None, None) => (now_us, now_us),
    }
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

enum CapKind {
    Heading,
    Link,
}

struct Capture {
    kind: CapKind,
    attrs: Vec<(String, String)>,
    text: String,
}

struct Importer<'a> {
    bm: &'a mut Bookmarks,
    opts: ImportOptions,
    now_us: i64,
    report: ImportReport,
    stack: Vec<Id>,
    /// Folder created by the last `<H3>`, waiting for its `<DL>`.
    pending: Option<Id>,
    /// URLs per destination folder, built lazily when `skip_duplicates` is on.
    known: HashMap<Id, HashSet<String>>,
}

impl Importer<'_> {
    fn top(&self) -> Id {
        self.stack.last().copied().unwrap_or(ROOT)
    }

    fn finish(&mut self, cap: Capture) -> Result<(), Error> {
        let dates = resolve_dates(
            parse_seconds(attr(&cap.attrs, "add_date")),
            parse_seconds(attr(&cap.attrs, "last_modified")),
            self.now_us,
        );
        let title = cap.text.trim();
        let parent = self.top();
        match cap.kind {
            CapKind::Heading => {
                let id = self.bm.insert_raw(parent, None, Kind::Folder, title, dates.0, dates.1, Vec::new(), None)?;
                self.report.folders += 1;
                self.pending = Some(id);
            }
            CapKind::Link => {
                self.pending = None;
                // `<A>` without href is not a bookmark (Firefox ignores it too).
                let Some(href) = attr(&cap.attrs, "href") else { return Ok(()) };
                let Ok(url) = normalize_url(href) else {
                    self.report.skipped_invalid += 1;
                    return Ok(());
                };
                if self.opts.skip_duplicates {
                    let bm = &*self.bm;
                    let set = self.known.entry(parent).or_insert_with(|| {
                        bm.children(parent)
                            .unwrap_or(&[])
                            .iter()
                            .filter_map(|c| match bm.get(*c).map(|n| &n.kind) {
                                Some(Kind::Bookmark { url }) => Some(url.clone()),
                                _ => None,
                            })
                            .collect()
                    });
                    if !set.insert(url.clone()) {
                        self.report.skipped_duplicates += 1;
                        return Ok(());
                    }
                }
                let tags = normalize_tags(attr(&cap.attrs, "tags").unwrap_or("").split(','));
                let keyword = normalize_keyword(attr(&cap.attrs, "shortcuturl"));
                self.bm.insert_raw(parent, None, Kind::Bookmark { url }, title, dates.0, dates.1, tags, keyword)?;
                self.report.bookmarks += 1;
            }
        }
        Ok(())
    }
}

fn is_inline(name: &str) -> bool {
    matches!(
        name,
        "b" | "i" | "u" | "s" | "em" | "strong" | "span" | "font" | "br" | "small" | "big" | "code" | "tt" | "sub" | "sup"
    )
}

impl Bookmarks {
    /// Import a Netscape bookmark file into the folder `into` (never ROOT), appending in file order. A folder the file flags
    /// as toolbar or unfiled becomes an ordinary folder with its own title: import never deletes or replaces anything.
    pub fn import_netscape_html(&mut self, html: &str, into: Id, opts: ImportOptions, now_us: i64) -> Result<ImportReport, Error> {
        if into == ROOT {
            return Err(Error::ProtectedNode(ROOT));
        }
        self.children(into)?;
        let mut imp = Importer {
            bm: self,
            opts,
            now_us,
            report: ImportReport::default(),
            stack: vec![into],
            pending: None,
            known: HashMap::new(),
        };
        let mut cap: Option<Capture> = None;
        for tok in tokenize(html) {
            match tok {
                Tok::Text(t) => {
                    if let Some(c) = &mut cap {
                        c.text.push_str(&decode_entities(t));
                    }
                }
                Tok::Open { name, attrs } => {
                    if !is_inline(&name)
                        && let Some(c) = cap.take()
                    {
                        imp.finish(c)?;
                    }
                    match name.as_str() {
                        "h3" => cap = Some(Capture { kind: CapKind::Heading, attrs, text: String::new() }),
                        "a" => cap = Some(Capture { kind: CapKind::Link, attrs, text: String::new() }),
                        "dl" => {
                            let top = imp.top();
                            let target = match imp.pending.take() {
                                Some(f) if imp.stack.len() < MAX_DEPTH => f,
                                _ => top,
                            };
                            imp.stack.push(target);
                        }
                        "hr" => {
                            imp.pending = None;
                            let parent = imp.top();
                            imp.bm.insert_raw(parent, None, Kind::Separator, "", now_us, now_us, Vec::new(), None)?;
                            imp.report.separators += 1;
                        }
                        _ => {}
                    }
                }
                Tok::Close(name) => {
                    if !is_inline(&name)
                        && let Some(c) = cap.take()
                    {
                        imp.finish(c)?;
                    }
                    if name == "dl" {
                        imp.pending = None;
                        if imp.stack.len() > 1 {
                            imp.stack.pop();
                        }
                    }
                }
            }
        }
        if let Some(c) = cap.take() {
            imp.finish(c)?;
        }
        Ok(imp.report)
    }

    /// Firefox's `BookmarkExporter` output: header, `<H1>` with the menu's name, the menu's children, then Toolbar and Other as
    /// flagged folders (only when they are not empty, as Firefox does).
    pub fn export_netscape_html(&self) -> String {
        let mut out = String::new();
        out.push_str("<!DOCTYPE NETSCAPE-Bookmark-file-1>\n");
        out.push_str("<!-- This is an automatically generated file.\n");
        out.push_str("     It will be read and overwritten.\n");
        out.push_str("     DO NOT EDIT! -->\n");
        out.push_str("<META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">\n");
        out.push_str("<meta http-equiv=\"Content-Security-Policy\"\n");
        out.push_str("      content=\"default-src 'self'; script-src 'none'; img-src data: *; object-src 'none'\"></meta>\n");
        out.push_str("<TITLE>Bookmarks</TITLE>\n");
        let menu_title = self.get(MENU).map_or("Bookmarks Menu", |n| n.title.as_str());
        out.push_str(&format!("<H1>{}</H1>\n\n", escape(menu_title)));
        out.push_str("<DL><p>\n");
        for c in self.children(MENU).unwrap_or(&[]) {
            self.export_node(*c, INDENT, None, &mut out);
        }
        for (id, flag) in [(TOOLBAR, "PERSONAL_TOOLBAR_FOLDER"), (OTHER, "UNFILED_BOOKMARKS_FOLDER")] {
            if !self.children(id).unwrap_or(&[]).is_empty() {
                self.export_node(id, INDENT, Some(flag), &mut out);
            }
        }
        out.push_str("</DL>\n");
        out
    }

    fn export_node(&self, id: Id, indent: &str, flag: Option<&str>, out: &mut String) {
        let Some(e) = self.entries.get(&id) else { return };
        let n = &e.node;
        match &n.kind {
            Kind::Folder => {
                out.push_str(&format!("{indent}<DT><H3{}", date_attrs(n.added_us, n.modified_us)));
                if let Some(f) = flag {
                    out.push_str(&format!(" {f}=\"true\""));
                }
                out.push_str(&format!(">{}</H3>\n{indent}<DL><p>\n", escape(&n.title)));
                let inner = format!("{indent}{INDENT}");
                for c in &e.children {
                    self.export_node(*c, &inner, None, out);
                }
                out.push_str(&format!("{indent}</DL><p>\n"));
            }
            // Firefox writes `<HR>` without a line break; we add one. Same parse, readable file.
            Kind::Separator => out.push_str(&format!("{indent}<HR>\n")),
            Kind::Bookmark { url } => {
                out.push_str(&format!(
                    "{indent}<DT><A HREF=\"{}\"{}",
                    escape(url),
                    date_attrs(n.added_us, n.modified_us)
                ));
                if let Some(k) = &n.keyword {
                    out.push_str(&format!(" SHORTCUTURL=\"{}\"", escape(k)));
                }
                if !n.tags.is_empty() {
                    out.push_str(&format!(" TAGS=\"{}\"", escape(&n.tags.join(","))));
                }
                out.push_str(&format!(">{}</A>\n", escape(&n.title)));
            }
        }
    }
}

/// `BrowserUtils.escapeHtmlEntities`: `& < > " '`.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Seconds, floored; a zero date is not written (Firefox's `_writeDateAttributes`).
fn date_attrs(added_us: i64, modified_us: i64) -> String {
    let mut s = String::new();
    if added_us != 0 {
        s.push_str(&format!(" ADD_DATE=\"{}\"", added_us.div_euclid(1_000_000)));
    }
    if modified_us != 0 {
        s.push_str(&format!(" LAST_MODIFIED=\"{}\"", modified_us.div_euclid(1_000_000)));
    }
    s
}
