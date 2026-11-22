#!/usr/bin/env bash
# Shows that src/js/parser/reparse_rules.rs is what keeps Verus on the function chain and entry packing rules: each `try` plants one bug in a copy of the
# shipped file, turned into what Verus reads by the mechanical steps below, and Verus must refuse every mutant.
#   verification/reparse/mutants.sh <path to reparse_rules.rs>          (needs `verus` on PATH, or VERUS=...)
# MUTANTS_STRICT=1: exit 1 if any mutant was refused without a verification run (a compile error, not a disproof).
# No argument: the shipped file below is read from the repo root, which is how the CI proof precheck runs this script.
# Normally started by verification/reparse/run.sh, which copies reparse_rules.rs and this script into a directory of their own and
# runs Verus on them.
set -uo pipefail
verus=${VERUS:-verus}
rules=${1:-src/js/parser/reparse_rules.rs}
work=$(mktemp -d "${TMPDIR:-/tmp}/reparse-mutants.XXXXXX")
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
# The search for the nearest non-arrow entry: its direction, its stop, its start and its step.
try "while i > 0 && arrows[i - 1]" "while i > 0 && !arrows[i - 1]"
try "while i > 0 && arrows[i - 1]" "while i > 1 && arrows[i - 1]"
try "while i > 0 && arrows[i - 1]" "while i > 0 && arrows[i - 1] && i > 2"
try "let mut i = arrows.len();" "let mut i = arrows.len() - 1;"
try "        i = i - 1;" "        i = i - 2;"
# Where the chain starts: at the nearest non-arrow entry (index i - 1), or at the bottom.
try "let start = if i > 0 { i - 1 } else { 0 };" "let start = if i > 0 { i } else { 0 };"
try "let start = if i > 0 { i - 1 } else { 0 };" "let start = if i > 0 { i - 1 } else { 1 };"
try "let start = if i > 0 { i - 1 } else { 0 };" "let start = if i > 2 { i - 1 } else { 0 };"
try "let start = if i > 0 { i - 1 } else { 0 };" "let start = i;"
# The refusal: strictly longer than the snapshot holds, and the two outcomes.
try "if arrows.len() - start > MAX_CHAIN {" "if arrows.len() - start >= MAX_CHAIN {"
try "if arrows.len() - start > MAX_CHAIN {" "if arrows.len() - start > MAX_CHAIN + 1 {"
try "if arrows.len() - start > MAX_CHAIN {" "if arrows.len() > MAX_CHAIN {"
try "if arrows.len() - start > MAX_CHAIN {" "if false {"
try "        Some(start)" "        Some(0)"
try "        Some(start)" "        None"
try $'if arrows.len() - start > MAX_CHAIN {\n        None' $'if arrows.len() - start > MAX_CHAIN {\n        Some(start)'
# Packing: the value of each flag and its shift.
try "let b0: u8 = if f0 { 1 } else { 0 };" "let b0: u8 = if f0 { 0 } else { 1 };"
try "let b2: u8 = if f2 { 1 } else { 0 };" "let b2: u8 = if f3 { 1 } else { 0 };"
try "let b4: u8 = if f4 { 1 } else { 0 };" "let b4: u8 = if f4 { 2 } else { 0 };"
try "b1 << 1u8" "b1 << 2u8"
try "b3 << 3u8" "b3 << 2u8"
try "b5 << 5u8" "b5 << 4u8"
try "b0 | b1 << 1u8" "b1 | b0 << 1u8"
# Reading a flag back: the shift, the mask and the comparison.
try "(bits >> i) & 1 == 1" "(bits >> (i + 1)) & 1 == 1"
try "(bits >> i) & 1 == 1" "(bits >> i) & 3 == 1"
try "(bits >> i) & 1 == 1" "(bits >> i) & 1 == 0"
try "(bits >> i) & 1 == 1" "(bits << i) & 1 == 1"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
