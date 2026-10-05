//! Phase 9A, part two: tables in a document, and the caret inside one.
//!
//! What these gate, requirement by requirement:
//!
//! | requirement | test |
//! | --- | --- |
//! | Ctrl+T inserts a 3x3 table whose columns fill the measure | [`a_control_t_inserts_a_three_by_three_table`] |
//! | Tab and Shift+Tab are distinguishable | [`tab_and_shift_tab_are_two_commands`] |
//! | Tab walks cells and appends a row at the bottom-right | [`tab_walks_cells_and_appends_a_row_at_the_bottom_right`] |
//! | Shift+Tab walks back and is a no-op at the first cell | [`shift_tab_walks_back_and_stops_at_the_first_cell`] |
//! | spans follow the bytes | [`table_spans_follow_the_bytes_around_them`] |
//! | undo restores the document and the span map together | [`undoing_a_table_insertion_removes_the_table`] |
//! | columns divide the measure | [`column_widths_divide_the_measure_and_refuse_when_they_cannot`] |
//! | the byte count is `cells - 1` | [`an_empty_table_is_exactly_one_separator_fewer_than_it_has_cells`] |

use holonomy_input::{InputEvent, Keymap, ModifierState, KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_T};
use holonomy_text::{
    col_widths_for, down, empty_table_bytes, left, right, shift_tab, tab, up, Nav, ResolvedTable,
    SpanPolicy, TableCursor, TableSpan, CELL_SEPARATOR,
};

/// The measure the page is laid out at: 80 columns.
const MEASURE: u32 = 80;

/// An editor holding `text`.
fn editor(text: &[u8]) -> holonomy_text::Editor {
    holonomy_text::Editor::from_text(text).expect("a document")
}

/// The cells of `span` resolved against `text`, as `(row, col) -> content`.
fn cell_text<'a>(
    t: &ResolvedTable<'a>,
    row: u16,
    col: u16,
) -> Result<&'a [u8], holonomy_text::TableError> {
    let c = t.cell_at(t.span.cell_index(row, col))?;
    Ok(&t.text[c.start_byte as usize..c.end_byte as usize])
}

/// Ctrl+T inserts a 3x3 table whose columns fill the measure.
///
/// The two things worth asserting are that the *shape* is 3x3 with the right number of separators --
/// `ResolvedTable::new` checks that, and it is the check that catches a byte count that is off by one
/// -- and that the columns are as wide as the measure allows. The width is `23` per column and not
/// `23.33`: see `col_widths_for`, which gives the leftover character to the gutter rather than
/// making one column a character wider than its neighbours.
#[test]
fn a_control_t_inserts_a_three_by_three_table() {
    let keymap = Keymap::us();
    let mut mods = ModifierState::new();
    // Ctrl down, then T. `dispatch_into` folds the modifier first, which is the whole reason it exists:
    // dispatching before folding would type a `t`.
    keymap.dispatch_into(InputEvent::press(KEY_LEFTCTRL), &mut mods);
    let command = keymap
        .dispatch_into(InputEvent::press(KEY_T), &mut mods)
        .expect("Ctrl+T is a command");
    assert_eq!(
        command,
        holonomy_input::Command::Hotkey(holonomy_input::Hotkey::InsertTable),
        "Ctrl+T is the hotkey, and it is resolved before the character table so it is not a `t`"
    );
    // And with no modifier it is an ordinary `t`, which is the other half of the claim: a keymap that
    // swallowed `t` would be worse than one missing Ctrl+T.
    let mut plain = ModifierState::new();
    assert_eq!(
        keymap.dispatch_into(InputEvent::press(KEY_T), &mut plain),
        Some(holonomy_input::Command::Insert('t')),
        "T on its own is still a letter"
    );

    let mut ed = editor(b"");
    let span = ed.insert_table(3, 3, MEASURE).expect("insert the table");
    assert_eq!((span.rows, span.cols), (3, 3), "a 3x3 table");
    assert_eq!(span.col_widths[..3], [23, 23, 23], "three equal columns");
    assert_eq!(
        span.width_cells(),
        MEASURE - 1,
        "3*23 content + 3*2 pad + 4 borders = 79 of the 80 available, the leftover to the gutter"
    );

    let text = ed.text().expect("the document");
    let t = ResolvedTable::new(span, &text).expect("the span resolves against its own bytes");
    assert_eq!(
        t.span.cell_count(),
        9,
        "nine cells, which needs eight separators and not nine"
    );
    for row in 0..3 {
        for col in 0..3 {
            assert!(
                cell_text(&t, row, col).expect("the cell").is_empty(),
                "cell ({row},{col}) starts empty, so the caret has somewhere to type"
            );
        }
    }
}

/// Tab and Shift+Tab are two commands, from the same physical key.
///
/// This is the whole of requirement B. `text_for(KEY_TAB, mods)` is `Some('\t')` with shift down and
/// without, because tab is tab on every layout, so the character table cannot tell them apart and any
/// implementation that reads the character is reading the same answer twice.
#[test]
fn tab_and_shift_tab_are_two_commands() {
    let keymap = Keymap::us();
    let tab = holonomy_input::KEY_TAB;
    let mut mods = ModifierState::new();

    keymap.dispatch_into(InputEvent::press(KEY_LEFTSHIFT), &mut mods);
    let shifted = keymap
        .dispatch_into(InputEvent::press(tab), &mut mods)
        .expect("Shift+Tab");
    keymap.dispatch_into(InputEvent::release(tab), &mut mods);
    keymap.dispatch_into(InputEvent::release(KEY_LEFTSHIFT), &mut mods);
    let plain = keymap
        .dispatch_into(InputEvent::press(tab), &mut mods)
        .expect("Tab");

    assert_eq!(
        (shifted, plain),
        (
            holonomy_input::Command::ShiftTab,
            holonomy_input::Command::Tab
        ),
        "shift decides, and it is read from the modifier state rather than from the character"
    );

    // And Shift+Tab is not a `t`-with-shift or any other character, which is what would happen if the
    // shift flag were consulted after the character table.
    let mut mods = ModifierState::new();
    keymap.dispatch_into(InputEvent::press(KEY_LEFTSHIFT), &mut mods);
    assert_ne!(
        keymap.dispatch_into(InputEvent::press(tab), &mut mods),
        Some(holonomy_input::Command::Insert('\t')),
        "Shift+Tab does not type a tab character into the document"
    );
}

/// Tab walks every cell in order and appends a row when it leaves the last one.
///
/// A 2x2 table has four cells, so Tab is called five times: four moves and one append. The append is
/// the claim worth having -- it is a *new row of separators*, an edit, and a navigation rule that
/// performed it would have to be undoable by machinery it does not have.
#[test]
fn tab_walks_cells_and_appends_a_row_at_the_bottom_right() {
    let mut ed = editor(b"");
    let span = ed.insert_table(2, 2, MEASURE).expect("a 2x2 table");
    let text = ed.text().expect("the document");
    let mut t = ResolvedTable::new(span, &text).expect("resolvable");

    let mut cur = TableCursor {
        row: 0,
        col: 0,
        offset_in_cell: 0,
    };
    let mut seen = vec![(cur.row, cur.col)];
    for _ in 0..3 {
        match tab(&t, cur).expect("Tab resolves") {
            Nav::Move {
                row,
                col,
                offset_in_cell,
                append_row,
            } => {
                assert!(
                    !append_row,
                    "the append only happens on the fifth Tab, not this one"
                );
                cur = TableCursor {
                    row,
                    col,
                    offset_in_cell,
                };
                seen.push((cur.row, cur.col));
            }
            other => panic!("expected a move, got {other:?}"),
        }
    }
    assert_eq!(
        seen,
        vec![(0, 0), (0, 1), (1, 0), (1, 1)],
        "row-major, wrapping at the end of each row"
    );

    // The fourth Tab leaves the table.
    let Nav::Move {
        row,
        col,
        offset_in_cell,
        append_row,
    } = tab(&t, cur).expect("Tab resolves")
    else {
        panic!("the bottom-right Tab appends a row");
    };
    assert!(append_row, "leaving the last cell wants a row appended");
    assert_eq!(
        (row, col, offset_in_cell),
        (2, 0, 0),
        "and lands in the new row's first cell"
    );

    let new_row = ed.append_table_row(span).expect("append the row");
    assert_eq!(
        new_row, 2,
        "the new row is index 2, the one after the old last"
    );
    let text = ed.text().expect("the document");
    let grown = ed
        .table_at(span.start_byte)
        .expect("the table is still there");
    assert_eq!(grown.rows, 3, "and the table knows it");
    assert_eq!(
        grown.end_byte - grown.start_byte,
        5,
        "a 2x3 table has six cells and therefore five separators"
    );
    t = ResolvedTable::new(grown, &text).expect("and it resolves");
    assert_eq!(t.span.cell_count(), 6, "six cells, which is the point");
}

/// Shift+Tab walks back, wraps at the start of a row, and does nothing at the very first cell.
///
/// Three claims, and the third is the one that distinguishes this from a spreadsheet: backwards from
/// `(0, 0)` is nowhere, not a wrap to the bottom-right cell.
#[test]
fn shift_tab_walks_back_and_stops_at_the_first_cell() {
    let mut ed = editor(b"");
    let span = ed.insert_table(2, 3, MEASURE).expect("a 2x3 table");
    let text = ed.text().expect("the document");
    let t = ResolvedTable::new(span, &text).expect("resolvable");

    let at = |row, col| TableCursor {
        row,
        col,
        offset_in_cell: 0,
    };
    let back = |cur| match shift_tab(&t, cur).expect("Shift+Tab resolves") {
        Nav::Move { row, col, .. } => (row, col),
        other => panic!("expected a move, got {other:?}"),
    };

    assert_eq!(back(at(0, 2)), (0, 1), "one left");
    assert_eq!(back(at(0, 1)), (0, 0), "two left");
    assert_eq!(
        back(at(1, 0)),
        (0, 2),
        "wrapping backwards off the start of a row lands at the end of the row above"
    );
    assert_eq!(
        shift_tab(&t, at(0, 0)).expect("resolves"),
        Nav::Nowhere,
        "and backwards from the first cell is nowhere, not a wrap to the last"
    );
}

/// A table's span follows the bytes around it: insertions before, inside, and at its first byte.
///
/// Three cases, and the boundary between the first two is the one worth having a test for: an insert
/// at *exactly* `start_byte` is **inside** the table, because typing in cell `(0, 0)` puts the caret
/// on the table's first byte. The first version of this code read it as "before", on the reasoning
/// that it was not yet part of the table -- which meant the character a person typed into the first
/// cell landed outside the table and every other cell shifted along by one.
#[test]
fn table_spans_follow_the_bytes_around_them() {
    let mut ed = editor(b"one\ntwo\n");
    // Caret at 4, which is the start of the second line, so the table begins with a newline of its
    // own and there are bytes in front of it to slide across.
    ed.caret_to(4).expect("caret into the second line");
    let span = ed.insert_table(2, 2, MEASURE).expect("a 2x2 table");
    assert_eq!(
        span.start_byte,
        4,
        "caret at 4, which is already the start of a line, so no newline was inserted before it -- \
         and there are still three bytes in front of the table for the first case below"
    );

    // Before the table: both ends move.
    let before = ed.table_at(span.start_byte).expect("still there");
    ed.insert_at(0, b"XY", SpanPolicy::GrowIntoInsert)
        .expect("insert before");
    let after = ed.table_at(before.start_byte + 2).expect("still there");
    assert_eq!(
        (after.start_byte, after.end_byte),
        (before.start_byte + 2, before.end_byte + 2),
        "an insert before the table slides it right, and it is still findable"
    );

    // At the table's very first byte: inside, so the table grows and the start stays put.
    let b2 = ed.table_at(after.start_byte).expect("still there");
    ed.insert_at(b2.start_byte, b"Z", SpanPolicy::GrowIntoInsert)
        .expect("insert at the table's first byte");
    let a2 = ed.table_at(b2.start_byte).expect("still there");
    assert_eq!(
        a2.start_byte, b2.start_byte,
        "an insert at the table's first byte is inside it, so the start does not move"
    );
    assert_eq!(
        a2.end_byte,
        b2.end_byte + 1,
        "and the table grew by the one byte"
    );
    // And the byte really is in the first cell, which is the claim the whole boundary is about.
    let text = ed.text().expect("the document");
    let t = ResolvedTable::new(a2, &text).expect("resolvable");
    assert_eq!(
        cell_text(&t, 0, 0).expect("cell"),
        b"Z",
        "the character typed at a cell's start is in that cell"
    );

    // Strictly inside, past the first byte: only the end moves.
    let b3 = ed.table_at(a2.start_byte).expect("still there");
    ed.insert_at(b3.start_byte + 1, b"Q", SpanPolicy::GrowIntoInsert)
        .expect("insert inside");
    let a3 = ed.table_at(b3.start_byte).expect("still there");
    assert_eq!(
        (a3.start_byte, a3.end_byte),
        (b3.start_byte, b3.end_byte + 1),
        "an insert inside the table extends it and leaves its start alone"
    );

    // After the table: nothing about it changes at all.
    let b4 = ed.table_at(a3.start_byte).expect("still there");
    let end = b4.end_byte;
    ed.insert_at(end, b"tail", SpanPolicy::GrowIntoInsert)
        .expect("insert after");
    let a4 = ed.table_at(b4.start_byte).expect("still there");
    assert_eq!(
        (a4.start_byte, a4.end_byte),
        (b4.start_byte, b4.end_byte),
        "an insert past the table's end leaves it exactly where it was"
    );
}

/// Undoing a table insertion removes the table, not just its bytes.
///
/// The bytes and the span map are updated in different places, so this is the test that they agree.
/// Before this, undo removed the separators and left the span pointing at whatever had moved into
/// their place -- so the next keystroke found a table where there was none and a cell that was not
/// there.
#[test]
fn undoing_a_table_insertion_removes_the_table() {
    let mut ed = editor(b"");
    let span = ed.insert_table(2, 2, MEASURE).expect("a 2x2 table");
    assert_eq!(ed.tables().len(), 1, "one table");
    ed.undo().expect("undo");
    assert_eq!(
        ed.tables().len(),
        0,
        "the span went with the bytes; leaving it would point a table at unrelated text"
    );
    assert_eq!(
        ed.text().expect("the document").len(),
        0,
        "and the bytes are gone, the trailing newline with them: they were one insertion, so undo \
         removes all of it rather than leaving the document's last line behind"
    );

    // And redo brings both back, because redo goes through `apply_insert_raw` and not `insert_at` --
    // two different code paths, and only the second one was reached by the first version of this.
    ed.redo().expect("redo");
    assert_eq!(ed.tables().len(), 1, "the table is back");
    let text = ed.text().expect("the document");
    let restored = ed.tables().spans()[0];
    assert_eq!(
        (restored.rows, restored.cols),
        (2, 2),
        "with the shape it had, not a default one"
    );
    ResolvedTable::new(restored, &text).expect("and it resolves against the restored bytes");
}

/// Column widths divide the measure, and refuse when they cannot.
///
/// The refusal matters as much as the division: a measure too narrow for `cols` columns plus their
/// borders must be refused rather than producing a zero-width column, because `TableSpan::with_widths`
/// rejects those and the error would then arrive from somewhere unrelated.
#[test]
fn column_widths_divide_the_measure_and_refuse_when_they_cannot() {
    assert_eq!(
        col_widths_for(80, 3).expect("3 columns fit in 80"),
        [23, 23, 23, 1, 1, 1, 1, 1],
        "the leftover character goes to the gutter, not to one column"
    );
    assert_eq!(
        col_widths_for(80, 1).expect("1 column")[0],
        76,
        "80 - 2 pad - 2 borders = 76, the whole width"
    );
    assert_eq!(
        col_widths_for(20, 2).expect("2 columns fit in 20"),
        [6, 6, 1, 1, 1, 1, 1, 1],
        "20 - 4 pad - 3 borders = 13, which is 6 and 6 with one left over"
    );
    assert_eq!(
        TableSpan::with_widths(1, 2, col_widths_for(20, 2).expect("widths"), 0, 0)
            .expect("a 2x1 table")
            .width_cells(),
        19,
        "and so the table is 19 of the 20 columns, never 20: a table that exactly filled the measure \
         would have its last border on the page's edge, where the shadow is"
    );
    // 8 columns need 8*2 + 9 = 25 characters of chrome before any content.
    assert_eq!(
        col_widths_for(24, 8),
        None,
        "24 is one short of the borders alone"
    );
    assert!(
        col_widths_for(34, 8).is_some(),
        "34 leaves 9 for 8 columns, which is one each"
    );
    assert_eq!(col_widths_for(80, 0), None, "no columns is not a table");
    assert_eq!(
        col_widths_for(80, 9),
        None,
        "nine columns is over TableSpan::MAX_COLS, which is 8"
    );
}

/// An empty table is exactly one separator fewer than it has cells.
///
/// `cells - 1`, not `cells`, and this is the only test that says so at the level of bytes rather than
/// through `ResolvedTable::new`'s count.
#[test]
fn an_empty_table_is_exactly_one_separator_fewer_than_it_has_cells() {
    for (rows, cols) in [(1u16, 1u16), (1, 3), (3, 3), (4, 2)] {
        let bytes = empty_table_bytes(rows, cols).expect("an empty table");
        let shape = TableSpan::new(rows, cols, 8, 0, bytes.len() as u32).expect("the shape");
        assert_eq!(
            bytes.len() as u32,
            shape.cell_count() - 1,
            "{rows}x{cols} is {} cells and {} separators",
            shape.cell_count(),
            bytes.len()
        );
        assert!(
            bytes.iter().all(|&b| b == CELL_SEPARATOR),
            "and every one of them is U+001F"
        );
    }
    assert_eq!(
        empty_table_bytes(1, 1).expect("1x1").len(),
        0,
        "a single cell has no separators"
    );
}

/// Arrows cross cell boundaries, and clamp to a ragged cell rather than escaping it.
///
/// The clamp is the substantive claim. A caret five bytes into a nine-byte cell, moved up into a
/// three-byte cell, must land at that cell's *end* -- not five bytes past it, which is inside the
/// neighbouring cell, so the next keystroke would edit the wrong one.
#[test]
fn arrows_cross_cells_and_clamp_to_a_ragged_cell() {
    let mut ed = editor(b"");
    let span = ed.insert_table(2, 2, MEASURE).expect("a 2x2 table");
    // Fill the top-left cell with five bytes and the bottom-left with one, by typing at their starts.
    let text = ed.text().expect("the document");
    let t0 = ResolvedTable::new(span, &text).expect("resolvable");
    let tl = t0.cell_at(0).expect("cell").start_byte;
    let bl = t0.cell_at(2).expect("cell").start_byte;
    ed.insert_at(tl, b"abcde", SpanPolicy::GrowIntoInsert)
        .expect("top-left");
    let bl = bl + 5;
    ed.insert_at(bl, b"z", SpanPolicy::GrowIntoInsert)
        .expect("bottom-left");

    let text = ed.text().expect("the document");
    let grown = ed.tables().spans()[0];
    let t = ResolvedTable::new(grown, &text).expect("resolvable");
    assert_eq!(
        cell_text(&t, 0, 0).expect("cell"),
        b"abcde",
        "the top-left cell"
    );
    assert_eq!(
        cell_text(&t, 1, 0).expect("cell"),
        b"z",
        "the bottom-left cell"
    );

    let cur = |row, col, offset| TableCursor {
        row,
        col,
        offset_in_cell: offset,
    };
    let land = |nav: Nav| match nav {
        Nav::Move {
            row,
            col,
            offset_in_cell,
            ..
        } => (row, col, offset_in_cell),
        other => panic!("expected a move, got {other:?}"),
    };

    assert_eq!(
        land(down(&t, cur(0, 0, 5)).expect("down")),
        (1, 0, 1),
        "five bytes down into a one-byte cell clamps to that cell's end"
    );
    assert_eq!(
        land(right(&t, cur(0, 0, 5)).expect("right")),
        (0, 1, 0),
        "right at a cell's end enters the next cell at its start"
    );
    assert_eq!(
        land(left(&t, cur(0, 1, 0)).expect("left")),
        (0, 0, 5),
        "and left at a cell's start enters the previous cell at its end"
    );
    assert_eq!(
        up(&t, cur(0, 0, 0)).expect("up"),
        Nav::Nowhere,
        "up from the first row is nowhere"
    );
    assert_eq!(
        down(&t, cur(1, 0, 0)).expect("down"),
        Nav::Nowhere,
        "and down from the last row is nowhere"
    );
}
