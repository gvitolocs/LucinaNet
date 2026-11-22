# Shared helpers for tests/acceptance (sourced, not executed). Bash.
# check_trace <log> <label> <required-props...>
#   Runs tools/media-trace-check ($TC) on <log>. Returns 0 only when the checker exits 0, its output has a non-empty
#   "properties checked:" list, the line count in "ok: no violation (N lines" is >= TRACE_MIN_LINES (default 200),
#   and every required property is in the checked list (exact name). Otherwise prints
#   "STEP-FAILED: <label>: <reason>" and returns 1. "not observed:" lines are printed (they never fail a step).
TRACE_MIN_LINES=${TRACE_MIN_LINES:-200}

# trace_checker <repo-root>: print the path of the built media-trace-check (building it first if needed).
trace_checker() {
  local root=$1 tc=""
  [ -n "${CARGO_TARGET_DIR:-}" ] && tc=$CARGO_TARGET_DIR/release/media-trace-check
  cargo build --offline --release --manifest-path "$root/tools/media-trace-check/Cargo.toml" >&2 || return 1
  [ -n "$tc" ] && [ -x "$tc" ] || tc=$root/tools/media-trace-check/target/release/media-trace-check
  [ -x "$tc" ] && echo "$tc"
}

check_trace() {
  local log=$1 label=$2; shift 2
  local out rc n props p
  if [ ! -f "$log" ]; then echo "STEP-FAILED: $label: log missing ($log)"; return 1; fi
  if [ -z "${TC:-}" ] || [ ! -x "$TC" ]; then echo "STEP-FAILED: $label: media-trace-check binary not available"; return 1; fi
  out=$("$TC" "$log" 2>&1); rc=$?
  printf "%s\n" "$out" | grep "^not observed:" | sed "s/^/  [$label] /" | cut -c1-200
  if [ $rc -ne 0 ]; then
    echo "STEP-FAILED: $label: checker exit $rc: $(printf "%s\n" "$out" | head -1 | cut -c1-200)"; return 1
  fi
  if printf "%s\n" "$out" | grep -q "properties checked: (none)"; then
    echo "STEP-FAILED: $label: properties checked: (none)"; return 1
  fi
  n=$(printf "%s\n" "$out" | sed -n "s/^ok: no violation (\([0-9][0-9]*\) lines.*/\1/p" | head -1)
  if [ -z "$n" ]; then echo "STEP-FAILED: $label: no \"ok: no violation (N lines\" line in the checker output"; return 1; fi
  if [ "$n" -lt "$TRACE_MIN_LINES" ]; then
    echo "STEP-FAILED: $label: trace too short ($n lines < $TRACE_MIN_LINES)"; return 1
  fi
  props=$(printf "%s\n" "$out" | sed -n "s/^ok: no violation (.*properties checked: \(.*\))\$/\1/p" | head -1)
  for p in "$@"; do
    case ", $props," in
      *", $p,"*) ;;
      *) echo "STEP-FAILED: $label: required property $p not in checked list ($props)"; return 1 ;;
    esac
  done
  echo "trace ok: $label ($n lines; $props)"
  return 0
}

# the test and measurement scripts drive the browser through doors that only a `harness` build has (`--render`, the
# environment's user script, style sheet and preferences: docs/harness-doors.md). Against a normal build those variables are
# not read and `--render` is refused, so a run would silently measure nothing. A harness build carries this marker in its
# version line (crates/lucinanet-ui/src/lib.rs VERSION_LINE).
HARNESS_MARKER=LUCINANET-HARNESS-BUILD

# is_harness_binary <binary>: true when the file holds the marker.
is_harness_binary() {
  [ -f "$1" ] && grep -aqF -- "$HARNESS_MARKER" "$1"
}

# require_harness_binary <binary> [label]: returns 0 for a harness build; otherwise prints one "STEP-FAILED: <label>: ..." line
# that says how to build one (tools/build-harness.sh) and returns 1.
require_harness_binary() {
  local bin=$1 label=${2:-harness}
  if [ ! -f "$bin" ]; then echo "STEP-FAILED: $label: no binary at $bin"; return 1; fi
  if ! is_harness_binary "$bin"; then
    echo "STEP-FAILED: $label: $bin is not a harness build, so --render and the LUCINANET_USER_JS / _USER_CSS / _PREFS doors do nothing in it; build one with tools/build-harness.sh --run (tests/acceptance/README.md)"
    return 1
  fi
  return 0
}
