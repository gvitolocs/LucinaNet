# lucinanet-fonts-rs

Pure-Rust font code for LucinaNet, a browser built on a Servo fork with its own JavaScript engine, all in Rust.
These three crates replace the C and C++ font libraries that Servo links.

| Crate | What it is | Upstream it follows |
| --- | --- | --- |
| `lucina-fontsan` | Font sanitiser: a pure-Rust replacement for OTS / fontsan. Validates and rewrites web fonts (TrueType, OpenType, WOFF, WOFF2, TTC) before they reach the text stack. | OTS (OpenType Sanitiser), fontsan |
| `lucina-ftcompat` | FreeType 2.13.2 size, metric and embolden arithmetic in Rust: fixed-point multiply and divide, ppem and strike selection, face metrics, bitmap presets, synthetic bold. | FreeType 2.13.2 |
| `lucina-fontface` | Font-face outlines and metrics for LucinaNet, on top of `skrifa` and `read-fonts`. | skrifa / read-fonts |

The original is the source of truth for behaviour: the code copies what OTS and FreeType do, not how they are laid out,
and the tests compare against their output where fonts are available.

## Build and test

```
cargo build --workspace
cargo test --workspace
```

Tests that read real fonts need a directory of sample fonts and skip themselves without it:

```
LUCINANET_FONT_SAMPLES=/path/to/fonts cargo test --workspace
```

CI also runs `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings` and
`cargo deny check licenses` (`.github/workflows/rust-ci.yml`).

## Proofs

The arithmetic in `lucina-ftcompat` and the bounds rules in `lucina-fontsan` are proved with
[Verus](https://github.com/verus-lang/verus) on the shipped source files, not on a model.
The harnesses are in `verification/`. They are not part of the cargo workspace.

```
bash verification/verify.sh     # proofs; needs verus on PATH (or $VERUS)
bash verification/mutants.sh    # plants bugs in copies of the sources; Verus must refuse every one
```

`tools/install-verus.sh <dir>` installs the Verus release pinned in `tools/verus.lock`, checking its hashes.
`.github/workflows/verus.yml` runs both scripts.

## Licence

MPL-2.0. `lucina-ftcompat` is also under the FreeType Licence (FTL) and `lucina-fontsan` also under BSD-3-Clause;
see `NOTICE` and `LICENSES/`.
