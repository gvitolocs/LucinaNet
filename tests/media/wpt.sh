#!/bin/bash
# Servo's WPT media tests through LucinaNet itself (servoshell has no media backend of ours): `wpt serve` on localhost, each test page
# in `lucinanet --render`, a user script that prints testharness.js's results to the console (LUCINANET_CONSOLE=1).
#   tests/media/wpt.sh <lucinanet binary> [test paths relative to tests/wpt/tests ...]
# Default: the media element tests and Media Source Extensions. Prints one line per test file (PASS/FAIL/TIMEOUT counts) and the
# totals; per-subtest results go to $OUT (default /tmp/lucinanet-wpt).
# Run it capped: systemd-run --user --scope -p CPUQuota=200% tests/media/wpt.sh ... ; it also waits while the CPU is at 90 C.
# Only localhost: a machine with no web-platform.test host entries fails the cross-origin subtests here, and they are not counted against us.
set -u
# These tests load clips with sound tracks; the pass is a number of subtests, not anything heard. Play to the null
# sink so a run on a Mac stays quiet, and leave the device alone for anyone watching the logs.
LUCINANET_MEDIA_SINK=${LUCINANET_MEDIA_SINK:-none}
export LUCINANET_MEDIA_SINK
bin=${1:?lucinanet binary}
shift
# The WPT corpus is not vendored here (`third_party/UPSTREAMS.toml` omits `tests/wpt`): set SERVO to a Servo checkout.
servo=${SERVO:-$(cd "$(dirname "$0")/../.." && pwd)/third_party/servo}
out=${OUT:-/tmp/lucinanet-wpt}
port=${PORT:-8123}
mkdir -p "$out"
cd "$servo/tests/wpt/tests" || exit 2
if [ $# -eq 0 ]; then
    set -- $(ls html/semantics/embedded-content/media-elements/*.html media-source/*.html 2>/dev/null | grep -v -- '-manual\|\.https\.\|\.sub\.')
fi
cat > "$out/config.json" <<EOF
{"browser_host": "localhost", "alternate_hosts": {}, "ports": {"http": [$port, "auto"], "https": [], "ws": [], "wss": [], "h2": [], "webtransport-h3": []}, "check_subdomains": false}
EOF
./wpt serve --config "$out/config.json" > "$out/serve.log" 2>&1 &
serve=$!
trap 'kill $serve 2>/dev/null' EXIT
for _ in $(seq 1 60); do curl -s -o /dev/null "http://localhost:$port/" && break; sleep 1; done
cat > "$out/report.js" <<'EOF'
// Waits for testharness.js, then prints each subtest's result and a last line when the file is done.
(function wait() {
  if (typeof add_completion_callback !== "function") { setTimeout(wait, 20); return; }
  const names = ["PASS", "FAIL", "TIMEOUT", "NOTRUN", "PRECONDITION_FAILED"];
  add_completion_callback(function (tests, status) {
    for (const t of tests) console.log("wpt-result: " + names[t.status] + " " + t.name.replace(/\s+/g, " ") + (t.message ? " | " + String(t.message).slice(0, 200).replace(/\s+/g, " ") : ""));
    console.log("wpt-done: harness=" + ["OK", "ERROR", "TIMEOUT", "PRECONDITION_FAILED"][status.status]);
  });
})();
EOF
# the machine's own thermal limit: background CPU work waits while the CPU is at or above 90 C (k10temp Tctl).
cool() {
    local h
    for h in /sys/class/hwmon/hwmon*; do
        [ "$(cat "$h/name" 2>/dev/null)" = k10temp ] || continue
        while [ $(( $(cat "$h/temp1_input") / 1000 )) -ge ${MAX_C:-90} ]; do sleep 20; done
    done
}
total_pass=0; total=0
for t in "$@"; do
    cool
    log="$out/$(echo "$t" | tr / _).log"
    LUCINANET_CONSOLE=1 LUCINANET_USER_JS="$out/report.js" LUCINANET_RENDER_HOLD=${HOLD:-15} LUCINANET_PROFILE_DIR=none \
        timeout 90 "$bin" --render "http://localhost:$port/$t" "$out/shot.png" > "$log" 2>&1
    pass=$(grep -c "wpt-result: PASS" "$log"); all=$(grep -c "wpt-result: " "$log")
    done_=$(grep -o "wpt-done: harness=[A-Z_]*" "$log" | head -1)
    echo "$t: $pass/$all ${done_:-wpt-done: none (timed out or crashed)}"
    total_pass=$((total_pass + pass)); total=$((total + all))
done
echo "total: $total_pass/$total subtests pass"
