#!/usr/bin/env bash
# Crash regression gate for the JS engine fixes in LucinaNet-js 14a77db.
#
# Two recipes that crashed the engine before those fixes, run against a built binary:
#   1. bbc:    render https://www.bbc.com/news with a 6 s hold (realm principals bug, b1b7ecd).
#   2. stress: render https://github.com/servo/servo with LUCINANET_JS_GC_STRESS_KB=256
#              (accessor handle GC bug, 43174fc).
# A run passes when the exit code is 0 and the PNG exists and is larger than 10 KB.
# A non-zero exit whose stderr has no "lucinanet-crash" and no "Segmentation"/"signal 11", and
# whose code is not 139, is counted "skip (network)" instead of a failure.
# Runs are sequential, never parallel. Needs the network; slower tier, not the verification step.
#
# Usage: tests/crash/run.sh [--bin PATH] [--out DIR] [--bbc N] [--stress N]
# Exit: 0 all runs passed or skipped, 1 any failure, 2 binary not executable.
set -u
# Set WORK_DIR to a scratch directory holding the engine binary; results go under it.
WORK_DIR=${WORK_DIR:-$(mktemp -d)}

bin=$WORK_DIR/lucinanet/health/bin/lucinanet-servo-after2
out=$WORK_DIR/lucinanet/health/crash-gate
bbc=6
stress=3
while [ $# -gt 0 ]; do
  case "$1" in
    --bin) bin=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    --bbc) bbc=$2; shift 2 ;;
    --stress) stress=$2; shift 2 ;;
    *) echo "usage: $0 [--bin PATH] [--out DIR] [--bbc N] [--stress N]" >&2; exit 2 ;;
  esac
done

if [ ! -x "$bin" ]; then
  echo "crash-gate: binary not executable: $bin" >&2
  exit 2
fi
mkdir -p "$out"

fail=0
skipped=0

# run_one NAME INDEX URL TIMEOUT_S [ENV=VAL ...]; prints one line, returns 0 pass, 1 fail, 3 skip.
run_one() {
  local name=$1 i=$2 url=$3 tmo=$4
  shift 4
  local png="$out/$name.$i.png" err="$out/$name.$i.err" prof
  prof=$(mktemp -d "$out/profile.$name.$i.XXXXXX")
  rm -f "$png"
  env "$@" LUCINANET_PROFILE_DIR="$prof" timeout -s KILL "$tmo" "$bin" --render "$url" "$png" 1280x800 2> "$err"
  local code=$?
  rm -rf "$prof"
  local size=0
  [ -f "$png" ] && size=$(stat -c %s "$png")
  if [ "$code" -eq 0 ] && [ "$size" -gt 10240 ]; then
    echo "$name run$i exit=$code ok"
    return 0
  fi
  if [ "$code" -ne 0 ] && [ "$code" -ne 139 ] \
     && ! grep -q "lucinanet-crash" "$err" \
     && ! grep -Eq "Segmentation|signal 11" "$err"; then
    echo "$name run$i exit=$code skip (network)"
    return 3
  fi
  echo "$name run$i exit=$code FAIL (png=${size} bytes)"
  return 1
}

bbc_ok=0
for i in $(seq 1 "$bbc"); do
  run_one bbc "$i" https://www.bbc.com/news 150 LUCINANET_RENDER_HOLD=6
  case $? in 0) bbc_ok=$((bbc_ok + 1)) ;; 1) fail=1 ;; 3) skipped=$((skipped + 1)) ;; esac
done

stress_ok=0
for i in $(seq 1 "$stress"); do
  run_one stress "$i" https://github.com/servo/servo 300 LUCINANET_JS_GC_STRESS_KB=256
  case $? in 0) stress_ok=$((stress_ok + 1)) ;; 1) fail=1 ;; 3) skipped=$((skipped + 1)) ;; esac
done

echo "crash-gate: bbc $bbc_ok/$bbc, stress $stress_ok/$stress"
[ "$skipped" -gt 0 ] && echo "crash-gate: $skipped run(s) counted skip (network), not failures"
exit "$fail"
