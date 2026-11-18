//! Selection: a range in the live document, and the bytes behind it.
//!
//! A selection is two offsets — the anchor where the gesture started and the focus where the caret
//! is — and nothing else. That is deliberate: the document is geometry, so a selection needs no
//! storage of its own, and the reversed case (a selection made right to left) is a normalisation
//! rather than a second type.
//!
//! Two consumers want different halves of the same range, and both get them here without
//! re-deriving anything:
//!
//! * a **painter** wants the leaves the range covers, in document order, with live offsets — that
//!   is [`Selection::spans`], which is [`Draft::spans_in`] with the clamping done;
//! * a **clipboard** wants the bytes — that is [`copy`], which reads exactly the part of each leaf
//!   the selection covers.
//!
//! The second is the constitutional clause at clipboard granularity: *never decrypt more than you
//! look at*. Copying two bytes out of the middle of a 64 KiB leaf faults that leaf and returns two
//! bytes; it does not return the leaf, and it does not touch the leaves on either side. What it
//! does *not* decide is anything about the operating system's clipboard — flagged confidential,
//! cleared on blur — which is per-platform work and stays open in AUDIT's OQ-3.
//!
//! # Where a leaf's bytes are is the register's answer
//!
//! This module contains the crate's first **reader**, and the first thing a reader learns is that a
//! leaf does not say where its bytes are. Since D-7 a leaf names a *source*, and a seal renames the
//! source in the register without touching a single leaf — so the same slab names a provisional id
//! before the seal and after it, and only [`OpenSources::locate`] knows whether that id is a buffer
//! being typed into or a segment in the world-line. Reading `span.slab().segment()` directly is
//! therefore a bug that hides until the first seal: it asks the world-line for the provisional id,
//! which no segment has.
//!
//! A drafted leaf needs one more distinction. [`Draft::spans_in`] returns a drafted leaf **whole**,
//! because a draft is about a length and not a sub-range, so [`copy`] clips it here. And a draft
//! whose bytes are *not* the session's open buffer — a resident slab edited in place before a
//! commit — is refused rather than guessed at, because those bytes live in the resident set and not
//! in the buffer this function is handed.

use crate::draft::{Draft, LiveSpan};
use crate::residency::{ResidencyError, ResidentSet, SegmentSource};
use crate::slab::SegmentId;
use crate::source::{Location, OpenSources};
use crate::spine::Spine;

/// A range in the live document: where the gesture started, and where the caret is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Where the selection began — the end that does not move as the gesture grows.
    anchor: u64,
    /// Where the caret is: the end that moves.
    focus: u64,
}

impl Selection {
    /// A selection from `anchor` to `focus`, in either direction.
    #[must_use]
    pub const fn new(anchor: u64, focus: u64) -> Self {
        Self { anchor, focus }
    }

    /// A selection of nothing, at `at` — what a click leaves behind.
    #[must_use]
    pub const fn caret(at: u64) -> Self {
        Self {
            anchor: at,
            focus: at,
        }
    }

    /// The end that does not move as the gesture grows.
    #[must_use]
    pub const fn anchor(self) -> u64 {
        self.anchor
    }

    /// The end that moves — and where the caret is painted.
    #[must_use]
    pub const fn focus(self) -> u64 {
        self.focus
    }

    /// The caret's offset. Typing replaces the selection and lands here.
    #[must_use]
    pub const fn caret_offset(self) -> u64 {
        self.focus
    }

    /// Whether the selection covers no bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.anchor == self.focus
    }

    /// The range as `start..end`, whichever way the gesture went.
    #[must_use]
    pub const fn normalised(self) -> (u64, u64) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// The same selection with both ends pulled inside a document of `document_len` bytes.
    ///
    /// Each end is clamped independently, so a selection that runs past the end keeps its direction
    /// and its anchor on the far side of the caret.
    #[must_use]
    pub const fn clamped(self, document_len: u64) -> Self {
        Self {
            anchor: if self.anchor > document_len {
                document_len
            } else {
                self.anchor
            },
            focus: if self.focus > document_len {
                document_len
            } else {
                self.focus
            },
        }
    }

    /// The live leaves the selection covers, in document order.
    ///
    /// Live, not sealed: a leaf being typed into is reported with the length it has now, which is
    /// exactly what a painter needs and what [`copy`] reads through.
    #[must_use]
    pub fn spans(self, spine: &Spine, draft: &Draft) -> Vec<LiveSpan> {
        let (start, end) = self.clamped(draft.document_len(spine)).normalised();
        draft.spans_in(spine, start, end)
    }
}

/// Why the bytes behind a selection could not be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyError {
    /// The resident set refused to fault a sealed leaf in.
    Residency(ResidencyError),
    /// A sealed leaf names a range its segment is too short to hold — a corrupt or stale spine.
    SegmentTooShort {
        /// The segment that was too short.
        segment: SegmentId,
        /// The end offset the leaf claims.
        needed: usize,
        /// The bytes the segment actually has.
        have: usize,
    },
    /// The open buffer is shorter than the leaf that names it.
    OpenTooShort {
        /// The end offset the leaf claims.
        needed: usize,
        /// The bytes the buffer actually has.
        have: usize,
    },
    /// A leaf names a source the register does not know: neither open nor sealed.
    ///
    /// Unreachable from a consistent session — a closed source's leaf is taken out of the spine —
    /// and reading the buffer or the world-line instead of saying so would be silent corruption.
    UnknownSource {
        /// The segment the leaf names.
        segment: SegmentId,
    },
    /// A drafted leaf whose bytes are not the open buffer this function was handed.
    ///
    /// A draft over the session's own source is the buffer at offset zero. A draft over sealed
    /// geometry means resident text edited in place before a commit — the commit boundary's
    /// business — and the live bytes are in the resident set, not here.
    DraftNotInTheSession {
        /// The segment the drafted leaf names.
        segment: SegmentId,
        /// The offset within that segment where the live text starts.
        start: u32,
    },
}

impl std::fmt::Display for CopyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Residency(error) => write!(formatter, "{error}"),
            Self::SegmentTooShort {
                segment,
                needed,
                have,
            } => write!(
                formatter,
                "{segment} holds {have} bytes but a leaf claims {needed}"
            ),
            Self::OpenTooShort { needed, have } => write!(
                formatter,
                "the open buffer holds {have} bytes but a leaf claims {needed}"
            ),
            Self::UnknownSource { segment } => {
                write!(formatter, "the register does not know {segment}")
            }
            Self::DraftNotInTheSession { segment, start } => write!(
                formatter,
                "the drafted leaf names {segment} at {start}: resident text, not the open buffer"
            ),
        }
    }
}

impl std::error::Error for CopyError {}

impl From<ResidencyError> for CopyError {
    fn from(error: ResidencyError) -> Self {
        Self::Residency(error)
    }
}

/// Reads the bytes behind `selection`, faulting in exactly the leaves it touches.
///
/// `sources` is the register, and it is what makes this correct: a leaf names a source, and only
/// the register knows whether that source is a buffer or a segment ([`OpenSources::locate`]).
/// `open` is the session's pending bytes — the buffer an open source's leaves name. Sealed leaves
/// are faulted through `residency` one at a time and only for the part the selection covers, so the
/// bytes returned are the selection's bytes and no more.
///
/// # Errors
///
/// A leaf naming a source the register does not know, a fault-in the resident set refuses, a leaf
/// that claims more bytes than its segment or the open buffer holds, or a drafted leaf whose bytes
/// are resident text rather than the session's buffer.
pub fn copy<R: SegmentSource>(
    residency: &mut ResidentSet<R>,
    spine: &Spine,
    draft: &Draft,
    sources: &OpenSources,
    selection: Selection,
    open: &[u8],
) -> Result<Vec<u8>, CopyError> {
    let (start, end) = selection.clamped(draft.document_len(spine)).normalised();
    let mut bytes = Vec::new();
    for span in draft.spans_in(spine, start, end) {
        // A sealed span is already clipped to the range; a drafted span is returned whole, so the
        // clip below is what keeps a copy from reading past the selection.
        let low = span.offset().max(start);
        let high = (span.offset() + span.len()).min(end);
        if low >= high {
            continue;
        }
        let from = u64::from(span.slab().start()) + (low - span.offset());
        let to = from + (high - low);
        let from = usize::try_from(from).expect("a leaf fits in usize");
        let to = usize::try_from(to).expect("a leaf fits in usize");
        let named = span.slab().segment();
        match sources.locate(named) {
            Location::Open { .. } => {
                // A span clipped to the selection inside an open leaf starts at that offset *in the
                // buffer*, so a non-zero inner start is normal here and `from` already accounts for
                // it. What would not be normal is the buffer ending before the span does.
                let have = open.len();
                let slice = open
                    .get(from..to)
                    .ok_or(CopyError::OpenTooShort { needed: to, have })?;
                bytes.extend_from_slice(slice);
            }
            Location::Sealed { segment } if span.is_draft() => {
                return Err(CopyError::DraftNotInTheSession {
                    segment,
                    start: span.slab().start(),
                });
            }
            Location::Sealed { segment } => {
                let leaf = residency.fault_in(segment)?;
                let have = leaf.len();
                let slice = leaf.get(from..to).ok_or(CopyError::SegmentTooShort {
                    segment,
                    needed: to,
                    have,
                })?;
                bytes.extend_from_slice(slice);
            }
            Location::Unknown => return Err(CopyError::UnknownSource { segment: named }),
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::residency::ResidentSet;
    use crate::slab::{Slab, SlabSpan};
    use crate::typing::{Recorder, Typing};
    use std::collections::BTreeMap;

    /// A stand-in for the world-line: the plaintext of every segment, by id.
    #[derive(Debug, Default)]
    struct Segments(BTreeMap<u64, Vec<u8>>);

    impl Segments {
        fn of(segments: &[&[u8]]) -> Self {
            Self(
                segments
                    .iter()
                    .enumerate()
                    .map(|(index, bytes)| (index as u64, bytes.to_vec()))
                    .collect(),
            )
        }
    }

    impl SegmentSource for Segments {
        fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
            self.0
                .get(&segment.get())
                .cloned()
                .ok_or_else(|| format!("no {segment}"))
        }
    }

    fn residency(segments: &[&[u8]]) -> ResidentSet<Segments> {
        ResidentSet::new(Segments::of(segments), 1024, 64).expect("a workable residency")
    }

    fn session(capacity: usize) -> Typing<Recorder> {
        Typing::new(Recorder::default(), capacity)
    }

    /// Copies out of a typing session, so the tests exercise the register as a caller would.
    fn copy_from(
        typing: &Typing<Recorder>,
        residents: &mut ResidentSet<Segments>,
        selection: Selection,
    ) -> Result<Vec<u8>, CopyError> {
        copy(
            residents,
            typing.spine(),
            typing.draft(),
            typing.sources(),
            selection,
            typing.pending(),
        )
    }

    #[test]
    fn a_reversed_selection_normalises_and_keeps_the_caret_where_the_gesture_ended() {
        let selection = Selection::new(9, 4);
        assert_eq!(selection.normalised(), (4, 9));
        assert_eq!(selection.anchor(), 9);
        assert_eq!(selection.caret_offset(), 4);
        assert!(!selection.is_empty());
        assert!(Selection::caret(7).is_empty());
        assert_eq!(Selection::caret(7).normalised(), (7, 7));
    }

    #[test]
    fn a_selection_clamps_to_the_document_and_keeps_its_direction() {
        let selection = Selection::new(2, 19).clamped(12);
        assert_eq!(selection, Selection::new(2, 12));
        assert_eq!(selection.normalised(), (2, 12));
        // Reversed past the end: the anchor is the clamped end, so the direction survives.
        let reversed = Selection::new(19, 2).clamped(12);
        assert_eq!(reversed.normalised(), (2, 12));
        assert_eq!(reversed.caret_offset(), 2);
    }

    #[test]
    fn copying_an_open_selection_reads_the_buffer_and_faults_nothing() {
        let mut typing = session(64);
        typing.insert(b"hello world").expect("typed");
        let mut residents = residency(&[]);
        // The leaf is returned whole by `spans_in`, so a sub-range must be clipped here.
        let bytes = copy_from(&typing, &mut residents, Selection::new(3, 8)).expect("copied");
        assert_eq!(bytes, b"lo wo");
        assert_eq!(residents.resident_segments(), 0, "no segment was touched");
        let all = copy_from(&typing, &mut residents, Selection::new(0, 11)).expect("copied");
        assert_eq!(all, b"hello world");
        assert_eq!(residents.resident_segments(), 0);
    }

    #[test]
    fn a_sealed_leaf_is_faulted_in_through_the_register() {
        // The bug this test exists for: after a seal, the leaf still names its **provisional** id,
        // because sealing renames the source and not the leaf. Faulting in that id asks the
        // world-line for a segment it has never had.
        let mut typing = session(8);
        typing.insert(b"0123456789abcdefghij").expect("typed");
        typing.flush().expect("flushed");
        let provisional = typing.spine().spans_in(0, 1)[0].slab().segment();
        assert!(
            crate::source::is_provisional(provisional),
            "the leaf keeps naming its source"
        );
        assert_eq!(
            typing.sources().locate(provisional),
            Location::Sealed {
                segment: SegmentId::new(0)
            },
            "and the register is what turns that into a segment"
        );
        let mut residents = residency(&[b"01234567", b"89abcdef", b"ghij"]);
        let bytes = copy_from(&typing, &mut residents, Selection::new(0, 4)).expect("copied");
        assert_eq!(bytes, b"0123");
        assert_eq!(residents.resident_segments(), 1);
    }

    #[test]
    fn copying_across_a_seal_reads_the_sealed_leaf_and_then_the_buffer() {
        // Eight bytes seal into segment 0; two stay open in the session's buffer.
        let mut typing = session(8);
        typing.insert(b"0123456789").expect("typed");
        assert_eq!(typing.pending(), b"89");
        let mut residents = residency(&[b"01234567"]);
        let bytes = copy_from(&typing, &mut residents, Selection::new(6, 10)).expect("copied");
        assert_eq!(bytes, b"6789");
        assert_eq!(
            residents.resident_segments(),
            1,
            "only the sealed leaf was faulted in, not the open one"
        );
        // A selection that lives entirely in the open buffer faults nothing.
        let residents_before = residents.resident_segments();
        let open_only = copy_from(&typing, &mut residents, Selection::new(9, 10)).expect("copied");
        assert_eq!(open_only, b"9");
        assert_eq!(residents.resident_segments(), residents_before);
    }

    #[test]
    fn a_fragment_of_a_leaf_copies_the_fragment_and_not_the_leaf() {
        let mut typing = session(8);
        typing.insert(b"0123456789abcdefghij").expect("typed");
        typing.flush().expect("flushed");
        let mut residents = residency(&[b"01234567", b"89abcdef", b"ghij"]);
        let bytes = copy_from(&typing, &mut residents, Selection::new(3, 5)).expect("copied");
        assert_eq!(bytes, b"34", "two bytes, out of an eight-byte leaf");
        assert_eq!(residents.resident_segments(), 1);
        // A range that spans all three leaves reads all three, in document order.
        let across = copy_from(&typing, &mut residents, Selection::new(4, 18)).expect("copied");
        assert_eq!(across, b"456789abcdefgh");
        assert_eq!(residents.resident_segments(), 3);
    }

    #[test]
    fn a_selection_past_the_end_copies_what_the_document_has() {
        let mut typing = session(64);
        typing.insert(b"abc").expect("typed");
        let mut residents = residency(&[]);
        let bytes = copy_from(&typing, &mut residents, Selection::new(1, 99)).expect("copied");
        assert_eq!(bytes, b"bc");
        let empty = copy_from(&typing, &mut residents, Selection::new(99, 99)).expect("copied");
        assert!(empty.is_empty());
    }

    #[test]
    fn a_draft_that_is_not_the_open_buffer_is_refused_rather_than_guessed_at() {
        // The shape the commit boundary uses: a drafted leaf over sealed geometry, whose live bytes
        // are in the resident set and not in the buffer a session hands to `copy`.
        let slab = Slab::new(SegmentId::new(4), 16, 5);
        let spine = Spine::from_slabs(vec![slab]);
        let mut draft = Draft::new();
        draft
            .record(SlabSpan::new(0, slab), 5)
            .expect("the only draft");
        // A register that knows no provisional source: the draft names sealed geometry, so it is
        // refused before the register is ever consulted about a buffer.
        let sources = OpenSources::new();
        let mut residents = residency(&[]);
        assert_eq!(
            copy(
                &mut residents,
                &spine,
                &draft,
                &sources,
                Selection::new(0, 2),
                b"",
            ),
            Err(CopyError::DraftNotInTheSession {
                segment: SegmentId::new(4),
                start: 16
            })
        );
    }

    #[test]
    fn a_leaf_the_register_never_handed_out_is_reported_not_read() {
        let provisional = SegmentId::new(1 << 63);
        let spine = Spine::from_slabs(vec![Slab::new(provisional, 0, 3)]);
        let mut residents = residency(&[]);
        assert_eq!(
            copy(
                &mut residents,
                &spine,
                &Draft::new(),
                &OpenSources::new(),
                Selection::new(0, 3),
                b"abc",
            ),
            Err(CopyError::UnknownSource {
                segment: provisional
            })
        );
    }

    #[test]
    fn a_leaf_that_claims_more_than_its_segment_is_reported_not_truncated() {
        let mut typing = session(8);
        typing.insert(b"0123456789").expect("typed");
        typing.flush().expect("flushed");
        // The world-line handed back a segment one byte short of what the spine names.
        let mut residents = residency(&[b"0123456", b"89"]);
        assert_eq!(
            copy_from(&typing, &mut residents, Selection::new(0, 8)),
            Err(CopyError::SegmentTooShort {
                segment: SegmentId::new(0),
                needed: 8,
                have: 7
            })
        );
    }
}
