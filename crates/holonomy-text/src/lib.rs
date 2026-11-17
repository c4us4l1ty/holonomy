//! The Spine — the immutable slab-tree document engine (ROADMAP Block 4).
//!
//! Block 4's construction is specified in `docs/rfc/RFC-003-the-spine.md`. Two slices have landed:
//! the spine itself with its position algebra and undo-as-pointer-swap, and the editable resident
//! text whose in-place insert/delete is **measured** allocation-free by
//! `tests/keystroke_allocations.rs` (GATES law L-2's keystroke half). What is still missing is the
//! editor above them — the commit boundary that path-copies the spine when a slab is re-sealed —
//! so L-2 as a whole remains `spec`, and the Block 4 gates remain open.
//!
//! The constitutional clause still governs everything here:
//!
//! > **Never decrypt more than you look at.** The document is a path-copied tree of segment
//! > references; plaintext exists only inside the viewport's resident set and is wiped leaving
//! > it. Undo is a pointer walk, never a plaintext buffer — otherwise the wipe set would grow
//! > with history.
//!
//! Four structural consequences, each witnessed by a test in this crate:
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

pub mod residency;
pub mod slab;
mod spine;
pub mod text;
mod tree;

pub use residency::{ResidencyError, ResidentSet, SegmentSource};
pub use slab::{SegmentId, Slab, SlabHit, SlabSpan};
pub use spine::{History, Spine, SpineError};
pub use text::{EditOutcome, SlabText, TextError};

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
