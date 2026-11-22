#!/usr/bin/env bash
# Verus on the shipped files (the rule): verification/glyph_key.rs (src/glyph_key_bits.rs).
# Exits non-zero when Verus reports any error. Needs Verus: put it on PATH or set VERUS to its binary.
set -uo pipefail
cd "$(dirname "$0")/.."
verus=${VERUS:-verus}
command -v "$verus" >/dev/null 2>&1 || { echo "error: Verus not found; put verus on PATH or set VERUS=/path/to/verus" >&2; exit 1; }
exec "$verus" verification/glyph_key.rs
