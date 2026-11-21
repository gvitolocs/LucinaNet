#!/usr/bin/env bash
# Shows that Verus can fail on js/mozjs-sys/src/boundary: each line plants one bug in a copy of a shipped file, and
# js/scripts/verify-shipped.sh must refuse it. Run on the build host from the repo root, through the verification script:
#   js/scripts/boundary-mutants.sh
set -uo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
work=$(mktemp -d "${TMPDIR:-/tmp}/lucinanet-js-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
refused=0
try() {
    local file=mozjs-sys/src/boundary/$1 from=$2 to=$3 why=$4
    local text; text=$(<"$file")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND in $1: $from"; fail=1; return ;;
    esac
    printf '%s\n' "${text/"$from"/"$to"}" > "$work/$1"
    if MUTANT_FILE_FOR=$1 MUTANT_FILE=$work/$1 scripts/verify-shipped.sh > "$work/out" 2>&1; then
        echo "NOT REFUSED ($why): $1: $from -> $to"; fail=1
    else
        echo "refused ($why): $(grep -m1 -oE "postcondition not satisfied|precondition not satisfied|requires not satisfied|invariant not satisfied[a-z ]*|assertion failed|possible arithmetic underflow/overflow|error\[E[0-9]+\]|FAIL [a-z_]+.rs: what Verus" "$work/out")"; refused=$((refused + 1))
    fi
}
# value_bits.rs
try value_bits.rs 'pub const UNDEFINED_TAG: u64 = 0x8006;' 'pub const UNDEFINED_TAG: u64 = 0x8004;' 'undefined reads as null'
try value_bits.rs $'        CANONICAL_NAN\n    } else {' $'        !raw\n    } else {' 'a NaN payload chosen by a script kept, can look like a pointer'
try value_bits.rs '    double(p)' '    p' 'PrivateValue stored as a pointer, GC would follow a Rust pointer'
try value_bits.rs '    bits != 0 && tag(bits) == 0' '    tag(bits) == 0' 'an all-zero slot traced'
try value_bits.rs 'if lo < 0x8000_0000 { lo as i32 }' 'if lo <= 0x8000_0000 { lo as i32 }' 'int32 sign off by one'
try value_bits.rs '(BOOL_TAG << TAG_SHIFT) | p' '(INT32_TAG << TAG_SHIFT) | p' 'a boolean made as an int32'
# id_bits.rs
try id_bits.rs $'    if s[0] == 0x30 && len > 1 {\n        //@ assert(units8' $'    if s[0] == 0x30 && len > 2 {\n        //@ assert(units8' '"00" taken as index 0'
try id_bits.rs $'        if acc >= 0xFFFF_FFFF {\n            //@ proof {\n            //@     let whole = units8' $'        if acc > 0xFFFF_FFFF {\n            //@ proof {\n            //@     let whole = units8' '"4294967295" taken as an index'
try id_bits.rs $'        if c < 0x30 || c > 0x39 {\n            //@ assert(!is_digit(units16' $'        if c < 0x30 || c > 0x3A {\n            //@ assert(!is_digit(units16' '":" taken as a digit'
try id_bits.rs 'is_index(bits) && (bits & 0xFFFF_FFFF) < 0x8000_0000' 'is_index(bits) && (bits & 0xFFFF_FFFF) <= 0x8000_0000' 'index 2^31 handed to Servo as an int id'
# bounds.rs
try bounds.rs '    if length > room / element_size {' '    if length > room {' 'typed array view past its buffer'
try bounds.rs $'    if byte_offset > buffer_len {\n        return None;\n    }\n' '' 'view offset past the buffer underflows'
try bounds.rs 'if index < count { Some(index as usize) }' 'if index <= count { Some(index as usize) }' 'reserved slot one past the end'
try bounds.rs '    let n = if argc > nargs { argc } else { nargs };' '    let n = argc;' 'vp not padded to nargs'
try bounds.rs 'if (i as usize) < len - 2 {' 'if (i as usize) < len - 1 {' 'argument one past vp'
try bounds.rs 'slot_bytes > 0x1_0000_0000 - header as u64' 'slot_bytes > 0x1_0000_0000' 'object size overflow'
try bounds.rs 'if str_len < dest_len { str_len } else { dest_len }' 'if str_len > dest_len { str_len } else { dest_len }' 'string copy past dest'
# roots.rs
try roots.rs '        if len > self.capacity - self.top {' '        if len > self.capacity {' 'frame past the end of the fixed array'
try roots.rs $'                self.top = start;\n' '' 'pop leaves the GC walking a dead frame'
try roots.rs 'let end = if i + 1 < n { self.starts[i + 1] } else { self.top };' 'let end = self.top;' 'an older frame overlapping newer ones'
echo "$refused refused"
exit "$fail"
