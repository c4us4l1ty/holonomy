//! The Seal — the blessed cryptographic boundary of Holonomy v6 (ROADMAP Block 2).
//!
//! All primitives — Argon2id, BLAKE2b, XChaCha20-Poly1305, CMT-4, ML-KEM-1024, X25519 — live
//! behind this crate and nowhere else. What other crates receive are **capabilities**, not
//! keys: a [`DEMKey`] can seal and open any document object, and no code anywhere in the
//! workspace can read its bytes.
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

mod capability;
mod commit;
mod envelope;
mod kdf;
mod rng;

pub use capability::{DEMKey, UnwrapKey};
pub use envelope::{Capsule, EnvelopeError, HybridCiphertext, HybridKem, MlKem1024X25519};
pub use error::SealError;
pub use kdf::{MasterKeys, ParamsClass, DERIVE_SALT_BYTES};

/// The boundary law, carried in code so it is compiled, not just quoted.
pub const BOUNDARY_LAW: &str =
    "key material crosses one boundary, held only behind capability tokens";

mod error;

use commit::{open_committed, seal_committed, CommittedCiphertext};
use zeroize::Zeroizing;

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
) -> Result<Zeroizing<Vec<u8>>, SealError> {
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
    Ok(envelope::open_capsule(&MlKem1024X25519, unwrap, capsule, usb)?)
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
    fn wrong_passphrase_never_opens_the_real_capsule() {
        let keys = MasterKeys::derive(b"pw", &[1u8; 64], None, ParamsClass::Test).unwrap();
        let bad_keys =
            MasterKeys::derive(b"nope", &[1u8; 64], None, ParamsClass::Test).unwrap();
        let (capsule, _) = create_capsule_for(keys_test_hook(&keys), None).unwrap();
        assert!(open_capsule_for(keys_test_hook(&bad_keys), &capsule, None).is_err());
    }

    fn keys_test_hook(keys: &MasterKeys) -> &UnwrapKey {
        keys.unwrap_key()
    }
}
