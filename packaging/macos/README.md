# macOS .app packaging for LucinaNet

`tools/release/package-macos.sh` builds a macOS `.app` bundle from an already-built arm64 engine binary. It runs on a Mac: the Apple SDK licence means the engine is built there anyway, and the icon needs `sips`/`iconutil` while the signing needs `codesign`.

## What it does

- Stages a `.app` bundle in the output's own parent directory (never at the output path until the final rename).
- Builds a multi-size `.icns` from the approved mark (`assets/logo/lucinanet-{16,32,64,128,256,512}.png`). The 1024 tile is interpolated from the 512 master; no native 1024 art exists and none is claimed.
- Copies fonts (with their licence — there is no bypass) and the mark's licence notice into `Contents/Resources/`.
- Ad-hoc signs the bundle (`codesign --force --deep --sign -`), verifies the signature, and checks that it is ad-hoc. There is deliberately no identity option: a Developer ID flow needs the maintainer's Apple account and its own review.
- Writes a manifest with the version, source provenance, input hashes, per-file modes and hashes, and the signing identity.
- Optionally writes a zip via `ditto -c -k --keepParent`.

## No-clobber guarantee

The packager refuses to write if `--output`, `--manifest`, or `--zip` already exists in any form (file, directory, or dangling symlink). The final rename is kernel-exclusive on Darwin: the helper (`packaging/macos/exclusive-rename.c`) calls `renamex_np(RENAME_EXCL)`, which the kernel rejects if the destination exists. This closes the TOCTOU window that a plain `[ -e ]` + `mv` leaves open. On non-Darwin hosts (the test suite runs on Linux) a best-effort check is used; the concurrent-creation test in the suite verifies the re-check at commit time.

## Version contract

`--version` accepts only `MAJOR.MINOR.PATCH` (a leading `v` is dropped). Prereleases like `0.1.0-rc.1` are refused: Apple requires `CFBundleShortVersionString` to be exactly three period-separated integers and `CFBundleVersion` one to three, digits and periods only. Both plist keys hold the same plain value.

## Usage

```
tools/release/package-macos.sh \
  --executable <arm64 binary> \
  --fonts <dir with .ttf/.otf + licence> \
  --output <staging dir>/LucinaNet.app \
  --version 0.1.0 \
  --source "<provenance line>" \
  [--zip <out.zip>] \
  [--bin-name <name in Contents/MacOS, default lucinanet-bin>] \
  [--bundle-id <CFBundleIdentifier, default io.github.gvitolocs.lucinanet>] \
  [--logo <approved mark dir, default <repo>/assets/logo>]
```

The `--source` line is copied verbatim into the manifest. It is the operator's responsibility to state what the binary actually is: a tree with uncommitted changes must not be labelled "main".

## Tests

`packaging/macos/tests/test-package.sh` runs anywhere (Linux included): the macOS commands are stubbed from `tests/stubs/`, and each stub is honest about the property that matters (`file` reads the Mach-O magic and cputype, `sips` reads the PNG IHDR, `codesign` only verifies a bundle it was asked to sign, `plutil` lints the plist shape). The happy path uses the committed `assets/logo` masters, so a bad master in the repository fails too.

What the stubs cannot prove is that macOS accepts the result. That is the real run below.

```
packaging/macos/tests/test-package.sh        # exit 0 = everything passed
KEEP=1 packaging/macos/tests/test-package.sh # leaves the fixture tree for inspection
```

## Real Mac staging (required before release)

The stub tests prove the packager's logic; they do not prove that macOS accepts the bundle. A real run on the build Mac is required. The procedure below uses the **currently installed engine** — not a freshly built one — because no fresh engine binary is accepted for macOS while the YouTube regression on that platform is open.

### Inputs

| Input | Source | Note |
|---|---|---|
| Engine binary | `~/Applications/LucinaNet.app/Contents/MacOS/lucinanet-bin` | The currently installed engine. SHA-256 must be `cc8f46bffa8364f1ca0128e4824ca3534132e0f9717cbb45b198ea8f6f66b578`. Source provenance is **unknown**; the manifest must say so, not label it a specific build. |
| Fonts | `~/Applications/LucinaNet.app/Contents/Resources/fonts` | The fonts from the currently installed bundle, including its `LICENSE` file. |
| Logo | `<repo>/assets/logo` | The approved mark, already in the repository. |

### Procedure

Run on the build Mac. Do **not** modify `~/LucinaNet-mac/src`, do **not** kill the browser, do **not** install or open the result — staging only.

```bash
# 0. Verify the installed engine hash matches the expected value.
ENG=~/Applications/LucinaNet.app/Contents/MacOS/lucinanet-bin
FONTS=~/Applications/LucinaNet.app/Contents/Resources/fonts
EXPECTED=cc8f46bffa8364f1ca0128e4824ca3534132e0f9717cbb45b198ea8f6f66b578
ACTUAL=$(shasum -a 256 "$ENG" | cut -d' ' -f1)
[ "$ACTUAL" = "$EXPECTED" ] || { echo "engine hash mismatch: $ACTUAL"; exit 1; }

# 1. Create a unique staging directory under ~/LucinaNet-mac.
STAGE=~/LucinaNet-mac/staging-$(date +%Y%m%d-%H%M%S)-$$
mkdir -p "$STAGE"

# 2. Run the packager. The --source line states the provenance honestly:
#    the engine is the currently installed one, its source is unknown, and
#    the exact input hash is recorded.
REPO=~/LucinaNet-mac/src   # the repository checkout (read-only; do not modify)
"$REPO/tools/release/package-macos.sh" \
  --executable "$ENG" \
  --fonts "$FONTS" \
  --output "$STAGE/LucinaNet.app" \
  --version 0.1.0 \
  --source "installed engine, unknown source provenance, input sha256 $EXPECTED"

# 3. Verify the result on the Mac: real iconutil, plutil, codesign.
plutil -lint "$STAGE/LucinaNet.app/Contents/Info.plist"
codesign --verify --deep --strict "$STAGE/LucinaNet.app"
codesign -dvv "$STAGE/LucinaNet.app" 2>&1 | grep 'Signature=adhoc'
file "$STAGE/LucinaNet.app/Contents/MacOS/lucinanet-bin"   # must say Mach-O arm64
ls -la "$STAGE/LucinaNet.app/Contents/Resources/fonts/"    # fonts + LICENSE present

# 4. Record the result for the maintainer's review:
echo "staged bundle: $STAGE/LucinaNet.app"
echo "manifest:      $STAGE/LucinaNet.app.manifest.txt"
cat "$STAGE/LucinaNet.app.manifest.txt"
```

### What the manifest must say

- `source=installed engine, unknown source provenance, input sha256 cc8f46bf…`
- **Not** a build number or a commit fragment, and not any other label that implies a known source. The exact input hash is the only provenance claim.

### What must not happen

- No fresh engine binary from `target/` or any other build directory.
- No live install (`cp` to `~/Applications/`), no `open`, no killing the running browser.
- No modification of `~/LucinaNet-mac/src`.
- No push, tag, or release action.

### The maintainer's review and install

After the staging run, the maintainer reviews the staged bundle and manifest. Only then is the bundle installed (by the maintainer, not by this script). The staging directory is left in place for inspection.

## Files

| Path | Purpose |
|---|---|
| `tools/release/package-macos.sh` | The packager (executable). |
| `packaging/macos/Info.plist.in` | Info.plist template (`@EXECUTABLE@`, `@ICON_FILE@`, `@BUNDLE_ID@`, `@SHORT_VERSION@`, `@BUILD_VERSION@`). |
| `packaging/macos/launcher.sh` | Launcher script template (`@BIN_NAME@`). Scrubs every `LUCINA*` / `LUCINANET_*` variable of the caller, then sets `LUCINANET_CONSOLE=1`; does not enable frame/render/GC tracing (to reproduce a tracing session run the engine binary directly). |
| `packaging/macos/exclusive-rename.c` | Darwin exclusive-rename helper (`renamex_np` + `RENAME_EXCL`), built with system `cc` at packager runtime. |
| `packaging/macos/tests/test-package.sh` | Stub-based test suite (runs on Linux and Mac). |
| `packaging/macos/tests/stubs/` | Honest stubs for `file`, `sips`, `iconutil`, `codesign`, `plutil`, `lipo`, `stat`, `sw_vers`, `ditto`. |

## Dependencies

- **Mac:** Xcode command line tools (provides `cc`, `sips`, `iconutil`, `codesign`, `plutil`, `lipo`, `sw_vers`, `ditto`).
- **Linux (tests only):** `bash`, `awk`, `od`, `zip`, `unzip`. No Python.
- **`cc`** is needed on the Mac to build the exclusive-rename helper at packager runtime. The helper source is committed; the binary is not.
