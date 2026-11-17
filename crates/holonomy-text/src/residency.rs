//! The resident set: the only place document plaintext is allowed to exist.
//!
//! [`ResidentSet`] holds the decrypted bytes of a bounded number of segments and nothing else.
//! Its capacity is a byte budget fixed at construction from the viewport, **never** derived from
//! the document's length (L-2): the same set holds four 64 KiB segments whether the spine
//! describes one page or thirty million words.
//!
//! Every buffer is editable in place ([`SlabText`]) and wipes on drop, so a segment that leaves
//! the viewport is wiped rather than merely forgotten. The budget is checked against **allocated
//! capacity**, not against text length: slack reserved for typing is memory, and a bound that
//! ignored it would be a bound in name only.
//!
//! Recency is a monotonic counter, not a reordered deque — deliberately. A `VecDeque` hit would
//! move an entry to the back, and `push_back` may reallocate, which would put an allocator call
//! inside the keystroke path this module exists to keep clear. Scanning for the oldest entry is
//! O(resident) on a *miss* only, and the resident set is viewport-sized, not document-sized.
//!
//! The source of plaintext is a trait so the Spine does not depend on the world-line: in
//! production it is `holonomy-line`'s `WorldLine` opening one sealed segment at a time.

use crate::slab::SegmentId;
use crate::text::SlabText;

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
        /// Its allocated capacity, including slack.
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
    /// The source supplied bytes that are not valid UTF-8.
    NotText {
        /// The segment whose plaintext was rejected.
        segment: SegmentId,
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
                "{segment} needs {bytes} bytes, which exceeds the {budget}-byte resident budget"
            ),
            Self::Source { segment, message } => {
                write!(formatter, "cannot fault in {segment}: {message}")
            }
            Self::NotText { segment } => {
                write!(formatter, "the plaintext of {segment} is not valid UTF-8")
            }
        }
    }
}

impl std::error::Error for ResidencyError {}

struct Entry {
    segment: SegmentId,
    text: SlabText,
    last_used: u64,
}

/// A hard-bounded, least-recently-used set of decrypted, editable segments.
pub struct ResidentSet<S: SegmentSource> {
    source: S,
    budget_bytes: usize,
    slack_bytes: usize,
    clock: u64,
    entries: Vec<Entry>,
}

impl<S: SegmentSource> ResidentSet<S> {
    /// Creates a resident set with a fixed byte budget and `slack_bytes` of spare capacity
    /// reserved per segment for in-place editing.
    ///
    /// # Errors
    ///
    /// [`ResidencyError::ZeroBudget`] if `budget_bytes` is zero.
    pub fn new(
        source: S,
        budget_bytes: usize,
        slack_bytes: usize,
    ) -> Result<Self, ResidencyError> {
        if budget_bytes == 0 {
            return Err(ResidencyError::ZeroBudget);
        }
        Ok(Self {
            source,
            budget_bytes,
            slack_bytes,
            clock: 0,
            entries: Vec::new(),
        })
    }

    /// The configured budget in bytes of **allocated capacity**.
    #[must_use]
    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    /// The slack reserved per segment for in-place editing.
    #[must_use]
    pub fn slack_bytes(&self) -> usize {
        self.slack_bytes
    }

    /// Bytes of plaintext currently held.
    #[must_use]
    pub fn resident_bytes(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| entry.text.len())
            .sum::<usize>()
    }

    /// Bytes of capacity currently allocated, including editing slack. This is what the budget
    /// bounds, and what the process actually holds.
    #[must_use]
    pub fn allocated_bytes(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| entry.text.capacity())
            .sum::<usize>()
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
    /// A hit refreshes recency and touches no source and no allocator. A miss loads the segment,
    /// evicts until it fits, and pushes it.
    ///
    /// # Errors
    ///
    /// [`ResidencyError::SegmentExceedsBudget`] if the segment's capacity alone is larger than
    /// the budget (refusing it keeps the bound true), [`ResidencyError::Source`] if the load
    /// failed, or [`ResidencyError::NotText`] if the plaintext was not UTF-8.
    pub fn fault_in(&mut self, segment: SegmentId) -> Result<&[u8], ResidencyError> {
        let index = self.ensure(segment)?;
        Ok(self.entries[index].text.as_bytes())
    }

    /// Faults `segment` in and returns its editable text.
    ///
    /// This is the keystroke path's entry point: on a hit it performs no allocation, so the edit
    /// that follows runs inside a scope the counting allocator can prove quiet.
    ///
    /// # Errors
    ///
    /// As [`Self::fault_in`].
    pub fn fault_in_mut(&mut self, segment: SegmentId) -> Result<&mut SlabText, ResidencyError> {
        let index = self.ensure(segment)?;
        Ok(&mut self.entries[index].text)
    }

    /// The resident text of `segment`, if it is resident.
    #[must_use]
    pub fn text(&self, segment: SegmentId) -> Option<&SlabText> {
        self.entries
            .iter()
            .find(|entry| entry.segment == segment)
            .map(|entry| &entry.text)
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
        self.entries
            .retain(|entry| keep.contains(&entry.segment));
        before - self.entries.len()
    }

    /// Makes `segment` resident and returns its index. The one place loads, evictions and recency
    /// writes happen.
    fn ensure(&mut self, segment: SegmentId) -> Result<usize, ResidencyError> {
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.segment == segment)
        {
            self.clock += 1;
            self.entries[index].last_used = self.clock;
            return Ok(index);
        }

        let plaintext = self
            .source
            .load(segment)
            .map_err(|message| ResidencyError::Source { segment, message })?;
        let text = SlabText::from_plaintext(plaintext, self.slack_bytes)
            .map_err(|_| ResidencyError::NotText { segment })?;
        let capacity = text.capacity();
        if capacity > self.budget_bytes {
            return Err(ResidencyError::SegmentExceedsBudget {
                segment,
                bytes: capacity,
                budget: self.budget_bytes,
            });
        }
        while self.allocated_bytes() + capacity > self.budget_bytes {
            let index = self
                .oldest_index()
                .expect("a resident set over its byte budget is not empty");
            let evicted = self.entries.remove(index);
            drop(evicted);
        }
        self.clock += 1;
        self.entries.push(Entry {
            segment,
            text,
            last_used: self.clock,
        });
        Ok(self.entries.len() - 1)
    }

    fn oldest_index(&self) -> Option<usize> {
        self.entries
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(index, _)| index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slab::Slab;
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
            // Valid UTF-8, and distinguishable per segment: ASCII only.
            let byte = u8::try_from(segment.get() % 26).expect("a remainder below 26 fits") + b'a';
            Ok(vec![byte; self.segment_bytes])
        }
    }

    fn id(value: u64) -> SegmentId {
        SegmentId::new(value)
    }

    fn set(budget: usize) -> ResidentSet<Counting> {
        ResidentSet::new(Counting::new(64), budget, 0).expect("non-zero budget")
    }

    #[test]
    fn a_fault_in_returns_the_segments_bytes() {
        let mut resident = set(256);
        let bytes = resident.fault_in(id(1)).expect("source works");
        assert_eq!(bytes.len(), 64);
        assert_eq!(bytes[0], b'b');
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
        // Four 64-byte segments fit exactly; nothing is evicted until a fifth arrives.
        let mut resident = set(256);
        for segment in 0..4 {
            resident.fault_in(id(segment)).expect("source works");
        }
        assert_eq!(resident.resident_bytes(), 256);
        assert_eq!(resident.resident_segments(), 4);
        assert!(resident.contains(id(0)));

        resident.fault_in(id(4)).expect("source works");
        assert!(
            !resident.contains(id(0)),
            "the oldest segment must be evicted"
        );
        assert!(resident.contains(id(4)));
        assert_eq!(resident.resident_bytes(), 256);

        resident.fault_in(id(5)).expect("source works");
        assert!(!resident.contains(id(1)), "then the next oldest");
        assert!(resident.contains(id(5)));
        assert!(resident.allocated_bytes() <= resident.budget_bytes());
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
        let mut resident = ResidentSet::new(Counting::new(128), 64, 0).expect("non-zero budget");
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
    fn slack_counts_against_the_budget_because_slack_is_memory() {
        let mut resident =
            ResidentSet::new(Counting::new(64), 256, 32).expect("non-zero budget");
        for segment in 0..3 {
            resident.fault_in(id(segment)).expect("source works");
        }
        assert_eq!(resident.resident_segments(), 2);
        assert_eq!(resident.resident_bytes(), 128, "plaintext of two segments");
        assert!(
            resident.allocated_bytes() > resident.resident_bytes(),
            "slack is allocated even when unused"
        );
        assert!(resident.allocated_bytes() <= resident.budget_bytes());
    }

    #[test]
    fn a_resident_slab_can_be_edited_in_place() {
        let mut resident = ResidentSet::new(Counting::new(64), 256, 32).expect("non-zero budget");
        resident.fault_in(id(3)).expect("source works");
        let text = resident.fault_in_mut(id(3)).expect("resident");
        assert_eq!(text.insert(0, b"typed ").expect("fits"), crate::EditOutcome::InPlace);
        assert_eq!(resident.resident_bytes(), 70);
        assert_eq!(
            resident.text(id(3)).expect("resident").as_str(),
            format!("typed {}", "d".repeat(64))
        );
    }

    #[test]
    fn a_zero_budget_is_refused() {
        assert_eq!(
            ResidentSet::new(Counting::new(64), 0, 0).map(|_| ()),
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
    fn plaintext_that_is_not_utf8_is_refused() {
        struct Binary;
        impl SegmentSource for Binary {
            fn load(&mut self, _segment: SegmentId) -> Result<Vec<u8>, String> {
                Ok(vec![0xFF, 0xFE])
            }
        }
        let mut resident = ResidentSet::new(Binary, 256, 0).expect("non-zero budget");
        assert_eq!(
            resident.fault_in(id(0)),
            Err(ResidencyError::NotText { segment: id(0) })
        );
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
        let mut resident = ResidentSet::new(Counting::new(65_536), budget, 0)
            .expect("non-zero budget");

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
            ResidentSet::new(Counting::new(65_536), budget, 0).expect("non-zero budget");
        small_resident
            .fault_in(small.locate(0).expect("in range").slab().segment())
            .expect("source works");
        assert_eq!(small_resident.resident_bytes(), 65_536);
        assert_eq!(small_resident.budget_bytes(), resident.budget_bytes());
    }
}
