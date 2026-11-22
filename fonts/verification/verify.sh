#!/usr/bin/env bash
# Verus on the shipped files: verification/ftcompat.rs (crates/lucina-ftcompat/src/lib.rs) and
# verification/fontsan_bounds.rs (crates/lucina-fontsan/src/bounds.rs). Each harness is run on its own,
# calling Verus directly.
# Exits non-zero when Verus reports any error, and also when a harness does not print
# "verification results:: N verified, 0 errors" with N >= 1 (a run that verifies nothing must not pass).
set -uo pipefail
cd "$(dirname "$0")/.."
verus=${VERUS:-}
if [ -z "$verus" ]; then
    if [ -x "$HOME/tools/verus/verus-x86-linux/verus" ]; then
        verus=$HOME/tools/verus/verus-x86-linux/verus
    else
        verus=verus
    fi
fi
status=0
for harness in verification/ftcompat.rs verification/fontsan_bounds.rs; do
    echo "== $harness"
    out=$(mktemp)
    "$verus" "$harness" 2>&1 | tee "$out"
    rc=${PIPESTATUS[0]}
    if [ "$rc" -ne 0 ]; then
        echo "FAIL: $harness: Verus exited $rc"
        status=1
    elif ! grep -Eq 'verification results:: [1-9][0-9]* verified, 0 errors' "$out"; then
        echo "FAIL: $harness: no 'N verified, 0 errors' line with N >= 1"
        status=1
    else
        echo "ok: $harness"
    fi
    rm -f "$out"
done
exit $status
