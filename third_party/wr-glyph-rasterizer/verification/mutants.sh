#!/usr/bin/env bash
# Shows that verification/glyph_key.rs can fail: each try plants one bug in a copy of the shipped
# src/glyph_key_bits.rs, and Verus has to refuse it. Run from the package root (the script cds there itself);
# it calls Verus directly, one verification at a time. Needs Verus: put it on PATH or set VERUS to its binary.
set -uo pipefail
cd "$(dirname "$0")/.."
verus=${VERUS:-verus}
command -v "$verus" >/dev/null 2>&1 || { echo "error: Verus not found; put verus on PATH or set VERUS=/path/to/verus" >&2; exit 1; }
work=$(mktemp -d "${TMPDIR:-/tmp}/glyphkey-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
fail=0
try() {
    local src=$1 harness=$2 from=$3 to=$4
    rm -rf "$work/src" "$work/verification"
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
    if "$verus" "$work/verification/$harness" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        echo "refused: $from -> $to ($(grep -c '^error' "$work/out") errors)"
    fi
}
k() { try src/glyph_key_bits.rs glyph_key.rs "$@"; }

# pack: overlapping fields, swapped fields, a dropped field.
k '| (sox as u32) << 26 | (soy as u32) << 28 |' '| (sox as u32) << 25 | (soy as u32) << 28 |'
k '| (soy as u32) << 28 | (dir as u32) << 30' '| (soy as u32) << 27 | (dir as u32) << 30'
k '    index | (sox as u32) << 26 | (soy as u32) << 28 | (dir as u32) << 30' '    index | (soy as u32) << 26 | (sox as u32) << 28 | (dir as u32) << 30'
k '| (dir as u32) << 30
}' '
}'
# unpacking: wrong mask or shift.
k '    k & 0x03FF_FFFF
}' '    k & 0x07FF_FFFF
}'
k 'let x = (k >> 26) as u8;' 'let x = (k >> 27) as u8;'
k '    y & 3
}' '    y & 7
}'
k 'let d = (k >> 30) as u8;' 'let d = (k >> 29) as u8;'
exit $fail
