# Release checklist

The ordered steps to take LucinaNet from a tree to something that is pushed publicly or installed
on a machine. Every step below is either an existing procedure in this repository (the file that
documents it is cited) or, for step 2, the nightly zero-network check this file adds. Nothing
here invents a procedure: it collects the ones that already exist into one list and adds the nightly
blocker.

## 1. Build the desktop binary

Linux, in the pinned Debian 12 image and as an unprivileged user (never root, never `--privileged`) —
the release job runs exactly this:

```sh
cargo build --release --locked -p lucinanet-servo --features desktop,media
```

`.github/workflows/release.yml` builds that image and prints the glibc floor; the image and its mount
rules are `tools/release/Dockerfile.linux-desktop`, described in `tools/release/README.md`.
The binary the nightly (step 2) and the packager (step 3) consume is the one this produces.

The shipped binary is built **without** the cargo feature `harness` (the test and measurement doors, `docs/harness-doors.md`).
Before anything is pushed or installed, `tests/harness-gate/check-release-binary.sh <binary>` must pass on it (no door name is in the
file); no release build names the `harness` feature, and the packager refuses a binary that carries its marker
(`tools/release/README.md`).

## 2. Nightly zero-network result must be GREEN for this commit  **(required)**

`tests/zero-network/run.sh` proves a real start of the binary makes no network request until the user
acts. It is not one of the checks CI runs on every push: it runs **nightly**, and the nightly result is
**non-optional**: a red or stale nightly **blocks the next public push and the next Mac install**.

The nightly runs `tests/zero-network/run.sh all --record <log>` and appends one line per run to the
record (a conventional place is `target/nightly/zero-network.log`, newest last). Before you push a
commit publicly, and again before you install a Mac build, the newest line of that log must be a PASS
**for the exact commit you are shipping** and **recent enough** (default window 26 h = one nightly cycle
plus slack):

```sh
tests/zero-network/release-check.sh target/nightly/zero-network.log <commit>
# exit 0 = GREEN, ship; any other exit = RED, do not push and do not install
```

`<commit>` is the full sha or a prefix. A missing, empty, stale, FAIL or ERROR newest line, or a newest
PASS for a different commit, is RED and prints one line saying why. See
`tests/zero-network/README.md` for how the nightly obtains the binary and what it costs.

**This step gates both (a) a public push and (b) a Mac install.** Do it after the build (step 1) and
before the packager (step 3).

## 3. Package the macOS `.app` (Mac installs only)

On the build Mac, from an already-built arm64 engine binary:

```sh
tools/release/package-macos.sh --executable <arm64 binary> --fonts <dir> \
  --output <staging>/LucinaNet.app --version <0.1.0> --source "<provenance line>" [--zip <out.zip>]
```

The packager writes nothing outside `--output`/`--manifest`/`--zip`, refuses to clobber an existing
install, and ad-hoc signs. `--source` is copied verbatim into the manifest — the operator states what
the binary is; a tree with uncommitted changes must not be labelled "main". Details:
`tools/release/package-macos.sh` and `packaging/macos/README.md`.

## 4. Real Mac staging, required before release

The stub test suite (`packaging/macos/tests/test-package.sh`) proves the packager's logic, not that
macOS accepts the bundle. Run the staging procedure in `packaging/macos/README.md` ("Real Mac staging
(required before release)") on the build Mac: verify the engine hash, stage under a unique dir, lint /
codesign / `file` the result, and record the manifest for review. Step 2 above must be GREEN for the
commit staged here.

## 5. Review and install

Per `packaging/macos/README.md`, only the maintainer installs (`cp` to `~/Applications/`, `open`), and only
after reviewing the staged bundle and manifest — never the packager, and never a helper script.

## Preconditions wired into the release scripts

`tools/release/package-macos.sh` carries step 2 as a documented **precondition comment in its header**. It
is a comment only — the script does not call `release-check.sh` and its behaviour is unchanged; the check
is run by the person pushing or installing.

## Known issues to name in the release notes

Say both in the notes, in these words:

* **WebGPU buffer mapping panics the page's pipeline at unmap** until the engine pays the free owner at detach
  (panic containment; Servo `datablock.rs`). WebGPU is off by default in our builds and `navigator.gpu` is not
  exposed to an ordinary page, so no page reached by normal browsing hits this.
* **Memory: the JS heap starts at 256 MB on desktop.** The glue defaults the engine's minimum heap to 256 MB on macOS and Linux
  x86_64 (elsewhere, and for a context with a parent runtime, the engine's 64 MB; `js/mozjs-sys/src/engine/cx.rs`). It is reserved,
  not committed: the pages become resident only as the heap is used. A memory complaint after an install should first check this
  default; `LUCINANET_JS_MIN_HEAP_MB=64` restores the old figure only when the engine binary (`lucinanet-servo`, or the bootstrap
  `lucinanet-bin`) is started directly, not through the launcher or the app bundle: the launchers scrub every `LUCINA*` and
  `LUCINANET_*` variable out of the environment before starting the engine. For a normally started app the heap minimum is
  known, not adjustable from the app. A real setting for it is planned; no date is promised.
