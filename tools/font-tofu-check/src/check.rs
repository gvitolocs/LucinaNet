//! Compares each cell of the rendered page with the tofu reference cells.
//!
//! The glyph area of a cell (the cell minus the page's inset on every side) is cut out of the PNG as raw RGBA bytes. A
//! cell is BLANK when its glyph area is one flat colour (the engine drew nothing), TOFU when its glyph area is
//! byte-identical (tolerance 0) to the glyph area of a reference cell (U+FFFF, U+10FFFF: no font assigns them, so each
//! is the notdef box the engine falls back to), and a glyph otherwise.

use crate::page::{Cell, Geometry, Page};

/// A decoded render: RGBA, 8 bits a channel, rows top to bottom.
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

pub fn decode(bytes: &[u8]) -> Result<Image, String> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|e| format!("not a PNG the decoder reads: {e}"))?
        .to_rgba8();
    Ok(Image { w: img.width(), h: img.height(), rgba: img.into_raw() })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Glyph,
    Tofu,
    Blank,
}

#[derive(Debug, Default, Clone)]
pub struct BlockResult {
    pub name: String,
    pub control: bool,
    pub cells: u32,
    pub tofu: u32,
    pub blank: u32,
    pub tofu_cps: Vec<String>,
    pub blank_cps: Vec<String>,
}

#[derive(Debug)]
pub struct Outcome {
    /// The blocks in page order, the reference block left out.
    pub blocks: Vec<BlockResult>,
    pub ref_cells: u32,
    pub ref_distinct: u32,
    pub ref_blank: u32,
}

impl Outcome {
    /// The tofu cells of the test blocks: the controls are not counted, they are there to show the detector works.
    pub fn tofu_total(&self) -> u32 {
        self.blocks.iter().filter(|b| !b.control).map(|b| b.tofu).sum()
    }

    pub fn blank_total(&self) -> u32 {
        self.blocks.iter().filter(|b| !b.control).map(|b| b.blank).sum()
    }

    pub fn cells_total(&self) -> u32 {
        self.blocks.iter().filter(|b| !b.control).map(|b| b.cells).sum()
    }

    /// Tofu detection means something only when there is a reference and every reference drew something.
    pub fn refs_usable(&self) -> bool {
        self.ref_cells > 0 && self.ref_blank == 0
    }
}

/// The glyph area of a cell as one byte string, rows in order.
fn interior(img: &Image, g: Geometry, cell: &Cell) -> Vec<u8> {
    let side = (g.cell - 2 * g.inset) as usize;
    let mut out = Vec::with_capacity(side * side * 4);
    for row in 0..side {
        let y = cell.y as usize + g.inset as usize + row;
        let x = cell.x as usize + g.inset as usize;
        let start = (y * img.w as usize + x) * 4;
        out.extend_from_slice(&img.rgba[start..start + side * 4]);
    }
    out
}

fn uniform(pixels: &[u8]) -> bool {
    match pixels.get(..4) {
        None => true,
        Some(first) => pixels.chunks_exact(4).all(|p| p == first),
    }
}

pub fn analyse(page: &Page, img: &Image) -> Result<Outcome, String> {
    if img.w != page.width() {
        return Err(format!(
            "the PNG is {} px wide and the page lays out {} px wide: render it at {}x{}",
            img.w,
            page.width(),
            page.width(),
            page.height
        ));
    }
    if img.h < page.height {
        return Err(format!(
            "the PNG is {} px tall and the page lays out {} px: render it at {}x{}",
            img.h,
            page.height,
            page.width(),
            page.height
        ));
    }
    let g = page.geom;
    let mut refs: Vec<Vec<u8>> = Vec::new();
    let mut ref_cells = 0;
    let mut ref_blank = 0;
    for cell in page.cells.iter().filter(|c| c.reference) {
        ref_cells += 1;
        let px = interior(img, g, cell);
        if uniform(&px) {
            ref_blank += 1;
        } else if !refs.contains(&px) {
            refs.push(px);
        }
    }
    let mut blocks: Vec<BlockResult> = Vec::new();
    for cell in page.cells.iter().filter(|c| !c.reference) {
        let at = match blocks.iter().position(|b| b.name == cell.block) {
            Some(at) => at,
            None => {
                blocks.push(BlockResult { name: cell.block.clone(), control: cell.control, ..Default::default() });
                blocks.len() - 1
            }
        };
        let px = interior(img, g, cell);
        let verdict = if uniform(&px) {
            Verdict::Blank
        } else if refs.contains(&px) {
            Verdict::Tofu
        } else {
            Verdict::Glyph
        };
        let b = &mut blocks[at];
        b.cells += 1;
        match verdict {
            Verdict::Blank => {
                b.blank += 1;
                b.blank_cps.push(cell.cps.clone());
            }
            Verdict::Tofu => {
                b.tofu += 1;
                b.tofu_cps.push(cell.cps.clone());
            }
            Verdict::Glyph => {}
        }
    }
    Ok(Outcome { blocks, ref_cells, ref_distinct: refs.len() as u32, ref_blank })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::parse;
    use image::{ImageFormat, Rgba, RgbaImage};
    use std::io::Cursor;

    // Cells of 8 px, 4 per row, a 1 px inset (a 6 px glyph area), 2 px headings, 1 px header: the page lays out 32x31.
    const HTML: &str = "<body data-cell=\"8\" data-cols=\"4\" data-head=\"2\" data-top=\"1\" data-inset=\"1\">\
        <section data-block=\"Ref\" data-ref=\"1\"><b data-cp=\"U+FFFF\"></b><b data-cp=\"U+E0FFF\"></b></section>\
        <section data-block=\"Test\"><b data-cp=\"U+4E2D\"></b><b data-cp=\"U+4E00\"></b><b data-cp=\"U+4E8C\"></b>\
        <b data-cp=\"U+4E09\"></b></section>\
        <section data-block=\"Latin\" data-control=\"1\"><b data-cp=\"U+0041\"></b><b data-cp=\"U+0042\"></b></section></body>";

    #[derive(Clone, Copy)]
    enum Shape {
        Frame,
        Diagonal,
        Cross,
        Nothing,
    }

    /// Paints `shape` into the 6 px glyph area of the cell whose top-left corner is (x, y).
    fn paint(img: &mut RgbaImage, x: u32, y: u32, shape: Shape) {
        for dy in 0..6u32 {
            for dx in 0..6u32 {
                let on = match shape {
                    Shape::Frame => dx == 0 || dy == 0 || dx == 5 || dy == 5,
                    Shape::Diagonal => dx == dy,
                    Shape::Cross => dx == dy || dx + dy == 5,
                    Shape::Nothing => false,
                };
                if on {
                    img.put_pixel(x + 1 + dx, y + 1 + dy, Rgba([0, 0, 0, 255]));
                }
            }
        }
    }

    fn png(img: &RgbaImage) -> Vec<u8> {
        let mut bytes = Vec::new();
        img.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png).unwrap();
        bytes
    }

    /// The synthetic render of HTML: two different reference boxes (a frame, a cross), then in the test block a cell
    /// that drew the frame (tofu), a distinct glyph, nothing (blank) and the cross (tofu to the second reference);
    /// in the control block a glyph and a frame.
    fn render(refs: (Shape, Shape)) -> Vec<u8> {
        let mut img = RgbaImage::from_pixel(32, 31, Rgba([255, 255, 255, 255]));
        paint(&mut img, 0, 3, refs.0);
        paint(&mut img, 8, 3, refs.1);
        paint(&mut img, 0, 13, Shape::Frame);
        paint(&mut img, 8, 13, Shape::Diagonal);
        paint(&mut img, 16, 13, Shape::Nothing);
        paint(&mut img, 24, 13, Shape::Cross);
        paint(&mut img, 0, 23, Shape::Diagonal);
        paint(&mut img, 8, 23, Shape::Frame);
        png(&img)
    }

    #[test]
    fn tofu_glyph_and_blank_cells_are_told_apart() {
        let page = parse(HTML).unwrap();
        let out = analyse(&page, &decode(&render((Shape::Frame, Shape::Cross))).unwrap()).unwrap();
        assert_eq!((out.ref_cells, out.ref_distinct, out.ref_blank), (2, 2, 0));
        assert!(out.refs_usable());
        let test = &out.blocks[0];
        assert_eq!((test.name.as_str(), test.cells, test.tofu, test.blank), ("Test", 4, 2, 1));
        assert_eq!(test.tofu_cps, vec!["U+4E2D", "U+4E09"]);
        assert_eq!(test.blank_cps, vec!["U+4E8C"]);
        let latin = &out.blocks[1];
        assert!(latin.control);
        assert_eq!((latin.cells, latin.tofu, latin.blank), (2, 1, 0));
        // The control's tofu is shown in its row but is not part of the total --max-tofu judges.
        assert_eq!(out.tofu_total(), 2);
        assert_eq!(out.blank_total(), 1);
        assert_eq!(out.cells_total(), 4);
    }

    #[test]
    fn a_blank_reference_makes_the_detector_unusable() {
        let page = parse(HTML).unwrap();
        let out = analyse(&page, &decode(&render((Shape::Nothing, Shape::Cross))).unwrap()).unwrap();
        assert_eq!(out.ref_blank, 1);
        assert!(!out.refs_usable());
        // A blank cell is still blank, never tofu, against a blank reference.
        assert_eq!(out.blocks[0].blank, 1);
    }

    #[test]
    fn a_render_of_the_wrong_size_is_an_error_not_a_table() {
        let page = parse(HTML).unwrap();
        let narrow = RgbaImage::from_pixel(31, 31, Rgba([255, 255, 255, 255]));
        let short = RgbaImage::from_pixel(32, 30, Rgba([255, 255, 255, 255]));
        assert!(analyse(&page, &decode(&png(&narrow)).unwrap()).unwrap_err().contains("wide"));
        assert!(analyse(&page, &decode(&png(&short)).unwrap()).unwrap_err().contains("tall"));
        assert!(decode(b"not a png").is_err());
    }

    #[test]
    fn a_taller_render_than_the_page_is_fine() {
        let page = parse(HTML).unwrap();
        let mut img = RgbaImage::from_pixel(32, 60, Rgba([255, 255, 255, 255]));
        paint(&mut img, 0, 3, Shape::Frame);
        let out = analyse(&page, &decode(&png(&img)).unwrap()).unwrap();
        assert_eq!(out.ref_cells, 2);
        assert_eq!(out.ref_blank, 1);
    }
}
