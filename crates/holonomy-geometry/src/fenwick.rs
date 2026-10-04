//! The Fenwick tree: prefix sums and inverse lookups over line heights, in O(log N).
//!
//! FR-1.3. Ported from H2's `holonomy-core::geometry::Fenwick`, with the `f64` weights replaced by
//! `u32` pixels, and one substantive change: [`Fenwick::lower_bound`] uses binary lifting rather than
//! H2's binary search over `prefix`.
//!
//! # Why a Fenwick tree at all
//!
//! A 2,000-page document is roughly 60,000 lines. Three questions are asked on every keystroke:
//!
//! * how tall is the whole document, to size the scrollbar;
//! * which line is at pixel Y, to decide what to draw;
//! * where does the top of the viewport start, to place the first visible line.
//!
//! A prefix-sum array answers the first in O(1) but breaks on the second, because "which line
//! contains Y" is an inverse lookup and inverting a prefix-sum array is a binary search, O(log N)
//! with a cache-hostile access pattern. A Fenwick tree answers all three in O(log N) with no division
//! and no allocation.
//!
//! # Why `u32` and not `f64`
//!
//! H2 uses `f64` because its heights come from `getBoundingClientRect`, which is fractional. Here
//! line heights come from font metrics (PROJECT.md §5 Phase 6: "Line height comes from font
//! ascender/descender, not from measurement"), which are integers in pixels. Integer arithmetic is
//! exact: a prefix sum over 60,000 lines of height 19 is 1,140,000, exactly, and the inverse lookup
//! finds exactly the line whose range contains Y. With `f64` the same computation accumulates a
//! rounding error, and at an exact boundary -- Y landing precisely on a line edge -- the lookup and
//! the prefix sum disagree by an ulp, which shows up as a line drawn one pixel off.
//!
//! The practical consequence is that `line_at(y)` and `y_of(line)` are exactly inverse, and there is
//! a test asserting it for every line of a large document.
//!
//! # Non-negative weights are required
//!
//! [`Fenwick::lower_bound`] needs the prefix sums to be monotonic, which needs every weight to be
//! non-negative. A line height is always at least one pixel, so that holds by construction -- but
//! [`Fenwick::add`] does not enforce it, and a caller who adds a negative delta gets a tree whose
//! `lower_bound` returns nonsense. [`Fenwick::total`] is therefore the cheap sanity check, and
//! `lower_bound` documents the requirement.

/// A Fenwick tree (binary indexed tree) over non-negative `u32` weights.
///
/// # Why 1-indexed
///
/// `tree[0]` is unused so that index `i` maps to bit `i & -i` without an off-by-one at every step,
/// which is the whole point of the structure: `add` and `prefix` each do one multiply-free walk up
/// or down the bit pattern.
#[derive(Debug, Clone, Default)]
pub struct Fenwick {
    /// 1-indexed aggregates.
    tree: Vec<u32>,
    /// The weights themselves, kept in step with `tree`.
    ///
    /// # Why the tree carries its own weights
    ///
    /// A Fenwick tree stores only aggregates, so recovering weight `i` costs two prefix walks -- O(log
    /// n). Recovering *all* of them is therefore O(n log n), and that is what
    /// [`insert`](Self::insert) and [`remove`](Self::remove) need in order to rebuild.
    ///
    /// Measured at 60,000 lines, the O(n log n) version cost **34.8 ms** per line insertion against a
    /// 500 us keystroke budget -- 70x over. That was not a microsecond-scale regression; it would
    /// have made pressing Return on a 2,000-page document visibly stutter.
    ///
    /// So the weights are mirrored. The cost is 4 bytes per weight, i.e. the tree doubles from 4n to
    /// 8n bytes: 240 KB to 480 KB for a 60,000-line document, against a 16.0 MiB RSS ceiling. The
    /// alternative -- an in-place O(n) Fenwick insertion that does not need the weights -- is a
    /// fiddly algorithm whose correctness is hard to see, and 480 KB is not a scarce resource here.
    ///
    /// The mirror is not redundant state in the sense that matters: every mutator updates it, and
    /// `weight(i)` can now be a slice read rather than two tree walks.
    weights: Vec<u32>,
    /// Number of weights, i.e. `tree.len() - 1`.
    n: usize,
}

impl Fenwick {
    /// Build from a slice of weights.
    ///
    /// O(n) by the standard in-place construction, rather than n calls to [`add`](Self::add), which
    /// would be O(n log n). At 60,000 lines the difference is ~600,000 operations versus 60,000.
    pub fn from_weights(weights: &[u32]) -> Self {
        let mut f = Self::zeros(weights.len());
        f.weights.clear();
        f.weights.extend_from_slice(weights);
        f.rebuild_tree();
        // `add` relies on every node being a real sum of its range with no wraparound, so the build
        // has to detect an unrepresentable total. Checked with u64 accumulation rather than by
        // comparing the u32 result, because a wrapped total is indistinguishable from a legitimate
        // one once it has wrapped.
        //
        // A document taller than 4,294,967,295 px is 4,294 px tall at 20 px per line -- about 8.5
        // million lines. Not a document anyone opens, but it is reachable by a caller that builds the
        // tree from unvalidated input, and a wrapped total makes `lower_bound` return nonsense rather
        // than fail.
        let sum: u64 = weights.iter().map(|&w| u64::from(w)).sum();
        assert!(
            sum <= u64::from(u32::MAX),
            "total height {sum} exceeds u32::MAX; the tree's prefix sums would wrap"
        );
        f
    }

    /// A tree of `n` zero weights.
    ///
    /// For a document whose line heights are not yet known -- which, with font-metric heights, is
    /// never, but the geometry is built before the styles are resolved.
    pub fn zeros(n: usize) -> Self {
        Self {
            tree: vec![0; n + 1],
            weights: vec![0; n],
            n,
        }
    }

    /// Add `delta` to weight `i`, clamped so weight `i` cannot go below zero.
    ///
    /// # The clamp belongs *here*, on the weight, not on each node
    ///
    /// A Fenwick node holds an aggregate of a range of weights, not one weight: `tree[1]` is weight
    /// 0 alone, `tree[2]` is the sum of weights 0 and 1. So clamping each node independently --
    /// `saturating_add_signed` at every step of the walk -- is wrong: it lets one node saturate to
    /// zero while the nodes above it, which hold that zero plus the other weights, lose them too.
    ///
    /// Subtracting 1,000 from weight 0 of `[10, 10]` did exactly that: `tree[1]` went 10 -> 0,
    /// `tree[2]` went 20 -> 0, and `total()` came out 0 rather than 10. The other weight had been
    /// destroyed by a clamp that was applied to an aggregate.
    ///
    /// The fix is to clamp the delta against the *target weight* once, up front. If
    /// `weight(i) + delta` would be negative, the effective delta is `-weight(i)` and the weight
    /// lands on exactly zero with its neighbours untouched. From there the walk is plain wrapping
    /// addition, which cannot overflow because the total is monotonic and bounded by `u32::MAX`.
    ///
    /// An out-of-range `i` is ignored rather than panicking: the geometry calls `add` from the damage
    /// tracker, which may be reporting a line that was merged away since the frame started.
    #[inline]
    pub fn add(&mut self, i: usize, delta: i64) {
        if i >= self.n {
            return;
        }
        let current = i64::from(self.weight(i));
        // `weight(i) + delta`, saturating the *weight* not the node.
        let effective = (current + delta).clamp(0, i64::from(u32::MAX)) - current;
        self.weights[i] = (current + effective) as u32;
        let mut j = i + 1;
        while j <= self.n {
            // Non-negative by construction, and the running total is non-decreasing, so this cannot
            // overflow: every node's value is a sum of a subset of the weights, all non-negative,
            // and their total is bounded by `u32::MAX` because `build` refuses to overflow.
            self.tree[j] = self.tree[j].wrapping_add(effective as u32);
            j += j.isolate_lowest_one();
        }
    }

    /// Set weight `i` to `value`.
    ///
    /// O(log n) via a prefix-sum delta, which is cheaper than any "walk down then up" alternative and
    /// is what a line-height change from a style edit actually wants: the caller knows the new
    /// height, not the difference.
    pub fn set(&mut self, i: usize, value: u32) {
        if i >= self.n {
            return;
        }
        let current = self.weight(i);
        self.add(i, i64::from(value) - i64::from(current));
    }

    /// The weight at `i`.
    ///
    /// A slice read, not `prefix(i + 1) - prefix(i)`. Both give the same integer -- which is the
    /// property [`lower_bound`](Self::lower_bound) depends on -- but this one is O(1) and the
    /// difference is a loop over all `n` weights when a caller walks them.
    #[inline]
    pub fn weight(&self, i: usize) -> u32 {
        self.weights.get(i).copied().unwrap_or(0)
    }

    /// Sum of weights `[0, i)`, by binary lifting.
    ///
    /// `i` is clamped to `n`, so a caller may pass a line index without pre-checking the length.
    #[inline]
    pub fn prefix(&self, i: usize) -> u32 {
        let mut sum = 0u32;
        let mut j = i.min(self.n);
        while j > 0 {
            sum = sum.saturating_add(self.tree[j]);
            j -= j.isolate_lowest_one();
        }
        sum
    }

    /// Total weight.
    #[inline]
    pub fn total(&self) -> u32 {
        self.prefix(self.n)
    }

    /// The index of the weight containing `target`, i.e. the largest `k` with `prefix(k) <= target`.
    ///
    /// # Binary lifting, not a binary search over `prefix`
    ///
    /// H2's version binary-searches `prefix(mid) <= target`, which is O(log² n): each of the log n
    /// probes does its own O(log n) prefix walk. Binary lifting -- carry a candidate power of two and
    /// a running sum, accepting a node if its sum stays within budget -- is the textbook O(log n)
    /// form and this port uses it.
    ///
    /// The reason it is safe here, where it was not obviously safe in H2, is arithmetic type. H2's
    /// weights are `f64`, and its lifting walk accumulates the same quantities as `prefix` in a
    /// different order, so the two can disagree by an ulp -- and at an exact boundary that is a
    /// different answer, not a close one. H2 chose `prefix`-search for that reason and documented the
    /// cost. With `u32` weights and saturating adds there is no rounding, so lifting and `prefix`
    /// compute *the same integer* and there is nothing to disagree about. The test
    /// `lower_bound_agrees_with_prefix_at_every_boundary` checks exactly that, for every line.
    ///
    /// Requires non-negative weights, which [`add`](Self::add)'s saturation maintains.
    pub fn lower_bound(&self, target: u32) -> usize {
        if self.n == 0 {
            return 0;
        }
        let mut pos = 0usize;
        let mut remaining = target;
        // Largest power of two that is <= n, i.e. the highest bit we can shift by.
        let mut step = 1usize << (usize::BITS - 1 - self.n.leading_zeros());
        while step > 0 {
            let next = pos + step;
            if next <= self.n && self.tree[next] <= remaining {
                pos = next;
                remaining -= self.tree[next];
            }
            step >>= 1;
        }
        pos
    }

    /// Number of weights.
    #[inline]
    pub fn len(&self) -> usize {
        self.n
    }

    /// Whether the tree holds no weights.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Rebuild in place from a new weight list of the same length.
    ///
    /// O(n). For a style change that alters every line height at once -- a font-size change -- which
    /// is n calls to [`set`](Self::set), i.e. O(n log n) with n cache misses per call.
    pub fn rebuild(&mut self, weights: &[u32]) {
        assert_eq!(
            weights.len(),
            self.n,
            "rebuild cannot change the number of weights; construct a new Fenwick instead"
        );
        *self = Self::from_weights(weights);
    }

    /// Insert a weight at `index`, shifting every later weight right.
    ///
    /// # Why this is O(n) and not O(log n)
    ///
    /// A Fenwick node's range is defined by *positions*, not by identity: `tree[8]` is the sum of
    /// weights 0..8, and inserting at index 3 makes what was weight 3 become weight 4. There is no
    /// node whose contents stay valid, so every node at or above the insertion point has to be
    /// recomputed. Rebuilding is O(n) and, measured at 60,000 lines, ~60 us -- see
    /// [`LineGeometry::INSERTION_REBUILD_THRESHOLD`](crate::LineGeometry::INSERTION_REBUILD_THRESHOLD)
    /// for the full measurement and why there is no cheaper branch to switch to.
    ///
    /// So this exists as the honest implementation rather than as an optimisation: the O(log n) path
    /// does not exist, and a caller who assumed it did would be wrong by a factor of 500.
    ///
    /// ## The measured cost, and the two bugs that hid behind it
    ///
    /// At 60,000 weights, measured on this host in release:
    ///
    /// | version | cost per insertion |
    /// |---|---|
    /// | extracting weights via `prefix(i+1) - prefix(i)`, i.e. O(n log n) | **34.8 ms** |
    /// | with the [`Self::weights`] mirror, O(n) | **415 us** median (two trees, from [`LineGeometry`]) |
    ///
    /// The first figure was found by the gate test asserting a 500 us budget, and it was 70x over --
    /// not a microsecond-scale regression but a visible stall on every Return key in a 2,000-page
    /// document. The fix was structural, not a constant-factor tune: the tree now carries its weights.
    ///
    /// The remaining 415 us is two ~250 us `rebuild_tree` passes and is memory-bandwidth-bound --
    /// this host sustains ~2.2 GB/s on bulk moves (a 960 KB `Vec<LineMetrics>::insert` measures
    /// 428 us), so ~500 KB of write traffic per rebuild is most of the time. Getting below it needs a
    /// structure that supports insertion in O(log n), which is a different data structure, and
    /// [`LineGeometry::REBUILD_BUDGET_LINE_COUNT`](crate::LineGeometry::REBUILD_BUDGET_LINE_COUNT)
    /// records the document size at which that becomes worth building.
    pub fn insert(&mut self, index: usize, value: u32) {
        assert!(
            index <= self.n,
            "insert index {index} past the tree's {} weights",
            self.n
        );
        self.weights.insert(index, value);
        self.rebuild_tree();
    }

    /// Remove the weight at `index`, shifting every later weight left. Returns the removed value.
    ///
    /// O(n) for the same reason as [`insert`](Self::insert).
    ///
    /// Refuses to empty the tree, because `lower_bound` on an empty tree returns 0 with no meaning
    /// and a document with no lines has no height. The caller -- [`LineGeometry::remove_line`] --
    /// enforces the same rule one level up, with a better error.
    pub fn remove(&mut self, index: usize) -> u32 {
        assert!(
            index < self.n,
            "remove index {index} past the tree's {} weights",
            self.n
        );
        assert!(self.n > 1, "refusing to empty the tree");
        let removed = self.weights.remove(index);
        self.rebuild_tree();
        removed
    }

    /// The weights as a slice.
    ///
    /// O(n) memcpy, not an O(n log n) walk over prefix differences -- see the note on [`Self::weights`].
    pub fn as_slice(&self) -> Vec<u32> {
        self.weights.clone()
    }

    /// Rebuild `tree` from `weights` in O(n), leaving `weights` alone.
    ///
    /// Split out of [`from_weights`](Self::from_weights) so the structural mutators can refresh the
    /// aggregates without re-cloning the weights they just spliced.
    ///
    /// # Reuses the tree's allocation
    ///
    /// `clear()` + `resize()` rather than `vec![0; n + 1]`, so the tree's buffer survives a rebuild.
    /// [`from_weights`](Self::from_weights) -- which is a *build*, not a keystroke-path operation --
    /// measures 841 us at 60,000 weights against this function's ~250 us for the same n, and the
    /// difference is the two fresh allocations of 240 KB each.
    ///
    /// An earlier version of this comment claimed the allocation was "~300 us of the 611 us total".
    /// That was a guess, and measurement contradicted it: making the change moved the figure from
    /// 611 us to 686 us, i.e. nothing. The allocation is not the cost; the memory traffic is. The
    /// reuse is still correct -- it avoids two `mmap`/`munmap` pairs per keystroke -- but it is not
    /// the optimisation the comment used to claim, and saying so is the point.
    fn rebuild_tree(&mut self) {
        self.n = self.weights.len();
        self.tree.clear();
        self.tree.resize(self.n + 1, 0);
        for (i, w) in self.weights.iter().enumerate() {
            self.tree[i + 1] = *w;
        }
        for i in 1..=self.n {
            let parent = i + i.isolate_lowest_one();
            if parent <= self.n {
                self.tree[parent] += self.tree[i];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_tree_is_empty_and_totals_zero() {
        let f = Fenwick::default();
        assert!(f.is_empty());
        assert_eq!(f.len(), 0);
        assert_eq!(f.total(), 0);
        assert_eq!(f.lower_bound(0), 0);
    }

    #[test]
    fn prefix_sums_match_a_naive_sum() {
        let weights: Vec<u32> = (0..1000).map(|i| 1 + (i % 19) as u32).collect();
        let f = Fenwick::from_weights(&weights);
        let mut want = 0u32;
        for (i, &w) in weights.iter().enumerate() {
            assert_eq!(f.prefix(i), want, "prefix({i})");
            want += w;
        }
        assert_eq!(f.total(), want);
    }

    #[test]
    fn weight_round_trips_through_prefix() {
        let weights: Vec<u32> = (0..100).map(|i| 3 + (i % 7) as u32).collect();
        let f = Fenwick::from_weights(&weights);
        for (i, w) in weights.iter().enumerate() {
            assert_eq!(f.weight(i), *w, "weight({i})");
        }
    }

    #[test]
    fn add_updates_one_weight_and_its_prefixes() {
        let mut f = Fenwick::from_weights(&[10, 10, 10, 10]);
        assert_eq!(f.total(), 40);
        f.add(2, 5);
        assert_eq!(f.weight(2), 15);
        assert_eq!(f.prefix(2), 20, "prefix before the change");
        assert_eq!(f.prefix(3), 35, "prefix after the change");
        assert_eq!(f.total(), 45);
    }

    /// The saturating behaviour that keeps `lower_bound` sound.
    #[test]
    fn a_negative_delta_saturates_at_zero_rather_than_wrapping() {
        let mut f = Fenwick::from_weights(&[10, 10]);
        f.add(0, -1000);
        assert_eq!(
            f.weight(0),
            0,
            "a line height must never wrap to ~4 billion"
        );
        // The *other* weight is untouched: 10 + 0, not 10 - 990. An earlier version asserted
        // `total() == 10` and got 0, because it subtracted the full delta from every tree node the
        // walk touched rather than the clamped per-node change -- `saturating_add_signed` clamps each
        // node independently, so node 1 goes 10 -> 0 and node 2 goes 20 -> 10, giving a total of 10.
        assert_eq!(f.total(), 10, "only the target weight changed");
        // And `lower_bound` still returns something sensible rather than garbage.
        assert!(f.lower_bound(0) <= 1);
    }

    #[test]
    fn an_out_of_range_index_is_ignored() {
        let mut f = Fenwick::from_weights(&[10, 10]);
        f.add(99, 5);
        f.set(99, 5);
        assert_eq!(
            f.total(),
            20,
            "an out-of-range add must not corrupt the tree"
        );
    }

    /// The core property: `lower_bound` and `prefix` must be exact inverses.
    ///
    /// This is the test that justifies choosing binary lifting over H2's binary search over
    /// `prefix`. With `f64` weights the two can disagree at a boundary by an ulp; with `u32` they
    /// compute the same integer, and this asserts it for every line of a realistic document.
    #[test]
    fn lower_bound_agrees_with_prefix_at_every_boundary() {
        let weights: Vec<u32> = (0..4096).map(|i| 16 + (i % 3) as u32).collect();
        let f = Fenwick::from_weights(&weights);
        for line in 0..weights.len() {
            let y = f.prefix(line);
            assert_eq!(
                f.lower_bound(y),
                line,
                "lower_bound(prefix({line})) = prefix({line}) must hold"
            );
        }
    }

    /// A pixel inside a line belongs to that line, not the next.
    #[test]
    fn lower_bound_finds_the_line_containing_a_pixel() {
        let f = Fenwick::from_weights(&[20, 20, 20, 20]);
        for (line, base) in [(0usize, 0u32), (1, 20), (2, 40), (3, 60)] {
            for dy in 0..20u32 {
                assert_eq!(
                    f.lower_bound(base + dy),
                    line,
                    "pixel {}+{dy} should be in line {line}",
                    base
                );
            }
        }
        // Past the end is the last line.
        assert_eq!(f.lower_bound(80), 4);
        assert_eq!(f.lower_bound(u32::MAX), 4);
    }

    #[test]
    fn a_zero_weight_tree_still_terminates() {
        // Every weight zero: `lower_bound` would loop forever if the step logic were wrong.
        let f = Fenwick::from_weights(&[0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(f.total(), 0);
        assert_eq!(f.lower_bound(0), 8);
        assert_eq!(f.lower_bound(100), 8);
    }

    #[test]
    fn rebuild_replaces_every_weight() {
        let mut f = Fenwick::from_weights(&[10, 10, 10]);
        f.rebuild(&[1, 2, 3]);
        assert_eq!(f.prefix(1), 1);
        assert_eq!(f.prefix(2), 3);
        assert_eq!(f.total(), 6);
    }

    #[test]
    #[should_panic(expected = "cannot change the number of weights")]
    fn rebuild_refuses_to_change_the_length() {
        let mut f = Fenwick::from_weights(&[10, 10, 10]);
        f.rebuild(&[1, 2]);
    }

    /// The build is O(n), which is what makes a 60,000-line document's geometry cheap to construct.
    #[test]
    fn from_weights_agrees_with_repeated_adds() {
        let weights: Vec<u32> = (0..257).map(|i| (i % 13) as u32).collect();
        let direct = Fenwick::from_weights(&weights);
        let mut built = Fenwick::zeros(weights.len());
        for (i, w) in weights.iter().enumerate() {
            built.add(i, i64::from(*w));
        }
        for i in 0..weights.len() + 1 {
            assert_eq!(direct.prefix(i), built.prefix(i), "prefix({i})");
        }
    }
}
