#!/usr/bin/env bash
# The global non-vacuity list: each mutation weakens shipped code or strengthens a claim into a false one, and Verus must REJECT the
# mutant. Brimstone keeps no mutations here: every mutant lives with the unit it attacks, as a `try` line of
# verification/<unit>/mutants.sh, which is what verification/<unit>/run.sh runs. This script exists because the proof inventory
# reads it as the `mutations/global` unit; the list below is
# the remainder, and it is empty.
# Run it through the verification script: scripts/verify-mutations.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
verus=${VERUS:-verus}
work=$(mktemp -d "${TMPDIR:-/tmp}/verify-mutations.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT

# file | sed expression (must change the file) | what it breaks. The fields are split on "|", so the sed expression cannot contain one.
mutations=(
)

status=0
for m in "${mutations[@]}"; do
    IFS='|' read -r file expr what <<< "$m"
    mutant="$work/$(basename "$file" .rs)-mutant.rs"
    sed "$expr" "$root/$file" > "$mutant"
    if cmp -s "$root/$file" "$mutant"; then
        echo "STALE $what: the sed expression no longer matches $file"
        status=1
        continue
    fi
    if "$verus" "$mutant" > "$work/out" 2>&1; then
        echo "SURVIVED $what"
        status=1
    else
        echo "refused: $what"
    fi
done
echo "${#mutations[@]} global mutations tried"
exit "$status"
