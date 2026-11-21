/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! LucinaNet: CSS animations of only `transform` and `opacity` run through WebRender property
//! bindings, as Chromium and Firefox run them on the compositor. Each animation tick still
//! restyles the animated element, but when nothing else changed, layout sends the new matrices
//! and opacities to `Paint` instead of building a new stacking context tree and display list.
//! A small pulsing dot on a long page cost a whole display list per frame before.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use app_units::Au;
use rustc_hash::{FxHashMap, FxHasher};
use servo_arc::Arc as ServoArc;
use style::animation::{AnimationSetKey, DocumentAnimationSet};
use style::properties::{ComputedValues, LonghandId, OwnedPropertyDeclarationId};

use crate::fragment_tree::BoxFragment;
use crate::geom::PhysicalRect;

/// Which of an element's properties are animated through bindings.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CompositorAnimated {
    pub transform: bool,
    pub opacity: bool,
}

/// The elements (and pseudo-elements) whose running animations change nothing but transforms
/// and opacity, and no transition is running on them.
pub(crate) fn compositor_animated_elements(
    animations: &DocumentAnimationSet,
    now: f64,
) -> FxHashMap<AnimationSetKey, CompositorAnimated> {
    let sets = animations.sets.read();
    let mut animated = FxHashMap::default();
    for (key, set) in sets.iter() {
        if !set.has_active_animation() || set.has_active_transition() {
            continue;
        }
        let Some(values) = set.get_value_map_for_active_animations(now) else {
            continue;
        };
        let mut which = CompositorAnimated::default();
        let only_compositor_properties = values.keys().all(|property| match property {
            OwnedPropertyDeclarationId::Longhand(
                LonghandId::Transform
                | LonghandId::Translate
                | LonghandId::Rotate
                | LonghandId::Scale,
            ) => {
                which.transform = true;
                true
            },
            OwnedPropertyDeclarationId::Longhand(LonghandId::Opacity) => {
                which.opacity = true;
                true
            },
            _ => false,
        });
        if only_compositor_properties && (which.transform || which.opacity) {
            animated.insert(key.clone(), which);
        }
    }
    animated
}

/// A WebRender property binding id for one property of one animated element: stable across
/// display lists, so that values sent between them keep applying.
pub(crate) fn binding_id(key: &AnimationSetKey, opacity: bool) -> u64 {
    let mut hasher = FxHasher::default();
    key.hash(&mut hasher);
    // A namespace of its own, away from those of pipelines (the caret's binding).
    let namespace = 0x4c4e_0000u64 | opacity as u64;
    (namespace << 32) | (hasher.finish() & 0xffff_ffff)
}

/// An element of the current display list whose transform or opacity is bound.
pub(crate) struct AnimatedBinding {
    pub key: AnimationSetKey,
    pub fragment: Arc<BoxFragment>,
    /// The containing block the fragment's reference frame was computed in.
    pub containing_block_rect: PhysicalRect<Au>,
    /// The style the display list was built with.
    pub style: ServoArc<ComputedValues>,
    pub transform: Option<u64>,
    pub opacity: Option<u64>,
}

/// Whether `new` differs from `old` in nothing but what bindings animate. Anything else changing
/// with an animation tick (a hover, a class) needs a new display list.
pub(crate) fn differs_only_in_compositor_properties(
    old: &ComputedValues,
    new: &ComputedValues,
) -> bool {
    macro_rules! same {
        ($($get:ident),*) => {
            $((std::ptr::eq(old.$get(), new.$get()) || old.$get() == new.$get()))&&*
        };
    }
    if !same!(
        get_background,
        get_border,
        get_column,
        get_counters,
        get_font,
        get_inherited_box,
        get_inherited_svg,
        get_inherited_table,
        get_inherited_text,
        get_inherited_ui,
        get_list,
        get_margin,
        get_outline,
        get_padding,
        get_position,
        get_svg,
        get_table,
        get_text,
        get_ui
    ) {
        return false;
    }
    let mut old_box = old.get_box().clone();
    let new_box = new.get_box();
    old_box.transform = new_box.transform.clone();
    old_box.translate = new_box.translate.clone();
    old_box.rotate = new_box.rotate.clone();
    old_box.scale = new_box.scale.clone();
    let mut old_effects = old.get_effects().clone();
    old_effects.opacity = new.get_effects().opacity;
    old_box == *new_box && old_effects == *new.get_effects()
}
