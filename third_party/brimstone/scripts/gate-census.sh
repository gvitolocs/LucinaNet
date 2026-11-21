#!/bin/bash
# Permanent-space census check. Runs from the repo root on the working tree, debug only (the census
# walks the permanent space through the verifier's item walk, which is built for debug assertions —
# the same reason gate-gc-verify.sh refuses --release).
#
# What it asserts: the GC proof relies on a premise about the *whole heap* — no list-kind item (WeakRef,
# WeakMap/WeakSet, FinalizationRegistry, prototype children vec, inline cache) ever sits in the permanent
# space, because nothing moves a permanent item and so no collection ever repairs its weak slots. A diff
# cannot be shown to touch that premise, so this check runs on every change.
set -euo pipefail
cd "$(dirname "$0")/.."

# Set WORK_DIR to a scratch directory. CARGO_TARGET_DIR is the caller's when set, otherwise a target
# directory under it.
: "${CARGO_TARGET_DIR:=${WORK_DIR:-$(mktemp -d)}/brim-gate-target}"
export CARGO_TARGET_DIR

# The census test in src/js/runtime/embed/tests.rs (package brimstone_core). Matched as a substring so
# neither the gc_verify_ prefix nor the module path is hardcoded here: step (a) resolves the full path
# from --list, step (b) runs exactly that path with --exact.
CENSUS_SUBSTRING="the_permanent_space_holds_no_item_whose_weak_slots_only_a_list_repairs"

# step_capture: gate-gc-verify.sh's step(), except that both steps here have to read the output — one
# for the test name, one for the count. The whole output is printed either way, so a compile error or a
# failing assertion is visible in the output.
CAPTURED=""
step_capture() {
  local label="$1"
  shift
  echo "gate-census: $label"
  if ! CAPTURED=$("$@" 2>&1); then
    printf '%s\n' "$CAPTURED" >&2
    echo "gate-census: $label: FAILED" >&2
    exit 1
  fi
  printf '%s\n' "$CAPTURED"
}

# (a) The guard. A filtered cargo test run of a name that does not exist *passes* — it reports
# "0 passed; N filtered out" and exits 0 — so asking for the test after filtering it would prove
# nothing about a tree that has no census test at all. So ask --list (unfiltered) first, and fail if
# no listed test carries that name. Without this, the check would pass vacuously on a tree that does
# not contain the census test yet.
step_capture "cargo test -p brimstone_core -- --list" \
  cargo test --quiet -p brimstone_core -- --list

CENSUS_PATH=$(printf '%s\n' "$CAPTURED" \
  | grep -F "$CENSUS_SUBSTRING" \
  | grep -F -m1 ': test' \
  | sed -e 's/: test$//' -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' || true)
if [ -z "$CENSUS_PATH" ]; then
  echo "check would be vacuous: the permanent-space census test does not exist in this tree" >&2
  echo "  no --list entry names *$CENSUS_SUBSTRING*" >&2
  exit 1
fi
echo "gate-census: census test found in --list as $CENSUS_PATH"

# (b) Run it by that exact name and require the one test to pass. " 1 passed;" is the whole claim: 0
# passed is a failure, 2 passed would mean the filter matched something else and the check is no
# longer naming one test. The space and the semicolon so a "11 passed" line cannot satisfy it either.
step_capture "cargo test -p brimstone_core $CENSUS_PATH -- --exact" \
  cargo test --quiet -p brimstone_core "$CENSUS_PATH" -- --exact
if ! printf '%s\n' "$CAPTURED" | grep -q " 1 passed;"; then
  echo "gate-census: census run did not report a single passing test: FAILED" >&2
  exit 1
fi

echo "gate-census: all steps ok ($SECONDS s)"
