# lucinanet-wpt-jsshell

Runs the `jsshell` variant of WPT tests through the Brimstone engine as a JavaScript
shell, and writes the results as JSONL. Slice I of the WebAssembly work
(`docs/lucinajs-wasm-design.md` §7.3, §5): the point is to be able to ask what the engine
asks of `WebAssembly`, file by file, with nothing else standing in for it.

```
lucinanet-wpt-jsshell --wpt <corpus/tests> --filter wasm/jsapi/ --out results.jsonl \
    [--summary summary.json] [--timeout-secs 10] [--jobs 4]
```

`--wpt` is the directory that holds `resources/testharness.js` - the read-only corpus
checkout, e.g. `/home/user/Projects/Lucina/LucinaNet-servo/tests/wpt/tests`. It is
read-only: the runner never writes inside it. `--filter` is a substring of the path
relative to that root, repeatable; the walk starts at the filters' common directory, so
one filter over `wasm/jsapi/` does not read the whole corpus.

## What it runs

A `.any.js` file *is* the jsshell variant of an `any` test (`tools/manifest/sourcefile.py`
maps `jsshell` to the bare source file), so only files whose `// META: global=` list
includes `jsshell` are candidates. The rest are reported as `skipped: not-jsshell`
without being opened by an engine - `idlharness.any.js` needs `idlharness.js` and a
`document`, and a shell has neither.

Each candidate runs in one realm, in this order:

1. `src/shell_env.js` - the environment testharness.js expects to find when there is no
   `document` and no worker global (`resources/testharness.js:523-553`): `self`, `print`,
   a `console` over `__wptLog`, and `setTimeout`/`clearTimeout` in a timer registry.
2. the title prelude - `META_TITLE` from the header and a `location` whose `pathname` is
   the test's path, since `get_title()` falls back to it for unnamed tests.
3. `resources/testharness.js`, from the corpus.
4. `src/shell_callbacks.js` - `add_result_callback`, `add_test_state_callback` and
   `add_completion_callback`, plus `__wptSnapshot()`.
5. the file's own `// META: script=` files, in order, resolved against the test's
   directory or against the `--wpt` root when the entry starts with `/`.
6. the test itself.

Then the driver loop runs microtasks (`js::RunJobs`), fires due timers, and repeats until
the harness reports completion or the wall clock ends the run. The loop is what makes
`promise_test` work: `ShellTestEnvironment` becomes ready one microtask after the harness
is evaluated, so a runner that stopped after the first script would report nothing.

A file that ends without the harness completing gets a snapshot instead: the runner asks
the harness which tests it knows about, and a test that started but never settled is a
TIMEOUT rather than a missing row (`src/exec.rs::status_name`).

## Isolation

One process per file, spawned by the parent, four at a time by default. An engine fault
therefore costs a file, not a run: a child that dies or overruns `--timeout-secs` plus
`--grace-secs` is recorded as `crash` or `timeout` with whatever it had printed, and the
pool moves on. The child's stdout is JSONL only - `print`, `console` and harness
diagnostics go to stderr - so a crashed child still yields parsable records.

## Records

One line per subtest, one per file, one summary at the end.

```json
{"kind":"subtest","file":"wasm/jsapi/interface.any.js","name":"WebAssembly: property descriptor","status":"FAIL","message":"assert_equals: expected \"object\" but got \"undefined\""}
{"kind":"file","file":"wasm/jsapi/interface.any.js","result":"error","reason":"...ReferenceError: `WebAssembly` is not defined","harness_status":"OK","subtests":27,"pass":0,"fail":27,"timeout":0,"notrun":0,"precondition_failed":0,"secs":0.208,"runner_errors":[]}
{"kind":"summary","tool":"lucinanet-wpt-jsshell","files":80,"files_run":71,"subtests":703,"pass":1,"fail":702,"...":"..."}
```

`status` is the harness's own: `PASS`, `FAIL`, `TIMEOUT`, `NOTRUN`,
`PRECONDITION_FAILED`. `result` is the file's: `ran`, `timeout`, `skipped`, `error` (a
file of the test's own threw or did not parse - an engine gap, and the subtests that did
run are still reported), `harness-error` (our glue, or the corpus harness, could not be
loaded), `crash` (the child died or was killed). Files are emitted in path order
regardless of the order children finished in, so two runs diff cleanly.

## Limits

- `--timeout-secs` bounds the driver loop, not a single long synchronous test body: a
  file that spins for 12 s in the middle of its top level is only stopped by the parent's
  `timeout + grace` kill. That is what an engine without an interrupt callback allows.
- No `document`, no worker, no `fetch`, no `WebAssembly`. Browser- and worker-shaped tests
  are out of scope here; this runner exists for the shell-shaped ones.
- `.any.js` only, from the META header: `.any.html`, `.https` variants and
  `manual-`/`reftests` are not candidates for a shell runner.
- The engine has no `WebAssembly` global yet, so today's honest output is a baseline of
  FAILs - `docs/missing/` in the design doc is generated from it, not from here.

## Tests

`cargo test` runs 7 unit tests (`src/meta.rs`, `src/exec.rs`) and 4 integration tests
(`tests/wpt_jsshell.rs`) over `tests/fixtures/`: a 250-line stand-in for
`resources/testharness.js` that reproduces the shapes the runner depends on, plus one
fixture per outcome - results in both directions from `test()` and `promise_test()`, a
test that never settles, a file that does not parse, a `script=` that throws, and a file
with no `jsshell` in its header. The fixtures exist so the JSONL contract is pinned
without a corpus checkout; the corpus itself is validated by hand, in a temporary checkout.

```
cargo build --offline     # or, on the build host:
cargo test --offline
```
