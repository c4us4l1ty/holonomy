//! Open sources: the text being typed, before it is sealed.
//!
//! D-7's turn is a separation of two questions that were tangled together: *where does this leaf's
//! text live* and *which sealed segment is it in*. A [`crate::Slab`] needs the first; only the
//! storage layer cares about the second. So a leaf names a **source**, and a source is either
//! **open** — bytes in a resident buffer, being typed into — or **sealed** — bytes in the
//! world-line. The same address means the same text either way, which is what lets the text under
//! the caret be an ordinary leaf: an index in the height model, a byte range from `slab_span`, and
//! therefore a place in `holonomy_eye::byte_window`.
//!
//! Sealing is then a **state change of one source**, not an edit of the document: the same id, the
//! bytes appended once, and **no new root** — which is why this type takes no [`crate::Spine`] at
//! all. History, undo and `verify` never observe a seal, because the document never changed; only
//! where its bytes live did. And because nothing that is sealed is ever typed into again, each
//! byte is written exactly once — which removes option A's quadratic cost rather than managing it.
//!
//! # Provisional ids
//!
//! An open source has no position in the world-line yet, so it is addressed by a **provisional
//! id** taken from the top of the id space. A provisional id is never handed out twice and can
//! never collide with a real segment id: the world-line would need 2^63 segments to reach it. On
//! seal the register records the rename from provisional id to real one, and the leaves that name
//! the provisional id keep working untouched.
//!
//! This is not a cache. Dropping the register loses no text — the bytes are in the resident buffer
//! and the world-line respectively — but it does lose the renames, so a store persists them beside
//! its segments.

use std::collections::BTreeMap;

use crate::slab::SegmentId;

/// The first provisional id. Every id at or above it belongs to an open source; every id below it
/// belongs to the world-line.
const PROVISIONAL_BASE: u64 = 1 << 63;

/// Where a source's bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// In a resident buffer: readable, editable, not yet in the world-line.
    Open {
        /// The provisional id the source is addressed by.
        source: SegmentId,
    },
    /// A sealed segment of the world-line.
    Sealed {
        /// The segment's id — the real one, not a provisional id.
        segment: SegmentId,
    },
    /// Neither open nor sealed: a provisional id this register never handed out.
    Unknown,
}

impl Location {
    /// Whether the bytes are in memory rather than in the world-line.
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Open { .. })
    }

    /// The sealed segment's id, when the source is sealed.
    #[must_use]
    pub const fn segment(self) -> Option<SegmentId> {
        match self {
            Self::Sealed { segment } => Some(segment),
            Self::Open { .. } | Self::Unknown => None,
        }
    }
}

/// Why a source could not be sealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealError {
    /// The source is not open — either it was already sealed, or it never existed.
    NotOpen {
        /// The id that was offered.
        source: SegmentId,
    },
    /// The id offered as the sealed name is provisional: a sealed segment's id must be real.
    ProvisionalName {
        /// The id that was refused.
        segment: SegmentId,
    },
    /// Another source is already sealed under that id.
    NameTaken {
        /// The id that was refused.
        segment: SegmentId,
    },
}

impl std::fmt::Display for SealError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotOpen { source } => write!(formatter, "{source} is not an open source"),
            Self::ProvisionalName { segment } => {
                write!(formatter, "{segment} is provisional, not a sealed id")
            }
            Self::NameTaken { segment } => {
                write!(formatter, "another source is already sealed as {segment}")
            }
        }
    }
}

impl std::error::Error for SealError {}

/// The sources that are still open, and the renames of those that have been sealed.
///
/// At most a handful of sources are open at once, because each one is a resident buffer and
/// residency is a bounded resource (L-2). The editor's insert path seals before it opens another.
#[derive(Debug, Clone, Default)]
pub struct OpenSources {
    /// Provisional ids handed out so far, counted up from [`PROVISIONAL_BASE`].
    handed_out: u64,
    /// Provisional id → sealed id, for every source that has been sealed.
    sealed: BTreeMap<SegmentId, SegmentId>,
    /// Provisional ids that are still open.
    open: Vec<SegmentId>,
}

impl OpenSources {
    /// An empty register.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            handed_out: 0,
            sealed: BTreeMap::new(),
            open: Vec::new(),
        }
    }

    /// Opens a source, returning the provisional id its leaves should name.
    ///
    /// The caller owns a resident buffer for this id from here until [`Self::seal`]. The id is
    /// never reused, so a leaf that names it keeps naming this text for the document's lifetime.
    #[must_use]
    pub fn open(&mut self) -> SegmentId {
        let value = PROVISIONAL_BASE + self.handed_out;
        self.handed_out += 1;
        let source = SegmentId::new(value);
        self.open.push(source);
        source
    }

    /// Seals `source` under `segment`, recording the rename.
    ///
    /// The source's bytes were appended to the world-line under `segment`; every leaf naming the
    /// provisional id now resolves to it. No root changes, so this is not an edit.
    pub fn seal(&mut self, source: SegmentId, segment: SegmentId) -> Result<(), SealError> {
        let Some(index) = self.open.iter().position(|open| *open == source) else {
            return Err(SealError::NotOpen { source });
        };
        if is_provisional(segment) {
            return Err(SealError::ProvisionalName { segment });
        }
        if self.sealed.values().any(|sealed| *sealed == segment) {
            return Err(SealError::NameTaken { segment });
        }
        self.open.remove(index);
        self.sealed.insert(source, segment);
        Ok(())
    }

    /// Abandons `source`: its text was deleted before it was ever sealed.
    ///
    /// Nothing is appended and nothing is renamed, so the provisional id stops naming text and
    /// [`Self::locate`] answers [`Location::Unknown`] for it. That is the honest answer — the id
    /// *was* handed out, so it is not a stranger, but the text it named is gone, and renaming it to
    /// a segment that was never written would be worse.
    ///
    /// Returns whether the source was open, so closing twice is refused rather than repeated.
    pub fn close(&mut self, source: SegmentId) -> bool {
        let Some(index) = self.open.iter().position(|open| *open == source) else {
            return false;
        };
        self.open.remove(index);
        true
    }

    /// Where the bytes of `address` are, as far as this register knows.
    ///
    /// A non-provisional id is a sealed segment by definition: the world-line only ever mints real
    /// ids when it appends.
    #[must_use]
    pub fn locate(&self, address: SegmentId) -> Location {
        if !is_provisional(address) {
            return Location::Sealed { segment: address };
        }
        if let Some(segment) = self.sealed.get(&address) {
            return Location::Sealed { segment: *segment };
        }
        if self.open.contains(&address) {
            return Location::Open { source: address };
        }
        Location::Unknown
    }

    /// Whether `id` was handed out and then abandoned — closed before its bytes were ever sealed.
    ///
    /// [`Self::close`] deliberately keeps no list of abandoned ids, because the register already
    /// knows enough to derive the answer: provisional ids are minted by counting up from
    /// [`PROVISIONAL_BASE`], so any id below the counter that is neither open nor sealed was handed
    /// out and then let go. Deriving it means the answer cannot drift from the counter.
    ///
    /// The distinction matters to anything that resurrects an old root: a leaf naming an abandoned
    /// id refers to text that was **never written** to the world-line, so it is unrecoverable, and
    /// that is a different fact from a leaf naming an id that was never minted at all.
    #[must_use]
    pub fn was_abandoned(&self, id: SegmentId) -> bool {
        is_provisional(id)
            && id.get() < PROVISIONAL_BASE + self.handed_out
            && !self.open.contains(&id)
            && !self.sealed.contains_key(&id)
    }

    /// How many sources are open right now — the number of resident buffers the editor owes.
    #[must_use]
    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    /// The sealed id a provisional id was renamed to, if it was.
    #[must_use]
    pub fn sealed_as(&self, source: SegmentId) -> Option<SegmentId> {
        self.sealed.get(&source).copied()
    }
}

/// Whether `id` belongs to the open-source half of the id space.
#[must_use]
pub const fn is_provisional(id: SegmentId) -> bool {
    id.get() >= PROVISIONAL_BASE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slab::Slab;
    use crate::spine::Spine;

    #[test]
    fn open_sources_get_distinct_provisional_ids() {
        let mut sources = OpenSources::new();
        let first = sources.open();
        let second = sources.open();
        assert_ne!(first, second);
        assert!(is_provisional(first) && is_provisional(second));
        assert_eq!(sources.open_count(), 2);
        assert_eq!(sources.locate(first), Location::Open { source: first });
        assert!(sources.locate(second).is_open());
    }

    #[test]
    fn sealing_renames_the_address_and_leaves_the_leaf_alone() {
        let mut sources = OpenSources::new();
        let provisional = sources.open();
        // The leaf that names the text being typed — built before anything is sealed, and never
        // rebuilt afterwards.
        let spine = Spine::from_slabs(vec![Slab::new(provisional, 0, 64)]);
        let before = spine.slab_span(0).expect("a leaf");

        sources.seal(provisional, SegmentId::new(7)).expect("open");
        assert_eq!(
            sources.locate(provisional),
            Location::Sealed {
                segment: SegmentId::new(7)
            }
        );
        assert_eq!(sources.sealed_as(provisional), Some(SegmentId::new(7)));
        assert_eq!(sources.open_count(), 0);

        let after = spine.slab_span(0).expect("a leaf");
        assert_eq!(before, after);
        assert_eq!(after.slab().segment(), provisional);
    }

    #[test]
    fn a_sealed_id_cannot_be_reused_and_a_provisional_one_cannot_be_a_name() {
        let mut sources = OpenSources::new();
        let first = sources.open();
        let second = sources.open();
        sources.seal(first, SegmentId::new(1)).expect("open");
        assert_eq!(
            sources.seal(second, SegmentId::new(1)),
            Err(SealError::NameTaken {
                segment: SegmentId::new(1)
            })
        );
        assert_eq!(
            sources.seal(second, first),
            Err(SealError::ProvisionalName { segment: first })
        );
        // A source that is already sealed is not open a second time.
        assert_eq!(
            sources.seal(first, SegmentId::new(2)),
            Err(SealError::NotOpen { source: first })
        );
        assert_eq!(sources.open_count(), 1);
    }

    #[test]
    fn a_real_id_is_sealed_by_definition_and_a_stranger_is_unknown() {
        let mut sources = OpenSources::new();
        assert_eq!(
            sources.locate(SegmentId::new(0)),
            Location::Sealed {
                segment: SegmentId::new(0)
            }
        );
        // A provisional id that was never handed out names nothing.
        assert_eq!(
            sources.locate(SegmentId::new(PROVISIONAL_BASE)),
            Location::Unknown
        );
        let first = sources.open();
        assert_eq!(first, SegmentId::new(PROVISIONAL_BASE));
        assert!(sources.locate(first).is_open());
    }

    #[test]
    fn an_abandoned_source_becomes_unknown_rather_than_staying_open() {
        let mut sources = OpenSources::new();
        let source = sources.open();
        assert!(sources.locate(source).is_open());
        assert!(sources.close(source));
        // Not open any more, and not sealed under some invented segment: unknown.
        assert_eq!(sources.locate(source), Location::Unknown);
        assert_eq!(sources.open_count(), 0);
        assert_eq!(sources.sealed_as(source), None);
        assert!(
            !sources.close(source),
            "closing twice is refused, not repeated"
        );
        // The evidence survives without being stored: an id below the counter that is neither open
        // nor sealed was handed out and let go, which is not the same as never having existed.
        assert!(sources.was_abandoned(source));
        assert!(!sources.was_abandoned(SegmentId::new(PROVISIONAL_BASE + 1)));
        assert!(!sources.was_abandoned(SegmentId::new(0)));
    }

    #[test]
    fn typing_a_megabyte_seals_a_megabyte() {
        // The buffer size the Spine's resident text uses.
        const BUFFER: u64 = 64 * 1024;
        let mut sources = OpenSources::new();
        let mut current = sources.open();
        let mut sealed_bytes = 0u64;
        let mut typed = 0u64;
        let mut next_segment = 0u64;
        let mut filled = 0u64;
        let total = 1024 * 1024;
        for _ in 0..total {
            typed += 1;
            filled += 1;
            if filled == BUFFER {
                sources
                    .seal(current, SegmentId::new(next_segment))
                    .expect("open");
                next_segment += 1;
                sealed_bytes += filled;
                filled = 0;
                current = sources.open();
            }
        }
        // Every byte that was sealed was typed once, and nothing was sealed twice.
        assert_eq!(sealed_bytes, typed - filled);
        assert_eq!(sealed_bytes, total - filled);
        assert_eq!(next_segment, total / BUFFER);
        // One source is still open, holding what has not been sealed yet.
        assert_eq!(sources.open_count(), 1);

        // Contrast with re-sealing the leaf that is being typed into (option A): the leaf keeps
        // growing, so the k-th seal writes k·64 KiB.
        let old_sealed: u64 = (1..=next_segment).map(|k| k * BUFFER).sum();
        assert!(
            old_sealed > 8 * sealed_bytes,
            "option A sealed {old_sealed} bytes against {sealed_bytes}"
        );
        // The ratio is not a constant: it grows linearly with what has already been written, so the
        // same policy costs 256× at 32 MiB of typing into one leaf.
        let old_at_32_mib: u64 = (1..=512).map(|k| k * BUFFER).sum();
        let typed_at_32_mib = 512 * BUFFER;
        assert!(old_at_32_mib > 200 * typed_at_32_mib);
    }
}
