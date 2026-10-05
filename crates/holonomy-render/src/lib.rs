//! SSE2 blitter, damage tracking and the surface tree.
//!
//! Lands in Phase 5. The SSE2 text blitter is Phase 4's [`holonomy_assets::blit`]; this crate owns
//! the parts that need to know the *panel's* geometry: the damage tracker and, next, the surface
//! tree.
//!
//! # What is here, and why damage comes first
//!
//! [`damage`] implements FR-3.4, the dirty-row model, because it is the requirement that the rest of
//! the renderer has to be built around: the blitter's inner loop has no bounds checks per row *because*
//! damage arrives pre-clipped, and a full-screen redraw during typing is prohibited precisely so that
//! the damage union stays small. Getting the tracker right first is what makes the surface tree's job
//! -- turning a list of draw commands into a bounded rect -- possible.
//!
//! See PROJECT.md §5 Phase 5 and Plan.md §2.3 FR-3.4.

mod damage;
mod tree;

/// Phase 9B: the LaTeX micro-parser and its procedural layout.
///
/// Public as modules, not only as the re-exports below, because a caller that needs to *name* the
/// module -- `holonomy_render::math::parse` rather than the free `parse_math` -- is doing something
/// the flat re-export list reads ambiguously. `parse` in particular collides with half a dozen other
/// `parse` functions in the workspace.
pub mod math;
pub mod math_layout;

pub mod chrome;
pub mod table;

pub use chrome::{
    AscendingRun, Blink, Caret, Chrome, ChromeMetrics, ChromeState, Layout, LineHeights,
    StyleFlags, StyleFlagsSlot,
};
pub use damage::{DamageRect, DamageTracker};
pub use math::{
    parse as parse_math, symbol, MathError, MathNode, MAX_DEPTH, OUT_OF_SCOPE, SYMBOLS,
};
pub use math_layout::{
    digits, layout as layout_math, layout_boxed, measure as measure_math, MathBox, MathLayout,
    MathMetrics, MathRun,
};
pub use table::{BorderIter, BorderRun, GridError, TableGrid};
pub use tree::{Icon, Node, NodeKind, Raster, RasterSource, Rect, Style, SurfaceTree, TextRun};
// `Node::Image` carries an `AssetId` and `RasterSource::raster` is keyed by one, so a consumer of
// either needs the type in scope. Re-exported here because `tree` is private and the alternative is
// for every caller to add a dependency on a module it cannot name.
pub use holonomy_text::AssetId;
