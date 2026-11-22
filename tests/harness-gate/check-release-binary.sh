#!/usr/bin/env bash
# prove from the BUILT binary that the harness doors are not in it. Static: no run, no network, no display.
#
#   tests/harness-gate/check-release-binary.sh <binary>             a normal (non-harness) build: none of the names may be in it
#   tests/harness-gate/check-release-binary.sh --harness <binary>   the positive control: a build with `--features harness`
#                                                                   must contain every one of them, and the marker
#   tests/harness-gate/check-release-binary.sh --harness --desktop <binary>   the same for a build WITH the desktop feature:
#                                                                   LUCINANET_SCRIPT is read on the non-desktop startup path only
#                                                                   (lib.rs `run`), so a desktop harness build does not hold it
#
# The names are the doors of docs/harness-doors.md: the environment variables that name a user script, a style sheet, preferences,
# a start script, a load list, a hold time or an accessibility socket path, and the command-line forms that start a request
# without a window or probe the network. When a door is compiled out its name is not a string in the program at all, which is
# what this checks (a variable that is merely ignored would still be spelled in the binary).
# The marker LUCINANET-HARNESS-BUILD is the harness build's version line: it must be absent from a normal build.
#
# Exit 0 = the expectation holds. Exit 1 = it does not (the names found or missing are printed). Exit 2 = a usage error.
set -u

NAMES=(LUCINANET_USER_JS LUCINANET_USER_CSS LUCINANET_PREFS LUCINANET_SCRIPT LUCINANET_LOADS LUCINANET_RENDER_HOLD LUCINANET_AX
       --netprobe --udpdns --render)
MARKER=LUCINANET-HARNESS-BUILD
# names that must be absent from EVERY build, the harness build included (the harness build does not enable the glue's
# `env-config` feature): the engine's function cache directory is not read from the environment, so its names are not strings in
# the program at all. LUCINANET_JS_FN_CACHE (the opt-in switch we read ourselves) is not on this list, and is not a prefix check:
# the whole name with its suffix is searched.
ALWAYS_ABSENT=(LUCINANET_JS_FN_CACHE_DIR LUCINANET_JS_FN_CACHE_MB)

mode=release; desktop=no
while [ "${1:-}" = --harness ] || [ "${1:-}" = --desktop ]; do
  case $1 in --harness) mode=harness ;; --desktop) desktop=yes ;; esac
  shift
done
bin=${1:-}
[ $# -eq 1 ] && [ -f "$bin" ] && [ -r "$bin" ] || { echo "usage: $0 [--harness] [--desktop] <binary>" >&2; exit 2; }

present() { grep -aqF -- "$1" "$bin"; }

bad=0
for name in "${NAMES[@]}" "$MARKER"; do
  # a desktop harness build has no script thread: the name is required absent from a release build only
  if [ "$mode" = harness ] && [ "$desktop" = yes ] && [ "$name" = LUCINANET_SCRIPT ]; then echo "skip  $name (desktop build: non-desktop startup path only)"; continue; fi
  if present "$name"; then found=yes; else found=no; fi
  if [ "$mode" = release ]; then
    if [ "$found" = yes ]; then echo "FAIL  $name is in $bin (a normal build must not contain it)"; bad=$((bad + 1)); else echo "ok    $name absent"; fi
  else
    if [ "$found" = no ]; then echo "FAIL  $name is not in $bin (a harness build must contain it, or this check proves nothing)"; bad=$((bad + 1)); else echo "ok    $name present"; fi
  fi
done

for name in "${ALWAYS_ABSENT[@]}"; do
  if present "$name"; then echo "FAIL  $name is in $bin (no build may contain it: the function cache directory does not come from the environment)"; bad=$((bad + 1)); else echo "ok    $name absent (every build)"; fi
done

size=$(wc -c <"$bin" | tr -d ' ')
if [ "$bad" -eq 0 ]; then
  echo "PASS  check-release-binary mode=$mode binary=$bin bytes=$size names=$(( ${#NAMES[@]} + 1 + ${#ALWAYS_ABSENT[@]} ))"
  exit 0
fi
echo "FAIL  check-release-binary mode=$mode binary=$bin bytes=$size failures=$bad"
exit 1
