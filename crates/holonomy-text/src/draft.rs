//! The live-length overlay: answering position queries while the spine is stale.
//!
//! D-2 made the spine stale **on purpose**. Updating a leaf's length per character in an
//! immutable, path-copied tree would allocate O(log N) nodes per keystroke, which puts L-2's
//! zero-allocation claim out of reach by construction; so the resident slab is authoritative while
//! it is being edited, and the spine is path-copied at the commit boundary ([`crate::commit`]).
//! The consequence was stated plainly when D-2 was recorded: *the editor must carry a live-length
//! overlay until commit, and that overlay is not built*. This is that overlay.
//!
//! It is a projection, not a document. A [`Draft`] records, for a handful of leaves, the length
//! their resident text currently has; every query maps through the accumulated difference, so the
//! **document view stays correct while the geometry it derives from is out of date**. Nothing here
//! holds plaintext, nothing here is consulted by the spine, and dropping the record returns the
//! view to the committed geometry exactly — which is what makes undo-after-commit sound: the root
//! is unchanged, so the overlay must be too.
//!
//! One property is deliberately *not* symmetric with [`crate::Spine::spans_in`]:
//!
//! * **Sealed leaves are clipped to the requested range** exactly as the spine clips them, so an
//!   empty overlay answers identically to the spine itself.
//! * **A drafted leaf is returned whole**, never clipped to the range. Its visible portion cannot
//!   be expressed as a `Slab` — the bytes exist in no sealed segment until commit — so reporting a
//!   partial extent would hand back a handle that names nothing. The caller clips within the
//!   resident buffer, which it already holds.
//!
//! A query costs O(drafted leaves), not O(document): the overlay only ever partitions the walk
//! into a few regions, and each region is answered by the spine's own O(log N) descent.

use crate::slab::{Slab, SlabSpan};
use crate::spine::Spine;

/// Why a draft could not be recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftError {
    /// The span overlaps a leaf that is already drafted.
    ///
    /// Drafts describe **whole leaves** and never overlap. Editing part of a leaf is still a
    /// record of that whole leaf, because the leaf is the unit the spine names and the unit the
    /// commit boundary replaces.
    Overlaps {
        /// The start of the span that was refused.
        at: u64,
        /// The start of the drafted leaf it collides with.
        with: u64,
    },
}

impl std::fmt::Display for DraftError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overlaps { at, with } => {
                write!(formatter, "the span at {at} overlaps the drafted leaf at {with}")
            }
        }
    }
}

impl std::error::Error for DraftError {}

/// One leaf whose resident text no longer has the length it was sealed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Drafted {
    /// Where the leaf sat when it was recorded, in **sealed** coordinates — which never move: while
    /// the spine is stale only lengths change, so a leaf's start is stable and the live offsets are
    /// derived from it.
    at: u64,
    /// The sealed geometry the resident text came from.
    slab: Slab,
    /// The length the resident text has now.
    len: u64,
}

/// The leaves being edited, with their live lengths.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    edits: Vec<Drafted>,
}

impl Draft {
    /// An empty overlay.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty overlay with room for `capacity` drafted leaves without reallocating.
    ///
    /// The editor sizes this from the viewport, like the resident budget: re-recording a leaf that
    /// is already drafted never allocates, and the number of leaves edited at once is a property of
    /// the session, never of the document.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            edits: Vec::with_capacity(capacity),
        }
    }

    /// How many leaves are drafted.
    #[must_use]
    pub fn len(&self) -> usize {
        self.edits.len()
    }

    /// Whether every leaf still matches its committed length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// The drafted leaves, in document order, as the spans the commit boundary takes.
    pub fn edited(&self) -> impl Iterator<Item = SlabSpan> + '_ {
        self.edits
            .iter()
            .map(|edit| SlabSpan::new(edit.at, edit.slab))
    }

    /// Records that the leaf at `span` now holds `len` bytes.
    ///
    /// `span` must be a leaf of the spine the queries will be made against — the caller has it from
    /// [`Spine::spans_in`] or [`Spine::locate`]. Re-recording a leaf that is already drafted
    /// updates it in place: that is the per-keystroke case, and it allocates nothing.
    ///
    /// # Errors
    ///
    /// [`DraftError::Overlaps`] if the span overlaps a *different* drafted leaf.
    pub fn record(&mut self, span: SlabSpan, len: u64) -> Result<(), DraftError> {
        let at = span.offset();
        let slab = span.slab();
        let end = at.saturating_add(u64::from(slab.len()));
        for &edit in &self.edits {
            if edit.at == at {
                let position = self
                    .edits
                    .iter()
                    .position(|candidate| candidate.at == at)
                    .expect("the entry was just found");
                self.edits[position].slab = slab;
                self.edits[position].len = len;
                return Ok(());
            }
            let edit_end = edit.at.saturating_add(u64::from(edit.slab.len()));
            if at < edit_end && edit.at < end {
                return Err(DraftError::Overlaps { at, with: edit.at });
            }
        }
        // Keep the list in document order: the queries walk it as a partition.
        let position = self
            .edits
            .iter()
            .position(|candidate| candidate.at > at)
            .unwrap_or(self.edits.len());
        self.edits.insert(position, Drafted { at, slab, len });
        Ok(())
    }

    /// Drops the draft at `at`, returning whether there was one.
    ///
    /// This is what the caller does after a seal: the new leaf's committed length *is* the live
    /// length, so keeping the draft would count the same bytes twice.
    pub fn forget(&mut self, at: u64) -> bool {
        if let Some(position) = self.edits.iter().position(|edit| edit.at == at) {
            self.edits.remove(position);
            true
        } else {
            false
        }
    }

    /// Drops every draft.
    pub fn clear(&mut self) {
        self.edits.clear();
    }

    /// The document's length with the drafts applied.
    #[must_use]
    pub fn document_len(&self, spine: &Spine) -> u64 {
        let mut total = spine.len();
        for edit in &self.edits {
            let sealed = u64::from(edit.slab.len());
            if edit.len >= sealed {
                total = total.saturating_add(edit.len - sealed);
            } else {
                total = total.saturating_sub(sealed - edit.len);
            }
        }
        total
    }

    /// The leaves overlapping `start..end`, with live offsets and lengths.
    ///
    /// Sealed leaves are clipped to the range exactly as [`Spine::spans_in`] clips them; a drafted
    /// leaf is returned whole. An empty overlay answers exactly as the spine does.
    #[must_use]
    pub fn spans_in(&self, spine: &Spine, start: u64, end: u64) -> Vec<LiveSpan> {
        let mut out = Vec::new();
        let end = end.min(self.document_len(spine));
        if start >= end {
            return out;
        }
        let mut sealed_at = 0u64;
        let mut live_at = 0u64;
        for edit in &self.edits {
            push_run(spine, sealed_at, edit.at, live_at, start, end, &mut out);
            live_at += edit.at - sealed_at;
            sealed_at = edit.at;
            if edit.len != 0 && start < live_at + edit.len && live_at < end {
                out.push(LiveSpan::Draft {
                    offset: live_at,
                    slab: edit.slab,
                    len: edit.len,
                });
            }
            live_at += edit.len;
            sealed_at += u64::from(edit.slab.len());
        }
        push_run(spine, sealed_at, spine.len(), live_at, start, end, &mut out);
        out
    }

    /// Where `offset` lands in the live document, or `None` past its end.
    #[must_use]
    pub fn locate(&self, spine: &Spine, offset: u64) -> Option<LiveHit> {
        if offset >= self.document_len(spine) {
            return None;
        }
        let mut sealed_at = 0u64;
        let mut live_at = 0u64;
        for edit in &self.edits {
            let run = edit.at - sealed_at;
            if offset < live_at + run {
                return sealed_hit(spine, sealed_at, live_at, offset);
            }
            live_at += run;
            sealed_at = edit.at;
            if offset < live_at + edit.len {
                return Some(LiveHit {
                    span: LiveSpan::Draft {
                        offset: live_at,
                        slab: edit.slab,
                        len: edit.len,
                    },
                    within: offset - live_at,
                });
            }
            live_at += edit.len;
            sealed_at += u64::from(edit.slab.len());
        }
        sealed_hit(spine, sealed_at, live_at, offset)
    }
}

/// Appends the sealed run `sealed_start..sealed_end` (which occupies live `live_start..`) clipped to
/// `start..end`, with every span's offset translated to live coordinates.
fn push_run(
    spine: &Spine,
    sealed_start: u64,
    sealed_end: u64,
    live_start: u64,
    start: u64,
    end: u64,
    out: &mut Vec<LiveSpan>,
) {
    if sealed_end <= sealed_start {
        // A draft recorded past the end of the spine (a caller error) must not underflow here.
        return;
    }
    let live_end = live_start + (sealed_end - sealed_start);
    let low = start.max(live_start);
    let high = end.min(live_end);
    if low >= high {
        return;
    }
    let sealed_low = sealed_start + (low - live_start);
    let sealed_high = sealed_low + (high - low);
    for span in spine.spans_in(sealed_low, sealed_high) {
        out.push(LiveSpan::Sealed {
            offset: live_start + (span.offset() - sealed_start),
            slab: span.slab(),
        });
    }
}

/// Locates `offset` in the sealed run that starts at sealed `sealed_at` / live `live_at`.
fn sealed_hit(spine: &Spine, sealed_at: u64, live_at: u64, offset: u64) -> Option<LiveHit> {
    let sealed_offset = sealed_at + (offset - live_at);
    let hit = spine.locate(sealed_offset)?;
    let leaf_start = sealed_offset - u64::from(hit.within());
    Some(LiveHit {
        span: LiveSpan::Sealed {
            offset: live_at + (leaf_start - sealed_at),
            slab: hit.slab(),
        },
        within: u64::from(hit.within()),
    })
}

/// One leaf of the live document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveSpan {
    /// Committed geometry: the leaf is exactly the slab the spine holds.
    Sealed {
        /// The leaf's live document offset.
        offset: u64,
        /// The sealed slab it names.
        slab: Slab,
    },
    /// A resident leaf whose text no longer has the length it was sealed with.
    Draft {
        /// The leaf's live document offset.
        offset: u64,
        /// The sealed geometry the resident text came from — what the commit boundary takes.
        slab: Slab,
        /// The length the resident text has now.
        len: u64,
    },
}

impl LiveSpan {
    /// The leaf's live document offset.
    #[must_use]
    pub const fn offset(self) -> u64 {
        match self {
            Self::Sealed { offset, .. } | Self::Draft { offset, .. } => offset,
        }
    }

    /// The leaf's live length: the sealed length, or the edited one.
    #[must_use]
    pub fn len(self) -> u64 {
        match self {
            Self::Sealed { slab, .. } => u64::from(slab.len()),
            Self::Draft { len, .. } => len,
        }
    }

    /// The sealed geometry the leaf derives from.
    #[must_use]
    pub const fn slab(self) -> Slab {
        match self {
            Self::Sealed { slab, .. } | Self::Draft { slab, .. } => slab,
        }
    }

    /// Whether the leaf's text is still uncommitted.
    #[must_use]
    pub const fn is_draft(self) -> bool {
        matches!(self, Self::Draft { .. })
    }
}

/// Where an offset lands in the live document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveHit {
    span: LiveSpan,
    within: u64,
}

impl LiveHit {
    /// The leaf the offset landed in.
    #[must_use]
    pub const fn span(self) -> LiveSpan {
        self.span
    }

    /// The offset within the leaf.
    #[must_use]
    pub const fn within(self) -> u64 {
        self.within
    }

    /// The offset within the sealed segment, when the leaf is committed geometry.
    ///
    /// `None` for a drafted leaf: its bytes are in no segment until the commit boundary seals them.
    #[must_use]
    pub fn segment_offset(self) -> Option<u32> {
        match self.span {
            LiveSpan::Sealed { slab, .. } => {
                u32::try_from(u64::from(slab.start()) + self.within).ok()
            }
            LiveSpan::Draft { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slab::SegmentId;

    fn slab(segment: u64, len: u32) -> Slab {
        Slab::new(SegmentId::new(segment), 0, len)
    }

    /// The whole leaf containing `offset`, in sealed coordinates.
    fn span_at(spine: &Spine, offset: u64) -> SlabSpan {
        let hit = spine.locate(offset).expect("in range");
        SlabSpan::new(offset - u64::from(hit.within()), hit.slab())
    }

    /// `(live offset, live length, segment)` for every leaf of the live document.
    fn geometry(spine: &Spine, draft: &Draft) -> Vec<(u64, u64, u64)> {
        draft
            .spans_in(spine, 0, draft.document_len(spine))
            .iter()
            .map(|span| (span.offset(), span.len(), span.slab().segment().get()))
            .collect()
    }

    #[test]
    fn an_empty_overlay_answers_exactly_as_the_spine() {
        let spine = Spine::from_slabs([slab(0, 10), slab(1, 20), slab(2, 30)]);
        let draft = Draft::new();
        assert_eq!(draft.document_len(&spine), 60);
        assert_eq!(
            geometry(&spine, &draft),
            vec![(0, 10, 0), (10, 20, 1), (30, 30, 2)]
        );
        for range in [(0, 60), (0, 1), (9, 11), (10, 30), (31, 59)] {
            let sealed: Vec<(u64, u64)> = spine
                .spans_in(range.0, range.1)
                .iter()
                .map(|span| (span.offset(), span.len()))
                .collect();
            let live: Vec<(u64, u64)> = draft
                .spans_in(&spine, range.0, range.1)
                .iter()
                .map(|span| (span.offset(), span.len()))
                .collect();
            assert_eq!(live, sealed, "range {range:?}");
        }
    }

    #[test]
    fn a_longer_leaf_pushes_everything_after_it_right() {
        let spine = Spine::from_slabs([slab(0, 10), slab(1, 20), slab(2, 30)]);
        let mut draft = Draft::new();
        draft.record(span_at(&spine, 10), 25).expect("records");
        assert_eq!(draft.document_len(&spine), 65);
        assert_eq!(
            geometry(&spine, &draft),
            vec![(0, 10, 0), (10, 25, 1), (35, 30, 2)]
        );
        assert_eq!(draft.locate(&spine, 9).expect("in range").within(), 9);
        let drafted = draft.locate(&spine, 10).expect("in range");
        assert!(drafted.span().is_draft());
        assert_eq!(drafted.within(), 0);
        assert_eq!(drafted.segment_offset(), None);
        let after = draft.locate(&spine, 35).expect("in range");
        assert!(!after.span().is_draft());
        assert_eq!(after.span().slab().segment(), SegmentId::new(2));
        assert_eq!(after.within(), 0);
        assert_eq!(after.segment_offset(), Some(0));
        assert!(draft.locate(&spine, 65).is_none());
    }

    #[test]
    fn a_shorter_leaf_pulls_everything_after_it_left() {
        let spine = Spine::from_slabs([slab(0, 10), slab(1, 20), slab(2, 30)]);
        let mut draft = Draft::new();
        draft.record(span_at(&spine, 10), 5).expect("records");
        assert_eq!(draft.document_len(&spine), 45);
        assert_eq!(
            geometry(&spine, &draft),
            vec![(0, 10, 0), (10, 5, 1), (15, 30, 2)]
        );
        // The tail leaf starts fifteen bytes into the live document, not thirty.
        assert_eq!(draft.locate(&spine, 15).expect("in range").within(), 0);
    }

    #[test]
    fn an_emptied_leaf_occupies_no_space_in_the_live_document() {
        let spine = Spine::from_slabs([slab(0, 10), slab(1, 20), slab(2, 30)]);
        let mut draft = Draft::new();
        draft.record(span_at(&spine, 10), 0).expect("records");
        assert_eq!(draft.document_len(&spine), 40);
        // A leaf with no bytes is not a span.
        assert_eq!(geometry(&spine, &draft), vec![(0, 10, 0), (10, 30, 2)]);
        assert!(!draft.locate(&spine, 10).expect("in range").span().is_draft());
    }

    #[test]
    fn several_drafts_compose_in_document_order() {
        let spine = Spine::from_slabs([slab(0, 10), slab(1, 20), slab(2, 30)]);
        let mut draft = Draft::with_capacity(3);
        draft.record(span_at(&spine, 30), 40).expect("records");
        draft.record(span_at(&spine, 0), 12).expect("records");
        draft.record(span_at(&spine, 10), 8).expect("records");
        assert_eq!(draft.len(), 3);
        assert_eq!(draft.document_len(&spine), 60 + 2 - 12 + 10);
        assert_eq!(
            geometry(&spine, &draft),
            vec![(0, 12, 0), (12, 8, 1), (20, 40, 2)]
        );
        let edited: Vec<u64> = draft.edited().map(|span| span.offset()).collect();
        assert_eq!(edited, vec![0, 10, 30]);
    }

    #[test]
    fn re_recording_updates_in_place_and_never_duplicates() {
        let spine = Spine::from_slabs([slab(0, 10), slab(1, 20)]);
        let mut draft = Draft::new();
        let span = span_at(&spine, 0);
        draft.record(span, 11).expect("records");
        draft.record(span, 12).expect("updates");
        draft.record(span, 13).expect("updates");
        assert_eq!(draft.len(), 1);
        assert_eq!(draft.document_len(&spine), 33);
        assert!(draft.forget(0));
        assert!(draft.is_empty());
        assert!(!draft.forget(0));
        assert_eq!(draft.document_len(&spine), 30);
    }

    #[test]
    fn overlapping_drafts_are_refused() {
        let spine = Spine::from_slabs([slab(0, 10), slab(1, 10)]);
        let mut draft = Draft::new();
        draft.record(span_at(&spine, 0), 12).expect("records");
        // A span beginning inside the drafted leaf: the overlay cannot describe both.
        let overlap = SlabSpan::new(5, Slab::new(SegmentId::new(9), 0, 3));
        assert_eq!(
            draft.record(overlap, 3),
            Err(DraftError::Overlaps { at: 5, with: 0 })
        );
        assert_eq!(draft.len(), 1);
    }

    #[test]
    fn random_documents_tile_and_locate_consistently() {
        use crate::test_support::XorShift;

        for seed in [1u64, 5, 99, 0x00F0_0D] {
            let mut rng = XorShift::new(seed);
            let count = 1 + rng.below(6) as usize;
            let mut slabs: Vec<Slab> = Vec::with_capacity(count);
            for index in 0..count {
                let segment = u64::try_from(index).expect("small");
                slabs.push(slab(segment, 5 + rng.below(20) as u32));
            }
            let spine = Spine::from_slabs(slabs.clone());

            let mut draft = Draft::with_capacity(count);
            let mut model: Vec<u64> = slabs.iter().map(|one| u64::from(one.len())).collect();
            let mut recorded = vec![false; count];
            for index in 0..count {
                if rng.below(2) == 0 {
                    continue;
                }
                let live = rng.below(40);
                let sealed: u64 = slabs[..index].iter().map(|one| u64::from(one.len())).sum();
                draft
                    .record(SlabSpan::new(sealed, slabs[index]), live)
                    .expect("leaves never overlap");
                model[index] = live;
                recorded[index] = true;
            }

            let total: u64 = model.iter().sum();
            assert_eq!(draft.document_len(&spine), total, "seed {seed}");

            // The leaves of the live document tile it contiguously.
            let mut cursor = 0u64;
            for span in draft.spans_in(&spine, 0, total) {
                assert_eq!(span.offset(), cursor, "seed {seed}");
                cursor += span.len();
            }
            assert_eq!(cursor, total, "seed {seed}");

            // Every offset lands in the leaf the model says, at the offset it says.
            for offset in 0..total {
                let hit = draft.locate(&spine, offset).expect("in range");
                let mut base = 0u64;
                let mut found = None;
                for (index, live) in model.iter().enumerate() {
                    if offset < base + live {
                        found = Some((index, base));
                        break;
                    }
                    base += live;
                }
                let (index, leaf_start) = found.expect("the model covers the document");
                assert_eq!(hit.span().offset(), leaf_start, "seed {seed}");
                assert_eq!(hit.within(), offset - leaf_start, "seed {seed}");
                assert_eq!(hit.span().len(), model[index], "seed {seed}");
                if model[index] != 0 {
                    assert_eq!(hit.span().is_draft(), recorded[index], "seed {seed}");
                }
            }
        }
    }
}
