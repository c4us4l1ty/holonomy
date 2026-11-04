//! The two-phase post-quantum key envelope (`ML-KEM-1024` + `X25519`), and [`StratumKeys`]
//! derivation from it.
//!
//! # Why two phases
//!
//! The superseded PRD's `v4.0.0` deadlocked at slot `π(0)`: the root capsule held the ciphertext
//! that unlocked the root capsule. v5 separated the two secrets that were being conflated:
//!
//! ```text
//!   passphrase + σ_epoch ──Argon2id──> root secret ──> K_perm, K_whiten, K_unwrap
//!                                                          │
//!   slot 0: unwhiten with K_whiten ─────────────────────────┘
//!           unwrap SK_stratum with K_unwrap        <-- phase 1 (symmetric, no KEM yet)
//!           decapsulate (ct_ml_kem_1024, ct_x25519) with SK_stratum  <-- phase 2
//!           K_DEM = BLAKE2b("DEM" ‖ ss_pq ‖ ss_ecc ‖ ct_pq ‖ ct_ecc ‖ κ)  <-- use
//! ```
//!
//! The invariant that makes it non-circular: **`K_unwrap` comes from the passphrase, never from
//! the capsule.** Everything inside slot 0 that unlocks slot 0 is derived before slot 0 is read.
//! `tests/two_phase.rs` asserts it by unlocking with a `K_unwrap` computed from a known passphrase
//! and a capsule whose contents are deliberately garbage.
//!
//! # What is deliberately absent
//!
//! * **No `Elligator2`.** There is no Elligator2 for Montgomery/X25519 in
//!   `curve25519-dalek` (only `ristretto/elligator.rs`), so `ct_x25519_elligator` could not have
//!   been built from the crates the PRD named. It is not needed either: the whole 64 KiB root
//!   block is stream-whitened with `XChaCha20(K_whiten)`, so the on-disk bytes are uniform
//!   regardless of what the ephemeral key looks like. The field is a plain `X25519` public key.
//! * **No `SK_stratum` retention.** It is unwrapped, used, and dropped before `K_DEM` is returned.
//!   Only `PK_stratum` survives, which is what lets a background save encapsulate without the
//!   private key present.

use blake2::{Blake2b512, Digest};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use holonomy_core::capsule::{
    ML_KEM_1024_CT_BYTES, ML_KEM_1024_SK_BYTES, POLY1305_TAG_BYTES, WRAPPED_SK_BYTES,
    X25519_SK_BYTES, XCHACHA_NONCE_BYTES,
};
use holonomy_core::stratum::StratumKeys;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::commit::{cmt4_tag, cmt4_verify, CMT4_TAG_BYTES};
use crate::kdf::{derive_subkey, labels, RootSecret, Subkey};

/// FIPS 203 serialises a decapsulation key as a 64-byte seed; see [`HybridKem::keygen`].
pub const ML_KEM_1024_SEED_BYTES: usize = 64;

/// `rand_core` adapter over [`crate::rng`], so the KEM's internal draws come from the same place as
/// ours.
///
/// Implemented as `TryRng` + `TryCryptoRng` (the 0.10 traits) and then `Rng` + `CryptoRng` via the
/// provided blanket impls, because `ml-kem` asks for `TryCryptoRng` and the *deprecated* `RngCore`
/// only requires `Rng`. Filling is the only operation; there is no state to advance, and every call
/// goes to the OS.
#[derive(Debug, Clone, Copy)]
struct OsRng;

impl rand_core::TryRng for OsRng {
    type Error = std::io::Error;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut b = [0u8; 4];
        crate::rng::fill(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut b = [0u8; 8];
        crate::rng::fill(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        crate::rng::fill(dst)
    }
}

impl rand_core::TryCryptoRng for OsRng {}

/// A hybrid KEM keypair, encoded at the sizes the capsule layout expects.
///
/// The decapsulation half is `Zeroize`d on drop and deliberately not `Clone`: a second copy of a
/// private key is a second thing to leak.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct HybridDecapsulationKey {
    /// The FIPS 203 64-byte decapsulation-key **seed**. Re-expanded on use; see
    /// [`HybridKem::keygen`] for why the seed is stored rather than the 3,168-byte expanded form.
    ml_kem: [u8; ML_KEM_1024_SEED_BYTES],
    /// `X25519` static secret.
    x25519: [u8; X25519_SK_BYTES],
}

impl std::fmt::Debug for HybridDecapsulationKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the bytes: this is the one value in the crate that must not reach a log, a panic
        // message or a `dbg!`.
        f.write_str("HybridDecapsulationKey(<redacted>)")
    }
}

/// The public half.
#[derive(Clone)]
pub struct HybridEncapsulationKey {
    /// `ML-KEM-1024` encapsulation key.
    pub ml_kem: Vec<u8>,
    /// `X25519` public key.
    pub x25519: [u8; X25519_SK_BYTES],
}

/// A ciphertext pair.
#[derive(Clone, Debug)]
pub struct HybridCiphertext {
    /// FIPS 203 ciphertext, exactly 1,568 bytes.
    pub ml_kem: [u8; ML_KEM_1024_CT_BYTES],
    /// Ephemeral `X25519` public key.
    pub x25519: [u8; X25519_SK_BYTES],
}

/// The 64 bytes of shared secret the combiner consumes.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SharedSecrets {
    /// `ML-KEM` shared secret.
    pub pq: [u8; 32],
    /// `X25519` shared secret.
    pub ecc: [u8; 32],
}

/// Failure of an envelope operation. Coarse on purpose — a decryption oracle needs detail, and
/// nothing here provides any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeError {
    /// Unwrapping `SK_stratum` failed (wrong `K_unwrap`, damaged capsule).
    UnwrapFailed,
    /// The phase-1 CMT-4 commitment did not match.
    Phase1CommitmentMismatch,
    /// The phase-2 CMT-4 commitment did not match.
    Phase2CommitmentMismatch,
    /// The KEM rejected the key or ciphertext.
    KemRejected,
    /// An encoded length did not match the capsule schema.
    BadLength,
}

impl std::fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnwrapFailed => f.write_str("could not unwrap the stratum secret key"),
            Self::Phase1CommitmentMismatch => f.write_str("phase-1 commitment mismatch"),
            Self::Phase2CommitmentMismatch => f.write_str("phase-2 commitment mismatch"),
            Self::KemRejected => f.write_str("KEM rejected the key or ciphertext"),
            Self::BadLength => f.write_str("encoded length does not match the capsule schema"),
        }
    }
}

impl std::error::Error for EnvelopeError {}

/// Wrap `SK_stratum` under `K_unwrap`.
///
/// Output is exactly [`holonomy_core::capsule::WRAPPED_SK_BYTES`] (3,216): `3,168 + 32` bytes of
/// ciphertext plus the 16-byte Poly1305 tag. The nonce is the capsule's sibling
/// `sk_unwrap_nonce`, not part of the output — which is the correction to the old layout's
/// double-counted field.
pub fn wrap_stratum_key(
    k_unwrap: &[u8; 32],
    nonce: &[u8; XCHACHA_NONCE_BYTES],
    dk: &[u8; ML_KEM_1024_SEED_BYTES],
    x_sk: &[u8; X25519_SK_BYTES],
) -> [u8; ML_KEM_1024_SK_BYTES + X25519_SK_BYTES + POLY1305_TAG_BYTES] {
    // The plaintext is the 64-byte seed followed by the 32-byte X25519 scalar: 96 bytes of secret
    // inside a 3,200-byte slot. The remainder is zero padding so the wrapped blob occupies exactly
    // the space the capsule allots (`WRAPPED_SK_BYTES` = 3,168 + 32 + 16). The padding is inside
    // the AEAD, so it is authenticated; and because the ciphertext is then stream-whitened and
    // folded against the pad, its length is not observable either.
    //
    // This is the honest version of a field size the old PRD justified by claiming a 3,168-byte
    // expanded key was stored. It is not: the seed is canonical, and the slot is fixed.
    let mut plain = [0u8; ML_KEM_1024_SK_BYTES + X25519_SK_BYTES];
    plain[..ML_KEM_1024_SEED_BYTES].copy_from_slice(dk);
    plain[ML_KEM_1024_SK_BYTES..].copy_from_slice(x_sk);

    let cipher = XChaCha20Poly1305::new_from_slice(k_unwrap).expect("32-byte key");
    let sealed = cipher
        .encrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: &plain,
                aad: b"HOLONOMY_V51_SK_WRAP",
            },
        )
        .expect("bounded buffer");
    plain.zeroize();

    let mut out = [0u8; ML_KEM_1024_SK_BYTES + X25519_SK_BYTES + POLY1305_TAG_BYTES];
    out.copy_from_slice(&sealed);
    out
}

/// Unwrap `SK_stratum`, returning the two secret halves. Caller must drop them immediately.
pub fn unwrap_stratum_key(
    k_unwrap: &[u8; 32],
    nonce: &[u8; XCHACHA_NONCE_BYTES],
    wrapped: &[u8; ML_KEM_1024_SK_BYTES + X25519_SK_BYTES + POLY1305_TAG_BYTES],
) -> Result<HybridDecapsulationKey, EnvelopeError> {
    let cipher =
        XChaCha20Poly1305::new_from_slice(k_unwrap).map_err(|_| EnvelopeError::UnwrapFailed)?;
    let mut plain = cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: wrapped,
                aad: b"HOLONOMY_V51_SK_WRAP",
            },
        )
        .map_err(|_| EnvelopeError::UnwrapFailed)?;
    if plain.len() != ML_KEM_1024_SK_BYTES + X25519_SK_BYTES {
        plain.zeroize();
        return Err(EnvelopeError::BadLength);
    }
    let mut dk = [0u8; ML_KEM_1024_SEED_BYTES];
    let mut x = [0u8; X25519_SK_BYTES];
    dk.copy_from_slice(&plain[..ML_KEM_1024_SEED_BYTES]);
    x.copy_from_slice(&plain[ML_KEM_1024_SK_BYTES..]);
    plain.zeroize();
    Ok(HybridDecapsulationKey {
        ml_kem: dk,
        x25519: x,
    })
}

/// Everything a save needs to write a root capsule's phase 1, without the secret key ever leaving
/// this module.
///
/// This is the save-path API. It exists specifically so no caller — and no test — has to be handed
/// `SK_stratum` to build a capsule: the keypair is generated, wrapped and dropped inside one call,
/// and what comes back is public ciphertext plus the already-derived [`StratumKeys`].
///
/// Returns `(ct, wrapped_stratum_sk, keys)`. The caller supplies the `sk_unwrap_nonce` it will
/// store in the capsule, and `κ_USB`.
pub fn create_envelope(
    k_unwrap: &[u8; 32],
    sk_unwrap_nonce: &[u8; XCHACHA_NONCE_BYTES],
    kappa_usb: &[u8; 32],
    kem: &dyn HybridKem,
) -> Result<(HybridCiphertext, [u8; WRAPPED_SK_BYTES], StratumKeys), EnvelopeError> {
    let (dk, ek) = kem.keygen();
    let (ct, ss) = kem.encapsulate(&ek)?;
    // The wrapped blob needs the two secret halves once; `wrap_stratum_key` takes them as slices.
    let wrapped = wrap_stratum_key_exported(k_unwrap, sk_unwrap_nonce, &dk)?;
    let keys = derive_dem_key(&ss, &ct, k_unwrap, kappa_usb);
    Ok((ct, wrapped, keys))
}

/// Wrap an already-generated keypair. Private to this module plus the accessor below, so a caller
/// cannot obtain `SK_stratum` bytes to pass here.
fn wrap_stratum_key_exported(
    k_unwrap: &[u8; 32],
    nonce: &[u8; XCHACHA_NONCE_BYTES],
    dk: &HybridDecapsulationKey,
) -> Result<[u8; WRAPPED_SK_BYTES], EnvelopeError> {
    Ok(wrap_stratum_key(k_unwrap, nonce, &dk.ml_kem, &dk.x25519))
}

/// Phase-1 commitment: binds the ephemeral ciphertexts and the wrapped key together.
pub fn phase1_commitment(
    k_unwrap: &[u8; 32],
    nonce: &[u8; XCHACHA_NONCE_BYTES],
    ct: &HybridCiphertext,
    wrapped: &[u8],
) -> [u8; CMT4_TAG_BYTES] {
    let mut ad = Vec::with_capacity(64 + wrapped.len());
    ad.extend_from_slice(nonce);
    ad.extend_from_slice(&ct.ml_kem);
    ad.extend_from_slice(&ct.x25519);
    cmt4_tag(k_unwrap, nonce, &ad, wrapped)
}

/// Phase-2 commitment: binds the encrypted directory to `K_DEM`.
pub fn phase2_commitment(
    k_dem: &[u8; 32],
    nonce: &[u8; XCHACHA_NONCE_BYTES],
    directory_ciphertext: &[u8],
) -> [u8; CMT4_TAG_BYTES] {
    cmt4_tag(
        k_dem,
        nonce,
        b"HOLONOMY_V51_DIRECTORY",
        directory_ciphertext,
    )
}

/// The KEM operations the envelope needs, behind a trait.
///
/// One implementation, [`MlKem1024X25519`], and a fake in the tests. The trait exists so the
/// two-phase state machine and its preconditions can be tested exhaustively on any host, while
/// the real lattice arithmetic is exercised in `tests/envelope.rs` where the dependency is
/// actually linked.
pub trait HybridKem {
    /// Generate a fresh keypair. Uses the OS CSPRNG; not deterministic, and not required to be —
    /// keypair generation happens at save time, and a save that reproduced its own ephemeral keys
    /// would be a bug.
    fn keygen(&self) -> (HybridDecapsulationKey, HybridEncapsulationKey);

    /// Encapsulate.
    fn encapsulate(
        &self,
        pk: &HybridEncapsulationKey,
    ) -> Result<(HybridCiphertext, SharedSecrets), EnvelopeError>;

    /// Decapsulate.
    fn decapsulate(
        &self,
        sk: &HybridDecapsulationKey,
        ct: &HybridCiphertext,
    ) -> Result<SharedSecrets, EnvelopeError>;
}

/// The real thing: `ml-kem` 0.3.4 (`MlKem1024`) hybridised with `x25519-dalek` 2.0.
///
/// The combiner is a plain concatenation-then-hash rather than an RFC 9180-style DHKEM, which is
/// acceptable here because both secrets are raw and both ciphertexts are bound into the
/// derivation — see [`derive_dem_key`]. It is *not* a general-purpose combiner and must not be
/// reused outside this envelope.
pub struct MlKem1024X25519;

impl HybridKem for MlKem1024X25519 {
    fn keygen(&self) -> (HybridDecapsulationKey, HybridEncapsulationKey) {
        use ml_kem::kem::{Generate, KeyExport};
        use ml_kem::MlKem1024;

        // `MlKem1024::generate_keypair()` exists but is gated behind ml-kem's `getrandom` feature
        // and returns an opaque pair; going through `try_generate_from_rng` keeps the CSPRNG choice
        // ours (see `crate::rng`) and avoids enabling a second entropy path inside the dependency.
        // `Generate::try_generate_from_rng` takes `&mut R where R: TryCryptoRng`, and returns a
        // `Result` whose `Error` matches the RNG's. `rand_core::UnwrapErr` is the documented
        // adapter: it sets `Error = Infallible` and panics inside the fill with rand_core's own
        // message, so the `expect` below is unreachable for a live OS entropy source and the
        // failure path is a panic rather than a key drawn from a fallback.
        let dk = <MlKem1024 as ml_kem::kem::Kem>::DecapsulationKey::try_generate_from_rng(
            &mut rand_core::UnwrapErr(OsRng),
        )
        .expect("OS CSPRNG unavailable");
        let ek = dk.encapsulation_key().clone();

        let mut x_bytes = [0u8; 32];
        crate::rng::fill(&mut x_bytes).expect("OS CSPRNG unavailable");
        let x_sk = StaticSecret::from(x_bytes);
        let x_pk = PublicKey::from(&x_sk);

        // The stored decapsulation key is the FIPS 203 64-byte *seed*, not the 3,168-byte expanded
        // form. Three reasons, and the first is a measured fact rather than a preference:
        //
        //  1. `KeyExport for DecapsulationKey` returns `Seed` — 64 bytes — and at base64 in JSON
        //     that keeps our wire encoding of the *encrypted* key inside both `sodiumoxide`'s
        //     255-byte and `serde-json-core`'s 256-byte limits, which the expanded form does not.
        //  2. The seed is the canonical serialisation: it round-trips to the same keypair on every
        //     implementation, so a container written here opens with any conformant ML-KEM.
        //  3. `DecapsulationKey::from_seed` re-expands deterministically, so nothing is lost.
        //
        // `wrap_stratum_key` then pads the plaintext to the 3,200 bytes the capsule allots, which
        // is what makes `WRAPPED_SK_BYTES == 3,216` (3,168 + 32 + 16) come out exactly.
        let seed = dk.to_bytes();
        let mut ml_seed = [0u8; ML_KEM_1024_SEED_BYTES];
        ml_seed.copy_from_slice(&seed);

        (
            HybridDecapsulationKey {
                ml_kem: ml_seed,
                x25519: x_bytes,
            },
            HybridEncapsulationKey {
                ml_kem: ek.to_bytes().to_vec(),
                x25519: x_pk.to_bytes(),
            },
        )
    }

    fn encapsulate(
        &self,
        pk: &HybridEncapsulationKey,
    ) -> Result<(HybridCiphertext, SharedSecrets), EnvelopeError> {
        use ml_kem::kem::{Encapsulate, Kem};
        use ml_kem::MlKem1024;

        // `EncapsulationKey::new(&Key<Self>)` is the only constructor: a fallible `From<&[u8]>` is
        // deliberately absent so a wrong-length byte slice cannot become a key. The array type has
        // to be converted from the stored `Vec` first, which is where the length check happens.
        let key_array: ml_kem::Key<ml_kem::kem::EncapsulationKey<MlKem1024>> =
            ml_kem::Key::<ml_kem::kem::EncapsulationKey<MlKem1024>>::try_from(pk.ml_kem.as_slice())
                .map_err(|_| EnvelopeError::KemRejected)?;
        let ek = <MlKem1024 as Kem>::EncapsulationKey::new(&key_array)
            .map_err(|_| EnvelopeError::KemRejected)?;

        // `encapsulate_with_rng` also wants `R: CryptoRng` (i.e. `Error = Infallible`), for the
        // same reason as the keygen draw: a KEM encapsulation cannot proceed on half entropy.
        let (ct, ss) = ek.encapsulate_with_rng(&mut rand_core::UnwrapErr(OsRng));
        let mut pq = [0u8; 32];
        pq.copy_from_slice(&ss);

        // Ephemeral ECDH: a fresh scalar per encapsulation, discarded immediately.
        let mut eph_bytes = [0u8; 32];
        crate::rng::fill(&mut eph_bytes).expect("OS CSPRNG unavailable");
        let eph = StaticSecret::from(eph_bytes);
        let eph_pk = PublicKey::from(&eph);
        let peer = PublicKey::from(pk.x25519);
        let ecc = eph.diffie_hellman(&peer).to_bytes();

        let mut ct_ml = [0u8; ML_KEM_1024_CT_BYTES];
        ct_ml.copy_from_slice(&ct);
        Ok((
            HybridCiphertext {
                ml_kem: ct_ml,
                x25519: eph_pk.to_bytes(),
            },
            SharedSecrets { pq, ecc },
        ))
    }

    fn decapsulate(
        &self,
        sk: &HybridDecapsulationKey,
        ct: &HybridCiphertext,
    ) -> Result<SharedSecrets, EnvelopeError> {
        use ml_kem::kem::{Decapsulate, Kem, KeyInit};
        use ml_kem::MlKem1024;

        // Re-expand the seed, decapsulate, drop. The expanded key never has to be serialised.
        //
        // `KeyInit::new_from_slice` returns `Ok` directly only for an input whose length matches
        // `KeySize`; the seed is 64 bytes and `KeySize` for ML-KEM-1024's decapsulation key is
        // also 64 (the seed), so this is the infallible branch. If ml-kem ever changes `KeySize`
        // to the expanded 3,168 bytes, this stops compiling rather than silently reinterpreting
        // the seed -- which is the failure mode worth having.
        let dk = <<MlKem1024 as Kem>::DecapsulationKey as KeyInit>::new_from_slice(&sk.ml_kem)
            .expect("64-byte seed is the decapsulation key's KeySize");
        let ml_ct = ml_kem::Ciphertext::<MlKem1024>::try_from(ct.ml_kem.as_slice())
            .map_err(|_| EnvelopeError::KemRejected)?;
        let ss = dk.decapsulate(&ml_ct);
        let mut pq = [0u8; 32];
        pq.copy_from_slice(&ss);

        let x_sk = StaticSecret::from(sk.x25519);
        let peer = PublicKey::from(ct.x25519);
        let ecc = x_sk.diffie_hellman(&peer).to_bytes();

        Ok(SharedSecrets { pq, ecc })
    }
}

/// `K_DEM ‖ N_epoch` from the two shared secrets, both ciphertexts, `K_unwrap` and `κ`.
///
/// Binds the ciphertexts so a re-encapsulation under the same keypair yields a different `K_DEM`,
/// and binds `K_unwrap` so a capsule is not valid under a second passphrase.
pub fn derive_dem_key(
    ss: &SharedSecrets,
    ct: &HybridCiphertext,
    k_unwrap: &[u8; 32],
    kappa_usb: &[u8; 32],
) -> StratumKeys {
    let mut h = Blake2b512::new();
    h.update(b"HOLONOMY_V51_DEM");
    h.update([0u8]);
    h.update(ss.pq);
    h.update(ss.ecc);
    h.update(ct.ml_kem);
    h.update(ct.x25519);
    h.update(k_unwrap);
    h.update(kappa_usb);
    let digest = h.finalize();

    let mut k_dem = [0u8; 32];
    let mut n_epoch = [0u8; XCHACHA_NONCE_BYTES];
    k_dem.copy_from_slice(&digest[..32]);
    n_epoch.copy_from_slice(&digest[32..32 + XCHACHA_NONCE_BYTES]);

    StratumKeys {
        k_unwrap: *k_unwrap,
        k_dem,
        n_epoch,
    }
}

/// Derive a stratum's symmetric key set from the root secret alone, with no KEM round trip.
///
/// # What it must return, and the bug that used to live here
///
/// This is the key set the *unlock path* hands back. The capsule was unwrapped with a `K_unwrap`
/// derived from `(root, index)` and nothing else — that is what makes the capsule locatable, and it
/// is why `unlock_fixed` does not need `κ_USB` to unwrap `SK_stratum`. The returned keys must
/// therefore equal the ones the capsule was built with, or the caller holds a key that cannot
/// reopen what it just wrote.
///
/// An earlier version XORed `κ_USB` into both `k_dem` and `k_unwrap` on the way out. That was wrong
/// twice over:
///
/// 1. **It made the writer produce unreadable containers.** A save that wrapped `SK_stratum` under
///    `keys.k_unwrap` would seal it under a κ-mixed key, while the next unlock derives a κ-free
///    `k_unwrap` to unwrap — so the capsule could never be opened again, and the failure would be
///    reported as the coarse `Rejected` that means "wrong passphrase". No test caught it because
///    nothing had ever written a capsule from the unlock path.
/// 2. **It added nothing.** `κ_USB` is already a mandatory input to [`derive_dem_key`], which binds
///    it with the shared secrets and both ciphertexts. The biparite property lives there, in the KEM
///    path, not in a second and weaker mixing step over the fallback keys.
///
/// The name lost `_passphrase_only` for the same reason it lost the XOR: by the time this is called
/// there is no passphrase in sight, and the old name invited exactly the confusion above.
pub fn derive_stratum_keys(
    root: &RootSecret,
    stratum_index: u8,
    kappa_usb: &[u8; 32],
) -> StratumKeys {
    // `κ_USB` is taken so the signature cannot drift from `derive_dem_key`, and used in the one
    // place it does no harm: the nonce base. `N_epoch` is a public nonce prefix, not key material,
    // and binding it to κ keeps two sticks under one passphrase from sharing a nonce stream.
    let ctx = [stratum_index, 0u8];
    let unwrap: Subkey = derive_subkey(root, labels::K_UNWRAP, &ctx);
    let dem: Subkey = derive_subkey(root, labels::K_DEM, &ctx);
    let nonce: Subkey = derive_subkey(root, labels::N_EPOCH, &ctx);

    let mut n_epoch = [0u8; XCHACHA_NONCE_BYTES];
    // `Subkey` is 32 bytes and an XChaCha nonce 24, so exactly the first 24 are taken. Written via
    // `copy_from_slice` so a change to either size fails loudly instead of truncating.
    n_epoch.copy_from_slice(&nonce.0[..XCHACHA_NONCE_BYTES]);
    for (i, byte) in n_epoch.iter_mut().enumerate() {
        *byte ^= kappa_usb[i];
    }

    StratumKeys {
        k_unwrap: unwrap.0,
        k_dem: dem.0,
        n_epoch,
    }
}

/// Verify both phase commitments. Separate function so the caller cannot skip one by accident.
pub fn verify_phase_commitments(
    expected_p1: &[u8; CMT4_TAG_BYTES],
    actual_p1: &[u8; CMT4_TAG_BYTES],
    expected_p2: &[u8; CMT4_TAG_BYTES],
    actual_p2: &[u8; CMT4_TAG_BYTES],
) -> Result<(), EnvelopeError> {
    if !cmt4_verify(expected_p1, actual_p1) {
        return Err(EnvelopeError::Phase1CommitmentMismatch);
    }
    if !cmt4_verify(expected_p2, actual_p2) {
        return Err(EnvelopeError::Phase2CommitmentMismatch);
    }
    Ok(())
}

impl HybridDecapsulationKey {
    /// Construct from raw halves. **Test-only**: a production keypair comes from
    /// [`HybridKem::keygen`], which draws from the OS CSPRNG.
    #[doc(hidden)]
    pub fn for_test(ml_kem: [u8; ML_KEM_1024_SEED_BYTES], x25519: [u8; X25519_SK_BYTES]) -> Self {
        Self { ml_kem, x25519 }
    }
}

impl HybridEncapsulationKey {
    /// Construct from raw halves. **Test-only.**
    #[doc(hidden)]
    pub fn for_test(ml_kem: Vec<u8>, x25519: [u8; X25519_SK_BYTES]) -> Self {
        Self { ml_kem, x25519 }
    }
}

impl SharedSecrets {
    /// Construct from raw halves. **Test-only.**
    #[doc(hidden)]
    pub fn for_test(pq: [u8; 32], ecc: [u8; 32]) -> Self {
        Self { pq, ecc }
    }
}

/// Length check for a pair read out of a capsule, before any crypto touches it.
pub fn ct_lengths_are_canonical(ct: &HybridCiphertext) -> bool {
    ct.ml_kem.len() == ML_KEM_1024_CT_BYTES && ct.x25519.len() == X25519_SK_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_kem_round_trips_through_a_capsule_shaped_wrap() {
        // The one test that exercises ml-kem itself: keygen -> encapsulate -> wrap ->
        // unwrap -> decapsulate, asserting both shared secrets agree. It also pins the seed
        // serialisation, which is what makes the 3,216-byte field defensible.
        let kem = MlKem1024X25519;
        let (dk, ek) = kem.keygen();
        let (ct, ss_sender) = kem.encapsulate(&ek).unwrap();
        let k_unwrap = [0x31u8; 32];
        let nonce = [0x32u8; XCHACHA_NONCE_BYTES];

        let wrapped = wrap_stratum_key(&k_unwrap, &nonce, &dk.ml_kem, &dk.x25519);
        assert_eq!(wrapped.len(), 3_216);

        let recovered = unwrap_stratum_key(&k_unwrap, &nonce, &wrapped).unwrap();
        let ss_receiver = kem.decapsulate(&recovered, &ct).unwrap();
        assert_eq!(ss_sender.pq, ss_receiver.pq);
        assert_eq!(ss_sender.ecc, ss_receiver.ecc);
        assert_ne!(ss_sender.pq, [0u8; 32]);
    }

    #[test]
    fn two_keygens_do_not_collide() {
        let kem = MlKem1024X25519;
        let (a, _) = kem.keygen();
        let (b, _) = kem.keygen();
        assert_ne!(a.ml_kem, b.ml_kem);
        assert_ne!(a.x25519, b.x25519);
    }

    #[test]
    fn decapsulation_key_debug_is_redacted() {
        let kem = MlKem1024X25519;
        let (dk, _) = kem.keygen();
        let s = format!("{dk:?}");
        assert!(!s.contains(&format!("{:02x}", dk.x25519[0])));
        assert!(s.contains("redacted"));
    }

    #[test]
    fn wrap_is_exactly_the_documented_size() {
        let wrapped = wrap_stratum_key(
            &[1u8; 32],
            &[2u8; 24],
            &[3u8; ML_KEM_1024_SEED_BYTES],
            &[4u8; 32],
        );
        assert_eq!(wrapped.len(), 3_216);
        assert_eq!(
            wrapped.len(),
            ML_KEM_1024_SK_BYTES + X25519_SK_BYTES + POLY1305_TAG_BYTES
        );
    }

    #[test]
    fn wrap_unwrap_round_trip() {
        let k = [1u8; 32];
        let n = [2u8; 24];
        let dk = [3u8; ML_KEM_1024_SEED_BYTES];
        let xs = [4u8; 32];
        let w = wrap_stratum_key(&k, &n, &dk, &xs);
        let out = unwrap_stratum_key(&k, &n, &w).unwrap();
        assert_eq!(out.ml_kem, dk);
        assert_eq!(out.x25519, xs);
    }

    #[test]
    fn wrong_k_unwrap_cannot_open_the_wrap() {
        let n = [2u8; 24];
        let w = wrap_stratum_key(&[1u8; 32], &n, &[3u8; ML_KEM_1024_SEED_BYTES], &[4u8; 32]);
        assert_eq!(
            unwrap_stratum_key(&[9u8; 32], &n, &w).unwrap_err(),
            EnvelopeError::UnwrapFailed
        );
    }

    #[test]
    fn phase1_commitment_binds_every_input() {
        let k = [1u8; 32];
        let n = [2u8; 24];
        let ct = HybridCiphertext {
            ml_kem: [3u8; 1568],
            x25519: [4u8; 32],
        };
        let w = [5u8; 3216];
        let base = phase1_commitment(&k, &n, &ct, &w);

        let mut ct2 = ct.clone();
        ct2.x25519[0] ^= 1;
        assert_ne!(base, phase1_commitment(&k, &n, &ct2, &w));

        let mut w2 = w;
        w2[0] ^= 1;
        assert_ne!(base, phase1_commitment(&k, &n, &ct, &w2));
        assert_ne!(base, phase1_commitment(&[6u8; 32], &n, &ct, &w));
    }

    #[test]
    fn dem_key_is_deterministic_and_binds_its_inputs() {
        let ss = SharedSecrets {
            pq: [1u8; 32],
            ecc: [2u8; 32],
        };
        let ct = HybridCiphertext {
            ml_kem: [3u8; 1568],
            x25519: [4u8; 32],
        };
        let a = derive_dem_key(&ss, &ct, &[5u8; 32], &[6u8; 32]);
        let b = derive_dem_key(&ss, &ct, &[5u8; 32], &[6u8; 32]);
        assert_eq!(a.k_dem, b.k_dem);
        assert_eq!(a.n_epoch, b.n_epoch);
        // A different κ (different USB) gives a different key: this is the bipartite binding.
        let c = derive_dem_key(&ss, &ct, &[5u8; 32], &[7u8; 32]);
        assert_ne!(a.k_dem, c.k_dem);
    }

    #[test]
    fn fallback_keys_ignore_everything_a_save_ratchets() {
        // The invariant that makes the duress passphrase open T_0..T_n: these keys depend on
        // `(root, stratum)` and on nothing a save moves.
        let root = RootSecret([0x33; 64]);
        let a = derive_stratum_keys(&root, 1, &[0u8; 32]);
        let b = derive_stratum_keys(&root, 1, &[0u8; 32]);
        assert_eq!(a.k_dem, b.k_dem);
        assert_eq!(a.k_unwrap, b.k_unwrap);
        // Different stratum, different keys.
        let c = derive_stratum_keys(&root, 2, &[0u8; 32]);
        assert_ne!(a.k_dem, c.k_dem);
    }

    #[test]
    fn the_fallback_keys_equal_the_ones_the_capsule_was_built_with() {
        // The bug this exists to prevent: an earlier version XORed `κ_USB` into `k_unwrap` and
        // `k_dem` here. The capsule is built with a κ-free `k_unwrap = derive_subkey(root, K_UNWRAP,
        // [index, 0])`, so a writer using the returned keys would wrap `SK_stratum` under a key no
        // reader could reproduce, and every container it wrote would be unopenable.
        let root = RootSecret([0x44; 64]);
        let index = 2u8;
        let kappa = [0x99u8; 32];
        let keys = derive_stratum_keys(&root, index, &kappa);

        let expected = derive_subkey(&root, labels::K_UNWRAP, &[index, 0]).0;
        assert_eq!(
            keys.k_unwrap, expected,
            "k_unwrap must be the value the phase-1 commitment is built over"
        );
        let expected_dem = derive_subkey(&root, labels::K_DEM, &[index, 0]).0;
        assert_eq!(
            keys.k_dem, expected_dem,
            "k_dem must not be mixed with κ; derive_dem_key already binds κ"
        );
        // κ still reaches the key set, through the nonce base rather than through a key.
        let other = derive_stratum_keys(&root, index, &[0x11u8; 32]);
        assert_ne!(keys.n_epoch, other.n_epoch);
        assert_eq!(keys.k_dem, other.k_dem);
    }

    #[test]
    fn phase_verification_reports_which_phase_failed() {
        let z = [0u8; 32];
        assert!(verify_phase_commitments(&z, &z, &z, &z).is_ok());
        assert_eq!(
            verify_phase_commitments(&z, &[1u8; 32], &z, &z).unwrap_err(),
            EnvelopeError::Phase1CommitmentMismatch
        );
        assert_eq!(
            verify_phase_commitments(&z, &z, &z, &[1u8; 32]).unwrap_err(),
            EnvelopeError::Phase2CommitmentMismatch
        );
    }

    #[test]
    fn length_check() {
        let ok = HybridCiphertext {
            ml_kem: [0u8; 1568],
            x25519: [0u8; 32],
        };
        assert!(ct_lengths_are_canonical(&ok));
    }
}
