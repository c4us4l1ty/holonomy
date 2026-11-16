//! Capability tokens: key-carrying types whose bytes never leave the crate.
//!
//! RFC-001's core mechanical rule is that "whoever holds the type may ask for the operation,
//! and nobody can read the key". These types are deliberately boring outside: no `Clone` (a
//! second copy of a key is a second thing to leak), redacted `Debug`, zeroized backing store.
//! Byte access is `pub(crate)` — only `holonomy-seal` internals may see key bytes.
//!
//! Block 3 (`holonomy-line`) will hold a [`DEMKey`] from unlock to lock and request segment
//! sealing/opening through it; at no point does the world-line crate learn what the key *is*.

use zeroize::Zeroizing;

/// The document-encryption key capability. Seals and opens segment payloads and witness
/// bodies. Presented ("what you proved you have") rather than copied ("what you hold").
pub struct DEMKey {
    inner: Zeroizing<[u8; 32]>,
}

impl DEMKey {
    /// Crate-internal construction. Callers outside this crate can never make one directly:
    /// a `DEMKey` exists only as the output of a successful envelope open or capsule create.
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self {
            inner: Zeroizing::new(bytes),
        }
    }

    /// Crate-internal read. The only byte-level view of the key anywhere in the workspace.
    pub(crate) fn expose(&self) -> &[u8; 32] {
        &self.inner
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
    inner: Zeroizing<[u8; 32]>,
}

impl UnwrapKey {
    /// Crate-internal construction; see [`DEMKey::from_bytes`].
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self {
            inner: Zeroizing::new(bytes),
        }
    }

    /// Crate-internal read; see [`DEMKey::expose`].
    pub(crate) fn expose(&self) -> &[u8; 32] {
        &self.inner
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
}
