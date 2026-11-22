//! The typing session: where open sources are born, filled and sealed.
//!
//! D-7's claim was that text being typed needs no special case, and this is the code that has to
//! earn it. Each keystroke goes into one resident buffer; the buffer's bytes belong to an **open
//! source** ([`OpenSources`]), and the open source is a **leaf** in the spine like any other. So
//! the document's tail is always a leaf, its bytes are always reachable by `slab_span`, and the
//! Eye can shape what the user is typing without knowing anything about buffers.
//!
//! # What a keystroke costs
//!
//! One `insert` into a [`SlabText`] with spare capacity — measured allocation-free by
//! `tests/keystroke_allocations.rs` — and one `Draft::record`, which updates an entry in place.
//! No root is copied, no segment is written, no allocation is made.
//!
//! # What a seal costs
//!
//! When the buffer is full the source is sealed: its bytes are appended once, the register renames
//! the provisional id, and the leaf's **length is corrected** in the current root. That correction
//! is why D-7's "sealing changes no root" was too strong, and the record has been amended: a root
//! *object* does change, because the tree has been carrying the leaf's registered length rather
//! than its live one. What does not change is the document's **history** — no undo entry is made,
//! so a user pressing undo steps over a seal the way they step over an idle moment. One root per
//! 64 KiB, not one per keystroke.
//!
//! # The caret is the caller's, the split is here
//!
//! [`Typing::insert_at`] types at a document offset, which is what a caret is. Placing text in the
//! middle of a leaf is pure geometry: [`Spine::insert_slab`] cuts the leaf in two — the halves name
//! adjacent ranges of the same sealed segment, so no byte moves and nothing is re-sealed — and the
//! new source sits between them.
//!
//! One rule makes that safe to reason about: **moving the caret seals what is open.** A session
//! holds at most one open source, so a jump to another position appends the pending bytes once and
//! leaves the document fully sealed. Without the rule there would be several open sources drafting
//! lengths at once and every offset query would walk them all; with it, every offset before the
//! open leaf is exact and the overlay is a single entry.
//!
//! # Deletion is a pointer operation
//!
//! [`Typing::delete`] takes a range out, and it is the cheapest structural change here. Inside
//! the open buffer it is one in-place [`SlabText::delete`]; across sealed text it seals what is
//! open and then removes the range from the spine — **one root copy, and no bytes written**,
//! because the document is geometry and the bytes were never its bytes. Cutting a megabyte out
//! costs what cutting ten out costs.
//!
//! **Deletion does not erase, and it does not persist either.** The world-line still holds the
//! ciphertext of what was removed, and a reader with the passphrase can still decrypt it. An
//! authenticated append-only log cannot do better without a compaction policy, and this session has
//! none; AUDIT's OQ-14 records that. The sharper half, recorded as OQ-30: a cut writes **nothing**,
//! and the sink this session holds can only append, so a deletion exists in the tree and nowhere
//! else. Nothing here can fix that — it needs a record kind the file does not have (OQ-29) —
//! but a caller must not read `delete` as an edit that will still be there tomorrow.
//!
//! # Undo is a length, not a copy
//!
//! [`Typing::mark`] records a state and [`Typing::undo`] returns to it, and the record is a
//! **length**: an undo changes how much of the buffer the document includes, not what is in it.
//! Undo and redo therefore copy no root, write no byte and allocate nothing, and redo works because
//! the bytes were never discarded — the tail simply stops being part of the document until
//! something edits it.
//!
//! A length cannot bring back bytes an edit destroyed, so a delete inside the buffer pops the marks
//! it invalidates: a mark promises that the bytes before its length are unchanged, and a cut breaks
//! that promise. Undoing *those* is the other half of the history, and it is a **root**: a deletion
//! that reaches sealed text records the tree from before the cut, and undo names that root again.
//! It costs no plaintext — the segments a cut removed were never written to or erased from — which
//! is OQ-14 seen from the other side, and it is why a replacement is one undoable action.
//!
//! Both halves are rings: a thousand lengths and sixty-four roots, so the history's memory is
//! bounded by the tree's depth and not by the number of edits. A seal ends the epoch, because a
//! root recorded before an append would orphan the segment it names (OQ-8).
//!
//! **A step records a state that can be reproduced, not the state as it stands.** That is the rule
//! `leave` exists for, and both halves pay it: a length alone does not say *where* the open leaf
//! was, so a mark carries the offset too, or redo re-appends the text at the caret; and a root
//! recorded over pending typing would name a leaf whose bytes undo discards, so a root step records
//! the **settled** tree — the one without the open leaf (D-17).

use crate::commit::{Appended, SegmentSink};
use crate::draft::Draft;
use crate::slab::{SegmentId, Slab, SlabSpan};
use crate::source::OpenSources;
use crate::spine::{Spine, SpineError};
use crate::text::{EditOutcome, SlabText, TextError};

/// Why a keystroke could not be taken.
#[derive(Debug)]
pub enum TypingError {
    /// The single insertion is larger than a whole buffer, so sealing cannot make room for it.
    TooLarge {
        /// The bytes offered.
        needed: usize,
        /// The buffer's capacity.
        capacity: usize,
    },
    /// The text is not valid UTF-8, or the offset arithmetic was refused.
    Text(TextError),
    /// The span arithmetic was refused — the leaf the session drafted is no longer where it was.
    Spine(SpineError),
    /// The world-line refused the append at a seal. Both the spine and the world-line are as they
    /// were, and the session still holds the text.
    Append {
        /// The sink's message.
        reason: String,
    },
}

impl std::fmt::Display for TypingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { needed, capacity } => write!(
                formatter,
                "{needed} bytes cannot be typed into a {capacity}-byte buffer"
            ),
            Self::Text(error) => write!(formatter, "{error}"),
            Self::Spine(error) => write!(formatter, "{error}"),
            Self::Append { reason } => {
                write!(formatter, "the world-line refused the seal: {reason}")
            }
        }
    }
}

impl std::error::Error for TypingError {}

impl From<TextError> for TypingError {
    fn from(error: TextError) -> Self {
        Self::Text(error)
    }
}

impl From<SpineError> for TypingError {
    fn from(error: SpineError) -> Self {
        Self::Spine(error)
    }
}

/// The leaf that is being typed into.
#[derive(Debug, Clone, Copy)]
struct Open {
    /// The provisional id its leaves name.
    source: SegmentId,
    /// Its document offset — the tail of the document while this slice is append-only.
    at: u64,
    /// The length the spine currently records for it. The live length is the buffer's.
    registered: u32,
}

/// How many undo states a session keeps.
///
/// A mark is a length, so the stack's memory is this many words, not a fraction of the document and
/// not a fraction of the history — which is OQ-8's answer for typing.
const UNDO_DEPTH: usize = 1024;

/// How many roots a session keeps, each O(log leaves) shared nodes.
///
/// Roots are the expensive half of the history and the only half that can restore destroyed bytes,
/// so they get their own, much smaller ring: the session's history memory is bounded by
/// `ROOT_DEPTH × O(log leaves)` plus `UNDO_DEPTH` words, and by nothing else (OQ-8).
const ROOT_DEPTH: usize = 64;

/// A state the session can return to.
///
/// A mark is a **length**, never a copy. The bytes it refers to are still in the buffer, because an
/// undo only changes how much of that buffer the document includes — the overlay is the authority
/// on the open leaf's length, so lowering it hides the tail without moving a byte. That is why undo
/// and redo of typing copy no root, write nothing and allocate nothing, and why redo can put the
/// text back: nothing was thrown away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// Nothing was open: the document was fully sealed and ended at this offset.
    Sealed {
        /// The document offset the next byte would have landed at.
        at: u64,
    },
    /// The source that is still open held this many bytes, at this document offset.
    ///
    /// The offset is part of the state, not a convenience: undo can take the leaf out of the tree
    /// and leave the caret somewhere else, so redo that put the text back at the **caret** would
    /// move it — the bytes would be the same and the document would read differently.
    Live {
        /// The document offset the open leaf sits at — where redo must put it back.
        at: u64,
        /// The open source's length at that moment.
        len: usize,
    },
}

/// One state the session can return to.
#[derive(Debug, Clone)]
enum Step {
    /// A length inside the open source — cheap, and it covers typing.
    Mark(Mark),
    /// A fully sealed document: the root to return to, and where the caret was.
    ///
    /// A root is a pointer (path-copied nodes shared with the current tree), so this is the only
    /// thing in the session that can restore **destroyed** bytes: the segments a cut removed from
    /// the spine were never written to or erased from, so naming them again brings the text back
    /// without decrypting anything (D-11's OQ-14, arriving from the other side).
    Root {
        /// The tree as it was before the cut.
        spine: Spine,
        /// The document offset the cut started at — where the caret goes back to.
        at: u64,
        /// How many bytes were open when this state was *left*, so the redo can re-attach them
        /// from the buffer. Zero for a root recorded before a cut: at a cut nothing is open, and an
        /// undo of it restores the root rather than leaving it.
        pending: usize,
        /// A copy of the open bytes, when this state is a **seal boundary** (D-43).
        ///
        /// A seal appends the open leaf and drops the buffer it lived in, so a copy is the only
        /// thing an undo can re-attach the text from. `None` for the two states where the buffer is
        /// still the live one — a root recorded before a cut, and a state recorded on the way out of
        /// an undo — where copying would be a copy of bytes already in hand.
        held: Option<Vec<u8>>,
    },
}

/// One retained state, as a debugger or a fuzz harness wants to see it.
///
/// This exists so a failure can say *which* state was wrong — a length, a root, and where — instead
/// of only that a document came back a byte short. It is not part of the editing API: nothing in
/// the session reads it, and `#[doc(hidden)]` keeps it out of the documentation.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepSummary {
    /// `sealed` for a length with no open source, `live` for one in a source, `root` for a tree.
    pub kind: &'static str,
    /// The document offset the state names: where the caret goes back to.
    pub at: u64,
    /// A live mark's length; a root's pending length; zero for a sealed mark.
    pub len: usize,
    /// A root's tree length; zero for a mark.
    pub tree: u64,
}

/// A document's tail while it is being typed into.
pub struct Typing<S> {
    spine: Spine,
    draft: Draft,
    sources: OpenSources,
    sink: S,
    buffer: SlabText,
    capacity: usize,
    open: Option<Open>,
    /// Where the next source is placed, when it is not the document's tail.
    next_at: Option<u64>,
    sealed_bytes: u64,
    /// States the session can return to, oldest first.
    steps: Vec<Step>,
    /// States an undo left behind, oldest first — the redo stack.
    undone: Vec<Step>,
    /// Whether the sealed spine differs from what has been committed to the sink (for example
    /// after a deletion across sealed text, or an undo/redo of a root step). Cleared only after
    /// the sink confirms the manifest or combined append commit (OQ-29, OQ-30).
    structure_dirty: bool,
}

impl<S: SegmentSink> Typing<S> {
    /// A session that types into a `capacity`-byte buffer and seals through `sink`.
    ///
    /// # Panics
    ///
    /// If `capacity` is zero: a session that can hold nothing can never accept a keystroke, and
    /// saying so at construction is better than refusing every `insert` forever.
    pub fn new(sink: S, capacity: usize) -> Self {
        assert!(
            capacity > 0,
            "a typing buffer needs room for at least one byte"
        );
        Self {
            spine: Spine::empty(),
            draft: Draft::new(),
            sources: OpenSources::new(),
            sink,
            buffer: SlabText::from_plaintext(Vec::new(), capacity)
                .expect("an empty slab is valid UTF-8"),
            capacity,
            open: None,
            next_at: None,
            sealed_bytes: 0,
            steps: Vec::new(),
            undone: Vec::new(),
            structure_dirty: false,
        }
    }

    /// Types `bytes` wherever the caret is — the document's end when nothing is open.
    ///
    /// # Errors
    ///
    /// A text, spine or sink error. A refused seal leaves the text in the buffer, so nothing typed
    /// is lost, and a burst larger than a whole buffer is split across seals rather than refused.
    pub fn insert(&mut self, bytes: &[u8]) -> Result<(), TypingError> {
        let at = self.caret();
        self.insert_at(at, bytes)
    }

    /// Types `bytes` at document offset `at`, moving the caret there first.
    ///
    /// Moving the caret **seals** whatever is open: one append, and the document is fully sealed
    /// while the new source is placed. That is what keeps every offset before the open leaf exact.
    ///
    /// `at` is a **character boundary of the document**. The session cannot check that on its own:
    /// it holds a [`SegmentSink`], which appends and never reads, so the bytes on the far side of a
    /// sealed leaf are not reachable from here. The caret that calls this is a cluster boundary out
    /// of `holonomy_eye`, which is what discharges the precondition (OQ-15).
    ///
    /// # Errors
    ///
    /// As [`Self::insert`].
    pub fn insert_at(&mut self, at: u64, bytes: &[u8]) -> Result<(), TypingError> {
        if at != self.caret() {
            self.flush()?;
            self.next_at = Some(at.min(self.spine.len()));
        }
        self.type_bytes(bytes)
    }

    /// Deletes the range `start..end`, leaving the caret at `start`.
    ///
    /// A range inside the open buffer is one in-place edit, and the keystroke path stays the
    /// keystroke path. A range that reaches sealed text seals what is open first — so every offset
    /// is the tree's own and no correction is needed — and then takes the range out of the spine
    /// and marks the spine geometry dirty for the next [`Self::flush`]. Either way **no segment
    /// plaintext is read or rewritten**: the world-line keeps the historical ciphertext until
    /// explicit compaction (OQ-14), while the manifest committed on save records the exact
    /// surviving slab spans so deleted text does not return on reopen (OQ-29, OQ-30).
    ///
    /// Deleting the whole open buffer takes the leaf with it and abandons the source: the tree may
    /// not carry a leaf that names nothing.
    ///
    /// `start` and `end` are **character boundaries of the document**, for the same reason as
    /// [`Self::insert_at`] — and the asymmetry is real: offsets that land inside the open buffer
    /// are checked by [`SlabText`], and offsets that land in sealed text are not, because this
    /// session holds no reader (OQ-15). A cut that splits a character leaves a leaf whose bytes are
    /// not valid UTF-8, which the resident set will refuse when it faults that leaf in.
    ///
    /// # Errors
    ///
    /// A range whose ends are not character boundaries **of the bytes in the buffer**, or a spine
    /// range the geometry refuses. A refused cut changes nothing.
    pub fn delete(&mut self, start: u64, end: u64) -> Result<(), TypingError> {
        let document = self.document_len();
        let (start, end) = (start.min(document), end.min(document));
        if start >= end {
            return Ok(());
        }
        if let Some(open) = self.open {
            let live = u64::try_from(self.live_len()).expect("a buffer fits in u64");
            let live_end = open.at + live;
            if start >= open.at && end <= live_end {
                let from = usize::try_from(start - open.at).expect("a buffer fits in usize");
                let to = usize::try_from(end - open.at).expect("a buffer fits in usize");
                // A cut destroys bytes, so it ends redo and every mark that promised them.
                self.drop_undone_tail()?;
                self.pop_marks_above(from);
                self.buffer.delete(from, to)?;
                if self.buffer.is_empty() {
                    // The leaf carried the only bytes this source had. It leaves the spine, and the
                    // provisional id becomes unknown rather than pointing at nothing.
                    let leaf_end = open.at + u64::from(open.registered);
                    self.spine = self.spine.remove_range(open.at, leaf_end)?;
                    self.sources.close(open.source);
                    self.draft.forget(open.at);
                    self.open = None;
                    self.next_at = Some(open.at);
                } else {
                    self.settle()?;
                }
                return Ok(());
            }
        }
        // Something sealed is involved: seal what is open so every offset is exact, then cut. The
        // root is recorded first, so the cut can be taken back as a pointer swap — no bytes need to
        // be remembered, because the bytes a cut removes are still in the world-line.
        self.flush()?;
        self.undone.clear();
        self.push_root(start);
        self.spine = self.spine.remove_range(start, end)?;
        self.next_at = Some(start);
        self.structure_dirty = true;
        Ok(())
    }

    /// Records a state the session can undo back to, and drops any redo stack.
    ///
    /// Call this before a **user action**, not before every keystroke: what counts as one action (a
    /// pause, a word, a paste) is knowledge the shell has and this layer does not.
    pub fn mark(&mut self) {
        self.undone.clear();
        let mark = self.current_mark();
        self.cap_marks();
        self.steps.push(Step::Mark(mark));
    }

    /// Returns to the last mark, reporting whether there was one.
    ///
    /// Only the document's length changes: the bytes stay in the buffer, so this copies no root,
    /// writes no byte and allocates nothing, and [`Self::redo`] can put the text back.
    ///
    /// # Errors
    ///
    /// A spine range the geometry refuses, when the state being restored had no open source and the
    /// leaf the action registered has to leave the tree.
    pub fn undo(&mut self) -> Result<bool, TypingError> {
        let Some(step) = self.steps.pop() else {
            return Ok(false);
        };
        let current = self.leave(step)?;
        self.undone.push(current);
        Ok(true)
    }

    /// Re-applies what [`Self::undo`] last took back, reporting whether there was anything.
    ///
    /// # Errors
    ///
    /// As [`Self::undo`]: restoring a state with an open source may have to register a leaf.
    pub fn redo(&mut self) -> Result<bool, TypingError> {
        let Some(step) = self.undone.pop() else {
            return Ok(false);
        };
        let current = self.leave(step)?;
        self.steps.push(current);
        Ok(true)
    }

    /// The state an [`Self::undo`] would return to, described for a debugger.
    #[doc(hidden)]
    #[must_use]
    pub fn top_undo(&self) -> Option<StepSummary> {
        self.steps.last().map(summary)
    }

    /// The state a [`Self::redo`] would re-apply, described for a debugger.
    #[doc(hidden)]
    #[must_use]
    pub fn top_redo(&self) -> Option<StepSummary> {
        self.undone.last().map(summary)
    }

    /// How many undo states the session holds.
    #[must_use]
    pub fn undo_depth(&self) -> usize {
        self.steps.len()
    }

    /// How many states [`Self::redo`] can re-apply.
    #[must_use]
    pub fn redo_depth(&self) -> usize {
        self.undone.len()
    }

    /// Replaces `start..end` with `bytes` — typing over a selection.
    ///
    /// It is [`Self::delete`] then [`Self::insert_at`], which is the whole implementation as well
    /// as the specification: the caret ends after the inserted text, and the pair is **one**
    /// undoable action, because the deletion records the root from before the cut and the text the
    /// insert left pending is dropped when that root is restored.
    ///
    /// # Errors
    ///
    /// As [`Self::delete`] and [`Self::insert`].
    pub fn replace(&mut self, start: u64, end: u64, bytes: &[u8]) -> Result<(), TypingError> {
        self.delete(start, end)?;
        self.insert_at(start, bytes)
    }

    /// Leaves `step`'s state, returning the state it left *from*.
    ///
    /// Undo and redo are the same move in opposite directions, and both have to answer the same
    /// question: what state am I about to be able to come back to? That answer is not always the
    /// session as it stands, and saying so in one place is what keeps the two directions from
    /// disagreeing.
    ///
    /// For a root the answer is the tree **without** whatever is open. Undo of a root drops the
    /// pending bytes — a root recorded before an append would orphan the segment that append wrote
    /// (OQ-8), so a seal ends the epoch and anything still open was never written — and a step
    /// recorded from the live tree would name a leaf over bytes that undo threw away. Redo would
    /// then restore a document claiming text nobody can produce. `settled_spine` is the state that
    /// can actually be come back to.
    fn leave(&mut self, step: Step) -> Result<Step, TypingError> {
        let current = match &step {
            Step::Mark(_) => Step::Mark(self.current_mark()),
            Step::Root { .. } => {
                let spine = self.settled_spine()?;
                let pending = self.pending_len();
                // The arithmetic the walk rests on, checked where it is used: the tree without the
                // open leaf, plus the bytes the leaf holds, is the document. A state that does not
                // add up is a state whose redo cannot be exact, and it is cheaper to say so here
                // than to infer it from a document twenty actions later.
                debug_assert_eq!(
                    spine.len() + u64::try_from(pending).expect("a buffer fits in u64"),
                    self.document_len(),
                    "the settled tree and the open bytes do not add up to the document"
                );
                Step::Root {
                    spine,
                    at: self.open.map_or_else(|| self.caret(), |open| open.at),
                    pending,
                    // The bytes travel with the state. A redo can land after a seal has replaced
                    // the buffer with another leaf's, so a length alone would point into the wrong
                    // text (found by the million-keystroke walk when a seal stopped being a floor).
                    held: (pending > 0).then(|| self.buffer.as_bytes().to_vec()),
                }
            }
        };
        match step {
            Step::Mark(mark) => self.restore_mark(mark)?,
            Step::Root {
                spine,
                at,
                pending,
                held,
            } => self.restore_root(spine, at, pending, held)?,
        }
        Ok(current)
    }

    /// The tree as it would be if the pending bytes were dropped: the open leaf's registered bytes
    /// taken out.
    ///
    /// This is the sealed shape of the session, and it is what a root step records so that redo
    /// cannot resurrect a leaf whose source undo abandoned.
    fn settled_spine(&self) -> Result<Spine, TypingError> {
        let Some(open) = self.open else {
            return Ok(self.spine.clone());
        };
        let end = open.at + u64::from(open.registered);
        let settled = self.spine.remove_range(open.at, end)?;
        debug_assert_eq!(
            self.spine.len() - settled.len(),
            u64::from(open.registered),
            "the open leaf is not in the tree at its stated offset"
        );
        Ok(settled)
    }

    /// The state the session is in now, as a mark would record it.
    fn current_mark(&self) -> Mark {
        match self.open {
            Some(open) => Mark::Live {
                at: open.at,
                len: self.live_len(),
            },
            None => Mark::Sealed { at: self.caret() },
        }
    }

    /// Moves the session to a length, touching at most one leaf — never bytes.
    fn restore_mark(&mut self, mark: Mark) -> Result<(), TypingError> {
        match (mark, self.open) {
            (Mark::Live { at, len }, Some(open)) if open.at != at => {
                // The open leaf is not the one this state was taken over — undo and redo can move a
                // leaf, and a mark remembers where it was. The bytes are still in the buffer, so
                // the leaf comes out of the tree and goes back where it belongs; a length-only
                // restore would put them in the wrong place, which is OQ-22.
                let leaf_end = open.at + u64::from(open.registered);
                self.spine = self.spine.remove_range(open.at, leaf_end)?;
                self.sources.close(open.source);
                self.open = None;
                self.next_at = Some(at.min(self.spine.len()));
                self.attach(len)
            }
            (Mark::Live { len, .. }, Some(_)) => self.set_live(len),
            (Mark::Live { at, len }, None) => {
                // Redo of "there was no source": the leaf comes back where it was, which is the
                // mark's offset and not `next_at`, because undo left the caret at the mark's.
                self.next_at = Some(at.min(self.spine.len()));
                self.attach(len)
            }
            (Mark::Sealed { at }, Some(open)) => {
                // The source was opened by the action being undone: its leaf leaves the spine and
                // the register abandons it. The bytes stay in the buffer, so redo can restore them.
                let leaf_end = open.at + u64::from(open.registered);
                self.spine = self.spine.remove_range(open.at, leaf_end)?;
                self.sources.close(open.source);
                self.draft.forget(open.at);
                self.open = None;
                self.next_at = Some(at.min(self.spine.len()));
                Ok(())
            }
            (Mark::Sealed { at }, None) => {
                self.next_at = Some(at.min(self.spine.len()));
                Ok(())
            }
        }
    }

    /// Moves the session to a sealed state: this root, this caret, and — when a redo is coming
    /// back to it — the bytes that were open, re-attached out of the buffer.
    ///
    /// Anything open here is *pending*: a seal would have dropped this step, so the bytes were
    /// never written and the world-line does not have them. The buffer does, and that is the whole
    /// reason a root restore **keeps** the buffer instead of clearing it: a mark is a length into
    /// that buffer (no copy, no allocation), so every state that measured those bytes stays
    /// restorable, and a redo can hand them back exactly as they were. `pending` says how much of
    /// the buffer the state being left had open; the text a cut removed is a different case — it is
    /// in the world-line, and the root names it again for free.
    fn restore_root(
        &mut self,
        spine: Spine,
        at: u64,
        pending: usize,
        held: Option<Vec<u8>>,
    ) -> Result<(), TypingError> {
        self.structure_dirty = true;
        if let Some(open) = self.open {
            self.sources.close(open.source);
            self.open = None;
        }
        self.draft.clear();
        self.spine = spine;
        self.next_at = Some(at.min(self.spine.len()));
        debug_assert!(
            pending == 0 || held.is_some(),
            "a state with open bytes carries them ({pending} open, {held:?})",
            held = held.as_ref().map(Vec::len)
        );
        if let Some(held) = held {
            // The copy is what the leaf comes back from: a seal has replaced the buffer with
            // another leaf's, and even without a seal the state a redo lands in need not be the one
            // the bytes were typed into. They came from a buffer of this capacity, so they fit.
            self.buffer = SlabText::from_plaintext(held, self.capacity)
                .expect("the bytes came from a buffer of this capacity");
        }
        if pending > 0 {
            self.attach(pending)?;
        }
        Ok(())
    }

    /// How many bytes the open source holds right now: what a redo has to re-attach.
    ///
    /// Zero when nothing is open — the settled tree already says everything the document says.
    fn pending_len(&self) -> usize {
        if self.open.is_none() {
            return 0;
        }
        self.live_len()
    }

    /// Records the tree as it is now, so a cut about to happen can be taken back.
    fn push_root(&mut self, at: u64) {
        self.cap_roots();
        self.steps.push(Step::Root {
            spine: self.spine.clone(),
            at,
            // Nothing is open at a cut — `delete` seals first, so every offset is the tree's own.
            pending: 0,
            held: None,
        });
    }

    /// Drops the oldest length once the length ring is full.
    fn cap_marks(&mut self) {
        let marks = self
            .steps
            .iter()
            .filter(|step| matches!(step, Step::Mark(_)))
            .count();
        if marks < UNDO_DEPTH {
            return;
        }
        if let Some(index) = self
            .steps
            .iter()
            .position(|step| matches!(step, Step::Mark(_)))
        {
            self.steps.remove(index);
        }
    }

    /// Drops the oldest root once the root ring is full. The ring is small because a root is the
    /// expensive half of the history, and the world-line is the deeper store (OQ-8).
    fn cap_roots(&mut self) {
        let roots = self
            .steps
            .iter()
            .filter(|step| matches!(step, Step::Root { .. }))
            .count();
        if roots < ROOT_DEPTH {
            return;
        }
        if let Some(index) = self
            .steps
            .iter()
            .position(|step| matches!(step, Step::Root { .. }))
        {
            // Dropping a root makes everything *behind* it unreachable — a mark behind a root can
            // only be honoured after that root has been restored — so the prefix goes with it.
            self.steps.drain(..=index);
        }
    }

    /// Sets the open leaf's live length, without touching the buffer.
    fn set_live(&mut self, len: usize) -> Result<(), TypingError> {
        let Some(open) = self.open else {
            return Ok(());
        };
        debug_assert!(
            len <= self.buffer.len(),
            "a length cannot be longer than the buffer it measures ({len} > {})",
            self.buffer.len()
        );
        if len == usize::try_from(open.registered).expect("capacity fits in usize") {
            // Back to the sealed length: the overlay has nothing left to say (D-2).
            self.draft.forget(open.at);
        } else {
            let span = SlabSpan::new(open.at, Slab::new(open.source, 0, open.registered));
            self.draft
                .record(span, u64::try_from(len).expect("a buffer fits in u64"))
                .expect("the only drafted leaf cannot overlap another");
        }
        Ok(())
    }

    /// Opens a source over the first `len` bytes of the buffer, at the caret.
    ///
    /// This is redo of "there was no source": the text comes back under a *new* provisional id,
    /// because the abandoned one is never reused.
    fn attach(&mut self, len: usize) -> Result<(), TypingError> {
        debug_assert!(
            len <= self.buffer.len(),
            "a leaf cannot claim bytes the buffer does not hold ({len} > {})",
            self.buffer.len()
        );
        let at = self.next_at.take().unwrap_or_else(|| self.spine.len());
        if len == 0 {
            // D-52: a source opened over no bytes registers a leaf `insert_slab` drops, so the
            // register holds a buffer that names nothing and `open_count` climbs past the one
            // buffer a caller can hand a reader. `restore_mark` is the only caller that can hand
            // this function a zero — a live mark whose length an undo walked back to nothing — and
            // there is nothing to attach: only the caret moves.
            self.next_at = Some(at.min(self.spine.len()));
            return Ok(());
        }
        let registered = u32::try_from(len).expect("capacity fits in u32");
        let source = self.sources.open();
        self.spine = self
            .spine
            .insert_slab(at, Slab::new(source, 0, registered))?;
        self.open = Some(Open {
            source,
            at,
            registered,
        });
        Ok(())
    }

    /// The open leaf's live length: the overlay's answer, or the registered length when the overlay
    /// does not disagree with it. Zero when nothing is open — an undone tail is not the document.
    fn live_len(&self) -> usize {
        let Some(open) = self.open else {
            return 0;
        };
        let live = self
            .draft
            .live_len(open.at)
            .unwrap_or(u64::from(open.registered));
        usize::try_from(live).expect("a buffer fits in usize")
    }

    /// Drops the bytes an undo left in the buffer's tail, so a new edit starts from the live text.
    fn drop_undone_tail(&mut self) -> Result<(), TypingError> {
        let live = self.live_len();
        if self.buffer.len() > live {
            self.buffer.delete(live, self.buffer.len())?;
        }
        self.undone.clear();
        Ok(())
    }

    /// Pops every mark whose promise this edit breaks: a mark says the bytes before its length are
    /// unchanged, and an edit below that length changes them.
    fn pop_marks_above(&mut self, len: usize) {
        while let Some(Step::Mark(Mark::Live { len: mark, .. })) = self.steps.last() {
            if *mark <= len {
                break;
            }
            self.steps.pop();
        }
    }

    /// The document offset the next typed byte lands at.
    #[must_use]
    pub fn caret(&self) -> u64 {
        match self.open {
            Some(open) => open.at + u64::try_from(self.live_len()).expect("a buffer fits in u64"),
            None => self.next_at.unwrap_or_else(|| self.spine.len()),
        }
    }

    /// The keystroke loop: fill the buffer, seal when it is full, place the source when the first
    /// byte arrives.
    fn type_bytes(&mut self, bytes: &[u8]) -> Result<(), TypingError> {
        let mut rest = bytes;
        if rest.is_empty() {
            return Ok(());
        }
        // A new edit from a state an undo left behind: the undone tail is dropped for good, and the
        // marks it promised are gone with it.
        self.drop_undone_tail()?;
        self.pop_marks_above(self.live_len());
        while !rest.is_empty() {
            let live = self.live_len();
            let room = self.capacity - live;
            if room == 0 {
                self.seal()?;
                continue;
            }
            // Take only what fits, so one burst can be many keystrokes and still never hides an
            // allocation inside an edit: the split happens here, at a seal, where it is counted.
            // The split lands *between* characters: a fragment cut inside one is not text, and
            // `SlabText` would refuse it — so when the next character does not fit whole, the seal
            // goes first and the character starts the next source.
            let mut take = room.min(rest.len());
            while take > 0 && !starts_a_character(rest, take) {
                take -= 1;
            }
            if take == 0 {
                // D-54: a character wider than the whole buffer could be neither typed nor
                // refused. A seal is the loop's other move, and with nothing open a seal does
                // nothing — so `insert` spun forever on a one-byte session and a three-byte
                // character: a hang in a public API where an error was owed. Nothing open means
                // the character being placed is the first of `rest`, and it does not fit the
                // buffer at all.
                if self.open.is_none() {
                    // The character's own width, not the burst's: `rest` can hold a whole paste,
                    // and reporting its length would name a number the buffer never had to hold.
                    return Err(TypingError::TooLarge {
                        needed: lead_byte_len(rest),
                        capacity: self.capacity,
                    });
                }
                self.seal()?;
                continue;
            }
            let at = live;
            match self.buffer.insert(at, &rest[..take])? {
                EditOutcome::InPlace => {}
                EditOutcome::WouldSplit { needed, capacity } => {
                    return Err(TypingError::TooLarge { needed, capacity });
                }
            }
            self.settle()?;
            rest = &rest[take..];
        }
        Ok(())
    }

    /// Seals whatever is open, so the document's geometry is final.
    ///
    /// # Errors
    ///
    /// As [`Self::insert`]: a refused append leaves the text in the buffer.
    pub fn flush(&mut self) -> Result<(), TypingError> {
        self.seal()
    }

    /// A session resumed over a document that is already in the world-line.
    ///
    /// `spine` is the geometry the file's segment lengths give; the caret starts at its end, and
    /// nothing is read until a frame or a residency fault asks for a leaf — so opening a long
    /// document costs what opening a short one costs. There is no open leaf and no history: the
    /// file is the floor, exactly as a seal leaves it.
    ///
    /// # Panics
    ///
    /// If `capacity` is zero, for the reason [`Self::new`] gives.
    pub fn resume(sink: S, spine: Spine, capacity: usize) -> Self {
        let mut typing = Self::new(sink, capacity);
        typing.next_at = Some(spine.len());
        typing.spine = spine;
        typing
    }

    /// The document's geometry, with the open source as its last leaf.
    #[must_use]
    pub fn spine(&self) -> &Spine {
        &self.spine
    }

    /// The live-length overlay: one entry, for the leaf being typed into.
    #[must_use]
    pub fn draft(&self) -> &Draft {
        &self.draft
    }

    /// The register of open and sealed sources.
    #[must_use]
    pub fn sources(&self) -> &OpenSources {
        &self.sources
    }

    /// The bytes that are in the buffer and in no segment yet.
    #[must_use]
    pub fn pending(&self) -> &[u8] {
        // An undone tail is in the buffer and not in the document, so it is not pending either.
        let live = self.live_len();
        debug_assert!(
            live <= self.buffer.len(),
            "the document cannot be pending over bytes the buffer does not hold ({live} > {})",
            self.buffer.len()
        );
        &self.buffer.as_bytes()[..live]
    }

    /// Whether the sealed spine carries a cut, deletion, or root-level undo/redo that has not yet
    /// been committed to the sink (OQ-29, OQ-30).
    #[must_use]
    pub fn has_uncommitted_structure(&self) -> bool {
        self.structure_dirty
    }

    /// The document's length, drafts applied — what the user would call "how long is this".
    #[must_use]
    pub fn document_len(&self) -> u64 {
        self.draft.document_len(&self.spine)
    }

    /// Bytes handed to the world-line so far. Equal to the bytes typed minus what is still open,
    /// which is the whole point: nothing is written twice.
    #[must_use]
    pub fn sealed_bytes(&self) -> u64 {
        self.sealed_bytes
    }

    /// Registers the open leaf, or updates its live length.
    ///
    /// The first byte of a source registers the leaf with its current length, so the tree is
    /// correct at that instant and needs no draft; later keystrokes are drafts, and the draft is
    /// where the per-keystroke work stays allocation-free.
    fn settle(&mut self) -> Result<(), TypingError> {
        let len = u64::try_from(self.buffer.len()).expect("a buffer fits in u64");
        match self.open {
            Some(open) => {
                let span = SlabSpan::new(open.at, Slab::new(open.source, 0, open.registered));
                self.draft
                    .record(span, len)
                    .expect("the only drafted leaf cannot overlap another");
            }
            None => {
                if len == 0 {
                    return Ok(());
                }
                let source = self.sources.open();
                let registered = u32::try_from(self.buffer.len()).expect("capacity fits in u32");
                let at = self.next_at.take().unwrap_or_else(|| self.spine.len());
                // `insert_slab` cuts the leaf at `at` if the offset lands inside one. The halves
                // name adjacent ranges of the same sealed segment, so the text is not touched.
                self.spine = self
                    .spine
                    .insert_slab(at, Slab::new(source, 0, registered))?;
                self.open = Some(Open {
                    source,
                    at,
                    registered,
                });
            }
        }
        Ok(())
    }

    /// Resolves a sealed slab's provisional or direct source address into its world-line segment.
    fn resolve_slab(&self, slab: Slab) -> Result<Slab, TypingError> {
        let segment = self
            .sources
            .locate(slab.segment())
            .segment()
            .ok_or_else(|| TypingError::Append {
                reason: format!("sealed leaf {} has no segment id", slab.segment()),
            })?;
        Ok(Slab::new(segment, slab.start(), slab.len()))
    }

    /// Commits the current fully-sealed spine geometry to the sink without reading or appending
    /// any segment bytes (OQ-29, OQ-30).
    fn commit_current_spine(&mut self) -> Result<(), TypingError> {
        let mut slabs = Vec::new();
        for span in self.spine.spans_in(0, self.spine.len()) {
            slabs.push(self.resolve_slab(span.slab())?);
        }
        self.sink
            .commit_spine(&slabs)
            .map_err(|reason| TypingError::Append { reason })?;
        self.structure_dirty = false;
        Ok(())
    }

    /// Seals the open source: append once, rename, correct the leaf's length, drop the draft.
    fn seal(&mut self) -> Result<(), TypingError> {
        let Some(open) = self.open else {
            return if self.structure_dirty {
                self.commit_current_spine()
            } else {
                Ok(())
            };
        };
        // The bytes that are in the document, not the buffer's capacity for them: an undo leaves
        // the bytes it took back *behind* the live length so redo can hand them over, and a seal
        // ends redo (`self.undone.clear()` below), so writing the tail would append text nobody
        // can name — the document would grow by exactly what undo removed.
        let len = self.live_len();
        if len == 0 {
            // D-52: an open source over no bytes. `delete` closes a leaf it empties, and `attach`
            // refuses to open one over nothing, so the public API cannot deliver this state today
            // — the guard is defence in depth, and it is also what `set_live(0)` needs: an undo
            // that takes a source back to zero bytes must not leave the leaf in the tree claiming
            // bytes no reader can produce. A seal over nothing therefore has one job: take the
            // leaf out and abandon the source, rather than answering `Ok` while `open_count`
            // stays one and `pending()` stays empty forever.
            let end = open.at + u64::from(open.registered);
            self.spine = self.spine.remove_range(open.at, end)?;
            self.sources.close(open.source);
            self.draft.forget(open.at);
            self.open = None;
            self.next_at = Some(open.at);
            self.undone.clear();
            if self.structure_dirty {
                self.commit_current_spine()?;
            }
            return Ok(());
        }
        let end = open.at + u64::from(open.registered);
        let mut before = Vec::new();
        for span in self.spine.spans_in(0, open.at) {
            before.push(self.resolve_slab(span.slab())?);
        }
        let mut after = Vec::new();
        for span in self.spine.spans_in(end, self.spine.len()) {
            after.push(self.resolve_slab(span.slab())?);
        }
        let appended: Appended = self
            .sink
            .append_segment_with_spine(&self.buffer.as_bytes()[..len], &before, &after)
            .map_err(|reason| TypingError::Append { reason })?;
        self.sources
            .seal(open.source, appended.segment)
            .expect("the register handed this id out");
        let corrected = Slab::new(
            open.source,
            0,
            u32::try_from(len).expect("capacity fits in u32"),
        );
        // The state being sealed, kept so an undo can come back to it (D-43). It has to be taken
        // now: the replace below is what makes the leaf final, and the buffer is dropped after it.
        let settled = self.settled_spine()?;
        let held = self.buffer.as_bytes().to_vec();
        self.spine = self.spine.replace_range(open.at, end, corrected)?;
        self.draft.forget(open.at);
        self.sealed_bytes += u64::try_from(len).expect("a buffer fits in u64");
        self.next_at = Some(open.at + u64::try_from(len).expect("a buffer fits in u64"));
        self.open = None;
        self.structure_dirty = false;
        // A seal used to be a floor that cleared the history — and that was D-43: the buffer is
        // dropped here, so no state behind the seal could produce the bytes it promised, and every
        // caret move seals (an open leaf's caret is its end), so one arrow key made the typing
        // before it permanent.
        //
        // The floor keeps **one** state instead, and it is the one a user would ask for: the last
        // mark's. A mark is where the burst being typed began, so the seal turns "everything up to
        // here is settled, and the last burst is still undoable" into a state that carries the
        // bytes (`held`) the buffer is about to lose. Only one state is kept on purpose: the marks
        // of older leaves cannot be honoured against a buffer that a later leaf has since become
        // (they are lengths, and two leaves can start at the same offset) — the million-keystroke
        // walk found exactly that as a document cut inside a character.
        //
        // With no mark above the seal there is nothing older to keep, so the state is the leaf
        // itself: one undo takes back what was just written. Roots behind the seal go with it, for
        // the reason the floor existed — a root recorded before this append names a tree whose open
        // leaf the append has moved, so restoring it would orphan the segment written here. Redo
        // goes too: it would name a state the append has already moved past.
        let pending = match self.steps.last() {
            Some(Step::Mark(Mark::Live { at, len: mark })) if *at == open.at => *mark,
            Some(Step::Mark(_)) => 0,
            _ => len,
        };
        self.steps.clear();
        self.steps.push(Step::Root {
            spine: settled,
            at: open.at,
            pending,
            held: Some(held),
        });
        self.undone.clear();
        self.buffer = SlabText::from_plaintext(Vec::new(), self.capacity)
            .expect("an empty slab is valid UTF-8");
        Ok(())
    }
}

/// Describes one retained state.
fn summary(step: &Step) -> StepSummary {
    match step {
        Step::Mark(Mark::Sealed { at }) => StepSummary {
            kind: "sealed",
            at: *at,
            len: 0,
            tree: 0,
        },
        Step::Mark(Mark::Live { at, len }) => StepSummary {
            kind: "live",
            at: *at,
            len: *len,
            tree: 0,
        },
        Step::Root {
            spine, at, pending, ..
        } => StepSummary {
            kind: "root",
            at: *at,
            len: *pending,
            tree: spine.len(),
        },
    }
}

/// The length of the UTF-8 character whose first byte is `bytes[0]`, from the first byte alone.
///
/// The keystroke loop needs this when a character does not fit: it has already split `rest` at a
/// character boundary, so `rest[0]` is a lead byte and the table in RFC 3629 says how many bytes
/// follow. A continuation byte cannot appear at `0` for that reason, but it is reported as one byte
/// rather than panicking — the caller's error message is about capacity, not encoding.
fn lead_byte_len(bytes: &[u8]) -> usize {
    match bytes.first() {
        Some(0xC2..=0xDF) => 2,
        Some(0xE0..=0xEF) => 3,
        Some(0xF0..=0xF4) => 4,
        _ => 1,
    }
}

/// Whether `at` begins a character in `bytes` — the same test [`SlabText`] applies to an edit.
fn starts_a_character(bytes: &[u8], at: usize) -> bool {
    at == bytes.len() || bytes.get(at).is_some_and(|byte| byte & 0xC0 != 0x80)
}

/// A sink that keeps what it is given, so a test can check what the world-line would have written.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct Recorder {
    /// One entry per sealed segment, in append order.
    pub(crate) segments: Vec<Vec<u8>>,
    /// The next segment id to hand out.
    next: u64,
}

#[cfg(test)]
impl Recorder {
    pub(crate) fn written(&self) -> Vec<u8> {
        self.segments.concat()
    }
}

#[cfg(test)]
impl SegmentSink for Recorder {
    fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
        if plaintext.is_empty() {
            return Err("an empty segment carries nothing".to_string());
        }
        let segment = SegmentId::new(self.next);
        self.next += 1;
        self.segments.push(plaintext.to_vec());
        Ok(Appended {
            segment,
            witness_sequence: self.next,
            segment_root: [0u8; 32],
            bytes_written: u64::try_from(plaintext.len()).expect("fits"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Location;

    fn session(capacity: usize) -> Typing<Recorder> {
        Typing::new(Recorder::default(), capacity)
    }

    #[test]
    fn the_first_byte_registers_a_leaf_with_no_draft() {
        let mut typing = session(64);
        typing.insert(b"hello").expect("typed");
        assert_eq!(typing.spine().slab_count(), 1);
        assert_eq!(typing.spine().len(), 5);
        assert_eq!(typing.document_len(), 5);
        // The tree already says five, so nothing needs drafting until the next keystroke.
        assert!(typing.draft().is_empty());
        assert_eq!(typing.pending(), b"hello");
        assert_eq!(typing.sealed_bytes(), 0);
    }

    #[test]
    fn later_keystrokes_are_drafts_and_the_tree_stays_stale() {
        let mut typing = session(64);
        typing.insert(b"hello").expect("typed");
        typing.insert(b" world").expect("typed");
        assert_eq!(typing.spine().len(), 5, "the tree still says five");
        assert_eq!(typing.document_len(), 11, "the overlay says eleven");
        assert_eq!(typing.draft().len(), 1);
        // The leaf the overlay drafts is the open source's leaf, and it is the document's tail.
        let span = typing.spine().slab_span(0).expect("a leaf");
        assert!(typing.sources().locate(span.slab().segment()).is_open());
    }

    #[test]
    fn a_caret_move_does_not_throw_the_undo_history_away() {
        // D-43, through the layer that had the bug: moving the caret seals the open leaf, and a
        // seal used to be an undo floor — so an arrow key, Home, End or a click silently ended
        // undo, and the text typed before it could never be taken back.
        let mut typing = session(64);
        typing.mark();
        typing.insert(b"hello").expect("typed");
        typing.insert_at(1, b"").expect("moved");
        assert_eq!(
            typing.sealed_bytes(),
            5,
            "the move should seal the open leaf"
        );
        assert_eq!(typing.document_len(), 5);

        // One undo takes the typing back, and a redo puts it back: the seal kept the state the
        // mark named instead of dropping it.
        assert!(typing.undo().expect("the typing can be taken back"));
        assert_eq!(typing.document_len(), 0);
        assert!(typing.redo().expect("redo"));
        assert_eq!(typing.document_len(), 5);
        assert_eq!(typing.spine().len(), 5, "the bytes are the world-line's");
    }

    #[test]
    fn a_seal_keeps_the_last_burst_undoable() {
        // The other half of D-43: a mark laid *after* the typing (which is what the window does —
        // one mark per burst) is the state the seal keeps, so the burst the user just typed can be
        // taken back after a caret move, and the state carries the bytes to do it.
        let mut typing = session(64);
        typing.mark();
        typing.insert(b"hel").expect("typed");
        typing.mark();
        typing.insert(b"lo").expect("typed");
        typing.insert_at(1, b"").expect("moved");
        assert_eq!(typing.sealed_bytes(), 5);

        let top = typing.top_undo().expect("the seal's state");
        assert_eq!(top.kind, "root");
        assert_eq!(top.len, 3, "the start of the burst, not the whole leaf");
        assert!(typing.undo().expect("undo"));
        assert_eq!(
            typing.pending(),
            b"hel",
            "the burst came back as open bytes"
        );
        assert_eq!(typing.document_len(), 3);
    }

    #[test]
    fn a_seal_over_no_bytes_abandons_the_source_instead_of_pretending() {
        // D-52, tested from inside because the public API cannot reach the state: `delete` closes
        // a leaf it empties and `attach` refuses to open one over nothing. The guard still
        // matters, because the alternative is a source over no bytes that `flush` answers `Ok`
        // for while `open_count` stays one — the window asks `pending()` whether the file has
        // everything, and that answer would never come.
        let mut typing = session(64);
        assert_eq!(typing.sources().open_count(), 0);
        let source = typing.sources.open();
        typing.open = Some(Open {
            source,
            at: 0,
            registered: 0,
        });
        assert_eq!(typing.sources().open_count(), 1);
        typing.flush().expect("a seal over nothing is total");
        assert_eq!(
            typing.sources().open_count(),
            0,
            "the source is abandoned, not left open"
        );
        assert!(typing.open.is_none(), "and the session stops naming it");
        assert!(typing.pending().is_empty());
        assert_eq!(typing.sealed_bytes(), 0, "nothing was appended");
        assert_eq!(typing.spine().slab_count(), 0);
        // The reachable shape of the same thing: a cut that empties the only leaf.
        typing.insert(b"abc").expect("typed");
        typing.delete(0, 3).expect("deleted");
        assert_eq!(typing.spine().slab_count(), 0);
        assert_eq!(typing.sources().open_count(), 0);
        // And typing again opens a fresh source at the same place.
        typing.insert(b"x").expect("typed");
        assert_eq!(typing.document_len(), 1);
        assert_eq!(typing.sources().open_count(), 1);
        assert_eq!(typing.caret(), 1);
    }

    #[test]
    fn attach_over_no_bytes_moves_the_caret_without_opening_a_source() {
        // D-52's other half, also from inside. `attach(0)` opened a source whose leaf
        // `insert_slab` then drops as empty, so the register held a buffer that named nothing and
        // `open_count` climbed past the one buffer a caller can hand a reader. `restore_mark` is
        // the only caller that can hand it a zero, and no public route produces one today — so
        // the test states the contract where the contract lives.
        let mut typing = session(64);
        typing.insert(b"ab").expect("typed");
        assert_eq!(typing.sources().open_count(), 1);
        let open = typing.open.take().expect("a source is open");
        typing.sources.close(open.source);
        typing.buffer = SlabText::from_plaintext(Vec::new(), 64).expect("empty is UTF-8");
        typing.spine = typing.spine.remove_range(0, 2).expect("emptied");
        typing.next_at = Some(0);
        typing.draft.clear();
        typing.attach(0).expect("attaches nothing");
        assert_eq!(typing.sources().open_count(), 0, "no source over no bytes");
        assert!(typing.open.is_none());
        assert_eq!(typing.caret(), 0, "only the caret moved");
        assert_eq!(typing.document_len(), 0);
        assert_eq!(typing.spine().slab_count(), 0);
    }

    #[test]
    fn a_character_wider_than_the_buffer_is_refused_rather_than_spun_on() {
        // D-54: capacity one, and a three-byte character. The room is one, the split search walks
        // `take` to zero because no prefix of the character is a character boundary, and the old
        // loop sealed — which with nothing open does nothing — forever. The error is the contract.
        let mut typing = session(1);
        let refused = typing.insert("\u{2192}".as_bytes());
        assert!(
            matches!(
                refused,
                Err(TypingError::TooLarge {
                    needed: 3,
                    capacity: 1
                })
            ),
            "a character the buffer cannot hold is an error, not a hang: {refused:?}"
        );
        assert_eq!(typing.document_len(), 0);
        // A character that does fit still types, one byte at a time.
        typing.insert(b"a").expect("typed");
        assert_eq!(typing.document_len(), 1);
        assert_eq!(typing.pending(), b"a");
    }

    #[test]
    fn a_full_buffer_seals_once_and_carries_on_in_a_new_source() {
        let mut typing = session(8);
        typing.insert(b"0123456789ab").expect("typed");
        // 12 bytes into an 8-byte buffer: one seal of 8, four still open.
        assert_eq!(typing.sealed_bytes(), 8);
        assert_eq!(typing.pending(), b"89ab");
        assert_eq!(typing.spine().slab_count(), 2);
        assert_eq!(typing.document_len(), 12);
        assert_eq!(typing.spine().len(), 12, "the sealed 8 and the open 4");
        let sealed = typing.spine().slab_span(0).expect("a leaf");
        let open = typing.spine().slab_span(1).expect("a leaf");
        assert_eq!(sealed.len(), 8);
        assert_eq!(open.len(), 4);
        match typing.sources().locate(sealed.slab().segment()) {
            Location::Sealed { segment } => assert_eq!(segment, SegmentId::new(0)),
            other => panic!("the first source was sealed, got {other:?}"),
        }
        assert!(typing.sources().locate(open.slab().segment()).is_open());
    }

    #[test]
    fn a_burst_that_fills_the_buffer_splits_between_characters_and_never_inside_one() {
        // Capacity eight, six one-byte characters open: two bytes of room and a three-byte
        // character next. Taking what fits would cut the character in half and hand the slab bytes
        // that are not text (`Text(InvalidUtf8)` from a real million-keystroke run); the seal goes
        // first instead, and the whole character starts the next source.
        let mut typing = session(8);
        typing.insert(b"abcdef").expect("typed");
        typing.insert("\u{2192} ok".as_bytes()).expect("typed");
        assert_eq!(typing.document_len(), 12);
        assert_eq!(
            typing.sealed_bytes(),
            6,
            "six characters, and not half an arrow"
        );
        assert_eq!(typing.pending(), "\u{2192} ok".as_bytes());
        assert_eq!(typing.spine().len(), 12, "the sealed six and the open six");
    }

    #[test]
    fn sealing_corrects_the_length_so_the_geometry_is_final() {
        let mut typing = session(8);
        typing.insert(b"abc").expect("typed");
        typing.insert(b"de").expect("typed");
        // The leaf was registered at its first byte, so the tree says three and the overlay five.
        assert_eq!(typing.spine().len(), 3);
        assert_eq!(typing.document_len(), 5);
        typing.insert(b"fgh").expect("typed");
        typing.flush().expect("flushed");
        // After the seal the tree, the overlay and the user agree again.
        assert_eq!(typing.spine().len(), 8);
        assert_eq!(typing.document_len(), 8);
        assert!(typing.draft().is_empty());
        assert!(typing.pending().is_empty());
        assert_eq!(typing.sealed_bytes(), 8);
    }

    #[test]
    fn every_byte_reaches_the_world_line_exactly_once() {
        let mut typing = session(64);
        let typed: Vec<u8> = (0..1000u32)
            .flat_map(|n| format!("line {n}\n").into_bytes())
            .collect();
        // In small bursts, the way a keyboard delivers them.
        for burst in typed.chunks(7) {
            typing.insert(burst).expect("typed");
        }
        typing.flush().expect("flushed");
        let written = typing.sink.written();
        assert_eq!(
            written, typed,
            "the world-line holds the text, once, in order"
        );
        assert_eq!(typing.sealed_bytes(), typed.len() as u64);
        assert_eq!(typing.spine().len(), typed.len() as u64);
        assert_eq!(typing.document_len(), typed.len() as u64);
        // No root was copied per keystroke: the seal count is the buffer's, not the keyboard's.
        assert!(typing.spine().slab_count() as usize >= typed.len() / 64);
        assert!(typing.spine().slab_count() as usize <= typed.len() / 64 + 1);
    }

    #[test]
    fn typing_in_the_middle_splits_a_leaf_and_moves_no_bytes() {
        let mut typing = session(64);
        typing.insert(b"hello world").expect("typed");
        // Moving the caret to 5 seals what was open, then places a new source there.
        typing.insert_at(5, b" beautiful").expect("typed");
        typing.flush().expect("flushed");
        let spine = typing.spine();
        assert_eq!(spine.slab_count(), 3);
        let leaves = spine.spans_in(0, spine.len());
        let (first, middle, last) = (leaves[0], leaves[1], leaves[2]);
        assert_eq!((first.offset(), first.len()), (0, 5));
        assert_eq!((middle.offset(), middle.len()), (5, 10));
        assert_eq!((last.offset(), last.len()), (15, 6));
        // The outer two are halves of one sealed segment: a cut, not a rewrite.
        assert_eq!(first.slab().segment(), last.slab().segment());
        assert_eq!(first.slab().start(), 0);
        assert_eq!(last.slab().start(), 5);
        assert_ne!(middle.slab().segment(), first.slab().segment());
        assert_eq!(typing.document_len(), 21);
        assert_eq!(
            typing.sink.segments,
            vec![b"hello world".to_vec(), b" beautiful".to_vec()]
        );
    }

    #[test]
    fn moving_the_caret_seals_what_was_open() {
        let mut typing = session(64);
        typing.insert(b"abc").expect("typed");
        assert_eq!(typing.sealed_bytes(), 0);
        typing.insert_at(0, b"xy").expect("typed");
        assert_eq!(
            typing.sealed_bytes(),
            3,
            "the caret moved, so the open bytes were sealed"
        );
        assert_eq!(typing.pending(), b"xy");
        assert_eq!(typing.document_len(), 5);
        assert_eq!(typing.caret(), 2);
    }

    #[test]
    fn typing_at_the_end_does_not_seal() {
        let mut typing = session(64);
        typing.insert(b"abc").expect("typed");
        typing.insert_at(3, b"def").expect("typed");
        assert_eq!(typing.sealed_bytes(), 0);
        assert_eq!(typing.spine().slab_count(), 1);
        assert_eq!(typing.document_len(), 6);
    }

    #[test]
    fn a_caret_past_the_end_appends() {
        let mut typing = session(64);
        typing.insert(b"abc").expect("typed");
        typing.insert_at(99, b"def").expect("typed");
        assert_eq!(typing.document_len(), 6);
        assert_eq!(typing.caret(), 6);
    }

    #[test]
    fn a_mid_document_source_fills_and_seals_without_moving_other_bytes() {
        let mut typing = session(8);
        typing.insert(b"HEADTAIL").expect("typed");
        typing.insert_at(4, b"0123456789").expect("typed");
        typing.flush().expect("flushed");
        let spine = typing.spine();
        let leaves: Vec<(u64, u64)> = spine
            .spans_in(0, spine.len())
            .iter()
            .map(|span| (span.offset(), span.len()))
            .collect();
        assert_eq!(leaves, vec![(0, 4), (4, 8), (12, 2), (14, 4)]);
        assert_eq!(typing.document_len(), 18);
    }

    #[test]
    fn undo_of_later_typing_is_a_length_and_copies_no_root() {
        let mut typing = session(64);
        typing.insert(b"hello").expect("typed");
        typing.mark();
        typing.insert(b" world").expect("typed");
        let leaves = typing.spine().slab_count();
        assert!(typing.undo().expect("undone"));
        assert_eq!(typing.document_len(), 5);
        assert_eq!(typing.pending(), b"hello");
        assert_eq!(typing.caret(), 5);
        assert_eq!(typing.spine().slab_count(), leaves, "no root changed");
        assert_eq!(typing.spine().len(), 5, "the tree still says five");
        assert_eq!(typing.sealed_bytes(), 0, "and nothing was written");
        assert!(
            typing.draft().is_empty(),
            "back at the sealed length, the overlay has nothing to say"
        );
    }

    #[test]
    fn redo_restores_the_text_without_writing_it() {
        let mut typing = session(64);
        typing.insert(b"hello").expect("typed");
        typing.mark();
        typing.insert(b" world").expect("typed");
        assert!(typing.undo().expect("undone"));
        assert!(typing.redo().expect("redone"));
        assert_eq!(typing.document_len(), 11);
        assert_eq!(typing.pending(), b"hello world");
        assert_eq!(typing.sealed_bytes(), 0, "neither direction wrote a byte");
        assert_eq!(typing.spine().len(), 5, "and the tree never heard about it");
        assert_eq!(typing.caret(), 11);
    }

    #[test]
    fn undoing_the_first_byte_of_a_source_takes_the_leaf_and_redo_brings_it_back() {
        let mut typing = session(64);
        typing.mark();
        typing.insert(b"abc").expect("typed");
        assert_eq!(typing.spine().slab_count(), 1);
        assert!(typing.undo().expect("undone"));
        assert_eq!(
            typing.spine().slab_count(),
            0,
            "a leaf that carries nothing may not stay"
        );
        assert_eq!(typing.document_len(), 0);
        assert!(
            typing.pending().is_empty(),
            "the bytes are in the buffer and not in the document"
        );
        assert_eq!(typing.sources().open_count(), 0, "the source was abandoned");
        assert!(typing.redo().expect("redone"));
        assert_eq!(typing.document_len(), 3);
        assert_eq!(typing.spine().slab_count(), 1);
        assert_eq!(typing.pending(), b"abc");
        assert_eq!(typing.caret(), 3);
    }

    #[test]
    fn typing_after_an_undo_drops_the_undone_tail() {
        let mut typing = session(64);
        typing.insert(b"hello").expect("typed");
        typing.mark();
        typing.insert(b" world").expect("typed");
        assert!(typing.undo().expect("undone"));
        typing.insert(b"!").expect("typed");
        assert_eq!(
            typing.pending(),
            b"hello!",
            "the undone tail is dropped, not typed over in place"
        );
        assert_eq!(typing.document_len(), 6);
        assert_eq!(typing.redo_depth(), 0, "what is gone cannot be redone");
        assert!(!typing.redo().expect("nothing to redo"));
    }

    #[test]
    fn a_delete_pops_the_marks_it_invalidates() {
        let mut typing = session(64);
        typing.insert(b"abcdef").expect("typed");
        typing.mark();
        typing.insert(b"ghi").expect("typed");
        assert_eq!(typing.undo_depth(), 1);
        typing.delete(2, 4).expect("deleted");
        assert_eq!(
            typing.undo_depth(),
            0,
            "the cut destroyed bytes a mark promised were unchanged"
        );
        assert_eq!(typing.document_len(), 7);
        assert!(!typing.undo().expect("nothing left to undo"));
    }

    #[test]
    fn undoing_a_deletion_is_a_pointer_swap_and_writes_nothing() {
        let mut typing = session(8);
        typing.insert(b"0123456789").expect("typed");
        typing.flush().expect("flushed");
        let leaves = typing.spine().slab_count();
        let written = typing.sink.written().len();
        typing.delete(2, 4).expect("deleted");
        assert_eq!(typing.document_len(), 8);
        let top = typing
            .top_undo()
            .expect("the cut recorded the root before it");
        assert_eq!(top.kind, "root");
        assert_eq!(top.at, 2, "the root is the state the cut started from");
        assert!(typing.undo().expect("undone"));
        assert_eq!(typing.document_len(), 10, "the text came back");
        assert_eq!(typing.spine().len(), 10);
        assert_eq!(typing.spine().slab_count(), leaves);
        assert_eq!(typing.caret(), 2, "the caret is back at the cut");
        assert_eq!(
            typing.sink.written().len(),
            written,
            "undoing a cut wrote no byte, and neither did the cut"
        );
        assert_eq!(typing.sealed_bytes(), 10);
    }

    #[test]
    fn redo_of_a_deletion_cuts_the_same_range_again() {
        let mut typing = session(8);
        typing.insert(b"0123456789").expect("typed");
        typing.flush().expect("flushed");
        typing.delete(2, 4).expect("deleted");
        assert!(typing.undo().expect("undone"));
        assert!(typing.redo().expect("redone"));
        assert_eq!(typing.document_len(), 8);
        assert_eq!(typing.spine().len(), 8);
        assert_eq!(typing.caret(), 2);
        assert!(typing.undo().expect("undone again"));
        assert_eq!(typing.document_len(), 10, "redo left the root takable too");
    }

    #[test]
    fn a_seal_after_a_deletion_ends_its_undo() {
        let mut typing = session(8);
        typing.insert(b"0123456789").expect("typed");
        typing.flush().expect("flushed");
        typing.delete(2, 4).expect("deleted");
        assert_eq!(typing.top_undo().expect("the cut").kind, "root");
        // Nine bytes into a new eight-byte buffer: the ninth forces a seal, and the root recorded
        // before the cut would orphan the segment that seal appends — so it goes, and the seal's
        // own state is what is left (D-43).
        typing.insert(b"abcdefghi").expect("typed");
        assert_eq!(typing.sealed_bytes(), 18);
        assert_eq!(
            typing.undo_depth(),
            1,
            "the seal's state, and no cut behind it"
        );
        assert_eq!(typing.top_undo().expect("the seal").len, 8);
        assert!(typing.undo().expect("the sealed leaf comes back"));
        // Sixteen: the eight sealed bytes the cut left, plus the eight the seal wrote, live again.
        // The ninth byte was typed after the seal and belongs to that state, not this one.
        assert_eq!(typing.document_len(), 16);
    }

    #[test]
    fn a_seal_writes_the_live_bytes_and_not_the_undone_tail_behind_them() {
        let mut typing = session(64);
        typing.insert(b"abcdef").expect("typed");
        typing.mark();
        typing.insert(b"xyz").expect("typed");
        assert!(typing.undo().expect("undone"));
        // The three bytes are still in the buffer — that is what redo would hand back — and they
        // are not in the document.
        assert_eq!(typing.document_len(), 6);
        assert_eq!(typing.pending(), b"abcdef");
        typing.flush().expect("the buffer seals");
        assert_eq!(
            typing.document_len(),
            6,
            "a seal may not resurrect the bytes an undo took back"
        );
        assert_eq!(typing.spine().len(), 6);
        assert_eq!(typing.sealed_bytes(), 6);
        assert_eq!(typing.sink.segments, vec![b"abcdef".to_vec()]);
        assert!(
            typing.pending().is_empty(),
            "the buffer went with the seal, tail and all"
        );
    }

    #[test]
    fn typing_after_a_deletion_still_undoes_to_the_root() {
        let mut typing = session(64);
        typing.insert(b"0123456789").expect("typed");
        typing.flush().expect("flushed");
        typing.delete(2, 4).expect("deleted");
        typing.insert(b"xy").expect("typed");
        assert_eq!(typing.document_len(), 10);
        assert!(typing.undo().expect("undone"));
        assert_eq!(
            typing.document_len(),
            10,
            "the pending text went, the cut text came back"
        );
        assert!(typing.pending().is_empty(), "nothing is open");
        assert_eq!(typing.sources().open_count(), 0);
        assert_eq!(typing.caret(), 2);
    }

    #[test]
    fn redo_of_a_cut_puts_back_the_bytes_that_were_open() {
        let mut typing = session(64);
        typing.insert(b"0123456789").expect("typed");
        typing.flush().expect("flushed");
        typing.delete(2, 4).expect("deleted");
        typing.insert(b"xy").expect("typed");
        assert_eq!(
            typing.document_len(),
            10,
            "the cut document and two open bytes"
        );
        assert!(typing.undo().expect("undone"));
        // The undo goes to the root, and a root names written bytes only: the open `xy` is dropped,
        // because it was typed after the cut and the state before the cut did not have it.
        assert_eq!(
            typing.document_len(),
            10,
            "the cut text came back, `xy` did not"
        );
        assert!(
            typing.pending().is_empty(),
            "nothing is open after an undo of a root"
        );
        assert_eq!(typing.spine().len(), 10);
        assert!(typing.redo().expect("redone"));
        // The redo re-applies the cut **and** puts `xy` back where it was, under a new provisional
        // id — from the copy the step kept, because the buffer the bytes lived in went with the
        // undo. Undo then redo is a no-op; without that copy it would silently delete what the user
        // last typed, one byte at a time if that is how it was typed (OQ-21).
        assert_eq!(typing.document_len(), 10);
        assert_eq!(typing.pending(), b"xy");
        assert_eq!(typing.spine().len(), 10);
        let spans = typing.spine().spans_in(0, 10);
        let open = spans
            .iter()
            .find(|span| typing.sources().locate(span.slab().segment()).is_open())
            .expect("the pending bytes are a leaf of their own");
        assert_eq!(open.offset(), 2, "back where the caret was, not appended");
        assert_eq!(open.len(), 2);
        // And what the redo produced is still a document: the cut can be taken back again.
        assert!(typing.undo().expect("undone again"));
        assert_eq!(typing.document_len(), 10);
    }

    #[test]
    fn redo_of_a_mid_document_insert_puts_the_text_back_where_it_was() {
        let mut typing = session(64);
        typing.insert(b"abcdef").expect("typed");
        typing.flush().expect("flushed");
        typing.mark();
        typing.insert_at(1, b"XY").expect("typed");
        assert_eq!(typing.document_len(), 8);
        assert!(typing.undo().expect("undone"));
        assert_eq!(typing.document_len(), 6, "the text is out of the document");
        assert!(typing.redo().expect("redone"));
        // Undo took the leaf out of the tree and left the caret at the end; the mark remembers
        // where the leaf was, so redo puts it back there instead of appending it — the bytes are
        // the same either way, and the document reads differently.
        assert_eq!(typing.document_len(), 8);
        assert_eq!(typing.pending(), b"XY");
        assert_eq!(
            typing.caret(),
            3,
            "the caret sits after the re-inserted text"
        );
        let spans = typing.spine().spans_in(0, 8);
        assert_eq!(spans.len(), 3, "a character, the open leaf, then the rest");
        assert_eq!(spans[1].offset(), 1, "the open leaf is back at offset one");
        assert_eq!(spans[1].len(), 2);
        assert!(typing.sources().locate(spans[1].slab().segment()).is_open());
        assert_eq!(spans[2].offset(), 3);
    }

    #[test]
    fn replacing_sealed_text_is_one_undoable_action() {
        let mut typing = session(8);
        typing.insert(b"hello world").expect("typed");
        typing.flush().expect("flushed");
        typing.replace(6, 11, b"there").expect("replaced");
        assert_eq!(typing.document_len(), 11);
        assert_eq!(typing.caret(), 11);
        assert!(typing.undo().expect("undone"));
        assert_eq!(
            typing.document_len(),
            11,
            "the old text is back and the new text is gone"
        );
        assert_eq!(typing.caret(), 6, "the caret is where the selection began");
        assert!(typing.pending().is_empty());
    }

    #[test]
    fn the_root_stack_is_a_ring_and_the_world_line_is_the_deeper_store() {
        let mut typing = session(256);
        let text: Vec<u8> = (0..200u32).map(|n| b'a' + (n % 26) as u8).collect();
        typing.insert(&text).expect("typed");
        typing.flush().expect("flushed");
        for _ in 0..(ROOT_DEPTH + 10) {
            typing.delete(0, 1).expect("deleted");
        }
        assert_eq!(
            typing.undo_depth(),
            ROOT_DEPTH,
            "the ring holds roots, not the whole history"
        );
        // Seventy-four cuts of a byte leave 126; the newest root is the state before the last cut.
        assert_eq!(typing.document_len(), 126);
        assert!(typing.undo().expect("the newest root is still there"));
        assert_eq!(typing.document_len(), 127);
    }

    #[test]
    fn a_seal_keeps_the_state_it_sealed() {
        // D-43. This used to be `a_seal_is_an_undo_floor`: a seal cleared the stack, so the text
        // that had just gone to the world-line could not be taken back — and since a caret move
        // seals, an arrow key made the typing before it permanent. The seal leaves one state now:
        // the leaf it wrote, with a copy of its bytes.
        let mut typing = session(8);
        typing.mark();
        // Twelve bytes into an eight-byte buffer: a seal happens inside the action.
        typing.insert(b"0123456789ab").expect("typed");
        assert_eq!(typing.sealed_bytes(), 8);
        assert_eq!(typing.undo_depth(), 1, "the seal's own state, and no more");

        let top = typing.top_undo().expect("a state to unwind");
        assert_eq!(top.kind, "root");
        // The mark named a state with nothing open, so the seal keeps exactly that: the eight bytes
        // it wrote are outside it, and one undo takes the typing back rather than doing nothing.
        assert_eq!(top.len, 0);

        assert!(typing.undo().expect("the typing can be taken back"));
        assert_eq!(typing.document_len(), 0);
        assert!(!typing.undo().expect("nothing older in this session"));
    }

    #[test]
    fn the_undo_stack_is_bounded_and_the_document_is_not() {
        let mut typing = session(4096);
        for _ in 0..(UNDO_DEPTH + 50) {
            typing.mark();
            typing.insert(b"x").expect("typed");
        }
        assert_eq!(
            typing.undo_depth(),
            UNDO_DEPTH,
            "the oldest mark is dropped, not the newest"
        );
        assert_eq!(typing.document_len(), (UNDO_DEPTH + 50) as u64);
    }

    #[test]
    fn replacing_a_selection_is_a_delete_and_an_insert() {
        let mut typing = session(64);
        typing.insert(b"hello world").expect("typed");
        typing.replace(6, 11, b"there").expect("replaced");
        assert_eq!(typing.pending(), b"hello there");
        assert_eq!(typing.document_len(), 11);
        assert_eq!(typing.caret(), 11, "the caret ends after what was typed");
        assert_eq!(
            typing.sealed_bytes(),
            0,
            "a replacement inside the buffer writes nothing"
        );
        assert_eq!(typing.spine().slab_count(), 1);
    }

    #[test]
    fn backspace_inside_the_open_buffer_writes_nothing() {
        let mut typing = session(64);
        typing.insert(b"hello world").expect("typed");
        typing.delete(5, 11).expect("deleted");
        assert_eq!(typing.pending(), b"hello");
        assert_eq!(typing.document_len(), 5);
        assert_eq!(typing.spine().slab_count(), 1, "no cut, so no fragment");
        assert_eq!(typing.caret(), 5);
        assert!(
            typing.sink.segments.is_empty(),
            "a backspace seals nothing at all"
        );
        assert_eq!(typing.sealed_bytes(), 0);
    }

    #[test]
    fn deleting_the_whole_open_buffer_takes_the_leaf_with_it() {
        let mut typing = session(64);
        typing.insert(b"abc").expect("typed");
        typing.delete(0, 3).expect("deleted");
        assert_eq!(
            typing.spine().slab_count(),
            0,
            "no empty leaf is left behind"
        );
        assert_eq!(typing.spine().len(), 0);
        assert_eq!(typing.document_len(), 0);
        assert!(typing.pending().is_empty());
        assert_eq!(
            typing.sources().open_count(),
            0,
            "the source was abandoned, not sealed"
        );
        assert_eq!(typing.sealed_bytes(), 0);
        assert_eq!(typing.caret(), 0, "the caret is where the text began");
    }

    #[test]
    fn deleting_sealed_text_costs_a_root_copy_and_no_bytes() {
        let mut typing = session(8);
        typing.insert(b"0123456789abcdefghij").expect("typed");
        typing.flush().expect("flushed");
        assert_eq!(typing.sealed_bytes(), 20);
        let segments = typing.sink.segments.len();
        typing.delete(2, 12).expect("deleted");
        assert_eq!(typing.sealed_bytes(), 20, "deletion wrote nothing");
        assert_eq!(
            typing.sink.segments.len(),
            segments,
            "and sealed no segment"
        );
        assert_eq!(typing.document_len(), 10);
        assert_eq!(typing.spine().len(), 10, "the tree and the document agree");
        // Three leaves, not two: the leaves the cut lands in are split, and nothing rejoins them.
        assert_eq!(typing.spine().slab_count(), 3);
        // And the fragments point at the right bytes: keeping bytes 0..2 of segment 0 and 4..8 of
        // segment 1 means inner offsets 0 and 4 — the middle one is the check, because it is the
        // only offset a bounds-only implementation could get wrong — so the document reads
        // "01" + "cdef" + "ghij".
        let spans: Vec<(u64, u64, u32)> = typing
            .spine()
            .spans_in(0, typing.spine().len())
            .iter()
            .map(|span| (span.offset(), span.len(), span.slab().start()))
            .collect();
        assert_eq!(spans, vec![(0, 2, 0), (2, 4, 4), (6, 4, 0)]);
        // The removed bytes are still in the world-line. That is OQ-14, and it is why this is
        // called deletion and not erasure.
        assert_eq!(typing.sink.written().len(), 20);
    }

    #[test]
    fn deleting_across_the_open_leaf_seals_it_first() {
        let mut typing = session(8);
        // Eight bytes seal, and two are still open in the second source.
        typing.insert(b"0123456789").expect("typed");
        assert_eq!(typing.sealed_bytes(), 8);
        assert_eq!(typing.pending(), b"89");
        typing.delete(6, 10).expect("deleted");
        assert_eq!(
            typing.sealed_bytes(),
            10,
            "the open bytes were sealed before the cut"
        );
        assert!(typing.pending().is_empty());
        assert!(
            typing.draft().is_empty(),
            "nothing is open, so nothing is drafted"
        );
        assert_eq!(typing.document_len(), 6);
        assert_eq!(typing.spine().len(), 6);
        assert_eq!(typing.caret(), 6);
    }

    #[test]
    fn a_half_character_is_refused_while_open_and_not_checked_once_sealed() {
        let mut typing = session(64);
        typing.insert("aéb".as_bytes()).expect("typed");
        // In the buffer the bytes are here, so the edit knows where the characters are and refuses.
        let refused = typing.delete(1, 2).expect_err("half a character");
        assert!(matches!(refused, TypingError::Text(_)));
        assert_eq!(typing.pending(), "aéb".as_bytes());
        typing.flush().expect("flushed");
        // Sealed, the session has no reader and cannot tell: the cut goes through. That
        // asymmetry is OQ-15, and it is why the caret that calls this is an Eye cluster boundary.
        typing
            .delete(1, 2)
            .expect("the geometry does not check characters");
        assert_eq!(typing.document_len(), 3);
        assert_eq!(typing.spine().len(), 3);
    }

    #[test]
    fn typing_after_a_deletion_continues_where_the_text_was_taken_out() {
        let mut typing = session(64);
        typing.insert(b"0123456789").expect("typed");
        typing.delete(4, 8).expect("deleted");
        assert_eq!(typing.pending(), b"012389", "the buffer closed the gap");
        typing.insert(b"XY").expect("typed");
        assert_eq!(typing.pending(), b"012389XY");
        // Take everything out: the leaf goes, and the next keystroke starts a new one at zero.
        typing.delete(0, 99).expect("deleted");
        assert_eq!(typing.document_len(), 0);
        assert_eq!(typing.spine().slab_count(), 0);
        assert_eq!(typing.caret(), 0);
        typing.insert(b"z").expect("typed");
        assert_eq!(typing.caret(), 1);
        assert_eq!(typing.document_len(), 1);
        assert_eq!(typing.spine().len(), 1);
    }

    #[test]
    fn a_burst_larger_than_a_buffer_is_split_across_seals() {
        let mut typing = session(8);
        // Twenty bytes in one call: two seals of eight, four left open.
        typing.insert(b"0123456789abcdefghij").expect("typed");
        assert_eq!(typing.sealed_bytes(), 16);
        assert_eq!(typing.pending(), b"ghij");
        assert_eq!(typing.document_len(), 20);
        assert_eq!(typing.spine().slab_count(), 3);
    }

    #[derive(Debug, Default)]
    struct SpineRecorder {
        segments: Vec<Vec<u8>>,
        spines: Vec<Vec<Slab>>,
        fail_commit: bool,
    }

    impl SegmentSink for SpineRecorder {
        fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
            let segment = SegmentId::new(self.segments.len() as u64);
            self.segments.push(plaintext.to_vec());
            Ok(Appended {
                segment,
                witness_sequence: self.segments.len() as u64,
                segment_root: [0u8; 32],
                bytes_written: plaintext.len() as u64,
            })
        }

        fn append_segment_with_spine(
            &mut self,
            plaintext: &[u8],
            before: &[Slab],
            after: &[Slab],
        ) -> Result<Appended, String> {
            let appended = self.append_segment(plaintext)?;
            let mut full = before.to_vec();
            full.push(Slab::new(
                appended.segment,
                0,
                u32::try_from(plaintext.len()).expect("fits"),
            ));
            full.extend_from_slice(after);
            self.spines.push(full);
            Ok(appended)
        }

        fn commit_spine(&mut self, slabs: &[Slab]) -> Result<(), String> {
            if self.fail_commit {
                return Err("injected manifest commit failure".to_string());
            }
            self.spines.push(slabs.to_vec());
            Ok(())
        }
    }

    #[test]
    fn mid_segment_insert_and_sealed_deletion_commit_exact_slabs_without_rewriting_bytes() {
        let mut typing = Typing::new(SpineRecorder::default(), 64);
        typing.insert(b"hello world").expect("typed");
        typing.flush().expect("flushed");
        assert!(!typing.has_uncommitted_structure());
        assert_eq!(typing.sink.segments.len(), 1);

        // Insert "dear " in the middle of sealed segment 0 (`0..6` + `new` + `6..11`).
        typing.insert_at(6, b"dear ").expect("inserted mid-segment");
        typing.flush().expect("flushed mid-segment insert");
        assert_eq!(typing.sink.segments.len(), 2);
        assert_eq!(
            typing.sink.spines.last().expect("recorded spine"),
            &vec![
                Slab::new(SegmentId::new(0), 0, 6),
                Slab::new(SegmentId::new(1), 0, 5),
                Slab::new(SegmentId::new(0), 6, 5),
            ]
        );

        // Delete across sealed text ("dear "): appends ZERO new segments and commits the cut.
        typing.delete(6, 11).expect("deleted sealed text");
        assert!(typing.has_uncommitted_structure());

        // A failed commit keeps `has_uncommitted_structure()` true so "saved" is never claimed.
        typing.sink.fail_commit = true;
        assert!(typing.flush().is_err());
        assert!(typing.has_uncommitted_structure());

        typing.sink.fail_commit = false;
        typing.flush().expect("committed cut");
        assert!(!typing.has_uncommitted_structure());
        assert_eq!(
            typing.sink.segments.len(),
            2,
            "deletion across sealed text must not append or rewrite segments"
        );
        assert_eq!(
            typing.sink.spines.last().expect("recorded cut"),
            &vec![
                Slab::new(SegmentId::new(0), 0, 6),
                Slab::new(SegmentId::new(0), 6, 5),
            ]
        );

        // Emptying the entire document commits an explicit empty slab list.
        typing.delete(0, typing.document_len()).expect("emptied");
        assert!(typing.has_uncommitted_structure());
        typing.flush().expect("committed empty document");
        assert!(!typing.has_uncommitted_structure());
        assert!(typing
            .sink
            .spines
            .last()
            .expect("recorded empty")
            .is_empty());
    }

    #[test]
    fn undo_after_a_sealing_move_marks_structure_dirty_until_flushed() {
        let mut typing = Typing::new(SpineRecorder::default(), 64);
        typing.mark();
        typing.insert(b"abc").expect("typed");
        // Moving the caret seals segment 0 to the sink while keeping the burst undoable (D-43).
        typing.insert_at(1, b"").expect("caret move sealed");
        assert_eq!(typing.sink.segments.len(), 1);
        assert!(!typing.has_uncommitted_structure());

        assert!(typing.undo().expect("undid sealed burst"));
        assert_eq!(typing.document_len(), 0);
        assert!(
            typing.has_uncommitted_structure(),
            "disk holds segment 0 while the undone session is empty"
        );
        typing.flush().expect("flushed undone state");
        assert!(!typing.has_uncommitted_structure());
        assert!(typing
            .sink
            .spines
            .last()
            .expect("recorded empty")
            .is_empty());
    }
}
