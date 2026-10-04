//! Fenwick line geometry and font-metric line heights.
//!
//! FR-1.3. Two Fenwick trees over a document's lines -- one over pixel heights, one over byte
//! lengths -- so that "which line is at pixel Y" and "which line contains byte B" are both O(log N)
//! and neither needs a rendered glyph measured.
//!
//! Lands in Phase 6 with the rest of the text engine. See PROJECT.md §5 Phase 6, Plan.md §2.1
//! FR-1.3, and `H2/crates/holonomy-core/src/geometry.rs` for the tree this is ported from.
//!
//! # The four modules, and what each is for
//!
//! * [`Fenwick`] -- prefix sums and inverse lookups over non-negative `u32` weights. O(log N) both
//!   ways, no division, no allocation.
//! * [`FontMetrics`] -- line height from the font's own `hhea` table. Integer arithmetic, so the
//!   tree's weights are exact and `lower_bound` and `prefix` cannot disagree.
//! * [`LineGeometry`] -- the two trees plus per-line metrics, and the damage rect a keystroke needs.
//! * [`DamageRect`] -- which pixels must be repainted. Owned here rather than in `holonomy-render`
//!   because [`LineGeometry::damage_rect_for`] produces it, and a keystroke's damage rect crossing a
//!   crate boundary as two structurally identical types would let the two drift apart.
//!
//! # What is ported from H2, and what is not
//!
//! Ported: the Fenwick tree, `lower_bound`, the offset/position inverse pair, `visible_range`,
//! `scroll_compensation`, and the structural-change invariants.
//!
//! Not ported: H2's height *estimation* -- `estimate_paragraphs`, the block-count table, the CSS
//! chrome calibration, and the 20 tests that exercise them. H2 must estimate because its heights come
//! from `getBoundingClientRect` on content that has not been mounted. H1's heights come from
//! [`FontMetrics`] and are known before anything is drawn, so there is nothing to estimate and the
//! entire subsystem is a solution to a problem H1 does not have. `tests/h2_port.rs` lists all 20 by
//! name with the reason each is inapplicable, and the test that would otherwise have covered the
//! shared behaviour is
//! [`deterministic_heights_need_no_estimation`](tests::deterministic_heights_need_no_estimation).

mod fenwick;
mod fontmetrics;
mod lines;

pub use fenwick::Fenwick;
pub use fontmetrics::FontMetrics;
pub use lines::{DamageRect, GeometryError, HeightUpdate, LineGeometry, LineMetrics};
