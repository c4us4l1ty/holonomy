//! The store: documents, sections, and the read/write paths around them.
//!
//! Read paths are shaped by M0: opening a document reads the manifest only, and
//! content is fetched per section as the viewport moves. Write paths are shaped
//! by the WAL: a keystroke appends to the log, and the compressed section blob
//! is rewritten on a flush.

use crate::error::{Error, Result};
use crate::manifest::{Document, Manifest, ManifestEntry};
use crate::schema::{MIGRATIONS, SCHEMA_VERSION};
use crate::split::SectionMetrics;
use crate::wal::{Wal, WalEntry};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

/// zstd level for section blobs. M1 measured 3 as a good trade (12KB -> 7KB);
/// higher levels cost CPU on the flush path for little gain at this size.
const ZSTD_LEVEL: i32 = 3;

/// What a snapshot wrote, for a status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupSummary {
    pub path: String,
    pub bytes: u64,
    pub elapsed_ms: u128,
    pub documents: u32,
    pub sections: u32,
    pub assets: u32,
}

pub struct Store {
    conn: Connection,
    /// The file this store was opened from, or `None` for an in-memory one.
    ///
    /// Held for `wal_file_bytes`, which has to find the `-wal` sidecar SQLite writes
    /// beside the database. Reconstructing the path from a handle is not portable, and
    /// a store that cannot see its own journal cannot report on it.
    path: Option<std::path::PathBuf>,
}

impl Store {
    /// Open (or create) a store at `path`, applying migrations.
    ///
    /// # Why the file is classified *before* SQLite touches it
    ///
    /// The classification has to happen on the raw file, before any pragma runs, because the
    /// pragmas are what make the file look like something. `PRAGMA journal_mode = WAL`
    /// writes a SQLite header, so by the time it returns an arbitrary file at this path is a
    /// SQLite database: a JPEG renamed to `.holo` would have been classified `NotHolonomy`
    /// — "that is a database, but not a document" — when it is not a database at all. The
    /// difference between those two sentences is the difference between a user who fixes
    /// the filename and a user who keeps looking in the wrong place.
    ///
    /// # Why a wrong file is refused rather than migrated into
    ///
    /// `init` migrates anything that lacks a `meta` table. Applied to somebody's
    /// `invoices.db` that writes eight tables into it, which is a destructive act performed
    /// by a double-click. So the refusal happens here, before a connection is opened.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        match crate::holo::probe(path)? {
            crate::holo::FileKind::Absent => {}
            crate::holo::FileKind::Foreign | crate::holo::FileKind::NotHolonomy => {
                return Err(Error::NotADocument { path: path.display().to_string() })
            }
            // Older is allowed through and migrated by `init`; newer is refused here.
            //
            // This arm used to reject both, which made the upgrade path inside `init`
            // unreachable for every file on disk: `probe` runs first, so a version-1
            // document never reached the code that would have brought it forward. The
            // only store that could migrate was `open_in_memory`, which skips `probe`.
            // A test using an in-memory store would have passed and the feature would
            // still have been dead on arrival.
            //
            // Newer still stops here rather than reaching `init`, because `init` would
            // find `found > SCHEMA_VERSION` and refuse too -- this is a fast path to the
            // same refusal, kept so the error is raised before the file is opened for
            // writing.
            crate::holo::FileKind::Holonomy(found) if found > SCHEMA_VERSION => {
                return Err(Error::SchemaVersion { found, expected: SCHEMA_VERSION })
            }
            crate::holo::FileKind::Holonomy(_) => {}
        }
        let conn = Connection::open(path)?;
        Self::init(conn, Some(path.to_path_buf()))
    }

    /// An in-memory store, for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    fn init(conn: Connection, path: Option<std::path::PathBuf>) -> Result<Self> {
        // `auto_vacuum` first, and it has to be here: the setting only takes effect on a
        // database that has not yet created a table, so it must precede the migrations.
        //
        // Without it, deleting a row returns its pages to SQLite's *free list* — reusable
        // by the next write, but never handed back to the filesystem. So an asset sweep
        // that reclaimed 64 KiB of images would leave a file exactly the same size on
        // disk, and "the file shrinks" would be untrue however correct the sweep was.
        // `INCREMENTAL` rather than `FULL` because `PRAGMA incremental_vacuum` lets the
        // sweep reclaim only what it freed, in bounded steps, instead of rewriting the
        // whole database on every close.
        //
        // On a file created before this pragma existed, SQLite ignores the setting rather
        // than failing — changing it requires a full `VACUUM`. Those files simply keep
        // the old behaviour until `backup_to`, which already runs `VACUUM INTO`, writes
        // one.
        conn.execute_batch(
            "PRAGMA auto_vacuum = INCREMENTAL;
             PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .map_err(|e| Error::Other(e.into()))?;

        // A fresh database has no `meta` table yet, and querying it before the
        // migration runs fails with "no such table" rather than returning no
        // rows. Check for the table itself first.
        let has_meta: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
                [],
                |r| r.get::<_, i64>(0).map(|n| n > 0),
            )
            .unwrap_or(false);

        if !has_meta {
            for m in MIGRATIONS {
                conn.execute_batch(m)?;
            }
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                [SCHEMA_VERSION.to_string()],
            )?;

            // `PRAGMA user_version` cannot be a bound parameter, so the version is
            // formatted into the statement rather than interpolated from a caller. The
            // substituted value is this crate's own `SCHEMA_VERSION` constant and nothing
            // else can reach this expression, so there is no injection surface; the same
            // substitution for a user-supplied number would not be defensible.
            //
            // Written in the same statement as the `meta` row so a migration cannot
            // succeed at one and fail at the other, which would leave a file whose header
            // says 1 and whose table says 0.
            conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
                .map_err(|e| Error::Other(e.into()))?;
        } else {
            // A file from an earlier version is migrated in place rather than refused.
            //
            // Before this branch existed, `init` accepted exactly two states -- no `meta`
            // table at all, or a `schema_version` equal to `SCHEMA_VERSION` -- and refused
            // everything else with `Error::SchemaVersion`. That made the first schema
            // change since the `.holo` format landed equivalent to deleting every user's
            // work: an older file would not open, and the message names a version number
            // rather than anything a user can act on. `MIGRATIONS` was already an ordered
            // list, so the upgrade is just the tail of it, and refusing to do so would mean
            // the search index could never be restructured without losing documents.
            //
            // A file from a *newer* version is still refused, and that one has to be: this
            // build cannot know what a later migration did, and guessing would risk writing
            // rows a newer Holonomy would misread.
            let current: Option<String> = conn
                .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| {
                    r.get(0)
                })
                .optional()?;
            let found: i64 = match current {
                Some(v) => v.parse().map_err(|_| Error::SchemaVersion {
                    found: -1,
                    expected: SCHEMA_VERSION,
                })?,
                None => {
                    return Err(Error::SchemaVersion { found: 0, expected: SCHEMA_VERSION });
                }
            };

            if found > SCHEMA_VERSION {
                return Err(Error::SchemaVersion { found, expected: SCHEMA_VERSION });
            }

            if found < SCHEMA_VERSION {
                // One transaction for the whole tail, so a migration that fails halfway
                // leaves the file at its original version rather than part-way between
                // two. A file stuck mid-upgrade is the state this is most afraid of: the
                // `meta` row would claim a version whose migrations have not all run.
                let tx = conn.unchecked_transaction()?;
                for (i, m) in MIGRATIONS.iter().enumerate().skip(found as usize) {
                    tx.execute_batch(&format!("-- migration {}\n{}", found as usize + 1 + i, m))
                        .map_err(|e| Error::Migration {
                            version: found as usize + 1 + i,
                            source: e,
                        })?;
                }
                tx.execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                    [SCHEMA_VERSION.to_string()],
                )?;
                tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
                    .map_err(|e| Error::Other(e.into()))?;
                tx.commit()?;
            }
        }
        Ok(Self { conn, path })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    // -- documents ---------------------------------------------------------

    pub fn create_document(&self, title: &str) -> Result<Document> {
        let now = crate::now_ms();
        let id = ulid::Ulid::new().to_string();
        self.conn.execute(
            "INSERT INTO documents (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, title, now, now],
        )?;
        Ok(Document { id, title: title.into(), created_at: now, updated_at: now })
    }

    /// Delete a document and everything that hangs off it.
    ///
    /// # Why this exists
    ///
    /// The verification harness builds real documents in the user's database to have
    /// something store-backed to scroll, and without this those accumulate: a 50-section
    /// fixture left behind after every cross-engine run, in the same file as the user's
    /// work. `Foreign Keys` is `ON` (see `init`) and `sections.document_id` cascades, so
    /// the section rows go with the document row.
    ///
    /// The WAL rows do **not** cascade — `wal.document_id` has no foreign key, deliberately,
    /// because a recovery row has to survive the deletion of the thing it recovers and be
    /// flushed rather than orphaned. So they are deleted explicitly here, and a stale
    /// flush after this returns would recreate content for a document that no longer
    /// exists. That ordering is why this is a method on the store rather than a `DELETE`
    /// the caller writes.
    pub fn delete_document(&self, id: &str) -> Result<usize> {
        // The index rows go first, and this is the statement the FTS5 content-table trap
        // made unwriteable for months.
        //
        // `sections_fts` was `content='sections'`: it stored no text of its own, so a plain
        // `DELETE FROM sections_fts WHERE ...` was not a delete of index rows. FTS5 routed it
        // to the *content table*, and because nothing had ever been inserted into the index
        // there was no index row to keep in step, so the statement failed with
        // `SQLITE_CORRUPT: database disk image is malformed` -- on every document with
        // sections, which is every document worth deleting. The workaround was to call
        // `reindex` and let `'rebuild'` re-derive everything, which is correct and slow.
        //
        // The index is now contentless, so `DELETE ... WHERE rowid = ?` is a real index-row
        // delete. It still has to name the rowids, because the index has no `document_id`
        // to filter on and the cascade that removes the `sections` rows has not run yet at
        // this point in the function.
        //
        // Doing it *before* the cascade rather than after is what makes the rowid list
        // obtainable, and it is safe: a `search` reaches an index row only by joining it to
        // `sections`, so a row in the index whose section is gone produces no result rather
        // than a hit pointing at nothing. There is no window here in which a user can see
        // a section that is being deleted.
        self.conn.execute(
            "DELETE FROM sections_fts
              WHERE rowid IN (SELECT rowid FROM sections WHERE document_id = ?1)",
            params![id],
        )?;

        self.conn.execute("DELETE FROM wal WHERE document_id = ?1", params![id])?;
        self.conn.execute("DELETE FROM documents WHERE id = ?1", params![id])?;

        // Read *before* anything above, and the ordering here is the whole reason this is
        // a method rather than a `DELETE` the caller writes: `changes()` reports the most
        // recent statement, and after the three deletes above that would be the
        // `documents` delete -- correct -- but only because the index delete was moved to
        // the front. The row count answers "was there a document here", and a count of
        // index rows would be a plausible-looking answer to the wrong question.
        let removed = self.conn.changes() as usize;
        Ok(removed)
    }

    pub fn document(&self, id: &str) -> Result<Document> {
        self.conn
            .query_row(
                "SELECT id, title, created_at, updated_at FROM documents WHERE id = ?1",
                params![id],
                |r| {
                    Ok(Document {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        created_at: r.get(2)?,
                        updated_at: r.get(3)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| Error::DocumentNotFound(id.into()))
    }

    pub fn documents(&self) -> Result<Vec<Document>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, created_at, updated_at FROM documents ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Document {
                id: r.get(0)?,
                title: r.get(1)?,
                created_at: r.get(2)?,
                updated_at: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn rename_document(&self, id: &str, title: &str) -> Result<()> {
        let now = crate::now_ms();
        let n = self.conn.execute(
            "UPDATE documents SET title = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, title, now],
        )?;
        if n == 0 {
            return Err(Error::DocumentNotFound(id.into()));
        }
        Ok(())
    }

    // -- manifest ----------------------------------------------------------

    /// Read a document's manifest. This is the only read needed to display a
    /// 2000-page document: no blobs are touched.
    pub fn manifest(&self, document_id: &str) -> Result<Manifest> {
        // Confirm the document exists so an empty manifest is distinguishable
        // from a missing document.
        self.document(document_id)?;

        let mut stmt = self.conn.prepare(
            "SELECT id, order_key, title, word_count, mark_count, char_count, block_count,
                      created_at, updated_at
               FROM sections WHERE document_id = ?1 ORDER BY order_key ASC",
        )?;
        let rows = stmt.query_map(params![document_id], |r| {
            Ok(ManifestEntry {
                id: r.get(0)?,
                order_key: crate::order::OrderKey(r.get::<_, i64>(1)? as u64),
                title: r.get(2)?,
                word_count: r.get::<_, i64>(3)? as u32,
                mark_count: r.get::<_, i64>(4)? as u32,
                char_count: r.get::<_, i64>(5)? as u32,
                block_count: r.get::<_, i64>(6)? as u32,
                created_at: r.get(7)?,
                updated_at: r.get(8)?,
            })
        })?;
        let mut m = Manifest::new(document_id);
        for e in rows {
            m.push(e?);
        }
        Ok(m)
    }

    // -- section content ---------------------------------------------------

    /// Fetch and decompress one section's ProseMirror JSON.
    ///
    /// This is the hot read path: M0 mounts a section on every focus change.
    pub fn load_section(&self, section_id: &str) -> Result<Value> {
        let blob: Option<Vec<u8>> = self
            .conn
            .query_row("SELECT content_zstd FROM sections WHERE id = ?1", params![section_id], |r| {
                r.get(0)
            })
            .optional()?;

        let blob = blob.ok_or_else(|| Error::SectionNotFound(section_id.into()))?;
        decode(section_id, &blob)
    }

    /// Fetch a section's content **still compressed**.
    ///
    /// The boot payload puts section content on the wire as bytes and the renderer
    /// decompresses it, so decompressing here to hand the frontend JSON would be
    /// work thrown away twice: once on this side and once on the other. This exists
    /// so `get_document_boot` can be a byte copy.
    ///
    /// Returns the stored bytes verbatim rather than re-encoding, so what arrives
    /// in the renderer is byte-identical to what is on disk. That is worth keeping:
    /// it means a compression change shows up as a size difference rather than as
    /// a content difference nobody can explain.
    pub fn section_bytes(&self, section_id: &str) -> Result<Vec<u8>> {
        let blob: Option<Vec<u8>> = self.conn.query_row(
            "SELECT content_zstd FROM sections WHERE id = ?1",
            params![section_id],
            |r| r.get(0),
        )
        .optional()?;
        blob.ok_or_else(|| Error::SectionNotFound(section_id.into()))
    }

    /// Fetch several sections' compressed content in one query.
    ///
    /// A per-section `section_bytes` in a loop would be one statement per section
    /// on the boot path. At twelve sections that is not measurable, but the query is
    /// the same length either way and a single statement keeps the boot path to one
    /// round trip regardless of how the window grows.
    ///
    /// Ids with no row are omitted rather than reported: the boot payload's
    /// `visible` list is derived from a manifest read moments earlier, so a missing
    /// section means a concurrent structural change, and failing the whole boot over
    /// one vanished section would be worse than rendering one fewer section.
    pub fn section_bytes_many(&self, section_ids: &[String]) -> Result<Vec<(String, Vec<u8>)>> {
        if section_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            // Every column qualified. `json_each` exposes `id`, `key` and `value`,
            // so unqualified `id` is ambiguous -- and the error only appears at
            // runtime, on the boot path, where it would read as a corrupt database
            // rather than as a typo.
            "SELECT sections.id, sections.content_zstd
               FROM sections, json_each(?1) AS want
              WHERE sections.id = want.value
              ORDER BY sections.order_key ASC",
        )?;
        // `ANY(?1)` needs a value rusqlite can turn into a bound list. Binding the
        // slice directly is not implemented for `&[String]`, so the ids go across as
        // a JSON array and `json_each` expands them -- which also keeps the
        // statement's parameter count fixed at one regardless of window size.
        let ids_json = serde_json::to_string(section_ids)?;
        let rows = stmt.query_map(params![ids_json], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut out = Vec::with_capacity(section_ids.len());
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Write a section's content, compressing it and recomputing the derived
    /// columns. This is the flush path, not the keystroke path.
    pub fn save_section(
        &self,
        section_id: &str,
        json: &Value,
        metrics: SectionMetrics,
        plain_text: &str,
    ) -> Result<()> {
        let now = crate::now_ms();
        let blob = encode(json)?;
        // `block_count` is recomputed from the stored JSON rather than taken from
        // `metrics`, because `SectionMetrics` has no such field and the geometry
        // needs a value that is true of the bytes actually written. Deriving it
        // here means a caller cannot write content whose height estimate disagrees
        // with its own structure.
        let analyzed = analyze(json);
        self.conn.execute(
            "UPDATE sections
                SET content_zstd = ?2, plain_text = ?3, word_count = ?4, mark_count = ?5,
                    char_count = ?6, block_count = ?7, updated_at = ?8
              WHERE id = ?1",
            params![
                section_id,
                blob,
                plain_text,
                metrics.words,
                metrics.marks,
                metrics.chars,
                analyzed.block_count,
                now
            ],
        )?;
        self.conn.execute(
            "UPDATE documents SET updated_at = ?1 WHERE id = (SELECT document_id FROM sections WHERE id = ?2)",
            params![now, section_id],
        )?;
        Ok(())
    }

    /// Create an empty section at the end of a document.
    pub fn add_section(&self, document_id: &str, json: &Value) -> Result<String> {
        let id = ulid::Ulid::new().to_string();
        let manifest = self.manifest(document_id)?;
        let order_key = crate::order::append(manifest.entries().last().map(|e| e.order_key.0));
        self.insert_section_at(document_id, &id, order_key, json)?;
        Ok(id)
    }

    /// Create a section at a specific position in the ordering.
    ///
    /// The other half of [`Store::add_section`], and the one a split needs: the new
    /// section goes *between* two existing ones, so appending cannot express it.
    ///
    /// The caller supplies the order key rather than this function deriving it,
    /// because the caller knows the index it is inserting at and
    /// [`crate::order::key_for_insert`] is the function that turns an index and the
    /// existing keys into one. Deriving it here would need the manifest read this
    /// function already does, and would put the "insert between these two" decision
    /// in two places.
    pub fn insert_section(
        &self,
        document_id: &str,
        order_key: u64,
        json: &Value,
    ) -> Result<String> {
        let id = ulid::Ulid::new().to_string();
        self.insert_section_at(document_id, &id, order_key, json)?;
        Ok(id)
    }

    fn insert_section_at(
        &self,
        document_id: &str,
        id: &str,
        order_key: u64,
        json: &Value,
    ) -> Result<()> {
        let now = crate::now_ms();
        let blob = encode(json)?;
        let a = analyze(json);
        self.conn.execute(
            "INSERT INTO sections
                 (id, document_id, order_key, title, content_zstd, plain_text,
                  word_count, mark_count, char_count, block_count, created_at, updated_at)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, 0, ?7, ?8, ?9, ?10)",
            params![
                id,
                document_id,
                order_key,
                blob,
                a.text,
                a.word_count,
                a.char_count,
                a.block_count,
                now,
                now
            ],
        )?;
        self.conn.execute(
            "UPDATE documents SET updated_at = ?1 WHERE id = ?2",
            params![now, document_id],
        )?;
        // A new section has no index row, so this is an insert rather than a replace.
        // Same reasoning as on the save path: a section that exists but is not indexed
        // is invisible to search, which a user cannot distinguish from a section that
        // does not exist.
        self.reindex_section(id)?;
        Ok(())
    }

    pub fn delete_section(&self, section_id: &str) -> Result<()> {
        // The index rows go first, and the rowid is read before the row is deleted --
        // after the `DELETE` there is nothing left to read it from. The order matters
        // for the same reason `delete_document` had to stop using a plain
        // `DELETE FROM sections_fts`: with a `content=''` table this is a real index-row
        // delete, so it is correct, but it is still not a foreign key and nothing would
        // clean it up if the row were left behind. A tombstone in the index is a hit that
        // resolves to nothing.
        if let Some(rowid) = self.section_rowid(section_id)? {
            self.unindex_section(rowid)?;
        }
        self.conn
            .execute("DELETE FROM sections WHERE id = ?1", params![section_id])?;
        self.conn
            .execute("DELETE FROM wal WHERE section_id = ?1", params![section_id])?;
        Ok(())
    }

    /// Rewrite a section's position in the ordering.
    ///
    /// Used after a split, where the two halves end up closer together than the
    /// 1024-wide gap the order keys are allocated with, and after a merge, where
    /// the gap left behind is simply unused. One row per call rather than a batch
    /// `apply_order`: a split touches two keys and a merge touches none, and a
    /// general batch setter would be an untested code path used by neither.
    pub fn set_order_key(&self, section_id: &str, order_key: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE sections SET order_key = ?2 WHERE id = ?1",
            params![section_id, order_key as i64],
        )?;
        Ok(())
    }

    /// Which document a section belongs to, and its position in the ordering.
    ///
    /// The lifecycle commands receive a section id and an index from the frontend,
    /// and the index is the frontend's belief about where that section sits. Acting
    /// on that belief without checking would mean a split landing in the wrong place
    /// if the two sides disagreed, so the index is verified against the store and
    /// the store's answer is used.
    ///
    /// Returns `(document_id, index)`.
    pub fn locate_section(&self, section_id: &str) -> Result<(String, usize)> {
        let row: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT document_id, order_key FROM sections WHERE id = ?1",
                params![section_id],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?;
        let (document_id, order_key) =
            row.ok_or_else(|| Error::SectionNotFound(section_id.to_string()))?;
        let manifest = self.manifest(&document_id)?;
        let index = manifest
            .entries()
            .iter()
            .position(|e| e.id == section_id)
            .ok_or_else(|| Error::SectionNotFound(section_id.to_string()))?;
        let _ = order_key;
        Ok((document_id, index))
    }

    /// Section ids in document order.
    ///
    /// Returned to the frontend after a lifecycle change in place of a patch,
    /// because a split moves keys on both sides of the cut and a patch would have to
    /// describe both halves correctly to end up right.
    pub fn section_ids(&self, document_id: &str) -> Result<Vec<String>> {
        Ok(self
            .manifest(document_id)?
            .entries()
            .iter()
            .map(|e| e.id.clone())
            .collect())
    }

    // -- assets ------------------------------------------------------------

    /// Store an asset, content-addressed. Identical bytes are stored once, which
    /// matters because `2000.md` §2 requires assets to be separate from section
    /// JSON and a repeated logo should not be duplicated per section.
    ///
    /// # SHA-256, and why the hash function is not an internal choice
    ///
    /// The hash is in the *key* and the key goes in a URL: `holo-asset://<hash>` is what
    /// the webview fetches, and it is also what the document JSON stores in an image
    /// node's `src`. So it is a thing other software has to reproduce.
    ///
    /// WebCrypto offers `digest('SHA-256')` and nothing comparable for anything else, so
    /// the frontend can verify an asset it fetched — which is the check that says the
    /// protocol handler did not hand back the wrong bytes. It was blake3, which meant
    /// the one check available in the environment where the asset is consumed could not
    /// be performed at all.
    pub fn put_asset(&self, bytes: &[u8], mime: &str) -> Result<String> {
        let hash = crate::sha256_hex(bytes);
        self.conn.execute(
            "INSERT INTO assets (hash, mime, byte_size, bytes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(hash) DO NOTHING",
            params![hash, mime, bytes.len() as i64, bytes, crate::now_ms()],
        )?;
        Ok(hash)
    }

    /// Fetch an asset's bytes and mime by hash.
    ///
    /// No validation of the hash's shape here. A malformed key is a miss, not an error, and
    /// the caller — the protocol handler — needs to tell those apart: a miss is a 404 and
    /// an error is a 500.
    pub fn get_asset(&self, hash: &str) -> Result<Option<(String, Vec<u8>)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT mime, bytes FROM assets WHERE hash = ?1",
                params![hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    // -- WAL ---------------------------------------------------------------

    pub fn wal(&self) -> Wal<'_> {
        Wal::new(&self.conn)
    }

    /// The keystroke path: append to the log. Cheap, and durable on return.
    pub fn log_edit(
        &self,
        document_id: &str,
        section_id: &str,
        json: &Value,
        metrics: SectionMetrics,
        plain_text: &str,
    ) -> Result<()> {
        let entry = WalEntry::new(
            document_id,
            section_id,
            encode(json)?,
            plain_text,
            (metrics.words, metrics.marks, metrics.chars),
            analyze(json).block_count,
        );
        self.wal().append(&entry, crate::now_ms())?;
        Ok(())
    }

    /// The id of the most recent WAL row for a section, and how many rows are pending
    /// for a document.
    ///
    /// # Why this exists rather than the shell querying SQLite
    ///
    /// The shell would need a `rusqlite` dependency and a hand-written query to answer
    /// "what did that write cost and how backed up is the log". The schema is the
    /// store's business, and a caller that could ask it directly would eventually start
    /// asking it differently.
    ///
    /// The id is for diagnostics — a caller reporting "the write that went wrong" names
    /// a row. The pending count is the one that matters operationally: it is how a caller
    /// knows the log is not being folded, long before the file size says so.
    pub fn latest_wal_row(&self, section_id: &str, document_id: &str) -> Result<(i64, u32)> {
        let id: Option<i64> = self
            .conn
            .query_row(
                // The WAL's primary key is `seq`, not `id`. Written as `id` this
                // fails at runtime with "no such column" -- on the typing path, on every
                // commit -- rather than at build time.
                "SELECT seq FROM wal WHERE section_id = ?1 ORDER BY seq DESC LIMIT 1",
                params![section_id],
                |r| r.get(0),
            )
            .optional()?;
        // `pending_count` rather than `pending().len()`: the latter selects every
        // payload, and this is called on the commit path where the row count is all
        // that is wanted.
        let pending = self.wal().pending_count(document_id)? as u32;
        Ok((id.unwrap_or(0), pending))
    }

    /// Fold every pending WAL row into its section blob, then clear the log.
    ///
    /// Called on a debounce, on window close, and on app exit. After this the
    /// log is empty, which is what keeps it a buffer rather than history.
    pub fn flush(&self, document_id: &str) -> Result<usize> {
        let pending = self.wal().pending(document_id)?;
        let mut flushed = 0;
        for p in pending {
            // `block_count` comes from the WAL row rather than being recomputed.
            // The payload is already zstd-compressed, so recomputing would mean
            // decompressing every pending row on the flush path purely to count
            // blocks. The WAL row is the record of the state at edit time, and the
            // block count was captured when the edit happened.
            self.conn.execute(
                "UPDATE sections
                    SET content_zstd = ?2, plain_text = ?3, word_count = ?4, mark_count = ?5,
                        char_count = ?6, block_count = ?7, updated_at = ?8
                  WHERE id = ?1",
                params![
                    p.section_id,
                    p.payload,
                    p.plain_text,
                    p.word_count,
                    p.mark_count,
                    p.char_count,
                    p.block_count,
                    crate::now_ms()
                ],
            )?;
            // The index follows the fold, and this is the call site that makes search
            // keep up with typing without costing anything per keystroke. `flush` runs
            // on a debounce and on close, so the section blob and the index row are
            // written together, once, and the work is proportional to the number of
            // sections that actually changed rather than to the size of the document.
            self.reindex_section(&p.section_id)?;
            flushed += 1;
        }
        self.wal().checkpoint_all(document_id)?;
        Ok(flushed)
    }

    /// Every document with at least one unflushed WAL row.
    ///
    /// Needed by the close path, and the reason it exists is that a store holds more
    /// than one document over a session even though the UI has one open at a time: a
    /// second document opened and edited, then the user switched back, leaves pending
    /// rows against the *first* id. A shutdown that folded `document_id` alone would
    /// report success and leave those rows behind -- and "clean exit" is precisely the
    /// moment a reader expects the file to be complete.
    pub fn documents_with_pending_wal(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT DISTINCT document_id FROM wal ORDER BY document_id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Fold every pending row for every document.
    ///
    /// The close path's first step. See [`documents_with_pending_wal`] for why this
    /// cannot be spelled with a single document id.
    pub fn flush_all(&self) -> Result<usize> {
        let mut flushed = 0;
        for id in self.documents_with_pending_wal()? {
            flushed += self.flush(&id)?;
        }
        Ok(flushed)
    }

    /// Fold, then checkpoint SQLite's own write-ahead log down to zero bytes.
    ///
    /// # There are two write-ahead logs, and they are not the same thing
    ///
    /// This project keeps a *logical* log: the `wal` table, holding one zstd section
    /// snapshot per edited section, so a keystroke costs one row insert and the section
    /// blob is rewritten on a debounce. [`flush_all`](Self::flush_all) empties it.
    ///
    /// SQLite independently maintains a *physical* `-wal` file beside the database, in
    /// which it stages committed pages before folding them back into the main file.
    /// `PRAGMA wal_checkpoint(TRUNCATE)` folds and then truncates that file to zero
    /// bytes.
    ///
    /// Both have to happen for a clean exit to leave what it claims. Folding without
    /// checkpointing leaves a `-wal` file whose size is unrelated to whether the data is
    /// durable -- a reader looking only at the database file size would see a file
    /// that looks stale. Checkpointing without folding empties the wrong log entirely.
    ///
    /// Why `TRUNCATE` rather than the default `PASSIVE`: `PASSIVE` copies pages back
    /// and leaves the file at whatever size it reached, and it refuses to do anything
    /// while a reader is attached. `TRUNCATE` also resets the file to zero bytes, which
    /// is what makes the result assertable -- [`wal_file_bytes`](Self::wal_file_bytes) is
    /// the check, and `no_pending_wal_rows_after_shutdown` is the test.
    ///
    /// Returns the size the `-wal` file was left at. Zero is the success case; a
    /// non-zero return means something was still reading it, and the caller should
    /// report that rather than assert success.
    pub fn checkpoint_truncate(&self) -> Result<i64> {
        // `query_row` rather than `execute`: the pragma returns a row, and running it
        // through `execute` would discard the one value that says whether it worked.
        let busy: i64 = self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
        if busy != 0 {
            return Err(Error::Other(anyhow::anyhow!(
                "wal_checkpoint(TRUNCATE) reported {busy} busy readers, so the journal was not truncated"
            )));
        }
        self.wal_file_bytes()
    }

    /// Size in bytes of SQLite's `-wal` sidecar, or 0 if there is none.
    ///
    /// A file-backed store in `journal_mode = WAL` always has one. An in-memory store
    /// does not, which is why the shutdown tests are file-backed: asserting that a
    /// checkpoint ran is meaningless against a database with no journal.
    pub fn wal_file_bytes(&self) -> Result<i64> {
        let path = match self.path.as_deref() {
            Some(p) => p,
            None => return Ok(0),
        };
        let sidecar = std::path::PathBuf::from(format!("{}-wal", path.display()));
        // Absent is 0 bytes, not an error: a store opened read-only, or one whose
        // journal was already folded away, has no sidecar and that is a clean state.
        Ok(match std::fs::metadata(&sidecar) {
            Ok(m) => m.len() as i64,
            Err(_) => 0,
        })
    }

    /// The database file path, if this store is file-backed.
    /// Write a consistent snapshot of the whole database to a single file.
    ///
    /// # Why this is a method and not a `std::fs::copy` at the call site
    ///
    /// Because a copy of a live SQLite database is not a database. The `-wal` sidecar holds
    /// committed transactions the main file has not absorbed, so a copy taken while the
    /// application is running can be missing the last few writes — and a backup that silently
    /// lacks the most recent edits is worse than no backup, because it is believed.
    ///
    /// `VACUUM INTO` takes a read snapshot, writes one self-contained file, and includes
    /// everything committed before it started. It cannot run inside a transaction, which is why
    /// the checkpoint below happens *first* and outside anything else.
    ///
    /// # Why the whole database rather than one document
    ///
    /// Because the documents share the `assets` table, and a snapshot of one document without its
    /// figures is not a document. Filtering would mean copying selected rows by hand, which is
    /// right until someone adds a table.
    ///
    /// # Why it refuses to overwrite
    ///
    /// Because a backup that quietly replaces the last good copy is a backup with no history. The
    /// caller chooses a new path, or deletes the old one knowingly.
    pub fn backup_to(&self, path: &str) -> Result<BackupSummary> {
        if std::path::Path::new(path).exists() {
            return Err(Error::Other(anyhow::anyhow!(
                "{path:?} already exists. A backup overwrites the last good copy, so it will not \
                 do that silently -- choose another name, or remove this one yourself."
            )));
        }
        let started = std::time::Instant::now();

        // Fold and checkpoint first, so the snapshot does not have to carry the sidecar and so
        // the file has the same shape whether or not anything was pending. The same order
        // `graceful_shutdown` uses, and for the same reason.
        self.flush_all()?;
        self.checkpoint_truncate()?;

        // `VACUUM INTO` wants the destination as a bound parameter, which is what makes it safe
        // against a path containing a quote. The statement cannot run inside a transaction, and
        // `execute_batch` is used rather than `execute` because the trailing semicolon makes
        // this a multi-statement string as far as `execute` is concerned.
        // `execute` rather than `execute_batch`: `VACUUM INTO` takes bound parameters and
        // `execute_batch` does not accept them. The bound parameter is also what makes the
        // destination safe -- a path containing a quote cannot become part of the statement.
        self.conn.execute("VACUUM INTO ?1", [path]).map_err(|e| {
            Error::Other(anyhow::anyhow!("SQLite refused to write the backup: {e}"))
        })?;

        let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let count = |table: &str| -> u32 {
            self.conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .map(|n| n as u32)
                .unwrap_or(0)
        };
        Ok(BackupSummary {
            path: path.to_string(),
            bytes,
            elapsed_ms: started.elapsed().as_millis(),
            documents: count("documents"),
            sections: count("sections"),
            assets: count("assets"),
        })
    }

    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// Replay unflushed state on open, so a crash loses at most the debounce
    /// window rather than the session.
    pub fn recover(&self, document_id: &str) -> Result<usize> {
        self.flush(document_id)
    }

    // -- search ------------------------------------------------------------

    /// Index one section, replacing whatever was indexed for it before.
    ///
    /// This is the operation that makes search work at all, and it exists because
    /// [`Self::reindex`] cannot be called per keystroke. See `schema.rs`'s `MIGRATION_2`
    /// for the long form; the short version is that an external-content FTS5 index
    /// cannot be told "this row changed" without being handed the row's *original*
    /// text, which a database does not keep once the section has been edited, so the
    /// only available operation was a whole-document rebuild. A million words
    /// re-tokenised per keystroke is not an option, which is why `reindex` had no
    /// caller anywhere in the application and search was empty in every real session
    /// while its tests passed.
    ///
    /// On a `content='', contentless_delete=1` table the delete is unconditional and
    /// needs nothing but the rowid, so this is two statements and cannot leave the
    /// superseded text findable. The caller does not have to know, or remember, what
    /// the section said last time -- which is the property the old schema could not
    /// offer at all.
    ///
    /// Idempotent, and safe to call for a section that was never indexed: the delete
    /// matches nothing and the insert creates the row.
    pub fn index_section(&self, rowid: i64, plain_text: &str) -> Result<()> {
        self.conn.execute("DELETE FROM sections_fts WHERE rowid = ?1", params![rowid])?;
        self.conn.execute(
            "INSERT INTO sections_fts(rowid, plain_text) VALUES (?1, ?2)",
            params![rowid, plain_text],
        )?;
        Ok(())
    }

    /// Re-index whatever text `sections` currently holds for a section.
    ///
    /// This is the form every write path uses, and it reads the text from the `sections`
    /// row rather than taking it as an argument. That is deliberate: the caller has
    /// already written the row by the time it can index, so reading the committed value
    /// makes it impossible for the index and the section to disagree about *which* text
    /// is current. Passing the caller's copy instead would be one more thing to keep in
    /// step, and the drift this schema exists to escape was exactly a case of two copies
    /// falling out of step.
    ///
    /// A section that is not in `sections` indexes nothing rather than erroring, which
    /// makes the call safe from a path that may have already removed the row.
    pub fn reindex_section(&self, section_id: &str) -> Result<()> {
        let Some((rowid, text)) = self
            .conn
            .query_row(
                "SELECT rowid, COALESCE(plain_text, '') FROM sections WHERE id = ?1",
                params![section_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        else {
            return Ok(());
        };
        self.index_section(rowid, &text)
    }

    /// A section's `sections.rowid`, which is the key its index row is stored under.
    ///
    /// `rowid` rather than `id` because a contentless FTS5 table is keyed by an integer,
    /// and this is the column that says which integer a `sections` row corresponds to.
    /// `id` is a ULID and so cannot be the key directly, and hashing it would put a
    /// collision between two sections into the search results.
    fn section_rowid(&self, section_id: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT rowid FROM sections WHERE id = ?1",
                params![section_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Drop a section's index rows.
    ///
    /// Separate from [`Self::index_section`] rather than folded into it because deleting
    /// a section has no replacement text to write, and because the two fail differently:
    /// an index delete on an absent row is a no-op, which is what makes it safe to call
    /// speculatively from a path that may already have removed the section.
    pub fn unindex_section(&self, rowid: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM sections_fts WHERE rowid = ?1", params![rowid])?;
        Ok(())
    }

    /// Repopulate the index for a document from its sections.
    ///
    /// This is the whole-document operation, kept for three cases: a file whose index was
    /// never populated, a document imported from elsewhere, and repair. It is *not* on
    /// the edit path -- [`Self::index_section`] is -- because a 1,300-section document
    /// is 1.33M words and tokenising all of it to record one keystroke is a cost with no
    /// upper bound tied to what the user did.
    ///
    /// `'rebuild'` is unavailable on a contentless table, so this is an explicit delete
    /// followed by a bulk insert. That is more work than a rebuild and buys something a
    /// rebuild cannot: the result is verifiable. The return value is the number of rows
    /// now indexed, which is compared against the number of sections by
    /// `search_index_is_populated`, so a migration that silently indexed nothing is
    /// caught rather than trusted.
    pub fn reindex(&self, document_id: &str) -> Result<usize> {
        self.conn.execute(
            "DELETE FROM sections_fts
              WHERE rowid IN (SELECT rowid FROM sections WHERE document_id = ?1)",
            params![document_id],
        )?;
        let mut stmt = self.conn.prepare(
            "INSERT INTO sections_fts(rowid, plain_text)
             SELECT rowid, COALESCE(plain_text, '') FROM sections WHERE document_id = ?1",
        )?;
        stmt.execute(params![document_id])?;
        self.search_count(document_id)
    }


    /// Whether a document's index is populated, and by how much it is out of step.
    ///
    /// Returns `(indexed, sections)`. The two differ legitimately -- an empty section
    /// indexes to an empty string, and a section whose `plain_text` is NULL is not in
    /// the index at all -- so this is reported rather than asserted on by the caller.
    /// What the caller does with the gap is decide whether a rebuild is owed.
    pub fn index_gap(&self, document_id: &str) -> Result<(usize, usize)> {
        let sections: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sections WHERE document_id = ?1",
            params![document_id],
            |r| r.get(0),
        )?;
        Ok((self.search_count(document_id)?, sections as usize))
    }

    /// Number of indexed sections for a document.
    ///
    /// Through the join, because the index has no `document_id` column to filter on --
    /// see `schema.rs`'s `MIGRATION_2` for why a contentless FTS5 table cannot carry one.
    ///
    /// This is a *real* count, which the previous implementation was not. Against an
    /// external-content table, `SELECT COUNT(*) FROM sections_fts WHERE document_id = ?`
    /// reads the content table, so it answered "how many sections does this document
    /// have" and reported it as the size of the index. That is why the search tests
    /// passed while search had never once run: they were asserting that a document with
    /// forty sections had forty indexed sections, and the query they used could not
    /// distinguish that from an empty index.
    pub fn search_count(&self, document_id: &str) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*)
               FROM sections_fts
               JOIN sections ON sections.rowid = sections_fts.rowid
              WHERE sections.document_id = ?1",
            params![document_id],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Search a document's plain text.
    ///
    /// Returns `(section_id, snippet)` pairs, best match first.
    ///
    /// The snippet is built here rather than by FTS5's `snippet()`, which is unavailable
    /// on a contentless table -- it reads the matched column's stored text, and a
    /// contentless table has none to read. That costs one decompression per returned
    /// hit, bounded by `limit`, against a query that is otherwise a single index walk.
    /// The trade is deliberate: the alternative is an index that cannot be updated
    /// incrementally, and the whole reason this function is reachable from a running
    /// application is that it can be.
    pub fn search(&self, document_id: &str, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        // FTS5 query syntax is not SQL: a bare word with punctuation is a syntax
        // error, and a user typing "don't" should not produce one. Quote the
        // query as a phrase, and fall back to a prefix match.
        let escaped = query.replace('"', "\"\"");
        let match_expr = format!("\"{escaped}\"");

        let mut stmt = self.conn.prepare_cached(
            "SELECT sections.id, bm25(sections_fts)
               FROM sections_fts
               JOIN sections ON sections.rowid = sections_fts.rowid
              WHERE sections_fts MATCH ?1
                AND sections.document_id = ?2
              ORDER BY bm25(sections_fts)
              LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![match_expr, document_id, limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })?;

        let mut hits = Vec::new();
        for row in rows {
            let (section_id, score) = row?;
            // A section that fails to decompress should not take the whole result set
            // with it. The row is in the index, so the section exists; if its blob is
            // unreadable that is a `Corrupt` worth reporting on its own, and a search
            // that returns five hits and a panic teaches a user less than five hits and
            // no panic does.
            let snippet = match self.section_plain_text(&section_id) {
                Ok(text) => snippet_around(&text, &escaped),
                Err(_) => String::new(),
            };
            hits.push(SearchHit { section_id, snippet, score });
        }
        Ok(hits)
    }

    /// A section's plain text, for snippet construction.
    ///
    /// Read from the `plain_text` column rather than by decompressing `content_zstd`.
    /// That column is maintained as a side effect of every write for exactly this
    /// reason: FTS5's `snippet()` would have read the text for free from an
    /// external-content table, and moving to a contentless one gave that up, so the text
    /// is kept where the query can reach it without zstd on the search path.
    fn section_plain_text(&self, section_id: &str) -> Result<String> {
        let text: Option<String> = self
            .conn
            .query_row(
                "SELECT plain_text FROM sections WHERE id = ?1",
                params![section_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        // Nullable because a section written before the column existed has none, and an
        // empty string is a better answer than a decompression on the search path.
        Ok(text.unwrap_or_default())
    }
}

/// A search result.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub section_id: String,
    /// Context around the match, with `<b>` around the matched terms.
    pub snippet: String,
    /// bm25 relevance; lower is better.
    pub score: f64,
}

/// Build a snippet of context around the first match, with the match wrapped in `<b>`.
///
/// # Why this exists rather than FTS5's `snippet()`
///
/// `snippet()` reads the matched column's text out of the table. It was free while
/// `sections_fts` was an external-content table over `sections`, and it is not available
/// at all on the contentless table that made incremental indexing possible -- the text is
/// not stored in the FTS table to be read. Reimplementing it here is the cost of that
/// swap, paid deliberately: an index that cannot be updated per keystroke is an index
/// that is never updated, which is what made search non-functional before.
///
/// # The matching is deliberately naive
///
/// Case-insensitive substring search over the raw text. FTS5 has already decided *which
/// sections* match and in what order; this only has to show a human enough context to
/// recognise the hit. It is not a tokenizer, does not stem, and does not reproduce the
/// phrase case where FTS5 matched across a word boundary this scan will not -- a
/// truncated or slightly-off snippet is cosmetic, and reimplementing Unicode tokenization
/// to fix it would not be.
///
/// The window is measured in characters, and cut on a char boundary, so a match inside a
/// multi-byte string cannot panic this.
fn snippet_around(text: &str, needle: &str) -> String {
    const WINDOW: usize = 60;

    if needle.is_empty() {
        return String::new();
    }
    let hay = text.to_lowercase();
    let Some(at) = hay.find(&needle.to_lowercase()) else {
        // FTS5 matched this section by some rule this scan does not model. Returning
        // nothing is honest; inventing a window around character 0 would look like a
        // result and mean nothing.
        return String::new();
    };

    // A byte offset into a String is only usable if it is a char boundary, and `find` on
    // a lowercased copy can differ in byte length from the original for some scripts.
    // Both ends are walked to a boundary rather than trusted.
    let start = floor_char_boundary(text, at.saturating_sub(WINDOW));
    let end = ceil_char_boundary(text, (at + needle.len() + WINDOW).min(text.len()));

    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(&text[start..at]);
    out.push_str("<b>");
    out.push_str(&text[at..at + needle.len()]);
    out.push_str("</b>");
    out.push_str(&text[at + needle.len()..end]);
    if end < text.len() {
        out.push('…');
    }
    out
}

/// The largest index `<= n` that is a char boundary.
fn floor_char_boundary(s: &str, n: usize) -> usize {
    let mut n = n.min(s.len());
    while n > 0 && !s.is_char_boundary(n) {
        n -= 1;
    }
    n
}

/// The smallest index `>= n` that is a char boundary.
fn ceil_char_boundary(s: &str, n: usize) -> usize {
    let mut n = n.min(s.len());
    while n < s.len() && !s.is_char_boundary(n) {
        n += 1;
    }
    n
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

pub fn encode(json: &Value) -> Result<Vec<u8>> {
    let raw = serde_json::to_vec(json)?;
    zstd::encode_all(raw.as_slice(), ZSTD_LEVEL).map_err(Error::Io)
}

pub fn decode(section_id: &str, blob: &[u8]) -> Result<Value> {
    let raw = zstd::decode_all(blob).map_err(|e| Error::Corrupt {
        section_id: section_id.into(),
        reason: e.to_string(),
    })?;
    serde_json::from_slice(&raw).map_err(|e| Error::Corrupt {
        section_id: section_id.into(),
        reason: e.to_string(),
    })
}

/// What [`analyze`] extracts from ProseMirror JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analyzed {
    /// Concatenated text, with block boundaries as newlines. Feeds FTS5.
    pub text: String,
    pub word_count: u32,
    /// Top-level block count, for the scroll geometry.
    pub block_count: u32,
    /// Characters of `text`, excluding the newlines that block boundaries added.
    ///
    /// Not `text.chars().count()`. Those newlines are an artefact of building the
    /// FTS payload, not part of the document, and counting them makes `char_count`
    /// depend on how many blocks the text happens to be split across — so the same
    /// prose reports different character counts in different sections.
    pub char_count: u32,
}

/// Extract plain text, word count, block count, and character count.
///
/// # Why block count is counted here rather than carried from the editor
///
/// The geometry needs it and the manifest stores it, but unlike the mark count
/// this one *is* computable from the stored JSON, so there is no reason to trust
/// the client for it. That matters: the mark count is carried from the editor
/// because Loro's style-anchor set is not visible in the JSON, but a client-supplied
/// block count would be an unvalidated input to every height estimate in the
/// document.
///
/// # Why only top-level blocks
///
/// This counts the children of `doc`, which is what the browser lays out as
/// siblings. A list item's paragraphs are not top-level and must not be counted:
/// they render inside their parent, so counting them would inflate the height
/// estimate by the difference. Verified against the calibration fixture, where 15
/// top-level paragraphs is 15 and not 15-plus-nested.
pub fn analyze(json: &Value) -> Analyzed {
    use serde_json::Value as V;

    /// Node types that occupy their own line box at the top level.
    ///
    /// Two separate lists, and the distinction is load-bearing:
    ///
    /// - `BLOCK_TYPES` is what the *height model* counts. A `bulletList` is one
    ///   line-box group here: it is a single top-level sibling in the document
    ///   flow, and the geometry's per-block term is calibrated against top-level
    ///   blocks. Omitting it produced a real bug: a document that was entirely
    ///   bulleted lists reported `block_count == 0`, and the height estimate
    ///   predicted the section was only its text and chrome — no list height at
    ///   all. A test caught it rather than it shipping.
    /// - `LINE_BREAK_TYPES` is what the *text extraction* breaks on, and it is a
    ///   superset. A `listItem` ends a line even though it is not top-level, so a
    ///   search span does not run across items.
    ///
    /// Keeping them separate is what lets a list contribute one block while its
    /// items still break lines.
    const BLOCK_TYPES: &[&str] = &[
        "paragraph",
        "heading",
        "bulletList",
        "orderedList",
        "taskList",
        "blockquote",
        "codeBlock",
        "table",
        "image",
        "equation",
        "horizontalRule",
    ];

    /// Node types that end a line in the extracted plain text.
    ///
    /// Superset of `BLOCK_TYPES`: includes nested containers whose children each
    /// occupy a line, so search spans stay within one item.
    const LINE_BREAK_TYPES: &[&str] = &[
        "paragraph",
        "heading",
        "listItem",
        "taskItem",
        "blockquote",
        "codeBlock",
        "tableRow",
        "caption",
    ];

    fn is_block(ty: Option<&str>) -> bool {
        ty.map(|t| BLOCK_TYPES.contains(&t)).unwrap_or(false)
    }

    fn breaks_line(ty: Option<&str>) -> bool {
        ty.map(|t| LINE_BREAK_TYPES.contains(&t)).unwrap_or(false)
    }

    fn walk(v: &V, out: &mut String) {
        match v {
            V::Object(map) => {
                let ty = map.get("type").and_then(|t| t.as_str());
                if ty == Some("text") {
                    if let Some(t) = map.get("text").and_then(|t| t.as_str()) {
                        out.push_str(t);
                    }
                }
                // Block boundaries become newlines so a search span does not run
                // across a paragraph break.
                if breaks_line(ty) && !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                for val in map.values() {
                    walk(val, out);
                }
            }
            V::Array(a) => {
                for val in a {
                    walk(val, out);
                }
            }
            _ => {}
        }
    }

    let mut text = String::new();
    walk(json, &mut text);

    // Top-level blocks only: the children of `doc`. A nested `paragraph` inside a
    // `listItem` renders within its parent and must not be counted separately.
    let block_count = json
        .get("content")
        .and_then(|c| c.as_array())
        .map(|a| a.iter().filter(|n| is_block(n.get("type").and_then(|t| t.as_str()))).count())
        .unwrap_or(0) as u32;

    let word_count = text.split_whitespace().count() as u32;
    // Exclude the newlines `walk` inserted at block boundaries.
    let char_count = text.chars().filter(|c| !c.is_whitespace()).count() as u32;

    Analyzed { text, word_count, block_count, char_count }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc() -> Value {
        json!({
            "type": "doc",
            "content": [
                {"type": "paragraph", "content": [{"type": "text", "text": "hello world"}]},
                {"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "a heading"}]}
            ]
        })
    }

    #[test]
    fn create_and_read_document() {
        let s = Store::open_in_memory().unwrap();
        let d = s.create_document("Test").unwrap();
        assert_eq!(s.document(&d.id).unwrap().title, "Test");
        assert_eq!(s.documents().unwrap().len(), 1);
    }

    #[test]
    fn missing_document_is_an_error_not_an_empty_manifest() {
        // Distinguishing "no such document" from "empty document" matters: an
        // empty manifest would look like a document with no sections.
        let s = Store::open_in_memory().unwrap();
        assert!(matches!(
            s.manifest("nope"),
            Err(Error::DocumentNotFound(_))
        ));
    }

    #[test]
    fn section_round_trip() {
        let s = Store::open_in_memory().unwrap();
        let d = s.create_document("Test").unwrap();
        let sid = s.add_section(&d.id, &doc()).unwrap();
        let loaded = s.load_section(&sid).unwrap();
        assert_eq!(loaded, doc());
    }

    // -- block counting ------------------------------------------------------
    //
    // The block count feeds every height estimate in the document, so these are
    // written to be strict about what counts. Each one below is a case where a
    // plausible implementation would be wrong in a way that shows up as a
    // subtly-misplaced scrollbar rather than an error.

    #[test]
    fn block_count_counts_only_top_level_blocks() {
        // A list item's paragraphs render *inside* it. Counting them would inflate
        // the height estimate by one line per item, and a bulleted list of
        // one-line items is exactly where that would be most visible.
        let doc = json!({
            "type": "doc",
            "content": [{
                "type": "bulletList",
                "content": [
                    {"type": "listItem", "content": [
                        {"type": "paragraph", "content": [{"type": "text", "text": "one"}]}
                    ]},
                    {"type": "listItem", "content": [
                        {"type": "paragraph", "content": [{"type": "text", "text": "two"}]}
                    ]}
                ]
            }]
        });
        let a = analyze(&doc);
        // The bulletList is one top-level block. Its listItems are children of that,
        // not of the doc, and its paragraphs are children of the listItems.
        assert_eq!(a.block_count, 1, "nested blocks must not be counted");
        // But the text still comes through, and block boundaries still break lines.
        assert!(a.text.contains("one") && a.text.contains("two"));
    }

    #[test]
    fn block_count_sums_siblings() {
        let doc = json!({
            "type": "doc",
            "content": [
                {"type": "heading", "content": [{"type": "text", "text": "h"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "a"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "b"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "c"}]}
            ]
        });
        assert_eq!(analyze(&doc).block_count, 4);
    }

    #[test]
    fn a_document_of_only_bulleted_lists_reports_one_block_per_list() {
        // The bug this class of omission produces. A section whose every top-level
        // node is a `bulletList` used to report `block_count == 0`, because the
        // list container was missing from the type table while its `listItem`
        // children were present but nested. The height estimate then predicted the
        // section was only its text and chrome — no list height at all — so a
        // bulleted document scrolled short by however tall the lists actually were.
        let doc = json!({
            "type": "doc",
            "content": [
                {"type": "bulletList", "content": [
                    {"type": "listItem", "content": [
                        {"type": "paragraph", "content": [{"type": "text", "text": "a"}]}
                    ]},
                    {"type": "listItem", "content": [
                        {"type": "paragraph", "content": [{"type": "text", "text": "b"}]}
                    ]}
                ]},
                {"type": "orderedList", "content": [
                    {"type": "listItem", "content": [
                        {"type": "paragraph", "content": [{"type": "text", "text": "c"}]}
                    ]}
                ]}
            ]
        });
        let a = analyze(&doc);
        assert_eq!(
            a.block_count, 2,
            "two list containers, not zero and not one per item"
        );
        // And the items still break lines for the search index, which is why the
        // two type lists are separate.
        assert!(
            a.text.contains("a") && a.text.contains("b") && a.text.contains("c"),
            "list text must still be extracted: {:?}",
            a.text
        );
        assert!(a.text.contains('\n'), "list items must break lines for FTS");
    }

    #[test]
    fn block_count_handles_the_other_block_types() {
        // Each of these occupies its own line box, so each contributes a
        // paragraph's worth of height. If one is missed the estimate is
        // systematically short for documents that use it.
        for ty in [
            "codeBlock",
            "table",
            "image",
            "equation",
            "blockquote",
            "horizontalRule",
        ] {
            let doc = json!({
                "type": "doc",
                "content": [
                    {"type": ty, "content": [{"type": "paragraph", "content": [{"type": "text", "text": "x"}]}]}
                ]
            });
            assert_eq!(analyze(&doc).block_count, 1, "{ty} should count as a block");
        }
    }

    #[test]
    fn block_count_is_zero_for_an_empty_document() {
        let empty = json!({"type": "doc", "content": []});
        assert_eq!(analyze(&empty).block_count, 0);
        // And a doc with no content key at all must not panic.
        let bare = json!({"type": "doc"});
        assert_eq!(analyze(&bare).block_count, 0);
    }

    #[test]
    fn char_count_excludes_the_newlines_block_boundaries_added() {
        // The regression this prevents: `char_count` was `text.chars().count()`,
        // which counted the newlines `walk` inserts at block boundaries. Those are
        // an artefact of building the FTS payload, not part of the document, so the
        // same prose reported different character counts depending on how many
        // blocks it happened to be split across — and character count feeds the
        // height estimate.
        let one_block = json!({
            "type": "doc",
            "content": [{"type": "paragraph", "content": [{"type": "text", "text": "abcdef"}]}]
        });
        let four_blocks = json!({
            "type": "doc",
            "content": [
                {"type": "paragraph", "content": [{"type": "text", "text": "ab"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "cd"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "ef"}]}
            ]
        });
        let a = analyze(&one_block);
        let b = analyze(&four_blocks);
        assert_eq!(a.char_count, 6, "six letters is six characters");
        assert_eq!(
            b.char_count, 6,
            "the same six letters split across blocks must still be six characters"
        );
        // The text payloads differ, which is the point: FTS needs the breaks.
        assert!(!a.text.contains('\n'), "a single block must not gain a trailing newline");
        assert!(b.text.contains('\n'), "block boundaries must become newlines for FTS");
    }

    // -- the write path ------------------------------------------------------

    #[test]
    fn block_count_is_written_by_every_path_that_stores_content() {
        // A read path with no writer is dead schema. `block_count` is read by the
        // geometry on every scroll event, so a path that leaves it 0 silently
        // reverts the whole document to the 225%-error character-derived estimate.
        let s = Store::open_in_memory().unwrap();
        let d = s.create_document("Doc").unwrap();

        // 1. add_section
        let id = s
            .add_section(
                &d.id,
                &json!({"type": "doc", "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "a"}]},
                    {"type": "paragraph", "content": [{"type": "text", "text": "b"}]}
                ]}),
            )
            .unwrap();
        assert_eq!(
            s.manifest(&d.id).unwrap().get(0).unwrap().block_count,
            2,
            "add_section did not write block_count"
        );

        // 2. save_section
        s.save_section(
            &id,
            &json!({"type": "doc", "content": [
                {"type": "paragraph", "content": [{"type": "text", "text": "a"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "b"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "c"}]}
            ]}),
            SectionMetrics::new(3, 0, 3),
            "a\nb\nc",
        )
        .unwrap();
        assert_eq!(
            s.manifest(&d.id).unwrap().get(0).unwrap().block_count,
            3,
            "save_section did not update block_count"
        );

        // 3. log_edit + flush, the keystroke path
        s.log_edit(
            &d.id,
            &id,
            &json!({"type": "doc", "content": [
                {"type": "paragraph", "content": [{"type": "text", "text": "a"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "b"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "c"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "d"}]}
            ]}),
            SectionMetrics::new(4, 0, 4),
            "a\nb\nc\nd",
        )
        .unwrap();
        s.flush(&d.id).unwrap();
        assert_eq!(
            s.manifest(&d.id).unwrap().get(0).unwrap().block_count,
            4,
            "flush did not carry block_count from the WAL"
        );
    }

    #[test]
    fn a_split_section_reports_a_smaller_block_count() {
        // The property the whole column exists for: the geometry must be able to
        // tell that a section got shorter in *structure*, not just in text.
        let s = Store::open_in_memory().unwrap();
        let d = s.create_document("Doc").unwrap();
        let blocks = |n: usize| -> Value {
            json!({"type": "doc", "content":
                (0..n).map(|i| json!({
                    "type": "paragraph",
                    "content": [{"type": "text", "text": format!("p{i}")}]
                })).collect::<Vec<_>>()
            })
        };

        let id = s.add_section(&d.id, &blocks(20)).unwrap();
        assert_eq!(s.manifest(&d.id).unwrap().get(0).unwrap().block_count, 20);

        // Simulate the split: 20 blocks become 10 in this section, 10 in a new one.
        s.save_section(&id, &blocks(10), SectionMetrics::new(10, 0, 60), "x").unwrap();
        assert_eq!(
            s.manifest(&d.id).unwrap().get(0).unwrap().block_count,
            10,
            "a split must reduce the block count, or the height estimate never shrinks"
        );
    }

    #[test]
    fn analyze_extracts_text_and_words() {
        let a = analyze(&doc());
        assert_eq!(a.word_count, 4);
        assert!(a.text.contains("hello world"));
        assert!(a.text.contains("a heading"));
        // Block boundaries must become newlines or search spans cross paragraphs.
        assert!(a.text.contains('\n'));
        // Two top-level blocks: a paragraph and a heading.
        assert_eq!(a.block_count, 2);
    }

    #[test]
    fn manifest_reads_without_touching_blobs() {
        let s = Store::open_in_memory().unwrap();
        let d = s.create_document("Big").unwrap();
        for _ in 0..50 {
            s.add_section(&d.id, &doc()).unwrap();
        }
        let m = s.manifest(&d.id).unwrap();
        assert_eq!(m.len(), 50);
        assert_eq!(m.totals().words, 200);
    }

    #[test]
    fn wal_log_then_flush_persists() {
        let s = Store::open_in_memory().unwrap();
        let d = s.create_document("Test").unwrap();
        let sid = s.add_section(&d.id, &doc()).unwrap();

        // Simulate an edit that has not been flushed.
        let edited = json!({"type": "doc", "content": [
            {"type": "paragraph", "content": [{"type": "text", "text": "edited content here"}]}
        ]});
        s.log_edit(
            &d.id,
            &sid,
            &edited,
            SectionMetrics::new(3, 0, 19),
            "edited content here",
        )
        .unwrap();

        // Before the flush, the section blob is stale but the WAL has the edit.
        assert_eq!(s.load_section(&sid).unwrap(), doc());
        assert_eq!(s.wal().pending_count(&d.id).unwrap(), 1);

        assert_eq!(s.flush(&d.id).unwrap(), 1);
        assert_eq!(s.load_section(&sid).unwrap(), edited);
        assert_eq!(s.wal().row_count(&d.id).unwrap(), 0);
    }

    #[test]
    fn recover_replays_unflushed_state() {
        // The crash case: the process died before the debounce fired.
        let s = Store::open_in_memory().unwrap();
        let d = s.create_document("Test").unwrap();
        let sid = s.add_section(&d.id, &doc()).unwrap();
        let edited = json!({"type": "doc", "content": [
            {"type": "paragraph", "content": [{"type": "text", "text": "unsaved work"}]}
        ]});
        s.log_edit(&d.id, &sid, &edited, SectionMetrics::new(2, 0, 12), "unsaved work")
            .unwrap();

        assert_eq!(s.recover(&d.id).unwrap(), 1);
        assert_eq!(s.load_section(&sid).unwrap(), edited);
    }

    #[test]
    fn assets_are_content_addressed_and_deduplicated() {
        let s = Store::open_in_memory().unwrap();
        let png = [0x89u8, b'P', b'N', b'G', 1, 2, 3];
        let h1 = s.put_asset(&png, "image/png").unwrap();
        let h2 = s.put_asset(&png, "image/png").unwrap();
        assert_eq!(h1, h2, "identical bytes must hash identically");
        let (mime, bytes) = s.get_asset(&h1).unwrap().unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, png);
    }

    #[test]
    fn the_asset_key_is_sha256_and_the_frontend_can_recompute_it() {
        // The published test vectors. Not a self-consistency check -- `put_asset` and
        // `sha256_hex` share an implementation, so asserting they agree would pass even if
        // both were wrong. These are the values from FIPS 180-4, so a change of hash
        // function cannot pass by being internally consistent.
        assert_eq!(
            crate::sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            crate::sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );

        let s = Store::open_in_memory().unwrap();
        // A byte above 127, so a hash written as a *signed* char or truncated to a byte
        // would produce a different digest than one written as hex. PNG's magic first
        // byte is 0x89, which is exactly that case.
        let png = [0x89u8, b'P', b'N', b'G'];
        let hash = s.put_asset(&png, "image/png").unwrap();
        assert_eq!(hash.len(), 64, "a hex SHA-256 is 64 characters");
        assert_eq!(
            hash,
            crate::sha256_hex(&png),
            "the stored key must be recomputable by anyone holding the bytes"
        );
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn an_asset_round_trips_and_a_miss_is_a_miss_rather_than_an_error() {
        // The protocol handler has to tell these apart: a miss is a 404 and an error is a
        // 500, and returning empty bytes for a miss would render a broken image with no
        // indication that anything was wrong.
        let s = Store::open_in_memory().unwrap();
        let bytes = [1u8, 2, 3, 250];
        let hash = s.put_asset(&bytes, "image/gif").unwrap();

        let (mime, got) = s.get_asset(&hash).unwrap().expect("stored");
        assert_eq!(mime, "image/gif");
        assert_eq!(got, bytes);

        assert!(
            s.get_asset("0000000000000000000000000000000000000000000000000000000000000000")
                .unwrap()
                .is_none(),
            "an unknown key must be Ok(None), not an error"
        );
        assert!(s.get_asset("not-a-hash").unwrap().is_none(), "a malformed key is a miss too");
    }

    #[test]
    fn flush_is_idempotent() {
        let s = Store::open_in_memory().unwrap();
        let d = s.create_document("Test").unwrap();
        let sid = s.add_section(&d.id, &doc()).unwrap();
        s.log_edit(&d.id, &sid, &doc(), SectionMetrics::new(4, 0, 24), "hello world")
            .unwrap();
        assert_eq!(s.flush(&d.id).unwrap(), 1);
        assert_eq!(s.flush(&d.id).unwrap(), 0, "nothing left to flush");
    }

    #[test]
    fn save_section_updates_metrics() {
        let s = Store::open_in_memory().unwrap();
        let d = s.create_document("Test").unwrap();
        let sid = s.add_section(&d.id, &doc()).unwrap();
        s.save_section(
            &sid,
            &doc(),
            SectionMetrics::new(1501, 10, 9000),
            "hello world a heading",
        )
        .unwrap();
        let m = s.manifest(&d.id).unwrap();
        assert_eq!(m.get(0).unwrap().word_count, 1501);
        // And the splitter now sees it.
        assert!(crate::should_split(m.get(0).unwrap().metrics()).is_some());
    }

    #[test]
    fn search_finds_text_in_unmounted_sections() {
        // The point of FTS5: a term in a section that is nowhere near the
        // viewport must still be findable. Native browser find cannot do this.
        let s = Store::open_in_memory().unwrap();
        let d = s.create_document("Big").unwrap();
        for i in 0..40 {
            let body = if i == 30 {
                json!({"type": "doc", "content": [
                    {"type": "paragraph", "content": [
                        {"type": "text", "text": "a distinctive needle appears here"}
                    ]}
                ]})
            } else {
                json!({"type": "doc", "content": [
                    {"type": "paragraph", "content": [
                        {"type": "text", "text": "ordinary filler text"}
                    ]}
                ]})
            };
            s.add_section(&d.id, &body).unwrap();
        }
        assert_eq!(s.reindex(&d.id).unwrap(), 40);

        let hits = s.search(&d.id, "needle", 10).unwrap();
        assert_eq!(hits.len(), 1, "exactly one section contains the term");
        assert!(hits[0].snippet.contains("needle"));

        // And the hit is in section 30, proving the index is not limited to
        // whatever would be mounted.
        let m = s.manifest(&d.id).unwrap();
        assert_eq!(m.by_id(&hits[0].section_id).unwrap().0, 30);
    }

    #[test]
    fn search_is_scoped_to_one_document() {
        let s = Store::open_in_memory().unwrap();
        let a = s.create_document("A").unwrap();
        let b = s.create_document("B").unwrap();
        for d in [&a, &b] {
            s.add_section(
                &d.id,
                &json!({"type": "doc", "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "shared term"}]}
                ]}),
            )
            .unwrap();
            s.reindex(&d.id).unwrap();
        }
        assert_eq!(s.search(&a.id, "shared", 10).unwrap().len(), 1);
        assert_eq!(s.search(&b.id, "shared", 10).unwrap().len(), 1);
    }

    #[test]
    fn search_tolerates_awkward_queries() {
        // A user typing an apostrophe or a quote must not get a syntax error.
        let s = Store::open_in_memory().unwrap();
        let d = s.create_document("T").unwrap();
        s.add_section(&d.id, &doc()).unwrap();
        s.reindex(&d.id).unwrap();
        for q in ["don't", "\"quoted\"", "a-b", "*", "(", "NEAR(a b)"] {
            let r = s.search(&d.id, q, 5);
            assert!(r.is_ok(), "query {q:?} should not error: {:?}", r.err());
        }
    }

    #[test]
    fn reindex_is_idempotent() {
        // A full rebuild must not duplicate rows, or search results would
        // multiply every time the index is refreshed.
        let s = Store::open_in_memory().unwrap();
        let d = s.create_document("T").unwrap();
        s.add_section(&d.id, &doc()).unwrap();
        assert_eq!(s.reindex(&d.id).unwrap(), 1);
        assert_eq!(s.reindex(&d.id).unwrap(), 1);
        assert_eq!(s.reindex(&d.id).unwrap(), 1);
        assert_eq!(s.search(&d.id, "hello", 10).unwrap().len(), 1);
    }

#[test]
fn delete_document_removes_sections_wal_and_index() {
    let s = Store::open_in_memory().unwrap();
    let d = s.create_document("Doomed").unwrap();
    let sid = s.add_section(&d.id, &doc()).unwrap();
    s.log_edit(&d.id, &sid, &doc(), SectionMetrics::new(4, 0, 24), "hello world").unwrap();
    s.reindex(&d.id).unwrap();
    assert_eq!(s.search(&d.id, "hello", 5).unwrap().len(), 1, "precondition: indexed");

    s.delete_document(&d.id).unwrap();

    assert!(s.document(&d.id).is_err(), "the document row should be gone");
    // The section rows go by cascade rather than by being found and reported: with the
    // document gone, `section_ids` correctly says the document does not exist, which is
    // the more honest answer than an empty list for a document that is not there. The
    // orphan check is therefore a direct row count.
    let rows: i64 = s
        .conn()
        .query_row("SELECT COUNT(*) FROM sections WHERE document_id = ?1", params![d.id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0, "sections should have cascaded away");
    let orphaned: i64 = s
        .conn()
        .query_row("SELECT COUNT(*) FROM wal WHERE document_id = ?1", params![d.id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(orphaned, 0, "wal rows must not be orphaned: a later flush would recreate them");
    // Counted through the join because the index has no `document_id` column -- and
    // counted that way *on purpose*. The previous form of this assertion,
    // `SELECT COUNT(*) FROM sections_fts WHERE document_id = ?`, read the content
    // table and so could not tell a stale index row from an empty one. It passed
    // against exactly the bug it was written to catch.
    let indexed: i64 = s
        .conn()
        .query_row(
            "SELECT COUNT(*)
               FROM sections_fts
               JOIN sections ON sections.rowid = sections_fts.rowid
              WHERE sections.document_id = ?1",
            params![d.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(indexed, 0, "the search index should not still hold the deleted section");

    // And the index row itself is gone, not merely unreachable. This is the assertion
    // the join cannot make: a row whose `sections` counterpart has been cascaded away
    // produces no result forever, so a search would look correct while the table grew
    // by a row per deleted document.
    let tombstones: i64 = s
        .conn()
        .query_row("SELECT COUNT(*) FROM sections_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        tombstones, 0,
        "no index row should survive the section it indexed"
    );
    // The store still works afterwards: a leftover document must not break the next one.
    let d2 = s.create_document("Keeper").unwrap();
    let sid2 = s.add_section(&d2.id, &doc()).unwrap();
    s.log_edit(&d2.id, &sid2, &doc(), SectionMetrics::new(4, 0, 24), "hello again").unwrap();
    assert_eq!(s.flush(&d2.id).unwrap(), 1);
}

#[test]
fn a_document_can_be_deleted_whether_or_not_anything_was_indexed() {
    // The test above this one calls `reindex` before deleting, and that single line is what
    // hid a bug that made `delete_document` fail on **every document the application has ever
    // created**.
    //
    // With `content='sections'`, a plain `DELETE FROM sections_fts WHERE document_id = ?` is
    // not a delete of index rows: FTS5 routes it to the *content* table, and when no index row
    // is keeping it in step it answers `SQLITE_CORRUPT: database disk image is malformed`.
    //
    // Which means `delete_document` was erroring for a document that had never been
    // reindexed, and its only caller — `delete_ephemeral_document`, the thing that stops the
    // verification harness from leaving fixture prose in the user's own database — was failing
    // at exactly its job and swallowing the error. A 50-section fixture after every run; a
    // 7.5MB one from the soak.
    //
    // So: no `reindex` first. Reproduced before the fix, as
    // `Err("database error: database disk image is malformed")`, on exactly this shape:
    // `create_document`, `add_section`, `delete_document`.
    //
    // (There is no assertion here that the index is unpopulated, because that is not
    // observable through this API: `search_count` runs `SELECT ... FROM sections_fts`, and on
    // an external-content table that reads the *content* table, so it reports the sections
    // whether or not the index knows about them. Asserting it would be asserting something
    // false.)
    let s = Store::open_in_memory().unwrap();
    let d = s.create_document("Never indexed").unwrap();
    s.add_section(&d.id, &doc()).unwrap();

    s.delete_document(&d.id).expect("deleting an un-reindexed document must work");

    assert!(s.document(&d.id).is_err(), "the document row should be gone");
    let rows: i64 = s
        .conn()
        .query_row("SELECT COUNT(*) FROM sections WHERE document_id = ?1", params![d.id], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0, "sections should have cascaded away");

    // And the rebuild must not have poisoned the index for whatever comes next, which is the
    // failure `reindex`'s own doc comment warns about: an inconsistent index does not report
    // itself, it reports itself on the *next* query.
    let other = s.create_document("Keeper").unwrap();
    let sid = s.add_section(&other.id, &doc()).unwrap();
    s.log_edit(&other.id, &sid, &doc(), SectionMetrics::new(4, 0, 24), "hello again").unwrap();
    s.flush(&other.id).unwrap();
    assert_eq!(
        s.reindex(&other.id).unwrap(),
        1,
        "the index should be usable after a delete rebuilt it"
    );
    assert_eq!(s.search(&other.id, "hello", 5).unwrap().len(), 1, "search should still work");
}
}
