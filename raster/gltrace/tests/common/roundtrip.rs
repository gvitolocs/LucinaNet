//! The round-trip scene shared by `roundtrip.rs` (C++ backend) and `roundtrip_rust.rs`. Drives a small scene
//! through `swgl::Context` with tracing on, replays the trace, and checks the replayed default framebuffer is byte-identical to the live one at every frame. No shaders: uploads (inline with a
//! row length, through a mapped unpack buffer), clears, `ClearColorRect` and `BlitFramebuffer`.

use std::os::raw::c_void;
use std::path::PathBuf;

use gleam::gl::{self, Gl};
use gltrace::{frame_rgba, replay, write_png, Options};

const W: i32 = 256;
const H: i32 = 256;

fn pattern(width: usize, height: usize, seed: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            v.extend_from_slice(&[(x * 4) as u8 ^ seed, (y * 4) as u8, seed.wrapping_add((x + y) as u8), 255]);
        }
    }
    v
}

/// The default framebuffer as it is in memory, without applying a pending delayed clear (which would change
/// what later calls draw), as `SoftContext::with_frame` reads it.
fn snapshot(ctx: &swgl::Context) -> Vec<u8> {
    let (ptr, w, h, stride) = ctx.get_color_buffer(0, false);
    assert_eq!((w, h), (W, H));
    unsafe { std::slice::from_raw_parts(ptr as *const u8, (stride * h) as usize) }.to_vec()
}

/// Records the scene live on `backend`, replays the trace on the same backend and checks every frame is byte-identical.
pub fn replay_matches_live(backend: swgl::Backend) {
    swgl::select_backend(backend).unwrap();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("gltrace-roundtrip-{backend:?}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::env::set_var("LUCINANET_GLTRACE", &dir);

    let ctx = swgl::Context::create();
    ctx.make_current();
    let mut fb_mem = vec![0u32; (W * H) as usize];
    ctx.init_default_framebuffer(0, 0, W, H, W * 4, fb_mem.as_mut_ptr() as *mut c_void);

    // A 64x64 texture: the top half uploaded inline from rows 80 pixels apart, the bottom half through a
    // mapped pixel unpack buffer.
    let tex = ctx.gen_textures(1)[0];
    ctx.bind_texture(gl::TEXTURE_2D, tex);
    ctx.tex_storage_2d(gl::TEXTURE_2D, 1, gl::RGBA8, 64, 64);
    let wide = pattern(80, 32, 0x35);
    ctx.pixel_store_i(gl::UNPACK_ROW_LENGTH, 80);
    ctx.tex_sub_image_2d(gl::TEXTURE_2D, 0, 0, 0, 64, 32, gl::BGRA, gl::UNSIGNED_BYTE, &wide);
    ctx.pixel_store_i(gl::UNPACK_ROW_LENGTH, 0);

    let pbo = ctx.gen_buffers(1)[0];
    ctx.bind_buffer(gl::PIXEL_UNPACK_BUFFER, pbo);
    let len = 64 * 32 * 4;
    ctx.buffer_data_untyped(gl::PIXEL_UNPACK_BUFFER, len as isize, std::ptr::null(), gl::STREAM_DRAW);
    let mapped = ctx.map_buffer_range(gl::PIXEL_UNPACK_BUFFER, 0, len as isize, gl::MAP_WRITE_BIT);
    assert!(!mapped.is_null());
    let lower = pattern(64, 32, 0xa7);
    unsafe { std::ptr::copy_nonoverlapping(lower.as_ptr(), mapped as *mut u8, len) };
    ctx.unmap_buffer(gl::PIXEL_UNPACK_BUFFER);
    ctx.tex_sub_image_2d_pbo(gl::TEXTURE_2D, 0, 0, 32, 64, 32, gl::BGRA, gl::UNSIGNED_BYTE, 0);
    ctx.bind_buffer(gl::PIXEL_UNPACK_BUFFER, 0);

    let fbo = ctx.gen_framebuffers(1)[0];
    ctx.bind_framebuffer(gl::READ_FRAMEBUFFER, fbo);
    ctx.framebuffer_texture_2d(gl::READ_FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D, tex, 0);
    ctx.bind_framebuffer(gl::DRAW_FRAMEBUFFER, 0);

    // Frame 0: clear, a red rectangle, the texture blitted at twice its size, filtered.
    ctx.clear_color(0.2, 0.4, 0.6, 1.0);
    ctx.clear(gl::COLOR_BUFFER_BIT);
    ctx.clear_color_rect(0, 10, 20, 50, 40, 1.0, 0.0, 0.0, 1.0);
    ctx.blit_framebuffer(0, 0, 64, 64, 100, 100, 228, 228, gl::COLOR_BUFFER_BIT, gl::LINEAR);
    ctx.trace_frame();
    let live0 = snapshot(&ctx);

    // Frame 1: on top of frame 0, a translucent rectangle and the texture once more, unscaled.
    ctx.clear_color_rect(0, 30, 30, 120, 60, 0.0, 1.0, 0.0, 0.5);
    ctx.blit_framebuffer(0, 0, 64, 64, 5, 180, 69, 244, gl::COLOR_BUFFER_BIT, gl::NEAREST);
    ctx.trace_frame();
    let live1 = snapshot(&ctx);

    let trace = swgl::trace::finish().expect("tracing was on");
    assert!(live0.as_chunks::<4>().0.contains(&[0, 0, 255, 255]), "the red rectangle is in frame 0");
    assert_ne!(live0, live1);

    let opts = Options { out: None, frames: 0..u64::MAX, per_draw: true, no_draws: false, verbose: true };
    let mut replayed = Vec::new();
    let stats = replay(&trace, &opts, &mut |end| {
        let p = end.replayer.color_buffer(0).expect("a default framebuffer");
        replayed.push(p.data.to_vec());
        let (w, h, rgba) = frame_rgba(end).unwrap();
        write_png(&dir.join(format!("frame-{}.png", end.frame)), w, h, &rgba)
    })
    .unwrap();

    assert_eq!(stats.len(), 2);
    assert_eq!(replayed.len(), 2);
    for (n, (r, l)) in replayed.iter().zip([&live0, &live1]).enumerate() {
        let diff: Vec<usize> = (0..r.len()).filter(|&i| r[i] != l[i]).collect();
        if let (Some(a), Some(b)) = (diff.first(), diff.last()) {
            eprintln!("frame {n}: {} bytes differ, px {}..{} (x {} y {}), live {:?} replay {:?}", diff.len(), a / 4, b / 4, (a / 4) % 256, a / 1024, &l[*a & !3..(*a & !3) + 4], &r[*a & !3..(*a & !3) + 4]);
        }
    }
    assert!(replayed[0] == live0, "frame 0 replayed differs from live");
    assert!(replayed[1] == live1, "frame 1 replayed differs from live");
    assert!(dir.join("frame-1.png").exists());
    drop(fb_mem);
}
