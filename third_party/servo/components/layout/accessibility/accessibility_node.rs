/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::VecDeque;
use std::fmt::Debug;

use accesskit::{Action, Affine, NodeId, Role};
use app_units::Au;
use bitflags::bitflags;
use euclid::Rect;
use layout_api::html_roles::{self, HtmlRoleContext};
use layout_api::{
    AccessibilityDamage, AccessibilityFacts, BoxAreaType, LayoutElement, LayoutNode,
    LayoutNodeType, accessibility_rules,
};
use log::trace;
use num_traits::ToPrimitive;
use script::layout_dom::{ServoLayoutElement, ServoLayoutNode};
use servo_base::print_tree::PrintTree;
use servo_config::pref;
use style::dom::{NodeInfo, OpaqueNode};
use style_traits::CSSPixel;
use web_atoms::{LocalName, ns};
use webrender_api::units::LayoutVector2D;

use super::accessibility_tree::{AccessibilityTree, AccessibilityUpdate, TreeChange};
use crate::ArcRefCell;
use crate::accessibility::AccessibilityContext;
use crate::cell::WeakRefCell;
use crate::query::{BoxAreaInclusion, process_box_area_request};

/// Convert a rectangle as layout reports it into the one [`accesskit`] wants.
fn au_rect_to_accesskit_rect(rect: Rect<Au, CSSPixel>) -> accesskit::Rect {
    accesskit::Rect::new(
        rect.min_x().to_f64_px(),
        rect.min_y().to_f64_px(),
        rect.max_x().to_f64_px(),
        rect.max_y().to_f64_px(),
    )
}

fn scroll_offset_to_affine(layout_vector: LayoutVector2D) -> Affine {
    Affine::translate((
        -layout_vector.x.to_f64().unwrap_or(0.),
        -layout_vector.y.to_f64().unwrap_or(0.),
    ))
}

bitflags! {
    /// Flags tracking an [`AccessibilityNode`]'s dirty state during an update. All flags which are
    /// set during the update should be unset by the end of the update.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) struct DirtyState : u16 {
        /// At least one descendant of this node has unresolved damage from the DOM tree.
        const DescendantHasDamage = 0b0001;
        /// This node has unresolved damage from the DOM tree.
        const HasDamage = 0b0010;
        /// This node's data changed, but it hasn't yet been added to the [`AccessibilityUpdate`].
        const Updated = 0b0100;
    }
}

bitflags! {
    /// Damage which was caused by changes to the accessibility tree. These changes can cause other
    /// properties to need to be re-computed based on the updated values, either on the same node or
    /// on other nodes.
    #[derive(Clone, Copy, Default, Debug, Eq, PartialEq)]
    pub(super) struct LocalAccessibilityDamage: u16 {
        /// This node's children changed, and/or any node in its subtree changed.
        const SubtreeChanged = 0b0001;
        /// This node's computed role changed.
        const RoleChanged = 0b0010;
        /// This node's computed label or text value (for a text node) changed.
        const TextChanged = 0b0100;
        /// This node's visibility changed.
        const VisibilityChanged = 0b1000;
        /// The facts script sent for this node changed, which may change its name.
        const FactsChanged = 0b10000;
    }
}

/// What HTML-AAM's scoped role rules need to know about a node's ancestors. It is passed down
/// the update rather than read from the ancestors, which are borrowed while their subtree updates.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Ancestry {
    /// An ancestor is sectioning content (`<article>`, `<aside>`, `<main>`, `<nav>`, `<section>`).
    in_sectioning_content: bool,
    /// An ancestor is a grid.
    in_grid: bool,
    /// How far up the nearest list-box `<select>` is: 1 for the parent, 2 for the grandparent
    /// (through an `<optgroup>`).
    list_box_select_distance: Option<u8>,
}

impl Ancestry {
    /// The ancestry of a node, from its ancestors. Only for a node whose ancestors are not
    /// borrowed: the root of an update.
    pub(super) fn of(node: &AccessibilityNode) -> Self {
        let ancestors: Vec<_> = node.ancestors().collect();
        let mut ancestry = Ancestry::default();
        for ancestor in ancestors.iter().rev() {
            ancestry = ancestor.borrow().ancestry_for_children(ancestry);
        }
        ancestry
    }
}

pub(super) struct AccessibilityNode {
    /// The unique ID for the node. This is used both as a key in [`AccessibilityTree`]'s cache of
    /// nodes, and as an identifier in [`accesskit`] datastructures: [`accesskit::Node`]s,
    /// [`accesskit::TreeUpdate`]s and [`accesskit::ActionRequest`]s.
    id: NodeId,
    /// The computed [`accesskit::Node`] data. This will be copied and serialized into a
    /// [`accesskit::TreeUpdate`] whenever it is changed during an update.
    accesskit_node: accesskit::Node,
    /// This node's parent, if any.
    parent_node: Option<WeakRefCell<AccessibilityNode>>,
    /// All this node's children.
    child_nodes: Vec<ArcRefCell<AccessibilityNode>>,
    /// The [`OpaqueNode`] for the DOM node which corresponds to this accessibility node, if any.
    /// An accessibility node may not correspond to a DOM node if it corresponds to a
    /// pseudo-element, or in a test.
    opaque_node: Option<OpaqueNode>,
    /// This node's scroll offset, if it is a scroll container which has scrolled. This is used to
    /// translate this node's children.
    scroll_offset: Option<LayoutVector2D>,
    /// Any dirty state for the current update.
    dirty_state: DirtyState,
    /// The facts script sent for this node's element, as last applied.
    facts: Option<AccessibilityFacts>,
}

impl AccessibilityNode {
    pub(super) fn new(id: NodeId) -> Self {
        Self::new_with_role(id, Role::Unknown)
    }

    pub(super) fn new_with_role(id: NodeId, role: Role) -> Self {
        Self {
            id,
            accesskit_node: accesskit::Node::new(role),
            parent_node: None,
            child_nodes: vec![],
            opaque_node: None,
            scroll_offset: None,
            dirty_state: DirtyState::empty(),
            facts: None,
        }
    }

    /// Update this node and its subtree based on damage from the DOM.
    ///
    /// - First, if this node has damage from the DOM to be resolved, update the node from the DOM
    ///   tree, recursively populating any new children.
    /// - Next, recursively call this method for any children which are dirty, or have dirty
    ///   descendants.
    /// - Finally, update any properties on this node which are may have changed due to other
    ///   changes in the tree.
    ///
    /// At the end of this method, both `has_dirty_descendants` and `is_dirty` should be false for
    /// this node and all its descendants.
    pub(super) fn update_subtree<'update>(
        &mut self,
        ref_self: ArcRefCell<Self>,
        damage_from_parent: AccessibilityDamage,
        hidden: bool,
        ancestry: Ancestry,
        context: &AccessibilityContext,
        tree: &mut AccessibilityTree,
        update: &mut AccessibilityUpdate<'update>,
    ) -> LocalAccessibilityDamage {
        let mut local_damage = LocalAccessibilityDamage::empty();

        let dom_node = update.take_dom_node(&self.id);
        let damage = update.take_damage(&self.id) | damage_from_parent;
        let mut children_changed = false;

        if let Some(dom_node) = dom_node {
            local_damage.insert(self.update_properties_and_children_from_dom_node(
                &ref_self, &dom_node, damage, ancestry, tree, update,
            ));
            local_damage
                .insert(self.update_node_from_layout(&dom_node, damage, hidden, context, update));

            if local_damage.contains(LocalAccessibilityDamage::SubtreeChanged) {
                children_changed = true;
            }

            self.dirty_state -= DirtyState::HasDamage;
        }

        let layout_damage = damage & AccessibilityDamage::Layout;
        let child_ancestry = self.ancestry_for_children(ancestry);
        if self.dirty_state.descendant_has_damage() || !layout_damage.is_empty() {
            for child_node in self.children() {
                let child_node_ref = child_node.clone();
                let mut child_node = child_node.borrow_mut();
                let child_local_damage = child_node.update_subtree(
                    child_node_ref,
                    layout_damage,
                    hidden || self.is_hidden(),
                    child_ancestry,
                    context,
                    tree,
                    update,
                );
                if !child_local_damage.is_empty() {
                    local_damage.insert(LocalAccessibilityDamage::SubtreeChanged);
                    if child_local_damage.contains(LocalAccessibilityDamage::VisibilityChanged) {
                        children_changed = true;
                    }
                }
            }
        }
        self.dirty_state -= DirtyState::DescendantHasDamage;

        if children_changed && let Some(scroll_offset) = self.scroll_offset {
            // If this node may have new, or newly-visible, children, update their scroll offsets.
            self.set_scroll_offset(scroll_offset, update);
        }

        local_damage.insert(self.update_node_local(local_damage, update));

        if self.dirty_state.updated() {
            update.add(self);
        }

        local_damage
    }

    /// Update each of this node's ancestors based on changes which have already been applied in the
    /// tree.
    pub(super) fn update_ancestors(
        &self,
        local_damage: LocalAccessibilityDamage,
        update: &mut AccessibilityUpdate,
    ) {
        if local_damage.is_empty() {
            return;
        }
        for node in self.ancestors() {
            let mut node = node.borrow_mut();
            node.update_node_local(LocalAccessibilityDamage::SubtreeChanged, update);
            node.dirty_state -= DirtyState::DescendantHasDamage;
            if node.dirty_state.updated() {
                update.add(&mut node);
            }
        }
    }

    pub(super) fn initialize_from_dom_node(&mut self, dom_node: &ServoLayoutNode) {
        self.opaque_node = Some(dom_node.opaque());
        if let Some(dom_element) = dom_node.as_element() {
            let local_name = dom_element.local_name().to_ascii_lowercase();
            self.set_html_tag(&local_name);
        }
    }

    /// Update the given [`AccessibilityNode`] from its corresponding DOM node and
    /// [`AccessibilityDamage`].
    /// If it has new children, those will be created here, but not yet populated.
    // Any changed nodes will be added to the given [`AccessibilityUpdate`].
    fn update_properties_and_children_from_dom_node<'update>(
        &mut self,
        ref_self: &ArcRefCell<Self>,
        dom_node: &ServoLayoutNode<'update>,
        dom_damage: AccessibilityDamage,
        ancestry: Ancestry,
        tree: &mut AccessibilityTree,
        update: &mut AccessibilityUpdate<'update>,
    ) -> LocalAccessibilityDamage {
        let mut local_damage = LocalAccessibilityDamage::empty();

        if !dom_damage.intersects(
            AccessibilityDamage::Node | AccessibilityDamage::Children | AccessibilityDamage::Layout,
        ) {
            return local_damage;
        }

        // We check for layout damage here because we need to walk the DOM children of nodes with
        // layout damage in order to be able to recompute their bounds. Text nodes have neither
        // bounds nor child nodes, so if the only damage is layout, we can early return here.
        if dom_damage == AccessibilityDamage::Layout && dom_node.is_text_node() {
            return local_damage;
        }

        update.counters.nodes_updated_from_dom += 1;

        if dom_damage.intersects(AccessibilityDamage::Node) {
            let facts = tree.facts_for(dom_node.opaque()).cloned();
            let context = self.role_context(dom_node, ancestry);
            local_damage.insert(self.update_properties_from_dom_node(dom_node, &context));
            local_damage.insert(self.apply_facts(facts));
        }

        if dom_damage.intersects(AccessibilityDamage::Children | AccessibilityDamage::Layout) {
            // If this node has damage from layout, this ensures that all of its children have
            // their corresponding DOM nodes in `update`.
            local_damage
                .insert(self.update_children_from_dom_node(ref_self, dom_node, tree, update));
        }

        local_damage
    }

    /// Update this node's [`Self::children`] from its corresponding DOM node.
    /// If it has new children, those will be created here, but not yet populated.
    fn update_children_from_dom_node<'update>(
        &mut self,
        ref_self: &ArcRefCell<AccessibilityNode>,
        dom_node: &ServoLayoutNode<'update>,
        tree: &mut AccessibilityTree,
        update: &mut AccessibilityUpdate<'update>,
    ) -> LocalAccessibilityDamage {
        // Text fields are leaves: their user-agent shadow tree holds their value. A `<select>`'s
        // options are its light-tree children. See `AccessibilityFacts::leaf`.
        let dom_children: Vec<_> = match &self.facts {
            Some(facts) if facts.leaf => vec![],
            Some(facts) if facts.light_tree_children => dom_node.dom_children().collect(),
            _ => dom_node.flat_tree_children().collect(),
        };
        let mut remaining_dom_children = dom_children.into_iter().peekable();
        let mut old_child_ids = self.child_ids().iter().peekable();

        // Iterate over existing children and DOM children while they match. No action is necessary
        // for these nodes.
        let mut unchanged_count = 0usize;
        while let Some(&old_id) = old_child_ids.peek() &&
            let Some(dom_child) = remaining_dom_children.peek()
        {
            if tree.existing_id_for_opaque(dom_child.opaque()) == Some(*old_id) {
                update.insert_dom_node(*old_id, *dom_child);
                unchanged_count += 1;
                old_child_ids.next();
                remaining_dom_children.next();
            } else {
                break;
            }
        }

        // If we iterated over all the DOM children without finding any changes, we're done.
        if old_child_ids.peek().is_none() && remaining_dom_children.peek().is_none() {
            return LocalAccessibilityDamage::empty();
        }

        // Remove all child nodes after the first `unchanged_count`.
        self.child_nodes.truncate(unchanged_count);
        let mut new_child_ids = Vec::from(self.child_ids());
        for removed_child_id in new_child_ids.split_off(unchanged_count) {
            update.set_tree_state_change(removed_child_id, TreeChange::Removed);
        }

        // Then, (re-)add all the remaining DOM children. Note that this means that some children
        // may end up being "Moved" even though they haven't changed parents, and may even be in the
        // same position as previously.
        let weak_self = ref_self.downgrade();
        for dom_child in remaining_dom_children {
            let (child_id, child_ref) = tree.get_or_create_node(&dom_child, update);
            // TODO(#47162): Since we need to update bounds for all nodes, we need to ensure every
            // AccessibilityNode has a corresponding DOM node available to be retrieved from the
            // AccessibilityUpdate. Once we no longer update bounds on all nodes, we won't need to
            // add all nodes like this.
            update.insert_dom_node(child_id, dom_child);

            // Update self.child_nodes in place.
            self.child_nodes.push(child_ref.clone());
            new_child_ids.push(child_id);

            let mut child = child_ref.borrow_mut();
            child.parent_node = Some(weak_self.clone());

            if update.is_new(&child_id) {
                self.dirty_state |= DirtyState::DescendantHasDamage;
            } else {
                update.set_tree_state_change(child_id, TreeChange::PendingMove);
            }

            self.dirty_state
                .propagate_descendant_has_damage(child.dirty_state);
        }

        // We can't update the AccessKit node's `children` in place, so we build up the full list
        // and then set it here.
        self.accesskit_node.set_children(new_child_ids);
        self.dirty_state |= DirtyState::Updated;

        LocalAccessibilityDamage::SubtreeChanged
    }

    /// Update this node's properties from its corresponding DOM node.
    fn update_properties_from_dom_node(
        &mut self,
        dom_node: &ServoLayoutNode,
        context: &HtmlRoleContext,
    ) -> LocalAccessibilityDamage {
        let mut local_damage = LocalAccessibilityDamage::empty();
        local_damage.insert(self.set_role(role_from_dom_node(dom_node, context)));
        if dom_node.type_id() == Some(LayoutNodeType::Text) {
            let text_content = dom_node.text_content();
            trace!("node text content = {text_content:?}");
            // FIXME: this should take into account editing selection units (grapheme clusters?)
            local_damage.insert(self.set_value(&text_content));
        }

        local_damage
    }

    /// Update this node's bounds from the current layout geometry.
    fn update_node_from_layout(
        &mut self,
        dom_node: &ServoLayoutNode<'_>,
        layout_damage: AccessibilityDamage,
        hidden: bool,
        context: &AccessibilityContext,
        update: &mut AccessibilityUpdate,
    ) -> LocalAccessibilityDamage {
        let mut local_damage = LocalAccessibilityDamage::empty();

        // Don't update bounds on nodes in hidden subtrees.
        if hidden || !layout_damage.intersects(AccessibilityDamage::Layout) {
            return local_damage;
        }

        if let Some(dom_element) = dom_node.as_element() &&
            dom_element.style_data().is_some()
        {
            let data = dom_element.element_data();
            let style = data.styles.primary();
            let hidden_by_facts = self.facts.as_ref().is_some_and(|facts| facts.target.hidden);
            if style.get_display().is_none() || hidden_by_facts {
                self.clear_bounds();
                local_damage.insert(self.set_hidden());
            } else {
                local_damage.insert(self.clear_hidden());
            }
        }

        if self.is_hidden() {
            return local_damage;
        }

        update.counters.nodes_updated_bounds += 1;

        // Border box without transforms. Bounds are in CSS pixels, relative to the document origin;
        // scroll containers set translations on their child nodes, and the embedder's graft node
        // carries the transform that composes them into AccessKit's coordinate space (see the
        // "Coordinates" section of
        // <https://docs.rs/accesskit/latest/accesskit/struct.Node.html>).
        // TODO(#47166): This doesn't take any CSS transforms into account.
        let bounds = process_box_area_request(
            context.layout_thread,
            context.stacking_context_tree,
            *dom_node,
            BoxAreaType::Border,
            BoxAreaInclusion::Inlines,
        )
        .map(au_rect_to_accesskit_rect);

        // For now only nodes with a box of their own get bounds; anything else, including
        // `display: none` content, gets its bounds cleared. That leaves two kinds of nodes
        // without geometry which assistive technology would like to have some:
        //
        // TODO(#47164): A text node never has bounds of its own: `LayoutBox::Text` has no
        // `LayoutBoxBase`, and `Fragment::Text` has no box area, so the query above always returns
        // `None` for one. Text nodes should get the union of the rectangles of their own
        // `Fragment::Text` fragments, once `cumulative_box_area_rect()` can handle those.
        //
        // TODO(#47163): A `display: contents` element generates no box either. Other
        // engines (Blink, WebKit, Gecko) compute its bounds as the union of the bounding boxes of
        // its rendered descendants.
        match bounds {
            Some(bounds) => self.set_bounds(bounds),
            None => self.clear_bounds(),
        }
        local_damage
    }

    /// Update this node's properties based on changes already made to the accessibility tree.
    /// For example, if there were nodes added or removed in its subtree, its computed text may have
    /// changed, so that will be recomputed here.
    /// If any changes are made, add this node to the given [`AccessibilityUpdate`].
    fn update_node_local(
        &mut self,
        local_damage: LocalAccessibilityDamage,
        update: &mut AccessibilityUpdate,
    ) -> LocalAccessibilityDamage {
        let mut new_damage = LocalAccessibilityDamage::empty();
        if local_damage.is_empty() {
            return new_damage;
        }
        update.counters.nodes_updated_from_tree += 1;

        if local_damage.intersects(
            LocalAccessibilityDamage::SubtreeChanged |
                LocalAccessibilityDamage::RoleChanged |
                LocalAccessibilityDamage::FactsChanged,
        ) {
            // accname's order: the author's or host language's name (from script), then the
            // content, then `title` or `placeholder`.
            let facts_name = self.facts.as_ref().and_then(|facts| facts.name.clone());
            let label = facts_name
                .or_else(|| {
                    self.label_from_descendants()
                        .filter(|text| !text.is_empty())
                })
                .or_else(|| {
                    self.facts
                        .as_ref()
                        .and_then(|facts| facts.fallback_name.clone())
                });
            if let Some(text) = label {
                new_damage.insert(self.set_label(accessibility_rules::clip_str(&text)));
            } else {
                new_damage.insert(self.clear_label());
            }
        }

        new_damage
    }

    fn label_from_descendants(&self) -> Option<String> {
        if !html_roles::name_from_contents(self.role()) {
            return None;
        }
        let mut children = VecDeque::from_iter(self.children().cloned());
        let mut text = String::new();
        while let Some(child) = children.pop_front() {
            let child = child.borrow();
            if child.is_hidden() {
                continue;
            }
            match child.role() {
                Role::TextRun => {
                    if let Some(child_text) = child.value() {
                        text.push_str(child_text);
                    }
                },
                _ => {
                    for node in child.children().rev() {
                        children.push_front(node.clone());
                    }
                },
            }
        }
        Some(text.trim().to_owned())
    }

    /// What HTML-AAM's scoped role rules need to know about this node: from its ancestry and its
    /// own attributes.
    fn role_context(&self, dom_node: &ServoLayoutNode<'_>, ancestry: Ancestry) -> HtmlRoleContext {
        let mut context = HtmlRoleContext {
            in_sectioning_content: ancestry.in_sectioning_content,
            in_grid: ancestry.in_grid,
            in_list_box_select: ancestry.list_box_select_distance.is_some(),
            ..Default::default()
        };
        if let Some(element) = dom_node.as_element() {
            let has = |name: &str| {
                element
                    .attribute_as_str(&ns!(), &LocalName::from(name))
                    .is_some_and(|value| !value.trim().is_empty())
            };
            context.has_accessible_name =
                has("aria-label") || has("aria-labelledby") || has("title");
            context.is_row_header = element
                .attribute_as_str(&ns!(), &LocalName::from("scope"))
                .is_some_and(|scope| scope.eq_ignore_ascii_case("row"));
        }
        context
    }

    /// The ancestry this node's children have, given its own.
    pub(super) fn ancestry_for_children(&self, ancestry: Ancestry) -> Ancestry {
        let tag = self.html_tag().unwrap_or_default();
        let role = self.role();
        Ancestry {
            in_sectioning_content: ancestry.in_sectioning_content ||
                matches!(tag, "article" | "aside" | "main" | "nav" | "section") ||
                matches!(
                    role,
                    Role::Article |
                        Role::Complementary |
                        Role::Main |
                        Role::Navigation |
                        Role::Region
                ),
            in_grid: ancestry.in_grid || matches!(role, Role::Grid | Role::TreeGrid),
            list_box_select_distance: if tag == "select" && role == Role::ListBox {
                Some(1)
            } else if tag == "optgroup" && ancestry.list_box_select_distance == Some(1) {
                Some(2)
            } else {
                None
            },
        }
    }

    /// Apply the facts script sent for this node's element: states, values, and the actions the
    /// node advertises. Returns [`LocalAccessibilityDamage::FactsChanged`] if anything changed.
    fn apply_facts(&mut self, facts: Option<AccessibilityFacts>) -> LocalAccessibilityDamage {
        if facts == self.facts {
            return LocalAccessibilityDamage::empty();
        }
        let node = &mut self.accesskit_node;
        let is_text = node.role() == Role::TextRun;
        node.clear_description();
        node.clear_placeholder();
        node.clear_numeric_value();
        node.clear_min_numeric_value();
        node.clear_max_numeric_value();
        node.clear_toggled();
        node.clear_selected();
        node.clear_expanded();
        node.clear_required();
        node.clear_multiselectable();
        node.clear_level();
        node.clear_url();
        node.clear_disabled();
        node.clear_read_only();
        node.clear_class_name();
        node.clear_actions();
        if !is_text {
            node.clear_value();
        }
        let target = facts.as_ref().map(|facts| facts.target).unwrap_or_default();
        if let Some(facts) = &facts {
            if let Some(description) = &facts.description {
                node.set_description(description.as_str());
            }
            if let Some(placeholder) = &facts.placeholder {
                node.set_placeholder(placeholder.as_str());
            }
            if !is_text && let Some(value) = &facts.value {
                node.set_value(value.as_str());
            }
            if let Some(value) = facts.numeric_value {
                node.set_numeric_value(value);
            }
            if let Some(value) = facts.min_numeric_value {
                node.set_min_numeric_value(value);
            }
            if let Some(value) = facts.max_numeric_value {
                node.set_max_numeric_value(value);
            }
            if let Some(toggled) = facts.toggled {
                node.set_toggled(toggled);
            }
            if let Some(selected) = facts.selected {
                node.set_selected(selected);
            }
            if let Some(expanded) = facts.expanded {
                node.set_expanded(expanded);
            }
            if facts.required {
                node.set_required();
            }
            if facts.multiselectable {
                node.set_multiselectable();
            }
            if let Some(level) = facts.level {
                node.set_level(level);
            }
            if let Some(url) = &facts.url {
                node.set_url(url.as_str());
            }
            if target.disabled {
                node.set_disabled();
            }
            if target.readonly {
                node.set_read_only();
            }
            // AccessKit has no flag for a protected field (IAccessible2's STATE_PROTECTED, ATK's
            // PASSWORD_TEXT); a secret field that isn't a password (a one-time code, a card) says
            // so here, so that a reader knows why it has no value.
            if target.secret {
                node.set_class_name("secret");
            }
        }
        // Every node but a bare text run or container can be scrolled into view, as in Chromium;
        // the rest of the actions are what the rules allow for this element.
        let advertised = accessibility_rules::advertised(target);
        if advertised.click {
            node.add_action(Action::Click);
        }
        if advertised.focus {
            node.add_action(Action::Focus);
            node.add_action(Action::Blur);
        }
        if advertised.set_value {
            node.add_action(Action::SetValue);
        }
        if advertised.scroll {
            node.add_action(Action::ScrollUp);
            node.add_action(Action::ScrollDown);
        }
        if advertised.scroll_into_view &&
            (facts.is_some() || !matches!(node.role(), Role::TextRun | Role::GenericContainer))
        {
            node.add_action(Action::ScrollIntoView);
        }
        self.facts = facts;
        self.dirty_state |= DirtyState::Updated;
        LocalAccessibilityDamage::FactsChanged
    }

    pub(super) fn print(&self, print_tree: &mut PrintTree, tree: &AccessibilityTree) {
        let focused = if tree.focused_node_id() == Some(self.id) {
            "[focused] "
        } else {
            ""
        };
        let node_string = format!("{focused}{self:?}");

        if self.child_nodes.is_empty() {
            print_tree.add_item(node_string);
            return;
        }

        print_tree.new_level(node_string);

        for child in self.children() {
            child.borrow().print(print_tree, tree);
        }
        print_tree.end_level();
    }

    pub(super) fn clone_accesskit_node(&self) -> accesskit::Node {
        self.accesskit_node.clone()
    }

    pub(super) fn id(&self) -> NodeId {
        self.id
    }

    pub(super) fn opaque_node(&self) -> Option<OpaqueNode> {
        self.opaque_node
    }

    pub(super) fn dirty_state(&self) -> DirtyState {
        self.dirty_state
    }

    pub(super) fn add_dirty_state(&mut self, state: DirtyState) {
        self.dirty_state |= state;
    }

    pub(super) fn remove_dirty_state(&mut self, state: DirtyState) {
        self.dirty_state -= state;
    }

    pub(super) fn parent(&self) -> Option<ArcRefCell<AccessibilityNode>> {
        self.parent_node.as_ref().and_then(|weak| weak.upgrade())
    }

    pub(super) fn children(
        &self,
    ) -> impl DoubleEndedIterator<Item = &ArcRefCell<AccessibilityNode>> {
        self.child_nodes.iter()
    }

    #[cfg(test)]
    pub(super) fn set_children_for_testing(
        &mut self,
        child_ids: Vec<NodeId>,
        child_nodes: Vec<ArcRefCell<AccessibilityNode>>,
    ) {
        self.accesskit_node.set_children(child_ids);
        self.child_nodes = child_nodes;
    }

    pub(super) fn ancestors(&self) -> impl Iterator<Item = ArcRefCell<AccessibilityNode>> {
        AccessibilityNodeIterator::new(self.parent(), |node| node.parent_node.clone()?.upgrade())
    }

    pub(super) fn child_ids(&self) -> &[NodeId] {
        self.accesskit_node.children()
    }

    pub(super) fn set_scroll_offset(
        &mut self,
        offset: LayoutVector2D,
        update: &mut AccessibilityUpdate,
    ) {
        self.scroll_offset = Some(offset);
        let transform = scroll_offset_to_affine(offset);
        for child in self.children() {
            let mut child = child.borrow_mut();
            if child.is_hidden() {
                continue;
            }
            child.set_transform(transform);
            if child.dirty_state.updated() {
                update.add(&mut child);
            }
        }
    }

    pub(super) fn role(&self) -> Role {
        self.accesskit_node.role()
    }

    pub(super) fn set_role(&mut self, role: Role) -> LocalAccessibilityDamage {
        if role == self.accesskit_node.role() {
            return LocalAccessibilityDamage::empty();
        }
        self.accesskit_node.set_role(role);
        self.dirty_state |= DirtyState::Updated;
        LocalAccessibilityDamage::RoleChanged
    }

    fn label(&self) -> Option<&str> {
        self.accesskit_node.label()
    }

    fn set_label(&mut self, label: &str) -> LocalAccessibilityDamage {
        if Some(label) == self.accesskit_node.label() {
            return LocalAccessibilityDamage::empty();
        }
        self.accesskit_node.set_label(label);
        self.dirty_state |= DirtyState::Updated;
        LocalAccessibilityDamage::TextChanged
    }

    fn clear_label(&mut self) -> LocalAccessibilityDamage {
        if self.accesskit_node.label().is_none() {
            return LocalAccessibilityDamage::empty();
        }
        self.accesskit_node.clear_label();
        self.dirty_state |= DirtyState::Updated;
        LocalAccessibilityDamage::TextChanged
    }

    fn html_tag(&self) -> Option<&str> {
        self.accesskit_node.html_tag()
    }

    fn set_html_tag(&mut self, html_tag: &str) {
        if Some(html_tag) == self.accesskit_node.html_tag() {
            return;
        }
        self.accesskit_node.set_html_tag(html_tag);
        self.dirty_state |= DirtyState::Updated;
    }

    fn value(&self) -> Option<&str> {
        self.accesskit_node.value()
    }

    fn set_value(&mut self, value: &str) -> LocalAccessibilityDamage {
        if Some(value) == self.accesskit_node.value() {
            return LocalAccessibilityDamage::empty();
        }
        self.accesskit_node.set_value(value);
        self.dirty_state |= DirtyState::Updated;
        LocalAccessibilityDamage::TextChanged
    }

    fn is_hidden(&self) -> bool {
        self.accesskit_node.is_hidden()
    }

    fn set_hidden(&mut self) -> LocalAccessibilityDamage {
        if self.is_hidden() {
            return LocalAccessibilityDamage::empty();
        }
        self.accesskit_node.set_hidden();
        self.dirty_state |= DirtyState::Updated;
        LocalAccessibilityDamage::VisibilityChanged
    }

    fn clear_hidden(&mut self) -> LocalAccessibilityDamage {
        if !self.is_hidden() {
            return LocalAccessibilityDamage::empty();
        }

        self.accesskit_node.clear_hidden();
        self.dirty_state |= DirtyState::Updated;
        LocalAccessibilityDamage::VisibilityChanged
    }

    fn bounds(&self) -> Option<accesskit::Rect> {
        self.accesskit_node.bounds()
    }

    fn set_bounds(&mut self, bounds: accesskit::Rect) {
        if Some(bounds) == self.accesskit_node.bounds() {
            return;
        }
        self.accesskit_node.set_bounds(bounds);
        self.dirty_state |= DirtyState::Updated;
    }

    fn clear_bounds(&mut self) {
        if self.accesskit_node.bounds().is_none() {
            return;
        }
        self.accesskit_node.clear_bounds();
        self.dirty_state |= DirtyState::Updated;
    }

    fn set_transform(&mut self, transform: Affine) {
        // TODO(#47166): Right now a node will only ever have a single transform from a scroll
        // container, if any. Once we correctly support CSS transforms, a node may have multiple
        // transforms, which we'll need to be able to combine.
        if self.accesskit_node.transform() == Some(&transform) {
            return;
        }
        if transform == Affine::IDENTITY {
            self.clear_transform();
            return;
        }
        self.accesskit_node.set_transform(transform);
        self.dirty_state |= DirtyState::Updated;
    }

    fn clear_transform(&mut self) {
        if self.accesskit_node.transform().is_none() {
            return;
        }
        self.accesskit_node.clear_transform();
        self.dirty_state |= DirtyState::Updated;
    }

    pub(super) fn assert_integrity(&self, expected_parent: Option<WeakRefCell<AccessibilityNode>>) {
        debug_assert!(pref!(expensive_accessibility_test_assertions_enabled));

        if let Some(actual_parent) = &self.parent_node {
            let expected = expected_parent.expect("Actual parent but no expected parent");
            let expected = expected.upgrade().expect("Expected parent was dropped");
            let actual = actual_parent.upgrade().expect("Actual parent was dropped");
            assert!(actual.ptr_eq(&expected));
        } else {
            assert!(
                expected_parent.is_none(),
                "Expected parent but no actual parent"
            );
        }

        assert!(
            self.dirty_state.is_empty(),
            "{self:?} has dirty state {:?}",
            self.dirty_state
        );

        let children_ids: Vec<_> = self.children().map(|child| child.borrow().id).collect();
        assert_eq!(
            children_ids,
            self.child_ids(),
            "children() IDs didn't match child_ids() for {self:?}"
        );
    }
}

impl Debug for AccessibilityNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_hidden() {
            write!(f, "[hidden] ")?;
        }
        write!(f, "{:?}: {:?}", self.id, self.role())?;
        if let Some(html_tag) = self.html_tag() {
            write!(f, " (html_tag: {html_tag:?})")?;
        }
        if let Some(label) = self.label() {
            write!(f, "\nlabel: {label:?}")?;
        }
        if let Some(bounds) = self.bounds() {
            write!(f, "\nbounds: {bounds:?}")?;
        }
        if !self.child_ids().is_empty() {
            write!(f, "\nchildren: {:?}", self.child_ids())?;
        }
        Ok(())
    }
}

/// <https://w3c.github.io/aria/#host_general_role>: the first token of the `role` attribute that
/// names a role, as ARIA's fallback roles say.
fn role_from_role_attribute(dom_element: &ServoLayoutElement<'_>) -> Option<Role> {
    let role_attribute = dom_element.attribute(&ns!(), &LocalName::from("role"))?;
    role_attribute
        .as_tokens()
        .iter()
        .find_map(|token| html_roles::aria_role(&token.to_ascii_lowercase()))
}

/// The role of a DOM node: its ARIA role if it has one, else its HTML-AAM role.
fn role_from_dom_node(dom_node: &ServoLayoutNode<'_>, context: &HtmlRoleContext) -> Role {
    if let Some(dom_element) = dom_node.as_element() {
        role_from_role_attribute(&dom_element).unwrap_or_else(|| {
            let tag = dom_element.local_name().to_ascii_lowercase();
            let attr = |name: &str| {
                dom_element
                    .attribute_as_str(&ns!(), &LocalName::from(name))
                    .map(str::to_owned)
            };
            html_roles::html_role(&tag, &attr, context)
        })
    } else if dom_node.type_id() == Some(LayoutNodeType::Text) {
        Role::TextRun
    } else {
        Role::GenericContainer
    }
}

struct AccessibilityNodeIterator<I>
where
    I: Fn(&AccessibilityNode) -> Option<ArcRefCell<AccessibilityNode>>,
{
    next_value: Option<ArcRefCell<AccessibilityNode>>,
    next_fn: I,
}

impl<I> AccessibilityNodeIterator<I>
where
    I: Fn(&AccessibilityNode) -> Option<ArcRefCell<AccessibilityNode>>,
{
    fn new(next_value: Option<ArcRefCell<AccessibilityNode>>, next_fn: I) -> Self {
        AccessibilityNodeIterator {
            next_value,
            next_fn,
        }
    }
}

impl<I> Iterator for AccessibilityNodeIterator<I>
where
    I: Fn(&AccessibilityNode) -> Option<ArcRefCell<AccessibilityNode>>,
{
    type Item = ArcRefCell<AccessibilityNode>;

    fn next(&mut self) -> Option<Self::Item> {
        let next_value = self.next_value.take();
        self.next_value = next_value
            .as_ref()
            .and_then(|node| (self.next_fn)(&node.borrow()));
        next_value
    }
}

impl DirtyState {
    pub(super) fn updated(&self) -> bool {
        self.contains(DirtyState::Updated)
    }

    pub(super) fn descendant_has_damage(&self) -> bool {
        self.contains(DirtyState::DescendantHasDamage)
    }

    pub(super) fn propagate_descendant_has_damage(&mut self, child_dirty_state: DirtyState) {
        if child_dirty_state.self_or_descendant_has_damage() {
            self.insert(DirtyState::DescendantHasDamage)
        }
    }

    pub(super) fn self_or_descendant_has_damage(&self) -> bool {
        self.intersects(DirtyState::HasDamage | DirtyState::DescendantHasDamage)
    }
}
