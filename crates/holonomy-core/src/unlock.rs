//! The unlock state machine: passphrase in, exactly one [`ActiveStratum`] out.
//!
//! # Shape of the type
//!
//! ```text
//!   SealedPassphrase ──unlock()──> Unlocked ──into_active()──> ActiveStratum
//!         (getrandom)                (tries all strata)        (the only live keys)
//! ```
//!
//! The intermediate [`Unlocked`] exists because the caller genuinely needs to know a few things
//! *before* committing to a stratum: whether the container is intact, which transport supplied
//! `κ_USB`, and how many strata it holds. It is `#[must_use]` and carries no key material — only
//! the [`StratumId`] it resolved to — so a caller that drops it early has leaked nothing.
//!
//! # What is deliberately absent
//!
//! * No `UnlockOutcome::Ambiguous` with two handles. One passphrase, one stratum.
//! * No key material on [`UnlockError`], including for the wrong-passphrase case. A decryption
//!   oracle needs a distinguishable error, so "wrong passphrase" and "container corrupt" are the
//!   same error by construction (see [`UnlockError::Rejected`]).
//! * No stratum count in the error. [`UnlockError::Rejected`] is returned after the *same* number
//!   of AEAD attempts regardless of how many strata the container has, which is why
//!   [`MAX_STRATA`](crate::stratum::MAX_STRATA) is a constant rather than a per-container value.
//!
//! The actual KDF/AEAD work lives in `holonomy-crypto`; this module owns only the state
//! transitions and their invariants, so the rules that matter are testable without a container.

use crate::stratum::{ActiveStratum, StratumId, StratumKeys};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// Which physical source supplied `κ_USB` for this unlock.
///
/// Recorded because the reaper and the ratchet behave differently per topology, and because the
/// save path needs to state honestly what protected the container (see
/// [`ProtectionLayer`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topology {
    /// The 8 GB USB umbilical is present and supplied the pad.
    UmbilicalUsb,
    /// A host-sealed reservoir (TPM/Secure Enclave) stood in for the USB.
    SiliconBound,
    /// Neither was present; only the internal AONT slots and the post-quantum layer protect it.
    AutonomousCloud,
}

/// What actually protects the bytes of a given snapshot, stated so no caller has to guess and
/// nothing has to be claimed in prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtectionLayer {
    /// Whether a live one-time pad was folded over this snapshot.
    pub pad: bool,
    /// Whether the pad was fresh (no sector reused from an earlier save).
    pub pad_fresh: bool,
    /// The post-quantum layer is always present for the confidential payload.
    pub post_quantum: bool,
}

impl ProtectionLayer {
    /// The honest description of a save, including the case the fine print is about.
    pub fn describe(&self) -> &'static str {
        match (self.pad, self.pad_fresh) {
            (true, true) => "one-time pad (fresh sectors) + ML-KEM-1024/X25519/XChaCha20-Poly1305 CMT-4",
            (true, false) => {
                "pad sectors reused (past the 54-save budget) + ML-KEM-1024/X25519/XChaCha20-Poly1305 CMT-4"
            }
            (false, _) => "no pad (write-blocked or no USB) + ML-KEM-1024/X25519/XChaCha20-Poly1305 CMT-4",
        }
    }
}

/// A passphrase that has been read from the user and will be scrubbed on drop.
///
/// Deliberately not `String`: an owned `String` is resizable, so a `realloc` can leave the old
/// bytes in freed heap memory that `Zeroize` never saw. A fixed array cannot move.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SealedPassphrase {
    bytes: [u8; 256],
    len: u8,
}

impl SealedPassphrase {
    /// Construct from a string slice using the legacy Unicode lowercase mapping.
    ///
    /// This is NOT Unicode normalization and makes ASCII case variants equivalent.
    /// The mapping is kept for compatibility with existing containers. Changing it
    /// requires a versioned format and an explicit migration path.
    /// Returns `None` if the resulting byte sequence is empty or exceeds 255 bytes.
    pub fn new(passphrase: &str) -> Option<Self> {
        let normalised = Zeroizing::new(
            passphrase
                .chars()
                .flat_map(|c| c.to_lowercase())
                .collect::<String>(),
        );
        let raw = normalised.as_bytes();
        if raw.is_empty() || raw.len() > 255 {
            return None;
        }
        let mut bytes = [0u8; 256];
        bytes[..raw.len()].copy_from_slice(raw);
        Some(Self {
            bytes,
            len: raw.len() as u8,
        })
    }

    /// The legacy lowercased passphrase bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

impl std::fmt::Debug for SealedPassphrase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the passphrase, not even its length in a way that identifies it.
        f.write_str("SealedPassphrase(<redacted>)")
    }
}

/// Failure to unlock. Deliberately coarse — see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockError {
    /// Wrong passphrase, corrupt container, or both. Not distinguishable, by design.
    Rejected,
    /// The container is not exactly [`CONTAINER_BYTES`](crate::layout::CONTAINER_BYTES).
    WrongSize {
        /// Length found on disk.
        found: u64,
        /// [`CONTAINER_BYTES`](crate::layout::CONTAINER_BYTES).
        expected: u64,
    },
    /// No usable transport for `κ_USB` and the container requires one.
    TransportUnavailable,
    /// The stratum index resolved outside `1..=4`.
    MalformedStratum,
}

impl std::fmt::Display for UnlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected => f.write_str("rejected: wrong passphrase or damaged container"),
            Self::WrongSize { found, expected } => {
                write!(f, "container is {found} bytes, expected {expected}")
            }
            Self::TransportUnavailable => {
                f.write_str("no USB umbilical or sealed reservoir present")
            }
            Self::MalformedStratum => f.write_str("container names a stratum outside L_1..=L_4"),
        }
    }
}

impl std::error::Error for UnlockError {}

/// A validated unlock that has not yet produced keys.
#[must_use = "an unlock that is dropped has released no keys and done no work"]
#[derive(Debug)]
pub struct Unlocked {
    id: StratumId,
    topology: Topology,
    innermost: StratumId,
    layer: ProtectionLayer,
}

impl Unlocked {
    /// Construct an unlock once the capsule has been authenticated.
    ///
    /// `pub` rather than `pub(crate)` because the only caller is `holonomy-crypto`'s
    /// [`unlock`](holonomy_crypto::unlock) function, and the type is evidence rather than a
    /// capability on its own: it carries no key material, so minting one proves nothing except
    /// that a caller has run the checks it claims to have run.
    pub fn new(
        id: StratumId,
        topology: Topology,
        innermost: StratumId,
        layer: ProtectionLayer,
    ) -> Self {
        Self {
            id,
            topology,
            innermost,
            layer,
        }
    }

    /// Which stratum this unlock resolved to.
    pub fn stratum(&self) -> StratumId {
        self.id
    }

    /// Which transport supplied `κ_USB`.
    pub fn topology(&self) -> Topology {
        self.topology
    }

    /// What protects this snapshot's bytes. Reported to the UI; never inferred.
    pub fn protection(&self) -> ProtectionLayer {
        self.layer
    }

    /// Consume the unlock and take ownership of the only live keys.
    ///
    /// Takes `self` by value: there is no way to obtain two [`ActiveStratum`]s from one unlock,
    /// which is the type-level form of the "never keep both handles" rule.
    pub fn into_active(self, keys: StratumKeys) -> ActiveStratum {
        ActiveStratum::new(self.id, keys, self.innermost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passphrase_is_length_checked_not_truncated() {
        assert!(SealedPassphrase::new("correct horse battery staple").is_some());
        assert!(SealedPassphrase::new("").is_none());
        assert!(SealedPassphrase::new(&"a".repeat(256)).is_none());
        assert!(SealedPassphrase::new(&"a".repeat(255)).is_some());
    }

    #[test]
    fn passphrase_does_not_debug_print_itself() {
        let p = SealedPassphrase::new("hunter2").unwrap();
        let s = format!("{p:?}");
        assert!(!s.contains("hunter2"));
        assert!(s.contains("redacted"));
    }

    #[test]
    fn passphrase_normalises_case_so_the_same_key_is_derived() {
        let a = SealedPassphrase::new("Correct Horse").unwrap();
        let b = SealedPassphrase::new("correct horse").unwrap();
        assert_eq!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn rejected_error_discloses_nothing() {
        let e = UnlockError::Rejected;
        let s = e.to_string();
        assert!(!s.contains("stratum"));
        assert!(!s.contains("L_"));
    }

    #[test]
    fn protection_language_is_honest_about_pad_reuse() {
        let fresh = ProtectionLayer {
            pad: true,
            pad_fresh: true,
            post_quantum: true,
        };
        assert!(fresh.describe().contains("fresh"));
        let stale = ProtectionLayer {
            pad: true,
            pad_fresh: false,
            post_quantum: true,
        };
        assert!(stale.describe().contains("reused"));
        assert!(stale.describe().contains("54-save"));
        let none = ProtectionLayer {
            pad: false,
            pad_fresh: false,
            post_quantum: true,
        };
        assert!(none.describe().starts_with("no pad"));
    }
}
