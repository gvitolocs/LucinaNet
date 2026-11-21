//! The one-row top bar of docs/design/one-bar.md, as arithmetic and nothing else: [`BarLayout::new`] takes the window's
//! width, the scale factor, the tab count and the two states that move rectangles (editing, and the tabs that do not fit)
//! and answers where every piece of the bar sits. No drawing, no engine types, no toolkit: the sizes are this module's own
//! constants, so the fitting rule can be read and tested on its own before `glass.rs` and `browser.rs` move over (the later
//! pieces). `chrome.rs`'s two-row `Layout` is untouched here and nothing here calls into it.
//!
//! Points are the design's units and are what the caller passes in; everything comes back in device pixels, multiplied by
//! `scale`, as `chrome.rs` does. The band is therefore BAR_H tall in every state, 44 points times the scale, which is the
//! one number the engine's top inset has to be told (docs/design/one-bar.md acceptance 7).
//!
//! The fitting rule is deliberately restated here rather than borrowed from lucina-glass `browser_rules`: that proved unit
//! sizes the two-row chips, and the one-row rule is a different rule - a capsule that *is* the active tab, inactive tabs
//! that go compact all at once, and one chip that folds the rest. The drawer may use `browser_rules::centre` to centre the
//! host text inside the field rectangle returned here; it has no part in the fitting.

use std::cmp::Reverse;

// The design's numbers, docs/design/one-bar.md "Geometry" and "Widths", in points.
/// The band's height: 44 points, so `BAR_H` times the scale in device px, in every state, and what the engine is told.
pub const BAR_H: f32 = 44.0;
/// A capsule's height (a tab, the field, the overflow chip), centred in the band.
pub const CAPSULE_H: f32 = 30.0;
/// A capsule's top edge.
pub const CAPSULE_Y: f32 = (BAR_H - CAPSULE_H) / 2.0;
/// A capsule's corner radius.
pub const CAPSULE_R: f32 = CAPSULE_H / 2.0;
/// A button is this wide and this tall, centred in the band.
pub const BTN: f32 = 28.0;
/// A button's top edge.
pub const BTN_Y: f32 = (BAR_H - BTN) / 2.0;
/// A button's corner radius.
pub const BTN_R: f32 = 8.0;
/// The gap between neighbours, and the padding at the window's sides.
pub const GAP: f32 = 6.0;
pub const PAD: f32 = 10.0;
/// What macOS reserves for the window buttons: pass it as `left_inset` and the first control starts at that x.
pub const INSET_MACOS: f32 = 78.0;
/// The most a `left_inset` may claim; past it the inset is this (window buttons are far narrower).
pub const INSET_MAX: f32 = 400.0;
/// The field (the active tab): narrowest, the width the rule starts from, and widest when not editing.
pub const FIELD_MIN: f32 = 220.0;
pub const FIELD_PREF: f32 = 360.0;
pub const FIELD_MAX: f32 = 520.0;
/// The field while editing: grows to this and never past the star. The run it sits in stops short of the star's `+` by
/// construction, so this cap is all the rule needs.
pub const FIELD_EDIT_MAX: f32 = 640.0;
/// An inactive tab in its full form: narrows to this before it goes compact, never wider than this, title ellipsised.
pub const TAB_FULL_MIN: f32 = 96.0;
pub const TAB_FULL_MAX: f32 = 160.0;
/// An inactive tab in its compact form: a favicon circle.
pub const TAB_COMPACT: f32 = 30.0;
/// The overflow chip's width. docs/design/one-bar.md says what it shows (the folded tabs' count) and not how wide it is,
/// so this is this module's own number: two digits in a capsule.
pub const CHIP_W: f32 = 44.0;

/// What the window knows, and [`BarLayout::new`] turns into rectangles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarInput {
    /// The window's width, points.
    pub width: f32,
    /// Device px per point. Not finite, or not positive, is taken as 1.
    pub scale: f32,
    /// Points kept free at the bar's left end for the window buttons: 0 on Juno, [`INSET_MACOS`] on macOS. The first
    /// control starts at this x, or at the side padding, whichever is further right.
    pub left_inset: f32,
    /// The engine's tab count.
    pub tabs: usize,
    /// The active tab's index in the engine's list, not among the visible ones. Out of range is clamped; no tabs is no
    /// field at all.
    pub active: usize,
    /// The field holds the full URL with a caret instead of the host.
    pub editing: bool,
    /// Back and forward are always placed: these say only whether the drawer draws them live.
    pub can_back: bool,
    pub can_forward: bool,
}

/// What a pointer in the band lands on. There is no close part: a full-form tab's close button is drawn on hover inside the
/// tab's own rectangle, and whether it is there is the drawer's decision, told by [`Placed::compact`] being false.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Back,
    Forward,
    /// An inactive tab, full or compact, at this index of the engine's list.
    Tab(usize),
    /// The active tab: the field's text area, between [`Part::FieldIcon`] and [`Part::FieldReload`]. The capsule around all
    /// three is [`BarLayout::field_capsule`].
    Field,
    /// The reload, or stop while loading, icon at the field's right end, inside the capsule.
    FieldReload,
    /// The lock, warning or search icon at the field's left end, inside the capsule.
    FieldIcon,
    /// The new-tab `+`.
    NewTab,
    /// The chip holding the count of the tabs that did not fit; a click opens the tab list sheet.
    OverflowChip,
    /// The flexible empty space between the `+` and the star: the window's drag region.
    DragRegion,
    Star,
    Menu,
}

/// One placed part, device px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub part: Part,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// An inactive tab in its compact form; false for every other part.
    pub compact: bool,
}

impl Placed {
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn cx(&self) -> f32 {
        self.x + self.w / 2.0
    }
    pub fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
    /// Left and top edges in, right and bottom out: two touching boxes never both take a point. A box a narrow window
    /// squeezed to no width takes nothing.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.w > 0.0 && x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    pub fn overlaps(&self, o: &Placed) -> bool {
        self.w > 0.0 && o.w > 0.0 && self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
}

/// Which branch of the fitting rule the window landed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// Every inactive tab keeps its full form ([`TAB_FULL_MAX`], or shrunk inside its band) and the field is at its
    /// preferred width. The state when there is no inactive tab either.
    Full,
    /// Every inactive tab compact, the field at its preferred width (640 while editing).
    Compact,
    /// Every inactive tab compact and the field shrunk between [`FIELD_MIN`] and its preferred width. In a window below
    /// the bar's floor ([`BarLayout::new`], step 5) the field is under [`FIELD_MIN`] here, and nowhere else.
    FieldShrunk,
    /// Compact tabs, the field at [`FIELD_MIN`] or - only in that same degenerate window - what the room leaves it, and the
    /// tabs furthest from the active one folded away.
    Folded,
}

/// Where the bar's pieces sit. A pure value: the same [`BarInput`] always gives the same `BarLayout`, and nothing here
/// depends on what a page, a title or a favicon says.
#[derive(Clone, Debug, PartialEq)]
pub struct BarLayout {
    /// The band's height, device px: 44 points times the scale, in every state.
    pub band_h: f32,
    /// Every placed part, left to right.
    pub parts: Vec<Placed>,
    /// The inactive tabs that are not placed, ascending - the chip's count is this list's length, and the drawer shows them
    /// in the tab list sheet. Empty whenever the bar holds every tab; the only non-empty case without a chip is a window
    /// so narrow that the chip's own width was given up for the field's ([`BarLayout::new`], step 4).
    pub overflow: Vec<usize>,
    /// The field's capsule: its left edge and width, device px. The drawer paints the capsule here and hangs the
    /// suggestions sheet off its bottom edge at the same width; [`Part::Field`] is the text strip inside it.
    pub field_x: f32,
    pub field_w: f32,
    /// A capsule's top edge, device px, so the drawer does not re-derive it.
    pub capsule_y: f32,
    pub fit: Fit,
    pub scale: f32,
    /// The window's width, device px: the right edge nothing crosses.
    pub width_px: f32,
    /// Carried through from the input: the geometry does not depend on them, the drawing does.
    pub can_back: bool,
    pub can_forward: bool,
}

/// Derived widths land on a 1/1024-point grid, rounded down. A width out of a division would otherwise come back a float
/// hair larger than the room it was made from, and a hair is enough for the next control to overlap it.
fn grid(v: f32) -> f32 {
    if v <= 0.0 {
        0.0
    } else {
        (v * 1024.0).floor() / 1024.0
    }
}

/// The room the tab run gets, and the right-pinned controls' positions, for one set of spacings. Everything in points.
struct Frame {
    /// The run's first item's x.
    run_start: f32,
    /// Where the run may end: the `+` and the gaps around it still fit before the star.
    run_limit: f32,
    plus_w: f32,
    star_x: f32,
    star_w: f32,
    menu_x: f32,
    menu_w: f32,
}

impl Frame {
    /// The star and the menu are pinned from the window's right edge and everything else packs from the left, which is
    /// what leaves the middle to the drag region. A control that has been given up is here with width 0, so the space it
    /// and its gap took comes back to what is left.
    fn of(width: f32, x0: f32, gap: f32, sm: f32, pad: f32, shed_plus: bool, shed_star: bool, shed_menu: bool) -> Frame {
        let plus_w = if shed_plus { 0.0 } else { BTN };
        let star_w = if shed_star { 0.0 } else { BTN };
        let menu_w = if shed_menu { 0.0 } else { BTN };
        let menu_x = width - pad - menu_w;
        let star_x = menu_x - sm - star_w;
        Frame {
            run_start: x0 + BTN + gap + BTN + gap,
            run_limit: star_x - gap - plus_w - gap,
            plus_w,
            star_x,
            star_w,
            menu_x,
            menu_w,
        }
    }

    /// The width the run has to itself.
    fn room(&self) -> f32 {
        (self.run_limit - self.run_start).max(0.0)
    }
}

/// What the fitting rule asks the run for.
struct Plan {
    fit: Fit,
    /// An inactive tab's width.
    tab: f32,
    /// The field's capsule width.
    field: f32,
    /// The inactive tabs that stay in the run, the rest going into the chip.
    visible: usize,
    /// Whether the chip stands at the run's end.
    chip: bool,
    /// What the run occupies, its own gaps included.
    total: f32,
    /// The run's item count: how much a point off `gap` buys back.
    items: usize,
}

/// The rule of docs/design/one-bar.md, in order: the field at its preferred width (640 while editing) and every inactive
/// tab full while they all fit; all of them compact when the full ones do not; the field shrinking toward its minimum when
/// the compact ones still do not; and only then the tabs furthest from the active one folded into one chip.
///
/// `floor` is what the field may not go under - [`FIELD_MIN`], except in the degenerate squeeze of [`BarLayout::new`], where
/// it is the room that is left. `chip_allowed` is false when that squeeze has given up the chip too. [`Frame::room`] is never
/// negative, so a window with no room at all still asks here for a full [`FIELD_MIN`]; what it gets back is clipped whole.
fn fit_run(room: f32, gap: f32, n_in: usize, editing: bool, chip_allowed: bool, floor: f32) -> Plan {
    let target = if editing { FIELD_EDIT_MAX } else { FIELD_PREF };
    let n = n_in as f32;
    let hi = floor.max(target);

    // Full form: at the top of the band if everything fits there, else shrunk inside the band, before any tab is made
    // compact. While editing the design puts every inactive tab compact regardless of space, so both steps are skipped.
    if !editing && n_in > 0 {
        let full = |w: f32| Plan { fit: Fit::Full, tab: w, field: target, visible: n_in, chip: false, total: n * (w + gap) + target, items: n_in + 1 };
        if n * TAB_FULL_MAX + target + n * gap <= room {
            return full(TAB_FULL_MAX);
        }
        let w = grid((room - target - n * gap) / n);
        if w >= TAB_FULL_MIN {
            return full(w);
        }
    }

    let compact = |f: f32, fit: Fit| Plan { fit, tab: TAB_COMPACT, field: f, visible: n_in, chip: false, total: n * (TAB_COMPACT + gap) + f, items: n_in + 1 };
    if n * TAB_COMPACT + target + n * gap <= room {
        // Nothing to make compact is still the roomy state.
        return compact(target, if n_in == 0 { Fit::Full } else { Fit::Compact });
    }
    // The raw difference, not [`grid`]'s clamped one: what the compact tabs leave is negative when there is nothing left,
    // and a 0 read as "the field still fits in it" would place every tab in a window that holds none of them.
    let shrunk = room - n * (TAB_COMPACT + gap);
    if shrunk >= floor {
        return compact(grid(shrunk), Fit::FieldShrunk);
    }

    // Fold. The chip costs its width and a gap and holds the tabs furthest from the active one; what the tabs leave grows
    // the field back up from its floor toward its target.
    let per = TAB_COMPACT + gap;
    let chip_slot = if chip_allowed { CHIP_W + gap } else { 0.0 };
    let base = floor + chip_slot;
    let visible = if room >= base { ((room - base) / per).floor() as usize } else { 0 }.min(n_in);
    let folded = n_in - visible;
    let chip = folded > 0 && chip_allowed;
    let tabs_w = visible as f32 * per + if chip { CHIP_W + gap } else { 0.0 };
    let field = grid((room - tabs_w).clamp(floor, hi)).max(floor);
    Plan { fit: if folded > 0 { Fit::Folded } else { Fit::FieldShrunk }, tab: TAB_COMPACT, field, visible, chip, total: tabs_w + field, items: visible + 1 + usize::from(chip) }
}

/// The inactive tabs that go into the chip, ascending: the `visible` nearest the active tab stay, so those that go are the
/// furthest from it. Two tabs at the same distance - one each side of the active one - and the higher index goes first.
fn folded(tabs: usize, active: usize, visible: usize) -> Vec<usize> {
    let dist = |t: usize| if t > active { t - active } else { active - t };
    let mut by_distance: Vec<(usize, usize)> = (0..tabs).filter(|t| *t != active).map(|t| (dist(t), t)).collect();
    by_distance.sort_by_key(|(d, t)| Reverse((*d, *t)));
    // The worst-first list keeps its `visible` last entries, which are the nearest the active tab; the head is what goes.
    let keep = visible.min(by_distance.len());
    let mut gone: Vec<usize> = by_distance[..by_distance.len() - keep].iter().map(|(_, t)| *t).collect();
    gone.sort_unstable();
    gone
}

/// A part's box, in points: capsules and buttons differ only in y and h.
fn at(part: Part, x: f32, w: f32, capsule: bool, compact: bool) -> Placed {
    let (y, h) = if capsule { (CAPSULE_Y, CAPSULE_H) } else { (BTN_Y, BTN) };
    Placed { part, x, y, w, h, compact }
}

/// Nothing crosses the window's right edge and nothing sits on its neighbour. The list is in left to right order, so a part
/// that cannot start before the edge is put at the edge with no width. In every window wide enough for the rule this is a
/// no-op; it runs again in device px because scaling a sum is not scaling its parts and adding them back, and a float hair
/// of slack between two boxes is enough for a point to sit in both.
fn pack(parts: &mut [Placed], width: f32) {
    let mut lo = 0.0f32;
    for b in parts {
        b.x = b.x.max(lo).clamp(0.0, width);
        b.w = b.w.min(width - b.x).max(0.0);
        lo = b.x + b.w;
    }
}

impl BarLayout {
    /// The bar for this window, tab count and state. The parts come back in the bar's order: back, forward, the tabs in
    /// tab order with the field in the active tab's slot, the overflow chip when it stands, the `+`, the drag region, the
    /// star, the menu.
    ///
    /// **A window narrower than the bar's minimum.** The field never goes below [`FIELD_MIN`] and is never clipped or
    /// pushed out of view; the room is bought instead, in this order, each step exhausted before the next takes over:
    /// 1. the drag region - it is the flexible space, so this is no degradation at all;
    /// 2. the gaps between the controls, all of them together, down to nothing;
    /// 3. last of the spacing, the star/menu pair: their own gap, then the padding after the menu;
    /// 4. the widths of the controls right of the field, least useful first - the menu, the star, the `+`, then the
    ///    overflow chip - each given width 0, still in the list, still in order, taking no click. The chip is last of them
    ///    because it is what makes the folded tabs reachable: when it too is gone, [`BarLayout::overflow`] still names the
    ///    tabs that did not fit, and nothing in the bar shows them;
    /// 5. the field itself, below [`FIELD_MIN`], by only as much as the window leaves it: still whole, never clipped.
    ///
    /// Back and forward are never given up. The floor before step 5 is the row's start, two buttons and [`FIELD_MIN`]: 286
    /// points on Juno, 354 with the macOS inset. Below it the field is what shrinks, and it is the only thing that ever is.
    pub fn new(i: &BarInput) -> BarLayout {
        let width = if i.width.is_finite() { i.width.max(0.0) } else { 0.0 };
        let scale = if i.scale.is_finite() && i.scale > 0.0 { i.scale } else { 1.0 };
        let inset = if i.left_inset.is_finite() { i.left_inset.clamp(0.0, INSET_MAX) } else { 0.0 };
        let x0 = inset.max(PAD).min(width);
        let has_field = i.tabs > 0;
        let active = if has_field { i.active.min(i.tabs - 1) } else { 0 };
        let n_in = if has_field { i.tabs - 1 } else { 0 };

        // The spacings and the gives-up, solved until the run's content fits the room. The nominal values are the design's;
        // every step below is one of the five above, in order.
        let mut gap = GAP;
        let mut sm = GAP;
        let mut pad = PAD;
        let mut shed_menu = false;
        let mut shed_star = false;
        let mut shed_plus = false;
        let mut shed_chip = false;
        let mut squeeze = false;
        let mut f = Frame::of(width, x0, gap, sm, pad, shed_plus, shed_star, shed_menu);
        let mut p = fit_run(f.room(), gap, n_in, i.editing, !shed_chip, if squeeze { f.room() } else { FIELD_MIN });
        // At most one pass per step: three of spacing, four of widths, one of the squeeze.
        for _ in 0..12 {
            if !has_field || p.total <= f.room() {
                break;
            }
            let deficit = p.total - f.room();
            // A point off the gap buys back four (two before the run, two after it) and one per gap inside the run.
            let buys = 4.0 + p.items.saturating_sub(1) as f32;
            if gap > 0.0 {
                gap = (gap - deficit / buys).max(0.0);
            } else if sm > 0.0 {
                sm = (sm - deficit).max(0.0);
            } else if pad > 0.0 {
                pad = (pad - deficit).max(0.0);
            } else if !squeeze {
                if !shed_menu {
                    shed_menu = true;
                } else if !shed_star {
                    shed_star = true;
                } else if !shed_plus {
                    shed_plus = true;
                } else {
                    // The chip is the last width, and with it goes the field's minimum: no room is left to buy anywhere
                    // else, and the field stays whole rather than clipped.
                    shed_chip = true;
                    squeeze = true;
                }
            } else {
                break;
            }
            f = Frame::of(width, x0, gap, sm, pad, shed_plus, shed_star, shed_menu);
            p = fit_run(f.room(), gap, n_in, i.editing, !shed_chip, if squeeze { f.room() } else { FIELD_MIN });
        }

        // Place, left to right, in points.
        let mut out: Vec<Placed> = Vec::new();
        let mut cur = x0;
        out.push(at(Part::Back, cur, BTN, false, false));
        cur += BTN + gap;
        out.push(at(Part::Forward, cur, BTN, false, false));
        cur += BTN + gap;
        let gone = folded(i.tabs, active, p.visible);
        // The tabs before the field are the inactive ones left of the active tab that were not folded away.
        let before = (0..active).filter(|t| gone.binary_search(t).is_err()).count() as f32;
        let field_x_slot = if has_field { cur + before * (p.tab + gap) } else { 0.0 };
        if has_field {
            for t in 0..i.tabs {
                if t == active {
                    // The capsule tiles into its icon, its text and its reload, so no part is inside another and no point
                    // of it is dead beyond the 1-point corner of an icon against the capsule's rounded end.
                    let w = p.field;
                    if w >= 2.0 * BTN {
                        out.push(at(Part::FieldIcon, cur, BTN, false, false));
                        out.push(at(Part::Field, cur + BTN, w - 2.0 * BTN, true, false));
                        out.push(at(Part::FieldReload, cur + w - BTN, BTN, false, false));
                    } else {
                        out.push(at(Part::Field, cur, w, true, false));
                    }
                    cur += w + gap;
                } else if gone.binary_search(&t).is_err() {
                    out.push(at(Part::Tab(t), cur, p.tab, true, p.tab == TAB_COMPACT));
                    cur += p.tab + gap;
                }
            }
            if p.chip {
                out.push(at(Part::OverflowChip, cur, CHIP_W, true, false));
                cur += CHIP_W + gap;
            }
        }
        out.push(at(Part::NewTab, cur, f.plus_w, false, false));
        cur += f.plus_w + gap;
        let drag_w = (f.star_x - gap - cur).max(0.0);
        // The drag region is the band's whole height: a window drag starts anywhere in the bar's empty space.
        out.push(Placed { part: Part::DragRegion, x: cur, y: 0.0, w: drag_w, h: BAR_H, compact: false });
        out.push(at(Part::Star, f.star_x, f.star_w, false, false));
        out.push(at(Part::Menu, f.menu_x, f.menu_w, false, false));

        pack(&mut out, width);
        let field_x = field_x_slot.clamp(0.0, width);
        let field_w = if has_field { p.field.min(width - field_x).max(0.0) } else { 0.0 };

        for b in &mut out {
            b.x *= scale;
            b.y *= scale;
            b.w *= scale;
            b.h *= scale;
        }
        pack(&mut out, width * scale);
        let fx = field_x * scale;
        let fw = (field_w * scale).min((width * scale - fx).max(0.0));
        BarLayout {
            band_h: BAR_H * scale,
            parts: out,
            overflow: gone,
            field_x: fx,
            field_w: fw,
            capsule_y: CAPSULE_Y * scale,
            fit: if has_field { p.fit } else { Fit::Full },
            scale,
            width_px: width * scale,
            can_back: i.can_back,
            can_forward: i.can_forward,
        }
    }

    /// The part under (`x`, `y`) device px, or nothing: outside the band, in the padding, or on a part a narrow window
    /// squeezed to no width.
    pub fn part_at(&self, x: f32, y: f32) -> Option<Part> {
        self.parts.iter().find(|b| b.contains(x, y)).map(|b| b.part)
    }

    /// The box a part sits in.
    pub fn placed(&self, part: Part) -> Option<Placed> {
        self.parts.iter().copied().find(|b| b.part == part)
    }

    /// The field's capsule, which is not itself a part: the drawer paints it and hangs the suggestions sheet off it.
    pub fn field_capsule(&self) -> Option<Placed> {
        if self.field_w <= 0.0 {
            return None;
        }
        Some(Placed { part: Part::Field, x: self.field_x, y: self.capsule_y, w: self.field_w, h: CAPSULE_H * self.scale, compact: false })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn juno(width: f32, scale: f32, tabs: usize, active: usize, editing: bool) -> BarInput {
        BarInput { width, scale, left_inset: 0.0, tabs, active, editing, can_back: true, can_forward: true }
    }

    fn mac(width: f32, scale: f32, tabs: usize, active: usize, editing: bool) -> BarInput {
        BarInput { left_inset: INSET_MACOS, ..juno(width, scale, tabs, active, editing) }
    }

    fn put(l: &BarLayout, part: Part) -> Placed {
        l.placed(part).unwrap_or_else(|| panic!("no {part:?} in the bar"))
    }

    /// The bar's order, with the three parts inside the field's capsule folded back into the one slot they share.
    fn spine(l: &BarLayout) -> Vec<Part> {
        l.parts.iter().map(|b| b.part).filter(|p| !matches!(p, Part::FieldIcon | Part::FieldReload)).collect()
    }

    /// Every invariant that has to hold whatever the window, the tab count or the state is.
    fn holds(l: &BarLayout, i: &BarInput, ctx: &str) {
        let s = if i.scale.is_finite() && i.scale > 0.0 { i.scale } else { 1.0 };
        let w = if i.width.is_finite() { i.width.max(0.0) } else { 0.0 };
        assert_eq!(l.band_h, BAR_H * s, "{ctx}: the band is 44 points, in every state");
        assert_eq!(l.width_px, w * s, "{ctx}");

        for (a, x) in l.parts.iter().enumerate() {
            for y in &l.parts[a + 1..] {
                assert!(!x.overlaps(y), "{ctx}: {x:?} overlaps {y:?}");
            }
        }
        for b in &l.parts {
            assert!(b.x >= 0.0 && b.right() <= l.width_px, "{ctx}: {b:?} is out of [0, {}]", l.width_px);
            assert!(b.y >= 0.0 && b.bottom() <= l.band_h, "{ctx}: {b:?} is out of the band");
        }
        for p in l.parts.windows(2) {
            assert!(p[1].x >= p[0].right(), "{ctx}: {:?} starts before {:?} ends", p[1], p[0]);
        }

        let sp = spine(l);
        assert_eq!(&sp[..2], [Part::Back, Part::Forward], "{ctx}: the pair is first");
        assert_eq!(&sp[sp.len() - 3..], [Part::DragRegion, Part::Star, Part::Menu], "{ctx}: drag region, star, menu last");
        assert_eq!(sp[sp.len() - 4], Part::NewTab, "{ctx}: the + is the drag region's left neighbour");

        let run = &sp[2..sp.len() - 4];
        // The tabs, in tab order, with the field in the active tab's own slot: the indices they name ascend.
        let named: Vec<usize> = run
            .iter()
            .filter(|p| **p != Part::OverflowChip)
            .map(|p| match p {
                Part::Tab(t) => *t,
                Part::Field => i.active.min(i.tabs.saturating_sub(1)),
                other => panic!("{ctx}: {other:?} does not belong in the tab run"),
            })
            .collect();
        for w2 in named.windows(2) {
            assert!(w2[1] > w2[0], "{ctx}: the run is out of tab order: {named:?}");
        }
        let chips: Vec<Part> = run.iter().filter(|p| **p == Part::OverflowChip).copied().collect();
        assert!(chips.len() <= 1, "{ctx}: more than one overflow chip");
        // The chip stands exactly when there is something to fold, until its own width is one of the losses the narrow
        // window forces; from there the folded tabs have no chip, and the ladder says nothing else could have gone first.
        let shed = l.parts.iter().any(|b| b.w == 0.0);
        if l.overflow.is_empty() {
            assert!(chips.is_empty(), "{ctx}: a chip with nothing to fold");
        } else if !shed {
            assert_eq!(chips.len(), 1, "{ctx}: the chip stands exactly when there is something to fold");
        }
        if i.tabs == 0 {
            assert!(run.is_empty(), "{ctx}: no tabs, no run");
            assert!(l.overflow.is_empty() && l.field_w == 0.0, "{ctx}: no field either");
        } else {
            assert_eq!(named.len(), run.len() - chips.len(), "{ctx}: the field is missing from the run");
            // Every inactive tab is either placed or in the chip, and the chip's count is exactly the list it names.
            let placed = run.iter().filter(|p| matches!(p, Part::Tab(_))).count();
            assert_eq!(placed + l.overflow.len(), i.tabs - 1, "{ctx}: tabs lost between the run and the chip");
            assert_eq!(&l.overflow, &{
                let mut v = l.overflow.clone();
                v.sort_unstable();
                v
            }, "{ctx}: the folded indices are listed ascending");
            for t in l.overflow.windows(2) {
                assert!(t[1] > t[0], "{ctx}: repeated folded index");
            }
            // What is left is nearer the active tab than anything folded: the furthest go first.
            let dist = |t: usize| if t > i.active { t - i.active } else { i.active - t };
            for v in run.iter().filter_map(|p| if let Part::Tab(t) = p { Some(*t) } else { None }) {
                for g in &l.overflow {
                    assert!(dist(v) <= dist(*g), "{ctx}: tab {v} at distance {} stayed while {g} at {} folded", dist(v), dist(*g));
                }
            }
        }
        assert_eq!(*l, BarLayout::new(i), "{ctx}: the same input twice is the same bar");
    }

    /// The nominal bar: the design's spacing everywhere, every control alive, the field inside its band.
    fn nominal(l: &BarLayout, i: &BarInput, ctx: &str) {
        let s = i.scale;
        assert_eq!(put(l, Part::Back).x, i.left_inset.max(PAD) * s, "{ctx}: the first control is at the inset");
        assert_eq!(put(l, Part::Menu).right(), l.width_px - PAD * s, "{ctx}: the padding after the menu");
        for p in [Part::Back, Part::Forward, Part::NewTab, Part::Star, Part::Menu] {
            assert_eq!(put(l, p).w, BTN * s, "{ctx}: {p:?} was given up in a window that has room");
        }
        assert_eq!(put(l, Part::Forward).x - put(l, Part::Back).right(), GAP * s, "{ctx}: the first gap");
        assert_eq!(put(l, Part::Menu).x - put(l, Part::Star).right(), GAP * s, "{ctx}: the star/menu gap");
        let hi = if i.editing { FIELD_EDIT_MAX } else { FIELD_MAX };
        assert!(l.field_w >= FIELD_MIN * s, "{ctx}: field {} is under its minimum", l.field_w);
        assert!(l.field_w <= hi * s, "{ctx}: field {} is over its maximum", l.field_w);
        assert!(l.field_x + l.field_w <= l.width_px - PAD * s, "{ctx}: the field ends short of the window's own padding");
        assert!(l.field_x >= i.left_inset.max(PAD) * s, "{ctx}: a field sits right of the window buttons");
        if i.tabs == 0 {
            assert_eq!((l.field_x, l.field_w), (0.0, 0.0), "{ctx}: a bar with no tab has no field to hang a sheet on");
        }
        let star = put(l, Part::Star);
        assert!(l.field_x + l.field_w <= star.x, "{ctx}: the field reaches the star");
    }

    #[test]
    fn the_design_numbers_are_the_geometry() {
        assert_eq!((BAR_H, CAPSULE_H, CAPSULE_Y, CAPSULE_R), (44.0, 30.0, 7.0, 15.0));
        assert_eq!((BTN, BTN_Y, BTN_R), (28.0, 8.0, 8.0));
        assert_eq!((GAP, PAD, INSET_MACOS), (6.0, 10.0, 78.0));
        assert_eq!((FIELD_MIN, FIELD_PREF, FIELD_MAX, FIELD_EDIT_MAX), (220.0, 360.0, 520.0, 640.0));
        assert_eq!((TAB_FULL_MIN, TAB_FULL_MAX, TAB_COMPACT), (96.0, 160.0, 30.0));
        for scale in [1.0f32, 2.0] {
            let l = BarLayout::new(&juno(1100.0, scale, 3, 1, false));
            let t = put(&l, Part::Tab(0));
            assert_eq!((t.y, t.h), (CAPSULE_Y * scale, CAPSULE_H * scale), "a capsule is 30 tall");
            assert_eq!(t.w, TAB_FULL_MAX * scale, "and here, in a window with room, 160 wide");
            assert_eq!(t.cy(), l.band_h / 2.0, "centred in the band");
            let b = put(&l, Part::Back);
            assert_eq!((b.w, b.h), (BTN * scale, BTN * scale), "a button is 28 by 28");
            assert_eq!(b.cy(), l.band_h / 2.0);
            // The reload and the lock are inside the field's capsule, at its ends.
            let c = l.field_capsule().unwrap();
            let icon = put(&l, Part::FieldIcon);
            let reload = put(&l, Part::FieldReload);
            assert_eq!(icon.x, c.x);
            assert_eq!(reload.right(), c.right());
            assert_eq!(reload.x, put(&l, Part::Field).right());
            assert_eq!(icon.right(), put(&l, Part::Field).x);
            assert_eq!(l.placed(Part::Tab(1)), None, "the active tab is the field, never also a Tab part");
        }
    }

    #[test]
    fn every_window_in_the_matrix_is_one_valid_bar() {
        for tabs in [1usize, 3, 12, 40] {
            for width in [700.0f32, 1100.0, 1920.0] {
                for scale in [1.0f32, 2.0] {
                    for active in [0usize, tabs / 2, tabs - 1] {
                        for editing in [false, true] {
                            for inset in [0.0f32, INSET_MACOS] {
                                let i = BarInput { width, scale, left_inset: inset, tabs, active, editing, can_back: true, can_forward: true };
                                let ctx = format!("n{tabs} w{width} s{scale} a{active} editing{editing} inset{inset}");
                                let l = BarLayout::new(&i);
                                holds(&l, &i, &ctx);
                                nominal(&l, &i, &ctx);
                                let chip = l.placed(Part::OverflowChip);
                                if let Some(c) = chip {
                                    assert_eq!(c.w, CHIP_W * scale, "{ctx}: the chip's width");
                                    assert_eq!(c.h, CAPSULE_H * scale, "{ctx}: the chip is a capsule");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_fitting_rule_step_by_step() {
        // Room the run gets, nominal: width less the padding, the star/menu pair, the +, the inset pair and their gaps.
        // At 1920 Juno that is 1730 points, at 1100 it is 910, at 700 it is 510.
        let l = BarLayout::new(&juno(1920.0, 1.0, 1, 0, false));
        assert_eq!(l.fit, Fit::Full);
        assert_eq!(l.field_w, FIELD_PREF);
        assert!(l.overflow.is_empty());
        let drag = put(&l, Part::DragRegion);
        assert!(drag.w > 1000.0, "the room the capsules do not need is the drag region's: {}", drag.w);

        // Three tabs: two full ones at their widest, and the field at its preferred width.
        let l = BarLayout::new(&juno(1100.0, 1.0, 3, 1, false));
        assert_eq!(l.fit, Fit::Full);
        assert_eq!(put(&l, Part::Tab(0)).w, TAB_FULL_MAX);
        assert_eq!(put(&l, Part::Tab(2)).w, TAB_FULL_MAX);
        assert_eq!(l.field_w, FIELD_PREF);
        // The run starts after the padding and the back/forward pair with their gaps: 78 points on Juno.
        assert_eq!(l.field_x, PAD + 2.0 * (BTN + GAP) + TAB_FULL_MAX + GAP, "the field is in the middle tab's slot");

        // The same three in 700: the full pair and 360 of field no longer fit, so both go compact, all at once.
        let l = BarLayout::new(&juno(700.0, 1.0, 3, 1, false));
        assert_eq!(l.fit, Fit::Compact);
        assert_eq!(put(&l, Part::Tab(0)).w, TAB_COMPACT);
        assert_eq!(put(&l, Part::Tab(2)).w, TAB_COMPACT);
        assert!(put(&l, Part::Tab(0)).compact && put(&l, Part::Tab(2)).compact);
        assert_eq!(l.field_w, FIELD_PREF, "the field keeps its preferred width before any tab is folded");

        // Twelve tabs at 1920: full form still, shrunk inside its band rather than compact.
        let l = BarLayout::new(&juno(1920.0, 1.0, 12, 5, false));
        assert_eq!(l.fit, Fit::Full);
        for t in 0..12usize {
            if t != 5 {
                let b = put(&l, Part::Tab(t));
                assert!(b.w >= TAB_FULL_MIN && b.w <= TAB_FULL_MAX, "tab {t} is {} points wide", b.w);
                assert!(!b.compact, "tab {t} keeps its title");
            }
        }
        assert_eq!(l.overflow.len(), 0);

        // Forty at 1920: compact, and the field shrunk between its minimum and its preferred width. No chip: they fit.
        let l = BarLayout::new(&juno(1920.0, 1.0, 40, 39, false));
        assert_eq!(l.fit, Fit::FieldShrunk);
        assert_eq!(l.field_w, 326.0);
        assert_eq!(l.overflow.len(), 0);
        assert_eq!(l.placed(Part::OverflowChip), None);
        assert_eq!(l.parts.iter().filter(|b| matches!(b.part, Part::Tab(_))).count(), 39);

        // Twelve in 700: compact tabs and the field at its minimum still do not fit, so the furthest fold.
        // Room 510, the chip 44 and its gap, the field 220: six compact tabs fit, five of eleven fold.
        let l = BarLayout::new(&juno(700.0, 1.0, 12, 5, false));
        assert_eq!(l.fit, Fit::Folded);
        assert_eq!(l.overflow, vec![0, 1, 9, 10, 11], "the tabs furthest from the active one, and the higher index first");
        assert_eq!(l.placed(Part::OverflowChip).unwrap().w, CHIP_W);
        assert_eq!(l.field_w, 244.0, "220 plus what the fold left, and never less than the minimum");
        assert_eq!(l.parts.iter().filter(|b| matches!(b.part, Part::Tab(_))).count(), 6);
        for t in [2usize, 3, 4, 6, 7, 8] {
            assert!(put(&l, Part::Tab(t)).compact, "{t} stayed, compact");
        }
        // The chip sits at the run's end, before the +.
        assert!(put(&l, Part::OverflowChip).x > put(&l, Part::Tab(8)).right());
        assert!(put(&l, Part::OverflowChip).right() < put(&l, Part::NewTab).x);

        // Forty in 700: the active tab at the end of the list folds all of the other end.
        let l = BarLayout::new(&juno(700.0, 1.0, 40, 39, false));
        assert_eq!(l.overflow.len(), 33);
        assert_eq!(l.overflow[0], 0);
        assert_eq!(l.overflow[32], 32);
        assert_eq!(put(&l, Part::Tab(33)).w, TAB_COMPACT);

        // The run holds only the tabs that fit, every one of them with room of its own, and the spacing gives up in the
        // ladder's order: the run's gaps first, the star/menu pair's only after them, the padding after that.
        for tabs in [3usize, 8, 20, 40] {
            for width in [700.0f32, 512.0, 400.0, 320.0] {
                let i = juno(width, 1.0, tabs, tabs / 2, false);
                let l = BarLayout::new(&i);
                let ctx = format!("{width} with {tabs} tabs");
                holds(&l, &i, &ctx);
                let sp = spine(&l);
                for b in &sp[2..sp.len() - 4] {
                    assert!(put(&l, *b).w > 0.0, "{ctx}: the run shows all of what it holds: {b:?}");
                }
                assert!(put(&l, Part::Field).w > 0.0, "{ctx}: the field's text strip has room of its own");
                let gap = put(&l, Part::Forward).x - put(&l, Part::Back).right();
                let sm = put(&l, Part::Menu).x - put(&l, Part::Star).right();
                assert!(gap <= GAP && sm <= GAP, "{ctx}: nothing is spaced wider than the design: {gap} {sm}");
                if gap == GAP {
                    // One gap, everywhere, while the ladder is idle on it - and the pair stands at it too. The field is
                    // one capsule however its own strips are cut up.
                    let mut caps: Vec<(f32, f32)> = l
                        .parts
                        .iter()
                        .filter_map(|b| match b.part {
                            Part::Tab(_) | Part::OverflowChip => Some((b.x, b.right())),
                            Part::FieldIcon | Part::Field => Some((l.field_x, l.field_x + l.field_w)),
                            _ => None,
                        })
                        .collect();
                    caps.dedup();
                    for c in caps.windows(2) {
                        assert_eq!(c[1].0, c[0].1 + GAP, "{ctx}: the run's spacing {c:?}");
                    }
                    assert_eq!(sm, GAP, "{ctx}: the star and the menu are the last apart");
                }
                assert!(gap == 0.0 || sm == GAP, "{ctx}: the pair's spacing survives every point the run's does: {gap} {sm}");
            }
        }
    }

    #[test]
    fn a_window_far_wider_than_the_run_gives_everything_its_nominal_width() {
        // Forty full tabs want 6 574 points, the widest Juno phone of them all; 14 000 is past that, so the rule stops at
        // the design's widths and nothing is graded, clipped or re-spaced.
        for tabs in [1usize, 3, 12, 40] {
            for scale in [1.0f32, 2.0] {
                for editing in [false, true] {
                    for inset in [0.0f32, INSET_MACOS] {
                        let i = BarInput { width: 14_000.0, scale, left_inset: inset, tabs, active: tabs / 2, editing, can_back: true, can_forward: true };
                        let ctx = format!("14000 n{tabs} s{scale} editing{editing} inset{inset}");
                        let l = BarLayout::new(&i);
                        holds(&l, &i, &ctx);
                        nominal(&l, &i, &ctx);
                        let want = if editing { TAB_COMPACT } else { TAB_FULL_MAX };
                        for b in l.parts.iter().filter(|b| matches!(b.part, Part::Tab(_) | Part::OverflowChip)) {
                            assert_eq!(b.w, want * scale, "{ctx}: {b:?}");
                            assert_eq!(b.h, CAPSULE_H * scale, "{ctx}: {b:?}");
                            assert_eq!(b.compact, editing, "{ctx}: {b:?}");
                        }
                        assert_eq!(l.field_w, if editing { FIELD_EDIT_MAX } else { FIELD_PREF } * scale, "{ctx}");
                        for c in [l.field_capsule().unwrap()] {
                            assert_eq!(c.h, CAPSULE_H * scale, "{ctx}: the field's capsule");
                        }
                        assert_eq!(put(&l, Part::Field).x, put(&l, Part::FieldIcon).right(), "{ctx}: the field's strips tile");
                        assert_eq!(put(&l, Part::FieldReload).x, put(&l, Part::Field).right(), "{ctx}: all three of them");
                        assert_eq!(put(&l, Part::FieldIcon).y, BTN_Y * scale, "{ctx}: the icons are buttons, not capsules");
                        assert_eq!(l.capsule_y, CAPSULE_Y * scale, "{ctx}");
                        assert_eq!(put(&l, Part::FieldIcon).x, l.field_x, "{ctx}: the capsule starts at its icon");
                    }
                }
            }
        }
    }

    #[test]
    fn editing_puts_every_inactive_tab_compact_and_grows_the_field() {
        // Regardless of space: 1920 has room enough for full tabs, and still they are compact.
        for width in [1920.0f32, 1100.0, 700.0] {
            let l = BarLayout::new(&juno(width, 1.0, 3, 1, true));
            for t in [0usize, 2] {
                let b = put(&l, Part::Tab(t));
                assert_eq!(b.w, TAB_COMPACT, "editing at {width}");
                assert!(b.compact, "editing at {width}");
            }
            assert!(l.field_w > FIELD_PREF, "editing at {width}: the field grew to {}", l.field_w);
            assert!(l.field_w <= FIELD_EDIT_MAX, "editing at {width}");
            assert!(l.field_x + l.field_w < put(&l, Part::Star).x, "editing at {width}: never past the star");
        }
        // Room for the whole 640.
        let l = BarLayout::new(&juno(1920.0, 1.0, 3, 1, true));
        assert_eq!(l.fit, Fit::Compact);
        assert_eq!(l.field_w, FIELD_EDIT_MAX);
        // Without room it shrinks toward its minimum rather than losing the star, and still no tab is folded.
        let l = BarLayout::new(&juno(700.0, 1.0, 3, 1, true));
        assert_eq!(l.fit, Fit::FieldShrunk);
        assert_eq!(l.field_w, 438.0);
        assert!(l.overflow.is_empty());
    }

    #[test]
    fn part_at_answers_for_what_is_there_and_for_nothing_else() {
        for (tabs, active, width, editing) in [(1usize, 0, 700.0f32, false), (3, 1, 1100.0, false), (12, 5, 700.0, true), (40, 39, 1920.0, false), (40, 0, 1920.0, true)] {
            let i = juno(width, 1.0, tabs, active, editing);
            let l = BarLayout::new(&i);
            assert_eq!(l.width_px, width, "the hit test works in the device px the parts are in");
            for b in &l.parts {
                let (x, y) = (b.cx(), b.cy());
                if b.w > 0.0 {
                    assert_eq!(l.part_at(x, y), Some(b.part), "{b:?} at its centre");
                    assert_eq!(l.part_at(b.x, y), Some(b.part), "{b:?} at its left edge");
                    assert_eq!(l.part_at(b.right() - 0.01, y), Some(b.part), "{b:?} at its last point");
                    assert_ne!(l.part_at(b.right(), y), Some(b.part), "{b:?} at its right edge, which is out");
                } else {
                    assert_ne!(l.part_at(b.x, y), Some(b.part), "a part squeezed to no width takes nothing");
                }
                assert_eq!(l.part_at(x, -1.0), None, "above the band");
                assert_eq!(l.part_at(x, l.band_h + 1.0), None, "below the band");
            }
            assert_eq!(l.part_at(PAD / 2.0, 22.0), None, "the side padding is nothing");
            assert_eq!(l.part_at(width - PAD / 2.0, 22.0), None, "and so is the right one");
        }
        // The field's own three areas, and the tab the click means.
        let l = BarLayout::new(&juno(1100.0, 1.0, 3, 1, false));
        let c = l.field_capsule().unwrap();
        assert_eq!(l.part_at(c.x + 4.0, l.band_h / 2.0), Some(Part::FieldIcon));
        assert_eq!(l.part_at(c.cx(), l.band_h / 2.0), Some(Part::Field));
        assert_eq!(l.part_at(c.right() - 4.0, l.band_h / 2.0), Some(Part::FieldReload));
        assert_eq!(l.part_at(put(&l, Part::Tab(2)).cx(), l.band_h / 2.0), Some(Part::Tab(2)));
        assert_eq!(l.part_at(put(&l, Part::DragRegion).cx(), l.band_h / 2.0), Some(Part::DragRegion), "the middle drags the window");
    }

    #[test]
    fn the_flags_move_nothing_and_a_tab_count_of_none_answers() {
        for flags in [(false, false), (true, false), (false, true), (true, true)] {
            let i = BarInput { can_back: flags.0, can_forward: flags.1, ..juno(1100.0, 1.0, 3, 1, false) };
            let l = BarLayout::new(&i);
            assert_eq!(&l.parts, &BarLayout::new(&juno(1100.0, 1.0, 3, 1, false)).parts, "back and forward are always placed");
            assert_eq!((l.can_back, l.can_forward), flags, "carried through for the drawer");
        }
        let l = BarLayout::new(&juno(1100.0, 1.0, 0, 0, false));
        assert_eq!(spine(&l), vec![Part::Back, Part::Forward, Part::NewTab, Part::DragRegion, Part::Star, Part::Menu]);
        assert_eq!(l.field_w, 0.0);
        assert_eq!(l.field_capsule(), None);
        assert_eq!(l.band_h, BAR_H);
        // An active index past the end is the last tab, not a panic and not a missing field.
        let l = BarLayout::new(&juno(1100.0, 1.0, 3, 9, false));
        assert_eq!(l.parts.iter().filter(|b| b.part == Part::Field).count(), 1);
        assert_eq!(l.placed(Part::Tab(2)), None, "the last tab's slot is the field");
        assert_eq!(l.field_x, put(&l, Part::Tab(1)).right() + GAP, "and the field's capsule is at the run's end");
        // Junk input is treated as the smallest bar, not as a panic.
        let i = BarInput { width: f32::NAN, scale: 0.0, left_inset: -50.0, ..juno(0.0, 1.0, 2, 0, false) };
        let l = BarLayout::new(&i);
        assert_eq!(l.band_h, 44.0, "a scale of 0 is read as 1");
        holds(&l, &i, "junk");
    }

    #[test]
    fn macos_starts_right_of_the_window_buttons() {
        for tabs in [1usize, 3, 12, 40] {
            for width in [700.0f32, 1100.0, 1920.0] {
                for scale in [1.0f32, 2.0] {
                    let i = mac(width, scale, tabs, tabs / 2, false);
                    let l = BarLayout::new(&i);
                    holds(&l, &i, &format!("mac n{tabs} w{width} s{scale}"));
                    assert_eq!(put(&l, Part::Back).x, INSET_MACOS * scale, "the first control is at 78 points");
                    assert_eq!(put(&l, Part::FieldIcon).x >= INSET_MACOS * scale, true);
                }
            }
        }
        // An inset nobody can pay is cut to what it can be, and the bar still answers.
        let i = mac(300.0, 1.0, 3, 1, false);
        let l = BarLayout::new(&i);
        holds(&l, &i, "mac 300");
    }

    #[test]
    fn a_narrow_window_gives_up_spacing_then_widths_then_the_field() {
        // 1. One tab on Juno needs 410 points with the design's spacing; from there the ladder walks down.
        let l = BarLayout::new(&juno(410.0, 1.0, 1, 0, false));
        nominal(&l, &juno(410.0, 1.0, 1, 0, false), "410");
        assert_eq!(l.field_w, FIELD_MIN, "the field at its minimum, and not a point less");

        // 2. Below it the gaps close before anything is lost: 400 is 10 points short over four gaps.
        let i = juno(400.0, 1.0, 1, 0, false);
        let l = BarLayout::new(&i);
        holds(&l, &i, "400");
        assert_eq!(put(&l, Part::Forward).x - put(&l, Part::Back).right(), 3.5, "the gaps took the whole loss");
        assert_eq!(put(&l, Part::Menu).x - put(&l, Part::Star).right(), GAP, "the star/menu spacing is untouched");
        assert_eq!(put(&l, Part::Menu).right(), 400.0 - PAD, "and so is the padding");
        assert_eq!(l.field_w, FIELD_MIN);

        // 3. Then the star/menu spacing: at 380 the gaps are gone and their 6+10 points go too.
        let i = juno(380.0, 1.0, 1, 0, false);
        let l = BarLayout::new(&i);
        holds(&l, &i, "380");
        assert_eq!(put(&l, Part::Forward).x, put(&l, Part::Back).right(), "the gaps are nothing");
        assert_eq!(put(&l, Part::Menu).x, put(&l, Part::Star).right(), "the pair touches");
        assert_eq!(put(&l, Part::Menu).right(), 380.0 - PAD, "the padding is the last spacing left");
        assert_eq!(l.field_w, FIELD_MIN);

        // 4. Then the widths, in order: menu, star, +, chip. Back and forward are never given up.
        let i = juno(340.0, 1.0, 1, 0, false);
        let l = BarLayout::new(&i);
        holds(&l, &i, "340");
        assert_eq!(put(&l, Part::Menu).w, 0.0);
        assert_eq!(put(&l, Part::Star).w, 0.0);
        assert_eq!(put(&l, Part::NewTab).w, BTN, "the + outlasts them both");
        assert!(l.field_w >= FIELD_MIN, "and the field is at or above its minimum: {}", l.field_w);

        // The shed order never skips one: whatever has gone was gone before it, in that order.
        for width in [370.0f32, 350.0, 340.0, 320.0, 300.0, 280.0, 250.0, 200.0, 120.0, 0.0] {
            for tabs in [1usize, 3, 40] {
                let i = juno(width, 1.0, tabs, tabs / 2, false);
                let l = BarLayout::new(&i);
                holds(&l, &i, &format!("shed order {width} {tabs}"));
                let gone = |p: Part| l.placed(p).map(|b| b.w == 0.0).unwrap_or(true);
                if gone(Part::NewTab) {
                    assert!(gone(Part::Star) && gone(Part::Menu), "{width}: the + went before the star could");
                }
                if gone(Part::Star) {
                    assert!(gone(Part::Menu), "{width}: the menu went before the star");
                }
                if width >= 2.0 * BTN + PAD + PAD {
                    assert_eq!(put(&l, Part::Back).w, BTN, "{width}: back is never given up");
                    assert_eq!(put(&l, Part::Forward).w, BTN, "{width}: forward is never given up");
                }
            }
        }

        // 5. Below the floor - the start, two buttons and the field's minimum, 286 on Juno - only the field shrinks, and it
        //    stays whole: never clipped at the edge, never pushed out of view.
        let mut w = 0.0f32;
        while w <= 460.0 {
            let i = juno(w, 1.0, 3, 1, false);
            let l = BarLayout::new(&i);
            holds(&l, &i, &format!("the floor at {w}"));
            assert!(l.field_x + l.field_w <= l.width_px, "{w}: the field is inside the window, never clipped or pushed out of view");
            if w >= 2.0 * BTN + PAD + PAD {
                assert_eq!(put(&l, Part::Back).w, BTN, "{w}: back and forward are the pair the inset protects");
                assert_eq!(put(&l, Part::Forward).w, BTN, "{w}: and neither is ever given up");
            }
            if l.field_w >= 2.0 * BTN {
                assert_eq!(put(&l, Part::FieldIcon).w, BTN, "{w}: the lock and the reload are the field's, and stay while it has room for them");
                assert_eq!(put(&l, Part::FieldReload).w, BTN, "{w}");
            }
            if w >= 286.0 {
                assert!(l.field_w >= FIELD_MIN, "{w}: above the floor and still at or over the minimum, got {}", l.field_w);
            } else {
                assert!(l.field_w < FIELD_MIN, "{w}: below the floor, so this is the field giving way, got {}", l.field_w);
                assert_eq!(l.field_x + l.field_w, l.width_px, "{w}: it took every point the window had");
            }
            w += 10.0;
        }
    }


    #[test]
    fn a_tab_count_no_bar_can_hold_is_still_one_bar() {
        // Whatever the window, the run holds only the tabs that fit and one chip naming the rest, so the bar stays a bar.
        // What a fold costs is the folded indices' vector and its sort - the tab count, once.
        let mut i = juno(1920.0, 1.0, 20_000, 10_000, false);
        for width in [4096.0f32, 1920.0, 700.0, 320.0, 0.0] {
            for inset in [0.0f32, INSET_MACOS] {
                i.width = width;
                i.left_inset = inset;
                let l = BarLayout::new(&i);
                let ctx = format!("20000 tabs at {width} inset {inset}");
                holds(&l, &i, &ctx);
                assert_eq!(l.fit, Fit::Folded, "{ctx}: nothing could ever hold twenty thousand tabs");
                assert!(l.overflow.len() > 19_000, "{ctx}");
                assert!(l.parts.len() < 200, "{ctx}: the bar stayed a bar: {} parts", l.parts.len());
            }
        }
        // A window wide enough for the whole list holds the whole list, and says so by placing every tab itself.
        let l = BarLayout::new(&juno(20_000_000.0, 1.0, 20_000, 10_000, false));
        assert_eq!(l.fit, Fit::Full);
        assert!(l.overflow.is_empty() && l.placed(Part::OverflowChip).is_none());
        // Nineteen thousand nine hundred and nine capsules, the field's capsule cut into three, and the six fixed controls.
        assert_eq!(l.parts.len(), 20_000 + 8);
        assert_eq!(l.field_w, FIELD_PREF);
        assert_eq!(put(&l, Part::Menu).right(), 20_000_000.0 - PAD);
    }

    #[test]
    fn the_scale_is_only_ever_multiplied_in_at_the_end() {
        // Build it at 1 and scale it here: the only way the module takes a scale factor is by multiplying the points.
        let mut i = juno(1100.0, 1.0, 3, 1, false);
        for scale in [1.0f32, 2.0] {
            for tabs in [1usize, 3, 12, 40] {
                for width in [700.0f32, 1100.0, 1920.0] {
                    for editing in [false, true] {
                        i.scale = scale;
                        i.tabs = tabs;
                        i.active = tabs / 2;
                        i.width = width;
                        i.editing = editing;
                        let l = BarLayout::new(&i);
                        i.scale = 1.0;
                        let base = BarLayout::new(&i);
                        i.scale = scale;
                        let ctx = format!("n{tabs} w{width} s{scale} editing{editing}");
                        assert_eq!(l.band_h, base.band_h * scale, "{ctx}");
                        assert_eq!(l.width_px, base.width_px * scale, "{ctx}");
                        assert_eq!(l.capsule_y, base.capsule_y * scale, "{ctx}");
                        for (a, b) in l.parts.iter().zip(base.parts.iter()) {
                            assert_eq!(a.part, b.part, "{ctx}");
                            for (x, y) in [(a.x, b.x), (a.y, b.y), (a.w, b.w), (a.h, b.h)] {
                                assert_eq!(x, y * scale, "{ctx}: {a:?} against {b:?}");
                            }
                            assert_eq!(a.compact, b.compact, "{ctx}");
                        }
                        assert_eq!(l.field_x, base.field_x * scale, "{ctx}");
                        assert_eq!(l.field_w, base.field_w * scale, "{ctx}");
                        assert_eq!(l.overflow, base.overflow, "{ctx}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_fold_keeps_every_tab_it_left_at_the_width_of_its_stage() {
        // However many tabs the window folds away, the ones it keeps are the width the stage says, not scraped smaller:
        // 320 points of room takes a hundred tabs down to four of them, all at the same size.
        for (width, tabs, active) in [(700.0f32, 40usize, 0usize), (700.0, 40, 39), (700.0, 40, 20), (700.0, 12, 5), (512.0, 20, 10)] {
            let i = juno(width, 1.0, tabs, active, false);
            let l = BarLayout::new(&i);
            let ctx = format!("{width} with {tabs} tabs, active {active}");
            holds(&l, &i, &ctx);
            let kept: Vec<f32> = l.parts.iter().filter(|b| matches!(b.part, Part::Tab(_))).map(|b| b.w).collect();
            assert!(!kept.is_empty(), "{ctx}: the fold always leaves a neighbour");
            for w in &kept {
                assert_eq!(*w, TAB_COMPACT, "{ctx}: a folded bar is a compact bar, and {w} is neither");
            }
            // What was folded away is the outer end of the list on each side of the active tab, never a hole in the middle.
            let gone = &l.overflow;
            let below: Vec<usize> = gone.iter().copied().filter(|t| *t < active).collect();
            let above: Vec<usize> = gone.iter().copied().filter(|t| *t > active).collect();
            assert_eq!(below, (0..below.len()).collect::<Vec<usize>>(), "{ctx}: the left fold starts at the first tab");
            assert_eq!(above, (tabs - above.len()..tabs).collect::<Vec<usize>>(), "{ctx}: the right fold ends at the last");
            assert_eq!(gone.len(), below.len() + above.len(), "{ctx}: the active tab is never folded");
        }
    }

    #[test]
    fn nothing_in_a_window_with_room_is_clipped_or_moved() {
        // The safety pack that keeps parts inside the window is a no-op at sane widths: the drag region reaches the star,
        // the run stops short of the +, and the field is where the rule put it.
        for tabs in [1usize, 3, 12, 40] {
            for width in [700.0f32, 1100.0, 1920.0] {
                let i = juno(width, 1.0, tabs, tabs / 2, false);
                let l = BarLayout::new(&i);
                let sp = spine(&l);
                for p in sp.iter().filter(|p| **p != Part::DragRegion) {
                    let b = put(&l, *p);
                    assert!(b.w > 0.0, "{width} n{tabs}: {p:?} was squeezed away");
                }
                if tabs > 1 {
                    assert_eq!(sp[sp.len() - 3], Part::DragRegion);
                }
                assert!(put(&l, Part::NewTab).right() + GAP <= put(&l, Part::Star).x, "{width} n{tabs}: the + before the star");
            }
        }
    }
}
