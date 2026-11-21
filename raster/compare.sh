#!/usr/bin/env bash
# Replay every trace on the C++ oracle (strict swgl) and the Rust rasterizer with the same gltrace binary
# (built with SWGL_STRICT_FP=1: --backend cxx is strict swgl, --backend rust is lucinanet-raster), then diff frames.
set -uo pipefail

# Set WORK_DIR to a scratch directory holding the recorded traces and the built binaries.
WORK_DIR=${WORK_DIR:-$(mktemp -d)}

TRACES_DIR=$WORK_DIR/lucinanet-raster/traces
OUT_DIR=
THREADS=
TOLERANCE=0
PAGES=()

GLTRACE=${GLTRACE:-$WORK_DIR/lucinanet-raster/target-s-strict/release/gltrace}
PIXDIFF=${PIXDIFF:-$WORK_DIR/lucinanet-raster/target-pixdiff/release/pixdiff}

usage() {
  cat <<'EOF'
Usage: raster/compare.sh [--traces DIR] [--out DIR] [--threads N] [--tolerance N] [page ...]

  --traces DIR       directory of trace pages (default: $WORK_DIR/lucinanet-raster/traces)
  --out DIR          output directory (default: $WORK_DIR/lucinanet-raster/compare/<UTC timestamp>)
  --threads N        the Rust replay draws with N threads on CPUs 4..3+N (the C++ oracle has one thread)
  --tolerance N      pixdiff tolerance (default: 0)
  page ...           pages to run (default: every subdirectory of --traces, sorted)

Environment overrides: GLTRACE (one binary built with SWGL_STRICT_FP=1, both backends), PIXDIFF, MAX_TCTL (default 97).
EOF
}

while [[ $# -gt 0 ]]; do
  case $1 in
    --traces) TRACES_DIR=${2:?--traces needs a value}; shift 2 ;;
    --out) OUT_DIR=${2:?--out needs a value}; shift 2 ;;
    --threads) THREADS=${2:?--threads needs a value}; shift 2 ;;
    --tolerance) TOLERANCE=${2:?--tolerance needs a value}; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    --*) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    *) PAGES+=("$1"); shift ;;
  esac
done

[[ -z $THREADS || $THREADS =~ ^[0-9]+$ ]] || { echo "--threads must be a positive integer" >&2; exit 2; }
[[ $TOLERANCE =~ ^[0-9]+$ ]] || { echo "--tolerance must be a non-negative integer" >&2; exit 2; }

for bin in "$GLTRACE" "$PIXDIFF"; do
  if [[ ! -x $bin ]]; then
    echo "error: required binary missing or not executable: $bin" >&2
    exit 1
  fi
done

if [[ ! -d $TRACES_DIR ]]; then
  echo "error: traces directory not found: $TRACES_DIR" >&2
  exit 1
fi

if [[ ${#PAGES[@]} -eq 0 ]]; then
  while IFS= read -r d; do
    PAGES+=("$(basename "$d")")
  done < <(find "$TRACES_DIR" -mindepth 1 -maxdepth 1 -type d | sort)
fi

if [[ -z $OUT_DIR ]]; then
  OUT_DIR="$WORK_DIR/lucinanet-raster/compare/$(date -u +%Y%m%d-%H%M%S)"
fi
mkdir -p "$OUT_DIR"
SUMMARY="$OUT_DIR/summary.txt"
: > "$SUMMARY"

wait_for_temp() {
  local limit=${MAX_TCTL:-97} t
  while :; do
    t=$(sensors | awk '/Tctl/{gsub(/[+°C]/,"",$2); print int($2); exit}')
    if [[ -z $t || $t -lt $limit ]]; then
      return 0
    fi
    sleep 20
  done
}

sum_ms() {
  awk '$1 == "frame" && ($4 + 0) > 0 { s += $6 } END { printf "%.3f", s + 0 }' "$1"
}

overall=0
for page in "${PAGES[@]}"; do
  page_dir=$TRACES_DIR/$page
  if [[ ! -d $page_dir ]]; then
    echo "error: page not found: $page_dir" >&2
    overall=1
    continue
  fi
  trace=$(find "$page_dir" -maxdepth 1 -name 'trace-*.bin' | sort | head -n 1)
  if [[ -z $trace ]]; then
    echo "$page FAIL (no trace-*.bin in $page_dir)" | tee -a "$SUMMARY"
    overall=1
    continue
  fi

  wait_for_temp

  base=$OUT_DIR/$page
  mkdir -p "$base/cxx" "$base/rust"

  # The C++ oracle has one thread. The Rust replay gets --threads N and as many CPUs (4 onwards, at most 4-11).
  thread_args=()
  rust_cpus=5
  if [[ -n $THREADS ]]; then
    thread_args=(--threads "$THREADS")
    last_cpu=$((3 + THREADS))
    ((last_cpu > 11)) && last_cpu=11
    rust_cpus=4-$last_cpu
  fi

  taskset -c 5 nice -n 10 "$GLTRACE" replay "$trace" --out "$base/cxx" --backend cxx > "$base/cxx.log"
  cxx_rc=$?
  taskset -c "$rust_cpus" nice -n 10 "$GLTRACE" replay "$trace" --out "$base/rust" --backend rust "${thread_args[@]}" > "$base/rust.log"
  rust_rc=$?

  cxx_ms=$(sum_ms "$base/cxx.log")
  rust_ms=$(sum_ms "$base/rust.log")

  if [[ $cxx_rc -ne 0 || $rust_rc -ne 0 ]]; then
    rc=$cxx_rc
    [[ $rust_rc -ne 0 ]] && rc=$rust_rc
    line="$page cxx $cxx_ms ms rust $rust_ms ms FAIL (replay $rc)"
    echo "$line" | tee -a "$SUMMARY"
    overall=1
    continue
  fi

  "$PIXDIFF" "$base/cxx" "$base/rust" --heat "$base/heat" --tolerance "$TOLERANCE" > "$base/pixdiff.txt" 2>&1
  pix_rc=$?
  last=$(tail -n 1 "$base/pixdiff.txt")
  read -r _ frames _ identical _ _ within _ worse _ <<< "$last"
  speed=$(awk -v c="$cxx_ms" -v r="$rust_ms" 'BEGIN { if (r > 0) printf "%.2f", c / r; else print "inf" }')

  if [[ $pix_rc -eq 0 ]]; then
    verdict=PASS
  else
    verdict=FAIL
    overall=1
  fi
  line="$page cxx $cxx_ms ms rust $rust_ms ms speed ${speed}x frames ${frames:-0} identical ${identical:-0} within ${within:-0} worse ${worse:-0} $verdict"
  echo "$line" | tee -a "$SUMMARY"
done

exit $overall
