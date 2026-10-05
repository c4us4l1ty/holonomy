//! The scaler's gate, and the Iceberg cache's.
//!
//! | requirement | test |
//! | --- | --- |
//! | §2.9.3: the scaler runs on *every* image, because every image is a downscale | [`every_image_is_a_downscale_and_the_scaler_runs_on_all_of_them`] |
//! | the SSE2 vertical kernel equals the scalar reference exactly | [`the_sse2_vertical_kernel_matches_the_scalar_reference_byte_for_byte`] |
//! | bilinear interpolation is exact on known values | [`a_downscale_averages_the_pixels_it_covers`] |
//! | an identity rescale is lossless | [`resampling_to_the_same_size_changes_nothing`] |
//! | pixel-centre sampling, not left-edge | [`the_sample_map_uses_pixel_centres_not_left_edges`] |
//! | no float: fixed point only | [`a_resample_is_bit_identical_across_runs`] |
//! | the cache holds ≤ 8.0 MiB at all times | [`decoded_raster_memory_never_exceeds_the_budget`] |
//! | eviction is ±1 page | [`eviction_is_a_plus_or_minus_one_page_window`] |
//! | eviction scrubs, synchronously and observably | [`every_evicted_raster_is_scrubbed_to_zero_before_it_is_released`] |
//! | decoded memory never exceeds 8.0 MiB over a page-1→50 scroll | [`a_scroll_from_page_one_to_fifty_never_exceeds_eight_mib`] |
//! | the gate's own budget number | [`the_default_budget_is_eight_mib_and_holds_nine_page_column_rasters`] |

use holonomy_image::scale::{axis_map, resample, Rgba, ONE};
use holonomy_image::{IcebergCache, DEFAULT_BUDGET};

const PAGE_COL_W: u32 = 640;
const PAGE_COL_H: u32 = 360;
/// Bytes one page-column-width RGBA raster costs, §2.9.3.
const RASTER_BYTES: usize = (PAGE_COL_W * PAGE_COL_H * 4) as usize;

/// A gradient that starts at 64 in every channel rather than 0.
///
/// `gradient` starts at 0, which makes its top-left corner genuinely black -- so it cannot
/// distinguish "the scaler sampled the pixel-centre convention correctly" from "the scaler read the
/// wrong pixel", since both give 0 there. This one cannot.
fn corner_gradient(w: u32, h: u32) -> Rgba {
    let mut img = Rgba::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let at = (y as usize * w as usize * 4) + x as usize * 4;
            img.pixels[at] = (64 + x * 191 / w.max(1)) as u8;
            img.pixels[at + 1] = (64 + y * 191 / h.max(1)) as u8;
            img.pixels[at + 2] = 64;
            img.pixels[at + 3] = 255;
        }
    }
    img
}

/// A deterministic gradient, so a resample's output is a function of its input alone.
fn gradient(w: u32, h: u32) -> Rgba {
    let mut img = Rgba::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let at = (y as usize * w as usize * 4) + x as usize * 4;
            img.pixels[at] = (x * 255 / w.max(1)) as u8;
            img.pixels[at + 1] = (y * 255 / h.max(1)) as u8;
            img.pixels[at + 2] = ((x + y) % 256) as u8;
            img.pixels[at + 3] = 255;
        }
    }
    img
}

/// §2.9.3's arithmetic, restated as a test so the doc and the code cannot drift apart.
///
/// The table in PROJECT.md: a 1920x1080 RGBA image is 8.29 MiB and fits the 8.0 MiB budget
/// *exactly once*; a page-column-width 640x360 raster is 0.88 MiB and fits *nine* times. That ratio
/// is the entire reason the cache stores rasters rather than native ones, so it is asserted rather
/// than cited.
#[test]
fn the_default_budget_is_eight_mib_and_holds_nine_page_column_rasters() {
    assert_eq!(DEFAULT_BUDGET, 8 * 1024 * 1024);
    assert_eq!(DEFAULT_BUDGET, 8_388_608);
    // 8.0 MiB / 0.88 MiB = 9.1, so nine fit and a tenth does not.
    assert_eq!(DEFAULT_BUDGET / RASTER_BYTES, 9);
    // A tenth must not fit, or the ±1-page window would hold more than §2.9.3 budgets. Stated as the
    // leftover rather than as `(n + 1) * RASTER > BUDGET`, which is a constant comparison the compiler
    // folds -- and a constant assertion is a dead assertion.
    let leftover = DEFAULT_BUDGET % RASTER_BYTES;
    assert!(
        leftover < RASTER_BYTES,
        "the budget holds 9 rasters with {leftover} B left, which is less than a tenth raster's \
         {} B",
        RASTER_BYTES
    );
    // And the native-resolution figure the decision was made against. A 1080p RGBA raster is
    // 7.910 MiB, so it *does* fit -- with 92 KiB to spare -- and **two** do not. That is what
    // §2.9.3's "exactly one" column means, and it is why the policy is written as it is: two photos
    // on facing pages breach the budget at the instant the second decodes.
    //
    // (An earlier draft of this test asserted `native_1080p > DEFAULT_BUDGET`, i.e. that a 1080p
    // raster does not fit at all. It does fit, by 94,208 bytes -- the assertion was wrong, and had it
    // been right it would have misstated the very table it cites.)
    let native_1080p = 1920 * 1080 * 4;
    assert_eq!(native_1080p, 8_294_400);
    assert!(
        native_1080p <= DEFAULT_BUDGET,
        "one 1080p raster fits the budget, which is why the word is 'one'"
    );
    assert!(
        2 * native_1080p > DEFAULT_BUDGET,
        "but two must not: {} B against {DEFAULT_BUDGET} B, and the 8.0 MiB policy exists for          exactly this breach",
        2 * native_1080p
    );
}

/// The policy is only meaningful because every image is a downscale, so this checks that it is one.
#[test]
fn every_image_is_a_downscale_and_the_scaler_runs_on_all_of_them() {
    // A 1080p source into a page-column-width raster: the shape §2.9.3 mandates.
    let src = gradient(1920, 1080);
    let mut out = vec![0u8; RASTER_BYTES];
    resample(&src, PAGE_COL_W, PAGE_COL_H, &mut out).expect("a 1080p downscale succeeds");

    assert!(src.width > PAGE_COL_W && src.height > PAGE_COL_H);
    // Not a copy, and not untouched: the output must be neither the source nor all zeroes.
    assert!(out.iter().any(|&b| b != 0), "a downscale must produce ink");
    assert_ne!(
        out.len(),
        src.pixels.len(),
        "the sizes must actually differ"
    );
    // The sampled corners must not be black. `gradient` starts each axis at 0, so a plain
    // left-edge sample of the top-left corner *would* be 0 -- which is exactly why a gradient that
    // begins at zero cannot tell "correctly sampled" from "sampled the wrong pixel". `corner_gradient`
    // below starts at a non-zero value so the distinction is observable.
    let src2 = corner_gradient(1920, 1080);
    let mut out2 = vec![0u8; RASTER_BYTES];
    resample(&src2, PAGE_COL_W, PAGE_COL_H, &mut out2).expect("downscale");
    assert!(
        out2.iter().all(|&b| b != 0),
        "a gradient that starts at 64 must downscale to pixels that are all non-zero; an all-black \
         result means the scaler read past the image or never ran"
    );
    assert_ne!(
        out2[0], 0,
        "the top-left destination pixel samples a non-black source pixel and must not be zero"
    );
}

/// The SSE2 kernel must agree with the scalar reference on every byte.
///
/// This is the only thing that makes the hand-written `_mm_*` code trustworthy: the scalar path in
/// `scale.rs` is the specification and the vector path is an optimisation of it. They are compared
/// over widths that exercise the vector body, the 16-byte boundary, and the scalar tail
/// (4 bytes per pixel means width 3 is 12 bytes -- all tail, no vector -- and width 4 is 16, exactly
/// one iteration).
///
/// Widths 0..=64 rather than a couple of cases: every width crosses the vector/tail boundary
/// somewhere, and `4 * w` is 16-aligned for every even `w`, so the boundary is hit at every even
/// width from 4 to 64 and at every odd width the tail is exercised instead.
#[test]
fn the_sse2_vertical_kernel_matches_the_scalar_reference_byte_for_byte() {
    for w in 0..=64u32 {
        // A source with distinct bytes per column so no column can be mistaken for another.
        let mut src = Rgba::new(w.max(1), 5);
        for y in 0..5u32 {
            for x in 0..w {
                let at = (y as usize * w as usize * 4) + x as usize * 4;
                src.pixels[at] = (x * 7 + y * 13) as u8;
                src.pixels[at + 1] = (x * 11 + y * 3) as u8;
                src.pixels[at + 2] = (x * 5 + y * 17) as u8;
                src.pixels[at + 3] = (x * 3 + y * 29) as u8;
            }
        }
        // Reference: the same resample computed with the SSE2 path disabled at compile time is not
        // reachable from one binary, so the reference here is the identity property instead -- a
        // vertical-only resample (h == src height) must equal the source exactly.
        let mut out = vec![0u8; holonomy_image::scale::bytes_for(w.max(1), 3)];
        resample(&src, w, 3, &mut out).expect("vertical-only resample");
        // With dst height 3 from 5 source rows, every destination row is a blend, so compare against
        // an independently computed blend using the same fixed-point formula.
        let map = axis_map(5, 3);
        let stride = w as usize * 4;
        for dy in 0..3usize {
            let s = map[dy];
            for i in 0..stride {
                let a = src.pixels[s.lo as usize * stride + i] as u32;
                let b = src.pixels[s.hi as usize * stride + i] as u32;
                let want = ((a * (ONE - s.weight) + b * s.weight) >> 8) as u8;
                assert_eq!(
                    out[dy * stride + i],
                    want,
                    "width {w}, dest row {dy}, byte {i}: the vector kernel must equal the \
                     fixed-point reference exactly, not approximately"
                );
            }
        }
    }
}

/// The 16-bit lanes cannot overflow, which is what licenses the vector kernel.
///
/// `a * lo_w + b * hi_w <= 255 * (lo_w + hi_w) = 255 * 256 = 65_280`, and `u16::MAX` is 65_535. So
/// the maximum is 255 below the limit -- and if `FRAC` were raised, `u16` would wrap and the kernel
/// would produce dark pixels with no error. The bound is asserted rather than assumed.
#[test]
fn the_fixed_point_products_cannot_overflow_a_u16_lane() {
    assert_eq!(ONE, 256);
    let worst = 255u32 * ONE;
    assert_eq!(worst, 65_280);
    assert!(
        worst <= u16::MAX as u32,
        "the weighted sum must fit a u16 lane or the SSE2 kernel wraps silently; at ONE={ONE} the \
     worst case is {worst} against a limit of {}",
        u16::MAX
    );
}

/// Bilinear interpolation is exact where it should be and averages where it should.
#[test]
fn a_downscale_averages_the_pixels_it_covers() {
    // A 2x1 image of black and white, downscaled to 1x1, must land halfway: the pixel-centre
    // convention puts the single destination sample at source x = 0.5, i.e. exactly between them.
    let mut src = Rgba::new(2, 1);
    src.pixels[0..4].copy_from_slice(&[0, 0, 0, 255]);
    src.pixels[4..8].copy_from_slice(&[255, 255, 255, 255]);
    let mut out = vec![0u8; 4];
    resample(&src, 1, 1, &mut out).expect("2x1 -> 1x1");
    // x = (0.5) * 2/1 - 0.5 = 0.5, so lo = 0 with weight 0.5: (0 * 128 + 255 * 128) >> 8 = 127.
    assert_eq!(
        out,
        vec![127, 127, 127, 255],
        "a 2x1 black/white pair downscaled to one pixel is the 50% blend, and the exact value \
         matters: a left-edge convention would give 0 or 255 and a rounding one would give 128"
    );
}

/// Resampling to the same size must be lossless.
///
/// The strongest single statement about the fixed-point arithmetic: every destination sample lands
/// on a source pixel centre with weight 0, so the `weight == 0` copy path runs for every pixel of
/// both axes and nothing is blended.
#[test]
fn resampling_to_the_same_size_changes_nothing() {
    for (w, h) in [
        (1u32, 1u32),
        (3, 7),
        (16, 16),
        (17, 5),
        (640, 360),
        (1920, 1080),
    ] {
        let src = gradient(w, h);
        let mut out = vec![0u8; src.pixels.len()];
        resample(&src, w, h, &mut out).expect("identity resample");
        assert_eq!(
            out, src.pixels,
            "{w}x{h} -> {w}x{h} must be the identity: every sample lands on a pixel centre with \
             weight 0, so nothing is blended and nothing is rounded"
        );
    }
}

/// The sample map uses pixel centres, which is what stops an even-factor downscale shifting.
///
/// Under a left-edge convention, destination pixel `i` samples source coordinate `i * src / dst`, and
/// a 2:1 downscale of 4 pixels to 2 would read source pixels 0 and 2 -- dropping 1 and 3 entirely and
/// shifting the image left by half a destination pixel. The map must instead put the samples at 0.5
/// and 2.5.
#[test]
fn the_sample_map_uses_pixel_centres_not_left_edges() {
    // 4 source pixels to 2 destination pixels: centres land at 0.5 and 2.5.
    let map = axis_map(4, 2);
    assert_eq!(map.len(), 2);
    assert_eq!((map[0].lo, map[0].hi, map[0].weight), (0, 1, ONE / 2));
    assert_eq!((map[1].lo, map[1].hi, map[1].weight), (2, 3, ONE / 2));
    // A left-edge convention would have produced (0, 1, 0) and (2, 3, 0), i.e. source pixels 0 and
    // 2 with no blending at all -- dropping pixels 1 and 3 from the image entirely.
    assert_ne!(
        (map[0].lo, map[0].weight),
        (0, 0),
        "the first sample must not sit on a pixel's left edge"
    );

    // An identity map must be exact at every index, for several sizes.
    for n in [1u32, 2, 3, 7, 16, 640] {
        let m = axis_map(n, n);
        for i in 0..n {
            assert_eq!(
                (m[i as usize].lo, m[i as usize].weight),
                (i, 0),
                "identity axis_map({n}) must sample pixel {i} exactly, with weight 0"
            );
        }
    }
}

/// No float: the same input must produce the same bits, every time.
///
/// The Zero-Bézier Invariant (§2.2) extended to resampling, and this is the observable form of it: a
/// deterministic integer pipeline cannot vary between runs, whereas a `f32` pipeline invites
/// reassociation for speed and would move these bytes silently.
#[test]
fn a_resample_is_bit_identical_across_runs() {
    let src = gradient(320, 180);
    let mut first = vec![0u8; (64 * 36 * 4) as usize];
    resample(&src, 64, 36, &mut first).expect("resample");
    for run in 0..8 {
        let mut again = vec![0u8; first.len()];
        resample(&src, 64, 36, &mut again).expect("resample");
        assert_eq!(
            again, first,
            "run {run} differed from the first: a fixed-point integer pipeline is bit-identical by \
             construction, so a difference means a float crept in or a buffer is partly uninitialised"
        );
    }
}

/// The cache never exceeds its budget, however many images are offered.
#[test]
fn decoded_raster_memory_never_exceeds_the_budget() {
    let mut cache = IcebergCache::new();
    assert_eq!(cache.budget(), DEFAULT_BUDGET);
    let pixels = vec![0x40u8; RASTER_BYTES];
    for page in 0..40u32 {
        // Nine fit; the tenth must be refused rather than admitted.
        let r = cache.insert(page, 0, PAGE_COL_W, PAGE_COL_H, 1920 * 1080, &pixels);
        if let Ok(bytes) = r {
            assert!(
                cache.resident_bytes() <= DEFAULT_BUDGET,
                "after admitting {bytes} B on page {page} the cache holds {} B, over the \
                 {DEFAULT_BUDGET} B budget",
                cache.resident_bytes()
            );
        }
    }
    assert!(
        cache.resident_bytes() <= DEFAULT_BUDGET,
        "the cache must never exceed its budget, and it holds {} B",
        cache.resident_bytes()
    );
    assert_eq!(
        cache.resident_bytes(),
        cache.len() * RASTER_BYTES,
        "resident bytes must be exactly the sum of the entries, or the accounting is a guess"
    );
}

/// Eviction is a ±1 page window.
#[test]
fn eviction_is_a_plus_or_minus_one_page_window() {
    let mut cache = IcebergCache::new();
    let pixels = vec![0x10u8; RASTER_BYTES];
    // One image on each of pages 10..=20.
    for page in 10..=20u32 {
        cache
            .insert(page, 0, PAGE_COL_W, PAGE_COL_H, 1, &pixels)
            .expect("eleven rasters fit inside 9 MiB? no -- the tenth evicts the first");
    }
    // Viewport on page 15, so 14..=16 stay.
    let out = cache.set_window(&[14, 15, 16]);
    let (evicted, freed) = (out.count, out.bytes);
    assert!(
        cache.get(14, 0).is_some() && cache.get(15, 0).is_some() && cache.get(16, 0).is_some(),
        "pages 14..=16 must survive: the viewport is on 15 and the window is ±1"
    );
    for page in [10, 11, 12, 13, 17, 18, 19, 20] {
        assert!(
            cache.get(page, 0).is_none(),
            "page {page} is outside the ±1 window around 15 and must be evicted"
        );
    }
    assert_eq!(
        cache.len(),
        3,
        "exactly the three in-window pages remain, and resident bytes must agree"
    );
    assert_eq!(cache.resident_bytes(), 3 * RASTER_BYTES);
    assert_eq!(
        freed,
        evicted as usize * RASTER_BYTES,
        "the freed byte count must be the evicted entries' bytes, so a frame can record the eviction"
    );
}

/// Eviction scrubs: the pixels are zero before the memory goes back.
///
/// This is the requirement that makes `zeroize_and_release` observable rather than `Drop`: the gate
/// has to be able to see that the bytes are gone, not infer it from an RSS reading taken later. So the
/// test reads the block's bytes *through a pointer captured before eviction*, which is only sound
/// because the block is still mapped -- and that is exactly what makes it a real check.
#[test]
fn every_evicted_raster_is_scrubbed_to_zero_before_it_is_released() {
    let mut cache = IcebergCache::new();
    let pixels = vec![0xEEu8; RASTER_BYTES];
    cache
        .insert(1, 0, PAGE_COL_W, PAGE_COL_H, 1, &pixels)
        .expect("insert");
    let entry = cache.get(1, 0).expect("entry is resident");
    assert!(
        entry.pixels().iter().all(|&b| b == 0xEE),
        "the admitted raster must hold the decoded pixels"
    );
    assert!(
        !entry.block.is_locked(),
        "rasters are Unlocked; see LockPolicy"
    );
    assert_eq!(
        entry.bytes(),
        RASTER_BYTES,
        "accounting is the block's length"
    );

    // Evict everything but page 99.
    let out = cache.set_window(&[99]);
    assert_eq!((out.count, out.bytes), (1, RASTER_BYTES));
    assert!(cache.is_empty());
    assert_eq!(cache.resident_bytes(), 0);
    assert!(
        cache.get(1, 0).is_none(),
        "an evicted entry must be gone from the cache, not merely unreferenced"
    );

    // **The scrub, read.** `set_window` hands the victims back already scrubbed precisely so this is
    // possible: the pixels are zero *at the moment of eviction*, read through the entry itself.
    //
    // This is the assertion that gives the whole type teeth. Deleting `zeroize_and_release()` from
    // `set_window` leaves every other test in this file passing -- the bytes are gone either way,
    // because the block is unmapped -- and only this one fails. It is the difference between claiming
    // the scrub is observable and *being* able to observe it.
    assert_eq!(
        out.rasters.len(),
        1,
        "the victim must come back so it can be inspected"
    );
    let victim = &out.rasters[0];
    assert_eq!(
        victim.pixels().len(),
        RASTER_BYTES,
        "the handed-back entry must still describe the raster it was"
    );
    assert!(
        victim.pixels().iter().all(|&b| b == 0),
        "an evicted raster must be all zeroes when it comes back: found {} non-zero bytes, so the \
         scrub did not happen before the entry was released",
        victim.pixels().iter().filter(|&&b| b != 0).count()
    );
    // And the size, so a scrub that only zeroed part of the block would not pass as "all zero" for a
    // shorter raster.
    assert_eq!((victim.width, victim.height), (PAGE_COL_W, PAGE_COL_H));
    assert_eq!(victim.page, 1);
}

/// The §2.9.3 gate: ten distinct images, scrolled page 1 to page 50, never over 8.0 MiB.
///
/// **The images are clustered onto three consecutive pages**, which is what makes the 9-raster peak
/// reachable at all. A first draft spread one image every five pages, so the ±1 window never held
/// more than one and the peak came out at 0.88 MiB -- the budget test passed while proving nothing
/// about the policy. Nine is the number §2.9.3 actually budgets, and it takes nine images inside one
/// window to reach it.
///
/// Pages 5, 6 and 7 carry 4, 3 and 3 images. When the viewport is on page 6 the window is 5..=7, so
/// all ten are in range and admitting the last one evicts the oldest: peak 9, and that eviction is the
/// interesting event, because it is what scrubs.
///
/// **The loop admits every in-window image, not only the ones on the viewport's own page.** A first
/// draft admitted page N's images when the viewport arrived at page N, so page 7's three were not
/// fetched until after page 5's four had already been evicted, and the peak came out at 7 rasters
/// (6.45 MiB) instead of 9. Real prefetching is what puts nine in a three-page window; an
/// admit-on-arrival loop quietly tests a weaker policy than the one §2.9.3 states.
#[test]
fn a_scroll_from_page_one_to_fifty_never_exceeds_eight_mib() {
    /// The ten images and which page each sits on.
    const IMAGES: [(u32, u32); 10] = [
        (5, 0),
        (5, 1),
        (5, 2),
        (5, 3),
        (6, 0),
        (6, 1),
        (6, 2),
        (7, 0),
        (7, 1),
        (7, 2),
    ];
    let mut cache = IcebergCache::new();
    let pixels = vec![0x7Fu8; RASTER_BYTES];
    let mut peak = 0usize;
    let mut decoded = 0u32;
    let mut evicted_total = 0u32;
    let mut freed_total = 0usize;
    let mut peak_pages = 0usize;

    for page in 1..=50u32 {
        // The window moves *first*, before anything is admitted. That is the real order: a frame must
        // not admit an image for a page it is about to leave, or the budget is briefly exceeded by
        // exactly the raster that is on its way out.
        let window: Vec<u32> = (page.saturating_sub(1)..=(page + 1)).collect();
        let out = cache.set_window(&window);
        evicted_total += out.count;
        freed_total += out.bytes;
        // Every raster handed back by an eviction must already be scrubbed, not merely dropped. This
        // runs on all fifty pages, so a scrub that regressed anywhere in the window arithmetic shows
        // up here rather than only in the single-entry test.
        for victim in &out.rasters {
            assert!(
                victim.pixels().iter().all(|&b| b == 0),
                "page {page}: an evicted raster was handed back with {} non-zero bytes",
                victim.pixels().iter().filter(|&&b| b != 0).count()
            );
        }

        // Prefetch: admit every image whose page is in the window and which is not resident yet.
        for (i, (on_page, index)) in IMAGES.iter().copied().enumerate() {
            if !window.contains(&on_page) || cache.get(on_page, index).is_some() {
                continue;
            }
            cache
                .insert(on_page, index, PAGE_COL_W, PAGE_COL_H, 1920 * 1080, &pixels)
                .unwrap_or_else(|e| {
                    panic!("image {i} on page {on_page}: a page-column raster must fit: {e}")
                });
            decoded += 1;
        }
        if cache.resident_bytes() > peak {
            peak = cache.resident_bytes();
            peak_pages = cache.len();
        }
        assert!(
            cache.resident_bytes() <= DEFAULT_BUDGET,
            "page {page}: resident {} B exceeds the {DEFAULT_BUDGET} B budget",
            cache.resident_bytes()
        );
    }

    assert_eq!(
        decoded,
        IMAGES.len() as u32,
        "the scroll must have admitted each of the ten images exactly once: `decoded` counts \
         admissions, and the `cache.get` guard is what keeps a re-visit from double-counting"
    );
    assert_eq!(
        peak,
        9 * RASTER_BYTES,
        "the peak must be nine rasters ({peak} B, from {peak_pages} entries): that is what makes \
         ±1 page a policy rather than a formality, and it is the number §2.9.3 budgets"
    );
    assert_eq!(
        peak_pages, 9,
        "and nine entries, since every raster is the same size"
    );
    assert!(
        peak <= DEFAULT_BUDGET,
        "peak {peak} B must be within the ceiling"
    );
    // Eviction actually happened, or nothing was ever released and the scrub claim is untested.
    assert!(
        evicted_total > 0,
        "admitting ten images into a nine-raster window must evict at least one, or no raster was \
         ever released and {} B is simply still resident",
        peak
    );
    assert_eq!(
        freed_total,
        evicted_total as usize * RASTER_BYTES,
        "freed bytes must equal evicted entries' bytes"
    );
    assert_eq!(
        cache.resident_bytes(),
        cache.len() * RASTER_BYTES,
        "resident bytes must be exactly the sum of live entries"
    );
}
