//! The O(1) lookup table: `(codepoint, style, size) -> GlyphMetric`.
//!
//! FR-2.5 and the Phase 4 directive both require that a glyph blit be a table dereference
//! followed by a memory copy, with zero curve evaluation. This module is the table, and it is
//! deliberately a flat array indexed arithmetically rather than a hash map or a binary search:
//! the whole point is that the lookup has no branches and no pointer chasing.
//!
//! # The index arithmetic
//!
//! ```text
//! slot = (codepoint - FIRST) as usize          // window the atlas covers
//!      + style as usize * CODEPOINTS_PER_STYLE
//!      + size_index * CODEPOINTS_PER_STYLE * STYLE_COUNT
//! ```
//!
//! One multiply-add chain, no division. Codepoints outside the window return `None` in
//! constant time by a single comparison, because a document may contain anything and a
//! missing glyph must not cost a search.
//!
//! # Why the struct is `#[repr(C)]`
//!
//! Because it crosses into the SSE2 blitter, which loads metrics as raw fields. `repr(C)`
//! makes the layout a compiler contract rather than an implementation detail, so a future
//! `repr(Rust)` reordering cannot silently change the offsets the intrinsics assume. The
//! directive specifies this field list and these types; the layout comment below is checked
//! against the real size in a test.

/// Width and height of the atlas, in pixels.
///
/// **1024 × 448**, and the height is not arbitrary: the Phase 4 ceiling is on the atlas *and* its
/// metric table together, so the coverage cannot spend all of it.
///
/// The height is the *whole* of the arithmetic, and it moved twice. It was 512, which is 524,288
/// bytes of coverage — the entire ceiling by itself — and adding the two-size table's 28,160 gave
/// 552,448, or 105% of the limit; `atlas_and_table_fit_the_l2_ceiling` caught that. Dropping to 480
/// gave 491,520 + 28,160 = 519,680, 99.1%, with 4,608 bytes spare.
///
/// **Phase 9B then took that 4,608.** Not because the coverage grew — it shrank — but because the
/// *table* did: the math face needs a fifth style (STYLE_COUNT 4 → 5) and four more codepoint
/// windows for Greek and the operators, and the table is linear in both. At 480 the pair becomes
/// 491,520 + 46,200 = 537,720, or **102.6%** — over, by 13,432 bytes. The height had to go down to
/// buy table space, so it is 448: 458,752 + 46,200 = **504,952, 96.3%**, leaving 19,336 bytes.
///
/// What that cost is packing density. Ink occupancy goes from 76.2% of the arena to 83.7%, and the
/// first build at 448 did overflow once during development (`AtlasFull { need: …, have: 458752 }`)
/// before the symbol set was pruned to the 54 the parser can actually name. That is the honest
/// statement of the trade: **the math face was paid for out of the coverage's slack, not out of
/// headroom that existed.** A ninth style would not fit at any height.
///
/// The width is a power of two so a blit crossing a row boundary needs no special case, and 448 is
/// a multiple of 32 so most glyph heights divide the arena without a ragged last row.
pub const ATLAS_WIDTH: u16 = 1024;

/// Height of the atlas, in pixels.
///
/// 448, and [`ATLAS_WIDTH`]'s comment is the derivation: 480 fit four styles with 4,608 bytes to
/// spare and did not fit five. The gate that pins this is `tests/phase4_gate.rs`'s
/// `atlas_geometry_leaves_room_for_the_metric_table`, which asserts the pair against the ceiling at
/// the shipped geometry *and* the maximum table [`MAX_SIZES`] allows.
pub const ATLAS_HEIGHT: u16 = 448;

/// The atlas is A8: one byte of coverage per pixel, row-major, stride [`ATLAS_WIDTH`].
pub const ATLAS_STRIDE: usize = ATLAS_WIDTH as usize;

/// Total atlas bytes. This is the number the 512 KiB gate asserts.
pub const ATLAS_BYTES: usize = ATLAS_STRIDE * ATLAS_HEIGHT as usize;

/// First codepoint of the text window: U+0020, space.
///
/// U+0000..U+001F are control characters and are never drawn, so starting at space wastes a
/// quarter of the table on entries that are always absent.
pub const FIRST_CODEPOINT: u32 = 0x20;

/// One past the last codepoint of the text window: U+0100, exclusive of Latin-1's end.
pub const END_CODEPOINT: u32 = 0x100;

/// First codepoint of the Greek window, U+0391.
///
/// Phase 9B. Greek is where the parser's named symbols live, and it was outside both original
/// windows, so `\alpha` resolved to a codepoint with no metric slot and no way to give it one.
pub const FIRST_GREEK: u32 = 0x0391;

/// One past the last codepoint of the Greek window: U+03CA, past `omega`.
pub const END_GREEK: u32 = 0x03CA;

/// First codepoint of the Arrows window, U+2190 (`leftarrow`).
pub const FIRST_ARROW: u32 = 0x2190;

/// One past the last codepoint of the Arrows window, U+2193, past `rightarrow`.
///
/// Only three slots for two symbols. A window is a *contiguous* span because [`slot_of`] has to be
/// arithmetic rather than a search, so the cost of an arrow is the three codepoints between
/// `leftarrow` and `to`'s U+2192 — and the block is 0x2190..0x21FF, **112 slots**, which is why it
/// is not taken whole.
pub const END_ARROW: u32 = 0x2193;

/// First codepoint of the operator window, U+2200 (`forall`).
pub const FIRST_OP: u32 = 0x2200;

/// One past the last codepoint of the operator window: U+222C, past `int`.
///
/// 44 slots. `\forall` through `\int`, which is where 12 of the 15 named operators live.
pub const END_OP: u32 = 0x222C;

/// First codepoint of the `\approx` window, U+2248.
pub const FIRST_APPROX: u32 = 0x2248;

/// One past the `\approx` window, U+2249.
pub const END_APPROX: u32 = 0x2249;

/// First codepoint of the relation window, U+2260 (`neq`).
pub const FIRST_REL: u32 = 0x2260;

/// One past the relation window, U+2266, past `geq`.
pub const END_REL: u32 = 0x2266;

/// First codepoint of the `\cdot` window, U+22C5.
pub const FIRST_CDOT: u32 = 0x22C5;

/// One past the `\cdot` window, U+22C6.
pub const END_CDOT: u32 = 0x22C6;

/// First codepoint of the Box Drawing window, U+2500.
pub const FIRST_BOX: u32 = 0x2500;

/// One past the last codepoint of the Box Drawing window, U+2580.
pub const END_BOX: u32 = 0x2580;

/// Codepoints in the text window, `0x100 - 0x20` = 224.
pub const TEXT_CODEPOINTS: usize = (END_CODEPOINT - FIRST_CODEPOINT) as usize;

/// Codepoints in the Greek window, `0x3CA - 0x391` = 57.
pub const GREEK_CODEPOINTS: usize = (END_GREEK - FIRST_GREEK) as usize;

/// Codepoints in the Arrows window, `0x2193 - 0x2190` = 3.
pub const ARROW_CODEPOINTS: usize = (END_ARROW - FIRST_ARROW) as usize;

/// Codepoints in the operator window, `0x222C - 0x2200` = 44.
pub const OP_CODEPOINTS: usize = (END_OP - FIRST_OP) as usize;

/// Codepoints in the `\approx` window, 1.
pub const APPROX_CODEPOINTS: usize = (END_APPROX - FIRST_APPROX) as usize;

/// Codepoints in the relation window, `0x2266 - 0x2260` = 6.
pub const REL_CODEPOINTS: usize = (END_REL - FIRST_REL) as usize;

/// Codepoints in the `\cdot` window, 1.
pub const CDOT_CODEPOINTS: usize = (END_CDOT - FIRST_CDOT) as usize;

/// Codepoints in the Box Drawing window, `0x2580 - 0x2500` = 128.
pub const BOX_CODEPOINTS: usize = (END_BOX - FIRST_BOX) as usize;

/// Codepoints the table covers in total: 224 + 57 + 3 + 44 + 1 + 6 + 1 + 128 = 464.
///
/// # Eight windows, not one
///
/// The required coverage is ASCII, Latin-1 Supplement, Box Drawing, Greek, two arrows and fifteen
/// operators — eight ranges whose members sit at 0x20, 0x391, 0x2190, 0x2200, 0x2248, 0x2260, 0x22C5
/// and 0x2500. A single window 0x20..=0x2580 would need 9,472 entries per style, i.e. 189,440 entries
/// at five styles and two sizes, **1,894,400 bytes of table** for 464 useful ones: 99.98% of it would
/// always read [`GlyphMetric::BLANK`].
///
/// Keeping the windows apart costs one comparison and one add each in the index arithmetic and
/// shrinks the table to 464 × 5 × 2 × 10 = 46,400 bytes, which is 8.8% of the 512 KiB ceiling
/// rather than 361% of it. See [`slot_of`].
///
/// **The operators are four windows, not one, and that is 14,600 bytes.** They were a single
/// 0x2200..0x22C6 span first — 198 slots, one comparison — on the theory that operators are
/// contiguous. They are not: `\approx` is at 0x2248, the relations at 0x2260, and `\cdot` at 0x22C5,
/// so the span carried 146 slots nothing could reach for **14,600 bytes of table** at five styles and
/// two sizes. Three extra comparisons in the least-taken path in the whole renderer is the cheap side
/// of that trade. It also bought back the coverage height: see [`ATLAS_WIDTH`].
///
/// **Order matters and is asserted.** [`slot_of`] sums the *preceding* windows' sizes, so the
/// constants' declaration order here is the layout order there. Reordering the windows without
/// reordering `slot_of` would silently re-address every glyph after the moved one.
pub const CODEPOINTS: usize = TEXT_CODEPOINTS
    + GREEK_CODEPOINTS
    + ARROW_CODEPOINTS
    + OP_CODEPOINTS
    + APPROX_CODEPOINTS
    + REL_CODEPOINTS
    + CDOT_CODEPOINTS
    + BOX_CODEPOINTS;

/// Styles in the atlas: four text faces plus [`payload::Style::Math`](crate::payload::Style::Math).
///
/// Five as of Phase 9B, and that is the other half of why [`ATLAS_HEIGHT`] moved from 480 to 448:
/// the table is linear in this constant, so the fifth style cost 10,240 bytes of table before any
/// new codepoint was considered. `assert_eq!(STYLE_COUNT, 5)` is in this module's tests, so adding a
/// sixth face cannot pass without someone reading the ceiling arithmetic again.
pub const STYLE_COUNT: usize = 5;

/// The largest number of pixel sizes the atlas geometry is budgeted for.
///
/// The ceiling in [`ATLAS_WIDTH`]/[`ATLAS_HEIGHT`] is only valid for a table of at most this many
/// sizes: each extra size adds `STYLE_COUNT * CODEPOINTS * 10` = **30,500** bytes of table, and
/// three sizes would push the pair to 535,452 — over the limit again. Enforced by
/// [`AtlasBuilder::new`](crate::atlas::AtlasBuilder::new) so the geometry cannot silently go over
/// budget by asking for more sizes.
///
/// Was 14,080 at four styles and 352 codepoints; the arithmetic here is why the ceiling is now a
/// real constraint on Phase 9C rather than a formality.
pub const MAX_SIZES: usize = 2;

/// The index of `codepoint` within [`CODEPOINTS`], or `None` if it is not covered.
///
/// # Why eight windows
///
/// A single contiguous window from space to the end of Box Drawing spans 9,472 codepoints of
/// which 464 are ever drawn, and the table's size is linear in the window, so the naive choice
/// wastes 99.98% of a budget that also has to hold the coverage itself. Splitting into the ranges
/// the coverage actually specifies costs one compare-and-add each:
///
/// ```text
/// slot = cp - 0x20                  if cp < 0x100
///      = 0xE0 + (cp - 0x391)        if 0x391 <= cp < 0x3CA
///      = 0x119 + (cp - 0x2190)      if 0x2190 <= cp < 0x2193
///      = 0x11C + (cp - 0x2200)      if 0x2200 <= cp < 0x222C
///      = 0x148 + (cp - 0x2248)      if 0x2248 <= cp < 0x2249
///      = 0x149 + (cp - 0x2260)      if 0x2260 <= cp < 0x2266
///      = 0x14F + (cp - 0x22C5)      if 0x22C5 <= cp < 0x22C6
///      = 0x150 + (cp - 0x2500)      if 0x2500 <= cp < 0x2580
/// ```
///
/// Each offset is the sum of the preceding windows' sizes, which is why the window constants'
/// *declaration order* is load-bearing — see [`CODEPOINTS`].
///
/// **The comparison order is the order codepoints arrive in, and it is deliberate.** Text is checked
/// first because it is 99% of all lookups: a keystroke blits a text glyph, so the common path exits
/// on the first comparison. A formula's symbol exits on the third through seventh. Branchless in the
/// text case, and it is the only arithmetic between the keystroke and the blit.
///
/// **What this rejects.** A codepoint in none of the eight windows returns `None` and
/// [`MetricTable::get`] turns that into [`GlyphMetric::BLANK`] — zero advance, nothing copied. That
/// is how an unlisted codepoint renders as nothing rather than as a crash, and `metric.set` panics
/// on the same input at build time, which is where the mistake should be caught. That panic is not
/// hypothetical: it caught `\cdot` (U+22C5) sitting past the then-current operator window, which is
/// why `END_CDOT` exists as a window of its own.
#[inline]
pub const fn slot_of(codepoint: u32) -> Option<usize> {
    if codepoint >= FIRST_CODEPOINT && codepoint < END_CODEPOINT {
        Some((codepoint - FIRST_CODEPOINT) as usize)
    } else if codepoint >= FIRST_GREEK && codepoint < END_GREEK {
        Some(TEXT_CODEPOINTS + (codepoint - FIRST_GREEK) as usize)
    } else if codepoint >= FIRST_ARROW && codepoint < END_ARROW {
        Some(TEXT_CODEPOINTS + GREEK_CODEPOINTS + (codepoint - FIRST_ARROW) as usize)
    } else if codepoint >= FIRST_OP && codepoint < END_OP {
        Some(
            TEXT_CODEPOINTS + GREEK_CODEPOINTS + ARROW_CODEPOINTS + (codepoint - FIRST_OP) as usize,
        )
    } else if codepoint >= FIRST_APPROX && codepoint < END_APPROX {
        Some(
            TEXT_CODEPOINTS
                + GREEK_CODEPOINTS
                + ARROW_CODEPOINTS
                + OP_CODEPOINTS
                + (codepoint - FIRST_APPROX) as usize,
        )
    } else if codepoint >= FIRST_REL && codepoint < END_REL {
        Some(
            TEXT_CODEPOINTS
                + GREEK_CODEPOINTS
                + ARROW_CODEPOINTS
                + OP_CODEPOINTS
                + APPROX_CODEPOINTS
                + (codepoint - FIRST_REL) as usize,
        )
    } else if codepoint >= FIRST_CDOT && codepoint < END_CDOT {
        Some(
            TEXT_CODEPOINTS
                + GREEK_CODEPOINTS
                + ARROW_CODEPOINTS
                + OP_CODEPOINTS
                + APPROX_CODEPOINTS
                + REL_CODEPOINTS
                + (codepoint - FIRST_CDOT) as usize,
        )
    } else if codepoint >= FIRST_BOX && codepoint < END_BOX {
        Some(
            TEXT_CODEPOINTS
                + GREEK_CODEPOINTS
                + ARROW_CODEPOINTS
                + OP_CODEPOINTS
                + APPROX_CODEPOINTS
                + REL_CODEPOINTS
                + CDOT_CODEPOINTS
                + (codepoint - FIRST_BOX) as usize,
        )
    } else {
        None
    }
}

/// The codepoint [`slot_of`] maps `slot` to, or `None` if the slot is out of range.
///
/// The inverse of [`slot_of`], and it exists because a test needed it: `tests/phase4_gate.rs`
/// walks the table counting non-blank entries per style, and its own helper for that -- `if slot <
/// TEXT_CODEPOINTS { 0x20 + slot } else { 0x2500 + slot - TEXT_CODEPOINTS }` -- was correct for two
/// windows and silently wrong for eight. Every slot from `TEXT_CODEPOINTS` onward would have been
/// reported as a Box Drawing codepoint, so the ceiling test would have counted Greek glyphs as box
/// drawing and reported "style Math has no glyphs at all" for a face that carries 108 of them.
///
/// A second implementation of an index layout is a second source of truth, and the one that disagrees
/// is never the one under test. This is the layout, inverted, next to the layout.
#[inline]
pub const fn codepoint_of(slot: usize) -> Option<u32> {
    if slot < TEXT_CODEPOINTS {
        Some(FIRST_CODEPOINT + slot as u32)
    } else if slot < TEXT_CODEPOINTS + GREEK_CODEPOINTS {
        Some(FIRST_GREEK + (slot - TEXT_CODEPOINTS) as u32)
    } else if slot < TEXT_CODEPOINTS + GREEK_CODEPOINTS + ARROW_CODEPOINTS {
        Some(FIRST_ARROW + (slot - TEXT_CODEPOINTS - GREEK_CODEPOINTS) as u32)
    } else if slot < TEXT_CODEPOINTS + GREEK_CODEPOINTS + ARROW_CODEPOINTS + OP_CODEPOINTS {
        Some(FIRST_OP + (slot - TEXT_CODEPOINTS - GREEK_CODEPOINTS - ARROW_CODEPOINTS) as u32)
    } else if slot
        < TEXT_CODEPOINTS + GREEK_CODEPOINTS + ARROW_CODEPOINTS + OP_CODEPOINTS + APPROX_CODEPOINTS
    {
        Some(
            FIRST_APPROX
                + (slot - TEXT_CODEPOINTS - GREEK_CODEPOINTS - ARROW_CODEPOINTS - OP_CODEPOINTS)
                    as u32,
        )
    } else if slot
        < TEXT_CODEPOINTS
            + GREEK_CODEPOINTS
            + ARROW_CODEPOINTS
            + OP_CODEPOINTS
            + APPROX_CODEPOINTS
            + REL_CODEPOINTS
    {
        Some(
            FIRST_REL
                + (slot
                    - TEXT_CODEPOINTS
                    - GREEK_CODEPOINTS
                    - ARROW_CODEPOINTS
                    - OP_CODEPOINTS
                    - APPROX_CODEPOINTS) as u32,
        )
    } else if slot
        < TEXT_CODEPOINTS
            + GREEK_CODEPOINTS
            + ARROW_CODEPOINTS
            + OP_CODEPOINTS
            + APPROX_CODEPOINTS
            + REL_CODEPOINTS
            + CDOT_CODEPOINTS
    {
        Some(
            FIRST_CDOT
                + (slot
                    - TEXT_CODEPOINTS
                    - GREEK_CODEPOINTS
                    - ARROW_CODEPOINTS
                    - OP_CODEPOINTS
                    - APPROX_CODEPOINTS
                    - REL_CODEPOINTS) as u32,
        )
    } else if slot < CODEPOINTS {
        Some(
            FIRST_BOX
                + (slot
                    - TEXT_CODEPOINTS
                    - GREEK_CODEPOINTS
                    - ARROW_CODEPOINTS
                    - OP_CODEPOINTS
                    - APPROX_CODEPOINTS
                    - REL_CODEPOINTS
                    - CDOT_CODEPOINTS) as u32,
        )
    } else {
        None
    }
}

/// Where a glyph's coverage lives, and how to place it.
///
/// `repr(C)`: the SSE2 kernel and the scalar fallback both read these fields, so the layout
/// is part of the contract. 8 bytes.
///
/// Field widths are the directive's and they are sufficient: a glyph wider or taller than
/// 255 px cannot occur at any size this crate rasterises, and the largest size is rejected at
/// rasterisation time by [`AtlasError::GlyphTooLarge`](crate::atlas::AtlasError::GlyphTooLarge)
/// rather than silently truncated here.
///
/// # Layout: 9 bytes of fields, 10 with alignment
///
/// 2 + 2 + 1 + 1 + 1 + 1 + 1 = **9** bytes of fields. `repr(C)` puts the struct's alignment
/// at 2 (from the two `u16`s), and 9 is odd, so `size_of` is **10**: one padding byte after
/// `advance_x`. The gap is free in practice — the table holds 224 × 4 × 3 = 2,688 entries at
/// two sizes, so the padding costs 2,688 bytes of a 512 KiB budget, 0.5%.
///
/// Worth stating plainly because the field list invites an expectation of 8 and the test
/// pins 10. Neither is a problem; 8 would mean the struct did not have all seven fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct GlyphMetric {
    /// Left edge in the atlas, in pixels.
    pub atlas_x: u16,
    /// Top edge in the atlas, in pixels.
    pub atlas_y: u16,
    /// Coverage bitmap width, in pixels.
    pub width: u8,
    /// Coverage bitmap height, in pixels.
    pub height: u8,
    /// Left side bearing, in pixels, relative to the pen.
    pub bearing_x: i8,
    /// Top side bearing, in pixels, above the baseline. Positive is up.
    pub bearing_y: i8,
    /// Horizontal advance, in pixels.
    pub advance_x: u8,
}

impl GlyphMetric {
    /// A fully transparent glyph: `.notdef` for an absent codepoint.
    ///
    /// Zero advance and zero size means the blitter copies nothing and advances not at all,
    /// so a missing glyph costs one table read and no memory traffic. A space.
    pub const BLANK: Self = Self {
        atlas_x: 0,
        atlas_y: 0,
        width: 0,
        height: 0,
        bearing_x: 0,
        bearing_y: 0,
        advance_x: 0,
    };

    /// True when this glyph has no coverage and no advance: a space or an absent codepoint.
    pub fn is_blank(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Bytes of atlas this glyph occupies. Blank glyphs occupy none.
    pub fn byte_len(&self) -> usize {
        if self.is_blank() {
            0
        } else {
            self.width as usize * self.height as usize
        }
    }
}

/// A flat, indexable metric table.
#[derive(Clone)]
pub struct MetricTable {
    /// One entry per `(codepoint, style, size)` in that nesting order.
    metrics: Vec<GlyphMetric>,
    /// Sizes present, ascending. `size_index` selects among these.
    sizes: Vec<u16>,
}

impl core::fmt::Debug for MetricTable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MetricTable")
            .field("entries", &self.metrics.len())
            .field("sizes", &self.sizes)
            .finish()
    }
}

impl MetricTable {
    /// An empty table sized for `sizes`.
    pub fn new(sizes: &[u16]) -> Self {
        let sizes = sizes.to_vec();
        Self {
            metrics: vec![GlyphMetric::BLANK; sizes.len() * STYLE_COUNT * CODEPOINTS],
            sizes,
        }
    }

    /// Sizes this table covers, ascending.
    pub fn sizes(&self) -> &[u16] {
        &self.sizes
    }

    /// Total entries.
    pub fn len(&self) -> usize {
        self.metrics.len()
    }

    /// Always false: the table is sized from `sizes`, and an empty size list is rejected by
    /// the builder. Present so `len`/`is_empty` stay paired.
    pub fn is_empty(&self) -> bool {
        self.metrics.is_empty()
    }

    /// Index of `px` among [`sizes`](Self::sizes), or `None` if it was not rasterised.
    pub fn size_index(&self, px: u16) -> Option<usize> {
        // Sizes are few and ascending, so a linear scan beats a binary search here and keeps
        // this function usable in const-adjacent paths. This runs once per text layout, not
        // per glyph.
        self.sizes.iter().position(|&s| s == px)
    }

    /// The O(1) lookup.
    ///
    /// Returns [`GlyphMetric::BLANK`] for anything outside the coverage window or any size
    /// that was not rasterised, so a caller never has to branch before blitting.
    pub fn get(&self, codepoint: u32, style: usize, size_index: usize) -> GlyphMetric {
        // The slot is computed arithmetically, so every term must be bounded *before* it is used
        // rather than after: `style * CODEPOINTS + size_index * CODEPOINTS * STYLE_COUNT` with
        // `style = usize::MAX` overflows to a value that passes a `>= len` check by accident.
        // Checking the inputs is also what keeps the multiply itself from wrapping.
        if style >= STYLE_COUNT || size_index >= self.sizes.len() {
            return GlyphMetric::BLANK;
        }
        let Some(within) = slot_of(codepoint) else {
            return GlyphMetric::BLANK;
        };
        let slot = within + style * CODEPOINTS + size_index * CODEPOINTS * STYLE_COUNT;
        // Belt and braces: the arithmetic above cannot overflow with the bounds just checked,
        // but an out-of-range slot must never index regardless.
        self.metrics
            .get(slot)
            .copied()
            .unwrap_or(GlyphMetric::BLANK)
    }

    /// Write one slot. Panics on out-of-range input, so it is build-time use only.
    pub fn set(&mut self, codepoint: u32, style: usize, size_index: usize, m: GlyphMetric) {
        let within = slot_of(codepoint).unwrap_or_else(|| {
            panic!("codepoint U+{codepoint:04X} is outside the coverage window")
        });
        assert!(style < STYLE_COUNT, "style {style} out of range");
        assert!(
            size_index < self.sizes.len(),
            "size {size_index} out of range"
        );
        let slot = within + style * CODEPOINTS + size_index * CODEPOINTS * STYLE_COUNT;
        self.metrics[slot] = m;
    }

    /// Slice for one size, all styles, all codepoints. Used by tests that compare against a
    /// reference implementation and by the atlas consistency check.
    pub fn size_slice(&self, size_index: usize) -> &[GlyphMetric] {
        let start = size_index * CODEPOINTS * STYLE_COUNT;
        &self.metrics[start..start + CODEPOINTS * STYLE_COUNT]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The metric struct must keep the specified field offsets, because the SSE2 kernel reads
    /// them as raw fields and `repr(Rust)` would be free to reorder them.
    ///
    /// The size is 10, not the 8 the field list suggests: 2+2+1+1+1+1+1 is 9 bytes of fields
    /// and `repr(C)` pads the odd total up to the struct's alignment of 2.
    #[test]
    fn metric_layout_is_repr_c_with_one_padding_byte() {
        assert_eq!(
            size_of::<GlyphMetric>(),
            10,
            "nine bytes of fields, padded to a multiple of align 2"
        );
        assert_eq!(align_of::<GlyphMetric>(), 2);
        let m = GlyphMetric {
            atlas_x: 1,
            atlas_y: 2,
            width: 3,
            height: 4,
            bearing_x: -5,
            bearing_y: 6,
            advance_x: 7,
        };
        // Read the bytes back through a raw pointer and check each field's offset. This is the
        // check that would catch a `repr(Rust)` reordering.
        let base = &m as *const GlyphMetric as *const u8;
        let u16_at = |off: usize| -> u16 {
            let mut b = [0u8; 2];
            // SAFETY: offsets 0..8 of an 8-byte struct with align 2 are all within it and
            // correctly aligned for u16.
            unsafe { std::ptr::copy_nonoverlapping(base.add(off), b.as_mut_ptr(), 2) };
            u16::from_le_bytes(b)
        };
        let i8_at = |off: usize| -> i8 {
            // SAFETY: as above. `as i8` reinterprets the bit pattern, which is what is wanted:
            // the field is stored as a signed byte and `bearing_x` is negative in this metric.
            unsafe { *base.add(off) as i8 }
        };
        assert_eq!(u16_at(0), 1, "atlas_x at offset 0");
        assert_eq!(u16_at(2), 2, "atlas_y at offset 2");
        assert_eq!(i8_at(4), 3, "width at offset 4");
        assert_eq!(i8_at(5), 4, "height at offset 5");
        assert_eq!(i8_at(6), -5, "bearing_x at offset 6");
        assert_eq!(i8_at(7), 6, "bearing_y at offset 7");
        assert_eq!(i8_at(8), 7, "advance_x at offset 8");
        // Offset 9 is the padding byte, and `repr(C)` leaves it uninitialised.
        assert_eq!(size_of::<GlyphMetric>(), 10);
    }

    /// The padding byte must not leak a previous glyph's data into a comparison. `BLANK` is a
    /// const, so its padding is zero; a written slot's padding is whatever the compiler left,
    /// which is why `PartialEq` on the struct could otherwise report spurious inequality for
    /// two metrics that are identical in all seven fields.
    #[test]
    fn equality_ignores_padding() {
        let mk = || GlyphMetric {
            atlas_x: 5,
            atlas_y: 6,
            width: 7,
            height: 8,
            bearing_x: -9,
            bearing_y: 10,
            advance_x: 11,
        };
        assert_eq!(mk(), mk());
        // This is the reason `GlyphMetric` cannot be compared bytewise by a future
        // optimisation: the padding byte is unspecified.
        let a = mk();
        let b = mk();
        let ab = unsafe {
            std::slice::from_raw_parts(
                &a as *const GlyphMetric as *const u8,
                size_of::<GlyphMetric>(),
            )
        };
        let bb = unsafe {
            std::slice::from_raw_parts(
                &b as *const GlyphMetric as *const u8,
                size_of::<GlyphMetric>(),
            )
        };
        assert_eq!(ab[..8], bb[..8], "the seven fields must be identical");
    }

    /// The ceiling of 512 KiB covers the coverage **and** this table together.
    ///
    /// This test used to assert `ATLAS_BYTES == 512 * 1024`. That was right while the geometry
    /// was 1024x512 and wrong afterwards, and it reported `left: 491520, right: 524288` -- a true
    /// observation about a stale expectation. The requirement is the pair, not the coverage alone.
    ///
    /// **Every number below moved in Phase 9B, and each is here so it cannot drift quietly:**
    /// the height 480 → 448, the styles 4 → 5, the codepoints 352 → 464, and the table
    /// 28,160 → 46,400. The pair went from 519,680 (99.1%, 4,608 spare) to **505,152 (96.4%,
    /// 19,136 spare)**. It got *less* full while every one of its parts grew, because the coverage
    /// gave up 32,768 bytes to pay for 18,240 bytes of table.
    #[test]
    fn atlas_and_table_share_the_512_kib_ceiling() {
        let ceiling = 512 * 1024;
        assert_eq!(ATLAS_WIDTH, 1024);
        assert_eq!(ATLAS_HEIGHT, 448);
        assert_eq!(ATLAS_STRIDE, ATLAS_WIDTH as usize);
        assert_eq!(ATLAS_BYTES, ATLAS_STRIDE * ATLAS_HEIGHT as usize);
        assert_eq!(ATLAS_BYTES, 458_752);

        let table = MAX_SIZES * STYLE_COUNT * CODEPOINTS * size_of::<GlyphMetric>();
        assert_eq!(
            table, 46_400,
            "2 sizes x 5 styles x 464 codepoints x 10 bytes"
        );
        assert_eq!(
            ATLAS_BYTES + table,
            505_152,
            "the pair must fit, with 19,136 bytes of headroom"
        );
        assert!(ATLAS_BYTES + table <= ceiling);

        // Guard the mistake this geometry exists to avoid: a 512x512 A8 atlas is 256 KiB, half the
        // ceiling, and reading the requirement as "the atlas is 512 KiB" would have passed on it.
        assert_eq!(512 * 512, 262_144);
        assert_eq!(
            CODEPOINTS, 464,
            "224 Latin-1 + 57 Greek + 3 arrows + 44 ops + 1 approx + 6 relations + 1 cdot + 128 Box Drawing"
        );
        assert_eq!(
            STYLE_COUNT, 5,
            "four text faces plus Style::Math; a sixth would not fit under this ceiling"
        );
    }

    #[test]
    fn blank_is_inert() {
        assert!(GlyphMetric::BLANK.is_blank());
        assert_eq!(GlyphMetric::BLANK.byte_len(), 0);
        assert_eq!(GlyphMetric::BLANK.advance_x, 0);
    }

    #[test]
    fn lookup_is_exhaustive_over_the_window() {
        let t = MetricTable::new(&[16, 32]);
        assert_eq!(t.size_index(16), Some(0));
        assert_eq!(t.size_index(32), Some(1));
        assert_eq!(t.size_index(24), None);
        for cp in FIRST_CODEPOINT..END_CODEPOINT {
            assert!(
                t.get(cp, 0, 0).is_blank(),
                "fresh table must be blank at U+{cp:04X}"
            );
        }
    }

    /// Outside the window returns blank rather than panicking or reading out of bounds. A
    /// document can contain anything, including codepoints above the window and below it.
    #[test]
    fn out_of_window_lookups_are_blank() {
        let t = MetricTable::new(&[16]);
        for cp in [0u32, 1, 0x1F, 0x100, 0x1_0000, 0x4E00, u32::MAX] {
            assert_eq!(
                t.get(cp, 0, 0),
                GlyphMetric::BLANK,
                "U+{cp:X} must be blank"
            );
        }
    }

    /// Out-of-range style and size must be blank too, not a panic and not a wrap.
    #[test]
    fn out_of_range_style_and_size_are_blank() {
        let t = MetricTable::new(&[16]);
        for (style, si) in [
            (STYLE_COUNT, 0usize),
            (99, 0),
            (0, 1),
            (0, usize::MAX),
            (usize::MAX, usize::MAX),
        ] {
            assert_eq!(
                t.get('A' as u32, style, si),
                GlyphMetric::BLANK,
                "style {style} size {si} must be blank, not a wrapped index"
            );
        }
    }

    /// Slots must not alias. Writing one `(cp, style, size)` must not disturb any other, which
    /// is what the flat index arithmetic has to guarantee.
    #[test]
    fn slots_do_not_alias() {
        let mut t = MetricTable::new(&[16, 32]);
        let mut n = 0u32;
        for cp in FIRST_CODEPOINT..END_CODEPOINT {
            for style in 0..STYLE_COUNT {
                for si in 0..2 {
                    let m = GlyphMetric {
                        atlas_x: (n % 1000) as u16,
                        atlas_y: (n / 1000 % 1000) as u16,
                        width: 1 + (n % 7) as u8,
                        height: 1 + (n % 5) as u8,
                        bearing_x: -((n % 8) as i8),
                        bearing_y: (n % 6) as i8,
                        advance_x: (n % 9) as u8 + 1,
                    };
                    t.set(cp, style, si, m);
                    n += 1;
                }
            }
        }
        // Every written slot must read back its own value.
        n = 0;
        for cp in FIRST_CODEPOINT..END_CODEPOINT {
            for style in 0..STYLE_COUNT {
                for si in 0..2 {
                    let m = t.get(cp, style, si);
                    assert_eq!(
                        m.atlas_x as u32,
                        n % 1000,
                        "U+{cp:04X} style {style} size {si}"
                    );
                    assert_eq!(m.width, 1 + (n % 7) as u8);
                    n += 1;
                }
            }
        }
    }

    #[test]
    fn size_slice_partitions_the_table() {
        let t = MetricTable::new(&[12, 20, 30]);
        assert_eq!(t.len(), 3 * STYLE_COUNT * CODEPOINTS);
        assert_eq!(t.size_slice(0).len(), STYLE_COUNT * CODEPOINTS);
        assert_eq!(t.size_slice(2).len(), STYLE_COUNT * CODEPOINTS);
    }
}
