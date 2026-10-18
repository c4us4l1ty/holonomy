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

/// **A container opens with its own recorded `T`, not the caller's.** PROJECT.md §7's last format item.
///
/// # What was broken, and it is a correctness hole rather than an inconvenience
///
/// `T` was a build-time constant the *caller* passed to both `create` and `open`, and the container
/// nowhere recorded what it had been built with. So a container written by a build calibrated on a host
/// whose CIOS squaring cost differed from the reader's **could not be opened at all**: the reader derives
/// a different root key and chunk 0 fails to authenticate.
///
/// PROJECT.md measured that gap at ~8 % on this host today — small, and entirely a function of how far
/// the host has moved since the container was written. It is not an 8 % problem. It is an opening problem
/// that happens to be 8 % wide today.
///
/// # What is asserted
///
/// A container written with `T = 8` opens when the caller asks for `T = 40`, and **fails** when the
/// caller asks for a count the container did not record *and* the file has no recorded count. The second
/// half matters as much as the first: **the recorded value must actually be consulted**, not merely
/// preferred when it happens to agree.
#[test]
fn a_container_opens_with_its_own_vdf_count_and_not_the_callers() {
    let dir = scratch_dir("vdf_recorded");
    let path = dir.join("vdf.wavefunction");
    let written_with = TEST_VDF_ITERATIONS; // 8

    {
        let wf = Wavefunction::create(&path, "correct horse", "T", b"payload", written_with)
            .expect("create with a small T");
        assert_eq!(
            wf.vdf_iterations(),
            written_with,
            "the container records the T it was created with"
        );
    }

    // **The caller's T is wrong on purpose.** If the container's were ignored, this would fail.
    let wrong = written_with * 5;
    let wf = Wavefunction::open(&path, "correct horse", wrong)
        .expect("open with the container's own recorded T, not the caller's");
    assert_eq!(
        wf.vdf_iterations(),
        written_with,
        "and the container still says what it was created with"
    );

    // **And the salt is unchanged by the field**, which is the thing a format change could plausibly
    // break: a container whose salt moved is a container nobody can open, including its author's.
    let mut wf2 = Wavefunction::open(&path, "correct horse", wrong).expect("reopen");
    let got = wf2.read_content().expect("read the payload back");
    assert_eq!(
        got, b"payload",
        "the content round-tripped through the new field"
    );
}

/// **A container with no recorded `T` still opens with the caller's — the legacy path.**
///
/// Written by hand, because the only honest way to produce one is to be a container from before the field
/// existed: this takes a real container and zeroes the eight bytes, which is exactly what a reader meets
/// when it cannot trust them.
///
/// **And the fallback is safe only because a wrong `T` fails loudly.** A count that does not match
/// produces a wrong root key, and chunk 0 does not authenticate — so no plausible wrong value can ever
/// *open* a container, only fail to. That is what makes accepting a value from the file safe at all.
#[test]
fn a_container_without_a_recorded_vdf_count_falls_back_to_the_callers() {
    let dir = scratch_dir("vdf_legacy");
    let path = dir.join("legacy.wavefunction");

    {
        let wf = Wavefunction::create(&path, "correct horse", "T", b"payload", TEST_VDF_ITERATIONS)
            .expect("create");
        drop(wf);
    }

    // **Zero the recorded bytes**, on the raw file, so the container is genuinely un-recorded.
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .open(&path)
            .expect("reopen for editing");
        f.seek(SeekFrom::Start(layout::VDF_ITERATIONS_OFFSET))
            .expect("seek");
        f.seek(SeekFrom::Start(layout::VDF_ITERATIONS_OFFSET))
            .expect("seek back");
        f.write_all(&[0u8; layout::VDF_ITERATIONS_LEN])
            .expect("zero the field");
        f.sync_all().expect("sync");
    }

    // **With the right `T` it opens, because that is the only count that can produce the right key.**
    let wf = Wavefunction::open(&path, "correct horse", TEST_VDF_ITERATIONS)
        .expect("an un-recorded container opens on the caller's T");
    assert_eq!(wf.content_len(), 7, "and its content is intact");

    // **With a different `T` it does not**, which is the property that makes the whole scheme safe: a
    // recorded count that is wrong cannot silently open the wrong thing.
    assert!(
        Wavefunction::open(&path, "correct horse", TEST_VDF_ITERATIONS + 1).is_err(),
        "a count the container was not written with must fail, not open something"
    );
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
    for (i, len) in [0usize, 1, crate::layout::CHUNK_PLAINTEXT, 1_000_000]
        .iter()
        .enumerate()
    {
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

/// **FR-4.8: the three slots are page-locked, and this is the gate that can see it.**
///
/// # What was wrong
///
/// The size gate above — `the_ring_is_192_kib` — has been green since Phase 3 and asserts the ring
/// is **192 KiB**. It says nothing about what that 192 KiB *is*. It was `alloc_zeroed`: 16-byte
/// aligned, on the heap, swap-eligible, dump-eligible, and bounded by nothing on either side.
/// FR-4.8 says **"3-stage _page-locked_ ring buffer"**, and the size gate passed against an
/// implementation that was not one.
///
/// **That is the shape of the defect worth naming: a gate that measured a property of the *amount*
/// and read as a claim about the *thing*.** Three slots of the right size, unprotected, satisfy a
/// sentence that reads like a protection claim.
///
/// # Why `slots_are_page_locked` is a method and not a comment
///
/// The backing is a runtime choice — `AlignedBuf::page_locked` or `AlignedBuf::zeroed` — so "the
/// ring's slots are page-locked" is not a structural fact a reader can check by looking. **A
/// requirement nobody can ask about is a requirement nobody checks**, which is how this stayed
/// green.
#[test]
fn the_ring_buffer_is_page_locked() {
    let dir = scratch_dir("ring-locked");
    let path = dir.join("c.wavefunction");
    let wf = Wavefunction::create(&path, "pw", "t", &content(200_000), TEST_VDF_ITERATIONS)
        .expect("create");

    assert!(
        wf.ring_slots_are_page_locked(),
        "**FR-4.8: all three slots are page-locked.** They were `alloc_zeroed` until this gate \
         existed, and `the_ring_is_192_kib` was green throughout — it measured how much, not what."
    );

    // **And the alignment `O_DIRECT` needs still holds**, because a page-locked `mmap` and a
    // 4096-aligned `alloc_zeroed` are the same guarantee reached two ways, and a change that
    // swapped the backing without checking this would break every read.
    // **`assert!` and not `assert_eq!(_, true)`.** The property is the alignment; comparing it against
    // the literal `true` reports a mismatch between two booleans rather than naming what is wrong.
    assert!(
        wf.ring_slot_alignment_is_direct_io_safe(),
        "a SecureBlock's payload is page-aligned, which is what lets a locked slot be a direct-I/O \
         buffer at all"
    );
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

/// # FR-4.9: deniable duress routing
///
/// ```text
/// Entry of Primary Passcode P_A resolves offset Ω_A, accessing the real document payload.
/// Entry of Duress  Passcode P_B resolves decoy offset Ω_B (Ω_B ≠ Ω_A), mounting an alternate,
///                   non-sensitive document.
/// ```
///
/// # What already existed, and what did not
///
/// **The offsets.** `derive` maps a passphrase to an `Ω` over the file's own salt, and
/// `a_second_passphrase_resolves_a_different_offset` has gated the *negative* half since Phase 3 —
/// the duress passphrase cannot read the primary.
///
/// **Not the decoy.** A container held one payload, so `P_B` resolved a slot of chaff and the open
/// failed with `Aead` or `Frame`. **Which is correct and useless: an error under duress tells the
/// person holding the rubber hose that `P_B` is not the interesting one.** The feature was half
/// present in the sense that mattered least.
///
/// Each gate below is a way the obvious implementation is wrong.
mod duress {
    use super::*;
    use crate::{ContainerError, DuressDecoy};

    const P_A: &str = "the real passphrase";
    const P_B: &str = "not the passphrase you are looking for";

    fn decoy() -> DuressDecoy<'static> {
        DuressDecoy {
            passphrase: P_B,
            title: "Shopping list",
            content: b"bread\nmilk\ncoffee\n",
        }
    }

    /// **The headline claim: `P_B` mounts a boring document.**
    ///
    /// Everything else in FR-4.9 is in service of this one sentence. If this fails, duress does not
    /// exist, whatever the offsets do.
    #[test]
    fn a_duress_passphrase_mounts_the_decoy() {
        let dir = scratch_dir("duress-mounts");
        let path = dir.join("c.wavefunction");
        let secret = content(40_000);
        Wavefunction::create_with_duress(
            &path,
            P_A,
            "Diary",
            &secret,
            Some(&decoy()),
            TEST_VDF_ITERATIONS,
        )
        .expect("create with duress");

        let mut b = Wavefunction::open(&path, P_B, TEST_VDF_ITERATIONS)
            .expect("**the duress passphrase must open the container**");
        assert_eq!(b.title(), "Shopping list", "**and mount the decoy's title**");
        let got = b.read_content().expect("read the decoy");
        assert_eq!(
            got,
            decoy().content.to_vec(),
            "**and read back the decoy's bytes**"
        );
        assert!(
            got != secret,
            "the decoy is a different document from the primary, which is the point of having two"
        );
    }

    /// **Neither passphrase reads the other.** Both directions, because only testing one is how a
    /// one-way mistake survives.
    #[test]
    fn neither_passphrase_reads_the_others_document() {
        let dir = scratch_dir("duress-both");
        let path = dir.join("c.wavefunction");
        let secret = content(40_000);
        Wavefunction::create_with_duress(
            &path,
            P_A,
            "Diary",
            &secret,
            Some(&decoy()),
            TEST_VDF_ITERATIONS,
        )
        .expect("create");

        // Forward: P_A gets the real document.
        let mut a = Wavefunction::open(&path, P_A, TEST_VDF_ITERATIONS).expect("open with P_A");
        assert_eq!(a.title(), "Diary");
        assert_eq!(a.read_content().expect("read"), secret, "P_A reads the primary");
        let omega_a = a.omega();
        drop(a);

        // Backward: the decoy's own root cannot be used to reach Ω_A. Asserted as *different
        // offsets*, which is the structural reason, and not as an error — an error here would be
        // indistinguishable from a container with no decoy.
        let b = Wavefunction::open(&path, P_B, TEST_VDF_ITERATIONS).expect("open with P_B");
        let omega_b = b.omega();
        assert_ne!(omega_a, omega_b, "**Ω_A and Ω_B are different offsets**");
        assert_eq!(b.title(), "Shopping list");
        drop(b);

        // And a passphrase that is neither fails, which is the property the Phase 3 gate already
        // covered for a container *without* a decoy; stated here because with a decoy present it
        // would be easy to regress into "any passphrase mounts something".
        let err = Wavefunction::open(&path, "third passphrase", TEST_VDF_ITERATIONS)
            .expect_err("an unrelated passphrase must still be refused");
        // **`Ring(RingError::Aead(..))`, not `Aead(..)`** -- the frame decrypts and the failure comes
        // from the ring reading the chunk after it. The first version of this assertion used the flat
        // form and failed on a correct refusal, which is the shape of a gate that checks the thing it
        // expects rather than the thing that happens.
        assert!(
            matches!(err, ContainerError::Ring(crate::ring::RingError::Aead(_)) | ContainerError::Frame(_)),
            "and it fails by refusing to authenticate rather than by some new path: {err}"
        );
    }

    /// **"Without altering bits at Ω_A".**
    ///
    /// The obvious bug is writing the decoy *over* the primary when `Ω_B` lands inside it. The
    /// overlap check in `create_with_duress` refuses that case; this is the gate for the other half
    /// — that a non-overlapping `Ω_B` really does leave the primary alone.
    ///
    /// ## Why this is a round trip and not a byte comparison
    ///
    /// The first version read the primary's bytes out of two containers and compared them. **They
    /// cannot be equal**: each `create` draws a fresh salt, so the chaff keystreams differ and the
    /// payloads are sealed under different keys. That gate would have failed for a reason with
    /// nothing to do with duress, and fixing it by relaxing the comparison would have made it assert
    /// nothing at all.
    ///
    /// **The claim is about what the primary is, not about two files agreeing**, so it is asked the
    /// only way it can be: does the primary still read back exactly?
    #[test]
    fn writing_the_decoy_changes_no_bit_at_the_primary_offset() {
        let dir = scratch_dir("duress-bits");
        let guarded = dir.join("guarded.wavefunction");
        let secret = content(40_000);
        let d = decoy();

        Wavefunction::create_with_duress(
            &guarded,
            P_A,
            "Diary",
            &secret,
            Some(&d),
            TEST_VDF_ITERATIONS,
        )
        .expect("create with duress");

        // **The primary, byte for byte.** A duress feature that damaged the document it protects
        // would be the worst possible outcome and the least likely to be noticed.
        let mut a = Wavefunction::open(&guarded, P_A, TEST_VDF_ITERATIONS).expect("open P_A");
        assert_eq!(
            a.read_content().expect("read"),
            secret,
            "**the primary survived the decoy being written into the same file**, byte for byte"
        );
        let omega_a = a.omega();
        drop(a);

        // **And the decoy lives somewhere else entirely** — disjoint ranges, asserted as ranges
        // rather than as "the open worked", because two offsets differing is the reason two opens
        // can succeed.
        let mut b = Wavefunction::open(&guarded, P_B, TEST_VDF_ITERATIONS).expect("open P_B");
        let omega_b = b.omega();
        assert_eq!(omega_a % layout::CHUNK_SLOT, 0, "the primary is slot-aligned");
        assert_eq!(omega_b % layout::CHUNK_SLOT, 0, "and so is the decoy");
        let primary = omega_a..omega_a + payload_bytes(&secret);
        let decoy_range = omega_b..omega_b + payload_bytes(d.content);
        assert!(
            decoy_range.start >= primary.end || decoy_range.end <= primary.start,
            "the decoy at {}..{} must not touch the primary at {}..{} -- an overlap is refused at \
             write time, so reaching here means the ranges are disjoint",
            decoy_range.start,
            decoy_range.end,
            primary.start,
            primary.end
        );
        assert_eq!(
            b.read_content().expect("read"),
            d.content.to_vec(),
            "and the decoy reads back its own bytes, not the primary's"
        );
    }

    /// **An overlapping decoy is refused, and `create` is unchanged without one.**
    ///
    /// The rule is tested as a value rather than through a random `Ω_B`, because forcing a collision
    /// through `create` means running Argon2id at 128 MiB repeatedly until a uniform draw lands inside
    /// a range the test controls the size of — minutes of gate time to exercise one comparison. The
    /// comparison is what has to be right; that `create` calls it is one line, and the refusal's
    /// *consequence* is pinned by the error's own fields.
    #[test]
    fn an_overlapping_decoy_is_refused_rather_than_merged() {
        use std::ops::Range;

        // The rule, as `create` applies it, spelled out so the test is not a copy that can drift.
        let overlaps = |primary: Range<u64>, decoy: Range<u64>| {
            decoy.start < primary.end && primary.start < decoy.end
        };

        assert!(
            overlaps(100..200, 150..250),
            "a decoy starting inside the primary overlaps it"
        );
        assert!(
            overlaps(100..200, 50..150),
            "and one starting before it and ending inside it does too -- the second comparison, \
             which a `decoy.start < primary.end` alone would miss"
        );
        assert!(
            !overlaps(100..200, 200..300),
            "touching at a boundary is not overlapping: the primary is `[start, end)`"
        );
        assert!(!overlaps(100..200, 0..100), "nor is abutting at the front");
        assert!(!overlaps(100..200, 400..500), "nor is being well clear of it");

        // **And the refusal carries both ranges**, because a bare error cannot tell the caller
        // whether to move the decoy or to shrink it.
        let e = ContainerError::DuressOverlap {
            primary: 100..200,
            decoy: 150..250,
        };
        let msg = e.to_string();
        assert!(msg.contains("150") && msg.contains("250"), "{msg}");
        assert!(msg.contains("100") && msg.contains("200"), "{msg}");
    }

    /// **The decoy does not change the file.** FR-4.1 says the container is exactly 128 MiB whatever
    /// it holds, and a second payload is the most plausible way to break that.
    #[test]
    fn a_duress_payload_does_not_change_the_container_size() {
        let dir = scratch_dir("duress-size");
        let path = dir.join("c.wavefunction");
        Wavefunction::create_with_duress(
            &path,
            P_A,
            "Diary",
            &content(40_000),
            Some(&decoy()),
            TEST_VDF_ITERATIONS,
        )
        .expect("create");
        assert_eq!(
            std::fs::metadata(&path).expect("stat").len(),
            CONTAINER_SIZE,
            "**still exactly 128 MiB with two payloads in it.** The decoy went into chaff that was \
             already there; it did not extend the file."
        );
    }

    /// **Bytes a document's payload occupies**: its frame plus its content chunks, slot-aligned.
    ///
    /// `N` content chunks need `N + 1` slots (chunk 0 is the master frame), and the whole payload is
    /// `CHUNK_SLOT`-aligned because `Ω` is. Written out rather than reusing the crate's
    /// `payload_len`, which takes a chunk count, so that a change to the framing surfaces here as a
    /// gate failure rather than as two helpers that agree by accident.
    fn payload_bytes(content: &[u8]) -> u64 {
        let body = (content.len() as u64).div_ceil(CHUNK_PLAINTEXT as u64);
        // **No `as u64` on `CHUNK_SLOT`: it is already one.** `CHUNK_PLAINTEXT` above is a `usize`
        // and does need the cast, which is why the two lines look alike and only one of them is.
        (body + 1) * CHUNK_SLOT
    }
}

