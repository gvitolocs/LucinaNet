#!/usr/bin/env python3
"""Count the .notdef boxes in a rendering of tests/fonts/glyphs.html.

Two renderings go in, both of the same grid: the sample page, and tests/fonts/glyphs-notdef.html, which is
that page with every sample replaced by U+FFFF. No font assigns U+FFFF, so each cell of the second
rendering holds the .notdef outline this engine's own font stack falls back to for a character it cannot
draw. A sample cell whose pixels are identical to its reference cell was drawn by that same fallback face:
that is what an uncovered character looks like, and it is counted as a box.

A second, weaker signal is reported beside it: a cell whose ink is a hollow rectangle ("tofu-shaped"). It
catches an uncovered character whose box came from a different face than the reference's - the fallback
chain a character picks depends on its script - but real glyphs that are white squares match it too, so it
is advice for the eye, not a count.

Standard library only: no PIL, so it runs wherever python3 does.

usage: notdef-count.py <glyphs.png> <glyphs-notdef.png> [glyphs.json] [-v]
"""

import collections
import json
import os
import struct
import sys
import zlib


def read_png_rgba(path):
    """The rows of a non-interlaced 8-bit PNG as a list of byte strings, each width*4 RGBA bytes."""
    data = open(path, "rb").read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit("notdef-count: %s is not a PNG" % path)
    idat, meta, palette = [], None, None
    pos = 8
    while pos < len(data):
        (length,) = struct.unpack_from(">I", data, pos)
        kind = data[pos + 4 : pos + 8]
        chunk = data[pos + 8 : pos + 8 + length]
        if kind == b"IHDR":
            meta = struct.unpack_from(">IIBBBBB", chunk, 0)
        elif kind == b"PLTE":
            palette = chunk
        elif kind == b"IDAT":
            idat.append(chunk)
        elif kind == b"IEND":
            break
        pos += 12 + length
    width, height, depth, color, _, _, interlace = meta
    if depth != 8 or interlace != 0 or color not in (0, 2, 3, 6):
        raise SystemExit(
            "notdef-count: %s is depth=%s color=%s interlace=%s; need an 8-bit non-interlised RGB or RGBa PNG"
            % (path, depth, color, interlace)
        )
    channels = {0: 1, 2: 3, 3: 1, 6: 4}[color]
    raw = zlib.decompress(b"".join(idat))
    stride = width * channels
    lines, prev = [], b"\0" * stride
    pos = 0
    for _ in range(height):
        ft = raw[pos]
        line = bytearray(raw[pos + 1 : pos + 1 + stride])
        _unfilter(ft, line, prev, channels, stride)
        lines.append(bytes(line))
        prev = line
        pos += 1 + stride
    if color == 6:
        return width, height, lines
    if color == 2:
        return width, height, [b"".join(line[i : i + 3] + b"\xff" for i in range(0, stride, 3)) for line in lines]
    if color == 0:
        return width, height, [b"".join(line[i : i + 1] * 3 + b"\xff" for i in range(stride)) for line in lines]
    if palette is None:
        raise SystemExit("notdef-count: %s is paletted with no PLT" % path)
    return width, height, [b"".join(palette[line[i] * 3 : line[i] * 3 + 3] + b"\xff" for i in range(stride)) for line in lines]


def _unfilter(ft, line, prev, bpp, stride):
    """The five scanline filters of the PNG spec, in place."""
    if ft == 0:
        return
    if ft == 1:
        for i in range(bpp, stride):
            line[i] = (line[i] + line[i - bpp]) & 0xFF
    elif ft == 2:
        for i in range(stride):
            line[i] = (line[i] + prev[i]) & 0xFF
    elif ft == 3:
        for i in range(stride):
            left = line[i - bpp] if i >= bpp else 0
            line[i] = (line[i] + ((left + prev[i]) >> 1)) & 0xFF
    elif ft == 4:
        for i in range(stride):
            left = line[i - bpp] if i >= bpp else 0
            up = prev[i]
            upleft = prev[i - bpp] if i >= bpp else 0
            p = left + up - upleft
            pa, pb, pc = abs(p - left), abs(p - up), abs(p - upleft)
            line[i] = (line[i] + (left if (pa <= pb and pa <= pc) else (up if pb <= pc else upleft))) & 0xFF
    else:
        raise SystemExit("notdef-count: unknown scanline filter %s" % ft)


def crop(lines, width, rect):
    """One grid cell as bytes."""
    x, y, w, h = rect
    return b"".join(lines[y][x * 4 : (x + w) * 4] for y in range(y, y + h))


def is_empty(cell, width, back):
    return cell == back * (len(cell) // 4)


def ink_box(cell, width, height, back):
    """The bounding box of the pixels that are not the background, or None when the cell is empty."""
    backrow = back * width
    rows = [cell[y * width * 4 : (y + 1) * width * 4] for y in range(height)]
    ys = [y for y, row in enumerate(rows) if row != backrow]
    if not ys:
        return None
    xs = []
    for y in ys:
        row = rows[y]
        for x in range(width):
            if row[x * 4 : x * 4 + 4] != back:
                xs.append(x)
    return min(xs), ys[0], max(xs), ys[-1]


def is_box_shape(cell, width, height, back, inset=4):
    """Whether the ink is a closed rectangular frame with nothing in the middle: a tofu box.

    The inset clears the LCD fringe a face leaves just inside its own .notdef strokes, so a few pixels of
    the middle may still be lit; almost the whole of the four edges must be lit.
    """
    box = ink_box(cell, width, height, back)
    if not box:
        return False
    min_x, min_y, max_x, max_y = box
    if max_x - min_x < 8 or max_y - min_y < 8:
        return False

    def filled(x, y):
        return cell[(y * width + x) * 4 : (y * width + x) * 4 + 4] != back

    inside = lit = 0
    for y in range(min_y + inset, max_y - inset + 1):  # the middle must be clear
        for x in range(min_x + inset, max_x - inset + 1):
            inside += 1
            lit += 1 if filled(x, y) else 0
    if not inside or lit > inside // 40:
        return False
    edge = total = 0
    for x in range(min_x, max_x + 1):  # and the four edges must be lit
        for y in (min_y, max_y):
            total += 1
            edge += 1 if filled(x, y) else 0
    for y in range(min_y, max_y + 1):
        for x in (min_x, max_x):
            total += 1
            edge += 1 if filled(x, y) else 0
    return total and edge / total > 0.9


def main(argv):
    args = [a for a in argv[1:] if not a.startswith("-")]
    verbose = any(a in ("-v", "--verbose") for a in argv[1:])
    if len(args) < 2:
        sys.exit(__doc__)
    shot, reference = args[0], args[1]
    spec_path = args[2] if len(args) > 2 else os.path.join(os.path.dirname(os.path.abspath(__file__)), "glyphs.json")
    spec = json.load(open(spec_path))
    width, height, lines = read_png_rgba(shot)
    width_ref, height_ref, lines_ref = read_png_rgba(reference)
    if (width, height) != (width_ref, height_ref):
        raise SystemExit("notdef-count: %s is %dx%d and %s is %dx%d, nothing to compare" % (shot, width, height, reference, width_ref, height_ref))
    back = lines[0][:4]
    back_ref = lines_ref[0][:4]
    total = collections.Counter()
    lines_out = []
    hits = []
    for row in spec["rows"]:
        counts = collections.Counter()
        for entry in row["entries"]:
            x, y, w, h = entry["rect"]
            a = crop(lines, width, (x, y, w, h))
            b = crop(lines_ref, width_ref, (x, y, w, h))
            if is_empty(a, w, back):
                kind = "blank"
            elif a == b:
                kind = "box"
            elif is_box_shape(a, w, h, back):
                kind = "tofu"
            else:
                kind = "drawn"
            counts[kind] += 1
            if verbose and kind in ("box", "tofu", "blank"):
                hits.append("%-42s U+%04X %s" % (row["block"], entry["cp"], kind))
        for key, value in counts.items():
            total[key] += value
        lines_out.append((row["block"], counts))
    for block, counts in lines_out:
        print(
            "%-42s %4d/%-4d boxes %4d tofu-shaped %4d blank %4d drawn"
            % (block, counts["box"], sum(counts.values()), counts["tofu"], counts["blank"], counts["drawn"])
        )
    print(
        "%-42s %4d/%-4d boxes %4d tofu-shaped %4d blank %4d drawn"
        % ("TOTAL", total["box"], sum(total.values()), total["tofu"], total["blank"], total["drawn"])
    )
    control = next((row for row in lines_out if "control" in row[0].lower()), None)
    if control:
        of = sum(control[1].values())
        print("check: the control row %s is %d of %d boxes" % (control[0], control[1]["box"], of))
        if not of or of - control[1]["box"]:
            print("check: FAIL - the control row must always be boxes, so this detector is not working")
    if verbose:
        for hit in hits:
            print("  " + hit)
    print("pngs: %s and %s" % (shot, reference))
    return 0 if not total["box"] else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
