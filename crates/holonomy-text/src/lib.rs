//! The Spine — the immutable slab-tree document engine (ROADMAP Block 4).
//!
//! Block 4's construction is specified in `docs/rfc/RFC-003-the-spine.md`. Four pieces have
//! landed: the spine itself with its position algebra and undo-as-pointer-swap; the editable
//! resident text whose in-place insert/delete is **measured** allocation-free by
//! `tests/keystroke_allocations.rs` (GATES law L-2's keystroke half); the commit boundary
//! ([`commit`]) that seals an edited slab into the world-line, takes the witness, and path-copies
//! the spine to the new root — proven against a real `holonomy-line` world-line in
//! `tests/commit_boundary.rs`; the live-length overlay ([`Draft`]) that keeps every position
//! query correct *between* commits, while the spine is stale by design; the typing session
//! ([`Typing`]) where open sources are born, typed into, sealed, deleted and undone; and selection
//! ([`Selection`], [`select::copy`]) — a range in the live document, the leaves it covers, and the
//! bytes behind them. What is still missing is the editor itself and Block 5's Eye, so L-2 as a
//! whole remains `spec` and the Block 4 gates remain open.
//!
//! The constitutional clause still governs everything here:
//!
//! > **Never decrypt more than you look at.** The document is a path-copied tree of segment
//! > references; plaintext exists only inside the viewport's resident set and is wiped leaving
//! > it. Undo is a pointer walk, never a plaintext buffer — otherwise the wipe set would grow
//! > with history.
//!
//! Ten structural consequences, each witnessed by a test in this crate:
//!
//! * A [`Spine`] holds no plaintext at all — only [`Slab`] references — so a sixty-year document
//!   costs bytes that scale with segment count, not with readable text.
//! * Every edit returns a new spine and leaves the old one valid; [`History`] therefore stores
//!   roots, and undo allocates nothing.
//! * [`ResidentSet`] has a fixed byte budget set from the viewport, checks it against **allocated
//!   capacity** (slack included), and no API accepts a document length, so document size cannot
//!   enter the memory equation.
//! * [`SlabText`] refuses an edit that would need to grow rather than growing quietly, which is
//!   what lets the counting allocator prove the keystroke path quiet instead of assuming it.
//! * [`commit_slab`] refuses a span the spine no longer holds **before** it appends, so a stale
//!   commit cannot leave an authenticated but unreferenced segment in the world-line.
//! * [`Draft`] answers offsets and ranges against the committed geometry plus the drafted lengths,
//!   so the document view is correct while the spine is out of date — and an empty overlay answers
//!   exactly as the spine does.
//! * A leaf names a **source**, and a source is either open (bytes in a resident buffer) or sealed
//!   (bytes in the world-line), at the same address ([`OpenSources`], D-7). Text being typed is
//!   therefore an ordinary leaf, and sealing it changes no root — so undo, history and `verify`
//!   never observe a seal.
//! * Deletion is a pointer operation: removing a range copies a root and writes no bytes, because
//!   the bytes were never the document's. It follows that deletion does not erase — the world-line
//!   keeps what was removed, exactly as an append-only log must ([`Typing::delete`], OQ-14).
//! * A selection costs the bytes it covers: [`select::copy`] faults in exactly the leaves the range
//!   touches and clips each one, so two bytes out of a 64 KiB leaf return two bytes — the
//!   eyes-only law at clipboard granularity.
//! * Undo of typing is a **length**, not a copy: [`Typing::mark`] records how many bytes the open
//!   source had, and the overlay hides the rest, so undo and redo copy no root, write no byte and
//!   allocate nothing. Undo of a **deletion** is the other half and costs a pointer: the root from
//!   before the cut, restored by name, because the segments a cut removed are still in the
//!   world-line. Both halves are rings — 1024 lengths and 64 roots — so history's memory is bounded
//!   by the tree's depth and not by the number of edits (OQ-8).

pub mod commit;
pub mod draft;
pub mod formatting;
pub mod residency;
pub mod search;
pub mod select;
pub mod slab;
pub mod source;
mod spine;
pub mod text;
mod tree;
pub mod typing;

pub use commit::{
    commit_slab, Appended, CommitError, Committed, ReplacementBuilder, SegmentSink, Witness,
};
pub use draft::{Draft, DraftError, LiveHit, LiveSpan};
pub use formatting::{
    Alignment, DocStyle, DocStyleSpan, FontFamily, Heading, ListKind, StyleMap, StylePatch,
    ToolbarState, TriState,
};
pub use residency::{ResidencyError, ResidentSet, SegmentSource, WipeRegistry};
pub use search::{find_streaming, SearchError, SearchMatch};
pub use select::{copy, CopyError, Selection};
pub use slab::{SegmentId, Slab, SlabHit, SlabSpan};
pub use source::{is_provisional, Location, OpenSources, SealError};
pub use spine::{History, Spine, SpineError};
pub use text::{EditOutcome, SlabText, TextError};
pub use typing::{StepSummary, Typing, TypingError};

/// Test-only support shared by the crate's property tests.
#[cfg(test)]
pub(crate) mod test_support {
    /// A deterministic xorshift64 PRNG.
    ///
    /// Property tests must be reproducible from a seed: a failing case has to be re-runnable
    /// without a dependency, a harness, or a saved corpus.
    pub(crate) struct XorShift(u64);

    impl XorShift {
        /// Seeds the generator; a zero seed is replaced so the sequence cannot be constant.
        pub(crate) fn new(seed: u64) -> Self {
            Self(seed | 1)
        }

        /// The next value.
        pub(crate) fn next(&mut self) -> u64 {
            let mut state = self.0;
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            self.0 = state;
            state
        }

        /// A value in `0..bound`; zero when `bound` is zero.
        pub(crate) fn below(&mut self, bound: u64) -> u64 {
            if bound == 0 {
                0
            } else {
                self.next() % bound
            }
        }
    }
}

/// The eyes-only law, carried in code so it is compiled, not just quoted.
pub const EYES_ONLY_LAW: &str =
    "plaintext is bounded by the viewport, wiped on exit, and document length never enters the memory equation";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_law_is_written() {
        assert!(!EYES_ONLY_LAW.is_empty());
    }

    #[test]
    fn a_keystroke_stays_inside_the_slab() {
        let mut text = SlabText::from_text("hello", 8);
        assert_eq!(text.insert(5, b"!").expect("fits"), EditOutcome::InPlace);
        assert_eq!(text.as_str(), "hello!");
        assert_eq!(text.delete(0, 1).map(|()| text.as_str()), Ok("ello!"));
    }

    #[test]
    fn a_spine_is_geometry_only() {
        // The public surface exposes slabs, offsets and spans; nothing returns plaintext except
        // through the resident set, whose source is the caller's own.
        let spine = Spine::from_slabs([Slab::new(SegmentId::new(0), 0, 4_096)]);
        assert_eq!(spine.len(), 4_096);
        assert_eq!(spine.slab_count(), 1);
        assert!(spine.locate(4_095).is_some());
    }
}
