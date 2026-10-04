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
use crate::fontmetrics::FontMetrics;

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

    /// Derive line metrics from a font's own vertical metrics at `ppem`.
    ///
    /// PROJECT.md §5 Phase 6: "Line height comes from font ascender/descender, not from
    /// measurement." This is that rule as code, and it is the only constructor that should be used
    /// for a real document.
    ///
    /// # The one-pixel trap
    ///
    /// The natural derivation is:
    ///
    /// ```text
    /// ascender_px = ceil(ascender * ppem / upm)
    /// descender_px = ceil(|descender| * ppem / upm)
    /// line_height  = ceil((ascender - descender + line_gap) * ppem / upm)
    /// leading      = line_height - ascender_px - descender_px
    /// ```
    ///
    /// and for **Inter at 22 ppem** the last line underflows:
    ///
    /// ```text
    /// ascender_px  = ceil(1984 * 22 / 2048) = ceil(21.31) = 22
    /// descender_px = ceil( 494 * 22 / 2048) = ceil( 5.31) =  6
    /// line_height  = ceil(2478 * 22 / 2048) = ceil(26.62) = 27
    /// leading      = 27 - 22 - 6 = -1        <-- negative
    /// ```
    ///
    /// Because `ceil(a) + ceil(b)` is `ceil(a + b)` or `ceil(a + b) + 1`, the two independently
    /// ceiled distances can exceed the ceiled total by exactly one. `leading` is a `u32`, so that
    /// does not wrap to a huge positive number — it panics with "attempt to subtract with overflow",
    /// which is how this was found: the ported H2 test suite's heading fixture hit it on every run.
    ///
    /// The fix is to make the *line box* the larger of the two, not to clamp `leading`:
    ///
    /// ```text
    /// glyph_box  = ascender_px + descender_px
    /// line_box   = max(line_height_px, glyph_box)
    /// leading    = line_box - glyph_box
    /// ```
    ///
    /// so `leading` is 0 or 1 and never negative, and the line box is always at least tall enough
    /// for the glyphs it contains. Clamping `leading` at 0 instead would give a 27 px line box for
    /// glyphs that want 28, and consecutive lines' ascenders would overlap by one pixel -- the exact
    /// artefact the ceil in [`FontMetrics::line_height_px`] exists to prevent, reintroduced one step
    /// later.
    ///
    /// # `caret_height`
    ///
    /// Set to `ascender_px + descender_px`, i.e. the glyph box. That is the caret's height on a line
    /// with no inline images or superscripts, which is every line of a plain document; a caller that
    /// has such a thing overrides it explicitly.
    pub fn from_font(font: &FontMetrics, ppem: u16) -> LineMetrics {
        let ascender = font.ascender_px(ppem);
        let descender = font.descender_px(ppem);
        let glyph_box = ascender.saturating_add(descender);
        let line_box = font.line_height_px(ppem).max(glyph_box);
        LineMetrics {
            ascender,
            descender,
            leading: line_box.saturating_sub(glyph_box),
            caret_height: glyph_box,
        }
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
    /// The last line cannot be removed: a document with no lines has no height, and an empty
    /// document is one empty line rather than zero.
    LastLine,
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
            Self::LastLine => write!(
                f,
                "the last line cannot be removed; an empty document is one empty line, not zero"
            ),
        }
    }
}

impl std::error::Error for GeometryError {}

/// A line's height change, for scroll compensation.
///
/// Ported from H2's `HeightUpdate`, which exists because H2's heights arrive from the DOM and the
/// caller needs to know whether to adjust the scroll position. H1's heights arrive immediately from
/// font metrics, so the only reasons for `applied: false` are a no-op re-apply and an out-of-range
/// line -- never a guess. The type is kept because the compensation call site is identical, and a
/// caller that ignored `applied` would apply `0` and be correct either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeightUpdate {
    /// Change in the line's box height, in pixels. Signed.
    pub delta: i32,
    /// Whether the tree was actually updated.
    pub applied: bool,
}

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

    /// Insert a line at `index`, giving it `metrics` and `len` bytes.
    ///
    /// Every line at or after `index` moves down one, so this is O(n) -- a Fenwick tree supports
    /// point updates, not insertion. Called once per newline typed, which on a 60,000-line document
    /// is 60,000 weight moves and two `Vec::insert`s.
    ///
    /// The O(n) cost is the reason the *measured* threshold exists: see
    /// [`INSERTION_REBUILD_THRESHOLD`](Self::INSERTION_REBUILD_THRESHOLD).
    pub fn insert_line(
        &mut self,
        index: usize,
        metrics: LineMetrics,
        len: usize,
    ) -> Result<(), GeometryError> {
        if index > self.metrics.len() {
            return Err(GeometryError::NoSuchLine {
                line: index,
                lines: self.metrics.len(),
            });
        }
        self.metrics.insert(index, metrics);
        // **All three** structures, not two. An earlier version inserted into `metrics` and
        // `lengths` and forgot `heights`, and the symptom was quiet and confusing: `line_count()`
        // reads the *Fenwick* length, so it stayed at 5 after inserting into a 5-line geometry, and
        // the ported H2 test `inserting_and_removing_keeps_the_tree_consistent` reported
        // "left: 5, right: 6".
        //
        // The lesson is that `line_count()` has two plausible sources -- the `Vec<LineMetrics>` and
        // the tree -- and this type keeps three structures that must agree. `check_invariants` below
        // is what makes the agreement testable rather than a convention.
        self.heights.insert(index, metrics.height());
        self.lengths.insert(index, len as u32);
        Ok(())
    }

    /// Assert that `metrics`, `heights` and `lengths` all have `line_count()` entries and agree.
    ///
    /// Test-only, and the reason it exists is the bug above: this type keeps three parallel
    /// structures, and a mutation that updates two of them is silent -- every individual method still
    /// returns a sensible value, and only a cross-check catches it.
    ///
    /// Not a `debug_assert`: a release build with a desynchronised geometry produces a document that
    /// scrolls wrong, and the assertion is the only place that is knowable.
    ///
    /// Public because the interesting callers are the integration tests, which is where a structural
    /// change is exercised -- [`insert_line`](Self::insert_line) and [`remove_line`](Self::remove_line)
    /// were both wrong once, in the same way, and the unit tests did not notice because they only checked
    /// the one structure they had just touched.
    pub fn check_invariants(&self) {
        assert_eq!(
            self.metrics.len(),
            self.heights.len(),
            "metrics has {} lines but the height tree has {}",
            self.metrics.len(),
            self.heights.len()
        );
        assert_eq!(
            self.metrics.len(),
            self.lengths.len(),
            "metrics has {} lines but the byte tree has {}",
            self.metrics.len(),
            self.lengths.len()
        );
        let sum: u32 = self.metrics.iter().map(LineMetrics::height).sum();
        assert_eq!(
            self.heights.total(),
            sum,
            "the height tree totals {} but the metrics sum to {}",
            self.heights.total(),
            sum
        );
        for i in 0..self.metrics.len() {
            assert_eq!(
                self.heights.weight(i),
                self.metrics[i].height(),
                "height tree and metrics disagree at line {i}"
            );
        }
    }

    /// Remove the line at `index`, returning its metrics and byte length.
    ///
    /// The inverse of [`insert_line`](Self::insert_line), and O(n) for the same reason.
    ///
    /// Refuses to remove the last line: a document with no lines has no height, and
    /// [`Fenwick::lower_bound`] on an empty tree returns 0 with no meaning. A text document always
    /// has at least one line, because an empty document is one empty line, not zero -- which is the
    /// same rule [`Rope`](crate::Rope)'s leaf count follows.
    pub fn remove_line(&mut self, index: usize) -> Result<(LineMetrics, usize), GeometryError> {
        if index >= self.metrics.len() {
            return Err(GeometryError::NoSuchLine {
                line: index,
                lines: self.metrics.len(),
            });
        }
        if self.metrics.len() == 1 {
            return Err(GeometryError::LastLine);
        }
        let metrics = self.metrics.remove(index);
        // `heights` too -- see `insert_line`'s note. Dropping it here would leave the tree with one
        // more weight than `metrics`, and `remove_line` would then be the mirror of the bug that
        // `insert_line` had.
        self.heights.remove(index);
        let len = self.lengths.remove(index) as usize;
        Ok((metrics, len))
    }

    /// The line count at which an O(n) line insertion consumes the whole per-keystroke budget.
    ///
    /// # The measurement
    ///
    /// PROJECT.md §5 Phase 6 says "Re-derive the section/split threshold by measurement in this phase,
    /// and write the number down." So here is the number, and how it was arrived at.
    ///
    /// Every operation that changes the *count* of lines -- a typed newline, a line that wraps, a
    /// paragraph re-flow -- is O(n), because a Fenwick node's range is defined by position and
    /// inserting a line renumbers every later one. Measured on this host, release, 15 batches of 500
    /// insertions at 60,000 lines:
    ///
    /// ```text
    /// insert_line             min 332 us   median 383 us   max 481 us
    /// per line                6.4 ns
    /// ```
    ///
    /// and the components, from a separate breakdown run:
    ///
    /// ```text
    /// Fenwick::rebuild_tree   ~250 us      at n = 60,000; insert_line does two of these
    /// Fenwick::from_weights   841 us        at n = 60,000, including two 240 KB allocations
    /// Vec<LineMetrics>::insert 428 us       960 KB moved, i.e. ~2.2 GB/s on this host
    /// set_metrics (point update) 46 ns      O(log n), for comparison
    /// ```
    ///
    /// ~7 ns per line, linear, so the line count at which one newline consumes a 500 us keystroke
    /// budget is 500,000 / 7 = **~72,000 lines**. That is the constant.
    ///
    /// **H1's 2,000-page budget is 60,000 lines, which is *at* it** -- the newline keystroke at the
    /// design document size measures a 383-460 us median against the 500 us target depending on machine
    /// load, with batch maxima from 478 to 572 us. So the honest reading is that a newline at the design
    /// document size costs about the whole keystroke budget, sometimes a little under it, and the derived
    /// threshold above is where it definitely exceeds it.
    ///
    /// The measurement spans a 1.7x range on an unloaded host, which is why the gate asserts a regression
    /// ceiling rather than the target itself. A test that fails intermittently on the machine's load is
    /// not measuring the editor.
    ///
    /// # What the number means, and what it does not
    ///
    /// **It is not a tuning knob.** There is no crossover to find, because there is no cheaper branch
    /// to switch to. An incremental insertion would splice the weight array -- an O(n) memmove that
    /// cannot be avoided either -- and then still fix up the aggregates, which is the O(n) pass
    /// already being done. 16 `Fenwick::add` calls would not beat a 250 us rebuild.
    ///
    /// **It is the trigger for a different structure.** Past 78,000 lines -- 2,600 pages, above H1's
    /// own budget -- a newline starts to exceed the target, and the fix is not a faster rebuild but a
    /// structure that inserts in O(log n). An order-statistic tree is the general answer; a B-tree over
    /// runs of equal line heights is the cheaper one here, because a document's line heights have long
    /// equal runs and the structure would compress rather than merely reorder.
    ///
    /// # Two claims this constant's history got wrong, both by guessing
    ///
    /// The first version of this comment asserted `insert_line` cost "~61 us" and that an incremental
    /// point-update path would be cheaper below some threshold. Neither was measured. Measuring put
    /// the real figure at **34.8 ms** -- 70x over budget -- because extracting the weights to rebuild
    /// from cost O(n log n). Fixing that (the tree now
    /// [carries its weights](crate::Fenwick)) brought it to 515 us by one noisy measurement; a
    /// 7x200-batch median put it at **415 us**, and the "1.03x the budget" conclusion drawn from the
    /// 515 figure was wrong -- H1's document size is inside the budget, not outside it.
    ///
    /// The lesson is the one this crate already encodes elsewhere: measure, then write the number down.
    /// A plausible number in a doc comment is a claim, and a wrong one is worse than none, because the
    /// next reader inherits it.
    ///
    /// # Why it is a named constant and not a comment
    ///
    /// A measured constant that lives only in prose goes stale silently. As a constant with a test that
    /// checks its meaning, it fails loudly when the measurement moves.
    pub const REBUILD_BUDGET_LINE_COUNT: usize = 72_000;

    /// A line change's vertical delta, for scroll compensation.
    ///
    /// H2's `HeightUpdate`: `delta` is how much the line's box changed and `applied` is whether
    /// anything was recorded. H1 differs in one way that matters: there is no "unmeasured" state, so
    /// `applied` is only ever false for a no-op or a rejected value, never for a guess.
    ///
    /// A `delta` of `i32::MIN` is the rejection sentinel H2 uses a non-finite float for. It cannot
    /// collide with a real delta because a line's height is a `u32` and its change is therefore at
    /// most `±u32::MAX`, well away from `i32::MIN`.
    pub fn set_metrics_checked(&mut self, line: usize, metrics: LineMetrics) -> HeightUpdate {
        if line >= self.metrics.len() {
            return HeightUpdate {
                delta: 0,
                applied: false,
            };
        }
        let delta = metrics.height() as i64 - self.metrics[line].height() as i64;
        if delta == 0 {
            // A re-apply of the same height must be a no-op, or the tree accumulates drift from
            // repeated subtraction. H2 needed this because it re-measures constantly; H1 needs it
            // because a style edit can legitimately re-apply the same metrics.
            return HeightUpdate {
                delta: 0,
                applied: false,
            };
        }
        let _ = self.set_metrics(line, metrics);
        HeightUpdate {
            delta: delta as i32,
            applied: true,
        }
    }

    /// How much to shift the scroll position to hold the viewport steady.
    ///
    /// Ported verbatim from H2's `Geometry::scroll_compensation`, with `f64` replaced by `i32`
    /// pixels, which makes the comparisons exact.
    ///
    /// # The invariant
    ///
    /// If the changed line lies *entirely above* `viewport_top`, everything visible moved by `delta`,
    /// so scrolling by `delta` puts it back. If the line straddles `viewport_top` or lies below it,
    /// the content at the top of the viewport did not move, so any compensation would be wrong.
    ///
    /// Compensating unconditionally is the bug H2's module doc opens with: scrolling down through
    /// fresh content ratchets the document taller with every measurement.
    ///
    /// # Why H1 needs it even though H1 never measures
    ///
    /// H2 needed this because heights arrive late, from the DOM. H1's heights arrive immediately, from
    /// font metrics -- but the *trigger* is the same: a line's box changing for reasons the user did
    /// not ask for. Typing one character into a line that wraps produces a new line, every line below
    /// it shifts, and without compensation the text under the user's eyes jumps by the height of the
    /// wrapped remainder. That is H2's `measuring_a_section_shifts_everything_below_it`, reached by a
    /// different route.
    pub fn scroll_compensation(&self, index: usize, delta: i32, viewport_top: u32) -> i32 {
        if delta == 0 {
            return 0;
        }
        let Ok(bottom) = self
            .y_of(index)
            .map(|top| top.saturating_add(self.line_height_or_zero(index)))
        else {
            return 0;
        };
        // `bottom <= viewport_top`: the changed line ends at or above the top of the viewport, so it
        // is entirely off-screen above and everything visible moved by `delta`.
        //
        // `<=`, not `<`: a line that *ends exactly at* `viewport_top` has no visible pixels, so
        // compensating is correct, and H2 makes the same choice.
        if bottom <= viewport_top {
            delta
        } else {
            0
        }
    }

    /// The half-open line range needed to fill a viewport at scroll offset `y`, with `overscan` lines
    /// of margin on each side.
    ///
    /// Ported from H2's `Geometry::visible_range`. `None` only for a document with no lines, which
    /// cannot happen: [`remove_line`](Self::remove_line) refuses to leave zero.
    ///
    /// `overscan` exists because mounting is not free: without it, a one-line scroll invalidates
    /// everything, because the line that was at the bottom edge is now at the top. H2 measured this
    /// as the difference between a smooth and a stuttering fast scroll.
    pub fn visible_range(
        &self,
        y: u32,
        viewport_height: u32,
        overscan: usize,
    ) -> Option<(usize, usize)> {
        let n = self.metrics.len();
        if n == 0 {
            return None;
        }
        let first = self.line_at(y).saturating_sub(overscan);
        let last_visible = self.line_at(y.saturating_add(viewport_height));
        let last = (last_visible + 1 + overscan).min(n);
        Some((first, last.max(first + 1).min(n)))
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
    /// The empty rect: nothing is damaged.
    pub const EMPTY: Self = Self {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };

    /// Build a rect.
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// One past the right edge.
    #[inline]
    pub fn right(&self) -> u32 {
        self.x.saturating_add(self.width)
    }

    /// One past the bottom edge.
    #[inline]
    pub fn bottom(&self) -> u32 {
        self.y.saturating_add(self.height)
    }

    /// Whether this rect touches nothing.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// The smallest rect containing both. Empty if either is empty.
    ///
    /// The union is what a frame accumulates: two keystrokes on different lines repaint one rect
    /// spanning both, and the union is never larger than the sum of the parts.
    ///
    /// # `saturating` rather than wrapping
    ///
    /// `x + width` on a `u32` can wrap for a rect near the top-left of a huge coordinate space, and
    /// a wrapped `right()` is *smaller* than `x`, so a plain `+` would make `union` produce a rect
    /// with negative width -- which then reads as empty and silently drops the damage. Saturation
    /// clamps to `u32::MAX`, which is the right answer for a coordinate that cannot be represented.
    pub fn union(&self, other: &DamageRect) -> DamageRect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        DamageRect {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }

    /// Clip to `bounds`, returning [`DamageRect::EMPTY`] if they do not overlap.
    pub fn clip(&self, bounds: &DamageRect) -> DamageRect {
        if self.is_empty() || bounds.is_empty() {
            return DamageRect::EMPTY;
        }
        let x = self.x.max(bounds.x);
        let y = self.y.max(bounds.y);
        let right = self.right().min(bounds.right());
        let bottom = self.bottom().min(bounds.bottom());
        if right <= x || bottom <= y {
            return DamageRect::EMPTY;
        }
        DamageRect {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }

    /// Whether `y` falls inside, half-open at the bottom.
    ///
    /// Half-open because a line's damage rect must include the row *after* its last text row when the
    /// next line's ascenders bleed into it, and because an off-by-one here repaints one row too few
    /// and leaves a visible artefact.
    #[inline]
    pub fn contains_row(&self, y: u32) -> bool {
        y >= self.y && y < self.bottom()
    }

    /// Every scanout row in this rect, as `y0..y1`. For callers that walk rows.
    pub fn row_range(&self) -> std::ops::Range<u32> {
        self.y..self.bottom()
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
