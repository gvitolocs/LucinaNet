# Sitecheck Tool

## Overview

The `sitecheck` tool is a Rust application designed to parse the stderr logs of `lucinanet --render` runs and generate a per-site pass/fail table. It is a standalone tool that does not require Servo and builds quickly.

## Usage

```sh
sitecheck [--json] [--scroll-ms N] [--load-ms N] <log>...
sitecheck diff <a.png> <b.png> [--threshold N] [--json]
```

### Options

- `--json`: Output the results in JSON format instead of a Markdown table.
- `--scroll-ms N`: Set the maximum allowed scroll time in milliseconds (default: 50).
- `--load-ms N`: Set the maximum allowed load time in milliseconds (default: 15000).

### Input

Each `<log>` is a file containing the stderr output of:

```sh
LUCINANET_TIMING=1 LUCINANET_CONSOLE=1 LUCINANET_SCROLL_TEST=6 lucinanet --render <url> out.png 1280x720
```

### Output

- **Markdown Table**: A table with columns for site, loaded status, load time, page load time, scroll median time, scroll worst time, console errors, and verdict.
- **JSON**: One JSON object per log with fields for site, loaded status, load time, page load time, scroll steps, console errors, crashed status, and crash reason.

## Example

```sh
sitecheck --json --scroll-ms 100 --load-ms 20000 log1.log log2.log
```

## Screenshot diff

```sh
sitecheck diff <a.png> <b.png> [--threshold N] [--json]
```

`sitecheck diff` compares a `lucinanet --render` screenshot with a reference
image, pixel by pixel:

- Both PNGs are decoded to RGBA8 and compared over the area they share,
  anchored at the top left, so a page LucinaNet rendered shorter than the
  reference still lines up. The two sizes are printed, plus `size differs`
  when they are not the same.
- A pixel counts as different when one of its four channels moves by more than
  `--threshold` (default 24).
- The output is the percentage of differing pixels and the same percentage for
  each cell of a 4x4 grid over the overlap, which shows *where* a page is wrong.

```console
$ sitecheck diff /tmp/sitesuite-try/hn.lucinanet.png /tmp/sitesuite-try/hn.firefox.png
a: /tmp/sitesuite-try/hn.lucinanet.png 1280x800
b: /tmp/sitesuite-try/hn.firefox.png 1280x800
overlap: 1280x800  threshold: 24 per channel
diff: 10.85% (111082/1024000 pixels)
grid 4x4, % differing pixels per cell:
      19.06   20.77    2.24    0.80
      25.49   22.78    0.44    0.00
      23.20   15.67    0.00    0.00
      25.10   17.42    0.61    0.00
```

Exit status: always 0 - a differing pair is a result, not a failure - except 3
when a file cannot be read or decoded, and 2 on a usage error. `--json` prints
one object per pair with the same numbers (`diff_percent`, `grid`,
`size_differs`, `overlap`, ...).

## Site suite

`suite.sh` renders every site in `sites.txt` twice, once with the LucinaNet
binary and once with headless Firefox as the reference, then writes both the
log table and the pixel diff to `report.md`:

```sh
bash suite.sh                                     # defaults
bash suite.sh --only example,hn --out /tmp/try    # two sites into a scratch dir
bash suite.sh --size 1280x720 --bin ../path/to/lucinanet
```

| Option | Default | Meaning |
|---|---|---|
| `--bin PATH` | `$WORK_DIR/lucinanet/health/bin/lucinanet-servo-after2` | LucinaNet binary to render with |
| `--out DIR` | `$WORK_DIR/lucinanet/health/sitesuite` | where images, logs and `report.md` go |
| `--only name,name` | every line of `sites.txt` | a subset, matched against the `name` column |
| `--size WxH` | `1280x800` | viewport for both engines |

Environment: `SITECHECK` picks the `sitecheck` binary (without it the script
builds and runs `cargo run --release`), `FIREFOX` the reference browser command,
`SITES` the site list, `THRESHOLD` the per-channel diff threshold, `TIMEOUT` the
seconds allowed for one render (default 150).

Per site, in the order the list is written, and always one site at a time -
both engines are heavy and running them side by side would distort the timings
in the logs:

```sh
printf '# url URL\n' > OUT/name.lucinanet.err   # the header the table shows the site by
LUCINANET_TIMING=1 LUCINANET_CONSOLE=1 LUCINANET_SCROLL_TEST=6 LUCINANET_PROFILE_DIR=none \
  timeout -s KILL 150 "$BIN" --render URL OUT/name.lucinanet.png WxH 2>> OUT/name.lucinanet.err
timeout 150 firefox --headless --no-remote --screenshot OUT/name.firefox.png \
  --window-size=W,H -profile TMP URL
```

Each Firefox run gets a fresh temporary profile, so no cache or restored
session changes the reference picture. Then `sitecheck` parses the render logs
and `sitecheck diff` compares each pair.

`sites.txt` holds one `name url` pair per line, `#` starts a comment, and `name`
is the file stem used for every output of that site.

`report.md` holds the Markdown table (over every `.err` log in `OUT`, including
ones from earlier runs), then one block per site with its diff percentage and
4x4 grid, worst first. A site that produced no image, or whose log does not
report a completed load, is recorded as `FAILED` with the exit codes, the diff
numbers if there were two images to compare, and the last log lines; it sorts
ahead of every measured site and the run carries on with the next one.

The suite renders with `LUCINANET_SCROLL_TEST=6`, so the table's scroll columns
carry real numbers; the render timeout defaults to 150 s to cover the scroll
steps.

Expect a non-zero diff on pages that change between the two loads (ad
slots, clocks, live feeds); the grid says whether the difference is spread over
the page or confined to one band.

## A/B of one environment switch

`ab.sh` runs the same binary with two settings of one environment variable -
the two "arms" - over the sites, so a switch is judged by its own effect and
not mixed up with a build difference:

```sh
bash ab.sh --bin ../path/to/lucinanet --env 'LUCINANET_TCP_NODELAY=0,LUCINANET_TCP_NODELAY=1'
bash ab.sh --bin ../path/to/lucinanet --env 'NAME=A,NAME=B' --only example,hn --rounds 2 --out /tmp/ab-try
```

| Option | Default | Meaning |
|---|---|---|
| `--bin PATH` | required | the one binary both arms run |
| `--env SPEC` | required | `NAME=A,NAME=B`: the two settings of that one variable |
| `--sites FILE` | `sites.txt` | `name url` lines, `#` comments |
| `--only LIST` | every line | a subset by name (the first seven lines are the usual seven sites) |
| `--rounds N` | `5` | passes over the whole list |
| `--out DIR` | `$WORK_DIR/lucinanet/health/ab` | `ab.md`, the per-run stderr, the profiles |
| `--size WxH` | `1280x800` | viewport |

Per site and arm, in every round, two measurements:

- **cold**: a new process and a fresh empty profile dir, the URL loaded once
  with `LUCINANET_TIMING=1 LUCINANET_RENDER_QUIET_MS=0` and a 90 s timeout; the
  number is that run's `render: complete true in N ms`.
- **same-process warm**: ONE new process whose `LUCINANET_LOADS` file holds a
  `timing` line and then the same URL twice, 150 s timeout; the number is
  `complete_ms` of the `timing step=1` line, the second navigation.

Warm is on purpose the second navigation *within one session* and not a second
process: the connection, the DNS entry and the caches the first load left are
still there, which is what a person hitting the back button or retyping an
address actually experiences, and it is how the Firefox comparison is measured
too (one session, navigate twice). A restart would time process start-up a
second time and throw that connection state away, so it would say nothing about
the switch while adding cold-start noise to both arms.

The arms alternate, and the arm that starts a round flips every round (A,B then
B,A), so a machine that speeds up or slows down during the run hits both arms
alike rather than whichever setting happened to be measured first. Renders never
run two at a time: they would take CPU from each other and distort the very
numbers being compared.

`OUT/ab.md` gets one table per measurement - per site the median and the min
and max of each arm in ms, `b - a` in ms and in percent, and a final row with
the median of the per-site deltas - then the settings, the binary path and its
mtime, the round count, `load1` at the start, and every run that failed or timed
out. A failed run is left out of its arm's median and listed there, never
dropped quietly; each run's stderr stays as
`OUT/<site>.<arm>.<round>.{cold,warm}.err`. The throwaway profiles under
`OUT/profiles` are removed as soon as each run ends. The exit code is 0 even
when sites failed, and 2 for a bad argument or a binary that is not executable.

These timings belong to the machine and the minute: a number means something
only against another `ab.sh` run on the same machine at a similar load, not
against another host or a busier hour.

## Tests

The tool includes tests to ensure it works correctly. Run the tests using:

```sh
cargo test
```

## Building

To build the tool, use:

```sh
cargo build --release
```

## Cross-Compilation

The tool can be cross-compiled for `aarch64-unknown-linux-musl` using:

```sh
cargo build --release --target aarch64-unknown-linux-musl
```
