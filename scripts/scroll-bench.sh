#!/usr/bin/env bash
# The scroll benchmark for one built LucinaNet binary, offscreen: frame times of a page being scrolled, and
# where the time of each frame went. `tools/scroll-bench-tables` turns those runs into the findings
# tables, and its README says what each column means and what the run answers.
#
#   scripts/scroll-bench.sh target/probe/lucinanet-servo
#   scripts/scroll-bench.sh ~/bin/lucinanet-servo --scales 2,1 --runs 3 --out /tmp/sb
#   scripts/scroll-bench.sh ./lucinanet-servo --pages long --raster swgl --pace free
#
# Nothing here needs a display: `--render` paints with swgl into a buffer in memory, and the binary the
# benchmark uses is built without the `desktop` feature, so the window code (winit) is not even in it. The
# child is run with DISPLAY and WAYLAND_DISPLAY taken away, so a window could not be mapped if the code tried.
#
# Each run leaves three files in --out: `table/NAME.txt` (the table, also printed here), `json/NAME.json`
# (the same run, per frame) and `log/NAME.log` (the run's own log, the source of the stage columns).
set -u

usage() {
    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
    cat <<'USAGE'

options
  --out DIR        where pages/, table/, json/, log/, png/ go        (scroll-bench-out)
  --pages LIST     long, wiki, yt, or a file:// or https:// URL      (all three)
  --steps N        wheel steps per run                               (60)
  --delta PX       CSS pixels per step                               (120)
  --scales LIST    device pixels per CSS pixel to run at             (2)
  --runs N         repeats of each page and scale, to see the spread (1)
  --raster NAME    rust (default) or swgl: which rasterizer draws    (rust)
  --threads N      rasterizer threads (LUCINANET_RASTER_THREADS)     (the engine's default)
  --pace MODE      vsync: one step per 16.7 ms. free: as fast as the engine presents   (vsync)
  --css WxH        the window in CSS points                          (1740x1090)
  --lean           no engine diagnostics: the table then lacks the stage columns
  --quiet-ms N     LUCINANET_RENDER_QUIET_MS for pages other than YouTube (1500)
USAGE
}

BIN=${1:-}
if [ -z "$BIN" ] || [ "${1#-}" != "$1" ]; then
    usage
    exit 2
fi
shift
OUT=scroll-bench-out
PAGES="long,wiki,yt"
STEPS=60
DELTA=120
SCALES="2"
RUNS=1
RASTER=${LUCINANET_RASTER:-rust}
THREADS=${LUCINANET_RASTER_THREADS:-}
PACE=vsync
CSS=1740x1090
LEAN=0
QUIET=${LUCINANET_RENDER_QUIET_MS:-1500}
WIKI='https://en.wikipedia.org/wiki/Rust_(programming_language)'
YT='https://www.youtube.com/results?search_query=rust+programming'
QUIET_YT=6000

while [ $# -gt 0 ]; do
    case $1 in
        --out) OUT=$2; shift 2 ;;
        --pages) PAGES=$2; shift 2 ;;
        --steps) STEPS=$2; shift 2 ;;
        --delta) DELTA=$2; shift 2 ;;
        --scales) SCALES=$2; shift 2 ;;
        --runs) RUNS=$2; shift 2 ;;
        --raster) RASTER=$2; shift 2 ;;
        --threads) THREADS=$2; shift 2 ;;
        --pace) PACE=$2; shift 2 ;;
        --css) CSS=$2; shift 2 ;;
        --lean) LEAN=1; shift ;;
        --quiet-ms) QUIET=$2; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "scroll-bench: unknown option $1" >&2; usage >&2; exit 2 ;;
    esac
done

[ -x "$BIN" ] || { echo "scroll-bench: no executable binary at $BIN (cargo build --profile probe -p lucinanet-servo --features media)" >&2; exit 2; }
case $PACE in vsync) PINES=1 ;; free) PINES=0 ;; *) echo "scroll-bench: --pace wants vsync or free" >&2; exit 2 ;; esac
CSS_W=${CSS%x*}
CSS_H=${CSS#*x}
case $CSS_W$CSS_H in *[!0-9]*) echo "scroll-bench: --css wants WxH" >&2; exit 2 ;; esac

HERE=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$OUT"/{pages,table,json,log,png}

# A page of nothing but scrollable content: text, an image, a gradient and a card, 200 sections deep, so the
# run measures the engine and not a site. No one's words: the sentences are made here from lists of phrases.
make_long_page() {
    local out=$OUT/pages/long.html n=${1:-200}
    if [ -f "$out" ] && [ "${REGEN:-0}" = 0 ]; then
        return 0
    fi
    cat > "$OUT/pages/tile.svg" <<'SVG'
<svg xmlns="http://www.w3.org/2000/svg" width="320" height="180" viewBox="0 0 320 180">
<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#3b6fd4"/><stop offset="1" stop-color="#d44a3b"/></linearGradient></defs>
<rect width="320" height="180" fill="url(#g)"/>
<circle cx="80" cy="58" r="34" fill="#ffffff" opacity="0.65"/>
<rect x="140" y="92" width="150" height="58" rx="8" fill="#1c1e21" opacity="0.55"/>
<path d="M0 150 L80 100 L160 140 L240 90 L320 130 L320 180 L0 180 Z" fill="#0b3d2e" opacity="0.5"/>
</svg>
SVG
    # Spelled out, not generated: `seq` is GNU and a Mac has none, where a silent empty card would change this
    # page's height - which the spec says is what makes two `long` runs comparable in the first place.
    local FILLER='1 2 3 4 5 6 7 8 9 10 11 12'
    local subj=(the renderer a frame the compositor a tile the picture cache the scroll tree a glyph run the rasterizer this section)
    local verb=(walks measures paints reuses invalidates queues blends splits counts)
    local obj=(the display list the whole window the tiles that changed a stack of layers the dirty rectangle the text of a paragraph a gradient band an image of a thumbnail the box of every line)
    local how=(once per second at the top of the window on the way down without a rebuild in one pass over four threads as fast as the loop allows)
    {
        echo '<!doctype html><html lang=en><head><meta charset=utf-8><title>LucinaNet scroll bench</title><style>'
        echo 'body{margin:0;font:16px/1.6 -apple-system,system-ui,sans-serif;color:#1c1e21;background:#fff}'
        echo 'header{position:sticky;top:0;height:48px;display:flex;align-items:center;padding:0 24px;background:#20232a;color:#fff}'
        echo 'main{max-width:900px;margin:0 auto;padding:8px 24px 48px}'
        echo 'section{padding:24px 0;border-bottom:1px solid #d8dadf}'
        echo 'h2{font-size:22px;margin:0 0 10px} p{margin:0 0 10px;max-width:70ch}'
        echo '.thumb{width:320px;height:180px;float:left;margin:0 20px 10px 0;border-radius:6px}'
        echo '.grad{height:140px;border-radius:6px;margin:10px 0}'
        echo '.card{padding:12px 16px;border-radius:8px;background:#f2f3f5;margin:10px 0}'
        echo '.clear{clear:both}</style></head><body>'
        echo '<header>LucinaNet scroll bench - synthetic page, '"$n"' sections</header><main>'
        local i j p
        for ((i = 1; i <= n; i++)); do
            echo "<section id=s$i><h2>Section $i</h2>"
            echo "<img class=thumb src=tile.svg alt='' style='filter:hue-rotate($((i * 7 % 360))deg)'>"
            for ((p = 0; p < 3; p++)); do
                printf '<p>'
                for ((j = 0; j < 8; j++)); do
                    local k=$(( (i + p * 3 + j * 5) % ${#subj[@]} ))
                    local v=$(( (i * 3 + j * 7 + p) % ${#verb[@]} ))
                    local o=$(( (i * 5 + j * 3 + p * 2) % ${#obj[@]} ))
                    local h=$(( (i + j * 2 + p * 4) % ${#how[@]} ))
                    printf '%s %s %s %s. ' "${subj[$k]}" "${verb[$v]}" "${obj[$o]}" "${how[$h]}"
                done
                printf '</p>\n'
            done
            echo "<div class=grad style='background:linear-gradient($((i * 11 % 360))deg,#$(( (i * 4096 + 60000) % 0xffffff )),#$(( (i * 91523 + 200000) % 0xffffff ))'></div>"
            echo "<div class=card>Card $i: $(printf 'filler %d ' $FILLER)</div>"
            echo '<div class=clear></div></section>'
        done
        echo '</main></body>'
    } > "$out"
    echo "scroll-bench: wrote $out ($(wc -c < "$out") bytes, $n sections)"
}

# What a page name means: the URL, how long the page is allowed to settle, and any script it needs first.
page_setup() {
    URL=$1
    QUIET_MS=$QUIET
    JS=
    case $URL in
        long) URL="file://$(cd "$OUT/pages" && pwd)/long.html" ;;
        wiki) URL=$WIKI ;;
        yt)
            URL=$YT
            QUIET_MS=$QUIET_YT
            JS=$HERE/tools/youtube-consent-reject.js
            [ -f "$JS" ] || { echo "scroll-bench: no consent script at $JS; pass a page URL instead of yt" >&2; JS=; }
            ;;
        http://* | https://* | file://*) ;;
        *) echo "scroll-bench: unknown page $URL (long, wiki, yt or a URL)" >&2; return 1 ;;
    esac
}

run_one() { # page scale run
    local page=$1 scale=$2 run=$3 name w h
    # The rasterizer and the pace are in the name: two configurations into one --out are two runs, not one
    # that quietly overwrote the other.
    name="${page}-s${scale}-${RASTER}-${PACE}-${run}"
    w=$(( CSS_W * scale ))
    h=$(( CSS_H * scale ))
    page_setup "$page" || return 1
    local diag=()
    if [ "$LEAN" = 0 ]; then
        diag=(LUCINANET_RENDER_STATS=1 LUCINANET_WR_PHASES=1 LUCINANET_LAYOUT_PHASES=1 LUCINANET_SWGL_PROFILE=1)
    fi
    printf '\n=== %s  %s  %sx%s device px at scale %s  [%s raster]\n' "$name" "$URL" "$w" "$h" "$scale" "$RASTER"
    printf '    before: %s  free %s\n' "$(uptime | sed 's/^ *//')" "$(df -h / | tail -1 | awk '{print $4}')"
    local started
    started=$(date +%s)
    env -u DISPLAY -u WAYLAND_DISPLAY \
        LUCINANET_SCALE="$scale" \
        LUCINANET_RASTER="$RASTER" \
        ${THREADS:+LUCINANET_RASTER_THREADS=$THREADS} \
        ${JS:+LUCINANET_USER_JS=$JS} \
        LUCINANET_RENDER_QUIET_MS="$QUIET_MS" \
        LUCINANET_SCROLL_BENCH="${STEPS}x${DELTA}" \
        LUCINANET_SCROLL_BENCH_PACE="$PINES" \
        LUCINANET_SCROLL_BENCH_OUT="$OUT/json/$name.json" \
        ${diag[@]+"${diag[@]}"} \
        "$BIN" --render "$URL" "$OUT/png/$name.png" "${w}x${h}" > "$OUT/table/$name.txt" 2> "$OUT/log/$name.log"
    local rc=$?
    printf '    %s ms wall, exit %s; after: %s  free %s\n' \
        "$(( ($(date +%s) - started) * 1000 ))" "$rc" "$(uptime | sed 's/^ *//')" "$(df -h / | tail -1 | awk '{print $4}')"
    cat "$OUT/table/$name.txt"
    if [ "$rc" != 0 ] || ! grep -q '^scroll-bench ' "$OUT/table/$name.txt"; then
        echo "    no table: the last 20 lines of $OUT/log/$name.log"
        tail -20 "$OUT/log/$name.log" | sed 's/^/    /'
    fi
    return $rc
}

echo "scroll-bench: binary $BIN"
echo "scroll-bench: offscreen - --render paints with swgl into memory; DISPLAY and WAYLAND_DISPLAY are unset for the child, and a probe build has no desktop feature to open a window with"
make_long_page

SUMMARY=()
IFS=, read -ra WANT <<< "$PAGES"
for page in "${WANT[@]}"; do
    for scale in ${SCALES//,/ }; do
            case $scale in *[!0-9]*|"" ) echo "scroll-bench: --scales takes whole numbers (2,1)" >&2; exit 2 ;; esac
        for ((r = 1; r <= RUNS; r++)); do
            name="${page}-s${scale}-${RASTER}-${PACE}-r${r}"
            if run_one "$page" "$scale" "r${r}"; then
                row=$(grep -m1 '^frame interval' "$OUT/table/$name.txt" || echo 'frame interval (no frames)')
                SUMMARY+=("$(printf '%-30s %s' "$name" "$row")")
            else
                SUMMARY+=("$(printf '%-30s FAILED' "$name")")
            fi
        done
    done
done

printf '\n=== every run, the frame interval it measured (ms; %s and %s are the budgets)\n' 16.7 33.3
printf '%s\n' ${SUMMARY[@]+"${SUMMARY[@]}"}
printf 'files under %s: table/ json/ log/ png/ pages/\n' "$OUT"
