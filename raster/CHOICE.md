# The rasterizer: which Rust path replaces swgl (2026-11-04)

> 2026-11-11: the Rust rasterizer is the default backend of LucinaNet; `LUCINANET_RASTER=swgl` selects the C++ oracle (swgl stays in the build as the oracle and the benchmark).

All runs on the reference Linux development machine (x86, Ryzen, 12 threads), pinned to CPUs 4-11, LucinaNet `juno` profile (swgl at O3), 1920x1080. The
machine was hot and busy throughout (load 47-61, Tctl 95-97 °C), so absolute times are high and multi-thread scaling
could not be measured fairly; the comparisons are within one run of each tool. [run]

## What swgl spends its time on

`LUCINANET_SWGL_PROFILE` (new, in `swgl/src/lucina_prof.h`) times every swgl call by shader and blend mode.

| example-site, idle with its CSS animations, three 2 s windows | |
|---|---|
| swgl's share of the engine thread | 1076-1140 ms of each 2 s, on one thread |
| `ps_quad_box_shadow` | 369-383 ms, 16-18 Mpx, **21-24 ns a pixel** |
| `ps_quad_gradient` | 200-250 ms, 17 Mpx, 12-15 ns a pixel |
| `ps_quad_textured` | 172-189 ms, 34 Mpx, 5 ns a pixel |
| `ps_quad_mask_FAST_PATH` | 92-119 ms, 11-12 Mpx |
| `ps_quad_repeat`, clears, composite, blits | most of the rest |
| Everything that is not per-pixel work (uploads, state, vertex setup) | under 2% |

Five quad shaders are 85-92% of swgl's time; per-pixel work of any kind is over 98%. WebRender's phase timing on the
same page: offscreen color targets 82-99 ms and picture tiles 9-20 ms a frame, 12-23 Mpx of offscreen pixels per frame
for a 2 Mpx screen. All of it runs on the engine thread while seven other cores idle. On Wikipedia scrolling, swgl is
130-170 ms per 2 s, mostly compositing and text.

## The same geometry through each candidate

`raster/bench/paint-bench.html`: a gradient page, a gradient header with a blurred rounded blob, an opacity group, and 48
cards with box shadows, borders, radial-gradient dots and rounded bars, every box absolutely placed so layout cannot
differ. `raster/bench/vello-bench` draws the identical scene with `vello_cpu` 0.3.0 and diffs it against Servo + swgl's
PNG of the page.

| | swgl (Servo, WebRender) | vello_cpu 0.3.0 |
|---|---|---|
| Full frame, one thread | 60-90 ms (picture tiles 35-52, offscreen 24-36, shadow caches) | 29-35 ms (min 28.8) |
| Threads | one, by design: WebRender draws every target from one GL context | 1/2/4/8 threads: min 27.6/27.6/25.9/32.3 ms. No scaling seen on the loaded machine; not a fair test |
| Pixels differing from swgl | 0 | 30% of all pixels (622,510); 326,448 by more than 2 levels; 4,608 by more than 32 |
| Where | | every box shadow, every anti-aliased curve (dot rims, bar ends, card corners), gradient interiors |

vello_cpu is 2-3 times faster per core on this scene. It cannot give swgl's pixels: its coverage-based anti-aliasing,
its blur and its gradient sampling are different algorithms, so the differences are on every edge of every page, not a
bug to fix.

## Choice

| Path | Verdict | Why |
|---|---|---|
| **Rust port of swgl's behaviour**: the C++ runtime ported module by module, and WebRender's 78 GLSL programs translated to Rust by a fork of `glsl-to-cxx` that emits Rust | **adopt** | Same pixels as swgl by construction, so parity pages and WebRender's own invariants hold. Keeps WebRender: its picture cache, render-task graph, `SwCompositor`, and the scroll-root, animation and diagnostics patches. And it can use all 8 cores: >98% of swgl's time is per-pixel work, which splits by rows with each pixel still written by one thread in order (`raster/DESIGN.md`). Work: about 15k lines of C++ runtime plus the emitter (glsl-to-cxx's `hir.rs` is kept) |
| WebRender display lists painted by `vello_cpu` / `vello_hybrid` (through anyrender or tiny-skia) | **reference** | Faster per core today, but 30% of pixels differ from swgl on a paint-only page, so "matches swgl on the parity pages" is unreachable. It replaces WebRender's renderer, not swgl: picture caching, invalidation and compositing would be rebuilt, and the patches the `lucinanet` session made to WebRender lost. Blitz already ships this route and stays in the parity run. Reference for: vello_cpu's strip-based threading and its box-blur shortcut, if the Rust port needs faster shadows later (only behind a switch, since it changes pixels) |
| `vello_hybrid` | **reject** | Needs a GPU backend (wgpu or WebGL2); Juno has neither |
| Rust GPU paths on the Adreno 618 through `/dev/lucina-gpu` | **reject for rasterizing, reference for compositing** | No shader compiler exists or can run on Juno: each of WebRender's 78 programs would be compiled by Mesa's IR3 on the reference Linux development machine and pasted in, as the one quad pipeline was. The GPU cannot draw into buffers yet, only the screen and one surface. Nothing runs on that machine, so no pixel parity loop. Compositing finished tiles on the GPU stays the documented next step for scrolling, independent of this choice |

## Existing Rust work on swgl or WebRender's GL path (checked 2026-11-04)

No Rust port of swgl or of WebRender's renderer exists: Servo uses vello only for canvas, and WebRender 0.70 still
ships swgl. Two crates came up; their sources were read, nothing built or run.

| Crate | What it is | Verdict | Why |
|---|---|---|---|
| `portablegl` 0.9.0 (2026-03-16, 322 downloads, MIT) | A Rust port of PortableGL: OpenGL 3.x core in software, 10.5k lines, shaders as Rust functions | **reference** | Its rasterizer walks each triangle's bounding box one pixel at a time with barycentric weights (`gl_internal.rs` `draw_triangle_fill`), scalar, no SIMD and no threads; swgl shades 4 pixels at a time along spans and has span fast paths, so it would be slower than what it replaces. It also lacks what WebRender needs from swgl: the `swgl_*` GLSL extensions WebRender's shaders call under `#ifdef SWGL`, AA blend keys, dual-source and KHR advanced blending, R8 targets, locked and external texture buffers, `Composite`. Using it means rewriting its core, and the pixels would not be swgl's. Worth reading for its separate-equation blending and clipping if a question comes up |
| `zng-swgl` 0.5.1 (2026-04-17) | swgl renamed for the Zng UI toolkit, built with `zng-glsl-to-cxx` 0.3 | **reject** | The same C++ (15.2k lines; its Rust is only the `swgl_fns.rs` wrapper, 2.8k lines). Its `FORK.md` says Zng has moved back to upstream WebRender and the fork only gets security fixes |

Neither saves work, so the port continues.

## What "done" means for the adopted path

1. One thread: byte-identical to swgl on every replayed trace of the parity pages (only FMA contraction may differ; see DESIGN).
2. Eight threads: a measured, reported difference that is only the last bit at stripe starts.
3. Faster than swgl on example-site idle frames and Wikipedia and example-site scrolling, on real hardware, with an installed generation.
4. swgl's C++ out of the Juno binary.
