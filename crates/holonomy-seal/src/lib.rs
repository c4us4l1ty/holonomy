//! The Seal — the blessed cryptographic boundary of Holonomy v6 (ROADMAP Block 2).
//!
//! All primitives — Argon2id, BLAKE2b, XChaCha20-Poly1305, CMT-4, ML-KEM-1024, X25519 — live
//! behind this crate and nowhere else. What other crates receive are **capabilities**, not
//! keys: a [`DEMKey`] can seal and open any document object, and no other workspace crate can
//! read its bytes. Only this crate's sealed operations expose them internally.
//!
//! ```text
//!   passphrase (NFC bytes) ─┐
//!   optional USB slot       ├─> MasterKeys::derive ─> UnwrapKey
//!   epoch salt (Block 3)    ┘         │
//!                                     ▼
//!                        create_capsule / open_capsule  (two-phase PQ hybrid)
//!                                     │
//!                                     ▼
//!                                  DEMKey ──> seal_object / open_object  (CMT-4 + XChaCha20)
//! ```
//!
//! Design lineage: two-phase non-circular envelope ported from v5.1.0's audited
//! `holonomy-crypto`; pad/strata/container concerns removed per RFC-001 (they belong to the
//! world-line or to the deferred list). The USB "extra deadbolt" is exactly one optional
//! argument, on exactly one code path, at exactly two hash inputs (KDF context + DEM bind).

#[cfg(test)]
mod allocation_audit;
mod capability;
mod commit;
mod envelope;
mod kdf;
mod rng;

mod calibrate;
mod error;
mod lock;
mod passphrase;
mod reaper;
mod secret;
pub mod v5;

#[cfg(test)]
#[global_allocator]
static ZEROIZE_AUDIT_ALLOCATOR: allocation_audit::AuditAllocator = allocation_audit::AuditAllocator;

use blake2::digest::consts::U32;
#[cfg(test)]
use blake2::Blake2b512;
use blake2::{Blake2b, Digest};
// Direct feature-unification edges keep the pinned transitive MAC and ML-KEM sponge state
// wipe-on-drop features enabled; their upstream parent crates do not request all of them.
use poly1305 as _;
use sha3 as _;
use shake as _;

type Blake2b256 = Blake2b<U32>;
use commit::{open_committed, seal_committed, CommittedCiphertext};
use envelope::MlKem1024X25519;
use zeroize::Zeroizing;

pub use calibrate::{
    calibrate, select_params, Calibration, KdfParamsRecord, CALIBRATION_P, CALIBRATION_T,
    FENCE_MAX_KIB, FENCE_MIN_KIB, TARGET_BAND_MS,
};
pub use capability::{DEMKey, UnwrapKey};
// The KEM trait and concrete implementation stay crate-private: their internal return path
// includes raw shared secrets. Downstream crates get only the opaque capsule/key operations.
pub use envelope::{Capsule, EnvelopeError, HybridCiphertext};
pub use error::SealError;
pub use kdf::{MasterKeys, ParamsClass, DERIVE_SALT_BYTES};
pub use lock::{describe_how, probe_capability, LockCapability, LockReport};
pub use passphrase::{
    estimate_passphrase_strength, PassphraseError, PassphraseInput, PassphraseStrength,
    StrengthTier, MAX_PASSPHRASE_BYTES, MAX_PASSPHRASE_INPUT_BYTES,
};
pub use reaper::{
    ArenaSlot, CanaryReport, CanarySet, ReaperGuard, ReaperOutcome, SchedClass, SecretArenas,
    TripReason, Tripwire, WipeStats, MAX_ARENAS,
};
pub use v5::{
    open_v5_streaming, scan_v5_cursors, V5Error, V5OpenReport, V5PadSource, V5Passphrase,
    V5_CONTAINER_BYTES, V5_PAYLOAD_PLAINTEXT_PER_SLOT, V5_SAVES_BEFORE_DOMAIN_REUSE,
    V5_ZONE_B_LBA_START,
};

/// The boundary law, carried in code so it is compiled, not just quoted.
pub const BOUNDARY_LAW: &str =
    "key material crosses one boundary, held only behind capability tokens";

/// Draw a fresh per-container identifier from the OS CSPRNG.
pub fn generate_container_id() -> Result<[u8; 16], SealError> {
    let mut id = [0u8; 16];
    rng::fill(&mut id).map_err(|_| SealError::EntropyUnavailable)?;
    Ok(id)
}

/// Draw a fresh Argon2 salt from the OS CSPRNG.
pub fn generate_kdf_salt() -> Result<[u8; DERIVE_SALT_BYTES], SealError> {
    let mut salt = [0u8; DERIVE_SALT_BYTES];
    rng::fill(&mut salt).map_err(|_| SealError::EntropyUnavailable)?;
    Ok(salt)
}

/// One-shot proof that the user re-entered the passphrase for an explicit history compaction.
///
/// It is scoped to one file identifier and cannot be constructed outside this crate. It carries
/// no key material; the fresh derivation used to create it is zeroized at the seal boundary.
#[must_use = "consume the confirmation token in an explicit compact operation"]
pub struct CompactionApproval {
    file_id: [u8; 16],
}

impl CompactionApproval {
    /// Whether this one-shot approval was created for the presented file identifier.
    pub fn is_for_file(&self, file_id: &[u8; 16]) -> bool {
        &self.file_id == file_id
    }
}

/// Re-derive the file's unwrap key from a freshly entered passphrase and mint a one-shot,
/// file-scoped compaction approval only if it matches the currently unlocked capability.
pub fn confirm_compaction(
    passphrase: &[u8],
    salt: &[u8; DERIVE_SALT_BYTES],
    usb: Option<&[u8; 32]>,
    params: &KdfParamsRecord,
    expected_unwrap: &UnwrapKey,
    file_id: &[u8; 16],
) -> Result<CompactionApproval, SealError> {
    let rederived = MasterKeys::derive_with(passphrase, salt, usb, params)?;
    let actual = rederived.unwrap_key().expose();
    let expected = expected_unwrap.expose();
    let mut difference = 0u8;
    for (&actual_byte, &expected_byte) in actual.iter().zip(expected.iter()) {
        difference |= actual_byte ^ expected_byte;
    }
    if std::hint::black_box(difference) != 0 {
        return Err(SealError::OpenRejected);
    }
    Ok(CompactionApproval { file_id: *file_id })
}

/// A public, domain-separated BLAKE2b-256 digest for authenticated format metadata.
///
/// This is not a key derivation or a MAC. It is the hashing hook the world-line uses for its
/// public Merkle accumulator; callers must still authenticate roots with sealed witnesses.
pub fn metadata_digest(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut h = Blake2b256::new();
    h.update(b"HOLONOMY_V6_PUBLIC_METADATA");
    h.update([0u8]);
    h.update((domain.len() as u64).to_le_bytes());
    h.update(domain);
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
    let digest = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// The zeroizing plaintext returned by a successful object-open operation.
pub type OpenedPlaintext = Zeroizing<Vec<u8>>;

/// A sealed document object: ciphertext, its nonce, and the CMT-4 commitment binding both to
/// the document key and the associated data. The world-line stores these verbatim.
pub struct SealedObject {
    /// Per-object random nonce (XChaCha20 tolerates random nonces at this volume).
    pub nonce: [u8; 24],
    /// The committed ciphertext (AEAD output plus tag).
    pub ciphertext: Vec<u8>,
    /// CMT-4 commitment over key/nonce/AAD/ciphertext.
    pub cmt4: [u8; 32],
}

/// Seal a plaintext object under the document key, with `associated_data` binding its identity
/// (Block 3 will pass segment/window identities here; nothing may be passed that is not part
/// of what must authenticate).
pub fn seal_object(key: &DEMKey, associated_data: &[u8], plaintext: &[u8]) -> SealedObject {
    let nonce = rng::fill_24();
    let committed = seal_committed(key.expose(), &nonce, associated_data, plaintext);
    SealedObject {
        nonce,
        ciphertext: committed.ciphertext,
        cmt4: committed.cmt4,
    }
}

/// Open a sealed object: commitment first, then AEAD; plaintext returns zeroizing.
pub fn open_object(
    key: &DEMKey,
    associated_data: &[u8],
    sealed: &SealedObject,
) -> Result<OpenedPlaintext, SealError> {
    let committed = CommittedCiphertext {
        ciphertext: sealed.ciphertext.clone(),
        cmt4: sealed.cmt4,
    };
    Ok(open_committed(
        key.expose(),
        &sealed.nonce,
        associated_data,
        &committed,
    )?)
}

/// Create a capsule (the save path): the envelope ceremony with the real hybrid KEM.
pub fn create_capsule_for(
    unwrap: &UnwrapKey,
    usb: Option<&[u8; 32]>,
) -> Result<(Capsule, DEMKey), SealError> {
    Ok(envelope::create_capsule(&MlKem1024X25519, unwrap, usb)?)
}

/// Open a capsule (the unlock path): wrong passphrase or wrong slot state is one uniform no.
pub fn open_capsule_for(
    unwrap: &UnwrapKey,
    capsule: &Capsule,
    usb: Option<&[u8; 32]>,
) -> Result<DEMKey, SealError> {
    Ok(envelope::open_capsule(
        &MlKem1024X25519,
        unwrap,
        capsule,
        usb,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_law_is_written() {
        assert!(!BOUNDARY_LAW.is_empty());
    }

    #[test]
    fn object_round_trip_through_capabilities_only() {
        let keys = MasterKeys::derive(b"pw", &[1u8; 64], None, ParamsClass::Test).unwrap();
        let unwrap = keys_test_hook(&keys);
        let (capsule, dem) = create_capsule_for(unwrap, None).unwrap();
        let reopened = open_capsule_for(unwrap, &capsule, None).unwrap();
        let sealed = seal_object(&reopened, b"segment:0", b"hello world-line");
        let plain = open_object(&dem, b"segment:0", &sealed).unwrap();
        assert_eq!(&plain[..], b"hello world-line");
        // Relocation fails at the commitment, through the public surface.
        assert!(open_object(&dem, b"segment:1", &sealed).is_err());
    }

    #[test]
    fn registered_secret_heap_buffers_are_zeroized_before_free() {
        let before = allocation_audit::snapshot();
        {
            let passphrase =
                PassphraseInput::new("allocator audit passphrase, not user data").unwrap();
            let oversize = "x".repeat(MAX_PASSPHRASE_BYTES + 1);
            assert!(matches!(
                PassphraseInput::new(&oversize),
                Err(PassphraseError::TooLong { .. })
            ));

            let salt = [0x52; DERIVE_SALT_BYTES];
            let usb = [0xD7; 32];
            let keys = passphrase
                .with_bytes(|bytes| MasterKeys::derive(bytes, &salt, Some(&usb), ParamsClass::Test))
                .unwrap();
            let (capsule, dem) = create_capsule_for(keys.unwrap_key(), Some(&usb)).unwrap();
            let reopened = open_capsule_for(keys.unwrap_key(), &capsule, Some(&usb)).unwrap();

            let message = [0xA6; 128];
            let sealed = seal_object(&dem, b"allocation-audit", &message);
            let plaintext = open_object(&reopened, b"allocation-audit", &sealed).unwrap();
            assert_eq!(&plaintext[..], &message);
        }
        let after = allocation_audit::snapshot();

        assert!(after.allocations > before.allocations);
        assert!(after.deallocations > before.deallocations);
        assert!(after.registered >= before.registered + 6);
        assert!(after.zeroed_before_free > before.zeroed_before_free);
        assert_eq!(after.nonzero_before_free, before.nonzero_before_free);
        assert_eq!(after.registry_overflow, before.registry_overflow);
        assert_eq!(after.secret_reallocations, before.secret_reallocations);
    }

    #[test]
    fn metadata_digest_is_blake2b_256_not_truncated_blake2b_512() {
        let digest = metadata_digest(b"test-domain", b"holonomy metadata");
        assert_eq!(
            digest,
            [
                0x77, 0x46, 0xe1, 0xee, 0x5c, 0x9d, 0x01, 0x3c, 0xee, 0xea, 0xa3, 0x11, 0xba, 0x8c,
                0xb8, 0x08, 0x9f, 0xbf, 0xe5, 0xbf, 0x6e, 0x45, 0x44, 0x7e, 0x5a, 0x94, 0xec, 0x50,
                0x4a, 0x18, 0x7f, 0xe0,
            ]
        );
        assert_ne!(digest, {
            let mut hasher = Blake2b512::new();
            hasher.update(b"HOLONOMY_V6_PUBLIC_METADATA");
            hasher.update([0u8]);
            hasher.update(11u64.to_le_bytes());
            hasher.update(b"test-domain");
            hasher.update(17u64.to_le_bytes());
            hasher.update(b"holonomy metadata");
            let wide = hasher.finalize();
            let mut truncated = [0u8; 32];
            truncated.copy_from_slice(&wide[..32]);
            truncated
        });
    }

    #[test]
    fn compaction_confirmation_rederives_and_is_file_scoped() {
        let salt = [0xA2; DERIVE_SALT_BYTES];
        let params = KdfParamsRecord {
            m_kib: 8_192,
            t: 1,
            p: 1,
        };
        let keys = MasterKeys::derive(b"confirm me", &salt, None, ParamsClass::Test).unwrap();
        let file_id = [0x61; 16];
        let approval = confirm_compaction(
            b"confirm me",
            &salt,
            None,
            &params,
            keys.unwrap_key(),
            &file_id,
        )
        .unwrap();
        assert!(approval.is_for_file(&file_id));
        assert!(!approval.is_for_file(&[0x62; 16]));
        assert!(matches!(
            confirm_compaction(b"wrong", &salt, None, &params, keys.unwrap_key(), &file_id,),
            Err(SealError::OpenRejected)
        ));
    }

    #[test]
    fn wrong_passphrase_never_opens_the_real_capsule() {
        let keys = MasterKeys::derive(b"pw", &[1u8; 64], None, ParamsClass::Test).unwrap();
        let bad_keys = MasterKeys::derive(b"nope", &[1u8; 64], None, ParamsClass::Test).unwrap();
        let (capsule, _) = create_capsule_for(keys_test_hook(&keys), None).unwrap();
        assert!(open_capsule_for(keys_test_hook(&bad_keys), &capsule, None).is_err());
    }

    fn keys_test_hook(keys: &MasterKeys) -> &UnwrapKey {
        keys.unwrap_key()
    }
}
