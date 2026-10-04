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

pub mod chrome;
pub mod table;

pub use chrome::{
    AscendingRun, Blink, Caret, Chrome, ChromeMetrics, ChromeState, Layout, StyleFlags,
    StyleFlagsSlot,
};
pub use damage::{DamageRect, DamageTracker};
pub use table::{BorderIter, BorderRun, GridError, TableGrid};
pub use tree::{Icon, Node, NodeKind, Rect, Style, SurfaceTree, TextRun};
