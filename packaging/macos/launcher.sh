#!/bin/sh
# LucinaNet.app/Contents/MacOS/lucinanet — the bundle's CFBundleExecutable. It is the only thing LaunchServices runs,
# so this is where desktop mode and a URL given as an argument are added: `open -a LucinaNet --args <url>` and
# `LucinaNet.app/Contents/MacOS/lucinanet <url>` both reach the engine as `--desktop <url>`.
# @BIN_NAME@ is filled in by tools/release/package-macos.sh: the engine executable sitting beside this script.
#
# The engine reads its bundled fonts relative to its own path (crates/lucinanet-ui/src/fonts.rs, install_font_dir),
# so this script must exec the real binary in place, not a copy moved elsewhere.
#
# Deliberately NOT set here: LUCINANET_FRAME_LOG, LUCINANET_RENDER_STATS, LUCINANET_WR_PHASES,
# LUCINANET_LAYOUT_PHASES, LUCINANET_RENDERING_REASON, LUCINANET_JS_GC_LOG, LUCINANET_CHROME_LOG. The launcher of the
# installed bundle exports them for a navigation-fluidity session (2026-11-19); a packager's default must not turn
# per-frame and per-GC tracing on for every ordinary start. Set them in the environment to reproduce that session.
here=$(cd "$(dirname "$0")" && pwd)
bin="$here/@BIN_NAME@"
[ -x "$bin" ] || { echo "lucinanet: $bin is missing or not executable" >&2; exit 127; }

# Page errors and missing JS features of each run, kept on this machine only.
logs="$HOME/Library/Logs/LucinaNet"
stamp=$(date +%Y%m%d-%H%M%S)
if mkdir -p "$logs" 2>/dev/null; then
	export LUCINANET_CONSOLE=1
	export LUCINANET_JS_MISSING_LOG="$logs/missing-$stamp.log"
	exec "$bin" --desktop "$@" >> "$logs/session-$stamp.log" 2>&1
fi
exec "$bin" --desktop "$@"
