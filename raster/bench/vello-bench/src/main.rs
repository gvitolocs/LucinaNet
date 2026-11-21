//! vello_cpu draws the scene of `../paint-bench.html`, timed, and is compared with another engine's PNG of that page.
//! `vello-bench [--threads N] [--runs R] [--out out.png] [--compare ref.png] [--diff diff.png]`

use std::time::Instant;

use vello_cpu::color::{AlphaColor, Srgb};
use vello_cpu::kurbo::{Point, Rect, RoundedRect, Shape};
use vello_cpu::peniko::{ColorStop, ColorStops, Extend, Gradient, LinearGradientPosition, RadialGradientPosition};
use vello_cpu::{Pixmap, RenderContext, RenderSettings, Resources};

const W: u16 = 1920;
const H: u16 = 1080;

fn rgb(hex: u32) -> AlphaColor<Srgb> {
    AlphaColor::from_rgba8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8, 255)
}

fn rgba(r: u8, g: u8, b: u8, a: f32) -> AlphaColor<Srgb> {
    AlphaColor::from_rgba8(r, g, b, (a * 255.0).round() as u8)
}

fn stops(a: u32, b: u32) -> ColorStops {
    ColorStops::from([ColorStop::from((0.0, rgb(a))), ColorStop::from((1.0, rgb(b)))].as_slice())
}

fn linear(x0: f64, y0: f64, x1: f64, y1: f64, a: u32, b: u32) -> Gradient {
    Gradient {
        kind: LinearGradientPosition { start: Point::new(x0, y0), end: Point::new(x1, y1) }.into(),
        stops: stops(a, b),
        extend: Extend::Pad,
        ..Default::default()
    }
}

fn rounded(ctx: &mut RenderContext, x0: f64, y0: f64, x1: f64, y1: f64, r: f64) {
    ctx.fill_path(&RoundedRect::new(x0, y0, x1, y1, r).to_path(0.1));
}

/// The cards, as `(x, y)` of each card's top left corner.
fn cards() -> impl Iterator<Item = (f64, f64)> {
    (0..6).flat_map(|r| (0..8).map(move |c| (40.0 + c as f64 * 230.0, 140.0 + r as f64 * 150.0)))
}

fn draw(ctx: &mut RenderContext) {
    ctx.set_paint(linear(0.0, 0.0, 0.0, 1080.0, 0xe8eef7, 0xc9d6ea));
    ctx.fill_rect(&Rect::new(0.0, 0.0, 1920.0, 1080.0));
    ctx.set_paint(linear(0.0, 0.0, 1920.0, 0.0, 0x7b2ff7, 0xf107a3));
    ctx.fill_rect(&Rect::new(0.0, 0.0, 1920.0, 100.0));
    // CSS blur(8px): Gaussian with standard deviation 8.
    ctx.set_paint(rgba(255, 255, 255, 0.6));
    ctx.fill_blurred_rounded_rect(&Rect::new(200.0, 10.0, 800.0, 90.0), 40.0, 8.0, false);
    ctx.push_opacity_layer(0.7);
    ctx.set_paint(rgb(0x00aa88));
    ctx.fill_rect(&Rect::new(1500.0, 20.0, 1700.0, 80.0));
    ctx.set_paint(rgb(0xaa0088));
    ctx.fill_rect(&Rect::new(1620.0, 20.0, 1860.0, 80.0));
    ctx.pop_layer();
    for (x, y) in cards() {
        // box-shadow 0 4px 12px: blur radius 12 is a standard deviation of 6.
        ctx.set_paint(rgba(0, 0, 0, 0.25));
        ctx.fill_blurred_rounded_rect(&Rect::new(x, y + 4.0, x + 200.0, y + 114.0), 12.0, 6.0, false);
        ctx.set_paint(rgb(0xc8c8c8));
        rounded(ctx, x, y, x + 200.0, y + 110.0, 12.0);
        ctx.set_paint(rgb(0xffffff));
        rounded(ctx, x + 1.0, y + 1.0, x + 199.0, y + 109.0, 11.0);
        let centre = Point::new(x + 40.0, y + 55.0);
        ctx.set_paint(Gradient {
            kind: RadialGradientPosition { start_center: centre, start_radius: 0.0, end_center: centre, end_radius: 24.0 }
                .into(),
            stops: stops(0xffcc33, 0xff6600),
            extend: Extend::Pad,
            ..Default::default()
        });
        rounded(ctx, x + 16.0, y + 31.0, x + 64.0, y + 79.0, 24.0);
        ctx.set_paint(rgb(0x3366cc));
        rounded(ctx, x + 80.0, y + 49.0, x + 180.0, y + 61.0, 6.0);
    }
}

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

struct Stats {
    px: u64,
    over: [u64; 4],
    sum: f64,
    sq: f64,
}

impl Stats {
    fn line(&self, name: &str) {
        let mse = self.sq / (self.px as f64 * 3.0);
        let psnr = if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / mse).log10() };
        println!(
            "diff {name}: px {}, d>0 {}, d>2 {}, d>8 {}, d>32 {}, mean |d| {:.3}, psnr {psnr:.2} dB",
            self.px, self.over[0], self.over[1], self.over[2], self.over[3], self.sum / self.px as f64
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let threads: u16 = arg(&args, "--threads").map_or(0, |n| n.parse().expect("--threads N"));
    let runs: usize = arg(&args, "--runs").map_or(20, |n| n.parse().expect("--runs R"));
    let settings = RenderSettings { num_threads: threads, ..Default::default() };
    let mut ctx = RenderContext::new_with(W, H, settings);
    let mut resources = Resources::new();
    let mut pixmap = Pixmap::new(W, H);
    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        let started = Instant::now();
        ctx.reset();
        draw(&mut ctx);
        ctx.flush();
        ctx.render(&mut pixmap, &mut resources);
        times.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    println!(
        "vello-bench threads {threads} runs {runs} min {:.2} ms median {:.2} ms max {:.2} ms",
        times[0],
        times[times.len() / 2],
        times[times.len() - 1]
    );
    // The scene is opaque, so premultiplied and straight bytes are the same.
    let ours = pixmap.data_as_u8_slice().to_vec();
    if let Some(out) = arg(&args, "--out") {
        image::save_buffer(out, &ours, W as u32, H as u32, image::ColorType::Rgba8).expect("write --out");
    }
    let Some(reference) = arg(&args, "--compare") else { return };
    let theirs = image::open(reference).expect("read --compare").to_rgba8();
    assert_eq!((theirs.width(), theirs.height()), (W as u32, H as u32), "reference must be 1920x1080");
    let in_card = |x: u32, y: u32| {
        cards().any(|(cx, cy)| (x as f64) >= cx && (x as f64) < cx + 200.0 && (y as f64) >= cy && (y as f64) < cy + 110.0)
    };
    let new = || Stats { px: 0, over: [0; 4], sum: 0.0, sq: 0.0 };
    let (mut all, mut header, mut grid, mut card) = (new(), new(), new(), new());
    let mut heat = vec![0u8; W as usize * H as usize * 4];
    for y in 0..H as u32 {
        for x in 0..W as u32 {
            let i = (y * W as u32 + x) as usize * 4;
            let t = theirs.get_pixel(x, y).0;
            let (mut d, mut sq) = (0i32, 0f64);
            for c in 0..3 {
                let e = ours[i + c] as i32 - t[c] as i32;
                d = d.max(e.abs());
                sq += (e * e) as f64;
            }
            let g = (d * 8).min(255) as u8;
            heat[i..i + 4].copy_from_slice(&[g, g, g, 255]);
            let mut add = |s: &mut Stats| {
                s.px += 1;
                for (k, limit) in [0, 2, 8, 32].into_iter().enumerate() {
                    s.over[k] += (d > limit) as u64;
                }
                s.sum += d as f64;
                s.sq += sq;
            };
            add(&mut all);
            if y < 100 {
                add(&mut header);
            } else {
                add(&mut grid);
            }
            if in_card(x, y) {
                add(&mut card);
            }
        }
    }
    all.line("all");
    header.line("header");
    grid.line("grid");
    card.line("cards");
    if let Some(out) = arg(&args, "--diff") {
        image::save_buffer(out, &heat, W as u32, H as u32, image::ColorType::Rgba8).expect("write --diff");
    }
}
