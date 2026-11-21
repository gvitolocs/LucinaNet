use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use image::{Rgba, RgbaImage};

fn tmpdir(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("pixdiff-test-{tag}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_png(path: &Path, img: &RgbaImage) {
    img.save(path).unwrap();
}

fn solid(w: u32, h: u32, px: [u8; 4]) -> RgbaImage {
    RgbaImage::from_pixel(w, h, Rgba(px))
}

fn run(dir_a: &Path, dir_b: &Path, extra: &[&str]) -> (String, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pixdiff"));
    cmd.arg(dir_a).arg(dir_b).args(extra);
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn identical_frames() {
    let a = tmpdir("identical-a");
    let b = tmpdir("identical-b");
    for n in [1, 2] {
        let img = solid(8, 8, [10, 20, 30, 255]);
        write_png(&a.join(format!("frame-{n}.png")), &img);
        write_png(&b.join(format!("frame-{n}.png")), &img);
    }
    let (out, code) = run(&a, &b, &[]);
    assert_eq!(code, 0, "stdout: {out}");
    assert!(out.contains("identical 2"), "stdout: {out}");
}

#[test]
fn one_pixel_differs() {
    let a = tmpdir("diff-a");
    let b = tmpdir("diff-b");
    let img_a = solid(16, 16, [100, 100, 100, 255]);
    let mut img_b = solid(16, 16, [100, 100, 100, 255]);
    img_b.put_pixel(5, 7, Rgba([103, 100, 100, 255]));
    write_png(&a.join("frame-1.png"), &img_a);
    write_png(&b.join("frame-1.png"), &img_b);

    let (out, code) = run(&a, &b, &[]);
    assert_eq!(code, 1, "stdout: {out}");
    let line = out.lines().find(|l| l.starts_with("frame-1.png")).unwrap();
    assert!(line.contains("d>2 1"), "line: {line}");
    assert!(line.contains("first 5,7"), "line: {line}");

    let (out, code) = run(&a, &b, &["--tolerance", "3"]);
    assert_eq!(code, 0, "stdout: {out}");
    assert!(out.contains("within 3 1"), "stdout: {out}");
}

#[test]
fn numeric_order() {
    let a = tmpdir("order-a");
    let b = tmpdir("order-b");
    for n in [2, 10] {
        let img = solid(4, 4, [1, 2, 3, 255]);
        write_png(&a.join(format!("frame-{n}.png")), &img);
        write_png(&b.join(format!("frame-{n}.png")), &img);
    }
    let (out, code) = run(&a, &b, &[]);
    assert_eq!(code, 0, "stdout: {out}");
    let i2 = out.find("frame-2.png").unwrap();
    let i10 = out.find("frame-10.png").unwrap();
    assert!(i2 < i10, "stdout: {out}");
}

#[test]
fn only_in_one_dir() {
    let a = tmpdir("only-a");
    let b = tmpdir("only-b");
    let img = solid(4, 4, [0, 0, 0, 255]);
    write_png(&a.join("frame-1.png"), &img);
    write_png(&b.join("frame-1.png"), &img);
    write_png(&a.join("frame-2.png"), &img);
    let (out, code) = run(&a, &b, &[]);
    assert_eq!(code, 1, "stdout: {out}");
    assert!(out.contains("only in a: frame-2.png"), "stdout: {out}");
}

#[test]
fn heat_map() {
    let a = tmpdir("heat-a");
    let b = tmpdir("heat-b");
    let heat = tmpdir("heat-out");
    let img_a = solid(8, 8, [50, 50, 50, 255]);
    let mut img_b = solid(8, 8, [50, 50, 50, 255]);
    img_b.put_pixel(3, 4, Rgba([53, 50, 50, 255])); // d = 3 -> gray 48
    write_png(&a.join("frame-1.png"), &img_a);
    write_png(&b.join("frame-1.png"), &img_b);

    let (out, code) = run(&a, &b, &["--heat", heat.to_str().unwrap()]);
    assert_eq!(code, 1, "stdout: {out}");
    let heat_img = image::open(heat.join("frame-1.png")).unwrap().to_rgba8();
    let p = heat_img.get_pixel(3, 4);
    assert_eq!(p.0, [48, 48, 48, 255], "heat pixel: {p:?}");
    let p0 = heat_img.get_pixel(0, 0);
    assert_eq!(p0.0, [0, 0, 0, 255], "heat pixel: {p0:?}");
}
