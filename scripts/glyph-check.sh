#!/usr/bin/env bash
# How many characters LucinaNet cannot draw: it renders tests/fonts/glyphs.html (one grid cell per sample
# code point, one row per Unicode block) and tests/fonts/glyphs-notdef.html (the same grid with every sample
# replaced by U+FFFF, which no font assigns), and compares each cell with its own reference cell. A cell
# that came out pixel-identical to the reference was drawn by the fallback face's .notdef: a box.
# tests/fonts/notdef-count.py explains the two signals it reports and why.
#
# What is under test is the binary's own defaults: this script sets no LUCINANET_FONT_DIRS, so a Linux build
# is tested with the directories it would use itself and a Mac build with CoreText. Set it yourself to test a
# different list:  LUCINANET_FONT_DIRS=/usr/share/fonts:/snap/.../share/fonts scripts/glyph-check.sh <bin>
#
# Usage: scripts/glyph-check.sh <binary> [out-dir] [--verbose]
#        default out-dir $WORK_DIR/lucinanet-glyph-check (set WORK_DIR to a scratch directory)
set -uo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
bin=${1:?usage: scripts/glyph-check.sh <binary> [out-dir] [--verbose]}
shift
out=${WORK_DIR:-$(mktemp -d)}/lucinanet-glyph-check
if [ -n "${1:-}" ] && [ "${1:0:1}" != "-" ]; then
  out=$1
  shift
fi
[ -x "$bin" ] || { echo "glyph-check: $bin is not an executable" >&2; exit 2; }
mkdir -p "$out/png" "$out/logs" "$out/profile" || exit 1

size=$(python3 -c 'import json,sys; print("%dx%d" % tuple(json.load(open(sys.argv[1]))["size"]))' "$root/tests/fonts/glyphs.json") || exit 1
echo "glyph-check: $bin at $size, fonts from the binary's own defaults, out $out"

run() {  # `timeout` is GNU; a Mac without coreutils runs the binary directly
  if command -v timeout >/dev/null 2>&1; then timeout "${TIMEOUT:-300}" "$@"; else "$@"; fi
}

export LUCINANET_RENDER_QUIET_MS="${LUCINANET_RENDER_QUIET_MS:-2500}"
export LUCINANET_PROFILE_DIR="$out/profile"

for page in glyphs glyphs-notdef; do
  run "$bin" --render "file://$root/tests/fonts/$page.html" "$out/png/$page.png" "$size" \
      > "$out/logs/$page.log" 2>&1
  rc=$?
  [ -f "$out/png/$page.png" ] || { echo "glyph-check: $page.html did not render (rc=$rc), see $out/logs/$page.log" >&2; exit 1; }
  echo "glyph-check: $page.png rc=$rc $(grep -h '^render:' "$out/logs/$page.log" | tail -1)"
done

python3 "$root/tests/fonts/notdef-count.py" "$out/png/glyphs.png" "$out/png/glyphs-notdef.png" "$root/tests/fonts/glyphs.json" "$@"
rc=$?
echo "glyph-check: exit $rc (0 is no boxes); the pages and the two renderings are under $out"
exit $rc
