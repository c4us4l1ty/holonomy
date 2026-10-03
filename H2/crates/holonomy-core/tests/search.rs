//! Search, end to end, through the paths the application actually uses.
//!
//! # Why this suite exists separately from the unit tests in `store.rs`
//!
//! Because the search tests already in `store.rs` were green for the entire life of a
//! search feature that had never once run.
//!
//! `reindex` had no caller anywhere in the application. Every search test called
//! `reindex` itself before asserting, so each one proved that *a* full rebuild makes text
//! findable -- and nothing proved that anything rebuilt it. Worse, the assertion could not
//! have caught it: against an external-content FTS5 table,
//! `SELECT COUNT(*) FROM sections_fts WHERE document_id = ?` reads the *content* table,
//! so `search_count` reported the number of sections in a document and called it the size
//! of the index. A test asserting "40 indexed" was asserting "40 sections exist".
//!
//! So the rule this suite enforces is narrow and deliberate: **nothing in here calls
//! `reindex`**. The index has to be current because the write paths make it current. The
//! one test that does call `reindex` is the one about `reindex`.

use holonomy_core::schema::SCHEMA_VERSION;
use holonomy_core::Store;
use serde_json::json;

/// A section whose body is a single paragraph.
fn section(text: &str) -> serde_json::Value {
    json!({"type": "doc", "content": [
        {"type": "paragraph", "content": [{"type": "text", "text": text}]}
    ]})
}

fn hits(s: &Store, doc: &str, q: &str) -> Vec<String> {
    s.search(doc, q, 10)
        .unwrap()
        .into_iter()
        .map(|h| h.section_id)
        .collect()
}

#[test]
fn text_is_findable_without_anybody_having_reindexed() {
    // The regression test for the whole reason this suite was written. No `reindex` call
    // anywhere: `add_section` is the only operation, exactly as the application performs
    // it, and the text has to be findable because of that and nothing else.
    let s = Store::open_in_memory().unwrap();
    let d = s.create_document("Indexing").unwrap();
    s.add_section(&d.id, &section("a distinctive needle appears here")).unwrap();

    assert_eq!(
        hits(&s, &d.id, "needle"),
        vec![s.section_ids(&d.id).unwrap()[0].clone()],
        "text added by the application must be findable, not only text a test indexed by hand"
    );
}

#[test]
fn an_edit_replaces_the_indexed_text_rather_than_adding_to_it() {
    // The half of the contentless-table design that the external-content schema could not
    // express. Deleting an index row there required handing FTS5 the row's *original*
    // column values, and a database does not keep what it indexed once the section has
    // been edited -- so the superseded text stayed findable. That is the specific behaviour
    // this asserts, and it is why the schema had to change rather than the call sites.
    let s = Store::open_in_memory().unwrap();
    let d = s.create_document("Edits").unwrap();
    let sid = s.add_section(&d.id, &section("the old wording is here")).unwrap();

    assert_eq!(hits(&s, &d.id, "wording").len(), 1, "precondition: the original is findable");

    s.log_edit(
        &d.id,
        &sid,
        &section("the new phrasing is here"),
        holonomy_core::SectionMetrics::new(5, 0, 23),
        "the new phrasing is here",
    )
    .unwrap();
    s.flush(&d.id).unwrap();

    assert!(
        hits(&s, &d.id, "wording").is_empty(),
        "text that was edited away must stop being findable; otherwise search reports \
         something the document no longer says"
    );
    assert_eq!(
        hits(&s, &d.id, "phrasing"),
        vec![sid],
        "the replacement text must be findable"
    );
}

#[test]
fn repeated_edits_do_not_accumulate_index_rows() {
    // A stale-row bug here is invisible to every other test in this file: a section
    // containing a term still matches, and the row count is the only thing that shows it.
    let s = Store::open_in_memory().unwrap();
    let d = s.create_document("Churn").unwrap();
    let sid = s.add_section(&d.id, &section("version zero")).unwrap();

    for i in 1..=25 {
        let text = format!("version {i}");
        s.log_edit(
            &d.id,
            &sid,
            &section(&text),
            holonomy_core::SectionMetrics::new(2, 0, text.len() as u32),
            &text,
        )
        .unwrap();
        s.flush(&d.id).unwrap();
    }

    assert_eq!(
        s.search_count(&d.id).unwrap(),
        1,
        "25 edits to one section must leave one indexed row, not 26"
    );
    assert!(hits(&s, &d.id, "zero").is_empty(), "the first version must be gone");
    assert_eq!(hits(&s, &d.id, "25").len(), 1, "the last version must be findable");
}

#[test]
fn deleting_a_section_takes_its_text_out_of_the_index() {
    let s = Store::open_in_memory().unwrap();
    let d = s.create_document("Deletes").unwrap();
    let keep = s.add_section(&d.id, &section("keeper text")).unwrap();
    let drop = s.add_section(&d.id, &section("ephemeral text")).unwrap();

    s.delete_section(&drop).unwrap();

    assert!(hits(&s, &d.id, "ephemeral").is_empty());
    assert_eq!(hits(&s, &d.id, "keeper"), vec![keep]);
    let rows: i64 = s
        .conn()
        .query_row("SELECT COUNT(*) FROM sections_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1, "the deleted section's index row must be gone, not orphaned");
}

#[test]
fn search_is_scoped_to_the_document_it_was_asked_about() {
    // Two documents, the same word in both. The index has no `document_id` column -- it
    // is a contentless table and cannot usefully carry one -- so the scoping is entirely
    // the join's doing, and this is the test for it.
    let s = Store::open_in_memory().unwrap();
    let a = s.create_document("A").unwrap();
    let b = s.create_document("B").unwrap();
    s.add_section(&a.id, &section("shared term in A")).unwrap();
    s.add_section(&b.id, &section("shared term in B")).unwrap();

    assert_eq!(hits(&s, &a.id, "shared").len(), 1);
    assert_eq!(hits(&s, &b.id, "shared").len(), 1);
    assert!(hits(&s, "no-such-document", "shared").is_empty());
}

#[test]
fn a_hit_carries_a_snippet_that_marks_the_match() {
    // FTS5's `snippet()` is unavailable on a contentless table, so this text is built in
    // `store.rs`. It is a user-visible part of a result, and "the result is correct but
    // shows nothing" is a plausible regression rather than a theoretical one.
    let s = Store::open_in_memory().unwrap();
    let d = s.create_document("Snippets").unwrap();
    let filler = "lorem ipsum ".repeat(60);
    s.add_section(&d.id, &section(&format!("{filler} NEEDLE {filler}"))).unwrap();

    let found = s.search(&d.id, "needle", 5).unwrap();
    assert_eq!(found.len(), 1);
    assert!(
        found[0].snippet.contains("<b>NEEDLE</b>"),
        "the matched term should be marked, got {:?}",
        found[0].snippet
    );
    assert!(
        found[0].snippet.len() < filler.len(),
        "a snippet is a window, not the whole section"
    );
}

#[test]
fn the_index_survives_closing_and_reopening_the_document() {
    // The index is a cache of `sections`, not the source of truth, so it is allowed to be
    // rebuilt -- but a *session* must not have to. A user who searches, closes, and
    // reopens should not find their own document has become unsearchable.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("d.holo");

    let (doc_id, sid) = {
        let s = Store::open(&path).unwrap();
        let d = s.create_document("Persisted").unwrap();
        let sid = s.add_section(&d.id, &section("persisted needle")).unwrap();
        (d.id, sid)
    };

    let s = Store::open(&path).unwrap();
    assert_eq!(hits(&s, &doc_id, "needle"), vec![sid]);
    assert_eq!(s.search_count(&doc_id).unwrap(), 1);
}

#[test]
fn an_index_that_was_never_built_is_repaired_rather_than_reported_as_empty() {
    // `reindex` is still the answer to "this file's index is wrong" -- a document
    // imported from elsewhere, or a file whose writes predate the index. It is *not* on
    // the edit path, and the difference is that here it is called explicitly and the
    // result is checked, rather than being something the user depends on happening.
    let s = Store::open_in_memory().unwrap();
    let d = s.create_document("Repair").unwrap();
    let sid = s.add_section(&d.id, &section("needing repair")).unwrap();
    assert_eq!(hits(&s, &d.id, "repair").len(), 1, "precondition: indexed by the write path");

    // Wipe the index behind the store's back, the way a partial failure or a file
    // written by an older build would leave it.
    s.conn().execute("DELETE FROM sections_fts", []).unwrap();
    assert!(hits(&s, &d.id, "repair").is_empty(), "the index really is empty");

    assert_eq!(s.reindex(&d.id).unwrap(), 1);
    assert_eq!(hits(&s, &d.id, "repair"), vec![sid]);
}

#[test]
fn reindex_is_idempotent_and_replaces_rather_than_appends() {
    // Three rebuilds must leave exactly the rows they started with. A `rebuild` command
    // could not drift; a hand-written delete-and-repopulate can, and this is the
    // assertion that says it does not.
    let s = Store::open_in_memory().unwrap();
    let d = s.create_document("Idempotent").unwrap();
    for i in 0..5 {
        s.add_section(&d.id, &section(&format!("section number {i}"))).unwrap();
    }
    for _ in 0..3 {
        assert_eq!(s.reindex(&d.id).unwrap(), 5);
    }
    assert_eq!(s.search(&d.id, "number", 20).unwrap().len(), 5);
}

#[test]
fn an_older_file_is_migrated_in_place_rather_than_refused() {
    // Schema version 1 to 2 replaces the search index outright. Before the upgrade path
    // existed, `Store::init` accepted only "no meta table" or "exactly this version", so
    // the first schema change since the `.holo` format landed would have made every
    // existing document unreadable -- and the error names a version number, which is not
    // something a user can act on.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.holo");

    {
        let s = Store::open(&path).unwrap();
        let d = s.create_document("Written by v1").unwrap();
        s.add_section(&d.id, &section("text that must survive the migration")).unwrap();
        // Wind the file back to what v1 wrote: the old external-content index, and a
        // v1 version number. The section rows are untouched and must not be touched.
        s.conn().execute_batch("DROP TABLE sections_fts").unwrap();
        s.conn()
            .execute_batch(
                "CREATE VIRTUAL TABLE sections_fts USING fts5(
                     plain_text, document_id UNINDEXED,
                     content='sections', content_rowid='rowid')",
            )
            .unwrap();
        s.conn()
            .execute("UPDATE meta SET value = '1' WHERE key = 'schema_version'", [])
            .unwrap();
        s.conn().execute_batch("PRAGMA user_version = 1").unwrap();
    }

    let s = Store::open(&path).unwrap();
    let version: String = s
        .conn()
        .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION.to_string(), "the file should have been brought forward");

    let doc_id: String = s
        .conn()
        .query_row("SELECT id FROM documents LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let rows: i64 = s
        .conn()
        .query_row("SELECT COUNT(*) FROM sections WHERE document_id = ?1", [doc_id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1, "the migration must not touch the user's sections");
}

#[test]
fn a_file_from_a_newer_holonomy_is_still_refused() {
    // The other half of the upgrade decision, and the half that must not move. This build
    // cannot know what a later migration did, and guessing risks writing rows a newer
    // Holonomy would misread.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("future.holo");

    {
        let s = Store::open(&path).unwrap();
        s.create_document("From the future").unwrap();
        s.conn()
            .execute(
                "UPDATE meta SET value = '99' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
    }

    match Store::open(&path) {
        Ok(_) => panic!("a file from a newer Holonomy must not be opened by this build"),
        Err(e) => assert!(
            matches!(e, holonomy_core::Error::SchemaVersion { found: 99, .. }),
            "expected a version refusal, got {e}"
        ),
    }
}
