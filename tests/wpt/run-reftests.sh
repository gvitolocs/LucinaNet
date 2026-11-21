#!/bin/bash
# WPT CSS/SVG/fonts/masking against the real Rust renderer, reftests included.
#
# `mach test-wpt --bin <lucinanet-servo>` cannot work: wptrunner's "servo" product launches the binary
# with --webdriver=PORT and this binary has no WebDriver server. This takes the path tests/media/wpt.sh
# proved -- a local `wpt serve`, one `lucinanet-servo --render` per test -- and hands the scoring to
# tools/wpt-reftest, which adds reftest references and WPT fuzzy tolerances from MANIFEST.json.
# tests/media/wpt.sh is left alone: it is the media baseline.
#
#   tests/wpt/run-reftests.sh --include css/css-masking/ --limit 8
#   tests/wpt/run-reftests.sh --include css/css-masking/ --include css/css-fonts/ --include svg/
#
# Everything after the wrapper's own env vars is passed straight to the tool; run it with --help for
# the full flag list. Run it capped on the build host:
#   systemd-run --user --scope -p CPUQuota=200% tests/wpt/run-reftests.sh --include css/css-masking/
# The tool also pauses while k10temp Tctl >= MAX_C (default 90), like wpt.sh's cool().
#
# Only localhost: the build host has no web-platform.test host entries, so cross-origin subtests fail here and
# must not be counted as engine bugs. http only, no https/h2/ws ports, check_subdomains off.
set -u
# Set WORK_DIR to a scratch directory for the results, the tool build and the fonts.
WORK_DIR=${WORK_DIR:-$(mktemp -d)}

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
bin=${BIN:-target/probe/lucinanet-servo}
servo=${SERVO:-/home/user/Projects/Lucina/LucinaNet-servo}
out=${OUT:-$WORK_DIR/lucinanet/health/wpt-reftest}
port=${PORT:-8123}
target=${CARGO_TARGET_DIR:-$WORK_DIR/lucinanet/health/target-wpt}
tool=${TOOL:-$target/release/wpt-reftest}
manifest=${MANIFEST:-$servo/tests/wpt/meta/MANIFEST.json}
fonts=${FONT_DIRS:-$WORK_DIR/lucinanet/fonts}

[ -x "$bin" ] || { echo "run-reftests: $bin is not executable (set BIN)" >&2; exit 3; }
[ -f "$manifest" ] || { echo "run-reftests: $manifest not found (set SERVO or MANIFEST)" >&2; exit 3; }
[ -d "$servo/tests/wpt/tests" ] || { echo "run-reftests: $servo/tests/wpt/tests not found" >&2; exit 3; }
mkdir -p "$out" || exit 3

# Build the scorer in a scratch target dir, never in the worktree, and never with more than 3 jobs.
if [ ! -x "$tool" ] || [ "${BUILD:-0}" = 1 ]; then
    echo "run-reftests: building $tool" >&2
    (
        cd "$root/tools/wpt-reftest" || exit 3
        export CARGO_TARGET_DIR="$target" CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-3}"
        cargo build --release --offline || cargo build --release
    ) || { echo "run-reftests: build failed" >&2; exit 3; }
fi

# the build host's owner limit: background CPU work waits while the CPU is at or above 90 C (k10temp Tctl).
cool() {
    local h
    for h in /sys/class/hwmon/hwmon*; do
        [ "$(cat "$h/name" 2>/dev/null)" = k10temp ] || continue
        while [ $(( $(cat "$h/temp1_input") / 1000 )) -ge ${MAX_C:-90} ]; do
            echo "run-reftests: Tctl >= ${MAX_C:-90} C, waiting" >&2
            sleep 20
        done
    done
}
cool

# Same config as tests/media/wpt.sh: http on one port, no subdomains, no TLS.
cat > "$out/config.json" <<EOF
{"browser_host": "localhost", "alternate_hosts": {}, "ports": {"http": [$port, "auto"], "https": [], "ws": [], "wss": [], "h2": [], "webtransport-h3": []}, "check_subdomains": false}
EOF

serve=""
cleanup() {
    if [ -n "$serve" ]; then
        kill -TERM -- "-$serve" 2>/dev/null
        kill -TERM "$serve" 2>/dev/null
    fi
}
trap cleanup EXIT INT TERM

if curl -s -o /dev/null --max-time 2 "http://localhost:$port/"; then
    echo "run-reftests: reusing the WPT server already on :$port" >&2
else
    cd "$servo/tests/wpt/tests" || exit 3
    # setsid so the whole python tree can be killed as one process group on the way out.
    setsid ./wpt serve --config "$out/config.json" > "$out/serve.log" 2>&1 &
    serve=$!
    cd "$root" || exit 3
    ok=0
    for _ in $(seq 1 60); do
        if curl -s -o /dev/null --max-time 2 "http://localhost:$port/"; then ok=1; break; fi
        sleep 1
    done
    if [ "$ok" != 1 ]; then
        echo "run-reftests: wpt serve did not come up on :$port; see $out/serve.log" >&2
        tail -20 "$out/serve.log" >&2
        exit 3
    fi
fi

LUCINANET_FONT_DIRS="$fonts" "$tool" \
    --manifest "$manifest" \
    --bin "$bin" \
    --out "$out" \
    --base-url "http://localhost:$port" \
    --font-dirs "$fonts" \
    --max-c "${MAX_C:-90}" \
    "$@"
status=$?
echo "run-reftests: exit $status; results in $out/results.jsonl and $out/summary.json" >&2
exit $status