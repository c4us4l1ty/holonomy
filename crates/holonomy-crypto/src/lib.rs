//! The cryptographic envelope: Argon2id → VDF → HKDF → the derived-key root.
//!
//! Passphrase (NFKD-normalised) → Argon2id (128 MiB, t=2, p=2, per PROJECT.md §2.4)
//! → 64-byte `K_int` → sequential Montgomery squaring `T` times mod a committed
//! 2048-bit safe prime → `K_root = Blake2b-512(S_T || K_int)` →
//! `Hkdf::<Sha512>::expand("HOLONOMY_V3_BARE_SILICON", …)` →
//! `K_enc(32) K_chaff(32) Ω(8) N_root(24)`.
//!
//! `T` is derived from a *measured* per-squaring cost, not hard-coded: PROJECT.md §1.1
//! measured 2,077 ns here, which makes the PRD's 1.5M-iteration figure 3.1 s rather
//! than the claimed 450 ms.
//!
//! Lands in Phase 2. Gate: KDF determinism, wrong-passcode rejection, HKDF output length
//! 96, observable zeroize-on-drop, and a measured `t_kdf` asserted against the §2.4
//! budget. See PROJECT.md §5 Phase 2.

pub use holonomy_jail::PHASE_0_PLACEHOLDER;
