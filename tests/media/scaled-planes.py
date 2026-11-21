#!/usr/bin/env python3
"""Is the painted picture a scaled frame, or one source row repeated? (a tracked flaw)

    scaled-planes.py painted <png>                    exit 0 once the box holds a picture, not the #222 background
    scaled-planes.py compare <planes.png> <bgra.png>  the two predictions, printed; exit 1 if the planes are bars

The box is 960x270 at the top left of a 1366x900 window and the coded frame 640x360, so object-fit: contain
paints a 480x270 picture from x=240. Sampling one source row for every output row leaves each of its columns
exactly constant down the height, which is what the bars are; the same frame through the BGRA path is not.
"""

import sys

from PIL import Image

BOX = (240, 0, 720, 270)
TOL = 12
AGREE = 90.0


def picture(path):
    im = Image.open(path).convert("RGB").crop(BOX)
    return im.size, im.load()


def painted(path):
    (w, h), px = picture(path)
    colors = {px[x, y] for y in range(h) for x in range(w)}
    if len(colors) < 8:
        print(f"not painted: {len(colors)} colors in the box")
        return 1
    print(f"painted: {len(colors)} colors in the box")
    return 0


def compare(planes, bgra):
    (w, h), got = picture(planes)
    _, want = picture(bgra)
    bars = sum(1 for x in range(w) if len({got[x, y] for y in range(h)}) == 1)
    same = sum(
        1
        for y in range(h)
        for x in range(w)
        if all(abs(a - b) <= TOL for a, b in zip(got[x, y], want[x, y]))
    )
    agree = same / (w * h) * 100
    print(f"columns constant down their height: {bars}/{w}")
    print(f"pixels within {TOL} of the BGRA render: {agree:.1f}%")
    if bars or agree < AGREE:
        print("FAIL: the planes picture is one row repeated, not the scaled frame")
        return 1
    print("PASS")
    return 0


def main():
    what, rest = sys.argv[1], sys.argv[2:]
    if what == "painted":
        sys.exit(painted(rest[0]))
    sys.exit(compare(*rest))


main()
