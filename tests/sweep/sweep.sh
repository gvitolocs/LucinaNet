#!/bin/sh
# sweep.sh: the headless site sweep. It loads the fixed list tests/sweep/sites.tsv, one page per engine process, and
# leaves <out-dir>/<id>/{log,png,rc,dump,wall_ms,meta} for tools/sweep-score to judge (loaded / broken / walled).
#
#   tests/sweep/sweep.sh <binary> <out-dir> [--ids S001,S017 | --category news | --prev-only]
#                        [--build-id COMMIT] [--scorer PATH-TO-sweep-score] [--list FILE] [--workers N]
#
# WHAT A PERSON IN THE ROOM NOTICES
#   No sound (LUCINANET_MEDIA_SINK=none is exported for every page), no window (--render is headless), no focus
#   change. Heavy network use: about 100 public sites, each page pulls its scripts, styles and images. One to two
#   cores busy for the whole run (at most 4 pages in parallel, and a page is mostly waiting on the network).
#   Roughly 10 to 40 minutes for the full list, and never more than 4 browser processes alive at once.
#
# THE RULES (never broken): public URLs only; no login, no credentials, no cookies (LUCINANET_PROFILE_DIR=none, and
# nothing personal is read); no captcha solving and no bot-wall bypass of any kind (a walled page is scored
# walled by tools/sweep-score and is never retried); at most 4 pages in parallel; never two pages of one host at
# once and a 5 s gap between the end of one and the start of the next page of the same host (SWEEP_HOST_GAP_S);
# 120 s of wall clock per page (SWEEP_PAGE_CAP_S), then a TERM and, 10 s later, a KILL of the process this script
# started (rc is then 124). Nothing is killed that this script did not start.
#
# MUTE. The Mac must never play sound, so on macOS the script refuses to run unless SWEEP_ALLOW_MAC=1 is set AND
# tests/media/sink-check.sh passes for this binary (the null sink really is what the switch gives). On every host
# each page's log is checked afterwards: the line "sound sink none" appears when media starts (the log filter
# includes lucinanet_media at info), and a "sound sink" line that is NOT "none" is a MUTE-VIOLATION that stops
# the sweep. Pages whose log shows the null sink are counted in the summary line.
#
# Portable sh (the Mac's bash 3.2 and BSD tools): no GNU-only flags, no /proc, no `wait -n`.
set -u

here=$(cd "$(dirname "$0")" && pwd)
TAB=$(printf '\t')

usage() {
  sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'
}

bin=""; out=""; sel_ids=""; sel_cat=""; sel_prev=0; build_id=""; scorer=${SWEEP_SCORE:-}
list=$here/sites.tsv; workers=4
while [ $# -gt 0 ]; do
  case $1 in
    --ids) sel_ids=${2:-}; shift 2 ;;
    --category) sel_cat=${2:-}; shift 2 ;;
    --prev-only) sel_prev=1; shift ;;
    --build-id) build_id=${2:-}; shift 2 ;;
    --scorer) scorer=${2:-}; shift 2 ;;
    --list) list=${2:-}; shift 2 ;;
    --workers) workers=${2:-}; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    -*) echo "sweep: unknown option $1" >&2; usage >&2; exit 2 ;;
    *) if [ -z "$bin" ]; then bin=$1; elif [ -z "$out" ]; then out=$1; else echo "sweep: stray argument $1" >&2; exit 2; fi; shift ;;
  esac
done
[ -n "$bin" ] && [ -n "$out" ] || { usage >&2; exit 2; }
[ -x "$bin" ] || { echo "sweep: no executable binary at $bin" >&2; exit 2; }
[ -f "$list" ] || { echo "sweep: no list at $list" >&2; exit 2; }
case $workers in *[!0-9]* | '') echo "sweep: --workers wants a number" >&2; exit 2 ;; esac
[ "$workers" -ge 1 ] || workers=1
[ "$workers" -le 4 ] || workers=4
case $bin in /*) ;; *) bin=$PWD/$bin ;; esac
case $list in /*) ;; *) list=$PWD/$list ;; esac

# macOS: only with the explicit flag, and only when the mute switch is proven for this very binary.
if [ "$(uname -s)" = Darwin ]; then
  if [ "${SWEEP_ALLOW_MAC:-}" != 1 ]; then
    echo "sweep: refusing to run on macOS: the Mac must never play sound. Set SWEEP_ALLOW_MAC=1 to run muted." >&2
    exit 2
  fi
  if ! "$here/../media/sink-check.sh" "$bin" >&2; then
    echo "sweep: refusing to run on macOS: tests/media/sink-check.sh did not pass, so the muted switch is not verified." >&2
    exit 2
  fi
fi

mkdir -p "$out" || exit 2
out=$(cd "$out" && pwd)
state=$out/.state
rm -rf "$state"; mkdir -p "$state" || exit 2
rm -f "$out/MUTE-VIOLATION" "$out/results.tsv"

TMPDIR=${TMPDIR:-${WORK_DIR:-$(mktemp -d)}/tmp}
mkdir -p "$TMPDIR" 2>/dev/null
export TMPDIR

# The selection: lines of "id TAB category TAB url TAB note" turned into "id TAB host TAB url".
sel_file=$state/selected
awk -F'\t' -v ids="$sel_ids" -v cat="$sel_cat" -v prev="$sel_prev" '
  BEGIN { n = split(ids, a, ","); for (i = 1; i <= n; i++) want[a[i]] = 1 }
  /^#/ || NF < 3 { next }
  { if (ids != "" && !($1 in want)) next
    if (cat != "" && $2 != cat) next
    if (prev == 1 && $4 !~ /prev=1/) next
    print $1 "\t" $2 "\t" $3 }' "$list" > "$sel_file"
total=$(wc -l < "$sel_file" | tr -d ' ')
[ "$total" -gt 0 ] || { echo "sweep: the selection matched no page" >&2; exit 2; }

host_of() {
  h=${1#*://}; h=${h%%/*}; h=${h%%\?*}; h=${h%%:*}
  h=$(printf '%s' "$h" | tr 'A-Z' 'a-z')
  case $h in www.*) h=${h#www.} ;; esac
  # A file: page has no host; tab-separated fields must never be empty (a tab is IFS white space).
  [ -n "$h" ] || h=local
  printf '%s' "$h"
}

: > "$state/queue"
while IFS="$TAB" read -r q_id q_cat q_url; do
  printf '%s\t%s\t%s\t%s\n' "$q_id" "$(host_of "$q_url")" "$q_url" "$q_cat" >> "$state/queue"
done < "$sel_file"

fonts=${LUCINANET_FONT_DIRS:-/usr/share/fonts}
[ -d "$fonts" ] || fonts=/System/Library/Fonts
cap=${SWEEP_PAGE_CAP_S:-120}
gap=${SWEEP_HOST_GAP_S:-5}
tmo=""
if command -v timeout >/dev/null 2>&1; then tmo=timeout
elif command -v gtimeout >/dev/null 2>&1; then tmo=gtimeout; fi

# Run "$@" with stdout and stderr into $1, capped at $cap s. rc 124 = the cap fired. Sets run_rc.
run_capped() {
  rc_log=$1; shift
  rc_mark=$rc_log.cap
  rm -f "$rc_mark"
  if [ -n "$tmo" ]; then
    "$tmo" -k 10 "$cap" "$@" > "$rc_log" 2>&1 < /dev/null
    run_rc=$?
    [ "$run_rc" = 137 ] && run_rc=124
  else
    # No timeout command: a watchdog on the one process this script started.
    "$@" > "$rc_log" 2>&1 < /dev/null &
    rc_pid=$!
    ( sleep "$cap"
      if kill -0 "$rc_pid" 2>/dev/null; then
        : > "$rc_mark"; kill -TERM "$rc_pid" 2>/dev/null; sleep 10; kill -KILL "$rc_pid" 2>/dev/null
      fi ) > /dev/null 2>&1 &
    rc_wd=$!
    wait "$rc_pid"; run_rc=$?
    kill "$rc_wd" 2>/dev/null
    [ -f "$rc_mark" ] && run_rc=124
  fi
  rm -f "$rc_mark"
}

do_page() {
  p_id=$1; p_host=$2; p_url=$3; p_cat=$4
  p_dir=$out/$p_id
  rm -rf "$p_dir"; mkdir -p "$p_dir"
  # The politeness gap, measured from the end of the last page of this host.
  if [ -f "$state/last-$p_host" ]; then
    p_last=$(cat "$state/last-$p_host"); p_now=$(date -u +%s); p_wait=$((p_last + gap - p_now))
    [ "$p_wait" -gt 0 ] && sleep "$p_wait"
  fi
  {
    echo "id=$p_id"; echo "category=$p_cat"; echo "url=$p_url"; echo "start_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  } > "$p_dir/meta"
  p_t0=$(date -u +%s)
  # The same environment as tests/media/yt-nav-ev.sh, minus its page driver; see the header for the mute.
  export LUCINANET_MEDIA_SINK=none
  run_capped "$p_dir/log" env \
    RUST_LOG="${RUST_LOG:-error,lucinanet_media=info}" LUCINANET_MEDIA_SINK=none \
    LUCINANET_CONSOLE=1 LUCINANET_TIMING=1 LUCINANET_FONT_DIRS="$fonts" LUCINA_AUDIO_TEST_DB=-90 \
    LUCINANET_PROFILE_DIR=none LUCINANET_RENDER_QUIET_MS="${SWEEP_QUIET_MS:-4000}" \
    LUCINANET_RENDER_SETTLE_MAX_MS="${SWEEP_SETTLE_MAX_MS:-20000}" TMPDIR="$TMPDIR" \
    "$bin" --render "$p_url" "$p_dir/png" 1280x720 "$p_dir/dump"
  p_rc=$run_rc
  p_t1=$(date -u +%s)
  echo "$p_rc" > "$p_dir/rc"
  echo $(((p_t1 - p_t0) * 1000)) > "$p_dir/wall_ms"
  echo "$p_t1" > "$state/last-$p_host"
  # The mute check: any sound sink line other than "none" is a violation.
  if grep 'sound sink' "$p_dir/log" 2>/dev/null | grep -v 'sound sink none' > /dev/null; then
    echo "$p_id" >> "$out/MUTE-VIOLATION"
    echo "sweep: MUTE-VIOLATION on $p_id: the log names a sound sink that is not none" >&2
  fi
  if grep 'sound sink none' "$p_dir/log" > /dev/null 2>&1; then : > "$p_dir/.muted-media"; fi
  mkdir "$state/done-$p_id" 2>/dev/null
  p_n=$(ls "$state" | grep -c '^done-')
  p_png=missing; [ -f "$p_dir/png" ] && p_png=$(wc -c < "$p_dir/png" | tr -d ' ')
  printf '[%s/%s] %s %s rc=%s %ss png=%s\n' "$p_n" "$total" "$p_id" "$p_url" "$p_rc" $((p_t1 - p_t0)) "$p_png"
}

worker() {
  while :; do
    w_got=0; w_left=0
    [ -f "$out/MUTE-VIOLATION" ] && break
    while IFS="$TAB" read -r w_id w_host w_url w_cat; do
      [ -d "$state/claim-$w_id" ] && continue
      w_left=1
      mkdir "$state/lock-$w_host" 2>/dev/null || continue
      if mkdir "$state/claim-$w_id" 2>/dev/null; then
        w_got=1
        do_page "$w_id" "$w_host" "$w_url" "$w_cat"
        rmdir "$state/lock-$w_host" 2>/dev/null || echo "lock-$w_host gone before its owner released it" >> "$out/lock-warnings"
        break
      fi
      rmdir "$state/lock-$w_host" 2>/dev/null || echo "lock-$w_host gone before its owner released it" >> "$out/lock-warnings"
    done < "$state/queue"
    [ "$w_got" = 1 ] && continue
    [ "$w_left" = 0 ] && break
    sleep 1
  done
}

start=$(date -u +%s)
echo "sweep: $total pages, $workers workers, binary $bin, out $out, $(date -u +%Y-%m-%dT%H:%M:%SZ)"
i=0
while [ "$i" -lt "$workers" ]; do worker & i=$((i + 1)); done
wait
end=$(date -u +%s)

done_n=$(ls "$state" | grep -c '^done-')
bad=0; capped=0; muted=0
for d in "$out"/S*/; do
  [ -f "$d/rc" ] || continue
  r=$(cat "$d/rc")
  [ "$r" = 0 ] || bad=$((bad + 1))
  [ "$r" = 124 ] && capped=$((capped + 1))
  [ -f "$d/.muted-media" ] && muted=$((muted + 1))
done
viol=0; [ -f "$out/MUTE-VIOLATION" ] && viol=$(wc -l < "$out/MUTE-VIOLATION" | tr -d ' ')
echo "sweep: done $done_n of $total pages, rc!=0 $bad, timeouts $capped, pages with media on the null sink $muted, mute violations $viol, wall $((end - start)) s, out $out"
[ "$viol" = 0 ] || exit 3

# Step 5, the history helper: score, then keep results.tsv under a build id, and move `latest`.
if [ -n "$scorer" ] && [ -x "$scorer" ]; then
  "$scorer" score "$out" || { echo "sweep: the scorer failed" >&2; exit 4; }
  if command -v sha256sum >/dev/null 2>&1; then sha=$(sha256sum "$bin" | cut -c1-12); else sha=$(shasum -a 256 "$bin" | cut -c1-12); fi
  id=$sha; [ -n "$build_id" ] && id=$build_id-$sha
  hist=${SWEEP_HISTORY:-${WORK_DIR:-$(mktemp -d)}/lucinanet/sweep-results}
  dest=$hist/$id-$(date -u +%Y%m%dT%H%M%SZ)
  mkdir -p "$dest" && cp "$out/results.tsv" "$dest/results.tsv" || exit 4
  diff_rc=0
  if [ -f "$hist/latest/results.tsv" ]; then
    echo "sweep: diff against the previous build ($(readlink "$hist/latest"))"
    "$scorer" diff "$hist/latest/results.tsv" "$dest/results.tsv"; diff_rc=$?
  fi
  ln -sfn "$dest" "$hist/latest"
  echo "sweep: results kept in $dest (latest -> it)"
  [ "$diff_rc" = 0 ] || { echo "sweep: REGRESSION gate: a page went loaded -> broken (exit 1 of sweep-score diff)" >&2; exit 1; }
else
  echo "sweep: not scored (no --scorer). Run: sweep-score score $out"
fi
exit 0
