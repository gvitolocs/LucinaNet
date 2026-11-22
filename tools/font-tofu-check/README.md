# font-tofu-check

Reads the PNG an engine rendered of `tests/fonts/cjk-emoji.html` and says, per block of characters, how many cells drew
as tofu (the notdef box) and how many drew nothing. Its own Cargo workspace, like `tools/wpt-reftest` and
`tools/scroll-bench-tables`; it links no engine.

```sh
font-tofu-check --page tests/fonts/cjk-emoji.html --size                      # 1536xH: what to render the page at
font-tofu-check --page tests/fonts/cjk-emoji.html --png render.png            # the table, exit 0
font-tofu-check --page tests/fonts/cjk-emoji.html --png render.png --max-tofu 40   # exit 1 above 40 tofu cells
font-tofu-check ... --json                                                    # the same numbers as json
```

`tests/fonts/font-check.sh` runs the whole thing (headless `--render`, then this tool) and is the way to use it.

## How a cell is judged

- The cell map is the page itself: `<section data-block>` starts a block, every `data-cp` tag is a cell, and the geometry
  (cell edge, cells per row, heading height, header height, inset) is on `<body>`. Fixed geometry was chosen over a
  console sidecar because the page is static with fixed-size cells, so a position is arithmetic, and a sidecar would need
  JavaScript to run in the engine being measured.
- The glyph area is the cell minus `data-inset` px on every side, as raw RGBA.
- BLANK: the glyph area is one flat colour. TOFU: it is byte-identical (tolerance 0) to the glyph area of a reference cell
  (U+FFFF or U+10FFFF; U+E0FFF was tried first and is a default-ignorable code point, which engines draw as nothing, which no font assigns). Anything else is a glyph.
- Control blocks (Latin, Cyrillic, Greek, Arabic, Devanagari) are listed but not counted in the total that `--max-tofu`
  judges; they show the detector works. A blank reference cell makes the counts untrustworthy: a warning is printed and
  `--max-tofu` exits 2.

Limits: a script whose fallback face draws a different notdef box than the generic fallback is not caught (the same
limit `tests/fonts/notdef-count.py` documents); a ZWJ, flag or skin-tone sequence that falls apart into several glyphs is
a glyph here, not tofu. Read the PNG for those.

Exit codes: 0 table printed; 1 `--max-tofu` exceeded; 2 an error (arguments, files, a render of the wrong size).

Dependencies: `image` (png feature only) and `serde_json`, both already locked in `tools/wpt-reftest/Cargo.lock`; this
tool's `Cargo.lock` is that file with the root package replaced.

Build: `cargo build --release --locked --manifest-path tools/font-tofu-check/Cargo.toml`;
tests: `cargo test --locked --manifest-path tools/font-tofu-check/Cargo.toml`.
