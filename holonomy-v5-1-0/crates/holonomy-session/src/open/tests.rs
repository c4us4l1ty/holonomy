//! The round trip, which is the only test in this project that proves a saved document can be
//! read back.
//!
//! # What was wrong before this existed
//!
//! Two defects, one in each half, each invisible on its own:
//!
//! 1. `seal_container` sealed the payload under the *stratum's* `K_DEM` and the directory under
//!    the *envelope's* — two unrelated keys inside one container.
//! 2. `unlock_fixed` verified the directory with the envelope's key and then handed back the
//!    *fallback* key set derived from the root secret, which is a third key again.
//!
//! Both were consistent with every existing test: `seal.rs`'s tests opened slots with the same
//! stratum they sealed them with, and `unlock.rs`'s tests asserted only that `k_dem` was not all
//! zeros. A container written by one and read by the other could never have opened, and the
//! failure would have appeared as "the document is corrupt" on a correct passphrase, on the first
//! real save.
//!
//! The tests here go through the public API end to end: real `Argon2id` (at `ParamsClass::Test`
//! cost), the real `ML-KEM-1024 + X25519` envelope, a real 134,217,728-byte image. They are the
//! reason `CI` runs `a_saved_container_reopens_to_the_bytes_that_went_in`.

use super::*;
use holonomy_core::layout::{slot_range, CONTAINER_BYTES, PAYLOAD_SLOT_BASE, SALT_BYTES};
use holonomy_core::stratum::{ActiveStratum, StratumKeys};
use holonomy_core::unlock::{SealedPassphrase, Topology};
use holonomy_crypto::envelope::derive_stratum_keys;
use holonomy_crypto::kdf::derive_root_secret;
use holonomy_crypto::ParamsClass;

use crate::salt::SaltBytes;

const KAPPA: [u8; 32] = [0x77u8; 32];
const PASSPHRASE: &str = "correct horse battery staple";

/// The identity placement the store replaces with the epoch permutation.
fn identity(i: usize) -> usize {
    PAYLOAD_SLOT_BASE + i
}

/// A placement that reverses the logical order, to prove the permutation is actually consulted
/// rather than being decoration that happens to work at identity.
fn reversed(i: usize) -> usize {
    PAYLOAD_SLOT_BASE + (PAYLOAD_SLOTS - 1 - i)
}

/// The stratum a save begins from: the fallback key set for `(passphrase, salt, index)`.
///
/// # Why the *fallback* set, and why that is not the bug this module documents
///
/// The save path only ever reads two fields of this key set — `k_unwrap` and `n_epoch` — because
/// `seal_container` creates its own envelope and seals everything under the `K_DEM` that envelope
/// produces. The fallback set's `k_dem` is therefore ignored by the writer, and the fallback set's
/// `k_unwrap` is *by construction* the value the capsule is bound to (see
/// `the_fallback_keys_equal_the_ones_the_capsule_was_built_with`). So a save driven from it writes
/// a container whose keys an unlock reproduces exactly.
///
/// The defect this module documents was the other direction: the *reader* returned `k_dem` from
/// this same derivation while the writer had used the envelope's, so no read ever matched a write.
fn stratum_from_root(
    passphrase: &str,
    salt: &[u8; SALT_BYTES],
    kappa: &[u8; 32],
    index: u8,
) -> ActiveStratum {
    use holonomy_core::stratum::StratumId;
    use holonomy_core::unlock::{ProtectionLayer, Unlocked};

    let root = derive_root_secret(passphrase.as_bytes(), salt, ParamsClass::Test).unwrap();
    let keys: StratumKeys = derive_stratum_keys(&root, index, kappa);
    let id = StratumId::new(index).unwrap();
    let layer = ProtectionLayer {
        pad: true,
        pad_fresh: Some(true),
        post_quantum: true,
    };
    Unlocked::new(id, Topology::UmbilicalUsb, id, layer).into_active(keys)
}

fn sealed_request(passphrase: &str) -> SealedPassphrase {
    SealedPassphrase::new(passphrase).expect("length-legal")
}

/// Save `document` as a container image, the way a real save would.
fn save_to_image(
    document: &[&[u8]],
    passphrase: &str,
    salt_bytes: [u8; SALT_BYTES],
    cursor: u64,
    kappa: &[u8; 32],
    placement: impl Fn(usize) -> usize,
) -> Vec<u8> {
    let stratum = stratum_from_root(passphrase, &salt_bytes, kappa, 1);
    let salt = SaltBytes::of_unknown_origin(salt_bytes);
    let sealed =
        crate::seal::seal_container(&stratum, document, salt, cursor, kappa).expect("seal");
    let mut image = vec![0u8; CONTAINER_BYTES];
    sealed.encode_into(&mut image, placement).expect("encode");
    image
}

fn open(
    image: &[u8],
    passphrase: &str,
    kappa: &[u8; 32],
    placement: impl Fn(usize) -> usize,
) -> Result<OpenedContainer, OpenError> {
    let pass = sealed_request(passphrase);
    open_container(
        &ReaderRequest {
            image,
            passphrase: &pass,
            kappa_usb: kappa,
            topology: Topology::UmbilicalUsb,
            params: ParamsClass::Test,
            pad_fresh: Some(true),
        },
        placement,
    )
}

#[test]
fn a_saved_container_reopens_to_the_bytes_that_went_in() {
    // The test the whole save path exists for. It fails against either of the two key-mismatch
    // defects described in this module's docs.
    let salt = [0x21u8; SALT_BYTES];
    let doc: [&[u8]; 2] = [
        b"CHAPTER ONE\nIt was a bright cold day in April.",
        b"CHAPTER TWO\n",
    ];
    let image = save_to_image(&doc, PASSPHRASE, salt, 0, &KAPPA, identity);

    let opened =
        open(&image, PASSPHRASE, &KAPPA, identity).expect("a container this build wrote must open");
    assert_eq!(opened.stratum.id().index(), 1);
    assert_eq!(opened.directory.slot_count(), 2);
    assert_eq!(opened.cursor(), 0);

    let mut expected = Vec::new();
    for part in doc {
        expected.extend_from_slice(part);
    }
    let document = opened.payload_bytes();
    assert_eq!(document.as_slice(), expected);
    assert!(!format!("{document:?}").contains("CHAPTER ONE"));
    // Slot by slot too, so a concatenation that happened to match is not enough.
    assert_eq!(opened.payload[0].as_slice(), doc[0]);
    assert_eq!(opened.payload[1].as_slice(), doc[1]);
}

#[test]
fn the_permutation_is_actually_consulted() {
    // Saves and opens under a non-identity placement. A reader that ignored `placement` would pass
    // the identity test and fail here, which is the difference between "the code has a parameter"
    // and "the code needs it".
    let salt = [0x31u8; SALT_BYTES];
    let doc: [&[u8]; 3] = [b"alpha", b"beta", b"gamma"];
    let image = save_to_image(&doc, PASSPHRASE, salt, 4, &KAPPA, reversed);

    // Slot 0's ciphertext must be at the *reversed* block and not at the identity one. Comparing
    // the two blocks directly is enough: under `identity` that block holds slot 0, and here it
    // holds whatever `reversed` put there (slot 2's data, or nothing).
    let (identity_off, identity_len) = slot_range(identity(0));
    let (reversed_off, reversed_len) = slot_range(reversed(0));
    assert_eq!(identity_len, reversed_len);
    assert!(
        image[identity_off..identity_off + identity_len]
            .iter()
            .all(|&b| b == 0),
        "under a reversed placement the identity block must be untouched"
    );
    assert!(
        image[reversed_off..reversed_off + reversed_len]
            .iter()
            .any(|&b| b != 0),
        "the reversed block must hold slot 0"
    );

    let opened =
        open(&image, PASSPHRASE, &KAPPA, reversed).expect("must open under its own placement");
    assert_eq!(opened.payload[0].as_slice(), b"alpha");
    assert_eq!(opened.payload[2].as_slice(), b"gamma");

    // And the wrong placement does not silently return the wrong document.
    assert!(open(&image, PASSPHRASE, &KAPPA, identity).is_err());
}

#[test]
fn the_save_cursor_survives_a_round_trip() {
    // The cursor is the only record of which pad window a container consumed, and it has no field
    // in the capsule. If the ledger loses it, the next save reads the wrong pad.
    let salt = [0x41u8; SALT_BYTES];
    for cursor in [0u64, 1, 53, 54, 1_000_000] {
        let image = save_to_image(&[b"x"], PASSPHRASE, salt, cursor, &KAPPA, identity);
        let opened = open(&image, PASSPHRASE, &KAPPA, identity).expect("opens");
        assert_eq!(opened.cursor(), cursor, "cursor {cursor} did not survive");
    }
}

#[test]
fn a_wrong_passphrase_is_rejected_and_says_nothing() {
    let salt = [0x51u8; SALT_BYTES];
    let image = save_to_image(&[b"secret"], PASSPHRASE, salt, 0, &KAPPA, identity);
    let err = open(&image, "not the passphrase", &KAPPA, identity).unwrap_err();
    assert!(matches!(err, OpenError::Rejected(_)), "{err}");
    let text = err.to_string();
    assert!(text.contains("did not open"), "{text}");
    // No distinction between "wrong passphrase" and "wrong capsule" in the message.
    let lower = text.to_lowercase();
    assert!(!lower.contains("commitment"), "{text}");
    assert!(!lower.contains("slot"), "{text}");
}

#[test]
fn a_wrong_usb_share_is_rejected() {
    // The bipartite property: the passphrase alone is not enough.
    let salt = [0x61u8; SALT_BYTES];
    let image = save_to_image(&[b"secret"], PASSPHRASE, salt, 0, &KAPPA, identity);
    assert!(matches!(
        open(&image, PASSPHRASE, &[0x66u8; 32], identity).unwrap_err(),
        OpenError::Rejected(_)
    ));
}

#[test]
fn a_tampered_payload_slot_is_caught_by_its_commitment() {
    let salt = [0x71u8; SALT_BYTES];
    let image = save_to_image(&[b"the document"], PASSPHRASE, salt, 0, &KAPPA, identity);
    let mut broken = image.clone();
    let (off, _) = slot_range(identity(0));
    broken[off] ^= 1;

    let err = open(&broken, PASSPHRASE, &KAPPA, identity).unwrap_err();
    assert!(matches!(err, OpenError::Slot(_)), "{err}");
}

#[test]
fn a_relocated_payload_slot_is_caught() {
    // Two slots swapped in the image. Each still carries a valid ciphertext and its own tag, so the
    // only thing that catches this is the AAD binding the logical index -- plus the ledger tag over
    // the commitments, which no longer line up with the slots they name.
    let salt = [0x81u8; SALT_BYTES];
    let image = save_to_image(
        &[b"first", b"second"],
        PASSPHRASE,
        salt,
        0,
        &KAPPA,
        identity,
    );
    let mut broken = image.clone();
    let (a, len) = slot_range(identity(0));
    let (b, _) = slot_range(identity(1));
    let first: Vec<u8> = broken[a..a + len].to_vec();
    let second: Vec<u8> = broken[b..b + len].to_vec();
    broken[a..a + len].copy_from_slice(&second);
    broken[b..b + len].copy_from_slice(&first);

    assert!(open(&broken, PASSPHRASE, &KAPPA, identity).is_err());
}

#[test]
fn a_truncated_image_is_refused_before_anything_is_read() {
    let pass = sealed_request(PASSPHRASE);
    let short = vec![0u8; 1024];
    let err = open_container(
        &ReaderRequest {
            image: &short,
            passphrase: &pass,
            kappa_usb: &KAPPA,
            topology: Topology::UmbilicalUsb,
            params: ParamsClass::Test,
            pad_fresh: Some(true),
        },
        identity,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        OpenError::WrongImageSize {
            found: 1024,
            needed: CONTAINER_BYTES
        }
    ));
}

#[test]
fn an_unprovisioned_container_is_named_rather_than_rejected_as_a_bad_passphrase() {
    // An all-zero salt slot is a device that has never been written. Reporting it as "wrong
    // passphrase" would send a user hunting for a credential they have not mistyped.
    let pass = sealed_request(PASSPHRASE);
    let zeros = vec![0u8; CONTAINER_BYTES];
    let err = open_container(
        &ReaderRequest {
            image: &zeros,
            passphrase: &pass,
            kappa_usb: &KAPPA,
            topology: Topology::UmbilicalUsb,
            params: ParamsClass::Test,
            pad_fresh: Some(true),
        },
        identity,
    )
    .unwrap_err();
    assert!(
        matches!(err, OpenError::Header(HeaderError::Unformatted)),
        "{err}"
    );
    assert!(err.to_string().contains("provisioned"), "{err}");
}

#[test]
fn a_second_save_of_the_same_document_is_a_different_container() {
    // The envelope is fresh per save and the salt is per save, so two saves of identical plaintext
    // share no bytes an observer can correlate. This is the IND-URN claim at the one place a test
    // can check it without a statistical model.
    let doc: [&[u8]; 1] = [b"the same text both times"];
    let a = save_to_image(&doc, PASSPHRASE, [0x91u8; SALT_BYTES], 0, &KAPPA, identity);
    let b = save_to_image(&doc, PASSPHRASE, [0x92u8; SALT_BYTES], 1, &KAPPA, identity);

    // Compare the payload slot, where the plaintext would show through if anything were reused.
    let (off, _) = slot_range(identity(0));
    assert_ne!(
        &a[off..off + 64],
        &b[off..off + 64],
        "two saves of one document share ciphertext"
    );
    // And both still open, so the difference is not damage.
    assert!(open(&a, PASSPHRASE, &KAPPA, identity).is_ok());
    assert!(open(&b, PASSPHRASE, &KAPPA, identity).is_ok());
}

#[test]
fn the_reader_agrees_with_the_handshake() {
    // The reader and `unlock_fixed` must accept exactly the same containers. If they diverged on a
    // key derivation the reader could open what the handshake rejects, and the handshake is the gate
    // that says a credential is right.
    let salt = [0xA1u8; SALT_BYTES];
    let image = save_to_image(&[b"payload"], PASSPHRASE, salt, 2, &KAPPA, identity);

    let mut capsule_bytes = [0u8; SLOT_BYTES];
    capsule_bytes.copy_from_slice(&image[..SLOT_BYTES]);
    let capsule = CapsuleWire(capsule_bytes);
    let pass = sealed_request(PASSPHRASE);
    let req = UnlockRequest {
        passphrase: &pass,
        salt: &salt,
        capsule: &capsule,
        kappa_usb: &KAPPA,
        topology: Topology::UmbilicalUsb,
        params: ParamsClass::Test,
        pad_fresh: Some(true),
    };
    let via_handshake = unlock_fixed(&req, &holonomy_crypto::MlKem1024X25519).expect("handshake");
    let via_reader = open(&image, PASSPHRASE, &KAPPA, identity).expect("reader");

    assert_eq!(via_handshake.id().index(), via_reader.stratum.id().index());
    // The key sets must be the same, not merely both valid: this is the equality that was broken
    // when the handshake returned the fallback derivation.
    assert_eq!(via_handshake.keys().k_dem, via_reader.stratum.keys().k_dem);
    assert_eq!(
        via_handshake.keys().n_epoch,
        via_reader.stratum.keys().n_epoch
    );
}

#[test]
fn the_payload_zeroizes_on_drop() {
    // Not a cryptographic guarantee, and not asserted as one: this checks the type is the one that
    // wipes, by checking the drop path exists rather than by reading freed memory.
    let salt = [0xB1u8; SALT_BYTES];
    let image = save_to_image(&[b"plaintext"], PASSPHRASE, salt, 0, &KAPPA, identity);
    let opened = open(&image, PASSPHRASE, &KAPPA, identity).expect("opens");
    let slot: Zeroizing = opened.payload.into_iter().next().expect("one slot");
    assert_eq!(slot.as_slice(), b"plaintext");
    drop(slot); // the `Drop` impl runs here; nothing to observe afterwards, by design
}
