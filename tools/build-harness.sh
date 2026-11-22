#!/bin/bash
# the one documented way to build the test and measurement binary, the one with the doors (cargo feature `harness`:
# `--render`, LUCINANET_USER_JS / _USER_CSS / _PREFS / _SCRIPT / _LOADS / _RENDER_HOLD, the AX socket path variable, the
# diagnostic subcommands; docs/harness-doors.md). The shipped browser is built WITHOUT it and cannot do any of that.
#
#   tools/build-harness.sh                       print the command and where the binary lands; build nothing
#   tools/build-harness.sh --run                 print it, run it, then check the binary says it is a harness build
#   tools/build-harness.sh --profile release|probe|dev    probe is the default, the profile the scripts look in
#   tools/build-harness.sh --target-dir DIR      default $CARGO_TARGET_DIR, else $WORK_DIR/lucinanet-harness under
#                                                the checkout's target/ (WORK_DIR: where you keep build trees)
#   tools/build-harness.sh --features LIST       default desktop,media (harness is always added)
#
# The binary is a MEASUREMENT build: never install it over the maintainer's app, package it or publish it. tools/release/ refuses it.
# It shares the output name `lucinanet-servo` with a normal build of the same profile, so the target directory holds whichever
# was built last: every script that needs the doors checks the marker (tests/acceptance/lib.sh `require_harness_binary`).
# Cargo alias for the same command in the default profile: `cargo build-harness` (.cargo/config.toml).
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
profile=probe; features=desktop,media; run=0
target=${CARGO_TARGET_DIR:-${WORK_DIR:-$ROOT/target}/lucinanet-harness}
while [ $# -gt 0 ]; do
  case $1 in
    --run) run=1; shift ;;
    --profile) profile=${2:?--profile needs a value}; shift 2 ;;
    --target-dir) target=${2:?--target-dir needs a value}; shift 2 ;;
    --features) features=${2:?--features needs a value}; shift 2 ;;
    -h|--help) sed -n '2,/^set -u/p' "$0" | sed '$d'; exit 0 ;;
    *) echo "build-harness: unknown argument $1 (see --help)" >&2; exit 2 ;;
  esac
done
case $profile in dev) dir=debug ;; release|probe|juno) dir=$profile ;; *) echo "build-harness: unknown profile $profile" >&2; exit 2 ;; esac
case ",$features," in *,harness,*) ;; *) features=$features,harness ;; esac
profile_arg=(--profile "$profile")
[ "$profile" = dev ] && profile_arg=()
bin=$target/$dir/lucinanet-servo
cmd=(cargo build --offline "${profile_arg[@]}" -p lucinanet-servo --features "$features")
echo "harness build: cd $ROOT && CARGO_TARGET_DIR=$target CC=clang CXX=clang++ PYTHON3=python3 ${cmd[*]}"
echo "binary: $bin   (a measurement build: never install, package or publish it)"
[ "$run" = 1 ] || { echo "(not run: add --run)"; exit 0; }
cd "$ROOT" || exit 1
CARGO_TARGET_DIR=$target CC=clang CXX=clang++ PYTHON3=python3 "${cmd[@]}" || exit 1
# shellcheck source=../tests/acceptance/lib.sh
. "$ROOT/tests/acceptance/lib.sh"
require_harness_binary "$bin" build-harness || exit 1
echo "harness build ok: $bin"
