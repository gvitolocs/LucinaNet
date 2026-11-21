# LucinaNet logo

The LucinaNet mark is a ray of light bent inside a glass tile, with a point of light above the bend. Light is
what the browser puts on the screen, and glass is how its interface looks: the ray enters the tile shallowly,
crosses the middle, and leaves steeply — refraction, in two straight legs and one kink.

Everything else in this directory is that one drawing, in the colours and at the sizes the project needs.

## The drawing

A 64 x 64 unit box holds a tile 56 units square with a corner radius of 18, inset 4 units on every side. The ink
is a stroke of 5.4 units — 8.5 % of the box — with round caps and joins, the same weight as the interface icons,
so the mark and the chrome are visibly one family.

| Part | Shape | Colour |
| --- | --- | --- |
| tile | rounded rectangle, 56 units, radius 18 | `#7f8fe0` |
| rim | the tile's own edge, stroke 2, 27.5 % white | `#ffffff` at 0.275 |
| ray | `(13,19)` → `(33,29)` → `(51,47)`, stroke 5.4 | `#f2f2f7` |
| ghost beam | `(13,36)` → `(26,42.5)`, stroke 5.4 at 55 % | `#f2f2f7` at 0.55 |
| point of light | disc, radius 9.35, centred `(46,17)` | `#f2f2f7` |

The point of light is deliberately the heaviest element: a disc of radius 9.35 has three times the area of a disc
of radius 5.4, the stroke width, so the mark keeps its shape at a glance even when the ray's two legs are only a
few pixels long.

The two deeper and lighter pairs of colours are `#4a5bb5` and `#f2f2f7` with a rim of black at 18 %, used by the
light and dark variants below.

## Files

| File | What it is |
| --- | --- |
| `lucinanet.svg` 1464 B | the mark: tile, ray, ghost beam, point of light |
| `lucinanet-small.svg` 1257 B | the same mark simplified for small sizes: no ghost beam, no rim, stroke enlarged from 5.4 to 7.2 units and the point of light set to radius 8.0 so the kink reads at 16 px |
| `lucinanet-mono.svg` 946 B | one colour (`currentColor`), no tile; the ray, the ghost beam and the point of light are three separate shapes |
| `lucinanet-light.svg` 1051 B | for a light background: tile deepened to `#4a5bb5` (5.6:1 against `#f6f6f3`, where `#7f8fe0` reaches only 2.8:1) |
| `lucinanet-dark.svg` 1110 B | for a dark background: the tile inverted to light glass `#f2f2f7` with a black rim at 18 %, ray and point of light in `#4a5bb5` (16.2:1 against `#16161b`) |
| `lucinanet-512.png` 23008 B, `lucinanet-256.png` 19344 B, `lucinanet-128.png` 8701 B, `lucinanet-64.png` 4142 B | the mark at those pixel sizes, transparent ground, square |
| `lucinanet-32.png` 1620 B, `lucinanet-16.png` 948 B | the small icon at those pixel sizes, transparent ground, square |
| `social-preview.png` 62526 B | 1280 x 640: the mark left, the name and a one-line tagline right |

Every SVG is hand-written from shapes only: no script, no external reference, no embedded raster, no
`<foreignObject>`, no text (so nothing depends on a font being installed), `viewBox` and `width`/`height` set, and
a `<title>` and `<desc>` for accessibility. The largest is 1464 bytes.

The 16 and 32 px PNGs are rendered from the simplified `lucinanet-small.svg`; the 64, 128, 256 and 512 px PNGs and
the social preview are rendered from `lucinanet.svg` and `lucinanet-light.svg`. The PNGs were rendered from the
SVGs with a headless browser (512 px master, then downscaled); the SVGs are the source. The social preview sets its
text in DejaVu Sans with a generic sans-serif fallback — a system font, not a file that ships here, and no SVG in
this directory contains text at all.

## Using it

* In a README or a page: `lucinanet.svg`, or `lucinanet-light.svg` / `lucinanet-dark.svg` when the background is
  near-white or near-black.
* In an app bar, a tab strip or a taskbar: `lucinanet-32.png` and `lucinanet-16.png`, or `lucinanet-small.svg`.
* `lucinanet-mono.svg` takes the colour of the text around it. In an `<img src>` context `currentColor` does not
  inherit from the page and resolves to black, so inline the `<svg>` — or use it as a CSS mask — to get the
  inherited colour.
* Nothing of the mark ever crosses the edge of the tile. In the full mark the point of light sits 2.25 units inside
  the tile's corner arc, 3.65 units from the top edge and 4.65 from the right edge, and is 5.64 units clear of the
  ray's stroke; the tightest clearance anywhere in the drawing is that 2.25 units, which is just over half a pixel
  at 16 px. Measured against the tile's own silhouette, the ink outside it is 0 at every rendered size, 16 px
  included.
* At 16 px the small icon resolves into exactly two separate pieces of ink — the bent ray and the point of light,
  with a visible gap between them — which is what makes it read as the mark and not as a smudge. Its point of light
  is radius 8.0 rather than 9.35 because at 16 px a 9.35 disc touches the enlarged ray and the two merge into one
  blob; 8.0 keeps a gap of 1.5 px.

## Licence

The mark and its files are MIT-licensed (see the licence text below). The name LucinaNet and the mark identify the
project.

    MIT License

    Copyright (c) 2026 LucinaNet contributors

    Permission is hereby granted, free of charge, to any person obtaining a copy
    of this software and associated documentation files (the "Software"), to deal
    in the Software without restriction, including without limitation the rights
    to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
    copies of the Software, and to permit persons to whom the Software is
    furnished to do so, subject to the following conditions:

    The above copyright notice and this permission notice shall be included in all
    copies or substantial portions of the Software.

    THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
    IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
    FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
    AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
    LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
    OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
    SOFTWARE.
