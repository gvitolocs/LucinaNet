/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `LUCINANET_GLTRACE=<dir>`: a recording of every call that crosses swgl's `extern "C"` boundary, in
//! `<dir>/trace-<pid>.bin`. The format and each op's payload are in `TRACE.md`; `raster/gltrace` replays it.
//!
//! Every dispatch function in `swgl_fns.rs` encodes its arguments, makes the call and appends one record
//! with the results. With tracing off that costs one relaxed atomic load.

use std::cell::Cell;
use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};
use std::ffi::CStr;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::raw::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, Once};

use gleam::gl::*;

pub const MAGIC: &[u8; 8] = b"SWGLTRC1";
/// Bytes before each payload: `u16 op, u32 thread, u32 payload length`.
pub const RECORD_HEADER: usize = 10;

/// How a pointer argument that points at data was recorded (`TRACE.md`, "Data pointers").
pub const DATA_NULL: u8 = 0;
pub const DATA_INLINE: u8 = 1;
pub const DATA_OFFSET: u8 = 2;

macro_rules! ops {
    ($($name:ident = $n:literal,)*) => {
        /// One op per `extern "C"` function in `swgl_fns.rs`, plus `FrameMarker`. The numbers are the format:
        /// append new ones, never renumber.
        #[repr(u16)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Op {
            $($name = $n,)*
        }

        impl Op {
            pub const ALL: &'static [Op] = &[$(Op::$name,)*];

            pub fn from_u16(v: u16) -> Option<Op> {
                match v {
                    $($n => Some(Op::$name),)*
                    _ => None,
                }
            }

            pub fn name(self) -> &'static str {
                match self {
                    $(Op::$name => stringify!($name),)*
                }
            }
        }
    };
}

ops! {
    FrameMarker = 0,
    ActiveTexture = 1,
    BindTexture = 2,
    BindBuffer = 3,
    BindVertexArray = 4,
    BindFramebuffer = 5,
    BindRenderbuffer = 6,
    BlendFunc = 7,
    BlendColor = 8,
    BlendEquation = 9,
    Enable = 10,
    Disable = 11,
    GenQueries = 12,
    BeginQuery = 13,
    EndQuery = 14,
    GetQueryObjectui64v = 15,
    GenBuffers = 16,
    GenTextures = 17,
    GenFramebuffers = 18,
    GenRenderbuffers = 19,
    BufferData = 20,
    BufferSubData = 21,
    MapBuffer = 22,
    MapBufferRange = 23,
    UnmapBuffer = 24,
    TexStorage2D = 25,
    FramebufferTexture2D = 26,
    CheckFramebufferStatus = 27,
    InvalidateFramebuffer = 28,
    TexImage2D = 29,
    TexSubImage2D = 30,
    GenerateMipmap = 31,
    GetUniformLocation = 32,
    BindAttribLocation = 33,
    GetAttribLocation = 34,
    GenVertexArrays = 35,
    VertexAttribPointer = 36,
    VertexAttribIPointer = 37,
    CreateShader = 38,
    AttachShader = 39,
    CreateProgram = 40,
    Uniform1i = 41,
    Uniform4fv = 42,
    UniformMatrix4fv = 43,
    DrawElementsInstanced = 44,
    EnableVertexAttribArray = 45,
    VertexAttribDivisor = 46,
    LinkProgram = 47,
    GetLinkStatus = 48,
    UseProgram = 49,
    SetViewport = 50,
    FramebufferRenderbuffer = 51,
    RenderbufferStorage = 52,
    DepthMask = 53,
    DepthFunc = 54,
    SetScissor = 55,
    ClearColor = 56,
    ClearDepth = 57,
    Clear = 58,
    ClearTexSubImage = 59,
    ClearTexImage = 60,
    ClearColorRect = 61,
    PixelStorei = 62,
    ReadPixels = 63,
    Finish = 64,
    ShaderSourceByName = 65,
    TexParameteri = 66,
    CopyImageSubData = 67,
    CopyTexSubImage2D = 68,
    BlitFramebuffer = 69,
    GetIntegerv = 70,
    GetBooleanv = 71,
    GetString = 72,
    GetStringi = 73,
    GetError = 74,
    InitDefaultFramebuffer = 75,
    GetColorBuffer = 76,
    ResolveFramebuffer = 77,
    SetTextureBuffer = 78,
    SetTextureParameter = 79,
    DeleteTexture = 80,
    DeleteRenderbuffer = 81,
    DeleteFramebuffer = 82,
    DeleteBuffer = 83,
    DeleteVertexArray = 84,
    DeleteQuery = 85,
    DeleteShader = 86,
    DeleteProgram = 87,
    LockFramebuffer = 88,
    LockTexture = 89,
    LockResource = 90,
    UnlockResource = 91,
    GetResourceBuffer = 92,
    Composite = 93,
    CompositeYUV = 94,
    ApplyMask = 95,
    CreateContext = 96,
    ReferenceContext = 97,
    DestroyContext = 98,
    MakeCurrent = 99,
    ReportMemory = 100,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static INIT: Once = Once::new();
static TRACER: Mutex<Option<Tracer>> = Mutex::new(None);

thread_local! {
    static THREAD: Cell<u32> = const { Cell::new(u32::MAX) };
}

#[inline(always)]
pub(crate) fn on() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Starts tracing if `LUCINANET_GLTRACE` names a directory. Only the first call (the first context) looks.
pub(crate) fn init() {
    INIT.call_once(|| {
        let Some(dir) = std::env::var_os("LUCINANET_GLTRACE") else { return };
        let dir = PathBuf::from(dir);
        let path = dir.join(format!("trace-{}.bin", std::process::id()));
        let file = std::fs::create_dir_all(&dir).and_then(|_| File::create(&path));
        let mut out = match file {
            Ok(f) => BufWriter::with_capacity(1 << 20, f),
            Err(e) => {
                eprintln!("swgl trace: cannot create {}: {e}; not tracing", path.display());
                return;
            }
        };
        if let Err(e) = out.write_all(MAGIC) {
            eprintln!("swgl trace: cannot write {}: {e}; not tracing", path.display());
            return;
        }
        eprintln!("swgl trace: recording to {}", path.display());
        *lock() = Some(Tracer {
            out,
            path,
            threads: 0,
            ctx: 0,
            ctxs: HashMap::new(),
        });
        ENABLED.store(true, Ordering::Relaxed);
    });
}

/// Flushes and closes the trace; nothing is recorded after this. Tests call it before replaying in the same process.
#[doc(hidden)]
pub fn finish() -> Option<PathBuf> {
    ENABLED.store(false, Ordering::Relaxed);
    let mut t = lock().take()?;
    let _ = t.out.flush();
    Some(t.path)
}

fn lock() -> MutexGuard<'static, Option<Tracer>> {
    TRACER.lock().unwrap_or_else(|e| e.into_inner())
}

struct Tracer {
    out: BufWriter<File>,
    path: PathBuf,
    threads: u32,
    /// The context `MakeCurrent` made current; swgl's own `ctx` is one global, not per thread.
    ctx: u64,
    ctxs: HashMap<u64, CtxState>,
}

impl Tracer {
    fn write(&mut self, op: Op, payload: &[u8]) -> io::Result<()> {
        let thread = THREAD.with(|t| {
            if t.get() == u32::MAX {
                t.set(self.threads);
                self.threads += 1;
            }
            t.get()
        });
        let len = u32::try_from(payload.len()).map_err(|_| io::Error::other("payload over 4 GiB"))?;
        self.out.write_all(&(op as u16).to_le_bytes())?;
        self.out.write_all(&thread.to_le_bytes())?;
        self.out.write_all(&len.to_le_bytes())?;
        self.out.write_all(payload)
    }
}

/// Stops tracing for good, saying why. The file stays valid up to the last whole record.
fn stop(guard: &mut Option<Tracer>, why: &str) {
    ENABLED.store(false, Ordering::Relaxed);
    if let Some(mut t) = guard.take() {
        let _ = t.out.flush();
        eprintln!("swgl trace: {why}; tracing stopped, {} ends here", t.path.display());
    }
}

/// Appends one record.
pub(crate) fn emit(op: Op, payload: Payload) {
    let mut guard = lock();
    let Some(t) = guard.as_mut() else { return };
    let mut res = t.write(op, &payload.0);
    if op == Op::FrameMarker || op == Op::DestroyContext {
        res = res.and_then(|_| t.out.flush());
    }
    if let Err(e) = res {
        stop(&mut guard, &format!("write failed: {e}"));
    }
}

/// Gives `f` the current context's bookkeeping. `None` from `f` means the call cannot be recorded exactly:
/// tracing stops with the message `f` left in `why`.
pub(crate) fn with_state<R>(f: impl FnOnce(&mut CtxState) -> Result<R, String>) -> Option<R> {
    let mut guard = lock();
    let t = guard.as_mut()?;
    let ctx = t.ctx;
    match f(t.ctxs.entry(ctx).or_default()) {
        Ok(r) => Some(r),
        Err(why) => {
            stop(&mut guard, &why);
            None
        }
    }
}

pub(crate) fn make_current(ctx: *mut c_void) {
    if let Some(t) = lock().as_mut() {
        t.ctx = ctx as u64;
    }
}

pub(crate) fn frame_marker() {
    if on() {
        emit(Op::FrameMarker, Payload::default());
    }
}

/// What the tracer needs to know about a context to size the memory behind pointers: buffer bindings and
/// sizes, mapped ranges and the unpack row length.
#[derive(Default)]
pub(crate) struct CtxState {
    /// `PIXEL_PACK_BUFFER`, `PIXEL_UNPACK_BUFFER`, `ARRAY_BUFFER`. `ELEMENT_ARRAY_BUFFER` belongs to the VAO.
    bindings: HashMap<GLenum, GLuint>,
    vao: GLuint,
    vao_elements: HashMap<GLuint, GLuint>,
    sizes: HashMap<GLuint, GLsizeiptr>,
    /// Per target: where `MapBuffer`/`MapBufferRange` pointed and how many bytes.
    mapped: HashMap<GLenum, (usize, usize)>,
    pub(crate) unpack_row_length: GLint,
}

impl CtxState {
    pub(crate) fn bound(&self, target: GLenum) -> GLuint {
        if target == ELEMENT_ARRAY_BUFFER {
            self.vao_elements.get(&self.vao).copied().unwrap_or(0)
        } else {
            self.bindings.get(&target).copied().unwrap_or(0)
        }
    }

    pub(crate) fn bind_buffer(&mut self, target: GLenum, buffer: GLuint) {
        if target == ELEMENT_ARRAY_BUFFER {
            self.vao_elements.insert(self.vao, buffer);
        } else {
            self.bindings.insert(target, buffer);
        }
    }

    pub(crate) fn bind_vertex_array(&mut self, vao: GLuint) {
        self.vao = vao;
    }

    /// Mirrors `DeleteBuffer` in gl.cc, which unbinds the pack, unpack and array targets but not the VAOs'.
    pub(crate) fn delete_buffer(&mut self, buffer: GLuint) {
        if buffer == 0 {
            return;
        }
        self.sizes.remove(&buffer);
        for target in [PIXEL_PACK_BUFFER, PIXEL_UNPACK_BUFFER, ARRAY_BUFFER] {
            if self.bindings.get(&target) == Some(&buffer) {
                self.bindings.insert(target, 0);
            }
        }
    }

    pub(crate) fn delete_vertex_array(&mut self, vao: GLuint) {
        if vao != 0 {
            self.vao_elements.remove(&vao);
            if self.vao == vao {
                self.vao = 0;
            }
        }
    }

    pub(crate) fn buffer_data(&mut self, target: GLenum, size: GLsizeiptr) {
        let b = self.bound(target);
        self.sizes.insert(b, size);
    }

    pub(crate) fn pixel_store(&mut self, name: GLenum, param: GLint) {
        if name == UNPACK_ROW_LENGTH {
            self.unpack_row_length = param;
        }
    }

    /// `MapBuffer` maps the whole buffer bound to `target`.
    pub(crate) fn map_whole(&mut self, target: GLenum, ptr: *mut c_void) -> Result<(), String> {
        if ptr.is_null() {
            self.mapped.remove(&target);
            return Ok(());
        }
        let b = self.bound(target);
        let Some(&size) = self.sizes.get(&b) else {
            return Err(format!("MapBuffer({target:#x}) on buffer {b}, whose size no BufferData gave"));
        };
        self.mapped.insert(target, (ptr as usize, size.max(0) as usize));
        Ok(())
    }

    pub(crate) fn map_range(&mut self, target: GLenum, ptr: *mut c_void, length: GLsizeiptr) {
        if ptr.is_null() {
            self.mapped.remove(&target);
        } else {
            self.mapped.insert(target, (ptr as usize, length.max(0) as usize));
        }
    }

    pub(crate) fn unmap(&mut self, target: GLenum) -> Option<(usize, usize)> {
        self.mapped.remove(&target)
    }
}

/// A record's payload being built.
#[derive(Default)]
pub(crate) struct Payload(pub(crate) Vec<u8>);

impl Payload {
    pub(crate) fn put<T: Enc + ?Sized>(&mut self, v: &T) -> &mut Self {
        v.enc(self);
        self
    }

    /// `u32` length, then the bytes.
    pub(crate) fn bytes(&mut self, b: &[u8]) -> &mut Self {
        (b.len() as u32).enc(self);
        self.0.extend_from_slice(b);
        self
    }

    /// `ptr` is valid for `len` bytes.
    pub(crate) unsafe fn bytes_at(&mut self, ptr: *const c_void, len: usize) -> &mut Self {
        let b = if len == 0 { &[][..] } else { std::slice::from_raw_parts(ptr as *const u8, len) };
        self.bytes(b)
    }

    /// A NUL-terminated string, or `DATA_NULL`.
    pub(crate) unsafe fn c_str(&mut self, s: *const c_char) -> &mut Self {
        if s.is_null() {
            self.put(&DATA_NULL)
        } else {
            self.put(&DATA_INLINE).bytes(CStr::from_ptr(s).to_bytes())
        }
    }
}

/// How each argument type goes into a payload: scalars as their little-endian bytes, pointers as `u64`.
pub(crate) trait Enc {
    fn enc(&self, p: &mut Payload);
}

macro_rules! enc_le {
    ($($t:ty),*) => {
        $(impl Enc for $t {
            fn enc(&self, p: &mut Payload) {
                p.0.extend_from_slice(&self.to_le_bytes());
            }
        })*
    };
}
enc_le!(u8, u16, u32, i32, u64, i64, f32, f64);

impl Enc for isize {
    fn enc(&self, p: &mut Payload) {
        (*self as i64).enc(p)
    }
}

impl Enc for () {
    fn enc(&self, _: &mut Payload) {}
}

impl<T> Enc for *const T {
    fn enc(&self, p: &mut Payload) {
        (*self as usize as u64).enc(p)
    }
}

impl<T> Enc for *mut T {
    fn enc(&self, p: &mut Payload) {
        (*self as usize as u64).enc(p)
    }
}

impl Enc for crate::YuvRangedColorSpace {
    fn enc(&self, p: &mut Payload) {
        // `#[repr(u8)]` and not `Copy`: read the discriminant through the reference.
        unsafe { *(self as *const Self as *const u8) }.enc(p)
    }
}

/// Bytes per pixel of client memory for `format`/`ty`: exactly the pairs gl.cc's `internal_format_for_data`
/// accepts, through `bytes_for_internal_format`. Anything else gl.cc asserts on, so tracing refuses it too.
pub fn bytes_per_pixel(format: GLenum, ty: GLenum) -> Option<usize> {
    const RGBA_INTEGER: GLenum = 0x8D99;
    const RGB_422_APPLE: GLenum = 0x8A1F;
    const UNSIGNED_SHORT_8_8_REV_APPLE: GLenum = 0x85BB;
    Some(match (format, ty) {
        (RED, UNSIGNED_BYTE) => 1,
        (RGBA | BGRA, UNSIGNED_BYTE | UNSIGNED_INT_8_8_8_8_REV) => 4,
        (RGBA, FLOAT) => 16,
        (RGBA_INTEGER, INT) => 16,
        (RG, UNSIGNED_BYTE) => 2,
        (RGB_422_APPLE, UNSIGNED_SHORT_8_8_REV_APPLE) => 2,
        (RED, UNSIGNED_SHORT) => 2,
        (RG, UNSIGNED_SHORT) => 4,
        _ => return None,
    })
}

/// Size of the one pixel `ClearTexSubImage`/`ClearTexImage` read: the formats and types gl.cc's
/// `ClearTexSubImage` switches on.
pub fn clear_value_size(format: GLenum, ty: GLenum) -> Option<usize> {
    let comps = match format {
        RGBA => 4,
        RGB => 3,
        RG => 2,
        RED | DEPTH_COMPONENT => 1,
        _ => return None,
    };
    let size = match ty {
        FLOAT => 4,
        DOUBLE => 8,
        UNSIGNED_BYTE => 1,
        _ => return None,
    };
    Some(comps * size)
}

/// One record as read back.
pub struct Record {
    pub op: u16,
    pub thread: u32,
    pub payload: Vec<u8>,
}

/// Reads a trace file record by record.
pub struct TraceReader {
    r: BufReader<File>,
    /// Bytes read so far, header included.
    pub offset: u64,
    /// Set when the file ended inside a record (the process died mid-write).
    pub truncated: bool,
}

impl TraceReader {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut r = BufReader::with_capacity(1 << 20, File::open(path)?);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a swgl trace (bad magic)"));
        }
        Ok(TraceReader { r, offset: 8, truncated: false })
    }

    /// The next record, or `None` at the end (check `truncated`).
    pub fn next_record(&mut self) -> io::Result<Option<Record>> {
        let mut head = [0u8; RECORD_HEADER];
        match read_full(&mut self.r, &mut head)? {
            0 => return Ok(None),
            n if n < RECORD_HEADER => {
                self.truncated = true;
                return Ok(None);
            }
            _ => {}
        }
        let op = u16::from_le_bytes([head[0], head[1]]);
        let thread = u32::from_le_bytes(head[2..6].try_into().unwrap());
        let len = u32::from_le_bytes(head[6..10].try_into().unwrap()) as usize;
        let mut payload = vec![0u8; len];
        if read_full(&mut self.r, &mut payload)? < len {
            self.truncated = true;
            return Ok(None);
        }
        self.offset += (RECORD_HEADER + len) as u64;
        Ok(Some(Record { op, thread, payload }))
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Reads a payload back, in the order `Payload` wrote it.
pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Cursor { buf, pos: 0 }
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.buf.len());
        let end = end.ok_or_else(|| format!("payload too short: need {n} bytes at {}", self.pos))?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    pub fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn f64(&mut self) -> Result<f64, String> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// `u32` length, then that many bytes.
    pub fn bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
}
