#!/usr/bin/env bash
# Runs the proof of src/js/parser/reparse_rules.rs and its mutants. From the repo root:
#   verification/reparse/run.sh
# The shipped file and mutants.sh are copied into a directory of their own, so that nothing depends on a path on the machine that
# runs Verus. Verus is the `verus` on PATH, or the binary named by VERUS.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
stage=$(mktemp -d "${TMPDIR:-/tmp}/reparse-stage.XXXXXX")
trap 'rm -rf "${stage:?}"' EXIT
cp "$root/src/js/parser/reparse_rules.rs" "$root/verification/reparse/mutants.sh" "$stage/"
verus=${VERUS:-verus}
command -v "$verus" >/dev/null 2>&1 || { echo "no Verus to run: '$verus' is not an executable here (put verus on PATH, or set VERUS)" >&2; exit 1; }
cd "$stage"
exec env VERUS="$verus" ./mutants.sh reparse_rules.rs
