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
    // **The scratch directory beside the build output, not `std::env::temp_dir()`.**
    //
    // This used to build the path under `/tmp`, which is tmpfs on this host — and tmpfs does
    // not support `O_DIRECT`, so every one of these tests failed at `open` with `EINVAL` on a
    // filesystem that cannot do direct I/O at all. The failure says nothing about the ring.
    scratch_path(name)
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

/// **A commit that fails must never leave a slot that is dirty *and* no longer holds plaintext.**
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
/// # There are now two places a commit can fail, and the invariant covers both
///
/// `Ring::commit` reads each dirty chunk's predecessor tag from the file before sealing it (see
/// `ring::prev_tag_from_disk` — that is what makes the nonce un-repeatable). So a closed
/// descriptor fails at the *read*, one step before it would have failed at the *write*, and at
/// that point the slot still holds its plaintext, which is the good outcome.
///
/// Asserting `!has_dirty()` unconditionally would therefore be asserting an accident of ordering
/// rather than the safety property. What is asserted instead is the property itself, from the
/// bytes: **if the slot is not dirty then it is not claiming residency either, and if it is dirty
/// then it still holds the exact plaintext that was staged.** The dangerous state -- dirty, still
/// resident, and holding ciphertext -- is the one that has no safe reading, so it is the one the
/// test rules out.
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
    const STAGED: &[u8] = b"plaintext that must never be stored as ciphertext";
    ring.stage_plaintext(1, STAGED).expect("stage");

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
        .expect_err("a closed descriptor cannot be read from or written to");
    assert!(
        matches!(err, holonomy_container::ring::RingError::Io(_)),
        "expected the i/o error itself, got {err:?}"
    );

    // **The invariant, read off the bytes rather than off the flag.** A commit that failed before
    // sealing leaves the plaintext exactly where it was and leaves the slot dirty, which is
    // correct: the retry re-seals plaintext. A commit that failed after sealing must have dropped
    // both flags and wiped the slot. What must never happen is a slot that is dirty *and* has
    // stopped being the plaintext it claims to be.
    let still_plaintext = ring.center_bytes().starts_with(STAGED);
    if still_plaintext {
        assert!(
            ring.has_dirty(),
            "the slot kept its plaintext but was marked clean, so the bytes would never be written"
        );
    } else {
        assert!(
            !ring.has_dirty(),
            "the slot no longer holds its plaintext and is still dirty: the next commit would seal \
             the ciphertext already in it and store it as the chunk's plaintext"
        );
        assert!(
            ring.center_bytes().iter().all(|&b| b == 0),
            "sealed bytes that never reached the disk are left sitting in the slot"
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// **A staged edit is not discarded by a rotation of the ring.**
///
/// # The bug
///
/// The ring has three slots and holds `[N−1, N, N+1]`. Rotating it evicts the slot that has
/// fallen out of the window, and **the eviction cleared both of that slot's flags** — the
/// `dirty` flag, which says "these bytes have changed and must be written", and the `resident`
/// index, which says "these bytes belong to chunk `i`".
///
/// **So an edit that had been staged but not yet written back was silently thrown away.** The
/// bytes were still sitting in the slot, but nothing would ever write them and nothing would
/// ever say they were missing: the user would type, the file would save, and the edit would not
/// be in it. `stage_blank` did the same thing to all three slots at once.
///
/// # The fix
///
/// **A slot with an uncommitted edit keeps both flags and its bytes.** The edit stays owed to
/// the disk; the next [`commit`] writes it; and if the ring wants that slot's memory for a
/// prefetch, it writes the edit out first rather than overwriting it.
///
/// # What is asserted
///
/// The state, not the bytes: after a rotation the ring still has an edit pending, and the slot
/// still holds it. **The end-to-end proof — that the bytes reach the disk — is the commit's
/// job, and this test is about the thing that would have stopped them getting there.**
#[test]
fn a_staged_edit_is_not_discarded_by_a_rotation() {
    let mut ring = Ring::new(4).expect("three page-locked slots");
    const STAGED: &[u8] = b"an edit that has not been written back yet";
    ring.stage_plaintext(1, STAGED).expect("stage");
    assert_eq!(ring.pending_count(), 1, "the staged edit should be pending");

    // **Rotate forward far enough to evict the slot holding the edit.** One rotation evicts the
    // slot *behind* the centre; two evicts the centre's own slot, which is where the staged
    // edit is.
    ring.advance();
    ring.advance();
    assert_eq!(
        ring.pending_count(),
        1,
        "rotating the ring discarded an edit that had not been written back: the user would type, \
         the file would save, and the edit would not be in it"
    );

    // And the same in the other direction.
    ring.retreat();
    ring.retreat();
    assert_eq!(
        ring.pending_count(),
        1,
        "rotating the ring backwards discarded an edit that had not been written back"
    );
}

/// **Staging a second chunk does not throw away the first one's edit.**
///
/// `stage_blank` used to wipe **all three** slots, so staging chunk 2 destroyed a staged edit to
/// chunk 1 that had not been written back. The fix gives the new centre a slot that has nothing
/// pending in it, and leaves the pending one alone.
///
/// **The count is what makes this a test rather than an assertion about the code**: `has_dirty`
/// would still be true after the old behaviour, because the *second* edit is pending — the two
/// behaviours differ only in how many edits are still owed to the disk.
#[test]
fn staging_another_chunk_does_not_discard_a_staged_edit() {
    let mut ring = Ring::new(4).expect("three page-locked slots");
    const FIRST: &[u8] = b"the first staged edit";
    const SECOND: &[u8] = b"the second staged edit";
    ring.stage_plaintext(1, FIRST).expect("stage the first");
    ring.stage_plaintext(2, SECOND).expect("stage the second");
    assert_eq!(
        ring.pending_count(),
        2,
        "staging a second chunk discarded the first chunk's pending edit"
    );
}

/// **A failed commit leaves the edit pending rather than dropping it.** The retry has to be able
/// to write the bytes that are still there.
#[test]
fn a_failed_commit_leaves_the_edit_pending() {
    let path = scratch_path("pending");
    let _ = std::fs::remove_file(&path);
    let open = DirectFile::create_or_open(&path).expect("a container-shaped file");
    let mut ring = Ring::new(4).expect("three page-locked slots");
    const STAGED: &[u8] = b"an edit that has not reached the disk";
    ring.stage_plaintext(1, STAGED).expect("stage");

    // **Close the descriptor behind the handle's back** — the same trick as
    // `a_commit_that_cannot_write_leaves_nothing_staged_for_a_retry`, and for the same reason:
    // reaching an I/O error from a test otherwise needs a full filesystem.
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
        "expected the i/o error itself, got {err:?}"
    );
    assert!(
        ring.has_dirty(),
        "the commit failed before it sealed anything, so the edit must still be pending: a retry \
         has to be able to write the bytes that are still in the slot"
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
