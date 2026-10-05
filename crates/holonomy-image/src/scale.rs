//! The scaler: fixed-point bilinear, integer-only, SSE2 on the axis where it helps.
//!
//! # Why the scaler runs on every image
//!
//! §2.9.3's decision -- the cache holds *page-column-width* rasters, not native ones -- means every
//! image in every document is a downscale. A 1920x1080 photo becomes 640x360. So the scaler is not an
//! occasional path that can go untested; it is the only path, and the gate for it is a real document
//! with real images in it.
//!
//! # Fixed point, and no floating point anywhere
//!
//! Positions are [`FRAC`]-bit fractions in `u32`. `FRAC == 8` is not arbitrary: it is the most
//! fractional bits a `u16` weight carries with the products still fitting (see [`scale_y`]'s
//! overflow argument), and 1/256 px is well under a pixel of position error at any size this product
//! renders at.
//!
//! Floating point is excluded for three reasons, in order of weight:
//!
//! 1. **Determinism.** The 9C gate compares decoded rasters byte for byte. `f32` is deterministic on
//!    one target, but the temptation to reassociate it for speed is exactly the sort of change that
//!    silently moves the comparison.
//! 2. **The target's instruction set is fixed at SSE2** (Phase 5, and `holonomy_assets::blit`), and
//!    `f32` would put the scaler on a different code path from the rest of the blitter.
//! 3. **No `sqrt` is needed.** Bilinear needs only `step = src / dst`, and both are `u32`, so the
//!    step is `(src << FRAC) / dst`: one integer division per axis, once.
//!
//! # Two passes, and which one gets the SSE2
//!
//! Horizontal first (into a full-height intermediate), then vertical. The two-pass form is what keeps
//! the *vertical* pass reading contiguous memory, which is why it gets the vector path; a single-pass
//! 2D bilinear reads four pixels at arbitrary strides, and SSE2 has no gather.
//!
//! **The horizontal pass is scalar, deliberately.** Its four source samples sit at arbitrary `x`, so
//! each output pixel is a gather of four unrelated bytes. There is no SSE2 instruction for that, and
//! emulating one with four loads and inserts costs more than it saves at these sizes. Stating that
//! here rather than claiming the whole scaler is vectorised: the vertical pass is SSE2 and the
//! horizontal pass is not.
//!
//! # The intermediate buffer, and why it is not in the budget
//!
//! The horizontal pass needs every destination column of a source row before the vertical pass can
//! combine rows, so the intermediate is `dst_w * src_h * 4`. At the product's numbers that is
//! 640 * 1080 * 4 = 2.64 MiB for a full-height 1080p source, against 0.88 MiB for the result. It is
//! allocated once per resample and dropped when this returns, so it is *not* part of the cache's
//! resident budget -- which is why [`IcebergCache`](crate::IcebergCache)'s accounting covers rasters
//! and this buffer appears nowhere in it. That is stated rather than left for someone to find the
//! discrepancy between `resident_bytes()` and `ps`.

use crate::error::{PngError, Result};

/// Fractional bits in a fixed-point weight. See the module docs.
pub const FRAC: u32 = 8;

/// One, in [`FRAC`] fixed point.
pub const ONE: u32 = 1 << FRAC;

/// A decoded RGBA image: `width` x `height`, 4 bytes per pixel, tightly packed, no row padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes, row-major.
    pub pixels: Vec<u8>,
}

impl Rgba {
    /// A zeroed image.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0u8; bytes_for(width, height)],
        }
    }

    /// Bytes one row occupies.
    #[inline]
    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }

    /// The pixel at `(x, y)`, or `None` outside the image.
    #[inline]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = y as usize * self.stride() + x as usize * 4;
        let p = self.pixels.get(at..at + 4)?;
        Some([p[0], p[1], p[2], p[3]])
    }
}

/// Bytes a `width x height` RGBA image occupies, saturating so a hostile header cannot overflow.
pub fn bytes_for(width: u32, height: u32) -> usize {
    (u64::from(width) * u64::from(height) * 4).min(usize::MAX as u64) as usize
}

/// One destination sample's position in the source, as `(lo, hi, weight)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// The nearer source pixel.
    pub lo: u32,
    /// The further source pixel, clamped to the last when `lo` is already the last.
    pub hi: u32,
    /// [`FRAC`]-bit weight to move from `lo` toward `hi`, in `[0, ONE]`.
    pub weight: u32,
}

/// The sampling map for one axis: `dst` destination pixels against `src` source pixels.
///
/// **Pixel centres, deliberately.** Destination pixel `i` samples at source coordinate
/// `(i + 0.5) * src / dst - 0.5`, the centre of the destination pixel rather than its left edge. This
/// is the standard image-resampling convention and it matters: under a left-edge convention a
/// downscale by an even factor drops every other source pixel's contribution and the result is
/// visibly shifted up and left by half a destination pixel. It is also why the first destination
/// pixel's source position is negative, handled by clamping to 0 rather than by wrapping.
///
/// Built once per axis so neither inner loop divides.
pub fn axis_map(src: u32, dst: u32) -> Vec<Sample> {
    let mut out = Vec::with_capacity(dst as usize);
    if src == 0 || dst == 0 {
        return out;
    }
    // pos(i) = (i + 0.5) * src / dst - 0.5, in FRAC-bit fixed point, with everything in u64:
    //   twice = (2i + 1) * src * ONE / dst   is the source position *2*
    //   pos   = (twice - ONE) / 2
    // `(2i + 1) * src * ONE` reaches 2 * 32e6 * 256 = 1.6e10, well past u32.
    let num = u64::from(src) * u64::from(ONE);
    let den = u64::from(dst);
    let last = src - 1;
    for i in 0..dst {
        let twice = 2 * u64::from(i) + 1;
        let pos2 = (twice * num) / den;
        // Subtract the half-pixel in fixed point, then halve. `pos2 - ONE` can be negative for the
        // first destination pixel, which is exactly the case the clamp is for.
        let pos = (pos2 as i64 - i64::from(ONE)) / 2;
        let clamped = pos.max(0) as u64;
        let lo = (clamped / u64::from(ONE)).min(u64::from(last)) as u32;
        let weight = (clamped % u64::from(ONE)) as u32;
        out.push(Sample {
            lo,
            hi: (lo + 1).min(last),
            weight,
        });
    }
    out
}

/// Resample `src` to exactly `dst_w x dst_h`, bilinear, writing RGBA into `dst`.
///
/// `dst` must be at least [`bytes_for`] long; the length is checked, not trusted.
///
/// See the module docs for why this is two passes and why only the second is vectorised.
pub fn resample(src: &Rgba, dst_w: u32, dst_h: u32, dst: &mut [u8]) -> Result<()> {
    let want = bytes_for(dst_w, dst_h);
    if dst.len() < want {
        return Err(PngError::DestinationTooSmall {
            want,
            have: dst.len(),
        });
    }
    if src.width == 0 || src.height == 0 {
        // The decoder refuses a zero-sized image at `IHDR`, so this is only reachable by
        // constructing an `Rgba` by hand.
        return Err(PngError::ZeroDimension);
    }
    // A zero destination is not an error: it is the degenerate "show nothing", and returning an empty
    // image for it is more useful than refusing.
    if dst_w == 0 || dst_h == 0 {
        return Ok(());
    }

    // Pass 1: horizontally, every source row down to `dst_w`.
    let xmap = axis_map(src.width, dst_w);
    let mid_stride = dst_w as usize * 4;
    let mut mid = vec![0u8; mid_stride * src.height as usize];
    for y in 0..src.height as usize {
        let row = &src.pixels[y * src.stride()..(y + 1) * src.stride()];
        let out = &mut mid[y * mid_stride..(y + 1) * mid_stride];
        scale_x(row, src.width, out, &xmap);
    }

    // Pass 2: vertically, `src.height` rows down to `dst_h`.
    let ymap = axis_map(src.height, dst_h);
    for dy in 0..dst_h as usize {
        let s = ymap[dy];
        let r0 = &mid[s.lo as usize * mid_stride..];
        let r1 = &mid[s.hi as usize * mid_stride..];
        let out = &mut dst[dy * mid_stride..(dy + 1) * mid_stride];
        scale_y(r0, r1, s.weight, out, dst_w);
    }
    Ok(())
}

/// The horizontal pass: one source row to `dst_w` pixels. Scalar; see the module docs.
fn scale_x(row: &[u8], src_w: u32, out: &mut [u8], map: &[Sample]) {
    let stride = (src_w as usize * 4).min(row.len());
    let s = &row[..stride];
    // Every byte offset the loop can reach, so there is no per-pixel bounds check below. `hi` is
    // clamped in `axis_map` to `src_w - 1`, so the largest byte index is `src_w * 4 - 1`.
    if s.len() < 4 {
        return;
    }
    let last = s.len() - 4;
    for (dx, sample) in map.iter().enumerate() {
        let i = dx * 4;
        if i + 4 > out.len() {
            break;
        }
        let a = (sample.lo as usize * 4).min(last);
        let b = (sample.hi as usize * 4).min(last);
        let weight = sample.weight;
        if weight == 0 {
            // The common case by a wide margin: a large downscale is mostly `weight == 0`, and this
            // is a 4-byte copy rather than four multiplies.
            out[i..i + 4].copy_from_slice(&s[a..a + 4]);
            continue;
        }
        let lo_w = ONE - weight;
        for c in 0..4 {
            let v = (u32::from(s[a + c]) * lo_w + u32::from(s[b + c]) * weight) >> FRAC;
            out[i + c] = v as u8;
        }
    }
}

/// The vertical pass: two intermediate rows to one output row. SSE2 on x86_64.
///
/// # Why `u16` lanes cannot overflow
///
/// The weighted sum is `a * lo_w + b * hi_w` with `lo_w + hi_w == ONE == 256` and `a, b <= 255`, so
/// the result is at most `255 * 256 = 65_280`, which fits a `u16` with 255 to spare. That is what lets
/// the whole kernel be `mullo`/`add`/`srli` on 16-bit lanes with no 32-bit widening -- and it is the
/// reason `FRAC` is 8 and not 12, so this is a load-bearing constraint rather than an accident.
///
/// The scalar tail exists because a row is `width * 4` bytes and need not be a multiple of 16.
#[cfg(target_arch = "x86_64")]
fn scale_y(r0: &[u8], r1: &[u8], weight: u32, out: &mut [u8], width: u32) {
    // x86_64 has SSE2 unconditionally, so this is not a runtime feature test -- PRD FR-3.1 fixes the
    // instruction set at SSE2, and going above it would break the target contract. See the comment in
    // `holonomy_assets::blit` for the same argument.
    use core::arch::x86_64::{
        __m128i, _mm_add_epi16, _mm_loadu_si128, _mm_mullo_epi16, _mm_packus_epi16, _mm_set1_epi16,
        _mm_setzero_si128, _mm_srli_epi16, _mm_storeu_si128, _mm_unpackhi_epi8, _mm_unpacklo_epi8,
    };

    let n = (width as usize * 4)
        .min(out.len())
        .min(r0.len())
        .min(r1.len());
    if weight == 0 {
        out[..n].copy_from_slice(&r0[..n]);
        return;
    }
    let hi_w = weight as i16;
    let lo_w = (ONE - weight) as i16;

    // SAFETY: every `_mm_loadu_si128` below reads 16 bytes from a slice already bounds-checked to
    // have at least `i + 16 <= n` bytes, and each `_mm_storeu_si128` writes 16 bytes into `out`,
    // likewise checked. Unaligned loads and stores are used deliberately (`_mm_loadu`/`_mm_storeu`),
    // because `Vec<u8>` allocations are 1-byte aligned and a 16-byte-aligned variant would fault.
    unsafe {
        let v_lo = _mm_set1_epi16(lo_w);
        let v_hi = _mm_set1_epi16(hi_w);
        let zero = _mm_setzero_si128();
        let mut i = 0usize;
        while i + 16 <= n {
            let a = _mm_loadu_si128(r0.as_ptr().add(i).cast::<__m128i>());
            let b = _mm_loadu_si128(r1.as_ptr().add(i).cast::<__m128i>());
            // 16 bytes -> two vectors of 8 x u16, in the low and high halves.
            let a_lo = _mm_unpacklo_epi8(a, zero);
            let a_hi = _mm_unpackhi_epi8(a, zero);
            let b_lo = _mm_unpacklo_epi8(b, zero);
            let b_hi = _mm_unpackhi_epi8(b, zero);
            // mullo + add per half, then >> FRAC. See the overflow argument above.
            let s_lo = _mm_add_epi16(_mm_mullo_epi16(a_lo, v_lo), _mm_mullo_epi16(b_lo, v_hi));
            let s_hi = _mm_add_epi16(_mm_mullo_epi16(a_hi, v_lo), _mm_mullo_epi16(b_hi, v_hi));
            let s_lo = _mm_srli_epi16(s_lo, FRAC as i32);
            let s_hi = _mm_srli_epi16(s_hi, FRAC as i32);
            // `packus` *saturates* to u8 rather than wrapping, and a weighted average of two u8s
            // weighted to sum to ONE cannot exceed 255 -- so the saturation never fires, and it is
            // the right primitive anyway: it would clamp rather than wrap if the arithmetic were
            // ever changed.
            let packed = _mm_packus_epi16(s_lo, s_hi);
            _mm_storeu_si128(out.as_mut_ptr().add(i).cast::<__m128i>(), packed);
            i += 16;
        }
        // Scalar tail for the sub-16-byte remainder.
        while i < n {
            let a = u32::from(r0[i]);
            let b = u32::from(r1[i]);
            out[i] = ((a * u32::from(lo_w as u16) + b * u32::from(hi_w as u16)) >> FRAC) as u8;
            i += 1;
        }
    }
}

/// The vertical pass on targets without the SSE2 path: the same arithmetic, scalar.
///
/// Kept as a real function rather than `#[cfg]`-ing the caller, so the two paths are textually
/// comparable and the SSE2 one can be checked against it in a test.
#[cfg(not(target_arch = "x86_64"))]
fn scale_y(r0: &[u8], r1: &[u8], weight: u32, out: &mut [u8], width: u32) {
    let n = (width as usize * 4)
        .min(out.len())
        .min(r0.len())
        .min(r1.len());
    if weight == 0 {
        out[..n].copy_from_slice(&r0[..n]);
        return;
    }
    let hi_w = weight;
    let lo_w = ONE - hi_w;
    for i in 0..n {
        let a = u32::from(r0[i]);
        let b = u32::from(r1[i]);
        out[i] = ((a * lo_w + b * hi_w) >> FRAC) as u8;
    }
}
