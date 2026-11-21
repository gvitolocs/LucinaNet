#!/bin/sh
# On the Juno laptop, from /data/lucina/net-media: the frame path A/B for YouTube-shaped video, then every laptop page.
#   sh ab-laptop.sh [runs...]     runs: yuv bgra nice10 or a page name (YUV path, decode at nice 0)
# Each run prints the backend's played line (shown, dropped, decode times), the page's getVideoPlaybackQuality line, and
# how many frames LucinaNet painted with their mean paint and copy times (LUCINA_APP_TRACE). Sound at -90 dB.
cd /data/lucina/net-media || exit 2
busybox httpd -f -p 127.0.0.1:8124 -h /data/lucina/net-media &
httpd=$!
trap 'kill $httpd 2>/dev/null' EXIT
run() { # name page [VAR=value...]
    n=$1; p=$2; shift 2
    case $p in mse*) url="http://127.0.0.1:8124/laptop/$p.html" ;; *) url="file:///data/lucina/net-media/laptop/$p.html" ;; esac
    env "$@" LUCINANET_CONSOLE=1 LUCINA_APP_TRACE=1 LUCINANET_RENDER_HOLD=${HOLD:-34} LUCINANET_PROFILE_DIR=none \
        LUCINA_AUDIO_TEST_DB=${DB:--90} timeout 90 ./lucinanet --render "$url" "out/$n.png" > "out/$n.log" 2>&1
    sync
    echo "== $n ($p $*)"
    grep -E "media-test: (PASS|FAIL|quality)|lucinanet-media: played" "out/$n.log"
    awk '/lucinanet: paint [0-9]+ us, copy/ { p += $3; c += $6; n++ } END { if (n) printf "painted %d frames, paint %d us, copy %d us (means)\n", n, p / n, c / n }' "out/$n.log"
}
runs=${*:-yuv bgra nice10 h264-720-30 h264-1080-60 vp9-1080 mse-1080 mse-replay seek-1080 mp3-30}
for r in $runs; do
    case $r in
        yuv) run yuv h264-1080-30 LUCINANET_MEDIA_YUV=1 ;;
        bgra) run bgra h264-1080-30 LUCINANET_MEDIA_YUV=0 ;;
        nice10) run nice10 h264-1080-30 LUCINANET_MEDIA_DECODE_NICE=10 ;;
        *) run "$r" "$r" ;;
    esac
done
echo "== all done"
