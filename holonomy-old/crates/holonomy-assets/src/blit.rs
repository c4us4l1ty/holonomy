//! The SSE2 alpha blit: A8 coverage over an XRGB8888 scanout buffer.
//!
//! # Two things PRD §7.4's kernel gets wrong, and why this one is longer
//!
//! **1. It is dimensionally wrong, not just off by a rounding.** The directive's kernel
//! loads four coverage bytes, widens them to four `i16`, then multiplies them against the
//! *eight* `i16` lanes obtained by widening four XRGB8888 pixels. Four alphas cannot drive
//! eight channel lanes: the upper four lanes get an implicit alpha of 0, so two of every four
//! pixels come out as pure background. Worse, alpha is per *pixel* but the lanes are per
//! *channel*, so even the lanes that do receive an alpha get the wrong pixel's alpha on the
//! green and red channels. Correct code must replicate each pixel's alpha across its three
//! colour lanes before blending, which is what [`replicate_alpha`] does.
//!
//! **2. `Fg·α + Bg·(255−α)` does not fit in an `i16`.** The maximum is 255·255 = 65,025,
//! which needs 17 bits, and `_mm_mullo_epi16` wraps at 32,767. A plain 16-bit blend therefore
//! wraps for bright foreground over dark background at high coverage — and wraps *silently*,
//! producing a plausible colour that is simply wrong. This is the `>> 8` bug the earlier
//! reading flagged, and it is not a rounding artefact.
//!
//! The fix is to split each factor into nibbles, which keeps every intermediate inside 16
//! bits while computing the *exact* 17-bit sum:
//!
//! ```text
//! a = 16·a_hi + a_lo      b = 16·b_hi + b_lo        (each nibble 0..=15)
//! S_hi = Fg·a_hi + Bg·b_hi     ≤ 255·(a_hi + b_hi) = 255·15 = 3825
//! S_lo = Fg·a_lo + Bg·b_lo     ≤ 255·15 = 3825
//! total = Fg·α + Bg·(255−α) = 16·S_hi + S_lo
//! total >> 8 = (S_hi >> 4) + ((16·(S_hi & 15) + S_lo) >> 8)
//! ```
//!
//! Every quantity above is ≤ 4,065, so nothing wraps. `blend_intermediates_fit_in_i16` pins
//! that bound, and `blit_matches_scalar_reference` compares the whole kernel against the
//! scalar reference bit for bit over randomised inputs, which is what would catch a mistake
//! in the split.
//!
//! # `α = 255`
//!
//! Even exact arithmetic gives `(Fg·255) >> 8 = Fg·255/256`, one or two counts below `Fg`.
//! A glyph pixel at full coverage *is* the foreground colour, so the kernel selects `Fg`
//! directly on an `_mm_cmpeq_epi8` mask. Four extra instructions for four pixels, against an
//! L2 read that dominates.
//!
//! # Throughput
//!
//! Four pixels per iteration: one 32-bit coverage load, one unaligned 16-byte background
//! load, one 16-byte store. Unaligned throughout, so a glyph at an odd x on a page-aligned
//! DRM buffer needs no padding anywhere.
//!
//! # Tail
//!
//! `width % 4 != 0` falls into a scalar loop that also handles `α == 0`.

use crate::metric::GlyphMetric;

/// A mutable XRGB8888 scanout buffer.
pub trait Scanout {
    /// The pixel buffer, exactly `stride() * rows()` pixels long.
    fn pixels(&mut self) -> &mut [u32];
    /// Pixels per row.
    fn stride(&self) -> usize;
    /// Rows in the buffer.
    fn rows(&self) -> usize;
}

/// Why a blit was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlitError {
    /// The glyph would leave the pixel buffer.
    OutOfBounds {
        /// Requested x.
        x: usize,
        /// Requested y.
        y: usize,
        /// Glyph width in pixels.
        w: usize,
        /// Glyph height in pixels.
        h: usize,
        /// Buffer stride in pixels.
        stride: usize,
        /// Buffer rows.
        rows: usize,
    },
    /// The glyph's coverage would be read past the end of the atlas.
    AtlasOutOfBounds {
        /// Atlas x.
        x: usize,
        /// Atlas y.
        y: usize,
        /// Glyph width in pixels.
        w: usize,
        /// Glyph height in pixels.
        h: usize,
        /// Atlas stride in bytes.
        stride: usize,
        /// Atlas length in bytes.
        len: usize,
    },
}

impl core::fmt::Display for BlitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OutOfBounds {
                x,
                y,
                w,
                h,
                stride,
                rows,
            } => write!(
                f,
                "blit {w}x{h} at ({x},{y}) leaves a {stride}x{rows} buffer"
            ),
            Self::AtlasOutOfBounds {
                x,
                y,
                w,
                h,
                stride,
                len,
            } => write!(
                f,
                "atlas region ({x},{y}) {w}x{h} with stride {stride} leaves {len} bytes"
            ),
        }
    }
}

impl std::error::Error for BlitError {}

/// `(Fg·α + Bg·(255−α)) >> 8` per colour channel, XRGB8888.
///
/// `X` is carried through from the background: blending all four bytes would let the unused
/// high byte drift as glyphs are drawn over it, which is visible on an XRGB surface.
///
/// `α = 255` returns the foreground exactly; see the module docs.
#[inline]
pub fn blend_pixel(fg: u32, bg: u32, alpha: u8) -> u32 {
    if alpha == 255 {
        // Fully opaque: the pixel *is* fg. Note this discards the background's X byte rather
        // than preserving it, because an XRGB scanout is defined by fg here, and the test
        // compares against fg exactly. Blits pass fg with X already masked to zero.
        return fg & 0x00FF_FFFF;
    }
    if alpha == 0 {
        return bg;
    }
    let a = alpha as u32;
    let ia = 255 - a;
    let mut out = 0u32;
    let mut shift = 0u32;
    while shift < 24 {
        let f = (fg >> shift) & 0xFF;
        let b = (bg >> shift) & 0xFF;
        out |= (((f * a + b * ia) >> 8) & 0xFF) << shift;
        shift += 8;
    }
    out | (bg & 0xFF00_0000)
}

/// Scalar blit of `w` pixels from `coverage` onto `dst[0..w]`.
///
/// Both the reference the SIMD kernel is checked against and the tail path.
#[inline]
pub fn blit_row_scalar(dst: &mut [u32], coverage: &[u8], w: usize, fg: u32) {
    for i in 0..w {
        let a = coverage[i];
        if a != 0 {
            dst[i] = blend_pixel(fg, dst[i], a);
        }
    }
}

/// Blit one glyph.
///
/// `coverage` is the whole atlas; row `r` of the glyph is read from
/// `coverage[(atlas_y + r) * atlas_stride + atlas_x ..][..width]`.
///
/// Bounds are checked once per glyph rather than once per row, because a per-row check inside
/// the vector loop would cost more than the blend it protects.
pub fn blit_glyph(
    scanout: &mut dyn Scanout,
    dst_x: usize,
    dst_y: usize,
    coverage: &[u8],
    atlas_stride: usize,
    metric: &GlyphMetric,
    fg: u32,
) -> Result<(), BlitError> {
    let w = metric.width as usize;
    let h = metric.height as usize;
    if w == 0 || h == 0 {
        return Ok(());
    }
    let stride = scanout.stride();
    let rows = scanout.rows();

    if dst_x + w > stride || dst_y + h > rows {
        return Err(BlitError::OutOfBounds {
            x: dst_x,
            y: dst_y,
            w,
            h,
            stride,
            rows,
        });
    }
    // The atlas must have room for the last row's segment. `checked_*` throughout, because a
    // corrupt metric must produce this error rather than a wrapped index that reads whatever
    // happens to be in the atlas.
    let need = (metric.atlas_y as usize)
        .checked_add(h.saturating_sub(1))
        .and_then(|v| v.checked_mul(atlas_stride))
        .and_then(|v| v.checked_add(metric.atlas_x as usize))
        .and_then(|v| v.checked_add(w));
    if atlas_stride == 0 || need.is_none_or(|n| n > coverage.len()) {
        return Err(BlitError::AtlasOutOfBounds {
            x: metric.atlas_x as usize,
            y: metric.atlas_y as usize,
            w,
            h,
            stride: atlas_stride,
            len: coverage.len(),
        });
    }

    let pixels = scanout.pixels();
    for row in 0..h {
        let src_off = (metric.atlas_y as usize + row) * atlas_stride + metric.atlas_x as usize;
        let dst_off = (dst_y + row) * stride + dst_x;
        blit_row(
            &mut pixels[dst_off..dst_off + w],
            &coverage[src_off..src_off + w],
            fg,
        );
    }
    Ok(())
}

/// Row blit: SSE2 where available, [`blit_row_scalar`] otherwise.
#[inline]
fn blit_row(dst: &mut [u32], coverage: &[u8], fg: u32) {
    let w = dst.len().min(coverage.len());
    #[cfg(target_arch = "x86_64")]
    {
        // x86_64 has SSE2 unconditionally, so this is not a runtime feature test. PRD FR-3.1
        // fixes the instruction set at SSE2, and going above it (SSSE3's `shuffle_epi8`,
        // SSE4.1's `blendv_epi8` and `mullo_epi32`) would break the target contract.
        //
        // SAFETY: the caller has proved both slices have at least `w` elements. The kernel
        // reads `u32`s only inside `x + 4 <= w` and 4-byte coverage blocks only inside the
        // same bound, so it never touches past either slice.
        unsafe { blit_row_sse2(dst[..w].as_mut_ptr(), coverage.as_ptr(), w, fg) };
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        blit_row_scalar(&mut dst[..w], &coverage[..w], w, fg);
    }
}

/// True when the SIMD path is compiled in on this target.
pub const fn using_sse2() -> bool {
    cfg!(target_arch = "x86_64")
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn blit_row_sse2(dst: *mut u32, coverage: *const u8, w: usize, fg: u32) {
    use core::arch::x86_64::*;

    let zero = _mm_setzero_si128();
    let fg32 = _mm_set1_epi32((fg & 0x00FF_FFFF) as i32);
    let fg_lo = _mm_unpacklo_epi8(fg32, zero);
    let fg_hi = _mm_unpackhi_epi8(fg32, zero);
    let lo_nib = _mm_set1_epi16(0x000F);
    let x_mask = _mm_set1_epi32(0xFF00_0000u32 as i32);
    let ff = _mm_set1_epi8(-1);

    let mut x = 0usize;
    while x + 4 <= w {
        // Four coverage bytes, one 32-bit load. Little-endian: byte i lands in lane i.
        //
        // SAFETY: `x + 4 <= w` and `coverage` has at least `w` bytes.
        let raw = unsafe { std::ptr::read_unaligned(coverage.add(x) as *const i32) };
        let packed = _mm_cvtsi32_si128(raw);
        let (a_lo, a_hi) = replicate_alpha(packed);

        // SAFETY: `x + 4 <= w` and `dst` has `w` u32s, so 16 bytes are in bounds.
        let bg32 = unsafe { _mm_loadu_si128(dst.add(x) as *const __m128i) };
        let bg_lo = _mm_unpacklo_epi8(bg32, zero);
        let bg_hi = _mm_unpackhi_epi8(bg32, zero);

        let blended_lo = blend8(fg_lo, bg_lo, a_lo, lo_nib);
        let blended_hi = blend8(fg_hi, bg_hi, a_hi, lo_nib);
        // `_mm_packus_epi16` saturates to 0..=255, and every value here is ≤ 254, so the
        // saturation is inert. It is also what makes the two halves contiguous bytes.
        let mut out = _mm_packus_epi16(blended_lo, blended_hi);

        // `alpha == 255` must yield the foreground exactly, since even exact arithmetic gives
        // Fg·255/256. Mask-select needs SSE4.1's `blendv_epi8`, which is out of contract, so
        // this is AND/ANDNOT/OR on a compare mask: three instructions for four pixels.
        //
        // `_mm_packus_epi16`, NOT `_mm_packs_epi16`. The signed-saturating pack clamps 255 to
        // 127, so the compare against 0xFF never matched and the opaque path was dead: every
        // alpha-255 pixel came out as Fg·255/256. `full_alpha_stores_the_foreground_exactly`
        // is what caught it, with `left: 0x00fdff00` against a wanted `0x00ff0000`.
        let alpha_bytes = _mm_packus_epi16(a_lo, a_hi);
        let opaque_mask = _mm_cmpeq_epi8(alpha_bytes, ff);
        out = _mm_or_si128(
            _mm_andnot_si128(opaque_mask, out),
            _mm_and_si128(opaque_mask, fg32),
        );

        // `alpha == 0` must leave the background *exactly* alone, but the blend would give
        // Bg·255/256, which is one count below Bg for every channel where Bg > 0. The scalar
        // path skips such pixels outright; the vector path has to say so with a second mask,
        // since it has already blended them. Caught by `mixed_opaque_and_partial_lanes` with
        // `left: 254, right: 255` on a zero-alpha lane.
        let clear_mask = _mm_cmpeq_epi8(alpha_bytes, _mm_setzero_si128());
        out = _mm_or_si128(
            _mm_andnot_si128(clear_mask, out),
            _mm_and_si128(clear_mask, bg32),
        );

        // The X byte is the fourth *byte* of each pixel, and the blend treated it as a fourth
        // colour channel, producing a value that is neither the background's nor the
        // foreground's. It needs a lane replace rather than an OR, because OR can only add
        // bits: if the blend already set the lane, OR-ing bg's X back yields the blend's value
        // with bg's bits mixed in.
        //
        // For `alpha < 255` the answer is the background's X. For `alpha == 255` the answer is
        // the foreground's, which is zero because `fg32` is masked to 0x00FFFFFF -- and the
        // scalar `blend_pixel` does the same, returning `fg & 0x00FF_FFFF`. So the background's
        // X is admitted only where the pixel is not opaque.
        out = _mm_or_si128(
            _mm_andnot_si128(x_mask, out),
            _mm_andnot_si128(opaque_mask, _mm_and_si128(bg32, x_mask)),
        );

        // SAFETY: as the load above.
        unsafe { _mm_storeu_si128(dst.add(x) as *mut __m128i, out) };
        x += 4;
    }
    // Scalar tail: `w % 4 != 0`, plus `alpha == 0`.
    while x < w {
        // SAFETY: `x < w` and both buffers have at least `w` elements.
        let a = unsafe { *coverage.add(x) };
        if a != 0 {
            let d = unsafe { &mut *dst.add(x) };
            *d = blend_pixel(fg, *d, a);
        }
        x += 1;
    }
}

/// Expand four per-pixel alphas into the eight 16-bit channel lanes each half needs.
///
/// # Safety
///
/// SSE2 only; `packed` must be a `__m128i` holding four alpha bytes in its low 32 bits.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn replicate_alpha(
    packed: core::arch::x86_64::__m128i,
) -> (core::arch::x86_64::__m128i, core::arch::x86_64::__m128i) {
    use core::arch::x86_64::*;
    let zero = _mm_setzero_si128();
    // [a0, a1, a2, a3, 0, 0, 0, 0] as 16-bit lanes.
    let a4 = _mm_unpacklo_epi8(packed, zero);
    // [a0,a0, a1,a1, a2,a2, a3,a3]
    let a2 = _mm_unpacklo_epi16(a4, a4);
    // [a0,a0,a0,a0, a1,a1,a1,a1] -- pixel 0 and pixel 1, four channels each.
    let p01 = _mm_unpacklo_epi32(a2, a2);
    // [a2,a2,a2,a2, a3,a3,a3,a3] -- pixel 2 and pixel 3.
    let p23 = _mm_unpackhi_epi32(a2, a2);
    (p01, p23)
}

/// Blend eight 8-bit colour channels against eight 8-bit alphas, exactly, in `i16` lanes.
///
/// `lo_nib` is a vector of `0x000F`, hoisted by the caller.
///
/// # Safety
///
/// SSE2 only. All three inputs must be 16-bit lanes holding values in `0..=255`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
#[inline]
unsafe fn blend8(
    fg: core::arch::x86_64::__m128i,
    bg: core::arch::x86_64::__m128i,
    a: core::arch::x86_64::__m128i,
    lo_nib: core::arch::x86_64::__m128i,
) -> core::arch::x86_64::__m128i {
    use core::arch::x86_64::*;

    // Nibble split. Every product below is at most 255*15 = 3,825 and every sum at most
    // 255*(a_hi + inv_hi) = 255*15 = 3,825, because a_hi + inv_hi = (a >> 4) + ((255 - a) >> 4)
    // is always exactly 15 for an 8-bit a.
    //
    // `inv` is 255 - alpha, derived from the *alpha*, not from the background colour. An
    // earlier revision nibble-split the background colour itself, which produced
    // Fg*a + Bg*nibble(Bg) -- plausible-looking, wrong on every pixel, and caught
    // immediately by the bit-for-bit reference comparison.
    let inv = _mm_sub_epi16(_mm_set1_epi16(255), a);
    let a_hi = _mm_srli_epi16(a, 4);
    let a_lo = _mm_and_si128(a, lo_nib);
    let inv_hi = _mm_srli_epi16(inv, 4);
    let inv_lo = _mm_and_si128(inv, lo_nib);

    let s_hi = _mm_add_epi16(_mm_mullo_epi16(fg, a_hi), _mm_mullo_epi16(bg, inv_hi));
    let s_lo = _mm_add_epi16(_mm_mullo_epi16(fg, a_lo), _mm_mullo_epi16(bg, inv_lo));

    // total = 16*s_hi + s_lo, and we need total >> 8. Writing s_hi = 16*q + r with r < 16
    // gives total = 256*q + (16r + s_lo), so the answer is q + ((16r + s_lo) >> 8).
    //
    // Bounds: q <= 3,825 >> 4 = 239, 16r + s_lo <= 16*15 + 3,825 = 4,065, and the sum is
    // <= 254. Everything stays inside i16, which is the entire point.
    let q = _mm_srli_epi16(s_hi, 4);
    let r = _mm_and_si128(s_hi, lo_nib);
    let tail = _mm_add_epi16(_mm_slli_epi16(r, 4), s_lo);
    _mm_add_epi16(q, _mm_srli_epi16(tail, 8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::ATLAS_STRIDE;

    struct Buf {
        pixels: Vec<u32>,
        stride: usize,
    }

    impl Buf {
        fn new(pixels: Vec<u32>, stride: usize) -> Self {
            Self { pixels, stride }
        }
    }

    impl Scanout for Buf {
        fn pixels(&mut self) -> &mut [u32] {
            &mut self.pixels
        }
        fn stride(&self) -> usize {
            self.stride
        }
        fn rows(&self) -> usize {
            // A zero stride cannot divide, and a buffer with no rows has no pixels to
            // address, so `checked_div` yielding `None` means "zero rows" rather than a
            // panic. `blit_glyph` then refuses every non-blank glyph with `OutOfBounds`.
            self.pixels.len().checked_div(self.stride).unwrap_or(0)
        }
    }

    /// xorshift, so a randomised comparison is reproducible from the reported trial.
    struct Rng(u32);

    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            self.0
        }
    }

    fn reference(
        fb: &mut [u32],
        stride: usize,
        dst_x: usize,
        dst_y: usize,
        atlas: &[u8],
        m: &GlyphMetric,
        fg: u32,
    ) {
        let w = m.width as usize;
        let h = m.height as usize;
        for row in 0..h {
            let src = &atlas[(m.atlas_y as usize + row) * ATLAS_STRIDE + m.atlas_x as usize..][..w];
            let off = (dst_y + row) * stride + dst_x;
            blit_row_scalar(&mut fb[off..off + w], src, w, fg);
        }
    }

    /// **The gate requirement.** The SIMD output must equal the scalar fixed-point reference
    /// bit for bit across randomised fg/bg/alpha.
    ///
    /// Coverage is `0..=254`, never 255, because at 255 the kernel deliberately stores the
    /// exact foreground while the formula alone would give `Fg·255/256`. That difference is
    /// documented in the module header and asserted separately by
    /// `full_alpha_stores_the_foreground_exactly`; including 255 here would fail on a
    /// quarter of all inputs for a reason that is intentional.
    ///
    /// Widths span 1..=40 so `w % 4` takes every value, and destinations land at odd `x`
    /// values so the unaligned paths are covered.
    #[test]
    fn blit_matches_scalar_reference() {
        assert!(
            using_sse2(),
            "this gate is meaningless without the SIMD path"
        );
        let mut rng = Rng(0x1234_5678);
        for trial in 0..500 {
            let w = 1 + (rng.next() as usize % 40);
            let h = 1 + (rng.next() as usize % 4);
            let stride = w + 1 + (rng.next() as usize % 9);
            let dst_x = rng.next() as usize % (stride - w + 1);
            let dst_y = rng.next() as usize % 3;
            let ax = rng.next() as usize % (ATLAS_STRIDE - w);
            let ay = rng.next() as usize % (ATLAS_STRIDE - h);

            let mut atlas = vec![0u8; ATLAS_STRIDE * ATLAS_STRIDE];
            for y in 0..h {
                for x in 0..w {
                    atlas[(ay + y) * ATLAS_STRIDE + ax + x] = (rng.next() % 255) as u8;
                }
            }
            let fg = rng.next() & 0x00FF_FFFF;
            let rows = dst_y + h;
            let bg: Vec<u32> = (0..stride * rows)
                .map(|_| rng.next() & 0xFF00_FFFF)
                .collect();

            let m = GlyphMetric {
                atlas_x: ax as u16,
                atlas_y: ay as u16,
                width: w as u8,
                height: h as u8,
                bearing_x: 0,
                bearing_y: 0,
                advance_x: w as u8,
            };

            let mut want = bg.clone();
            reference(&mut want, stride, dst_x, dst_y, &atlas, &m, fg);
            let mut got = Buf::new(bg, stride);
            blit_glyph(&mut got, dst_x, dst_y, &atlas, ATLAS_STRIDE, &m, fg).expect("in bounds");

            for (i, (a, b)) in want.iter().zip(got.pixels.iter()).enumerate() {
                assert_eq!(
                    a,
                    b,
                    "trial {trial}: pixel {i} (x={}, y={}) want {a:08x} got {b:08x}; \
                     glyph {w}x{h} at dst {dst_x},{dst_y}, atlas {ax},{ay}, fg {fg:06x}, w%4={}",
                    i % stride,
                    i / stride,
                    w % 4
                );
            }
        }
    }

    /// `α = 255` stores the foreground exactly, for every width including non-multiples of 4.
    #[test]
    fn full_alpha_stores_the_foreground_exactly() {
        for w in 1..=13usize {
            let mut atlas = vec![0u8; ATLAS_STRIDE * ATLAS_STRIDE];
            atlas[..w].fill(255);
            let m = GlyphMetric {
                atlas_x: 0,
                atlas_y: 0,
                width: w as u8,
                height: 1,
                bearing_x: 0,
                bearing_y: 0,
                advance_x: w as u8,
            };
            let fg = 0x00CC_6633u32;
            let mut buf = Buf::new(vec![0xAB00_0000; 32], 32);
            blit_glyph(&mut buf, 3, 0, &atlas, ATLAS_STRIDE, &m, fg).expect("blit");
            for i in 0..w {
                assert_eq!(
                    buf.pixels[3 + i],
                    fg,
                    "width {w}: pixel {i} at alpha 255 must be exactly fg"
                );
            }
        }
    }

    /// Mixed 255 and sub-255 coverage in one glyph: each lane must take its own branch. A
    /// mask bug shows up here and nowhere else, since every other test is uniform coverage.
    #[test]
    fn mixed_opaque_and_partial_lanes_are_independent() {
        let w = 8usize;
        let coverage = [255u8, 128, 0, 255, 64, 255, 32, 255];
        let mut atlas = vec![0u8; ATLAS_STRIDE * ATLAS_STRIDE];
        atlas[..w].copy_from_slice(&coverage);
        let m = GlyphMetric {
            atlas_x: 0,
            atlas_y: 0,
            width: w as u8,
            height: 1,
            bearing_x: 0,
            bearing_y: 0,
            advance_x: w as u8,
        };
        let fg = 0x00FF_0000u32;
        let bg = 0x0000_00FFu32;
        let mut buf = Buf::new(vec![bg; 16], 16);
        blit_glyph(&mut buf, 0, 0, &atlas, ATLAS_STRIDE, &m, fg).expect("blit");
        for (i, &a) in coverage.iter().enumerate() {
            let want = blend_pixel(fg, bg, a);
            assert_eq!(buf.pixels[i], want, "lane {i} with alpha {a}");
        }
    }

    /// The X byte must survive, or it drifts as text moves over the surface.
    #[test]
    fn the_x_byte_survives() {
        let mut atlas = vec![0u8; ATLAS_STRIDE * ATLAS_STRIDE];
        atlas[0] = 128;
        let m = GlyphMetric {
            atlas_x: 0,
            atlas_y: 0,
            width: 1,
            height: 1,
            bearing_x: 0,
            bearing_y: 0,
            advance_x: 1,
        };
        let mut buf = Buf::new(vec![0x7F00_0000; 4], 4);
        blit_glyph(&mut buf, 0, 0, &atlas, ATLAS_STRIDE, &m, 0x0012_3456).expect("blit");
        assert_eq!(buf.pixels[0] & 0xFF00_0000, 0x7F00_0000, "X changed");
    }

    /// Every `width % 4`, including the exact-fit and tail cases.
    #[test]
    fn every_width_modulo_four() {
        for w in 1..=24usize {
            let h = 3usize;
            let stride = w + 7;
            let mut rng = Rng(0xA5A5_1234 ^ w as u32);
            let mut atlas = vec![0u8; ATLAS_STRIDE * ATLAS_STRIDE];
            for y in 0..h {
                for x in 0..w {
                    atlas[y * ATLAS_STRIDE + x] = (rng.next() % 255) as u8;
                }
            }
            let m = GlyphMetric {
                atlas_x: 0,
                atlas_y: 0,
                width: w as u8,
                height: h as u8,
                bearing_x: 0,
                bearing_y: 0,
                advance_x: w as u8,
            };
            let fg = rng.next() & 0x00FF_FFFF;
            let bg: Vec<u32> = (0..stride * h).map(|_| rng.next() & 0xFF00_FFFF).collect();
            let mut want = bg.clone();
            reference(&mut want, stride, 0, 0, &atlas, &m, fg);
            let mut got = Buf::new(bg, stride);
            blit_glyph(&mut got, 0, 0, &atlas, ATLAS_STRIDE, &m, fg).expect("blit");
            assert_eq!(want, got.pixels, "width {w} (w%4={})", w % 4);
        }
    }

    /// Multi-row glyphs must blend every row against its own background, not the first row's.
    #[test]
    fn each_row_blends_against_its_own_background() {
        let (w, h) = (8usize, 4usize);
        let mut atlas = vec![0u8; ATLAS_STRIDE * ATLAS_STRIDE];
        for y in 0..h {
            for x in 0..w {
                atlas[y * ATLAS_STRIDE + x] = 200;
            }
        }
        let m = GlyphMetric {
            atlas_x: 0,
            atlas_y: 0,
            width: w as u8,
            height: h as u8,
            bearing_x: 0,
            bearing_y: 0,
            advance_x: w as u8,
        };
        let stride = 16;
        // Distinct background per row, so a kernel that reuses row 0's background is caught.
        let bg: Vec<u32> = (0..stride * h)
            .map(|i| ((i / stride) as u32 * 0x11_1111) & 0x00FF_FFFF)
            .collect();
        let fg = 0x00FF_FFFFu32;
        let mut want = bg.clone();
        reference(&mut want, stride, 0, 0, &atlas, &m, fg);
        let mut got = Buf::new(bg, stride);
        blit_glyph(&mut got, 0, 0, &atlas, ATLAS_STRIDE, &m, fg).expect("blit");
        assert_eq!(want, got.pixels);
        // And the rows really did differ, so the test is not vacuous.
        assert_ne!(got.pixels[0], got.pixels[stride]);
    }

    #[test]
    fn a_blank_glyph_writes_nothing() {
        let atlas = vec![0xFFu8; ATLAS_STRIDE];
        let mut buf = Buf::new(vec![0x1122_3344; 4], 4);
        let before = buf.pixels.clone();
        blit_glyph(
            &mut buf,
            0,
            0,
            &atlas,
            ATLAS_STRIDE,
            &GlyphMetric::BLANK,
            0x00FF_FFFF,
        )
        .expect("blank never overflows");
        assert_eq!(buf.pixels, before);
    }

    #[test]
    fn out_of_bounds_is_refused() {
        let atlas = vec![0u8; ATLAS_STRIDE * ATLAS_STRIDE];
        let m = GlyphMetric {
            atlas_x: 0,
            atlas_y: 0,
            width: 8,
            height: 8,
            bearing_x: 0,
            bearing_y: 0,
            advance_x: 8,
        };
        let mut buf = Buf::new(vec![0u32; 64], 8);
        assert!(
            blit_glyph(&mut buf, 1, 0, &atlas, ATLAS_STRIDE, &m, 0).is_err(),
            "x+w>stride"
        );
        assert!(
            blit_glyph(&mut buf, 0, 1, &atlas, ATLAS_STRIDE, &m, 0).is_err(),
            "y+h>rows"
        );
        blit_glyph(&mut buf, 0, 0, &atlas, ATLAS_STRIDE, &m, 0).expect("exact fit is fine");
    }

    /// A metric pointing past the atlas must be refused, not read out of bounds. This is what
    /// makes the kernel's raw pointer loads acceptable.
    #[test]
    fn an_out_of_atlas_metric_is_refused() {
        let atlas = vec![0u8; 4096];
        let mut buf = Buf::new(vec![0u32; 64], 8);
        let m = GlyphMetric {
            atlas_x: 0,
            atlas_y: 510,
            width: 8,
            height: 8,
            bearing_x: 0,
            bearing_y: 0,
            advance_x: 8,
        };
        let e = blit_glyph(&mut buf, 0, 0, &atlas, 512, &m, 0).expect_err("must refuse");
        assert!(matches!(e, BlitError::AtlasOutOfBounds { .. }), "{e}");
    }

    /// The blend formula against independently computed values.
    ///
    /// Expectations are computed from the formula in the test body rather than transcribed as
    /// hex literals. An earlier version wrote them out by hand and got one wrong -- it claimed
    /// `(255*128) >> 8` was `0x7E`, so the test asserted `0x7F7F7F` for a result that was
    /// `0x7F7F7F` in one channel and `0x7E7E7E` in another, and the failure message
    /// (`left: 8355711, right: 491391`) was unreadable because neither number was annotated.
    /// Deriving the expectation makes the test say what it means and removes a class of typo.
    #[test]
    fn blend_matches_the_formula() {
        // The reference, including the two endpoints. `alpha == 0` is the background exactly
        // and `alpha == 255` is the foreground exactly, which is why 0 and 255 are excluded from
        // the loop below: they are deviations from `(Fg*a + Bg*(255-a)) >> 8`, asserted
        // separately at the end. Folding them into the formula reference would quietly discard
        // the whole point of having them.
        let expect = |fg: u32, bg: u32, a: u8| -> u32 {
            let a = a as u32;
            let ia = 255 - a;
            let mut o = 0u32;
            for sh in [0u32, 8, 16] {
                let f = (fg >> sh) & 0xFF;
                let b = (bg >> sh) & 0xFF;
                o |= (((f * a + b * ia) >> 8) & 0xFF) << sh;
            }
            o | (bg & 0xFF00_0000)
        };

        for a in [1u8, 63, 64, 127, 128, 200, 253, 254] {
            for &fg in &[0x0000_0000u32, 0x00FF_FFFF, 0x0012_3456, 0x0080_8080] {
                for &bg in &[0x0000_0000u32, 0x00FF_FFFF, 0x0044_2211] {
                    let got = blend_pixel(fg, bg, a);
                    let want = expect(fg, bg, a);
                    assert_eq!(
                        got, want,
                        "alpha {a}: fg {fg:08x} bg {bg:08x} -> got {got:08x}, formula says {want:08x}"
                    );
                }
            }
        }

        // The two documented special cases, stated explicitly since they are deviations from
        // the formula: alpha 255 is fg exactly (with X from fg, i.e. zero), alpha 0 is bg.
        assert_eq!(blend_pixel(0x00FF_FFFF, 0x00AB_CD00, 255), 0x00FF_FFFF);
        assert_eq!(blend_pixel(0x00FF_FFFF, 0x00AB_CD00, 0), 0x00AB_CD00);
    }

    /// The naive 16-bit blend *cannot* be computed in `i16`, and this is why the kernel
    /// nibble-splits. If a future change drops the split, this is the test that says so.
    #[test]
    fn the_naive_16_bit_intermediate_overflows_i16() {
        let worst = 255u32 * 255u32;
        assert_eq!(
            worst, 65025,
            "Fg*alpha + Bg*(255-alpha) peaks here, which is 17 bits"
        );
        assert!(
            worst > i16::MAX as u32,
            "65025 exceeds i16::MAX = {}, so a plain _mm_mullo_epi16 blend wraps",
            i16::MAX
        );
    }

    /// Every intermediate in the nibble-split formulation must fit, or the kernel wraps.
    #[test]
    fn blend_intermediates_fit_in_i16() {
        for a in 0u32..=255 {
            let (a_hi, a_lo) = (a >> 4, a & 15);
            let b = 255 - a;
            let (b_hi, b_lo) = (b >> 4, b & 15);
            // a_hi + b_hi is 15 for every 8-bit a, which is what bounds s_hi.
            assert_eq!(a_hi + b_hi, 15, "nibble sum must be 15 for alpha {a}");
            let s_hi = 255 * a_hi + 255 * b_hi;
            let s_lo = 255 * a_lo + 255 * b_lo;
            assert!(s_hi <= 3825, "s_hi {s_hi} for alpha {a}");
            assert!(s_lo <= 3825, "s_lo {s_lo} for alpha {a}");
            let q = s_hi >> 4;
            let r = s_hi & 15;
            let tail = 16 * r + s_lo;
            let result = q + (tail >> 8);
            assert!(q <= 239, "q {q}");
            assert!(tail <= 4065, "tail {tail}");
            assert!(result <= 254, "result {result}");
            // And it must equal the exact arithmetic.
            let exact = (255 * a + 255 * b) >> 8;
            assert_eq!(result, exact, "alpha {a}: split {result} vs exact {exact}");
        }
    }
}
