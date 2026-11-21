//! The cell map of the font test page, read from the page itself.
//!
//! `tests/fonts/cjk-emoji.html` is static and every cell has a fixed size, so where a cell is drawn is arithmetic:
//! `<body data-cell data-cols data-head data-top data-inset>` gives the cell edge, the cells per row, the height of a
//! block's heading, the height of the page header and how far the glyph area is inset from the cell edge. Each
//! `<section data-block="..">` starts a block (a `data-control` one is a control, a `data-ref` one holds the tofu
//! reference cells) and each tag with a `data-cp` is one cell of the latest section, in document order, left to right
//! and top to bottom. No sidecar file and no script in the engine under test.

/// The fixed geometry the page declares on its `<body>`, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub cell: u32,
    pub cols: u32,
    pub head: u32,
    pub top: u32,
    pub inset: u32,
}

impl Default for Geometry {
    fn default() -> Self {
        Geometry { cell: 96, cols: 16, head: 32, top: 40, inset: 4 }
    }
}

/// One test character (or sequence) and where its cell sits on the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub block: String,
    pub control: bool,
    pub reference: bool,
    /// The code points as the page wrote them, `U+1F468+200D+1F469`.
    pub cps: String,
    pub x: u32,
    pub y: u32,
}

#[derive(Debug)]
pub struct Page {
    pub geom: Geometry,
    pub cells: Vec<Cell>,
    /// The height the page lays out to; the render must be at least this tall.
    pub height: u32,
}

impl Page {
    pub fn width(&self) -> u32 {
        self.geom.cols * self.geom.cell
    }
}

struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn flag(attrs: &[(String, String)], key: &str) -> bool {
    matches!(attr(attrs, key), Some("") | Some("1") | Some("true"))
}

fn number(attrs: &[(String, String)], key: &str, default: u32) -> Result<u32, String> {
    match attr(attrs, key) {
        None => Ok(default),
        Some(v) => v.trim().parse::<u32>().map_err(|_| format!("<body {key}=\"{v}\"> is not a number")),
    }
}

/// `name attr="v" attr2='v' bare` -> the name and its attributes, names lower-cased.
fn split_tag(inner: &str) -> (String, Vec<(String, String)>) {
    let inner = inner.trim_end_matches('/');
    let name_end = inner.find(char::is_whitespace).unwrap_or(inner.len());
    let name = inner[..name_end].to_string();
    let mut rest = &inner[name_end..];
    let mut attrs = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let key_end = rest.find(|c: char| c == '=' || c.is_whitespace()).unwrap_or(rest.len());
        let key = rest[..key_end].to_ascii_lowercase();
        rest = rest[key_end..].trim_start();
        let mut value = String::new();
        if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'');
            if let Some(q) = quote {
                let body = &after[1..];
                let end = body.find(q).unwrap_or(body.len());
                value = body[..end].to_string();
                rest = body.get(end + 1..).unwrap_or("");
            } else {
                let end = after.find(char::is_whitespace).unwrap_or(after.len());
                value = after[..end].to_string();
                rest = &after[end..];
            }
        }
        if !key.is_empty() {
            attrs.push((key, value));
        }
    }
    (name, attrs)
}

/// Every start tag of the page, in order. Comments, end tags and the raw text of <style> and <script> are skipped.
fn tags(html: &str) -> Vec<Tag> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(rel) = html[i..].find('<') {
        let start = i + rel;
        let rest = &html[start..];
        if rest.starts_with("<!--") {
            match rest.find("-->") {
                Some(end) => {
                    i = start + end + 3;
                    continue;
                }
                None => break,
            }
        }
        let Some(end) = rest.find('>') else { break };
        let inner = &rest[1..end];
        i = start + end + 1;
        if inner.starts_with('/') || inner.starts_with('!') {
            continue;
        }
        let (name, attrs) = split_tag(inner);
        let lower = name.to_ascii_lowercase();
        if lower == "style" || lower == "script" {
            let close = format!("</{lower}");
            match html[i..].to_ascii_lowercase().find(&close) {
                Some(end) => i += end,
                None => break,
            }
        }
        out.push(Tag { name: lower, attrs });
    }
    out
}

fn rows(n: u32, cols: u32) -> u32 {
    n.div_ceil(cols)
}

/// Reads the cell map out of the page's HTML.
pub fn parse(html: &str) -> Result<Page, String> {
    let mut geom = Geometry::default();
    let mut cells = Vec::new();
    let mut block: Option<(String, bool, bool)> = None;
    let mut y = geom.top;
    let mut section_top = 0;
    let mut n = 0u32;
    for tag in tags(html) {
        match tag.name.as_str() {
            "body" => {
                let d = Geometry::default();
                geom = Geometry {
                    cell: number(&tag.attrs, "data-cell", d.cell)?,
                    cols: number(&tag.attrs, "data-cols", d.cols)?,
                    head: number(&tag.attrs, "data-head", d.head)?,
                    top: number(&tag.attrs, "data-top", d.top)?,
                    inset: number(&tag.attrs, "data-inset", d.inset)?,
                };
                if geom.cell == 0 || geom.cols == 0 || geom.inset * 2 >= geom.cell {
                    return Err("the page's data-cell, data-cols and data-inset leave no glyph area".to_string());
                }
                y = geom.top;
            }
            "section" => {
                if block.is_some() {
                    y = section_top + rows(n, geom.cols) * geom.cell;
                }
                let name = attr(&tag.attrs, "data-block").ok_or("a <section> has no data-block")?;
                block = Some((name.to_string(), flag(&tag.attrs, "data-control"), flag(&tag.attrs, "data-ref")));
                y += geom.head;
                section_top = y;
                n = 0;
            }
            _ => {
                if let Some(cps) = attr(&tag.attrs, "data-cp") {
                    let Some((name, control, reference)) = &block else {
                        return Err(format!("the cell {cps} is outside any <section data-block>"));
                    };
                    cells.push(Cell {
                        block: name.clone(),
                        control: *control,
                        reference: *reference,
                        cps: cps.to_string(),
                        x: (n % geom.cols) * geom.cell,
                        y: section_top + (n / geom.cols) * geom.cell,
                    });
                    n += 1;
                }
            }
        }
    }
    if block.is_some() {
        y = section_top + rows(n, geom.cols) * geom.cell;
    }
    if cells.is_empty() {
        return Err("the page has no data-cp cells".to_string());
    }
    Ok(Page { geom, cells, height: y })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINI: &str = "<!doctype html><!-- <b data-cp=\"U+0001\"> in a comment --><style>b > i { x: y }</style>\
        <body data-cell=\"8\" data-cols=\"4\" data-head=\"2\" data-top=\"1\" data-inset=\"1\">\
        <section data-block=\"Ref\" data-ref=\"1\"><h2>t</h2><b data-cp=\"U+FFFF\"></b><b data-cp=\"U+E0FFF\"></b></section>\
        <section data-block=\"Test\"><b data-cp=\"U+4E2D\"></b><b data-cp=\"U+4E00\"></b><b data-cp=\"U+4E8C\"></b>\
        <b data-cp=\"U+4E09\"></b><b data-cp=\"U+56DB\"></b></section>\
        <section data-block='Latin' data-control><b data-cp=\"U+0041\"></b></section></body>";

    #[test]
    fn the_cells_land_where_the_grid_puts_them() {
        let page = parse(MINI).unwrap();
        assert_eq!(page.width(), 32);
        // header 1, ref head 2 -> cells at y 3 (one row, ends 11); Test head -> 13, two rows (ends 29); Latin head -> 31, ends 39.
        assert_eq!(page.height, 39);
        let at = |cps: &str| page.cells.iter().find(|c| c.cps == cps).map(|c| (c.x, c.y)).unwrap();
        assert_eq!(at("U+FFFF"), (0, 3));
        assert_eq!(at("U+E0FFF"), (8, 3));
        assert_eq!(at("U+4E2D"), (0, 13));
        assert_eq!(at("U+4E09"), (24, 13));
        assert_eq!(at("U+56DB"), (0, 21));
        assert_eq!(at("U+0041"), (0, 31));
    }

    #[test]
    fn a_comment_and_style_text_are_not_cells_and_the_flags_are_read() {
        let page = parse(MINI).unwrap();
        assert_eq!(page.cells.len(), 8);
        assert!(page.cells.iter().all(|c| c.cps != "U+0001"));
        let flags = |name: &str| page.cells.iter().find(|c| c.block == name).map(|c| (c.control, c.reference)).unwrap();
        assert_eq!(flags("Ref"), (false, true));
        assert_eq!(flags("Test"), (false, false));
        assert_eq!(flags("Latin"), (true, false));
    }

    #[test]
    fn a_cell_before_any_section_and_an_empty_page_are_errors() {
        assert!(parse("<body><b data-cp=\"U+0041\"></b>").is_err());
        assert!(parse("<body><section data-block=\"A\"></section>").is_err());
        assert!(parse("<body data-cell=\"8\" data-inset=\"4\"><section data-block=\"A\"><b data-cp=\"U+1\"></b>").is_err());
    }

    #[test]
    fn the_shipped_page_parses_with_the_geometry_the_render_size_needs() {
        let page = parse(include_str!("../../../tests/fonts/cjk-emoji.html")).unwrap();
        assert_eq!(page.width(), 1536);
        assert_eq!(page.cells.iter().filter(|c| c.reference).count(), 2);
        assert_eq!(page.cells.iter().filter(|c| c.control).count(), 80);
        assert_eq!(page.cells.iter().filter(|c| !c.control && !c.reference).count(), 320);
        assert!(page.cells.iter().all(|c| c.cps.starts_with("U+")));
        // Each cell is inside the declared page.
        assert!(page.cells.iter().all(|c| c.x + 96 <= page.width() && c.y + 96 <= page.height));
    }
}
