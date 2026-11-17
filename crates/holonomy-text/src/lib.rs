//! The Spine — the immutable slab-tree document engine (ROADMAP Block 4).
//!
//! Block 4's construction is specified in `docs/rfc/RFC-003-the-spine.md`. This crate is its
//! first slice: the spine itself, its position algebra, undo as a pointer swap, and the
//! viewport-bounded resident set. What the slice deliberately does **not** contain is the part
//! that needs measurement rather than design — the mutable resident text buffer, the keystroke
//! path, and therefore the L-2 and zero-allocation gates, which stay `spec` in `docs/GATES.md`
//! until they are measured rather than asserted.
//!
//! The constitutional clause still governs everything here:
//!
//! > **Never decrypt more than you look at.** The document is a path-copied tree of segment
//! > references; plaintext exists only inside the viewport's resident set and is wiped leaving
//! > it. Undo is a pointer walk, never a plaintext buffer — otherwise the wipe set would grow
//! > with history.
//!
//! Three structural consequences, each witnessed by a test in this crate:
//!
//! * A [`Spine`] holds no plaintext at all — only [`Slab`] references — so a sixty-year document
//!   costs bytes that scale with segment count, not with readable text.
//! * Every edit returns a new spine and leaves the old one valid; [`History`] therefore stores
//!   roots, and undo allocates nothing.
//! * [`ResidentSet`] has a fixed byte budget set from the viewport, and no API accepts a document
//!   length, so document size cannot enter the memory equation (L-2's mechanism; its gate is a
//!   measurement that arrives with Block 6's reference hardware).

pub mod residency;
pub mod slab;
mod spine;
mod tree;

pub use residency::{ResidencyError, ResidentSet, SegmentSource};
pub use slab::{SegmentId, Slab, SlabHit, SlabSpan};
pub use spine::{History, Spine, SpineError};

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
    fn a_spine_is_geometry_only() {
        // The public surface exposes slabs, offsets and spans; nothing returns plaintext except
        // through the resident set, whose source is the caller's own.
        let spine = Spine::from_slabs([Slab::new(SegmentId::new(0), 0, 4_096)]);
        assert_eq!(spine.len(), 4_096);
        assert_eq!(spine.slab_count(), 1);
        assert!(spine.locate(4_095).is_some());
    }
}
