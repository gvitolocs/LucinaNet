#!/bin/sh
# page-text-unobserved: the find bar's page-text request is invisible to the page.
#   tests/web/page-text-unobserved.sh [--bin PATH]   else $LUCINANET_BIN, else $WORK_DIR/lucinanet/target-fullrust-x86/probe/
#
# Loads tests/web/page-text-unobserved.html offscreen (`--render`, a HARNESS build: tools/build-harness.sh) with a LUCINANET_LOADS
# line `pagetext`, which sends the same command as Ctrl+F and prints `render: pagetext blocks=N runs=M partial=B` (counts only).
# (console.log goes to the engine's STDOUT under LUCINANET_CONSOLE, not stderr: the TRAPS lines are read from .out.)
# The page traps textContent, nodeValue, innerText, child links, a MutationObserver on the document and the selection and focus
# events, and prints `TRAPS n` to the console. Two runs:
#   plain   the request ran (blocks >= 1) and the last TRAPS line is 0                -> the page observed nothing
#   probe   the same page, which reads textContent itself once: the last TRAPS >= 1   -> the traps do work (positive control)
# Exit 0 both hold, 1 either fails, 2 no harness binary. Portable sh. No network, no media, no profile.
set -u

bin=${LUCINANET_BIN:-}
while [ $# -gt 0 ]; do
    case $1 in
        --bin) bin=$2; shift 2 ;;
        *) echo "page-text-unobserved: unknown option $1" >&2; exit 2 ;;
    esac
done
if [ -z "$bin" ]; then
    bin=${WORK_DIR:?set WORK_DIR to a scratch directory (not /tmp)}/lucinanet/target-fullrust-x86/probe/lucinanet-servo
fi
[ -x "$bin" ] || { echo "page-text-unobserved: no executable at $bin (tools/build-harness.sh)" >&2; exit 2; }
"$bin" --version 2>/dev/null | grep -q LUCINANET-HARNESS-BUILD || { echo "page-text-unobserved: $bin is not a harness build" >&2; exit 2; }

here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d "${TMPDIR:?set TMPDIR to a scratch directory (not /tmp)}/lucinanet-page-text.XXXXXX") || exit 2
[ -n "${KEEP_WORK:-}" ] && echo "page-text-unobserved: keeping $work" >&2 || trap 'rm -rf "$work"' EXIT INT TERM
printf 'wait 600\npagetext\nwait 1200\n' > "$work/loads"

run() {
    env -u DISPLAY -u WAYLAND_DISPLAY \
        LUCINANET_MEDIA_SINK=none LUCINANET_PROFILE_DIR=none LUCINANET_CONSOLE=1 LUCINANET_LOADS="$work/loads" \
        "$bin" --render "file://$here/page-text-unobserved.html$2" "$work/$1.png" 800x600 > "$work/$1.out" 2> "$work/$1.err"
}
last_traps() { sed -n 's/.*TRAPS \([0-9][0-9]*\).*/\1/p' "$work/$1.out" | tail -1; }
blocks() { sed -n 's/.*render: pagetext blocks=\([0-9][0-9]*\).*/\1/p' "$work/$1.err" | tail -1; }

run plain ""
run probe "#probe"
fail=0
b=$(blocks plain); t=$(last_traps plain); p=$(last_traps probe)
echo "plain: blocks=${b:-none} traps=${t:-none}   probe: traps=${p:-none}"
[ -n "$b" ] && [ "$b" -ge 1 ] || { echo "FAIL: the page text request did not run (no blocks)"; fail=1; }
[ "${t:-x}" = 0 ] || { echo "FAIL: the page observed the request (TRAPS ${t:-none})"; fail=1; }
[ -n "$p" ] && [ "$p" -ge 1 ] || { echo "FAIL: the positive control did not trip a trap (TRAPS ${p:-none}); the traps prove nothing"; fail=1; }
[ "$fail" = 0 ] && echo "PASS: the page observed nothing"
exit "$fail"
