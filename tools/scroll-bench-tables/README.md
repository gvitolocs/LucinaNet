# scroll-bench-tables — the findings tables, from the runs that made them

Turns scroll-bench run directories and the matrix job logs they went through into the six findings
tables of the scroll-bench analysis. It is the Rust port of `scroll-bench-analyse.py`, which the
project's rule that new tools are Rust retires; the tables print the same numbers, in the same
order, under the same headings, byte for byte.

Its own Cargo workspace, like `tools/wpt-reftest` and `tools/sitecheck`. It links no engine crate,
reads files, and needs nothing but the standard library and serde_json.

## Usage

```sh
scroll-bench-tables [--base DIR] [INPUT...]
scroll-bench-tables --help
```

An argument ending in `.log` is a job log; any other is a directory of runs. A relative argument is
read under `--base`, which stands in for the scratch path the Python had written into it - so the
directories and the logs can be named as the matrix scripts named them.

```sh
runs=$WORK_DIR/scroll-bench        # where the run directories and the matrix job logs are kept
cargo run --release --manifest-path tools/scroll-bench-tables/Cargo.toml -- \
  --base "$runs" m1 m2 m3 m4 \
  "$runs/logs/run-matrix.log" "$runs/logs/run-matrix2.log" "$runs/logs/run-matrix3.log" "$runs/logs/run-matrix4.log"
```

The tables go to stdout. The one thing on stderr is a note about runs with no load line, and it is
absent when every run matched - which is the case for the 72 runs of the three matrices.

## Where each number comes from

A run's json holds its frame records and nothing else; a job log holds the machine load and wall time
around each run and no frame values. Neither knows the other's name: m1's run directories were renamed
to carry the rasterizer and the pace *after* the runs, and the logs' three header formats were edited
between the matrices. The two meet on the clock - a run's json is written when the run ends, so the run
belongs to the log block whose `before:` to `after:` window holds that timestamp, and of several such
blocks to the one that ended first. Where the block names a configuration and the directory names
another, neither is believed and the row loses its load.

`m1` is the pre-rewrite driver, whose runs returned 0 to 28 frames where the same cell returns 120
after it. Sections 1, 2 and 4 keep every run, because what those report is what each run contains;
sections 3, 5 and 6 pool only the runs of the rewritten driver that returned twenty frames or more.
That filter is the analysis' own, and it is why a re-run of this tool over fewer directories can print
a section that is short rather than empty.

## What differs from the Python, and why it cannot show in a byte

- A run directory whose json cannot be read, or that lacks `meta.device_px` or
  `summary.window_device_px`, is an error naming the file. The Python raised a traceback at the same
  point; a table with `inf` in an area column would be worse than no table.
- A missing job log or a missing run directory is an error too, where the Python globbed to nothing and
  printed `no json under [...]`.
- A directory that holds its own `json/` is read as one run, named by the json file. This is the shape
  the Mac output has, and the Python could only read matrices.
- The formatting is Rust's `"{:.N}"`, which was checked against CPython's `"%.*f"` on 53,087 doubles -
  every float in the 72 runs' frame records, the midpoints of consecutive pairs, the exact ties at each
  printed digit, and random bit patterns - and agrees on all of them. `statistics.mean` is the one place
  a naive port goes wrong: it sums exactly and divides once, which differs from a float loop by an ulp on
  over half the lists of tile areas this tool reads. `src/stats.rs` does the exact thing.

## Tests

```sh
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target-tools} cargo test --offline
```

`tests/fixture.rs` runs the binary over `tests/fixture/` - ten runs of the three matrices, a job log
excerpt whose clocks sit at a fixed epoch, and the expected tables the retired Python printed - and
compares stdout byte for byte. git carries no mtimes, so the test touches each run's json to the second
`mtimes.txt` names before it runs; without that the clock matching would be an accident of the checkout.

## Re-generating the expected output

Only worth doing if the fixture changes. The Python is not in this tree: it sits with the run
directories, at `$runs/py/scroll-bench-analyse.py`, and is unchanged - fix the invocation,
never the analysis. Its `BASE` is hard-coded, so name the directories absolutely, and it reads the
mtimes a copy does not carry, so set them first - the same seconds `mtimes.txt` names, which is what
`tests/fixture.rs` does too.

```sh
tmproot=${TMPDIR:?set TMPDIR to a scratch directory (not /tmp)}
fx=$tmproot/fixture-copy && rm -rf $fx && cp -r tools/scroll-bench-tables/tests/fixture $fx && cd $fx
python3 - "$fx" <<'PY'
import os, sys, glob
base = sys.argv[1]
for line in open(os.path.join(base, "mtimes.txt")):
    line = line.split("#")[0].strip()
    if not line:
        continue
    run, secs = line.split("\t")[:2]
    json = glob.glob(os.path.join(base, run, "json", "*.json"))[0]
    os.utime(json, (int(secs), int(secs)))
PY
python3 $WORK_DIR/lucinanet/scroll-bench/py/scroll-bench-analyse.py $fx/m1 $fx/m3 $fx/m4 $fx/matrix.log \
  > analyse.expected.txt
```
