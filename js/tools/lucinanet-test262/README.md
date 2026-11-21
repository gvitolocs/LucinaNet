# LucinaNet test262

ECMAScript tests run through the same mozjs-compatible host API as the browser.
Adapted from this repository's `tools/t262` at commit 8913cc8; the implementation
and metadata rules are shared, with LucinaNet naming, path dependencies to the
Rust backend, failure exit codes, zero-selection rejection, and worker reaping.

```sh
cargo run --locked --offline --manifest-path tools/lucinanet-test262/Cargo.toml --bin lucinanet-test262 -- \
  --test262 /path/to/test262 --jobs 2 \
  --filter eval-code --filter built-ins/Function --filter built-ins/eval \
  --out target/lucinanet-test262-results.jsonl --summary target/lucinanet-test262-summary.json
```

The input snapshot used for the current audit is test262
`227d905513f790dec90858d04ddf8cf81326706f`. Filters are substrings, joined with OR.
Each strict/sloppy/raw/module variant is counted separately. Workers are restarted
on crashes/timeouts; these outcomes count as failures. `--with-intl` and
`--with-staging` explicitly include those directories. See `exec.rs` for host
hooks and module limitations; skipped tests never count as passes.
Zero selected variants, all-skipped runs, and report write failures are errors.
This tool does not establish DOM behavior, GC safety, or browser security parity.

Annex B is included. Unlike the internal Brimstone runner, this runner does not
load its blanket `annexB/*` exclusion. Negative tests must fail in the expected
compile (parse/early/resolution) or runtime phase with the expected exception type.
Module tests run: the harness is a script of the test's realm and the test file itself
is compiled as a module, linked and evaluated (its top-level await drained with the job
queue); `$262.agent` has no threading implementation, so this is not
a full test262 acceptance gate.

Runner self-check (separate synthetic fixtures, not upstream conformance):

```sh
cargo run --locked --offline --manifest-path tools/lucinanet-test262/Cargo.toml --bin lucinanet-test262 -- \
  --test262 tools/lucinanet-test262/fixtures --jobs 1
```

Expected: three passes and three intentional failures, exit 1. This verifies
negative compile/runtime handling, rejection of the wrong phase, missing negative
errors and duplicate async completion, and evalScript in a newly created realm.

## The known-failure baseline (the gate)

`baseline-failures.txt` lists the test variants that do not pass today (`path<TAB>variant`, path relative to the checkout). A run
with `--baseline baseline-failures.txt` fails only for a non-pass that is not in the file, after the runner has rerun it alone with
a one-minute timeout (timeouts under load are the only flakiness). The file only shrinks: when tests start passing, run once with
`--shrink-baseline` and commit the smaller file; a test that fails again then fails the gate. The corpus is tc39/test262 at the commit
in `test262.sha`. Locally: `scripts/check-tier test262`. Exit codes: 0 no new non-pass, 1 a new non-pass (or a lost result), 2 a bad
baseline or no tests selected.

Regenerating the baseline from zero (only when the corpus commit or the set of executed tests changes; a shrink is the normal
change): one full run, then the sorted `path<TAB>variant` of every fail, timeout and crash, paths relative to the checkout, under
the three header lines (the first two comment lines of the current file, and the engine/corpus/count line):

```sh
R=/path/to/test262   # the checkout at the commit in test262.sha
lucinanet-test262 --test262 $R --jobs 2 --timeout-ms 30000 --out t262.jsonl --summary summary.json
jq -r --arg r "$R/" 'select(.result=="fail" or .result=="timeout" or .result=="crash")
    | [(.path | ltrimstr($r)), .variant] | @tsv' t262.jsonl | LC_ALL=C sort -u > entries.txt
{ head -n 2 baseline-failures.txt
  echo "# Engine: <engine name and rev>, test262 $(cat test262.sha), $(wc -l < entries.txt) entries."
  cat entries.txt; } > baseline-failures.new && mv baseline-failures.new baseline-failures.txt
```

`import()` from a test file, and the static imports of a module test, go through a module load hook the
worker installs (`src/exec.rs`, `module_load_hook`): it resolves the specifier against the referrer module's
file (the test's directory for scripts), reads and compiles it as a module (cached per global under the
module's canonical path plus its requested type, so a cycle and a re-import see one record), loads its static
imports with `JS::LoadRequestedModules`, and finishes the `import()` promise; a missing file or a compile error
rejects it. A request with `type: "json"` is compiled by `JS::CompileJsonModule1` as a synthetic JSON module.
`import.meta.url` comes from a metadata hook that reads the same path back from the module's private.
The hook answers every request synchronously, and the glue links and evaluates whatever record it is handed
(`mozjs-sys/src/engine/module.rs`, `continue_dynamic_import`), so in a debug build of the engine `import()` of a
module whose own evaluation is still running trips the linker's assertion and aborts the worker; those tests are
reported as crashes, not as skips. A release build has the assertion compiled out and passes them.

## Inventory

`inventory` turns the JSONL of a finished run into a ranked list of what the language still
lacks. It is a pure post-processor: it reads the `--out` files and needs neither the engine nor
the corpus (the corpus root only shortens the paths in its output). The same code is built as a
second binary without the engine, so the inventory runs while the engine is broken:

```sh
cargo run --locked --offline --manifest-path tools/lucinanet-test262/Cargo.toml --bin lucinanet-inventory -- \
  --results results.jsonl [--intl-results intl402.jsonl] \
  --test262 /path/to/test262 --out-dir docs/missing
```

Equivalent as `lucinanet-test262 inventory ...`; both write byte-identical files, and several
runs merge into one inventory (the ranking and every count are computed over all of them).
It writes four files, all deterministic (ranked by non-passing variants, then by name), so
diffs between runs stay small:

| file | one row per |
|---|---|
| `missing-test262-features.tsv` | test262 `features:` value; a test with several features counts under each |
| `missing-test262-areas.tsv` | test262 directory area: the first two directories under `test/`, plus a third when `built-ins` or `intl402` names `Intl` or `Temporal` (`built-ins/Temporal/PlainDate`, `intl402/NumberFormat`); a test directly in a directory names that directory |
| `missing-test262-top-failures.tsv` | failing message text: the 200 most frequent, with count, the feature most often attached and one example variant |
| `missing-test262.md` | the human summary: totals, the 25 features with the most non-passes, every feature with no pass, every area below 90%, the unsupported list |

Columns of the two first tables: `total, pass, fail, skipped, pass_pct, status`. Each test
variant (sloppy, strict, module, raw) is one count, as the runner reports them separately.
`fail` is fail, timeout and crash; `skipped` is a variant the runner did not execute. Status is
`complete` (all pass), `unsupported-by-runner` (all skipped, normally `feature-not-supported:
<feature>`), `none` (no pass and at least one executed variant) or `partial`.
Failure messages group after normalisation: a number becomes `N`, and text quoted with
backticks, double quotes or `«»` becomes `X`.

The TSVs start with `# ` comment lines (what generated them, which results file and its date,
the rules above); readers must skip them, the first data line after them is the column header.

`docs/missing/` is the checked-in inventory: both tables came from
`<work-dir>/pin-icu-2-3/full-icu23.jsonl` (2026-11-12, 92890 variants, no `intl402` or `staging` tests:
`<work-dir>/lucinanet/bottleneck/cc/misc/full.jsonl` of 2026-11-11 with its 9206 `built-ins/Temporal`
rows replaced by a run of that directory on fork pin `d48f9b706c`) merged with
`<test262-checkout>/pin-intl-locale/intl402-after.jsonl`
(2026-11-13, 6674 `intl402` variants, 4858 passing, run with `--with-intl` on fork pin `1f065a160e`),
corpus `test262.sha`, 99564 variants in total. That `intl402` run replaced the one on fork pin
`707ec0fd6d` (3398 passing) that the tables carried before.

Of the non-`intl402` file, only the `built-ins/Temporal` rows have been re-measured since it was
assembled: a run of that directory on `1f065a160e` returned the same result for all 9206 variants, so
those rows still describe the current engine. The `annexB` rows do not: they are from the 2026-11-11
engine, before `JSCLASS_EMULATES_UNDEFINED` landed, and today 70 of the variants those rows call
`skipped` are executed (20 pass, 50 fail — the `*emulates-undefined*` files, which need the
`$262.IsHTMLDDA` host hook the runner does not provide). Only a full non-`intl402` run would fold that
in honestly, so it is left outstanding rather than patched by hand.
