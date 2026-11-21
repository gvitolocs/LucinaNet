/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::RefCell;

use dom_struct::dom_struct;
use js::context::JSContext;
use script_bindings::reflector::reflect_dom_object;
use style::shared_lock::{SharedRwLockReadGuard, ToCssWithGuard};
use style::stylesheets::{CssRule as StyleCssRule, CssRuleType};

use super::cssrule::{CSSRule, SpecificCSSRule};
use super::cssstylesheet::CSSStyleSheet;
use crate::dom::bindings::root::DomRoot;
use crate::dom::bindings::str::DOMString;
use crate::dom::css::cssgroupingrule::CSSGroupingRule;
use crate::dom::window::Window;

/// What a stylo at-rule looks like to a page before it has an interface of its own: the rule
/// keeps its identity and its serialisation, and gains both again when the sheet is re-parsed.
struct UnknownRule {
    ty: CssRuleType,
    css: String,
}

impl UnknownRule {
    fn of(rule: &StyleCssRule, guard: &SharedRwLockReadGuard) -> Self {
        Self {
            ty: rule.rule_type(),
            css: rule.to_css_string(guard),
        }
    }
}

/// <https://drafts.csswg.org/cssom/#dom-cssrule-type>
#[dom_struct]
pub(crate) struct CSSUnknownRule {
    css_rule: CSSRule,
    /// The rule as text, which stylo owns and this mirrors; nothing here is a DOM pointer.
    #[ignore_malloc_size_of = "a copy of the rule type and its serialisation"]
    #[no_trace]
    unknown_rule: RefCell<UnknownRule>,
}

impl CSSUnknownRule {
    fn new_inherited(
        parent_rule: Option<&CSSGroupingRule>,
        parent_stylesheet: &CSSStyleSheet,
        unknown_rule: UnknownRule,
    ) -> Self {
        Self {
            css_rule: CSSRule::new_inherited(parent_rule, parent_stylesheet),
            unknown_rule: RefCell::new(unknown_rule),
        }
    }

    pub(crate) fn new(
        cx: &mut JSContext,
        window: &Window,
        parent_rule: Option<&CSSGroupingRule>,
        parent_stylesheet: &CSSStyleSheet,
        rule: StyleCssRule,
    ) -> DomRoot<Self> {
        // Serialised here, while the rule is still owned by the caller: `CSSRule::new_specific`
        // matches the stylo rule by value, so this is the last place the whole rule exists.
        let guard = parent_stylesheet.shared_lock().read();
        let unknown_rule = UnknownRule::of(&rule, &guard);
        drop(guard);
        reflect_dom_object(
            cx,
            Box::new(Self::new_inherited(
                parent_rule,
                parent_stylesheet,
                unknown_rule,
            )),
            window,
        )
    }

    pub(crate) fn update_rule(&self, rule: &StyleCssRule, guard: &SharedRwLockReadGuard) {
        *self.unknown_rule.borrow_mut() = UnknownRule::of(rule, guard);
    }
}

impl SpecificCSSRule for CSSUnknownRule {
    fn ty(&self) -> CssRuleType {
        self.unknown_rule.borrow().ty
    }

    /// The CSSOM constants name the rule interfaces that exist, and this rule has none.
    fn type_constant(&self) -> u16 {
        0
    }

    fn get_css(&self) -> DOMString {
        self.unknown_rule.borrow().css.clone().into()
    }
}
