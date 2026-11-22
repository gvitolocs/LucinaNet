#!/usr/bin/env bash
# Runs the proof of mozjs-sys/src/engine/store_buffer_rules.rs and its mutants. From the js workspace root:
#   verification/store-buffer/run.sh
# The two shipped files and mutants.sh are copied into a directory of their own, so that nothing depends on a path on the machine
# that runs Verus. Verus is the `verus` on PATH, or the binary named by VERUS.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
stage=$(mktemp -d "${TMPDIR:-/tmp}/store-buffer-stage.XXXXXX")
trap 'rm -rf "${stage:?}"' EXIT
cp "$root/mozjs-sys/src/boundary/value_bits.rs" "$root/mozjs-sys/src/engine/store_buffer_rules.rs" "$root/verification/store-buffer/mutants.sh" "$stage/"
verus=${VERUS:-verus}
command -v "$verus" >/dev/null 2>&1 || { echo "no Verus to run: '$verus' is not an executable here (put verus on PATH, or set VERUS)" >&2; exit 1; }
cd "$stage"
exec env VERUS="$verus" ./mutants.sh value_bits.rs store_buffer_rules.rs
