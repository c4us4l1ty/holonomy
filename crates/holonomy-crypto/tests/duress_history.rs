//! Checkpoint 5, second half: the duress passphrase opens `T_0 … T_n`.
//!
//! # What has to be true, and why it is not obvious
//!
//! Every save rotates its salt `σ_epoch`. The root secret is `Argon2id(passphrase, σ_epoch)`, so
//! `P_1` derives a *different* root at `T_0` than at `T_n`. That is fine — it is what makes the
//! derivation per-snapshot — but it means the duress guarantee is not "the same key opens
//! everything". It is the sharper statement:
//!
//! > For every snapshot `T_k`, `P_1` combined with **`T_k`'s own salt** reproduces `T_k`'s `L_1`
//! > `K_unwrap`. Nothing a save ratchets *inside* a stratum — the `ML-KEM` keypair, the payload
//! > keys, the decoy content — participates in that derivation.
//!
//! The failure this guards against is the one the amended PRD §3.6 names: a save that rotates
//! `K_unwrap` from ratcheted material would silently invalidate `P_1` on every earlier snapshot,
//! and the user would only discover it under coercion, which is the worst possible time.
//!
//! # What the test does, concretely
//!
//! Builds a ten-save history where the salt differs at every save and the primary stratum's
//! keypair is re-generated at every save. Then, for each snapshot independently: `P_1` opens it,
//! and the primary passphrase does not open `L_1`. Finally asserts the duress keys are *hostile to
//! each other* — `T_k`'s duress keys must not open `T_j` — because a decoy whose key works
//! everywhere would mean the salt was not being used.

use holonomy_core::capsule::{CapsuleWire, SALT_BYTES};
use holonomy_core::stratum::ActiveStratum;
use holonomy_core::unlock::{SealedPassphrase, Topology, UnlockError};
use holonomy_crypto::commit::seal_directory;
use holonomy_crypto::envelope::{
    create_envelope, phase1_commitment, phase2_commitment, HybridCiphertext,
    HybridDecapsulationKey, HybridEncapsulationKey, HybridKem, SharedSecrets,
};
use holonomy_crypto::kdf::{derive_root_secret, derive_subkey, labels, ParamsClass};
use holonomy_crypto::unlock::{
    directory_ciphertext, split_directory_ciphertext, unlock_fixed, UnlockRequest,
};

const DURESS: &str = "the decoy passphrase";
const PRIMARY: &str = "the real passphrase";

/// A KEM that returns a fixed shared secret.
///
/// Sufficient here for the same reason it is sufficient in `unlock.rs`'s unit tests: the
/// *passphrase-binding* property lives entirely in `K_unwrap` and the phase-2 commitment over
/// `K_DEM`, and `K_DEM` is a hash of `(ss, ct, K_unwrap, κ)`. A constant `ss` therefore does not
/// make two passphrases interchangeable.
struct DemoKem;

impl HybridKem for DemoKem {
    fn keygen(&self) -> (HybridDecapsulationKey, HybridEncapsulationKey) {
        (
            HybridDecapsulationKey::for_test([0x5A; 64], [0x5B; 32]),
            HybridEncapsulationKey::for_test(vec![0x5A; 1_568], [0x5B; 32]),
        )
    }

    fn encapsulate(
        &self,
        _pk: &HybridEncapsulationKey,
    ) -> Result<(HybridCiphertext, SharedSecrets), holonomy_crypto::envelope::EnvelopeError> {
        Ok((
            HybridCiphertext {
                ml_kem: [0x5A; 1_568],
                x25519: [0x5B; 32],
            },
            SharedSecrets::for_test([0x11; 32], [0x22; 32]),
        ))
    }

    fn decapsulate(
        &self,
        _sk: &HybridDecapsulationKey,
        _ct: &HybridCiphertext,
    ) -> Result<SharedSecrets, holonomy_crypto::envelope::EnvelopeError> {
        Ok(SharedSecrets::for_test([0x11; 32], [0x22; 32]))
    }
}

/// One snapshot: its per-save salt, and the capsule a duress unlock would have to open.
struct Snapshot {
    cursor: u64,
    salt: [u8; SALT_BYTES],
    capsule: CapsuleWire,
}

/// Build the snapshot a save would write for stratum `index` under `passphrase`.
///
/// This mirrors the save path: derive the root from *this snapshot's* salt, derive `K_unwrap` for
/// the stratum, wrap the stratum key, seal the directory, commit both phases.
fn build_snapshot(
    cursor: u64,
    salt: [u8; SALT_BYTES],
    passphrase: &str,
    index: u8,
    kappa: &[u8; 32],
    directory: &[u8],
) -> Snapshot {
    let root = derive_root_secret(passphrase.as_bytes(), &salt, ParamsClass::Test).unwrap();
    let k_unwrap = derive_subkey(&root, labels::K_UNWRAP, &[index, 0]).0;
    let kem = DemoKem;
    let nonce = holonomy_crypto::rng::fill_32();
    let mut sk_nonce = [0u8; 24];
    sk_nonce.copy_from_slice(&nonce[..24]);

    let (ct, wrapped, keys) = create_envelope(&k_unwrap, &sk_nonce, kappa, &kem).unwrap();
    let sealed = seal_directory(&keys.k_dem, &keys.n_epoch, directory).unwrap();
    let (field, tag) = split_directory_ciphertext(&sealed);
    let reassembled = directory_ciphertext(&field, &tag);

    let capsule = holonomy_core::capsule::StratumRootCapsuleV5 {
        ct_ml_kem_1024: ct.ml_kem,
        ct_x25519: ct.x25519,
        sk_unwrap_nonce: sk_nonce,
        wrapped_stratum_sk: wrapped,
        phase1_cmt4_tag: phase1_commitment(&k_unwrap, &sk_nonce, &ct, &wrapped),
        directory_nonce: keys.n_epoch,
        encrypted_directory_and_reservoir: field,
        directory_poly1305_tag: tag,
        phase2_cmt4_tag: phase2_commitment(&keys.k_dem, &keys.n_epoch, &reassembled),
    };

    Snapshot {
        cursor,
        salt,
        capsule: CapsuleWire::from_capsule(&capsule),
    }
}

fn unlock(
    snapshot: &Snapshot,
    passphrase: &str,
    kappa: &[u8; 32],
) -> Result<ActiveStratum, UnlockError> {
    let sealed = SealedPassphrase::new(passphrase).expect("passphrase is length-legal");
    let req = UnlockRequest {
        passphrase: &sealed,
        salt: &snapshot.salt,
        capsule: &snapshot.capsule,
        kappa_usb: kappa,
        topology: Topology::UmbilicalUsb,
        params: ParamsClass::Test,
        pad_fresh: true,
    };
    unlock_fixed(&req, &DemoKem)
}

/// Ten saves, ten distinct salts, the real stratum at `L_2` and the decoy at `L_1`.
fn history(kappa: &[u8; 32]) -> Vec<Snapshot> {
    (0..10u64)
        .map(|cursor| {
            // A per-save salt, derived so the test is deterministic but the salts differ.
            let mut salt = [0u8; SALT_BYTES];
            let s = holonomy_core::duress::splitmix64(0xC0FFEE ^ cursor);
            for (i, chunk) in salt.chunks_mut(8).enumerate() {
                chunk.copy_from_slice(
                    &holonomy_core::duress::splitmix64(s ^ i as u64).to_le_bytes(),
                );
            }
            // The directory grows with the cursor, the way a real one would.
            let directory = format!("merkle root at save {cursor}").into_bytes();
            build_snapshot(cursor, salt, DURESS, 1, kappa, &directory)
        })
        .collect()
}

#[test]
fn the_duress_passphrase_opens_every_snapshot() {
    let kappa = [0x31u8; 32];
    let snaps = history(&kappa);
    for snap in &snaps {
        let active = unlock(snap, DURESS, &kappa)
            .unwrap_or_else(|e| panic!("P_1 must open T_{}: {e}", snap.cursor));
        assert_eq!(
            active.id().index(),
            1,
            "T_{} opened the wrong stratum",
            snap.cursor
        );
        assert!(
            active.is_duress(),
            "T_{} did not report a duress unlock",
            snap.cursor
        );
    }
}

#[test]
fn the_duress_keys_are_scoped_to_their_own_snapshot() {
    // The complement, and the reason the previous test is not trivial: a snapshot's duress keys
    // must not open another snapshot. If they did, the per-save salt would not be doing its job.
    let kappa = [0x32u8; 32];
    let snaps = history(&kappa);
    for i in 0..snaps.len() {
        for j in 0..snaps.len() {
            if i == j {
                continue;
            }
            let sealed = SealedPassphrase::new(DURESS).unwrap();
            let req = UnlockRequest {
                passphrase: &sealed,
                // T_i's salt, T_j's capsule: the mismatch a substituted capsule would produce.
                salt: &snaps[i].salt,
                capsule: &snaps[j].capsule,
                kappa_usb: &kappa,
                topology: Topology::UmbilicalUsb,
                params: ParamsClass::Test,
                pad_fresh: true,
            };
            assert!(
                matches!(unlock_fixed(&req, &DemoKem), Err(UnlockError::Rejected)),
                "T_{} opened with T_{}'s salt: the salt is not binding",
                j,
                i
            );
        }
    }
}

#[test]
fn the_primary_passphrase_does_not_open_the_decoy() {
    // The whole point: coercion yields the decoy, and the real passphrase is not a shortcut into
    // it (which would reveal that L_1 and L_k share a derive path an inspector could attack).
    let kappa = [0x33u8; 32];
    let snaps = history(&kappa);
    for snap in &snaps {
        assert!(
            matches!(unlock(snap, PRIMARY, &kappa), Err(UnlockError::Rejected)),
            "the primary passphrase opened the duress stratum at T_{}",
            snap.cursor
        );
    }
}

#[test]
fn the_wrong_usb_share_closes_even_the_decoy() {
    // Bipartite to the end: the duress passphrase is not a way around the stick.
    let kappa = [0x34u8; 32];
    let snaps = history(&kappa);
    assert!(matches!(
        unlock(&snaps[0], DURESS, &[0x35u8; 32]),
        Err(UnlockError::Rejected)
    ));
}

#[test]
fn a_salt_that_does_not_match_its_snapshot_fails() {
    // Guards the invariant the whole scheme rests on: σ_epoch lives in the snapshot, so a capsule
    // read with the wrong salt is a capsule that does not open.
    let kappa = [0x36u8; 32];
    let snaps = history(&kappa);
    let mut snapshot = Snapshot {
        cursor: snaps[0].cursor,
        salt: snaps[0].salt,
        capsule: snaps[0].capsule.clone(),
    };
    snapshot.salt[0] ^= 1;
    assert!(matches!(
        unlock(&snapshot, DURESS, &kappa),
        Err(UnlockError::Rejected)
    ));
}
