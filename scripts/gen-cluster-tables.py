#!/usr/bin/env python3
"""Generate `crates/holonomy-eye/src/cluster/tables.rs` from the Unicode Character Database.

The eye segments text by UAX #29, and UAX #29 is a set of rules *over* property data. The rules are
ours and are tested as rules; the data is the database's, and data typed by hand is data that has
holes (AUDIT OQ-11). This script turns the database into the tables.

Input
-----
Two generated sources from `unicode-rs`, both generated from the UCD and carrying the Unicode
version they were built from: `unicode-segmentation`'s `src/tables.rs` (for grapheme cluster break
classes and `InCB`) and `unicode-width`'s `src/gen/tables.rs` (for UAX #11 / emoji two-column
display width, OQ-25):

    curl -sSL \
        https://codeload.github.com/unicode-rs/unicode-segmentation/tar.gz/refs/heads/master \\
        | tar xz -C /tmp
    curl -sSL \
        https://codeload.github.com/unicode-rs/unicode-width/tar.gz/refs/heads/master \\
        | tar xz -C /tmp
    scripts/gen-cluster-tables.py \\
        /tmp/unicode-segmentation-master/src/tables.rs \\
        /tmp/unicode-width-master/src/gen/tables.rs

Both `unicode-segmentation` and `unicode-width` are Copyright the Rust Project Developers, dual
MIT/Apache-2.0; the data is Unicode's, under the Unicode license. `THIRD_PARTY.md` records all
three. Nothing in this script's output is code from those projects: it is the property *data*, laid
out in our own types.

What is dropped
---------------
`GC_LV` and `GC_LVT` — the precomposed Hangul syllables are arithmetic, not data, and 798 ranges of
arithmetic is a table that has to be regenerated to stay true. The eye computes them.

What is derived rather than read
--------------------------------
`InCB=Extend` is not in any table here: UAX #29 defines it as
`[gcb=Extend gcb=ZWJ] - InCB=Linker - InCB=Consonant - U+200C`, so `cluster.rs` derives it from the
break class it already has. `InCB=Consonant` and `InCB=Linker` are data and are emitted below.
"""

import re
import sys

CATEGORIES = {
    "GC_CR": "Cr",
    "GC_LF": "Lf",
    "GC_Control": "Control",
    "GC_Extend": "Extend",
    "GC_ZWJ": "Zwj",
    "GC_Regional_Indicator": "RegionalIndicator",
    "GC_Prepend": "Prepend",
    "GC_SpacingMark": "SpacingMark",
    "GC_L": "HangulL",
    "GC_V": "HangulV",
    "GC_T": "HangulT",
    "GC_Extended_Pictographic": "Pictographic",
}

DROPPED = {"GC_LV", "GC_LVT", "GC_Any"}


def parse(source: str):
    start = source.index("const grapheme_cat_table")
    end = source.index("];", start)
    # The table is laid out to a width, so a row can be broken across lines — between the literal
    # and the class, or between the two literals. Collapse the whitespace before reading rows, and
    # then insist that every class token in the region became a row: the count is the guard against
    # a row shape this expression does not read (which is how a wrong table would look plausible).
    body = re.sub(r"\s+", " ", source[start:end])
    rows = re.findall(r"\('\\u\{([0-9a-f]+)\}', '\\u\{([0-9a-f]+)\}', (GC_\w+)\)", body)
    tokens = re.findall(r"GC_\w+\)", body)
    assert len(rows) == len(tokens), f"{len(rows)} rows for {len(tokens)} class tokens"

    breaks = []
    consonants = []
    for low, high, category in rows:
        low, high = int(low, 16), int(high, 16)
        if category == "GC_InCB_Consonant":
            # Not a break class: these are `gcb=Other` consonants the conjunct rule keys on, so
            # they are `Other` to `BREAK` and a range of their own here.
            consonants.append((low, high))
        elif category not in DROPPED:
            breaks.append((low, high, CATEGORIES[category]))
    breaks.sort()
    consonants.sort()

    linkers = parse_linkers(source)
    version = re.search(r"UNICODE_VERSION: \(u64, u64, u64\) = \((\d+), (\d+), (\d+)\)", source)
    return breaks, consonants, linkers, ".".join(version.groups()) if version else "unknown"


def parse_linkers(source: str):
    """The `InCB=Linker` code points, from the `matches!` that spells them out."""
    start = source.index("pub fn is_incb_linker")
    # The literals contain `}`, so the function ends at the first `}` on its own line.
    body = source[start : source.index("\n}\n", start)]
    linkers = set()
    for point in re.findall(r"'\\u\{([0-9a-fA-F]+)\}'", body):
        linkers.add(int(point, 16))
    for low, high in re.findall(r"'\\u\{([0-9a-fA-F]+)\}'\.\.='\\u\{([0-9a-fA-F]+)\}'", body):
        linkers.update(range(int(low, 16), int(high, 16) + 1))
    assert linkers, "no linkers found — the source's shape changed"
    return sorted(linkers)


def parse_wide(width_source: str, expected_version: str):
    """Two-column code-point ranges (UAX #11 Wide/Fullwidth + default Emoji_Presentation)."""
    ver = re.search(r"UNICODE_VERSION: \(u8, u8, u8\) = \((\d+), (\d+), (\d+)\)", width_source)
    width_version = ".".join(ver.groups()) if ver else "unknown"
    assert width_version == expected_version, f"version mismatch: {width_version} != {expected_version}"

    def parse_bytes(name: str):
        start = width_source.index(f"static {name}")
        end = width_source.index(");", start)
        return [int(tok, 16) for tok in re.findall(r"0x[0-9a-fA-F]+", width_source[start:end])]

    root = parse_bytes("WIDTH_ROOT:")
    mid_flat = parse_bytes("WIDTH_MIDDLE:")
    leaves_flat = parse_bytes("WIDTH_LEAVES:")
    mid = [mid_flat[i : i + 64] for i in range(0, len(mid_flat), 64)]
    leaves = [leaves_flat[i : i + 32] for i in range(0, len(leaves_flat), 32)]

    narrow_special = {0xA, 0x5DC, 0x17D8, 0x1A10, 0xFE01, 0xFE0E, 0xFE0F, 0x10C03, 0x16D67, 0x16D68}

    def is_narrow_special(cp: int) -> bool:
        return (
            cp in narrow_special
            or 0x622 <= cp <= 0x882
            or 0x1780 <= cp <= 0x17AF
            or 0x2D31 <= cp <= 0x2D6F
            or 0xA4FC <= cp <= 0xA4FD
            or 0x1F1E6 <= cp <= 0x1F1FF
        )

    ranges = []
    for cp in range(0xA0, 0x110000):
        if 0xD800 <= cp <= 0xDFFF:
            continue
        t1 = root[cp >> 13]
        t2 = mid[t1][(cp >> 7) & 0x3F]
        packed = leaves[t2][(cp >> 2) & 0x1F]
        raw = (packed >> (2 * (cp & 3))) & 3
        if raw == 2 or (raw == 3 and not is_narrow_special(cp)):
            if ranges and ranges[-1][1] + 1 == cp:
                ranges[-1] = (ranges[-1][0], cp)
            else:
                ranges.append((cp, cp))
    return ranges


def check(ranges):
    previous = None
    for low, high in ranges:
        assert low <= high, f"inverted range {low:#x}..{high:#x}"
        if previous is not None:
            assert previous < low, f"ranges overlap or are unsorted at {low:#x}"
        previous = high
        assert 0x0 <= low <= 0x10FFFF and 0x0 <= high <= 0x10FFFF, "outside Unicode"


def render(breaks, consonants, linkers, wide_ranges, version):
    lines = [
        "//! The Unicode property data cluster segmentation and cell width read (AUDIT D-27, D-28, OQ-25).",
        "//!",
        f"//! Generated from the Unicode Character Database {version} by",
        "//! `scripts/gen-cluster-tables.py` — **do not edit this file by hand**. The rules",
        "//! that read it are `cluster.rs`'s and are tested there; this is the data they read, and",
        "//! the reason it is data is OQ-11: a hand-written table covers the scripts one thinks",
        "//! of, and a mark from a script nobody thought of splits from its base, silently.",
        "//!",
        "//! Four items, in the order the standard defines them: the grapheme cluster break class",
        "//! (GB3–GB999 read it), `InCB=Consonant` and `InCB=Linker` (GB9c reads those two), and",
        "//! the two-column terminal display-width ranges (`WIDE_RANGES`, UAX #11 Wide/Fullwidth",
        "//! plus default emoji presentation, OQ-25). `InCB=Extend` is *derived* by `cluster.rs`",
        "//! rather than stored, because UAX #29 defines it as",
        "//! `[gcb=Extend gcb=ZWJ] - InCB=Linker - InCB=Consonant - U+200C`: a formula over data",
        "//! already here, and a formula cannot disagree with what it reads.",
        "//!",
        "//! One thing is deliberately absent from `BREAK`. The precomposed Hangul syllables are",
        "//! arithmetic (`cluster.rs` computes LV/LVT from the jamo), so their 798 ranges are not",
        "//! duplicated there.",
        "//!",
        "//! The data is Unicode's, under the Unicode license; the extraction follows",
        "//! `unicode-segmentation` and `unicode-width`, both dual MIT/Apache-2.0. All three are",
        "//! recorded in `THIRD_PARTY.md`. The counts are pinned by a test, so an upgrade is a",
        "//! visible diff.",
        "",
        "use super::Property;",
        "",
        "use core::cmp::Ordering;",
        "",
        "/// Every code point whose break class is not `Other`, in code point order,",
        "/// as disjoint `(first, last, class)` triples.",
        "pub(super) const BREAK: &[(char, char, Property)] = &[",
    ]
    for low, high, category in breaks:
        lines.append(f"    ('\\u{{{low:x}}}', '\\u{{{high:x}}}', Property::{category}),")
    lines += [
        "];",
        "",
        "/// The break class of `character`, or `None` for the class it is not in (`Other`).",
        "///",
        "/// One binary search over disjoint ranges: the table is sorted by construction, and the",
        "/// test beside it refuses to let a regenerated one be anything else.",
        "pub(super) fn break_class(character: char) -> Option<Property> {",
        "    BREAK",
        "        .binary_search_by(|&(first, last, _)| {",
        "            if last < character {",
        "                Ordering::Less",
        "            } else if first > character {",
        "                Ordering::Greater",
        "            } else {",
        "                Ordering::Equal",
        "            }",
        "        })",
        "        .ok()",
        "        .map(|index| BREAK[index].2)",
        "}",
        "",
        "/// `InCB=Consonant`: the consonants a conjunct run starts from (GB9c).",
        "///",
        "/// These are `Other` in the break table, so a consonant that no rule joins stays a",
        "/// cluster of its own; what makes them interesting is only that a linker between two of",
        "/// them suppresses the break.",
        "pub(super) const INCB_CONSONANT: &[(char, char)] = &[",
    ]
    for low, high in consonants:
        lines.append(f"    ('\\u{{{low:x}}}', '\\u{{{high:x}}}'),")
    lines += [
        "];",
        "",
        "/// Whether `character` is `InCB=Consonant`.",
        "pub(super) fn incb_consonant(character: char) -> bool {",
        "    INCB_CONSONANT",
        "        .binary_search_by(|&(first, last)| {",
        "            if last < character {",
        "                Ordering::Less",
        "            } else if first > character {",
        "                Ordering::Greater",
        "            } else {",
        "                Ordering::Equal",
        "            }",
        "        })",
        "        .is_ok()",
        "}",
        "",
        "/// `InCB=Linker`: the virama-like characters that make a conjunct (GB9c), sorted.",
        "///",
        "/// Since Unicode 18 these are not all `gcb=Extend`, which is why they are data of their",
        "/// own and are checked before a break class says anything about them.",
        "pub(super) const INCB_LINKER: &[char] = &[",
    ]
    for point in linkers:
        lines.append(f"    '\\u{{{point:x}}}',")
    lines += [
        "];",
        "",
        "/// Whether `character` is `InCB=Linker`.",
        "pub(super) fn incb_linker(character: char) -> bool {",
        "    INCB_LINKER.binary_search(&character).is_ok()",
        "}",
        "",
        "/// Two-column display-width code points (UAX #11 `W`/`F` plus default emoji",
        "/// presentation), as sorted disjoint `(first, last)` ranges (AUDIT OQ-25).",
        "pub(super) const WIDE_RANGES: &[(char, char)] = &[",
    ]
    for low, high in wide_ranges:
        lines.append(f"    ('\\u{{{low:x}}}', '\\u{{{high:x}}}'),")
    lines += [
        "];",
        "",
        "/// Whether `character` defaults to two terminal columns.",
        "pub(super) fn wide(character: char) -> bool {",
        "    WIDE_RANGES",
        "        .binary_search_by(|&(first, last)| {",
        "            if last < character {",
        "                Ordering::Less",
        "            } else if first > character {",
        "                Ordering::Greater",
        "            } else {",
        "                Ordering::Equal",
        "            }",
        "        })",
        "        .is_ok()",
        "}",
        "",
    ]
    return "\n".join(lines)


def main(seg_path: str, width_path: str) -> int:
    with open(seg_path, encoding="utf-8") as handle:
        source = handle.read()
    with open(width_path, encoding="utf-8") as handle:
        width_source = handle.read()
    breaks, consonants, linkers, version = parse(source)
    wide_ranges = parse_wide(width_source, version)
    check([(low, high) for low, high, _ in breaks])
    check(consonants)
    check(wide_ranges)
    assert linkers == sorted(set(linkers)), "linkers are not sorted and unique"
    out = "crates/holonomy-eye/src/cluster/tables.rs"
    with open(out, "w", encoding="utf-8") as handle:
        handle.write(render(breaks, consonants, linkers, wide_ranges, version))
    print(f"{out}: {len(breaks)} break ranges, {len(consonants)} consonant ranges,")
    print(f"  {len(linkers)} linkers, {len(wide_ranges)} wide ranges, Unicode {version}")
    counts = {}
    for _, _, category in breaks:
        counts[category] = counts.get(category, 0) + 1
    for category in sorted(counts):
        print(f"  {category}: {counts[category]}")
    return 0


if __name__ == "__main__":
    default_seg = "/tmp/unicode-segmentation-master/src/tables.rs"
    default_width = "/tmp/unicode-width-master/src/gen/tables.rs"
    seg_arg = sys.argv[1] if len(sys.argv) > 1 else default_seg
    width_arg = sys.argv[2] if len(sys.argv) > 2 else default_width
    sys.exit(main(seg_arg, width_arg))
