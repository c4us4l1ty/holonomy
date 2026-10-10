//! **Bold is stored, and this is where it started being drawn.** 7 tests.
//!
//! # What was wrong
//!
//! `Editor::style_range` writes style flags into the span map, `Session::toggle_bold` calls it, and the
//! flags survive undo — `holonomy-text/tests/style_undo.rs` proves that. **And nothing ever read them.**
//! `emit_body_text` emitted one `DocRun` per line with `Style::REGULAR` hardcoded, and `Editor::style_at`
//! was called from nowhere in the product — only from `holonomy-text`'s own tests.
//!
//! **So a document with every character set bold painted pixel-for-pixel like the same document
//! unstyled.** `every_styled_byte_was_drawn_in_the_bold_face` is the gate for that, and its first version
//! asserted the *opposite*: that the two frames are identical. **The assertion was green, which is how
//! the bug was confirmed rather than assumed.**
//!
//! # What it uses
//!
//! `SpanMap::runs_in(start, end) -> Vec<(TextIntervalSpan, u32, u32)>`, whose own doc reads:
//!
//! > What a renderer walks: for each visible line, the runs it must draw, each with a colour and an
//! > atlas style.
//!
//! **It existed, it was correct, and nothing called it.** So did `TextIntervalSpan::atlas_style`, which
//! collapses the four flag combinations onto the four faces the atlas has.
//!
//! | what it proves | test |
//! | --- | --- |
//! | **the styled document draws differently** | [`every_styled_byte_was_drawn_in_the_bold_face`] |
//! | and only on the styled row | [`only_the_styled_row_changes`] |
//! | and the plain bytes of that row do not | [`the_plain_bytes_of_a_styled_row_are_unchanged`] |
//! | a run in the middle keeps the tail | [`a_styled_run_in_the_middle_keeps_the_tail_after_it`] |
//! | **cell positions, not byte positions** | [`runs_are_positioned_by_cells_not_bytes`] |
//! | italic is a different face | [`italic_resolves_to_a_different_atlas_style_than_bold`] |
//! | and the empty case costs nothing | [`an_unstyled_document_takes_the_single_run_path`] |

use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SpanPolicy, STYLE_BOLD, STYLE_CODE, STYLE_ITALIC};

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (a, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

fn painted(text: &str, style: Option<(u32, u32, u16)>) -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    ed.insert_at(0, text.as_bytes(), SpanPolicy::GrowIntoInsert)
        .expect("room");
    let len = ed.text_len() as u32;
    if let Some((lo, hi, flags)) = style {
        ed.style_range(lo, hi, flags, 0).expect("style the range");
    }
    let mut s = Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    );
    s.caret_to(0).expect("park the caret");
    s.repaint_all().expect("paint");
    let _ = len;
    s
}

/// How many pixels differ between two frames, and the box they differ in.
fn difference(a: &Session<'_>, b: &Session<'_>) -> (u32, (u32, u32, u32, u32)) {
    let mut n = 0u32;
    let (mut x0, mut y0) = (u32::MAX, u32::MAX);
    let (mut x1, mut y1) = (0u32, 0u32);
    for y in 0..a.chrome.metrics.height {
        for x in 0..a.chrome.metrics.width {
            if a.frame().pixel(x, y) != b.frame().pixel(x, y) {
                n += 1;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    (n, (x0, y0, x1, y1))
}

/// **A document with every character bold draws differently from the same document unstyled.**
///
/// **This is the gate the whole part is for.** Before part 24 the two frames were identical and this test
/// asserted that; the assertion being green is how the bug was confirmed rather than suspected, and the
/// assertion was inverted when the emitter learned to read the span map.
#[test]
fn every_styled_byte_was_drawn_in_the_bold_face() {
    let text = "bold\n";
    let plain = painted(text, None);
    let bold = painted(text, Some((0, 4, STYLE_BOLD)));
    let (n, box_) = difference(&plain, &bold);
    assert!(
        n > 0,
        "**the frames are identical**: a document with every character set bold paints exactly like the \
         same document unstyled. That was the state of this build before part 24 -- `style_at` was \
         called from no product code at all -- and the gate asserting it was green."
    );
    // **Not one pixel of difference anywhere.** A test that only wanted "different" would pass if bold
    // painted a stray dot in the chrome; the box keeps it on the page.
    let l = plain.chrome_layout();
    let (x0, y0, x1, y1) = box_;
    assert!(
        x0 >= l.page.x
            && x1 < l.page.x + l.page.width
            && y0 >= l.page.y
            && y1 < l.page.y + l.page.height,
        "the difference must be on the page: box x {x0}..={x1} y {y0}..={y1}, page {:?}",
        l.page
    );
}

/// **Only the styled row changes, and the rows around it do not.**
///
/// **The fixture is three lines with the style on the middle one.** A single-line document cannot tell
/// "the styled row changed" from "the only row changed", which is the same fixture rule as part 20's
/// three-line click gate and part 22's sparse gate.
#[test]
fn only_the_styled_row_changes() {
    let plain = painted("one\ntwo\nthree\n", None);
    let bold = painted("one\ntwo\nthree\n", Some((4, 7, STYLE_BOLD)));
    let (_, (x0, y0, x1, y1)) = difference(&plain, &bold);

    // **From the *text* rect, not the page rect.** The first version of this divided by `page.y`, which is
    // the top of the white sheet rather than the top of the first line, so every row came out three rows
    // lower than it was. `Layout::page` and `Layout::text` are different rects and only one of them has
    // the first glyph on its top edge.
    let l = plain.text_rect();
    let cell_h = plain.chrome.metrics.cell_h;
    let row = (y0 - l.y) / cell_h;
    assert_eq!(
        row, 1,
        "the differing pixels start on row {row}; `two` is row 1 and the style covers bytes 4..7"
    );
    // **And the box does not reach the rows below.** Row 1 spans `y0` to `y0 + cell_h`; a difference on
    // row 2 would push `y1` past it.
    assert!(
        y1 < l.y + 2 * cell_h,
        "the difference reaches y {y1}, which is row {} -- only row 1 is styled",
        (y1 - l.y) / cell_h
    );
    assert!(
        x1 < l.x + 4 * plain.chrome.metrics.cell_w,
        "and it stays inside the word"
    );
}

/// **The plain bytes of a styled row are unchanged.**
///
/// **Half of a line styled must not restyle the other half, and the only way to know is to look.** The
/// first pixel of the row and the last are compared between the two frames directly: they belong to
/// bytes outside the styled range, so every pixel in them must be equal.
#[test]
fn the_plain_bytes_of_a_styled_row_are_unchanged() {
    let plain = painted("plain bold tail\n", None);
    let bold = painted("plain bold tail\n", Some((6, 10, STYLE_BOLD)));
    let l = plain.chrome_layout();
    let cell_w = plain.chrome.metrics.cell_w;
    let cell_h = plain.chrome.metrics.cell_h;

    let row_y = l.page.y;
    // **Bytes 0..6, "plain "** — one cell wide of margin on each side, because glyphs can bleed a pixel
    // outside their cell.
    for x in l.page.x..l.page.x + 5 * cell_w {
        for y in row_y..row_y + cell_h {
            assert_eq!(
                plain.frame().pixel(x, y),
                bold.frame().pixel(x, y),
                "the plain prefix changed at ({x}, {y}): styling bytes 6..10 restyled the text before it"
            );
        }
    }
    // **And bytes 11.., " tail"**, the part after the run. This is the half that the *tail* branch in
    // `emit_body_text` exists for, and dropping it would lose the text rather than restyle it.
    for x in l.page.x + 11 * cell_w..l.page.x + 15 * cell_w {
        for y in row_y..row_y + cell_h {
            assert_eq!(
                plain.frame().pixel(x, y),
                bold.frame().pixel(x, y),
                "the plain tail changed at ({x}, {y}): the text after the styled run moved or vanished"
            );
        }
    }
}

/// **A run in the middle keeps the tail after it, and the tail is at the right x.**
///
/// **The failure this catches is text disappearing**, which is the worst symptom a styling bug can have:
/// bold is applied, and part of the line goes with it. The assertion is that the ink extent of the bold
/// frame is the same as the plain frame's — **styling changes faces, not lengths**, because the grid is
/// fixed-cell and every run advances by its codepoint count.
#[test]
fn a_styled_run_in_the_middle_keeps_the_tail_after_it() {
    let plain = painted("alpha beta gamma\n", None);
    let bold = painted("alpha beta gamma\n", Some((6, 10, STYLE_BOLD)));
    let extent = |s: &Session<'_>| {
        let l = s.chrome_layout();
        let mut right = 0u32;
        for y in l.page.y..l.page.y + l.page.height.min(plain.chrome.metrics.cell_h) {
            for x in l.page.x..l.page.x + l.page.width {
                if s.frame().pixel(x, y) != 0 {
                    right = right.max(x);
                }
            }
        }
        right - l.page.x
    };
    assert_eq!(
        extent(&bold),
        extent(&plain),
        "styling four bytes changed how wide the row's ink is: {} against {}",
        extent(&bold),
        extent(&plain)
    );
}

/// **Runs are positioned by cells, not by bytes.**
///
/// **The fixture is `éé bold`**, because ASCII cannot tell the two apart — a byte offset and a cell offset
/// coincide on every byte below 0x80. After two two-byte characters the byte offset is 4 and the cell
/// offset is 2, so positioning by bytes puts the bold run **two cells too far right** and leaves a
/// two-cell gap.
#[test]
fn runs_are_positioned_by_cells_not_bytes() {
    // **The styled bytes are letters, and that is the second half of this fixture's correctness.** The
    // first version styled `(4, 8)` on `"éé bold\n"`, which is `" bol"` -- the run *starts* with the
    // space, and a space is the same glyph in both faces. So the first *differing pixel* was one cell
    // later than the run's first cell, and the test failed for the right reason at the wrong place.
    //
    // **`"ééab"` with bytes 4..6 styled**: `é` is cell 0, `é` is cell 1, `a` is cell 2 and `b` is cell 3,
    // so the run starts at **cell 2 and byte 4**. Position by bytes and it starts at cell 4 — two cells
    // right, with a two-cell gap where the `é`s are.
    let plain = painted("ééab\n", None);
    let bold = painted("ééab\n", Some((4, 6, STYLE_BOLD)));
    let l = plain.text_rect();
    let cell_w = plain.chrome.metrics.cell_w;
    let (n, (x0, _, _, _)) = difference(&plain, &bold);
    assert!(n > 0, "the styled run drew nothing");
    assert_eq!(
        (x0 - l.x) / cell_w,
        2,
        "the bold run starts {} px in, which is cell {}; byte 4 is cell 2, because `éé` is two cells \
         and four bytes. Positioning by bytes puts it at cell 4.",
        x0 - l.x,
        (x0 - l.x) / cell_w
    );
}

/// **Italic and code resolve to different atlas styles, and the flags reach the face.**
///
/// **`atlas_style` is the authority** — `STYLE_CODE` wins over bold, bold over italic, and `u8::MAX` for
/// an unknown request. Asserted here through the *product's* painter rather than through the function, so
/// the test is about what reaches the screen and not about what the function returns.
#[test]
fn italic_resolves_to_a_different_atlas_style_than_bold() {
    use holonomy_text::TextIntervalSpan;
    // **The mapping, which is a restatement rather than a discovery.** `atlas_style` is `const fn` and
    // has no gate of its own for the *combinations*, only for the spans it was given; this pins the
    // table its doc prints.
    assert_eq!(
        TextIntervalSpan::plain(0, 1).atlas_style(),
        0,
        "plain is Inter Regular"
    );
    assert_eq!(
        TextIntervalSpan::styled(0, 1, STYLE_BOLD, 0).atlas_style(),
        1,
        "bold is Inter Bold"
    );
    assert_eq!(
        TextIntervalSpan::styled(0, 1, STYLE_ITALIC, 0).atlas_style(),
        2,
        "italic is Inter Italic"
    );
    assert_eq!(
        TextIntervalSpan::styled(0, 1, STYLE_CODE, 0).atlas_style(),
        3,
        "code is JetBrains Mono"
    );
    // **Bold beats italic, because there is no bold-italic face.** This is the collapse the doc names
    // and it is worth a gate: a renderer that asked for its own combination would produce a face index
    // the atlas does not have.
    assert_eq!(
        TextIntervalSpan::styled(0, 1, STYLE_BOLD | STYLE_ITALIC, 0).atlas_style(),
        1,
        "bold+italic collapses to bold, which is what its doc says and the only thing that can be right \
         with four faces"
    );

    // **And through the product: italic draws, and draws differently from bold.**
    let bold = painted("word\n", Some((0, 4, STYLE_BOLD)));
    let italic = painted("word\n", Some((0, 4, STYLE_ITALIC)));
    let (n, _) = difference(&bold, &italic);
    assert!(
        n > 0,
        "bold and italic drew the same pixels, so one of them is not being applied"
    );
}

/// **An unstyled document costs what it cost before part 24.**
///
/// **This is the performance claim, and the first version of it was simply false.** It asserted
/// `editor().spans().is_empty()` for a document nobody styled, and the span map was *not* empty:
/// `SpanMap::plain(text_len)` seeds one plain span over the whole text and `apply_insert` keeps it there.
/// So "the empty map means the styled loop never runs" was a premise the data did not support.
///
/// **What is actually true, and what the gate now asserts:** the plain spans resolve to atlas style 0,
/// so **the emitter produces the same one run per line it always did** — the styled loop runs, finds
/// one plain run covering the line, and emits it as `Style(0)`. **Same output, one extra branch.** The
/// count of runs is asserted rather than the emptiness of the map, because the count is what the paint
/// path pays.
#[test]
fn an_unstyled_document_takes_the_single_run_path() {
    // **A line with no spaces in it, and that is the second half of this fixture being right.** The first
    // version used `"no styling here"` and asserted `doc_glyphs == 16` for fifteen characters; the
    // counter said 13. **A space advances the pen and is not counted as a glyph**, so the number was
    // right and the expectation was not — and an assertion about a counter whose semantics are not pinned
    // is an assertion about a coincidence.
    const LINE: &str = "abcdefghijklmno\n";
    let len = LINE.trim_end().len() as u32;
    let s = painted(LINE, None);
    let runs = s.editor().spans().runs_in(0, len);
    assert!(
        !runs.is_empty(),
        "**the span map is not empty** — `SpanMap::plain` seeds a plain span over the whole text, so \
         part 24's first draft premise ('an unstyled document has no spans') was wrong. It is recorded \
         here because the gate that would have caught it is this one."
    );
    assert!(
        runs.iter().all(|(span, _, _)| span.atlas_style() == 0),
        "every run of an unstyled document must resolve to atlas style 0, or the plain path is not \
         being taken: {:?}",
        runs.iter().map(|(sp, lo, hi)| (*lo, *hi, sp.atlas_style())).collect::<Vec<_>>()
    );

    // **And the observable consequence: the line is drawn whole, in the plain face.** `doc_glyphs` counts
    // glyphs the painter actually drew, so for fifteen non-blank characters it is fifteen.
    let stats = s.paint_stats();
    assert_eq!(
        stats.doc_glyphs, len,
        "every character of the line is drawn, which is what the single-run path produced before part \
         24: {stats:?}"
    );
    assert_eq!(stats.runs_missing, 0, "and nothing was skipped");
}
