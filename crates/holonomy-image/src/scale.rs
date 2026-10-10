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
//! # Two filters, because one of them is a decimator at the product's own ratio
//!
//! Everything below describes a **bilinear** scaler, and it was the only one. §2.9.3 makes every image a
//! downscale to page-column width and the product's own arithmetic is **1920 -> 640: exactly 3:1**. Under
//! the pixel-centre convention of [`axis_map`], that places destination pixel `i` at source `3i + 1` --
//! an exact integer, so **every interpolation weight is zero and the filter reads one pixel in three and
//! discards the rest.** At 6.86:1 it reads two of every seven. A hard edge in the source therefore
//! produces no intermediate value at all.
//!
//! **That is correct behaviour for a bilinear filter at an exact integer ratio, and `axis_map` is not
//! wrong.** The defect is the *choice* of filter: a decimator is the wrong answer for a reduction that
//! covers nine source pixels per output pixel. So [`axis_area_map`] and its two passes were added, and
//! [`use_area`] picks between them per axis at [`AREA_THRESHOLD`] = 2:1.
//!
//! **Below the threshold the byte-for-byte behaviour is unchanged** -- same functions, same fixed point,
//! same SSE2 kernel -- so magnification and mild reduction are untouched and the existing gates for them
//! still mean what they meant. **`crates/holonomy-image/tests/area_filter.rs` is the gate for the new
//! half**, and the two tests in `scale_cache.rs` that used to pin the decimation were rewritten rather
//! than deleted: `axis_map` still has to decimate at 3:1, and that is still pinned, because the filter
//! that no longer uses it is exactly the kind of thing that should stay pinned.
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

/// One destination pixel's **footprint** in the source: a contiguous run of source pixels.
///
/// The area filter's counterpart to [`Sample`]. Where a bilinear [`Sample`] names two pixels and a
/// weight, an [`AreaSample`] names every pixel the destination pixel covers, with equal weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AreaSample {
    /// First source pixel in the run.
    pub start: u32,
    /// How many source pixels the run holds. **Never zero.**
    pub len: u32,
}

/// The area map for one axis: `dst` destination pixels against `src` source pixels.
///
/// # Why this exists, and what was wrong without it
///
/// §2.9.3 makes **every** image in the product a downscale, and the product's own arithmetic is
/// **1920 -> 640: exactly 3:1**. Under [`axis_map`]'s pixel-centre convention that places destination
/// pixel `i` at source `3i + 1` — an exact integer, so **every interpolation weight is zero and the
/// bilinear filter is a decimator**. Two of every three source pixels are discarded without
/// contributing, and a hard edge in the source produces no intermediate value at all. PROJECT.md §7
/// item 3 records this as *the filter choice being wrong*, not `axis_map`: `axis_map` is correct, its
/// convention is right, and a bilinear filter at an exact integer ratio *should* decimate. What is
/// wrong is that a decimator is the filter for a 9:1 reduction in covered area.
///
/// **The fix is to average every pixel the destination covers**, which is the area (box) filter and is
/// what a photograph actually wants at 3:1.
///
/// # The footprint rule
///
/// Destination pixel `i` covers the source pixels whose **centres** fall inside its own extent, which is
/// `[i * src/dst, (i+1) * src/dst)`. So `start = floor(i * src / dst)` and
/// `end = floor((i+1) * src / dst)`.
///
/// **Two properties this rule is chosen for.** At an exact integer ratio `r`, every footprint is exactly
/// `r` pixels long — so 3:1 averages three and 7:1 averages seven, rather than reading one and seven.
/// And the footprints are **contiguous and non-overlapping**, so the ranges are monotonic and a caller
/// can walk the source once with a running sum instead of re-reading it per destination pixel.
///
/// **`len` is never zero**, which is the one place this departs from the arithmetic: when `src < dst`, some
/// destination pixels map to an empty half-open interval, and a zero-length run has no average. Those
/// get one pixel, sampled at the clamped centre — which is exactly what [`axis_map`] gives at the same
/// place, so **magnification is bit-identical to before** rather than merely similar.
pub fn axis_area_map(src: u32, dst: u32) -> Vec<AreaSample> {
    let mut out = Vec::with_capacity(dst as usize);
    if src == 0 || dst == 0 {
        return out;
    }
    for i in 0..dst {
        // u64 throughout: `i * src` reaches 640 * 1920 in normal use and `dst * src` can exceed u32 on
        // a large magnification, and the multiply happens before the divide.
        let lo = (u64::from(i) * u64::from(src) / u64::from(dst)) as u32;
        let hi = ((u64::from(i) + 1) * u64::from(src) / u64::from(dst)) as u32;
        let start = lo.min(src - 1);
        let end = hi.min(src);
        let len = end.saturating_sub(start);
        out.push(AreaSample {
            start,
            // The one deviation from the arithmetic, and `axis_map`'s own answer for the same pixel.
            len: len.max(1),
        });
    }
    out
}

/// The reduction at which the area filter takes over from bilinear.
///
/// **2:1, and the threshold is a property of the filter rather than a tuning knob.** A bilinear sample
/// spans exactly two source pixels, so at a ratio below 2 those two pixels *do* cover the destination
/// pixel's footprint and the interpolation is a legitimate area estimate — that is what bilinear is for.
/// At 2:1 and above it does not: the footprint is two pixels or more wide and the filter is reading a
/// subset of it, which is the defect. **Below the threshold the code path is byte-for-byte the one that
/// was there before**, so magnification and mild reduction are untouched.
pub const AREA_THRESHOLD: u32 = 2;

/// Whether `src -> dst` is a large enough reduction for the area filter to be the right answer.
///
/// Checked with `>=` against twice the destination, which is `src/dst >= 2` without a division and
/// without an `f32`.
#[inline]
pub fn use_area(src: u32, dst: u32) -> bool {
    dst != 0 && src >= AREA_THRESHOLD * dst
}

/// Resample `src` to exactly `dst_w x dst_h`, writing RGBA into `dst`.
///
/// **Bilinear or area-average per axis, chosen by [`use_area`]** -- see [`AREA_THRESHOLD`]. At the
/// product's own 3:1 this is an area average; below 2:1 it is the bilinear this has always been.
///
/// `dst` must be at least [`bytes_for`] long; the length is checked, not trusted.
///
/// See the module docs for why this is two passes and why only the second is vectorised.
pub fn resample(src: &Rgba, dst_w: u32, dst_h: u32, dst: &mut [u8]) -> Result<()> {
    resample_pixels(src.width, src.height, &src.pixels, dst_w, dst_h, dst)
}

/// Resample `src_w x src_h` RGBA from `src` into `dst`, borrowing the source.
///
/// # Why this exists beside [`resample`]
///
/// [`Rgba`] owns its pixels, so a caller that decodes into a reused buffer has to wrap the borrow in
/// an `Rgba` -- and cannot, because the constructor allocates. The session is exactly that caller: it
/// decodes a 1920x1080 source into an 8.3 MB scratch it reuses, and wrapping that borrow in an owned
/// `Rgba` would either copy all 8.3 MB per image or mean keeping two buffers alive for no reason.
///
/// So the dimensions are parameters and the pixels are a slice, and [`resample`] is the owned-pixels
/// convenience over it. One implementation, two entry points, no duplicated arithmetic.
pub fn resample_pixels(
    src_w: u32,
    src_h: u32,
    src: &[u8],
    dst_w: u32,
    dst_h: u32,
    dst: &mut [u8],
) -> Result<()> {
    let want = bytes_for(dst_w, dst_h);
    if dst.len() < want {
        return Err(PngError::DestinationTooSmall {
            want,
            have: dst.len(),
        });
    }
    if src_w == 0 || src_h == 0 {
        // The decoder refuses a zero-sized image at `IHDR`, so this is only reachable by
        // constructing an `Rgba` by hand or by calling this function with zero dimensions.
        return Err(PngError::ZeroDimension);
    }
    // A source shorter than its own dimensions is a *caller* error -- a half-filled buffer would
    // otherwise read past the end in the horizontal pass. `bytes_for` is the same arithmetic
    // `Rgba::new` uses, so an `Rgba` can never trip this and only a hand-built call can.
    let have = bytes_for(src_w, src_h);
    if src.len() < have {
        return Err(PngError::SourceTooSmall {
            want: have,
            have: src.len(),
        });
    }
    // A zero destination is not an error: it is the degenerate "show nothing", and returning an empty
    // image for it is more useful than refusing.
    if dst_w == 0 || dst_h == 0 {
        return Ok(());
    }

    // Pass 1: horizontally, every source row down to `dst_w`.
    //
    // **The two passes pick their filter independently, and that is separability rather than an
    // oversight.** A separable filter's whole point is that the 2D result is the composition of a
    // horizontal and a vertical 1D filter, so a wide-but-short image -- 1920x1080 to 640x1080 -- gets
    // an area average across the width and *bilinear* down the height, which is right: only one axis
    // reduced, and interpolation is the correct filter for an axis that did not. Forcing both to agree
    // would apply a downscale filter where none is needed.
    let src_stride = src_w as usize * 4;
    let mid_stride = dst_w as usize * 4;
    let mut mid = vec![0u8; mid_stride * src_h as usize];
    if use_area(src_w, dst_w) {
        let xmap = axis_area_map(src_w, dst_w);
        for y in 0..src_h as usize {
            let row = &src[y * src_stride..(y + 1) * src_stride];
            let out = &mut mid[y * mid_stride..(y + 1) * mid_stride];
            scale_x_area(row, out, &xmap);
        }
    } else {
        let xmap = axis_map(src_w, dst_w);
        for y in 0..src_h as usize {
            let row = &src[y * src_stride..(y + 1) * src_stride];
            let out = &mut mid[y * mid_stride..(y + 1) * mid_stride];
            scale_x(row, src_w, out, &xmap);
        }
    }

    // Pass 2: vertically, `src.height` rows down to `dst_h`.
    //
    // **The accumulator is hoisted out of the loop**, because the kernel needs `dst_w * 4` `u32`s of
    // running sum per output row and there is no reason to ask for them 360 times for a 1080p source.
    // `Vec::clear` is used rather than reallocating, so the whole area path allocates exactly once --
    // the `mid` buffer that both passes already required -- and the numbers are allocated *here*, at
    // the point where the width is known, rather than inside a kernel that is handed a slice.
    if use_area(src_h, dst_h) {
        let ymap = axis_area_map(src_h, dst_h);
        let mut acc: Vec<u32> = Vec::new();
        for dy in 0..dst_h as usize {
            let s = ymap[dy];
            let out = &mut dst[dy * mid_stride..(dy + 1) * mid_stride];
            acc.clear();
            acc.resize(dst_w as usize * 4, 0);
            scale_y_area(&mid, mid_stride, s, out, &mut acc);
        }
    } else {
        let ymap = axis_map(src_h, dst_h);
        for dy in 0..dst_h as usize {
            let s = ymap[dy];
            let r0 = &mid[s.lo as usize * mid_stride..];
            let r1 = &mid[s.hi as usize * mid_stride..];
            let out = &mut dst[dy * mid_stride..(dy + 1) * mid_stride];
            scale_y(r0, r1, s.weight, out, dst_w);
        }
    }
    Ok(())
}

/// The horizontal area pass: average each destination pixel's footprint from one source row.
///
/// Scalar, and **not** SSE2, unlike [`scale_y`]. Two reasons, and they are different in kind.
///
/// The arithmetic is a division by a run length that varies per destination pixel, and SSE2 has no
/// variable-divide. Below the threshold this path is never taken, and above it the output is small —
/// 640 pixels for the product's 1920-wide source — so the division count is 691k over a whole 1920x1080
/// image, which is a few milliseconds against a PNG decode that costs orders of magnitude more.
///
/// # The division, and why it is not a rounding-off
///
/// `(sum + len/2) / len` is round-half-up on an unsigned integer, which is the rounding every
/// image API uses and — importantly — is **not** the same as truncating. Truncation biases every output
/// one step dark, and on a gradient that reads as a band running down the image, because the error is
/// systematic rather than random. The bias is bounded by half a code value per channel and it is the
/// reason this is not a bare `/ len`.
fn scale_x_area(row: &[u8], out: &mut [u8], map: &[AreaSample]) {
    let s = row;
    let last = s.len().checked_sub(4);
    let Some(last) = last else { return };
    for (dx, sample) in map.iter().enumerate() {
        let i = dx * 4;
        if i + 4 > out.len() {
            break;
        }
        let start = (sample.start as usize).min(last / 4);
        let len = (sample.len as usize).max(1).min(s.len() / 4 - start);
        let mut acc = [0u32; 4];
        for px in start..start + len {
            let at = px * 4;
            for c in 0..4 {
                acc[c] += u32::from(s[at + c]);
            }
        }
        let half = (len as u32) / 2;
        for c in 0..4 {
            out[i + c] = ((acc[c] + half) / len as u32) as u8;
        }
    }
}

/// The vertical area pass: average each destination row's footprint from the intermediate.
///
/// **Strided, and that is why it is one function rather than a row-pair kernel.** The footprint is a
/// *run* of rows, not two rows, so there is no pair to load — and because the runs are contiguous and
/// monotonic, this walks the intermediate top to bottom once per destination row instead of
/// re-reading it. The total work is `O(src_h)` per output column set, the same order as the SSE2 kernel
/// it replaces.
fn scale_y_area(
    mid: &[u8],
    mid_stride: usize,
    sample: AreaSample,
    out: &mut [u8],
    acc: &mut [u32],
) {
    let n = (out.len()).min(acc.len());
    if n == 0 || mid.is_empty() {
        return;
    }
    let rows = mid.len() / mid_stride.max(1);
    let start = (sample.start as usize).min(rows.saturating_sub(1));
    let len = (sample.len as usize).max(1).min(rows - start);
    for row in start..start + len {
        let base = row * mid_stride;
        let line = &mid[base..(base + n).min(mid.len())];
        for (a, b) in acc.iter_mut().zip(line) {
            *a += u32::from(*b);
        }
    }
    let half = (len as u32) / 2;
    for (i, a) in acc.iter().enumerate().take(n) {
        out[i] = ((*a + half) / len as u32) as u8;
    }
    // **Zeroed on the way out.** The accumulator holds plaintext pixel values summed from the decoded
    // image, and it is reused for the next row rather than dropped -- so the last row's sums would
    // otherwise still be in it when this buffer is freed, which is the same exposure the session's
    // scratch buffers are scrubbed for.
    for a in acc.iter_mut().take(n) {
        *a = 0;
    }
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
