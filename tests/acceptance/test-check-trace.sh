#!/bin/bash
# Logic test of check_trace (lib.sh) on small synthetic logs; no engine run. Usage: test-check-trace.sh [repo-root]
set -u
R=${1:-$(cd "$(dirname "$0")/../.." && pwd)}
. "$R/tests/acceptance/lib.sh"
export TMPDIR=${TMPDIR:-${WORK_DIR:-$(mktemp -d)}/tmp}; mkdir -p "$TMPDIR"
T=$(mktemp -d "$TMPDIR/ckt.XXXXXX"); trap 'rm -rf "$T"' EXIT INT TERM
TC=$(trace_checker "$R") || { echo "cannot build media-trace-check"; exit 2; }; export TC
F=$R/tools/media-trace-check/tests/fixtures
REQ="element-contract-1 MEDIA-STR clock-contract"
fails=0
expect() { # expect <pass|fail> <name> <log> [grep-pattern for the failure line]
  local want=$1 name=$2 log=$3 pat=${4:-} out rc
  out=$(check_trace "$log" "$name" $REQ); rc=$?
  printf "%s\n" "$out" | sed 's/^/    /'
  if [ "$want" = pass ] && [ $rc -eq 0 ]; then echo "OK   $name: passed as expected"
  elif [ "$want" = fail ] && [ $rc -ne 0 ] && printf "%s\n" "$out" | grep -q "^STEP-FAILED: $name: .*$pat"; then echo "OK   $name: failed as expected ($pat)"
  else echo "BAD  $name: wanted $want, rc=$rc"; fails=$((fails+1)); fi
}
# 1: a 13-line log with no trace lines -> "properties checked: (none)"
for i in $(seq 1 13); do echo "[t+ 0.$i] some unrelated line $i"; done > "$T/thirteen.log"
expect fail thirteen-lines "$T/thirteen.log" "(none)"
# 2: an empty log
: > "$T/empty.log"
expect fail empty "$T/empty.log" ""
# 3: a violation log
expect fail violation "$F/planted/element-contract-1.log" "checker exit 1"
# 4: a missing log
expect fail missing "$T/does-not-exist.log" "log missing"
# 5: the healthy fixture has only $(wc -l < healthy-all.log) lines (< 200): it must fail as "too short"
n=$(wc -l < "$F/healthy-all.log"); echo "NOTE healthy-all.log has $n lines (min $TRACE_MIN_LINES)"
if [ "$n" -lt "$TRACE_MIN_LINES" ]; then expect fail healthy-fixture-short "$F/healthy-all.log" "too short"; else expect pass healthy-fixture "$F/healthy-all.log"; fi
# 6: a healthy log of >= 200 lines built from the fixture's first 8 lines + 230 clock lines + the final summary line
{ head -n 8 "$F/healthy-all.log"
  i=0; while [ $i -lt 230 ]; do i=$((i+1)); ms=$((1000+i*10)); p=$((ms*1000))
    printf "[t+%4d.%03d] lucinanet-media: show pts=%d now=Some(%d) sound=Some(%d)\n" $((ms/1000)) $((ms%1000)) $p $((p+1500)) $((p+1500)); done
  tail -n 1 "$F/healthy-all.log"; } > "$T/healthy-long.log"
echo "NOTE healthy-long.log has $(wc -l < "$T/healthy-long.log") lines"
expect pass healthy-long "$T/healthy-long.log"
# 7: the same long log but requiring a property that is not in the checked list
grep -v "media elem" "$T/healthy-long.log" > "$T/no-elem.log"
expect fail required-missing "$T/no-elem.log" "element-contract-1"
echo "failures: $fails"; [ $fails -eq 0 ] && echo TEST-CHECK-TRACE-PASS || echo TEST-CHECK-TRACE-FAIL
exit $fails
