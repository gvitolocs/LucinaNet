//! The placeholder pages behind the chrome: plain text and a plain form on a paper ground. They stand for any site; they
//! carry no brand, no picture and no decoration.

use lucina_glass::canvas::{Canvas, Color, R};
use lucina_glass::icons::{self, Icon};
use lucina_glass::imagery;
use lucina_glass::text::{Font, Type};

use crate::ui::Cfg;

const GROUND: Color = Color::hex(0xf6f6f3);
const INK: Color = Color::hex(0x1c1c1e);
const INK_2: Color = Color::hex(0x5c5c60);
const FIELD_RIM: Color = Color::hex(0xc9c9cd);

pub enum Kind<'a> {
    Article(&'a str),
    /// The same page on a dark ground.
    ArticleDark(&'a str),
    Form,
    /// The night sky, for the browser's own pages (cards are drawn over it by the caller).
    Sky,
}

pub struct Page {
    pub cv: Canvas,
    /// The `<select>` of the form page, in dp.
    pub select: R,
    /// The "Delete address" link of the form page.
    pub delete: R,
}

pub fn canvas(cfg: &Cfg) -> Canvas {
    Canvas::new((cfg.w * cfg.k) as u32, (cfg.h * cfg.k) as u32, cfg.k)
}

fn column(cfg: &Cfg) -> (f32, f32) {
    if cfg.phone() { (20.0, cfg.w - 40.0) } else { ((cfg.w - 640.0) / 2.0, 640.0) }
}

/// A page whose content starts `top` dp from the top.
pub fn draw(cfg: &Cfg, ty: &mut Type, kind: &Kind, top: f32) -> Page {
    let mut cv = canvas(cfg);
    let mut page = Page { cv: canvas(cfg), select: R::default(), delete: R::default() };
    let (x, w) = column(cfg);
    let (h1, body, lead) = if cfg.phone() { (28.0, 17.0, 25.0) } else { (34.0, 18.0, 28.0) };
    match kind {
        Kind::Sky => {
            cv = imagery::night_sky(cv.w, cv.h, cfg.k, 7);
        }
        Kind::Article(title) | Kind::ArticleDark(title) => {
            let dark = matches!(kind, Kind::ArticleDark(_));
            let (ground, ink) = if dark { (Color::hex(0x16161b), Color::hex(0xe8e8ee)) } else { (GROUND, INK) };
            cv.clear(ground);
            let mut y = top + 44.0;
            ty.draw(&mut cv, Font::Semibold, h1, -0.3, x, y, ink, title);
            y += 44.0;
            y = ty.paragraph(
                &mut cv,
                Font::Regular,
                body,
                lead,
                x,
                y,
                w,
                ink,
                "This domain is for use in illustrative examples in documents. You may use this domain in literature without prior coordination or asking for permission.",
            ) + 40.0;
            let lw = ty.draw(&mut cv, Font::Regular, body, 0.0, x, y, ink, "More information");
            cv.fill_rrect(R::new(x, y + 4.0, lw, 1.2), 0.0, ink);
        }
        Kind::Form => {
            cv.clear(GROUND);
            let mut y = top + 44.0;
            ty.draw(&mut cv, Font::Semibold, h1, -0.3, x, y, INK, "Delivery");
            y += 40.0;
            let fw = if cfg.phone() { w } else { 420.0 };
            for (label, value, is_select) in [("Name", "Sam Rivers", false), ("Country", "Denmark", true)] {
                ty.draw(&mut cv, Font::Semibold, 13.0, 0.0, x, y, INK_2, label);
                let f = R::new(x, y + 10.0, fw, 44.0);
                cv.fill_rrect(f, 8.0, Color::hex(0xffffff));
                cv.stroke_rrect(f, 8.0, 1.0, FIELD_RIM);
                ty.draw(&mut cv, Font::Regular, 16.0, 0.0, f.x + 14.0, f.cy() + 5.5, INK, value);
                if is_select {
                    icons::draw(&mut cv, Icon::ChevronDown, f.right() - 24.0, f.cy(), 16.0, INK_2);
                    page.select = f;
                }
                y += 76.0;
            }
            let b = R::new(x, y + 4.0, 120.0, 44.0);
            cv.fill_rrect(b, 22.0, INK);
            ty.draw_centered(&mut cv, Font::Semibold, 16.0, 0.0, b.cx(), b.cy() + 5.5, Color::hex(0xffffff), "Save");
            let dw = ty.draw(&mut cv, Font::Semibold, 15.0, 0.0, b.right() + 24.0, b.cy() + 5.0, Color::hex(0xb3261e), "Delete address");
            page.delete = R::new(b.right() + 24.0, b.y, dw, 44.0);
        }
    }
    page.cv = cv;
    page
}
