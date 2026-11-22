#!/usr/bin/env bash
# Verus on the functions that ship, not on a model of them. The files of
# mozjs-sys/src/boundary are plain Rust with their proof in `//@` lines; this script makes what Verus reads by mechanical steps
# only, and checks that undoing them gives the shipped text back:
#   `-> T //@ret`   becomes `-> (ret: T)`, the one thing Rust's syntax has no comment form for
#   `... //@omit`   is dropped (a derive Verus does not check, and that holds no logic)
#   `//@ proof`     becomes `proof`
# All boundary files go into one crate, one module each, so `use super::x` between them works as in mozjs_sys.
# Run through the verification script, one verification at a time: scripts/verify-shipped.sh [file.rs ...]   (MUTANT_FILE=... for mutants.sh)
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
verus=${VERUS:-verus}
command -v "$verus" >/dev/null 2>&1 || { echo "no Verus to run: '$verus' is not an executable here (put verus on PATH, or set VERUS)" >&2; exit 1; }
dir=$root/mozjs-sys/src/boundary
mods=(value_bits id_bits bounds roots)
work=$(mktemp -d "${TMPDIR:-/tmp}/lucinanet-js-verify.XXXXXX")
trap 'rm -rf "$work"' EXIT
out=$work/boundary.rs
# mozjs_sys is 64-bit only (pointers live in NaN-boxed values; lib.rs asserts usize::BITS == 64).
printf '#![allow(unused)]\nuse vstd::prelude::*;\nverus! { global size_of usize == 8; }\n' > "$out"
status=0
for m in "${mods[@]}"; do
    src=$dir/$m.rs
    # mutants.sh substitutes one file.
    if [ -n "${MUTANT_FILE:-}" ] && [ "$(basename "${MUTANT_FILE_FOR:-}")" = "$m.rs" ]; then src=$MUTANT_FILE; fi
    [ -f "$src" ] || continue
    body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$src")
    a=$(sed -E -e '/^ *\/\/@/d' -e '/\/\/@omit$/d' -e 's/ \/\/@ret$//' "$src")
    b=$(printf '%s\n' "$body" | grep -vF '/*proof*/' | sed -E 's/-> \(ret: (.*)\)$/-> \1/')
    if [ "$a" != "$b" ]; then
        echo "FAIL $m.rs: what Verus reads is not the shipped code"
        diff <(echo "$a") <(echo "$b") | head -20
        status=1
    fi
    printf 'pub mod %s {\nuse vstd::prelude::*;\nverus! {\n%s\n}\n}\n' "$m" "$body" >> "$out"
done
printf 'fn main() {}\n' >> "$out"
[ "$status" = 0 ] || exit 1
if "$verus" "$out"; then echo "ok boundary (${mods[*]})"; else echo "FAIL boundary"; exit 1; fi
