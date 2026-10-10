//! **The area filter: the large-reduction path is an average, not a decimator.** 6 tests.
//!
//! # What PROJECT.md asked for, verbatim
//!
//! §7 item 3 recorded the defect and the fix and then said why the fix was deferred:
//!
//! > `axis_map` is correct and its pixel-centre convention is pinned; **the filter choice for large
//! > reductions is what is wrong, and an area average is the fix.** […] it is a rewrite of a
//! > mutation-verified module and **wants its own gate**.
//!
//! This is that gate. The two tests in `scale_cache.rs` that pinned the decimation were **rewritten,
//! not deleted** — `axis_map` still has to decimate at 3:1 and that is still pinned there — and this
//! file covers the half that is new.
//!
//! # The defect in one sentence
//!
//! §2.9.3 makes **every** image in the product a downscale to page-column width, and the product's own
//! arithmetic is **1920 → 640: exactly 3:1**. Under the pixel-centre convention, destination pixel `i`
//! samples source `3i + 1` — an exact integer, so **every interpolation weight is zero and bilinear
//! reads one pixel in three and discards the other two.** At 6.86:1 it reads two of every seven. A hard
//! edge in the source therefore produces **no intermediate value at all**.
//!
//! # What is asserted, and why each is not the same claim
//!
//! | what it proves | test |
//! | --- | --- |
//! | at an exact ratio every covered pixel contributes | [`an_exact_integer_ratio_covers_every_source_pixel`] |
//! | **and a hard edge comes out intermediate** | [`a_hard_edge_survives_a_reduction_as_an_intermediate_value`] |
//! | the two axes are independent filters | [`the_two_axes_pick_their_filters_independently`] |
//! | below the threshold nothing moved | [`below_the_threshold_the_bytes_are_exactly_what_they_were`] |
//! | the footprints tile without gaps or overlap | [`the_footprints_tile_the_source_without_gaps_or_overlap`] |
//! | and a long run of 255s does not overflow | [`a_long_footprint_does_not_overflow_the_accumulator`] |

use holonomy_image::scale::{
    axis_area_map, axis_map, resample, use_area, AreaSample, Rgba, AREA_THRESHOLD,
};

/// A solid image of one value, so a mean is trivially checkable.
fn solid(w: u32, h: u32, v: u8) -> Rgba {
    let mut img = Rgba::new(w, h);
    for px in img.pixels.chunks_exact_mut(4) {
        px.copy_from_slice(&[v, v, v, 255]);
    }
    img
}

/// A left/right split at `at_x`, with a distinct G and B so a wrong channel is visible.
///
/// # Why `at_x` is a parameter, and this is the subtlety the whole file turns on
///
/// **An area filter only produces greys where the source actually varies across a destination pixel's
/// footprint.** Put the edge exactly on a footprint boundary and the footprint is constant -- correctly,
/// correctly giving 0 or 255 -- and the test then passes against a *decimator* as readily as against the
/// filter. So every edge in this file is placed **strictly inside** a footprint.
///
/// Concretely: at 96 -> 14 the footprints are `[floor(48i/7), floor(48(i+1)/7))`, and `48 = 7 * 48/7`
/// is a boundary -- an edge at `x = 48` straddles nothing and gives a binary image. At `x = 50` the
/// footprint `[48, 54)` holds two black and four white pixels and its mean is `1020/6 = 170`.
///
/// **The first version of the hard-edge test below got this wrong and produced a completely binary
/// image from an area filter**, which is the failure mode this note exists to prevent.
fn edge_at(w: u32, h: u32, at_x: u32) -> Rgba {
    let mut img = Rgba::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let at = (y as usize * w as usize + x as usize) * 4;
            let on = x >= at_x;
            img.pixels[at] = if on { 255 } else { 0 };
            img.pixels[at + 1] = 128;
            img.pixels[at + 2] = if on { 64 } else { 32 };
            img.pixels[at + 3] = 255;
        }
    }
    img
}

/// The red channel of destination pixel `x` in row 0.
fn red(dst: &[u8], x: usize) -> u8 {
    dst[x * 4]
}

/// **At an exact ratio every covered source pixel contributes.** The whole claim, on the map.
///
/// At `src/dst == r` each destination pixel's footprint is exactly `r` source pixels, so a 3:1
/// reduction averages three and a 7:1 averages seven. `axis_map` covers **one** at 3:1 and **two** at
/// 7:1, which is the defect stated as a number.
///
/// The second assertion is the one that would catch a regression to nearest-neighbour even if the map
/// were right: it computes the mean of the actual source pixels for one destination pixel and requires
/// the resampled image to contain it.
#[test]
fn an_exact_integer_ratio_covers_every_source_pixel() {
    for r in [2u32, 3, 4, 7] {
        let src = r * 8;
        let dst = 8;
        let map = axis_area_map(src, dst);
        assert_eq!(map.len(), dst as usize);
        assert!(
            map.iter().all(|a| a.len == r),
            "at src/dst == {r} every footprint must be exactly {r} source pixels. Got {:?}",
            map.iter().map(|a| a.len).collect::<Vec<_>>()
        );
        assert!(
            use_area(src, dst),
            "and {r}:1 must be handed to the area filter"
        );
    }

    // **The pixels, not just the map.** One white column among black, at 7:1 -- far enough that a
    // two-tap filter is very likely to miss it entirely.
    let mut img = Rgba::new(56, 4);
    for x in 0..56usize {
        let v: u8 = if x == 28 { 255 } else { 0 };
        for y in 0..4usize {
            let at = (y * 56 + x) * 4;
            img.pixels[at] = v;
            img.pixels[at + 3] = 255;
        }
    }
    let mut out = vec![0u8; holonomy_image::scale::bytes_for(8, 4)];
    resample(&img, 8, 4, &mut out).expect("resample 56 -> 8");

    // Column 4 covers source 28..35, which contains the white pixel at 28 and nothing else that is lit.
    // Its mean is 255/7 = 36.4 -> (255 + 3) / 7 = 36.
    assert_eq!(
        red(&out, 4),
        36,
        "a single lit source pixel in a 7:1 reduction must contribute 1/7 of its value. A two-tap \\
         filter would return 0 here -- it reads two of the seven and neither is lit."
    );
    // And every other column is untouched, so the contribution did not spread.
    for x in [0usize, 1, 2, 3, 5, 6, 7] {
        assert_eq!(
            red(&out, x),
            0,
            "column {x} covers no lit source pixel and must be black"
        );
    }
}

/// **A hard edge survives a reduction as an intermediate value.**
///
/// This is the observable consequence, and it is the assertion a user would recognise: decimation turns
/// a step edge into a step edge, at whatever phase it happens to land on, and no amount of filtering
/// downstream can put the grey back. An area average produces the greys.
#[test]
fn a_hard_edge_survives_a_reduction_as_an_intermediate_value() {
    let mut out = vec![0u8; holonomy_image::scale::bytes_for(14, 4)];
    resample(&edge_at(96, 4, 50), 14, 4, &mut out).expect("resample 96 -> 14");

    // **Footprint 7 is `[48, 54)` -- six pixels, not seven**, because the window is half-open and
    // `floor(8 * 96 / 14)` is `floor(54.857)` = 54. Two of the six are black and four are white:
    //
    //     (2 * 0 + 4 * 255 + 3) / 6 = 1023 / 6 = 170
    //
    // **The first draft of this assertion said 182, from `floor(8*96/14)` rounded up to 55.** The
    // filter was right and the arithmetic in the comment was not, which is the ordinary way a
    // hand-computed expectation goes wrong -- and the reason the per-column loop below exists.
    assert_eq!(
        red(&out, 7),
        170,
        "footprint [48, 54) holds two black and four white pixels, so its mean is 170. A two-tap \
         filter reads two of the six and returns 0 or 255."
    );

    // **Every column is the mean of its own footprint**, which is the general claim. Recomputing all
    // 14 from the source is cheap and catches a filter that is right for one phase and wrong for
    // another -- which is exactly what a decimator looks like, since a decimator is right at every
    // boundary-aligned phase and wrong at every other one.
    for x in 0..14usize {
        let lo = (x as u64 * 96 / 14) as u32;
        let hi = ((x as u64 + 1) * 96 / 14) as u32;
        let sum: u32 = (lo..hi).map(|sx| if sx < 50 { 0u32 } else { 255 }).sum();
        let len = hi - lo;
        let want = ((sum + len / 2) / len) as u8;
        assert_eq!(
            red(&out, x),
            want,
            "column {x} covers source [{lo}, {hi}) and must be its mean. Got {}, want {want}.",
            red(&out, x)
        );
    }
}

/// **The two axes choose independently, because a separable filter is a composition.**
///
/// A wide-but-short image — 1920×1080 to 640×1080 — reduces on one axis and not the other. Forcing the
/// two passes to agree would apply a downscale filter to an axis that did not reduce, which is the
/// mirror image of the original defect: using the wrong filter in the *other* direction.
///
/// The assertion is on the *pixels*: reducing only the width at a 3:1 ratio averages across three
/// columns while the height is copied, and a 3:1 left/right edge shows up as exactly that.
#[test]
fn the_two_axes_pick_their_filters_independently() {
    assert!(use_area(1920, 640), "the width reduces at 3:1");
    assert!(!use_area(1080, 1080), "and the height does not reduce at all");

    // **The edge is at x = 50, inside footprint 16** ([48, 51)), for the reason `edge_at` gives.
    let img = edge_at(96, 8, 50);
    let mut out = vec![0u8; holonomy_image::scale::bytes_for(32, 8)];
    resample(&img, 32, 8, &mut out).expect("resample 96x8 -> 32x8");
    // Only the width reduced, so the vertical pass is a pure copy: all 8 output rows of a column are
    // equal. If the height had taken the area path too, a 1:1 footprint is length 1 and this would
    // still hold -- which is precisely why the width assertion below is the one that matters.
    for y in 1..8usize {
        assert_eq!(
            &out[y * 32 * 4..(y + 1) * 32 * 4],
            &out[0..32 * 4],
            "row {y} differs from row 0, so the 1:1 vertical axis is not a copy"
        );
    }
    assert!(
        (0..32).any(|x| red(&out, x) > 20 && red(&out, x) < 235),
        "the 3:1 horizontal axis averaged the edge, as it must"
    );
}

/// **Below the threshold the bytes are exactly what they were.**
///
/// `AREA_THRESHOLD` is 2, and the claim is not "mild reduction is approximately unchanged" — it is that
/// **the same functions run**: `axis_map`, `scale_x` and the SSE2 `scale_y`. At 1.5:1 the two taps a
/// destination pixel reads *do* span its footprint, so bilinear is the right filter and there is nothing
/// to gain by replacing it. Pinned on exact values so a threshold that creeps to 1 would fail here.
#[test]
fn below_the_threshold_the_bytes_are_exactly_what_they_were() {
    // 3 -> 2 is 1.5:1. pos(0) = 0.25 -> (lo=0, w=64); pos(1) = 1.75 -> (lo=1, w=192).
    assert!(!use_area(3, 2));
    assert_eq!(AREA_THRESHOLD, 2, "the threshold is a design number, not a tuning knob");
    let m = axis_map(3, 2);
    assert_eq!(m[0], holonomy_image::scale::Sample { lo: 0, hi: 1, weight: 64 });
    assert_eq!(m[1], holonomy_image::scale::Sample { lo: 1, hi: 2, weight: 192 });

    // **And the area map is not consulted**, which is what "unchanged" means operationally. It *would*
    // give `[{0, 1}, {1, 2}]` -- two pixels for the second destination -- while bilinear gives the same
    // two pixels at a 3:1 weight rather than half-and-half. **Neither is wrong at 1.5:1**: the footprint
    // is under two pixels wide, so two taps do cover it, and that is exactly why the threshold is where
    // it is. Below 2:1 the area filter buys nothing, so it does not run.
    let a = axis_area_map(3, 2);
    assert_eq!(a[0], AreaSample { start: 0, len: 1 });
    assert_eq!(a[1], AreaSample { start: 1, len: 2 });

    // A 3x1 ramp 0/100/200 to 2x1: the exact truncating fixed-point values.
    let mut img = Rgba::new(3, 1);
    for (x, v) in [0u8, 100, 200].into_iter().enumerate() {
        img.pixels[x * 4..x * 4 + 4].copy_from_slice(&[v, v, v, 255]);
    }
    let mut out = vec![0u8; 8];
    resample(&img, 2, 1, &mut out).expect("3x1 -> 2x1");
    assert_eq!(red(&out, 0), 25, "(0 * 192 + 100 * 64) >> 8");
    assert_eq!(red(&out, 1), 175, "(100 * 64 + 200 * 192) >> 8");
}

/// **The footprints tile the source: contiguous, non-overlapping, and covering everything.**
///
/// This is not cosmetic. `scale_y_area` walks the intermediate top to bottom **once per destination
/// row**, which is only correct because the ranges are monotonic — and it is the property that would
/// silently produce a skewed image if it ever failed. A filter that dropped or double-counted a row
/// would still return plausible pixels, just wrong ones.
#[test]
fn the_footprints_tile_the_source_without_gaps_or_overlap() {
    for (src, dst) in [(1920u32, 640u32), (1080, 360), (96, 14), (56, 8), (640, 640)] {
        let map = axis_area_map(src, dst);
        assert_eq!(map.len(), dst as usize, "one sample per destination pixel");
        let mut at = 0u32;
        for (i, a) in map.iter().enumerate() {
            assert_eq!(
                a.start, at,
                "footprint {i} starts at {at} for {src} -> {dst}: the ranges must be contiguous, or \\
                 a source pixel is either skipped or counted twice"
            );
            assert!(
                a.len >= 1,
                "footprint {i} is empty. A zero-length run has no average, so every footprint is at \\
                 least one pixel wide."
            );
            assert!(
                a.start + a.len <= src,
                "footprint {i} runs past the source: {} + {} > {src}",
                a.start,
                a.len
            );
            at = a.start + a.len;
        }
        assert!(
            at <= src,
            "the footprints cover {at} of {src} source pixels. Coverage short of the source means the \\
             last destination pixel's window was computed wrong."
        );
        assert_eq!(at, src, "a reduction must consume the whole source, and {src} -> {dst} did not");
    }

    // **Magnification is a different claim, and this is where the first version of this test was
    // wrong.** Under magnification the footprints *overlap*: at 7 -> 9 the first two destination pixels
    // both cover source 0, because `floor(i * 7/9)` is 0 for `i` in {0, 1}. That is correct -- each of
    // those destination pixels is centred near source 0, so source 0 should inform both -- and it means
    // **contiguity is a property of reductions only**. `scale_y_area` walks the intermediate once per
    // output row, which is sound because it only runs on a reducing axis here; the map itself is public
    // and does not enforce that, which is what this second half is for.
    let m = axis_area_map(7, 9);
    assert_eq!(m.len(), 9);
    assert_eq!(m[0], AreaSample { start: 0, len: 1 });
    assert_eq!(m[1], AreaSample { start: 0, len: 1 }, "the same source pixel, informatively twice");
    assert!(
        m.iter().all(|a| a.len >= 1 && a.start + a.len <= 7),
        "magnifying footprints are in range and never empty, even though they are not contiguous"
    );
}

/// **A long footprint does not overflow the accumulator.**
///
/// The `u32` accumulator holds `len * 255`, and `len` is a function of the reduction ratio — which a
/// caller chooses. A single-call 65536:1 reduction would put 16.7 M in a `u32` accumulator, which
/// still fits, but the *scaling* has to be right for the smaller cases too and a silent wrap would show
/// up as a dark band rather than as a failure.
///
/// **The real hazard is not the accumulator but the divisor's width**, and this checks both ends: a
/// wide footprint that saturates white, and a 2:1 case where the rounding is visible.
#[test]
fn a_long_footprint_does_not_overflow_the_accumulator() {
    // A 512:1 reduction of an all-white image. The accumulator peaks at 512 * 255 = 130,560, which is
    // well inside `u32` -- and the point is that the *result* is 255, not 0, so a wrap would show.
    let img = solid(512, 4, 255);
    let mut out = vec![0u8; holonomy_image::scale::bytes_for(1, 4)];
    resample(&img, 1, 4, &mut out).expect("512 -> 1");
    assert_eq!(
        out[0..4],
        [255, 255, 255, 255],
        "512 white pixels averaged must be white. A wrapped accumulator would come back near zero."
    );

    // And the same down the vertical axis, which is the pass with the SSE2 replacement and therefore
    // the one whose `u16` overflow argument (`scale_y`'s) does *not* apply to it.
    let img = solid(8, 512, 255);
    let mut out = vec![0u8; holonomy_image::scale::bytes_for(8, 1)];
    resample(&img, 8, 1, &mut out).expect("512 rows -> 1");
    assert_eq!(
        out[0..4],
        [255, 255, 255, 255],
        "512 white rows averaged must be white: the vertical area pass accumulates the same way"
    );

    // **The rounding, pinned.** Two mid-grey pixels whose mean is exactly x.5 must round up, because
    // truncating would bias every output one step dark and on a gradient that reads as a band.
    let mut pair = Rgba::new(2, 1);
    pair.pixels[0..4].copy_from_slice(&[100, 100, 100, 255]);
    pair.pixels[4..8].copy_from_slice(&[101, 101, 101, 255]);
    let mut out = vec![0u8; 4];
    resample(&pair, 1, 1, &mut out).expect("2x1 -> 1x1");
    assert_eq!(
        out[0], 101,
        "(100 + 101 + 1) / 2 rounds half-up to 101. Truncating gives 100, and a systematic dark bias \\
         is worse than the half-step it costs."
    );
}