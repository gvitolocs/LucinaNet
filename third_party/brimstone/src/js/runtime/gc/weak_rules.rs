//! The pure decisions of the weak-edge repair.
//!
//! `garbage_collector.rs` keeps every heap read, every slot write, every loop and the order of the
//! passes; it calls one of these functions for each decision it makes. Everything here is plain
//! Rust over plain values: `bool`, `usize`, `Option<usize>`, small local enums, and slices of
//! `Copy` elements. Addresses are `usize`, kinds are the local `WeakKind`, and the resolution of a
//! weak slot is an `Option<usize>` (`Some(addr)` live, `None` collected).

/// A heap item kind that holds at least one weak slot, plus `NoWeak` for every other kind. Mapped
/// from `HeapItemKind` by `holder_of` in `garbage_collector.rs`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WeakKind {
    Shape,
    TransitionVec,
    TransitionTreeRootsMap,
    InternedStringsSet,
    WeakRefObject,
    WeakSetObject,
    WeakValueSet,
    WeakMapObject,
    WeakValueMap,
    FinalizationRegistryObject,
    FinalizationRegistryCells,
    PrototypeObjectChildrenShapesVec,
    CacheArray,
    PolymorphicCacheArray,
    NoWeak,
}

/// A target is in a moved space exactly when it is in the from-space, or in the old permanent space
/// during a resizing collection.
#[inline]
pub fn is_moved(in_from: bool, resizing: bool, in_old_perm: bool) -> bool {
    in_from || (resizing && in_old_perm)
}

/// A moved target is live exactly when it has a forwarding pointer, and then resolves to it. An
/// unmoved host target is dead exactly when unmarked. Any other unmoved target is live and
/// unchanged.
///
/// `addr` is the target's own address; `fwd` is the forwarding pointer the copy phase left, if any;
/// `host_marked` is only read by the collector for a target that is in the host space.
#[inline]
pub fn resolve(
    in_moved: bool,
    in_host: bool,
    host_marked: bool,
    fwd: Option<usize>,
    addr: usize,
) -> Option<usize> {
    if in_moved {
        fwd
    } else if in_host {
        host_marked.then_some(addr)
    } else {
        Some(addr)
    }
}

/// The intrusive list a moved holder is linked into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListAction {
    WeakRef,
    WeakSet,
    WeakMap,
    FinalizationRegistry,
    WeakVec,
    CacheArray,
    PolymorphicCacheArray,
    NoList,
}

/// Exactly the seven list kinds are linked into a list when they move, each into its own list. No
/// other kind is.
#[inline]
pub fn list_action(kind: WeakKind) -> ListAction {
    match kind {
        WeakKind::WeakRefObject => ListAction::WeakRef,
        WeakKind::WeakSetObject => ListAction::WeakSet,
        WeakKind::WeakMapObject => ListAction::WeakMap,
        WeakKind::FinalizationRegistryObject => ListAction::FinalizationRegistry,
        WeakKind::PrototypeObjectChildrenShapesVec => ListAction::WeakVec,
        WeakKind::CacheArray => ListAction::CacheArray,
        WeakKind::PolymorphicCacheArray => ListAction::PolymorphicCacheArray,
        _ => ListAction::NoList,
    }
}

/// A space the repair sweep walks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SweptSpace {
    NewPerm,
    To,
}

/// The repair sweep walks both the new permanent space and the to-space. Neither range may be
/// dropped.
#[inline]
pub fn swept_spaces() -> [SweptSpace; 2] {
    [SweptSpace::NewPerm, SweptSpace::To]
}

/// What the sweep does with one holder.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SweepAction {
    ShapeEdge,
    PruneInternedStringsSet,
    PruneRootsMap,
    CompressTransitionVec,
    PruneWeakValueSet,
    PruneWeakValueMap,
    PruneFinalizationRegistryCells,
    Skip,
}

/// Shape to repair its edge. InternedStringsSet to prune the set. RootsMap to prune the roots.
/// TransitionVec to compress. The three data kinds to prune when they are not owned by a listed
/// collection, and to skip when they are: a live collection fixes its own data, with its side
/// effects. Every other kind is skipped.
#[inline]
pub fn sweep_action(kind: WeakKind, owned: bool) -> SweepAction {
    match kind {
        WeakKind::Shape => SweepAction::ShapeEdge,
        WeakKind::InternedStringsSet => SweepAction::PruneInternedStringsSet,
        WeakKind::TransitionTreeRootsMap => SweepAction::PruneRootsMap,
        WeakKind::TransitionVec => SweepAction::CompressTransitionVec,
        WeakKind::WeakValueSet if !owned => SweepAction::PruneWeakValueSet,
        WeakKind::WeakValueMap if !owned => SweepAction::PruneWeakValueMap,
        WeakKind::FinalizationRegistryCells if !owned => {
            SweepAction::PruneFinalizationRegistryCells
        }
        _ => SweepAction::Skip,
    }
}

/// What to do with the weak edge a shape holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShapeEdgeAction {
    CompressVec,
    SetNextShape(usize),
    Clear,
}

/// A shape with a vec compresses the vec, whatever the resolution: the vec pointer is strong, so
/// the caller does not resolve it at all. Otherwise a live next shape is written back, a dead one
/// clears the slot.
#[inline]
pub fn shape_edge_action(has_transitions_vec: bool, resolved: Option<usize>) -> ShapeEdgeAction {
    if has_transitions_vec {
        ShapeEdgeAction::CompressVec
    } else {
        match resolved {
            Some(next_shape) => ShapeEdgeAction::SetNextShape(next_shape),
            None => ShapeEdgeAction::Clear,
        }
    }
}

/// Keep exactly the entries `keep` returns, in their original order, rewritten to what `keep`
/// returns. Compacts the prefix `s[..len]` in place and returns the new length; nothing at or past
/// `len` is read.
#[inline]
pub fn compact<T: Copy>(s: &mut [T], len: usize, mut keep: impl FnMut(T) -> Option<T>) -> usize {
    debug_assert!(len <= s.len());

    let mut next_kept_index = 0;

    for index in 0..len {
        let element = s[index];

        if let Some(kept) = keep(element) {
            s[next_kept_index] = kept;
            next_kept_index += 1;
        }
    }

    next_kept_index
}

/// The tag of a transition's kind, without the payload the collector reads the slot from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransitionTag {
    DefineProperty,
    SetPrototype,
    PreventExtensions,
}

/// Which further weak slot a kept transition holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransitionFixup {
    Key,
    Prototype,
    None,
}

/// A DefineProperty holds its key weakly, a SetPrototype holds its prototype weakly; both are
/// resolved and must be live. PreventExtensions holds nothing further.
///
/// Whether the key is a heap item at all, and whether the prototype is present, is a slot read and
/// stays with the caller.
#[inline]
pub fn transition_fixup(tag: TransitionTag) -> TransitionFixup {
    match tag {
        TransitionTag::DefineProperty => TransitionFixup::Key,
        TransitionTag::SetPrototype => TransitionFixup::Prototype,
        TransitionTag::PreventExtensions => TransitionFixup::None,
    }
}

/// What to do with one entry of a transition tree roots map.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RootsEntryAction {
    Remove,
    UpdateShape { shape: usize },
    UpdateShapeAndPrototype { shape: usize, prototype: usize },
    DeadPrototype,
}

/// A roots entry with a dead shape is removed. A live one is updated, and its key prototype must be
/// live.
///
/// `prototype` is the resolution of the key's prototype slot: `None` when the key names no
/// prototype, `Some(res)` when it names one.
#[inline]
pub fn roots_entry_action(
    shape: Option<usize>,
    prototype: Option<Option<usize>>,
) -> RootsEntryAction {
    match (shape, prototype) {
        (None, _) => RootsEntryAction::Remove,
        (Some(shape), None) => RootsEntryAction::UpdateShape { shape },
        (Some(shape), Some(Some(prototype))) => {
            RootsEntryAction::UpdateShapeAndPrototype { shape, prototype }
        }
        (Some(_), Some(None)) => RootsEntryAction::DeadPrototype,
    }
}

/// What to do with one entry of outgrown weak map data.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapEntryAction {
    Remove,
    UpdateKey { key: usize },
    UpdateKeyAndValue { key: usize, value: usize },
}

/// A dead key removes the entry. A live key with a pointer value that is dead removes it too;
/// otherwise the entry is kept with key and value updated. Nothing traced the value, so it is
/// resolved rather than assumed live.
///
/// `value` is the resolution of the value, which the collector only reads for a pointer value.
#[inline]
pub fn map_entry_action(
    key: Option<usize>,
    value_is_ptr: bool,
    value: Option<usize>,
) -> MapEntryAction {
    match (key, value_is_ptr, value) {
        (None, _, _) => MapEntryAction::Remove,
        (Some(key), false, _) => MapEntryAction::UpdateKey { key },
        (Some(key), true, Some(value)) => MapEntryAction::UpdateKeyAndValue { key, value },
        (Some(_), true, None) => MapEntryAction::Remove,
    }
}

/// What to do with one cell of outgrown finalization registry cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CellAction {
    DeleteCell,
    UpdateTarget { target: usize },
    UpdateTargetAndToken { target: usize, token: usize },
    UpdateTargetClearToken { target: usize },
}

/// A dead target deletes the cell, with no callback; the registry that owned these cells runs the
/// callback from its own copy. A dead pointer token becomes `None`. Otherwise both are updated.
///
/// `token` is the resolution of the token, which the collector only reads for a pointer token.
#[inline]
pub fn cell_action(target: Option<usize>, token_is_ptr: bool, token: Option<usize>) -> CellAction {
    let Some(target) = target else {
        return CellAction::DeleteCell;
    };

    if !token_is_ptr {
        return CellAction::UpdateTarget { target };
    }

    match token {
        Some(token) => CellAction::UpdateTargetAndToken { target, token },
        None => CellAction::UpdateTargetClearToken { target },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_moved_from_space_always_moved() {
        assert!(is_moved(true, false, false));
        assert!(is_moved(true, true, false));
    }

    #[test]
    fn is_moved_old_perm_only_when_resizing() {
        assert!(is_moved(false, true, true));
        assert!(!is_moved(false, false, true));
    }

    #[test]
    fn is_moved_other_spaces_never_moved() {
        assert!(!is_moved(false, true, false));
        assert!(!is_moved(false, false, false));
    }

    #[test]
    fn resolve_moved_target_follows_forwarding_pointer() {
        assert_eq!(resolve(true, false, false, Some(44), 12), Some(44));
        assert_eq!(resolve(true, false, false, None, 12), None);
    }

    #[test]
    fn resolve_unmoved_host_target_needs_the_mark() {
        assert_eq!(resolve(false, true, true, None, 12), Some(12));
        assert_eq!(resolve(false, true, false, None, 99), None);
    }

    #[test]
    fn resolve_unmoved_other_target_is_unchanged() {
        assert_eq!(resolve(false, false, false, None, 12), Some(12));
        assert_eq!(resolve(false, false, true, None, 12), Some(12));
    }

    #[test]
    fn list_action_covers_every_list_kind() {
        assert_eq!(list_action(WeakKind::WeakRefObject), ListAction::WeakRef);
        assert_eq!(list_action(WeakKind::WeakSetObject), ListAction::WeakSet);
        assert_eq!(list_action(WeakKind::WeakMapObject), ListAction::WeakMap);
        assert_eq!(
            list_action(WeakKind::FinalizationRegistryObject),
            ListAction::FinalizationRegistry
        );
        assert_eq!(list_action(WeakKind::PrototypeObjectChildrenShapesVec), ListAction::WeakVec);
        assert_eq!(list_action(WeakKind::CacheArray), ListAction::CacheArray);
        assert_eq!(list_action(WeakKind::PolymorphicCacheArray), ListAction::PolymorphicCacheArray);
    }

    #[test]
    fn list_action_skips_every_other_kind() {
        assert_eq!(list_action(WeakKind::Shape), ListAction::NoList);
        assert_eq!(list_action(WeakKind::TransitionVec), ListAction::NoList);
        assert_eq!(list_action(WeakKind::WeakValueSet), ListAction::NoList);
        assert_eq!(list_action(WeakKind::NoWeak), ListAction::NoList);
    }

    #[test]
    fn swept_spaces_walks_both_ranges() {
        assert_eq!(swept_spaces(), [SweptSpace::NewPerm, SweptSpace::To]);
    }

    #[test]
    fn sweep_action_repairs_the_named_walks_holders() {
        assert_eq!(sweep_action(WeakKind::Shape, false), SweepAction::ShapeEdge);
        assert_eq!(
            sweep_action(WeakKind::InternedStringsSet, false),
            SweepAction::PruneInternedStringsSet
        );
        assert_eq!(
            sweep_action(WeakKind::TransitionTreeRootsMap, true),
            SweepAction::PruneRootsMap
        );
        assert_eq!(sweep_action(WeakKind::TransitionVec, true), SweepAction::CompressTransitionVec);
    }

    #[test]
    fn sweep_action_prunes_unowned_data_only() {
        assert_eq!(sweep_action(WeakKind::WeakValueSet, false), SweepAction::PruneWeakValueSet);
        assert_eq!(sweep_action(WeakKind::WeakValueSet, true), SweepAction::Skip);
        assert_eq!(sweep_action(WeakKind::WeakValueMap, false), SweepAction::PruneWeakValueMap);
        assert_eq!(sweep_action(WeakKind::WeakValueMap, true), SweepAction::Skip);
        assert_eq!(
            sweep_action(WeakKind::FinalizationRegistryCells, false),
            SweepAction::PruneFinalizationRegistryCells
        );
        assert_eq!(sweep_action(WeakKind::FinalizationRegistryCells, true), SweepAction::Skip);
    }

    #[test]
    fn sweep_action_skips_list_kinds_and_everything_else() {
        assert_eq!(sweep_action(WeakKind::WeakRefObject, false), SweepAction::Skip);
        assert_eq!(sweep_action(WeakKind::CacheArray, false), SweepAction::Skip);
        assert_eq!(sweep_action(WeakKind::NoWeak, false), SweepAction::Skip);
    }

    #[test]
    fn shape_edge_action_vec_ignores_the_resolution() {
        assert_eq!(shape_edge_action(true, Some(20)), ShapeEdgeAction::CompressVec);
        assert_eq!(shape_edge_action(true, None), ShapeEdgeAction::CompressVec);
    }

    #[test]
    fn shape_edge_action_next_shape_follows_the_resolution() {
        assert_eq!(shape_edge_action(false, Some(20)), ShapeEdgeAction::SetNextShape(20));
        assert_eq!(shape_edge_action(false, None), ShapeEdgeAction::Clear);
    }

    #[test]
    fn compact_keeps_live_entries_in_order() {
        let mut v = [1usize, 2, 3, 4];
        let len = compact(&mut v, 4, |x| if x % 2 == 0 { Some(10 * x) } else { None });

        assert_eq!(len, 2);
        assert_eq!(v, [20, 40, 3, 4]);
    }

    #[test]
    fn compact_only_touches_the_prefix() {
        let mut v = [1usize, 2, 3, 4];
        let len = compact(&mut v, 2, |x| Some(x + 100));

        assert_eq!(len, 2);
        assert_eq!(v, [101, 102, 3, 4]);
    }

    #[test]
    fn compact_of_everything_and_of_nothing() {
        let mut v = [1usize, 2, 3];
        assert_eq!(compact(&mut v, 3, |x| Some(x)), 3);
        assert_eq!(v, [1, 2, 3]);

        let mut v = [1usize, 2, 3];
        assert_eq!(compact(&mut v, 3, |_| None), 0);

        let mut v = [1usize, 2, 3];
        assert_eq!(compact(&mut v, 0, |_| None), 0);
        assert_eq!(v, [1, 2, 3]);
    }

    #[test]
    fn transition_fixup_names_the_further_weak_slot() {
        assert_eq!(transition_fixup(TransitionTag::DefineProperty), TransitionFixup::Key);
        assert_eq!(transition_fixup(TransitionTag::SetPrototype), TransitionFixup::Prototype);
        assert_eq!(transition_fixup(TransitionTag::PreventExtensions), TransitionFixup::None);
    }

    #[test]
    fn roots_entry_action_removes_dead_shapes() {
        assert_eq!(roots_entry_action(None, None), RootsEntryAction::Remove);
        assert_eq!(roots_entry_action(None, Some(Some(9))), RootsEntryAction::Remove);
    }

    #[test]
    fn roots_entry_action_updates_live_shapes() {
        assert_eq!(roots_entry_action(Some(7), None), RootsEntryAction::UpdateShape { shape: 7 });
        assert_eq!(
            roots_entry_action(Some(7), Some(Some(9))),
            RootsEntryAction::UpdateShapeAndPrototype { shape: 7, prototype: 9 }
        );
    }

    #[test]
    fn roots_entry_action_refuses_a_dead_prototype() {
        assert_eq!(roots_entry_action(Some(7), Some(None)), RootsEntryAction::DeadPrototype);
    }

    #[test]
    fn map_entry_action_removes_dead_keys_and_dead_pointer_values() {
        assert_eq!(map_entry_action(None, false, None), MapEntryAction::Remove);
        assert_eq!(map_entry_action(None, true, Some(9)), MapEntryAction::Remove);
        assert_eq!(map_entry_action(Some(7), true, None), MapEntryAction::Remove);
    }

    #[test]
    fn map_entry_action_keeps_live_entries() {
        assert_eq!(map_entry_action(Some(7), false, None), MapEntryAction::UpdateKey { key: 7 });
        assert_eq!(map_entry_action(Some(7), false, Some(9)), MapEntryAction::UpdateKey { key: 7 });
        assert_eq!(
            map_entry_action(Some(7), true, Some(9)),
            MapEntryAction::UpdateKeyAndValue { key: 7, value: 9 }
        );
    }

    #[test]
    fn cell_action_deletes_a_cell_with_a_dead_target() {
        assert_eq!(cell_action(None, false, None), CellAction::DeleteCell);
        assert_eq!(cell_action(None, true, Some(9)), CellAction::DeleteCell);
    }

    #[test]
    fn cell_action_updates_a_live_cell() {
        assert_eq!(cell_action(Some(7), false, None), CellAction::UpdateTarget { target: 7 });
        assert_eq!(
            cell_action(Some(7), true, Some(9)),
            CellAction::UpdateTargetAndToken { target: 7, token: 9 }
        );
        assert_eq!(
            cell_action(Some(7), true, None),
            CellAction::UpdateTargetClearToken { target: 7 }
        );
    }
}
