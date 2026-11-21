//! Tests for `sitecheck diff`: generated PNG pairs cover identical images, a
//! single differing pixel, different sizes and grid cell attribution, plus the
//! exit codes and JSON shape of the CLI.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use image::{ImageBuffer, Rgba};
use serde_json::Value;
use sitecheck::diff::{compare, diff_files, format_diff, load_png, DEFAULT_THRESHOLD, GRID};

type Png = ImageBuffer<Rgba<u8>, Vec<u8>>;

const BLACK: Rgba<u8> = Rgba([0, 0, 0, 255]);
const WHITE: Rgba<u8> = Rgba([255, 255, 255, 255]);

/// A flat image of `w` x `h`.
fn solid(w: u32, h: u32, pixel: Rgba<u8>) -> Png {
    ImageBuffer::from_pixel(w, h, pixel)
}

/// Copy of `base` with the given pixels replaced.
fn with_pixels(base: &Png, pixels: &[((u32, u32), Rgba<u8>)]) -> Png {
    let mut image = base.clone();
    for ((x, y), pixel) in pixels {
        image.put_pixel(*x, *y, *pixel);
    }
    image
}

/// Unique scratch directory, so parallel tests never share a file.
fn scratch(tag: &str) -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "sitecheck-diff-{}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst),
        tag
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write `image` as `name.png` in `dir` and return its path.
fn write_png(dir: &Path, name: &str, image: &Png) -> PathBuf {
    let path = dir.join(format!("{}.png", name));
    image.save(&path).expect("png encoding is enabled");
    path
}

/// Compare two generated images through the same entry point the CLI uses.
fn diff_of(a: &Png, b: &Png, threshold: u32, tag: &str) -> sitecheck::diff::DiffReport {
    let dir = scratch(tag);
    let (a_path, b_path) = (write_png(&dir, "a", a), write_png(&dir, "b", b));
    let report = diff_files(&a_path, &b_path, threshold)
        .unwrap_or_else(|(path, reason)| panic!("{}: {}", path, reason));
    std::fs::remove_dir_all(&dir).ok();
    report
}

/// Every cell but `wanted` must be zero; used by the attribution tests.
fn other_cells_are_zero(report: &sitecheck::diff::DiffReport, cell: (usize, usize)) {
    for (gy, row) in report.grid.iter().enumerate() {
        for (gx, value) in row.iter().enumerate() {
            if (gy, gx) == cell {
                assert_ne!(*value, 0.0, "cell {:?} should carry the diff", cell);
            } else {
                assert_eq!(*value, 0.0, "cell ({}, {}) should be clean", gy, gx);
            }
        }
    }
}

#[test]
fn identical_images_do_not_differ() {
    let image = solid(8, 8, BLACK);
    let report = diff_of(&image, &image, DEFAULT_THRESHOLD, "identical");

    assert_eq!(report.a_size, [8, 8]);
    assert_eq!(report.b_size, [8, 8]);
    assert!(!report.size_differs);
    assert_eq!(report.overlap, [8, 8]);
    assert_eq!(report.compared_pixels, 64);
    assert_eq!(report.differing_pixels, 0);
    assert_eq!(report.diff_percent, 0.0);
    assert_eq!(report.grid.len(), GRID);
    assert!(report.grid.iter().all(|row| row.iter().all(|v| *v == 0.0)));
    assert!(!format_diff(&report).contains("size differs"));
}

#[test]
fn one_pixel_difference_is_measured() {
    let a = solid(8, 8, BLACK);
    let b = with_pixels(&a, &[((3, 2), WHITE)]);
    let report = diff_of(&a, &b, DEFAULT_THRESHOLD, "one-pixel");

    assert_eq!(report.differing_pixels, 1);
    assert_eq!(report.compared_pixels, 64);
    assert_eq!(report.diff_percent, 1.56); // 1 of 64, 2 decimals
    // An 8x8 overlap splits into 2x2 pixel cells, so one cell holds 4 pixels.
    assert_eq!(report.grid[1][1], 25.0);
    other_cells_are_zero(&report, (1, 1));
}

#[test]
fn grid_cells_attribute_the_difference_by_position() {
    // One differing pixel per corner of the grid, in a 16x16 overlap where
    // each cell is 4x4 pixels (6.25% on its own).
    let a = solid(16, 16, BLACK);
    let b = with_pixels(&a, &[((0, 0), WHITE)]);
    let top_left = diff_of(&a, &b, DEFAULT_THRESHOLD, "grid-topleft");
    assert_eq!(top_left.grid[0][0], 6.25);
    other_cells_are_zero(&top_left, (0, 0));

    let b = with_pixels(&a, &[((15, 0), WHITE)]);
    let top_right = diff_of(&a, &b, DEFAULT_THRESHOLD, "grid-topright");
    assert_eq!(top_right.grid[0][3], 6.25);
    other_cells_are_zero(&top_right, (0, 3));

    let b = with_pixels(&a, &[((0, 15), WHITE)]);
    let bottom_left = diff_of(&a, &b, DEFAULT_THRESHOLD, "grid-bottomleft");
    assert_eq!(bottom_left.grid[3][0], 6.25);
    other_cells_are_zero(&bottom_left, (3, 0));

    let b = with_pixels(&a, &[((15, 15), WHITE)]);
    let bottom_right = diff_of(&a, &b, DEFAULT_THRESHOLD, "grid-bottomright");
    assert_eq!(bottom_right.grid[3][3], 6.25);
    other_cells_are_zero(&bottom_right, (3, 3));

    // Both halves of one row differing gives two full cells, not one shared.
    let left = solid(16, 16, BLACK);
    let right = ImageBuffer::from_fn(16, 16, |x, _| {
        if x < 8 {
            BLACK
        } else {
            WHITE
        }
    });
    let halves = diff_of(&left, &right, DEFAULT_THRESHOLD, "grid-halves");
    assert_eq!(halves.differing_pixels, 128);
    assert_eq!(halves.diff_percent, 50.0);
    assert!(halves.grid.iter().all(|row| row[0] == 0.0 && row[1] == 0.0));
    assert!(halves.grid.iter().all(|row| row[2] == 100.0 && row[3] == 100.0));
}

#[test]
fn different_sizes_compare_the_overlap() {
    let a = solid(8, 8, BLACK);
    let b = solid(8, 16, BLACK);
    let report = diff_of(&a, &b, DEFAULT_THRESHOLD, "sizes");

    assert!(report.size_differs);
    assert_eq!(report.overlap, [8, 8]);
    assert_eq!(report.compared_pixels, 64);
    assert_eq!(report.diff_percent, 0.0);
    assert!(format_diff(&report).contains("size differs"));

    // Content outside the overlap is not compared: the black half of `b`
    // starts at x=4, past the 4 pixel width `a` shares with it.
    let narrow = solid(4, 4, WHITE);
    let wide = ImageBuffer::from_fn(8, 4, |x, _| if x < 4 { WHITE } else { BLACK });
    let clipped = diff_of(&narrow, &wide, DEFAULT_THRESHOLD, "clipped");
    assert_eq!(clipped.overlap, [4, 4]);
    assert_eq!(clipped.differing_pixels, 0);
    assert_eq!(clipped.diff_percent, 0.0);

    // ...while a difference inside it is still counted, top left anchored.
    let wide_with_mark = with_pixels(&wide, &[((1, 1), BLACK)]);
    let marked = diff_of(&narrow, &wide_with_mark, DEFAULT_THRESHOLD, "clipped-mark");
    assert_eq!(marked.overlap, [4, 4]);
    assert_eq!(marked.differing_pixels, 1);
    assert_eq!(marked.diff_percent, 6.25); // 1 of 16
    assert_eq!(marked.grid[1][1], 100.0); // a 4x4 overlap is one pixel per cell
    other_cells_are_zero(&marked, (1, 1));
}

#[test]
fn threshold_is_per_channel_and_exclusive() {
    let a = solid(2, 2, Rgba([100, 100, 100, 255]));
    // Exactly at the threshold: equal. One above: differing, in every channel.
    let at = solid(2, 2, Rgba([124, 100, 100, 255]));
    let over = solid(2, 2, Rgba([125, 100, 100, 255]));
    assert_eq!(diff_of(&a, &at, DEFAULT_THRESHOLD, "at-limit").differing_pixels, 0);
    assert_eq!(diff_of(&a, &over, DEFAULT_THRESHOLD, "over-limit").differing_pixels, 4);
    // A tighter threshold catches what the default one forgives.
    assert_eq!(diff_of(&a, &at, 10, "tight").differing_pixels, 4);
    // Alpha is a channel like any other: 15 apart passes, 155 apart does not.
    let near = solid(2, 2, Rgba([100, 100, 100, 240]));
    assert_eq!(diff_of(&a, &near, DEFAULT_THRESHOLD, "alpha-near").differing_pixels, 0);
    let translucent = solid(2, 2, Rgba([100, 100, 100, 100]));
    assert_eq!(diff_of(&a, &translucent, DEFAULT_THRESHOLD, "alpha-far").differing_pixels, 4);
}

#[test]
fn decoded_images_keep_their_size_and_pixels() {
    let dir = scratch("decode");
    let image = with_pixels(&solid(3, 5, BLACK), &[((2, 4), WHITE)]);
    let path = write_png(&dir, "shot", &image);
    let decoded = load_png(&path).expect("the generated png decodes");

    assert_eq!((decoded.width(), decoded.height()), (3, 5));
    assert_eq!(decoded.get_pixel(2, 4), &WHITE);
    assert_eq!(decoded.get_pixel(0, 0), &BLACK);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn cli_reports_a_difference_and_exits_zero() {
    let dir = scratch("cli");
    let a = write_png(&dir, "a", &solid(4, 4, BLACK));
    let b = write_png(&dir, "b", &with_pixels(&solid(4, 4, BLACK), &[((0, 0), WHITE)]));

    let out = Command::new(env!("CARGO_BIN_EXE_sitecheck"))
        .args(["diff", a.to_str().unwrap(), b.to_str().unwrap()])
        .output()
        .expect("sitecheck runs");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("diff: 6.25%"), "stdout:\n{}", stdout);
    assert!(stdout.contains("grid 4x4"), "stdout:\n{}", stdout);

    // The threshold flag changes the verdict and the JSON carries it.
    let json = Command::new(env!("CARGO_BIN_EXE_sitecheck"))
        .args([
            "diff",
            a.to_str().unwrap(),
            b.to_str().unwrap(),
            "--threshold",
            "300",
            "--json",
        ])
        .output()
        .expect("sitecheck runs");
    assert!(json.status.success());
    let value: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["threshold"], 300);
    assert_eq!(value["diff_percent"], 0.0);
    assert_eq!(value["grid"].as_array().map(Vec::len), Some(GRID));
    assert_eq!(value["grid"][0][0], 0.0);

    // A file that cannot be read or decoded is the only failure mode: exit 3.
    let missing = dir.join("nope.png");
    let bad = Command::new(env!("CARGO_BIN_EXE_sitecheck"))
        .args(["diff", a.to_str().unwrap(), missing.to_str().unwrap()])
        .output()
        .expect("sitecheck runs");
    assert_eq!(bad.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("nope.png"));

    // Two arguments only: a lone path is a usage error.
    let alone = Command::new(env!("CARGO_BIN_EXE_sitecheck"))
        .args(["diff", a.to_str().unwrap()])
        .output()
        .expect("sitecheck runs");
    assert_eq!(alone.status.code(), Some(2));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn compare_works_on_decoded_images_without_a_file() {
    let a = solid(4, 4, BLACK);
    let b = with_pixels(&a, &[((0, 0), WHITE), ((3, 3), WHITE)]);
    let report = compare(&a, &b, "a", "b", DEFAULT_THRESHOLD);

    assert_eq!(report.a, "a");
    assert_eq!(report.b, "b");
    assert_eq!(report.differing_pixels, 2);
    assert_eq!(report.diff_percent, 12.5);
    assert_eq!(report.grid[0][0], 100.0); // one pixel per cell at this size
    assert_eq!(report.grid[3][3], 100.0);
}
