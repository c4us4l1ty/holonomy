//! The Fenwick height model: how the Eye finds a viewport without reading the document.
//!
//! RFC-004 decision 3 says pagination is layout, never text, and decision 2 says shaping cost is
//! invariant in document length. Both rest on the same object: a per-leaf height index maintained
//! *above* the document, so the Eye can answer "which leaves are on screen" in O(log n) reads
//! instead of measuring the document to find out.
//!
//! # What a leaf is
//!
//! A leaf is Block 4's resident slab: the unit the Spine already tracks, splits and re-faults. The
//! Eye does not invent a second partition of the document — it keeps one height per leaf, and a
//! leaf's height is whatever its text measured at the current wrap width and styles.
//!
//! # Where the heights come from, and why zero is allowed
//!
//! Heights are measured, not known: the first time a leaf is laid out the Eye learns its height and
//! writes it back with [`HeightModel::set`]. Until then its height is zero, which is why zero is a
//! legal value and not a bug — [`HeightModel::leaf_at`] simply steps over unmeasured leaves rather
//! than claiming they occupy space. A caller that gets a window containing unmeasured leaves knows
//! its answer is provisional and must widen the window (or shape the leaves) before trusting the
//! scroll geometry.
//!
//! # Cost, measured
//!
//! `prefix`, `set`, `leaf_at` and `window` read O(log n) cells of the index. The model counts those
//! reads in [`HeightModel::reads`], so the cost claim is a number a test can assert rather than a
//! complexity class a comment can assert — the same discipline the allocator gate uses. The counter
//! is a relaxed atomic, so the cost is a few nanoseconds per operation and a model can still be
//! shared across threads.
//!
//! Wall-clock timing here would measure the machine; reads measure the algorithm. The Block 5 gate
//! is about the algorithm, so that is what is counted.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::geom::Fixed;

/// A contiguous run of leaves covering a vertical range of the document.
///
/// `first` and `end` are leaf indices, `end` exclusive; `top` and `bottom` are the document
/// coordinates of those two edges, so a scrolled viewport can be described without re-reading the
/// index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    first: usize,
    end: usize,
    top: Fixed,
    bottom: Fixed,
}

impl Window {
    /// The index of the first leaf in the window.
    #[must_use]
    pub fn first(&self) -> usize {
        self.first
    }

    /// One past the index of the last leaf in the window.
    #[must_use]
    pub fn end(&self) -> usize {
        self.end
    }

    /// The leaves in the window, as a range.
    #[must_use]
    pub fn leaves(&self) -> core::ops::Range<usize> {
        self.first..self.end
    }

    /// The document coordinate of the window's top edge.
    #[must_use]
    pub fn top(&self) -> Fixed {
        self.top
    }

    /// The document coordinate of the window's bottom edge.
    #[must_use]
    pub fn bottom(&self) -> Fixed {
        self.bottom
    }

    /// How many leaves the window holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.end - self.first
    }

    /// Whether the window holds no leaves — past the end of the document, or a model with none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.first == self.end
    }

    /// Whether `leaf` is in the window.
    #[must_use]
    pub fn contains(&self, leaf: usize) -> bool {
        self.leaves().contains(&leaf)
    }
}

/// One measured height per leaf, with a Fenwick tree over them.
///
/// `heights[leaf]` is the leaf's measured height, or zero while it is unmeasured. `tree` is the
/// standard 1-indexed Fenwick array, so a prefix sum is O(log n) additions of whole leaves.
#[derive(Debug)]
pub struct HeightModel {
    heights: Vec<Fixed>,
    tree: Vec<Fixed>,
    reads: AtomicU64,
}

impl HeightModel {
    /// A model of `leaves` leaves, every height zero (nothing measured yet).
    #[must_use]
    pub fn new(leaves: usize) -> Self {
        let mut tree = Vec::with_capacity(leaves + 1);
        tree.push(Fixed::ZERO);
        tree.resize(leaves + 1, Fixed::ZERO);
        Self {
            heights: vec![Fixed::ZERO; leaves],
            tree,
            reads: AtomicU64::new(0),
        }
    }

    /// How many leaves the model covers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.heights.len()
    }

    /// Whether the model covers no leaves at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.heights.is_empty()
    }

    /// How many index cells this model has read since it was created.
    ///
    /// This is the measured cost of the window search: O(log n) per call, and independent of how
    /// much text the leaves hold.
    #[must_use]
    pub fn reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }

    /// Forgets the read count, so a caller can measure one call at a time.
    pub fn reset_reads(&self) {
        self.reads.store(0, Ordering::Relaxed);
    }

    /// The height recorded for `leaf`, or zero while it is unmeasured.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `leaf` is not a leaf of this model.
    #[must_use]
    pub fn height(&self, leaf: usize) -> Fixed {
        debug_assert!(leaf < self.heights.len(), "leaf out of range");
        self.heights[leaf]
    }

    /// Records `height` for `leaf`, O(log n).
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `leaf` is not a leaf of this model.
    pub fn set(&mut self, leaf: usize, height: Fixed) {
        debug_assert!(leaf < self.heights.len(), "leaf out of range");
        debug_assert!(!height.sixtyfourths().is_negative(), "heights are not negative");
        let delta = height - self.heights[leaf];
        self.heights[leaf] = height;
        let mut cell = leaf + 1;
        while cell < self.tree.len() {
            self.tree[cell] += delta;
            cell += cell & cell.wrapping_neg();
        }
    }

    /// The total height of the document, O(log n).
    #[must_use]
    pub fn total(&self) -> Fixed {
        self.prefix(self.heights.len())
    }

    /// The summed height of the leaves before `leaf`, O(log n).
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `leaf` is greater than the number of leaves.
    #[must_use]
    pub fn prefix(&self, leaf: usize) -> Fixed {
        debug_assert!(leaf <= self.heights.len(), "prefix past the model");
        let mut sum = Fixed::ZERO;
        let mut cell = leaf;
        while cell > 0 {
            sum += self.read_cell(cell);
            cell -= cell & cell.wrapping_neg();
        }
        sum
    }

    /// The leaf that contains document coordinate `y`, and how far into it `y` falls.
    ///
    /// Unmeasured (zero-height) leaves occupy no space, so this steps over them: the answer is the
    /// leaf that would actually be drawn at `y`. `None` when `y` is negative, or at or past the
    /// total height.
    #[must_use]
    pub fn leaf_at(&self, y: Fixed) -> Option<(usize, Fixed)> {
        if y < Fixed::ZERO {
            return None;
        }
        let mut index = 0;
        let mut remaining = y;
        let mut step = highest_power_of_two(self.heights.len());
        while step > 0 {
            let candidate = index + step;
            if candidate <= self.heights.len() {
                let covered = self.read_cell(candidate);
                if covered <= remaining {
                    remaining -= covered;
                    index = candidate;
                }
            }
            step >>= 1;
        }
        if index >= self.heights.len() {
            return None;
        }
        Some((index, remaining))
    }

    /// The window of leaves covering `[top, top + height)`.
    ///
    /// A zero or negative `height` yields the single leaf containing `top`. A `top` outside the
    /// document is clamped: negative behaves like zero, past the end yields an empty window at the
    /// document's end. The result is provisional while it contains unmeasured leaves — see the
    /// module docs.
    #[must_use]
    pub fn window(&self, top: Fixed, height: Fixed) -> Window {
        let leaves = self.heights.len();
        let total = self.total();
        if leaves == 0 {
            return Self::empty_window(Fixed::ZERO);
        }
        let start = if top < Fixed::ZERO { Fixed::ZERO } else { top };
        let Some((first, _)) = self.leaf_at(start) else {
            return Self::empty_window(total);
        };
        let end = if height <= Fixed::ZERO {
            first + 1
        } else {
            let stop = start + height;
            match self.leaf_before(stop) {
                Some(leaf) => leaf + 1,
                None => leaves,
            }
        };
        let end = end.clamp(first + 1, leaves);
        Window {
            first,
            end,
            top: self.prefix(first),
            bottom: self.prefix(end),
        }
    }

    /// The largest leaf index whose top edge is strictly before `y`, O(log n).
    fn leaf_before(&self, y: Fixed) -> Option<usize> {
        if y <= Fixed::ZERO {
            return None;
        }
        let mut index = 0;
        let mut remaining = y;
        let mut step = highest_power_of_two(self.heights.len());
        while step > 0 {
            let candidate = index + step;
            if candidate <= self.heights.len() {
                let covered = self.read_cell(candidate);
                if covered < remaining {
                    remaining -= covered;
                    index = candidate;
                }
            }
            step >>= 1;
        }
        Some(index)
    }

    fn read_cell(&self, cell: usize) -> Fixed {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.tree[cell]
    }

    fn empty_window(at: Fixed) -> Window {
        Window {
            first: 0,
            end: 0,
            top: at,
            bottom: at,
        }
    }
}

/// The largest power of two that is at most `n`; `0` for `n == 0`.
fn highest_power_of_two(n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    1usize << (usize::BITS - 1 - n.leading_zeros())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(heights: &[i64]) -> HeightModel {
        let mut model = HeightModel::new(heights.len());
        for (leaf, height) in heights.iter().enumerate() {
            model.set(leaf, Fixed::units(*height));
        }
        model
    }

    /// The leaf containing `y`, by scanning; `None` when `y` is outside the document.
    fn naive_leaf_at(heights: &[i64], y: Fixed) -> Option<usize> {
        if y < Fixed::ZERO {
            return None;
        }
        let mut acc = Fixed::ZERO;
        for (leaf, height) in heights.iter().enumerate() {
            let next = acc + Fixed::units(*height);
            if y < next {
                return Some(leaf);
            }
            acc = next;
        }
        None
    }

    /// The largest leaf index whose top edge is strictly before `y`, by scanning.
    fn naive_leaf_before(heights: &[i64], y: Fixed) -> Option<usize> {
        if y <= Fixed::ZERO {
            return None;
        }
        let mut acc = Fixed::ZERO;
        let mut answer = None;
        for (leaf, height) in heights.iter().enumerate() {
            if acc < y {
                answer = Some(leaf);
            }
            acc += Fixed::units(*height);
        }
        answer
    }

    #[test]
    fn prefix_is_the_sum_of_the_leaves_before() {
        let model = model(&[3, 0, 7, 1]);
        assert_eq!(model.prefix(0), Fixed::ZERO);
        assert_eq!(model.prefix(1), Fixed::units(3));
        assert_eq!(model.prefix(2), Fixed::units(3));
        assert_eq!(model.prefix(3), Fixed::units(10));
        assert_eq!(model.prefix(4), Fixed::units(11));
        assert_eq!(model.total(), Fixed::units(11));
        assert_eq!(model.height(2), Fixed::units(7));
    }

    #[test]
    fn set_updates_prefixes_and_totals() {
        let mut model = HeightModel::new(4);
        assert_eq!(model.total(), Fixed::ZERO);
        model.set(2, Fixed::units(5));
        assert_eq!(model.total(), Fixed::units(5));
        assert_eq!(model.prefix(2), Fixed::ZERO);
        assert_eq!(model.prefix(3), Fixed::units(5));
        // Re-measuring a leaf replaces its height rather than adding to it.
        model.set(2, Fixed::units(2));
        assert_eq!(model.total(), Fixed::units(2));
        model.set(2, Fixed::ZERO);
        assert_eq!(model.total(), Fixed::ZERO);
        assert_eq!(model.len(), 4);
        assert!(!model.is_empty());
    }

    #[test]
    fn leaf_at_matches_a_scan_and_steps_over_unmeasured_leaves() {
        let heights = [4, 0, 0, 6, 2];
        let model = model(&heights);
        for y in 0..12 {
            let point = Fixed::units(y);
            let expected = naive_leaf_at(&heights, point);
            let found = model.leaf_at(point);
            assert_eq!(found.map(|(leaf, _)| leaf), expected, "at {y}");
        }
        // The zero-height leaves occupy no space: y = 4 is the fourth leaf, not the second.
        let at_four = model.leaf_at(Fixed::units(4)).expect("inside the document");
        assert_eq!(at_four.0, 3);
        assert_eq!(at_four.1, Fixed::ZERO);
        // A point inside the fourth leaf reports its offset into that leaf.
        let inside = model.leaf_at(Fixed::units(7)).expect("inside the document");
        assert_eq!(inside.0, 3);
        assert_eq!(inside.1, Fixed::units(3));
        // Past the end, before the start.
        assert!(model.leaf_at(Fixed::units(12)).is_none());
        assert!(model.leaf_at(Fixed::from_64ths(-1)).is_none());
    }

    #[test]
    fn leaf_before_matches_a_scan() {
        let heights = [4, 0, 0, 6, 2];
        let model = model(&heights);
        for y in -1..14 {
            let point = Fixed::units(y);
            assert_eq!(model.leaf_before(point), naive_leaf_before(&heights, point), "at {y}");
        }
    }

    #[test]
    fn windows_cover_the_requested_range() {
        let model = model(&[4, 4, 4, 4]);
        let window = model.window(Fixed::units(2), Fixed::units(4));
        assert_eq!(window.leaves(), 0..2);
        assert_eq!(window.top(), Fixed::ZERO);
        assert_eq!(window.bottom(), Fixed::units(8));
        assert_eq!(window.len(), 2);
        assert!(window.contains(1));
        assert!(!window.contains(2));

        let second = model.window(Fixed::units(5), Fixed::units(2));
        assert_eq!(second.leaves(), 1..2);
        assert_eq!(second.top(), Fixed::units(4));
        assert_eq!(second.bottom(), Fixed::units(8));
    }

    #[test]
    fn windows_clamp_at_the_edges() {
        let model = model(&[4, 4]);
        let below = model.window(Fixed::units(-5), Fixed::units(2));
        assert_eq!(below.leaves(), 0..1);
        let past = model.window(Fixed::units(20), Fixed::units(2));
        assert!(past.is_empty());
        assert_eq!(past.top(), Fixed::units(8));
        // A viewport with no height still names the leaf its top edge is in.
        let thin = model.window(Fixed::units(4), Fixed::ZERO);
        assert_eq!(thin.leaves(), 1..2);
        let empty = HeightModel::new(0);
        assert!(empty.is_empty());
        assert!(empty.window(Fixed::ZERO, Fixed::units(4)).is_empty());
        assert_eq!(empty.total(), Fixed::ZERO);
    }

    #[test]
    fn a_window_over_a_tall_leaf_is_one_leaf() {
        let model = model(&[2, 40, 2]);
        let window = model.window(Fixed::units(3), Fixed::units(4));
        assert_eq!(window.leaves(), 1..2);
        assert_eq!(window.bottom() - window.top(), Fixed::units(40));
    }

    #[test]
    fn the_search_cost_is_logarithmic_in_the_number_of_leaves() {
        // The claim Block 5's second gate needs: finding a viewport costs reads proportional to
        // log(leaves), not to the document. Uniform heights keep the leaves' content irrelevant.
        let mut reads = Vec::new();
        for exponent in [10u32, 14, 18] {
            let leaves = 1usize << exponent;
            let mut model = HeightModel::new(leaves);
            for leaf in 0..leaves {
                model.set(leaf, Fixed::units(1));
            }
            let middle = Fixed::units((leaves as i64) / 2);
            model.reset_reads();
            let window = model.window(middle, Fixed::units(20));
            let measured = model.reads();
            assert_eq!(window.len(), 20);
            // log2(leaves) = exponent; a window is two searches and two prefixes.
            let bound = 8 * u64::from(exponent) + 16;
            assert!(measured <= bound, "{measured} reads for 2^{exponent} leaves");
            reads.push(measured);
        }
        // Sixteen times the leaves costs less than twice the reads: the work tracks log(leaves),
        // not leaves.
        assert!(reads[2] < 2 * reads[0], "reads grew from {reads:?}");
        assert!(reads[1] <= reads[2]);
    }

    #[test]
    fn the_window_agrees_with_a_scan_over_random_heights() {
        let state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for round in 0..32u64 {
            let leaves = 1 + (next() % 24) as usize;
            let mut heights = Vec::new();
            for _ in 0..leaves {
                heights.push((next() % 6) as i64);
            }
            let model = model(&heights);
            let total = model.total();
            for _ in 0..8 {
                let top = Fixed::from_64ths((next() % 400) as i64);
                let height = Fixed::from_64ths((next() % 120) as i64);
                let window = model.window(top, height);
                let start = if top < Fixed::ZERO { Fixed::ZERO } else { top };
                let expected_first = if start >= total {
                    None
                } else {
                    naive_leaf_at(&heights, start)
                };
                match expected_first {
                    None => assert!(window.is_empty(), "round {round}"),
                    Some(first) => {
                        assert_eq!(window.first(), first, "round {round} top {top:?}");
                        assert_eq!(window.top(), model.prefix(first));
                        let expected_end = if height <= Fixed::ZERO {
                            first + 1
                        } else {
                            match naive_leaf_before(&heights, start + height) {
                                Some(leaf) => leaf + 1,
                                None => leaves,
                            }
                        };
                        assert_eq!(window.end(), expected_end.min(leaves), "round {round}");
                        assert_eq!(window.bottom(), model.prefix(window.end()));
                    }
                }
            }
        }
    }
}
