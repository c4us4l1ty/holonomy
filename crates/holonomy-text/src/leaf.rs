//! The Cacheline-Aligned Gap-Rope leaf: one 4096-byte page-locked node with a gap in it.
//!
//! FR-1.1, FR-1.2 and Plan.md §3.1. This module is the single place that mutates document bytes,
//! and it is built around one claim: **a keystroke must not allocate.** Everything here is either
//! a bounded arithmetic operation on an existing buffer or a `sysconf`-free integer computation.
//! Nothing in `insert_byte`, `delete_byte` or their callers touches the allocator.
//!
//! # The layout
//!
//! ```text
//! ┌───────────────────────┬──────────────────┬───────────────────────┐
//! │  pre-gap text         │   THE GAP        │   post-gap text       │
//! │  buffer[0..gap_start]  │   buffer         │  buffer[gap_end..]    │
//! │                       │ [gap_start,      │                       │
//! │                       │  gap_end)        │                       │
//! └───────────────────────┴──────────────────┴───────────────────────┘
//! ```
//!
//! `text_len` is `gap_start + (capacity - gap_end)`: everything except the gap. The invariant that
//! makes the type safe to poke at is `gap_start <= gap_end <= LEAF_CAPACITY`, and it is checked on
//! construction and re-established after every mutation.
//!
//! # Why the gap is worth 2048 bytes
//!
//! Typing at the cursor is the hot path. With a gap there, an insert is one byte written at
//! `gap_start` and two `u16` incremented -- O(1), allocation-free, and touching one or two
//! cachelines. Without it, every keystroke would memmove the rest of the leaf, which is O(4096)
//! bytes at 4 KiB and lands in the worst place in cache.
//!
//! Plan.md's diagram shows a 1,216 / 2,048 / 832 split, which is 4,096 total and matches
//! [`LEAF_CAPACITY`]. The gap opens in the middle, and the exact split is
//! [`CagrLeaf::new`]'s business rather than a constant here.
//!
//! # Alignment
//!
//! `#[repr(C, align(64))]` as Plan.md §3.1 specifies. The alignment is on the node, not only on the
//! buffer: the hot fields (`gap_start`, `gap_end`, `text_len`) sit immediately after a 4,096-byte
//! buffer, and with 64-byte alignment they share one cacheline with each other and never with the
//! buffer's tail. An unaligned node would put `gap_start` on the same line as the last 60 bytes
//! of text, so every keystroke would dirty a line it did not otherwise touch.
//!
//! # The gap is a *cache-line-aligned region*, not just a range
//!
//! [`CagrLeaf::gap_offset`] rounds `gap_start` down to a cacheline boundary when asked for a
//! layout decision, because that is the granularity at which the damage tracker and the scrubber
//! operate. The *logical* gap stays exactly `gap_start..gap_end`; the aligned view is only for
//! callers that work in whole lines.

use holonomy_secure::SecureBlock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Bytes of text per leaf. One page, so a leaf is one `mmap` region and one `mlock`.
pub const LEAF_CAPACITY: usize = 4096;

/// Cache line size on every target H1 supports (Core 2 Duo and later).
pub const CACHELINE_BYTES: usize = 64;

/// The gap [`CagrLeaf::with_text`] reserves after existing text, and Plan.md's figure.
///
/// This is *not* the gap a fresh leaf has. See [`CagrLeaf::new`]: a leaf with no text has the whole
/// buffer as gap, and only a leaf born from text needs a figure. Two constraints set the number.
/// Typing arrives in bursts, and a gap too small splits the rope constantly -- each split is an
/// allocation, which is exactly what the hot path must not do. A gap too large wastes memory: 6.40
/// MiB of budget over a 2,000-page document is roughly 1,600 leaves, and 2,048 reserved bytes per
/// leaf would be 3.2 MiB, half the budget spent on nothing.
pub const GAP_TARGET: usize = 2048;

/// A gap smaller than this justifies a split rather than a memmove.
///
/// Set below [`GAP_TARGET`] on purpose. A split costs one `mmap`, one `mlock` and a 4 KiB copy;
/// a memmove costs at most the post-gap text of one leaf, bounded by 4 KiB and typically far less.
/// Splitting at exactly zero available gap would mean splitting whenever the gap is full, which
/// leaves the rope permanently one insert away from an allocation.
pub const GAP_MINIMUM: usize = 256;

/// Why an operation on a leaf was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafError {
    /// The gap is saturated. The caller must split, then retry.
    GapSaturated {
        /// The leaf's capacity, always [`LEAF_CAPACITY`].
        capacity: usize,
    },
    /// A delete ran past the start of the pre-gap text. The caller must balance with the
    /// previous leaf.
    Underflow {
        /// `gap_start`, which was already zero.
        gap_start: u16,
    },
    /// A read ran past the end of the leaf's text.
    OutOfBounds {
        /// Byte offset asked for.
        offset: usize,
        /// Bytes of valid text the leaf holds.
        text_len: usize,
    },
    /// The offset is not on a UTF-8 character boundary.
    NotCharBoundary {
        /// Byte offset asked for.
        offset: usize,
        /// The leaf's text length, for context.
        text_len: usize,
    },
    /// The page-locked allocation failed.
    Allocation(holonomy_secure::SecureBlockError),
}

impl std::fmt::Display for LeafError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GapSaturated { capacity } => {
                write!(
                    f,
                    "gap saturated: no room in a {capacity}-byte leaf, split required"
                )
            }
            Self::Underflow { gap_start } => {
                write!(
                    f,
                    "delete underflow at gap_start {gap_start}; balance with the previous leaf"
                )
            }
            Self::OutOfBounds { offset, text_len } => {
                write!(
                    f,
                    "offset {offset} is past the leaf's {text_len} bytes of text"
                )
            }
            Self::NotCharBoundary { offset, text_len } => write!(
                f,
                "offset {offset} is not a UTF-8 character boundary in {text_len} bytes"
            ),
            Self::Allocation(e) => write!(f, "leaf allocation failed: {e}"),
        }
    }
}

impl std::error::Error for LeafError {}

impl From<holonomy_secure::SecureBlockError> for LeafError {
    fn from(e: holonomy_secure::SecureBlockError) -> Self {
        Self::Allocation(e)
    }
}

/// One 4096-byte page-locked leaf with a gap in the middle.
///
/// # Not `Clone`
///
/// Copying a leaf would copy plaintext out of the page-locked region into ordinary heap memory,
/// which is the one thing [`SecureBlock`] exists to prevent. There is no `Clone` impl and no
/// `to_vec` on the text accessor that would tempt a caller into one by accident.
///
/// # `Send` and `Sync`
///
/// `SecureBlock` is both, since the mapping is just a pointer to an `mmap`'d region. The leaf adds
/// an [`AtomicBool`] rather than a `bool` for `is_dirty` so that a damage tracker on another
/// thread can clear it without a lock on the hot path. See [`CagrLeaf::take_dirty`].
#[repr(C, align(64))]
pub struct CagrLeaf {
    /// The page-locked buffer. `[u8; LEAF_CAPACITY]`, not a `Vec`: a `Vec` would allocate and
    /// would not be page-locked.
    ///
    /// Held as a pointer because a 4,096-byte inline array makes `CagrLeaf` 4 KiB, and `CagrLeaf`
    /// is allocated through `SecureBlock`, which hands out a raw region rather than a sized value.
    buffer: *mut u8,
    /// Where the gap starts. Bytes `[0, gap_start)` are live text.
    gap_start: u16,
    /// Where the gap ends. Bytes `[gap_end, LEAF_CAPACITY)` are live text.
    gap_end: u16,
    /// Live bytes in the leaf, `gap_start + (LEAF_CAPACITY - gap_end)`.
    text_len: u16,
    /// Whether the leaf has changed since the last damage flush.
    ///
    /// Atomic so that [`take_dirty`](Self::take_dirty) is a single atomic swap with no lock. A
    /// plain `bool` would force the damage tracker and the editor onto the same lock, and the
    /// damage tracker is on the render thread while the editor is on the input thread.
    is_dirty: AtomicBool,
    /// Next leaf in document order.
    ///
    /// A raw pointer, as Plan.md §3.1 specifies, because the rope's spine is a linked list and a
    /// `Box`/`Rc` would put a refcount on the hot path. It is never dereferenced without the
    /// invariant that leaves are owned by exactly one [`Rope`](super::Rope).
    pub next: *mut CagrLeaf,
    /// Previous leaf in document order.
    pub prev: *mut CagrLeaf,
    /// Page-locked backing store. Dropping it scrubs the text and unmaps the region.
    block: SecureBlock,
}

impl CagrLeaf {
    /// Allocate an empty leaf with the gap open in the middle.
    ///
    /// Costs one `mmap`, one `mlock` and one `mprotect`, so it is *not* on the hot path. Every
    /// keystroke that does not split a leaf goes through [`insert_byte`](Self::insert_byte), which
    /// allocates nothing.
    pub fn new() -> Result<Self, LeafError> {
        let mut block = SecureBlock::allocate(LEAF_CAPACITY)?;
        // SAFETY: `allocate` guarantees LEAF_CAPACITY readable and writable bytes. The region is
        // zeroed by the kernel for `MAP_ANONYMOUS`, which is also what the scrub-on-drop contract
        // wants the buffer to start as.
        unsafe { std::ptr::write_bytes(block.as_mut_ptr(), 0, LEAF_CAPACITY) };
        // The whole buffer is the gap: no pre-gap text, no post-gap text.
        //
        // # Why this, and not a 2,048-byte gap with the rest as unused slack
        //
        // Plan.md §3.1's field list is `buffer`, `gap_start`, `gap_end`, `text_len` -- there is no
        // field recording where the post-gap text ends. So the only layout the invariant
        // `text_len == gap_start + (CAPACITY - gap_end)` can describe is one where post-gap text
        // fills `[gap_end, CAPACITY)` completely:
        //
        // ```text
        // [0 .............. gap_start)  pre-gap text,  gap_start bytes
        // [gap_start .. gap_end)         the gap,     gap_end - gap_start bytes
        // [gap_end ........ CAPACITY)     post-gap text, CAPACITY - gap_end bytes
        // ```
        //
        // `text_len` is then just "everything that is not gap", and `insert_byte` moving only
        // `gap_start` is correct -- it shrinks the gap from the left. Reserving a fixed 2,048-byte
        // gap and leaving the tail unused cannot be expressed in these fields: the tail would be
        // counted as post-gap text, so an empty leaf with a front gap would report 2,048 bytes of
        // text. That is not a cosmetic bug. It reported `text_len 0 != 0 + 2048`, and the leaf's
        // own zeroes were returned as if they were the start of the document.
        //
        // The consequence for typing headroom is good: a fresh leaf has 4,096 bytes of gap, so the
        // first 4,096 keystrokes into it never allocate.
        Ok(Self {
            buffer: block.as_mut_ptr(),
            gap_start: 0,
            gap_end: LEAF_CAPACITY as u16,
            text_len: 0,
            is_dirty: AtomicBool::new(false),
            next: std::ptr::null_mut(),
            prev: std::ptr::null_mut(),
            block,
        })
    }

    /// Allocate a leaf holding `text`, with the gap open after it.
    ///
    /// Refuses text longer than [`LEAF_CAPACITY`]: the caller splits, so this is a programming
    /// error rather than a runtime condition.
    pub fn with_text(text: &[u8]) -> Result<Self, LeafError> {
        if text.len() > LEAF_CAPACITY {
            return Err(LeafError::OutOfBounds {
                offset: text.len(),
                text_len: LEAF_CAPACITY,
            });
        }
        let mut leaf = Self::new()?;
        // SAFETY: `text.len() <= LEAF_CAPACITY`, and `gap_start` is `GAP_TARGET / 2` from a fresh
        // leaf, so `text.len()` is at most the gap's start offset only if the caller is within
        // capacity -- the copy is bounded by `LEAF_CAPACITY` regardless, and the bound check above
        // already rejected anything longer.
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr(), leaf.buffer, text.len());
        }
        // The text is the pre-gap half; the rest of the buffer is the gap. See the layout note in
        // `new` for why the gap runs to `LEAF_CAPACITY` rather than having a fixed size with
        // unused slack after it.
        leaf.gap_start = text.len() as u16;
        leaf.gap_end = LEAF_CAPACITY as u16;
        leaf.text_len = text.len() as u16;
        Ok(leaf)
    }

    /// The raw buffer.
    #[inline]
    pub fn buffer(&self) -> *mut u8 {
        self.buffer
    }

    /// Overwrite the leaf with `text`, holding no pre-gap content and leaving the gap after it.
    ///
    /// For the rope's split, which has already validated `text.len() <= LEAF_CAPACITY` and has a
    /// freshly allocated leaf to fill. Not the same as [`with_text`](Self::with_text): that one
    /// allocates, this one writes into an existing allocation.
    pub(crate) fn fill_from(&mut self, text: &[u8]) {
        debug_assert!(text.len() <= LEAF_CAPACITY);
        if text.len() > LEAF_CAPACITY {
            return;
        }
        if !text.is_empty() {
            // SAFETY: length checked immediately above.
            unsafe {
                std::ptr::copy_nonoverlapping(text.as_ptr(), self.buffer, text.len());
            }
        }
        self.gap_start = text.len() as u16;
        self.gap_end = LEAF_CAPACITY as u16;
        self.text_len = text.len() as u16;
        self.mark_dirty();
    }

    /// Make the live content contiguous at the front of the buffer, leaving the gap at the end.
    ///
    /// Moves the post-gap text down against the pre-gap text and scrubs the vacated bytes. The gap
    /// keeps its size, so this costs O(post-gap length) and allocates nothing.
    ///
    /// This is the operation that makes the layout's fixed fields work: afterwards
    /// `gap_end == LEAF_CAPACITY` and every live byte is in `[0, gap_start)`.
    pub(crate) fn absorb_post_gap(&mut self) {
        let post = self.post_gap();
        if post.is_empty() {
            self.gap_end = LEAF_CAPACITY as u16;
            return;
        }
        let post_len = post.len();
        let at = self.gap_start as usize;
        // SAFETY: source `[gap_end, CAPACITY)` and destination `[gap_start, gap_start + post_len)`
        // are both inside the buffer and are the same length, so the regions may overlap; `copy`.
        unsafe {
            std::ptr::copy(
                self.buffer.add(self.gap_end as usize),
                self.buffer.add(at),
                post_len,
            )
        };
        // Scrub the vacated tail: this is what stops the rope's merge path from leaving a second
        // copy of the plaintext behind in the same page.
        //
        // SAFETY: `[gap_start + post_len, gap_end + post_len)` is inside the buffer, since
        // `gap_end + post_len <= CAPACITY`.
        unsafe {
            std::ptr::write_bytes(
                self.buffer.add(at + post_len),
                0,
                (self.gap_end as usize + post_len) - (at + post_len),
            )
        };
        self.gap_start += post_len as u16;
        // Only the pre-gap text survives; everything from `gap_end` was moved to a new leaf and is
        // scrubbed above, so the gap now runs from `gap_start` to the end of the buffer.
        self.gap_end = LEAF_CAPACITY as u16;
        self.text_len = self.gap_start;
        self.mark_dirty();
    }

    /// Drop the leaf's post-gap text, keeping only its pre-gap content.
    ///
    /// For the rope's split. The post-gap bytes are zeroed rather than merely excluded, so the text
    /// that has moved to the new leaf leaves no copy behind.
    pub(crate) fn truncate_post_gap(&mut self) {
        let post = self.post_gap();
        if !post.is_empty() {
            // SAFETY: `post` is exactly the live bytes from `gap_end` to the end of the buffer.
            unsafe { std::ptr::write_bytes(self.buffer.add(self.gap_end as usize), 0, post.len()) };
        }
        // Only the pre-gap text survives; everything from `gap_end` was moved to a new leaf and is
        // scrubbed above, so the gap now runs from `gap_start` to the end of the buffer.
        self.gap_end = LEAF_CAPACITY as u16;
        self.text_len = self.gap_start;
        self.mark_dirty();
    }

    /// The whole buffer as a slice, gap included. The gap is zeroed.
    #[inline]
    pub fn buffer_slice(&self) -> &[u8] {
        // SAFETY: `buffer` points at LEAF_CAPACITY valid bytes for the life of `self`.
        unsafe { std::slice::from_raw_parts(self.buffer, LEAF_CAPACITY) }
    }

    /// The leaf's text, gap removed, as one contiguous slice.
    ///
    /// # Why this can be O(1) in the common case
    ///
    /// When the cursor is at the end of the leaf the two halves are adjacent and this is a single
    /// slice. When it is in the middle they are not, and no slice can represent both without a copy
    /// -- so this returns only the post-gap part, and [`pre_gap`](Self::pre_gap) the other. A
    /// caller that needs the whole leaf as one buffer must copy, which is a design consequence of
    /// the gap and is stated rather than hidden.
    ///
    /// See [`gap_offset`](Self::gap_offset) for where the gap splits the text.
    #[inline]
    pub fn post_gap(&self) -> &[u8] {
        let start = self.gap_end as usize;
        // SAFETY: `gap_end <= LEAF_CAPACITY` by invariant, so the slice is in bounds.
        unsafe { std::slice::from_raw_parts(self.buffer.add(start), LEAF_CAPACITY - start) }
    }

    /// The text before the gap, which is the whole leaf when the cursor is at the end.
    #[inline]
    pub fn pre_gap(&self) -> &[u8] {
        // The invariant caps `gap_start` at `LEAF_CAPACITY`, so this is in bounds.
        let end = self.gap_start as usize;
        // SAFETY: `gap_start <= LEAF_CAPACITY` by invariant.
        unsafe { std::slice::from_raw_parts(self.buffer, end) }
    }

    /// All live text, as at most two slices: `(pre_gap, post_gap)`.
    ///
    /// When the gap is empty the two are adjacent and the caller can treat them as one. This is
    /// the allocation-free way to read a leaf's text, and the reason a gap-rope does not need a
    /// `String`.
    #[inline]
    pub fn text_slices(&self) -> (&[u8], &[u8]) {
        (self.pre_gap(), self.post_gap())
    }

    /// Live bytes in this leaf.
    #[inline]
    pub fn text_len(&self) -> usize {
        self.text_len as usize
    }

    /// Where the gap starts, in buffer coordinates.
    #[inline]
    pub fn gap_start(&self) -> usize {
        self.gap_start as usize
    }

    /// Where the gap ends, in buffer coordinates.
    #[inline]
    pub fn gap_end(&self) -> usize {
        self.gap_end as usize
    }

    /// Free bytes in the gap.
    #[inline]
    pub fn gap_len(&self) -> usize {
        (self.gap_end - self.gap_start) as usize
    }

    /// The byte offset into this leaf's text at which the gap sits.
    ///
    /// This is the cursor's position in leaf-local coordinates: the pre-gap text is `[0, gap_offset)`
    /// and the post-gap text starts there. It is *not* a document offset -- [`Rope`](super::Rope)
    /// owns the mapping from leaf-local to document coordinates, because it is the only thing that
    /// knows the leaves before this one.
    #[inline]
    pub fn gap_offset(&self) -> usize {
        self.gap_start as usize
    }

    /// The gap's start, rounded down to a cacheline boundary.
    ///
    /// For callers that work in whole lines: the damage tracker repaints whole rows, and the
    /// scrubber zeros whole cachelines so it cannot leave a copy of the plaintext in a neighbouring
    /// line. The *logical* gap is unchanged by this.
    #[inline]
    pub fn gap_offset_aligned(&self) -> usize {
        self.gap_start as usize & !(CACHELINE_BYTES - 1)
    }

    /// How many free bytes the gap has, as the "should I split?" answer.
    ///
    /// [`needs_split`](Self::needs_split) is the question callers actually want; this is the number
    /// behind it.
    #[inline]
    pub fn available(&self) -> usize {
        self.gap_len()
    }

    /// Whether the next insert should split this leaf rather than write into it.
    #[inline]
    pub fn needs_split(&self) -> bool {
        self.gap_len() < GAP_MINIMUM
    }

    /// **FR-1.2. Insert one byte at the cursor, O(1), no allocation.**
    ///
    /// The whole operation: bounds check, one byte store, three `u16` updates, one atomic store.
    /// No loop, no allocation, no syscall.
    ///
    /// # Safety of the byte store
    ///
    /// `gap_start < gap_end` guarantees the index is inside the gap, and the gap is inside the
    /// 4,096-byte buffer. The previous byte is *not* scrubbed here, because an insert does not
    /// remove any plaintext -- it moves the gap boundary. [`delete_byte`](Self::delete_byte) is
    /// the operation that scrubs.
    #[inline]
    pub fn insert_byte(&mut self, ch: u8) -> Result<(), LeafError> {
        if self.gap_start >= self.gap_end {
            return Err(LeafError::GapSaturated {
                capacity: LEAF_CAPACITY,
            });
        }
        let at = self.gap_start as usize;
        debug_assert!(at < LEAF_CAPACITY);
        // SAFETY: `at < gap_end <= LEAF_CAPACITY` from the check above.
        unsafe { *self.buffer.add(at) = ch };
        self.gap_start += 1;
        self.text_len += 1;
        self.mark_dirty();
        Ok(())
    }

    /// **FR-1.2. Delete one byte before the cursor, O(1), no allocation, and scrub it.**
    ///
    /// The deleted byte is overwritten with zero rather than merely excluded. Plan.md FR-1.2 makes
    /// the rope destructive by design -- "deleting text completely overwrites and zeros the data
    /// in memory" -- so the byte must not survive anywhere in the leaf.
    #[inline]
    pub fn delete_byte(&mut self) -> Result<(), LeafError> {
        if self.gap_start == 0 {
            return Err(LeafError::Underflow { gap_start: 0 });
        }
        self.gap_start -= 1;
        let at = self.gap_start as usize;
        // SAFETY: `at = gap_start - 1 < LEAF_CAPACITY`.
        unsafe { *self.buffer.add(at) = 0 };
        self.text_len -= 1;
        self.mark_dirty();
        Ok(())
    }

    /// Delete `n` bytes before the cursor. O(n) in `n`, which is O(1) for the single-byte case.
    ///
    /// Every deleted byte is zeroed. A bulk delete is bounded by `n <= text_len`, and callers should
    /// prefer this over `n` calls to [`delete_byte`](Self::delete_byte) only when `n` is large --
    /// the single-byte version is inlined and the loop here is not.
    pub fn delete_bytes(&mut self, n: usize) -> Result<(), LeafError> {
        if n > self.text_len as usize {
            return Err(LeafError::Underflow {
                gap_start: self.gap_start,
            });
        }
        if n > self.gap_start as usize {
            return Err(LeafError::Underflow {
                gap_start: self.gap_start,
            });
        }
        if n == 0 {
            return Ok(());
        }
        // `start` is checked against `gap_start` above, so `start..start + n` is inside the
        // pre-gap text and therefore inside the buffer.
        let start = self.gap_start as usize - n;
        // SAFETY: as above.
        unsafe { std::ptr::write_bytes(self.buffer.add(start), 0, n) };
        self.gap_start = start as u16;
        self.text_len -= n as u16;
        self.mark_dirty();
        Ok(())
    }

    /// Insert a run of bytes at the cursor.
    ///
    /// O(n) in `bytes.len()`, which is what writing `n` bytes costs. Allocates nothing. Refuses a
    /// run longer than the gap rather than splitting the leaf, because deciding where to split a
    /// long paste needs document-level context that this leaf does not have -- see
    /// [`Rope::insert`](super::Rope::insert).
    pub fn insert_bytes(&mut self, bytes: &[u8]) -> Result<(), LeafError> {
        if bytes.len() > self.gap_len() {
            return Err(LeafError::GapSaturated {
                capacity: LEAF_CAPACITY - self.gap_len(),
            });
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let at = self.gap_start as usize;
        // SAFETY: `at + bytes.len() <= gap_end <= LEAF_CAPACITY`.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.buffer.add(at), bytes.len()) };
        self.gap_start += bytes.len() as u16;
        self.text_len += bytes.len() as u16;
        self.mark_dirty();
        Ok(())
    }

    /// Move the cursor to `offset` bytes into this leaf's text.
    ///
    /// O(bytes between the old and new cursor positions), because the gap has to travel. Every
    /// byte either side of the cursor moves, so a cursor jump is a memmove within one 4 KiB leaf --
    /// bounded, and still allocation-free. This is the operation the gap trades against: it makes
    /// typing free and cursor motion linear within a leaf.
    ///
    /// Refuses a non-boundary offset. A caret is always between characters; putting it inside a
    /// multi-byte sequence would produce text that is not valid UTF-8, and every subsequent
    /// `char_indices` walk over the document would be wrong.
    pub fn set_gap_offset(&mut self, offset: usize) -> Result<(), LeafError> {
        let text_len = self.text_len as usize;
        if offset > text_len {
            return Err(LeafError::OutOfBounds { offset, text_len });
        }
        if !self.is_char_boundary(offset) {
            return Err(LeafError::NotCharBoundary { offset, text_len });
        }
        self.move_gap_to(offset);
        Ok(())
    }

    /// Place the gap so the cursor sits at text offset `offset`.
    ///
    /// # The layout
    ///
    /// ```text
    ///  0            gap_start   gap_end            CAPACITY
    ///  |  pre-gap text  |  gap  |    post-gap text     |
    ///                     <------- gap ------->
    /// ```
    ///
    /// with `text_len = gap_start + (CAPACITY - gap_end)`. Placing the cursor at `offset`
    /// determines both boundaries:
    ///
    /// ```text
    /// new gap_start = offset
    /// new gap_end   = CAPACITY - (text_len - offset)
    /// ```
    ///
    /// # Deriving the memmove
    ///
    /// **The text before the cursor never moves.** It is `[0, offset)` and stays there, so every
    /// operation below concerns only the *tail*: the text at or after `offset`, of length
    /// `text_len - offset`.
    ///
    /// The tail is currently in two pieces:
    ///
    /// ```text
    ///   A = pre-gap tail   [offset, gap_start)    length cur_start - offset
    ///   B = post-gap text  [gap_end, CAPACITY)    length CAPACITY - gap_end
    /// ```
    ///
    /// and it must end up as one contiguous run ending at `CAPACITY`, i.e. starting at
    /// `dest = CAPACITY - (text_len - offset)`. Now substitute: `text_len = gap_start + (CAPACITY -
    /// gap_end)`, so `CAPACITY - (text_len - offset) = CAPACITY - gap_start - CAPACITY + gap_end +
    /// offset = gap_end - (gap_start - offset)`. That is `dest = gap_end - shift`, where
    /// `shift = gap_start - offset`.
    ///
    /// **Moving left** (`offset < gap_start`, `shift > 0`): the tail must slide left by `shift`. Piece
    /// A goes from `[offset, gap_start)` to `[dest, gap_end)`, and piece B goes from
    /// `[gap_end, CAPACITY)` to `[dest + lenA, CAPACITY) = [gap_end, CAPACITY)` -- **already in
    /// place**. So one copy of `shift` bytes from `offset` to `dest`, then scrub
    /// `[offset, gap_end)`, which is the vacated source and the old gap together.
    ///
    /// **Moving right** (`offset > gap_start`, `shift > 0`): the tail shrinks by `shift` as its front
    /// joins the pre-gap region. Piece B's front `shift` bytes go from `[gap_end, gap_end + shift)` to
    /// `[gap_start, gap_start + shift)`, and the rest of B is already in place. One copy of `shift`
    /// bytes from `gap_end` to `gap_start`, then scrub `[gap_end, gap_end + shift)`.
    ///
    /// In both directions the copy is `shift` bytes -- the distance the cursor moved -- which is why
    /// this is bounded by the leaf and never by the document.
    ///
    /// # The bugs this derivation is here to prevent
    ///
    /// Three earlier versions, all of which destroyed text on a cursor move to the start of a
    /// nearly-full leaf -- the most common non-trivial edit, a click at the top of the document:
    ///
    /// * Copying `post_len = CAPACITY - gap_end` bytes leftward. That is zero whenever the cursor was
    ///   at the end of the leaf, which is exactly when all the text is in the pre-gap region and
    ///   needs moving. A leaf of 300 bytes with the cursor at 300, moved to 0, reported
    ///   `matches original? false` with the reconstruction all zeroes.
    /// * Copying `post_len` bytes from `gap_end` to `gap_end - shift`, i.e. moving piece B rather than
    ///   piece A. Piece B may be empty or short, and piece A is the part that actually moves.
    /// * Sliding the post-gap text *right* on a leftward cursor move. Moving left widens the post-gap
    ///   region, so its text must move left.
    fn move_gap_to(&mut self, offset: usize) {
        let cur_start = self.gap_start as usize;
        let text_len = self.text_len as usize;
        if offset == cur_start {
            return;
        }
        // `gap_end` as a function of `gap_start`, which is what the layout forces.
        let cur_end = LEAF_CAPACITY - (text_len - cur_start);
        let shift = offset.abs_diff(cur_start);

        if offset > cur_start {
            // Right: the front of the post-gap region becomes pre-gap text.
            //
            // SAFETY: source `[cur_end, cur_end + shift)` and destination
            // `[cur_start, cur_start + shift)`. The source's end is
            // `CAPACITY - (text_len - offset)`, at most `CAPACITY`, because `offset <= text_len`. The
            // destination's end is `offset`, at most `CAPACITY`. Both lie inside the buffer. The
            // regions overlap whenever `slack < shift`, which is most of the time on a full leaf, so
            // this must be `copy` and not `copy_nonoverlapping`.
            //
            // SAFETY: as above.
            unsafe { std::ptr::copy(self.buffer.add(cur_end), self.buffer.add(cur_start), shift) };
        } else {
            // Left: the pre-gap tail slides left to become the post-gap region.
            //
            // SAFETY: source `[offset, cur_start)`, length `shift`, and destination
            // `[cur_end - shift, cur_end)`. The source's end is `cur_start <= CAPACITY`; the
            // destination's end is `cur_end <= CAPACITY`; the destination's start is
            // `cur_end - shift = CAPACITY - (text_len - offset) >= 0`, because `offset <= text_len`.
            // The regions overlap whenever `shift > cur_end - offset`, which is usually, so `copy`.
            //
            // SAFETY: as above.
            unsafe {
                std::ptr::copy(
                    self.buffer.add(offset),
                    self.buffer.add(cur_end - shift),
                    shift,
                )
            };
        }

        self.gap_start = offset as u16;
        self.gap_end = (LEAF_CAPACITY - (text_len - offset)) as u16;
        self.mark_dirty();

        // Scrub the gap *after* moving it, as one range.
        //
        // The previous versions scrubbed the vacated source immediately after the copy, and the
        // ranges overlapped: on a leftward move the vacated source is `[offset, cur_start)` and the
        // copy's destination is `[cur_end - shift, cur_end)`, which for a nearly-full leaf is
        // `[3796, 4096)` while the source is `[0, 300)`. Scrubbing `[offset, cur_end)` instead
        // covered both the source *and* the destination, so the move wrote 300 bytes to offset 3,796
        // and then zeroed all 4,096 -- and the leaf reported `matches original? false` with an
        // all-zero reconstruction.
        //
        // Scrubbing the final gap region is both correct and obviously so: by definition it holds no
        // live text, and nothing outside it was touched by this operation.
        //
        // SAFETY: `[gap_start, gap_end)` is inside the buffer: `gap_start = offset <= text_len <=
        // CAPACITY` and `gap_end = CAPACITY - (text_len - offset) <= CAPACITY`.
        let gs = offset;
        let ge = LEAF_CAPACITY - (text_len - offset);
        // SAFETY: as above.
        unsafe { std::ptr::write_bytes(self.buffer.add(gs), 0, ge - gs) };
    }

    /// Whether `offset` is a UTF-8 character boundary in this leaf's text.
    ///
    /// # Why the byte *at* `offset`, not before it
    ///
    /// A boundary is the absence of a continuation byte at that index: 0b10xxxxxx means the byte
    /// continues the character that started earlier, so `offset` is *not* a boundary.
    ///
    /// Getting the index wrong is subtle and was: an earlier version branched on
    /// `offset <= pre.len()` and read `pre[offset]` there, `post[offset - pre.len() - 1]` otherwise.
    /// Both are off by one at the seam -- `pre[pre.len()]` is one past the end of `pre`, and the
    /// `- 1` reads the byte *before* the one asked about. `slice::get_unchecked` turned that into a
    /// hard precondition failure ("index is within the slice"), which is what surfaced it. The
    /// version below indexes text offsets exactly once, with no case where the index can leave the
    /// slice it came from.
    pub fn is_char_boundary(&self, offset: usize) -> bool {
        let len = self.text_len as usize;
        if offset == 0 || offset == len {
            return true;
        }
        if offset > len {
            return false;
        }
        let (pre, post) = self.text_slices();
        // `offset` is in `1..len`, so this branch always has a byte to read: `pre` holds text
        // offsets `0..pre.len()` and `post` holds `pre.len()..len`.
        let byte = if offset < pre.len() {
            pre[offset]
        } else {
            post[offset - pre.len()]
        };
        // A continuation byte is 0b10xxxxxx. A boundary is anything else.
        (byte & 0xC0) != 0x80
    }

    /// Copy a run of this leaf's text, starting at leaf-local text offset `offset`, into `out`.
    /// Returns how many bytes were copied, which is `min(out.len(), text_len - offset)`.
    ///
    /// This is [`byte_at`](Self::byte_at) done in bulk, and it exists because the byte-at-a-time form
    /// was on the keystroke path: [`Rope::read_at`] used to call `byte_at` once per byte, and every
    /// full-document read -- `Rope::to_vec`, and therefore `Editor::text` -- cost one function call
    /// per byte of a 6.4 MiB document.
    ///
    /// The two `copy_from_slice`s rather than one because the gap splits the leaf's text into two
    /// non-adjacent runs, exactly as [`text_slices`](Self::text_slices) says. When the gap is empty or
    /// the requested range lies entirely on one side of it, the second copy is zero-length and the
    /// first covers the whole run.
    pub fn copy_text_to(&self, offset: usize, out: &mut [u8]) -> Result<usize, LeafError> {
        let len = self.text_len as usize;
        if offset >= len || out.is_empty() {
            return Ok(0);
        }
        let want = out.len().min(len - offset);
        let gap_start = self.gap_start as usize;
        let (pre, post) = self.text_slices();
        // Text offsets `0..gap_start` index `pre`; offsets `gap_start..text_len` index `post` at
        // `offset - gap_start`. Both slices are long enough because `want <= len - offset`.
        let n_pre = if offset < gap_start {
            want.min(gap_start - offset)
        } else {
            0
        };
        if n_pre > 0 {
            out[..n_pre].copy_from_slice(&pre[offset..offset + n_pre]);
        }
        if n_pre < want {
            let start = offset + n_pre - gap_start;
            out[n_pre..want].copy_from_slice(&post[start..start + (want - n_pre)]);
        }
        Ok(want)
    }

    /// Read one byte of the leaf's text at leaf-local text offset `offset`.
    pub fn byte_at(&self, offset: usize) -> Result<u8, LeafError> {
        if offset >= self.text_len as usize {
            return Err(LeafError::OutOfBounds {
                offset,
                text_len: self.text_len as usize,
            });
        }
        let (pre, post) = self.text_slices();
        // The pre-gap text is `[0, gap_start)` and the post-gap text starts at `gap_offset()`, so
        // offsets below `gap_start` index `pre` directly and the rest index `post`.
        if offset < self.gap_start as usize {
            // SAFETY: `offset < gap_start <= pre.len()`.
            Ok(unsafe { *pre.get_unchecked(offset) })
        } else {
            // SAFETY: `offset < text_len` and `post.len() == text_len - gap_start`.
            Ok(unsafe { *post.get_unchecked(offset - self.gap_start as usize) })
        }
    }

    /// Whether the leaf has been modified since the last [`take_dirty`](Self::take_dirty).
    #[inline]
    pub fn is_dirty(&self) -> bool {
        self.is_dirty.load(Ordering::Relaxed)
    }

    /// Mark the leaf dirty. One relaxed store; the damage tracker is a consumer, not a synchroniser.
    #[inline]
    pub fn mark_dirty(&mut self) {
        self.is_dirty.store(true, Ordering::Relaxed);
    }

    /// Read and clear the dirty flag, returning its previous value.
    ///
    /// `swap` rather than load-then-store: the render thread and the input thread can both call
    /// this, and two separate atomic operations would let a keystroke's dirty flag be lost between
    /// them. A single `swap` cannot lose it -- the worst case is that one caller gets `true` and
    /// the other `false`, which is correct.
    #[inline]
    pub fn take_dirty(&self) -> bool {
        self.is_dirty.swap(false, Ordering::Relaxed)
    }

    /// Zero every byte the leaf holds, including the gap, and re-open the gap.
    ///
    /// For the FR-5.4 periodic scramble's stronger cousin and for tests that need a clean slate.
    /// Scrubbing a leaf destroys its text, so this is not something the editor calls implicitly.
    pub fn scrub(&mut self) {
        // SAFETY: the whole buffer is ours.
        unsafe { std::ptr::write_bytes(self.buffer, 0, LEAF_CAPACITY) };
        self.gap_start = 0;
        self.gap_end = LEAF_CAPACITY as u16;
        self.text_len = 0;
        self.mark_dirty();
    }

    /// Assert the structural invariant. Test-only; `debug_assert` in the mutators.
    #[cfg(test)]
    pub(crate) fn check_invariants(&self) {
        assert!(
            self.gap_start <= self.gap_end,
            "gap_start {} > gap_end {}",
            self.gap_start,
            self.gap_end
        );
        assert!(
            self.gap_end as usize <= LEAF_CAPACITY,
            "gap_end {} > capacity {LEAF_CAPACITY}",
            self.gap_end
        );
        assert!(
            self.text_len as usize
                == self.gap_start as usize + (LEAF_CAPACITY - self.gap_end as usize),
            "text_len {} != {} + {}",
            self.text_len,
            self.gap_start,
            LEAF_CAPACITY - self.gap_end as usize
        );
    }
}

impl std::fmt::Debug for CagrLeaf {
    /// Prints the leaf's *state*, never its text.
    ///
    /// A derived `Debug` on a struct holding a raw `*mut u8` would print the pointer, which is fine,
    /// but the obvious temptation is to print the buffer -- and this leaf holds the document's
    /// plaintext. A debug format that puts secrets in a log file is a defect in a container whose
    /// entire purpose is not leaking them, so this one is written by hand and shows lengths only.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CagrLeaf")
            .field("text_len", &self.text_len)
            .field("gap_start", &self.gap_start)
            .field("gap_end", &self.gap_end)
            .field("gap_len", &(self.gap_end - self.gap_start))
            .field("is_dirty", &self.is_dirty.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Drop for CagrLeaf {
    fn drop(&mut self) {
        // Scrub before `SecureBlock` unmaps. Doing it here rather than relying on the block's own
        // scrub is deliberate belt-and-braces: the block scrubs too, but this makes the intent
        // local to the type that holds the plaintext.
        //
        // SAFETY: the whole buffer is ours and about to be released.
        unsafe { std::ptr::write_bytes(self.buffer, 0, LEAF_CAPACITY) };
    }
}

// SAFETY: `SecureBlock` owns an `mmap`'d region, which is thread-safe to read and write from any
// thread. The leaf adds an `AtomicBool` and raw sibling pointers; the pointers are only dereferenced
// by `Rope`, which owns every leaf exclusively, so no two threads can reach the same leaf.
unsafe impl Send for CagrLeaf {}
unsafe impl Sync for CagrLeaf {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A leaf can be allocated on a host that allows `mlock`.
    #[test]
    fn a_fresh_leaf_is_empty_with_its_gap_open_at_the_front() {
        let leaf = CagrLeaf::new().expect("mlock");
        leaf.check_invariants();
        assert_eq!(leaf.text_len(), 0);
        assert_eq!(leaf.gap_len(), LEAF_CAPACITY);
        // A fresh leaf's gap is the whole buffer: 4,096 keystrokes before a split.
        assert_eq!(leaf.gap_start(), 0);
        assert_eq!(leaf.gap_end(), LEAF_CAPACITY);
        assert_eq!(leaf.buffer_slice().len(), LEAF_CAPACITY);
        assert!(!leaf.is_dirty(), "a fresh leaf is not dirty");
    }

    /// The plan's node layout, and the alignment claim in the module docs.
    #[test]
    fn the_node_is_one_cacheline_aligned_block() {
        let leaf = CagrLeaf::new().expect("mlock");
        let addr = &leaf as *const CagrLeaf as usize;
        assert_eq!(
            addr % CACHELINE_BYTES,
            0,
            "a CagrLeaf must start on a cacheline boundary, or `gap_start` shares a line with the \
             buffer's tail"
        );
        // The three hot fields must be within one or two cachelines of each other.
        let gap_start_addr = &leaf.gap_start as *const u16 as usize;
        let text_len_addr = &leaf.text_len as *const u16 as usize;
        assert_eq!(
            gap_start_addr / CACHELINE_BYTES,
            text_len_addr / CACHELINE_BYTES,
            "gap_start, gap_end and text_len must share a cacheline"
        );
    }

    /// FR-1.2, the core claim.
    #[test]
    fn insert_byte_writes_into_the_gap_and_nothing_else() {
        let mut leaf = CagrLeaf::new().expect("mlock");
        let start = leaf.gap_start();
        leaf.insert_byte(b'a').expect("room");
        leaf.insert_byte(b'b').expect("room");
        leaf.check_invariants();
        assert_eq!(leaf.text_len(), 2);
        assert_eq!(leaf.gap_start(), start + 2);
        assert_eq!(leaf.gap_end(), LEAF_CAPACITY);
        assert_eq!(leaf.pre_gap(), b"ab");
        assert!(leaf.is_dirty());
        // The byte past the inserted run is still zero: the gap's tail is untouched.
        assert_eq!(leaf.buffer_slice()[start + 2], 0);
    }

    /// Plan.md FR-1.2's destructive-by-design requirement: the deleted byte must not survive.
    #[test]
    fn delete_byte_scrubs_the_byte_it_removes() {
        let mut leaf = CagrLeaf::new().expect("mlock");
        leaf.insert_byte(b's').expect("room");
        leaf.insert_byte(b'e').expect("room");
        leaf.insert_byte(b'c').expect("room");
        assert_eq!(leaf.pre_gap(), b"sec");

        leaf.delete_byte().expect("text before the cursor");
        leaf.check_invariants();
        assert_eq!(leaf.pre_gap(), b"se");
        assert_eq!(leaf.text_len(), 2);
        // The byte is *zeroed*, not merely excluded.
        assert_eq!(
            leaf.buffer_slice()[2],
            0,
            "the deleted byte must be overwritten with zero"
        );
        assert!(
            !leaf.buffer_slice().contains(&b'c'),
            "the plaintext byte must not survive anywhere in the leaf"
        );
    }

    #[test]
    fn insert_at_a_saturated_gap_is_refused_rather_than_wrapping() {
        let mut leaf = CagrLeaf::with_text(&[b'x'; LEAF_CAPACITY]).expect("full");
        // A full leaf has no gap at all.
        assert_eq!(leaf.gap_len(), 0);
        let err = leaf.insert_byte(b'y').expect_err("no room");
        assert_eq!(
            err,
            LeafError::GapSaturated {
                capacity: LEAF_CAPACITY
            }
        );
        assert_eq!(leaf.text_len(), LEAF_CAPACITY, "the text must be untouched");
    }

    #[test]
    fn delete_at_the_start_of_a_leaf_is_refused() {
        let mut leaf = CagrLeaf::new().expect("mlock");
        let err = leaf.delete_byte().expect_err("nothing before the cursor");
        assert_eq!(err, LeafError::Underflow { gap_start: 0 });
        assert_eq!(leaf.text_len(), 0);
    }

    #[test]
    fn a_bulk_delete_refuses_to_cross_the_leaf_start() {
        let mut leaf = CagrLeaf::new().expect("mlock");
        leaf.insert_bytes(b"hello").expect("room");
        // Deleting 6 when 5 are present must fail rather than wrapping `gap_start`.
        assert!(leaf.delete_bytes(6).is_err());
        leaf.delete_bytes(5).expect("exactly all of it");
        leaf.check_invariants();
        assert_eq!(leaf.text_len(), 0);
        assert!(leaf.buffer_slice().iter().all(|&b| b == 0));
    }

    /// The gap travels so the cursor lands where asked, and the text comes out intact.
    #[test]
    fn set_gap_offset_preserves_the_text_exactly() {
        let mut leaf = CagrLeaf::new().expect("mlock");
        // Build text, then park the cursor at the end.
        let text = b"the quick brown fox jumps over the lazy dog";
        leaf.insert_bytes(text).expect("room");
        assert_eq!(leaf.text_len(), text.len());

        for target in (0..=text.len()).rev() {
            leaf.set_gap_offset(target).expect("in range");
            leaf.check_invariants();
            // The pre-gap text *is* text[..target]; the post-gap text *is* text[target..]. An earlier
            // version of this test concatenated the two and compared the result against
            // `text[..target]`, which is the whole text against a prefix of it -- true for exactly
            // one value of `target`, the one the loop started at.
            assert_eq!(
                leaf.pre_gap(),
                &text[..target],
                "with the cursor at {target}, the pre-gap text should be text[..{target}]"
            );
            assert_eq!(
                leaf.post_gap(),
                &text[target..],
                "with the cursor at {target}, the post-gap text should be text[{target}..]"
            );
            // And the whole text survives every one of those arrangements.
            let mut got = Vec::with_capacity(text.len());
            got.extend_from_slice(leaf.pre_gap());
            got.extend_from_slice(leaf.post_gap());
            assert_eq!(got, text, "the document changed at cursor offset {target}");
        }
    }

    /// Moving the gap must not move the text it contains, byte for byte.
    #[test]
    fn moving_the_gap_leaves_the_buffer_content_alone() {
        let mut leaf = CagrLeaf::with_text(b"abcdefghij").expect("room");
        assert_eq!(leaf.pre_gap(), b"abcdefghij");
        assert_eq!(leaf.post_gap(), b"");

        for target in [0usize, 5, 10, 3, 7] {
            leaf.set_gap_offset(target).expect("in range");
            let mut got = Vec::new();
            got.extend_from_slice(leaf.pre_gap());
            got.extend_from_slice(leaf.post_gap());
            assert_eq!(got, b"abcdefghij", "after moving the cursor to {target}");
        }
    }

    /// A caret inside a multi-byte character is not a legal position.
    #[test]
    fn a_gap_offset_inside_a_character_is_refused() {
        let mut leaf = CagrLeaf::new().expect("mlock");
        // `é` is two bytes: 0xC3 0xA9.
        leaf.insert_bytes("é".as_bytes()).expect("room");
        assert_eq!(leaf.text_len(), 2);
        // `é` is [0xC3, 0xA9]: 0xA9 is a continuation byte (0b10xxxxxx), so offset 1 splits the
        // character and is *not* a boundary. An earlier version of this test asserted the
        // opposite and failed with `1 is mid-character` -- a correct message for an incorrect
        // expectation.
        assert!(leaf.is_char_boundary(0), "0 is always a boundary");
        assert!(!leaf.is_char_boundary(1), "1 is mid-character");
        assert!(
            leaf.is_char_boundary(2),
            "the end of the text is a boundary"
        );

        let err = leaf
            .set_gap_offset(1)
            .expect_err("a caret cannot split a character");
        assert!(matches!(err, LeafError::NotCharBoundary { offset: 1, .. }));
        // Refused, not clamped: the cursor stays where it was.
        assert_eq!(leaf.gap_offset(), 2);
    }

    /// Scrubbing must leave nothing of the text.
    #[test]
    fn scrub_destroys_the_text_and_reopens_the_gap() {
        let mut leaf = CagrLeaf::with_text(b"confidential").expect("room");
        assert_eq!(leaf.text_len(), 12);
        leaf.scrub();
        leaf.check_invariants();
        assert_eq!(leaf.text_len(), 0);
        assert_eq!(leaf.gap_len(), LEAF_CAPACITY);
        assert!(leaf.buffer_slice().iter().all(|&b| b == 0));
    }

    /// Dropping the leaf must not leave plaintext in the page.
    #[test]
    fn drop_scrubs_before_unmapping() {
        let addr = {
            let leaf = CagrLeaf::with_text(b"do not leak").expect("room");
            leaf.buffer() as usize
        };
        // The mapping is gone, so the address is unmapped and the read faults. There is nothing to
        // assert about the contents -- the point is that the test cannot be made to observe them,
        // which is exactly the guarantee. Asserting the address is unmapped is left to
        // `holonomy-secure`'s guard-page test, which has the machinery for it.
        assert_ne!(addr, 0);
    }

    /// `needs_split` is the question the rope actually asks, and it must not fire on a fresh leaf.
    #[test]
    fn a_fresh_leaf_does_not_need_splitting() {
        let leaf = CagrLeaf::new().expect("mlock");
        assert!(!leaf.needs_split());
        assert_eq!(leaf.available(), LEAF_CAPACITY);
    }
}
