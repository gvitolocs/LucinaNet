//! `LUCINANET_CHROME_SHOT=<png>` for `--render`: the browser window as the toolkit draws it (bars included) over the page the
//! engine just rendered, written as a PNG. `--render` alone writes the page without any chrome. Test tool, not a mode of the
//! browser: the window is a `Browser` on the host harness, the engine behind it a stand-in that hands over the page already
//! rendered (cropped to the window's page rectangle), and the news the real engine sent (tabs, address, title) is replayed.
//!
//! [`picture`] is the same window and the same writer with the state handed to it instead of collected: one headless load
//! has one tab, types nothing and asks nothing, and the row's other states (several tabs, the field open for editing, the
//! macOS reserve) have to be drawn from what the layout was made to hold. The state goes in as news and real harness input,
//! so what a picture shows is what `Browser::view` decided, not what a picture-drawer redrew.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use lucina_app::{Harness, keys};
use lucinanet_shortcuts::code;

use crate::browser::{Browser, Msg};
use crate::chrome::{self, Mode};
use crate::onebar::{Fit, Part};
use crate::{Cmd, Engine, News, Page, Start, Starting, TabInfo, Url};

/// The news that shapes the bars, kept because `News` is not `Clone`.
pub enum Recorded {
    Url(String),
    Title(String),
    Tabs(Vec<TabInfo>, u32),
    Loading(bool),
    History(bool, bool),
    Bookmarked(bool),
}

pub fn record(news: &News) -> Option<Recorded> {
    Some(match news {
        News::Url(u) => Recorded::Url(u.clone()),
        News::Title(t) => Recorded::Title(t.clone()),
        News::Tabs { tabs, active } => Recorded::Tabs(tabs.clone(), *active),
        News::Loading(l) => Recorded::Loading(*l),
        News::History { back, forward } => Recorded::History(*back, *forward),
        News::Bookmarked(on) => Recorded::Bookmarked(*on),
        _ => return None,
    })
}

impl Recorded {
    fn news(self) -> News {
        match self {
            Recorded::Url(u) => News::Url(u),
            Recorded::Title(t) => News::Title(t),
            Recorded::Tabs(tabs, active) => News::Tabs { tabs, active },
            Recorded::Loading(l) => News::Loading(l),
            Recorded::History(back, forward) => News::History { back, forward },
            Recorded::Bookmarked(on) => News::Bookmarked(on),
        }
    }
}

/// The rendered page, for the stand-in engine (`Start` is a plain function, so it cannot hold it).
static PAGE: Mutex<Option<(u32, u32, Vec<u32>)>> = Mutex::new(None);

struct Quiet;

impl Engine for Quiet {
    fn send(&self, _: Cmd) {}
}

fn stand_in(starting: Starting) -> Box<dyn Engine> {
    let (w, h) = (starting.width as usize, starting.height as usize);
    let mut page = starting.page.lock().unwrap();
    let mut px = vec![0x00ff_ffff; w * h];
    if let Some((sw, sh, src)) = PAGE.lock().unwrap().as_ref() {
        for y in 0..h.min(*sh as usize) {
            let n = w.min(*sw as usize);
            px[y * w..y * w + n].copy_from_slice(&src[y * *sw as usize..y * *sw as usize + n]);
        }
    }
    *page = Page { w: w as u32, h: h as u32, px };
    Box::new(Quiet)
}

/// Paint the window `width` x `height` over `page` with the recorded news, twice (the second frame has nothing new), and write it.
pub fn write(url: &str, width: u32, height: u32, page: &Page, recorded: Vec<Recorded>, out: &str) {
    *PAGE.lock().unwrap() = Some((page.w, page.h, page.px.clone()));
    let first = Url::parse(url).expect("a valid URL");
    let start: Start = stand_in;
    let mut h = Harness::new(Browser::with_news(Arc::new(|_| {}), first, start), width, height);
    for r in recorded {
        h.core.message(Msg::News(r.news()));
    }
    for n in 1..=3 {
        let t = Instant::now();
        h.core.paint();
        eprintln!("render: chrome shot paint #{n} {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
    }
    if let Some((draws, cached, draw_ms, cached_ms)) = h.app().glass_stats() {
        eprintln!("render: glass chrome drew {draws} time(s) (last {draw_ms:.2} ms), found the frame unchanged {cached} time(s) (last {cached_ms:.3} ms)");
    }
    let frame = &h.core.frame;
    png(std::path::Path::new(out), frame.w, frame.h, &frame.px);
    eprintln!("render: chrome shot written to {out}");
}

/// The window as one picture, with the state handed to it rather than collected from a load. One headless `--render` has one
/// tab, types nothing and asks nothing, so everything else the row has to be seen doing - several tabs, the field open for
/// editing, the sheet under it, the macOS reserve - comes in here. It is still the news a `Browser` is told and the keys a
/// person presses: `Browser::view` lays the row out and draws it, and nothing in this file draws a bar of its own.
#[derive(Default)]
pub struct Shot {
    /// What the shown tab is at, and what it is called: the field's display of the address and the window title come from
    /// these.
    pub url: String,
    pub title: String,
    /// The window in device px. The harness takes the scale from it: 1366x768 is 1.0, 2048x1536 is 2.0.
    pub width: u32,
    pub height: u32,
    /// Which chrome draws the row; both are the same `BarLayout` (docs/design/one-bar.md).
    pub glass: bool,
    /// Points kept free at the row's left end, as a host with window buttons over the bar sets it.
    pub inset: f32,
    /// Every tab in tab order, `(title, url, registrable domain)`. The engine's ids are 1 to n in this order; `active` is
    /// the shown index. The third is what the engine's Public Suffix List answers for that host - see [`TABS`], which is
    /// where the fixture states them; `None` is its answer for a host it cannot place.
    pub tabs: Vec<(String, String, Option<String>)>,
    pub active: usize,
    pub loading: bool,
    /// Back and forward, as the engine reports them: placed either way, drawn live or dimmed.
    pub can_back: bool,
    pub can_forward: bool,
    pub bookmarked: bool,
    /// Typed into the field after Ctrl+L, which is what opens it for editing.
    pub typed: Option<String>,
    /// What the engine answered the omnibox's query with, `(url, title)` rows of the sheet.
    pub suggestions: Vec<(String, String)>,
}

/// What the drawn window measured, so a picture can be checked against the criteria and its numbers put in the record.
pub struct Measured {
    /// Device px per point, as the harness chose it for this window.
    pub scale: f32,
    /// The band's height, device px. Criterion 1: 44 x scale, in every state and both chromes.
    pub band_h: u32,
    /// Where the page begins. Criterion 7: exactly the band's height, so the row is all the chrome there is.
    pub page_y: i32,
    /// The field's width, points. Criterion 3: never below its minimum.
    pub field_w: f32,
    /// How the row fit itself, and how many tabs did not fit at all.
    pub fit: Fit,
    pub shed: usize,
    /// Any part crossing the band's top or bottom, or the window's right edge. Criterion 3: never.
    pub clipped: bool,
    /// The leftmost control's x, points. Criterion 6: with a macOS reserve, at or right of it.
    pub first_x: f32,
    /// The box the layout gave the field's mark: device px x, y, w, h, the only part of the picture the page's security
    /// state is drawn in. Zero where the row dropped the mark.
    pub mark: (i32, i32, i32, i32),
    /// The box the layout gave the field itself, device px x, y, w, h: where the address is drawn, and so where an
    /// emphasised part of it can be looked for.
    pub field_box: (i32, i32, i32, i32),
}

/// Draw `shot`'s state over `page`, write `<stem>.png` and `<stem>-band.png` (the band cropped to its own height, which is
/// how criterion 1 is read off the picture) into `dir`, and return what the row measured.
pub fn picture(shot: &Shot, page: &Page, dir: &Path, stem: &str) -> Measured {
    *PAGE.lock().unwrap() = Some((page.w, page.h, page.px.clone()));
    let settings = chrome::Settings { mode: if shot.glass { Mode::Glass } else { Mode::Classic }, left_inset: shot.inset };
    let first = Url::parse(&shot.url).expect("a valid URL");
    let start: Start = stand_in;
    let mut h = Harness::new(Browser::with_chrome(Arc::new(|_| {}), first, start, settings), shot.width, shot.height);
    let shown = shot.active.min(shot.tabs.len().saturating_sub(1));
    let tabs: Vec<TabInfo> = shot
        .tabs
        .iter()
        .enumerate()
        .map(|(i, (title, url, domain))| TabInfo {
            id: i as u32 + 1,
            title: title.clone(),
            url: url.clone(),
            registrable_domain: domain.clone(),
            loading: shot.loading && i == shown,
        })
        .collect();
    h.core.message(Msg::News(News::Tabs { tabs, active: shown as u32 + 1 }));
    h.core.message(Msg::News(News::Url(shot.url.clone())));
    h.core.message(Msg::News(News::Title(shot.title.clone())));
    h.core.message(Msg::News(News::Loading(shot.loading)));
    h.core.message(Msg::News(News::History { back: shot.can_back, forward: shot.can_forward }));
    h.core.message(Msg::News(News::Bookmarked(shot.bookmarked)));
    if let Some(text) = &shot.typed {
        // The person's own way in: Ctrl+L focuses the field, and the typing is the toolkit's.
        h.press(code::L, keys::Mods { ctrl: true, ..Default::default() });
        h.type_str(text);
        if !shot.suggestions.is_empty() {
            // A day apart, newest first, in the order the rows are written. Stamping all three with `now()` left the
            // ranking to the omnibox's tie-breaks, which read the microsecond each stamp happened to land on: the same
            // code run twice drew the sheet's rows in another order, so the pictures were not reproducible.
            let now_us = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros() as i64;
            let entries = shot
                .suggestions
                .iter()
                .enumerate()
                .map(|(i, (url, title))| lucinanet_omnibox::HistoryEntry {
                    url: url.clone(),
                    title: title.clone(),
                    visit_count: 8,
                    typed_count: 2,
                    last_visit_us: now_us - (i as i64 + 1) * DAY_US,
                })
                .collect();
            let id = h.app().omni_query();
            h.core.message(Msg::News(News::HistoryHits { id, entries }));
        }
    }
    h.core.paint();
    let (layout, page_rect) = h.app().one_bar();
    let band_h = layout.band_h.round() as u32;
    let clipped = layout
        .parts
        .iter()
        .any(|b| b.x < 0.0 || b.y < 0.0 || b.right() > layout.width_px + 0.5 || b.bottom() > layout.band_h + 0.5);
    let box_of = |part: Part| {
        layout.placed(part).map(|b| (b.x.round() as i32, b.y.round() as i32, b.w.round() as i32, b.h.round() as i32)).unwrap_or((0, 0, 0, 0))
    };
    let measured = Measured {
        scale: layout.scale,
        band_h,
        page_y: page_rect.y,
        field_w: layout.field_w / layout.scale,
        fit: layout.fit,
        shed: layout.overflow.len(),
        clipped,
        first_x: layout.parts.iter().map(|b| b.x).fold(f32::INFINITY, f32::min) / layout.scale,
        mark: box_of(Part::FieldIcon),
        field_box: box_of(Part::Field),
    };
    let frame = &h.core.frame;
    let (w, px) = (frame.w, &frame.px);
    png(&dir.join(format!("{stem}.png")), w, frame.h, px);
    let band: Vec<u32> = px[..(band_h as usize).min(frame.h as usize) * w as usize].to_vec();
    png(&dir.join(format!("{stem}-band.png")), w, band_h, &band);
    measured
}

/// The window's pixels as an RGBA PNG.
fn png(path: &Path, width: u32, height: u32, px: &[u32]) {
    let mut rgba = Vec::with_capacity(px.len() * 4);
    for p in px {
        rgba.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]);
    }
    let file = std::fs::File::create(path).expect("create the PNG");
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("a PNG header");
    writer.write_image_data(&rgba).expect("write the PNG");
}

/// The fixture's ink: the page's text, and the rule across its very first rows.
pub const INK: u32 = 0x003a_3a3c;

/// How many device rows the fixture's top rule is. Three, because it has to survive two things: the glass chrome mirrors
/// the page's top rows into the bar's bottom edge, and a one-row rule would be half blurred away there; and a picture
/// read by eye needs a band of ink, not a hairline.
pub const INK_RULE_H: usize = 3;

/// A day in microseconds, to space the suggested visits apart.
const DAY_US: i64 = 86_400_000_000;

/// A light page to sit under the row: first a rule of ink across the page's own top rows, then a pale header band, then
/// lines of text the width of a paragraph. The rule is what makes the seam readable: the header band is cream, the same
/// family as the glass bar's tint, and a picture whose page starts in cream cannot show where the chrome ends - it reads
/// as an empty second row. Ink of the page's own colour, unmistakably not a chrome colour, is what the test measures.
pub fn fixture(width: u32, height: u32) -> Page {
    let (w, h) = (width as usize, height as usize);
    let (paper, header, divider, ink) = (0x00f7_f6f0, 0x00ff_f8e1, 0x00d8_d6cc, INK);
    let px = vec![paper; w * h];
    let mut page = Page { w: width, h: height, px };
    let (head, margin, line_h, gap) = (120usize, 48usize, 22usize, 18usize);
    let rules = [head, head + 300, head + 620];
    for (y, px) in page.px.chunks_mut(w).enumerate() {
        if y < INK_RULE_H {
            px.fill(ink);
            continue;
        }
        if y < head {
            px.fill(header);
            continue;
        }
        let left = margin.min(w);
        let line = (y - head).saturating_sub(gap);
        if line % (line_h + gap) < line_h {
            let n = if line / (line_h + gap) % 4 == 3 { w / 3 } else { (w * 2) / 3 };
            px[left..(left + n).min(w)].fill(ink);
        }
        if rules.contains(&y) {
            px[left..(left + w / 2).min(w)].fill(divider);
        }
    }
    page
}

#[cfg(test)]
mod acceptance {
    use std::path::PathBuf;

    use super::*;
    use crate::onebar::{BAR_H, FIELD_MIN, Fit};

    /// A written picture, decoded back from the PNG file. The seam is measured from the ink the file holds rather than
    /// from the layout's numbers, because the picture's whole job is to show where the page starts: a test that only
    /// re-reads the numbers proves the arithmetic, not the drawing.
    struct Pixels {
        w: usize,
        h: usize,
        rgba: Vec<u8>,
    }

    impl Pixels {
        fn read(path: &Path) -> Pixels {
            let file = std::io::BufReader::new(std::fs::File::open(path).expect("open the picture"));
            let mut reader = png::Decoder::new(file).read_info().expect("a readable PNG");
            let (w, h) = (reader.info().width as usize, reader.info().height as usize);
            let mut rgba = vec![0u8; w * h * 4];
            let out = reader.next_frame(&mut rgba).expect("the picture's pixels");
            assert!(
                matches!(out.color_type, png::ColorType::Rgba) && matches!(out.bit_depth, png::BitDepth::Eight),
                "the writer above only ever writes RGBA 8-bit"
            );
            Pixels { w, h, rgba }
        }

        /// The pixel at (x, y), as the 0xRRGGBB the chrome itself works in.
        fn at(&self, x: usize, y: usize) -> u32 {
            let i = (y * self.w + x) * 4;
            (u32::from(self.rgba[i]) << 16) | (u32::from(self.rgba[i + 1]) << 8) | u32::from(self.rgba[i + 2])
        }

        /// The first row at column `x` drawn in the fixture's ink: the page's first painted row, as the picture says.
        fn rule_row(&self, x: usize) -> usize {
            (0..self.h).find(|&y| self.at(x, y) == INK).unwrap_or_else(|| panic!("no ink rule at x={x} in a {}x{} picture", self.w, self.h))
        }

        /// The pixels of `m`, the device-px box the layout gave the field's mark, row by row.
        fn region(&self, m: (i32, i32, i32, i32)) -> Vec<u32> {
            let (x, y, w, h) = (m.0.max(0) as usize, m.1.max(0) as usize, m.2.max(0) as usize, m.3.max(0) as usize);
            assert!(x + w <= self.w && y + h <= self.h, "the mark's box {m:?} is outside a {}x{} picture", self.w, self.h);
            (0..h).flat_map(|j| (0..w).map(move |i| self.at(x + i, y + j))).collect()
        }

        /// Pixels at which two boxes of the same picture differ.
        fn differs(a: &[u32], b: &[u32]) -> usize {
            assert_eq!(a.len(), b.len(), "the two boxes are the same box");
            a.iter().zip(b).filter(|(x, y)| x != y).count()
        }

        /// Pixels of a box in the warning's amber, `browser.rs`'s `WARN` and the kit's `color::WARNING`. Only the warning
        /// is drawn in it, so this says which mark is in the box, not where its edges are.
        fn amber(a: &[u32]) -> usize {
            a.iter().filter(|&&p| p == AMBER).count()
        }

        /// Pixels that differ between this picture and `other` anywhere but inside the given boxes.
        fn differs_outside(&self, other: &Pixels, m: &[(i32, i32, i32, i32)]) -> usize {
            assert_eq!((self.w, self.h), (other.w, other.h), "the two pictures are the same window");
            let inside = |i: usize, j: usize| {
                m.iter().any(|b| {
                    let (x, y, w, h) = (b.0.max(0) as usize, b.1.max(0) as usize, b.2.max(0) as usize, b.3.max(0) as usize);
                    (x..x + w).contains(&i) && (y..y + h).contains(&j)
                })
            };
            (0..self.h)
                .flat_map(|j| (0..self.w).map(move |i| (i, j)))
                .filter(|(i, j)| !inside(*i, *j))
                .filter(|(i, j)| self.at(*i, *j) != other.at(*i, *j))
                .count()
        }

        /// Pixels that differ between this picture and `other` from row `y` down: the page, which no band material touches.
        fn differs_below(&self, other: &Pixels, y: u32) -> usize {
            assert_eq!((self.w, self.h), (other.w, other.h), "the two pictures are the same window");
            (y as usize..self.h)
                .flat_map(|j| (0..self.w).map(move |i| (i, j)))
                .filter(|(i, j)| self.at(*i, *j) != other.at(*i, *j))
                .count()
        }

        /// Per column of `m`, how far its inked pixels are from the surface they sit on: the most frequent colour of the
        /// row each pixel is in, which is what a text strip is mostly made of. A ground that changes down the rows - the
        /// glass capsule's tint over a blurred page, on a host that showed or skipped the blur - cancels against itself
        /// this way, so what is left is the ink's own distance from its own row. A column of plain ground reads 0.
        fn column_peaks(&self, m: (i32, i32, i32, i32)) -> Vec<i64> {
            let (x, y, w, h) = (m.0.max(0) as usize, m.1.max(0) as usize, m.2.max(0) as usize, m.3.max(0) as usize);
            assert!(x + w <= self.w && y + h <= self.h, "the field's box {m:?} is outside a {}x{} picture", self.w, self.h);
            (0..h).fold(vec![0i64; w], |mut peaks, j| {
                let row: Vec<u32> = (0..w).map(|i| self.at(x + i, y + j)).collect();
                let mut counts: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
                for p in &row {
                    *counts.entry(*p).or_default() += 1;
                }
                let (ground, _) = counts.into_iter().max_by_key(|(_, n)| *n).expect("a row of the field box");
                for (i, p) in row.iter().enumerate() {
                    peaks[i] = peaks[i].max(inkness(*p, ground));
                }
                peaks
            })
        }
    }

    /// How far one pixel is from another, summed over the channels: no colour model and no weighting, just a distance to
    /// call ink by.
    fn inkness(a: u32, b: u32) -> i64 {
        let c = |v: u32, s: u32| ((v >> s) & 0xff) as i64;
        (c(a, 16) - c(b, 16)).abs() + (c(a, 8) - c(b, 8)).abs() + (c(a, 0) - c(b, 0)).abs()
    }

    /// The middle of `v`, which must not be empty.
    fn median(v: &[i64]) -> i64 {
        assert!(!v.is_empty(), "a median of nothing");
        let mut s = v.to_vec();
        s.sort_unstable();
        s[s.len() / 2]
    }

    /// The amber of a page that arrived in the clear, as both chromes paint it.
    const AMBER: u32 = 0xff9f0a;

    /// The hosts of the fixture tabs, what the field shows of each and the pair the engine's `News::Tabs` carries with it:
    /// `(title, url, registrable domain)`. Real hosts would do, but a picture that outlives the page behind it should not
    /// depend on a site still being up; these are the `example` reserved names and a handful of permanent ones, most with a
    /// path so the field shows the page and not just the site.
    ///
    /// The third is written out because it is the engine's answer and this crate cannot compute it - the Public Suffix
    /// List is in the engine binary (`lucinanet-servo`'s `domains`), and a picture is a fixture of what the engine sends,
    /// so the fixture says what the engine would say. It is what the pinned list answers for each host today;
    /// [`EMPHASIS`] checks the drawing against it, not the list against these words.
    const TABS: [(&str, &str, Option<&str>); 12] = [
        ("World news", "https://news.example.org/world", Some("example.org")),
        ("Rust Programming Language", "https://www.rust-lang.org/", Some("rust-lang.org")),
        ("Example Domain", "https://example.com/", Some("example.com")),
        ("MDN", "https://developer.mozilla.org/docs/Web", Some("mozilla.org")),
        ("Wikipedia", "https://en.wikipedia.org/wiki/Browser_engine", Some("wikipedia.org")),
        ("RFC 6266", "https://www.rfc-editor.org/rfc/rfc6266", Some("rfc-editor.org")),
        ("News.ycombinator", "https://news.ycombinator.com/", Some("ycombinator.com")),
        ("Stack Overflow", "https://stackoverflow.com/questions", Some("stackoverflow.com")),
        ("GitHub", "https://github.com/servo/servo", Some("github.com")),
        ("WebKit", "https://webkit.org/blog", Some("webkit.org")),
        ("Crates.io", "https://crates.io/keywords/browser", Some("crates.io")),
        ("Searchfox", "https://searchfox.org/lucina", Some("searchfox.org")),
    ];

    /// The four addresses the emphasis itself is proved on, `(slug, url, registrable domain, ink before the site?)`, each
    /// a shape the field has to get right: a subdomain with a query long enough to be cut, a two-label suffix, a host that
    /// *is* the registrable domain, and a host with no answer from the list at all. Both chromes, at 2x.
    const EMPHASIS: [(&str, &str, Option<&str>, bool); 4] = [
        (
            "signin",
            "https://accounts.google.com/v3/signin/identifier?continue=https%3A%2F%2Fmail.google.com%2F&hl=en&flowName=GlifWebSignIn&flow=entry",
            Some("google.com"),
            true,
        ),
        ("bbc", "https://news.bbc.co.uk/world/2026/08", Some("bbc.co.uk"), true),
        ("io", "https://me.github.io/path", Some("me.github.io"), false),
        ("ip", "http://127.0.0.1:8080/", None, true),
    ];

    fn tabs(n: usize) -> Vec<(String, String, Option<String>)> {
        TABS[..n].iter().map(|(t, u, d)| (t.to_string(), u.to_string(), d.map(str::to_owned))).collect()
    }

    impl Shot {
        /// Show tab `i`: the field's address and the window's title are that tab's, as the engine reports them.
        fn show(&mut self, i: usize) {
            self.active = i;
            self.url = TABS[i].1.into();
            self.title = TABS[i].0.into();
        }
    }

    /// Each state of the acceptance list, applied to a shot whose window, chrome and reserve are already set.
    const STATES: [(&str, fn(&mut Shot)); 6] = [
        ("1-one-tab", one_tab),
        ("2-three-tabs", three_tabs),
        ("3-twelve-tabs", twelve_tabs),
        ("4-editing-suggestions", editing),
        ("5-loading", loading),
        ("6-mac-inset", mac_inset),
    ];

    /// 1. One tab: the field alone, its host centred, no second row.
    fn one_tab(s: &mut Shot) {
        s.tabs = tabs(1);
        s.show(0);
        s.can_back = true;
    }

    /// 2. Three tabs, the middle one shown: order preserved, the field between its neighbours. The star is on.
    fn three_tabs(s: &mut Shot) {
        s.tabs = tabs(3);
        s.show(1);
        s.can_back = true;
        s.can_forward = true;
        s.bookmarked = true;
    }

    /// 3. Twelve tabs: inactive tabs compact, the field at its min or more, the chip only if some did not fit.
    fn twelve_tabs(s: &mut Shot) {
        s.tabs = tabs(12);
        s.show(5);
        s.can_back = true;
    }

    /// 4. Editing with suggestions: the field grown, neighbours compact, the sheet hanging below over the page.
    fn editing(s: &mut Shot) {
        s.tabs = tabs(4);
        s.show(1);
        s.typed = Some("news".into());
        s.suggestions = vec![
            ("https://news.example.org/world".into(), "World news".into()),
            ("https://news.ycombinator.com/".into(), "News.ycombinator".into()),
            ("https://newsletter.example.net/weekly".into(), "The weekly newsletter".into()),
        ];
    }

    /// 5. Loading: the accent line along the field's bottom edge and the stop in it.
    fn loading(s: &mut Shot) {
        s.tabs = tabs(3);
        s.show(1);
        s.loading = true;
        s.can_back = true;
    }

    /// 6. macOS: the reserve for the window buttons, with nothing drawn left of it.
    fn mac_inset(s: &mut Shot) {
        s.tabs = tabs(3);
        s.show(1);
        s.inset = crate::onebar::INSET_MACOS;
        s.can_back = true;
    }

    /// The field's mark, one state per picture: the lock of a page that came over an encrypted scheme, the warning of a page
    /// that did not, the search glass while the person is typing. One tab each, and the same host in the two read states, so
    /// the mark is all that separates their pictures - which is what the pixel checks in the test lean on.
    const SECURITY: [(&str, fn(&mut Shot)); 3] = [("https", security_https), ("http", security_http), ("editing", security_editing)];

    /// One tab, shown: the field's address and the window's title are that tab's, as the engine reports them. The list's
    /// answer is `None` here, which for these two hosts is the same drawing as `Some("example.com")` - the whole host is
    /// the site either way - and all the security pictures need is the same address in both states.
    fn one(s: &mut Shot, title: &str, url: &str) {
        s.tabs = vec![(title.to_string(), url.to_string(), None)];
        s.active = 0;
        s.url = url.to_string();
        s.title = title.to_string();
        s.can_back = true;
    }

    /// The lock.
    fn security_https(s: &mut Shot) {
        one(s, "Example Domain", "https://example.com/");
    }

    /// The warning: the same page, over http. The host is the same, so the field reads the same either way and only its
    /// left end knows the difference.
    fn security_http(s: &mut Shot) {
        one(s, "Example Domain", "http://example.com/");
    }

    /// The search glass: the field open, holding what the person typed.
    fn security_editing(s: &mut Shot) {
        security_https(s);
        s.typed = Some("http://example.com/".into());
    }

    /// The six states, in both chromes at both scales.
    fn table() -> Vec<(String, Shot)> {
        let mut out = Vec::new();
        for glass in [false, true] {
            let chrome = if glass { "glass" } else { "classic" };
            for (width, height, scale) in [(1366, 768, "1x"), (2048, 1536, "2x")] {
                for (slug, set) in STATES {
                    let mut shot = Shot { width, height, glass, ..Default::default() };
                    set(&mut shot);
                    out.push((format!("{slug}-{chrome}-{scale}"), shot));
                }
            }
        }
        // The fold, in both chromes: the harness floors the scale at 0.6, so 400x300 is the narrowest window it can
        // make - 666 points of it - and twelve tabs have to fold into the chip there.
        for glass in [false, true] {
            let mut shot = Shot { width: 400, height: 300, glass, ..Default::default() };
            twelve_tabs(&mut shot);
            out.push((format!("3b-twelve-tabs-floor-{}", if glass { "glass" } else { "classic" }), shot));
        }
        // The field's three marks, in both chromes at 2x - the scale the glyphs are judged at, because it is the one at
        // which a shackle either meets the body or does not.
        for glass in [false, true] {
            let chrome = if glass { "glass" } else { "classic" };
            for (slug, set) in SECURITY {
                let mut shot = Shot { width: 2048, height: 1536, glass, ..Default::default() };
                set(&mut shot);
                out.push((format!("security-{slug}-{chrome}-2x"), shot));
            }
        }
        // The four addresses the emphasis is proved on, same scale and both chromes: one tab each, so the field has the
        // row to itself and its strip is as wide as it gets.
        for glass in [false, true] {
            let chrome = if glass { "glass" } else { "classic" };
            for (slug, url, domain, _) in EMPHASIS {
                let mut shot = Shot { width: 2048, height: 1536, glass, ..Default::default() };
                shot.tabs = vec![("Shown page".into(), url.into(), domain.map(str::to_owned))];
                shot.url = url.into();
                shot.title = "Shown page".into();
                shot.can_back = true;
                out.push((format!("emphasis-{slug}-{chrome}-2x"), shot));
            }
        }
        out
    }

    /// The pictures, into the directory `LUCINANET_ONEBAR_SHOTS` names, with a manifest of what each measured. Ignored so
    /// a normal test run does not write files; `scripts/onebar-shots.sh` runs it and keeps the PNGs out of the repository.
    #[test]
    #[ignore]
    fn the_one_bar_pictures() {
        let dir = PathBuf::from(std::env::var("LUCINANET_ONEBAR_SHOTS").expect("LUCINANET_ONEBAR_SHOTS=<directory>"));
        std::fs::create_dir_all(&dir).expect("the picture directory");
        let mut manifest = String::new();
        let mut marks = Vec::new();
        let mut fields = Vec::new();
        for (stem, shot) in table() {
            let m = picture(&shot, &fixture(shot.width, shot.height), &dir, &stem);
            marks.push((stem.clone(), m.mark, m.band_h));
            fields.push((stem.clone(), m.field_box));
            let k = m.scale;
            // 1 and 7: the band is 44 points and the page begins under it - in every state, both chromes, both scales.
            assert_eq!(m.band_h, (BAR_H * k).round() as u32, "{stem}: the band is {BAR_H} points times {k}");
            assert_eq!(m.page_y, m.band_h as i32, "{stem}: the row is the whole chrome above the page");
            assert!(!m.clipped, "{stem}: a part crosses the band or the window's edge");
            assert!(m.field_w >= FIELD_MIN - 0.5, "{stem}: the field is {} points, under its {FIELD_MIN} minimum", m.field_w);
            if stem.starts_with("3-twelve-tabs") {
                assert!(!matches!(m.fit, Fit::Full), "{stem}: twelve tabs at full width cannot fit");
            }
            if stem.starts_with("3b-") {
                // The fold: the tabs furthest from the shown one go into the chip, and the field keeps its minimum.
                assert!(m.shed > 0, "{stem}: the narrow window folded nothing away");
                assert!(m.field_w >= FIELD_MIN, "{stem}: the field was squeezed under its minimum");
            }
            if shot.inset > 0.0 {
                // 6: with the reserve for the window buttons, the leftmost control starts at or right of it.
                assert!(m.first_x >= shot.inset - 0.5, "{stem}: a control is at {} dp, left of the {} dp reserve", m.first_x, shot.inset);
            }
            // 1 and 7 again, this time off the ink: the page's first painted row - the fixture's rule - is exactly
            // band_h rows down, and the row above it is chrome rather than ink. Probed away from the middle, where a
            // sheet or the field could sit over the page, and on the editing shots also at x = 10, left of the sheet.
            let pic = Pixels::read(&dir.join(format!("{stem}.png")));
            let mut probes = vec![pic.w - 300];
            if stem.starts_with("4-editing") {
                probes.push(10);
            }
            for x in probes {
                let seam = pic.rule_row(x);
                assert_eq!(seam, m.band_h as usize, "{stem}: the page's ink rule is at row {seam} at x={x}, the band measured {}", m.band_h);
                assert_ne!(pic.at(x, seam - 1), INK, "{stem}: row {} is ink too at x={x}, so the chrome is drawing the rule itself", seam - 1);
            }
            manifest.push_str(&format!(
                "{stem}.png\t{k}x\tband={}px\tpage_y={}px\tfield={:.0}dp\t{:?}\tshed={}\n",
                m.band_h, m.page_y, m.field_w, m.fit, m.shed
            ));
            eprintln!("shot: {stem} k={k} band={} page_y={} field={:.0}dp {:?} mark={:?}", m.band_h, m.page_y, m.field_w, m.fit, m.mark);
        }
        // The field's mark, read off the pixels. The three states are three drawings, and the lock and the warning are not
        // one drawing in two colours. `box_of` is the box the layout gave the mark; the two read states share the host, the
        // tab and the page, so what differs inside that box is the mark - and nothing else may differ, or the row moved
        // around it and the comparison below would be about the row.
        //
        // How strictly that is checked depends on what the chrome is made of. The classic one is solid fills and comes out
        // the same whatever else the machine is doing, so the whole picture is compared. The glass one samples the page's
        // top rows for its backdrop and gives up on the blur when the blur costs more than its budget, which on a busy
        // host turns the whole band from translucent to solid between one picture and the next; there the comparison stops
        // at the seam, and below it the page is the fixture either way.
        for chrome in ["classic", "glass"] {
            let state = |slug: &str| {
                let stem = format!("security-{slug}-{chrome}-2x");
                let (box_of, band_h) = marks
                    .iter()
                    .find(|(s, _, _)| *s == stem)
                    .map(|(_, m, band)| (*m, *band))
                    .expect("the security picture's mark box");
                (Pixels::read(&dir.join(format!("{stem}.png"))), box_of, band_h)
            };
            let (lock, box_of, band_h) = state("https");
            let (warn, warn_box, _) = state("http");
            let (search, search_box, _) = state("editing");
            assert_eq!(box_of, warn_box, "{chrome}: the lock and the warning are not in the same box");
            assert_eq!(box_of, search_box, "{chrome}: the search glass is not in the box the lock was");
            let (l, w, s) = (lock.region(box_of), warn.region(box_of), search.region(box_of));
            if chrome == "classic" {
                // The two read states are the same host in two schemes, and the field says so: `https://` is dropped and
                // `http://` is kept. So the mark's box and the field's own strip are the two places these pictures may
                // differ; a difference anywhere else means the row moved something it should not have.
                let strip = |slug: &str| {
                    let stem = format!("security-{slug}-{chrome}-2x");
                    fields.iter().find(|(s, _)| *s == stem).map(|(_, b)| *b).expect("the security picture's field box")
                };
                let may_differ = [box_of, strip("https"), strip("http")];
                assert_eq!(
                    lock.differs_outside(&warn, &may_differ),
                    0,
                    "{chrome}: the https and http pictures differ outside the mark and the field strip {may_differ:?}"
                );
            } else {
                assert_eq!(lock.differs_below(&warn, band_h), 0, "{chrome}: the page moved between the https and http pictures");
            }
            for (what, n) in
                [("lock/warning", Pixels::differs(&l, &w)), ("lock/search", Pixels::differs(&l, &s)), ("warning/search", Pixels::differs(&w, &s))]
            {
                assert!(n >= 100, "{chrome}: the mark says the same thing for {what} - only {n} of the {} pixels of its box differ", l.len());
                eprintln!("mark {chrome} {what}: {n}/{} pixels", l.len());
            }
            // And which mark is in the box, by colour: the amber of a page that came in the clear belongs to the warning
            // and to nothing else the row draws.
            assert!(Pixels::amber(&w) >= 20, "{chrome}: the http mark holds no amber, only {} pixels of it", Pixels::amber(&w));
            assert_eq!(Pixels::amber(&l), 0, "{chrome}: the https mark is drawn in the warning's amber");
            assert_eq!(Pixels::amber(&s), 0, "{chrome}: the mark while typing is drawn in the warning's amber");
        }
        // The emphasis, read off the ink of the field and not off the layout's numbers: the strip must hold two levels of
        // ink, the stronger one a single run of columns with the weaker one outside it, which is the site standing out of
        // its scheme, subdomain and path.
        //
        // Deliberately not the check: that pixels changed inside `Part::Field`'s box, or where the emphasis begins and ends
        // in x. That box is the same rectangle for every address this row shows, so a proof about it would hold just as
        // well for a field that moved the page, dimmed a border or drew a caret - the bounding box is not the check. That
        // is the lesson of the scaled-planes bug: the pictures were proved against a box, and the planes drawn inside the
        // box were the wrong ones, scaled and dark, until someone looked at one. An ink *level* says something a rectangle
        // cannot.
        for chrome in ["classic", "glass"] {
            for (slug, _, _, ink_before) in EMPHASIS {
                let stem = format!("emphasis-{slug}-{chrome}-2x");
                let field = *fields.iter().find(|(s, _)| *s == stem).map(|(_, b)| b).expect("the emphasis picture's field box");
                let pic = Pixels::read(&dir.join(format!("{stem}.png")));
                assert!(field.2 > 0 && field.3 > 0, "{stem}: the row gave the field no box to draw in: {field:?}");
                let peaks = pic.column_peaks(field);
                let top = *peaks.iter().max().expect("a field with columns in it");
                assert!(top > 0, "{stem}: nothing at all is drawn in the field's box {field:?}");
                // The two bands, as a share of the darkest column in the strip. A gap is left between them on purpose: the
                // claim is that the strip holds two levels of ink, so a column of one must not read as a column of the
                // other, and the half-covered columns in between belong to neither and are counted by neither.
                let strong_ink = |p: i64| 4 * p >= 3 * top;
                let dim_ink = |p: i64| 10 * p >= 3 * top && 10 * p <= 7 * top;
                let strong: Vec<usize> = (0..peaks.len()).filter(|&i| strong_ink(peaks[i])).collect();
                assert!(!strong.is_empty(), "{stem}: no ink at all reaches the field's top level");
                let (lo, hi) = (*strong.first().unwrap() as i64, *strong.last().unwrap() as i64);
                // An anti-aliased edge of the strong run is a column or two of half-ink, so the rest of the address is
                // looked for from four columns clear of the run rather than from its first pixel.
                let (from, to) = (lo - 4, hi + 4);
                let outside: Vec<i64> =
                    (0..peaks.len()).filter(|&i| (i as i64) < from || (i as i64) > to).map(|i| peaks[i]).filter(|&p| dim_ink(p)).collect();
                assert!(!outside.is_empty(), "{stem}: nothing but the site's own ink in the field: the two levels are one");
                let dim_left = (0..peaks.len()).filter(|&i| (i as i64) < from && dim_ink(peaks[i])).count();
                let dim_right = (0..peaks.len()).filter(|&i| (i as i64) > to && dim_ink(peaks[i])).count();
                assert_eq!(dim_left > 0, ink_before, "{stem}: dim ink on the site's left where the address has {ink_before}");
                assert!(dim_right > 0, "{stem}: nothing dim to the right of the site: the run is the whole address");
                // The strong ink is one band, not pixels scattered over the strip. The letters of a host do not touch:
                // inside a word the columns between them carry nothing, and the holes measured here run 4 to 7 columns.
                // What would mean the emphasis went wrong is a hole wider than any letter gap - a space, or the gap
                // between two places that reached the top level - and a band too empty to be one run of text.
                let gaps: Vec<usize> = strong.windows(2).map(|p| p[1] - p[0] - 1).collect();
                let widest = gaps.iter().max().copied().unwrap_or(0);
                assert!(widest <= 12, "{stem}: the strong ink has a {widest}-column hole in it, past any letter gap: {gaps:?}");
                assert!(strong.len() * 2 >= (hi - lo + 1) as usize, "{stem}: only {}/{} columns between its ends are strong ink", strong.len(), hi - lo + 1);
                let (s_med, d_med) = (median(&strong.iter().map(|&i| peaks[i]).collect::<Vec<_>>()), median(&outside));
                assert!(s_med * 20 >= 27 * d_med, "{stem}: the site's ink is only {s_med} against {d_med} for the rest of it");
                eprintln!(
                    "emphasis {stem}: top={top} strong={}px [{lo}..={hi}] median={s_med}, dim left={dim_left} right={dim_right} median={d_med}, ratio={:.2}",
                    strong.len(),
                    s_med as f64 / d_med as f64
                );
                manifest.push_str(&format!(
                    "{stem}.png\tfield={field:?}\tstrong={}px@{lo}..{hi}\ttop={top}\tstrong_med={s_med}\tdim_med={d_med}\tratio={:.2}\n",
                    strong.len(),
                    s_med as f64 / d_med as f64
                ));
            }
        }
        std::fs::write(dir.join("manifest.txt"), manifest).expect("write the manifest");
    }
}
