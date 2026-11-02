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
use holonomy_text::{
    Editor, SpanPolicy, STYLE_BOLD, STYLE_CODE, STYLE_HEADER, STYLE_ITALIC, STYLE_STRIKE,
    STYLE_SUBSCRIPT, STYLE_SUPERSCRIPT, STYLE_UNDERLINE,
};

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

/// **The four decorative bits reach the HTML as tags, each opening its own element.**
///
/// The defect this exists for is the quiet one: the bits were added to the model, the toolbar and the
/// painter and **not to the exporter**, so an underlined word exported as plain text with the underline
/// gone and nothing said so. A format that drops a user's formatting without saying so is worse than
/// one that refuses the document.
#[test]
fn html_applies_the_decorations_as_their_own_elements() {
    for (flag, open, close) in [
        (STYLE_UNDERLINE, "<u>", "</u>"),
        (STYLE_STRIKE, "<s>", "</s>"),
        (STYLE_SUPERSCRIPT, "<sup>", "</sup>"),
        (STYLE_SUBSCRIPT, "<sub>", "</sub>"),
    ] {
        let mut ed = Editor::from_text(b"abcd").expect("load");
        ed.style_range(1, 3, flag, 0).expect("style the middle");
        let mut out: Vec<u8> = Vec::new();
        let stats = html::export_body(&mut out, &ed).expect("export");
        let s = String::from_utf8(out).expect("utf-8");
        assert_eq!(
            s,
            format!("a{open}bc{close}d"),
            "flag {flag:#x} must wrap exactly the styled bytes"
        );
        // And each is counted, once, because the counters are what a status line reports.
        let counted = match flag {
            STYLE_UNDERLINE => stats.underline_runs,
            STYLE_STRIKE => stats.strike_runs,
            STYLE_SUPERSCRIPT => stats.superscript_runs,
            _ => stats.subscript_runs,
        };
        assert_eq!(counted, 1, "flag {flag:#x} must be counted once");
    }
}

/// **A decoration composes with bold rather than replacing it.**
///
/// The nesting contract is `<b><u>` and not `<u><b>`, and the close order is the exact reverse --
/// get it wrong and the rest of the page renders underlined, which a browser will not report.
#[test]
fn html_nests_a_decoration_inside_bold() {
    let mut ed = Editor::from_text(b"abc").expect("load");
    ed.style_range(0, 3, STYLE_BOLD | STYLE_UNDERLINE, 0)
        .expect("style");
    let mut out: Vec<u8> = Vec::new();
    html::export_body(&mut out, &ed).expect("export");
    assert_eq!(
        String::from_utf8(out).expect("utf-8"),
        "<b><u>abc</u></b>",
        "the decoration is inside the weight, and closes first"
    );
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

/// **A decorated run still extracts to its own text, and the content stream carries the operators.**
///
/// Two claims, deliberately separate. **The text** must round-trip, because a decoration is not a
/// character: a `Ts` rise or a stroked line must not change what the reader selects or copies. **The
/// stream** must carry the operators, because the failure this guards is the quiet one -- the bits were
/// added to the model, the toolbar and the painter and not to the exporter, so an underlined word
/// exported as plain text with nothing said.
///
/// **The stream is decoded rather than grepped**, which is the difference between "the bytes
/// `/F1 11 Tf` appear somewhere" and "the operators are in the text object they belong to".
#[test]
fn pdf_carries_the_decorations_without_changing_the_text() {
    let mut ed = Editor::new();
    ed.insert_at(0, b"E=mc2 and more\n", SpanPolicy::Strict)
        .expect("insert");
    // "2" is superscript; "an" rises and "d" sinks (a real subscript next to a superscript, so both
    // signs are exercised); "more" is underlined and struck through at once.
    ed.style_range(4, 5, STYLE_SUPERSCRIPT, 0).expect("sup");
    ed.style_range(6, 8, STYLE_SUPERSCRIPT, 0).expect("sup 2");
    ed.style_range(8, 9, STYLE_SUBSCRIPT, 0).expect("sub");
    ed.style_range(10, 14, STYLE_STRIKE | STYLE_UNDERLINE, 0)
        .expect("both rules");

    let mut stats = pdf::PdfStats::default();
    let bytes = pdf::build(&ed, &PdfOptions::default(), &mut stats).expect("build");
    let text = content::extract_text(&bytes).expect("parse");

    // **The text is the source**, decorations and all -- a rise moves a glyph and does not insert one.
    assert_eq!(text, text_of(&ed));

    // **And the operators are there.** `Ts` for the rises (positive and negative), a stroked path for
    // the two rules. Read out of the decoded stream, not out of the file, so a `Ts` in a comment or a
    // font dictionary could not satisfy it.
    let streams = content::content_streams(&bytes).expect("streams");
    assert!(!streams.is_empty(), "a PDF with text has a content stream");
    let ops: String = streams
        .iter()
        .map(|(_, data)| String::from_utf8_lossy(data))
        .collect::<Vec<_>>()
        .join("\n");
    // **Whitespace-split tokens, not substrings.** `pdf-writer` chooses its own padding -- a newline
    // before an operator with no operands and a space before one with them -- so a gate written as
    // `ops.contains(" m\n")` would be asserting the writer's formatting rather than the operator.
    // A token that *is* `S` is the stroke operator and nothing else can be.
    let tokens: Vec<&str> = ops.split_whitespace().collect();
    let count_op = |op: &str| tokens.iter().filter(|t| **t == op).count();

    assert!(
        count_op("Ts") >= 2,
        "a rise per decorated run: {} Ts",
        count_op("Ts")
    );
    // The rise is a third of the run's size: positive for the superscript, negative for the subscript.
    // **The number is parsed back off the stream**, because `pdf-writer` formats floats with `ryu` --
    // a substring built with `format!` would be asserting `Display`'s digits rather than the writer's,
    // which is a test that passes and proves nothing.
    let rise = PdfOptions::default().font_size / 3.0;
    let rises: Vec<f32> = tokens
        .windows(2)
        .filter(|w| w[1] == "Ts")
        .filter_map(|w| w[0].parse::<f32>().ok())
        .collect();
    assert!(
        rises.iter().any(|r| (*r - rise).abs() < 1e-3),
        "no superscript rise of {rise} in {rises:?}"
    );
    assert!(
        rises.iter().any(|r| (*r + rise).abs() < 1e-3),
        "no subscript rise of -{rise} in {rises:?}"
    );

    // **The two rules are a path**: one `m`/`l` pair for the underline and one for the strike, stroked
    // once with `S`. A text-only PDF has neither, so any of these appearing at all is the claim.
    assert_eq!(count_op("m"), 2, "one subpath per rule: {tokens:?}");
    assert_eq!(count_op("l"), 2, "each rule is one straight segment");
    assert_eq!(count_op("S"), 1, "both rules stroke in one pass");
    assert_eq!(count_op("RG"), 1, "one stroke colour, set once");
    // And the thickness, parsed the same way: half a point, not the default one.
    assert!(
        tokens
            .windows(2)
            .any(|w| w[1] == "w" && (w[0].parse::<f32>().unwrap_or(9.0) - 0.5).abs() < 1e-6),
        "the rule is half a point thick: {tokens:?}"
    );

    // **And the counters say the same thing as the stream, which is the point of having them.**
    //
    // The operators above prove *something* stroked; they cannot prove *which runs* stroked, because a
    // path has no identity. The counters are per segment, so the two together are the claim: `struck ==
    // 1` and `underlined == 1` for the one segment carrying both bits, and `risen == 3` for the three
    // styled runs — `2` superscript, `an` superscript and `d` subscript. **A PDF that dropped a
    // decoration entirely still extracts the same text**, which is why the text assertion above cannot
    // stand alone.
    //
    // **The numbers are per *segment*, and a segment is not a span.** `push_many` merges only when face,
    // size, rise, underline and strike all agree, and **`flush_spaces` deliberately pushes the held space
    // plain and unraised** — an underline across the gap between two underlined words would draw one rule
    // where the author wrote two. So the space between `2` and `an` is its own plain segment and the
    // three styled words are three risen segments, not one.
    assert_eq!(stats.underlined, 1, "one underlined segment");
    assert_eq!(
        stats.struck, 1,
        "one struck segment, and it is the same one: `more` carries both bits and is one run"
    );
    assert_eq!(
        stats.risen, 3,
        "three risen segments: `2` and `an` raised and `d` lowered. **Two would mean the plain space \
         between the two superscripts had been merged into one of them**, which is the defect \
         `flush_spaces`' own comment is about"
    );
    // **The rise count and the `Ts` count are different numbers, and that is not a bug**: `Ts` is
    // emitted before *every* segment, including the `0` that puts plain text back on the baseline, so
    // the stream has more rises than the document has raised runs. Asserting they were equal would be
    // asserting that text state did not persist -- which is the defect `write_line`'s comment is about.
    assert!(
        count_op("Ts") > stats.risen as usize,
        "every segment gets a rise operator, decorated or not: {} Ts for {} risen segments",
        count_op("Ts"),
        stats.risen
    );
}

/// Plain ASCII, no styling: the simplest possible round trip.
/// **A raised run's rule rises with it, in the content stream.**
///
/// # Why the screen's gate cannot stand in for this one
///
/// `tests/decorations.rs` asserts the same *relationship* on the painter, and **the exporter is a second
/// implementation of where a rule goes** — a stroked path built from `advance(seg.metric, ..)` and a
/// constant offset, in points, with no atlas anywhere in it. A raise is `Ts`, which moves the text matrix;
/// a rule is a path in user space. The two are reconciled in `draw_rules` and nowhere else, so a fix in
/// the painter says nothing about the file.
///
/// # What is parsed, and why the row is the assertion
///
/// **The `y` operand of each `m`,** which is the subpath's start row. Underlining a plain word and the
/// same word raised puts two rules a whole rise apart; underlining only the raised word puts its rule a
/// rise *above* where the same underline would be for body text on that line. Both are read back off the
/// decoded stream, so this is asserting the operator arguments and not the writer's formatting.
#[test]
fn a_raised_runs_rule_rises_with_it_in_the_stream() {
    let opts = PdfOptions::default();
    let rise = opts.font_size / 3.0;

    // One word, raised and underlined: its rule must be a rise above the baseline it would have had.
    let mut raised = Editor::new();
    raised
        .insert_at(0, b"raised\n", SpanPolicy::Strict)
        .expect("insert");
    raised
        .style_range(0, 6, STYLE_SUPERSCRIPT | STYLE_UNDERLINE, 0)
        .expect("style");
    // The same word, underlined and *not* raised.
    let mut flat = Editor::new();
    flat.insert_at(0, b"raised\n", SpanPolicy::Strict)
        .expect("insert");
    flat.style_range(0, 6, STYLE_UNDERLINE, 0).expect("style");

    // **A nested `fn` and not a closure**, because it is called twice with different documents and a
    // closure that borrows `opts` would have to be `Fn` over a capture the caller also uses. The row of
    // every `m` in the decoded streams, which is a subpath's start point.
    fn rule_rows(ed: &Editor, opts: &PdfOptions) -> Vec<f32> {
        let mut stats = pdf::PdfStats::default();
        let bytes = pdf::build(ed, opts, &mut stats).expect("build");
        let streams = content::content_streams(&bytes).expect("streams");
        let ops: Vec<String> = streams
            .iter()
            .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
            .collect();
        let ops = ops.join("\n");
        let tokens: Vec<&str> = ops.split_whitespace().collect();
        // **`x y m`**, so the row is the operand two back from the operator.
        tokens
            .windows(3)
            .filter(|w| w[2] == "m")
            .filter_map(|w| w[1].parse::<f32>().ok())
            .collect()
    }

    let raised_rows = rule_rows(&raised, &opts);
    let flat_rows = rule_rows(&flat, &opts);
    assert_eq!(
        raised_rows.len(),
        1,
        "one underlined run is one subpath: {raised_rows:?}"
    );
    assert_eq!(flat_rows.len(), 1, "one subpath: {flat_rows:?}");

    // **A rise up the page**, so the raised word's rule is the higher of the two: PDF's y grows upward.
    assert!(
        (raised_rows[0] - flat_rows[0] - rise).abs() < 1e-3,
        "**the raised run's rule did not rise with its glyphs**: it is at y = {} and the same underline \
         on unraised text is at y = {}, a gap of {} where the rise is {rise}. A rule left on the line's \
         baseline draws under the air below a superscript.",
        raised_rows[0],
        flat_rows[0],
        raised_rows[0] - flat_rows[0]
    );
}

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

/// **A character that straddles a chunk boundary is one character, not three question marks.**
///
/// The exporter reads the document in 64 KiB pieces, and a UTF-8 character can straddle the edge.
/// The character used to be split: the lead byte was read as a one-byte character, `transcode`
/// could not decode it and produced `?`, and the two continuation bytes at the head of the next
/// piece each became a `?` as well.
///
/// **One CJK character became three question marks, silently, in the exported PDF.** The
/// substitution counter said three, and nothing said where they came from.
#[test]
fn pdf_a_character_at_a_chunk_boundary_is_not_corrupted() {
    // **Put a three-byte character exactly on the boundary.** The chunk is 64 KiB, so the
    // character starts at 65535 and its last byte is the first byte of the next piece.
    const CHUNK: usize = 64 * 1024;
    let mut source = vec![b'a'; CHUNK - 1];
    source.extend_from_slice("é".as_bytes()); // two bytes: the second lands on the boundary
    source.extend_from_slice(b"tail");
    let ed = Editor::from_text(&source).expect("load");

    let mut stats = pdf::PdfStats::default();
    let bytes = pdf::build(&ed, &PdfOptions::default(), &mut stats).expect("build");
    let text = content::extract_text(&bytes).expect("parse");

    // **`é` is in WinAnsi**, so it survives the export exactly.
    assert!(
        text.contains('é'),
        "the character on the chunk boundary was corrupted: {:?}",
        text.chars().skip(CHUNK - 3).take(8).collect::<String>()
    );
    assert!(
        !text.contains('?'),
        "a question mark means a character was substituted: {:?}",
        text.chars().skip(CHUNK - 3).take(8).collect::<String>()
    );
    assert_eq!(stats.substituted, 0, "nothing had to be substituted");
}

/// **A word that straddles a chunk boundary is one word.**
///
/// The exporter's pieces are 64 KiB and a formatting run can cross one. The word open at the end
/// of a piece used to be committed there, so a word straddling the boundary was broken in two —
/// and if the tail did not fit in the remaining measure, **the line broke inside the word**, which
/// is the exact defect the layouter exists to prevent.
#[test]
fn pdf_a_word_at_a_chunk_boundary_is_not_broken() {
    const CHUNK: usize = 64 * 1024;
    // **A word that starts just before the boundary and ends just after it**, with no space in it.
    let mut source = vec![b'a'; CHUNK - 4];
    source.extend_from_slice(b"word");
    source.extend_from_slice(b" more text");
    let ed = Editor::from_text(&source).expect("load");

    // **A narrow measure**, so the line is nearly full where the boundary falls and the tail of
    // the word genuinely does not fit. Without this the word would be split into two words that
    // both happened to fit on the same line, and the text would come out the same either way —
    // which is why the first version of this test could not fail.
    let opts = PdfOptions {
        page: pdf::PageSize {
            width: 200.0,
            height: 400.0,
        },
        ..PdfOptions::default()
    };
    let mut stats = pdf::PdfStats::default();
    let bytes = pdf::build(&ed, &opts, &mut stats).expect("build");
    let text = content::extract_text(&bytes).expect("parse");

    // **The word is whole.** A word broken across two lines comes back as two words with a
    // newline between them.
    assert!(
        !text.contains("wor\n") && !text.contains("\nd"),
        "the word on the chunk boundary was broken across a line: {:?}",
        &text[text.len().saturating_sub(40)..]
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

/// **A held space inside a `code` run is measured in Courier, so the text does not grow.**
///
/// # What this catches
///
/// `flush_spaces` used to convert the held space width at `BaseFont::Helvetica` while naming the run's
/// own face -- two halves of one decision disagreeing. **Courier's space is twice Helvetica's**, so every
/// space inside a `code` run came out as two spaces: the exported text read `a  b` where the document said
/// `a b`, and `draw_rules` traced a rule twice the length of the text underneath it.
///
/// # Why the assertion is the extracted text and not a width
///
/// **Because the defect was invisible in every measurement that did not reconstruct the string.** The
/// line still fitted, the page still had one line, and the extra space is a legal PDF byte. Only a reader
/// that puts the runs back together sees the document the author wrote, which is what `extract_text` is.
#[test]
fn a_space_inside_a_code_run_is_one_space() {
    // Two words, both in the code face, so the space between them is a *held* space in a Courier run --
    // the one case the conversion used to get wrong.
    let mut ed = Editor::from_text(b"alpha beta\n").expect("load");
    ed.style_range(0, 10, STYLE_CODE, 0).expect("code");
    let (bytes, text) = build_and_extract(&ed, &PdfOptions::default());
    assert_eq!(
        text, "alpha beta",
        "**the code run's words must be one space apart**: a doubled space means the held width was \
         converted in Helvetica and measured in Courier"
    );
    // **And the face really is Courier**, so a pass that silently fell back to Helvetica would not make
    // the assertion above true for the wrong reason. `base_fonts` reads the `/BaseFont` entries by name,
    // which is the helper that exists for exactly this -- `contains(&bytes, b"Courier")` would also match
    // the string on a line that never named a resource.
    assert!(
        content::base_fonts(&bytes).iter().any(|f| f == "Courier"),
        "the code run must be drawn in Courier, or this gate is comparing two Helvetica spaces: {:?}",
        content::base_fonts(&bytes).join(", ")
    );
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
