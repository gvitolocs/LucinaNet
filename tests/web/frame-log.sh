#!/bin/sh
# frame-log: the frame timeline (LUCINANET_FRAME_LOG) of an offscreen --render run of a local page.
#   tests/web/frame-log.sh [--bin PATH]
#
# Scrolls a local page 30 steps with the scroll bench, with the log on, and checks the file: at least one frame
# line, `n` counting 1, 2, 3 ..., `t` never going back, every line key=value words with no URL and no path
# separator in it. Then the switch's other two rules: a log path inside the profile directory is refused (no
# file, one warning), and with the variable empty nothing is written or said.
#
# Portable sh and awk, no GNU-only flags. No network, no media. Exit 0 pass, 1 fail, 2 binary not executable.
set -u

bin=${LUCINANET_BIN:-${WORK_DIR:-$(mktemp -d)}/lucinanet/target/probe/lucinanet-servo}
while [ $# -gt 0 ]; do
    case $1 in
        --bin) bin=$2; shift 2 ;;
        *) echo "frame-log: unknown option $1" >&2; exit 2 ;;
    esac
done
[ -x "$bin" ] || { echo "frame-log: no executable at $bin" >&2; exit 2; }

here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/lucinanet-frame-log.XXXXXX") || exit 2
trap 'rm -rf "$work"' EXIT INT TERM
fails=0
fail() { echo "FAIL: $*"; fails=$((fails + 1)); }

# run PROFILE NAME LOGPATH: one offscreen run of the idle control page; stdout and stderr stay in $work/NAME.*
run() {
    env -u DISPLAY -u WAYLAND_DISPLAY \
        LUCINANET_MEDIA_SINK=none LUCINANET_SCALE=1 LUCINANET_RENDER_QUIET_MS=800 \
        LUCINANET_PROFILE_DIR="$1" LUCINANET_SCROLL_BENCH=30x120 LUCINANET_FRAME_LOG="$3" \
        "$bin" --render "file://$here/scroll-busy.html#idle" "$work/$2.png" 1000x700 \
        > "$work/$2.out" 2> "$work/$2.err"
}

# 1. the log is on: one line per presented frame.
log=$work/frames.log
run none on "$log"
if [ ! -s "$log" ]; then
    echo "FAIL: no log file written"
    exit 1
fi
lines=$(grep -vc '^#' "$log")
[ "$lines" -ge 5 ] || fail "only $lines frame lines"
bad=$(grep -v '^#' "$log" | awk '
    { want++
      ok = 1
      for (i = 1; i <= NF; i++) if ($i !~ /^[a-z_]+=[-0-9a-z_.]+$/) ok = 0
      if (!ok) { print "not key=value: " $0; next }
      split($1, a, "="); split($2, b, "=")
      if (a[1] != "t" || b[1] != "n") { print "t and n must come first: " $0; next }
      if (a[2] + 0 < last_t) print "t went back: " $0
      if (b[2] + 0 != want) print "n out of count: " $0
      last_t = a[2] + 0 }')
[ -z "$bad" ] || fail "$bad"
if grep -v '^#' "$log" | grep -q '/'; then fail "a line holds a URL or a path separator"; fi
echo "frame-log: $lines frame lines, t monotonic, n counted"

# 2. a path inside the profile directory is refused.
mkdir -p "$work/profile"
run "$work/profile" refused "$work/profile/frames.log"
[ ! -e "$work/profile/frames.log" ] || fail "a log was written into the profile directory"
grep -q 'LUCINANET_FRAME_LOG refused' "$work/refused.err" || fail "no warning for the refused path"

# 3. empty is off: nothing is said and no profile file is touched by it.
run none off ""
if grep -qi "frame log" "$work/off.err"; then fail "the empty switch said something"; fi

if [ "$fails" = 0 ]; then
    echo "frame-log: PASS"
    exit 0
fi
echo "frame-log: $fails failure(s)"
exit 1
