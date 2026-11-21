# sitejs

Saves the JavaScript that real pages load, for engine tests outside a browser (written for the Perry vs
Brimstone test, `docs/research/perry-vs-brimstone.md`). Standalone crate, not a member of the LucinaNet workspace.

```
cd tools/sitejs && cargo run --release -- <out_dir>
```

Fetches example.com, a Wikipedia article (plus two ResourceLoader bundles), Hacker News, github.com/servo/servo and
discord.com/login with a desktop Chrome User-Agent. Per page it writes `page.html`, every external, inline and preloaded
script (`ext-NN-*.js`, `inline-NN.js`, `pre-NN-*.js`), and `manifest.json` (url, kind, is_module, bytes, sha256, HTTP status).
Preload and external entries can be the same file; dedupe by `sha256`. Files are renamed, so ES modules' relative imports
between captured chunks don't resolve.

The 2026-11-04 capture is on the build host in `<work-dir>/lucinanet/site-scripts-2026-11-04/`.
