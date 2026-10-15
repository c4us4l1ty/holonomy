//! **How a logical line becomes one or more visual rows.** Part 45.
//!
//! # Why this is a module and not a function in the emitter
//!
//! **Because soft wrap is not a painting decision — it is a coordinate system, and a coordinate system
//! that lives inside a paint loop is one that only the paint loop agrees with.**
//!
//! Part 39b found that a line longer than the measure was *cut*: 72 cells measured, 72 cells drawn,
//! and the remainder neither drawn nor reachable by the caret. The obvious repair is to draw the
//! remainder on a second row, and that repair is wrong in four places at once, because **a visual row
//! is not a logical line**:
//!
//! | what | what it indexes by now | what it must index by |
//! |---|---|---|
//! | `Layout::caret_at` | `y / cell_h` → a logical line | the visual row under the pointer |
//! | `Session::column_at_pen` | the whole line | the *segment* the pointer is in |
//! | the selection highlight | one rect per line | one rect per segment |
//! | `emit_body_text` | one run per line | one run per segment |
//!
//! **Four subsystems each deriving the row boundaries for themselves is four subsystems that will
//! disagree**, and the disagreement is invisible: the text is drawn, the caret is somewhere, and the
//! highlight is a slightly wrong number of pixels. That is the exact failure mode parts 22, 23 and 33
//! each found by hand.
//!
//! **So the splitting rule lives here, once, and every consumer reads it.** The rule is a pure
//! function of `(bytes, measure)` with no access to the layout, the painter, the scroll position or
//! the clock, and the gates in `tests/wrap.rs` hold it on its own.
//!
//! # The rule, in full
//!
//! For one line's content — the line's bytes **without** its `\n` — and a measure of `m` cells:
//!
//! 1. **Every codepoint occupies a cell, whitespace included.** A space is a column of the row; if it
//!    were free, a line of spaces would be one row per measure *visually* and zero cells *arithmetically*,
//!    and those are the kind of two answers that disagree.
//! 2. **Break *after* the last whitespace at or before the overflow point.** A wrap that leaves a
//!    leading space on every continuation row indents the whole paragraph by one cell, which is the
//!    most recognisable sign of a naive wrap. **The space stays on the row it ends**, so the rows
//!    still tile the line exactly -- see the correction in [`split_line`], which is where the first
//!    version got this wrong.
//! 3. **Break hard when there is no whitespace to break at.** A 300-character URL, a digest or a path
//!    with no spaces all exceed any measure, and refusing to break them makes them *unreachable*
//!    rather than merely ugly. **A row is never longer than the measure.**
//! 4. **Break between codepoints, never inside one.** A multi-byte character is never half on one row
//!    and half on the next.
//! 5. **An empty line is one row of zero cells, and the row still exists.** A zero-row line has
//!    nowhere to put a caret, so a user cannot click the blank line they just pressed Return on.
//!
//! # Why the measure is in cells and not in pixels
//!
//! **Because every other length in this program is in cells**: the ruler counts them, `Caret::locate`
//! places the caret at `column * cell_w`, damage rects are `cell_h` tall, and the tables lay
//! themselves out on them. **A wrap point measured in pixels would put the last glyph of a row a
//! fraction of a cell past the edge, and the caret would have no cell to sit in** — the row would end
//! between two of the grid's own lines.
//!
//! **It also introduces nothing new.** The body face is proportional, so a 72-cell row of glyphs
//! drawn at their own advances already ends wherever their advances end; part 39b measured exactly
//! this. **The ragged right margin is the existing behaviour of a proportional face, not something
//! wrapping adds.**
//!
//! What it *does* cost is worth stating: **the wrap column and the drawn ink disagree by up to one
//! cell per row**, because a cell is the monospace advance (10 at 16 ppem) and the body face's mean
//! advance is 8.58. That is a *shortfall* — the ink ends before the measured column — which is the
//! safe direction: nothing is drawn over the right margin and nothing is clipped.

/// One visual row: a run of bytes from one logical line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// The logical line this row is part of.
    ///
    /// **Carried rather than derived, because the map spans several lines and a caller asking "which
    /// line is this row on?" must not have to sum the rows of the lines before it.**
    pub line: u32,
    /// The byte offset, into the document, where this row starts.
    pub start: u32,
    /// The byte offset where it ends, **exclusive, and never including the line's `\n`.**
    ///
    /// **The newline is excluded because the row is what a caret and a highlight rectangle are sized
    /// against**, and a row that included its own newline would be one cell wider than the text it
    /// shows.
    pub end: u32,
    /// The column this row starts at: `0` for a line's first row, otherwise the column the previous
    /// row of the same line ended at.
    ///
    /// **Carried rather than recomputed from `start`.** Recomputing it means walking the line again,
    /// and this is the value the emitter needs to place a run and the value a gate needs to say where
    /// a continuation begins.
    pub start_column: u32,
    /// How many whole cells this row occupies.
    ///
    /// **Zero only for the empty line's row**, and that zero is rule 5, not an accident.
    pub cells: u32,
}

impl Row {
    /// How many bytes this row covers: `end - start`.
    #[must_use]
    pub const fn len(&self) -> u32 {
        self.end - self.start
    }

    /// Whether this row covers no bytes.
    ///
    /// **`#[must_use]` because a caller sizing a caret box from a zero-length row gets a zero-width
    /// box**, which is the bug rather than the answer.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.end == self.start
    }

    /// The byte range this row covers, for slicing a page buffer.
    #[must_use]
    pub const fn range(&self) -> std::ops::Range<u32> {
        self.start..self.end
    }

    /// Whether this row is a *continuation* — a second or later row of its line.
    ///
    /// ## `start_column > 0`, with nothing else, and that is a correction
    ///
    /// **The first version was `start_column > 0 && start_column < measure`, on the reasoning that "a
    /// continuation starts at a column *above* the measure"** — a row that filled its predecessor
    /// exactly would start *at* the measure, and a short final row of a line that wrapped would start
    /// below it, and neither is a continuation in the intended sense.
    ///
    /// **Both halves of that reasoning are wrong, and `tests/wrap.rs` found it.**
    ///
    /// - **The `< measure` excludes the commonest continuation.** `alpha bravo charlie delta echo
    ///   foxtrot` at a measure of 20 gives a second row starting at column **20** — the previous row
    ///   used every cell, so it is exactly at the measure and the test said `false`.
    /// - **The distinction it was trying to draw does not exist.** "A short last row" and "a
    ///   continuation" are not different things: **a row that starts at column 3 is a row whose line
    ///   has a row above it**, so it *is* a continuation. There is no non-first row that is not one.
    ///
    /// **So the test is `start_column > 0` alone, and it needs no argument.** `start_column` is 0 for a
    /// line's first row and for that row only — `split_line` initialises it from the previous row of
    /// the *same line*, or to 0 when there is none — which makes this exactly "is not my line's first
    /// row".
    #[must_use]
    pub const fn is_continuation(&self) -> bool {
        self.start_column > 0
    }
}

/// One logical line, as the caller found it: an index, an absolute offset, and the bytes.
///
/// **A struct and not a `(u32, u32, &[u8])`**, for the reason every other tuple in this codebase is
/// a struct: **a three-tuple has no field names at the use site**, and `line, start, content` is
/// exactly the kind of thing a reader has to guess the order of.
#[derive(Debug, Clone, Copy)]
pub struct Line<'a> {
    /// The line's index, 0-based.
    pub line: u32,
    /// The byte offset of the line's first byte, **including its newline if it has one** — the
    /// document's own offset, so a row's `start` needs no adjustment to become a buffer index.
    pub start: u32,
    /// The line's bytes, **with its newline**.
    pub bytes: &'a [u8],
}

/// The visual rows of a run of logical lines, and the measure they were built at.
///
/// # What it is for
///
/// **One instance per paint, covering the visible lines only.** Building the map for a whole document
/// would be O(document) per frame, and FR-1.3's 0.5 ms keystroke budget has no room for that;
/// building it for the visible window is O(page).
///
/// # What it deliberately is not
///
/// **It is not a cache and it holds no text.** `Row` stores offsets, so a caller still needs the
/// bytes to draw — and `DocRun` resolves them at *paint* time, after every row has been emitted,
/// which is why `emit_body_text` reads the whole page into one page-sized buffer rather than one line
/// at a time. **A wrap map that held the text would be a second copy of the page.**
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WrapMap {
    rows: Vec<Row>,
    measure: u32,
}

impl WrapMap {
    /// **Split `lines` into visual rows, `measure` cells wide.**
    ///
    /// # Why the caller supplies the offsets
    ///
    /// **Because a window that starts mid-document cannot recover them.** The obvious design — pass
    /// only the bytes and have the map count lines up from zero — is correct for the first page and
    /// **wrong for every page after it**, by one byte per line already seen. A window on page 40 would
    /// place every row 1,000-odd bytes before where its glyphs are, **and the symptom is a caret
    /// that drifts further off the longer the user scrolls.**
    ///
    /// So `Line::start` is the document's own offset and the map adds nothing to it.
    ///
    /// # `measure` of zero
    ///
    /// **Every row becomes one cell**, via `max(1)`. A zero measure can only come from a page narrower
    /// than one cell, which `ChromeMetrics::for_size` does not produce, **and a paint that panicked on
    /// a degenerate layout would take the window down for a mistake a one-cell measure hides.**
    pub fn build<'a, I>(measure: u32, lines: I) -> Self
    where
        I: IntoIterator<Item = Line<'a>>,
    {
        let measure = measure.max(1);
        let mut rows = Vec::new();
        for line in lines {
            split_line(&line, measure, &mut rows);
        }
        Self { rows, measure }
    }

    /// The visual rows, in document order.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The measure the rows were built at, in whole cells.
    #[must_use]
    pub const fn measure(&self) -> u32 {
        self.measure
    }

    /// How many visual rows there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there are no visual rows.
    ///
    /// **Present because `len` is, and a `map.len() == 0` is the exact spelling clippy asks not to
    /// use.**
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// **The row holding `byte`, or `None` if the map is empty.**
    ///
    /// ## A byte is in exactly one row, and that is why there is no flag here
    ///
    /// **The first version took a `wrapping: bool`.** The argument for it was real: a byte at the end
    /// of a row is *both* the last byte of that row and the first byte of the next, so a click wanted
    /// one answer and pressing Right wanted the other.
    ///
    /// **The argument stopped being true when the rows were made to tile** — the space at a wrap
    /// point stays on the row it ends (see [`split_line`]), so a row's `end` *is* the next row's
    /// `start` and every byte belongs to exactly one row. **Measured over every byte of the longest
    /// fixture in `tests/wrap.rs`, the two flag values differed for 22 bytes and all 22 were past the
    /// end of the content**, where `wrapping: true` answers `None`.
    ///
    /// **`None` is the wrong answer there.** A caret at the end of a document is a real position, the
    /// one a user puts it at with `End` on the last line — so the last row is the right answer and
    /// "no row" is not. **The flag was complexity with a wrong answer on the side that had one**, and
    /// it is gone.
    ///
    /// ## A byte before every row answers the first row
    ///
    /// **Not `None`.** The map is a window, so the caller routinely asks about a byte the window starts
    /// after — and answering `None` there would make every row on the second page of a scroll
    /// unreachable by byte.
    ///
    /// ## A byte past every row answers the last row
    ///
    /// **Same reason, the other end.** The document's last byte is in the last row, and a byte one
    /// past it is the caret's "end of document" position.
    #[must_use]
    pub fn row_at_byte(&self, byte: u32) -> Option<usize> {
        if self.rows.is_empty() {
            return None;
        }
        // **A binary search over `start`, which is sorted because the rows are in document order.**
        // A linear scan would be correct and O(page) per keystroke, which is the kind of thing that is
        // fine at 72 rows and not at 72,000.
        let after = self.rows.partition_point(|r| r.start <= byte);
        if after == 0 {
            return Some(0);
        }
        let here = after - 1;
        // **`wrapping: false`'s arm, which is the only one.** `here` is the last row whose `start` is
        // at or below `byte`; because the rows tile, that is the row containing `byte` — and it is
        // also the right answer for a byte past the end of the document.
        Some(here)
    }

    /// **The indices of `line`'s rows, as a range.**
    ///
    /// **A range rather than a `Vec<&Row>`**, because the emitter walks them with an index it needs
    /// for the damage rect, and a range lets it keep the index without a second lookup.
    ///
    /// **Empty for a line the map does not cover**, which is a line off the top or bottom of the
    /// window — and `Line::rows` is `None` there, so a caller cannot mistake "no rows" for "no line".
    #[must_use]
    pub fn line_rows(&self, line: u32) -> std::ops::Range<usize> {
        let start = self.rows.partition_point(|r| r.line < line);
        let end = start + self.rows[start..].partition_point(|r| r.line == line);
        start..end
    }
}

/// **Append `line`'s rows to `rows`.**
///
/// # The state it carries
///
/// - `col` — cells used on the current row.
/// - `row_start` — index into the content where the current row began.
/// - `start_column` — the current row's starting column, carried forward from the last row pushed.
/// - `break_at` — `(index, cells_used_before_it)` for the most recent whitespace, which is rule 2's
///   break point.
///
/// # Why `break_at` carries the cell count as well as the index
///
/// **Because a row that breaks *after* a space still has to account for the space's cell.** The row
/// is `[row_start, index + 1)` and uses one more cell than was used before the space, so the cell count
/// is `col + 1` — which the pair makes available without a second walk to recompute. **Carrying only
/// the index would make every word-wrapped row one cell short of its own byte count**, which is a row
/// whose reported column ends one cell before its last glyph, for every row of every paragraph.
fn split_line(line: &Line<'_>, measure: u32, rows: &mut Vec<Row>) {
    let content = strip_newline(line.bytes);
    // **A continuation's starting column is the previous row's start column plus its cells**, which
    // is why `start_column` is carried on `Row` at all. **`mut` because a line that wraps more than
    // once advances it** — a three-row line's second continuation starts at twice the measure, and a
    // row that always reported `measure` would put the third row's glyphs where the second row's
    // ended.
    let mut start_column = rows
        .last()
        .filter(|r| r.line == line.line)
        .map_or(0, |r| r.start_column + r.cells);

    // **Rule 5: an empty line is one row.** Not no rows.
    if content.is_empty() {
        rows.push(Row {
            line: line.line,
            start: line.start,
            end: line.start,
            start_column,
            cells: 0,
        });
        return;
    }

    let mut col = 0u32;
    let mut row_start = 0usize;
    let mut break_at: Option<(usize, u32)> = None;
    let mut i = 0usize;
    while i < content.len() {
        // **Rule 4: a continuation byte belongs to the codepoint above it and costs no cell.** It is
        // also stepped over before the fullness test, so a wrap can never land inside a character.
        if content[i] & 0xC0 == 0x80 {
            i += 1;
            continue;
        }
        if col == measure {
            // **The row is full and there is a codepoint to put on the next one.** Rule 2 when there
            // is a break candidate strictly inside this row, rule 3 otherwise.
            //
            // **`w > row_start`** — a candidate at `row_start` would produce an empty row, which is
            // what rule 2 does when the row begins with a space. **`>`, not `>=`: the space at
            // `row_start` is the leading space of this row and consuming it would mean the row starts
            // after itself.**
            let (at, cells) = break_at.filter(|&(w, _)| w > row_start).unwrap_or((i, col));
            // ## The break space **stays on this row**, and that is a correction
            //
            // **The first version *consumed* it -- `end = at`, `row_start = at + 1` -- and the rows
            // stopped tiling.** `tests/wrap.rs::a_lines_rows_tile_it_exactly` failed on its first run
            // with *"alpha bravo charlie" ends at 19 and "delta echo foxtrot" starts at 20*: **byte
            // 19, the space, was in no row at all.**
            //
            // **A gap is not a cosmetic defect and it is the reason the gate exists.** `row_at_byte`
            // walks `start` and compares against `end`, so a byte in a gap resolves to whichever row
            // is nearest rather than the row it is drawn in; the caret would sit at the end of one row
            // while the next row's glyphs start one byte further on. **The rows must partition the
            // line** -- that is the invariant the caret, the highlight and the emitter all rest on.
            //
            // **And a space at the end of a visual row is not an artefact** -- it is what the user
            // typed, it is one cell wide, and the row it ends is exactly the row it belongs to. The
            // *wrap* is what is invisible, not the space.
            let consumed = if at == i { at } else { at + 1 };
            let cells = if at == i { col } else { cells + 1 };
            rows.push(Row {
                line: line.line,
                start: line.start + u32::try_from(row_start).unwrap_or(u32::MAX),
                end: line.start + u32::try_from(consumed).unwrap_or(u32::MAX),
                start_column,
                cells,
            });
            col = 0;
            start_column += cells;
            row_start = consumed.min(content.len());
            // **Cleared, and this is not tidiness.** The candidate space is on the row just pushed.
            // Leaving it set would make the next row break at a byte *before* its own start, which
            // `w > row_start` then rejects — so the row would hard-break instead, and a paragraph of
            // single-letter words would wrap at the measure instead of at its spaces.
            break_at = None;
            i = row_start;
            continue;
        }
        let is_space = content[i] == b' ' || content[i] == b'\t';
        if is_space {
            break_at = Some((i, col));
        }
        col += 1;
        i += 1;
    }
    rows.push(Row {
        line: line.line,
        start: line.start + u32::try_from(row_start).unwrap_or(u32::MAX),
        end: line.start + u32::try_from(content.len()).unwrap_or(u32::MAX),
        start_column,
        cells: col,
    });
}

/// **The line's bytes without its line terminator.**
///
/// **`strip_suffix(b"\n")` and not a trim.** A line ending `\r\n` should lose both, but **the
/// document's own line splitter has already decided what a line terminator is**, and this module must
/// not have a second opinion: a trailing `\r` is content, it is one cell, and it is drawn. **`trim_end`
/// would silently drop every trailing space on every line**, which changes where the wrap falls on
/// every paragraph.
fn strip_newline(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}