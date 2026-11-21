#!/usr/bin/env bash
# Shows that raster/rules/src/span.rs (the span arithmetic of raster/core/src/ext.rs) can fail: each line plants one bug in a copy of the shipped file, and Verus must
# refuse it. Run on the build host from the repository root, through the verification script: raster/rules/mutants_span.sh
set -uo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
shipped=$root/raster/rules/src/span.rs
work=$(mktemp -d "${TMPDIR:-/tmp}/raster-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT
mkdir -p "$work/raster/rules/src"
cp "$root/raster/verify.sh" "$work/raster/"
fail=0
refused=0
try() {
    local from=$1 to=$2
    local text; text=$(<"$shipped")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    printf "%s\n" "${text/"$from"/"$to"}" > "$work/raster/rules/src/span.rs"
    if cmp -s "$work/raster/rules/src/span.rs" "$shipped"; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if FILES=raster/rules/src/span.rs "$work/raster/verify.sh" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        refused=$((refused + 1))
        echo "refused: $from -> $to ($(grep -c "^error" "$work/out") errors)"
    fi
}
# Chunks: a remainder of 4, a chunk count that is not whole chunks.
try "    let rest = len % 4;" "    let rest = len % 4 + 1;"
try "    let up = (n + 3) / 4 * 4;" "    let up = (n + 4) / 4 * 4;"
try "    let k = if down < avail { down } else { avail };" "    let k = down;"
try "    let px = steps.wrapping_mul(4) as i64;
    if px < 0 {
        return 0;
    }" "    let px = steps.wrapping_mul(4) as i64;"
try "    if n < len {
        return n;
    }
    len" "    n"
# Clamped texels: one past the last texel, the row not clamped, a nearest span that reads past max_x.
try "    if lo < limit - 1 {" "    if lo <= limit {"
try "    let r = clamp_coord(b, height);" "    let r = b;"
try "    let lim = if max_x + 1 < end_x { max_x + 1 } else { end_x };" "    let lim = if max_x + 2 < end_x { max_x + 2 } else { end_x };"
try "    let max_x = if max_uv < min_x { min_x } else if max_uv > width - 1 { width - 1 } else { max_uv };" "    let max_x = if max_uv > width - 1 { width - 1 } else { max_uv };"
try "    let end = if a < width { a } else { width };" "    let end = a;"
# Gradient tables: a dither entry past 64, a stop pair past the end, a table longer than the row, an entry past it.
try "    let yi = ((y % 8) + 8) % 8;" "    let yi = (y % 8) + 8;"
try "    let i = if index < 1 { 1 } else if index > stops - 1 { stops - 1 } else { index };" "    let i = if index < 1 { 1 } else { index };"
try "    if ax + texels > width {" "    if ax + texels > width + 1 {"
try "    let texels = if from_stops { entries + (entries + 3) / 4 } else { 2 * entries };" "    let texels = if from_stops { entries } else { 2 * entries };"
try "    let i = if index < 0 { 0 } else if index > entries - 1 { entries - 1 } else { index };" "    let i = if index < 0 { 0 } else if index > entries { entries } else { index };"
# The linear filters' texel range (`linear_reads_in_bounds`): the last chunk's +4 dropped, a strict > for the
# row check, row1 not checked, the max of the two rows dropped, a wider t in the proved conclusion.
try "    row0 >= 0 && row1 >= 0 && last >= 0 && m + 4 * last + 4 <= len_texels" "    row0 >= 0 && row1 >= 0 && last >= 0 && m + 4 * last <= len_texels"
try "    row0 >= 0 && row1 >= 0" "    row0 > 0 && row1 >= 0"
try "    row0 >= 0 && row1 >= 0 && last >= 0 && m" "    row0 >= 0 && last >= 0 && m"
try "    let m = if row0 >= row1 { row0 } else { row1 };" "    let m = row0;"
try "0 <= j <= last && 0 <= t < 4 ==>" "0 <= j <= last && 0 <= t < 5 ==>"
echo "$refused refused"
exit "$fail"
