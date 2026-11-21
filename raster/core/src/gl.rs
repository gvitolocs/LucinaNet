/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Port of swgl's `gl.cc`: the GL context, its object stores and every entry point the 100 `extern "C"` functions of
//! `swgl/src/swgl_fns.rs` reach.
//!
//! gl.cc keeps the current context in a static `ctx` and works on it; here each entry point is a method of
//! [`Context`] (the name in snake_case), and the caller keeps the current context (`swgl/src/rust_backend.rs`). The
//! entry points that take a `LockedTexture*` and no context (`LockResource`, `UnlockResource`, `Composite`,
//! `GetResourceBuffer`, ...) are free `unsafe fn`s on `*mut Texture`, as in the C++.
//!
//! Where gl.cc indexes a store with `operator[]` (which creates the object), the Rust does too
//! ([`ObjectStore::get_mut`]), and where it uses `find` (which does not), [`ObjectStore::find`]: the ids `Gen*`
//! hands out depend on which objects exist, and the trace replayer compares them with the recording.
//!
//! C++ `assert`s are compiled out in release swgl; they are `debug_assert!`s here. Where the C++ would read or write
//! outside a buffer in release, the Rust clips to the buffer instead, as the doc comment of each function says.

use core::ffi::{c_void, CStr};
use core::sync::atomic::Ordering;

use crate::glsl::{bit_cast, round_pixel, Float, V8};
use crate::composite;
use crate::prof;
use crate::program::{ProgramImpl, VertexAttrib, MAX_ATTRIBS, NULL_ATTRIB};
use crate::pool::Pool;
use crate::rasterize::{self, DrawCtx};
use std::sync::atomic::{AtomicI64, Ordering as AtomicOrdering};
use std::sync::Mutex;
use crate::RasterState;
use lucinanet_raster_rules as rules;

pub mod defs;
use defs::*;

pub use crate::blend::{BlendKey, BLEND_KEY_NONE};
pub use crate::texture::{
    bytes_for_internal_format, clear_buffer, prepare_texture, request_clear, IntPoint, IntRect, Texture,
};

/// `MAX_TEXTURE_UNITS`.
pub const MAX_TEXTURE_UNITS: usize = 16;
/// `MAX_TEXEL_OFFSET` (texture.h).
pub const MAX_TEXEL_OFFSET: i32 = 8;

/// The extensions `GetStringi(GL_EXTENSIONS, i)` lists.
pub const EXTENSIONS: [&CStr; 12] = [
    c"GL_ARB_blend_func_extended",
    c"GL_ARB_clear_texture",
    c"GL_ARB_copy_image",
    c"GL_ARB_draw_instanced",
    c"GL_ARB_explicit_attrib_location",
    c"GL_ARB_instanced_arrays",
    c"GL_ARB_invalidate_subdata",
    c"GL_ARB_texture_storage",
    c"GL_EXT_timer_query",
    c"GL_KHR_blend_equation_advanced",
    c"GL_KHR_blend_equation_advanced_coherent",
    c"GL_APPLE_rgb_422",
];

// ---- small types -------------------------------------------------------------------------------------------------

/// `IntRange`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IntRange {
    pub start: i32,
    pub end: i32,
}

impl IntRange {
    pub fn len(&self) -> i32 {
        self.end.wrapping_sub(self.start)
    }
    pub fn is_empty(&self) -> bool {
        self.len() <= 0
    }
    pub fn intersect(&self, r: IntRange) -> IntRange {
        IntRange { start: self.start.max(r.start), end: self.end.min(r.end) }
    }
}

/// `FloatRange`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FloatRange {
    pub start: f32,
    pub end: f32,
}

impl FloatRange {
    pub fn clip_x(&self, x: f32) -> f32 {
        crate::glsl::clamp(x, self.start, self.end)
    }
    pub fn clip(&self, r: FloatRange) -> FloatRange {
        FloatRange { start: self.clip_x(r.start), end: self.clip_x(r.end) }
    }
    pub fn merge(&self, r: FloatRange) -> FloatRange {
        FloatRange { start: crate::glsl::min(self.start, r.start), end: crate::glsl::max(self.end, r.end) }
    }
    pub fn round(&self) -> IntRange {
        let f = crate::glsl::f32_to_i32_scalar;
        IntRange { start: f((self.start + 0.5).floor()), end: f((self.end + 0.5).floor()) }
    }
    pub fn round_out(&self) -> IntRange {
        let f = crate::glsl::f32_to_i32_scalar;
        IntRange { start: f(self.start.floor()), end: f(self.end.ceil()) }
    }
}

/// `Query`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Query {
    pub value: u64,
}

/// `Buffer`: `data.len()` is the C++ `capacity` (realloc never shrinks it), `size` the bytes in use.
#[derive(Debug, Default)]
pub struct Buffer {
    data: Vec<u8>,
    pub size: usize,
}

impl Buffer {
    /// `allocate(new_size)`: grows like `realloc`, keeping the contents; false if the allocation failed.
    pub fn allocate(&mut self, new_size: usize) -> bool {
        if new_size == self.size {
            return true;
        }
        if new_size <= self.data.len() {
            self.size = new_size;
            return true;
        }
        if !crate::texture::realloc_untouched(&mut self.data, new_size) {
            self.cleanup();
            return false;
        }
        self.size = new_size;
        true
    }

    pub fn cleanup(&mut self) {
        self.data = Vec::new();
        self.size = 0;
    }

    /// `buf`, null when nothing was ever allocated.
    pub fn buf(&mut self) -> *mut u8 {
        if self.data.is_empty() {
            core::ptr::null_mut()
        } else {
            self.data.as_mut_ptr()
        }
    }

    pub fn has_buf(&self) -> bool {
        !self.data.is_empty()
    }

    /// The `size` bytes in use.
    pub fn bytes(&self) -> &[u8] {
        &self.data[..self.size]
    }

    /// `get_data(offset)`: the offset into the buffer, if it is inside it.
    pub fn get_data(&self, offset: usize) -> Option<usize> {
        (self.has_buf() && offset < self.size).then_some(offset)
    }
}

/// `Framebuffer`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Framebuffer {
    pub color_attachment: u32,
    pub depth_attachment: u32,
}

/// `Renderbuffer`: a texture under another name.
#[derive(Clone, Copy, Debug, Default)]
pub struct Renderbuffer {
    pub texture: u32,
}

/// A vertex attribute's state in a `VertexArray` (gl.cc's `VertexAttrib` without the buffer pointer): the buffer
/// is looked up by `vertex_buffer` when the vertex array is validated and lent to the shader for each draw.
#[derive(Clone, Copy, Debug, Default)]
pub struct VertexAttribState {
    pub size: usize,
    pub type_: u32,
    pub normalized: bool,
    pub stride: i32,
    pub offset: u32,
    pub enabled: bool,
    pub divisor: u32,
    pub vertex_array: i32,
    pub vertex_buffer: i32,
    /// `buf_size` as of the last `validate` (0: no buffer, the C++ `buf == nullptr`).
    pub buf_size: usize,
}

impl VertexAttribState {
    /// `disable()`.
    pub fn disable(&mut self) {
        self.enabled = false;
        self.buf_size = 0;
    }
}

/// `VertexArray`.
#[derive(Clone, Debug)]
pub struct VertexArray {
    pub attribs: [VertexAttribState; MAX_ATTRIBS],
    pub max_attrib: i32,
    pub element_array_buffer_binding: u32,
}

impl Default for VertexArray {
    fn default() -> Self {
        VertexArray { attribs: [VertexAttribState::default(); MAX_ATTRIBS], max_attrib: -1, element_array_buffer_binding: 0 }
    }
}

/// The program registry (`load_shader.h`): the generated programs of `crate::shaders`. An unknown name (or a
/// program needing the `ext` feature without it) gives `None`, so `ShaderSourceByName` logs it and the program
/// never links, as swgl does with a name it lacks.
pub fn load_shader(name: &str) -> Option<Box<dyn ProgramImpl>> {
    crate::shaders::load_shader(name).map(|loader| loader())
}

/// `Shader`: its type, and the program `load_shader` made for its name, cloned for each program it is attached to.
#[derive(Default)]
pub struct Shader {
    pub type_: u32,
    pub loader: Option<Box<dyn ProgramImpl>>,
}

/// `Program`. `vert_impl`/`frag_impl` say whether `LinkProgram` fetched the shader halves.
#[derive(Default)]
pub struct Program {
    pub impl_: Option<Box<dyn ProgramImpl>>,
    pub vert_impl: bool,
    pub frag_impl: bool,
    pub deleted: bool,
}

// ---- ObjectStore -------------------------------------------------------------------------------------------------

/// `ObjectStore<O>`: objects by id, 0 reserved, ids reused lowest first.
pub struct ObjectStore<O> {
    objects: Vec<Option<Box<O>>>,
    first_free: usize,
    invalid: O,
}

/// The largest id a store grows to. The C++ `realloc`s the pointer table to any size and returns `invalid` when that
/// fails; a table this large would be gigabytes, so ids past it get `invalid` here.
const MAX_OBJECT_ID: usize = 1 << 24;

impl<O: Default> Default for ObjectStore<O> {
    fn default() -> Self {
        ObjectStore { objects: Vec::new(), first_free: 1, invalid: O::default() }
    }
}

impl<O: Default> ObjectStore<O> {
    fn grow(&mut self, i: usize) -> bool {
        if i >= MAX_OBJECT_ID {
            return false;
        }
        let mut new_size = if self.objects.is_empty() { 8 } else { self.objects.len() };
        while new_size <= i {
            new_size += new_size / 2;
        }
        self.objects.resize_with(new_size, || None);
        true
    }

    /// `insert(i, o)`: puts `o` at `i` if that slot is empty.
    pub fn insert_at(&mut self, i: usize, o: O) {
        if i >= self.objects.len() && !self.grow(i) {
            return;
        }
        if self.objects[i].is_none() {
            self.objects[i] = Some(Box::new(o));
        }
    }

    fn next_free(&mut self) -> usize {
        let mut i = self.first_free;
        while i < self.objects.len() && self.objects[i].is_some() {
            i += 1;
        }
        self.first_free = i;
        i
    }

    /// `insert(o)`: the lowest free id.
    pub fn insert(&mut self, o: O) -> u32 {
        let i = self.next_free();
        self.insert_at(i, o);
        i as u32
    }

    /// `operator[]`: the object, created if missing.
    pub fn get_mut(&mut self, i: u32) -> &mut O {
        let i = i as usize;
        self.insert_at(i, O::default());
        match self.objects.get_mut(i) {
            Some(Some(o)) => o,
            _ => &mut self.invalid,
        }
    }

    /// `find(i)`.
    pub fn find(&self, i: u32) -> Option<&O> {
        self.objects.get(i as usize).and_then(|o| o.as_deref())
    }

    pub fn find_mut(&mut self, i: u32) -> Option<&mut O> {
        self.objects.get_mut(i as usize).and_then(|o| o.as_deref_mut())
    }

    /// `erase(i, should_delete)` without the delete: the removed object, for the caller to drop or keep.
    pub fn take(&mut self, i: u32) -> Option<Box<O>> {
        let i = i as usize;
        let o = self.objects.get_mut(i)?.take()?;
        if i < self.first_free {
            self.first_free = i;
        }
        Some(o)
    }

    /// The live objects (`begin()..end()`).
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut O> {
        self.objects.iter_mut().filter_map(|o| o.as_deref_mut())
    }

    pub fn iter(&self) -> impl Iterator<Item = &O> {
        self.objects.iter().filter_map(|o| o.as_deref())
    }

    /// Two different objects at once, both created if missing. `None` if `a == b` or an id is out of range.
    pub fn get_two_mut(&mut self, a: u32, b: u32) -> Option<(&mut O, &mut O)> {
        if a == b {
            return None;
        }
        self.get_mut(a);
        self.get_mut(b);
        let (a, b) = (a as usize, b as usize);
        if a >= self.objects.len() || b >= self.objects.len() {
            return None;
        }
        let (lo, hi) = (a.min(b), a.max(b));
        let (left, right) = self.objects.split_at_mut(hi);
        let (x, y) = (left[lo].as_deref_mut()?, right[0].as_deref_mut()?);
        Some(if a < b { (x, y) } else { (y, x) })
    }
}

/// `unlink(binding, n)`.
fn unlink(binding: &mut u32, n: u32) -> bool {
    if *binding == n {
        *binding = 0;
        true
    } else {
        false
    }
}

/// `Context::TextureUnit`.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextureUnit {
    pub texture_2d_binding: u32,
    pub texture_rectangle_binding: u32,
}

impl TextureUnit {
    fn unlink(&mut self, n: u32) {
        unlink(&mut self.texture_2d_binding, n);
        unlink(&mut self.texture_rectangle_binding, n);
    }
}

/// gl.cc's `Context`.
pub struct Context {
    pub references: i32,

    pub queries: ObjectStore<Query>,
    pub buffers: ObjectStore<Buffer>,
    pub textures: ObjectStore<Texture>,
    pub vertex_arrays: ObjectStore<VertexArray>,
    pub framebuffers: ObjectStore<Framebuffer>,
    pub renderbuffers: ObjectStore<Renderbuffer>,
    pub shaders: ObjectStore<Shader>,
    pub programs: ObjectStore<Program>,

    pub last_error: u32,

    pub viewport: IntRect,

    pub blend: bool,
    pub blendfunc_srgb: u32,
    pub blendfunc_drgb: u32,
    pub blendfunc_sa: u32,
    pub blendfunc_da: u32,
    pub blend_equation: u32,
    pub blendcolor: V8<u16>,
    pub blend_key: BlendKey,

    pub depthtest: bool,
    pub depthmask: bool,
    pub depthfunc: u32,

    pub scissortest: bool,
    pub scissor: IntRect,

    pub clearcolor: [f32; 4],
    pub cleardepth: f64,

    pub unpack_row_length: i32,

    pub shaded_rows: i32,
    pub shaded_pixels: i32,

    pub texture_units: [TextureUnit; MAX_TEXTURE_UNITS],
    pub active_texture_unit: usize,

    pub current_program: u32,

    pub current_vertex_array: u32,
    pub validate_vertex_array: bool,

    pub pixel_pack_buffer_binding: u32,
    pub pixel_unpack_buffer_binding: u32,
    pub array_buffer_binding: u32,
    pub time_elapsed_query: u32,
    pub samples_passed_query: u32,
    pub renderbuffer_binding: u32,
    pub draw_framebuffer_binding: u32,
    pub read_framebuffer_binding: u32,
    pub unknown_binding: u32,

    /// gl.cc's statics `vertex_shader`/`fragment_shader`: the program whose halves `setup_program` installed, if it
    /// had them. They are set only by `UseProgram` and `MakeCurrent`, so a `LinkProgram` after `UseProgram` does not
    /// install them, as in the C++.
    pub vertex_shader: Option<u32>,
    pub fragment_shader: Option<u32>,

    /// swgl's raster statics for this context's draws (one rasterizing thread).
    pub raster: RasterState,

    /// Threads a large draw is split across (raster/DESIGN.md, "Parallel rasterizing"); 1 draws on the calling
    /// thread only. `LUCINANET_RASTER_THREADS` by default, [`Context::set_raster_threads`] to change it.
    raster_threads: usize,
    /// Draws whose target (scissored rows times width) has at most this many pixels stay on the calling thread.
    raster_split_pixels: i64,
    /// The workers, made at the first split draw and kept for the context's life.
    pool: Option<Pool>,
    /// For tests and measurements: stripes dealt out statically (`s % n == t`) instead of claimed from a counter.
    raster_static_stripes: bool,
    /// For tests: threads `from..` sleep `d` per stripe they take, as slow cores would.
    raster_slow_workers: Option<(usize, std::time::Duration)>,
    /// Diagnostic: the stripes each worker drew in the most recent split draw, indexed by worker number.
    raster_last_split_stripes: Vec<u32>,
}

/// `LUCINANET_RASTER_CLAIM=area`, read once: claim stripes by covered area, most first, instead of ascending.
fn claim_by_area() -> bool {
    static AREA: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AREA.get_or_init(|| std::env::var("LUCINANET_RASTER_CLAIM").is_ok_and(|v| v == "area"))
}

/// What one thread of a split draw draws with: its draw context, the color and depth texture views, and the stripes
/// it claimed.
type StripeJob = (DrawCtx, Texture, Texture, Vec<i64>);
/// A primitive's raster state and fragment shader as its vertex stage left them.
type ShadedState = (RasterState, Box<dyn crate::program::FragmentShaderImpl>);

/// The default of [`Context::set_raster_split_pixels`]: a target of at most this many pixels is drawn on one thread.
pub const RASTER_SPLIT_PIXELS: i64 = 64 * 1024;
/// A split draw's primitives must cover this many pixels on average (raster/PERF.md: text runs of 400-pixel glyphs
/// were slower on 8 threads than on one).
pub const RASTER_SPLIT_MIN_PRIMITIVE_PIXELS: i64 = 1024;
/// Both thresholds can be overridden for measurements, read once: `LUCINANET_RASTER_SPLIT_PIXELS` (a draw whose
/// primitives cover at most this many pixels stays on one thread) and `LUCINANET_RASTER_SPLIT_MIN_PRIM` (the
/// average primitive area a split draw needs). Unset or unparsable: the constants above.
fn split_thresholds() -> (i64, i64) {
    static T: std::sync::OnceLock<(i64, i64)> = std::sync::OnceLock::new();
    *T.get_or_init(|| {
        let get = |name: &str, default: i64| {
            std::env::var(name).ok().and_then(|v| v.trim().parse::<i64>().ok()).filter(|&n| n >= 0).unwrap_or(default)
        };
        (
            get("LUCINANET_RASTER_SPLIT_PIXELS", RASTER_SPLIT_PIXELS),
            get("LUCINANET_RASTER_SPLIT_MIN_PRIM", RASTER_SPLIT_MIN_PRIMITIVE_PIXELS),
        )
    })
}

impl Context {
    /// The threads large draws are split across, at least 1 (the one-thread path) and at most `rules::MAX_THREADS`.
    pub fn set_raster_threads(&mut self, threads: usize) {
        let threads = threads.clamp(1, lucinanet_raster_rules::MAX_THREADS as usize);
        if threads != self.raster_threads {
            self.raster_threads = threads;
            self.pool = None;
        }
    }
    pub fn raster_threads(&self) -> usize {
        self.raster_threads
    }
    /// Draws whose target has more than `pixels` pixels are split across the threads (0: every draw).
    pub fn set_raster_split_pixels(&mut self, pixels: i64) {
        self.raster_split_pixels = pixels;
    }
    /// Deals stripes out statically, thread `t` of `n` taking `s % n == t` (as before claiming), for comparisons.
    pub fn set_raster_static_stripes(&mut self, on: bool) {
        self.raster_static_stripes = on;
    }
    /// Test hook: threads `from..` (0 is the calling thread) sleep `per_stripe` for each stripe they take.
    pub fn set_raster_slow_workers(&mut self, slow: Option<(usize, std::time::Duration)>) {
        self.raster_slow_workers = slow;
    }
    /// Diagnostic: the stripes each worker drew in the most recent split draw, indexed by worker number.
    pub fn raster_last_split_stripes(&self) -> Vec<u32> {
        self.raster_last_split_stripes.clone()
    }
}

impl Default for Context {
    fn default() -> Self {
        Context {
            references: 1,
            queries: ObjectStore::default(),
            buffers: ObjectStore::default(),
            textures: ObjectStore::default(),
            vertex_arrays: ObjectStore::default(),
            framebuffers: ObjectStore::default(),
            renderbuffers: ObjectStore::default(),
            shaders: ObjectStore::default(),
            programs: ObjectStore::default(),
            last_error: GL_NO_ERROR,
            viewport: IntRect::default(),
            blend: false,
            blendfunc_srgb: GL_ONE,
            blendfunc_drgb: GL_ZERO,
            blendfunc_sa: GL_ONE,
            blendfunc_da: GL_ZERO,
            blend_equation: GL_FUNC_ADD,
            blendcolor: V8::splat(0),
            blend_key: BLEND_KEY_NONE,
            depthtest: false,
            depthmask: true,
            depthfunc: GL_LESS,
            scissortest: false,
            scissor: IntRect::default(),
            clearcolor: [0.0; 4],
            cleardepth: 1.0,
            unpack_row_length: 0,
            shaded_rows: 0,
            shaded_pixels: 0,
            texture_units: [TextureUnit::default(); MAX_TEXTURE_UNITS],
            active_texture_unit: 0,
            current_program: 0,
            current_vertex_array: 0,
            validate_vertex_array: true,
            pixel_pack_buffer_binding: 0,
            pixel_unpack_buffer_binding: 0,
            array_buffer_binding: 0,
            time_elapsed_query: 0,
            samples_passed_query: 0,
            renderbuffer_binding: 0,
            draw_framebuffer_binding: 0,
            read_framebuffer_binding: 0,
            unknown_binding: 0,
            vertex_shader: None,
            fragment_shader: None,
            raster: RasterState::default(),
            raster_threads: crate::pool::default_threads(),
            raster_split_pixels: split_thresholds().0,
            pool: None,
            raster_static_stripes: false,
            raster_slow_workers: None,
            raster_last_split_stripes: Vec::new(),
        }
    }
}

impl Drop for Context {
    /// `delete c` frees every texture, locked or not (the C++ asserts none is locked). A texture still locked here is
    /// in use by another thread through its `LockedTexture*`, so it becomes a zombie instead, freed by the last
    /// `UnlockResource`.
    fn drop(&mut self) {
        for slot in self.textures.objects.iter_mut() {
            if slot.as_ref().is_some_and(|t| t.locked.load(Ordering::Acquire) > 0) {
                let mut t = slot.take().unwrap();
                t.set_zombie(true);
                let _ = Box::into_raw(t);
            }
        }
    }
}

/// `remap_blendfunc(rgb, a)`: an alpha factor that is the alpha form of the rgb factor becomes the rgb factor.
pub fn remap_blendfunc(rgb: u32, a: u32) -> u32 {
    let pairs = [
        (GL_SRC_ALPHA, GL_SRC_COLOR),
        (GL_ONE_MINUS_SRC_ALPHA, GL_ONE_MINUS_SRC_COLOR),
        (GL_DST_ALPHA, GL_DST_COLOR),
        (GL_ONE_MINUS_DST_ALPHA, GL_ONE_MINUS_DST_COLOR),
        (GL_CONSTANT_ALPHA, GL_CONSTANT_COLOR),
        (GL_ONE_MINUS_CONSTANT_ALPHA, GL_ONE_MINUS_CONSTANT_COLOR),
        (GL_SRC_COLOR, GL_SRC_ALPHA),
        (GL_ONE_MINUS_SRC_COLOR, GL_ONE_MINUS_SRC_ALPHA),
        (GL_DST_COLOR, GL_DST_ALPHA),
        (GL_ONE_MINUS_DST_COLOR, GL_ONE_MINUS_DST_ALPHA),
        (GL_CONSTANT_COLOR, GL_CONSTANT_ALPHA),
        (GL_ONE_MINUS_CONSTANT_COLOR, GL_ONE_MINUS_CONSTANT_ALPHA),
        (GL_SRC1_ALPHA, GL_SRC1_COLOR),
        (GL_ONE_MINUS_SRC1_ALPHA, GL_ONE_MINUS_SRC1_COLOR),
        (GL_SRC1_COLOR, GL_SRC1_ALPHA),
        (GL_ONE_MINUS_SRC1_COLOR, GL_ONE_MINUS_SRC1_ALPHA),
    ];
    for (alpha, color) in pairs {
        if a == alpha && rgb == color {
            return color;
        }
    }
    a
}

/// `remap_internal_format(format)`.
pub fn remap_internal_format(format: u32) -> u32 {
    match format {
        GL_DEPTH_COMPONENT => GL_DEPTH_COMPONENT24,
        GL_RGBA => GL_RGBA8,
        GL_RED => GL_R8,
        GL_RG => GL_RG8,
        GL_RGB_422_APPLE => GL_RGB_RAW_422_APPLE,
        f => f,
    }
}

/// `format_requires_conversion(external_format, internal_format)`.
pub fn format_requires_conversion(external_format: u32, internal_format: u32) -> bool {
    match external_format {
        GL_RGBA => internal_format == GL_RGBA8,
        GL_RED => internal_format != GL_R8 && internal_format != GL_R16,
        GL_RG => internal_format != GL_RG8 && internal_format != GL_RG16,
        _ => false,
    }
}

/// `internal_format_for_data(format, ty)`; 0 for a pair gl.cc does not know (it asserts).
pub fn internal_format_for_data(format: u32, ty: u32) -> u32 {
    match (format, ty) {
        (GL_RED, GL_UNSIGNED_BYTE) => GL_R8,
        (GL_RGBA | GL_BGRA, GL_UNSIGNED_BYTE | GL_UNSIGNED_INT_8_8_8_8_REV) => GL_RGBA8,
        (GL_RGBA, GL_FLOAT) => GL_RGBA32F,
        (GL_RGBA_INTEGER, GL_INT) => GL_RGBA32I,
        (GL_RG, GL_UNSIGNED_BYTE) => GL_RG8,
        (GL_RGB_422_APPLE, GL_UNSIGNED_SHORT_8_8_REV_APPLE) => GL_RGB_RAW_422_APPLE,
        (GL_RED, GL_UNSIGNED_SHORT) => GL_R16,
        (GL_RG, GL_UNSIGNED_SHORT) => GL_RG16,
        _ => 0,
    }
}

/// `bytes_per_type(type)`.
pub fn bytes_per_type(ty: u32) -> i32 {
    match ty {
        GL_INT | GL_FLOAT => 4,
        GL_UNSIGNED_SHORT => 2,
        GL_UNSIGNED_BYTE => 1,
        _ => 0,
    }
}

/// `uint32_t(x)` of a non-negative float or double as clang lowers it on x86-64 (a 64-bit truncating convert, low
/// 32 bits kept); 0 for values no `int64_t` holds.
fn to_u32(x: f64) -> u32 {
    if (-9.2e18..9.2e18).contains(&x) {
        x as i64 as u32
    } else {
        0
    }
}

// ---- pixel copies with bounds (convert_copy) -----------------------------------------------------------------------

/// A position in a byte buffer, with the bounds `clip_ptrs_against_bounds` keeps it in (`None`: unbounded, the C++
/// passing `nullptr`). Offsets are relative to `base`.
#[derive(Clone, Copy)]
struct Cursor {
    base: *mut u8,
    off: i64,
    bounds: Option<(i64, i64)>,
}

/// `clip_ptrs_against_bounds<T, N>`: moves `dst`/`src` and shortens `len` (in units of `tsize`-byte `T`s of the
/// source; the destination advances `n` of them per source unit) to stay within both bounds. 1: past the end of a
/// bound, -1: entirely before one, 0: the clipped span (possibly empty) is in bounds.
fn clip_ptrs_against_bounds(dst: &mut Cursor, src: &mut Cursor, len: &mut usize, tsize: i64, n: i64) -> i32 {
    if let Some((b0, b1)) = dst.bounds {
        if dst.off < b0 {
            let offset = ((b0 - dst.off) / tsize / n) as usize;
            if *len <= offset {
                *len = 0;
                return -1;
            }
            src.off += offset as i64 * tsize;
            dst.off += offset as i64 * n * tsize;
            *len -= offset;
        }
        if dst.off >= b1 {
            *len = 0;
            return 1;
        }
        let remaining = ((b1 - dst.off) / tsize / n) as usize;
        if *len > remaining {
            *len = remaining;
        }
    }
    if let Some((b0, b1)) = src.bounds {
        if src.off < b0 {
            let offset = ((b0 - src.off) / tsize) as usize;
            if *len <= offset {
                *len = 0;
                return -1;
            }
            src.off += offset as i64 * tsize;
            dst.off += offset as i64 * n * tsize;
            *len -= offset;
        }
        if src.off >= b1 {
            *len = 0;
            return 1;
        }
        let remaining = ((b1 - src.off) / tsize) as usize;
        if *len > remaining {
            *len = remaining;
        }
    }
    0
}

/// `convert_copy(...)`: `height` rows of `width` pixels from `src` to `dst`, converting RGBA to BGRA and RED to
/// BGRA8 or RGBA32F where gl.cc does, each row clipped to the bounds.
///
/// # Safety
/// Bytes the clipped rows reach must be valid: inside `bounds` where a cursor has them, and for an unbounded cursor
/// the caller vouches for `height` rows of `stride` bytes (as the C++ caller does).
#[allow(clippy::too_many_arguments)]
unsafe fn convert_copy(
    external_format: u32,
    internal_format: u32,
    mut dst: Cursor,
    dst_stride: i64,
    mut src: Cursor,
    src_stride: i64,
    width: usize,
    height: usize,
) {
    // (T size, N, how a row is copied)
    enum Row {
        Bgra,
        RedToBgra8,
        RedToRgba32f,
        Bytes,
    }
    let (tsize, n, kind, units) = match (external_format, internal_format) {
        (GL_RGBA, GL_RGBA8) => (4, 1, Row::Bgra, width),
        (GL_RED, GL_RGBA8) => (1, 4, Row::RedToBgra8, width),
        (GL_RED, GL_RGBA32F) => (4, 4, Row::RedToRgba32f, width),
        (GL_RED, f) if f != GL_R8 && f != GL_R16 && f != GL_RGBA8 && f != GL_RGBA32F => {
            debug_assert!(false, "unsupported format conversion from {external_format:#x} to {internal_format:#x}");
            return;
        }
        _ => (1, 1, Row::Bytes, width * bytes_for_internal_format(internal_format).max(0) as usize),
    };
    for _ in 0..height {
        let (mut d, mut s) = (dst, src);
        let mut len = units;
        if clip_ptrs_against_bounds(&mut d, &mut s, &mut len, tsize, n) > 0 {
            return;
        }
        if len > 0 {
            let dp = d.base.wrapping_offset(d.off as isize);
            let sp = s.base.wrapping_offset(s.off as isize) as *const u8;
            // SAFETY: the clipped span lies inside both bounds, or the caller vouches for an unbounded side.
            unsafe {
                match kind {
                    Row::Bgra => {
                        for i in 0..len {
                            let p = (sp as *const u32).add(i).read_unaligned();
                            let rb = p & 0x00FF_00FF;
                            (dp as *mut u32).add(i).write_unaligned((p & 0xFF00_FF00) | (rb << 16) | (rb >> 16));
                        }
                    }
                    Row::RedToBgra8 => {
                        for i in 0..len {
                            let r = *sp.add(i);
                            dp.add(i * 4).cast::<[u8; 4]>().write_unaligned([0, 0, r, 255]);
                        }
                    }
                    Row::RedToRgba32f => {
                        for i in 0..len {
                            let r = (sp as *const f32).add(i).read_unaligned();
                            (dp as *mut [f32; 4]).add(i).write_unaligned([r, 0.0, 0.0, 1.0]);
                        }
                    }
                    Row::Bytes => core::ptr::copy(sp, dp, len),
                }
            }
        }
        dst.off += dst_stride;
        src.off += src_stride;
    }
}

/// A texture's buffer as a bounded cursor at `(x, y)`: bounds `[buf, end_ptr())` as gl.cc clips to.
fn texture_cursor(t: &Texture, x: i32, y: i32) -> Cursor {
    let off = i64::from(y) * t.stride() as i64 + i64::from(x) * i64::from(t.bpp());
    let end = (i64::from(t.height) - 1) * t.stride() as i64 + i64::from(t.width) * i64::from(t.bpp());
    Cursor { base: t.buf, off, bounds: Some((0, end.max(0))) }
}

// ---- set_tex_storage, clears -------------------------------------------------------------------------------------

/// `set_tex_storage(t, external_format, width, height, buf, stride, min_width, min_height)`; false on out of memory.
///
/// # Safety
/// A non-null `buf` must hold `height` rows of `stride` bytes and outlive its use by the texture (the embedder's
/// `SetTextureBuffer`/`InitDefaultFramebuffer` contract).
#[allow(clippy::too_many_arguments)]
unsafe fn set_tex_storage(
    t: &mut Texture,
    external_format: u32,
    width: i32,
    height: i32,
    buf: *mut u8,
    stride: i32,
    min_width: i32,
    min_height: i32,
) -> bool {
    let internal_format = remap_internal_format(external_format);
    let mut changed = false;
    if t.width != width || t.height != height || t.internal_format != internal_format {
        changed = true;
        t.internal_format = internal_format;
        t.width = width;
        t.height = height;
    }
    let should_free = buf.is_null() || format_requires_conversion(external_format, internal_format);
    if t.should_free() != should_free {
        changed = true;
        t.cleanup();
        t.set_should_free(should_free);
    }
    if !should_free {
        t.set_buffer(buf, stride.max(0) as usize);
    }
    t.disable_delayed_clear();
    let ok = t.allocate(changed, min_width, min_height);
    if !buf.is_null() && should_free {
        let dst = texture_cursor(t, 0, 0);
        let src = Cursor { base: buf, off: 0, bounds: None };
        // SAFETY: the destination is clipped to the texture; the caller vouches for `height` rows of `stride`.
        unsafe {
            convert_copy(external_format, internal_format, dst, t.stride() as i64, src, i64::from(stride), width.max(0) as usize, height.max(0) as usize)
        };
    }
    ok
}

/// What `ClearTexSubImage` reads through its `data` pointer, decoded.
#[derive(Clone, Copy, Debug)]
pub enum ClearData {
    /// `GL_FLOAT`: the first 1..4 components, per format.
    Float([f32; 4]),
    /// `GL_DOUBLE` (depth).
    Double(f64),
    /// `GL_UNSIGNED_BYTE`.
    Bytes([u8; 4]),
    /// No data or a type gl.cc does not read.
    None,
}

impl ClearData {
    /// Reads the components `format` names, of `ty`, from `data`.
    ///
    /// # Safety
    /// `data` is null or points at one clear value of `format`/`ty` (`trace::clear_value_size` bytes).
    pub unsafe fn read(format: u32, ty: u32, data: *const c_void) -> ClearData {
        if data.is_null() {
            return ClearData::None;
        }
        let n = match format {
            GL_RGBA => 4,
            GL_RGB => 3,
            GL_RG => 2,
            GL_RED | GL_DEPTH_COMPONENT => 1,
            _ => 0,
        };
        // SAFETY: the caller guarantees `n` components of `ty`.
        unsafe {
            match ty {
                GL_FLOAT => {
                    let mut v = [0.0f32; 4];
                    for (i, c) in v.iter_mut().enumerate().take(n) {
                        *c = (data as *const f32).add(i).read_unaligned();
                    }
                    ClearData::Float(v)
                }
                GL_DOUBLE => ClearData::Double((data as *const f64).read_unaligned()),
                GL_UNSIGNED_BYTE => {
                    let mut v = [0u8; 4];
                    for (i, c) in v.iter_mut().enumerate().take(n) {
                        *c = *(data as *const u8).add(i);
                    }
                    ClearData::Bytes(v)
                }
                _ => ClearData::None,
            }
        }
    }
}

// ---- locks, zombies, composites (no context) ---------------------------------------------------------------------

/// `LockResource(resource)`: one more lock on an already locked texture.
///
/// # Safety
/// `resource` is null or a pointer `LockTexture`/`LockFramebuffer` returned that still holds a lock.
pub unsafe fn lock_resource(resource: *mut Texture) {
    if resource.is_null() {
        return;
    }
    // SAFETY: a locked texture stays alive (as a zombie if deleted) until its last unlock.
    unsafe { (*resource).locked.fetch_add(1, Ordering::AcqRel) };
}

/// `UnlockResource(resource)`: drops a lock and returns how many remain (-1 for null). The last unlock of a
/// deleted texture frees it.
///
/// # Safety
/// As [`lock_resource`]; after it returns 0 for a zombie, `resource` is gone.
pub unsafe fn unlock_resource(resource: *mut Texture) -> i32 {
    if resource.is_null() {
        return -1;
    }
    // SAFETY: the caller holds a lock, so the texture is alive.
    let locked = unsafe { (*resource).locked.fetch_sub(1, Ordering::AcqRel) };
    if locked <= 0 {
        debug_assert!(false, "unlock of an unlocked texture");
    } else if locked == 1 && unsafe { (*resource).zombie() } {
        // SAFETY: zombies come from `Box::into_raw` in `delete_texture` (or `Context::drop`), and this was the
        // last lock, so nobody else points at it.
        drop(unsafe { Box::from_raw(resource) });
    }
    locked - 1
}

/// `GetResourceBuffer(resource, width, height, stride)`.
///
/// # Safety
/// `resource` is a locked texture.
pub unsafe fn get_resource_buffer(resource: *mut Texture) -> (*mut u8, i32, i32, i32) {
    // SAFETY: the caller holds a lock.
    let t = unsafe { &*resource };
    (t.buf, t.width, t.height, t.stride() as i32)
}

/// `Composite(lockedDst, lockedSrc, ...)`.
///
/// # Safety
/// Both are locked textures; nothing else writes them during the call (SwCompositor's contract).
#[allow(clippy::too_many_arguments)]
pub unsafe fn composite(
    locked_dst: *mut Texture,
    locked_src: *mut Texture,
    src_x: i32,
    src_y: i32,
    src_width: i32,
    src_height: i32,
    dst_x: i32,
    dst_y: i32,
    dst_width: i32,
    dst_height: i32,
    opaque: bool,
    flip_x: bool,
    flip_y: bool,
    filter: u32,
    clip_x: i32,
    clip_y: i32,
    clip_width: i32,
    clip_height: i32,
) {
    if locked_dst.is_null() || locked_src.is_null() || locked_dst == locked_src {
        return;
    }
    let p = prof::begin();
    // SAFETY: distinct locked textures, alive for the call.
    let (dst, src) = unsafe { (&mut *locked_dst, &mut *locked_src) };
    composite::Composite(
        dst, src, src_x, src_y, src_width, src_height, dst_x, dst_y, dst_width, dst_height, opaque, flip_x, flip_y,
        filter, clip_x, clip_y, clip_width, clip_height,
    );
    prof::end(p, "Composite", i32::from(opaque), 1, (dst_width.max(0) as u64) * (dst_height.max(0) as u64));
}

/// `CompositeYUV(lockedDst, lockedY, lockedU, lockedV, colorSpace, colorDepth, ...)` (composite.h): nothing for a null
/// texture or a colour space past `GbrIdentity` (an assert in debug swgl, a return in release).
///
/// # Safety
/// All four are distinct locked textures (or null: nothing is drawn).
#[allow(clippy::too_many_arguments)]
pub unsafe fn composite_yuv(
    locked_dst: *mut Texture,
    locked_y: *mut Texture,
    locked_u: *mut Texture,
    locked_v: *mut Texture,
    color_space: u8,
    color_depth: u32,
    src_x: i32,
    src_y: i32,
    src_width: i32,
    src_height: i32,
    dst_x: i32,
    dst_y: i32,
    dst_width: i32,
    dst_height: i32,
    flip_x: bool,
    flip_y: bool,
    clip_x: i32,
    clip_y: i32,
    clip_width: i32,
    clip_height: i32,
) {
    let p = [locked_dst, locked_y, locked_u, locked_v];
    if p.iter().any(|t| t.is_null()) || (1..4).any(|i| p[..i].contains(&p[i])) {
        return;
    }
    let Some(color_space) = composite::YUVRangedColorSpace::from_u8(color_space) else {
        return;
    };
    // SAFETY: four distinct locked textures, alive for the call.
    unsafe {
        composite::CompositeYUV(
            &mut *p[0], &mut *p[1], &mut *p[2], &mut *p[3], color_space, color_depth, src_x, src_y, src_width,
            src_height, dst_x, dst_y, dst_width, dst_height, flip_x, flip_y, clip_x, clip_y, clip_width, clip_height,
        )
    };
}

/// `ApplyMask(lockedDst, lockedMask)`.
///
/// # Safety
/// Both are distinct locked textures.
pub unsafe fn apply_mask(locked_dst: *mut Texture, locked_mask: *mut Texture) {
    if locked_dst.is_null() || locked_mask.is_null() || locked_dst == locked_mask {
        return;
    }
    // SAFETY: distinct locked textures, alive for the call.
    unsafe { composite::ApplyMask(&mut *locked_dst, &mut *locked_mask) };
}

// ---- the context ------------------------------------------------------------------------------------------------

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    /// `get_binding(name)`.
    pub fn get_binding(&mut self, name: u32) -> &mut u32 {
        match name {
            GL_PIXEL_PACK_BUFFER => &mut self.pixel_pack_buffer_binding,
            GL_PIXEL_UNPACK_BUFFER => &mut self.pixel_unpack_buffer_binding,
            GL_ARRAY_BUFFER => &mut self.array_buffer_binding,
            GL_ELEMENT_ARRAY_BUFFER => {
                &mut self.vertex_arrays.get_mut(self.current_vertex_array).element_array_buffer_binding
            }
            GL_TEXTURE_2D => &mut self.texture_units[self.active_texture_unit].texture_2d_binding,
            GL_TEXTURE_RECTANGLE => &mut self.texture_units[self.active_texture_unit].texture_rectangle_binding,
            GL_TIME_ELAPSED => &mut self.time_elapsed_query,
            GL_SAMPLES_PASSED => &mut self.samples_passed_query,
            GL_RENDERBUFFER => &mut self.renderbuffer_binding,
            GL_DRAW_FRAMEBUFFER => &mut self.draw_framebuffer_binding,
            GL_READ_FRAMEBUFFER => &mut self.read_framebuffer_binding,
            _ => {
                debug_assert!(false, "unknown binding {name:#x}");
                &mut self.unknown_binding
            }
        }
    }

    fn binding(&mut self, name: u32) -> u32 {
        *self.get_binding(name)
    }

    /// `apply_scissor(t)`.
    pub fn apply_scissor(&self, t: &Texture) -> IntRect {
        let bb = t.bounds();
        if self.scissortest {
            bb.intersection(&(self.scissor - t.offset))
        } else {
            bb
        }
    }

    /// `setup_program(program)`.
    pub fn setup_program(&mut self, program: u32) {
        if program == 0 {
            self.vertex_shader = None;
            self.fragment_shader = None;
            return;
        }
        let p = self.programs.get_mut(program);
        debug_assert!(p.impl_.is_some() && p.vert_impl && p.frag_impl);
        let linked = p.impl_.is_some();
        self.vertex_shader = (linked && p.vert_impl).then_some(program);
        self.fragment_shader = (linked && p.frag_impl).then_some(program);
    }

    /// `MakeCurrent(c)` for this context (the caller keeps track of which context is current and skips it when
    /// it already is).
    pub fn make_current(&mut self) {
        self.setup_program(self.current_program);
    }

    pub fn use_program(&mut self, program: u32) {
        if self.current_program != 0
            && program != self.current_program
            && self.programs.find(self.current_program).is_some_and(|p| p.deleted)
        {
            self.programs.take(self.current_program);
        }
        self.current_program = program;
        self.setup_program(program);
    }

    pub fn set_viewport(&mut self, x: i32, y: i32, width: i32, height: i32) {
        self.viewport = IntRect::new(x, y, x.wrapping_add(width), y.wrapping_add(height));
    }

    pub fn enable(&mut self, cap: u32) {
        match cap {
            GL_BLEND => self.blend = true,
            GL_DEPTH_TEST => self.depthtest = true,
            GL_SCISSOR_TEST => self.scissortest = true,
            _ => {}
        }
    }

    pub fn disable(&mut self, cap: u32) {
        match cap {
            GL_BLEND => self.blend = false,
            GL_DEPTH_TEST => self.depthtest = false,
            GL_SCISSOR_TEST => self.scissortest = false,
            _ => {}
        }
    }

    pub fn get_error(&mut self) -> u32 {
        core::mem::replace(&mut self.last_error, GL_NO_ERROR)
    }

    fn out_of_memory(&mut self) {
        self.last_error = GL_OUT_OF_MEMORY;
    }

    /// `GetIntegerv(pname, params)`: the value gl.cc writes to `params[0]`, `None` where it writes nothing.
    pub fn get_integerv(&self, pname: u32) -> Option<i32> {
        Some(match pname {
            GL_MAX_TEXTURE_UNITS | GL_MAX_TEXTURE_IMAGE_UNITS => MAX_TEXTURE_UNITS as i32,
            GL_MAX_TEXTURE_SIZE => 1 << 15,
            GL_MAX_ARRAY_TEXTURE_LAYERS => 0,
            GL_READ_FRAMEBUFFER_BINDING => self.read_framebuffer_binding as i32,
            GL_DRAW_FRAMEBUFFER_BINDING => self.draw_framebuffer_binding as i32,
            GL_PIXEL_PACK_BUFFER_BINDING => self.pixel_pack_buffer_binding as i32,
            GL_PIXEL_UNPACK_BUFFER_BINDING => self.pixel_unpack_buffer_binding as i32,
            GL_NUM_EXTENSIONS => EXTENSIONS.len() as i32,
            GL_MAJOR_VERSION => 3,
            GL_MINOR_VERSION => 2,
            GL_MIN_PROGRAM_TEXEL_OFFSET => 0,
            GL_MAX_PROGRAM_TEXEL_OFFSET => MAX_TEXEL_OFFSET,
            _ => {
                // As swgl: debug gl.cc stops here too (`assert(false)`, swgl/src/gl.cc:1225), release writes nothing.
                // The oracle tests probe an unhandled pname only in release (tests/oracle_gl.rs).
                debug_assert!(false, "unhandled glGetIntegerv parameter {pname:#x}");
                return None;
            }
        })
    }

    /// `GetBooleanv(pname, params)`.
    pub fn get_booleanv(&self, pname: u32) -> Option<u8> {
        match pname {
            GL_DEPTH_WRITEMASK => Some(self.depthmask as u8),
            _ => {
                debug_assert!(false, "unhandled glGetBooleanv parameter {pname:#x}");
                None
            }
        }
    }

    /// `GetString(name)`.
    pub fn get_string(name: u32) -> Option<&'static CStr> {
        match name {
            GL_VENDOR => Some(c"Mozilla Gfx"),
            GL_RENDERER => Some(c"Software WebRender"),
            GL_VERSION => Some(c"3.2"),
            GL_SHADING_LANGUAGE_VERSION => Some(c"1.50"),
            _ => None,
        }
    }

    /// `GetStringi(name, index)`.
    pub fn get_stringi(name: u32, index: u32) -> Option<&'static CStr> {
        match name {
            GL_EXTENSIONS => EXTENSIONS.get(index as usize).copied(),
            _ => None,
        }
    }

    /// `hash_blend_key()`. swgl hashes the four enums and the equation into one word whose fields overlap, then
    /// switches on it; this looks the tuple up in `rules::BLEND_TABLE` instead (Verus-proved). The two agree on
    /// every state except the 22 aliased ones `raster/rules/tests/blend_hash.rs` lists, where swgl's hash happens to
    /// equal a table entry's (or misses one) and the tuple comparison gives the honest answer. A state outside the
    /// table falls back to premultiplied source-over, as LucinaNet's swgl does (patches/swgl), with its message.
    fn hash_blend_key(&mut self) {
        let (srgb, drgb, sa, da, eq) =
            (self.blendfunc_srgb, self.blendfunc_drgb, self.blendfunc_sa, self.blendfunc_da, self.blend_equation);
        if rules::blend_key(srgb, drgb, sa, da, eq).is_none() {
            eprintln!("swgl: unknown blend {srgb:x}, {drgb:x}, separate: {sa:x}, {da:x}, equation: {eq:x}");
        }
        self.blend_key = rules::blend_key_or_fallback(srgb, drgb, sa, da, eq) as BlendKey;
    }

    pub fn blend_func(&mut self, srgb: u32, drgb: u32, sa: u32, da: u32) {
        self.blendfunc_srgb = srgb;
        self.blendfunc_drgb = drgb;
        self.blendfunc_sa = remap_blendfunc(srgb, sa);
        self.blendfunc_da = remap_blendfunc(drgb, da);
        self.hash_blend_key();
    }

    pub fn blend_color(&mut self, r: f32, g: f32, b: f32, a: f32) {
        let c = round_pixel(Float::new([b, g, r, a]), 255.0);
        let c16 = c.convert::<u16>();
        self.blendcolor = V8::new([c16.0[0], c16.0[1], c16.0[2], c16.0[3], c16.0[0], c16.0[1], c16.0[2], c16.0[3]]);
    }

    pub fn blend_equation(&mut self, mode: u32) {
        debug_assert!(
            mode == GL_FUNC_ADD || mode == GL_MIN || mode == GL_MAX || (GL_MULTIPLY_KHR..=GL_HSL_LUMINOSITY_KHR).contains(&mode)
        );
        if mode != self.blend_equation {
            self.blend_equation = mode;
            self.hash_blend_key();
        }
    }

    pub fn depth_mask(&mut self, flag: u8) {
        self.depthmask = flag != 0;
    }

    pub fn depth_func(&mut self, func: u32) {
        debug_assert!(func == GL_LESS || func == GL_LEQUAL);
        self.depthfunc = func;
    }

    pub fn set_scissor(&mut self, x: i32, y: i32, width: i32, height: i32) {
        self.scissor = IntRect::new(x, y, x.wrapping_add(width), y.wrapping_add(height));
    }

    pub fn clear_color(&mut self, r: f32, g: f32, b: f32, a: f32) {
        self.clearcolor = [r, g, b, a];
    }

    pub fn clear_depth(&mut self, depth: f64) {
        self.cleardepth = depth;
    }

    pub fn active_texture(&mut self, texture: u32) {
        debug_assert!(texture >= GL_TEXTURE0 && texture < GL_TEXTURE0 + MAX_TEXTURE_UNITS as u32);
        self.active_texture_unit = (texture.wrapping_sub(GL_TEXTURE0) as i32).clamp(0, MAX_TEXTURE_UNITS as i32 - 1) as usize;
    }

    pub fn gen_queries(&mut self, result: &mut [u32]) {
        for r in result {
            *r = self.queries.insert(Query::default());
        }
    }

    pub fn delete_query(&mut self, n: u32) {
        if n != 0 && self.queries.take(n).is_some() {
            unlink(&mut self.time_elapsed_query, n);
            unlink(&mut self.samples_passed_query, n);
        }
    }

    pub fn gen_buffers(&mut self, result: &mut [u32]) {
        for r in result {
            *r = self.buffers.insert(Buffer::default());
        }
    }

    pub fn delete_buffer(&mut self, n: u32) {
        if n != 0 && self.buffers.take(n).is_some() {
            unlink(&mut self.pixel_pack_buffer_binding, n);
            unlink(&mut self.pixel_unpack_buffer_binding, n);
            unlink(&mut self.array_buffer_binding, n);
        }
    }

    pub fn gen_vertex_arrays(&mut self, result: &mut [u32]) {
        for r in result {
            *r = self.vertex_arrays.insert(VertexArray::default());
        }
    }

    pub fn delete_vertex_array(&mut self, n: u32) {
        if n != 0 && self.vertex_arrays.take(n).is_some() {
            unlink(&mut self.current_vertex_array, n);
        }
    }

    pub fn create_shader(&mut self, type_: u32) -> u32 {
        self.shaders.insert(Shader { type_, loader: None })
    }

    pub fn shader_source_by_name(&mut self, shader: u32, name: &str) {
        let loader = load_shader(name);
        if loader.is_none() {
            eprintln!("swgl: unknown shader {name}");
        }
        self.shaders.get_mut(shader).loader = loader;
    }

    pub fn attach_shader(&mut self, program: u32, shader: u32) {
        self.programs.get_mut(program);
        let s = self.shaders.get_mut(shader);
        let (type_, made) = (s.type_, s.loader.as_ref().map(|l| l.box_clone()));
        let p = self.programs.get_mut(program);
        match type_ {
            GL_VERTEX_SHADER | GL_FRAGMENT_SHADER => {
                if p.impl_.is_none() {
                    p.impl_ = made;
                }
            }
            _ => debug_assert!(false, "shader type {type_:#x}"),
        }
    }

    pub fn delete_shader(&mut self, n: u32) {
        if n != 0 {
            self.shaders.take(n);
        }
    }

    pub fn create_program(&mut self) -> u32 {
        self.programs.insert(Program::default())
    }

    pub fn delete_program(&mut self, n: u32) {
        if n == 0 {
            return;
        }
        if self.current_program == n {
            if let Some(p) = self.programs.find_mut(n) {
                p.deleted = true;
            }
        } else {
            self.programs.take(n);
        }
    }

    pub fn link_program(&mut self, program: u32) {
        let p = self.programs.get_mut(program);
        let Some(imp) = p.impl_.as_ref() else {
            debug_assert!(false, "LinkProgram without a shader");
            return;
        };
        debug_assert!(imp.interpolants_size() <= core::mem::size_of::<rasterize::Interpolants>());
        p.vert_impl = true;
        p.frag_impl = true;
    }

    pub fn get_link_status(&self, program: u32) -> i32 {
        self.programs.find(program).map_or(0, |p| p.impl_.is_some() as i32)
    }

    pub fn bind_attrib_location(&mut self, program: u32, index: u32, name: &str) {
        if let Some(imp) = self.programs.get_mut(program).impl_.as_mut() {
            imp.bind_attrib(name, index as i32);
        }
    }

    pub fn get_attrib_location(&mut self, program: u32, name: &str) -> i32 {
        self.programs.get_mut(program).impl_.as_ref().map_or(-1, |imp| imp.get_attrib(name))
    }

    pub fn get_uniform_location(&mut self, program: u32, name: &str) -> i32 {
        self.programs.get_mut(program).impl_.as_ref().map_or(-1, |imp| imp.get_uniform(name))
    }

    pub fn begin_query(&mut self, target: u32, id: u32) {
        *self.get_binding(target) = id;
        let q = self.queries.get_mut(id);
        match target {
            GL_SAMPLES_PASSED => q.value = 0,
            GL_TIME_ELAPSED => q.value = get_time_value(),
            _ => debug_assert!(false, "unknown query target {target:#x}"),
        }
    }

    pub fn end_query(&mut self, target: u32) {
        let id = self.binding(target);
        let q = self.queries.get_mut(id);
        match target {
            GL_SAMPLES_PASSED => {}
            GL_TIME_ELAPSED => q.value = get_time_value().wrapping_sub(q.value),
            _ => debug_assert!(false, "unknown query target {target:#x}"),
        }
        *self.get_binding(target) = 0;
    }

    /// `GetQueryObjectui64v(id, pname, params)`: the value written to `params[0]`.
    pub fn get_query_objectui64v(&mut self, id: u32, pname: u32) -> Option<u64> {
        let q = self.queries.get_mut(id);
        match pname {
            GL_QUERY_RESULT => Some(q.value),
            _ => None,
        }
    }

    pub fn bind_vertex_array(&mut self, vertex_array: u32) {
        if vertex_array != self.current_vertex_array {
            self.validate_vertex_array = true;
        }
        self.current_vertex_array = vertex_array;
    }

    pub fn bind_texture(&mut self, target: u32, texture: u32) {
        *self.get_binding(target) = texture;
    }

    pub fn bind_buffer(&mut self, target: u32, buffer: u32) {
        *self.get_binding(target) = buffer;
    }

    pub fn bind_framebuffer(&mut self, target: u32, fb: u32) {
        if target == GL_FRAMEBUFFER {
            self.read_framebuffer_binding = fb;
            self.draw_framebuffer_binding = fb;
        } else {
            debug_assert!(target == GL_READ_FRAMEBUFFER || target == GL_DRAW_FRAMEBUFFER);
            *self.get_binding(target) = fb;
        }
    }

    pub fn bind_renderbuffer(&mut self, target: u32, rb: u32) {
        *self.get_binding(target) = rb;
    }

    pub fn pixel_storei(&mut self, name: u32, param: i32) {
        if name == GL_UNPACK_ALIGNMENT {
            debug_assert!(param == 1);
        } else if name == GL_UNPACK_ROW_LENGTH {
            self.unpack_row_length = param;
        }
    }

    pub fn tex_storage_2d(&mut self, target: u32, levels: i32, internal_format: u32, width: i32, height: i32) {
        debug_assert!(levels == 1);
        let id = self.binding(target);
        let t = self.textures.get_mut(id);
        // SAFETY: no external buffer.
        if !unsafe { set_tex_storage(t, internal_format, width, height, core::ptr::null_mut(), 0, 0, 0) } {
            self.out_of_memory();
        }
    }

    /// `TexSubImage2D`. With a pixel unpack buffer bound, `data` is an offset into it and the copy is clipped to it;
    /// otherwise `data` points at the caller's pixels. The destination is clipped to the texture (`convert_copy`).
    ///
    /// # Safety
    /// Without an unpack buffer, a non-null `data` must hold `height` rows of `GL_UNPACK_ROW_LENGTH` (or `width`)
    /// pixels.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn tex_sub_image_2d(
        &mut self,
        target: u32,
        level: i32,
        xoffset: i32,
        yoffset: i32,
        width: i32,
        height: i32,
        format: u32,
        ty: u32,
        data: *const c_void,
    ) {
        let p = prof::begin();
        // SAFETY: the caller's contract, passed on.
        unsafe { self.tex_sub_image_2d_unprofiled(target, level, xoffset, yoffset, width, height, format, ty, data) };
        prof::end(p, "TexSubImage2D", 0, 1, (width.max(0) as u64) * (height.max(0) as u64));
    }

    /// # Safety
    /// As `tex_sub_image_2d`.
    #[allow(clippy::too_many_arguments)]
    unsafe fn tex_sub_image_2d_unprofiled(
        &mut self,
        target: u32,
        level: i32,
        xoffset: i32,
        yoffset: i32,
        width: i32,
        height: i32,
        format: u32,
        ty: u32,
        data: *const c_void,
    ) {
        if level != 0 {
            debug_assert!(false);
            return;
        }
        let pbo_id = self.pixel_unpack_buffer_binding;
        let src = if pbo_id != 0 {
            let pbo = self.buffers.get_mut(pbo_id);
            let Some(off) = pbo.get_data(data as usize) else { return };
            Cursor { base: pbo.buf(), off: off as i64, bounds: Some((0, pbo.size as i64)) }
        } else {
            if data.is_null() {
                return;
            }
            Cursor { base: data as *mut u8, off: 0, bounds: None }
        };
        let row_length = if self.unpack_row_length != 0 { self.unpack_row_length } else { width };
        let id = self.binding(target);
        let t = self.textures.get_mut(id);
        let skip = IntRect::new(xoffset, yoffset, xoffset.wrapping_add(width), yoffset.wrapping_add(height));
        prepare_texture(t, Some(&skip));
        debug_assert!(t.internal_format == internal_format_for_data(format, ty));
        let src_bpp =
            if format_requires_conversion(format, t.internal_format) { bytes_for_internal_format(format) } else { t.bpp() };
        if src_bpp == 0 || t.buf.is_null() {
            return;
        }
        let dst = texture_cursor(t, xoffset, yoffset);
        // SAFETY: both sides are clipped to their buffers, except caller memory, which the caller vouches for.
        unsafe {
            convert_copy(
                format,
                t.internal_format,
                dst,
                t.stride() as i64,
                src,
                i64::from(row_length) * i64::from(src_bpp),
                width.max(0) as usize,
                height.max(0) as usize,
            )
        };
    }

    /// `TexImage2D`: storage, then the upload.
    ///
    /// # Safety
    /// As [`Context::tex_sub_image_2d`].
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn tex_image_2d(
        &mut self,
        target: u32,
        level: i32,
        internal_format: i32,
        width: i32,
        height: i32,
        border: i32,
        format: u32,
        ty: u32,
        data: *const c_void,
    ) {
        if level != 0 {
            debug_assert!(false);
            return;
        }
        debug_assert!(border == 0);
        self.tex_storage_2d(target, 1, internal_format as u32, width, height);
        // SAFETY: the caller's contract.
        unsafe { self.tex_sub_image_2d(target, 0, 0, 0, width, height, format, ty, data) };
    }

    pub fn generate_mipmap(&mut self, _target: u32) {}

    pub fn set_texture_parameter(&mut self, texid: u32, pname: u32, param: i32) {
        let t = self.textures.get_mut(texid);
        match pname {
            GL_TEXTURE_WRAP_S | GL_TEXTURE_WRAP_T => debug_assert!(param as u32 == GL_CLAMP_TO_EDGE),
            GL_TEXTURE_MIN_FILTER => t.min_filter = param as u32,
            GL_TEXTURE_MAG_FILTER => t.mag_filter = param as u32,
            _ => {}
        }
    }

    pub fn tex_parameteri(&mut self, target: u32, pname: u32, param: i32) {
        let id = self.binding(target);
        self.set_texture_parameter(id, pname, param);
    }

    /// `LockTexture(texId)`: null if the texture has no buffer. The first lock applies its delayed clear.
    pub fn lock_texture(&mut self, tex_id: u32) -> *mut Texture {
        let tex = self.textures.get_mut(tex_id);
        if tex.buf.is_null() {
            return core::ptr::null_mut();
        }
        if tex.locked.fetch_add(1, Ordering::AcqRel) == 0 {
            prepare_texture(tex, None);
        }
        tex as *mut Texture
    }

    /// `LockFramebuffer(fboId)`: its color attachment, locked.
    pub fn lock_framebuffer(&mut self, fbo_id: u32) -> *mut Texture {
        let color = self.framebuffers.get_mut(fbo_id).color_attachment;
        if color == 0 {
            return core::ptr::null_mut();
        }
        self.lock_texture(color)
    }

    pub fn gen_textures(&mut self, result: &mut [u32]) {
        for r in result {
            *r = self.textures.insert(Texture::default());
        }
    }

    fn gen_texture(&mut self) -> u32 {
        let mut id = [0];
        self.gen_textures(&mut id);
        id[0]
    }

    /// `DeleteTexture(n)`: forgets the texture and frees it, or, if another thread holds a lock on it, leaves it
    /// alive as a zombie that the last `UnlockResource` frees.
    pub fn delete_texture(&mut self, n: u32) {
        if n == 0 {
            return;
        }
        let Some(tex) = self.textures.take(n) else { return };
        let tex = Box::into_raw(tex);
        // SAFETY: `tex` came from `Box::into_raw` just above; locking it keeps it alive across the unlock below.
        unsafe { lock_resource(tex) };
        for unit in self.texture_units.iter_mut() {
            unit.unlink(n);
        }
        // SAFETY: as above; the zombie is freed by whichever unlock is last, this one if nobody else holds a lock.
        unsafe {
            (*tex).set_zombie(true);
            unlock_resource(tex);
        }
    }

    pub fn gen_renderbuffers(&mut self, result: &mut [u32]) {
        for r in result {
            *r = self.renderbuffers.insert(Renderbuffer::default());
        }
    }

    /// `DeleteRenderbuffer(n)`, with `Renderbuffer::on_erase`: unlinks its texture from every framebuffer, deletes it.
    pub fn delete_renderbuffer(&mut self, n: u32) {
        if n == 0 {
            return;
        }
        let Some(rb) = self.renderbuffers.take(n) else { return };
        for fb in self.framebuffers.iter_mut() {
            unlink(&mut fb.color_attachment, rb.texture);
            unlink(&mut fb.depth_attachment, rb.texture);
        }
        self.delete_texture(rb.texture);
        unlink(&mut self.renderbuffer_binding, n);
    }

    pub fn gen_framebuffers(&mut self, result: &mut [u32]) {
        for r in result {
            *r = self.framebuffers.insert(Framebuffer::default());
        }
    }

    pub fn delete_framebuffer(&mut self, n: u32) {
        if n != 0 && self.framebuffers.take(n).is_some() {
            unlink(&mut self.read_framebuffer_binding, n);
            unlink(&mut self.draw_framebuffer_binding, n);
        }
    }

    pub fn renderbuffer_storage(&mut self, target: u32, internal_format: u32, width: i32, height: i32) {
        let id = self.binding(target);
        if self.renderbuffers.get_mut(id).texture == 0 {
            let t = self.gen_texture();
            self.renderbuffers.get_mut(id).texture = t;
        }
        let internal_format = match internal_format {
            GL_DEPTH_COMPONENT | GL_DEPTH_COMPONENT16 | GL_DEPTH_COMPONENT24 | GL_DEPTH_COMPONENT32 => GL_DEPTH_COMPONENT24,
            f => f,
        };
        let tid = self.renderbuffers.get_mut(id).texture;
        let t = self.textures.get_mut(tid);
        // SAFETY: no external buffer.
        if !unsafe { set_tex_storage(t, internal_format, width, height, core::ptr::null_mut(), 0, 0, 0) } {
            self.out_of_memory();
        }
    }

    fn set_attrib(&mut self, index: u32, size: i32, type_: u32, normalized: bool, stride: i32, offset: usize) {
        let (array_buffer, vao) = (self.array_buffer_binding, self.current_vertex_array);
        let v = self.vertex_arrays.get_mut(vao);
        if index as usize >= NULL_ATTRIB {
            debug_assert!(false);
            return;
        }
        let va = &mut v.attribs[index as usize];
        va.size = (size.wrapping_mul(bytes_per_type(type_))) as usize;
        va.type_ = type_;
        va.normalized = normalized;
        va.stride = stride;
        va.offset = offset as u32;
        va.vertex_buffer = array_buffer as i32;
        va.vertex_array = vao as i32;
        self.validate_vertex_array = true;
    }

    pub fn vertex_attrib_pointer(&mut self, index: u32, size: i32, type_: u32, normalized: bool, stride: i32, offset: usize) {
        self.set_attrib(index, size, type_, normalized, stride, offset);
    }

    pub fn vertex_attrib_i_pointer(&mut self, index: u32, size: i32, type_: u32, stride: i32, offset: usize) {
        self.set_attrib(index, size, type_, false, stride, offset);
    }

    pub fn enable_vertex_attrib_array(&mut self, index: u32) {
        let v = self.vertex_arrays.get_mut(self.current_vertex_array);
        if index as usize >= NULL_ATTRIB {
            debug_assert!(false);
            return;
        }
        let va = &mut v.attribs[index as usize];
        let was = va.enabled;
        va.enabled = true;
        v.max_attrib = v.max_attrib.max(index as i32);
        if !was {
            self.validate_vertex_array = true;
        }
    }

    pub fn disable_vertex_attrib_array(&mut self, index: u32) {
        let v = self.vertex_arrays.get_mut(self.current_vertex_array);
        if index as usize >= NULL_ATTRIB {
            debug_assert!(false);
            return;
        }
        let va = &mut v.attribs[index as usize];
        let was = va.enabled;
        va.disable();
        if was {
            self.validate_vertex_array = true;
        }
    }

    pub fn vertex_attrib_divisor(&mut self, index: u32, divisor: u32) {
        let v = self.vertex_arrays.get_mut(self.current_vertex_array);
        if index as usize >= NULL_ATTRIB || divisor > 1 {
            debug_assert!(false);
            return;
        }
        v.attribs[index as usize].divisor = divisor;
    }

    /// `BufferData`.
    ///
    /// # Safety
    /// A non-null `data` holds `size` bytes.
    pub unsafe fn buffer_data(&mut self, target: u32, size: isize, data: *const c_void, _usage: u32) {
        let id = self.binding(target);
        let size = size as usize;
        let b = self.buffers.get_mut(id);
        let mut oom = false;
        if size != b.size {
            oom = !b.allocate(size);
            self.validate_vertex_array = true;
        }
        let b = self.buffers.get_mut(id);
        if !data.is_null() && b.has_buf() && size <= b.size {
            // SAFETY: the caller guarantees `size` bytes at `data`; the buffer holds at least `size`.
            unsafe { core::ptr::copy_nonoverlapping(data as *const u8, b.buf(), size) };
        }
        if oom {
            self.out_of_memory();
        }
    }

    /// `BufferSubData`, clipped as gl.cc does.
    ///
    /// # Safety
    /// A non-null `data` holds `size` bytes.
    pub unsafe fn buffer_sub_data(&mut self, target: u32, offset: isize, size: isize, data: *const c_void) {
        if offset < 0 {
            debug_assert!(false);
            return;
        }
        let id = self.binding(target);
        let b = self.buffers.get_mut(id);
        let (off, size) = (offset as usize, size as usize);
        if !data.is_null() && b.has_buf() && off < b.size && size <= b.size - off {
            // SAFETY: `[off, off + size)` is inside the buffer; the caller guarantees `size` bytes at `data`.
            unsafe { core::ptr::copy_nonoverlapping(data as *const u8, b.buf().add(off), size) };
        }
    }

    pub fn map_buffer(&mut self, target: u32, _access: u32) -> *mut c_void {
        let id = self.binding(target);
        self.buffers.get_mut(id).buf() as *mut c_void
    }

    pub fn map_buffer_range(&mut self, target: u32, offset: isize, length: isize, _access: u32) -> *mut c_void {
        let id = self.binding(target);
        let b = self.buffers.get_mut(id);
        if !b.has_buf() || offset < 0 || length == 0 {
            return core::ptr::null_mut();
        }
        let off = offset as usize;
        if off >= b.size || length as usize > b.size - off {
            return core::ptr::null_mut();
        }
        b.buf().wrapping_add(off) as *mut c_void
    }

    pub fn unmap_buffer(&mut self, target: u32) -> u8 {
        let id = self.binding(target);
        self.buffers.get_mut(id).has_buf() as u8
    }

    fn vertex_shader_mut(&mut self) -> Option<&mut (dyn ProgramImpl + 'static)> {
        let id = self.vertex_shader?;
        self.programs.find_mut(id)?.impl_.as_deref_mut()
    }

    pub fn uniform_1i(&mut self, location: i32, v0: i32) {
        if let Some(p) = self.vertex_shader_mut() {
            p.get_vertex_shader().set_uniform_1i(location, v0);
        }
    }

    /// `Uniform4fv` (`count` is 1 in WebRender; gl.cc asserts it).
    pub fn uniform_4fv(&mut self, location: i32, count: i32, v: &[f32]) {
        debug_assert!(count == 1);
        if let Some(p) = self.vertex_shader_mut() {
            p.get_vertex_shader().set_uniform_4fv(location, v);
        }
    }

    pub fn uniform_matrix4fv(&mut self, location: i32, count: i32, transpose: u8, value: &[f32]) {
        debug_assert!(count == 1 && transpose == 0);
        if let Some(p) = self.vertex_shader_mut() {
            p.get_vertex_shader().set_uniform_matrix4fv(location, value);
        }
    }

    pub fn framebuffer_texture_2d(&mut self, target: u32, attachment: u32, textarget: u32, texture: u32, level: i32) {
        debug_assert!(target == GL_READ_FRAMEBUFFER || target == GL_DRAW_FRAMEBUFFER);
        debug_assert!(textarget == GL_TEXTURE_2D || textarget == GL_TEXTURE_RECTANGLE);
        debug_assert!(level == 0);
        let id = self.binding(target);
        let fb = self.framebuffers.get_mut(id);
        match attachment {
            GL_COLOR_ATTACHMENT0 => fb.color_attachment = texture,
            GL_DEPTH_ATTACHMENT => fb.depth_attachment = texture,
            _ => debug_assert!(false),
        }
    }

    pub fn framebuffer_renderbuffer(&mut self, target: u32, attachment: u32, renderbuffertarget: u32, renderbuffer: u32) {
        debug_assert!(target == GL_READ_FRAMEBUFFER || target == GL_DRAW_FRAMEBUFFER);
        debug_assert!(renderbuffertarget == GL_RENDERBUFFER);
        let id = self.binding(target);
        self.framebuffers.get_mut(id);
        let tex = self.renderbuffers.get_mut(renderbuffer).texture;
        let fb = self.framebuffers.get_mut(id);
        match attachment {
            GL_COLOR_ATTACHMENT0 => fb.color_attachment = tex,
            GL_DEPTH_ATTACHMENT => fb.depth_attachment = tex,
            _ => debug_assert!(false),
        }
    }

    /// `get_framebuffer(target, fallback)`: the bound framebuffer's id, if it exists (or framebuffer 0, created,
    /// with `fallback`).
    fn get_framebuffer(&mut self, target: u32, fallback: bool) -> Option<u32> {
        let target = if target == GL_FRAMEBUFFER { GL_DRAW_FRAMEBUFFER } else { target };
        let id = self.binding(target);
        if self.framebuffers.find(id).is_some() {
            return Some(id);
        }
        if fallback {
            self.framebuffers.get_mut(0);
            return Some(0);
        }
        None
    }

    /// `InitDefaultFramebuffer(x, y, width, height, stride, buf)`.
    ///
    /// # Safety
    /// A non-null `buf` holds `height` rows of `stride` bytes and stays valid while it is the default color buffer.
    pub unsafe fn init_default_framebuffer(&mut self, x: i32, y: i32, width: i32, height: i32, stride: i32, buf: *mut c_void) {
        if self.framebuffers.get_mut(0).color_attachment == 0 {
            let t = self.gen_texture();
            self.framebuffers.get_mut(0).color_attachment = t;
        }
        let color = self.framebuffers.get_mut(0).color_attachment;
        let colortex = self.textures.get_mut(color);
        // SAFETY: the caller's contract.
        let ok = unsafe { set_tex_storage(colortex, GL_RGBA8, width, height, buf as *mut u8, stride, 0, 0) };
        colortex.offset = IntPoint { x, y };
        if self.framebuffers.get_mut(0).depth_attachment == 0 {
            let t = self.gen_texture();
            self.framebuffers.get_mut(0).depth_attachment = t;
        }
        let depth = self.framebuffers.get_mut(0).depth_attachment;
        let depthtex = self.textures.get_mut(depth);
        // SAFETY: no external buffer.
        let ok2 = unsafe { set_tex_storage(depthtex, GL_DEPTH_COMPONENT24, width, height, core::ptr::null_mut(), 0, 0, 0) };
        depthtex.offset = IntPoint { x, y };
        if !ok || !ok2 {
            self.out_of_memory();
        }
    }

    /// `GetColorBuffer(fbo, flush, width, height, stride)`: the pointer, then the dimensions (`None`: no buffer and
    /// nothing written).
    pub fn get_color_buffer(&mut self, fbo: u32, flush: bool) -> Option<(*mut u8, i32, i32, i32)> {
        let color = self.framebuffers.find(fbo)?.color_attachment;
        if color == 0 {
            return None;
        }
        let colortex = self.textures.get_mut(color);
        if flush {
            prepare_texture(colortex, None);
        }
        let p = if colortex.buf.is_null() { core::ptr::null_mut() } else { colortex.sample_ptr(0, 0) };
        Some((p, colortex.width, colortex.height, colortex.stride() as i32))
    }

    pub fn resolve_framebuffer(&mut self, fbo: u32) {
        let Some(fb) = self.framebuffers.find(fbo) else { return };
        let color = fb.color_attachment;
        if color != 0 {
            prepare_texture(self.textures.get_mut(color), None);
        }
    }

    /// `SetTextureBuffer(texid, internal_format, width, height, stride, buf, min_width, min_height)`.
    ///
    /// # Safety
    /// As [`Context::init_default_framebuffer`].
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn set_texture_buffer(
        &mut self,
        texid: u32,
        internal_format: u32,
        width: i32,
        height: i32,
        stride: i32,
        buf: *mut c_void,
        min_width: i32,
        min_height: i32,
    ) {
        let t = self.textures.get_mut(texid);
        // SAFETY: the caller's contract.
        if !unsafe { set_tex_storage(t, internal_format, width, height, buf as *mut u8, stride, min_width, min_height) } {
            self.out_of_memory();
        }
    }

    pub fn check_framebuffer_status(&mut self, target: u32) -> u32 {
        match self.get_framebuffer(target, false).and_then(|id| self.framebuffers.find(id)) {
            Some(fb) if fb.color_attachment != 0 => GL_FRAMEBUFFER_COMPLETE,
            _ => GL_FRAMEBUFFER_UNSUPPORTED,
        }
    }

    /// `ClearTexSubImage(texture, level, xoffset, yoffset, zoffset, width, height, depth, format, type, data)`.
    #[allow(clippy::too_many_arguments)]
    pub fn clear_tex_sub_image(
        &mut self,
        texture: u32,
        level: i32,
        xoffset: i32,
        yoffset: i32,
        zoffset: i32,
        width: i32,
        height: i32,
        depth: i32,
        format: u32,
        data: ClearData,
    ) {
        if level != 0 {
            debug_assert!(false);
            return;
        }
        let t = self.textures.get_mut(texture);
        debug_assert!(t.locked.load(Ordering::Relaxed) == 0);
        if width <= 0 || height <= 0 || depth <= 0 {
            return;
        }
        debug_assert!(zoffset == 0 && depth == 1);
        let scissor = IntRect::new(xoffset, yoffset, xoffset.wrapping_add(width), yoffset.wrapping_add(height));
        if t.internal_format == GL_DEPTH_COMPONENT24 {
            let mut value = 0xFF_FFFFu32;
            if format == GL_DEPTH_COMPONENT {
                match data {
                    ClearData::Double(d) => value = to_u32(d * f64::from(0xFF_FFFF)),
                    ClearData::Float(f) => value = to_u32(f64::from(f[0] * 0xFF_FFFF as f32)),
                    _ => debug_assert!(false),
                }
            } else {
                debug_assert!(false);
            }
            if t.cleared() && !scissor.contains(&t.offset_bounds()) {
                t.fill_depth_runs(value, &scissor);
            } else {
                t.init_depth_runs(value);
            }
            return;
        }
        let mut color = 0xFF00_0000u32;
        match data {
            ClearData::Float(f) => {
                let mut v = [0.0f32, 0.0, 0.0, 1.0];
                match format {
                    GL_RGBA => v = f,
                    GL_RGB => v[..3].copy_from_slice(&f[..3]),
                    GL_RG => v[..2].copy_from_slice(&f[..2]),
                    GL_RED => v[0] = f[0],
                    _ => debug_assert!(false),
                }
                let c = round_pixel(Float::new(v), 255.0).convert::<u8>();
                color = bit_cast::<u32, _>(c);
            }
            ClearData::Bytes(b) => {
                let n = match format {
                    GL_RGBA => 4,
                    GL_RGB => 3,
                    GL_RG => 2,
                    GL_RED => 1,
                    _ => {
                        debug_assert!(false);
                        0
                    }
                };
                for (i, byte) in b.iter().enumerate().take(n) {
                    color = (color & !(0xFF << (8 * i))) | (u32::from(*byte) << (8 * i));
                }
            }
            _ => debug_assert!(false),
        }
        match t.internal_format {
            GL_RGBA8 => {
                let bgra = (color & 0xFF00_FF00) | ((color << 16) & 0xFF_0000) | ((color >> 16) & 0xFF);
                request_clear::<u32>(t, bgra, &scissor);
            }
            GL_R8 => request_clear::<u8>(t, color as u8, &scissor),
            GL_RG8 => request_clear::<u16>(t, color as u16, &scissor),
            _ => debug_assert!(false),
        }
    }

    /// `ClearTexImage(texture, level, format, type, data)`.
    pub fn clear_tex_image(&mut self, texture: u32, level: i32, format: u32, data: ClearData) {
        let scissor = self.textures.get_mut(texture).offset_bounds();
        self.clear_tex_sub_image(texture, level, scissor.x0, scissor.y0, 0, scissor.width(), scissor.height(), 1, format, data);
    }

    pub fn clear(&mut self, mask: u32) {
        let p = prof::begin();
        self.clear_unprofiled(mask);
        prof::end(p, "Clear", 0, 1, 0);
    }

    fn clear_unprofiled(&mut self, mask: u32) {
        let Some(fbid) = self.get_framebuffer(GL_DRAW_FRAMEBUFFER, true) else { return };
        let fb = *self.framebuffers.get_mut(fbid);
        if mask & GL_COLOR_BUFFER_BIT != 0 && fb.color_attachment != 0 {
            let ob = self.textures.get_mut(fb.color_attachment).offset_bounds();
            let scissor = if self.scissortest { self.scissor.intersection(&ob) } else { ob };
            let c = ClearData::Float(self.clearcolor);
            self.clear_tex_sub_image(fb.color_attachment, 0, scissor.x0, scissor.y0, 0, scissor.width(), scissor.height(), 1, GL_RGBA, c);
        }
        if mask & GL_DEPTH_BUFFER_BIT != 0 && fb.depth_attachment != 0 {
            let ob = self.textures.get_mut(fb.depth_attachment).offset_bounds();
            let scissor = if self.scissortest { self.scissor.intersection(&ob) } else { ob };
            let d = ClearData::Double(self.cleardepth);
            self.clear_tex_sub_image(fb.depth_attachment, 0, scissor.x0, scissor.y0, 0, scissor.width(), scissor.height(), 1, GL_DEPTH_COMPONENT, d);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn clear_color_rect(&mut self, fbo: u32, xoffset: i32, yoffset: i32, width: i32, height: i32, r: f32, g: f32, b: f32, a: f32) {
        let p = prof::begin();
        let color = self.framebuffers.get_mut(fbo).color_attachment;
        let ob = self.textures.get_mut(color).offset_bounds();
        let scissor = IntRect::new(xoffset, yoffset, xoffset.wrapping_add(width), yoffset.wrapping_add(height)).intersection(&ob);
        self.clear_tex_sub_image(color, 0, scissor.x0, scissor.y0, 0, scissor.width(), scissor.height(), 1, GL_RGBA, ClearData::Float([r, g, b, a]));
        prof::end(p, "ClearColorRect", 0, 1, (width.max(0) as u64) * (height.max(0) as u64));
    }

    pub fn invalidate_framebuffer(&mut self, target: u32, attachments: &[u32]) {
        let Some(id) = self.get_framebuffer(target, false) else { return };
        if attachments.is_empty() {
            return;
        }
        for &a in attachments {
            let fb = *self.framebuffers.get_mut(id);
            match a {
                GL_DEPTH_ATTACHMENT => self.textures.get_mut(fb.depth_attachment).set_cleared(false),
                GL_COLOR_ATTACHMENT0 => self.textures.get_mut(fb.color_attachment).disable_delayed_clear(),
                _ => {}
            }
        }
    }

    /// `ReadPixels`. With a pixel pack buffer bound, `data` is an offset into it and the copy is clipped to it.
    ///
    /// # Safety
    /// Without a pack buffer, `data` is null or holds `height` rows of `width` pixels.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn read_pixels(&mut self, mut x: i32, mut y: i32, mut width: i32, mut height: i32, format: u32, ty: u32, data: *mut c_void) {
        let pbo_id = self.pixel_pack_buffer_binding;
        let dest_base = if pbo_id != 0 {
            let pbo = self.buffers.get_mut(pbo_id);
            let Some(off) = pbo.get_data(data as usize) else { return };
            Cursor { base: pbo.buf(), off: off as i64, bounds: Some((0, pbo.size as i64)) }
        } else {
            if data.is_null() {
                return;
            }
            Cursor { base: data as *mut u8, off: 0, bounds: None }
        };
        let Some(fbid) = self.get_framebuffer(GL_READ_FRAMEBUFFER, false) else { return };
        let color = self.framebuffers.get_mut(fbid).color_attachment;
        let t = self.textures.get_mut(color);
        if t.buf.is_null() {
            return;
        }
        prepare_texture(t, None);
        x = x.wrapping_sub(t.offset.x);
        y = y.wrapping_sub(t.offset.y);
        if internal_format_for_data(format, ty) != t.internal_format {
            // As swgl: debug gl.cc stops here too (`assert(false)`, swgl/src/gl.cc:2841), release reads nothing.
            // The oracle tests probe a mismatched format only in release (tests/oracle_gl.rs).
            debug_assert!(false, "mismatched format for read pixels");
            return;
        }
        let bpp = i64::from(t.bpp());
        let mut dest = dest_base;
        let dest_stride = i64::from(width) * bpp;
        if y < 0 {
            dest.off += -i64::from(y) * dest_stride;
            height += y;
            y = 0;
        }
        if y + height > t.height {
            height = t.height - y;
        }
        if x < 0 {
            dest.off += -i64::from(x) * bpp;
            width += x;
            x = 0;
        }
        if x + width > t.width {
            width = t.width - x;
        }
        if width <= 0 || height <= 0 {
            return;
        }
        let src = texture_cursor(t, x, y);
        // SAFETY: the source is clipped to the texture, the destination to the pack buffer or the caller's memory.
        unsafe { convert_copy(format, t.internal_format, dest, dest_stride, src, t.stride() as i64, width as usize, height as usize) };
    }

    /// `CopyImageSubData`, each row clipped to both textures.
    #[allow(clippy::too_many_arguments)]
    pub fn copy_image_sub_data(
        &mut self,
        mut src_name: u32,
        src_target: u32,
        _src_level: i32,
        src_x: i32,
        src_y: i32,
        _src_z: i32,
        mut dst_name: u32,
        dst_target: u32,
        _dst_level: i32,
        dst_x: i32,
        dst_y: i32,
        _dst_z: i32,
        src_width: i32,
        src_height: i32,
        _src_depth: i32,
    ) {
        if src_target == GL_RENDERBUFFER {
            src_name = self.renderbuffers.get_mut(src_name).texture;
        }
        if dst_target == GL_RENDERBUFFER {
            dst_name = self.renderbuffers.get_mut(dst_name).texture;
        }
        if self.textures.get_mut(src_name).buf.is_null() {
            return;
        }
        prepare_texture(self.textures.get_mut(src_name), None);
        if self.textures.get_mut(dst_name).buf.is_null() {
            return;
        }
        let skip = IntRect::new(dst_x, dst_y, dst_x.wrapping_add(src_width), dst_y.wrapping_add(src_height));
        prepare_texture(self.textures.get_mut(dst_name), Some(&skip));
        let same = src_name == dst_name;
        let (srctex, dsttex): (*const Texture, *mut Texture) = if same {
            let t = self.textures.get_mut(src_name) as *mut Texture;
            (t, t)
        } else {
            let (s, d) = self.textures.get_two_mut(src_name, dst_name).expect("both textures exist");
            (s as *const Texture, d as *mut Texture)
        };
        // SAFETY: both point at live boxed textures of this context; for one texture, the rows are copied with
        // `ptr::copy`, which allows overlap, as memcpy on distinct rows does in the C++.
        let (srctex, dsttex) = unsafe { (&*srctex, &*dsttex) };
        let bpp = srctex.bpp();
        let mut dst = texture_cursor(dsttex, dst_x, dst_y);
        let mut src = texture_cursor(srctex, src_x, src_y);
        let (src_stride, dst_stride) = (srctex.stride() as i64, dsttex.stride() as i64);
        for _ in 0..src_height.max(0) {
            let (mut d, mut s) = (dst, src);
            let mut len = (src_width.max(0) as usize) * bpp as usize;
            if clip_ptrs_against_bounds(&mut d, &mut s, &mut len, 1, 1) > 0 {
                break;
            }
            if len > 0 {
                // SAFETY: both spans are clipped to their texture buffers.
                unsafe {
                    core::ptr::copy(s.base.wrapping_offset(s.off as isize), d.base.wrapping_offset(d.off as isize), len)
                };
            }
            dst.off += dst_stride;
            src.off += src_stride;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn copy_tex_sub_image_2d(&mut self, target: u32, _level: i32, xoffset: i32, yoffset: i32, x: i32, y: i32, width: i32, height: i32) {
        let Some(fbid) = self.get_framebuffer(GL_READ_FRAMEBUFFER, false) else { return };
        let color = self.framebuffers.get_mut(fbid).color_attachment;
        let dst = self.binding(target);
        self.copy_image_sub_data(color, GL_TEXTURE_2D, 0, x, y, 0, dst, GL_TEXTURE_2D, 0, xoffset, yoffset, 0, width, height, 1);
    }

    /// `BlitFramebuffer`: the framebuffer and texture lookup; the pixels are composite.rs's.
    #[allow(clippy::too_many_arguments)]
    pub fn blit_framebuffer(
        &mut self,
        src_x0: i32,
        src_y0: i32,
        src_x1: i32,
        src_y1: i32,
        dst_x0: i32,
        dst_y0: i32,
        dst_x1: i32,
        dst_y1: i32,
        mask: u32,
        filter: u32,
    ) {
        debug_assert!(mask == GL_COLOR_BUFFER_BIT);
        let Some(srcfb) = self.get_framebuffer(GL_READ_FRAMEBUFFER, false) else { return };
        let Some(dstfb) = self.get_framebuffer(GL_DRAW_FRAMEBUFFER, false) else { return };
        let src = self.framebuffers.get_mut(srcfb).color_attachment;
        if self.textures.get_mut(src).buf.is_null() {
            return;
        }
        let dst = self.framebuffers.get_mut(dstfb).color_attachment;
        if self.textures.get_mut(dst).buf.is_null() {
            return;
        }
        let p = prof::begin();
        if src == dst {
            let t = self.textures.get_mut(src);
            composite::blit_framebuffer_same(t, src_x0, src_y0, src_x1, src_y1, dst_x0, dst_y0, dst_x1, dst_y1, filter);
        } else if let Some((s, d)) = self.textures.get_two_mut(src, dst) {
            composite::blit_framebuffer(s, d, src_x0, src_y0, src_x1, src_y1, dst_x0, dst_y0, dst_x1, dst_y1, filter);
        }
        let px = u64::from(dst_x1.abs_diff(dst_x0)) * u64::from(dst_y1.abs_diff(dst_y0));
        prof::end(p, "BlitFramebuffer", 0, 1, px);
    }

    /// Not in gl.cc: a texture's pixels without applying its delayed clear, for the trace replayer's per-call
    /// hashes. `bound` names the texture bound to `target` on the active unit; otherwise `name` is a texture, or a
    /// renderbuffer when `target` is `GL_RENDERBUFFER`. Creates nothing. `(buf, width, height, stride, bpp)`.
    pub fn get_texture_buffer(&self, target: u32, name: u32, bound: bool) -> Option<(*mut u8, i32, i32, i32, i32)> {
        let id = if bound {
            let unit = &self.texture_units[self.active_texture_unit];
            match target {
                GL_TEXTURE_2D => unit.texture_2d_binding,
                GL_TEXTURE_RECTANGLE => unit.texture_rectangle_binding,
                _ => return None,
            }
        } else if target == GL_RENDERBUFFER {
            self.renderbuffers.find(name)?.texture
        } else {
            name
        };
        let t = self.textures.find(id)?;
        (!t.buf.is_null()).then(|| (t.buf, t.width, t.height, t.stride() as i32, t.bpp()))
    }

    /// `VertexArray::validate()` for the current vertex array.
    fn validate_current_vertex_array(&mut self) {
        let vao = self.current_vertex_array;
        let max = self.vertex_arrays.get_mut(vao).max_attrib;
        let mut last_enabled = -1;
        for i in 0..=max {
            let a = self.vertex_arrays.get_mut(vao).attribs[i as usize];
            if a.enabled {
                let size = self.buffers.get_mut(a.vertex_buffer as u32).size;
                self.vertex_arrays.get_mut(vao).attribs[i as usize].buf_size = size;
                last_enabled = i;
            }
        }
        self.vertex_arrays.get_mut(vao).max_attrib = last_enabled;
    }

    /// `DrawElementsInstanced(mode, count, type, offset, instancecount)`.
    pub fn draw_elements_instanced(&mut self, mode: u32, count: i32, type_: u32, offset: isize, instancecount: i32) {
        let p = prof::begin();
        if offset < 0 || count <= 0 || instancecount <= 0 || self.vertex_shader.is_none() || self.fragment_shader.is_none() {
            return;
        }
        let Some(fbid) = self.get_framebuffer(GL_DRAW_FRAMEBUFFER, true) else { return };
        let fb = *self.framebuffers.get_mut(fbid);
        if fb.color_attachment == 0 {
            return;
        }
        if self.textures.get_mut(fb.color_attachment).buf.is_null() {
            return;
        }
        let depth_id = if self.depthtest { fb.depth_attachment } else { 0 };
        self.textures.get_mut(depth_id);
        if self.validate_vertex_array {
            self.validate_vertex_array = false;
            self.validate_current_vertex_array();
        }
        self.shaded_rows = 0;
        self.shaded_pixels = 0;

        let program = self.current_program;
        let mut dc = DrawCtx {
            viewport: self.viewport,
            scissor: self.scissortest.then_some(self.scissor),
            blend: self.blend,
            blend_key: self.blend_key,
            depthfunc: self.depthfunc,
            depthmask: self.depthmask,
            shaded_rows: 0,
            shaded_pixels: 0,
            stripe: None,
        };
        self.raster.blendcolor = self.blendcolor;

        // The samplers' textures, looked up the way `get_texture` does when a shader asks (`operator[]` for the
        // texture 0 every unbound unit names; a bound id with no object reads as no texture). Each is delay-cleared
        // here, as `init_sampler`'s `prepare_texture` would when the shader binds it (a bound texture the shader does
        // not sample is cleared too: the same bytes), and the views keep only what a sampler reads, so that binding
        // the samplers in `init_batch` touches no texture object while this draw borrows its targets.
        self.textures.get_mut(0);
        for (unit, view) in self.texture_units.iter().zip(self.raster.texture_units.iter_mut()) {
            let source = |id: u32, store: &mut ObjectStore<Texture>| {
                let t = store.find_mut(id)?;
                // The formats a delayed clear can be pending on (`prepare_texture`).
                if matches!(t.internal_format, GL_RGBA8 | GL_R8 | GL_RG8) {
                    prepare_texture(t, None);
                }
                t.sampler_source()
            };
            view.texture_2d = source(unit.texture_2d_binding, &mut self.textures);
            view.texture_rectangle = source(unit.texture_rectangle_binding, &mut self.textures);
        }
        // A draw that samples its own color or depth buffer (a feedback loop, undefined in GL) is drawn on this
        // thread alone, as swgl draws it: rows are read as they are written, in order, and no other thread writes
        // a row a sampler reads.
        let feedback = {
            let targets: Vec<(*const u8, usize)> = [fb.color_attachment, depth_id]
                .iter()
                .filter_map(|&id| self.textures.find(id).map(|t| (t.buf() as *const u8, t.len())))
                .collect();
            let units = self.raster.texture_units.iter();
            let mut views = units.flat_map(|v| [v.texture_2d, v.texture_rectangle]).flatten();
            views.any(|v| targets.iter().any(|&(buf, len)| v.overlaps(buf, len)))
        };

        let threads = self.raster_threads;
        let split_pixels = self.raster_split_pixels;
        let (static_stripes, slow_workers) = (self.raster_static_stripes, self.raster_slow_workers);
        let Context { textures, programs, buffers, vertex_arrays, raster, current_vertex_array, pool, .. } = self;
        let indices_id = vertex_arrays.get_mut(*current_vertex_array).element_array_buffer_binding;
        buffers.get_mut(indices_id);
        let colortex: *mut Texture = textures.get_mut(fb.color_attachment);
        let mut no_depth = Texture::default();
        let depthtex: *mut Texture =
            if depth_id == fb.color_attachment { &mut no_depth } else { textures.get_mut(depth_id) };
        // SAFETY: two distinct boxed textures of this context (or the local empty one), alive for the draw. Nothing
        // else refers to these objects while the references live: the samplers hold `SamplerSource`s (the views
        // above), which point at texel buffers, not at textures, and read them through raw pointers only, as swgl's
        // do. A sampled buffer that is also a target is drawn on this thread only (`feedback`), and written through
        // raw row pointers; the one `&mut [u8]` over its rows, a delayed clear's `force_clear_row`, lives only while
        // it clears that row, never while a shader samples.
        let (colortex, depthtex) = unsafe { (&mut *colortex, &mut *depthtex) };
        debug_assert!(colortex.internal_format == GL_RGBA8 || colortex.internal_format == GL_R8);

        let v = vertex_arrays.get_mut(*current_vertex_array);
        let attribs: Vec<VertexAttrib<'_>> = v
            .attribs
            .iter()
            .map(|a| {
                let bytes = if a.enabled && a.buf_size > 0 {
                    buffers.find(a.vertex_buffer as u32).map_or(&[][..], |b| {
                        let n = a.buf_size.min(b.size);
                        &b.bytes()[..n]
                    })
                } else {
                    &[][..]
                };
                VertexAttrib {
                    size: a.size,
                    type_: a.type_,
                    normalized: a.normalized,
                    stride: a.stride,
                    offset: a.offset,
                    enabled: a.enabled,
                    divisor: a.divisor,
                    vertex_array: a.vertex_array,
                    vertex_buffer: a.vertex_buffer,
                    buf: bytes,
                }
            })
            .collect();
        let Some(prog) = programs.find_mut(program).and_then(|p| p.impl_.as_deref_mut()) else { return };

        prog.get_vertex_shader().init_batch(raster);

        // The draw's primitives in order, each loaded with `load_attribs` and passed to `f` with its point count.
        let visit = |prog: &mut dyn ProgramImpl, f: &mut dyn FnMut(&mut dyn ProgramImpl, i32)| match type_ {
            GL_UNSIGNED_SHORT | GL_UNSIGNED_INT => {
                debug_assert!(mode == GL_TRIANGLES);
                let bytes = buffers.find(indices_id).filter(|b| b.has_buf()).map_or(&[][..], |b| b.bytes());
                let size = if type_ == GL_UNSIGNED_SHORT { 2 } else { 4 };
                rasterize::for_each_primitive(prog, size, count, instancecount, offset as usize, &attribs, bytes, f);
            }
            GL_NONE => {
                for instance in 0..instancecount {
                    let n = match mode {
                        GL_LINES => 2,
                        GL_TRIANGLES => 3,
                        _ => {
                            debug_assert!(false);
                            break;
                        }
                    };
                    let mut i = 0;
                    while i + n <= count {
                        prog.get_vertex_shader().load_attribs(&attribs, (offset as i64 + i64::from(i)) as u32, instance, n);
                        f(prog, n);
                        i += n;
                    }
                }
            }
            _ => debug_assert!(false),
        };

        let target = dc.apply_scissor(colortex);
        let target_pixels = i64::from(target.height().max(0)) * i64::from(target.width().max(0));
        if threads <= 1 || target_pixels <= split_pixels || feedback {
            visit(prog, &mut |prog, nump| rasterize::draw_quad(&mut dc, raster, prog, nump, colortex, depthtex));
        } else {
            // The vertex stage runs once per primitive, here, keeping what each rasterizes from: its outputs, and the
            // raster state and fragment shader (flat varyings included) as it left them.
            // Primitives are rasterized here, without copying any state, until their areas add up to more than
            // `split_pixels`; only the rest is kept for the threads. Both paths give the same bytes, and everything
            // drawn here is done before the threads start, so pixels are still written in submission order.
            let mut shaded: Vec<(rasterize::Shaded, Mutex<ShadedState>)> = Vec::new();
            let mut serial_area = 0i64;
            visit(prog, &mut |prog, nump| {
                let sh = rasterize::shade_quad(&dc, raster, prog, nump, colortex);
                if sh.area <= 0 {
                    return;
                }
                if shaded.is_empty() && serial_area + sh.area <= split_pixels {
                    serial_area += sh.area;
                    rasterize::raster_shaded(&mut dc, raster, prog.get_fragment_shader(), &sh, colortex, depthtex);
                } else {
                    shaded.push((sh, Mutex::new((raster.clone(), prog.get_fragment_shader().box_clone()))));
                }
            });
            let area: i64 = shaded.iter().map(|(sh, _)| sh.area).sum();
            // Small primitives (glyphs) cost each thread a state copy and their rows' stepping, more than their
            // pixels: those draws stay on this thread.
            let small = area < split_thresholds().1 * shaded.len() as i64;
            if area <= split_pixels || small {
                for (sh, state) in shaded {
                    let (mut rs, mut fs) = state.into_inner().unwrap_or_else(|e| e.into_inner());
                    rasterize::raster_shaded(&mut dc, &mut rs, fs.as_mut(), &sh, colortex, depthtex);
                }
            } else {
                let pool = pool.get_or_insert_with(|| Pool::new(threads));
                let n = pool.threads();
                // The stripes the draw's primitives can touch; threads claim them one at a time from `next`.
                let y0 = shaded.iter().map(|(sh, _)| sh.rows.0.max(0)).min().unwrap_or(0);
                let y1 = shaded.iter().map(|(sh, _)| sh.rows.1.max(0)).max().unwrap_or(0);
                let (s0, s1) = (i64::from(y0) / rules::STRIPE, (i64::from(y1) + rules::STRIPE - 1) / rules::STRIPE);
                let next = AtomicI64::new(0);
                // The order stripes are claimed in: ascending, or (`LUCINANET_RASTER_CLAIM=area`) by the pixels the
                // draw's primitives cover in them, most first, so that the last claims are the short ones.
                let order: Vec<i64> = if claim_by_area() {
                    let mut cover: Vec<(i64, i64)> = (s0..s1).map(|s| (0, s)).collect();
                    for (sh, _) in &shaded {
                        let (a, b) = (i64::from(sh.rows.0.max(0)), i64::from(sh.rows.1.max(0)));
                        let w = if b > a { sh.area / (b - a) } else { 0 };
                        for (c, s) in cover.iter_mut() {
                            let rows = (b.min((*s + 1) * rules::STRIPE) - a.max(*s * rules::STRIPE)).max(0);
                            *c += rows * w;
                        }
                    }
                    cover.sort_by_key(|&(c, s)| (core::cmp::Reverse(c), s));
                    cover.into_iter().map(|(_, s)| s).collect()
                } else {
                    (s0..s1).collect()
                };
                // Each mutex of `jobs` is taken once, by its own thread; a primitive's state is locked only while a
                // thread copies it, before rasterizing it.
                let jobs: Vec<Mutex<StripeJob>> = (0..n)
                    .map(|_| Mutex::new((DrawCtx { shaded_rows: 0, shaded_pixels: 0, ..dc }, colortex.stripe_view(), depthtex.stripe_view(), Vec::new())))
                    .collect();
                // SAFETY: the threads write the color and depth buffers through raw row pointers of their views at
                // the same time. Each stripe index is taken once (one `fetch_add` of `next`, or by `s % n` with
                // static stripes), a thread shades only the rows of the stripe it took (`rasterize::owns_row`,
                // `rules::stripe_span`), and a delayed-clear word holds the 32 rows of one stripe, so no row, pixel
                // or word is reached by two threads. The buffers outlive `pool.run`.
                pool.run(&|t| {
                    let mut job = jobs[t].lock().unwrap_or_else(|e| e.into_inner());
                    let (dc, colortex, depthtex, claimed) = &mut *job;
                    let mut k = 0;
                    loop {
                        let s = if static_stripes {
                            let s = s0 + (k * n as i64 + t as i64);
                            k += 1;
                            s
                        } else {
                            match order.get(next.fetch_add(1, AtomicOrdering::Relaxed) as usize) {
                                Some(&s) => s,
                                None => break,
                            }
                        };
                        if s >= s1 {
                            break;
                        }
                        claimed.push(s);
                        if let Some((from, d)) = slow_workers {
                            if t >= from {
                                std::thread::sleep(d);
                            }
                        }
                        dc.stripe = Some(s);
                        for (sh, state) in &shaded {
                            if !rasterize::stripe_meets(sh.rows, s) {
                                continue;
                            }
                            let (mut rs, mut fs) = {
                                let st = state.lock().unwrap_or_else(|e| e.into_inner());
                                (st.0.clone(), st.1.box_clone())
                            };
                            rasterize::raster_shaded(dc, &mut rs, fs.as_mut(), sh, colortex, depthtex);
                        }
                    }
                });
                let jobs: Vec<_> = jobs.into_iter().map(|j| j.into_inner().unwrap_or_else(|e| e.into_inner())).collect();
                colortex.merge_stripe_views(&jobs.iter().map(|j| (&j.1, &j.3[..])).collect::<Vec<_>>());
                depthtex.merge_stripe_views(&jobs.iter().map(|j| (&j.2, &j.3[..])).collect::<Vec<_>>());
                for j in &jobs {
                    dc.shaded_rows += j.0.shaded_rows;
                    dc.shaded_pixels += j.0.shaded_pixels;
                }
                self.raster_last_split_stripes = jobs.iter().map(|j| j.3.len() as u32).collect();
            }
        }

        // The views name this draw's textures, which may be deleted or reallocated before the next one.
        self.raster.texture_units = Default::default();
        self.shaded_rows = dc.shaded_rows;
        self.shaded_pixels = dc.shaded_pixels;
        if self.samples_passed_query != 0 {
            let q = self.queries.get_mut(self.samples_passed_query);
            q.value = q.value.wrapping_add(self.shaded_pixels as u64);
        }
        if p != 0 {
            let name = self.programs.find_mut(program).and_then(|p| p.impl_.as_deref_mut()).map_or("", |i| i.get_name());
            prof::end(p, name, self.blend_key as i32, instancecount as u64, self.shaded_pixels as u64);
        }
    }

    pub fn finish(&mut self) {}

    /// `ReportMemory(ctx, size_of_op)`: the bytes of the buffers swgl allocated.
    pub fn report_memory(&self, size_of_op: &mut dyn FnMut(*const c_void) -> usize) -> usize {
        self.textures.iter().filter(|t| t.should_free() && !t.buf.is_null()).map(|t| size_of_op(t.buf as *const c_void)).sum()
    }
}

/// `get_time_value()`: `CLOCK_MONOTONIC` in nanoseconds.
fn get_time_value() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

/// `CreateContext()`.
pub fn create_context() -> *mut Context {
    Box::into_raw(Box::new(Context::new()))
}

/// `ReferenceContext(c)`.
///
/// # Safety
/// `c` is null or a live context from [`create_context`].
pub unsafe fn reference_context(c: *mut Context) {
    if let Some(c) = unsafe { c.as_mut() } {
        c.references += 1;
    }
}

/// `DestroyContext(c)`: drops a reference; true if that freed the context (the caller then makes nothing current
/// if it was).
///
/// # Safety
/// `c` is null or a live context from [`create_context`]; after `true`, it is gone.
pub unsafe fn destroy_context(c: *mut Context) -> bool {
    let Some(r) = (unsafe { c.as_mut() }) else { return false };
    debug_assert!(r.references > 0);
    r.references -= 1;
    if r.references > 0 {
        return false;
    }
    // SAFETY: the last reference; `c` came from `Box::into_raw`.
    drop(unsafe { Box::from_raw(c) });
    true
}
