# LucinaNet engine acceptance

Run from LucinaNet-js:

```sh
cargo run --locked --offline --manifest-path tools/lucinanet-engine-check/Cargo.toml
```

The tool builds mozjs tests and runs each case in a new process. This avoids the
one-JSEngine-per-process restriction while preserving all test failures. Each
case has a timeout, its own log, and an entry in `target/lucinanet-engine-check/results.json`.
Zero selected tests is an error. Empty libtest targets are listed separately.
A failed, crashed, or timed-out test makes this command exit nonzero.
Use `--filter`, `--timeout-secs`, `--repo`, or `--out` to narrow or redirect a run.
This gate measures host API integration; it is not a test262 or browser-compatibility percentage.
