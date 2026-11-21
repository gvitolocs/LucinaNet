/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Port of swgl's `program.h`: the interface between the rasterizer and generated shaders.
//!
//! The C++ keeps a table of function pointers in each shader object; here each pointer is a required trait method
//! (`run_func`, `skip_func`, ...) and the C++ inline wrappers are provided methods. Shader objects are cloned per
//! rasterizing thread (`box_clone`), so the traits require `Send`.

use crate::glsl::{
    ivec2, ivec2_scalar, ivec3_scalar, ivec4, ivec4_scalar, vec2, vec2_scalar, vec3, vec3_scalar, vec4, vec4_scalar, Bool,
    Float, PutNth, I32,
};
use crate::RasterState;

/// Type holding group of scalars interpolated across rasterized rows and spans,
/// shuttling values between vertex shaders and fragment shaders.
pub type Interpolants = vec3;

/// Clip distances, if enabled, are always stored in the first SIMD chunk of the
/// interpolants.
#[inline(always)]
pub fn get_clip_distances(interp: &Interpolants) -> Float {
    interp.x
}

/// The maximum size of the gl_ClipDistance array.
pub const gl_MaxClipDistances: i32 = 4;

/// The number of pixels in a step.
pub const swgl_StepSize: i32 = 4;

/// Represents a vertex attribute in the vertex shader.
#[derive(Clone, Copy, Debug, Default)]
pub struct VertexAttrib<'a> {
    /// Size of the attribute in bytes.
    pub size: usize,
    /// Type of the attribute (GLenum).
    pub type_: u32,
    /// Whether the attribute is normalized.
    pub normalized: bool,
    /// Stride between consecutive attributes.
    pub stride: i32,
    /// Offset of the attribute in the buffer.
    pub offset: u32,
    /// Whether the attribute is enabled.
    pub enabled: bool,
    /// Divisor for instanced rendering.
    pub divisor: u32,
    /// Vertex array object.
    pub vertex_array: i32,
    /// Vertex buffer object.
    pub vertex_buffer: i32,
    /// Buffer containing the attribute data.
    pub buf: &'a [u8],
}

impl<'a> VertexAttrib<'a> {
    /// Disables the vertex attribute.
    pub fn disable(&mut self) {
        self.enabled = false;
        self.buf = &[];
    }

    /// Returns the size of the buffer.
    pub fn buf_size(&self) -> usize {
        self.buf.len()
    }
}

/// Trait for program implementations.
pub trait ProgramImpl: Send {
    /// Gets the uniform location by name.
    fn get_uniform(&self, name: &str) -> i32;

    /// Binds an attribute to a specific index.
    fn bind_attrib(&mut self, name: &str, index: i32);

    /// Gets the attribute location by name.
    fn get_attrib(&self, name: &str) -> i32;

    /// Returns the size of the interpolants.
    fn interpolants_size(&self) -> usize;

    /// Gets the vertex shader implementation.
    fn get_vertex_shader(&mut self) -> &mut dyn VertexShaderImpl;

    /// Gets the fragment shader implementation.
    fn get_fragment_shader(&mut self) -> &mut dyn FragmentShaderImpl;

    /// Gets the name of the program.
    fn get_name(&self) -> &str;

    /// Clones the program implementation.
    fn box_clone(&self) -> Box<dyn ProgramImpl>;
}

/// Clones the program implementation.
impl Clone for Box<dyn ProgramImpl> {
    fn clone(&self) -> Self {
        self.box_clone()
    }
}

/// Type alias for program loaders.
pub type ProgramLoader = fn() -> Box<dyn ProgramImpl>;

/// Represents the state of a vertex shader.
#[derive(Clone, Copy, Debug, Default)]
pub struct VertexShaderState {
    /// Flags for the vertex shader.
    pub flags: i32,
    /// The position of the vertex.
    pub gl_Position: vec4,
    /// Clip distances for the vertex.
    pub gl_ClipDistance: [Float; gl_MaxClipDistances as usize],
}

impl VertexShaderState {
    /// Flag for clip distance.
    pub const CLIP_DISTANCE: i32 = 1 << 0;

    /// Enables clip distance.
    pub fn enable_clip_distance(&mut self) {
        self.flags |= Self::CLIP_DISTANCE;
    }

    /// Checks if clip distance is enabled.
    #[inline(always)]
    pub fn use_clip_distance(&self) -> bool {
        (self.flags & Self::CLIP_DISTANCE) != 0
    }
}

/// Trait for vertex shader implementations.
pub trait VertexShaderImpl: Send {
    /// Gets the vertex shader state.
    fn state(&self) -> &VertexShaderState;

    /// Gets a mutable reference to the vertex shader state.
    fn state_mut(&mut self) -> &mut VertexShaderState;

    /// Sets a uniform integer value.
    fn set_uniform_1i(&mut self, index: i32, value: i32);

    /// Sets a uniform float vector value.
    fn set_uniform_4fv(&mut self, index: i32, value: &[f32]);

    /// Sets a uniform matrix value.
    fn set_uniform_matrix4fv(&mut self, index: i32, value: &[f32]);

    /// Initializes a batch of vertices (binds the samplers through `ctx`).
    fn init_batch(&mut self, ctx: &mut RasterState);

    /// Loads vertex attributes.
    fn load_attribs(&mut self, attribs: &[VertexAttrib<'_>], start: u32, instance: i32, count: i32);

    /// Runs the vertex shader on 4 vertices and writes 4 `InterpOutputs`, `interp_stride` bytes apart.
    fn run_primitive(&mut self, ctx: &mut RasterState, interps: &mut [u8], interp_stride: usize);

    /// Clones the vertex shader implementation.
    fn box_clone(&self) -> Box<dyn VertexShaderImpl>;

    fn enable_clip_distance(&mut self) {
        self.state_mut().enable_clip_distance();
    }

    #[inline(always)]
    fn use_clip_distance(&self) -> bool {
        self.state().use_clip_distance()
    }
}

/// Clones the vertex shader implementation.
impl Clone for Box<dyn VertexShaderImpl> {
    fn clone(&self) -> Self {
        self.box_clone()
    }
}

/// The span output position (`uint32_t* swgl_OutRGBA8` / `uint8_t* swgl_OutR8` in the C++).
#[derive(Debug)]
pub struct OutPtr<T>(pub *mut T);

impl<T> OutPtr<T> {
    /// Checks if the pointer is null.
    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }

    /// Gets the raw pointer.
    pub fn get(&self) -> *mut T {
        self.0
    }
}

// Manual Clone and Copy: the derives would require `T: Copy`.
impl<T> Clone for OutPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for OutPtr<T> {}

/// Manual implementation of Default for OutPtr.
impl<T> Default for OutPtr<T> {
    fn default() -> Self {
        OutPtr(core::ptr::null_mut())
    }
}

// SAFETY: the pointer is only set by `draw_span_*` for the duration of one span, on the thread that owns this
// shader copy, and is never dereferenced outside that call; sending the shader to another thread between spans
// moves a pointer nobody uses.
unsafe impl<T> Send for OutPtr<T> {}

/// Represents the state of a fragment shader.
#[derive(Clone, Copy, Debug)]
pub struct FragmentShaderState {
    /// Flags for the fragment shader.
    pub flags: i32,
    /// The fragment coordinate.
    pub gl_FragCoord: vec4,
    /// The fragment color.
    pub gl_FragColor: vec4,
    /// The secondary fragment color.
    pub gl_SecondaryFragColor: vec4,
    /// Step values for Z and W coordinates.
    pub swgl_StepZW: vec2_scalar,
    /// Whether the pixel is discarded.
    pub swgl_IsPixelDiscarded: Bool,
    /// Output pointer for RGBA8 data.
    pub swgl_OutRGBA8: OutPtr<u32>,
    /// Output pointer for R8 data.
    pub swgl_OutR8: OutPtr<u8>,
    /// The length of the span.
    pub swgl_SpanLength: i32,
}

impl Default for FragmentShaderState {
    fn default() -> Self {
        FragmentShaderState {
            flags: 0,
            gl_FragCoord: vec4::default(),
            gl_FragColor: vec4::default(),
            gl_SecondaryFragColor: vec4::default(),
            swgl_StepZW: vec2_scalar::default(),
            swgl_IsPixelDiscarded: Bool::splat(0),
            swgl_OutRGBA8: OutPtr::default(),
            swgl_OutR8: OutPtr::default(),
            swgl_SpanLength: 0,
        }
    }
}

impl FragmentShaderState {
    /// Flag for discarding fragments.
    pub const DISCARD: i32 = 1 << 0;
    /// Flag for perspective correction.
    pub const PERSPECTIVE: i32 = 1 << 1;

    /// Enables fragment discarding.
    pub fn enable_discard(&mut self) {
        self.flags |= Self::DISCARD;
    }

    /// Enables perspective correction.
    pub fn enable_perspective(&mut self) {
        self.flags |= Self::PERSPECTIVE;
    }

    /// Checks if fragment discarding is enabled.
    pub fn use_discard(&self) -> bool {
        (self.flags & Self::DISCARD) != 0
    }

    /// Checks if perspective correction is enabled.
    pub fn use_perspective(&self) -> bool {
        (self.flags & Self::PERSPECTIVE) != 0
    }

    /// `gl_FragCoord.x += steps` (the C++ default is 4: pass [`swgl_StepSize`]).
    #[inline(always)]
    pub fn step_fragcoord(&mut self, steps: i32) {
        self.gl_FragCoord.x += Float::splat(steps as f32);
    }

    /// `gl_FragCoord.z += swgl_StepZW.x * steps`, `gl_FragCoord.w += swgl_StepZW.y * steps`.
    #[inline(always)]
    pub fn step_perspective(&mut self, steps: i32) {
        self.gl_FragCoord.z += Float::splat(self.swgl_StepZW.x * steps as f32);
        self.gl_FragCoord.w += Float::splat(self.swgl_StepZW.y * steps as f32);
    }
}

/// Trait for fragment shader implementations.
pub trait FragmentShaderImpl: Send {
    /// Gets the fragment shader state.
    fn state(&self) -> &FragmentShaderState;

    /// Gets a mutable reference to the fragment shader state.
    fn state_mut(&mut self) -> &mut FragmentShaderState;

    /// Initializes a span of fragments. `interps` and `step` hold one `InterpOutputs` each (`interpolants_size()`
    /// bytes).
    fn init_span_func(&mut self, interps: &[u8], step: &[u8]);

    /// Runs the fragment shader.
    fn run_func(&mut self, ctx: &mut RasterState);

    /// Skips a number of fragments.
    fn skip_func(&mut self, steps: i32);

    /// Initializes a span of fragments with perspective correction.
    fn init_span_w_func(&mut self, interps: &[u8], step: &[u8]);

    /// Runs the fragment shader with perspective correction.
    fn run_w_func(&mut self, ctx: &mut RasterState);

    /// Skips a number of fragments with perspective correction.
    fn skip_w_func(&mut self, steps: i32);

    /// Draws a span of RGBA8 fragments.
    fn draw_span_rgba8_func(&mut self, ctx: &mut RasterState) -> i32;

    /// Draws a span of R8 fragments.
    fn draw_span_r8_func(&mut self, ctx: &mut RasterState) -> i32;

    /// Checks if the fragment shader can draw RGBA8 spans.
    fn has_draw_span_rgba8(&self) -> bool;

    /// Checks if the fragment shader can draw R8 spans.
    fn has_draw_span_r8(&self) -> bool;

    /// Clones the fragment shader implementation.
    fn box_clone(&self) -> Box<dyn FragmentShaderImpl>;

    /// `init_span<W>`: the perspective (`W`) or plain span setup.
    #[inline(always)]
    fn init_span<const W: bool>(&mut self, interps: &[u8], step: &[u8])
    where
        Self: Sized,
    {
        self.init_span_dyn(W, interps, step)
    }

    #[inline(always)]
    fn run<const W: bool>(&mut self, ctx: &mut RasterState)
    where
        Self: Sized,
    {
        self.run_dyn(ctx, W)
    }

    /// `skip<W>(steps)`; the C++ default of 4 steps is [`swgl_StepSize`].
    #[inline(always)]
    fn skip<const W: bool>(&mut self, steps: i32)
    where
        Self: Sized,
    {
        self.skip_dyn(W, steps)
    }

    #[inline(always)]
    fn init_span_dyn(&mut self, w: bool, interps: &[u8], step: &[u8]) {
        if w {
            self.init_span_w_func(interps, step)
        } else {
            self.init_span_func(interps, step)
        }
    }

    #[inline(always)]
    fn run_dyn(&mut self, ctx: &mut RasterState, w: bool) {
        if w {
            self.run_w_func(ctx)
        } else {
            self.run_func(ctx)
        }
    }

    #[inline(always)]
    fn skip_dyn(&mut self, w: bool, steps: i32) {
        if w {
            self.skip_w_func(steps)
        } else {
            self.skip_func(steps)
        }
    }

    /// `draw_span(uint32_t* buf, int len)`: point the span output at `buf` and let the shader draw what it can.
    /// Returns the number of pixels drawn.
    #[inline(always)]
    fn draw_span_rgba8(&mut self, ctx: &mut RasterState, buf: *mut u32, len: i32) -> i32 {
        let state = self.state_mut();
        state.swgl_OutRGBA8 = OutPtr(buf);
        state.swgl_SpanLength = len;
        self.draw_span_rgba8_func(ctx)
    }

    /// `draw_span(uint8_t* buf, int len)`.
    #[inline(always)]
    fn draw_span_r8(&mut self, ctx: &mut RasterState, buf: *mut u8, len: i32) -> i32 {
        let state = self.state_mut();
        state.swgl_OutR8 = OutPtr(buf);
        state.swgl_SpanLength = len;
        self.draw_span_r8_func(ctx)
    }

    fn enable_discard(&mut self) {
        self.state_mut().enable_discard();
    }

    fn enable_perspective(&mut self) {
        self.state_mut().enable_perspective();
    }

    #[inline(always)]
    fn use_discard(&self) -> bool {
        self.state().use_discard()
    }

    #[inline(always)]
    fn use_perspective(&self) -> bool {
        self.state().use_perspective()
    }

    #[inline(always)]
    fn step_fragcoord(&mut self, steps: i32) {
        self.state_mut().step_fragcoord(steps);
    }

    #[inline(always)]
    fn step_perspective(&mut self, steps: i32) {
        self.state_mut().step_perspective(steps);
    }
}

/// Clones the fragment shader implementation.
impl Clone for Box<dyn FragmentShaderImpl> {
    fn clone(&self) -> Self {
        self.box_clone()
    }
}

// ---- vertex attribute and interpolant loading (gl.cc) ------------------------------------------------------------

/// The number of vertex attributes; the last one is the null attribute (gl.cc `MAX_ATTRIBS`).
pub const MAX_ATTRIBS: usize = 17;
/// The attribute reserved as a null attribute, used when a shader reads an attribute that was never set.
pub const NULL_ATTRIB: usize = 16;

const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_UNSIGNED_SHORT: u32 = 0x1403;

/// A plain-data type the interpolant buffers hold: its bytes can be written into a `[u8]` and read back.
///
/// # Safety
///
/// Every byte of a `Self` is initialized (no padding: [`store_interp`] copies all `size_of::<Self>()` bytes into a byte
/// buffer the rasterizer reads as floats), and every bit pattern of `size_of::<Self>()` bytes is a valid `Self`
/// ([`load_interp`] reads one from any bytes). So: `f32`, `i32`, `u32`, arrays and vectors of them, and structs made
/// only of `Pod` fields that fill them without padding (the generated `InterpOutputs` asserts that).
pub unsafe trait Pod: Copy {}

macro_rules! pod {
    ($($t:ty => $bytes:expr),* $(,)?) => {
        $(
            // SAFETY: `$t` is made of 4-byte floats or integers only, any bits valid, and it is exactly as large as
            // they are (asserted), so it has no padding.
            unsafe impl Pod for $t {}
            const _: () = assert!(core::mem::size_of::<$t>() == $bytes);
        )*
    };
}

pod!(
    f32 => 4,
    i32 => 4,
    u32 => 4,
    Float => 16,
    I32 => 16,
    vec2_scalar => 8,
    vec3_scalar => 12,
    vec4_scalar => 16,
    vec2 => 32,
    vec3 => 48,
    vec4 => 64,
);

// SAFETY: an array of `Pod` values has no padding between them (an element's size is a multiple of its alignment) and
// every bit pattern of each element is valid.
unsafe impl<T: Pod, const N: usize> Pod for [T; N] {}

/// Writes `v` at the start of `dest` (unaligned). Panics if `dest` is shorter than `T`.
#[inline(always)]
pub fn store_interp<T: Pod>(dest: &mut [u8], v: &T) {
    assert!(dest.len() >= core::mem::size_of::<T>(), "interpolant store past the end of the buffer");
    // SAFETY: `dest` holds at least `size_of::<T>()` bytes (checked above); the write is unaligned. `T: Pod` has no
    // padding, so every byte written is initialized.
    unsafe { (dest.as_mut_ptr() as *mut T).write_unaligned(*v) }
}

/// Reads a `T` from the start of `src` (unaligned). Panics if `src` is shorter than `T`.
#[inline(always)]
pub fn load_interp<T: Pod>(src: &[u8]) -> T {
    assert!(src.len() >= core::mem::size_of::<T>(), "interpolant load past the end of the buffer");
    // SAFETY: `src` holds at least `size_of::<T>()` initialized bytes (checked above); the read is unaligned, and any
    // bit pattern is a valid `T: Pod`.
    unsafe { (src.as_ptr() as *const T).read_unaligned() }
}

/// A scalar attribute value (`decltype(force_scalar(attrib))`): `N` components of 4 bytes each.
pub trait AttribScalar: Copy + Default {
    /// Number of components.
    const N: usize;
    /// `ElementType<S>::ty` is `int32_t` (an `ivec`); otherwise it is `float` (as in glsl.h, also for `int`).
    const INT_ELEMENTS: bool;
    /// Component `i` from its raw 4 bytes (the `*reinterpret_cast<const S*>(src)` path).
    fn set_raw(&mut self, i: usize, bits: u32);
    /// `put_nth_component(scalar, i, v)` with a value of the element type, as `f32` or `i32`.
    fn set_elem(&mut self, i: usize, f: f32, n: i32);
}

macro_rules! attrib_scalar_f {
    ($t:ty, $n:literal, [$($c:ident),*]) => {
        impl AttribScalar for $t {
            const N: usize = $n;
            const INT_ELEMENTS: bool = false;
            #[inline(always)]
            fn set_raw(&mut self, i: usize, bits: u32) {
                let mut k = 0usize;
                $(if k == i { self.$c = f32::from_bits(bits); } k += 1;)*
                let _ = k;
            }
            #[inline(always)]
            fn set_elem(&mut self, i: usize, f: f32, _n: i32) {
                let mut k = 0usize;
                $(if k == i { self.$c = f; } k += 1;)*
                let _ = k;
            }
        }
    };
}
macro_rules! attrib_scalar_i {
    ($t:ty, $n:literal, [$($c:ident),*]) => {
        impl AttribScalar for $t {
            const N: usize = $n;
            const INT_ELEMENTS: bool = true;
            #[inline(always)]
            fn set_raw(&mut self, i: usize, bits: u32) {
                let mut k = 0usize;
                $(if k == i { self.$c = bits as i32; } k += 1;)*
                let _ = k;
            }
            #[inline(always)]
            fn set_elem(&mut self, i: usize, _f: f32, n: i32) {
                let mut k = 0usize;
                $(if k == i { self.$c = n; } k += 1;)*
                let _ = k;
            }
        }
    };
}
attrib_scalar_f!(vec2_scalar, 2, [x, y]);
attrib_scalar_f!(vec3_scalar, 3, [x, y, z]);
attrib_scalar_f!(vec4_scalar, 4, [x, y, z, w]);
attrib_scalar_i!(ivec2_scalar, 2, [x, y]);
attrib_scalar_i!(ivec3_scalar, 3, [x, y, z]);
attrib_scalar_i!(ivec4_scalar, 4, [x, y, z, w]);

impl AttribScalar for f32 {
    const N: usize = 1;
    const INT_ELEMENTS: bool = false;
    fn set_raw(&mut self, i: usize, bits: u32) {
        if i == 0 {
            *self = f32::from_bits(bits);
        }
    }
    fn set_elem(&mut self, i: usize, f: f32, _n: i32) {
        if i == 0 {
            *self = f;
        }
    }
}

impl AttribScalar for i32 {
    const N: usize = 1;
    // glsl.h: `ElementType<int>::ty` is `float`; `put_nth_component(int&, 0, float)` truncates.
    const INT_ELEMENTS: bool = false;
    fn set_raw(&mut self, i: usize, bits: u32) {
        if i == 0 {
            *self = bits as i32;
        }
    }
    fn set_elem(&mut self, i: usize, f: f32, _n: i32) {
        if i == 0 {
            *self = crate::glsl::f32_to_i32_scalar(f);
        }
    }
}

/// An attribute in lanes (`vec2`, `vec4`, `Float`, ...), loaded for four vertexes.
pub trait LaneAttrib: Copy + Default {
    type Scalar: AttribScalar;
    /// `T{a, b, c, d}`: one vertex per lane.
    fn from_lanes(lanes: [Self::Scalar; 4]) -> Self;
}

macro_rules! lane_attrib {
    ($($t:ty => $s:ty),*) => {$(
        impl LaneAttrib for $t {
            type Scalar = $s;
            #[inline(always)]
            fn from_lanes(lanes: [$s; 4]) -> Self {
                let mut v = <$t>::default();
                for (i, l) in lanes.into_iter().enumerate() {
                    v.put_nth(i, l);
                }
                v
            }
        }
    )*};
}
lane_attrib!(vec2 => vec2_scalar, vec3 => vec3_scalar, vec4 => vec4_scalar, ivec2 => ivec2_scalar,
             ivec4 => ivec4_scalar);

impl LaneAttrib for Float {
    type Scalar = f32;
    fn from_lanes(lanes: [f32; 4]) -> Self {
        Float::new(lanes)
    }
}

impl LaneAttrib for I32 {
    type Scalar = i32;
    fn from_lanes(lanes: [i32; 4]) -> Self {
        I32::new(lanes)
    }
}

/// A flat (per-primitive) attribute: a scalar value, read once.
pub trait FlatAttrib: Copy {
    type Scalar: AttribScalar;
    fn from_scalar(s: Self::Scalar) -> Self;
}

macro_rules! flat_attrib {
    ($($t:ty),*) => {$(
        impl FlatAttrib for $t {
            type Scalar = $t;
            #[inline(always)]
            fn from_scalar(s: $t) -> Self { s }
        }
    )*};
}
flat_attrib!(f32, i32, vec2_scalar, vec3_scalar, vec4_scalar, ivec2_scalar, ivec3_scalar, ivec4_scalar);

/// `expand_attrib<S, C>`: `size / sizeof(C)` components of `C`, normalized to `0..=1` if asked.
fn expand_attrib<S: AttribScalar>(src: &[u8], csize: usize, size: usize, normalized: bool) -> S {
    let mut scalar = S::default();
    let scale = 1.0f32 / (((1u64 << (8 * csize)) - 1) as f32);
    for i in 0..size / csize {
        let c = match csize {
            1 => u32::from(src.get(i).copied().unwrap_or(0)),
            _ => u32::from(u16::from_ne_bytes([src.get(2 * i).copied().unwrap_or(0), src.get(2 * i + 1).copied().unwrap_or(0)])),
        };
        if S::INT_ELEMENTS {
            // `int32_t(src[i]) * scale` is a float, converted back to the int component.
            let n = if normalized { crate::glsl::f32_to_i32_scalar(c as i32 as f32 * scale) } else { c as i32 };
            scalar.set_elem(i, n as f32, n);
        } else {
            let f = if normalized { c as f32 * scale } else { c as f32 };
            scalar.set_elem(i, f, f as i32);
        }
    }
    scalar
}

/// `load_attrib_scalar<S>(va, src)`. `src` is the attribute's bytes up to the end of its buffer.
fn load_attrib_scalar<S: AttribScalar>(va: &VertexAttrib<'_>, src: &[u8]) -> S {
    let full = S::N * 4;
    let word = |i: usize| -> u32 {
        let b = |k: usize| src.get(4 * i + k).copied().unwrap_or(0);
        u32::from_ne_bytes([b(0), b(1), b(2), b(3)])
    };
    if full <= va.size {
        let mut s = S::default();
        for i in 0..S::N {
            s.set_raw(i, word(i));
        }
        return s;
    }
    if va.type_ == GL_UNSIGNED_SHORT {
        return expand_attrib::<S>(src, 2, va.size, va.normalized);
    }
    if va.type_ == GL_UNSIGNED_BYTE {
        return expand_attrib::<S>(src, 1, va.size, va.normalized);
    }
    // `memcpy(&scalar, src, va.size)`: whole components, then the bytes of a partial one (little-endian).
    let mut s = S::default();
    for i in 0..S::N {
        let n = va.size.saturating_sub(4 * i).min(4);
        if n == 0 {
            break;
        }
        let mut bytes = [0u8; 4];
        for (k, b) in bytes.iter_mut().enumerate().take(n) {
            *b = src.get(4 * i + k).copied().unwrap_or(0);
        }
        s.set_raw(i, u32::from_ne_bytes(bytes));
    }
    s
}

/// The bytes of the attribute at `va.stride * index + va.offset + extra`, to the end of the buffer. The C++ asserts the
/// read is inside the buffer and reads past it in release builds; here bytes past the end read as zero.
fn attrib_bytes<'a>(va: &VertexAttrib<'a>, index: i64, extra: i64) -> &'a [u8] {
    let off = i64::from(va.stride) * index + i64::from(va.offset) + extra;
    if off < 0 || off as u64 >= va.buf.len() as u64 {
        return &[];
    }
    &va.buf[off as usize..]
}

/// `load_attrib(attrib, va, start, instance, count)` (gl.cc): a per-vertex attribute for the 2, 3 or 4 vertexes of a
/// line, triangle or quad, in WebRender's lane order (lines 0 1 1 0, triangles 0 1 2 2, quads 0 1 3 2), or a
/// per-instance one splat to all lanes.
pub fn load_attrib<T: LaneAttrib>(attrib: &mut T, va: &VertexAttrib<'_>, start: u32, instance: i32, count: i32) {
    if va.buf.is_empty() {
        *attrib = T::from_lanes([T::Scalar::default(); 4]);
    } else if va.divisor != 0 {
        let s: T::Scalar = load_attrib_scalar(va, attrib_bytes(va, i64::from(instance), 0));
        *attrib = T::from_lanes([s; 4]);
    } else {
        if count == 0 {
            return;
        }
        debug_assert!((2..=4).contains(&count));
        let start = i64::from(start);
        let stride = i64::from(va.stride);
        let at = |k: i64| -> T::Scalar { load_attrib_scalar(va, attrib_bytes(va, start, stride * k)) };
        *attrib = match count {
            2 => {
                let (a, b) = (at(0), at(1));
                T::from_lanes([a, b, b, a])
            }
            3 => {
                let (a, b, c) = (at(0), at(1), at(2));
                T::from_lanes([a, b, c, c])
            }
            _ => T::from_lanes([at(0), at(1), at(3), at(2)]),
        };
    }
}

/// `load_flat_attrib(attrib, va, start, instance, count)` (gl.cc): one value for the primitive.
pub fn load_flat_attrib<T: FlatAttrib>(attrib: &mut T, va: &VertexAttrib<'_>, start: u32, instance: i32, count: i32) {
    if va.buf.is_empty() {
        *attrib = T::from_scalar(T::Scalar::default());
        return;
    }
    let src = if va.divisor != 0 {
        attrib_bytes(va, i64::from(instance), 0)
    } else {
        if count == 0 {
            return;
        }
        attrib_bytes(va, i64::from(start), 0)
    };
    *attrib = T::from_scalar(load_attrib_scalar(va, src));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shader that counts what the rasterizer asked of it.
    #[derive(Clone, Default)]
    struct CountingShader {
        state: FragmentShaderState,
        spans: i32,
        perspective_spans: i32,
        runs_w: i32,
        last_step: f32,
    }

    impl FragmentShaderImpl for CountingShader {
        fn state(&self) -> &FragmentShaderState {
            &self.state
        }
        fn state_mut(&mut self) -> &mut FragmentShaderState {
            &mut self.state
        }
        fn init_span_func(&mut self, _interps: &[u8], step: &[u8]) {
            self.spans += 1;
            self.last_step = load_interp::<Interpolants>(step).x[0];
        }
        fn run_func(&mut self, _ctx: &mut RasterState) {
            self.state.gl_FragColor.x += Float::splat(1.0);
        }
        fn skip_func(&mut self, steps: i32) {
            self.state.step_fragcoord(steps);
        }
        fn init_span_w_func(&mut self, _interps: &[u8], step: &[u8]) {
            self.perspective_spans += 1;
            self.last_step = load_interp::<Interpolants>(step).x[0];
        }
        fn run_w_func(&mut self, _ctx: &mut RasterState) {
            self.runs_w += 1;
        }
        fn skip_w_func(&mut self, steps: i32) {
            self.state.step_perspective(steps);
        }
        fn draw_span_rgba8_func(&mut self, _ctx: &mut RasterState) -> i32 {
            // Draws whole chunks only, like the generated span shaders.
            self.state.swgl_SpanLength & !3
        }
        fn draw_span_r8_func(&mut self, _ctx: &mut RasterState) -> i32 {
            self.state.swgl_SpanLength
        }
        fn has_draw_span_rgba8(&self) -> bool {
            true
        }
        fn has_draw_span_r8(&self) -> bool {
            false
        }
        fn box_clone(&self) -> Box<dyn FragmentShaderImpl> {
            Box::new(self.clone())
        }
    }

    #[test]
    fn shader_copies_move_to_rasterizing_threads() {
        let mut shader: Box<dyn FragmentShaderImpl> = Box::new(CountingShader::default());
        shader.run_dyn(&mut RasterState::default(), false);
        let copies: Vec<Box<dyn FragmentShaderImpl>> = (0..3).map(|_| shader.clone()).collect();
        let lanes: Vec<f32> = copies
            .into_iter()
            .enumerate()
            .map(|(i, mut copy)| {
                std::thread::spawn(move || {
                    for _ in 0..i {
                        copy.run_dyn(&mut RasterState::default(), false);
                    }
                    copy.state().gl_FragColor.x[0]
                })
            })
            .map(|t| t.join().unwrap())
            .collect();
        assert_eq!(lanes, [1.0, 2.0, 3.0]);
        assert_eq!(shader.state().gl_FragColor.x[0], 1.0);
    }

    #[test]
    fn wrappers_dispatch_like_the_cpp() {
        let mut s = CountingShader::default();
        let ctx = &mut RasterState::default();
        s.run::<false>(ctx);
        s.run::<true>(ctx);
        assert_eq!(s.state.gl_FragColor.x.0, [1.0; 4]);
        assert_eq!(s.runs_w, 1);
        let interp = Interpolants::from(2.0f32);
        let step = Interpolants::from(0.5f32);
        let (mut ib, mut sb) = ([0u8; 48], [0u8; 48]);
        store_interp(&mut ib, &interp);
        store_interp(&mut sb, &step);
        s.init_span::<true>(&ib, &sb);
        assert_eq!((s.spans, s.perspective_spans, s.last_step), (0, 1, 0.5));

        s.state.swgl_StepZW = vec2_scalar { x: 0.25, y: -1.0 };
        s.skip::<true>(swgl_StepSize);
        assert_eq!(s.state.gl_FragCoord.z.0, [1.0; 4]);
        assert_eq!(s.state.gl_FragCoord.w.0, [-4.0; 4]);
        s.skip::<false>(swgl_StepSize);
        assert_eq!(s.state.gl_FragCoord.x.0, [4.0; 4]);

        let mut row = [0u32; 10];
        assert_eq!(s.draw_span_rgba8(ctx, row.as_mut_ptr(), 10), 8);
        assert_eq!(s.state.swgl_OutRGBA8.get(), row.as_mut_ptr());
        assert_eq!(s.state.swgl_SpanLength, 10);
    }

    #[test]
    fn boxed_copies_are_independent() {
        let mut a: Box<dyn FragmentShaderImpl> = Box::new(CountingShader::default());
        a.enable_perspective();
        let mut b = a.clone();
        let ctx = &mut RasterState::default();
        b.run_dyn(ctx, false);
        a.run_dyn(ctx, false);
        a.run_dyn(ctx, false);
        assert_eq!(a.state().gl_FragColor.x.0, [2.0; 4]);
        assert_eq!(b.state().gl_FragColor.x.0, [1.0; 4]);
        assert!(b.use_perspective() && !b.use_discard());
        assert!(!b.has_draw_span_r8());
    }

    #[test]
    fn clip_distances_are_the_first_chunk() {
        let mut v = VertexShaderState::default();
        assert!(!v.use_clip_distance());
        v.enable_clip_distance();
        assert!(v.use_clip_distance());
        let interp = Interpolants::new(Float::splat(3.0), Float::splat(1.0), Float::splat(2.0));
        assert_eq!(get_clip_distances(&interp).0, [3.0; 4]);
        assert_eq!(gl_MaxClipDistances, 4);
    }
}
