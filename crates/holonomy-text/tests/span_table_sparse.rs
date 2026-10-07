//! **The correction to Phase 13 part 6: a document's styling is *stored*, not discovered.**
//! 6 tests.
//!
//! # What part 6 got wrong
//!
//! Part 6 was written on the premise that a container-loaded document's styling had to be **computed
//! per leaf** as leaves faulted in — that a leaf's styling would be scanned for on arrival, and
//! `SpanMap::observe` would receive those findings. That premise is wrong.
//!
//! [`payload::encode`] writes the span list into the payload as 16-byte records immediately after the
//! text, and the header declares the count. So the styling is **stored, authoritative, and at a
//! computable offset** — `HEADER_LEN + text_len` — and can be read without the text ever being
//! resident.
//!
//! That inverts the cost. Styling is not O(document bytes), it is O(styled runs): one canonical plain
//! span for an unstyled document, and at most 65,535 × 16 ≈ 1 MiB at the `u16` cap. **The text is what
//! has to be windowed. The span table does not.**
//!
//! | what it proves | test |
//! | --- | --- |
//! | **styling survives the text's absence** | [`styling_is_recoverable_with_the_text_absent`] |
//! | the offset is where the layout says | [`the_span_table_is_where_the_layout_says_it_is`] |
//! | both paths agree byte for byte | [`the_sparse_path_and_the_whole_payload_path_agree`] |
//! | a short table is refused | [`a_truncated_span_table_is_refused_not_padded`] |
//! | a loaded map is fully *read* | [`a_map_from_a_payload_is_fully_read_not_partly`] |
//! | and the cost is in runs, not bytes | [`the_span_table_costs_runs_not_document_bytes`] |

use holonomy_text::asset::AssetCatalog;
use holonomy_text::payload::{self, Header, PayloadError};
use holonomy_text::{SpanMap, STYLE_BOLD, STYLE_HEADER};

/// A document with deliberate styling, plus the payload it encodes to.
///
/// **The text is built first and the map sized to it afterwards**, because `SpanMap::new()` is a map
/// over zero bytes and `style_range` rightly refuses to paint outside its length. Sizing the map to
/// the finished text is also what `payload::decode` does, so the fixture matches the load path rather
/// than inventing a way to build a map the load path cannot produce.
fn styled_document() -> (String, SpanMap, Vec<u8>) {
    let text = "# A heading\n\
                This line is bold and the rest is not.\n\
                A second plain paragraph, long enough to have a distinct tail.\n"
        .to_string();

    // "# A heading\n" is 11 bytes; the bold line runs from there to the next newline.
    let h0 = 0u32;
    let h1 = 11u32;
    let b1 = text.find("This line").unwrap() as u32 + "This line is bold and the rest is not.".len() as u32;

    let mut spans = SpanMap::empty_over(text.len() as u32);
    spans.style_range(h0, h1, STYLE_HEADER, 0x00FF_0000).unwrap();
    spans.style_range(h1, b1, STYLE_BOLD, 0).unwrap();

    let bytes = payload::encode(&text, &spans, &[], &AssetCatalog::new());
    (text, spans, bytes)
}

/// **The load-bearing claim.** A document's styling is recovered from a payload **with the text
/// absent** — the header and the span table, and nothing between them.
///
/// This is what makes a sparse editor's styling correct from the first paint rather than filling in
/// as the user scrolls, and it is the claim part 6 got wrong.
#[test]
fn styling_is_recoverable_with_the_text_absent() {
    let (text, original, bytes) = styled_document();

    // Read the header. In the product this is the first section, which any window at offset 0 holds.
    let header = Header::parse(&bytes).expect("a header");
    assert_eq!(header.text_len, text.len() as u32, "the header declares the text length");

    // Read the span table **directly**, skipping the text entirely.
    let at = usize::try_from(header.span_table_offset()).unwrap();
    let table = &bytes[at..at + usize::try_from(header.span_table_bytes()).unwrap()];
    assert!(
        header.span_table_bytes() > 0,
        "and there is a span table to read -- otherwise this test proves nothing about styled bytes"
    );
    assert!(table.len() < text.len(), "the table is smaller than the text it describes");

    let recovered = SpanMap::from_spans(
        payload::read_span_table(&header, table).expect("the span table"),
        text.len() as u32,
    )
    .expect("a valid map");

    // Byte-for-byte the same styling, from the same document, **without the text present**.
    assert_eq!(recovered.spans(), original.spans(), "identical spans");
    assert_eq!(recovered.style_at(0).style_flags, STYLE_HEADER, "the heading is still a heading");
    // "# A heading\n" is 11 bytes, so the bold run starts there.
    assert_eq!(recovered.style_at(11).style_flags, STYLE_BOLD, "and so is the bold run");
}

/// **The offset formula is load-bearing, so it is checked rather than assumed.** If
/// `HEADER_LEN + text_len` were wrong by one byte the table would be garbage that happens to parse,
/// which is the worst kind of wrong.
#[test]
fn the_span_table_is_where_the_layout_says_it_is() {
    let (text, _, bytes) = styled_document();
    let header = Header::parse(&bytes).unwrap();
    let at = usize::try_from(header.span_table_offset()).unwrap();

    // The declared offset is exactly one past the last text byte.
    assert_eq!(at, payload::HEADER_LEN + text.len(), "header + text");
    assert_eq!(bytes[at - 1], *text.as_bytes().last().unwrap(), "so the byte before it is the last text byte");

    // And the first record begins there: its `start_byte` is 0, because encode sorts.
    let first = &bytes[at..at + 16];
    assert_eq!(u32::from_le_bytes([first[0], first[1], first[2], first[3]]), 0, "first span starts at 0");

    // A text length that differs would move the table, so the formula is not a constant offset.
    let mut longer = String::from("x");
    longer.push_str(&text);
    let m = SpanMap::plain(longer.len() as u32);
    let bytes2 = payload::encode(&longer, &m, &[], &AssetCatalog::new());
    let h2 = Header::parse(&bytes2).unwrap();
    assert_eq!(h2.span_table_offset(), payload::HEADER_LEN as u64 + longer.len() as u64);
    assert_eq!(h2.span_table_offset(), at as u64 + 1, "one more byte of text, one byte further in");
}

/// **The sparse path and the whole-payload path must produce the same map.** Two copies of a record
/// parser is one copy too many: they would agree until someone changed a field offset in one of
/// them, and the symptom would be styling that differs between opening a document fully and opening
/// it scrolled.
#[test]
fn the_sparse_path_and_the_whole_payload_path_agree() {
    let (_, _, bytes) = styled_document();
    let header = Header::parse(&bytes).unwrap();
    let at = usize::try_from(header.span_table_offset()).unwrap();

    let sparse = payload::read_span_table(&header, &bytes[at..]).expect("sparse");
    let whole = payload::decode(&bytes).expect("whole").spans;
    assert_eq!(sparse.as_slice(), whole.spans(), "the same records, from the same bytes");
    assert_eq!(sparse.len(), header.span_count as usize, "and as many as the header declared");
}

/// **A short table is refused, not padded.** Returning the spans that did arrive would style part of
/// the document and leave the rest plain with no error anywhere — and styling that varies by position
/// in a file is not something a reader would notice.
#[test]
fn a_truncated_span_table_is_refused_not_padded() {
    let (_, _, bytes) = styled_document();
    let header = Header::parse(&bytes).unwrap();
    let at = usize::try_from(header.span_table_offset()).unwrap();
    let need = usize::try_from(header.span_table_bytes()).unwrap();
    assert!(need >= 32, "more than one record, so dropping one is a real truncation");

    let err = payload::read_span_table(&header, &bytes[at..at + need - 16])
        .expect_err("one record short must be refused");
    assert!(matches!(err, PayloadError::RecordOverruns { .. }), "got {err:?}");

    // And nothing at all is refused the same way, rather than yielding an empty list.
    let none = payload::read_span_table(&header, &[]).expect_err("no records at all");
    assert!(matches!(none, PayloadError::RecordOverruns { .. }), "got {none:?}");
}

/// **A map loaded from a payload is fully *read*, and this is a correction to part 6.**
///
/// `from_spans` used to set `read_through` to 0. That made every span read from disk report as "not
/// yet read" — the one thing that is definitely false about it — and would have sent the paint path
/// hunting for a fault that cannot happen.
#[test]
fn a_map_from_a_payload_is_fully_read_not_partly() {
    let (text, _, bytes) = styled_document();
    let decoded = payload::decode(&bytes).expect("decode");
    assert_eq!(decoded.spans.read_through(), text.len() as u32, "the whole document is read");
    assert!(decoded.spans.is_read(0) && decoded.spans.is_read(text.len() as u32 - 1));
    assert!(
        decoded.spans.style_at_known(0).is_some(),
        "so a styled byte reports known styling rather than asking for a fault"
    );

    // And the contrast that makes the point: a skeleton map claims nothing.
    let skeleton = SpanMap::plain(text.len() as u32);
    assert_eq!(skeleton.read_through(), 0, "a skeleton has read nothing, however plain it claims");
}

/// **The span table's cost tracks styled runs, not document bytes** — which is why it does not need
/// windowing while the text does.
///
/// This is the arithmetic that justifies loading spans eagerly. At 8 MiB of text with three styled
/// runs the table is 48 bytes; even at the `u16` cap it is about 1 MiB, against a text an order of
/// magnitude larger.
#[test]
fn the_span_table_costs_runs_not_document_bytes() {
    let text = "x".repeat(8 * 1024 * 1024);
    let mut spans = SpanMap::plain(text.len() as u32);
    for i in 0..3u32 {
        let at = i * 1024;
        spans.style_range(at, at + 512, STYLE_BOLD, 0).unwrap();
    }
    let bytes = payload::encode(&text, &spans, &[], &AssetCatalog::new());
    let header = Header::parse(&bytes).unwrap();

    assert_eq!(header.text_len as usize, text.len(), "an 8 MiB document");
    assert_eq!(header.span_count as usize, 6, "four styled runs plus the two plain gaps between them");
    assert_eq!(header.span_table_bytes(), 96, "so 96 bytes of styling for 8 MiB of text");
    assert!(
        header.span_table_bytes() * 1000 < header.text_len as u64,
        "three orders of magnitude apart -- the text is what has to be windowed, and it is"
    );
}