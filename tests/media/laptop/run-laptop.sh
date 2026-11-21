#!/bin/sh
# On the Juno laptop, from /data/lucina/net-media: each page in LucinaNet with the real sound device at an inaudible level
# (LUCINA_AUDIO_TEST_DB only lowers it), results to /data as they come (sync), then a summary line per page.
#   sh run-laptop.sh [pages...]
# Pages that fetch() (MSE) are served by busybox httpd on 127.0.0.1:8124; the others load from file:.
cd /data/lucina/net-media || exit 2
pages=${*:-h264-720-30 h264-1080-30 h264-1080-60 h264-1080 vp9-1080 mp3-30 seek-1080 mse-1080}
busybox httpd -f -p 127.0.0.1:8124 -h /data/lucina/net-media &
httpd=$!
trap 'kill $httpd 2>/dev/null' EXIT
for p in $pages; do
    case $p in
        mse*) url="http://127.0.0.1:8124/laptop/$p.html" ;;
        *) url="file:///data/lucina/net-media/laptop/$p.html" ;;
    esac
    LUCINANET_CONSOLE=1 LUCINANET_RENDER_HOLD=${HOLD:-34} LUCINANET_PROFILE_DIR=none LUCINA_AUDIO_TEST_DB=${DB:--90} \
        timeout 90 ./lucinanet --render "$url" "out/$p.png" > "out/$p.log" 2>&1
    sync
    echo "== $p"
    grep -E "media-test: (PASS|FAIL|metadata|seeked|ended|appended|quality)|lucinanet-media: (played|first frame)|render: complete" "out/$p.log"
done
echo "== all done"
