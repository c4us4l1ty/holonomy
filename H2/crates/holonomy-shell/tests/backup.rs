//! Backup: one file, complete, and not silently destructive.
//!
//! # Why a backup needs its own suite
//!
//! Because the failure this guards against is invisible. A copy of a live SQLite database is a
//! valid file that opens, reports a schema version, and is missing the last few writes — the ones
//! in the `-wal` sidecar that the main file has not absorbed. Every check a user would make on it
//! passes. So the tests here assert the *content* of a snapshot taken while the database has
//! uncommitted-to-main work in it, and the content is the only thing that can tell.

use holonomy_core::Store;
use serde_json::json;

fn document(store: &Store, name: &str, sections: usize) -> String {
    let document = store.create_document(name).expect("document");
    for i in 0..sections {
        store
            .add_section(
                &document.id,
                &json!({"type":"doc","content":[
                    {"type":"paragraph","content":[{"type":"text","text": format!("{name} section {i}")}]}
                ]}),
            )
            .expect("section");
    }
    document.id
}

/// A file-backed store in a temporary directory, plus a handle for cleaning it up.
///
/// # Why these tests do not use `open_in_memory`
///
/// Because an in-memory store has no file, so a `std::fs::copy` implementation cannot even
/// attempt the backup. The first version used one, and the mutation run showed all five tests
/// failing for the same trivial reason -- `a file-backed store has a path` -- which made four of
/// them unable to say anything about whether the data survived. A file-backed store makes the
/// naive implementation *run*, so the tests can see what it actually loses.
struct Scratch {
    dir: std::path::PathBuf,
    live: std::path::PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = temp_path(name)
            .parent()
            .expect("a parent")
            .join(format!("holonomy-backup-scratch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let live = dir.join("live.db");
        Self { dir, live }
    }

    fn open(&self) -> Store {
        Store::open(&self.live).expect("store")
    }

    /// A path inside the scratch directory that does not exist yet.
    fn fresh(&self, name: &str) -> std::path::PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A temporary path that does not exist yet, and removes itself.
fn temp_path(name: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "holonomy-backup-{name}-{}-{:?}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    path
}

#[test]
fn a_backup_is_one_file_that_opens_and_holds_the_data() {
    let scratch = Scratch::new("basic");
    let store = scratch.open();
    document(&store, "Alpha", 3);
    document(&store, "Beta", 5);
    // A figure, because the assets table is shared and a snapshot without it is not a document.
    let png: Vec<u8> = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    let hash = store.put_asset(&png, "image/png").expect("put_asset");

    let path = scratch.fresh("snapshot.db");
    let summary = store.backup_to(path.to_str().expect("utf-8 path")).expect("backup");

    assert_eq!(summary.documents, 2, "two documents should be in the snapshot");
    assert_eq!(summary.sections, 8, "3 + 5 sections");
    assert_eq!(summary.assets, 1, "the figure should be in the snapshot");
    assert!(summary.bytes > 0, "the snapshot should not be empty");
    assert_eq!(summary.path, path.to_str().expect("utf-8 path"));

    // The real test: open the snapshot and read the data back. A file that exists and is
    // non-empty proves nothing -- a truncated copy would pass that.
    let restored = Store::open(&path).expect("the snapshot should open");
    let alpha = restored
        .documents()
        .expect("documents")
        .into_iter()
        .find(|d| d.title == "Alpha")
        .expect("Alpha should be in the snapshot");
    assert_eq!(
        restored.section_ids(&alpha.id).expect("ids").len(),
        3,
        "Alpha's sections should have survived"
    );
    assert!(
        restored.get_asset(&hash).expect("get_asset").is_some(),
        "the figure should have survived, keyed by its digest"
    );

}

#[test]
fn a_backup_includes_writes_that_had_not_reached_the_main_file() {
    // The reason this exists as a test rather than as a claim in a comment.
    //
    // SQLite's `-wal` holds committed transactions the main file has not absorbed. A `fs::copy`
    // of the main file taken at this point would open cleanly, report the right schema version,
    // and be missing everything written below. Every user-visible check would pass.
    //
    // So: write, and snapshot, and then open the snapshot and look for what was just written. If
    // the implementation were ever changed to a file copy, this fails.
    let dir = temp_path("wal").parent().expect("a parent").to_path_buf();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let live = dir.join("holonomy-live-test.db");
    let _ = std::fs::remove_file(&live);
    let _ = std::fs::remove_file(dir.join("holonomy-live-test.db-wal"));

    let store = Store::open(&live).expect("store");
    let id = document(&store, "Written late", 4);

    // A section written *after* the store was opened, with no checkpoint in between. Whatever
    // the storage layer is doing, this is the write a naive copy would lose.
    store
        .save_section(
            &store.section_ids(&id).expect("ids")[0],
            &json!({"type":"doc","content":[
                {"type":"paragraph","content":[{"type":"text","text":"written after open"}]}
            ]}),
            holonomy_core::SectionMetrics::new(0, 3, 0),
            "",
        )
        .expect("save_section");

    let snapshot = dir.join("holonomy-snapshot-test.db");
    let _ = std::fs::remove_file(&snapshot);
    store
        .backup_to(snapshot.to_str().expect("utf-8 path"))
        .expect("backup");

    let restored = Store::open(&snapshot).expect("snapshot opens");
    let section = restored.section_ids(&id).expect("ids")[0].clone();
    let json = restored.load_section(&section).expect("load the section");
    assert!(
        json.to_string().contains("written after open"),
        "the snapshot should contain the most recent write; it held {json}"
    );

    let _ = std::fs::remove_file(&snapshot);
    let _ = std::fs::remove_file(&live);
    let _ = std::fs::remove_file(dir.join("holonomy-live-test.db-wal"));
}

#[test]
fn a_backup_refuses_to_overwrite() {
    // A backup that quietly replaces the last good copy is a backup with no history. The
    // behaviour is a refusal with a message naming the file, not a silent overwrite and not a
    // numbered rotation -- rotation is a policy, and this is a mechanism.
    let scratch = Scratch::new("overwrite");
    let store = scratch.open();
    document(&store, "Original", 1);

    let path = scratch.fresh("snapshot.db");
    store.backup_to(path.to_str().expect("utf-8 path")).expect("first backup");

    let second = store
        .backup_to(path.to_str().expect("utf-8 path"))
        .expect_err("a second backup to the same path should fail");
    let message = second.to_string();
    assert!(
        message.contains("already exists"),
        "the error should say why; got: {message}"
    );

    // And the first one is untouched, which is the property the refusal exists for.
    let restored = Store::open(&path).expect("the first backup still opens");
    assert_eq!(restored.documents().expect("documents").len(), 1);
}

#[test]
fn a_backup_to_an_unwritable_path_says_so() {
    // Not "succeeds and writes nothing". A backup command that returns success on failure is
    // the specific shape of this bug that a user cannot detect.
    let scratch = Scratch::new("unwritable");
    let store = scratch.open();
    document(&store, "Doomed", 1);

    let err = store
        .backup_to("/nonexistent-directory-for-holonomy/backup.db")
        .expect_err("a backup into a missing directory should fail");
    let message = err.to_string();
    assert!(
        message.to_lowercase().contains("backup"),
        "the error should say what failed; got: {message}"
    );
}

#[test]
fn a_backup_reports_what_it_wrote() {
    // The counts are what a status line shows, and they are the only way a user can tell a
    // 4KB backup of an empty store from a 400MB one.
    let scratch = Scratch::new("counts");
    let store = scratch.open();
    document(&store, "One", 2);
    document(&store, "Two", 2);

    let path = scratch.fresh("snapshot.db");
    let summary = store.backup_to(path.to_str().expect("utf-8 path")).expect("backup");
    assert_eq!(summary.documents, 2);
    assert_eq!(summary.sections, 4);
    assert_eq!(summary.assets, 0, "no figures were added");
    assert!(
        summary.bytes >= 4096,
        "an empty SQLite page is 4096 bytes, so a snapshot with four sections should exceed one \
         page; got {}",
        summary.bytes
    );
}

#[test]
fn a_clean_exit_leaves_one_self_contained_file_that_says_it_is_intact() {
    // The whole teardown claim, end to end, and it is three properties rather than one.
    //
    // This suite exists because "a copy of a live SQLite database is a valid file that opens,
    // reports a schema version, and is missing the last few writes". The shutdown path is the
    // other half of that story: it is what makes the *live* file safe to copy, sync, or put on
    // a USB stick, and each of the three properties below is a way that can be untrue while
    // every other one still looks fine.
    //
    // 1. **No sidecars.** SQLite creates `-wal` and `-shm` beside a WAL database and deletes
    //    them when the last connection closes. A file copied while they exist is a file whose
    //    recent writes are somewhere else — the `-wal` is what a user does not send. The order
    //    matters: the checkpoint must happen *before* the last close, because after the close
    //    there is nothing left to checkpoint through.
    // 2. **The checkpoint actually ran.** `wal_bytes == 0` is the report's own claim, and a
    //    report field is not evidence. The file's size after the fold is.
    // 3. **`integrity_check` says `ok`.** SQLite's own verdict on the file it just wrote, on
    //    every page and index. Nothing else in the suite looks at the bytes at this level, and
    //    it is the one check here that would notice a fold that wrote a page wrong.
    //
    // The document is deliberately larger than a toy: 200 sections, each with pending edits in
    // the logical log at shutdown, so the fold and the checkpoint are doing real work rather
    // than tidying a nearly-empty file.
    let scratch = Scratch::new("teardown");
    let doc_id = {
        let store = scratch.open();
        let doc_id = document(&store, "Long", 200);

        // Pending edits in the logical log, which is the state a real session ends in: the
        // typing path commits and the debounce may not have fired.
        let section = store.manifest(&doc_id).expect("manifest").entries()[0].id.clone();
        holonomy_shell_lib::core::commit_section_edit(
            &store,
            &doc_id,
            &section,
            &json!({"type":"doc","content":[
                {"type":"paragraph","content":[{"type":"text","text":"the last edit"}]},
                {"type":"paragraph","content":[{"type":"text","text":"which must survive"}]}
            ]}),
            1,
        )
        .expect("commit");

        let report = holonomy_shell_lib::core::graceful_shutdown(&store).expect("shutdown");
        assert_eq!(report.documents, 1, "one document had pending rows");
        assert!(report.flushed >= 1, "the pending snapshot should have been folded");
        assert_eq!(report.pending_rows, 0, "the log should be empty after a fold");
        assert_eq!(report.wal_bytes, 0, "the checkpoint should have truncated the journal");
        doc_id
    };

    // Closing the last connection is what makes SQLite delete the sidecars, so this is where
    // they are finally observable. Asserting before the drop would be asserting that SQLite
    // leaks two files per open document, which is not the claim.
    assert!(!scratch.live.with_extension("db-wal").exists(), "a -wal sidecar survived the close");
    assert!(!scratch.live.with_extension("db-shm").exists(), "a -shm sidecar survived the close");

    let bytes = std::fs::metadata(&scratch.live).expect("metadata").len();
    assert!(
        bytes >= 4096,
        "a file of {bytes} bytes cannot hold 200 sections, so the fold wrote nothing"
    );

    // Reopened and asked directly. `Store::open` runs migrations and would report success on
    // a structurally broken file; `integrity_check` is SQLite walking the pages itself.
    let reopened = Store::open(&scratch.live).expect("reopen");
    let verdict: String = reopened
        .conn()
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .expect("integrity_check");
    assert_eq!(verdict, "ok", "the file SQLite just wrote does not pass its own check: {verdict}");
    assert_eq!(
        reopened.recover(&doc_id).expect("recover"),
        0,
        "a clean exit must leave nothing for the recovery path to replay"
    );
}
