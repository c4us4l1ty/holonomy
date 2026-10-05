#!/usr/bin/env python3
"""Build the Phase 4 font payload: subset -> one concatenated brotli stream -> a blob + manifest.

Design decisions this script encodes, and why
=============================================

Coverage
--------
ASCII 0x20..0x7E plus Latin-1 Supplement 0xA0..0xFF, taken from the fonts. Box Drawing
0x2500..0x257F is **not** taken from the fonts: it is 128 codepoints of pure line geometry
that we rasterise procedurally at boot (see `box_drawing.rs` in the crate). Inter ships zero
Box Drawing glyphs, so this is the only way to have both Inter and the required coverage --
and it is also better than the font's own glyphs, which round poorly at cell boundaries and
show antialiasing gaps where two box glyphs meet.

One stream, not four
--------------------
All faces are concatenated and compressed as a single brotli stream, with a manifest giving
each face's offset and length. Measured: 56,713 B as four independent streams vs 45,491 B
joined, a 19.8% saving, because the faces share glyph names, table layouts and many
near-identical outlines. The boot path slices the decompressed buffer and hands each face to
`ttf_parser` with no copying.

Subset levers
-------------
Dropped: GSUB/GPOS/GDEF (no shaping -- we draw codepoint to glyph directly, one glyph per
character), `fpgm`/`prep`/`cvt ` (TrueType hinting; irrelevant at these sizes and not used by
our rasteriser), `name` (no UI shows font names), `post` (no glyph names are resolved by
string), `FFTM`/`gasp`/`DSIG` (WOLFF-era metadata). Retained: `glyf`, `loca`, `head`,
`hhea`, `hmtx`, `maxp`, `cmap`, `OS/2` -- everything `ttf_parser` needs to produce outlines
and metrics.

Determinism
-----------
`head.modified` is forced to 0 and `recalcTimestamp` is off, so re-running this on the same
inputs produces byte-identical output. That is checked in the Rust gate by comparing against a
committed hash, which is only meaningful because of it.
"""
from __future__ import annotations

import hashlib
import io
import json
import subprocess
import sys
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont

ROOT = Path(__file__).resolve().parent.parent
FONT_DIR = ROOT / "assets" / "fonts"
OUT_DIR = ROOT / "assets" / "generated"

# Coverage taken from the fonts. Box Drawing is procedural and deliberately absent.
TEXT_RANGES = [(0x20, 0x7E), (0xA0, 0xFF)]

# Box Drawing, for the coverage test only. Nothing here reaches the font payload.
BOX_RANGE = (0x2500, 0x257F)

# Phase 9B's math coverage, and only ever for the math face.
#
# **These are the codepoints `holonomy_render::SYMBOLS` can name, and nothing else.** Every entry
# here is a glyph the layout can ask for, so the list is the parser's symbol table read as ranges.
# `crates/holonomy-render/tests/math_coverage.rs` asserts the two agree in both directions: no
# symbol outside these ranges (`every_symbol_is_inside_a_declared_math_range`) and no symbol missing
# from the face (`every_symbol_the_parser_can_name_is_in_the_math_face`).
#
# # Why this was 421 codepoints and is now 108
#
# The first version listed the *blocks* -- Arrows 0x2190..0x21FF, Greek, and Mathematical Operators
# 0x2200..0x22FF -- which is 421 codepoints and a 507-glyph face. That was defensible while math
# glyphs were rasterised on demand, where a glyph costs nothing unless a formula asks for it.
#
# It stopped being defensible when the atlas became boot-rasterised, because then every listed
# codepoint is a glyph in the coverage *and* a slot in the metric table, whether or not a formula
# ever draws it. `metric::ATLAS_WIDTH`'s comment is the arithmetic: the fifth style and four new
# codepoint windows pushed the pair to 537,720 against a 524,288 ceiling -- over by 13,432 -- and the
# coverage height had to come down from 480 to 448 to pay for it. Pruning to the symbol set is what
# made that trade survivable, and it is the reason the height had to move as little as it did.
#
# So the ranges are now the smallest spans that cover the parser's symbols:
#
# * **Greek** 0x391..0x3C9 as two runs, because 0x3AA..0x3B0 is unassigned and subsetting an
#   unassigned codepoint makes fontTools emit a .notdef glyph, which is worse than omitting it
#   because it *looks* present. The 57-slot metric window does span the gap -- the table needs a
#   contiguous span because `slot_of` is arithmetic, not a search -- but the *font* need not.
# * **Arrows** 0x2190..0x2192, three slots for `\leftarrow` and `\rightarrow`. The block is 112.
# * **Operators** as four short runs rather than the 256-slot block: the 15 operators the parser
#   names cluster into 0x2200..0x222B, then 0x2248, 0x2260..0x2265, and 0x22C5.
# * **Latin-1** `\pm` (U+00B1), `\times` (U+00D7), `\div` (U+00F7), which are already inside the
#   text window. They still have to be in *this* face's range list, because the ranges say what this
#   face carries and the other four do not carry Greek -- but they cost no new metric slots.
MATH_RANGES = [
    (0x00B1, 0x00B1),   # plus-minus
    (0x00D7, 0x00D7),   # multiplication sign
    (0x00F7, 0x00F7),   # division sign
    (0x2190, 0x2192),   # Arrows: leftarrow, rightarrow (and 1 unused slot between them)
    (0x391, 0x3A9),     # Greek, uppercase: Gamma..Upsilon
    (0x3B1, 0x3C9),     # Greek, lowercase: alpha..omega
    (0x2200, 0x222B),   # Operators: forall partial exists nabla in prod mp infty int
    (0x2248, 0x2248),   # approx
    (0x2260, 0x2265),   # neq equiv leq geq
    (0x22C5, 0x22C5),   # cdot
]

FACES = [
    ("Inter-Regular", "Regular", False, "Inter-Regular.ttf", TEXT_RANGES),
    ("Inter-Bold", "Bold", False, "Inter-Bold.ttf", TEXT_RANGES),
    ("Inter-Italic", "Italic", False, "Inter-Italic.ttf", TEXT_RANGES),
    ("JetBrainsMono-Regular", "Monospace", True, "JetBrainsMono-Regular.ttf", TEXT_RANGES),
    # Noto Sans Math is chosen over STIX Two Math for one reason recorded in PROJECT.md 2.9.2: STIX
    # ships CFF outlines, and supporting them means a CFF interpreter, which is not 60 KiB. Noto Sans
    # Math is TrueType, so `ttf_parser` -- already a dependency for the other four faces -- reads it.
    ("NotoSansMath-Regular", "Math", False, "NotoSansMath-Regular.ttf", MATH_RANGES),
]

DROP_TABLES = ["fpgm", "prep", "cvt ", "cvt", "FFTM", "gasp", "DSIG", "GSUB", "GPOS", "GDEF"]

BROTLI_QUALITY = 11


def codepoints(ranges) -> list[int]:
    out: set[int] = set()
    for lo, hi in ranges:
        out.update(range(lo, hi + 1))
    return sorted(out)


def subset_face(path: Path, cps: list[int]) -> bytes:
    opts = subset.Options()
    opts.layout_features = []          # no shaping; belt-and-braces with DROP_TABLES
    opts.name_IDs = []                # drop the name table outright
    opts.name_legacy = False
    opts.glyph_names = False          # drop post
    opts.legacy_kern = False
    opts.notdef_outline = False       # glyph 0 carries no outline
    opts.recommended_glyphs = False   # no .notdef filler for space
    opts.recalc_timestamp = False     # determinism
    opts.drop_tables = list(DROP_TABLES)

    font = TTFont(str(path), recalcBBoxes=False, recalcTimestamp=False)
    have = set(font.getBestCmap())
    # fontTools 4.66 on Python 3.14 raises a TypeError formatting its missing-unicode report,
    # so subsetting an absent codepoint is fatal rather than a warning. Filter first.
    wanted = [c for c in cps if c in have]
    missing = [c for c in cps if c not in have]
    if missing:
        print(f"  note: {path.name} lacks {len(missing)} requested codepoints "
              f"(U+{missing[0]:04X}..U+{missing[-1]:04X})")

    sub = subset.Subsetter(options=opts)
    sub.populate(unicodes=wanted)
    sub.subset(font)

    font["head"].modified = 0          # determinism: no build timestamp
    font["head"].checkSumAdjustment = 0

    buf = io.BytesIO()
    font.save(buf)
    return buf.getvalue()


def brotli(data: bytes, quality: int = BROTLI_QUALITY) -> bytes:
    proc = subprocess.run(
        ["brotli", "-q", str(quality), "-c"], input=data, capture_output=True
    )
    if proc.returncode != 0:
        raise RuntimeError(f"brotli failed: {proc.stderr.decode()[:300]}")
    if not proc.stdout:
        raise RuntimeError("brotli produced no output")
    return proc.stdout


def main() -> int:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    entries = []
    joined = bytearray()
    print(f"subsetting {len(FACES)} faces (Box Drawing 0x2500-0x257F is procedural, not in the "
          f"payload)")
    for name, style, mono, fname, ranges in FACES:
        cps = codepoints(ranges)
        src = FONT_DIR / fname
        raw = subset_face(src, cps)
        entries.append(
            {
                "name": name,
                "style": style,
                "monospace": mono,
                "offset": len(joined),
                "length": len(raw),
                "sha256": hashlib.sha256(raw).hexdigest(),
                "source_sha256": hashlib.sha256(src.read_bytes()).hexdigest(),
            }
        )
        joined += raw
        print(f"  {name:24s} {len(raw):>7,} B raw   glyphs={len(TTFont(io.BytesIO(raw)).getGlyphOrder()):>4}"
              f"   coverage={len(cps)} cps")

    print(f"joined payload: {len(joined):,} B raw")
    packed = brotli(bytes(joined))
    print(f"brotli q{BROTLI_QUALITY}:     {len(packed):,} B "
          f"({len(joined)/len(packed):.2f}x)")

    blob = OUT_DIR / "fonts.bin.br"
    blob.write_bytes(packed)

    manifest = {
        "version": 1,
        "brotli_quality": BROTLI_QUALITY,
        "raw_len": len(joined),
        "packed_len": len(packed),
        "packed_sha256": hashlib.sha256(packed).hexdigest(),
        "raw_sha256": hashlib.sha256(bytes(joined)).hexdigest(),
        "text_ranges": [list(r) for r in TEXT_RANGES],
        "math_ranges": [list(r) for r in MATH_RANGES],
        "box_range": list(BOX_RANGE),
        "box_is_procedural": True,
        "faces": entries,
    }
    (OUT_DIR / "fonts.json").write_text(json.dumps(manifest, indent=2) + "\n")

    print(f"wrote {blob.relative_to(ROOT)} and fonts.json")
    return 0


if __name__ == "__main__":
    sys.exit(main())