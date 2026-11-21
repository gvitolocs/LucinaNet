/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Accessibility actions (AccessKit [`Action`]s) on the DOM, as Chromium's
//! `AXObject::PerformAction` and Firefox's `LocalAccessible::DoAction` do them:
//!
//! - `Click` is the element's default action: an `<option>` is picked as if from its `<select>`'s
//!   picker; anything else is focused if it is focusable and then clicked. The click is trusted and
//!   counts as user activation, as an action from assistive technology does in Chromium.
//! - `Focus` and `Blur` run the focusing and unfocusing steps.
//! - `SetValue` sets a text field's value and fires `input` then `change`, as Chromium's
//!   `kDispatchInputAndChangeEvent`; on a `<select>` it picks the option with that label or value.
//! - `ScrollIntoView` scrolls the element to the centre if it isn't visible (Chromium's
//!   `CenterIfNeeded`); `ScrollUp` and `ScrollDown` scroll the page by most of a screen.
//!
//! Every action is first checked by [`accessibility_rules::check_action`] against the element as it
//! is now: nothing runs on a hidden or disabled element, and no value is ever set on a secret
//! field (a password, a one-time code, a payment card).

use accesskit::{Action, ActionData};
use js::context::JSContext;
use layout_api::AccessibilityActionRequest;
use layout_api::accessibility_rules::{self, Act, Verdict};
use log::debug;

use crate::dom::bindings::codegen::Bindings::ElementBinding::ScrollLogicalPosition;
use crate::dom::bindings::codegen::Bindings::HTMLElementBinding::HTMLElementMethods;
use crate::dom::bindings::codegen::Bindings::HTMLInputElementBinding::HTMLInputElementMethods;
use crate::dom::bindings::codegen::Bindings::HTMLOptionElementBinding::HTMLOptionElementMethods;
use crate::dom::bindings::codegen::Bindings::HTMLSelectElementBinding::HTMLSelectElementMethods;
use crate::dom::bindings::codegen::Bindings::HTMLTextAreaElementBinding::HTMLTextAreaElementMethods;
use crate::dom::bindings::codegen::Bindings::NodeBinding::NodeMethods;
use crate::dom::bindings::codegen::Bindings::WindowBinding::{ScrollBehavior, WindowMethods};
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::root::DomRoot;
use crate::dom::bindings::str::DOMString;
use crate::dom::document::accessibility_facts::{self, LabelTexts};
use crate::dom::element::Element;
use crate::dom::event::{EventBubbles, EventCancelable, EventComposed};
use crate::dom::eventtarget::EventTarget;
use crate::dom::html::htmlelement::HTMLElement;
use crate::dom::html::htmlinputelement::HTMLInputElement;
use crate::dom::html::htmloptionelement::HTMLOptionElement;
use crate::dom::html::htmlselectelement::HTMLSelectElement;
use crate::dom::html::htmltextareaelement::HTMLTextAreaElement;
use crate::dom::html::input_type::ValueChangeEvents;
use crate::dom::node::focus::FocusTrigger;
use crate::dom::node::{Node, from_untrusted_node_address};
use crate::dom::window::Window;
use crate::dom::window::scrolling_box::{ScrollAxisState, ScrollRequirement};
use crate::dom::window::useractivation::UserActivation;

fn act_for(action: Action) -> Act {
    match action {
        Action::Click => Act::Click,
        Action::Focus => Act::Focus,
        Action::Blur => Act::Blur,
        Action::SetValue => Act::SetValue,
        Action::ScrollIntoView => Act::ScrollIntoView,
        Action::ScrollUp => Act::ScrollUp,
        Action::ScrollDown => Act::ScrollDown,
        _ => Act::Other,
    }
}

/// Perform the accessibility actions layout resolved to DOM nodes during the last reflow.
#[expect(unsafe_code)]
pub(crate) fn perform_actions(
    window: &Window,
    actions: Vec<AccessibilityActionRequest>,
    cx: &mut JSContext,
) {
    // Root every target before any action runs: an action runs page script, which can take a later
    // target out of the document, and then the GC could free it while only its address is held.
    let requests: Vec<_> = actions
        .into_iter()
        .map(|request| {
            // SAFETY: no script has run since the reflow that resolved these addresses, and layout
            // resolves actions only on nodes still in its tree at the end of that reflow, so still in
            // this document, which keeps them alive; see `AccessibilityTree::update_tree()`. Rooting
            // them does not allocate on the JS heap, so no GC runs before they are all rooted.
            let node = unsafe { from_untrusted_node_address(request.target.into()) };
            (node, request)
        })
        .collect();
    let document = window.Document();
    for (node, request) in requests {
        // An earlier action's script may have removed this node, or moved it to another document.
        // It is still alive, as it is rooted, but the action is no longer one the user saw: skip it.
        if !node.is_connected() || node.owner_doc() != document {
            debug!(
                "Accessibility action {:?} skipped: its node left the document",
                request.action
            );
            continue;
        }
        // An action on a text run goes to its element.
        let element = match DomRoot::downcast::<Element>(node.clone()) {
            Some(element) => element,
            None => match node.GetParentElement() {
                Some(element) => element,
                None => continue,
            },
        };
        let act = act_for(request.action);
        let mut target =
            accessibility_facts::facts_for_element(&element, &LabelTexts::default(), cx.no_gc())
                .map(|facts| facts.target)
                .unwrap_or_default();
        if element.is_document_element() {
            target.scrolls = true;
        }
        // Outside a reflow the real focusability can be asked for (it needs style).
        target.focusable = element.is_focusable_area(cx.no_gc());
        // An <option> has no box of its own; anything else must be rendered to be acted on.
        if !element.is::<HTMLOptionElement>() &&
            !element
                .upcast::<Node>()
                .is_being_rendered_or_delegates_rendering(None)
        {
            target.hidden = true;
        }
        let verdict = accessibility_rules::check_action(target, act);
        if verdict != Verdict::Allow {
            debug!(
                "Accessibility action {:?} refused on <{}>: {verdict:?}",
                request.action,
                element.local_name()
            );
            continue;
        }
        match act {
            Act::Click => click(&element, target.focusable, cx),
            Act::Focus => {
                element
                    .upcast::<Node>()
                    .run_the_focusing_steps(cx, None, FocusTrigger::Script);
            },
            Act::Blur => {
                if let Some(html_element) = element.downcast::<HTMLElement>() {
                    html_element.Blur(cx);
                }
            },
            Act::SetValue => set_value(&element, request.data, cx),
            Act::ScrollIntoView => {
                let center_if_needed = ScrollAxisState {
                    position: ScrollLogicalPosition::Center,
                    requirement: ScrollRequirement::IfNotVisible,
                };
                element.scroll_into_view_with_options(
                    cx,
                    ScrollBehavior::Instant,
                    center_if_needed,
                    center_if_needed,
                    None,
                    None,
                );
            },
            Act::ScrollUp | Act::ScrollDown => {
                // Most of a screen, as a page key does, so that a line stays in view.
                let page = f64::from(window.InnerHeight()) * 0.875;
                let dy = if act == Act::ScrollUp { -page } else { page };
                window.ScrollBy_(cx, 0.0, dy);
            },
            Act::Other => {},
        }
    }
}

/// The default action, Chromium's `DoDefault`.
fn click(element: &Element, focusable: bool, cx: &mut JSContext) {
    // An action from assistive technology is a user's action, as in Chromium.
    UserActivation::handle_user_activation_notification(&element.upcast::<Node>().owner_doc());

    if let Some(option) = element.downcast::<HTMLOptionElement>() {
        pick_option(option, cx);
        return;
    }
    if focusable {
        element
            .upcast::<Node>()
            .run_the_focusing_steps(cx, None, FocusTrigger::Click);
    }
    if element.click_in_progress() {
        return;
    }
    element.set_click_in_progress(true);
    element
        .upcast::<Node>()
        .fire_synthetic_pointer_event(cx, atom!("click"), true);
    element.set_click_in_progress(false);
}

/// Pick an option as the user would in its `<select>`'s picker: in a single `<select>` it
/// becomes the selection, in a multiple one it is toggled.
fn pick_option(option: &HTMLOptionElement, cx: &mut JSContext) {
    let Some(select) = option.nearest_ancestor_select() else {
        return;
    };
    let options: Vec<DomRoot<HTMLOptionElement>> = select
        .list_of_options(cx.no_gc())
        .map(|option| DomRoot::from_ref(&**option))
        .collect();
    let Some(index) = options.iter().position(|candidate| &**candidate == option) else {
        return;
    };
    let selection = if select.Multiple() {
        options
            .iter()
            .enumerate()
            .filter(|(i, candidate)| (*i == index) != candidate.Selected())
            .map(|(i, _)| i)
            .collect()
    } else {
        vec![index]
    };
    select.handle_embedder_response(cx, selection);
}

fn set_value(element: &Element, data: Option<ActionData>, cx: &mut JSContext) {
    let value = match data {
        Some(ActionData::Value(value)) => value.to_string(),
        Some(ActionData::NumericValue(value)) => value.to_string(),
        _ => return,
    };
    if let Some(select) = element.downcast::<HTMLSelectElement>() {
        let wanted = value.split_whitespace().collect::<Vec<_>>().join(" ");
        let index = select.list_of_options(cx.no_gc()).position(|option| {
            let label = option.Label().to_string();
            label.split_whitespace().collect::<Vec<_>>().join(" ") == wanted ||
                option.Value().to_string() == value
        });
        if let Some(index) = index {
            select.handle_embedder_response(cx, vec![index]);
        }
    } else if let Some(input) = element.downcast::<HTMLInputElement>() {
        if input.SetValue(cx, DOMString::from(value)).is_ok() {
            input
                .fire_value_change_events(cx, ValueChangeEvents::Input | ValueChangeEvents::Change);
        }
    } else if let Some(textarea) = element.downcast::<HTMLTextAreaElement>() {
        textarea.SetValue(cx, DOMString::from(value));
        let target = textarea.upcast::<EventTarget>();
        target.fire_event_with_params(
            cx,
            atom!("input"),
            EventBubbles::Bubbles,
            EventCancelable::NotCancelable,
            EventComposed::Composed,
        );
        target.fire_bubbling_event(cx, atom!("change"));
    }
}
