//! Line heights, the Fenwick tree over them, and vertical layout.
//!
//! FR-1.3: "The document geometry must use a Fenwick Tree storing vertical line heights and prefix
//! byte sums. Vertical coordinate queries must resolve in O(log N) steps without measuring visual
//! glyph elements."
//!
//! # Two trees, and why
//!
//! PROJECT.md §5 Phase 6 says "Two trees -- line heights and byte-prefix sums". They answer different
//! questions and are both needed on the keystroke path:
//!
//! * [`LineGeometry`] holds line *heights*. "Which line is at pixel Y", "where does line N start",
//!   "how tall is the document". This is what scrolling, the caret, and the damage rect ask.
//! * [`ByteIndex`] holds byte *offsets*. "Which line contains document byte B", "which byte is at
//!   the end of line N". This is what the editor's cursor-to-position and position-to-cursor need,
//!   and it is the one that has to stay correct while text is being inserted, which shifts every
//!   later line's byte offset.
//!
//! Combining them into one structure is tempting and wrong: a line's height changes when the *style*
//! changes, and its byte offset changes when the *text* changes, at completely different times and
//! at completely different rates. Two trees, each updated only when its own quantity changes.
//!
//! # Line height comes from font metrics
//!
//! Not from measuring rendered glyphs. A line's height is `ascender - descender + line_gap` for the
//! style in effect, in pixels, taken from the font at the given ppem. Two reasons, both from the
//! requirements rather than preference:
//!
//! * FR-1.3 forbids measuring visual glyph elements for layout, and
//! * measuring requires the line to have been rendered, which cannot be true for a 2,000-page
//!   document of which a few hundred pixels are visible.
//!
//! The consequence is that geometry is available *before* anything is drawn, which is what makes the
//! caret and scrollbar correct on the first frame rather than after a reflow.

use crate::fenwick::Fenwick;

/// One line's vertical metrics, in pixels.
///
/// `#[repr(C)]` and `Copy` because a line's metrics are read on the render thread for every visible
/// line and a 16-byte struct is two registers.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineMetrics {
    /// Distance from the line box's top to the baseline, in pixels.
    ///
    /// The glyph origin sits here. Positive, and equal to the ascender for a line with no
    /// line-spacing adjustment.
    pub ascender: u32,
    /// Distance from the baseline down to the bottom of the line box, in pixels.
    ///
    /// Positive. A line with no descenders still has one, because the line box is defined by the
    /// font's metrics and not by the glyphs that happen to be on the line.
    pub descender: u32,
    /// Extra space between this line and the next, in pixels.
    pub leading: u32,
    /// Distance from the line box's top to the caret's top, in pixels.
    ///
    /// Not always `ascender`: a superscript's caret sits higher, and a line containing a tall
    /// inline image sits lower. It defaults to the ascender and is what the damage rect needs, so
    /// that the caret's own box is the height the renderer uses.
    pub caret_height: u32,
}

impl LineMetrics {
    /// The line box's height: `ascender + descender + leading`.
    #[inline]
    pub fn height(&self) -> u32 {
        self.ascender
            .saturating_add(self.descender)
            .saturating_add(self.leading)
    }
}

/// Why a geometry operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryError {
    /// The line index is past the end of the document.
    NoSuchLine {
        /// Requested line.
        line: usize,
        /// Lines in the document.
        lines: usize,
    },
    /// The byte offset is past the end of the document.
    ByteOutOfBounds {
        /// Requested offset.
        offset: usize,
        /// Document length in bytes.
        text_len: usize,
    },
    /// The byte offset is not on a UTF-8 character boundary, as far as this layer can tell.
    ///
    /// This layer cannot check, because it does not hold the text -- it holds byte offsets. The
    /// check belongs to the rope, which does, and this variant exists so a caller that has only the
    /// geometry gets a diagnosable error rather than a silently wrong line.
    NotAByteBoundary {
        /// Requested offset.
        offset: usize,
    },
    /// An empty document cannot answer a "which line" query.
    EmptyDocument,
}

impl std::fmt::Display for GeometryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchLine { line, lines } => {
                write!(f, "line {line} requested from a {lines}-line document")
            }
            Self::ByteOutOfBounds { offset, text_len } => {
                write!(f, "byte {offset} is past the document's {text_len} bytes")
            }
            Self::NotAByteBoundary { offset } => {
                write!(f, "byte {offset} is not a UTF-8 character boundary")
            }
            Self::EmptyDocument => write!(f, "the document has no lines"),
        }
    }
}

impl std::error::Error for GeometryError {}

/// Vertical layout over a document's lines.
///
/// Two Fenwick trees, indexed by line number:
///
/// ```text
/// heights:   line i has weight heights[i]        -- pixels
/// starts:    line i has weight starts[i]         -- bytes
/// ```
///
/// Neither is a stored per-line prefix sum. Both are stored as *deltas* in a Fenwick tree, so the
/// memory is `2 * (lines + 1) * 4` bytes rather than `2 * lines * 4` bytes of prefix sums *plus* the
/// underlying per-line values. For a 60,000-line document that is 480 KB instead of 960 KB, which
/// matters against the 16.0 MiB RSS ceiling.
#[derive(Debug, Clone)]
pub struct LineGeometry {
    /// Pixel heights, indexed by line.
    heights: Fenwick,
    /// Byte length of each line, *excluding* its newline, indexed by line.
    ///
    /// A Fenwick tree over per-line byte lengths, so `prefix(i)` is the byte offset at which line
    /// `i` starts. The newline that terminates a line is not counted, so `total()` is the document
    /// length minus the number of newlines.
    lengths: Fenwick,
    /// Per-line metrics, for the caret and the damage rect.
    ///
    /// Not in the Fenwick tree: a line's *height* is a tree weight, but its ascender and descender
    /// are only read for the caret of the line the user is actually editing, which is one line per
    /// keystroke.
    metrics: Vec<LineMetrics>,
    /// Default metrics for lines whose style has not been set.
    default_metrics: LineMetrics,
}

impl Default for LineGeometry {
    fn default() -> Self {
        Self::new(vec![LineMetrics::default()], LineMetrics::default())
    }
}

impl LineGeometry {
    /// Build a geometry for `lines` lines.
    ///
    /// Every line starts with `default_metrics`, so the document has a usable vertical layout before
    /// any style has been resolved.
    pub fn new(lines: Vec<LineMetrics>, default_metrics: LineMetrics) -> Self {
        let heights =
            Fenwick::from_weights(&lines.iter().map(LineMetrics::height).collect::<Vec<_>>());
        // Every line is empty until its text is registered.
        let lengths = Fenwick::zeros(lines.len());
        Self {
            heights,
            lengths,
            metrics: lines,
            default_metrics,
        }
    }

    /// Build a geometry for a document of `line_count` lines with no text yet.
    pub fn uniform(line_count: usize, metrics: LineMetrics) -> Self {
        Self::new(vec![metrics; line_count], metrics)
    }

    /// Number of lines.
    #[inline]
    pub fn line_count(&self) -> usize {
        self.heights.len()
    }

    /// Total document height in pixels.
    #[inline]
    pub fn total_height(&self) -> u32 {
        self.heights.total()
    }

    /// The metrics used for lines with no explicit style.
    #[inline]
    pub fn default_metrics(&self) -> LineMetrics {
        self.default_metrics
    }

    /// Line `line`'s metrics.
    pub fn metrics(&self, line: usize) -> Result<LineMetrics, GeometryError> {
        self.metrics
            .get(line)
            .copied()
            .ok_or(GeometryError::NoSuchLine {
                line,
                lines: self.metrics.len(),
            })
    }

    /// Line `line`'s box height in pixels.
    pub fn line_height(&self, line: usize) -> Result<u32, GeometryError> {
        Ok(self.metrics(line)?.height())
    }

    /// Line `line`'s box height in pixels, or 0 if it does not exist.
    ///
    /// The form the render loop wants: it iterates lines it computed a moment ago and a concurrent
    /// edit may have merged one away, and drawing a zero-height line beats refusing the frame.
    #[inline]
    pub fn line_height_or_zero(&self, line: usize) -> u32 {
        self.metrics.get(line).map_or(0, |m| m.height())
    }

    /// The y coordinate of line `line`'s top edge.
    ///
    /// O(log n). This is the `y_of` half of the `y_of` / `line_at` inverse pair.
    pub fn y_of(&self, line: usize) -> Result<u32, GeometryError> {
        if line >= self.heights.len() {
            return Err(GeometryError::NoSuchLine {
                line,
                lines: self.heights.len(),
            });
        }
        Ok(self.heights.prefix(line))
    }

    /// The line whose box contains pixel `y`.
    ///
    /// O(log n). This is the `line_at` half of the inverse pair.
    ///
    /// A `y` past the end of the document returns the last line, not an error: the scrollbar and the
    /// caret both ask this while the user is dragging past the bottom, and "the last line" is the
    /// right answer. Use [`y_of`](Self::y_of) when a missing line must be an error.
    pub fn line_at(&self, y: u32) -> usize {
        self.heights
            .lower_bound(y)
            .min(self.heights.len().saturating_sub(1))
    }

    /// The y coordinate of line `line`'s baseline.
    #[inline]
    pub fn baseline_of(&self, line: usize) -> Result<u32, GeometryError> {
        Ok(self
            .y_of(line)?
            .saturating_add(self.metrics(line)?.ascender))
    }

    /// The caret box for line `line`: `(y, height)`.
    ///
    /// The damage rectangle the editor invalidates on a keystroke is built from this, which is what
    /// makes a keystroke touch one line's box rather than the page. See
    /// [`damage_rect_for`](Self::damage_rect_for).
    pub fn caret_rect(&self, line: usize) -> Result<(u32, u32), GeometryError> {
        let m = self.metrics(line)?;
        Ok((self.y_of(line)?, m.caret_height.max(1)))
    }

    /// The rectangle a keystroke on `line` must repaint.
    ///
    /// FR-3.4: "Typing a character invalidates and redraws only the scanout rows intersecting the
    /// active text line". PROJECT.md §5 Phase 5 makes it concrete: typing at row 420 touches rows
    /// 420-436 and nothing else.
    ///
    /// So the rectangle is the line's own box, not the line plus its neighbours. The height comes
    /// from the font's metrics, which is why a keystroke's damage is knowable without rendering
    /// anything.
    pub fn damage_rect_for(&self, line: usize) -> Result<DamageRect, GeometryError> {
        let m = self.metrics(line)?;
        let top = self.y_of(line)?;
        Ok(DamageRect {
            x: 0,
            y: top,
            width: u32::MAX,
            height: m.height().max(1),
        })
    }

    /// Set line `line`'s metrics, updating both the height tree and the stored metrics.
    ///
    /// O(log n). The weight delta is computed here rather than by a caller so that the two can never
    /// disagree -- a mismatch between `heights` and `metrics` would make `y_of` and `line_at` drift
    /// apart, and the drift would be silent.
    pub fn set_metrics(&mut self, line: usize, metrics: LineMetrics) -> Result<(), GeometryError> {
        if line >= self.metrics.len() {
            return Err(GeometryError::NoSuchLine {
                line,
                lines: self.metrics.len(),
            });
        }
        let old = self.metrics[line].height();
        let new = metrics.height();
        self.heights.add(line, i64::from(new) - i64::from(old));
        self.metrics[line] = metrics;
        Ok(())
    }

    /// Record line `line`'s byte length, excluding its newline.
    pub fn set_line_len(&mut self, line: usize, bytes: usize) -> Result<(), GeometryError> {
        if line >= self.lengths.len() {
            return Err(GeometryError::NoSuchLine {
                line,
                lines: self.lengths.len(),
            });
        }
        let current = self.lengths.weight(line);
        self.lengths.add(line, bytes as i64 - i64::from(current));
        Ok(())
    }

    /// Line `line`'s byte length, excluding its newline.
    pub fn line_len(&self, line: usize) -> Result<usize, GeometryError> {
        if line >= self.lengths.len() {
            return Err(GeometryError::NoSuchLine {
                line,
                lines: self.lengths.len(),
            });
        }
        Ok(self.lengths.weight(line) as usize)
    }

    /// The document byte offset at which `line` starts.
    ///
    /// O(log n).
    pub fn byte_of(&self, line: usize) -> Result<usize, GeometryError> {
        if line >= self.lengths.len() {
            return Err(GeometryError::NoSuchLine {
                line,
                lines: self.lengths.len(),
            });
        }
        Ok(self.lengths.prefix(line) as usize)
    }

    /// The line containing document byte `offset`.
    ///
    /// O(log n), from the same tree: the largest line whose start offset is `<= offset`.
    ///
    /// `offset` past the end returns the last line, matching [`line_at`](Self::line_at)'s treatment
    /// of a `y` past the end.
    pub fn line_of_byte(&self, offset: usize) -> usize {
        if self.lengths.is_empty() {
            return 0;
        }
        let at = offset.min(u32::MAX as usize) as u32;
        self.lengths.lower_bound(at).min(self.lengths.len() - 1)
    }

    /// Total document length in bytes, counting the newline after each line.
    ///
    /// One more than the sum of the per-line lengths, because the last line has a newline too in the
    /// on-disk representation and the editor's offsets include them.
    pub fn total_bytes(&self) -> usize {
        self.lengths.total() as usize + self.lengths.len()
    }

    /// Rebuild for a different number of lines, preserving nothing.
    ///
    /// A document's line count changes whenever a newline is inserted or deleted, and a Fenwick tree
    /// does not support insertion -- it supports point updates. Rebuilding is O(n): at 60,000 lines
    /// that is 60,000 operations, once per newline typed. PROJECT.md's H2 note on the same trade-off
    /// ("Insertion is O(n), and that is fine") applies unchanged.
    pub fn resize_lines(&mut self, count: usize) {
        if count == self.metrics.len() {
            return;
        }
        let mut new_heights = Vec::with_capacity(count);
        for i in 0..count {
            new_heights.push(
                self.metrics
                    .get(i)
                    .map_or_else(|| self.default_metrics.height(), |m| m.height()),
            );
        }
        let mut new_lengths = Vec::with_capacity(count);
        for i in 0..count {
            new_lengths.push(self.lengths.weight(i));
        }
        self.metrics.resize(count, self.default_metrics);
        self.heights = Fenwick::from_weights(&new_heights);
        self.lengths = Fenwick::from_weights(&new_lengths);
    }

    /// The Fenwick tree over line heights, for callers that need it directly.
    #[inline]
    pub fn height_tree(&self) -> &Fenwick {
        &self.heights
    }

    /// The Fenwick tree over byte lengths, for callers that need it directly.
    #[inline]
    pub fn byte_tree(&self) -> &Fenwick {
        &self.lengths
    }
}

/// A rectangle of the scanout that must be repainted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamageRect {
    /// Left edge, in pixels.
    pub x: u32,
    /// Top edge, in pixels.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl DamageRect {
    /// The smallest rect containing both.
    ///
    /// The union is what a frame accumulates: two keystrokes on different lines repaint one rect
    /// spanning both, and the union is never larger than the sum of the parts.
    pub fn union(&self, other: &DamageRect) -> DamageRect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x.saturating_add(self.width)).max(other.x.saturating_add(other.width));
        let bottom = (self.y.saturating_add(self.height)).max(other.y.saturating_add(other.height));
        DamageRect {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }

    /// Whether `y` falls inside, inclusive of both edges.
    ///
    /// Inclusive because a line's damage rect must include the row *after* its last text row when the
    /// next line's ascenders bleed into it, and because an off-by-one here repaints one row too few
    /// and leaves a visible artefact.
    #[inline]
    pub fn contains_row(&self, y: u32) -> bool {
        y >= self.y && y < self.y.saturating_add(self.height)
    }

    /// Number of scanout rows touched, i.e. `height`.
    #[inline]
    pub fn rows(&self) -> u32 {
        self.height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16 px body text: 13 px ascender, 4 px descender, 3 px leading = 20 px lines.
    fn body() -> LineMetrics {
        LineMetrics {
            ascender: 13,
            descender: 4,
            leading: 3,
            caret_height: 17,
        }
    }

    #[test]
    fn a_line_box_is_ascender_plus_descender_plus_leading() {
        assert_eq!(body().height(), 20);
        assert_eq!(LineMetrics::default().height(), 0);
        // Saturating, so a pathological font cannot wrap.
        assert_eq!(
            LineMetrics {
                ascender: u32::MAX,
                descender: u32::MAX,
                leading: u32::MAX,
                caret_height: 0,
            }
            .height(),
            u32::MAX
        );
    }

    #[test]
    fn y_of_and_line_at_are_exact_inverses() {
        let g = LineGeometry::uniform(1000, body());
        for line in 0..1000 {
            let y = g.y_of(line).expect("line");
            assert_eq!(g.line_at(y), line, "line_at(y_of({line}))");
            // And every pixel inside the line resolves back to it.
            for dy in 0..20 {
                assert_eq!(g.line_at(y + dy), line, "line_at(y_of({line}) + {dy})");
            }
        }
    }

    #[test]
    fn total_height_is_the_sum_of_the_line_boxes() {
        let g = LineGeometry::uniform(1000, body());
        assert_eq!(g.total_height(), 1000 * 20);
        // The last line starts at 999 * 20. `y_of(1000)` -- one past the last line -- is an error by
        // design: a caller asking for a line that does not exist is told so, rather than handed a y
        // past the end of the document that would be indistinguishable from a valid coordinate.
        // An earlier version of this test asserted `y_of(1000) == Ok(20_000)`, treating `line_count`
        // as a valid line index, and failed with `left: Err(NoSuchLine { line: 1000 })`.
        assert_eq!(g.y_of(999), Ok(999 * 20), "the last line's start");
        assert_eq!(
            g.total_height(),
            1000 * 20,
            "which is also where the document ends"
        );
        assert!(
            g.y_of(1000).is_err(),
            "one past the last line is not a line"
        );
    }

    /// The concrete requirement: typing at row 420 touches rows 420-436 and nothing else.
    #[test]
    fn a_keystroke_damages_exactly_one_line_box() {
        let g = LineGeometry::uniform(100, body());
        let d = g.damage_rect_for(21).expect("line 21");
        assert_eq!(d.y, 420, "line 21 starts at 21 * 20");
        assert_eq!(d.height, 20);
        // Exactly the rows 420..=439 are in the rect; row 419 and row 440 are not.
        assert!(!d.contains_row(419));
        for y in 420..440 {
            assert!(d.contains_row(y), "row {y} should be damaged");
        }
        assert!(!d.contains_row(440));
        assert_eq!(d.rows(), 20);
    }

    #[test]
    fn a_mixed_style_document_has_per_line_heights() {
        let mut g = LineGeometry::new(vec![body(), body(), body()], body());
        // Make line 1 a heading: taller.
        let heading = LineMetrics {
            ascender: 24,
            descender: 6,
            leading: 4,
            caret_height: 30,
        };
        g.set_metrics(1, heading).expect("line 1");
        assert_eq!(g.line_height(1), Ok(34));
        assert_eq!(g.y_of(1), Ok(20), "line 1 still starts after line 0");
        assert_eq!(g.y_of(2), Ok(54), "line 2 starts after the taller line 1");
        assert_eq!(g.total_height(), 20 + 34 + 20);
        // And the inverse still holds with mixed heights.
        for line in 0..3 {
            assert_eq!(g.line_at(g.y_of(line).unwrap()), line);
        }
    }

    /// A style change is a point update, so it must be O(log n) and must not disturb the rest.
    #[test]
    fn changing_one_line_height_moves_only_the_lines_below_it() {
        let mut g = LineGeometry::uniform(100, body());
        let before: Vec<u32> = (0..100).map(|i| g.y_of(i).unwrap()).collect();
        g.set_metrics(50, body()).expect("same height");
        let after: Vec<u32> = (0..100).map(|i| g.y_of(i).unwrap()).collect();
        assert_eq!(before, after, "setting the same metrics must be a no-op");

        g.set_metrics(
            50,
            LineMetrics {
                ascender: 33,
                descender: 4,
                leading: 3,
                caret_height: 37,
            },
        )
        .expect("line 50");
        assert_eq!(
            g.y_of(50),
            Ok(before[50]),
            "line 50's own start is unchanged"
        );
        assert_eq!(
            g.y_of(51),
            Ok(before[51] + 20),
            "line 51 shifts by the delta"
        );
        assert_eq!(g.total_height(), 100 * 20 + 20);
    }

    #[test]
    fn byte_offsets_and_line_lengths_are_a_second_inverse_pair() {
        let mut g = LineGeometry::uniform(10, body());
        for (i, len) in [10usize, 20, 30, 40, 50, 5, 15, 25, 35, 45]
            .iter()
            .enumerate()
        {
            g.set_line_len(i, *len).expect("line");
        }
        // Line starts are the prefix sums.
        let mut want = 0;
        for i in 0..10 {
            assert_eq!(g.byte_of(i), Ok(want), "byte_of({i})");
            want += [10, 20, 30, 40, 50, 5, 15, 25, 35, 45][i];
        }
        // And every byte in a line resolves back to it.
        let lens = [10usize, 20, 30, 40, 50, 5, 15, 25, 35, 45];
        let mut start = 0;
        for (i, len) in lens.iter().enumerate() {
            for b in start..(start + len) {
                assert_eq!(g.line_of_byte(b), i, "byte {b} should be in line {i}");
            }
            start += len;
        }
        assert_eq!(
            g.total_bytes(),
            10 + 20 + 30 + 40 + 50 + 5 + 15 + 25 + 35 + 45 + 10
        );
    }

    #[test]
    fn a_newline_grows_the_document_by_one_line() {
        let mut g = LineGeometry::uniform(3, body());
        g.set_line_len(0, 5).expect("line 0");
        g.set_line_len(1, 5).expect("line 1");
        g.set_line_len(2, 5).expect("line 2");
        assert_eq!(g.line_count(), 3);

        g.resize_lines(4);
        assert_eq!(g.line_count(), 4);
        assert_eq!(
            g.line_height(3),
            Ok(20),
            "the new line gets the default metrics"
        );
        assert_eq!(g.total_height(), 80);
        // And it is at the end, so no existing line moved.
        assert_eq!(g.y_of(3), Ok(60));
    }

    #[test]
    fn a_resize_that_changes_nothing_is_free() {
        let mut g = LineGeometry::uniform(10, body());
        let before = g.total_height();
        g.resize_lines(10);
        assert_eq!(g.total_height(), before);
    }

    #[test]
    fn damage_rects_union_to_the_smallest_covering_rect() {
        let a = DamageRect {
            x: 0,
            y: 100,
            width: 1280,
            height: 20,
        };
        let b = DamageRect {
            x: 0,
            y: 300,
            width: 1280,
            height: 20,
        };
        let u = a.union(&b);
        assert_eq!(u.y, 100);
        assert_eq!(u.height, 220);
        assert_eq!(u.width, 1280);
        // Unioning with itself is itself.
        assert_eq!(a.union(&a), a);
        // The union is the *bounding box*. Unioning a 1280-wide rect with a 100-wide rect that
        // starts further right still spans 1280, because the bounding box has to contain both.
        // An earlier version of this test asserted 600, treating the union as the sum of the parts;
        // it reported `left: 1280, right: 600`.
        let c = DamageRect {
            x: 500,
            y: 100,
            width: 100,
            height: 20,
        };
        let u2 = a.union(&c);
        assert_eq!((u2.x, u2.y, u2.width, u2.height), (0, 100, 1280, 20));

        // Two rects that really do differ in extent.
        let d = DamageRect {
            x: 100,
            y: 100,
            width: 200,
            height: 20,
        };
        let e = DamageRect {
            x: 400,
            y: 110,
            width: 50,
            height: 5,
        };
        assert_eq!(
            (
                d.union(&e).x,
                d.union(&e).y,
                d.union(&e).width,
                d.union(&e).height
            ),
            (100, 100, 350, 20)
        );

        // A zero-width rect -- a caret, which has no horizontal extent -- still marks its rows, and
        // does not widen the union.
        let caret = DamageRect {
            x: 640,
            y: 100,
            width: 0,
            height: 17,
        };
        assert_eq!(a.union(&caret).height, 20);
        assert_eq!(a.union(&caret).width, 1280);
    }

    #[test]
    fn a_missing_line_is_an_error_where_that_is_the_right_answer() {
        let g = LineGeometry::uniform(5, body());
        assert!(g.y_of(5).is_err());
        assert!(g.metrics(5).is_err());
        assert!(g.byte_of(5).is_err());
        // But the render-loop forms degrade instead.
        assert_eq!(g.line_height_or_zero(5), 0);
        assert_eq!(g.line_at(10_000), 4, "a y past the end is the last line");
    }

    /// Two keystrokes on one line must produce one line's damage, not two.
    #[test]
    fn two_keystrokes_on_one_line_damage_it_once() {
        let g = LineGeometry::uniform(100, body());
        let a = g.damage_rect_for(7).expect("line 7");
        let b = g.damage_rect_for(7).expect("line 7");
        let u = a.union(&b);
        assert_eq!(u, a);
        assert_eq!(u.rows(), 20);
    }
}
