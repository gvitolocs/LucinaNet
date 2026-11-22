# The headless site sweep

One page per engine process over a fixed list of 100 public pages, scored `loaded`, `broken` or `walled`, with a
diff against the previous build. It answers "which pages fail" without asking anyone.

| file | what |
| --- | --- |
| `sites.tsv` | the fixed list: `id, category, url, note`. Ids `S001`..`S100`. `prev=1` in the note marks the 25 pages of the 2026-11-13 sweep. Changing a row is a reviewed change. |
| `walls.tsv` | wall and error-page markers and the numeric limits, as data. It grows here, not in code. |
| `sweep.sh` | the runner. |
| `selftest.sh`, `fixtures/` | the shell self-test on three local `file:` pages. |
| `../../tools/sweep-score` | the scorer and diff tool (Rust, its own workspace). |

Categories (about ten each): search, social, video, news, shopping, reference, developer, finance,
gov-edu-health, travel-maps, vendor-docs, forums-blogs. Pages in CJK, Arabic, Cyrillic and Devanagari
(`script=` in the note) also exercise the font fallback.

## The rules (never broken)

Public URLs only. No login, no credentials, no cookies (`LUCINANET_PROFILE_DIR=none`). Muted
(`LUCINANET_MEDIA_SINK=none` for every page). No captcha solving and no bot-wall bypass of any kind: a walled page
is scored walled and never retried with tricks. At most 4 pages in parallel, never two pages of one host at once
and a 5 s gap between pages of one host. 120 s cap per page. Nothing personal is read. The full sweep is
a long job: run it when the machine is free.

How the mute is checked: each page's log must show `sound sink none` whenever media started (the runner sets
`RUST_LOG=error,lucinanet_media=info` so the line is printed); a `sound sink` line that is not `none` writes
`MUTE-VIOLATION` in the out dir and stops the sweep (exit 3). The summary line counts the pages that showed the
null sink. On macOS the runner also runs `tests/media/sink-check.sh` first.

## Run it

Run it from a checkout, on Linux:

```sh
# WORK_DIR: set it to a scratch directory for the results
cargo build --release --manifest-path tools/sweep-score/Cargo.toml
tests/sweep/sweep.sh <engine binary> $WORK_DIR/lucinanet/site-sweep/run-$(date -u +%Y%m%d) \
    --build-id <commit> --scorer tools/sweep-score/target/release/sweep-score
```

Selections: `--ids S001,S017`, `--category news`, `--prev-only`. After the run the runner scores, stores
`$WORK_DIR/lucinanet/sweep-results/<commit>-<sha256 prefix of the binary>-<utc>/results.tsv`, prints the diff against the
`latest` link and then moves `latest`. Exit 1 means a page went loaded -> broken (the regression gate).
What a person in the room notices: no sound, no window, no focus change, heavy network use to about 100 sites, one
to two cores for 10 to 40 minutes.

On the Mac, muted, with the explicit flag (without it the runner refuses; it also refuses if `sink-check.sh` fails):

```sh
SWEEP_ALLOW_MAC=1 tests/sweep/sweep.sh <engine binary> <out-dir> --prev-only --scorer <sweep-score>
```

Check the chain first: `tests/sweep/selftest.sh <engine binary> <sweep-score binary>` (three local pages, no network).

## Read `results.tsv`

`id verdict reason rc png_bytes content_ratio errlines panics wall_ms first_paint_ms category`

* `verdict`: `loaded`, `broken` or `walled`; `reason` is ONE line, the first decisive evidence.
* `content_ratio`: percent of pixels that differ from the frame's most frequent colour (under 0.5 is a blank frame).
* `errlines`: console errors and `ERROR` log records; a loaded page may have many (over 20 the reason lists the top 3).
* `panics`: lines with `panicked at` or starting `CRASHED:`, counted as they are written. A reason names a crash
  line's thread, its place and its kind (`thread=… location=… reason=…`) and never its message: the message stays
  in the page's `log`.
* `wall_ms`: the runner's wall clock for the page, 1 s resolution. `first_paint_ms`: document fetch to the first frame
  painted after its response headers; empty when the log has no such pair.

Rules, in order (the crate docs in `tools/sweep-score/src/lib.rs` are the source, unit-tested on synthetic input):
1. broken: timeout, `rc != 0`, a panic or crash line, the engine's error page, a missing or blank PNG.
2. walled: the document status is 403/429/503, or a marker of `walls.tsv` is in the visible text AND the visible
   text is small. A page is never walled by a guess.
3. loaded: everything else.

The HTTP status rule is DORMANT: the render log has no document status today. It reads `status=NNN` from the
proposed line `netphase <t> document <origin> status=<n> final=<origin>` (or from a line naming the page's URL), and
is unit-tested on synthetic logs. Until the net layer logs that line, walls are found by markers only.

## Diff

```sh
sweep-score diff <old results.tsv> <new results.tsv>      # or one file: against sweep-results/latest
```

Prints the pages that changed verdict (both reasons), pages that newly panic, pages whose `errlines` grew by more
than 50 percent or whose `png_bytes` changed by more than 30 percent, and the counts per category. Exit 1 when
any page went loaded -> broken, 0 otherwise, 2 on error.
