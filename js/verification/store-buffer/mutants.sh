#!/usr/bin/env bash
# Shows that mozjs-sys/src/engine/store_buffer_rules.rs is what keeps Verus on the store buffer: each `try` plants one bug in a
# copy of the shipped file, and Verus must refuse every mutant.
#   verification/store-buffer/mutants.sh <boundary/value_bits.rs> <engine/store_buffer_rules.rs>   (needs `verus`, or VERUS=...)
# Normally started by verification/store-buffer/run.sh, which copies both files and this script into a directory of their own and
# runs them on the proof host (`verus-remote`), or on the build host (FALLBACK=1).
# What Verus reads is the crate scripts/verify-shipped.sh builds from mozjs-sys/src/boundary: `value_bits` as `boundary::value_bits`,
# so that the shipped `use crate::boundary::value_bits;` is the same line in both.
set -uo pipefail
verus=${VERUS:-verus}
value_bits=${1:?path of boundary/value_bits.rs}
rules=${2:?path of engine/store_buffer_rules.rs}
work=$(mktemp -d "${TMPDIR:-/tmp}/store-buffer-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT

# The mechanical steps of scripts/verify-shipped.sh on one file.
body_of() { sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$1"; }
code_of() { sed -E -e '/^ *\/\/@/d' -e '/\/\/@omit$/d' -e 's/ \/\/@ret$//' "$1"; }
undo() { printf '%s\n' "$1" | grep -vF '/*proof*/' | sed -E 's/-> \(ret: (.*)\)$/-> \1/'; }

# Undoing those steps must give the shipped code back, so that what Verus checks is the code that is compiled.
for f in "$value_bits" "$rules"; do
    if [ "$(code_of "$f")" != "$(undo "$(body_of "$f")")" ]; then
        echo "WHAT VERUS READS IS NOT THE SHIPPED CODE: $f"
        exit 1
    fi
done

# The crate: the same wrapping as scripts/verify-shipped.sh, plus the module of the file under test.
make_verus_input() {
    printf '#![allow(unused)]\nuse vstd::prelude::*;\nverus! { global size_of usize == 8; }\n'
    printf 'pub mod boundary {\npub mod value_bits {\nuse vstd::prelude::*;\nverus! {\n%s\n}\n}\n}\n' "$(body_of "$value_bits")"
    printf 'pub mod store_buffer_rules {\nuse vstd::prelude::*;\nverus! {\n%s\n}\n}\nfn main() {}\n' "$(body_of "$1")"
}

# The unchanged files must verify, or nothing below means anything.
make_verus_input "$rules" > "$work/base.rs"
if ! "$verus" "$work/base.rs" > "$work/base.out" 2>&1; then
    echo "BASE FAILS"
    cat "$work/base.out"
    exit 1
fi
echo "base: $(grep -E '^verification results' "$work/base.out")"

fail=0
count=0
try() {
    local from=$1 to=$2
    local text; text=$(<"$rules")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    local mut; mut=${text/"$from"/"$to"}
    if [ "$mut" = "$text" ]; then echo "NOT CHANGED: $from"; fail=1; return; fi
    printf '%s\n' "$mut" > "$work/mutant-src.rs"
    make_verus_input "$work/mutant-src.rs" > "$work/mutant.rs"
    count=$((count + 1))
    if "$verus" "$work/mutant.rs" > "$work/verus.out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"
        cat "$work/verus.out"
        fail=1
    else
        echo "refused: $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
    fi
}
# What makes a slot recorded: a cell slot when it is not null, a value slot when its bits are a heap pointer.
try "    content != 0" "    content == 0"
try "    content != 0" "    true"
try "value_bits::Kind::Pointer => true," "value_bits::Kind::Double => true,"
try "value_bits::Kind::Pointer => true," "value_bits::Kind::Pointer | value_bits::Kind::Int32 => true,"
try "            _ => false," "            _ => true,"
try "if is_value { c != 0 && value_bits::tag_of(c) == 0 } else { c != 0 }" "if is_value { value_bits::tag_of(c) == 0 } else { c != 0 }"
try "if is_value { c != 0 && value_bits::tag_of(c) == 0 } else { c != 0 }" "if is_value { c != 0 && value_bits::tag_of(c) == 0 } else { c > 1 }"
# What a fresh or dropped Heap holds, and what it does to the buffer.
try "if is_value { 0x8006u64 << 48u64 } else { 0 }" "if is_value { 1u64 } else { 0 }"
try "Buffer { mem: b.mem.insert(s, init_content(is_value)), rec: b.rec }" "Buffer { mem: b.mem.insert(s, init_content(is_value)), rec: b.rec.insert(s) }"
try "Op::Create(s) => if b.mem.dom().contains(s) { b } else { create(b, is_value, s) }," "Op::Create(s) => create(b, is_value, s),"
# The barrier: recorded if a pointer, removed if not.
try "if ptr_content(is_value, v) { b.rec.insert(s) } else { b.rec.remove(s) }" "if ptr_content(is_value, v) { b.rec.remove(s) } else { b.rec.insert(s) }"
try "else { b.rec.remove(s) } }" "else { b.rec } }"
try "{ b.rec.insert(s) } else" "{ b.rec } else"
# Drop: the slot leaves the set and the memory.
try "Buffer { mem: w.mem.remove(s), rec: w.rec }" "Buffer { mem: w.mem.remove(s), rec: b.rec }"
try "mem: w.mem.remove(s)," "mem: w.mem,"
# The GC: every recorded slot updated and only those, dead objects cleared, slots that lost their pointer dropped from the set.
try "b.rec.filter(|s: int| ptr_content(is_value, mem[s]))" "b.rec"
try "forwarded(f, is_value, b.mem[s]) } else { b.mem[s] }" "b.mem[s] } else { b.mem[s] }"
try "|s: int| if b.rec.contains(s) { forwarded" "|s: int| if true { forwarded"
try "if f.dom().contains(c) { f[c] } else { init_content(is_value) }" "if f.dom().contains(c) { f[c] } else { c }"
echo "$count mutants tried"
[ "$fail" = 0 ] && echo "all refused"
exit "$fail"
