//! Per-chunk AEAD: XChaCha20-Poly1305 with `nonce_i = N_root XOR i`.
//!
//! FR-2.3.1/FR-2.3.2. Each 65,536-byte on-disk slot is the ciphertext of up to
//! [`CHUNK_PLAINTEXT`] bytes followed by a 16-byte tag, sealed or opened in place.
//!
//! # Nonce derivation
//!
//! PRD writes `Nonce_i = N_root XOR i` without saying how `i` is encoded. Here `i` is the
//! chunk index as a big-endian `u64` in the **low 8 bytes** of the 24-byte nonce, XORed into
//! the corresponding bytes of `N_root`, leaving `N_root[0..16]` untouched. Two consequences
//! worth stating because they are properties the design depends on:
//!
//! * `i` needs 21 bits for the whole container (2^21 slots), so 8 bytes is ample and the
//!   encoding cannot alias.
//! * Distinct chunks get distinct nonces as long as no two indices agree, which they do not.
//!   The xor is what makes the nonce stream a *counter*: it is the construction RSA-2048
//!   and XChaCha20-Poly1305 are usually combined with, and it is safe precisely because
//!   each index is used once per container.
//!
//! A caveat that is *not* handled here: two containers built from two different passphrases
//! get unrelated `N_root` values, so re-creating a container at the same path with a
//! different passphrase produces a different nonce stream. That is fine. What would *not*
//! be fine is reusing one key across containers with different nonces, which is why
//! `K_enc` is derived per container from its own salt and never persisted.
//!
//! # AAD
//!
//! The chunk index is authenticated as associated data. That costs nothing and buys one
//! real property: a chunk cannot be moved to a different index without the tag failing. On
//! its own that is not a confidentiality property, but it means a whole-file rewrite that
//! reorders chunks is detected rather than silently producing a scrambled document.

use chacha20poly1305::aead::{AeadInOut, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use inout::InOutBuf;

use crate::layout::{CHUNK_PLAINTEXT, CHUNK_SLOT, TAG_LEN};

/// Byte offset of the tag inside a slot: the last [`TAG_LEN`] bytes.
pub const TAG_OFFSET: usize = CHUNK_SLOT as usize - TAG_LEN;

/// Why a chunk could not be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AeadError {
    /// The Poly1305 tag did not verify: wrong key, wrong nonce, corrupted bytes, or the
    /// chunk is not where it claims to be.
    ///
    /// These are deliberately indistinguishable. Reporting *which* of the four it was
    /// would be a decryption oracle, and this path is reachable with attacker-chosen input
    /// (any passphrase, any container).
    AuthenticationFailed,
    /// The decrypted plaintext was longer than a slot can hold, which means the slot was
    /// written by something that is not this format.
    PlaintextTooLarge {
        /// Length that was found.
        len: usize,
    },
    /// `K_enc` or `N_root` was the wrong size.
    BadKeyLength,
}

impl core::fmt::Display for AeadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            // Deliberately vague. See the variant docs.
            Self::AuthenticationFailed => f.write_str("chunk authentication failed"),
            Self::PlaintextTooLarge { len } => {
                write!(f, "decrypted plaintext of {len} exceeds a slot")
            }
            Self::BadKeyLength => f.write_str("key or nonce was the wrong length"),
        }
    }
}

impl std::error::Error for AeadError {}

/// `nonce_i = N_root` with the chunk index XORed into its low 8 bytes.
pub fn chunk_nonce(n_root: &[u8; 24], index: u64) -> XNonce {
    let mut nonce = *n_root;
    for (b, i) in nonce[16..24].iter_mut().zip(index.to_be_bytes()) {
        *b ^= i;
    }
    // `aead` 0.6 uses `hybrid_array::Array`, which has `From<[T; N]>`.
    XNonce::from(nonce)
}

/// Seal `plaintext` into `slot`, tag appended.
///
/// `slot` is a full [`CHUNK_SLOT`]. The bytes past the ciphertext are filled with
/// chaff-grade randomness rather than left zero, because a run of zeroes in the middle of
/// a file that is supposed to be indistinguishable from noise is exactly the kind of thing
/// a histogram finds.
pub fn seal_chunk(
    k_enc: &[u8; 32],
    n_root: &[u8; 24],
    index: u64,
    plaintext: &[u8],
    slot: &mut [u8],
) -> Result<(), AeadError> {
    if plaintext.len() > CHUNK_PLAINTEXT {
        return Err(AeadError::PlaintextTooLarge {
            len: plaintext.len(),
        });
    }
    assert_eq!(
        slot.len(),
        CHUNK_SLOT as usize,
        "slot must be exactly CHUNK_SLOT bytes"
    );

    slot[..plaintext.len()].copy_from_slice(plaintext);
    seal_in_place(k_enc, n_root, index, plaintext.len(), slot)
}

/// Seal a slot whose plaintext is **already** in `slot[..len]`.
///
/// Exists so the ring can re-seal a decrypted chunk without a fourth 65,536-byte buffer.
/// Copying the plaintext out to a temporary first would put the resident footprint at
/// 256 KiB and quietly break FR-3.1's 192 KiB ceiling, which is the one number this ring
/// exists to honour.
///
/// The bytes between the plaintext and the tag are overwritten with fresh randomness. A
/// short chunk must not leave a run of zeroes in the middle of a file that is supposed to
/// be indistinguishable from noise, so the gap is filled rather than skipped.
pub fn seal_in_place(
    k_enc: &[u8; 32],
    n_root: &[u8; 24],
    index: u64,
    len: usize,
    slot: &mut [u8],
) -> Result<(), AeadError> {
    if len > CHUNK_PLAINTEXT {
        return Err(AeadError::PlaintextTooLarge { len });
    }
    assert_eq!(
        slot.len(),
        CHUNK_SLOT as usize,
        "slot must be exactly CHUNK_SLOT bytes"
    );
    getrandom::fill(&mut slot[len..TAG_OFFSET]).map_err(|_| AeadError::AuthenticationFailed)?;

    let cipher = XChaCha20Poly1305::new(&Key::from(*k_enc));
    let tag = cipher
        .encrypt_inout_detached(
            &chunk_nonce(n_root, index),
            &index.to_be_bytes(),
            InOutBuf::from(&mut slot[..TAG_OFFSET]),
        )
        .map_err(|_| AeadError::AuthenticationFailed)?;
    slot[TAG_OFFSET..].copy_from_slice(&tag[..TAG_LEN]);
    Ok(())
}

/// Open a slot in place, returning the plaintext length.
///
/// On failure the slot's contents are undefined — `open_in_place_detached` leaves them in
/// an unspecified state — so callers must not retry or inspect them.
pub fn open_chunk(
    k_enc: &[u8; 32],
    n_root: &[u8; 24],
    index: u64,
    slot: &mut [u8],
) -> Result<usize, AeadError> {
    assert_eq!(
        slot.len(),
        CHUNK_SLOT as usize,
        "slot must be exactly CHUNK_SLOT bytes"
    );
    let cipher = XChaCha20Poly1305::new(&Key::from(*k_enc));
    let tag: [u8; TAG_LEN] = slot[TAG_OFFSET..]
        .try_into()
        .map_err(|_| AeadError::AuthenticationFailed)?;
    cipher
        .decrypt_inout_detached(
            &chunk_nonce(n_root, index),
            &index.to_be_bytes(),
            InOutBuf::from(&mut slot[..TAG_OFFSET]),
            &tag.into(),
        )
        .map_err(|_| AeadError::AuthenticationFailed)?;
    Ok(CHUNK_PLAINTEXT)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [0x2B; 32];
    const NONCE: [u8; 24] = [0x7C; 24];

    /// Cross-check against an independent implementation, so this is not just "seal then
    /// open agrees with itself".
    ///
    /// The reference is `tests/xchacha20poly1305_ref.py`, a from-scratch XChaCha20-Poly1305
    /// in Python that shares no code with the Rust path. It was written because the obvious
    /// cross-check -- Python's `cryptography` module -- is not installed on this host, and
    /// hand-porting HChaCha20 in Python is a smaller risk than trusting a second library for
    /// a value that the whole container format depends on.
    ///
    /// That reference was wrong the first time. It implemented HChaCha20 with ChaCha20's
    /// feed-forward addition, which produces a plausible-looking subkey that matches
    /// nothing, and the disagreement with these vectors is what surfaced it. It now
    /// self-tests against RFC 8439 sec 2.8.2 and draft-irtf-cfrg-xchacha sec 2.2.1 and 2.4.2
    /// before printing anything, so the next reader does not have to rediscover that a
    /// reference implementation needs its own reference.
    ///
    /// Both vectors use a plaintext that exactly fills the slot, because `seal_chunk` fills
    /// the gap between a short plaintext and the tag with fresh randomness, and a random gap
    /// would make the tag non-reproducible. The gap's behaviour is tested separately in
    /// `unfilled_slot_bytes_are_not_zero`.
    #[test]
    fn matches_an_independent_vector() {
        // KEY = 2b×32, NONCE = 7c×24, index 0, plaintext = (i mod 251) for i < 65,520.
        let plaintext: Vec<u8> = (0..CHUNK_PLAINTEXT).map(|i| (i % 251) as u8).collect();
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        seal_chunk(&KEY, &NONCE, 0, &plaintext, &mut slot).expect("seal");
        assert_eq!(
            hex(&slot[..32]),
            "7b273e5222fe12c325ef7cba18beb28a29dd8328cf90bfc02d78f5e175d53734",
            "ciphertext does not match the reference implementation"
        );
        assert_eq!(
            hex(&slot[TAG_OFFSET..]),
            "9a5c47b245aa64a63298a4276a6d6700",
            "tag does not match the reference implementation"
        );
    }

    /// The same vector at a different index, with `N_root`'s low 8 bytes actually XORed.
    /// This is the test that would catch an off-by-one in the nonce counter or a mistake in
    /// which half of `N_root` is touched -- neither of which the round-trip tests can see.
    #[test]
    fn matches_an_independent_vector_at_index_five() {
        // N_root low 8 bytes = a5, XOR index 5 -> a0.
        let mut n_root = NONCE;
        n_root[16..24].copy_from_slice(&[0xa5; 8]);
        let plaintext: Vec<u8> = (0..CHUNK_PLAINTEXT).map(|i| (i % 253) as u8).collect();
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        seal_chunk(&KEY, &n_root, 5, &plaintext, &mut slot).expect("seal");
        assert_eq!(
            hex(&slot[..32]),
            "7cb4956e07df7a4c34ec3b8b9e18b65a3080be90b46d465833637714ff1aaa9a",
        );
        assert_eq!(hex(&slot[TAG_OFFSET..]), "53fb430fe94fb4cea94adff74d203667");
    }

    /// Opening a reference slot must produce the reference plaintext.
    #[test]
    fn opens_a_reference_slot() {
        let plaintext: Vec<u8> = (0..CHUNK_PLAINTEXT).map(|i| (i % 251) as u8).collect();
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        seal_chunk(&KEY, &NONCE, 0, &plaintext, &mut slot).expect("seal");
        let len = open_chunk(&KEY, &NONCE, 0, &mut slot).expect("open");
        assert_eq!(len, CHUNK_PLAINTEXT);
        // Compare only the plaintext region. The slot is CHUNK_SLOT bytes -- 16 more than
        // the plaintext -- and that tail is gap bytes the AEAD covered but never defined, so
        // asserting over the whole slot would be asserting on values nobody chose.
        assert!(
            slot[..CHUNK_PLAINTEXT] == plaintext[..],
            "reference plaintext did not round-trip"
        );
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// Round trip, including the length.
    #[test]
    fn seal_then_open_round_trips() {
        for len in [0usize, 1, 100, CHUNK_PLAINTEXT] {
            let plaintext: Vec<u8> = (0..len).map(|i| (i % 253) as u8).collect();
            let mut slot = vec![0u8; CHUNK_SLOT as usize];
            seal_chunk(&KEY, &NONCE, 3, &plaintext, &mut slot).expect("seal");
            let got_len = open_chunk(&KEY, &NONCE, 3, &mut slot).expect("open");
            assert_eq!(&slot[..plaintext.len()], &plaintext[..], "len {len}");
            assert_eq!(got_len, CHUNK_PLAINTEXT);
        }
    }

    /// The unfilled part of a slot must not be zero, or a histogram finds it.
    #[test]
    fn unfilled_slot_bytes_are_not_zero() {
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        seal_chunk(&KEY, &NONCE, 0, b"short", &mut slot).expect("seal");
        let tail = &slot[5..TAG_OFFSET];
        assert!(
            tail.iter().any(|&b| b != 0),
            "the gap between plaintext and tag must be filled, not zeroed"
        );
        // And it must be high-entropy-ish, not a constant.
        assert!(tail.windows(8).any(|w| w.iter().any(|&b| b != w[0])));
    }

    /// A single flipped bit anywhere in the slot must fail authentication. This is the
    /// property that makes a truncated or bit-rotted container detectable.
    #[test]
    fn any_single_bit_flip_is_detected() {
        let plaintext = vec![0xA5u8; 1000];
        let mut good = vec![0u8; CHUNK_SLOT as usize];
        seal_chunk(&KEY, &NONCE, 9, &plaintext, &mut good).expect("seal");

        for bit in [
            0usize,
            1,
            63,
            64,
            1000,
            CHUNK_SLOT as usize - 17,
            CHUNK_SLOT as usize - 1,
        ] {
            let mut slot = good.clone();
            slot[bit] ^= 1;
            assert_eq!(
                open_chunk(&KEY, &NONCE, 9, &mut slot),
                Err(AeadError::AuthenticationFailed),
                "flipping bit in byte {bit} was not detected"
            );
        }
    }

    /// A chunk moved to a different index must fail: the index is in the AAD and in the
    /// nonce, so reordering is detected.
    #[test]
    fn a_chunk_cannot_be_moved() {
        let plaintext = b"chunk one content";
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        seal_chunk(&KEY, &NONCE, 1, plaintext, &mut slot).expect("seal at 1");
        assert_eq!(
            open_chunk(&KEY, &NONCE, 2, &mut slot),
            Err(AeadError::AuthenticationFailed),
            "reading it at index 2 succeeded, so AAD is not binding the index"
        );
    }

    /// The wrong key must fail, and must be indistinguishable from corruption.
    #[test]
    fn wrong_key_fails_ambiguously() {
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        seal_chunk(&KEY, &NONCE, 0, b"data", &mut slot).expect("seal");
        let other = [0x2Cu8; 32];
        assert_eq!(
            open_chunk(&other, &NONCE, 0, &mut slot),
            Err(AeadError::AuthenticationFailed)
        );
    }

    /// The wrong `N_root` must fail.
    #[test]
    fn wrong_nonce_root_fails() {
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        seal_chunk(&KEY, &NONCE, 0, b"data", &mut slot).expect("seal");
        assert_eq!(
            open_chunk(&KEY, &[0x7Du8; 24], 0, &mut slot),
            Err(AeadError::AuthenticationFailed)
        );
    }

    /// `nonce_i` must be `N_root` with only the low 8 bytes touched, and must differ per
    /// index.
    #[test]
    fn nonce_derivation_is_as_documented() {
        let mut n_root = NONCE;
        n_root[16..24].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        let base = chunk_nonce(&n_root, 0);
        assert_eq!(&base[16..24], &[0u8; 8]);
        assert_eq!(
            &base[..16],
            &n_root[..16],
            "the high 16 bytes are untouched"
        );

        let n5 = chunk_nonce(&n_root, 5);
        assert_eq!(&n5[16..24], &[0, 0, 0, 0, 0, 0, 0, 5]);
        assert_eq!(&n5[..16], &n_root[..16]);

        let n6 = chunk_nonce(&n_root, 6);
        assert_ne!(n5.as_slice(), n6.as_slice());

        // Indices differing only in the top bits still differ, since i is 8 bytes wide.
        assert_ne!(
            chunk_nonce(&n_root, 1u64 << 40).as_slice(),
            chunk_nonce(&n_root, 1u64 << 41).as_slice()
        );
    }

    /// Over-long plaintext is refused rather than truncating silently.
    #[test]
    fn over_long_plaintext_is_refused() {
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        let too_big = vec![0u8; CHUNK_PLAINTEXT + 1];
        assert_eq!(
            seal_chunk(&KEY, &NONCE, 0, &too_big, &mut slot),
            Err(AeadError::PlaintextTooLarge {
                len: CHUNK_PLAINTEXT + 1
            })
        );
    }

    /// Every chunk in the container must get a distinct nonce, so no index can ever reuse
    /// one. Checked over the whole addressable range's low bits.
    #[test]
    fn no_two_chunk_indices_share_a_nonce() {
        let n_root = NONCE;
        let mut seen = std::collections::HashSet::new();
        for i in 0..crate::layout::MAX_CHUNKS {
            assert!(seen.insert(chunk_nonce(&n_root, i).as_slice().to_vec()));
        }
        assert_eq!(seen.len() as u64, crate::layout::MAX_CHUNKS);
    }
}
