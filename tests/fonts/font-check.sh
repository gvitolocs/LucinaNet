#!/bin/sh
# font-check.sh: how many characters of tests/fonts/cjk-emoji.html (Chinese, Japanese, Korean, emoji, plus Latin,
# Cyrillic, Greek, Arabic and Devanagari as controls) this machine's LucinaNet draws as tofu, from one headless
# render. 
#
# What a person in the room notices: nothing. No sound (nothing on the page plays; LUCINANET_MEDIA_SINK=none),
# no window and no focus change (--render is headless), about 10 seconds of one core, no network (a file: page,
# LUCINANET_PROFILE_DIR=none writes no profile).
#
# Usage: tests/fonts/font-check.sh [--bin PATH] [--tool PATH] [--out DIR] [--max-tofu N] [--system-fonts]
#   --bin      the engine binary (default $WORK_DIR/lucinanet/target/probe/lucinanet-servo, or $LUCINANET_BIN)
#   --tool     the built font-tofu-check (default $FONT_TOFU_CHECK, else tools/font-tofu-check/target/release/
#              font-tofu-check, built with cargo if that is missing and cargo is on PATH; build it
#              first and pass --tool)
#   --out      where the render, its log and the table go (default $WORK_DIR/lucinanet/font-test-page)
#   --max-tofu PASS when the non-control cells that drew as tofu are at most N, else FAIL (exit 1);
#              without it the run only reports and exits 0
#   --system-fonts  set LUCINANET_FONT_DIRS to the platform list (Linux /usr/share/fonts, macOS
#              /System/Library/Fonts:/Library/Fonts). Left alone by default: the engine sets its own platform list
#              (docs/fonts.md) and an exported LUCINANET_FONT_DIRS is used as given, as the WHOLE list.
# Exit: 0 reported (or PASS), 1 FAIL, 2 the run itself failed (no render, wrong size, blank tofu reference).
set -u
# Set WORK_DIR to a scratch directory holding the engine binary and the render output.
WORK_DIR=${WORK_DIR:-$(mktemp -d)}
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
bin=${LUCINANET_BIN:-$WORK_DIR/lucinanet/target/probe/lucinanet-servo}
tool=${FONT_TOFU_CHECK:-}
out=$WORK_DIR/lucinanet/font-test-page
max=""
system_fonts=0
while [ $# -gt 0 ]; do
    case $1 in
        --bin) bin=${2:?--bin needs a path}; shift 2 ;;
        --tool) tool=${2:?--tool needs a path}; shift 2 ;;
        --out) out=${2:?--out needs a directory}; shift 2 ;;
        --max-tofu) max=${2:?--max-tofu needs a number}; shift 2 ;;
        --system-fonts) system_fonts=1; shift ;;
        -h|--help) sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "font-check: unknown argument $1" >&2; exit 2 ;;
    esac
done
case $max in
    ""|*[!0-9]*) [ -z "$max" ] || { echo "font-check: --max-tofu $max is not a whole number" >&2; exit 2; } ;;
esac
page=$here/cjk-emoji.html
[ -x "$bin" ] || { echo "font-check: $bin is not an executable (--bin)" >&2; exit 2; }
[ -f "$page" ] || { echo "font-check: $page is missing" >&2; exit 2; }

if [ -z "$tool" ]; then
    tool=$root/tools/font-tofu-check/target/release/font-tofu-check
    if [ ! -x "$tool" ] && command -v cargo >/dev/null 2>&1; then
        echo "font-check: building tools/font-tofu-check"
        cargo build --release --locked --manifest-path "$root/tools/font-tofu-check/Cargo.toml" >&2 || exit 2
    fi
fi
[ -x "$tool" ] || { echo "font-check: $tool is not built (--tool)" >&2; exit 2; }

mkdir -p "$out" || exit 2
size=$("$tool" --page "$page" --size) || exit 2
png=$out/cjk-emoji.png
log=$out/render.log
rm -f "$png"

# Headless and silent: no profile written, the media mixer off, the console to the log.
export LUCINANET_PROFILE_DIR=none
export LUCINANET_MEDIA_SINK=none
export LUCINANET_TIMING=1
export LUCINANET_RENDER_QUIET_MS=${LUCINANET_RENDER_QUIET_MS:-2500}
if [ "$system_fonts" = 1 ]; then
    case $(uname -s) in
        Darwin) LUCINANET_FONT_DIRS=/System/Library/Fonts:/Library/Fonts ;;
        *) LUCINANET_FONT_DIRS=/usr/share/fonts ;;
    esac
    export LUCINANET_FONT_DIRS
fi
echo "font-check: $bin at $size, fonts: ${LUCINANET_FONT_DIRS:-the defaults of the binary}, out $out"

run() {  # `timeout` is GNU; a Mac without coreutils runs the binary directly
    if command -v timeout >/dev/null 2>&1; then timeout "${TIMEOUT:-120}" "$@"; else "$@"; fi
}
run "$bin" --render "file://$page" "$png" "$size" > "$log" 2>&1
rc=$?
[ -f "$png" ] || { echo "font-check: the page did not render (rc=$rc), see $log" >&2; exit 2; }

faces=$(grep 'font list' "$log" | tail -1)
echo "${faces:-phase font list: not reported by this binary}"

if [ -n "$max" ]; then
    "$tool" --page "$page" --png "$png" --max-tofu "$max" > "$out/table.txt" 2> "$out/tool.err"
else
    "$tool" --page "$page" --png "$png" > "$out/table.txt" 2> "$out/tool.err"
fi
trc=$?
cat "$out/table.txt"
cat "$out/tool.err" >&2
case $trc in
    0) [ -n "$max" ] && echo "font-check: PASS (tofu at most $max)" || echo "font-check: report only (no --max-tofu)" ;;
    1) echo "font-check: FAIL (more than $max cells drew as tofu)"; exit 1 ;;
    *) echo "font-check: the check could not judge this render (exit $trc)" >&2; exit 2 ;;
esac
echo "font-check: render, log and table are under $out"
exit 0
