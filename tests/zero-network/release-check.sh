#!/usr/bin/env bash
# release-check.sh: the nightly zero-network result is not optional. This checker is the gate the
# release steps run before (a) a public push and (b) a Mac install: a red or stale nightly blocks both.
#
#   release-check.sh <log> <commit> [max-age-hours=26]
#
#   <log>     the nightly record, one line per run, newest last (what `run.sh --record` writes; a
#             conventional place is target/nightly/zero-network.log in the checkout).
#   <commit>  the commit being shipped; matched against the newest line's commit= field, full sha or prefix.
#   max-age   how many hours a PASS stays valid; a nightly older than this is red (26 h = one nightly cycle + slack).
#
# Exit 0 only when ALL of: the newest line's status is PASS, its commit matches <commit>, and it is no older
# than max-age-hours. Anything else - a newest FAIL, a newest ERROR, a stale PASS, a wrong commit, an empty or
# missing log - exits non-zero and prints exactly one line saying why.
set -u

usage() { printf 'usage: release-check.sh <log> <commit> [max-age-hours=26]\n'; }

die() { printf 'release-check: RED %s\n' "$1"; exit 1; }

log=${1:-}
commit=${2:-}
maxage=${3:-26}

[ -n "$log" ] && [ -n "$commit" ] || { usage; exit 2; }
case "$maxage" in
  ''|*[!0-9]*) printf 'release-check: RED max-age-hours must be a non-negative integer, got "%s"\n' "$maxage" >&2; exit 2 ;;
esac
[ -e "$log" ] || die "no nightly log at $log (the zero-network nightly has not recorded here yet)"
[ -s "$log" ] || die "nightly log $log is empty (the zero-network nightly has not recorded a run)"

# the newest line is the last non-empty one (newest last by the record convention)
last=$(awk 'NF {l=$0} END {print l}' "$log")
[ -n "$last" ] || die "nightly log $log holds no record line"

ts=$(printf '%s' "$last" | awk '{print $1}')
status=$(printf '%s' "$last" | awk '{print $2}')
rec_commit=$(printf '%s' "$last" | sed -n 's/.* commit=\([^ ]*\).*/\1/p')

# 1 the newest run must be a PASS (FAIL/ERROR and any other verdict block)
if [ "$status" != PASS ]; then
  die "newest nightly line is $status, not PASS: $last"
fi

# 2 it must be a PASS of the commit we are shipping (full sha or prefix)
case "$rec_commit" in
  "$commit"*) : ;;                       # recorded commit starts with the given prefix (or is equal)
  *) die "newest PASS is for commit $rec_commit, not $commit" ;;
esac

# 3 and it must not be stale
now=$(date -u +%s)
then=$(date -u -d "$ts" +%s 2>/dev/null) || die "cannot read the timestamp on the newest line: $ts"
age=$(( now - then ))
if [ "$age" -lt 0 ]; then
  die "newest PASS is timestamped in the future ($ts): the clock or the log is wrong"
fi
if [ "$age" -gt $(( maxage * 3600 )) ]; then
  die "newest PASS is $(( age / 3600 ))h old, older than the ${maxage}h window ($ts)"
fi

printf 'release-check: GREEN newest PASS commit=%s age=%dh <= %dh\n' "$rec_commit" $(( age / 3600 )) "$maxage"
exit 0
