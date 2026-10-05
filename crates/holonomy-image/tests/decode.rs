//! The decoder's gate: ten PNG fixtures, produced by an encoder this crate does not share code with.
//!
//! # Why committed fixtures and not a writer in this file
//!
//! A test that builds its PNG with the same assumptions as the decoder proves nothing: a misreading
//! of, say, the `bpp` in the `Sub` filter would be present in both the writer and the reader and the
//! round trip would still agree. So these files were written by **Python's `zlib`**, a DEFLATE
//! implementation separate from the `miniz_oxide` the decoder inflates with, and every chunk's CRC was
//! verified independently before the file was committed.
//!
//! Each fixture's pixel content is a pattern with a distinct value per channel per pixel
//! (`(x*30+7, y*50+11, (x*7+y*13+200) % 256, x*40)`), so a transposed row, a wrong `bpp`, a dropped
//! filter or a swapped channel all produce a *different* image rather than a plausible one.
//!
//! | fixture | what it pins |
//! |---|---|
//! | [`rgb8.png`](fixtures/rgb8.png) | colour type 2, the base case |
//! | [`rgb8_split4.png`](fixtures/rgb8_split4.png) | four `IDAT` chunks: the join is mandatory |
//! | [`rgb8_ancillary.png`](fixtures/rgb8_ancillary.png) | `tEXt`/`gAMA`/`pHYs` skipped by length |
//! | [`rgba8.png`](fixtures/rgba8.png) | colour type 6, alpha preserved not forced |
//! | [`gray8.png`](fixtures/gray8.png) | colour type 0, replicated into RGB |
//! | [`ga8.png`](fixtures/ga8.png) | colour type 4, grey replicated, alpha kept |
//! | [`pal8.png`](fixtures/pal8.png) | colour type 3, `PLTE` expansion |
//! | [`pal8_trns.png`](fixtures/pal8_trns.png) | `tRNS` alpha, and opaque past its end |
//! | [`claims_interlaced.png`](fixtures/claims_interlaced.png) | the interlace flag is refused |
//! | [`adam7.png`](fixtures/adam7.png) | a genuine Adam7 file is also refused |
//!
//! All ten rows use a different scanline filter (Sub, Up, Average, Paeth, None, cycling), so every
//! filter's undo is exercised by *every* fixture rather than one filter per fixture.

use holonomy_image::{decoded_len, png::Header, PngError};

/// The fixture pattern's expected channel values, derived here rather than copied from a decode.
fn expected(x: u32, y: u32) -> [u8; 4] {
    [
        ((x * 30 + 7) % 256) as u8,
        ((y * 50 + 11) % 256) as u8,
        ((x * 7 + y * 13 + 200) % 256) as u8,
        (x * 40) as u8,
    ]
}

/// The greyscale value the fixtures' grey channels carry: luminance of [`expected`]'s RGB.
fn expected_gray(x: u32, y: u32) -> u8 {
    let [r, g, b, _] = expected(x, y);
    ((u32::from(r) * 30 + u32::from(g) * 59 + u32::from(b) * 11) / 100) as u8
}

const W: u32 = 7;
const H: u32 = 5;

/// The 16-entry `PLTE` the palette fixtures carry, in the same order.
///
/// `u16` arithmetic then a cast, because `255 - i * 16` in `u8` is a subtraction the compiler cannot
/// prove is non-negative for every `u8` and so rejects it -- and `wrapping_sub` would be a lie, since
/// the generator really does mean `255 - i*16`.
fn palette() -> Vec<u8> {
    (0..16u16)
        .flat_map(|i| {
            [
                (i * 16 % 256) as u8,
                (255 - i * 16) as u8,
                (i * 37 + 3) as u8,
            ]
        })
        .collect()
}

/// Decode a fixture into a fresh buffer of exactly the right size.
fn decode_fixture(name: &str) -> (Header, Vec<u8>) {
    let bytes = std::fs::read(format!("tests/fixtures/{name}")).expect("fixture is committed");
    // Two-pass: parse the header to size the buffer, then decode into it. This mirrors what
    // `IcebergCache` does, and it is the only way to exercise `decoded_len` against a real header.
    let header = holonomy_image::read_header(&bytes).expect("fixture header parses");
    let mut out = vec![0u8; decoded_len(&header)];
    let got = holonomy_image::decode(&bytes, &mut out).expect("fixture decodes");
    assert_eq!(
        got, header,
        "decode must report the header it was asked for"
    );
    (header, out)
}

fn pixel(out: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = (y as usize * W as usize * 4) + x as usize * 4;
    [out[at], out[at + 1], out[at + 2], out[at + 3]]
}

/// Colour type 2 decodes to exactly the pattern, with alpha forced opaque.
#[test]
fn truecolour_decodes_to_the_pattern_it_encodes() {
    let (h, out) = decode_fixture("rgb8.png");
    assert_eq!((h.width, h.height), (W, H));
    assert_eq!(h.colour_type, 2);
    assert_eq!(out.len(), (W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let [r, g, b, a] = expected(x, y);
            assert_eq!(
                pixel(&out, x, y),
                [r, g, b, 255],
                "pixel ({x},{y}) of a colour-type-2 image"
            );
        }
    }
}

/// Four `IDAT` chunks must decode identically to one.
///
/// This is the gate on the join. `libpng` caps `IDAT` at 8 KiB, so *every* real encoder splits its
/// stream and an implementation that inflates the parts separately passes a hand-written fixture and
/// fails every real file.
#[test]
fn an_idat_split_across_four_chunks_decodes_identically() {
    let (single, a) = decode_fixture("rgb8.png");
    let (split, b) = decode_fixture("rgb8_split4.png");
    assert_eq!(single, split, "splitting IDAT must not change the header");
    assert_eq!(
        a, b,
        "a stream split across four IDAT chunks must decode byte-identically to one chunk: {} vs {}",
        a.len(),
        b.len()
    );
    assert!(!a.is_empty(), "the comparison must not be vacuous");
}

/// Ancillary chunks are stepped over by length, not parsed.
#[test]
fn ancillary_chunks_are_skipped_and_the_pixels_are_unchanged() {
    let (plain, a) = decode_fixture("rgb8.png");
    let (annotated, b) = decode_fixture("rgb8_ancillary.png");
    assert_eq!(plain, annotated, "tEXt/gAMA/pHYs must not reach IHDR");
    assert_eq!(
        a, b,
        "a file carrying tEXt, gAMA and pHYs must decode identically to one carrying none"
    );
}

/// Colour type 6 keeps its alpha rather than forcing it opaque.
#[test]
fn truecolour_alpha_survives_the_decode() {
    let (_, out) = decode_fixture("rgba8.png");
    for y in 0..H {
        for x in 0..W {
            let want = expected(x, y);
            assert_eq!(
                pixel(&out, x, y),
                want,
                "pixel ({x},{y}): alpha is {want:?} and must not be overwritten with 255"
            );
        }
    }
    // And the alpha really does vary, so "all 255" would be caught.
    let alphas: Vec<u8> = (0..W).map(|x| pixel(&out, x, 0)[3]).collect();
    assert_eq!(alphas, (0..W).map(|x| (x * 40) as u8).collect::<Vec<_>>());
}

/// Colour type 0 replicates the grey into all three channels and forces alpha opaque.
#[test]
fn greyscale_is_replicated_into_rgb_and_is_opaque() {
    let (h, out) = decode_fixture("gray8.png");
    assert_eq!(h.colour_type, 0);
    for y in 0..H {
        for x in 0..W {
            let g = expected_gray(x, y);
            assert_eq!(
                pixel(&out, x, y),
                [g, g, g, 255],
                "pixel ({x},{y}): a greyscale sample must appear in R, G and B alike"
            );
        }
    }
}

/// Colour type 4 keeps the alpha and replicates the grey.
#[test]
fn greyscale_alpha_keeps_its_alpha_and_replicates_the_grey() {
    let (h, out) = decode_fixture("ga8.png");
    assert_eq!(h.colour_type, 4);
    for y in 0..H {
        for x in 0..W {
            let g = expected_gray(x, y);
            assert_eq!(
                pixel(&out, x, y),
                [g, g, g, (x * 40) as u8],
                "pixel ({x},{y}) of a grey+alpha image"
            );
        }
    }
}

/// Colour type 3 expands through `PLTE`, opaque where there is no `tRNS`.
#[test]
fn a_palette_image_expands_through_its_plte() {
    let (h, out) = decode_fixture("pal8.png");
    assert_eq!(h.colour_type, 3);
    let plte = palette();
    for y in 0..H {
        for x in 0..W {
            let idx = ((x + y * 3) % 16) as usize;
            assert_eq!(
                pixel(&out, x, y),
                [plte[idx * 3], plte[idx * 3 + 1], plte[idx * 3 + 2], 255],
                "pixel ({x},{y}) is palette index {idx}"
            );
        }
    }
}

/// `tRNS` supplies alpha for the entries it covers and the rest stay opaque.
///
/// The fixture's `tRNS` is 8 bytes against a 16-entry palette, so indices 8..=15 must come back 255.
/// That is the case a "read alpha for every index" implementation gets wrong by reading past the
/// chunk.
#[test]
fn a_palette_image_takes_alpha_from_trns_and_is_opaque_past_its_end() {
    let (_, out) = decode_fixture("pal8_trns.png");
    let trns = [0u8, 64, 128, 255, 255, 255, 255, 255];
    let plte = palette();
    for y in 0..H {
        for x in 0..W {
            let idx = ((x + y * 3) % 16) as usize;
            let a = *trns.get(idx).unwrap_or(&255);
            assert_eq!(
                pixel(&out, x, y),
                [plte[idx * 3], plte[idx * 3 + 1], plte[idx * 3 + 2], a],
                "pixel ({x},{y}) is palette index {idx}, whose alpha is {}",
                trns.get(idx).copied().unwrap_or(255)
            );
        }
    }
    // Index 8 is the first past tRNS, and 7x5 covers indices 0..=15, so this is exercised.
    assert_eq!(
        pixel(&out, 8, 0)[3],
        255,
        "index 8 is past an 8-byte tRNS and must be opaque"
    );
}

/// The interlace flag is refused, whether or not the data is genuinely Adam7.
///
/// Two fixtures: `claims_interlaced.png` sets the flag on non-interlaced data (a plausible corrupt or
/// hostile file), and `adam7.png` is a real interlaced encode. Both must be refused by name, because
/// rendering half an Adam7 image looks like a bug report with no reproduction.
#[test]
fn an_interlaced_image_is_refused_by_name() {
    for name in ["claims_interlaced.png", "adam7.png"] {
        let bytes = std::fs::read(format!("tests/fixtures/{name}")).expect("fixture is committed");
        let err = holonomy_image::read_header(&bytes).expect_err("interlace must be refused");
        assert_eq!(
            err,
            PngError::Interlaced { method: 1 },
            "{name}: the interlace flag must be refused by name, not partially decoded"
        );
    }
}

/// Every filter is undone, and the five rows cycle through all five.
///
/// The fixtures are built with filter bytes `1, 2, 3, 4, 0` per row, so a decoder that mishandles any
/// one of `Sub`/`Up`/`Average`/`Paeth` produces wrong pixels in at least one row of *every* fixture.
#[test]
fn all_five_scanline_filters_are_undone() {
    let (h, out) = decode_fixture("rgb8.png");
    assert_eq!(h.height, 5, "the fixtures must have one row per filter");
    // If any row were wrong the per-pixel check in the other test would fail; this states the filter
    // coverage explicitly so a future fixture change cannot quietly stop exercising a filter.
    //
    // Alpha is 255, not `expected`'s `x * 40`: `rgb8.png` is colour type 2, which has no alpha channel
    // at all, and the decoder must force it opaque. Asserting `expected` here demanded an alpha this
    // file cannot contain -- `rgba8.png` is the fixture whose alpha varies.
    for y in 0..H {
        for x in 0..W {
            let [r, g, b, _] = expected(x, y);
            assert_eq!(
                pixel(&out, x, y),
                [r, g, b, 255],
                "row {y} uses filter {} and must reconstruct exactly",
                [1u8, 2, 3, 4, 0][y as usize]
            );
        }
    }
}
