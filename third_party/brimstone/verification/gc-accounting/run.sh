#!/usr/bin/env bash
# Runs the proof of src/js/runtime/gc/accounting.rs and its mutants. From the repo root on the build host:
#   verification/gc-accounting/run.sh
# The shipped file and mutants.sh are copied into a directory of their own, so that nothing depends on a path on the machine that
# runs Verus. Verus runs on the proof host (`verus-remote`); if the proof host does not answer, FALLBACK=1 runs it here instead.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
stage=$(mktemp -d "${TMPDIR:-/tmp}/gc-accounting-stage.XXXXXX")
trap 'rm -rf "${stage:?}"' EXIT
cp "$root/src/js/runtime/gc/accounting.rs" "$root/verification/gc-accounting/mutants.sh" "$stage/"
if [ "${FALLBACK:-0}" = 1 ]; then
    cd "$stage"
    VERUS=/home/user/tools/verus/verus-x86-linux/verus exec ./mutants.sh accounting.rs
fi
exec "$HOME/.local/bin/verus-remote" "$stage" ./mutants.sh accounting.rs
