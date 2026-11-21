#!/bin/sh
# frozen-play two-SourceBuffer MSE test, in the real engine, with no Python and no committed blobs:
#   tests/media/mse-two-buffers.sh [lucinanet binary] [variant ...]
# Generates a 30 s VP9 + Opus fragmented WebM pair into a scratch tree with ffmpeg, serves it with a small Rust static server
# (tests/media/serve.rs), and runs the page for each variant. The plain run must print "MSE-TEST ok"; every variant must reach
# currentTime >= 25 s. The sound is silent (LUCINANET_MEDIA_SINK=none) but the clock still follows it, as a real play does.
# Run it against the OLD binary too to see which variants reproduce the stall. KEEP=1 leaves the scratch tree (logs, pngs, webm) behind.
set -u
bin=${1:-${WORK_DIR:-$(mktemp -d)}/lucinanet/nav05-fix/lucinanet-servo-diag}
[ $# -gt 0 ] && shift
variants=${*:-plain pause seek init2}
here=$(cd "$(dirname "$0")" && pwd)
root=$(mktemp -d "${TMPDIR:-/tmp}/mse-two-buffers.XXXXXX")
trap 'kill $server 2>/dev/null; [ -n "${KEEP:-}" ] || rm -rf "$root"' EXIT INT TERM
port=${PORT:-8137}
mkdir -p "$root/tests/media" "$root/_mse_work" "$root/out"
cp "$here/mse-two-buffers.html" "$root/tests/media/"

echo "== generating 30 s fragmented WebM with ffmpeg (vp9 + opus) =="
# Fast VP9/Opus settings; ~5 clusters a track at an 8 s cluster limit -> init + 4-6 appends each.
ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc=size=320x240:rate=15:duration=30" \
    -an -c:v libvpx-vp9 -crf 45 -b:v 0 -deadline realtime -cpu-used 8 \
    -cluster_time_limit 8000 "$root/_mse_work/video.webm" || { echo "ffmpeg video failed"; exit 2; }
ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=30" \
    -vn -c:a libopus -b:a 48k -cluster_time_limit 8000 "$root/_mse_work/audio.webm" || { echo "ffmpeg audio failed"; exit 2; }
ls -la "$root/_mse_work"

echo "== building the static server =="
rustc --edition 2021 -O "$here/serve.rs" -o "$root/serve" || { echo "rustc serve.rs failed"; exit 2; }
"$root/serve" "$port" "$root" > "$root/out/http.log" 2>&1 &
server=$!
sleep 1

fail=0
for v in $variants; do
    url="http://127.0.0.1:$port/tests/media/mse-two-buffers.html?variant=$v"
    log="$root/out/$v.log"
    # The media trace is the caller's to ask for: with LUCINANET_MEDIA_TRACE set the log carries `media elem`, `show pts=` and
    # the mixer lines, which is what tools/media-trace-check reads. Unset, the browser is given exactly what it was before.
    set -- LUCINANET_CONSOLE=1 LUCINANET_MEDIA_SINK=none LUCINANET_PROFILE_DIR=none \
        LUCINANET_RENDER_HOLD=${HOLD:-50} LUCINANET_MEDIA_RECOVERY=${RECOVERY:-${LUCINANET_MEDIA_RECOVERY:-1}}
    if [ -n "${LUCINANET_MEDIA_TRACE:-}" ]; then set -- "$@" LUCINANET_MEDIA_TRACE="$LUCINANET_MEDIA_TRACE"; fi
    env "$@" "$bin" --render "$url" "$root/out/$v.png" > "$log" 2>&1
    if grep -q "MSE-TEST ok" "$log"; then
        echo "$v: PASS"
    else
        echo "$v: FAIL"
        grep -E "mse-two-buffers:|MSE-TEST|lucinanet-media: clock stood still" "$log" | sed "s/^/    /" | tail -20
        fail=1
    fi
done
echo "artifacts in $root"
exit "$fail"
