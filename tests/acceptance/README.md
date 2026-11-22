# Mac-build acceptance

`mac-build.sh <worktree> <outdir>` builds the pinned main and runs the acceptance steps, muted
(`LUCINANET_MEDIA_SINK=none`), profile `none` except one throwaway profile that is deleted at the end.
It runs on a Linux desktop; every path it writes comes from `WORK_DIR` or its arguments. A run takes about 15 minutes. `test-check-trace.sh` tests the trace
rule on synthetic logs in a few seconds and runs no engine.

## Steps

| Step | What it checks |
| --- | --- |
| build | `cargo build --profile probe -p lucinanet-servo --features desktop,media` succeeds |
| OOM pages | the `tests/edge-wrap/oom-*.html` pages render (run early; exit codes are printed) |
| local-rec{0,1}-{plain,pause,seek,init2,play2} | `tests/media/mse-two-buffers.sh` with the trace on; the script's `artifacts in <dir>` line is read and `check_trace` runs on each per-variant browser log `<dir>/out/<variant>.log` (not on the script's own short summary) |
| consent | YouTube consent answered through the redirect in a throwaway profile; the run stops unless `consent2: clicked` is in the log |
| yt-rec{0,1} | YouTube 120 s with recovery 0 and 1; `check_trace` on the whole log |
| media unit tests | only with `RUN_MEDIA_TESTS=1` (`cargo test -p lucinanet-media`); otherwise "media unit tests: not run (separate job)" |

## Failure rules (`check_trace` in `lib.sh`, required: element-contract-1 MEDIA-STR clock-contract)

A trace step prints `STEP-FAILED: <label>: <reason>` when any of these holds:

1. `tools/media-trace-check` exits non-zero (a violation);
2. the output contains `properties checked: (none)`;
3. the line count in `ok: no violation (N lines` is below 200 (`TRACE_MIN_LINES`);
4. a required property is missing from the `properties checked:` list;
5. the log file does not exist.

`not observed:` lines (for MEDIA-STR-RING and MEDIA-CUR the trace lacks their fields) are printed and do not fail a step.
At the end the script prints one line per step (`PASS` or `STEP-FAILED`), then `RESULT`, then `ACCEPT-DONE`
(only after the summary). The exit code is non-zero if any step failed.

## Precondition

Never delete a cargo target by file age; delete a whole profile (a half-pruned target links stale rlibs).

## Known issue: element-contract-1 on local variants

On current main the local variants report element-contract-1 violations: the element line
`media elem event=play ... potentially_playing=0 ... cmd=play` is followed by `player play on`. This is accepted
(the checker is stricter than the contract). The script shows these as `STEP-FAILED` and does
not exempt them; `tools/media-trace-check` and its fixtures are not changed by this script.
The fixture `healthy-all.log` has only 17 lines, so it fails the 200-line rule by design;
`test-check-trace.sh` builds a 239-line healthy log from it for the passing case.

## The harness build

`mac-build.sh` drives the browser through `--render` and the `LUCINANET_USER_JS` / `_PREFS` variables. Those doors exist only in a
build made with the cargo feature `harness` (`docs/harness-doors.md`); the shipped browser has none of them. So the script builds
`--features desktop,media,harness` and refuses a binary that is not a harness build (`require_harness_binary` in `lib.sh`: the
binary's version line carries the marker `LUCINANET-HARNESS-BUILD`), instead of measuring nothing.

One way to build it: `tools/build-harness.sh --run` (or `cargo build-harness`), which prints the exact command and checks the result:
`cargo build --offline --profile probe -p lucinanet-servo --features desktop,media,harness`. The binary is a measurement build; never
install, package or publish it.

Which scripts need it, and how each finds its binary (default location unless you pass one):

| Script | Needs | Binary |
|---|---|---|
| `tests/acceptance/mac-build.sh` | `--render`, USER_JS, PREFS, RENDER_HOLD | builds its own; checks the marker (shared `lib.sh`) |
| `tests/media/run.sh`, `scaled-planes.sh` | `--render`, RENDER_HOLD, USER_JS | `$1`, default `$WORK_DIR/lucinanet/target/probe/lucinanet-servo` (`WORK_DIR`: where you keep build trees) |
| `tests/media/mse-two-buffers.sh`, `sink-check.sh`, `wpt.sh` | `--render`, RENDER_HOLD, USER_JS | `$1` / `$LUCINANET_BIN` |
| `tests/media/laptop/*.sh` | `--render`, RENDER_HOLD | the binary on the laptop, passed in |
| `tests/profile/restart.sh` | `--render`, RENDER_HOLD, USER_JS | `$1`, default `/usr/bin/lucinanet` (a device binary: not a harness build, so pass one) |
| `tests/web/github-home.sh`, `frame-log.sh`, `scroll-busy.sh`, `csp-origin/run.sh` | `--render`, USER_JS, RENDER_HOLD | `$LUCINANET_BIN` / `$1` |
| `tests/css/at-rule-objects.sh` | `--render` | `$1` |
| `tests/wpt/run-reftests.sh`, `tools/wpt-reftest` | `--render`, RENDER_HOLD, USER_JS | `$BIN`, `--bin` |
| `tests/crash/run.sh`, `tools/sitecheck/ab.sh`, `suite.sh` | `--render`, LOADS | a binary named in the script |
| `scripts/scroll-bench.sh`, `scripts/glyph-check.sh`, `tests/fonts/font-check.sh`, `parity` | `--render` | argument / `$LUCINANET_BIN`; `parity` also runs the Blitz binary, built from `blitz/` with `--features harness` |

Only `mac-build.sh` shares `lib.sh` and so checks the marker today. The others could source `lib.sh`, or
copy the one-line `grep -aqF LUCINANET-HARNESS-BUILD "$bin"` check, so a normal binary stops them with a clear error.
