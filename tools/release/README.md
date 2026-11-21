# tools/release

`Dockerfile.linux-desktop` is the build image for the Linux x86_64 release file.

- **Base.** Debian 12, which has glibc 2.36. The image holds only the toolchain and the build dependencies (`clang`, `cmake`, `make`, `python3`, `pkg-config`, `binutils`); the source and the cargo directories are mounted when the container runs.
- **Toolchain.** `rustup-init` is fetched at a pinned version and checked against a recorded sha256 before it runs; there is no `curl | sh`. The Rust version is the `RUST_TOOLCHAIN` build argument, which the release job reads from `rust-toolchain.toml`.
- **Glibc rule.** The binary built in this image must not need a glibc symbol newer than `GLIBC_2.35`, the Ubuntu 22.04 baseline the README names. The release job lists the symbols of the finished binary and fails if one is newer.
- **How the release job uses it.** On a `v*` tag, `.github/workflows/release.yml` builds the image with plain `docker build`, runs it with plain `docker run --rm` (no `--privileged`, source mounted read-only), and builds with `cargo build --release --locked -p lucinanet-servo --features desktop,media`. It then packs the binary with the DejaVu fonts and their licence file into the Linux tar.
