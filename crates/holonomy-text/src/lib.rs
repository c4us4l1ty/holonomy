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

mod editor;
mod leaf;
mod rope;
mod span;
pub mod table;
pub mod tables;
pub mod undo;

pub use editor::{EditOutcome, Editor, EditorError, STYLE_UNDO_DEPTH};
pub use span::{
    SpanError, SpanMap, SpanPolicy, TextIntervalSpan, STYLE_BOLD, STYLE_CODE, STYLE_HEADER,
    STYLE_ITALIC,
};
pub use table::{Cell, ResolvedTable, TableError, TableSpan, CELL_SEPARATOR};
pub use tables::{
    appended_row_bytes, col_widths_for, down, empty_table_bytes, left, right, shift_tab, tab, up,
    Nav, TableCursor, TableMap, TableMapError,
};
pub use undo::{ActionKind, UndoAction, UndoError, UndoStack, ARENA_BYTES, UNDO_DEPTH};

pub use leaf::{CagrLeaf, LeafError, CACHELINE_BYTES, GAP_MINIMUM, GAP_TARGET, LEAF_CAPACITY};
pub use rope::{Rope, RopeError};
