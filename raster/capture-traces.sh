#!/usr/bin/env bash
# Records swgl call traces of real pages for the rasterizer comparison (raster/gltrace). On the build host:
#   raster/capture-traces.sh <lucinanet-servo binary> <out dir> [site ...]
# Each site is `name=url`; without sites, the default set below. Each run is short test work, not background work: it
# waits only while Tctl is at or above MAX_TCTL (default 97 °C), runs at nice 10, pinned to CPUs 4-11 (8, like the laptop). example-site idles 3 s with its animations; Wikipedia scrolls 4 steps down and up.
# Set WORK_DIR to a scratch directory holding the font tree that LUCINANET_FONT_DIRS falls back to.
set -uo pipefail
bin=${1:?binary}; out=${2:?out dir}; shift 2
sites=("$@")
if [ ${#sites[@]} -eq 0 ]; then
    sites=(
        "bench=file://$(cd "$(dirname "$0")" && pwd)/bench/paint-bench.html"
        "example=https://example.com/"
        "example-site=https://example-site.com/"
        "wiki=https://en.wikipedia.org/wiki/Rust_(programming_language)"
        "hn=https://news.ycombinator.com/"
        "github=https://github.com/servo/servo"
        "bbc=https://www.bbc.com/news"
    )
fi
temp() { sensors | awk '/Tctl/{gsub(/[+°C]/,"",$2); print int($2); exit}'; }
export LUCINANET_FONT_DIRS=${LUCINANET_FONT_DIRS:-${WORK_DIR:-$(mktemp -d)}/lucinanet/fonts}
for site in "${sites[@]}"; do
    name=${site%%=*}; url=${site#*=}
    dir=$out/$name
    rm -rf "$dir"; mkdir -p "$dir"
    until [ "$(temp)" -lt "${MAX_TCTL:-97}" ]; do sleep 20; done
    extra=()
    case $name in
        example-site) extra=(LUCINANET_IDLE_TEST=3) ;;
        wiki) extra=(LUCINANET_SCROLL_TEST=4) ;;
    esac
    env "${extra[@]}" LUCINANET_GLTRACE="$dir" timeout 120 nice -n 10 taskset -c 4-11 "$bin" --render "$url" "$dir/live.png" 1920x1080 \
        > "$dir/run.log" 2>&1
    echo "$name rc=$? $(du -sh "$dir" | cut -f1) $(grep -c . "$dir/run.log") log lines"
done
