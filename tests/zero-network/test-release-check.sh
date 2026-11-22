#!/usr/bin/env bash
# prove release-check.sh. It exits 0 only when the NEWEST line of the nightly log is PASS,
# for the given commit (full sha or prefix), and no older than max-age-hours (default 26). Every other
# situation - a newest FAIL, a newest ERROR, a stale PASS, a wrong commit, an empty or missing log - is
# red and prints exactly one line saying why. No network, no binary: it only reads constructed logs.
#
#   bash tests/zero-network/test-release-check.sh        # exit 0 = every assertion held
set -u

d=$(cd "$(dirname "$0")" && pwd)
CHECK="$d/release-check.sh"
TMPROOT=${TMPDIR:?set TMPDIR to a scratch directory (not /tmp)}
mkdir -p "$TMPROOT"
WORK=$(mktemp -d "$TMPROOT/lucinanet-testrelcheck.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

fails=0
ok()  { printf 'ok   %s\n' "$1"; }
bad() { printf 'FAIL %s\n' "$1"; fails=$((fails + 1)); }

FULL=01806108933cc050d5aa309dd5b5d6cc7e0e2f33
OTHER=deadbeefdeadbeefdeadbeefdeadbeefdeadbeef

now()  { date -u +%Y-%m-%dT%H:%M:%SZ; }
ago()  { date -u -d "$1" +%Y-%m-%dT%H:%M:%SZ; }
# a record line in run.sh's own format (the checker only reads ts, status and commit, but stay honest)
line() { printf '%s %s commit=%s binary=aaaa1111bbbb2222 cases=fresh,seeded violations=%s controls=%s observer=strace seconds=138\n' \
          "$1" "$2" "$3" "${4:-0}" "${5:-2/2}"; }

# 1 green: newest line PASS, full commit, fresh (2 h old, window 26 h)
L="$WORK/green.log"
line "$(ago '2 hours ago')" PASS "$FULL" >"$L"
out=$(bash "$CHECK" "$L" "$FULL" 2>&1); rc=$?
[ "$rc" = 0 ] && ok "green: PASS newest, full commit, fresh" || bad "green must exit 0 (got $rc): $out"

# 2 green via commit prefix
out=$(bash "$CHECK" "$L" 0180610 2>&1); rc=$?
[ "$rc" = 0 ] && ok "green: commit prefix accepted" || bad "prefix must exit 0 (got $rc): $out"

# 3 red: newest line is FAIL even though an older PASS exists
L="$WORK/red.log"
{ line "$(ago '3 hours ago')" PASS "$FULL"; line "$(now)" FAIL "$FULL" 1 1/2; } >"$L"
out=$(bash "$CHECK" "$L" "$FULL" 2>&1); rc=$?
[ "$rc" != 0 ] && ok "red: newest FAIL (an older PASS does not count)" || bad "newest FAIL must be red: $out"

# 4 red: stale PASS (30 h old) against the 26 h default window; green when the window is widened to 48 h
L="$WORK/stale.log"
line "$(ago '30 hours ago')" PASS "$FULL" >"$L"
out=$(bash "$CHECK" "$L" "$FULL" 2>&1); rc=$?
[ "$rc" != 0 ] && ok "red: newest PASS older than max-age (default 26 h)" || bad "stale must be red: $out"
out=$(bash "$CHECK" "$L" "$FULL" 48 2>&1); rc=$?
[ "$rc" = 0 ] && ok "green: same line when max-age raised to 48 h" || bad "max-age 48 must be green (got $rc): $out"

# 5 red: wrong commit (the newest PASS is not for the commit we are shipping)
out=$(bash "$CHECK" "$L" "$OTHER" 2>&1); rc=$?
[ "$rc" != 0 ] && ok "red: commit does not match the newest PASS" || bad "wrong commit must be red: $out"

# 6 red: newest line ERROR (an aborted nightly run)
L="$WORK/err.log"
{ line "$(ago '1 hour ago')" PASS "$FULL"; } >"$L"
printf '%s ERROR commit=%s binary=none cases=none reason=build missing\n' "$(now)" "$FULL" >>"$L"
out=$(bash "$CHECK" "$L" "$FULL" 2>&1); rc=$?
[ "$rc" != 0 ] && ok "red: newest ERROR blocks" || bad "ERROR must be red: $out"

# 7 red: empty log
L="$WORK/empty.log"; : >"$L"
out=$(bash "$CHECK" "$L" "$FULL" 2>&1); rc=$?
[ "$rc" != 0 ] && ok "red: empty log" || bad "empty log must be red: $out"

# 8 red: missing log
out=$(bash "$CHECK" "$WORK/does-not-exist.log" "$FULL" 2>&1); rc=$?
[ "$rc" != 0 ] && ok "red: missing log" || bad "missing log must be red: $out"

# 9 the newest line wins: our commit is the last line, another commit is older -> green
L="$WORK/order.log"
{ line "$(ago '1 hours ago')" PASS "$OTHER"; line "$(now)" PASS "$FULL"; } >"$L"
out=$(bash "$CHECK" "$L" "$FULL" 2>&1); rc=$?
[ "$rc" = 0 ] && ok "green: newest line is our commit (older line ignored)" || bad "newest line must win (got $rc): $out"

# a failure prints exactly one line saying why
out=$(bash "$CHECK" "$WORK/order.log" "$OTHER" 2>&1)
lines=$(printf '%s\n' "$out" | grep -c .)
[ "$lines" = 1 ] && ok "a red verdict is exactly one line" || bad "expected one output line, got $lines: $out"

printf '\n%s\n' "$([ "$fails" = 0 ] && echo 'test-release-check: ALL PASS' || echo "test-release-check: $fails FAILURE(S)")"
[ "$fails" = 0 ] || exit 1
