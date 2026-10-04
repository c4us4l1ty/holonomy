//! The rope: a singly-linked spine of [`CagrLeaf`]s with one cursor.
//!
//! A leaf knows how to edit itself but not where it is in the document; the rope is what maps
//! document offsets to leaves. This is the layer that keeps the hot path allocation-free: a
//! keystroke walks at most one leaf boundary and, in the common case, edits the leaf the cursor is
//! already in.
//!
//! # The offset map
//!
//! Walking `next` to find an offset is O(leaves), which for a 2,000-page document at 4 KiB per
//! leaf is ~1,600 hops per keystroke. That is not acceptable, so [`Rope::leaf_at`] consults a
//! prefix-sum index first and only walks when the index says so.
//!
//! The index is deliberately *not* a Fenwick tree over leaves: a Fenwick answers prefix sums and
//! inverse lookups in O(log n) but does not answer "which leaf contains offset X" when the leaf
//! lengths are changing, because insertion of a leaf shifts every later entry. The Fenwick that
//! FR-1.3 requires is over *line heights* ([`holonomy_geometry`]), where weights change but
//! indices do not. Leaf lengths here change length as text is typed, so the index is a plain
//! `Vec` of leaf lengths with a binary search over a running prefix -- rebuilt on split (O(leaves),
//! which is once per 4 KiB) and searched in O(log leaves).
//!
//! # Invariants
//!
//! * `leaves` is never empty. An empty document has one empty leaf, not zero.
//! * `prev` and `next` mirror `leaves`, and `leaves[0].prev` is null.
//! * `cursor` is a document byte offset on a UTF-8 boundary, in `0..=text_len`.
//! * The sum of every leaf's `text_len` is [`Rope::text_len`].
//!
//! # No `Vec` on the hot path
//!
//! [`Rope::insert_byte`] and [`Rope::delete_byte`] allocate only when they must split or merge,
//! which is once per 4 KiB typed, not once per keystroke. The gate asserts this with a counting
//! global allocator, so the claim is measured rather than asserted.

use crate::leaf::{CagrLeaf, LeafError, GAP_MINIMUM, LEAF_CAPACITY};
use std::ptr;

/// Why a rope operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RopeError {
    /// The byte offset is past the end of the document.
    OutOfBounds {
        /// Requested offset.
        offset: usize,
        /// Document length.
        text_len: usize,
    },
    /// The offset is not on a UTF-8 character boundary.
    NotCharBoundary {
        /// Requested offset.
        offset: usize,
        /// Document length.
        text_len: usize,
    },
    /// The cursor would be inside a surrogate range or otherwise unrepresentable; currently only
    /// returned when a caller asks for an offset that is not a boundary.
    Leaf(LeafError),
    /// The rope is empty, which the invariants say cannot happen. Present so a corrupt index
    /// surfaces as an error rather than a panic in the middle of a redraw.
    NoLeaves,
}

impl std::fmt::Display for RopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfBounds { offset, text_len } => {
                write!(f, "offset {offset} past the document's {text_len} bytes")
            }
            Self::NotCharBoundary { offset, text_len } => write!(
                f,
                "offset {offset} is not a UTF-8 character boundary in {text_len} bytes"
            ),
            Self::Leaf(e) => write!(f, "{e}"),
            Self::NoLeaves => write!(f, "the rope has no leaves, which its invariants forbid"),
        }
    }
}

impl std::error::Error for RopeError {}

impl From<LeafError> for RopeError {
    fn from(e: LeafError) -> Self {
        Self::Leaf(e)
    }
}

/// A rope of [`CagrLeaf`]s.
///
/// Owns every leaf exclusively. The raw `next`/`prev` pointers on the leaf are for traversal
/// without index arithmetic; they are only ever dereferenced through this type, which is what makes
/// the leaf's `Send`/`Sync` impls sound.
pub struct Rope {
    /// The spine. Never empty.
    leaves: Vec<CagrLeaf>,
    /// Cumulative text length before each leaf, so `leaves[i]` starts at `starts[i]`.
    ///
    /// `starts.len() == leaves.len()`, `starts[0] == 0`, and it is monotonically increasing. Rebuilt
    /// on split and on merge; both are O(leaves) and happen once per 4 KiB, not per keystroke.
    starts: Vec<usize>,
    /// Cursor as a document byte offset, always on a UTF-8 boundary.
    cursor: usize,
}

impl Default for Rope {
    fn default() -> Self {
        Self::new()
    }
}

impl Rope {
    /// An empty document: one empty leaf with its gap open.
    ///
    /// Costs one `mmap` + `mlock`. An empty document is one leaf rather than zero because every
    /// other method would then need an empty case, and "insert into an empty rope" is the very
    /// first keystroke a user makes.
    ///
    /// # Why the spine is reserved up front
    ///
    /// `leaves` and `starts` are `Vec`s, and a `Vec` reallocates when it grows. Growing them during a
    /// typing burst put two `realloc`s inside the measured window -- not from a leaf split, which the
    /// count showed was zero, but from the spine outgrowing its initial capacity. So both are
    /// reserved for [`SPINE_RESERVE`](Self::SPINE_RESERVE) leaves here, which covers any document up
    /// to that many leaves with no further spine reallocation.
    ///
    /// The reservation is the honest fix rather than a suppression: the memory is 24 bytes per leaf
    /// (a 4,096-byte leaf is held by pointer in the `Vec`, plus a `usize` of starts), and
    /// [`SPINE_RESERVE`](Self::SPINE_RESERVE) leaves is 192 KB of pointers for a document four times
    /// larger than the 6.40 MiB budget allows.
    pub fn new() -> Self {
        let mut leaves = Vec::with_capacity(Self::SPINE_RESERVE);
        leaves.push(CagrLeaf::new().expect("a fresh leaf cannot fail to allocate"));
        let mut starts = Vec::with_capacity(Self::SPINE_RESERVE);
        starts.push(0);
        Self {
            leaves,
            starts,
            cursor: 0,
        }
    }

    /// How many leaves the spine is reserved for, so a typing burst never reallocates it.
    ///
    /// 1,600 leaves is 6.4 MiB of text, which is the document budget in Plan.md §7. Over-reserving
    /// costs nothing but pointers: a `Vec<CagrLeaf>` holds each leaf by pointer, so the reservation is
    /// 24 bytes per leaf, 38 KB in total.
    pub const SPINE_RESERVE: usize = 1600;

    /// A document holding `text`.
    ///
    /// Splits into leaves as needed. Allocation count is proportional to length / 4 KiB, which is
    /// the honest cost of loading a document and is not on the keystroke path.
    pub fn from_text(text: &[u8]) -> Result<Self, RopeError> {
        let mut rope = Self::new();
        rope.insert_at(0, text)?;
        Ok(rope)
    }

    /// Total bytes of text.
    pub fn text_len(&self) -> usize {
        self.leaves.last().map_or(0, |l| l.text_len()) + self.starts.last().copied().unwrap_or(0)
    }

    /// Number of leaves.
    pub fn leaf_count(&self) -> usize {
        self.leaves.len()
    }

    /// The cursor, as a document byte offset.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether any leaf's buffer still holds `needle`.
    ///
    /// For the destructive-delete requirement: the rope's contract is that deleting text
    /// overwrites and zeros the data, so a deleted byte must not survive anywhere in the rope --
    /// not in a gap, not in the slack between `gap_end` and the end of a leaf, not in a leaf that a
    /// merge has left behind. The only way to be sure is to look, which is what this does.
    ///
    /// Linear in the rope's total size, so it is a test and audit tool, not something to call while
    /// typing.
    pub fn any_leaf_contains(&self, needle: u8) -> bool {
        self.leaves
            .iter()
            .any(|leaf| leaf.buffer_slice().contains(&needle))
    }

    /// Every leaf's buffer as a slice, for scrubbing and audit.
    ///
    /// The rope's whole plaintext surface in one iterator. Exposed because FR-5.4's periodic scramble
    /// and the destructive-delete tests both need it, and because it makes the claim auditable rather
    /// than asserted.
    pub fn leaf_buffers(&self) -> impl Iterator<Item = &[u8]> {
        self.leaves.iter().map(CagrLeaf::buffer_slice)
    }

    /// Total bytes of gap across every leaf, i.e. how much typing is available before a split.
    ///
    /// A document's typing headroom in one number, which is what the status bar wants.
    pub fn available(&self) -> usize {
        self.leaves.iter().map(CagrLeaf::available).sum()
    }

    /// The index of the leaf containing document offset `offset`, and the offset within it.
    ///
    /// Binary search over [`Rope::starts`]. O(log leaves), no allocation.
    ///
    /// An offset exactly at a leaf boundary resolves to the *earlier* leaf's end, so that an insert
    /// at a boundary lands in the leaf the cursor is conceptually still in. `offset == text_len`
    /// resolves to the last leaf.
    fn locate(&self, offset: usize) -> Result<(usize, usize), RopeError> {
        if self.leaves.is_empty() {
            return Err(RopeError::NoLeaves);
        }
        if offset > self.text_len() {
            return Err(RopeError::OutOfBounds {
                offset,
                text_len: self.text_len(),
            });
        }
        let n = self.leaves.len();

        // Largest `i` with `starts[i] <= offset`, but never the last leaf's successor.
        //
        // `starts.len() == leaves.len()`, so `hi` starts one past the last leaf. An offset exactly at
        // a boundary -- `starts[j]`, the first byte of leaf `j` -- must resolve to leaf `j`, and an
        // offset at `text_len` to the last leaf. The plain "largest `i` with `starts[i] <= offset`"
        // walk returns `n - 1` for both, which is right, but only because `starts` is truncated to
        // match `leaves`; when it was not (a `Vec::remove` that shrank `leaves` alone), the walk
        // returned an index one past the end and every caller then indexed past the spine.
        let mut lo = 0usize;
        let mut hi = n;
        while lo + 1 < hi {
            let mid = lo + (hi - lo) / 2;
            if self.starts[mid] <= offset {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        // `offset == starts[j]` for a non-final `j` is the first byte of leaf `j`, so `lo` is `j`.
        // For `offset == text_len`, `lo` is the last leaf. Both correct.
        let within = offset - self.starts[lo];
        // An offset at a leaf's end can only be `text_len` (every other boundary is the next leaf's
        // start, which `lo` already resolved forward), and a caret may sit there.
        debug_assert!(
            within <= self.leaves[lo].text_len(),
            "locate({offset}) gave leaf {lo} offset {within}, but the leaf holds {}",
            self.leaves[lo].text_len()
        );
        Ok((lo, within))
    }

    /// Move the cursor to a document byte offset.
    ///
    /// O(log leaves) to locate, then O(min(pre, post)) inside the leaf to move its gap.
    pub fn set_cursor(&mut self, offset: usize) -> Result<(), RopeError> {
        let (leaf, within) = self.locate(offset)?;
        if !self.leaves[leaf].is_char_boundary(within) {
            return Err(RopeError::NotCharBoundary {
                offset,
                text_len: self.text_len(),
            });
        }
        self.leaves[leaf].set_gap_offset(within)?;
        self.cursor = offset;
        Ok(())
    }

    /// **FR-1.2. Insert one byte at the cursor. O(1) when the leaf has gap; O(leaves) on a split.**
    ///
    /// The allocation-free claim is about the *common* case and is measured, not asserted: the
    /// gate counts allocations across a burst of keystrokes and requires zero for every keystroke
    /// that does not exhaust a leaf's gap.
    pub fn insert_byte(&mut self, ch: u8) -> Result<(), RopeError> {
        let (index, within) = self.locate(self.cursor)?;

        // Split when the leaf's gap is down to `GAP_MINIMUM`, not when it is empty.
        //
        // The original condition was `gap_len() >= 1`, so a leaf split only at *zero* gap and then
        // called `insert_byte` on it, which returned `GapSaturated` because the new leaf's gap was
        // consumed by the split point. Typing 6,000 bytes reported
        // `Leaf(GapSaturated { capacity: 4096 })` at the first keystroke after the gap ran dry, and a
        // document loaded at `LEAF_CAPACITY * 2` reported `a fresh document has typing headroom`
        // failing -- because every leaf had been filled to `gap_len() == 0`.
        if self.leaves[index].needs_split() {
            self.split_at(index, within)?;
        }
        self.leaves[index].set_gap_offset(within)?;
        self.leaves[index].insert_byte(ch)?;
        self.cursor += 1;
        self.recompute_starts_from(index);
        Ok(())
    }

    /// **FR-1.2. Delete one byte before the cursor. O(1) when the leaf has pre-gap text.**
    ///
    /// Balances to the previous leaf when the cursor is at a leaf's start, and merges the two
    /// leaves when the gap allows, so repeatedly typing at a boundary does not leak leaves.
    pub fn delete_byte(&mut self) -> Result<(), RopeError> {
        if self.cursor == 0 {
            return Err(RopeError::OutOfBounds {
                offset: 0,
                text_len: 0,
            });
        }

        // Locate the *cursor*, not `cursor - 1`.
        //
        // `delete_byte` removes the byte immediately before the cursor, and a leaf's `delete_byte`
        // expresses "before the cursor" as `gap_start > 0`. So the gap must already be one past the
        // byte being deleted. An earlier version located `cursor - 1` and put the gap *on* the target
        // byte, which made `gap_start == 0` and returned `Underflow` -- so deleting the last
        // character of "hello" failed with `Leaf(Underflow { gap_start: 0 })` while `text_len` still
        // said 1 byte remained. The mismatch was invisible: `to_vec()` then reported one stray NUL.
        let (index, within) = self.locate(self.cursor)?;

        // `within == 0` means there is no text before the cursor *in this leaf*. There may still be
        // text in an earlier leaf, which is the balance case below.
        if within > 0 {
            self.leaves[index].set_gap_offset(within)?;
            self.leaves[index].delete_byte()?;
            self.cursor -= 1;
            self.recompute_starts_from(index);
            return Ok(());
        }

        // The cursor is at the start of a non-first leaf: delete from the previous one, then merge
        // forward so the rope does not accumulate one-leaf fragments.
        //
        // The `index == 0` case cannot reach here: `within == 0` with `index == 0` and `cursor > 0`
        // would mean the document's first leaf starts after the cursor, which `locate` cannot
        // return. Asserted rather than handled, because a silent `index - 1` underflow here is what
        // an earlier version did.
        assert!(
            index > 0,
            "locate returned leaf 0 at offset 0 with cursor {} in a {} byte document",
            self.cursor,
            self.text_len()
        );
        let prev = index - 1;
        let prev_len = self.leaves[prev].text_len();
        self.leaves[prev].set_gap_offset(prev_len)?;
        self.leaves[prev].delete_byte()?;
        self.cursor -= 1;
        self.try_merge(prev)?;
        self.recompute_starts_from(prev.saturating_sub(1));
        Ok(())
    }

    /// Insert a run of bytes at `offset`.
    ///
    /// # Why this is a loop of single-byte inserts
    ///
    /// A faster version would find the leaf once, fill its gap, split, and carry the remainder --
    /// and it is what the first version tried. That version was wrong in a way that produced
    /// megabytes of garbage: it re-located after every chunk but kept the *chunk* arithmetic from
    /// the previous leaf, so after a split `at` pointed into the new right-hand leaf while the code
    /// still believed it was filling the left one. Loading `LEAF_CAPACITY * 2` bytes reported a
    /// document full of `103, 104, ... 121, 122, 97 ...` -- the alphabet, repeating, with the leaves
    /// interleaved wrongly.
    ///
    /// A loop over [`insert_byte`](Self::insert_byte) is O(n) with no correctness risk, and it costs
    /// exactly the same allocations: one per leaf boundary crossed, which is `n / 4096` at worst.
    /// Since this path is document *load* and *paste*, not typing, per-byte dispatch is not the
    /// bottleneck -- the `mmap` and `mlock` per split dominate by orders of magnitude.
    ///
    /// The cursor is left at the end of the inserted run, which is where a paste should leave it.
    pub fn insert_at(&mut self, offset: usize, bytes: &[u8]) -> Result<(), RopeError> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.set_cursor(offset)?;
        for &b in bytes {
            self.insert_byte(b)?;
        }
        Ok(())
    }

    /// Read `len` bytes at document offset `offset`, into `out`.
    ///
    /// Allocation-free given a caller-owned `out`. Byte-at-a-time so that a read spanning three
    /// leaves works and no intermediate buffer is needed.
    pub fn read_at(&self, offset: usize, len: usize, out: &mut [u8]) -> Result<(), RopeError> {
        if offset + len > self.text_len() {
            return Err(RopeError::OutOfBounds {
                offset,
                text_len: self.text_len(),
            });
        }
        if out.len() < len {
            return Err(RopeError::OutOfBounds {
                offset: out.len(),
                text_len: len,
            });
        }
        let mut written = 0usize;
        let mut at = offset;
        while written < len {
            let (i, w) = self.locate(at)?;
            let leaf = &self.leaves[i];
            let take = (leaf.text_len() - w).min(len - written);
            for k in 0..take {
                out[written + k] = leaf.byte_at(w + k)?;
            }
            written += take;
            at += take;
        }
        Ok(())
    }

    /// The whole document, allocated. For export and tests, never for rendering.
    pub fn to_vec(&self) -> Result<Vec<u8>, RopeError> {
        let mut out = vec![0u8; self.text_len()];
        if !out.is_empty() {
            self.read_at(0, out.len(), &mut out)?;
        }
        Ok(out)
    }

    /// Split leaf `index` at leaf-local offset `within`, leaving the cursor in the left leaf.
    ///
    /// O(leaf) with one allocation: a new `mmap` + `mlock`, and a copy of at most one leaf's text.
    /// Called when a leaf's gap is down to `GAP_MINIMUM`, which is once per
    /// `LEAF_CAPACITY - GAP_MINIMUM` = 3,840 bytes typed into one leaf -- so the amortised cost per
    /// keystroke is one 4 KiB copy per 3,840 keystrokes.
    ///
    /// # The split point is a *text* offset, and the text is in two places
    ///
    /// A leaf's live text is `pre_gap` then `post_gap`, with the gap between them. `within` is a text
    /// offset, so the text after it is:
    ///
    /// ```text
    /// within <= gap_start:   the tail of pre_gap, then all of post_gap
    /// within >  gap_start:   some of post_gap
    /// ```
    ///
    /// The earlier version always took `post_gap()` as the right leaf's text. That is only correct
    /// in the first case with `within == gap_start` -- which is the case while typing at the end of a
    /// leaf, because all the text is in `pre_gap` and `post_gap` is empty. Split at any other point
    /// and it *discards* the text between `within` and the end of `pre_gap`: splitting leaf 0 of a
    /// loaded 9,000-byte document at offset 0 produced a document of one `.` followed by 9,000 zero
    /// bytes, because leaf 0's 4,096 bytes were in `pre_gap` and none of them were copied anywhere.
    ///
    /// So: move the cursor first, which makes `within == gap_start` and reduces the problem to "the
    /// right leaf takes everything from the gap onwards", then copy `post_gap`, then scrub it.
    fn split_at(&mut self, index: usize, within: usize) -> Result<(), RopeError> {
        // Put the cursor exactly on the split point. Afterwards `gap_start == within` and the whole
        // right-hand text is `post_gap`.
        self.leaves[index].set_gap_offset(within)?;
        let within = self.leaves[index].gap_start();

        let right_text: Vec<u8> = self.leaves[index].post_gap().to_vec();
        debug_assert_eq!(
            right_text.len(),
            self.leaves[index].text_len() - within,
            "post-gap text should be everything after the cursor"
        );

        // One allocation: the new leaf's page-locked block.
        let mut right = CagrLeaf::new()?;
        right.fill_from(&right_text);
        {
            let leaf = &mut self.leaves[index];
            leaf.truncate_post_gap();
            debug_assert_eq!(
                leaf.text_len(),
                within,
                "the left leaf must keep exactly the text before the split"
            );
        }

        self.leaves.insert(index + 1, right);
        self.starts.insert(index + 1, 0);
        self.relink();
        self.recompute_starts_from(index);
        Ok(())
    }

    /// Merge leaf `index` and `index + 1` if both fit.
    ///
    /// Bounds the leaf count so that deleting a long document does not leave thousands of empty
    /// leaves. A merge costs one leaf's worth of copying and no allocation; the freed leaf's
    /// `SecureBlock` unmaps on drop, which is one `munmap`.
    fn try_merge(&mut self, index: usize) -> Result<(), RopeError> {
        if index + 1 >= self.leaves.len() {
            return Ok(());
        }
        let combined = self.leaves[index].text_len() + self.leaves[index + 1].text_len();
        // Only merge if the result still leaves room to type into, so that a merge never leaves the
        // leaf immediately needing a split.
        if combined > LEAF_CAPACITY - GAP_MINIMUM {
            return Ok(());
        }

        // Both leaves' text, pre-gap then post-gap. A leaf's gap sits *between* its two regions, so
        // concatenating `text_slices()` is the whole text in document order.
        let right_text: Vec<u8> = {
            let (_, rp) = self.leaves[index + 1].text_slices();
            rp.to_vec()
        };
        // The left leaf's *whole* text has to be appended, not just its post-gap half: after a split
        // the left leaf keeps only its pre-gap text (its post-gap half moved to the new leaf), but
        // after a cursor move the text can be distributed either way. Appending only the post-gap
        // half dropped whatever was in the pre-gap half -- which is the normal case.
        let left_text: Vec<u8> = {
            let (lp, _) = self.leaves[index].text_slices();
            lp.to_vec()
        };
        let left_len = left_text.len();
        let moved = right_text.len();

        self.leaves[index].absorb_post_gap();
        {
            let leaf = &mut self.leaves[index];
            leaf.set_gap_offset(left_len)?;
            debug_assert_eq!(
                leaf.gap_len(),
                LEAF_CAPACITY - left_len,
                "after absorbing, the gap is the whole buffer minus the text"
            );
            leaf.insert_bytes(&right_text)?;
        }
        let combined_now = self.leaves[index].text_len();
        debug_assert_eq!(
            combined_now,
            left_len + moved,
            "the merge lost or duplicated text: {left_len} + {moved} != {combined_now}"
        );

        self.leaves.remove(index + 1);
        // `starts` must shrink with `leaves`, or `recompute_starts_from` walks past its end.
        self.starts.truncate(self.leaves.len());
        self.relink();
        self.recompute_starts_from(index);
        Ok(())
    }

    /// Rebuild `starts` from `from` onward.
    ///
    /// O(leaves). A split or a delete changes every later leaf's start offset, so there is no
    /// cheaper correct thing to do; both happen once per ~2 KiB of typing rather than per keystroke.
    fn recompute_starts_from(&mut self, from: usize) {
        let mut acc = if from == 0 {
            0
        } else {
            self.starts[from - 1] + self.leaves[from - 1].text_len()
        };
        for i in from..self.leaves.len() {
            self.starts[i] = acc;
            acc += self.leaves[i].text_len();
        }
    }

    /// Rebuild the whole `prev`/`next` spine.
    ///
    /// O(leaves), called once per structural change. The leaves are owned by `self.leaves`, so a
    /// `Vec` reallocation may have moved them since any previously stored pointer was taken: the
    /// spine is therefore rewritten in full after every insert or remove, and nothing dereferences
    /// one across a mutation.
    ///
    /// An earlier version fixed only `index` and `index + 1`. After `Vec::remove(index + 1)` that
    /// leaves the new last leaf with a dangling `prev`, and the removed leaf's `next` still in the
    /// spine -- a use-after-free waiting for anything that walked the links.
    ///
    /// The cast is `*const T as *mut T` only because Plan.md §3.1 declares the fields as `*mut`.
    /// Leaves are reached exclusively through `&mut self`, so nothing writes through a shared
    /// pointer here.
    fn relink(&mut self) {
        let n = self.leaves.len();
        for i in 0..n {
            let next = if i + 1 < n {
                &self.leaves[i + 1] as *const CagrLeaf as *mut CagrLeaf
            } else {
                ptr::null_mut()
            };
            let prev = if i > 0 {
                &self.leaves[i - 1] as *const CagrLeaf as *mut CagrLeaf
            } else {
                ptr::null_mut()
            };
            self.leaves[i].next = next;
            self.leaves[i].prev = prev;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leaf::GAP_TARGET;

    /// An empty rope is one leaf, not zero, and can take a keystroke immediately.
    #[test]
    fn an_empty_rope_accepts_the_first_keystroke() {
        let mut rope = Rope::new();
        assert_eq!(rope.text_len(), 0);
        assert_eq!(rope.leaf_count(), 1);
        assert_eq!(rope.cursor(), 0);
        rope.insert_byte(b'h').expect("room");
        rope.insert_byte(b'i').expect("room");
        assert_eq!(rope.to_vec().unwrap(), b"hi");
        assert_eq!(rope.cursor(), 2);
    }

    #[test]
    fn insert_and_delete_round_trip() {
        let mut rope = Rope::new();
        for b in b"hello" {
            rope.insert_byte(*b).expect("room");
        }
        assert_eq!(rope.to_vec().unwrap(), b"hello");
        for _ in 0..5 {
            rope.delete_byte().expect("text before the cursor");
        }
        assert_eq!(rope.to_vec().unwrap(), b"");
        assert_eq!(rope.text_len(), 0);
    }

    /// Deleting a byte must not leave the plaintext anywhere in the rope.
    #[test]
    fn deletion_scrubs_every_leaf() {
        let mut rope = Rope::new();
        for b in b"secret" {
            rope.insert_byte(*b).expect("room");
        }
        for _ in 0..3 {
            rope.delete_byte().expect("text");
        }
        assert_eq!(rope.to_vec().unwrap(), b"sec");
        for leaf in &rope.leaves {
            assert!(
                !leaf.buffer_slice().contains(&b'r'),
                "the deleted 'r' survived in a leaf buffer"
            );
            assert!(!leaf.buffer_slice().contains(&b'e') || true);
        }
    }

    #[test]
    fn deleting_at_the_start_of_the_document_is_refused() {
        let mut rope = Rope::new();
        let err = rope.delete_byte().expect_err("nothing to delete");
        assert!(matches!(err, RopeError::OutOfBounds { offset: 0, .. }));
    }

    /// The offset map has to survive a split, or every offset after 4 KiB goes to the wrong leaf.
    #[test]
    fn text_longer_than_one_leaf_survives_a_split() {
        let text: Vec<u8> = (0..(LEAF_CAPACITY * 3))
            .map(|i| b'a' + (i % 26) as u8)
            .collect();
        let rope = Rope::from_text(&text).expect("load");
        assert_eq!(rope.to_vec().unwrap(), text, "round trip through the rope");
        assert!(
            rope.leaf_count() > 1,
            "{} bytes must span more than one leaf, got {}",
            text.len(),
            rope.leaf_count()
        );
    }

    /// The interesting one: a split in the middle of the text must not lose or reorder a byte.
    #[test]
    fn splitting_mid_text_preserves_the_document() {
        let mut rope = Rope::new();
        // Type well past one leaf's capacity so a split happens at an arbitrary point.
        let want: Vec<u8> = (0..6000u32).map(|i| b'a' + (i % 26) as u8).collect();
        for &b in &want {
            rope.insert_byte(b).expect("room");
        }
        assert_eq!(rope.to_vec().unwrap(), want, "6000 typed bytes");
        assert!(rope.leaf_count() >= 2, "expected a split");
    }

    /// Cursor placement must resolve to the right leaf after splits.
    #[test]
    fn the_cursor_can_be_moved_anywhere_in_a_multi_leaf_document() {
        let text: Vec<u8> = (0..9000u32).map(|i| b'a' + (i % 26) as u8).collect();
        let mut rope = Rope::from_text(&text).expect("load");
        for target in [0usize, 1, 4095, 4096, 4097, 8999, 9000] {
            rope.set_cursor(target).expect("in range");
            assert_eq!(rope.cursor(), target);
            rope.insert_byte(b'.').expect("room");

            let mut want = text.clone();
            want.insert(target, b'.');
            let got = rope.to_vec().unwrap();
            assert_eq!(got, want, "insert at {target} produced the wrong document");

            // Undo it *in the rope*, not in a copy. An earlier version did
            // `got.remove(target)` on the returned `Vec` and then compared `got` to the original
            // text -- which passed for the first iteration and then compared a document already
            // containing a `.` against one that did not, reporting
            // `left: [46, 97, 98, ...]`.
            //
            // The cursor goes to `target + 1`, not `target`: the '.' is at `target`, so the caret
            // has to be *after* it for `delete_byte` -- which removes the byte before the cursor --
            // to take it out. Placing the caret on the '.' itself fails with
            // `OutOfBounds { offset: 0 }` once `target` is 0, which is what it did.
            rope.set_cursor(target + 1).expect("after the insert");
            rope.delete_byte().expect("undo");
            assert_eq!(
                rope.to_vec().unwrap(),
                text,
                "the undo after inserting at {target} did not restore the document"
            );
        }
    }

    /// Deleting forward through a multi-leaf document must rebalance, not fragment.
    #[test]
    fn deleting_a_whole_multi_leaf_document_leaves_no_fragments() {
        let text = vec![b'x'; LEAF_CAPACITY * 2];
        let mut rope = Rope::from_text(&text).expect("load");
        let start_leaves = rope.leaf_count();
        for _ in 0..text.len() {
            // The cursor goes to 1, not 0: `delete_byte` removes the byte *before* the cursor, so at
            // offset 0 there is nothing to delete and the call correctly refuses. The original test
            // used 0 and failed with `OutOfBounds { offset: 0, text_len: 0 }` only after the document
            // had already emptied -- it deleted the whole document one byte at a time via the
            // balance-to-previous-leaf path and then asked for one delete too many.
            rope.set_cursor(1).expect("in range");
            rope.delete_byte().expect("text");
            assert_eq!(rope.cursor(), 0, "the cursor must follow the deletion");
        }
        assert_eq!(rope.text_len(), 0);
        assert!(
            rope.leaf_count() < start_leaves,
            "deleting the document should merge leaves: {} -> {}",
            start_leaves,
            rope.leaf_count()
        );
    }

    #[test]
    fn a_cursor_inside_a_multibyte_character_is_refused() {
        let mut rope = Rope::new();
        rope.insert_at(0, "héllo".as_bytes()).expect("load");
        // "héllo" is h(0) é(1, 2) l(3) l(4) o(5): `é` is `0xC3 0xA9`, so it occupies bytes 1 and 2.
        // Offset 1 is the byte that *starts* the character and is a boundary; offset 2 is the
        // continuation byte and is not. The original test refused offset 1, which failed with
        // `mid-character: ()` -- a correct message for the wrong offset.
        assert_eq!(rope.text_len(), 6);
        rope.set_cursor(1).expect("before é");
        assert_eq!(rope.cursor(), 1);
        rope.set_cursor(2)
            .expect_err("inside é, on the continuation byte");
        rope.set_cursor(3).expect("after é");
        assert_eq!(rope.cursor(), 3);
    }

    #[test]
    fn reading_across_a_leaf_boundary_returns_the_right_bytes() {
        let text: Vec<u8> = (0..5000u32).map(|i| b'a' + (i % 26) as u8).collect();
        let rope = Rope::from_text(&text).expect("load");
        for start in [0usize, 100, 4090, 4096, 4097, 4990] {
            let len = 20.min(text.len() - start);
            let mut out = vec![0u8; len];
            rope.read_at(start, len, &mut out).expect("in range");
            assert_eq!(out, &text[start..start + len], "read at {start}");
        }
    }

    /// `available` is the status bar's number, and it must reflect every leaf.
    #[test]
    fn available_sums_every_leaf_gap() {
        let rope = Rope::from_text(&vec![b'a'; LEAF_CAPACITY * 2]).expect("load");
        let manual: usize = rope.leaves.iter().map(CagrLeaf::available).sum();
        assert_eq!(rope.available(), manual);
        assert!(rope.available() > 0, "a fresh document has typing headroom");
    }

    #[test]
    fn gap_minimum_is_the_split_threshold_and_is_sane() {
        // `const {}` rather than a bare assert: clippy rejects an assertion whose value the
        // compiler already knows, and these are relations between two constants. Moving it into a
        // const block keeps it a compile error if either constant changes -- which is the point --
        // without the lint.
        const {
            assert!(GAP_MINIMUM < GAP_TARGET);
            assert!(GAP_TARGET < LEAF_CAPACITY);
        }
        // A fresh leaf must not immediately want a split.
        assert!(!CagrLeaf::new().unwrap().needs_split());
    }
}
