#!/usr/bin/env bash
# Installs the Verus release pinned in fonts/tools/verus.lock into $1, and the Rust toolchain it needs. The
# zip's sha256 is checked before it is unpacked, and the launcher and rust_verify binaries after.
# Usage: fonts/tools/install-verus.sh <dir>     (Verus is then <dir>/verus-x86-linux/verus)
set -euo pipefail
dest=${1:?usage: install-verus.sh <dir>}
lock=$(cd "$(dirname "$0")" && pwd)/verus.lock
get() { sed -n '/^\[verus\]/,/^\[/s/^'"$1"' = "\([^"]*\)".*/\1/p' "$lock"; }
release=$(get release)
sum=$(get zip_x86_linux_sha256)
bin_sum=$(get binary_sha256)
ver_sum=$(get verifier_sha256)
toolchain=$(get rust_toolchain)
[ -n "$release" ] && [ -n "$sum" ] && [ -n "$bin_sum" ] && [ -n "$ver_sum" ] && [ -n "$toolchain" ] || {
    echo "install-verus: verus.lock is missing a [verus] pin" >&2
    exit 2
}
mkdir -p "$dest"
zip="$dest/verus-$release.zip"
if ! echo "$sum  $zip" | sha256sum -c --status 2>/dev/null; then
    curl -fsSL --retry 5 -o "$zip" "https://github.com/verus-lang/verus/releases/download/release%2F$release/verus-$release-x86-linux.zip"
    echo "$sum  $zip" | sha256sum -c --status || { echo "install-verus: checksum mismatch for $zip" >&2; exit 2; }
fi
rm -rf "$dest/verus-x86-linux"
unzip -q "$zip" -d "$dest"
echo "$bin_sum  $dest/verus-x86-linux/verus" | sha256sum -c --status || { echo "install-verus: verus launcher hash mismatch" >&2; exit 2; }
echo "$ver_sum  $dest/verus-x86-linux/rust_verify" | sha256sum -c --status || { echo "install-verus: rust_verify hash mismatch" >&2; exit 2; }
rustup toolchain install "$toolchain" --profile minimal --no-self-update >/dev/null
"$dest/verus-x86-linux/verus" --version
