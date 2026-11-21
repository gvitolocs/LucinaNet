/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The Rust rasterizer behind swgl's `extern "C"` surface: one function per `cxx` function, same name and
//! signature, calling `lucinanet_raster::gl`. `cxx::F` routes here when the backend is Rust, which is the default
//! (2026-11-11): `LUCINANET_RASTER` unset, empty or `rust`, read once at the first `CreateContext`, or [`select`] before
//! it. `LUCINANET_RASTER=swgl` selects swgl's C++, which stays in the build as the oracle and the benchmark.
//!
//! gl.cc keeps the current context in a static; so does this module ([`CTX`]). As in the C++, a GL call with no
//! current context is the caller's error; here it does nothing (and returns zero or null) instead of crashing.
#![allow(non_snake_case, clippy::missing_safety_doc, clippy::too_many_arguments)]

use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicPtr, AtomicU8, Ordering};

use gleam::gl::*;
use lucinanet_raster::gl::{self as rgl, ClearData, Context, Texture};

use crate::swgl_fns::{LockedTexture, YuvRangedColorSpace};

const UNDECIDED: u8 = 0;
const CXX: u8 = 1;
const RUST: u8 = 2;

static BACKEND: AtomicU8 = AtomicU8::new(UNDECIDED);

/// The rasterizer behind the GL entry points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// swgl's C++ (gl.cc).
    Cxx,
    /// `lucinanet_raster`.
    Rust,
}

/// Chooses the backend for this process. It must come before the first `CreateContext`, which otherwise reads
/// `LUCINANET_RASTER`; afterwards only the backend already in use is accepted. The call that decides says which
/// rasterizer is in use, once, on stderr.
pub fn select(b: Backend) -> Result<(), String> {
    let want = if b == Backend::Rust { RUST } else { CXX };
    match BACKEND.compare_exchange(UNDECIDED, want, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {
            eprintln!("{}", describe(b, false));
            Ok(())
        },
        Err(have) if have == want => Ok(()),
        Err(_) => Err(format!("the {:?} backend was asked for after contexts were made with the other", b)),
    }
}

/// The backend in use (`Rust`, the default, until one is chosen).
pub fn current() -> Backend {
    if on() {
        Backend::Rust
    } else {
        Backend::Cxx
    }
}

/// Decides the backend from `LUCINANET_RASTER` if nothing chose it yet (the `CreateContext` dispatch calls this).
/// The call that decides says which rasterizer is in use, once, on stderr, so a run is not judged from the picture.
pub(crate) fn init() {
    if BACKEND.load(Ordering::Acquire) != UNDECIDED {
        return;
    }
    let value = std::env::var("LUCINANET_RASTER").ok();
    let (backend, warning) = backend_from_env(value.as_deref());
    if let Some(w) = warning {
        eprintln!("{w}");
    }
    let want = if backend == Backend::Rust { RUST } else { CXX };
    if BACKEND.compare_exchange(UNDECIDED, want, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        // The variable named this backend if it was set to its own spelling (`rust`/`swgl`), not left to the default.
        eprintln!("{}", describe(backend, value.as_deref() == Some(label(backend))));
    }
}

/// The start-up line saying which rasterizer runs, as the engine's own lines read. `from_env` is whether
/// `LUCINANET_RASTER` asked for this backend by name, in which case the line quotes it.
pub(crate) fn describe(b: Backend, from_env: bool) -> String {
    let name = label(b);
    if from_env {
        format!("lucinanet: rasterizer {name} (LUCINANET_RASTER={name})")
    } else {
        format!("lucinanet: rasterizer {name}")
    }
}

/// What each backend calls itself in that line and in `LUCINANET_RASTER`.
fn label(b: Backend) -> &'static str {
    match b {
        Backend::Rust => "rust",
        Backend::Cxx => "swgl",
    }
}

/// The backend `LUCINANET_RASTER` asks for, and a warning for a value that is neither `rust` nor `swgl`: unset, empty
/// or `rust` is Rust (the default), `swgl` is the C++, anything else is Rust with a warning.
pub(crate) fn backend_from_env(value: Option<&str>) -> (Backend, Option<String>) {
    match value {
        None | Some("") | Some("rust") => (Backend::Rust, None),
        Some("swgl") => (Backend::Cxx, None),
        Some(v) => (Backend::Rust, Some(format!("swgl: LUCINANET_RASTER={v} is not rust or swgl; using rust"))),
    }
}

#[inline(always)]
/// True unless the C++ was chosen: a call before the first `CreateContext` (which decides) gets the default, Rust.
pub(crate) fn on() -> bool {
    BACKEND.load(Ordering::Relaxed) != CXX
}

/// gl.cc's `ctx`.
static CTX: AtomicPtr<Context> = AtomicPtr::new(std::ptr::null_mut());

/// The current context. Only the GL thread calls the functions that use it, as with gl.cc's `ctx`.
#[inline(always)]
unsafe fn ctx() -> Option<&'static mut Context> {
    // SAFETY: `CTX` is null or a context `CreateContext` made and `DestroyContext` has not freed (it clears `CTX`
    // when it frees the current one); only the GL thread uses it.
    unsafe { CTX.load(Ordering::Relaxed).as_mut() }
}

macro_rules! with_ctx {
    ($c:ident => $e:expr) => {
        match unsafe { ctx() } {
            Some($c) => $e,
            None => Default::default(),
        }
    };
}

fn tex(p: *mut LockedTexture) -> *mut Texture {
    p as *mut Texture
}

unsafe fn ids<'a>(n: GLsizei, result: *mut GLuint) -> &'a mut [GLuint] {
    if result.is_null() || n <= 0 {
        return &mut [];
    }
    // SAFETY: the caller passes room for `n` ids, as gl.cc assumes.
    unsafe { std::slice::from_raw_parts_mut(result, n as usize) }
}

unsafe fn name<'a>(p: *const GLchar) -> std::borrow::Cow<'a, str> {
    if p.is_null() {
        return "".into();
    }
    // SAFETY: a NUL-terminated string, as gl.cc reads it.
    unsafe { CStr::from_ptr(p) }.to_string_lossy()
}

pub unsafe fn ActiveTexture(texture: GLenum) {
    with_ctx!(c => c.active_texture(texture))
}
pub unsafe fn BindTexture(target: GLenum, texture: GLuint) {
    with_ctx!(c => c.bind_texture(target, texture))
}
pub unsafe fn BindBuffer(target: GLenum, buffer: GLuint) {
    with_ctx!(c => c.bind_buffer(target, buffer))
}
pub unsafe fn BindVertexArray(vao: GLuint) {
    with_ctx!(c => c.bind_vertex_array(vao))
}
pub unsafe fn BindFramebuffer(target: GLenum, fb: GLuint) {
    with_ctx!(c => c.bind_framebuffer(target, fb))
}
pub unsafe fn BindRenderbuffer(target: GLenum, rb: GLuint) {
    with_ctx!(c => c.bind_renderbuffer(target, rb))
}
pub unsafe fn BlendFunc(srgb: GLenum, drgb: GLenum, sa: GLenum, da: GLenum) {
    with_ctx!(c => c.blend_func(srgb, drgb, sa, da))
}
pub unsafe fn BlendColor(r: GLfloat, g: GLfloat, b: GLfloat, a: GLfloat) {
    with_ctx!(c => c.blend_color(r, g, b, a))
}
pub unsafe fn BlendEquation(mode: GLenum) {
    with_ctx!(c => c.blend_equation(mode))
}
pub unsafe fn Enable(cap: GLenum) {
    with_ctx!(c => c.enable(cap))
}
pub unsafe fn Disable(cap: GLenum) {
    with_ctx!(c => c.disable(cap))
}
pub unsafe fn GenQueries(n: GLsizei, result: *mut GLuint) {
    with_ctx!(c => c.gen_queries(unsafe { ids(n, result) }))
}
pub unsafe fn BeginQuery(target: GLenum, id: GLuint) {
    with_ctx!(c => c.begin_query(target, id))
}
pub unsafe fn EndQuery(target: GLenum) {
    with_ctx!(c => c.end_query(target))
}
pub unsafe fn GetQueryObjectui64v(id: GLuint, pname: GLenum, params: *mut GLuint64) {
    with_ctx!(c => if let Some(v) = c.get_query_objectui64v(id, pname) {
        if !params.is_null() {
            unsafe { *params = v };
        }
    })
}
pub unsafe fn GenBuffers(n: i32, result: *mut GLuint) {
    with_ctx!(c => c.gen_buffers(unsafe { ids(n, result) }))
}
pub unsafe fn GenTextures(n: i32, result: *mut GLuint) {
    with_ctx!(c => c.gen_textures(unsafe { ids(n, result) }))
}
pub unsafe fn GenFramebuffers(n: i32, result: *mut GLuint) {
    with_ctx!(c => c.gen_framebuffers(unsafe { ids(n, result) }))
}
pub unsafe fn GenRenderbuffers(n: i32, result: *mut GLuint) {
    with_ctx!(c => c.gen_renderbuffers(unsafe { ids(n, result) }))
}
pub unsafe fn BufferData(target: GLenum, size: GLsizeiptr, data: *const GLvoid, usage: GLenum) {
    with_ctx!(c => unsafe { c.buffer_data(target, size, data, usage) })
}
pub unsafe fn BufferSubData(target: GLenum, offset: GLintptr, size: GLsizeiptr, data: *const GLvoid) {
    with_ctx!(c => unsafe { c.buffer_sub_data(target, offset, size, data) })
}
pub unsafe fn MapBuffer(target: GLenum, access: GLbitfield) -> *mut c_void {
    match unsafe { ctx() } {
        Some(c) => c.map_buffer(target, access),
        None => std::ptr::null_mut(),
    }
}
pub unsafe fn MapBufferRange(target: GLenum, offset: GLintptr, length: GLsizeiptr, access: GLbitfield) -> *mut c_void {
    match unsafe { ctx() } {
        Some(c) => c.map_buffer_range(target, offset, length, access),
        None => std::ptr::null_mut(),
    }
}
pub unsafe fn UnmapBuffer(target: GLenum) -> GLboolean {
    with_ctx!(c => c.unmap_buffer(target))
}
pub unsafe fn TexStorage2D(target: GLenum, levels: GLint, internal_format: GLenum, width: GLsizei, height: GLsizei) {
    with_ctx!(c => c.tex_storage_2d(target, levels, internal_format, width, height))
}
pub unsafe fn FramebufferTexture2D(target: GLenum, attachment: GLenum, textarget: GLenum, texture: GLuint, level: GLint) {
    with_ctx!(c => c.framebuffer_texture_2d(target, attachment, textarget, texture, level))
}
pub unsafe fn CheckFramebufferStatus(target: GLenum) -> GLenum {
    with_ctx!(c => c.check_framebuffer_status(target))
}
pub unsafe fn InvalidateFramebuffer(target: GLenum, num_attachments: GLsizei, attachments: *const GLenum) {
    let a: &[GLenum] = if attachments.is_null() || num_attachments <= 0 {
        &[]
    } else {
        // SAFETY: the caller passes `num_attachments` enums.
        unsafe { std::slice::from_raw_parts(attachments, num_attachments as usize) }
    };
    with_ctx!(c => c.invalidate_framebuffer(target, a))
}
pub unsafe fn TexImage2D(
    target: GLenum,
    level: GLint,
    internal_format: GLint,
    width: GLsizei,
    height: GLsizei,
    border: GLint,
    format: GLenum,
    ty: GLenum,
    data: *const c_void,
) {
    with_ctx!(c => unsafe { c.tex_image_2d(target, level, internal_format, width, height, border, format, ty, data) })
}
pub unsafe fn TexSubImage2D(
    target: GLenum,
    level: GLint,
    xoffset: GLint,
    yoffset: GLint,
    width: GLsizei,
    height: GLsizei,
    format: GLenum,
    ty: GLenum,
    data: *const c_void,
) {
    with_ctx!(c => unsafe { c.tex_sub_image_2d(target, level, xoffset, yoffset, width, height, format, ty, data) })
}
pub unsafe fn GenerateMipmap(target: GLenum) {
    with_ctx!(c => c.generate_mipmap(target))
}
pub unsafe fn GetUniformLocation(program: GLuint, name_: *const GLchar) -> GLint {
    with_ctx!(c => c.get_uniform_location(program, &unsafe { name(name_) }))
}
pub unsafe fn BindAttribLocation(program: GLuint, index: GLuint, name_: *const GLchar) {
    with_ctx!(c => c.bind_attrib_location(program, index, &unsafe { name(name_) }))
}
pub unsafe fn GetAttribLocation(program: GLuint, name_: *const GLchar) -> GLint {
    with_ctx!(c => c.get_attrib_location(program, &unsafe { name(name_) }))
}
pub unsafe fn GenVertexArrays(n: i32, result: *mut GLuint) {
    with_ctx!(c => c.gen_vertex_arrays(unsafe { ids(n, result) }))
}
pub unsafe fn VertexAttribPointer(
    index: GLuint,
    size: GLint,
    type_: GLenum,
    normalized: GLboolean,
    stride: GLsizei,
    offset: *const GLvoid,
) {
    with_ctx!(c => c.vertex_attrib_pointer(index, size, type_, normalized != 0, stride, offset as usize))
}
pub unsafe fn VertexAttribIPointer(index: GLuint, size: GLint, type_: GLenum, stride: GLsizei, offset: *const GLvoid) {
    with_ctx!(c => c.vertex_attrib_i_pointer(index, size, type_, stride, offset as usize))
}
pub unsafe fn CreateShader(shader_type: GLenum) -> GLuint {
    with_ctx!(c => c.create_shader(shader_type))
}
pub unsafe fn AttachShader(program: GLuint, shader: GLuint) {
    with_ctx!(c => c.attach_shader(program, shader))
}
pub unsafe fn CreateProgram() -> GLuint {
    with_ctx!(c => c.create_program())
}
pub unsafe fn Uniform1i(location: GLint, v0: GLint) {
    with_ctx!(c => c.uniform_1i(location, v0))
}
pub unsafe fn Uniform4fv(location: GLint, count: GLsizei, value: *const GLfloat) {
    if value.is_null() {
        return;
    }
    // SAFETY: the caller passes `count` vec4s.
    let v = unsafe { std::slice::from_raw_parts(value, count.max(1) as usize * 4) };
    with_ctx!(c => c.uniform_4fv(location, count, v))
}
pub unsafe fn UniformMatrix4fv(location: GLint, count: GLsizei, transpose: GLboolean, value: *const GLfloat) {
    if value.is_null() {
        return;
    }
    // SAFETY: the caller passes `count` 4x4 matrices.
    let v = unsafe { std::slice::from_raw_parts(value, count.max(1) as usize * 16) };
    with_ctx!(c => c.uniform_matrix4fv(location, count, transpose, v))
}
pub unsafe fn DrawElementsInstanced(mode: GLenum, count: GLsizei, type_: GLenum, indices: GLintptr, instancecount: GLsizei) {
    with_ctx!(c => c.draw_elements_instanced(mode, count, type_, indices, instancecount))
}
pub unsafe fn EnableVertexAttribArray(index: GLuint) {
    with_ctx!(c => c.enable_vertex_attrib_array(index))
}
pub unsafe fn VertexAttribDivisor(index: GLuint, divisor: GLuint) {
    with_ctx!(c => c.vertex_attrib_divisor(index, divisor))
}
pub unsafe fn LinkProgram(program: GLuint) {
    with_ctx!(c => c.link_program(program))
}
pub unsafe fn GetLinkStatus(program: GLuint) -> GLint {
    with_ctx!(c => c.get_link_status(program))
}
pub unsafe fn UseProgram(program: GLuint) {
    with_ctx!(c => c.use_program(program))
}
pub unsafe fn SetViewport(x: GLint, y: GLint, width: GLsizei, height: GLsizei) {
    with_ctx!(c => c.set_viewport(x, y, width, height))
}
pub unsafe fn FramebufferRenderbuffer(target: GLenum, attachment: GLenum, renderbuffertarget: GLenum, renderbuffer: GLuint) {
    with_ctx!(c => c.framebuffer_renderbuffer(target, attachment, renderbuffertarget, renderbuffer))
}
pub unsafe fn RenderbufferStorage(target: GLenum, internalformat: GLenum, width: GLsizei, height: GLsizei) {
    with_ctx!(c => c.renderbuffer_storage(target, internalformat, width, height))
}
pub unsafe fn DepthMask(flag: GLboolean) {
    with_ctx!(c => c.depth_mask(flag))
}
pub unsafe fn DepthFunc(func: GLenum) {
    with_ctx!(c => c.depth_func(func))
}
pub unsafe fn SetScissor(x: GLint, y: GLint, width: GLsizei, height: GLsizei) {
    with_ctx!(c => c.set_scissor(x, y, width, height))
}
pub unsafe fn ClearColor(r: GLfloat, g: GLfloat, b: GLfloat, a: GLfloat) {
    with_ctx!(c => c.clear_color(r, g, b, a))
}
pub unsafe fn ClearDepth(depth: GLdouble) {
    with_ctx!(c => c.clear_depth(depth))
}
pub unsafe fn Clear(mask: GLbitfield) {
    with_ctx!(c => c.clear(mask))
}
pub unsafe fn ClearTexSubImage(
    target: GLenum,
    level: GLint,
    xoffset: GLint,
    yoffset: GLint,
    zoffset: GLint,
    width: GLsizei,
    height: GLsizei,
    depth: GLsizei,
    format: GLenum,
    ty: GLenum,
    data: *const c_void,
) {
    // SAFETY: `data` is one clear value of `format`/`ty`, as gl.cc reads it.
    let d = unsafe { ClearData::read(format, ty, data) };
    with_ctx!(c => c.clear_tex_sub_image(target, level, xoffset, yoffset, zoffset, width, height, depth, format, d))
}
pub unsafe fn ClearTexImage(target: GLenum, level: GLint, format: GLenum, ty: GLenum, data: *const c_void) {
    // SAFETY: as in `ClearTexSubImage`.
    let d = unsafe { ClearData::read(format, ty, data) };
    with_ctx!(c => c.clear_tex_image(target, level, format, d))
}
pub unsafe fn ClearColorRect(
    fbo: GLuint,
    xoffset: GLint,
    yoffset: GLint,
    width: GLsizei,
    height: GLsizei,
    r: GLfloat,
    g: GLfloat,
    b: GLfloat,
    a: GLfloat,
) {
    with_ctx!(c => c.clear_color_rect(fbo, xoffset, yoffset, width, height, r, g, b, a))
}
pub unsafe fn PixelStorei(name: GLenum, param: GLint) {
    with_ctx!(c => c.pixel_storei(name, param))
}
pub unsafe fn ReadPixels(x: GLint, y: GLint, width: GLsizei, height: GLsizei, format: GLenum, ty: GLenum, data: *mut c_void) {
    with_ctx!(c => unsafe { c.read_pixels(x, y, width, height, format, ty, data) })
}
pub unsafe fn Finish() {
    with_ctx!(c => c.finish())
}
pub unsafe fn ShaderSourceByName(shader: GLuint, name_: *const GLchar) {
    with_ctx!(c => c.shader_source_by_name(shader, &unsafe { name(name_) }))
}
pub unsafe fn TexParameteri(target: GLenum, pname: GLenum, param: GLint) {
    with_ctx!(c => c.tex_parameteri(target, pname, param))
}
pub unsafe fn CopyImageSubData(
    src_name: GLuint,
    src_target: GLenum,
    src_level: GLint,
    src_x: GLint,
    src_y: GLint,
    src_z: GLint,
    dst_name: GLuint,
    dst_target: GLenum,
    dst_level: GLint,
    dst_x: GLint,
    dst_y: GLint,
    dst_z: GLint,
    src_width: GLsizei,
    src_height: GLsizei,
    src_depth: GLsizei,
) {
    with_ctx!(c => c.copy_image_sub_data(
        src_name, src_target, src_level, src_x, src_y, src_z, dst_name, dst_target, dst_level, dst_x, dst_y, dst_z,
        src_width, src_height, src_depth,
    ))
}
pub unsafe fn CopyTexSubImage2D(
    target: GLenum,
    level: GLint,
    xoffset: GLint,
    yoffset: GLint,
    x: GLint,
    y: GLint,
    width: GLsizei,
    height: GLsizei,
) {
    with_ctx!(c => c.copy_tex_sub_image_2d(target, level, xoffset, yoffset, x, y, width, height))
}
pub unsafe fn BlitFramebuffer(
    src_x0: GLint,
    src_y0: GLint,
    src_x1: GLint,
    src_y1: GLint,
    dst_x0: GLint,
    dst_y0: GLint,
    dst_x1: GLint,
    dst_y1: GLint,
    mask: GLbitfield,
    filter: GLenum,
) {
    with_ctx!(c => c.blit_framebuffer(src_x0, src_y0, src_x1, src_y1, dst_x0, dst_y0, dst_x1, dst_y1, mask, filter))
}
pub unsafe fn GetIntegerv(pname: GLenum, params: *mut GLint) {
    with_ctx!(c => if let Some(v) = c.get_integerv(pname) {
        if !params.is_null() {
            unsafe { *params = v };
        }
    })
}
pub unsafe fn GetBooleanv(pname: GLenum, params: *mut GLboolean) {
    with_ctx!(c => if let Some(v) = c.get_booleanv(pname) {
        if !params.is_null() {
            unsafe { *params = v };
        }
    })
}
pub unsafe fn GetString(name: GLenum) -> *const c_char {
    Context::get_string(name).map_or(std::ptr::null(), |s| s.as_ptr())
}
pub unsafe fn GetStringi(name: GLenum, index: GLuint) -> *const c_char {
    Context::get_stringi(name, index).map_or(std::ptr::null(), |s| s.as_ptr())
}
pub unsafe fn GetError() -> GLenum {
    with_ctx!(c => c.get_error())
}
pub unsafe fn InitDefaultFramebuffer(x: i32, y: i32, width: i32, height: i32, stride: i32, buf: *mut c_void) {
    with_ctx!(c => unsafe { c.init_default_framebuffer(x, y, width, height, stride, buf) })
}
pub unsafe fn GetColorBuffer(fbo: GLuint, flush: GLboolean, width: *mut i32, height: *mut i32, stride: *mut i32) -> *mut c_void {
    let Some(c) = (unsafe { ctx() }) else { return std::ptr::null_mut() };
    let Some((p, w, h, s)) = c.get_color_buffer(fbo, flush != 0) else { return std::ptr::null_mut() };
    // SAFETY: each out-pointer is null or writable, as gl.cc assumes.
    unsafe {
        if !width.is_null() {
            *width = w;
        }
        if !height.is_null() {
            *height = h;
        }
        if !stride.is_null() {
            *stride = s;
        }
    }
    p as *mut c_void
}
pub unsafe fn ResolveFramebuffer(fbo: GLuint) {
    with_ctx!(c => c.resolve_framebuffer(fbo))
}
pub unsafe fn SetTextureBuffer(
    tex: GLuint,
    internal_format: GLenum,
    width: GLsizei,
    height: GLsizei,
    stride: GLsizei,
    buf: *mut c_void,
    min_width: GLsizei,
    min_height: GLsizei,
) {
    with_ctx!(c => unsafe { c.set_texture_buffer(tex, internal_format, width, height, stride, buf, min_width, min_height) })
}
pub unsafe fn SetTextureParameter(tex: GLuint, pname: GLenum, param: GLint) {
    with_ctx!(c => c.set_texture_parameter(tex, pname, param))
}
pub unsafe fn DeleteTexture(n: GLuint) {
    with_ctx!(c => c.delete_texture(n))
}
pub unsafe fn DeleteRenderbuffer(n: GLuint) {
    with_ctx!(c => c.delete_renderbuffer(n))
}
pub unsafe fn DeleteFramebuffer(n: GLuint) {
    with_ctx!(c => c.delete_framebuffer(n))
}
pub unsafe fn DeleteBuffer(n: GLuint) {
    with_ctx!(c => c.delete_buffer(n))
}
pub unsafe fn DeleteVertexArray(n: GLuint) {
    with_ctx!(c => c.delete_vertex_array(n))
}
pub unsafe fn DeleteQuery(n: GLuint) {
    with_ctx!(c => c.delete_query(n))
}
pub unsafe fn DeleteShader(shader: GLuint) {
    with_ctx!(c => c.delete_shader(shader))
}
pub unsafe fn DeleteProgram(program: GLuint) {
    with_ctx!(c => c.delete_program(program))
}
pub unsafe fn LockFramebuffer(fbo: GLuint) -> *mut LockedTexture {
    match unsafe { ctx() } {
        Some(c) => c.lock_framebuffer(fbo) as *mut LockedTexture,
        None => std::ptr::null_mut(),
    }
}
pub unsafe fn LockTexture(tex: GLuint) -> *mut LockedTexture {
    match unsafe { ctx() } {
        Some(c) => c.lock_texture(tex) as *mut LockedTexture,
        None => std::ptr::null_mut(),
    }
}
pub unsafe fn LockResource(resource: *mut LockedTexture) {
    // SAFETY: a handle `LockTexture`/`LockFramebuffer` returned, still locked.
    unsafe { rgl::lock_resource(tex(resource)) }
}
pub unsafe fn UnlockResource(resource: *mut LockedTexture) -> i32 {
    // SAFETY: as in `LockResource`.
    unsafe { rgl::unlock_resource(tex(resource)) }
}
pub unsafe fn GetResourceBuffer(resource: *mut LockedTexture, width: *mut i32, height: *mut i32, stride: *mut i32) -> *mut c_void {
    // SAFETY: a locked handle and writable out-pointers, as composite.h assumes.
    unsafe {
        let (p, w, h, s) = rgl::get_resource_buffer(tex(resource));
        *width = w;
        *height = h;
        *stride = s;
        p as *mut c_void
    }
}
pub unsafe fn Composite(
    locked_dst: *mut LockedTexture,
    locked_src: *mut LockedTexture,
    src_x: GLint,
    src_y: GLint,
    src_width: GLsizei,
    src_height: GLsizei,
    dst_x: GLint,
    dst_y: GLint,
    dst_width: GLsizei,
    dst_height: GLsizei,
    opaque: GLboolean,
    flip_x: GLboolean,
    flip_y: GLboolean,
    filter: GLenum,
    clip_x: GLint,
    clip_y: GLint,
    clip_width: GLsizei,
    clip_height: GLsizei,
) {
    // SAFETY: locked handles (SwCompositor's contract).
    unsafe {
        rgl::composite(
            tex(locked_dst), tex(locked_src), src_x, src_y, src_width, src_height, dst_x, dst_y, dst_width, dst_height,
            opaque != 0, flip_x != 0, flip_y != 0, filter, clip_x, clip_y, clip_width, clip_height,
        )
    }
}
pub unsafe fn CompositeYUV(
    locked_dst: *mut LockedTexture,
    locked_y: *mut LockedTexture,
    locked_u: *mut LockedTexture,
    locked_v: *mut LockedTexture,
    color_space: YuvRangedColorSpace,
    color_depth: GLuint,
    src_x: GLint,
    src_y: GLint,
    src_width: GLsizei,
    src_height: GLsizei,
    dst_x: GLint,
    dst_y: GLint,
    dst_width: GLsizei,
    dst_height: GLsizei,
    flip_x: GLboolean,
    flip_y: GLboolean,
    clip_x: GLint,
    clip_y: GLint,
    clip_width: GLsizei,
    clip_height: GLsizei,
) {
    // SAFETY: locked handles (SwCompositor's contract).
    unsafe {
        rgl::composite_yuv(
            tex(locked_dst), tex(locked_y), tex(locked_u), tex(locked_v), color_space as u8, color_depth, src_x, src_y,
            src_width, src_height, dst_x, dst_y, dst_width, dst_height, flip_x != 0, flip_y != 0, clip_x, clip_y,
            clip_width, clip_height,
        )
    }
}
pub unsafe fn ApplyMask(locked_dst: *mut LockedTexture, locked_mask: *mut LockedTexture) {
    // SAFETY: locked handles.
    unsafe { rgl::apply_mask(tex(locked_dst), tex(locked_mask)) }
}
pub unsafe fn CreateContext() -> *mut c_void {
    rgl::create_context() as *mut c_void
}
pub unsafe fn ReferenceContext(c: *mut c_void) {
    // SAFETY: a context `CreateContext` made, or null.
    unsafe { rgl::reference_context(c as *mut Context) }
}
pub unsafe fn DestroyContext(c: *mut c_void) {
    let c = c as *mut Context;
    let current = CTX.load(Ordering::Relaxed);
    // `DestroyContext` makes nothing current before it frees the current context.
    // SAFETY: a context `CreateContext` made, or null.
    if unsafe { rgl::destroy_context(c) } && current == c {
        CTX.store(std::ptr::null_mut(), Ordering::Relaxed);
    }
}
pub unsafe fn MakeCurrent(c: *mut c_void) {
    let c = c as *mut Context;
    if CTX.load(Ordering::Relaxed) == c {
        return;
    }
    CTX.store(c, Ordering::Relaxed);
    // SAFETY: a live context or null.
    if let Some(c) = unsafe { c.as_mut() } {
        c.make_current();
    }
}
pub unsafe fn ReportMemory(c: *mut c_void, size_of_op: unsafe extern "C" fn(ptr: *const c_void) -> usize) -> usize {
    // SAFETY: a live context or null; `size_of_op` measures heap blocks, which the texture buffers are.
    match unsafe { (c as *mut Context).as_ref() } {
        Some(c) => c.report_memory(&mut |p| unsafe { size_of_op(p) }),
        None => 0,
    }
}
pub unsafe fn GetTextureBuffer(
    target: GLenum,
    name: GLuint,
    bound: GLboolean,
    width: *mut i32,
    height: *mut i32,
    stride: *mut i32,
    bpp: *mut i32,
) -> *mut c_void {
    let Some(c) = (unsafe { ctx() }) else { return std::ptr::null_mut() };
    let Some((p, w, h, s, b)) = c.get_texture_buffer(target, name, bound != 0) else { return std::ptr::null_mut() };
    // SAFETY: writable out-pointers.
    unsafe {
        *width = w;
        *height = h;
        *stride = s;
        *bpp = b;
    }
    p as *mut c_void
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_decides_the_backend() {
        assert_eq!(backend_from_env(None), (Backend::Rust, None));
        assert_eq!(backend_from_env(Some("")), (Backend::Rust, None));
        assert_eq!(backend_from_env(Some("rust")), (Backend::Rust, None));
        assert_eq!(backend_from_env(Some("swgl")), (Backend::Cxx, None));
        let (b, w) = backend_from_env(Some("bogus"));
        assert_eq!(b, Backend::Rust);
        assert_eq!(w.as_deref(), Some("swgl: LUCINANET_RASTER=bogus is not rust or swgl; using rust"));
    }

    #[test]
    fn the_start_up_line_names_the_rasterizer() {
        assert_eq!(describe(Backend::Rust, false), "lucinanet: rasterizer rust");
        assert_eq!(describe(Backend::Cxx, true), "lucinanet: rasterizer swgl (LUCINANET_RASTER=swgl)");
        assert_eq!(describe(Backend::Rust, true), "lucinanet: rasterizer rust (LUCINANET_RASTER=rust)");
    }
}
