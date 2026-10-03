//! Holonomy storage core.
//!
//! The document model, established by the M0 and M1 spikes:
//!
//! * A document is a **manifest of sections**. Only the focused section and its
//!   neighbours are ever materialised; the rest are rows and blobs.
//! * A section is roughly **1500 words** (M0 rendering knee) and is capped at
//!   **3000 marks** (M1b CRDT read ceiling). Both limits are enforced by
//!   [`split`].
//! * Local persistence is **zstd-compressed ProseMirror JSON**, not a Loro
//!   snapshot. Loro is for cross-device sync; re-reading a section on every
//!   focus change is far cheaper as `JSON.parse` than as a CRDT load, and M1
//!   measured snapshot import at 0.076ms p50 which leaves room but not for a
//!   hot path we do not need.
//! * Writes go through a **write-ahead log** so a keystroke is one small row
//!   insert rather than a compressed blob rewrite, which is what makes instant
//!   autosave compatible with the 99.99% availability goal.
//! * Scroll geometry is a **Fenwick tree over section heights** in
//!   [`geometry`], which is what lets the scrollbar know the document is 30
//!   million pixels tall when almost none of it is in the DOM.

pub mod error;
pub mod asset_gc;
pub mod geometry;
pub mod holo;
pub mod manifest;
pub mod order;
pub mod schema;
pub mod split;
pub mod store;

#[cfg(test)]
mod test_support;
pub mod wal;

pub use error::{Error, Result};
pub use geometry::{Fenwick, Geometry, GeometryCalibration, HeightUpdate, SectionGeometry};
pub use manifest::{Document, Manifest, ManifestEntry, ManifestTotals};
pub use split::{
    should_split, MarkCostModel, SectionMetrics, SplitReason, MAX_MARKS_PER_SECTION,
    MAX_WORDS_PER_SECTION, STYLED_READ_BUDGET_MS,
};
pub use holo::{FileKind, FILE_EXTENSION, MIME};
pub use store::{analyze, Analyzed, Store};

/// The URL scheme asset bytes are served under.
///
/// # Why this is in core rather than in the shell
///
/// The scheme is part of the *document format*: an image node's `src` is
/// `holo-asset://<sha256>`, and that string is persisted in section JSON and travels to
/// other devices with the document. So the frontend needs the scheme to build a URL and
/// the shell needs it to register a handler, and if they were defined separately they
/// would agree right up until a document written by one was opened by the other.
pub const ASSET_SCHEME: &str = "holo-asset";

/// Lowercase hex SHA-256 of `bytes`.
///
/// # Why this is exposed rather than left inside `put_asset`
///
/// Because the frontend has to compute the same digest to verify what the protocol handler
/// returned, and a test has to compute it to prove the store's key really is SHA-256
/// rather than whatever it was last changed to. Two implementations of a hash are two
/// chances to disagree, and this is the one place they can be made to agree by
/// construction.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Current time in milliseconds since the Unix epoch.
///
/// Sections and WAL rows are ordered and de-duplicated by time, so this needs
/// to be monotonic within a process. `SystemTime` is not guaranteed monotonic
/// across NTP adjustments, which could reorder two sections edited in the same
/// millisecond.
pub fn now_ms() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static LAST: AtomicI64 = AtomicI64::new(0);
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    // Clamp to be non-decreasing.
    LAST.fetch_max(wall, Ordering::SeqCst).max(wall)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_non_decreasing() {
        let mut prev = 0;
        for _ in 0..1000 {
            let t = now_ms();
            assert!(t >= prev, "time went backwards: {t} < {prev}");
            prev = t;
        }
    }
}
