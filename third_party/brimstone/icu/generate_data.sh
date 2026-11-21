#!/bin/bash

# This script is used to generate the data files for ICU4X

set -e

CURRENT_DIR=$(cd "$(dirname "$0")" && pwd)
DATA_DIR="$CURRENT_DIR/data"

# This version must match the version of icu in Cargo.toml
ICU_VERSION="2.3.0"

# Install the datagen tool into a scratch cargo bin directory instead of ~/.cargo
CARGO_BIN_DIR="${CARGO_BIN_DIR:-${WORK_DIR:-$(mktemp -d)}/lucinanet/js-engine/cargo-bin}"

# `unstable` is the icu4x-datagen 2.3 feature name (it was `experimental` on the 2.1 line);
# it enables the draft data markers both this set and the Intl set bake.
cargo install --version "$ICU_VERSION" --root "$CARGO_BIN_DIR" --features unstable icu4x-datagen

"$CARGO_BIN_DIR/bin/icu4x-datagen" \
  --markers $(cat "$CURRENT_DIR/markers.txt") \
  --locales en \
  --format baked \
  --out "$DATA_DIR" \
  --overwrite \
  --pretty \
  --use-separate-crates