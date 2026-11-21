#!/bin/sh
# selftest.sh: does the whole chain (sweep.sh, the engine's dump, sweep-score) say loaded / broken / walled on
# three local file: pages?
#
#   tests/sweep/selftest.sh <engine binary> <sweep-score binary> [work-dir]
#
# Pages (tests/sweep/fixtures): normal.html -> loaded, blank.html -> broken, wall.html ("Verify you are human") -> walled.
# What a person in the room notices: nothing. No network (file: pages), no sound (the sweep exports the mute switch),
# no window, about 20 s of one core. Exit 0 when all three verdicts are as expected, 1 otherwise, 2 for a bad setup.
# Portable sh (Mac bash 3.2, BSD tools). On macOS it needs SWEEP_ALLOW_MAC=1 like the sweep itself.
set -u
here=$(cd "$(dirname "$0")" && pwd)
bin=${1:-}; scorer=${2:-}
[ -x "$bin" ] && [ -x "$scorer" ] || { echo "selftest: usage: selftest.sh <engine binary> <sweep-score binary> [work-dir]" >&2; exit 2; }
work=${3:-${TMPDIR:-${WORK_DIR:-$(mktemp -d)}/tmp}/sweep-selftest.$$}
mkdir -p "$work" || exit 2
list=$work/list.tsv
TAB=$(printf '\t')
{
  printf 'T001%sselftest%sfile://%s/fixtures/normal.html%snormal\n' "$TAB" "$TAB" "$here" "$TAB"
  printf 'T002%sselftest%sfile://%s/fixtures/blank.html%sblank\n' "$TAB" "$TAB" "$here" "$TAB"
  printf 'T003%sselftest%sfile://%s/fixtures/wall.html%swall\n' "$TAB" "$TAB" "$here" "$TAB"
} > "$list"

SWEEP_HOST_GAP_S=0 SWEEP_PAGE_CAP_S=60 SWEEP_QUIET_MS=1500 SWEEP_SETTLE_MAX_MS=6000 \
  "$here/sweep.sh" "$bin" "$work/out" --list "$list" --workers 1 || { echo "selftest: the sweep failed" >&2; exit 2; }
"$scorer" score "$work/out" --list "$list" --walls "$here/walls.tsv" || exit 2

fail=0
check() { # id expected
  got=$(awk -F'\t' -v id="$1" '$1 == id { print $2 }' "$work/out/results.tsv")
  if [ "$got" = "$2" ]; then echo "selftest: ok   $1 is $got"; else echo "selftest: FAIL $1 is '$got', wanted $2"; fail=1; fi
}
check T001 loaded
check T002 broken
check T003 walled
if [ "$fail" = 0 ]; then echo "selftest: PASS"; rm -rf "$work"; else echo "selftest: FAIL (results in $work/out)"; fi
exit "$fail"
