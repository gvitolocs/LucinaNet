# LucinaNet site log

Source 3 of `docs/missing.md`: what real sites find missing when they run, as opposed to what test262
says (source 1) and the engine's own audit (source 2). It is a pure post-processor — it reads log files
and writes tables, and needs neither the engine nor mozjs nor the network.

## Producing the logs

The browser writes the console messages of a page to its stderr with `LUCINANET_CONSOLE=1`; the glue
writes the `LUCINANET_JS_MISSING_LOG` file described below when that variable names one. One render per
process, one site at a time (the pages are heavy and the runs share a machine):

```sh
# Set WORK_DIR to a temporary working directory; the run logs go under it.
mkdir -p "$WORK_DIR"/sitelog/runs/$(date -u +%F)/sites
while read -r name url; do
  LUCINANET_TIMING=1 LUCINANET_CONSOLE=1 timeout 120 \
    /path/to/lucinanet-servo --render "$url" sites/$name.png 1280x720 2> sites/$name.err
done < docs/missing/sites-script-heavy.txt
```

The site list is `docs/missing/sites-script-heavy.txt`, one `name url` pair per line; `name` is the file
stem, so it is also the site name in the tables.

## Aggregating

```sh
cargo run --locked --offline --manifest-path tools/lucinanet-sitelog/Cargo.toml -- \
  --logs "$WORK_DIR"/sitelog/runs/2026-11-13/sites --out-dir docs/missing \
  --build "lucinanet-servo of 2026-11-13 12:52 UTC, pin 1f065a160e"
```

`--logs` takes any number of directories and files; a directory contributes the files whose extension is
`err`, `log`, `missing`, `mlog`, `stderr` or `txt`, so screenshots and stdout in a run directory are
skipped, not fatal. `mlog` is the stem the 30-site run writes its `LUCINANET_JS_MISSING_LOG` to; the site
name is the file stem, so `youtube.err` and `youtube.mlog` are still one site. `--build` is copied verbatim
into both outputs: it is where the run says which binary made these
logs, because a table of stale lines is only honest with its date. The files are `docs/missing/missing-
runtime.tsv` and `docs/missing/missing-runtime.md`. Exit codes: 0 written, 2 bad arguments or no log
found.

## Input lines

The site name is the file stem, and the two channels of one site (`.err` and `.missing`) are one site.
Every other line is ignored — phases, timings and network notes are not missing features.

| line | becomes |
|---|---|
| `<unix_ms>\tmissing\t<name>` | `missing`, `<name> is not implemented` |
| `<unix_ms>\terror\t<Type>: <message>\t<file>:<line>` | `error` with that location |
| `console Error: Error at <url>:<line>:<col> <Type>: <message>` | `error` with location `<url>:<line>` |
| `console Error: Unhandled promise rejection: <Type>: <message>` | `error` |
| `console Error: <text>`, `console Warn: <text>` | `error`, `warn` |
| `lucinanet-js: <text>` | `js` |
| `<name> is not implemented`, with or without the `mozjs_sys (Rust): ` prefix | `missing` |

The `missing` lines come from `mozjs-sys`'s `missing()`, which prints that sentence and aborts; the
`error` lines from the one path on which the glue hands an uncaught script error to the embedder. Both
are appended live, before the abort, and a file that cannot be opened is not an error: a diagnostic
channel must not break the run. Error lines are capped at 500 per process, missing lines are not capped
because there are few and each ends the process.

The wording of a stub is identical in the log file and in the abort line, so a stub seen through both
channels is one row.

## The tables

`missing-runtime.tsv` has one row per normalised message:

```
kind	normalised message	sites	total hits	example site	example location
```

Rows are ranked by sites, then by hits, then by kind and text, so the table reads "what hurts most"
first. The example is the first hit that names a location, which is the one worth opening.

`missing-runtime.md` is the short read: totals, the 20 rows that span the most sites, and then only the
rows that name a missing built-in or global — a message containing ` is not defined`, ` is not a
constructor`, ` is not a function`, ` is not implemented` or ` not supported in this build` — each marked
with whose job it is.

**engine or web-engine.** A name ECMA-262 or ECMA-402 defines (`Intl`, `Temporal`, `Array`,
`NumberFormat`) is `engine`'s to provide; a name the Web platform defines (`document`, `fetch`,
`IntersectionObserver`) is `web-engine`'s. The lists are in `src/names.rs` and are curated, not derived
from a spec database. A name on neither list follows the channel it arrived on: a `not implemented` line
is something the glue itself declares missing, so `engine`; anything a page reached for and did not find
is nearly always a Web global, so `web-engine`.

**Normalisation.** Two hits of one cause become one row, so what varies between them is replaced:
whitespace collapses, a URL is `U`, a build hash (twelve or more hex digits with a digit in it, on its
own) is `H`, a number on its own is `N`, text quoted with backticks, `"` or `«»` is `X`. A digit inside a
name is left alone, or every `Int8Array` would be an `IntNArray`, and a quoted run that reads as a
JavaScript name keeps its text without its quotes, because saying *which* thing is missing is the point
of the table: `` `Intl` `` survives as `Intl`.

The markdown elides a long example location to its last characters — the file and line are at the end of
a bundle URL — while the TSV keeps it whole.

## Determinism and tests

Files are read in path order and rows ranked by count then text, so the same input produces the same
bytes; `cargo test` asserts that, between two runs as well as within one. The tests are

```sh
cargo test --locked --offline --manifest-path tools/lucinanet-sitelog/Cargo.toml
```

`tests/fixtures/sites/` holds four real stderr logs, copied whole from `<work-dir>/lucinanet/example-site-run/
sites` (2026-11-12), and `probe.missing`, a synthetic `LUCINANET_JS_MISSING_LOG` file: no shipped binary
has written one, because the glue that does it is on this branch. The fixture expectations are the point —
they say what the aggregator owes the reader, not what the sites happened to print that day.

What this tool does not establish: that a site's error is the engine's fault rather than the page's, that
a site would work with the feature, or that the set of ten sites is representative. It counts what the
logs say and nothing more.
