#!/bin/bash
# Mac-build acceptance. Usage: mac-build.sh <worktree of the pinned main> <outdir>
# Steps: build; OOM pages (run early); local MSE variants (recovery 0/1) with the trace checked
# PER VARIANT log; consent answered through the redirect in a throwaway profile (only proceeds if "consent2: clicked");
# YouTube 120 s recovery 0 then 1 with the trace checked on the WHOLE log. Muted; profile none except the throwaway one.
# A trace step cannot pass vacuously: see check_trace in lib.sh and tests/acceptance/README.md.
# The binary it tests is a HARNESS build (cargo feature `harness`, tools/build-harness.sh): the doors it uses exist only there.
# Optional: RUN_MEDIA_TESTS=1 also runs `cargo test -p lucinanet-media` (default off: a separate job).
# PRECONDITION: never delete a cargo target by file age; delete a whole profile (a half-pruned target links stale rlibs).
set -u
# Set WORK_DIR to a scratch directory for temp files, build output and the probe scripts.
WORK_DIR=${WORK_DIR:-$(mktemp -d)}
W=${1:?worktree}; D=${2:?outdir}; mkdir -p "$D"
export TMPDIR=$WORK_DIR/tmp CARGO_TARGET_DIR=$WORK_DIR/lucinanet/target-fullrust-x86 CC=clang CXX=clang++ PYTHON3=python3
export LUCINANET_MEDIA_SINK=none
cd "$W" || exit 99
# shellcheck source=lib.sh
. "$W/tests/acceptance/lib.sh"

SUMMARY=()   # lines "PASS <label>" or "STEP-FAILED: <label>: <reason>"
FAILED=0
step_pass() { SUMMARY+=("PASS $1"); }
step_fail() { SUMMARY+=("STEP-FAILED: $1: $2"); FAILED=1; }
# trace_step <log> <label>: run check_trace with the standard required list and record the result
trace_step() {
  local log=$1 label=$2 out rc
  out=$(check_trace "$log" "$label" element-contract-1 MEDIA-STR clock-contract); rc=$?
  printf "%s\n" "$out"
  if [ $rc -eq 0 ]; then step_pass "$label"; else step_fail "$label" "$(printf "%s\n" "$out" | grep '^STEP-FAILED' | head -1 | sed "s/^STEP-FAILED: $label: //")"; fi
}
finish() {
  echo "=== SUMMARY"
  printf "%s\n" "${SUMMARY[@]}"
  [ "$FAILED" -eq 0 ] && echo "RESULT: all steps passed" || echo "RESULT: at least one step FAILED"
  echo "ACCEPT-DONE $(date -u +%T)"
  exit "$FAILED"
}

echo "HEAD $(git rev-parse HEAD) START $(date -u +%T)"
if [ "${RUN_MEDIA_TESTS:-0}" = "1" ]; then
  cargo test --offline -p lucinanet-media 2>&1 | grep -E "^test result|FAILED|panicked"; mrc=${PIPESTATUS[0]}
  echo "rc media-tests=$mrc"; [ "$mrc" -eq 0 ] && step_pass "media unit tests" || step_fail "media unit tests" "cargo test exit $mrc"
else
  echo "media unit tests: not run (separate job)"
fi
cargo build --offline --profile probe -p lucinanet-servo --features desktop,media,harness 2>&1 | grep -E "^error|Finished"; brc=${PIPESTATUS[0]}
echo "rc build=$brc"
if [ "$brc" -ne 0 ]; then step_fail "build" "cargo build exit $brc"; finish; fi
step_pass "build"
cp "$CARGO_TARGET_DIR/probe/lucinanet-servo" "$D/lucinanet-servo-accept" && sha256sum "$D/lucinanet-servo-accept" | cut -c1-16
TC=$(trace_checker "$W") || { step_fail "media-trace-check build" "cargo build of tools/media-trace-check failed"; finish; }
export TC
B=$D/lucinanet-servo-accept
# this script drives the browser through harness doors (--render, LUCINANET_USER_JS, LUCINANET_PREFS); refuse a binary without them.
require_harness_binary "$B" "harness binary" || { step_fail "harness binary" "not a harness build"; finish; }

echo "=== OOM pages (early) $(date -u +%T)"
for pg in oom-probe oom-microtask oom-raf oom-worker; do
  f=$W/tests/edge-wrap/$pg.html; [ -f "$f" ] || f=$WORK_DIR/lucinanet/edge-wrap/$pg.html; [ -f "$f" ] || continue
  LUCINANET_PROFILE_DIR=none timeout 170 "$B" --render "file://$f" "$D/$pg.png" 1280x720 "$D/$pg.dump" > "$D/$pg.log" 2>&1; echo "rc $pg=$?"
done
echo OOM-EARLY-DONE

for rec in 0 1; do
  echo "=== local variants recovery=$rec $(date -u +%T)"
  LUCINANET_PROFILE_DIR=none LUCINANET_MEDIA_RECOVERY=$rec LUCINANET_MEDIA_TRACE=1 KEEP=1 sh tests/media/mse-two-buffers.sh "$B" plain pause seek init2 play2 > "$D/local-rec$rec.log" 2>&1
  echo "rc local-rec$rec=$?"; grep -E "MSE-TEST|variant" "$D/local-rec$rec.log" | tail -8
  art=$(sed -n 's/^artifacts in //p' "$D/local-rec$rec.log" | tail -1)
  if [ -z "$art" ] || [ ! -d "$art/out" ]; then
    for v in plain pause seek init2 play2; do step_fail "local-rec$rec-$v" "no 'artifacts in <dir>' line / out dir in local-rec$rec.log"; done
  else
    for v in plain pause seek init2 play2; do trace_step "$art/out/$v.log" "local-rec$rec-$v"; done
  fi
done

P=$D/profile-throwaway; rm -rf "$P"; mkdir -p "$P"
URL='https://www.youtube.com/watch?v=z3hMX65Khtg'
echo "=== consent through the redirect (probe3, hold 90 s) $(date -u +%T)"
LUCINANET_PROFILE_DIR=$P LUCINANET_CONSOLE=1 LUCINANET_USER_JS=$WORK_DIR/lucinanet/nav07/probe3.js LUCINANET_RENDER_HOLD=90 \
  timeout 180 "$B" --render "$URL" "$D/consent.png" 1280x900 "$D/consent.json" > "$D/consent.log" 2>&1
echo "rc consent=$?"; grep -E "consent2: clicked" "$D/consent.log" | cut -c1-80
if ! grep -q "consent2: clicked" "$D/consent.log"; then
  echo "CONSENT NOT ANSWERED: stop"; rm -rf "$P"
  step_fail "consent" "consent2: clicked not found; YouTube steps not run"; finish
fi
step_pass "consent"
for rec in 0 1; do
  echo "=== youtube 120 s recovery=$rec $(date -u +%T) load $(cut -d' ' -f1 /proc/loadavg)"
  LUCINANET_PROFILE_DIR=$P LUCINANET_MEDIA_RECOVERY=$rec LUCINANET_MEDIA_TRACE=1 LUCINANET_CONSOLE=1 LUCINANET_TIMING=1 \
    LUCINANET_PREFS=dom_web_animations_enabled=true LUCINANET_USER_JS=$WORK_DIR/lucinanet/nav07/probe3.js \
    LUCINANET_RENDER_HOLD=120 RUST_BACKTRACE=full \
    timeout 220 "$B" --render "$URL" "$D/yt-rec$rec.png" 1280x900 "$D/yt-rec$rec.json" > "$D/yt-rec$rec.log" 2>&1
  echo "rc yt-rec$rec=$?"
  echo "consent document fetches: $(grep -c 'netphase .* fetch https://consent.youtube.com/m' "$D/yt-rec$rec.log")  watchdog lines: $(grep -c 'clock stood still' "$D/yt-rec$rec.log")"
  grep -E "YTP r[0-9]+ state" "$D/yt-rec$rec.log" | tail -1 | cut -c1-170
  grep -E "panicked at|SIGSEGV|Segmentation" "$D/yt-rec$rec.log" | head -3 | cut -c1-160
  trace_step "$D/yt-rec$rec.log" "yt-rec$rec"
done
rm -rf "$P"
finish
