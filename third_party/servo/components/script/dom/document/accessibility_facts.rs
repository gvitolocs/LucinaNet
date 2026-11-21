/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The [`AccessibilityFacts`] of an element: what the accessibility tree needs from the DOM that
//! layout can't read on its own. Names follow the accname specification's order as Chromium's
//! `AXNodeObject` computes them (`aria-labelledby`, `aria-label`, the host language's label, then
//! the content, which layout adds, then `title` and `placeholder`); roles, states and values follow
//! HTML-AAM.
//!
//! The values of secret fields (passwords, one-time codes, payment cards) are withheld here,
//! through [`accessibility_rules::exposed_value`], so they never reach layout or the embedder.

use accesskit::{Role, Toggled};
use html5ever::local_name;
use js::context::NoGC;
use layout_api::accessibility_rules::{self, Secrecy, Target};
use layout_api::html_roles::{self, HtmlRoleContext};
use layout_api::{AccessibilityDamage, AccessibilityFacts};
use rustc_hash::FxHashMap;
use style::dom::OpaqueNode;

use crate::dom::bindings::codegen::Bindings::DocumentBinding::DocumentMethods;
use crate::dom::bindings::codegen::Bindings::HTMLDetailsElementBinding::HTMLDetailsElementMethods;
use crate::dom::bindings::codegen::Bindings::HTMLInputElementBinding::HTMLInputElementMethods;
use crate::dom::bindings::codegen::Bindings::HTMLLabelElementBinding::HTMLLabelElementMethods;
use crate::dom::bindings::codegen::Bindings::HTMLOptionElementBinding::HTMLOptionElementMethods;
use crate::dom::bindings::codegen::Bindings::HTMLSelectElementBinding::HTMLSelectElementMethods;
use crate::dom::bindings::codegen::Bindings::HTMLTextAreaElementBinding::HTMLTextAreaElementMethods;
use crate::dom::bindings::codegen::Bindings::NodeBinding::NodeMethods;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::root::DomRoot;
use crate::dom::characterdata::CharacterData;
use crate::dom::document::Document;
use crate::dom::element::Element;
use crate::dom::eventtarget::EventTarget;
use crate::dom::html::htmldetailselement::HTMLDetailsElement;
use crate::dom::html::htmlinputelement::HTMLInputElement;
use crate::dom::html::htmllabelelement::HTMLLabelElement;
use crate::dom::html::htmloptionelement::HTMLOptionElement;
use crate::dom::html::htmlselectelement::HTMLSelectElement;
use crate::dom::html::htmltextareaelement::HTMLTextAreaElement;
use crate::dom::iterators::ShadowIncluding;
use crate::dom::node::Node;
use crate::dom::text::Text;

/// The texts of the `<label>`s of each labelled control in a document, in tree order.
pub(crate) type LabelTexts = FxHashMap<OpaqueNode, String>;

/// Collect the text of every `<label>` per labelled control, once per walk.
pub(crate) fn label_texts(document: &Document, no_gc: &NoGC) -> LabelTexts {
    let mut labels = LabelTexts::default();
    for node in document
        .upcast::<Node>()
        .traverse_preorder_unrooted(no_gc, ShadowIncluding::Yes)
    {
        let Some(label) = node.downcast::<HTMLLabelElement>() else {
            continue;
        };
        let Some(control) = label.GetControl(no_gc) else {
            continue;
        };
        let text = text_without_controls(label.upcast());
        if text.is_empty() {
            continue;
        }
        let entry = labels
            .entry(control.upcast::<Node>().to_opaque())
            .or_default();
        if !entry.is_empty() {
            entry.push(' ');
        }
        entry.push_str(&text);
    }
    labels
}

/// The facts of an element, or `None` if it has none worth sending (a plain container).
pub(crate) fn facts_for_element(
    element: &Element,
    labels: &LabelTexts,
    no_gc: &NoGC,
) -> Option<AccessibilityFacts> {
    let node = element.upcast::<Node>();
    if node.is_in_ua_widget() {
        return None;
    }
    let tag = element.local_name().to_ascii_lowercase().to_string();
    let attr = |name: &str| -> Option<String> { attribute(element, name) };
    let aria_role = attr("role").and_then(|roles| {
        roles
            .split_ascii_whitespace()
            .find_map(|token| html_roles::aria_role(&token.to_ascii_lowercase()))
    });
    let native_role = html_roles::html_role(&tag, &attr, &HtmlRoleContext::default());
    let role = aria_role.unwrap_or(native_role);

    let has_click_listener = node
        .upcast::<EventTarget>()
        .has_listeners_for(&atom!("click"));
    // Not `Element::is_focusable_area()`: it asks for style, which would force a reflow per
    // element while the reflow is being prepared. Layout hides what isn't rendered anyway, and an
    // action checks the real focusability.
    let focusable = focusable_by_markup(element, &tag, &attr);
    let named_by_author = ["aria-label", "aria-labelledby", "aria-describedby", "title"]
        .iter()
        .any(|name| attr(name).is_some());
    let has_aria_state = [
        "aria-checked",
        "aria-selected",
        "aria-expanded",
        "aria-disabled",
        "aria-hidden",
        "aria-level",
        "aria-valuenow",
    ]
    .iter()
    .any(|name| attr(name).is_some());
    let named_natively = matches!(
        tag.as_str(),
        "img" |
            "area" |
            "fieldset" |
            "table" |
            "figure" |
            "details" |
            "summary" |
            "a" |
            "h1" |
            "h2" |
            "h3" |
            "h4" |
            "h5" |
            "h6" |
            "label"
    );
    if !html_roles::is_interactive(role) &&
        !has_click_listener &&
        !focusable &&
        !named_by_author &&
        !has_aria_state &&
        !named_natively
    {
        return None;
    }

    let mut facts = AccessibilityFacts::default();
    let input = element.downcast::<HTMLInputElement>();
    let textarea = element.downcast::<HTMLTextAreaElement>();
    let select = element.downcast::<HTMLSelectElement>();
    let option = element.downcast::<HTMLOptionElement>();

    // Secrecy: `type=password`, or an autocomplete token naming a secret.
    let is_password = input.is_some_and(|input| input.Type().eq_ignore_ascii_case("password"));
    let secret_autocomplete = attr("autocomplete").is_some_and(|tokens| {
        tokens
            .split_ascii_whitespace()
            .any(accessibility_rules::autocomplete_token_is_secret)
    });
    let secrecy = accessibility_rules::secrecy(is_password, secret_autocomplete);

    // The name, in accname's order. Content comes from layout, between these and the fallback.
    facts.name = labelled_by(element, no_gc)
        .or_else(|| non_empty(attr("aria-label")))
        .or_else(|| native_name(element, &tag, labels, no_gc));
    facts.fallback_name = non_empty(attr("title")).or_else(|| {
        if input.is_some() || textarea.is_some() {
            non_empty(attr("placeholder"))
        } else {
            None
        }
    });
    facts.description = described_by(element, no_gc);
    if input.is_some() || textarea.is_some() {
        facts.placeholder = non_empty(attr("placeholder"));
    }

    let mut takes_value = false;
    if let Some(input) = input {
        let input_type = input.Type().to_string().to_ascii_lowercase();
        match input_type.as_str() {
            "checkbox" | "radio" => {
                facts.toggled = Some(if input.Indeterminate() && input_type == "checkbox" {
                    Toggled::Mixed
                } else if input.Checked() {
                    Toggled::True
                } else {
                    Toggled::False
                });
            },
            "button" | "submit" | "reset" | "image" | "file" | "hidden" => {},
            "range" | "number" => {
                takes_value = true;
                let value = input.ValueAsNumber();
                facts.numeric_value = value.is_finite().then_some(value);
                facts.min_numeric_value = attr("min").and_then(|min| min.trim().parse().ok());
                facts.max_numeric_value = attr("max").and_then(|max| max.trim().parse().ok());
                facts.value = Some(input.Value().to_string());
            },
            _ => {
                takes_value = true;
                let value = input.Value().to_string();
                facts.value = accessibility_rules::exposed_value(secrecy, &value)
                    .map(|value| clip(value).to_owned());
                facts.leaf = true;
            },
        }
        facts.required = element.has_attribute(&local_name!("required"));
    } else if let Some(textarea) = textarea {
        takes_value = true;
        let value = textarea.Value().to_string();
        facts.value =
            accessibility_rules::exposed_value(secrecy, &value).map(|value| clip(value).to_owned());
        facts.required = element.has_attribute(&local_name!("required"));
        facts.leaf = true;
    } else if let Some(select) = select {
        takes_value = true;
        facts.multiselectable = select.Multiple();
        facts.required = element.has_attribute(&local_name!("required"));
        facts.light_tree_children = true;
        let selected: Vec<String> = select
            .selected_options(no_gc)
            .iter()
            .map(|option| collapse(&option.Label().to_string()))
            .collect();
        if !selected.is_empty() {
            facts.value = Some(clip(&selected.join(", ")).to_owned());
        }
        if !select.Multiple() {
            facts.expanded = Some(element.has_attribute(&local_name!("open")));
        }
    } else if let Some(option) = option {
        facts.selected = Some(option.Selected());
        if facts.name.is_none() {
            facts.name = non_empty(Some(option.Label().to_string()));
        }
    } else if tag == "summary" {
        if let Some(details) = node
            .GetParentNode()
            .and_then(|parent| parent.downcast::<HTMLDetailsElement>().map(|d| d.Open()))
        {
            facts.expanded = Some(details);
        }
    }

    // ARIA states on ARIA widgets.
    if aria_role.is_some() {
        if let Some(checked) = attr("aria-checked") {
            facts.toggled = match checked.trim() {
                "true" => Some(Toggled::True),
                "mixed" => Some(Toggled::Mixed),
                "false" => Some(Toggled::False),
                _ => facts.toggled,
            };
        }
        if let Some(value) = attr("aria-valuenow").and_then(|v| v.trim().parse().ok()) {
            facts.numeric_value = Some(value);
            facts.min_numeric_value = attr("aria-valuemin").and_then(|v| v.trim().parse().ok());
            facts.max_numeric_value = attr("aria-valuemax").and_then(|v| v.trim().parse().ok());
        }
        if let Some(text) = non_empty(attr("aria-valuetext")) {
            facts.value = Some(text);
        }
    }
    if let Some(selected) = attr("aria-selected") {
        facts.selected = Some(selected.trim() == "true");
    }
    if let Some(expanded) = attr("aria-expanded") {
        facts.expanded = match expanded.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => facts.expanded,
        };
    }
    if attr("aria-required").is_some_and(|v| v.trim() == "true") {
        facts.required = true;
    }

    if role == Role::Heading {
        facts.level = attr("aria-level")
            .and_then(|level| level.trim().parse().ok())
            .or_else(|| tag.strip_prefix('h').and_then(|n| n.parse().ok()))
            .filter(|level| (1..=9).contains(level));
    }
    if matches!(tag.as_str(), "a" | "area") &&
        let Some(href) = attr("href")
    {
        facts.url = node
            .owner_doc()
            .base_url()
            .join(&href)
            .ok()
            .map(|url| clip(url.as_str()).to_owned());
    }

    let readonly = (input.is_some() || textarea.is_some()) &&
        element.has_attribute(&local_name!("readonly")) ||
        attr("aria-readonly").is_some_and(|v| v.trim() == "true");
    let disabled =
        element.is_actually_disabled() || attr("aria-disabled").is_some_and(|v| v.trim() == "true");
    facts.target = Target {
        activatable: html_roles::is_interactive(role) ||
            has_click_listener ||
            matches!(tag.as_str(), "summary" | "label"),
        focusable,
        takes_value,
        disabled,
        readonly,
        secret: secrecy == Secrecy::Secret,
        scrolls: element.is_document_element(),
        hidden: attr("aria-hidden").is_some_and(|v| v.trim() == "true") ||
            element.has_attribute(&local_name!("inert")),
    };
    Some(facts)
}

/// The facts of the document's root element: it always has some, so that the page can be
/// scrolled by accessibility.
pub(crate) fn root_facts(
    element: &Element,
    labels: &LabelTexts,
    no_gc: &NoGC,
) -> AccessibilityFacts {
    let mut facts = facts_for_element(element, labels, no_gc).unwrap_or_default();
    facts.target.scrolls = true;
    facts
}

/// HTML's focusable areas, from the markup alone: links with `href`, form controls, `<summary>`,
/// `<iframe>`, editing hosts, and anything with `tabindex`.
fn focusable_by_markup(
    element: &Element,
    tag: &str,
    attr: &dyn Fn(&str) -> Option<String>,
) -> bool {
    if attr("tabindex").is_some_and(|index| index.trim().parse::<i32>().is_ok()) {
        return true;
    }
    if attr("contenteditable").is_some_and(|value| !value.eq_ignore_ascii_case("false")) {
        return true;
    }
    match tag {
        "a" | "area" => attr("href").is_some(),
        "input" => {
            !attr("type").is_some_and(|t| t.eq_ignore_ascii_case("hidden")) &&
                !element.is_actually_disabled()
        },
        "button" | "select" | "textarea" => !element.is_actually_disabled(),
        "summary" | "iframe" => true,
        _ => false,
    }
}

fn attribute(element: &Element, name: &str) -> Option<String> {
    element
        .get_attribute_string_value(&html5ever::LocalName::from(name))
        .map(|value| value.to_string())
}

fn non_empty(text: Option<String>) -> Option<String> {
    let text = collapse(&text?);
    (!text.is_empty()).then(|| clip(&text).to_owned())
}

/// Whitespace collapsed to single spaces and trimmed, as accname does.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(text: &str) -> &str {
    accessibility_rules::clip_str(text)
}

/// The text of a subtree, leaving out form controls: their values are reported by the controls
/// themselves, and a secret field's value must never become part of another node's name.
fn text_without_controls(root: &Node) -> String {
    let mut text = String::new();
    let mut stack: Vec<_> = root.rev_children().collect();
    while let Some(node) = stack.pop() {
        if let Some(element) = node.downcast::<Element>() {
            let tag = element.local_name();
            if *tag == local_name!("input") ||
                *tag == local_name!("textarea") ||
                *tag == local_name!("select") ||
                *tag == local_name!("script") ||
                *tag == local_name!("style")
            {
                continue;
            }
        }
        if let Some(text_node) = node.downcast::<Text>() {
            text.push_str(&text_node.upcast::<CharacterData>().data());
            text.push(' ');
        }
        stack.extend(node.rev_children());
    }
    collapse(&text)
}

/// The text of the elements an `aria-labelledby` or `aria-describedby` attribute names.
fn referenced_text(element: &Element, attribute_name: &str, no_gc: &NoGC) -> Option<String> {
    let ids = attribute(element, attribute_name)?;
    let document = element.upcast::<Node>().owner_doc();
    let text = ids
        .split_ascii_whitespace()
        .filter_map(|id| document.get_element_by_id(no_gc, &id.into()))
        .map(|referenced| {
            attribute(&referenced, "aria-label")
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| text_without_controls(referenced.upcast()))
        })
        .collect::<Vec<_>>()
        .join(" ");
    non_empty(Some(text))
}

fn labelled_by(element: &Element, no_gc: &NoGC) -> Option<String> {
    referenced_text(element, "aria-labelledby", no_gc)
}

fn described_by(element: &Element, no_gc: &NoGC) -> Option<String> {
    referenced_text(element, "aria-describedby", no_gc)
}

/// The host language's name for an element (accname step 2E/2D for HTML, per HTML-AAM).
fn native_name(element: &Element, tag: &str, labels: &LabelTexts, no_gc: &NoGC) -> Option<String> {
    let node = element.upcast::<Node>();
    let attr = |name: &str| attribute(element, name);
    let first_child_text = |child_tag: html5ever::LocalName| {
        node.children()
            .filter_map(DomRoot::downcast::<Element>)
            .find(|child| *child.local_name() == child_tag)
            .map(|child| text_without_controls(child.upcast()))
    };
    let _ = no_gc;
    match tag {
        "input" => {
            let input = element.downcast::<HTMLInputElement>()?;
            let input_type = input.Type().to_string().to_ascii_lowercase();
            match input_type.as_str() {
                "button" | "submit" | "reset" => {
                    non_empty(attr("value")).or_else(|| match input_type.as_str() {
                        "submit" => Some("Submit".to_owned()),
                        "reset" => Some("Reset".to_owned()),
                        _ => None,
                    })
                },
                "image" => non_empty(attr("alt"))
                    .or_else(|| non_empty(attr("value")))
                    .or_else(|| Some("Submit".to_owned())),
                _ => labels.get(&node.to_opaque()).cloned(),
            }
        },
        "textarea" | "select" | "meter" | "progress" | "output" => {
            labels.get(&node.to_opaque()).cloned()
        },
        "button" => labels.get(&node.to_opaque()).cloned(),
        "img" | "area" => non_empty(attr("alt")),
        "fieldset" => non_empty(first_child_text(local_name!("legend"))),
        "table" => non_empty(first_child_text(local_name!("caption"))),
        "figure" => non_empty(first_child_text(local_name!("figcaption"))),
        "option" => non_empty(attr("label")),
        _ => None,
    }
}

/// Walk the document, compute every element's facts, and return the ones which changed since the
/// last call, adding accessibility damage to their nodes so that layout updates them. Elements
/// which no longer have facts, or which left the document, are returned with `None`.
pub(crate) fn collect_fact_changes(
    document: &Document,
    no_gc: &NoGC,
) -> Vec<(OpaqueNode, Option<AccessibilityFacts>)> {
    let Some(mut previous) = document.accessibility_data_mut().take_facts_if_stale() else {
        return vec![];
    };

    let labels = label_texts(document, no_gc);
    let root = document.GetDocumentElement();
    let mut current = FxHashMap::default();
    let mut changes = vec![];
    for node in document
        .upcast::<Node>()
        .traverse_preorder(ShadowIncluding::Yes)
    {
        let Some(element) = node.downcast::<Element>() else {
            continue;
        };
        let opaque = node.to_opaque();
        let facts = if root.as_deref() == Some(element) {
            Some(root_facts(element, &labels, no_gc))
        } else {
            facts_for_element(element, &labels, no_gc)
        };
        let old = previous.remove(&opaque);
        if facts != old {
            node.add_pending_accessibility_damage(
                no_gc,
                AccessibilityDamage::Node | AccessibilityDamage::Children,
            );
            changes.push((opaque, facts.clone()));
        }
        if let Some(facts) = facts {
            current.insert(opaque, facts);
        }
    }
    changes.extend(previous.into_keys().map(|opaque| (opaque, None)));
    document.accessibility_data_mut().store_facts(current);
    changes
}
