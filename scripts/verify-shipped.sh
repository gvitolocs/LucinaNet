#!/usr/bin/env bash
# Verus on the functions that ship in our Servo patches, not on a model of them. As LucinaOS scripts/verify-shipped.sh: such a
# file is plain Rust with its proof in `//@` lines, and this makes what Verus reads by mechanical steps only, then checks that undoing
# them gives the shipped text back:
#   `-> T //@ret`   becomes `-> (ret: T)`
#   `... //@omit`   is dropped (a derive Verus does not check, and that holds no logic)
#   `//@ proof`     becomes `proof`
# SERVO is the Servo checkout with our patches (default ../LucinaNet-servo). Run on the build host through
# the verification script:
#   scripts/verify-shipped.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
servo=${SERVO:-$root/../LucinaNet-servo}
source "$root/scripts/verus-pin.sh"   # resolves `verus`, prints it, and refuses a version other than verification/verus.version
files=(${FILES:-components/profile_store/rules/bounds.rs components/profile_store/rules/evict.rs components/zstd/window.rs crates/lucinanet-media/src/rules/flow.rs crates/lucinanet-media/src/rules/clock.rs crates/lucinanet-servo/src/rules/pacing.rs crates/lucinanet-media/src/rules/buffered.rs crates/lucinanet-media/src/rules/caps.rs}) # crates/… is this repo, the rest the Servo checkout
work=$(mktemp -d "${TMPDIR:-/tmp}/lucinanet-verify.XXXXXX")
trap 'rm -rf "$work"' EXIT
status=0
for f in "${files[@]}"; do
    out=$work/$(basename "$f")
    src=$servo/$f
    case $f in crates/*) src=$root/$f ;; esac
    body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$src")
    printf 'use vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body" > "$out"
    a=$(sed -E -e '/^ *\/\/@/d' -e '/\/\/@omit$/d' -e 's/ \/\/@ret$//' "$src")
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
