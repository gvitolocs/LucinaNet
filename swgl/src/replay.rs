/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Replays a `LUCINANET_GLTRACE` recording (`trace.rs`, `TRACE.md`) through the same dispatch functions the
//! live `swgl::Context` calls, on one thread. `raster/gltrace` drives it.
//!
//! Recorded handles (contexts, locked resources) are mapped to the replay's own; buffers the caller owned
//! (`InitDefaultFramebuffer`, `SetTextureBuffer`) are zeroed memory the replayer owns.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::os::raw::c_void;
use std::ptr;

use gleam::gl::*;

use crate::swgl_fns::{self as f, LockedTexture};
use crate::trace::{Cursor, Op, DATA_INLINE, DATA_NULL, DATA_OFFSET};
use crate::YuvRangedColorSpace;

/// Handle maps and memory the replay owns.
#[derive(Default)]
pub struct Maps {
    ctxs: HashMap<u64, *mut c_void>,
    /// References held on each replay context, so `finish` can destroy the ones the trace left alive.
    refs: HashMap<usize, u32>,
    /// The recorded context current now.
    current: u64,
    locked: HashMap<u64, *mut LockedTexture>,
    /// Per target, what the replay's own `MapBuffer`/`MapBufferRange` returned.
    mapped: HashMap<GLenum, *mut c_void>,
    /// `(recorded context, None)` for the default framebuffer, `(context, Some(tex))` for `SetTextureBuffer`.
    buffers: HashMap<(u64, Option<GLuint>), Vec<u8>>,
    warned: HashSet<Op>,
    warnings: u64,
}

impl Maps {
    fn warn(&mut self, op: Op, msg: String) {
        self.warnings += 1;
        if self.warned.insert(op) {
            eprintln!("gltrace: {}: {msg} (said once per op)", op.name());
        }
    }
}

/// Decodes one argument as `trace::Enc` wrote it.
pub(crate) trait Dec: Sized {
    fn dec(c: &mut Cursor, m: &mut Maps) -> Result<Self, String>;
}

macro_rules! dec_prim {
    ($($t:ty => $m:ident),*) => {
        $(impl Dec for $t {
            fn dec(c: &mut Cursor, _: &mut Maps) -> Result<Self, String> {
                c.$m()
            }
        })*
    };
}
dec_prim!(u8 => u8, u32 => u32, i32 => i32, u64 => u64, f32 => f32, f64 => f64);

impl Dec for isize {
    fn dec(c: &mut Cursor, _: &mut Maps) -> Result<Self, String> {
        Ok(c.i64()? as isize)
    }
}

/// `*const GLvoid` arguments are offsets (vertex attributes), passed through as they were.
impl Dec for *const c_void {
    fn dec(c: &mut Cursor, _: &mut Maps) -> Result<Self, String> {
        Ok(c.u64()? as usize as *const c_void)
    }
}

/// `*mut c_void` arguments are contexts.
impl Dec for *mut c_void {
    fn dec(c: &mut Cursor, m: &mut Maps) -> Result<Self, String> {
        match c.u64()? {
            0 => Ok(ptr::null_mut()),
            v => m.ctxs.get(&v).copied().ok_or_else(|| format!("context {v:#x} was never created")),
        }
    }
}

impl Dec for *mut LockedTexture {
    fn dec(c: &mut Cursor, m: &mut Maps) -> Result<Self, String> {
        match c.u64()? {
            0 => Ok(ptr::null_mut()),
            v => m.locked.get(&v).copied().ok_or_else(|| format!("resource {v:#x} was never locked")),
        }
    }
}

impl Dec for YuvRangedColorSpace {
    fn dec(c: &mut Cursor, _: &mut Maps) -> Result<Self, String> {
        match c.u8()? {
            v @ 0..=6 => Ok(unsafe { std::mem::transmute::<u8, YuvRangedColorSpace>(v) }),
            v => Err(format!("bad YuvRangedColorSpace {v}")),
        }
    }
}

/// A recorded result, and what to do when the replay's differs.
pub(crate) trait Ret: Sized {
    fn read(c: &mut Cursor) -> Result<Self, String>;
    fn check(want: Self, got: Self, op: Op, m: &mut Maps);
}

impl Ret for () {
    fn read(_: &mut Cursor) -> Result<Self, String> {
        Ok(())
    }
    fn check(_: (), _: (), _: Op, _: &mut Maps) {}
}

macro_rules! ret_eq {
    ($($t:ty => $m:ident),*) => {
        $(impl Ret for $t {
            fn read(c: &mut Cursor) -> Result<Self, String> {
                c.$m()
            }
            fn check(want: Self, got: Self, op: Op, m: &mut Maps) {
                if want != got {
                    m.warn(op, format!("recorded {want}, replay got {got}"));
                }
            }
        })*
    };
}
ret_eq!(u32 => u32, i32 => i32);

/// `LockFramebuffer`/`LockTexture`: remember which replay resource stands for the recorded one.
impl Ret for *mut LockedTexture {
    fn read(c: &mut Cursor) -> Result<Self, String> {
        Ok(c.u64()? as usize as *mut LockedTexture)
    }
    fn check(want: Self, got: Self, op: Op, m: &mut Maps) {
        if want.is_null() != got.is_null() {
            m.warn(op, format!("recorded {want:?}, replay got {got:?}"));
        }
        if !want.is_null() {
            m.locked.insert(want as u64, got);
        }
    }
}

/// What a call drew into, for `gltrace --per-draw`.
#[derive(Clone, Copy, Debug)]
pub enum Target {
    Framebuffer(GLuint),
    /// A locked resource, by its recorded handle (`Composite`'s destination).
    Resource(u64),
    /// The rectangle `rect` (x, y, width, height) of a texture an upload or copy wrote: the one bound to `target`
    /// on the active unit if `bound`, else texture (or, for `GL_RENDERBUFFER`, renderbuffer) `name`.
    Texture { target: GLenum, name: GLuint, bound: bool, rect: (i32, i32, i32, i32) },
}

/// A color buffer as swgl holds it.
pub struct Pixels<'a> {
    pub data: &'a [u8],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    /// The bytes of each row that hold pixels, when known (a texture rectangle); otherwise readers infer it from
    /// the width and stride.
    pub row_bytes: Option<usize>,
}

pub struct Replayer {
    m: Maps,
}

impl Default for Replayer {
    fn default() -> Self {
        Self::new()
    }
}

impl Replayer {
    pub fn new() -> Self {
        Replayer { m: Maps::default() }
    }

    /// Results that differed from the recording (each op is reported on stderr once).
    pub fn warnings(&self) -> u64 {
        self.m.warnings
    }

    /// Replays one record. An `Err` means the record could not be decoded or refers to a handle the replay
    /// never saw; the call was not made.
    pub fn step(&mut self, op: Op, payload: &[u8]) -> Result<(), String> {
        let mut c = Cursor::new(payload);
        let m = &mut self.m;
        unsafe {
            if f::replay_scalar(op, &mut c, m)? {
                match op {
                    Op::MakeCurrent => m.current = Cursor::new(payload).u64()?,
                    Op::ReferenceContext => {
                        let ctx = <*mut c_void>::dec(&mut Cursor::new(payload), m)?;
                        *m.refs.entry(ctx as usize).or_default() += 1;
                    }
                    Op::DestroyContext => {
                        let ctx = <*mut c_void>::dec(&mut Cursor::new(payload), m)?;
                        if let Some(n) = m.refs.get_mut(&(ctx as usize)) {
                            *n -= 1;
                            if *n == 0 {
                                m.refs.remove(&(ctx as usize));
                            }
                        }
                    }
                    _ => {}
                }
                return Ok(());
            }
            replay_other(op, &mut c, m)
        }
    }

    // The observers below call `cxx` directly: they are not part of the replay, and must not change it.

    /// The color buffer of framebuffer `fbo` as it is in memory. A delayed clear swgl has not applied yet is
    /// not in it: applying it here (`flush`) would change when swgl applies it, and so what later calls draw
    /// (gl.cc's `force_clear` with a skip rectangle does not clear every row the same way a full one does).
    /// `SoftContext::with_frame` reads the default framebuffer the same way.
    pub fn color_buffer(&self, fbo: GLuint) -> Option<Pixels<'_>> {
        let (mut w, mut h, mut s) = (0, 0, 0);
        let p = unsafe { f::cxx::GetColorBuffer(fbo, 0, &mut w, &mut h, &mut s) };
        pixels(p, w, h, s)
    }

    /// A locked resource's buffer, by its recorded handle.
    pub fn resource_buffer(&self, recorded: u64) -> Option<Pixels<'_>> {
        let r = *self.m.locked.get(&recorded)?;
        let (mut w, mut h, mut s) = (0, 0, 0);
        let p = unsafe { f::cxx::GetResourceBuffer(r, &mut w, &mut h, &mut s) };
        pixels(p, w, h, s)
    }

    pub fn draw_framebuffer(&self) -> GLuint {
        let mut v = [0; 4];
        unsafe { f::cxx::GetIntegerv(DRAW_FRAMEBUFFER_BINDING, v.as_mut_ptr()) };
        v[0] as GLuint
    }

    /// What `op` (just replayed from `payload`) drew into, if it is one of the calls `--per-draw` follows.
    pub fn written_target(&self, op: Op, payload: &[u8]) -> Option<Target> {
        let mut c = Cursor::new(payload);
        match op {
            Op::DrawElementsInstanced | Op::Clear | Op::BlitFramebuffer => {
                Some(Target::Framebuffer(self.draw_framebuffer()))
            }
            Op::ClearColorRect => c.u32().ok().map(Target::Framebuffer),
            Op::Composite => c.u64().ok().map(Target::Resource),
            Op::TexSubImage2D => {
                let (target, _level) = (c.u32().ok()?, c.i32().ok()?);
                let rect = (c.i32().ok()?, c.i32().ok()?, c.i32().ok()?, c.i32().ok()?);
                Some(Target::Texture { target, name: 0, bound: true, rect })
            }
            Op::TexImage2D => {
                let (target, _level, _internal_format) = (c.u32().ok()?, c.i32().ok()?, c.i32().ok()?);
                let rect = (0, 0, c.i32().ok()?, c.i32().ok()?);
                let (_border, _format, _ty) = (c.i32().ok()?, c.u32().ok()?, c.u32().ok()?);
                // Storage only (no pixels): nothing is written, the memory is whatever the allocator gave.
                (c.u8().ok()? != DATA_NULL).then_some(Target::Texture { target, name: 0, bound: true, rect })
            }
            Op::CopyTexSubImage2D => {
                let (target, _level, x, y) = (c.u32().ok()?, c.i32().ok()?, c.i32().ok()?, c.i32().ok()?);
                let (_sx, _sy, w, h) = (c.i32().ok()?, c.i32().ok()?, c.i32().ok()?, c.i32().ok()?);
                Some(Target::Texture { target, name: 0, bound: true, rect: (x, y, w, h) })
            }
            Op::CopyImageSubData => {
                for _ in 0..6 {
                    c.u32().ok()?;
                }
                let (name, target, _level) = (c.u32().ok()?, c.u32().ok()?, c.i32().ok()?);
                let (x, y, _z) = (c.i32().ok()?, c.i32().ok()?, c.i32().ok()?);
                let (w, h) = (c.i32().ok()?, c.i32().ok()?);
                Some(Target::Texture { target, name, bound: false, rect: (x, y, w, h) })
            }
            _ => None,
        }
    }

    /// The pixels of `rect` in a texture, as they are in memory (a delayed clear not applied, as in
    /// [`Replayer::color_buffer`]), clipped to the texture.
    pub fn texture_pixels(&self, target: GLenum, name: GLuint, bound: bool, rect: (i32, i32, i32, i32)) -> Option<Pixels<'_>> {
        let (mut w, mut h, mut s, mut bpp) = (0, 0, 0, 0);
        let p = unsafe { f::cxx::GetTextureBuffer(target, name, bound as GLboolean, &mut w, &mut h, &mut s, &mut bpp) };
        if p.is_null() || w <= 0 || h <= 0 || s <= 0 || bpp <= 0 {
            return None;
        }
        let x0 = rect.0.clamp(0, w);
        let y0 = rect.1.clamp(0, h);
        let x1 = rect.0.saturating_add(rect.2).clamp(x0, w);
        let y1 = rect.1.saturating_add(rect.3).clamp(y0, h);
        let (rw, rh, stride, bpp) = ((x1 - x0) as usize, (y1 - y0) as usize, s as usize, bpp as usize);
        if rw == 0 || rh == 0 {
            return None;
        }
        let start = y0 as usize * stride + x0 as usize * bpp;
        let len = (rh - 1) * stride + rw * bpp;
        let data = unsafe { std::slice::from_raw_parts((p as *const u8).add(start), len) };
        Some(Pixels { data, width: rw, height: rh, stride, row_bytes: Some(rw * bpp) })
    }

    pub fn target_pixels(&self, t: Target) -> Option<Pixels<'_>> {
        match t {
            Target::Framebuffer(fb) => self.color_buffer(fb),
            Target::Resource(r) => self.resource_buffer(r),
            Target::Texture { target, name, bound, rect } => self.texture_pixels(target, name, bound, rect),
        }
    }

    /// Destroys the contexts the trace left alive, then frees the memory the replay owned for them.
    pub fn finish(&mut self) {
        for (ctx, n) in std::mem::take(&mut self.m.refs) {
            for _ in 0..n {
                unsafe { f::DestroyContext(ctx as *mut c_void) };
            }
        }
        self.m = Maps::default();
    }
}

impl Drop for Replayer {
    fn drop(&mut self) {
        self.finish();
    }
}

fn pixels<'a>(p: *mut c_void, w: i32, h: i32, s: i32) -> Option<Pixels<'a>> {
    if p.is_null() || w <= 0 || h <= 0 || s <= 0 {
        return None;
    }
    let (width, height, stride) = (w as usize, h as usize, s as usize);
    let data = unsafe { std::slice::from_raw_parts(p as *const u8, stride * height) };
    Some(Pixels { data, width, height, stride, row_bytes: None })
}

/// A data pointer as `put_data` wrote it: the payload's own bytes, an offset, or null.
fn data_ptr(c: &mut Cursor) -> Result<*const c_void, String> {
    match c.u8()? {
        DATA_NULL => Ok(ptr::null()),
        DATA_INLINE => Ok(c.bytes()?.as_ptr() as *const c_void),
        DATA_OFFSET => Ok(c.u64()? as usize as *const c_void),
        v => Err(format!("bad data flag {v}")),
    }
}

/// `put_pixels`: inline rows were packed; lay them out again at the row length the call saw. `keep` holds the
/// memory while the call runs.
fn pixels_ptr(c: &mut Cursor, width: GLsizei, height: GLsizei, keep: &mut Vec<u8>) -> Result<*const c_void, String> {
    match c.u8()? {
        DATA_NULL => Ok(ptr::null()),
        DATA_OFFSET => Ok(c.u64()? as usize as *const c_void),
        DATA_INLINE => {
            let row_length = c.i32()?;
            let bpp = c.u32()? as usize;
            let packed = c.bytes()?;
            let (w, h) = (width.max(0) as usize, height.max(0) as usize);
            if packed.len() != w * h * bpp {
                return Err(format!("{} pixel bytes for {w}x{h} at {bpp} bytes", packed.len()));
            }
            if row_length == 0 || row_length as usize == w || h <= 1 {
                return Ok(packed.as_ptr() as *const c_void);
            }
            let pitch = row_length as usize * bpp;
            keep.clear();
            keep.resize((h - 1) * pitch + w * bpp, 0);
            for (row, src) in packed.chunks_exact(w * bpp).enumerate() {
                keep[row * pitch..row * pitch + w * bpp].copy_from_slice(src);
            }
            Ok(keep.as_ptr() as *const c_void)
        }
        v => Err(format!("bad data flag {v}")),
    }
}

fn c_string(c: &mut Cursor) -> Result<Option<CString>, String> {
    match c.u8()? {
        DATA_NULL => Ok(None),
        DATA_INLINE => CString::new(c.bytes()?).map(Some).map_err(|e| e.to_string()),
        v => Err(format!("bad string flag {v}")),
    }
}

fn c_str_ptr(s: &Option<CString>) -> *const GLchar {
    s.as_ref().map_or(ptr::null(), |s| s.as_ptr())
}

/// The recorded out-parameters of `GetColorBuffer`/`GetResourceBuffer`: `None` where the caller passed null.
fn dims(c: &mut Cursor) -> Result<[Option<i32>; 3], String> {
    let mut d = [None; 3];
    for v in &mut d {
        if c.u8()? != 0 {
            *v = Some(c.i32()?);
        }
    }
    Ok(d)
}

fn check_dims(op: Op, want: [Option<i32>; 3], got: [i32; 3], m: &mut Maps) {
    for (w, g) in want.iter().zip(got) {
        if let Some(w) = *w {
            if w != g {
                m.warn(op, format!("recorded size {want:?}, replay got {got:?}"));
                return;
            }
        }
    }
}

/// The ops `scalar_calls!` does not cover, decoded as their dispatch functions in `swgl_fns.rs` encoded them.
unsafe fn replay_other(op: Op, c: &mut Cursor, m: &mut Maps) -> Result<(), String> {
    match op {
        Op::FrameMarker | Op::ReportMemory => {}
        Op::CreateContext => {
            let want = c.u64()?;
            let got = f::CreateContext();
            m.ctxs.insert(want, got);
            m.refs.insert(got as usize, 1);
        }
        Op::GenQueries
        | Op::GenBuffers
        | Op::GenTextures
        | Op::GenFramebuffers
        | Op::GenRenderbuffers
        | Op::GenVertexArrays => {
            let n = c.i32()?;
            let want = (0..n.max(0)).map(|_| c.u32()).collect::<Result<Vec<_>, _>>()?;
            let mut got = vec![0; want.len()];
            let gen = match op {
                Op::GenQueries => f::GenQueries,
                Op::GenBuffers => f::GenBuffers,
                Op::GenTextures => f::GenTextures,
                Op::GenFramebuffers => f::GenFramebuffers,
                Op::GenRenderbuffers => f::GenRenderbuffers,
                _ => f::GenVertexArrays,
            };
            gen(n, got.as_mut_ptr());
            if want != got {
                m.warn(op, format!("recorded ids {want:?}, replay generated {got:?}; later calls use the recorded ones"));
            }
        }
        Op::GetQueryObjectui64v => {
            // Timer queries measure the run, so the value is not compared.
            let (id, pname) = (c.u32()?, c.u32()?);
            let mut v = 0;
            f::GetQueryObjectui64v(id, pname, &mut v);
        }
        Op::BufferData => {
            let (target, size) = (c.u32()?, c.i64()? as GLsizeiptr);
            let data = data_ptr(c)?;
            f::BufferData(target, size, data, c.u32()?);
        }
        Op::BufferSubData => {
            let (target, offset, size) = (c.u32()?, c.i64()? as GLintptr, c.i64()? as GLsizeiptr);
            f::BufferSubData(target, offset, size, data_ptr(c)?);
        }
        Op::MapBuffer => {
            let (target, access, want) = (c.u32()?, c.u32()?, c.u64()?);
            let got = f::MapBuffer(target, access);
            map_result(op, target, want, got, m);
        }
        Op::MapBufferRange => {
            let (target, offset, length) = (c.u32()?, c.i64()? as GLintptr, c.i64()? as GLsizeiptr);
            let (access, want) = (c.u32()?, c.u64()?);
            let got = f::MapBufferRange(target, offset, length, access);
            map_result(op, target, want, got, m);
        }
        Op::UnmapBuffer => {
            let target = c.u32()?;
            if c.u8()? == DATA_INLINE {
                let bytes = c.bytes()?;
                match m.mapped.get(&target) {
                    Some(&p) if !p.is_null() => ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len()),
                    _ => m.warn(op, format!("{} mapped bytes for target {target:#x}, not mapped in the replay", bytes.len())),
                }
            }
            m.mapped.remove(&target);
            let got = f::UnmapBuffer(target);
            let want = c.u8()?;
            if want != got {
                m.warn(op, format!("recorded {want}, replay got {got}"));
            }
        }
        Op::InvalidateFramebuffer => {
            let (target, n) = (c.u32()?, c.i32()?);
            let list = (0..n.max(0)).map(|_| c.u32()).collect::<Result<Vec<_>, _>>()?;
            let p = if list.is_empty() { ptr::null() } else { list.as_ptr() };
            f::InvalidateFramebuffer(target, n, p);
        }
        Op::TexImage2D => {
            let (target, level, internal_format) = (c.u32()?, c.i32()?, c.i32()?);
            let (width, height, border, format, ty) = (c.i32()?, c.i32()?, c.i32()?, c.u32()?, c.u32()?);
            let mut keep = Vec::new();
            let data = pixels_ptr(c, width, height, &mut keep)?;
            f::TexImage2D(target, level, internal_format, width, height, border, format, ty, data);
        }
        Op::TexSubImage2D => {
            let (target, level, xoffset, yoffset) = (c.u32()?, c.i32()?, c.i32()?, c.i32()?);
            let (width, height, format, ty) = (c.i32()?, c.i32()?, c.u32()?, c.u32()?);
            let mut keep = Vec::new();
            let data = pixels_ptr(c, width, height, &mut keep)?;
            f::TexSubImage2D(target, level, xoffset, yoffset, width, height, format, ty, data);
        }
        Op::GetUniformLocation | Op::GetAttribLocation => {
            let program = c.u32()?;
            let name = c_string(c)?;
            let want = c.i32()?;
            let got = if op == Op::GetUniformLocation {
                f::GetUniformLocation(program, c_str_ptr(&name))
            } else {
                f::GetAttribLocation(program, c_str_ptr(&name))
            };
            if want != got {
                m.warn(op, format!("{name:?}: recorded {want}, replay got {got}"));
            }
        }
        Op::BindAttribLocation => {
            let (program, index) = (c.u32()?, c.u32()?);
            let name = c_string(c)?;
            f::BindAttribLocation(program, index, c_str_ptr(&name));
        }
        Op::ShaderSourceByName => {
            let shader = c.u32()?;
            let name = c_string(c)?;
            f::ShaderSourceByName(shader, c_str_ptr(&name));
        }
        Op::Uniform4fv => {
            let (location, count) = (c.i32()?, c.i32()?);
            f::Uniform4fv(location, count, data_ptr(c)? as *const GLfloat);
        }
        Op::UniformMatrix4fv => {
            let (location, count, transpose) = (c.i32()?, c.i32()?, c.u8()?);
            f::UniformMatrix4fv(location, count, transpose, data_ptr(c)? as *const GLfloat);
        }
        Op::ClearTexSubImage => {
            let (target, level, xoffset, yoffset, zoffset) = (c.u32()?, c.i32()?, c.i32()?, c.i32()?, c.i32()?);
            let (width, height, depth, format, ty) = (c.i32()?, c.i32()?, c.i32()?, c.u32()?, c.u32()?);
            let data = data_ptr(c)?;
            f::ClearTexSubImage(target, level, xoffset, yoffset, zoffset, width, height, depth, format, ty, data);
        }
        Op::ClearTexImage => {
            let (target, level, format, ty) = (c.u32()?, c.i32()?, c.u32()?, c.u32()?);
            f::ClearTexImage(target, level, format, ty, data_ptr(c)?);
        }
        Op::ReadPixels => {
            let (x, y, width, height, format, ty) = (c.i32()?, c.i32()?, c.i32()?, c.i32()?, c.u32()?, c.u32()?);
            match c.u8()? {
                DATA_INLINE => {
                    let want = c.bytes()?;
                    let mut got = vec![0u8; want.len()];
                    f::ReadPixels(x, y, width, height, format, ty, got.as_mut_ptr() as *mut c_void);
                    if want != &got[..] {
                        m.warn(op, format!("{width}x{height} at ({x},{y}) read back different pixels"));
                    }
                }
                DATA_OFFSET => f::ReadPixels(x, y, width, height, format, ty, c.u64()? as usize as *mut c_void),
                _ => f::ReadPixels(x, y, width, height, format, ty, ptr::null_mut()),
            }
        }
        Op::GetIntegerv => {
            let (pname, want) = (c.u32()?, c.i32()?);
            let mut got = [0; 4];
            f::GetIntegerv(pname, got.as_mut_ptr());
            if want != got[0] {
                m.warn(op, format!("{pname:#x}: recorded {want}, replay got {}", got[0]));
            }
        }
        Op::GetBooleanv => {
            let (pname, want) = (c.u32()?, c.u8()?);
            let mut got = [0; 4];
            f::GetBooleanv(pname, got.as_mut_ptr());
            if want != got[0] {
                m.warn(op, format!("{pname:#x}: recorded {want}, replay got {}", got[0]));
            }
        }
        Op::GetString | Op::GetStringi => {
            let name = c.u32()?;
            let got = if op == Op::GetString {
                f::GetString(name)
            } else {
                f::GetStringi(name, c.u32()?)
            };
            let want = c_string(c)?;
            let got = (!got.is_null()).then(|| std::ffi::CStr::from_ptr(got).to_owned());
            if want != got {
                m.warn(op, format!("{name:#x}: recorded {want:?}, replay got {got:?}"));
            }
        }
        Op::InitDefaultFramebuffer => {
            let (x, y, width, height, stride) = (c.i32()?, c.i32()?, c.i32()?, c.i32()?, c.i32()?);
            let mut buf = Vec::new();
            if c.u8()? != 0 {
                buf = vec![0u8; stride.unsigned_abs() as usize * height.max(0) as usize];
            }
            let p = if buf.is_empty() { ptr::null_mut() } else { buf.as_mut_ptr() as *mut c_void };
            f::InitDefaultFramebuffer(x, y, width, height, stride, p);
            m.buffers.insert((m.current, None), buf);
        }
        Op::GetColorBuffer => {
            let (fbo, flush, want) = (c.u32()?, c.u8()?, c.u8()?);
            let want_dims = dims(c)?;
            let (mut w, mut h, mut s) = (0, 0, 0);
            let got = f::GetColorBuffer(fbo, flush, &mut w, &mut h, &mut s);
            if (want != 0) == got.is_null() {
                m.warn(op, format!("framebuffer {fbo}: recorded non-null {want}, replay got {got:?}"));
            }
            check_dims(op, want_dims, [w, h, s], m);
        }
        Op::SetTextureBuffer => {
            let (tex, internal_format, width, height, stride) = (c.u32()?, c.u32()?, c.i32()?, c.i32()?, c.i32()?);
            let has_buf = c.u8()? != 0;
            let (min_width, min_height) = (c.i32()?, c.i32()?);
            let mut buf = Vec::new();
            if has_buf {
                buf = vec![0u8; stride.unsigned_abs() as usize * height.max(min_height).max(0) as usize];
            }
            let p = if buf.is_empty() { ptr::null_mut() } else { buf.as_mut_ptr() as *mut c_void };
            f::SetTextureBuffer(tex, internal_format, width, height, stride, p, min_width, min_height);
            m.buffers.insert((m.current, Some(tex)), buf);
        }
        Op::GetResourceBuffer => {
            let res = <*mut LockedTexture>::dec(c, m)?;
            let want = c.u8()?;
            let want_dims = dims(c)?;
            let (mut w, mut h, mut s) = (0, 0, 0);
            let got = f::GetResourceBuffer(res, &mut w, &mut h, &mut s);
            if (want != 0) == got.is_null() {
                m.warn(op, format!("recorded non-null {want}, replay got {got:?}"));
            }
            check_dims(op, want_dims, [w, h, s], m);
        }
        _ => return Err(format!("{} is not a replayable op", op.name())),
    }
    Ok(())
}

fn map_result(op: Op, target: GLenum, want: u64, got: *mut c_void, m: &mut Maps) {
    if (want == 0) != got.is_null() {
        m.warn(op, format!("target {target:#x}: recorded {want:#x}, replay got {got:?}"));
    }
    m.mapped.insert(target, got);
}
