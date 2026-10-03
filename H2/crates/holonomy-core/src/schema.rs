//! SQLite schema for Holonomy.
//!
//! Shape follows `2000.md` §2 with two changes that the M0/M1 spikes forced:
//!
//! 1. **A write-ahead log.** `2000.md` §7 proposes last-writer-wins section sync
//!    with no history, and `Plan.md` asks for "99.99% document availability and
//!    instant auto-saving". Those two pull in opposite directions: a debounced
//!    snapshot write loses everything since the last flush, and rewriting a
//!    compressed section blob on every keystroke puts zstd on the edit path.
//!    The WAL is small and append-only, so a keystroke costs one row insert, and
//!    section snapshots are written on a debounce.
//!
//! 2. **Mark accounting on the section row.** M1b established that a styled read
//!    is linear in mark count and crosses a 5ms budget at ~5,500 marks. The
//!    section splitter needs that number, so it is maintained as we go rather
//!    than recomputed by scanning the CRDT.
//!
//! Deliberately absent: no `version_history`, no `timeline`, no op-log table.
//! Snapshots replace state in place; the WAL is a crash-recovery buffer that is
//! truncated once its contents are folded into a snapshot.
//!
//! Also absent: no `undo_log`. An earlier revision had one, storing a
//! pre-edit section snapshot per undo group so undo was "restore the previous
//! snapshot". M3 implemented undo instead as in-memory ProseMirror steps, which
//! is smaller, exact, and global across sections without any extra schema. Since
//! undo state is not persisted across sessions (confirmed as intended), the table
//! had no writer and no reader. Dead schema is worse than no schema: it implies
//! a capability that does not exist.

/// Bumped whenever the layout changes; stored in `meta` and checked on open.
///
/// Raising this does **not** invalidate a file. [`MIGRATIONS`] is an ordered list and
/// `Store::init` applies the tail of it to an older file, so a v1 file is migrated in
/// place on open rather than refused. That was not true before version 2: `init` accepted
/// either "no meta table" or "exactly this version", which made the first schema change
/// since the format landed an automatic "your document is unreadable" for every user.
pub const SCHEMA_VERSION: i64 = 2;

pub const MIGRATIONS: &[&str] = &[MIGRATION_1, MIGRATION_2];

const MIGRATION_1: &str = r#"
-- One row per document. `title` is the document name shown in the UI.
CREATE TABLE documents (
    id          TEXT PRIMARY KEY NOT NULL,
    title       TEXT NOT NULL DEFAULT 'Untitled',
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;

-- The manifest. This table is the entire 2000-page document as far as anything
-- outside the focused window is concerned: ~100 bytes per section, so a
-- 2000-page document is ~50KB. Everything else is a blob we do not read.
--
-- `order_key` is an INTEGER with gaps, not a TEXT fractional index. See
-- `order.rs`: bare fractional digit strings are not a total order, and a u64
-- with a 1024 gap gives ~10 levels of midpoint insertion before a cheap
-- rebalance. INTEGER also sorts correctly under SQLite's BINARY collation,
-- which is what the index below relies on.
--
-- `content_zstd` holds zstd-compressed ProseMirror JSON, NOT a Loro snapshot.
-- Rationale (M1): Loro is used for cross-device sync, but local persistence is
-- plain JSON because a section is snapshotted and re-read constantly, and
-- JSON.parse of a 24KB section is far cheaper than reconstructing a CRDT. The
-- CRDT state is only materialised when a section is being synced.
--
-- `mark_count` and `word_count` drive the split thresholds established by M0
-- (words) and M1b (marks).
--
-- `block_count` exists for the scroll geometry, not for splitting. Layout height
-- depends far more on block structure than on character count: measured in
-- Chromium, an 8000-character section that is one paragraph renders 2070px and
-- the same characters as ten paragraphs render 2373px. Estimating block count
-- from characters at any fixed density measured at 225% error on short dense
-- sections and 41% on long sparse ones, and no density fixes it, because the
-- paragraph ratio spans 1 to 20. One integer per section is cheap against a
-- scrollbar that is 40% wrong without it.
CREATE TABLE sections (
    id            TEXT PRIMARY KEY NOT NULL,
    document_id   TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    order_key     INTEGER NOT NULL,
    title         TEXT,
    content_zstd  BLOB NOT NULL,
    plain_text    TEXT,
    word_count    INTEGER NOT NULL DEFAULT 0,
    mark_count    INTEGER NOT NULL DEFAULT 0,
    char_count    INTEGER NOT NULL DEFAULT 0,
    block_count   INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
) STRICT;

-- Manifest reads are always "all sections of this document, in order".
CREATE INDEX sections_doc_order ON sections(document_id, order_key);

-- Content-addressed assets (images, rendered equation SVGs). Never inline
-- base64 in the section JSON: a 2000-page document with figures would otherwise
-- be mostly base64, and every section read would pay to decode them.
--
-- Keyed by the hash of the bytes, so identical images are stored once and a
-- section can be synced to a device that already has the asset.
CREATE TABLE assets (
    hash       TEXT PRIMARY KEY NOT NULL,
    mime       TEXT NOT NULL,
    byte_size  INTEGER NOT NULL,
    bytes      BLOB NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

-- Write-ahead log. Appended on every transaction, truncated after the section
-- snapshot that supersedes it. `seq` is monotonic per document so replay is
-- ordered and idempotent.
--
-- This is what makes autosave durable without putting compression on the
-- keystroke path: a keystroke appends one small row, and the section blob is
-- rewritten on a debounce or on a threshold.
CREATE TABLE wal (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    document_id TEXT NOT NULL,
    section_id  TEXT NOT NULL,
    payload     BLOB NOT NULL,     -- zstd-compressed section JSON
    plain_text  TEXT,
    word_count  INTEGER NOT NULL DEFAULT 0,
    mark_count  INTEGER NOT NULL DEFAULT 0,
    char_count  INTEGER NOT NULL DEFAULT 0,
    -- Mirrors sections.block_count. A WAL row is a point-in-time snapshot of the
    -- section's metrics, so replaying it has to restore the block count too or
    -- the geometry would be wrong for every section touched during a crash.
    block_count INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL
) STRICT;

CREATE INDEX wal_doc_seq ON wal(document_id, seq);
CREATE INDEX wal_section ON wal(section_id, seq);

-- Schema bookkeeping.
CREATE TABLE meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;

-- Full-text search over section plain text.
--
-- External-content FTS5: the index references `sections` rather than
-- duplicating it, so a section update does not need two writes kept in sync.
-- Native browser find is not an option here because it only sees mounted DOM
-- and would miss ~99% of a 2000-page document (2000.md section 4).
--
-- `document_id` is stored unindexed-but-searchable via a `title`-like column so
-- results can be filtered to one document without a join back to sections.
CREATE VIRTUAL TABLE sections_fts USING fts5(
    plain_text,
    document_id UNINDEXED,
    content='sections',
    content_rowid='rowid'
);
"#;

/// The search index, rebuilt as a **contentless** FTS5 table.
///
/// # Why version 1's `content='sections'` had to go
///
/// An external-content index stores no text of its own: a row's text is read back out of
/// `sections` when needed, and the index holds only a *snapshot* of what was indexed at
/// the time. That is the problem. Deleting a row from such an index requires supplying the
/// **exact original column values**, via `INSERT INTO sections_fts(sections_fts, ...)
/// VALUES('delete', ...)`, and the original value is a thing the database no longer knows:
/// the moment a section is edited, `sections.plain_text` and the indexed copy diverge, and
/// there is no column anywhere holding the indexed copy. So the two ways to keep an
/// external-content index in sync are both unavailable --
///
/// - pass the current text as the original, which silently no-ops after any edit and
///   leaves the superseded text findable forever, or
/// - call `'rebuild'`, which re-derives the entire index from every section on every
///   keystroke. On a 1,300-section document that is a million words re-tokenised per edit.
///
/// A third option is to never call `DELETE` at all and rely on `'rebuild'` alone, which is
/// what version 1 did -- and which meant the only way to index an edit was to reindex the
/// whole document. That is why `reindex` had no caller in the application: it was not
/// callable per-keystroke, so it was never called at all, and search was green in the test
/// suite and empty in every real session.
///
/// # What replaces it
///
/// `content=''` with `contentless_delete=1`. The table stores its own text, so:
///
/// - `DELETE FROM sections_fts WHERE rowid = ?` is a genuine index-row delete, needing
///   nothing but the rowid, and
/// - `INSERT INTO sections_fts(rowid, plain_text, document_id) VALUES(?, ?, ?)` re-adds it.
///
/// An edit is therefore a two-statement, per-section operation with no full rebuild, and it
/// cannot drift: there is no second copy of the text to fall out of step, because this copy
/// is the only copy the index consults.
///
/// `contentless_delete=1` is required and is not the default. Without it, SQLite refuses
/// outright -- `cannot DELETE from contentless fts5 table` -- because a contentless table
/// has no column values to check a delete against. The option landed in FTS5 3.43; the
/// bundled SQLite is 3.46. Verified rather than assumed: a bare `content=''` table on this
/// same SQLite rejects the delete, and adding the option accepts it.
///
/// # What is given up
///
/// Two things, both paid for deliberately.
///
/// - **`snippet()` is unavailable.** It reads the matched column's stored text, and a
///   contentless table has none to read. `Store::search` therefore builds the snippet from
///   the section it already joins to. That is a decompression per returned hit, bounded by
///   the `limit`, against a query that would otherwise be one index walk.
/// - **`'rebuild'` is unavailable**, for the same reason. `Store::reindex` becomes an
///   explicit delete-and-repopulate, which is *more* work than a rebuild but is verifiable
///   by `SELECT COUNT(*)`, where a rebuild is not.
///
///
/// **No `document_id` column, deliberately.** It was `UNINDEXED` before, which is the
/// only way to store a column without indexing it -- and on a contentless table neither
/// form is usable. `UNINDEXED` stores nothing to filter on; making it an indexed column
/// does not help either, because a contentless table builds no column index for values it
/// does not hold. Measured both against the bundled SQLite rather than assumed: with
/// `document_id UNINDEXED`, `SELECT COUNT(*) FROM sections_fts WHERE document_id = ?`
/// returns 0 for a table that visibly holds the row, and the same is true with the column
/// indexed. `DELETE ... WHERE document_id = ?` silently matches nothing in both shapes,
/// which is worse than the count being wrong.
///
/// So the index is only *text keyed by section rowid*, and every question of which
/// document a row belongs to is answered by joining `sections`, which already has
/// `document_id` in an ordinary B-tree index. That join is the shape the `search` query
/// needed anyway, and it means the index holds exactly one thing -- the text -- with
/// exactly one thing as its key. There is no second copy of anything to fall out of step.
/// The stored text is duplicated -- once zstd-compressed in `sections`, once plain here.
/// That is what a search index is, and it buys the two properties above.
const MIGRATION_2: &str = r#"
DROP TABLE IF EXISTS sections_fts;

CREATE VIRTUAL TABLE sections_fts USING fts5(
    plain_text,
    content='',
    contentless_delete=1
);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_ordered_and_versioned() {
        assert_eq!(MIGRATIONS.len(), SCHEMA_VERSION as usize);
        for (i, m) in MIGRATIONS.iter().enumerate() {
            assert!(!m.trim().is_empty(), "migration {i} is empty");
        }
    }

    #[test]
    fn no_history_tables() {
        // The plan explicitly drops version history and timelines. Guard
        // against a future migration reintroducing them.
        let all = MIGRATIONS.join("\n").to_lowercase();
        for forbidden in ["version_history", "timeline", "op_log", "revisions"] {
            assert!(
                !all.contains(forbidden),
                "schema must not contain a `{forbidden}` table"
            );
        }
    }

    #[test]
    fn split_driving_columns_exist() {
        // M0 bounded rendering cost by word count; M1b bounded CRDT read cost by
        // mark count. Both columns must exist for the splitter.
        let all = MIGRATIONS.join("\n");
        assert!(all.contains("word_count"));
        assert!(all.contains("mark_count"));
    }
}
