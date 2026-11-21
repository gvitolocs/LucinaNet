#!/bin/sh
# LucinaNet's CSS-rule object gate (blocker spec 07, B15): at-rules that stylo produces but the
# CSSOM has no interface for must still come out of cssRules as an object, and must survive a
# mutation of their sheet. One process per page, because a Rust panic takes the whole tab with it.
#
# A page spec is name:switches, and the switches are the LUCINANET_CSS_PREFS value for that page
# ("none" runs with nothing on). Each page ends with "css-test: PASS <name>" and prints one table
# row: exit code, png bytes, switches, and the last css-test line.
#
#   register-four            the four at-rules B15 names, alone in a sheet (they are dropped by the
#                            parser at this stylo pin, so this page is the evidence for that)
#   at-rule-objects          one sheet of at-rules read by index, the register's recipe
#   insert-rule              each at-rule through insertRule: keep it honestly, or SyntaxError
#   sheet-mutate             read a rule, then rewrite the sheet that holds it (CSSRule::update_rule)
#   position-try-mutate      the same for an at-rule with no interface of its own
#
#   tests/css/at-rule-objects.sh [lucinanet binary] [pages...]
# KEEP=1 keeps the per-page logs and PNGs even when everything passes.
# Exit: 0 all pages passed, 1 any failure, 2 binary not executable.
set -u
bin=${1:-${WORK_DIR:-$(mktemp -d)}/lucinanet/target-fullrust-x86/probe/lucinanet-servo}
[ $# -gt 0 ] && shift
here=$(cd "$(dirname "$0")" && pwd)
pages=${*:-register-four:none at-rule-objects:+anchor-positioning insert-rule:+anchor-positioning sheet-mutate:none position-try-mutate:+anchor-positioning}
[ -x "$bin" ] || { echo "not executable: $bin" >&2; exit 2; }
out=${TMPDIR:-/tmp}/lucinanet-css-pages.$$
mkdir -p "$out"
port=${PORT:-8126}
(cd "$here/../.." && exec python3 -m http.server --bind 127.0.0.1 "$port") > "$out/http.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null' EXIT
sleep 1
fail=0
printf '%-22s %-6s %-7s %-20s %s\n' page exit png switches readout
for spec in $pages; do
    case $spec in *:*) p=${spec%%:*}; prefs=${spec#*:} ;; *) p=$spec; prefs=none ;; esac
    if [ "$prefs" = none ]; then unset LUCINANET_CSS_PREFS; else LUCINANET_CSS_PREFS=$prefs; fi
    export LUCINANET_CSS_PREFS
    LUCINANET_CONSOLE=1 LUCINANET_PROFILE_DIR=none \
        timeout 120 "$bin" --render "http://127.0.0.1:$port/tests/css/$p.html" "$out/$p-$prefs.png" \
        > "$out/$p.log" 2>&1
    code=$?
    png=$(stat -c %s "$out/$p-$prefs.png" 2>/dev/null || echo 0)
    # stylo prints "panicked at <file>:<line>:<col>:" and the reason on the next line.
    panic=$(grep -A1 "panicked at" "$out/$p.log" | tr '\n' ' ' | sed 's|.*/components/|components/|' | cut -c1-160)
    printf '%-22s %-6s %-7s %-20s %s\n' "$spec" "$code" "$png" "$prefs" \
        "$(grep -o 'css-test: .*' "$out/$p.log" | tail -1)"
    if [ -n "$panic" ]; then echo "    panic: $panic"; fail=1; fi
    grep -q "css-test: PASS ${p}\$" "$out/$p.log" || { echo "    ! no PASS line (exit $code)"; fail=1; }
done
if [ "$fail" = 0 ] && [ "${KEEP:-0}" != 1 ]; then
    rm -rf "$out"
else
    echo "    logs in $out"
fi
exit "$fail"
