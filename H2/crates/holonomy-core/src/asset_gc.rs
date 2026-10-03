//! Reclaiming assets no section references any more.
//!
//! # The leak this exists to close
//!
//! `assets` is content-addressed and has **no `document_id` and no foreign key**. That
//! is deliberate — a repeated logo must be stored once, and a document synced to a device
//! that already has the bytes must be able to name them — but it means the table cannot
//! tell a live asset from a dead one on its own. Nothing in the schema ever deletes a row.
//!
//! So deleting an image from a section leaves its bytes in the file forever, and so does
//! deleting the whole document: `delete_document` removes the `sections` rows, which
//! cascades nothing, because nothing points at `assets`.
//!
//! # Why the scan reads *all* sections, not just this document's
//!
//! The tempting implementation is to look at one document and delete everything it does
//! not mention. That deletes the shared logo out from under every other document that
//! uses it — a document-wide sweep that is not a sweep, it is a data-loss bug with a
//! reassuring name.
//!
//! So reachability is computed over the **whole file**, and `document_id` is used only to
//! decide *whether to run the sweep at all* and to report what was reclaimed. An asset is
//! deleted only when no section in any document names it. The cost is one decompression
//! pass over every section, which is why this is a save/close-time or manual operation
//! and never runs on a keystroke.
//!
//! # Why it decompresses rather than scanning the blob
//!
//! `content_zstd` is compressed, so the literal `holo-asset://…` bytes are not in the file
//! to be matched. A substring search over the compressed blob would find nothing and would
//! cheerfully report every asset as an orphan. Decompression is the price of correctness.

use crate::error::Result;
use crate::store::Store;
use serde_json::Value;
use std::collections::{BTreeSet, HashSet};

/// What a sweep reclaimed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Rows removed from `assets`.
    pub deleted: u32,
    /// Bytes those rows occupied. What the file gives back after a checkpoint.
    pub bytes_freed: i64,
    /// Assets still referenced by some section and therefore kept.
    pub retained: u32,
    /// Assets in the table that no section in the file references.
    pub orphans_found: u32,
    /// Sections decompressed to answer the question.
    pub sections_scanned: u32,
}

/// The `holo-asset://` prefix, borrowed from the one definition of the scheme.
///
/// A plain literal rather than `concat!`, which was here to make the constant look derived
/// from the scheme and is in fact longer than the literal it wraps. Clippy (`useless_concat`)
/// is right about that, and CI runs `-D warnings`.
const PREFIX: &str = "holo-asset://";

/// Every asset hash named anywhere inside `json`.
///
/// # Why a walk rather than a substring search
///
/// A substring search over the serialized JSON would find `holo-asset://…` inside a
/// *code block* — a fenced snippet showing someone how to embed an image — and count it
/// as a live reference. That is the safe direction: it retains an asset that is not
/// needed. The unsafe direction is missing a real reference, so a walk that only accepts
/// the shape of an actual `src` attribute is the right trade.
///
/// It also has to survive the URL appearing in an `attrs` object rather than in a string
/// node, because that is where ProseMirror puts it: `{"type":"image","attrs":{"src":…}}`.
fn hashes_in(json: &Value, out: &mut HashSet<String>) {
    match json {
        Value::String(s) => {
            if let Some(rest) = s.strip_prefix(PREFIX) {
                if is_hash(rest) {
                    out.insert(rest.to_string());
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|i| hashes_in(i, out)),
        Value::Object(map) => map.values().for_each(|v| hashes_in(v, out)),
        _ => {}
    }
}

/// 64 lowercase hex characters — the same grammar the protocol handler accepts.
///
/// Duplicated rather than shared because the handler lives in the shell crate and this is
/// in core, and because the check is four lines. `tests/` asserts the two agree.
fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Every asset hash referenced by any section in the file.
///
/// # Why this is file-wide, and what `document_id` is for
///
/// `assets` is shared across every document in the file, so reachability has to be too:
/// an asset that looks unreferenced by the document being swept may be the logo in the
/// footer of a different one. The caller passes `document_id` to decide *whether* to
/// sweep and to label the report; it deliberately does not narrow this scan. See the
/// module docs for the data-loss this avoids.
fn referenced_hashes(store: &Store) -> Result<(HashSet<String>, u32)> {
    let mut stmt = store
        .conn()
        .prepare("SELECT id, content_zstd FROM sections ORDER BY document_id, order_key")?;
    let mut rows = stmt.query([])?;

    let mut referenced = HashSet::new();
    let mut scanned = 0u32;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let blob: Vec<u8> = row.get(1)?;
        let value = crate::store::decode(&id, &blob)?;
        hashes_in(&value, &mut referenced);
        scanned += 1;
    }
    Ok((referenced, scanned))
}

/// Reclaim assets no section references.
///
/// Called on document close and from the manual "Vacuum / Optimize" action. Not called on
/// the edit path: the scan is O(total document bytes decompressed), and a keystroke is not
/// the place for that.
///
/// # What it will not delete
///
/// - An asset referenced by any section of any document. Shared assets are the common
///   case, not an edge case.
/// - An asset a section names in a form this does not recognise. A missed *format* keeps
///   bytes alive; a missed *reference* would show as a broken image, so the parser is
///   deliberately permissive and the deletion rule is deliberately strict.
pub fn sweep_orphaned_assets(store: &Store, document_id: &str) -> Result<SweepReport> {
    // An unknown document is a caller bug, not an empty sweep. Reporting "0 orphans"
    // for a document that does not exist would let a typo in the trigger look like a
    // clean bill of health.
    let exists: Option<i64> = store
        .conn()
        .query_row(
            "SELECT 1 FROM documents WHERE id = ?1",
            [document_id],
            |r| r.get(0),
        )
        .ok();
    if exists.is_none() {
        return Err(crate::error::Error::DocumentNotFound(document_id.to_string()));
    }

    let (referenced, sections_scanned) = referenced_hashes(store)?;

    // Every asset, and its size, so the report can say what was reclaimed rather than
    // only how many rows went. Sizes are collected *before* the delete, because after it
    // there is nothing left to measure — a report that said "12 rows freed" without a
    // byte count would leave the reader unable to tell a megabyte from a gigabyte.
    let mut stmt = store.conn().prepare("SELECT hash, byte_size FROM assets")?;
    let mut rows = stmt.query([])?;
    let mut orphans: BTreeSet<String> = BTreeSet::new();
    let mut orphan_bytes = 0i64;
    let mut retained = 0u32;
    while let Some(row) = rows.next()? {
        let hash: String = row.get(0)?;
        let byte_size: i64 = row.get(1)?;
        if referenced.contains(&hash) {
            retained += 1;
        } else {
            orphans.insert(hash);
            orphan_bytes += byte_size;
        }
    }

    let mut report = SweepReport {
        retained,
        orphans_found: orphans.len() as u32,
        sections_scanned,
        bytes_freed: 0,
        deleted: 0,
    };
    if orphans.is_empty() {
        return Ok(report);
    }

    // Delete by hash rather than `NOT IN (SELECT…)`, so the set of victims is exactly
    // what the scan decided and the statement cannot disagree with it.
    //
    // In a transaction because a partial sweep would leave the file in a state neither
    // the reachability scan nor the next sweep is prepared for: the report would claim
    // rows were freed that are still on disk.
    //
    // `unchecked_transaction` rather than `transaction`, because the latter needs
    // `&mut Connection` and `Store` hands out `&Connection` to every other operation.
    // This is the same choice `Store::init` makes for migrations.
    let tx = store.conn().unchecked_transaction()?;
    {
        let mut del = tx.prepare("DELETE FROM assets WHERE hash = ?1")?;
        for hash in &orphans {
            del.execute([hash])?;
        }
    }
    tx.commit()?;

    // Hand the freed pages back to the filesystem. A deleted row only returns its pages
    // to SQLite's free list, where the next write may reuse them; the file on disk does
    // not shrink until they are actually released. `PRAGMA incremental_vacuum` does that
    // in bounded steps, which is why the store is opened with `auto_vacuum = INCREMENTAL`
    // — without that setting this statement is a silent no-op and the sweep would report
    // bytes it did not return.
    //
    // A database created before that pragma cannot change mode without a full `VACUUM`,
    // so this may reclaim nothing there. That is not an error: the rows are gone either
    // way, and `backup_to` already writes a compacted copy.
    if report.deleted > 0 {
        store.conn().execute_batch("PRAGMA incremental_vacuum").ok();
    }

    report.deleted = orphans.len() as u32;
    report.bytes_freed = orphan_bytes;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn image_src(hash: &str) -> Value {
        json!({ "type": "image", "attrs": { "src": format!("holo-asset://{hash}"), "alt": "x" } })
    }

    #[test]
    fn a_hash_is_read_out_of_an_attrs_object_not_just_a_bare_string() {
        // Where ProseMirror actually puts it. A parser that only looked for a top-level
        // string would find nothing here and delete every image in the document.
        let mut out = HashSet::new();
        hashes_in(&image_src(&"a".repeat(64)), &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn a_hash_nested_deep_inside_a_table_is_still_found() {
        let mut out = HashSet::new();
        let h = "b".repeat(64);
        let doc = json!({
            "type": "doc",
            "content": [{
                "type": "table",
                "content": [{
                    "type": "tableRow",
                    "content": [{
                        "type": "tableCell",
                        "content": [{ "type": "paragraph", "content": [image_src(&h)] }]
                    }]
                }]
            }]
        });
        hashes_in(&doc, &mut out);
        assert_eq!(out.len(), 1, "an image inside a table cell is still referenced");
    }

    #[test]
    fn a_malformed_hash_is_not_a_reference() {
        // A short, uppercase, or path-bearing string is not an asset address. Counting it
        // would retain a row that nothing can resolve, which is a smaller failure than
        // the reverse, and is still not a reference.
        let mut out = HashSet::new();
        for bad in [
            "holo-asset://short",
            "holo-asset://../../etc/passwd",
            &format!("holo-asset://{}", "A".repeat(64)),
            "http://example.com/x.png",
        ] {
            hashes_in(&Value::String(bad.to_string()), &mut out);
        }
        assert!(out.is_empty(), "malformed addresses must not count as references");
    }

    #[test]
    fn a_hash_in_prose_is_kept_because_being_wrong_here_is_the_cheap_direction() {
        // A fenced code sample containing the URL. Retaining the bytes is harmless;
        // dropping them would break a document that mentions the scheme.
        let mut out = HashSet::new();
        let h = "c".repeat(64);
        let doc = json!({ "type": "codeBlock", "content": [{ "type": "text", "text": format!("holo-asset://{h}") }] });
        hashes_in(&doc, &mut out);
        assert_eq!(out.len(), 1);
    }
}

// ---------------------------------------------------------------------------
// Store-level behaviour
// ---------------------------------------------------------------------------

#[cfg(test)]
mod sweep_tests {
    use super::*;
    use crate::store::Store;
    use serde_json::json;

    /// A store with one document holding a single empty paragraph.
///
/// The first version of this helper took a list of hashes and built sections for them,
/// then indexed `nodes[0]` unconditionally — so every test that passed no hashes, which
/// is most of them, panicked on an empty array before reaching the assertion it existed
/// to make. The tests only need somewhere to hang a document id; the images they care
/// about they add themselves, where the reference is visible.
fn doc_with(title: &str) -> (Store, String) {
    let s = Store::open_in_memory().unwrap();
    let d = s.create_document(title).unwrap();
    s.add_section(&d.id, &json!({ "type": "doc", "content": [{ "type": "paragraph" }] }))
        .unwrap();
    (s, d.id)
}

    #[test]
    fn an_image_still_in_a_section_survives_the_sweep() {
        let (s, doc) = doc_with("keep");
        let png = [0x89u8, b'P', b'N', b'G', 7];
        let hash = s.put_asset(&png, "image/png").unwrap();
        // Put the image into an existing section by rewriting it.
        s.add_section(
            &doc,
            &json!({ "type": "doc", "content": [
                { "type": "image", "attrs": { "src": format!("holo-asset://{hash}") } }
            ]}),
        )
        .unwrap();

        let r = sweep_orphaned_assets(&s, &doc).unwrap();
        assert_eq!(r.deleted, 0, "a referenced image must not be deleted");
        assert!(s.get_asset(&hash).unwrap().is_some(), "the bytes must still be there");
        assert_eq!(r.retained, 1);
    }

    #[test]
    fn an_image_nothing_references_is_reclaimed() {
        let (s, doc) = doc_with("orphan");
        let png = [0x89u8, b'P', b'N', b'G', 9];
        let hash = s.put_asset(&png, "image/png").unwrap();
        assert!(s.get_asset(&hash).unwrap().is_some(), "precondition: stored");

        let r = sweep_orphaned_assets(&s, &doc).unwrap();
        assert_eq!(r.orphans_found, 1, "the image is unreferenced");
        assert_eq!(r.deleted, 1);
        assert_eq!(r.bytes_freed, png.len() as i64, "the report must state the bytes freed");
        assert!(
            s.get_asset(&hash).unwrap().is_none(),
            "the row must be gone from the assets table"
        );
    }

    #[test]
    fn deleting_the_image_node_reclaims_the_blob() {
        // The whole point, end to end: store, reference, delete the reference, sweep.
        // The reference is removed through the real write path -- a WAL edit followed by
        // a flush -- rather than by rewriting the row directly, because the sweep reads
        // what the write path actually left behind.
        let (s, doc) = doc_with("lifecycle");
        let mut png = vec![0x89u8, b'P', b'N', b'G'];
        png.extend(std::iter::repeat(0x5au8).take(4096));
        let hash = s.put_asset(&png, "image/png").unwrap();

        let sec = s.add_section(
            &doc,
            &json!({ "type": "doc", "content": [
                { "type": "image", "attrs": { "src": format!("holo-asset://{hash}") } }
            ]}),
        )
        .unwrap();

        // Still referenced: nothing is reclaimed.
        assert_eq!(sweep_orphaned_assets(&s, &doc).unwrap().deleted, 0);

        // The user deletes the image.
        let edited = json!({ "type": "doc", "content": [{ "type": "paragraph" }] });
        let analyzed = crate::store::analyze(&edited);
        s.log_edit(
            &doc,
            &sec,
            &edited,
            crate::split::SectionMetrics::new(analyzed.word_count, 0, analyzed.char_count),
            &analyzed.text,
        )
        .unwrap();
        s.flush(&doc).unwrap();

        let r = sweep_orphaned_assets(&s, &doc).unwrap();
        assert_eq!(r.deleted, 1, "the deleted image's bytes are now orphaned");
        assert!(s.get_asset(&hash).unwrap().is_none());
    }

    #[test]
    fn an_asset_used_by_another_document_is_not_deleted() {
        // The data-loss case. `assets` has no `document_id`, so an image shared by two
        // documents looks unreferenced by whichever one is being swept. Deleting it would
        // break the other document with no error anywhere.
        let s = Store::open_in_memory().unwrap();
        let shared_png = [0x89u8, b'P', b'N', b'G', 42];
        let shared = s.put_asset(&shared_png, "image/png").unwrap();

        let doc_a = s.create_document("a").unwrap();
        let doc_b = s.create_document("b").unwrap();
        // Only document B references it.
        s.add_section(
            &doc_b.id,
            &json!({ "type": "doc", "content": [
                { "type": "image", "attrs": { "src": format!("holo-asset://{shared}") } }
            ]}),
        )
        .unwrap();

        // Sweeping A must not take it.
        let r = sweep_orphaned_assets(&s, &doc_a.id).unwrap();
        assert_eq!(r.deleted, 0, "an asset another document uses must survive");
        assert!(
            s.get_asset(&shared).unwrap().is_some(),
            "the shared image must still resolve for document B"
        );

        // And sweeping B keeps it too, because B still references it.
        assert_eq!(sweep_orphaned_assets(&s, &doc_b.id).unwrap().deleted, 0);
    }

    #[test]
    fn a_deleted_document_releases_its_images() {
        // Without the sweep, `delete_document` leaves every figure in the file forever:
        // `assets` has no foreign key, so the cascade removes the `sections` rows and
        // nothing else. This is the largest single leak.
        let s = Store::open_in_memory().unwrap();
        let png = [0x89u8, b'P', b'N', b'G', 11];
        let hash = s.put_asset(&png, "image/png").unwrap();
        let d = s.create_document("doomed").unwrap();
        s.add_section(
            &d.id,
            &json!({ "type": "doc", "content": [
                { "type": "image", "attrs": { "src": format!("holo-asset://{hash}") } }
            ]}),
        )
        .unwrap();

        s.delete_document(&d.id).unwrap();
        assert!(
            s.get_asset(&hash).unwrap().is_some(),
            "precondition: deleting a document does not touch assets -- that is the leak"
        );

        // A sweep needs *some* document to be triggered from, so keep a second one.
        let survivor = s.create_document("survivor").unwrap();
        let r = sweep_orphaned_assets(&s, &survivor.id).unwrap();
        assert_eq!(r.deleted, 1);
        assert!(s.get_asset(&hash).unwrap().is_none());
    }

    #[test]
    fn sweeping_a_document_that_does_not_exist_is_an_error() {
        // Silently reporting "0 orphans" for a typo'd id would read as a clean bill of
        // health, which is the worst possible answer.
        let s = Store::open_in_memory().unwrap();
        let err = sweep_orphaned_assets(&s, "nope").unwrap_err();
        assert!(
            matches!(err, crate::error::Error::DocumentNotFound(_)),
            "expected DocumentNotFound, got {err:?}"
        );
    }

    #[test]
    fn a_sweep_with_nothing_to_do_reports_zero_rather_than_failing() {
        // The common case on every close. It must be cheap to call and must not error.
        let s = Store::open_in_memory().unwrap();
        let d = s.create_document("empty").unwrap();
        s.add_section(&d.id, &json!({ "type": "doc", "content": [{ "type": "paragraph" }] })).unwrap();
        let r = sweep_orphaned_assets(&s, &d.id).unwrap();
        assert_eq!(r, SweepReport { deleted: 0, bytes_freed: 0, retained: 0, orphans_found: 0, sections_scanned: 1 });
    }

    #[test]
    fn a_sweep_frees_the_pages_and_a_vacuum_returns_them_to_the_filesystem() {
        // The physical-size claim, and what it actually takes.
        //
        // The obvious version of this test deletes an asset, checkpoints, and asserts the
        // file is smaller. It does not hold, and the reason is worth writing down rather
        // than working around:
        //
        //   * Without `auto_vacuum`, a deleted row's pages go to SQLite's *free list* --
        //     reusable by the next write, never returned to the filesystem. `Store::init`
        //     therefore opens every new file with `PRAGMA auto_vacuum = INCREMENTAL`.
        //   * With it, `PRAGMA incremental_vacuum` still only truncates pages at the
        //     *end* of the file, and a bare call releases exactly one of them. A blob
        //     freed from the middle is not reachable that way at all; only a full
        //     `VACUUM` compacts it away. Measured on this fixture: 16 pages freed, and
        //     `incremental_vacuum(16)` returns one.
        //
        // So this asserts the two claims that are actually true -- the pages are freed,
        // and a vacuum compacts them out of the file -- instead of a checkpoint-based
        // shrink that passes only when the allocator happens to place the freed pages
        // last.
        let dir = std::env::temp_dir().join(format!("holonomy-sweep-{}", crate::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.holo");

        let s = Store::open(&path).unwrap();
        let d = s.create_document("big").unwrap();

        let auto_vacuum: i64 = s.conn().query_row("PRAGMA auto_vacuum", [], |r| r.get(0)).unwrap();
        assert_eq!(
            auto_vacuum, 2,
            "a store must open with auto_vacuum = INCREMENTAL, or no delete ever returns space"
        );

        // 64 KiB of image, referenced and then unreferenced.
        let png: Vec<u8> = std::iter::repeat(0x11u8).take(64 * 1024).collect();
        let hash = s.put_asset(&png, "image/png").unwrap();
        let sec = s.add_section(
            &d.id,
            &json!({ "type": "doc", "content": [
                { "type": "image", "attrs": { "src": format!("holo-asset://{hash}") } }
            ]}),
        )
        .unwrap();

        let edited = json!({ "type": "doc", "content": [{ "type": "paragraph" }] });
        let analyzed = crate::store::analyze(&edited);
        s.log_edit(
            &d.id,
            &sec,
            &edited,
            crate::split::SectionMetrics::new(analyzed.word_count, 0, analyzed.char_count),
            &analyzed.text,
        )
        .unwrap();
        s.flush(&d.id).unwrap();

        // Checkpoint before measuring: until the WAL is folded in, the bytes live in the
        // sidecar and the main file reads as nearly empty, so every comparison would be
        // against a number that was never true.
        s.checkpoint_truncate().unwrap();
        let before = std::fs::metadata(&path).unwrap().len();
        let freelist_before: i64 = s.conn().query_row("PRAGMA freelist_count", [], |r| r.get(0)).unwrap();
        assert_eq!(freelist_before, 0, "precondition: nothing is free yet");
        assert!(before >= 64 * 1024, "the image must be on disk before the sweep, got {before}");

        let r = sweep_orphaned_assets(&s, &d.id).unwrap();
        assert_eq!(r.deleted, 1);
        assert!(
            r.bytes_freed >= 64 * 1024,
            "the report should account for the image bytes, got {}",
            r.bytes_freed
        );

        // The pages are now free. This is the real, checkable effect of the sweep.
        s.checkpoint_truncate().unwrap();
        let freelist_after: i64 = s.conn().query_row("PRAGMA freelist_count", [], |r| r.get(0)).unwrap();
        assert!(
            freelist_after > 0,
            "deleting the asset must return its pages to the free list, freelist is {freelist_after}"
        );

        // And a vacuum compacts them out of the file. `backup_to` is the existing
        // `VACUUM INTO` path, which is what a manual "Vacuum / Optimize" uses.
        let compacted = dir.join("compacted.holo");
        let summary = s.backup_to(&compacted.to_string_lossy()).expect("VACUUM INTO should succeed");
        assert!(
            summary.bytes < before,
            "a vacuumed copy must be smaller: {before} -> {}",
            summary.bytes
        );

        drop(s);
        std::fs::remove_dir_all(&dir).ok();
    }
}