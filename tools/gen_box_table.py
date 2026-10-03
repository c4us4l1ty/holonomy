#!/usr/bin/env python3
"""Emit the corrected `put!` lines for `box_drawing.rs` from measured glyph geometry.

Reads JetBrains Mono's rasters for U+2500..U+257F -- the reference committed under
`assets/fonts/` -- and derives each glyph's arm set from which cell *edges* its ink reaches. Where
the edge-band test cannot decide, the Unicode character name is used instead:

* **Dashes** (`U+2504`..`U+250B`, `U+254C`..`U+254F`) have gaps that stop short of the cell edges,
  so the band test reports nothing. Their names say HORIZONTAL or VERTICAL, which is the answer.
* **Diagonals** (`U+2571`..`U+2573`) touch all four edges, so the band test reports all four. They
  are the only three diagonals in the block and are written by hand.

Emits the table grouped by weight, with the Unicode name in the comment so the file is auditable
without running anything.
"""
import pathlib
import re
import sys
import unicodedata

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from check_box_arms import (  # noqa: E402
    CELL,
    TTFont,
    FONT,
    arms_of,
    fill,
    polygons,
    reference_box,
    vertical_ok,
)

FIRST, LAST = 0x2500, 0x257F

# Weight, from the character name.
#
#   "DOUBLE" in the name, or the codepoint is in U+2550..U+256C   -> W_DOUBLE
#   "HEAVY" and no "LIGHT"                                        -> W_HEAVY
#   "LIGHT" and no "HEAVY"                                        -> W_LIGHT
#   both, or neither                                              -> W_LIGHT
#
# A name carrying both weights, like U+2525's "VERTICAL LIGHT AND LEFT HEAVY", describes a
# two-weight cross. This generator has one weight per glyph, so it takes the light one. That is a
# real simplification and is recorded as such rather than papered over: the mixed-weight glyphs are
# U+251D..U+2530 and U+2552..U+2565.
def weight_of(cp, name):
    if 0x2550 <= cp <= 0x256C or "DOUBLE" in name:
        return "W_DOUBLE"
    if "HEAVY" in name and "LIGHT" not in name:
        return "W_HEAVY"
    return "W_LIGHT"

# Only U+2571, U+2572 and U+2573 are diagonals in this block. Eleven other codepoints were drawn
# as diagonals by an earlier revision and are in fact orthogonal: `U+2531`/`U+2532`/`U+2535` are
# half-weight crosses, `U+253F`..`U+2543` are full crosses, and `U+257C`..`U+257F` are half-weight
# bars. Verified against the rasters; see the module doc of `box_drawing.rs`.
DIAGONALS = {
    0x2571: "DIAG_UP_RIGHT",
    0x2572: "DIAG_UP_LEFT",
    0x2573: "DIAG_UP_RIGHT | DIAG_UP_LEFT",
}

# Names that fix an arm the band test cannot see.
BY_NAME = {
    0x2504: "LR", 0x2505: "LR", 0x2506: "UD", 0x2507: "UD",
    0x2508: "LR", 0x2509: "LR", 0x250A: "UD", 0x250B: "UD",
    0x254C: "LR", 0x254D: "LR", 0x254E: "UD", 0x254F: "UD",
    # Rounded corners. Their ink is a quarter arc centred on the *corner*, so nothing lands at the
    # cell centre and the centre-pixel precondition rejects them. Their names are unambiguous:
    # U+256D `╭` ARC DOWN AND RIGHT, U+256E `╮` ARC DOWN AND LEFT, U+256F `╯` ARC UP AND LEFT,
    # U+2570 `╰` ARC UP AND RIGHT. (Note the previous table had these three wrong and, for
    # U+256E and U+256F, had each other's arms -- so `╮` rendered in the wrong corner.)
    0x256D: "DR", 0x256E: "DL", 0x256F: "UL", 0x2570: "UR",
}

ARM = {"U": "UP", "D": "DOWN", "L": "LEFT", "R": "RIGHT"}


def main():
    font = TTFont(str(FONT))
    gs = font.getGlyphSet()
    cmap = font.getBestCmap()
    box = reference_box(gs, cmap)
    m = CELL // 2

    arms = {}
    for cp in range(FIRST, LAST + 1):
        if cp in DIAGONALS:
            arms[cp] = DIAGONALS[cp]
            continue
        if cp in BY_NAME:
            arms[cp] = " | ".join(ARM[c] for c in BY_NAME[cp])
            continue
        grid = fill(polygons(cp, gs, cmap), box)
        if not vertical_ok(grid, m, m):
            raise SystemExit(f"U+{cp:04X}: nothing at the cell centre; the box mapping is wrong")
        got = "".join(c for c in arms_of(grid, box) if c != "-")
        if not got:
            raise SystemExit(
                f"U+{cp:04X}: measured no arms at all. The band test cannot see a "
                f"glyph that stops short of every cell edge; add it to BY_NAME."
            )
        arms[cp] = " | ".join(ARM[c] for c in got)

    for cp in range(FIRST, LAST + 1):
        name = unicodedata.name(chr(cp))
        w = "W_ROUND" if cp in (0x256D, 0x256E, 0x256F, 0x2570) else weight_of(cp, name)
        print(
            f"    put!(0x{cp:04X}, sh({arms[cp]}, {w})); "
            f"// {chr(cp)} {name.split(' ', 2)[2]}"
        )


if __name__ == "__main__":
    main()
