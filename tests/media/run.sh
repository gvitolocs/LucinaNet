#!/bin/sh
# LucinaNet's media pages, in the real engine: each page plays a clip (or WebAudio) and logs "media-test: ..." lines to the
# console, which LUCINANET_CONSOLE=1 sends to stderr. Pass: every page ends with "media-test: PASS".
#   tests/media/run.sh [lucinanet binary] [pages...]
# On a machine with no sound device the sound plays to nowhere at real time (no /dev/lucina-audio); LUCINANET_MEDIA_WAV writes the mix to a file.
# The backend's "lucinanet-media: played ..." line (shown, dropped, decode times, sound_frames) is printed for each video.
set -u
# Nothing here listens, so the clips play to the null sink: on a Mac the mixer would otherwise open CoreAudio and the room
# would hear every page. LUCINANET_MEDIA_WAV still records the mix, and the clock runs at real time either way.
# Override it (LUCINANET_MEDIA_SINK= empties it back to the device) to hear a run on purpose.
LUCINANET_MEDIA_SINK=${LUCINANET_MEDIA_SINK:-none}
export LUCINANET_MEDIA_SINK
bin=${1:-${WORK_DIR:-$(mktemp -d)}/lucinanet/target/probe/lucinanet-servo}
[ $# -gt 0 ] && shift
here=$(cd "$(dirname "$0")" && pwd)
pages=${*:-video-mp4 video-mp4-moov-end video-webm audio-mp3 seek-mp4 webaudio mse mse?single}
out=${TMPDIR:-/tmp}/lucinanet-media-pages.$$
mkdir -p "$out"
# Pages that use fetch() go over HTTP (Servo's fetch refuses file:); media elements load file: with ranges answered (206).
port=${PORT:-8124}
(cd "$here/../.." && exec python3 -m http.server --bind 127.0.0.1 "$port") > "$out/http.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null' EXIT
sleep 1
fail=0
for p in $pages; do
    case $p in
        webaudio*|mse*) url="http://127.0.0.1:$port/tests/media/${p%%\?*}.html${p#${p%%\?*}}" ;;
        *) url="file://$here/$p.html" ;;
    esac
    name=$(echo "$p" | tr '?' '-')
    LUCINANET_CONSOLE=1 LUCINANET_RENDER_HOLD=${HOLD:-12} LUCINANET_PROFILE_DIR=none LUCINANET_MEDIA_WAV="$out/$name.wav" \
        "$bin" --render "$url" "$out/$name.png" > "$out/$name.log" 2>&1
    p=$name
    grep -E "media-test:|lucinanet-media: played|render: complete" "$out/$p.log" | sed "s/^/$p: /"
    if grep -q "media-test: PASS" "$out/$p.log"; then
        echo "$p: PASS"
    else
        echo "$p: FAIL (log $out/$p.log)"
        fail=1
    fi
done
[ "$fail" = 0 ] && rm -rf "$out"
exit "$fail"
