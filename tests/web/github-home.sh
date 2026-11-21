#!/bin/sh
# github-home: does the signed-out https://github.com/ home paint its React content, or only its chrome?
#   tests/web/github-home.sh [lucinanet binary]
#
# What this gates. GitHub's signed-out home is a marketing page, not a dashboard: the server sends the whole
# thing inside <react-app> (378 KB of markup, ~2200 elements, the live activity feed as DataMarquee/Cell/
# RepositoryPanel), but it ships the hero with inline style="opacity:0" and only React's first client render
# clears it. In LucinaNet that render runs as one uninterrupted ~8 s task on the script thread, so a snapshot
# taken at the default settle window catches the page after the header painted and before the hero and the feed
# turned visible -- the "nav, search, Sign in paint; the feed area stays empty; no console line" of the
# 2026-11-13 30-site sweep. Nothing is missing and nothing is thrown: no absent web API, no unhandled rejection,
# no failed module, no hung fetch (the marketing home makes no fetch or XHR at all), and react-app does upgrade.
#
# So the check is not "is the DOM there" -- it always is -- but "did the feed area paint boxes and did the hero
# turn opaque", read out of the live page by tests/web/github-home-check.js. The render gets a settle window
# long enough to span the reveal; that is the point of the gate, not a workaround: a window that cannot span it
# is the regression this test exists to catch.
#
# The repo page /github/gitignore is rendered as a control and scored on its own terms (a painted main box,
# enough elements): it is not React-hydrated and carries none of the marketing classes. If the control fails
# too, the fault is elsewhere in the binary, not in this page's boot.
#
# Needs the network. Slower tier, not the verification step. Runs headless: --render takes the snapshot itself.
# Usage: tests/web/github-home.sh [--bin PATH] [--out DIR] [--quiet MS] [--max MS] [--tries N]
# Exit: 0 all scored renders passed, 1 any failed, 2 binary not executable, 77 no network so nothing measured.
set -u

bin=${LUCINANET_BIN:-${WORK_DIR:-$(mktemp -d)}/lucinanet/target/probe/lucinanet-servo}
out=${TMPDIR:-/tmp}/lucinanet-github-home.$$
# A live site may carry an autoplaying clip, and this checks pixels: keep the room out of it.
LUCINANET_MEDIA_SINK=${LUCINANET_MEDIA_SINK:-none}
export LUCINANET_MEDIA_SINK
quiet=${LUCINANET_RENDER_QUIET_MS:-16000}
max=${LUCINANET_RENDER_SETTLE_MAX_MS:-24000}
tries=2
while [ $# -gt 0 ]; do
    case "$1" in
        --bin)   bin=$2; shift 2 ;;
        --out)   out=$2; shift 2 ;;
        --quiet) quiet=$2; shift 2 ;;
        --max)   max=$2; shift 2 ;;
        --tries) tries=$2; shift 2 ;;
        -h|--help) echo "usage: $0 [--bin PATH] [--out DIR] [--quiet MS] [--max MS] [--tries N]"; exit 0 ;;
        *) echo "usage: $0 [--bin PATH] [--out DIR] [--quiet MS] [--max MS] [--tries N]" >&2; exit 2 ;;
    esac
done

if [ ! -x "$bin" ]; then
    echo "github-home: the binary is not executable: $bin"
    echo "github-home: build one, or pass it: tests/web/github-home.sh --bin PATH"
    exit 2
fi

# Reachability, not a verdict: a site that cannot be reached measures nothing.
if ! curl -s -o /dev/null --max-time 20 -A 'Mozilla/5.0 (X11; Linux x86_64; rv:150.0) Gecko/20100101 Firefox/150.0' \
        https://github.com/ ; then
    echo "github-home: SKIPPED -- https://github.com/ is not reachable from here"
    exit 77
fi

here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
fail=0

# `timeout` is GNU; macOS ships none (coreutils names it gtimeout). A hung render is a different failure than
# the one this gate asks about, so where there is no timeout the run goes without one.
if command -v timeout >/dev/null 2>&1; then to='timeout --kill-after=15'
elif command -v gtimeout >/dev/null 2>&1; then to='gtimeout --kill-after=15'
else to=''; fi

# $1 seconds, then the command. A subshell so the exports die with it. `$to` is deliberately unquoted: it is
# two words ("timeout --kill-after=15") and has to split.
launch() {
    secs=$1; shift
    if [ -n "$to" ]; then ( $to "$secs" "$@" ); else ( "$@" ); fi
}

# One render. $1 name, $2 url, $3 quiet, $4 max, $5 userjs, $6 scoring: "verdict" for the home (the page's own
# ok= flag, which is the React question), "content" for the control (a painted main box, which is the binary's).
render() {
    name=$1 url=$2 q=$3 m=$4 ujs=$5 mode=$6
    i=0; v=''; c=''
    while [ "$i" -lt "$tries" ]; do
        i=$((i + 1))
        rm -f "$out/$name.png"
        (
            export LUCINANET_CONSOLE=1 LUCINANET_PROFILE_DIR=none \
                   LUCINANET_RENDER_QUIET_MS="$q" LUCINANET_RENDER_SETTLE_MAX_MS="$m" \
                   LUCINANET_USER_JS="$ujs" LUCINANET_JS_MISSING_LOG="$out/$name.mlog"
            launch 180 "$bin" --render "$url" "$out/$name.png" 1366x900
        ) > "$out/$name.log" 2>&1
        rc=$?
        # The last line of its kind is the one taken nearest the snapshot.
        v=$(grep -o 'ghhome VERDICT .*' "$out/$name.log" | tail -1)
        c=$(grep -o 'ghhome CONTENT .*' "$out/$name.log" | tail -1)
        case "$mode" in
            verdict)
                case "$v" in *ok=1*) echo "  $name: PASS ${v#ghhome VERDICT }"; return 0 ;; esac ;;
            content)
                h=$(printf '%s' "$c" | sed 's/.*mainBox=[0-9]*x\([0-9]*\).*/\1/')
                n=$(printf '%s' "$c" | sed 's/.* els=\([0-9]*\).*/\1/')
                case "$c" in
                    *mainBox=*)
                        if [ -n "$h" ] && [ "$h" -gt 500 ] 2>/dev/null \
                           && [ -n "$n" ] && [ "$n" -gt 1000 ] 2>/dev/null; then
                            echo "  $name: PASS ${c#ghhome CONTENT }"; return 0
                        fi ;;
                esac ;;
        esac
        echo "  $name: attempt $i/$tries rc=$rc verdict=${v:-none} content=${c:-none}"
    done
    echo "  $name: FAIL verdict=${v:-none} content=${c:-none}"
    return 1
}

echo "github-home: signed-out https://github.com/ home, settle quiet ${quiet} ms at most ${max} ms"
render home https://github.com/ "$quiet" "$max" "$here/github-home-check.js" verdict || fail=1

echo "github-home: control, the repo page /github/gitignore"
render repo https://github.com/github/gitignore "$quiet" "$max" "$here/github-home-check.js" content || fail=1

# The blank frame this gate exists for, measured rather than asserted: a 9 s snapshot of a page that reveals at
# ~12 s is expected to be empty, so this reports the PNG size that says so and scores nothing.
echo "github-home: for reference, the home at the sweep's window (quiet 8000 ms, max 12000 ms)"
rm -f "$out/home-short.png"
(
    export LUCINANET_CONSOLE=1 LUCINANET_PROFILE_DIR=none \
           LUCINANET_RENDER_QUIET_MS=8000 LUCINANET_RENDER_SETTLE_MAX_MS=12000 \
           LUCINANET_USER_JS="$here/github-home-check.js"
    launch 120 "$bin" --render https://github.com/ "$out/home-short.png" 1366x900
) > "$out/home-short.log" 2>&1
short_bytes=$( if [ -f "$out/home-short.png" ]; then wc -c < "$out/home-short.png" | tr -d ' '; else echo 0; fi )
short_v=$(grep -o 'ghhome VERDICT .*' "$out/home-short.log" | tail -1)
# "no verdict line" here is the finding, not a broken run: the first read is taken 4 s after the script starts
# and the page's one ~8 s task delays every timer, so a snapshot that settles at 8.5 s exits before any read lands.
echo "  home-short: png ${short_bytes} bytes, ${short_v:-no verdict line taken before the snapshot} (not scored)"

for f in home repo; do
    [ -f "$out/$f.png" ] && echo "  png $f: $(wc -c < "$out/$f.png" | tr -d ' ') bytes, $out/$f.png"
done

if [ "$fail" -eq 0 ]; then
    echo "github-home: PASS -- on the home the hero is opaque and the feed painted boxes; the control painted too"
    if [ "${short_bytes:-0}" -gt 0 ] 2>/dev/null && [ "$short_bytes" -lt 40000 ]; then
        echo "github-home: note the reference shot at the sweep's window is ${short_bytes} bytes, the blank frame;"
        echo "github-home: the scored window is ${quiet} ms quiet, which is what spans React's first client render"
    fi
else
    echo "github-home: FAIL -- see $out; on the home, ok=0 with upgraded=1 and heroOp=0 means React's first"
    echo "github-home: client render had not run yet when the snapshot was taken"
fi
exit "$fail"
