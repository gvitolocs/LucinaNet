#!/usr/bin/env bash
# Shows that Verus can fail on the connector rules: each line plants one bug in a copy of dns.rs or slots.rs, and Verus must refuse it.
# Run on the build host from the Servo checkout root, through the verification script: components/net_policy/mutants.sh
set -uo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/netpolicy-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
mkdir -p "$work/components/net_policy/src"
fail=0
refused=0
try() {
    local file=$1 from=$2 to=$3
    local shipped=$root/components/net_policy/src/$file
    local text; text=$(<"$shipped")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$work/components/net_policy/src/$file"
    if FILES=components/net_policy/src/$file ROOT_OVERRIDE=$work "$root/components/net_policy/verify.sh" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        refused=$((refused + 1))
        echo "refused: $from -> $to ($(grep -c "^error" "$work/out") errors)"
    fi
}
# Lifetimes: the failure and the success lifetime swapped, the end of life inclusive.
try dns.rs 'if failure {
        return NEGATIVE_TTL_MS;
    }
    return POSITIVE_TTL_MS;' 'if failure {
        return POSITIVE_TTL_MS;
    }
    return NEGATIVE_TTL_MS;'
try dns.rs 'return age_ms < ttl_ms(failure);' 'return age_ms <= ttl_ms(failure);'
# Single-flight: an ask while a lookup runs must attach, not start another; a fresh entry must hit; an old one must start.
try dns.rs 'Slot::InFlight => Action::Attach,' 'Slot::InFlight => Action::Start,'
try dns.rs 'Slot::Absent => Action::Start,' 'Slot::Absent => Action::Attach,'
try dns.rs '            if is_fresh(*age_ms, *failure) {
                Action::Hit
            } else {
                Action::Start
            }' '            if is_fresh(*age_ms, *failure) {
                Action::Start
            } else {
                Action::Hit
            }'
# Capacity: one entry too many, nothing evicted when full.
try dns.rs 'return len - cap + 1;' 'return len - cap;'
try dns.rs 'if len < cap {
        return 0;
    }' 'if len <= cap {
        return 0;
    }'
# Slots: the limit off by one, a taker not at the front, a release below zero, the queue moving the wrong way.
try slots.rs 'return live < max;' 'return live <= max;'
try slots.rs 'return position == 0 && live < max;' 'return live < max;'
try slots.rs 'return position == 0 && live < max;' 'return position <= 1 && live < max;'
try slots.rs 'return live + 1;' 'return live + 2;'
try slots.rs 'if live == 0 {
        return 0;
    }
    return live - 1;' 'if live == 0 {
        return 0;
    }
    return live;'
try slots.rs 'if position > leaver {
        return position - 1;
    }
    return position;' 'if position > leaver {
        return position;
    }
    return position;'
# (position >= leaver instead of > is equivalent: the function requires position != leaver.)
echo "refused $refused, failures $fail"
exit "$fail"
