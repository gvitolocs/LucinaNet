#!/bin/bash
# Build a macOS .app bundle for LucinaNet from an already built arm64 binary. Run it on a Mac: the Apple SDK licence
# means the engine is built there anyway, and the icon needs sips/iconutil while the signing needs codesign.
#
#   tools/release/package-macos.sh --executable <arm64 binary> --fonts <dir> --output <.../LucinaNet.app> \
#                                  --version <0.1.0> --source <provenance line> [--zip <out.zip>]
#
# Everything it needs is an explicit argument. It never asks git for a revision: a tree with uncommitted changes would
# otherwise be packaged under a claim ("main") that the binary does not support. --source is copied verbatim into the
# manifest, so the maintainer decides what it says.
#
# It writes nothing outside --output, --manifest and --zip, and fails if any of those already exists: an existing
# install is never overwritten. The bundle is staged in the output's own parent directory and moved into place at the
# end, so a failure leaves no half bundle at the output path. Signing happens on the staged copy; the input binary is
# only read.
#
# Ad-hoc signing (no identity, no notarisation), the same as .github/workflows/release.yml: the bundle runs on the Mac
# that built it, which is the only Mac this project ships to today. There is deliberately no --identity option: a
# Developer ID flow needs the maintainer's Apple account and its own review, not an option on a packager.
#
# RELEASE PRECONDITION (comment only: this script does not run it): before the bundle this packages is installed on a
# Mac (or its source pushed publicly), the nightly zero-network result for that commit must be GREEN:
#   tests/zero-network/release-check.sh target/nightly/zero-network.log <commit>   # must exit 0
# A red or stale nightly blocks the install and the push. See docs/release-checklist.md.
set -u

PROG=package-macos
die() { echo "$PROG: $1" >&2; exit 1; }
say() { echo "$PROG: $1"; }

usage() {
  cat <<'USE'
usage: package-macos.sh --executable F --fonts D --output APP --version V --source S [options]

required
  --executable F   an already built Mach-O arm64 engine binary (read-only input)
  --fonts D        font directory; must hold at least one .ttf/.otf and the licence that covers them
  --output APP     the .app to create, e.g. .../staging/LucinaNet.app (must not exist)
  --version V      MAJOR.MINOR.PATCH (a leading v is dropped; prereleases are refused — not valid for Apple's plist keys)
  --source S       provenance of the executable, copied verbatim into the manifest

optional
  --manifest F     manifest path (default APP.manifest.txt; must not exist)
  --zip F          also write a zip of the bundle with ditto -c -k --keepParent (must not exist)
  --bin-name N     engine file name inside Contents/MacOS (default lucinanet-bin; must not be the launcher's own name)
  --bundle-id ID   CFBundleIdentifier (default io.github.gvitolocs.lucinanet)
  --logo D         approved mark directory (default <repo>/assets/logo)

Signing is ad-hoc only (codesign -s -). Fonts without a licence file beside them are refused: the faces are
third-party and may not ship without the licence that covers them.
USE
}

LAUNCHER_NAME=lucinanet
EXEC=""; FONTS=""; OUTPUT=""; VERSION=""; SOURCE=""; MANIFEST=""; ZIP=""
BIN_NAME="lucinanet-bin"; BUNDLE_ID="io.github.gvitolocs.lucinanet"
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
LOGO=$ROOT/assets/logo

need_value() { [ "$2" -ge 2 ] || { usage >&2; die "$1 needs a value"; }; }
while [ $# -gt 0 ]; do
  case $1 in
    --executable) need_value "$1" $#; EXEC=$2; shift 2;;
    --fonts) need_value "$1" $#; FONTS=$2; shift 2;;
    --output) need_value "$1" $#; OUTPUT=$2; shift 2;;
    --version) need_value "$1" $#; VERSION=$2; shift 2;;
    --source) need_value "$1" $#; SOURCE=$2; shift 2;;
    --manifest) need_value "$1" $#; MANIFEST=$2; shift 2;;
    --zip) need_value "$1" $#; ZIP=$2; shift 2;;
    --bin-name) need_value "$1" $#; BIN_NAME=$2; shift 2;;
    --bundle-id) need_value "$1" $#; BUNDLE_ID=$2; shift 2;;
    --logo) need_value "$1" $#; LOGO=$2; shift 2;;
    -h|--help) usage; exit 0;;
    *) usage >&2; die "unknown argument $1";;
  esac
done

for req in EXEC FONTS OUTPUT VERSION SOURCE; do
  eval "v=\${$req}"
  [ -n "$v" ] || { usage >&2; die "--$(printf '%s' "$req" | tr '[:upper:]' '[:lower:]' | tr '_' '-') is required"; }
done

# ---- the inputs have to be what they claim to be -----------------------
# Apple requires CFBundleShortVersionString to be exactly three period-separated integers and CFBundleVersion one
# to three. A prerelease suffix (0.1.0-rc.1) is valid semver but not a valid value for either key, so it is refused:
# the version must be plain MAJOR.MINOR.PATCH.
case $VERSION in v*) VERSION=${VERSION#v};; esac
printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' \
  || die "--version must be MAJOR.MINOR.PATCH (a leading v is dropped; prereleases like 0.1.0-rc.1 are not valid for Apple's CFBundleVersion or CFBundleShortVersionString): $VERSION"
BUNDLE_VERSION=$VERSION
printf '%s' "$BUNDLE_ID" | grep -Eq '^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$' \
  || die "--bundle-id is not a bundle identifier: $BUNDLE_ID"
printf '%s' "$BIN_NAME" | grep -Eq '^[A-Za-z0-9._-]+$' || die "--bin-name is not a file name: $BIN_NAME"
case $BIN_NAME in .|..) die "--bin-name must not be '$BIN_NAME': the engine path must stay inside Contents/MacOS";; esac
[ "$BIN_NAME" = "$LAUNCHER_NAME" ] \
  && die "--bin-name must not be '$LAUNCHER_NAME': the launcher already owns that name in Contents/MacOS, and staging over it would replace the launcher with the engine"

[ -d "$ROOT/packaging/macos" ] || die "templates missing under $ROOT/packaging/macos"
[ -f "$EXEC" ] && [ -r "$EXEC" ] || die "no readable engine binary at $EXEC"
# a harness build (test and measurement doors, docs/harness-doors.md) is never packaged, and never put over an installed app.
# shellcheck source=tools/release/harness-guard.sh
. "$ROOT/tools/release/harness-guard.sh"
refuse_harness_binary "$PROG" "$EXEC" "$OUTPUT"
fileinfo=$(file -b "$EXEC") || die "file(1) failed on $EXEC"
case $fileinfo in *Mach-O*) ;; *) die "$EXEC is not a Mach-O binary ($fileinfo): the engine cannot be cross-built on Linux";; esac
ARCHS=""
if command -v lipo >/dev/null 2>&1; then ARCHS=$(lipo -archs "$EXEC" 2>/dev/null || true); fi
if [ -n "$ARCHS" ]; then
  case " $ARCHS " in *" arm64 "*) ;; *) die "$EXEC is not an arm64 binary (archs: $ARCHS)";; esac
else
  case $fileinfo in *arm64*) ;; *) die "$EXEC is not an arm64 binary ($fileinfo)";; esac
  ARCHS=arm64
fi

[ -d "$FONTS" ] || die "no fonts directory at $FONTS"
nfaces=$(cd "$FONTS" && find . -type f \( -name '*.ttf' -o -name '*.otf' \) ! -name '._*' | wc -l | tr -d ' ')
[ "$nfaces" -gt 0 ] || die "no .ttf or .otf face in $FONTS"
# The faces are third-party (DejaVu and friends), so the licence that covers them ships with them. There is no
# bypass: a bundle that starts without it would be one the release cannot publish.
licence=$(cd "$FONTS" && find . -maxdepth 1 -type f \( -iname 'LICENSE*' -o -iname 'LICENCE*' -o -iname 'OFL*' -o -iname '*COPYING*' \) ! -name '._*' | head -1)
[ -n "$licence" ] || die "no licence file beside the fonts in $FONTS: the faces are third-party, so they may not ship without the licence that covers them (put it in the fonts directory and run again)"

for m in lucinanet-16.png lucinanet-32.png lucinanet-64.png lucinanet-128.png lucinanet-256.png lucinanet-512.png README.md; do
  [ -f "$LOGO/$m" ] || die "approved mark missing: $LOGO/$m"
done
png_magic=89504e470d0a1a0a
for m in 16 32 64 128 256 512; do
  got=$(head -c 8 "$LOGO/lucinanet-$m.png" | od -An -tx1 | tr -d ' \n')
  [ "$got" = "$png_magic" ] || die "$LOGO/lucinanet-$m.png is not a PNG (magic $got)"
done

# ---- the outputs must not exist yet -----------------------------------
# -e alone misses a dangling symlink; -L catches it. A directory at the output path would make mv nest the bundle
# inside it, so refuse that too (covered by -e for a real dir, but the message should be precise).
[ -e "$OUTPUT" ] && die "refusing to overwrite: $OUTPUT already exists"
[ -L "$OUTPUT" ] && die "refusing to overwrite: $OUTPUT is a dangling symlink"
case $OUTPUT in */*) OUTDIR=${OUTPUT%/*};; *) OUTDIR=.;; esac
[ -d "$OUTDIR" ] || die "no such directory to write the bundle into: $OUTDIR"
[ -n "$MANIFEST" ] || MANIFEST="$OUTPUT.manifest.txt"
[ -e "$MANIFEST" ] && die "refusing to overwrite: $MANIFEST already exists"
[ -L "$MANIFEST" ] && die "refusing to overwrite: $MANIFEST is a dangling symlink"
if [ -n "$ZIP" ]; then
  [ -e "$ZIP" ] && die "refusing to overwrite: $ZIP already exists"
  [ -L "$ZIP" ] && die "refusing to overwrite: $ZIP is a dangling symlink"
fi

for t in sips iconutil codesign plutil sw_vers stat; do
  command -v "$t" >/dev/null 2>&1 || die "$PROG needs $t, which is not on PATH: run it on a Mac with the command line tools"
done
if command -v sha256sum >/dev/null 2>&1; then HASHER=sha256sum
elif command -v shasum >/dev/null 2>&1; then HASHER="shasum -a 256"
else die "neither sha256sum nor shasum is on PATH"; fi
hash_file() { $HASHER "$1" | cut -d' ' -f1; }
png_size() { local w h
  w=$(sips -g pixelWidth "$1" 2>/dev/null | sed -n 's/.*pixelWidth: *//p' | head -1)
  h=$(sips -g pixelHeight "$1" 2>/dev/null | sed -n 's/.*pixelHeight: *//p' | head -1)
  printf '%sx%s' "$w" "$h"; }
# Literal replacement, no pattern in the value: an operator may pass a path with anything in it.
substitute() { local dst=$1 content=$2; shift 2
  while [ $# -ge 2 ]; do content=${content//"$1"/"$2"}; shift 2; done
  case $content in *@[A-Z]*@*) die "$dst: a placeholder of the template was not filled";; esac
  printf '%s\n' "$content" > "$dst" || die "cannot write $dst"; }

STAGE=$OUTDIR/.$(basename "$OUTPUT").stage.$$
APP=$STAGE/$(basename "$OUTPUT")
cleanup() { [ -n "${STAGE:-}" ] && [ -d "$STAGE" ] && rm -rf "$STAGE"; }
on_exit() { local rc=$?; cleanup; exit "$rc"; }
trap on_exit EXIT INT TERM

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/fonts" "$APP/Contents/Resources/logo" || die "cannot stage under $OUTDIR"

# ---- the icon ----------------------------------------------------------
# iconutil wants an .iconset with exactly these names. The approved mark ships every size but 1024, so the last tile
# is interpolated from the 512 master and the manifest says so: no art is redrawn, and nothing is claimed to be of
# native 1024 quality. An exact-size render from the SVG would be better and needs a renderer (rsvg-convert or
# inkscape) that the build Macs do not have; install one and this script still uses the PNG masters unchanged.
ICONSET=$STAGE/LucinaNet.iconset
mkdir -p "$ICONSET"
put_icon() { local src=$LOGO/lucinanet-$1.png dst=$ICONSET/$2 want=$3 got
  cp "$src" "$dst" || die "cannot copy $src into the iconset"
  chmod 0644 "$dst" || die "cannot set the mode on $dst"
  got=$(png_size "$dst")
  [ "$got" = "$want" ] || die "$2 is $got, expected $want"
  say "icon $2 $got (lucinanet-$1.png)"
}
put_icon 16 icon_16x16.png 16x16
put_icon 32 icon_16x16@2x.png 32x32
put_icon 32 icon_32x32.png 32x32
put_icon 64 icon_32x32@2x.png 64x64
put_icon 128 icon_128x128.png 128x128
put_icon 256 icon_128x128@2x.png 256x256
put_icon 256 icon_256x256.png 256x256
put_icon 512 icon_256x256@2x.png 512x512
put_icon 512 icon_512x512.png 512x512
sips -z 1024 1024 "$LOGO/lucinanet-512.png" --out "$ICONSET/icon_512x512@2x.png" >/dev/null 2>&1 \
  || die "sips could not interpolate the 1024 tile from $LOGO/lucinanet-512.png"
chmod 0644 "$ICONSET/icon_512x512@2x.png"
up=$(png_size "$ICONSET/icon_512x512@2x.png")
[ "$up" = 1024x1024 ] || die "the interpolated 1024 tile came out $up"
say "icon icon_512x512@2x.png 1024x1024 (interpolated from the 512 master)"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/LucinaNet.icns" || die "iconutil failed to build the .icns"
ICNS=$APP/Contents/Resources/LucinaNet.icns
[ -s "$ICNS" ] || die "the .icns is empty"
[ "$(head -c 4 "$ICNS")" = icns ] || die "the .icns does not start with the icns magic"

# ---- the rest of the bundle -------------------------------------------
install -m 0755 "$EXEC" "$APP/Contents/MacOS/$BIN_NAME" || die "cannot stage the engine binary"
substitute "$APP/Contents/MacOS/$LAUNCHER_NAME" "$(cat "$ROOT/packaging/macos/launcher.sh")" '@BIN_NAME@' "$BIN_NAME"
substitute "$APP/Contents/Info.plist" "$(cat "$ROOT/packaging/macos/Info.plist.in")" \
  '@EXECUTABLE@' "$LAUNCHER_NAME" '@ICON_FILE@' LucinaNet.icns '@BUNDLE_ID@' "$BUNDLE_ID" \
  '@SHORT_VERSION@' "$VERSION" '@BUILD_VERSION@' "$BUNDLE_VERSION"
plutil -lint "$APP/Contents/Info.plist" >/dev/null || die "the staged Info.plist does not parse"

# AppleDouble (._*) and local xattrs stay out of a bundle that other people install.
( cd "$FONTS" && find . -type f ! -name '._*' -print0 ) | LC_ALL=C sort -z > "$STAGE/.fonts.list"
while IFS= read -r -d '' f; do
  r=${f#./}
  mkdir -p "$APP/Contents/Resources/fonts/$(dirname "$r")" || die "cannot stage the font directory for $r"
  install -m 0644 "$FONTS/$r" "$APP/Contents/Resources/fonts/$r" || die "cannot copy the font $r"
done < "$STAGE/.fonts.list"
install -m 0644 "$LOGO/README.md" "$APP/Contents/Resources/logo/README.md" || die "cannot copy the mark licence notice"

# Modes come from the packager, never from the umask of whoever ran it: the manifest records them, so two operators on
# two hosts must still get the same bundle.
find "$APP" -type d -exec chmod 0755 {} + || die "cannot set the directory modes"
find "$APP" -type f -exec chmod 0644 {} + || die "cannot set the file modes"
chmod 0755 "$APP/Contents/MacOS/$BIN_NAME" "$APP/Contents/MacOS/$LAUNCHER_NAME" || die "cannot set the executable modes"

# ---- signing ----------------------------------------------------------
# Ad-hoc only: no identity is asked for, and the ad-hoc result is checked, not assumed.
say "codesign --sign - (ad-hoc)"
codesign --force --deep --sign - "$APP" || die "codesign failed on the staged bundle"
codesign --verify --deep --strict --verbose=2 "$APP" || die "codesign --verify failed on the staged bundle"
codesign -dvv "$APP" 2>&1 | grep -q 'Signature=adhoc' || die "the bundle is not ad-hoc signed"
# The signature seals contents, not modes, so normalising these after signing is safe and keeps the manifest stable.
if [ -d "$APP/Contents/_CodeSignature" ]; then
  chmod 0755 "$APP/Contents/_CodeSignature"
  chmod 0644 "$APP/Contents/_CodeSignature"/* 2>/dev/null
fi

# ---- the manifest -----------------------------------------------------
# Relative paths, sorted, own modes: the same inputs give a byte-identical manifest in a different output directory.
( cd "$APP" && find . -type f ! -name '._*' -print0 ) | LC_ALL=C sort -z > "$STAGE/.bundle.list"
{
  printf 'format=lucinanet-macos-bundle/1\n'
  printf 'bundle=%s\n' "$(basename "$OUTPUT")"
  printf 'version=%s\n' "$VERSION"
  printf 'build_version=%s\n' "$BUNDLE_VERSION"
  printf 'source=%s\n' "$SOURCE"
  printf 'bundle_id=%s\n' "$BUNDLE_ID"
  printf 'codesign_identity=-\n'
  printf 'host.macos=%s\n' "$(sw_vers -productVersion)"
  printf 'input.executable=%s\n' "$EXEC"
  printf 'input.executable.arch=%s\n' "$ARCHS"
  printf 'input.executable.sha256=%s\n' "$(hash_file "$EXEC")"
  printf 'input.fonts=%s\n' "$FONTS"
  printf 'input.fonts.faces=%s\n' "$nfaces"
  printf 'input.fonts.licence=%s\n' "${licence#./}"
  printf 'input.logo=%s\n' "$LOGO"
  for m in 16 32 64 128 256 512; do
    printf 'input.logo.sha256.lucinanet-%s.png=%s\n' "$m" "$(hash_file "$LOGO/lucinanet-$m.png")"
  done
  printf 'icon.512x512@2x=interpolated from lucinanet-512.png: no native 1024 art exists, the mark was not redrawn\n'
  while IFS= read -r -d '' f; do
    r=${f#./}
    printf 'file %s %s %s\n' "$(stat -f '%OLp' "$APP/$r")" "$(hash_file "$APP/$r")" "$r"
  done < "$STAGE/.bundle.list"
} > "$STAGE/manifest.txt" || die "cannot write the manifest"

# ---- put it in place --------------------------------------------------
# The final renames (bundle, manifest, zip) must be kernel-exclusive: a plain [ -e ] + mv is TOCTOU (the path can
# appear between the test and the rename), and on macOS /bin/mv does not use an exclusive rename either. On Darwin we
# build a tiny helper that calls renamex_np(RENAME_EXCL): the kernel refuses the rename if dst exists, including a
# dangling symlink. The helper source is packaging/macos/exclusive-rename.c; it needs the system C compiler (cc),
# which every Mac with the command line tools has. On non-Darwin hosts renamex_np does not exist, so this path is a
# best-effort check-and-mv: it catches everything except a true race, and it is NOT equivalent to the Darwin
# guarantee. The stub suite runs on Linux and exercises only this best-effort path; the exclusive-rename behaviour is
# verified for real on the Mac (packaging/macos/README.md).
refuse_existing() { # refuse_existing PATH LABEL: die if PATH exists in any form (file, dir, dangling symlink)
  local p=$1 label=${2:-$1}
  [ -e "$p" ] && die "refusing to overwrite: $label already exists ($p)"
  [ -L "$p" ] && die "refusing to overwrite: $label is a dangling symlink ($p)"
}

exclusive_rename() { # exclusive_rename SRC DST: kernel-exclusive rename on Darwin; best-effort elsewhere
  local src=$1 dst=$2 helper=""
  if [ "$(uname -s)" = Darwin ]; then
    helper=$(mktemp "${TMPDIR:?set TMPDIR to a scratch directory (not /tmp)}/.ln-xrename.XXXXXX") || die "cannot create a temp file for the rename helper"
    cc -o "$helper" "$ROOT/packaging/macos/exclusive-rename.c" 2>/dev/null \
      || { rm -f "$helper"; die "cc could not build the exclusive-rename helper: install the Xcode command line tools"; }
    if ! "$helper" "$src" "$dst"; then
      rm -f "$helper"
      refuse_existing "$dst" "$dst"
      die "refusing to overwrite: $dst already exists (exclusive rename failed)"
    fi
    rm -f "$helper"
  else
    # Non-Darwin: no renamex_np. Best-effort check-and-mv; NOT equivalent to the Darwin exclusive rename.
    refuse_existing "$dst" "$dst"
    mv "$src" "$dst" || die "cannot move $src to $dst"
  fi
}

refuse_existing "$OUTPUT" "--output"
exclusive_rename "$APP" "$OUTPUT"
[ -x "$OUTPUT/Contents/MacOS/$LAUNCHER_NAME" ] || die "$OUTPUT has no executable launcher after the move"
plutil -lint "$OUTPUT/Contents/Info.plist" >/dev/null || die "the installed Info.plist does not parse"
codesign --verify --deep --strict "$OUTPUT" || die "codesign --verify failed on $OUTPUT"

refuse_existing "$MANIFEST" "--manifest"
exclusive_rename "$STAGE/manifest.txt" "$MANIFEST"

if [ -n "$ZIP" ]; then
  # The zip gets the same no-clobber contract as the bundle and manifest: it is written into the staging directory we
  # own, then moved to --zip with the same exclusive rename. Writing ditto straight to the final path would let a
  # concurrent creator be overwritten between the early check and the write.
  ZIP_STAGE="$STAGE/.$(basename "$ZIP").stage.zip"
  refuse_existing "$ZIP" "--zip"
  ditto -c -k --keepParent "$OUTPUT" "$ZIP_STAGE" || die "ditto could not write $ZIP_STAGE"
  exclusive_rename "$ZIP_STAGE" "$ZIP"
  say "zip $ZIP"
fi
cleanup
trap - EXIT INT TERM

say "bundle $OUTPUT ($(grep -c '^file ' "$MANIFEST") files, ad-hoc signed)"
say "manifest $MANIFEST"
