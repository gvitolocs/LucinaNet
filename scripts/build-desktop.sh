#!/bin/bash
# A desktop LucinaNet build for x86_64 Linux: desktop window and media. PROFILE=probe (default) or juno.
[ -n "$LUCINANET_BUILD_LOCKED" ] || { exec 9>/tmp/lucinanet-servo-build.lock; flock 9; }
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
export CARGO_TARGET_DIR=${WORK_DIR:-$(mktemp -d)}/lucinanet/target PYTHON3=python3 CC=clang CXX=clang++
# `media` installs crates/lucinanet-media as Servo's media backend: without it servo-media runs its
# dummy and a page says it cannot play the video. Same features as the Mac build, packaging/macos/README.md.
nice -n 15 cargo build -j 6 --profile ${PROFILE:-probe} -p lucinanet-servo --features desktop,media
