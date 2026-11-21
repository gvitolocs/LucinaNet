#!/usr/bin/env bash
# Runs the proof of src/js/parser/reparse_rules.rs and its mutants. From the repo root on the build host:
#   verification/reparse/run.sh
# The shipped file and mutants.sh are copied into a directory of their own, so that nothing depends on a path on the machine that
# runs Verus. Verus runs on the proof host (`verus-remote`); if the proof host does not answer, FALLBACK=1 runs it here instead.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
stage=$(mktemp -d "${TMPDIR:-/tmp}/reparse-stage.XXXXXX")
trap 'rm -rf "${stage:?}"' EXIT
cp "$root/src/js/parser/reparse_rules.rs" "$root/verification/reparse/mutants.sh" "$stage/"
if [ "${FALLBACK:-0}" = 1 ]; then
    cd "$stage"
    VERUS=/home/user/tools/verus/verus-x86-linux/verus exec ./mutants.sh reparse_rules.rs
fi
exec "$HOME/.local/bin/verus-remote" "$stage" ./mutants.sh reparse_rules.rs
