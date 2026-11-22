#!/usr/bin/env bash
# prove that run.sh --record writes correct PASS / FAIL / ERROR lines, and that a failed case
# can NEVER yield a PASS line. No network, no binary, no Xvfb: it drives run.sh's --self-test mode, which
# exercises the record / finish / abort logic with stubbed state (a fake observer, a fake binary hash).
#
#   bash tests/zero-network/test-record.sh          # exit 0 = every assertion held
set -u

d=$(cd "$(dirname "$0")" && pwd)
RUNSH="$d/run.sh"
TMPROOT=${TMPDIR:?set TMPDIR to a scratch directory (not /tmp)}
mkdir -p "$TMPROOT"
WORK=$(mktemp -d "$TMPROOT/lucinanet-testrecord.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

fails=0
ok()  { printf 'ok   %s\n' "$1"; }
bad() { printf 'FAIL %s\n' "$1"; fails=$((fails + 1)); }

status_of() { printf '%s' "$1" | awk '{print $2}'; }   # 2nd whitespace field = PASS|FAIL|ERROR
has()       { printf '%s' "$1" | grep -qE "$2"; }

REC="$WORK/record.log"
out=$(bash "$RUNSH" --self-test --record "$REC" 2>&1); rc=$?

[ "$rc" = 0 ] && ok "self-test exits 0" || bad "self-test exit $rc (want 0): $out"
[ -f "$REC" ] && ok "record file created" || bad "record file missing at $REC"

n=$(wc -l <"$REC" 2>/dev/null || echo 0)
[ "$n" = 5 ] && ok "five lines recorded (PASS, 3 failures, ERROR)" || bad "expected 5 record lines, got $n"

l1=$(sed -n '1p' "$REC"); l2=$(sed -n '2p' "$REC"); l3=$(sed -n '3p' "$REC"); l4=$(sed -n '4p' "$REC"); l5=$(sed -n '5p' "$REC")

# scenario 1: every case passed and the positive control landed -> PASS
[ "$(status_of "$l1")" = PASS ] && ok "clean run records PASS" || bad "clean run should be PASS, got: $l1"

# the PASS line carries the same facts as the RESULT line, plus time / commit / binary
for pat in \
  '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z PASS ' \
  'commit=' 'binary=' 'cases=fresh' 'violations=0' 'controls=1/1' 'observer=strace' 'seconds=[0-9]+'; do
  has "$l1" "$pat" && ok "PASS line matches /$pat/" || bad "PASS line missing /$pat/: $l1"
done

# scenarios 2-4: a failed case, a blind control and a violation must never be recorded as PASS
i=1
for l in "$l2" "$l3" "$l4"; do
  i=$((i + 1))
  st=$(status_of "$l")
  [ "$st" != PASS ] && ok "failure scenario $i is not PASS" || bad "failure scenario $i recorded PASS: $l"
  [ "$st" = FAIL ]  && ok "failure scenario $i is FAIL"     || bad "failure scenario $i should be FAIL, got $st: $l"
done

# scenario 5: an aborted run (a missing prerequisite) -> ERROR with a reason, never PASS
st=$(status_of "$l5")
[ "$st" = ERROR ] && ok "aborted run records ERROR" || bad "aborted run should be ERROR, got $st: $l5"
has "$l5" 'reason=' && ok "ERROR line carries the reason" || bad "ERROR line missing reason: $l5"

# exactly one PASS across all five scenarios (three failures + one abort can never be PASS)
npass=$(awk '$2=="PASS"{c++} END{print c+0}' "$REC")
[ "$npass" = 1 ] && ok "exactly one PASS across the five scenarios" || bad "expected 1 PASS, got $npass"

# the env hook: LUCINANET_ZERO_NET_RECORD with no --record argument still writes the line
REC2="$WORK/record-env.log"
out=$(LUCINANET_ZERO_NET_RECORD="$REC2" bash "$RUNSH" --self-test 2>&1); rc=$?
if [ -f "$REC2" ] && [ "$(status_of "$(sed -n '1p' "$REC2" 2>/dev/null)")" = PASS ]; then
  ok "LUCINANET_ZERO_NET_RECORD env writes the PASS line"
else
  bad "env hook did not write a PASS line (rc $rc): $out"
fi

printf '\n%s\n' "$([ "$fails" = 0 ] && echo 'test-record: ALL PASS' || echo "test-record: $fails FAILURE(S)")"
[ "$fails" = 0 ] || exit 1
