//! The store, tested against a real file.
//!
//! These are the first tests in the project that write a container to a path, close it, open it
//! again and read the document back. Everything above this line could pass while `holonomy save`
//! did not exist.
//!
//! Every write below goes through `DerivedPad`, the same `PadSource` trait production uses, so these
//! exercise the code path that ships minus the device read. The stand-in is exercised honestly: it
//! derives sector material from the permutation rather than reading a stick, it reports that it
//! cannot ratchet, and the tests below assert what makes that a *stand-in* rather than a fake. The window is a function of `(κ_USB, cursor)` — deliberately not
//! of the salt, which lives inside the pad-covered region and so cannot key the pad that hides it —
//! so it is reproducible for a reader holding the stick and different for anyone who is not.

use super::*;
// The pad types are not re-exported from `store`: production reaches them as `store::pad::X`, and the
// tests should name them the way production does, not through a shortcut the store does not offer.
use super::pad::{DerivedPad, DevicePad, WINDOW_BYTES};
use holonomy_core::layout::{slot_range, SALT_BYTES, SALT_SLOT_INDEX, ZONE_B_SECTORS};
use holonomy_core::unlock::{SealedPassphrase, Topology};
use holonomy_crypto::ParamsClass;

const KAPPA: [u8; 32] = [0x4Du8; 32];

/// A fresh stand-in source. One per operation: a source models a device, and reusing one across two
/// saves would hide a cursor mistake behind state that a real stick would not have.
fn source() -> DerivedPad {
    DerivedPad::new()
}

/// The pad window under the test's fixed `κ_USB`, as bytes.
fn pad(cursor: u64) -> Vec<u8> {
    let mut src = source();
    src.window(&KAPPA, cursor)
        .expect("the stand-in always serves a window")
        .as_slice()
        .to_vec()
}

/// The sector index under the test's fixed `κ_USB`.
fn sector(cursor: u64, j: usize) -> u32 {
    sector_index(&KAPPA, cursor, j)
}

/// A second stick, for the tests that need the pad to depend on it.
const OTHER_KAPPA: [u8; 32] = [0x4Eu8; 32];
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
    first_write(
        &mut file,
        &mut source(),
        &ctx,
        holonomy_session::SaltBytes::of_unknown_origin(salt),
    )
    .expect("first write");
}

#[test]
fn the_fold_is_applied_in_image_order() {
    // The fold must be positional: byte `i` of the image is XORed with byte `i` of the window. A
    // future change that folded in *sector read order* would put each 4 KiB block somewhere else in
    // the image, and then the salt a reader recovers from slot 1 is not the salt the container was
    // written with -- which is the failure mode that already cost this module two rounds.
    let image: Vec<u8> = (0..CONTAINER_BYTES).map(|i| (i % 253) as u8).collect();
    let window = pad(3);

    let mut folded = image.clone();
    fold(&mut folded, &window);
    let (off, _) = slot_range(SALT_SLOT_INDEX);
    for i in 0..SALT_BYTES {
        assert_eq!(
            folded[off + i],
            image[off + i] ^ window[off + i],
            "byte {i} of slot 1"
        );
    }
    // Slot 0 too: the capsule is covered by the pad and lives where the layout says.
    assert_eq!(folded[0], image[0] ^ window[0]);
}

#[test]
fn the_store_round_trips_one_raw_save_without_the_pad() {
    // Isolates the two halves. If this passes, the seal/encode/open path and the derived salt agree
    // and the failure is in the fold. If it fails, the two halves of `first_write` and
    // `open_document` disagree about something else and the fold is not the suspect.
    let path = tempfile("nopad");
    let salt = [0x5Au8; SALT_BYTES];
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let ctx = ctx(&pass, 0);
    let mut bytes = vec![0u8; CONTAINER_BYTES];
    holonomy_crypto::rng::fill(&mut bytes).expect("CSPRNG");
    std::fs::write(&path, &bytes).expect("seed");

    // Write, without folding, by hand.
    let mut file = ContainerFile::open(&path, true).expect("open");
    first_write_unpadded_for_test(&mut file, &ctx, salt).expect("write");
    drop(file);

    // Read, without unfolding, by hand.
    let mut file = ContainerFile::open(&path, false).expect("open");
    let image = file.read_image().expect("read");
    let (salt_off, _) = slot_range(SALT_SLOT_INDEX);
    assert_eq!(
        &image[salt_off..salt_off + SALT_BYTES],
        &salt[..],
        "the salt in slot 1 must be the salt the container was written under"
    );
    let opened = open_container(
        &ReaderRequest {
            image: &image,
            passphrase: &pass,
            kappa_usb: &KAPPA,
            topology: Topology::UmbilicalUsb,
            params: ParamsClass::Test,
            pad_fresh: None,
        },
        // The container was written by `first_write_unpadded_for_test`, which now uses the epoch
        // permutation keyed by the salt it was given, so the reader must use the same one.
        epoch_placement(&salt),
    )
    .expect("the unpadded container must open");
    assert_eq!(opened.cursor(), 0);
    let _ = std::fs::remove_file(&path);
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
    let (opened, document) = open_document(&mut file, &mut source(), &ctx(&pass, 0))
        .expect("a provisioned container must open");
    assert_eq!(
        document.as_slice(),
        b"",
        "provisioning writes an empty document"
    );
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
    save(
        &mut file,
        &mut source(),
        &ctx,
        b"the first thing written by a user",
    )
    .expect("the first save must succeed");
    drop(file);

    let mut file = ContainerFile::open(&path, false).expect("reopen");
    let (_, document) = open_document(&mut file, &mut source(), &ctx).expect("open");
    assert_eq!(document.as_slice(), b"the first thing written by a user");
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
    let report = save(&mut file, &mut source(), &ctx, document).expect("save");
    assert_eq!(report.plaintext_bytes, document.len());
    assert_eq!(report.slots, 1);
    assert_eq!(report.pad_sectors, PAD_SECTORS);
    assert!(report.ratcheted.is_none(), "a file has no ratchet");

    // A fresh handle, exactly as a second process would open it.
    drop(file);
    let mut file = ContainerFile::open(&path, false).expect("reopen");
    let (opened, read_back) = open_document(&mut file, &mut source(), &ctx).expect("open");
    assert_eq!(read_back.as_slice(), document);
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
    let report = save(&mut file, &mut source(), &ctx, &document).expect("save");
    assert_eq!(report.slots, 3);
    drop(file);

    let mut file = ContainerFile::open(&path, false).expect("reopen");
    let (opened, read_back) = open_document(&mut file, &mut source(), &ctx).expect("open");
    assert_eq!(opened.directory.slot_count(), 3);
    assert_eq!(read_back.as_slice(), document);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_wrong_passphrase_fails_before_a_byte_is_written() {
    let path = tempfile("wrongpass");
    provision(&path, [0x13u8; SALT_BYTES], 0);
    let before = std::fs::read(&path).expect("read");

    let wrong = SealedPassphrase::new("the wrong passphrase").expect("legal");
    let mut file = ContainerFile::open(&path, true).expect("open");
    let err = save(&mut file, &mut source(), &ctx(&wrong, 1), b"a document").unwrap_err();
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
    assert!(
        !file.is_writable(),
        "opened read-only, so is_writable must say so"
    );
    let err = save(&mut file, &mut source(), &ctx(&pass, 1), b"x").unwrap_err();
    assert!(err.contains("read-only"), "{err}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_pad_depends_on_the_stick_and_the_cursor_and_neither_alone_is_enough() {
    // Three properties, and the third is the one that forces the design.
    let a = pad(0);
    assert_eq!(a.len(), CONTAINER_BYTES);

    // 1. Deterministic: the same stick and cursor give the same window, which is what lets a reader
    //    undo what a writer folded.
    assert_eq!(a, pad(0));

    // 2. Different cursor, different window: save-to-save disjointness.
    assert_ne!(a, pad(1), "the pad must depend on the cursor");

    // 3. Different stick, different window: the pad is a property of the stick, so a stolen file
    //    yields nothing. This is *not* decorative -- it is the whole reason the key is `κ_USB`.
    let mut other_src = source();
    let other = other_src
        .window(&OTHER_KAPPA, 0)
        .expect("window")
        .as_slice()
        .to_vec();
    assert_ne!(a, other, "the pad must depend on the USB share");

    // And the salt is deliberately absent from the key: it lives inside the pad-covered region, so
    // a window that needed it could never be computed by a reader. `PadSource::window` has no
    // salt parameter at all -- the signature is the assertion, and this comment says why.
}

#[test]
fn a_container_written_under_one_stick_does_not_open_under_another() {
    // The consequence of the pad being keyed by `κ_USB`, tested rather than argued.
    let path = tempfile("otherstick");
    provision(&path, [0x0Bu8; SALT_BYTES], 0);
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");

    let wrong = SaveContext {
        passphrase: &pass,
        kappa_usb: &OTHER_KAPPA,
        topology: Topology::UmbilicalUsb,
        params: ParamsClass::Test,
        cursor: 0,
    };
    let mut file = ContainerFile::open(&path, false).expect("open");
    assert!(
        open_document(&mut file, &mut source(), &wrong).is_err(),
        "a container must not open under a different stick"
    );
    drop(file);

    // The right stick still opens it, so the refusal above is not just "everything fails".
    let ctx = ctx(&pass, 0);
    let mut file = ContainerFile::open(&path, false).expect("open");
    open_document(&mut file, &mut source(), &ctx)
        .expect("the stick it was written with must open it");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn consecutive_cursors_read_disjoint_sectors() {
    // The 54-save budget, at the store rather than in the arithmetic module: the sector indices
    // this function chooses for saves 0..54 must be distinct across all of them.
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    for cursor in 0..holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE {
        for j in 0..PAD_SECTORS {
            let sector = sector(cursor, j);
            assert!(
                seen.insert(sector),
                "sector {sector} reused at save {cursor}"
            );
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
    // The store keys the permutation with `BLAKE2b("K_PERM" ‖ κ_USB)`; `PadPlan` takes that 32-byte
    // key directly. This test reconstructs the key independently and checks every sector of four
    // saves, so `k_perm_key` and the mixing inside `sector_index` cannot drift apart.
    let k_perm = k_perm_key(&KAPPA);

    for cursor in [0u64, 1, 27, 53] {
        let plan = holonomy_usb::pad::PadPlan::for_save(&k_perm, cursor)
            .expect("test cursor is within the no-reuse budget");
        assert_eq!(plan.len(), PAD_SECTORS);
        for (j, &lba) in plan.lbas().iter().enumerate() {
            assert_eq!(
                sector(cursor, j),
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
fn k_perm_key(kappa_usb: &[u8; 32]) -> [u8; 32] {
    use blake2::{Blake2b512, Digest};
    let mut h = Blake2b512::new();
    h.update(b"HOLONOMY_V51_K_PERM");
    h.update([0u8]);
    h.update(kappa_usb);
    let d = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&d[..32]);
    out
}

#[test]
fn the_fold_does_not_move_the_salt_or_the_capsule() {
    // The invariant the positional fold exists for, and the test the invariant job has been invoking
    // by name since the store got a write path -- a name that did not exist. `cargo test <filter>`
    // exits 0 when nothing matches, so this gate asserted nothing for as long as it was in the
    // workflow. It exists now; `audit.yml` also stops accepting a zero-match filter.
    let path = tempfile("foldpositions");
    provision(&path, [0x77u8; SALT_BYTES], 0);

    let raw = std::fs::read(&path).expect("read");
    assert_eq!(raw.len(), CONTAINER_BYTES);
    let window = pad(0);
    let mut unfolded = raw.clone();
    fold(&mut unfolded, &window);
    assert_ne!(unfolded, raw, "the pad must change the bytes it covers");

    // Slot 1: the salt comes back where the layout says it is, and it was not on disk in the clear.
    let (salt_off, _) = slot_range(SALT_SLOT_INDEX);
    assert_eq!(
        &unfolded[salt_off..salt_off + SALT_BYTES],
        &[0x77u8; SALT_BYTES][..],
        "unfolding must restore the salt to slot 1"
    );
    assert_ne!(
        &raw[salt_off..salt_off + SALT_BYTES],
        &[0x77u8; SALT_BYTES][..],
        "the salt must not be on disk in the clear"
    );

    // Slot 0: the capsule, which must open once unfolded. The opened container reports the cursor it
    // was written with, so a fold that moved it would fail here rather than pass quietly.
    let opened = open_container(
        &ReaderRequest {
            image: &unfolded,
            passphrase: &SealedPassphrase::new(PASSPHRASE).expect("legal"),
            kappa_usb: &KAPPA,
            topology: Topology::UmbilicalUsb,
            params: ParamsClass::Test,
            pad_fresh: None,
        },
        // The permutation is a function of the salt, and slot 1 of the unfolded image is the salt.
        epoch_placement(&{
            let (off, _) = slot_range(SALT_SLOT_INDEX);
            let mut s = [0u8; SALT_BYTES];
            s.copy_from_slice(&unfolded[off..off + SALT_BYTES]);
            s
        }),
    )
    .expect("the unfolded image must open");
    assert_eq!(opened.cursor(), 0);

    // And the fold covers the whole container, not only the slots that happened to be written.
    let tail = CONTAINER_BYTES - SECTOR_BYTES;
    assert_eq!(
        unfolded[tail],
        raw[tail] ^ window[tail],
        "the last sector must be covered by the pad as well"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_stand_in_refuses_to_pretend_it_ratcheted() {
    // `save` reports `ratcheted: Some(n)` when a source ratcheted, and that report is how an operator
    // learns whether the pad is fresh. A stand-in returning `Ok(0)` -- or a made-up count -- would
    // make a demo look like a deployment. It must refuse, in words that name the limitation.
    let mut derived = source();
    assert!(!derived.can_ratchet());
    let err = derived.ratchet(&KAPPA, 0).unwrap_err();
    assert!(err.contains("cannot ratchet"), "{err}");
    assert!(err.contains("demo"), "{err}");
    assert!(
        derived.describe().contains("stand-in"),
        "{}",
        derived.describe()
    );
}

#[test]
fn a_save_reports_that_the_stand_in_did_not_ratchet() {
    let path = tempfile("ratchet-report");
    provision(&path, [0x71u8; SALT_BYTES], 0);
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let mut file = ContainerFile::open(&path, true).expect("open");
    let report = save(&mut file, &mut source(), &ctx(&pass, 1), b"x").expect("save");
    assert_eq!(
        report.ratcheted, None,
        "the stand-in cannot ratchet, so the report must not claim it did"
    );
    assert_eq!(report.pad_sectors, PAD_SECTORS);
    drop(file);
    let _ = std::fs::remove_file(&path);
}

/// The indices of a window worth asserting on: the first, one near the middle, the last.
///
/// Not a time-saving micro-optimisation chosen in the abstract — a device read is 32,768 real
/// pread(2) calls whatever is asserted, and it took 465 s for the suite to finish when the two
/// device tests each read a window twice. Three positions catch start, middle and end of a
/// reordering bug, which is what these tests exist to catch.
const PROBED: [usize; 3] = [0, 12_345, PAD_SECTORS - 1];

/// A sparse file standing in for Zone B, with the probed sectors stamped with their own index.
///
/// A window is 32,768 sectors scattered across Zone B's whole codomain, so a dense file would be
/// 7.3 GiB of mostly zeros. Sparse is instant, and unstamped sectors read as zeros the way an
/// unwritten stick does. Only the four positions the assertions look at are stamped — which is still
/// a real test, because a read that went to the wrong LBA would find zeros there and fail the
/// comparison, not pass it.
fn synthetic_zone_b(name: &str, cursor: u64, kappa: &[u8; 32]) -> std::path::PathBuf {
    use std::io::{Seek, SeekFrom, Write};
    let path = tempfile(name);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .expect("create");
    // Every LBA the plan can reach is inside Zone B, so this length is what a read needs.
    file.set_len(ZONE_B_SECTORS as u64 * SECTOR_BYTES as u64)
        .expect("set length");
    for j in PROBED {
        let lba = sector_index(kappa, cursor, j);
        let mut stamp = [0u8; SECTOR_BYTES];
        stamp[..8].copy_from_slice(&(lba as u64).to_le_bytes());
        stamp[8..].fill((lba % 251) as u8);
        file.seek(SeekFrom::Start(lba as u64 * SECTOR_BYTES as u64))
            .expect("seek");
        file.write_all(&stamp).expect("stamp");
    }
    file.sync_all().expect("sync");
    path
}

#[test]
fn the_device_pad_reads_zone_b_in_plan_order() {
    // `DevicePad` is the production path and no CI machine has a stick. This points it at a sparse
    // file laid out as Zone B, which exercises everything the device path does: build a `PadPlan`,
    // read sectors at the plan's offsets through `DirectFile`, and assemble them in plan order. A
    // fold is positional, so plan order is the property that matters and a byte count cannot show it.
    let cursor = 7u64;
    let path = synthetic_zone_b("devicepad-read", cursor, &OTHER_KAPPA);
    let mut device = DevicePad::open_relocated(&path, &OTHER_KAPPA, 0).expect("open the stand-in");

    assert!(!device.can_ratchet(), "opened read-only, so it must refuse");
    assert!(
        device.describe().contains("read-only"),
        "{}",
        device.describe()
    );

    let window = device.window(&OTHER_KAPPA, cursor).expect("window");
    assert_eq!(window.byte_len(), WINDOW_BYTES);

    for j in PROBED {
        let at = j * SECTOR_BYTES;
        let stamped = u64::from_le_bytes(window.as_slice()[at..at + 8].try_into().unwrap());
        assert_eq!(
            stamped,
            sector_index(&OTHER_KAPPA, cursor, j) as u64,
            "position {j}: the device read LBA {stamped}"
        );
        assert_eq!(
            window.as_slice()[at + 8],
            (stamped % 251) as u8,
            "position {j}: LBA {stamped} is not the sector the stamp says"
        );
    }

    // A relocated handle must refuse to ratchet rather than write to the wrong offsets.
    let err = device.ratchet(&OTHER_KAPPA, cursor).unwrap_err();
    assert!(err.contains("relocated"), "{err}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_device_pad_and_the_stand_in_select_the_same_sectors() {
    // The two sources must agree on *which* sectors a save uses and differ only in where the bytes
    // come from. The file stamps each probed sector with its own index; the stand-in hashes the
    // index `sector_index` returns, and its window is assembled from the same call. If the two
    // disagreed about the permutation, each would pass its own tests and a container written by one
    // could not be read by the other.
    let cursor = 3u64;
    let path = synthetic_zone_b("devicepad-vs-standin", cursor, &OTHER_KAPPA);
    let mut device = DevicePad::open_relocated(&path, &OTHER_KAPPA, 0).expect("open");
    let from_device = device.window(&OTHER_KAPPA, cursor).expect("window");

    let mut derived = source();
    let from_stand_in = derived.window(&OTHER_KAPPA, cursor).expect("window");

    for j in PROBED {
        let at = j * SECTOR_BYTES;

        // 1. The sector the device *visited* is the one `sector_index` selects -- the same call the
        //    stand-in assembles its window from. This is the claim: one permutation, two sources.
        let visited = u64::from_le_bytes(from_device.as_slice()[at..at + 8].try_into().unwrap());
        assert_eq!(
            visited,
            sector_index(&OTHER_KAPPA, cursor, j) as u64,
            "position {j}: the device read one sector and the stand-in would derive another"
        );

        // 2. The bytes differ, because they come from different places: the file carries the LBA
        //    stamped into it, the stand-in carries a hash of that LBA. If these matched, one of the
        //    two sources would not be doing what it says.
        assert_ne!(
            &from_device.as_slice()[at + 8..at + 16],
            &from_stand_in.as_slice()[at + 8..at + 16],
            "position {j}: the device and the stand-in returned the same bytes, so one of them is \
             not reading from where it claims"
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_epoch_placement_is_a_bijection_of_the_payload_range() {
    // Every property the format depends on, in one place. A placement that failed any of these would
    // either overwrite a slot with another (not injective), write into the capsule or the salt (out of
    // range), or leave a payload block permanently unused (not surjective -- which would make the
    // container's occupancy map a fingerprint of *which* slots a document used).
    use holonomy_core::layout::{PAYLOAD_SLOTS, PAYLOAD_SLOT_BASE, TOTAL_BLOCKS};
    for salt in [
        [0x00u8; SALT_BYTES],
        [0x33u8; SALT_BYTES],
        [0xFFu8; SALT_BYTES],
    ] {
        let place = epoch_placement(&salt);
        let mut seen = vec![false; TOTAL_BLOCKS];
        for logical in 0..PAYLOAD_SLOTS {
            let physical = place(logical);
            assert!(
                (PAYLOAD_SLOT_BASE..TOTAL_BLOCKS).contains(&physical),
                "salt {:02x}: slot {logical} placed at block {physical}, which is not a payload block",
                salt[0]
            );
            assert!(
                !seen[physical],
                "salt {:02x}: block {physical} used twice -- the placement is not injective",
                salt[0]
            );
            seen[physical] = true;
        }
        // Surjective onto exactly the payload range: every payload block is used once.
        assert_eq!(seen.iter().filter(|&&b| b).count(), PAYLOAD_SLOTS);
        assert!(
            !seen[0] && !seen[1],
            "the capsule and the salt must be untouched"
        );
    }
}

#[test]
fn two_salts_permute_the_same_slot_differently() {
    // Without this the permutation is just the identity with extra steps. If two epochs
    // produced the same mapping, a save would leave the layout of the previous one intact and the
    // whole mechanism would buy nothing.
    let a = epoch_placement(&[0x11u8; SALT_BYTES]);
    let b = epoch_placement(&[0x22u8; SALT_BYTES]);
    let slots = holonomy_core::layout::PAYLOAD_SLOTS;
    let differing = (0..slots).filter(|&i| a(i) != b(i)).count();

    // Not "almost every slot differs". A swap chain leaves a given element in place with probability
    // about 1/4 (no transposition touches it), so two independent chains agree on a slot about 1/4 of
    // the time and the expectation here is ~1,535 of 2,046 -- 1,874 in the run that turned this up.
    // An earlier version of this assertion demanded 99%, which is true of a permutation of 2^22
    // elements and not of one drawn from 2,046 swaps. What must hold is that the layouts are
    // substantially unrelated, not that they are disjoint.
    assert!(
        differing > slots / 2,
        "only {differing} of {slots} slots differ between two epochs; the permutation is barely doing anything"
    );
}

#[test]
fn a_saved_payload_is_not_written_in_logical_order() {
    // The observable consequence, on a real container: after unfolding, the ciphertext of slot 0 must
    // *not* be at block 2, because block 2 is where an identity placement would have put it. A test
    // that only checked the placement function in isolation could pass while the store still called
    // the identity.
    use holonomy_core::layout::{PAYLOAD_SLOTS, PAYLOAD_SLOT_BASE};
    let path = tempfile("permuted");
    let salt = [0x5Du8; SALT_BYTES];
    provision(&path, salt, 0);

    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let ctx = ctx(&pass, 1);
    let document = b"a document long enough to occupy exactly one payload slot";
    let mut file = ContainerFile::open(&path, true).expect("open");
    save(&mut file, &mut source(), &ctx, document).expect("save");
    drop(file);

    let raw = std::fs::read(&path).expect("read");
    let mut unfolded = raw;
    fold(&mut unfolded, &pad(1));

    // The salt the save chose is in slot 1, and it is what keys the permutation.
    let (salt_off, _) = slot_range(SALT_SLOT_INDEX);
    let mut new_salt = [0u8; SALT_BYTES];
    new_salt.copy_from_slice(&unfolded[salt_off..salt_off + SALT_BYTES]);
    assert_ne!(
        new_salt, [0u8; SALT_BYTES],
        "the save must choose a fresh salt"
    );
    let place = epoch_placement(&new_salt);

    // Across all slots, not just slot 0: a single slot has a 1-in-2,046 chance of landing where the
    // identity would have put it, so asserting on one would be a test that fails about once per two
    // thousand runs for no reason. A permutation that is *mostly* the identity is not one.
    let moved = (0..PAYLOAD_SLOTS)
        .filter(|&i| place(i) != PAYLOAD_SLOT_BASE + i)
        .count();
    assert!(
        moved > PAYLOAD_SLOTS / 2,
        "only {moved} of {PAYLOAD_SLOTS} slots moved: the save wrote its payload in logical order"
    );

    // The one payload slot this document filled is somewhere the permutation chose, and it is not
    // empty. Which logical index it is does not matter -- the document is one slot -- but the block
    // has to hold something, or the permutation would be pointing at unused space.
    let flat = place(0);
    let (off, len) = slot_range(flat);
    assert!(
        unfolded[off..off + len].iter().any(|&b| b != 0),
        "the block the permutation selected is empty, so nothing was written there"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_fold_is_its_own_inverse() {
    let image: Vec<u8> = (0..CONTAINER_BYTES).map(|i| (i % 251) as u8).collect();
    let window = pad(3);
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
    save(
        &mut file,
        &mut source(),
        &ctx(&pass, 1),
        b"the first document",
    )
    .expect("first");
    save(
        &mut file,
        &mut source(),
        &ctx(&pass, 2),
        b"the second document, longer than the first",
    )
    .expect("second");
    drop(file);

    let mut file = ContainerFile::open(&path, false).expect("reopen");
    let (opened, read_back) =
        open_document(&mut file, &mut source(), &ctx(&pass, 2)).expect("open at 2");
    assert_eq!(
        read_back.as_slice(),
        b"the second document, longer than the first"
    );
    assert_eq!(opened.cursor(), 2);
    drop(file);

    // And the cursor is part of the key schedule: reading at the old cursor fails, because the
    // pad window is wrong. This is the reason the ledger carries the cursor at all.
    let mut file = ContainerFile::open(&path, false).expect("reopen");
    assert!(open_document(&mut file, &mut source(), &ctx(&pass, 1)).is_err());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_truncated_file_is_reported_as_truncated() {
    let path = tempfile("truncated");
    std::fs::write(&path, vec![0u8; 4096]).expect("write");
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let mut file = ContainerFile::open(&path, false).expect("open");
    let err = open_document(&mut file, &mut source(), &ctx(&pass, 0)).unwrap_err();
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

#[test]
fn save_rejects_provisioning_cursor_without_changing_container() {
    let path = tempfile("zero-cursor");
    provision(&path, [0x19u8; SALT_BYTES], 0);
    let before = std::fs::read(&path).expect("read before");
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let mut file = ContainerFile::open(&path, true).expect("open");
    let error = save(&mut file, &mut source(), &ctx(&pass, 0), b"new")
        .expect_err("zero is not a successor cursor");
    assert!(error.contains("at least 1"));
    drop(file);
    assert_eq!(std::fs::read(&path).expect("read after"), before);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn provision_refuses_existing_path_without_overwriting_it() {
    let path = tempfile("existing-provision");
    std::fs::write(&path, b"preserve me").expect("seed");
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let error = super::provision(&path, &mut source(), &ctx(&pass, 0)).unwrap_err();
    assert!(error.contains("existing paths"));
    assert_eq!(std::fs::read(&path).expect("read"), b"preserve me");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn pad_reuse_boundary_refuses_without_touching_container() {
    let limit = holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE;
    assert!(validate_save_cursor(0).is_err());
    assert!(validate_save_cursor(1).is_ok());
    assert!(validate_save_cursor(limit - 1).is_ok());
    assert!(validate_save_cursor(limit).is_err());
    assert!(validate_save_cursor(u64::MAX).is_err());

    let path = tempfile("pad-reuse-boundary");
    std::fs::write(&path, b"unchanged").expect("seed");
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let mut file = ContainerFile::open(&path, true).expect("open");
    let err = save(&mut file, &mut source(), &ctx(&pass, limit), b"new")
        .expect_err("the first overlapping window must be refused");
    assert!(err.contains("pad reuse"), "{err}");
    drop(file);
    assert_eq!(std::fs::read(&path).expect("read"), b"unchanged");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn provision_rejects_nonzero_cursor_without_creating_file() {
    let path = tempfile("provision-nonzero");
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let err = super::provision(&path, &mut source(), &ctx(&pass, 1))
        .expect_err("provisioning must start at zero");
    assert!(err.contains("cursor must be 0"), "{err}");
    assert!(!path.exists());
}

#[test]
fn open_rejects_cursor_alias_even_when_pad_window_matches() {
    // Simulate a reused window: the bytes unfold successfully, but the authenticated
    // directory still names cursor zero rather than the caller's cursor one.
    struct AliasPad(DerivedPad);
    impl PadSource for AliasPad {
        fn window(&mut self, kappa: &[u8; 32], _cursor: u64) -> Result<pad::PadWindow, String> {
            self.0.window(kappa, 0)
        }
        fn ratchet(&mut self, _kappa: &[u8; 32], _cursor: u64) -> Result<u32, String> {
            Err("not supported".into())
        }
        fn can_ratchet(&self) -> bool {
            false
        }
        fn describe(&self) -> String {
            "alias test pad".into()
        }
    }

    let path = tempfile("open-cursor-alias");
    provision(&path, [0x1au8; SALT_BYTES], 0);
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let mut file = ContainerFile::open(&path, false).expect("open");
    let err = open_document(&mut file, &mut AliasPad(source()), &ctx(&pass, 1))
        .expect_err("authenticated cursor must match");
    assert!(err.contains("does not match requested cursor"), "{err}");
    drop(file);
    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[test]
fn staged_replacement_updates_file_and_open_handle() {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let path = tempfile("staged-replacement");
    let original = std::fs::File::create(&path).expect("create");
    original.set_len(CONTAINER_BYTES as u64).expect("size");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("permissions");
    let old_ino = original.metadata().expect("metadata").ino();
    drop(original);
    let mut file = ContainerFile::open(&path, true).expect("open");
    let image = vec![0x5au8; CONTAINER_BYTES];
    let expected = file.folded_digest().expect("original digest");
    file.replace_image(&image, &expected).expect("replace");
    let metadata = std::fs::metadata(&path).expect("new metadata");
    assert_ne!(old_ino, metadata.ino());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
    let mut byte = [0u8; 1];
    file.file.seek(SeekFrom::Start(0)).expect("seek");
    file.file
        .read_exact(&mut byte)
        .expect("read from updated handle");
    assert_eq!(byte, [0x5a]);
    assert_eq!(
        std::fs::metadata(&path).expect("metadata").len(),
        CONTAINER_BYTES as u64
    );
    drop(file);
    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[test]
fn staged_replacement_refuses_changed_path() {
    let path = tempfile("staged-path-change");
    let moved = tempfile("staged-path-original");
    std::fs::write(&path, b"original").expect("original");
    let mut file = ContainerFile::open(&path, true).expect("open");
    std::fs::rename(&path, &moved).expect("move original");
    std::fs::write(&path, b"replacement").expect("replace path");
    let image = vec![0u8; CONTAINER_BYTES];
    let err = file
        .replace_image(&image, &[0; 32])
        .expect_err("stale handle must be refused");
    assert!(err.to_string().contains("same regular file"), "{err}");
    assert_eq!(std::fs::read(&path).expect("new file"), b"replacement");
    assert_eq!(std::fs::read(&moved).expect("old file"), b"original");
    drop(file);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&moved);
}

#[cfg(unix)]
#[test]
fn standalone_container_refuses_extra_bytes_without_reading_pad_or_replacing() {
    struct NeverReadPad;
    impl PadSource for NeverReadPad {
        fn window(&mut self, _kappa: &[u8; 32], _cursor: u64) -> Result<pad::PadWindow, String> {
            panic!("invalid file size must be rejected before reading the pad")
        }
        fn ratchet(&mut self, _kappa: &[u8; 32], _cursor: u64) -> Result<u32, String> {
            panic!("invalid file size must never ratchet")
        }
        fn can_ratchet(&self) -> bool {
            true
        }
        fn describe(&self) -> String {
            "test pad".into()
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("container");
    let mut original = std::fs::File::create(&path).expect("create");
    original.set_len(CONTAINER_BYTES as u64).expect("size");
    original.seek(SeekFrom::Start(0)).expect("seek");
    original.write_all(b"prefix").expect("prefix");
    original.seek(SeekFrom::End(0)).expect("seek");
    original.write_all(b"trailing data").expect("suffix");
    drop(original);
    let mut file = ContainerFile::open(&path, true).expect("open");
    let error = file.read_image().expect_err("oversized file rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("extra data"));
    let pass = SealedPassphrase::new(PASSPHRASE).expect("passphrase");
    let error = save(&mut file, &mut NeverReadPad, &ctx(&pass, 1), b"new data")
        .expect_err("save must not truncate a larger file");
    assert!(error.contains("extra data"), "{error}");
    let image = vec![0u8; CONTAINER_BYTES];
    let error = file
        .replace_image(&image, &[0; 32])
        .expect_err("replacement refused");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    drop(file);
    let mut original = std::fs::File::open(&path).expect("open original");
    assert_eq!(
        original.metadata().expect("metadata").len(),
        CONTAINER_BYTES as u64 + 13
    );
    let mut prefix = [0u8; 6];
    original.read_exact(&mut prefix).expect("prefix");
    assert_eq!(&prefix, b"prefix");
    original
        .seek(SeekFrom::Start(CONTAINER_BYTES as u64))
        .expect("seek");
    let mut suffix = [0u8; 13];
    original.read_exact(&mut suffix).expect("suffix");
    assert_eq!(&suffix, b"trailing data");
}

#[cfg(unix)]
#[test]
fn standalone_container_refuses_growth_before_replacement() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("container");
    let original = std::fs::File::create(&path).expect("create");
    original.set_len(CONTAINER_BYTES as u64).expect("size");
    drop(original);
    let mut container = ContainerFile::open(&path, true).expect("open");
    let mut other = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append");
    other.write_all(b"external data").expect("append data");
    let image = vec![0u8; CONTAINER_BYTES];
    assert_eq!(
        container
            .replace_image(&image, &[0; 32])
            .expect_err("refuse growth")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    assert_eq!(
        std::fs::metadata(&path).expect("metadata").len(),
        CONTAINER_BYTES as u64 + 13
    );
}

#[test]
fn standalone_container_rejects_truncation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("short");
    std::fs::write(&path, b"short").expect("write");
    let mut file = ContainerFile::open(&path, false).expect("open");
    let error = file.read_image().expect_err("short file rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    assert_eq!(std::fs::read(&path).expect("read"), b"short");
}

#[cfg(unix)]
#[test]
fn verify_reports_snapshot_digest_and_rejects_tampered_payload() {
    struct NoRatchet(DerivedPad);
    impl PadSource for NoRatchet {
        fn window(&mut self, kappa: &[u8; 32], cursor: u64) -> Result<pad::PadWindow, String> {
            self.0.window(kappa, cursor)
        }
        fn ratchet(&mut self, _kappa: &[u8; 32], _cursor: u64) -> Result<u32, String> {
            panic!("verification must not ratchet")
        }
        fn can_ratchet(&self) -> bool {
            panic!("verification must not request write capability")
        }
        fn describe(&self) -> String {
            "test pad".into()
        }
    }
    fn flip(path: &Path, offset: usize) {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("open");
        file.seek(SeekFrom::Start(offset as u64)).expect("seek");
        let mut byte = [0u8; 1];
        file.read_exact(&mut byte).expect("read");
        byte[0] ^= 1;
        file.seek(SeekFrom::Start(offset as u64)).expect("rewind");
        file.write_all(&byte).expect("write");
        file.sync_all().expect("sync");
    }

    let path = tempfile("verify-tamper");
    provision(&path, [0x23u8; SALT_BYTES], 0);
    let pass = SealedPassphrase::new(PASSPHRASE).expect("passphrase");
    let mut writer = ContainerFile::open(&path, true).expect("open writer");
    let report = save(&mut writer, &mut source(), &ctx(&pass, 1), b"document").expect("save");
    drop(writer);
    let mut reader = ContainerFile::open(&path, false).expect("open reader");
    let original =
        verify_candidate(&mut reader, &mut NoRatchet(source()), &ctx(&pass, 1)).expect("verify");
    assert_eq!(original, report.digest);

    // Recover the save's salt only in the test to identify occupied versus unused
    // physical slots. The production verifier does not expose it.
    let mut image = reader.read_image().expect("read image");
    let mut pad = source();
    let window = pad.window(&KAPPA, 1).expect("window");
    fold(&mut image, window.as_slice());
    let (salt_offset, _) = slot_range(SALT_SLOT_INDEX);
    let mut salt = [0u8; SALT_BYTES];
    salt.copy_from_slice(&image[salt_offset..salt_offset + SALT_BYTES]);
    drop(image);
    drop(window);
    let placement = epoch_placement(&salt);
    let used = placement(0);
    let unused = placement(1);
    assert_ne!(used, unused);

    // Unused bytes are not covered by payload authentication, but the unkeyed
    // fingerprint must still change. It is not a proof that the change is benign.
    flip(&path, slot_range(unused).0);
    let changed = verify_candidate(&mut reader, &mut NoRatchet(source()), &ctx(&pass, 1))
        .expect("unused bytes are not authenticated");
    assert_ne!(changed, original);
    // A declared payload byte is authenticated and cannot be accepted after corruption.
    flip(&path, slot_range(used).0);
    assert!(verify_candidate(&mut reader, &mut NoRatchet(source()), &ctx(&pass, 1)).is_err());
    drop(reader);
    std::fs::remove_file(path).expect("cleanup");
}

#[cfg(unix)]
#[test]
fn save_refuses_same_size_original_change_without_ratchet() {
    use std::os::unix::fs::MetadataExt;

    struct MutatingPad {
        inner: DerivedPad,
        path: PathBuf,
        mutated: bool,
    }
    impl PadSource for MutatingPad {
        fn window(&mut self, kappa: &[u8; 32], cursor: u64) -> Result<pad::PadWindow, String> {
            if !self.mutated {
                // The store has already read its old image. Simulate a separate
                // process changing one byte without changing the file size.
                let mut file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&self.path)
                    .unwrap();
                file.seek(SeekFrom::Start((CONTAINER_BYTES - 1) as u64))
                    .unwrap();
                let mut byte = [0u8; 1];
                file.read_exact(&mut byte).unwrap();
                byte[0] ^= 1;
                file.seek(SeekFrom::Start((CONTAINER_BYTES - 1) as u64))
                    .unwrap();
                file.write_all(&byte).unwrap();
                file.sync_all().unwrap();
                self.mutated = true;
            }
            self.inner.window(kappa, cursor)
        }
        fn ratchet(&mut self, _kappa: &[u8; 32], _cursor: u64) -> Result<u32, String> {
            panic!("a refused replacement must not ratchet")
        }
        fn can_ratchet(&self) -> bool {
            true
        }
        fn describe(&self) -> String {
            "mutating test pad".into()
        }
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("container");
    provision(&path, [0x27u8; SALT_BYTES], 0);
    let old_ino = std::fs::metadata(&path).expect("metadata").ino();
    let mut original = File::open(&path).expect("open original");
    original
        .seek(SeekFrom::Start((CONTAINER_BYTES - 1) as u64))
        .expect("seek");
    let mut byte = [0u8; 1];
    original.read_exact(&mut byte).expect("read");
    let expected_byte = byte[0] ^ 1;
    drop(original);

    let pass = SealedPassphrase::new(PASSPHRASE).expect("passphrase");
    let mut file = ContainerFile::open(&path, true).expect("open");
    let mut pad = MutatingPad {
        inner: source(),
        path: path.clone(),
        mutated: false,
    };
    let error = save(&mut file, &mut pad, &ctx(&pass, 1), b"new document")
        .expect_err("changed original must not be overwritten");
    assert!(error.contains("contents changed while saving"), "{error}");
    assert!(
        error.contains("staged encrypted image may have been written"),
        "{error}"
    );
    assert!(error.contains("Do not retry"), "{error}");
    assert!(pad.mutated);
    assert_eq!(std::fs::metadata(&path).expect("metadata").ino(), old_ino);
    let mut after = File::open(&path).expect("open original");
    after
        .seek(SeekFrom::Start((CONTAINER_BYTES - 1) as u64))
        .expect("seek");
    after.read_exact(&mut byte).expect("read");
    assert_eq!(byte[0], expected_byte);
    // The container and the save lock's empty sidecar; no staged `*` file survives the refusal.
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .expect("list")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "container".to_string(),
            "container.holonomy-lock".to_string()
        ],
        "staged file cleaned up"
    );
}

#[cfg(unix)]
#[test]
fn save_refuses_changed_path_before_pad_access() {
    struct NeverReadPad;
    impl PadSource for NeverReadPad {
        fn window(&mut self, _kappa: &[u8; 32], _cursor: u64) -> Result<pad::PadWindow, String> {
            panic!("stale path must be rejected before pad access")
        }
        fn ratchet(&mut self, _kappa: &[u8; 32], _cursor: u64) -> Result<u32, String> {
            panic!("stale path must not ratchet")
        }
        fn can_ratchet(&self) -> bool {
            true
        }
        fn describe(&self) -> String {
            "test pad".into()
        }
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("container");
    let moved = dir.path().join("moved");
    std::fs::write(&path, b"original").expect("create");
    let mut file = ContainerFile::open(&path, true).expect("open");
    std::fs::rename(&path, &moved).expect("move original");
    std::fs::write(&path, b"new path contents").expect("replacement");
    let pass = SealedPassphrase::new(PASSPHRASE).expect("passphrase");
    let error = save(&mut file, &mut NeverReadPad, &ctx(&pass, 1), b"edit")
        .expect_err("stale handle refused");
    assert!(error.contains("same regular file"), "{error}");
    assert_eq!(
        std::fs::read(&path).expect("new path"),
        b"new path contents"
    );
    assert_eq!(std::fs::read(&moved).expect("old path"), b"original");
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .expect("list")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "container".to_string(),
            "container.holonomy-lock".to_string(),
            "moved".to_string(),
        ],
        "stale-path refusal may leave only the persistent empty lock sidecar"
    );
}

#[test]
fn ratchet_count_must_match_the_full_window() {
    let expected = PAD_SECTORS_PER_SAVE;
    assert!(validate_ratchet_count(expected, 7, 8).is_ok());
    for count in [0, 1, expected - 1, expected + 1, u32::MAX] {
        let error = validate_ratchet_count(count, 7, 8).expect_err("invalid count");
        assert!(error.contains("cursor 8"), "{error}");
        assert!(error.contains("window 7"), "{error}");
        assert!(error.contains("Do not retry"), "{error}");
    }
}

/// The store reports what it knows. It never establishes pad freshness, so the layer it hands to
/// the sealing path must say "unknown" rather than claim a fresh pad on its behalf.
#[test]
fn the_store_layer_does_not_claim_pad_freshness() {
    let layer = protection_layer();
    assert!(layer.pad);
    assert_eq!(layer.pad_fresh, None);
    assert!(!layer.describe().contains("fresh sectors"));
    assert!(!layer.describe().contains("reused"));
    assert!(layer.describe().contains("not established"));
}

/// The report a caller gets back must not carry a freshness claim the store cannot support.
#[test]
fn save_reports_unestablished_pad_freshness() {
    let path = tempfile("freshness-report");
    provision(&path, [0x13u8; SALT_BYTES], 0);
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let ctx = ctx(&pass, 1);
    let mut file = ContainerFile::open(&path, true).expect("open");
    let report = save(&mut file, &mut source(), &ctx, b"a report").expect("save");
    assert_eq!(report.protection.pad_fresh, None);
    assert!(!report.protection.describe().contains("fresh sectors"));
    assert!(report.protection.describe().contains("not established"));
    let _ = std::fs::remove_file(&path);
}

/// Two saves of one path must not interleave. The lock is advisory, keyed by path, and held only
/// for the save.
#[cfg(unix)]
#[test]
fn a_second_save_is_refused_while_a_save_is_running() {
    let path = tempfile("save-lock");
    provision(&path, [0x17u8; SALT_BYTES], 0);
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let ctx = ctx(&pass, 1);
    let mut file = ContainerFile::open(&path, true).expect("open");
    let before = std::fs::read(&path).expect("read");

    let lock = SaveLock::acquire(&path).expect("first lock");
    let error = save(&mut file, &mut source(), &ctx, b"contended")
        .expect_err("a held lock must refuse the save");
    assert!(error.contains("another Holonomy save holds"), "{error}");
    assert!(error.contains("nothing was written"), "{error}");
    assert_eq!(
        std::fs::read(&path).expect("read"),
        before,
        "container changed"
    );

    drop(lock);
    save(&mut file, &mut source(), &ctx, b"after the lock").expect("save after release");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(save_lock_path(&path).expect("lock path"));
}

/// Relative path spellings in the same directory must not bypass the save lock.
#[cfg(unix)]
#[test]
fn save_lock_normalizes_relative_parent_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).expect("nested directory");
    let path = dir.path().join("container");
    std::fs::write(&path, b"test").expect("container");
    let alternate = nested.join("..").join("container");

    let lock = SaveLock::acquire(&path).expect("first lock");
    assert!(
        SaveLock::acquire(&alternate).is_err(),
        "canonical parent paths must resolve to the same sidecar lock"
    );
    drop(lock);
}

/// Save locking preserves non-UTF-8 path bytes instead of using the lossy display string.
#[cfg(unix)]
#[test]
fn save_lock_preserves_non_utf8_container_name() {
    use std::os::unix::ffi::OsStringExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let name = std::ffi::OsString::from_vec(b"container-\xff".to_vec());
    let path = dir.path().join(name);
    std::fs::write(&path, b"test").expect("container");
    let file = ContainerFile::open(&path, false).expect("open container");

    let lock = SaveLock::acquire(file.filesystem_path()).expect("native path lock");
    let mut lock_name = path.file_name().expect("file name").to_os_string();
    lock_name.push(".holonomy-lock");
    assert!(dir.path().join(lock_name).is_file());
    drop(lock);
}

/// Atomic replacement of a hard-linked container would leave a stale copy sharing pad history.
#[cfg(unix)]
#[test]
fn writable_save_refuses_hardlinked_container_before_reading_pad() {
    struct NeverReadPad;
    impl PadSource for NeverReadPad {
        fn window(&mut self, _kappa: &[u8; 32], _cursor: u64) -> Result<pad::PadWindow, String> {
            panic!("hard-linked container must be rejected before reading the pad")
        }
        fn ratchet(&mut self, _kappa: &[u8; 32], _cursor: u64) -> Result<u32, String> {
            panic!("hard-linked container must never be ratcheted")
        }
        fn can_ratchet(&self) -> bool {
            true
        }
        fn describe(&self) -> String {
            "test pad".into()
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("container");
    let alias = dir.path().join("container-alias");
    provision(&path, [0x19u8; SALT_BYTES], 0);
    std::fs::hard_link(&path, &alias).expect("hard link");
    let before = std::fs::read(&path).expect("read original");
    let pass = SealedPassphrase::new(PASSPHRASE).expect("passphrase");
    let mut file = ContainerFile::open(&path, true).expect("open");

    let error = save(
        &mut file,
        &mut NeverReadPad,
        &ctx(&pass, 1),
        b"must not fork",
    )
    .expect_err("hard-linked path must not be atomically replaced");
    assert!(error.contains("exactly one link"), "{error}");
    assert!(error.contains("before reading the pad"), "{error}");
    assert_eq!(std::fs::read(&path).expect("read original"), before);
    assert_eq!(std::fs::read(&alias).expect("read alias"), before);
}

/// A caller that already holds the lock must not deadlock against itself.
#[cfg(unix)]
#[test]
fn a_caller_held_lock_lets_its_own_save_proceed() {
    let path = tempfile("save-lock-held");
    provision(&path, [0x18u8; SALT_BYTES], 0);
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let ctx = ctx(&pass, 1);
    let mut file = ContainerFile::open(&path, true).expect("open");
    let digest = {
        let mut reader = ContainerFile::open(&path, false).expect("open for digest");
        holonomy_session::seal::image_digest(&reader.read_image().expect("read image"))
    };
    let lock = SaveLock::acquire(&path).expect("lock");
    save_if_unchanged_locked(
        &mut file,
        &mut source(),
        &ctx,
        b"the caller already locked this path",
        &digest,
        &lock,
    )
    .expect("a held lock belongs to this save");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(save_lock_path(&path).expect("lock path"));
}

/// A remnant report is a directory listing: it must find the staged prefix, report each file once,
/// and stay quiet about everything else in the directory.
#[test]
fn remnants_are_reported_from_the_container_directory_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let container = dir.path().join("book");
    std::fs::write(&container, b"placeholder").expect("container");
    std::fs::write(dir.path().join("book.holonomy-lock"), b"").expect("lock sidecar");
    std::fs::write(dir.path().join(".holonomy-save-bbb"), vec![0u8; 10]).expect("remnant b");
    std::fs::write(dir.path().join(".holonomy-save-aaa"), vec![0u8; 4]).expect("remnant a");
    std::fs::write(dir.path().join("notes.txt"), b"unrelated").expect("unrelated");

    let (found, truncated) = find_save_remnants(&container).expect("scan");
    assert!(!truncated);
    let names: Vec<String> = found
        .iter()
        .map(|remnant| {
            remnant
                .path
                .file_name()
                .expect("name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ".holonomy-save-aaa".to_string(),
            ".holonomy-save-bbb".to_string()
        ],
        "sorted, and only the staged prefix"
    );
    assert_eq!(found[0].len, 4);
    assert_eq!(found[1].len, 10);

    // Pointing at the directory itself answers the same question.
    assert_eq!(find_save_remnants(dir.path()).expect("scan dir").0.len(), 2);
}

/// A planted symlink is not a remnant: the scan must not follow it.
#[cfg(unix)]
#[test]
fn remnants_do_not_follow_symlinks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let container = dir.path().join("book");
    std::fs::write(&container, b"placeholder").expect("container");
    let target = dir.path().join("elsewhere");
    std::fs::write(&target, b"not a remnant").expect("target");
    std::os::unix::fs::symlink(&target, dir.path().join(".holonomy-save-link")).expect("symlink");
    let (found, _) = find_save_remnants(&container).expect("scan");
    assert!(found.is_empty(), "{found:?}");
    assert!(
        !is_save_remnant_beside(&container, &dir.path().join(".holonomy-save-link"))
            .expect("direct symlink check"),
        "an explicit recovery path must not follow the planted symlink"
    );
    assert!(
        open_save_remnant_beside(&container, &dir.path().join(".holonomy-save-link")).is_err(),
        "opening the selected remnant must reject a symlink"
    );
}

/// New-format origin names parse strictly, while pre-v2 names remain explicitly unbound.
#[cfg(unix)]
#[test]
fn remnant_origin_names_distinguish_legacy_from_malformed_v2() {
    let digest = [0xA5; 32];
    let name = format!("{}nonce", remnant_v2_prefix(7, 11, &digest));
    let path = std::path::Path::new(&name);
    assert_eq!(
        parse_remnant_origin(path),
        RemnantOrigin::Bound {
            device: 7,
            inode: 11,
            old_digest: digest,
        }
    );
    assert_eq!(
        parse_remnant_origin(std::path::Path::new(".holonomy-save-old")),
        RemnantOrigin::LegacyUnbound
    );
    assert_eq!(
        parse_remnant_origin(std::path::Path::new(".holonomy-save-v2-broken")),
        RemnantOrigin::MalformedBound
    );
}

/// A v2 origin matches only the same Unix file object with its original complete image bytes.
#[cfg(unix)]
#[test]
fn remnant_association_checks_file_identity_and_full_old_image_digest() {
    use std::io::{Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let container = dir.path().join("book");
    let file = File::create(&container).expect("create sparse container");
    file.set_len(CONTAINER_BYTES as u64)
        .expect("size container");
    drop(file);
    let metadata = std::fs::metadata(&container).expect("container metadata");
    let origin = current_container_origin(&container).expect("current container origin");
    let bound = dir.path().join(format!(
        "{}nonce",
        remnant_v2_prefix(metadata.dev(), metadata.ino(), &origin.image_digest)
    ));
    std::fs::write(&bound, b"staged image").expect("bound remnant");
    assert_eq!(
        remnant_association_with_origin(&container, &bound, Some(&origin))
            .expect("matching association"),
        RemnantAssociation::BoundToCurrentContainer
    );

    let wrong_identity = dir.path().join(format!(
        "{}nonce",
        remnant_v2_prefix(metadata.dev() ^ 1, metadata.ino(), &origin.image_digest)
    ));
    std::fs::write(&wrong_identity, b"staged image").expect("wrong-identity remnant");
    assert_eq!(
        remnant_association_with_origin(&container, &wrong_identity, Some(&origin))
            .expect("identity mismatch"),
        RemnantAssociation::DoesNotMatch
    );

    let mut changed = OpenOptions::new()
        .write(true)
        .open(&container)
        .expect("open container to mutate");
    changed.seek(SeekFrom::Start(0)).expect("seek");
    changed.write_all(&[1]).expect("change old image in place");
    changed.sync_all().expect("sync mutation");
    assert_eq!(
        remnant_association(&container, &bound).expect("changed association"),
        RemnantAssociation::DoesNotMatch
    );

    let legacy = dir.path().join(".holonomy-save-legacy");
    std::fs::write(&legacy, b"legacy staged image").expect("legacy remnant");
    assert_eq!(
        remnant_association(&container, &legacy).expect("legacy association"),
        RemnantAssociation::LegacyUnbound
    );
}

/// The listing is bounded, and the caller is told when it stopped early.
#[test]
fn remnant_reporting_is_bounded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let container = dir.path().join("book");
    std::fs::write(&container, b"placeholder").expect("container");
    for index in (0..(MAX_REMNANTS_REPORTED + 3)).rev() {
        std::fs::write(dir.path().join(format!(".holonomy-save-{index:03}")), b"x")
            .expect("remnant");
    }
    let (found, truncated) = find_save_remnants(&container).expect("scan");
    assert_eq!(found.len(), MAX_REMNANTS_REPORTED);
    assert!(truncated, "the report must say it was truncated");
    let names: Vec<_> = found
        .iter()
        .filter_map(|remnant| remnant.path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names.len(),
        found.len(),
        "every remnant path must have a filename"
    );
    let expected: Vec<_> = (0..MAX_REMNANTS_REPORTED)
        .map(|index| format!(".holonomy-save-{index:03}"))
        .collect();
    assert_eq!(names, expected, "the bounded result must be stable by name");
}

/// A display cap must not make an explicitly selected valid remnant inaccessible.
#[test]
fn explicit_remnant_membership_is_not_limited_by_the_report_cap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let container = dir.path().join("book");
    std::fs::write(&container, b"placeholder").expect("container");
    for index in 0..(MAX_REMNANTS_REPORTED + 1) {
        std::fs::write(dir.path().join(format!(".holonomy-save-{index:03}")), b"x")
            .expect("remnant");
    }
    let beyond_report = dir
        .path()
        .join(format!(".holonomy-save-{:03}", MAX_REMNANTS_REPORTED + 1));
    std::fs::write(&beyond_report, b"selected").expect("selected remnant");

    let (reported, truncated) = find_save_remnants(&container).expect("bounded scan");
    assert!(truncated);
    assert_eq!(reported.len(), MAX_REMNANTS_REPORTED);
    assert!(!reported.iter().any(|entry| entry.path == beyond_report));
    assert!(is_save_remnant_beside(&container, &beyond_report).expect("direct check"));
    let ordinary_copy = dir.path().join("ordinary-copy");
    let ordinary_is_remnant =
        is_save_remnant_beside(&container, &ordinary_copy).expect("ordinary file check");
    assert!(!ordinary_is_remnant);
}

/// The reported digest is the same unkeyed fingerprint the store reports for a container image.
#[test]
fn remnant_digest_matches_the_container_fingerprint() {
    use std::io::{Seek, SeekFrom};

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("image");
    let bytes: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &bytes).expect("write");
    let expected = holonomy_session::seal::image_digest(&bytes);
    assert_eq!(path_digest(&path).expect("path digest"), expected);

    let mut file = File::open(&path).expect("open for streaming digest");
    file.seek(SeekFrom::Start(13)).expect("set nonzero offset");
    assert_eq!(file_digest(&mut file).expect("file digest"), expected);
    assert_eq!(file.stream_position().expect("current offset"), 13);
}

/// The recovery question after a crash: which cursor does this image need? A scan answers it,
/// reports the same fingerprint `verify` does, and changes nothing.
#[test]
fn scanning_cursors_finds_the_saved_cursor_and_writes_nothing() {
    let path = tempfile("cursor-scan");
    provision(&path, [0x31u8; SALT_BYTES], 0);
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    for cursor in [1u64, 2] {
        let mut file = ContainerFile::open(&path, true).expect("open");
        save(
            &mut file,
            &mut source(),
            &ctx(&pass, cursor),
            b"a scanned document",
        )
        .expect("save");
    }
    let before = std::fs::read(&path).expect("read container");

    let mut file = ContainerFile::open(&path, false).expect("open read-only");
    let scan = scan_cursor_range(&mut file, &mut source(), &ctx(&pass, 0), 0..4, true);
    assert_eq!(scan.hits.len(), 1, "{scan:?}");
    assert_eq!(scan.hits[0].cursor, 2);
    assert_eq!(scan.attempts, 3, "cursors 0, 1 and 2");
    assert!(!scan.cancelled);
    assert_eq!(
        scan.hits[0].digest,
        holonomy_session::seal::image_digest(&before),
        "the reported fingerprint must be the one verify reports for this image"
    );
    drop(file);
    assert_eq!(
        std::fs::read(&path).expect("read container again"),
        before,
        "a scan must not write the container"
    );

    // A cancellation requested during one expensive attempt stops before the next candidate.
    struct CancelAfterWindow<'a> {
        inner: DerivedPad,
        cancelled: &'a std::sync::atomic::AtomicBool,
    }
    impl PadSource for CancelAfterWindow<'_> {
        fn window(&mut self, kappa: &[u8; 32], cursor: u64) -> Result<pad::PadWindow, String> {
            let window = self.inner.window(kappa, cursor);
            self.cancelled
                .store(true, std::sync::atomic::Ordering::Relaxed);
            window
        }
        fn ratchet(&mut self, _kappa: &[u8; 32], _cursor: u64) -> Result<u32, String> {
            panic!("read-only cursor scan must not ratchet")
        }
        fn can_ratchet(&self) -> bool {
            false
        }
        fn describe(&self) -> String {
            "cancel-after-window test pad".into()
        }
    }
    let mut file = ContainerFile::open(&path, false).expect("open read-only");
    let cancel_after_first = std::sync::atomic::AtomicBool::new(false);
    let completed = std::sync::atomic::AtomicU64::new(0);
    let mut cancel_pad = CancelAfterWindow {
        inner: source(),
        cancelled: &cancel_after_first,
    };
    let cancelled = scan_cursor_range_with_cancel(
        &mut file,
        &mut cancel_pad,
        &ctx(&pass, 0),
        0..1,
        true,
        || cancel_after_first.load(std::sync::atomic::Ordering::Relaxed),
        |attempts| completed.store(attempts, std::sync::atomic::Ordering::Relaxed),
    );
    assert!(cancelled.cancelled);
    assert_eq!(cancelled.attempts, 1);
    assert_eq!(completed.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(cancelled.hits.is_empty());

    // Without the early stop the whole range is tried, and still only one cursor opens.
    let mut file = ContainerFile::open(&path, false).expect("open read-only");
    let wide = scan_cursor_range(&mut file, &mut source(), &ctx(&pass, 0), 0..4, false);
    assert_eq!(wide.hits.len(), 1, "{wide:?}");
    assert_eq!(wide.hits[0].cursor, 2);
    assert_eq!(wide.attempts, 4, "every cursor in the range was tried");
    let _ = std::fs::remove_file(&path);
}

/// A scan that opens nothing keeps the last refusal, because "no cursor worked" is not a diagnosis.
#[test]
fn a_failed_scan_reports_the_last_refusal() {
    let path = tempfile("cursor-scan-failure");
    provision(&path, [0x32u8; SALT_BYTES], 0);
    let pass = SealedPassphrase::new(PASSPHRASE).expect("legal");
    let mut file = ContainerFile::open(&path, true).expect("open");
    save(&mut file, &mut source(), &ctx(&pass, 1), b"document").expect("save");
    drop(file);

    let wrong = SealedPassphrase::new("not the passphrase").expect("legal");
    let mut file = ContainerFile::open(&path, false).expect("open");
    let scan = scan_cursor_range(&mut file, &mut source(), &ctx(&wrong, 0), 0..4, true);
    assert!(scan.hits.is_empty(), "{scan:?}");
    assert_eq!(
        scan.attempts, 4,
        "the early stop ends a scan that found something; a scan that opens nothing walks the range"
    );
    assert!(
        scan.last_error.is_some(),
        "the refusal must be kept for the report"
    );
    let _ = std::fs::remove_file(&path);
}
