//! **The chrome is a fixed grid and the document is proportional, and they have been mixed in five
//! places.** 4 tests.
//!
//! # PART 32 CORRECTED EVERY NUMBER IN THIS FILE
//!
//! Part 30 measured all four of these and left them as defect gates, with the decision of what to do
//! about them deferred. **Part 32 took decision (b)** -- put the chrome on the font's grid -- and
//! [`Painter::cell_width`] now returns the Monospace face's own advance instead of `ppem / 2`. **Two
//! of the four gates failed, which is what they were written to do**, and the other two kept passing.
//!
//! ## The lesson from the two that kept passing, which is the more useful half
//!
//! **Every gate in this file read `ChromeMetrics::DESKTOP.cell_w`, and `DESKTOP` is a design constant
//! that no running session uses.** `Session::new` overrides `cell_w` from the face's advance, so
//! `DESKTOP.cell_w` stayed 8 while the running value became 10 -- and
//! `cell_width_is_not_the_monospace_advance_it_claims_to_be` went on asserting that the cell was 8
//! **through the entire change that made it 10.** It is the same failure as
//! `CHROME_CELL_W` in part 32's other finding, in the same week, in a different file.
//!
//! **A gate that reads a design constant cannot see the running value.** Every gate below now reads
//! `session.chrome.metrics`, which is the thing that actually lays the text out.
//!
//! # The numbers now, measured at 16 ppem
//!
//! | | before (part 30) | **now** |
//! | --- | --- | --- |
//! | `Painter::cell_width()` | `ppem / 2` = 8 | **the Monospace advance = 10** |
//! | `ChromeMetrics::DESKTOP.cell_w` | 8 | 8 -- *and no longer the value in use* |
//! | a **running session's** `cell_w` | 8 | **10** |
//! | Monospace face, every glyph in `a-z A-Z 0-9` | advance 10 | advance 10 |
//! | Regular face, `a-z` | advance 4-16, mean 8.58 | advance 4-16, mean 8.58 |
//! | `'a'`, Regular | advance 9 | advance 9 |
//! | **40 × `'a'`, rendered** | ink 358, grid **320** | ink 358, grid **400** |
//! | **`alpha bravo charlie`** | ink 143, grid **160** | ink 143, grid **190** |
//! | **40 × `'W'`** | -- | ink 638, grid **400** |
//!
//! # What part 30 decided and part 32 did
//!
//! **The cell is now the face's advance, so the doc it always claimed to honour is true.** That fixes
//! the *grid's* honesty -- the ruler, the tables, the wrap point and the measure all now speak in the
//! monospace face's own unit -- and it is why 40 identical glyphs now **fit** rather than overflow.
//!
//! **It does not make the caret right, and the caret is the reason this file still exists.** The body
//! text is Inter, which is proportional, so no fixed grid can place a caret inside it: `'a'` advances
//! 9 and `'W'` 16 in the same cell of 10. **The caret is now 10, 20 and 40 px *ahead* of the glyph it
//! points at**, where before the 8 px cell put it behind. **The sign of the error moved and the error
//! is still unbounded.**
//!
//! # Where the two are still mixed
//!
//! | place | uses |
//! | --- | --- |
//! | `Painter::doc_text` | **`m.advance_x`** -- the font's metric |
//! | `Painter::text` (chrome labels) | `cell_width()` -- the grid |
//! | `Caret::locate` | `text.x + caret_column * cell_w` -- the grid |
//! | `Session::emit_body_text` run positions | the font's pen (part 31) |
//! | the selection highlight (part 28) | `col * cell_w` -- the grid |
//! | the ruler, the tables, the wrap point | `cell_w` -- the grid |
//!
//! | what it proves | test |
//! | --- | --- |
//! | **the grid is now the face's own advance** | [`cell_width_is_the_monospace_advance_it_claims_to_be`] |
//! | **and part 33 put the caret on the pen** | [`the_caret_is_at_the_pen_and_not_where_the_grid_would_put_it`] |
//! | **40 identical glyphs now fit inside 40 cells** | [`forty_identical_glyphs_fit_inside_forty_cells`] |
//! | **and the error still changes sign with the letters** | [`the_error_changes_sign_with_the_letters`] |
//! | **and the caret was therefore in the wrong place** | [`the_caret_is_at_the_pen_and_not_where_the_grid_would_put_it`] |

use holonomy::session::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_render::chrome::{Caret, ChromeMetrics};
use holonomy_text::{Editor, SpanPolicy};

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (a, _) = holonomy_assets::build_atlas(&[16]).expect("atlas");
        Box::leak(Box::new(a))
    })
}

fn painted(text: &str, caret: usize) -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    ed.insert_at(0, text.as_bytes(), SpanPolicy::GrowIntoInsert)
        .expect("seed");
    let mut s = Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    );
    s.caret_to(caret).expect("caret");
    s.repaint_all().expect("paint");
    s
}

/// The horizontal extent of the document's ink on the first text row.
fn ink_span(s: &Session<'_>) -> (u32, u32) {
    let l = s.chrome_layout().text;
    let (mut lo, mut hi) = (u32::MAX, 0u32);
    for x in l.x..l.x + l.width {
        for y in l.y..l.y + s.chrome.metrics.cell_h {
            if s.frame().pixel(x, y) & 0x00FF_FFFF < 0x0060_0000 {
                lo = lo.min(x);
                hi = hi.max(x);
            }
        }
    }
    (lo, hi)
}

/// **The running cell width is the Monospace face's own advance, and the doc it always carried is now
/// true.** Part 32. This gate's predecessor asserted the opposite and asserted that the cell was 8.
///
/// ## What it asserted, and why it kept passing
///
/// ```text
/// assert_eq!(ChromeMetrics::DESKTOP.cell_w, 8, "and `cell_width` is `ppem / 2` = 8")
/// ```
///
/// **`DESKTOP` is a design constant and `Session::new` overwrites `cell_w`.** So this gate watched a
/// number nothing lays out with, held its value across the very change that made the running value
/// different, and reported a pass. **This is the second time in one part that a "cannot drift" gate
/// has compared a constant with something that is not in use** -- the first was `CHROME_CELL_W`
/// against `DESKTOP.cell_w`, which compares the constant with itself.
///
/// ## What replaces it
///
/// **The running value, and the face's measurement, compared.** If the cell ever stops being the
/// face's advance, this fails -- which is the direction part 30 asked for.
#[test]
fn cell_width_is_the_monospace_advance_it_claims_to_be() {
    let a = atlas();
    let mut all_ten = true;
    for cp in (b'a' as u32..=b'z' as u32)
        .chain(b'A' as u32..=b'Z' as u32)
        .chain(b'0' as u32..=b'9' as u32)
    {
        let m = a.metric(cp, holonomy_assets::payload::Style::Monospace, a.sizes()[0]);
        if m.advance_x != 10 {
            all_ten = false;
        }
    }
    assert!(
        all_ten,
        "the Monospace face's advance is 10 for every Latin letter and digit at 16 ppem, measured \
         from the atlas"
    );

    // **The running session's cell, not `DESKTOP`'s.** This is the whole correction.
    let s = painted("x", 0);
    assert_eq!(
        s.chrome.metrics.cell_w, 10,
        "**and a running session's cell is that advance.** Part 31 read `ppem / 2` and part 32 \
         reads the face; the doc -- 'half the ppem, which is a monospace advance for the packed \
         faces' -- is now true of the code as well as of the doc."
    );

    // **And the constant it used to be is still 8, which is the trap.** Recorded so the next reader
    // does not "fix" `DESKTOP` and break a hundred gates that legitimately quote it as the design.
    assert_eq!(
        ChromeMetrics::DESKTOP.cell_w, 8,
        "`DESKTOP.cell_w` is still 8 and is *not* what lays the text out: it is the no-atlas \
         fallback and the 16 ppem design. Asserted here so that the gate above is visibly reading a \
         different number, and so nobody reads `DESKTOP` as the running cell again."
    );
}

/// **Forty `a`s now fit inside forty cells, where before they overflowed by 38 px.**
///
/// ## What it asserted, and what it measures now
///
/// Part 30's version held that **40 × `'a'` renders 358 px against a grid of 320** -- 38 px *over* --
/// and named that "the measurement that breaks the coincidence", because every other fixture in the
/// project uses a line of mixed letters where the narrow and the wide cancel.
///
/// **Now the grid is 400, because the cell is the monospace advance and `'a'` is a *Regular* glyph
/// that advances 9.** So the coincidence is broken the other way: **358 px of ink in a 400 px
/// measure, 42 px of slack**, and the caret at the end of the line lands 40 px past the last glyph.
///
/// **The gap is no longer a disagreement about how wide `a` is; it is the difference between two
/// faces.** That is the residual this file exists to hold, and it is why decision (b) was necessary
/// and not sufficient.
#[test]
fn forty_identical_glyphs_fit_inside_forty_cells() {
    let n = 40u32;
    let s = painted(&"a".repeat(n as usize), 0);
    let cell_w = s.chrome.metrics.cell_w;
    let (lo, hi) = ink_span(&s);
    let drawn = hi - lo;
    let grid = n * cell_w;

    assert_eq!(
        cell_w, 10,
        "the cell is the monospace advance, which is what makes this gate's arithmetic below mean \
         something"
    );
    assert_eq!(
        drawn, 358,
        "**40 `'a'`s render 358 px** -- part 30 measured this too, and it is unchanged, because the \
         document's advance has not moved. Only the grid it is measured against has."
    );
    assert_eq!(
        grid, 400,
        "**and the grid is now 400**, 40 cells of 10, where it was 320."
    );
    assert!(
        drawn < grid,
        "**so forty identical glyphs now fit inside forty cells**, {drawn} in {grid}. Part 30's \
         version of this gate asserted the opposite and failed with exactly these numbers, which is \
         what it was written to do."
    );
    // **And the slack is the face difference, stated.** Inter's `'a'` advances 9; the cell is the
    // *Monospace* face's 10. **One pixel a glyph is 40 px at the end of a line, and it is the
    // caret's error** -- which `the_caret_is_where_the_grid_says_not_where_the_text_is` asserts.
    let advance = u32::from(
        atlas()
            .metric(b'a' as u32, holonomy_assets::payload::Style::Regular, 16)
            .advance_x,
    );
    assert_eq!(
        advance, 9,
        "`'a'` advances 9 px in Regular at 16 ppem, measured from the atlas"
    );
    let pen = n * advance;

    // **Three numbers, and the first draft conflated two of them.**
    //
    // It asserted `grid - drawn == 40 * (cell_w - advance)`, which predicted **40** against a
    // measured **42**. The arithmetic was wrong, not the pixels: `drawn` is the *ink span*, from the
    // first glyph's left bearing to the last glyph's right overhang, while `n * advance` is the
    // *pen* -- and ink and pen are different quantities that differ by up to one glyph's width at
    // each end. Part 30's file made the same sort of move and part 31 corrected it.
    //
    // **So the claim is the one that is true and checkable**: the pen is 360, the grid allows 400,
    // and the ink is within one glyph of the pen.
    assert_eq!(pen, 360, "**the pen advances 360 px** -- 40 advances of 9");
    assert_eq!(
        grid - pen, 40,
        "**and the grid allows 400, so the pen falls 40 px short of the measure**: one pixel a glyph"
    );
    assert!(
        (drawn as i64 - pen as i64).abs() <= i64::from(atlas()
            .metric(b'a' as u32, holonomy_assets::payload::Style::Regular, 16)
            .width),
        "**and the ink, {drawn} px, is within one glyph's width of the pen** -- {} px away, against a \
         glyph {} px wide. Ink extent is not the pen; conflating the two is what made the first \
         draft of this assertion predict 40 and the frame render 42.",
        (drawn as i64 - pen as i64).abs(),
        atlas().metric(b'a' as u32, holonomy_assets::payload::Style::Regular, 16).width
    );
    let _ = lo;
}

/// **The error's sign still depends on the line's letters, which is worse than a constant offset.**
///
/// ## Part 30's measurement, and part 32's
///
/// ```text
///                        part 30 (cell 8)        part 32 (cell 10)
/// alpha bravo charlie    143 px, grid 160  -17    143 px, grid 190  -47
/// 40 x 'a'               358 px, grid 320  +38    358 px, grid 400  -42
/// 40 x 'W'               --                          638 px, grid 400  +238
/// ```
///
/// **Part 32 changed the size of every one of these and the sign of one.** The mixed line went from
/// 17 px short to 47 px short; forty `a`s went from 38 px long to 42 px short; and forty `W`s are
/// **238 px long**. **The reason the sign flipped for `a` is that the cell and the glyph are
/// different faces now** -- it was a narrower *grid* before, and a narrower *glyph* now, and the
/// arithmetic happens to agree.
///
/// **Which is the point of this gate.** A constant offset can be lived with: a user learns it, or
/// it is a one-line correction. **A sign that depends on the letters cannot**, because the caret is
/// past the end of one line and inside the text of the next, and nothing in the chrome can know which
/// without summing advances -- which is what part 30's decision (b) does for the grid and cannot do
/// for a proportional face.
#[test]
fn the_error_changes_sign_with_the_letters() {
    let mixed = painted("alpha bravo charlie", 0);
    let cell_w = mixed.chrome.metrics.cell_w;
    let (mlo, mhi) = ink_span(&mixed);
    let mixed_ink = mhi - mlo;
    assert_eq!(
        mixed_ink as i64 - (19 * cell_w) as i64,
        -47,
        "**the mixed line's ink is 143 px against the grid's 190** -- forty-seven pixels short, so a \
         caret at column 19 is drawn past the end of the text. Part 30 measured this at -17 against \
         a 160 px grid; the ink has not moved and the grid has doubled its shortfall."
    );

    // **The other sign, from a glyph that is genuinely wide.** `'W'` is 16 in Regular and the cell
    // is 10, so forty of them overrun by six pixels each.
    let w = painted(&"W".repeat(40), 0);
    let (wlo, whi) = ink_span(&w);
    let wide_ink = whi - wlo;
    assert_eq!(
        wide_ink as i64 - (40 * cell_w) as i64,
        238,
        "**and forty `W`s overrun by 238 px** -- the other way, and much the larger, because `'W'` \
         advances 16 in a cell of 10"
    );
    assert!(
        (mixed_ink as i64 - (19 * cell_w) as i64) * (wide_ink as i64 - (40 * cell_w) as i64) < 0,
        "**so the error has no sign**: the caret is past the end of one line and inside the text of \
         another, and no constant correction fixes it"
    );
    let _ = (mlo, wlo);
}

/// **The caret is at the pen, and it is exactly right. Part 33 fixed this gate's predecessor.**
///
/// ## What this was, and the numbers it held
///
/// ```text
///                            part 30        part 32        part 33
/// caret at column 10         10 px behind   10 px ahead    0 px
/// ```
///
/// **The gate asserted the defect in all three columns** — that `Caret::locate` is
/// `text.x + caret_column * cell_w`, and therefore somewhere other than where the glyph is. **Part 33
/// gave the caret a pen**, so it failed with **330 against 340**: the caret is at `text.x + 90`, which
/// is ten advances of 9, and the grid's 340 is ten cells of 10.
///
/// **It is flipped, not deleted, and the flip is the whole claim**: the same measurement with the sign
/// removed. A defect gate that is deleted leaves the fact unrecorded; one that is weakened is how a
/// defect gate becomes a lie.
///
/// ## Why it is now exactly right, and why that was not free
///
/// **The pen sums each glyph's own advance**, so it is right for `'a'` and `'W'` alike — which is the
/// whole of what part 30 said no fixed cell could do. **Two things had to change together and neither
/// is enough alone**, and that is the finding worth recording:
///
/// * **the caret is placed by the pen** (`ChromeState::caret_pen_x`, written by the session's walk),
///   **and**
/// * **a click is resolved by the pen** (`Session::column_at_pen`).
///
/// **Placing the caret by the pen while hit-testing by the grid is worse than either being wrong.**
/// A caret drawn 10 px from the glyph but landing where the user clicked is a caret that does not go
/// where the pointer went, and `a_click_and_the_drawn_cell_agree_in_both_directions` caught exactly
/// that: **249 against 250** on the first run.
///
/// ## What is still on the grid, and is not in this file any more
///
/// **The ruler's ticks and the tables' cells.** They are chrome furniture in the Monospace face's
/// unit and they are *right* there — a ruler that counts cells is counting the chrome's grid, which is
/// the grid the chrome is drawn on. **The wrap point and the truncation** are in cells too, and the
/// measure is 80 of them; at 800 px of column and a mean advance of 8.58 that is 93 glyphs, so a long
/// line is clipped a little before the page edge rather than a little after it, which is the safe
/// direction to be wrong in.
#[test]
fn the_caret_is_at_the_pen_and_not_where_the_grid_would_put_it() {
    let n = 40u32;
    let mut s = painted(&"a".repeat(n as usize), 0);
    let l = s.chrome_layout().text;
    let cell_w = s.chrome.metrics.cell_w;
    let (_, hi) = ink_span(&s);

    // **Column 10.**
    s.caret_to(10).expect("caret");
    let cell = Caret::locate(&s.chrome_layout(), &s.chrome.metrics, &s.state)
        .expect("on screen")
        .cell
        .x;
    // **Where the text is at column 10: ten advances of 9.**
    let advance = u32::from(
        atlas()
            .metric(b'a' as u32, holonomy_assets::payload::Style::Regular, 16)
            .advance_x,
    );
    assert_eq!(
        advance, 9,
        "`'a'` advances 9 px at 16 ppem, measured from the atlas"
    );
    let true_x = l.x + 10 * advance;
    assert_eq!(
        cell, true_x,
        "**the caret is at the pen: {0} px in, which is ten advances of 9.** Part 30 measured this \
         gate at 10 px *behind* the glyph and part 32 at 10 px *ahead*, because the cell was 8 and \
         then 10 while the glyph was always 9.",
        cell - l.x
    );
    // **And demonstrably not the grid**, which is what makes it a measurement rather than a tautology.
    assert_eq!(cell, l.x + 90, "and 90 is not 10 cells of 10");
    assert_ne!(
        cell,
        l.x + 10 * cell_w,
        "**so this is not `text.x + column * cell_w`**, which would be {}. The grid is still the \
         chrome's unit and it is still used for the ruler and the tables; it is no longer used to \
         place a caret inside proportional text.",
        l.x + 10 * cell_w
    );
    // **And the end of the line.** A caret at column 40 is at the pen of forty advances.
    s.caret_to(40).expect("caret");
    let end = Caret::locate(&s.chrome_layout(), &s.chrome.metrics, &s.state)
        .expect("on screen")
        .cell
        .x;
    assert_eq!(
        end,
        l.x + 40 * advance,
        "**and so is the end of the line: {} px in, against an ink that ends at {}.** Forty glyphs \
         of 9 is 360 and the grid's forty cells of 10 is 400, so the old expression drew the caret 40 \
         px past the last glyph and this one draws it on the glyph's own trailing edge.",
        end - l.x,
        hi - l.x
    );
    // **And a wide glyph moves it the other way, which is the part no grid could ever do.**
    let mut w = painted(&"W".repeat(10), 0);
    w.caret_to(10).expect("caret");
    let wpen = Caret::locate(&w.chrome_layout(), &w.chrome.metrics, &w.state)
        .expect("on screen")
        .cell
        .x;
    let w_adv = u32::from(
        atlas()
            .metric(b'W' as u32, holonomy_assets::payload::Style::Regular, 16)
            .advance_x,
    );
    assert_eq!(w_adv, 16, "`W` advances 16 px, measured from the atlas");
    assert_eq!(
        wpen,
        w.chrome_layout().text.x + 10 * w_adv,
        "**ten `W`s put the caret at 160 px**, where ten cells of 10 would say 100 and ten `a`s say \
         90. Three glyphs, three answers, and only one of them is a fixed grid."
    );
}
