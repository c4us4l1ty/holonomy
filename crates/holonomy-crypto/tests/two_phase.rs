//! The two-phase envelope, end to end, with the real `ML-KEM-1024` and `X25519`.
//!
//! These are the tests that would have caught the `π(0)` deadlock in `v4.0.0`: they prove that
//! everything needed to unlock slot 0 is derived *before* slot 0 is read, and that the secret key
//! never has to leave `holonomy-crypto` for a capsule to be written or opened.

use holonomy_core::capsule::{CapsuleWire, StratumRootCapsuleV5, WRAPPED_SK_BYTES};
use holonomy_core::layout::SLOT_BYTES;
use holonomy_crypto::commit::{open_directory, seal_directory};
use holonomy_crypto::envelope::{
    create_envelope, derive_dem_key, phase1_commitment, phase2_commitment, unwrap_stratum_key,
    verify_phase_commitments, HybridCiphertext, HybridKem, MlKem1024X25519,
};
use holonomy_crypto::kdf::{derive_root_secret, ParamsClass};

/// Directory ciphertext capacity inside the capsule, after the trailing Poly1305 tag.
const DIR_CAPACITY: usize = 60_592;

/// Build slot 0 the way a save does, using only the public save-path API.
fn build_capsule(
    k_unwrap: &[u8; 32],
    sk_nonce: &[u8; 24],
    kappa: &[u8; 32],
    directory: &[u8],
) -> StratumRootCapsuleV5 {
    let kem = MlKem1024X25519;
    let (ct, wrapped, keys) = create_envelope(k_unwrap, sk_nonce, kappa, &kem).unwrap();
    let p1 = phase1_commitment(k_unwrap, sk_nonce, &ct, &wrapped);

    // Phase 2: encrypt the directory under K_DEM, with the epoch nonce. `seal_directory` pads the
    // plaintext to the fixed field length first -- see its docs for why a variable-length
    // ciphertext cannot work with a capsule that has no length field.
    let sealed = seal_directory(&keys.k_dem, &keys.n_epoch, directory).unwrap();
    assert_eq!(
        sealed.len(),
        DIR_CAPACITY + 16,
        "the fixed-length seal must fill the field exactly"
    );
    let p2 = phase2_commitment(&keys.k_dem, &keys.n_epoch, &sealed);

    // The capsule stores the ciphertext in the big field and the Poly1305 tag separately, which is
    // how the 60,592 + 16 layout is meant to be used: the tag is *not* duplicated inside the field.
    let mut enc = [0u8; DIR_CAPACITY];
    let mut poly = [0u8; 16];
    enc.copy_from_slice(&sealed[..DIR_CAPACITY]);
    poly.copy_from_slice(&sealed[DIR_CAPACITY..]);

    StratumRootCapsuleV5 {
        ct_ml_kem_1024: ct.ml_kem,
        ct_x25519: ct.x25519,
        sk_unwrap_nonce: *sk_nonce,
        wrapped_stratum_sk: wrapped,
        phase1_cmt4_tag: p1,
        directory_nonce: keys.n_epoch,
        encrypted_directory_and_reservoir: enc,
        directory_poly1305_tag: poly,
        phase2_cmt4_tag: p2,
    }
}

/// Reassemble the AEAD input from the two capsule fields. Fixed length, by type.
fn reassemble(c: &StratumRootCapsuleV5) -> [u8; DIR_CAPACITY + 16] {
    let mut v = [0u8; DIR_CAPACITY + 16];
    v[..DIR_CAPACITY].copy_from_slice(&c.encrypted_directory_and_reservoir);
    v[DIR_CAPACITY..].copy_from_slice(&c.directory_poly1305_tag);
    v
}

/// The unlock path, as a test helper: capsule in, directory out.
fn open_capsule(
    c: &StratumRootCapsuleV5,
    k_unwrap: &[u8; 32],
    kappa: &[u8; 32],
) -> Result<Vec<u8>, String> {
    let kem = MlKem1024X25519;
    let dk = unwrap_stratum_key(k_unwrap, &c.sk_unwrap_nonce, &c.wrapped_stratum_sk)
        .map_err(|e| format!("phase 1: {e}"))?;
    let ct = HybridCiphertext { ml_kem: c.ct_ml_kem_1024, x25519: c.ct_x25519 };
    let ss = kem.decapsulate(&dk, &ct).map_err(|e| format!("phase 2 kem: {e}"))?;
    let keys = derive_dem_key(&ss, &ct, k_unwrap, kappa);

    let p1 = phase1_commitment(k_unwrap, &c.sk_unwrap_nonce, &ct, &c.wrapped_stratum_sk);
    let p2 = phase2_commitment(&keys.k_dem, &c.directory_nonce, &reassemble(c));
    verify_phase_commitments(&c.phase1_cmt4_tag, &p1, &c.phase2_cmt4_tag, &p2)
        .map_err(|e| format!("commitment: {e}"))?;

    open_directory(&keys.k_dem, &c.directory_nonce, &reassemble(c))
        .map_err(|e| format!("directory: {e}"))
}

#[test]
fn capsule_is_exactly_one_slot_after_a_real_build() {
    let c = build_capsule(&[1u8; 32], &[2u8; 24], &[3u8; 32], b"directory payload");
    let wire = CapsuleWire::from_capsule(&c);
    assert_eq!(wire.as_bytes().len(), SLOT_BYTES);
    assert_eq!(c.wrapped_stratum_sk.len(), WRAPPED_SK_BYTES);
    assert_eq!(WRAPPED_SK_BYTES, 3_216);
}

#[test]
fn unlock_recovers_the_directory() {
    let k_unwrap = [0xAAu8; 32];
    let kappa = [0xBBu8; 32];
    let directory = b"merkle root + decoy capability envelopes";

    let c = build_capsule(&k_unwrap, &[0xCCu8; 24], &kappa, directory);
    // Round-trip through the wire form, which is what the disk actually holds.
    let wire = CapsuleWire::from_capsule(&c);
    let back = wire.to_capsule();

    let plain = open_capsule(&back, &k_unwrap, &kappa).expect("a correctly built capsule must open");
    assert_eq!(&plain[..directory.len()], directory);
}

#[test]
fn wrong_passphrase_cannot_reach_phase_two() {
    // The deadlock test: with the wrong K_unwrap, phase 1 fails and nothing downstream runs.
    let c = build_capsule(&[0x01u8; 32], &[0x02u8; 24], &[0x03u8; 32], b"x");
    let err = unwrap_stratum_key(&[0xFFu8; 32], &c.sk_unwrap_nonce, &c.wrapped_stratum_sk)
        .unwrap_err();
    assert_eq!(err, holonomy_crypto::envelope::EnvelopeError::UnwrapFailed);
    assert!(open_capsule(&c, &[0xFFu8; 32], &[0x03u8; 32]).is_err());
}

#[test]
fn wrong_usb_cannot_open_the_directory() {
    // Different κ ⇒ different K_DEM ⇒ the phase-2 commitment fails. The bipartite property, as a
    // test rather than as a claim: without the USB, the passphrase alone does not open payload.
    let k_unwrap = [0x11u8; 32];
    let c = build_capsule(&k_unwrap, &[0x12u8; 24], &[0x22u8; 32], b"confidential");
    let err = open_capsule(&c, &k_unwrap, &[0x23u8; 32]).unwrap_err();
    assert!(err.contains("commitment") || err.contains("directory"), "{err}");
}

#[test]
fn tampering_with_the_directory_is_caught() {
    let k_unwrap = [0x31u8; 32];
    let kappa = [0x32u8; 32];
    let c = build_capsule(&k_unwrap, &[0x33u8; 24], &kappa, b"payload");
    let mut tampered = c.clone();
    tampered.encrypted_directory_and_reservoir[0] ^= 1;
    assert!(open_capsule(&tampered, &k_unwrap, &kappa).is_err());
}

#[test]
fn relocating_the_ciphertext_to_another_phase_fails() {
    // Phase 1 and phase 2 are committed with different associated data; a blob from one must not
    // authenticate as the other. This is what the two separate tags are for.
    let k_unwrap = [0x41u8; 32];
    let kappa = [0x42u8; 32];
    let c = build_capsule(&k_unwrap, &[0x43u8; 24], &kappa, b"payload");
    let mut swapped = c.clone();
    swapped.phase2_cmt4_tag = c.phase1_cmt4_tag;
    assert!(open_capsule(&swapped, &k_unwrap, &kappa).is_err());
}

#[test]
fn derivation_is_reproducible_across_calls() {
    let salt = [0x5Au8; 64];
    let a = derive_root_secret(b"correct horse battery staple", &salt, ParamsClass::Legacy).unwrap();
    let b = derive_root_secret(b"correct horse battery staple", &salt, ParamsClass::Legacy).unwrap();
    assert_eq!(a.as_bytes(), b.as_bytes());
}

#[test]
fn each_save_uses_a_fresh_ephemeral_keypair() {
    // FR-3.2's ratchet, observed: two capsules built with the same K_unwrap and κ must not share
    // a ciphertext, because `create_envelope` generates a new keypair each time.
    let k_unwrap = [0x61u8; 32];
    let kappa = [0x62u8; 32];
    let a = build_capsule(&k_unwrap, &[0x63u8; 24], &kappa, b"payload");
    let b = build_capsule(&k_unwrap, &[0x64u8; 24], &kappa, b"payload");
    assert_ne!(a.ct_ml_kem_1024, b.ct_ml_kem_1024);
    assert_ne!(a.ct_x25519, b.ct_x25519);
    assert_ne!(a.wrapped_stratum_sk, b.wrapped_stratum_sk);
}
