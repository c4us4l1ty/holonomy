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

use argon2::{Algorithm, Argon2, Block, Params, Version};
use blake2::{Blake2b512, Digest};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

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
}

/// Derivation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KdfError {
    /// Argon2 rejected the parameter block (impossible with the constants above).
    InvalidParams,
    /// Argon2 failed internally.
    Argon2Failed,
    /// OQ-6's guard fired: a build that could touch a user container was asked to derive at the
    /// weakened **test-only** schedule. Reaching this means a bug routed real material through
    /// `ParamsClass::Test`; the loud refusal is the point.
    TestScheduleRefused,
}

impl core::fmt::Display for KdfError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidParams => f.write_str("argon2 rejected the parameter block"),
            Self::Argon2Failed => f.write_str("argon2 failed"),
            Self::TestScheduleRefused => {
                f.write_str("test-only kdf schedule reached a production derivation guard")
            }
        }
    }
}

impl std::error::Error for KdfError {}

/// The Argon2 working arena: caller-owned so it can be scrubbed before the heap reclaims it
/// (OQ-5, closed).
///
/// `Argon2::hash_password_into` allocates `m_cost` KiB internally and frees it **unwiped** —
/// v5's H1 file wound, where freed arena bytes carried password-derived state into whatever
/// the allocator touched next. Here the arena is ours: `Block` is plain data (`Copy`, 1 KiB)
/// and implements `Zeroize` via argon2's `zeroize` feature, so drop zeroizes the slab and
/// only then frees it. Every Argon2 invocation in this crate runs through [`run_argon2`]
/// below, so the discipline cannot exist in one entry point and lapse in another.
struct Arena(Vec<Block>);

impl Arena {
    /// Allocate exactly the number of blocks these parameters demand (argon2 errors on fewer).
    fn for_params(params: &Params) -> Self {
        let arena = Self(vec![Block::new(); params.block_count()]);
        #[cfg(test)]
        crate::allocation_audit::watch(
            arena.0.as_ptr().cast(),
            arena.0.len() * core::mem::size_of::<Block>(),
        );
        arena
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        // `zeroize` intentionally does not implement Zeroize for `[T]` when T is only
        // `Zeroize` (rather than the stronger `DefaultIsZeroes` bound); iterate so each
        // Argon2 Block's own volatile zeroization impl is used.
        for block in &mut self.0 {
            block.zeroize();
        }
    }
}

/// Run Argon2id once with explicit `params`, through a scrubbed [`Arena`]. Shared by both
/// derivation entry points (creation's class, open's replayed record) and by the calibration
/// probe — the only place in the workspace where Argon2 actually executes.
pub(crate) fn run_argon2(
    passphrase: &[u8],
    salt: &[u8],
    params: &Params,
    out: &mut [u8],
) -> Result<(), KdfError> {
    let mut arena = Arena::for_params(params);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params.clone())
        .hash_password_into_with_memory(passphrase, salt, out, &mut arena.0)
        .map_err(|_| KdfError::Argon2Failed)
}

/// OQ-6's guard, compiled into existence only where it matters: in a build without
/// `debug_assertions` and outside test harnesses — i.e. exactly the builds that could touch a
/// user's container — the weakened `Test` schedule is refused at the derivation boundary.
/// Test and debug builds get a nil guard with the *same signature*, so call sites never drift.
#[cfg(not(any(debug_assertions, test)))]
fn refuse_test_class_in_release(class: ParamsClass) -> Result<(), KdfError> {
    if matches!(class, ParamsClass::Test) {
        return Err(KdfError::TestScheduleRefused);
    }
    Ok(())
}

/// The nil-profile twin of the guard above; see its documentation.
#[cfg(any(debug_assertions, test))]
fn refuse_test_class_in_release(_: ParamsClass) -> Result<(), KdfError> {
    Ok(())
}

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
    refuse_test_class_in_release(class)?;
    let (m_kib, t, p) = class.costs();
    let params =
        Params::new(m_kib, t, p, Some(ROOT_SECRET_BYTES)).map_err(|_| KdfError::InvalidParams)?;
    let mut out = Zeroizing::new([0u8; ROOT_SECRET_BYTES]);
    run_argon2(passphrase, salt, &params, &mut out[..])?;
    Ok(RootSecret(*out))
}

/// Stage 1, replayed: derive under an *explicit* recorded parameter set.
///
/// This is how a container created on a calibrated host opens on any other machine: the
/// header (Block 3) stores the [`crate::calibrate::KdfParamsRecord`]; open replays it. Direct
/// param selection bypasses the fence and is therefore crate-internal on purpose — the only
/// calibrated producer is [`crate::calibrate::calibrate`].
fn validate_replayed_params(
    record: &crate::calibrate::KdfParamsRecord,
    allow_test_schedule: bool,
) -> Result<(), KdfError> {
    if allow_test_schedule && (record.m_kib, record.t, record.p) == (8_192, 1, 1) {
        return Ok(());
    }
    if record.m_kib < crate::calibrate::FENCE_MIN_KIB {
        return Err(KdfError::TestScheduleRefused);
    }
    if !record.is_production_fenced() {
        return Err(KdfError::InvalidParams);
    }
    Ok(())
}

pub fn derive_root_secret_with(
    passphrase: &[u8],
    salt: &[u8; DERIVE_SALT_BYTES],
    record: &crate::calibrate::KdfParamsRecord,
) -> Result<RootSecret, KdfError> {
    validate_replayed_params(record, cfg!(any(debug_assertions, test)))?;
    let params = Params::new(record.m_kib, record.t, record.p, Some(ROOT_SECRET_BYTES))
        .map_err(|_| KdfError::InvalidParams)?;
    let mut out = Zeroizing::new([0u8; ROOT_SECRET_BYTES]);
    run_argon2(passphrase, salt, &params, &mut out[..])?;
    Ok(RootSecret(*out))
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
    h.update(root.0.as_slice());
    h.update((context.len() as u64).to_le_bytes());
    h.update(context);
    let mut digest = h.finalize();
    let mut out = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(&digest[..32]);
    digest.as_mut_slice().zeroize();
    Subkey(*out)
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
        Ok(Self::from_root(&root, usb))
    }

    /// The open path: replay the exact parameters the container header recorded at creation
    /// (see [`crate::calibrate::KdfParamsRecord`]). Must derive **identical** keys to `derive`
    /// for equal parameters — one formula, two entry points, tested pairwise — so a container
    /// created on a calibrated host opens on every other machine.
    pub fn derive_with(
        passphrase: &[u8],
        salt: &[u8; DERIVE_SALT_BYTES],
        usb: Option<&[u8; 32]>,
        params: &crate::calibrate::KdfParamsRecord,
    ) -> Result<Self, KdfError> {
        let root = derive_root_secret_with(passphrase, salt, params)?;
        Ok(Self::from_root(&root, usb))
    }

    /// The one USB-binding subkey step both entry points share, so the two can never drift:
    /// presence flag then bytes, in the derivation context, hashed — never XORed (v5's lesson).
    fn from_root(root: &RootSecret, usb: Option<&[u8; 32]>) -> Self {
        // Fixed-size, stack-only context: the optional device secret never enters an ordinary
        // heap Vec. The wrapper also erases this temporary on every exit path.
        let mut ctx = Zeroizing::new([0u8; 33]);
        ctx[0] = u8::from(usb.is_some());
        if let Some(usb_bytes) = usb {
            ctx[1..].copy_from_slice(usb_bytes);
        }
        let unwrap = derive_subkey(root, labels::K_UNWRAP, &ctx[..]);
        Self {
            unwrap: UnwrapKey::from_bytes(unwrap.0),
        }
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
        let zeros = MasterKeys::derive(b"pw", &salt, Some(&[0u8; 32]), ParamsClass::Test).unwrap();
        assert_ne!(none.unwrap_key().expose(), with.unwrap_key().expose());
        // And "no stick" is distinct from "a stick of zeros": the presence flag is in the hash.
        assert_ne!(none.unwrap_key().expose(), zeros.unwrap_key().expose());
    }

    #[test]
    fn replayed_params_derive_identical_keys() {
        // The portability contract: a header record replayed on another host equals the class
        // derivation performed at creation.
        let record = crate::calibrate::KdfParamsRecord {
            m_kib: 8_192,
            t: 1,
            p: 1,
        };
        let via_class = MasterKeys::derive(b"pw", &[5u8; 64], None, ParamsClass::Test).unwrap();
        let via_record = MasterKeys::derive_with(b"pw", &[5u8; 64], None, &record).unwrap();
        assert_eq!(
            via_class.unwrap_key().expose(),
            via_record.unwrap_key().expose()
        );
        let stick = [7u8; 32];
        let via_class_usb =
            MasterKeys::derive(b"pw", &[5u8; 64], Some(&stick), ParamsClass::Test).unwrap();
        let via_record_usb =
            MasterKeys::derive_with(b"pw", &[5u8; 64], Some(&stick), &record).unwrap();
        assert_eq!(
            via_class_usb.unwrap_key().expose(),
            via_record_usb.unwrap_key().expose()
        );
    }

    #[test]
    fn replayed_params_reject_weak_or_unbounded_release_schedules() {
        let test = crate::calibrate::KdfParamsRecord {
            m_kib: 8_192,
            t: 1,
            p: 1,
        };
        assert_eq!(
            validate_replayed_params(&test, false),
            Err(KdfError::TestScheduleRefused)
        );
        assert_eq!(validate_replayed_params(&test, true), Ok(()));
        let oversized_time = crate::calibrate::KdfParamsRecord {
            m_kib: crate::calibrate::FENCE_MIN_KIB,
            t: u32::MAX,
            p: 1,
        };
        assert_eq!(
            validate_replayed_params(&oversized_time, false),
            Err(KdfError::InvalidParams)
        );
    }

    #[test]
    fn scrubbed_arena_matches_argon2s_internal_arena() {
        // OQ-5's correctness pin: the caller-owned scrubbed arena must derive byte-identical
        // output to argon2's own (unwiped) internal allocation, through both entry points.
        let salt = [9u8; DERIVE_SALT_BYTES];
        let params = Params::new(8_192, 1, 1, Some(ROOT_SECRET_BYTES)).unwrap();
        let mut expected = [0u8; ROOT_SECRET_BYTES];
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(b"pw", &salt, &mut expected)
            .unwrap();
        let via_class = derive_root_secret(b"pw", &salt, ParamsClass::Test).unwrap();
        assert_eq!(via_class.0, expected);
        let record = crate::calibrate::KdfParamsRecord {
            m_kib: 8_192,
            t: 1,
            p: 1,
        };
        let via_record = derive_root_secret_with(b"pw", &salt, &record).unwrap();
        assert_eq!(via_record.0, expected);
    }

    #[test]
    fn secrets_do_not_debug_print() {
        let root = RootSecret([9u8; 64]);
        let keys = MasterKeys::derive(b"pw", &[1u8; 64], None, ParamsClass::Test).unwrap();
        assert_eq!(format!("{root:?}"), "RootSecret(<redacted>)");
        assert_eq!(format!("{keys:?}"), "MasterKeys(<redacted>)");
    }
}
