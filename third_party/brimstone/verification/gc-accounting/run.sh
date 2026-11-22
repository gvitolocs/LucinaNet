#!/usr/bin/env bash
# Runs the proof of src/js/runtime/gc/accounting.rs and its mutants. From the repo root:
#   verification/gc-accounting/run.sh
# The shipped file and mutants.sh are copied into a directory of their own, so that nothing depends on a path on the machine that
# runs Verus. Verus is the `verus` on PATH, or the binary named by VERUS.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
stage=$(mktemp -d "${TMPDIR:-/tmp}/gc-accounting-stage.XXXXXX")
trap 'rm -rf "${stage:?}"' EXIT
cp "$root/src/js/runtime/gc/accounting.rs" "$root/verification/gc-accounting/mutants.sh" "$stage/"
verus=${VERUS:-verus}
command -v "$verus" >/dev/null 2>&1 || { echo "no Verus to run: '$verus' is not an executable here (put verus on PATH, or set VERUS)" >&2; exit 1; }
cd "$stage"
exec env VERUS="$verus" ./mutants.sh accounting.rs
