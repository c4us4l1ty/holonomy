//! Crash recovery: what survives an unclean exit.
//!
//! # The claim under test
//!
//! An application that is killed — a power cut, a container stop, a `kill -9`, a laptop
//! closing its lid — must lose at most the debounce window, and must not leave a document
//! SQLite cannot open. Those are two separate claims and they fail differently:
//!
//! - **Durability** is about the *logical* log. Every keystroke appends a row to the
//!   project's own `wal` table rather than rewriting a compressed section blob, so a crash
//!   leaves committed-but-unfolded rows behind. Recovery replays them.
//! - **Integrity** is about SQLite's own `-wal` sidecar, which the crash leaves non-empty
//!   and which the next `Connection::open` replays before returning.
//!
//! # Why this is a separate test file
//!
//! Because the interesting part cannot be arranged in a unit test. Every other suite here
//! uses `Store::open_in_memory`, where there is no sidecar to leave behind and no file to
//! reopen. Simulating a crash means dropping a real connection without closing it
//! cleanly and then examining the bytes on disk — which is only possible with a
//! file-backed store in its own directory.
//!
//! # Why `mem::forget` is how a crash is simulated, and `drop` is not
//!
//! The obvious way to crash a store in a test is to `drop` it. **That is a clean exit.**
//!
//! `rusqlite::Connection` closes the underlying `sqlite3` handle in `Drop`, and closing
//! the last handle to a WAL database checkpoints it and removes both sidecars. Measured on
//! this fixture: 2,492,632 bytes of `-wal` while the store is open, **0 bytes** the
//! instant it is dropped.
//!
//! So a suite that drops its stores tests recovery from nothing, and passes — which is
//! worse than having no test, because it reports the crash path as covered. The first
//! version of this file did exactly that and its own precondition assertion
//! (`a crash must leave a non-empty -wal sidecar`) caught it, which is the only reason it
//! was caught at all.
//!
//! `std::mem::forget` leaks the handle instead, so nothing closes and nothing checkpoints:
//! exactly the state a `kill -9`, a power cut or a container stop leaves behind. Forking a
//! child and signalling it would be the same simulation with more moving parts, and the
//! failure mode would be a harness bug reported as a database fact.

use holonomy_core::store::analyze;
use holonomy_core::{SectionMetrics, Store};
use serde_json::{json, Value};

/// A file-backed store in its own directory, so the sidecars can be inspected.
///
/// Each test gets its own `TempDir` because the sidecars are per-file state and a shared
/// one would let a leftover `-wal` from a previous test make a later one pass for the
/// wrong reason.
struct Crash {
    dir: tempfile::TempDir,
    path: std::path::PathBuf,
}

impl Crash {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("crash.holo");
        Self { dir, path }
    }

    fn open(&self) -> Store {
        Store::open(&self.path).expect("open")
    }

    /// Size of a sidecar, or 0 when it does not exist.
    ///
    /// Absent is 0 rather than an error: a store with nothing pending has no `-wal`, and
    /// that is the clean state rather than a missing file.
    fn sidecar(&self, suffix: &str) -> u64 {
        let p = self.sidecar_path(suffix);
        std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
    }

    /// Where a sidecar is, asserted to be beside the document and inside the temp dir.
    ///
    /// Reading `self.dir` is the point of this function rather than an incidental use to
    /// quiet a lint. SQLite writes `-wal` and `-shm` next to the database file by naming,
    /// not by configuration, so a sidecar found outside the temp directory would mean this
    /// test was reading some other run's leftover rather than its own crash's — and every
    /// size number in this file would then be about a different store. Comparing against
    /// `dir.path()` is what makes those numbers attributable to *this* store, which is the
    /// only reason they mean anything.
    fn sidecar_path(&self, suffix: &str) -> std::path::PathBuf {
        let p = self.path.with_extension(format!("holo{suffix}"));
        assert!(
            p.starts_with(self.dir.path()),
            "the sidecar for {} must be written beside the document, inside the temp dir; \
             it resolved to {}",
            self.path.display(),
            p.display()
        );
        p
    }

    /// SQLite's own verdict on the file, which is the only check that reads every page.
    fn integrity(&self) -> String {
        let store = self.open();
        store
            .conn()
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .expect("integrity_check")
    }
}

/// Abandon a store the way a killed process would: no close, no checkpoint.
///
/// `std::mem::forget` rather than `drop`, and the reason is load-bearing enough to be
/// worth the whole module header: dropping the handle *closes* the database, and closing
/// the last handle to a WAL database checkpoints it and deletes both sidecars. Dropping
/// therefore simulates a clean exit, and every test below would pass without SQLite's
/// replay ever running.
///
/// The handle is leaked rather than closed, so the on-disk state is left exactly as the
/// last commit produced it. Rust will not reclaim it, which is the point: the process ends
/// and the bytes stay.
fn crash_without_closing(store: Store) {
    std::mem::forget(store);
}

/// A section holding `blocks` paragraphs, each `words` words long.
fn section_json(blocks: usize, words: usize, tag: &str) -> Value {
    let body: String = (0..words).map(|i| format!("{tag}w{i} ")).collect();
    json!({
        "type": "doc",
        "content": (0..blocks)
            .map(|b| json!({"type": "paragraph", "content": [{"type": "text", "text": format!("{tag}b{b} {body}")}]}))
            .collect::<Vec<_>>()
    })
}

/// Commit an edit the way the typing path does: into the logical log, not into `sections`.
///
/// Returns the id of the section created, or the id edited.
fn commit(store: &Store, doc: &str, section: &str, json: &Value) {
    let a = analyze(json);
    store
        .log_edit(
            doc,
            section,
            json,
            SectionMetrics::new(a.word_count, 0, a.char_count),
            &a.text,
        )
        .expect("log_edit");
}

// ---------------------------------------------------------------------------
// The headline case: 50 committed writes, no clean exit
// ---------------------------------------------------------------------------

#[test]
fn fifty_unflushed_writes_survive_a_crash_intact() {
    // The directive's scenario, end to end. Fifty sections committed and never flushed,
    // the handle dropped without a checkpoint, then the file reopened.
    let crash = Crash::new();
    let (doc_id, ids) = {
        let store = crash.open();
        let doc = store.create_document("Crash test").expect("document");

        let mut ids = Vec::new();
        for i in 0..50 {
            let json = section_json(3, 40, &format!("s{i}"));
            ids.push(store.add_section(&doc.id, &json).expect("add section"));
            // And an *edit* on top, so the log carries both creates and updates.
            commit(&store, &doc.id, &ids[i], &section_json(4, 40, &format!("e{i}")));
        }

        // Nothing has been folded. This is the precondition the whole test rests on.
        //
        // Note what is *not* pending: `add_section` writes the `sections` row directly, so
        // the fifty rows are already durable and the count below is fifty. What recovery
        // has to supply is the fifty *edits* — which is the part that would otherwise be
        // lost, and it is the part asserted at the end.
        //
        // This asymmetry is deliberate and is worth stating rather than papering over with
        // a weaker assertion. Creating a section is rare and structural, so it is
        // committed directly. Editing one is the keystroke path, so it goes through the
        // log, which is what makes a crash cost at most the debounce window instead of a
        // blob rewrite per character.
        let pending = store.wal().row_count(&doc.id).expect("row count");
        assert_eq!(pending, 50, "precondition: 50 edits should be waiting in the log");
        let sections_now: i64 = store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sections WHERE document_id = ?1",
                [&doc.id],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(
            sections_now, 50,
            "precondition: the section rows are written directly, so they are already there. \
             If this were 0 the test would be asserting the wrong thing."
        );

        // The crash. No `flush`, no `graceful_shutdown`, no `drop` ceremony.
        crash_without_closing(store);
        (doc.id, ids)
    };

    // The sidecars must be there. A crash that left nothing behind would mean the writes
    // were already durable in the main file, and SQLite's replay — the thing under test —
    // would never run.
    assert!(
        crash.sidecar("-wal") > 0,
        "precondition: a crash must leave a non-empty -wal sidecar, found {} bytes",
        crash.sidecar("-wal")
    );

    // Reopen and let recovery do its work. `recover` is what `get_document_boot` calls
    // first, so it is the real path rather than a test-only entry point.
    let reopened = crash.open();
    let recovered = reopened.recover(&doc_id).expect("recover");
    assert_eq!(recovered, 50, "every pending row should have been replayed");

    // All fifty present, with the content of the *edit* rather than the create. Asserting
    // the edited shape is what distinguishes recovery from "the create happened to be
    // durable" — a test that only counted rows would pass on a fold that lost the edit.
    let manifest = reopened.manifest(&doc_id).expect("manifest");
    assert_eq!(manifest.len(), 50, "the manifest must list all 50 sections");
    assert_eq!(
        manifest.entries().iter().map(|e| e.block_count).sum::<u32>(),
        50 * 4,
        "block counts must describe the edited sections (4 blocks), not the created ones (3)"
    );

    for (i, id) in ids.iter().enumerate() {
        let json = reopened.load_section(id).expect("load section");
        let blocks = json["content"].as_array().expect("content array");
        assert_eq!(blocks.len(), 4, "section {i} should hold the edited 4 blocks");
        let text = blocks[0]["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            text.starts_with(&format!("e{i}b0 ")),
            "section {i} should hold the edit, not the original. got {text:.40?}"
        );
    }

    // And the logical log is empty afterwards, so a second crash cannot replay them twice.
    assert_eq!(
        reopened.wal().row_count(&doc_id).expect("rows"),
        0,
        "recovery must drain the log"
    );
}

#[test]
fn a_crashed_file_passes_sqlites_own_integrity_check() {
    // `Store::open` runs migrations and would report success on a structurally broken
    // file. `integrity_check` is SQLite walking every page itself, which is the only thing
    // here that would notice a torn write.
    let crash = Crash::new();
    {
        let store = crash.open();
        let doc = store.create_document("Torn").expect("document");
        for i in 0..50 {
            let id = store.add_section(&doc.id, &section_json(3, 40, &format!("t{i}"))).expect("add");
            commit(&store, &doc.id, &id, &section_json(5, 40, &format!("u{i}")));
        }
        crash_without_closing(store);
    }

    assert_eq!(crash.integrity(), "ok", "SQLite must consider the crashed file sound");
}

#[test]
fn reopening_a_crashed_file_never_reports_busy_or_corrupt() {
    // The directive asks for zero `SQLITE_BUSY` and zero `SQLITE_CORRUPT` explicitly,
    // rather than relying on the reopen succeeding. Asserting the *error codes* means a
    // failure names the condition instead of surfacing as "expect() panicked".
    let crash = Crash::new();
    let doc_id = {
        let store = crash.open();
        let doc = store.create_document("Codes").expect("document");
        for i in 0..50 {
            let id = store.add_section(&doc.id, &section_json(3, 30, &format!("c{i}"))).expect("add");
            commit(&store, &doc.id, &id, &section_json(4, 30, &format!("d{i}")));
        }
        crash_without_closing(store);
        doc.id
    };

    let reopened = match Store::open(&crash.path) {
        Ok(s) => s,
        Err(e) => panic!(
            "reopening a crashed file failed: {e}\n\
             the message must name the condition — SQLITE_BUSY means the writer's lock was \
             inherited, SQLITE_CORRUPT means the sidecar did not replay"
        ),
    };
    reopened.recover(&doc_id).expect("recovery must not report a busy or corrupt file");
}

#[test]
fn recovery_is_idempotent_across_repeated_reopens() {
    // A crash *after* a recovery, before the next checkpoint. The second open must replay
    // nothing and change nothing — recovery that doubled a write would be a corruption
    // that only appears when a crash follows a crash, which is exactly when a user is
    // least able to diagnose it.
    let crash = Crash::new();
    {
        let store = crash.open();
        let doc = store.create_document("Twice").expect("document");
        for i in 0..10 {
            let id = store.add_section(&doc.id, &section_json(2, 20, &format!("a{i}"))).expect("add");
            commit(&store, &doc.id, &id, &section_json(3, 20, &format!("b{i}")));
        }
        crash_without_closing(store);
    }

    let first = {
        let store = crash.open();
        let docs = store.documents().expect("documents");
        assert_eq!(store.recover(&docs[0].id).expect("recover"), 10);
        let count = store.manifest(&docs[0].id).expect("manifest").len();
        crash_without_closing(store);
        count
    };

    let second = {
        let store = crash.open();
        let docs = store.documents().expect("documents");
        assert_eq!(
            store.recover(&docs[0].id).expect("recover"),
            0,
            "a second recovery has nothing left to replay"
        );
        let count = store.manifest(&docs[0].id).expect("manifest").len();
        crash_without_closing(store);
        count
    };

    assert_eq!(first, 10);
    assert_eq!(second, 10, "a second open must not add or duplicate sections");
}

#[test]
fn the_last_edit_of_a_section_wins_not_the_first() {
    // Fifty edits to *one* section, which is the shape of real typing. The log keeps every
    // row and the fold applies them in order, so the recovered content must be the
    // thirtieth keystroke, not the first.
    let crash = Crash::new();
    let (doc_id, section_id) = {
        let store = crash.open();
        let doc = store.create_document("Typing").expect("document");
        let id = store.add_section(&doc.id, &section_json(1, 5, "start")).expect("add");

        for i in 0..30 {
            commit(&store, &doc.id, &id, &section_json(1, 5, &format!("v{i} ")));
        }
        assert_eq!(store.wal().row_count(&doc.id).expect("rows"), 30);
        crash_without_closing(store);
        (doc.id, id)
    };

    let store = crash.open();
    store.recover(&doc_id).expect("recover");
    let json = store.load_section(&section_id).expect("load");
    // Spelled out rather than reconstructed: `section_json` builds
    // `{tag}b{block} {words…}`, and hand-writing the expectation wrong once already cost a
    // debugging round on a test that was asserting the right thing about the wrong string.
    let text = json["content"][0]["content"][0]["text"].as_str().unwrap_or_default();
    assert!(
        text.starts_with("v29 b0 "),
        "the newest edit must win; a fold that stopped at the first row would leave 'start'. got {text:?}"
    );
    assert!(
        !text.contains("v0 ") && !text.contains("start"),
        "no earlier edit may survive: {text:?}"
    );
    assert_eq!(store.wal().row_count(&doc_id).expect("rows"), 0);
}

// ---------------------------------------------------------------------------
// The two logs, distinguished
// ---------------------------------------------------------------------------

#[test]
fn the_two_logs_are_independent_and_both_end_empty() {
    // The project keeps a logical `wal` *table* so a keystroke is a small row insert rather
    // than a compressed blob rewrite, and SQLite keeps its own `-wal` sidecar. They are
    // separate files with separate failure modes, and `graceful_shutdown` folds the first
    // and checkpoints the second — in that order, which is why it is a function and not a
    // closure.
    let crash = Crash::new();
    let (doc_id, section_id) = {
        let store = crash.open();
        let doc = store.create_document("Both").expect("document");
        let id = store.add_section(&doc.id, &section_json(2, 20, "x")).expect("add");
        for i in 0..50 {
            commit(&store, &doc.id, &id, &section_json(3, 20, &format!("y{i} ")));
        }
        crash_without_closing(store);
        (doc.id, id)
    };

    // Before recovery: the logical table holds the work, the section row holds the create.
    {
        let store = crash.open();
        assert_eq!(store.wal().row_count(&doc_id).expect("rows"), 50, "the logical log is pending");
        let json = store.load_section(&section_id).expect("load");
        assert_eq!(
            json["content"].as_array().unwrap().len(),
            2,
            "precondition: the section row still holds the *created* content, not the last edit"
        );
        crash_without_closing(store);
    }

    // Recovery empties the logical log. It does not checkpoint SQLite's — that is the
    // checkpoint's job, and conflating them would mean "recovered" and "compacted" were
    // the same claim.
    {
        let store = crash.open();
        store.recover(&doc_id).expect("recover");
        assert_eq!(store.wal().row_count(&doc_id).expect("rows"), 0, "the logical log drains");
        assert!(
            store.wal_file_bytes().expect("wal bytes") > 0,
            "the -wal sidecar is still there; recovery did not checkpoint it"
        );
        crash_without_closing(store);
    }

    // A clean exit empties both.
    {
        let store = crash.open();
        let report = holonomy_shell_lib::core::graceful_shutdown(&store).expect("shutdown");
        assert!(report.is_clean(), "both logs must end empty: {report:?}");
    }
    assert_eq!(crash.sidecar("-wal"), 0, "the sidecar should be truncated away");
}

// ---------------------------------------------------------------------------
// What a crash must not do
// ---------------------------------------------------------------------------

#[test]
fn a_crashed_document_survives_a_clean_reopen_and_can_be_edited_again() {
    // Recovery is not a dead end. The recovered document has to be writable, or a crash
    // would leave a document the user can read but never save.
    let crash = Crash::new();
    let doc_id = {
        let store = crash.open();
        let doc = store.create_document("Writable").expect("document");
        for i in 0..5 {
            let id = store.add_section(&doc.id, &section_json(2, 20, &format!("p{i}"))).expect("add");
            commit(&store, &doc.id, &id, &section_json(3, 20, &format!("q{i} ")));
        }
        crash_without_closing(store);
        doc.id
    };

    let store = crash.open();
    store.recover(&doc_id).expect("recover");

    // Write into it again, as a user resuming their work would.
    let ids = store.section_ids(&doc_id).expect("ids");
    let target = &ids[2];
    commit(&store, &doc_id, target, &section_json(9, 20, "after-crash "));
    store.flush(&doc_id).expect("flush");

    let json = store.load_section(target).expect("load");
    assert_eq!(
        json["content"].as_array().unwrap().len(),
        9,
        "the post-crash edit must be durable"
    );
    assert_eq!(crash.integrity(), "ok");
}

#[test]
fn assets_referenced_by_a_crashed_edit_are_reclaimed_not_orphaned() {
    // The interaction worth checking: a crash between writing an image and folding the
    // section that referenced it. The asset row is committed immediately; the reference
    // lives in the log. A sweep that ran before recovery would see no reference and delete
    // a figure the user's next open would bring back — a data loss produced entirely by
    // running the right code in the wrong order.
    let crash = Crash::new();
    let doc_id = {
        let store = crash.open();
        let doc = store.create_document("Figures").expect("document");
        let doc_id = doc.id.clone();

        let mut png = vec![0x89u8, b'P', b'N', b'G'];
        png.extend(std::iter::repeat(0x33u8).take(8192));
        let hash = store.put_asset(&png, "image/png").expect("put asset");

        let id = store
            .add_section(&doc.id, &section_json(2, 10, "fig "))
            .expect("add section");
        commit(
            &store,
            &doc.id,
            &id,
            &json!({"type":"doc","content":[
                {"type":"paragraph","content":[{"type":"text","text":"with a figure"}]},
                {"type":"image","attrs":{"src": format!("holo-asset://{hash}")}}
            ]}),
        );
        crash_without_closing(store);
        doc_id
    };

    // Recovery first, then the sweep — which is the order `graceful_shutdown` uses.
    let hash = hash_id();
    let store = crash.open();
    store.recover(&doc_id).expect("recover");
    let report = holonomy_core::asset_gc::sweep_orphaned_assets(&store, &doc_id).expect("sweep");
    assert_eq!(
        report.deleted, 0,
        "the figure is referenced again after recovery, so the sweep must not delete it"
    );
    assert!(
        store.get_asset(&hash).expect("get asset").is_some(),
        "the bytes must still resolve for the recovered document"
    );
}

/// The hash of the image this file writes, computed rather than pasted.
///
/// A literal digest here would be a test that fails for an unrelated reason the day the
/// fixture's bytes change, and would fail *silently* in the meantime if it were wrong in
/// the direction of "the asset is absent" — the sweep would report a deletion and the
/// `get_asset` assertion would pass vacuously.
fn hash_id() -> String {
    let mut png = vec![0x89u8, b'P', b'N', b'G'];
    png.extend(std::iter::repeat(0x33u8).take(8192));
    holonomy_core::sha256_hex(&png)
}