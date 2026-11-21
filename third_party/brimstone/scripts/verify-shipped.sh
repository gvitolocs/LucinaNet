#!/usr/bin/env bash
# Verus on the functions that ship, not on a model of them. Such a file is plain Rust with its proof in `//@` lines; this script
# makes what Verus reads by mechanical steps only, and checks that undoing them gives the shipped text back:
#   `-> T //@ret`   becomes `-> (ret: T)`, the one thing Rust's syntax has no comment form for
#   `... //@omit`   is dropped (a derive Verus does not check, and that holds no logic)
#   `//@ proof`     becomes `proof`
# The same steps each verification/<unit>/mutants.sh applies to its copy. Run it through the verification script: scripts/verify-shipped.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
verus=${VERUS:-verus}
files=(src/js/runtime/bytecode/fncache/disk_rules.rs src/js/runtime/bytecode/fncache/partition.rs src/js/runtime/bytecode/fncache/patch_rules.rs src/js/runtime/bytecode/fncache/prov_rules.rs src/js/runtime/bytecode/fncache/wire.rs src/js/runtime/bytecode/call_position.rs src/js/runtime/gc/accounting.rs src/js/runtime/gc/sizing.rs src/js/parser/reparse_rules.rs)
# VERIFY_SHIPPED_ONLY=<file>: only that file (one unit of the CI matrix, tools/ci/proof-inventory.tsv). It must be in the list.
if [ -n "${VERIFY_SHIPPED_ONLY:-}" ]; then
    found=0; for f in "${files[@]}"; do [ "$f" = "$VERIFY_SHIPPED_ONLY" ] && found=1; done
    [ "$found" = 1 ] || { echo "verify-shipped: $VERIFY_SHIPPED_ONLY is not in the list" >&2; exit 2; }
    files=("$VERIFY_SHIPPED_ONLY")
fi
work=$(mktemp -d "${TMPDIR:-/tmp}/verify-shipped.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
status=0
for f in "${files[@]}"; do
    out=$work/$(basename "$f")
    # The proof lines stay tagged, so that taking them out again can be compared with the shipped text.
    body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$root/$f")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body" > "$out"
    a=$(sed -E -e '/^ *\/\/@/d' -e '/\/\/@omit$/d' -e 's/ \/\/@ret$//' "$root/$f")
    b=$(printf '%s\n' "$body" | grep -vF '/*proof*/' | sed -E 's/-> \(ret: (.*)\)$/-> \1/')
    if [ "$a" != "$b" ]; then
        echo "FAIL $f: what Verus reads is not the shipped code"
        diff <(echo "$a") <(echo "$b") | head -20
        status=1
        continue
    fi
    if "$verus" "$out"; then echo "ok $f"; else echo "FAIL $f"; status=1; fi
done
exit "$status"
