//! Pixel comparison between two PNG screenshots.
//!
//! Used by `sitecheck diff` to measure how far a `lucinanet --render` shot is
//! from a reference browser shot. Both images are decoded to RGBA8 and compared
//! over the area they share, starting at the top left, which is what makes a
//! short LucinaNet page comparable with a full-height reference screenshot.

use std::path::Path;

use image::ImageBuffer;
use serde::Serialize;

/// A pixel differs when one channel moves by more than this much.
pub const DEFAULT_THRESHOLD: u32 = 24;
/// The overlap is reported as a GRID x GRID table of per-cell percentages.
pub const GRID: usize = 4;

/// One decoded screenshot.
pub type RgbaImage = ImageBuffer<image::Rgba<u8>, Vec<u8>>;

/// Everything `sitecheck diff` knows about one pair of screenshots.
#[derive(Debug, Clone, Serialize)]
pub struct DiffReport {
    pub a: String,
    pub b: String,
    /// Pixels wide, then high, for `a` and `b`.
    pub a_size: [u32; 2],
    pub b_size: [u32; 2],
    pub size_differs: bool,
    /// The area both images cover: the smaller of the two widths and heights.
    pub overlap: [u32; 2],
    pub threshold: u32,
    pub differing_pixels: u64,
    pub compared_pixels: u64,
    /// `differing_pixels` as a percentage of `compared_pixels`, 2 decimals.
    pub diff_percent: f64,
    /// Per-cell differing percentage, rows top to bottom, 2 decimals.
    pub grid: Vec<Vec<f64>>,
}

/// Read a PNG from disk; the message is what the CLI reports verbatim.
pub fn load_png(path: &Path) -> Result<RgbaImage, String> {
    let reader = image::ImageReader::open(path)
        .map_err(|e| format!("cannot open: {}", e))?
        .with_guessed_format()
        .map_err(|e| format!("cannot guess format: {}", e))?;
    let image = reader.decode().map_err(|e| format!("cannot decode: {}", e))?;
    Ok(image.to_rgba8())
}

/// Compare two decoded images over their shared top-left area.
pub fn compare(
    a: &RgbaImage,
    b: &RgbaImage,
    a_name: &str,
    b_name: &str,
    threshold: u32,
) -> DiffReport {
    let (w, h) = (a.width().min(b.width()), a.height().min(b.height()));
    let (a_stride, b_stride) = (a.width() as usize * 4, b.width() as usize * 4);
    let (a_px, b_px) = (a.as_raw(), b.as_raw());

    // Column -> cell, so the inner loop only indexes.
    let col_cell: Vec<usize> = (0..w).map(|x| (x as usize * GRID) / w.max(1) as usize).collect();
    let mut cell_diff = [[0u64; GRID]; GRID];
    let mut cell_total = [[0u64; GRID]; GRID];

    for y in 0..h {
        let row_cell = (y as usize * GRID) / h.max(1) as usize;
        let a_row = y as usize * a_stride;
        let b_row = y as usize * b_stride;
        for x in 0..w {
            let gx = col_cell[x as usize];
            cell_total[row_cell][gx] += 1;
            let ia = a_row + x as usize * 4;
            let ib = b_row + x as usize * 4;
            let differs = (0..4).any(|c| {
                (a_px[ia + c] as i32 - b_px[ib + c] as i32).abs() > threshold as i32
            });
            if differs {
                cell_diff[row_cell][gx] += 1;
            }
        }
    }

    let differing_pixels: u64 = cell_diff.iter().flatten().sum();
    let compared_pixels: u64 = cell_total.iter().flatten().sum();
    let grid = cell_diff
        .iter()
        .zip(cell_total.iter())
        .map(|(diff_row, total_row)| {
            diff_row
                .iter()
                .zip(total_row.iter())
                .map(|(&diffed, &total)| percent(diffed, total))
                .collect()
        })
        .collect();

    DiffReport {
        a: a_name.to_string(),
        b: b_name.to_string(),
        a_size: [a.width(), a.height()],
        b_size: [b.width(), b.height()],
        size_differs: (a.width(), a.height()) != (b.width(), b.height()),
        overlap: [w, h],
        threshold,
        differing_pixels,
        compared_pixels,
        diff_percent: percent(differing_pixels, compared_pixels),
        grid,
    }
}

/// Read both files and compare them. On failure the tuple is the path that
/// could not be read or decoded and the reason.
pub fn diff_files(a: &Path, b: &Path, threshold: u32) -> Result<DiffReport, (String, String)> {
    let a_name = a.to_string_lossy().into_owned();
    let b_name = b.to_string_lossy().into_owned();
    match (load_png(a), load_png(b)) {
        (Ok(a_img), Ok(b_img)) => Ok(compare(&a_img, &b_img, &a_name, &b_name, threshold)),
        (Err(e), _) => Err((a_name, e)),
        (Ok(_), Err(e)) => Err((b_name, e)),
    }
}

/// Percentage with 2 decimals, 0 for an empty cell.
fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    (part as f64 * 10000.0 / whole as f64).round() / 100.0
}

/// The human readable block: sizes, overall percentage, then the grid.
pub fn format_diff(report: &DiffReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "a: {} {}x{}\n",
        report.a, report.a_size[0], report.a_size[1]
    ));
    out.push_str(&format!(
        "b: {} {}x{}\n",
        report.b, report.b_size[0], report.b_size[1]
    ));
    if report.size_differs {
        out.push_str("size differs\n");
    }
    out.push_str(&format!(
        "overlap: {}x{}  threshold: {} per channel\n",
        report.overlap[0], report.overlap[1], report.threshold
    ));
    out.push_str(&format!(
        "diff: {:.2}% ({}/{} pixels)\n",
        report.diff_percent, report.differing_pixels, report.compared_pixels
    ));
    out.push_str(&format!(
        "grid {}x{}, % differing pixels per cell:\n",
        GRID, GRID
    ));
    for row in &report.grid {
        let cells: Vec<String> = row.iter().map(|value| format!("{:7.2}", value)).collect();
        out.push_str("    ");
        out.push_str(&cells.join(" "));
        out.push('\n');
    }
    out
}

/// The whole report as one JSON object.
pub fn format_diff_json(report: &DiffReport) -> String {
    serde_json::to_string(report).expect("a DiffReport is always serializable")
}
