/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `lucinanet_raster::gl` against gl.cc itself (the oracle crate compiles it, strict floats, asserts out): the same
//! call sequence runs on both, and the bytes in memory and the values returned must be the same. The scripts cover
//! delayed clears (pending, partial through a scissor or an upload's skip rectangle, flushed), locks and zombies,
//! uploads with `GL_UNPACK_ROW_LENGTH` and through pixel unpack buffers, read-backs, copies, and `GetIntegerv`.
//!
//! Every texture a script reads lives in memory the script owns (`SetTextureBuffer`), filled with a pattern first:
//! gl.cc's own allocations are not zeroed, so their untouched bytes would differ from run to run.

// The trait mirrors GL calls, which take many arguments.
#![allow(clippy::too_many_arguments)]

use std::ffi::{c_void, CStr};
use std::sync::Mutex;

use lucinanet_raster::gl::defs::*;
use lucinanet_raster::gl::Context;
use lucinanet_raster_oracle::gl as cxx;

/// gl.cc keeps its current context in a static: one oracle script at a time.
static ORACLE: Mutex<()> = Mutex::new(());

/// The GL calls the scripts make, on either implementation.
trait Gl {
    fn gen_textures(&mut self, n: usize) -> Vec<u32>;
    fn gen_buffers(&mut self, n: usize) -> Vec<u32>;
    fn gen_framebuffers(&mut self, n: usize) -> Vec<u32>;
    fn bind_texture(&mut self, target: u32, t: u32);
    fn bind_buffer(&mut self, target: u32, b: u32);
    fn bind_framebuffer(&mut self, target: u32, f: u32);
    fn framebuffer_texture_2d(&mut self, target: u32, attachment: u32, t: u32);
    unsafe fn set_texture_buffer(&mut self, t: u32, format: u32, w: i32, h: i32, stride: i32, buf: *mut c_void);
    fn tex_storage_2d(&mut self, target: u32, format: u32, w: i32, h: i32);
    unsafe fn tex_sub_image_2d(&mut self, target: u32, x: i32, y: i32, w: i32, h: i32, format: u32, ty: u32, data: *const c_void);
    fn pixel_storei(&mut self, name: u32, v: i32);
    unsafe fn buffer_data(&mut self, target: u32, size: isize, data: *const c_void);
    fn clear_color(&mut self, c: [f32; 4]);
    fn clear(&mut self, mask: u32);
    fn clear_color_rect(&mut self, fbo: u32, x: i32, y: i32, w: i32, h: i32, c: [f32; 4]);
    fn enable(&mut self, cap: u32);
    fn disable(&mut self, cap: u32);
    fn scissor(&mut self, x: i32, y: i32, w: i32, h: i32);
    /// `GetColorBuffer`: pointer, width, height, stride.
    fn color_buffer(&mut self, fbo: u32, flush: bool) -> Option<(*mut u8, i32, i32, i32)>;
    unsafe fn read_pixels(&mut self, x: i32, y: i32, w: i32, h: i32, format: u32, ty: u32, data: *mut c_void);
    fn copy_image_sub_data(&mut self, src: u32, sx: i32, sy: i32, dst: u32, dx: i32, dy: i32, w: i32, h: i32);
    fn lock_texture(&mut self, t: u32) -> *mut c_void;
    unsafe fn lock_resource(&mut self, r: *mut c_void);
    unsafe fn unlock_resource(&mut self, r: *mut c_void) -> i32;
    unsafe fn resource_buffer(&mut self, r: *mut c_void) -> (*mut u8, i32, i32, i32);
    fn delete_texture(&mut self, t: u32);
    fn get_integerv(&mut self, pname: u32) -> i32;
    fn get_booleanv(&mut self, pname: u32) -> u8;
    fn get_error(&mut self) -> u32;
    fn get_string(&mut self, name: u32) -> Option<String>;
    fn get_stringi(&mut self, name: u32, i: u32) -> Option<String>;
}

struct Rust(Context);

impl Gl for Rust {
    fn gen_textures(&mut self, n: usize) -> Vec<u32> {
        let mut v = vec![0; n];
        self.0.gen_textures(&mut v);
        v
    }
    fn gen_buffers(&mut self, n: usize) -> Vec<u32> {
        let mut v = vec![0; n];
        self.0.gen_buffers(&mut v);
        v
    }
    fn gen_framebuffers(&mut self, n: usize) -> Vec<u32> {
        let mut v = vec![0; n];
        self.0.gen_framebuffers(&mut v);
        v
    }
    fn bind_texture(&mut self, target: u32, t: u32) {
        self.0.bind_texture(target, t)
    }
    fn bind_buffer(&mut self, target: u32, b: u32) {
        self.0.bind_buffer(target, b)
    }
    fn bind_framebuffer(&mut self, target: u32, f: u32) {
        self.0.bind_framebuffer(target, f)
    }
    fn framebuffer_texture_2d(&mut self, target: u32, attachment: u32, t: u32) {
        self.0.framebuffer_texture_2d(target, attachment, GL_TEXTURE_2D, t, 0)
    }
    unsafe fn set_texture_buffer(&mut self, t: u32, format: u32, w: i32, h: i32, stride: i32, buf: *mut c_void) {
        unsafe { self.0.set_texture_buffer(t, format, w, h, stride, buf, 0, 0) }
    }
    fn tex_storage_2d(&mut self, target: u32, format: u32, w: i32, h: i32) {
        self.0.tex_storage_2d(target, 1, format, w, h)
    }
    unsafe fn tex_sub_image_2d(&mut self, target: u32, x: i32, y: i32, w: i32, h: i32, format: u32, ty: u32, data: *const c_void) {
        unsafe { self.0.tex_sub_image_2d(target, 0, x, y, w, h, format, ty, data) }
    }
    fn pixel_storei(&mut self, name: u32, v: i32) {
        self.0.pixel_storei(name, v)
    }
    unsafe fn buffer_data(&mut self, target: u32, size: isize, data: *const c_void) {
        unsafe { self.0.buffer_data(target, size, data, 0) }
    }
    fn clear_color(&mut self, c: [f32; 4]) {
        self.0.clear_color(c[0], c[1], c[2], c[3])
    }
    fn clear(&mut self, mask: u32) {
        self.0.clear(mask)
    }
    fn clear_color_rect(&mut self, fbo: u32, x: i32, y: i32, w: i32, h: i32, c: [f32; 4]) {
        self.0.clear_color_rect(fbo, x, y, w, h, c[0], c[1], c[2], c[3])
    }
    fn enable(&mut self, cap: u32) {
        self.0.enable(cap)
    }
    fn disable(&mut self, cap: u32) {
        self.0.disable(cap)
    }
    fn scissor(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.0.set_scissor(x, y, w, h)
    }
    fn color_buffer(&mut self, fbo: u32, flush: bool) -> Option<(*mut u8, i32, i32, i32)> {
        self.0.get_color_buffer(fbo, flush)
    }
    unsafe fn read_pixels(&mut self, x: i32, y: i32, w: i32, h: i32, format: u32, ty: u32, data: *mut c_void) {
        unsafe { self.0.read_pixels(x, y, w, h, format, ty, data) }
    }
    fn copy_image_sub_data(&mut self, src: u32, sx: i32, sy: i32, dst: u32, dx: i32, dy: i32, w: i32, h: i32) {
        self.0.copy_image_sub_data(src, GL_TEXTURE_2D, 0, sx, sy, 0, dst, GL_TEXTURE_2D, 0, dx, dy, 0, w, h, 1)
    }
    fn lock_texture(&mut self, t: u32) -> *mut c_void {
        self.0.lock_texture(t) as *mut c_void
    }
    unsafe fn lock_resource(&mut self, r: *mut c_void) {
        unsafe { lucinanet_raster::gl::lock_resource(r as *mut _) }
    }
    unsafe fn unlock_resource(&mut self, r: *mut c_void) -> i32 {
        unsafe { lucinanet_raster::gl::unlock_resource(r as *mut _) }
    }
    unsafe fn resource_buffer(&mut self, r: *mut c_void) -> (*mut u8, i32, i32, i32) {
        unsafe { lucinanet_raster::gl::get_resource_buffer(r as *mut _) }
    }
    fn delete_texture(&mut self, t: u32) {
        self.0.delete_texture(t)
    }
    fn get_integerv(&mut self, pname: u32) -> i32 {
        self.0.get_integerv(pname).unwrap_or(-12345)
    }
    fn get_booleanv(&mut self, pname: u32) -> u8 {
        self.0.get_booleanv(pname).unwrap_or(77)
    }
    fn get_error(&mut self) -> u32 {
        self.0.get_error()
    }
    fn get_string(&mut self, name: u32) -> Option<String> {
        Context::get_string(name).map(|s| s.to_string_lossy().into_owned())
    }
    fn get_stringi(&mut self, name: u32, i: u32) -> Option<String> {
        Context::get_stringi(name, i).map(|s| s.to_string_lossy().into_owned())
    }
}

/// gl.cc through the oracle; the context is current for the lifetime of the value.
struct Cxx(*mut c_void);

impl Cxx {
    fn new() -> Self {
        unsafe {
            let c = cxx::CreateContext();
            cxx::MakeCurrent(c);
            Cxx(c)
        }
    }
}

impl Drop for Cxx {
    fn drop(&mut self) {
        unsafe {
            cxx::MakeCurrent(std::ptr::null_mut());
            cxx::DestroyContext(self.0);
        }
    }
}

fn gen(n: usize, f: unsafe extern "C" fn(i32, *mut u32)) -> Vec<u32> {
    let mut v = vec![0; n];
    unsafe { f(n as i32, v.as_mut_ptr()) };
    v
}

fn cstr(p: *const std::ffi::c_char) -> Option<String> {
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

impl Gl for Cxx {
    fn gen_textures(&mut self, n: usize) -> Vec<u32> {
        gen(n, cxx::GenTextures)
    }
    fn gen_buffers(&mut self, n: usize) -> Vec<u32> {
        gen(n, cxx::GenBuffers)
    }
    fn gen_framebuffers(&mut self, n: usize) -> Vec<u32> {
        gen(n, cxx::GenFramebuffers)
    }
    fn bind_texture(&mut self, target: u32, t: u32) {
        unsafe { cxx::BindTexture(target, t) }
    }
    fn bind_buffer(&mut self, target: u32, b: u32) {
        unsafe { cxx::BindBuffer(target, b) }
    }
    fn bind_framebuffer(&mut self, target: u32, f: u32) {
        unsafe { cxx::BindFramebuffer(target, f) }
    }
    fn framebuffer_texture_2d(&mut self, target: u32, attachment: u32, t: u32) {
        unsafe { cxx::FramebufferTexture2D(target, attachment, GL_TEXTURE_2D, t, 0) }
    }
    unsafe fn set_texture_buffer(&mut self, t: u32, format: u32, w: i32, h: i32, stride: i32, buf: *mut c_void) {
        unsafe { cxx::SetTextureBuffer(t, format, w, h, stride, buf, 0, 0) }
    }
    fn tex_storage_2d(&mut self, target: u32, format: u32, w: i32, h: i32) {
        unsafe { cxx::TexStorage2D(target, 1, format, w, h) }
    }
    unsafe fn tex_sub_image_2d(&mut self, target: u32, x: i32, y: i32, w: i32, h: i32, format: u32, ty: u32, data: *const c_void) {
        unsafe { cxx::TexSubImage2D(target, 0, x, y, w, h, format, ty, data) }
    }
    fn pixel_storei(&mut self, name: u32, v: i32) {
        unsafe { cxx::PixelStorei(name, v) }
    }
    unsafe fn buffer_data(&mut self, target: u32, size: isize, data: *const c_void) {
        unsafe { cxx::BufferData(target, size, data, 0) }
    }
    fn clear_color(&mut self, c: [f32; 4]) {
        unsafe { cxx::ClearColor(c[0], c[1], c[2], c[3]) }
    }
    fn clear(&mut self, mask: u32) {
        unsafe { cxx::Clear(mask) }
    }
    fn clear_color_rect(&mut self, fbo: u32, x: i32, y: i32, w: i32, h: i32, c: [f32; 4]) {
        unsafe { cxx::ClearColorRect(fbo, x, y, w, h, c[0], c[1], c[2], c[3]) }
    }
    fn enable(&mut self, cap: u32) {
        unsafe { cxx::Enable(cap) }
    }
    fn disable(&mut self, cap: u32) {
        unsafe { cxx::Disable(cap) }
    }
    fn scissor(&mut self, x: i32, y: i32, w: i32, h: i32) {
        unsafe { cxx::SetScissor(x, y, w, h) }
    }
    fn color_buffer(&mut self, fbo: u32, flush: bool) -> Option<(*mut u8, i32, i32, i32)> {
        let (mut w, mut h, mut s) = (-1, -1, -1);
        let p = unsafe { cxx::GetColorBuffer(fbo, flush as u8, &mut w, &mut h, &mut s) };
        (w != -1).then_some((p as *mut u8, w, h, s))
    }
    unsafe fn read_pixels(&mut self, x: i32, y: i32, w: i32, h: i32, format: u32, ty: u32, data: *mut c_void) {
        unsafe { cxx::ReadPixels(x, y, w, h, format, ty, data) }
    }
    fn copy_image_sub_data(&mut self, src: u32, sx: i32, sy: i32, dst: u32, dx: i32, dy: i32, w: i32, h: i32) {
        unsafe { cxx::CopyImageSubData(src, GL_TEXTURE_2D, 0, sx, sy, 0, dst, GL_TEXTURE_2D, 0, dx, dy, 0, w, h, 1) }
    }
    fn lock_texture(&mut self, t: u32) -> *mut c_void {
        unsafe { cxx::LockTexture(t) }
    }
    unsafe fn lock_resource(&mut self, r: *mut c_void) {
        unsafe { cxx::LockResource(r) }
    }
    unsafe fn unlock_resource(&mut self, r: *mut c_void) -> i32 {
        unsafe { cxx::UnlockResource(r) }
    }
    unsafe fn resource_buffer(&mut self, r: *mut c_void) -> (*mut u8, i32, i32, i32) {
        let (mut w, mut h, mut s) = (0, 0, 0);
        let p = unsafe { cxx::GetResourceBuffer(r, &mut w, &mut h, &mut s) };
        (p as *mut u8, w, h, s)
    }
    fn delete_texture(&mut self, t: u32) {
        unsafe { cxx::DeleteTexture(t) }
    }
    fn get_integerv(&mut self, pname: u32) -> i32 {
        let mut v = [-12345i32; 4];
        unsafe { cxx::GetIntegerv(pname, v.as_mut_ptr()) };
        v[0]
    }
    fn get_booleanv(&mut self, pname: u32) -> u8 {
        let mut v = [77u8; 4];
        unsafe { cxx::GetBooleanv(pname, v.as_mut_ptr()) };
        v[0]
    }
    fn get_error(&mut self) -> u32 {
        unsafe { cxx::GetError() }
    }
    fn get_string(&mut self, name: u32) -> Option<String> {
        cstr(unsafe { cxx::GetString(name) })
    }
    fn get_stringi(&mut self, name: u32, i: u32) -> Option<String> {
        cstr(unsafe { cxx::GetStringi(name, i) })
    }
}

/// Runs `script` on gl.cc and on the Rust context; each returns what it observed, and the two must be equal.
fn same<T: PartialEq + std::fmt::Debug>(script: impl Fn(&mut dyn Gl) -> T) -> T {
    let want = {
        let _one = ORACLE.lock().unwrap_or_else(|e| e.into_inner());
        let mut c = Cxx::new();
        script(&mut c)
    };
    let got = script(&mut Rust(Context::new()));
    assert_eq!(got, want, "Rust (left) and gl.cc (right) differ");
    got
}

fn pattern(n: usize, seed: u32) -> Vec<u8> {
    (0..n as u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13 ^ seed) as u8).collect()
}

/// The `stride * height` bytes of a color buffer.
fn bytes(p: Option<(*mut u8, i32, i32, i32)>) -> Option<(i32, i32, i32, Vec<u8>)> {
    let (p, w, h, s) = p?;
    if p.is_null() {
        return Some((w, h, s, Vec::new()));
    }
    Some((w, h, s, unsafe { std::slice::from_raw_parts(p, (s * h) as usize) }.to_vec()))
}

/// A framebuffer whose color attachment is a `w` by `h` RGBA8 texture in `mem` (filled with a pattern).
fn target(gl: &mut dyn Gl, mem: &mut Vec<u8>, w: i32, h: i32, stride: i32) -> (u32, u32) {
    *mem = pattern((stride * h) as usize, 0x5a);
    let t = gl.gen_textures(1)[0];
    unsafe { gl.set_texture_buffer(t, GL_RGBA8, w, h, stride, mem.as_mut_ptr() as *mut c_void) };
    let f = gl.gen_framebuffers(1)[0];
    gl.bind_framebuffer(GL_FRAMEBUFFER, f);
    gl.framebuffer_texture_2d(GL_DRAW_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, t);
    (t, f)
}

#[test]
fn delayed_clears_match_gl_cc() {
    same(|gl| {
        let mut mem = Vec::new();
        let mut seen = Vec::new();
        // 75 rows: the row mask's last word is partly full.
        let (t, f) = target(gl, &mut mem, 53, 75, 53 * 4 + 12);
        gl.clear_color([0.2, 0.4, 0.6, 1.0]);
        gl.clear(GL_COLOR_BUFFER_BIT);
        // Pending: memory untouched.
        seen.push(bytes(gl.color_buffer(f, false)));
        // An upload's skip rectangle: rows outside it are cleared now, its own rows outside the rectangle too.
        let px = pattern(20 * 9 * 4, 3);
        gl.bind_texture(GL_TEXTURE_2D, t);
        unsafe { gl.tex_sub_image_2d(GL_TEXTURE_2D, 7, 40, 20, 9, GL_BGRA, GL_UNSIGNED_BYTE, px.as_ptr() as *const c_void) };
        seen.push(bytes(gl.color_buffer(f, false)));
        // A new delayed clear, then a scissored one (cleared at once, the rest of the rows flushed).
        gl.clear_color([1.0, 0.0, 0.0, 0.5]);
        gl.clear(GL_COLOR_BUFFER_BIT);
        gl.enable(GL_SCISSOR_TEST);
        gl.scissor(-3, 30, 30, 50);
        gl.clear_color([0.0, 1.0, 0.0, 1.0]);
        gl.clear(GL_COLOR_BUFFER_BIT);
        gl.disable(GL_SCISSOR_TEST);
        seen.push(bytes(gl.color_buffer(f, false)));
        // ClearColorRect covering the texture: delayed again; a partial one: immediate.
        gl.clear_color_rect(f, -10, -10, 100, 100, [0.0, 0.0, 1.0, 1.0]);
        seen.push(bytes(gl.color_buffer(f, false)));
        gl.clear_color_rect(f, 5, 70, 10, 30, [1.0, 1.0, 0.0, 1.0]);
        seen.push(bytes(gl.color_buffer(f, false)));
        // Flushing applies what is left.
        seen.push(bytes(gl.color_buffer(f, true)));
        seen
    });
}

#[test]
fn delayed_clear_of_r8_and_rg8_matches_gl_cc() {
    same(|gl| {
        let mut seen = Vec::new();
        for (format, bpp) in [(GL_R8, 1), (GL_RG8, 2)] {
            let (w, h) = (37, 41);
            let stride = ((w * bpp + 3) & !3) + 4;
            let mut mem = pattern((stride * h) as usize, 9);
            let t = gl.gen_textures(1)[0];
            unsafe { gl.set_texture_buffer(t, format, w, h, stride, mem.as_mut_ptr() as *mut c_void) };
            let f = gl.gen_framebuffers(1)[0];
            gl.bind_framebuffer(GL_FRAMEBUFFER, f);
            gl.framebuffer_texture_2d(GL_DRAW_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, t);
            gl.clear_color([0.7, 0.3, 0.0, 0.0]);
            gl.clear(GL_COLOR_BUFFER_BIT);
            gl.clear_color_rect(f, 3, 3, 10, 50, [0.1, 0.9, 0.0, 0.0]);
            seen.push(bytes(gl.color_buffer(f, false)));
            seen.push(bytes(gl.color_buffer(f, true)));
            seen.push(Some((0, 0, 0, mem.clone())));
        }
        seen
    });
}

#[test]
fn unpack_row_length_and_unpack_buffers_match_gl_cc() {
    same(|gl| {
        let mut seen = Vec::new();
        let mut mem = Vec::new();
        let (t, f) = target(gl, &mut mem, 48, 40, 48 * 4);
        gl.bind_texture(GL_TEXTURE_2D, t);
        // Rows 70 pixels apart in the source.
        let src = pattern(70 * 30 * 4, 1);
        gl.pixel_storei(GL_UNPACK_ROW_LENGTH, 70);
        unsafe { gl.tex_sub_image_2d(GL_TEXTURE_2D, 2, 3, 41, 30, GL_BGRA, GL_UNSIGNED_BYTE, src.as_ptr() as *const c_void) };
        // RGBA data is swizzled.
        unsafe { gl.tex_sub_image_2d(GL_TEXTURE_2D, 0, 34, 20, 5, GL_RGBA, GL_UNSIGNED_BYTE, src.as_ptr() as *const c_void) };
        gl.pixel_storei(GL_UNPACK_ROW_LENGTH, 0);
        seen.push(bytes(gl.color_buffer(f, true)));
        // Through an unpack buffer, at an offset, the last rows past the buffer's end (clipped).
        let b = gl.gen_buffers(1)[0];
        gl.bind_buffer(GL_PIXEL_UNPACK_BUFFER, b);
        let pb = pattern(25 * 20 * 4, 2);
        unsafe { gl.buffer_data(GL_PIXEL_UNPACK_BUFFER, pb.len() as isize, pb.as_ptr() as *const c_void) };
        unsafe { gl.tex_sub_image_2d(GL_TEXTURE_2D, 20, 15, 25, 25, GL_BGRA, GL_UNSIGNED_BYTE, 100 as *const c_void) };
        // An offset past the end: nothing.
        unsafe { gl.tex_sub_image_2d(GL_TEXTURE_2D, 0, 0, 4, 4, GL_BGRA, GL_UNSIGNED_BYTE, 1_000_000 as *const c_void) };
        gl.bind_buffer(GL_PIXEL_UNPACK_BUFFER, 0);
        seen.push(bytes(gl.color_buffer(f, true)));
        // An upload hanging off the texture's start and end is clipped to the buffer.
        unsafe { gl.tex_sub_image_2d(GL_TEXTURE_2D, -5, -2, 20, 6, GL_BGRA, GL_UNSIGNED_BYTE, src.as_ptr() as *const c_void) };
        unsafe { gl.tex_sub_image_2d(GL_TEXTURE_2D, 40, 37, 20, 6, GL_BGRA, GL_UNSIGNED_BYTE, src.as_ptr() as *const c_void) };
        seen.push(bytes(gl.color_buffer(f, true)));
        // RED into an internally allocated R8 texture, then read back.
        let r = gl.gen_textures(1)[0];
        gl.bind_texture(GL_TEXTURE_2D, r);
        gl.tex_storage_2d(GL_TEXTURE_2D, GL_R8, 13, 7);
        let red = pattern(13 * 7, 4);
        unsafe { gl.tex_sub_image_2d(GL_TEXTURE_2D, 0, 0, 13, 7, GL_RED, GL_UNSIGNED_BYTE, red.as_ptr() as *const c_void) };
        let rf = gl.gen_framebuffers(1)[0];
        gl.bind_framebuffer(GL_FRAMEBUFFER, rf);
        gl.framebuffer_texture_2d(GL_READ_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, r);
        let mut back = vec![0u8; 13 * 7];
        unsafe { gl.read_pixels(0, 0, 13, 7, GL_RED, GL_UNSIGNED_BYTE, back.as_mut_ptr() as *mut c_void) };
        assert_eq!(back, red);
        seen.push(Some((0, 0, 0, back)));
        seen.push(Some((0, 0, 0, mem.clone())));
        seen
    });
}

#[test]
fn read_pixels_and_copies_match_gl_cc() {
    same(|gl| {
        let mut seen = Vec::new();
        let (mut ma, mut mb) = (Vec::new(), Vec::new());
        let (a, fa) = target(gl, &mut ma, 30, 22, 30 * 4 + 8);
        let (b, fb) = target(gl, &mut mb, 19, 17, 19 * 4);
        gl.clear_color([0.5, 0.25, 0.125, 1.0]);
        gl.bind_framebuffer(GL_FRAMEBUFFER, fb);
        gl.clear(GL_COLOR_BUFFER_BIT);
        // A copy into a target with a pending clear: it is applied outside the copied rectangle first.
        gl.copy_image_sub_data(a, 2, 3, b, 4, 5, 12, 9);
        seen.push(bytes(gl.color_buffer(fb, false)));
        // Hanging off the end of the destination: each row clipped.
        gl.copy_image_sub_data(a, 0, 0, b, 10, 12, 15, 10);
        seen.push(bytes(gl.color_buffer(fb, true)));
        // Read back, clipped at every side, rows out of range left as they were.
        gl.bind_framebuffer(GL_READ_FRAMEBUFFER, fa);
        let mut back = vec![0xEEu8; 40 * 30 * 4];
        unsafe { gl.read_pixels(-5, -4, 40, 30, GL_BGRA, GL_UNSIGNED_BYTE, back.as_mut_ptr() as *mut c_void) };
        seen.push(Some((0, 0, 0, back)));
        // Into a pack buffer.
        let pbo = gl.gen_buffers(1)[0];
        gl.bind_buffer(GL_PIXEL_PACK_BUFFER, pbo);
        unsafe { gl.buffer_data(GL_PIXEL_PACK_BUFFER, 500, std::ptr::null()) };
        unsafe { gl.read_pixels(1, 1, 10, 10, GL_BGRA, GL_UNSIGNED_BYTE, 8 as *mut c_void) };
        gl.bind_buffer(GL_PIXEL_PACK_BUFFER, 0);
        // A mismatched format reads nothing in release; debug gl.cc stops on `assert(false)` (swgl/src/gl.cc:2841),
        // as the Rust does, so it is probed only without debug asserts.
        if !cfg!(debug_assertions) {
            let mut none = vec![0x11u8; 64];
            unsafe { gl.read_pixels(0, 0, 4, 4, GL_RED, GL_UNSIGNED_BYTE, none.as_mut_ptr() as *mut c_void) };
            seen.push(Some((0, 0, 0, none)));
        }
        seen.push(Some((0, 0, 0, ma.clone())));
        seen.push(Some((0, 0, 0, mb.clone())));
        seen
    });
}

#[test]
fn locks_and_zombies_match_gl_cc() {
    same(|gl| {
        let mut seen: Vec<i64> = Vec::new();
        let mut mem = Vec::new();
        let (t, f) = target(gl, &mut mem, 16, 16, 64);
        gl.clear_color([0.0, 0.0, 0.0, 1.0]);
        gl.clear(GL_COLOR_BUFFER_BIT);
        // No buffer: no lock.
        let empty = gl.gen_textures(1)[0];
        seen.push(gl.lock_texture(empty).is_null() as i64);
        // The first lock applies the delayed clear.
        let r = gl.lock_texture(t);
        seen.push(r.is_null() as i64);
        seen.push(i64::from(mem[0]) | i64::from(mem[63 * 16]) << 8);
        unsafe { gl.lock_resource(r) };
        let r2 = gl.lock_texture(t);
        seen.push((r2 == r) as i64);
        // Deleted while locked: a zombie, still readable through its handle.
        gl.delete_texture(t);
        let (p, w, h, s) = unsafe { gl.resource_buffer(r) };
        seen.extend([(p == mem.as_mut_ptr()) as i64, i64::from(w), i64::from(h), i64::from(s)]);
        // The name is free again, and the framebuffer still names it (gl.cc does not unlink attachments).
        let again = gl.gen_textures(1)[0];
        seen.push((again == t) as i64);
        seen.push(gl.color_buffer(f, false).map_or(-1, |c| c.1 as i64));
        // The locks count down; the last unlock frees the zombie.
        for _ in 0..3 {
            seen.push(i64::from(unsafe { gl.unlock_resource(r) }));
        }
        seen.push(i64::from(unsafe { gl.unlock_resource(std::ptr::null_mut()) }));
        seen
    });
}

#[test]
fn get_integerv_and_strings_match_gl_cc() {
    same(|gl| {
        let mut seen = Vec::new();
        let mut pnames = vec![
            GL_MAX_TEXTURE_UNITS,
            GL_MAX_TEXTURE_IMAGE_UNITS,
            GL_MAX_TEXTURE_SIZE,
            GL_MAX_ARRAY_TEXTURE_LAYERS,
            GL_READ_FRAMEBUFFER_BINDING,
            GL_DRAW_FRAMEBUFFER_BINDING,
            GL_PIXEL_PACK_BUFFER_BINDING,
            GL_PIXEL_UNPACK_BUFFER_BINDING,
            GL_NUM_EXTENSIONS,
            GL_MAJOR_VERSION,
            GL_MINOR_VERSION,
            GL_MIN_PROGRAM_TEXEL_OFFSET,
            GL_MAX_PROGRAM_TEXEL_OFFSET,
        ];
        // GL_DEPTH_TEST is not handled: release gl.cc writes nothing, debug gl.cc stops on `assert(false)`
        // (swgl/src/gl.cc:1225, :1237), as the Rust does, so the rejected pname is probed only without debug asserts.
        let unhandled = !cfg!(debug_assertions);
        if unhandled {
            pnames.push(0x0B71);
        }
        let read = |gl: &mut dyn Gl| pnames.iter().map(|&p| gl.get_integerv(p)).collect::<Vec<_>>();
        seen.push(read(gl));
        let f = gl.gen_framebuffers(3);
        let b = gl.gen_buffers(2);
        gl.bind_framebuffer(GL_READ_FRAMEBUFFER, f[1]);
        gl.bind_framebuffer(GL_DRAW_FRAMEBUFFER, f[2]);
        gl.bind_buffer(GL_PIXEL_PACK_BUFFER, b[0]);
        gl.bind_buffer(GL_PIXEL_UNPACK_BUFFER, b[1]);
        seen.push(read(gl));
        gl.bind_framebuffer(GL_FRAMEBUFFER, f[0]);
        seen.push(read(gl));
        seen.push(vec![i32::from(gl.get_booleanv(GL_DEPTH_WRITEMASK))]);
        if unhandled {
            seen.push(vec![i32::from(gl.get_booleanv(0x0B71))]);
        }
        seen.push(vec![gl.get_error() as i32]);
        let mut strings: Vec<Option<String>> = Vec::new();
        for n in [GL_VENDOR, GL_RENDERER, GL_VERSION, GL_SHADING_LANGUAGE_VERSION, 0x1234] {
            strings.push(gl.get_string(n));
        }
        for i in 0..14 {
            strings.push(gl.get_stringi(GL_EXTENSIONS, i));
        }
        (seen, strings)
    });
}
