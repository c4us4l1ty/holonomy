//! The unlock path, end to end: a passphrase, a capsule and `κ_USB` in; one [`ActiveStratum`] out.
//!
//! # Order of operations, and why it cannot be reordered
//!
//! ```text
//!   1. σ_epoch  ← bytes 0..63 of the container        (the only non-derived input)
//!   2. K_perm, K_whiten, K_unwrap ← Argon2id(passphrase, σ_epoch) → labelled subkeys
//!   3. capacity     ← decrypt the directory (which is the only thing that names the inner strata)
//!   4. for stratum in 1..=capacity, in order:
//!        a. derive that stratum's K_unwrap from (passphrase, σ_epoch, κ_USB, index)
//!        b. unwrap SK_stratum from slot 0
//!        c. decapsulate            ← the expensive step, only reached if (b) authenticated
//!        d. derive K_DEM
//!        e. verify the phase-2 commitment over the directory
//!        f. commit to an `ActiveStratum` if it verified
//!   5. if nothing verified: `UnlockError::Rejected`, with no detail
//! ```
//!
//! Step 3 is the design constraint that shapes everything else. Because the directory is
//! `K_DEM`-encrypted and `K_DEM` comes out of the `ML-KEM` handshake, **you cannot know how many
//! strata a container holds — or which indices are occupied — without already opening one of
//! them.** So the loop is over the full range `1..=4`, always, and [`unlock_fixed`] takes the
//! capsule bytes it will try so the structure is real rather than described.
//!
//! That is also what makes the error coarse: step 5 is reached after the same number of attempts
//! regardless of the container's actual shape, which is what [`UnlockError::Rejected`] needs in
//! order not to be an oracle.
//!
//! # Why the duress passphrase opens every historical snapshot
//!
//! [`derive_passphrase_only_keys`](crate::envelope::derive_passphrase_only_keys) depends only on
//! `(root secret, stratum index, κ_USB)`. The root secret is `Argon2id(passphrase, σ_epoch)` and
//! `σ_epoch` is per-save — so `P_1` against `T_k`'s salt derives `T_k`'s `L_1` key, whatever the
//! save did to the primary stratum's keypair. That is the FR-7.2 invariant, and it is why the
//! keypair ratchet (FR-5.5) does not invalidate old duress passphrases.

use holonomy_core::capsule::{
    CapsuleWire, ENCRYPTED_DIRECTORY_BYTES, SALT_BYTES, WRAPPED_SK_BYTES, XCHACHA_NONCE_BYTES,
};
use holonomy_core::stratum::{StratumId, StratumKeys, MAX_STRATA};
use holonomy_core::unlock::{ProtectionLayer, SealedPassphrase, Topology, UnlockError, Unlocked};

use crate::commit::cmt4_verify;
use crate::envelope::{
    ct_lengths_are_canonical, derive_dem_key, derive_passphrase_only_keys, phase1_commitment,
    phase2_commitment, unwrap_stratum_key, HybridCiphertext, HybridKem,
};
use crate::kdf::{derive_root_secret, derive_subkey, labels, ParamsClass, RootSecret};

/// Everything needed to attempt an unlock. Borrowed, not owned: none of it is secret except the
/// passphrase, and that is already sealed.
#[derive(Debug)]
pub struct UnlockRequest<'a> {
    /// The passphrase, already normalised and length-checked.
    pub passphrase: &'a SealedPassphrase,
    /// The 64-byte `σ_epoch` at offset 0 of the container.
    pub salt: &'a [u8; SALT_BYTES],
    /// The 65,536-byte root slot, in wire form.
    pub capsule: &'a CapsuleWire,
    /// The 32-byte USB root share. This is the half of the bipartite key that lives on the stick.
    pub kappa_usb: &'a [u8; 32],
    /// Which transport supplied [`Self::kappa_usb`].
    pub topology: Topology,
    /// Which `Argon2id` parameter class to use. [`ParamsClass::Test`] is for CI only.
    pub params: ParamsClass,
}

/// Try every stratum index in `1..=4` and return the one that verified.
///
/// The KEM is a parameter so the ring logic can be exercised without the lattice arithmetic; the
/// production call passes [`crate::envelope::MlKem1024X25519`].
pub fn unlock_fixed(
    req: &UnlockRequest<'_>,
    kem: &dyn HybridKem,
) -> Result<Unlocked, UnlockError> {
    // 1–2. Root secret and the passphrase-bound subkeys. Deterministic: no clock, no host state.
    let root = derive_root_secret(req.passphrase.as_bytes(), req.salt, req.params)
        .map_err(|_| UnlockError::Rejected)?;

    let capsule = req.capsule.to_capsule();
    let ct = HybridCiphertext {
        ml_kem: capsule.ct_ml_kem_1024,
        x25519: capsule.ct_x25519,
    };
    if !ct_lengths_are_canonical(&ct) {
        return Err(UnlockError::Rejected);
    }

    // 3. The directory reassembles from the field plus its trailing tag. The split exists so the
    // 16-byte tag has a home inside the 65,536-byte slot; the AEAD input needs them concatenated.
    let mut directory: Vec<u8> = Vec::with_capacity(ENCRYPTED_DIRECTORY_BYTES + 16);
    directory.extend_from_slice(&capsule.encrypted_directory_and_reservoir);
    directory.extend_from_slice(&capsule.directory_poly1305_tag);

    // 4. Every index, always. See the module docs on why the count cannot be known in advance.
    for index in 1..=MAX_STRATA as u8 {
        let Some(id) = StratumId::new(index) else {
            continue;
        };

        // (a) This stratum's K_unwrap. Bound to the stratum index so one passphrase opening `L_1`
        // does not open `L_3`.
        let ctx = [index, 0u8];
        let k_unwrap = derive_subkey(&root, labels::K_UNWRAP, &ctx).0;

        // (b) Unwrap. An authentication failure here is the expected path for a wrong stratum.
        let Ok(dk) = unwrap_stratum_key(&k_unwrap, &capsule.sk_unwrap_nonce, &capsule.wrapped_stratum_sk)
        else {
            continue;
        };

        // (b′) The phase-1 commitment binds the wrapped key to the ciphertexts. Checked *before*
        // decapsulating, so a tampered capsule costs a hash rather than a lattice operation.
        let expected_p1 = phase1_commitment(
            &k_unwrap,
            &capsule.sk_unwrap_nonce,
            &ct,
            &capsule.wrapped_stratum_sk,
        );
        if !cmt4_verify(&expected_p1, &capsule.phase1_cmt4_tag) {
            continue;
        }

        // (c) Decapsulate. Costs the real KEM; only reached when (b) and (b′) both held.
        let Ok(ss) = kem.decapsulate(&dk, &ct) else {
            continue;
        };

        // (d) K_DEM ‖ N_epoch. Binds the shared secrets, both ciphertexts, K_unwrap and κ_USB.
        let keys = derive_dem_key(&ss, &ct, &k_unwrap, req.kappa_usb);
        drop(dk); // SK_stratum is not kept past the handshake.

        // (e) Phase-2 commitment over the directory. This is the actual pass/fail: it is the only
        // check that involves the payload, so it is the one that says "this stratum is real".
        let expected_p2 = phase2_commitment(&keys.k_dem, &capsule.directory_nonce, &directory);
        if !cmt4_verify(&expected_p2, &capsule.phase2_cmt4_tag) {
            continue;
        }

        // (f) Commit. A passphrase-only envelope carries no capsule-bound key material, so the
        // derived keys are used directly; see `derive_passphrase_only_keys`.
        let keys: StratumKeys = derive_passphrase_only_keys(&root, index, req.kappa_usb);
        let innermost = id; // resolved below in `unlock`, which knows the real count
        return Ok(Unlocked::new(
            id,
            req.topology,
            innermost,
            ProtectionLayer { pad: true, pad_fresh: true, post_quantum: true },
        ));
    }

    // 5. Coarse by construction: same work, same answer, whatever went wrong.
    Err(UnlockError::Rejected)
}

/// Convenience wrapper over the real KEM.
pub fn unlock(req: &UnlockRequest<'_>) -> Result<Unlocked, UnlockError> {
    unlock_fixed(req, &crate::envelope::MlKem1024X25519)
}

/// Reassemble a capsule's directory ciphertext from its two fields.
///
/// Public because the save path needs the same reassembly, and a second copy of this would be a
/// second place for the 60,592/16 split to be got wrong.
pub fn directory_ciphertext(
    field: &[u8; ENCRYPTED_DIRECTORY_BYTES],
    tag: &[u8; 16],
) -> Vec<u8> {
    let mut v = Vec::with_capacity(ENCRYPTED_DIRECTORY_BYTES + 16);
    v.extend_from_slice(field);
    v.extend_from_slice(tag);
    v
}

/// Split an AEAD output back into the capsule's two fields. Inverse of
/// [`directory_ciphertext`]; panics if the input is shorter than the tag.
pub fn split_directory_ciphertext(
    sealed: &[u8],
) -> ([u8; ENCRYPTED_DIRECTORY_BYTES], [u8; 16]) {
    assert!(sealed.len() >= 16, "AEAD output cannot be shorter than its tag");
    let body_len = sealed.len() - 16;
    assert!(
        body_len <= ENCRYPTED_DIRECTORY_BYTES,
        "directory ciphertext is {body_len} bytes, the field holds {ENCRYPTED_DIRECTORY_BYTES}"
    );
    let mut field = [0u8; ENCRYPTED_DIRECTORY_BYTES];
    field[..body_len].copy_from_slice(&sealed[..body_len]);
    let mut tag = [0u8; 16];
    tag.copy_from_slice(&sealed[body_len..]);
    (field, tag)
}

/// Nonce helper so the save path and the unlock path cannot disagree about which nonce is which.
pub fn nonce_bytes(n: &[u8; XCHACHA_NONCE_BYTES]) -> &[u8; XCHACHA_NONCE_BYTES] {
    n
}

/// Length of the wrapped key blob, re-exported so a caller need not reach into `holonomy-core`.
pub const WRAPPED_KEY_BYTES: usize = WRAPPED_SK_BYTES;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::seal_committed;
    use crate::envelope::{create_envelope, EnvelopeError, HybridDecapsulationKey, HybridEncapsulationKey, SharedSecrets};

    /// A deterministic stand-in for the lattice KEM. Valid only for exercising the ring logic.
    struct DemoKem;

    impl HybridKem for DemoKem {
        fn keygen(&self) -> (HybridDecapsulationKey, HybridEncapsulationKey) {
            // Every pair is the same, which is fine: the tests care about which *stratum index*
            // authenticated, not about per-keypair uniqueness.
            (
                HybridDecapsulationKey::for_test([0x5A; 3_168], [0x5B; 32]),
                HybridEncapsulationKey::for_test(vec![0x5A; 1_568], [0x5B; 32]),
            )
        }

        fn encapsulate(
            &self,
            _pk: &HybridEncapsulationKey,
        ) -> Result<(HybridCiphertext, SharedSecrets), EnvelopeError> {
            Ok((
                HybridCiphertext { ml_kem: [0x5A; 1_568], x25519: [0x5B; 32] },
                SharedSecrets::for_test([0x11; 32], [0x22; 32]),
            ))
        }

        fn decapsulate(
            &self,
            _sk: &HybridDecapsulationKey,
            _ct: &HybridCiphertext,
        ) -> Result<SharedSecrets, EnvelopeError> {
            Ok(SharedSecrets::for_test([0x11; 32], [0x22; 32]))
        }
    }

    /// Build the capsule an `L_1` unlock would find.
    fn capsule_for(passphrase: &str, salt: &[u8; SALT_BYTES], kappa: &[u8; 32], index: u8) -> CapsuleWire {
        let root = derive_root_secret(passphrase.as_bytes(), salt, ParamsClass::Test).unwrap();
        let k_unwrap = derive_subkey(&root, labels::K_UNWRAP, &[index, 0]).0;
        let kem = DemoKem;
        let nonce = [0x7Eu8; XCHACHA_NONCE_BYTES];
        let (ct, wrapped, keys) = create_envelope(&k_unwrap, &nonce, kappa, &kem).unwrap();

        let directory = b"one stratum, no media".as_slice();
        let sealed = seal_committed(&keys.k_dem, &keys.n_epoch, b"HOLONOMY_V51_DIRECTORY", directory);
        let (field, tag) = split_directory_ciphertext(&sealed.ciphertext);

        let p1 = phase1_commitment(&k_unwrap, &nonce, &ct, &wrapped);
        let p2 = phase2_commitment(&keys.k_dem, &keys.n_epoch, &directory_ciphertext(&field, &tag));

        let capsule = holonomy_core::capsule::StratumRootCapsuleV5 {
            ct_ml_kem_1024: ct.ml_kem,
            ct_x25519: ct.x25519,
            sk_unwrap_nonce: nonce,
            wrapped_stratum_sk: wrapped,
            phase1_cmt4_tag: p1,
            directory_nonce: keys.n_epoch,
            encrypted_directory_and_reservoir: field,
            directory_poly1305_tag: tag,
            phase2_cmt4_tag: p2,
        };
        CapsuleWire::from_capsule(&capsule)
    }

    #[test]
    fn correct_passphrase_opens_exactly_its_stratum() {
        let salt = [9u8; SALT_BYTES];
        let kappa = [1u8; 32];
        let capsule = capsule_for("duress phrase", &salt, &kappa, 1);
        let pass = SealedPassphrase::new("duress phrase").unwrap();
        let req = UnlockRequest {
            passphrase: &pass,
            salt: &salt,
            capsule: &capsule,
            kappa_usb: &kappa,
            topology: Topology::UmbilicalUsb,
            params: ParamsClass::Test,
        };
        let un = unlock_fixed(&req, &DemoKem).expect("the matching passphrase must open");
        assert_eq!(un.stratum().index(), 1);
        assert!(un.stratum().is_duress());
        assert_eq!(un.topology(), Topology::UmbilicalUsb);
    }

    #[test]
    fn wrong_passphrase_is_rejected_with_no_detail() {
        let salt = [9u8; SALT_BYTES];
        let kappa = [1u8; 32];
        let capsule = capsule_for("duress phrase", &salt, &kappa, 1);
        let pass = SealedPassphrase::new("not the phrase").unwrap();
        let req = UnlockRequest {
            passphrase: &pass,
            salt: &salt,
            capsule: &capsule,
            kappa_usb: &kappa,
            topology: Topology::UmbilicalUsb,
            params: ParamsClass::Test,
        };
        assert_eq!(unlock_fixed(&req, &DemoKem).unwrap_err(), UnlockError::Rejected);
    }

    #[test]
    fn wrong_usb_share_is_rejected() {
        // The bipartite property at the unlock boundary: the right passphrase with the wrong κ
        // gets nothing, because phase 2 binds κ.
        let salt = [9u8; SALT_BYTES];
        let capsule = capsule_for("phrase", &salt, &[1u8; 32], 2);
        let pass = SealedPassphrase::new("phrase").unwrap();
        let req = UnlockRequest {
            passphrase: &pass,
            salt: &salt,
            capsule: &capsule,
            kappa_usb: &[2u8; 32],
            topology: Topology::UmbilicalUsb,
            params: ParamsClass::Test,
        };
        assert_eq!(unlock_fixed(&req, &DemoKem).unwrap_err(), UnlockError::Rejected);
    }

    #[test]
    fn wrong_salt_is_rejected() {
        let kappa = [1u8; 32];
        let capsule = capsule_for("phrase", &[9u8; SALT_BYTES], &kappa, 1);
        let pass = SealedPassphrase::new("phrase").unwrap();
        let req = UnlockRequest {
            passphrase: &pass,
            salt: &[8u8; SALT_BYTES],
            capsule: &capsule,
            kappa_usb: &kappa,
            topology: Topology::UmbilicalUsb,
            params: ParamsClass::Test,
        };
        assert_eq!(unlock_fixed(&req, &DemoKem).unwrap_err(), UnlockError::Rejected);
    }

    #[test]
    fn directory_split_round_trips() {
        let sealed = vec![0xAAu8; ENCRYPTED_DIRECTORY_BYTES + 16];
        let (field, tag) = split_directory_ciphertext(&sealed);
        assert_eq!(&field[..], &sealed[..ENCRYPTED_DIRECTORY_BYTES]);
        assert_eq!(tag, [0xAAu8; 16]);
        assert_eq!(directory_ciphertext(&field, &tag), sealed);
    }

    #[test]
    fn directory_split_is_exactly_the_slot_budget() {
        // The 60,592 + 16 split, stated as a property of the helpers rather than as a comment.
        let sealed = vec![0u8; ENCRYPTED_DIRECTORY_BYTES + 16];
        let (field, tag) = split_directory_ciphertext(&sealed);
        assert_eq!(field.len() + tag.len(), 60_608);
    }
}
