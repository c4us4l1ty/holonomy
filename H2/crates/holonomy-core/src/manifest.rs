//! The section manifest: a document's structure without its content.
//!
//! This is the "99% frozen" layer of `2000.md`'s Iceberg. Reading a manifest for
//! a 2000-page document is ~50KB and touches no blobs, which is what makes the
//! scrollbar, the outline, and word counts all cheap.

use crate::error::{Error, Result};
use crate::order;
use crate::split::SectionMetrics;
use serde::{Deserialize, Serialize};

/// A document row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One row of the manifest. Deliberately carries no content: this is what you
/// hold in memory for the whole document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub id: String,
    /// Fractional index. `u64` with gaps rather than a digit string, because
    /// bare fractional digits are not a total order — see [`crate::order`].
    pub order_key: order::OrderKey,
    pub title: Option<String>,
    pub word_count: u32,
    pub mark_count: u32,
    pub char_count: u32,
    /// Number of top-level blocks in this section.
    ///
    /// Carried for the scroll geometry, and only because measurement showed it is
    /// not derivable. An 8000-character section that is one paragraph renders
    /// 2070px; the same characters as ten paragraphs render 2373px. Estimating
    /// block count from characters at any fixed density measured at 225% error on
    /// short dense sections and 41% on long sparse ones, and no density fixes it
    /// because the paragraph ratio spans 1 to 20. The full table is on
    /// [`crate::geometry`].
    ///
    /// Cost is one integer per section, so ~4KB across a 2000-page manifest.
    /// Cheap against a scrollbar that is 40% wrong without it.
    pub block_count: u32,
    pub created_at: i64,
    pub updated_at: i64,
}

impl ManifestEntry {
    /// The split-driving metrics for this section.
    pub fn metrics(&self) -> SectionMetrics {
        SectionMetrics::new(self.word_count, self.mark_count, self.char_count)
    }
}

/// Metrics snapshot used to report a whole document cheaply.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ManifestTotals {
    pub sections: u32,
    pub words: u64,
    pub marks: u64,
    pub chars: u64,
}

/// An in-memory manifest, ordered by `order_key`.
///
/// Kept sorted on insert so callers never have to sort, and so the neighbour
/// lookups the viewport needs (`prev`, `next`, `range`) are index arithmetic.
#[derive(Debug, Clone, Default)]
pub struct Manifest {
    document_id: String,
    entries: Vec<ManifestEntry>,
}

impl Manifest {
    pub fn new(document_id: impl Into<String>) -> Self {
        Self { document_id: document_id.into(), entries: Vec::new() }
    }

    pub fn document_id(&self) -> &str {
        &self.document_id
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[ManifestEntry] {
        &self.entries
    }

    pub fn get(&self, index: usize) -> Option<&ManifestEntry> {
        self.entries.get(index)
    }

    pub fn by_id(&self, id: &str) -> Option<(usize, &ManifestEntry)> {
        self.entries.iter().enumerate().find(|(_, e)| e.id == id)
    }

    /// Append a section, allocating a fresh key.
    pub fn push(&mut self, mut entry: ManifestEntry) {
        let last = self.entries.last().map(|e| e.order_key.0);
        entry.order_key = order::OrderKey(order::append(last));
        self.entries.push(entry);
    }

    /// Insert a section at a specific index, shifting the rest.
    ///
    /// Only the moved entry gets a new order key; the others keep theirs. This
    /// is why keys are allocated with gaps instead of renumbered: with 467
    /// sections, renumbering would rewrite 467 rows on every insert, and on a
    /// sync conflict two devices would fight over the same keys.
    ///
    /// Returns an error when the gap at `index` has closed and the manifest
    /// needs a rebalance. Callers should treat that as expected, not fatal.
    pub fn insert_at(&mut self, index: usize, mut entry: ManifestEntry) -> Result<()> {
        if index > self.entries.len() {
            return Err(Error::Other(anyhow::anyhow!(
                "insert index {index} out of range for {} sections",
                self.entries.len()
            )));
        }
        let keys: Vec<u64> = self.entries.iter().map(|e| e.order_key.0).collect();
        entry.order_key = order::OrderKey(order::key_for_insert(index, &keys)?);
        self.entries.insert(index, entry);
        Ok(())
    }

    /// Renumber every key with fresh gaps, preserving section order.
    ///
    /// Needed when an insert reports that the gap has closed. Cheap: the
    /// manifest is a few hundred rows.
    pub fn rebalance(&mut self) -> Result<()> {
        let keys: Vec<u64> = self.entries.iter().map(|e| e.order_key.0).collect();
        let fresh = order::rebalance_existing(&keys).ok_or_else(|| {
            Error::Other(anyhow::anyhow!(
                "manifest order keys are not strictly increasing; rebalancing cannot \
                 establish an order from them"
            ))
        })?;
        for (e, k) in self.entries.iter_mut().zip(fresh) {
            e.order_key = order::OrderKey(k);
        }
        Ok(())
    }

    /// Insert, rebalancing and retrying once if the gap has closed.
    ///
    /// This is the call the UI should use: a rebalance is invisible and takes
    /// well under a millisecond, so handling it here rather than surfacing an
    /// error keeps reorder simple at the call site.
    pub fn insert_at_or_rebalance(&mut self, index: usize, entry: ManifestEntry) -> Result<()> {
        match self.insert_at(index, entry.clone()) {
            Ok(()) => Ok(()),
            Err(e) if e.to_string().contains("rebalanc") => {
                self.rebalance()?;
                self.insert_at(index, entry)
            }
            Err(e) => Err(e),
        }
    }

    /// Remove a section, returning it.
    pub fn remove(&mut self, id: &str) -> Option<ManifestEntry> {
        let idx = self.entries.iter().position(|e| e.id == id)?;
        Some(self.entries.remove(idx))
    }

    /// Update the metrics of a section in place.
    pub fn update_metrics(&mut self, id: &str, m: SectionMetrics, updated_at: i64) -> bool {
        self.update_metrics_with_blocks(id, m, None, updated_at)
    }

    /// Update metrics, including the block count the geometry needs.
    ///
    /// `block_count` is `Option` because not every caller can know it. Passing
    /// `None` leaves the existing value alone rather than writing zero: the column
    /// is the difference between a scrollbar within ~5% and one within 225%, so
    /// defaulting it to zero on an incomplete update would silently destroy the
    /// estimate for a section the caller merely touched.
    pub fn update_metrics_with_blocks(
        &mut self,
        id: &str,
        m: SectionMetrics,
        block_count: Option<u32>,
        updated_at: i64,
    ) -> bool {
        match self.entries.iter_mut().find(|e| e.id == id) {
            Some(e) => {
                e.word_count = m.words;
                e.mark_count = m.marks;
                e.char_count = m.chars;
                // Only overwritten when the caller knows it. See the doc comment.
                if let Some(b) = block_count {
                    e.block_count = b;
                }
                e.updated_at = updated_at;
                true
            }
            None => false,
        }
    }

    /// Indices of sections that exceed a split threshold.
    pub fn sections_needing_split(&self) -> Vec<(usize, crate::split::SplitReason)> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| crate::split::should_split(e.metrics()).map(|r| (i, r)))
            .collect()
    }

    pub fn totals(&self) -> ManifestTotals {
        ManifestTotals {
            sections: self.entries.len() as u32,
            words: self.entries.iter().map(|e| e.word_count as u64).sum(),
            marks: self.entries.iter().map(|e| e.mark_count as u64).sum(),
            chars: self.entries.iter().map(|e| e.char_count as u64).sum(),
        }
    }

    /// The index of the section containing word offset `word_offset`.
    ///
    /// This is the lookup behind "jump to page N" and behind a search hit. It
    /// is a prefix sum over `word_count`, computed on demand from the manifest,
    /// which is small enough that caching the tree is not worth the
    /// invalidation. For 467 sections this is a linear scan of a `Vec<u32>`,
    /// well under a microsecond.
    pub fn section_at_word_offset(&self, word_offset: u64) -> Option<usize> {
        let mut acc = 0u64;
        for (i, e) in self.entries.iter().enumerate() {
            let next = acc + e.word_count as u64;
            if word_offset < next {
                return Some(i);
            }
            acc = next;
        }
        if self.entries.is_empty() {
            None
        } else {
            Some(self.entries.len() - 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split::SplitReason;

    fn entry(id: &str, words: u32, marks: u32) -> ManifestEntry {
        ManifestEntry {
            id: id.into(),
            order_key: order::OrderKey(0),
            title: None,
            word_count: words,
            mark_count: marks,
            char_count: words * 6,
            block_count: 1,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn keys(m: &Manifest) -> Vec<u64> {
        m.entries().iter().map(|e| e.order_key.0).collect()
    }

    fn ids(m: &Manifest) -> Vec<&str> {
        m.entries().iter().map(|e| e.id.as_str()).collect()
    }

    #[test]
    fn push_assigns_increasing_keys() {
        let mut m = Manifest::new("doc");
        for i in 0..50 {
            m.push(entry(&format!("s{i}"), 100, 10));
        }
        let k = keys(&m);
        assert!(k.windows(2).all(|w| w[0] < w[1]), "order keys must increase");
    }

    #[test]
    fn insert_at_keeps_order() {
        let mut m = Manifest::new("doc");
        m.push(entry("a", 100, 0));
        m.push(entry("c", 100, 0));
        m.insert_at(1, entry("b", 100, 0)).unwrap();
        assert_eq!(ids(&m), ["a", "b", "c"]);
        assert!(keys(&m).windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn insert_at_head_and_tail() {
        let mut m = Manifest::new("doc");
        m.push(entry("b", 100, 0));
        m.insert_at(0, entry("a", 100, 0)).unwrap();
        m.push(entry("c", 100, 0));
        assert_eq!(ids(&m), ["a", "b", "c"]);
    }

    #[test]
    fn many_inserts_at_same_point_stay_ordered() {
        // The sync-conflict case: two devices inserting at the same position.
        // Uses the rebalancing variant, which is what the UI calls, so this
        // asserts the end-to-end behaviour rather than the gap arithmetic.
        let mut m = Manifest::new("doc");
        m.push(entry("first", 100, 0));
        m.push(entry("last", 100, 0));
        for i in 0..40 {
            m.insert_at_or_rebalance(1, entry(&format!("mid{i}"), 100, 0)).unwrap();
        }
        assert!(keys(&m).windows(2).all(|w| w[0] < w[1]), "40 inserts at one point");
        assert_eq!(m.len(), 42);
        assert_eq!(m.get(0).unwrap().id, "first");
        assert_eq!(m.get(41).unwrap().id, "last");
    }

    #[test]
    fn rebalance_preserves_section_order() {
        let mut m = Manifest::new("doc");
        for i in 0..10 {
            m.push(entry(&format!("s{i}"), 100, 0));
        }
        let before: Vec<String> = m.entries().iter().map(|e| e.id.clone()).collect();
        // Collapse every gap to exactly 1, so no insert is possible anywhere and
        // a rebalance is the only way forward.
        let start = keys(&m)[0];
        for (i, e) in m.entries.iter_mut().enumerate() {
            e.order_key = order::OrderKey(start + i as u64);
        }
        assert!(m.insert_at(1, entry("x", 1, 0)).is_err());
        m.insert_at_or_rebalance(1, entry("x", 1, 0)).unwrap();
        let after = ids(&m);
        assert_eq!(after[0], before[0], "order must survive rebalancing");
        assert_eq!(after[10], before[9], "and so must the tail");
        assert_eq!(after[1], "x");
        assert!(keys(&m).windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn word_offset_lookup_finds_the_right_section() {
        let mut m = Manifest::new("doc");
        m.push(entry("a", 100, 0));
        m.push(entry("b", 200, 0));
        m.push(entry("c", 300, 0));
        assert_eq!(m.section_at_word_offset(0), Some(0));
        assert_eq!(m.section_at_word_offset(99), Some(0));
        assert_eq!(m.section_at_word_offset(100), Some(1));
        assert_eq!(m.section_at_word_offset(299), Some(1));
        assert_eq!(m.section_at_word_offset(300), Some(2));
        // Past the end clamps to the last section.
        assert_eq!(m.section_at_word_offset(10_000), Some(2));
    }

    #[test]
    fn split_candidates_include_mark_overflow() {
        // The case 2000.md's page-count-only split rule would miss: short but
        // heavily formatted.
        let mut m = Manifest::new("doc");
        m.push(entry("ok", 100, 10));
        m.push(entry("wordy", 2000, 10));
        m.push(entry("inky", 200, 4000));
        let candidates = m.sections_needing_split();
        assert_eq!(candidates.len(), 2);
        assert!(matches!(candidates[0].1, SplitReason::TooManyWords { .. }));
        assert!(matches!(candidates[1].1, SplitReason::TooManyMarks { .. }));
    }

    #[test]
    fn totals_aggregate() {
        let mut m = Manifest::new("doc");
        m.push(entry("a", 100, 5));
        m.push(entry("b", 250, 7));
        let t = m.totals();
        assert_eq!(t.sections, 2);
        assert_eq!(t.words, 350);
        assert_eq!(t.marks, 12);
    }

    #[test]
    fn manifest_of_2000_pages_is_small() {
        // The claim 2000.md makes: the whole document is a tiny manifest that
        // can be held in memory and read without touching a single blob.
        //
        // 2000 pages / ~4.7 pages per section (1500 words) is ~426 sections;
        // 667 is used as a conservative upper bound.
        let mut m = Manifest::new("doc");
        for i in 0..667 {
            m.push(entry(&format!("s{i}"), 1500, 300));
        }
        let json = serde_json::to_string(m.entries()).unwrap();
        // ~460 bytes per entry, which is dominated by the ULID section id and
        // the JSON field names. In SQLite the row is far smaller because there
        // are no field names, and the point of the manifest is that it is
        // orders of magnitude below the 6MB of section blobs.
        assert!(
            json.len() < 400_000,
            "manifest for a 2000-page document is {} bytes, expected well under 400KB",
            json.len()
        );
        // The important comparison: manifest versus content.
        let content_bytes = 667 * 7000; // M1 measured ~7KB compressed per section
        assert!(
            json.len() * 10 < content_bytes,
            "manifest ({}) should be far smaller than content ({})",
            json.len(),
            content_bytes
        );
    }
}
