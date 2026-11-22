#!/usr/bin/env bash
# Shows that verification/ftcompat.rs can fail: each try plants one bug in a copy of the shipped
# crates/lucina-ftcompat/src/lib.rs, and Verus has to refuse it. Run from the repository root; this script
# calls Verus directly, one harness at a time.
# Exits non-zero when a mutant is not refused or its text is not found.
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
work=$(mktemp -d "${TMPDIR:-/tmp}/ftcompat-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
try() { try_in crates/lucina-ftcompat/src/lib.rs ftcompat.rs "$@"; }
tryb() { try_in crates/lucina-fontsan/src/bounds.rs fontsan_bounds.rs "$@"; }
try_in() {
    local src=$1 harness=$2 from=$3 to=$4
    rm -rf "$work/crates" "$work/verification"
    mkdir -p "$work/$(dirname "$src")" "$work/verification"
    cp "$src" "$work/$src"
    cp "verification/$harness" "$work/verification/"
    local target=$work/$src
    local text; text=$(<"$target")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$target"
    if cmp -s "$target" "$src"; then echo "NOT CHANGED: $from"; fail=1; return; fi
    local rc
    "$verus" "$work/verification/$harness" > "$work/out" 2>&1
    rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    elif ! grep -q '^error' "$work/out"; then
        # A non-zero exit without a Verus error (missing binary, crash) is not a refusal.
        echo "NOT REFUSED (Verus failed without an error message, exit $rc): $from -> $to"; fail=1
    else
        echo "refused: $from -> $to ($(grep -c '^error' "$work/out") errors)"
    fi
}

# mul_fix: wrong shift, wrong sign term, wrong rounding constant (in fixed_to_int's twin formula).
try 'let r32 = (t >> 16) as i32;' 'let r32 = (t >> 15) as i32;'
try 'wrapping_add(ab >> 63);' 'wrapping_add(ab >> 62);'
try 'let t = (x as u32).wrapping_add(0x8000);' 'let t = (x as u32).wrapping_add(0x4000);'
# div_fix: wrong zero-divisor result, wrong rounding of the quotient.
try $'} else {\n        0x7FFF_FFFF\n    };' $'} else {\n        0x7FFF_FFFE\n    };'
try 'wrapping_add(bu >> 1)' 'wrapping_add(bu >> 2)'
# mul_fix rounding constant, div_fix scale.
try 'let t = ab.wrapping_add(0x8000)' 'let t = ab.wrapping_add(0x7FFF)'
try 'au.wrapping_shl(16)' 'au.wrapping_shl(15)'
# mul_div: wrong rounding of the quotient.
try 'wrapping_add(cu >> 1)' 'wrapping_add(cu >> 2)'
# pix_round / pix_floor / pix_ceil: wrong mask.
try '(x + 32) & !63' '(x + 32) & !31'
try $'\n    x & !63\n' $'\n    x & !31\n'
try '(x + 63) & !63' '(x + 63) & !31'
# ppem: wrong divisor.
try 'let q = t / 64;' 'let q = t / 63;'
# embolden_strength: wrong divisor.
try 'y_scale) / 48;' 'y_scale) / 47;'
# choose_strike: >= vs > in the update condition, wrong winner, sign flip on the first distance.
try $'dist >= best_dist)\n            || (if dist < 0' $'dist > best_dist)\n            || (if dist < 0'
try 'best = i;' 'best = 0;'
try 'let mut best_dist = first - requested;' 'let mut best_dist = requested - first;'
# face_metrics: fallback branch drops the line gap.
try $'.wrapping_sub(descender)\n                    .wrapping_add(os2.typo_line_gap);' $'.wrapping_sub(descender);'
# preset_bitmap: wrong LCD padding, wrong range check, Mono widening on the wrong side, a dropped guard.
try $'preset_axis_aa(x_min, x_max, 21),\n            preset_axis_aa(y_min, y_max, 0)' $'preset_axis_aa(x_min, x_max, 0),\n            preset_axis_aa(y_min, y_max, 0)'
try '|| px_max > 0x7FFF ||' '|| px_max > 0x8000 ||'
try '            hi += 1;' '            lo += 1;'
try 'if x_min < -LIMIT || x_max > LIMIT ||' 'if x_min < -LIMIT ||'
# lucina-fontsan bounds.rs: off-by-one limits, a lost alignment rule, an unchecked sum, a wrapping cap.
tryb 'length <= MAX_FILE_SIZE' 'length < MAX_FILE_SIZE'
tryb 'num_tables >= 1 && num_tables < MAX_NUM_TABLES' 'num_tables < MAX_NUM_TABLES'
tryb '    offset & 3 == 0
}' '    offset & 1 == 0
}'
tryb 'Some(x) => {' 'Some(_x) => { let x = v;'
tryb '(offset as u64) >= data_offset && (offset as u64) < length' '(offset as u64) >= data_offset && (offset as u64) <= length'
tryb 'if new > u32::MAX as u64 {' 'if new > u64::MAX - 1 {'
tryb 'let new = sum.checked_add(add as u64)?;' 'let new = sum.wrapping_add(add as u64);'
tryb 'Some(e) => e <= table_len,' 'Some(e) => e < table_len,'
tryb '64 * (input_len as u128) + 64 * 1024 * 1024' '64 * (input_len as u128) + 65 * 1024 * 1024'
tryb 'round4(end).unwrap_or(u64::MAX)' 'end'
tryb 'let range_shift = 16 * num_tables - search_range;' 'let range_shift = 16 * num_tables - search_range + 1;'
exit $fail
