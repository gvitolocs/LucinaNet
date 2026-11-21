#!/usr/bin/env bash
# Regression gate for "a page must never be able to panic the engine" in the CSP check.
#
# Each opaque-parent page puts a document with an *opaque* origin between the top level and a
# frame that is served with its own Content-Security-Policy. The engine walks that frame's
# ancestors to build the `frame-ancestors` source list, and used to serialise each ancestor
# origin to a string and parse it back with an expect() -- an opaque origin serialises to
# "null", which is not a URL, so https://www.amazon.com/ killed a script thread.
#
# Expectations:
#   MODE=verify (default) pass = no panic line anywhere and the CSP child script ran.
#   MODE=repro            pass = the opaque-parent cases panic; use it to show the bug on a
#                         pre-fix binary.
# `control-same-origin.html` has no opaque origin and must render in both modes.
#
# Two pages - opaque-parent-srcdoc and opaque-parent-csp-sandbox - are kind `opaque-cspblock` and
# carry an explicit exception in verify mode. Their parent is opaque through *sandboxing*, and the
# sandbox flags and the resulting opaque origin are inherited by the nested browsing context, so
# child-csp.html runs with an opaque document origin and its own `script-src 'self'` refuses
# child.js:
#
#   Fetching classic script failed Blocked by Content-Security-Policy (...) /child.js
#
# That is a KNOWN OPEN engine difference, not the Amazon panic: CSP3 matches 'self' against the URL
# origin of the protected resource, so Chrome and Firefox load child.js here. The engine matches it
# against the document origin instead, which never equals a URL origin once the origin is opaque, so
# no http-hosted script can ever pass a 'self' check inside a sandboxed frame. Fixing it belongs to
# the CSP source-matching code, not to this gate. The page still counts as good if there is no panic
# *and* that block is visible in the log: a panic fails, and so does a child script that neither ran
# nor was blocked by CSP, which keeps a blank or differently-broken page from passing. If a future
# engine makes 'self' match here, the child runs and the page passes on the normal path.
#
# Usage: tests/web/csp-origin/run.sh [binary]   MODE=repro|verify PORT=8137 HOLD=10 LOGS=DIR
# Exit: 0 as expected for the mode, 2 bad invocation.
set -u
here=$(cd "$(dirname "$0")" && pwd)
bin=${1:-${LUCINANET_BIN:-target/probe/lucinanet-servo}}
mode=${MODE:-verify}
port=${PORT:-8137}          # any free port; 8097-8099 belong to other harnesses
out=$(mktemp -d "${TMPDIR:-/tmp}/csp-origin.$$XXXXXX")
mkdir -p "$out/profile"
[ -x "$bin" ] || { echo "run.sh: no binary at $bin" >&2; exit 2; }
case "$mode" in repro|verify) ;; *) echo "run.sh: MODE must be repro or verify" >&2; exit 2 ;; esac

python3 "$here/serve.py" "$port" "$here" > "$out/http.log" 2>&1 9>&- &
server=$!
trap 'kill "$server" 2>/dev/null; rm -rf "$out"' EXIT
sleep 1

base="http://127.0.0.1:$port"
fail=0
run() { # run NAME KIND URL    KIND = opaque | opaque-cspblock | control
    local name=$1 kind=$2 url=$3 panic png loaded blocked want_panic=0
    case "$kind" in opaque|opaque-cspblock) [ "$mode" = repro ] && want_panic=1 ;; esac
    LUCINANET_CONSOLE=1 LUCINANET_RENDER_HOLD=${HOLD:-10} LUCINANET_PROFILE_DIR="$out/profile" \
        timeout 90 "$bin" --render "$url" "$out/$name.png" 1280x720 > "$out/$name.log" 2>&1
    panic=$(grep -c "Must always be able to parse document origin" "$out/$name.log")
    png=$(stat -c %s "$out/$name.png" 2>/dev/null || echo 0)
    loaded=$(grep -c "csp-origin: child loaded" "$out/$name.log")
    blocked=$(grep -cE 'Fetching classic script failed Blocked by Content-Security-Policy.*/child\.js' \
        "$out/$name.log")
    if [ "$panic" -gt 0 ]; then
        echo "$name: PANIC x$panic"
        [ "$want_panic" = 1 ] || fail=1
    elif [ "$kind" = opaque-cspblock ] && [ "$want_panic" = 0 ] \
            && [ "$loaded" = 0 ] && [ "$blocked" -gt 0 ]; then
        echo "$name: no panic; child script blocked by its own CSP ('self' vs opaque document" \
            "origin): KNOWN OPEN engine difference (png $png)"
    elif [ "$loaded" = 0 ]; then
        echo "$name: no panic, but the CSP child script neither ran nor was blocked by CSP" \
            "(png $png) [FAIL]"
        fail=1
    else
        echo "$name: no panic, child ran (png $png)"
        [ "$want_panic" = 0 ] || fail=1
    fi
    [ -n "${LOGS:-}" ] && cp "$out/$name.log" "$LOGS/$name.log" 2>/dev/null
}
run opaque-parent-srcdoc      opaque-cspblock "$base/opaque-parent-srcdoc.html"
run opaque-parent-data        opaque "$base/opaque-parent-data.html"
run opaque-parent-csp-sandbox opaque-cspblock "$base/opaque-parent-csp-sandbox.html"
run top-level-data-iframe     opaque "data:text/html,<iframe src='$base/child-csp.html'></iframe>"
run control-same-origin       control "$base/control-same-origin.html"

echo "csp-origin: mode=$mode bin=$bin exit=$fail"
exit "$fail"
