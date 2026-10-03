//! What happens when the wrong file arrives at `Store::open`.
//!
//! # Why this suite exists separately from the unit tests in `holo.rs`
//!
//! Because `probe` classifies and `open` decides, and the decision is the part that can be
//! destructive. `init` migrates anything without a `meta` table; if `open` ever routes a
//! foreign file into `init`, a double-click on `invoices.db` writes eight Holonomy tables
//! into somebody's accounting database. That is the failure worth a suite of its own, and
//! it is only visible at the boundary — every input to `probe` is legal, and it is the
//! pairing of `probe`'s verdict with `open`'s response that matters.

use holonomy_core::{Error, FileKind, Store};

/// Build a SQLite database that is emphatically not ours.
fn foreign_database(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE invoices (id INTEGER PRIMARY KEY, amount REAL);
         INSERT INTO invoices (amount) VALUES (42.0);",
    )
    .unwrap();
}

fn err_of(path: &std::path::Path) -> Error {
    // `Store` holds a live `Connection`, so it is not `Debug` and `expect_err` is
    // unavailable. Matched by hand rather than by `unwrap_err` for that reason.
    match Store::open(path) {
        Ok(_) => panic!("opening a non-document should have failed: {}", path.display()),
        Err(e) => e,
    }
}

#[test]
fn a_foreign_database_is_refused_and_left_exactly_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("invoices.holo");
    foreign_database(&path);

    let before = std::fs::read(&path).unwrap();

    let err = err_of(&path);
    assert!(
        matches!(err, Error::NotADocument { .. }),
        "expected NotADocument, got {err:?}"
    );
    // The load-bearing half. "Refused" and "refused *without writing*" are different
    // properties, and only the second one is any use to the person whose file it is.
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "the refused file was modified on disk"
    );

    // Specifically: no Holonomy table appeared in it.
    let conn = rusqlite::Connection::open(&path).unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(tables, vec!["invoices"], "Holonomy tables were written into it");
}

#[test]
fn a_non_database_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    // A PNG. The realistic version of this is a file that was renamed by hand or by an
    // export that guessed the extension.
    let path = dir.path().join("screenshot.holo");
    std::fs::write(&path, b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR....").unwrap();
    let before = std::fs::read(&path).unwrap();

    assert!(matches!(err_of(&path), Error::NotADocument { .. }));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn a_valid_header_on_a_broken_body_is_refused_rather_than_opened() {
    // The two halves of the guard, and the case that only fails when one of them is missing.
    //
    // `probe` checks the 16-byte SQLite magic and then opens the file read-only to read
    // `PRAGMA user_version`. A file can pass the first and fail the second, and the realistic
    // way to produce one is a machine losing power mid-write: the header made it to disk and
    // the pages did not. SQLite is lazy enough that opening such a file succeeds -- the failure
    // arrives at the pragma.
    //
    // The three outcomes that would each be a different bug:
    //
    // - **Opened as a Holonomy document**, because the magic matched and the code assumed the
    //   rest. This is the one that matters: the next step is a migration, and a migration on a
    //   corrupt file *writes*. The user comes back to a damaged file that is now damaged
    //   differently.
    // - **`Absent`**, which would silently overwrite the user's file with a new empty
    //   document. Losing a document is the worst thing this program can do, and it would do it
    //   quietly.
    // - **A panic**, from an unwrapped SQLite error.
    //
    // What it must be is a refusal, and a refusal that leaves the bytes alone.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("interrupted.holo");

    let mut bytes = b"SQLite format 3\0".to_vec();
    // Enough header for SQLite to read a page size and to try; not a database.
    bytes.extend_from_slice(&[0u8; 200]);
    std::fs::write(&path, &bytes).unwrap();
    let before = std::fs::read(&path).unwrap();

    let err = err_of(&path);
    assert!(
        matches!(err, Error::NotADocument { .. } | Error::Io(_)),
        "a file with a valid header and no pages behind it must be refused, not opened, and \
         not reported as absent; got {err:?}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "a file that could not be classified was modified on disk. Classification is a read: \
         nothing here is allowed to write, whatever it decides"
    );
}

#[test]
fn a_holonomy_file_named_with_the_wrong_extension_is_still_a_document() {
    // Deliberate asymmetry. The *content* decides what a file is; the extension only
    // decides what the OS offers to hand over. Refusing `report.db` because it is not
    // called `.holo` would refuse a file the app wrote itself — `Store::open` is given a
    // temp path in dozens of tests, and none of them care about the name.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("untitled.db");
    {
        let store = Store::open(&path).unwrap();
        store.create_document("Whatever the name says").unwrap();
        store.flush_all().unwrap();
    }
    let store = Store::open(&path).expect("content decides, not the extension");
    assert_eq!(store.documents().unwrap().len(), 1);
}

#[test]
fn a_future_version_names_both_numbers() {
    // The message a user with a too-new file needs is not "unsupported". It is "this is
    // version N, this is version M" — because the actionable response is different: a
    // newer file needs a newer Holonomy, while a foreign file needs a different file.
    //
    // The future version is derived from `SCHEMA_VERSION` rather than written down, which
    // is what this test used to do and why it started failing the moment the constant
    // moved. It forged `PRAGMA user_version = 2` to mean "the future" back when the
    // current version was 1; raising the schema to 2 made that number mean "this build",
    // and the file then opened successfully. A test whose fixture silently changes
    // meaning when a constant moves is not testing the version at all -- it is testing
    // the constant.
    let future = holonomy_core::schema::SCHEMA_VERSION + 1;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("from-the-future.holo");
    {
        let store = Store::open(&path).unwrap();
        store.create_document("Future").unwrap();
    }
    // Forged with raw SQL rather than by bumping the constant: the point is to hold the
    // *reading* side, and a test that bumps `SCHEMA_VERSION` moves both sides at once.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(&format!("PRAGMA user_version = {future};")).unwrap();
    drop(conn);

    match err_of(&path) {
        Error::SchemaVersion { found, expected } => {
            assert_eq!(found, future);
            assert_eq!(expected, holonomy_core::schema::SCHEMA_VERSION);
            // And the rendered sentence must contain both, since the rendered sentence is
            // what the user sees.
            let msg = Error::SchemaVersion { found, expected }.to_string();
            assert!(
                msg.contains(&format!("found {future}")),
                "unhelpful message: {msg}"
            );
        }
        other => panic!("expected a version mismatch, got {other:?}"),
    }
}

#[test]
fn a_missing_parent_directory_is_an_io_error_not_a_silent_create() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nope").join("deeper").join("doc.holo");

    // Not `Absent`: the *file* is absent, but nothing here can create it. Reporting this
    // as a fresh document would mean a save that silently goes nowhere.
    let err = err_of(&path);
    assert!(
        matches!(err, Error::Io(_)),
        "expected an io error for a missing parent, got {err:?}"
    );
}

#[test]
fn probe_agrees_with_open_on_every_kind_of_file() {
    // The two functions are one decision split across a boundary, and the boundary is where
    // they would drift: `open` refusing something `probe` called a document (or the
    // reverse) would show up as a user who can save a file and cannot reopen it.
    let dir = tempfile::tempdir().unwrap();

    let absent = dir.path().join("absent.holo");
    assert_eq!(holonomy_core::holo::probe(&absent).unwrap(), FileKind::Absent);

    let zero = dir.path().join("zero.holo");
    std::fs::write(&zero, b"").unwrap();
    assert_eq!(holonomy_core::holo::probe(&zero).unwrap(), FileKind::Absent);

    let foreign = dir.path().join("foreign.holo");
    foreign_database(&foreign);
    assert_eq!(
        holonomy_core::holo::probe(&foreign).unwrap(),
        FileKind::NotHolonomy
    );
    assert!(Store::open(&foreign).is_err());

    let ours = dir.path().join("ours.holo");
    {
        let store = Store::open(&ours).unwrap();
        store.create_document("Ours").unwrap();
    }
    assert_eq!(
        holonomy_core::holo::probe(&ours).unwrap(),
        FileKind::Holonomy(holonomy_core::schema::SCHEMA_VERSION)
    );
    assert!(Store::open(&ours).is_ok());
}

#[test]
fn a_backup_is_itself_a_document() {
    // `backup_to` writes with `VACUUM INTO`, which rebuilds the file from scratch. If that
    // dropped `user_version`, every backup would be a file Holonomy then refused to open —
    // the single most embarrassing possible failure of a backup feature, and one that a
    // round-trip test through `open` catches and a bytes-on-disk test does not.
    //
    // This passed the first time, which is worth stating: `VACUUM INTO` preserves the
    // header. It was measured rather than assumed because the SQLite documentation does not
    // promise it, and a future SQLite is entitled not to.
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("live.holo");
    let copy = dir.path().join("backup.holo");

    {
        let store = Store::open(&live).unwrap();
        store.create_document("Worth backing up").unwrap();
        store.flush_all().unwrap();
        store.backup_to(&copy.to_string_lossy()).unwrap();
    }

    assert_eq!(
        holonomy_core::holo::probe(&copy).unwrap(),
        FileKind::Holonomy(holonomy_core::schema::SCHEMA_VERSION),
        "the backup is not recognised as a document"
    );
    let reopened = Store::open(&copy).expect("a backup must be openable as a document");
    assert_eq!(reopened.documents().unwrap().len(), 1);
}