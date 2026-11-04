//! The store, tested against a real file.
//!
//! These are the first tests in the project that write a container to a path, close it, open it
//! again and read the document back. Everything above this line could pass while `holonomy save`
//! did not exist.
//!
//! The stand-in pad is exercised honestly here: `pad_window_for_save` derives sector material from
//! the permutation rather than reading a stick, and the tests below assert the two properties that
//! make that a *stand-in* rather than a fake. First, the window is a function of `(salt, cursor)`
//! only, so two saves at one cursor fold identically — which is what makes the second save's read
//! legible. Second, it changes with the salt, which is what makes a save genuinely re-keyed.

use super::*;
use holonomy_core::layout::{slot_range, SALT_BYTES, SALT_SLOT_INDEX};
use holonomy_core::unlock::{SealedPassphrase, Topology};
use holonomy_crypto::ParamsClass;

const KAPPA: [u8; 32] = [0x4Du8; 32];

/// The pad window under the test's fixed `κ_USB`.
fn pad(salt: [u8; SALT_BYTES], cursor: u64) -> Vec<u8> {
    pad_window_for_save(&KAPPA, &salt, cursor)
}

/// The sector index under the test's fixed `κ_USB`.
fn sector(salt: [u8; SALT_BYTES], cursor: u64, j: usize) -> u32 {
    sector_index(&KAPPA, &salt, cursor, j)
}
const PASSPHRASE: &str = "correct horse battery staple";

fn ctx<'a>(pass: &'a SealedPassphrase, cursor: u64) -> SaveContext<'a> {
    SaveContext {
        passphrase: pass,
        kappa_usb: &KAPPA,
        topology: Topology::UmbilicalUsb,
        params: ParamsClass::Test,
        cursor,
    }
}

fn tempfile(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("holonomy-store-{name}-{}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

/// Provision a container, then save over it, with the salt pinned so the test is deterministic.
fn provision(path: &std::path::Path, salt: [u8; SALT_BYTES], cursor: u64) {
    let mut bytes = vec![0u8; CONTAINER_BYTES];
    holonomy_crypto::rng::fill(&mut bytes).expect("CSPRNG");
    std::fs::write(path, &bytes).expect("seed file");

    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let ctx = ctx(&pass, cursor);
    let mut file = ContainerFile::open(path, true).expect("open");
    first_write(&mut file, &ctx, holonomy_session::SaltBytes::of_unknown_origin(salt))
        .expect("first write");
}

#[test]
fn the_fold_does_not_move_the_salt_or_the_capsule() {
    // The ordering constraint, made checkable. The salt is read from slot 1 and the pad window is
    // keyed by it, so the fold must not *move* those bytes anywhere: what a reader finds in slot 1
    // is the salt, folded with the window that same salt selects. If a future change folds in read
    // order rather than in image order, the salt a reader recovers is not the salt that keyed the
    // window and no container opens -- which is exactly the failure this pins down.
    let salt = [0x77u8; SALT_BYTES];
    let image: Vec<u8> = (0..CONTAINER_BYTES).map(|i| (i % 253) as u8).collect();
    let window = pad(salt, 3);

    let mut folded = image.clone();
    fold(&mut folded, &window);
    let (off, _) = slot_range(SALT_SLOT_INDEX);
    for i in 0..SALT_BYTES {
        assert_eq!(folded[off + i], image[off + i] ^ window[off + i], "byte {i} of slot 1");
    }
    // Slot 0 too: the capsule is covered by the pad and lives where the layout says.
    assert_eq!(folded[0], image[0] ^ window[0]);
}

#[test]
fn a_provisioned_container_opens_before_any_save() {
    // The invariant the unpadded-first-write bug violated. A container `provision` wrote must be
    // readable by the same reader a later save uses, because *every* write folds its own pad
    // window -- including the first. If the two write paths differ here, the first `save` after
    // provisioning destroys the capsule it was meant to replace.
    let path = tempfile("provisioned");
    let salt = [0x09u8; SALT_BYTES];
    provision(&path, salt, 0);
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");

    let mut file = ContainerFile::open(&path, false).expect("open");
    let (opened, document) = open_document(&mut file, &ctx(&pass, 0)).expect("a provisioned container must open");
    assert_eq!(document, b"", "provisioning writes an empty document");
    assert_eq!(opened.cursor(), 0);
    drop(file);

    // And its salt is not sitting in the clear: the fold covers slot 1 like everything else, so
    // the bytes on disk are not the salt the container was written under.
    let on_disk = std::fs::read(&path).expect("read");
    let (off, _) = slot_range(SALT_SLOT_INDEX);
    assert_ne!(
        &on_disk[off..off + SALT_BYTES],
        &salt[..],
        "the salt must be covered by the pad, not stored in the clear"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_first_save_after_provisioning_succeeds() {
    // The bug, directly: `save` undoes the old pad before opening the old container, so a
    // provision that wrote an unpadded capsule made the very next save report "wrong passphrase".
    let path = tempfile("firstsave");
    provision(&path, [0x0Au8; SALT_BYTES], 0);
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let ctx = ctx(&pass, 1);

    let mut file = ContainerFile::open(&path, true).expect("open");
    save(&mut file, &ctx, b"the first thing written by a user").expect("the first save must succeed");
    drop(file);

    let mut file = ContainerFile::open(&path, false).expect("reopen");
    let (_, document) = open_document(&mut file, &ctx).expect("open");
    assert_eq!(document, b"the first thing written by a user");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_document_survives_a_save_and_a_reopen() {
    let path = tempfile("roundtrip");
    provision(&path, [0x11u8; SALT_BYTES], 0);

    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let ctx = ctx(&pass, 1);
    let mut file = ContainerFile::open(&path, true).expect("open");

    let document = b"CHAPTER ONE\nIt was a bright cold day in April, and the clocks were striking \
                     thirteen.\n\nCHAPTER TWO\nMr Jones, of the Manor Farm, had locked the hen-houses.";
    let report = save(&mut file, &ctx, document).expect("save");
    assert_eq!(report.plaintext_bytes, document.len());
    assert_eq!(report.slots, 1);
    assert_eq!(report.pad_sectors, PAD_SECTORS);
    assert!(report.ratcheted.is_none(), "a file has no ratchet");

    // A fresh handle, exactly as a second process would open it.
    drop(file);
    let mut file = ContainerFile::open(&path, false).expect("reopen");
    let (opened, read_back) = open_document(&mut file, &ctx).expect("open");
    assert_eq!(read_back, document);
    assert_eq!(opened.cursor(), 1);
    assert_eq!(opened.stratum.id().index(), 1);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_multi_slot_document_reassembles_in_order() {
    let path = tempfile("multislot");
    provision(&path, [0x12u8; SALT_BYTES], 0);
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let ctx = ctx(&pass, 1);

    // Three slots' worth, with a distinct pattern per slot so a reordering cannot pass.
    let per = holonomy_session::seal::PAYLOAD_PLAINTEXT_PER_SLOT;
    let mut document = Vec::new();
    for slot in 0..3u8 {
        document.extend(std::iter::repeat_n(slot.wrapping_add(b'A'), per));
    }
    document.truncate(per * 2 + 100);

    let mut file = ContainerFile::open(&path, true).expect("open");
    let report = save(&mut file, &ctx, &document).expect("save");
    assert_eq!(report.slots, 3);
    drop(file);

    let mut file = ContainerFile::open(&path, false).expect("reopen");
    let (opened, read_back) = open_document(&mut file, &ctx).expect("open");
    assert_eq!(opened.directory.slot_count(), 3);
    assert_eq!(read_back, document);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_wrong_passphrase_fails_before_a_byte_is_written() {
    let path = tempfile("wrongpass");
    provision(&path, [0x13u8; SALT_BYTES], 0);
    let before = std::fs::read(&path).expect("read");

    let wrong = SealedPassphrase::new("the wrong passphrase").expect("legal");
    let mut file = ContainerFile::open(&path, true).expect("open");
    let err = save(&mut file, &ctx(&wrong, 1), b"a document").unwrap_err();
    assert!(err.contains("did not open"), "{err}");
    drop(file);

    // The file is byte-identical: the refusal happened before the write, not after it.
    let after = std::fs::read(&path).expect("read");
    assert_eq!(before, after, "a refused save modified the container");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_read_only_handle_cannot_save() {
    let path = tempfile("readonly");
    provision(&path, [0x14u8; SALT_BYTES], 0);
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let mut file = ContainerFile::open(&path, false).expect("open");
    assert!(!file.is_writable(), "opened read-only, so is_writable must say so");
    let err = save(&mut file, &ctx(&pass, 1), b"x").unwrap_err();
    assert!(err.contains("read-only"), "{err}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_salt_changes_on_every_save_and_with_it_the_pad() {
    // The two properties that make a save a re-keying rather than a rewrite.
    let a = pad([0x21u8; SALT_BYTES], 0);
    let b = pad([0x22u8; SALT_BYTES], 0);
    assert_eq!(a.len(), CONTAINER_BYTES);
    assert_ne!(a, b, "the pad must depend on the salt");
    // Same salt and cursor, same window: this is what lets a reader undo the fold.
    assert_eq!(a, pad([0x21u8; SALT_BYTES], 0));
    // Different cursor, different window.
    let c = pad([0x21u8; SALT_BYTES], 1);
    assert_ne!(a, c, "the pad must depend on the cursor");
}

#[test]
fn consecutive_cursors_read_disjoint_sectors() {
    // The 54-save budget, at the store rather than in the arithmetic module: the sector indices
    // this function chooses for saves 0..54 must be distinct across all of them.
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    for cursor in 0..holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE {
        for j in 0..PAD_SECTORS {
            let sector = sector([0x31u8; SALT_BYTES], cursor, j);
            assert!(seen.insert(sector), "sector {sector} reused at save {cursor}");
        }
    }
    assert_eq!(
        seen.len() as u64,
        holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE * PAD_SECTORS as u64
    );
}

#[test]
fn the_store_and_the_usb_crate_choose_the_same_sectors() {
    // Two computations of one permutation: `PadPlan::for_save` derives the sectors a save will read
    // off a device, and `sector_index` derives the sectors this store folds. If they disagree, a
    // container written here cannot be read by the path that reads a stick, and vice versa.
    //
    // The store keys the permutation with `κ_USB ‖ σ_epoch` mixed through BLAKE2b; `PadPlan` takes
    // that 32-byte key directly. So this test reconstructs the key the same way the store does and
    // checks every sector of four saves. `k_perm_key` below must stay byte-identical to the mixing
    // inside `sector_index` -- two places, one definition, and this is what holds them together.
    let salt = [0x41u8; SALT_BYTES];
    let k_perm = k_perm_key(&KAPPA, &salt);

    for cursor in [0u64, 1, 27, 53] {
        let plan = holonomy_usb::pad::PadPlan::for_save(&k_perm, cursor);
        assert_eq!(plan.len(), PAD_SECTORS);
        for (j, &lba) in plan.lbas().iter().enumerate() {
            assert_eq!(
                sector(salt, cursor, j),
                lba,
                "save {cursor}, sector {j}: the store and the USB plan disagree"
            );
        }
    }
}

/// The permutation key `sector_index` builds, duplicated here deliberately.
///
/// A test that called the production function to build its expected value would not be testing
/// anything. This is the second, independent statement of the mixing, and
/// `the_store_and_the_usb_crate_choose_the_same_sectors` is what fails if the two drift.
fn k_perm_key(kappa_usb: &[u8; 32], salt: &[u8; SALT_BYTES]) -> [u8; 32] {
    use blake2::{Blake2b512, Digest};
    let mut h = Blake2b512::new();
    h.update(b"HOLONOMY_V51_K_PERM");
    h.update([0u8]);
    h.update(kappa_usb);
    h.update(salt);
    let d = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&d[..32]);
    out
}

#[test]
fn the_fold_is_its_own_inverse() {
    let image: Vec<u8> = (0..CONTAINER_BYTES).map(|i| (i % 251) as u8).collect();
    let window = pad([0x51u8; SALT_BYTES], 3);
    let mut folded = image.clone();
    fold(&mut folded, &window);
    assert_ne!(folded, image, "the fold did nothing");
    fold(&mut folded, &window);
    assert_eq!(folded, image, "folding twice must restore the image");
}

#[test]
#[should_panic(expected = "one save of pad is exactly one container")]
fn a_short_pad_is_refused_rather_than_weakening_the_fold() {
    let mut image = vec![0u8; CONTAINER_BYTES];
    let pad = vec![0u8; CONTAINER_BYTES / 2];
    fold(&mut image, &pad);
}

#[test]
fn a_second_save_replaces_the_first() {
    // Save twice, read once: the file holds the second document and nothing of the first. That is
    // what "a container is a fixed-size record" means, and it is why in-place rewrite is safe here
    // and is not for a variable-length file.
    let path = tempfile("twosaves");
    provision(&path, [0x61u8; SALT_BYTES], 0);
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");

    let mut file = ContainerFile::open(&path, true).expect("open");
    save(&mut file, &ctx(&pass, 1), b"the first document").expect("first");
    save(&mut file, &ctx(&pass, 2), b"the second document, longer than the first").expect("second");
    drop(file);

    let mut file = ContainerFile::open(&path, false).expect("reopen");
    let (opened, read_back) = open_document(&mut file, &ctx(&pass, 2)).expect("open at 2");
    assert_eq!(read_back, b"the second document, longer than the first");
    assert_eq!(opened.cursor(), 2);
    drop(file);

    // And the cursor is part of the key schedule: reading at the old cursor fails, because the
    // pad window is wrong. This is the reason the ledger carries the cursor at all.
    let mut file = ContainerFile::open(&path, false).expect("reopen");
    assert!(open_document(&mut file, &ctx(&pass, 1)).is_err());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_truncated_file_is_reported_as_truncated() {
    let path = tempfile("truncated");
    std::fs::write(&path, vec![0u8; 4096]).expect("write");
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let mut file = ContainerFile::open(&path, false).expect("open");
    let err = open_document(&mut file, &ctx(&pass, 0)).unwrap_err();
    assert!(err.contains("a container is"), "{err}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_document_is_split_evenly_and_only_the_last_slot_is_short() {
    let per = holonomy_session::seal::PAYLOAD_PLAINTEXT_PER_SLOT;
    let doc = vec![0xABu8; per + 10];
    let slots = split_payload(&doc);
    assert_eq!(slots.len(), 2);
    assert_eq!(slots[0].len(), per);
    assert_eq!(slots[1].len(), 10);

    // Exactly a slot is one slot, not two with an empty second.
    let exact = vec![0xCDu8; per];
    assert_eq!(split_payload(&exact).len(), 1);

    // An empty document is one empty slot: a zero-slot container has a directory describing
    // nothing, which no reader path expects.
    assert_eq!(split_payload(&[]).len(), 1);
    assert!(split_payload(&[])[0].is_empty());
}
