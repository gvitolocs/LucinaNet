#!/usr/bin/env bash
# Shows that Verus can fail on crates/lucinanet-history/src/policy.rs: each line plants one bug in a copy, and Verus must refuse it.
# Run on the build host from the repository root, through the verification script: crates/lucinanet-history/mutants.sh
set -uo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
shipped=$root/crates/lucinanet-history/src/policy.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/history-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
mkdir -p "$work/raster" "$work/crates/lucinanet-history/src"
cp "$root/raster/verify.sh" "$work/raster/"
fail=0
refused=0
try() {
    local from=$1 to=$2
    local text; text=$(<"$shipped")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$work/crates/lucinanet-history/src/policy.rs"
    if FILES=crates/lucinanet-history/src/policy.rs "$work/raster/verify.sh" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        refused=$((refused + 1))
        echo "refused: $from -> $to ($(grep -c "^error" "$work/out") errors)"
    fi
}
# (The values between the steps of the Firefox table are pinned by the tests against the formula, not by Verus.)
# Firefox table: a bigger count at the top, a count below 64 MiB.
try '    return 8;
}' '    return 9;
}'
try 'if total_mib < 64 {
        return 0;' 'if total_mib < 64 {
        return 1;'
# Pressure: critical not zero, low not capped, the polling levels.
try 'if pressure >= PRESSURE_CRITICAL {
        return 0;' 'if pressure > PRESSURE_CRITICAL {
        return 0;'
try 'if pressure == PRESSURE_LOW && n > LOW_PRESSURE_ENTRIES {' 'if pressure == PRESSURE_LOW && n > LOW_PRESSURE_ENTRIES + 1 {'
try 'if n > VIEWER_WINDOW {' 'if n > VIEWER_WINDOW + 1 {'
try 'if available_mib < LOW_BELOW_MIB {' 'if available_mib < LOW_BELOW_MIB - 100 {'
try 'if available_mib < CRITICAL_BELOW_MIB {' 'if available_mib < CRITICAL_BELOW_MIB - 100 {'
# Reserve: under the floor; at the reserve still keeping a page.
try 'return RESERVE_MIN_MIB;' 'return RESERVE_MIN_MIB - 1;'
# (available_mib < reserve instead of <= is equivalent: usable is 0 and so is the count.)
# Memory arithmetic: the cap and the usable memory.
try 'let n = if by_memory < ram as u64 { by_memory as u32 } else { ram };' 'let n = if by_memory < ram as u64 { ram } else { by_memory as u32 };'
try 'let usable = available_mib - reserve;' 'let usable = available_mib;'
# Ages: one entry past the end, the wrong side kept, the evict count off.
try 'while fresh < ages.len() && ages[ages.len() - 1 - fresh] <= ttl' 'while fresh <= ages.len() && ages[ages.len() - 1 - fresh] <= ttl'
try 'let keep = if cap < fresh { cap } else { fresh };' 'let keep = if cap < fresh { fresh } else { cap };'
try 'evict: ages.len() - keep }' 'evict: ages.len() - keep + 1 }'
# Tabs: the share bigger than the whole, the reserve forgotten.
try 'return reserve + (available_mib - reserve) / tabs;' 'return reserve + (available_mib - reserve) * tabs;'
try 'if tabs <= 1 || available_mib <= reserve {' 'if tabs <= 1 {'
echo "refused $refused, failures $fail"
exit "$fail"
