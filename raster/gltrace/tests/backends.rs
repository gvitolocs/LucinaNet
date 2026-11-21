//! The Rust backend against swgl's C++ on everything but drawing (no shaders exist for the Rust one yet): each trace
//! is replayed with `gltrace replay --per-draw --no-draws` on both backends, and every hash line (one per `Clear`,
//! `ClearColorRect`, `BlitFramebuffer`, `Composite`, texture upload and `CopyImageSubData`/`CopyTexSubImage2D`)
//! must be the same. The real traces are LucinaNet sessions on three pages (`raster/capture-traces.sh`); the
//! synthetic one covers the calls those pages do not make.

use std::os::raw::c_void;
use std::path::{Path, PathBuf};
use std::process::Command;

use gleam::gl::{self, Gl};

/// The `hash` lines of `gltrace replay <trace> --per-draw --no-draws --backend <backend>`.
fn hash_lines(trace: &Path, backend: &str) -> Vec<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_gltrace"))
        .arg("replay")
        .arg(trace)
        .args(["--per-draw", "--no-draws", "--backend", backend])
        .env_remove("LUCINANET_RASTER")
        .env_remove("LUCINANET_GLTRACE")
        .output()
        .expect("run gltrace");
    assert!(out.status.success(), "gltrace on {backend} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().lines().filter(|l| l.contains(" hash ")).map(str::to_owned).collect()
}

/// Replays `trace` on both backends; returns how many hash lines each op had.
fn compare(trace: &Path) -> std::collections::BTreeMap<String, usize> {
    let cxx = hash_lines(trace, "cxx");
    let rust = hash_lines(trace, "rust");
    assert!(!cxx.is_empty(), "{}: no hash lines", trace.display());
    for (a, b) in cxx.iter().zip(&rust) {
        assert_eq!(a, b, "{}: first differing call", trace.display());
    }
    assert_eq!(cxx.len(), rust.len(), "{}: the backends hashed a different number of calls", trace.display());
    let mut ops = std::collections::BTreeMap::new();
    for l in &cxx {
        *ops.entry(l.split_whitespace().nth(1).unwrap().to_owned()).or_default() += 1;
    }
    eprintln!("{}: {} calls identical, {ops:?}", trace.display(), cxx.len());
    ops
}

/// The recorded session of `page` (`$GLTRACE_TRACES/<page>/trace-*.bin`, by default the scratch directory the
/// capture script writes).
fn page_trace(page: &str) -> PathBuf {
    let root = std::env::var_os("GLTRACE_TRACES").map_or_else(
        || std::env::temp_dir().join("lucinanet-raster/traces"),
        PathBuf::from,
    );
    let dir = root.join(page);
    let found = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e} (set GLTRACE_TRACES to the traces directory)", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("trace-")));
    found.unwrap_or_else(|| panic!("no trace-*.bin in {}", dir.display()))
}

#[test]
fn bench_page_matches() {
    let ops = compare(&page_trace("bench"));
    assert!(ops.contains_key("Clear") && ops.contains_key("TexSubImage2D") && ops.contains_key("Composite"));
}

#[test]
fn example_page_matches() {
    let ops = compare(&page_trace("example"));
    assert!(ops.contains_key("Clear") && ops.contains_key("TexSubImage2D") && ops.contains_key("Composite"));
}

#[test]
fn github_page_matches() {
    let ops = compare(&page_trace("github"));
    assert!(ops.contains_key("BlitFramebuffer"));
}

fn pattern(width: usize, height: usize, seed: u8, bpp: usize) -> Vec<u8> {
    (0..width * height * bpp).map(|i| (i as u8).wrapping_mul(31) ^ seed).collect()
}

/// Records (on the C++ backend, in this process) a scene with the calls the pages above lack: uploads with a row
/// length and through an unpack buffer into RGBA8, R8 and RG8 textures, a scissored and an unscissored `Clear` on a
/// texture target, `ClearColorRect`, `CopyImageSubData` between textures and from a renderbuffer, `CopyTexSubImage2D`,
/// `ReadPixels` into a pack buffer and `InvalidateFramebuffer`. Then compares the backends on it.
#[test]
fn synthetic_scene_matches() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("gltrace-backends-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::env::set_var("LUCINANET_GLTRACE", &dir);
    swgl::select_backend(swgl::Backend::Cxx).unwrap();

    let ctx = swgl::Context::create();
    ctx.make_current();
    let (w, h) = (96, 80);
    let mut fb_mem = vec![0u32; w * h];
    ctx.init_default_framebuffer(0, 0, w as i32, h as i32, w as i32 * 4, fb_mem.as_mut_ptr() as *mut c_void);

    let tex = ctx.gen_textures(4);
    ctx.bind_texture(gl::TEXTURE_2D, tex[0]);
    ctx.tex_storage_2d(gl::TEXTURE_2D, 1, gl::RGBA8, 40, 36);
    ctx.tex_sub_image_2d(gl::TEXTURE_2D, 0, 0, 0, 40, 36, gl::BGRA, gl::UNSIGNED_BYTE, &pattern(40, 36, 1, 4));
    ctx.pixel_store_i(gl::UNPACK_ROW_LENGTH, 50);
    ctx.tex_sub_image_2d(gl::TEXTURE_2D, 0, 3, 2, 33, 30, gl::BGRA, gl::UNSIGNED_BYTE, &pattern(50, 30, 7, 4));
    ctx.pixel_store_i(gl::UNPACK_ROW_LENGTH, 0);
    // RGBA source data is swizzled to BGRA (convert_copy).
    ctx.tex_sub_image_2d(gl::TEXTURE_2D, 0, 0, 0, 8, 4, gl::RGBA, gl::UNSIGNED_BYTE, &pattern(8, 4, 9, 4));

    ctx.bind_texture(gl::TEXTURE_2D, tex[1]);
    ctx.tex_storage_2d(gl::TEXTURE_2D, 1, gl::R8, 33, 17);
    ctx.tex_sub_image_2d(gl::TEXTURE_2D, 0, 1, 1, 31, 15, gl::RED, gl::UNSIGNED_BYTE, &pattern(31, 15, 3, 1));
    ctx.bind_texture(gl::TEXTURE_2D, tex[2]);
    ctx.tex_storage_2d(gl::TEXTURE_2D, 1, gl::RG8, 21, 9);
    ctx.tex_sub_image_2d(gl::TEXTURE_2D, 0, 0, 0, 21, 9, gl::RG, gl::UNSIGNED_BYTE, &pattern(21, 9, 5, 2));

    // An upload through a pixel unpack buffer, at an offset, clipped by the buffer's end.
    let pbo = ctx.gen_buffers(1)[0];
    ctx.bind_buffer(gl::PIXEL_UNPACK_BUFFER, pbo);
    let src = pattern(16, 16, 11, 4);
    ctx.buffer_data_untyped(gl::PIXEL_UNPACK_BUFFER, src.len() as isize, src.as_ptr() as *const c_void, gl::STREAM_DRAW);
    ctx.bind_texture(gl::TEXTURE_2D, tex[0]);
    ctx.tex_sub_image_2d_pbo(gl::TEXTURE_2D, 0, 20, 20, 16, 16, gl::BGRA, gl::UNSIGNED_BYTE, 64);
    ctx.bind_buffer(gl::PIXEL_UNPACK_BUFFER, 0);

    // Clears into a texture render target: whole, then scissored, then a rectangle.
    let fbo = ctx.gen_framebuffers(2);
    ctx.bind_framebuffer(gl::FRAMEBUFFER, fbo[0]);
    ctx.bind_texture(gl::TEXTURE_2D, tex[3]);
    ctx.tex_storage_2d(gl::TEXTURE_2D, 1, gl::RGBA8, 64, 70);
    // Every byte written first: a pending delayed clear leaves rows as they are in memory, and the hashes read
    // them so. C++ `realloc`s new storage (here, memory the test just freed), Rust zero-fills it.
    ctx.tex_sub_image_2d(gl::TEXTURE_2D, 0, 0, 0, 64, 70, gl::BGRA, gl::UNSIGNED_BYTE, &pattern(64, 70, 13, 4));
    ctx.framebuffer_texture_2d(gl::DRAW_FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D, tex[3], 0);
    ctx.clear_color(0.25, 0.5, 0.75, 1.0);
    ctx.clear(gl::COLOR_BUFFER_BIT);
    ctx.enable(gl::SCISSOR_TEST);
    ctx.scissor(5, 37, 40, 20);
    ctx.clear_color(1.0, 0.0, 0.5, 0.5);
    ctx.clear(gl::COLOR_BUFFER_BIT);
    ctx.disable(gl::SCISSOR_TEST);
    ctx.clear_color_rect(fbo[0], -4, 60, 30, 30, 0.0, 1.0, 0.0, 1.0);

    // Copies: texture to texture (partly out of bounds is the caller's error; stay inside), into the cleared
    // target, and from a renderbuffer.
    unsafe {
        ctx.copy_image_sub_data(tex[0], gl::TEXTURE_2D, 0, 2, 3, 0, tex[3], gl::TEXTURE_2D, 0, 10, 41, 0, 30, 20, 1);
    }
    let rb = ctx.gen_renderbuffers(1)[0];
    ctx.bind_renderbuffer(gl::RENDERBUFFER, rb);
    ctx.renderbuffer_storage(gl::RENDERBUFFER, gl::RGBA8, 24, 24);
    ctx.bind_framebuffer(gl::FRAMEBUFFER, fbo[1]);
    ctx.framebuffer_renderbuffer(gl::DRAW_FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::RENDERBUFFER, rb);
    // Defined memory before the delayed clear, as for the texture target above.
    unsafe {
        ctx.copy_image_sub_data(tex[0], gl::TEXTURE_2D, 0, 0, 0, 0, rb, gl::RENDERBUFFER, 0, 0, 0, 0, 24, 24, 1);
    }
    ctx.clear_color(0.1, 0.2, 0.3, 0.4);
    ctx.clear(gl::COLOR_BUFFER_BIT);
    unsafe {
        ctx.copy_image_sub_data(rb, gl::RENDERBUFFER, 0, 0, 0, 0, tex[0], gl::TEXTURE_2D, 0, 4, 4, 0, 12, 12, 1);
    }
    // Read framebuffer fbo[0] into texture 0.
    ctx.bind_framebuffer(gl::READ_FRAMEBUFFER, fbo[0]);
    ctx.bind_texture(gl::TEXTURE_2D, tex[0]);
    ctx.copy_tex_sub_image_2d(gl::TEXTURE_2D, 0, 1, 1, 30, 50, 20, 15);
    // Read back through a pack buffer (the record holds no bytes; the replay must not fail).
    let pack = ctx.gen_buffers(1)[0];
    ctx.bind_buffer(gl::PIXEL_PACK_BUFFER, pack);
    ctx.buffer_data_untyped(gl::PIXEL_PACK_BUFFER, 64 * 70 * 4, std::ptr::null(), gl::STREAM_READ);
    // SAFETY: a pack buffer of 64 x 70 BGRA pixels is bound.
    unsafe { ctx.read_pixels_into_pbo(0, 0, 64, 70, gl::BGRA, gl::UNSIGNED_BYTE) };
    ctx.bind_buffer(gl::PIXEL_PACK_BUFFER, 0);
    // Inline readback: compared with the recording by the replayer itself.
    let back = ctx.read_pixels(3, 3, 9, 7, gl::BGRA, gl::UNSIGNED_BYTE);
    assert_eq!(back.len(), 9 * 7 * 4);
    ctx.invalidate_framebuffer(gl::DRAW_FRAMEBUFFER, &[gl::COLOR_ATTACHMENT0]);
    ctx.clear_color(0.0, 0.0, 0.0, 1.0);
    ctx.clear(gl::COLOR_BUFFER_BIT);
    ctx.bind_framebuffer(gl::FRAMEBUFFER, 0);
    ctx.clear_color_rect(0, 8, 8, 30, 20, 0.5, 0.5, 0.5, 1.0);
    ctx.trace_frame();
    ctx.destroy();

    let trace = swgl::trace::finish().expect("tracing was on");
    let ops = compare(&trace);
    for op in ["Clear", "ClearColorRect", "TexSubImage2D", "CopyImageSubData", "CopyTexSubImage2D"] {
        assert!(ops.contains_key(op), "the scene hashed no {op}");
    }
    drop(fb_mem);
}
