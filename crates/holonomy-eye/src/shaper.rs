//! The metrics contract between the Eye and whatever actually shapes glyphs.
//!
//! The Eye owns geometry, never fonts. A shaper answers two questions per cluster — how wide is it,
//! how tall is a line — and everything else in this crate is arithmetic over those answers. In
//! production the shaper is Block 8's GPU text pipeline; in tests it is a deterministic fake, which
//! is why the property that matters can be proven without a font, a window or a GPU.
//!
//! **There is exactly one place advances enter the system.** Paint scales them, hit-test inverts
//! them, selection walks them — all through the run tree the Eye builds once. A second call into
//! the shaper from the hit-test path would be the two-geometry disease with extra steps.

use crate::geom::Fixed;
use crate::style::Style;

/// Where cluster widths and line heights come from.
///
/// Every metric question is asked *with* the style it applies to, because a heading is not the
/// same layout drawn bigger: it has bigger advances and a taller line box, and the layout has to
/// know that before it can place a caret.
pub trait Shaper {
    /// The advance width of one grapheme cluster at `style`, in logical units.
    ///
    /// Called once per cluster per layout, never from hit-test.
    fn advance(&self, cluster: &str, style: Style) -> Fixed;

    /// The height of one line of text at `style`, in logical units.
    ///
    /// Must be positive: hit-testing picks a line by `y`, and a zero-height line box would make
    /// that choice undefined. Layout debug-asserts it.
    fn line_height(&self, style: Style) -> Fixed;

    /// The distance from a line's top to its baseline at `style`, in logical units.
    ///
    /// Paint needs it to place glyphs; hit-test never does, which is why it cannot drift from it.
    fn ascent(&self, style: Style) -> Fixed;
}
