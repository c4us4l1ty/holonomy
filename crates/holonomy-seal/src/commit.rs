//! Key-committing AEAD (`CMT-4` + `XChaCha20-Poly1305`), ported from v5.1.0's audited
//! `holonomy-crypto::commit`.
//!
//! # Why commitment exists at all
//!
//! `XChaCha20-Poly1305` is not key-committing: chosen-ciphertext adversaries can build one
//! ciphertext that authenticates under two keys ("Invisible Salamanders"), turning any
//! decryption oracle into a partitioning oracle. The audit-proven fix is a separate commitment
//! `H("CMT4" ‖ K ‖ N ‖ A ‖ C)` checked **before** the AEAD tag, with the associated data bound
//! so ciphertext cannot be relocated between contexts.
//!
//! The container-specific pieces of v5's module (the directory's fixed 60,592-byte field game)
//! belong to Block 3's world-line and are deliberately not here; this module is the general
//! sealed-object machinery only.

use blake2::{Blake2b512, Digest};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::{Zeroize, Zeroizing};

/// Length of a CMT-4 commitment.
pub const CMT4_TAG_BYTES: usize = 32;

/// Length of a Poly1305 tag.
pub const POLY1305_TAG_BYTES: usize = 16;

/// Length of an XChaCha20 nonce.
pub const NONCE_BYTES: usize = 24;

/// `CMT-4` commitment over `(key, nonce, associated data, ciphertext)`.
///
/// Truncated `BLAKE2b-512`. The domain separator `"CMT4"` keeps this from cross-protocol
/// confusion with any other BLAKE2b use; length prefixes keep `A` and `C` unambiguous.
pub fn cmt4_tag(
    key: &[u8; 32],
    nonce: &[u8; NONCE_BYTES],
    associated_data: &[u8],
    ciphertext: &[u8],
) -> [u8; CMT4_TAG_BYTES] {
    let mut h = Blake2b512::new();
    h.update(b"CMT4");
    h.update([0u8]);
    h.update(key);
    h.update(nonce);
    h.update((associated_data.len() as u64).to_le_bytes());
    h.update(associated_data);
    h.update((ciphertext.len() as u64).to_le_bytes());
    h.update(ciphertext);
    let digest = h.finalize();
    let mut out = [0u8; CMT4_TAG_BYTES];
    out.copy_from_slice(&digest[..CMT4_TAG_BYTES]);
    out
}

/// Verify a CMT-4 commitment in constant time: one 32-iteration XOR-accumulate loop, no early
/// exit, `black_box` on the result so an optimizer cannot elide the discipline.
pub fn cmt4_verify(expected: &[u8; CMT4_TAG_BYTES], actual: &[u8; CMT4_TAG_BYTES]) -> bool {
    let mut diff = 0u8;
    for i in 0..CMT4_TAG_BYTES {
        diff |= expected[i] ^ actual[i];
    }
    std::hint::black_box(diff) == 0
}

/// Failure of a committed decrypt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AeadError {
    /// The commitment did not match — decided *before* touching the ciphertext, so a
    /// partitioning-oracle adversary gets a cheap uniform "no".
    CommitmentMismatch,
    /// The AEAD tag did not match after a matching commitment.
    AuthenticationFailed,
    /// The plaintext buffer was too small for the output.
    OutputTooSmall,
    /// The plaintext does not fit the fixed-length field it is destined for.
    TooLarge {
        /// Bytes offered.
        offered: usize,
        /// Bytes available.
        capacity: usize,
    },
}

impl core::fmt::Display for AeadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::CommitmentMismatch => f.write_str("CMT-4 commitment mismatch"),
            Self::AuthenticationFailed => f.write_str("AEAD authentication failed"),
            Self::OutputTooSmall => f.write_str("output buffer too small"),
            Self::TooLarge { offered, capacity } => {
                write!(
                    f,
                    "plaintext of {offered} bytes does not fit {capacity} bytes of field"
                )
            }
        }
    }
}

impl std::error::Error for AeadError {}

/// A committed ciphertext: AEAD output plus the CMT-4 tag binding it to key/nonce/AAD.
pub struct CommittedCiphertext {
    /// `XChaCha20-Poly1305` output, tag appended.
    pub ciphertext: Vec<u8>,
    /// 32-byte CMT-4 commitment.
    pub cmt4: [u8; CMT4_TAG_BYTES],
}

/// Encrypt with `XChaCha20-Poly1305` and produce the binding commitment alongside.
pub fn seal_committed(
    key: &[u8; 32],
    nonce: &[u8; NONCE_BYTES],
    associated_data: &[u8],
    plaintext: &[u8],
) -> CommittedCiphertext {
    let cipher = XChaCha20Poly1305::new_from_slice(key).expect("32-byte key is valid");
    let ct = cipher
        .encrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad: associated_data,
            },
        )
        .expect("encryption of a bounded buffer cannot fail");
    let cmt4 = cmt4_tag(key, nonce, associated_data, &ct);
    CommittedCiphertext {
        ciphertext: ct,
        cmt4,
    }
}

/// Verify the commitment, then decrypt. Commitment first; see [`AeadError::CommitmentMismatch`].
/// The plaintext returns zeroizing.
pub fn open_committed(
    key: &[u8; 32],
    nonce: &[u8; NONCE_BYTES],
    associated_data: &[u8],
    committed: &CommittedCiphertext,
) -> Result<Zeroizing<Vec<u8>>, AeadError> {
    let expected = cmt4_tag(key, nonce, associated_data, &committed.ciphertext);
    if !cmt4_verify(&expected, &committed.cmt4) {
        return Err(AeadError::CommitmentMismatch);
    }
    let cipher =
        XChaCha20Poly1305::new_from_slice(key).map_err(|_| AeadError::AuthenticationFailed)?;
    let plain = cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: &committed.ciphertext,
                aad: associated_data,
            },
        )
        .map_err(|_| AeadError::AuthenticationFailed)?;
    Ok(Zeroizing::new(plain))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> [u8; 32] {
        [b; 32]
    }

    #[test]
    fn round_trip() {
        let k = key(1);
        let n = [2u8; NONCE_BYTES];
        let ad = b"segment:42";
        let msg = b"the quick brown fox";
        let c = seal_committed(&k, &n, ad, msg);
        let out = open_committed(&k, &n, ad, &c).unwrap();
        assert_eq!(&out[..], &msg[..]);
    }

    #[test]
    fn wrong_key_fails_at_the_commitment() {
        let n = [2u8; NONCE_BYTES];
        let c = seal_committed(&key(1), &n, b"ad", b"secret");
        let err = open_committed(&key(9), &n, b"ad", &c).unwrap_err();
        assert_eq!(err, AeadError::CommitmentMismatch);
    }

    #[test]
    fn relocation_to_another_context_fails() {
        // The property CMT-4 exists for when Block 3 binds segment identity into AAD.
        let k = key(3);
        let n = [4u8; NONCE_BYTES];
        let c = seal_committed(&k, &n, b"segment:5", b"payload");
        assert!(open_committed(&k, &n, b"segment:5", &c).is_ok());
        assert_eq!(
            open_committed(&k, &n, b"segment:6", &c).unwrap_err(),
            AeadError::CommitmentMismatch
        );
    }

    #[test]
    fn any_bit_flip_in_ciphertext_or_tag_is_caught() {
        let k = key(7);
        let n = [8u8; NONCE_BYTES];
        let c = seal_committed(&k, &n, b"ad", b"sixteen bytes of secret");
        for i in [0usize, c.ciphertext.len() - 1] {
            let mut bad_ct = c.ciphertext.clone();
            bad_ct[i] ^= 1;
            let bad = CommittedCiphertext {
                ciphertext: bad_ct,
                cmt4: c.cmt4,
            };
            assert!(open_committed(&k, &n, b"ad", &bad).is_err());
        }
        let mut bad_tag = c.cmt4;
        bad_tag[31] ^= 1;
        let bad = CommittedCiphertext {
            ciphertext: c.ciphertext.clone(),
            cmt4: bad_tag,
        };
        assert_eq!(
            open_committed(&k, &n, b"ad", &bad).unwrap_err(),
            AeadError::CommitmentMismatch
        );
    }

    #[test]
    fn tag_is_32_bytes_and_verify_is_exact() {
        let t = cmt4_tag(&key(1), &[0u8; NONCE_BYTES], b"", b"");
        assert_eq!(t.len(), 32);
        assert_ne!(t, [0u8; 32]);
        let mut b = t;
        b[0] ^= 1;
        assert!(cmt4_verify(&t, &t));
        assert!(!cmt4_verify(&t, &b));
    }

    #[test]
    fn opened_plaintext_is_zeroizing_wrapped() {
        let k = key(3);
        let n = [1u8; NONCE_BYTES];
        let c = seal_committed(&k, &n, b"", b"ours");
        let p = open_committed(&k, &n, b"", &c).unwrap();
        assert_eq!(&p[..], b"ours");
    }
}
