//! Images in the export path.
//!
//! # What each format needs, and why they are not symmetric
//!
//! §2.9.5: "HTML and PDF get `<img>`/XObject references resolved at export time from the container; if
//! an image is not in the viewport the exporter reads it from the container fd with `pread64`
//! (allowlisted), never from a decoded cache entry, because a PDF must not depend on scroll
//! position."
//!
//! Two rules follow, and both are load-bearing:
//!
//! * **Neither exporter may touch `IcebergCache`.** A PDF that drew an image only when it happened to
//!   be scrolled into view would be a document that changes when you scroll.
//! * **The bytes come from `Editor::assets()`** -- the payload's catalog, always present -- not from a
//!   decoded entry.
//!
//! | | HTML | PDF |
//! |---|---|---|
//! | a reader wants | `data:image/png;base64,...` | an `/XObject` with `/DeviceRGB` samples |
//! | therefore needs | the PNG, verbatim | the **decoded pixels** |
//!
//! PDF has no notion of "a PNG", so the PDF path must decode. That is why this crate takes a
//! dependency on `holonomy-image`, and it is the only one it takes: §2.9.5 already put the decoder in
//! the product, and reusing it is cheaper and more correct than a second implementation.
//!
//! # No floats, except the one that has to be a float
//!
//! Zero-Bézier Invariant (§2.2) is about curve evaluation, and neither a base64 encoder nor a chroma
//! flatten has a curve in it. The one float is the image's height in PDF points, which is
//! `measure * src_h / src_w`: PDF's coordinate system is in points and the aspect ratio has to survive.
//! Everything else in this file is integer arithmetic over bytes.

use holonomy_text::{Asset, AssetCatalog, ANCHOR_BYTES};

/// Append `bytes` to `out`, base64-encoded.
///
/// **A hand-written encoder, not a crate.** A dependency whose entire job is a 64-entry table and a
/// shift is not worth a version to track, and §2.9.5's rule -- one decoder, no incidental crates -- is
/// a statement about what this crate pulls in.
///
/// RFC 4648 §4, `=` padded. `+` and `/` rather than the URL-safe `-` and `_`, because the output goes
/// inside a `data:` URI in an HTML *attribute*: `data:` is never percent-decoded by a reader, and the
/// URL-safe alphabet would be one more way for this to differ from what a base64 decoder expects.
pub fn base64_into(bytes: &[u8], out: &mut Vec<u8>) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    // Index loop rather than `chunks_exact(3)`: clippy flags the constant chunk size as a missed
    // `as_chunks`, and the index loop also keeps the tail handling below reading as one thing rather
    // than two. Same arithmetic, and one fewer suggestion to suppress.
    let whole = bytes.len() / 3;
    for i in 0..whole {
        let c = &bytes[i * 3..i * 3 + 3];
        let n = (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63]);
        out.push(ALPHABET[(n >> 12) as usize & 63]);
        out.push(ALPHABET[(n >> 6) as usize & 63]);
        out.push(ALPHABET[n as usize & 63]);
    }
    match &bytes[whole * 3..] {
        [a] => {
            let n = u32::from(*a) << 16;
            out.push(ALPHABET[(n >> 18) as usize & 63]);
            out.push(ALPHABET[(n >> 12) as usize & 63]);
            out.push(b'=');
            out.push(b'=');
        }
        [a, b] => {
            let n = (u32::from(*a) << 16) | (u32::from(*b) << 8);
            out.push(ALPHABET[(n >> 18) as usize & 63]);
            out.push(ALPHABET[(n >> 12) as usize & 63]);
            out.push(ALPHABET[(n >> 6) as usize & 63]);
            out.push(b'=');
        }
        _ => {}
    }
}

/// The number of base64 characters `n` bytes encode to, padding included.
///
/// So a caller can size a buffer exactly rather than growing one -- which matters when the thing being
/// encoded is a 213 KB PNG.
#[must_use]
pub fn base64_len(n: usize) -> usize {
    n.div_ceil(3) * 4
}

/// Append `<img src="data:...">` for `asset` to `out`.
///
/// The `width`/`height` attributes are the **source** dimensions, for two reasons: a reader can lay the
/// page out before the data URI is decoded, and an image with no intrinsic size makes the document jump
/// when it arrives. They are attributes rather than CSS, so they cannot change the rendered size.
pub fn html_img_into(asset: &Asset, out: &mut Vec<u8>) {
    out.extend_from_slice(br#"<img alt="" src="data:image/png;base64,"#);
    base64_into(asset.png.as_slice(), out);
    out.extend_from_slice(b"\" width=\"");
    out.extend_from_slice(asset.width.to_string().as_bytes());
    out.extend_from_slice(b"\" height=\"");
    out.extend_from_slice(asset.height.to_string().as_bytes());
    out.extend_from_slice(b"\">");
}

/// Append what an anchor with no asset becomes in HTML.
///
/// U+FFFC as a numeric character reference, which is what the character *is*: a placeholder for a picture
/// that is not there. Emitting nothing would silently shorten the document; emitting a literal U+FFFC is
/// correct but unreadable in a diff.
pub fn html_missing_into(out: &mut Vec<u8>) {
    out.extend_from_slice(b"&#xfffc;");
}

/// The `alt` text for an image in a PDF.
///
/// Empty, because the catalog carries no caption and inventing one would put words in the author's
/// document. `/Alt` is required on a `/Figure` structure and an omitted one is an accessibility
/// complaint from every reader's checker.
pub const PDF_ALT: &[u8] = b"";

/// The first anchor in `bytes`, and how many trailing bytes might be a partial one.
///
/// Both exporters walk the document in chunks and the anchor is three bytes, so it can straddle a
/// boundary in the middle of the page. A scanner that did not report the partial tail would either miss
/// anchors or emit half of one, and both look like a document that quietly lost its pictures.
///
/// `partial` is a count, not a bool, because the exporter needs to know how many bytes to hold back.
#[must_use]
pub fn find_anchor(bytes: &[u8]) -> (Option<usize>, usize) {
    let mut found: Option<usize> = None;
    let mut partial = 0usize;
    for i in 0..bytes.len() {
        if bytes[i] != ANCHOR_BYTES[0] {
            continue;
        }
        let have = bytes.len() - i;
        if have >= ANCHOR_BYTES.len() {
            if bytes[i + 1] == ANCHOR_BYTES[1] && bytes[i + 2] == ANCHOR_BYTES[2] {
                found = found.or(Some(i));
            }
        } else {
            // One or two lead bytes with the rest of the anchor past the end: always a candidate.
            partial = partial.max(have);
        }
    }
    (found, partial)
}

/// The catalog entry for the `ordinal`-th anchor, or `None`.
///
/// One function because both exporters ask this at several places and each of them wants the same two
/// answers; four copies of it would be four slightly different error paths.
#[must_use]
pub fn asset_at(catalog: &AssetCatalog, ordinal: usize) -> Option<&Asset> {
    catalog.get(ordinal).ok()
}

/// Decode `asset`'s PNG to RGB triplets, dropping alpha.
///
/// # Why alpha is dropped and not carried as an `/SMask`
///
/// An `/SMask` is the correct representation and it is not written. The reason is a budget one and it
/// should be stated rather than discovered: an SMask doubles the sample bytes and needs a second stream,
/// and §2.9.1's decoder budget has nothing in it for a second stream. **Dropping alpha composites the
/// picture onto white**, which is what a reader does with a transparent PNG in a document anyway.
///
/// So a transparent PNG exports as its colours over white rather than over the page. That is a real
/// limitation, it is stated here, and it is the first thing to change if a document ever needs a
/// transparent image in a PDF.
pub fn decode_to_rgb(asset: &Asset) -> Result<(u32, u32, Vec<u8>), crate::ExportError> {
    use crate::ExportError;
    let header = holonomy_image::read_header(asset.png.as_slice())
        .map_err(|e| ExportError::Asset(format!("{}: {}", asset.id, e)))?;
    let want = holonomy_image::decoded_len(&header);
    let mut rgba = vec![0u8; want];
    holonomy_image::decode(asset.png.as_slice(), &mut rgba)
        .map_err(|e| ExportError::Asset(format!("{}: {}", asset.id, e)))?;
    let n = (header.width as usize)
        .checked_mul(header.height as usize)
        .ok_or(ExportError::Asset("dimensions overflow".into()))?;
    let mut rgb = Vec::with_capacity(n * 3);
    // An index loop over `n` rather than `chunks_exact(4)`: the stride is a constant, and this way the
    // count is the same `n` the output was sized with, so the two cannot disagree.
    for i in 0..n {
        let p = &rgba[i * 4..i * 4 + 4];
        // Composite onto white: `c*a + 255*(255-a)`, integer, no float.
        let a = u32::from(p[3]);
        let mix = |c: u8| -> u8 { ((u32::from(c) * a + 255 * (255 - a)) / 255) as u8 };
        rgb.push(mix(p[0]));
        rgb.push(mix(p[1]));
        rgb.push(mix(p[2]));
    }
    Ok((header.width, header.height, rgb))
}
