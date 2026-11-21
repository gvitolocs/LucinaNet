//! The PNG half of the scorer over real files: written by the `png` encoder into the target's own temp
//! directory (never /tmp) and removed again, also when an assertion fails.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::PathBuf;

use sweep_score::{png_content_ratio, read_page, score, Walls};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("sweep-score-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a scratch directory");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_png(path: &PathBuf, w: u32, h: u32, color: png::ColorType, data: &[u8]) {
    let file = BufWriter::new(File::create(path).expect("create png"));
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().expect("png header");
    writer.write_image_data(data).expect("png data");
    writer.finish().expect("png end");
}

#[test]
fn blank_and_painted_frames() {
    let s = Scratch::new("frames");
    let blank = s.0.join("blank.png");
    write_png(&blank, 100, 100, png::ColorType::Rgb, &vec![255u8; 100 * 100 * 3]);
    assert_eq!(png_content_ratio(&blank).unwrap(), 0.0);

    // 10 of 100 rows black: 10 percent of the pixels differ from the white background.
    let mut data = vec![255u8; 100 * 100 * 3];
    for b in data.iter_mut().take(10 * 100 * 3) {
        *b = 0;
    }
    let painted = s.0.join("painted.png");
    write_png(&painted, 100, 100, png::ColorType::Rgb, &data);
    let r = png_content_ratio(&painted).unwrap();
    assert!((r - 10.0).abs() < 1e-9, "{r}");

    let rgba = s.0.join("rgba.png");
    write_png(&rgba, 10, 10, png::ColorType::Rgba, &vec![7u8; 10 * 10 * 4]);
    assert_eq!(png_content_ratio(&rgba).unwrap(), 0.0);

    let bad = s.0.join("bad.png");
    fs::write(&bad, b"not a png").unwrap();
    assert!(png_content_ratio(&bad).is_err());
}

#[test]
fn a_page_directory_scores_end_to_end() {
    let s = Scratch::new("pagedir");
    let dir = s.0.join("S900");
    fs::create_dir_all(&dir).unwrap();
    let mut data = vec![255u8; 100 * 100 * 3];
    for b in data.iter_mut().take(20 * 100 * 3) {
        *b = 10;
    }
    write_png(&dir.join("png"), 100, 100, png::ColorType::Rgb, &data);
    fs::write(dir.join("rc"), "0\n").unwrap();
    fs::write(dir.join("wall_ms"), "3000\n").unwrap();
    fs::write(dir.join("meta"), "id=S900\ncategory=test\nurl=file:///x.html\n").unwrap();
    fs::write(dir.join("log"), "render: complete true in 900 ms\n").unwrap();
    fs::write(dir.join("dump"), r#"{"title":"t","text":"Please verify you are human","elements":4,"boxes":[]}"#).unwrap();

    let walls = Walls::parse("bot\tverify you are human\n").unwrap();
    let row = score(&read_page(&dir, "S900", ""), &walls);
    assert_eq!(row.verdict, "walled", "{}", row.reason);
    assert_eq!(row.category, "test");
    assert_eq!(row.wall_ms, "3000");

    // The same page without the marker is loaded; with a blank PNG it is broken.
    let plain = Walls::parse("").unwrap();
    assert_eq!(score(&read_page(&dir, "S900", ""), &plain).verdict, "loaded");
    write_png(&dir.join("png"), 100, 100, png::ColorType::Rgb, &vec![255u8; 100 * 100 * 3]);
    assert_eq!(score(&read_page(&dir, "S900", ""), &plain).verdict, "broken");
}
