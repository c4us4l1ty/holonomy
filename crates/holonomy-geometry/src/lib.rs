//! Fenwick-indexed geometry: line heights from font metrics, never from measurement.
//!
//! Two trees (PRD FR-1.3): one over vertical line heights, one over byte prefix sums.
//! Both answer "which line is at pixel Y" in O(log n) with zero DOM and zero glyph
//! measurement. `Fenwick` is ported from H2 with `f64` weights replaced by `u32`, which
//! removes the ulp-disagreement that forced H2 into an O(log² n) `lower_bound`.
//!
//! Lands in Phase 6. Gate: H2's 36 `geometry.rs` tests plus the 4 `Fenwick` tests,
//! re-targeted at the new tree. See PROJECT.md §3 and §5 Phase 6.

pub use holonomy_jail::PHASE_0_PLACEHOLDER;
