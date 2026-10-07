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

    /// Where the bytes this edit inserted ended up, in final-document coordinates.
    ///
    /// **Derived from [`to_current`](Self::to_current), which is exhaustively gated — and that is the whole
    /// point of this method.** Parts 9 and 11 each failed because they carried an offset accumulator across
    /// edits and applied it unconditionally; an edit at 50 does not shift a position at 10. Computing the
    /// position *per edit*, from a verified primitive, removes the accumulator entirely rather than fixing it.
    ///
    /// Inserting at `at` shifts everything from `at` onward up by `inserted.len()`, so the byte that was at
    /// `at` ends at `to_current(at)` and **the inserted bytes occupy the `inserted.len()` positions
    /// immediately before it**.
    ///
    /// `None` when `at` falls inside an earlier edit's removed run — the overlapping-delete case, where the
    /// byte this edit inserted at no longer exists to be measured against. **Not worked around**: the caller
    /// treats it as "this edit's position is not derivable" and says so rather than guessing.
    fn inserted_run(&self, e: &Edit) -> Option<(usize, usize)> {
        if e.inserted.is_empty() {
            return None;
        }
        let end = self.to_current(e.at)?;
        Some((end.checked_sub(e.inserted.len())?, e.inserted.len()))
    }

    /// The byte currently at `q` if it was **typed**, as `(which edit, index into that edit's inserted
    /// bytes)`. `None` if `q` is a saved byte.
    ///
    /// **Scans every edit's final run, so it is O(edits) — and deliberately so.** Any faster scheme needs to
    /// know *which* edits are near `q`, which is the same positional question this function exists to answer,
    /// and answering it with an approximation is how parts 9 and 11 went wrong. A compacted record is small,
    /// and this is called once per byte of a leaf-sized read, so the cost is bounded by the record rather than
    /// by the document.
    fn typed_byte(&self, q: usize) -> Option<(usize, usize)> {
        for (i, e) in self.edits.iter().enumerate() {
            if let Some((start, n)) = self.inserted_run(e) {
                if q >= start && q < start + n {
                    return Some((i, q - start));
                }
            }
        }
        None
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
        current_at.checked_add(len).ok_or(ReplayError::WindowOutOfRange)?;
        let mut out = vec![0u8; len];
        if len == 0 {
            return Ok(out);
        }

        // Gather the saved bytes the window needs, as a set of runs so `fetch` is called per run and not
        // per byte. A run is a maximal ascending stretch of saved offsets with no gap.
        let mut saved_at: Vec<Option<usize>> = Vec::with_capacity(len);
        let mut typed: Vec<Option<(usize, usize)>> = Vec::with_capacity(len);
        for i in 0..len {
            let q = current_at + i;
            match self.to_saved(q) {
                Some(s) => {
                    saved_at.push(Some(s));
                    typed.push(None);
                }
                None => {
                    let t = self
                        .typed_byte(q)
                        .ok_or(ReplayError::Unresolvable { at: q })?;
                    saved_at.push(None);
                    typed.push(Some(t));
                }
            }
        }

        let mut i = 0;
        while i < len {
            if let Some(s) = saved_at[i] {
                let start = i;
                let mut prev = s;
                while i < len && saved_at[i] == Some(prev) {
                    prev += 1;
                    i += 1;
                }
                let n = i - start;
                let bytes = fetch(s, n);
                if bytes.len() != n {
                    return Err(ReplayError::ShortFetch { want: n, got: bytes.len() });
                }
                out[start..i].copy_from_slice(&bytes);
            } else {
                // A typed byte: no source call at all.
                let (ei, idx) = typed[i].expect("a non-saved byte was located as typed");
                out[i] = self.edits[ei].inserted[idx];
                i += 1;
            }
        }
        Ok(out)
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
