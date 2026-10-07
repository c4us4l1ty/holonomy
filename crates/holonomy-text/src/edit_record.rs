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
//! # It translates; it does not yet replay content
//!
//! Turning the *bytes* a source holds into the bytes the rope wants also needs each edit's inserted text
//! spliced over the fetched range and its removed text skipped. **That is not implemented here** — see
//! `PROJECT.md` Phase 13 part 10. Splitting them matters: translation is easy to gate exhaustively against a
//! model, whereas content replay is easy to get subtly wrong and needs a model of its own. **Building and
//! gating the first alone is what makes the second checkable.**
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
    pub removed: Vec<u8>,
    /// The bytes this edit inserted.
    pub inserted: Vec<u8>,
}

impl Edit {
    /// An insert of `inserted` at `at`.
    pub fn insert(at: usize, inserted: impl Into<Vec<u8>>) -> Self {
        Self { at, removed: Vec::new(), inserted: inserted.into() }
    }

    /// A delete of `removed` at `at`.
    pub fn delete(at: usize, removed: impl Into<Vec<u8>>) -> Self {
        Self { at, removed: removed.into(), inserted: Vec::new() }
    }

    /// A replacement: `removed` out, `inserted` in, at the same offset.
    pub fn replace(at: usize, removed: impl Into<Vec<u8>>, inserted: impl Into<Vec<u8>>) -> Self {
        Self { at, removed: removed.into(), inserted: inserted.into() }
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
    pub fn new() -> Self {
        Self { edits: Vec::new() }
    }

    /// Record an edit.
    pub fn push(&mut self, edit: Edit) {
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

    /// The edits, in application order.
    pub fn edits(&self) -> &[Edit] {
        &self.edits
    }

    /// Net change to the document's length.
    pub fn net_delta(&self) -> isize {
        self.edits.iter().map(|e| e.inserted.len() as isize - e.removed.len() as isize).sum()
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