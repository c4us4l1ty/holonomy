//! Phase 9A: the integer grid a [`TableSpan`] paints into.
//!
//! # Borders cannot be misaligned, and that is structural
//!
//! A table's geometry is defined in **character cells**, not pixels: `col_widths` is in cells, padding
//! is in cells, and the border is one `│` glyph wide, which is one cell. So every border coordinate
//! this module produces is `origin + k * cell_w` for an integer `k`, and a pixel position for it is
//! always exact.
//!
//! That is worth stating as a property rather than a hope, because "sub-pixel precision" is the phrase
//! in the gate and it is the wrong requirement for a grid: there is no fractional position to get
//! wrong. The test is `every_border_coordinate_is_a_whole_number_of_cells_from_the_origin`, and it
//! holds by construction rather than by rounding.
//!
//! The alternative — widths in pixels — would make one-pixel alignment a matter of arithmetic
//! discipline at every call site, and a table one pixel off on one side is a border that does not meet
//! its own corners.
//!
//! # Junction runes come from an arm mask, not a hardcoded list of ten
//!
//! Each junction is (up, down, left, right) and [`junction`] maps that to a codepoint with a `match`.
//! Sixteen combinations, sixteen answers, and every answer is asserted to be in Phase 4's verified
//! table. Hardcoding the ten runes instead would mean the *four* cases a table never produces
//! (`┤` alone, `├` alone, and two impossible corners) are never checked, and a future grid feature --
//! a merged header row, say -- would find them missing.
//!
//! # Nothing here allocates
//!
//! [`TableGrid`] is `Copy` and [`TableGrid::borders`] returns an iterator. A table is `rows+1` by
//! `cols+1` lines of `cols+1` junctions, which for the gate's 4×3 is 20 junctions and 11 horizontal
//! and 9 vertical runs: small enough that a `Vec` would be harmless, but small enough that not
//! needing one is free.

use holonomy_text::{TableError, TableSpan};

/// One run of border, as a `TextRun` and the rect it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorderRun {
    /// The codepoint to draw, repeated `len` times.
    pub codepoint: u32,
    /// How many cells the run covers.
    pub len: u16,
    /// Left edge, pixels.
    pub x: u32,
    /// Top edge, pixels.
    pub y: u32,
    /// Width in pixels. Exactly `len * cell_w`.
    pub width: u32,
    /// Height in pixels. A junction and a horizontal run are one border row tall.
    pub height: u32,
}

/// A table's pixel geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableGrid {
    /// Left edge of the table's first border column, pixels.
    pub origin_x: u32,
    /// Top edge of the table's first border row, pixels.
    pub origin_y: u32,
    /// Text cell width. Every coordinate is a whole number of these.
    pub cell_w: u32,
    /// Height of a border row, pixels. Normally one text cell.
    pub border_h: u32,
    /// Height of a cell's content, pixels. Normally one text cell.
    pub row_h: u32,
    /// Rows.
    pub rows: u16,
    /// Columns.
    pub cols: u16,
    /// Content width per column, **pixels**.
    col_px: [u32; TableSpan::MAX_COLS],
    /// Left edge of each vertical border line, pixels. `cols + 1` entries are used.
    border_x: [u32; TableSpan::MAX_COLS + 1],
    /// Top edge of each horizontal border line, pixels. `rows + 1` entries are used.
    border_y: [u32; TableSpan::MAX_COLS + 1],
}

impl TableGrid {
    /// Build the grid for `span` at `(origin_x, origin_y)` on a `cell_w`-wide grid.
    ///
    /// `border_h` and `row_h` are **separate** because a border line and a line of text both need a
    /// full cell to draw in, and they cannot share one.
    ///
    /// The first version had a single `cell_h` and put the content rect at
    /// `border_y(row) + border + pad`, which is 16 px below the border line -- 34 px into an 18-px row,
    /// so every cell's text started *below its own bottom border*. Conflating them made the row pitch
    /// too small for the thing it has to hold, and no padding arithmetic could fix it.
    ///
    /// So the pitch is `border_h + row_h` and `height_px` is
    /// `(rows + 1) * border_h + rows * row_h` -- the only combination where each content rect lands
    /// exactly in the gap between two border lines.
    pub fn new(
        span: &TableSpan,
        origin_x: u32,
        origin_y: u32,
        cell_w: u32,
        border_h: u32,
        row_h: u32,
    ) -> Self {
        let mut col_px = [0u32; TableSpan::MAX_COLS];
        let mut border_x = [0u32; TableSpan::MAX_COLS + 1];
        let mut border_y = [0u32; TableSpan::MAX_COLS + 1];

        for (i, &w) in span
            .col_widths
            .iter()
            .take(usize::from(span.cols))
            .enumerate()
        {
            col_px[i] = u32::from(w) * cell_w;
        }

        // `border_x[i+1] = border_x[i] + col[i] + BORDER + 2*PAD`, all in cells then scaled. The
        // recursion is exact because every term is a whole number of cells, which is the whole point.
        let step_pad = 2 * TableSpan::CELL_PAD * cell_w;
        let step_border = TableSpan::BORDER_W * cell_w;
        border_x[0] = origin_x;
        for i in 0..usize::from(span.cols) {
            border_x[i + 1] = border_x[i] + col_px[i] + step_border + step_pad;
        }
        debug_assert!(
            border_x[usize::from(span.cols)] + step_border - origin_x
                == span.width_cells() * cell_w,
            "the recursion and width_cells() must agree; they are two spellings of one arithmetic"
        );
        let pitch = border_h + row_h;
        for (i, y) in border_y
            .iter_mut()
            .take(usize::from(span.rows) + 1)
            .enumerate()
        {
            *y = origin_y + (i as u32) * pitch;
        }

        Self {
            origin_x,
            origin_y,
            cell_w,
            border_h,
            row_h,
            rows: span.rows,
            cols: span.cols,
            col_px,
            border_x,
            border_y,
        }
    }

    /// Total width in pixels. Exactly `span.width_cells() * cell_w`.
    pub const fn width_px(&self) -> u32 {
        self.border_x_at(self.cols) - self.origin_x + TableSpan::BORDER_W * self.cell_w
    }

    /// Total height in pixels: `rows + 1` border rows plus `rows` content rows.
    pub const fn height_px(&self) -> u32 {
        (self.rows as u32 + 1) * self.border_h + (self.rows as u32) * self.row_h
    }

    /// Distance between two adjacent horizontal border lines.
    pub const fn row_pitch(&self) -> u32 {
        self.border_h + self.row_h
    }

    /// Left edge of vertical border line `i`, for `i <= cols`.
    pub const fn border_x_at(&self, i: u16) -> u32 {
        self.border_x[i as usize]
    }

    /// Top edge of horizontal border line `i`, for `i <= rows`.
    pub const fn border_y_at(&self, i: u16) -> u32 {
        self.border_y[i as usize]
    }

    /// The pixel rect of cell `(row, col)`'s **content**, padding and borders excluded.
    pub fn cell_content_rect(&self, row: u16, col: u16) -> Option<(u32, u32, u32, u32)> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        let pad = TableSpan::CELL_PAD * self.cell_w;
        let border = TableSpan::BORDER_W * self.cell_w;
        // Vertically there is no *pad*: a cell's content fills the whole gap between two border
        // lines, and the horizontal padding is what separates content from the vertical rules.
        let x = self.border_x_at(col) + border + pad;
        let y = self.border_y_at(row) + self.border_h;
        Some((x, y, self.col_px[col as usize], self.row_h))
    }

    /// Every border run, in paint order: horizontal lines top to bottom, then vertical.
    ///
    /// Horizontal first because they are drawn first and a vertical that meets a horizontal must not
    /// paint over it at the junction -- the junction glyph is emitted by the horizontal pass, so it
    /// already carries both arms and the vertical pass stops one cell short of each junction.
    pub fn borders(&self) -> BorderIter {
        BorderIter {
            grid: *self,
            step: 0,
            // h * (2v - 1) horizontal items, then (h - 1) * v vertical segments.
            //
            // The first version left the old `(rows+1)*(cols+1) + ...` total in place while `next`
            // was rewritten for the 2v-1 row shape. For a 4x3 table that is 29 against an actual 51,
            // so the iterator stopped after the fourth row's first junction and silently omitted the
            // rest of the table -- no panic, no gap in the run list, just a truncated border. A short
            // `total` is the worst kind of bug here because `borders()` still *looks* complete.
            total: {
                let h = self.rows as usize + 1;
                let v = self.cols as usize + 1;
                h * (2 * v - 1) + (h - 1) * v
            },
        }
    }

    /// True when every border coordinate is a whole number of cells from the origin.
    ///
    /// The property the gate asks for, as a function rather than only a test, so the renderer can
    /// assert it in debug builds for the tables it is actually drawing.
    pub const fn is_cell_aligned(&self) -> bool {
        let mut i = 0usize;
        while i <= self.cols as usize {
            if !(self.border_x[i] - self.origin_x).is_multiple_of(self.cell_w) {
                return false;
            }
            i += 1;
        }
        let mut j = 0usize;
        while j <= self.rows as usize && j < TableSpan::MAX_COLS + 1 {
            if !(self.border_y[j] - self.origin_y).is_multiple_of(self.row_pitch()) {
                return false;
            }
            j += 1;
        }
        true
    }
}

/// Iterator over [`BorderRun`]s. `Copy`, allocation-free.
#[derive(Debug, Clone, Copy)]
pub struct BorderIter {
    grid: TableGrid,
    step: usize,
    total: usize,
}

impl Iterator for BorderIter {
    type Item = BorderRun;

    /// # A horizontal line is `2*v - 1` items, not `v`
    ///
    /// It is junction, run, junction, run, ..., junction -- so a row emits `v` junctions and `v - 1`
    /// runs between them.
    ///
    /// The first version emitted `v` items per row and drew a junction only for `c == 0`, with a run
    /// for every `c > 0`. So a 4x3 table drew four junctions in its leftmost column, three plain
    /// `─` runs, and **nothing at all** for the three other columns: no `┬`, no `┼`, no `┐`. The test
    /// caught it by noticing that `┐` never appeared, which is the kind of bug a "does it look like a
    /// table" check would have missed, because four correct corners and three rules *do* look like a
    /// table until you notice the missing verticals.
    fn next(&mut self) -> Option<Self::Item> {
        if self.step >= self.total {
            return None;
        }
        let step = self.step;
        self.step += 1;

        let g = self.grid;
        let v = g.cols as usize + 1;
        let h = g.rows as usize + 1;
        let per_row = 2 * v - 1;
        let horizontal_items = h * per_row;

        if step < horizontal_items {
            let (r, k) = (step / per_row, step % per_row);
            let y = g.border_y[r];
            if k % 2 == 0 {
                return Some(g.junction_at(r as u16, (k / 2) as u16, y));
            }
            // The run between vertical border lines `k/2` and `k/2 + 1`. One cell short at each end,
            // because both ends are junctions this same pass has already drawn.
            let x0 = g.border_x[k / 2];
            let x1 = g.border_x[k / 2 + 1];
            let cells = (x1 - x0) / g.cell_w;
            if cells < 2 {
                return Some(empty(x0, y, g.cell_w, g.border_h));
            }
            let len = (cells - 2) as u16;
            return Some(BorderRun {
                codepoint: HORIZONTAL,
                len,
                x: x0 + g.cell_w,
                y,
                width: u32::from(len) * g.cell_w,
                height: g.border_h,
            });
        }

        // Pass 2: one `│` per (border line, content row), at the content row's own y.
        //
        // Not one segment spanning the whole `row_pitch`. The pitch is `border_h + row_h` = 36 px and
        // the junction glyphs at each end occupy 18 px of it, so a segment spanning the pitch has
        // nothing left to draw in -- the first version computed `pitch/pitch - 1 == 0` rows and
        // emitted **no vertical border at all**, which looks like a table with no sides.
        //
        // One `│` per content row is also the only correct placement: the stroke is the side of the
        // cell, so it belongs to the content row's height, and the junctions already cover the border
        // rows above and below it.
        let rest = step.saturating_sub(horizontal_items);
        let seg = rest % v;
        let row = rest / v;
        if row >= h - 1 {
            return None;
        }
        Some(BorderRun {
            codepoint: VERTICAL,
            len: 1,
            x: g.border_x[seg],
            y: g.border_y[row] + g.border_h,
            width: g.cell_w,
            height: g.row_h,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.total - self.step, Some(self.total - self.step))
    }
}

/// A run that draws nothing: `len == 0` means zero cells wide, so nothing is painted.
///
/// Used for the degenerate cases -- a table narrower than two cells, a row pitch of zero -- rather
/// than by clamping `len` to 1, which would paint a stroke where no stroke belongs.
const fn empty(x: u32, y: u32, width: u32, height: u32) -> BorderRun {
    BorderRun {
        codepoint: 0,
        len: 0,
        x,
        y,
        width,
        height,
    }
}

impl TableGrid {
    /// The junction glyph at horizontal line `r`, vertical line `c`.
    ///
    /// The arms are decided by position, not looked up: `r == 0` means nothing above, `r == rows`
    /// means nothing below, and likewise for `c`. So the arm mask is a pure function of the two
    /// coordinates and the shape, and [`junction`] is the only place that knows which codepoint an arm
    /// combination is.
    pub fn junction_at(&self, r: u16, c: u16, y: u32) -> BorderRun {
        let up = r > 0;
        let down = r < self.rows;
        let left = c > 0;
        let right = c < self.cols;
        // Straight `Some(..)`, not `Some(..).expect(..)`. An earlier version wrapped this in an
        // `expect` with the message "junction always returns a codepoint" -- true, but it made a
        // total function look fallible, and clippy is right that the `expect` can never fire.
        BorderRun {
            codepoint: junction(up, down, left, right),
            len: 1,
            x: self.border_x[c as usize],
            y,
            width: self.cell_w,
            height: self.border_h,
        }
    }
}

/// `─` U+2500.
const HORIZONTAL: u32 = 0x2500;
/// `│` U+2502.
const VERTICAL: u32 = 0x2502;

/// The codepoint for a junction with the given arms.
///
/// All sixteen combinations, so the four a plain table never produces are still answered rather than
/// left as `Option::None`. A `None` would be a bug at the only call site that has one, which is the
/// worst place to discover it.
pub const fn junction(up: bool, down: bool, left: bool, right: bool) -> u32 {
    match (up, down, left, right) {
        (false, true, false, true) => 0x250C,   // ┌
        (false, true, true, true) => 0x252C,    // ┬
        (false, true, true, false) => 0x2510,   // ┐
        (true, true, false, true) => 0x251C,    // ├
        (true, true, true, true) => 0x253C,     // ┼
        (true, true, true, false) => 0x2524,    // ┤
        (true, false, false, true) => 0x2514,   // └
        (true, false, true, true) => 0x2534,    // ┴
        (true, false, true, false) => 0x2518,   // ┘
        (false, false, false, true) => 0x2500,  // ─  no vertical arm
        (false, false, true, false) => 0x2500,  // ─
        (false, false, true, true) => 0x2500,   // ─
        (true, true, false, false) => 0x2502,   // │  no horizontal arm
        (true, false, false, false) => 0x2502,  // │
        (false, true, false, false) => 0x2502,  // │  down only
        (false, false, false, false) => 0x2500, // nothing: drawn as ─
    }
}

/// Why a table could not be laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridError {
    /// The span itself is invalid.
    Table(TableError),
    /// The table does not fit the measure.
    TooWide(TableError),
}

impl std::fmt::Display for GridError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Table(e) | Self::TooWide(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for GridError {}

/// Lay out `span` against a measure, in pixels, at `origin_x`.
pub fn layout(
    span: &TableSpan,
    origin_x: u32,
    origin_y: u32,
    cell_w: u32,
    border_h: u32,
    row_h: u32,
    measure_px: u32,
) -> Result<TableGrid, GridError> {
    span.check_measure(measure_px / cell_w.max(1))
        .map_err(GridError::TooWide)?;
    Ok(TableGrid::new(
        span, origin_x, origin_y, cell_w, border_h, row_h,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use holonomy_assets::box_drawing;

    fn span_4x3() -> TableSpan {
        TableSpan::new(4, 3, 4, 0, 0).expect("shape is legal")
    }

    #[test]
    fn every_junction_is_in_the_verified_table() {
        for up in [false, true] {
            for down in [false, true] {
                for left in [false, true] {
                    for right in [false, true] {
                        let cp = junction(up, down, left, right);
                        assert!(
                            (0x2500..=0x257F).contains(&cp),
                            "junction({up},{down},{left},{right}) -> U+{cp:04X} is outside the table"
                        );
                        assert!(
                            box_drawing::glyph_kind(cp).is_some(),
                            "U+{cp:04X} has no arms in Phase 4's table"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_ten_runes_a_table_uses_are_the_ones_the_specification_names() {
        // Corner and tee glyphs for a 3x3 grid's nine interior junctions, plus the two runs.
        let seen: Vec<u32> = {
            let g = TableGrid::new(&span_4x3(), 0, 0, 8, 18, 18);
            g.borders()
                .map(|r| r.codepoint)
                .filter(|&c| c != 0)
                .collect()
        };
        // If this ever fails again, the `seen` set is worth printing: the first version just said
        // "U+2510 never appeared" with no indication of what did, which is the least useful failure
        // message available for a geometry bug.
        assert!(
            !seen.is_empty(),
            "the grid emitted nothing at all, so this assertion is not measuring what it claims"
        );
        for expect in [
            0x250Cu32, // ┌
            0x2510,    // ┐
            0x2514,    // └
            0x2518,    // ┘
            0x251C,    // ├
            0x2524,    // ┤
            0x252C,    // ┬
            0x2534,    // ┴
            0x253C,    // ┼
            0x2500,    // ─
            0x2502,    // │
        ] {
            assert!(
                seen.contains(&expect),
                "U+{expect:04X} never appeared in a 4x3 grid"
            );
        }
    }

    #[test]
    fn border_coordinates_are_whole_numbers_of_cells_from_the_origin() {
        // The gate's "sub-pixel precision", restated as the property that actually holds: the table is
        // defined in character cells, so no border can land between pixels.
        for (cw, ch, ox, oy) in [(8u32, 18u32, 0u32, 0u32), (8, 18, 320, 144), (7, 13, 5, 3)] {
            let g = TableGrid::new(&span_4x3(), ox, oy, cw, ch, ch);
            assert!(g.is_cell_aligned(), "cell_w={cw} row_h={ch}");
            for i in 0..=g.cols {
                assert_eq!(
                    (g.border_x_at(i) - ox) % cw,
                    0,
                    "vertical border {i} at {} is not a whole number of {cw}-px cells from {ox}",
                    g.border_x_at(i)
                );
            }
            for i in 0..=g.rows {
                assert_eq!((g.border_y_at(i) - oy) % ch, 0, "horizontal border {i}");
            }
        }
    }

    #[test]
    fn adjacent_borders_are_exactly_one_cell_plus_padding_plus_content_apart() {
        let g = TableGrid::new(&span_4x3(), 100, 200, 8, 18, 18);
        // 4 cells of content + 1 border + 2 pads = 7 cells = 56 px.
        for c in 0..g.cols {
            assert_eq!(
                g.border_x_at(c + 1) - g.border_x_at(c),
                7 * 8,
                "columns must be a whole 7 cells apart"
            );
        }
    }

    #[test]
    fn the_total_width_is_the_spans_width_in_cells_times_the_cell_width() {
        for cw in [6u32, 7, 8, 9, 16] {
            let span = span_4x3();
            let g = TableGrid::new(&span, 0, 0, cw, 18, 18);
            assert_eq!(
                g.width_px(),
                span.width_cells() * cw,
                "cell_w={cw}: the grid and the span disagree on the table's width"
            );
        }
    }

    #[test]
    fn the_height_is_one_more_row_than_the_table_has() {
        // rows + 1 horizontal border lines, each `border_h + row_h` apart.
        let g = TableGrid::new(&span_4x3(), 0, 0, 8, 18, 18);
        // 5 border rows of 18 plus 4 content rows of 18.
        assert_eq!(g.height_px(), 5 * 18 + 4 * 18);
        assert_eq!(g.row_pitch(), 36);
    }

    #[test]
    fn every_cell_content_rect_sits_inside_its_borders() {
        let g = TableGrid::new(&span_4x3(), 64, 36, 8, 18, 18);
        for row in 0..g.rows {
            for col in 0..g.cols {
                let (x, y, w, h) = g.cell_content_rect(row, col).expect("in range");
                assert!(x > g.border_x_at(col), "content starts left of its border");
                assert!(
                    x + w <= g.border_x_at(col + 1),
                    "cell ({row},{col}) content overruns its right border"
                );
                assert!(y > g.border_y_at(row));
                assert!(y + h <= g.border_y_at(row + 1));
            }
        }
    }

    #[test]
    fn a_cell_outside_the_table_has_no_rect() {
        let g = TableGrid::new(&span_4x3(), 0, 0, 8, 18, 18);
        assert!(
            g.cell_content_rect(4, 0).is_none(),
            "row 4 of a 4-row table"
        );
        assert!(
            g.cell_content_rect(0, 3).is_none(),
            "column 3 of a 3-column table"
        );
    }

    #[test]
    fn the_border_iterator_terminates_and_is_not_empty() {
        let g = TableGrid::new(&span_4x3(), 0, 0, 8, 18, 18);
        let runs: Vec<_> = g.borders().collect();
        assert!(!runs.is_empty());
        // The *exact* count: h * (2v - 1) horizontal items plus (h - 1) * v vertical ones, which for a
        // 4x3 table is 35 + 16 = 51. An upper bound would have accepted the truncated iterator that
        // stopped at 29, which is exactly the bug this count now prevents.
        let (h, v) = (5usize, 4usize);
        assert_eq!(
            runs.len(),
            h * (2 * v - 1) + (h - 1) * v,
            "wrong number of border runs"
        );
        // Every non-empty run has positive extent.
        for r in &runs {
            if r.len > 0 {
                assert!(r.width > 0 && r.height > 0, "empty run with len {r:?}");
            }
        }
    }

    #[test]
    fn a_table_too_wide_for_the_measure_is_refused_before_any_pixels_are_computed() {
        let wide = TableSpan::new(2, 8, 12, 0, 0).expect("shape is legal");
        // 8 columns of 12 = 121 cells against an 80-cell measure, at 8 px per cell.
        assert!(layout(&wide, 0, 0, 8, 18, 18, 80 * 8).is_err());
        assert!(
            layout(&span_4x3(), 0, 0, 8, 18, 18, 80 * 8).is_ok(),
            "22 cells fits 80"
        );
    }

    #[test]
    fn junctions_carry_both_arms_so_corners_meet() {
        // `┌` must have DOWN and RIGHT. Verified against the table rather than against a comment,
        // because the arms-vs-edges confusion in Phase 4 made every glyph in the block wrong while
        // still looking like something.
        // Bits per `box_drawing::edge`: UP = 1<<0, DOWN = 1<<1, LEFT = 1<<2, RIGHT = 1<<3. The
        // first version of this test used 0b0100 for DOWN and 0b0010 for RIGHT, which are LEFT and
        // DOWN -- so it asserted that ┌ reaches down by reading its LEFT bit, and failed on the
        // correct table. Constants transcribed from the wrong end are how a test invents a bug.
        const UP: u8 = 1 << 0;
        const DOWN: u8 = 1 << 1;
        const LEFT: u8 = 1 << 2;
        const RIGHT: u8 = 1 << 3;

        let (arms, _) = box_drawing::glyph_kind(0x250C).expect("┌ has arms");
        assert_ne!(arms & DOWN, 0, "┌ must reach down");
        assert_ne!(arms & RIGHT, 0, "┌ must reach right");
        assert_eq!(arms & UP, 0, "┌ must not reach up");
        assert_eq!(arms & LEFT, 0, "┌ must not reach left");

        // And the interior crossing, which is the one a table emits most often.
        let (cross, _) = box_drawing::glyph_kind(0x253C).expect("┼ has arms");
        assert_eq!(
            cross & (UP | DOWN | LEFT | RIGHT),
            UP | DOWN | LEFT | RIGHT,
            "┼ must reach all four ways or interior junctions leave gaps"
        );
    }

    #[test]
    fn a_one_by_one_table_still_produces_a_closed_box() {
        // The degenerate case, and the one most likely to produce a negative length somewhere.
        let one = TableSpan::new(1, 1, 6, 0, 0).expect("legal");
        let g = TableGrid::new(&one, 0, 0, 8, 18, 18);
        assert_eq!(g.width_px(), one.width_cells() * 8);
        assert!(!g.borders().collect::<Vec<_>>().is_empty());
        // Its four corners must be the four corner glyphs.
        let corners: Vec<u32> = [
            junction(false, true, false, true),
            junction(false, true, true, false),
            junction(true, false, false, true),
            junction(true, false, true, false),
        ]
        .to_vec();
        assert_eq!(corners, vec![0x250C, 0x2510, 0x2514, 0x2518]);
    }
}
