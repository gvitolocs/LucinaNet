#!/usr/bin/env bash
# Verus on the shipped files (the rule): verification/glyph_key.rs (src/glyph_key_bits.rs).
# Exits non-zero when Verus reports any error.
set -uo pipefail
cd "$(dirname "$0")/.."
verus=${VERUS:-/home/user/tools/verus/verus-x86-linux/verus}
exec "$verus" verification/glyph_key.rs
