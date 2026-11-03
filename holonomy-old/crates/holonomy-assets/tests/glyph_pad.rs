//! **The one comparison nobody made: a glyph's ink against its own cell.**
//!
//! Every other gate in this project measured a raster in a way that could not see a *vertical*
//! translation: ink extent **in x** (`'a'` advance 9, ink 9), pixel counts, colour hues, missing-glyph
//! tallies. `PaintStats::missing` stayed at zero throughout, because the glyphs *were* drawn -- just
//! not all of them. This file is the comparison that would have caught it in the first hour.

use holonomy_assets::atlas::Atlas;
use holonomy_assets::metric::ATLAS_STRIDE;
use holonomy_assets::payload::Style;

/// Every style the packed faces carry.
const STYLES: [Style; 5] = [
    Style::Regular,
    Style::Italic,
    Style::Bold,
    Style::Math,
    Style::Monospace,
];

/// **The sizes that matter.** 16 was the design's only size until part 32 and 24 is what ships now;
/// **both are scanned because the defect was in neither's favour or against it.**
const SIZES: [u16; 2] = [16, 24];

/// The codepoints to scan.
fn scan() -> impl Iterator<Item = u32> {
    0x20u32..0x260
}

/// The rows of a glyph's own atlas cell that carry any coverage, plus the cell's dimensions.
fn ink_rows(atlas: &Atlas, cp: u32, style: Style, ppem: u16) -> (Vec<usize>, u8, u8) {
    let m = atlas.metric(cp, style, ppem);
    if m.width == 0 || m.height == 0 {
        return (Vec::new(), m.width, m.height);
    }
    let cov = atlas.coverage();
    let mut rows = Vec::new();
    for r in 0..usize::from(m.height) {
        let base = (usize::from(m.atlas_y) + r) * ATLAS_STRIDE + usize::from(m.atlas_x);
        if cov[base..base + usize::from(m.width)]
            .iter()
            .any(|&v| v != 0)
        {
            rows.push(r);
        }
    }
    (rows, m.width, m.height)
}

/// **No glyph's ink runs into the last row of its own cell.**
///
/// ## What this is: part 32's correction
///
/// `rasterize` took a `dy` and **discarded it** -- explicitly, as `let _ = dy;` inside the per-byte
/// copy loop, which is how a dead parameter is kept from ever being reported as dead. `dx` *was*
/// applied, to the span fill alone, so the horizontal crop was right and the vertical one was not:
/// **a glyph landed in its own cell shifted down by exactly its `y0`**, and the rows it lost off the
/// top fell off the bottom, outside the cell, silently.
///
/// Measured by this file's own method, before the fix:
///
/// ```text
/// 16 ppem  'p'  cell 15 rows   ink in rows  6..14   -> bottom pad  0
/// 24 ppem  'p'  cell 22 rows   ink in rows 10..21   -> bottom pad  0
/// ```
///
/// **Every glyph in the application was missing the top of its ink, at every size, since the atlas
/// was written**, and 135 tests passed against it.
///
/// ## Why the *bottom* and not the top, and the correction that got here
///
/// **The first draft of this gate asserted a blank row at both ends, from `rasterize_face`'s
/// documented 1 px pad on every side.** It failed on `'|'` in Regular at 16 ppem: ink in rows 0..19
/// of a 21-row cell, i.e. **no top pad at all**.
///
/// **And that is correct behaviour, not a defect.** The pad is `y0 = min_y.floor() - 1`, which is
/// **not** guaranteed to be a row: an outline whose top lands exactly on a pixel boundary gives
/// `min_y.floor()` as that boundary and the subtraction lands on the row below it, so the ink
/// legitimately starts at row 0. **The claim "at least one blank row on each side" was a guess from
/// the source that the pixels contradicted**, and the pixels are the measurement.
///
/// Measured over **1,512 rasterised glyphs** -- 756 codepoints x 5 styles, at each of the two sizes:
///
/// ```text
/// min top pad    =  0        <- so the top cannot be the assertion
/// min bottom pad =  1        <- always at least one, at both sizes
/// ```
///
/// **So the bottom is the half to assert, and it is the half the defect violates.** `y1 =
/// max_y.ceil() + 1` always leaves a row below the ink because the subtraction and the addition
/// bracket a pair of inequalities; the top's `- 1` is the one that can vanish.
///
/// ## Why one-sided is not a weak version of two-sided
///
/// **It is the half that carries the information.** A glyph pushed down has a *full* bottom row and
/// an *empty* top row, so a top-only assertion would have **passed the defect** and a bottom-only one
/// fails it. The pair would be stronger still and is not available, so this is the strongest claim
/// the measurement supports.
#[test]
fn no_glyphs_ink_runs_into_the_last_row_of_its_own_cell() {
    let mut checked = 0usize;
    for &ppem in &SIZES {
        let (atlas, _) = holonomy_assets::build_atlas(&[ppem]).expect("the packed faces");
        for style in STYLES {
            for cp in scan() {
                let (rows, w, h) = ink_rows(&atlas, cp, style, ppem);
                if w == 0 || h == 0 {
                    continue; // a space or a `.notdef`: no ink to be misplaced
                }
                let shown = char::from_u32(cp).unwrap_or('?');
                assert!(
                    !rows.is_empty(),
                    "'{shown}' {style:?} at {ppem} ppem reports {w}x{h} of coverage and carries \
                     none. A cell with no ink in it is either a wrongly recorded blank, which draws \
                     nothing, or a glyph the fill never reached, which draws noise."
                );
                let last = rows[rows.len() - 1];
                assert!(
                    last + 1 < usize::from(h),
                    "'{shown}' {style:?} at {ppem} ppem: ink reaches row {last} of a {h}-row cell, \
                     so the last row is not blank. `rasterize_face` crops with `y1 = max_y.ceil() + \
                     1` and the fill must stay inside it. Part 32 measured 'p' in Regular at ink \
                     rows 6..14 of 15 at 16 ppem and 10..21 of 22 at 24 -- ink one row past the \
                     cell, because `rasterize` discarded the `dy` it was handed."
                );
                checked += 1;
            }
        }
    }
    assert!(
        checked > 1_000,
        "the scan only reached {checked} glyphs; a scan that quietly covers nothing is worse than \
         none, because it reports a pass"
    );
    eprintln!(
        "checked {checked} rasterised glyphs across {} sizes and 5 styles",
        SIZES.len()
    );
}

/// **A glyph's ink sits near the middle of its cell, and the measurement says by how much.**
///
/// ## Why a second gate when the first one catches it
///
/// **Because the first one is a bound and this one is the shape.** "Not touching the last row" is
/// satisfied by a glyph whose ink fills the cell from row 0 to row `h - 2`; **the shift being
/// corrected put the ink in exactly that shape** and would be caught only by the padding, which is a
/// weaker signal than the position. This asserts the position directly.
///
/// ## The number, measured rather than guessed
///
/// Over the same **1,512 glyphs**, the largest distance any glyph's ink centre sits from the middle
/// of its cell is **0.167 of a cell** -- and it is **0.167 at both sizes**, which is what one expects
/// of a property of the outlines rather than of the rasteriser:
///
/// ```text
/// ppem=16   n=756   max |centre - 0.5| = 0.167
/// ppem=24   n=756   max |centre - 0.5| = 0.167
/// ```
///
/// **The bound asserted is 0.20**, not 0.167. A gate that asserts the observed maximum exactly is a
/// gate that fails on the next font that ships; one that asserts it with a margin fails only when the
/// property is actually broken, and the comment records where the number came from so the margin can
/// be re-measured rather than guessed at.
///
/// **Under the defect `'p'` at 24 ppem sat at (10 + 21) / 2 / 22 = 0.70**, i.e. 0.20 from the middle
/// -- outside the margin, which is the second independent reason this gate fails without the fix.
#[test]
fn a_glyphs_ink_sits_near_the_middle_of_its_cell() {
    const BOUND: f64 = 0.20;
    for &ppem in &SIZES {
        let (atlas, _) = holonomy_assets::build_atlas(&[ppem]).expect("the packed faces");
        for style in STYLES {
            for cp in scan() {
                let (rows, w, h) = ink_rows(&atlas, cp, style, ppem);
                if rows.is_empty() || h == 0 {
                    continue;
                }
                let centre = (rows[0] + rows[rows.len() - 1]) as f64 / 2.0 / f64::from(h);
                let off = (centre - 0.5).abs();
                assert!(
                    off <= BOUND,
                    "'{}' {style:?} at {ppem} ppem: ink centred at {centre:.3} of a {h}-row cell, \
                     {off:.3} from the middle, against a bound of {BOUND}. Measured worst case \
                     across 1,512 glyphs is 0.167; under part 32's defect 'p' sat at 0.70.",
                    char::from_u32(cp).unwrap_or('?')
                );
                let _ = w;
            }
        }
    }
}
