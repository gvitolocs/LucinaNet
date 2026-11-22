#!/usr/bin/env bash
# Verus on the functions that ship, not on a model of them, as scripts/verify-shipped.sh does: a shipped
# file is plain Rust with its proof in `//@` lines, and what Verus reads is made by mechanical steps only, checked by
# undoing them:
#   `-> T //@ret`  becomes `-> (ret: T)`;   `... //@omit` is dropped;   `//@ proof` becomes `proof`.
# Run from the repository root: raster/verify.sh (needs Verus: put it on PATH or set VERUS to its binary).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
verus=${VERUS:-verus}
command -v "$verus" >/dev/null 2>&1 || { echo "error: Verus not found; put verus on PATH or set VERUS=/path/to/verus" >&2; exit 1; }
read -r -a files <<< "${FILES:-raster/rules/src/lib.rs raster/rules/src/blit.rs raster/rules/src/span.rs}"
work=$(mktemp -d "${TMPDIR:-/tmp}/raster-verify.XXXXXX")
trap 'rm -rf "$work"' EXIT
status=0
for f in "${files[@]}"; do
    out=$work/$(basename "$f")
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
    # shellcheck disable=SC2086
    if "$verus" ${VERUS_ARGS:-} "$out"; then echo "ok $f"; else echo "FAIL $f"; status=1; fi
done
exit "$status"
