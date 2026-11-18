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
//! **Deletion does not erase.** The world-line still holds the ciphertext of what was removed, and
//! a reader with the passphrase can still decrypt it. An authenticated append-only log cannot do
//! better without a compaction policy, and this session has none; AUDIT's OQ-14 records that,
//! rather than letting "deleted" quietly mean two different things.
//!
//! # Undo is a length, not a copy
//!
//! [`Typing::mark`] records a state and [`Typing::undo`] returns to it, and the record is a
//! **length**: an undo changes how much of the buffer the document includes, not what is in it.
//! Undo and redo therefore copy no root, write no byte and allocate nothing, and redo works because
//! the bytes were never discarded — the tail simply stops being part of the document until
//! something edits it.
//!
//! What undo cannot do is bring back bytes an edit destroyed. A mark promises that the bytes before
//! its length are unchanged, so a delete inside the buffer pops the marks it invalidates, and a
//! seal pops all of them because the bytes move to the world-line and the buffer is dropped.
//! Undoing a deletion needs the root from before the cut — a pointer swap, and the next slice.

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
    /// The source that is still open held this many bytes.
    Live {
        /// The open source's length at that moment.
        len: usize,
    },
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
    marks: Vec<Mark>,
    /// States an undo left behind, oldest first — the redo stack.
    undone: Vec<Mark>,
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
            marks: Vec::new(),
            undone: Vec::new(),
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
    /// is the tree's own and no correction is needed — and then takes the range out of the spine.
    /// Either way **no bytes are written**: the world-line keeps what was deleted, which is OQ-14's
    /// subject, and the leaf fragments a cut leaves behind are OQ-9's.
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
        // Something sealed is involved: seal what is open so every offset is exact, then cut.
        self.flush()?;
        self.spine = self.spine.remove_range(start, end)?;
        self.next_at = Some(start);
        Ok(())
    }

    /// Records a state the session can undo back to, and drops any redo stack.
    ///
    /// Call this before a **user action**, not before every keystroke: what counts as one action (a
    /// pause, a word, a paste) is knowledge the shell has and this layer does not.
    pub fn mark(&mut self) {
        self.undone.clear();
        let mark = self.current_mark();
        if self.marks.len() == UNDO_DEPTH {
            self.marks.remove(0);
        }
        self.marks.push(mark);
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
        let Some(mark) = self.marks.pop() else {
            return Ok(false);
        };
        let current = self.current_mark();
        self.restore(mark)?;
        self.undone.push(current);
        Ok(true)
    }

    /// Re-applies what [`Self::undo`] last took back, reporting whether there was anything.
    ///
    /// # Errors
    ///
    /// As [`Self::undo`]: restoring a state with an open source may have to register a leaf.
    pub fn redo(&mut self) -> Result<bool, TypingError> {
        let Some(mark) = self.undone.pop() else {
            return Ok(false);
        };
        let current = self.current_mark();
        self.restore(mark)?;
        self.marks.push(current);
        Ok(true)
    }

    /// How many undo states the session holds.
    #[must_use]
    pub fn undo_depth(&self) -> usize {
        self.marks.len()
    }

    /// How many states [`Self::redo`] can re-apply.
    #[must_use]
    pub fn redo_depth(&self) -> usize {
        self.undone.len()
    }

    /// Replaces `start..end` with `bytes` — typing over a selection.
    ///
    /// It is [`Self::delete`] then [`Self::insert_at`], which is the whole implementation as well
    /// as the specification: the caret ends after the inserted text, and a replacement that
    /// destroys bytes can no more be undone than the deletion inside it can (see the module
    /// documentation).
    ///
    /// # Errors
    ///
    /// As [`Self::delete`] and [`Self::insert`].
    pub fn replace(&mut self, start: u64, end: u64, bytes: &[u8]) -> Result<(), TypingError> {
        self.delete(start, end)?;
        self.insert_at(start, bytes)
    }

    /// The state the session is in now, as a mark would record it.
    fn current_mark(&self) -> Mark {
        match self.open {
            Some(_) => Mark::Live {
                len: self.live_len(),
            },
            None => Mark::Sealed { at: self.caret() },
        }
    }

    /// Moves the session to `mark`, touching lengths and at most one leaf — never bytes.
    fn restore(&mut self, mark: Mark) -> Result<(), TypingError> {
        match (mark, self.open) {
            (Mark::Live { len }, Some(_)) => self.set_live(len),
            (Mark::Live { len }, None) => self.attach(len),
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

    /// Sets the open leaf's live length, without touching the buffer.
    fn set_live(&mut self, len: usize) -> Result<(), TypingError> {
        let Some(open) = self.open else {
            return Ok(());
        };
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
        let registered = u32::try_from(len).expect("capacity fits in u32");
        let source = self.sources.open();
        let at = self.next_at.take().unwrap_or_else(|| self.spine.len());
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
        while let Some(Mark::Live { len: mark }) = self.marks.last().copied() {
            if mark <= len {
                break;
            }
            self.marks.pop();
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
            let take = room.min(rest.len());
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
        &self.buffer.as_bytes()[..self.live_len()]
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

    /// Seals the open source: append once, rename, correct the leaf's length, drop the draft.
    fn seal(&mut self) -> Result<(), TypingError> {
        let Some(open) = self.open else {
            return Ok(());
        };
        let len = self.buffer.len();
        if len == 0 {
            return Ok(());
        }
        let appended: Appended = self
            .sink
            .append_segment(self.buffer.as_bytes())
            .map_err(|reason| TypingError::Append { reason })?;
        self.sources
            .seal(open.source, appended.segment)
            .expect("the register handed this id out");
        let corrected = Slab::new(
            open.source,
            0,
            u32::try_from(len).expect("capacity fits in u32"),
        );
        let end = open.at + u64::from(open.registered);
        self.spine = self.spine.replace_range(open.at, end, corrected)?;
        self.draft.forget(open.at);
        self.sealed_bytes += u64::try_from(len).expect("a buffer fits in u64");
        self.next_at = Some(open.at + u64::try_from(len).expect("a buffer fits in u64"));
        self.open = None;
        // A seal is an undo floor: the bytes move to the world-line and the buffer is dropped, so
        // no mark that promises them can be honoured any more.
        self.marks.clear();
        self.undone.clear();
        self.buffer = SlabText::from_plaintext(Vec::new(), self.capacity)
            .expect("an empty slab is valid UTF-8");
        Ok(())
    }
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
    fn a_seal_is_an_undo_floor() {
        let mut typing = session(8);
        typing.mark();
        // Twelve bytes into an eight-byte buffer: a seal happens inside the action.
        typing.insert(b"0123456789ab").expect("typed");
        assert_eq!(typing.sealed_bytes(), 8);
        assert_eq!(typing.undo_depth(), 0, "the bytes are the world-line's now");
        assert!(!typing.undo().expect("nothing to undo"));
        assert_eq!(typing.document_len(), 12);
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
}
