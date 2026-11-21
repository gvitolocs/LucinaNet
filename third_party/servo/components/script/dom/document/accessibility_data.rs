/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::Cell;

use js::context::NoGC;
use layout_api::{AccessibilityDamage, AccessibilityFacts, TrustedNodeAddress};
use rustc_hash::{FxHashMap, FxHashSet};
use script_bindings::cell::DomRefCell;
use script_bindings::root::Dom;
use servo_config::pref;
use style::dom::OpaqueNode;

use crate::dom::Node;
use crate::dom::bindings::trace::NoTrace;

#[derive(Clone, Default, JSTraceable, MallocSizeOf)]
#[cfg_attr(crown, crown::unrooted_must_root_lint::must_root)]
pub(crate) struct AccessibilityData {
    /// Nodes which have been removed from the DOM but may not yet have been removed from the
    /// accessibility tree. This is cleared after each reflow.
    rooted_nodes: FxHashSet<Dom<Node>>,

    /// Damage to the accessibility tree as a result of DOM mutations. This is drained and sent to
    /// the accessibility tree during reflow.
    pending_damage: DomRefCell<FxHashMap<Dom<Node>, NoTrace<AccessibilityDamage>>>,

    /// The [`AccessibilityFacts`] last sent to layout, per element. See
    /// `accessibility_facts::collect_fact_changes()`.
    #[no_trace]
    #[ignore_malloc_size_of = "Only while accessibility is active, and small"]
    facts: FxHashMap<OpaqueNode, AccessibilityFacts>,

    /// Something the facts depend on changed: an element's state (checkedness, focus,
    /// selectedness) or an attribute other than `style`, `class` and `data-*`. Changes to the tree
    /// and to text (a field's value included) arrive as accessibility damage instead. Not the
    /// document's version: an animation tick bumps it with nothing changed.
    facts_dirty: Cell<bool>,

    /// The focused element sent with the last accessibility reflow request.
    #[no_trace]
    #[ignore_malloc_size_of = "Only an address"]
    last_focus: Cell<Option<OpaqueNode>>,
}

impl AccessibilityData {
    /// Root a node which has been removed from the DOM but which may still have an associated
    /// accessibility tree node. It will be unrooted after the next reflow, since the accessibility
    /// tree is updated as part of the reflow process.
    ///
    /// Longer explanation:
    /// - The accessibility tree doesn't hold strong references to DOM nodes, but uses
    ///   [`OpaqueNode`]s as a way of mapping from an incoming DOM node to an existing accessibility
    ///   tree node. This allows us to cache previously computed accessibility data, and update it
    ///   based on the current DOM node state, which is passed in to the update function.
    /// - If a DOM node is garbage collected before its corresponding node is removed from the
    ///   accessibility tree, there is a risk that another new DOM node may be created at the same
    ///   memory address, causing it to have an identical `OpaqueNode`. If this `OpaqueNode` was
    ///   used to look up a node in the accessibility tree, we would get the stale accessibility
    ///   node corresponding to the node which was removed.
    /// - A DOM node is prevented from being garbage collected while it's connected to the document;
    ///   it's kept alive by strong references in its parent, child and/or sibling [`Node`]s (and in
    ///   the case of the document itself, by a strong reference in the [`Window`]). See
    ///   [`Node::first_child`], [`Node::next_sibling`], etc.
    ///    - Note that this means we only need to root nodes which are removed from the document,
    ///      and not their descendants, as descendant nodes will still be rooted via these
    ///      properties as long as the subtree root is stored here.
    /// - After a node is removed from the tree, those strong references are removed, and it _may_
    ///   become a candidate for GC if its DOM object isn't held (directly or indirectly) in script
    ///   and it isn't immediately inserted elsewhere in the DOM.
    /// - To make sure the node isn't GCed before the next accessibility update occurs, we
    ///   temporarily root it here in between its removal from the tree and the subsequent reflow.
    /// - During reflow, the accessibility tree is updated, and all stale accessibility nodes are
    ///   removed.
    /// - Once reflow has begun, no further DOM mutations can occur, and we can safely un-root these
    ///   nodes by dropping all the strong references being held here. This will allow them to be
    ///   potential candidates for GC after reflow has finished.
    ///   See [`Self::unroot_all_removed_nodes()`] and
    ///   [`Self::unroot_and_drain_all_removed_nodes()`].
    pub(crate) fn root_removed_node(&mut self, _no_gc: &NoGC, node_to_root: &Node) {
        debug_assert!(pref!(accessibility_enabled));

        self.rooted_nodes.insert(Dom::from_ref(node_to_root));
    }

    /// Clear all nodes which were rooted using [`Self::root_removed_node()`], and return the nodes
    /// which are still disconnected from the tree.
    /// This should be called instead of [`Self::unroot_all_removed_nodes()`] during reflow
    /// if [`pref::expensive_accessibility_test_assertions_enabled`] set.
    pub(crate) fn unroot_and_drain_all_removed_nodes(&mut self) -> FxHashSet<OpaqueNode> {
        self.rooted_nodes
            .drain()
            .filter_map(|node| {
                if node.is_connected() {
                    return None;
                }
                Some(node.to_opaque())
            })
            .collect()
    }

    /// Clear all nodes which were rooted using [`Self::root_removed_node()`].
    /// This should only be called during reflow.
    pub(crate) fn unroot_all_removed_nodes(&mut self) {
        self.rooted_nodes.clear();
    }

    /// Track accessibility damage to the given node caused by mutations in the DOM tree.
    pub(crate) fn add_pending_accessibility_damage_for_node(
        &self,
        node: &Node,
        damage: AccessibilityDamage,
    ) {
        assert!(pref!(accessibility_enabled));

        let map = &mut self.pending_damage.borrow_mut();
        let pending_damage = map.entry(Dom::from_ref(node)).or_default();
        pending_damage.0 |= damage;
    }

    /// Drain all pending accessibility damage so that it can be passed to the accessibility tree.
    pub(crate) fn drain_pending_accessibility_damage(
        &mut self,
    ) -> Vec<(TrustedNodeAddress, AccessibilityDamage)> {
        let pending_damage = &mut self.pending_damage.borrow_mut();
        pending_damage
            .drain()
            .map(|(node, damage)| (node.to_trusted_node_address(), damage.0))
            .collect()
    }

    /// Note that the facts may be stale: an element's state or an attribute changed.
    pub(crate) fn mark_facts_dirty(&self) {
        self.facts_dirty.set(true);
    }

    /// Forget the facts sent so far, so that all are sent again: layout made a new tree.
    pub(crate) fn reset_facts(&mut self) {
        self.facts.clear();
        self.facts_dirty.set(true);
        self.last_focus.set(None);
    }

    /// The facts last sent, if they may be stale: an element's state or attribute changed, or
    /// the tree or a text did.
    pub(crate) fn take_facts_if_stale(&mut self) -> Option<FxHashMap<OpaqueNode, AccessibilityFacts>> {
        let pending_damage = !self.pending_damage.borrow().is_empty();
        if !self.facts_dirty.get() && !pending_damage {
            return None;
        }
        Some(std::mem::take(&mut self.facts))
    }

    /// Keep the facts just sent.
    pub(crate) fn store_facts(&mut self, facts: FxHashMap<OpaqueNode, AccessibilityFacts>) {
        self.facts = facts;
        self.facts_dirty.set(false);
    }

    /// Whether the focused element differs from the one last sent, noting it as sent.
    pub(crate) fn focus_changed(&self, focus: Option<OpaqueNode>) -> bool {
        self.last_focus.replace(focus) != focus
    }
}
