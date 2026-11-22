# glsl-to-rust

WebRender's GLSL shaders to vectorized Rust, for `raster/core` (`raster/DESIGN.md`). A fork of
[glsl-to-cxx](https://crates.io/crates/glsl-to-cxx) 0.70 (MPL-2.0), the translator swgl's `build.rs` runs: same
input (the shader list, preprocessing and `#define`s of swgl's build script), same walk over the same HIR, and an
output that reads side by side with the C++ header of the same program. Struct names, function names, locals,
temporaries (`_c3_`, `ret_mask`, `_cond_mask_`, `<sampler>_<base>_fetch`) and their numbering are the C++'s.

```
glsl-to-rust <name>.vert <name>.frag        # prints the module, like glsl-to-cxx
```

From a build script (raster/core's, later): `glsl_to_rust::build::write_shaders(shader_dir, out_dir)` writes
`<out_dir>/<program>.rs` for every program and `<out_dir>/shaders.rs` (one `mod` per program and `load_shader`, the
Rust form of swgl's `load_shader.h`), and returns the `.glsl` files read for `cargo:rerun-if-changed`. Lower level:
`build::shader_keys()`, `build::preprocess()`, `build::translate_program()`, and `translate()` (glsl-to-cxx's entry
point). `cargo test --release` translates all programs into `$CARGO_TARGET_DIR/emitted/`, parses each with `syn` and
regenerates the inventory at the end of this file.

## What changed from glsl-to-cxx

- `src/hir.rs` is glsl-to-cxx's, with two changes listed at its top: `FunctionSignature`'s fields are public (the
  emitter re-finds the overload a native call resolved to, to spell out implicit conversions), and a `bvec`
  swizzle is typed `bool`/`bvecN` instead of `float`/`vecN`.
- `src/lib.rs` is a new emitter. It keeps glsl-to-cxx's traversal (entry points `main`, `swgl_drawSpanRGBA8`,
  `swgl_drawSpanR8`; one variant per vector mask, dependencies written before their callers; the dry run that finds
  conditional returns; the masking of vector `if`s and `switch`es) and replaces every place where C++ relied on
  overloading or implicit conversion with explicit code, using each expression's type and run class from the HIR.
- `src/build.rs` is the preprocessing half of swgl's `build.rs` as library functions (glsl-to-cxx had none).

## How each C++ construct is emitted

| C++ (glsl-to-cxx) | Rust (glsl-to-rust) |
|---|---|
| `struct X_common { struct Samplers ...; struct AttribLocations ...; }` | `Samplers`, `AttribLocations` (with `set_slot`, `bind_loc`, `get_loc`) and `X_common` at module level |
| `struct X_vert : VertexShaderImpl, X_common` | `struct X_vert { common: X_common, vs: VertexShaderState, <attributes, vector outputs> }`, `Deref<Target = X_common>` |
| `struct X_frag : FragmentShaderImpl, X_vert` | `struct X_frag { vert: X_vert, fs: FragmentShaderState, <vector inputs>, interp_step, interp_perspective }`, `Deref<Target = X_vert>` |
| `struct X_program : ProgramImpl, X_frag` | `struct X_program { frag: X_frag }`, `impl ProgramImpl`, `X_program::loader()` |
| members `gl_Position`, `gl_ClipDistance` | `self.vs.gl_Position`, `self.vs.gl_ClipDistance` |
| members `gl_FragCoord`, `gl_FragColor`, `swgl_SpanLength`, `swgl_IsPixelDiscarded`; `#define oFragColor gl_FragColor`, `#define oFragBlend gl_SecondaryFragColor` | `self.fs.<same name>` |
| a uniform, sampler, flat varying, attribute (`sColor0`, `v_color`) | `self.sColor0` (through `Deref` to the struct that holds it) |
| function pointer table filled in the constructor | trait methods (`run_func`, `skip_func`, `draw_span_rgba8_func`, `has_draw_span_rgba8`, ...); `enable_perspective()` & co in `X_frag::new()` |
| `DISPATCH_DRAW_SPAN(self, RGBA8)` | `fn draw_span_RGBA8(&mut self, ctx)` with the same body |
| shader function `T f(A a)` | method `fn f(&self, mut a: A) -> T` (`&mut self` when it writes the shader object, `ctx: &mut RasterState` first when it or a callee touches swgl's raster statics) |
| overloads of one function for different vector masks | one method per variant: the GLSL name for the all-scalar one (or the only one), `f_v0_2` for vector parameters 0 and 2, `_m` for the variant taking `_cond_mask_` |
| `ALWAYS_INLINE void main()` | `#[inline(always)] fn main(&mut self ...)` |
| `out`/`inout` parameter `T& p` | `p: &mut T`, read as `*p`; argument `&mut place`. A place on `self`, behind a multi-component swizzle, or a scalar argument of a vector parameter goes through a temporary (`_arg1_`, copied back with `force_scalar` where C++ adapted) |
| `float`, `int32_t`, `uint32_t`, `bool` / `Float`, `I32`, `U32`, `Bool` | `f32`, `i32`, `u32`, `bool` / `Float`, `I32`, `U32`, `Bool` |
| `vec4_scalar`, `vec4`, `mat3_scalar`, `ivec2`, ... | the same names |
| `Array<vec4_scalar,2>`, `Array<..>{{a, b}}` | `[vec4_scalar; 2]`, `[a, b]` |
| `0.5f`, `1.f`, `2u` | `0.5f32`, `1.0f32`, `2u32`; an int literal where C++ converted it to float is written as a float literal |
| implicit scalar to lanes (`vec4 v = vec4_scalar`, `Float + float`) | `vec4::from(x)`, `Float::from(0.5f32)` on the scalar operand |
| implicit int to float (`swgl_StepSize * vec4`, `clamp(v, 0.f, int)`) | `(x as f32)` for a scalar, `make_float(x)` for lanes |
| `force_scalar(e)` | `force_scalar(e)` |
| `a + b`, `a * b`, `a & b`, `m * v` | `(a) + (b)` etc. through `std::ops` on the runtime types (`Add`, `Mul`, `BitAnd`, `Shl`, ...); operands in the same run class, except a matrix product, which keeps C++'s `mat4_scalar * vec4` |
| `a < b` on lanes (a mask) | `lessThan(a, b)`, `greaterThan`, `lessThanEqual`, `greaterThanEqual`, `equal`, `notEqual` |
| `a == b` on vectors of lanes | `(a).cmp_eq(&b)` / `cmp_ne` (an `I32` mask, as C++'s `operator==`) |
| `a < b`, `a == b`, `a && b` on scalars | the same Rust operators |
| `a && b`, `a \|\| b`, `!a`, `~a` on masks | `(a) & (b)`, `(a) \| (b)`, `!(a)`, `!(a)` |
| `c ? a : b` (scalar `c`) | `if c { a } else { b }` |
| `if_then_else(c, a, b)` (lanes `c`) | `if_then_else(c, a, b)`, both values in the result's run class |
| `(v).x`, `(v).sel(X,Y)`, `(v).lsel(X,Y) = e` | `v.x`, `v.sel2(X, Y)` (`sel3`, `sel4`), `{ let _v_ = e; v.lsel2(X, Y).assign(_v_) }` |
| `v[i]`, `m[0]`, `a[i]` | `v[(i) as usize]`, `m[0]`, `a[(i) as usize]`; a lane index is `v.index_lanes(i)` |
| `make_vec4(a, b, c)`, `make_vec2(x)`, `make_float(x)` | `make_vec4((a, b, c))` (a tuple: the runtime's `make_*` take one argument), `make_vec2(x)`, `make_float(x)` |
| struct `S_scalar` / `S`, its constructors, `IMPLICIT S(S_scalar)`, `friend if_then_else` | `#[derive(Clone, Copy, Default)]` structs `S_scalar` / `S`, constructor functions named like the structs, `impl From<S_scalar> for S`, `impl IfThenElse<S> for I32` |
| `static constexpr` globals | associated `const` (literal initializer) or associated `#[inline(always)] fn` (`Self::RgbFromYuv_Rec709()`) |
| uninitialized local `vec4 x;` | `let mut x: vec4 = Default::default();` |
| `if` on lanes: `auto _c3_ = cond;` and masked assignments `x = if_then_else(_c3_, e, x)` | the same, `let mut _c3_ = cond;` |
| masked `if (_c3_[0]) { s = force_scalar(e); }` | `if (_c3_)[0] != 0 { s = force_scalar(e); }` |
| conditional returns: `I32 ret_mask = ~0; ret = if_then_else(ret_mask & I32(c), e, ret); ret_mask &= ~I32(c);` | `let mut ret_mask: I32 = I32::from(!0i32); ret = if_then_else(ret_mask & c, e, ret); ret_mask = ret_mask & !(c);` |
| `discard` | `self.fs.swgl_IsPixelDiscarded = ...` |
| `switch` on a scalar | a labeled block `'swN: { let _swN_ = head; if _swN_ == 1 \|\| _swN_ == 2 { .. } else if .. else { default } }`; `break` is `break 'swN` |
| `switch` on lanes | lowered to masked `if`s, as glsl-to-cxx does |
| `while`, `for (init; cond; post)`, `do .. while` | labeled `'lN: while`; a `for` is `{ init; 'lN: while cond { body; post } }` (with `continue` in the body the post expression moves into the condition block so `continue` still runs it); a `do .. while` is `'lN: loop { 'doN: { body } if !(cond) { break 'lN; } }`, `continue` being `break 'doN` |
| `texelFetchOffset(s, base, 0, ivec2(1, 0))` | `texelFetchUnchecked(self.s, s_base_fetch, 1, 0)` after `let s_base_fetch = texelFetchPtr(self.s, base, ..);`, as C++ |
| `swgl_commitTextureLinearRGBA8(s, p, r)` (a macro using `blend_key`, `swgl_OutRGBA8`, `swgl_SpanLength`) | `swgl_commitTextureLinearRGBA8(ctx, self.s, p, r, &mut self.fs)`: `ctx` first, the fragment shader state last |
| `swgl_antiAlias(e)`, `swgl_clipMask(..)`, `swgl_blendDropShadow(c)`, `swgl_blendSubpixelText(c)` | the same with `ctx` first |
| `swgl_stepInterp()`, `swgl_interpStep(v)` | `self.step_interp_inputs(4)`, `self.interp_step.v` |
| `InterpOutputs`, `store_interp_outputs(char*, size_t)` | `#[repr(C)] struct InterpOutputs` of `_scalar` fields, written with `store_interp(&mut dest[n * stride..], &out)` |
| `read_interp_inputs(impl, const void*, const void*)` | `fn read_interp_inputs(&mut self, init_: &[u8], step_: &[u8])` reading with `load_interp` |
| `self->v += interp_step.v * chunks` | `self.v = self.v + vec2::from(self.interp_step.v * chunks)` |
| `assert(0); // uTransform` in `set_uniform_*` | `debug_assert!(false, "uTransform")` (compiled out in release, as swgl's `assert`) |

## What the runtime provides

Each module starts with `use` lines naming every runtime item it takes, by module: `crate::glsl` (types, `make_*`,
built-ins, `if_then_else`, `force_scalar`, comparisons, `XYZW` constants), `crate::texture` (texel fetches,
sampling, `lookup_sampler`), `crate::ext` (the `swgl_*` intrinsics), `crate::program` (the traits, the shader
states, attribute and interpolant loading), plus `crate::RasterState`. The inventory below lists them all; its last
section lists the operator impls, `From` conversions, swizzles and call signatures (with argument types) the code
relies on, one per line, so each can be checked against the runtime.

The traits the emitted code implements, as `crate::program` must declare them (names from `program.h`, the shape of
the core's `program.rs`; the differences from that file as of this commit are `ctx` and the byte slices):

```rust
pub trait ProgramImpl: Send {
    fn get_uniform(&self, name: &str) -> i32;
    fn bind_attrib(&mut self, name: &str, index: i32);
    fn get_attrib(&self, name: &str) -> i32;
    fn interpolants_size(&self) -> usize;
    fn get_vertex_shader(&mut self) -> &mut dyn VertexShaderImpl;
    fn get_fragment_shader(&mut self) -> &mut dyn FragmentShaderImpl;
    fn get_name(&self) -> &str;
    fn box_clone(&self) -> Box<dyn ProgramImpl>;
}
pub trait VertexShaderImpl: Send {
    fn state(&self) -> &VertexShaderState;
    fn state_mut(&mut self) -> &mut VertexShaderState;
    fn set_uniform_1i(&mut self, index: i32, value: i32);
    fn set_uniform_4fv(&mut self, index: i32, value: &[f32]);
    fn set_uniform_matrix4fv(&mut self, index: i32, value: &[f32]);
    fn init_batch(&mut self, ctx: &mut RasterState);
    fn load_attribs(&mut self, attribs: &[VertexAttrib<'_>], start: u32, instance: i32, count: i32);
    /// Runs the vertex shader on 4 vertices and writes 4 `InterpOutputs`, `interp_stride` bytes apart.
    fn run_primitive(&mut self, ctx: &mut RasterState, interps: &mut [u8], interp_stride: usize);
    fn box_clone(&self) -> Box<dyn VertexShaderImpl>;
}
pub trait FragmentShaderImpl: Send {
    fn state(&self) -> &FragmentShaderState;
    fn state_mut(&mut self) -> &mut FragmentShaderState;
    /// `interps` and `step` hold one `InterpOutputs` each (`interpolants_size()` bytes).
    fn init_span_func(&mut self, interps: &[u8], step: &[u8]);
    fn run_func(&mut self, ctx: &mut RasterState);
    fn skip_func(&mut self, steps: i32);
    fn init_span_w_func(&mut self, interps: &[u8], step: &[u8]);
    fn run_w_func(&mut self, ctx: &mut RasterState);
    fn skip_w_func(&mut self, steps: i32);
    fn draw_span_rgba8_func(&mut self, ctx: &mut RasterState) -> i32;
    fn draw_span_r8_func(&mut self, ctx: &mut RasterState) -> i32;
    fn has_draw_span_rgba8(&self) -> bool;
    fn has_draw_span_r8(&self) -> bool;
    fn box_clone(&self) -> Box<dyn FragmentShaderImpl>;
}
```

plus `VertexShaderState` (`gl_Position`, `gl_ClipDistance`, `enable_clip_distance`), `FragmentShaderState`
(`gl_FragCoord`, `gl_FragColor`, `gl_SecondaryFragColor`, `swgl_StepZW`, `swgl_IsPixelDiscarded`, `swgl_OutRGBA8`,
`swgl_OutR8`, `swgl_SpanLength`, `enable_discard`, `enable_perspective`, `step_fragcoord`, `step_perspective`), both
`Clone + Default`; `NULL_ATTRIB`; `load_attrib(&mut T, &VertexAttrib, start, instance, count)` and
`load_flat_attrib`; `store_interp<T: Copy>(dest: &mut [u8], v: &T)` and `load_interp<T: Copy>(src: &[u8]) -> T`
(unaligned, length-checked); `ProgramLoader = fn() -> Box<dyn ProgramImpl>`; and `lookup_sampler(ctx, &mut
sampler2D_impl, slot) -> sampler2D` (`lookup_isampler` for `isampler2D`). Samplers (`sampler2D`, `sampler2DRect`,
`isampler2D`) and their `_impl` structs must be `Copy`/`Clone + Default`.

## Where the Rust differs from the C++ on purpose

- An `out`/`inout` argument that is a place on the shader object (or behind a multi-component swizzle) is copied in
  and out of a temporary, because the method call already borrows `self`. That is GLSL's semantics; the C++ passed
  a reference, which differs only if the callee also touches the same global directly (no WebRender shader does).
- Arguments of a call that holds `ctx`, `&mut self.fs` or `self` are bound to temporaries first when an argument
  itself calls something needing `ctx` or `&mut self` (borrowck). Evaluation stays left to right.
- A scalar `switch` is an `if` chain in a labeled block; a case falling through into the next non-empty case is
  refused (glsl-to-cxx's C++ `switch` allowed it; no WebRender shader uses it).
- C++'s `~` on a scalar `bool` mask (an integer complement, always true) is Rust's `!` (logical not). It only
  arises where glsl-to-cxx masks a scalar condition inside a function with conditional returns.
- A non-void function whose every path returns in GLSL gets `Default::default()` at its end (unreachable; C++ fell
  off the end without a value).

## The programs on the runtime, and the `ext` intrinsics each uses

All 77 programs build in raster/core (`crate::shaders`, `gl::load_shader`) with clippy `-D warnings`, against the
`crate::ext` as it stands now (the old `ext` feature gate is gone). Replayed through `raster/compare.sh`
(one gltrace built with `SWGL_STRICT_FP=1`, `--backend cxx` against `--backend rust`, one thread), every frame of
the example, bench, hn, github, bbc, wiki and example-site traces is identical. Per-draw hashes can still differ after a
delayed clear on a freshly allocated target: the readback does not resolve the clear, and the C++ buffer then holds
whatever `realloc` returned where the Rust one is zeroed (rows nothing has drawn yet; never on screen).

Which `ext` intrinsics each program calls (from the `use crate::ext::{..}` line of each module, taken
before the YUV commits were split into `..YUV2`/`..YUV3`):

| program | `crate::ext` intrinsics it needs |
|---|---|
| `brush_blend` | `swgl_antiAlias`, `swgl_clipMask` |
| `brush_blend_ALPHA_PASS` | `swgl_antiAlias`, `swgl_clipMask` |
| `brush_blend_DEBUG_OVERDRAW` | `swgl_antiAlias`, `swgl_clipMask` |
| `brush_image_ADVANCED_BLEND_ALPHA_PASS_ANTIALIASING_REPETITION_TEXTURE_2D` | `swgl_antiAlias`, `swgl_blendDropShadow`, `swgl_clipMask`, `swgl_commitTextureRepeatColorRGBA8`, `swgl_commitTextureRepeatRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_image_ADVANCED_BLEND_ALPHA_PASS_ANTIALIASING_REPETITION_TEXTURE_RECT` | `swgl_antiAlias`, `swgl_blendDropShadow`, `swgl_clipMask`, `swgl_commitTextureRepeatColorRGBA8`, `swgl_commitTextureRepeatRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_image_ADVANCED_BLEND_ALPHA_PASS_TEXTURE_2D` | `swgl_antiAlias`, `swgl_blendDropShadow`, `swgl_clipMask`, `swgl_commitTextureColorRGBA8`, `swgl_commitTextureRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_image_ADVANCED_BLEND_ALPHA_PASS_TEXTURE_RECT` | `swgl_antiAlias`, `swgl_blendDropShadow`, `swgl_clipMask`, `swgl_commitTextureColorRGBA8`, `swgl_commitTextureRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_image_ALPHA_PASS_ANTIALIASING_DUAL_SOURCE_BLENDING_REPETITION_TEXTURE_2D` | `swgl_antiAlias`, `swgl_blendDropShadow`, `swgl_clipMask` |
| `brush_image_ALPHA_PASS_ANTIALIASING_DUAL_SOURCE_BLENDING_REPETITION_TEXTURE_RECT` | `swgl_antiAlias`, `swgl_blendDropShadow`, `swgl_clipMask` |
| `brush_image_ALPHA_PASS_ANTIALIASING_REPETITION_TEXTURE_2D` | `swgl_antiAlias`, `swgl_blendDropShadow`, `swgl_clipMask`, `swgl_commitTextureRepeatColorRGBA8`, `swgl_commitTextureRepeatRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_image_ALPHA_PASS_ANTIALIASING_REPETITION_TEXTURE_RECT` | `swgl_antiAlias`, `swgl_blendDropShadow`, `swgl_clipMask`, `swgl_commitTextureRepeatColorRGBA8`, `swgl_commitTextureRepeatRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_image_ALPHA_PASS_DUAL_SOURCE_BLENDING_TEXTURE_2D` | `swgl_antiAlias`, `swgl_blendDropShadow`, `swgl_clipMask` |
| `brush_image_ALPHA_PASS_DUAL_SOURCE_BLENDING_TEXTURE_RECT` | `swgl_antiAlias`, `swgl_blendDropShadow`, `swgl_clipMask` |
| `brush_image_ALPHA_PASS_TEXTURE_2D` | `swgl_antiAlias`, `swgl_blendDropShadow`, `swgl_clipMask`, `swgl_commitTextureColorRGBA8`, `swgl_commitTextureRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_image_ALPHA_PASS_TEXTURE_RECT` | `swgl_antiAlias`, `swgl_blendDropShadow`, `swgl_clipMask`, `swgl_commitTextureColorRGBA8`, `swgl_commitTextureRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_image_ANTIALIASING_DEBUG_OVERDRAW_REPETITION_TEXTURE_2D` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureRepeatRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_image_ANTIALIASING_DEBUG_OVERDRAW_REPETITION_TEXTURE_RECT` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureRepeatRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_image_ANTIALIASING_REPETITION_TEXTURE_2D` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureRepeatRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_image_ANTIALIASING_REPETITION_TEXTURE_RECT` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureRepeatRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_image_DEBUG_OVERDRAW_TEXTURE_2D` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_image_DEBUG_OVERDRAW_TEXTURE_RECT` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_image_TEXTURE_2D` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_image_TEXTURE_RECT` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureRGBA8`, `swgl_forceScalar`, `swgl_isTextureRGBA8` |
| `brush_mix_blend` | `swgl_antiAlias`, `swgl_clipMask` |
| `brush_mix_blend_ALPHA_PASS` | `swgl_antiAlias`, `swgl_clipMask` |
| `brush_mix_blend_DEBUG_OVERDRAW` | `swgl_antiAlias`, `swgl_clipMask` |
| `brush_opacity` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureLinearColorRGBA8`, `swgl_forceScalar` |
| `brush_opacity_ALPHA_PASS` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureLinearColorRGBA8`, `swgl_forceScalar` |
| `brush_opacity_ALPHA_PASS_ANTIALIASING` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureLinearColorRGBA8`, `swgl_forceScalar` |
| `brush_opacity_ANTIALIASING` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureLinearColorRGBA8`, `swgl_forceScalar` |
| `brush_opacity_ANTIALIASING_DEBUG_OVERDRAW` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureLinearColorRGBA8`, `swgl_forceScalar` |
| `brush_opacity_DEBUG_OVERDRAW` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureLinearColorRGBA8`, `swgl_forceScalar` |
| `brush_solid` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitSolidR8`, `swgl_commitSolidRGBA8` |
| `brush_solid_ALPHA_PASS` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitSolidR8`, `swgl_commitSolidRGBA8` |
| `brush_solid_DEBUG_OVERDRAW` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitSolidR8`, `swgl_commitSolidRGBA8` |
| `brush_yuv_image_ALPHA_PASS_TEXTURE_2D_YUV` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureLinearYUV` |
| `brush_yuv_image_ALPHA_PASS_TEXTURE_RECT_YUV` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureLinearYUV` |
| `brush_yuv_image_DEBUG_OVERDRAW_TEXTURE_2D_YUV` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureLinearYUV` |
| `brush_yuv_image_DEBUG_OVERDRAW_TEXTURE_RECT_YUV` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureLinearYUV` |
| `brush_yuv_image_TEXTURE_2D_YUV` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureLinearYUV` |
| `brush_yuv_image_TEXTURE_RECT_YUV` | `swgl_antiAlias`, `swgl_clipMask`, `swgl_commitTextureLinearYUV` |
| `composite_FAST_PATH_TEXTURE_2D` | `swgl_commitTextureColorRGBA8`, `swgl_commitTextureRGBA8` |
| `composite_FAST_PATH_TEXTURE_2D_YUV` | `swgl_commitTextureLinearYUV` |
| `composite_FAST_PATH_TEXTURE_RECT` | `swgl_commitTextureColorRGBA8`, `swgl_commitTextureRGBA8` |
| `composite_FAST_PATH_TEXTURE_RECT_YUV` | `swgl_commitTextureLinearYUV` |
| `composite_TEXTURE_2D` | `swgl_commitTextureColorRGBA8`, `swgl_commitTextureRGBA8` |
| `composite_TEXTURE_2D_YUV` | `swgl_commitTextureLinearYUV` |
| `composite_TEXTURE_RECT` | `swgl_commitTextureColorRGBA8`, `swgl_commitTextureRGBA8` |
| `composite_TEXTURE_RECT_YUV` | `swgl_commitTextureLinearYUV` |
| `cs_blur_ALPHA_TARGET` | `swgl_commitGaussianBlurR8` |
| `cs_blur_COLOR_TARGET` | `swgl_commitGaussianBlurRGBA8` |
| `cs_border_segment` | none: builds without `ext` |
| `cs_border_solid` | none: builds without `ext` |
| `cs_clip_rectangle` | `swgl_commitColorR8`, `swgl_commitPartialSolidR8`, `swgl_commitSolidR8`, `swgl_forceScalar` |
| `cs_clip_rectangle_FAST_PATH` | `swgl_commitColorR8`, `swgl_commitPartialSolidR8`, `swgl_commitSolidR8`, `swgl_forceScalar` |
| `cs_line_decoration` | none: builds without `ext` |
| `cs_scale_TEXTURE_2D` | `swgl_commitTextureLinearRGBA8` |
| `cs_scale_TEXTURE_RECT` | `swgl_commitTextureLinearRGBA8` |
| `cs_svg_filter_node` | none: builds without `ext` |
| `debug_color` | none: builds without `ext` |
| `debug_font` | none: builds without `ext` |
| `ps_clear` | none: builds without `ext` |
| `ps_copy` | none: builds without `ext` |
| `ps_quad_box_shadow` | `swgl_antiAlias` |
| `ps_quad_gradient` | `swgl_antiAlias`, `swgl_commitLinearGradientFromStopsRGBA8`, `swgl_commitRadialGradientFromStopsRGBA8`, `swgl_commitSolidRGBA8`, `swgl_validateGradientFromStops` |
| `ps_quad_mask` | `swgl_antiAlias` |
| `ps_quad_mask_FAST_PATH` | `swgl_antiAlias` |
| `ps_quad_repeat` | `swgl_antiAlias`, `swgl_commitTextureRepeatColorRGBA8`, `swgl_commitTextureRepeatRGBA8`, `swgl_isTextureRGBA8` |
| `ps_quad_textured_TEXTURE_2D` | `swgl_antiAlias`, `swgl_commitSolidRGBA8`, `swgl_commitTextureColorRGBA8`, `swgl_commitTextureRGBA8` |
| `ps_quad_textured_TEXTURE_RECT` | `swgl_antiAlias`, `swgl_commitSolidRGBA8`, `swgl_commitTextureColorRGBA8`, `swgl_commitTextureRGBA8` |
| `ps_split_composite` | `swgl_clipMask`, `swgl_commitTextureRGBA8`, `swgl_forceScalar` |
| `ps_text_run_ALPHA_PASS_DUAL_SOURCE_BLENDING_GLYPH_TRANSFORM_TEXTURE_2D` | `swgl_blendDropShadow`, `swgl_blendSubpixelText`, `swgl_clipMask`, `swgl_commitTextureLinearRGBA8` |
| `ps_text_run_ALPHA_PASS_DUAL_SOURCE_BLENDING_TEXTURE_2D` | `swgl_blendDropShadow`, `swgl_blendSubpixelText`, `swgl_clipMask`, `swgl_commitTextureLinearRGBA8` |
| `ps_text_run_ALPHA_PASS_GLYPH_TRANSFORM_TEXTURE_2D` | `swgl_blendDropShadow`, `swgl_blendSubpixelText`, `swgl_clipMask`, `swgl_commitTextureLinearColorR8ToRGBA8`, `swgl_commitTextureLinearColorRGBA8`, `swgl_isTextureR8` |
| `ps_text_run_ALPHA_PASS_TEXTURE_2D` | `swgl_blendDropShadow`, `swgl_blendSubpixelText`, `swgl_clipMask`, `swgl_commitTextureLinearColorR8ToRGBA8`, `swgl_commitTextureLinearColorRGBA8`, `swgl_isTextureR8` |
| `ps_text_run_DEBUG_OVERDRAW_DUAL_SOURCE_BLENDING_TEXTURE_2D` | `swgl_blendDropShadow`, `swgl_blendSubpixelText`, `swgl_clipMask`, `swgl_commitTextureLinearRGBA8` |
| `ps_text_run_DEBUG_OVERDRAW_TEXTURE_2D` | `swgl_blendDropShadow`, `swgl_blendSubpixelText`, `swgl_clipMask`, `swgl_commitTextureLinearColorR8ToRGBA8`, `swgl_commitTextureLinearColorRGBA8`, `swgl_isTextureR8` |

By intrinsic (number of programs blocked):

- `swgl_antiAlias`: 48
- `swgl_clipMask`: 48
- `swgl_forceScalar`: 25
- `swgl_blendDropShadow`: 18
- `swgl_isTextureRGBA8`: 17
- `swgl_commitTextureRGBA8`: 15
- `swgl_commitTextureColorRGBA8`: 10
- `swgl_commitTextureLinearYUV`: 10
- `swgl_commitTextureLinearColorRGBA8`: 9
- `swgl_commitTextureRepeatRGBA8`: 9
- `swgl_blendSubpixelText`: 6
- `swgl_commitSolidRGBA8`: 6
- `swgl_commitSolidR8`: 5
- `swgl_commitTextureLinearRGBA8`: 5
- `swgl_commitTextureRepeatColorRGBA8`: 5
- `swgl_commitTextureLinearColorR8ToRGBA8`: 3
- `swgl_isTextureR8`: 3
- `swgl_commitColorR8`: 2
- `swgl_commitPartialSolidR8`: 2
- `swgl_commitGaussianBlurR8`: 1
- `swgl_commitGaussianBlurRGBA8`: 1
- `swgl_commitLinearGradientFromStopsRGBA8`: 1
- `swgl_commitRadialGradientFromStopsRGBA8`: 1
- `swgl_validateGradientFromStops`: 1

<!-- runtime:begin -->

### Runtime inventory (generated by `tests/emit_all.rs`)

77 programs, 91200 lines of Rust.

#### `crate::glsl` (83)

`A`, `B`, `Bool`, `Float`, `G`, `I32`, `IfThenElse`, `R`, `U32`, `W`, `X`, `Y`, `Z`, `abs`, `all`, `any`,
`bvec2_scalar`, `bvec4_scalar`, `ceil`, `clamp`, `cos`, `distance`, `dot`, `equal`, `exp`, `floor`,
`force_scalar`, `fract`, `fwidth`, `get_nth`, `greaterThan`, `greaterThanEqual`, `if_then_else`, `init_interp`,
`inversesqrt`, `isampler2D`, `isampler2D_impl`, `ivec2`, `ivec2_scalar`, `ivec4`, `ivec4_scalar`, `length`,
`lessThan`, `lessThanEqual`, `make_bvec2`, `make_bvec4`, `make_float`, `make_int`, `make_ivec2`, `make_ivec4`,
`make_mat3`, `make_mat4`, `make_uint`, `make_vec2`, `make_vec3`, `make_vec4`, `mat3`, `mat3_scalar`, `mat4`,
`mat4_scalar`, `max`, `min`, `mix`, `normalize`, `notEqual`, `pow`, `r#mod`, `recip`, `round`, `sampler2D`,
`sampler2DRect`, `sampler2DRect_impl`, `sampler2D_impl`, `sin`, `sqrt`, `step`, `transpose`, `vec2`,
`vec2_scalar`, `vec3`, `vec3_scalar`, `vec4`, `vec4_scalar`

#### `crate::texture` (7)

`lookup_isampler`, `lookup_sampler`, `texelFetch`, `texelFetchPtr`, `texelFetchUnchecked`, `texture`,
`textureSize`

#### `crate::ext` (26)

`swgl_antiAlias`, `swgl_blendDropShadow`, `swgl_blendSubpixelText`, `swgl_clipMask`, `swgl_commitColorR8`,
`swgl_commitGaussianBlurR8`, `swgl_commitGaussianBlurRGBA8`, `swgl_commitLinearGradientFromStopsRGBA8`,
`swgl_commitPartialSolidR8`, `swgl_commitRadialGradientFromStopsRGBA8`, `swgl_commitSolidR8`,
`swgl_commitSolidRGBA8`, `swgl_commitTextureColorRGBA8`, `swgl_commitTextureLinearColorR8ToRGBA8`,
`swgl_commitTextureLinearColorRGBA8`, `swgl_commitTextureLinearRGBA8`, `swgl_commitTextureLinearYUV`,
`swgl_commitTextureLinearYUV2`, `swgl_commitTextureLinearYUV3`, `swgl_commitTextureRGBA8`,
`swgl_commitTextureRepeatColorRGBA8`, `swgl_commitTextureRepeatRGBA8`, `swgl_forceScalar`, `swgl_isTextureR8`,
`swgl_isTextureRGBA8`, `swgl_validateGradientFromStops`

#### `crate::program` (13)

`FragmentShaderImpl`, `FragmentShaderState`, `NULL_ATTRIB`, `Pod`, `ProgramImpl`, `VertexAttrib`,
`VertexShaderImpl`, `VertexShaderState`, `load_attrib`, `load_flat_attrib`, `load_interp`, `store_interp`,
`swgl_StepSize`

#### Methods called on runtime types (12)

`assign`, `clone`, `enable_clip_distance`, `enable_perspective`, `index_lanes`, `lsel2`, `lsel3`, `map`,
`sel2`, `sel3`, `sel4`, `step_perspective`

#### Associated functions of runtime types (3)

`mat4_scalar::load_from_ptr`, `mem::align_of`, `mem::size_of`

#### `T::from` conversions, by target type (11)

`Bool`, `Float`, `I32`, `U32`, `ivec2`, `ivec4`, `mat3`, `mat4`, `vec2`, `vec3`, `vec4`

#### Signatures and operator impls the emitted code relies on (301)

- `Bool::from(i32)`
- `Float::from(f32)`
- `I32::from(i32)`
- `U32::from(u32)`
- `bvec2_scalar::sel4(XYZW, XYZW, XYZW, XYZW) -> bvec4_scalar`
- `equal(Float, Float)`
- `equal(I32, I32)`
- `fn abs(Float) -> Float`
- `fn abs(f32) -> f32`
- `fn abs(vec2) -> vec2`
- `fn abs(vec3) -> vec3`
- `fn all(bvec2) -> Bool`
- `fn all(bvec2_scalar) -> bool`
- `fn all(bvec4_scalar) -> bool`
- `fn any(bvec4_scalar) -> bool`
- `fn ceil(f32) -> f32`
- `fn clamp(Float, Float, Float) -> Float`
- `fn clamp(f32, f32, f32) -> f32`
- `fn clamp(vec2, vec2, vec2) -> vec2`
- `fn clamp(vec2_scalar, vec2_scalar, vec2_scalar) -> vec2_scalar`
- `fn clamp(vec3, Float, Float) -> vec3`
- `fn clamp(vec3, vec3, vec3) -> vec3`
- `fn clamp(vec4, vec4, vec4) -> vec4`
- `fn clamp(vec4_scalar, f32, f32) -> vec4_scalar`
- `fn cos(f32) -> f32`
- `fn distance(vec2, vec2) -> Float`
- `fn dot(vec2, vec2) -> Float`
- `fn dot(vec2_scalar, vec2_scalar) -> f32`
- `fn dot(vec3, vec3) -> Float`
- `fn dot(vec3_scalar, vec3_scalar) -> f32`
- `fn exp(f32) -> f32`
- `fn floor(Float) -> Float`
- `fn floor(f32) -> f32`
- `fn floor(vec2) -> vec2`
- `fn floor(vec4) -> vec4`
- `fn fract(Float) -> Float`
- `fn fract(vec2) -> vec2`
- `fn fract(vec2_scalar) -> vec2_scalar`
- `fn fwidth(vec2) -> vec2_scalar`
- `fn greaterThan(vec2_scalar, vec2_scalar) -> bvec2_scalar`
- `fn greaterThan(vec4_scalar, vec4_scalar) -> bvec4_scalar`
- `fn inversesqrt(Float) -> Float`
- `fn inversesqrt(f32) -> f32`
- `fn length(vec2) -> Float`
- `fn length(vec2_scalar) -> f32`
- `fn lessThan(vec2, vec2) -> bvec2`
- `fn lessThan(vec2_scalar, vec2_scalar) -> bvec2_scalar`
- `fn lessThanEqual(vec3, vec3) -> bvec3`
- `fn lessThanEqual(vec4_scalar, vec4_scalar) -> bvec4_scalar`
- `fn make_bvec2(bool) -> bvec2_scalar`
- `fn make_bvec2(bool, bool) -> bvec2_scalar`
- `fn make_bvec4(bool) -> bvec4_scalar`
- `fn make_bvec4(bvec2_scalar, bvec2_scalar) -> bvec4_scalar`
- `fn make_float(I32) -> Float`
- `fn make_float(i32) -> f32`
- `fn make_int(Bool) -> I32`
- `fn make_int(Float) -> I32`
- `fn make_int(bool) -> i32`
- `fn make_int(f32) -> i32`
- `fn make_int(u32) -> i32`
- `fn make_ivec2(I32, I32) -> ivec2`
- `fn make_ivec2(U32, U32) -> ivec2`
- `fn make_ivec2(i32, i32) -> ivec2_scalar`
- `fn make_ivec2(u32, u32) -> ivec2_scalar`
- `fn make_ivec2(vec2) -> ivec2`
- `fn make_ivec4(I32, I32, I32, I32) -> ivec4`
- `fn make_ivec4(i32, i32, i32, i32) -> ivec4_scalar`
- `fn make_ivec4(ivec2_scalar, i32, i32) -> ivec4_scalar`
- `fn make_ivec4(vec4_scalar) -> ivec4_scalar`
- `fn make_mat3(f32, f32, f32, f32, f32, f32, f32, f32, f32) -> mat3_scalar`
- `fn make_mat3(mat4_scalar) -> mat3_scalar`
- `fn make_mat4(vec4_scalar, vec4_scalar, vec4_scalar, vec4_scalar) -> mat4_scalar`
- `fn make_uint(I32) -> U32`
- `fn make_uint(i32) -> u32`
- `fn make_vec2(Float, Float) -> vec2`
- `fn make_vec2(f32) -> vec2_scalar`
- `fn make_vec2(f32, f32) -> vec2_scalar`
- `fn make_vec2(ivec2_scalar) -> vec2_scalar`
- `fn make_vec3(Float, Float, Float) -> vec3`
- `fn make_vec3(f32) -> vec3_scalar`
- `fn make_vec3(f32, f32, f32) -> vec3_scalar`
- `fn make_vec3(vec2, Float) -> vec3`
- `fn make_vec3(vec2_scalar, f32) -> vec3_scalar`
- `fn make_vec4(Float) -> vec4`
- `fn make_vec4(Float, Float, Float, Float) -> vec4`
- `fn make_vec4(Float, Float, vec2) -> vec4`
- `fn make_vec4(f32) -> vec4_scalar`
- `fn make_vec4(f32, f32, f32, f32) -> vec4_scalar`
- `fn make_vec4(f32, f32, vec2_scalar) -> vec4_scalar`
- `fn make_vec4(ivec4_scalar) -> vec4_scalar`
- `fn make_vec4(vec2, Float, Float) -> vec4`
- `fn make_vec4(vec2, vec2) -> vec4`
- `fn make_vec4(vec2_scalar, f32, f32) -> vec4_scalar`
- `fn make_vec4(vec2_scalar, vec2_scalar) -> vec4_scalar`
- `fn make_vec4(vec3, Float) -> vec4`
- `fn make_vec4(vec3_scalar, f32) -> vec4_scalar`
- `fn make_vec4(vec4) -> vec4`
- `fn max(Float, Float) -> Float`
- `fn max(f32, f32) -> f32`
- `fn max(vec2, Float) -> vec2`
- `fn max(vec2, vec2) -> vec2`
- `fn max(vec2_scalar, f32) -> vec2_scalar`
- `fn max(vec2_scalar, vec2_scalar) -> vec2_scalar`
- `fn max(vec3, vec3) -> vec3`
- `fn min(Float, Float) -> Float`
- `fn min(f32, f32) -> f32`
- `fn min(i32, i32) -> i32`
- `fn min(vec2, vec2) -> vec2`
- `fn min(vec2_scalar, vec2_scalar) -> vec2_scalar`
- `fn min(vec3, vec3) -> vec3`
- `fn mix(Float, Float, Float) -> Float`
- `fn mix(f32, f32, f32) -> f32`
- `fn mix(vec2, vec2, Float) -> vec2`
- `fn mix(vec2, vec2, vec2) -> vec2`
- `fn mix(vec2_scalar, vec2_scalar, bvec2_scalar) -> vec2_scalar`
- `fn mix(vec2_scalar, vec2_scalar, f32) -> vec2_scalar`
- `fn mix(vec3, vec3, Float) -> vec3`
- `fn mix(vec3, vec3, bvec3) -> vec3`
- `fn mix(vec3, vec3, vec3) -> vec3`
- `fn mix(vec3_scalar, vec3_scalar, vec3_scalar) -> vec3_scalar`
- `fn mix(vec4, vec4, Float) -> vec4`
- `fn mix(vec4, vec4, bvec4_scalar) -> vec4`
- `fn mix(vec4_scalar, vec4_scalar, bvec4_scalar) -> vec4_scalar`
- `fn mix(vec4_scalar, vec4_scalar, vec4_scalar) -> vec4_scalar`
- `fn normalize(vec2) -> vec2`
- `fn normalize(vec2_scalar) -> vec2_scalar`
- `fn notEqual(vec2_scalar, vec2_scalar) -> bvec2_scalar`
- `fn pow(Float, Float) -> Float`
- `fn pow(f32, f32) -> f32`
- `fn pow(vec3, vec3) -> vec3`
- `fn r#mod(Float, Float) -> Float`
- `fn r#mod(vec2, vec2) -> vec2`
- `fn recip(f32) -> f32`
- `fn recip(vec2_scalar) -> vec2_scalar`
- `fn round(f32) -> f32`
- `fn sin(f32) -> f32`
- `fn sqrt(Float) -> Float`
- `fn sqrt(f32) -> f32`
- `fn step(Float, Float) -> Float`
- `fn step(f32, vec4_scalar) -> vec4_scalar`
- `fn step(vec3, vec3) -> vec3`
- `fn step(vec3_scalar, vec3_scalar) -> vec3_scalar`
- `fn swgl_antiAlias(&mut RasterState, i32) -> ()`
- `fn swgl_blendDropShadow(&mut RasterState, vec4_scalar) -> ()`
- `fn swgl_blendSubpixelText(&mut RasterState, vec4_scalar) -> ()`
- `fn swgl_clipMask(&mut RasterState, sampler2D, vec2_scalar, vec2_scalar, vec2_scalar) -> ()`
- `fn swgl_commitColorR8(&mut RasterState, Float, &mut FragmentShaderState) -> ()`
- `fn swgl_commitGaussianBlurR8(&mut RasterState, sampler2D, vec2, vec4_scalar, bool, i32, vec2_scalar, &mut FragmentShaderState) -> ()`
- `fn swgl_commitGaussianBlurRGBA8(&mut RasterState, sampler2D, vec2, vec4_scalar, bool, i32, vec2_scalar, &mut FragmentShaderState) -> ()`
- `fn swgl_commitLinearGradientFromStopsRGBA8(&mut RasterState, sampler2D, i32, i32, f32, bool, vec2, vec2_scalar, f32, &mut FragmentShaderState) -> ()`
- `fn swgl_commitPartialSolidR8(&mut RasterState, i32, f32, &mut FragmentShaderState) -> ()`
- `fn swgl_commitRadialGradientFromStopsRGBA8(&mut RasterState, sampler2D, i32, i32, f32, bool, vec2, f32, &mut FragmentShaderState) -> ()`
- `fn swgl_commitSolidR8(&mut RasterState, f32, &mut FragmentShaderState) -> ()`
- `fn swgl_commitSolidRGBA8(&mut RasterState, vec4_scalar, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureColorRGBA8(&mut RasterState, sampler2D, vec2, vec4_scalar, vec4_scalar, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureColorRGBA8(&mut RasterState, sampler2DRect, vec2, vec4_scalar, vec4_scalar, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureLinearColorR8ToRGBA8(&mut RasterState, sampler2D, vec2, vec4_scalar, vec4_scalar, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureLinearColorRGBA8(&mut RasterState, sampler2D, vec2, vec4_scalar, f32, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureLinearColorRGBA8(&mut RasterState, sampler2D, vec2, vec4_scalar, vec4_scalar, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureLinearRGBA8(&mut RasterState, sampler2D, vec2, vec4_scalar, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureLinearRGBA8(&mut RasterState, sampler2DRect, vec2, vec4_scalar, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureLinearYUV(&mut RasterState, sampler2D, vec2, vec4_scalar, vec3_scalar, mat3_scalar, i32, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureLinearYUV(&mut RasterState, sampler2DRect, vec2, vec4_scalar, vec3_scalar, mat3_scalar, i32, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureLinearYUV2(&mut RasterState, sampler2D, vec2, vec4_scalar, sampler2D, vec2, vec4_scalar, vec3_scalar, mat3_scalar, i32, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureLinearYUV2(&mut RasterState, sampler2DRect, vec2, vec4_scalar, sampler2DRect, vec2, vec4_scalar, vec3_scalar, mat3_scalar, i32, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureLinearYUV3(&mut RasterState, sampler2D, vec2, vec4_scalar, sampler2D, vec2, vec4_scalar, sampler2D, vec2, vec4_scalar, vec3_scalar, mat3_scalar, i32, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureLinearYUV3(&mut RasterState, sampler2DRect, vec2, vec4_scalar, sampler2DRect, vec2, vec4_scalar, sampler2DRect, vec2, vec4_scalar, vec3_scalar, mat3_scalar, i32, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureRGBA8(&mut RasterState, sampler2D, vec2, vec4_scalar, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureRGBA8(&mut RasterState, sampler2DRect, vec2, vec4_scalar, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureRepeatColorRGBA8(&mut RasterState, sampler2D, vec2, vec2_scalar, vec4_scalar, vec4_scalar, vec4_scalar, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureRepeatColorRGBA8(&mut RasterState, sampler2DRect, vec2, vec2_scalar, vec4_scalar, vec4_scalar, vec4_scalar, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureRepeatRGBA8(&mut RasterState, sampler2D, vec2, vec2_scalar, vec4_scalar, vec4_scalar, &mut FragmentShaderState) -> ()`
- `fn swgl_commitTextureRepeatRGBA8(&mut RasterState, sampler2DRect, vec2, vec2_scalar, vec4_scalar, vec4_scalar, &mut FragmentShaderState) -> ()`
- `fn swgl_forceScalar(Float) -> f32`
- `fn swgl_forceScalar(vec2) -> vec2_scalar`
- `fn swgl_isTextureR8(sampler2D) -> bool`
- `fn swgl_isTextureRGBA8(sampler2D) -> bool`
- `fn swgl_isTextureRGBA8(sampler2DRect) -> bool`
- `fn swgl_validateGradientFromStops(sampler2D, ivec2_scalar, i32) -> i32`
- `fn texelFetch(isampler2D, ivec2_scalar, i32) -> ivec4_scalar`
- `fn texelFetch(sampler2D, ivec2, i32) -> vec4`
- `fn texelFetch(sampler2D, ivec2_scalar, i32) -> vec4_scalar`
- `fn texture(sampler2D, vec2) -> vec4`
- `fn texture(sampler2DRect, vec2) -> vec4`
- `fn textureSize(sampler2D, i32) -> ivec2_scalar`
- `fn textureSize(sampler2DRect) -> ivec2_scalar`
- `fn transpose(mat3_scalar) -> mat3_scalar`
- `force_scalar(I32) -> i32`
- `force_scalar(ivec4) -> ivec4_scalar`
- `force_scalar(vec2) -> vec2_scalar`
- `force_scalar(vec4) -> vec4_scalar`
- `greaterThan(Float, Float)`
- `greaterThan(I32, I32)`
- `greaterThanEqual(Float, Float)`
- `greaterThanEqual(I32, I32)`
- `if_then_else(Bool, Float, Float)`
- `if_then_else(Bool, vec2, vec2)`
- `if_then_else(Bool, vec3, vec3)`
- `if_then_else(I32, Float, Float)`
- `if_then_else(I32, I32, I32)`
- `if_then_else(I32, RectWithEndpoint, RectWithEndpoint)`
- `if_then_else(I32, vec2, vec2)`
- `if_then_else(I32, vec3, vec3)`
- `if_then_else(I32, vec4, vec4)`
- `impl Add<Float> for Float`
- `impl Add<Float> for vec2`
- `impl Add<Float> for vec3`
- `impl Add<I32> for I32`
- `impl Add<f32> for vec2_scalar`
- `impl Add<ivec2> for ivec2`
- `impl Add<ivec2_scalar> for ivec2_scalar`
- `impl Add<vec2> for vec2`
- `impl Add<vec2_scalar> for vec2_scalar`
- `impl Add<vec3> for Float`
- `impl Add<vec3> for vec3`
- `impl Add<vec4> for vec4`
- `impl Add<vec4_scalar> for vec4_scalar`
- `impl BitAnd<Bool> for Bool`
- `impl BitAnd<I32> for I32`
- `impl Div<Float> for Float`
- `impl Div<Float> for vec2`
- `impl Div<Float> for vec3`
- `impl Div<I32> for I32`
- `impl Div<U32> for U32`
- `impl Div<f32> for vec2_scalar`
- `impl Div<f32> for vec3_scalar`
- `impl Div<f32> for vec4_scalar`
- `impl Div<vec2> for vec2`
- `impl Div<vec2_scalar> for f32`
- `impl Div<vec2_scalar> for vec2_scalar`
- `impl Div<vec4> for vec4`
- `impl Div<vec4_scalar> for vec4_scalar`
- `impl Index<usize> + IndexMut<usize> for mat4_scalar`
- `impl Index<usize> + IndexMut<usize> for vec4`
- `impl Mul<Float> for Float`
- `impl Mul<Float> for vec2`
- `impl Mul<Float> for vec3`
- `impl Mul<Float> for vec4`
- `impl Mul<I32> for I32`
- `impl Mul<f32> for vec2_scalar`
- `impl Mul<f32> for vec3_scalar`
- `impl Mul<f32> for vec4_scalar`
- `impl Mul<mat3_scalar> for mat3_scalar`
- `impl Mul<vec2> for Float`
- `impl Mul<vec2> for vec2`
- `impl Mul<vec2_scalar> for f32`
- `impl Mul<vec2_scalar> for vec2_scalar`
- `impl Mul<vec3> for Float`
- `impl Mul<vec3> for mat3_scalar`
- `impl Mul<vec3> for vec3`
- `impl Mul<vec3_scalar> for mat3_scalar`
- `impl Mul<vec3_scalar> for vec3_scalar`
- `impl Mul<vec4> for Float`
- `impl Mul<vec4> for mat4_scalar`
- `impl Mul<vec4> for vec4`
- `impl Mul<vec4_scalar> for f32`
- `impl Mul<vec4_scalar> for mat4_scalar`
- `impl Mul<vec4_scalar> for vec4_scalar`
- `impl Neg for Float`
- `impl Neg for vec2_scalar`
- `impl Not for Bool`
- `impl Rem<I32> for I32`
- `impl Rem<U32> for U32`
- `impl Shl<i32> for ivec4_scalar`
- `impl Sub<Float> for Float`
- `impl Sub<Float> for vec2`
- `impl Sub<Float> for vec3`
- `impl Sub<I32> for I32`
- `impl Sub<vec2> for vec2`
- `impl Sub<vec2_scalar> for f32`
- `impl Sub<vec2_scalar> for vec2_scalar`
- `impl Sub<vec3> for vec3`
- `impl Sub<vec4_scalar> for f32`
- `impl Sub<vec4_scalar> for vec4_scalar`
- `ivec2::from(ivec2_scalar)`
- `ivec2_scalar::sel2(XYZW, XYZW) -> ivec2_scalar`
- `ivec4::from(ivec4_scalar)`
- `ivec4_scalar::sel2(XYZW, XYZW) -> ivec2_scalar`
- `lessThan(Float, Float)`
- `lessThan(I32, I32)`
- `lessThanEqual(Float, Float)`
- `mat3::from(mat3_scalar)`
- `mat4::from(mat4_scalar)`
- `notEqual(Float, Float)`
- `notEqual(I32, I32)`
- `vec2::from(vec2_scalar)`
- `vec2::sel2(XYZW, XYZW) -> vec2`
- `vec2_scalar::sel2(XYZW, XYZW) -> vec2_scalar`
- `vec2_scalar::sel3(XYZW, XYZW, XYZW) -> vec3_scalar`
- `vec2_scalar::sel4(XYZW, XYZW, XYZW, XYZW) -> vec4_scalar`
- `vec3::from(vec3_scalar)`
- `vec3::sel3(XYZW, XYZW, XYZW) -> vec3`
- `vec3_scalar::sel2(XYZW, XYZW) -> vec2_scalar`
- `vec4::from(vec4_scalar)`
- `vec4::index_lanes(I32)`
- `vec4::sel2(XYZW, XYZW) -> vec2`
- `vec4::sel3(XYZW, XYZW, XYZW) -> vec3`
- `vec4::sel4(XYZW, XYZW, XYZW, XYZW) -> vec4`
- `vec4_scalar::sel2(XYZW, XYZW) -> vec2_scalar`
- `vec4_scalar::sel3(XYZW, XYZW, XYZW) -> vec3_scalar`
- `vec4_scalar::sel4(XYZW, XYZW, XYZW, XYZW) -> vec4_scalar`

<!-- runtime:end -->
