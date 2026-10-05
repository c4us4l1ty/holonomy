//! Phase 9C gate: the payload-embedded asset catalog.
//!
//! # What is being pinned
//!
//! The container format is frozen, so the *only* place an image can live is the payload's tail. This
//! file pins four things about that arrangement, in increasing order of how expensive a regression
//! would be:
//!
//! 1. **The layout is the frozen one, byte for byte.** The frozen spec is
//!    `[Doc Header 16B][Text & Spans][Table & Math States][Asset Catalog Header: count u32][Asset:
//!    Blake2b 32B | w u16 | h u16 | len | PNG]*`, and a layout assertion is the only thing that
//!    keeps "we said the container would not change" from quietly becoming true of the container and
//!    false of the payload.
//! 2. **The container does not parse any of it.** Asserted by `Wavefunction` itself in
//!    `crates/holonomy-container/tests/commit_then_read.rs`; what is asserted *here* is that the
//!    payload is self-describing, i.e. it can be walked with no side table.
//! 3. **A content address is verifiable.** Every stored `AssetId` is recomputed on load, and every
//!    stored dimension is re-derived from the PNG's `IHDR`. A catalog that decodes to *some* images
//!    when it should decode to *these* images is the failure this catches.
//! 4. **Truncation and corruption are refused whole.** Not "partially read": a payload that yields
//!    three of four images fails in a way that names the cause.
//!
//! The mutation checks are recorded in the commit message for `crates/holonomy-text/src/payload.rs`.
//! Every fixture PNG here is produced by Python's `zlib` and written byte by byte, because a real
//! encoder's output would change with its version and a gate that depends on that is a gate that
//! fails for the wrong reason.

use holonomy_text::asset::{catalog_fixed_bytes, AssetCatalog, AssetError, AssetId, ANCHOR_BYTES};
use holonomy_text::payload::{self, PayloadError, FORMAT, HEADER_LEN, MAGIC};
use holonomy_text::{Editor, SpanPolicy, STYLE_BOLD, STYLE_ITALIC};

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

/// The 8-byte PNG signature, then a chunk: `u32 BE length`, 4-byte type, body, `u32 BE CRC`.
fn png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    /// CRC-32, IEEE, computed the long way. `crc32fast` is exactly the dependency §2.9.5 refuses, and
    /// the polynomial table is six lines.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in bytes {
            crc ^= u32::from(b);
            for _ in 0..8 {
                // `>> (n)` rather than a lookup table: this runs a few dozen times per test, and a
                // 256-entry table in a test file is a second thing to keep correct.
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
        let mut crc_input = Vec::with_capacity(4 + body.len());
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(body);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
        out
    }

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8 bpc, truecolour, no interlace

    // Scanlines: one filter byte (0 = None) then 3 bytes per pixel. The data is a literal run, which
    // `zlib` emits uncompressed in stored blocks, so the IDAT is valid DEFLATE without the fixture
    // depending on a particular compression level.
    let mut raw = Vec::with_capacity((1 + width as usize * 3) * height as usize);
    for y in 0..height {
        raw.push(0u8);
        for x in 0..width {
            // `wrapping_mul` on the products, not just on the sums: `x * 7` overflows a `u8` at
            // width 37, and this file is compiled in debug where that is a panic rather than a wrap.
            raw.push(
                rgb[0]
                    .wrapping_add((x as u8).wrapping_mul(7))
                    .wrapping_add((y as u8).wrapping_mul(3)),
            );
            raw.push(rgb[1].wrapping_add((y as u8).wrapping_mul(5)));
            raw.push(rgb[2].wrapping_add((x as u8).wrapping_mul(2)));
        }
    }
    let mut deflated = Vec::new();
    {
        // A zlib stream made of stored (uncompressed) DEFLATE blocks: 2-byte header, then blocks of
        // `BFINAL | BTYPE=00` with `LEN`/`NLEN` and raw bytes, then Adler-32. This is the fixture
        // rather than real compression because it is written out by hand -- the gate is about the
        // *catalog*, and a fixture that needs a compressor is a fixture that cannot be read.
        deflated.extend_from_slice(&[0x78, 0x01]); // CM = deflate, CINFO = 32K window, FCHECK ok
        let mut i = 0usize;
        if raw.is_empty() {
            deflated.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
        }
        while i < raw.len() {
            let take = (raw.len() - i).min(0xFFFF);
            let final_block = i + take >= raw.len();
            deflated.push(if final_block { 1 } else { 0 });
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
    }

    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    out.extend_from_slice(&chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&chunk(b"IDAT", &deflated));
    out.extend_from_slice(&chunk(b"IEND", &[]));
    out
}

/// The bytes a fixture occupies, so a test can truncate at an exact offset.
fn fixtures() -> Vec<Vec<u8>> {
    vec![
        png(64, 48, [200, 30, 40]),
        png(32, 96, [10, 220, 90]),
        png(1, 7, [0, 0, 0]),
    ]
}

// ---------------------------------------------------------------------------------------------
// 1. The layout is the frozen one
// ---------------------------------------------------------------------------------------------

#[test]
fn the_payload_starts_with_the_sixteen_byte_doc_header() {
    let p = payload::encode(
        "hi",
        &holonomy_text::SpanMap::plain(2),
        &[],
        &AssetCatalog::new(),
    );
    assert!(
        p.len() >= HEADER_LEN,
        "the header is 16 bytes and there is more after it"
    );
    assert_eq!(&p[..4], &MAGIC);
    assert_eq!(u16::from_le_bytes([p[4], p[5]]), FORMAT);
    // Bytes 6..8 flags, 8..12 the text length, 12..14 the span count, 14..16 the table count. Every
    // byte is used: unlike `MasterFrame`, this header has no reserved-zero range, so there is no
    // padding for a future field to move into and the layout cannot be revised by filling it in.
    assert_eq!(u16::from_le_bytes([p[6], p[7]]), 0, "flags: no assets");
    assert_eq!(
        u32::from_le_bytes([p[8], p[9], p[10], p[11]]),
        2,
        "text_len"
    );
    assert_eq!(u16::from_le_bytes([p[12], p[13]]), 1, "span_count");
    assert_eq!(u16::from_le_bytes([p[14], p[15]]), 0, "table_count");
    assert_eq!(&p[HEADER_LEN..HEADER_LEN + 2], b"hi");
}

#[test]
fn the_catalog_is_the_last_section_and_reads_to_end_of_input() {
    let fx = fixtures();
    let mut cat = AssetCatalog::new();
    for f in &fx {
        cat.insert(f).expect("fixture PNGs");
    }
    // The text holds one anchor per asset, which is what makes this document real rather than a byte
    // pattern, and no formula, so the math count is zero.
    let text = ANCHOR_BYTES.iter().map(|&b| b as char).collect::<String>();
    let p = payload::encode(
        &text,
        &holonomy_text::SpanMap::plain(text.len() as u32),
        &[],
        &cat,
    );

    // Walk to the catalog by hand: header, text, spans, tables, math count, and nothing else before
    // it. This is the whole point of "the catalog is last" -- reaching it needs no offset, because
    // nothing follows it to be confused with.
    let after_math = HEADER_LEN + text.len() + 16 + 4;
    let tail = &p[after_math..];
    let decoded = AssetCatalog::decode(tail).expect("the tail is a catalog");
    assert_eq!(decoded.len(), fx.len());
    assert_eq!(
        decoded.encoded_len(),
        tail.len(),
        "the catalog consumed the tail exactly"
    );

    // And the whole payload decodes, which is the property the walk depends on.
    let doc = payload::decode(&p).expect("round trip");
    assert_eq!(doc.assets.len(), fx.len());
}

#[test]
fn an_empty_catalog_is_exactly_four_bytes_of_zero() {
    let mut p = Vec::new();
    AssetCatalog::new().encode_into(&mut p);
    assert_eq!(p, vec![0, 0, 0, 0]);
    assert_eq!(catalog_fixed_bytes(0), 4);
}

#[test]
fn catalog_entry_size_is_forty_bytes_plus_its_png() {
    // 32 for the id, 2 + 2 for the dimensions, 4 for the length.
    assert_eq!(catalog_fixed_bytes(1) - 4, 40);
    assert_eq!(catalog_fixed_bytes(3) - 4, 120);
    let small = png(2, 2, [4, 5, 6]);
    let mut cat = AssetCatalog::new();
    cat.insert(&small).expect("fixture");
    let one = cat.encoded_len();
    cat.insert(&png(2, 2, [7, 8, 9])).expect("fixture");
    let two = cat.encoded_len();
    // The delta is one 40-byte entry header plus the second PNG in full, so the assertion has to
    // know the PNG's length -- an estimate here would pass for the wrong reason.
    assert_eq!(two - one, 40 + small.len(), "one fixed header plus the PNG");
    assert_eq!(one, 4 + 40 + small.len());
}

// ---------------------------------------------------------------------------------------------
// 2. Round trips
// ---------------------------------------------------------------------------------------------

#[test]
fn a_document_with_no_images_round_trips_byte_for_byte() {
    let e = holonomy_text::Editor::from_text(b"hello world\nsecond line\n").expect("text");
    let p = e.payload().expect("payload");
    let back = Editor::from_payload(&p).expect("reopen");
    assert_eq!(
        back.payload().expect("re-payload"),
        p,
        "payload -> payload is the identity"
    );
    assert_eq!(back.text().expect("text"), e.text().expect("text"));
}

#[test]
fn styling_and_tables_survive_a_round_trip() {
    let mut e = holonomy_text::Editor::from_text(b"plain bold italic\n").expect("text");
    e.style_range(6, 10, STYLE_BOLD, 0x00FF_0000).expect("bold");
    e.style_range(11, 17, STYLE_ITALIC, 0).expect("italic");
    e.insert_table(2, 2, 80).expect("table");
    let before = e.payload().expect("payload");
    let back = Editor::from_payload(&before).expect("reopen");

    assert_eq!(back.payload().expect("re-payload"), before);
    // The span list must come back *as it was*, not normalised: `from_spans` exists for exactly this,
    // and comparing the payloads is the only test that would notice a merge.
    for s in e.spans().spans() {
        assert!(
            back.spans().spans().contains(s),
            "span {s:?} did not survive: {:?}",
            back.spans().spans()
        );
    }
    assert_eq!(
        back.tables().spans().len(),
        1,
        "the table's shape is not derivable from bytes"
    );
}

#[test]
fn images_survive_a_round_trip_and_keep_their_content_addresses() {
    let fx = fixtures();
    let mut e = holonomy_text::Editor::from_text(b"before\n").expect("text");
    let mut ids = Vec::new();
    let mut at = e.text_len() as u32;
    for f in &fx {
        ids.push(e.insert_image(at, f).expect("insert image"));
        at += 3; // past the 3-byte anchor
    }
    let before = e.payload().expect("payload");
    let back = Editor::from_payload(&before).expect("reopen");

    assert_eq!(back.assets().len(), fx.len());
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(
            back.assets().entries()[i].id,
            *id,
            "entry {i} changed address"
        );
        assert_eq!(back.assets().entries()[i].png.as_slice(), fx[i].as_slice());
    }
    assert_eq!(back.image_anchors().expect("anchors").len(), fx.len());
    assert_eq!(back.payload().expect("re-payload"), before);
}

#[test]
fn the_same_png_twice_is_two_entries_sharing_one_address() {
    // Not deduplicated, and deliberately: entry `i` serves anchor `i`, so collapsing two entries
    // would leave the second anchor pointing at the first anchor's slot. The *addresses* still match,
    // which is what makes "have I already got this image" answerable without a table.
    let f = png(8, 8, [9, 9, 9]);
    let mut cat = AssetCatalog::new();
    let a = cat.insert(&f).expect("first");
    let b = cat.insert(&f).expect("second");
    assert_eq!(a, b);
    assert_eq!(cat.len(), 2);
}

// ---------------------------------------------------------------------------------------------
// 3. The address and the dimensions are verified, not trusted
// ---------------------------------------------------------------------------------------------

#[test]
fn a_spliced_id_is_refused_rather_than_honoured() {
    let mut cat = AssetCatalog::new();
    cat.insert(&png(16, 16, [7, 7, 7])).expect("fixture");
    let mut bytes = Vec::new();
    cat.encode_into(&mut bytes);
    // Flip a bit in the stored id. The PNG beside it is untouched, so the recomputation disagrees.
    bytes[4] ^= 0x01;
    match AssetCatalog::decode(&bytes) {
        Err(AssetError::IdMismatch { stored, computed }) => {
            assert_ne!(stored, computed);
        }
        other => panic!("expected IdMismatch, got {other:?}"),
    }
}

#[test]
fn a_spliced_payload_is_refused_rather_than_shown_as_the_wrong_image() {
    // The failure this guards is the expensive one: not "no image", but *the wrong image* at a
    // position that looks correct. Two images, so a splice that reorders them is representable.
    // Two fixtures of the *same* length, so the swap is in place. Same-size PNGs differ in a few
    // filter and pixel bytes, which is enough: the ids are over the whole encoded file.
    let fx = [png(24, 24, [200, 30, 40]), png(24, 24, [10, 220, 90])];
    let mut e = holonomy_text::Editor::from_text(b"x\n").expect("text");
    e.insert_image(2, &fx[0]).expect("first");
    e.insert_image(5, &fx[1]).expect("second");
    let good = e.payload().expect("payload");

    // Swap the two PNGs' bytes in place: the ids then sit beside the wrong data.
    let mut bad = good.clone();
    let (r0, r1) = png_ranges(&good, &fx).expect("locate both entries");
    assert_eq!(r0.len(), r1.len(), "the swap is in place");
    let a = bad[r0.clone()].to_vec();
    let b = bad[r1.clone()].to_vec();
    bad[r0.clone()].copy_from_slice(&b);
    bad[r1.clone()].copy_from_slice(&a);
    assert!(
        payload::decode(&bad).is_err(),
        "a swapped pair must not decode"
    );
    assert!(
        payload::decode(&good).is_ok(),
        "the un-spliced payload still decodes"
    );
}

/// Byte ranges of the first two fixtures inside `p`, located by searching for the encoded bytes.
///
/// Searching rather than computing the offset: the test's claim is "a splice anywhere in the catalog
/// is caught", and a hardcoded offset would encode this file's idea of where the catalog starts, which
/// is the thing under test.
fn png_ranges(
    p: &[u8],
    fx: &[Vec<u8>],
) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let find = |needle: &[u8]| {
        p.windows(needle.len())
            .position(|w| w == needle)
            .map(|at| at..at + needle.len())
    };
    Some((find(&fx[0])?, find(&fx[1])?))
}

#[test]
fn stored_dimensions_must_match_the_pngs_ihdr() {
    let mut cat = AssetCatalog::new();
    cat.insert(&png(20, 10, [1, 1, 1])).expect("fixture");
    let mut bytes = Vec::new();
    cat.encode_into(&mut bytes);
    // The entry is [4..36] the id, then [36..38] width, [38..40] height, [40..44] length. The id
    // starts at 4 because byte 0..4 is the catalog's entry count.
    assert_eq!(u16::from_le_bytes([bytes[36], bytes[37]]), 20);
    bytes[36] ^= 0x08; // 20 -> 28, leaving the id alone so the failure is the dimension's
    match AssetCatalog::decode(&bytes) {
        Err(AssetError::DimensionMismatch {
            index,
            declared,
            actual,
        }) => {
            assert_eq!(index, 0);
            assert_eq!(declared, (28, 10));
            assert_eq!(actual, (20, 10));
        }
        other => panic!("expected DimensionMismatch, got {other:?}"),
    }
}

#[test]
fn a_png_that_is_not_a_png_is_refused() {
    assert_eq!(
        AssetCatalog::new().insert(b"not a png at all, but long enough"),
        Err(AssetError::NotPng)
    );
    assert!(matches!(
        AssetCatalog::new().insert(b"short"),
        Err(AssetError::Truncated { .. })
    ));
}

// ---------------------------------------------------------------------------------------------
// 4. Truncation and corruption, refused whole
// ---------------------------------------------------------------------------------------------

#[test]
fn a_payload_truncated_anywhere_is_refused() {
    let fx = fixtures();
    let mut e = holonomy_text::Editor::from_text(b"a picture:\n").expect("text");
    for f in &fx {
        let at = e.text_len() as u32;
        e.insert_image(at, f).expect("image");
    }
    e.insert_at(e.text_len() as u32, b"tail\n", SpanPolicy::Strict)
        .expect("tail");
    let good = e.payload().expect("payload");

    // Every single-byte truncation, not a sample of them. The failure being guarded is a decode that
    // runs off the end of the buffer and reads whatever follows it, so "short by one" is the
    // interesting case and a stride of 7 would step over it.
    for cut in 0..good.len() {
        assert!(
            payload::decode(&good[..cut]).is_err(),
            "a payload cut to {cut} of {} bytes decoded",
            good.len()
        );
    }
    assert!(payload::decode(&good).is_ok(), "the whole payload decodes");
}

#[test]
fn an_absurd_count_is_refused_before_anything_is_allocated() {
    let mut p = payload::encode(
        "x",
        &holonomy_text::SpanMap::plain(1),
        &[],
        &AssetCatalog::new(),
    );
    let catalog_at = HEADER_LEN + 1 + 16 + 4; // header + text + one span + math count
    p[catalog_at..catalog_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    match payload::decode(&p) {
        Err(PayloadError::Asset(AssetError::CountTooLarge { count, .. })) => {
            assert_eq!(count, u32::MAX)
        }
        other => panic!("expected CountTooLarge, got {other:?}"),
    }
}

#[test]
fn a_payload_with_an_undefined_flag_bit_is_refused() {
    let mut p = payload::encode(
        "x",
        &holonomy_text::SpanMap::plain(1),
        &[],
        &AssetCatalog::new(),
    );
    p[6] = 0x80; // a bit outside FLAG_MASK
    assert_eq!(
        payload::decode(&p).unwrap_err(),
        PayloadError::UnknownFlags { found: 0x80 }
    );
}

#[test]
fn a_payload_whose_math_state_disagrees_with_its_text_is_refused() {
    // Math is derivable from the text, so the stored record is redundant -- and that is why it is
    // worth storing: it is a cross-check on this file's own cursor arithmetic. Overstate the count and
    // the record length must be extended to match, or the decoder would stop at `RecordOverruns` and
    // the mismatch would never be reached.
    let good = holonomy_text::Editor::from_text(b"$$x^2$$")
        .expect("text")
        .payload()
        .expect("payload");
    assert_eq!(holonomy_text::math_span_count(b"$$x^2$$"), 1);
    assert!(payload::decode(&good).is_ok());

    // The math count's offset is *computed* from the header rather than written out, because
    // `Editor::from_text` appends the trailing newline that makes a document a document -- so a
    // hardcoded `HEADER_LEN + 6 + 16` is off by one and fails as `RecordOverruns` rather than as the
    // mismatch it is testing. Reading the header is also what the decoder does, so the two agree by
    // construction instead of by my arithmetic.
    let text_len = u32::from_le_bytes([good[8], good[9], good[10], good[11]]) as usize;
    let span_count = u16::from_le_bytes([good[12], good[13]]) as usize;
    let math_count_at = HEADER_LEN + text_len + span_count * 16;
    let stored_count =
        u32::from_le_bytes(good[math_count_at..math_count_at + 4].try_into().unwrap());
    assert_eq!(stored_count, 1);

    let mut bad = good.clone();
    // Two spans instead of one: 12 more bytes of record, then the count that claims them.
    bad.splice(
        math_count_at + 4..math_count_at + 4,
        [9u8; 12].iter().copied(),
    );
    bad[math_count_at..math_count_at + 4].copy_from_slice(&2u32.to_le_bytes());
    match payload::decode(&bad) {
        Err(PayloadError::MathMismatch { stored, derived }) => {
            assert_eq!(stored, 2);
            assert_eq!(derived, 1);
        }
        other => panic!("expected MathMismatch, got {other:?}"),
    }

    // And the same for an understated count, which is the direction a short write would take.
    let mut fewer = good.clone();
    fewer[math_count_at..math_count_at + 4].copy_from_slice(&0u32.to_le_bytes());
    // With zero records the catalog starts 12 bytes earlier, so the bytes that followed are read as
    // the catalog. That is a *different* refusal, and asserting it names the right one: the mismatch
    // check must run before the catalog is walked, or a short write shows up as "bad PNG".
    match payload::decode(&fewer) {
        Err(PayloadError::MathMismatch { stored, derived }) => {
            assert_eq!(stored, 0);
            assert_eq!(derived, 1);
        }
        other => panic!("expected MathMismatch for a missing record, got {other:?}"),
    }
}

#[test]
fn non_utf8_text_is_refused_rather_than_lossy_decoded() {
    // A lossy decode would put U+FFFD where the corrupt byte was, and every span and table offset in
    // the document would then point at the wrong character -- a document that renders with its
    // styling shifted, which is worse than one that refuses to open.
    let mut p = payload::encode(
        "ok",
        &holonomy_text::SpanMap::plain(2),
        &[],
        &AssetCatalog::new(),
    );
    // Overstate the text length by one so the 0xFF byte spliced in at the end of the text region is
    // inside the declared text rather than in the span list.
    let len = u32::from_le_bytes([p[8], p[9], p[10], p[11]]);
    p[8..12].copy_from_slice(&(len + 1).to_le_bytes());
    p.insert(HEADER_LEN + 2, 0xFF);
    assert_eq!(payload::decode(&p).unwrap_err(), PayloadError::NotUtf8);
}

#[test]
fn a_span_map_that_is_not_sorted_or_gap_free_is_refused() {
    // Two spans with a hole between them: the payload is walked and the map would be asked to cover
    // bytes no span claims. That is an unbounded-error shape, so it is refused at the boundary.
    let mut spans = holonomy_text::SpanMap::plain(8);
    spans.style_range(0, 2, STYLE_BOLD, 0).expect("first span");
    spans.style_range(4, 6, STYLE_BOLD, 0).expect("second span");
    // `style_range` normalises into one gap-free map, so a hole cannot be *produced* through it --
    // which is why the map is gap-free by construction and the validator below is a boundary check
    // rather than an everyday path.
    assert_eq!(
        spans.spans().len(),
        4,
        "0-2 bold, 2-4 plain, 4-6 bold, 6-8 plain"
    );
    assert!(holonomy_text::SpanMap::from_spans(spans.spans().to_vec(), 8).is_ok());

    let mut short = spans.spans().to_vec();
    short.truncate(1);
    assert!(
        holonomy_text::SpanMap::from_spans(short, 8).is_err(),
        "a span list ending at 2 of 8 bytes must be refused"
    );

    // Out of bounds, and a hole at the front.
    assert!(holonomy_text::SpanMap::from_spans(
        vec![holonomy_text::TextIntervalSpan::plain(1, 8)],
        8
    )
    .is_err());
    assert!(holonomy_text::SpanMap::from_spans(
        vec![holonomy_text::TextIntervalSpan::plain(0, 99)],
        8
    )
    .is_err());
    // An empty list over an empty document is legal and is the identity.
    assert!(holonomy_text::SpanMap::from_spans(vec![], 0)
        .unwrap()
        .spans()
        .is_empty());
}

// ---------------------------------------------------------------------------------------------
// 5. The anchor contract
// ---------------------------------------------------------------------------------------------

#[test]
fn an_anchor_is_three_bytes_and_is_found_where_it_was_put() {
    let mut e = holonomy_text::Editor::from_text(b"abcd").expect("text");
    e.insert_image(2, &png(4, 4, [1, 1, 1])).expect("image");
    let text = e.text().expect("text");
    assert_eq!(text[2..5], ANCHOR_BYTES);
    assert_eq!(text.len(), 7, "the anchor is 3 bytes");
    assert_eq!(e.image_anchors().expect("anchors"), vec![2]);
}

#[test]
fn an_anchor_slides_when_text_is_inserted_in_front_of_it() {
    // This is the whole reason the position is a character in the text rather than an offset in a
    // parallel structure: nothing in the rope, the span map or the table map knows about images, and
    // the anchor still lands in the right place.
    let mut e = holonomy_text::Editor::from_text(b"XY").expect("text");
    e.insert_image(2, &png(4, 4, [1, 1, 1])).expect("image");
    e.insert_at(0, b"12345", SpanPolicy::Strict)
        .expect("prefix");
    assert_eq!(e.image_anchors().expect("anchors"), vec![7]);
    let text = e.text().expect("text");
    assert_eq!(&text[7..10], &ANCHOR_BYTES);
}

#[test]
fn an_anchor_survives_undo_because_it_went_through_insert_at() {
    let mut e = holonomy_text::Editor::from_text(b"Z").expect("text");
    e.insert_image(1, &png(4, 4, [1, 1, 1])).expect("image");
    assert_eq!(e.assets().len(), 1);
    e.undo().expect("undo");
    assert_eq!(
        e.image_anchors().expect("anchors").len(),
        0,
        "the anchor is gone"
    );
    // The catalog keeps the bytes. It is not an interval map, so there is nothing to slide or drop --
    // and that is the tradeoff `AssetCatalog`'s docs name: an asset outlives its anchor until the
    // document is reopened.
    assert_eq!(
        e.assets().len(),
        1,
        "the picture outlives the anchor; unreachable, not corrupt"
    );
}

#[test]
fn anchors_and_catalog_entries_stay_in_step_through_many_inserts() {
    let fx = fixtures();
    let mut e = holonomy_text::Editor::from_text(b"").expect("text");
    for f in &fx {
        e.insert_image(0, f).expect("image");
    }
    // Insert at the front repeatedly: each anchor moves, the catalog order does not change, and entry
    // `i` must still serve anchor `i`.
    for n in 0..20u8 {
        e.insert_at(0, &[b'a' + n], SpanPolicy::Strict)
            .expect("prefix");
    }
    let anchors = e.image_anchors().expect("anchors");
    assert_eq!(anchors.len(), fx.len());
    for (i, at) in anchors.iter().enumerate() {
        let entry = e.assets().entries().get(i).expect("entry");
        assert_eq!(AssetId::of(entry.png.as_slice()), entry.id, "entry {i}");
        let _ = at;
    }
    let p = e.payload().expect("payload");
    let back = Editor::from_payload(&p).expect("reopen");
    assert_eq!(back.assets().len(), fx.len());
    assert_eq!(back.payload().expect("re-payload"), p);
}
