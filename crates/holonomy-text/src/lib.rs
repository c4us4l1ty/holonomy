//! The Spine — the immutable slab-tree document engine (ROADMAP Block 4).
//!
//! Block 4 has not begun; its construction is specified in `docs/rfc/RFC-003-the-spine.md`.
//! What exists now is the crate's constitutional clause:
//!
//! > **Never decrypt more than you look at.** The document is a path-copied tree of segment
//! > references; plaintext exists only inside the viewport's resident set and is wiped leaving
//! > it. Undo is a pointer walk, never a plaintext buffer — otherwise the wipe set would grow
//! > with history. Keystroke allocation is measured zero (GATES law L-2 via the ledger's
//! > counting allocator), and latency is invariant in document length.

/// The eyes-only law, carried in code so it is compiled, not just quoted.
pub const EYES_ONLY_LAW: &str = "plaintext is bounded by the viewport, wiped on exit, and document length never enters the memory equation";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_law_is_written() {
        assert!(!EYES_ONLY_LAW.is_empty());
    }
}
