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
//! # What this does not do yet
//!
//! The open source is the document's **tail**. Typing in the middle of a document splits a leaf and
//! needs the caret, which is the editor's model, not the storage's; that is the next slice, and
//! nothing here pretends otherwise: `at` is always `spine.len()`.

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

/// A document's tail while it is being typed into.
pub struct Typing<S> {
    spine: Spine,
    draft: Draft,
    sources: OpenSources,
    sink: S,
    buffer: SlabText,
    capacity: usize,
    open: Option<Open>,
    sealed_bytes: u64,
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
            sealed_bytes: 0,
        }
    }

    /// Types `bytes` at the end of the document, sealing as often as the buffer requires.
    ///
    /// # Errors
    ///
    /// A text, spine or sink error. A refused seal leaves the text in the buffer, so nothing typed
    /// is lost, and a burst larger than a whole buffer is split across seals rather than refused.
    pub fn insert(&mut self, bytes: &[u8]) -> Result<(), TypingError> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let room = self.capacity - self.buffer.len();
            if room == 0 {
                self.seal()?;
                continue;
            }
            // Take only what fits, so one burst can be many keystrokes and still never hides an
            // allocation inside an edit: the split happens here, at a seal, where it is counted.
            let take = room.min(rest.len());
            let at = self.buffer.len();
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
        self.buffer.as_bytes()
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
                let at = self.spine.len();
                self.spine = self.spine.append(Slab::new(source, 0, registered));
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
        self.open = None;
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
