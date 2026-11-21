#!/usr/bin/env bash
# Runs the proof of one shipped file of src/js/runtime/bytecode/fncache and its mutants. From the repo root on the build host:
#   verification/fncache/run.sh disk_rules
# The shipped file and mutants.sh are copied into a directory of their own, so that nothing depends on a path on the machine that
# runs Verus. Verus runs on the proof host (`verus-remote`); if the proof host does not answer, FALLBACK=1 runs it here instead.
set -euo pipefail
name=${1:?name of the proof (disk_rules)}
root=$(cd "$(dirname "$0")/../.." && pwd)
stage=$(mktemp -d "${TMPDIR:-/tmp}/fncache-stage.XXXXXX")
trap 'rm -rf "${stage:?}"' EXIT
cp "$root/src/js/runtime/bytecode/fncache/$name.rs" "$root/verification/fncache/mutants.sh" "$stage/"
if [ "${FALLBACK:-0}" = 1 ]; then
    cd "$stage"
    VERUS=/home/user/tools/verus/verus-x86-linux/verus exec ./mutants.sh "$name" "$name.rs"
fi
exec "$HOME/.local/bin/verus-remote" "$stage" ./mutants.sh "$name" "$name.rs"
