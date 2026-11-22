//! The commit boundary: an edited slab becomes a sealed segment and a new root.
//!
//! This is the module where AUDIT decision **D-2** stops being a description. While a slab is
//! being edited the resident plaintext is authoritative and the spine is stale *by design*;
//! here the edited bytes are appended to the world-line as a **new segment**, the world-line
//! commits it with a witness, and the spine is path-copied to name it. Seal, append, witness,
//! new root — one structural event at the end of a burst of keystrokes, never one per character.
//!
//! **The unit is one slab's document extent, not one segment's.** A re-seal replaces exactly the
//! range one leaf covers, so no offset outside it moves and no neighbour has to be rewritten. A
//! segment that has already been cut into several leaves is committed leaf by leaf, and each
//! commit seals that leaf's bytes as a fresh segment. The alternative — rewriting every leaf that
//! names an edited segment — would have to shift offsets after the edit point in every one of
//! them, which is the offset arithmetic this design exists to avoid.
//!
//! **The fence comes before the append.** A span that no longer names the leaf at its document
//! offset is refused *before* the world-line is touched, because an append with no matching spine
//! edit is an orphan segment: durable, authenticated and unreferenced. Undoing a commit and
//! re-committing the stale span is exactly how that would happen in a real editor.
//!
//! **Nothing here is measured, and nothing here claims L-2.** Commitment allocates — path copies,
//! the segment record, the witness — and it is supposed to: L-2's zero-allocation claim is about
//! the keystroke path *between* commitments, which `tests/keystroke_allocations.rs` measures with
//! a counting allocator. The engine therefore does not claim that editing is allocation-free; it
//! claims that *typing* is, and that the cost of sealing is paid in one place.

use crate::slab::{SegmentId, Slab, SlabHit, SlabSpan};
use crate::spine::{Spine, SpineError};
use crate::text::SlabText;

/// What the world-line reports after committing one appended segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appended {
    /// The identifier the world-line assigned to the new segment.
    pub segment: SegmentId,
    /// The witness sequence number that commits it.
    pub witness_sequence: u64,
    /// The authenticated root over all committed segment records after the append.
    pub segment_root: [u8; 32],
    /// Bytes physically written for the segment record and its witness.
    pub bytes_written: u64,
}

/// The world-line, as the commit boundary needs it: append one sealed segment and commit it.
///
/// A trait rather than a concrete type for the same reason [`crate::SegmentSource`] is one: the
/// spine must not depend on the container format. In production this is `holonomy-line`'s
/// `WorldLine`, and `tests/commit_boundary.rs` proves the seam against the real one.
pub trait SegmentSink {
    /// Seals `plaintext` as one new segment and commits it with a witness.
    ///
    /// # Errors
    ///
    /// A human-readable message when the world-line refuses the append — an empty or oversized
    /// segment, an unsupported durability barrier, or an I/O failure. The commit boundary wraps it
    /// in [`CommitError::Refused`] and leaves both the spine and the world-line as they were.
    fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String>;

    /// Seals `plaintext` as one new segment alongside the resolved spine slabs around it
    /// (`before` precedes the new segment's full span; `after` follows it).
    ///
    /// Sinks backed by a world-line commit both the new segment and any non-implicit slab
    /// manifest under a single witness; pure segment-log test doubles keep the default, which
    /// delegates to [`Self::append_segment`].
    ///
    /// # Errors
    ///
    /// A human-readable message when the world-line refuses the append or its geometry manifest.
    fn append_segment_with_spine(
        &mut self,
        plaintext: &[u8],
        _before: &[Slab],
        _after: &[Slab],
    ) -> Result<Appended, String> {
        self.append_segment(plaintext)
    }

    /// Commits `slabs` as the active document geometry when no new segment bytes are being
    /// appended — for example after a deletion across sealed text, or an undo of one (OQ-29,
    /// OQ-30).
    ///
    /// # Errors
    ///
    /// A human-readable message when the sink cannot persist the spine geometry.
    fn commit_spine(&mut self, _slabs: &[Slab]) -> Result<(), String> {
        Ok(())
    }
}

/// The witness that commits a re-sealed slab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Witness {
    /// The new segment naming the edited bytes.
    pub segment: SegmentId,
    /// The witness sequence number.
    pub sequence: u64,
    /// The authenticated segment root after the append.
    pub segment_root: [u8; 32],
    /// Bytes physically written for the segment record and the witness.
    pub bytes_written: u64,
}

/// What a commit did to the spine.
#[derive(Debug, Clone)]
pub enum Committed {
    /// The edited slab was sealed as a new segment, and the spine now names it.
    Sealed {
        /// The new root. The old root still describes the previous document.
        spine: Spine,
        /// The witness that commits the new segment.
        witness: Witness,
    },
    /// The edit emptied the slab, so the commit only removed it. Nothing was appended, so there is
    /// no witness: a zero-byte segment would be a leaf that carries nothing.
    Removed {
        /// The new root, without the emptied slab.
        spine: Spine,
    },
}

impl Committed {
    /// The new root.
    #[must_use]
    pub fn spine(&self) -> &Spine {
        match self {
            Self::Sealed { spine, .. } | Self::Removed { spine } => spine,
        }
    }

    /// The witness, when the commit sealed a segment.
    #[must_use]
    pub fn witness(&self) -> Option<&Witness> {
        match self {
            Self::Sealed { witness, .. } => Some(witness),
            Self::Removed { .. } => None,
        }
    }
}

/// Why a commit was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitError {
    /// The span no longer names the leaf at its document offset — the spine moved under it.
    ///
    /// Nothing was appended: this is checked before the world-line is touched.
    StaleSpan {
        /// The document offset the commit would have replaced.
        offset: u64,
        /// The slab the caller says lives there.
        expected: Slab,
        /// The slab that is actually there, if any.
        found: Option<Slab>,
    },
    /// The edited text cannot be one segment: it does not fit a `u32` slab range.
    TooLarge {
        /// The size of the edited slab in bytes.
        bytes: usize,
    },
    /// The world-line refused the append. Nothing was appended and the spine is unchanged.
    Refused {
        /// The world-line's own message.
        message: String,
    },
    /// The structural edit failed. Unreachable after a successful append, since the range was
    /// validated against the same spine before the append; it exists so the type stays total.
    Spine(SpineError),
}

impl std::fmt::Display for CommitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleSpan {
                offset,
                expected,
                found,
            } => match found {
                Some(slab) => write!(
                    formatter,
                    "offset {offset} holds segment {} bytes {}..{}, not segment {} bytes {}..{}",
                    slab.segment(),
                    slab.start(),
                    slab.end(),
                    expected.segment(),
                    expected.start(),
                    expected.end()
                ),
                None => write!(
                    formatter,
                    "offset {offset} is past the end of the document; the span named segment {}",
                    expected.segment()
                ),
            },
            Self::TooLarge { bytes } => write!(
                formatter,
                "a {bytes}-byte slab cannot be one segment; split it before committing"
            ),
            Self::Refused { message } => write!(formatter, "the world-line refused: {message}"),
            Self::Spine(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for CommitError {}

impl From<SpineError> for CommitError {
    fn from(error: SpineError) -> Self {
        Self::Spine(error)
    }
}

/// Re-seals one edited slab and returns the new root.
///
/// `span` is where the slab sits in the document and which slab it is — exactly what
/// [`Spine::spans_in`] hands the viewport. `text` is that slab's resident plaintext after editing;
/// it is sealed as a new segment and the span's extent is replaced by one leaf naming it.
///
/// An emptied `text` removes the span instead of appending: a segment of no bytes is a leaf that
/// carries nothing (and the container refuses empty segments).
///
/// # Errors
///
/// [`CommitError::StaleSpan`] when the spine no longer has that leaf where the caller says it is
/// — checked before the append, so a stale commit cannot append an orphan segment;
/// [`CommitError::TooLarge`] when the text cannot be one segment; [`CommitError::Refused`] when the
/// world-line declines.
pub fn commit_slab<S: SegmentSink>(
    spine: &Spine,
    span: SlabSpan,
    text: &SlabText,
    sink: &mut S,
) -> Result<Committed, CommitError> {
    let slab = span.slab();
    let offset = span.offset();
    let end = offset.saturating_add(u64::from(slab.len()));

    // Fence first, and it checks two things `locate` alone would not. The leaf containing `offset`
    // must be this slab — and `offset` must be that leaf's **first byte**. Without the alignment
    // check, a span starting mid-leaf could name the right slab, pass the identity test, and then
    // delete `len` bytes from inside it: the leaf's tail plus however much of its neighbour the
    // extent reaches into. A silent hole in the document, so alignment is part of the fence.
    let found = spine.locate(offset);
    let aligned = found.is_some_and(|hit| hit.within() == 0 && hit.slab() == slab);
    if !aligned {
        return Err(CommitError::StaleSpan {
            offset,
            expected: slab,
            found: found.map(SlabHit::slab),
        });
    }
    if text.is_empty() {
        return Ok(Committed::Removed {
            spine: spine.remove_range(offset, end)?,
        });
    }

    let len = u32::try_from(text.len()).map_err(|_| CommitError::TooLarge { bytes: text.len() })?;
    let appended = sink
        .append_segment(text.as_bytes())
        .map_err(|message| CommitError::Refused { message })?;
    let sealed = spine.replace_range(offset, end, Slab::new(appended.segment, 0, len))?;
    Ok(Committed::Sealed {
        spine: sealed,
        witness: Witness {
            segment: appended.segment,
            sequence: appended.witness_sequence,
            segment_root: appended.segment_root,
            bytes_written: appended.bytes_written,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in world-line: assigns segment ids from `first`, records what it was given.
    struct Recording {
        first: u64,
        appended: Vec<Vec<u8>>,
        refuse: Option<&'static str>,
    }

    impl Recording {
        fn new(first: u64) -> Self {
            Self {
                first,
                appended: Vec::new(),
                refuse: None,
            }
        }
    }

    impl SegmentSink for Recording {
        fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
            if let Some(message) = self.refuse {
                return Err(message.to_owned());
            }
            self.appended.push(plaintext.to_vec());
            let index = u64::try_from(self.appended.len()).expect("a handful of segments");
            let segment = SegmentId::new(self.first + index - 1);
            Ok(Appended {
                segment,
                witness_sequence: index,
                segment_root: [0xAB; 32],
                bytes_written: u64::try_from(plaintext.len()).expect("small"),
            })
        }
    }

    fn slab(segment: u64, len: u32) -> Slab {
        Slab::new(SegmentId::new(segment), 0, len)
    }

    fn span_of(spine: &Spine, index: usize) -> SlabSpan {
        let spans = spine.spans_in(0, spine.len());
        spans[index]
    }

    #[test]
    fn a_commit_replaces_the_leaf_it_names_and_leaves_its_neighbour_alone() {
        let spine = Spine::from_slabs([slab(0, 5), slab(1, 5)]);
        let text = SlabText::from_text("hello!", 8);
        let mut sink = Recording::new(10);

        let span = span_of(&spine, 0);
        let committed = commit_slab(&spine, span, &text, &mut sink).expect("commits");
        let geometry: Vec<(u64, u64, u32, u32)> = committed
            .spine()
            .spans_in(0, committed.spine().len())
            .iter()
            .map(|span| {
                let slab = span.slab();
                (
                    span.offset(),
                    slab.segment().get(),
                    slab.start(),
                    slab.len(),
                )
            })
            .collect();
        assert_eq!(geometry, vec![(0, 10, 0, 6), (6, 1, 0, 5)]);
        assert_eq!(committed.spine().len(), 11);
        assert_eq!(
            committed.witness().map(|witness| witness.segment),
            Some(SegmentId::new(10))
        );
        assert_eq!(sink.appended, vec![b"hello!".to_vec()]);
    }

    #[test]
    fn a_stale_span_is_refused_before_anything_is_appended() {
        let spine = Spine::from_slabs([slab(0, 5)]);
        let span = span_of(&spine, 0);
        let text = SlabText::from_text("hello!", 8);
        let mut sink = Recording::new(10);

        let committed = commit_slab(&spine, span, &text, &mut sink).expect("commits");
        let after = committed.spine().clone();
        // Undoing the commit puts the old geometry back; committing the old span against the
        // *new* root names a leaf that is no longer there.
        let refused = commit_slab(&after, span, &text, &mut sink);
        assert_eq!(
            refused.unwrap_err(),
            CommitError::StaleSpan {
                offset: 0,
                expected: slab(0, 5),
                found: Some(slab(10, 6)),
            }
        );
        assert_eq!(sink.appended.len(), 1, "a refusal must not append");

        // The old root still describes the document the span was cut from, so the same span
        // commits there — and against the new root it is only stale, not destructive.
        assert!(commit_slab(&spine, span, &text, &mut sink).is_ok());
        assert_eq!(after.len(), 6);
    }

    #[test]
    fn an_emptied_slab_is_removed_and_nothing_is_appended() {
        let spine = Spine::from_slabs([slab(0, 5), slab(1, 5)]);
        let mut text = SlabText::from_text("hello", 8);
        text.delete(0, 5).expect("covers the slab");
        let mut sink = Recording::new(10);

        let span = span_of(&spine, 0);
        let committed = commit_slab(&spine, span, &text, &mut sink).expect("commits");
        assert!(matches!(committed, Committed::Removed { .. }));
        assert_eq!(committed.spine().len(), 5);
        assert_eq!(committed.spine().slab_count(), 1);
        assert_eq!(committed.witness(), None);
        assert!(sink.appended.is_empty());
        assert_eq!(
            committed.spine().locate(0).map(|hit| hit.slab().segment()),
            Some(SegmentId::new(1))
        );
    }

    #[test]
    fn a_refused_append_leaves_the_spine_unchanged() {
        let spine = Spine::from_slabs([slab(0, 5)]);
        let text = SlabText::from_text("hello!", 8);
        let mut sink = Recording::new(10);
        sink.refuse = Some("no space left on device");

        let span = span_of(&spine, 0);
        assert_eq!(
            commit_slab(&spine, span, &text, &mut sink).unwrap_err(),
            CommitError::Refused {
                message: "no space left on device".to_owned(),
            }
        );
        assert_eq!(spine.len(), 5);
        assert_eq!(spine.slab_count(), 1);
    }

    #[test]
    fn a_span_that_starts_inside_a_leaf_is_refused() {
        // The span names the right slab but starts three bytes into it. Committing it would
        // replace `3..8` of a five-byte document: the leaf's tail and two bytes of its neighbour.
        let spine = Spine::from_slabs([slab(0, 5), slab(1, 5)]);
        let text = SlabText::from_text("tail!", 8);
        let mut sink = Recording::new(10);
        let inside = SlabSpan::new(3, slab(0, 5));
        assert_eq!(
            commit_slab(&spine, inside, &text, &mut sink).unwrap_err(),
            CommitError::StaleSpan {
                offset: 3,
                expected: slab(0, 5),
                found: Some(slab(0, 5)),
            }
        );
        assert!(sink.appended.is_empty(), "nothing may be appended");
        assert_eq!(spine.len(), 10, "and nothing may be removed");
    }

    #[test]
    fn a_span_that_does_not_match_the_leaf_is_refused() {
        let spine = Spine::from_slabs([slab(0, 5)]);
        let text = SlabText::from_text("hello!", 8);
        let mut sink = Recording::new(10);
        // Same offset, a different claimed extent: the leaf is what the spine holds, not the span.
        let overlong = SlabSpan::new(0, Slab::new(SegmentId::new(0), 0, 9));
        assert_eq!(
            commit_slab(&spine, overlong, &text, &mut sink).unwrap_err(),
            CommitError::StaleSpan {
                offset: 0,
                expected: Slab::new(SegmentId::new(0), 0, 9),
                found: Some(slab(0, 5)),
            }
        );
        assert!(sink.appended.is_empty());
    }
}
