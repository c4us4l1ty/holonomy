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
    /// The byte range needs leaf `leaf`, and that leaf is not resident.
    ///
    /// **Phase 13, part 3.** An absent leaf is not an error in the document -- it is a leaf whose bytes
    /// are not held right now, which is the whole point of a bounded resident set. So this is returned
    /// only by the paths that have **no way to fetch**: [`Rope::read_at`] takes `&self` and therefore
    /// cannot fault a leaf in, and every mutating path refuses rather than editing bytes it does not have.
    ///
    /// Carrying the index is what makes it recoverable: a caller holding a store can map `leaf` to the
    /// section or sections that back it and retry. [`Rope::read_at_faulting`] is that caller.
    LeafAbsent {
        /// The leaf that would have to be made resident.
        leaf: usize,
    },
    /// A [`LeafSource`] could not produce the bytes it was asked for.
    ///
    /// **Deliberately carries no detail, and that is the design rather than an omission.**
    /// `fetch_leaf` sits on the seam between the rope and whatever stores the document, so whatever fails
    /// under it — an unreadable chunk, a failed authentication, a truncated final section, a full page-lock
    /// ceiling — arrives here as one variant with one message. A rope that reported "chunk 47 failed to
    /// authenticate" would be a decryption oracle with a nicer interface: FR-1.2's threat model treats
    /// *which* operation failed as sensitive, and `SectionStore::StoreError::Read` is opaque for the same
    /// reason.
    ///
    /// So this is deliberately **not** a wrapper around the source's error type. The source logs or counts
    /// what it needs; the rope learns only that it has no bytes.
    SourceUnavailable,
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
            Self::LeafAbsent { leaf } => write!(
                f,
                "leaf {leaf} is not resident and this path cannot fault it in; use \
                 read_at_faulting with a store"
            ),
            Self::SourceUnavailable => {
                write!(f, "the byte source could not produce the requested bytes")
            }
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
#[derive(Debug)]
pub struct Rope {
    /// The spine. Never empty, and **never short**: an absent leaf is `None` here, not a hole.
    ///
    /// # Why `None` and not a removal
    ///
    /// **Because the geometry outlives the bytes, and every offset in this file is geometry.** `starts`,
    /// `text_len()` and [`locate`](Self::locate) are the only things that decide where a byte lives, and
    /// none of them reads leaf memory: `locate` is a binary search over `starts`, and `text_len` sums
    /// `starts`. If an absent leaf were *removed* from the spine then its bytes would have to be
    /// re-addressable, every offset past it would shift, and `starts` would need rewriting -- which is a
    /// document-sized operation per eviction. So **absence is a `None` that still occupies its slot**, and
    /// an absent leaf's length is carried by `starts` exactly as a resident leaf's is.
    ///
    /// That is what makes eviction `O(1)` and, more importantly, what makes it *non-perturbing*: a
    /// document read after an eviction addresses the same bytes at the same offsets, because nothing about
    /// the offsets changed.
    ///
    /// `LeafSlot::Resident` holds the bytes; `LeafSlot::Absent` holds only the length. **Every read of leaf
    /// bytes goes through [`leaf`](Self::leaf) or [`leaf_mut`](Self::leaf_mut)**, never through
    /// `self.leaves[i]` directly, so there is exactly one place where absence is handled rather than one
    /// per accessor.
    leaves: Vec<LeafSlot>,
    /// Cumulative text length before each leaf, so `leaves[i]` starts at `starts[i]`.
    ///
    /// `starts.len() == leaves.len()`, `starts[0] == 0`, and it is monotonically increasing. Rebuilt
    /// on split and on merge; both are O(leaves) and happen once per 4 KiB, not per keystroke.
    starts: Vec<usize>,
    /// Cursor as a document byte offset, always on a UTF-8 boundary.
    cursor: usize,
}

/// One entry in the spine: bytes held, or only a length remembered.
///
/// # Why the length lives in the absent case too
///
/// **`Rope::text_len` sums `starts` and then asks the *last* leaf for its own length.** So an absent leaf
/// that carried no length would make `text_len()` — and therefore every offset, every bounds check, and
/// the caret — wrong, not merely slow. **A rope whose length depends on which bytes happen to be held is
/// not a rope.** Hence `Absent { text_len }`: the slot remembers how long it is even when it does not
/// remember what it says.
///
/// This is the alternative to `Option<CagrLeaf>`, which cannot work here for exactly that reason: `None`
/// has no length, so the document length would silently shrink when the last leaf was evicted. That is a
/// corruption, not a missed read, and it would not be caught by any test that only read resident bytes.
#[derive(Debug)]
enum LeafSlot {
    /// The leaf, holding its bytes.
    Resident(CagrLeaf),
    /// The bytes are not held. The length is, so the spine stays correct.
    Absent {
        /// How many bytes of document text this leaf holds in total.
        ///
        /// **Not the leaf's capacity, and not `LEAF_CAPACITY - gap`.** It is the same number
        /// `CagrLeaf::text_len` would report, so a slot's length means one thing in both cases.
        text_len: usize,
    },
}

impl LeafSlot {
    /// This slot's length, resident or not.
    #[inline]
    fn text_len(&self) -> usize {
        match self {
            Self::Resident(l) => l.text_len(),
            Self::Absent { text_len } => *text_len,
        }
    }

    /// Whether the bytes are held.
    #[inline]
    fn is_resident(&self) -> bool {
        matches!(self, Self::Resident(_))
    }

    /// Whether this leaf both holds its bytes and is due a split.
    ///
    /// **An absent leaf reports `false`, and that is the only defensible answer.** Splitting is a
    /// mutation, and a mutation needs the bytes: a leaf that is not held has no gap to measure and no
    /// text to divide. Reporting `true` would send the split path at a slot it cannot read, and the
    /// resulting `LeafAbsent` would surface one level up from the keystroke rather than here, where it
    /// is a statement about *this* leaf rather than about whatever the caller did next.
    #[inline]
    fn is_resident_and_needs_split(&self) -> bool {
        match self {
            Self::Resident(l) => l.needs_split(),
            Self::Absent { .. } => false,
        }
    }

    /// This slot's leaf as a raw pointer, or null if absent.
    ///
    /// **Only [`Rope::relink`] uses this**, and only because the leaf's `next`/`prev` are raw pointers
    /// (Plan.md §3.1 declares them `*mut`) and relinking must therefore produce raw pointers. Nothing else
    /// may: handing out a pointer that bypasses the absence check is exactly the mistake the
    /// [`leaf`](Self::leaf) choke point exists to prevent.
    #[inline]
    fn as_resident_ptr(&self) -> *mut CagrLeaf {
        match self {
            Self::Resident(l) => l as *const CagrLeaf as *mut CagrLeaf,
            Self::Absent { .. } => ptr::null_mut(),
        }
    }
}

impl Rope {
    /// Leaf `i`'s bytes, or [`RopeError::LeafAbsent`] if they are not held.
    ///
    /// **The single choke point for absence on the read side.** Every `self.leaves[i]` that touches leaf
    /// bytes becomes `self.leaf(i)?`, so "what happens when a leaf is absent" is answered in one function
    /// instead of thirty-one call sites that each have to answer it.
    #[inline]
    fn leaf(&self, i: usize) -> Result<&CagrLeaf, RopeError> {
        match self.leaves.get(i) {
            Some(LeafSlot::Resident(l)) => Ok(l),
            _ => Err(RopeError::LeafAbsent { leaf: i }),
        }
    }

    /// Leaf `i`'s bytes mutably, or [`RopeError::LeafAbsent`].
    ///
    /// **Every mutating path goes through this, and every one of them therefore refuses to edit a leaf it
    /// does not hold.** That refusal is the point: the alternative is a leaf whose gap is moved without
    /// its bytes being present, which is not a wrong answer but no answer at all. Faulting an edit in is
    /// the follow-on step; until then an edit into an absent leaf is an error the caller can act on.
    #[inline]
    fn leaf_mut(&mut self, i: usize) -> Result<&mut CagrLeaf, RopeError> {
        match self.leaves.get_mut(i) {
            Some(LeafSlot::Resident(l)) => Ok(l),
            _ => Err(RopeError::LeafAbsent { leaf: i }),
        }
    }

    /// Leaf `i`'s length, **resident or not**.
    #[inline]
    fn leaf_len(&self, i: usize) -> usize {
        self.leaves.get(i).map_or(0, LeafSlot::text_len)
    }

    /// Leaf `i`'s first byte, as a document offset.
    ///
    /// **The only stable address a leaf has.** `starts[i]` is it, and that is the value a
    /// [`LeafSource`] is keyed by — *not* `i`. See [`read_at_faulting`](Self::read_at_faulting), where
    /// the two are mixed up in the arithmetic.
    ///
    /// **Public, because a caller holding a store needs it** to work out which sections back a leaf, and
    /// because a test that assumed `i * LEAF_FILL` would be asserting the very thing that is false.
    #[inline]
    pub fn leaf_offset(&self, i: usize) -> usize {
        self.starts.get(i).copied().unwrap_or(0)
    }

    /// Leaf `i`'s length, resident or not.
    ///
    /// Public for the same reason as [`leaf_offset`](Self::leaf_offset): a caller addressing a leaf needs
    /// both ends of its range, and both have to come from the spine rather than from arithmetic.
    #[inline]
    pub fn leaf_len_of(&self, i: usize) -> usize {
        self.leaf_len(i)
    }

    /// Whether leaf `i` currently holds its bytes.
    #[inline]
    pub fn is_resident(&self, i: usize) -> bool {
        self.leaves.get(i).is_some_and(LeafSlot::is_resident)
    }

    /// How many leaves hold their bytes right now.
    pub fn resident_count(&self) -> usize {
        self.leaves.iter().filter(|l| l.is_resident()).count()
    }

    /// Total bytes of document text held right now, over the resident leaves only.
    ///
    /// **This is the number the page-lock ceiling is spent on**, which is why it is a method rather than
    /// something a caller adds up: one absent leaf releases one page-locked `SecureBlock`, and this is what
    /// shows it.
    pub fn resident_bytes(&self) -> usize {
        self.leaves.iter().filter(|l| l.is_resident()).map(LeafSlot::text_len).sum()
    }
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
        leaves.push(LeafSlot::Resident(
            CagrLeaf::new().expect("a fresh leaf cannot fail to allocate"),
        ));
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
        // **`self.leaves.last()`'s length, not its text.** Phase 13: the last slot may be
        // `LeafSlot::Absent`, which remembers its length precisely so this sum stays correct. Reading the
        // leaf's bytes here would make the document's length depend on residency -- the corruption
        // `LeafSlot::Absent { text_len }` exists to prevent.
        self.leaves.last().map_or(0, LeafSlot::text_len) + self.starts.last().copied().unwrap_or(0)
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
    ///
    /// # Why this returns `Result`, which it did not before Phase 13
    ///
    /// **Because it asks "is this byte anywhere in the rope" and an absent leaf is somewhere it cannot
    /// look.** An `Option<CagrLeaf>` slot holds no bytes, so an absent leaf contributes nothing to the
    /// scan — and a function that *skips* what it cannot read reports `false`, which reads as "the
    /// document does not contain this byte" and is exactly the wrong answer for the destructive-delete
    /// gate. Returning [`RopeError::LeafAbsent`] makes the scan refuse instead, so the answer is never
    /// wrong in the direction that matters.
    ///
    /// A caller that needs a total answer must therefore make every leaf resident first. That is
    /// [`read_at_faulting`](Self::read_at_faulting)'s job for reads, and for the audit it means the same
    /// window has to be faulted in before it is swept.
    pub fn any_leaf_contains(&self, needle: u8) -> Result<bool, RopeError> {
        for i in 0..self.leaves.len() {
            if self.leaf(i)?.buffer_slice().contains(&needle) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Every **resident** leaf's buffer as a slice, for scrubbing and audit.
    ///
    /// The rope's whole plaintext surface in one iterator. Exposed because FR-5.4's periodic scramble
    /// and the destructive-delete tests both need it, and because it makes the claim auditable rather
    /// than asserted.
    ///
    /// **Absent leaves are skipped, and here that is sound rather than a silent gap:** an absent leaf
    /// holds no bytes, so there is nothing in it to scrub. The byte would be in the *container*, which
    /// is encrypted and outside this iterator's remit. Contrast [`any_leaf_contains`](Self::any_leaf_contains),
    /// where skipping is unsound and so the signature refuses.
    pub fn leaf_buffers(&self) -> impl Iterator<Item = &[u8]> {
        self.leaves.iter().filter_map(|l| match l {
            LeafSlot::Resident(l) => Some(l.buffer_slice()),
            LeafSlot::Absent { .. } => None,
        })
    }

    /// Total bytes of gap across every **resident** leaf, i.e. how much typing is available before a
    /// split.
    ///
    /// A document's typing headroom in one number, which is what the status bar wants.
    ///
    /// **An absent leaf contributes 0, because its gap is unknown until it is faulted in.** That
    /// under-reports for a sparse rope, and deliberately so: the alternative is to invent a gap for a
    /// leaf whose bytes are not held, and an invented gap is a promise the leaf cannot keep. The
    /// keystroke path only needs the headroom of the leaf the caret is in, which
    /// [`Editor`](crate::Editor) only ever asks about after locating the caret's leaf — so on a sparse
    /// rope this number is a floor, not a forecast, and it is used as one.
    pub fn available(&self) -> usize {
        self.leaves
            .iter()
            .filter_map(|l| match l {
                LeafSlot::Resident(l) => Some(l.available()),
                LeafSlot::Absent { .. } => None,
            })
            .sum()
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
            within <= self.leaf_len(lo),
            "locate({offset}) gave leaf {lo} offset {within}, but the leaf holds {}",
            self.leaf_len(lo)
        );
        Ok((lo, within))
    }

    /// Move the cursor to a document byte offset.
    ///
    /// O(log leaves) to locate, then O(min(pre, post)) inside the leaf to move its gap.
    pub fn set_cursor(&mut self, offset: usize) -> Result<(), RopeError> {
        let (leaf, within) = self.locate(offset)?;
        if !self.leaf(leaf)?.is_char_boundary(within) {
            return Err(RopeError::NotCharBoundary {
                offset,
                text_len: self.text_len(),
            });
        }
        // **The cursor moves only after the leaf accepts the offset.** `leaf_mut` can fail on an absent
        // leaf, and moving the cursor first would leave the caret in a leaf the rope cannot read -- a
        // state with no way back, since `set_cursor` would then refuse the same offset it just took.
        self.leaf_mut(leaf)?.set_gap_offset(within)?;
        self.cursor = offset;
        Ok(())
    }

    /// **FR-1.2. Insert one byte at the cursor. O(1) when the leaf has gap; O(leaves) on a split.**
    ///
    /// The allocation-free claim is about the *common* case and is measured, not asserted: the
    /// gate counts allocations across a burst of keystrokes and requires zero for every keystroke
    /// that does not exhaust a leaf's gap.
    pub fn insert_byte(&mut self, ch: u8) -> Result<(), RopeError> {
        let (mut index, mut within) = self.locate(self.cursor)?;

        // Split when the leaf's gap is down to `GAP_MINIMUM`, not when it is empty.
        //
        // The original condition was `gap_len() >= 1`, so a leaf split only at *zero* gap and then
        // called `insert_byte` on it, which returned `GapSaturated` because the new leaf's gap was
        // consumed by the split point. Typing 6,000 bytes reported
        // `Leaf(GapSaturated { capacity: 4096 })` at the first keystroke after the gap ran dry, and a
        // document loaded at `LEAF_CAPACITY * 2` reported `a fresh document has typing headroom`
        // failing -- because every leaf had been filled to `gap_len() == 0`.
        //
        // `should_split` states the policy; the call is inlined away.
        if self.should_split(index) {
            self.split_at(index, within)?;
            // **Re-locate.** `split_at` now splits at the text midpoint, not at the cursor, so `within`
            // -- an offset into the *old* leaf -- refers to the wrong half. The cursor's *document*
            // offset is unchanged by the split, so re-resolving it is both the fix and the cheapest
            // fix: one binary search, once per 3,840 keystrokes.
            //
            // Without this, the very next line called `set_gap_offset(within)` with a `within` past the
            // end of the left half, and loading a 4,096-byte document failed with
            // `Leaf(OutOfBounds { offset: 3841, text_len: 1920 })` -- 1,920 being exactly half of
            // 3,840, which is the tell.
            let here = self.locate(self.cursor)?;
            index = here.0;
            within = here.1;
        }
        let leaf = self.leaf_mut(index)?;
        leaf.set_gap_offset(within)?;
        leaf.insert_byte(ch)?;
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
            let leaf = self.leaf_mut(index)?;
            leaf.set_gap_offset(within)?;
            leaf.delete_byte()?;
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
        let prev_len = self.leaf_len(prev);
        let leaf = self.leaf_mut(prev)?;
        leaf.set_gap_offset(prev_len)?;
        leaf.delete_byte()?;
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
    /// Allocation-free given a caller-owned `out`. Copies **once per leaf touched**, not once per
    /// byte: the inner loop was `for k in 0..take { out[...] = leaf.byte_at(w + k)? }`, which made a
    /// full-document read one `byte_at` call per byte -- 6.7 M calls for a 6.4 MiB document, repeated
    /// by every `Editor::text()` on the keystroke path. The byte loop was never needed for correctness:
    /// [`CagrLeaf::copy_text_to`] does the same work as two `copy_from_slice`s because the gap splits
    /// a leaf's text into at most two contiguous runs.
    ///
    /// Byte-identical output is asserted by `reading_across_a_leaf_boundary_returns_the_right_bytes`
    /// and `a_bulk_read_matches_the_byte_at_a_time_reader`; the change is gated on the output, not on
    /// a timing improvement.
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
            let take = (self.leaf_len(i) - w).min(len - written);
            let n = self.leaf(i)?.copy_text_to(w, &mut out[written..written + take])?;
            // `copy_text_to` clamps to the leaf's remaining text, and `take` is already that clamped
            // value, so it copies exactly `take` or the read is truncated and the next `locate` would
            // silently re-read the same bytes. Asserted rather than handled.
            debug_assert_eq!(n, take, "copy_text_to truncated a clamped run");
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


    /// Read `len` bytes at `offset`, **faulting absent leaves in from `source` as needed**.
    ///
    /// # Why this is a separate method from [`read_at`](Self::read_at)
    ///
    /// **Because faulting is `&mut` and `read_at` is `&self`, and that is not a detail to work around.**
    /// `Editor::read_into` is `&self` and is called from `&self` contexts all over the paint path
    /// (`session.rs`, `doclines.rs`, `counts.rs`), so it cannot fault. Rather than put interior mutability
    /// in the rope — which would make "is this leaf resident" a runtime question behind a `RefCell` and
    /// put a panic on the keystroke path — **the two capabilities are two methods.** `read_at` is the fast
    /// path and fails cleanly on an absent leaf; this is the path that pays a load.
    ///
    /// ## What it does not do
    ///
    /// **It does not evict.** A read that touches four leaves leaves four leaves resident, which is
    /// correct — LRU's job is [`evict`](Self::evict)'s, and doing it here would make a read
    /// unpredictable. It also does **not** make a leaf evictable while a `&[u8]` into it is outstanding:
    /// the borrows end with the loop iteration, so there is no such hazard, but a caller holding a slice
    /// from a previous [`leaf`](Self::leaf) call while calling this must not.
    ///
    /// ## The two-section case
    ///
    /// A leaf is 3,840 B and a section 65,520 B, so one leaf can need two sections and `fetch_leaf` gets
    /// called with a whole leaf's worth of room either way. **Pinning both sections for the leaf's
    /// lifetime is the source's problem, not the rope's** — which is the point of the seam: the rope asks
    /// for a leaf, and whatever stores leaves in sections answers.
    pub fn read_at_faulting(
        &mut self,
        source: &mut dyn LeafSource,
        offset: usize,
        len: usize,
        out: &mut [u8],
    ) -> Result<(), RopeError> {
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
            self.fault_leaf(source, i)?;
            let take = (self.leaf_len(i) - w).min(len - written);
            let n = self.leaf(i)?.copy_text_to(w, &mut out[written..written + take])?;
            debug_assert_eq!(n, take, "copy_text_to truncated a clamped run");
            written += take;
            at += take;
        }
        Ok(())
    }

    /// Make leaf `i` resident, fetching it if it is not.
    ///
    /// **The one place a leaf is fetched.** It was inlined in [`read_at_faulting`] and is now shared,
    /// because a second copy would be a second way to decide when a fault happens -- and the two would
    /// disagree the moment one of them grew a retry or an `on_resident` call. One fetch, one set of
    /// checks.
    ///
    /// ## It allocates, and that is the right trade here
    ///
    /// One `Vec` per fault. On the keystroke path that would be unacceptable, but a fault happens only
    /// when a read or a cursor move *crosses into* an absent leaf, not once per keystroke. Typing inside
    /// a resident leaf allocates nothing. Sizing the buffer per fault is what lets this be one function
    /// shared by the read and cursor paths rather than a caller-owned scratch buffer both would then
    /// have to keep in sync.
    pub fn fault_leaf(&mut self, source: &mut dyn LeafSource, i: usize) -> Result<(), RopeError> {
        if self.is_resident(i) {
            return Ok(());
        }
        let want = self.leaf_len(i);
        let mut buf = vec![0u8; want];
        // **By document offset, not by leaf index.** This is the seam's one non-obvious
        // requirement, and getting it wrong is silent: a rope's leaf boundaries are wherever its
        // splits and merges left them, so leaf `i` does not begin at `i * LEAF_FILL` -- typing
        // splits at the cursor and merges pull neighbours together. Keying by index therefore
        // reads the right *number* of bytes from the wrong *place*, and a document read that way
        // is wrong in a way no length check catches. The offset is the only stable address, and
        // it is also what maps to a section: `offset / CHUNK_PLAINTEXT`.
        //
        // **And it is the offset in the SAVED document**, which is the whole of the constraint
        // documented on the faulting methods below.
        let got = source.fetch_leaf(self.leaf_offset(i), &mut buf)?;
        if got != want {
            // A short source padded with zeros would read as a document full of NULs, which is
            // indistinguishable from real text at this level. Refuse instead.
            return Err(RopeError::OutOfBounds { offset: i, text_len: got });
        }
        self.leaves[i] = LeafSlot::Resident(CagrLeaf::with_text(&buf)?);
        source.on_resident(self.leaf_offset(i));
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // Faulting a leaf in, and where it stops
    //
    // `fault_leaf` is the one place a leaf is fetched. Everything above it -- reads, and the cursor
    // move below -- may fault, because **none of them change a byte.**
    //
    // # Why there are no faulting *mutators*, which is the substantive finding
    //
    // The obvious next step was `insert_byte_faulting` and friends: fault the leaf, then perform the
    // edit. It works, and it is **silently wrong**, and the reason is structural rather than a bug to
    // fix:
    //
    // **A `LeafSource` is addressed by document offset, and that is only true of an *unmodified*
    // document.** An insert at offset `p` shifts every leaf after it by one, so from that moment the
    // rope's leaf offsets and the store's offsets are different numbers. A later fault asks the store
    // for "the leaf at offset `q`" and receives the right *number* of bytes from one byte too far --
    // which is exactly the failure mode offset-keying was introduced to prevent, reintroduced by
    // editing. `VecSource` in `sparse_rope.rs` and the 1-in-17 straddling arithmetic in
    // `SectionStore::fetch_leaf` are both correct only for the saved document.
    //
    // The store cannot follow, because **there is no write-back path**: `evict_leaf` scrubs the bytes it
    // releases and hands them to the caller, and `SectionStore::evict` releases memory without telling
    // the container anything. So the store holds the document *as it was saved*, permanently, while the
    // rope holds the document *as it is now*, and after one edit the two are different documents.
    //
    // A faulting mutator here would return `Ok`, put bytes in the document, and leave a trap for the
    // next read somewhere else in the file. **That is the worst available outcome**, and it is why the
    // methods are absent rather than present-and-documented. The ways out are in PROJECT.md Phase 13
    // part 7, and the load-bearing requirement in every one of them is that the store become the
    // authority on the document's *current* state rather than its saved state.
    // ---------------------------------------------------------------------------

    /// Fault in every leaf holding any byte of `[start, end]`.
    ///
    /// **A range read, not an edit.** The leaf before `start` is not included: a caller inspecting the
    /// bytes either side of a boundary asks for a range that includes them, and silently widening the
    /// range here would make the fetch count depend on something the caller did not ask for.
    ///
    /// The walk steps a whole leaf per iteration, so it is O(leaves in the range) rather than O(bytes),
    /// and each step is one binary search.
    ///
    /// **This can fault many leaves**, bounded afterwards by the source's own LRU. The rope does not
    /// evict, because it has no store -- which is also why it cannot promise residency is bounded.
    pub fn fault_range(
        &mut self,
        source: &mut dyn LeafSource,
        start: usize,
        end: usize,
    ) -> Result<(), RopeError> {
        let len = self.text_len();
        if start > end {
            return Err(RopeError::OutOfBounds { offset: start, text_len: end });
        }
        if end > len {
            return Err(RopeError::OutOfBounds { offset: end, text_len: len });
        }
        let mut at = start;
        while at <= end {
            let (i, _) = self.locate(at)?;
            self.fault_leaf(source, i)?;
            // A zero-length leaf would make `next == at` and spin forever, so this is a `break` rather
            // than a `+ 1`: an empty document must not hang a fault.
            let next = self.leaf_offset(i) + self.leaf_len(i);
            if next <= at {
                break;
            }
            at = next;
        }
        Ok(())
    }

    /// Fault in whichever leaf holds document byte `offset`.
    ///
    /// **What a caller means to say**, rather than which leaf that is. Every caller wants "make the
    /// bytes at this offset present", and none of them should have to run `locate` and then remember
    /// the `(index, within)` shape -- a caller that dropped the `within` half on the floor would fault
    /// the right leaf and get a confusing error from `set_cursor` instead.
    pub fn fault_leaf_containing(
        &mut self,
        source: &mut dyn LeafSource,
        offset: usize,
    ) -> Result<(), RopeError> {
        let (i, _) = self.locate(offset)?;
        self.fault_leaf(source, i)
    }

    /// Move the cursor, faulting the leaf it lands in.
    ///
    /// `set_cursor` refuses an absent leaf rather than leaving the caret in a leaf the rope cannot read,
    /// which is a state with no way back. Faulting first is what lets a caret be *placed* anywhere in a
    /// document whose bytes are not all present -- and it is safe here precisely because **moving the
    /// cursor does not move a byte**, so every other leaf's offset still agrees with the store's.
    pub fn set_cursor_faulting(
        &mut self,
        source: &mut dyn LeafSource,
        offset: usize,
    ) -> Result<(), RopeError> {
        self.fault_leaf_containing(source, offset)?;
        self.set_cursor(offset)
    }

    /// Release leaf `index`'s bytes, keeping its length.
    ///
    /// **This is the call that lowers the page-lock charge.** The leaf's `SecureBlock` is dropped, which
    /// unmaps and scrubs it, and the slot keeps `LeafSlot::Absent { text_len }`. `resident_bytes()` drops
    /// by that leaf's length and `text_len()` does not move.
    ///
    /// # Why it returns the bytes rather than taking them
    ///
    /// **Because eviction that cannot fail to save them is not eviction, it is deletion.** The caller has
    /// to get the bytes to a store *before* the block goes; the rope cannot do that itself (no
    /// dependency on the container crate) and must not pretend to. So this hands the text back and lets
    /// the caller decide — and the safe order is `take_leaf_bytes` → write to store → `evict_leaf`, which
    /// is what the gate drives.
    ///
    /// Refuses on an already-absent leaf rather than counting it as an eviction, so a double-evict is a
    /// visible error rather than a second entry in a statistic.
    ///
    /// ## The order a caller must use
    ///
    /// `evict_leaf` does **not** save the bytes, and cannot: the rope has no dependency on the container
    /// crate, so it has nowhere to put them. The sequence is therefore
    /// `read_at_faulting`/`read_at` for the leaf's range → write those bytes to the store →
    /// `evict_leaf` → **`fsync`/commit in the store before using the buffer**. Evicting first and saving
    /// after cannot be written, because the bytes are gone once the block is unmapped.
    pub fn evict_leaf(&mut self, index: usize) -> Result<usize, RopeError> {
        let slot = self.leaves.get_mut(index).ok_or(RopeError::NoLeaves)?;
        match std::mem::replace(slot, LeafSlot::Absent { text_len: 0 }) {
            LeafSlot::Resident(l) => {
                let n = l.text_len();
                // Drop here: this is where the `SecureBlock` unmaps and scrubs.
                drop(l);
                *slot = LeafSlot::Absent { text_len: n };
                self.relink();
                Ok(n)
            }
            LeafSlot::Absent { text_len } => {
                // Put it back untouched and refuse. Reporting an eviction that freed nothing would
                // make a double-evict look like progress.
                *slot = LeafSlot::Absent { text_len };
                Err(RopeError::LeafAbsent { leaf: index })
            }
        }
    }

    /// Split leaf `index` at leaf-local offset `within`, leaving the cursor in the left leaf.
    ///
    /// O(leaf) with one allocation: a new `mmap` + `mlock`, and a copy of at most one leaf's text.
    /// Called when a leaf's gap is down to [`GAP_MINIMUM`], which is once per
    /// `LEAF_CAPACITY - GAP_MINIMUM` = 3,840 bytes typed into one leaf -- so the amortised cost per
    /// keystroke is one 4 KiB copy per 3,840 keystrokes.
    ///
    /// # Splitting at the cursor, and why not at the midpoint
    ///
    /// Splitting at the **midpoint** of the leaf's text looks better balanced -- both children carry
    /// half the text, so neither is empty -- and an intermediate version of this function did that, on
    /// the reasoning that "half the leaves of a loaded document carry no text" is a defect.
    ///
    /// It is not a defect. **Leaf occupancy is what determines the largest document H1 can open**, and
    /// the binding constraint is `RLIMIT_MEMLOCK`, not aesthetics.
    ///
    /// Every leaf is a 4 KiB `SecureBlock`, and `SecureBlock::allocate` **refuses rather than continuing
    /// unlocked** when `mlock` returns `ENOMEM` -- NFR-3 is that a block must never reach swap. So the
    /// document's text must fit inside the page-lock limit, and:
    ///
    /// ```text
    /// page-lock ceiling here:  2,048 leaves (8,192 KiB / 4 KiB)
    ///
    /// split at the cursor, 3,840 B/leaf:  2,048 x 3,840 = 7.50 MiB of document
    /// split at the midpoint, 1,920 B/leaf: 2,048 x 1,920 = 3.75 MiB of document
    /// ```
    ///
    /// Measured, not derived: the midpoint version could not load past 3.75 MiB, and Plan.md §7's text
    /// budget is **6.40 MiB**. The midpoint split made the top half of the permitted document size
    /// unopenable, to avoid an empty leaf that lasts only until the next keystroke fills it.
    ///
    /// With the cursor split, the right child is born empty and is filled by the keystroke that caused
    /// the split plus the next 3,839. During a load that is immediate. The one transient empty leaf per
    /// split is the right trade.
    ///
    /// # What was wrong before
    ///
    /// Two earlier versions. The first took `post_gap()` as the right leaf's text unconditionally, which
    /// is only right while the cursor is at a leaf's end: splitting leaf 0 of a loaded 9,000-byte
    /// document at offset 0 produced a document of one `.` followed by 9,000 zero bytes, because leaf 0's
    /// 4,096 bytes were in the pre-gap region and none were copied anywhere.
    ///
    /// The second split at the midpoint but read the cursor offset from the argument *after* using it to
    /// place the split, so the two disagreed and the cursor landed in the wrong leaf. Moving the cursor
    /// first makes the post-gap slice *be* everything after the split point, which removes the case
    /// analysis entirely.
    fn split_at(&mut self, index: usize, within: usize) -> Result<(), RopeError> {
        // Put the gap on the split point, which makes `post_gap` exactly the right half's text.
        self.leaf_mut(index)?.set_gap_offset(within)?;
        let total = self.leaf_len(index);
        debug_assert!(
            within <= total,
            "a split point of {within} is past the leaf's {total} bytes"
        );

        // One allocation: the new leaf's page-locked block. Its text is copied *straight* from the left
        // leaf's post-gap slice.
        //
        // An earlier version materialised the right half into a `Vec` first and copied from that: one
        // allocation and one 2 KB memcpy per split, on the keystroke path, inside the burst FR-1.2
        // requires to allocate nothing. The gate reported "1 allocations, 0 reallocations and 1
        // deallocations ... across 1 leaf splits", which is exactly this.
        let mut right = CagrLeaf::new()?;
        {
            let left = self.leaf_mut(index)?;
            debug_assert_eq!(
                left.post_gap().len(),
                total - within,
                "post-gap text should be everything after the split"
            );
            // Two disjoint leaves, so the borrows do not overlap: `left` is borrowed immutably for the
            // argument and `right` mutably for the call.
            right.fill_from(left.post_gap());
            left.truncate_post_gap();
            debug_assert_eq!(
                left.text_len(),
                within,
                "the left leaf must keep exactly the text before the split"
            );
        }

        self.leaves.insert(index + 1, LeafSlot::Resident(right));
        self.starts.insert(index + 1, 0);
        self.relink();
        self.recompute_starts_from(index);
        Ok(())
    }

    /// Whether leaf `index` has at most [`GAP_TARGET`] of gap, i.e. it should be split.
    ///
    /// The directive's split condition is "when `gap_start == gap_end`, the gap is depleted". That is the
    /// *necessary* condition and it is far too late: a leaf with no gap cannot accept a keystroke, so
    /// the next one allocates, which puts an allocation on the keystroke path that FR-1.2 forbids. The
    /// threshold is [`GAP_MINIMUM`] instead, and the difference is 3,840 free keystrokes per leaf.
    ///
    /// [`CagrLeaf::needs_split`] implements this; the method exists on the rope so the *policy* is
    /// stated once, next to the reason it is not the plan's.
    #[inline]
    pub fn should_split(&self, index: usize) -> bool {
        self.leaves.get(index).is_some_and(LeafSlot::is_resident_and_needs_split)
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
        let combined = self.leaf_len(index) + self.leaf_len(index + 1);
        // Only merge if the result still leaves room to type into, so that a merge never leaves the
        // leaf immediately needing a split.
        if combined > LEAF_CAPACITY - GAP_MINIMUM {
            return Ok(());
        }

        // Both leaves' text, pre-gap then post-gap. A leaf's gap sits *between* its two regions, so
        // concatenating `text_slices()` is the whole text in document order.
        let right_text: Vec<u8> = {
            let (_, rp) = self.leaf(index + 1)?.text_slices();
            rp.to_vec()
        };
        // The left leaf's *whole* text has to be appended, not just its post-gap half: after a split
        // the left leaf keeps only its pre-gap text (its post-gap half moved to the new leaf), but
        // after a cursor move the text can be distributed either way. Appending only the post-gap
        // half dropped whatever was in the pre-gap half -- which is the normal case.
        let left_text: Vec<u8> = {
            let (lp, _) = self.leaf(index)?.text_slices();
            lp.to_vec()
        };
        let left_len = left_text.len();
        let moved = right_text.len();

        self.leaf_mut(index)?.absorb_post_gap();
        {
            let leaf = self.leaf_mut(index)?;
            leaf.set_gap_offset(left_len)?;
            debug_assert_eq!(
                leaf.gap_len(),
                LEAF_CAPACITY - left_len,
                "after absorbing, the gap is the whole buffer minus the text"
            );
            leaf.insert_bytes(&right_text)?;
        }
        let combined_now = self.leaf_len(index);
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
            self.starts[from - 1] + self.leaf_len(from - 1)
        };
        for i in from..self.leaves.len() {
            self.starts[i] = acc;
            acc += self.leaf_len(i);
        }
    }

    /// Assert the rope's invariants. Test-only.
    ///
    /// `starts` is the structure that can silently disagree with `leaves`: a `Vec::insert` or
    /// `Vec::remove` on one without the other leaves the offset map pointing at the wrong leaf, and
    /// every subsequent edit then writes to the wrong place. It already happened once, as an index-out-
    /// of-bounds in `recompute_starts_from`. [`Editor`](crate::Editor) calls this on every operation.
    #[cfg(test)]
    pub(crate) fn check_invariants(&self) {
        assert_eq!(
            self.starts.len(),
            self.leaves.len(),
            "starts has {} entries for {} leaves",
            self.starts.len(),
            self.leaves.len()
        );
        assert!(
            !self.leaves.is_empty(),
            "an empty document is one leaf, not zero"
        );
        assert_eq!(self.starts[0], 0, "the first leaf starts at 0");
        let mut acc = 0usize;
        for (i, leaf) in self.leaves.iter().enumerate() {
            // **Only a resident leaf has a buffer to check.** An absent slot's invariant is that it
            // remembers its length, and that `starts` still agrees with the running total -- both of
            // which are asserted below, using `LeafSlot::text_len`, which answers in either case.
            if let LeafSlot::Resident(l) = leaf {
                l.check_invariants();
            }
            assert_eq!(
                self.starts[i], acc,
                "leaf {i} starts at {} but the running total says {acc}",
                self.starts[i]
            );
            acc += leaf.text_len();
        }
        assert_eq!(self.text_len(), acc, "text_len disagrees with the leaf sum");
        assert!(self.cursor <= self.text_len(), "the cursor is past the end");
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
        // **A resident leaf's links point at resident leaves, skipping absent ones.** A `prev`/`next` that
        // pointed at an absent slot would be a pointer to no leaf at all, so the walk has to be a search
        // for the nearest *resident* neighbour rather than `i - 1` and `i + 1`.
        //
        // That is O(n) per leaf in the worst case, which would make relink O(n^2). It is not: the scan runs
        // outwards from `i` and stops at the first resident slot, and the common case -- an all-resident
        // rope -- finds one immediately, so the cost is two comparisons per leaf rather than a walk.
        for i in 0..n {
            let next = (i + 1..n).find(|&j| self.is_resident(j)).map_or(ptr::null_mut(), |j| {
                self.leaves[j].as_resident_ptr()
            });
            let prev = (0..i).rev().find(|&j| self.is_resident(j)).map_or(ptr::null_mut(), |j| {
                self.leaves[j].as_resident_ptr()
            });
            if let LeafSlot::Resident(l) = &mut self.leaves[i] {
                l.next = next;
                l.prev = prev;
            }
        }
    }

    /// A rope with a correct spine and **no bytes at all**, for a document of `text_len` bytes.
    ///
    /// # What this is for, and the number it is trying to move
    ///
    /// [`from_text`](Self::from_text) is the only other way to build a rope holding text, and it needs the
    /// **whole document as a contiguous `&[u8]`**. So loading costs *two* copies at the peak: the caller's
    /// slice, plus every leaf. For the format's maximum document that is
    /// `8,321,040 x (1 + 4096/2048)` = **24.8 MiB**, of which the page-locked half is the 8.46 MiB the
    /// `RLIMIT_MEMLOCK` ceiling refuses.
    ///
    /// This constructor moves that to **the spine and nothing else**: `ceil(text_len / 2048)` slots of
    /// `LeafSlot::Absent`, 24 bytes each, about **97 KiB** for the maximum document. `resident_bytes()` is
    /// 0 and no `SecureBlock` is ever allocated, so the peak becomes O(leaves) rather than O(document).
    ///
    /// # Why the bytes can be absent at all
    ///
    /// Because [`locate`](Self::locate) is a binary search over `starts` and reads no leaf memory, **the
    /// spine *is* the document's geometry**. A rope that knows its own length and where every leaf starts
    /// can answer every address question; the only thing it cannot do is hand back the bytes, and that is
    /// [`read_at_faulting`](Self::read_at_faulting)'s job. Geometry and residency are separable, and this
    /// is the constructor that exploits it.
    ///
    /// # Why `GAP_TARGET` and not a fuller leaf
    ///
    /// The fill is 2,048 because that is what a *fresh* leaf's gap is set to, so a leaf faulted in by
    /// [`read_at_faulting`](Self::read_at_faulting) comes back shaped like every other leaf in the rope:
    /// half text, half gap at the target. Using `LEAF_CAPACITY` would hold more bytes per leaf and halve
    /// the spine, at the cost of every leaf having **no gap at all** -- so `available()` would report 0
    /// and a later edit-fault-in would find nowhere to put a byte.
    ///
    /// The spine is ~97 KiB either way against an 8 MiB ceiling, so this is not a memory trade: **it is
    /// about not designing the partition twice.**
    ///
    /// # What this does not give you
    ///
    /// A skeleton rope is **read-only** until leaves are faulted in, and editing an absent leaf still
    /// refuses. This removes the *load-time* peak; it does not make a sparse rope writable, which is the
    /// separate undo-and-spans design question and is not solved by anything here.
    pub fn from_skeleton(text_len: usize) -> Self {
        let fill = crate::leaf::GAP_TARGET;
        // **At least one leaf, even for an empty document.** Every other method would otherwise need an
        // empty case, and "insert into an empty rope" is the first thing a user does. An empty document is
        // one slot of length 0, and `resident_bytes()` is still 0.
        let n = text_len.div_ceil(fill).max(1);
        let mut leaves = Vec::with_capacity(n);
        let mut starts = Vec::with_capacity(n);
        for i in 0..n {
            let at = i * fill;
            starts.push(at);
            // The last slot holds the remainder; every earlier slot is exactly one fill.
            leaves.push(LeafSlot::Absent { text_len: (text_len - at).min(fill) });
        }
        Self { leaves, starts, cursor: 0 }
    }
}

/// Where an absent leaf's bytes come from.
///
/// **A trait rather than a `SectionStore`, because the dependency only runs one way.**
/// `holonomy-text` depends on `holonomy-jail`, `holonomy-secure` and `holonomy-geometry` — not on
/// `holonomy-container`, which holds `Wavefunction`. Making the rope take a `&Wavefunction` would mean
/// the text crate depends on the container crate, and the container crate is the one that decides what
/// a chunk is. **So the seam is declared here and implemented above**, which is also what lets
/// `holonomy-text`'s own gates run with an in-memory source and no crypto at all.
///
/// The contract is deliberately narrow, and it is **not** "give me a leaf's bytes": it is *fill `out`*
/// and report how many. The rope knows the length; the source knows the storage; neither has to
/// translate for the other. That is what keeps the leaf/section size mismatch out of this crate —
/// `65,520 / 3,840 = 17.0625`, so a leaf can straddle two sections and the *source* is the only thing
/// that has to care.
pub trait LeafSource {
    /// Fill `out` with leaf `index`'s bytes, returning how many were written.
    ///
    /// Returning fewer than `out.len()` is an error the rope propagates rather than pads: a short
    /// source would otherwise be filled with zeros and read as a document full of NULs.
    fn fetch_leaf(&mut self, offset: usize, out: &mut [u8]) -> Result<usize, RopeError>;

    /// Called after `index` has been made resident, so a source that caches may invalidate.
    ///
    /// **The default does nothing, and that is the right default**: a source that reads on demand has
    /// nothing to invalidate. It exists for a source that *does* hold a cache — a `SectionStore` whose
    /// LRU evicted the section this leaf came from while the leaf is still resident, which is the case
    /// that decides whether the store and the rope agree.
    fn on_resident(&mut self, _index: usize) {}
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
        // **Through `leaf_buffers`, not through `leaves`.** The destructive-delete gate asks "is this
        // byte anywhere in the rope"; an absent leaf holds nothing so it has nothing to find, and
        // `leaf_buffers` is the iterator that says so rather than the caller deciding.
        for leaf in rope.leaf_buffers() {
            assert!(
                !leaf.contains(&b'r'),
                "the deleted 'r' survived in a leaf buffer"
            );
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

    /// Phase 11's gate on the bulk reader: it must be byte-identical to the byte-at-a-time reader it
    /// replaced, across the gap in every leaf rather than only at a leaf boundary.
    ///
    /// A leaf's text is split into two non-contiguous runs by its gap, and `copy_text_to` handles that
    /// with two `copy_from_slice`s. So the interesting cases are reads that **start before the gap and
    /// end after it** -- one range, two copies -- which is the case the per-byte loop got right for free
    /// and a naive single `copy_from_slice` would get wrong. `reading_across_a_leaf_boundary_returns_the_right_bytes`
    /// covers the boundary between leaves; this covers the gap inside one.
    #[test]
    fn a_bulk_read_matches_the_byte_at_a_time_reader() {
        let text: Vec<u8> = (0..9000u32).map(|i| b'a' + (i % 26) as u8).collect();
        let mut rope = Rope::from_text(&text).expect("load");

        // Put the cursor inside the first leaf so its gap splits that leaf's text in two, then read
        // ranges that straddle the gap. `set_cursor` is what moves it.
        for cursor in [0usize, 10, 2_000, 4_095, 4_096] {
            rope.set_cursor(cursor).expect("cursor in range");
            for (start, len) in [
                (0usize, 4_100usize),
                (cursor.saturating_sub(50), 100),
                (cursor, 200),
                (cursor.saturating_sub(200), 400),
                (1, 4_098),
            ] {
                if start + len > text.len() {
                    continue;
                }
                let mut bulk = vec![0u8; len];
                rope.read_at(start, len, &mut bulk).expect("in range");
                assert_eq!(
                    bulk,
                    &text[start..start + len],
                    "bulk read at {start} len {len} with cursor at {cursor}"
                );
            }
        }
    }

    /// A read that stops at a leaf's end copies the whole remainder of that leaf and no more.
    #[test]
    fn a_bulk_read_reports_a_short_read_past_the_end() {
        let leaf = CagrLeaf::new().expect("leaf");
        let mut out = [0u8; 16];
        // An empty leaf has no text, so any offset past its length copies nothing and says so rather
        // than erroring -- which is what lets `read_at`'s loop treat a zero-length run as impossible.
        assert_eq!(leaf.copy_text_to(0, &mut out).expect("short read"), 0);
        assert_eq!(leaf.copy_text_to(9_999, &mut out).expect("short read"), 0);
    }

    /// The directive's split requirement: **both** children get [`GAP_TARGET`] of headroom.
    ///
    /// Splitting at the cursor instead -- which the original version did -- gives the left child every
    /// byte and the right child none, so half the leaves of a loaded document carry no text at all.
    /// This is the test that would have caught that, and it states the property rather than the
    /// arithmetic so it survives a change to the split point.
    #[test]
    fn a_split_gives_both_children_gap_target_of_headroom() {
        let mut rope = Rope::new();
        // Fill one leaf to its split threshold.
        let before = rope.leaf_count();
        for i in 0..(LEAF_CAPACITY - GAP_MINIMUM) {
            rope.insert_byte(b'a' + (i % 26) as u8).expect("room");
        }
        assert_eq!(rope.leaf_count(), before, "one leaf holds a page of typing");

        // At `LEAF_CAPACITY - GAP_MINIMUM` bytes the gap is exactly `GAP_MINIMUM`, and `needs_split` is
        // `gap < GAP_MINIMUM` -- strict -- so the gap must go *below* the threshold before a split fires.
        // An earlier version assumed one more keystroke was enough and reported "left: 1, right: 2",
        // which was the threshold working exactly as written.
        assert_eq!(
            rope.available(),
            GAP_MINIMUM,
            "the gap is exactly at the threshold"
        );
        assert!(!rope.should_split(0), "at the threshold, so not yet");

        // Keep typing until it splits. `needs_split` is checked *before* each insert, so a leaf whose gap
        // is exactly `GAP_MINIMUM` does not split on the next keystroke -- it splits on the one after,
        // once the gap is `GAP_MINIMUM - 1`. An earlier version assumed a single keystroke crossed the
        // threshold and reported "left: 1, right: 2".
        let mut typed = 0;
        while rope.leaf_count() == 1 && typed < 8 {
            rope.insert_byte(b'z').expect("room");
            typed += 1;
        }
        assert_eq!(
            typed, 2,
            "one keystroke to go below the threshold, one to split"
        );
        assert_eq!(rope.leaf_count(), 2, "the split happened");
        assert_eq!(rope.text_len(), LEAF_CAPACITY - GAP_MINIMUM + 2);

        // Both halves carry text, and both have headroom.
        let total_gap = rope.available();
        assert!(
            total_gap >= 2 * GAP_TARGET,
            "two leaves have {total_gap} of gap between them, want at least {}",
            2 * GAP_TARGET
        );
        // And no leaf is empty, which is the specific failure splitting at the cursor produces.
        for (i, leaf) in rope.leaves.iter().enumerate() {
            assert!(
                leaf.text_len() > 0,
                "leaf {i} is empty; a split at the cursor produces exactly this"
            );
        }
    }

    /// A loaded document's leaves must be **full**, not balanced -- and the difference decides the
    /// largest document H1 can open, against `RLIMIT_MEMLOCK`. See [`split_at`](Self::split_at).
    #[test]
    fn a_loaded_document_packs_its_leaves_full() {
        let text: Vec<u8> = (0..40_000u32).map(|i| b'a' + (i % 26) as u8).collect();
        let rope = Rope::from_text(&text).expect("load");
        assert_eq!(rope.to_vec().unwrap(), text, "the document round-trips");
        assert!(rope.leaf_count() > 8, "40,000 bytes span many leaves");

        // Occupancy, not emptiness. A rope split at the text *midpoint* would leave no leaf empty and
        // half the occupancy, and half the occupancy halves the largest document that fits inside
        // `RLIMIT_MEMLOCK` -- 3.75 MiB against a 6.40 MiB budget, measured. That is the whole reason this
        // test asserts fullness rather than balance.
        let (last, rest) = rope.leaves.split_last().expect("at least one leaf");
        for (i, leaf) in rest.iter().enumerate() {
            assert!(
                leaf.text_len() >= LEAF_CAPACITY - GAP_MINIMUM - 1,
                "leaf {i} of {} holds only {} bytes; a cursor split runs leaves full",
                rest.len(),
                leaf.text_len()
            );
        }
        assert!(
            last.text_len() < LEAF_CAPACITY - GAP_MINIMUM,
            "the newest leaf holds {} bytes, which cannot happen: it was never filled",
            last.text_len()
        );

        // The occupancy figure that decides the page-lock budget, as a ratio, so a regression here reads
        // as "half the document".
        let overhead = rope.leaf_count() * (LEAF_CAPACITY - GAP_MINIMUM) / rope.text_len();
        assert!(
            overhead <= 2,
            "{} leaves for {} bytes is {overhead}x overhead; a cursor split runs at ~1x",
            rope.leaf_count(),
            rope.text_len()
        );
    }

    /// A split must not move the document offset of the cursor, which is what makes it invisible to the
    /// editor.
    #[test]
    fn a_split_does_not_move_the_cursor() {
        let text: Vec<u8> = (0..9_000u32).map(|i| b'a' + (i % 26) as u8).collect();
        let mut rope = Rope::from_text(&text).expect("load");
        // Not `text_len`: the cursor at the very end of the document cannot trigger a split, because a
        // split happens on the keystroke that follows and there is nowhere to put one. An earlier version
        // included 9,000 and reported "typing at 9000 did not split a leaf" -- the case cannot happen.
        for target in [0usize, 100, 2_000, 4_500, 8_999] {
            rope.set_cursor(target).expect("in range");
            let leaves = rope.leaf_count();
            // Force a split of the cursor's leaf.
            while rope.leaf_count() == leaves && rope.cursor() < rope.text_len() {
                rope.set_cursor(target).expect("cursor");
                let i = rope.locate(rope.cursor()).expect("locate").0;
                rope.split_at(i, rope.leaf(i).expect("resident leaf").gap_offset())
                    .expect("split");
            }
            assert!(
                rope.leaf_count() > leaves,
                "typing at {target} did not split a leaf"
            );
            assert_eq!(
                rope.cursor(),
                target,
                "the document offset must survive the split"
            );
            assert_eq!(rope.to_vec().unwrap(), text, "and so must the text");
        }
    }

    /// `available` is the status bar's number, and it must reflect every leaf.
    #[test]
    fn available_sums_every_leaf_gap() {
        let rope = Rope::from_text(&vec![b'a'; LEAF_CAPACITY * 2]).expect("load");
        let manual: usize = rope
            .leaves
            .iter()
            .filter_map(|l| match l {
                LeafSlot::Resident(l) => Some(l.available()),
                LeafSlot::Absent { .. } => None,
            })
            .sum();
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
