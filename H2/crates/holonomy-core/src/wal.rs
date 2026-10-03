//! Write-ahead log.
//!
//! # Why this exists
//!
//! Two requirements collide. `Plan.md` asks for "99.99% document availability and
//! instant auto-saving", and M1 measured that a section is 12KB raw / 7KB
//! zstd-compressed. Rewriting a compressed blob on every keystroke puts zstd on
//! the edit path: at M0's measured 0.4ms cold-parse cost, compressing on every
//! keystroke would dominate the 0.5ms keystroke budget outright.
//!
//! The resolution is the standard one: append the new section state to a WAL on
//! every transaction, and rewrite the section blob on a debounce or a size
//! threshold. A keystroke costs one small row insert. On open, any WAL rows
//! newer than the section's `updated_at` are replayed, so a crash loses at most
//! the unflushed window.
//!
//! This is the one place the plan's "no op-log" instruction needs qualifying. The
//! WAL is a crash-recovery buffer, not history: it is truncated the moment its
//! contents are folded into a section snapshot, and it is never synced or
//! exposed. There is still no version history and no timeline.

use crate::error::Result;
use rusqlite::{params, Connection, OptionalExtension};

/// A section's new state, as appended to the log.
#[derive(Debug, Clone)]
pub struct WalEntry {
    pub document_id: String,
    pub section_id: String,
    /// zstd-compressed section JSON.
    pub payload: Vec<u8>,
    /// Plain text, for FTS5. Kept alongside so a replay does not have to
    /// decompress just to update the search index.
    pub plain_text: String,
    pub word_count: u32,
    pub mark_count: u32,
    pub char_count: u32,
    /// Top-level block count at the moment of the edit.
    ///
    /// Carried rather than recomputed at flush: the payload is already compressed,
    /// so deriving it later would mean decompressing every pending row during a
    /// flush purely to count blocks.
    pub block_count: u32,
}

impl WalEntry {
    pub fn new(
        document_id: impl Into<String>,
        section_id: impl Into<String>,
        payload: Vec<u8>,
        plain_text: impl Into<String>,
        metrics: (u32, u32, u32),
        block_count: u32,
    ) -> Self {
        let (word_count, mark_count, char_count) = metrics;
        Self {
            document_id: document_id.into(),
            section_id: section_id.into(),
            payload,
            plain_text: plain_text.into(),
            word_count,
            mark_count,
            char_count,
            block_count,
        }
    }
}

/// One replayed row.
#[derive(Debug, Clone)]
pub struct ReplayedEntry {
    pub seq: i64,
    pub section_id: String,
    pub payload: Vec<u8>,
    pub plain_text: String,
    pub word_count: u32,
    pub mark_count: u32,
    pub char_count: u32,
    pub block_count: u32,
}

pub struct Wal<'a> {
    conn: &'a Connection,
}

impl<'a> Wal<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Append one section state. This is the hot path: one insert, no compression
    /// of anything other than the payload the caller already produced.
    pub fn append(&self, e: &WalEntry, now: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO wal
                 (document_id, section_id, payload, plain_text,
                  word_count, mark_count, char_count, block_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                e.document_id,
                e.section_id,
                e.payload,
                e.plain_text,
                e.word_count,
                e.mark_count,
                e.char_count,
                e.block_count,
                now,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The newest un-flushed state for each section in a document.
    ///
    /// Only the latest row per section is needed: replaying two states in order
    /// for the same section is wasted work, since the later one wins.
    pub fn pending(&self, document_id: &str) -> Result<Vec<ReplayedEntry>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT seq, section_id, payload, plain_text, word_count, mark_count, char_count,
                    block_count
               FROM wal
              WHERE document_id = ?1
                AND seq = (SELECT MAX(seq) FROM wal w2
                            WHERE w2.document_id = wal.document_id
                              AND w2.section_id = wal.section_id)
              ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![document_id], |row| {
            Ok(ReplayedEntry {
                seq: row.get(0)?,
                section_id: row.get(1)?,
                payload: row.get(2)?,
                plain_text: row.get(3)?,
                word_count: row.get::<_, i64>(4)? as u32,
                mark_count: row.get::<_, i64>(5)? as u32,
                char_count: row.get::<_, i64>(6)? as u32,
                block_count: row.get::<_, i64>(7)? as u32,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// How many rows are pending for a document, for the status indicator.
    pub fn pending_count(&self, document_id: &str) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT section_id) FROM wal WHERE document_id = ?1",
            params![document_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Total rows for a document, for diagnostics.
    pub fn row_count(&self, document_id: &str) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM wal WHERE document_id = ?1",
            params![document_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Drop WAL rows for a section, called once its state is folded into the
    /// section blob. This is what keeps the log a buffer rather than history.
    pub fn checkpoint_section(&self, section_id: &str) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM wal WHERE section_id = ?1", params![section_id])?)
    }

    /// Drop every WAL row for a document, after a full flush.
    pub fn checkpoint_all(&self, document_id: &str) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM wal WHERE document_id = ?1", params![document_id])?)
    }

    /// The largest payload in the pending set, so the caller can decide whether
    /// the debounce window is producing more log than is worth keeping.
    pub fn pending_bytes(&self, document_id: &str) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(payload)), 0) FROM wal WHERE document_id = ?1",
            params![document_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// The newest WAL sequence for a section, if any.
    pub fn latest_for_section(&self, section_id: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT MAX(seq) FROM wal WHERE section_id = ?1",
                params![section_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_conn;

    fn entry(doc: &str, sec: &str, tag: &str) -> WalEntry {
        WalEntry::new(doc, sec, format!("zstd:{tag}").into_bytes(), format!("plain {tag}"), (100, 5, 600), 1)
    }

    #[test]
    fn append_and_replay_round_trips() {
        let conn = temp_conn();
        let wal = Wal::new(&conn);
        wal.append(&entry("d1", "s1", "a"), 1).unwrap();
        let pending = wal.pending("d1").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].payload, b"zstd:a");
        assert_eq!(pending[0].plain_text, "plain a");
    }

    #[test]
    fn only_the_newest_row_per_section_is_replayed() {
        // Two states for one section: the later one wins, so replaying both
        // would be wasted decompression.
        let conn = temp_conn();
        let wal = Wal::new(&conn);
        wal.append(&entry("d1", "s1", "old"), 1).unwrap();
        wal.append(&entry("d1", "s1", "new"), 2).unwrap();
        let pending = wal.pending("d1").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].payload, b"zstd:new");
    }

    #[test]
    fn pending_spans_sections() {
        let conn = temp_conn();
        let wal = Wal::new(&conn);
        wal.append(&entry("d1", "s1", "a"), 1).unwrap();
        wal.append(&entry("d1", "s2", "b"), 2).unwrap();
        wal.append(&entry("d1", "s3", "c"), 3).unwrap();
        let pending = wal.pending("d1").unwrap();
        assert_eq!(pending.len(), 3);
        // Ordered by seq so replay is deterministic.
        let ids: Vec<&str> = pending.iter().map(|p| p.section_id.as_str()).collect();
        assert_eq!(ids, ["s1", "s2", "s3"]);
    }

    #[test]
    fn documents_are_isolated() {
        let conn = temp_conn();
        let wal = Wal::new(&conn);
        wal.append(&entry("d1", "s1", "a"), 1).unwrap();
        wal.append(&entry("d2", "s1", "b"), 2).unwrap();
        assert_eq!(wal.pending("d1").unwrap().len(), 1);
        assert_eq!(wal.pending("d1").unwrap()[0].payload, b"zstd:a");
        assert_eq!(wal.pending("d2").unwrap()[0].payload, b"zstd:b");
    }

    #[test]
    fn checkpoint_clears_a_section() {
        let conn = temp_conn();
        let wal = Wal::new(&conn);
        wal.append(&entry("d1", "s1", "a"), 1).unwrap();
        wal.append(&entry("d1", "s2", "b"), 2).unwrap();
        assert_eq!(wal.pending_count("d1").unwrap(), 2);
        assert_eq!(wal.checkpoint_section("s1").unwrap(), 1);
        assert_eq!(wal.pending_count("d1").unwrap(), 1);
        assert_eq!(wal.pending("d1").unwrap()[0].section_id, "s2");
    }

    #[test]
    fn checkpoint_all_clears_the_document() {
        let conn = temp_conn();
        let wal = Wal::new(&conn);
        wal.append(&entry("d1", "s1", "a"), 1).unwrap();
        wal.append(&entry("d1", "s2", "b"), 2).unwrap();
        wal.append(&entry("d2", "s1", "c"), 3).unwrap();
        assert_eq!(wal.checkpoint_all("d1").unwrap(), 2);
        assert_eq!(wal.row_count("d1").unwrap(), 0);
        // The other document is untouched.
        assert_eq!(wal.row_count("d2").unwrap(), 1);
    }

    #[test]
    fn pending_bytes_tracks_log_size() {
        // This is the number that decides when the debounce must force a flush.
        let conn = temp_conn();
        let wal = Wal::new(&conn);
        assert_eq!(wal.pending_bytes("d1").unwrap(), 0);
        wal.append(&entry("d1", "s1", "12345"), 1).unwrap();
        assert!(wal.pending_bytes("d1").unwrap() > 0);
    }

    #[test]
    fn latest_for_section_reports_the_seq() {
        let conn = temp_conn();
        let wal = Wal::new(&conn);
        assert_eq!(wal.latest_for_section("s1").unwrap(), None);
        let seq = wal.append(&entry("d1", "s1", "a"), 1).unwrap();
        assert_eq!(wal.latest_for_section("s1").unwrap(), Some(seq));
    }
}
