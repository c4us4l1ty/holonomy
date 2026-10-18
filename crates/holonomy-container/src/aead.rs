//! Per-chunk AEAD: XChaCha20-Poly1305 with `nonce_i = N_root XOR i XOR tag(i)`.
//!
//! FR-2.3.1/FR-2.3.2. Each 65,536-byte on-disk slot is the ciphertext of up to
//! [`CHUNK_PLAINTEXT`] bytes followed by a 16-byte tag, sealed or opened in place.
//!
//! # Nonce derivation
//!
//! PRD writes `Nonce_i = N_root XOR i` without saying how `i` is encoded. Here `i` is the
//! chunk index as a big-endian `u64` in the **low 8 bytes** of the 24-byte nonce, XORed into
//! the corresponding bytes of `N_root`. The remaining **16 bytes** carry the chaining term,
//! described below.
//!
//! * `i` needs 21 bits for the whole container (2^21 slots), so 8 bytes is ample and the
//!   encoding cannot alias.
//!
//! # Why the chaining term exists: `Nonce_i = N_root XOR i` alone reuses a nonce
//!
//! The bare `XOR i` derivation is only safe if each index is sealed **exactly once, ever**. A
//! container is not write-once: `Wavefunction::write_chunk`, `Ring::commit` and
//! `Wavefunction::rewrite_master_frame` all re-seal an index that already holds ciphertext, on
//! every save, every content-length change and every title change. Each of those encrypts new
//! plaintext under a `(K_enc, nonce)` pair the container has already used.
//!
//! XChaCha20 is counter mode, so the two ciphertexts XOR to the XOR of the two plaintexts:
//! `C1 XOR C2 = P1 XOR P2`. For a chunk that is being edited, `P2` is `P1` with a few keystrokes
//! changed, and `P1 XOR P2` is a sparse, *readable* mask over exactly the region the user touched.
//! For XChaCha20-Poly1305 the Poly1305 one-time key is a deterministic function of
//! `(key, nonce)` as well, so a repeated nonce also repeats the tag key, which is the standard
//! route to forgery. The salt is stored in the clear, so `K_enc` and `N_root` re-derive
//! identically in every session and the reuse spans sessions too.
//!
//! So the nonce is
//!
//! ```text
//! nonce[0..16]  = N_root[0..16]  XOR tag(i)      // the seal this one supersedes
//! nonce[16..24] = N_root[16..24] XOR index_be    // unchanged: the counter
//! ```
//!
//! where `prev_tag(i)` is the Poly1305 tag of the **previous** seal of chunk `i`, and it is
//! **stored in the slot**: the layout is `ciphertext | prev_tag | tag`, with `CHUNK_PLAINTEXT`
//! reduced by 16 bytes to pay for it (`crate::layout`). That is what lets [`open_chunk`] rebuild
//! the nonce from the slot alone — it is handed a slot and a key and nothing else — and it sits
//! inside the authenticated range, so an attacker cannot steer the chaining term.
//!
//! Sealing needs the predecessor tag, so it takes one 4 KiB `pread64` of the slot's trailing page
//! (`ring::prev_tag_from_disk`): one extra block on a path that already writes 64 KiB, read from
//! the file rather than cached, so a failed write and a retry cannot chain on a tag the file does
//! not have.
//!
//! A caveat that is *not* handled here: two containers built from two different passphrases
//! get unrelated `N_root` values, so re-creating a container at the same path with a
//! different passphrase produces a different nonce stream. That is fine. What would *not*
//! be fine is reusing one key across containers with different nonces, which is why
//! `K_enc` is derived per container from its own salt and never persisted.
//!
//! ## Why this is safe, precisely
//!
//! * **Injective in the predecessor tag at a fixed index.** `t -> N_root[0..16] XOR t` is a
//!   bijection, so two seals of the same chunk collide only if their predecessor tags are equal.
//! * **No cross-index collision is possible.** Chunks `i != j` differ in the low 8 bytes by
//!   construction, whatever their tags.
//! * **The chained term is unpredictable and unsteerable.** The predecessor tag is a Poly1305
//!   output under a key nobody can compute without `K_enc`, and it lies inside the authenticated
//!   range of the seal that follows it, so rewriting it invalidates a tag the attacker cannot
//!   produce. A fresh slot's predecessor tag is whatever chaff is there, equally unpredictable.
//! * **The degenerate case.** A slot whose predecessor-tag field is zero — a region never
//!   written — chains from `0`, which is exactly the old derivation. `create` lays chaff over the
//!   whole file before any payload is written, so the only way to reach it is a container whose
//!   chaff was destroyed, and such a container no longer authenticates anyway.
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

use crate::layout::{CHUNK_PLAINTEXT, CHUNK_SLOT, PREV_TAG_OFFSET, TAG_LEN, TAG_OFFSET};

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

/// `nonce_i = N_root`, the chunk index XORed into its low 8 bytes and the tag this seal
/// supersedes XORed into its high 16.
///
/// See the module docs for why the tag is there: `N_root XOR i` on its own reuses a nonce on
/// every re-save, and a reused nonce in counter mode is a one-time pad worn twice.
pub fn chunk_nonce(n_root: &[u8; 24], index: u64, prev_tag: &[u8; TAG_LEN]) -> XNonce {
    let mut nonce = *n_root;
    // The tag goes in the half the index counter never touches, so the two terms cannot
    // cancel: a fixed index is a bijection on the tag, and distinct indices differ below.
    for (b, t) in nonce[..16].iter_mut().zip(prev_tag.iter()) {
        *b ^= *t;
    }
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
///
/// `prev_tag` is the tag the destination slot **currently carries on disk** — the seal this one
/// supersedes. It must be read before the slot is overwritten;
/// [`crate::ring::prev_tag_from_disk`] is the accessor. Passing the tag the slot is about to
/// hold, or passing [`UNSEALED`] where a real predecessor belonged, is a nonce reuse and is the
/// whole reason this parameter exists.
pub fn seal_chunk(
    k_enc: &[u8; 32],
    n_root: &[u8; 24],
    index: u64,
    prev_tag: &[u8; TAG_LEN],
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
    seal_in_place(k_enc, n_root, index, prev_tag, plaintext.len(), slot)
}

/// The tag a slot carries when it has never been sealed by this build: all zero.
///
/// Only correct for a slot that has genuinely never been written — a region of a fresh
/// container that no payload has reached. Everywhere else the predecessor tag must be *read*,
/// not assumed. `crate::ring::prev_tag_from_disk` is the accessor.
pub const UNSEALED: [u8; TAG_LEN] = [0u8; TAG_LEN];


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
    prev_tag: &[u8; TAG_LEN],
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
    // **Skip the `getrandom` call when there is nothing to fill.**
    //
    // `len` is `CHUNK_PLAINTEXT` for every full chunk and `TAG_OFFSET` is exactly
    // `CHUNK_PLAINTEXT`, so the gap is **empty** for all but the last chunk of a document.
    // Asking the kernel for zero bytes is not free, and the condition is one comparison.
    //
    // **This was not the cause of the 28 ms seal that sent me here**, which is the useful thing to
    // say out loud: the first version of this comment claimed it was, and the measurement said
    // otherwise — 29,455 us after the skip against 28,503 us before, on the same host and build.
    // The real cause was that the cipher's constant belongs to an optimised build and the gate ran
    // in a debug one; see `write_through_cost.rs`.
    //
    // **The randomness requirement is unchanged** — a *short* chunk still gets a random gap, which
    // is what keeps a run of zeroes out of the middle of a file that is supposed to be
    // indistinguishable from noise. This only skips work that had nothing to do.
    if len < CHUNK_PLAINTEXT {
        getrandom::fill(&mut slot[len..CHUNK_PLAINTEXT])
            .map_err(|_| AeadError::AuthenticationFailed)?;
    }

    // **The predecessor tag is written clear, outside the ciphertext, and authenticated as AAD.**
    //
    // Those two requirements pull in opposite directions and this is the arrangement that
    // satisfies both:
    //
    // * It cannot be inside the encrypted range, because `open_chunk` must *read* it in order
    //   to build the nonce that decrypts that range. The first version of this put the write
    //   inside `slot[..TAG_OFFSET]` and every container failed to reopen: the bytes came back
    //   as ciphertext, so the nonce was rebuilt from the wrong value.
    // * It cannot be unauthenticated either. In the clear and outside the message means an
    //   attacker may rewrite it — and that has to fail, or the nonce becomes a thing they steer.
    //
    // The AAD is where it belongs: it is authenticated by the tag, and it is known in advance at
    // both ends because at open time it is read out of the same slot.
    slot[PREV_TAG_OFFSET..TAG_OFFSET].copy_from_slice(&prev_tag[..TAG_LEN]);
    let cipher = XChaCha20Poly1305::new(&Key::from(*k_enc));
    let tag = cipher
        .encrypt_inout_detached(
            &chunk_nonce(n_root, index, prev_tag),
            &aad(index, prev_tag),
            InOutBuf::from(&mut slot[..CHUNK_PLAINTEXT]),
        )
        .map_err(|_| AeadError::AuthenticationFailed)?;
    slot[TAG_OFFSET..].copy_from_slice(&tag[..TAG_LEN]);
    Ok(())
}

/// The 24 bytes of associated data a chunk is sealed under: the index, then the predecessor tag.
///
/// **The index was the whole of it until tag chaining**, and the shape of this is load-bearing
/// rather than decorative. `open_chunk` computes the AAD from the index and from bytes it has
/// just read out of the slot, so the tag fails if either the chunk is moved to another index or
/// its chaining term is rewritten. That second property is what makes the chaining term
/// unsteerable, and it is the reason the term is in the AAD instead of merely being present in
/// the slot.
fn aad(index: u64, prev_tag: &[u8; TAG_LEN]) -> [u8; 8 + TAG_LEN] {
    let mut a = [0u8; 8 + TAG_LEN];
    a[..8].copy_from_slice(&index.to_be_bytes());
    a[8..].copy_from_slice(&prev_tag[..]);
    a
}

/// Open a slot in place, returning the plaintext length.
///
/// On failure the slot's contents are undefined — `open_in_place_detached` leaves them in
/// an unspecified state — so callers must not retry or inspect them.
///
/// The chaining term of the nonce is read **out of the slot** rather than passed in, which is
/// why this signature is unchanged by tag chaining: the predecessor tag sits in the clear at
/// [`PREV_TAG_OFFSET`], so the bytes needed to rebuild the nonce are the bytes already in hand.
/// It is read *before* decryption, and it is outside `slot[..CHUNK_PLAINTEXT]` so reading it
/// cannot disturb the plaintext being produced.
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
    let tag: [u8; TAG_LEN] = slot[TAG_OFFSET..]
        .try_into()
        .map_err(|_| AeadError::AuthenticationFailed)?;
    let prev_tag: [u8; TAG_LEN] = slot[PREV_TAG_OFFSET..TAG_OFFSET]
        .try_into()
        .map_err(|_| AeadError::AuthenticationFailed)?;
    let cipher = XChaCha20Poly1305::new(&Key::from(*k_enc));
    cipher
        .decrypt_inout_detached(
            &chunk_nonce(n_root, index, &prev_tag),
            &aad(index, &prev_tag),
            InOutBuf::from(&mut slot[..CHUNK_PLAINTEXT]),
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
    /// Both vectors use a plaintext that exactly fills the ciphertext region, because
    /// `seal_chunk` fills the gap between a short plaintext and the end of that region with
    /// fresh randomness, and a random gap would make the tag non-reproducible. The gap's
    /// behaviour is tested separately in `unfilled_slot_bytes_are_not_zero`.
    ///
    /// **Vector A chains from an all-zero predecessor tag**, which is what a slot nobody has
    /// written yet holds. Vector B chains from a non-zero one, so all three terms of the nonce
    /// — the tag in the high 16 bytes, the index in the low 8, and `N_root` itself — are
    /// exercised against the reference at once.
    #[test]
    fn matches_an_independent_vector() {
        // KEY = 2b×32, NONCE = 7c×24, index 0, prev_tag = 0, plaintext = (i mod 251).
        let plaintext: Vec<u8> = (0..CHUNK_PLAINTEXT).map(|i| (i % 251) as u8).collect();
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        seal_chunk(&KEY, &NONCE, 0, &UNSEALED, &plaintext, &mut slot).expect("seal");
        assert_eq!(
            hex(&slot[..32]),
            "7b273e5222fe12c325ef7cba18beb28a29dd8328cf90bfc02d78f5e175d53734",
            "ciphertext does not match the reference implementation"
        );
        assert_eq!(
            hex(&slot[TAG_OFFSET..]),
            "13c06951db681e1f83545a7b0a58c098",
            "tag does not match the reference implementation"
        );
    }

    /// The same vector at a different index, with `N_root`'s low 8 bytes actually XORed **and a
    /// non-zero predecessor tag**. This is the test that would catch an off-by-one in the
    /// nonce counter, a mistake in which half of `N_root` each term lands in, or a chained
    /// nonce that was derived but never written -- none of which the round-trip tests can see,
    /// because a nonce that is derived consistently on both sides agrees with itself perfectly.
    #[test]
    fn matches_an_independent_vector_at_index_five() {
        // N_root low 8 bytes = a5, XOR index 5 -> a0. High 16 = 7c, XOR 10..1f -> 6c..63.
        let mut n_root = NONCE;
        n_root[16..24].copy_from_slice(&[0xa5; 8]);
        let prev: [u8; TAG_LEN] = std::array::from_fn(|i| 0x10 + i as u8);
        let plaintext: Vec<u8> = (0..CHUNK_PLAINTEXT).map(|i| (i % 253) as u8).collect();
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        seal_chunk(&KEY, &n_root, 5, &prev, &plaintext, &mut slot).expect("seal");
        assert_eq!(
            hex(&slot[..32]),
            "1a7929911f7fafcbf78abe694e2969557a174c3105708cc60af7973df3eac8c4",
        );
        assert_eq!(hex(&slot[TAG_OFFSET..]), "b40d3b4acd95cd8aae573a68bba542d8");
        // **The nonce itself, not just its output.** The reference prints it, and pinning it
        // says where each term landed rather than only that the two implementations agree --
        // which they would also agree on if both had the same misunderstanding.
        assert_eq!(hex(chunk_nonce(&n_root, 5, &prev).as_slice()), "6c6d6e6f68696a6b6465666760616263a5a5a5a5a5a5a5a0");
        // And the predecessor tag is stored clear, where `open_chunk` can read it back.
        assert_eq!(hex(&slot[PREV_TAG_OFFSET..TAG_OFFSET]), hex(&prev[..]));
    }

    /// Opening a reference slot must produce the reference plaintext.
    #[test]
    fn opens_a_reference_slot() {
        let plaintext: Vec<u8> = (0..CHUNK_PLAINTEXT).map(|i| (i % 251) as u8).collect();
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        seal_chunk(&KEY, &NONCE, 0, &UNSEALED, &plaintext, &mut slot).expect("seal");
        let len = open_chunk(&KEY, &NONCE, 0, &mut slot).expect("open");
        assert_eq!(len, CHUNK_PLAINTEXT);
        // Compare only the plaintext region. The slot is CHUNK_SLOT bytes -- two tag-sized
        // trailers more than the plaintext -- and that tail is a predecessor tag and a gap the
        // AEAD covered but never defined, so asserting over the whole slot would be asserting
        // on values nobody chose.
        assert!(
            slot[..CHUNK_PLAINTEXT] == plaintext[..],
            "reference plaintext did not round-trip"
        );
    }

    /// **The gate for the defect this whole construction exists to close.**
    ///
    /// A container is not write-once. Sealing the same chunk index again encrypts *new*
    /// plaintext, and if the nonce were a function of the index alone it would be the same
    /// `(key, nonce)` pair — so the two ciphertexts XOR to `P1 XOR P2`, a readable mask over
    /// exactly the bytes the user edited. Every round-trip test above passes with that bug
    /// present, because both sides derive the same (wrong) nonce; nothing here is visible from
    /// the plaintext side. So it is asserted directly: **two seals of one index, chaining
    /// correctly, must not share a nonce**, and the leakage must be checkable rather than
    /// merely argued.
    #[test]
    fn a_re_seal_of_one_index_does_not_reuse_the_nonce() {
        let mut on_disk = vec![0u8; CHUNK_SLOT as usize];
        let first: Vec<u8> = vec![0x41u8; CHUNK_PLAINTEXT];
        let mut second = first.clone();
        second[100] = b'Z'; // one keystroke later
        assert_ne!(first, second, "the two revisions must differ or the test proves nothing");

        // Seal 1: the slot holds whatever was there before -- nothing.
        let before_1: [u8; TAG_LEN] = on_disk[TAG_OFFSET..].try_into().unwrap();
        seal_chunk(&KEY, &NONCE, 4, &before_1, &first, &mut on_disk).expect("seal 1");
        let nonce_1 = chunk_nonce(&NONCE, 4, &before_1);
        let ct_1 = on_disk[..CHUNK_PLAINTEXT].to_vec();

        // Seal 2: the predecessor tag is the one seal 1 actually left on disk.
        let before_2: [u8; TAG_LEN] = on_disk[TAG_OFFSET..].try_into().unwrap();
        seal_chunk(&KEY, &NONCE, 4, &before_2, &second, &mut on_disk).expect("seal 2");
        let nonce_2 = chunk_nonce(&NONCE, 4, &before_2);
        let ct_2 = on_disk[..CHUNK_PLAINTEXT].to_vec();

        assert_ne!(
            nonce_1.as_slice(),
            nonce_2.as_slice(),
            "two seals of chunk 4 shared a nonce: the container reuses a one-time pad"
        );
        // And the consequence, stated as arithmetic rather than as prose: the ciphertexts must
        // not XOR to the plaintexts' XOR. Under a repeated nonce they do, exactly.
        let mut leak = vec![0u8; CHUNK_PLAINTEXT];
        for i in 0..CHUNK_PLAINTEXT {
            leak[i] = ct_1[i] ^ ct_2[i];
        }
        let mut expected = vec![0u8; CHUNK_PLAINTEXT];
        for i in 0..CHUNK_PLAINTEXT {
            expected[i] = first[i] ^ second[i];
        }
        assert_ne!(
            leak, expected,
            "ciphertext XOR leaked the plaintext XOR: this is a reused keystream"
        );
        // The single differing byte must not be the only difference in the XOR.
        assert!(
            leak.iter().filter(|&&b| b != 0).count() > 1,
            "the ciphertexts differ in only one place, which is what a reused keystream does"
        );

        // Both revisions still open, which is the property a nonce break trades away.
        let mut a = on_disk.clone();
        assert_eq!(open_chunk(&KEY, &NONCE, 4, &mut a).expect("open 2"), CHUNK_PLAINTEXT);
        assert_eq!(&a[..CHUNK_PLAINTEXT], &second[..]);
    }

    /// Rewriting a chunk with **byte-identical** plaintext must still move the nonce.
    ///
    /// This is the case a "skip the write if nothing changed" optimisation would reintroduce the
    /// bug through, and the case a reader who assumes "same plaintext, same ciphertext, so the
    /// nonce cannot have mattered" would wave away. The predecessor tag is what makes the two
    /// distinguishable, so it is asserted rather than left as an argument.
    #[test]
    fn an_identical_rewrite_still_moves_the_nonce() {
        let body = vec![0x5Au8; CHUNK_PLAINTEXT];
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        seal_chunk(&KEY, &NONCE, 2, &UNSEALED, &body, &mut slot).expect("seal 1");
        let t1: [u8; TAG_LEN] = slot[TAG_OFFSET..].try_into().unwrap();
        seal_chunk(&KEY, &NONCE, 2, &t1, &body, &mut slot).expect("seal 2");
        let t2: [u8; TAG_LEN] = slot[TAG_OFFSET..].try_into().unwrap();
        assert_ne!(t1, t2, "an identical rewrite reused the predecessor tag");
        assert_ne!(
            chunk_nonce(&NONCE, 2, &t1).as_slice(),
            chunk_nonce(&NONCE, 2, &t2).as_slice(),
        );
        assert_eq!(open_chunk(&KEY, &NONCE, 2, &mut slot).expect("open"), CHUNK_PLAINTEXT);
    }

    /// The predecessor tag is authenticated, so rewriting it must break the chunk.
    ///
    /// Without this the chaining term is a nonce an attacker can steer: they change 16 bytes
    /// of cleartext in the slot and the next seal of that slot lands on a nonce of their
    /// choosing. The tag is what refuses.
    #[test]
    fn rewriting_the_predecessor_tag_fails_to_open() {
        let body = vec![0x33u8; CHUNK_PLAINTEXT];
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        seal_chunk(&KEY, &NONCE, 6, &UNSEALED, &body, &mut slot).expect("seal");
        slot[PREV_TAG_OFFSET] ^= 0x01;
        assert_eq!(
            open_chunk(&KEY, &NONCE, 6, &mut slot),
            Err(AeadError::AuthenticationFailed),
            "a rewritten chaining term must not authenticate"
        );
    }

    /// Moving a chunk to another index must fail — the AAD binds the index, and the chaining
    /// term is part of that AAD rather than decoration.
    #[test]
    fn a_chunk_moved_to_another_index_does_not_open() {
        let body = vec![0x11u8; CHUNK_PLAINTEXT];
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        seal_chunk(&KEY, &NONCE, 1, &UNSEALED, &body, &mut slot).expect("seal");
        assert_eq!(
            open_chunk(&KEY, &NONCE, 2, &mut slot),
            Err(AeadError::AuthenticationFailed),
            "the index is authenticated and a chunk must not be relocatable"
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
            seal_chunk(&KEY, &NONCE, 3, &UNSEALED, &plaintext, &mut slot).expect("seal");
            let got_len = open_chunk(&KEY, &NONCE, 3, &mut slot).expect("open");
            assert_eq!(&slot[..plaintext.len()], &plaintext[..], "len {len}");
            assert_eq!(got_len, CHUNK_PLAINTEXT);
        }
    }

    /// The unfilled part of a slot must not be zero, or a histogram finds it.
    #[test]
    fn unfilled_slot_bytes_are_not_zero() {
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        seal_chunk(&KEY, &NONCE, 0, &UNSEALED, b"short", &mut slot).expect("seal");
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
        seal_chunk(&KEY, &NONCE, 9, &UNSEALED, &plaintext, &mut good).expect("seal");

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
        seal_chunk(&KEY, &NONCE, 1, &UNSEALED, plaintext, &mut slot).expect("seal at 1");
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
        seal_chunk(&KEY, &NONCE, 0, &UNSEALED, b"data", &mut slot).expect("seal");
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
        seal_chunk(&KEY, &NONCE, 0, &UNSEALED, b"data", &mut slot).expect("seal");
        assert_eq!(
            open_chunk(&KEY, &[0x7Du8; 24], 0, &mut slot),
            Err(AeadError::AuthenticationFailed)
        );
    }

    /// `nonce_i` must be `N_root` with the index in the low 8 bytes and the predecessor tag in
    /// the high 16, and the two must not disturb each other.
    #[test]
    fn nonce_derivation_is_as_documented() {
        let mut n_root = NONCE;
        n_root[16..24].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        let base = chunk_nonce(&n_root, 0, &UNSEALED);
        assert_eq!(&base[16..24], &[0u8; 8]);
        assert_eq!(
            &base[..16],
            &n_root[..16],
            "an unsealed predecessor tag must leave the high 16 bytes alone"
        );

        let n5 = chunk_nonce(&n_root, 5, &UNSEALED);
        assert_eq!(&n5[16..24], &[0, 0, 0, 0, 0, 0, 0, 5]);
        assert_eq!(&n5[..16], &n_root[..16]);

        let n6 = chunk_nonce(&n_root, 6, &UNSEALED);
        assert_ne!(n5.as_slice(), n6.as_slice());

        // Indices differing only in the top bits still differ, since i is 8 bytes wide.
        assert_ne!(
            chunk_nonce(&n_root, 1u64 << 40, &UNSEALED).as_slice(),
            chunk_nonce(&n_root, 1u64 << 41, &UNSEALED).as_slice()
        );

        // **The tag lands in the high half and the index in the low half, and neither bleeds.**
        // The `n_root[..16]` comparisons above only hold for a zero tag, so this is the half
        // that would stay untested if the chaining term were wired to the wrong 8 bytes -- and
        // a nonce that is *consistently* wrong on both sides opens every container correctly.
        let tag = std::array::from_fn(|i| 0xF0u8 ^ i as u8);
        let chained = chunk_nonce(&n_root, 5, &tag);
        for (i, (&a, &b)) in n_root[..16].iter().zip(tag.iter()).enumerate() {
            assert_eq!(chained[i], a ^ b, "byte {i} of the tag was not XORed in");
        }
        assert_eq!(
            &chained[16..24],
            &n5[16..24],
            "the tag must not disturb the index's half"
        );
        // A one-bit change in the tag changes the nonce.
        let mut other = tag;
        other[7] ^= 0x80;
        assert_ne!(
            chunk_nonce(&n_root, 5, &other).as_slice(),
            chained.as_slice()
        );
    }

    /// The predecessor tag's influence on the nonce is a **bijection**, so no two distinct
    /// predecessors of one index can land on the same nonce.
    ///
    /// This is the property the whole scheme rests on, and it is cheap to state and cheap to
    /// check: XOR by a constant is invertible, so distinct inputs give distinct outputs.
    /// Every one-bit neighbourhood of a fixed tag is walked, which covers 16 × 8 distinct
    /// predecessors rather than the 128 a single-bit sweep would give.
    #[test]
    fn a_distinct_predecessor_tag_always_gives_a_distinct_nonce() {
        let base = [0x37u8; TAG_LEN];
        let mut seen = std::collections::HashSet::new();
        let mut count = 0usize;
        for byte in 0..TAG_LEN {
            for bit in 0..8u8 {
                let mut t = base;
                t[byte] ^= 1u8 << bit;
                let n = chunk_nonce(&NONCE, 11, &t);
                assert!(
                    seen.insert(n.as_slice().to_vec()),
                    "two distinct predecessor tags collided at index 11 (byte {byte} bit {bit})"
                );
                count += 1;
            }
        }
        assert_eq!(seen.len(), count, "expected {count} distinct nonces");
        assert_eq!(count, TAG_LEN * 8);
    }

    /// Over-long plaintext is refused rather than truncating silently.
    #[test]
    fn over_long_plaintext_is_refused() {
        let mut slot = vec![0u8; CHUNK_SLOT as usize];
        let too_big = vec![0u8; CHUNK_PLAINTEXT + 1];
        assert_eq!(
            seal_chunk(&KEY, &NONCE, 0, &UNSEALED, &too_big, &mut slot),
            Err(AeadError::PlaintextTooLarge {
                len: CHUNK_PLAINTEXT + 1
            })
        );
    }

    /// Every chunk in the container must get a distinct nonce, **at the same predecessor tag**.
    ///
    /// The tag is pinned to one value here on purpose: varying it is what
    /// `a_distinct_predecessor_tag_always_gives_a_distinct_nonce` does, and mixing the two axes
    /// in one test means a failure does not say which half broke. This one is the "the counter
    /// still separates chunks" half, and it holds even for an entirely unsealed container,
    /// which is the degenerate worst case for the chaining term.
    #[test]
    fn no_two_chunk_indices_share_a_nonce() {
        let n_root = NONCE;
        let mut seen = std::collections::HashSet::new();
        for i in 0..crate::layout::MAX_CHUNKS {
            assert!(seen.insert(chunk_nonce(&n_root, i, &UNSEALED).as_slice().to_vec()));
        }
        assert_eq!(seen.len() as u64, crate::layout::MAX_CHUNKS);
    }

    /// ...and the two halves are independent, so a **rewritten** slot at index `i` cannot land
    /// on the nonce of a *different* chunk `j` either. The counter is checked against the whole
    /// addressable range above; this is the statement that the tag does not weaken it.
    #[test]
    fn a_chained_nonce_still_separates_every_index() {
        let n_root = NONCE;
        // One fixed, non-zero predecessor tag, then every index.
        let tag = [0xC7u8; TAG_LEN];
        let mut seen = std::collections::HashSet::new();
        for i in 0..crate::layout::MAX_CHUNKS {
            assert!(
                seen.insert(chunk_nonce(&n_root, i, &tag).as_slice().to_vec()),
                "chaining the tag collapsed two indices ({i})"
            );
        }
        assert_eq!(seen.len() as u64, crate::layout::MAX_CHUNKS);
    }
}
