//! The browser's bars: tab strip and toolbar on top (Juno, Diana tablet), address pill and dock at the bottom (Diana phone,
//! within thumb reach). All glass, all controls; the page is the content.

use lucina_glass::canvas::{Color, R};
use lucina_glass::text::Font;
use lucina_glass::tokens::{color, radius, ty};
use lucina_glass::widgets::adaptive::adaptive_bar;
use lucina_glass::widgets::text_field::{FieldMetrics, TextField};
use lucina_glass::widgets::{Adapt, TabChip};
use lucina_glass::Paint;

use crate::icons::I;
use crate::ui::*;

pub struct Tab {
    pub title: &'static str,
    pub host: &'static str,
    pub loading: bool,
}

pub const TABS: [Tab; 5] = [
    Tab { title: "Example Domain", host: "example.org", loading: false },
    Tab { title: "Delivery", host: "shop.example.net", loading: false },
    Tab { title: "Release notes", host: "notes.example.org", loading: true },
    Tab { title: "Timetable", host: "transit.example.dk", loading: false },
    Tab { title: "Guidelines", host: "docs.example.org", loading: false },
];

/// Where the pieces sit, in dp.
pub struct Geo {
    pub tabs_row: R,
    pub toolbar: R,
    pub field: R,
    pub back: f32,
    pub forward: f32,
    pub reload: f32,
    pub star: f32,
    pub dl: f32,
    pub menu: f32,
    pub tabs_btn: f32,
    pub lock: f32,
    /// Phone only.
    pub dock: R,
    /// Where the page's content may start (the bars end), dp.
    pub page_top: f32,
}

pub const M: f32 = 12.0;

pub fn geo(cfg: &Cfg) -> Geo {
    let ctl = cfg.ctl();
    if cfg.phone() {
        let dock = R::new(M, cfg.h - GESTURE - 56.0, cfg.w - 2.0 * M, 56.0);
        let field = R::new(M, dock.y - 8.0 - 48.0, cfg.w - 2.0 * M, 48.0);
        let at = |i: f32| dock.x + dock.w * (i + 0.5) / 5.0;
        return Geo {
            tabs_row: R::default(),
            toolbar: R::default(),
            field,
            back: at(0.0),
            forward: at(1.0),
            reload: field.right() - 26.0,
            star: 0.0,
            dl: 0.0,
            menu: at(4.0),
            tabs_btn: at(3.0),
            lock: field.cx(),
            dock,
            page_top: STATUS,
        };
    }
    let tabs_row = R::new(M, 10.0, cfg.w - 2.0 * M, ctl + 8.0);
    let toolbar = R::new(M, tabs_row.bottom() + 8.0, cfg.w - 2.0 * M, ctl + 12.0);
    let cy = toolbar.cy();
    let back = toolbar.x + 8.0 + ctl / 2.0;
    let forward = back + ctl + 2.0;
    let reload = forward + ctl + 2.0;
    let menu = toolbar.right() - 8.0 - ctl / 2.0;
    let dl = menu - ctl - 2.0;
    let x0 = reload + ctl / 2.0 + 8.0;
    let x1 = dl - ctl / 2.0 - 8.0;
    let field = R::new(x0, cy - ctl / 2.0, x1 - x0, ctl);
    Geo {
        tabs_row,
        toolbar,
        field,
        back,
        forward,
        reload,
        star: field.right() - ctl / 2.0 - 2.0,
        dl,
        menu,
        tabs_btn: tabs_row.right() - 6.0 - ctl / 2.0,
        lock: field.x + 8.0 + ctl / 2.0 - 4.0,
        dock: R::default(),
        page_top: toolbar.bottom() + 10.0,
    }
}

/// What the bars show.
pub struct St<'a> {
    pub active: usize,
    pub host: &'a str,
    pub path: &'a str,
    /// The address field has the keys, with this text.
    pub typing: Option<&'a str>,
    pub download_ring: Option<f32>,
    pub asking_location: bool,
    pub lock_open: bool,
    pub menu_open: bool,
    pub bookmarked: bool,
    pub hide_dock: bool,
    /// A page of the browser itself (lucina://): a gear instead of a lock.
    pub internal: bool,
}

impl Default for St<'_> {
    fn default() -> Self {
        St {
            active: 0,
            host: "example.org",
            path: "/",
            typing: None,
            download_ring: None,
            asking_location: false,
            lock_open: false,
            menu_open: false,
            bookmarked: false,
            hide_dock: false,
            internal: false,
        }
    }
}

pub fn draw(p: &mut Paint, cfg: &Cfg, g: &Geo, st: &St) {
    if cfg.phone() { phone(p, cfg, g, st) } else { top(p, cfg, g, st) }
}

const LOCK: f32 = 20.0;

fn top(p: &mut Paint, cfg: &Cfg, g: &Geo, st: &St) {
    let ctl = cfg.ctl();
    // Tab strip.
    let a = adaptive_bar(p, g.tabs_row, radius::PILL);
    let cy = g.tabs_row.cy();
    let n = TABS.len() as f32;
    let avail = g.tabs_row.w - 8.0 - (ctl + 4.0) - (ctl + 8.0);
    let tw = TabChip::width(avail, TABS.len(), 4.0, if cfg.touch() { 230.0 } else { 210.0 });
    let tab_ty = if cfg.touch() { ty::LEAD } else { ty::BODY };
    for (i, t) in TABS.iter().enumerate() {
        let r = R::new(g.tabs_row.x + 4.0 + i as f32 * (tw + 4.0), cy - ctl / 2.0, tw, ctl);
        let mut chip = TabChip::new(t.title, t.host);
        chip.active = i == st.active;
        chip.loading = t.loading;
        chip.draw(p, r, tab_ty, &a.palette());
    }
    let plus_x = g.tabs_row.x + 4.0 + n * (tw + 4.0) + ctl / 2.0;
    bar_btn(p, plus_x, cy, ctl, I::Plus, a.ink2, false);
    // Overview: the tabs icon with the count inside.
    ic(p, I::Tabs, g.tabs_btn, cy, ctl * 0.62, a.ink);
    text_c(p, Font::Semibold, ty::SMALL, g.tabs_btn, cy + 0.5, a.ink, &format!("{}", TABS.len()));

    // Toolbar.
    let a = adaptive_bar(p, g.toolbar, radius::PILL);
    let cy = g.toolbar.cy();
    bar_btn(p, g.back, cy, ctl, I::Back, a.ink, false);
    bar_btn(p, g.forward, cy, ctl, I::Forward, a.ink3, false);
    bar_btn(p, g.reload, cy, ctl, I::Reload, a.ink, false);
    field(p, cfg, g, st, &a);
    if let Some(f) = st.download_ring {
        p.cv.stroke_circle(g.dl, cy, ctl * 0.4, 1.8, a.ink3);
        p.cv.arc(g.dl, cy, ctl * 0.4, 1.8, -std::f32::consts::FRAC_PI_2, f * std::f32::consts::TAU, color::ACCENT);
    }
    bar_btn(p, g.dl, cy, ctl, I::Download, a.ink, false);
    bar_btn(p, g.menu, cy, ctl, I::Dots, a.ink, st.menu_open);
}

fn field(p: &mut Paint, cfg: &Cfg, g: &Geo, st: &St, a: &Adapt) {
    let f = g.field;
    let ctl = cfg.ctl();
    let main = cfg.main();
    if let Some(t) = st.typing {
        let mut tf = TextField::new(t);
        tf.focus(p.now);
        tf.draw(p, f, &FieldMetrics::bar(ctl, main), &a.palette());
        return;
    }
    p.cv.fill_rrect(f, radius::PILL, a.ink.alpha(0.09));
    p.cv.stroke_rrect(f, radius::PILL, 0.7, a.ink.alpha(0.16));
    let cy = f.cy();
    if st.lock_open {
        p.cv.fill_circle(g.lock, cy, ctl / 2.0 - 2.0, color::ACCENT_SOFT);
    }
    ic(p, if st.internal { I::Gear } else { I::Lock }, g.lock, cy, LOCK, a.ink2);
    let mut x = g.lock + ctl / 2.0 + 4.0;
    if st.asking_location {
        p.cv.fill_circle(x + 12.0, cy, ctl / 2.0 - 3.0, color::ACCENT_SOFT);
        ic(p, I::Pin, x + 12.0, cy, 20.0, a.ink);
        x += ctl - 2.0;
    }
    let w = text_l(p, Font::Regular, main, x, cy, a.ink, st.host);
    text_l(p, Font::Regular, main, x + w, cy, a.ink2, st.path);
    let (i, c) = if st.bookmarked { (I::StarFill, color::ACCENT) } else { (I::Star, a.ink2) };
    bar_btn(p, g.star, cy, ctl, i, c, false);
}

fn phone(p: &mut Paint, cfg: &Cfg, g: &Geo, st: &St) {
    let f = g.field;
    let a = adaptive_bar(p, f, radius::PILL);
    let cy = f.cy();
    let main = cfg.main();
    if let Some(t) = st.typing {
        let mut tf = TextField::new(t);
        tf.focus(p.now);
        tf.draw(p, f, &FieldMetrics::pill(main), &a.palette());
        return;
    }
    // The address centred: lock and host; the path is left out on a phone.
    let host = st.host;
    let w = p.measure(Font::Regular, main, host);
    let total = LOCK + 8.0 + w + if st.asking_location { 32.0 } else { 0.0 };
    let mut x = f.cx() - total / 2.0 + 10.0;
    if st.lock_open {
        p.cv.fill_circle(x + LOCK / 2.0, cy, 16.0, color::ACCENT_SOFT);
    }
    ic(p, if st.internal { I::Gear } else { I::Lock }, x + LOCK / 2.0, cy, LOCK, a.ink2);
    x += LOCK + 6.0;
    if st.asking_location {
        p.cv.fill_circle(x + 12.0, cy, 15.0, color::ACCENT_SOFT);
        ic(p, I::Pin, x + 12.0, cy, 20.0, a.ink);
        x += 32.0;
    }
    text_l(p, Font::Regular, main, x, cy, a.ink, host);
    bar_btn(p, g.reload, cy, 44.0, I::Reload, a.ink2, false);
    if st.hide_dock {
        return;
    }
    let a = adaptive_bar(p, g.dock, radius::PILL);
    let cy = g.dock.cy();
    bar_btn(p, g.back, cy, 44.0, I::Back, a.ink, false);
    bar_btn(p, g.forward, cy, 44.0, I::Forward, a.ink3, false);
    bar_btn(p, g.dock.x + g.dock.w * 2.5 / 5.0, cy, 44.0, I::Plus, a.ink, false);
    ic(p, I::Tabs, g.tabs_btn, cy, 28.0, a.ink);
    text_c(p, Font::Semibold, ty::SMALL, g.tabs_btn, cy + 0.5, a.ink, &format!("{}", TABS.len()));
    bar_btn(p, g.menu, cy, 44.0, I::Dots, a.ink, st.menu_open);
}

pub fn dim(c: f32) -> Color {
    Color::hexa(0x000000, (c * 255.0) as u8)
}
