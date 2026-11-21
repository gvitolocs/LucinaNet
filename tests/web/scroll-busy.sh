#!/bin/sh
# scroll-busy: does scrolling wait for script? (and the regression test for the planned "a scroll the
# renderer applies at once when no blocking listener is on the path".)
#   tests/web/scroll-busy.sh [--bin PATH] [--steps N] [--delta PX] [--scale N]
#
# Scrolls tests/web/scroll-busy.html offscreen (`--render`, the scroll bench's wheel steps, LUCINANET_FRAME_LOG on)
# twice: the page as it is, whose script blocks the main thread for 500 ms once a second, and the same page with
# `#idle`, which blocks nothing. From each frame timeline it prints how many frames were presented, how long
# the longest gap between two frames was, and how much of the run was spent in gaps of 300 ms or more.
#
# The blocks are read from the gaps, not from the page's own clock: the page cannot write into the frame log, and
# a frame cannot be presented while the main thread is stuck only if scrolling waits for script. So:
#   busy run with gaps of about 500 ms, every second, idle run without  -> scrolling WAITS for script.
#   busy run without such gaps                                         -> scrolling continues during script.
# Frames inside a block are therefore 0 when it waits; "outside" are all the others. The verdict line says which.
#
# Portable sh and awk. No network, no media (LUCINANET_MEDIA_SINK=none). Exit 0 the run was made (the verdict is
# a measurement, printed, not a pass/fail), 1 a run produced no frame timeline, 2 binary not executable.
set -u

bin=${LUCINANET_BIN:-${WORK_DIR:-$(mktemp -d)}/lucinanet/target/probe/lucinanet-servo}
steps=240
delta=40
scale=1
while [ $# -gt 0 ]; do
    case $1 in
        --bin) bin=$2; shift 2 ;;
        --steps) steps=$2; shift 2 ;;
        --delta) delta=$2; shift 2 ;;
        --scale) scale=$2; shift 2 ;;
        *) echo "scroll-busy: unknown option $1" >&2; exit 2 ;;
    esac
done
[ -x "$bin" ] || { echo "scroll-busy: no executable at $bin" >&2; exit 2; }

here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/lucinanet-scroll-busy.XXXXXX") || exit 2
trap 'rm -rf "$work"' EXIT INT TERM
w=$((1280 * scale))
h=$((800 * scale))

# run NAME PAGEURL: the frame timeline lands in $work/NAME.log
run() {
    env -u DISPLAY -u WAYLAND_DISPLAY \
        LUCINANET_MEDIA_SINK=none LUCINANET_PROFILE_DIR=none LUCINANET_SCALE="$scale" \
        LUCINANET_RENDER_QUIET_MS=1500 LUCINANET_SCROLL_BENCH="${steps}x${delta}" \
        LUCINANET_FRAME_LOG="$work/$1.log" \
        "$bin" --render "$2" "$work/$1.png" "${w}x${h}" > "$work/$1.out" 2> "$work/$1.err"
}

# summary NAME: frames, run length, gaps >= 300 ms, longest gap, share of the run in gaps, frames per second outside them.
summary() {
    grep -v '^#' "$work/$1.log" | awk -v name="$1" '
        { t = ""; iv = ""
          for (i = 1; i <= NF; i++) {
              if (substr($i, 1, 2) == "t=") t = substr($i, 3) + 0
              if (substr($i, 1, 9) == "interval=") iv = substr($i, 10) + 0
          }
          if (t == "") next
          frames++
          if (frames == 1) first = t
          last = t
          if (iv != "" && iv >= 300) { gaps++; gaptime += iv }
          if (iv != "" && iv > longest) longest = iv
        }
        END {
          span = last - first
          outside = span - gaptime
          printf "%-5s frames %d  run %.0f ms  gaps>=300ms %d  longest gap %.0f ms  in gaps %.0f%%  frames/s outside gaps %.1f\n",
              name, frames, span, gaps, longest, (span > 0 ? 100 * gaptime / span : 0), (outside > 0 ? 1000 * frames / outside : 0)
          printf "%d %.0f %.0f\n", gaps, longest, (span > 0 ? 100 * gaptime / span : 0) > "/dev/stderr"
        }' 2> "$work/$1.sum"
}

run busy "file://$here/scroll-busy.html"
run idle "file://$here/scroll-busy.html#idle"
for n in busy idle; do
    if [ ! -s "$work/$n.log" ] || [ "$(grep -vc '^#' "$work/$n.log")" -lt 1 ]; then
        echo "scroll-busy: the $n run wrote no frame timeline; the last lines of its stderr:" >&2
        tail -20 "$work/$n.err" >&2
        exit 1
    fi
done
summary busy
summary idle
read -r bg bl bs < "$work/busy.sum"
read -r ig il is < "$work/idle.sum"
if [ "$bg" -ge 2 ] && [ "$bs" -ge 25 ] && [ "$ig" -le 1 ]; then
    echo "verdict: scrolling WAITS for script: $bg gaps (longest $bl ms, $bs% of the run) with the script running, $ig without it."
elif [ "$bg" -le 1 ]; then
    echo "verdict: scrolling CONTINUES while script runs: $bg gaps of 300 ms or more with the script running."
else
    echo "verdict: unclear: busy $bg gaps ($bs% of the run), idle $ig gaps ($is%). Run again with more --steps."
fi
exit 0
