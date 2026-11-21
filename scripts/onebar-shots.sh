#!/bin/bash
# The acceptance pictures of docs/design/one-bar.md: the one-row bar, both chromes, both scales, in the six states the
# design lists, written by the UI crate's own shot path (crates/lucinanet-ui/src/shot.rs). Each state is measured as it is
# drawn and the numbers go in the manifest; the test asserts criteria 1, 3, 6 and 7 while it writes, so a picture that
# stops matching the design fails this script instead of quietly lying.
#
# They land in the scratch directory by default so a re-run cannot dirty the tree; the committed set is
# docs/design/one-bar/, refreshed by copying this script's output over it. See docs/design/one-bar-acceptance.md for what
# the pictures are, and for the four things they do not show.
#
# The pictures come from the harness, not from a live Servo: one headless `--render` load has one tab, types nothing and
# asks nothing, and four of the six states are not reachable that way.
#
# Usage: scripts/onebar-shots.sh [directory]     default $WORK_DIR/lucinanet-one-bar/shots (WORK_DIR: a scratch directory)
set -uo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-${WORK_DIR:-$(mktemp -d)}/lucinanet-one-bar/shots}
mkdir -p "$out" || exit 1
cd "$root" || exit 1
echo "onebar-shots: writing into $out"
if ! LUCINANET_ONEBAR_SHOTS="$out" cargo test -p lucinanet-ui --offline --lib -- the_one_bar_pictures --ignored; then
    echo "onebar-shots: the pictures failed their own checks" >&2
    exit 1
fi
echo
cat "$out/manifest.txt"
echo
ls -1 "$out" | wc -l | xargs echo "onebar-shots: files in $out:"
sha256sum "$out"/*.png | sha256sum | xargs echo "onebar-shots: pictures sha256"
