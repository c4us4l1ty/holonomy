//! The edit record: current offset → saved offset, for a source that holds the document as it was.
//!
//! # Why this exists
//!
//! Part 7 said a `LeafSource` keyed by document offset is only true of an *unmodified* document. Part 8 found
//! why write-back alone does not fix it: **an insert moves a byte across a leaf boundary**, so correcting one
//! leaf leaves its neighbour one byte short at its new offset. Part 9 built the `(offset, ±delta)` log part 8
//! recommended, and removed it as wrong.
//!
//! # The finding that fixes it: fold BACKWARDS
//!
//! Part 9's fold walked edits **forwards**, carrying a running `delta`. That is wrong as soon as two edits
//! overlap, and the reason is worth stating precisely:
//!
//! > **Each edit's `at` is an offset in the document as it was *before that edit*.** So the edits are in a
//! > *chain of coordinate systems*, not in one. Walking forwards, an earlier edit's shift has already been
//! > applied to `delta` — but a *later* edit that spans backwards past an earlier one is measured against
//! > coordinates the forward walk has already moved past.
//!
//! Walking **backwards** removes the problem entirely, because each step converts an offset from one
//! coordinate system to the *previous* one, and the previous one is exactly what the next edit's `at` uses:
//!
//! ```text
//! cur = current_offset                      # in the final document
//! for edit in REVERSE application order:
//!     if cur < edit.at:                 pass        # before the edit; unmoved
//!     elif cur < edit.at + inserted:    return None  # an inserted byte: no saved origin
//!     else:                             cur = cur - inserted + removed
//! ```
//!
//! **Gated against a brute-force model at every offset**, over several scripts — because a worked example
//! validates exactly one point, and this fold's whole difficulty is at the boundaries.
//!
//! # It translates, and since part 12 it also replays content
//!
//! Turning the *bytes* a source holds into the bytes the rope wants needs each edit's inserted text spliced
//! over the fetched range and its removed text skipped. **Part 10 built and gated the translation alone,
//! because translation is easy to gate exhaustively against a model and content replay is easy to get
//! subtly wrong** — and building the first alone is what made the second checkable. Part 12 wrote the
//! second: [`saved_runs`](EditRecord::saved_runs) says which saved bytes go where, and
//! [`fill_typed`](EditRecord::fill_typed) writes the rest, and [`replay`](EditRecord::replay) is the two
//! composed. `Rope::fault_leaf` is the version that fetches straight into the leaf's page-locked block.
//!
//! **The record is the whole of the repair.** Part 13 measured that per-leaf write-back is not a
//! substitute for it — a shift is not a leaf-local event — and `Rope::commit` is the other end: a
//! commit-time whole-document write, after which the source holds the current document and
//! [`clear`](EditRecord::clear) is the whole of what remains to do to the record.
//!
//! # This is `UndoStack`'s structure, built once
//!
//! Part 9 noted that the objection to carrying content was "do not build this twice". The answer is to build
//! it once: this record *is* the undo record — same edits, same bytes, same order — so undo should read it
//! rather than maintain a second one.

/// One edit: what was removed, and what went in its place.
///
/// **The bytes are the point.** Part 9 proved a record of `(offset, ±delta)` is not enough — resolving which
/// bytes a removal consumed needs the content — so `removed` carries what came out and `inserted` what went
/// in. The *values* are not read by [`EditRecord::to_saved`]; they are what makes the record complete enough
/// for content replay, and what lets undo read this instead of building its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Offset **before this edit** — the rope's own offset when it applied.
    pub at: usize,
    /// The bytes this edit removed.
    pub removed: Bytes,
    /// The bytes this edit inserted.
    pub inserted: Bytes,
}

/// How many bytes a [`Bytes`] holds without allocating.
pub const INLINE_BYTES: usize = 16;

/// How many [`Edit`]s a fresh [`EditRecord`] reserves room for.
///
/// # The arithmetic, because a reserve chosen by feel is a reserve that is wrong somewhere
///
/// Two gates have to fit without the array ever reallocating, and they need different amounts for
/// different reasons:
///
/// * **Typing.** [`EditRecord::push`] merges consecutive inserts, so 4,000 characters is
///   `4,000 / INLINE_BYTES` = **250** entries.
/// * **Deleting.** Consecutive backspaces **cannot** merge, and the reason is worth stating rather than
///   working around: a backspace deletes at `cursor - 1` and then moves the cursor back, so successive
///   `at`s *descend*. Merging them would need the removed runs concatenated in document order, which is
///   the reverse of the order they were deleted — correct, but easy to get backwards, and a record that
///   concatenates deleted bytes in the wrong order restores the wrong text. **So 1,000 deletions are
///   1,000 entries**, and the reserve is set by this gate, not the other.
///
/// **1,536 rather than 1,024**, and the extra 512 is not slack for its own sake: `no_alloc.rs` types 4,000
/// characters and *then* deletes 1,000 times on the same rope, so the peak is `250 + 1,000 = 1,250`
/// entries, and a reserve of exactly 1,000 would reallocate on the last 250 deletions.
///
/// # What it costs, stated rather than discovered
///
/// **1,536 x 56 B = 86 KB per rope, allocated once and never freed until the rope drops.** On the
/// product's 6.40 MiB document that is 0.9 %, and on an *empty* editor it is 3 % of the 1.90 MiB
/// baseline — **a standing cost paid by every session to serve the editing gate**, which is a real
/// trade and not a free one.
///
/// # What would remove it
///
/// **Recording only when the rope has a source**, because a fully resident rope is never faulted and so
/// never replays. It is tempting and it is **not sound for the resident-then-evict path**: a rope loaded
/// with `from_text`, edited, and *then* evicted has unrecorded edits, and the first fetch would replay an
/// empty record over a shifted document — the exact failure part 13 measured. Compaction against a
/// write-back ([`compact_before`](EditRecord::compact_before)) is what actually bounds this, and that is
/// part 15.
pub const RECORD_RESERVE: usize = 1536;

/// Up to [`INLINE_BYTES`] bytes stored in the value, spilling to a `Vec` beyond that.
///
/// # Why this exists, and it is FR-1.2 rather than tidiness
///
/// **A keystroke is one byte, and one `vec![ch]` is one heap allocation per keystroke.** With `Vec<u8>`
/// fields, recording an insert allocated on every single character typed, and
/// `tests/session_no_alloc.rs` reported exactly that: *typing 1000 characters performed 1000
/// allocations*. The gate is right and the code was wrong — FR-1.2's claim is that the edit path does
/// not allocate, and a record that allocates is an edit path that allocates.
///
/// # Why 16, and not 1
///
/// **A `char` is at most 4 UTF-8 bytes, so 1 would be the minimum that fixes the keystroke — and it
/// would allocate on the next thing that happens.** Typing is not the only small edit: a paste of a
/// short word, an auto-indent, a table cell's newline-and-padding, and `insert_at` of a pasted line all
/// land under 16 and should not allocate either. 16 bytes is two cache lines' worth of slack on an
/// `Edit` that is already 56 bytes, and it covers every realistic small edit without a heap.
///
/// # What it costs
///
/// **`Edit` grows by 32 bytes and gains a branch per read.** The branch is on a discriminant in the same
/// cache line as the length, so the common path is one compare. The size is paid once per *recorded*
/// edit, and `compact_before` exists to stop that count growing without bound — which is why the trade is
/// affordable at all. A design with no compaction would not be.
///
/// # Why not a `SmallVec` dependency
///
/// **One enum and a `Deref`.** This project already refuses dependencies for smaller wins than this one,
/// and a hand-rolled 40-line type has no version skew, no feature flags, and no behaviour that depends on
/// the dependency's.
///
/// # Spilling is not a special case anywhere
///
/// [`Deref`](std::ops::Deref) to `[u8]` means every reader — `replay`, `fill_typed`, `typed_byte`, the
/// length checks — is written against a slice and is **identical whether the bytes are inline or on the
/// heap.** That is the property worth having: the spill cannot change an answer, because no code path
/// knows it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bytes {
    /// Held in the value. `len` is a `u8` so the whole thing is one discriminant plus one length byte.
    Inline {
        /// How many of the `INLINE_BYTES` slots are live.
        len: u8,
        /// The bytes. Only the first `len` are meaningful; the rest are always zero.
        buf: [u8; INLINE_BYTES],
    },
    /// Too long to hold, so held on the heap.
    ///
    /// **`Box<[u8]>` rather than `Vec<u8>`, and it is about `Edit`'s size.** A `Vec` is 24 bytes, a fat
    /// slice is 16, and `Bytes` is the larger of its two arms plus a discriminant — so the choice moves
    /// `Bytes` from 32 to 24 and **`Edit` from 72 to 56.** At [`RECORD_RESERVE`] entries that is 86 KB of
    /// reserve rather than 110 KB, on a structure that exists on every rope in the process.
    ///
    /// The cost is a realloc when a `Vec` is converted in (`Vec`'s capacity is usually larger than its
    /// length), which only happens for an insert longer than [`INLINE_BYTES`] — a paste, not a keystroke.
    Heap(Box<[u8]>),
}

impl Default for Bytes {
    /// **Empty and inline**, so a `Default` cannot allocate — which matters because `mem::take` in
    /// [`EditRecord::push`] uses it, and `push` is on the keystroke path.
    fn default() -> Self {
        Self::empty()
    }
}

impl Bytes {
    /// An empty value, which **never allocates**.
    pub const fn empty() -> Self {
        Self::Inline {
            len: 0,
            buf: [0; INLINE_BYTES],
        }
    }

    /// Whether there are no bytes at all.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The bytes, wherever they live.
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Inline { len, buf } => &buf[..*len as usize],
            Self::Heap(b) => b,
        }
    }
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl From<&[u8]> for Bytes {
    fn from(s: &[u8]) -> Self {
        // **The empty case takes the inline path deliberately**, so `Edit::insert(at, b"")` cannot
        // allocate either -- an empty `Vec` does not allocate on its own, but routing it through `Heap`
        // would put an empty edit in the heap-spilling arm for no reason.
        if s.len() <= INLINE_BYTES {
            let mut buf = [0u8; INLINE_BYTES];
            buf[..s.len()].copy_from_slice(s);
            Self::Inline {
                len: s.len() as u8,
                buf,
            }
        } else {
            Self::Heap(s.into())
        }
    }
}

impl<const N: usize> From<&[u8; N]> for Bytes {
    /// **So `b"literal"` works**, which is how every call site spells a small edit. Without it a caller
    /// writes `&b"hello"[..]` at every use, and the noise buys nothing: the slice path is the same one.
    fn from(a: &[u8; N]) -> Self {
        Self::from(&a[..])
    }
}

impl<const N: usize> From<[u8; N]> for Bytes {
    /// **A fixed-size array goes through the slice path**, so a one-byte delete is inlined by the same
    /// rule as a one-byte insert. Without this the two keystroke directions would take different paths
    /// through the same type, which is a distinction with no meaning behind it.
    fn from(a: [u8; N]) -> Self {
        Self::from(&a[..])
    }
}

impl From<Vec<u8>> for Bytes {
    /// **Adopts the `Vec` without copying**, so a caller that already has one pays nothing.
    fn from(v: Vec<u8>) -> Self {
        if v.len() <= INLINE_BYTES {
            Self::from(v.as_slice())
        } else {
            Self::Heap(v.into_boxed_slice())
        }
    }
}

impl Edit {
    /// An insert of `inserted` at `at`.
    pub fn insert(at: usize, inserted: impl Into<Bytes>) -> Self {
        Self {
            at,
            removed: Bytes::empty(),
            inserted: inserted.into(),
        }
    }

    /// A delete of `removed` at `at`.
    pub fn delete(at: usize, removed: impl Into<Bytes>) -> Self {
        Self {
            at,
            removed: removed.into(),
            inserted: Bytes::empty(),
        }
    }

    /// A replacement: `removed` out, `inserted` in, at the same offset.
    pub fn replace(at: usize, removed: impl Into<Bytes>, inserted: impl Into<Bytes>) -> Self {
        Self {
            at,
            removed: removed.into(),
            inserted: inserted.into(),
        }
    }
}

/// An ordered record of edits, able to translate a current offset into a saved one.
///
/// **Compaction is what keeps it bounded**, and it is not an optimisation: an unbounded record on a document
/// edited all afternoon is a second copy of the document.
#[derive(Debug, Clone, Default)]
pub struct EditRecord {
    /// In application order. **Never reordered** — the reverse fold walks it backwards, which is only
    /// meaningful if the order is the order they were applied.
    edits: Vec<Edit>,
}

impl EditRecord {
    /// An empty record — a document nobody has edited, where current and saved offsets are the same number.
    ///
    /// **Reserved to [`RECORD_RESERVE`]**, and the number is load-bearing rather than a round figure:
    /// it is sized from the two allocation gates' arithmetic so the array never reallocates while
    /// someone types or deletes. Reserving less means a reallocation on the edit path, which is the
    /// thing FR-1.2 forbids. See [`RECORD_RESERVE`] for the arithmetic and for what the reserve costs.
    pub fn new() -> Self {
        Self {
            edits: Vec::with_capacity(RECORD_RESERVE),
        }
    }

    /// Record an edit.
    ///
    /// ## Consecutive typing is one entry, not one per character
    ///
    /// **An insert immediately after the previous insert's bytes is merged into it**, up to
    /// [`INLINE_BYTES`]. This is not a compaction heuristic — it is the same boundary an editor draws
    /// anyway, and it is forced by a measurement:
    ///
    /// `no_alloc.rs` types 4,000 characters and requires **zero reallocations**. With one `Edit` per
    /// character the record's `Vec` doubled eleven times on the way, which is eleven reallocations of an
    /// array of 56-byte entries — copying up to 224 KB across a typing burst. Merging on the character
    /// boundary gives 250 entries for the same 4,000 characters, which the reserve below covers.
    ///
    /// **The merge is also what keeps typing allocation-free at all.** `Bytes` inlines up to
    /// [`INLINE_BYTES`], and a character is one byte, so a 16-character run is one `Edit` with an inline
    /// buffer and **no heap at all**. Without merging, each keystroke would be its own `Edit` and the
    /// inline buffer would be paying for nothing.
    ///
    /// ## Why the merge is *not* safe to generalise
    ///
    /// **It applies only to two adjacent pure inserts.** A deletion in between, or an insert that is not
    /// exactly at the previous one's end, must not merge: `at` is an offset in the document *as it was
    /// before that edit*, so merging across a deletion would leave the earlier edit's `at` describing a
    /// coordinate system that no longer exists. That is the same class of bug parts 9 and 11 found, and
    /// the guard is written to make it impossible rather than to be revisited.
    ///
    /// ## What this does *not* bound
    ///
    /// The record is still **O(edits since the last compaction)**, and nothing calls
    /// [`compact_before`](Self::compact_before) yet — a whole-document write-back is what would call it,
    /// and that is part 15. **So a session that types without ever committing grows a record
    /// proportionally to what was typed.** It is bounded by typing rather than by document size, which is
    /// far better, but it is not bounded, and it should not be described as if it were.
    pub fn push(&mut self, edit: Edit) {
        // Merge into the previous entry when this is the character-wise continuation of it.
        if let Some(last) = self.edits.last_mut() {
            let continues = last.removed.is_empty()
                && edit.removed.is_empty()
                && last.at + last.inserted.len() == edit.at
                && last.inserted.len() + edit.inserted.len() <= INLINE_BYTES;
            if continues {
                // **Appended in place, never round-tripped through a `Vec`.** The first version did
                // `mem::take(..).into_vec()` then `extend_from_slice` then `Bytes::from(Vec)`, which is
                // three heap operations on the keystroke path — `into_vec` allocates unconditionally for
                // an inline value. `session_no_alloc.rs` reported *938 allocations for 1,000
                // keystrokes*, which is the number that found it. The `continues` guard already bounds
                // the result by `INLINE_BYTES`, so the inline arm is the one that runs.
                let old = last.inserted.len();
                match &mut last.inserted {
                    Bytes::Inline { len, buf } => {
                        buf[old..old + edit.inserted.len()].copy_from_slice(&edit.inserted);
                        *len = (old + edit.inserted.len()) as u8;
                    }
                    // **Reachable, and it rebuilds rather than appends.** A `Box<[u8]>` is fixed-length,
                    // so extending one means a fresh allocation and a copy. That is fine because it is
                    // only reached when the previous entry is already longer than `INLINE_BYTES` -- a
                    // paste being extended by one character, not a keystroke on its own.
                    Bytes::Heap(v) => {
                        let mut grown = v.to_vec();
                        grown.extend_from_slice(&edit.inserted);
                        *v = grown.into_boxed_slice();
                    }
                }
                return;
            }
        }
        self.edits.push(edit);
    }

    /// How many edits are recorded.
    pub fn len(&self) -> usize {
        self.edits.len()
    }

    /// Whether no edit is recorded.
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// **Test-only view of [`typed_byte`](Self::typed_byte)**, which is otherwise private.
    ///
    /// Exists so `to_saved_and_typed_byte_never_disagree` can assert the two walks against each other
    /// directly. **That assertion is the gate for a bug part 12 could not have caught**: the two were
    /// implemented separately, each gated on its own terms, and each was individually defensible while
    /// the pair disagreed — because nothing ever asked them the same question. A test that can only reach
    /// them through `replay` sees the *consequence* (an unlocatable byte) and has to infer the cause; this
    /// lets it ask both and compare.
    /// **A `pub` method rather than a feature flag, and that is deliberate.** `#[cfg(test)]` would not reach
    /// an integration test, and a `record-introspection` feature would be reachable from a
    /// default-features build — which is exactly what `crates/holonomy/tests/release_artifact.rs` exists to
    /// prevent. Two words of dead code in the shipping binary are the cheaper trade, and this is
    /// `#[doc(hidden)]` so it does not read as API.
    #[doc(hidden)]
    pub fn typed_byte_for_test(&self, q: usize) -> Option<(usize, usize)> {
        self.typed_byte(q)
    }

    /// The edits, in application order.
    pub fn edits(&self) -> &[Edit] {
        &self.edits
    }

    /// Net change to the document's length.
    pub fn net_delta(&self) -> isize {
        self.edits
            .iter()
            .map(|e| e.inserted.len() as isize - e.removed.len() as isize)
            .sum()
    }

    /// The saved offset holding what is currently at `current`, or `None` if that byte is **inserted
    /// content with no counterpart in the saved document**.
    ///
    /// Walks the record **backwards**, as documented at the module level. O(edits).
    ///
    /// # `None` means "the source cannot answer this byte"
    ///
    /// A byte that was typed has no saved origin, full stop — there is nothing to fetch. A caller must then
    /// take it from the rope, and **must not substitute a nearby offset**: `40 - 3 = 37` is a real byte of the
    /// saved document and *not* the byte at current 40, which is plausible text from one run early and the
    /// exact failure class this work exists to rule out.
    ///
    /// # It saturates rather than wrapping
    ///
    /// Where a number is returned it saturates at 0. A wrapped `usize` would name a different byte with no
    /// way to tell; saturating names the first one, which is wrong but visible.
    pub fn to_saved(&self, current: usize) -> Option<usize> {
        // `cur` walks backwards through the chain of coordinate systems, ending in the saved document.
        // `isize` because each step subtracts an insertion and adds a removal, and a delete larger than the
        // document is not a thing but a wrap here would be.
        let mut cur = current as isize;
        for e in self.edits.iter().rev() {
            let at = e.at as isize;
            let end = at + e.inserted.len() as isize;
            if cur < at {
                // Before the edit's own span: this edit did not move the byte we are tracking, and the
                // offset is unchanged going back a step.
                continue;
            }
            if cur < end {
                // **Inside the inserted run.** This byte was typed; the source has never seen it.
                return None;
            }
            // Past the edit: the byte we track sat `inserted - removed` further along before the edit.
            cur = cur - e.inserted.len() as isize + e.removed.len() as isize;
        }
        Some(cur.max(0) as usize)
    }

    /// The current offset of what the source holds at `saved`, or `None` if those bytes were **deleted**.
    ///
    /// The inverse of [`to_saved`](Self::to_saved), needed because a write-back has to say *where* in the
    /// source's coordinates a leaf's current bytes land — which is not its current offset.
    ///
    /// Walks **forwards**: a deleted byte has no current position, and a saved offset that an edit removed
    /// is exactly that.
    pub fn to_current(&self, saved: usize) -> Option<usize> {
        let mut at = saved as isize;
        for e in &self.edits {
            let end = e.at as isize + e.removed.len() as isize;
            if at < e.at as isize {
                // Before this edit: unmoved, and every later edit's `at` is in coordinates that include it.
                continue;
            }
            if at < end {
                // **These bytes were deleted**, so nothing sits at this saved offset any more.
                return None;
            }
            at += e.inserted.len() as isize - e.removed.len() as isize;
        }
        Some(at.max(0) as usize)
    }

    /// The byte currently at `q` if it was **typed**, as `(which edit, index into that edit's inserted
    /// bytes)`. `None` if `q` is a saved byte.
    ///
    /// # It is [`to_saved`](Self::to_saved) with the index kept, and that is the whole design
    ///
    /// Both walk the record **backwards**, and both convert one coordinate system into the previous one per
    /// step. When `to_saved` finds `cur` inside an edit's inserted run it knows two things: that `q` was
    /// typed, and *which* edit typed it and at what index. This returns the second and third; the first
    /// discards them. **One walk answers both questions, so there is no second reading of the record that
    /// could disagree with the first** — which is the failure class parts 9 and 11 were both instances of.
    ///
    /// ## What this replaced, and why it was wrong
    ///
    /// The previous version located each edit's inserted run **by position** — computing `(start, len)` in
    /// final coordinates and testing containment — and it was wrong in two separate ways, both found by
    /// part 15 asking a *two-edit* question that part 12's gates never did.
    ///
    /// **1. An edit's `at` is not a final coordinate.** `Edit::at` is an offset in the document as it was
    /// *before that edit*, so for any edit after the first length-changing one it names a different byte in
    /// final coordinates. Measured on `insert(10, "ZZZZZ")`, `insert(81927, "QQ")`, `insert(100000, "tail")`:
    ///
    /// ```text
    /// located the "QQ" run at:  (81932, 2)     <-- five bytes too far
    /// where it actually is:     (81927, 2)
    /// ```
    ///
    /// Five is exactly the first insert's length, re-applied to a coordinate that had already had it
    /// applied. Every byte of the run then came back unlocatable, so `fill_typed` returned `Unresolvable`
    /// and `Rope::fault_leaf` reported it as `SourceUnavailable` — a fault refusing on every leaf past the
    /// second edit, for no visible reason.
    ///
    /// **2. A run is not contiguous in the final document.** A later edit can land *inside* an earlier
    /// edit's inserted text: type `abc` at offset 0, then insert `X` at offset 1, and the `abc` bytes sit
    /// at 0, 2 and 3 with `X` between them. **No `(start, len)` describes that**, so no positional scheme
    /// can be correct here, however carefully the arithmetic is done.
    ///
    /// **The backwards walk has neither problem, because it never forms a run.** It arrives at `q` having
    /// already subtracted every later edit's delta, which is precisely the information the positional form
    /// was trying to reconstruct and could not.
    ///
    /// ## Why part 12's gates missed it
    ///
    /// Every gate in part 12 used **a single edit**, where the two coordinate systems coincide and neither
    /// bug is expressible. The record was exhaustively correct over its inputs and still wrong, and the
    /// lesson is worth more than the fix: *exhaustive coverage of one case is not coverage of a
    /// composition.* The gates that mattered were the ones that varied a script; this was a script of
    /// length one.
    ///
    /// **O(edits), and deliberately so.** Any faster scheme needs to know which edits are near `q`, which is
    /// the same positional question this function exists to answer, and answering it approximately is how
    /// parts 9 and 11 went wrong. This is called once per typed byte of a leaf-sized read, so the cost is
    /// bounded by the record rather than by the document.
    fn typed_byte(&self, q: usize) -> Option<(usize, usize)> {
        let mut cur = q as isize;
        for (i, e) in self.edits.iter().enumerate().rev() {
            let at = e.at as isize;
            let end = at + e.inserted.len() as isize;
            if cur < at {
                continue;
            }
            if cur < end {
                // **Inside this edit's inserted run, and `cur` is already in the coordinate system this
                // edit produced** — every later edit's delta has been subtracted. So `cur - at` is the index
                // into `inserted`, with no further translation.
                return Some((i, (cur - at) as usize));
            }
            cur = cur - e.inserted.len() as isize + e.removed.len() as isize;
        }
        None
    }

    /// Classify each byte of `[current_at, current_at + len)` as **saved at offset `s`** or **typed**.
    ///
    /// **This is the one place a window is understood, and `replay`, `saved_runs` and `fill_typed` are all
    /// three views of it.** They were written separately first and then folded together, because three
    /// separate readings of the same window is three opportunities for them to disagree — and a
    /// disagreement between the byte-producing path and the byte-*fetching* path is a document that is
    /// right when read one way and wrong when read the other.
    fn classify(&self, current_at: usize, len: usize) -> Vec<Option<usize>> {
        (0..len).map(|i| self.to_saved(current_at + i)).collect()
    }

    /// The stretches of `[current_at, current_at + len)` whose bytes come from the **saved** document, in
    /// ascending `out_at` order, each maximal.
    ///
    /// ## Why the rope needs this rather than [`replay`](Self::replay)
    ///
    /// **Because a source is handed a destination, not a return value.** `fetch_leaf(offset, out)` fills a
    /// buffer the *caller* owns — the rope's leaf — so a leaf-sized fault writes straight into the leaf's
    /// page-locked block with no intermediate `Vec`. [`replay`](Self::replay) cannot do that: it returns
    /// an owned `Vec` built by a closure that has to hand back a fresh buffer per run.
    ///
    /// So this answers *"which saved bytes go where"*, the caller fetches each run into place, and
    /// [`fill_typed`](Self::fill_typed) writes the rest. **Between them they are [`replay`](Self::replay),
    /// without the allocation.**
    ///
    /// **Empty is a real answer**, not an omission: a window that is entirely typed bytes needs nothing
    /// from the source at all, and that is the case `a_typed_byte_is_served_without_asking_the_source`
    /// counts.
    pub fn saved_runs(&self, current_at: usize, len: usize) -> Vec<SavedRun> {
        let mut runs: Vec<SavedRun> = Vec::new();
        for (i, s) in self.classify(current_at, len).into_iter().enumerate() {
            let Some(s) = s else { continue };
            match runs.last_mut() {
                // **Contiguous in both coordinate systems**, which is the condition for merging. Saved
                // offsets ascending by one *and* output positions adjacent -- neither alone is enough,
                // and checking only the first is how a run would come to claim bytes it does not own.
                Some(last) if last.out_at + last.len == i && last.saved_at + last.len == s => {
                    last.len += 1
                }
                _ => runs.push(SavedRun {
                    out_at: i,
                    saved_at: s,
                    len: 1,
                }),
            }
        }
        runs
    }

    /// Fill in the bytes of `[current_at, current_at + out.len())` that were **typed**, leaving the
    /// positions a source wrote untouched.
    ///
    /// **Partial by design.** This writes only what the record knows and returns; it does not clear the
    /// rest, because the rest is the source's to write and clearing it would destroy bytes fetched a
    /// moment earlier. `Rope::fault_leaf` runs [`saved_runs`](Self::saved_runs)'s fetches first and this
    /// second, which is the only order in which both halves are correct.
    ///
    /// Refuses a byte that is neither a saved byte nor a locatable typed one, for the reason
    /// [`ReplayError::Unresolvable`] gives.
    pub fn fill_typed(&self, current_at: usize, out: &mut [u8]) -> Result<(), ReplayError> {
        for (i, s) in self.classify(current_at, out.len()).into_iter().enumerate() {
            if s.is_some() {
                continue;
            }
            let q = current_at + i;
            let (ei, idx) = self
                .typed_byte(q)
                .ok_or(ReplayError::Unresolvable { at: q })?;
            out[i] = self.edits[ei].inserted[idx];
        }
        Ok(())
    }

    /// Turn the source's **saved** bytes into the **current** bytes for `[current_at, current_at + len)`.
    ///
    /// This is the other half of the record, and it is what part 10 left out. [`to_saved`](Self::to_saved)
    /// answers *where* the rope's bytes live; this produces *what they are*.
    ///
    /// `fetch(saved_at, saved_len)` supplies the source's bytes for a saved range, and **may be called more
    /// than once** when the window needs two disjoint saved ranges.
    ///
    /// ## There is no accumulator here, and that is the design
    ///
    /// Parts 9 and 11 each failed the same way: a running `delta` applied unconditionally across edits, which
    /// is wrong because **an edit only shifts positions after it**. So this does not walk at all. Every byte
    /// of the window is resolved by asking a question about *that byte*:
    ///
    /// * [`to_saved`](Self::to_saved) — is this a saved byte, and where does it live? Already exhaustively
    ///   gated in part 10.
    /// * [`typed_byte`](Self::typed_byte) — or was it typed, and which edit typed it? Positioned from
    ///   [`to_current`](Self::to_current), also gated.
    ///
    /// **Two verified primitives and no state.** The cost is O(`len` × `edits`) — for a leaf-sized read and a
    /// compacted record that is small, and it is bounded by the record rather than by the document, which is
    /// the property that matters here.
    ///
    /// ## Refusals
    ///
    /// A window starting on a typed byte is a **refusal, not a guess**: the rope already holds those bytes —
    /// typing is what put them there — so any answer would be a different byte. And a byte that is *neither*
    /// a saved byte nor a locatable typed one is [`ReplayError::Unresolvable`], which means the record cannot
    /// describe this document and **saying so is the only safe answer**.
    pub fn replay<F>(
        &self,
        current_at: usize,
        len: usize,
        mut fetch: F,
    ) -> Result<Vec<u8>, ReplayError>
    where
        F: FnMut(usize, usize) -> Vec<u8>,
    {
        // **Checked for overflow and then discarded**, which looks odd and is not: the window's end is
        // `current_at + len`, and an overflow would wrap it to a small number, so every byte index below
        // would be wrong in a way no length check catches. The error variant is the whole of the check.
        current_at
            .checked_add(len)
            .ok_or(ReplayError::WindowOutOfRange)?;
        let mut out = vec![0u8; len];
        if len == 0 {
            return Ok(out);
        }

        // **Through `saved_runs` and `fill_typed`, not around them.** The byte-producing path and the
        // byte-fetching path must read a window the same way, and three hand-written readings of one
        // window is three ways for them to drift apart.
        let runs = self.saved_runs(current_at, len);
        for r in &runs {
            let bytes = fetch(r.saved_at, r.len);
            if bytes.len() != r.len {
                return Err(ReplayError::ShortFetch {
                    want: r.len,
                    got: bytes.len(),
                });
            }
            out[r.out_at..r.out_at + r.len].copy_from_slice(&bytes);
        }
        self.fill_typed(current_at, &mut out)?;
        Ok(out)
    }

    /// Forget every edit, because the source now holds the document as it is.
    ///
    /// **The whole of what a commit does to the record, and it is correct only because the commit was
    /// whole-document.** A commit writes the current document to the source, so afterwards the source's
    /// bytes *are* the current document's bytes, the two coordinate systems coincide, and an empty record
    /// is the accurate description rather than a convenient lie.
    ///
    /// This is the contrast with [`compact_before`](Self::compact_before), which is the partial version and
    /// has no caller. A partial commit would drop only the prefix it wrote, and every survivor's `at` is
    /// measured in coordinates that include the dropped edits — so compaction is a **rebasing** operation
    /// and not a truncation, which is why the partial path is not the one the product takes. Whole-document
    /// is affordable here precisely because the document's maximum is 8,321,040 B and a commit is not the
    /// keystroke path; see `Rope::commit`.
    ///
    /// **The capacity is kept.** `clear` empties the `Vec` without releasing it, because the reserve is
    /// sized so the edit path never reallocates, and releasing it would make the next keystroke after a
    /// commit pay the allocation the reserve exists to prevent.
    pub fn clear(&mut self) {
        self.edits.clear();
    }

    /// Drop every edit whose whole effect lies before `saved`, which is safe once the rope has written those
    /// bytes back — and is what stops the record growing without bound.
    ///
    /// **Only wholly-before edits are dropped.** An edit that merely *starts* before `saved` still has bytes
    /// at or after it, and dropping it would corrupt the coordinates of every edit after it, because the
    /// record is relative to the edits before it rather than absolute.
    ///
    /// Returns how many edits remain.
    pub fn compact_before(&mut self, saved: usize) -> usize {
        let mut at = saved as isize;
        let mut drop_to = 0usize;
        for (i, e) in self.edits.iter().enumerate() {
            let end = e.at as isize + e.removed.len() as isize;
            if at < end {
                // **A later edit is measured against coordinates that include this one.** Stopping here is
                // what keeps the remaining edits' offsets meaningful; dropping one that a later edit was
                // measured against would silently shift every offset in the record.
                break;
            }
            if at < e.at as isize {
                break;
            }
            at += e.inserted.len() as isize - e.removed.len() as isize;
            drop_to = i + 1;
        }
        if drop_to > 0 {
            self.edits.drain(..drop_to);
        }
        self.edits.len()
    }

    /// The lowest current offset still in flux, or `None` if nothing has been edited.
    ///
    /// Everything below this has either never been edited or has been written back, so a fault there needs
    /// no translation at all — which is what makes reading an unedited prefix free.
    pub fn first_in_flight(&self) -> Option<usize> {
        let mut at = 0usize;
        for e in &self.edits {
            if e.at >= at {
                return Some(e.at);
            }
            at += e.inserted.len() - e.removed.len();
        }
        None
    }
}

/// One contiguous stretch of a **current** window whose bytes come from the **saved** document.
///
/// **A coordinate translation, not a copy** — it names a range twice, once where it goes and once where
/// it comes from, and carries no bytes. That is what lets a fault fetch straight into the destination and
/// leave the record holding nothing proportional to the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedRun {
    /// Where in the current window these bytes go.
    pub out_at: usize,
    /// Where in the saved document they come from.
    pub saved_at: usize,
    /// How many bytes.
    pub len: usize,
}

/// Why [`EditRecord::replay`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayError {
    /// `current_at + len` overflowed.
    WindowOutOfRange,
    /// The source returned fewer bytes than asked for.
    ///
    /// **Checked rather than padded**, for the same reason as `Rope::fault_leaf`: zeros would read as a run
    /// of NULs, which is indistinguishable from real text at this level.
    ShortFetch {
        /// How many bytes were asked for.
        want: usize,
        /// How many came back.
        got: usize,
    },
    /// A byte the record describes as typed, but which no edit's inserted run covers.
    ///
    /// **The record cannot describe this document**, which is a real defect rather than a caller mistake, and
    /// it is reachable: an edit whose `at` fell inside an earlier edit's removed run has no derivable
    /// position. Answering with a guess would put a plausible wrong byte in a document, so this refuses.
    Unresolvable {
        /// The offset that could not be resolved.
        at: usize,
    },
}
