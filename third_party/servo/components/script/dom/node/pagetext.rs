/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The text of a document as blocks of runs, for the embedder's find bar (LucinaNet ).
//!
//! The walk reads the engine's own DOM structs (`Node`, `Element`, `CharacterData`) directly, on
//! the script thread, in one synchronous loop. It never evaluates script, never goes through a
//! JS wrapper or a binding getter the page could redefine, creates no wrapper that did not exist
//! (no `Attr` object, no `NodeList`), fires no event, mutates nothing, changes no selection or
//! focus, and forces no layout. The page cannot observe it.
//!
//! What it covers, and what it does not (the stated limits of this change):
//!
//! - Only the text nodes of the TOP document. Child frames (`<iframe>`, `<frame>`), shadow trees
//!   and templates are not walked. Cross-origin frames only later, by the same in-engine walk
//!   per pipeline, never through script.
//! - Skipped: the content of `script`, `style`, `noscript`, `template`, `head`, `title`,
//!   `datalist`, `iframe`, `frame`, `noframes`, `noembed`, `canvas`, `audio`, `video`; elements
//!   with the `hidden` attribute (except `hidden="until-found"`, which find-in-page may reach);
//!   and elements whose own inline `style` attribute says `display: none`. The engine's resolved
//!   style is NOT used: it is not available without forcing layout, and the walk must not. So a
//!   `display: none` that comes from a stylesheet or a class is not seen, and its text is
//!   included. `visibility: hidden` text is included.
//! - Form controls: `<input>` and `<textarea>` contribute nothing (values, placeholders, default
//!   text and children are never read: a password must not be reachable from here). The text of
//!   `<option>`, `<optgroup>`, `<button>` and `<label>` is page text and is included.
//! - One block per block-level element: consecutive inline text of the same block-level
//!   ancestor are the runs of one block. A forced break (`<br>`, `<hr>`) ends a block.
//!   Custom elements and unknown elements are inline, as their default `display` is.
//!
//! The result is an owned value; nothing here keeps a copy, and nothing here logs a character of
//! page text.

use embedder_traits::{PageText, TextBlock, TextRun};
use html5ever::local_name;

use crate::dom::bindings::codegen::Bindings::NodeBinding::NodeMethods;
use crate::dom::bindings::inheritance::{Castable, CharacterDataTypeId, NodeTypeId};
use crate::dom::bindings::root::DomRoot;
use crate::dom::characterdata::CharacterData;
use crate::dom::document::Document;
use crate::dom::element::Element;
use crate::dom::node::Node;

/// What a node is, as far as the walk cares.
pub(crate) enum NodeKind {
    /// A text node, with its data.
    Text(String),
    Element(ElementInfo),
    /// A comment, doctype, processing instruction, document, fragment: nothing to read.
    Other,
}

/// The facts about an element the walk decides on.
pub(crate) struct ElementInfo {
    /// The local name, ASCII-lowercased.
    pub(crate) name: String,
    /// In the HTML namespace.
    pub(crate) html: bool,
    /// The `hidden` attribute's value, if the attribute is present.
    pub(crate) hidden: Option<String>,
    /// The raw `style` attribute, if present.
    pub(crate) style: Option<String>,
}

/// A tree the walk can run over. The DOM implements it ([`DomTextTree`]); the tests implement it
/// over an arena.
pub(crate) trait TextTree {
    type Node: Clone;
    /// The node whose children are walked (the document).
    fn root(&self) -> Option<Self::Node>;
    fn first_child(&self, node: &Self::Node) -> Option<Self::Node>;
    fn next_sibling(&self, node: &Self::Node) -> Option<Self::Node>;
    fn kind(&self, node: &Self::Node) -> NodeKind;
    /// The id reported as `TextRun::node`.
    fn id(&self, node: &Self::Node) -> u64;
}

/// How the walk treats an element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ElementClass {
    /// Neither it nor its descendants contribute text.
    Skip,
    /// A block-level box: its text starts and ends a block.
    Block,
    /// A forced break: ends the current block; has no content.
    Break,
    /// Inline: its text continues the current block.
    Inline,
}

pub(crate) fn classify(element: &ElementInfo) -> ElementClass {
    let name = element.name.as_str();
    // Content that is never rendered as text, whatever the namespace.
    if matches!(name, "script" | "style") {
        return ElementClass::Skip;
    }
    if !element.html {
        // SVG and MathML: the tooltip and metadata children are not page text; the rest is inline.
        return match name {
            "title" | "desc" | "metadata" => ElementClass::Skip,
            _ if element.style.as_deref().is_some_and(style_hides) => ElementClass::Skip,
            _ => ElementClass::Inline,
        };
    }
    match name {
        "noscript" | "template" | "head" | "title" | "datalist" | "iframe" | "frame" |
        "noframes" | "noembed" | "canvas" | "audio" | "video" | "input" | "textarea" => {
            return ElementClass::Skip;
        },
        _ => {},
    }
    if let Some(hidden) = &element.hidden {
        // `hidden="until-found"` is reachable by find-in-page.
        if !hidden.eq_ignore_ascii_case("until-found") {
            return ElementClass::Skip;
        }
    }
    if element.style.as_deref().is_some_and(style_hides) {
        return ElementClass::Skip;
    }
    if name == "br" || name == "hr" {
        ElementClass::Break
    } else if is_block_name(name) {
        ElementClass::Block
    } else {
        ElementClass::Inline
    }
}

/// HTML elements whose default `display` is block-level (or table-ish, list-item).
fn is_block_name(name: &str) -> bool {
    matches!(
        name,
        "address" | "article" | "aside" | "blockquote" | "body" | "caption" | "center" |
        "colgroup" | "dd" | "details" | "dialog" | "dir" | "div" | "dl" | "dt" | "fieldset" |
        "figcaption" | "figure" | "footer" | "form" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" |
        "header" | "hgroup" | "html" | "legend" | "li" | "listing" | "main" | "menu" | "nav" |
        "ol" | "optgroup" | "option" | "p" | "plaintext" | "pre" | "search" | "section" |
        "select" | "summary" | "table" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr" |
        "ul" | "xmp"
    )
}

/// Whether an inline `style` attribute value makes the element `display: none` (the last
/// `display` declaration wins).
pub(crate) fn style_hides(style: &str) -> bool {
    let mut hides = false;
    for declaration in style.split(';') {
        let Some((property, value)) = declaration.split_once(':') else {
            continue;
        };
        if !property.trim().eq_ignore_ascii_case("display") {
            continue;
        }
        let value = value.trim();
        let value = match value.find('!') {
            Some(bang) => value[..bang].trim_end(),
            None => value,
        };
        hides = value.eq_ignore_ascii_case("none");
    }
    hides
}

/// `text` cut to at most `max` bytes, at a char boundary.
pub(crate) fn truncate_at_boundary(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn is_blank(text: &str) -> bool {
    text.chars().all(char::is_whitespace)
}

/// Collects runs into blocks under a byte budget.
struct Builder {
    blocks: Vec<TextBlock>,
    runs: Vec<TextRun>,
    remaining: usize,
    partial: bool,
}

impl Builder {
    fn new(limit: usize) -> Self {
        Builder {
            blocks: Vec::new(),
            runs: Vec::new(),
            remaining: limit,
            partial: false,
        }
    }

    /// Adds one text node. Returns false when the budget is spent and the walk must stop.
    fn text(&mut self, node: u64, text: String) -> bool {
        if text.is_empty() {
            return true;
        }
        if is_blank(&text) {
            // Whitespace opens no block; it is kept between runs only while it fits.
            if self.runs.is_empty() || text.len() > self.remaining {
                return true;
            }
            self.remaining -= text.len();
            self.runs.push(TextRun { node, text });
            return true;
        }
        if text.len() <= self.remaining {
            self.remaining -= text.len();
            self.runs.push(TextRun { node, text });
            return true;
        }
        // It does not fit: keep the part that does, at a char boundary, and stop.
        self.partial = true;
        let kept = truncate_at_boundary(&text, self.remaining);
        if !kept.is_empty() {
            self.remaining -= kept.len();
            self.runs.push(TextRun {
                node,
                text: kept.to_owned(),
            });
        }
        false
    }

    /// Ends the current block: trailing whitespace-only runs are dropped, an empty block is not
    /// emitted.
    fn end_block(&mut self) {
        while self.runs.last().is_some_and(|run| is_blank(&run.text)) {
            if let Some(run) = self.runs.pop() {
                self.remaining += run.text.len();
            }
        }
        if !self.runs.is_empty() {
            self.blocks.push(TextBlock {
                runs: std::mem::take(&mut self.runs),
            });
        }
    }

    fn finish(mut self) -> PageText {
        self.end_block();
        PageText {
            blocks: self.blocks,
            partial: self.partial,
        }
    }
}

/// Walks `tree` and returns its text blocks, at most `limit` bytes of text in all.
pub(crate) fn walk_page_text<T: TextTree>(tree: &T, limit: usize) -> PageText {
    let mut builder = Builder::new(limit);
    let Some(root) = tree.root() else {
        return builder.finish();
    };
    // The ancestors being walked below the root, with whether each is a block (to end the block
    // on the way out).
    let mut ancestors: Vec<(T::Node, bool)> = Vec::new();
    let mut current = tree.first_child(&root);
    'walk: while let Some(node) = current.take() {
        let mut descend = false;
        let mut is_block = false;
        match tree.kind(&node) {
            NodeKind::Text(text) => {
                if !builder.text(tree.id(&node), text) {
                    break 'walk;
                }
            },
            NodeKind::Element(element) => match classify(&element) {
                // A skipped inline element (`foo<span hidden>x</span>bar`) does not split its
                // block; a skipped block-level one does.
                ElementClass::Skip => {
                    if element.html && is_block_name(&element.name) {
                        builder.end_block();
                    }
                },
                ElementClass::Break => builder.end_block(),
                ElementClass::Block => {
                    builder.end_block();
                    descend = true;
                    is_block = true;
                },
                ElementClass::Inline => descend = true,
            },
            NodeKind::Other => {},
        }
        if descend {
            if let Some(child) = tree.first_child(&node) {
                ancestors.push((node, is_block));
                current = Some(child);
                continue;
            }
            if is_block {
                builder.end_block();
            }
        }
        // The next sibling, climbing out of finished ancestors.
        let mut at = node;
        loop {
            if let Some(sibling) = tree.next_sibling(&at) {
                current = Some(sibling);
                break;
            }
            match ancestors.pop() {
                Some((parent, was_block)) => {
                    if was_block {
                        builder.end_block();
                    }
                    at = parent;
                },
                None => break,
            }
        }
    }
    builder.finish()
}

/// The DOM as a [`TextTree`]. It holds the document only; it reads nodes through their Rust
/// structs.
struct DomTextTree {
    document: DomRoot<Document>,
}

impl TextTree for DomTextTree {
    type Node = DomRoot<Node>;

    fn root(&self) -> Option<DomRoot<Node>> {
        Some(DomRoot::from_ref(self.document.upcast::<Node>()))
    }

    fn first_child(&self, node: &DomRoot<Node>) -> Option<DomRoot<Node>> {
        node.GetFirstChild()
    }

    fn next_sibling(&self, node: &DomRoot<Node>) -> Option<DomRoot<Node>> {
        node.GetNextSibling()
    }

    fn kind(&self, node: &DomRoot<Node>) -> NodeKind {
        match node.type_id() {
            NodeTypeId::CharacterData(CharacterDataTypeId::Text(_)) => {
                match node.downcast::<CharacterData>() {
                    Some(data) => NodeKind::Text(data.data().clone()),
                    None => NodeKind::Other,
                }
            },
            NodeTypeId::Element(_) => match node.downcast::<Element>() {
                Some(element) => NodeKind::Element(ElementInfo {
                    name: {
                        let name: &str = element.local_name();
                        name.to_ascii_lowercase()
                    },
                    html: element.is_html_element(),
                    hidden: element.get_attribute_string_value(&local_name!("hidden")),
                    style: element.get_attribute_string_value(&local_name!("style")),
                }),
                None => NodeKind::Other,
            },
            _ => NodeKind::Other,
        }
    }

    fn id(&self, node: &DomRoot<Node>) -> u64 {
        &**node as *const Node as usize as u64
    }
}

/// The text of `document` as blocks, at most `limit` bytes of text in all. See the module docs
/// for what is covered.
pub(crate) fn collect_page_text(document: &Document, limit: usize) -> PageText {
    let tree = DomTextTree {
        document: DomRoot::from_ref(document),
    };
    walk_page_text(&tree, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree description for the tests.
    enum N {
        T(&'static str),
        E(&'static str, Vec<N>),
        /// An element with a `hidden` attribute value and a `style` attribute.
        A(&'static str, Option<&'static str>, Option<&'static str>, Vec<N>),
    }

    struct Fake {
        kinds: Vec<NodeKind>,
        first: Vec<Option<usize>>,
        next: Vec<Option<usize>>,
    }

    impl Fake {
        fn new(top: Vec<N>) -> Fake {
            let mut fake = Fake {
                kinds: vec![NodeKind::Other],
                first: vec![None],
                next: vec![None],
            };
            let children = fake.add_all(top);
            fake.first[0] = children;
            fake
        }

        fn add_all(&mut self, nodes: Vec<N>) -> Option<usize> {
            let mut first = None;
            let mut prev: Option<usize> = None;
            for node in nodes {
                let id = self.add(node);
                match prev {
                    Some(p) => self.next[p] = Some(id),
                    None => first = Some(id),
                }
                prev = Some(id);
            }
            first
        }

        fn add(&mut self, node: N) -> usize {
            let id = self.kinds.len();
            let (kind, children) = match node {
                N::T(text) => (NodeKind::Text(text.to_owned()), vec![]),
                N::E(name, children) => (
                    NodeKind::Element(ElementInfo {
                        name: name.to_owned(),
                        html: true,
                        hidden: None,
                        style: None,
                    }),
                    children,
                ),
                N::A(name, hidden, style, children) => (
                    NodeKind::Element(ElementInfo {
                        name: name.to_owned(),
                        html: true,
                        hidden: hidden.map(str::to_owned),
                        style: style.map(str::to_owned),
                    }),
                    children,
                ),
            };
            self.kinds.push(kind);
            self.first.push(None);
            self.next.push(None);
            let first = self.add_all(children);
            self.first[id] = first;
            id
        }
    }

    impl TextTree for Fake {
        type Node = usize;
        fn root(&self) -> Option<usize> {
            Some(0)
        }
        fn first_child(&self, node: &usize) -> Option<usize> {
            self.first[*node]
        }
        fn next_sibling(&self, node: &usize) -> Option<usize> {
            self.next[*node]
        }
        fn kind(&self, node: &usize) -> NodeKind {
            match &self.kinds[*node] {
                NodeKind::Text(t) => NodeKind::Text(t.clone()),
                NodeKind::Element(e) => NodeKind::Element(ElementInfo {
                    name: e.name.clone(),
                    html: e.html,
                    hidden: e.hidden.clone(),
                    style: e.style.clone(),
                }),
                NodeKind::Other => NodeKind::Other,
            }
        }
        fn id(&self, node: &usize) -> u64 {
            1000 + *node as u64
        }
    }

    fn walk(top: Vec<N>, limit: usize) -> PageText {
        walk_page_text(&Fake::new(top), limit)
    }

    fn texts(page: &PageText) -> Vec<Vec<&str>> {
        page.blocks
            .iter()
            .map(|b| b.runs.iter().map(|r| r.text.as_str()).collect())
            .collect()
    }

    const BIG: usize = 1 << 20;

    #[test]
    fn inline_text_of_one_block_is_one_block_of_runs() {
        let page = walk(
            vec![N::E(
                "p",
                vec![N::T("foo"), N::E("b", vec![N::T("bar")]), N::T("baz")],
            )],
            BIG,
        );
        assert_eq!(texts(&page), vec![vec!["foo", "bar", "baz"]]);
        assert!(!page.partial);
    }

    #[test]
    fn each_block_element_is_its_own_block() {
        let page = walk(
            vec![
                N::E("p", vec![N::T("one")]),
                N::E("div", vec![N::T("a"), N::E("p", vec![N::T("b")]), N::T("c")]),
                N::E("li", vec![N::T("two")]),
            ],
            BIG,
        );
        assert_eq!(
            texts(&page),
            vec![vec!["one"], vec!["a"], vec!["b"], vec!["c"], vec!["two"]]
        );
    }

    #[test]
    fn a_forced_break_ends_a_block() {
        let page = walk(
            vec![N::E("p", vec![N::T("a"), N::E("br", vec![]), N::T("b")])],
            BIG,
        );
        assert_eq!(texts(&page), vec![vec!["a"], vec!["b"]]);
    }

    #[test]
    fn run_ids_are_the_trees_ids_and_distinct() {
        let page = walk(
            vec![N::E("p", vec![N::T("a"), N::E("b", vec![N::T("b")])])],
            BIG,
        );
        let ids: Vec<u64> = page.blocks[0].runs.iter().map(|r| r.node).collect();
        assert_eq!(ids, vec![1002, 1004]);
    }

    #[test]
    fn whitespace_only_text_between_blocks_makes_no_block() {
        let page = walk(
            vec![
                N::E("p", vec![N::T("a")]),
                N::T("\n  "),
                N::E("p", vec![N::T("b"), N::T(" ")]),
                N::E("p", vec![N::T("   ")]),
            ],
            BIG,
        );
        assert_eq!(texts(&page), vec![vec!["a"], vec!["b"]]);
    }

    #[test]
    fn whitespace_between_inline_runs_is_kept() {
        let page = walk(
            vec![N::E(
                "p",
                vec![N::T("a"), N::T(" "), N::E("b", vec![N::T("b")])],
            )],
            BIG,
        );
        assert_eq!(texts(&page), vec![vec!["a", " ", "b"]]);
    }

    #[test]
    fn skipped_elements_contribute_nothing() {
        for name in [
            "script", "style", "noscript", "template", "head", "title", "datalist", "iframe",
            "frame", "noframes", "noembed", "canvas", "audio", "video",
        ] {
            let page = walk(
                vec![
                    N::E("p", vec![N::T("seen")]),
                    N::E(name, vec![N::T("SECRET"), N::E("p", vec![N::T("SECRET2")])]),
                    N::E("p", vec![N::T("after")]),
                ],
                BIG,
            );
            assert_eq!(texts(&page), vec![vec!["seen"], vec!["after"]], "{name}");
        }
    }

    #[test]
    fn the_head_is_skipped_but_not_the_body() {
        let page = walk(
            vec![N::E(
                "html",
                vec![
                    N::E("head", vec![N::E("title", vec![N::T("T")])]),
                    N::E("body", vec![N::E("p", vec![N::T("B")])]),
                ],
            )],
            BIG,
        );
        assert_eq!(texts(&page), vec![vec!["B"]]);
    }

    #[test]
    fn hidden_attribute_and_inline_display_none_skip_the_subtree() {
        let page = walk(
            vec![
                N::A("div", Some(""), None, vec![N::T("h1")]),
                N::A("div", Some("hidden"), None, vec![N::T("h2")]),
                N::A("p", None, Some("color: red; display:none"), vec![N::T("h3")]),
                N::A("span", None, Some("DISPLAY : NONE !important"), vec![N::T("h4")]),
                N::A("p", None, Some("display:none; display:block"), vec![N::T("shown1")]),
                N::A("p", None, Some("visibility:hidden"), vec![N::T("shown2")]),
                N::A("p", Some("until-found"), None, vec![N::T("shown3")]),
                N::A("p", Some("UNTIL-FOUND"), None, vec![N::T("shown4")]),
            ],
            BIG,
        );
        assert_eq!(
            texts(&page),
            vec![vec!["shown1"], vec!["shown2"], vec!["shown3"], vec!["shown4"]]
        );
    }

    #[test]
    fn a_skipped_inline_element_does_not_split_its_block_but_a_skipped_block_does() {
        let page = walk(
            vec![
                N::E(
                    "p",
                    vec![N::T("foo"), N::A("span", Some(""), None, vec![N::T("x")]), N::T("bar")],
                ),
                N::E(
                    "div",
                    vec![N::T("a"), N::A("div", Some(""), None, vec![N::T("x")]), N::T("b")],
                ),
            ],
            BIG,
        );
        assert_eq!(texts(&page), vec![vec!["foo", "bar"], vec!["a"], vec!["b"]]);
    }

    #[test]
    fn style_hides_cases() {
        assert!(style_hides("display:none"));
        assert!(style_hides("  display : none ;"));
        assert!(style_hides("color:red;display:none"));
        assert!(style_hides("display:block;display:none"));
        assert!(style_hides("DISPLAY:NONE !important"));
        assert!(!style_hides("display:block"));
        assert!(!style_hides("display:none;display:inline"));
        assert!(!style_hides("visibility:hidden"));
        assert!(!style_hides("background:url(x);color:red"));
        assert!(!style_hides("not-display:none"));
        assert!(!style_hides(""));
    }

    #[test]
    fn form_controls_never_contribute_values_or_children() {
        let page = walk(
            vec![N::E(
                "form",
                vec![
                    N::E("label", vec![N::T("Name")]),
                    N::E("input", vec![N::T("INPUTCHILD")]),
                    N::E("textarea", vec![N::T("TEXTAREA SECRET")]),
                    N::E("button", vec![N::T("Go")]),
                    N::E(
                        "select",
                        vec![
                            N::E("option", vec![N::T("One")]),
                            N::E("optgroup", vec![N::E("option", vec![N::T("Two")])]),
                        ],
                    ),
                ],
            )],
            BIG,
        );
        let all: Vec<&str> = texts(&page).into_iter().flatten().collect();
        assert!(all.contains(&"Name"));
        assert!(all.contains(&"Go"));
        assert!(all.contains(&"One"));
        assert!(all.contains(&"Two"));
        assert!(!all.iter().any(|t| t.contains("SECRET") || t.contains("INPUTCHILD")));
    }

    #[test]
    fn non_html_elements_are_inline_and_svg_script_style_are_skipped() {
        let mut fake = Fake::new(vec![N::E(
            "p",
            vec![
                N::T("a"),
                N::E("svg", vec![N::E("text", vec![N::T("b")])]),
                N::E("style", vec![N::T("BAD")]),
            ],
        )]);
        for kind in fake.kinds.iter_mut() {
            if let NodeKind::Element(e) = kind {
                if e.name == "svg" || e.name == "text" || e.name == "style" {
                    e.html = false;
                }
            }
        }
        let page = walk_page_text(&fake, BIG);
        assert_eq!(texts(&page), vec![vec!["a", "b"]]);
    }

    #[test]
    fn the_cap_cuts_at_a_char_boundary_and_says_partial() {
        // "é" is two bytes: a limit of 3 holds "aé" (3 bytes), a limit of 2 holds "a" only.
        let tree = || vec![N::E("p", vec![N::T("aébc")])];
        let page = walk(tree(), 3);
        assert_eq!(texts(&page), vec![vec!["aé"]]);
        assert!(page.partial);
        let page = walk(tree(), 2);
        assert_eq!(texts(&page), vec![vec!["a"]]);
        assert!(page.partial);
        let page = walk(tree(), 1);
        assert_eq!(texts(&page), vec![vec!["a"]]);
        assert!(page.partial);
    }

    #[test]
    fn a_limit_of_zero_gives_nothing_and_partial() {
        let page = walk(vec![N::E("p", vec![N::T("x")])], 0);
        assert!(page.blocks.is_empty());
        assert!(page.partial);
    }

    #[test]
    fn the_total_never_exceeds_the_limit_and_stops_at_the_first_block_that_does_not_fit() {
        let page = walk(
            vec![
                N::E("p", vec![N::T("aaaa")]),
                N::E("p", vec![N::T("bbbb")]),
                N::E("p", vec![N::T("cccc")]),
            ],
            6,
        );
        assert_eq!(texts(&page), vec![vec!["aaaa"], vec!["bb"]]);
        assert!(page.partial);
        let total: usize = page.blocks.iter().flat_map(|b| &b.runs).map(|r| r.text.len()).sum();
        assert!(total <= 6);
    }

    #[test]
    fn an_exact_fit_is_not_partial_but_one_more_byte_is() {
        let tree = || vec![N::E("p", vec![N::T("abc")]), N::E("p", vec![N::T("de")])];
        let page = walk(tree(), 5);
        assert_eq!(texts(&page), vec![vec!["abc"], vec!["de"]]);
        assert!(!page.partial);
        let page = walk(tree(), 4);
        assert_eq!(texts(&page), vec![vec!["abc"], vec!["d"]]);
        assert!(page.partial);
    }

    #[test]
    fn text_after_the_cap_that_is_only_whitespace_is_not_partial() {
        let page = walk(
            vec![N::E("p", vec![N::T("abc")]), N::E("p", vec![N::T("   ")])],
            3,
        );
        assert_eq!(texts(&page), vec![vec!["abc"]]);
        assert!(!page.partial);
    }

    #[test]
    fn truncate_at_boundary_cases() {
        assert_eq!(truncate_at_boundary("héllo", 0), "");
        assert_eq!(truncate_at_boundary("héllo", 1), "h");
        assert_eq!(truncate_at_boundary("héllo", 2), "h");
        assert_eq!(truncate_at_boundary("héllo", 3), "hé");
        assert_eq!(truncate_at_boundary("héllo", 99), "héllo");
        assert_eq!(truncate_at_boundary("😀x", 3), "");
        assert_eq!(truncate_at_boundary("😀x", 4), "😀");
    }

    #[test]
    fn debug_of_page_text_shows_counts_never_text() {
        let page = walk(vec![N::E("p", vec![N::T("MARKER-7f3a")])], BIG);
        let shown = format!("{page:?} {:?} {:?}", page.blocks, page.blocks[0].runs[0]);
        assert!(!shown.contains("MARKER"), "{shown}");
        assert!(shown.contains("blocks: 1"));
    }

    /// Rule 1 and rule 4 by inspection of this file's own non-test source: the walk goes through no
    /// JS context, no wrapper-creating accessor and no binding getter, and logs nothing. The
    /// forbidden names are written with `concat!` so this test does not match itself.
    #[test]
    fn the_walk_source_reaches_no_script_no_wrapper_and_no_log() {
        let source = include_str!("pagetext.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        // The module docs and comments may name these; only code lines are checked.
        let code: String = production
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with("//")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let forbidden = [
            concat!("JS", "Context"),
            concat!("evaluate", "_js"),
            concat!("ensure", "_dom"),
            concat!("dom", "_attrs"),
            concat!("get_attribute", "_by_name"),
            concat!("get_attribute", "_with_namespace"),
            concat!("Get", "TextContent"),
            concat!("Inner", "Text"),
            concat!("Get", "Attribute"),
            concat!("reflect", "_dom_object"),
            concat!("Get", "Selection"),
            concat!("fire", "_event"),
            concat!("dispatch", "_event"),
            concat!("log", "::"),
            concat!("debug", "!("),
            concat!("trace", "!("),
            concat!("warn", "!("),
            concat!("info", "!("),
            concat!("error", "!("),
            concat!("println", "!("),
            concat!("eprintln", "!("),
            concat!("dbg", "!("),
            concat!("static ", "mut"),
            concat!("thread", "_local"),
            concat!("lazy", "_static"),
            concat!("Once", "Lock"),
        ];
        for name in forbidden {
            assert!(!code.contains(name), "the walk source mentions {name}");
        }
    }
}
