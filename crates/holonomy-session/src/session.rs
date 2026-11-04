//! The session: one unlocked container, its pad budget, and the rules for what a save may do.
//!
//! # The state machine
//!
//! ```text
//!                    unlock (passphrase + κ_USB)
//!   Locked ────────────────────────────────────► Unlocked { ActiveStratum, PadBudget }
//!                    ◄────────────────────────────────────────────────────┘
//!                          wipe / lock / reaper fire
//! ```
//!
//! A save is the only operation that can move the pad budget, and it is the only one that needs a
//! writable device. Everything about `SaveOutcome` exists to make one thing impossible: a save that
//! *silently* leaves the container protected by less than it was before.
//!
//! # The rule this module enforces
//!
//! > A save either consumes fresh pad, or it is refused. It never quietly reuses spent pad while
//! > reporting success.
//!
//! That is what [`SaveOutcome::Refused`] is for, and it is why the pad budget lives here rather
//! than being recomputed per call site. The previous arrangement had two copies of this accounting —
//! one in `holonomy-crypto::usb_pad` and one in `holonomy-ui::shell` — and the crypto copy was dead
//! code with four bugs in it.

use holonomy_core::feistel::Feistel22;
use holonomy_core::layout::{PAD_SECTORS_PER_SAVE, SAVES_BEFORE_DOMAIN_REUSE};
use holonomy_core::stratum::ActiveStratum;
use holonomy_core::unlock::ProtectionLayer;
use holonomy_crypto::usb_pad::{PadBudget, PadError, PadMode};

/// Why a session operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    /// The pad is exhausted and the device cannot ratchet to restore it.
    PadExhausted {
        /// Saves already consumed since the last ratchet.
        cursor: u64,
    },
    /// The device is read-only, so a ratchet was attempted when it could not succeed.
    NotWritable,
    /// A ratchet reported the wrong sector count, so the pad was not actually refreshed.
    PartialRatchet {
        /// Sectors the ratchet reported.
        got: u32,
    },
    /// A save was requested with no stratum unlocked.
    Locked,
}

impl From<PadError> for SessionError {
    fn from(e: PadError) -> Self {
        match e {
            PadError::NotWritable => Self::NotWritable,
            PadError::PadExhausted { cursor } => Self::PadExhausted { cursor },
            PadError::WrongSectorCount { got } => Self::PartialRatchet { got },
        }
    }
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PadExhausted { cursor } => write!(
                f,
                "save refused at {cursor} of {SAVES_BEFORE_DOMAIN_REUSE}: the pad is spent and the \
                 device cannot ratchet. Further saves would be protected only by ML-KEM-1024 + \
                 X25519 + XChaCha20-Poly1305, which is not a one-time pad. Insert a writable stick, \
                 or accept the weaker layer explicitly."
            ),
            Self::NotWritable => {
                f.write_str("the device is open read-only; a ratchet cannot succeed")
            }
            Self::PartialRatchet { got } => write!(
                f,
                "ratchet reported {got} of {PAD_SECTORS_PER_SAVE} sectors; the pad was not refreshed"
            ),
            Self::Locked => f.write_str("no stratum is unlocked"),
        }
    }
}

impl std::error::Error for SessionError {}

/// What a completed save did to the protection layer. Reported to the user, never inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    /// Pad was fresh and the save consumed it. The container is one-time-pad backed.
    ConsumedFreshPad {
        /// Saves remaining before the window closes, assuming no ratchet.
        remaining: u64,
    },
    /// Pad was spent, the device ratcheted, so the next 54 saves are fresh again.
    Ratcheted {
        /// Sectors rewritten.
        sectors: u32,
    },
    /// The save did not happen, and the container is unchanged.
    Refused(SessionError),
}

impl SaveOutcome {
    /// Whether the container is still one-time-pad backed after this save.
    pub fn is_pad_backed(&self) -> bool {
        matches!(self, Self::ConsumedFreshPad { .. } | Self::Ratcheted { .. })
    }

    /// Whether the save actually wrote anything.
    pub fn committed(&self) -> bool {
        !matches!(self, Self::Refused(_))
    }
}

/// One unlocked container and the state that governs its saves.
pub struct Session {
    /// The single live stratum. Owning one is the proof that this process unlocked something.
    stratum: Option<ActiveStratum>,
    budget: PadBudget,
    /// Mixed into the pad permutation. Provisioned once; see [`Feistel22::provisioned`].
    k_perm: [u8; 32],
}

impl Session {
    /// Start a session with an unlocked stratum.
    ///
    /// `k_perm` comes from the container's provisioning step. It is *not* re-derived per save: the
    /// pad permutation must be a single bijection for save-to-save disjointness to hold.
    pub fn unlocked(stratum: ActiveStratum, k_perm: [u8; 32], mode: PadMode) -> Self {
        let mut budget = PadBudget::fresh();
        budget.mode = mode;
        Self {
            stratum: Some(stratum),
            budget,
            k_perm,
        }
    }

    /// A locked session, for the pre-unlock state.
    pub fn locked() -> Self {
        Self {
            stratum: None,
            budget: PadBudget::default(),
            k_perm: [0u8; 32],
        }
    }

    /// Whether a stratum is unlocked.
    pub fn is_unlocked(&self) -> bool {
        self.stratum.is_some()
    }

    /// The live stratum, if any.
    pub fn stratum(&self) -> Option<&ActiveStratum> {
        self.stratum.as_ref()
    }

    /// The pad budget as it stands.
    pub fn budget(&self) -> &PadBudget {
        &self.budget
    }

    /// What protects the container right now, as reported rather than assumed.
    pub fn protection(&self) -> ProtectionLayer {
        ProtectionLayer {
            pad: true,
            pad_fresh: self.budget.is_fresh(),
            post_quantum: true,
        }
    }

    /// Perform a save.
    ///
    /// `did_ratchet` is what the caller's USB layer reports: `Some(sectors)` if it rewrote the spent
    /// sectors, `None` if it did not. Passing `Some` when the write did not actually happen is the
    /// one way to lie to this function, which is why the parameter is a count the caller has to have
    /// obtained from the device rather than a bool it can assert.
    ///
    /// # The refusal rule
    ///
    /// If the pad is spent and no ratchet happened, the save is **refused**. It is not downgraded,
    /// not partially written, and not reported as successful with a footnote. The check happens
    /// before the budget is advanced, so a refused save leaves the container exactly as it was.
    pub fn save(&mut self, did_ratchet: Option<u32>) -> SaveOutcome {
        if self.stratum.is_none() {
            return SaveOutcome::Refused(SessionError::Locked);
        }

        let was_fresh = self.budget.is_fresh();

        if !was_fresh {
            // The pad is spent. Only a ratchet can restore it, and only a writable device can.
            match did_ratchet {
                Some(sectors) => match self.budget.record_ratchet(sectors) {
                    Ok(()) => {
                        // The ratchet happened *before* this save's read in the real call path, so
                        // the cursor restart applies to this save rather than the next one.
                        if let Err(e) = self.budget.complete_save() {
                            return SaveOutcome::Refused(e.into());
                        }
                        return SaveOutcome::Ratcheted { sectors };
                    }
                    Err(e) => return SaveOutcome::Refused(e.into()),
                },
                None => {
                    return SaveOutcome::Refused(SessionError::PadExhausted {
                        cursor: self.budget.cursor,
                    })
                }
            }
        }

        // Fresh pad: consume it. If the device also ratcheted, record that so the window restarts.
        if let Some(sectors) = did_ratchet {
            if let Err(e) = self.budget.record_ratchet(sectors) {
                return SaveOutcome::Refused(e.into());
            }
        }
        if let Err(e) = self.budget.complete_save() {
            return SaveOutcome::Refused(e.into());
        }
        let outcome = SaveOutcome::ConsumedFreshPad {
            remaining: self.budget.remaining_fresh_saves(),
        };
        debug_assert!(outcome.committed());
        outcome
    }

    /// The physical sector indices this save would read, in read order.
    ///
    /// Exposed so the check binary and the tests can verify disjointness without a device.
    pub fn plan_for_save(&self, cursor: u64) -> Vec<u32> {
        let f = Feistel22::provisioned(&self.k_perm);
        (0..PAD_SECTORS_PER_SAVE)
            .map(|j| f.permute(Feistel22::logical_sector(cursor, j)))
            .collect()
    }

    /// Drop the stratum and zero the pad accounting. The reaper calls this after wiping.
    pub fn lock(&mut self) {
        self.stratum = None;
        self.budget = PadBudget::default();
        self.k_perm = [0u8; 32];
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the keys, never the permutation.
        f.debug_struct("Session")
            .field("unlocked", &self.is_unlocked())
            .field("cursor", &self.budget.cursor)
            .field("mode", &self.budget.mode)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use holonomy_core::stratum::{StratumId, StratumKeys};

    fn keys() -> StratumKeys {
        StratumKeys {
            k_unwrap: [1u8; 32],
            k_dem: [2u8; 32],
            n_epoch: [3u8; 24],
        }
    }

    fn unlocked_session(mode: PadMode) -> Session {
        let id = StratumId::new(1).unwrap();
        let stratum = holonomy_core::unlock::Unlocked::new(
            id,
            holonomy_core::unlock::Topology::UmbilicalUsb,
            id,
            ProtectionLayer {
                pad: true,
                pad_fresh: true,
                post_quantum: true,
            },
        )
        .into_active(keys());
        Session::unlocked(stratum, [0x5Au8; 32], mode)
    }

    #[test]
    fn a_save_consumes_fresh_pad_and_counts_down() {
        let mut s = unlocked_session(PadMode::ReadOnly);
        assert_eq!(s.budget().remaining_fresh_saves(), 54);
        match s.save(None) {
            SaveOutcome::ConsumedFreshPad { remaining } => assert_eq!(remaining, 53),
            other => panic!("expected a fresh-pad save, got {other:?}"),
        }
        assert!(s.protection().pad_fresh);
    }

    #[test]
    fn the_fifty_fifth_save_on_a_read_only_device_is_refused() {
        // The contract: refusal, not silent degradation.
        let mut s = unlocked_session(PadMode::ReadOnly);
        for _ in 0..54 {
            assert!(s.save(None).committed());
        }
        assert!(!s.budget().is_fresh());
        let outcome = s.save(None);
        assert!(!outcome.committed(), "the 55th save must not commit");
        assert!(matches!(
            outcome,
            SaveOutcome::Refused(SessionError::PadExhausted { cursor: 54 })
        ));
        assert!(
            !s.protection().pad_fresh,
            "and the status line must admit it"
        );
    }

    #[test]
    fn a_refused_save_does_not_advance_the_budget() {
        let mut s = unlocked_session(PadMode::ReadOnly);
        for _ in 0..54 {
            s.save(None);
        }
        let before = s.budget().cursor;
        assert!(!s.save(None).committed());
        assert_eq!(s.budget().cursor, before);
    }

    #[test]
    fn a_ratcheting_device_saves_indefinitely() {
        let mut s = unlocked_session(PadMode::ReadWrite);
        for i in 0..200 {
            let outcome = s.save(Some(PAD_SECTORS_PER_SAVE));
            assert!(outcome.committed(), "writable save {i} refused");
        }
        assert!(
            s.protection().pad_fresh,
            "a ratcheting session is always pad-backed"
        );
    }

    #[test]
    fn a_partial_ratchet_is_refused_rather_than_counted() {
        // A ratchet that rewrote 100 of 32,768 sectors did not refresh the pad. Treating it as
        // success would mark sectors fresh that still hold their old contents.
        let mut s = unlocked_session(PadMode::ReadWrite);
        for _ in 0..54 {
            s.save(Some(PAD_SECTORS_PER_SAVE));
        }
        let outcome = s.save(Some(100));
        assert!(!outcome.committed());
        assert!(matches!(
            outcome,
            SaveOutcome::Refused(SessionError::PartialRatchet { got: 100 })
        ));
    }

    #[test]
    fn a_read_only_device_cannot_claim_a_ratchet() {
        // The lie this catches: a caller passing Some(sectors) on an O_RDONLY handle. The budget
        // must not believe it.
        let mut s = unlocked_session(PadMode::ReadOnly);
        for _ in 0..54 {
            s.save(None);
        }
        let outcome = s.save(Some(PAD_SECTORS_PER_SAVE));
        assert!(!outcome.committed(), "O_RDONLY cannot ratchet");
        assert!(matches!(
            outcome,
            SaveOutcome::Refused(SessionError::NotWritable)
        ));
    }

    #[test]
    fn a_locked_session_refuses_to_save() {
        let mut s = Session::locked();
        assert_eq!(s.save(None), SaveOutcome::Refused(SessionError::Locked));
        assert!(!s.is_unlocked());
    }

    #[test]
    fn locking_clears_the_budget_and_the_permutation() {
        // A re-lock that left the cursor where it was would let a second session skip the window.
        let mut s = unlocked_session(PadMode::ReadWrite);
        s.save(None);
        s.lock();
        assert_eq!(s.budget().cursor, 0);
        assert!(!s.is_unlocked());
        assert_eq!(s.k_perm, [0u8; 32]);
    }

    #[test]
    fn the_plan_never_repeats_a_sector_within_a_save() {
        let s = unlocked_session(PadMode::ReadWrite);
        for cursor in [0u64, 1, 53, 54, 1_000] {
            let plan = s.plan_for_save(cursor);
            let mut sorted = plan.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), plan.len(), "cursor {cursor} repeats a sector");
            assert_eq!(plan.len(), PAD_SECTORS_PER_SAVE as usize);
        }
    }

    #[test]
    fn the_plan_over_a_full_budget_is_disjoint() {
        let s = unlocked_session(PadMode::ReadWrite);
        let mut seen = std::collections::HashSet::new();
        for cursor in 0..SAVES_BEFORE_DOMAIN_REUSE {
            for lba in s.plan_for_save(cursor) {
                assert!(seen.insert(lba), "overlap at cursor {cursor}");
            }
        }
        assert_eq!(
            seen.len() as u64,
            SAVES_BEFORE_DOMAIN_REUSE * PAD_SECTORS_PER_SAVE as u64
        );
    }

    #[test]
    fn debug_never_prints_key_material() {
        let s = unlocked_session(PadMode::ReadWrite);
        let text = format!("{s:?}");
        assert!(!text.contains("90") && !text.contains("0x5a"), "{text}");
        assert!(text.contains("unlocked"));
    }
}
