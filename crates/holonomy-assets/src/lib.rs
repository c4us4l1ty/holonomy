//! Embeddable assets: brotli-compressed TrueType faces, the A8 glyph atlas, icon masks.
//!
//! `build.rs` brotli-compresses three committed subset TTFs (Inter Regular, Inter
//! SemiBold, JetBrains Mono) at quality 11 and `include_bytes!`s the result — 67,070 B
//! total, measured. Subsetting is a one-time human step via system HarfBuzz, so the
//! build needs no HarfBuzz and the binary never queries a system font path (FR-2.1).
//!
//! This crate cannot be a dependency of anything else: `include_bytes!` data is only
//! visible to the crate that declares it. `holonomy` re-exports.
//!
//! Lands in Phase 4. Gate: byte-identical brotli round trip, atlas ≤ 512 KiB, every
//! codepoint the UI draws present in all three faces. See PROJECT.md §2.1 and §5
//! Phase 4.

pub use holonomy_jail::PHASE_0_PLACEHOLDER;
