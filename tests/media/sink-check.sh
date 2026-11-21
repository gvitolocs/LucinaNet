#!/bin/sh
# sink-check: does LUCINANET_MEDIA_SINK=none really keep the machine quiet, in the browser and not only in a unit test?
#   tests/media/sink-check.sh [lucinanet binary]      (or LUCINANET_BIN=...)
#
# What a person in the room notices: NOTHING AUDIBLE -- every sample the page makes is zero, and the sink is the null
# one besides, so it stays silent even if the switch itself were broken. No window (--render is headless), no focus
# change, about 10 s of one CPU core, and no network: a file: page, no server, nothing fetched.
#
# How it answers. The engine renders tests/media/silent-audio.html with the switch on and LUCINANET_MEDIA_WAV pointed
# at a temp file: the tee wraps whichever sink was chosen, so the file holds what the mixer mixed even though nothing
# was played. PASS needs all three:
#   (i)   the log carries "lucinanet-media: sound sink none", so base_sink took the switch;
#   (ii)  that WAV is longer than its 44-byte header, so frames really ran through the sink while the page's clock moved;
#   (iii) the log carries no CoreAudio line at all: with the switch on, crates/lucinanet-media/src/mixer.rs returns the
#         stand-in before its macOS block, so cpal is never asked for an output stream on a Mac.
# Where there is no sound device (the build host, QEMU) the stand-in is what you get with the switch off too, so there the
# proof of the switch is (i), the unit tests in mixer.rs and that code path; (iii) still says the device path was not
# walked. On a Mac (i) is the line that would be missing if the switch did not work, and the clip would be audible.
#
# Exit: 0 PASS, 1 a check failed, 2 no binary.
# Portable sh for the Mac's bash 3.2 and the BSD tools: no GNU-only flags, no /proc. `timeout` is GNU, and macOS names
# it gtimeout (coreutils); with neither, the render runs uncapped and LUCINANET_RENDER_HOLD is what bounds it.
set -u
bin=${1:-${LUCINANET_BIN:-${WORK_DIR:-$(mktemp -d)}/lucinanet/target-fullrust-x86/probe/lucinanet-servo}}
[ $# -gt 0 ] && shift
here=$(cd "$(dirname "$0")" && pwd)
[ -x "$bin" ] || { echo "sink-check: FAIL no executable at $bin"; exit 2; }
# Not SECONDS: bash keeps its own elapsed-seconds counter under that name.
limit=${LIMIT:-25}
if command -v timeout >/dev/null 2>&1; then to="timeout $limit"
elif command -v gtimeout >/dev/null 2>&1; then to="gtimeout $limit"
else to=""; fi
out=${TMPDIR:-/tmp}/lucinanet-sink-check.$$
mkdir -p "$out" || exit 2
log="$out/sink.log"
wav="$out/sink.wav"

# The hold is the wait for playback: the page plays its two silent sounds for ~3 s, and the mixer writes as it goes.
# RUST_LOG names only our crate (its three debug lines are the CoreAudio ones, so nothing hot): at debug, check (iii)
# catches a Mac that did open CoreAudio, and check (i) is the line the switch says it logs.
LUCINANET_MEDIA_SINK=none LUCINANET_MEDIA_WAV="$wav" LUCINANET_CONSOLE=1 LUCINANET_PROFILE_DIR=none \
    RUST_LOG=${RUST_LOG:-lucinanet_media=debug} \
    LUCINANET_RENDER_HOLD=${HOLD:-7} LUCINANET_FONT_DIRS=${LUCINANET_FONT_DIRS:-/usr/share/fonts} \
    $to "$bin" --render "file://$here/silent-audio.html" "$out/sink.png" > "$log" 2>&1
rc=$?

fail=0
[ "$rc" = 124 ] && { echo "sink-check: FAIL the render outlived its ${limit} s timeout"; fail=1; }

if grep -q "sound sink none" "$log"; then
    echo "sink-check: ok (i) the log says the sink is none"
else
    echo "sink-check: FAIL (i) no \"lucinanet-media: sound sink none\" line in $log"
    fail=1
fi

if [ -f "$wav" ]; then bytes=$(wc -c < "$wav" | tr -d ' '); else bytes=0; fi
if [ "$bytes" -gt 44 ]; then
    echo "sink-check: ok (ii) $bytes bytes of WAV past the 44-byte header: audio ran through the null sink"
else
    echo "sink-check: FAIL (ii) $wav holds $bytes bytes: nothing was mixed into it"
    fail=1
fi

if grep -q "CoreAudio" "$log"; then
    echo "sink-check: FAIL (iii) the log carries a CoreAudio line, so a device was asked for:"
    grep "CoreAudio" "$log" | sed 's/^/    /'
    fail=1
else
    echo "sink-check: ok (iii) no CoreAudio line: no output stream was ever built"
fi

grep "sink-check:" "$log" | sed 's/^/  page: /'

if [ "$fail" = 0 ]; then
    echo "sink-check: PASS (nothing in the room but ~10 s of a core; log and WAV dropped with $out)"
    rm -rf "$out"
else
    echo "sink-check: FAIL (log, WAV and picture in $out)"
fi
exit "$fail"
