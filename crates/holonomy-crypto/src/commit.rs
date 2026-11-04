//! `CMT-4` key commitment, and the AEAD wrapper that uses it.
//!
//! # Why commitment is needed at all
//!
//! `XChaCha20-Poly1305` is *not* key-committing. An adversary who can choose the ciphertext can
//! often construct one that authenticates under two different keys ("Invisible Salamanders"),
//! which turns a single decryption oracle into a partitioning oracle across a key space. The fix
//! is to bind the key to the ciphertext with a separate commitment that is checked before — or
//! independently of — the AEAD tag.
//!
//! [`crate::commit::cmt4_tag`] is the standard construction from the key-commitment literature:
//! `H("CMT4" ‖ K ‖ N ‖ A ‖ C)`, truncated to 256 bits. Adding the associated data `A` (which
//! carries the slot index and stratum id) is what stops a ciphertext from being moved between
//! slots: the commitment covers *where* it lives, so slot-swapping fails the commitment even if
//! the AEAD would accept it.
//!
//! # What is deliberately not here
//!
//! The old PRD's §7.1 sketch kept two CMT-4 tags per capsule — one over phase 1 and one over phase
//! 2 — plus a 16-byte Poly1305 tag, and the resulting struct overflowed its slot by 16 bytes. Two
//! commitments are still correct in principle (the two phases are unwrapped separately), so
//! v5.1.0 keeps both *fields* but derives them from one helper, and [`CapsuleWire`] is the only
//! thing that decides how they fit. The size assertion in `holonomy-core` is what keeps them
//! fitting.
//!
//! [`CapsuleWire`]: holonomy_core::capsule::CapsuleWire

use blake2::{Blake2b512, Digest};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::Zeroize;

/// Length of a CMT-4 commitment.
pub const CMT4_TAG_BYTES: usize = 32;

/// Length of a Poly1305 tag.
pub const POLY1305_TAG_BYTES: usize = 16;

/// Length of an XChaCha20 nonce.
pub const NONCE_BYTES: usize = 24;

/// `CMT-4` commitment over `(key, nonce, associated data, ciphertext)`.
///
/// Truncated `BLAKE2b-512`. The domain string `"CMT4"` is what keeps this from being confused with
/// any other use of BLAKE2b in the system; the length prefixes keep `A` and `C` unambiguous.
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

/// Verify a CMT-4 commitment in constant time.
///
/// `subtle`-style comparison is written out rather than pulled in as a dependency, because one
/// `|`-accumulated XOR loop is the whole of it and a dependency for this would be noise.
pub fn cmt4_verify(expected: &[u8; CMT4_TAG_BYTES], actual: &[u8; CMT4_TAG_BYTES]) -> bool {
    let mut diff = 0u8;
    for i in 0..CMT4_TAG_BYTES {
        diff |= expected[i] ^ actual[i];
    }
    // No early return anywhere above: the loop always runs 32 times.
    core::hint::black_box(diff) == 0
}

/// Alternative spelling used by the capsule layout doc, kept so call sites read like the spec.
pub fn verify_cmt4(expected: &[u8; CMT4_TAG_BYTES], actual: &[u8; CMT4_TAG_BYTES]) -> bool {
    cmt4_verify(expected, actual)
}

/// Associated data for a slot's commitment and AEAD. Binds stratum id and slot index, so a
/// ciphertext cannot be relocated within the container.
pub fn slot_associated_data(stratum: u8, slot_index: u32) -> [u8; 8] {
    let mut ad = [0u8; 8];
    ad[0] = stratum;
    ad[1..5].copy_from_slice(&slot_index.to_le_bytes());
    ad
}

/// Failure of a committed decrypt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AeadError {
    /// The commitment did not match. Decided *before* touching the ciphertext.
    CommitmentMismatch,
    /// The AEAD tag did not match.
    AuthenticationFailed,
    /// The plaintext buffer was too small for the output.
    OutputTooSmall,
}

impl std::fmt::Display for AeadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CommitmentMismatch => f.write_str("CMT-4 commitment mismatch"),
            Self::AuthenticationFailed => f.write_str("AEAD authentication failed"),
            Self::OutputTooSmall => f.write_str("output buffer too small"),
        }
    }
}

impl std::error::Error for AeadError {}

/// A committed ciphertext: the AEAD output plus the CMT-4 tag that binds it.
pub struct CommittedCiphertext {
    /// `XChaCha20-Poly1305` output, tag appended.
    pub ciphertext: Vec<u8>,
    /// 32-byte CMT-4 commitment.
    pub cmt4: [u8; CMT4_TAG_BYTES],
}

impl CommittedCiphertext {
    /// Length of the ciphertext, excluding the commitment (which travels beside it in the capsule).
    pub fn len(&self) -> usize {
        self.ciphertext.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ciphertext.is_empty()
    }
}

/// Encrypt with `XChaCha20-Poly1305` and produce the CMT-4 commitment alongside.
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

/// Verify the commitment, then decrypt.
///
/// Commitment first, deliberately: checking it before the AEAD means a partitioning-oracle
/// attacker gets a single "no" from a cheap hash rather than an oracle that behaves differently
/// for a valid key with a bad tag.
pub fn open_committed(
    key: &[u8; 32],
    nonce: &[u8; NONCE_BYTES],
    associated_data: &[u8],
    committed: &CommittedCiphertext,
) -> Result<Vec<u8>, AeadError> {
    let expected = cmt4_tag(key, nonce, associated_data, &committed.ciphertext);
    if !cmt4_verify(&expected, &committed.cmt4) {
        return Err(AeadError::CommitmentMismatch);
    }
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| AeadError::AuthenticationFailed)?;
    cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: &committed.ciphertext,
                aad: associated_data,
            },
        )
        .map_err(|_| AeadError::AuthenticationFailed)
}

/// A nonce sequence that cannot repeat, plus the ability to be reconstructed deterministically.
///
/// Counters rather than random nonces: with a per-save `N_epoch` base and a counter for the slot,
/// a nonce collision under one `K_DEM` requires 2^24 slots within one save, and a container has
/// 2,047. It also means a save is reproducible from `(K_DEM, N_epoch, cursor)`, which is the
/// determinism rule again.
pub fn slot_nonce(base: &[u8; NONCE_BYTES], save_cursor: u64, slot_index: u32, stratum: u8) -> [u8; NONCE_BYTES] {
    let mut n = *base;
    // XOR the counter fields into the tail 16 bytes of the nonce; the first 8 are the base's own.
    let counter = save_cursor
        .wrapping_shl(32)
        .wrapping_add(slot_index as u64);
    let mut tail = [0u8; 16];
    tail[0..8].copy_from_slice(&counter.to_le_bytes());
    tail[8] = stratum;
    for i in 0..16 {
        n[8 + i] ^= tail[i];
    }
    n
}

/// Scrub a buffer. Wraps `zeroize` so call sites cannot forget the compiler fence.
pub fn scrub(buf: &mut [u8]) {
    buf.zeroize();
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
        let ad = slot_associated_data(2, 17);
        let msg = b"the quick brown fox";
        let c = seal_committed(&k, &n, &ad, msg);
        let out = open_committed(&k, &n, &ad, &c).unwrap();
        assert_eq!(out, msg);
    }

    #[test]
    fn wrong_key_fails_at_the_commitment() {
        let n = [2u8; NONCE_BYTES];
        let ad = slot_associated_data(1, 0);
        let c = seal_committed(&key(1), &n, &ad, b"secret");
        let err = open_committed(&key(9), &n, &ad, &c).unwrap_err();
        assert_eq!(err, AeadError::CommitmentMismatch);
    }

    #[test]
    fn relocation_to_another_slot_fails() {
        // The property CMT-4 exists for: a valid ciphertext cannot be moved.
        let k = key(3);
        let n = [4u8; NONCE_BYTES];
        let src = slot_associated_data(2, 5);
        let dst = slot_associated_data(2, 6);
        let c = seal_committed(&k, &n, &src, b"payload");
        assert!(open_committed(&k, &n, &src, &c).is_ok());
        assert_eq!(
            open_committed(&k, &n, &dst, &c).unwrap_err(),
            AeadError::CommitmentMismatch
        );
    }

    #[test]
    fn stratum_is_bound_too() {
        // Same slot index, different stratum: the commitment must not transfer, or a duress save
        // could overwrite a primary slot with a ciphertext the primary stratum's key would accept.
        let k = key(3);
        let n = [4u8; NONCE_BYTES];
        let l1 = slot_associated_data(1, 0);
        let l2 = slot_associated_data(2, 0);
        let c = seal_committed(&k, &n, &l1, b"payload");
        assert_eq!(open_committed(&k, &n, &l2, &c).unwrap_err(), AeadError::CommitmentMismatch);
    }

    #[test]
    fn tag_is_32_bytes_and_not_all_zero() {
        let t = cmt4_tag(&key(1), &[0u8; NONCE_BYTES], b"", b"");
        assert_eq!(t.len(), 32);
        assert_ne!(t, [0u8; 32]);
    }

    #[test]
    fn any_bit_flip_in_the_tag_is_caught() {
        let a = cmt4_tag(&key(1), &[0u8; NONCE_BYTES], b"ad", b"ct");
        let mut b = a;
        b[31] ^= 1;
        assert!(!cmt4_verify(&a, &b));
        assert!(cmt4_verify(&a, &a));
    }

    #[test]
    fn nonces_are_unique_across_the_whole_plausible_history() {
        // 2,047 slots × 54 saves is the entire pad budget; no two nonces may collide.
        let base = [0xABu8; NONCE_BYTES];
        let mut seen = std::collections::HashSet::new();
        for save in 0..54u64 {
            for slot in 0..2_047u32 {
                let n = slot_nonce(&base, save, slot, 2);
                assert!(seen.insert(n), "nonce collision at save {save} slot {slot}");
            }
        }
        assert_eq!(seen.len(), 54 * 2_047);
    }

    #[test]
    fn scrub_zeroes() {
        let mut b = b"plaintext".to_vec();
        scrub(&mut b);
        assert!(b.iter().all(|&x| x == 0));
    }
}
