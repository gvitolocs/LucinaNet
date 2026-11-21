#!/bin/bash
# LucinaNet for the build host's desktop: x86_64, desktop window and media. PROFILE=probe (default) or juno.
[ -n "$LUCINANET_BUILD_LOCKED" ] || { exec 9>/tmp/lucinanet-servo-build.lock; flock 9; }
cd /home/user/Projects/Lucina/LucinaNet
export CARGO_TARGET_DIR=${WORK_DIR:-$(mktemp -d)}/lucinanet/target PYTHON3=python3 CC=clang CXX=clang++
# `media` installs crates/lucinanet-media as Servo's media backend: without it servo-media runs its
# dummy and a page says it cannot play the video. Same features as the Mac build, docs/mac-tree.md.
nice -n 15 cargo build -j 6 --profile ${PROFILE:-probe} -p lucinanet-servo --features desktop,media
