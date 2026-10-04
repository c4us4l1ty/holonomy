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

use holonomy_container::Wavefunction;

/// A shared passphrase and a deliberately tiny VDF count: this test is about the ring, not about
/// the delay function, and the container's own constants are the ones that matter.
const PASSPHRASE: &str = "regression";
const VDF_ITERATIONS: u64 = 2;

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

/// A document large enough to occupy all three ring slots, so the "forgotten slot" path is the only
/// one available and the prefetch logic is exercised.
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
