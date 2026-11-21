#!/bin/bash
# Runs LucinaNet's Verus proofs (verification/*.rs) with the Verus LucinaOS pins. Run it through the
# verification script, one verification at a time. VERIFY_ONLY names one file, including a model.
#
# verification/servo-ax.rs proves our Servo fork's accessibility rules where the fork is checked out
# next to LucinaNet (../LucinaNet-servo). LUCINANET_SERVO points it at another checkout (a worktree).
set -u
root=$(cd "$(dirname "$0")/.." && pwd)
source "$root/scripts/verus-pin.sh"   # resolves `verus`, prints it, and refuses a version other than verification/verus.version
servo=${LUCINANET_SERVO:-$root/../LucinaNet-servo}
status=0
# By default every top-level proof; VERIFY_ONLY names exactly one file, including verification/<dir>/model.rs.
if [ -n "${VERIFY_ONLY:-}" ]; then
    proofs=("$root/$VERIFY_ONLY")
    [ -f "${proofs[0]}" ] || { echo "no proof named $VERIFY_ONLY"; exit 1; }
else
    # The top-level proofs and the models in their own folders (handover, registry), which this used to skip.
    proofs=("$root"/verification/*.rs "$root"/verification/*/model.rs)
fi
for proof in "${proofs[@]}"; do
    name=${proof#"$root"/}
    file=$proof
    if grep -q '\.\./\.\./LucinaNet-servo/' "$proof"; then
        # Point the #[path] at the chosen Servo checkout, in a copy next to the original.
        file=$(mktemp "$root/verification/verify_tmp_XXXXXX.rs")
        sed "s#\.\./\.\./LucinaNet-servo/#$servo/#" "$proof" > "$file"
    fi
    echo "== $name"
    if out=$("$verus" "$file" 2>&1); then
        echo "$out" | grep "verification results"
        echo "ok $name"
    else
        echo "$out" | tail -30
        echo "FAIL $name"
        status=1
    fi
    [ "$file" != "$proof" ] && rm -f "$file"
done
exit $status
