#!/usr/bin/env bash
# Runs the proof of mozjs-sys/src/engine/store_buffer_rules.rs and its mutants. From the repo root on the build host:
#   verification/store-buffer/run.sh
# The two shipped files and mutants.sh are copied into a directory of their own, so that nothing depends on a path on the machine
# that runs Verus. Verus runs on the proof host (`verus-remote`); if the proof host does not answer, FALLBACK=1 runs it here instead.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
stage=$(mktemp -d "${TMPDIR:-/tmp}/store-buffer-stage.XXXXXX")
trap 'rm -rf "${stage:?}"' EXIT
cp "$root/mozjs-sys/src/boundary/value_bits.rs" "$root/mozjs-sys/src/engine/store_buffer_rules.rs" "$root/verification/store-buffer/mutants.sh" "$stage/"
if [ "${FALLBACK:-0}" = 1 ]; then
    cd "$stage"
    VERUS=/home/user/tools/verus/verus-x86-linux/verus exec ./mutants.sh value_bits.rs store_buffer_rules.rs
fi
exec "$HOME/.local/bin/verus-remote" "$stage" ./mutants.sh value_bits.rs store_buffer_rules.rs
