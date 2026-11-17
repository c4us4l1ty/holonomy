//! Slabs: the leaves of the Spine.
//!
//! A slab is a byte range of **one sealed segment's plaintext**. It holds no plaintext itself —
//! only a [`SegmentId`], a start offset and a length — so a whole lifetime document can be
//! described by a few hundred thousand of these without a single readable byte in memory. That is
//! the point: the spine is geometry, and plaintext exists only in the [`crate::ResidentSet`].
//!
//! A segment's bytes may be split across several slabs once edits have cut it; therefore a slab
//! carries `start` as well as `len`, and two slabs from the same segment are not an error.

use core::fmt;

/// Identifier of a sealed segment in the world-line (its global append sequence).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentId(u64);

impl SegmentId {
    /// Creates an identifier from the segment's position in the world-line.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the raw sequence number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SegmentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "segment {}", self.0)
    }
}

/// One contiguous run of plaintext inside a sealed segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slab {
    segment: SegmentId,
    start: u32,
    len: u32,
}

impl Slab {
    /// Creates a slab naming `len` bytes from `start` inside `segment`.
    #[must_use]
    pub const fn new(segment: SegmentId, start: u32, len: u32) -> Self {
        Self {
            segment,
            start,
            len,
        }
    }

    /// The segment this slab reads from.
    #[must_use]
    pub const fn segment(self) -> SegmentId {
        self.segment
    }

    /// The first byte of `segment`'s plaintext covered by this slab.
    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }

    /// The number of bytes covered.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.len
    }

    /// The first byte of `segment`'s plaintext after this slab.
    #[must_use]
    pub const fn end(self) -> u32 {
        self.start + self.len
    }

    /// Whether this slab covers no bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Returns the sub-range `[offset, offset + len)` of this slab, or `None` if it does not fit.
    #[must_use]
    pub fn slice(self, offset: u32, len: u32) -> Option<Self> {
        let end = offset.checked_add(len)?;
        if end > self.len {
            return None;
        }
        Some(Self {
            segment: self.segment,
            start: self.start + offset,
            len,
        })
    }

    /// Returns the sub-range `[offset, offset + len)`, assuming the caller has already checked
    /// that it fits (all internal callers derive it from the slab's own length).
    pub(crate) fn slice_clamped(self, offset: u32, len: u32) -> Self {
        debug_assert!(offset.checked_add(len).is_some_and(|end| end <= self.len));
        Self {
            segment: self.segment,
            start: self.start + offset,
            len,
        }
    }
}

/// A slab together with the document offset where its first byte lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlabSpan {
    offset: u64,
    slab: Slab,
}

impl SlabSpan {
    /// Pairs a document offset with the slab that starts there.
    #[must_use]
    pub const fn new(offset: u64, slab: Slab) -> Self {
        Self { offset, slab }
    }

    /// The document offset of the slab's first byte.
    #[must_use]
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// The slab itself.
    #[must_use]
    pub const fn slab(self) -> Slab {
        self.slab
    }

    /// The number of bytes covered.
    #[must_use]
    pub const fn len(self) -> u64 {
        self.slab.len() as u64
    }

    /// Whether this span covers no bytes.
    ///
    /// A span produced by [`crate::Spine::spans_in`] is never empty; this exists so the span's
    /// length obeys the same shape as every other length in the crate.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.slab.is_empty()
    }
}

/// Where a document offset lands: the slab containing it, and the offset within that slab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlabHit {
    slab: Slab,
    within: u32,
}

impl SlabHit {
    /// Records a hit on `slab` at `within` bytes into it.
    #[must_use]
    pub const fn new(slab: Slab, within: u32) -> Self {
        Self { slab, within }
    }

    /// The slab containing the position.
    #[must_use]
    pub const fn slab(self) -> Slab {
        self.slab
    }

    /// The offset within the slab (not within the segment).
    #[must_use]
    pub const fn within(self) -> u32 {
        self.within
    }

    /// The offset within the **segment's** plaintext, which is what the resident set needs.
    #[must_use]
    pub const fn segment_offset(self) -> u32 {
        self.slab.start() + self.within
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: u64) -> SegmentId {
        SegmentId::new(value)
    }

    #[test]
    fn slicing_moves_the_segment_offset_not_the_identity() {
        let slab = Slab::new(id(7), 100, 64);
        let head = slab.slice(0, 16).expect("fits");
        let tail = slab.slice(16, 48).expect("fits");
        assert_eq!(head.segment(), id(7));
        assert_eq!(head.start(), 100);
        assert_eq!(head.len(), 16);
        assert_eq!(tail.start(), 116);
        assert_eq!(tail.end(), 164);
        assert_eq!(slab.end(), 164);
    }

    #[test]
    fn slicing_past_the_end_is_refused() {
        let slab = Slab::new(id(0), 0, 10);
        assert!(slab.slice(0, 11).is_none());
        assert!(slab.slice(11, 0).is_none());
        assert!(slab.slice(10, 0).is_some());
        assert!(slab.slice(u32::MAX, 1).is_none());
    }

    #[test]
    fn a_hit_translates_to_a_segment_offset() {
        let slab = Slab::new(id(3), 500, 64);
        let hit = SlabHit::new(slab, 12);
        assert_eq!(hit.within(), 12);
        assert_eq!(hit.segment_offset(), 512);
    }

    #[test]
    fn spans_report_document_length() {
        let span = SlabSpan::new(1_000, Slab::new(id(1), 0, 4_096));
        assert_eq!(span.offset(), 1_000);
        assert_eq!(span.len(), 4_096);
    }
}
