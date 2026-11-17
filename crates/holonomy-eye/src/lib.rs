//! The Eye — headless layout, paint geometry, and hit-testing (ROADMAP Block 5).
//!
//! The crate exists to hold one law, the one v3 violated to death:
//!
//! > **One geometry, two consumers.** Paint and hit-test walk the same shaped runs through the
//! > same API. There is no second arithmetic to drift from the first — v3 painted proportional
//! > glyph advances while clicking in monospace cells, and caret, selection, and click each
//! > pointed at a different byte.
//!
//! GATES law **L-8** — `hit_test(paint_point(glyph)) == glyph` for every shaped glyph, at every
//! zoom and DPI — still carries `status: spec`. What lands here is the machinery that law
//! quantifies over:
//!
//! - `geom`: [`Fixed`] coordinates in 64ths of a logical unit (integer, so equality is a
//!   question about geometry and not about rounding) and [`Point`].
//! - `cluster`: grapheme-cluster segmentation, so a caret can never land inside an extended
//!   grapheme cluster.
//! - `shaper`: the [`Shaper`] trait — the only door through which advances enter the system.
//! - `layout`: one run tree per laid-out window, which both [`Layout::paint_point`] and
//!   [`Layout::hit_test`] walk.
//!
//! Two gaps keep L-8 at `spec` rather than `enforced`. Segmentation is a hand-written
//! approximation of UAX #29 (a few sequences UAX #29 joins are left as separate clusters), and the
//! zoom/DPI view transform does not exist yet, so the "at every zoom and DPI" half of the law has
//! nothing to quantify over. Both are later Block 5 slices; neither can produce two answers for
//! the same point, which is what the law forbids.

mod cluster;
mod geom;
mod layout;
mod shaper;

pub use crate::cluster::Clusters;
pub use crate::geom::{Fixed, Point};
pub use crate::layout::{layout_window, Caret, Layout, Line, Placed};
pub use crate::shaper::Shaper;

/// The single-geometry law, carried in code so it is compiled, not just quoted.
pub const SINGLE_GEOMETRY_LAW: &str =
    "paint and hit-test consume one layout run tree; no second geometry may exist";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_law_is_written() {
        assert!(!SINGLE_GEOMETRY_LAW.is_empty());
    }
}
