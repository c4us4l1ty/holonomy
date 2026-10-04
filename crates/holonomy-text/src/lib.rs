//! The CAGR text engine: gap-rope leaves, the style interval map, undo and search.
//!
//! Leaves are 4096-byte `SecureBlock`s, 64-byte aligned, carrying `gap_start` / `gap_end` /
//! `text_len`. Typing at the cursor writes into the gap in O(1) with no heap allocation; deletion
//! moves the boundary and scrubs the byte. Style spans live in a parallel interval map of the
//! PRD's 16-byte `TextIntervalSpan`, so the UTF-8 buffer stays contiguous and never carries
//! formatting metadata.
//!
//! The gap rope is destructive by design — that is the point. A CRDT would keep the deletion as a
//! tombstone, and Plan.md Part 2 is unambiguous that recoverable edit history is a forensic
//! liability, not a feature.
//!
//! Lands in Phase 6. Gate: insert/delete O(1) with zero allocations, asserted by a counting
//! global allocator. See PROJECT.md §5 Phase 6 and PRD §7.1.

mod leaf;
mod rope;

pub use leaf::{CagrLeaf, LeafError, CACHELINE_BYTES, GAP_MINIMUM, GAP_TARGET, LEAF_CAPACITY};
pub use rope::{Rope, RopeError};
