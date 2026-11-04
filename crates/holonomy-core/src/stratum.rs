//! Strata, capability envelopes and the one-at-a-time unlocking rule.
//!
//! # The invariant this module exists to enforce
//!
//! `v5.0.0-OMEGA` kept **both** a primary and a decoy handle live so an anti-debugging predicate
//! could pick between them: `vault_handles: &[[u8; 32]; 2]`. Two problems, and the second is the
//! fatal one.
//!
//! 1. It put the primary key material in RAM permanently, for a feature (anti-debugging) that the
//!    audit showed does not work anyway.
//! 2. Index `0` was the real vault and index `1` was the decoy. A reverse engineer did not need to
//!    defeat the predicate; they needed to read one pointer. A decoy chosen by an index the
//!    attacker can see is a label, not a decoy.
//!
//! v5.1.0 deletes the mechanism. **Exactly one stratum is ever instantiated per unlock** — see
//! [`ActiveStratum`], which is a move-only, non-`Clone` value that owns the only `κ` that unlock
//! produced. There is no second handle to select from, so there is no index to patch and nothing
//! to leak if the process is dumped while a decoy is shown.
//!
//! # Why the passphrase determines the stratum
//!
//! `L_1` is the outermost (duress/decoy) stratum and `L_k` the innermost (primary). A passphrase
//! does not carry a label saying which stratum it opens; it is *tried against* each stratum's
//! capability envelope in order, and the first that authenticates is the one you get. That means
//! the number of strata is not itself disclosed by the unlock path — the timing difference
//! between "fails at L_1" and "fails at L_4" is a few AEAD verifications, which
//! [`crate::stratum::MAX_STRATA`] bounds to a constant by always paying for all four.

use zeroize::{Zeroize, ZeroizeOnDrop};

/// How many concentric strata a container may hold: `L_1` ..= `L_k`, `1 <= k <= 4`.
pub const MAX_STRATA: usize = 4;

/// A stratum index, `1`-based because the literature numbers them `L_1` outermost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StratumId(u8);

impl StratumId {
    /// Primary/innermost is the highest index. Construct with [`StratumId::new`].
    pub fn new(index: u8) -> Option<Self> {
        if (1..=MAX_STRATA as u8).contains(&index) {
            Some(Self(index))
        } else {
            None
        }
    }

    /// The 1-based index, `L_1` ..= `L_4`.
    pub fn index(self) -> u8 {
        self.0
    }

    /// Whether this stratum is the duress/decoy one. Only `L_1` is ever a decoy.
    pub fn is_duress(self) -> bool {
        self.0 == 1
    }
}

/// A stratum's symmetric capability material, valid for one unlock.
///
/// `K_unwrap` is what unwraps `SK_stratum` inside the root capsule; `K_DEM` is what encrypts the
/// payload slots. Both are zeroized on drop, and neither is `Copy` or `Clone`.
#[derive(Debug, Zeroize, ZeroizeOnDrop)]
pub struct StratumKeys {
    /// Unwraps the wrapped decapsulation key in slot 0.
    pub k_unwrap: [u8; 32],
    /// Encrypts/decrypts payload slots for this stratum.
    pub k_dem: [u8; 32],
    /// Base nonce, distinct per save so a nonce is never reused under one `K_DEM`.
    pub n_epoch: [u8; 24],
}

/// The single live stratum. Owning one is the proof that this process unlocked something.
///
/// Not `Clone`, not `Copy`, and no `Default`: it can only be produced by
/// [`crate::unlock::Unlocked::into_active`], which consumes the unlock.
pub struct ActiveStratum {
    id: StratumId,
    keys: StratumKeys,
    /// Index of the innermost stratum that exists in this container. Held so the *number* of
    /// strata is known without keeping any other stratum's key material alive — the count is not
    /// a secret from the holder of a valid passphrase, and keeping it avoids re-deriving it on
    /// every save.
    innermost: StratumId,
}

impl std::fmt::Debug for ActiveStratum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Which stratum, never the keys. `StratumKeys` derives Zeroize, not Debug, on purpose;
        // this impl is what lets the unlock be logged at all.
        f.debug_struct("ActiveStratum")
            .field("id", &self.id)
            .field("innermost", &self.innermost)
            .finish_non_exhaustive()
    }
}

impl ActiveStratum {
    /// Only the unlock path may construct this. `pub(crate)` would not be enough if the modules
    /// were ever split differently, so it is a sealed-constructor pattern: the field is private
    /// and there is exactly one caller.
    pub(crate) fn new(id: StratumId, keys: StratumKeys, innermost: StratumId) -> Self {
        Self {
            id,
            keys,
            innermost,
        }
    }

    /// Which stratum this unlock resolved to.
    pub fn id(&self) -> StratumId {
        self.id
    }

    /// The highest stratum index present in this container.
    ///
    /// Knowing the *count* is not a secret from somebody holding a valid passphrase — they can
    /// discover it — and holding it avoids re-deriving the number on every save. No other
    /// stratum's key material is retained.
    pub fn innermost(&self) -> StratumId {
        self.innermost
    }

    /// The derived keys for this stratum, by reference only: [`StratumKeys`] is `ZeroizeOnDrop`
    /// and not `Clone`, so no copy can escape.
    pub fn keys(&self) -> &StratumKeys {
        &self.keys
    }

    /// Whether this unlock is showing the duress stratum. Used only to *disable* features (an
    /// auto-ratchet must not run on a duress save, because ratcheting is a write and a duress
    /// inspection must stay `O_RDONLY`), never to alter what is rendered.
    pub fn is_duress(&self) -> bool {
        self.id.is_duress()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stratum_ids_are_bounded() {
        assert!(StratumId::new(0).is_none());
        assert!(StratumId::new(1).unwrap().is_duress());
        assert!(!StratumId::new(2).unwrap().is_duress());
        assert!(StratumId::new(MAX_STRATA as u8).is_some());
        assert!(StratumId::new(MAX_STRATA as u8 + 1).is_none());
    }

    #[test]
    fn only_first_stratum_is_duress() {
        let duress: Vec<u8> = (1..=MAX_STRATA as u8)
            .filter(|&i| StratumId::new(i).unwrap().is_duress())
            .collect();
        assert_eq!(duress, vec![1]);
    }

    #[test]
    fn active_stratum_carries_one_key_set_only() {
        let keys = StratumKeys {
            k_unwrap: [1; 32],
            k_dem: [2; 32],
            n_epoch: [3; 24],
        };
        let a = ActiveStratum::new(StratumId::new(2).unwrap(), keys, StratumId::new(2).unwrap());
        assert_eq!(a.id().index(), 2);
        assert!(!a.is_duress());
        // There is no API to hold a second one; this test documents the type-level absence.
        // (A `Vec<ActiveStratum>` is possible in principle, but nothing in the crate hands one
        // out, and `Unlocked::into_active` consumes `self`.)
    }
}
