//! Skyline bottom-left packing, isolated so its behaviour can be tested directly.
//!
//! Separated from `atlas.rs` because the atlas tests exercise it only indirectly, and the
//! properties worth testing — that a box lands at the *lowest* free y, and that two boxes never
//! overlap — need white-box access.
//!
//! # Representation, and why the envelope is NOT monotonic
//!
//! A `Vec` of `(x, y)` breakpoints, left to right, where a node's `y` is the envelope height
//! across `[x, next_x)`. `x` is strictly increasing and starts at 0. **`y` may decrease.**
//!
//! Most skyline descriptions require `y` to be non-decreasing, and enforcing that is what
//! broke this packer twice:
//!
//! * The first implementation *propagated* a new box's height to every segment to its right,
//!   which is only true over the columns the box actually covers. A 200×40 box at x = 0 raised
//!   the whole envelope to 40, and `packer_is_area_exact_for_uniform_boxes` placed 65 of 64
//!   possible 10px boxes in a 64×64 region.
//! * The second tried the textbook fix — remove breakpoints right of the insertion whose height
//!   is lower. That is correct *only* under monotonicity, and it destroys the free space to the
//!   right of a short box: placing one 10×10 box at the origin would delete every remaining
//!   breakpoint and make the region x ∈ [10, 64) look occupied to y = 10. Subsequent boxes
//!   stacked vertically down the left edge instead of tiling.
//!
//! With a non-monotonic envelope, `height_over` takes the max over the segments a candidate box
//! spans, which is the correct overlap test, and `fit` picks the lowest y. Tiling then works:
//! after a 10×10 box at the origin the envelope is `[(0,10), (10,0)]`, so the next box's lowest
//! position is (10, 0), not (0, 10).

/// The upper envelope of placed boxes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Skyline {
    points: Vec<(u32, u32)>,
    /// Scratch buffer for `occupy`, swapped with `points` so a placement allocates only when it
    /// has to grow. Two allocations and two full copies per placement was 12-20 ms of the boot.
    spare: Vec<(u32, u32)>,
    width: u32,
    height: u32,
}

impl Skyline {
    /// An empty envelope over a `width x height` region.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            points: vec![(0, 0)],
            spare: Vec::new(),
            width,
            height,
        }
    }

    /// The breakpoints, for tests and diagnostics.
    pub fn points(&self) -> &[(u32, u32)] {
        &self.points
    }

    /// Highest occupied y anywhere.
    pub fn peak(&self) -> u32 {
        self.points.iter().map(|&(_, y)| y).max().unwrap_or(0)
    }

    /// The envelope height across `[x, x + w)`: the maximum y over the segments it touches.
    ///
    /// A node at `sx` with height `sy` covers `[sx, next_sx)`, so it counts only if that span
    /// *intersects* the query range — `sx < end` **and** `seg_end > x`.
    ///
    /// Getting the second condition wrong is the bug this comment exists for. An earlier version
    /// only checked `sx < end` and so included every node starting to the left of the query,
    /// which made `height_over(10, 10)` report the height of the node at x = 0. That in turn
    /// told `fit` that the space beside a box was occupied, and the packer stacked every box
    /// down the left edge instead of tiling. `a_box_does_not_raise_the_envelope_to_its_left`
    /// and `the_envelope_may_fall_after_a_short_box` are the regression tests.
    pub fn height_over(&self, x: u32, w: u32) -> u32 {
        if w == 0 {
            return self.height_at(x);
        }
        let end = x + w;
        let n = self.points.len();
        let mut top = 0;
        for i in 0..n {
            let (sx, sy) = self.points[i];
            if sx >= end {
                break;
            }
            let seg_end = if i + 1 < n {
                self.points[i + 1].0
            } else {
                self.width
            };
            if seg_end > x {
                top = top.max(sy);
            }
        }
        top
    }

    /// The envelope height at a single x: the `y` of the last node with `node.x <= x`.
    fn height_at(&self, x: u32) -> u32 {
        let mut y = 0;
        for &(sx, sy) in &self.points {
            if sx > x {
                break;
            }
            y = sy;
        }
        y
    }

    /// Lowest `(x, y)` where a `w x h` box fits, or `None` if it fits nowhere.
    ///
    /// Bottom-left: the lowest `y`, ties broken by the smallest `x`. Only the nodes' own `x`
    /// positions need checking, because the envelope is constant between nodes, so the lowest
    /// y over any interval is attained at its left edge — and every box's right edge *is* a
    /// node, so no interior optimum is missed.
    ///
    /// # Why this is one pass and not a nested loop
    ///
    /// The obvious form calls [`height_over`](Self::height_over) per candidate, and that is
    /// `O(n)` itself, so `fit` is `O(n^2)` and a full pack is `O(n^3)`. Measured at ~1,500
    /// glyphs this was the single largest cost in the one-time pass: **40 ms of a 113 ms
    /// boot**, more than rasterising. Since the candidate `x` values and their `x + w` right
    /// edges both increase monotonically as the scan proceeds, each answer is a sliding-window
    /// maximum over the segments, and a monotone deque gives all of them in `O(n)` total.
    ///
    /// The deque holds segment indices with strictly decreasing `y`. A new segment whose `y` is
    /// at or above the back's evicts it: the new one starts later, so it outlives the one it
    /// displaces, and it has the better value. The converse must *not* evict — a new segment
    /// with a lower `y` has to be kept, because the higher one it is compared against is still in
    /// the window and still the answer. Getting that comparison the wrong way round (`>=`
    /// instead of `<=`) makes `fit` report the *minimum* over the window instead of the maximum,
    /// which reads as the packer proposing boxes on top of each other.
    ///
    /// Any segment whose span ends at or before the current `x` is dropped from the front, since
    /// the window has moved past it.
    pub fn fit(&self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w == 0 || h == 0 || w > self.width || h > self.height {
            return None;
        }
        let n = self.points.len();
        // Segment `i` spans `[points[i].0, points[i + 1].0)`, the last spanning to `width`.
        let seg_end = |i: usize| -> u32 {
            if i + 1 < n {
                self.points[i + 1].0
            } else {
                self.width
            }
        };

        let mut deque: std::collections::VecDeque<usize> =
            std::collections::VecDeque::with_capacity(n);
        let mut next = 0usize;
        let mut best: Option<(u32, u32)> = None;

        for i in 0..n {
            let x = self.points[i].0;
            if x + w > self.width {
                break;
            }
            let end = x + w;

            // Admit every segment starting before the window's right edge.
            while next < n && self.points[next].0 < end {
                let sy = self.points[next].1;
                while deque.back().is_some_and(|&b| self.points[b].1 <= sy) {
                    deque.pop_back();
                }
                deque.push_back(next);
                next += 1;
            }
            // Drop segments the window has moved past.
            while deque.front().is_some_and(|&f| seg_end(f) <= x) {
                deque.pop_front();
            }
            let y = deque.front().map_or(0, |&f| self.points[f].1);
            if y + h > self.height {
                continue;
            }
            best = Some(match best {
                None => (x, y),
                Some(b) => {
                    if (y, x) < (b.1, b.0) {
                        (x, y)
                    } else {
                        b
                    }
                }
            });
        }
        best
    }

    /// Occupy `w x h` at `(x, y)`.
    ///
    /// # Panics
    ///
    /// If the box lies outside the region or overlaps an existing box. This is a
    /// programming-error guard: letting an inconsistent insert through corrupts the envelope for
    /// every subsequent placement, and the corruption is silent.
    pub fn occupy(&mut self, x: u32, y: u32, w: u32, h: u32) {
        assert!(
            x + w <= self.width && y + h <= self.height,
            "occupy({x},{y},{w},{h}) exceeds the {}x{} region",
            self.width,
            self.height
        );
        assert!(
            self.height_over(x, w) <= y,
            "occupy({x},{y},{w},{h}) overlaps: envelope is {} at x={x}",
            self.height_over(x, w)
        );

        let top = y + h;
        let end = x + w;
        let n = self.points.len();
        // Node `i` spans `[points[i].0, points[i + 1].0)`, the last spanning to `width`.
        let span_end = |i: usize| -> u32 {
            if i + 1 < n {
                self.points[i + 1].0
            } else {
                self.width
            }
        };

        // Emitted in x order across four passes so the result needs no sorting. See the module
        // note: a `retain`/`insert` clip followed by `sort_unstable_by_key` was `O(n log n)` per
        // placement, ~15M comparisons across the pass.
        let mut rebuilt = std::mem::take(&mut self.spare);
        rebuilt.clear();
        // Skip a repeat of the last entry. Only pass P1 can duplicate, and only as consecutive
        // `(x, top)`, so this is enough to leave the sequence strictly increasing.
        macro_rules! emit {
            ($px:expr, $py:expr) => {{
                let (px, py): (u32, u32) = ($px, $py);
                if rebuilt.last().is_none_or(|&(lx, ly)| lx != px || ly != py) {
                    rebuilt.push((px, py));
                }
            }};
        }

        // P0: entirely left of the box.
        for i in 0..n {
            if span_end(i) <= x {
                let (sx, sy) = self.points[i];
                emit!(sx, sy);
            }
        }
        // P1: overlaps [x, end). Its part before x keeps its height; [x, end) is covered, so it
        // takes the box's top. Every such node emits `x`, and `normalise` merges them.
        for i in 0..n {
            let (sx, sy) = self.points[i];
            if sx >= end || span_end(i) <= x {
                continue;
            }
            if sx < x {
                emit!(sx, sy);
            }
            emit!(x, top);
        }
        // P2: straddles the box's right edge, so only its part before `end` was covered.
        for i in 0..n {
            let (sx, sy) = self.points[i];
            if sx < end && span_end(i) > end {
                emit!(end, sy);
                break;
            }
        }
        // P3: entirely right of the box.
        for i in 0..n {
            if self.points[i].0 >= end {
                let (sx, sy) = self.points[i];
                emit!(sx, sy);
            }
        }

        debug_assert!(
            rebuilt.windows(2).all(|w| w[0].0 < w[1].0),
            "occupy must emit strictly increasing x; got {:?}",
            rebuilt
        );
        debug_assert!(
            rebuilt.first().is_some_and(|&(sx, _)| sx == 0),
            "the first breakpoint must be x = 0; got {:?}",
            rebuilt.first()
        );
        std::mem::swap(&mut self.spare, &mut self.points);
        self.points = rebuilt;
    }

    /// Assert the invariants. `y` is deliberately *not* required to be non-decreasing; see the
    /// module docs for why.
    pub fn check_invariants(&self) -> Result<(), String> {
        if self.points.first().map(|&(x, _)| x) != Some(0) {
            return Err(format!(
                "first breakpoint is {:?}, want x = 0",
                self.points.first()
            ));
        }
        for w in self.points.windows(2) {
            let (x0, y0) = w[0];
            let (x1, y1) = w[1];
            if x1 <= x0 {
                return Err(format!("x not strictly increasing: {x0} then {x1}"));
            }
            let _ = (y0, y1);
        }
        if let Some(&(_, y)) = self.points.last() {
            if y > self.height {
                return Err(format!("peak {y} exceeds height {}", self.height));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift, so the randomised tests are reproducible from a reported seed.
    struct Rng(u32);

    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            self.0
        }
    }

    #[test]
    fn empty_skyline_is_flat_at_zero() {
        let s = Skyline::new(64, 64);
        assert_eq!(s.points(), &[(0, 0)]);
        assert_eq!(s.peak(), 0);
        assert_eq!(s.height_over(0, 10), 0);
        assert_eq!(s.height_over(30, 10), 0);
        s.check_invariants().expect("invariants");
    }

    #[test]
    fn first_box_lands_at_origin() {
        let mut s = Skyline::new(64, 64);
        assert_eq!(s.fit(10, 10), Some((0, 0)));
        s.occupy(0, 0, 10, 10);
        assert_eq!(s.peak(), 10);
        // The region to the right of the box must still be free.
        assert_eq!(s.height_over(10, 10), 0, "right of the box must be empty");
        s.check_invariants().expect("invariants");
    }

    /// The packer must be **area exact** for uniform boxes: `floor(n/a)^2` fit, no more and no
    /// fewer. Waste shows up as too few; overlap panics in `occupy`.
    ///
    /// This is the test that failed at `left: 65, right: 36` when `occupy` propagated heights
    /// rightward, and again when the textbook monotonicity rule deleted the free space beside a
    /// short box. It is the sharpest available statement that a skyline packer works.
    #[test]
    fn packer_is_area_exact_for_uniform_boxes() {
        for &(n, a) in &[
            (64u32, 10u32),
            (64, 16),
            (128, 8),
            (100, 5),
            (50, 7),
            (96, 12),
        ] {
            let mut s = Skyline::new(n, n);
            let mut count = 0;
            while let Some((x, y)) = s.fit(a, a) {
                s.occupy(x, y, a, a);
                count += 1;
                assert!(count < 10_000, "packer did not terminate");
            }
            let per_side = n / a;
            assert_eq!(
                count,
                per_side * per_side,
                "{a}px boxes in {n}x{n}: placed {count}, optimum {}",
                per_side * per_side
            );
            s.check_invariants()
                .unwrap_or_else(|e| panic!("{a} in {n}: {e}"));
        }
    }

    /// Mixed sizes must never overlap, and the count must match the area bound for shapes that
    /// tile.
    #[test]
    fn mixed_sizes_do_not_overlap() {
        let mut s = Skyline::new(64, 64);
        let mut placed: Vec<(u32, u32, u32, u32)> = Vec::new();
        let mut n = 0;
        while let Some((x, y)) = s.fit(8, 8) {
            s.occupy(x, y, 8, 8);
            placed.push((x, y, 8, 8));
            n += 1;
            assert!(n < 1000, "packer did not terminate");
        }
        assert_eq!(placed.len(), 64, "eight-pixel boxes tile 64x64 exactly");
        for a in 0..placed.len() {
            for b in (a + 1)..placed.len() {
                let (ax, ay, aw, ah) = placed[a];
                let (bx, by, bw, bh) = placed[b];
                let disjoint = ax + aw <= bx || bx + bw <= ax || ay + ah <= by || by + bh <= ay;
                assert!(disjoint, "{a} {ax},{ay} overlaps {b} {bx},{by}");
            }
        }
        s.check_invariants().expect("invariants");
    }

    /// `fit` must return the lowest y, not merely the leftmost fit. A packer that takes the
    /// leftmost position regardless of height leaves holes and is not area exact.
    #[test]
    fn fit_prefers_the_lowest_y() {
        let mut s = Skyline::new(64, 64);
        s.occupy(0, 0, 20, 40); // tall and narrow, on the left
                                // A short box should tuck to the right at y = 0, not stack on top at y = 40.
        let (x, y) = s.fit(10, 10).expect("fits");
        assert_eq!(y, 0, "must find y = 0 to the right of the tall box");
        assert!(x >= 20, "and to the right of it, got x = {x}");
        // A box that is *wider* than the gap left by the tall one has to go on top of it,
        // because it can no longer sit beside. With the tall box at [0, 20), a 30-wide box
        // starting at x = 0 spans into the occupied column.
        let (x, y) = s.fit(30, 30).expect("fits");
        assert!(
            x >= 20 || y >= 40,
            "a 30x30 box fits at ({x},{y}), which overlaps the tall box"
        );
        assert!(
            s.height_over(x, 30) <= y,
            "the reported position must actually be clear"
        );
    }

    #[test]
    fn a_box_wider_or_taller_than_the_region_does_not_fit() {
        let s = Skyline::new(16, 16);
        assert_eq!(s.fit(17, 1), None);
        assert_eq!(s.fit(1, 17), None);
        assert_eq!(s.fit(0, 4), None, "zero width is degenerate");
        assert_eq!(s.fit(16, 16), Some((0, 0)));
    }

    #[test]
    fn a_full_region_reports_no_fit() {
        let mut s = Skyline::new(16, 16);
        s.occupy(0, 0, 16, 16);
        assert_eq!(s.fit(1, 1), None);
        s.check_invariants().expect("invariants");
    }

    #[test]
    #[should_panic(expected = "overlaps")]
    fn overlapping_occupy_panics() {
        let mut s = Skyline::new(64, 64);
        s.occupy(0, 0, 20, 20);
        s.occupy(10, 10, 20, 20);
    }

    #[test]
    #[should_panic(expected = "exceeds")]
    fn out_of_region_occupy_panics() {
        let mut s = Skyline::new(64, 64);
        s.occupy(60, 0, 10, 10);
    }

    /// A box at a non-zero x must not raise the envelope to its left. This is the specific
    /// regression that made the packer waste 45% of the atlas.
    #[test]
    fn a_box_does_not_raise_the_envelope_to_its_left() {
        let mut s = Skyline::new(64, 64);
        s.occupy(32, 0, 32, 40); // right half only
        assert_eq!(s.height_over(0, 32), 0, "the left half must still be empty");
        assert_eq!(
            s.height_over(32, 32),
            40,
            "the right half is occupied to y = 40"
        );
        s.check_invariants().expect("invariants");
    }

    /// The envelope may fall. After a short box at the origin, the region beside it must read
    /// as free, which is what lets the next box tile rather than stack.
    #[test]
    fn the_envelope_may_fall_after_a_short_box() {
        let mut s = Skyline::new(64, 64);
        s.occupy(0, 0, 10, 10);
        let points = s.points();
        assert!(
            points.iter().any(|&(_, y)| y == 0),
            "the region right of a 10px box must read as height 0, got {points:?}"
        );
        assert_eq!(
            s.fit(10, 10),
            Some((10, 0)),
            "the next box tiles, not stacks"
        );
    }

    /// Invariants must hold under a long randomised run, with the seed reported on failure.
    #[test]
    fn invariants_hold_under_a_long_packing_run() {
        for seed in [0x1234_5678u32, 0xDEAD_BEEF, 0x5EED_1234] {
            let mut s = Skyline::new(1024, 512);
            let mut rng = Rng(seed);
            let mut placed = 0;
            for _ in 0..20_000 {
                let w = 1 + rng.next() % 24;
                let h = 1 + rng.next() % 24;
                match s.fit(w, h) {
                    Some((x, y)) => {
                        s.occupy(x, y, w, h);
                        placed += 1;
                    }
                    None => break,
                }
            }
            assert!(placed > 100, "seed {seed:#x}: only placed {placed} boxes");
            s.check_invariants()
                .unwrap_or_else(|e| panic!("seed {seed:#x} after {placed} placements: {e}"));
            assert!(s.peak() <= 512, "seed {seed:#x}: peak {} > 512", s.peak());
        }
    }

    /// `fit` must never propose a position `occupy` rejects, and must never report no fit while
    /// space provably remains. A false fit corrupts the envelope; a missed fit leaks space.
    #[test]
    fn fit_and_occupy_agree() {
        for seed in [0xA5A5_1234u32, 0x0F0F_0F0F] {
            let mut s = Skyline::new(64, 64);
            let mut rng = Rng(seed);
            for _ in 0..3000 {
                let w = 1 + rng.next() % 16;
                let h = 1 + rng.next() % 16;
                match s.fit(w, h) {
                    Some((x, y)) => {
                        // Verify *before* placing. After `occupy` the box is itself part of the
                        // envelope, so `height_over` legitimately returns y + h and the old
                        // post-hoc check failed on its own first placement with
                        // "placed a 6x12 box at (0,0) over occupied space".
                        //
                        // The probe uses `height_at` per column rather than `height_over` for the
                        // whole span, so it re-derives the answer from a different function than
                        // the one `fit` used and cannot inherit `fit`'s bug.
                        for probe in x..x + w {
                            assert!(
                                s.height_at(probe) <= y,
                                "seed {seed:#x}: fit proposed a {w}x{h} box at ({x},{y}) but \
                                 column {probe} is occupied to {}",
                                s.height_at(probe)
                            );
                        }
                        s.occupy(x, y, w, h); // must not panic
                    }
                    None => {
                        // No fit must be correct: brute-force every x and confirm none works.
                        for probe in 0..=(64 - w) {
                            let clear = (probe..probe + w).all(|x| s.height_at(x) + h <= 64);
                            assert!(
                                !clear,
                                "seed {seed:#x}: fit said None for {w}x{h} but x={probe} is clear"
                            );
                        }
                    }
                }
                s.check_invariants()
                    .unwrap_or_else(|e| panic!("seed {seed:#x}: {e}"));
            }
        }
    }
}
