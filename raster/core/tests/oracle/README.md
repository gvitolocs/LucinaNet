# The glsl oracle: swgl's C++ against `lucinanet_raster::glsl`

`shim.cc` includes swgl's `gl_defs.h`, `glsl.h` and `program.h` from `swgl/src/` the way `gl.cc` does (same
macros defined first) and exports `extern "C"` wrappers (`o_<name>`). `raster/core/oracle/` is a dev-only crate
whose `build.rs` compiles the shim as swgl's strict-float build (`SWGL_STRICT_FP=1` in swgl/build.rs): clang,
`-std=c++20 -fno-fast-math -ffp-contract=off -mrecip=none -fno-math-errno`, always `-O3`. `oracle/src/lib.rs`
declares the wrappers.
`tests/oracle_glsl.rs` runs every check below on 100,000 inputs from a fixed seed and requires identical bits.
Floats compare by bit pattern, except that two NaNs count as equal whatever their payloads.

Inputs mix edge values (±0, the smallest and largest denormals, `f32::MIN_POSITIVE`, ±inf, ±NaN, ±1e30,
`f32::MAX`, ±2^31, 2^32, 3e9, 2^23 and 2^24, values one ulp either side of .5 boundaries, π, 255, 127.5, ...),
random bit patterns, and random values in the ranges shaders use ([0, 1], [-10, 10], ±1e6). Integer inputs mix
`i32::MIN`/`MAX`, 0, ±1, byte and short limits and random bits. Conversions whose C++ result is undefined outside
a range (float to `u8`) are tested inside it only.

Command: `CC=clang CXX=clang++ cargo test --release` in `raster/core` (x86_64). For AArch64 see the end of this file.

## The bar: swgl with strict floats

The oracle is swgl compiled without fast-math and without FMA contraction (raster/DESIGN.md, "Correctness bar"),
on x86_64 and AArch64 alike. The C++ then rounds every operation separately in glsl.h's source order, and so does
the Rust: it never fuses a multiply into an add and never reorders a sum, on any target. The shipped swgl keeps
`-ffast-math`; it is allowed to differ from the strict build, and does by up to 0.04% of the pixels of a whole
frame (measured on example-site; identical on the paint page and Wikipedia). A difference between the Rust and the
strict build is a porting bug.

## Functions covered (156 checks, one per shim wrapper)

**Lane arithmetic and comparisons** (`Float`, `I32`, `U32`): `+`, `-`, `*`, `/`, unary `-`, `a*b+c`, `1 - a*b`,
`<`, `<=`, `>`, `>=`, `==`, `!=` (all-ones / zero masks), `I32` `+`, `*`, `& ~`, `^`, `>`, `>>` (arithmetic),
`<<`, `U32 >>` (logical), `clamp(I32, I32, I32)`, `if_then_else(I32, Float, Float)`, `if_then`, `test_all`,
`test_any`, `test_none`, `test_equal`.

**Conversions and rounding modes**: `CONVERT` `Float→I32`, `Float→U32`, `I32→Float`, `U32→Float`; `cast(U32)`,
`cast(Float)`, `cast(cast(Float))`; `make_int`/`make_uint` on lanes and scalars (`int32_t(float)`,
`uint32_t(float)`), `make_float(U32)` on lanes and scalars; `roundeven`, `roundzero`, `roundfast` (lanes and
scalars), `round_pixel`, `roundfast(vec4, Float)`.

**Approximations**: `recip`, `inversesqrt`, `sqrt` (lanes and scalars), `approx_log2`, `approx_pow2`, `exp`,
`exp2`, `log`, `log2`, `pow` on lanes; `pow(vec2)`, `pow(vec3)`.

**Transcendentals**: `sin`, `cos`, `tan`, `atan`, `atan(y, x)` on lanes; `exp`, `exp2`, `log`, `log2`, `sin`,
`cos`, `atan`, `atan(y, x)`, `pow` on scalars (libm).

**Shaping built-ins**: `fract`, `floor`, `ceil`, `round`, `abs`, `sign`, `min`, `max`, `clamp`, `mix`, `step`,
`mod` on lanes; `floor`, `ceil`, `round`, `fract`, `abs`, `sign`, `min`, `max`, `step`, `mix`, `clamp` on
scalars; `init_interp`, `dFdx`, `fwidth`.

**Vectors**: `dot(vec2)`, `dot(vec3)`, `dot(vec2_scalar)`, `dot(vec3_scalar)`, `length(vec2)`,
`length(vec2_scalar)` (`hypotf`), `normalize(vec2)`, `normalize(vec2_scalar)`, `distance(vec2, vec2)`,
`mix(vec4, vec4, Float)`, `mix(vec4_scalar, vec4_scalar, vec4_scalar)`, `clamp(vec4, Float, Float)`,
`step(vec2, vec2)`, `floor(vec4)`, `mod(vec2, vec2)`, `vec4 / Float`, `vec2 * vec2_scalar`, `vec4 == vec4`.

**Matrices**: `mat4_scalar * vec4_scalar`, `mat4_scalar * vec4`, `mat4 * vec4`, `mat4x3_scalar * vec4_scalar`,
`mat4x3_scalar * vec4`, `mat3 * vec3`, `mat3_scalar * vec3_scalar`, `mat3_scalar * vec3`,
`mat3x4_scalar * vec3_scalar`, `mat3x4_scalar * vec3`, `mat3_scalar * mat3_scalar`, `mat2 * vec2`,
`inverse(mat2_scalar)`, `inverse(mat2)`.

**vector_type.h**: `zip`, `zipLow`, `zipHigh`, `zip2Low`, `zip2High`, `combine(highHalf, lowHalf)`,
`combineLow` on `V8<uint16_t>`; `zipLow`, `zipHigh` on `V16<uint8_t>`; `SHUFFLE` (`V4<float>` pair to
`V8<float>`), the `.zwxy` swizzle; packs and unpacks via `CONVERT`: `V8<uint16_t>→V8<uint8_t>`,
`V16<uint8_t>→V16<uint16_t>`, `I32→U16`, `U16→I32`, `V8<int32_t>→V8<int16_t>`, `Float→U8`; `V8<uint16_t>`
multiply-add-shift and `<`, `V16<uint8_t>` wrapping `+`, `bit_cast<V4<uint32_t>>(V16<uint8_t>)`.

Not covered because glsl.h does not define them: `smoothstep`, `cross`, `roundto`, `length`/`normalize` of
`vec3`/`vec4` (glsl-to-cxx 0.70 declares none of these; WebRender's shaders do not use them under swgl).

## What the source leaves to the hardware

Float→int conversions out of range are undefined in C++; swgl gets what the instruction returns, and the Rust
reproduces it: `cvttps2dq` gives `0x80000000` on x86_64 (Rust's `as` saturates), `fptoui` is clang's
`cvttps2dq` sequence, scalar `uint32_t(float)` is the 64-bit `cvttss2si` truncated; AArch64's `fcvtzs`/`fcvtzu`
saturate like Rust's `as`. The approximations use the intrinsic glsl.h calls on each target (`rcpps`, `rsqrtps`,
`vrecpe`/`vrecps`, `vrsqrte`/`vrsqrts`, `cvtps2dq`, `movmskps`, `vminq`/`vmaxq`, `vabsq`).

## AArch64

The oracle test builds for `aarch64-unknown-linux-musl`, with the shim cross-compiled by clang
(`--target=aarch64-alpine-linux-musl`) against the Alpine aarch64 sysroot (`$S`), linked
statically with lld as `scripts/build-juno.sh` does:

```
W=<dir with cc/cxx wrappers: clang(++) --target=aarch64-alpine-linux-musl --sysroot=$S -B<rust gcc-ld> -fuse-ld=lld>
export CC_aarch64_unknown_linux_musl=$W/cc CXX_aarch64_unknown_linux_musl=$W/cxx AR_aarch64_unknown_linux_musl=<rust llvm-ar>
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=$W/cxx
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static -C link-self-contained=no \
  -C link-arg=-static -C link-arg=-lstdc++ -L $W/lib"
cargo test --release --no-run --target aarch64-unknown-linux-musl
```

The binary is static (no interpreter, no shared libraries) and runs on an AArch64 Linux machine as
`./oracle_glsl-<hash>`; the shim in it has no fused instruction (`llvm-objdump -d` shows no `fmla`/`fmadd`).
Until that run, the AArch64 results are unverified.

## Run on the laptop

2026-11-04, Galaxy Book Go under Juno (8 CPUs): the static aarch64-unknown-linux-musl oracle test, with the shim built
strict-float by clang for AArch64, passed all 13 groups (156 checks, 100,000 inputs each) in 1.04 s.

# The texture, blend and composite oracle: swgl's gl.cc against `texture`, `blend`, `composite`

`shim_t.cc` `#include`s `swgl/src/gl.cc` itself, so texture.h, blend.h, composite.h and their statics (`blend_key`,
`swgl_ClipMaskBuf`, `swgl_OpaqueStart`, `ctx`, ...) are in one translation unit with the wrappers (`t_<name>`).
`stub/load_shader.h` comes first on the include path and is empty; `shim_t.cc` defines `load_shader` with one
hand-written program, "flat" (see below). This is the only translation unit holding gl.cc: `oracle_gl.rs` calls the same
gl.cc symbols. `oracle/build.rs` compiles it as swgl's
strict-float release build: the flags above plus `NDEBUG` and `_GLIBCXX_USE_CXX11_ABI=0`, as `swgl/build.rs` does.
Samplers and textures cross the boundary as plain structs (`TSampler`, `TTexture`, `TBlendState` in
`oracle/src/lib.rs`); the shim builds swgl's own objects from them (`Texture::set_buffer`, `enable_delayed_clear`).

Inputs (`tests/common/mod.rs`): textures of every format swgl samples (RGBA8, R8, RG8, R16, RG16, RGBA32F, RGBA32I,
YUY2), sides 1..70 with 1 and 2 often, strides `set_buffer` accepts (row plus 0..3 units), random contents biased to
0 and 255, floats with NaN, ±inf and random bits. Buffers have 256 zero bytes before and after: the C++ reads up to a
pixel before a row of fewer than 2 pixels (linear filter) and past an odd YUY2 row, with zero weight, and the Rust
reads zero outside the texture. Destinations are compared whole, guard bands included.

- `oracle_texture.rs`: `texelFetch` (sampler2D, sampler2DRect and the typed samplers; lanes and scalars), isampler2D,
  `texelFetchPtr` + `texelFetchUnchecked` (lanes and scalars), `texture` nearest and linear on every format (lanes,
  scalars, normalised and rect coordinates, NaN and far-out ones), the packed/unpacked/planar linear filters
  (`textureLinear{Unpacked,Packed,Planar}{RGBA8,R8,RG8}`, `textureNearestPackedRGBA8`, `textureLinearUnpacked{R16,RG16}`,
  `textureLinearPlanarYUY2`), the Gaussian blur taps (RGBA8 and R8, both directions), delayed clears
  (`prepare_texture` with and without a skip rect: buffer, `delay_clear`, bit vector), and `lookup_sampler`.
- `oracle_blend.rs`: `blend_pixels` for all 4 x 31 RGBA8 keys (plain, MASK, AA, AA_MASK of every `FOR_EACH_BLEND_KEY`
  entry) and the 4 x 3 R8 ones, spans 1..4, 10,000 cases a key, with random clip masks, opaque ranges, AA distances
  and slopes (NaN included), blend colors and secondary colors; `commit_blend_span` (full and partial),
  `commit_solid_span` (blended or not, any R8 alignment); every `pack_pixels_RGBA8` / `pack_pixels_R8` overload;
  exhaustively, `muldiv255` for all `x, y <= 255` against `rules::muldiv255` and the C++, and premultiplied
  source-over for all `src <= alpha <= 255`, `dst <= 255` against `rules::source_over`.
- `oracle_composite.rs`: `scale_blit` (all 9 R8/RG8/RGBA8 pairs; composited RGBA8), `linear_blit` (opaque and
  composited, X and Y flips, sources with pending delayed clears), `Composite` (opaque or not, flips, both filters,
  clips partly outside, empty and negative sizes), `ApplyMask`, `CompositeYUV` (all 7 color spaces; 8-bit R8 and
  10/12/16-bit R16 planes; chroma at full and half resolution, wide rows so the half-resolution fast path runs;
  1-pixel rows), `BlitFramebuffer` through swgl's GL entry points (`SetTextureBuffer`, framebuffers), and requests
  wholly outside, which must write nothing.

The C++ blend state is global, so the tests that set it take `oracle_lock()`. What the tests stay away from, because
the C++ faults there: RGBA32F/RGBA32I rows not 16-byte aligned (swgl loads those texels as aligned vectors; its own
buffers and WebRender's are aligned), and negative `texelFetchOffset` offsets (the C++ computes `x * 4 + y * stride`
unsigned; no WebRender shader uses one). The AArch64 test binaries build as described above (no fused instruction
in `shim_t.o`); until they run on an AArch64 machine, the NEON paths (`vqmovun` packs, `vqrdmulh` lerps, unsigned
`vminq_u16`/`vmaxq_u16`) are unverified.

The texture, blend and composite oracle tests ran on the laptop too (2026-11-04, AArch64 static binaries built as
above): oracle_texture 8, oracle_blend 6, oracle_composite 7, oracle_glsl 13 and the 14 unit tests, all passed. The NEON
paths (vqmovun packs, vqrdmulh lerps, unsigned vminq_u16/vmaxq_u16) are therefore checked against strict swgl on
real hardware.

# The rasterizer oracle: gl.cc drawing with a hand-written program

Until generated shaders exist, `shim_t.cc` carries "flat" against program.h (a `ProgramImpl` with its vertex and
fragment halves: positions from attribute 0, colour from uniform `uColor`, depth and per-vertex Ws from `uZW`), and
`load_shader("flat")` returns it. `tests/rasterize_flat.rs` writes the same program against `lucinanet_raster::program`,
draws the same quads, triangles, lines, blends, scissors and perspective cases through `gl::Context` and through gl.cc,
and compares the whole framebuffer after every draw and after the delayed clear is flushed.
