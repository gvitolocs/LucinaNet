#!/usr/bin/env bash
# Shows that src/js/runtime/gc/sizing.rs is what keeps Verus on the heap sizing policy: each `try` plants one bug in a copy of the
# shipped file, turned into what Verus reads by the mechanical steps below, and Verus must refuse every mutant.
#   verification/gc-sizing/mutants.sh <path to sizing.rs>          (needs `verus` on PATH, or VERUS=...)
# MUTANTS_STRICT=1: exit 1 if any mutant was refused without a verification run (a compile error, not a disproof).
# No argument: the shipped file below is read from the repo root, which is how the CI proof precheck runs this script.
# Normally started by verification/gc-sizing/run.sh, which copies sizing.rs and this script into a directory of their own and
# runs Verus on them.
set -uo pipefail
verus=${VERUS:-verus}
rules=${1:-src/js/runtime/gc/sizing.rs}
work=$(mktemp -d "${TMPDIR:-/tmp}/gc-sizing-mutants.XXXXXX")
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
# The saturating helpers: a sum and a difference that cannot wrap.
try "if a > usize::MAX - b {" "if false {"
try "if a >= b { a - b } else { 0 }" "if a >= b { a - b } else { b - a }"
# The heap limits and the factors (the interpolation, the two ends, the named constants).
try "pub const SMALL_HEAP: usize = 100 << 20;" "pub const SMALL_HEAP: usize = 101 << 20;"
try "pub const LARGE_HEAP: usize = 500 << 20;" "pub const LARGE_HEAP: usize = 400 << 20;"
try "pub const MAX_FACTOR_TENTHS: usize = 20;" "pub const MAX_FACTOR_TENTHS: usize = 5;"
try "pub const MIN_FACTOR_TENTHS: usize = 15;" "pub const MIN_FACTOR_TENTHS: usize = 8;"
try "pub const MIN_FACTOR_TENTHS: usize = 15;" "pub const MIN_FACTOR_TENTHS: usize = 31;"
try "pub const MAX_FACTOR_TENTHS: usize = 20;" "pub const MAX_FACTOR_TENTHS: usize = 2000;"
try "if live <= SMALL_HEAP {" "if live <= SMALL_HEAP << 1 {"
try "} else if live >= LARGE_HEAP {" "} else if live >= LARGE_HEAP >> 1 {"
try $'        MAX_FACTOR_TENTHS\n    } else if' $'        MIN_FACTOR_TENTHS\n    } else if'
try $'        MIN_FACTOR_TENTHS\n    } else {' $'        MAX_FACTOR_TENTHS\n    } else {'
try "MAX_FACTOR_TENTHS - ((MAX_FACTOR_TENTHS - MIN_FACTOR_TENTHS) * (live - SMALL_HEAP))" "MAX_FACTOR_TENTHS - ((MAX_FACTOR_TENTHS - MIN_FACTOR_TENTHS) * (live - SMALL_HEAP) * 2)"
try "MAX_FACTOR_TENTHS - ((MAX_FACTOR_TENTHS - MIN_FACTOR_TENTHS) * (live - SMALL_HEAP))" "MAX_FACTOR_TENTHS - ((MAX_FACTOR_TENTHS - MIN_FACTOR_TENTHS) * (live - SMALL_HEAP) / 2)"
try "MAX_FACTOR_TENTHS - ((MAX_FACTOR_TENTHS - MIN_FACTOR_TENTHS) * (live - SMALL_HEAP))" "MIN_FACTOR_TENTHS + ((MAX_FACTOR_TENTHS - MIN_FACTOR_TENTHS) * (live - SMALL_HEAP))"
try "/ (LARGE_HEAP - SMALL_HEAP)" "/ (LARGE_HEAP - SMALL_HEAP + 1)"
try "/ (LARGE_HEAP - SMALL_HEAP)" "/ LARGE_HEAP"
# The minimum window, the grown live data, the window and the allocation limit.
try "sat_sub(min_heap, overhead) / 2" "sat_sub(min_heap, overhead) / 4"
try "sat_sub(min_heap, overhead) / 2" "min_heap / 2"
try "if live > usize::MAX / MAX_FACTOR_TENTHS { usize::MAX }" "if live > usize::MAX / MAX_FACTOR_TENTHS { 0 }"
try "if live > usize::MAX / MAX_FACTOR_TENTHS {" "if live > usize::MAX / (MAX_FACTOR_TENTHS + 1) {"
try "live * growth_factor_tenths(live) / 10 }" "live * growth_factor_tenths(live) / 20 }"
try "live * growth_factor_tenths(live) / 10 }" "live * MIN_FACTOR_TENTHS / 10 }"
try "if g > min_window { g } else { min_window }" "if g < min_window { g } else { min_window }"
try "if g > min_window { g } else { min_window }" "if g > min_window { g } else { 0 }"
try "let g = grown(live);" "let g = live;"
try "let limit = if w > need { w } else { need };" "let limit = if w < need { w } else { need };"
try "let limit = if w > need { w } else { need };" "let limit = w;"
try "if limit > semispace { semispace } else { limit }" "if limit < semispace { semispace } else { limit }"
try "if limit > semispace { semispace } else { limit }" "limit"
try "let need = sat_add(live, alloc);" "let need = live;"
try "let w = if high_frequency { window(live, min_window) } else { low_frequency_window(live, min_window) };" "let w = if high_frequency { low_frequency_window(live, min_window) } else { window(live, min_window) };"
try "let w = if high_frequency { window(live, min_window) } else { low_frequency_window(live, min_window) };" "let w = window(live, min_window);"
try "let w = if high_frequency { window(live, min_window) } else { low_frequency_window(live, min_window) };" "let w = low_frequency_window(live, min_window);"
try "let w = if high_frequency { window(live, min_window) } else { low_frequency_window(live, min_window) };" "let w = if high_frequency { grown(live) } else { low_frequency_window(live, min_window) };"
# The low-frequency window: the factor, the saturation, the floor, the multiplication and the division.
try "pub const LOW_FACTOR_TENTHS: usize = 15;" "pub const LOW_FACTOR_TENTHS: usize = 9;"
try "pub const LOW_FACTOR_TENTHS: usize = 15;" "pub const LOW_FACTOR_TENTHS: usize = 16;"
try "pub const LOW_FACTOR_TENTHS: usize = 15;" "pub const LOW_FACTOR_TENTHS: usize = 2000;"
try "let g = if live > usize::MAX / LOW_FACTOR_TENTHS { usize::MAX } else { live * LOW_FACTOR_TENTHS / 10 };" "let g = if live > usize::MAX / LOW_FACTOR_TENTHS { 0 } else { live * LOW_FACTOR_TENTHS / 10 };"
try "let g = if live > usize::MAX / LOW_FACTOR_TENTHS { usize::MAX } else { live * LOW_FACTOR_TENTHS / 10 };" "let g = if live > usize::MAX / (LOW_FACTOR_TENTHS - 1) { usize::MAX } else { live * LOW_FACTOR_TENTHS / 10 };"
try "let g = if live > usize::MAX / LOW_FACTOR_TENTHS { usize::MAX } else { live * LOW_FACTOR_TENTHS / 10 };" "let g = if live > usize::MAX / (LOW_FACTOR_TENTHS + 1) { usize::MAX } else { live * LOW_FACTOR_TENTHS / 10 };"
try "let g = if live > usize::MAX / LOW_FACTOR_TENTHS { usize::MAX } else { live * LOW_FACTOR_TENTHS / 10 };" "let g = if live > usize::MAX / LOW_FACTOR_TENTHS { usize::MAX } else { live * LOW_FACTOR_TENTHS / 20 };"
try "let g = if live > usize::MAX / LOW_FACTOR_TENTHS { usize::MAX } else { live * LOW_FACTOR_TENTHS / 10 };" "let g = if live > usize::MAX / LOW_FACTOR_TENTHS { usize::MAX } else { live * LOW_FACTOR_TENTHS };"
try "let g = if live > usize::MAX / LOW_FACTOR_TENTHS { usize::MAX } else { live * LOW_FACTOR_TENTHS / 10 };" "let g = if live > usize::MAX / LOW_FACTOR_TENTHS { usize::MAX } else { live * growth_factor_tenths(live) / 10 };"
try "let g = if live > usize::MAX / LOW_FACTOR_TENTHS { usize::MAX } else { live * LOW_FACTOR_TENTHS / 10 };" "let g = if live > usize::MAX / LOW_FACTOR_TENTHS { usize::MAX } else { live + LOW_FACTOR_TENTHS / 10 };"
try $'live * LOW_FACTOR_TENTHS / 10 };\n    if g > min_window { g } else { min_window }' $'live * LOW_FACTOR_TENTHS / 10 };\n    if g < min_window { g } else { min_window }'
try $'live * LOW_FACTOR_TENTHS / 10 };\n    if g > min_window { g } else { min_window }' $'live * LOW_FACTOR_TENTHS / 10 };\n    if g > min_window { g } else { 0 }'
try $'live * LOW_FACTOR_TENTHS / 10 };\n    if g > min_window { g } else { min_window }' $'live * LOW_FACTOR_TENTHS / 10 };\n    g'
try "let need = sat_add(live, alloc);" "let need = alloc;"
# The room test: the grown live data plus the allocation against the semispace.
try "sat_add(grown(live), alloc) > semispace" "sat_add(grown(live), alloc) >= semispace"
try "sat_add(grown(live), alloc) > semispace" "sat_add(live, alloc) > semispace"
try "sat_add(grown(live), alloc) > semispace" "grown(live) > semispace"
# The size of a halved semispace.
try "sat_sub(heap_size / 2, overhead) / 2" "sat_sub(heap_size, overhead) / 2"
try "sat_sub(heap_size / 2, overhead) / 2" "(heap_size / 2) / 2"
# Growing before the collection: only below the maximum, and only once a collection has said what is live.
try "heap_size < max_heap && collected_before && needs_room(last_live" "collected_before && needs_room(last_live"
try "heap_size < max_heap && collected_before && needs_room(last_live" "heap_size < max_heap && needs_room(last_live"
try "heap_size < max_heap && collected_before && needs_room(last_live" "heap_size <= max_heap && collected_before && needs_room(last_live"
try "needs_room(last_live, alloc, semispace)
}" "needs_room(last_live, 0, semispace)
}"
# After the collection: grow when the allocation does not fit, shrink when the grown live data and the allocation fit the half heap.
try "let can_grow = heap_size < max_heap;" "let can_grow = heap_size <= max_heap;"
try "if can_grow && sat_add(live, alloc) > semispace {" "if can_grow && sat_add(live, alloc) >= semispace {"
try "if can_grow && sat_add(live, alloc) > semispace {" "if can_grow && sat_add(grown(live), alloc) > semispace {"
try "if can_grow && sat_add(live, alloc) > semispace {" "if can_grow && live > semispace {"
try "if can_grow && sat_add(live, alloc) > semispace {" "if sat_add(live, alloc) > semispace {"
try "} else if heap_size / 2 >= min_heap" "} else if heap_size >= min_heap"
try "} else if heap_size / 2 >= min_heap" "} else if heap_size / 2 > min_heap"
try "&& sat_add(grown(live), alloc) <= shrunk_semispace(heap_size, overhead)" "&& sat_add(live, alloc) <= shrunk_semispace(heap_size, overhead)"
try "&& sat_add(grown(live), alloc) <= shrunk_semispace(heap_size, overhead)" "&& sat_add(grown(live), alloc) < shrunk_semispace(heap_size, overhead)"
try "&& sat_add(grown(live), alloc) <= shrunk_semispace(heap_size, overhead)" "&& sat_add(grown(live), alloc) <= shrunk_semispace(heap_size, overhead) * 2"
try "&& sat_add(grown(live), alloc) <= shrunk_semispace(heap_size, overhead)" "&& sat_add(grown(live), 0) <= shrunk_semispace(heap_size, overhead)"
# The size of a growing heap: double, never past the maximum, a power of two big enough for a large allocation.
try "let doubled = if heap_size <= max_heap / 2 {" "let doubled = if heap_size <= max_heap {"
try "{ heap_size * 2 } else { max_heap }" "{ heap_size * 4 } else { max_heap }"
try "let twice = sat_add(a, a);" "let twice = a;"
try "if twice > heap_size {" "if twice >= heap_size {"
try "if twice > max_heap {" "if false {"
try "pow2_at_least(twice, max_heap)" "twice"
try "p = p * 2;" "p = p + 1;"
try "while p < n" "while p <= n"
# Room for the grown live data too: its size, the allocation, the overhead, the doubling of the need, the check and the cap.
try "let held = grown(live);" "let held = live;"
try "let held = grown(live);" "let held = grown(live) / 2;"
try "Some(a) => sat_add(held, a)," "Some(a) => a,"
try "Some(a) => sat_add(held, a)," "Some(a) => held,"
try "let target = sat_add(overhead, sat_add(need, need));" "let target = sat_add(need, need);"
try "let target = sat_add(overhead, sat_add(need, need));" "let target = sat_add(overhead, need);"
try "let target = sat_add(overhead, sat_add(need, need));" "let target = sat_sub(sat_add(overhead, sat_add(need, need)), 1);"
try "if target <= base {" "if true {"
try "} else if target > max_heap {" "} else if false {"
try "pow2_at_least(target, max_heap)" "base"
# The size of a shrinking heap: half of it, never below the minimum, and only when that is smaller.
try "let n = if heap_size / 2 >= min_heap { heap_size / 2 } else { min_heap };" "let n = heap_size / 2;"
try "if n < heap_size { Some(n) } else { None }" "if n <= heap_size { Some(n) } else { None }"
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
