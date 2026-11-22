#!/usr/bin/env bash
# Shows that src/js/runtime/bytecode/call_position.rs is what keeps Verus on the call position rule: each `try` plants one bug in a copy of the
# shipped file, turned into what Verus reads by the mechanical steps below, and Verus must refuse every mutant.
#   verification/call-position/mutants.sh <path to call_position.rs>          (needs `verus` on PATH, or VERUS=...)
# MUTANTS_STRICT=1: exit 1 if any mutant was refused without a verification run (a compile error, not a disproof).
# No argument: the shipped file below is read from the repo root, which is how the CI proof precheck runs this script.
# Normally started by verification/call-position/run.sh, which copies call_position.rs and this script into a directory of their own and
# runs Verus on them.
set -uo pipefail
verus=${VERUS:-verus}
rules=${1:-src/js/runtime/bytecode/call_position.rs}
work=$(mktemp -d "${TMPDIR:-/tmp}/call-position-mutants.XXXXXX")
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
# The dot callee reports the property, anything else the paren, a name the callee start unless it starts the statement with arguments.
try "CalleeKind::Dot => CoordAt::Property," "CalleeKind::Dot => CoordAt::Paren,"
try "CalleeKind::Dot => CoordAt::Property," "CalleeKind::Dot => CoordAt::Callee,"
try "CalleeKind::Other => CoordAt::Paren," "CalleeKind::Other => CoordAt::Callee,"
try "CalleeKind::Other => CoordAt::Paren," "CalleeKind::Other => CoordAt::Property,"
try "if !has_args || !at_statement_start { CoordAt::Callee } else { CoordAt::Paren }" "if !has_args && !at_statement_start { CoordAt::Callee } else { CoordAt::Paren }"
try "if !has_args || !at_statement_start { CoordAt::Callee } else { CoordAt::Paren }" "if has_args || !at_statement_start { CoordAt::Callee } else { CoordAt::Paren }"
try "if !has_args || !at_statement_start { CoordAt::Callee } else { CoordAt::Paren }" "if !has_args || at_statement_start { CoordAt::Callee } else { CoordAt::Paren }"
try "if !has_args || !at_statement_start { CoordAt::Callee } else { CoordAt::Paren }" "CoordAt::Callee"
try "if !has_args || !at_statement_start { CoordAt::Callee } else { CoordAt::Paren }" "CoordAt::Paren"
# The paren search: it finds a paren at or after the start, stays in bounds, and only skips what may sit between.
try "        if c == b'(' {" "        if c == b')' {"
try "    let mut i = from;" "    let mut i = from + 1;"
try "    let mut i = from;" "    let mut i = 0;"
try "    while i < src.len()" "    while i <= src.len()"
try "            i += 1;" "            i += 2;"
try "            return Some(i);" "            return Some(i + 1);"
try "            return Some(i);" "            return Some(from);"
# The dot callee reports the property, anything else the paren, a name the callee start unless it starts the statement with arguments.
# The paren search: it finds a paren at or after the start, stays in bounds, and only skips what may sit between.
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
