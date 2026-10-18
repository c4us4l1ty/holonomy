//! Regression: reading after a commit must return plaintext, not the bytes the commit sealed.
//!
//! Found by the Phase 7 census workload, not by this crate's own tests, which is why it lives in its
//! own file rather than being folded into an existing suite.
//!
//! # The bug
//!
//! `Ring::commit` seals each dirty slot **in place** -- deliberately, to avoid a fourth 64 KiB
//! bounce buffer -- and then writes the slot out. The slot's `resident` entry is left saying
//! `Some(index)`, which every other method reads as "this slot holds chunk `index` as *plaintext*".
//!
//! `ensure_center_loaded` short-circuits on that:
//
//! ```text
//! if self.resident[self.center_slot] == Some(self.center) { return Ok(()); }
//! ```
//!
//! so a `seek` to a chunk that happens to still be in the ring returns the *sealed* bytes, and
//! `Wavefunction::read_content` concatenates them into what it reports as the document.
//!
//! # Why the Phase 3 tests missed it
//!
//! They read through a freshly opened `Wavefunction`, whose ring is empty, so the first `seek`
//! always goes to disk. Every one of them does `create -> commit -> drop -> open -> read`.
//!
//! The sequence that breaks is `create -> write_content -> commit -> read_content` on the *same*
//! handle -- which is what the Phase 8 session does on every autosave followed by an export, and
//! what the Phase 7 workload did by accident.
//!
//! # What the fix costs
//!
//! One `pread64` for the chunk the caller is about to read anyway. There is no cheaper option that
//! is also correct: distinguishing "sealed by commit" from "resident plaintext" would need another
//! bit of per-slot state to answer a question the answer to which is always "reload".
//!
//! # And the same fix, one line earlier, for the case where the write never landed
//!
//! `reading_after_a_commit_on_the_same_handle_returns_plaintext` is about a commit that *succeeded*.
//! [`a_commit_that_cannot_write_leaves_nothing_staged_for_a_retry`] is about one that did not, and it
//! is the more dangerous of the two: the flags were cleared *after* the write, so a failed write left
//! a slot marked resident and dirty that held sealed bytes -- and the retry sealed them again and
//! wrote the result as the chunk's plaintext.

use std::os::fd::FromRawFd;

use holonomy_container::io::DirectFile;
use holonomy_container::ring::Ring;
use holonomy_container::Wavefunction;

/// A shared passphrase and a deliberately tiny VDF count: this test is about the ring, not about
/// the delay function, and the container's own constants are the ones that matter.
const PASSPHRASE: &str = "regression";
const VDF_ITERATIONS: u64 = 2;

/// A container file next to the build output rather than in `/tmp`, because `DirectFile` opens with
/// `O_DIRECT` and `/tmp` is tmpfs on this host. `tests/chunk_read.rs` says the same thing about its
/// own scratch directory.
fn scratch_path(name: &str) -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test exe path");
    let dir = exe
        .ancestors()
        .nth(3)
        .expect("target/<profile> layout")
        .join("holonomy-container-tests")
        .join("commit-then-read");
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir.join(format!("{}-{name}.wavefunction", std::process::id()))
}

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "holonomy-ring-commit-{}-{name}.wavefunction",
        std::process::id()
    ))
}

/// The exact sequence that returned ciphertext: commit, then read, on one handle.
#[test]
fn reading_after_a_commit_on_the_same_handle_returns_plaintext() {
    let path = temp_path("same-handle");
    let _ = std::fs::remove_file(&path);

    let mut container =
        Wavefunction::create(&path, PASSPHRASE, "t", b"original", VDF_ITERATIONS).expect("create");
    container.write_content(b"after the edit").expect("edit");
    container.commit().expect("commit");

    let read = container.read_content().expect("read after commit");
    assert_eq!(
        read,
        b"after the edit",
        "read_content() after commit() returned {} bytes of {:?}; expected the plaintext",
        read.len(),
        String::from_utf8_lossy(&read)
    );

    drop(container);
    let _ = std::fs::remove_file(&path);
}

/// Two commits in a row, with a read between and after, since the second commit finds the slots
/// *forgotten* rather than resident and takes a different path.
#[test]
fn two_commits_with_a_read_between_stay_plaintext() {
    let path = temp_path("twice");
    let _ = std::fs::remove_file(&path);

    let mut container =
        Wavefunction::create(&path, PASSPHRASE, "t", b"one", VDF_ITERATIONS).expect("create");
    for (i, text) in [b"two".as_slice(), b"three".as_slice(), b"four".as_slice()]
        .iter()
        .enumerate()
    {
        container.write_content(text).expect("edit");
        container.commit().expect("commit");
        let read = container.read_content().expect("read");
        assert_eq!(
            read,
            *text,
            "round {i}: read after commit returned {:?}",
            String::from_utf8_lossy(&read)
        );
    }

    drop(container);
    let _ = std::fs::remove_file(&path);
}

/// And the round trip still works through a fresh handle, i.e. the fix did not break the write path.
#[test]
fn a_fresh_handle_still_sees_the_committed_plaintext() {
    let path = temp_path("fresh");
    let _ = std::fs::remove_file(&path);

    let mut container =
        Wavefunction::create(&path, PASSPHRASE, "t", b"start", VDF_ITERATIONS).expect("create");
    container.write_content(b"final").expect("edit");
    container.commit().expect("commit");
    drop(container);

    let mut reopened = Wavefunction::open(&path, PASSPHRASE, VDF_ITERATIONS).expect("open");
    assert_eq!(reopened.read_content().expect("read"), b"final");

    let _ = std::fs::remove_file(&path);
}

/// **A commit whose write fails must leave nothing staged for a retry.**
///
/// The fix above clears `resident` and `dirty` when the slot is *sealed*. It used to clear them when
/// the slot was *written*, which is one line too late: on a failed write the slot still claimed to
/// hold chunk `index` as plaintext while holding sealed bytes, and it was still dirty. Two things
/// follow, and the second cannot be undone by anything in the format:
///
/// * `ensure_center_loaded` short-circuits on the resident claim, so the next read hands the
///   ciphertext back as document text.
/// * A retry re-seals that ciphertext and writes the result as the chunk's **plaintext**. A chunk
///   that merely failed to save becomes garbage that decrypts perfectly.
///
/// The write is made to fail by taking the descriptor away from the `DirectFile`: the test takes
/// ownership of it, closes it, and forgets the handle so nothing closes it twice. Reaching an I/O
/// error from a test otherwise needs a full filesystem, and this path is the entire point.
#[test]
fn a_commit_that_cannot_write_leaves_nothing_staged_for_a_retry() {
    let path = scratch_path("unwritable");
    let _ = std::fs::remove_file(&path);
    let open = DirectFile::create_or_open(&path).expect("a container-shaped file");
    let mut ring = Ring::new(4).expect("three page-locked slots");
    ring.stage_plaintext(1, b"plaintext that must never be stored as ciphertext")
        .expect("stage");

    // **Close the descriptor behind the handle's back.** `from_raw_fd` takes ownership of a number
    // this test is giving up, and the value is dropped on this statement, so the close happens here
    // and here only. `ManuallyDrop` is what keeps the `DirectFile`'s own `OwnedFd` from closing the
    // same number again on drop, which would risk closing a descriptor a later test in this binary
    // has since been given.
    //
    // SAFETY: `stolen` is a live descriptor owned by `open`, and exactly one of the two owners
    // closes it.
    let stolen = open.as_raw();
    let open = std::mem::ManuallyDrop::new(open);
    drop(unsafe { std::fs::File::from_raw_fd(stolen) });

    let err = ring
        .commit(&open, 0, &[7u8; 32], &[9u8; 24])
        .expect_err("a closed descriptor cannot be written to");
    assert!(
        matches!(err, holonomy_container::ring::RingError::Io(_)),
        "expected the write's own error, got {err:?}"
    );
    assert!(
        !ring.has_dirty(),
        "the slot is still dirty, so the next commit would seal the ciphertext already in it"
    );
    assert!(
        ring.center_bytes().iter().all(|&b| b == 0),
        "sealed bytes that never reached the disk are left sitting in the slot"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_document_spanning_the_whole_ring_reads_back_after_a_commit() {
    let path = temp_path("whole-ring");
    let _ = std::fs::remove_file(&path);

    // Four chunks: more than the ring holds, so at least one read must go to disk.
    let original: Vec<u8> = (0..4usize)
        .flat_map(|i| {
            let mut chunk = vec![b'a' + i as u8; 60_000];
            chunk[0] = b'0' + i as u8;
            chunk
        })
        .collect();
    let mut container =
        Wavefunction::create(&path, PASSPHRASE, "t", &original, VDF_ITERATIONS).expect("create");
    container
        .write_content(&original)
        .expect("stage the whole document");
    container.commit().expect("commit");

    let read = container.read_content().expect("read");
    assert_eq!(
        read.len(),
        original.len(),
        "expected {} bytes, got {} -- a chunk is missing, which is the prefetch path failing",
        original.len(),
        read.len()
    );
    assert_eq!(
        read, original,
        "the committed document did not read back byte for byte"
    );

    drop(container);
    let _ = std::fs::remove_file(&path);
}
