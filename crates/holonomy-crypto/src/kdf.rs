//! Deterministic key derivation: `Argon2id` → label-separated `BLAKE2b` expansion.
//!
//! # The one rule
//!
//! **Identical inputs produce identical keys. Always.** No timings, no cycle counters, no
//! instruction bytes, no pointer values, no host state, no environment. v5.1.0 deleted the
//! `PEED` construction that mixed `.text`, `Caller_RIP` and `RDTSC` into the keystream, because a
//! keystream that depends on those is a keystream you cannot reproduce: a patch, a relocation, a
//! different compiler or a scheduler hiccup changes the key, and the user's document becomes
//! undecryptable. Anti-tampering is a code-integrity problem, not a key-derivation one.
//!
//! `tests/determinism.rs` asserts this at the crate boundary, and
//! `tests/static_audit.rs` greps the source for the forbidden inputs so a regression fails CI
//! before it can fail a user.
//!
//! # Parameters
//!
//! `m = 256 MiB, t = 4, p = 2`. That is a memory-hard choice aimed at the 2 GiB legacy host: 256
//! MiB of the 440 MiB unlock peak is this arena. It is *not* free — the old repo measured
//! ≈1.7 ms per GiB-pass on a modern core, so 1 GiB of passes is ~1.7 s here and slower on a Core
//! 2 Duo. `PARAMS_LEGACY` and `PARAMS_MODERN` are separate so the budget can be met per host
//! class rather than asserted for both.

use argon2::{Algorithm, Argon2, Params, Version};
use blake2::{Blake2b512, Digest};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Argon2id memory in KiB (256 MiB).
pub const M_COST_KIB: u32 = 262_144;
/// Argon2id time cost.
pub const T_COST: u32 = 4;
/// Argon2id lanes.
pub const P_COST: u32 = 2;

/// Salt length for the derivation. 64 bytes, matching `σ_epoch`.
pub const DERIVE_SALT_BYTES: usize = 64;

/// Output length of the root secret.
pub const ROOT_SECRET_BYTES: usize = 64;

/// The 64-byte root secret. Zeroized on drop, not `Copy`, not `Clone`.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct RootSecret(pub [u8; ROOT_SECRET_BYTES]);

impl RootSecret {
    pub fn as_bytes(&self) -> &[u8; ROOT_SECRET_BYTES] {
        &self.0
    }
}

impl std::fmt::Debug for RootSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RootSecret(<redacted>)")
    }
}

/// Which parameter set to use. Chosen by host class, recorded in the container header so an
/// unlock on a slower machine does not silently change the derived key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamsClass {
    /// 256 MiB / t=4 / p=2. The documented parameters and the only class a real container uses.
    Legacy,
    /// 8 MiB / t=1 / p=1. **Tests and CI only.** Deriving a container's key with this class makes
    /// offline passphrase search roughly thirty times cheaper, so it must never be selected by
    /// anything that touches a user's file; it exists because a test suite that spends 1.5 s and
    /// 256 MiB per unlock is a test suite that gets marked `#[ignore]`.
    Test,
}

impl ParamsClass {
    /// `(m_cost_kib, t_cost, p_cost)` for this class.
    pub fn costs(&self) -> (u32, u32, u32) {
        match self {
            Self::Legacy => (M_COST_KIB, T_COST, P_COST),
            Self::Test => (8_192, 1, 1),
        }
    }

    /// The concrete `Argon2` instance.
    pub fn argon2(&self) -> Result<Argon2<'static>, KdfError> {
        let (m, t, p) = self.costs();
        let params =
            Params::new(m, t, p, Some(ROOT_SECRET_BYTES)).map_err(|_| KdfError::InvalidParams)?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }
}

/// Derivation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KdfError {
    /// Argon2 rejected the parameter block (impossible with the constants above).
    InvalidParams,
    /// Argon2 failed internally.
    Argon2Failed,
}

impl std::fmt::Display for KdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidParams => f.write_str("argon2 rejected the parameter block"),
            Self::Argon2Failed => f.write_str("argon2 failed"),
        }
    }
}

impl std::error::Error for KdfError {}

/// Stage 1: passphrase + `σ_epoch` → 64-byte root secret.
///
/// `salt` **must** be `σ_epoch`, the per-save 64-byte salt at [`SALT_OFFSET`]. Passing a constant
/// salt would make the same passphrase derive the same key forever, which is what a rainbow table
/// wants; the per-save regeneration is what forces an attacker holding the cloud snapshots to pay
/// the 256 MiB cost once per snapshot.
///
/// [`SALT_OFFSET`]: holonomy_core::layout::SALT_OFFSET
pub fn derive_root_secret(
    passphrase: &[u8],
    salt: &[u8; DERIVE_SALT_BYTES],
    class: ParamsClass,
) -> Result<RootSecret, KdfError> {
    let mut out = [0u8; ROOT_SECRET_BYTES];
    class
        .argon2()?
        .hash_password_into(passphrase, salt, &mut out)
        .map_err(|_| KdfError::Argon2Failed)?;
    Ok(RootSecret(out))
}

/// A 32-byte subkey, zeroized on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct Subkey(pub [u8; 32]);

/// Derive one 32-byte subkey from the root secret, cryptographically separated by `label`.
///
/// `BLAKE2b-512(label ‖ 0x00 ‖ root_secret ‖ context)` then truncate. This is the standard
/// "expand one secret into labelled subkeys" shape: distinct labels give independent-looking
/// keys, and a key leaked under one label says nothing about another. The `0x00` separator means
/// no label is a prefix of another (`b"DEM"` and `b"DEMx"` cannot collide).
///
/// Deterministic, and the only inputs are the root secret and the label. Nothing about the host,
/// the build, or the clock enters here — see the module docs.
pub fn derive_subkey(root: &RootSecret, label: &[u8], context: &[u8]) -> Subkey {
    let mut h = Blake2b512::new();
    h.update((label.len() as u64).to_le_bytes());
    h.update(label);
    h.update([0u8]);
    h.update(root.0);
    h.update((context.len() as u64).to_le_bytes());
    h.update(context);
    let digest = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest[..32]);
    Subkey(out)
}

/// Pre-expanded labels. Spelling them once here is what keeps two call sites from drifting apart.
pub mod labels {
    /// Permutation key `K_perm`.
    pub const K_PERM: &[u8] = b"HOLONOMY_V51_K_PERM";
    /// Whitening key `K_whiten`.
    pub const K_WHITEN: &[u8] = b"HOLONOMY_V51_K_WHITEN";
    /// Key that unwraps `SK_stratum` in the root capsule.
    pub const K_UNWRAP: &[u8] = b"HOLONOMY_V51_K_UNWRAP";
    /// Document encryption key `K_DEM`.
    pub const K_DEM: &[u8] = b"HOLONOMY_V51_K_DEM";
    /// Nonce base `N_epoch`.
    pub const N_EPOCH: &[u8] = b"HOLONOMY_V51_N_EPOCH";
    /// Decoy-generation key, derived per epoch.
    pub const K_DECOY: &[u8] = b"HOLONOMY_V51_K_DECOY";
    /// CMT-4 commitment domain separator.
    pub const CMT4: &[u8] = b"CMT4";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_is_deterministic() {
        let salt = [7u8; 64];
        let a = derive_root_secret(b"passphrase", &salt, ParamsClass::Legacy).unwrap();
        let b = derive_root_secret(b"passphrase", &salt, ParamsClass::Legacy).unwrap();
        assert_eq!(a.0, b.0);
    }

    #[test]
    fn salt_and_passphrase_both_change_the_root() {
        let a = derive_root_secret(b"p1", &[1u8; 64], ParamsClass::Legacy).unwrap();
        let b = derive_root_secret(b"p1", &[2u8; 64], ParamsClass::Legacy).unwrap();
        let c = derive_root_secret(b"p2", &[1u8; 64], ParamsClass::Legacy).unwrap();
        assert_ne!(a.0, b.0);
        assert_ne!(a.0, c.0);
    }

    #[test]
    fn subkeys_are_separated_by_label() {
        let root = RootSecret([0x42; 64]);
        let perm = derive_subkey(&root, labels::K_PERM, b"");
        let whiten = derive_subkey(&root, labels::K_WHITEN, b"");
        let dem = derive_subkey(&root, labels::K_DEM, b"");
        assert_ne!(perm.0, whiten.0);
        assert_ne!(whiten.0, dem.0);
        assert_ne!(perm.0, dem.0);
    }

    #[test]
    fn label_prefixes_cannot_collide() {
        // Without the length prefix and the `0x00` separator, `b"DEM"` + context `b"x"` would
        // equal `b"DEMx"` + context `b""`. This test is the reason both exist.
        let root = RootSecret([1u8; 64]);
        let a = derive_subkey(&root, b"DEM", b"x");
        let b = derive_subkey(&root, b"DEMx", b"");
        assert_ne!(a.0, b.0);
    }

    #[test]
    fn context_separates_strata() {
        let root = RootSecret([1u8; 64]);
        let l1 = derive_subkey(&root, labels::K_UNWRAP, &[1u8]);
        let l2 = derive_subkey(&root, labels::K_UNWRAP, &[2u8]);
        assert_ne!(l1.0, l2.0);
    }

    #[test]
    fn secrets_do_not_debug_print() {
        let root = RootSecret([9u8; 64]);
        assert_eq!(format!("{root:?}"), "RootSecret(<redacted>)");
    }
}
