//! Capability tokens: key-carrying types whose bytes never leave the crate.
//!
//! RFC-001's core mechanical rule is that "whoever holds the type may ask for the operation,
//! and nobody can read the key". These types are deliberately boring outside: no `Clone` (a
//! second copy of a key is a second thing to leak), redacted `Debug`, and the bytes live in a
//! [`LockedSecret`] — boxed, locked best-effort under the [`crate::lock`] ladder, and zeroized
//! on drop — rather than in an ordinary heap allocation.
//!
//! Block 3 (`holonomy-line`) will hold a [`DEMKey`] from unlock to lock and request segment
//! sealing/opening through it; at no point does the world-line crate learn what the key *is*.

use std::sync::Arc;

use zeroize::{Zeroize, Zeroizing};

use crate::lock::LockReport;
use crate::reaper::SecretArenas;
use crate::secret::LockedSecret;

/// The document-encryption key capability. Seals and opens segment payloads and witness
/// bodies. Presented ("what you proved you have") rather than copied ("what you hold").
pub struct DEMKey {
    inner: LockedSecret<32>,
}

impl DEMKey {
    /// Crate-internal construction. Callers outside this crate can never make one directly:
    /// a `DEMKey` exists only as the output of a successful envelope open or capsule create.
    pub(crate) fn from_bytes(mut bytes: [u8; 32]) -> Self {
        let guarded = Zeroizing::new(bytes);
        bytes.zeroize();
        Self {
            inner: LockedSecret::new(*guarded),
        }
    }

    /// Crate-internal read. The only byte-level view of the key anywhere in the workspace.
    pub(crate) fn expose(&self) -> &[u8; 32] {
        self.inner.expose()
    }

    /// The memory-ladder state for this token's cell — what the Security panel reports.
    pub fn memory_report(&self) -> LockReport {
        self.inner.report()
    }

    /// Mint a second, independently locked handle to the same key material.
    ///
    /// `DEMKey` is deliberately not `Clone`: a copy is another thing to leak. OQ-27 nevertheless
    /// needs one in the few places where an owning `holonomy-line` `WorldLine` must hand the
    /// capability to a second owner — re-opening for `verify`, and seeding a compaction target.
    /// This method is that exception, named so every call site is visible in review; the new cell
    /// is locked on its own and zeroized on drop, like the original.
    #[must_use]
    pub fn duplicate(&self) -> Self {
        Self {
            inner: LockedSecret::new(*self.expose()),
        }
    }

    /// Arms this key cell in `arenas` for emergency reaper zeroization without exposing key bytes.
    pub fn arm_reaper(&mut self, arenas: &Arc<SecretArenas>) -> bool {
        self.inner.arm_reaper(arenas)
    }

    /// Whether an emergency wipe has zeroed every byte of this key cell in place.
    #[must_use]
    pub fn is_zeroized(&self) -> bool {
        self.inner.is_zeroized()
    }
}

impl core::fmt::Debug for DEMKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("DEMKey(<redacted>)")
    }
}

/// The unwrap-key capability: derived from the passphrase (plus optional USB slot), used only
/// to open or create capsules. Valid for the whole session; re-derivable, so it is wiped on
/// lock rather than persisted anywhere.
pub struct UnwrapKey {
    inner: LockedSecret<32>,
}

impl UnwrapKey {
    /// Crate-internal construction; see [`DEMKey::from_bytes`].
    pub(crate) fn from_bytes(mut bytes: [u8; 32]) -> Self {
        let guarded = Zeroizing::new(bytes);
        bytes.zeroize();
        Self {
            inner: LockedSecret::new(*guarded),
        }
    }

    /// Crate-internal read; see [`DEMKey::expose`].
    pub(crate) fn expose(&self) -> &[u8; 32] {
        self.inner.expose()
    }

    /// The memory-ladder state for this token's cell.
    pub fn memory_report(&self) -> LockReport {
        self.inner.report()
    }

    /// Arms this key cell in `arenas` for emergency reaper zeroization without exposing key bytes.
    pub fn arm_reaper(&mut self, arenas: &Arc<SecretArenas>) -> bool {
        self.inner.arm_reaper(arenas)
    }

    /// Whether an emergency wipe has zeroed every byte of this key cell in place.
    #[must_use]
    pub fn is_zeroized(&self) -> bool {
        self.inner.is_zeroized()
    }
}

impl core::fmt::Debug for UnwrapKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("UnwrapKey(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_never_debug_print_key_bytes() {
        let dem = DEMKey::from_bytes([0xAB; 32]);
        let unwrap = UnwrapKey::from_bytes([0xCD; 32]);
        assert_eq!(format!("{dem:?}"), "DEMKey(<redacted>)");
        assert_eq!(format!("{unwrap:?}"), "UnwrapKey(<redacted>)");
        assert!(!format!("{dem:?}").contains("171")); // 0xAB = 171
    }

    #[test]
    fn exposure_is_crate_internal_but_functional() {
        let dem = DEMKey::from_bytes([7u8; 32]);
        assert_eq!(dem.expose(), &[7u8; 32]);
    }

    #[test]
    fn tokens_report_their_ladder_rung() {
        let dem = DEMKey::from_bytes([3u8; 32]);
        let report = dem.memory_report();
        assert_eq!(report.requested, 32);
    }
}
