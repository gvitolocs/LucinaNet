#!/usr/bin/env bash
# tests for check-release-binary.sh and the refusal of tools/release/harness-guard.sh, on stub files (no build).
# Run: tests/harness-gate/test-check-release-binary.sh        (exit 0 = everything passed)
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
CHECK=$ROOT/tests/harness-gate/check-release-binary.sh
BUILD=$ROOT/tools/release/build-linux-desktop.sh
T=$(mktemp -d "${TMPDIR:-/tmp}/ln-harness-gate.XXXXXX")
trap 'rm -rf "$T"' EXIT
PASS=0; FAILED=0
ok()  { printf 'ok    %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; FAILED=$((FAILED + 1)); }
check()  { local l=$1; shift; if "$@" >/dev/null 2>&1; then ok "$l"; else bad "$l"; fi; }
ncheck() { local l=$1; shift; if ! "$@" >/dev/null 2>&1; then ok "$l"; else bad "$l"; fi; }

NAMES=(LUCINANET_USER_JS LUCINANET_USER_CSS LUCINANET_PREFS LUCINANET_SCRIPT LUCINANET_LOADS LUCINANET_RENDER_HOLD LUCINANET_AX
       --netprobe --udpdns --render)
# a normal build's stand-in: binary noise and names that are NOT doors (the neighbours the gate leaves alone)
printf '\x7fELF....LUCINANET_CONSOLE\0LUCINANET_CSS_PREFS\0LUCINANET_COMPAT_SHIMS\0LUCINANET_PROFILE_DIR\0--desktop\0--version\0lucinanet 0.1.0\0' >"$T/clean"
{ cat "$T/clean"; for n in "${NAMES[@]}"; do printf '%s\0' "$n"; done; printf 'lucinanet 0.1.0 harness build (LUCINANET-HARNESS-BUILD: for tests)\0'; } >"$T/harness"

check  "a clean binary passes"                              "$CHECK" "$T/clean"
ncheck "a clean binary fails the positive control"          "$CHECK" --harness "$T/clean"
ncheck "a harness binary fails the release check"           "$CHECK" "$T/harness"
check  "a harness binary passes the positive control"       "$CHECK" --harness "$T/harness"
# a desktop harness build has no script thread (LUCINANET_SCRIPT is on the non-desktop startup path only)
{ cat "$T/clean"; for n in "${NAMES[@]}"; do [ "$n" = LUCINANET_SCRIPT ] || printf "%s\0" "$n"; done; printf "lucinanet harness LUCINANET-HARNESS-BUILD\0"; } >"$T/harness-desktop"
check  "a desktop harness stand-in passes the desktop positive control" "$CHECK" --harness --desktop "$T/harness-desktop"
ncheck "... but not the plain one (it lacks LUCINANET_SCRIPT)"          "$CHECK" --harness "$T/harness-desktop"
ncheck "a harness stand-in fails the release check" "$CHECK" "$T/harness-desktop"
# the Windows form: windows.yml runs the same check, in bash, on the lucinanet-servo.exe of the shipped build. A PE file holds the names as
# plain strings like an ELF does, so the check needs no Windows mode; these stubs start with the MZ header of a PE file
{ printf "MZ\220\0\3\0\0\0This program cannot be run in DOS mode.\0"; tail -c +5 "$T/clean"; } >"$T/clean.exe"
{ printf "MZ\220\0\3\0\0\0This program cannot be run in DOS mode.\0"; tail -c +5 "$T/harness-desktop"; } >"$T/harness-desktop.exe"
check  "a clean Windows PE stand-in passes the release check"                  "$CHECK" "$T/clean.exe"
ncheck "a harness Windows PE stand-in fails the release check"                 "$CHECK" "$T/harness-desktop.exe"
check  "a desktop harness Windows PE stand-in passes the desktop positive control" "$CHECK" --harness --desktop "$T/harness-desktop.exe"
ncheck "a clean Windows PE stand-in fails the positive control"                "$CHECK" --harness --desktop "$T/clean.exe"
for n in "${NAMES[@]}" LUCINANET-HARNESS-BUILD; do
  { cat "$T/clean"; printf 'xx%sxx\0' "$n"; } >"$T/one"
  ncheck "a binary holding only $n fails the release check" "$CHECK" "$T/one"
  { cat "$T/clean"; for m in "${NAMES[@]}"; do [ "$m" = "$n" ] || printf '%s\0' "$m"; done; printf 'LUCINANET-HARNESS-BUILD\0'; } >"$T/almost"
  [ "$n" = LUCINANET-HARNESS-BUILD ] && { cat "$T/clean"; for m in "${NAMES[@]}"; do printf '%s\0' "$m"; done; } >"$T/almost"
  ncheck "a harness stand-in missing $n fails the positive control" "$CHECK" --harness "$T/almost"
done
# the function cache's directory/size names are absent from EVERY build, the harness build included
for n in LUCINANET_JS_FN_CACHE_DIR LUCINANET_JS_FN_CACHE_MB; do
  { cat "$T/clean"; printf 'xx%sxx\0' "$n"; } >"$T/fnc"
  ncheck "a binary holding $n fails the release check" "$CHECK" "$T/fnc"
  { cat "$T/harness"; printf 'xx%sxx\0' "$n"; } >"$T/fnc-h"
  ncheck "a harness binary holding $n fails the positive control" "$CHECK" --harness "$T/fnc-h"
done
{ cat "$T/clean"; printf 'LUCINANET_JS_FN_CACHE\0'; } >"$T/switch"
check  "the opt-in switch LUCINANET_JS_FN_CACHE alone is allowed" "$CHECK" "$T/switch"
ncheck "no argument is a usage error"                       "$CHECK"
ncheck "a missing file is a usage error"                    "$CHECK" "$T/nope"
"$CHECK" "$T/nope" >/dev/null 2>&1; rc=$?; if [ "$rc" -eq 2 ]; then ok "a usage error exits 2"; else bad "a usage error exits 2"; fi
"$CHECK" "$T/harness" >/dev/null 2>&1; rc=$?; if [ "$rc" -eq 1 ]; then ok "a failed check exits 1"; else bad "a failed check exits 1"; fi

# the Linux release script refuses a harness binary (and a harness feature list) without building anything; this tree does not carry
# that script, so its cases run only where it exists
if [ -f "$BUILD" ]; then
check  "build-linux-desktop accepts a clean binary"         "$BUILD" --verify-binary "$T/clean"
ncheck "build-linux-desktop refuses a harness binary"       "$BUILD" --verify-binary "$T/harness"
if "$BUILD" --verify-binary "$T/harness" 2>&1 | grep -q "harness build"; then ok "and says why"; else bad "and says why"; fi
else
  echo "skip  build-linux-desktop cases (tools/release/build-linux-desktop.sh is not in this tree)"
fi
# shellcheck source=tools/release/harness-guard.sh
. "$ROOT/tools/release/harness-guard.sh"
for f in harness desktop,harness desktop,media,harness "desktop harness" lucinanet-servo/harness lucinanet-ui/harness; do
  check "the feature list '$f' is a harness list" harness_features "$f"
done
for f in desktop,media desktop "" media; do
  ncheck "the feature list '$f' is not a harness list" harness_features "$f"
done
check  "/Applications/X.app is an installed app path"       installed_app_path /Applications/X.app
check  "$HOME/Applications/X.app is an installed app path"      installed_app_path "$HOME/Applications/X.app"
ncheck "a scratch directory is not an installed app path"   installed_app_path "$T/stage/LucinaNet.app"

printf -- '--- %s passed, %s failed\n' "$PASS" "$FAILED"
[ "$FAILED" -eq 0 ] || exit 1
