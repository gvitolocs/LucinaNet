# Mac-build acceptance

`mac-build.sh <worktree> <outdir>` builds the pinned main and runs the acceptance steps, muted
(`LUCINANET_MEDIA_SINK=none`), profile `none` except one throwaway profile that is deleted at the end.
It runs on the build host (`/home/user/...` paths); a run takes about 15 minutes. `test-check-trace.sh` tests the trace
rule on synthetic logs in a few seconds and runs no engine.

## Steps

| Step | What it checks |
| --- | --- |
| build | `cargo build --profile probe -p lucinanet-servo --features desktop,media` succeeds |
| OOM pages | the four `tests/edge-wrap/oom-*.html` pages render (run early: fork-landing evidence; exit codes are printed) |
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
`media elem event=play ... potentially_playing=0 ... cmd=play` is followed by `player play on`. The project lead
accepted this (the checker is stricter than the contract). The script shows these as `STEP-FAILED` and does
not exempt them; `tools/media-trace-check` and its fixtures are not changed by this script.
The fixture `healthy-all.log` has only 17 lines, so it fails the 200-line rule by design;
`test-check-trace.sh` builds a 239-line healthy log from it for the passing case.
