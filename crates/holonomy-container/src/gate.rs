//! The Phase 3 gate for the container: create → close → open → read → write → close →
//! open → verify.
//!
//! **These tests are slow.** Every `Wavefunction::create` writes 128 MiB through
//! `O_DIRECT`, and every `Wavefunction::open` runs Argon2id at 128 MiB. The round-trip test
//! alone does one create and three opens. Run them with `--test-threads=2` so they do not
//! fight over memory.
//!
//! Scratch files go next to the build output rather than under `/tmp`, because `/tmp` is
//! tmpfs on this host and tmpfs rejects `O_DIRECT`. See [`scratch_dir`].

use std::path::PathBuf;

use super::*;
use crate::ring::RingError;

/// A per-test scratch directory on a filesystem that supports `O_DIRECT`.
///
/// `std::env::temp_dir()` is `/tmp`, which is tmpfs here, and tmpfs returns `EINVAL` for
/// `O_DIRECT` -- so every container test would fail for a reason that has nothing to do with
/// the code under test. Walking up from the test executable to `target/<profile>` keeps the
/// files on the same btrfs mount as the repo.
fn scratch_dir(tag: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("test exe path");
    // target/<profile>/deps/<test> -> target/<profile>
    let base = exe.ancestors().nth(3).expect("target/<profile> layout");
    let dir = base.join("holonomy-container-tests").join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Deterministic content of a given length, so a round-trip failure says *where* it differs.
fn content(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// The PRD's gate, end to end.
#[test]
fn create_write_read_round_trip() {
    let dir = scratch_dir("round_trip");
    let path = dir.join("doc.wavefunction");
    let original = content(200_000); // spans 4 content chunks

    // Create, then close.
    {
        let wf = Wavefunction::create(
            &path,
            "correct horse",
            "Notes",
            &original,
            TEST_VDF_ITERATIONS,
        )
        .expect("create");
        assert_eq!(wf.content_len(), original.len() as u64);
        assert_eq!(wf.title(), "Notes");
        assert_eq!(wf.len(), CONTAINER_SIZE);
    }

    // Open and read back.
    {
        let mut wf = Wavefunction::open(&path, "correct horse", TEST_VDF_ITERATIONS).expect("open");
        let got = wf.read_content().expect("read");
        assert_eq!(got.len(), original.len());
        assert!(
            got == original,
            "content did not survive create -> open -> read"
        );
    }

    // Modify, write back, close.
    let edited = content(150_000);
    {
        let mut wf =
            Wavefunction::open(&path, "correct horse", TEST_VDF_ITERATIONS).expect("open 2");
        wf.write_content(&edited).expect("write");
        wf.commit().expect("commit");
    }

    // Reopen and verify the edit landed.
    {
        let mut wf =
            Wavefunction::open(&path, "correct horse", TEST_VDF_ITERATIONS).expect("open 3");
        let got = wf.read_content().expect("read 2");
        assert!(got == edited, "the edit did not survive reopen");
        assert_eq!(wf.content_len(), edited.len() as u64);
    }
}

/// FR-4.1 / FR-2.1.1: the file is exactly 128 MiB, whatever the content.
#[test]
fn the_file_is_exactly_128_mib_whatever_the_content() {
    let dir = scratch_dir("size");
    for (i, len) in [0usize, 1, 65_520, 1_000_000].iter().enumerate() {
        let path = dir.join(format!("c{i}.wavefunction"));
        Wavefunction::create(&path, "pw", "t", &content(*len), TEST_VDF_ITERATIONS)
            .expect("create");
        let on_disk = std::fs::metadata(&path).expect("stat").len();
        assert_eq!(
            on_disk, CONTAINER_SIZE,
            "content of {len} bytes changed the file size"
        );
    }
}

/// A wrong passphrase must fail, and must be indistinguishable from damage.
#[test]
fn a_wrong_passphrase_is_rejected() {
    let dir = scratch_dir("wrong_pw");
    let path = dir.join("c.wavefunction");
    Wavefunction::create(
        &path,
        "right passphrase",
        "t",
        b"secret",
        TEST_VDF_ITERATIONS,
    )
    .expect("create");

    let err = Wavefunction::open(&path, "wrong passphrase", TEST_VDF_ITERATIONS)
        .expect_err("wrong passphrase must not open the container");
    assert!(
        matches!(
            err,
            ContainerError::Ring(RingError::Aead(AeadError::AuthenticationFailed))
        ),
        "unexpected error: {err}"
    );

    // A near-miss passphrase too, so it is not just "obviously different".
    assert!(Wavefunction::open(&path, "right passphrasf", TEST_VDF_ITERATIONS).is_err());
    assert!(Wavefunction::open(&path, "right passphras", TEST_VDF_ITERATIONS).is_err());
}

/// Corrupting any single byte of the payload must be detected.
///
/// One bit is flipped inside the first content chunk and the open must fail, which is the
/// property that makes the container safe to store on a failing disk.
#[test]
fn a_flipped_bit_in_the_payload_is_detected() {
    let dir = scratch_dir("bitflip");
    let path = dir.join("c.wavefunction");
    {
        let wf = Wavefunction::create(&path, "pw", "t", &content(10_000), TEST_VDF_ITERATIONS)
            .expect("create");
        let om = wf.omega();
        std::mem::forget(wf); // keep the fd open until after we corrupt the file below
                              // Flip one bit in chunk 1, inside the ciphertext.
        let byte_offset = (om + layout::CHUNK_SLOT + 100) as usize;
        use std::fs::OpenOptions;
        use std::io::{Seek, SeekFrom, Write};
        let mut f = OpenOptions::new().write(true).open(&path).expect("open rw");
        f.seek(SeekFrom::Start(byte_offset as u64)).expect("seek");
        f.write_all(&[0x01]).expect("write");
        f.sync_all().expect("sync");
    }

    // Opening only reads chunk 0, the master frame, so it legitimately succeeds even with a
    // damaged content chunk -- which is the point of authenticating each chunk independently.
    // Reading the content is what touches the damage.
    let mut wf = Wavefunction::open(&path, "pw", TEST_VDF_ITERATIONS).expect("open");
    let err = wf
        .read_content()
        .expect_err("corruption in a content chunk must be detected");
    assert!(
        matches!(
            err,
            ContainerError::Ring(RingError::Aead(AeadError::AuthenticationFailed))
        ),
        "unexpected error: {err}"
    );
}

/// The 3-stage ring must be 192 KiB and nothing more. FR-3.1.
#[test]
fn the_ring_is_192_kib() {
    let dir = scratch_dir("ring");
    let path = dir.join("c.wavefunction");
    let wf = Wavefunction::create(&path, "pw", "t", &content(200_000), TEST_VDF_ITERATIONS)
        .expect("create");
    assert_eq!(wf.ring_resident_bytes(), 196_608);
    assert_eq!(wf.ring_resident_bytes(), 192 * 1024);
}

/// The salt must be at fixed offset 0 and readable without a passphrase, because deriving Ω
/// requires it.
///
/// This is the circularity that PROJECT.md and the PRD's own layout section disagree about:
/// the PRD puts the salt at the start of the *payload*, which is at Ω, and Ω is derived from
/// the passphrase. Fixed offset 0 is the only order the steps can actually run in.
#[test]
fn the_salt_is_at_fixed_offset_zero() {
    let dir = scratch_dir("salt");
    let path = dir.join("c.wavefunction");
    let wf = Wavefunction::create(&path, "pw", "t", b"x", TEST_VDF_ITERATIONS).expect("create");
    drop(wf);

    // Read the first page with plain I/O -- the salt must not need O_DIRECT to be found.
    let data = std::fs::read(&path).expect("read");
    assert_eq!(data.len(), CONTAINER_SIZE as usize);
    let salt = &data[..SALT_LEN];
    assert!(
        salt.iter().any(|&b| b != 0),
        "the salt is all zeroes, which is not 32 CSPRNG bytes"
    );

    // And the rest of page 0 is not zeroes either: it is chaff with the salt laid over the
    // first 32 bytes. A run of zeros there would be exactly the cleartext structure FR-4.1
    // forbids.
    let tail = &data[SALT_LEN..4096];
    assert!(
        tail.iter().filter(|&&b| b == 0).count() < tail.len() / 2,
        "most of page 0 after the salt is zero, so the file has a detectable hole"
    );
}

/// Ω must be where the derivation says, and the region before it must be chaff.
#[test]
fn the_region_before_omega_is_chaff_and_the_payload_is_not() {
    let dir = scratch_dir("omega");
    let path = dir.join("c.wavefunction");
    let wf = Wavefunction::create(&path, "pw", "t", &content(70_000), TEST_VDF_ITERATIONS)
        .expect("create");
    let om = wf.omega();
    assert!(om >= layout::CHUNK_SLOT, "omega {om} overlaps the salt");
    assert_eq!(om % layout::CHUNK_SLOT, 0, "omega must be slot aligned");

    // Just before Ω: pure chaff, byte-for-byte reproducible.
    assert!(
        wf.region_is_chaff(om - 4096, 4096).expect("check"),
        "the region before omega is not the chaff keystream"
    );
    // At Ω: the master frame, which is not chaff.
    assert!(
        !wf.region_is_chaff(om, 4096).expect("check"),
        "the payload looks like chaff, so it is not distinguishable"
    );
}

/// Duress: two passphrases over the same salt resolve different offsets, and neither can read
/// the other's payload.
#[test]
fn a_second_passphrase_resolves_a_different_offset() {
    let dir = scratch_dir("duress");
    let path = dir.join("c.wavefunction");
    let a = Wavefunction::create(&path, "primary", "t", &content(10_000), TEST_VDF_ITERATIONS)
        .expect("create");
    let omega_a = a.omega();
    drop(a);

    // Derive from the same salt with a different passphrase.
    let salt = {
        let data = std::fs::read(&path).expect("read");
        let mut s = [0u8; SALT_LEN];
        s.copy_from_slice(&data[..SALT_LEN]);
        s
    };
    let (root_b, omega_b) = {
        let d = holonomy_crypto::envelope::derive_root("duress", &salt, TEST_VDF_ITERATIONS)
            .expect("derive b");
        let m = d.root.expose_secret().clone();
        let om = layout::omega(m.omega);
        (m, om)
    };

    assert_ne!(
        omega_a, omega_b,
        "duress resolved the same offset as the primary"
    );
    assert_eq!(omega_b % layout::CHUNK_SLOT, 0);

    // Opening with B reads bytes at Ω_B, which are not the primary frame, so it must fail.
    let err = Wavefunction::open(&path, "duress", TEST_VDF_ITERATIONS)
        .expect_err("duress passphrase must not read the primary payload");
    assert!(
        matches!(
            err,
            ContainerError::Ring(RingError::Aead(_)) | ContainerError::Frame(_)
        ),
        "{err}"
    );
    let _ = root_b;
}

/// Growing and shrinking the document must keep `content_len` and `chunk_count` consistent,
/// and the master frame must be rewritten with them.
#[test]
fn the_document_can_grow_and_shrink() {
    let dir = scratch_dir("resize");
    let path = dir.join("c.wavefunction");
    Wavefunction::create(&path, "pw", "t", b"small", TEST_VDF_ITERATIONS).expect("create");

    for len in [0usize, 200_000, 1_500_000, 50, 3] {
        let data = content(len);
        {
            let mut wf = Wavefunction::open(&path, "pw", TEST_VDF_ITERATIONS).expect("open");
            wf.write_content(&data).expect("write");
        }
        let mut wf = Wavefunction::open(&path, "pw", TEST_VDF_ITERATIONS).expect("reopen");
        assert_eq!(
            wf.content_len(),
            len as u64,
            "content_len wrong after writing {len}"
        );
        let got = wf.read_content().expect("read");
        assert!(got == data, "content wrong at length {len}");
    }
}

/// The title round-trips and is re-sealed with it.
#[test]
fn the_title_round_trips() {
    let dir = scratch_dir("title");
    let path = dir.join("c.wavefunction");
    Wavefunction::create(&path, "pw", "First title", b"x", TEST_VDF_ITERATIONS).expect("create");
    {
        let mut wf = Wavefunction::open(&path, "pw", TEST_VDF_ITERATIONS).expect("open");
        assert_eq!(wf.title(), "First title");
        wf.set_title("Second \u{6f22}\u{5b57} title")
            .expect("set title");
    }
    let wf = Wavefunction::open(&path, "pw", TEST_VDF_ITERATIONS).expect("reopen");
    assert_eq!(wf.title(), "Second \u{6f22}\u{5b57} title");
}

/// An empty document is valid and uses exactly one chunk.
#[test]
fn an_empty_document_is_one_chunk() {
    let dir = scratch_dir("empty");
    let path = dir.join("c.wavefunction");
    let wf = Wavefunction::create(&path, "pw", "Empty", b"", TEST_VDF_ITERATIONS).expect("create");
    assert_eq!(wf.content_len(), 0);
    assert_eq!(wf.frame().chunk_count, 1);
    drop(wf);
    let mut wf = Wavefunction::open(&path, "pw", TEST_VDF_ITERATIONS).expect("open");
    assert!(wf.read_content().expect("read").is_empty());
}

/// `Debug` must not print the root material.
#[test]
fn debug_redacts_the_root() {
    let dir = scratch_dir("debug");
    let path = dir.join("c.wavefunction");
    let wf = Wavefunction::create(&path, "pw", "t", b"x", TEST_VDF_ITERATIONS).expect("create");
    let rendered = format!("{wf:?}");
    assert!(rendered.contains("redacted"), "{rendered}");
    assert!(!rendered.contains("k_enc"), "{rendered}");
}

/// Opening a file that is not a container, or is the wrong size, must fail cleanly.
#[test]
fn opening_a_non_container_fails() {
    let dir = scratch_dir("notcontainer");
    let path = dir.join("junk.wavefunction");
    std::fs::write(&path, vec![0xABu8; 8192]).expect("write");
    let err = Wavefunction::open(&path, "pw", TEST_VDF_ITERATIONS).expect_err("must refuse");
    assert!(matches!(err, ContainerError::Io(_)), "{err}");
}

/// Opening a path that does not exist must fail rather than create one.
#[test]
fn opening_a_missing_file_does_not_create_it() {
    let dir = scratch_dir("missing");
    let path = dir.join("absent.wavefunction");
    assert!(Wavefunction::open(&path, "pw", TEST_VDF_ITERATIONS).is_err());
    assert!(!path.exists(), "open created the file");
}

/// The entropy gate: a NIST SP 800-22 subset over the produced 128 MiB container, plus the
/// Shannon floor.
///
/// # This reads the whole file, on purpose
///
/// 128 MiB is loaded into memory here, which looks like a contradiction of the 192 KiB
/// streaming claim. It is not: that claim is about the *product* path, and this is an offline
/// auditor that has to see every byte to say anything about the file as a whole. The
/// streaming property is asserted separately, and honestly, in
/// `tests/resident_memory.rs`. If the auditor went through `Wavefunction::read_raw` instead it
/// would also work, but it would allocate 128 MiB through the container's own API, blurring
/// which code owns the claim.
///
/// The full 128 MiB is required, not a sample. See `sp800_22::ENTROPY_FLOOR`: the plug-in
/// entropy estimator is biased low by `255/(2m·ln2)`, which at 16 MiB exceeds the gap between
/// the floor and the mean, so a 16 MiB sample of a *perfectly valid* container would fail.
#[test]
fn the_container_passes_the_entropy_gate() {
    use crate::sp800_22;

    let dir = scratch_dir("entropy");
    let path = dir.join("c.wavefunction");
    let wf = Wavefunction::create(&path, "pw", "t", &content(200_000), TEST_VDF_ITERATIONS)
        .expect("create");
    let om = wf.omega();
    drop(wf);

    let data = std::fs::read(&path).expect("read whole container");
    assert_eq!(
        data.len() as u64,
        CONTAINER_SIZE,
        "the audit must see the whole file"
    );

    let report = sp800_22::run(&data);
    assert_eq!(report.bits, CONTAINER_SIZE * 8);
    assert!(
        report.all_passed(),
        "a fresh container was rejected as non-random:\n{}",
        report.summary()
    );
    assert!(
        report.entropy >= sp800_22::ENTROPY_FLOOR,
        "Shannon entropy {:.9} is below the gate floor {}:\n{}",
        report.entropy,
        sp800_22::ENTROPY_FLOOR,
        report.summary()
    );

    // The payload itself is ciphertext under XChaCha20-Poly1305, so it has to look like the
    // chaff around it. Asserting the file is random while the payload is distinguishable
    // would be worse than useless -- it would mean the gate measures the 120 MiB of noise and
    // ignores the 400 KiB that matters. So: the payload region must also be high-entropy, and
    // must *not* be equal to the keystream there.
    let payload = &data[om as usize..om as usize + 4 * layout::CHUNK_SLOT as usize];
    let payload_entropy = sp800_22::shannon_entropy(payload);
    assert!(
        payload_entropy >= sp800_22::ENTROPY_FLOOR - 2e-3,
        "the payload region has entropy {payload_entropy:.8}, below the noise floor; a \
         histogram would find where the document lives"
    );
    let wf = Wavefunction::open(&path, "pw", TEST_VDF_ITERATIONS).expect("open");
    assert!(
        !wf.region_is_chaff(om, 4096).expect("compare"),
        "the payload region is identical to the chaff keystream"
    );
}
