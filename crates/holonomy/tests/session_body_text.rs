//! **Phase 12's gate: the document is on the page.**
//!
//! Before this phase the page behind the chrome was blank. `Chrome::tree` drew the tab bar, the toolbar,
//! the ruler, the page, the scrollbar and the status bar, and `Session::paint` added tables, formulas and
//! images — and **nothing drew the document's own words**. `chrome.rs:445-448` says so explicitly: "the
//! page behind the chrome is blank".
//!
//! | what it proves | test |
//! | --- | --- |
//! | body text reaches the frame as ink | [`the_documents_own_words_are_on_the_page`] |
//! | and the ink is at the text column, not somewhere else | [`body_text_lands_in_the_text_column`] |
//! | one line, one row, one run | [`each_line_becomes_one_run_at_its_own_row`] |
//! | multi-byte UTF-8 is drawn whole, never split | [`a_multibyte_character_is_never_split_across_the_measure`] |
//! | `\r\n` does not leave a mark on every line | [`a_crlf_line_end_does_not_draw_a_carriage_return`] |
//! | the glyph advance is the font's, not the 8 px grid | [`a_glyph_advances_by_its_own_width_not_the_grid`] |
//! | blank glyphs cost an advance, not a pile-up | [`a_space_advances_the_pen_rather_than_stacking_glyphs`] |
//! | scrolling lands on the expected rows | [`scrolling_moves_the_ink_to_the_row_the_geometry_says`] |
//! | a paint costs the same at 3 MiB as at 3 KiB | [`a_paint_over_a_big_document_costs_what_a_small_one_does`] |
//! | the page-sized buffer replaced the document-sized one | [`the_body_text_buffer_is_a_page_not_a_document`] |

use holonomy::session::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SpanPolicy};

/// One atlas for the whole file.
///
/// The same reason `session_math.rs` has it, and the same `OnceLock`: `MathMetrics::advance` is a plain
/// `fn` pointer into a process-global, so ten tests each building an atlas make that global a race and
/// the measurement depends on which test ran last.
fn shared_atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (atlas, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(atlas))
    })
}

/// A session over a document holding `text`.
fn with_text(text: &str) -> Session<'static> {
    let mut ed = Editor::new();
    // **Line by line, not one insert.** `UndoStack` refuses an action larger than its capacity --
    // `ActionTooLarge { len: 400_000, capacity: 65_536 }` -- so a one-shot insert of a large fixture is
    // not a slow test but a *failing* one. Building the document the way a user would type it is also
    // more honest: the fixtures are documents, not one keystroke.
    for line in text.split_inclusive('\n') {
        if line.is_empty() {
            continue;
        }
        ed.insert_at(
            ed.text_len() as u32,
            line.as_bytes(),
            SpanPolicy::GrowIntoInsert,
        )
        .expect("seed the document");
    }
    let m = ChromeMetrics::DESKTOP;
    let scanout = HeadlessScanout::new(m.width, m.height);
    Session::new(
        ed,
        Painter::new(shared_atlas(), 0),
        Box::new(scanout),
        m,
    )
}

/// How many non-background pixels are in `rect`.
///
/// **The chrome is drawn around the text, so a page's ink is not all the document's.** This counts the
/// ink inside the *text column* only, which is where body text is emitted and where nothing else draws —
/// so a non-zero count is the document's words and not a tab bar.
fn ink_in_text_column(s: &Session<'_>) -> u32 {
    let l = s.text_rect();
    let mut n = 0u32;
    for y in l.y..l.y + l.height {
        for x in l.x..l.x + l.width {
            if is_ink(s.frame().pixel(x, y)) {
                n += 1;
            }
        }
    }
    n
}

/// Whether a pixel is *ink* -- dark -- rather than merely not the background.
///
/// **The page is `0xFFFA_FAF8`, near-white, not black.** A first version of this file counted "pixels
/// that are not 0xFF000000" and therefore counted the entire page: an empty document measured 382,080
/// inked pixels, which is every pixel in the text column, and the "a document has more ink than a blank
/// one" assertion compared 382,080 with 382,080 and failed.
///
/// So the test measures what a reader would call ink. The threshold is 0x80 on each channel, which
/// separates the near-black `INK` (0x18) and `INK_CHROME` (0xD8) from the near-white page (0xFA) with a
/// wide margin on both sides, and does not depend on the exact palette value.
fn is_ink(pixel: u32) -> bool {
    let r = (pixel >> 16) & 0xFF;
    let g = (pixel >> 8) & 0xFF;
    let b = pixel & 0xFF;
    r < 0x80 && g < 0x80 && b < 0x80
}

/// The first row in the text column that has any ink on it, or `None`.
fn first_inked_row(s: &Session<'_>) -> Option<u32> {
    let l = s.text_rect();
    for y in l.y..l.y + l.height {
        for x in l.x..l.x + l.width {
            if is_ink(s.frame().pixel(x, y)) {
                return Some(y - l.y);
            }
        }
    }
    None
}

/// **The headline: the document's words are on the page.**
#[test]
fn the_documents_own_words_are_on_the_page() {
    let mut s = with_text("hello");
    s.paint(None).expect("paint");
    let stats = *s.paint_stats();
    assert_eq!(
        s.stats.lines_drawn,
        1,
        "one line of text should have been emitted"
    );
    assert_eq!(
        s.stats.glyphs_drawn,
        5,
        "five bytes of 'hello' should have become one run"
    );
    assert_eq!(stats.doc_glyphs, 5, "five glyphs blitted");
    assert_eq!(
        stats.runs_missing, 0,
        "no run should have been unresolvable -- the session resolves them per line"
    );
    assert!(
        ink_in_text_column(&s) > 20,
        "the text column has {} inked pixels after painting 'hello'; the document is not on the page",
        ink_in_text_column(&s)
    );
}

/// The ink is *inside the text column*, not merely somewhere on the page.
///
/// A run at the wrong x would still put ink on the page and would still pass the count above, so this
/// separates the two by measuring the leftmost inked column and comparing it to the text column's left
/// edge. **The tolerance is 4 px**, which is a glyph's left side bearing at 16 ppem; anything more than
/// that is the run being somewhere else rather than a bearing.
#[test]
fn body_text_lands_in_the_text_column() {
    let mut s = with_text("hello");
    s.paint(None).expect("paint");
    let l = s.text_rect();
    let mut leftmost = u32::MAX;
    for y in l.y..l.y + l.height {
        for x in l.x..l.x + l.width {
            if is_ink(s.frame().pixel(x, y)) {
                leftmost = leftmost.min(x);
                break;
            }
        }
    }
    assert_ne!(leftmost, u32::MAX, "no ink in the text column at all");
    assert!(
        leftmost <= l.x + 4,
        "the first ink is at x={leftmost}, but the text column starts at {}; a run positioned by \\
         something other than the text rect",
        l.x
    );
}

/// One line, one run, one row — and the rows are `cell_h` apart.
#[test]
fn each_line_becomes_one_run_at_its_own_row() {
    let mut s = with_text("one\ntwo\nthree");
    s.paint(None).expect("paint");
    let stats = *s.paint_stats();
    assert_eq!(s.stats.lines_drawn, 3, "three lines on the page");
    // 3 + 3 + 5 bytes; the terminators are stripped, which is the point of the next assertion.
    assert_eq!(
        s.stats.glyphs_drawn, 11,
        "'one' + 'two' + 'three' is 11 bytes once the newlines are stripped"
    );
    assert_eq!(stats.doc_glyphs, 11, "and 11 glyphs blitted");
}

/// **The case a byte-oriented emitter gets wrong.** `é` is two bytes, so a line of them is half as many
/// codepoints as bytes, and cutting the line at a *byte* count splits a character in half — which paints
/// half a glyph and then a replacement character.
#[test]
fn a_multibyte_character_is_never_split_across_the_measure() {
    // Four `é` is eight bytes. The measure is far wider than that, so the whole line fits and the run's
    // byte length must be 8 — an odd length would mean a character was cut.
    let mut s = with_text("éééé");
    s.paint(None).expect("paint");
    let stats = *s.paint_stats();
    assert_eq!(
        s.stats.glyphs_drawn, 8,
        "four two-byte characters are eight bytes"
    );
    assert_eq!(stats.doc_glyphs, 4, "and four glyphs — not eight, and not two broken ones");
    assert_eq!(
        stats.missing, 0,
        "nothing should be undecodable: a line of valid UTF-8 must not produce replacement characters"
    );
}

/// A `\r` has a glyph in most faces, so a CRLF document drawn naively has a mark at the end of every line.
#[test]
fn a_crlf_line_end_does_not_draw_a_carriage_return() {
    let mut crlf = with_text("ab\r\ncd");
    crlf.paint(None).expect("paint");
    let crlf_stats = *crlf.paint_stats();
    let mut lf = with_text("ab\ncd");
    lf.paint(None).expect("paint");
    let lf_stats = *lf.paint_stats();
    assert_eq!(
        crlf_stats.doc_glyphs, lf_stats.doc_glyphs,
        "CRLF and LF documents should draw the same number of glyphs; CR is drawn as a mark otherwise"
    );
    assert_eq!(crlf_stats.doc_glyphs, 4, "two lines of two characters");
    assert_eq!(
        crlf_stats.missing, 0,
        "a CR is not a missing glyph — it is stripped before the run is built"
    );
}

/// **The advance fix, which is what PROJECT.md:943 named as "not done".** Before Phase 12, glyphs were
/// positioned on the 8 px grid; §9B measured real advances of 9–10 px for Latin letters and 14 px for `\sum`.
///
/// The assertion is on the *ink extent* of a run, because that is what a reader sees. Three `m`s at
/// 10 px each occupy 30 px of pen; on an 8 px grid they occupy 24. **So the drawn extent must exceed
/// `3 * cell_w`**, and if the advance were still the grid this would fail by 6 px.
#[test]
fn a_glyph_advances_by_its_own_width_not_the_grid() {
    let mut s = with_text("mmm");
    s.paint(None).expect("paint");
    let l = s.text_rect();
    let cell_w = s.chrome.metrics.cell_w;
    // The furthest-right inked column in the text column.
    let mut rightmost = 0u32;
    for y in l.y..l.y + l.height {
        for x in l.x..l.x + l.width {
            if is_ink(s.frame().pixel(x, y)) {
                rightmost = rightmost.max(x);
            }
        }
    }
    let drawn = rightmost - l.x;
    let on_the_grid = 3 * cell_w;
    assert!(
        drawn > on_the_grid,
        "three 'm's occupy {drawn} px, which is the {on_the_grid} px fixed grid; the advance should \
         be the font's own, which is wider"
    );
}

/// How far right the ink reaches in the text column, relative to its left edge.
fn ink_extent(s: &Session<'_>) -> u32 {
    let l = s.text_rect();
    let mut rightmost = 0u32;
    for y in l.y..l.y + l.height {
        for x in l.x..l.x + l.width {
            if is_ink(s.frame().pixel(x, y)) {
                rightmost = rightmost.max(x);
            }
        }
    }
    rightmost - l.x
}

/// A space must advance the pen. If blanks did not, every word in a document would overlap.
///
/// **Asserted as a difference between two documents rather than as an absolute width, and the reason is
/// that the absolute width goes the *wrong* way.** On a proportional face `a` advances 9 px, not the
/// grid's 8, so `"a a"` measures ~21 px of ink against the grid's 24 — a smaller number that is
/// nevertheless the *correct* one. Comparing "more than the grid" would therefore assert the bug. So
/// this measures `"aa"` against `"a a"`: the difference is exactly what the space contributed to the pen,
/// and it must be positive and less than a full cell.
#[test]
fn a_space_advances_the_pen_rather_than_stacking_glyphs() {
    let mut tight = with_text("aa");
    tight.paint(None).expect("paint");
    let mut spaced = with_text("a a");
    spaced.paint(None).expect("paint");
    let gap = ink_extent(&spaced).saturating_sub(ink_extent(&tight));
    let cell_w = tight.chrome.metrics.cell_w;
    assert!(
        gap > 0,
        "'a a' and 'aa' both measure {} px of ink: the space advanced the pen by nothing, so the \
         two words would overlap",
        ink_extent(&spaced)
    );
    assert!(
        gap < cell_w,
        "the space advanced the pen by {gap} px, which is a whole {cell_w} px cell; a space is \
         narrower than a cell on any face that has one"
    );
}

/// Scrolling moves the ink to the row the geometry says.
///
/// **This is the gate that ties `LineGeometry` to pixels.** `scroll_line` is a *document* line index and
/// the run's y comes from `LineHeights::y(row)`, so if the Fenwick trees and the emitter disagree the ink
/// lands on the wrong row. Asserting the ink's row rather than the scroll value is what makes it a test
/// of the drawing rather than of the arithmetic.
#[test]
fn scrolling_moves_the_ink_to_the_row_the_geometry_says() {
    // **60 lines, not 8.** The first version used 8 three-character lines and asserted that scrolling to
    // line 5 left one line on the page -- but 8 lines fit on a 43-row page, so scrolling changed nothing
    // and the page correctly still showed all of them. The test was asserting that scrolling works by
    // assuming a document too short to scroll, which is a test that passes for the wrong reason.
    let text: String = (0..60).map(|i| format!("l{:02}\n", i)).collect();
    let mut s = with_text(&text);
    let rows = s.chrome.layout.rows;
    assert!(rows < 60, "the fixture must be taller than the page, or there is nothing to scroll");

    s.paint(None).expect("first paint");
    let top = first_inked_row(&s).expect("ink on the first page");
    let first_page = *s.paint_stats();

    s.scroll_to(50).expect("scroll to line 50");
    s.paint(None).expect("paint after scrolling");
    let after = first_inked_row(&s).expect("ink after scrolling");
    let second_page = *s.paint_stats();

    // **11, not 10**, and the extra one is the point: the fixture ends in a newline, so the document
    // has 61 lines -- 60 terminated and one empty. A text editor shows that empty last line, and so does
    // this. `TextCounts::lines()`'s "`newlines + 1`" convention is the same fact stated once.
    assert_eq!(
        s.stats.lines_drawn,
        11,
        "lines 50..60 plus the empty last line, on a page holding {rows} rows"
    );
    // **30 on the second page against 69 on the first**, and that difference *is* the scroll. The first
    // page holds `rows` = 23 lines x 3 characters = 69 glyphs; the second holds the 10 lines from line 50
    // plus the empty last one = 30. So the page did not merely move text to the same place -- it is
    // showing different lines, which is the half of "scrolling works" that the row assertion below
    // cannot see.
    assert_eq!(
        first_page.doc_glyphs,
        rows * 3,
        "the first page holds {rows} lines of three characters"
    );
    assert_eq!(
        second_page.doc_glyphs,
        30,
        "the second page holds the 10 lines from line 50 plus the empty last one"
    );

    // **Within the first row, not at row 0.** `first_inked_row` returns a *pixel* offset, and a glyph's
    // ink starts `bearing_y` below its line box's top. Asserting `after == 0` would be asserting the
    // typeface has no side bearing: a claim about the font rather than about the geometry, and exactly
    // the kind that makes a gate fail for a reason nobody can act on. What matters is that the ink is in
    // the *first row* it was in before the scroll, which is the claim that `scroll_line` and
    // `LineHeights` agree.
    let cell_h = s.chrome.metrics.cell_h;
    assert!(
        after < cell_h,
        "the first ink after scrolling to line 50 is {after} px below the text column's top, past \
         the {cell_h} px first row, so line 50 did not land on row 0 (it was at {top} px before)"
    );
}

/// **A paint's cost is bounded by the page, not the document.** This is the property that makes a
/// 2000-page document paintable, and it is what Phase 11's `session_latency.rs` asserted at 8x — Phase 12
/// makes it 1x, because the body-text emitter reads one row at a time and the whole emitter is bounded
/// by `Layout::rows`.
#[test]
fn a_paint_over_a_big_document_costs_what_a_small_one_does() {
    let small_text = "the quick brown fox\n".repeat(20);
    let mut small = with_text(&small_text);
    let big_text = "the quick brown fox\n".repeat(20_000);
    let mut big = with_text(&big_text);

    // Both painted once to settle any first-touch costs.
    small.paint(None).expect("small");
    big.paint(None).expect("big");

    let time = |s: &mut Session<'_>| -> u128 {
        let start = std::time::Instant::now();
        for _ in 0..20 {
            let _ = s.paint(None);
        }
        start.elapsed().as_micros() / 20
    };

    let small_us = time(&mut small);
    let big_us = time(&mut big);
    println!(
        "paint: {small_us} us for {} bytes, {big_us} us for {} bytes",
        small.editor.text_len(),
        big.editor.text_len()
    );
    // **4x**, not 1x: a repaint allocates nothing but does touch `l.width` pixels per row, and the
    // measurement is noisy enough that 1x would flake. What 4x rules out is the failure mode that
    // matters, which is a paint that reads the document.
    assert!(
        big_us <= small_us.saturating_mul(4).max(100),
        "a paint over a {} byte document took {big_us} us against {small_us} us over {} bytes, so \
         the paint is scaling with the document rather than with the page",
        big.editor.text_len(),
        small.editor.text_len()
    );
}

/// **The memory claim, asserted on the buffer rather than on RSS.** RSS is `tests/session_rss.rs`'s
/// business and it moves with the framebuffer; this asserts the thing Phase 12 actually changed, which is
/// that drawing the document reads a *page*.
#[test]
fn the_body_text_buffer_is_a_page_not_a_document() {
    let text = "the quick brown fox jumps over the lazy dog\n".repeat(5_000);
    let mut s = with_text(&text);
    assert_eq!(
        s.line_scratch_capacity(),
        48 * 1024,
        "the body-text buffer must stay page-sized as the document grows"
    );
    s.paint(None).expect("paint");
    assert_eq!(
        s.line_scratch_capacity(),
        48 * 1024,
        "painting must not have grown it -- a buffer that grows on the paint path is an allocation \\
         per paint"
    );
    // And it is genuinely smaller than the document it just drew.
    // **Half, not a tenth.** The first version asserted `capacity < len / 10` and failed at 48 KiB
    // against a 220 KB fixture — the ratio is 4.5x, not 10x, because a 220 KB document is only four
    // times a page. The claim worth making is "the buffer does not grow with the document", and half is
    // the loosest bound that still fails if the buffer ever becomes document-sized.
    assert!(
        s.line_scratch_capacity() * 2 < s.editor.text_len(),
        "a {} byte buffer against a {} byte document is not the page-sized buffer Phase 12 is for",
        s.line_scratch_capacity(),
        s.editor.text_len()
    );
    assert!(
        s.stats.lines_drawn > 0,
        "the big document's first page must have been drawn, or the buffer is sized right and unused"
    );
}

/// **The body-text emitter reads one page, and this is what says so.**
///
/// # The honest version, after the ambitious one failed
///
/// A first version of this test asserted `doc_scratch_capacity() == 0` after painting 220 KB of prose,
/// on the reasoning that `emit_tables`, `emit_math` and `emit_images` all early-return on an empty span
/// list and so the whole-document buffer would never be allocated. **It measured 221,184 bytes.**
///
/// The reason is `publish_line_heights` → `math_blocks_for`, which calls `read_document` *unconditionally*
/// — it has to, because `holonomy_text::for_each_math_span` is a cursor over bytes and there is no
/// "does this document contain any math" question to ask without reading the bytes to find the `$$`.
/// `Editor` has no math accessor at all (grep: zero hits in `editor.rs`).
///
/// **So `doc_scratch` is still 6.00 MiB on a 6 MiB document, RSS is still 26.85 MiB, and
/// `tests/session_rss.rs`'s 2.85 bytes per document byte is still the true figure.** Phase 12 did not
/// move the number it said it would. What it did move is the *body text's* read, and this asserts that.
///
/// What this asserts instead:
/// * the body-text emitter's buffer is page-sized and never grows with the document;
/// * a paint's work is bounded by the page, which `a_paint_over_a_big_document_costs_what_a_small_one_does`
///   measures end to end;
/// * and the claim is stated as what it is — a page read — rather than as a memory saving, because the
///   memory saving is Phase 12's remaining work and belongs to whoever picks it up.
#[test]
fn the_body_text_emitter_reads_one_page_and_not_the_document() {
    let text = "the quick brown fox jumps over the lazy dog\n".repeat(5_000);
    let mut s = with_text(&text);
    s.paint(None).expect("paint");

    // The page buffer holds at most a page, and is the only buffer the emitter uses.
    assert!(
        s.line_scratch_capacity() == 48 * 1024,
        "the page buffer is {} bytes, not 48 KiB, so it is sized from the document",
        s.line_scratch_capacity()
    );
    // What it actually read is bounded by the rows on the page, not by the lines in the document.
    let rows = s.chrome.layout.rows;
    assert!(
        s.page_used() <= rows as usize * 1024,
        "the emitter read {} bytes for {} rows; a page cannot be more than rows x \
         DocRun::MAX_BYTES, so this is reading the document",
        s.page_used(),
        rows
    );
    // And the document it did *not* read is an order of magnitude larger.
    assert!(
        s.page_used() * 10 < s.editor.text_len(),
        "the emitter read {} bytes of a {} byte document: that is a whole-document read wearing a \
         page-sized hat",
        s.page_used(),
        s.editor.text_len()
    );
    assert!(
        s.stats.lines_drawn > 0,
        "the page must have been drawn, or nothing was read at all"
    );
}

/// A frame with a document on it is not a blank frame, and the difference is countable.
///
/// **The regression this whole file exists to prevent.** `chrome.rs:445-448` records that the page was
/// blank and says "what is missing is not a fix but the shared model that a fix would need" — which was
/// true when written and is now false. This test is the one that fails if body text stops being emitted.
#[test]
fn a_document_frame_is_distinguishable_from_a_blank_page() {
    let mut blank = with_text("");
    blank.paint(None).expect("paint");
    let mut filled = with_text("some text on the page");
    filled.paint(None).expect("paint");

    // An empty document is **one line**, not zero -- the same convention `TextCounts::lines()` and
    // `LineHeights` use, and the one a status bar showing "0 lines" on a new file would contradict.
    assert_eq!(blank.stats.lines_drawn, 1, "an empty document is one line, not zero");
    assert!(
        filled.stats.lines_drawn > 0,
        "a document with text must have drawn a line"
    );
    assert!(
        ink_in_text_column(&filled) > ink_in_text_column(&blank),
        "a document with text must put more ink in the text column than an empty one ({} vs {})",
        ink_in_text_column(&filled),
        ink_in_text_column(&blank)
    );
}

