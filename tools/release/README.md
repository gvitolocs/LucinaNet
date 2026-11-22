# tools/release

`Dockerfile.linux-desktop` is the build image for the Linux x86_64 release file.

- **Base.** Debian 12, which has glibc 2.36. The image holds only the toolchain and the build dependencies (`clang`, `cmake`, `make`, `python3`, `pkg-config`, `binutils`); the source and the cargo directories are mounted when the container runs.
- **Toolchain.** `rustup-init` is fetched at a pinned version and checked against a recorded sha256 before it runs; there is no `curl | sh`. The Rust version is the `RUST_TOOLCHAIN` build argument, which the release job reads from `rust-toolchain.toml`.
- **Glibc rule.** The binary built in this image must not need a glibc symbol newer than `GLIBC_2.35`, the Ubuntu 22.04 baseline the README names. The release job lists the symbols of the finished binary and fails if one is newer.
- **How the release job uses it.** On a `v*` tag, `.github/workflows/release.yml` builds the image with plain `docker build`, runs it with plain `docker run --rm` (no `--privileged`, source mounted read-only), and builds with `cargo build --release --locked -p lucinanet-servo --features desktop,media`. It then packs the binary with the DejaVu fonts and their licence file into the Linux tar.

## Release / install steps

See [`../../docs/release-checklist.md`](../../docs/release-checklist.md) for the ordered release checklist. Before a public push and before
a Mac install, `tests/zero-network/release-check.sh <nightly log> <commit>` must exit 0. A red or stale nightly zero-network result blocks
the push and the install. `package-macos.sh` carries that as a precondition comment in its header; it does not run it, the operator does.
## Harness builds are never shipped

The shipped browser has no test and measurement doors (`--render`, `LUCINANET_USER_JS` and the rest: `docs/harness-doors.md`). They exist
only in a build made with the cargo feature `harness` (`tools/build-harness.sh`), which says so in `lucinanet --version` and in its
window title and carries the marker `LUCINANET-HARNESS-BUILD`.

* `package-macos.sh` refuses an `--executable` that carries the marker (exit 1, before anything is written), and says so differently
  when `--output` is an installed app path (`/Applications`, `~/Applications`): a measurement build is never installed over the
  maintainer's app.
* `harness-guard.sh` is the shared code; `packaging/macos/tests/test-package.sh` and `tests/harness-gate/test-check-release-binary.sh`
  test the refusals on stub binaries (no build).
* Before a release, `tests/harness-gate/check-release-binary.sh <binary>` must pass on the binary you ship (none of the doors'
  names is in it); `--harness <binary>` is the positive control on a harness build.
* `.github/workflows/release.yml` builds without `harness` (it fails on a feature list that names it) and runs
  `check-release-binary.sh` on every binary it builds before anything is attached to a release.
