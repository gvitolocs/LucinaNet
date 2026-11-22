//! The glass top bar: one row - back, forward, the tabs with the active one as the address field, new tab, the drag space, the
//! star, the menu - drawn with the lucina-glass kit on a CPU canvas, then handed to the toolkit as pixels (`Ui::pixels`) over
//! the band the window reserved for the bar. The browser's own logic (keys, clicks, the address editor, the omnibox) stays in
//! `browser.rs`; this only draws what it is told, where [`crate::onebar::BarLayout`] placed it.
//!
//! Backdrop. The page sits BELOW the bar in this slice (the engine renders into the rectangle under it), so there is no page
//! under the glass. The backdrop is made from the page's top rows, MIRRORED about the seam (the page's first row lands next
//! to the bar's bottom edge) and stretched to the band, taken at half resolution, blurred, and kept until those rows change
//! ([`Backdrop::sample`] compares a hash). Known limit: true page-under-bars needs engine support (render full height with a top
//! inset), a follow-up.
//!
//! Cost rules: the backdrop is sampled every frame (a hash over the half-size samples, no allocation) but blurred only
//! when the hash changed; the chrome canvas is drawn again only when what it shows changed (tabs, address, caret phase, hover,
//! backdrop generation, adaptive flip); a blur over [`BLUR_BUDGET_MS`] makes the capsules solid tinted ones.
//!
//! The layout's rectangles are device px ([`crate::onebar`]); this canvas is drawn in points at the window's scale, so every box
//! crosses through [`dp`].

use std::f32::consts::{FRAC_PI_2, PI};
use std::time::{Duration, Instant};

use lucina_glass::browser_rules;
use lucina_glass::canvas::{Canvas, Color, R};
use lucina_glass::icons::{self, Icon};
use lucina_glass::text::{Font, Type};
use lucina_glass::tokens::{color, glass, radius, ty};
use lucina_glass::widgets::adaptive::{Adapt, Adaptive, region_luma};
use lucina_glass::widgets::chrome::spinner;
use lucina_glass::widgets::ink::{text_centre, text_left};
use lucina_glass::widgets::tab_chip::site_mark;
use lucina_glass::widgets::text_field::BLINK_MS;
use lucina_glass::widgets::BarButton;
use lucina_glass::Paint;
use url::Url;

use crate::{Security, TabInfo, security_of};
use crate::chrome::{BLUR_BUDGET_MS, blur_now, use_blur};
use crate::onebar::{BarLayout, Part, Placed};

/// An even number at or above `v` (the glass kit reads a half-size backdrop only over a canvas of even size).
fn even_up(v: u32) -> u32 {
    v + (v & 1)
}

/// The band's size in px for this bar: the window's width and the height down to where the page starts, both even.
pub fn band_px(layout: &BarLayout) -> (u32, u32) {
    (even_up(layout.width_px.round().max(2.0) as u32), even_up(layout.band_h.round().max(2.0) as u32))
}

/// A placed part as the canvas's rectangle: the layout's device px back to the points this canvas draws in.
fn dp(b: &Placed, k: f32) -> R {
    R::new(b.x / k, b.y / k, (b.w / k).max(0.0), (b.h / k).max(0.0))
}

/// The hairline under the bar, and the progress line inside the field: how thick, in points.
const HAIR: f32 = 0.7;
const PROGRESS: f32 = 2.0;
/// How long the loading sweep takes to cross the field.
const SWEEP: f64 = 1.6;
/// The edge of the field in a refusal's flash (the kit has no red token).
const REFUSED: Color = Color::hex(0xff453a);

/// The backdrop the glass shows: the page's top rows, mirrored, at half resolution, blurred.
pub struct Backdrop {
    /// The samples, half size, unblurred: always there once a page was seen; the luminance reads it when no blur is shown.
    src: Option<Canvas>,
    blurred: Option<Canvas>,
    scratch: Vec<u32>,
    sig: Option<u64>,
    seen: bool,
    pending: bool,
    last_blur_ms: Option<f32>,
    since_slow: u32,
    /// Counts: samples that differed from the last (so were kept), blurs run.
    pub changes: u32,
    pub blurs: u32,
    /// Bumps whenever the backdrop (or its absence) changed what the glass shows.
    pub generation: u64,
}

impl Backdrop {
    pub fn new() -> Backdrop {
        Backdrop { src: None, blurred: None, scratch: Vec::new(), sig: None, seen: false, pending: false, last_blur_ms: None, since_slow: 0, changes: 0, blurs: 0, generation: 0 }
    }

    /// Take the page's top rows for a band of `w` x `h` px (both even) at `k` px/dp. Cheap when nothing changed: a hash over
    /// the samples, nothing kept. Returns whether they differ from the last time. Do not hold the page's lock for longer than this.
    pub fn sample(&mut self, px: &[u32], pw: usize, ph: usize, w: u32, h: u32, k: f32) -> bool {
        let (hw, hh) = ((w / 2) as usize, (h / 2) as usize);
        if pw == 0 || ph == 0 || px.len() < pw * ph || hw == 0 || hh == 0 {
            // No page yet: nothing to sample, and a state of its own (a plain ground).
            return self.set_sig(None);
        }
        self.scratch.clear();
        self.scratch.reserve(hw * hh);
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325 ^ (u64::from(w) << 32 | u64::from(h));
        for hy in 0..hh {
            // The pair of band rows 2hy, 2hy+1 shows page row `h - 2 - 2hy`: mirrored about the seam, clamped to the page.
            let sy = (h as usize - 2 - hy * 2).min(ph - 1);
            let row = &px[sy * pw..(sy + 1) * pw];
            for hx in 0..hw {
                let v = row[(hx * 2 * pw / w as usize).min(pw - 1)];
                hash = (hash ^ u64::from(v)).wrapping_mul(0x0000_0100_0000_01b3);
                self.scratch.push(v);
            }
        }
        if !self.set_sig(Some(hash)) {
            return false;
        }
        let mut c = Canvas::new(hw as u32, hh as u32, k / 2.0);
        c.px.copy_from_slice(&self.scratch);
        self.src = Some(c);
        self.pending = true;
        true
    }

    fn set_sig(&mut self, sig: Option<u64>) -> bool {
        if self.seen && self.sig == sig {
            return false;
        }
        self.seen = true;
        self.sig = sig;
        self.changes += 1;
        if sig.is_none() {
            self.src = None;
            self.blurred = None;
            self.generation += 1;
        }
        true
    }

    /// Blur what [`sample`](Self::sample) kept, if it kept something new and the budget rule allows. Returns the blur's time.
    pub fn refresh(&mut self, k: f32) -> Option<f32> {
        if !std::mem::take(&mut self.pending) {
            return None;
        }
        self.generation += 1;
        let Some(src) = &self.src else { return None };
        if !blur_now(self.last_blur_ms, self.since_slow) {
            self.since_slow += 1;
            self.blurred = None;
            return None;
        }
        let r = ((glass::BLUR * k / 2.0).round() as u32).max(1);
        let t = Instant::now();
        let b = src.blurred(r);
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        self.blurred = Some(b);
        self.blurs += 1;
        self.last_blur_ms = Some(ms);
        self.since_slow = 0;
        Some(ms)
    }

    /// The backdrop the glass is drawn over: the blurred rows, unless the blur was over budget (then a plain tinted capsule).
    pub fn for_glass(&self) -> Option<&Canvas> {
        if use_blur(self.last_blur_ms) { self.blurred.as_ref() } else { None }
    }

    /// What the luminance and the ground are read from: the blurred rows if there are, else the unblurred samples.
    pub fn for_luma(&self) -> Option<&Canvas> {
        self.blurred.as_ref().or(self.src.as_ref())
    }

    pub fn last_blur_ms(&self) -> Option<f32> {
        self.last_blur_ms
    }
}

impl Default for Backdrop {
    fn default() -> Self {
        Backdrop::new()
    }
}

/// What the bar shows: everything `draw` reads from the browser, owned so a frame can be compared with the last one.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct State {
    pub tabs: Vec<TabInfo>,
    pub active: u32,
    /// The address as the page has it (the field shows its display), or what is being typed.
    pub address: String,
    /// The registrable domain of `address` as the engine's Public Suffix List answers it, the part of the field to draw in
    /// the full ink. `None` is an answer (`localhost`, an IP, a bare suffix) and puts the whole host in that ink; the
    /// browser only ever sets it for the `address` it is showing with it (`browser.rs`, `field_domain`).
    pub domain: Option<String>,
    pub typing: bool,
    pub cursor: usize,
    pub anchor: usize,
    pub placeholder: String,
    pub loading: bool,
    /// The page's zoom label, shown in the row's empty space while it is not 100 %.
    pub zoom: Option<String>,
    /// The shown page's bookmark, as the engine told the window: the star is filled when it is set.
    pub bookmarked: bool,
    pub note: Option<String>,
    /// A refusal's shake: how far the field is pushed sideways (points) and how red its edge is (0 to 1), both from
    /// [`crate::address::shake_offset`] and [`crate::address::shake_flash`] at the frame's time. Part of the frame's key,
    /// so the canvas is drawn again while it moves.
    pub shake: f32,
    pub flash: f32,
    /// The inactive tab whose close mark the pointer is over: a full tab shows it, a compact one has no room.
    pub close_hover: Option<usize>,
}

/// One frame of the bar: the pixels of the band (XRGB, `w` per row).
pub struct Frame<'a> {
    pub px: &'a [u32],
    pub w: u32,
    pub h: u32,
    /// Whether the canvas was drawn this call (false: the cached one).
    pub redrawn: bool,
    /// What this call took, milliseconds.
    pub ms: f32,
}

#[derive(PartialEq)]
struct Key {
    state: State,
    layout: BarLayout,
    size: (u32, u32),
    k: u32,
    generation: u64,
    caret: bool,
    anim: u64,
}

/// Draws and caches the bar.
pub struct GlassChrome {
    cv: Canvas,
    ty: Type,
    pub backdrop: Backdrop,
    bar_look: Adaptive,
    last: Option<Key>,
    start: Instant,
    /// When the address text or caret last changed: the caret is solid just after.
    edit_at: Instant,
    edit_key: (String, usize, usize),
    /// Where the text is scrolled to, dp.
    scroll: f32,
    /// How many times the canvas was drawn, and how many calls found it unchanged.
    pub draws: u32,
    pub cached: u32,
    pub last_draw_ms: f32,
    pub last_cached_ms: f32,
    /// The bar is still flipping between dark and light, the caret blinks or the page loads: when to ask for another frame.
    wake: Option<Duration>,
    /// The look of the last draw, for the dropdown's colours.
    pub look: Adapt,
}

impl GlassChrome {
    pub fn new() -> GlassChrome {
        let now = Instant::now();
        GlassChrome {
            cv: Canvas::new(2, 2, 1.0),
            ty: Type::new(),
            backdrop: Backdrop::new(),
            bar_look: Adaptive::new(),
            last: None,
            start: now,
            edit_at: now,
            edit_key: (String::new(), 0, 0),
            scroll: 0.0,
            draws: 0,
            cached: 0,
            last_draw_ms: 0.0,
            last_cached_ms: 0.0,
            wake: None,
            look: lucina_glass::widgets::adaptive::look(0.0),
        }
    }

    /// When to draw again without any input (the caret's blink, the loading sweep, the dark/light flip), if at all.
    pub fn wake_after(&self) -> Option<Duration> {
        self.wake
    }

    /// The frame for `state` in `layout`. The backdrop must have been sampled for this size first.
    pub fn frame(&mut self, state: &State, layout: &BarLayout) -> Frame<'_> {
        let t0 = Instant::now();
        let k = layout.scale;
        let (w, h) = band_px(layout);
        if let Some(ms) = self.backdrop.refresh(k) {
            if ms > BLUR_BUDGET_MS {
                eprintln!("lucinanet: glass chrome blur took {ms:.1} ms, over the {BLUR_BUDGET_MS} ms budget: plain tinted capsules until it fits");
            }
        }
        let now = self.start.elapsed().as_secs_f64();
        // The caret: solid for the first half of its period after the last edit.
        let edit_key = (state.address.clone(), state.cursor, state.anchor);
        if edit_key != self.edit_key || !state.typing {
            self.edit_key = edit_key;
            self.edit_at = t0;
        }
        let blink_ms = t0.duration_since(self.edit_at).as_millis() as u64;
        let caret = state.typing && browser_rules::caret_visible(blink_ms, BLINK_MS);
        let settled = self.bar_look.settled(now);
        let key = Key {
            state: state.clone(),
            layout: layout.clone(),
            size: (w, h),
            k: k.to_bits(),
            generation: self.backdrop.generation,
            caret,
            anim: if settled && !state.loading { 0 } else { (now * 60.0) as u64 + 1 },
        };
        self.wake = if state.typing {
            let half = BLINK_MS / 2;
            Some(Duration::from_millis(half - blink_ms % half))
        } else if state.loading {
            Some(Duration::from_millis(16))
        } else {
            None
        };
        if !settled {
            self.wake = Some(Duration::from_millis(16));
        }
        if self.last.as_ref() == Some(&key) {
            self.cached += 1;
            let ms = t0.elapsed().as_secs_f32() * 1000.0;
            self.last_cached_ms = ms;
            return Frame { px: &self.cv.px, w, h, redrawn: false, ms };
        }
        if (self.cv.w, self.cv.h) != (w, h) || self.cv.k != k {
            self.cv = Canvas::new(w, h, k);
        }
        self.draw(state, layout, now, caret);
        self.last = Some(key);
        self.draws += 1;
        let ms = t0.elapsed().as_secs_f32() * 1000.0;
        self.last_draw_ms = ms;
        if self.draws == 1 || std::env::var_os("LUCINANET_CHROME_LOG").is_some() {
            eprintln!(
                "lucinanet: glass chrome draw #{} {ms:.2} ms ({w}x{h} px, backdrop blur {}, fallback {})",
                self.draws,
                self.backdrop.last_blur_ms().map_or("none".to_owned(), |b| format!("{b:.2} ms")),
                self.backdrop.for_glass().is_none()
            );
        }
        Frame { px: &self.cv.px, w, h, redrawn: true, ms }
    }

    /// The ground under the capsules: the backdrop rows at full size, or the kit's dark ground before a page.
    fn ground(&mut self) {
        let Some(src) = self.backdrop.for_luma() else {
            self.cv.clear(color::CARBON);
            return;
        };
        let (w, h, sw) = (self.cv.w as usize, self.cv.h as usize, src.w as usize);
        for y in 0..h {
            let srow = &src.px[(y / 2).min(src.h as usize - 1) * sw..][..sw];
            let drow = &mut self.cv.px[y * w..(y + 1) * w];
            for (x, d) in drow.iter_mut().enumerate() {
                *d = srow[(x / 2).min(sw - 1)];
            }
        }
    }

    fn draw(&mut self, st: &State, l: &BarLayout, now: f64, caret: bool) {
        self.ground();
        let k = l.scale;
        let band = R::new(0.0, 0.0, l.width_px / k, l.band_h / k);
        let luma = self.backdrop.for_luma().map_or(0.0, |c| region_luma(c, band));
        let GlassChrome { cv, ty: tp, bar_look, scroll, backdrop, .. } = self;
        let mut p = Paint { cv, ty: tp, now, backdrop: backdrop.for_glass() };
        bar_look.set_luma(p.now, luma);
        let a = bar_look.look(p.now);
        // The row's only border: a hairline along the band's bottom edge.
        p.cv.fill_rrect(R::new(0.0, band.h - HAIR, band.w, HAIR), 0.0, color::HAIR);
        // The field's capsule first: it is the row's brighter layer. The parts never overlap, so one pass in the
        // layout's order draws every capsule before any of the text on top of them.
        // A refusal's shake pushes the whole field - capsule, text, icons - sideways by `st.shake`, and its edge flashes red.
        let push = |r: R| R::new(r.x + st.shake, r.y, r.w, r.h);
        if let Some(c) = l.field_capsule() {
            let cap = push(dp(&c, k));
            capsule(&mut p, &a, cap, true);
            if st.flash > 0.0 {
                p.cv.stroke_rrect(cap, radius::PILL, 1.8, REFUSED.alpha(st.flash.clamp(0.0, 1.0)));
            }
        }
        for b in &l.parts {
            let r = dp(b, k);
            match b.part {
                Part::Back => button(&mut p, r, Icon::Back, if l.can_back { a.ink } else { a.ink3 }),
                Part::Forward => button(&mut p, r, Icon::Forward, if l.can_forward { a.ink } else { a.ink3 }),
                Part::Tab(i) => tab(&mut p, r, st, i, b.compact, &a),
                Part::OverflowChip => {
                    capsule(&mut p, &a, r, false);
                    let n = l.overflow.len().to_string();
                    text_centre(&mut p, Font::Semibold, ty::SMALL, r.cx(), r.cy(), a.ink2, &n);
                },
                Part::NewTab => button(&mut p, r, Icon::Plus, a.ink2),
                Part::Field => field(&mut p, st, l, push(r), &a, scroll, caret, k),
                Part::FieldIcon => field_icon(&mut p, st, push(r), &a),
                // Reload is not a bar button: it is this icon, at the field's right end inside the capsule, a stop while loading.
                Part::FieldReload => {
                    let r = push(r);
                    let icon = if st.loading { Icon::Close } else { Icon::Reload };
                    icons::draw(p.cv, icon, r.cx(), r.cy(), 18.0, a.ink2);
                },
                Part::DragRegion => readout(&mut p, st, r, &a),
                Part::Star => star(&mut p, r, &a, st.bookmarked),
                Part::Menu => button(&mut p, r, Icon::Dots, a.ink2),
            }
        }
        self.look = a;
    }

    /// Whether the caret is lit in the last drawn frame (for tests and the dropdown).
    pub fn caret_lit(&self) -> bool {
        self.last.as_ref().is_some_and(|k| k.caret)
    }

    /// The canvas's pixel at (`x`, `y`) of the band (for tests).
    pub fn pixel(&self, x: u32, y: u32) -> u32 {
        self.cv.get(x as i32, y as i32)
    }
}

impl Default for GlassChrome {
    fn default() -> Self {
        GlassChrome::new()
    }
}

/// The classic dropdown's colours (0xRRGGBB: panel, text, dim text, selection, border) taken from the bar's look: its tint made
/// opaque, its ink, the second ink over the panel, the accent over the panel and the accent.
pub fn dropdown_colours(a: &Adapt) -> [u32; 5] {
    let over = |c: Color, under: Color| {
        let f = f32::from(c.a) / 255.0;
        let m = |x: u8, y: u8| (f32::from(x) * f + f32::from(y) * (1.0 - f)).round() as u32;
        (m(c.r, under.r) << 16) | (m(c.g, under.g) << 8) | m(c.b, under.b)
    };
    let panel = Color::hex(a.tint.xrgb());
    [panel.xrgb() & 0xff_ffff, a.ink.xrgb() & 0xff_ffff, over(a.ink2, panel), over(Color::hexa(0x7f8fe0, 110), panel), color::ACCENT.xrgb() & 0xff_ffff]
}

/// A capsule of the row: a shadow, then the kit's glass, or a plain tinted pill when no blur is shown. The field is the
/// row's brighter layer (docs/design/one-bar.md "Material"), so it takes a little ink over the tint.
fn capsule(p: &mut Paint, a: &Adapt, r: R, bright: bool) {
    p.shadow(r, radius::PILL, 7.0);
    let tint = if bright { a.tint.mix(color::INK.alpha(0.07), 1.0) } else { a.tint };
    match p.backdrop {
        Some(b) => p.cv.glass(r, radius::PILL, b, tint, a.rim),
        None => {
            p.cv.fill_rrect(r, radius::PILL, tint);
            p.cv.stroke_rrect(r, radius::PILL, 0.6, color::HAIR);
        },
    }
}

/// A control of the row: the kit's bar button, the icon in `ink`, at the box the layout gave it.
fn button(p: &mut Paint, r: R, icon: Icon, ink: Color) {
    BarButton::new(icon).draw(p, r.cx(), r.cy(), r.w.min(r.h), ink);
}

/// The star, right of the row's empty space: the kit's vector star, filled in the accent when the page is bookmarked.
fn star(p: &mut Paint, r: R, a: &Adapt, bookmarked: bool) {
    let (icon, ink) = if bookmarked { (Icon::StarFill, color::ACCENT) } else { (Icon::Star, a.ink2) };
    icons::draw(p.cv, icon, r.cx(), r.cy(), r.w.min(r.h), ink);
}

/// One inactive tab: the site's mark on its own in the compact form the row falls back to, mark, title and - while the
/// pointer is over its close area - a close mark in the full one. The active tab is the field, never a part here.
fn tab(p: &mut Paint, r: R, st: &State, index: usize, compact: bool, a: &Adapt) {
    capsule(p, a, r, false);
    let Some(t) = st.tabs.get(index) else { return };
    let cy = r.cy();
    if compact {
        mark(p, r.cx(), cy, t, a);
        return;
    }
    // The kit's own chip geometry: mark 10 dp in, its diameter, the title after it, the close area held back at the end
    // whether or not the mark is drawn, so the title does not jump when the pointer arrives.
    let (d, back) = (18.0, 32.0);
    mark(p, r.x + 10.0 + d / 2.0, cy, t, a);
    let tx = r.x + 10.0 + d + 8.0;
    let room = (r.right() - back - tx).max(0.0);
    let title = p.fit(Font::Regular, ty::BODY, &tab_title(t), room);
    text_left(p, Font::Regular, ty::BODY, tx, cy, a.ink2, &title);
    if st.close_hover == Some(index) {
        icons::draw(p.cv, Icon::Close, r.right() - back / 2.0, cy, 16.0, a.ink);
    }
}

/// A tab's mark: the kit's site disc of the host's initial, a spinner while that tab loads.
fn mark(p: &mut Paint, cx: f32, cy: f32, t: &TabInfo, a: &Adapt) {
    if t.loading {
        spinner(p, cx, cy, 8.0, a.ink2);
    } else {
        site_mark(p, cx, cy, 18.0, &host_of(&t.url), a.ink);
    }
}

/// What the row's empty space says: why a tab was refused, else the page's zoom. Read-outs, not controls - the space is
/// the window's drag area, and the design's row holds no zoom button (Cmd/Ctrl+0 resets).
fn readout(p: &mut Paint, st: &State, r: R, a: &Adapt) {
    let room = (r.w - 16.0).max(0.0);
    if room < 40.0 {
        return;
    }
    let x = r.x + 8.0;
    if let Some(note) = &st.note {
        let note = p.fit(Font::Regular, ty::CAPTION, note, room);
        text_left(p, Font::Regular, ty::CAPTION, x, r.cy(), color::WARNING, &note);
    } else if let Some(zoom) = &st.zoom {
        let zoom = p.fit(Font::Semibold, ty::SMALL, zoom, room);
        text_left(p, Font::Semibold, ty::SMALL, x, r.cy(), a.ink2, &zoom);
    }
}

/// The field's left mark: `one-bar.md`'s search glass while the person types, else the closed padlock of an address that
/// came over a scheme encrypted on the wire, or the warning of one that did not. The classic chrome reads the same
/// [`security_of`] (`browser.rs`); here there is a canvas, so the two glyphs are drawn instead of stepped out of
/// rectangles, and the warning takes the kit's own amber.
fn field_icon(p: &mut Paint, st: &State, r: R, a: &Adapt) {
    let (cx, cy, s) = (r.cx(), r.cy(), 18.0);
    if st.typing {
        icons::draw(p.cv, Icon::Search, cx, cy, s, a.ink2);
        return;
    }
    match security_of(&st.address) {
        Security::Secure => lock(p, cx, cy, s, a.ink2),
        Security::NotSecure => warning(p, cx, cy, s),
    }
}

/// The padlock, closed. The kit's `Icon::Lock` is not one: its shackle is `arc(.., PI, PI)`, and the canvas counts angles
/// clockwise from twelve o'clock, so that sweep is the circle's left half - a hook over the body's left edge that reads as
/// a lock standing open. LucinaOS is not this crate to change, so the arch is drawn here: the kit's own body, legs and
/// stroke, the top half of the circle rather than the left one, and both legs down to the body's top edge.
fn lock(p: &mut Paint, cx: f32, cy: f32, s: f32, ink: Color) {
    let sw = s * 0.085;
    let at = |x: f32, y: f32| (cx + x * s, cy + y * s);
    for x in [-0.21, 0.21] {
        let (x0, y0) = at(x, -0.18);
        let (x1, y1) = at(x, -0.02);
        p.cv.line(x0, y0, x1, y1, sw, ink);
    }
    p.cv.arc(cx, cy - 0.18 * s, 0.21 * s, sw, -FRAC_PI_2, PI, ink);
    p.cv.fill_rrect(R::new(cx - 0.32 * s, cy - 0.02 * s, 0.64 * s, 0.44 * s), 0.09 * s, ink);
}

/// The warning triangle and its exclamation, in the kit's `color::WARNING`. The exclamation is cut in the kit's dark
/// `CARBON` rather than in what the mark sits on: the glass has no flat colour to cut with, and inside an amber triangle a
/// dark notch reads on a light page and on a dark one alike - as it does in the classic chrome, whose field is dark too.
fn warning(p: &mut Paint, cx: f32, cy: f32, s: f32) {
    let at = |x: f32, y: f32| (cx + x * s, cy + y * s);
    p.cv.polygon(&[at(0.0, -0.36), at(-0.40, 0.36), at(0.40, 0.36)], color::WARNING);
    p.cv.fill_rrect(R::new(cx - 0.05 * s, cy - 0.14 * s, 0.10 * s, 0.24 * s), 0.05 * s, color::CARBON);
    p.cv.fill_circle(cx, cy + 0.22 * s, 0.055 * s, color::CARBON);
}

/// The tab's chip title: its title, else its address, else the shown name of an empty tab.
fn tab_title(tab: &TabInfo) -> String {
    if !tab.title.is_empty() {
        tab.title.clone()
    } else if !tab.url.is_empty() && tab.url != "about:blank" {
        tab.url.clone()
    } else {
        "New tab".to_owned()
    }
}

fn host_of(url: &str) -> String {
    Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_else(|| url.to_owned())
}

/// A byte offset cut back to a character boundary of `s`.
fn boundary(s: &str, at: usize) -> usize {
    let mut at = at.min(s.len());
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The field: the active tab, so the address is what its capsule holds. Typing shows the whole address from its left
/// end with the caret and the selection; not typing shows the display of the address - what
/// [`crate::address::field_display`] fits in this capsule - centred, its registrable domain in the full ink. The accent is
/// the row's own: the focus ring while typing, the loading line along the capsule's bottom edge.
#[allow(clippy::too_many_arguments)]
fn field(p: &mut Paint, st: &State, l: &BarLayout, t: R, a: &Adapt, scroll: &mut f32, caret: bool, k: f32) {
    let Some(cap) = l.field_capsule() else { return };
    let cap = R::new(dp(&cap, k).x + st.shake, dp(&cap, k).y, dp(&cap, k).w, dp(&cap, k).h);
    let cy = t.cy();
    let main = ty::LEAD;
    if st.typing {
        p.cv.stroke_rrect(cap, radius::PILL, 1.4, color::ACCENT);
    }
    p.cv.push_clip(t, 0.0);
    if st.typing {
        let text = &st.address;
        let (cur, anc) = (boundary(text, st.cursor), boundary(text, st.anchor));
        let caret_x = p.measure(Font::Regular, main, &text[..cur]);
        let view = (t.w - 12.0).max(1.0);
        let q = |v: f32| (v * 100.0).round() as i32;
        *scroll = browser_rules::scroll_for_caret(q(*scroll), q(caret_x), q(view)) as f32 / 100.0;
        let x = t.x + 6.0 - *scroll;
        if cur != anc {
            let (lo, hi) = (cur.min(anc), cur.max(anc));
            let (xa, xb) = (p.measure(Font::Regular, main, &text[..lo]), p.measure(Font::Regular, main, &text[..hi]));
            p.cv.fill_rrect(R::new(x + xa, cy - 10.0, xb - xa, 20.0), 3.0, color::ACCENT.alpha(0.45));
        }
        if text.is_empty() {
            text_left(p, Font::Regular, main, t.x + 6.0, cy, a.ink3, &st.placeholder);
        } else {
            text_left(p, Font::Regular, main, x, cy, a.ink, text);
        }
        if caret {
            p.cv.fill_rrect(R::new(x + caret_x, cy - 10.0, 1.5, 20.0), 0.7, a.ink);
        }
    } else {
        *scroll = 0.0;
        let display = crate::address::field_display(&st.address, st.domain.as_deref(), (t.w - 12.0).max(1.0), |s| p.measure(Font::Regular, main, s));
        let (before, strong, after) = crate::address::field_pieces(&display);
        if display.text.is_empty() {
            text_centre(p, Font::Regular, main, t.cx(), cy, a.ink3, &st.placeholder);
        } else {
            // The site in the row's own ink and the rest of the address - scheme, subdomain, path - at 62 % of it: one
            // line, but the eye lands on the part that says which page this is. With nothing to emphasise (no host) the
            // whole line keeps the one ink it always had.
            let rest = if display.domain.is_none() { a.ink } else { a.ink2 };
            let runs = [(before, rest), (strong, a.ink), (after, rest)];
            let widths = [before, strong, after].map(|s| p.measure(Font::Regular, main, s).max(0.0));
            let mut x = t.cx() - widths.iter().sum::<f32>() / 2.0;
            for ((run, ink), w) in runs.iter().zip(widths) {
                if !run.is_empty() {
                    text_left(p, Font::Regular, main, x, cy, *ink, run);
                }
                x += w;
            }
        }
    }
    p.cv.pop_clip();
    if st.loading {
        // The engine says only whether the page is loading (`News::Loading`), not how far: the line sweeps until it does.
        // The kit's clip takes the radius as it is: a PILL's 999 is wider than the capsule and clips the row away
        // whole, so the capsule's own radius is what the line is clipped by.
        p.cv.push_clip(cap, cap.h / 2.0);
        let w = (cap.w / 3.0).max(8.0);
        let x = cap.x + (cap.w - w) * ((p.now % SWEEP) / SWEEP) as f32;
        p.cv.fill_rrect(R::new(x, cap.bottom() - PROGRESS, w, PROGRESS), 1.0, color::ACCENT);
        p.cv.pop_clip();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onebar::BarInput;

    fn page(w: usize, h: usize, f: impl Fn(usize, usize) -> u32) -> Vec<u32> {
        (0..w * h).map(|i| f(i % w, i / w)).collect()
    }

    fn bar(tabs: usize, active: usize, editing: bool) -> BarLayout {
        BarLayout::new(&BarInput { width: 400.0, scale: 1.0, left_inset: 0.0, tabs, active, editing, can_back: true, can_forward: true })
    }

    /// A row wide enough to keep every part of itself: the drawings read best from it.
    fn wide(tabs: usize, active: usize, editing: bool) -> BarLayout {
        BarLayout::new(&BarInput { width: 900.0, scale: 1.0, left_inset: 0.0, tabs, active, editing, can_back: true, can_forward: true })
    }

    /// Three tabs, the middle one read, the row not being edited: the everyday shape.
    fn layout() -> BarLayout {
        bar(3, 1, false)
    }

    fn state() -> State {
        State {
            tabs: (0..3).map(|i| TabInfo { id: i, title: format!("Tab {i}"), url: format!("https://rs.example/{i}"), loading: false, registrable_domain: None }).collect(),
            active: 1,
            ..State::default()
        }
    }

    /// The load's line, `one-bar.md`'s fifth acceptance picture: 2 dp of accent along the capsule's bottom edge, clipped
    /// to the capsule. It is the kit's own clip that bites here - a PILL radius wider than the capsule clips the row away
    /// whole - so the drawn pixels, not the arithmetic, are what this checks.
    #[test]
    fn a_load_inks_the_accent_along_the_capsules_bottom_edge() {
        let l = wide(3, 1, false);
        let cap = l.field_capsule().unwrap();
        let accent = color::ACCENT.xrgb() & 0xff_ffff;
        let scan = |g: &GlassChrome| {
            let mut n = 0;
            let mut x = cap.x + 2.0;
            while x < cap.right() - 2.0 {
                if g.pixel(x as u32, (cap.bottom() - 1.0) as u32) & 0xff_ffff == accent {
                    n += 1;
                }
                x += 1.0;
            }
            n
        };
        let mut g = GlassChrome::new();
        g.frame(&State { loading: true, ..state() }, &l);
        assert!(scan(&g) >= 8, "the sweep is at least its 8 px minimum wide, and it is inside the capsule");
        g.frame(&State { loading: false, ..state() }, &l);
        assert_eq!(scan(&g), 0, "a page that has loaded has no line");
    }

    /// How far a pixel sits from a colour, in any channel.
    fn far(v: u32, bg: u32) -> bool {
        let ch = |a: u32, s: u32| (a >> s) & 255;
        ch(v, 16).abs_diff(ch(bg, 16)) > 24 || ch(v, 8).abs_diff(ch(bg, 8)) > 24 || ch(v, 0).abs_diff(ch(bg, 0)) > 24
    }

    /// How many pixels of a box are not the colour around it.
    fn inked(g: &GlassChrome, r: R, bg: u32) -> usize {
        let mut y = r.y;
        let mut n = 0;
        while y < r.bottom() {
            let mut x = r.x;
            while x < r.right() {
                if far(g.pixel(x as u32, y as u32), bg) {
                    n += 1;
                }
                x += 1.0;
            }
            y += 1.0;
        }
        n
    }

    /// A placed part of the row, in the points the canvas is drawn in.
    fn box_of(b: &Placed) -> R {
        R::new(b.x, b.y, b.w, b.h)
    }

    #[test]
    fn the_band_is_even_and_a_row_tall() {
        for k in [1.0, 1.25, 1.4, 1.5, 2.0] {
            let mut i = BarInput { width: 400.0, scale: k, left_inset: 0.0, tabs: 2, active: 0, editing: false, can_back: true, can_forward: true };
            let (w, h) = band_px(&BarLayout::new(&i));
            assert!(w % 2 == 0 && h % 2 == 0, "{w}x{h} at {k}");
            assert!(w as f32 >= i.width * k - 1.0 && h as f32 >= 44.0 * k - 1.0, "{w}x{h} at {k}");
            i.tabs = 12;
            i.active = 5;
            i.editing = true;
            assert_eq!(band_px(&BarLayout::new(&i)).1 % 2, 0, "editing with twelve tabs at {k}");
        }
    }

    #[test]
    fn one_row_in_every_state_and_nothing_hangs_below_it() {
        for (tabs, active, editing) in [(1, 0, false), (3, 1, false), (12, 5, false), (3, 1, true), (0, 0, true)] {
            let l = bar(tabs, active, editing);
            assert_eq!(l.band_h, 44.0, "{tabs} tabs, editing {editing}");
            assert_eq!(band_px(&l).1, 44, "criterion 7: the height told to the engine");
            for b in &l.parts {
                assert!(b.bottom() <= l.band_h + 0.01, "{:?} hangs below the row", b.part);
                assert!(b.right() <= l.width_px + 0.01, "{:?} runs past the right edge", b.part);
            }
        }
    }

    #[test]
    fn the_field_sits_in_tab_order_and_on_the_capsule_line() {
        let l = wide(3, 1, false);
        let x = |pt: Part| l.placed(pt).expect("placed").x;
        assert!(x(Part::Back) < x(Part::Tab(0)) && x(Part::Tab(0)) < x(Part::Tab(2)) && x(Part::Tab(2)) < x(Part::NewTab));
        assert!(x(Part::NewTab) < x(Part::Star) && x(Part::Star) < x(Part::Menu));
        let cap = l.field_capsule().expect("a field");
        assert!(cap.x > x(Part::Tab(0)) && cap.x < x(Part::Tab(2)), "the field stands where the read tab would be");
        assert_eq!(cap.y, l.capsule_y, "and centred in the row, not sitting on the baseline");
        assert_eq!(cap.h, 30.0);
    }

    #[test]
    fn unchanged_rows_cost_no_recompute_and_changed_rows_do() {
        let l = layout();
        let k = 1.0;
        let (w, h) = band_px(&l);
        let mut b = Backdrop::new();
        let p1 = page(400, 200, |x, y| ((x * 3 + y) % 256) as u32 * 0x010101);
        assert!(b.sample(&p1, 400, 200, w, h, k), "the first page is a change");
        b.refresh(k);
        assert_eq!((b.changes, b.blurs), (1, 1));
        for _ in 0..50 {
            assert!(!b.sample(&p1, 400, 200, w, h, k), "same rows");
            assert_eq!(b.refresh(k), None, "and no blur");
        }
        assert_eq!((b.changes, b.blurs), (1, 1), "50 identical frames: still one recompute");
        // Rows far below the band change: the band does not look at them.
        let mut p2 = p1.clone();
        for v in &mut p2[400 * (h as usize + 5)..] {
            *v ^= 0xffffff;
        }
        assert!(!b.sample(&p2, 400, 200, w, h, k), "rows the band does not read changed nothing");
        // A row it does read changes it.
        let mut p3 = p1.clone();
        p3[10 * 400 + 100] ^= 0xffffff;
        assert!(b.sample(&p3, 400, 200, w, h, k));
        b.refresh(k);
        assert_eq!((b.changes, b.blurs), (2, 2));
        // A different band size is a change (the sample positions moved).
        assert!(b.sample(&p3, 400, 200, w + 2, h, k));
    }

    #[test]
    fn the_backdrop_mirrors_the_top_rows_about_the_seam() {
        let l = layout();
        let (w, h) = band_px(&l);
        let mut b = Backdrop::new();
        // Page row r is the colour r, so the band's bottom row must show the page's first row.
        let p = page(400, 300, |_, y| y as u32);
        b.sample(&p, 400, 300, w, h, 1.0);
        let src = b.src.as_ref().unwrap();
        let hh = src.h as usize;
        assert_eq!(src.px[(hh - 1) * src.w as usize], (h as usize - 2 - (hh - 1) * 2) as u32, "bottom sample row is page row 0");
        assert!(src.px[0] > src.px[(hh - 1) * src.w as usize], "going up the band goes deeper into the page");
        // A page shorter than the band is clamped, never read outside.
        let short = page(400, 3, |_, y| y as u32);
        let mut b2 = Backdrop::new();
        assert!(b2.sample(&short, 400, 3, w, h, 1.0));
    }

    #[test]
    fn no_page_is_a_state_of_its_own_and_is_only_a_change_once() {
        let l = layout();
        let (w, h) = band_px(&l);
        let mut b = Backdrop::new();
        assert!(b.sample(&[], 0, 0, w, h, 1.0), "the first sight of 'no page'");
        assert!(!b.sample(&[], 0, 0, w, h, 1.0));
        assert!(b.for_luma().is_none() && b.for_glass().is_none());
        let p = page(400, 100, |_, _| 0xffffff);
        assert!(b.sample(&p, 400, 100, w, h, 1.0), "then a page");
    }

    #[test]
    fn an_unchanged_frame_is_not_drawn_again() {
        let l = layout();
        let k = 1.0;
        let (w, h) = band_px(&l);
        let mut g = GlassChrome::new();
        let p = page(400, 200, |_, _| 0xf5f5f0);
        g.backdrop.sample(&p, 400, 200, w, h, k);
        let st = State { tabs: vec![TabInfo { id: 1, title: "A page".into(), url: "https://example.org/".into(), loading: false, registrable_domain: Some("example.org".into()) }], active: 1, address: "https://example.org/".into(), domain: Some("example.org".into()), ..State::default() };
        assert!(g.frame(&st, &l).redrawn);
        for _ in 0..10 {
            g.backdrop.sample(&p, 400, 200, w, h, k);
            assert!(!g.frame(&st, &l).redrawn);
        }
        assert_eq!((g.draws, g.cached, g.backdrop.blurs), (1, 10, 1));
        // A change of what is shown draws again.
        let mut st2 = st.clone();
        st2.loading = true;
        assert!(g.frame(&st2, &l).redrawn);
        assert_eq!(g.draws, 2);
        // The emphasised part of the address is part of what is shown too: it is in `State`, which is the frame's key.
        let mut st3 = st.clone();
        st3.domain = None;
        assert_ne!(st3, st);
    }

    /// a refusal's shake moves the field's capsule and its flash inks the edge red, both from `State`.
    #[test]
    fn a_refusal_pushes_the_field_sideways_and_flashes_its_edge() {
        let l = wide(3, 1, false);
        let cap = l.field_capsule().unwrap();
        let k = 1.0;
        let (w, h) = band_px(&l);
        let p = page(400, 200, |_, _| 0xf5f5f0);
        let mut g = GlassChrome::new();
        g.backdrop.sample(&p, 400, 200, w, h, k);
        let calm = State { address: "https://example.org/".into(), ..State::default() };
        g.frame(&calm, &l);
        let edge = (cap.x as u32 + 2, cap.cy() as u32);
        let top = (cap.cx() as u32, cap.y as u32 + 1);
        let (calm_edge, calm_top) = (g.pixel(edge.0, edge.1), g.pixel(top.0, top.1));
        let redness = |px: u32| ((px >> 16) & 0xff) as i32 - (px & 0xff) as i32;
        let pushed = State { shake: 6.0, ..calm.clone() };
        assert!(g.frame(&pushed, &l).redrawn, "the shake is part of what the frame shows");
        assert_ne!(g.pixel(edge.0, edge.1), calm_edge, "the capsule's left edge moved right");
        let flashing = State { flash: 1.0, ..calm.clone() };
        assert!(g.frame(&flashing, &l).redrawn);
        assert!(redness(g.pixel(top.0, top.1)) > redness(calm_top) + 40, "the capsule's top edge is red");
    }

    #[test]
    fn a_bookmarked_page_fills_the_star_and_a_plain_one_only_outlines_it() {
        let l = wide(3, 1, false);
        let k = 1.0;
        let (w, h) = band_px(&l);
        let p = page(400, 200, |_, _| 0xf5f5f0);
        let star = l.placed(Part::Star).expect("a 400 dp row keeps the star");
        // How much of the star's box is ink, against the box's own corner (empty of the icon).
        let mut g = GlassChrome::new();
        g.backdrop.sample(&p, 400, 200, w, h, k);
        let plain = State { address: "https://example.org/".into(), ..State::default() };
        g.frame(&plain, &l);
        let bg = g.pixel(star.x as u32 + 1, star.y as u32 + 1);
        let outline = inked(&g, box_of(&star), bg);
        let marked = State { bookmarked: true, ..plain.clone() };
        assert!(g.frame(&marked, &l).redrawn, "the star is part of what the frame shows");
        let filled = inked(&g, box_of(&star), bg);
        assert!(outline > 0, "the outline star draws something");
        assert!(filled > outline, "a filled star covers more of the box than its outline: {filled} vs {outline}");
    }

    #[test]
    fn the_bar_follows_the_page_light_page_light_glass_dark_page_dark_glass() {
        let l = wide(3, 1, false);
        let k = 1.0;
        let (w, h) = band_px(&l);
        let lum = |g: &GlassChrome, x: u32, y: u32| {
            let v = g.pixel(x, y);
            (0.2126 * ((v >> 16) & 255) as f32 + 0.7152 * ((v >> 8) & 255) as f32 + 0.0722 * (v & 255) as f32) / 255.0
        };
        let mut shades = Vec::new();
        for c in [0xf5f5f0u32, 0x101418] {
            let mut g = GlassChrome::new();
            let p = page(400, 200, |_, _| c);
            g.backdrop.sample(&p, 400, 200, w, h, k);
            let st = State::default();
            g.frame(&st, &l);
            // A spot of the tab strip's capsule away from controls (its right part).
            let cap = l.field_capsule().expect("a field at 400 dp");
            shades.push(lum(&g, cap.x as u32 + 60, cap.cy() as u32));
        }
        assert!(shades[0] > 0.6 && shades[1] < 0.3, "{shades:?}");
    }

    #[test]
    fn a_blur_over_budget_falls_back_to_a_plain_capsule() {
        let l = layout();
        let (w, h) = band_px(&l);
        let mut b = Backdrop::new();
        let p = page(400, 100, |_, _| 0xaaaaaa);
        b.sample(&p, 400, 100, w, h, 1.0);
        b.refresh(1.0);
        assert!(b.for_glass().is_some() || b.last_blur_ms().is_some_and(|m| m > BLUR_BUDGET_MS));
        // Force the slow case: the next page changes are not blurred, the old blur is not shown.
        b.last_blur_ms = Some(BLUR_BUDGET_MS * 3.0);
        assert!(b.for_glass().is_none(), "over budget: no glass");
        let mut p2 = p.clone();
        p2[5] ^= 0xffffff;
        b.sample(&p2, 400, 100, w, h, 1.0);
        let blurs = b.blurs;
        b.refresh(1.0);
        assert_eq!(b.blurs, blurs, "skipped while over budget");
        assert!(b.for_luma().is_some(), "but the luminance is still read from the samples");
    }

    #[test]
    fn a_full_tab_shows_its_close_mark_only_under_the_pointer() {
        let l = wide(3, 1, false);
        let (w, h) = band_px(&l);
        let slot = l.placed(Part::Tab(0)).expect("tab 0 keeps a full slot");
        assert!(!slot.compact, "an inactive tab is full while the row has room");
        let mut g = GlassChrome::new();
        g.backdrop.sample(&page(400, 200, |_, _| 0xf5f5f0), 400, 200, w, h, 1.0);
        // The last 30 dp of the slot holds the close mark, past where the title is cut.
        let end = R::new(slot.right() - 30.0, slot.y, 30.0, slot.h);
        g.frame(&state(), &l);
        let bg = g.pixel(slot.x as u32 + 1, slot.y as u32 + 1);
        let quiet = inked(&g, end, bg);
        let mut hovered = state();
        hovered.close_hover = Some(0);
        assert!(g.frame(&hovered, &l).redrawn, "the pointer is part of what the frame shows");
        assert!(inked(&g, end, bg) > quiet, "the close mark appears only under the pointer");
    }
}
