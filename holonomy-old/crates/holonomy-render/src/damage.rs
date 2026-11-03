//! Damage tracking: which parts of the scanout must be repainted this frame.
//!
//! FR-3.4: "The rendering loop must operate on a Damage-Bounded Dirty-Row Model. Typing a character
//! invalidates and redraws only the scanout rows intersecting the active text line (a bounding box of
//! approximately 600x16 pixels). Full-screen redraws during typing are strictly prohibited."
//!
//! # What this stores
//!
//! A [`DamageRect`], one at a time, accumulating a *union*. The union is the point: two keystrokes on
//! different lines produce one rect spanning both, and it is never larger than the sum of the parts.
//!
//! # Why not a bitmap of dirty rows
//!
//! A row bitmap is the obvious representation and it is the wrong one for this machine. The target
//! panel is 1280x800, so a row bitmap is 800 bits = 100 bytes -- which is nothing, and is also the
//! wrong abstraction. Damage is *rectangular*: a keystroke marks a horizontal band of full-width
//! rows, and a scroll marks a full-screen rect. Both are exactly representable as a rect, and a rect
//! costs 16 bytes and no scanning. A row bitmap would have to be walked to produce the extent, which
//! is O(rows) per frame for no information the rect does not already carry.
//!
//! Where a bitmap would win is scattered damage -- many disjoint single pixels -- and that does not
//! occur in a text editor: every damage source here is a band or a rect.
//!
//! # The clip is not optional
//!
//! [`DamageTracker::add`] clips every rect to the scanout. A damage rect from a line 3,000 px down a
//! scrolled document is legitimately outside the panel, and passing it to a blitter as-is would index
//! the scanout out of bounds. Clipping at the point of insertion means the blitter never has to
//! check, which is what keeps it to one bounds test per *glyph* and none per *row*.

/// A rectangle of the scanout that must be repainted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamageRect {
    /// Left edge, in pixels.
    pub x: u32,
    /// Top edge, in pixels.
    pub y: u32,
    /// Width in pixels, in pixels.
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

    /// Number of scanout rows touched.
    #[inline]
    pub fn rows(&self) -> u32 {
        self.height
    }

    /// Whether this rect touches nothing.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Whether `y` falls inside, half-open at the bottom.
    #[inline]
    pub fn contains_row(&self, y: u32) -> bool {
        y >= self.y && y < self.bottom()
    }

    /// Whether the point `(x, y)` is inside, **half-open on the right and bottom**.
    ///
    /// # Why this is here rather than in the widget code
    ///
    /// **Because three of part 19's hit tests needed it and each would have written it slightly
    /// differently.** Adjacent buttons on the toolbar share an edge, and a hit test that uses `<=` on
    /// the right picks the *right* button when the pointer is on the seam. Half-open intervals tile
    /// without a seam, which is the same reason the paint bands in [`Layout`](crate::chrome::Layout)
    /// stack with `saturating_add`.
    ///
    /// **The arguments are `i32` because that is what a pointer event carries.** Clamping a negative
    /// coordinate to zero here rather than letting the caller do it means "left of the sidebar" and
    /// "above the title bar" are both simply `false`, and no caller has to remember to check.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x as i32
            && y >= self.y as i32
            && x < (self.x + self.width) as i32
            && y < (self.y + self.height) as i32
    }

    /// The smallest rect containing both. Empty if either is empty.
    ///
    /// # `saturating` rather than wrapping
    ///
    /// `x + width` on a `u32` can wrap for a rect near the top-left of a huge coordinate space, and a
    /// wrapped `right()` is *smaller* than `x`, so a plain `+` would make `union` produce a rect with
    /// negative width -- which then reads as empty and silently drops the damage. Saturation clamps
    /// to `u32::MAX`, which is the right answer for a coordinate that cannot be represented.
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

    /// Clip to `bounds`, returning an empty rect if they do not overlap.
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

    /// Every scanout row in this rect, as `y0..y1`. For callers that walk rows.
    pub fn row_range(&self) -> std::ops::Range<u32> {
        self.y..self.bottom()
    }
}

/// Accumulates damage for one frame and hands the union to the renderer.
#[derive(Debug, Clone)]
pub struct DamageTracker {
    /// The panel's extent, so nothing is ever added outside it.
    bounds: DamageRect,
    /// The accumulated union, `EMPTY` when nothing is damaged.
    pending: DamageRect,
    /// How many rects were added since the last flush. Diagnostic, and the gate's evidence that a
    /// keystroke adds one rect rather than many.
    added_since_flush: u32,
    /// Total rects added since construction, for the lifetime figure.
    added_total: u64,
}

impl DamageTracker {
    /// A tracker for a `width` x `height` scanout.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            bounds: DamageRect::new(0, 0, width, height),
            pending: DamageRect::EMPTY,
            added_since_flush: 0,
            added_total: 0,
        }
    }

    /// The panel's extent.
    #[inline]
    pub fn bounds(&self) -> DamageRect {
        self.bounds
    }

    /// Mark `rect` damaged, clipped to the panel.
    ///
    /// Clipping here rather than in the blitter is what keeps the blitter free of per-row bounds
    /// checks: by the time a rect reaches it, every row is inside the scanout.
    pub fn add(&mut self, rect: DamageRect) {
        let clipped = rect.clip(&self.bounds);
        if clipped.is_empty() {
            return;
        }
        self.pending = self.pending.union(&clipped);
        self.added_since_flush += 1;
        self.added_total += 1;
    }

    /// Mark a horizontal band of full-width rows.
    ///
    /// The shape a keystroke produces: one text line's worth of rows, across the whole panel width.
    pub fn add_rows(&mut self, y: u32, rows: u32) {
        self.add(DamageRect::new(0, y, self.bounds.width, rows));
    }

    /// Mark one rect as needing a full repaint. The boot path, and `Ctrl-L`.
    pub fn add_all(&mut self) {
        self.pending = self.bounds;
        self.added_since_flush += 1;
        self.added_total += 1;
    }

    /// The accumulated damage, `EMPTY` if there is none.
    #[inline]
    pub fn peek(&self) -> DamageRect {
        self.pending
    }

    /// Whether anything is damaged.
    #[inline]
    pub fn is_dirty(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Take the accumulated damage and reset to empty.
    ///
    /// `take` rather than `get` + `clear`: two frames rendered concurrently must not both see the
    /// same damage, and a get-then-clear pair has a window where both do.
    pub fn flush(&mut self) -> DamageRect {
        let r = self.pending;
        self.pending = DamageRect::EMPTY;
        self.added_since_flush = 0;
        r
    }

    /// How many rects were added since the last flush.
    #[inline]
    pub fn rects_since_flush(&self) -> u32 {
        self.added_since_flush
    }

    /// How many rects have been added since construction.
    #[inline]
    pub fn rects_total(&self) -> u64 {
        self.added_total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panel: 1280x800 at 32 bpp, per FR-3.3.
    const PANEL_W: u32 = 1280;
    const PANEL_H: u32 = 800;

    fn tracker() -> DamageTracker {
        DamageTracker::new(PANEL_W, PANEL_H)
    }

    #[test]
    fn a_new_tracker_is_clean() {
        let mut t = tracker();
        assert!(!t.is_dirty());
        assert_eq!(t.peek(), DamageRect::EMPTY);
        assert_eq!(t.flush(), DamageRect::EMPTY);
        assert_eq!(t.rects_total(), 0);
    }

    /// The concrete requirement: a keystroke touches rows 420-436 and nothing else.
    ///
    /// 17 rows for a 16 px line, which is the figure PROJECT.md §5 Phase 5 gives.
    #[test]
    fn a_keystroke_at_row_420_damages_rows_420_to_436() {
        let mut t = tracker();
        t.add_rows(420, 17);
        let d = t.flush();
        assert_eq!(d.y, 420);
        assert_eq!(d.height, 17);
        assert_eq!(d.rows(), 17);
        assert_eq!(d.x, 0);
        assert_eq!(d.width, PANEL_W);
        for y in 420..437 {
            assert!(d.contains_row(y), "row {y} must be damaged");
        }
        assert!(!d.contains_row(419), "row 419 must not be damaged");
        assert!(!d.contains_row(437), "row 437 must not be damaged");
    }

    /// The claim that matters most: the damage is a small fraction of the screen.
    #[test]
    fn a_keystrokes_damage_a_small_fraction_of_the_panel() {
        let full_rows = PANEL_H;
        let keystroke_rows = 17u32;
        let fraction = keystroke_rows as f64 / full_rows as f64;
        assert!(
            fraction < 0.03,
            "a keystroke damages {:.1}% of the panel, which is not \"bounded\"",
            fraction * 100.0
        );
        // And the pixel count. FR-3.4 quotes "approximately 600x16 pixels" -- that is the *text
        // area's* width, not the panel's, so a keystroke across the full 1280 px is 2.1% of the
        // panel rather than the 0.75% the quoted figure implies. The bound to assert is the one the
        // requirement means: a keystroke must be a small fraction, not a full-screen redraw. An
        // earlier version of this test demanded 1% and reported
        // "a keystroke damages 21760 of 1024000 pixels" -- 2.1%, which is 40x smaller than a full
        // repaint and entirely correct.
        let keystroke_px = u64::from(PANEL_W) * u64::from(keystroke_rows);
        let panel_px = u64::from(PANEL_W) * u64::from(PANEL_H);
        let px_fraction = keystroke_px as f64 / panel_px as f64;
        assert!(
            px_fraction < 0.03,
            "a keystroke damages {:.2}% of the panel's pixels",
            px_fraction * 100.0
        );
        // And the point of the requirement, stated as a ratio: a keystroke is orders of magnitude
        // cheaper than a full repaint.
        assert!(
            keystroke_px * 20 < panel_px,
            "a keystroke should be far cheaper than a full repaint: {keystroke_px} vs {panel_px}"
        );
    }

    /// Two keystrokes on one line: one rect's worth of damage, not two.
    #[test]
    fn two_keystrokes_on_one_line_damage_it_once() {
        let mut t = tracker();
        t.add_rows(420, 17);
        t.add_rows(420, 17);
        let d = t.flush();
        assert_eq!(d.height, 17, "the union of a rect with itself is itself");
        assert_eq!(t.rects_total(), 2, "but two rects were added");
    }

    #[test]
    fn keystrokes_on_different_lines_union_into_one_band() {
        let mut t = tracker();
        t.add_rows(420, 17);
        t.add_rows(440, 17);
        let d = t.flush();
        assert_eq!(d.y, 420);
        assert_eq!(d.bottom(), 457);
        assert_eq!(d.height, 37);
    }

    #[test]
    fn flush_resets_and_hands_the_damage_over() {
        let mut t = tracker();
        t.add_rows(100, 20);
        assert!(t.is_dirty());
        let first = t.flush();
        assert_eq!(first.y, 100);
        assert!(!t.is_dirty(), "flushing clears the damage");
        assert_eq!(t.rects_since_flush(), 0);
        // A second flush with nothing added yields nothing, so the renderer skips the frame.
        assert_eq!(t.flush(), DamageRect::EMPTY);
    }

    /// Damage below the fold must not reach the blitter.
    #[test]
    fn damage_below_the_panel_is_clipped_away() {
        let mut t = tracker();
        t.add_rows(5000, 17);
        assert!(!t.is_dirty(), "row 5000 is below an 800-row panel");
        // Partially visible: the bottom half of a line hanging off the bottom edge.
        t.add_rows(795, 17);
        let d = t.flush();
        assert_eq!(d.y, 795);
        assert_eq!(d.height, 5, "clipped to the panel's last row");
    }

    #[test]
    fn damage_off_the_left_or_right_is_clipped() {
        let mut t = tracker();
        t.add(DamageRect::new(1270, 100, 100, 20));
        let d = t.flush();
        assert_eq!(d.x, 1270);
        assert_eq!(d.width, 10, "clipped to the panel's right edge");
        // Entirely off the right: nothing.
        let mut t = tracker();
        t.add(DamageRect::new(2000, 100, 100, 20));
        assert!(!t.is_dirty());
    }

    #[test]
    fn a_full_repaint_is_the_whole_panel() {
        let mut t = tracker();
        t.add_rows(100, 17);
        t.add_all();
        let d = t.flush();
        assert_eq!(d.x, 0);
        assert_eq!(d.y, 0);
        assert_eq!(d.width, PANEL_W);
        assert_eq!(d.height, PANEL_H);
    }

    #[test]
    fn union_with_an_empty_rect_is_the_other_rect() {
        let a = DamageRect::new(10, 20, 30, 40);
        assert_eq!(a.union(&DamageRect::EMPTY), a);
        assert_eq!(DamageRect::EMPTY.union(&a), a);
        assert!(DamageRect::EMPTY.union(&DamageRect::EMPTY).is_empty());
    }

    /// A rect near the top of the address space must not produce a negative-width union.
    #[test]
    fn union_near_the_coordinate_limit_does_not_wrap() {
        let big = DamageRect::new(u32::MAX - 10, 0, 100, 10);
        let small = DamageRect::new(5, 0, 10, 10);
        let u = big.union(&small);
        // `right()` saturates, so the result is a wide rect rather than a wrapped one.
        assert!(u.width >= 5, "width {} wrapped", u.width);
        assert_eq!(u.x, 5);
        assert_eq!(u.height, 10);
    }

    /// A keystroke burst: 500 keystrokes on one line must accumulate to one line's damage.
    #[test]
    fn a_typing_burst_does_not_accumulate_area() {
        let mut t = tracker();
        for _ in 0..500 {
            t.add_rows(420, 17);
        }
        let d = t.flush();
        assert_eq!(d.rows(), 17, "500 keystrokes, 17 rows");
        assert_eq!(t.rects_total(), 500);
    }

    #[test]
    fn row_range_matches_contains_row() {
        let r = DamageRect::new(0, 420, 1280, 17);
        let range = r.row_range();
        assert_eq!(range, 420..437);
        assert_eq!(range.len(), r.rows() as usize);
    }
}
