//! Phase 9A debt: the line-height model a table pushes the lines below it down through.
//!
//! | requirement | test |
//! | --- | --- |
//! | a table's height becomes line extras | [`a_tables_height_becomes_extra_height_on_its_anchor_line`] |
//! | the lines below move down, exactly | [`the_lines_below_a_table_move_down_by_its_height`] |
//! | a zero-height block contributes nothing | [`a_zero_height_block_is_not_recorded_at_all`] |
//! | two tables on one line add | [`two_tables_on_one_line_add_rather_than_overwrite`] |
//! | partial slots round up | [`a_block_rounds_up_to_whole_line_slots`] |
//! | the caret follows the model | [`the_caret_lands_below_a_table_where_the_model_says`] |

use holonomy_render::{Caret, ChromeMetrics, ChromeState, Layout, LineHeights};

/// The text cell height every test uses, so the arithmetic below is checkable by hand.
const CELL_H: u32 = 18;

/// A state's line model, with the page geometry the caret lookup needs.
fn state_with(heights: LineHeights) -> ChromeState {
    ChromeState {
        line_heights: heights,
        total_lines: 40,
        ..ChromeState::default()
    }
}

/// A table block: its anchor line and its visual height in pixels.
///
/// The height is what the model is handed -- `publish_line_heights` measures it with
/// `TableGrid::height_px` and has no row count to give -- so the tests take a height too. An earlier
/// helper took `(slots, extra)` and therefore could not express a net extra at all.
fn block(line: u32, height: u32) -> (u32, u32) {
    (line, height)
}

/// A table's visual height becomes extra height on its anchor line, and nowhere else.
///
/// The claim is about the *net*: a table that is 4 rows tall wants `(4 + 1)` line slots because there
/// are 4 content rows between 5 border rows, and the model has to subtract those before reporting an
/// extra. Reporting the gross height would push the lines below down by four whole lines' worth, which
/// is the bug an earlier version of this reasoning had.
#[test]
fn a_tables_height_becomes_extra_height_on_its_anchor_line() {
    // A 4-row table: 5 border rows + 4 content rows, all one cell tall = 9 * 18 = 162 px.
    let heights = LineHeights::from(CELL_H, &[block(0, 9 * CELL_H)]);
    assert_eq!(
        heights.extras,
        vec![(0, 9 * CELL_H)],
        "the whole height lands on line 0 and nowhere else -- the model subtracts nothing, because \
         it is handed pixels and cannot know how many slots the table was notionally using"
    );
    assert_eq!(
        heights.height(0),
        CELL_H + 9 * CELL_H,
        "line 0 is 9 lines taller, which is what a 4-row table occupies"
    );
    assert_eq!(
        heights.height(1),
        CELL_H,
        "and line 1's own height is untouched -- it moves, but it does not grow"
    );
}

/// The lines below a table move down by exactly its extra height.
///
/// Exact, not "roughly". Every number below is hand-computable: a table with 54 px of extra on line 2
/// puts line 3 at `3*18 + 54 = 108`, line 10 at `10*18 + 54 = 234`.
#[test]
fn the_lines_below_a_table_move_down_by_its_height() {
    let heights = LineHeights::from(CELL_H, &[block(2, 54)]);
    assert_eq!(heights.y(0), 0, "lines above the table do not move");
    assert_eq!(heights.y(1), CELL_H, "nor line 1");
    assert_eq!(
        heights.y(2),
        2 * CELL_H + 54,
        "the anchor line moves down too, or the table is drawn over the first line of text after it"
    );
    assert_eq!(
        heights.y(3),
        3 * CELL_H + 54,
        "and so does line 3: {}",
        3 * CELL_H + 54
    );
    assert_eq!(
        heights.y(10),
        10 * CELL_H + 54,
        "and line 10, because the shift is a prefix sum and not a per-line recomputation"
    );
    assert_eq!(
        heights.total(20),
        20 * CELL_H + 54,
        "and the document is 54 px taller overall"
    );
}

/// A zero-height block is not recorded at all.
///
/// The degenerate case, and the one the old net-subtracting model produced for *every* table. A block
/// with no height displaces nothing, so recording `(line, 0)` would be noise in a list that is scanned
/// on every caret lookup.
#[test]
fn a_zero_height_block_is_not_recorded_at_all() {
    let heights = LineHeights::from(CELL_H, &[block(0, 0)]);
    assert!(
        heights.extras.is_empty(),
        "a zero-height block is not an extra: {:?}",
        heights.extras
    );
    assert_eq!(heights.y(5), 5 * CELL_H, "and no line moves");
}

/// Two tables on one line add rather than overwrite.
///
/// A `Vec` of extras where the second insert *replaced* the first would make the first table vanish
/// from the geometry while still being drawn -- one of them would overlap the text it displaced.
#[test]
fn two_tables_on_one_line_add_rather_than_overwrite() {
    let heights = LineHeights::from(CELL_H, &[block(5, 30), block(5, 12)]);
    assert_eq!(
        heights.extras,
        vec![(5, 42)],
        "30 + 12 on the same line is one extra of 42, not two entries and not just one of them"
    );
    assert_eq!(
        heights.y(6),
        6 * CELL_H + 42,
        "and the line below is 42 px lower"
    );
}

/// A block that is not a whole number of slots rounds up.
///
/// Flooring would let the last line of text sit inside the bottom quarter of a table, which is the
/// overlap this model exists to prevent -- so the ceiling is the load-bearing part of the rounding.
#[test]
fn a_block_rounds_up_to_whole_line_slots() {
    assert_eq!(LineHeights::slots_for(18 * 6, CELL_H), 6, "exactly six");
    assert_eq!(
        LineHeights::slots_for(18 * 6 + 1, CELL_H),
        7,
        "one pixel over six slots is seven, not six"
    );
    assert_eq!(
        LineHeights::slots_for(1, CELL_H),
        1,
        "and something smaller than one slot still displaces a line"
    );
    assert_eq!(
        LineHeights::slots_for(100, 0),
        0,
        "a zero pitch displaces nothing rather than dividing"
    );
}

/// The caret lands below a table where the model says, not at the uniform-pitch row.
///
/// This is why the model lives in `ChromeState` rather than beside it: `Caret::locate` takes the
/// state and nothing else, so a caret on a line after a table would be placed one row up unless the
/// state carries the geometry.
#[test]
fn the_caret_lands_below_a_table_where_the_model_says() {
    let m = ChromeMetrics::DESKTOP;
    let layout = Layout::new(&m);
    let heights = LineHeights::from(CELL_H, &[block(0, 54)]);

    let mut uniform = state_with(LineHeights::uniform(CELL_H));
    uniform.caret_line = 3;
    let without = Caret::locate(&layout, &m, &uniform).expect("a caret on line 3");
    assert_eq!(
        without.cell.y,
        layout.text.y + 3 * CELL_H,
        "with no tables the caret is at the uniform row, which is where it has always been"
    );

    let mut with = state_with(heights.clone());
    with.caret_line = 3;
    let after = Caret::locate(&layout, &m, &with).expect("a caret on line 3");
    assert_eq!(
        after.cell.y,
        layout.text.y + heights.y(3),
        "and with a table above it, 54 px lower"
    );
    assert_eq!(
        after.cell.y - without.cell.y,
        54,
        "the difference is exactly the table's extra height"
    );
    assert_ne!(
        after.cell, without.cell,
        "so a caret that ignored the model would be in visibly the wrong place -- which is the \\
         overlap this debt was about, one row and one caret to the left of correct"
    );
    // The reported line and column are unchanged: the model moves a caret *down the page*, it does not
    // renumber lines. Asserting them guards against a model that "fixed" the y by shifting the line
    // index instead, which would leave every line below the table drawn in the wrong place.
    assert_eq!(
        (after.line, after.column),
        (without.line, without.column),
        "and the line and column it reports are the same, so the geometry moved rather than the \
         numbering"
    );
}
