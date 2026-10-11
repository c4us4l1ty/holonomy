//! **The chrome is a fixed grid and the document is proportional, and they have been mixed in five
//! places.** 4 tests.
//!
//! # The numbers, measured at 16 ppem
//!
//! | | |
//! | --- | --- |
//! | `Painter::cell_width()` | **`ppem / 2` = 8** |
//! | `ChromeMetrics::DESKTOP.cell_w` | **8** |
//! | Monospace face, every glyph in `a-z A-Z 0-9` | **advance 10**, ink 9–11 |
//! | Regular face, same glyphs | **advance 4–16, mean 9.69**, ink mean **10.24** |
//! | `'a'`, Regular | **advance 9**, ink **9** |
//! | **40 × `'a'`, rendered** | **ink 358 px** where the grid says **320 px** |
//!
//! # Why it survived so long
//!
//! **Because the caret is drawn, not the text, at the grid position — and the caret is one pixel
//! wide.** Its error is invisible unless you know where the glyph it points at is, and nothing in the
//! project measures that.
//!
//! **It is worse than a constant offset.** `alpha bravo charlie` renders **17 px narrower** than the
//! grid and twenty `a`s render **18 px wider**. **The error's sign depends on the letters**, so the
//! caret is past the end of one line and inside the text of the next, and there is no constant a user
//! could learn to compensate for.
//!
//! # Where the two are mixed
//!
//! | place | uses |
//! | --- | --- |
//! | `Painter::doc_text` | **`m.advance_x`** — the font's metric |
//! | `Painter::text` (chrome labels) | `cell_width()` — the grid |
//! | `Caret::locate` | `text.x + caret_column * cell_w` — the grid |
//! | `Session::emit_body_text` run positions | `text.x + cell_x * cell_w` — the grid |
//! | the selection highlight (part 28) | `col * cell_w` — the grid |
//! | the ruler, the tables, the wrap point | `cell_w` — the grid |
//!
//! **So a run boundary is a discontinuity, which is part 29's finding and this file's cause.**
//!
//! | what it proves | test |
//! | --- | --- |
//! | **the grid and the text disagree, by 38 px on 40 glyphs** | [`forty_identical_glyphs_are_wider_than_the_grid_allows`] |
//! | **and the error changes sign with the letters** | [`the_error_changes_sign_with_the_letters`] |
//! | **and the caret is therefore in the wrong place** | [`the_caret_is_where_the_grid_says_not_where_the_text_is`] |
//! | **and `cell_width`'s own doc is false** | [`cell_width_is_not_the_monospace_advance_it_claims_to_be`] |

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

/// **Forty `a`s are wider than forty cells, and by an amount a gate should not have to tolerate.**
///
/// **This is the measurement that breaks the coincidence.** Every other fixture in the project that
/// compares a caret or a highlight to text uses a line of mixed letters, where the narrow and the wide
/// roughly cancel. **Forty of the same letter do not cancel.**
#[test]
fn forty_identical_glyphs_are_wider_than_the_grid_allows() {
    let n = 40u32;
    let s = painted(&"a".repeat(n as usize), 0);
    let cell_w = s.chrome.metrics.cell_w;
    let (lo, hi) = ink_span(&s);
    let drawn = hi - lo;
    let grid = n * cell_w;

    assert!(
        drawn > grid,
        "**{n} identical glyphs occupy {drawn} px and the grid allows {grid}** ({n} cells of \
         {cell_w}). If this gate fails, the painter and the grid agree -- the document is on the \
         chrome's grid, and the run-boundary shift part 29 recorded is gone with it."
    );
    // **The gap, named.** 38 px on 40 glyphs is 0.95 px per glyph, which is `advance_x` (9) minus
    // `cell_w` (8). **Asserting the exact figure would enshrine a font metric that a new face would
    /// change**, so this asserts the *size* of the disagreement and leaves the reason to the doc above.
    assert!(
        drawn - grid > 20,
        "**the disagreement is {0} px, which is not a rounding artefact**",
        drawn - grid
    );
}

/// **The error's sign depends on the line's letters, which is worse than a constant offset.**
///
/// ## CORRECTION — the first draft of this gate claimed a coincidence and was wrong
///
/// It asserted that `alpha bravo charlie` lands within a few pixels of the grid, and called that the
/// reason the defect survived. **The measurement that supported it was 159 px against a grid of 160,
/// and it was the caret being counted as ink** — the fixture painted with the caret at the end of the
/// line, and the caret is drawn at the grid position, which is precisely the thing under question.
///
/// **Without the caret the text's ink is 143 px against a grid of 160. There is no coincidence; the
/// mixed line is 17 px *narrower* than the grid** — and forty `a`s are 38 px *wider*.
///
/// ## Why that is worse
///
/// **A constant offset can be lived with.** A document whose text is uniformly 17 px off has a caret
/// that is uniformly in the wrong place, and a user learns where to look. **An error whose sign depends
/// on the letters means the caret is wrong by a different amount on every line**, and the user has no
/// rule to learn: it is near right on the lines where the narrow and wide letters happen to cancel,
/// which is why no gate noticed.
///
/// ## The two numbers
///
/// ```text
/// alpha bravo charlie : ink 143 px, grid 160 px   -- 17 px SHORT
/// a x 20             : ink 178 px, grid 160 px   -- 18 px LONG
/// ```
#[test]
fn the_error_changes_sign_with_the_letters() {
    let cell_w = ChromeMetrics::DESKTOP.cell_w;
    let grid = 20 * cell_w;

    let mixed = painted("alpha bravo charlie", 0);
    let (mlo, mhi) = ink_span(&mixed);
    let mixed_ink = mhi - mlo;
    assert_eq!(
        mixed_ink as i64 - grid as i64,
        -17,
        "**the mixed line's ink is 143 px against the grid's 160** -- seventeen pixels short, so a \
         caret at column 20 is drawn past the end of the text"
    );

    let uniform = painted(&"a".repeat(20), 0);
    let (ulo, uhi) = ink_span(&uniform);
    let uniform_ink = uhi - ulo;
    assert_eq!(
        uniform_ink as i64 - grid as i64,
        18,
        "**and twenty `a`s are eighteen pixels long** -- the other way"
    );
    assert!(
        (mixed_ink as i64 - grid as i64) * (uniform_ink as i64 - grid as i64) < 0,
        "**so the error has no sign**: the caret is past the end of one line and inside the text of \
         another, and no constant correction fixes it"
    );
}

/// The caret is where the grid says, and the grid is not where the text is.
///
/// **The user-visible consequence.** A caret at column 20 of a line of forty `a`s is drawn at
/// `text.x + 160` and the text runs to `text.x + 358` — **the caret is 198 px behind the end of the
/// line it belongs to**, and a caret at column 10 is drawn inside the fourteenth glyph.
#[test]
fn the_caret_is_where_the_grid_says_not_where_the_text_is() {
    let n = 40u32;
    let mut s = painted(&"a".repeat(n as usize), 0);
    let l = s.chrome_layout().text;
    let cell_w = s.chrome.metrics.cell_w;
    let (lo, hi) = ink_span(&s);

    // **Column 10.**
    s.caret_to(10).expect("caret");
    let cell = Caret::locate(&s.chrome_layout(), &s.chrome.metrics, &s.state)
        .expect("on screen")
        .cell
        .x;
    let grid_x = l.x + 10 * cell_w;
    assert_eq!(
        cell,
        grid_x,
        "`Caret::locate` is `text.x + caret_column * cell_w`, and that is where it puts the caret"
    );
    // **Where the text actually is at column 10: ten advances of 9, not ten cells of 8.**
    let advance =
        u32::from(atlas().metric(b'a' as u32, holonomy_assets::payload::Style::Regular, 16).advance_x);
    assert_eq!(advance, 9, "`'a'` advances 9 px at 16 ppem, measured from the atlas");
    let true_x = l.x + 10 * advance;
    assert_eq!(
        true_x - cell,
        10,
        "**the caret is 10 px behind the glyph it points at** -- and that grows by 1 px for every \\
         further column, reaching {} px at column 40",
        40 * (advance - cell_w)
    );
    assert!(
        cell < hi,
        "and it is drawn well before the end of the line: {} against the ink's {}",
        cell,
        hi
    );
    let _ = lo;
}

/// **`cell_width` is documented as a monospace advance and is `ppem / 2`.**
///
/// The doc, verbatim:
///
/// > The cell width. Half the ppem, which is a monospace advance for the packed faces.
///
/// **Measured, the Monospace face's advance at 16 ppem is 10 for every glyph in `a-z A-Z 0-9`, and half
/// the ppem is 8.** The claim is false and the code is a two-pixels-per-glyph guess wearing a
/// measurement's clothes.
///
/// **Asserting the false claim rather than the correct value**, because part 30 does not change
/// `cell_width`: doing so moves every x in the project and every number §2.9.3 measured against a
/// 640 px page column. When it is changed this gate fails, which is the direction that is useful.
#[test]
fn cell_width_is_not_the_monospace_advance_it_claims_to_be() {
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
        "the Monospace face's advance is 10 for every Latin letter and digit at 16 ppem"
    );
    assert_eq!(
        ChromeMetrics::DESKTOP.cell_w,
        8,
        "**and `cell_width` is `ppem / 2` = 8**, so the chrome's grid is 2 px narrower than the face \
         it was documented to be measuring. Its own doc says this is a monospace advance; it is not."
    );
    // **20% out.** 8 against 10 is the ratio that makes a 40-glyph line 38 px long, and 38 px is
    // nearly five cells of error at the end of a line.
    assert_eq!(
        (10 - ChromeMetrics::DESKTOP.cell_w) * 100 / 10,
        20,
        "**the grid is 20% narrower than the monospace advance it claims to be**"
    );
}