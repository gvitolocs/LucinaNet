# vello-bench

vello_cpu 0.3.0 draws the scene of ../paint-bench.html (the same boxes, gradients, shadows and opacity group), times it, and diffs it against a PNG of that page from another engine. Used for the rasterizer choice in ../../CHOICE.md. One RenderContext is reused: each run is reset, draw, flush and render into one Pixmap.

## Command Line

`vello-bench [--threads N] [--runs R] [--out out.png] [--compare ref.png] [--diff diff.png]`

- `--threads N` (default 0): Number of threads to use for rendering.
- `--runs R` (default 20): Number of times to run the rendering process.
- `--out`: Path to save the rendered image.
- `--compare`: Path to the reference image for comparison.
- `--diff`: Path to save the difference image.

## Building and Running

1. Set a scratch directory for the build and the images, and point the target directory at it:

```sh
export WORK_DIR=$(mktemp -d)
export CARGO_TARGET_DIR=$WORK_DIR/target-bench
mkdir -p $WORK_DIR/m2
```

2. Build the project:

```sh
nice -n 15 cargo build --release -j 4
```

3. Run the benchmark:

```sh
$CARGO_TARGET_DIR/release/vello-bench --threads 0 --runs 5 --out $WORK_DIR/m2/vello-1t.png --compare $WORK_DIR/m2/swgl-3.png --diff $WORK_DIR/m2/diff-1t.png
$CARGO_TARGET_DIR/release/vello-bench --threads 8 --runs 5
```

## Scene Description

The scene is drawn on a 1920x1080 canvas and includes:

1. Background: A linear gradient from (0,0) to (0,1080).
2. Header: A linear gradient from (0,0) to (1920,0).
3. Blob: A blurred rounded rectangle.
4. Group: Two rectangles with opacity layer.
5. Cards: A grid of cards with shadows, borders, faces, dots, and bars.

For more details, refer to the source code.