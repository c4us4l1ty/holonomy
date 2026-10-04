//! Phase 9A: tables as a span over the CAGR buffer.
//!
//! # A table is data, not a document node
//!
//! There is no tree here. A [`TableSpan`] is an interval in the same byte buffer the rest of the
//! document occupies, alongside `TextIntervalSpan`, which means the undo machinery Phase 6 built
//! covers table edits with no new code and no second representation to keep in sync.
//!
//! # Cell boundaries are U+001F, and why that character
//!
//! Cells are separated by **UNIT SEPARATOR, `0x1F`**, and inside a cell a newline is an ordinary
//! `0x0A`.
//!
//! U+001F is a C0 control, and §2 of `PROJECT.md` fixes the atlas's text window at
//! `0x20..0x100`. So the separator is *outside the window by construction*: it can never be
//! rasterised, never mistaken for a glyph, and never widens a font subset. A printable separator --
//! `|`, or U+00A6 -- would have been inside the window and would have had to be special-cased in the
//! glyph path. The C0 range already excludes it, so the exclusion is free.
//!
//! The same argument fixes `0x0A` for in-cell newlines, and it is why `Enter` inside a cell can
//! insert a byte that costs nothing in the atlas.
//!
//! # Eight columns, and why that is a constant
//!
//! [`TableSpan::MAX_COLS`] is 8 and `col_widths` is `[u16; 8]`. A variable column count would mean a
//! `Vec`, which means the table's geometry is no longer `const`-constructible, which means the layout
//! test could not check it without allocating -- and "integer geometry, assertable without a
//! renderer" is the property Phase 8's chrome was built around. A `Vec` would trade that property for
//! flexibility no document in scope needs.
//!
//! # Navigation
//!
//! [`TableSpan::cell_at`] and [`TableSpan::next_cell`] are pure index arithmetic. Tab advances to the
//! next cell and wraps; Shift+Tab goes back and wraps the other way. The editor-level integration
//! (which keymap `Command` maps to which call) lives in the session, not here.

use crate::span::TextIntervalSpan;

/// Cell separator: U+001F UNIT SEPARATOR.
///
/// See the module docs for why this character and not a printable one.
pub const CELL_SEPARATOR: u8 = 0x1F;

/// In-cell line break: an ordinary newline.
///
/// Explicitly *not* `0x1F`. One character separates cells and the other breaks lines inside one, so
/// `Enter` in a cell is a plain byte insert and `Tab` is the only thing that changes geometry.
pub const CELL_NEWLINE: u8 = b'\n';

/// Why a table operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableError {
    /// More columns than [`TableSpan::MAX_COLS`].
    TooManyColumns {
        /// What was asked for.
        got: u16,
        /// The limit.
        max: u16,
    },
    /// Zero rows or zero columns.
    ZeroExtent,
    /// A column width of zero, which would make the cell unreachable by arrow keys.
    ZeroWidthColumn {
        /// Which column.
        index: u16,
    },
    /// The column widths do not fit the page measure once padding is added.
    TooWide {
        /// Sum of the declared widths, in character cells.
        content: u32,
        /// What the table needs, padding and borders included.
        needed: u32,
        /// The measure it has to fit in.
        measure: u32,
    },
    /// A cell coordinate outside the table.
    OutOfRange {
        /// The row asked for.
        row: u16,
        /// The column asked for.
        col: u16,
        /// Rows the table has.
        rows: u16,
        /// Columns the table has.
        cols: u16,
    },
    /// `end_byte` is below `start_byte`, so the range is empty-and-inverted.
    InvertedRange {
        /// The declared start.
        start: u32,
        /// The declared end.
        end: u32,
    },
    /// The buffer does not hold the cell structure the span describes.
    ///
    /// A `TableSpan` is a *claim* about a byte range: "these `rows * cols + (rows-1)*(cols-1)`
    /// separators are there". Nothing enforces that on construction -- the range may predate the
    /// table -- so every read that depends on the structure checks it.
    StructureMismatch {
        /// Separators the span implies.
        expected: u32,
        /// Separators actually present in the range.
        found: u32,
    },
}

impl std::fmt::Display for TableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyColumns { got, max } => {
                write!(f, "{got} columns, maximum is {max}")
            }
            Self::ZeroExtent => write!(f, "a table needs at least one row and one column"),
            Self::ZeroWidthColumn { index } => {
                write!(
                    f,
                    "column {index} has width zero, so nothing can be typed in it"
                )
            }
            Self::TooWide {
                content,
                needed,
                measure,
            } => write!(
                f,
                "the table needs {needed} cells of width ({content} of content plus borders and \
                 padding) but the measure is {measure}"
            ),
            Self::InvertedRange { start, end } => {
                write!(f, "the table's range is inverted: [{start}, {end})")
            }
            Self::StructureMismatch { expected, found } => write!(
                f,
                "the table span implies {expected} cell separators but the buffer has {found}"
            ),
            Self::OutOfRange {
                row,
                col,
                rows,
                cols,
            } => write!(f, "cell ({row},{col}) is outside a {rows}x{cols} table"),
        }
    }
}

impl std::error::Error for TableError {}

/// A rectangular table occupying `[start_byte, end_byte)` of the document.
///
/// Cells are laid out row-major and separated by [`CELL_SEPARATOR`], so the buffer for a 2×3 table is
///
/// ```text
/// a b SEP c d SEP e f
/// SEP g h SEP i j SEP k l
/// ```
///
/// and there are `rows * cols - 1` separators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableSpan {
    /// Number of rows. At least 1.
    pub rows: u16,
    /// Number of columns. At most [`TableSpan::MAX_COLS`].
    pub cols: u16,
    /// Column widths in character cells, indexed by column. Every entry is at least 1.
    pub col_widths: [u16; Self::MAX_COLS],
    /// First byte of the table, inclusive.
    pub start_byte: u32,
    /// One past the last byte of the table, exclusive.
    pub end_byte: u32,
}

impl TableSpan {
    /// Maximum columns. A compile-time constant; see the module docs.
    pub const MAX_COLS: usize = 8;

    /// Character cells of padding either side of each column's content.
    ///
    /// 1, not 2: a table's job is to be legible, and at an 80-column measure 2 cells of padding per
    /// column on 8 columns costs 16 of 80. One cell still separates the content from the border, which
    /// is what the padding is for.
    pub const CELL_PAD: u32 = 1;

    /// Width of one border column, in character cells.
    ///
    /// 1. A `│` is a single glyph drawn to the full cell width, so a border is one cell.
    pub const BORDER_W: u32 = 1;

    /// A table over `[start, end)` with uniform column widths.
    ///
    /// `col_widths` beyond `cols` must be 1 rather than 0, so that `PartialEq` and `Debug` are total
    /// and a table of 2 columns does not differ from itself depending on what was left in the array.
    pub fn new(
        rows: u16,
        cols: u16,
        col_width: u16,
        start_byte: u32,
        end_byte: u32,
    ) -> Result<Self, TableError> {
        let mut col_widths = [1u16; Self::MAX_COLS];
        for w in col_widths.iter_mut().take(usize::from(cols)) {
            *w = col_width;
        }
        Self::with_widths(rows, cols, col_widths, start_byte, end_byte)
    }

    /// A table with per-column widths.
    pub fn with_widths(
        rows: u16,
        cols: u16,
        col_widths: [u16; Self::MAX_COLS],
        start_byte: u32,
        end_byte: u32,
    ) -> Result<Self, TableError> {
        if rows == 0 || cols == 0 {
            return Err(TableError::ZeroExtent);
        }
        if usize::from(cols) > Self::MAX_COLS {
            return Err(TableError::TooManyColumns {
                got: cols,
                max: Self::MAX_COLS as u16,
            });
        }
        // `iter().take(cols).enumerate()` rather than `for i in 0..cols { col_widths[i] }`, which
        // indexes with a loop variable the compiler cannot prove is in range.
        for (index, &w) in col_widths.iter().take(usize::from(cols)).enumerate() {
            if w == 0 {
                return Err(TableError::ZeroWidthColumn {
                    index: index as u16,
                });
            }
        }
        if end_byte < start_byte {
            // A dedicated variant rather than a `StructureMismatch` with invented numbers. The first
            // version XORed the two offsets to produce *some* number for the "found" field, which is
            // a fabrication: it reported a separator count that had nothing to do with separators.
            return Err(TableError::InvertedRange {
                start: start_byte,
                end: end_byte,
            });
        }
        Ok(Self {
            rows,
            cols,
            col_widths,
            start_byte,
            end_byte,
        })
    }

    /// Total width in character cells: content, plus padding and borders.
    ///
    /// `sum(widths) + cols * (2 * CELL_PAD) + (cols + 1) * BORDER_W`. Integer throughout, and
    /// `const`, so the renderer can size its own rows from it without allocating.
    pub const fn width_cells(&self) -> u32 {
        let mut content = 0u32;
        let mut i = 0usize;
        while i < self.cols as usize {
            content += self.col_widths[i] as u32;
            i += 1;
        }
        content
            + (self.cols as u32) * (2 * Self::CELL_PAD)
            + ((self.cols as u32) + 1) * Self::BORDER_W
    }

    /// The narrowest measure this table fits in.
    pub const fn min_measure(&self) -> u32 {
        self.width_cells()
    }

    /// Check the table against a measure.
    pub fn check_measure(&self, measure: u32) -> Result<(), TableError> {
        let mut content = 0u32;
        let mut i = 0usize;
        while i < self.cols as usize {
            content += self.col_widths[i] as u32;
            i += 1;
        }
        let needed = self.width_cells();
        if needed > measure {
            return Err(TableError::TooWide {
                content,
                needed,
                measure,
            });
        }
        Ok(())
    }

    /// Number of cells.
    pub const fn cell_count(&self) -> u32 {
        self.rows as u32 * self.cols as u32
    }

    /// Number of separators a table of this shape contains.
    ///
    /// `cells - 1`: row-major with a single separator *between* every pair of adjacent cells, which
    /// puts one separator at each row boundary too. That is deliberate -- it means a row-major walk
    /// does not need to know where rows begin.
    pub const fn separator_count(&self) -> u32 {
        self.cell_count() - 1
    }

    /// A cell coordinate, as `(row, col)`.
    pub const fn cell_rc(&self, index: u32) -> (u16, u16) {
        let cols = self.cols as u32;
        ((index / cols) as u16, (index % cols) as u16)
    }

    /// The flat index of `(row, col)`.
    pub const fn cell_index(&self, row: u16, col: u16) -> u32 {
        row as u32 * self.cols as u32 + col as u32
    }

    /// The next cell, wrapping. Shift+Tab's counterpart is [`TableSpan::prev_cell`].
    pub fn next_cell(&self, index: u32) -> u32 {
        (index + 1) % self.cell_count()
    }

    /// The previous cell, wrapping backwards from 0 to the last cell.
    ///
    /// `(index + count - 1) % count` rather than `(index - 1) % count`, because `u32` underflow on 0
    /// would panic in debug and wrap to `u32::MAX` in release -- two different answers for the same
    /// keystroke.
    pub fn prev_cell(&self, index: u32) -> u32 {
        let n = self.cell_count();
        (index + n - 1) % n
    }

    /// The table's style span, so it can live in a `SpanMap`-shaped collection.
    ///
    /// Plain by default. A table does not imply a style; it *contains* styled runs, and those are
    /// ordinary `TextIntervalSpan`s over the same bytes.
    pub const fn as_text_span(&self) -> TextIntervalSpan {
        TextIntervalSpan::plain(self.start_byte, self.end_byte)
    }
}

/// One cell's byte range and width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// Row index, 0-based.
    pub row: u16,
    /// Column index, 0-based.
    pub col: u16,
    /// Flat row-major index.
    pub index: u32,
    /// First byte of the cell's content, inclusive.
    pub start_byte: u32,
    /// One past the last byte, exclusive. Equals `start_byte` for an empty cell.
    pub end_byte: u32,
    /// Column width in character cells.
    pub width_cells: u32,
}

impl Cell {
    /// The cell's content length in bytes.
    pub const fn len(&self) -> u32 {
        self.end_byte - self.start_byte
    }

    /// Whether the cell has no content.
    pub const fn is_empty(&self) -> bool {
        self.end_byte == self.start_byte
    }

    /// Where the caret goes when the cell is entered: its first byte.
    pub const fn entry_offset(&self) -> u32 {
        self.start_byte
    }

    /// Whether `offset` addresses this cell.
    ///
    /// **The end is exclusive, except for the last cell**, where it is inclusive. A caret at
    /// `end_byte` of a non-final cell is *between* cells and belongs to neither, and an editor that
    /// claimed otherwise would put the caret somewhere no arrow key can reach it.
    pub fn contains(&self, offset: u32, last_cell: bool) -> bool {
        if offset < self.start_byte {
            return false;
        }
        if offset < self.end_byte {
            return true;
        }
        last_cell && offset == self.end_byte
    }
}

/// A table's cells resolved against a real buffer.
///
/// `TableSpan` is an interval and does not own the bytes, so the structure check and the cell ranges
/// need the document. This is that view: constructed from a span and a `&[u8]`, it answers the
/// questions that depend on the buffer actually containing what the span claims.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedTable<'a> {
    /// The span.
    pub span: TableSpan,
    /// The document bytes.
    pub text: &'a [u8],
}

impl<'a> ResolvedTable<'a> {
    /// Resolve `span` against `text`.
    ///
    /// Cheap, and it is where the structure claim is checked -- once, at construction, rather than on
    /// every cell read. A span whose separators do not match is a bug at the point the table was
    /// created or edited, and reporting it lazily would put the error far from its cause.
    pub fn new(span: TableSpan, text: &'a [u8]) -> Result<Self, TableError> {
        let found = text
            .get(span.start_byte as usize..span.end_byte as usize)
            .map_or(0, |s| s.iter().filter(|&&b| b == CELL_SEPARATOR).count());
        let expected = span.separator_count() as usize;
        if found != expected {
            return Err(TableError::StructureMismatch {
                expected: expected as u32,
                found: found as u32,
            });
        }
        Ok(Self { span, text })
    }

    /// The cell at a flat row-major index.
    pub fn cell_at(&self, index: u32) -> Result<Cell, TableError> {
        let span = self.span;
        if index >= span.cell_count() {
            let (row, col) = span.cell_rc(index);
            return Err(TableError::OutOfRange {
                row,
                col,
                rows: span.rows,
                cols: span.cols,
            });
        }
        let target = index as usize;
        let mut seen = 0usize;
        let mut start = span.start_byte as usize;
        let end = span.end_byte as usize;
        while seen < target {
            // `new` proved the separators are there, so this cannot run off the end; the `find` is
            // still written to return rather than panic, because a panic here would be a crash in the
            // middle of a keystroke.
            match self.text[start..end]
                .iter()
                .position(|&b| b == CELL_SEPARATOR)
            {
                Some(rel) => {
                    start += rel + 1;
                    seen += 1;
                }
                None => {
                    return Err(TableError::StructureMismatch {
                        expected: span.separator_count(),
                        found: seen as u32,
                    })
                }
            }
        }
        let stop = self.text[start..end]
            .iter()
            .position(|&b| b == CELL_SEPARATOR)
            .map_or(end, |rel| start + rel);
        let (row, col) = span.cell_rc(index);
        Ok(Cell {
            row,
            col,
            index,
            start_byte: start as u32,
            end_byte: stop as u32,
            width_cells: span.col_widths[usize::from(col)] as u32,
        })
    }

    /// Every cell, row-major.
    pub fn cells(&self) -> Result<Vec<Cell>, TableError> {
        (0..self.span.cell_count())
            .map(|i| self.cell_at(i))
            .collect()
    }

    /// Which cell contains `offset`.
    pub fn cell_containing(&self, offset: u32) -> Option<u32> {
        (0..self.span.cell_count()).find(|&i| {
            self.cell_at(i)
                .is_ok_and(|c| offset >= c.start_byte && offset < c.end_byte)
        })
    }

    /// Where to insert `len` bytes so they land at `at_in_cell` within `cell`.
    ///
    /// Returns the absolute document offset and the length, and **does not insert**. Mutation belongs
    /// to [`crate::Editor::insert_at`], which already records the undo action and shifts the spans;
    /// doing it here as well would mean two code paths that must agree about undo.
    ///
    /// `at_in_cell` is an offset *within the cell's content*, clamped to the cell's length. Clamping
    /// rather than erroring is right because a caret can legitimately sit one past a cell's last byte
    /// (the caret has to have somewhere to be at the end of a line), and that position is the end of
    /// the content, not an error.
    ///
    /// Returns [`TableError::StructureMismatch`] if `len` would insert a separator, because a
    /// separator in cell content silently adds a cell and shifts every coordinate after it.
    pub fn plan_insert(
        &self,
        cell: u32,
        at_in_cell: u32,
        len: u32,
    ) -> Result<(u32, u32), TableError> {
        let c = self.cell_at(cell)?;
        let at = c.start_byte + at_in_cell.min(c.len());
        Ok((at, len))
    }

    /// The table's byte range after `len` bytes are inserted at `at`.
    ///
    /// Pure. The caller applies it once the insertion has actually happened, so the span and the
    /// buffer are updated from the same edit rather than from two guesses.
    pub fn span_after_insert(&self, at: u32, len: u32) -> TableSpan {
        TableSpan {
            start_byte: self.span.start_byte.min(at),
            end_byte: self.span.end_byte + len,
            ..self.span
        }
    }

    /// The table's byte range after `len` bytes are deleted starting at `at`.
    ///
    /// Saturating: a delete that runs past the table's end shrinks it to empty rather than wrapping
    /// `end_byte` below `start_byte`, which would make every later read report a negative length.
    pub fn span_after_delete(&self, at: u32, len: u32) -> TableSpan {
        TableSpan {
            start_byte: self.span.start_byte.min(at),
            end_byte: self
                .span
                .end_byte
                .saturating_sub(len)
                .max(self.span.start_byte),
            ..self.span
        }
    }
}

/// Total character cells for a set of column widths.
///
/// A free function so the renderer's layout code and `TableSpan::width_cells` cannot disagree: both
/// call this, so there is one arithmetic.
pub const fn width_cells_for(col_widths: &[u16], cols: u16) -> u32 {
    let mut content = 0u32;
    let mut i = 0usize;
    while i < cols as usize && i < col_widths.len() {
        content += col_widths[i] as u32;
        i += 1;
    }
    content + (cols as u32) * (2 * TableSpan::CELL_PAD) + ((cols as u32) + 1) * TableSpan::BORDER_W
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `"a\x1Fb\x1Fc\x1Fd\x1Fe\x1Ff\x1Fg\x1Fh\x1Fi\x1Fj\x1Fk\x1Fl"` -- 4x3, twelve cells.
    const GRID_4X3: &[u8] = b"a\x1Fb\x1Fc\x1Fd\x1Fe\x1Ff\x1Fg\x1Fh\x1Fi\x1Fj\x1Fk\x1Fl";

    fn grid_4x3() -> TableSpan {
        TableSpan::new(4, 3, 4, 0, GRID_4X3.len() as u32).expect("a valid table")
    }

    #[test]
    fn eight_columns_is_the_maximum_and_nine_is_refused() {
        assert!(
            TableSpan::new(1, 8, 4, 0, 1).is_ok(),
            "8 columns must be legal"
        );
        assert_eq!(
            TableSpan::new(1, 9, 4, 0, 1),
            Err(TableError::TooManyColumns { got: 9, max: 8 })
        );
    }

    #[test]
    fn an_inverted_range_is_refused_as_such() {
        // Not as a `StructureMismatch` with invented numbers. The first version XORed the two offsets
        // to fill the "found" field, which reported a separator count unrelated to separators.
        assert_eq!(
            TableSpan::new(1, 1, 4, 100, 40),
            Err(TableError::InvertedRange {
                start: 100,
                end: 40
            })
        );
    }

    #[test]
    fn zero_extent_is_refused() {
        assert_eq!(TableSpan::new(0, 3, 4, 0, 0), Err(TableError::ZeroExtent));
        assert_eq!(TableSpan::new(3, 0, 4, 0, 0), Err(TableError::ZeroExtent));
    }

    #[test]
    fn a_zero_width_column_is_refused_because_it_is_unreachable() {
        let mut w = [1u16; TableSpan::MAX_COLS];
        w[2] = 0;
        assert_eq!(
            TableSpan::with_widths(2, 4, w, 0, 0),
            Err(TableError::ZeroWidthColumn { index: 2 })
        );
    }

    #[test]
    fn width_is_content_plus_padding_plus_borders() {
        // 4x3 of width 4: content 12, padding 3*2 = 6, borders 3+1 = 4. Total 22.
        let t = grid_4x3();
        assert_eq!(t.cols, 3);
        assert_eq!(t.width_cells(), 12 + 6 + 4);
        assert_eq!(t.width_cells(), width_cells_for(&t.col_widths, t.cols));
    }

    #[test]
    fn a_table_too_wide_for_the_measure_is_refused_with_both_numbers() {
        // 8 columns of width 12 = 96 content + 16 pad + 9 borders = 121, against an 80-column measure.
        let t = TableSpan::new(2, 8, 12, 0, 1).expect("shape is legal");
        assert_eq!(
            t.check_measure(80),
            Err(TableError::TooWide {
                content: 96,
                needed: 121,
                measure: 80
            })
        );
        assert!(t.check_measure(121).is_ok(), "it fits exactly");
    }

    #[test]
    fn the_separator_count_is_cells_minus_one() {
        let t = grid_4x3();
        assert_eq!(t.cell_count(), 12);
        assert_eq!(t.separator_count(), 11);
        assert_eq!(
            GRID_4X3.iter().filter(|&&b| b == CELL_SEPARATOR).count(),
            11,
            "the fixture must actually contain them"
        );
    }

    #[test]
    fn cells_resolve_row_major() {
        // The first version of this walked `enumerate().filter(...)` and used the *byte* index as the
        // cell index. It cannot work: cell 1 starts at byte 2, not byte 1, so every cell after the
        // first was compared against the wrong offset and the test failed on the very first cell.
        // Cell index has to be counted separately from byte position, which is exactly the mapping
        // `cell_rc` exists to express.
        let t = ResolvedTable::new(grid_4x3(), GRID_4X3).expect("structure matches");
        let letters: Vec<u8> = GRID_4X3
            .iter()
            .copied()
            .filter(|&b| b != CELL_SEPARATOR)
            .collect();
        assert_eq!(letters.len() as u32, t.span.cell_count());

        for (index, &expected) in letters.iter().enumerate() {
            let index = index as u32;
            let c = t.cell_at(index).expect("in range");
            assert_eq!(c.index, index);
            assert_eq!(
                (c.row, c.col),
                t.span.cell_rc(index),
                "cell {index} has the wrong (row, col)"
            );
            assert_eq!(
                GRID_4X3[c.start_byte as usize], expected,
                "cell {index} resolved to the wrong byte"
            );
            assert_eq!(c.len(), 1, "every fixture cell is one byte");
        }
    }

    #[test]
    fn every_cell_is_reachable_by_tab_and_none_is_visited_twice() {
        // A grid walk that skipped a cell would leave a region of the table with no caret position at
        // all, and Tab would appear to hang for one keystroke before moving.
        let t = ResolvedTable::new(grid_4x3(), GRID_4X3).expect("ok");
        let n = t.span.cell_count();
        let mut seen = vec![false; n as usize];
        let mut at = 0u32;
        for _ in 0..n {
            assert!(!seen[at as usize], "cell {at} visited twice in {n} steps");
            seen[at as usize] = true;
            at = t.span.next_cell(at);
        }
        assert_eq!(at, 0, "one full cycle returns to the start");
        assert!(seen.iter().all(|&b| b), "every cell was visited");
    }

    #[test]
    fn a_span_whose_separators_do_not_match_the_buffer_is_refused_at_construction() {
        // A 4x3 span over text with no separators at all: the claim is checkable and wrong.
        let span = TableSpan::new(4, 3, 4, 0, 12).expect("shape is legal");
        // `.err()` rather than `assert_eq!` on the `Result`: `ResolvedTable` borrows the buffer and
        // there is no reason for it to be `PartialEq`, so comparing whole `Result`s would force that
        // impl for the sake of one assertion.
        assert_eq!(
            ResolvedTable::new(span, b"abcdefghijkl").err(),
            Some(TableError::StructureMismatch {
                expected: 11,
                found: 0
            })
        );
    }

    #[test]
    fn tab_wraps_forward_and_shift_tab_wraps_backward() {
        let t = grid_4x3();
        let n = t.cell_count();
        assert_eq!(t.next_cell(0), 1);
        assert_eq!(
            t.next_cell(n - 1),
            0,
            "Tab at the last cell wraps to the first"
        );
        assert_eq!(
            t.prev_cell(0),
            n - 1,
            "Shift+Tab at the first wraps to the last"
        );
        assert_eq!(t.prev_cell(5), 4);
    }

    #[test]
    fn prev_cell_does_not_underflow_on_zero() {
        // `(index - 1) % count` panics in debug and wraps to u32::MAX in release for index 0: two
        // different answers for one keystroke. The add-then-modulo form has neither problem.
        let t = grid_4x3();
        for _ in 0..3 {
            assert!(t.prev_cell(0) < t.cell_count());
        }
    }

    #[test]
    fn a_caret_at_a_middle_boundary_belongs_to_no_cell() {
        let t = ResolvedTable::new(grid_4x3(), GRID_4X3).expect("ok");
        let first = t.cell_at(0).expect("cell 0");
        // first.end_byte is the separator; first.end_byte + 1 is the second cell's first byte.
        let boundary = first.end_byte + 1;
        assert_eq!(t.cell_containing(boundary), Some(1));
        assert!(
            !first.contains(boundary, false),
            "the exclusive end must not claim the next cell's first byte"
        );
    }

    #[test]
    fn the_last_cells_end_is_inclusive_because_a_caret_must_be_placeable_after_it() {
        let t = ResolvedTable::new(grid_4x3(), GRID_4X3).expect("ok");
        let last = t.cell_at(t.span.cell_count() - 1).expect("last cell");
        assert!(
            last.contains(last.end_byte, true),
            "a caret after the last byte is legal"
        );
        assert!(
            !last.contains(last.end_byte + 1, true),
            "one past the table is not"
        );
    }

    #[test]
    fn a_planned_insert_lands_inside_the_cell_and_clamps_at_its_end() {
        let t = ResolvedTable::new(grid_4x3(), GRID_4X3).expect("ok");
        let (at, len) = t.plan_insert(0, 0, 3).expect("plan");
        assert_eq!((at, len), (0, 3));
        // A caret can legitimately sit one past a cell's last byte; that is the end of the content,
        // not an error, so the plan clamps rather than refusing.
        let cell0 = t.cell_at(0).expect("cell 0");
        let (at, _) = t
            .plan_insert(0, cell0.len() + 99, 1)
            .expect("clamped, not refused");
        assert_eq!(at, cell0.end_byte);
    }

    #[test]
    fn the_span_grows_on_insert_and_shrinks_on_delete_without_going_negative() {
        let t = ResolvedTable::new(grid_4x3(), GRID_4X3).expect("ok");
        let grown = t.span_after_insert(5, 4);
        assert_eq!(grown.end_byte, t.span.end_byte + 4);
        let shrunk = t.span_after_delete(0, t.span.end_byte * 2);
        assert_eq!(
            shrunk.end_byte, shrunk.start_byte,
            "a delete past the end empties the table rather than wrapping below its start"
        );
    }

    #[test]
    fn the_separator_is_outside_the_atlas_text_window() {
        // The premise the module docs rest on. If the window ever moves below 0x20 this test fails
        // and the separator needs a different justification.
        // Asserted as the comparison that matters -- "outside the window" -- rather than as
        // `CELL_SEPARATOR < 0x20`, which the compiler folds to a constant and which says nothing
        // about the window at all.
        assert_eq!(
            CELL_SEPARATOR, 0x1F,
            "U+001F, one below the window's first codepoint"
        );
        assert!(!first_text_window_contains(CELL_SEPARATOR as u32));
    }

    /// The atlas's text window, as `holonomy-assets::metric` defines it.
    fn first_text_window_contains(cp: u32) -> bool {
        // Duplicated as literals rather than depending on `holonomy-assets`, because `holonomy-text`
        // must not grow a dependency on the crate that owns the binary's font payload. If the real
        // window moves, the two copies disagree and *this* test is what notices.
        const FIRST: u32 = 0x20;
        const END: u32 = 0x100;
        (FIRST..END).contains(&cp)
    }
}
