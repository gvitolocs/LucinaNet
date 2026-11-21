#!/bin/sh
# a tracked flaw: a scaled video drawn as YUV planes must be the whole frame, not one source row repeated down the box.
#   tests/media/scaled-planes.sh [lucinanet binary]
# Same seeked frame twice: once as planes (WebRender draws them with brush_yuv_image, under a transform or a
# non-1:1 size) and once as BGRA (LUCINANET_MEDIA_YUV=0, an ordinary image). scaled-planes.py counts the picture's
# columns that are exactly constant down their height -- bars -- and how much the two renders agree.
# Runs headless: --render takes the snapshot itself. A render that leaves the box unpainted is retried, since the
# frame is painted a few ms after the seek lands.
# Needs python3 with PIL, for the painted check and for the scoring. Without it nothing is measured and nothing is
# rendered: the run says SKIPPED and exits 77. Install PIL with `python3 -m pip install pillow`.
# To measure on a machine without PIL (a Mac with the stock python3) and score it elsewhere, take the two renders
# there by hand, then run the compare where PIL is. Both renders, same window, one line each with YUV=1 and with
# YUV=0; the exit code of the compare is the verdict:
#   here=$(cd tests/media && pwd)
#   for mode in planes bgra; do
#     [ $mode = planes ] && yuv=1 || yuv=0
#     LUCINANET_CONSOLE=1 LUCINANET_USER_JS="$here/pause-at.js" LUCINANET_MEDIA_YUV=$yuv \
#       LUCINANET_FONT_DIRS=<a font dir, /System/Library/Fonts on macOS> LUCINA_AUDIO_TEST_DB=-90 \
#       LUCINANET_PROFILE_DIR=none LUCINANET_RENDER_QUIET_MS=1500 LUCINANET_RENDER_SETTLE_MAX_MS=12000 \
#       LUCINANET_MEDIA_SINK=none \
#       <binary> --render "file://$here/scaled-planes.html" /tmp/$mode.png 1366x900
#   done
#   python3 tests/media/scaled-planes.py compare /tmp/planes.png /tmp/bgra.png   # copy the two PNGs over first
set -u
bin=${1:-${WORK_DIR:-$(mktemp -d)}/lucinanet/target/probe/lucinanet-servo}
[ $# -gt 0 ] && shift
here=$(cd "$(dirname "$0")" && pwd)
# The clip is here for its picture, not its sound: play it to the null sink so a headless run is silent in the room.
# LUCINA_AUDIO_TEST_DB, which the laptop scripts set, is Juno's own device and does not silence a Mac.
LUCINANET_MEDIA_SINK=${LUCINANET_MEDIA_SINK:-none}
export LUCINANET_MEDIA_SINK

if ! python3 -c 'import PIL' 2>/dev/null; then
    echo "scaled-planes: SKIPPED -- python3 has no PIL, so the renders cannot be scored (install it with: python3 -m pip install pillow)"
    echo "scaled-planes: to capture here and score elsewhere, see the header of this script"
    exit 77
fi

out=${TMPDIR:-/tmp}/lucinanet-scaled-planes.$$
mkdir -p "$out"
tries=${TRIES:-4}
fail=0

# A digest for the log line: Linux has md5sum, macOS ships md5, and shasum is what both tend to have. Any stable
# digest answers the question asked here, which is only "is this the render I took before".
digest() {
    if command -v md5sum >/dev/null 2>&1; then md5sum "$1" | cut -c1-12
    elif command -v md5 >/dev/null 2>&1; then md5 -q "$1" | cut -c1-12
    elif command -v shasum >/dev/null 2>&1; then shasum "$1" | cut -c1-12
    else echo "no-digest-tool"
    fi
}

for mode in planes bgra; do
    case $mode in
        planes) yuv=1 ;;
        bgra) yuv=0 ;;
    esac
    i=0
    while [ "$i" -lt "$tries" ]; do
        i=$((i + 1))
        LUCINANET_CONSOLE=1 LUCINANET_USER_JS="$here/pause-at.js" LUCINANET_MEDIA_YUV=$yuv \
            LUCINANET_FONT_DIRS=${LUCINANET_FONT_DIRS:-/usr/share/fonts} LUCINA_AUDIO_TEST_DB=-90 \
            LUCINANET_PROFILE_DIR=none LUCINANET_RENDER_QUIET_MS=${QUIET:-1500} LUCINANET_RENDER_SETTLE_MAX_MS=${SETTLE:-12000} \
            "$bin" --render "file://$here/scaled-planes.html" "$out/$mode.png" 1366x900 > "$out/$mode.$i.log" 2>&1
        if python3 "$here/scaled-planes.py" painted "$out/$mode.png" > "$out/$mode.try" 2>&1; then
            echo "$mode: painted on try $i ($(digest "$out/$mode.png"))"
            break
        fi
        [ "$i" = "$tries" ] && echo "$mode: never painted ($(cat "$out/$mode.try"))" && fail=1
    done
done

python3 "$here/scaled-planes.py" compare "$out/planes.png" "$out/bgra.png" || fail=1
if [ "$fail" = 0 ]; then
    echo "scaled-planes: PASS"
    rm -rf "$out"
else
    echo "scaled-planes: FAIL (renders and logs in $out)"
fi
exit "$fail"
