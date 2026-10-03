//! The key envelope: Argon2id → VDF → Blake2b → HKDF → the four root keys.
//!
//! ```text
//! passphrase --NFKD--> Argon2id(128 MiB, t=2, p=2)--> K_int (64 B)
//! K_int --mod N_pub--> S_0 --square T times--> S_T
//! K_root = Blake2b-512(S_T || K_int)
//! okm    = HKDF-Expand-SHA512(PRK = K_root, info = "HOLONOMY_V3_BARE_SILICON")  (96 B)
//!         -> K_enc(32) K_chaff(32) Omega(8) N_root(24)
//! ```
//!
//! # The parameters, and why they differ from the PRD
//!
//! PROJECT.md §2.4 replaces the PRD's Argon2id numbers. The PRD asked for m = 384 MiB,
//! t = 16, p = 2 and claimed 180 ms; measured on this host that is 6.0 GiB-passes and
//! **10,180 ms**, 57× optimistic, and on a Core 2 Duo it would be tens of seconds. That is
//! not an unlock, it is a denial of service on the user's own device. m = 131,072 KiB,
//! t = 2, p = 2 measures 524 ms here. The security argument is preserved -- 128 MiB is a
//! real memory fence, a GPU attacker must supply 128 MiB of physical memory per guess --
//! and only the constant is corrected.
//!
//! `T` is likewise derived from a *measured* per-squaring cost rather than hard-coded. See
//! [`calibration`](../modulus/index.html) and PROJECT.md §2.4.
//!
//! # What is deliberately absent
//!
//! The 128 MiB Argon2id working buffer is allocated by the `argon2` crate and freed when
//! `hash_password_into` returns. FR-4.5 asks for a volatile-fenced scrub and `munmap`
//! before the UI initialises. Those are host-level guarantees, not library ones, so
//! `assert_locked_memory` in [`containment`] covers the `mlockall` / `RLIMIT_CORE` /
//! `PR_SET_DUMPABLE` half, and the scrub half is a Phase 2 follow-up that needs the
//! allocator hook to be measurable. It is not claimed as done here.
//!
//! # The HKDF step is Expand-only
//!
//! FR-4.6 specifies HKDF-*Expand*-SHA512 with `K_root` as the PRK. There is no extract
//! step, because `K_root` is already 64 uniform bytes from Blake2b-512, which is exactly
//! what a PRK is. Running extract again would be a second, redundant mixing stage.

use core::fmt;

use hkdf::Hkdf;
use secrecy::SecretBox;
use sha2::Sha512;
use zeroize::Zeroize;

use crate::bignum::{to_le_bytes, U2048};
use crate::modulus::{self, VdfError};

/// HKDF `info` string. FR-4.6.
pub const HKDF_INFO: &[u8] = b"HOLONOMY_V3_BARE_SILICON";

/// Salt length. FR-4.1/Phase 3: the salt lives at **fixed offset 0** of the container,
/// not at Ω -- Ω is derived from the passphrase, so a salt stored at Ω would be circular.
pub const SALT_LEN: usize = 32;

/// Length of `K_int`, the Argon2id output that seeds the VDF. FR-4.3.
pub const K_INT_LEN: usize = 64;

/// Total HKDF output: 32 + 32 + 8 + 24. FR-4.6.
pub const OKM_LEN: usize = 96;

/// Argon2id memory cost in KiB. PROJECT.md §2.4: 128 MiB.
pub const ARGON2_M_COST_KIB: u32 = 131_072;
/// Argon2id time cost (passes). PROJECT.md §2.4.
pub const ARGON2_T_COST: u32 = 2;
/// Argon2id parallelism. PROJECT.md §2.4: dual-core, no thrash.
pub const ARGON2_P_COST: u32 = 2;

/// Why derivation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeError {
    /// The salt was not exactly [`SALT_LEN`] bytes.
    BadSaltLength,
    /// The passphrase was empty or not valid UTF-8 after normalisation.
    EmptyPassphrase,
    /// Argon2id failed. In practice only parameter or allocation failure.
    Argon2Failed,
    /// HKDF-Expand produced the wrong length, which means the crate's limits changed.
    HkdfLengthMismatch,
    /// The VDF refused to run. See [`VdfError`].
    Vdf(VdfError),
}

impl fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadSaltLength => f.write_str("salt must be exactly 32 bytes"),
            Self::EmptyPassphrase => f.write_str("passphrase is empty after NFKD"),
            Self::Argon2Failed => f.write_str("argon2id failed"),
            Self::HkdfLengthMismatch => f.write_str("hkdf output length mismatch"),
            Self::Vdf(e) => write!(f, "vdf: {e}"),
        }
    }
}

impl std::error::Error for EnvelopeError {}

impl From<VdfError> for EnvelopeError {
    fn from(e: VdfError) -> Self {
        Self::Vdf(e)
    }
}

/// The four keys FR-4.6 expands, and nothing else.
///
/// `Drop` is written out rather than derived from `ZeroizeOnDrop` so the test build can
/// observe the bytes *after* zeroization and prove the drop path really scrubs rather than
/// merely claiming to.
#[derive(Clone, PartialEq, Eq)]
pub struct RootMaterial {
    /// Content cipher key for XChaCha20-Poly1305.
    pub k_enc: [u8; 32],
    /// Stream key for chaff generation.
    pub k_chaff: [u8; 32],
    /// Dynamic payload offset pointer, little-endian. FR-4.6 / FR-2.3.3.
    pub omega: u64,
    /// Extended AEAD base nonce.
    pub n_root: [u8; 24],
}

impl RootMaterial {
    /// Build from the 96-byte HKDF output, `K_enc || K_chaff || Omega || N_root`.
    ///
    /// `Omega` is read little-endian, because PRD FR-2.3.3 specifies
    /// `Read_U64_LE(OffsetBytes)`. PROJECT.md is silent on the byte order, so the PRD
    /// governs. An earlier revision read big-endian on the stated grounds that "a
    /// little-endian read would silently halve every payload offset", which is not a real
    /// effect: byte order is a bijection on the eight HKDF bytes, so either convention
    /// selects an equally arbitrary and equally valid offset. There is no correctness
    /// argument for preferring one -- only a conformance argument, and the PRD's
    /// convention is the one to conform to.
    pub fn from_okm(okm: &[u8; OKM_LEN]) -> Self {
        let mut k_enc = [0u8; 32];
        let mut k_chaff = [0u8; 32];
        let mut omega = [0u8; 8];
        let mut n_root = [0u8; 24];
        k_enc.copy_from_slice(&okm[0..32]);
        k_chaff.copy_from_slice(&okm[32..64]);
        omega.copy_from_slice(&okm[64..72]);
        n_root.copy_from_slice(&okm[72..96]);
        Self {
            k_enc,
            k_chaff,
            omega: u64::from_le_bytes(omega),
            n_root,
        }
    }

    /// Flatten back to the 96-byte OKM layout.
    pub fn to_okm(&self) -> [u8; OKM_LEN] {
        let mut okm = [0u8; OKM_LEN];
        okm[0..32].copy_from_slice(&self.k_enc);
        okm[32..64].copy_from_slice(&self.k_chaff);
        okm[64..72].copy_from_slice(&self.omega.to_le_bytes());
        okm[72..96].copy_from_slice(&self.n_root);
        okm
    }
}

/// `secrecy` requires the payload to implement `Zeroize`, and `Drop` calls through the
/// trait rather than an inherent method so there is exactly one scrub implementation.
impl Zeroize for RootMaterial {
    fn zeroize(&mut self) {
        self.k_enc.zeroize();
        self.k_chaff.zeroize();
        self.omega.zeroize();
        self.n_root.zeroize();
    }
}

impl fmt::Debug for RootMaterial {
    /// Never prints key material. Same reasoning as `SecureBlock`'s `Debug`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RootMaterial")
            .field("omega", &self.omega)
            .field("k_enc", &"[redacted]")
            .field("k_chaff", &"[redacted]")
            .field("n_root", &"[redacted]")
            .finish()
    }
}

impl Drop for RootMaterial {
    fn drop(&mut self) {
        Zeroize::zeroize(self);
        #[cfg(test)]
        crate::envelope::record_post_drop_snapshot(&self.k_enc, &self.k_chaff, &self.n_root);
    }
}

/// Re-exported so callers do not need a direct `secrecy` dependency just to read the
/// derived keys.
pub use secrecy::ExposeSecret;

/// The derived root, wrapped so it cannot be printed or cloned by accident.
///
/// `SecretBox` gives `Debug` that prints `SecretBox<..>` and requires an explicit
/// [`ExposeSecret`] to read, which is the point: `println!("{root:?}")` cannot leak.
pub type DerivedRoot = SecretBox<RootMaterial>;

/// Per-stage timings for `t_kdf`, so the budget can be attributed rather than guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timings {
    /// NFKD normalisation.
    pub normalize_us: u64,
    /// Argon2id.
    pub argon2_ms: u64,
    /// VDF chain.
    pub vdf_ms: u64,
    /// Blake2b + HKDF.
    pub hkdf_us: u64,
    /// Everything.
    pub total_ms: u64,
}

/// A derived root plus the stage timings that produced it.
pub struct Derived {
    /// The four keys.
    pub root: DerivedRoot,
    /// Where the time went.
    pub timings: Timings,
}

/// Normalise a passphrase to NFKD. FR-4.2.
///
/// Normalisation is not cosmetic: without it the same visible passphrase typed on two
/// keyboards, or pasted as NFC from one source and NFD from another, derives two
/// different keys and the user cannot open their own container.
pub fn normalize_passphrase(passphrase: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    passphrase.nfkd().collect()
}

/// Argon2id with the PROJECT.md §2.4 parameters, producing the 64-byte `K_int`.
///
/// The parameters are hard-coded rather than accepted as an argument. An unlock-time
/// parameter is an attacker-reachable knob: letting the container header choose the
/// memory cost would let a hostile file demand 4 GiB.
///
/// **Normalises internally.** FR-4.2 is a security requirement, not a convenience: the
/// precomposed "é" and the decomposed "e" + combining acute are the same character to a
/// human, and if they hash differently the user cannot open their own container. Doing it
/// here rather than in [`derive_root`] means there is exactly one entry point that can be
/// forgotten. NFKD is idempotent, so the extra call in `derive_root` is free.
pub fn argon2id_k_int(passphrase: &str, salt: &[u8]) -> Result<[u8; K_INT_LEN], EnvelopeError> {
    if salt.len() != SALT_LEN {
        return Err(EnvelopeError::BadSaltLength);
    }
    let passphrase = normalize_passphrase(passphrase);
    if passphrase.is_empty() {
        return Err(EnvelopeError::EmptyPassphrase);
    }
    let params = argon2::Params::new(ARGON2_M_COST_KIB, ARGON2_T_COST, ARGON2_P_COST, None)
        .map_err(|_| EnvelopeError::Argon2Failed)?;
    let argon2 = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = [0u8; K_INT_LEN];
    // Argon2 takes bytes, not a `str`.
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, &mut out)
        .map_err(|_| EnvelopeError::Argon2Failed)?;
    Ok(out)
}

/// `K_root = Blake2b-512(S_T || K_int)`. FR-4.4.
///
/// `s_t` is the 256-byte little-endian chain output. `k_int` is appended rather than
/// folded in separately so that a wrong `S_T` -- a wrong passcode, a truncated chain --
/// cannot produce a root that still matches.
pub fn root_key(s_t: &U2048, k_int: &[u8; K_INT_LEN]) -> [u8; 64] {
    use blake2::Digest;
    let mut hasher = blake2::Blake2b512::new();
    hasher.update(to_le_bytes(s_t));
    hasher.update(k_int);
    let mut out = [0u8; 64];
    out.copy_from_slice(&hasher.finalize());
    out
}

/// HKDF-Expand-SHA512 with `k_root` as the PRK. FR-4.6.
pub fn expand_root_keys(k_root: &[u8; 64]) -> Result<[u8; OKM_LEN], EnvelopeError> {
    let hk = Hkdf::<Sha512>::from_prk(k_root).map_err(|_| EnvelopeError::HkdfLengthMismatch)?;
    let mut okm = [0u8; OKM_LEN];
    hk.expand(HKDF_INFO, &mut okm)
        .map_err(|_| EnvelopeError::HkdfLengthMismatch)?;
    Ok(okm)
}

/// The full derivation: passphrase + salt + VDF iteration count → the four root keys.
///
/// `vdf_iterations` is passed in rather than computed here so that the caller owns the
/// measured-cost policy; see PROJECT.md §2.4 and the `vdf-calibrate` bin.
pub fn derive_root(
    passphrase: &str,
    salt: &[u8],
    vdf_iterations: u64,
) -> Result<Derived, EnvelopeError> {
    let wall = std::time::Instant::now();
    let mut timings = Timings::default();

    let t = std::time::Instant::now();
    let normalised = normalize_passphrase(passphrase);
    timings.normalize_us = t.elapsed().as_micros() as u64;
    if normalised.is_empty() {
        return Err(EnvelopeError::EmptyPassphrase);
    }

    let t = std::time::Instant::now();
    let k_int = argon2id_k_int(&normalised, salt)?;
    timings.argon2_ms = t.elapsed().as_millis() as u64;

    let t = std::time::Instant::now();
    let seed = modulus::seed_from_k_int(&k_int);
    let s_t = modulus::sequential_squarings(&seed, vdf_iterations)?;
    timings.vdf_ms = t.elapsed().as_millis() as u64;

    let t = std::time::Instant::now();
    let k_root = root_key(&s_t, &k_int);
    let okm = expand_root_keys(&k_root)?;
    timings.hkdf_us = t.elapsed().as_micros() as u64;
    timings.total_ms = wall.elapsed().as_millis() as u64;

    Ok(Derived {
        root: SecretBox::new(Box::new(RootMaterial::from_okm(&okm))),
        timings,
    })
}

/// Test-only recorder proving `Drop` scrubs.
///
/// Snapshot is taken *after* `zeroize`, so anything recorded here must already be zero.
/// Without this the crate only claims the drop path scrubs; with it, the claim is checked.
/// Robust under parallel tests because every recorded snapshot is zeros regardless of
/// which root was dropped.
/// Length of one recorded snapshot: 32 + 32 + 24. Omega is not key material.
#[cfg(test)]
const SNAPSHOT_LEN: usize = 88;

#[cfg(test)]
thread_local! {
    /// Flat `Vec<u8>` of concatenated fixed-size snapshots. A `Vec<Vec<u8>>` cannot be
    /// built in the `const` initialiser that `thread_local!` requires.
    static SNAPSHOTS: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(crate) fn record_post_drop_snapshot(k_enc: &[u8; 32], k_chaff: &[u8; 32], n_root: &[u8; 24]) {
    let mut buf = Vec::with_capacity(SNAPSHOT_LEN);
    buf.extend_from_slice(k_enc);
    buf.extend_from_slice(k_chaff);
    buf.extend_from_slice(n_root);
    SNAPSHOTS.with(|s| s.borrow_mut().extend_from_slice(&buf));
}

/// Every post-drop snapshot recorded so far, and clears the log.
#[cfg(test)]
pub(crate) fn take_post_drop_snapshots() -> Vec<Vec<u8>> {
    SNAPSHOTS
        .with(|s| std::mem::take(&mut *s.borrow_mut()))
        .chunks(SNAPSHOT_LEN)
        .map(<[u8]>::to_vec)
        .collect()
}

#[cfg(test)]
mod tests;
