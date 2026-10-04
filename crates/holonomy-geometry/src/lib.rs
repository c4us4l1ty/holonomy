//! Fenwick line geometry and font-metric line heights.
//!
//! FR-1.3. Two Fenwick trees over a document's lines -- one over pixel heights, one over byte
//! lengths -- so that "which line is at pixel Y" and "which line contains byte B" are both O(log N)
//! and neither needs a rendered glyph measured.
//!
//! Lands in Phase 6 with the rest of the text engine. See PROJECT.md §5 Phase 6, Plan.md §2.1
//! FR-1.3, and `H2/crates/holonomy-core/src/geometry.rs` for the tree this is ported from.

mod fenwick;
mod lines;

pub use fenwick::Fenwick;
pub use lines::{DamageRect, GeometryError, LineGeometry, LineMetrics};
