//! The resident set: the only place document plaintext is allowed to exist.
//!
//! [`ResidentSet`] holds the decrypted bytes of a bounded number of segments and nothing else.
//! Its capacity is a byte budget fixed at construction from the viewport, **never** derived from
//! the document's length (L-2): the same set holds four 64 KiB segments whether the spine
//! describes one page or thirty million words.
//!
//! Every buffer is a [`Zeroizing<Vec<u8>>`], and eviction drops it on the spot, so a segment that
//! leaves the viewport is wiped rather than merely forgotten. That is by construction, not by
//! measurement: proving the wipe needs the Block 9 canary and the counting allocator, and this
//! module claims neither. What it does claim is that the *set of bytes it owns* never exceeds the
//! budget, and tests assert exactly that.
//!
//! The source of plaintext is a trait so the Spine does not depend on the world-line: in
//! production it is `holonomy-line`'s [`WorldLine`] opening one sealed segment at a time.

use std::collections::VecDeque;

use zeroize::Zeroizing;

use crate::slab::SegmentId;

/// A supplier of sealed-segment plaintext — the world-line, in production.
pub trait SegmentSource {
    /// Faults `segment` in and returns its plaintext bytes.
    ///
    /// Returning the plaintext as an owned buffer is deliberate: the caller decides where it
    /// lives, and the [`ResidentSet`] takes ownership into a zeroizing guard immediately.
    ///
    /// # Errors
    ///
    /// Returns a human-readable message when the segment cannot be opened (absent, unauthenticated,
    /// or I/O failure). The resident set wraps it in [`ResidencyError::Source`].
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String>;
}

/// Why a fault-in failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResidencyError {
    /// A zero-byte budget can never hold a segment.
    ZeroBudget,
    /// One segment is larger than the whole budget, so admitting it would break the bound.
    SegmentExceedsBudget {
        /// The segment that was refused.
        segment: SegmentId,
        /// Its plaintext length.
        bytes: usize,
        /// The configured budget.
        budget: usize,
    },
    /// The source could not supply the segment.
    Source {
        /// The segment that was requested.
        segment: SegmentId,
        /// The source's message.
        message: String,
    },
}

impl std::fmt::Display for ResidencyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroBudget => formatter.write_str("the resident set needs a non-zero budget"),
            Self::SegmentExceedsBudget {
                segment,
                bytes,
                budget,
            } => write!(
                formatter,
                "{segment} is {bytes} bytes, which exceeds the {budget}-byte resident budget"
            ),
            Self::Source { segment, message } => {
                write!(formatter, "cannot fault in {segment}: {message}")
            }
        }
    }
}

impl std::error::Error for ResidencyError {}

struct Entry {
    segment: SegmentId,
    plaintext: Zeroizing<Vec<u8>>,
}

/// A hard-bounded, least-recently-used set of decrypted segments.
pub struct ResidentSet<S: SegmentSource> {
    source: S,
    budget_bytes: usize,
    /// Least recently used at the front, most recent at the back.
    entries: VecDeque<Entry>,
}

impl<S: SegmentSource> ResidentSet<S> {
    /// Creates a resident set with a fixed byte budget.
    ///
    /// # Errors
    ///
    /// [`ResidencyError::ZeroBudget`] if `budget_bytes` is zero.
    pub fn new(source: S, budget_bytes: usize) -> Result<Self, ResidencyError> {
        if budget_bytes == 0 {
            return Err(ResidencyError::ZeroBudget);
        }
        Ok(Self {
            source,
            budget_bytes,
            entries: VecDeque::new(),
        })
    }

    /// The configured budget in bytes.
    #[must_use]
    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    /// Bytes of plaintext currently held. Never exceeds [`Self::budget_bytes`].
    #[must_use]
    pub fn resident_bytes(&self) -> usize {
        self.entries.iter().map(|entry| entry.plaintext.len()).sum()
    }

    /// How many segments are currently resident.
    #[must_use]
    pub fn resident_segments(&self) -> usize {
        self.entries.len()
    }

    /// Whether `segment` is currently resident.
    #[must_use]
    pub fn contains(&self, segment: SegmentId) -> bool {
        self.entries.iter().any(|entry| entry.segment == segment)
    }

    /// The source, for callers that need to report on it or drive it directly.
    #[must_use]
    pub fn source(&self) -> &S {
        &self.source
    }

    /// The source, mutably.
    pub fn source_mut(&mut self) -> &mut S {
        &mut self.source
    }

    /// Faults `segment` in and returns its plaintext, evicting the coldest segments as needed.
    ///
    /// A hit refreshes recency and touches no source. A miss loads the segment, then evicts from
    /// the front of the LRU list until the new segment fits.
    ///
    /// # Errors
    ///
    /// [`ResidencyError::SegmentExceedsBudget`] if the segment alone is larger than the budget
    /// (refusing it keeps the bound true), or [`ResidencyError::Source`] if the load failed.
    pub fn fault_in(&mut self, segment: SegmentId) -> Result<&[u8], ResidencyError> {
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.segment == segment)
        {
            let entry = self
                .entries
                .remove(index)
                .expect("the index came from a successful position search");
            self.entries.push_back(entry);
            return Ok(self
                .entries
                .back()
                .expect("an entry was just pushed")
                .plaintext
                .as_slice());
        }

        let plaintext = self
            .source
            .load(segment)
            .map_err(|message| ResidencyError::Source { segment, message })?;
        if plaintext.len() > self.budget_bytes {
            return Err(ResidencyError::SegmentExceedsBudget {
                segment,
                bytes: plaintext.len(),
                budget: self.budget_bytes,
            });
        }
        while self.resident_bytes() + plaintext.len() > self.budget_bytes {
            let evicted = self
                .entries
                .pop_front()
                .expect("the budget exceeds zero, so the set is not empty here");
            drop(evicted);
        }
        self.entries.push_back(Entry {
            segment,
            plaintext: Zeroizing::new(plaintext),
        });
        Ok(self
            .entries
            .back()
            .expect("an entry was just pushed")
            .plaintext
            .as_slice())
    }

    /// Evicts every resident segment, returning how many were wiped.
    pub fn evict_all(&mut self) -> usize {
        let count = self.entries.len();
        self.entries.clear();
        count
    }

    /// Keeps only the listed segments resident, returning how many were evicted.
    ///
    /// This is what a viewport move calls: pages that left the neighborhood are wiped before the
    /// next neighborhood is faulted in.
    pub fn retain(&mut self, keep: &[SegmentId]) -> usize {
        let before = self.entries.len();
        self.entries.retain(|entry| keep.contains(&entry.segment));
        before - self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slab::{SegmentId, Slab};
    use crate::Spine;

    /// A fake world-line that records load counts and never allocates more than asked.
    struct Counting {
        loads: usize,
        segment_bytes: usize,
        fail_on: Option<SegmentId>,
    }

    impl Counting {
        fn new(segment_bytes: usize) -> Self {
            Self {
                loads: 0,
                segment_bytes,
                fail_on: None,
            }
        }
    }

    impl SegmentSource for Counting {
        fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
            if self.fail_on == Some(segment) {
                return Err("unauthenticated segment".to_owned());
            }
            self.loads += 1;
            let byte = u8::try_from(segment.get() % 251).expect("a remainder below 251 fits");
            Ok(vec![byte; self.segment_bytes])
        }
    }

    fn id(value: u64) -> SegmentId {
        SegmentId::new(value)
    }

    fn set(budget: usize) -> ResidentSet<Counting> {
        ResidentSet::new(Counting::new(64), budget).expect("non-zero budget")
    }

    #[test]
    fn a_fault_in_returns_the_segments_bytes() {
        let mut resident = set(256);
        let bytes = resident.fault_in(id(1)).expect("source works");
        assert_eq!(bytes.len(), 64);
        assert_eq!(bytes[0], 1);
        assert_eq!(resident.resident_bytes(), 64);
        assert_eq!(resident.resident_segments(), 1);
        assert!(resident.contains(id(1)));
        assert_eq!(resident.source().loads, 1);
    }

    #[test]
    fn a_hit_does_not_touch_the_source() {
        let mut resident = set(256);
        resident.fault_in(id(1)).expect("source works");
        resident.fault_in(id(2)).expect("source works");
        let loads_before = resident.source().loads;
        resident.fault_in(id(1)).expect("still cached");
        assert_eq!(
            resident.source().loads,
            loads_before,
            "a hit must not reload"
        );
        assert_eq!(resident.resident_segments(), 2);
    }

    #[test]
    fn the_budget_holds_and_the_coldest_segment_leaves_first() {
        let mut resident = set(256);
        resident.fault_in(id(0)).expect("source works");
        resident.fault_in(id(1)).expect("source works");
        resident.fault_in(id(2)).expect("source works");
        resident.fault_in(id(3)).expect("source works");
        assert_eq!(resident.resident_bytes(), 256);
        assert!(
            !resident.contains(id(0)),
            "the oldest segment must be evicted"
        );

        resident.fault_in(id(4)).expect("source works");
        assert!(!resident.contains(id(1)));
        assert!(resident.contains(id(4)));
        assert!(resident.resident_bytes() <= resident.budget_bytes());
    }

    #[test]
    fn a_hit_refreshes_recency() {
        let mut resident = set(192);
        resident.fault_in(id(0)).expect("source works");
        resident.fault_in(id(1)).expect("source works");
        resident.fault_in(id(2)).expect("source works");
        // Touch 0, making 1 the coldest.
        resident.fault_in(id(0)).expect("still cached");
        resident.fault_in(id(3)).expect("source works");
        assert!(resident.contains(id(0)), "recently used segment survived");
        assert!(!resident.contains(id(1)), "coldest segment was evicted");
    }

    #[test]
    fn a_segment_larger_than_the_budget_is_refused_rather_than_admitted() {
        let mut resident = ResidentSet::new(Counting::new(128), 64).expect("non-zero budget");
        assert_eq!(
            resident.fault_in(id(0)),
            Err(ResidencyError::SegmentExceedsBudget {
                segment: id(0),
                bytes: 128,
                budget: 64
            })
        );
        assert_eq!(resident.resident_bytes(), 0);
    }

    #[test]
    fn a_zero_budget_is_refused() {
        assert_eq!(
            ResidentSet::new(Counting::new(64), 0).map(|_| ()),
            Err(ResidencyError::ZeroBudget)
        );
    }

    #[test]
    fn source_failures_name_the_segment() {
        let mut resident = set(256);
        resident.source_mut().fail_on = Some(id(9));
        match resident.fault_in(id(9)) {
            Err(ResidencyError::Source { segment, message }) => {
                assert_eq!(segment, id(9));
                assert!(message.contains("unauthenticated"));
            }
            other => panic!("expected a source error, got {other:?}"),
        }
        assert_eq!(resident.resident_bytes(), 0);
    }

    #[test]
    fn eviction_is_explicit_and_total() {
        let mut resident = set(256);
        resident.fault_in(id(0)).expect("source works");
        resident.fault_in(id(1)).expect("source works");
        assert_eq!(resident.evict_all(), 2);
        assert_eq!(resident.resident_bytes(), 0);
        assert_eq!(resident.resident_segments(), 0);
    }

    #[test]
    fn retain_wipes_everything_outside_the_new_viewport() {
        let mut resident = set(256);
        for segment in 0..4 {
            resident.fault_in(id(segment)).expect("source works");
        }
        assert_eq!(resident.retain(&[id(2), id(3)]), 2);
        assert_eq!(resident.resident_segments(), 2);
        assert!(resident.contains(id(2)) && resident.contains(id(3)));
        assert!(!resident.contains(id(0)) && !resident.contains(id(1)));
    }

    #[test]
    fn residency_is_independent_of_document_length() {
        // The lifetime document as geometry: 916 slabs, 60,000,000 bytes.
        let slabs: Vec<Slab> = (0..915)
            .map(|segment| Slab::new(id(segment), 0, 65_536))
            .chain(std::iter::once(Slab::new(id(915), 0, 34_560)))
            .collect();
        let spine = Spine::from_slabs(slabs);
        assert_eq!(spine.len(), 60_000_000);

        let budget = 4 * 65_536;
        let mut resident =
            ResidentSet::new(Counting::new(65_536), budget).expect("non-zero budget");

        // Scroll through the whole lifetime a window at a time; the resident bound never moves,
        // and what is resident is always a consequence of the window, not of the document.
        let mut offset = 0u64;
        while offset < spine.len() {
            let end = (offset + 8_192).min(spine.len());
            let window = spine.spans_in(offset, end);
            let keep: Vec<SegmentId> = window.iter().map(|span| span.slab().segment()).collect();
            resident.retain(&keep);
            for segment in keep {
                resident.fault_in(segment).expect("source works");
                assert!(
                    resident.resident_bytes() <= budget,
                    "resident bytes broke the budget at offset {offset}"
                );
                assert!(resident.resident_segments() <= 4);
            }
            offset = end;
        }
        assert_eq!(
            resident.source().loads,
            916,
            "each segment faulted exactly once"
        );
        assert!(resident.resident_bytes() <= budget);

        // The same budget over a two-slab document: the set does not know the difference.
        let small = Spine::from_slabs(vec![Slab::new(id(0), 0, 65_536)]);
        let mut small_resident =
            ResidentSet::new(Counting::new(65_536), budget).expect("non-zero budget");
        small_resident
            .fault_in(small.locate(0).expect("in range").slab().segment())
            .expect("source works");
        assert_eq!(small_resident.resident_bytes(), 65_536);
        assert_eq!(small_resident.budget_bytes(), resident.budget_bytes());
    }
}
