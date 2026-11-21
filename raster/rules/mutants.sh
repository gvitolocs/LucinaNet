#!/usr/bin/env bash
# Shows that raster/rules/src/lib.rs (or, with TARGET=blit.rs, blit.rs) can fail: each line plants one bug in a copy of the shipped file, and Verus must
# refuse it. Run on the build host from the repository root, through the verification script: raster/rules/mutants.sh
set -uo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
shipped=$root/raster/rules/src/${TARGET:-lib.rs}
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
    printf "%s\n" "${text/"$from"/"$to"}" > "$work/raster/rules/src/${TARGET:-lib.rs}"
    if cmp -s "$work/raster/rules/src/${TARGET:-lib.rs}" "$shipped"; then echo "NOT CHANGED: $from"; fail=1; return; fi
    if FILES=raster/rules/src/${TARGET:-lib.rs} "$work/raster/verify.sh" > "$work/out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"; fail=1
    else
        refused=$((refused + 1))
        echo "refused: $from -> $to ($(grep -c "^error" "$work/out") errors)"
    fi
}
if [ "${TARGET:-lib.rs}" = lib.rs ]; then
# Rectangles: a clip that keeps a negative edge, or lets the far edge pass the texture.
try "let cx0 = if ax0 > 0 { ax0 } else { 0 };" "let cx0 = ax0;"
try "let cx1 = if ax1 < w { ax1 } else { w };" "let cx1 = ax1;"
try "let cy1 = if ay1 < h { ay1 } else { h };" "let cy1 = if ay1 < h { ay1 } else { h + 1 };"
try "if cx0 >= cx1 || cy0 >= cy1 {" "if cx0 > cx1 || cy0 > cy1 {"
# Stripes: wrong owner, a stripe one row too long, the wrong stripe index.
try "((s as u64) % (threads as u64)) as i64" "((s as u64 + 1) % (threads as u64)) as i64"
try "let a1 = a0 + STRIPE;" "let a1 = a0 + STRIPE + 1;"
try "let s = k * threads + t;" "let s = k * threads + t + 1;"
try "pub const STRIPE: i64 = 32;" "pub const STRIPE: i64 = 16;"
# Claimed stripes (stripe_span): rows before y0, past y1, a stripe 33 rows long, the next stripe's rows, an empty span
# returned (twice), a lemma that lets a row sit in two stripes.
try "    let lo = if top > y0 { top } else { y0 };
    let hi = if bottom < y1 { bottom } else { y1 };
    if lo >= hi {
        return None;
    }
    //@ assert(lo as int / 32 == s as int);" "    let lo = top;
    let hi = if bottom < y1 { bottom } else { y1 };
    if lo >= hi {
        return None;
    }
    //@ assert(lo as int / 32 == s as int);"
try "    let hi = if bottom < y1 { bottom } else { y1 };
    if lo >= hi {
        return None;
    }
    //@ assert(lo as int / 32 == s as int);
    //@ assert((hi as int - 1) / 32 == s as int);
    Some((lo, hi))" "    let hi = bottom;
    if lo >= hi {
        return None;
    }
    //@ assert(lo as int / 32 == s as int);
    //@ assert((hi as int - 1) / 32 == s as int);
    Some((lo, hi))"
try "    let bottom = top + STRIPE;
    let lo = if top > y0 { top } else { y0 };" "    let bottom = top + STRIPE + 1;
    let lo = if top > y0 { top } else { y0 };"
try "    let top = s * STRIPE;
    let bottom = top + STRIPE;
    let lo = if top > y0 { top } else { y0 };" "    let top = (s + 1) * STRIPE;
    let bottom = top + STRIPE;
    let lo = if top > y0 { top } else { y0 };"
try "    if lo >= hi {
        return None;
    }
    //@ assert(lo as int / 32 == s as int);
    //@ assert((hi as int - 1) / 32 == s as int);
    Some((lo, hi))" "    if lo > hi {
        return None;
    }
    //@ assert(lo as int / 32 == s as int);
    //@ assert((hi as int - 1) / 32 == s as int);
    Some((lo, hi))"
try "        return None;
    }
    //@ assert(lo as int / 32 == s as int);" "        return Some((lo, lo));
    }
    //@ assert(lo as int / 32 == s as int);"
try "//@     ensures (s * 32 <= y < s * 32 + 32) == (s == y / 32)," "//@     ensures (s * 32 <= y <= s * 32 + 32) == (s == y / 32),"
# Buffers: stride not padded, offset without the row, a texel one past the row, a short allocation.
try "(row_bytes + 3) / 4 * 4" "(row_bytes + 2) / 4 * 4"
try "Some(y * stride + x * bpp)" "Some(y * stride + (x + 1) * bpp)"
try "if x < 0 || y < 0 || x >= w || y >= h {" "if x < 0 || y < 0 || x > w || y >= h {"
try "let mh = if h > min_h { h } else { min_h };" "let mh = min_h;"
try "Some((y * stride + x0 * bpp, y * stride + x1 * bpp))" "Some((y * stride + x0 * bpp, y * stride + x1 * bpp + 1))"
try "pub const MAX_STRIDE: i64 = 524288;" "pub const MAX_STRIDE: i64 = 524284;"
# Blend table: a wrong entry index, a duplicate entry, a dropped normalisation, a stale fallback, a short search.
try "            return Some(i);" "            return Some(i + 1);"
try "    (GL_ONE, GL_ONE, 0, 0)," "    (GL_ONE, GL_ZERO, 0, 0),"
try "    if srgb == sa && drgb == da {
        return (srgb, drgb, 0, 0);" "    if srgb == sa {
        return (srgb, drgb, 0, 0);"
try "        None => 2," "        None => 40,"
try "    while i < BLEND_KEYS_FROM_STATE" "    while i < BLEND_KEYS_FROM_STATE - 1"
# 8-bit arithmetic: muldiv255 without the +x, source-over without the dst term.
try "let r = (x * y + x) >> 8;" "let r = (x * y + y + x) >> 8;"
try "    src + (dst - m)" "    src + dst"
elif [ "${TARGET}" = blit.rs ]; then
# blit.rs (TARGET=blit.rs): a source index off by one, a start rounded down instead of up, a far edge that reads
# one texel past the texture, a negative start not cut at 0, an end left at 1 for an empty extent, a bound too large.
try "    x * src / dst" "    x * src / dst + 1"
try "let n = t0 * dst + src - 1;" "let n = t0 * dst;"
try "let m = t1 * dst;" "let m = t1 * dst + src;"
try "    let d0 = if t0 <= 0 {
        0" "    let d0 = if t0 <= 0 {
        t0"
try "    let d1 = if t1 <= 0 {
        0" "    let d1 = if t1 <= 0 {
        1"
try "pub const MAX: i64 = 32768;" "pub const MAX: i64 = 1 << 40;"
else
    echo "no mutants for $TARGET"; exit 2
fi
echo "$refused refused"
exit "$fail"
