#!/usr/bin/env bash
# Runs the proof of one shipped file of src/js/runtime/bytecode/fncache and its mutants. From the repo root:
#   verification/fncache/run.sh disk_rules
# The shipped file and mutants.sh are copied into a directory of their own, so that nothing depends on a path on the machine that
# runs Verus. Verus is the `verus` on PATH, or the binary named by VERUS.
set -euo pipefail
name=${1:?name of the proof (disk_rules)}
root=$(cd "$(dirname "$0")/../.." && pwd)
stage=$(mktemp -d "${TMPDIR:-/tmp}/fncache-stage.XXXXXX")
trap 'rm -rf "${stage:?}"' EXIT
cp "$root/src/js/runtime/bytecode/fncache/$name.rs" "$root/verification/fncache/mutants.sh" "$stage/"
verus=${VERUS:-verus}
command -v "$verus" >/dev/null 2>&1 || { echo "no Verus to run: '$verus' is not an executable here (put verus on PATH, or set VERUS)" >&2; exit 1; }
cd "$stage"
exec env VERUS="$verus" ./mutants.sh "$name" "$name.rs"
