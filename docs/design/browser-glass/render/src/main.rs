//! Renders the LucinaNet browser chrome mockups: `browser-glass-render <out dir> [juno|phone|tablet]`.

mod chrome;
mod icons;
mod overlays;
mod page;
mod ui;

use std::path::Path;

use lucina_glass::canvas::{Canvas, Color, R};
use lucina_glass::screens::home_indicator;
use lucina_glass::text::{Font, Type};
use lucina_glass::tokens::{color, glass, radius, space, ty as tys};
use lucina_glass::widgets::chrome::status_bar;
use lucina_glass::widgets::{DialogCard, TabCard};
use lucina_glass::Paint;

use chrome::{geo, Geo, St, M, TABS};
use icons::I;
use overlays::*;
use page::Kind;
use ui::*;

const PAGE_INK: Color = Color::hex(0x1c1c1e);

/// Dim `base` over `area`, blur it for the glass, run `f` on the result.
fn compose(cfg: &Cfg, ty: &mut Type, mut base: Canvas, dim: Option<(R, f32)>, f: impl FnOnce(&mut Paint)) -> Canvas {
    if let Some((r, a)) = dim {
        base.fill_rrect(r, 0.0, chrome::dim(a));
    }
    let blurred = base.blurred((glass::BLUR * cfg.k / 3.0).round().max(1.0) as u32);
    let mut cv = base;
    {
        let mut p = Paint { cv: &mut cv, ty, now: 1.0, backdrop: Some(&blurred) };
        f(&mut p);
    }
    cv
}

fn full(cfg: &Cfg, a: f32) -> Option<(R, f32)> {
    Some((R::new(0.0, 0.0, cfg.w, cfg.h), a))
}

fn phone_furniture(p: &mut Paint, cfg: &Cfg, light_ink: bool) {
    if cfg.phone() {
        let ink = if light_ink { color::INK } else { PAGE_INK };
        status_bar(p, R::new(0.0, 0.0, cfg.w, STATUS), ink);
        home_indicator(p.cv, ink);
    }
}

fn article(cfg: &Cfg, ty: &mut Type, g: &Geo) -> page::Page {
    page::draw(cfg, ty, &Kind::Article("Example Domain"), g.page_top)
}

// 1 ---------------------------------------------------------------------------------------------------------------

fn browse_on(cfg: &Cfg, ty: &mut Type, dark: bool) -> Canvas {
    let g = geo(cfg);
    let kind = if dark { Kind::ArticleDark("Example Domain") } else { Kind::Article("Example Domain") };
    let pg = page::draw(cfg, ty, &kind, g.page_top);
    compose(cfg, ty, pg.cv, None, |p| {
        chrome::draw(p, cfg, &g, &St { path: "/", ..St::default() });
        phone_furniture(p, cfg, dark);
    })
}

fn browse(cfg: &Cfg, ty: &mut Type) -> Canvas {
    browse_on(cfg, ty, false)
}

/// The same screen over a light and over a dark page, side by side (scaled to `width` px each).
fn light_dark(cfg: &Cfg, ty: &mut Type) -> Canvas {
    let width = if cfg.phone() { 540.0 } else { 1200.0 };
    let s = width / (cfg.w * cfg.k);
    let h = (cfg.h * cfg.k * s).round();
    let gap = 16.0;
    let mut out = Canvas::new((2.0 * width + gap) as u32, h as u32, 1.0);
    out.clear(color::VOID);
    for (i, dark) in [false, true].into_iter().enumerate() {
        let shot = browse_on(cfg, ty, dark);
        out.blit(&shot, R::new(i as f32 * (width + gap), 0.0, width, h), 0.0, 1.0);
    }
    out
}

// 2 ---------------------------------------------------------------------------------------------------------------

fn tab_switcher(cfg: &Cfg, ty: &mut Type) -> Canvas {
    let g = geo(cfg);
    let kinds = [Kind::Article("Example Domain"), Kind::Form, Kind::Article("Release notes"), Kind::Article("Timetable"), Kind::Article("Guidelines")];
    let top = if cfg.phone() { STATUS } else { 20.0 };
    let thumbs: Vec<Canvas> = kinds.iter().map(|k| page::draw(cfg, ty, k, top).cv).collect();
    let _ = &g;
    let base = page::draw(cfg, ty, &Kind::Sky, 0.0).cv;
    compose(cfg, ty, base, full(cfg, 0.0), |p| {
        let (cols, side, gap) = match cfg.dev {
            Dev::Juno => (4usize, 32.0, 16.0),
            Dev::Tablet => (3, 32.0, 16.0),
            Dev::Phone => (2, 16.0, 12.0),
        };
        let grid_top;
        if cfg.phone() {
            segmented(p, R::new(side, 44.0, cfg.w - 2.0 * side, 44.0), &["Tabs", "Private"], 0);
            grid_top = 104.0;
        } else {
            let cy = 56.0;
            let d = cfg.ctl();
            text_l(p, Font::Thin, tys::TITLE, side, cy, color::INK, "Tabs");
            let done = R::new(cfg.w - side - 100.0, cy - d / 2.0, 100.0, d);
            let new = R::new(done.x - 10.0 - 140.0, cy - d / 2.0, 140.0, d);
            let seg = R::new(new.x - 16.0 - 240.0, cy - d / 2.0, 240.0, d);
            segmented(p, seg, &["Tabs", "Private"], 0);
            for (r, icon, label) in [(new, I::Plus, "New tab"), (done, I::Check, "Done")] {
                p.glass(r, radius::PILL, color::GLASS_TINT);
                ic(p, icon, r.x + 24.0, r.cy(), 16.0, color::INK);
                text_l(p, Font::Semibold, tys::LEAD, r.x + 42.0, r.cy(), color::INK, label);
            }
            grid_top = 112.0;
        }
        let cw = (cfg.w - 2.0 * side - (cols as f32 - 1.0) * gap) / cols as f32;
        let th = if cfg.phone() { cw - 16.0 } else { (cw - 16.0) * 0.62 };
        let ch = th + 16.0 + 44.0;
        for (i, t) in TABS.iter().enumerate() {
            let r = R::new(side + (i % cols) as f32 * (cw + gap), grid_top + (i / cols) as f32 * (ch + gap), cw, ch);
            let mut c = TabCard::new(t.title, t.host);
            c.loading = t.loading;
            c.selected = i == 0;
            c.draw(p, r, &thumbs[i], cfg.touch());
        }
        if cfg.phone() {
            let dock = R::new(M, cfg.h - 34.0 - 72.0, cfg.w - 2.0 * M, 72.0);
            panel(p, dock, radius::XL);
            for (i, (icon, label)) in [(I::Close, "Close all"), (I::Plus, "New tab"), (I::Check, "Done")].iter().enumerate() {
                action(p, dock.x + dock.w * (i as f32 + 0.5) / 3.0, dock.y + 4.0, *icon, label);
            }
        }
        phone_furniture(p, cfg, true);
    })
}

// 3 ---------------------------------------------------------------------------------------------------------------

fn keyboard(p: &mut Paint, cfg: &Cfg, r: R) {
    p.cv.fill_rrect(r, 0.0, color::CARBON);
    let rows = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
    let kw = (cfg.w - 4.0 * 11.0) / 10.0;
    for (ri, row) in rows.iter().enumerate() {
        let n = row.len() as f32;
        let x0 = (cfg.w - n * kw - (n - 1.0) * 5.0) / 2.0;
        for (i, ch) in row.chars().enumerate() {
            let k = R::new(x0 + i as f32 * (kw + 5.0), r.y + 8.0 + ri as f32 * 54.0, kw, 46.0);
            p.cv.fill_rrect(k, 6.0, color::CARBON_HI);
            text_c(p, Font::Regular, (22.0, 0.0), k.cx(), k.cy(), color::INK, &ch.to_string());
        }
    }
    let y = r.y + 8.0 + 3.0 * 54.0;
    p.cv.fill_rrect(R::new(6.0, y, 90.0, 46.0), 6.0, color::CARBON_HI);
    text_c(p, Font::Regular, tys::LEAD, 51.0, y + 23.0, color::INK, "123");
    p.cv.fill_rrect(R::new(102.0, y, cfg.w - 204.0, 46.0), 6.0, color::CARBON_HI);
    p.cv.fill_rrect(R::new(cfg.w - 96.0, y, 90.0, 46.0), 6.0, color::CARBON_HI);
    text_c(p, Font::Semibold, tys::LEAD, cfg.w - 51.0, y + 23.0, color::INK, "Go");
}

fn address(cfg: &Cfg, ty: &mut Type) -> Canvas {
    let mut g = geo(cfg);
    let pg = article(cfg, ty, &g);
    let kb_h = KEYBOARD;
    if cfg.phone() {
        g.field = R::new(M, cfg.h - kb_h - 8.0 - 48.0 - 8.0, cfg.w - 2.0 * M, 48.0);
    }
    let dim = Some((R::new(0.0, 0.0, cfg.w, cfg.h), 0.5));
    compose(cfg, ty, pg.cv, dim, |p| {
        let st = St { typing: Some("lucina"), hide_dock: true, ..St::default() };
        let h = sugg_h(cfg);
        let r = if cfg.phone() {
            R::new(M, g.field.y - 8.0 - h, cfg.w - 2.0 * M, h)
        } else {
            R::new(g.field.x, g.toolbar.bottom() + 6.0, g.field.w, h)
        };
        chrome::draw(p, cfg, &g, &st);
        p.shadow(r, 20.0, 18.0);
        card(p, r, 20.0);
        suggestions(p, cfg, r);
        if cfg.phone() {
            status_bar(p, R::new(0.0, 0.0, cfg.w, STATUS), color::INK);
            keyboard(p, cfg, R::new(0.0, cfg.h - kb_h, cfg.w, kb_h));
            home_indicator(p.cv, color::INK);
        }
    })
}

// 4 ---------------------------------------------------------------------------------------------------------------

fn menu_screen(cfg: &Cfg, ty: &mut Type) -> Canvas {
    let g = geo(cfg);
    let pg = article(cfg, ty, &g);
    let dim = if cfg.phone() { Some((R::new(0.0, 0.0, cfg.w, cfg.h), 0.4)) } else { None };
    compose(cfg, ty, pg.cv, dim, |p| {
        chrome::draw(p, cfg, &g, &St { menu_open: true, hide_dock: cfg.phone(), ..St::default() });
        let r = container(p, cfg, &g, g.menu, 320.0, menu_h(cfg));
        menu(p, cfg, r);
        phone_furniture(p, cfg, !cfg.phone());
        if cfg.phone() {
            home_indicator(p.cv, color::INK);
        }
    })
}

// 5 ---------------------------------------------------------------------------------------------------------------

fn permission_screen(cfg: &Cfg, ty: &mut Type) -> Canvas {
    let g = geo(cfg);
    let pg = article(cfg, ty, &g);
    compose(cfg, ty, pg.cv, None, |p| {
        chrome::draw(p, cfg, &g, &St { asking_location: true, ..St::default() });
        let h = perm_h(cfg);
        let r = if cfg.phone() {
            let r = R::new(M, g.field.y - 8.0 - (h + 20.0), cfg.w - 2.0 * M, h + 20.0);
            panel(p, r, 20.0);
            r.inset(10.0)
        } else {
            container(p, cfg, &g, g.field.x + 340.0 - 18.0, 340.0, h)
        };
        permission(p, cfg, r);
        phone_furniture(p, cfg, false);
    })
}

fn site_screen(cfg: &Cfg, ty: &mut Type) -> Canvas {
    let g = geo(cfg);
    let pg = article(cfg, ty, &g);
    let dim = if cfg.phone() { Some((R::new(0.0, 0.0, cfg.w, cfg.h), 0.4)) } else { None };
    compose(cfg, ty, pg.cv, dim, |p| {
        chrome::draw(p, cfg, &g, &St { lock_open: true, hide_dock: cfg.phone(), ..St::default() });
        let r = container(p, cfg, &g, g.field.x + 360.0 - 18.0, 360.0, site_h(cfg));
        site_info(p, cfg, r);
        phone_furniture(p, cfg, !cfg.phone());
        if cfg.phone() {
            home_indicator(p.cv, color::INK);
        }
    })
}

// 6 ---------------------------------------------------------------------------------------------------------------

fn downloads_screen(cfg: &Cfg, ty: &mut Type) -> Canvas {
    let g = geo(cfg);
    let pg = article(cfg, ty, &g);
    let dim = if cfg.phone() { Some((R::new(0.0, 0.0, cfg.w, cfg.h), 0.4)) } else { None };
    compose(cfg, ty, pg.cv, dim, |p| {
        chrome::draw(p, cfg, &g, &St { download_ring: Some(0.35), hide_dock: cfg.phone(), ..St::default() });
        let r = container(p, cfg, &g, g.dl, 400.0, downloads_h(cfg));
        downloads(p, cfg, r);
        phone_furniture(p, cfg, !cfg.phone());
        if cfg.phone() {
            home_indicator(p.cv, color::INK);
        }
    })
}

// 7 ---------------------------------------------------------------------------------------------------------------

enum Row<'a> {
    Toggle(bool),
    Chevron,
    Value(&'a str),
    Button(&'a str),
}

fn group(p: &mut Paint, cfg: &Cfg, x: f32, y: f32, w: f32, rowh: f32, rows: &[(&str, Row)]) -> f32 {
    let r = R::new(x, y, w, rowh * rows.len() as f32);
    card(p, r, radius::LG);
    for (i, (title, kind)) in rows.iter().enumerate() {
        let row = R::new(x, y + i as f32 * rowh, w, rowh);
        if i > 0 {
            divider(p, x + 16.0, x + w, row.y);
        }
        let main = cfg.main();
        let room = w - 32.0 - 130.0;
        let t = p.fit(Font::Regular, main, title, room.max(120.0));
        text_l(p, Font::Regular, main, x + 16.0, row.cy(), color::INK, &t);
        match kind {
            Row::Toggle(on) => toggle(p, row.right() - 16.0, row.cy(), *on),
            Row::Chevron => ic(p, I::ChevronRight, row.right() - 22.0, row.cy(), 18.0, color::INK_2),
            Row::Value(v) => {
                text_r(p, Font::Regular, main, row.right() - 40.0, row.cy(), color::INK_2, v);
                ic(p, I::ChevronRight, row.right() - 22.0, row.cy(), 18.0, color::INK_2);
            }
            Row::Button(l) => {
                let bh = if cfg.touch() { 44.0 } else { 32.0 };
                button(p, R::new(row.right() - 16.0 - 112.0, row.cy() - bh / 2.0, 112.0, bh), l, false);
            }
        }
    }
    r.bottom()
}

fn settings_screen(cfg: &Cfg, ty: &mut Type) -> Canvas {
    let g = geo(cfg);
    let mut base = page::draw(cfg, ty, &Kind::Sky, 0.0).cv;
    let sky_blur = base.blurred((glass::BLUR * cfg.k / 3.0).round().max(1.0) as u32);
    {
        let mut p = Paint { cv: &mut base, ty, now: 1.0, backdrop: Some(&sky_blur) };
        let p = &mut p;
        if cfg.phone() {
            ic(p, I::Back, 28.0, STATUS + 24.0, 20.0, color::INK);
            text_l(p, Font::Regular, tys::HEADLINE, 44.0, STATUS + 24.0, color::INK, "Settings");
            text_l(p, Font::Thin, tys::TITLE, M + 4.0, STATUS + 76.0, color::INK, "Privacy");
            let mut y = STATUS + 104.0;
            let w = cfg.w - 2.0 * M;
            y = group(p, cfg, M, y, w, 56.0, &[
                ("Block pop-ups", Row::Toggle(true)),
                ("Block third-party cookies", Row::Toggle(true)),
                ("Ask before using location", Row::Toggle(true)),
                ("Send Do Not Track", Row::Toggle(false)),
            ]) + 12.0;
            y = group(p, cfg, M, y, w, 56.0, &[("Site permissions", Row::Chevron), ("Clear browsing data", Row::Button("Clear data"))]) + 12.0;
            group(p, cfg, M, y, w, 56.0, &[("Ask where to save files", Row::Toggle(false))]);
        } else {
            let top = g.page_top + 12.0;
            let rowh = cfg.row() + 8.0;
            // Navigation: the selected section is the one accent.
            let nav = R::new(24.0, top, 230.0, 5.0 * cfg.row());
            for (i, (icon, label)) in [(I::Gear, "General"), (I::Lock, "Privacy"), (I::Globe, "Appearance"), (I::Download, "Downloads"), (I::Doc, "About")].iter().enumerate() {
                let row = R::new(nav.x, nav.y + i as f32 * cfg.row(), nav.w, cfg.row());
                list_row(p, cfg, row, Some(*icon), label, None, false, i == 1);
            }
            let mx = 24.0 + 230.0 + 32.0;
            let mw = cfg.w - mx - 24.0;
            text_l(p, Font::Thin, tys::TITLE, mx, top + 22.0, color::INK, "Privacy");
            let mut y = top + 62.0;
            y = group(p, cfg, mx, y, mw, rowh, &[
                ("Block pop-ups", Row::Toggle(true)),
                ("Block third-party cookies", Row::Toggle(true)),
                ("Ask before using camera or location", Row::Toggle(true)),
                ("Send Do Not Track", Row::Toggle(false)),
            ]) + 14.0;
            y = group(p, cfg, mx, y, mw, rowh, &[("Site permissions", Row::Chevron), ("Clear browsing data", Row::Button("Clear data"))]) + 14.0;
            group(p, cfg, mx, y, mw, rowh, &[("Default zoom", Row::Value("100%")), ("Ask where to save files", Row::Toggle(false))]);
        }
    }
    compose(cfg, ty, base, None, |p| {
        let st = St { host: "lucina://settings", path: "/privacy", internal: true, ..St::default() };
        chrome::draw(p, cfg, &g, &st);
        phone_furniture(p, cfg, true);
    })
}

// 8 ---------------------------------------------------------------------------------------------------------------

fn form_page(cfg: &Cfg, ty: &mut Type, g: &Geo) -> page::Page {
    page::draw(cfg, ty, &Kind::Form, g.page_top)
}

fn form_state() -> St<'static> {
    St { active: 1, host: "shop.example.net", path: "/delivery", ..St::default() }
}

fn dialog_screen(cfg: &Cfg, ty: &mut Type) -> Canvas {
    let g = geo(cfg);
    let pg = form_page(cfg, ty, &g);
    let area = R::new(0.0, if cfg.phone() { 0.0 } else { g.page_top - 4.0 }, cfg.w, cfg.h);
    compose(cfg, ty, pg.cv, Some((area, 0.5)), |p| {
        chrome::draw(p, cfg, &g, &form_state());
        // The origin is the title: the message is the page's own words and must not pass for the browser's.
        let w = if cfg.phone() { 306.0 } else { 360.0 };
        let r = DialogCard::place(area, w, if cfg.phone() { 50.0 } else { 0.0 });
        DialogCard::new("shop.example.net says", "Delete this address?", "Cancel", "OK").draw(p, r);
        phone_furniture(p, cfg, false);
    })
}

fn select_screen(cfg: &Cfg, ty: &mut Type) -> Canvas {
    let g = geo(cfg);
    let pg = form_page(cfg, ty, &g);
    let sel = pg.select;
    let dim = if cfg.phone() { Some((R::new(0.0, 0.0, cfg.w, cfg.h), 0.4)) } else { None };
    let mut base = pg.cv;
    // The focused element: the accent ring on the page's own field.
    base.stroke_rrect(sel.inset(-1.0), 9.0, 2.0, color::ACCENT);
    let options = ["Denmark", "Finland", "Germany", "Iceland", "Norway", "Sweden"];
    compose(cfg, ty, base, dim, |p| {
        chrome::draw(p, cfg, &g, &St { hide_dock: cfg.phone(), ..form_state() });
        let rh = cfg.row();
        if cfg.phone() {
            let h = 44.0 + options.len() as f32 * rh;
            let inner = container(p, cfg, &g, 0.0, 0.0, h);
            text_c(p, Font::Semibold, cfg.main(), inner.cx(), inner.y + 14.0, color::INK, "Country");
            for (i, o) in options.iter().enumerate() {
                let row = R::new(inner.x, inner.y + 44.0 + i as f32 * rh, inner.w, rh);
                list_row(p, cfg, row, None, o, None, false, i == 0);
                if i == 0 {
                    ic(p, I::Check, row.right() - 24.0, row.cy(), 18.0, color::ACCENT);
                }
            }
        } else {
            let r = R::new(sel.x, sel.bottom() + 6.0, sel.w, options.len() as f32 * rh + 16.0);
            p.shadow(r, radius::LG, 18.0);
            card(p, r, radius::LG);
            for (i, o) in options.iter().enumerate() {
                let row = R::new(r.x + 6.0, r.y + 8.0 + i as f32 * rh, r.w - 12.0, rh);
                list_row(p, cfg, row, None, o, None, false, i == 0);
                if i == 0 {
                    ic(p, I::Check, row.right() - 22.0, row.cy(), 16.0, color::ACCENT);
                }
            }
        }
        phone_furniture(p, cfg, false);
        if cfg.phone() {
            home_indicator(p.cv, color::INK);
        }
    })
}

// -----------------------------------------------------------------------------------------------------------------

type Shot = fn(&Cfg, &mut Type) -> Canvas;

const SHOTS: [(&str, Shot); 10] = [
    ("01-browse", browse),
    ("02-tabs", tab_switcher),
    ("03-address", address),
    ("04-menu", menu_screen),
    ("05a-permission", permission_screen),
    ("05b-site-info", site_screen),
    ("06-downloads", downloads_screen),
    ("07-settings", settings_screen),
    ("08a-dialog", dialog_screen),
    ("08b-select", select_screen),
];

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("usage: browser-glass-render <out dir> [juno|phone|tablet] [shot]");
    let only_dev = args.next();
    let only_shot = args.next();
    std::fs::create_dir_all(&dir).expect("out dir");
    let mut ty = Type::new();
    for dev in [Dev::Juno, Dev::Phone, Dev::Tablet] {
        let cfg = Cfg::new(dev);
        if only_dev.as_deref().is_some_and(|d| !cfg.name().contains(d)) {
            continue;
        }
        for (name, shot) in SHOTS {
            if only_shot.as_deref().is_some_and(|s| !name.contains(s)) {
                continue;
            }
            let cv = shot(&cfg, &mut ty);
            let path = Path::new(&dir).join(format!("{}-{name}.png", cfg.name()));
            std::fs::write(&path, cv.png()).expect("write png");
            println!("{}", path.display());
        }
        if cfg.dev != Dev::Tablet && only_shot.as_deref().is_none_or(|s| "01b-browse-light-dark".contains(s)) {
            let cv = light_dark(&cfg, &mut ty);
            let path = Path::new(&dir).join(format!("{}-01b-browse-light-dark.png", cfg.name()));
            std::fs::write(&path, cv.png()).expect("write png");
            println!("{}", path.display());
        }
    }
    let _ = space::TAP;
}
