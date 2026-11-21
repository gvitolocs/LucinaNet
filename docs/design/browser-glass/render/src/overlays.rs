//! Popovers (Juno, tablet) and bottom sheets (phone) with their content: menu, address suggestions, permission prompt, site
//! info, downloads. The content functions draw into the rectangle the container gives them.

use lucina_glass::canvas::R;
use lucina_glass::text::Font;
use lucina_glass::tokens::{color, radius};
use lucina_glass::widgets::{BottomSheet, ListRow, Palette, Popover, ProgressBar, Stepper};
use lucina_glass::Paint;

use crate::chrome::{Geo, M};
use crate::icons::I;
use crate::ui::*;

/// The container of `h` dp of content: a popover under the toolbar at `anchor` (top layouts) or a bottom sheet (phone).
/// Returns the content rectangle.
pub fn container(p: &mut Paint, cfg: &Cfg, g: &Geo, anchor: f32, w: f32, h: f32) -> R {
    if cfg.phone() {
        let r = BottomSheet::place(cfg.w, cfg.h, h);
        let mut sheet = BottomSheet::new();
        sheet.open(p.now - 10.0);
        sheet.draw(p, r, cfg.h).expect("open")
    } else {
        let r = Popover::place(anchor + 18.0, g.toolbar.bottom() + 6.0, w, h, M, cfg.w);
        let mut pop = Popover::new();
        pop.open(p.now - 10.0);
        pop.draw(p, r).expect("open")
    }
}

/// A row: optional icon, title, optional value and chevron; `selected` is the one accent.
#[allow(clippy::too_many_arguments)]
pub fn list_row(p: &mut Paint, cfg: &Cfg, r: R, icon: Option<I>, title: &str, value: Option<&str>, chevron: bool, selected: bool) {
    let mut row = ListRow::new(icon, title).selected(selected);
    row.value = value.map(str::to_string);
    row.chevron = chevron;
    row.draw(p, r, cfg.main(), &Palette::STANDARD);
}

fn sep(p: &mut Paint, r: R, y: &mut f32) {
    divider(p, r.x + 4.0, r.right() - 4.0, *y + 4.0);
    *y += 9.0;
}

// ---- menu ----

pub fn menu_h(cfg: &Cfg) -> f32 {
    76.0 + 9.0 + cfg.row() + 9.0 + 2.0 * cfg.row()
}

pub fn menu(p: &mut Paint, cfg: &Cfg, r: R) {
    let mut y = r.y;
    for (i, (icon, label)) in [(I::Search, "Find"), (I::Star, "Bookmarks"), (I::Clock, "History"), (I::Download, "Downloads")].iter().enumerate() {
        action(p, r.x + r.w * (i as f32 + 0.5) / 4.0, y + 4.0, *icon, label);
    }
    y += 76.0;
    sep(p, r, &mut y);
    // Zoom stepper.
    let row = R::new(r.x, y, r.w, cfg.row());
    Stepper::new("Zoom", 110, 50, 300, 10, "%").draw(p, row, cfg.main(), cfg.ctl());
    y += cfg.row();
    sep(p, r, &mut y);
    list_row(p, cfg, R::new(r.x, y, r.w, cfg.row()), Some(I::Private), "New private window", None, false, false);
    y += cfg.row();
    list_row(p, cfg, R::new(r.x, y, r.w, cfg.row()), Some(I::Gear), "Settings", None, false, false);
}

// ---- address suggestions ----

pub fn sugg_rh(cfg: &Cfg) -> f32 {
    if cfg.touch() { 56.0 } else { 48.0 }
}

pub fn sugg_h(cfg: &Cfg) -> f32 {
    4.0 * sugg_rh(cfg) + 2.0 * 28.0 + 16.0
}

pub fn suggestions(p: &mut Paint, cfg: &Cfg, r: R) {
    let rh = sugg_rh(cfg);
    let mut y = r.y + 8.0;
    let rows: [(Option<&str>, I, &str, &str); 4] = [
        (None, I::Search, "lucina", "Search"),
        (Some("Bookmarks"), I::Star, "Lucina docs", "docs.example.org/lucina"),
        (Some("History"), I::Clock, "Lucina release notes", "notes.example.org/lucina"),
        (None, I::Clock, "Lucina design", "example.org/design"),
    ];
    for (i, (head, icon, title, url)) in rows.iter().enumerate() {
        if let Some(h) = head {
            caption(p, r.x + 14.0, y + 14.0, h);
            y += 28.0;
        }
        let row = R::new(r.x + 4.0, y, r.w - 8.0, rh);
        if i == 0 {
            p.cv.fill_rrect(row, radius::MD, color::ACCENT_SOFT);
        }
        ic(p, *icon, row.x + 24.0, row.cy(), 20.0, if i == 0 { color::ACCENT } else { color::INK_2 });
        let tx = row.x + 52.0;
        let room = row.right() - 44.0 - tx;
        let t = p.fit(Font::Regular, cfg.main(), title, room);
        let u = p.fit(Font::Regular, cfg.sub(), url, room);
        text_l(p, Font::Regular, cfg.main(), tx, row.cy() - 9.0, color::INK, &t);
        text_l(p, Font::Regular, cfg.sub(), tx, row.cy() + 11.0, color::INK_2, &u);
        if cfg.touch() {
            ic(p, I::ArrowUpRight, row.right() - 24.0, row.cy(), 16.0, color::INK_2);
        }
        y += rh;
    }
}

// ---- permission prompt ----

pub fn perm_h(cfg: &Cfg) -> f32 {
    16.0 + 48.0 + 14.0 + if cfg.touch() { 48.0 } else { 40.0 } + 6.0
}

pub fn permission(p: &mut Paint, cfg: &Cfg, r: R) {
    let top = r.y + 8.0;
    p.cv.fill_circle(r.x + 30.0, top + 24.0, 24.0, color::ACCENT_SOFT);
    ic(p, I::Pin, r.x + 30.0, top + 24.0, 24.0, color::INK);
    let tx = r.x + 72.0;
    let room = r.right() - tx - 6.0;
    let a = p.fit(Font::Semibold, cfg.main(), "Use your location?", room);
    let b = p.fit(Font::Regular, cfg.sub(), "example.org wants to know where you are", room);
    text_l(p, Font::Semibold, cfg.main(), tx, top + 15.0, color::INK, &a);
    text_l(p, Font::Regular, cfg.sub(), tx, top + 37.0, color::INK_2, &b);
    let bh = if cfg.touch() { 48.0 } else { 40.0 };
    let y = top + 48.0 + 14.0;
    let half = (r.w - 12.0) / 2.0;
    button(p, R::new(r.x + 6.0, y, half - 6.0, bh), "Block", false);
    button(p, R::new(r.x + 6.0 + half + 6.0, y, half - 6.0, bh), "Allow", true);
}

// ---- site info ----

pub fn site_h(cfg: &Cfg) -> f32 {
    64.0 + 9.0 + 28.0 + 4.0 * (cfg.row() + 8.0) + 8.0 + 44.0
}

pub fn site_info(p: &mut Paint, cfg: &Cfg, r: R) {
    let mut y = r.y;
    site_mark(p, r.x + 28.0, y + 32.0, 36.0, "example.org");
    text_l(p, Font::Semibold, cfg.main(), r.x + 60.0, y + 24.0, color::INK, "example.org");
    ic(p, I::Lock, r.x + 68.0, y + 46.0, 18.0, color::POSITIVE);
    text_l(p, Font::Regular, cfg.sub(), r.x + 86.0, y + 46.0, color::INK_2, "Connection is secure");
    y += 64.0;
    sep(p, r, &mut y);
    caption(p, r.x + 14.0, y + 14.0, "Permissions");
    y += 28.0;
    let rows = [
        (I::Camera, "Camera", "Ask", false),
        (I::Pin, "Location", "Allowed", true),
        (I::Bell, "Notifications", "Blocked", false),
        (I::Popup, "Pop-ups", "Blocked", false),
    ];
    for (icon, name, state, on) in rows {
        let row = R::new(r.x, y, r.w, cfg.row() + 8.0);
        ic(p, icon, row.x + 24.0, row.cy(), 20.0, color::INK);
        text_l(p, Font::Regular, cfg.main(), row.x + 52.0, row.cy() - 8.0, color::INK, name);
        text_l(p, Font::Regular, cfg.sub(), row.x + 52.0, row.cy() + 11.0, color::INK_2, state);
        toggle(p, row.right() - 8.0, row.cy(), on);
        y += row.h;
    }
    y += 8.0;
    button(p, R::new(r.x + 4.0, y, r.w - 8.0, 44.0), "Clear site data", false);
}

// ---- downloads ----

pub fn dl_rh(cfg: &Cfg) -> f32 {
    if cfg.touch() { 68.0 } else { 60.0 }
}

pub fn downloads_h(cfg: &Cfg) -> f32 {
    44.0 + 3.0 * dl_rh(cfg) + 10.0
}

pub fn downloads(p: &mut Paint, cfg: &Cfg, r: R) {
    text_l(p, Font::Semibold, cfg.main(), r.x + 14.0, r.y + 22.0, color::INK, "Downloads");
    text_r(p, Font::Semibold, cfg.sub(), r.right() - 14.0, r.y + 22.0, color::INK_2, "Clear list");
    let mut y = r.y + 44.0;
    let rh = dl_rh(cfg);
    // (name, status, fraction, state)
    let rows: [(&str, &str, Option<f32>, u8); 3] = [
        ("report-q3.pdf", "42.1 MB of 120.0 MB", Some(0.35), 0),
        ("photos.zip", "Saved to Downloads", None, 1),
        ("installer.bin", "Network error", None, 2),
    ];
    for (name, status, frac, state) in rows {
        let row = R::new(r.x, y, r.w, rh);
        p.cv.fill_circle(row.x + 26.0, row.cy(), 18.0, color::INK.alpha(0.10));
        ic(p, I::Doc, row.x + 26.0, row.cy(), 18.0, color::INK);
        let tx = row.x + 56.0;
        let room = row.right() - 52.0 - tx;
        let n = p.fit(Font::Regular, cfg.main(), name, room);
        let sc = if state == 2 { color::LIVE } else { color::INK_2 };
        let by = if frac.is_some() { row.cy() - 12.0 } else { row.cy() - 9.0 };
        text_l(p, Font::Regular, cfg.main(), tx, by, color::INK, &n);
        let s = p.fit(Font::Regular, cfg.sub(), status, room);
        text_l(p, Font::Regular, cfg.sub(), tx, by + 20.0, sc, &s);
        if let Some(f) = frac {
            ProgressBar::new((f * 1000.0).round() as u64, 1000).draw(p, R::new(tx, by + 32.0, room, 4.0));
        }
        let icon = match state {
            0 => I::Close,
            1 => I::Folder,
            _ => I::Reload,
        };
        bar_btn(p, row.right() - 24.0, row.cy(), 44.0, icon, color::INK_2, false);
        y += rh;
    }
}
