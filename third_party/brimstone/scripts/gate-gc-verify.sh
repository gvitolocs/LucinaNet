#!/bin/bash
# GC verifier checks. Runs from the repo root on the working tree. The heap verifier steps (the
# gc_verify unit tests and the BS_GC_VERIFY harness subsets) are debug builds: the verifier is built
# for debug assertions and these checks must not hide it behind --release. The last steps (d) are
# release builds with `--features gc_stress_test`, the compile-time from-space poisoning that the
# public workflow also uses.
set -euo pipefail
cd "$(dirname "$0")/.."

# Set WORK_DIR to a scratch directory. CARGO_TARGET_DIR is the caller's when set, otherwise a target
# directory under it.
: "${CARGO_TARGET_DIR:=${WORK_DIR:-$(mktemp -d)}/brim-gate-target}"
export CARGO_TARGET_DIR

step() {
  local label="$1"
  shift
  echo "gate-gc-verify: $label"
  if ! "$@"; then
    echo "gate-gc-verify: $label: FAILED" >&2
    exit 1
  fi
  echo "gate-gc-verify: $label: ok"
}

# Guard 1: the switch these checks exercise lives in the harness; a tree without it would let them
# pass while testing nothing.
if ! grep -q BS_GC_VERIFY tests/harness/src/runner.rs; then
  echo "check would be vacuous: tests/harness/src/runner.rs does not read BS_GC_VERIFY" >&2
  exit 1
fi

# Guard 2: the same for the test262 checkout. The harness indexes the whole test262 suite and loads
# assert.js/sta.js from it for every test it runs, so without the checkout at the pinned commit step
# (c) cannot run at all — and a check that quietly ran less than it claims proves nothing either.
# The checkout is shared and read-only, outside any worktree; it is gitignored inside the worktree, so
# a per-worktree clone would mean a 375 MB clone per run. Same commit as install_test262.sh, read from
# the script so the two cannot drift apart.
TEST262_SHA=$(grep -m1 '^TEST262_COMMIT_SHA=' tests/test262/install_test262.sh | cut -d'"' -f2)
TEST262_DIR="${BRIMSTONE_TEST262:-${WORK_DIR:-$(mktemp -d)}/test262-$TEST262_SHA}"
if [ ! -d "$TEST262_DIR" ] || [ "$(git -C "$TEST262_DIR" rev-parse HEAD 2>/dev/null || true)" != "$TEST262_SHA" ]; then
  echo "check would be vacuous: pinned test262 checkout missing or not at the pinned commit" >&2
  echo "  wanted $TEST262_SHA at $TEST262_DIR; BRIMSTONE_TEST262 overrides the location" >&2
  exit 1
fi

# The harness finds the checkout at the manifest's "test262_repo_path" (tests/test_manifest.jsonc:
# tests/test262/test262, gitignored), so point that path at the shared dir. Created only when
# absent; the shared checkout itself is never written.
if [ ! -e tests/test262/test262 ]; then
  ln -s "$TEST262_DIR" tests/test262/test262
fi

# The verifier's unit tests (src/js/runtime/embed/tests.rs, package brimstone_core); they enable the
# verifier themselves, so they hold in a plain cargo test too — BS_GC_VERIFY is set anyway so the run
# matches what the check is for.
step "cargo test -p brimstone_core gc_verify" \
  env BS_GC_VERIFY=1 cargo test --quiet -p brimstone_core gc_verify

# (c) The harness over GC-reachable shapes and weak edges, verifier on and a 4096-byte allocation
# window so nearly every allocation crosses a collection. Each entry is the harness filter string,
# a substring of the test path relative to its suite root (tests/harness/src/runner.rs), and with no
# -s it runs that path in every suite that has it: first-party under tests/integration, test262
# under the pinned checkout. The names are the weak-reference and shape-transition directories,
# chosen for what they exercise, not for coverage.
GC_SUBSET=()
# IsHTMLDDA markers: the only objects whose named walk keeps them alive through a transition, and
# the case the weak-edge repair exists for (tests/integration/built-ins/IsHTMLDDA, first-party only).
GC_SUBSET+=("built-ins/IsHTMLDDA")
# First-party regressions, one file per fixed bug: the whole engine under a stress window
# (tests/integration/regression, first-party only).
GC_SUBSET+=("regression")
# Weak map edges: key holds weakly, value strongly; a missed edge frees a live key.
GC_SUBSET+=("built-ins/WeakMap")
# Weak set edges: the value itself is the weak edge.
GC_SUBSET+=("built-ins/WeakSet")
# WeakRef cells: deref/ClearWeakRef, the cells the verifier reports on and repairs.
GC_SUBSET+=("built-ins/WeakRef")
# FinalizationRegistry cells plus the QueuesAsFinalization registry jobs: weak held target with a
# strongly held callback, and the job queue that keeps markers alive.
GC_SUBSET+=("built-ins/FinalizationRegistry")
# Redefining properties: shape and property-table transitions, where the named walks miss edges; the
# largest directory here, so it is also the slowest step.
GC_SUBSET+=("built-ins/Object/defineProperty")

first=1
for filter in "${GC_SUBSET[@]}"; do
  # --reindex on the first run: tests/test_index.json is gitignored and the index has to be built
  # from this tree plus the pinned checkout before the first filter can match anything; it is cheap
  # (~11 s) so it is rebuilt on every run instead of trusted from a stale tree.
  if [ "$first" = 1 ]; then
    first=0
    step "cargo btest --reindex $filter" \
      env BS_GC_VERIFY=1 BS_GC_STRESS=4096 cargo btest -- --reindex "$filter"
  else
    step "cargo btest $filter" \
      env BS_GC_VERIFY=1 BS_GC_STRESS=4096 cargo btest -- "$filter"
  fi
done

# (d) The compile-time poisoning mode (`--features gc_stress_test`, release, the same build the public
# gc-stress job uses) over the directories that cover `with` scopes and direct eval: the dynamic
# reference instructions run script inside a scope lookup, and only a poisoned from-space
# shows a bytecode operand read after a collection. About 4 s each plus the feature build. The third
# filter is the first-party test of the MakeDynamicRef / LoadDynamicRef handlers; it lives under
# tests/integration/language/scope, which neither directory filter matches, and `-s integration`
# keeps the step on that suite.
# The full `--features gc_stress_test` run (10+ minutes) is deliberately NOT in this script. It is
# the matrix entry `gc-stress` (args `--release --features gc_stress_test`, shown as
# "integration (gc-stress)") of the job `integration` in `rust-ci.yml` of the public repository
# gvitolocs/LucinaJS, which runs on every branch and every pull request. Do not remove that entry on
# the assumption that this script covers it. The upstream `.github/workflows/ci.yml` in this tree
# (job `integration-tests-gc-stress-test`) is the vendored engine's own workflow and triggers on
# `master`.
step "cargo btest --release --features gc_stress_test language/statements/with" \
  cargo btest --release --features gc_stress_test -- language/statements/with
step "cargo btest --release --features gc_stress_test language/eval-code" \
  cargo btest --release --features gc_stress_test -- language/eval-code
step "cargo btest --release --features gc_stress_test language/scope/dynamic_ref_allocating_lookup" \
  cargo btest --release --features gc_stress_test -- -s integration language/scope/dynamic_ref_allocating_lookup

echo "gate-gc-verify: all steps ok ($SECONDS s)"
