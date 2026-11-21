#!/bin/bash
# Tests for tools/release/package-macos.sh. Run anywhere: the macOS commands the packager needs are stubbed from
# tests/stubs, and each stub is honest about the property that matters (file(1) reads the Mach-O magic and cputype,
# sips reads the PNG IHDR, codesign only verifies a bundle it was asked to sign, stat reports the real mode), so a
# packager that writes the wrong bytes fails here rather than on a Mac. The happy path uses the committed
# assets/logo masters, so a bad master in the repository fails too.
# What the stubs cannot prove is that macOS accepts the result; that is the real run on the build Mac recorded in
# packaging/macos/README.md. No Python tooling, so the fixtures are built from bytes.
#
# Usage: packaging/macos/tests/test-package.sh        (exit 0 = everything passed)
#        KEEP=1 packaging/macos/tests/test-package.sh   leaves the fixture tree for inspection
set -u

ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
PKG=$ROOT/tools/release/package-macos.sh
STUBS=$ROOT/packaging/macos/tests/stubs
[ -x "$PKG" ] || { echo "not runnable: $PKG" >&2; exit 1; }
for t in zip unzip od awk; do command -v "$t" >/dev/null 2>&1 || { echo "$t is needed by the fixtures" >&2; exit 1; }; done

T=$(mktemp -d "${TMPDIR:-/tmp}/ln-package-tests.XXXXXX")
trap '[ -n "${KEEP:-}" ] || rm -rf "$T"' EXIT
PASS=0; FAILED=0
ok()  { printf 'ok    %s\n' "$1"; PASS=$((PASS+1)); }
bad() { printf 'FAIL  %s\n        %s\n' "$1" "$2"; FAILED=$((FAILED+1)); }
check()  { local l=$1; shift; if "$@" >/dev/null 2>&1; then ok "$l"; else bad "$l" "failed: $*"; fi }
ncheck() { local l=$1; shift; if ! "$@" >/dev/null 2>&1; then ok "$l"; else bad "$l" "unexpectedly true: $*"; fi }
# plist KEYFILE KEY : the value the plutil stub read out of the Info.plist
plist() { awk -F'\t' -v k="$2" '$1==k{print $2}' "$1"; }
KV=$T/plist.keys
run_pkg() { ( cd "$T" && PATH="$STUBS:$PATH" HOME="$T/home" STUB_PLIST_KEYS="$KV" \
               "$PKG" "$@" ) >"$T/out" 2>"$T/err"; }
want_fail() { local label=$1 pat=$2; shift 2
  run_pkg "$@"
  local r=$? hits
  hits=$(grep -c "$pat" "$T/err")
  if [ "$r" -ne 0 ] && [ "$hits" -ge 1 ]; then ok "$label"; else bad "$label" "exit $r; stderr: $(head -2 "$T/err")"; fi
}

# ---------------------------------------------------------------- fixtures
be32() { printf '%02x%02x%02x%02x' $(( ($1 >> 24) & 255 )) $(( ($1 >> 16) & 255 )) $(( ($1 >> 8) & 255 )) $(( $1 & 255 )); }
# printf needs one \x per byte: the string "\x00000010" is one NUL followed by the text "00000010".
hesc() { local h=$1 i out=""
  for (( i = 0; i < ${#h}; i += 2 )); do out+="\\x${h:i:2}"; done
  printf '%s' "$out"
}
# A PNG header with the size the name claims. Nothing decodes these; the stub reads IHDR, like sips does.
mkpng() { local f=$1 n=$2 dims
  dims=$(hesc "$(be32 "$n")$(be32 "$n")")
  { printf '\x89PNG\r\n\x1a\n'
    printf '\x00\x00\x00\x0dIHDR'
    printf '%b' "$dims"
    printf '\x08\x06\x00\x00\x00\x00\x00\x00\x00'
    printf '\x00\x00\x00\x00IEND'
    printf '\xae\x42\x60\x82'; } > "$f" || { echo "cannot write the fixture PNG $f" >&2; exit 1; }
}
mkbadlogo() { local d=$1; mkdir -p "$d"
  for n in 16 32 64 128 256; do mkpng "$d/lucinanet-$n.png" "$n"; done
  mkpng "$d/lucinanet-512.png" 120
  cp "$ROOT/assets/logo/README.md" "$d/README.md"
}
macho() { # macho FILE arm64|x86_64 : the first 8 bytes are a real Mach-O magic and cputype
  local cpu=0c000001; [ "$2" = x86_64 ] && cpu=04000001
  printf '\xcf\xfa\xed\xbe' > "$1"
  printf '%b' "\\x${cpu:0:2}\\x${cpu:2:2}\\x${cpu:4:2}\\x${cpu:6:2}" >> "$1"
  head -c 4096 /dev/zero >> "$1"
}
mkfonts() { local d=$1 lic=${2:-1}; mkdir -p "$d"
  head -c 2048 /dev/zero | tr '\0' 'A' > "$d/DejaVuSans.ttf"
  head -c 1024 /dev/zero | tr '\0' 'B' > "$d/DejaVuSans-Bold.ttf"
  printf 'fake AppleDouble sidecar\n' > "$d/._DejaVuSans.ttf"
  [ "$lic" = 1 ] && printf 'Licence: fake but present\n' > "$d/LICENSE"
}

mkdir -p "$T/home" "$T/stage" "$T/bad" "$T/alt" "$T/d1" "$T/d2" "$T/unz" "$T/launch" "$T/zip"
macho "$T/lucinanet-servo-arm64" arm64
macho "$T/x64" x86_64
mkfonts "$T/fonts"
mkfonts "$T/fonts-nolic" 0
mkdir -p "$T/fonts-empty"; printf 'no faces here\n' > "$T/fonts-empty/README"
printf '#!/bin/sh\necho I am a script, not a Mach-O\n' > "$T/notmacho"; chmod 0755 "$T/notmacho"

OUT=$T/stage/LucinaNet.app
run_pkg --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$OUT" \
        --version 0.1.0 --source "built on this Mac at 5fc6ea275b9e with uncommitted local changes"
MAIN_RC=$?
if [ "$MAIN_RC" -eq 0 ]; then ok "packaging succeeds"; else bad "packaging succeeds" "exit $MAIN_RC; stderr: $(cat "$T/err")"; fi
cp "$KV" "$T/keys.main" 2>/dev/null
M=$OUT.manifest.txt
RES=$OUT/Contents/Resources
L=$OUT/Contents/MacOS/lucinanet

# ---------------------------------------------------------------- structure
check "bundle and manifest written"        test -d "$OUT" -a -s "$M"
check "launcher is executable"             test -x "$L"
check "engine staged as lucinanet-bin"     test -x "$OUT/Contents/MacOS/lucinanet-bin"
check "icns staged in Resources"           test -s "$RES/LucinaNet.icns"
check "fonts copied"                       test -s "$RES/fonts/DejaVuSans.ttf"
check "font licence copied"                test -s "$RES/fonts/LICENSE"
check "mark licence notice shipped"        test -s "$RES/logo/README.md"
ncheck "AppleDouble left out"             test -e "$RES/fonts/._DejaVuSans.ttf"
check "bundle signed"                      test -s "$OUT/Contents/_CodeSignature/CodeDirectory"
ncheck "no staging directory left"        ls "$T/stage/."*".stage."*
check "launcher mode 755"                  test "$(stat -c '%a' "$L")" = 755
check "plist mode 644"                     test "$(stat -c '%a' "$OUT/Contents/Info.plist")" = 644

# ---------------------------------------------------------------- Info.plist
K=$T/keys.main
check "Info.plist linted"                  test -s "$K"
check "CFBundleExecutable is the launcher" test "$(plist "$K" CFBundleExecutable)" = lucinanet
check "CFBundleIconFile names the icns"    test "$(plist "$K" CFBundleIconFile)" = LucinaNet.icns
check "CFBundleShortVersionString"         test "$(plist "$K" CFBundleShortVersionString)" = 0.1.0
check "CFBundleVersion is the build string" test "$(plist "$K" CFBundleVersion)" = 0.1.0
check "CFBundleIdentifier default"         test "$(plist "$K" CFBundleIdentifier)" = io.github.gvitolocs.lucinanet
check "CFBundlePackageType APPL"           test "$(plist "$K" CFBundlePackageType)" = APPL
check "NSHighResolutionCapable"            test "$(plist "$K" NSHighResolutionCapable)" = True
ncheck "no placeholder left in the plist"  grep -q '@[A-Z][A-Z_]*@' "$OUT/Contents/Info.plist"

# ---------------------------------------------------------------- the icon
check "icns starts with the icns magic"    test "$(head -c 4 "$RES/LucinaNet.icns")" = icns
for e in icon_16x16 icon_16x16@2x icon_32x32 icon_32x32@2x icon_128x128 icon_128x128@2x \
         icon_256x256 icon_256x256@2x icon_512x512 icon_512x512@2x; do
  check "icns holds the $e tile"           grep -q "iconset $e.png" "$RES/LucinaNet.icns"
done
ncheck "no iconset directory left in the bundle" ls -d "$OUT/Contents"/*.iconset

# ---------------------------------------------------------------- the launcher
check "launcher names the staged engine"   grep -qF 'bin="$here/lucinanet-bin"' "$L" # $here is literal in the launcher
ncheck "no placeholder left in launcher"   grep -q '@[A-Z][A-Z_]*@' "$L"
for v in LUCINANET_FRAME_LOG LUCINANET_RENDER_STATS LUCINANET_WR_PHASES LUCINANET_LAYOUT_PHASES \
         LUCINANET_RENDERING_REASON LUCINANET_JS_GC_LOG LUCINANET_CHROME_LOG; do
  ncheck "launcher exports no $v"          grep -E "^[[:space:]]*export $v=" "$L"
done
cp "$L" "$T/launch/lucinanet"
printf '#!/bin/sh\nprintf "%%s\\n" "$*" > "%s/argv"\n' "$T" > "$T/launch/lucinanet-bin"
chmod 0755 "$T/launch/lucinanet-bin"
HOME="$T/home" "$T/launch/lucinanet" "https://example.com/a?x=1&y=2" >/dev/null 2>&1
check "a URL argument reaches the engine"  test "$(cat "$T/argv")" = "--desktop https://example.com/a?x=1&y=2"
: > "$T/argv"
HOME="$T/home" "$T/launch/lucinanet" >/dev/null 2>&1
check "no argument still means desktop"    test "$(cat "$T/argv")" = "--desktop"
check "session log goes under HOME"        ls "$T/home/Library/Logs/LucinaNet"/session-*.log
rm -f "$T/launch/lucinanet-bin"
out=$(HOME="$T/home" "$T/launch/lucinanet" 2>&1); rc=$?
check "a missing engine fails loudly"      test $rc -ne 0
check "and names the path"                 printf '%s' "$out" | grep -qF 'missing or not executable'

# ---------------------------------------------------------------- the manifest
check "manifest names the version"         grep -q '^version=0\.1\.0$' "$M"
check "manifest carries the source line"   grep -q '^source=built on this Mac at 5fc6ea275b9e with uncommitted local changes$' "$M"
ncheck "manifest claims no branch"         grep -qiE '(^|[= ])main([ =]|$)' "$M"
check "manifest records the identity"      grep -q '^codesign_identity=-$' "$M"
check "manifest records arm64"             grep -q '^input\.executable\.arch=arm64$' "$M"
check "manifest records the input hash"    grep -q '^input\.executable\.sha256=[0-9a-f]\{64\}$' "$M"
check "manifest records the fonts licence" grep -q '^input\.fonts\.licence=LICENSE$' "$M"
check "manifest records the 1024 upscale"  grep -q '^icon\.512x512@2x=interpolated' "$M"
check "manifest lists every bundle file"   test "$(grep -c '^file ' "$M")" = "$(cd "$OUT" && find . -type f | wc -l | tr -d ' ')"
mismatch=$(awk '$1=="file"{print $2, $3, $4}' "$M" | while read -r mode sha rel; do
  if [ ! -f "$OUT/$rel" ]; then echo "listed but absent: $rel"; continue; fi
  [ "$(sha256sum "$OUT/$rel" | cut -d' ' -f1)" = "$sha" ] || echo "hash does not match: $rel"
  [ "$(printf '%04o' "0$(stat -c '%a' "$OUT/$rel")")" = "$(printf '%04o' "0$mode")" ] || echo "mode does not match: $rel"
done)
check "every manifest line matches the bundle" test -z "$mismatch"
[ -z "$mismatch" ] || bad "manifest diff" "$mismatch"

# ---------------------------------------------------------------- no clobber
before=$(cd "$OUT" && find . -type f -exec sha256sum {} + | LC_ALL=C sort | sha256sum)
want_fail "an existing bundle is refused"   "refusing to overwrite" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$OUT" --version 0.1.0 --source "second"
check "the refusal names the output"        grep -q 'refusing to overwrite.*LucinaNet.app' "$T/err"
ncheck "the refusal happens before signing" grep -q 'codesign' "$T/out"
ncheck "nothing was staged by the refusal" ls "$T/stage/."*".stage."*
after=$(cd "$OUT" && find . -type f -exec sha256sum {} + | LC_ALL=C sort | sha256sum)
check "the old bundle is untouched"        test "$before" = "$after"
check "the old manifest is untouched"      grep -q '^source=built on this Mac at 5fc6ea275b9e' "$M"
want_fail "an existing manifest is refused" "refusing to overwrite.*manifest.txt" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/stage/other.app" \
  --manifest "$M" --version 0.1.0 --source "third"
ncheck "that refusal wrote no bundle"      test -e "$T/stage/other.app"
touch "$T/stage/preexisting.zip"
want_fail "an existing zip is refused"      "refusing to overwrite.*preexisting.zip" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/stage/new2.app" \
  --zip "$T/stage/preexisting.zip" --version 0.1.0 --source "fourth"
ncheck "a refused zip leaves no bundle"     test -e "$T/stage/new2.app"

# A dangling symlink at the output path: [ -e ] is false for it, so the packager must use [ -L ] to catch it.
ln -s "$T/nonexistent-target" "$T/stage/dangling.app"
want_fail "a dangling symlink at the output is refused" "dangling symlink" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/stage/dangling.app" --version 0.1.0 --source s
rm -f "$T/stage/dangling.app"

# An existing directory at the output path: mv would nest the bundle inside it, so it must be refused.
mkdir -p "$T/stage/dirapp.app"
want_fail "an existing directory at the output is refused" "refusing to overwrite" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/stage/dirapp.app" --version 0.1.0 --source s
ncheck "no bundle was nested inside the directory" test -e "$T/stage/dirapp.app/Contents"
rmdir "$T/stage/dirapp.app"

# Concurrent creation: a second process creates the output path between the packager's early check and its final
# rename. On Linux the packager uses a best-effort [ -e ] + mv (no renamex_np), so this test verifies that the
# re-check at commit time catches the race. The concurrent creator runs in a background subshell that sleeps briefly
# to let the packager pass its early check, then creates the output path.
( sleep 0.3; mkdir -p "$T/stage/concurrent.app" ) &
CONC_PID=$!
run_pkg --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/stage/concurrent.app" \
        --version 0.1.0 --source "concurrent"
CONC_RC=$?
wait "$CONC_PID" 2>/dev/null
# The packager must have refused (exit != 0) because the concurrent creator made the path exist.
if [ "$CONC_RC" -ne 0 ]; then ok "a concurrently created output is refused"; else bad "a concurrently created output is refused" "packager exited 0 despite concurrent creation"; fi
ncheck "no bundle was written over the concurrent dir" test -e "$T/stage/concurrent.app/Contents"
rmdir "$T/stage/concurrent.app" 2>/dev/null

# ---------------------------------------------------------------- failure cases
want_fail "a missing executable is refused"   "no readable engine binary" \
  --executable "$T/nope" --fonts "$T/fonts" --output "$T/fail.app" --version 0.1.0 --source s
want_fail "a non-Mach-O binary is refused"    "not a Mach-O" \
  --executable "$T/notmacho" --fonts "$T/fonts" --output "$T/fail.app" --version 0.1.0 --source s
want_fail "an x86_64 binary is refused"       "not an arm64" \
  --executable "$T/x64" --fonts "$T/fonts" --output "$T/fail.app" --version 0.1.0 --source s
want_fail "a missing fonts directory is refused" "no fonts directory" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/nosuch" --output "$T/fail.app" --version 0.1.0 --source s
want_fail "a fonts directory with no faces is refused" "no .ttf or .otf face" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts-empty" --output "$T/fail.app" --version 0.1.0 --source s
# There is no bypass: the faces are third-party, so a bundle without the licence that covers them cannot ship.
want_fail "fonts with no licence are refused, no override exists" "no licence file beside the fonts" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts-nolic" --output "$T/fail.app" --version 0.1.0 --source s
want_fail "a bad version is refused"          "must be MAJOR.MINOR.PATCH" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/fail.app" --version 1.0 --source s
# A prerelease is valid semver but not a valid value for Apple's CFBundleVersion or CFBundleShortVersionString,
# so it is refused: the version must be plain MAJOR.MINOR.PATCH.
want_fail "a prerelease version is refused"   "must be MAJOR.MINOR.PATCH" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/fail.app" --version 0.1.0-rc.1 --source s
want_fail "no provenance is refused"          "is required" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/fail.app" --version 0.1.0
want_fail "an argument with no value is refused" "needs a value" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/fail.app" --version 0.1.0 --source
want_fail "an unknown argument is refused"    "unknown argument" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/fail.app" --version 0.1.0 --source s --wat
want_fail "a bad bundle identifier is refused" "not a bundle identifier" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/fail.app" --version 0.1.0 --source s --bundle-id "-nope-"
want_fail "a bad bin name is refused"          "not a file name" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/fail.app" --version 0.1.0 --source s --bin-name "../escape"
# The launcher owns that name: staging the engine over it would replace the launcher with the engine, so it is refused.
want_fail "a bin name of the launcher is refused" "must not be 'lucinanet'" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/fail.app" --version 0.1.0 --source s --bin-name lucinanet
# "." and ".." would make the engine path escape Contents/MacOS, so they are refused before any staging.
want_fail "a bin name of . is refused"         "must not be '\.'" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/fail.app" --version 0.1.0 --source s --bin-name .
want_fail "a bin name of .. is refused"        "must not be '\.\.'" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/fail.app" --version 0.1.0 --source s --bin-name ..
want_fail "a missing output directory is refused" "no such directory" \
  --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/nosuch/LucinaNet.app" --version 0.1.0 --source s
check "no failure wrote a bundle"          test ! -e "$T/fail.app" -a ! -e "$T/fail.app.manifest.txt"
ncheck "no failure left a staging directory" ls "$T/stage/."*".stage."* "$T/."*".stage."*

# a wrong-size master must stop the run before anything lands at the output path
mkbadlogo "$T/logo-bad"
run_pkg --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/bad/LucinaNet.app" \
        --logo "$T/logo-bad" --version 0.1.0 --source s
check "a wrong-size master stops the run"  grep -q 'expected 512x512' "$T/err"
ncheck "and leaves no bundle"              test -e "$T/bad/LucinaNet.app"
ncheck "and leaves no staging directory"   ls "$T/bad/."*".stage."*

# ---------------------------------------------------------------- options
run_pkg --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/alt/LucinaNet.app" \
        --version v0.2.0 --bin-name lucinanet-servo --bundle-id org.lucinanet.browser \
        --source "tag v0.2.0"
if [ -d "$T/alt/LucinaNet.app" ]; then ok "version, name and id accepted"
else bad "version, name and id accepted" "$(cat "$T/err")"; fi
KA=$T/plist.keys
check "the engine took the given name"      test -x "$T/alt/LucinaNet.app/Contents/MacOS/lucinanet-servo"
check "no default name left behind"         test ! -e "$T/alt/LucinaNet.app/Contents/MacOS/lucinanet-bin"
# Both version keys hold the same plain three-integer value: Apple requires CFBundleShortVersionString to be exactly
# three period-separated integers and CFBundleVersion one to three; a prerelease is valid for neither.
check "CFBundleShortVersionString has no v" test "$(plist "$KA" CFBundleShortVersionString)" = 0.2.0
check "CFBundleVersion matches the version" test "$(plist "$KA" CFBundleVersion)" = 0.2.0
check "CFBundleIdentifier override"         test "$(plist "$KA" CFBundleIdentifier)" = org.lucinanet.browser
check "the launcher points at that name"    grep -qF 'bin="$here/lucinanet-servo"' "$T/alt/LucinaNet.app/Contents/MacOS/lucinanet" # $here is literal
check "manifest records the version"        grep -q '^version=0\.2\.0$' "$T/alt/LucinaNet.app.manifest.txt"
check "manifest records the ad-hoc identity" grep -q '^codesign_identity=-$' "$T/alt/LucinaNet.app.manifest.txt"

# ---------------------------------------------------------------- determinism
run_pkg --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/d1/LucinaNet.app" \
        --version 0.1.0 --source "same inputs"
run_pkg --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/d2/LucinaNet.app" \
        --version 0.1.0 --source "same inputs"
check "two runs write the same manifest"   cmp -s "$T/d1/LucinaNet.app.manifest.txt" "$T/d2/LucinaNet.app.manifest.txt"
h1=$(cd "$T/d1" && find . -type f -exec sha256sum {} + | LC_ALL=C sort)
h2=$(cd "$T/d2" && find . -type f -exec sha256sum {} + | LC_ALL=C sort)
check "two runs write the same bundle"     test "$h1" = "$h2"
check "both determinism runs wrote an icns" test -s "$T/d2/LucinaNet.app/Contents/Resources/LucinaNet.icns"

# ---------------------------------------------------------------- the zip
run_pkg --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/zip/LucinaNet.app" \
        --zip "$T/zip/lucinanet-0.1.0-macos-arm64.zip" --version 0.1.0 --source "zipped"
check "the zip is written"                 test -s "$T/zip/lucinanet-0.1.0-macos-arm64.zip"
( PATH="$STUBS:$PATH" ditto -x -k "$T/zip/lucinanet-0.1.0-macos-arm64.zip" "$T/unz" ) >/dev/null 2>&1
check "the zip holds the bundle at its top level" \
  test -f "$T/unz/LucinaNet.app/Contents/Info.plist"
check "the zipped Info.plist matches"      cmp -s "$T/unz/LucinaNet.app/Contents/Info.plist" \
                                           "$T/zip/LucinaNet.app/Contents/Info.plist"
check "the zipped bundle keeps the icon"   test -f "$T/unz/LucinaNet.app/Contents/Resources/LucinaNet.icns"
check "the zipped bundle keeps the signature" test -f "$T/unz/LucinaNet.app/Contents/_CodeSignature/CodeDirectory"

# ---------------------------------------------------------------- zip no-clobber under concurrency
# The zip is staged under the packager's own staging directory and moved to --zip with the same exclusive rename as
# the bundle. A concurrent creator that makes the final zip path exist between the early check and the commit must be
# refused, not overwritten. On Linux this exercises the best-effort re-check (no renamex_np); on Darwin the kernel
# enforces it. The marker file lets the test prove the packager did not clobber the concurrent creator's bytes.
mkdir -p "$T/zipc"
( sleep 0.3; printf 'concurrent zip, do not overwrite\n' > "$T/zipc/race.zip" ) &
ZIPC_PID=$!
run_pkg --executable "$T/lucinanet-servo-arm64" --fonts "$T/fonts" --output "$T/zipc/LucinaNet.app" \
        --zip "$T/zipc/race.zip" --version 0.1.0 --source "zip race"
ZIPC_RC=$?
wait "$ZIPC_PID" 2>/dev/null
if [ "$ZIPC_RC" -ne 0 ]; then ok "a concurrently created zip is refused"; else bad "a concurrently created zip is refused" "packager exited 0 despite concurrent creation"; fi
check "the concurrent zip bytes are untouched" test "$(cat "$T/zipc/race.zip" 2>/dev/null)" = "concurrent zip, do not overwrite"
ncheck "no staging zip left behind" ls "$T/zipc/."*".stage.zip"

printf -- '--- %s passed, %s failed\n' "$PASS" "$FAILED"
[ -n "${KEEP:-}" ] && printf 'fixture tree kept in %s\n' "$T"
[ "$FAILED" -eq 0 ] || exit 1
