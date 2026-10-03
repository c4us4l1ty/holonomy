//! The `.holo` document file: what the extension means and how a file is classified.
//!
//! # Why a format deserves its own module
//!
//! Because "a `.holo` file is a SQLite database" is a claim with consequences in four
//! directions at once, and it is easy to hold one of them and miss the rest:
//!
//! * **The OS** hands us a path from a double-click, and the bytes behind it are whatever
//!   the user dragged in. A `.holo` that is actually a JPEG must produce a sentence a person
//!   can act on, not `file is not a database`.
//! * **The filesystem** puts the extension on the file. Renaming `notes.db` to `notes.holo`
//!   is trivial and will be done, and it must not produce a store that answers queries
//!   against a schema it does not have.
//! * **Other tools** may open the file first. `PRAGMA user_version` is SQLite's own header
//!   field for "what wrote this", readable by anything, with no Holonomy code involved.
//!   That is what makes the file self-describing rather than self-labelling.
//! * **A future version** of this program will write a different schema. It has to be able
//!   to tell "written by a newer Holonomy" from "not a Holonomy file", which are very
//!   different messages for the person holding it.
//!
//! # Why `user_version` and not the `meta` row
//!
//! `meta.schema_version` already records the version and is already checked on open, so
//! this looks like a second place for one fact. It is not: `PRAGMA user_version` is written
//! into the SQLite file header at byte 60 and can be read with a 100-byte read and no
//! schema at all, which is exactly what a *stranger's* tool needs. The `meta` row can only
//! be read by code that already believes the file is Holonomy's — so it can validate a
//! file, but it cannot introduce one.
//!
//! Both are checked on open and both are written on migrate, and there is a test that they
//! cannot disagree.

use crate::error::{Error, Result};
use rusqlite::Connection;
use std::io::Read;
use std::path::Path;

/// The extension, without the dot.
///
/// # Why it is a constant rather than a literal at each use
///
/// It appears in the Tauri file association, in the native dialog filters, and in the
/// message a user sees when they open the wrong file. Three spellings of one extension is
/// how a rename-by-hand arrives at a file the app claims to support and then cannot open.
pub const FILE_EXTENSION: &str = "holo";

/// The MIME type registered with the OS.
///
/// `vnd.` is the vendor tree, and this is a format no other vendor ships, so claiming a
/// name under somebody else's would be the wrong move.
pub const MIME: &str = "application/vnd.holonomy.document";

/// The first sixteen bytes of every SQLite database, whatever the version.
///
/// SQLite writes this to offset 0 and never varies it. Comparing against it is how a
/// non-SQLite file is identified without asking SQLite — and asking SQLite is the thing to
/// avoid, because its answer is the error message we are trying to replace with one a
/// person can read.
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// What a path turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Nothing there, or a zero-length file.
    ///
    /// A zero-length file is `Absent` rather than `Foreign` because that is what SQLite
    /// itself does with one: it opens it and finds an empty database. Calling it foreign
    /// would mean refusing to open a file SQLite is happy to initialise, which would break
    /// "create the file, then open it" in any order.
    Absent,

    /// Bytes that are not a SQLite database at all.
    Foreign,

    /// A SQLite database with no Holonomy schema in it.
    ///
    /// Distinct from `Foreign` because the two produce different sentences: this one is
    /// "that is a database, but not a document", which points at the wrong file; the other
    /// is "that is not a database at all", which points at the wrong *kind* of file.
    NotHolonomy,

    /// A Holonomy document at the given `user_version`.
    Holonomy(i64),
}

/// Inspect `path` without writing to it.
///
/// Reads at most the file header and, for a SQLite file, one pragma. The point of the
/// byte-level check is that it cannot fail in the way SQLite's own parse does: a JPEG
/// renamed to `.holo` is identified by its first sixteen bytes, with no error path to
/// convert.
///
/// An I/O failure is returned rather than mapped to `Foreign`: "I could not read it" and
/// "it is not mine" are different facts, and the first is usually a permissions problem the
/// user can fix.
pub fn probe(path: &Path) -> Result<FileKind> {
    // Read the header directly. `Connection::open` is deliberately not used here: it would
    // create the file we were asked to classify, and a classifier that creates its own
    // input cannot be asked "was this here already?".
    let mut magic = [0u8; SQLITE_MAGIC.len()];
    match std::fs::File::open(path) {
        Ok(mut f) => match read_full(&mut f, &mut magic) {
            // A short file cannot hold a header, so it is not a database. Treated as
            // `Absent` rather than `Foreign` because a zero-length file is *exactly* what
            // SQLite itself initialises, and refusing one would break the ordinary
            // create-then-open order that `File::create` produces.
            Ok(false) => return Ok(FileKind::Absent),
            Ok(true) if &magic == SQLITE_MAGIC => {}
            Ok(true) => return Ok(FileKind::Foreign),
            Err(e) => return Err(Error::Io(e)),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // "This file is not there" and "the folder it would go in is not there" are
            // different facts. The first means *create it*; the second means the save is
            // going somewhere that does not exist, and letting the caller treat that as a
            // fresh document is how a write is lost without an error.
            //
            // The check is on the parent *directory*, not on the file's own path, because
            // `NotFound` from `File::open` means neither and only the parent tells them
            // apart.
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() && !parent.is_dir() {
                    return Err(Error::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("{} is not a directory", parent.display()),
                    )));
                }
            }
            return Ok(FileKind::Absent);
        }
        Err(e) => return Err(Error::Io(e)),
    }

    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;

    // A read-only open is used so classification cannot dirty a file it is only inspecting.
    // WAL databases are readable this way provided the `-wal` sidecar is present, which it
    // is for every file Holonomy has written.
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;

    if version == 0 && !has_meta_table(&conn)? {
        return Ok(FileKind::NotHolonomy);
    }
    Ok(FileKind::Holonomy(version))
}

/// Fill `buf`, reporting whether it was filled completely.
///
/// `Read::read_exact` reports a short file by returning `UnexpectedEof`, which cannot be
/// told apart from a genuine read failure at the call site without re-reading. Here the
/// distinction does not matter — a file too short to hold a SQLite header is `Foreign`
/// either way — but the loop has to exist because a single `read` is permitted to return
/// fewer bytes than asked for, even from a regular file.
fn read_full<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..])? {
            0 => return Ok(false),
            n => filled += n,
        }
    }
    Ok(true)
}

fn has_meta_table(conn: &Connection) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(Error::from)?
        > 0)
}

/// The `sqlite3` file dialog filter pair, for the native open/save dialogs.
///
/// Held here rather than in the shell so the extension, the MIME type and the dialog's
/// description are one value. The Tauri plugin takes them as three strings, which is a
/// place for `holo` to become `Holo`.
pub fn dialog_filter() -> (String, String) {
    (MIME.to_string(), format!("*.{FILE_EXTENSION}"))
}

/// The name shown beside the filter in a native dialog, which is what makes the
/// entry in a file picker readable rather than showing a bare glob.
pub const DIALOG_FILTER_NAME: &str = "Holonomy document";

/// `Path::extension` for a `.holo` path, as a bool, case-insensitively.
///
/// # Why the case-insensitivity
///
/// macOS and Windows both report extensions in whatever case the file was created with, and
/// a document saved from a system that used `.HOLO` opens identically. Treating it as a
/// different file would make the format's identity depend on the filesystem's mood.
pub fn has_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(FILE_EXTENSION))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[test]
    fn an_absent_path_is_absent_and_a_jpeg_is_foreign() {
        let dir = tempfile::tempdir().unwrap();

        let missing = dir.path().join("nothing.holo");
        assert_eq!(probe(&missing).unwrap(), FileKind::Absent);

        // The first bytes of a real PNG: a signature, not a database header. This is the
        // case a renamed file produces, and the reason the header is read by hand.
        let fake = dir.path().join("photo.holo");
        std::fs::write(&fake, b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR").unwrap();
        assert_eq!(probe(&fake).unwrap(), FileKind::Foreign);
    }

    #[test]
    fn a_zero_length_file_is_absent_not_foreign() {
        // SQLite opens a zero-length file and finds an empty database, so calling it
        // foreign would refuse a file the format is entitled to create.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.holo");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(probe(&path).unwrap(), FileKind::Absent);
    }

    #[test]
    fn a_foreign_sqlite_database_is_not_holonomy() {
        // A different program that also uses SQLite. Same magic bytes, no `meta` table,
        // `user_version` 0. Reported separately from `Foreign` because the sentence a user
        // needs is different: this is the wrong file, not the wrong kind of file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("other.holo");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE invoices (id INTEGER);").unwrap();
        drop(conn);

        assert_eq!(probe(&path).unwrap(), FileKind::NotHolonomy);
    }

    #[test]
    fn a_holonomy_document_carries_its_version_in_the_file_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.holo");
        {
            let store = Store::open(&path).unwrap();
            store.create_document("Versioned").unwrap();
            store.flush_all().unwrap();
        }

        assert_eq!(
            probe(&path).unwrap(),
            FileKind::Holonomy(crate::schema::SCHEMA_VERSION),
        );

        // Readable with a 100-byte read and no Holonomy code at all, which is the whole
        // point of `user_version` over the `meta` row.
        //
        // Offset 60, **four** bytes, big-endian. The width is the part worth writing down:
        // an eight-byte read of the same offset returns `1 << 32`, not `1`, because byte 64
        // is the *incremental vacuum* flag and not the high half of anything. That was
        // this test's first result, and it is the kind of mistake that produces a version
        // check which passes on every file written by the same version and fails on every
        // file that crossed a machine.
        let mut header = [0u8; 100];
        let mut f = std::fs::File::open(&path).unwrap();
        f.read_exact(&mut header).unwrap();
        let be = u32::from_be_bytes(header[60..64].try_into().unwrap());
        assert_eq!(be as i64, crate::schema::SCHEMA_VERSION);

        // And the neighbouring field is *not* the high half of the version.
        //
        // Byte 64 is SQLite's own "incremental vacuum" flag, which is not part of
        // anything Holonomy writes -- and it is now `1`, because `Store::init` opens
        // every new file with `PRAGMA auto_vacuum = INCREMENTAL` so that a sweep which
        // deletes an image can return its pages instead of leaving them on the free list
        // forever. The first version of this assertion demanded a zero here and failed
        // when that pragma landed, which is the assertion being wrong rather than the
        // pragma: the byte was never ours.
        //
        // It is asserted rather than skipped so that a future change to the vacuum mode
        // is a deliberate edit here. This is the byte that made an eight-byte read of
        // the version return `1 << 32`.
        assert_eq!(
            u32::from_be_bytes(header[64..68].try_into().unwrap()),
            1,
            "byte 64 is SQLite's incremental-vacuum flag; the store sets INCREMENTAL, so 1 is expected"
        );
    }

    #[test]
    fn the_header_and_the_meta_row_cannot_disagree() {
        // Two records of one fact, written in one place. A database where they differ is
        // one where a stranger's tool and Holonomy disagree about what the file is.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.holo");
        let store = Store::open(&path).unwrap();
        store.create_document("Agreeing").unwrap();

        let from_header: i64 = store
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        let from_meta: i64 = store
            .conn()
            .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(from_header, from_meta);
    }

    #[test]
    fn the_extension_is_matched_without_regard_to_case() {
        assert!(has_extension(Path::new("/tmp/a.holo")));
        assert!(has_extension(Path::new("/tmp/a.HOLO")));
        assert!(has_extension(Path::new("/tmp/a.Holo")));
        assert!(!has_extension(Path::new("/tmp/a.db")));
        // A file merely *containing* the word is not an extension. `.holo.bak` is a backup.
        assert!(!has_extension(Path::new("/tmp/a.holo.bak")));
        assert!(!has_extension(Path::new("/tmp/holonomy")));
    }
}