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
//! - `shaper`: the [`Shaper`] trait — the only door through which advances enter the system, and
//!   it is asked with a [`Style`], because a heading is a geometry change and not a decoration.
//! - `style`: [`Style`], [`StyledRun`] and [`StyledText`]: style runs as layout data, so Block 7's
//!   toolbar can change a heading without a renderer ever applying a style the layout did not
//!   shape with.
//! - `layout`: one run tree per laid-out window, which both [`Layout::paint_point`] and
//!   [`Layout::hit_test`] walk.
//! - `heights`: [`HeightModel`], the Fenwick index over leaf heights, so the Eye can find a
//!   viewport in O(log n) index reads instead of measuring the document to discover where it is.
//! - `window`: [`byte_window`], the join between that index and the document — leaf positions to a
//!   contiguous byte range, one [`Spine::slab_span`] descent per edge, so what to shape costs
//!   O(log n) in the document's length.
//!
//! [`Spine::slab_span`]: holonomy_text::Spine::slab_span
//! - `view`: [`View`], zoom and DPI as one exact integer transform that **both** consumers call —
//!   paint through [`View::paint_point`], hit-test through [`View::hit_test`] on a device-space
//!   point. There is deliberately no inverse transform: hit-testing compares device observations
//!   against the device positions paint would draw, so no rounding is ever undone by a second
//!   rounding.
//!
//! The property L-8 names — every caret survives the trip from paint to hit-test and back, at
//! every zoom and DPI — is tested across a corpus of scripts, marks, ZWJ sequences, wide clusters
//! and line breaks, at eight zoom steps and five DPIs, on a fake shaper so it needs no font.
//!
//! L-8 still carries `status: spec`, and the crate says why rather than leaving it to a reader:
//! segmentation is a hand-written approximation of UAX #29 (a few sequences UAX #29 joins are left
//! as separate clusters, and BiDi reordering is not attempted at all), and the run tree is built
//! from a whole window of text, so nothing here yet constrains shaping cost to the viewport. Those
//! are Block 5's remaining gates, not this crate's footnotes.

mod cluster;
mod geom;
mod heights;
mod layout;
mod shaper;
mod style;
mod view;
mod window;

pub use crate::cluster::Clusters;
pub use crate::geom::{Fixed, Point};
pub use crate::heights::{HeightModel, Window};
pub use crate::layout::{layout_styled, layout_window, Caret, Layout, Line, Placed};
pub use crate::shaper::Shaper;
pub use crate::style::{Style, StyledRun, StyledText};
pub use crate::view::View;
pub use crate::window::{byte_window, ShapedWindow};

/// The single-geometry law, carried in code so it is compiled, not just quoted.
pub const SINGLE_GEOMETRY_LAW: &str =
    "paint and hit-test consume one layout run tree; no second geometry may exist";

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_law_is_written() {
        assert!(!SINGLE_GEOMETRY_LAW.is_empty());
    }
}
