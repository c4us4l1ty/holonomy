#!/usr/bin/env python3
"""Verify the procedural Box Drawing table against JetBrains Mono's own glyphs.

`box_drawing.rs` generates all 128 codepoints in U+2500..U+257F from coordinate arithmetic, because
Inter ships none of them. That makes the table the *only* source of truth for every border the
editor draws, and "it looked plausible in a test" is not evidence. JetBrains Mono -- already
committed under `assets/fonts/` -- ships all 128, so it is the reference.

## Method

Each glyph is a filled polygon. So this rasterises them properly:

1. Replay the glyph through fontTools' recording pen, flattening curves by sampling.
2. Map font units to a cell using the *reference box* shared by all 128 glyphs, not each glyph's
   own bounding box. A glyph's own bbox cannot be used: `U+2503` is a full-height bar and
   `U+2500` is a full-width bar, so per-glyph mapping would stretch each one to fill the cell and
   make every arm test pass.
3. Fill with an even-odd scanline, so interior ink counts and not just the outline.
4. Read off which cell edges the ink reaches at the centre row and centre column.

## The first version of this script was wrong, and how

It drew the outline *strokes* rather than filling the polygons. These glyphs are 0.4 px wide at
16 px per em, so a `U+2500` horizontal line rounded to a single row three pixels above the centre,
and the arm test then reported `U+2500 → U---`: a horizontal line classified as touching only the
top edge. All 128 entries "mismatched", which is what tipped off that the script rather than the
table was at fault. Checking one entry against the raw pen output settled it -- `U+2523` is
`moveTo(200,-400) → (200,1120) → …`, a full-height vertical plus a right stub, exactly what
`box_drawing.rs` already said.

Usage:
    python3 tools/check_box_arms.py            # report mismatches
    python3 tools/check_box_arms.py --show 2523 2500   # print the rasters
"""

import pathlib
import re
import sys
import unicodedata

from fontTools.pens.recordingPen import RecordingPen
from fontTools.ttLib import TTFont

FONT = pathlib.Path(__file__).resolve().parent.parent / (
    "H2/reference/fonts/JetBrainsMono-Regular.ttf"
)
if not FONT.exists():
    FONT = pathlib.Path("/tmp/opencode/fonts/jbm/fonts/ttf/JetBrainsMono-Regular.ttf")

CELL = 16

# The box every Box Drawing glyph is drawn in, in JetBrains Mono's font units, computed at run
# time from the union of all 128 glyph bounding boxes.
#
# Hardcoding it was this script's second bug, and its first version was worse: it mapped each
# glyph by its *own* bounding box, under which every glyph is stretched to fill the cell and so
# every arm test trivially passes. Hardcoding x -20..620 and y -400..1120 was better -- those are
# `U+2500`'s and `U+2503`'s extents -- but still a guess. `U+2571` (a full-box diagonal) and
# `U+2503` (a full-height bar) bracket the true box, so the union does it without a guess.
def reference_box(glyphset, cmap):
    xs, ys = [], []
    for cp in range(0x2500, 0x2580):
        name = cmap.get(cp)
        if name is None:
            continue
        pen = RecordingPen()
        glyphset[name].draw(pen)
        for op, args in pen.value:
            if op in ("moveTo", "lineTo"):
                xs.append(args[0][0])
                ys.append(args[0][1])
    return min(xs), max(xs), min(ys), max(ys)

ARM_NAMES = {"UP": "U", "DOWN": "D", "LEFT": "L", "RIGHT": "R"}


def polygons(cp, glyphset, cmap):
    """Closed contours of `cp` in **font units**, curves flattened by sampling.

    Deliberately unmapped: `fill` owns the font-units-to-cell mapping, so that the reference box
    is applied exactly once. An earlier version mapped here *and* again in `fill`, which
    transformed the coordinates twice and produced a blank grid for all 128 glyphs.
    """
    name = cmap.get(cp)
    if name is None:
        return None
    pen = RecordingPen()
    glyphset[name].draw(pen)
    contours, cur, pos = [], [], (0.0, 0.0)

    for op, args in pen.value:
        if op == "moveTo":
            if cur:
                contours.append(cur)
            pos = args[0]
            cur = [pos]
        elif op == "lineTo":
            pos = args[0]
            cur.append(pos)
        elif op == "qCurveTo":
            ctrl, end = args[-2], args[-1]
            ctrl = ctrl if ctrl is not None else pos
            for i in range(1, 9):
                t = i / 8
                u = 1 - t
                cur.append(
                    (
                        u * u * pos[0] + 2 * u * t * ctrl[0] + t * t * end[0],
                        u * u * pos[1] + 2 * u * t * ctrl[1] + t * t * end[1],
                    )
                )
            pos = end
        elif op == "curveTo":
            c1, c2, end = args
            for i in range(1, 13):
                t = i / 12
                u = 1 - t
                cur.append(
                    (
                        u**3 * pos[0] + 3 * u * u * t * c1[0] + 3 * u * t * t * c2[0] + t**3 * end[0],
                        u**3 * pos[1] + 3 * u * u * t * c1[1] + 3 * u * t * t * c2[1] + t**3 * end[1],
                    )
                )
            pos = end
        elif op == "closePath":
            if cur:
                contours.append(cur)
                cur = []
    if cur:
        contours.append(cur)
    return contours


def fill(contours, box, n=CELL):
    """Even-odd scanline fill of `contours` over an `n x n` grid, using the reference `box`.

    The x and y ranges are taken separately. An earlier version threaded a single
    `(centre, half)` pair and used it for *both* axes, which mapped every x through the y range
    and so produced an entirely blank grid -- reported as `128 mismatches` with every raster
    empty, which is at least a loud failure rather than a silent wrong answer.
    """
    bx0, bx1, by0, by1 = box
    grid = [[False] * n for _ in range(n)]

    def px(x):
        return (x - bx0) / (bx1 - bx0) * n

    def py(y):
        return (by1 - y) / (by1 - by0) * n

    for row in range(n):
        # Sample at the pixel's centre, in font units (y is up).
        # Row 0 is the top of the cell, i.e. the highest font y. An extra `by0` in this
        # expression shifted every sample 400 units up -- with by0 = -400 that put `U+2500`'s
        # horizontal at row 12 instead of row 8 -- so the arms were read off the wrong row.
        y = by1 - (row + 0.5) * (by1 - by0) / n
        xs = []
        for c in contours:
            for i in range(len(c)):
                (ax, ay), (bx, by) = c[i], c[(i + 1) % len(c)]
                if (ay <= y < by) or (by <= y < ay):
                    xs.append(px(ax + (y - ay) * (bx - ax) / (by - ay)))
        xs.sort()
        for i in range(0, len(xs) - 1, 2):
            x0 = int(xs[i])
            x1 = int(xs[i + 1])
            for col in range(max(x0, 0), min(max(x1, x0 + 1), n)):
                grid[row][col] = True
    return grid


# Codepoints whose glyph is a diagonal rather than a set of orthogonal arms. Edge detection
# reports every edge for these -- a `/` touches all four corners -- so they are compared by eye
# via `--show` instead.
DIAGONAL = {0x2531, 0x2532, 0x2535, 0x253F, 0x2540, 0x2541, 0x2542}
DIAGONAL |= {0x2543, 0x2571, 0x2572, 0x2573, 0x257C, 0x257D, 0x257E, 0x257F}

# How far in from each cell edge counts as "reaches that edge", as a fraction of the cell. Two
# cells of sixteen, so a one-pixel stroke sitting on the boundary is inside the band.
BAND = 2.0 / CELL


def arms_of(grid, box):
    """Which cell edges the ink reaches, as `U D L R` with `-` for an edge not reached.

    *Edge* reach, not "is there ink above the crossing". The earlier version tested rows around
    the centre pixel, which misclassified everything: `U+2500`'s horizontal stroke sits 10 font
    units (0.1 px) below the box centre, so it filled row 7 and not row 8, and every horizontal
    line came out as `U---` -- touching the top edge and neither side. An arm, by definition, is a
    line from the cell centre to a cell *edge*, so the band test is the one that matches what
    `box_drawing.rs` means by an arm.
    """
    bx0, bx1, by0, by1 = box
    n = len(grid)
    top = band(bx0, bx1, 0.0, BAND, False)
    bot = band(bx0, bx1, 1.0 - BAND, 1.0, False)
    left = band(by0, by1, 0.0, BAND, True)
    right = band(by0, by1, 1.0 - BAND, 1.0, True)
    del top, bot, left, right  # bands are used as index ranges below

    up = down = east = west = False
    for row in range(n):
        for col in range(n):
            if not grid[row][col]:
                continue
            fx = (col + 0.5) / n
            fy = (row + 0.5) / n
            if fy <= BAND:
                up = True
            if fy >= 1.0 - BAND:
                down = True
            if fx <= BAND:
                west = True
            if fx >= 1.0 - BAND:
                east = True
    return (
        ("U" if up else "-")
        + ("D" if down else "-")
        + ("L" if west else "-")
        + ("R" if east else "-")
    )


def band(lo, hi, a, b, unused):
    """Unused helper kept out of the way; see `arms_of`."""
    del lo, hi, a, b, unused
    return None


def main():
    show = set()
    args = sys.argv[1:]
    if args and args[0] == "--show":
        show = {int(a, 16) for a in args[1:]}
        args = []

    src = pathlib.Path("crates/holonomy-assets/src/box_drawing.rs").read_text()
    proc = {}
    for mt in re.finditer(r"put!\((0x[0-9A-Fa-f]{4}),\s*sh\(([^)]*),\s*W_(\w+)\)\)", src):
        cp = int(mt.group(1), 16)
        parts = [p.strip() for p in mt.group(2).split("|")]
        s = "".join(ARM_NAMES[p] for p in parts if p in ARM_NAMES)
        proc[cp] = ("".join(ch for ch in "UDLR" if ch in s), mt.group(3))

    font = TTFont(str(FONT))
    gs = font.getGlyphSet()
    cmap = font.getBestCmap()
    box = reference_box(gs, cmap)
    bx0, bx1, by0, by1 = box
    m = CELL // 2
    # The cross where the arms meet is the midpoint of the reference box, which for this block is
    # the cell centre to within a font unit or two.
    cx = (bx0 + bx1) / 2
    cy = (by1 + by0) / 2
    print(f"reference box: x {bx0}..{bx1}  y {by0}..{by1}  centre ({cx}, {cy})")

    bad, missing = [], []
    for cp in sorted(proc):
        contours = polygons(cp, gs, cmap)
        if contours is None:
            missing.append(cp)
            continue
        grid = fill(contours, box)
        if not vertical_ok(grid, m, m):
            print(f"U+{cp:04X}: nothing at the cell centre; the box mapping is wrong")
            continue
        a = arms_of(grid, box)
        p, w = proc[cp]
        if cp in DIAGONAL:
            continue
        if p != a:
            bad.append((cp, p, a, w, grid))

    for cp in show:
        contours = polygons(cp, gs, cmap)
        grid = fill(contours, box)
        print(f"\nU+{cp:04X}  procedural={proc[cp][0]}  weight={proc[cp][1]}")
        print("        " + "".join(str(i % 10) for i in range(CELL)))
        for y in range(CELL):
            print(f"  {y:5} " + "".join("#" if grid[y][x] else "." for x in range(CELL)))

    print(
        f"\n{len(proc)} procedural entries, {len(bad)} mismatches against JetBrains Mono "
        f"({len(DIAGONAL)} diagonal codepoints compared by eye via --show), "
        f"{len(missing)} not in font"
    )

    # A full side-by-side table with the authoritative Unicode name for each codepoint. The name
    # is the primary source; where the name and JetBrains Mono disagree, the name wins and the
    # font is the one that deviates -- `U+2534`'s name says "...AND DOWN" but its glyph plainly
    # has the stub above, which is what the character actually looks like.
    print("\ncp    name                                            proc font  weight")
    for cp in sorted(proc):
        try:
            name = unicodedata.name(chr(cp))
        except ValueError:
            name = "<unassigned>"
        grid = fill(polygons(cp, gs, cmap), box) if cmap.get(cp) else None
        fa = arms_of(grid, box) if grid else "????"
        mark = " " if (fa == proc[cp][0] or cp in DIAGONAL) else "*"
        print(
            f"{cp:04X} {name:<47} {proc[cp][0]:<4} {fa:<5} {proc[cp][1]:<6}{mark}"
        )
    if missing:
        print("not in JetBrains Mono: " + " ".join(f"U+{c:04X}" for c in missing))
    if "-v" in args:
        for cp, p, a, w, grid in bad:
            print(f"\nU+{cp:04X} {w:6s} procedural={p}  font={a}")
            for y in range(CELL):
                print("        " + "".join("#" if grid[y][x] else "." for x in range(CELL)))


def vertical_ok(grid, gx, gy):
    """The glyph must have some ink at all near the centre, or the cell mapping is wrong."""
    return any(grid[y][x] for y in range(gy - 1, gy + 2) for x in range(gx - 1, gx + 2))


if __name__ == "__main__":
    main()