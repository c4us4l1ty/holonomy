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

/// Plaintext budget of the directory.
///
/// # Deriving it, rather than picking it
///
/// The capsule allots `ENCRYPTED_DIRECTORY_BYTES = 60,592` bytes to the ciphertext **body** and a
/// separate 16 bytes to the Poly1305 tag, for 60,608 total. `XChaCha20-Poly1305` output is
/// `plaintext + 16`, and that output is what gets split into body-and-tag. So
///
/// ```text
///   plaintext + 16 = 60,608      ⇒      plaintext = 60,592
/// ```
///
/// which is the field size, not the field size minus the tag. Subtracting the tag here as well
/// double-counts it and leaves 16 bytes of the field permanently unused -- a mistake worth
/// recording because the constant is named `..._DIRECTORY_BYTES` and reads like the body length.
/// A serialised directory is a varint count, one entry per stratum and a reservoir descriptor:
/// tens of bytes. The allowance is reservoir-sized.
pub const DIRECTORY_PLAINTEXT_BYTES: usize = 60_592;

/// Version of the directory format, as a `u16` at offset 0 of the plaintext.
///
/// Lives here, beside the field it describes, rather than in the crate that writes directories: an
/// *opener* must reject a format it does not know before it has interpreted anything, and the
/// opener is `unlock`, in this crate. The writer re-exports this rather than defining a second one.
pub const DIRECTORY_VERSION: u16 = 1;

/// Length of a sealed directory: plaintext plus tag. Equal to the capsule's field **plus** its tag
/// field. The assertion below ties this to the numbers the layout uses, so a change in either place
/// fails the build rather than a save.
pub const SEALED_DIRECTORY_BYTES: usize = DIRECTORY_PLAINTEXT_BYTES + POLY1305_TAG_BYTES;

const _: () = assert!(DIRECTORY_PLAINTEXT_BYTES == 60_592);
const _: () = assert!(SEALED_DIRECTORY_BYTES == 60_608);
const _: () = assert!(SEALED_DIRECTORY_BYTES == 60_592 + 16);

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

/// Seal a directory into its **fixed-length** capsule field.
///
/// # Why the length is not "whatever the plaintext needs"
///
/// The capsule allots 60,592 bytes plus a 16-byte tag. If the ciphertext carried only its own
/// plaintext plus tag, two things would break, and both were live bugs:
///
/// 1. **The commitment would not survive a round trip.** `cmt4_tag` hashes
///    `len(ciphertext) ‖ ciphertext`, and the capsule has no length field — so the verifier has to
///    reconstruct the ciphertext from the 60,592-byte field, giving it zero padding on the right.
///    Computed over the short ciphertext and verified over the padded one, the commitment can
///    never match. (Found by `two_phase::unlock_recovers_the_directory`.)
/// 2. **The directory's length would leak.** A 200-byte directory and a 60 KiB one would produce
///    differently sized fields, and the capsule is the one structure an adversary can read without
///    any passphrase. Fixed-length ciphertext under a fixed-length AAD discloses nothing about how
///    many strata a container holds or how large the reservoir is.
///
/// So the plaintext is zero-padded to exactly [`DIRECTORY_PLAINTEXT_BYTES`] *before* encryption,
/// the AAD carries the constant `expected_len`, and the AEAD then refuses a field that was truncated
/// or extended. The padding is inside the authenticated ciphertext.
pub fn seal_directory(
    k_dem: &[u8; 32],
    nonce: &[u8; NONCE_BYTES],
    plaintext: &[u8],
) -> Result<[u8; SEALED_DIRECTORY_BYTES], AeadError> {
    let mut padded = [0u8; DIRECTORY_PLAINTEXT_BYTES];
    if plaintext.len() > DIRECTORY_PLAINTEXT_BYTES {
        return Err(AeadError::TooLarge {
            offered: plaintext.len(),
            capacity: DIRECTORY_PLAINTEXT_BYTES,
        });
    }
    padded[..plaintext.len()].copy_from_slice(plaintext);

    let cipher = XChaCha20Poly1305::new(k_dem.into());
    let aad = directory_aad();
    let out = cipher
        .encrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: &padded,
                aad: &aad,
            },
        )
        .map_err(|_| AeadError::AuthenticationFailed)?;
    assert_eq!(out.len(), SEALED_DIRECTORY_BYTES);

    let mut sealed = [0u8; SEALED_DIRECTORY_BYTES];
    sealed.copy_from_slice(&out);
    Ok(sealed)
}

/// The decrypted directory, as a fixed-size array.
///
/// The length is a property of the type rather than of a runtime check. An earlier version returned
/// a `Vec` and `unlock` compared its length against a constant — which cannot fail, because a
/// fixed-length decrypt produces a fixed-length plaintext. A gate that cannot fail is worse than no
/// gate: it reads like a check in review and does nothing in a test.
pub type DirectoryPlaintext = [u8; DIRECTORY_PLAINTEXT_BYTES];

/// Open a fixed-length directory field back into its plaintext.
///
/// Returns the **whole** [`DIRECTORY_PLAINTEXT_BYTES`], including the zero padding.
///
/// # Why it used to trim, and why that was wrong
///
/// This returned `take_while(|b| b != 0)`, on the reasoning that a caller should not have to know
/// the padding rule. That reasoning holds for an arbitrary byte string and fails for what the
/// directory has since become. A real directory is an *index*, and an index contains zeros in its
/// ordinary course: a `u16` version of `1` is `01 00`, a cursor of `0` is eight zero bytes, a slot
/// count below 256 contributes two more. Trimming at the first zero returned a **one-byte directory
/// for every container this build would write**, and every layer above carried on as if it had a
/// directory.
///
/// The general form is worth naming, because this field has now been the site of three mistakes:
/// *a fixed-length field padded with zeros cannot also be a variable-length field terminated by
/// zeros.* [`seal_directory`] pads to the full length precisely so that an observer cannot learn the
/// document's size, and that padding is exactly what makes a zero terminator meaningless.
///
/// Returns [`DirectoryPlaintext`], so the length is a property of the type.
pub fn open_directory(
    k_dem: &[u8; 32],
    nonce: &[u8; NONCE_BYTES],
    sealed: &[u8; SEALED_DIRECTORY_BYTES],
) -> Result<DirectoryPlaintext, AeadError> {
    let cipher = XChaCha20Poly1305::new(k_dem.into());
    let aad = directory_aad();
    let plain = cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: sealed,
                aad: &aad,
            },
        )
        .map_err(|_| AeadError::AuthenticationFailed)?;
    // An `XChaCha20-Poly1305` decrypt of `SEALED_DIRECTORY_BYTES` yields exactly the plaintext
    // length, so this conversion cannot shorten anything. `try_into` rather than `copy_from_slice`
    // so that if it ever *could*, this is an error rather than a silent truncation.
    plain
        .try_into()
        .map_err(|_| AeadError::AuthenticationFailed)
}

/// AAD for the directory field: constant, and equal to the plaintext length.
///
/// Binding the expected length is what makes a truncated-or-extended field a decryption failure
/// rather than a different plaintext.
fn directory_aad() -> Vec<u8> {
    let mut aad = Vec::with_capacity(32);
    aad.extend_from_slice(b"HOLONOMY_V51_DIRECTORY");
    aad.extend_from_slice(&(DIRECTORY_PLAINTEXT_BYTES as u64).to_le_bytes());
    aad
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
    /// The plaintext does not fit the fixed-length field it is destined for.
    TooLarge {
        /// Bytes offered.
        offered: usize,
        /// Bytes available.
        capacity: usize,
    },
}

impl std::fmt::Display for AeadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

    /// Whether the ciphertext is empty (only possible for an empty plaintext).
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
    let cipher =
        XChaCha20Poly1305::new_from_slice(key).map_err(|_| AeadError::AuthenticationFailed)?;
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
pub fn slot_nonce(
    base: &[u8; NONCE_BYTES],
    save_cursor: u64,
    slot_index: u32,
    stratum: u8,
) -> [u8; NONCE_BYTES] {
    let mut n = *base;
    // XOR the counter fields into the tail 16 bytes of the nonce; the first 8 are the base's own.
    let counter = save_cursor.wrapping_shl(32).wrapping_add(slot_index as u64);
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
mod directory_tests {
    use super::*;

    const K: [u8; 32] = [0x11u8; 32];
    const N: [u8; NONCE_BYTES] = [0x22u8; NONCE_BYTES];

    #[test]
    fn a_short_directory_still_produces_the_full_field() {
        let sealed = seal_directory(&K, &N, b"one stratum").unwrap();
        // 60,608 splits into the capsule's 60,592-byte body field and its 16-byte tag field.
        assert_eq!(sealed.len(), SEALED_DIRECTORY_BYTES);
        assert_eq!(sealed.len(), 60_592 + 16);
        let plain = open_directory(&K, &N, &sealed).unwrap();
        assert_eq!(plain.len(), DIRECTORY_PLAINTEXT_BYTES);
        assert_eq!(&plain[..b"one stratum".len()], b"one stratum");
        assert!(plain[b"one stratum".len()..].iter().all(|&b| b == 0));
    }

    #[test]
    fn directory_plaintext_is_returned_whole() {
        // The regression this exists for. `open_directory` used to `take_while(|b| b != 0)`, which
        // returns a *one-byte* directory for any real index because a little-endian version of 1 has
        // a zero for its second byte. The assertion is on length, not content: content was never
        // the problem.
        let index = [0x01u8, 0x00, 0x02, 0x00, 0x2Au8, 0x00];
        let sealed = seal_directory(&K, &N, &index).unwrap();
        let plain = open_directory(&K, &N, &sealed).unwrap();
        assert_eq!(plain.len(), DIRECTORY_PLAINTEXT_BYTES);
        assert_eq!(&plain[..index.len()], &index);
        assert_ne!(plain.len(), 1, "the old trimming behaviour");
    }

    #[test]
    fn the_plaintext_budget_is_the_field_size_not_the_field_minus_the_tag() {
        // The off-by-16 this constant used to have. Stated as a test so it cannot come back.
        assert_eq!(DIRECTORY_PLAINTEXT_BYTES, 60_592);
        assert_ne!(DIRECTORY_PLAINTEXT_BYTES, 60_592 - POLY1305_TAG_BYTES);
        assert!(seal_directory(&K, &N, &vec![0u8; DIRECTORY_PLAINTEXT_BYTES]).is_ok());
    }

    #[test]
    fn an_empty_directory_is_legal_and_round_trips() {
        let sealed = seal_directory(&K, &N, b"").unwrap();
        let plain = open_directory(&K, &N, &sealed).unwrap();
        assert_eq!(plain.len(), DIRECTORY_PLAINTEXT_BYTES);
        assert!(plain.iter().all(|&b| b == 0));
    }

    #[test]
    fn the_ciphertext_length_does_not_depend_on_the_plaintext_length() {
        // The confidentiality property: an observer with no key learns nothing about how many
        // strata exist from the capsule's shape.
        let a = seal_directory(&K, &N, b"x").unwrap();
        let b = seal_directory(&K, &N, &vec![b'y'; 60_000]).unwrap();
        assert_eq!(a.len(), b.len());
    }

    #[test]
    fn an_oversized_directory_is_refused_rather_than_truncated() {
        let too_big = vec![0u8; DIRECTORY_PLAINTEXT_BYTES + 1];
        assert!(matches!(
            seal_directory(&K, &N, &too_big),
            Err(AeadError::TooLarge { .. })
        ));
    }

    #[test]
    fn flipping_any_byte_of_the_field_fails_to_open() {
        let sealed = seal_directory(&K, &N, b"directories are small").unwrap();
        for i in [0usize, 1, 15, 60_590, 60_591] {
            let mut bad = sealed;
            bad[i] ^= 1;
            assert!(
                open_directory(&K, &N, &bad).is_err(),
                "byte {i} went undetected"
            );
        }
    }

    #[test]
    fn a_different_key_or_nonce_fails_to_open() {
        let sealed = seal_directory(&K, &N, b"x").unwrap();
        assert!(open_directory(&[0x99u8; 32], &N, &sealed).is_err());
        let mut other_nonce = N;
        other_nonce[0] ^= 1;
        assert!(open_directory(&K, &other_nonce, &sealed).is_err());
    }
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
        assert_eq!(
            open_committed(&k, &n, &l2, &c).unwrap_err(),
            AeadError::CommitmentMismatch
        );
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
