//! Phase 9A part two: where tables live in a document, and where the caret is inside one.
//!
//! [`crate::table`] is the *shape*: a [`TableSpan`] over a byte interval, resolved against a buffer to
//! give cells. This file is the two things a shape cannot be on its own:
//!
//! * [`TableMap`] — the spans themselves, and the arithmetic that keeps them correct as bytes move
//!   around them. A span is an interval, so an insert inside one extends it and an insert before one
//!   slides it; getting that wrong does not crash, it silently misattributes every cell after the
//!   edit. That is why it lives beside the edit, not in a caller that remembers to call it.
//! * [`TableCursor`] — the navigation state machine. Tab, Shift+Tab, Enter and the four arrows, in
//!   terms of `(row, col)` and an offset *within the cell*, with no knowledge of bytes or of the
//!   document.
//!
//! # Why the cursor is a separate type
//!
//! Every rule here is about *positions*, and a position can be checked without a buffer. "Tab at the
//! bottom-right cell appends a row" is arithmetic. "Tab at the bottom-right cell appends a row" *in a
//! document whose byte offsets have all moved* is arithmetic plus bookkeeping plus the possibility
//! that the bookkeeping is wrong. Splitting them means the arithmetic is tested against
//! hand-written `(row, col, offset)` triples and the bookkeeping is tested against exactly one thing:
//! that it puts the caret where the arithmetic said.

use crate::table::{ResolvedTable, TableError, TableSpan, CELL_SEPARATOR};

/// The spans of every table in a document, kept in step with the bytes.
///
/// A `Vec`, not an interval tree: a document in scope has a handful of tables, and a tree would be a
/// second index structure to keep correct for a count that `iter().position()` answers. The invariant
/// it must keep is that spans are **disjoint and in ascending order**, and it is checked rather than
/// assumed -- see [`TableMap::check_order`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableMap {
    spans: Vec<TableSpan>,
}

/// Why a [`TableMap`] operation was refused.
///
/// Not `Copy`, unlike every other error in this crate, because it carries an [`EditorError`] and that
/// is not `Copy`. Cloning an error to propagate it is not the pattern the rest of the crate
/// establishes, and a `TableMapError` is raised once per table insertion rather than per keystroke, so
/// the clone is not on any measured path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableMapError {
    /// A new span would overlap an existing one, or the spans are not in ascending order.
    ///
    /// Two tables sharing a byte would make "which cell contains this offset" have two answers, and
    /// the editor would pick one silently.
    Overlap {
        /// The start of the span that was refused.
        start: u32,
        /// One past the end of the span before it, which is where a new one must start at the earliest.
        after: u32,
    },
    /// A table of this shape does not fit the measure, or is otherwise malformed.
    Table(TableError),
    /// The document rejected the edit.
    Editor(crate::EditorError),
}

impl From<TableError> for TableMapError {
    fn from(e: TableError) -> Self {
        Self::Table(e)
    }
}

impl From<crate::EditorError> for TableMapError {
    fn from(e: crate::EditorError) -> Self {
        Self::Editor(e)
    }
}

impl TableMap {
    /// An empty map.
    pub const fn new() -> Self {
        Self { spans: Vec::new() }
    }

    /// How many tables.
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Every span, ascending and disjoint.
    pub fn spans(&self) -> &[TableSpan] {
        &self.spans
    }

    /// Add `span`, keeping the map sorted.
    ///
    /// Refuses an overlap rather than merging or dropping, because a caller that computed a span
    /// which intersects an existing one has a bug, and silently repairing it would hide that bug
    /// behind a table that renders slightly wrong.
    pub fn insert(&mut self, span: TableSpan) -> Result<usize, TableMapError> {
        if let Some(next) = self
            .spans
            .iter()
            .find(|s| span.start_byte < s.end_byte && s.start_byte < span.end_byte)
        {
            return Err(TableMapError::Overlap {
                start: next.start_byte,
                after: next.end_byte,
            });
        }
        let at = self
            .spans
            .iter()
            .position(|s| s.start_byte > span.start_byte)
            .unwrap_or(self.spans.len());
        self.spans.insert(at, span);
        Ok(at)
    }

    /// Remove the table containing `offset`, if any.
    pub fn remove_at(&mut self, offset: u32) -> Option<TableSpan> {
        let at = self
            .spans
            .iter()
            .position(|s| offset >= s.start_byte && offset < s.end_byte)?;
        Some(self.spans.remove(at))
    }

    /// The table containing `offset`.
    ///
    /// # The end is inclusive, and that is not a convenience
    ///
    /// A table of `rows` by `cols` has `rows * cols - 1` separators, so the bytes of the **last** cell
    /// are whatever follows the final separator -- and for a cell nobody has typed into, that is
    /// nothing at all, which means the last cell's `start_byte` is the table's `end_byte`. A caret in
    /// an empty last cell therefore sits *at* `end_byte`, and a half-open range cannot find the table
    /// there.
    ///
    /// The first version used `offset < end_byte` and it failed in the most ordinary way possible: Tab
    /// to the bottom-right cell of a fresh table, and the next keystroke reported
    /// `NotInTable { offset: 12 }` -- 12 being exactly the byte the caret was on. Typing there is also
    /// correct rather than merely tolerable: the byte at `end_byte` is the newline that terminates the
    /// table, and inserting before it puts the character in the last cell, where the person meant it.
    ///
    /// [`Cell::contains`] carries the same asymmetry with a `last_cell` flag, for the same reason.
    pub fn at(&self, offset: u32) -> Option<TableSpan> {
        self.spans
            .iter()
            .copied()
            .find(|s| offset >= s.start_byte && offset <= s.end_byte)
    }

    /// Shift every span for `len` bytes inserted at `offset`.
    ///
    /// Three cases, the ordinary half-open-interval rule:
    ///
    /// * `offset < start`: the table moves right.
    /// * `start <= offset < end`: the insert landed inside the table, so the table grows.
    /// * `offset >= end`: nothing about the table changes.
    ///
    /// # Why the boundary is `start`, not `start + 1`
    ///
    /// The first version wrote `offset <= start_byte` for the first case, on the reasoning that an
    /// insert *at* a table's first byte is "before" the table and should not count as inside it. That
    /// reasoning is wrong, and it is wrong in the direction that breaks the most common operation in
    /// the whole feature: **typing in cell `(0, 0)` inserts at exactly `start_byte`**, because that
    /// cell's first byte is the table's first byte. So the table slid right instead of growing, the
    /// character a person had just typed landed *outside* the table, and cell `(0, 0)` stayed empty
    /// while every other cell shifted along by one. The test that caught it is
    /// `arrows_cross_cells_and_clamp_to_a_ragged_cell`, which fills the first cell and then reads it
    /// back.
    ///
    /// The general rule needs no special case because it is the same rule a byte range obeys: an
    /// insert at the first byte of a range is inside it.
    pub fn apply_insert(&mut self, offset: u32, len: u32) {
        for span in &mut self.spans {
            if offset < span.start_byte {
                span.start_byte += len;
                span.end_byte += len;
            } else if offset < span.end_byte {
                span.end_byte += len;
            }
        }
    }

    /// Shift every span for `len` bytes deleted starting at `offset`.
    ///
    /// Saturating at both ends, for the same reason [`ResolvedTable::span_after_delete`] is: a delete
    /// that runs off the end of a table must shrink it to empty, not wrap `end_byte` below
    /// `start_byte`.
    pub fn apply_delete(&mut self, offset: u32, len: u32) {
        for span in &mut self.spans {
            let start = if offset <= span.start_byte {
                span.start_byte.saturating_sub(len)
            } else {
                span.start_byte
            };
            let end = if offset < span.end_byte {
                span.end_byte.saturating_sub(len).max(start)
            } else {
                span.end_byte
            };
            span.start_byte = start;
            span.end_byte = end.max(start);
        }
    }

    /// Drop tables that can no longer hold the separators their shape claims.
    ///
    /// # Why a length check and not a scan
    ///
    /// The obvious implementation reads the document and counts `CELL_SEPARATOR`s in each span, which
    /// is what [`ResolvedTable::new`] does. That was the first version, and it takes a
    /// `&self.rope.to_vec()` at each of two call sites -- **a full copy of the document, on every
    /// backspace**, which is exactly the allocation the "no heap allocation while editing" invariant
    /// forbids and which a keystroke-latency gate would have caught as a rate rather than a count.
    ///
    /// So this is arithmetic on the span alone, and it is sufficient for the failure it exists to
    /// prevent: a table with `rows * cols` cells needs `rows * cols - 1` separators, so a span whose
    /// byte range is shorter than that **cannot** contain them. Deleting the separators shrinks the
    /// range past the bound, the span is dropped, and `cell_count()` -- which subtracts one from
    /// `rows * cols` and would underflow `u32` -- is never called on it.
    ///
    /// What this does *not* catch is a span that is long enough but whose separators are wrong, e.g.
    /// one separator deleted with no content deleted. That is a genuine inconsistency and
    /// [`ResolvedTable::new`] reports it as [`TableError::StructureMismatch`] at the point of use,
    /// which is the right place: it is a real error rather than a reason to silently drop a table the
    /// user can see.
    pub fn retain_intact(&mut self) {
        self.spans.retain(|s| {
            s.end_byte > s.start_byte && s.end_byte - s.start_byte >= s.separator_count()
        });
    }

    /// The table at `offset`, resolved against `text`.
    pub fn resolve_at<'a>(&self, offset: u32, text: &'a [u8]) -> Option<ResolvedTable<'a>> {
        self.at(offset)
            .and_then(|span| ResolvedTable::new(span, text).ok())
    }

    /// That the spans are ascending and disjoint, which [`TableMap::insert`] maintains.
    ///
    /// A gate rather than an assertion inside `insert`: the map is not `unsafe` and a violation is
    /// not memory-unsafe, it is a wrong answer, and a wrong answer should be *reported* by the test
    /// that caused it rather than panic inside the editor.
    pub fn check_order(&self) -> Result<(), TableMapError> {
        let mut previous_end = 0u32;
        for (i, span) in self.spans.iter().enumerate() {
            if i > 0 && span.start_byte < previous_end {
                return Err(TableMapError::Overlap {
                    start: span.start_byte,
                    after: previous_end,
                });
            }
            if span.end_byte < span.start_byte {
                return Err(TableMapError::Overlap {
                    start: span.start_byte,
                    after: span.start_byte,
                });
            }
            previous_end = span.end_byte;
        }
        Ok(())
    }

    /// Replace the whole map. For undo and redo, which move many spans at once.
    pub fn replace(&mut self, spans: Vec<TableSpan>) {
        self.spans = spans;
    }

    /// Take the spans out, leaving the map empty.
    pub fn take(&mut self) -> Vec<TableSpan> {
        core::mem::take(&mut self.spans)
    }
}

/// A position inside a table: which cell, and how far into it.
///
/// Plain integers, and `Copy`, so the state machine's whole state is eight bytes and a caller can
/// hold a "where was the caret before the table" without an allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableCursor {
    /// Row, 0-based.
    pub row: u16,
    /// Column, 0-based.
    pub col: u16,
    /// Bytes into the cell's content. Not characters: the content is a byte range and every offset in
    /// this file is a byte offset, so converting here would mean a UTF-8 walk per keystroke for a
    /// number the editor stores as bytes anyway.
    pub offset_in_cell: u32,
}

/// What a navigation asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    /// Move within the table. `append_row` is set when Tab left the last cell, because appending
    /// changes the table's shape and the caller has to do that as an *edit*, not as a caret move.
    Move {
        /// The cell to be in afterwards.
        row: u16,
        /// The column to be in afterwards.
        col: u16,
        /// Where in that cell's content the caret goes.
        offset_in_cell: u32,
        /// Whether this navigation left the last cell and so wants a row appended first.
        append_row: bool,
    },
    /// The navigation cannot happen: no cell above, or already in the first cell.
    Nowhere,
    /// Insert an in-cell newline at the current position.
    NewlineInCell,
}

/// The width in character cells of each of `cols` columns at `measure`.
///
/// # The arithmetic
///
/// [`TableSpan::width_cells`] is `sum(widths) + cols * 2 * CELL_PAD + (cols + 1) * BORDER_W`, so the
/// content budget is `measure - cols * 2 * PAD - (cols + 1) * BORDER` and each column gets an equal
/// share of what is left over. The remainder is *not* distributed: a table whose columns differ by
/// one character is a table whose borders do not land on the page's cell grid in a way anyone can
/// predict, and the leftover character goes to the gutter instead.
///
/// Returns `None` when even one character per column plus the borders does not fit, which for a
/// 3-column table means a measure under 13 and for an 8-column table under 34.
pub const fn col_widths_for(measure: u32, cols: u16) -> Option<[u16; TableSpan::MAX_COLS]> {
    // `cols as usize > MAX_COLS` rather than `usize::from(cols) > MAX_COLS`: `From<u16> for usize` is
    // not a `const fn` on this toolchain (rust-lang/rust#143874), so a `const` function cannot call it.
    // `cols` is a `u16` and `MAX_COLS` is 8, so the widening cannot lose anything.
    if cols == 0 || cols as usize > TableSpan::MAX_COLS {
        return None;
    }
    let c = cols as u32;
    let chrome = c * 2 * TableSpan::CELL_PAD + (c + 1) * TableSpan::BORDER_W;
    let content = match measure.checked_sub(chrome) {
        Some(v) => v,
        None => return None,
    };
    let each = content / c;
    if each == 0 {
        return None;
    }
    let mut widths = [1u16; TableSpan::MAX_COLS];
    let mut i = 0usize;
    while i < cols as usize {
        widths[i] = each as u16;
        i += 1;
    }
    Some(widths)
}

/// The bytes an empty `rows` by `cols` table occupies: `rows * cols - 1` separators.
///
/// `cells - 1` and not `cells`, because a separator goes *between* two cells and there is no cell
/// after the last one. This is [`TableSpan::separator_count`] expressed as bytes, and the two must
/// agree or [`ResolvedTable::new`] refuses the table.
pub fn empty_table_bytes(rows: u16, cols: u16) -> Option<Vec<u8>> {
    let span = TableSpan::new(rows, cols, 1, 0, 0).ok()?;
    let mut bytes = vec![CELL_SEPARATOR; span.separator_count() as usize];
    bytes.truncate(bytes.len());
    Some(bytes)
}

/// One more row's worth of separators, for appending to the bottom-right cell.
///
/// **`cols`, not `cols - 1`.** A table of `rows` by `cols` has `rows * cols - 1` separators, so going
/// from `rows` to `rows + 1` adds exactly `cols`. The first version subtracted one, reasoning that the
/// boundary between the old last cell and the new first cell "already has" a separator -- and it does,
/// but it is the *last* existing separator, sitting at `end_byte - 1`, and the new row still needs its
/// own `cols` boundaries: `(rows-1, cols-1) -> (rows, 0)` plus the `cols - 1` inside the new row. So a
/// 2x2 table grew to 3x2 with four separators where five were needed, `ResolvedTable::new` counted
/// four and expected five, and every cell after the append was misreported. The invariant that settles
/// it is `separator_count() == rows * cols - 1`, and `cell_count() - separator_count()` for the same
/// shape is what makes the arithmetic unarguable.
pub fn appended_row_bytes(cols: u16) -> Vec<u8> {
    if cols == 0 {
        return Vec::new();
    }
    vec![CELL_SEPARATOR; cols as usize]
}

/// The navigation rules, as pure functions of a resolved table and a position.
///
/// Every one of these *decides* and returns; none of them moves the caret or touches a byte. That is
/// the whole reason they are separate from the session: appending a row is an **edit**, with new
/// bytes and an undo action, and the only thing that can make one is the editor. So Tab's answer is
/// "this navigation wants a row appended, then land at `(rows, 0)`", and the session does the edit
/// and the caret move as two steps it can check separately.
///
/// Offsets within a cell are clamped to that cell's length rather than refused. A caret five
/// characters into a row of nine lands at the *end* of a three-character cell above rather than five
/// characters past its end -- which would be inside the next cell, so the next keystroke would edit
/// the wrong cell and the one after that would not.
pub fn tab(t: &ResolvedTable<'_>, cur: TableCursor) -> Result<Nav, TableError> {
    let span = t.span;
    let last = span.cell_count() - 1;
    if span.cell_index(cur.row, cur.col) == last {
        // The bottom-right cell. `rows` is the row *after* the last, which is the row an append
        // creates; the session appends before it acts on this.
        return Ok(Nav::Move {
            row: span.rows,
            col: 0,
            offset_in_cell: 0,
            append_row: true,
        });
    }
    let next = span.cell_index(cur.row, cur.col) + 1;
    land(t, next)
}

/// Shift+Tab: the previous cell, wrapping backwards at the start of a row.
///
/// At `(0, 0)` this is [`Nav::Nowhere`] rather than a wrap to the last cell. Backwards from the
/// document's first cell is nowhere; wrapping would put the caret in the bottom-right cell of the same
/// table, which is what a spreadsheet does and is not what a document does.
pub fn shift_tab(t: &ResolvedTable<'_>, cur: TableCursor) -> Result<Nav, TableError> {
    if cur.row == 0 && cur.col == 0 {
        return Ok(Nav::Nowhere);
    }
    let span = t.span;
    land(t, span.prev_cell(span.cell_index(cur.row, cur.col)))
}

/// Left: back one byte, or to the end of the previous cell when at this cell's start.
pub fn left(t: &ResolvedTable<'_>, cur: TableCursor) -> Result<Nav, TableError> {
    let span = t.span;
    if cur.offset_in_cell > 0 {
        return Ok(Nav::Move {
            row: cur.row,
            col: cur.col,
            offset_in_cell: cur.offset_in_cell - 1,
            append_row: false,
        });
    }
    let index = span.cell_index(cur.row, cur.col);
    if index == 0 {
        return Ok(Nav::Nowhere);
    }
    // The *end* of the previous cell, so the two are adjacent: the byte before this cell's first byte
    // is the previous cell's last byte.
    let prev = t.cell_at(span.prev_cell(index))?;
    Ok(Nav::Move {
        row: prev.row,
        col: prev.col,
        offset_in_cell: prev.len(),
        append_row: false,
    })
}

/// Right: forward one byte, or to the start of the next cell when at this cell's end.
pub fn right(t: &ResolvedTable<'_>, cur: TableCursor) -> Result<Nav, TableError> {
    let span = t.span;
    let here = t.cell_at(span.cell_index(cur.row, cur.col))?;
    if cur.offset_in_cell < here.len() {
        return Ok(Nav::Move {
            row: cur.row,
            col: cur.col,
            offset_in_cell: cur.offset_in_cell + 1,
            append_row: false,
        });
    }
    let index = span.cell_index(cur.row, cur.col);
    if index + 1 >= span.cell_count() {
        return Ok(Nav::Nowhere);
    }
    land(t, index + 1)
}

/// Up: the cell above, at the same offset within it.
pub fn up(t: &ResolvedTable<'_>, cur: TableCursor) -> Result<Nav, TableError> {
    if cur.row == 0 {
        return Ok(Nav::Nowhere);
    }
    land_keeping_offset(t, cur.row - 1, cur.col, cur.offset_in_cell)
}

/// Down: the cell below, at the same offset within it.
pub fn down(t: &ResolvedTable<'_>, cur: TableCursor) -> Result<Nav, TableError> {
    let span = t.span;
    if cur.row + 1 >= span.rows {
        return Ok(Nav::Nowhere);
    }
    land_keeping_offset(t, cur.row + 1, cur.col, cur.offset_in_cell)
}

/// Land at the start of a flat cell index.
fn land(t: &ResolvedTable<'_>, index: u32) -> Result<Nav, TableError> {
    let cell = t.cell_at(index)?;
    Ok(Nav::Move {
        row: cell.row,
        col: cell.col,
        offset_in_cell: 0,
        append_row: false,
    })
}

/// Land at `(row, col)`, keeping `offset` as far as that cell allows.
fn land_keeping_offset(
    t: &ResolvedTable<'_>,
    row: u16,
    col: u16,
    offset: u32,
) -> Result<Nav, TableError> {
    let cell = t.cell_at(t.span.cell_index(row, col))?;
    Ok(Nav::Move {
        row: cell.row,
        col: cell.col,
        offset_in_cell: offset.min(cell.len()),
        append_row: false,
    })
}
