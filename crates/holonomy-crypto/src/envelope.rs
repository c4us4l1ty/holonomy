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
//! # What is host-level, and where it lives
//!
//! `mlockall(MCL_CURRENT|MCL_FUTURE)`, `setrlimit(RLIMIT_CORE, 0)` and
//! `prctl(PR_SET_DUMPABLE, 0)` are process-wide and belong to the boot chain, not to a
//! library: `holonomy_jail::rlimits`. `main.rs` calls them before this crate runs.
//!
//! **FR-4.5's scrub is neither.** It was originally written off as "host-level, not a
//! library concern" because `argon2::hash_password_into` allocates its working buffer
//! internally and hands the pointer to nobody — so from this crate the bytes that held a
//! passcode-derived intermediate went back to the allocator unwiped, where the next
//! allocation could read them. **That was true of the API that was being used, and not of
//! Argon2.** `hash_password_into_with_memory` takes the blocks from the caller, so the buffer
//! is now a [`SecureBlock`]: `mmap` + `mlock` + `MADV_DONTDUMP` + `MADV_DONTFORK` +
//! `PROT_NONE` guards, scrubbed and unmapped before [`argon2id_k_int`] returns.
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
///
/// # FR-4.5: the working memory is scrubbed, and the block is never left where anything can read it
///
/// **This used to call `hash_password_into`, which allocates its 128 MiB internally, frees it, and
/// hands the pointer to nobody.** From this crate the bytes that held a passcode-derived intermediate
/// therefore went back to the allocator unwiped, where the next allocation could read them — and the
/// crate's own module docs said so, and called it a host-level guarantee rather than a library one.
/// It is a library one. [`hash_password_into_with_memory`](argon2::Argon2::hash_password_into_with_memory)
/// takes the blocks from the caller, so the buffer is now here and is **explicitly zeroed with
/// `Zeroize`'s volatile writes before this returns**, behind a compiler fence.
///
/// ## Why a `Vec<Block>` and not a `SecureBlock`
///
/// **Because getting a `SecureBlock` to be a `[Block]` needs `unsafe`, and this crate has none.** A
/// `SecureBlock`'s payload is `[u8]`, `Block` is `#[repr(align(1024))]` over `[u64; 128]`, and the
/// conversion is a raw-pointer cast. The alignment is provable — `mmap` returns page-aligned memory
/// and a page is 4096 — so the cast would be *correct*, and it would still be the only `unsafe` in a
/// crate whose entire argument is that it is pure arithmetic over pure computation.
///
/// **What is given up, stated rather than glossed:** the buffer is not `MADV_DONTDUMP`-flagged, has no
/// `PROT_NONE` guards around it, and is returned to the allocator rather than `munmap`ped.
///
/// **What is not given up, because the boot chain already provides it:**
///
/// * **Swap** — `mlockall(MCL_CURRENT | MCL_FUTURE)` runs at boot *before* any KDF runs
///   (`main.rs`, stage 5), and `MCL_FUTURE` marks every future mapping `VM_LOCKED`, so this buffer is
///   page-locked from the moment it is allocated. `holonomy-jail/src/rlimits.rs` owns that call.
/// * **Core dumps** — `RLIMIT_CORE = 0` and `PR_SET_DUMPABLE = 0`, same boot chain, same ordering.
///
/// **What this change adds over the old call is the one thing nothing else was providing: the scrub.**
/// A freed `Vec` whose contents were zeroed hands the next allocation a clean buffer, so the residue
/// a cold-boot attacker reads is zero rather than the intermediate. That is FR-4.5's substance.
///
/// **The passphrase is scrubbed too**, on the way out, because `normalised` is NFKD-expanded text
/// derived directly from what the user typed and is live on the heap for the length of this call.
pub fn argon2id_k_int(passphrase: &str, salt: &[u8]) -> Result<[u8; K_INT_LEN], EnvelopeError> {
    if salt.len() != SALT_LEN {
        return Err(EnvelopeError::BadSaltLength);
    }
    let mut passphrase = normalize_passphrase(passphrase);
    if passphrase.is_empty() {
        return Err(EnvelopeError::EmptyPassphrase);
    }
    let result = argon2id_k_int_inner(&passphrase, salt);
    // **On every path, including the error one.** An Argon2 failure after filling 128 MiB still
    // leaves the buffer to be scrubbed, and a scrub written only on the success path is a scrub
    // that does not happen when something has already gone wrong.
    passphrase.zeroize();
    result
}

/// The Argon2id call, with the working memory owned by this frame. See [`argon2id_k_int`].
fn argon2id_k_int_inner(
    passphrase: &str,
    salt: &[u8],
) -> Result<[u8; K_INT_LEN], EnvelopeError> {
    let params = argon2::Params::new(ARGON2_M_COST_KIB, ARGON2_T_COST, ARGON2_P_COST, None)
        .map_err(|_| EnvelopeError::Argon2Failed)?;
    // **Read the block count before `Argon2::new` takes `params` by value**, which is the only reason
    // this is a separate `let` rather than `argon2.params().m_cost()`: `Params` is not `Copy`, and
    // reaching back for it after the move is the borrow error above.
    let blocks = params.m_cost() as usize;
    let argon2 = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = [0u8; K_INT_LEN];

    // **`m_cost` is in KiB and a `Block` is `Block::SIZE` = 1024 bytes = one KiB**, so the block count
    // is the memory cost exactly. Stated rather than derived from a division, because the count is
    // what has to be right: a short allocation is Argon2 writing past the end of a `Vec`.
    debug_assert_eq!(
        blocks * argon2::Block::SIZE,
        ARGON2_M_COST_KIB as usize * 1024,
        "a Block is one KiB, so the block count is the memory cost"
    );
    let mut memory = vec![argon2::Block::new(); blocks];

    let filled = argon2
        .hash_password_into_with_memory(
            passphrase.as_bytes(),
            salt,
            &mut out,
            memory.as_mut_slice(),
        )
        .is_ok();

    scrub_blocks(&mut memory);
    drop(memory);

    if filled {
        Ok(out)
    } else {
        Err(EnvelopeError::Argon2Failed)
    }
}

/// **The scrub, before anything else happens — including before `filled` is read.**
///
/// # Why this is a named function and not a loop in the caller
///
/// Because this is the one statement in the file whose *absence* is invisible, and a loop in a
/// caller is a loop a reader has to notice. Naming it puts a name on the requirement, and lets
/// `the_argon2_working_memory_is_scrubbed` call it with a buffer it controls — **a gate that has
/// to run the real 128 MiB derivation to observe a scrub is a gate that will not be run often**.
///
/// `Block` exposes its limbs through `AsMut<[u64]>`, so this is `zeroize`'s volatile write on the
/// contents with no transmute and no feature flag. Iterating per block rather than zeroizing the
/// slice as bytes is what makes it a `Block`-typed scrub rather than a byte scrub wearing its
/// name.
fn scrub_blocks(memory: &mut [argon2::Block]) {
    // **The "before" snapshot, in test builds only.** A scrub of an already-zero buffer passes every
    // assertion a scrub of a real one does, and Argon2 fills 128 MiB of *derived* material — so the
    // gate has to prove the buffer was not zero to begin with or it is testing a `memset` of nothing.
    #[cfg(test)]
    let before: Vec<u64> = memory.first().map(|b| b.as_ref().to_vec()).unwrap_or_default();

    for block in memory.iter_mut() {
        block.as_mut().zeroize();
    }
    // **The fence between the scrub and the free.** `Zeroize`'s volatile writes cannot be elided, and
    // this stops the *deallocation* from being hoisted above them.
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);

    // **And the "after", in the same breath**, so the two halves cannot disagree about what was
    // scrubbed. Recorded rather than asserted here because a production build has no test module.
    #[cfg(test)]
    record_scrub(before, memory.first().map(|b| b.as_ref().to_vec()).unwrap_or_default());
}

/// The VDF's share of the unlock budget, in milliseconds. PROJECT.md §2.4.
///
/// **250 ms is a choice, and here is the argument rather than the number.**
///
/// Argon2id at m = 128 MiB, t = 2, p = 2 measures **524 ms** on this host. So the memory-hard half of the
/// unlock already costs half a second, and NFR-1.3's 400–550 ms total leaves **nothing** for a serial
/// chain — which is the same conclusion §2.4 reached when it declared the PRD's parameters a
/// denial of service on the user's own device.
///
/// The question is then what a VDF is *for*, given Argon2 is already there. **Argon2 is memory-hard but
/// parallel**: an attacker with more cores and the same memory goes faster. The VDF is neither — its cost
/// is strictly serial, and no amount of parallel hardware shortens it. So a second of Argon2 is worth less
/// against an attacker than a second of VDF, and the marginal security of the serial chain is *higher* per
/// millisecond.
///
/// 250 ms is chosen to make the serial cost **comparable to** the memory-hard cost rather than dominant:
/// the total unlock lands near **775 ms**, which is over NFR-1.3's original 550 ms and is a deliberate
/// restatement consistent with §2.4. Going higher buys serial resistance linearly and costs the user
/// linearly; going lower leaves the serial dimension thin. 250 ms is the point where neither half is
/// decorative.
///
/// **This is a policy number and the security argument is about the shape, not the digits.** What is not
/// acceptable is a `T` chosen for roundness or copied from another implementation's hardware — see
/// [`vdf_iterations_for`], which is the only way to obtain one.
pub const TARGET_VDF_MS: u64 = 250;

/// Measured nanoseconds per Montgomery squaring on the build host.
///
/// **Measured, not assumed.** `modulus::tests::calibrate_ns_per_squaring` measured **2,664.2 ns** here,
/// against the ~2,657 ns median recorded in that test's doc comment and the 2,077 ns PROJECT.md §1.1
/// measured for a *different* implementation. The spread on this host is roughly ±8 %, so a single reading
/// is indicative only and this constant is rounded **down** deliberately: see
/// [`vdf_iterations_for`].
pub const MEASURED_NS_PER_SQUARING: u64 = 2_664;

/// The iteration count this build uses: [`TARGET_VDF_MS`] worth of squarings at the measured cost.
///
/// At 2,664 ns and 250 ms that is `250e6 / 2,664 = 93,843`, so the unlock costs about **250 ms** of VDF on
/// top of Argon2id's 524 ms.
///
/// **A build-time constant, not a runtime measurement**, deliberately: the VDF's whole security property is
/// that its cost is *known and fixed in advance*, so a caller who could vary it could be given a cheap
/// challenge. Calibrating at runtime would make `T` depend on the attacker's machine. §2.4 asks for
/// `build.rs` to bake this in from a calibration run; that is not implemented, so **this constant is the
/// substituted step and it is the part to replace** — see [`MEASURED_NS_PER_SQUARING`].
pub const VDF_ITERATIONS: u64 = 93_843;

/// Derive the iteration count for a VDF budget, in squarings.
///
/// `T = floor(target_ms * 1e6 / ns_per_squaring)`
///
/// # Why this is a function and not a constant
///
/// **Because a constant is the bug this exists to prevent.** PROJECT.md §1.1 records the PRD claiming
/// 300 ns per squaring and 2,077 ns measured, and §2.4 records the PRD claiming 180 ms for Argon2id where
/// 524 ms was measured — **every hard-coded VDF parameter in the tree's history has been wrong by 3–7×**.
/// A derivation makes the dependence on host speed explicit and checkable, and it puts the refusal in one
/// place instead of at each call site.
///
/// # Rounding is deliberately **down**, and here is why
///
/// `floor` bounds the VDF's cost at the budget rather than above it. Rounding up would let a slow host
/// overshoot; rounding down can only ever spend slightly *less* than the budget, which costs a fraction of
/// a percent of security and never a stall. **A budget is a ceiling, so it is enforced as one.**
///
/// # Refusals
///
/// Both inputs are refused when they would make the arithmetic meaningless rather than being clamped.
/// `ns_per_squaring == 0` would divide by zero and report an instant VDF, which is the worst possible
/// failure: a challenge that costs nothing. `target_ms == 0` would produce `T = 0`, and `sequential_squarings`
/// already refuses zero iterations — refusing here too means the mistake is reported where it is made.
pub fn vdf_iterations_for(target_ms: u64, ns_per_squaring: u64) -> Result<u64, EnvelopeError> {
    if ns_per_squaring == 0 {
        return Err(EnvelopeError::Vdf(VdfError::Uncalibrated));
    }
    if target_ms == 0 {
        return Err(EnvelopeError::Vdf(VdfError::ZeroIterations));
    }
    // Saturating, not wrapping: `target_ms * 1e6` overflows u64 at ~1.8e13 ms, which is a number a
    // caller can type. Saturating gives `u64::MAX / ns` iterations, which is a very slow VDF rather than a
    // very fast one -- the safe direction for a budget.
    Ok(target_ms.saturating_mul(1_000_000) / ns_per_squaring)
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

    // **The normalised passphrase is scrubbed on every path out, not on the happy path.**
    //
    // `normalised` is NFKD-expanded text derived directly from what the user typed, living on
    // the heap, and it is live across the whole of the derivation — the longest part of an
    // unlock. `ScopeGuard` does not exist in `core` and this crate is not adding a dependency
    // for one call, so the scrub is written at each exit instead: **there are exactly four, and
    // the compiler says so if a fifth is added.**
    //
    // FR-4.5 is about the Argon2id *memory block*, which `argon2id_k_int` now scrubs itself. This
    // is a different buffer, in a different crate, that the same requirement covers and that
    // nothing else in the envelope was reaching.
    let normalised = normalize_passphrase(passphrase);
    timings.normalize_us = wall.elapsed().as_micros() as u64;
    if normalised.is_empty() {
        return Err(EnvelopeError::EmptyPassphrase);
    }

    let t = std::time::Instant::now();
    // **`?` and not a `match` that re-wraps `Err(e) => return Err(e)`.** The error type is the same
    // on both sides, so the `match` was four lines spelling `?` and one line of room for the two to
    // drift apart. **The scrubbing below is unaffected**: it is after this line either way, which is
    // the property the `match` might have looked like it protected and did not.
    let k_int = argon2id_k_int(&normalised, salt)?;
    // **Two copies of the passphrase in memory at once, and both are now scrubbed.** `argon2id_k_int`
    // normalises internally, so the argument below is *already* the normalised text and the call
    // above made a second copy of it, which it scrubs on its own way out.
    let mut normalised = normalised;
    normalised.zeroize();
    timings.argon2_ms = t.elapsed().as_millis() as u64;

    let t = std::time::Instant::now();
    // **`k_int` is now the only live copy of the intermediate, and it is scrubbed below.**
    let seed = modulus::seed_from_k_int(&k_int);
    let s_t = modulus::sequential_squarings(&seed, vdf_iterations)?;
    timings.vdf_ms = t.elapsed().as_millis() as u64;

    let t = std::time::Instant::now();
    let k_root = root_key(&s_t, &k_int);
    let okm = expand_root_keys(&k_root)?;
    timings.hkdf_us = t.elapsed().as_micros() as u64;
    timings.total_ms = wall.elapsed().as_millis() as u64;

    // **Every intermediate is scrubbed before this returns, on the success path.**
    //
    // `k_int` is a `[u8; 64]` on the stack — the passcode-derived secret that seeds the VDF and is
    // one of the two halves of `K_root`. `k_root` and `okm` are its descendants; `okm` is copied
    // into the `SecretBox` below and then scrubbed. **None of these four is in a `Secret`,** and
    // that was the reason `K_int` appeared in the audit as "never zeroized". `seed` is a `U2048`
    // limb array holding the chain state, which is no less sensitive.
    //
    // **The root is built before the scrub, not after.** `RootMaterial::from_okm` copies, so the
    // order is: copy into the owned `Secret`, then wipe every stack copy. Doing it the other way
    // round would hand a `SecretBox` full of zeros — which compiles, passes a length assertion,
    // and opens nobody's container.
    let root = RootMaterial::from_okm(&okm);

    let mut k_int = k_int;
    let mut seed = seed;
    let mut k_root = k_root;
    let mut okm = okm;
    k_int.zeroize();
    seed.zeroize();
    k_root.zeroize();
    okm.zeroize();
    // **The fence, and it is the point of using `zeroize` rather than `fill`.** It tells the
    // compiler not to reorder the scrub relative to the stores above, and not to elide stores whose
    // values are never read again — which is exactly the optimisation that would turn a scrub into
    // a comment.
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);

    Ok(Derived {
        root: SecretBox::new(Box::new(root)),
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

// `(before, after)` for every [`scrub_blocks`] call, so the gate can assert both halves.
//
// **Two `Vec<u64>`, recorded together, because either alone proves nothing.** `after` alone is a
// scrub of a buffer that was already zero; `before` alone is a buffer that was filled and then
// abandoned. Only the pair says *"this held derived material and this is what is left"*.
//
// **A comment and not a doc comment, and that is the correction.** It was `///`, which rustc reads as
// documenting the `thread_local!` *macro invocation* -- and a macro invocation is not a documented item,
// so every `cargo build` printed `warning: unused doc comment` and the project carried a standing
// reminder of it. **The declaration it describes is the next two lines**, so the comment belongs on the
// `static`, or as a plain comment here.
#[cfg(test)]
thread_local! {
    static SCRUBS: std::cell::RefCell<Vec<(Vec<u64>, Vec<u64>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn record_scrub(before: Vec<u64>, after: Vec<u64>) {
    SCRUBS.with(|s| s.borrow_mut().push((before, after)));
}

/// Every scrub recorded so far, and clears the log. Empty is a pass; **one entry per scrub is not**.
#[cfg(test)]
pub(crate) fn take_scrubs() -> Vec<(Vec<u64>, Vec<u64>)> {
    SCRUBS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

#[cfg(test)]
mod tests;
