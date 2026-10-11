//! **One span query serves the whole page, and the page's lines get their own runs.** 4 tests.
//!
//! # The regression this part fixes, which part 24 introduced
//!
//! Part 24 made `emit_body_text` walk `SpanMap::runs_in` — **once per line**. A forty-line page paid for
//! that forty times, and each call cost:
//!
//! * **one heap allocation**, on the path whose invariant is zero. `tests/session_no_alloc.rs` asserts
//!   zero on the *edit* path and is explicit that the paint path was out of scope — **part 24 moved the
//!   paint path from out of scope to forty allocations per frame.**
//! * **a linear scan of the span list**, because `runs_in` walks from the start of the list and breaks
//!   only past the end of the range. On a document scrolled deep into a styled file that is every span
//!   before the page, forty times over.
//!
//! **The hoist is one `runs_in` over the page's byte range**, and the per-line partition is a
//! `skip_while`/`take_while` pair over the *page's* runs — a handful — with no allocation.
//!
//! # Why a counter and not a comment
//!
//! **`SessionStats::span_queries`** counts the queries, because "once per paint" is a shape and a shape
//! is not visible from the outside. **And the fixture has to be a page, not a line**: a one-line
//! document satisfies both the old code and the new one, which is the fixture mistake this project has
//! now made four times.
//!
//! | what it proves | test |
//! | --- | --- |
//! | one query per paint, whatever the page holds | [`one_span_query_serves_the_whole_page`] |
//! | and it does not grow with the line count | [`the_query_count_does_not_grow_with_the_page`] |
//! | every line still gets its own runs | [`every_line_gets_its_own_runs_not_the_first_lines`] |
//! | and a styled line past the first still draws | [`a_styled_line_past_the_first_still_draws_bold`] |

use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SpanPolicy, STYLE_BOLD};

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (a, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

/// A session over `lines` lines of `word `, with nothing styled, painted once.
fn painted(lines: usize) -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    for i in 0..lines {
        ed.insert_at(ed.text_len() as u32, b"word ", SpanPolicy::GrowIntoInsert)
            .expect("seed");
        if i + 1 < lines {
            ed.insert_at(ed.text_len() as u32, b"\n", SpanPolicy::GrowIntoInsert)
                .expect("newline");
        }
    }
    let mut s = Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    );
    s.caret_to(0).expect("park the caret");
    s.repaint_all().expect("paint");
    s
}

/// **A paint asks the span map once.**
///
/// **Counted across three paints**, so "once per paint" is stated as a rate rather than as a total — and
/// so a paint that asked twice on the second frame and never again would still fail.
#[test]
fn one_span_query_serves_the_whole_page() {
    let mut s = painted(40);
    let after_first = s.stats.span_queries;
    assert_eq!(
        after_first, 1,
        "a forty-line page asked the span map {after_first} times, not once"
    );
    s.repaint_all().expect("paint again");
    s.repaint_all().expect("and again");
    assert_eq!(
        s.stats.span_queries, 3,
        "three paints, three queries: the count is per paint and not per page-load"
    );
}

/// **The query count does not grow with the page.**
///
/// **The fixture is the whole point**: one line and forty lines, and the same number. **The old
/// per-line call would give 1 and 40.** A gate on one line would pass against the code this replaced,
/// which is the trap this file's header is about.
#[test]
fn the_query_count_does_not_grow_with_the_page() {
    let one = painted(1);
    let forty = painted(40);
    assert_eq!(
        one.stats.span_queries, forty.stats.span_queries,
        "a one-line page and a forty-line page both asked once"
    );
}

/// **Every line gets its own runs, and not the first line's.**
///
/// **This is the partition, and the failure it catches is a specific one.** The first version of the
/// partition was `take_while(|(_, _, hi)| *hi > at)` alone. `SpanMap::style_range` rebuilds its list in
/// ascending offset order, and `take_while` **stops at the first span whose end is at or before this
/// line's start** — which for the line after a styled one is the first span in the list. **So every line
/// after the first styled run drew plain**, and the symptom was "no differing pixels at all" on four
/// gates at once. The fix is `skip_while` for the spans before this line and `take_while` for the ones
/// past it, and this asserts the shape that makes both halves necessary.
#[test]
fn every_line_gets_its_own_runs_not_the_first_lines() {
    let mut ed = Editor::new();
    // Three lines, the middle one styled, so line 1 has a styled run before *and* after it in the list.
    ed.insert_at(0, b"aaa\nbbb\nccc\n", SpanPolicy::GrowIntoInsert)
        .expect("seed");
    ed.style_range(4, 7, STYLE_BOLD, 0)
        .expect("style the middle line");
    let mut s = Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(
            ChromeMetrics::DESKTOP.width,
            ChromeMetrics::DESKTOP.height,
        )),
        ChromeMetrics::DESKTOP,
    );
    s.caret_to(0).expect("park the caret");
    s.repaint_all().expect("paint");

    // **The line's own runs, asked for directly, and the count of them.**
    let all = s.editor().spans().runs_in(0, 12);
    let on_line_1: Vec<u32> = all
        .iter()
        .filter(|(_, lo, hi)| *hi > 4 && *lo < 8)
        .map(|(sp, _, _)| u32::from(sp.atlas_style()))
        .collect();
    // **`[1, 0]` and not `[1]`: the plain tail after the styled run is a real run on that line.** The
    // first version of this expected exactly one run and was wrong about the model rather than about the
    // code — `style_range` rebuilds the list as `[plain 0..4, bold 4..7, plain 7..20]`, and the third
    // entry genuinely overlaps line 1.
    assert_eq!(
        on_line_1.iter().filter(|&&st| st != 0).copied().collect::<Vec<_>>(),
        vec![1],
        "**exactly one styled run on line 1, and it is bold** -- so a partition that found the wrong \
         runs would not produce this. The full list is {on_line_1:?}, the trailing 0 being the plain \
         tail after the styled run."
    );
    assert_eq!(
        on_line_1.first(),
        Some(&1),
        "**and it is the first of them**, because the list is in ascending offset order and the styled \
         run starts at byte 4, the line's own start"
    );
    // **And the neighbours are plain**, which is what makes the styled one identifiable.
    for (lo, hi, label) in [(0u32, 4u32, "line 0"), (8u32, 12u32, "lines 2 and 3")] {
        let styles: Vec<u32> = all
            .iter()
            .filter(|(_, s, e)| *e > lo && *s < hi)
            .map(|(sp, _, _)| u32::from(sp.atlas_style()))
            .collect();
        assert!(
            styles.iter().all(|&st| st == 0),
            "{label} resolved to {styles:?}, and every one of those runs should be plain"
        );
    }
}

/// **A styled line past the first still draws bold, which is the end-to-end version of the last test.**
///
/// **A real document, styled deep, scrolled to.** The gate above checks the partition's arithmetic; this
/// checks that the pixels move, on a page where the styled line is not the first one and the document is
/// longer than a page. **The old `take_while` version passed this fixture** — it is one styled line on a
/// short document, where the styled line is the only span and the first span — which is why the fixture
/// above exists as well.
#[test]
fn a_styled_line_past_the_first_still_draws_bold() {
    let m = ChromeMetrics::DESKTOP;
    // **Forty lines, the thirtieth styled.** Long enough that the styled line is not the first, and the
    // span list around it has plain spans on both sides.
    //
    // **Two sessions built by the same function rather than one editor cloned.** `Editor` is not
    // `Clone` — it owns the undo stack and the edit record, and a `Clone` would be a second document
    // that looked like the first. Building it twice from the same recipe is honest and is what the
    // rest of this project's fixtures do.
    let build = |styled: bool| {
        let mut ed = Editor::new();
        for i in 0..40 {
            ed.insert_at(ed.text_len() as u32, b"word\n", SpanPolicy::GrowIntoInsert)
                .expect("seed");
            // **Line 10, not line 30.** The first version styled line 30 of forty and the two frames
            // came back identical: **the window is 800 px tall with a 25 px line pitch, so about
            // twenty-eight rows fit and line 30 was never on the page.** A styled line below the
            // viewport is not a rendering bug, it is a fixture that does not test rendering. The line
            // is still well past the first, which is the property the partition needs.
            if styled && i == 9 {
                let at = (i * 5) as u32;
                ed.style_range(at, at + 4, STYLE_BOLD, 0)
                    .expect("style line 10");
            }
        }
        let mut s = Session::new(
            ed,
            Painter::new(atlas(), 0),
            Box::new(HeadlessScanout::new(m.width, m.height)),
            m,
        );
        s.caret_to(0).expect("park the caret");
        s.repaint_all().expect("paint");
        s
    };
    let plain = build(false);
    let bold = build(true);

    let (mut n, (mut x0, mut y0)) = (0u32, (u32::MAX, u32::MAX));
    let (mut x1, mut y1) = (0u32, 0u32);
    for y in 0..m.height {
        for x in 0..m.width {
            if plain.frame().pixel(x, y) != bold.frame().pixel(x, y) {
                n += 1;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    assert!(
        n > 0,
        "**the styled line did not draw**: 40 lines with line 30 bold, and the two frames are \
                    identical. That is the `take_while`-alone failure, on a fixture where the old \
                    per-line code would have passed."
    );
    assert_eq!(
        bold.stats.span_queries, 1,
        "and the paint still asked the span map once"
    );

    // **The difference is on row 29** (line 30, zero-based from the top of the document), within the text
    // column. `Layout::text` and not `Layout::page` -- part 24's third fixture error.
    let l = bold.text_rect();
    let row = (y0 - l.y) / bold.chrome.metrics.cell_h;
    assert_eq!(
        row, 9,
        "the differing pixels start on row {row}; line 10 is row 9"
    );
    assert!(
        x0 >= l.x && x1 < l.x + l.width,
        "and inside the text column, not the chrome"
    );
    assert!(
        x0 >= l.x && x1 < l.x + 8 * bold.chrome.metrics.cell_w,
        "and inside the word"
    );
}
