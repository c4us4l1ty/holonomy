//! The two-phase post-quantum hybrid envelope and its capsule, ported from v5.1.0's audited
//! `holonomy-crypto::envelope` and re-scoped for v6:
//!
//! ```text
//!   passphrase (+ optional USB slot, one path) ──Argon2id──> root secret ──> K_unwrap
//!        │
//!        └──> capsule: unwrap SK with K_unwrap            <-- phase 1 (symmetric only)
//!              decapsulate (ct_ml_kem_1024, ct_x25519)    <-- phase 2 (PQ hybrid)
//!              K_DEM = BLAKE2b("V6_DEM" ‖ ss_pq ‖ ss_ecc ‖ cts ‖ K_unwrap ‖ usb-flag ‖ usb)
//! ```
//!
//! # The invariant that makes it non-circular
//!
//! **`K_unwrap` comes from the KDF, never from the capsule.** Whatever is inside the capsule
//! that unlocks the capsule is derived *before* the capsule is read — v4's deadlock at slot
//! `π(0)` (K-3 on the audit wall) was precisely the failure to keep that separation. The
//! `unlock_garbage_with_known_key` test asserts it: a capsule of waste bytes under a correct
//! passphrase fails at the phase-1 commitment, never at "the payload was wrong".
//!
//! # What is deliberately absent from the v5 vintage
//!
//! * **No `Ω`, no whitening, no pad folding** — those were container-placement concerns; Block
//!   3 owns placement and there is no mandatory pad anywhere in v6.
//! * **No strata** — one world-line per file in v6; duress strata are explicitly deferred past
//!   6.0 (ROADMAP, "beyond the horizon").
//! * **No Elligator2** — as v5 recorded: there is no Elligator2 for Montgomery/X25519 in
//!   curve25519-dalek, and v6 does not need indistinguishability of a public key field that
//!   sits inside an *opaque sealed slot* anyway.

use blake2::{Blake2b512, Digest};
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{Tag, XChaCha20Poly1305, XNonce};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::capability::{DEMKey, UnwrapKey};
use crate::commit::{cmt4_tag, cmt4_verify, CMT4_TAG_BYTES, NONCE_BYTES, POLY1305_TAG_BYTES};

/// FIPS 203 serializes an ML-KEM decapsulation key as a 64-byte seed (re-expanded on use).
pub const ML_KEM_1024_SEED_BYTES: usize = 64;

/// ML-KEM-1024 ciphertext length.
pub const ML_KEM_1024_CT_BYTES: usize = 1_568;

/// X25519 secret/public key length.
pub const X25519_SK_BYTES: usize = 32;

/// The padded plaintext a wrapped hybrid key occupies: 96 secret bytes (64+32) zero-padded
/// inside the AEAD to the fixed slot, so its length discloses nothing about its contents.
pub const WRAPPED_SK_PLAINTEXT_BYTES: usize = 3_200;

/// On-wire length of the wrapped key: plaintext plus the Poly1305 tag.
pub const WRAPPED_SK_BYTES: usize = WRAPPED_SK_PLAINTEXT_BYTES + POLY1305_TAG_BYTES;

/// A hybrid KEM decapsulation key. `Zeroize`d on drop, not `Clone`: a second copy of a private
/// key is a second thing to leak. `Debug` prints the word "redacted", never bytes.
#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct HybridDecapsulationKey {
    pub(crate) ml_kem: [u8; ML_KEM_1024_SEED_BYTES],
    pub(crate) x25519: [u8; X25519_SK_BYTES],
}

impl core::fmt::Debug for HybridDecapsulationKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("HybridDecapsulationKey(<redacted>)")
    }
}

/// The public half of a hybrid keypair.
#[derive(Clone)]
pub(crate) struct HybridEncapsulationKey {
    /// ML-KEM-1024 encapsulation key.
    ml_kem: Vec<u8>,
    /// X25519 public key.
    x25519: [u8; X25519_SK_BYTES],
}

/// A hybrid ciphertext pair.
#[derive(Clone, Debug)]
pub struct HybridCiphertext {
    /// FIPS 203 ciphertext, exactly 1,568 bytes.
    pub ml_kem: [u8; ML_KEM_1024_CT_BYTES],
    /// Ephemeral X25519 public key.
    pub x25519: [u8; X25519_SK_BYTES],
}

/// The 64 bytes of shared secret the combiner consumes. Zeroized on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct SharedSecrets {
    /// ML-KEM shared secret.
    pub(crate) pq: [u8; 32],
    /// X25519 shared secret.
    pub(crate) ecc: [u8; 32],
}

/// Envelope-operation failure. Coarse on purpose: a decryption oracle needs vocabulary, and
/// nothing here provides any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeError {
    /// Unwrapping `SK` failed (wrong `K_unwrap`, tampered bytes).
    UnwrapFailed,
    /// The phase-1 CMT-4 commitment did not match — checked before any key material is used.
    Phase1CommitmentMismatch,
    /// The KEM rejected the key or ciphertext.
    KemRejected,
    /// An encoded length did not match the schema.
    BadLength,
}

impl core::fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnwrapFailed => f.write_str("could not unwrap the decapsulation key"),
            Self::Phase1CommitmentMismatch => f.write_str("phase-1 commitment mismatch"),
            Self::KemRejected => f.write_str("KEM rejected the key or ciphertext"),
            Self::BadLength => f.write_str("encoded length does not match the capsule schema"),
        }
    }
}

impl std::error::Error for EnvelopeError {}

/// A capsule: everything needed at open time, in one fixed-size record. What the world-line
/// stores at its head (Block 3); what `open_capsule` consumes.
#[derive(Clone)]
pub struct Capsule {
    /// The hybrid ciphertexts created at the last save.
    pub ct: HybridCiphertext,
    /// `SK` wrapped under `K_unwrap` with [`Self::sk_unwrap_nonce`].
    pub wrapped_sk: [u8; WRAPPED_SK_BYTES],
    /// Nonce of the `SK` wrap. Not secret.
    pub sk_unwrap_nonce: [u8; NONCE_BYTES],
    /// Phase-1 commitment over (nonce ‖ ct), computed with `K_unwrap` — verified first, so a
    /// wrong passphrase is a fast uniform "no" before any key material is touched.
    pub phase1: [u8; CMT4_TAG_BYTES],
}

impl Capsule {
    /// Fixed serialized length: ct(1,568+32) ‖ wrapped(3,216) ‖ nonce(24) ‖ phase1(32).
    pub const WIRE_BYTES: usize =
        ML_KEM_1024_CT_BYTES + X25519_SK_BYTES + WRAPPED_SK_BYTES + NONCE_BYTES + CMT4_TAG_BYTES;

    /// Serialize in field order. The layout is the contract; Block 3 writes it verbatim.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::WIRE_BYTES);
        out.extend_from_slice(&self.ct.ml_kem);
        out.extend_from_slice(&self.ct.x25519);
        out.extend_from_slice(&self.wrapped_sk);
        out.extend_from_slice(&self.sk_unwrap_nonce);
        out.extend_from_slice(&self.phase1);
        debug_assert_eq!(out.len(), Self::WIRE_BYTES);
        out
    }

    /// Parse a wire record. Wrong length is refused *before* any crypto touches the bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        if bytes.len() != Self::WIRE_BYTES {
            return Err(EnvelopeError::BadLength);
        }
        let mut pos = 0;
        let mut take = |n: usize| -> &[u8] {
            let s = &bytes[pos..pos + n];
            pos += n;
            s
        };
        let ml_kem: [u8; ML_KEM_1024_CT_BYTES] = take(ML_KEM_1024_CT_BYTES)
            .try_into()
            .map_err(|_| EnvelopeError::BadLength)?;
        let x25519: [u8; X25519_SK_BYTES] = take(X25519_SK_BYTES)
            .try_into()
            .map_err(|_| EnvelopeError::BadLength)?;
        let wrapped_sk: [u8; WRAPPED_SK_BYTES] = take(WRAPPED_SK_BYTES)
            .try_into()
            .map_err(|_| EnvelopeError::BadLength)?;
        let sk_unwrap_nonce: [u8; NONCE_BYTES] = take(NONCE_BYTES)
            .try_into()
            .map_err(|_| EnvelopeError::BadLength)?;
        let phase1: [u8; CMT4_TAG_BYTES] = take(CMT4_TAG_BYTES)
            .try_into()
            .map_err(|_| EnvelopeError::BadLength)?;
        Ok(Self {
            ct: HybridCiphertext { ml_kem, x25519 },
            wrapped_sk,
            sk_unwrap_nonce,
            phase1,
        })
    }
}

/// Wrap the hybrid decapsulation key under `K_unwrap`.
///
/// The plaintext is the 64-byte seed followed by the 32-byte X25519 scalar, zero-padded to
/// [`WRAPPED_SK_PLAINTEXT_BYTES`]; the padding is *inside* the AEAD, so it is authenticated,
/// and the fixed size means the record's length leaks nothing. Output is the ciphertext body
/// plus tag: exactly [`WRAPPED_SK_BYTES`].
pub(crate) fn wrap_key(
    k_unwrap: &[u8; 32],
    nonce: &[u8; NONCE_BYTES],
    dk: &HybridDecapsulationKey,
) -> [u8; WRAPPED_SK_BYTES] {
    let mut payload = Zeroizing::new([0u8; WRAPPED_SK_PLAINTEXT_BYTES]);
    payload[..ML_KEM_1024_SEED_BYTES].copy_from_slice(&dk.ml_kem);
    payload[ML_KEM_1024_SEED_BYTES..ML_KEM_1024_SEED_BYTES + X25519_SK_BYTES]
        .copy_from_slice(&dk.x25519);

    let cipher = XChaCha20Poly1305::new_from_slice(k_unwrap).expect("32-byte key");
    let tag = cipher
        .encrypt_in_place_detached(
            XNonce::from_slice(nonce),
            b"HOLONOMY_V6_SK_WRAP",
            &mut payload[..],
        )
        .expect("bounded buffer");

    let mut out = [0u8; WRAPPED_SK_BYTES];
    out[..WRAPPED_SK_PLAINTEXT_BYTES].copy_from_slice(&payload[..]);
    out[WRAPPED_SK_PLAINTEXT_BYTES..].copy_from_slice(tag.as_slice());
    out
}

/// Unwrap the hybrid decapsulation key. Failures are deliberately undifferentiated.
pub(crate) fn unwrap_key_bytes(
    k_unwrap: &[u8; 32],
    nonce: &[u8; NONCE_BYTES],
    wrapped: &[u8; WRAPPED_SK_BYTES],
) -> Result<HybridDecapsulationKey, EnvelopeError> {
    let cipher =
        XChaCha20Poly1305::new_from_slice(k_unwrap).map_err(|_| EnvelopeError::UnwrapFailed)?;
    let tag_offset = WRAPPED_SK_PLAINTEXT_BYTES;
    let mut plain = Zeroizing::new(wrapped[..tag_offset].to_vec());
    #[cfg(test)]
    crate::allocation_audit::watch(plain.as_ptr(), plain.len());
    let tag = Tag::from_slice(&wrapped[tag_offset..]);
    cipher
        .decrypt_in_place_detached(
            XNonce::from_slice(nonce),
            b"HOLONOMY_V6_SK_WRAP",
            &mut plain[..],
            tag,
        )
        .map_err(|_| EnvelopeError::UnwrapFailed)?;
    if plain.len() != WRAPPED_SK_PLAINTEXT_BYTES {
        return Err(EnvelopeError::BadLength);
    }
    let mut ml_seed = Zeroizing::new([0u8; ML_KEM_1024_SEED_BYTES]);
    let mut x25519 = Zeroizing::new([0u8; X25519_SK_BYTES]);
    ml_seed.copy_from_slice(&plain[..ML_KEM_1024_SEED_BYTES]);
    x25519
        .copy_from_slice(&plain[ML_KEM_1024_SEED_BYTES..ML_KEM_1024_SEED_BYTES + X25519_SK_BYTES]);
    Ok(HybridDecapsulationKey {
        ml_kem: *ml_seed,
        x25519: *x25519,
    })
}

/// Phase-1 commitment: binds nonce, both ciphertexts, and the wrapped key, checked before any
/// of their contents are used — the wrong-passphrase "no" is cheap, constant, and uniform.
pub(crate) fn phase1_commitment(
    k_unwrap: &[u8; 32],
    nonce: &[u8; NONCE_BYTES],
    ct: &HybridCiphertext,
    wrapped: &[u8; WRAPPED_SK_BYTES],
) -> [u8; CMT4_TAG_BYTES] {
    let mut ad = Vec::with_capacity(NONCE_BYTES + ML_KEM_1024_CT_BYTES + X25519_SK_BYTES);
    ad.extend_from_slice(nonce);
    ad.extend_from_slice(&ct.ml_kem);
    ad.extend_from_slice(&ct.x25519);
    cmt4_tag(k_unwrap, nonce, &ad, wrapped)
}

/// `K_DEM` from the two shared secrets, both ciphertexts, `K_unwrap`, and the USB slot.
///
/// Binding the ciphertexts means a re-encapsulation under the same keypair yields a different
/// key; binding `K_unwrap` means a capsule is only valid under one passphrase; binding the USB
/// slot's *presence flag and bytes* (never XORed, hashed — v5's unlock-destruction lesson)
/// means an armed stick participates in exactly one, deterministic, non-circular place.
pub(crate) fn derive_dem_key(
    ss: &SharedSecrets,
    ct: &HybridCiphertext,
    k_unwrap: &[u8; 32],
    usb: Option<&[u8; 32]>,
) -> DEMKey {
    let mut h = Blake2b512::new();
    h.update(b"HOLONOMY_V6_DEM");
    h.update([0u8]);
    h.update(ss.pq.as_slice());
    h.update(ss.ecc.as_slice());
    h.update(ct.ml_kem);
    h.update(ct.x25519);
    h.update(k_unwrap);
    h.update([u8::from(usb.is_some())]);
    if let Some(usb_bytes) = usb {
        h.update(usb_bytes);
    } else {
        h.update([0u8; 32]);
    }
    let mut digest = h.finalize();
    let mut k_dem = Zeroizing::new([0u8; 32]);
    k_dem.copy_from_slice(&digest[..32]);
    digest.as_mut_slice().zeroize();
    DEMKey::from_bytes(*k_dem)
}

/// The KEM operations the envelope needs, behind a trait so the state machine can be tested
/// exhaustively without lattice arithmetic, while `tests/` exercise the real one — v5.1.0's
/// audit pattern, inherited deliberately.
pub(crate) trait HybridKem {
    /// Generate a fresh keypair from the OS CSPRNG. Not deterministic — a save that could
    /// reproduce its own ephemeral keys would be a bug.
    fn keygen(&self) -> (HybridDecapsulationKey, HybridEncapsulationKey);
    /// Encapsulate to an encapsulation key.
    fn encapsulate(
        &self,
        pk: &HybridEncapsulationKey,
    ) -> Result<(HybridCiphertext, SharedSecrets), EnvelopeError>;
    /// Decapsulate from a decapsulation key.
    fn decapsulate(
        &self,
        sk: &HybridDecapsulationKey,
        ct: &HybridCiphertext,
    ) -> Result<SharedSecrets, EnvelopeError>;
}

/// `ml-kem` 0.3.4 (`MlKem1024`) hybridized with `x25519-dalek` 2.0.
///
/// The combiner is concatenation-then-hash with everything bound (see [`derive_dem_key`]),
/// not a general-purpose construction: do not reuse it outside this boundary.
pub(crate) struct MlKem1024X25519;

impl HybridKem for MlKem1024X25519 {
    fn keygen(&self) -> (HybridDecapsulationKey, HybridEncapsulationKey) {
        use ml_kem::kem::{Generate, KeyExport};
        use ml_kem::MlKem1024;

        // Draw through our own audited CSPRNG adapter (`crate::rng`) rather than ml-kem's
        // hidden `getrandom` feature: one entropy path, one audit surface. `UnwrapErr` panics
        // inside the draw on failure — a key from a fallback source is worse than no key.
        let dk = <MlKem1024 as ml_kem::kem::Kem>::DecapsulationKey::try_generate_from_rng(
            &mut rand_core::UnwrapErr(crate::rng::OsRng),
        )
        .expect("OS CSPRNG unavailable");
        let ek = dk.encapsulation_key().clone();

        let mut x_bytes = Zeroizing::new([0u8; X25519_SK_BYTES]);
        crate::rng::fill(&mut x_bytes[..]).expect("OS CSPRNG unavailable");
        let x_sk = StaticSecret::from(*x_bytes);
        let x_pk = PublicKey::from(&x_sk);
        drop(x_sk);

        // The stored decapsulation key is the FIPS 203 64-byte *seed*, the canonical form that
        // round-trips on any conformant implementation — not the expanded 3,168-byte form.
        let seed = Zeroizing::new(dk.to_bytes());
        drop(dk);
        let mut ml_seed = Zeroizing::new([0u8; ML_KEM_1024_SEED_BYTES]);
        ml_seed.copy_from_slice(&seed[..]);

        let decapsulation_key = HybridDecapsulationKey {
            ml_kem: *ml_seed,
            x25519: *x_bytes,
        };
        drop(seed);
        drop(ml_seed);
        drop(x_bytes);

        (
            decapsulation_key,
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

        // `EncapsulationKey::new(&Key<Self>)` is the only constructor — a wrong-length slice
        // can never silently become a key; the checks live at the converts.
        let key_array: ml_kem::Key<ml_kem::kem::EncapsulationKey<MlKem1024>> =
            ml_kem::Key::<ml_kem::kem::EncapsulationKey<MlKem1024>>::try_from(pk.ml_kem.as_slice())
                .map_err(|_| EnvelopeError::KemRejected)?;
        let ek = <MlKem1024 as Kem>::EncapsulationKey::new(&key_array)
            .map_err(|_| EnvelopeError::KemRejected)?;

        let (ct, ss) = ek.encapsulate_with_rng(&mut rand_core::UnwrapErr(crate::rng::OsRng));
        let ss = Zeroizing::new(ss);
        let mut pq = Zeroizing::new([0u8; 32]);
        pq.copy_from_slice(ss.as_slice());
        drop(ss);

        // Ephemeral ECDH: a fresh scalar per encapsulation, discarded immediately.
        let mut eph_bytes = Zeroizing::new([0u8; 32]);
        crate::rng::fill(&mut eph_bytes[..]).expect("OS CSPRNG unavailable");
        let eph = StaticSecret::from(*eph_bytes);
        let eph_pk = PublicKey::from(&eph);
        let peer = PublicKey::from(pk.x25519);
        let shared = eph.diffie_hellman(&peer);
        let ecc = Zeroizing::new(shared.to_bytes());
        drop(shared);
        drop(eph);

        let mut ct_ml = [0u8; ML_KEM_1024_CT_BYTES];
        ct_ml.copy_from_slice(&ct);
        let shared_secrets = SharedSecrets { pq: *pq, ecc: *ecc };
        Ok((
            HybridCiphertext {
                ml_kem: ct_ml,
                x25519: eph_pk.to_bytes(),
            },
            shared_secrets,
        ))
    }

    fn decapsulate(
        &self,
        sk: &HybridDecapsulationKey,
        ct: &HybridCiphertext,
    ) -> Result<SharedSecrets, EnvelopeError> {
        use ml_kem::kem::{Decapsulate, Kem, KeyInit};
        use ml_kem::MlKem1024;

        // The seed is the decapsulation key's `KeySize` input; re-expand, use, drop. If ml-kem
        // ever changes `KeySize` to the expanded form this stops compiling instead of silently
        // reinterpreting the seed — the failure mode worth having (v5's note, v6's choice too).
        let dk = <<MlKem1024 as Kem>::DecapsulationKey as KeyInit>::new_from_slice(&sk.ml_kem)
            .expect("64-byte seed is the decapsulation key's KeySize");
        let ml_ct = ml_kem::Ciphertext::<MlKem1024>::try_from(ct.ml_kem.as_slice())
            .map_err(|_| EnvelopeError::KemRejected)?;
        let ss = Zeroizing::new(dk.decapsulate(&ml_ct));
        let mut pq = Zeroizing::new([0u8; 32]);
        pq.copy_from_slice(ss.as_slice());
        drop(ss);
        drop(dk);

        let x_sk = StaticSecret::from(sk.x25519);
        let peer = PublicKey::from(ct.x25519);
        let shared = x_sk.diffie_hellman(&peer);
        let ecc = Zeroizing::new(shared.to_bytes());
        drop(shared);
        drop(x_sk);

        Ok(SharedSecrets { pq: *pq, ecc: *ecc })
    }
}

/// Create the capsule and the document key for a save.
///
/// The keypair is generated, encapsulated, wrapped, and *dropped* inside this call — what
/// leaves the boundary is public ciphertext plus the derived [`DEMKey`] capability. This is
/// precisely what lets a later save re-capsulate without ever holding the private key: it has
/// `K_unwrap` and generates a fresh pair (v5's design, v6 keeps the shape).
pub(crate) fn create_capsule(
    kem: &dyn HybridKem,
    unwrap: &UnwrapKey,
    usb: Option<&[u8; 32]>,
) -> Result<(Capsule, DEMKey), EnvelopeError> {
    let (dk, ek) = kem.keygen();
    let (ct, ss) = kem.encapsulate(&ek)?;
    let nonce = crate::rng::fill_24();
    let wrapped = wrap_key(unwrap.expose(), &nonce, &dk);
    let phase1 = phase1_commitment(unwrap.expose(), &nonce, &ct, &wrapped);
    let dem = derive_dem_key(&ss, &ct, unwrap.expose(), usb);
    Ok((
        Capsule {
            ct,
            wrapped_sk: wrapped,
            sk_unwrap_nonce: nonce,
            phase1,
        },
        dem,
    ))
}

/// Open a capsule, returning the document-key capability.
///
/// Phase order is load-bearing: commitment → unwrap → decapsulate → derive. Every failure is
/// the same coarse "no" through [`EnvelopeError`]; the phase-1 check happens before any byte
/// of the capsule influences anything.
pub(crate) fn open_capsule(
    kem: &dyn HybridKem,
    unwrap: &UnwrapKey,
    capsule: &Capsule,
    usb: Option<&[u8; 32]>,
) -> Result<DEMKey, EnvelopeError> {
    let expected = phase1_commitment(
        unwrap.expose(),
        &capsule.sk_unwrap_nonce,
        &capsule.ct,
        &capsule.wrapped_sk,
    );
    if !cmt4_verify(&expected, &capsule.phase1) {
        return Err(EnvelopeError::Phase1CommitmentMismatch);
    }
    let dk = unwrap_key_bytes(
        unwrap.expose(),
        &capsule.sk_unwrap_nonce,
        &capsule.wrapped_sk,
    )?;
    let ss = kem.decapsulate(&dk, &capsule.ct)?;
    let dem = derive_dem_key(&ss, &capsule.ct, unwrap.expose(), usb);
    drop(dk); // ZeroizeOnDrop: the expanded halves leave here, nowhere else.
    Ok(dem)
}

// Test-only constructors, so unit tests can build keys without the real lattice arithmetic.

impl HybridDecapsulationKey {
    /// Construct from raw halves. **Test-only**: production keypairs come from
    /// [`HybridKem::keygen`], which draws from the OS CSPRNG.
    #[cfg(test)]
    #[doc(hidden)]
    pub fn for_test(ml_kem: [u8; ML_KEM_1024_SEED_BYTES], x25519: [u8; X25519_SK_BYTES]) -> Self {
        Self { ml_kem, x25519 }
    }
}

impl HybridEncapsulationKey {
    /// Construct from raw halves. **Test-only.**
    #[cfg(test)]
    #[doc(hidden)]
    pub fn for_test(ml_kem: Vec<u8>, x25519: [u8; X25519_SK_BYTES]) -> Self {
        Self { ml_kem, x25519 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unwrap() -> UnwrapKey {
        UnwrapKey::from_bytes([0x31; 32])
    }

    struct FakeKem;

    impl HybridKem for FakeKem {
        fn keygen(&self) -> (HybridDecapsulationKey, HybridEncapsulationKey) {
            (
                HybridDecapsulationKey::for_test([0x01; ML_KEM_1024_SEED_BYTES], [0x02; 32]),
                HybridEncapsulationKey::for_test(vec![0x03; 1_568], [0x04; 32]),
            )
        }

        fn encapsulate(
            &self,
            _pk: &HybridEncapsulationKey,
        ) -> Result<(HybridCiphertext, SharedSecrets), EnvelopeError> {
            Ok((
                HybridCiphertext {
                    ml_kem: [0x05; ML_KEM_1024_CT_BYTES],
                    x25519: [0x06; 32],
                },
                SharedSecrets {
                    pq: [0x07; 32],
                    ecc: [0x08; 32],
                },
            ))
        }

        fn decapsulate(
            &self,
            sk: &HybridDecapsulationKey,
            _ct: &HybridCiphertext,
        ) -> Result<SharedSecrets, EnvelopeError> {
            // The fake asserts the ceremony, not the math: decrypting with the *unwrapped* key
            // (seed byte 0x01) must produce the secrets the wrap was protecting; anything else
            // (e.g. corrupted bytes reaching decapsulate) gets the wrong secrets and a different
            // K_DEM — which is exactly the failure the binding in `derive_dem_key` amplifies
            // into a different sealed payload.
            if sk.ml_kem == [0x01; ML_KEM_1024_SEED_BYTES] {
                Ok(SharedSecrets {
                    pq: [0x07; 32],
                    ecc: [0x08; 32],
                })
            } else {
                Ok(SharedSecrets {
                    pq: [0x77; 32],
                    ecc: [0x88; 32],
                })
            }
        }
    }

    #[test]
    fn create_then_open_recovers_the_document_key() {
        let k = unwrap();
        let (capsule, dem_a) = create_capsule(&FakeKem, &k, None).unwrap();
        let dem_b = open_capsule(&FakeKem, &k, &capsule, None).unwrap();
        assert_eq!(dem_a.expose(), dem_b.expose());
    }

    #[test]
    fn unlock_garbage_with_known_key_fails_at_phase1_before_anything_runs() {
        // The non-circularity proof in miniature: K_unwrap is externally derived, and a capsule
        // whose contents are waste is rejected by the commitment phase, never by "the payload
        // was wrong" deep in the KEM.
        let k = unwrap();
        let garbage_ct = HybridCiphertext {
            ml_kem: [0xEE; ML_KEM_1024_CT_BYTES],
            x25519: [0xEE; 32],
        };
        let garbage = Capsule {
            ct: garbage_ct,
            wrapped_sk: [0xEE; WRAPPED_SK_BYTES],
            sk_unwrap_nonce: [0xEE; NONCE_BYTES],
            phase1: [0xEE; CMT4_TAG_BYTES],
        };
        let err = open_capsule(&FakeKem, &k, &garbage, None).unwrap_err();
        assert_eq!(err, EnvelopeError::Phase1CommitmentMismatch);
    }

    #[test]
    fn capsule_wire_round_trip_and_length_refusal() {
        let k = unwrap();
        let (capsule, _) = create_capsule(&FakeKem, &k, None).unwrap();
        let wire = capsule.to_bytes();
        assert_eq!(wire.len(), Capsule::WIRE_BYTES);
        let parsed = Capsule::from_bytes(&wire).unwrap();
        assert_eq!(parsed.ct.x25519, capsule.ct.x25519);
        assert!(Capsule::from_bytes(&wire[..wire.len() - 1]).is_err());
        assert!(Capsule::from_bytes(&[]).is_err());
    }

    #[test]
    fn usb_slot_participates_in_dem_derivation() {
        let k = unwrap();
        let stick = [0x42; 32];
        let (_, dem_none) = create_capsule(&FakeKem, &k, None).unwrap();
        let (_, dem_stick) = create_capsule(&FakeKem, &k, Some(&stick)).unwrap();
        assert_ne!(dem_none.expose(), dem_stick.expose());
    }

    #[test]
    fn every_capsule_field_bit_flip_is_rejected_at_the_coarse_phase1_boundary() {
        let k = unwrap();
        let (capsule, _) = create_capsule(&FakeKem, &k, None).unwrap();

        let mut bad = capsule.clone();
        bad.ct.ml_kem[0] ^= 1;
        assert_eq!(
            open_capsule(&FakeKem, &k, &bad, None).unwrap_err(),
            EnvelopeError::Phase1CommitmentMismatch
        );
        let mut bad = capsule.clone();
        bad.ct.x25519[0] ^= 1;
        assert_eq!(
            open_capsule(&FakeKem, &k, &bad, None).unwrap_err(),
            EnvelopeError::Phase1CommitmentMismatch
        );
        let mut bad = capsule.clone();
        bad.wrapped_sk[0] ^= 1;
        assert_eq!(
            open_capsule(&FakeKem, &k, &bad, None).unwrap_err(),
            EnvelopeError::Phase1CommitmentMismatch
        );
        let mut bad = capsule.clone();
        bad.sk_unwrap_nonce[0] ^= 1;
        assert_eq!(
            open_capsule(&FakeKem, &k, &bad, None).unwrap_err(),
            EnvelopeError::Phase1CommitmentMismatch
        );
        let mut bad = capsule;
        bad.phase1[0] ^= 1;
        assert_eq!(
            open_capsule(&FakeKem, &k, &bad, None).unwrap_err(),
            EnvelopeError::Phase1CommitmentMismatch
        );
    }

    #[test]
    fn wrong_unwrap_key_is_a_fast_phase1_no() {
        let k = unwrap();
        let bad = UnwrapKey::from_bytes([0x99; 32]);
        let (capsule, _) = create_capsule(&FakeKem, &k, None).unwrap();
        assert_eq!(
            open_capsule(&FakeKem, &bad, &capsule, None).unwrap_err(),
            EnvelopeError::Phase1CommitmentMismatch
        );
    }
}
