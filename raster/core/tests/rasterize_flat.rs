/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The rasterizer against gl.cc's (the oracle crate compiles it, strict floats, asserts out), with a hand-written
//! program until generated shaders exist: "flat", a colour from a uniform and positions from attribute 0, written
//! here against `lucinanet_raster::program` and in `tests/oracle/shim_t.cc` against swgl's program.h. Each script
//! draws quads, triangles, lines and the perspective path through `gl::Context` and through gl.cc, and the whole
//! framebuffer must hold the same bytes after every draw (before the delayed clear is flushed, and after).

use std::ffi::{c_void, CString};
use std::sync::{Arc, Mutex};

use lucinanet_raster::gl::defs::*;
use lucinanet_raster::gl::Context;
use lucinanet_raster::glsl::{cast, init_interp, ivec2, sampler2D_impl, vec2, vec4, Float};
use lucinanet_raster::texture::{lookup_sampler, texelFetchRGBA8};
use lucinanet_raster::program::{
    load_attrib, FragmentShaderImpl, FragmentShaderState, ProgramImpl, VertexAttrib, VertexShaderImpl,
    VertexShaderState,
};
use lucinanet_raster::RasterState;
use lucinanet_raster_oracle::gl as cxx;

/// gl.cc keeps its current context in a static: one script at a time.
/// Every test here takes this lock: gl.cc keeps global state, and the worker-thread test counts the process's threads.
static ORACLE: Mutex<()> = Mutex::new(());

// ---- the program, as tests/oracle/shim_t.cc's flat::Program -----------------------------------------------------

#[derive(Clone)]
struct Vert {
    vs: VertexShaderState,
    color: Arc<Mutex<[f32; 4]>>,
    pos: vec2,
    z: f32,
    w: [f32; 4],
}

impl VertexShaderImpl for Vert {
    fn state(&self) -> &VertexShaderState {
        &self.vs
    }
    fn state_mut(&mut self) -> &mut VertexShaderState {
        &mut self.vs
    }
    fn set_uniform_1i(&mut self, _index: i32, _value: i32) {}
    fn set_uniform_4fv(&mut self, index: i32, value: &[f32]) {
        if index == 0 {
            self.color.lock().unwrap().copy_from_slice(&value[..4]);
        } else if index == 1 {
            self.z = value[0];
            self.w.copy_from_slice(&value[..4]);
            self.w[0] = 1.0;
        }
    }
    fn set_uniform_matrix4fv(&mut self, _index: i32, _value: &[f32]) {}
    fn init_batch(&mut self, _ctx: &mut RasterState) {}
    fn load_attribs(&mut self, attribs: &[VertexAttrib<'_>], start: u32, instance: i32, count: i32) {
        load_attrib(&mut self.pos, &attribs[0], start, instance, count);
    }
    fn run_primitive(&mut self, _ctx: &mut RasterState, _interps: &mut [u8], _interp_stride: usize) {
        let w = if self.w[1] == 0.0 { Float::splat(1.0) } else { Float::new(self.w) };
        self.vs.gl_Position = vec4 { x: self.pos.x * w, y: self.pos.y * w, z: Float::splat(self.z) * w, w };
    }
    fn box_clone(&self) -> Box<dyn VertexShaderImpl> {
        Box::new(self.clone())
    }
}

#[derive(Clone)]
struct Frag {
    fs: FragmentShaderState,
    color: Arc<Mutex<[f32; 4]>>,
}

impl FragmentShaderImpl for Frag {
    fn state(&self) -> &FragmentShaderState {
        &self.fs
    }
    fn state_mut(&mut self) -> &mut FragmentShaderState {
        &mut self.fs
    }
    fn init_span_func(&mut self, _interps: &[u8], _step: &[u8]) {}
    fn run_func(&mut self, _ctx: &mut RasterState) {
        let c = *self.color.lock().unwrap();
        self.fs.gl_FragColor =
            vec4 { x: Float::splat(c[0]), y: Float::splat(c[1]), z: Float::splat(c[2]), w: Float::splat(c[3]) };
    }
    fn skip_func(&mut self, _steps: i32) {}
    fn init_span_w_func(&mut self, _interps: &[u8], _step: &[u8]) {}
    fn run_w_func(&mut self, ctx: &mut RasterState) {
        self.run_func(ctx)
    }
    fn skip_w_func(&mut self, _steps: i32) {}
    fn draw_span_rgba8_func(&mut self, _ctx: &mut RasterState) -> i32 {
        0
    }
    fn draw_span_r8_func(&mut self, _ctx: &mut RasterState) -> i32 {
        0
    }
    fn has_draw_span_rgba8(&self) -> bool {
        false
    }
    fn has_draw_span_r8(&self) -> bool {
        false
    }
    fn box_clone(&self) -> Box<dyn FragmentShaderImpl> {
        Box::new(self.clone())
    }
}

#[derive(Clone)]
struct Flat {
    vert: Vert,
    frag: Frag,
}

impl Flat {
    fn new() -> Flat {
        let color = Arc::new(Mutex::new([0.0; 4]));
        Flat {
            vert: Vert {
                vs: VertexShaderState::default(),
                color: color.clone(),
                pos: vec2::default(),
                z: 0.0,
                w: [1.0, 0.0, 1.0, 1.0],
            },
            frag: Frag { fs: FragmentShaderState::default(), color },
        }
    }
}

impl ProgramImpl for Flat {
    fn get_uniform(&self, name: &str) -> i32 {
        match name {
            "uColor" => 0,
            "uZW" => 1,
            _ => -1,
        }
    }
    fn bind_attrib(&mut self, _name: &str, _index: i32) {}
    fn get_attrib(&self, _name: &str) -> i32 {
        0
    }
    fn interpolants_size(&self) -> usize {
        0
    }
    fn get_vertex_shader(&mut self) -> &mut dyn VertexShaderImpl {
        &mut self.vert
    }
    fn get_fragment_shader(&mut self) -> &mut dyn FragmentShaderImpl {
        &mut self.frag
    }
    fn get_name(&self) -> &str {
        "flat"
    }
    fn box_clone(&self) -> Box<dyn ProgramImpl> {
        Box::new(self.clone())
    }
}

// ---- "grad": flat with one interpolated value, as tests/oracle/shim_t.cc's grad::Program --------------------------

/// The vertex shader of flat that also outputs `v = (x + y) * 0.25 + 0.5` of each position.
#[derive(Clone)]
struct GradVert(Vert);

impl VertexShaderImpl for GradVert {
    fn state(&self) -> &VertexShaderState {
        self.0.state()
    }
    fn state_mut(&mut self) -> &mut VertexShaderState {
        self.0.state_mut()
    }
    fn set_uniform_1i(&mut self, index: i32, value: i32) {
        self.0.set_uniform_1i(index, value)
    }
    fn set_uniform_4fv(&mut self, index: i32, value: &[f32]) {
        self.0.set_uniform_4fv(index, value)
    }
    fn set_uniform_matrix4fv(&mut self, index: i32, value: &[f32]) {
        self.0.set_uniform_matrix4fv(index, value)
    }
    fn init_batch(&mut self, ctx: &mut RasterState) {
        self.0.init_batch(ctx)
    }
    fn load_attribs(&mut self, attribs: &[VertexAttrib<'_>], start: u32, instance: i32, count: i32) {
        self.0.load_attribs(attribs, start, instance, count)
    }
    fn run_primitive(&mut self, ctx: &mut RasterState, interps: &mut [u8], interp_stride: usize) {
        self.0.run_primitive(ctx, interps, interp_stride);
        let v = (self.0.pos.x + self.0.pos.y) * Float::splat(0.25) + Float::splat(0.5);
        for k in 0..4 {
            interps[k * interp_stride..k * interp_stride + 4].copy_from_slice(&v.0[k].to_ne_bytes());
        }
    }
    fn box_clone(&self) -> Box<dyn VertexShaderImpl> {
        Box::new(self.clone())
    }
}

/// The colour `(v, 0.25, 1 - v, 1)`, `v` interpolated.
#[derive(Clone)]
struct GradFrag {
    fs: FragmentShaderState,
    v: Float,
    vstep: f32,
}

impl GradFrag {
    fn init(&mut self, interps: &[u8], step: &[u8]) {
        let i = f32::from_ne_bytes(interps[..4].try_into().unwrap());
        let d = f32::from_ne_bytes(step[..4].try_into().unwrap());
        self.v = init_interp(i, d);
        self.vstep = d * 4.0;
    }
}

impl FragmentShaderImpl for GradFrag {
    fn state(&self) -> &FragmentShaderState {
        &self.fs
    }
    fn state_mut(&mut self) -> &mut FragmentShaderState {
        &mut self.fs
    }
    fn init_span_func(&mut self, interps: &[u8], step: &[u8]) {
        self.init(interps, step)
    }
    fn run_func(&mut self, _ctx: &mut RasterState) {
        self.fs.gl_FragColor = vec4 { x: self.v, y: Float::splat(0.25), z: Float::splat(1.0) - self.v, w: Float::splat(1.0) };
        self.v += Float::splat(self.vstep);
    }
    fn skip_func(&mut self, steps: i32) {
        self.v += Float::splat(self.vstep * (steps as f32 * 0.25));
    }
    fn init_span_w_func(&mut self, interps: &[u8], step: &[u8]) {
        self.init(interps, step)
    }
    fn run_w_func(&mut self, ctx: &mut RasterState) {
        self.run_func(ctx)
    }
    fn skip_w_func(&mut self, steps: i32) {
        self.skip_func(steps)
    }
    fn draw_span_rgba8_func(&mut self, _ctx: &mut RasterState) -> i32 {
        0
    }
    fn draw_span_r8_func(&mut self, _ctx: &mut RasterState) -> i32 {
        0
    }
    fn has_draw_span_rgba8(&self) -> bool {
        false
    }
    fn has_draw_span_r8(&self) -> bool {
        false
    }
    fn box_clone(&self) -> Box<dyn FragmentShaderImpl> {
        Box::new(self.clone())
    }
}

#[derive(Clone)]
struct Grad {
    vert: GradVert,
    frag: GradFrag,
}

impl ProgramImpl for Grad {
    fn get_uniform(&self, name: &str) -> i32 {
        match name {
            "uColor" => 0,
            "uZW" => 1,
            _ => -1,
        }
    }
    fn bind_attrib(&mut self, _name: &str, _index: i32) {}
    fn get_attrib(&self, _name: &str) -> i32 {
        0
    }
    fn interpolants_size(&self) -> usize {
        4
    }
    fn get_vertex_shader(&mut self) -> &mut dyn VertexShaderImpl {
        &mut self.vert
    }
    fn get_fragment_shader(&mut self) -> &mut dyn FragmentShaderImpl {
        &mut self.frag
    }
    fn get_name(&self) -> &str {
        "grad"
    }
    fn box_clone(&self) -> Box<dyn ProgramImpl> {
        Box::new(self.clone())
    }
}

// ---- "feedback": flat whose colour is read from the texture on unit 0, as tests/oracle/shim_t.cc's feedback::Program

/// The colour is the texel at `(gl_FragCoord.x, gl_FragCoord.y - 1)` of the 2D texture on unit 0, red, green and blue
/// rotated `(g, b, r, a)`. Bound to the colour target, each row is the row above it rotated: a feedback loop.
#[derive(Clone)]
struct FeedbackFrag {
    fs: FragmentShaderState,
    sampler: sampler2D_impl,
}

impl FragmentShaderImpl for FeedbackFrag {
    fn state(&self) -> &FragmentShaderState {
        &self.fs
    }
    fn state_mut(&mut self) -> &mut FragmentShaderState {
        &mut self.fs
    }
    fn init_span_func(&mut self, _interps: &[u8], _step: &[u8]) {}
    fn run_func(&mut self, ctx: &mut RasterState) {
        // gl.cc binds it once a batch (the vertex shader's init_batch); binding it per chunk reads the same texture.
        let s = lookup_sampler(ctx, &mut self.sampler, 0);
        let p = ivec2 { x: cast(self.fs.gl_FragCoord.x), y: cast(self.fs.gl_FragCoord.y - Float::splat(1.0)) };
        let c = texelFetchRGBA8(&s, p);
        self.fs.gl_FragColor = vec4 { x: c.y, y: c.z, z: c.x, w: c.w };
        self.fs.step_fragcoord(4);
    }
    fn skip_func(&mut self, steps: i32) {
        self.fs.step_fragcoord(steps);
    }
    fn init_span_w_func(&mut self, _interps: &[u8], _step: &[u8]) {}
    fn run_w_func(&mut self, ctx: &mut RasterState) {
        self.run_func(ctx)
    }
    fn skip_w_func(&mut self, steps: i32) {
        self.skip_func(steps)
    }
    fn draw_span_rgba8_func(&mut self, _ctx: &mut RasterState) -> i32 {
        0
    }
    fn draw_span_r8_func(&mut self, _ctx: &mut RasterState) -> i32 {
        0
    }
    fn has_draw_span_rgba8(&self) -> bool {
        false
    }
    fn has_draw_span_r8(&self) -> bool {
        false
    }
    fn box_clone(&self) -> Box<dyn FragmentShaderImpl> {
        Box::new(self.clone())
    }
}

#[derive(Clone)]
struct Feedback {
    vert: Vert,
    frag: FeedbackFrag,
}

impl ProgramImpl for Feedback {
    fn get_uniform(&self, name: &str) -> i32 {
        match name {
            "uColor" => 0,
            "uZW" => 1,
            _ => -1,
        }
    }
    fn bind_attrib(&mut self, _name: &str, _index: i32) {}
    fn get_attrib(&self, _name: &str) -> i32 {
        0
    }
    fn interpolants_size(&self) -> usize {
        0
    }
    fn get_vertex_shader(&mut self) -> &mut dyn VertexShaderImpl {
        &mut self.vert
    }
    fn get_fragment_shader(&mut self) -> &mut dyn FragmentShaderImpl {
        &mut self.frag
    }
    fn get_name(&self) -> &str {
        "feedback"
    }
    fn box_clone(&self) -> Box<dyn ProgramImpl> {
        Box::new(self.clone())
    }
}

fn program(name: &str) -> Box<dyn ProgramImpl> {
    let flat = Flat::new();
    match name {
        "grad" => Box::new(Grad {
            vert: GradVert(flat.vert),
            frag: GradFrag { fs: FragmentShaderState::default(), v: Float::splat(0.0), vstep: 0.0 },
        }),
        "feedback" => Box::new(Feedback {
            vert: flat.vert,
            frag: FeedbackFrag { fs: FragmentShaderState::default(), sampler: sampler2D_impl::default() },
        }),
        _ => Box::new(flat),
    }
}

// ---- the GL calls the scripts make, on either implementation ----------------------------------------------------

trait Gl {
    /// Binds the program `name` ("flat" or "grad"), a vertex array with attribute 0 (two floats) from an array
    /// buffer, and an element buffer.
    fn setup(&mut self, name: &str) -> u32;
    fn uniform_location(&mut self, prog: u32, name: &str) -> i32;
    fn uniform_4fv(&mut self, location: i32, v: &[f32; 4]);
    unsafe fn buffer_data(&mut self, target: u32, size: isize, data: *const c_void);
    fn draw(&mut self, mode: u32, count: i32, ty: u32);
    fn clear_color(&mut self, c: [f32; 4]);
    fn clear_depth(&mut self, d: f64);
    fn clear(&mut self, mask: u32);
    fn enable(&mut self, cap: u32);
    fn depth_func(&mut self, f: u32);
    fn depth_mask(&mut self, m: u8);
    fn blend_func(&mut self, s: u32, d: u32);
    fn scissor(&mut self, x: i32, y: i32, w: i32, h: i32);
    fn bind_texture_2d(&mut self, texture: u32);
    fn flush(&mut self);
}

struct Rust(Context);

impl Gl for Rust {
    fn setup(&mut self, name: &str) -> u32 {
        let c = &mut self.0;
        let prog = c.create_program();
        c.programs.get_mut(prog).impl_ = Some(program(name));
        c.link_program(prog);
        c.use_program(prog);
        let mut vao = [0];
        c.gen_vertex_arrays(&mut vao);
        c.bind_vertex_array(vao[0]);
        let mut ids = [0u32; 2];
        c.gen_buffers(&mut ids);
        c.bind_buffer(GL_ARRAY_BUFFER, ids[0]);
        c.bind_buffer(GL_ELEMENT_ARRAY_BUFFER, ids[1]);
        c.vertex_attrib_pointer(0, 2, GL_FLOAT, false, 8, 0);
        c.enable_vertex_attrib_array(0);
        prog
    }
    fn uniform_location(&mut self, prog: u32, name: &str) -> i32 {
        self.0.get_uniform_location(prog, name)
    }
    fn uniform_4fv(&mut self, location: i32, v: &[f32; 4]) {
        self.0.uniform_4fv(location, 1, v)
    }
    unsafe fn buffer_data(&mut self, target: u32, size: isize, data: *const c_void) {
        unsafe { self.0.buffer_data(target, size, data, 0) }
    }
    fn draw(&mut self, mode: u32, count: i32, ty: u32) {
        self.0.draw_elements_instanced(mode, count, ty, 0, 1)
    }
    fn clear_color(&mut self, c: [f32; 4]) {
        self.0.clear_color(c[0], c[1], c[2], c[3])
    }
    fn clear_depth(&mut self, d: f64) {
        self.0.clear_depth(d)
    }
    fn clear(&mut self, mask: u32) {
        self.0.clear(mask)
    }
    fn enable(&mut self, cap: u32) {
        self.0.enable(cap)
    }
    fn depth_func(&mut self, f: u32) {
        self.0.depth_func(f)
    }
    fn depth_mask(&mut self, m: u8) {
        self.0.depth_mask(m)
    }
    fn blend_func(&mut self, s: u32, d: u32) {
        self.0.blend_func(s, d, s, d)
    }
    fn scissor(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.0.set_scissor(x, y, w, h)
    }
    fn bind_texture_2d(&mut self, texture: u32) {
        self.0.bind_texture(GL_TEXTURE_2D, texture)
    }
    fn flush(&mut self) {
        self.0.get_color_buffer(0, true);
    }
}

struct Cxx(*mut c_void);

impl Drop for Cxx {
    fn drop(&mut self) {
        unsafe { cxx::DestroyContext(self.0) }
    }
}

impl Gl for Cxx {
    fn setup(&mut self, name: &str) -> u32 {
        unsafe {
            let name = CString::new(name).unwrap();
            let prog = cxx::CreateProgram();
            for ty in [GL_VERTEX_SHADER, GL_FRAGMENT_SHADER] {
                let s = cxx::CreateShader(ty);
                cxx::ShaderSourceByName(s, name.as_ptr());
                cxx::AttachShader(prog, s);
            }
            cxx::LinkProgram(prog);
            assert_eq!(cxx::GetLinkStatus(prog), 1, "gl.cc links the program");
            cxx::UseProgram(prog);
            let mut vao = 0;
            cxx::GenVertexArrays(1, &mut vao);
            cxx::BindVertexArray(vao);
            let mut ids = [0u32; 2];
            cxx::GenBuffers(2, ids.as_mut_ptr());
            cxx::BindBuffer(GL_ARRAY_BUFFER, ids[0]);
            cxx::BindBuffer(GL_ELEMENT_ARRAY_BUFFER, ids[1]);
            cxx::VertexAttribPointer(0, 2, GL_FLOAT, 0, 8, std::ptr::null());
            cxx::EnableVertexAttribArray(0);
            prog
        }
    }
    fn uniform_location(&mut self, prog: u32, name: &str) -> i32 {
        let name = CString::new(name).unwrap();
        unsafe { cxx::GetUniformLocation(prog, name.as_ptr()) }
    }
    fn uniform_4fv(&mut self, location: i32, v: &[f32; 4]) {
        unsafe { cxx::Uniform4fv(location, 1, v.as_ptr()) }
    }
    unsafe fn buffer_data(&mut self, target: u32, size: isize, data: *const c_void) {
        unsafe { cxx::BufferData(target, size, data, 0) }
    }
    fn draw(&mut self, mode: u32, count: i32, ty: u32) {
        unsafe { cxx::DrawElementsInstanced(mode, count, ty, 0, 1) }
    }
    fn clear_color(&mut self, c: [f32; 4]) {
        unsafe { cxx::ClearColor(c[0], c[1], c[2], c[3]) }
    }
    fn clear_depth(&mut self, d: f64) {
        unsafe { cxx::ClearDepth(d) }
    }
    fn clear(&mut self, mask: u32) {
        unsafe { cxx::Clear(mask) }
    }
    fn enable(&mut self, cap: u32) {
        unsafe { cxx::Enable(cap) }
    }
    fn depth_func(&mut self, f: u32) {
        unsafe { cxx::DepthFunc(f) }
    }
    fn depth_mask(&mut self, m: u8) {
        unsafe { cxx::DepthMask(m) }
    }
    fn blend_func(&mut self, s: u32, d: u32) {
        unsafe { cxx::BlendFunc(s, d, s, d) }
    }
    fn scissor(&mut self, x: i32, y: i32, w: i32, h: i32) {
        unsafe { cxx::SetScissor(x, y, w, h) }
    }
    fn bind_texture_2d(&mut self, texture: u32) {
        unsafe { cxx::BindTexture(GL_TEXTURE_2D, texture) }
    }
    fn flush(&mut self) {
        let (mut w, mut h, mut s) = (0, 0, 0);
        unsafe { cxx::GetColorBuffer(0, 1, &mut w, &mut h, &mut s) };
    }
}

// ---- scripts ---------------------------------------------------------------------------------------------------

const W: i32 = 32;
const H: i32 = 32;

/// How [`compare_with`] draws: the framebuffer size, the program, and the Rust context's threads and split threshold
/// (`Context::set_raster_split_pixels`).
#[derive(Clone, Copy, Debug)]
struct Cfg {
    w: i32,
    h: i32,
    program: &'static str,
    threads: usize,
    split: i64,
    /// Threads 2.. sleep a millisecond per stripe (`Context::set_raster_slow_workers`).
    slow: bool,
    /// Stripes dealt out statically, thread `t` of `n` taking `s % n == t` (`Context::set_raster_static_stripes`).
    static_stripes: bool,
}

impl Default for Cfg {
    fn default() -> Cfg {
        Cfg {
            w: W,
            h: H,
            program: "flat",
            threads: 1,
            split: lucinanet_raster::gl::RASTER_SPLIT_PIXELS,
            slow: false,
            static_stripes: false,
        }
    }
}

/// One backend drawing into its own framebuffer, which starts as a pattern (so pending clears show).
struct Scene<G: Gl> {
    gl: G,
    fb: Vec<u32>,
    color: i32,
    zw: i32,
}

impl<G: Gl> Scene<G> {
    fn quad(&mut self, r: [f32; 4], z: f32, c: [f32; 4], w: [f32; 4], tri: bool) {
        let [x0, y0, x1, y1] = r;
        // Vertexes 0 1 2 3 = (x0,y0) (x1,y0) (x0,y1) (x1,y1); indices 0 1 2 2 1 3 make the quad.
        let v: [f32; 8] = [x0, y0, x1, y0, x0, y1, x1, y1];
        let idx: [u16; 6] = [0, 1, 2, 2, 1, 3];
        self.set(z, c, w);
        unsafe {
            self.gl.buffer_data(GL_ARRAY_BUFFER, 32, v.as_ptr() as *const _);
            self.gl.buffer_data(GL_ELEMENT_ARRAY_BUFFER, 12, idx.as_ptr() as *const _);
        }
        self.gl.draw(GL_TRIANGLES, if tri { 3 } else { 6 }, GL_UNSIGNED_SHORT);
    }

    /// Vertexes straight from the array buffer (gl.cc's `GL_NONE` element type), as lines or triangles.
    fn arrays(&mut self, mode: u32, v: &[f32], z: f32, c: [f32; 4], w: [f32; 4]) {
        self.set(z, c, w);
        unsafe { self.gl.buffer_data(GL_ARRAY_BUFFER, (v.len() * 4) as isize, v.as_ptr() as *const _) };
        self.gl.draw(mode, (v.len() / 2) as i32, GL_NONE);
    }

    fn set(&mut self, z: f32, c: [f32; 4], w: [f32; 4]) {
        let mut zw = w;
        zw[0] = z;
        if w == [1.0; 4] {
            zw[1] = 0.0;
        }
        self.gl.uniform_4fv(self.color, &c);
        self.gl.uniform_4fv(self.zw, &zw);
    }

    fn clear(&mut self, depth: bool) {
        self.gl.clear_color([128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0]);
        self.gl.clear_depth(1.0);
        self.gl.clear(GL_COLOR_BUFFER_BIT | if depth { GL_DEPTH_BUFFER_BIT } else { 0 });
    }
}

/// Runs `script` on both, comparing the framebuffers after every step and after a final flush; returns the flushed
/// framebuffer and which steps changed it (so a script that draws nothing on both sides cannot pass).
fn compare(script: impl Fn(&mut dyn FnMut(&mut dyn Step))) -> (Vec<u32>, Vec<bool>) {
    let (rust, _, changed) = compare_with(Cfg::default(), &script, true);
    (rust, changed)
}

/// [`compare`] as `cfg` says. With `exact` false the framebuffers are not compared: both are returned (Rust's, gl.cc's).
/// The caller holds `ORACLE` (every test takes it first).
fn compare_with(cfg: Cfg, script: &dyn Fn(Steps<'_>), exact: bool) -> (Vec<u32>, Vec<u32>, Vec<bool>) {
    // gl.cc keeps its current context in a static: a script run without the lock draws into another test's context.
    assert!(ORACLE.try_lock().is_err(), "compare_with needs the caller to hold ORACLE");
    let (w, h) = (cfg.w, cfg.h);
    let mut ctx = Context::new();
    ctx.set_raster_threads(cfg.threads);
    ctx.set_raster_split_pixels(cfg.split);
    ctx.set_raster_static_stripes(cfg.static_stripes);
    if cfg.slow {
        ctx.set_raster_slow_workers(Some((2, std::time::Duration::from_millis(1))));
    }
    let mut rust = new_scene(Rust(ctx), cfg, |s| unsafe {
        let p = s.fb.as_mut_ptr() as *mut c_void;
        s.gl.0.init_default_framebuffer(0, 0, w, h, w * 4, p);
        s.gl.0.set_viewport(0, 0, w, h);
    });
    let mut oracle = new_scene(
        unsafe {
            let c = cxx::CreateContext();
            cxx::MakeCurrent(c);
            Cxx(c)
        },
        cfg,
        |s| unsafe {
            cxx::InitDefaultFramebuffer(0, 0, w, h, w * 4, s.fb.as_mut_ptr() as *mut c_void);
            cxx::SetViewport(0, 0, w, h);
        },
    );
    let (mut n, mut changed) = (0, Vec::new());
    script(&mut |step: &mut dyn Step| {
        let before = rust.fb.clone();
        step.run(&mut rust as &mut dyn AnyScene, &mut oracle as &mut dyn AnyScene);
        n += 1;
        if exact {
            diff(&rust.fb, &oracle.fb, w, &format!("{cfg:?}, after step {n}"));
        }
        changed.push(rust.fb != before);
    });
    rust.gl.flush();
    oracle.gl.flush();
    if exact {
        diff(&rust.fb, &oracle.fb, w, &format!("{cfg:?}, after the flush"));
    }
    (rust.fb.clone(), oracle.fb.clone(), changed)
}

/// How many pixels hold `c` (as BGRA8 bytes in a `u32`).
fn count(fb: &[u32], c: u32) -> usize {
    fb.iter().filter(|&&p| p == c).count()
}

const RED8: u32 = 0xFF_FF0000;
const GREEN8: u32 = 0xFF_00FF00;
const BLUE8: u32 = 0xFF_0000FF;

fn new_scene<G: Gl>(gl: G, cfg: Cfg, init: impl FnOnce(&mut Scene<G>)) -> Scene<G> {
    let mut s = Scene { gl, fb: vec![0x1234_5678u32; (cfg.w * cfg.h) as usize], color: -1, zw: -1 };
    init(&mut s);
    let prog = s.gl.setup(cfg.program);
    s.color = s.gl.uniform_location(prog, "uColor");
    s.zw = s.gl.uniform_location(prog, "uZW");
    assert_eq!((s.color, s.zw), (0, 1));
    s
}

fn diff(rust: &[u32], cxx: &[u32], w: i32, when: &str) {
    let bad: Vec<String> = (0..rust.len())
        .filter(|&i| rust[i] != cxx[i])
        .take(8)
        .map(|i| format!("({}, {}): rust {:08x} gl.cc {:08x}", i as i32 % w, i as i32 / w, rust[i], cxx[i]))
        .collect();
    assert!(bad.is_empty(), "{when}: {} pixels differ, first {bad:?}", rust.iter().zip(cxx).filter(|(a, b)| a != b).count());
}

/// The scenes both backends run, behind one object-safe interface.
trait AnyScene {
    fn quad(&mut self, r: [f32; 4], z: f32, c: [f32; 4], w: [f32; 4], tri: bool);
    fn arrays(&mut self, mode: u32, v: &[f32], z: f32, c: [f32; 4], w: [f32; 4]);
    fn clear(&mut self, depth: bool);
    fn gl(&mut self) -> &mut dyn Gl;
    fn fb(&mut self) -> &mut [u32];
}

impl<G: Gl> AnyScene for Scene<G> {
    fn quad(&mut self, r: [f32; 4], z: f32, c: [f32; 4], w: [f32; 4], tri: bool) {
        Scene::quad(self, r, z, c, w, tri)
    }
    fn arrays(&mut self, mode: u32, v: &[f32], z: f32, c: [f32; 4], w: [f32; 4]) {
        Scene::arrays(self, mode, v, z, c, w)
    }
    fn clear(&mut self, depth: bool) {
        Scene::clear(self, depth)
    }
    fn gl(&mut self) -> &mut dyn Gl {
        &mut self.gl
    }
    fn fb(&mut self) -> &mut [u32] {
        &mut self.fb
    }
}

/// What a script gets: the function that runs each of its steps.
type Steps<'a> = &'a mut dyn FnMut(&mut dyn Step);

/// One step of a script, run on each backend in turn.
trait Step {
    fn run(&mut self, a: &mut dyn AnyScene, b: &mut dyn AnyScene);
}

impl<F: FnMut(&mut dyn AnyScene)> Step for F {
    fn run(&mut self, a: &mut dyn AnyScene, b: &mut dyn AnyScene) {
        self(a);
        self(b);
    }
}

const ONE: [f32; 4] = [1.0; 4];
const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

fn ndc(p: f32) -> f32 {
    p / 16.0 - 1.0
}

// ---- the scripts --------------------------------------------------------------------------------------------

fn script_quads_and_the_delayed_clear(step: &mut dyn FnMut(&mut dyn Step)) {
    step(&mut |s: &mut dyn AnyScene| s.clear(false));
    step(&mut |s: &mut dyn AnyScene| s.quad([-0.5, -0.5, 0.5, 0.5], 0.0, RED, ONE, false));
    step(&mut |s: &mut dyn AnyScene| s.quad([-1.2, 0.7, 1.3, 1.4], 0.0, GREEN, ONE, false));
    step(&mut |s: &mut dyn AnyScene| s.quad([0.3, -0.9, 0.36, 0.2], 0.0, BLUE, ONE, false));
}

fn script_fractional_edges(step: &mut dyn FnMut(&mut dyn Step)) {
    step(&mut |s: &mut dyn AnyScene| {
        s.clear(false);
        s.gl().flush();
    });
    for (x0, x1, y0, y1) in [(8.4, 8.6, 4.0, 6.0), (8.4, 9.6, 4.0, 6.0), (10.5, 12.5, 7.5, 9.5), (13.49, 13.51, 1.0, 3.0)] {
        step(&mut |s: &mut dyn AnyScene| s.quad([ndc(x0), ndc(y0), ndc(x1), ndc(y1)], 0.0, GREEN, ONE, false));
    }
    step(&mut |s: &mut dyn AnyScene| {
        s.quad([ndc(20.3), ndc(20.7), ndc(29.9), ndc(30.2)], 0.0, RED, ONE, true)
    });
}

fn script_depth_test_and_mask(step: &mut dyn FnMut(&mut dyn Step)) {
    step(&mut |s: &mut dyn AnyScene| {
        s.gl().enable(GL_DEPTH_TEST);
        s.gl().depth_func(GL_LESS);
        s.clear(true);
    });
    step(&mut |s: &mut dyn AnyScene| s.quad([-1.0, -1.0, 0.0, 0.0], 0.0, RED, ONE, false));
    step(&mut |s: &mut dyn AnyScene| s.quad([-0.5, -0.5, 0.5, 0.5], 0.5, BLUE, ONE, false));
    step(&mut |s: &mut dyn AnyScene| s.quad([-0.25, -0.25, 0.25, 0.25], -0.5, GREEN, ONE, false));
    step(&mut |s: &mut dyn AnyScene| {
        s.gl().depth_mask(0);
        s.quad([-0.25, -0.25, 0.25, 0.25], -0.9, RED, ONE, false);
        s.gl().depth_mask(1);
    });
    step(&mut |s: &mut dyn AnyScene| s.quad([-0.25, -0.25, 0.25, 0.25], -0.6, BLUE, ONE, false));
    step(&mut |s: &mut dyn AnyScene| {
        s.gl().depth_func(GL_LEQUAL);
        s.quad([-0.75, -0.75, 0.75, 0.75], -0.5, RED, ONE, false);
    });
}

fn script_triangles_lines_blend_and_scissor(step: &mut dyn FnMut(&mut dyn Step)) {
    step(&mut |s: &mut dyn AnyScene| s.clear(false));
    step(&mut |s: &mut dyn AnyScene| s.quad([-1.0, -1.0, 1.0, 1.0], 0.0, RED, ONE, true));
    step(&mut |s: &mut dyn AnyScene| {
        s.arrays(GL_TRIANGLES, &[0.1, 0.9, 0.95, 0.2, 0.4, -0.7, -0.3, 0.3, -0.9, 0.8, -0.6, -0.1], 0.0, GREEN, ONE)
    });
    step(&mut |s: &mut dyn AnyScene| {
        s.arrays(GL_LINES, &[-0.9, -0.9, 0.9, 0.5, -0.5, 0.8, -0.5, -0.8, 0.0, 0.25, 0.75, 0.25], 0.0, BLUE, ONE)
    });
    step(&mut |s: &mut dyn AnyScene| {
        s.gl().enable(GL_BLEND);
        s.gl().blend_func(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
        s.quad([-0.6, -0.6, 0.6, 0.6], 0.0, [0.1, 0.2, 0.45, 0.5], ONE, false);
    });
    step(&mut |s: &mut dyn AnyScene| {
        s.gl().enable(GL_SCISSOR_TEST);
        s.gl().scissor(5, 7, 13, 9);
        s.quad([-1.0, -1.0, 1.0, 1.0], 0.0, [0.6, 0.07, 0.2, 0.7], ONE, false);
    });
}

fn script_perspective(step: &mut dyn FnMut(&mut dyn Step)) {
    step(&mut |s: &mut dyn AnyScene| {
        s.clear(false);
        s.gl().flush();
    });
    step(&mut |s: &mut dyn AnyScene| s.quad([-0.5, -0.5, 0.5, 0.5], 0.0, BLUE, [1.0, 2.0, 1.0, 2.0], false));
    step(&mut |s: &mut dyn AnyScene| s.quad([-0.9, 0.1, 0.8, 0.95], 0.2, RED, [1.0, 1.5, 3.0, 0.75], false));
    step(&mut |s: &mut dyn AnyScene| s.quad([-0.7, -0.9, 0.2, 0.0], 0.0, GREEN, [1.0, 0.5, 4.0, 2.0], true));
}

fn script_perspective_behind_the_camera(step: &mut dyn FnMut(&mut dyn Step)) {
    step(&mut |s: &mut dyn AnyScene| {
        s.clear(false);
        s.gl().flush();
    });
    step(&mut |s: &mut dyn AnyScene| s.quad([-0.5, -0.5, 0.5, 0.5], 0.0, GREEN, [1.0, -1.0, 1.0, 2.0], false));
    step(&mut |s: &mut dyn AnyScene| s.quad([-0.8, -0.3, 0.6, 0.9], 0.1, RED, [1.0, 2.0, -0.5, -1.0], false));
    step(&mut |s: &mut dyn AnyScene| s.quad([-0.2, -0.8, 0.9, 0.4], 0.0, BLUE, [1.0, 1.0, 1.0, 0.0], false));
}

// ---- one thread, 32 by 32 -------------------------------------------------------------------------------------

#[test]
fn quads_and_the_delayed_clear() {
    let _g = ORACLE.lock().unwrap_or_else(|e| e.into_inner());
    let (fb, changed) = compare(script_quads_and_the_delayed_clear);
    assert_eq!(changed, [false, true, true, true], "the clear is delayed");
    assert_eq!(count(&fb, RED8), 16 * 16 - 11, "the blue column covers 11 of the red pixels");
    assert!(count(&fb, GREEN8) > 0 && count(&fb, BLUE8) > 0);
}

#[test]
fn fractional_edges() {
    let _g = ORACLE.lock().unwrap_or_else(|e| e.into_inner());
    let (fb, changed) = compare(script_fractional_edges);
    assert_eq!(changed, [true; 6], "every quad draws on gl.cc, the 8.4..8.6 one included");
    assert!(count(&fb, GREEN8) > 0 && count(&fb, RED8) > 0);
}

#[test]
fn depth_test_and_mask() {
    let _g = ORACLE.lock().unwrap_or_else(|e| e.into_inner());
    let (fb, changed) = compare(script_depth_test_and_mask);
    assert_eq!(changed, [false, true, true, true, true, true, true], "the clear is delayed");
    assert!(count(&fb, RED8) > 0 && count(&fb, BLUE8) > 0);
}

#[test]
fn triangles_lines_blend_and_scissor() {
    let _g = ORACLE.lock().unwrap_or_else(|e| e.into_inner());
    let (fb, changed) = compare(script_triangles_lines_blend_and_scissor);
    assert_eq!(changed, [false, true, true, true, true, true], "the clear is delayed");
    assert!(count(&fb, RED8) > 0 && count(&fb, GREEN8) > 0);
}

#[test]
fn perspective() {
    let _g = ORACLE.lock().unwrap_or_else(|e| e.into_inner());
    let (fb, changed) = compare(script_perspective);
    assert_eq!(changed, [true; 4]);
    assert!(count(&fb, BLUE8) > 0 && count(&fb, RED8) > 0 && count(&fb, GREEN8) > 0);
}

#[test]
fn perspective_behind_the_camera() {
    let _g = ORACLE.lock().unwrap_or_else(|e| e.into_inner());
    let (fb, changed) = compare(script_perspective_behind_the_camera);
    assert_eq!(changed, [true; 4]);
    assert!(count(&fb, GREEN8) > 0);
}

// ---- several threads (raster/DESIGN.md, "Parallel rasterizing") ------------------------------------------------

/// A script, as a function.
type Script = fn(Steps<'_>);

const SCRIPTS: [(&str, Script); 6] = [
    ("quads_and_the_delayed_clear", script_quads_and_the_delayed_clear),
    ("fractional_edges", script_fractional_edges),
    ("depth_test_and_mask", script_depth_test_and_mask),
    ("triangles_lines_blend_and_scissor", script_triangles_lines_blend_and_scissor),
    ("perspective", script_perspective),
    ("perspective_behind_the_camera", script_perspective_behind_the_camera),
];

/// Every script with 1, 2, 3 and 8 threads, every draw split (threshold 0), on targets of one stripe and of several
/// (200 rows: the last stripe is partial). Flat's interpolants are constant, so every byte must equal gl.cc's.
#[test]
fn flat_scripts_on_several_threads() {
    let _g = ORACLE.lock().unwrap_or_else(|e| e.into_inner());
    for (w, h) in [(32, 32), (48, 200)] {
        for threads in [1, 2, 3, 8] {
            for (name, script) in SCRIPTS {
                let cfg = Cfg { w, h, threads, split: 0, ..Cfg::default() };
                let (_, _, changed) = compare_with(cfg, &script, true);
                assert!(changed.iter().any(|&c| c), "{name} {cfg:?} draws something");
            }
        }
    }
}

/// The names of this process's threads (Linux).
fn thread_names() -> Vec<String> {
    std::fs::read_dir("/proc/self/task")
        .map(|d| d.filter_map(|e| std::fs::read_to_string(e.ok()?.path().join("comm")).ok()).map(|n| n.trim().to_owned()).collect())
        .unwrap_or_default()
}

/// A split draw runs on `RasterWorker#1..7`, kept by the context between draws, and blending on them proves each row is
/// drawn once: a row drawn by two threads would blend twice.
#[test]
#[cfg(target_os = "linux")]
fn workers_are_named_and_persistent() {
    let _g = ORACLE.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = Cfg { w: 64, h: 256, threads: 8, split: 0, ..Cfg::default() };
    let mut ctx = Context::new();
    ctx.set_raster_threads(8);
    ctx.set_raster_split_pixels(0);
    let mut s = new_scene(Rust(ctx), cfg, |s| unsafe {
        let p = s.fb.as_mut_ptr() as *mut c_void;
        s.gl.0.init_default_framebuffer(0, 0, cfg.w, cfg.h, cfg.w * 4, p);
        s.gl.0.set_viewport(0, 0, cfg.w, cfg.h);
    });
    // The pools of earlier tests are joined, but under load the kernel may list their exited threads a little longer.
    let t0 = std::time::Instant::now();
    while thread_names().iter().any(|n| n.starts_with("RasterWorker#")) && t0.elapsed() < std::time::Duration::from_secs(2) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!thread_names().iter().any(|n| n.starts_with("RasterWorker#")), "no pool before a split draw");
    s.quad([-1.0, -1.0, 1.0, 1.0], 0.0, RED, ONE, false);
    let names = thread_names();
    for n in 1..8 {
        assert!(names.contains(&format!("RasterWorker#{n}")), "RasterWorker#{n} in {names:?}");
    }
    let before = names.iter().filter(|n| n.starts_with("RasterWorker#")).count();
    s.quad([-1.0, -1.0, 1.0, 1.0], 0.0, GREEN, ONE, false);
    assert_eq!(thread_names().iter().filter(|n| n.starts_with("RasterWorker#")).count(), before, "no new threads");
    assert_eq!(count(&s.fb, GREEN8), (cfg.w * cfg.h) as usize);
}

/// The largest difference of any channel, and how many pixels differ.
fn channel_diff(a: &[u32], b: &[u32]) -> (u32, usize) {
    let mut worst = 0;
    let mut n = 0;
    for (&x, &y) in a.iter().zip(b) {
        if x != y {
            n += 1;
        }
        for s in [0, 8, 16, 24] {
            worst = worst.max(((x >> s) & 0xFF).abs_diff((y >> s) & 0xFF));
        }
    }
    (worst, n)
}

/// A colour interpolated across quads that cross stripes: every thread steps the edges and interpolants from the
/// primitive's top, as one thread does, so the bytes equal gl.cc's with any number of threads.
#[test]
fn interpolated_colour_on_several_threads() {
    let _g = ORACLE.lock().unwrap_or_else(|e| e.into_inner());
    let script = |step: &mut dyn FnMut(&mut dyn Step)| {
        step(&mut |s: &mut dyn AnyScene| s.clear(false));
        step(&mut |s: &mut dyn AnyScene| s.quad([-0.9, -0.95, 0.8, 0.9], 0.0, ONE, ONE, false));
        step(&mut |s: &mut dyn AnyScene| s.quad([-0.3, -0.7, 0.95, 0.6], 0.0, ONE, ONE, true));
        step(&mut |s: &mut dyn AnyScene| {
            s.gl().enable(GL_BLEND);
            s.gl().blend_func(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
            s.quad([-1.0, -0.4, 0.5, 1.0], 0.0, ONE, ONE, false);
        });
    };
    let base = Cfg { w: 64, h: 256, program: "grad", split: 0, ..Cfg::default() };
    let (one, cxx, _) = compare_with(base, &script, true);
    assert!(one.iter().any(|&p| p != one[0]), "the gradient draws");
    for threads in [2, 3, 8] {
        let (rust, _, _) = compare_with(Cfg { threads, ..base }, &script, true);
        assert!(rust == cxx, "{threads} threads: {} pixels differ from gl.cc", channel_diff(&rust, &cxx).1);
    }
}

/// 200 random overlapping blended quads: 8 threads draw exactly what one does.
#[test]
fn random_blended_quads_on_eight_threads() {
    let _g = ORACLE.lock().unwrap_or_else(|e| e.into_inner());
    let script = |step: &mut dyn FnMut(&mut dyn Step)| {
        let mut seed = 0x2545_f491_u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32
        };
        step(&mut |s: &mut dyn AnyScene| {
            s.clear(false);
            s.gl().enable(GL_BLEND);
            s.gl().blend_func(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
        });
        for _ in 0..200 {
            let (x0, y0) = (rnd() * 2.2 - 1.1, rnd() * 2.2 - 1.1);
            let (x1, y1) = (x0 + rnd() * 1.2, y0 + rnd() * 1.2);
            let a = rnd() * 0.9 + 0.05;
            let c = [rnd() * a, rnd() * a, rnd() * a, a];
            let tri = rnd() < 0.3;
            step(&mut |s: &mut dyn AnyScene| s.quad([x0, y0, x1, y1], 0.0, c, ONE, tri));
        }
    };
    for program in ["flat", "grad"] {
        let base = Cfg { w: 160, h: 300, program, split: 0, ..Cfg::default() };
        let (one, _, _) = compare_with(base, &script, true);
        let (eight, _, _) = compare_with(Cfg { threads: 8, ..base }, &script, true);
        assert!(one == eight, "{program}: 8 threads differ from 1 in {} pixels", channel_diff(&one, &eight).1);
    }
}

/// Slow cores (the SC7180's A55s at low clocks): threads 2..8 sleep per stripe they take. Claimed stripes let the
/// two fast threads draw most of the 64, while stripes dealt out statically give each of the seven slow threads 8.
/// Both draw every stripe exactly once and give gl.cc's bytes.
#[test]
fn claimed_stripes_do_not_wait_for_slow_threads() {
    let slow = std::time::Duration::from_millis(20);
    let cfg = Cfg { w: 64, h: 32 * 64, threads: 8, split: 0, ..Cfg::default() };
    let draw = |static_stripes: bool| {
        let _g = ORACLE.lock().unwrap_or_else(|e| e.into_inner());
        let mut ctx = Context::new();
        ctx.set_raster_threads(8);
        ctx.set_raster_split_pixels(0);
        ctx.set_raster_static_stripes(static_stripes);
        ctx.set_raster_slow_workers(Some((2, slow)));
        let mut s = new_scene(Rust(ctx), cfg, |s| unsafe {
            let p = s.fb.as_mut_ptr() as *mut c_void;
            s.gl.0.init_default_framebuffer(0, 0, cfg.w, cfg.h, cfg.w * 4, p);
            s.gl.0.set_viewport(0, 0, cfg.w, cfg.h);
        });
        s.clear(false);
        let t0 = std::time::Instant::now();
        s.quad([-1.0, -1.0, 1.0, 1.0], 0.0, RED, ONE, false);
        let t = t0.elapsed();
        assert_eq!(count(&s.fb, RED8), (cfg.w * cfg.h) as usize, "static {static_stripes}: every pixel drawn once");
        (s.gl.0.raster_last_split_stripes(), t)
    };
    let (claimed_c, claimed_t) = draw(false);
    let (dealt_c, dealt_t) = draw(true);
    println!("64 stripes, threads 2..8 sleeping {slow:?} a stripe: claimed {claimed_c:?} ({claimed_t:?}), static {dealt_c:?} ({dealt_t:?})");
    let stripes = (cfg.h / 32) as u32;
    for (name, counts) in [("claimed", &claimed_c), ("static", &dealt_c)] {
        assert_eq!(counts.iter().sum::<u32>(), stripes, "{name}: the 64 stripes are all drawn once");
    }
    // Static dealing is round-robin: 64 stripes over 8 workers is exactly 8 each.
    assert!(dealt_c.iter().all(|&k| k == 8), "static stripes are dealt evenly: {dealt_c:?}");
    // Claimed: the slow workers (2..8) take fewer than half of the stripes the static dealing gives them.
    let slow_share: u32 = dealt_c[2..].iter().sum();
    let slow_claimed: u32 = claimed_c[2..].iter().sum();
    assert!(slow_claimed * 2 < slow_share, "slow workers took {slow_claimed} of their {slow_share} static share");
}

/// The scripts again with claimed stripes and slow threads: the order of claims changes nothing in the bytes.
#[test]
fn flat_scripts_with_slow_threads() {
    let _g = ORACLE.lock().unwrap_or_else(|e| e.into_inner());
    for (name, script) in SCRIPTS {
        let cfg = Cfg { w: 48, h: 200, threads: 8, split: 0, slow: true, ..Cfg::default() };
        let (_, _, changed) = compare_with(cfg, &script, true);
        assert!(changed.iter().any(|&c| c), "{name} draws something");
    }
}

/// A draw that samples its own colour target (a feedback loop, undefined in GL): swgl draws it on one thread, rows in
/// order, each reading the row above as it was just written. With 8 threads and every draw split, the Rust context
/// draws it on the calling thread alone and must equal gl.cc, every row the rotation of the one above.
#[test]
fn feedback_loop_on_eight_threads() {
    let _g = ORACLE.lock().unwrap_or_else(|e| e.into_inner());
    let (w, h) = (64, 256);
    let script = |step: &mut dyn FnMut(&mut dyn Step)| {
        step(&mut |s: &mut dyn AnyScene| {
            for (i, p) in s.fb().iter_mut().enumerate() {
                *p = 0xFF00_0000 | (i as u32 % 64).wrapping_mul(0x0003_0507);
            }
        });
        // Texture 1 is the default framebuffer's colour (`init_default_framebuffer` made it first, on both).
        step(&mut |s: &mut dyn AnyScene| s.gl().bind_texture_2d(1));
        // Rows 1.. read the row above; the quad leaves row 0, so nothing reads outside the texture.
        step(&mut |s: &mut dyn AnyScene| s.quad([-1.0, -1.0 + 2.0 / h as f32, 1.0, 1.0], 0.0, ONE, ONE, false));
    };
    let base = Cfg { w, h, program: "feedback", split: 0, ..Cfg::default() };
    let (one, cxx, changed) = compare_with(base, &script, true);
    assert_eq!(changed, [true, false, true]);
    let rot = |p: u32| (p & 0xFF00_0000) | ((p & 0xFF_FFFF) << 8 & 0xFF_FFFF) | ((p >> 16) & 0xFF);
    for y in 1..h as usize {
        let (row, above) = (&one[y * w as usize..][..w as usize], &one[(y - 1) * w as usize..][..w as usize]);
        assert!(row.iter().zip(above).all(|(&p, &q)| p == rot(q)), "row {y} is the row above rotated");
    }
    let (eight, _, _) = compare_with(Cfg { threads: 8, ..base }, &script, true);
    assert!(eight == cxx, "8 threads: {} pixels differ from gl.cc", channel_diff(&eight, &cxx).1);
}

/// A draw that samples its own depth target while it depth-tests and writes it. swgl has no depth sampler: debug gl.cc
/// stops on `assert(0)` in `gl_format_to_texture_format` (swgl/src/gl.cc:297) for `GL_DEPTH_COMPONENT24`, as the Rust
/// does (texture.rs `gl_format_to_texture_format`), and release gl.cc falls back to `RGBA8`, so the sampler reads the
/// depth words (`DepthRun`s) as RGBA8 texels. That release case is the one tested, and so only without debug asserts.
///
/// A cleared depth row holds one run header, its other words are whatever the allocation held (`init_depth_runs`,
/// swgl/src/rasterize.h:261), so the rows sampled are first flattened, each word its pixel's depth: a perspective quad
/// (sampling the colour target) over rows 1.. at depth 0.5 does that. Then unit 0 holds the depth attachment and a
/// quad over rows 2.. at depth 0 colours each pixel with the depth word of the row above: row 2 reads row 1 (the
/// first quad's), every later row a row this draw has just written. With 8 threads and every draw split it must
/// equal gl.cc. A stale read stays in its row (unlike the colour loop's, which every row below copies), so a split
/// draw would differ only where it lost a race; the run with static stripes and slow threads 2.. makes it lose one:
/// the calling thread draws stripe 8 right after stripe 0, while thread 7 still sleeps before stripe 7.
#[test]
#[cfg_attr(debug_assertions, ignore = "gl.cc asserts on sampling a depth texture (swgl/src/gl.cc:297); release only")]
fn depth_feedback_loop_on_eight_threads() {
    let _g = ORACLE.lock().unwrap_or_else(|e| e.into_inner());
    let (w, h) = (64, 512);
    let rows_from = |y: i32| [-1.0, -1.0 + 2.0 * y as f32 / h as f32, 1.0, 1.0];
    let script = |step: &mut dyn FnMut(&mut dyn Step)| {
        step(&mut |s: &mut dyn AnyScene| {
            s.clear(true);
            s.gl().enable(GL_DEPTH_TEST);
            s.gl().depth_func(GL_LESS);
            s.gl().depth_mask(1);
        });
        // Texture 1 is the default framebuffer's colour, texture 2 its depth (`init_default_framebuffer` made them
        // in that order, on both). Rows 1.. read the row above, so nothing reads outside the texture.
        step(&mut |s: &mut dyn AnyScene| {
            s.gl().bind_texture_2d(1);
            s.quad(rows_from(1), 0.5, ONE, [1.0, 2.0, 1.0, 2.0], false);
        });
        step(&mut |s: &mut dyn AnyScene| {
            s.gl().bind_texture_2d(2);
            s.quad(rows_from(2), 0.0, ONE, ONE, false);
        });
    };
    let base = Cfg { w, h, program: "feedback", split: 0, ..Cfg::default() };
    let (one, cxx, changed) = compare_with(base, &script, true);
    // The colour clear is delayed: it shows with the first draw.
    assert_eq!(changed, [false, true, true]);
    let row = |fb: &[u32], y: usize| fb[y * w as usize..][..w as usize].to_vec();
    assert_ne!(row(&one, 2), row(&one, 3), "row 2 reads the first quad's depth, row 3 the depth this draw wrote");
    for y in 4..h as usize {
        assert_eq!(row(&one, y), row(&one, 3), "row {y} reads the depth this draw wrote in the row above");
    }
    for cfg in [Cfg { threads: 8, ..base }, Cfg { threads: 8, slow: true, static_stripes: true, ..base }] {
        let (eight, _, _) = compare_with(cfg, &script, true);
        assert!(eight == cxx, "{cfg:?}: {} pixels differ from gl.cc", channel_diff(&eight, &cxx).1);
    }
}
