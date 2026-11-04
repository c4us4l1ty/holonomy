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
            pad_fresh: true,
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
    let report = save(&mut file, &mut source(), &ctx, document).expect("save");
    assert_eq!(report.plaintext_bytes, document.len());
    assert_eq!(report.slots, 1);
    assert_eq!(report.pad_sectors, PAD_SECTORS);
    assert!(report.ratcheted.is_none(), "a file has no ratchet");

    // A fresh handle, exactly as a second process would open it.
    drop(file);
    let mut file = ContainerFile::open(&path, false).expect("reopen");
    let (opened, read_back) = open_document(&mut file, &mut source(), &ctx).expect("open");
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
    let report = save(&mut file, &mut source(), &ctx, &document).expect("save");
    assert_eq!(report.slots, 3);
    drop(file);

    let mut file = ContainerFile::open(&path, false).expect("reopen");
    let (opened, read_back) = open_document(&mut file, &mut source(), &ctx).expect("open");
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
        let plan = holonomy_usb::pad::PadPlan::for_save(&k_perm, cursor);
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
            pad_fresh: true,
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
    for salt in [[0x00u8; SALT_BYTES], [0x33u8; SALT_BYTES], [0xFFu8; SALT_BYTES]] {
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
        assert!(!seen[0] && !seen[1], "the capsule and the salt must be untouched");
    }
}

#[test]
fn two_salts_permute_the_same_slot_differently() {
    // Without this the permutation is just the identity with extra steps. If two epochs
    // produced the same mapping, a save would leave the layout of the previous one intact and the
    // whole mechanism would buy nothing.
    let a = epoch_placement(&[0x11u8; SALT_BYTES]);
    let b = epoch_placement(&[0x22u8; SALT_BYTES]);
    let differing = (0..holonomy_core::layout::PAYLOAD_SLOTS)
        .filter(|&i| a(i) != b(i))
        .count();
    let slots = holonomy_core::layout::PAYLOAD_SLOTS;
    assert!(
        differing > slots * 99 / 100,
        "only {differing} of {slots} slots moved between two epochs; the permutation is barely doing anything"
    );
}

#[test]
fn a_saved_payload_is_not_written_in_logical_order() {
    // The observable consequence, on a real container: after unfolding, the ciphertext of slot 0 must
    // *not* be at block 2, because block 2 is where an identity placement would have put it. A test
    // that only checked the placement function in isolation could pass while the store still called
    // the identity.
    use holonomy_core::layout::{PAYLOAD_SLOT_BASE, PAYLOAD_SLOTS};
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
    assert_ne!(new_salt, [0u8; SALT_BYTES], "the save must choose a fresh salt");
    let place = epoch_placement(&new_salt);

    let flat = place(0);
    assert_ne!(
        flat, PAYLOAD_SLOT_BASE,
        "logical slot 0 landed on the first payload block, which is what the identity placement does"
    );
    assert!(flat < PAYLOAD_SLOTS + PAYLOAD_SLOT_BASE);

    // The slot at the permuted position carries ciphertext; the block the identity would have used
    // is not necessarily empty (another slot may have landed there), so the assertion is about the
    // mapping rather than about which block is occupied.
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
    assert_eq!(read_back, b"the second document, longer than the first");
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
