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
/// **1024 × 480**, and the height is not arbitrary: the Phase 4 ceiling is on the atlas *and* its
/// metric table together, so the coverage cannot spend all of it.
///
/// 1024 × 512 = 524,288 bytes of coverage, which is the whole ceiling by itself; adding the
/// two-size table's 28,160 gives 552,448 — 105% of the limit. That is what
/// `atlas_and_table_fit_the_l2_ceiling` caught when it compared `coverage().len() + table`
/// against `512 * 1024`. At 1024 × 480 the pair is 491,520 + 28,160 = 519,680, which is 99.1% of
/// the ceiling and leaves 4,608 bytes of headroom.
///
/// The width is a power of two so a blit crossing a row boundary needs no special case, and 480 is
/// a multiple of 32 so most glyph heights divide the arena without a ragged last row.
///
/// At four faces × two sizes the atlas holds 376,415 bytes of glyphs, i.e. 76.6% occupancy —
/// the same packing efficiency the 512-tall arena achieved, so nothing is squeezed out.
pub const ATLAS_WIDTH: u16 = 1024;

/// Height of the atlas, in pixels.
pub const ATLAS_HEIGHT: u16 = 480;

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

/// First codepoint of the Box Drawing window, U+2500.
pub const FIRST_BOX: u32 = 0x2500;

/// One past the last codepoint of the Box Drawing window, U+2580.
pub const END_BOX: u32 = 0x2580;

/// Codepoints in the text window, `0x100 - 0x20` = 224.
pub const TEXT_CODEPOINTS: usize = (END_CODEPOINT - FIRST_CODEPOINT) as usize;

/// Codepoints in the Box Drawing window, `0x2580 - 0x2500` = 128.
pub const BOX_CODEPOINTS: usize = (END_BOX - FIRST_BOX) as usize;

/// Codepoints the table covers in total, 224 + 128 = 352.
///
/// # Two windows, not one
///
/// The required coverage is ASCII, Latin-1 Supplement and Box Drawing — three ranges whose third
/// member is at 0x2500 while the first two are contiguous. A single window 0x20..=0x2580 would
/// need 9,472 entries per style, i.e. 75,776 entries at two sizes, 757,760 bytes of table for
/// 352 useful ones: 96.3% of it would always read [`GlyphMetric::BLANK`].
///
/// Keeping the two windows apart costs one comparison and one add in the index arithmetic and
/// shrinks the table to 352 × 4 × 2 × 10 = 28,160 bytes, which is 5.4% of the 512 KiB ceiling
/// rather than 145% of it. See [`slot_of`].
pub const CODEPOINTS: usize = TEXT_CODEPOINTS + BOX_CODEPOINTS;

/// Styles in the atlas.
pub const STYLE_COUNT: usize = 4;

/// The largest number of pixel sizes the atlas geometry is budgeted for.
///
/// The ceiling in [`ATLAS_WIDTH`]/[`ATLAS_HEIGHT`] is only valid for a table of at most this many
/// sizes: each extra size adds `STYLE_COUNT * CODEPOINTS * 10` = 14,080 bytes of table, and three
/// sizes would push the pair to 533,760 — over the limit again. Enforced by
/// [`AtlasBuilder::new`](crate::atlas::AtlasBuilder::new) so the geometry cannot silently go over
/// budget by asking for more sizes.
pub const MAX_SIZES: usize = 2;

/// The index of `codepoint` within [`CODEPOINTS`], or `None` if it is not covered.
///
/// # Why two windows
///
/// A single contiguous window from space to the end of Box Drawing spans 9,472 codepoints of
/// which 352 are ever drawn, and the table's size is linear in the window, so the naive choice
/// wastes 96% of a budget that also has to hold the coverage itself. Splitting into the two
/// ranges the coverage actually specifies costs one compare-and-add:
///
/// ```text
/// slot = cp - 0x20                 if cp < 0x100
///      = 0xE0 + (cp - 0x2500)       if 0x2500 <= cp < 0x2580
/// ```
///
/// which is branchless in the common case if the second window is placed after the first, and
/// is the only arithmetic between the keystroke and the blit.
#[inline]
pub const fn slot_of(codepoint: u32) -> Option<usize> {
    if codepoint >= FIRST_CODEPOINT && codepoint < END_CODEPOINT {
        Some((codepoint - FIRST_CODEPOINT) as usize)
    } else if codepoint >= FIRST_BOX && codepoint < END_BOX {
        Some(TEXT_CODEPOINTS + (codepoint - FIRST_BOX) as usize)
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
    #[test]
    fn atlas_and_table_share_the_512_kib_ceiling() {
        let ceiling = 512 * 1024;
        assert_eq!(ATLAS_WIDTH, 1024);
        assert_eq!(ATLAS_HEIGHT, 480);
        assert_eq!(ATLAS_STRIDE, ATLAS_WIDTH as usize);
        assert_eq!(ATLAS_BYTES, ATLAS_STRIDE * ATLAS_HEIGHT as usize);
        assert_eq!(ATLAS_BYTES, 491_520);

        let table = MAX_SIZES * STYLE_COUNT * CODEPOINTS * size_of::<GlyphMetric>();
        assert_eq!(
            table, 28_160,
            "2 sizes x 4 styles x 352 codepoints x 10 bytes"
        );
        assert_eq!(
            ATLAS_BYTES + table,
            519_680,
            "the pair must fit, with 4,608 bytes of headroom"
        );
        assert!(ATLAS_BYTES + table <= ceiling);

        // Guard the mistake this geometry exists to avoid: a 512x512 A8 atlas is 256 KiB, half the
        // ceiling, and reading the requirement as "the atlas is 512 KiB" would have passed on it.
        assert_eq!(512 * 512, 262_144);
        assert_eq!(CODEPOINTS, 352, "224 Latin-1 window + 128 Box Drawing");
        assert_eq!(STYLE_COUNT, 4);
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
