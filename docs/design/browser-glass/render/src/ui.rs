//! Shared pieces of the browser mockups: the device setup, text helpers and the small controls the kit lacks (see
//! docs/design/browser-glass.md, "Kit gaps").

use lucina_glass::canvas::{Color, R};
use lucina_glass::text::Font;
use lucina_glass::tokens::{color, space, ty};
use lucina_glass::widgets::{BarButton, PushButton, SegmentedPlain, Switch};
use lucina_glass::Paint;

use crate::icons::I;

pub const STATUS: f32 = 32.0;
pub const GESTURE: f32 = 24.0;
pub const KEYBOARD: f32 = 224.0 + GESTURE;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dev {
    Juno,
    Phone,
    Tablet,
}

#[derive(Clone, Copy)]
pub struct Cfg {
    pub dev: Dev,
    pub w: f32,
    pub h: f32,
    pub k: f32,
}

impl Cfg {
    pub fn new(dev: Dev) -> Cfg {
        match dev {
            Dev::Juno => Cfg { dev, w: 1366.0, h: 768.0, k: 1.5 },
            Dev::Phone => Cfg { dev, w: 360.0, h: 800.0, k: 3.0 },
            Dev::Tablet => Cfg { dev, w: 1024.0, h: 768.0, k: 1.5 },
        }
    }
    pub fn name(&self) -> &'static str {
        match self.dev {
            Dev::Juno => "juno",
            Dev::Phone => "diana-phone",
            Dev::Tablet => "diana-tablet",
        }
    }
    /// Diana phone: 1080 x 2400 px at 3 px/dp = 360 x 800 dp (the kit's phone canvas). Status bar 32 dp (the front camera
    /// hole sits in it); gesture inset 24 dp (the indicator is drawn in it); keyboard 224 dp of keys plus that inset.
    pub fn top_inset(&self) -> f32 {
        if self.phone() { STATUS } else { 0.0 }
    }
    pub fn phone(&self) -> bool {
        self.dev == Dev::Phone
    }
    pub fn touch(&self) -> bool {
        self.dev != Dev::Juno
    }
    /// A control's diameter: 36 with a pointer, the 44 dp tap target on Diana.
    pub fn ctl(&self) -> f32 {
        if self.touch() { space::TAP } else { 36.0 }
    }
    pub fn row(&self) -> f32 {
        if self.touch() { 52.0 } else { 40.0 }
    }
    /// Main text of a row and its secondary line.
    pub fn main(&self) -> (f32, f32) {
        if self.touch() { ty::HEADLINE } else { ty::LEAD }
    }
    pub fn sub(&self) -> (f32, f32) {
        if self.touch() { ty::BODY } else { ty::CAPTION }
    }
}

pub use lucina_glass::widgets::ink::{divider, text_centre as text_c, text_left as text_l, text_right as text_r};

pub fn ic(p: &mut Paint, i: I, cx: f32, cy: f32, size: f32, c: Color) {
    lucina_glass::icons::draw(p.cv, i, cx, cy, size, c);
}

/// A floating glass panel with its shadow: popovers, sheets.
pub use lucina_glass::widgets::popover::panel;

/// A thicker material for content that is read (rule 7).
pub fn card(p: &mut Paint, r: R, rad: f32) {
    p.glass(r, rad, color::MATERIAL);
}

/// A quiet round control inside a glass bar: no glass of its own, an icon, a soft fill while it is pressed or active.
pub fn bar_btn(p: &mut Paint, cx: f32, cy: f32, d: f32, i: I, ink: Color, active: bool) {
    let mut b = BarButton::new(i);
    b.active = active;
    b.draw(p, cx, cy, d, ink);
}

/// An action: a round control with an icon over a one-word label (rule 2), inside a panel.
pub fn action(p: &mut Paint, cx: f32, y: f32, i: I, label: &str) {
    let d = space::TAP;
    p.cv.fill_circle(cx, y + d / 2.0, d / 2.0, color::INK.alpha(0.10));
    p.cv.stroke_circle(cx, y + d / 2.0, d / 2.0 - 0.3, 0.6, color::HAIR);
    ic(p, i, cx, y + d / 2.0, 20.0, color::INK);
    text_c(p, Font::Semibold, ty::SMALL, cx, y + d + 13.0, color::INK, label);
}

/// A switch, on = accent (selection and activity are the one accent). Centre-right anchored.
pub fn toggle(p: &mut Paint, right: f32, cy: f32, on: bool) {
    Switch::new(on).draw(p, right, cy);
}

/// A text button: a verb. `primary` is the light PAPER button of the kit (the accent stays for selection).
pub fn button(p: &mut Paint, r: R, label: &str, primary: bool) {
    PushButton::new(label, primary).draw(p, r);
}

/// A tab or section caption: caption 12/600, secondary ink.
pub fn caption(p: &mut Paint, x: f32, cy: f32, s: &str) {
    text_l(p, Font::Semibold, ty::CAPTION, x, cy, color::INK_2, s);
}

/// A quiet disc with an initial: a site's mark when no icon was fetched (no third-party marks, no decoration).
pub fn site_mark(p: &mut Paint, cx: f32, cy: f32, d: f32, host: &str) {
    lucina_glass::widgets::tab_chip::site_mark(p, cx, cy, d, host, color::INK);
}

/// A segmented control without the kit's trailing plus.
pub fn segmented(p: &mut Paint, r: R, labels: &[&str], selected: usize) {
    SegmentedPlain::new(labels, selected).draw(p, r);
}
