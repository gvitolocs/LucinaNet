# wpt-reftest — Web Platform Tests against the real Rust renderer

Runs the vendored WPT corpus (CSS, SVG, fonts, masking) against a built `lucinanet-servo` by driving
`--render` over a local `wpt serve`. Scores **reftests** with WPT's fuzzy pixel rule, **testharness**
tests with the proven `add_completion_callback` console hook, and **crashtests** by survival.

Its own Cargo workspace, like `parity/` and `tools/sitecheck/`. It links no engine crate.

## Why not `mach test-wpt`

`mach test-wpt --bin <lucinanet-servo>` cannot work. `python/wpt/run.py:99-100` sets
`binary = webdriver_binary`, and wptrunner's `servo` product
(`tests/wpt/tests/tools/wptrunner/wptrunner/browsers/servo.py:130`) launches it with
`--webdriver=PORT --hard-fail ...`. This binary has no WebDriver server:
`crates/lucinanet-servo/src/main.rs:20-60` and `crates/lucinanet-ui/src/lib.rs:278-303` only dispatch
`--render` or open the GUI, and no `servodriver` binary exists.

So this takes the path `tests/media/wpt.sh` already proved — a localhost `wpt serve`, one
`lucinanet-servo --render` per test — and adds the part the media script does not have: reftests.
`tests/media/wpt.sh` is **testharness-only** and would score a reftest 0/0. It is left untouched
because it is the media baseline.

## Exact invocations

From the root of a separate working copy of the repository (`$WORK_DIR/LucinaNet/health-suite`, `WORK_DIR` a scratch
directory). The wrapper builds the tool if needed, writes the server config, starts `wpt serve`, runs the
selection, and kills the server.

```sh
# CSS masking (502 reftest variants + 64 testharness + 5 crashtest)
systemd-run --user --scope -p CPUQuota=200% \
  tests/wpt/run-reftests.sh --include css/css-masking/

# CSS fonts (383 reftest + 173 testharness + 10 crashtest)
systemd-run --user --scope -p CPUQuota=200% \
  tests/wpt/run-reftests.sh --include css/css-fonts/

# SVG (607 reftest + 785 testharness + 53 crashtest, after the default .https. exclusions)
systemd-run --user --scope -p CPUQuota=200% \
  tests/wpt/run-reftests.sh --include svg/

# All three in one run, sequential
systemd-run --user --scope -p CPUQuota=200% \
  tests/wpt/run-reftests.sh --include css/css-masking/ --include css/css-fonts/ --include svg/

# Reftests only, or testharness only
tests/wpt/run-reftests.sh --include css/css-masking/ --types reftest
tests/wpt/run-reftests.sh --include css/css-fonts/ --types testharness

# Smoke: 8 tests, keep every PNG
OUT=$WORK_DIR/lucinanet/health/wpt-smoke \
  tests/wpt/run-reftests.sh --include css/css-masking/ --limit 8 --keep-all

# Preview the selection without rendering anything
tests/wpt/run-reftests.sh --include svg/ --types reftest --list
```

There is **no top-level `fonts/` suite** in this corpus — the font tests live in `css/css-fonts/`.
Asking for `--include fonts/` selects zero tests and exits 2.

### Wrapper environment

| var | default | meaning |
| --- | --- | --- |
| `BIN` | `target/probe/lucinanet-servo` | the renderer to test |
| `SERVO` | `third_party/servo` | a Servo checkout holding `tests/wpt/` (the corpus is not vendored here) |
| `MANIFEST` | `$SERVO/tests/wpt/meta/MANIFEST.json` | the v9 manifest |
| `OUT` | `$WORK_DIR/lucinanet/health/wpt-reftest` | results, PNGs, logs, config |
| `PORT` | `8123` | localhost http port; an already-running server there is reused |
| `CARGO_TARGET_DIR` | `$WORK_DIR/lucinanet/health/target-wpt` | scratch build dir, never the worktree |
| `CARGO_BUILD_JOBS` | `3` | capped; raise it only on a machine that can take it |
| `FONT_DIRS` | `$WORK_DIR/lucinanet/fonts` | `LUCINANET_FONT_DIRS` |
| `MAX_C` | `90` | Tctl pause threshold; `0` disables the gate |
| `BUILD` | `0` | `1` forces a rebuild |
| `TOOL` | `$CARGO_TARGET_DIR/release/wpt-reftest` | use a prebuilt scorer |

### Tool flags

`--include PREFIX` (repeatable), `--exclude SUBSTR` (repeatable), `--types reftest,testharness,crashtest`,
`--limit N`, `--jobs N` (default **1**, max 8), `--viewport WxH` (default **800x600**, WPT's reftest
window), `--hold SECS` (default 2, reftest/crashtest), `--harness-hold SECS` (default 10),
`--timeout SECS` (default 60), `--font-dirs DIR`, `--report-js FILE`, `--max-c N`, `--keep-all`,
`--no-default-excludes`, `--list`. `--help` prints the same list.

`--include` is a **URL prefix from the WPT root**, so `--include css/css-masking/` works and
`--include clip-absolute-positioned-001.html` matches nothing. A prefix that matches zero tests prints
a warning; if *nothing at all* is selected the run exits 2.

The tool can also be run directly against an already-running server:

```sh
$WORK_DIR/lucinanet/health/target-wpt/release/wpt-reftest \
  --manifest "$SERVO/tests/wpt/meta/MANIFEST.json" \
  --bin target/probe/lucinanet-servo \
  --out $WORK_DIR/lucinanet/health/wpt-reftest \
  --base-url http://localhost:8123 --include css/css-masking/ --limit 8
```

Each render is:

```sh
LUCINANET_RASTER=rust LUCINANET_CONSOLE=1 LUCINANET_PROFILE_DIR=none \
LUCINANET_FONT_DIRS=$WORK_DIR/lucinanet/fonts \
LUCINANET_RENDER_HOLD=2 LUCINANET_USER_JS=<out>/report.js \
  timeout -k 5 60 "$BIN" --render "http://localhost:8123/$t" "$out/shots/$t.png" 800x600
```

`LUCINANET_USER_JS` is set for testharness tests only. A hold of 1–2 s is enough for static CSS
reftests; the media script's 15 s is far too slow for a 25k-test corpus.

## Outputs

Under `$OUT`:

- `results.jsonl` — one object per test, flushed as it finishes: `path`, `type`, `dir`, `status`
  (`PASS`/`FAIL`/`ERROR`/`TIMEOUT`), `ms`, `render_exit`, and for reftests a `relations` array carrying
  `ref`, `op`, `fuzzy: [maxDifference, maxPixels]` and
  `pixels: {differing, differing_over_max_difference, max_difference, total, width, height, size_mismatch}`.
  Testharness tests carry `subtests`, `harness`, `subtest_failures`.
- `summary.json` — counts and pass rate overall, `by_type`, and `by_dir` (the matched `--include`
  prefix, else the top-level suite), plus `subtests`, `selected`, `run`, `vacuous`, `exit_code`.
- `shots/<path>.png`, `logs/<path>.log` — **kept for failures**, deleted for passes unless `--keep-all`.
- `refs/<path>.png` — the reference cache, shared across tests; pruned to the references a failure
  points at unless `--keep-all`.
- `report.js`, `config.json`, `serve.log`.

Stdout is one line per test with the actual pixel counts, e.g.

```
PASS    reftest     css/css-masking/clip/clip-absolute-positioned-001.html  == px=0/480000 maxdiff=0 fuzzy=0/0  3120ms
FAIL    reftest     css/css-masking/clip/clip-filter-order.html  == px=1234/480000 maxdiff=255 fuzzy=0/0  3050ms
```

**Exit codes:** `0` all selected tests pass · `1` at least one FAIL/ERROR/TIMEOUT · `2` **zero tests
selected** (vacuity is an error in this project) · `3` bad arguments, missing manifest/binary, or the
server did not come up.

## Reports

`results.jsonl` is the input, so a report can be regenerated from any run directory without rendering
again. `report` is a subcommand, not a flag: everything else about the invocation is the runner's.

```sh
$TOOL report clusters $OUT --top 25 --md $OUT/clusters.md      # what is failing, grouped
$TOOL report compare  $OUT-before $OUT-after --md $OUT/delta.md # rust vs swgl, or two engine commits
```

`report clusters` prints totals by type with the pass rate, then the failing tests grouped into
clusters, biggest first. A cluster key is the test's directory plus its file stem with any trailing
`-NNN` / `_NNN` number and any `-ref` / `-notref` suffix removed, so
`clip-path-inset-001.html` and `clip-path-inset-002.html` are one cluster. Each cluster shows its
failing and total counts, a failure-kind breakdown, and up to 3 example paths. The kinds are
`timeout`, `error`, `size-mismatch`, `pixels-small` (a reftest FAIL under 1% differing pixels),
`pixels-large`, `subtests` and `crash`; exactly one applies per test, most-specific-first, so the
breakdown adds up to the failing count.

`report compare` compares **only the tests present in both** files: the overall pass counts and delta,
a table per `dir` (tests, pass before, pass after, fixed, regressed), every regressed path — the full
list, never truncated — and the top 10 clusters still failing after with their counts before and after.
"fixed" is FAIL/ERROR/TIMEOUT → PASS, "regressed" is PASS → not PASS.

`--md FILE` writes the same markdown to `FILE` as well as printing it. Both subcommands exit 0, or 3
with one line on stderr for a missing `results.jsonl` or a bad argument. A torn last line (an
interrupted run) is reported and ignored rather than fatal.

## Reftest scoring

References and fuzzy tolerances come from `MANIFEST.json` (v9), not from re-parsing the HTML's
`<link rel=match>` / `<meta name=fuzzy>` — the manifest is already parsed and is what wptrunner uses.
The trie is `items[type][suite][...segments...][file] = [hash, payload, payload, ...]`.

Two details that a naive reader gets wrong, both handled here:

- **A leaf can hold several payloads.** 42 reftest files and 2389 testharness files do: query-string
  variants, and the `.any.js` → `.any.html` / `.any.worker.html` expansion. Each payload is a separate
  runnable test, which is why `css/css-masking/` selects 502 reftests from 500 files.
- **`payload[0]` is a URL override, not a timeout.** `null` means "the file itself"; a string is the
  variant's own URL. `payload[1]` is the reference list for a reftest and a metadata object for
  testharness/crashtest; `payload[2]` carries `fuzzy`.

The fuzzy rule, as this project scores it:

> `maxDifference` is the largest per-channel difference allowed for **any** pixel; `maxPixels` is how
> many pixels may differ **at all**. A `==` relation holds iff
> `differing <= maxPixels && max_difference <= maxDifference`. A missing fuzzy entry is exact `(0, 0)`.

`!=` is the negation of `==`. **A reftest passes only if all of its relations hold** — every `==` must
match and every `!=` must mismatch. Different PNG dimensions are a mismatch outright (`==` fails, `!=`
passes). All four RGBA channels are compared, alpha included. `differing_over_max_difference` (pixels
beyond the per-channel ceiling) is reported alongside as a diagnostic, but the verdict uses the rule
above. A scoped fuzzy entry (`Some(ref_url)`) beats an unscoped one; among equals the most permissive
wins. None of the three target subsets has a scoped entry.

References are rendered once and cached, so 500 masking tests do not cost 1000 renders.

## Thermal discipline

The tool reads `/sys/class/hwmon/*/name` for `k10temp` and pauses 20 s at a time while
`temp1_input / 1000 >= --max-c` (default 90), before **every** render — the same gate as
`tests/media/wpt.sh`'s `cool()`. The wrapper runs the same check before starting the server.

`--jobs` defaults to **1**. Renders are heavy; raise it only on a machine that can take it. `--jobs > 2` prints a
warning. With `--jobs N`, reference renders are deduplicated through a condvar so parallel workers
never render the same reference twice.

On a host with no `k10temp` (or with `--max-c 0`) the gate is a no-op: **the caller must then wrap the
run in the heatpause guard (`js/tools/heatpause`)**, e.g. the `systemd-run --user --scope -p CPUQuota=200%` form
above plus a built `heatpause`.

## Limitations — read before scoring anything

- **Localhost only.** a machine with no `web-platform.test` host entries fails the cross-origin subtests, and the config serves http on one
  port with `check_subdomains: false` and no https/ws/h2 ports. **Cross-origin subtests will fail here
  and must not be counted as engine bugs.** `.https.`, `.h2.`, `.http2.` and `.webtransport.` variants
  are excluded by default for that reason (`--no-default-excludes` puts them back). That is the whole
  difference between SVG's 619 reftest files and the 607 selected. `.sub.` is *not* excluded —
  `wpt serve` does server-side substitution.
- **Masking is genuinely exercised.** LucinaNet enables `layout_css_masking_enabled` unconditionally at
  `crates/lucinanet-servo/src/engine.rs:232`, so the `--render` path does test masking. A `mach` run
  would need `--pref layout.css.masking.enabled=true` to reach the same code.
- **`:has()` is on; its invalidation is an open item.** `layout.css.has-selector.enabled` is on by default
  (`crates/lucinanet-servo/src/css_prefs.rs`), so the static part of `css/selectors-4` is scored for real — the engine
  parses and matches the selector, and pages that were dropping the rule now draw it. The **dynamic** tests are not a
  fair read of the engine: a DOM mutation that changes whether an element which *already has a style* matches does not
  recompute that element, so a panel stays revealed after the `.b` that justified it is removed, and stays hidden
  though the element now matches. A freshly inserted element styles itself correctly in the same frame, which is what
  pins the gap on invalidation rather than on parsing or matching. At this stylo pin
  `invalidation/element/relative_selector.rs` is dead code, un-called from `style/data.rs`, so the fix is engine-side.
  Score a mutating reftest as a known gap, not as a new engine bug.
  `LUCINANET_CSS_PREFS=-layout.css.has-selector.enabled` restores the previous behaviour for one run.
- **Viewport.** The default is 800x600, WPT's reftest window (wptrunner's own `reftest.js` opens
  `width=800,height=600`). Test and reference are always rendered at the same size, so the comparison
  is self-consistent either way, but `vw`/`vh` and media-query tests are viewport-sensitive. Use
  `--viewport 1920x1000` to match the media script's form.
- **Crashtests are weakly scored.** `--render` hard-exits 0 via `libc::_exit(0)`, so "exit 0 and no
  signal" is the pass condition. A panic caught in a worker thread does not fail the test; it is
  recorded as `panic_in_log` and counted in `crashtests_with_a_panic_in_the_log`. A hang is a TIMEOUT.
- **Testharness with zero subtests.** WPT scores a file whose harness reports OK with no subtests as a
  pass. That is honoured here but flagged as `no_subtests` and counted in
  `testharness_ok_with_no_subtests`, so a `report.js` that never attached cannot hide behind it.
- **No expected-result metadata.** `tests/wpt/meta/**/*.ini` (expected FAIL/CRASH/timeout overrides) is
  not read. These are raw engine results, not a wptrunner-style "unexpected results only" report.
- **Animations and async reftests.** A 2 s hold is right for static CSS. Reftests that animate or wait
  on fonts/images may need a longer `--hold`; they are not specially detected.

## Validation

`cargo test --release --offline` runs 15 unit tests: 7 over the comparison itself, on synthetic PNGs —
identical images pass exactly; one pixel off by one fails exact and passes `fuzzy(1,1)`; a single pixel
at difference 70 fails a `0-69 / 0-360` ceiling even though only one pixel differs; 361 differing
pixels fail a 360-pixel budget even when every one is inside the per-channel ceiling; alpha counts as
a channel; a size mismatch is a mismatch; and `[[0,69],[0,360]]` parses to `(69, 360)` — and 8 over the
report (`report.rs`), on in-memory JSONL: cluster keys, the failure-kind rule, cluster grouping,
ordering and `--top`, the compare delta with one-sided tests ignored, the completeness of the regressed
list, and the exit codes.

```sh
cd tools/wpt-reftest
CARGO_TARGET_DIR=$WORK_DIR/lucinanet/health/target-wpt CARGO_BUILD_JOBS=3 \
  cargo test --release --offline
```