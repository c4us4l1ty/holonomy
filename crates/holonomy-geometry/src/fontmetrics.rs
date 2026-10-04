//! Deterministic line heights from font metrics. No measurement, ever.
//!
//! FR-1.3 and PROJECT.md §5 Phase 6: "Line height comes from font ascender/descender, not from
//! measurement."
//!
//! # The formula
//!
//! ```text
//! line_height = ceil((ascender - descender + line_gap) * ppem / units_per_em)
//! ```
//!
//! with **`descender` negative**, which is the sign convention every font's `hhea` table uses.
//! Inter's `hhea.descender` is `-494`, so `ascender - descender` is `1984 + 494 = 2478` font units:
//! the distance from the ascent's top to the descent's bottom. Writing it as a subtraction rather
//! than an addition is not a stylistic choice -- it is what lets the field hold the value the font
//! file actually stores, so [`FontMetrics::from_hhea`] is a straight copy of four `i16`s with no
//! sign fixup to get wrong.
//!
//! # Why integer arithmetic and not `f32`
//!
//! `f32` would give 19.359375 for a 16 px Inter line and the same value back for every call, so the
//! non-determinism argument does not apply here. The reason is subtler: `(f32).ceil()` on a value
//! that is exactly an integer can go either way across a FPU mode change, and more importantly a
//! `f32` line height makes the Fenwick tree's weights inexact, which breaks the one property this
//! crate depends on -- that `lower_bound` and `prefix` compute the *same integer*. See
//! [`Fenwick::lower_bound`](crate::Fenwick::lower_bound).
//!
//! So the ceiling is done with [`u64::div_ceil`] on a `u64` product, which is exact for every input,
//! and the test `ceil_matches_the_rational_form_exactly` checks it against `f64::ceil` for 1,024 ppem
//! and font-metric combinations.
//!
//! # The values are real, and where they came from
//!
//! [`FontMetrics::INTER`] and [`FontMetrics::JETBRAINS_MONO`] are read from the `hhea` and `head`
//! tables of the two TTFs in `assets/fonts/`, by `tools/`'s font tooling. They are compile-time
//! constants because PROJECT.md §1 forbids generating parameters at build or test time, and a
//! reader who wants to verify them can:
//!
//! ```text
//! $ python3 -c "from fontTools.ttLib import TTFont; t=TTFont('assets/fonts/Inter-Regular.ttf');
//!             print(t['head'].unitsPerEm, t['hhea'].ascent, t['hhea'].descent, t['hhea'].lineGap)"
//! 2048 1984 -494 0
//! ```

/// A font's vertical metrics, in font design units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct FontMetrics {
    /// Distance from the baseline to the top of the ascent. **Positive.**
    ///
    /// OpenType's `hhea.ascent`, which is a signed 16-bit value and is positive in every shipped
    /// font. A font with a *negative* ascent would make the line box shorter than the descent, which
    /// [`line_height_px`] handles by treating the whole expression as unsigned after the sum.
    pub ascender: i32,
    /// Distance from the baseline to the bottom of the descent. **Negative.**
    ///
    /// OpenType's `hhea.descent`. Inter's is `-494`, so this is *not* an error to be corrected --
    /// storing it as a positive number would make [`FontMetrics::from_hhea`] a lossy copy.
    pub descender: i32,
    /// Extra leading the font asks for between lines. May be negative.
    ///
    /// OpenType's `hhea.lineGap`. Zero for both faces H1 ships.
    pub line_gap: i32,
    /// Font design units per em, `head.unitsPerEm`. A power of two in practice: 2048 for Inter,
    /// 1000 for JetBrains Mono.
    pub units_per_em: u32,
}

impl FontMetrics {
    /// Inter Regular, the proportional body face. `hhea` of `assets/fonts/Inter-Regular.ttf`.
    pub const INTER: FontMetrics = FontMetrics {
        ascender: 1984,
        descender: -494,
        line_gap: 0,
        units_per_em: 2048,
    };

    /// JetBrains Mono Regular, the code face. `hhea` of `assets/fonts/JetBrainsMono-Regular.ttf`.
    pub const JETBRAINS_MONO: FontMetrics = FontMetrics {
        ascender: 1020,
        descender: -300,
        line_gap: 0,
        units_per_em: 1000,
    };

    /// Build from the four `hhea` values, in OpenType's signed convention.
    ///
    /// A straight copy, with no sign fixup: [`descender`](Self::descender) stays negative because
    /// that is what the font file says and what [`line_height_px`] expects.
    ///
    /// # Why `units_per_em` is validated here
    ///
    /// A zero `units_per_em` makes the scale division a division by zero, and in a bare-metal
    /// integer build that is a hardware trap -- not a panic, not an `Err`, a `SIGFPE` with no
    /// handler. Font metrics come from a file, so a malformed font is a real input, and the
    /// requirement here (FR-5.5's spirit: no unreachable error paths) is that it is rejected at the
    /// boundary where it can still be reported.
    pub const fn from_hhea(
        ascender: i16,
        descender: i16,
        line_gap: i16,
        units_per_em: u16,
    ) -> FontMetrics {
        assert!(
            units_per_em != 0,
            "unitsPerEm is zero; the ppem-to-font-unit scale would divide by zero"
        );
        FontMetrics {
            ascender: ascender as i32,
            descender: descender as i32,
            line_gap: line_gap as i32,
            units_per_em: units_per_em as u32,
        }
    }

    /// The total em box height in font units: `ascender - descender + line_gap`.
    ///
    /// Saturating, and `0` if the font's own metrics are non-positive in total -- a font whose
    /// ascent and descent cancel would produce a zero-height line box and an infinite document.
    #[inline]
    pub const fn em_height(&self) -> u32 {
        let total = self
            .ascender
            .saturating_sub(self.descender)
            .saturating_add(self.line_gap);
        if total <= 0 {
            0
        } else {
            total as u32
        }
    }

    /// The line height in pixels at `ppem`, rounded up.
    ///
    /// `line_height = ceil(em_height * ppem / units_per_em)`, computed exactly in `u64` with
    /// [`u64::div_ceil`].
    ///
    /// Rounding **up** rather than to nearest is deliberate and it is the choice that keeps glyphs
    /// from colliding: two lines of a 19.36 px metric rounded to 19 are 38 px apart while the
    /// glyphs want 38.72, so ascenders overlap by 0.72 px on every line boundary. Ceil gives 20 and
    /// 40, which is 1.28 px of slack rather than a deficit.
    ///
    /// Returns 0 for a zero-height metric rather than a wrapped `u32`; see [`FontMetrics::em_height`].
    #[inline]
    pub const fn line_height_px(&self, ppem: u16) -> u32 {
        let em = self.em_height();
        if em == 0 || ppem == 0 {
            return 0;
        }
        // u64 so the multiply cannot overflow: em can be up to 65,535 and ppem up to 65,535.
        let n = em as u64 * ppem as u64;
        let d = self.units_per_em as u64;
        // Ceil division, exact, via `u64::div_ceil` rather than a hand-written `(n + d - 1) / d`. The
        // hand-written form is what the first version used and it is correct; `div_ceil` is the same
        // operation under a name that says so, and clippy's `manual_div_ceil` is right that the spelling
        // is noise. `u64` because `em` can be 65,535 and `ppem` can be 65,535, and the product overflows
        // `u32` for a large face.
        n.div_ceil(d) as u32
    }

    /// The baseline's distance from the top of the line box, in pixels, at `ppem`.
    ///
    /// `ceil(ascender * ppem / units_per_em)`.
    ///
    /// This is *not* `line_height_px` minus a descender: the two ceilings are computed
    /// independently and their sum can exceed the line height by 1 px, which is correct -- the
    /// line box is the em box and the glyph origin sits `ascender` below its top, with any
    /// rounding remainder as slack at the bottom.
    #[inline]
    pub const fn baseline_px(&self, ppem: u16) -> u32 {
        if self.ascender <= 0 || ppem == 0 {
            return 0;
        }
        let n = self.ascender as u64 * ppem as u64;
        let d = self.units_per_em as u64;
        n.div_ceil(d) as u32
    }

    /// The ascent in pixels at `ppem`, rounded up. `ceil(ascender * ppem / upm)`.
    #[inline]
    pub const fn ascender_px(&self, ppem: u16) -> u32 {
        if self.ascender <= 0 || ppem == 0 {
            return 0;
        }
        let n = self.ascender as u64 * ppem as u64;
        let d = self.units_per_em as u64;
        n.div_ceil(d) as u32
    }

    /// The descent in pixels at `ppem`, rounded up, as a **positive** distance below the baseline.
    ///
    /// `ceil(|descender| * ppem / upm)`.
    #[inline]
    pub const fn descender_px(&self, ppem: u16) -> u32 {
        if self.descender >= 0 || ppem == 0 {
            return 0;
        }
        let n = (self.descender as i64).unsigned_abs() * ppem as u64;
        let d = self.units_per_em as u64;
        n.div_ceil(d) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The values the doc comment claims, verified against what the probe reports for a 16 px line.
    #[test]
    fn inter_line_heights_are_the_ones_the_probe_measures() {
        let f = FontMetrics::INTER;
        assert_eq!(f.units_per_em, 2048);
        assert_eq!(f.em_height(), 2478, "1984 - (-494) + 0");
        // 2478 * 16 / 2048 = 19.359375 -> 20.
        assert_eq!(f.line_height_px(16), 20);
        // 2478 * 22 / 2048 = 26.6181 -> 27.
        assert_eq!(f.line_height_px(22), 27);
        // The Phase 5 geometry test used 20 px body lines, which is this number. Not a coincidence.
        assert_eq!(f.line_height_px(16), 20);
    }

    #[test]
    fn jetbrains_mono_line_heights() {
        let f = FontMetrics::JETBRAINS_MONO;
        assert_eq!(f.units_per_em, 1000);
        assert_eq!(f.em_height(), 1320, "1020 - (-300) + 0");
        assert_eq!(f.line_height_px(16), 22, "1320 * 16 / 1000 = 21.12 -> 22");
        assert_eq!(f.line_height_px(22), 30, "1320 * 22 / 1000 = 29.04 -> 30");
    }

    /// The ceiling must agree with the rational form exactly, for every plausible ppem.
    ///
    /// This is the test that justifies integer arithmetic: if `(n + d - 1) / d` ever disagreed with
    /// `f64::ceil`, a line height would depend on which path computed it.
    #[test]
    fn ceil_matches_the_rational_form_exactly() {
        for &f in &[FontMetrics::INTER, FontMetrics::JETBRAINS_MONO] {
            for ppem in 1..=512u16 {
                let want =
                    ((f.em_height() as f64 * f64::from(ppem)) / f.units_per_em as f64).ceil();
                let got = f64::from(f.line_height_px(ppem));
                assert_eq!(
                    got, want,
                    "line_height_px({ppem}) = {got}, but the rational form ceils to {want}"
                );
            }
        }
    }

    /// The sign convention: `descender` is stored negative and the em height adds it.
    #[test]
    fn a_negative_descender_is_the_font_files_convention_not_an_error() {
        let f = FontMetrics::INTER;
        assert!(f.descender < 0, "hhea.descent is -494 for Inter");
        // Storing it positive would give 990 font units of line box instead of 2478.
        assert_eq!(f.em_height(), 2478);
        let wrong = FontMetrics {
            descender: 494,
            ..f
        };
        assert_eq!(wrong.em_height(), 1984 - 494, "the sign is load-bearing");
    }

    #[test]
    fn zero_and_degenerate_metrics_give_zero_not_a_wrap() {
        let zero = FontMetrics {
            ascender: 0,
            descender: 0,
            line_gap: 0,
            units_per_em: 1000,
        };
        assert_eq!(zero.em_height(), 0);
        assert_eq!(
            zero.line_height_px(16),
            0,
            "a zero line box must not wrap to u32::MAX"
        );

        let ppem_zero = FontMetrics::INTER;
        assert_eq!(
            ppem_zero.line_height_px(0),
            0,
            "a zero ppem has no line height"
        );

        // An ascent and descent that cancel exactly.
        let cancelling = FontMetrics {
            ascender: 500,
            descender: -500,
            line_gap: 0,
            units_per_em: 1000,
        };
        assert_eq!(cancelling.em_height(), 1000);
    }

    #[test]
    fn from_hhea_is_a_straight_copy() {
        let f = FontMetrics::from_hhea(1984, -494, 0, 2048);
        assert_eq!(f, FontMetrics::INTER);
        let m = FontMetrics::from_hhea(1020, -300, 0, 1000);
        assert_eq!(m, FontMetrics::JETBRAINS_MONO);
    }

    #[test]
    #[should_panic(expected = "unitsPerEm is zero")]
    fn a_zero_units_per_em_is_rejected_at_the_boundary() {
        // A divide by zero in integer code is a SIGFPE, not an `Err`, so it has to be refused here.
        let _ = FontMetrics::from_hhea(1000, -200, 0, 0);
    }

    /// The ascent and the descent are independent ceilings, so their sum can exceed the line height
    /// by one pixel -- never more, and never less.
    ///
    /// `ceil(a) + ceil(b)` is always `ceil(a + b)` or `ceil(a + b) + 1`. Short would clip glyphs;
    /// more than one pixel over would waste vertical space on every line of a 60,000-line document.
    ///
    /// An earlier version of this asserted `sum == h || sum + 1 == h` -- the excess on the wrong
    /// side -- and failed at ppem 9 with "baseline 12 vs line height 11", which is exactly the legal
    /// one-pixel excess. The identity is asymmetric and easy to get backwards, and it also had the
    /// names swapped: it compares the *ascender*, not the baseline, because `baseline_px` for these
    /// faces equals `ascender_px` and using it would hide a divergence.
    #[test]
    fn ascent_plus_descent_exceeds_the_line_height_by_at_most_one_pixel() {
        let f = FontMetrics::INTER;
        for ppem in 1..=64u16 {
            let sum = f.ascender_px(ppem) + f.descender_px(ppem);
            let h = f.line_height_px(ppem);
            assert!(
                sum == h || sum == h + 1,
                "ppem {ppem}: glyph box {sum} vs line box {h}"
            );
            // Never short: the glyph box must fit inside the line box, or glyphs clip.
            assert!(sum >= h, "ppem {ppem}: glyph box {sum} is shorter than {h}");
            assert!(
                sum <= h + 1,
                "ppem {ppem}: glyph box {sum} wastes more than 1 px of the {h} px line box"
            );
        }
    }

    #[test]
    fn ascent_and_descent_in_pixels_are_positive_distances() {
        let f = FontMetrics::INTER;
        assert_eq!(f.ascender_px(16), 16, "1984 * 16 / 2048 = 15.5 -> 16");
        assert_eq!(f.descender_px(16), 4, "494 * 16 / 2048 = 3.86 -> 4");
        assert_eq!(f.baseline_px(16), 16);
        assert_eq!(f.baseline_px(22), 22, "1984 * 22 / 2048 = 21.3 -> 22");
    }

    /// A face with a positive line gap must get the extra room, or a deliberately loose font
    /// renders with overlapping lines.
    #[test]
    fn a_line_gap_is_added_before_the_ceiling() {
        let tight = FontMetrics::INTER;
        let loose = FontMetrics {
            line_gap: 256,
            ..tight
        };
        assert_eq!(tight.em_height(), 2478);
        assert_eq!(loose.em_height(), 2734);
        assert_eq!(tight.line_height_px(16), 20);
        assert_eq!(
            loose.line_height_px(16),
            22,
            "256 font units is 2 px at 16 ppem"
        );
    }
}
