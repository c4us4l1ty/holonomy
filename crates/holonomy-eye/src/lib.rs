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
//! - `cluster`: grapheme-cluster segmentation by the rules of UAX #29 (GB3–GB13, in their
//!   numbered order), so a caret can never land inside an extended grapheme cluster.
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
//!   O(log n) in the document's length — and [`window_text`], which reads that range out of the
//!   resident set and the pending buffer as the [`WindowText`] a layout is handed.
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
//! L-8 still carries `status: spec`, and the crate says why rather than leaving it to a reader. The
//! segmentation *rules* are the standard's, applied in order, and the **property data** they read
//! is generated from the Unicode Character Database (`scripts/gen-cluster-tables.py` writes
//! `cluster/tables.rs`), so a mark from any script joins its base, not only the scripts this UI's
//! spec names. The rules run through GB13, Unicode 15.1's Indic conjunct rule included (AUDIT
//! D-28). Per-paragraph bidirectional resolution and per-line visual reordering follow UAX #9
//! (`bidi.rs` + `bidi/tables.rs`, generated from UCD 18.0.0 by `scripts/gen-bidi-tables.py`),
//! preserving L-8's single-geometry bijection across RTL and mixed-direction lines (AUDIT OQ-11).
//! Complex-script glyph shaping (`GSUB`/`GPOS` beyond `Face`'s `cmap`/`hmtx` metrics, `OQ-24`) and
//! reference-hardware certification remain open before L-8 promotes from `spec` to `gated`.

mod bidi;
mod cluster;
mod face;
mod geom;
mod heights;
mod layout;
mod picture;
mod shaper;
mod style;
mod view;
mod window;

pub use crate::bidi::ParagraphContext;
pub use crate::cluster::{cluster_cells, ClusterContext, Clusters};
pub use crate::face::{Face, FaceError};
pub use crate::geom::{Fixed, Point};
pub use crate::heights::{HeightModel, Window};
pub use crate::layout::{
    layout_styled, layout_styled_continuing, layout_styled_from, layout_styled_in_paragraph,
    layout_window, Caret, Layout, Line, Placed,
};
pub use crate::picture::{Picture, PictureCluster, PictureLine, SelectionSpan};
pub use crate::shaper::Shaper;
pub use crate::style::{Style, StyledRun, StyledText};
pub use crate::view::View;
pub use crate::window::{byte_window, window_text, ShapedWindow, WindowText, WindowTextError};

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
