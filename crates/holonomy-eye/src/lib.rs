//! The Eye — headless layout, paint geometry, and hit-testing (ROADMAP Block 5).
//!
//! Block 5 has not begun. What lands now is not code but the crate's constitutional clause,
//! because it is the one v3 violated to death:
//!
//! > **One geometry, two consumers.** Paint and hit-test walk the same shaped runs through the
//! > same API. There is no second arithmetic to drift from the first — v3 painted proportional
//! > glyph advances while clicking in monospace cells, and caret, selection, and click each
//! > pointed at a different byte.
//!
//! When Block 5 starts, the property test `hit_test(paint_to_point(g)) == g` for every shaped
//! glyph is GATES law **L-8**. Until then, this crate is the place that law lives.

/// The single-geometry law, carried in code so it is compiled, not just quoted.
pub const SINGLE_GEOMETRY_LAW: &str = "paint and hit-test consume one layout run tree; no second geometry may exist";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_law_is_written() {
        assert!(!SINGLE_GEOMETRY_LAW.is_empty());
    }
}
