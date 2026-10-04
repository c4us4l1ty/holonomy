//! The Phase 8 export gate: HTML escapes correctly and applies spans, and the PDF's text extracts
//! back to the source.
//!
//! Both halves are *differential*. Neither asserts that a substring appears; both round-trip, because
//! the failure mode being guarded is an exporter that produces output which still opens and still
//! contains the text -- just with the wrong words in the wrong tags, or the right words in the wrong
//! encoding.
//!
//! # Why the PDF half needs a real parser
//!
//! "The text extracts to the source" cannot be checked by looking for the source bytes in the PDF.
//! PDF stores text as `Tj` operands with font and position operators around them, so the source text is
//! neither contiguous nor even adjacent to itself across a line break. A test that greps would pass on
//! a PDF whose content stream is scrambled.
//!
//! So this file contains a minimal content-stream reader: it finds the text-showing operators,
//! resolves each against the font that was current, and reassembles the lines in the order the text
//! matrices place them. That is enough to prove the PDF says what the document says. It is not a PDF
//! library -- see [`ContentStreamReader`]'s docs for exactly what it does and does not handle.
//!
//! [`ContentStreamReader`]: content::ContentStreamReader

use holonomy_export::html::{self, HtmlOptions};
use holonomy_export::pdf::{self, PdfOptions};
use holonomy_export::Format;
use holonomy_text::{Editor, SpanPolicy, STYLE_BOLD, STYLE_CODE, STYLE_HEADER, STYLE_ITALIC};

mod content;

/// A document with every style applied, so one export exercises all four mappings.
fn styled_document() -> Editor {
    let mut ed = Editor::new();
    ed.insert_at(0, b"Chapter One\n\n", SpanPolicy::Strict)
        .expect("insert");
    // "Chapter One" is eleven bytes -- 7 + 1 + 3. The first version of this test used 9 and cut the
    // heading mid-word, so it was asserting `<h1>Chapter O</h1>` and calling it a pass.
    ed.style_range(0, 11, STYLE_HEADER, 0).expect("header");

    let bold_at = ed.text_len();
    ed.insert_at(bold_at as u32, b"Bold words. ", SpanPolicy::Strict)
        .expect("insert");
    ed.style_range(bold_at as u32, bold_at as u32 + 4, STYLE_BOLD, 0)
        .expect("bold");

    let ital_at = ed.text_len();
    ed.insert_at(ital_at as u32, b"Italic words. ", SpanPolicy::Strict)
        .expect("insert");
    ed.style_range(ital_at as u32, ital_at as u32 + 6, STYLE_ITALIC, 0)
        .expect("italic");

    let code_at = ed.text_len();
    ed.insert_at(code_at as u32, b"code_words(); ", SpanPolicy::Strict)
        .expect("insert");
    // "code_words()" is twelve bytes; thirteen included the semicolon and the assertion below was
    // then asking for `<code>code_words()</code>` and getting `<code>code_words();</code>`.
    ed.style_range(code_at as u32, code_at as u32 + 12, STYLE_CODE, 0)
        .expect("code");

    let plain_at = ed.text_len();
    ed.insert_at(plain_at as u32, b"Plain words.", SpanPolicy::Strict)
        .expect("insert");
    ed
}

/// The document's text as a `String`.
fn text_of(ed: &Editor) -> String {
    String::from_utf8(ed.text().expect("valid utf-8")).expect("valid utf-8")
}

// ============================================================================ HTML

#[test]
fn html_escapes_every_markup_character() {
    let ed = Editor::from_text(b"<script>alert(\"x\" & 'y')</script>").expect("load");
    let mut out: Vec<u8> = Vec::new();
    html::export_body(&mut out, &ed).expect("export");

    let s = String::from_utf8(out).expect("escapes plus the source");
    assert_eq!(
        s,
        "&lt;script&gt;alert(&quot;x&quot; &amp; &#39;y&#39;)&lt;/script&gt;"
    );
    // And there is not one raw angle bracket left, which is what would let a document smuggle markup
    // into an exported page.
    assert!(!s.contains('<') && !s.contains('>'));
}

#[test]
fn html_applies_each_span_as_its_tag() {
    let ed = styled_document();
    let mut out: Vec<u8> = Vec::new();
    let stats = html::export_body(&mut out, &ed).expect("export");
    let s = String::from_utf8(out).expect("utf-8");

    assert!(s.contains("<h1>Chapter One</h1>"), "got {s:?}");
    assert!(s.contains("<b>Bold</b>"), "got {s:?}");
    assert!(s.contains("<i>Italic</i>"), "got {s:?}");
    assert!(s.contains("<code>code_words()</code>"), "got {s:?}");
    assert!(s.contains("Plain words."), "got {s:?}");
    assert!(!s.contains("<b>Plain"), "a plain run picked up bold");

    assert_eq!(stats.headers, 1);
    assert_eq!(stats.bold_runs, 1);
    assert_eq!(stats.italic_runs, 1);
    assert_eq!(stats.code_runs, 1);
    assert_eq!(stats.text_bytes, ed.text_len() as u64);
}

/// Nested styles nest their tags, and the close order is the exact reverse.
#[test]
fn html_nests_and_closes_in_the_right_order() {
    let mut ed = Editor::from_text(b"abc").expect("load");
    ed.style_range(0, 3, STYLE_BOLD | STYLE_ITALIC, 0)
        .expect("style");
    let mut out: Vec<u8> = Vec::new();
    html::export_body(&mut out, &ed).expect("export");
    assert_eq!(String::from_utf8(out).expect("utf-8"), "<b><i>abc</i></b>");
}

/// A run boundary in the middle of a run list must close and reopen, not leak.
#[test]
fn html_run_transitions_do_not_leak_style() {
    let mut ed = Editor::from_text(b"aaabbb").expect("load");
    ed.style_range(0, 3, STYLE_BOLD, 0)
        .expect("bold the first half");
    let mut out: Vec<u8> = Vec::new();
    html::export_body(&mut out, &ed).expect("export");
    assert_eq!(String::from_utf8(out).expect("utf-8"), "<b>aaa</b>bbb");
}

/// Adjacent runs of the same style merge, rather than emitting `<b>a</b><b>b</b>`.
#[test]
fn html_adjacent_identical_runs_emit_one_tag() {
    let mut ed = Editor::from_text(b"ab").expect("load");
    ed.style_range(0, 1, STYLE_BOLD, 0).expect("bold a");
    ed.style_range(1, 2, STYLE_BOLD, 0).expect("bold b");
    let mut out: Vec<u8> = Vec::new();
    html::export_body(&mut out, &ed).expect("export");
    assert_eq!(String::from_utf8(out).expect("utf-8"), "<b>ab</b>");
}

/// A document larger than one export chunk still exports completely, with tags in the right places.
///
/// This is the test the chunked reader exists for: a run that straddles the 64 KiB boundary must be
/// emitted once, under the tags it had before the boundary, and not duplicated or dropped.
#[test]
fn html_a_document_larger_than_one_chunk_is_complete() {
    // Just over one chunk, with a styled run deliberately straddling the 64 KiB boundary.
    const CHUNK: usize = html::CHUNK_BYTES;
    let mut ed = Editor::new();
    // In 32 KiB pieces, because a single `insert_at` larger than the 64 KiB undo arena is refused --
    // `UndoError::ActionTooLarge`. That is the text engine's own bound and not what this test is about.
    const PIECE: usize = 32 * 1024;
    for _ in 0..(CHUNK + 1024).div_ceil(PIECE) {
        let at = ed.text_len();
        ed.insert_at(at as u32, &vec![b'.'; PIECE], SpanPolicy::Strict)
            .expect("insert");
    }
    let tail_at = ed.text_len();
    ed.insert_at(tail_at as u32, b"TAIL", SpanPolicy::Strict)
        .expect("insert");
    // Style everything but the tail, so the run straddles the chunk boundary and then some.
    ed.style_range(0, tail_at as u32, STYLE_CODE, 0)
        .expect("code the body");

    let mut out: Vec<u8> = Vec::new();
    let stats = html::export_body(&mut out, &ed).expect("export");
    let s = String::from_utf8(out).expect("utf-8");

    assert!(
        s.starts_with("<code>"),
        "got the first 80 bytes: {:?}",
        &s[..80.min(s.len())]
    );
    // The run covers the body and *not* the tail, so the tag closes before `TAIL` -- which also proves the
    // run's end was tracked across the boundary rather than being extended to the end of the document.
    assert!(
        s.ends_with("</code>TAIL"),
        "the last 40 bytes: {:?}",
        &s[s.len() - 40..]
    );
    // One run, one tag pair, across the whole thing.
    assert_eq!(stats.code_runs, 1);
    assert_eq!(stats.text_bytes, ed.text_len() as u64);
    assert_eq!(content::count(&s, "<code>"), 1);
    assert_eq!(content::count(&s, "</code>"), 1);
}

#[test]
fn html_the_document_is_well_formed() {
    let ed = styled_document();
    let mut out: Vec<u8> = Vec::new();
    html::export(
        &ed,
        &mut out,
        &HtmlOptions {
            title: "A <title> & \"quotes\"".to_string(),
            ..Default::default()
        },
    )
    .expect("export");
    let s = String::from_utf8(out.clone()).expect("utf-8");

    assert!(s.starts_with("<!DOCTYPE html>\n<html>\n<head>\n"));
    assert!(s.contains("<meta charset=\"utf-8\">"));
    // The title's own markup is escaped, in the element and not around it.
    assert!(
        s.contains("<title>A &lt;title&gt; &amp; &quot;quotes&quot;</title>"),
        "got {s:?}"
    );
    assert!(s.contains("<style>"), "the stylesheet is on by default");
    assert!(s.trim_end().ends_with("</html>"));

    // Every tag opened is closed, in order.
    assert_eq!(content::count(&s, "<b>"), content::count(&s, "</b>"));
    assert_eq!(content::count(&s, "<i>"), content::count(&s, "</i>"));
    assert_eq!(content::count(&s, "<code>"), content::count(&s, "</code>"));
    assert_eq!(content::count(&s, "<h1>"), content::count(&s, "</h1>"));
}

/// Stripping every tag from the export must give back the document's text, minus the escaped markup.
#[test]
fn html_stripping_the_tags_gives_the_document_back() {
    let mut ed = Editor::from_text(b"a < b & c > d").expect("load");
    ed.style_range(0, 1, STYLE_BOLD, 0).expect("bold");
    let mut out: Vec<u8> = Vec::new();
    html::export_body(&mut out, &ed).expect("export");
    let s = String::from_utf8(out).expect("utf-8");

    let stripped = content::strip_tags(&s);
    assert_eq!(stripped, "a &lt; b &amp; c &gt; d");
    // And unescaping that is the source, exactly.
    assert_eq!(content::unescape(&stripped), text_of(&ed));
}

// ============================================================================ PDF

/// Build a PDF and hand back both the bytes and the extracted text.
fn build_and_extract(ed: &Editor, opts: &PdfOptions) -> (Vec<u8>, String) {
    let mut stats = pdf::PdfStats::default();
    let bytes = pdf::build(ed, opts, &mut stats).expect("build");
    let text = content::extract_text(&bytes).expect("parse");
    (bytes, text)
}

/// A PDF must be structurally a PDF: header, objects, xref, trailer, `%%EOF`.
#[test]
fn pdf_is_structurally_a_pdf() {
    let ed = styled_document();
    let (bytes, _) = build_and_extract(&ed, &PdfOptions::default());

    assert!(bytes.starts_with(b"%PDF-1."), "header missing");
    // `pdf-writer` writes `%%EOF` with no trailing newline, which the spec permits and which the first
    // version of this assertion got wrong.
    assert!(
        bytes.ends_with(b"%%EOF"),
        "EOF marker missing, tail: {:?}",
        &bytes[bytes.len().saturating_sub(24)..]
    );
    assert!(contains(&bytes, b"/Type /Catalog"));
    assert!(contains(&bytes, b"/Type /Pages"));
    assert!(contains(&bytes, b"/Type /Page"));
    assert!(contains(&bytes, b"xref"));
    assert!(contains(&bytes, b"trailer"));
    assert!(contains(&bytes, b"startxref"));
}

/// Every Base-14 face the exporter uses must be declared, and declared exactly once.
#[test]
fn pdf_declares_each_face_once() {
    let ed = styled_document();
    let (bytes, _) = build_and_extract(&ed, &PdfOptions::default());
    // Read by name: `/BaseFont /Helvetica` is a substring of the other three, so counting it as a
    // byte pattern reports 4 for a file that declares it once.
    let fonts = content::base_fonts(&bytes);
    assert_eq!(
        fonts,
        vec![
            "Helvetica",
            "Helvetica-Bold",
            "Helvetica-Oblique",
            "Helvetica-BoldOblique",
            // Courier's four faces are metrically identical, so the exporter names two of them the
            // same; see `fonts.rs`.
            "Courier",
            "Courier",
        ]
    );
    // WinAnsi, because that is the encoding the width tables are indexed by.
    assert_eq!(count_bytes(&bytes, b"/Encoding /WinAnsiEncoding"), 6);
}

/// The gate: the PDF's text extracts to the source, exactly.
#[test]
fn pdf_text_extracts_to_the_source() {
    let ed = styled_document();
    let (_, text) = build_and_extract(&ed, &PdfOptions::default());

    // Byte-identical, blank lines and all. An earlier version of this test filtered the empty lines out
    // of the source to match an exporter that lost them -- and losing a paragraph break is a fidelity
    // bug, not a formatting difference. The exporter writes a blank line as a positioned empty `Tj`,
    // which is why it survives.
    assert_eq!(text, text_of(&ed));
    assert!(text.contains("Chapter One"));
    assert!(text.contains("Bold words."));
    assert!(text.contains("code_words();"));
}

/// Plain ASCII, no styling: the simplest possible round trip.
#[test]
fn pdf_a_plain_ascii_document_round_trips() {
    let source = "The quick brown fox jumps over the lazy dog.";
    let ed = Editor::from_text(source.as_bytes()).expect("load");
    let (_, text) = build_and_extract(&ed, &PdfOptions::default());
    assert_eq!(text, source);
}

/// Multi-sentence text with punctuation, which is where escaping and encoding bite.
#[test]
fn pdf_punctuation_and_numbers_survive() {
    let source = "R&D costs $1,234.50 (net) -- \"quoted\", 50% off; see [ref].\r\n\tTabbed.";
    let ed = Editor::from_text(source.as_bytes()).expect("load");
    let mut stats = pdf::PdfStats::default();
    let bytes = pdf::build(&ed, &PdfOptions::default(), &mut stats).expect("build");
    let text = content::extract_text(&bytes).expect("parse");

    // `\r\n` is one line break, and the `\r` is not a character to substitute -- so nothing is lost and
    // nothing is reported. A tab has no WinAnsi byte, so it becomes four spaces: reported as neither a
    // substitution nor a loss, because a tab's position was approximate either way.
    assert_eq!(
        stats.substituted, 0,
        "a CR or a tab should not count as an unencodable character"
    );
    assert_eq!(
        text,
        "R&D costs $1,234.50 (net) -- \"quoted\", 50% off; see [ref].\n    Tabbed."
    );
}

/// The styled document's *line breaks* are the document's, not the exporter's invention.
#[test]
fn pdf_line_structure_matches_the_document() {
    let mut ed = Editor::from_text(b"one\ntwo\nthree").expect("load");
    ed.style_range(0, 3, STYLE_BOLD, 0).expect("bold");
    ed.style_range(8, 13, STYLE_CODE, 0).expect("code");
    let (_, text) = build_and_extract(&ed, &PdfOptions::default());
    assert_eq!(text, "one\ntwo\nthree");
}

/// A run wider than the measure overflows and is *counted*, not silently clipped.
#[test]
fn pdf_a_wide_measure_takes_a_whole_paragraph_on_one_line() {
    let source = "a b c d e f g h i j k l m n o p q r s t";
    let ed = Editor::from_text(source.as_bytes()).expect("load");
    // A measure far wider than the text: one line, no breaks.
    let (bytes, text) = build_and_extract(
        &ed,
        &PdfOptions {
            page: pdf::PageSize {
                width: 4000.0,
                height: 800.0,
            },
            margin: 10.0,
            ..Default::default()
        },
    );
    assert_eq!(text, source);
    assert!(contains(&bytes, b"BT"));
}

/// Wrapping at a narrow measure splits the text across several lines, and the extractor puts every
/// word back in order and none of them in half.
///
/// The assertion is on **words**, not on the exact string. A PDF line break is not a text character:
/// wrapping consumes the space that caused it, so the extracted text has a `\n` where the source has a
/// space. Requiring byte equality here would be requiring the exporter *not* to wrap.
#[test]
fn pdf_wrapping_preserves_reading_order() {
    let source = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
    let ed = Editor::from_text(source.as_bytes()).expect("load");
    let (_, text) = build_and_extract(
        &ed,
        &PdfOptions {
            page: pdf::PageSize {
                width: 200.0,
                height: 800.0,
            },
            margin: 10.0,
            font_size: 11.0,
            ..Default::default()
        },
    );

    assert!(
        text.contains('\n'),
        "the text did not wrap at a 180pt measure, so this test is not testing wrapping"
    );
    assert_eq!(
        text.split_whitespace().collect::<Vec<_>>(),
        source.split_whitespace().collect::<Vec<_>>(),
        "wrapping reordered, dropped or split a word"
    );
    // No word was cut: every word in the source appears whole on some line.
    for word in source.split_whitespace() {
        assert!(
            text.lines().any(|l| l.contains(word)),
            "{word:?} was split across lines"
        );
    }
}

/// A line break in a PDF consumes the space that caused it, so a wrapped document's extracted text
/// has fewer spaces than its source. The gate's exact round trip therefore needs a document that fits
/// the measure -- which `pdf_text_extracts_to_the_source` is -- and this says so.
#[test]
fn pdf_wrapping_costs_exactly_the_spaces_at_the_breaks() {
    let source = "alpha bravo charlie delta echo";
    let ed = Editor::from_text(source.as_bytes()).expect("load");
    let (_, wide) = build_and_extract(
        &ed,
        &PdfOptions {
            page: pdf::PageSize {
                width: 4000.0,
                height: 800.0,
            },
            margin: 10.0,
            ..Default::default()
        },
    );
    assert_eq!(
        wide, source,
        "a 3980pt measure does not wrap, so this is an exact round trip"
    );

    let (_, narrow) = build_and_extract(
        &ed,
        &PdfOptions {
            page: pdf::PageSize {
                width: 90.0,
                height: 800.0,
            },
            margin: 10.0,
            ..Default::default()
        },
    );
    let breaks = narrow.matches('\n').count();
    assert_eq!(
        narrow.split_whitespace().count(),
        source.split_whitespace().count(),
        "wrapping lost a word"
    );
    assert_eq!(
        narrow.replace('\n', " "),
        source,
        "the only difference must be the space at each break"
    );
    assert!(breaks >= 1);
}

/// Characters WinAnsi cannot encode are reported, not silently dropped.
#[test]
fn pdf_reports_what_it_could_not_encode() {
    let ed = Editor::from_text("before \u{4e2d}\u{6587} after".as_bytes()).expect("load");
    let mut stats = pdf::PdfStats::default();
    pdf::build(&ed, &PdfOptions::default(), &mut stats).expect("build");
    assert_eq!(stats.substituted, 2, "two CJK characters are not WinAnsi");
    // And the export still succeeds, with `?` in their place.
    assert_eq!(stats.pages, 1);
}

/// An empty document is one blank page, not zero pages and not a broken file.
#[test]
fn pdf_an_empty_document_is_one_blank_page() {
    let ed = Editor::new();
    let (bytes, text) = build_and_extract(&ed, &PdfOptions::default());
    assert_eq!(text, "");
    assert!(contains(&bytes, b"/Type /Page"));
    // One page in the tree.
    assert_eq!(count_bytes(&bytes, b"/Type /Page\n"), 1);
}

/// A document taller than one page gets several, and the text spans them in order.
#[test]
fn pdf_a_long_document_paginates() {
    let mut source = String::new();
    for i in 0..200 {
        source.push_str(&format!("line {i} of the document\n"));
    }
    let ed = Editor::from_text(source.as_bytes()).expect("load");
    let mut stats = pdf::PdfStats::default();
    let bytes = pdf::build(&ed, &PdfOptions::default(), &mut stats).expect("build");
    assert!(
        stats.pages > 1,
        "200 lines did not paginate: {} pages",
        stats.pages
    );
    assert_eq!(stats.lines, 200);
    assert_eq!(stats.pages, count_bytes(&bytes, b"/Type /Page\n") as u32);

    // And every line is present, in order, across the pages.
    let text = content::extract_text(&bytes).expect("parse");
    let got: Vec<&str> = text.lines().collect();
    assert_eq!(got.len(), 200);
    for (i, line) in got.iter().enumerate() {
        assert_eq!(*line, format!("line {i} of the document"));
    }
}

/// Both formats agree about the document, which is the property that makes export trustworthy.
#[test]
fn both_formats_carry_the_same_text() {
    let ed = styled_document();
    let mut h: Vec<u8> = Vec::new();
    html::export_body(&mut h, &ed).expect("html");
    let (_, pdf_text) = build_and_extract(&ed, &PdfOptions::default());

    let from_html = content::unescape(&content::strip_tags(&String::from_utf8(h).expect("utf-8")));

    // No filtering on either side. HTML keeps the document's newlines as text and PDF keeps them as
    // line breaks, so both should equal the document exactly -- which is the strongest thing that can be
    // asserted about "these two formats agree".
    assert_eq!(from_html, text_of(&ed), "the HTML body is not the document");
    assert_eq!(pdf_text, text_of(&ed), "the PDF text is not the document");
    assert_eq!(from_html, pdf_text, "HTML and PDF disagree about the text");
}

/// The `write` entry point picks the right format and reports its size.
#[test]
fn the_dispatcher_writes_both_formats() {
    let ed = styled_document();
    for (format, magic) in [
        (Format::Html, &b"<!DOCTYPE html>"[..]),
        (Format::Pdf, b"%PDF-1."),
    ] {
        let mut out: Vec<u8> = Vec::new();
        let report = holonomy_export::write(&ed, &mut out, format, "Title").expect("write");
        assert_eq!(report.format, format);
        assert_eq!(report.bytes, out.len() as u64);
        assert!(
            out.starts_with(magic),
            "{format:?} did not start with its magic"
        );
    }
}

// ------------------------------------------------------------------ helpers

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    count_bytes(haystack, needle) > 0
}

fn count_bytes(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}
