#!/usr/bin/env bash
# Shows that src/js/runtime/gc/accounting.rs is what keeps Verus on the memory accounting triggers: each `try` plants one bug in a copy of the
# shipped file, turned into what Verus reads by the mechanical steps below, and Verus must refuse every mutant.
#   verification/gc-accounting/mutants.sh <path to accounting.rs>          (needs `verus` on PATH, or VERUS=...)
# MUTANTS_STRICT=1: exit 1 if any mutant was refused without a verification run (a compile error, not a disproof).
# No argument: the shipped file below is read from the repo root, which is how the CI proof precheck runs this script.
# Normally started by verification/gc-accounting/run.sh, which copies accounting.rs and this script into a directory of their own and
# runs Verus on them.
set -uo pipefail
verus=${VERUS:-verus}
rules=${1:-src/js/runtime/gc/accounting.rs}
work=$(mktemp -d "${TMPDIR:-/tmp}/gc-accounting-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT

# What Verus reads from the shipped file: the mechanical steps of scripts/verify-shipped.sh.
make_verus_input() {
    local body
    body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$1")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body"
}

# Undoing those steps must give the shipped code back, so that what Verus checks is the code that is compiled.
shipped_code=$(sed -E -e '/^ *\/\/@/d' -e '/\/\/@omit$/d' -e 's/ \/\/@ret$//' "$rules")
read_back=$(make_verus_input "$rules" | sed -e '1,2d' | head -n -2 | grep -vF '/*proof*/' | sed -E 's/-> \(ret: (.*)\)$/-> \1/')
if [ "$shipped_code" != "$read_back" ]; then
    echo "WHAT VERUS READS IS NOT THE SHIPPED CODE"
    diff <(echo "$shipped_code") <(echo "$read_back") | head -20
    exit 1
fi

# The unchanged file must verify, or nothing below means anything.
make_verus_input "$rules" > "$work/base.rs"
if ! "$verus" "$work/base.rs" > "$work/base.out" 2>&1; then
    echo "BASE FAILS"
    cat "$work/base.out"
    exit 1
fi
echo "baseline: verifies"

fail=0
refused=0
weak=0
try() {
    local from=$1 to=$2
    local text; text=$(<"$rules")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    local mut; mut=${text/"$from"/"$to"}
    if [ "$mut" = "$text" ]; then echo "NOT CHANGED: $from"; fail=1; return; fi
    local in=$work/mutant-src.rs out=$work/mutant.rs
    printf "%s\n" "$mut" > "$in"
    make_verus_input "$in" > "$out"
    if "$verus" "$out" > "$work/verus.out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"
        cat "$work/verus.out"
        fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/verus.out" && ! grep -q 'error\[E' "$work/verus.out"; then
            refused=$((refused + 1))
            echo "refused: $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
            weak=$((weak + 1))
        fi
    fi
}
# The growth factor: low frequency or a small retained size uses the low factor; while frequent, small/large/interpolated.
try "if !high_frequency || retained < ONE_MIB {" "if !high_frequency {"
try "if !high_frequency || retained < ONE_MIB {" "if retained < ONE_MIB {"
try "if !high_frequency || retained < ONE_MIB {" "if !high_frequency || retained <= ONE_MIB * 2 {"
try "    } else if retained <= small {" "    } else if retained < small / 2 {"
try "        high_small
    } else if retained >= large {" "        high_large
    } else if retained >= large {"
try "    } else if retained >= large {
        high_large" "    } else if retained >= large {
        high_small"
try "high_small - ((high_small - high_large) * (retained - small)) / (large - small)" "high_small - ((high_small - high_large) * (retained - small)) / (large - small) * 2"
try "high_small - ((high_small - high_large) * (retained - small)) / (large - small)" "high_large + ((high_small - high_large) * (retained - small)) / (large - small) + 1"
try "        low
    } else if retained <= small {" "        low + 1
    } else if retained <= small {"
# The threshold: the larger of retained and base, times the factor, saturated.
try "let b = if retained > base { retained } else { base };" "let b = retained;"
try "let b = if retained > base { retained } else { base };" "let b = base;"
try "let b = if retained > base { retained } else { base };" "let b = if retained > base { base } else { retained };"
try "if b > usize::MAX / MAX_TENTHS { usize::MAX } else { b * factor_tenths / 10 }" "if b > usize::MAX / MAX_TENTHS { usize::MAX } else { b * factor_tenths / 20 }"
try "if b > usize::MAX / MAX_TENTHS { usize::MAX } else { b * factor_tenths / 10 }" "if b > usize::MAX / MAX_TENTHS { 0 } else { b * factor_tenths / 10 }"
try "if b > usize::MAX / MAX_TENTHS { usize::MAX } else { b * factor_tenths / 10 }" "if b > usize::MAX / MAX_TENTHS { usize::MAX } else { b * 15 / 10 }"
# Due when the counter reaches the threshold, not after.
try "    bytes >= threshold" "    bytes > threshold"
try "    bytes >= threshold" "    bytes >= threshold / 2"
# Charges saturate, removals stop at zero.
try "if count > usize::MAX - n { usize::MAX } else { count + n }" "if count > usize::MAX - n { count } else { count + n }"
try "if count > usize::MAX - n { usize::MAX } else { count + n }" "if count > usize::MAX - n { usize::MAX } else { count }"
try "if n > count { 0 } else { count - n }" "if n > count { count } else { count - n }"
try "if n > count { 0 } else { count - n }" "if n >= count { 0 } else { count - n + 1 }"
# The maximum heap: powers of two from the minimum, within the options and JSGC_MAX_BYTES.
try "while p <= options_max / 2 && p * 2 <= max_bytes" "while p <= options_max / 2"
try "while p <= options_max / 2 && p * 2 <= max_bytes" "while p * 2 <= max_bytes && p < options_max"
try "while p <= options_max / 2 && p * 2 <= max_bytes" "while p < options_max / 2 && p * 2 < max_bytes"
try "        p = p * 2;" "        p = p * 2 + 1;"
try "        p = p * 2;" "        p = p + min_heap;"
# The constants are SpiderMonkey's.
try "pub const MALLOC_BASE: usize = 38 << 20;" "pub const MALLOC_BASE: usize = 38 << 10;"
try "pub const HIGH_SMALL_TENTHS: usize = 30;" "pub const HIGH_SMALL_TENTHS: usize = 20;"
# Release free memory only when collections are infrequent, always when shrinking.
try "if shrinking { true } else { !high_frequency }" "if shrinking { false } else { !high_frequency }"
try "if shrinking { true } else { !high_frequency }" "if shrinking { true } else { high_frequency }"
try "if shrinking { true } else { !high_frequency }" "if shrinking { true } else { true }"
try "if shrinking { true } else { !high_frequency }" "if shrinking { true } else { false }"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
