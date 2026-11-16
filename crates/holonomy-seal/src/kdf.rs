//! Passphrase → key hierarchy. Ported from v5.1.0's audited `holonomy-crypto::kdf`, with the
//! v6 difference stated once and implemented once:
//!
//! # The USB slot is optional, and there is exactly one code path
//!
//! `MasterKeys::derive` takes `usb: Option<&[u8; 32]>`. The slot's bytes (or their absence,
//! as a presence flag) enter the subkey-derivation context — *inside the hash*, not as an XOR
//! and never as a second routine. v5 learned this the hard way: its unlock path once XORed
//! `κ_USB` into keys on output and thereby produced containers that could never be reopened.
//! The rule here: same call, one path, deterministic output; a stick present at creation must
//! be present at unlock, and that is the entire contract.
//!
//! # Determinism is the law
//!
//! Identical inputs produce identical keys — no timing, no counters, no host state. v5.0-OMEGA
//! mixed instruction bytes and cycle counters into the keystream ("PEED") and thereby wrote
//! documents it could never decrypt again; K-6 stays on the wall in `AUDIT.md`.

use argon2::{Algorithm, Argon2, Params, Version};
use blake2::{Blake2b512, Digest};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::capability::UnwrapKey;

/// Argon2id memory in KiB (256 MiB) — the memory fence, inherited from v5.1.0's measurement.
pub const M_COST_KIB: u32 = 262_144;
/// Argon2id time cost.
pub const T_COST: u32 = 4;
/// Argon2id lanes (dual-core, no thrash).
pub const P_COST: u32 = 2;

/// Salt length for the derivation. 64 bytes; the world-line supplies a fresh one per epoch.
pub const DERIVE_SALT_BYTES: usize = 64;

/// Output length of the root secret.
pub const ROOT_SECRET_BYTES: usize = 64;

/// The 64-byte root secret. Zeroized on drop, not `Copy`, not `Clone`.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct RootSecret(pub [u8; ROOT_SECRET_BYTES]);

impl core::fmt::Debug for RootSecret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("RootSecret(<redacted>)")
    }
}

/// Which parameter set to use, chosen by host class. Recorded in the container header (Block
/// 3) so an unlock elsewhere never silently derives a different key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamsClass {
    /// 256 MiB / t=4 / p=2 — the memory fence for real containers.
    Legacy,
    /// 8 MiB / t=1 / p=1 — **tests and CI only**. A test suite that spends ~1.5 s and 256 MiB
    /// per unlock is a suite that gets marked `#[ignore]`; this class keeps every test fast,
    /// and must never be reached by anything that touches a user's file.
    Test,
}

impl ParamsClass {
    /// `(m_cost_kib, t_cost, p_cost)` for this class.
    pub fn costs(self) -> (u32, u32, u32) {
        match self {
            Self::Legacy => (M_COST_KIB, T_COST, P_COST),
            Self::Test => (8_192, 1, 1),
        }
    }

    /// The concrete `Argon2` instance.
    pub fn argon2(self) -> Result<Argon2<'static>, KdfError> {
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

impl core::fmt::Display for KdfError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidParams => f.write_str("argon2 rejected the parameter block"),
            Self::Argon2Failed => f.write_str("argon2 failed"),
        }
    }
}

impl std::error::Error for KdfError {}

/// Stage 1: passphrase + salt → 64-byte root secret.
///
/// The per-epoch salt (fresh per save epoch in Block 3's world-line) is what forces an
/// attacker holding cloud snapshots to pay the memory cost once per snapshot rather than
/// once per document.
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
/// `BLAKE2b-512(len(label) ‖ label ‖ 0x00 ‖ root_secret ‖ len(context) ‖ context)`, truncated
/// to 32 bytes. Distinct labels give independent keys; the length prefixes and the `0x00`
/// separator keep labels and contexts unambiguous (no cross-collisions, no prefix tricks).
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

/// Labels, spelled once so call sites cannot drift.
pub mod labels {
    /// Key that wraps/unwraps the capsule decapsulation key.
    pub const K_UNWRAP: &[u8] = b"HOLONOMY_V6_K_UNWRAP";
}

/// The session's derived keys, as capability tokens. Today: the unwrap key. The document key
/// comes from the capsule (post-quantum hybrid), never from a passphrase directly — the
/// capsule is the anti-"harvest-now-decrypt-later" mechanism.
pub struct MasterKeys {
    unwrap: UnwrapKey,
}

impl MasterKeys {
    /// Derive the unwrap key from passphrase, salt, and the optional USB slot.
    ///
    /// The USB slot enters the derivation **context** as `presence-flag ‖ secret-or-zeros`:
    /// one code path, deterministic, and a stick state change changes the key — so an unlock
    /// attempted with the wrong slot configuration fails at the capsule commitment (RFC-001 §3
    /// contract), never silently.
    pub fn derive(
        passphrase: &[u8],
        salt: &[u8; DERIVE_SALT_BYTES],
        usb: Option<&[u8; 32]>,
        class: ParamsClass,
    ) -> Result<Self, KdfError> {
        let root = derive_root_secret(passphrase, salt, class)?;
        let usb_bytes = usb.copied().unwrap_or([0u8; 32]);
        let mut ctx = Vec::with_capacity(1 + 32);
        ctx.push(u8::from(usb.is_some()));
        ctx.extend_from_slice(&usb_bytes);
        let unwrap = derive_subkey(&root, labels::K_UNWRAP, &ctx);
        Ok(Self {
            unwrap: UnwrapKey::from_bytes(unwrap.0),
        })
    }

    /// The unwrap-key capability, presented to the envelope operations. Public because the
    /// *token* is safe to hold — its bytes are not readable outside this crate; only the seal
    /// envelope may present it.
    pub fn unwrap_key(&self) -> &UnwrapKey {
        &self.unwrap
    }
}

impl core::fmt::Debug for MasterKeys {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("MasterKeys(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_is_deterministic() {
        let salt = [7u8; 64];
        let a = derive_root_secret(b"passphrase", &salt, ParamsClass::Test).unwrap();
        let b = derive_root_secret(b"passphrase", &salt, ParamsClass::Test).unwrap();
        assert_eq!(a.0, b.0);
    }

    #[test]
    fn salt_and_passphrase_both_change_the_root() {
        let a = derive_root_secret(b"p1", &[1u8; 64], ParamsClass::Test).unwrap();
        let b = derive_root_secret(b"p1", &[2u8; 64], ParamsClass::Test).unwrap();
        let c = derive_root_secret(b"p2", &[1u8; 64], ParamsClass::Test).unwrap();
        assert_ne!(a.0, b.0);
        assert_ne!(a.0, c.0);
    }

    #[test]
    fn label_prefixes_cannot_collide() {
        // The property the length-prefix + `0x00` separator exists for: `b"A"` + ctx `b"x"`
        // must never equal `b"Ax"` + no ctx. Kept from v5's audit-proven tests.
        let root = RootSecret([1u8; 64]);
        let a = derive_subkey(&root, b"DEM", b"x");
        let b = derive_subkey(&root, b"DEMx", b"");
        assert_ne!(a.0, b.0);
    }

    #[test]
    fn usb_slot_state_changes_the_unwrap_key() {
        let salt = [3u8; 64];
        let stick = [9u8; 32];
        let none = MasterKeys::derive(b"pw", &salt, None, ParamsClass::Test).unwrap();
        let with = MasterKeys::derive(b"pw", &salt, Some(&stick), ParamsClass::Test).unwrap();
        let zeros = MasterKeys::derive(b"pw", &salt, Some(&[0u8; 32]), ParamsClass::Test)
            .unwrap();
        assert_ne!(none.unwrap_key().expose(), with.unwrap_key().expose());
        // And "no stick" is distinct from "a stick of zeros": the presence flag is in the hash.
        assert_ne!(none.unwrap_key().expose(), zeros.unwrap_key().expose());
    }

    #[test]
    fn secrets_do_not_debug_print() {
        let root = RootSecret([9u8; 64]);
        let keys =
            MasterKeys::derive(b"pw", &[1u8; 64], None, ParamsClass::Test).unwrap();
        assert_eq!(format!("{root:?}"), "RootSecret(<redacted>)");
        assert_eq!(format!("{keys:?}"), "MasterKeys(<redacted>)");
    }
}
