# Parity: LucinaNet's engines against headless Chromium

Chromium is a test reference on the build host only: never on Juno, never linked. Build and run:

```sh
cd parity && cargo run --release -- --engine /path/to/lucinanet-blitz --name blitz --out ../evidence/parity/blitz sites.txt
```

For every site it loads Chromium and the engine at 1920x1000, scale 1, and writes `scores.tsv`, each site's PNGs, and
`todo.md`, the failures worst first. A score is out of 100:
- `pixels`: 8x8 cells of the viewport, by mean luminance;
- `text`: the words of Chromium's `innerText` found in the engine's, as a multiset;
- `boxes`: over elements on Chromium's first two screens, with the same path: 1 if within 8 px on every edge, else the overlap (IoU);
- `dom`: the element count, as the smaller over the larger (Blitz runs no scripts yet, so pages built by JS score low);
- `score`: their mean. A load that fails or crashes scores 0.

`--render` runs headless in an engine, at scale 1, with the same fonts Juno has if `LUCINANET_FONT_DIRS` says so. Chromium uses
the build host's fonts: expect a few points of pixel difference from that alone.

The owner's Secretary web app needs a login; add it with a cookie once there is a test account.
