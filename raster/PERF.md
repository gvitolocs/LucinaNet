# lucinanet-raster speed against strict swgl, per program

> 2026-11-11: the Rust rasterizer is the default backend of LucinaNet; `LUCINANET_RASTER=swgl` selects the C++ oracle (swgl stays in the build as the oracle and the benchmark).

One thread on the build host (CPU 5), the strict gltrace binary (`SWGL_STRICT_FP=1`, both backends in one binary), default
release profile (opt-level 3, no LTO), 2026-11-04. Measured with `LUCINANET_SWGL_PROFILE=1`: swgl's timer
(`swgl/src/lucina_prof.h`) and its Rust twin (`raster/core/src/prof.rs`, same keys and output), summed over all windows
of one replay. The table is made by `gltrace perf-table <cxx.prof> <rust.prof> --page <page>`.

```
for b in cxx rust; do LUCINANET_SWGL_PROFILE=1 taskset -c 5 gltrace replay <trace> --out <dir> --backend $b 2> $b.prof; done
```

Note: the Rust side's BlitFramebuffer, ClearColorRect and TexSubImage2D pixel counts come from the call's arguments
(swgl's from the clipped rectangle), and the Rust records these calls also when they return early.

### example-site

| key | blend | calls | px | C++ ms | Rust ms | C++ ns/px | Rust ns/px | Rust/C++ |
|---|---|---|---|---|---|---|---|---|
| ps_quad_gradient | 2 | 286 | 45737798 | 327.98 | 1457.79 | 7.17 | 31.87 | 4.44 |
| ps_quad_box_shadow | 2 | 334 | 46301833 | 689.92 | 1368.27 | 14.90 | 29.55 | 1.98 |
| ps_quad_mask_FAST_PATH | 6 | 439 | 30878715 | 161.70 | 508.58 | 5.24 | 16.47 | 3.15 |
| ps_quad_textured_TEXTURE_2D | 2 | 1118 | 78541105 | 207.91 | 410.02 | 2.65 | 5.22 | 1.97 |
| brush_opacity_ALPHA_PASS | 2 | 113 | 19586399 | 132.47 | 345.64 | 6.76 | 17.65 | 2.61 |
| ps_quad_repeat | 2 | 64 | 29859840 | 65.04 | 201.07 | 2.18 | 6.73 | 3.09 |
| Clear | 0 | 738 | 0 | 133.07 | 61.38 | - | - | 0.46 |
| brush_image_ALPHA_PASS_TEXTURE_2D | 2 | 461 | 6527051 | 23.02 | 50.75 | 3.53 | 7.78 | 2.20 |
| BlitFramebuffer | 0 | 150 | 30875967 | 21.81 | 17.65 | 0.71 | 0.57 | 0.81 |
| ps_text_run_ALPHA_PASS_DUAL_SOURCE_BLENDING_TEXTURE_2D | 11 | 227 | 742389 | 8.94 | 17.26 | 12.04 | 23.25 | 1.93 |
| Composite | 1 | 216 | 106170368 | 17.29 | 15.06 | 0.16 | 0.14 | 0.87 |
| ps_quad_textured_TEXTURE_2D | 11 | 64 | 18085434 | 14.43 | 14.43 | 0.80 | 0.80 | 1.00 |
| cs_blur_COLOR_TARGET | 2 | 95 | 139327 | 3.51 | 10.14 | 25.19 | 72.78 | 2.89 |
| ps_text_run_ALPHA_PASS_TEXTURE_2D | 2 | 68 | 294150 | 3.05 | 6.59 | 10.37 | 22.40 | 2.16 |
| ps_quad_textured_TEXTURE_2D | 6 | 16 | 8388608 | 5.96 | 4.71 | 0.71 | 0.56 | 0.79 |
| cs_blur_COLOR_TARGET | 6 | 3 | 28524 | 2.63 | 4.15 | 92.20 | 145.49 | 1.58 |
| ps_quad_gradient | 11 | 11 | 696194 | 2.73 | 3.49 | 3.92 | 5.01 | 1.28 |
| cs_scale_TEXTURE_2D | 2 | 102 | 851215 | 1.15 | 1.98 | 1.35 | 2.33 | 1.72 |
| TexSubImage2D | 0 | 695 | 6008167 | 4.99 | 1.96 | 0.83 | 0.03 | 0.39 |
| cs_blur_COLOR_TARGET | 11 | 32 | 21371 | 0.56 | 1.58 | 26.20 | 73.93 | 2.82 |
| ps_text_run_ALPHA_PASS_DUAL_SOURCE_BLENDING_GLYPH_TRANSFORM_TEXTURE_2D | 11 | 53 | 48825 | 0.92 | 1.54 | 18.84 | 31.54 | 1.67 |
| cs_blur_COLOR_TARGET | 0 | 2 | 7998 | 0.45 | 1.21 | 56.26 | 151.29 | 2.69 |
| ps_quad_gradient | 6 | 7 | 328104 | 0.54 | 0.76 | 1.65 | 2.32 | 1.41 |
| cs_border_solid | 2 | 6 | 22492 | 0.30 | 0.50 | 13.34 | 22.23 | 1.67 |
| cs_clip_rectangle_FAST_PATH | 2 | 27 | 138639 | 0.30 | 0.42 | 2.16 | 3.03 | 1.40 |
| ps_quad_textured_TEXTURE_2D | 0 | 1 | 18792 | 0.23 | 0.38 | 12.24 | 20.22 | 1.65 |
| ps_clear | 2 | 16 | 108895 | 0.17 | 0.25 | 1.56 | 2.30 | 1.47 |
| cs_scale_TEXTURE_2D | 11 | 32 | 41580 | 0.09 | 0.19 | 2.16 | 4.57 | 2.11 |
| cs_clip_rectangle_FAST_PATH | 11 | 12 | 94806 | 0.11 | 0.17 | 1.16 | 1.79 | 1.55 |
| ps_clear | 11 | 11 | 87150 | 0.10 | 0.17 | 1.15 | 1.95 | 1.70 |
| brush_blend_ALPHA_PASS | 2 | 3 | 1452 | 0.04 | 0.09 | 27.55 | 61.98 | 2.25 |
| ps_clear | 6 | 4 | 34650 | 0.04 | 0.07 | 1.15 | 2.02 | 1.75 |
| cs_clip_rectangle | 0 | 2 | 23116 | 0.06 | 0.07 | 2.60 | 3.03 | 1.17 |
| cs_clip_rectangle_FAST_PATH | 6 | 4 | 34650 | 0.04 | 0.06 | 1.15 | 1.73 | 1.50 |
| cs_clip_rectangle | 6 | 1 | 7790 | 0.02 | 0.03 | 2.57 | 3.85 | 1.50 |
| cs_clip_rectangle | 2 | 2 | 5097 | 0.02 | 0.02 | 3.92 | 3.92 | 1.00 |
| cs_clip_rectangle | 11 | 2 | 6928 | 0.01 | 0.02 | 1.44 | 2.89 | 2.00 |

total: C++ 1831.60 ms, Rust 4508.45 ms, Rust/C++ 2.46

### bench

| key | blend | calls | px | C++ ms | Rust ms | C++ ns/px | Rust ns/px | Rust/C++ |
|---|---|---|---|---|---|---|---|---|
| ps_quad_box_shadow | 2 | 65 | 2376192 | 32.10 | 71.62 | 13.51 | 30.14 | 2.23 |
| ps_quad_mask_FAST_PATH | 6 | 2 | 1230592 | 6.03 | 19.61 | 4.90 | 15.94 | 3.25 |
| ps_quad_textured_TEXTURE_2D | 2 | 70 | 2412192 | 4.41 | 8.88 | 1.83 | 3.68 | 2.01 |
| cs_blur_COLOR_TARGET | 2 | 4 | 57672 | 1.31 | 3.70 | 22.71 | 64.16 | 2.82 |
| brush_image_ALPHA_PASS_TEXTURE_2D | 2 | 64 | 126672 | 1.12 | 2.64 | 8.84 | 20.84 | 2.36 |
| ps_quad_gradient | 2 | 6 | 1749504 | 4.32 | 1.55 | 2.47 | 0.89 | 0.36 |
| Composite | 1 | 18 | 7971840 | 1.18 | 1.44 | 0.15 | 0.18 | 1.22 |
| Clear | 0 | 25 | 0 | 18.68 | 1.24 | - | - | 0.07 |
| ps_quad_gradient | 6 | 1 | 626688 | 1.49 | 0.36 | 2.38 | 0.57 | 0.24 |
| brush_opacity_ALPHA_PASS | 2 | 1 | 21600 | 0.05 | 0.09 | 2.31 | 4.17 | 1.80 |
| cs_scale_TEXTURE_2D | 2 | 1 | 20736 | 0.05 | 0.07 | 2.41 | 3.38 | 1.40 |
| cs_clip_rectangle | 2 | 1 | 8100 | 0.02 | 0.03 | 2.47 | 3.70 | 1.50 |
| cs_border_solid | 2 | 1 | 608 | 0.01 | 0.03 | 16.45 | 49.34 | 3.00 |
| ps_quad_textured_TEXTURE_2D | 0 | 1 | 6400 | 0.03 | 0.02 | 4.69 | 3.12 | 0.67 |
| TexSubImage2D | 0 | 10 | 7810 | 0.05 | 0.01 | 6.40 | 0.00 | 0.20 |

total: C++ 70.85 ms, Rust 111.29 ms, Rust/C++ 1.57

### wiki

| key | blend | calls | px | C++ ms | Rust ms | C++ ns/px | Rust ns/px | Rust/C++ |
|---|---|---|---|---|---|---|---|---|
| Composite | 0 | 722 | 175410465 | 49.90 | 127.26 | 0.28 | 0.73 | 2.55 |
| Composite | 1 | 892 | 339357632 | 54.57 | 91.19 | 0.16 | 0.27 | 1.67 |
| ps_text_run_ALPHA_PASS_TEXTURE_2D | 2 | 50 | 908189 | 14.75 | 50.85 | 16.24 | 55.99 | 3.45 |
| ps_quad_textured_TEXTURE_2D | 2 | 148 | 19262503 | 19.15 | 22.11 | 0.99 | 1.15 | 1.15 |
| ps_quad_textured_TEXTURE_2D | 6 | 14 | 7124200 | 9.67 | 6.62 | 1.36 | 0.93 | 0.68 |
| ps_text_run_ALPHA_PASS_DUAL_SOURCE_BLENDING_TEXTURE_2D | 11 | 39 | 222457 | 3.29 | 6.60 | 14.79 | 29.67 | 2.01 |
| brush_image_ALPHA_PASS_TEXTURE_2D | 2 | 52 | 41470 | 1.37 | 6.18 | 33.04 | 149.02 | 4.51 |
| ps_quad_gradient | 2 | 6 | 24576 | 0.43 | 5.12 | 17.50 | 208.33 | 11.91 |
| Clear | 0 | 215 | 0 | 3.04 | 4.17 | - | - | 1.37 |
| ps_quad_mask_FAST_PATH | 6 | 18 | 143570 | 0.73 | 2.26 | 5.08 | 15.74 | 3.10 |
| brush_blend_ALPHA_PASS | 2 | 22 | 11044 | 0.34 | 0.53 | 30.79 | 47.99 | 1.56 |
| TexSubImage2D | 0 | 606 | 282443 | 2.50 | 0.35 | 8.85 | 0.01 | 0.14 |
| ps_quad_textured_TEXTURE_2D | 11 | 3 | 175412 | 0.19 | 0.21 | 1.08 | 1.20 | 1.11 |
| cs_border_solid | 2 | 5 | 992 | 0.56 | 0.11 | 564.52 | 110.89 | 0.20 |
| brush_opacity_ALPHA_PASS | 2 | 7 | 4696 | 0.10 | 0.09 | 21.29 | 19.17 | 0.90 |
| cs_clip_rectangle_FAST_PATH | 2 | 4 | 32 | 0.03 | 0.03 | 937.50 | 937.50 | 1.00 |
| cs_clip_rectangle_FAST_PATH | 6 | 1 | 8 | - | 0.01 | 0.00 | 1250.00 | - |

total: C++ 160.62 ms, Rust 323.69 ms, Rust/C++ 2.02

## compare.sh with gltrace's release profile at fat LTO, one codegen unit (2026-11-04)

```
bbc cxx 78.787 ms rust 272.339 ms speed 0.29x frames 28 identical 28 within 0 worse 0 PASS
bench cxx 115.958 ms rust 142.116 ms speed 0.82x frames 4 identical 4 within 0 worse 0 PASS
example cxx 16.501 ms rust 34.943 ms speed 0.47x frames 10 identical 10 within 0 worse 0 PASS
github cxx 77.752 ms rust 176.626 ms speed 0.44x frames 20 identical 20 within 0 worse 0 PASS
hn cxx 32.354 ms rust 49.551 ms speed 0.65x frames 10 identical 10 within 0 worse 0 PASS
example-site cxx 2125.744 ms rust 3923.536 ms speed 0.54x frames 26 identical 26 within 0 worse 0 PASS
wiki cxx 79.881 ms rust 135.837 ms speed 0.59x frames 70 identical 70 within 0 worse 0 PASS
```

Default release profile on the same day, example-site: 0.38x. Not met yet: Rust >= 1.0x. The biggest per-program gaps on
the profile table are `ps_quad_gradient` (4.4x the C++ time a pixel on example-site, 12x on wiki), `ps_quad_mask_FAST_PATH`
(3.1x), `ps_quad_repeat` (3.1x), `ps_text_run` (2-3.5x) and `Composite` blend 0 on wiki (2.6x); their machine code
has not been read yet (step 2 of task O).

## Causes found and fixed (task O, steps 2-3)

1. **An environment lookup in the gradient span loop** (027ea11). `commit_linear_gradient_from_stops` called
   `std::env::var_os("XTRACE")` once per sub-span, a debug leftover that takes libc's environment lock and scans
   `environ`. It was the per-span setup cost behind the small wiki gradients (208 ns/px against 17.5) and much of
   the example-site gradient. Removed: example-site `ps_quad_gradient` went from 31.9 to 17.6 ns/px (C++ 6.7); bbc 272 -> 111 ms.
2. **blend_pixels and texture() out of line** (90b16bc). swgl marks `blend_pixels` `PREFER_INLINE` (always_inline
   with clang in release) and `texture(sampler2D, vec2)` `SI`. The Rust had no inline attribute on them, and LLVM
   kept `blend_pixels_RGBA8` (2,576 instructions, all blend keys) out of line at 51 call sites: every 4-pixel chunk
   was a call with its 32-byte vectors passed through memory, and the switch on the blend key stayed inside the loop.
   Now `#[inline(always)]` on `blend_pixels_RGBA8`, `blend_pixels_R8`, `commit_blend_solid_span`, the
   `texture()` impls and the texel-fetch impls. example-site: Rust 3328 -> 2307 ms.
3. Tried and dropped: also inlining `texture_linear_any`, the format switch inside `texture()` (swgl inlines it).
   No gain beyond the noise, and the build took twice as long.

example-site, one thread, after both fixes: 0.56x -> 0.58-0.64x of strict swgl. Repeated runs on the same binary vary
by about 15%, because the build host runs at Tctl 95 °C. All 7 pages stay identical.

```
bbc cxx 69.646 ms rust 107.427 ms speed 0.65x frames 28 identical 28 within 0 worse 0 PASS
bench cxx 62.158 ms rust 113.192 ms speed 0.55x frames 4 identical 4 within 0 worse 0 PASS
example cxx 11.639 ms rust 30.150 ms speed 0.39x frames 10 identical 10 within 0 worse 0 PASS
github cxx 55.466 ms rust 120.680 ms speed 0.46x frames 20 identical 20 within 0 worse 0 PASS
hn cxx 16.462 ms rust 36.722 ms speed 0.45x frames 10 identical 10 within 0 worse 0 PASS
example-site cxx 1477.446 ms rust 3133.257 ms speed 0.47x frames 26 identical 26 within 0 worse 0 PASS
wiki cxx 89.984 ms rust 145.832 ms speed 0.62x frames 70 identical 70 within 0 worse 0 PASS
```

### What is left
Still about 1.6x slower than C++ on example-site. The remaining out-of-line calls in the release binary are
`texture_linear_any` (111 sites, the same layer that is a call in C++),
`textureLinearUnpackedR16/R8`, `set_lum` and `lookup_sampler` (per draw). The next things to read:
- the `ps_quad_box_shadow` pixel loop (`distance_to_rounded_rect`, the R8 `textureLinearR8`), now the largest
  key in ms;
- the gradient's `find_gradient_stop_pair`, a call per sub-span that reads through the bounds-checked `at32`;
- the instruction mix: `pextrw`/`movzwl`/`movswl` in the gradient span function point to lane loops left scalar.

# lucinanet-raster performance notes

## Memory

Measured with one strict gltrace binary (`raster/gltrace` built with `SWGL_STRICT_FP=1`, fat LTO, one codegen unit),
`LUCINANET_RASTER_THREADS=1`, peak RSS from `/usr/bin/time -f %M`, the first trace of each page.

| page | `--backend cxx` | rust before | rust after | after vs cxx |
|---|---|---|---|---|
| wiki | 75,300 KiB | 155,296 KiB | 77,596 KiB | +3.0% |
| example-site | 212,772 KiB | 270,764 KiB | 211,816 KiB | -0.4% |

(Two runs each; they agree within 0.2%.)

### Cause: zero-filled texel and buffer storage

gl.cc grows `Texture::buf` and `Buffer::buf` with `realloc`. The new bytes are not written, and glibc serves large
blocks from fresh `mmap` pages, so RSS only counts the rows a draw, clear or upload actually touches. A render
target or texture-cache page that is only partly drawn stays mostly out of RSS.

The Rust `Texture::allocate` and `Buffer::allocate` grew with `Vec::resize(n, 0)`, which writes every new byte and
so touches every page of every texture at allocation: on wiki that was the whole 80 MB difference.

### Fix

`texture::realloc_untouched` grows through `std::alloc::alloc_zeroed` (calloc). Large blocks come back as fresh mmap
pages that the allocator does not write, so they stay untouched like the C++'s `realloc` tail. The old contents are
copied over, as `realloc` keeps them. A failed allocation returns false and the caller cleans up, as gl.cc does.
The output is unchanged: `raster/compare.sh` gives identical frames on all seven pages.

Left over: on growth with contents, the copy briefly holds the old block and the touched part of the new one,
where glibc's `realloc` `mremap`s large blocks in place. It doesn't show at the peak on these traces.

## Round 3: 8 threads on small pages (2026-11-04, after merging rust-raster d2ab5ed)

Wiki, Rust at 1 thread against 8, with the profiler (`gltrace perf-table w1.prof w8.prof`): the keys that lost on
8 threads were `ps_text_run_ALPHA_PASS_TEXTURE_2D` (18.7 -> 22.5 ms, 50 draws of many ~400-pixel glyphs) and
`ps_quad_textured_TEXTURE_2D` with blend 11 (0.14 -> 2.00 ms on 3 draws) and blend 6 (2.9 -> 4.8 ms).
Two costs of a split draw came on top of its pixels:

1. Every draw into a target larger than `RASTER_SPLIT_PIXELS` copied the RasterState and boxed a fragment shader
   clone per primitive, even when its area then kept it on one thread. Now primitives are rasterized in place until
   their areas pass the threshold, and only the rest is copied for the pool (both paths give the same bytes;
   what is drawn in place is finished before the workers start).
2. Draws of small primitives: each worker copies the state and steps the rows of every primitive its stripes meet,
   more work than a glyph's pixels. A draw is split only if its primitives average at least
   `RASTER_SPLIT_MIN_PRIMITIVE_PIXELS` (1024). 4096 lost example-site's 8-thread gain (1134 -> 1401 ms).

compare.sh at 8 threads, then 1 thread, final binary (load average 12 on the build host, so single runs vary by 30% or more):

```
bbc cxx 51.500 ms rust 43.986 ms speed 1.17x frames 28 identical 28 within 0 worse 0 PASS
bench cxx 55.559 ms rust 72.946 ms speed 0.76x frames 4 identical 4 within 0 worse 0 PASS
example cxx 6.958 ms rust 6.430 ms speed 1.08x frames 10 identical 10 within 0 worse 0 PASS
github cxx 34.976 ms rust 43.471 ms speed 0.80x frames 20 identical 20 within 0 worse 0 PASS
hn cxx 11.610 ms rust 22.733 ms speed 0.51x frames 10 identical 10 within 0 worse 0 PASS
example-site cxx 1379.009 ms rust 1299.647 ms speed 1.06x frames 26 identical 26 within 0 worse 0 PASS
wiki cxx 61.809 ms rust 92.864 ms speed 0.67x frames 70 identical 70 within 0 worse 0 PASS
```
```
bbc cxx 46.352 ms rust 59.078 ms speed 0.78x frames 28 identical 28 within 0 worse 0 PASS
bench cxx 50.350 ms rust 76.735 ms speed 0.66x frames 4 identical 4 within 0 worse 0 PASS
example cxx 10.042 ms rust 8.038 ms speed 1.25x frames 10 identical 10 within 0 worse 0 PASS
github cxx 41.139 ms rust 63.549 ms speed 0.65x frames 20 identical 20 within 0 worse 0 PASS
hn cxx 21.175 ms rust 26.341 ms speed 0.80x frames 10 identical 10 within 0 worse 0 PASS
example-site cxx 1561.973 ms rust 2812.087 ms speed 0.56x frames 26 identical 26 within 0 worse 0 PASS
wiki cxx 84.215 ms rust 109.109 ms speed 0.77x frames 70 identical 70 within 0 worse 0 PASS
```

Not done in round 3: the one-thread items (box_shadow pixel loop, find_gradient_stop_pair, scalar lane code in the
gradient span, textureLinearUnpackedR8/R16, set_lum). Also not measured: the pool's own cost (channel allocation
per `Pool::run`; it wakes all 7 workers even when the draw covers fewer stripes), and `ps_quad_textured`
blend 6 being slower on 8 threads although its draws are large.

## Round 4: the one-thread shader loops (2026-11-04)

Read with `objdump -d --no-show-raw-insn -C` on the strict gltrace binary, which holds both backends. The C++
`ps_quad_box_shadow_frag::pattern_fragment` is 1,405 instructions with the bilinear R8 sampling inlined as SSE2.
The Rust one is 414 instructions plus a call to `texture_linear_any`, which calls `textureLinearUnpackedR8`:
621 instructions without one SSE op (220 `mov`, 16 `imul`, 28 `movzwl`, 11 `movswl`, `cmov` clamps,
8 bounds-checked taps). The same scalarized 16-bit lane code is the `pextrw`/`movzwl`/`movswl` seen in the
gradient span function.

1. **textureLinearUnpackedR8 in SSE2** (aec6531): left and right texels in the two halves of one register (the same
   lane arithmetic, so the same bytes), and one bounds check over all 8 taps. On example-site, box_shadow went from 1.76x
   to 1.68x the C++ time.
2. **16-bit lane ops through SSE2** (vector.rs): `+ - *` and `>>` on i16/u16 vectors, and `& | ^` on integer
   vectors, of 8 bytes or whole 16-byte registers, as `paddw psubw pmullw psraw/psrlw pand por pxor`. This is
   x86_64 only; aarch64 keeps the portable code. example-site, Rust/C++ time per key, C++ and Rust run back to back:

| key | before | after |
|---|---|---|
| ps_quad_box_shadow | 1.68 | 1.31 |
| ps_quad_textured_TEXTURE_2D | 2.35 | 1.37 |
| ps_quad_gradient | 2.36 | 1.94 |
| ps_quad_mask_FAST_PATH | 2.30 | 1.89 |
| brush_opacity_ALPHA_PASS | 2.70 | 2.05 |
| ps_quad_repeat | 2.65 | 2.34 |
| example-site, all keys | 1.69 | 1.52 |

compare.sh, final binary, at 1 thread and then 8 threads. Load average was 26 during the 8-thread run, so its
numbers are not comparable with the 1-thread ones:

```
bbc cxx 86.701 ms rust 69.615 ms speed 1.25x frames 28 identical 28 within 0 worse 0 PASS
bench cxx 51.011 ms rust 66.870 ms speed 0.76x frames 4 identical 4 within 0 worse 0 PASS
example cxx 8.646 ms rust 5.946 ms speed 1.45x frames 10 identical 10 within 0 worse 0 PASS
github cxx 33.523 ms rust 48.384 ms speed 0.69x frames 20 identical 20 within 0 worse 0 PASS
hn cxx 13.732 ms rust 22.170 ms speed 0.62x frames 10 identical 10 within 0 worse 0 PASS
example-site cxx 1506.867 ms rust 2728.063 ms speed 0.55x frames 26 identical 26 within 0 worse 0 PASS
wiki cxx 82.909 ms rust 95.434 ms speed 0.87x frames 70 identical 70 within 0 worse 0 PASS
```
```
bbc cxx 58.219 ms rust 58.572 ms speed 0.99x frames 28 identical 28 within 0 worse 0 PASS
bench cxx 79.715 ms rust 77.481 ms speed 1.03x frames 4 identical 4 within 0 worse 0 PASS
example cxx 12.459 ms rust 12.343 ms speed 1.01x frames 10 identical 10 within 0 worse 0 PASS
github cxx 89.337 ms rust 98.514 ms speed 0.91x frames 20 identical 20 within 0 worse 0 PASS
hn cxx 20.879 ms rust 35.704 ms speed 0.58x frames 10 identical 10 within 0 worse 0 PASS
example-site cxx 1831.999 ms rust 2104.135 ms speed 0.87x frames 26 identical 26 within 0 worse 0 PASS
wiki cxx 101.311 ms rust 128.038 ms speed 0.79x frames 70 identical 70 within 0 worse 0 PASS
```

Not done: `find_gradient_stop_pair`, `textureLinearUnpackedR16` (still generic), `set_lum`, NEON for the same
ops on aarch64, and a separate pass on text_run, repeat and Composite. 32-bit lane multiplies (`pmulld` needs
SSE4.1) and float code were left as they are.

## Round 5: AArch64, and the rest of round 4's list (2026-11-04)

- **NEON lane ops** (78cd1a4): `+ - *` and `>>` on i16/u16 vectors and `& | ^` on integer vectors, as
  `add sub mul sshl/ushl and orr eor` on `.8h`, for 8-byte vectors and whole 16-byte registers.
  `textureLinearUnpackedR8` in NEON, the same layout as the SSE2 path.
- **add_mulhi** (R16/RG16 lerps): it was a per-lane emulation of `_mm_mulhi_epi16 << 1` (x86) and
  `vqrdmulhq_s16` (aarch64). It now calls those instructions, which give the same values. In the aarch64 rlib,
  `textureLinearUnpackedR16` was 360 instructions with 13 scalar `madd` and `smov`/`umov` lane moves.
- **find_gradient_stop_pair** (fa8864e): one `Float` load per tree node instead of four bounds-checked `f32`
  loads. **set_lum**, **set_lum_sat** and **clip_color** get `#[inline]`, as blend.h has `static inline`. They
  serve only the HSL blend modes, which no profiled page uses.

x86_64: all 7 pages identical at 1 and 8 threads. The timings are not comparable with earlier rounds (load
average 85 during the run). example-site per key, Rust/C++: box_shadow 1.40, gradient 1.87, mask_FAST_PATH 1.89,
textured 1.33, opacity 1.92, repeat 2.28, page 1.54. Github: Composite (blend 1) 1.34, textured 1.49,
text_run (dual source) 2.30, mask_FAST_PATH 1.59, page 1.50.

AArch64: not run here (no qemu). The static oracle test binaries, built from this source as
tests/oracle/README.md describes, for the laptop:
```
<work-dir>/lucinanet-raster/target-o-a64t/aarch64-unknown-linux-musl/release/deps/ext_signatures-b3127e639743e377
<work-dir>/lucinanet-raster/target-o-a64t/aarch64-unknown-linux-musl/release/deps/lucinanet_raster-1b5bdacef8524537
<work-dir>/lucinanet-raster/target-o-a64t/aarch64-unknown-linux-musl/release/deps/oracle_blend-5592b1fabe973c91
<work-dir>/lucinanet-raster/target-o-a64t/aarch64-unknown-linux-musl/release/deps/oracle_composite-4e0b76c553beb16f
<work-dir>/lucinanet-raster/target-o-a64t/aarch64-unknown-linux-musl/release/deps/oracle_ext-bac0530ccabbaae5
<work-dir>/lucinanet-raster/target-o-a64t/aarch64-unknown-linux-musl/release/deps/oracle_gl-0432d58281019c3d
<work-dir>/lucinanet-raster/target-o-a64t/aarch64-unknown-linux-musl/release/deps/oracle_glsl-204e27585328b2db
<work-dir>/lucinanet-raster/target-o-a64t/aarch64-unknown-linux-musl/release/deps/oracle_texture-4047d4e8a3727a1b
<work-dir>/lucinanet-raster/target-o-a64t/aarch64-unknown-linux-musl/release/deps/rasterize_flat-74399b3763281ab5
```

Left: ps_text_run (2.3x on github), ps_quad_repeat (2.3x on example-site), and the AArch64 timings on the laptop.

## Round 6 (2026-11-04): ps_quad_textured blend 6, partly

- The blend column of the profile is `ctx->blend_key`, also when GL blending is off. With temporary span
  counters on both backends, the github blend-6 draws took the same texture paths with the same counts on C++ and
  Rust (nearest, non-blending: 57,513 spans, 1.26M px). Most of the key's 6.9M px are not texture spans. So the
  gap is not a fast path the Rust misses. Re-measured, it is 1.95x, not 3.7x (C++ 0.19 ns/px).
- `blendTextureNearestFast`: the C++ inner loop is `movups` load and store. The Rust one bounds-checked every
  4-texel chunk (three compares, a cold partial-load call) and moved it through the stack. Now there is one check
  for the whole run of chunks, then unchecked loads (`ExtPx::tex_chunk_unchecked`). github blend 6: 1.95 -> 1.91.

github, one thread, after it (load average 31): textured blend 2 1.89, Composite 1.35, text_run (dual source)
2.05, mask_FAST_PATH 1.97, page 1.39.

Not done in round 6: ps_text_run, ps_quad_repeat, Composite. No machine-code reading on AArch64 for these keys,
and no search for scalar `ins`/`umov` lane code in the aarch64 library.

## Round 7 (2026-11-05): open AArch64 failure, oracle_ext texture_yuv

On the laptop, round 5's oracle_ext fails `commitTextureLinearYUV2` case 7 (R16 + RG16 planes, rescale 4, 2x7
texture, blend key 25). B and R are 1 level high on pixels 10 and 11, G is right, and x86_64 passes. Read and
found equal to swgl's AArch64 build: the R16/RG16 lerps (swgl `sqrdmulh`, Rust `vqrdmulhq_s16`), the 16-bit NEON
shifts (`sshl`/`ushl` by `-n`), `zip2Low`/`zip2High`, `YUVMatrix::From` and `convert`, `packYUV`, and the
double-to-int16 conversions. The R16/RG16 samplers pass oracle_texture on the laptop, and the 8-bit YUV cases pass.
No AArch64 machine or emulator here (docker gives "exec format error"), so it is not fixed. To bisect, run on the
laptop oracle_ext built from 3c3ca4f (before round 5's NEON):
<work-dir>/lucinanet-raster/target-bisect-a64/aarch64-unknown-linux-musl/release/deps/oracle_ext-ff9d01de1139c416
If it fails the same way, the bug predates round 5, because the laptop had not run oracle_ext before.

Round 8 (2026-11-05): the bisect binary from 3c3ca4f fails the same way, so this predates round 5's NEON. Narrowed
down with the new test switches:
- `ORACLE_KEY=0` (no blending): the YUV output of case 7 is the constant (53, 255, 195, 255) on both sides on x86;
  the sampling and conversion are not where the difference is.
- `ORACLE_DUMP`: on x86 both C++ and Rust give B 99 / 86 for pixels 10 / 11 (bytes 40 / 44; R is the same on
  every side). The AArch64 C++ alone gives 98 / 85. The Rust gives 99 / 86 on both ISAs.
- So the difference sits in the HSL hue blend (`DO_HSL`), and only in the AArch64 C++. Every per-ISA branch of
  swgl on that path has a matching Rust branch (packRGBA8/packR8 saturation, pack, clampCoord, roundfast,
  recip/inversesqrt/sqrt, min/max, abs), and neither AArch64 binary has a fused multiply-add.
- `oracle_blend blend_pixels_hsl_yuv_chunk` runs blend_pixels alone on exactly that chunk. If it passes on
  AArch64, the AArch64 C++ result depends on blend_pixels being inlined into blendYUV (codegen, not source
  semantics). If it fails, the AArch64 HSL arithmetic differs for this input, which the 10,000 random cases per
  key in blend_pixels_rgba8_every_key did not hit.

## Round 9 (2026-11-05)

YUV2 on AArch64: the laptop showed the blend chunk equal and `ORACLE_KEY=0` still failing (B 52 vs 53 from byte
0), so the difference is in the YUV2 R16 sampling or conversion. `texture_yuv` now compares the stages of each R16
case's first chunk (`x_yuv2_r16_stages` / `ext::yuv2_r16_stages`: taps after LINEAR_QUANTIZE_UV and the clamp, the
R16/RG16 samples, the colour-depth shift, YUVMatrix::From, convert) and panics naming the first stage that differs.
All equal on x86_64; the AArch64 binary is to be run on the laptop.

AArch64 speed, box_shadow first (Rust aarch64 rlib next to the shipped juno swgl gl.o for aarch64):
- Both `pattern_fragment`s have the same 9 scalar `fdiv` (inverse_radii_squared on flat inputs, once a chunk in
  both), so divisions are not the gap.
- The Rust R8 bilinear sampler converted I32 to I16 lane by lane (computeFracX: umov/and/add/lsl/orr per lane)
  where clang emits `xtn`. 32-to-16-bit converts are now `vmovn_s32` on aarch64 (textureLinearUnpackedR8 356 -> 334
  instructions). The remaining lane moves are the tap address extractions, which the C++ has too.
- The C++ sampler (textureLinearR8, 102 instructions) is out of line in both. The Rust adds its one bounds check
  over the 8 taps (64-bit compares, `cmhi`).
Not done: repeat, mask_FAST_PATH, cs_blur, gradient, opacity, text_run; no laptop timing of this change yet.

## Round 10 (2026-11-05)

- **AArch64 texture_yuv was the test, not the port.** The stage oracle named the RG16 samples, and the cause was
  a 1-texel-wide linear plane (UV planes were `max(w / 2, 1)` wide; Y planes 1-3 wide 10% of the time; YUY2 2 wide).
  There swgl's taps start at x = -1 (`clampCoord(x, width - margin)`) and read before the texture, bytes that
  belong to the allocator: zero on glibc x86_64, not on the laptop's musl. The Rust clips that read (DESIGN
  rule 5). oracle_ext's textures now start after a 16-byte 0xA5 guard, which reproduced the laptop failure on x86
  (case 4, same stage), and YUV planes are at least 2 wide (YUY2 4). oracle_texture already uses zero guards on
  both ISAs.
- **lerp_lanes** (the span filters' 16-lane lerp, e.g. LINEAR_FILTER_FAST, where ps_quad_repeat spends its time in
  ~49-pixel sub-spans) was an element loop over `[i16; 16]`; it now goes through V16<i16> (SSE2 / NEON).
  example-site x86 repeat 2.43 -> 1.96, textured 1.51 -> 1.37.
- **packRGBA8 / addsat_i16** as `_mm_packs_epi32` / `vqmovun_s32` and `_mm_adds_epi16` / `vqaddq_s16`.
  example-site x86 total 1.57 -> 1.48.
- Tried and dropped: `#[inline(always)]` on textureLinearUnpackedRGBA8 (AArch64 swgl inlines it into the repeat
  loop; the Rust aarch64 rlib calls it). No x86 gain; it needs a laptop A/B first.
The laptop timings of rounds 9 and 10 are still to be taken.

## Round 11 (2026-11-07, task SP-sp1): ps_quad_box_shadow and cs_blur

Baseline on x86 (strict gltrace, fat LTO, one codegen unit, `LUCINANET_SWGL_PROFILE=1`, CPU 5, example-site):
`ps_quad_box_shadow` C++ 704.17 ms / Rust 1135.25 ms (1.61x), bench 49.79 / 62.95 (1.26x);
`cs_blur_COLOR_TARGET` blend 2 example-site 3.53 / 7.18 (2.03x), bench 1.26 / 3.09 (2.45x).

### cs_blur: per-load bounds checks and spilled rotate registers in `gaussianBlur*`

Read against clang's strict build (`blendGaussianBlur<true, sampler2D_impl*, unsigned int>`, whose inner loop is
two `movdqu` chunk loads, two `mulss`, four `punpck*`, two `paddw`, one `cvttss2si` + splat, two `pmullw`, two
`paddusw` per offset, no checks): the Rust `gaussianBlurHorizontal_RGBA8` was 0xbd6 bytes with 195 `mov`,
56 `movzbl`, 40 `punpcklwd`, 21 `cmp` and 5 cold `load_texels_partial` calls on x86; on AArch64 0xbb8 bytes with
231 `mov`, 32 `stp`/`28 stur`/`17 ldur` (the chunk registers spilled around the `pixels_right.0[0] = ...` lane
store) and per-load `cmp`/`b`/`csel`. Every texel load went through `load_texels`' bounds check where swgl reads
raw pointers.

Fix (37941c7), bytes identical on all 7 pages at 1 thread:
- `blend_gaussian_blur` proves once per span that every read is inside the sampler — `rules::blur_len` gives
  `cur_uv.x + n <= width`, so each chunk's row is `x + y*stride` with `x <= width - 4`; the horizontal loops clamp
  their reads to `[row - left_bound, row + right_bound] ⊆ [y*stride, y*stride + width)`, the vertical loops read
  rows `[max(lo,0), min(hi, height-1)]` — so `(height-1)*stride + width <= len` in texels plus `0 <= y < height`
  suffices — and calls the blur with `UNCHECKED=true`, whose loads are the new `samplerCommon::at_unchecked`.
  The checked variant stays as the fallback when the proof fails (texture edge).
- The horizontal valid-radius loop now loads the whole shifted chunk (`chunk(row ± offset)`), which holds exactly
  the texels the C++'s rotated registers hold (`offset + 3 <= right_bound`, `offset <= left_bound`), instead of
  one pixel plus a rotate through memory; the clamped loop uses the closed form `row ± min(offset ± j, bound)`.
- `addsat` is now `_mm_adds_epu16` / `vqaddq_u16` (it was a `saturating_add` lane loop).

x86 after: bench `cs_blur_COLOR_TARGET` blend 2 3.09 -> 1.96 ms per replay (2.45x -> ~1.4x of C++; the machine was
at load 35-44, so ratios within one back-to-back run are the reliable part); example-site blend 2 7.18 -> ~6 ms
(139k px, noisy). The `UNCHECKED=true` variant is 0x5a4 bytes with a check-free inner loop.

### ps_quad_box_shadow: two calls plus the shared format dispatch per chunk for the R8 sample

The C++ `pattern_fragment` (0x18ad bytes) has the R8 bilinear inlined and calls only `texelFetch` (cold NEAREST
branch); `texture(sampler2D, vec2)` is `SI`, so the format switch is inlined at the call site and `textureLinearR8`
(one call, sampler passed as a pointer) holds the inlined `textureLinearUnpackedR8`. The Rust `pattern_fragment`
copied the ~40-byte sampler to the stack and called `texture_linear_any` (the shared 7-format dispatch), which
called `textureLinearUnpackedR8` — two calls plus the switch per 4 pixels.

Fix (37941c7): `TextureSample<vec2> for sampler2D_impl` now matches the format itself and calls `textureLinearR8`
/ `textureLinearRGBA8` directly for the two common formats (the rest still go through `texture_linear_any`), and
`textureLinearUnpackedR8` is `#[inline(always)]` as clang inlines it into `textureLinearR8`. One call per chunk now.

x86 after: example-site `ps_quad_box_shadow` 1135.25 -> 921.72 ms (Rust/C++ 1.61 -> 1.33); bench 62.95 -> 50.45 ms.
The 9 scalar `fdiv` (`inverse_radii_squared` on the flat varyings) are the same in both backends (the Rust even
vectorizes the four corners into two `divps`), so divisions are not the remaining gap; what is left is the
per-chunk `run_func` -> `pattern_fragment` call with the `vec4` result through the stack, which swgl also pays
(`run` -> `pattern_fragment`), and the blend commit.

Not done: laptop (AArch64) timings of this round; the AArch64 gltrace built from this commit is at
`<work-dir>/lucinanet-raster/target-sp1-a64/aarch64-unknown-linux-musl/release/gltrace`.

## Round 11 (2026-11-07): brush_opacity

`brush_opacity_ALPHA_PASS` spends its span in `swgl_commitTextureLinearColorRGBA8` -> `blendTextureLinear<true>` ->
`blendTextureLinearDispatch` -> `blendTextureLinearFast` (1:1 scale with a subpixel offset), with the color
`packColor(swgl_OutRGBA8, v_opacity)` applied by `muldiv255` per chunk. example-site x86, one thread, strict gltrace,
`LUCINANET_SWGL_PROFILE=1`: C++ 255.17 ms / Rust 270.62 ms (13.03 vs 13.82 ns/px, 1.06x) before the round; the laptop
(AArch64) had 1.50x.

The Rust `blend_texture_linear_fast::<true, 4, sampler2D_impl, Vector<u16, 16>, *mut u32>` loop was 137 instructions
per 4-pixel chunk against clang's ~50 in the C++ `blendTextureLinearFast` inlined into `blendTextureLinearDispatch`
(the AArch64 build's loop was ~93 with `stur`/`ldur` stack traffic). Two causes, both fixed in
`raster/core/src/ext.rs` only:

- **The signed-unpacked texel load went through an `[i16; 16]` array.** `tex_signed` returned the array and
  `lerp_lanes` took it back, so each chunk's two 16-byte loads were stored to the stack and reloaded as `movq`
  halves (`stur`/`ldur` on AArch64), each half widened by its own `punpcklbw`. The C++ is one `CONVERT` per load
  (`punpcklbw`+`punpckhbw` with a zero register on SSE2, `ushll`/`ushll2` on NEON) straight into registers. The
  helpers now pass `V16<i16>` end to end (`tex_signed` is `at32::<PackedRGBA8>(idx).convert::<i16>()`, and
  `lerp_lanes`/`shuffle_px`/`spread`/`wide_from_signed` take and return `V16<i16>`), so the widening is one convert
  per load and nothing touches the stack.
- **A bounds check per texel load** (`at32`'s compares and its cold partial-load branch, twice per chunk). swgl's
  fast filters read unchecked, trusting the dispatch's `inside` cap of `(width - swgl_StepSize) *
  swgl_LinearQuantizeScale`. A first version of this round did the same with `tex_signed_unchecked`; the unsafe
  review (2026-11-07) refused it: `inside` bounds the right edge only, and a span that starts left of `min_uv`
  (when the `before` block does not cover it) reads before the row. The loads stay checked (`tex_signed`); a
  per-span bounds check hoisted out of the loop is the open follow-up.

- The first version of `shuffle_px` numbered the shuffle lanes as `idx[p] * C + ch`, which is only right for
  RGBA8 (`C = 4`, where `b`'s first pixel is lane 16 of the `V16` shuffle); for R8 (`C = 1`) it sent pixel 4 to
  `a[4]` (zero) instead of `b[0]`. `oracle_ext::texture_linear` case 32 caught the one-byte difference; the lanes
  are now `q` for `q < 4*C` and `q + 16 - 4*C` beyond, the old element-loop mapping for both channel counts.

x86 with unchecked loads: the chunk loop was 119 instructions (was 137) — no bounds checks, no stack traffic, loads as
`movq`+`punpcklbw` pairs, two `shufps $0x4e` for the `[1,2,3,4]` shuffle. example-site brush_opacity best of 7 paired
replays 1.04x of C++ (was 1.06x); the box was saturated by the sibling rounds (load 34-60, the C++ side alone swung
207-491 ms across identical replays), so the x86 delta is inside the noise here — the laptop AArch64 run of the new
gltrace is the real measurement. All 7 pages identical at 1 and 8 threads (compare.sh), raster/core tests and
clippy `-D warnings` clean.

Not done: the blend-key switch inside the chunk loop (clang unswitches it out of the C++ loop; LLVM keeps the
compares and their spilled reloads in the Rust one, ~10 instructions a chunk).

## Round 11 baseline at low load (2026-11-07, rust-raster 5d27193)

Earlier x86 round-11 numbers were taken at load 34-60, where the C++ side alone swung 207-491 ms across identical
replays; they understated the gap. Measured again at load ~25, both backends of one strict LTO gltrace pinned to
CPU 5 (`taskset -c 5`), alternating, example-site trace 1, one thread (`gltrace perf-table`, Rust/C++):
ps_quad_box_shadow 1.44, ps_quad_gradient 2.03, ps_quad_textured 1.44, brush_opacity 2.21 (C++ 125-136 ms over five
runs, Rust 276-288), ps_quad_mask_FAST_PATH 1.65, ps_quad_repeat 2.07, brush_image 1.74, text_run 1.44-1.69,
cs_blur 1.65-2.07; Clear, BlitFramebuffer and Composite at parity. The C++ shim and the Rust build use the same
baseline ISA (clang `-O2`, no `-march`; Rust default target), so this is a code-generation gap in the span loops,
not an ISA one. The AArch64 numbers on the laptop decide; x86 is the guide.

Branch `raster-sp4` (reviewed): one exact bounds check per span for the fast and downscale linear
filters (`rules::linear_reads_in_bounds`, Verus-proved, 5 planted bugs refused), unchecked loads under it. Every
page identical at 1 and 8 threads, but brush_opacity on x86 is unchanged (275-292 ms against 276-288), so the
per-load check is not where the time goes. Not merged until AArch64 shows a gain.

## Round 11 on the laptop (AArch64, 2026-11-07, rust-raster 5d27193)

Laptop (SC7180), no other sharers, strict swgl vs Rust, `LUCINANET_SWGL_PROFILE=1`, one replay each.

- Oracles: texture 8/8, blend 7/7, composite 7/7, glsl 14/14, gl 6/6, rasterize_flat 14/14 pass. oracle_ext printed
  `validate_and_state ... ok`, then the laptop reset during its other six tests (all CPUs busy); juno-crash-recovery
  is reading that boot's crash record and runs an A/B with it.
- Frames: bench 4, wiki 70, example-site 26, all byte-identical to swgl at 1 and 8 threads (md5 per frame).
- Per-shader totals (Rust/C++): example-site 1.06 at 1 thread, 0.98 at 8; wiki 1.04 at 1 thread, 1.13 at 8.
- AArch64 gap is much smaller than x86 (gradient 1.46, box_shadow 1.22, brush_opacity 1.08, textured 0.91 at 1 thread).
- 8 threads barely help: example-site 8841 -> 8156 ms. Big keys gain (box_shadow 0.85, repeat 0.76, brush_opacity 0.83)
  but small draws lose (text, brush_image, blur ~1.8-2.9x), so the split overhead on small draws and the six slow
  Silver cores are the next targets.

## Round 12 (2026-11-13, task raster-neon): a tracked flaw, scaled YUV on AArch64, under qemu

The flaw (docs/media.md "aarch64 macOS: planes off by default"): a video WebRender scales, drawn from I420 planes by
the Rust image path, comes out as vertical colour bars; the same page with BGRA frames and the 1:1 compositor-surface
path are correct; x86_64 is correct. `surfaces.rs` starts BGRA on that platform until this is fixed.

Nothing in `raster/` covered the scaled image path as an image: `texture_yuv` and `composite_yuv_every_color_space_and_depth`
draw one span of 1-40 pixels per call, so no test ever compared two output rows, and `linear_unchecked` checks reads,
not pixels. Added `oracle_ext texture_yuv_scaled_image_path` and `..._padded_strides`: a 4:2:0 picture (diagonal
gradient + checkerboard, every plane varying in x and y) drawn row by row through `swgl_commitTextureLinearYUV3`, the
entry the image shader reaches, at 12 scaled renders (320x240, 640x360 and 1280x720 in; 960x270 / 942x707 / 1920x1080
out; `sampler2D` and `sampler2DRect`), 640x360 at 1:1 as the CompositeYUV control, and 16 more at row strides
641/642/643/646. Two assertions: the Rust must equal the C++ oracle byte for byte, and no output column may be
constant down its height - the vertical-bar signature, which a function-of-x-only output fails at every column.

No repro. Every case passes on x86_64 and under `qemu-aarch64 -cpu max` on `aarch64-unknown-linux-musl`, and the FNV of
the whole Rust image is identical on the two ISAs in all 29 cases (`SCALED_DUMP=1`); a build with
`-C target-cpu=apple-m1` (SVE2/dotprod codegen) passes all 29 as well. The whole aarch64 suite under qemu is green:
blend 7, composite 7, ext 9, gl 6, glsl 14, texture 8, linear_unchecked 1, rasterize_flat 14, ext_signatures 1; x86 the
same plus 19 lib tests. So `linear_r8_neon`, `cvt_array`'s narrowing, `clampCoord` and `add_mulhi` are not what the Mac
does wrong, and no memory-layout-independent difference exists between the two ISA builds at these coordinates.

That the pass is not a vacuous one was checked by mutation, on the same build: making `linear_r8_neon` take every tap
from source row 0 - a hand-made version of the reported symptom, and aarch64-only, so x86 cannot be the control for it -
fails both assertions under qemu. With the oracle: `scaled YUV3 320x240->960x270 2D stride 320 row 0: Rust != C++ oracle
at pixel 4 (C++ 0xff000642, Rust 0xff00023e)`. With the oracle output suppressed: `960 of 960 output columns are
constant down their height`. A first, weaker mutation (lerp row 0 against the true row) was caught only by the oracle
and not by the column check, which is why the bars assertion needs a picture that varies in y and a mutation that makes
the taps independent of y. Both were reverted.

What qemu cannot cover, and where the bug therefore still lives:
- qemu is a functional model: it executes the NEON the host compiler selected, so an Apple-clang miscompile (or an
  intrinsic whose result real M1 hardware differs on) is invisible here. The Mac build is clang's own, on a different
  LLVM version, for `aarch64-apple-darwin`.
- memory layout and allocator: the plane buffers here are Rust `Vec`s with a 0xA5 guard; on the Mac the planes are
  decoder-owned bytes under jemalloc, so an out-of-row or out-of-plane read gets *those* bytes. As the scoping report
  put it, a clean qemu run does not exonerate suspect #2 (an edge tap reading bytes that are zero only here) - it only
  says the code path itself agrees across the ISAs. Rounds 7 and 8 chased exactly such a difference (the AArch64 C++
  of a 1-texel-wide linear plane) and round 10 closed it as the allocator's bytes, not the port.
- the plane geometry the media path actually hands the sampler: a decoder's padded rows, chroma at half a padded luma
  stride, an odd width. `..._padded_strides` covers strides 641/642/643/646 here; a real Mac frame is the check.
- libc: macOS zero-fill and page behaviour differ from musl, so a read of uninitialized plane bytes shows different
  contents there.

The two deciding runs are the scoping report's, on the Mac, with the planes forced back on (`surfaces.rs` now starts
BGRA there by default, so the pages must say `LUCINANET_MEDIA_YUV=1`):

    LUCINANET_MEDIA_YUV=1 LUCINANET_CONSOLE=1 LUCINANET_RENDER_HOLD=12 LUCINANET_PROFILE_DIR=none \
      ./lucinanet --render "http://127.0.0.1:$PORT/video-size/page/page.html" out/a2.png            # A2
    LUCINANET_RASTER=swgl ... same ... out/a5.png                                                  # A5

A2 (Rust rasterizer, scaled page, `LUCINANET_RASTER_THREADS=1` to rule the 32-row thread stripes out) says whether the
scaled image path is still wrong at all on the Mac build; A5 (C++ swgl over the same bytes) says whether the Rust
kernel or the bytes are to blame. Both barred → the plane geometry or its memory, not `linear_r8_neon`; A2 barred and
A5 clean → the Rust aarch64 kernels as Apple clang builds them; A2 clean → the flaw is already gone or was never in
`raster/`, and `surfaces.rs`'s exception should be re-measured and dropped.

> Note (2026-11-13, task yuv-on-again), on the wording above: the BGRA default that made these runs pass
> `LUCINANET_MEDIA_YUV=1` was an interim for `a tracked flaw` and is reverted. It was neither the aarch64 kernels nor the plane
> geometry nor the allocator: the media planes' uv rect was handed to the sampler unflipped. The engine pinned at `40c0ff93c`
> hands it flipped, and with it `tests/media/scaled-planes.sh` passes on the build host x86_64 as on the owner's Mac, so planes are
> the default on every platform again. `LUCINANET_MEDIA_YUV=1` in the commands above is now needed only against an older
> build; `=0` is what asks for BGRA.
