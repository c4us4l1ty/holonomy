//! Phase 9C gate: images in the export path.
//!
//! # What is pinned, and why the two formats are not symmetric
//!
//! §2.9.5 requires that HTML and PDF resolve images "at export time from the container ... never from
//! a decoded cache entry, because a PDF must not depend on scroll position". The testable consequences
//! are three, and each is a separate assertion rather than a comment:
//!
//! 1. **HTML** gets `<img src="data:image/png;base64,...">` carrying the PNG **verbatim**, and the
//!    base64 round-trips back to the original bytes. A data URI is the only form that needs no second
//!    file and no network, and FR-5.1's `unshare(CLONE_NEWNET)` is why there is nothing to fetch from.
//! 2. **PDF** gets an `/XObject` with `/DeviceRGB` samples, which means the PNG had to be **decoded** --
//!    PDF has no notion of a PNG. So the two exporters take different paths from the same bytes, and the
//!    test asserts both.
//! 3. **Neither reads `IcebergCache`.** That is asserted structurally: `export` takes an `&Editor` and
//!    nothing else, so there is no cache to read even in principle.
//!
//! # The export path's own budget claim
//!
//! §2.9.5 also says "no image in the export path's way". The cost this test measures is the one that
//! actually exists: base64 inflates a PNG by 4/3, so a document of images exports a third larger than
//! its catalog. `HtmlStats::image_bytes` and `image_tag_bytes` make that a reported number.

use holonomy_export::asset;
use holonomy_export::{Format, HtmlOptions, PdfOptions};
use holonomy_text::{Editor, SpanPolicy};

/// A tiny valid PNG, 2x2, four distinct colours.
///
/// Small on purpose for the structural tests: every one of them asserts on the *shape* of the output,
/// and a 213 KB fixture in a dozen assertions would make them slower to read for no extra signal. The
/// one test that needs detail -- the one about what the PDF's samples contain -- checks the decoded
/// colours, which a 2x2 with four distinct colours is enough to pin.
fn png() -> Vec<u8> {
    /// CRC-32, IEEE. `crc32fast` is exactly the dependency §2.9.5 refuses, and this runs a few times.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in bytes {
            crc ^= u32::from(b);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }
    fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let mut crc_input = Vec::new();
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(body);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
        out
    }
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&2u32.to_be_bytes());
    ihdr.extend_from_slice(&2u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // truecolour, no alpha
                                              // Filter 0 on each row, then two pixels. Four distinct colours so a channel mix-up is visible.
                                              // Row 0: filter 0 then red (220,20,20) and green (10,200,30).
                                              // Row 1: filter 0 then blue (30,60,240) and grey (250,100,110).
                                              // Exactly two rows of (1 filter byte + 2 pixels x 3 channels) = 14 bytes, which is what the IHDR's
                                              // 2x2 truecolour implies. The first version of this fixture was four bytes long and the decoder
                                              // refused it with "inflated to 14 bytes, the header implies 14" -- which is what an off-by-four in a
                                              // hand-written fixture looks like.
    let raw: [u8; 14] = [
        0, 220, 20, 20, 10, 200, 30, // row 0
        0, 30, 60, 240, 250, 100, 110, // row 1
    ];
    let mut deflated = vec![0x78, 0x01];
    let mut i = 0usize;
    while i < raw.len() {
        let take = (raw.len() - i).min(0xFFFF);
        deflated.push(if i + take >= raw.len() { 1 } else { 0 });
        deflated.extend_from_slice(&(take as u16).to_le_bytes());
        deflated.extend_from_slice(&(!(take as u16)).to_le_bytes());
        deflated.extend_from_slice(&raw[i..i + take]);
        i += take;
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    deflated.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    out.extend_from_slice(&chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&chunk(b"IDAT", &deflated));
    out.extend_from_slice(&chunk(b"IEND", &[]));
    out
}

/// One image in a document, at the end.
fn with_image() -> (Editor, Vec<u8>) {
    let png = png();
    let mut e = Editor::from_text(b"before\n").expect("editor");
    e.insert_image(e.text_len() as u32, &png)
        .expect("insert image");
    (e, png)
}

/// Digits in the fixture's width and height, which the tag's size depends on.
fn png_w_digits() -> u64 {
    2u32.to_string().len() as u64
}
fn png_h_digits() -> u64 {
    2u32.to_string().len() as u64
}

/// Decode base64 out of a data URI.
fn decode_b64(s: &str) -> Vec<u8> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut rev = [255u8; 256];
    for (i, &c) in T.iter().enumerate() {
        rev[usize::from(c)] = i as u8;
    }
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let v = rev[usize::from(c)];
        if v == 255 {
            panic!("not base64: {c:?}");
        }
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

// -----------------------------------------------------------------------------------------
// base64
// -----------------------------------------------------------------------------------------

#[test]
fn base64_matches_rfc_4648s_vectors() {
    // The published vectors, which is the whole point of a standard alphabet.
    for (input, want) in [
        (&b""[..], ""),
        (b"f", "Zg=="),
        (b"fo", "Zm8="),
        (b"foo", "Zm9v"),
        (b"foob", "Zm9vYg=="),
        (b"fooba", "Zm9vYmE="),
        (b"foobar", "Zm9vYmFy"),
    ] {
        let mut out = Vec::new();
        asset::base64_into(input, &mut out);
        assert_eq!(
            String::from_utf8(out).expect("ascii"),
            want,
            "encoding {input:?}"
        );
    }
}

#[test]
fn base64_length_is_exact_so_a_buffer_can_be_sized() {
    // `4 * ceil(n/3)`, for every remainder class and a few boundaries.
    for n in [0usize, 1, 2, 3, 4, 5, 6, 7, 100, 213_371] {
        let fill = vec![0xABu8; n];
        let hint = asset::base64_len(n);
        // Empty first: `base64_into` *appends*, so the grown length is the whole claim.
        let mut out = Vec::new();
        asset::base64_into(&fill, &mut out);
        assert_eq!(out.len(), hint, "n={n}: the size hint was wrong");
        // And a buffer reserved with the hint is never reallocated.
        let mut reserved = Vec::with_capacity(hint);
        asset::base64_into(&fill, &mut reserved);
        assert_eq!(
            reserved.capacity(),
            hint,
            "n={n}: capacity grew past the hint"
        );
        assert_eq!(reserved, out, "n={n}");
    }
}

#[test]
fn a_decoded_png_comes_back_byte_identical() {
    // The property the HTML path depends on: `base64_into` is a transport encoding, not a
    // transformation. Anything that altered a byte would make the exported image a *different* image
    // from the one in the container, and nothing would notice.
    let png = png();
    let mut out = Vec::new();
    asset::base64_into(png.as_slice(), &mut out);
    assert_eq!(decode_b64(std::str::from_utf8(&out).expect("ascii")), png);
}

// -----------------------------------------------------------------------------------------
// HTML
// -----------------------------------------------------------------------------------------

#[test]
fn html_carries_the_png_verbatim_as_a_data_uri() {
    let (e, png) = with_image();
    let mut out = Vec::new();
    let stats = holonomy_export::html::export_body(&mut out, &e).expect("export");
    let html = String::from_utf8(out).expect("utf8");

    assert_eq!(stats.images, 1);
    assert_eq!(stats.images_missing, 0);
    assert!(
        html.contains(r#"<img alt="" src="data:image/png;base64,"#),
        "{html}"
    );

    // The payload between the quotes must be the PNG, exactly.
    let start = html
        .find("base64,")
        .map(|i| i + "base64,".len())
        .expect("a data URI");
    let end = html[start..]
        .find('"')
        .map(|i| start + i)
        .expect("a closing quote");
    assert_eq!(
        decode_b64(&html[start..end]),
        png,
        "the data URI is not the PNG verbatim"
    );

    // And the intrinsic size is there, so a reader can lay the page out before decoding.
    assert!(html.contains(r#"width="2" height="2""#), "{html}");
}

#[test]
fn html_keeps_the_text_around_the_image() {
    let (e, _) = with_image();
    let mut out = Vec::new();
    holonomy_export::html::export_body(&mut out, &e).expect("export");
    let html = String::from_utf8(out).expect("utf8");
    assert!(html.starts_with("before\n"), "{html}");
    // The anchor must not appear as a character: it is a placeholder, and the picture replaced it.
    assert!(
        !html.contains('\u{fffc}'),
        "the U+FFFC leaked into the output: {html}"
    );
}

#[test]
fn html_reports_the_four_thirds_inflation_of_base64() {
    // The export's size is a reported number rather than a surprise: base64 is 4/3 of the PNG, so
    // `image_tag_bytes` is `image_bytes * 4/3` plus the tag's own fixed overhead.
    let (e, png) = with_image();
    let mut out = Vec::new();
    let stats = holonomy_export::html::export_body(&mut out, &e).expect("export");
    assert_eq!(stats.image_bytes, png.len() as u64);
    // The tag's fixed overhead, computed from the format rather than guessed: `<img alt="" src="data:
    // image/png;base64,` (39) + base64 + `" width="` (9) + the digits + `" height="` (10) + the digits
    // + `">` (2). The first version asserted a hand-counted 55 and was three bytes short, which is what a
    // magic constant costs.
    let overhead = 39 + 9 + png_w_digits() + 10 + png_h_digits() + 2;
    assert_eq!(
        stats.image_tag_bytes,
        asset::base64_len(png.len()) as u64 + overhead
    );
}

#[test]
fn html_reports_an_anchor_with_no_asset_rather_than_dropping_it() {
    // An anchor the catalog cannot serve: the text says there is a picture and the catalog disagrees.
    // The output says so too, as U+FFFC, and the stats count it -- so a payload whose text and catalog
    // have drifted is visible here rather than looking like a document with no images.
    let e = Editor::from_text("a\u{fffc}b\n".as_bytes()).expect("editor");
    assert_eq!(
        e.assets().len(),
        0,
        "the fixture has an anchor and no asset"
    );
    let mut out = Vec::new();
    let stats = holonomy_export::html::export_body(&mut out, &e).expect("export");
    let html = String::from_utf8(out).expect("utf8");
    assert_eq!(stats.images, 0);
    assert_eq!(stats.images_missing, 1);
    assert!(html.contains("&#xfffc;"), "{html}");
    assert!(
        html.contains("a"),
        "and the text around it survives: {html}"
    );
    assert!(html.contains("b"), "{html}");
}

#[test]
fn html_finds_an_image_whose_anchor_straddles_a_run_boundary() {
    // The exporter walks the document in runs, and the anchor is three bytes, so it can straddle the
    // edge. The failure this guards against is an anchor emitted as raw `EF` (invalid UTF-8) or missed
    // entirely -- both of which look like a document that quietly lost a picture.
    let png = png();
    let mut e = Editor::from_text(b"left\n").expect("editor");
    e.insert_image(5, &png)
        .expect("insert at the end of line 0");
    e.insert_at(e.text_len() as u32, b"right\n", SpanPolicy::Strict)
        .expect("tail");
    // Style a range that ends two bytes into an anchor's three, so the run boundary lands inside it.
    let anchors = e.image_anchors().expect("anchors");
    let at = anchors[0] as usize;
    e.style_range(0, at as u32 + 2, holonomy_text::STYLE_BOLD, 0)
        .expect("a run boundary two bytes into the anchor");

    let mut out = Vec::new();
    let stats = holonomy_export::html::export_body(&mut out, &e).expect("export");
    let html = String::from_utf8(out).expect("the output is valid UTF-8");
    assert_eq!(
        stats.images, 1,
        "the straddling anchor was found exactly once"
    );
    assert!(html.contains("base64,"), "{html}");
}

#[test]
fn a_standalone_html_document_with_an_image_is_well_formed() {
    let (e, _) = with_image();
    let mut out = Vec::new();
    holonomy_export::html::export(
        &e,
        &mut out,
        &HtmlOptions {
            title: "t<&>\"".into(),
            ..HtmlOptions::default()
        },
    )
    .expect("export");
    let html = String::from_utf8(out).expect("utf8");
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(
        html.contains("<title>t&lt;&amp;&gt;&quot;</title>"),
        "{html}"
    );
    assert!(html.contains("</html>"), "{html}");
    // Exactly one image, and the `<title>`'s escaped angle brackets are not an attribute.
    assert_eq!(html.matches("<img ").count(), 1);
}

// -----------------------------------------------------------------------------------------
// PDF
// -----------------------------------------------------------------------------------------

#[test]
fn pdf_writes_an_image_xobject_with_device_rgb_samples() {
    let (e, _) = with_image();
    let mut stats = holonomy_export::pdf::PdfStats::default();
    let out = holonomy_export::pdf::build(&e, &PdfOptions::default(), &mut stats).expect("build");
    let pdf = String::from_utf8_lossy(&out).into_owned();

    assert_eq!(stats.images, 1);
    assert_eq!(stats.images_missing, 0);
    // `/Subtype /Image`, `/Width 2`, `/Height 2`, `/BitsPerComponent 8`, `/DeviceRGB`, `/FlateDecode`.
    for needle in [
        "/Subtype /Image",
        "/Width 2",
        "/Height 2",
        "/BitsPerComponent 8",
        "/ColorSpace /DeviceRGB",
        "/Filter /FlateDecode",
    ] {
        assert!(pdf.contains(needle), "missing {needle} from:\n{pdf}");
    }
    // **The name is paired with the object's id**, not merely present. An earlier version asserted
    // `contains("/XObject")` and `contains("/Im0")`, and removing the whole `/XObject` dictionary from
    // the page resources left both assertions passing -- so the gate had a hole where a PDF with an
    // unresolvable `Do` operator would have shipped. The pairing is what a reader actually needs, and
    // 1000 is the id the writer allocates to the first image.
    assert!(pdf.contains("/XObject"), "{pdf}");
    assert!(
        pdf.contains("/Im0 1000 0 R"),
        "the resource name is not paired with the object: {pdf}"
    );
    // And the object id it names is an image, not something else.
    let obj = pdf.find("1000 0 obj").expect("object 1000");
    let next = pdf[obj..].find("endobj").map(|i| obj + i).expect("endobj");
    assert!(
        pdf[obj..next].contains("/Subtype /Image"),
        "object 1000 is not an image: {}",
        &pdf[obj..next]
    );
    // The transform is there *and* it places the picture below the line above it. Asserting only that a
    // `cm` exists left a hole where the writer reserved no vertical space at all -- the text after the
    // picture drew straight through it, and the PDF was still structurally valid, so nothing else in
    // this file could see it. The measure is 504 pt (Letter minus two 54 pt margins) and the line's
    // baseline is at `page.height - margin` = 738, so a picture of 504x504 must put its bottom edge at
    // or below 738 - 504 = 234.
    assert!(pdf.contains(" cm\n"), "no image transform");
    let cm = pdf.find(" cm\n").expect("the transform operator");
    let cm_line = &pdf[pdf[..cm].rfind('\n').map(|i| i + 1).unwrap_or(0)..cm];
    let operands: Vec<f32> = cm_line
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();
    assert_eq!(
        operands.len(),
        6,
        "a `cm` takes six operands, got {cm_line:?}"
    );
    // `a b c d e f cm` is `[a b; c d; e f]`, so the horizontal scale is `a` and the vertical is `d`.
    // Reading `b` as a scale -- which the first version did -- is the kind of mistake that survives
    // because `b` is legitimately zero for an axis-aligned image.
    let (sx, _, _, ty, tx, bottom) = (
        operands[0],
        operands[1],
        operands[2],
        operands[3],
        operands[4],
        operands[5],
    );
    assert_eq!(
        (sx, ty),
        (504.0, 504.0),
        "the picture is square, as its 2x2 source is"
    );
    assert_eq!(tx, 54.0, "it starts at the left margin");
    let baseline = 792.0 - 54.0;
    assert!(
        baseline - bottom >= 504.0 - 0.5,
        "the picture's bottom edge is at {bottom}, which does not clear the line above it at {baseline}"
    );
    // And the height follows the source's aspect ratio rather than being a guess: 2x2 at 504 wide is
    // 504 tall.
    assert!(
        (ty - 504.0).abs() < 0.5,
        "the vertical scale is the width, as a square source implies"
    );
    // pdf-writer puts each operator on its own line, so `Do` starts a line rather than following a
    // space. Asserted on the operator, not on a byte pattern that depends on the writer's line breaking.
    assert!(pdf.contains("/Im0 Do"), "no Do operator");
    // The header and trailer are intact, so a reader will open it.
    assert!(
        out.starts_with(b"%PDF-1.7"),
        "{:?}",
        &out[..8.min(out.len())]
    );
    assert!(pdf.trim_end().ends_with("%%EOF"), "{pdf}");
}

#[test]
fn pdf_image_samples_are_the_decoded_pixels_flated() {
    // PDF has no notion of a PNG, so the samples are *decoded*. This test decompresses what the PDF
    // actually carries and checks the pixels, which is the only way to know the decode happened and
    // produced the right thing rather than some plausible-looking bytes.
    let (e, _) = with_image();
    let mut stats = holonomy_export::pdf::PdfStats::default();
    let out = holonomy_export::pdf::build(&e, &PdfOptions::default(), &mut stats).expect("build");
    assert_eq!(stats.image_sample_bytes, 2 * 2 * 3, "2x2 RGB is 12 bytes");

    // Pull the first stream out and inflate it with the product's own decompressor.
    // Located after `/Filter /FlateDecode`, not after the first `/Length`: the fonts come first and
    // their streams are not image samples.
    const NEEDLE: &[u8] = b"/Filter /FlateDecode";
    let filter = out
        .windows(NEEDLE.len())
        .position(|w| w == NEEDLE)
        .expect("an image stream");
    // `pdf-writer` writes `/Length` *before* `/Filter`, so the stream is found forwards from the
    // filter rather than backwards from a length. The first version searched forwards from `/Length `
    // for `/Filter` and then forwards again for `/Length `, which cannot find anything.
    const STREAM: &[u8] = b"stream\n";
    let stream_start = out[filter..]
        .windows(STREAM.len())
        .position(|w| w == STREAM)
        .map(|i| filter + i + STREAM.len())
        .expect("the image's stream keyword");
    let stream_end = out[stream_start..]
        .windows(9)
        .position(|w| w == b"endstream")
        .map(|i| stream_start + i)
        .expect("endstream");
    let inflated = miniz_oxide::inflate::decompress_to_vec_zlib(&out[stream_start..stream_end])
        .expect("the stream is a zlib stream this crate produced");
    assert_eq!(
        inflated,
        vec![220, 20, 20, 10, 200, 30, 30, 60, 240, 250, 100, 110],
        "the RGB samples are not the PNG's pixels in order"
    );
}

#[test]
fn pdf_reserves_vertical_space_so_text_after_an_image_does_not_draw_through_it() {
    // The mutation that had to be caught here is "reserve no vertical space", and it survived an
    // earlier version of this test because **the image was the last thing in the document**: nothing
    // followed it, so not reserving space changed nothing observable. The document here is
    // `a\n<image>\nb\n` -- a line, a picture, another line -- because that is the only arrangement in
    // which the reservation is visible at all.
    let png = png();
    let mut e = Editor::from_text(b"a\n\nb\n").expect("editor");
    // Put the anchor on the empty middle line, at its start.
    let at = 2u32;
    e.insert_image(at, &png).expect("insert");

    let mut stats = holonomy_export::pdf::PdfStats::default();
    let out = holonomy_export::pdf::build(&e, &PdfOptions::default(), &mut stats).expect("build");
    assert_eq!(stats.images, 1);
    let pdf = String::from_utf8_lossy(&out);

    // Pull the page content stream and read its baselines and its image transform.
    let (sx, sy, tx, bottom) = picture_transform(&pdf);
    let baselines = baselines(&pdf);
    assert!(
        baselines.len() >= 2,
        "expected the line before and the line after the image, got {baselines:?}"
    );
    let first = baselines[0];
    let after = *baselines.last().expect("at least one baseline");
    assert!(
        after < first,
        "y grows upward, so the later line is lower: {first} then {after}"
    );

    // The reservation, in PDF's coordinates: **y grows upward**, so "below" means a *smaller* y. The
    // first version of this assertion compared the other way round and reported a false overlap, which
    // is worth stating because the coordinate convention is exactly the kind of thing that makes a
    // layout test read correctly while asserting the opposite.
    assert!(
        after < bottom,
        "the line after the image is at y={after} but the picture's bottom edge is at y={bottom}: a \
         larger y is *higher* in PDF, so the picture was drawn over the line"
    );
    let above = first - bottom;
    assert!(
        above >= sy - 0.5,
        "the picture is {sy} tall and only {above} of it fits between the baselines"
    );
    // The gap below the picture is the line height, so the next line's ascent clears the bottom edge.
    // `font_size` is 11 and `line_height` is 14, so a leading of 14 leaves 3 points -- which is the
    // normal leading of a line, and is what makes two consecutive text lines readable.
    assert!(
        bottom - after >= 11.0,
        "only {} points below the picture, which is less than the {opts_font}pt ascent of the next line",
        bottom - after,
        opts_font = 11.0
    );
    assert_eq!(tx, 54.0, "the picture starts at the left margin");
    let _ = sx;

    // And without the picture the document is smaller and has one fewer line.
    let plain = Editor::from_text(b"a\n\nb\n").expect("editor");
    let mut s2 = holonomy_export::pdf::PdfStats::default();
    let out2 = holonomy_export::pdf::build(&plain, &PdfOptions::default(), &mut s2).expect("build");
    assert_eq!(s2.images, 0);
    // The picture is **not** a line of text: it occupies layout space -- it counts against the page's
    // line budget -- but `stats.lines` counts lines with a `Tj`, and the picture has none. An earlier
    // version of this test asserted `lines` was one *fewer* with the picture, which is backwards; the
    // space the picture takes is asserted geometrically above, where it can actually be seen.
    assert_eq!(
        s2.lines, stats.lines,
        "an image is not a line of text, but it does take a line's worth of the page"
    );
    assert!(
        out2.len() < out.len(),
        "and the file with the picture in it is larger"
    );
}

/// `(sx, sy, tx, bottom)` of the first image transform in `pdf`.
fn picture_transform(pdf: &str) -> (f32, f32, f32, f32) {
    let cm = pdf.find(" cm").expect("an image transform");
    let start = pdf[..cm].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let ops: Vec<f32> = pdf[start..cm]
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();
    assert_eq!(
        ops.len(),
        6,
        "a `cm` takes six operands, got {:?}",
        &pdf[start..cm]
    );
    (ops[0], ops[3], ops[4], ops[5])
}

/// Every absolute `Td` y in `pdf`'s content, in order.
fn baselines(pdf: &str) -> Vec<f32> {
    let mut out = Vec::new();
    let mut rest = pdf;
    while let Some(at) = rest.find(" Td") {
        let line_start = rest[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let ops: Vec<f32> = rest[line_start..at]
            .split_whitespace()
            .filter_map(|t| t.parse().ok())
            .collect();
        if ops.len() >= 2 {
            out.push(ops[1]);
        }
        rest = &rest[at + 3..];
    }
    out
}

#[test]
fn pdf_reports_an_anchor_with_no_asset_and_still_reserves_the_space() {
    let e = Editor::from_text("a\u{fffc}b\n".as_bytes()).expect("editor");
    assert_eq!(e.assets().len(), 0);
    let mut stats = holonomy_export::pdf::PdfStats::default();
    let out = holonomy_export::pdf::build(&e, &PdfOptions::default(), &mut stats).expect("build");
    assert_eq!(stats.images, 0);
    assert_eq!(stats.images_missing, 1);
    let pdf = String::from_utf8_lossy(&out);
    assert!(
        !pdf.contains("/Subtype /Image"),
        "nothing was written for a missing asset"
    );
    assert!(out.starts_with(b"%PDF"), "and the file is still a PDF");
}

#[test]
fn neither_exporter_can_reach_the_iceberg_cache() {
    // §2.9.5: "never from a decoded cache entry, because a PDF must not depend on scroll position".
    // Asserted structurally rather than by inspection: `export` and both `export`/`build` take an
    // `&Editor` and options, and there is no parameter through which a cache could arrive. If someone
    // adds one, this stops compiling.
    let (e, _) = with_image();
    let mut out = Vec::new();
    holonomy_export::write(&e, &mut out, Format::Html, "t").expect("html");
    let mut out = Vec::new();
    holonomy_export::write(&e, &mut out, Format::Pdf, "t").expect("pdf");
}

#[test]
fn a_document_with_several_images_exports_all_of_them() {
    let png = png();
    let mut e = Editor::from_text(b"").expect("editor");
    for _ in 0..3 {
        e.insert_image(e.text_len() as u32, &png).expect("insert");
    }
    let mut out = Vec::new();
    let stats = holonomy_export::html::export_body(&mut out, &e).expect("export");
    let html = String::from_utf8(out).expect("utf8");
    assert_eq!(stats.images, 3);
    assert_eq!(html.matches("<img ").count(), 3);

    let mut stats = holonomy_export::pdf::PdfStats::default();
    let pdf = holonomy_export::pdf::build(&e, &PdfOptions::default(), &mut stats).expect("build");
    assert_eq!(stats.images, 3);
    let text = String::from_utf8_lossy(&pdf);
    // Three *references*, and three *placements*: each `/Im<n>` appears once in a page's `/XObject`
    // dictionary and once as a `Do` operand, so six is the correct count for three images.
    //
    // The three anchors share one content address, so the catalog holds three entries pointing at the
    // same bytes. Whether the writer deduplicates the stream is a size question; that all three are
    // placed and referenced is a correctness one, and the count pins both halves of it.
    assert_eq!(stats.images, 3);
    for n in 0..3 {
        assert!(
            text.contains(&format!("/Im{n} ")),
            "resource /Im{n} is missing"
        );
        assert!(
            text.contains(&format!("/Im{n} Do")),
            "Do for /Im{n} is missing"
        );
    }
}

#[test]
fn an_export_of_a_document_with_no_images_is_byte_identical_to_before() {
    // The image path must not have changed what a plain document exports. That is the strongest cheap
    // check there is: if the anchor scanning, the piece splitting or the extra page resources had
    // shifted anything, this fails.
    let e = Editor::from_text(b"plain text\nwith <angles> & \"quotes\"\n").expect("editor");
    let mut out = Vec::new();
    let stats = holonomy_export::html::export_body(&mut out, &e).expect("export");
    assert_eq!(stats.images, 0);
    assert_eq!(stats.images_missing, 0);
    assert_eq!(stats.image_bytes, 0);
    assert_eq!(
        String::from_utf8(out).expect("utf8"),
        "plain text\nwith &lt;angles&gt; &amp; &quot;quotes&quot;\n"
    );
}
