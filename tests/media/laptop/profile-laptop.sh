#!/bin/sh
# On the Juno laptop: one page in LucinaNet, and the CPU each of its threads used over 10 s of playback (from /proc), to see
# where a 1080p frame's time goes. busybox sh/awk only.
#   sh profile-laptop.sh <page> [seconds]
cd /data/lucina/net-media || exit 2
p=${1:-h264-1080}
secs=${2:-10}
LUCINANET_CONSOLE=1 LUCINANET_RENDER_HOLD=34 LUCINANET_PROFILE_DIR=none LUCINA_AUDIO_TEST_DB=-90 \
    ./lucinanet --render "file:///data/lucina/net-media/laptop/$p.html" "out/$p-prof.png" > "out/$p-prof.log" 2>&1 &
pid=$!
sleep 8
snap() { for t in /proc/$pid/task/*; do printf "%s %s\n" "$(tr ' ' '_' < $t/comm)" "$(cut -d' ' -f14,15 $t/stat)"; done; }
snap > out/prof-a.txt
sleep "$secs"
snap > out/prof-b.txt
hz=100
awk -v secs="$secs" -v hz="$hz" 'NR==FNR { a[$1" "NR] = 0; ua[$1] += $2 + $3; next } { ub[$1] += $2 + $3 } END {
    for (n in ub) { d = (ub[n] - ua[n]) * 100 / hz / secs; if (d >= 1) printf "%6.1f%%  %s\n", d, n } }' out/prof-a.txt out/prof-b.txt | sort -rn | head -25
wait $pid
grep -E "lucinanet-media: played|media-test: (PASS|FAIL|ended)" "out/$p-prof.log"
