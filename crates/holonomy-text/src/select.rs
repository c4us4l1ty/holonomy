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
//! * a **clipboard** wants the bytes — that is [`copy`], which faults in exactly the leaves the
//!   selection touches and returns exactly the selected bytes.
//!
//! The second is the constitutional clause at clipboard granularity: *never decrypt more than you
//! look at*. Copying two bytes out of the middle of a 64 KiB leaf faults that leaf and returns two
//! bytes; it does not return the leaf, and it does not touch the leaves on either side. What it
//! does *not* decide is anything about the operating system's clipboard — flagged confidential,
//! cleared on blur — which is per-platform work and stays open in AUDIT's OQ-3.
//!
//! A drafted leaf is the one case that needs care: [`Draft::spans_in`] returns a drafted leaf
//! **whole**, because a draft is about a length and not a sub-range, so [`copy`] clips it here. A
//! drafted leaf whose bytes are not the typing session's buffer — a resident slab edited in place
//! before a commit — is refused rather than guessed at, because those bytes live in the resident
//! set and not in the buffer this function is handed.

use crate::draft::{Draft, LiveSpan};
use crate::residency::{ResidencyError, ResidentSet, SegmentSource};
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
        let (start, end) = self
            .clamped(draft.document_len(spine))
            .normalised();
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
        segment: crate::slab::SegmentId,
        /// The end offset the leaf claims.
        needed: usize,
        /// The bytes the segment actually has.
        have: usize,
    },
    /// The open buffer is shorter than the drafted leaf that names it.
    OpenTooShort {
        /// The end offset the leaf claims.
        needed: usize,
        /// The bytes the buffer actually has.
        have: usize,
    },
    /// A drafted leaf whose bytes are not the buffer this function was handed.
    ///
    /// A typing session's draft is the buffer at offset zero; a draft recorded against a resident
    /// slab edited in place is the commit boundary's business, and guessing would copy the wrong
    /// bytes silently.
    DraftNotInTheSession {
        /// The offset within the resident text where the drafted leaf starts.
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
            Self::OpenTooShort { needed, have } => {
                write!(formatter, "the open buffer holds {have} bytes but a leaf claims {needed}")
            }
            Self::DraftNotInTheSession { start } => write!(
                formatter,
                "the drafted leaf starts at {start} within resident text, not in the open buffer"
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
/// `open` is the typing session's pending bytes — the buffer a drafted leaf names. Sealed leaves
/// are faulted through `residency` one at a time and only for the part the selection covers, so
/// the bytes returned are the selection's bytes and no more.
///
/// # Errors
///
/// A fault-in the resident set refuses, a leaf that claims more bytes than its segment or the open
/// buffer holds, or a drafted leaf that is not in the session's buffer.
pub fn copy<R: SegmentSource>(
    residency: &mut ResidentSet<R>,
    spine: &Spine,
    draft: &Draft,
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
        let skip = low - span.offset();
        let take = high - low;
        let from = u64::from(span.slab().start()) + skip;
        let to = from + take;
        let from = usize::try_from(from).expect("a leaf fits in usize");
        let to = usize::try_from(to).expect("a leaf fits in usize");
        if span.is_draft() {
            if span.slab().start() != 0 {
                return Err(CopyError::DraftNotInTheSession {
                    start: span.slab().start(),
                });
            }
            let have = open.len();
            let slice = open
                .get(from..to)
                .ok_or(CopyError::OpenTooShort { needed: to, have })?;
            bytes.extend_from_slice(slice);
        } else {
            let segment = span.slab().segment();
            let leaf = residency.fault_in(segment)?;
            let have = leaf.len();
            let slice = leaf
                .get(from..to)
                .ok_or(CopyError::SegmentTooShort {
                    segment,
                    needed: to,
                    have,
                })?;
            bytes.extend_from_slice(slice);
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slab::{SegmentId, Slab, SlabSpan};
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
                .ok_or_else(|| format!("no segment {segment}"))
        }
    }

    fn residency(segments: &[&[u8]]) -> ResidentSet<Segments> {
        ResidentSet::new(Segments::of(segments), 1024, 64).expect("a workable residency")
    }

    fn session(capacity: usize) -> Typing<Recorder> {
        Typing::new(Recorder::default(), capacity)
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
        // The drafted leaf is returned whole by `spans_in`, so a sub-range must be clipped here.
        let bytes = copy(
            &mut residents,
            typing.spine(),
            typing.draft(),
            Selection::new(3, 8),
            typing.pending(),
        )
        .expect("copied");
        assert_eq!(bytes, b"lo wo");
        assert_eq!(residents.resident_segments(), 0, "no segment was touched");
        // And the whole buffer, for completeness.
        let all = copy(
            &mut residents,
            typing.spine(),
            typing.draft(),
            Selection::new(0, 11),
            typing.pending(),
        )
        .expect("copied");
        assert_eq!(all, b"hello world");
        assert_eq!(residents.resident_segments(), 0);
    }

    #[test]
    fn copying_across_a_seal_reads_the_sealed_leaf_and_then_the_buffer() {
        // Eight bytes seal into segment 0; two stay open in the session's buffer.
        let mut typing = session(8);
        typing.insert(b"0123456789").expect("typed");
        assert_eq!(typing.pending(), b"89");
        let mut residents = residency(&[b"01234567"]);
        let bytes = copy(
            &mut residents,
            typing.spine(),
            typing.draft(),
            Selection::new(6, 10),
            typing.pending(),
        )
        .expect("copied");
        assert_eq!(bytes, b"6789");
        assert_eq!(
            residents.resident_segments(),
            1,
            "only the sealed leaf was faulted in, not the draft"
        );
        // A selection that lives entirely in the open buffer faults nothing.
        let residents_before = residents.resident_segments();
        let open_only = copy(
            &mut residents,
            typing.spine(),
            typing.draft(),
            Selection::new(9, 10),
            typing.pending(),
        )
        .expect("copied");
        assert_eq!(open_only, b"9");
        assert_eq!(residents.resident_segments(), residents_before);
    }

    #[test]
    fn a_fragment_of_a_leaf_copies_the_fragment_and_not_the_leaf() {
        let mut typing = session(8);
        typing.insert(b"0123456789abcdefghij").expect("typed");
        typing.flush().expect("flushed");
        let mut residents = residency(&[b"01234567", b"89abcdef", b"ghij"]);
        let bytes = copy(
            &mut residents,
            typing.spine(),
            typing.draft(),
            Selection::new(3, 5),
            typing.pending(),
        )
        .expect("copied");
        assert_eq!(bytes, b"34", "two bytes, out of an eight-byte leaf");
        assert_eq!(residents.resident_segments(), 1);
        // A range that spans all three leaves reads all three, in document order.
        let across = copy(
            &mut residents,
            typing.spine(),
            typing.draft(),
            Selection::new(4, 18),
            typing.pending(),
        )
        .expect("copied");
        assert_eq!(across, b"456789abcdefgh");
        assert_eq!(residents.resident_segments(), 3);
    }

    #[test]
    fn a_selection_past_the_end_copies_what_the_document_has() {
        let mut typing = session(64);
        typing.insert(b"abc").expect("typed");
        let mut residents = residency(&[]);
        let bytes = copy(
            &mut residents,
            typing.spine(),
            typing.draft(),
            Selection::new(1, 99),
            typing.pending(),
        )
        .expect("copied");
        assert_eq!(bytes, b"bc");
        let empty = copy(
            &mut residents,
            typing.spine(),
            typing.draft(),
            Selection::new(99, 99),
            typing.pending(),
        )
        .expect("copied");
        assert!(empty.is_empty());
    }

    #[test]
    fn a_draft_that_is_not_the_open_buffer_is_refused_rather_than_guessed_at() {
        // The shape the commit boundary uses: a drafted leaf inside resident text, whose bytes are
        // in the resident set and not in the buffer a session hands to `copy`.
        let slab = Slab::new(SegmentId::new(4), 16, 5);
        let spine = Spine::from_slabs(vec![slab]);
        let mut draft = Draft::new();
        draft
            .record(SlabSpan::new(0, slab), 5)
            .expect("the only draft");
        let mut residents = residency(&[]);
        assert_eq!(
            copy(
                &mut residents,
                &spine,
                &draft,
                Selection::new(0, 2),
                b"",
            ),
            Err(CopyError::DraftNotInTheSession { start: 16 })
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
            copy(
                &mut residents,
                typing.spine(),
                typing.draft(),
                Selection::new(0, 8),
                typing.pending(),
            ),
            Err(CopyError::SegmentTooShort {
                segment: SegmentId::new(0),
                needed: 8,
                have: 7
            })
        );
    }
}
