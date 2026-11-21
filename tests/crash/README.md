# Crash regression gate

`tests/crash/run.sh [--bin PATH] [--out DIR] [--bbc N] [--stress N]` runs two recipes that crashed the JS engine
before LucinaNet-js 14a77db (accessor GC fix 43174fc, realm principals fix b1b7ecd), against a built binary:

- **bbc**: `--render https://www.bbc.com/news` with `LUCINANET_RENDER_HOLD=6` (default 6 runs; principals bug).
- **stress**: `--render https://github.com/servo/servo` with `LUCINANET_JS_GC_STRESS_KB=256` (default 3 runs; accessor handle bug).

A run passes when the exit code is 0 and the PNG is larger than 10 KB. Runs go one at a time. A non-zero exit with no
`lucinanet-crash`, `Segmentation` or `signal 11` in stderr (and not code 139) counts as `skip (network)`, not a failure.
Exit codes: 0 pass, 1 failure, 2 binary not executable.

It needs the network and takes many minutes, so it belongs in the slower tier, not the verification step.
