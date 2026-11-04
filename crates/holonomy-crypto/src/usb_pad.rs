//! Folding the container against the physical USB pad, and the honest statement of what that buys.
//!
//! # The budget, not a guarantee
//!
//! `holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE` is 54: Zone B holds 1,782,579 sectors and a
//! save consumes 32,768, so epochs `0..=53` read mutually disjoint pad material and epoch 54 is
//! the first that wraps.
//!
//! So [`PadBudget`] tracks two facts per container: the **monotonic save cursor**, and whether the
//! cursor is still inside the disjoint window. When it is not, [`PadBudget::layer`] reports
//! `pad_fresh: false` and [`holonomy_core::unlock::ProtectionLayer::describe`] says so in the UI.
//! That is the correction to the old PRD's "True Shannon One-Time Pad security" claim: past the
//! budget, the confidential payload is protected by `ML-KEM-1024` + `X25519` +
//! `XChaCha20-Poly1305` with `CMT-4`, which is a strong claim, and *not* by the pad, which is a
//! broken one.
//!
//! # Ratcheting
//!
//! Ratcheting writes fresh CSPRNG bytes over the sectors a save just read. It is the only thing
//! that restores freshness, and it requires a writable USB. [`PadMode`] records which mode the
//! device was opened in, so a write-blocked session cannot accidentally schedule a ratchet that
//! will fail halfway.
//!
//! # What is *not* claimed
//!
//! A pad sector that is used once and then overwritten gives one-time-pad security for the bytes
//! it covered. A pad sector that is read but never overwritten — the write-blocked case — is a
//! static key, and XORing a file against a static key is a stream cipher with a known-key-risk
//! profile, not an information-theoretic guarantee. Hence the budget accounting rather than a
//! slogan.

use holonomy_core::layout::{PAD_SECTORS_PER_SAVE, SAVES_BEFORE_DOMAIN_REUSE, ZONE_B_SECTORS};

/// How the USB device was opened for this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadMode {
    /// `O_RDONLY`. Unlock and view only; no ratchet, no writes.
    ReadOnly,
    /// `O_RDWR`. Saves may ratchet spent sectors.
    ReadWrite,
}

impl PadMode {
    /// Whether a ratchet is possible in this mode.
    pub fn can_ratchet(self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

/// Per-container pad accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadBudget {
    /// Monotonic count of completed saves that consumed pad.
    pub cursor: u64,
    /// Mode this session's device handle is in.
    pub mode: PadMode,
    /// Whether a ratchet completed after the most recent save, which resets the window.
    pub ratcheted_last_save: bool,
}

impl Default for PadBudget {
    fn default() -> Self {
        Self {
            cursor: 0,
            mode: PadMode::ReadOnly,
            ratcheted_last_save: false,
        }
    }
}

impl PadBudget {
    /// A fresh container at provisioning time.
    pub fn fresh() -> Self {
        Self { cursor: 0, mode: PadMode::ReadWrite, ratcheted_last_save: false }
    }

    /// Sectors this save will consume.
    pub fn sectors_this_save() -> u32 {
        PAD_SECTORS_PER_SAVE
    }

    /// The logical sector range for the current cursor, as `[start, end)` within Zone B's index
    /// space before permutation.
    pub fn logical_range(&self) -> (u32, u32) {
        let start = ((self.cursor * PAD_SECTORS_PER_SAVE as u64) % ZONE_B_SECTORS as u64) as u32;
        (start, start + PAD_SECTORS_PER_SAVE)
    }

    /// Whether every sector this save reads is untouched since the last ratchet.
    ///
    /// A ratchet resets the served window: after overwriting the sectors that were just spent,
    /// the next save may reuse their *LBAs* with fresh contents, so the freshness clock restarts.
    pub fn is_fresh(&self) -> bool {
        if self.ratcheted_last_save {
            return true;
        }
        self.cursor < SAVES_BEFORE_DOMAIN_REUSE
    }

    /// Saves remaining before fresh pad runs out, assuming no ratchet.
    pub fn remaining_fresh_saves(&self) -> u64 {
        if self.ratcheted_last_save {
            return SAVES_BEFORE_DOMAIN_REUSE;
        }
        SAVES_BEFORE_DOMAIN_REUSE.saturating_sub(self.cursor)
    }

    /// Record a completed save. Refuses to advance past the point where the mode cannot ratchet
    /// *and* the pad is no longer fresh, because silently continuing would move the container
    /// from "one-time pad backed" to "not" without telling anyone.
    pub fn complete_save(&mut self) -> Result<(), PadError> {
        let was_fresh = self.is_fresh();
        self.cursor = self.cursor.wrapping_add(1);
        if !was_fresh && !self.mode.can_ratchet() && !self.ratcheted_last_save {
            return Err(PadError::PadExhausted { cursor: self.cursor });
        }
        Ok(())
    }

    /// Record that a ratchet of `spent` sectors completed successfully.
    pub fn record_ratchet(&mut self, spent: u32) -> Result<(), PadError> {
        if !self.mode.can_ratchet() {
            return Err(PadError::NotWritable);
        }
        if spent != PAD_SECTORS_PER_SAVE {
            return Err(PadError::WrongSectorCount { got: spent });
        }
        self.ratcheted_last_save = true;
        Ok(())
    }
}

/// Pad accounting failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadError {
    /// The device is `O_RDONLY`, so it cannot be ratcheted.
    NotWritable,
    /// The 54-save window is exhausted and the mode cannot ratchet.
    PadExhausted {
        /// Saves already spent in this epoch.
        cursor: u64,
    },
    /// A ratchet reported a sector count other than 32,768.
    WrongSectorCount {
        /// Sectors the ratchet actually reported.
        got: u32,
    },
}

impl std::fmt::Display for PadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotWritable => f.write_str("device is open read-only; cannot ratchet"),
            Self::PadExhausted { cursor } => write!(
                f,
                "fresh pad exhausted at save {cursor}: further saves are protected by the \
                 post-quantum layer only (ML-KEM-1024/X25519/XChaCha20-Poly1305 CMT-4)"
            ),
            Self::WrongSectorCount { got } => {
                write!(f, "ratchet reported {got} sectors, expected {PAD_SECTORS_PER_SAVE}")
            }
        }
    }
}

impl std::error::Error for PadError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_container_has_fifty_four_saves() {
        let b = PadBudget::fresh();
        assert_eq!(b.remaining_fresh_saves(), 54);
        assert!(b.is_fresh());
    }

    #[test]
    fn window_runs_out_exactly_at_fifty_four() {
        let mut b = PadBudget::fresh();
        b.mode = PadMode::ReadOnly;
        for _ in 0..SAVES_BEFORE_DOMAIN_REUSE {
            b.complete_save().expect("saves inside the window must succeed");
        }
        assert_eq!(b.cursor, 54);
        assert!(!b.is_fresh());
        assert_eq!(b.remaining_fresh_saves(), 0);
        // The 55th save on a read-only device is refused rather than silently downgrading.
        let err = b.complete_save().unwrap_err();
        assert!(matches!(err, PadError::PadExhausted { .. }));
        assert!(err.to_string().contains("post-quantum"));
        assert!(err.to_string().contains("54-save") || err.to_string().contains("save 55"));
    }

    #[test]
    fn ratchet_restores_freshness() {
        let mut b = PadBudget::fresh();
        for _ in 0..54 {
            b.complete_save().unwrap();
        }
        b.record_ratchet(PAD_SECTORS_PER_SAVE).unwrap();
        assert!(b.is_fresh(), "a completed ratchet must restore the window");
        assert_eq!(b.remaining_fresh_saves(), 54);
        b.complete_save().expect("after a ratchet the next save is fresh again");
    }

    #[test]
    fn readonly_device_cannot_ratchet() {
        let mut b = PadBudget::fresh();
        b.mode = PadMode::ReadOnly;
        assert_eq!(b.record_ratchet(PAD_SECTORS_PER_SAVE).unwrap_err(), PadError::NotWritable);
    }

    #[test]
    fn wrong_sector_count_is_rejected() {
        let mut b = PadBudget::fresh();
        assert_eq!(
            b.record_ratchet(1_000).unwrap_err(),
            PadError::WrongSectorCount { got: 1_000 }
        );
    }

    #[test]
    fn ranges_advance_by_one_save_and_wrap_inside_zone_b() {
        let mut b = PadBudget::fresh();
        let (s0, e0) = b.logical_range();
        assert_eq!((s0, e0), (0, 32_768));
        b.cursor = 1;
        assert_eq!(b.logical_range().0, 32_768);
        b.cursor = 54; // past the disjoint window
        let (s, e) = b.logical_range();
        // The range is expressed in logical (pre-permutation) index space, which is bounded by the
        // FPE domain, not by Zone B. At cursor 54 it runs past the end of Zone B in *logical* space;
        // the cycle-walk in `Feistel22::permute` is what folds it back inside. Asserting the range
        // stayed inside Zone B here would be asserting the wrong thing.
        assert!(s < ZONE_B_SECTORS);
        assert!(e > s);
        assert!(e <= holonomy_core::feistel::DOMAIN);
    }

    #[test]
    fn writable_mode_does_not_refuse_past_the_window() {
        // With a writable device the caller ratchets, so the save is allowed; freshness is
        // carried by `ratcheted_last_save`, not by refusing the save.
        let mut b = PadBudget::fresh();
        b.mode = PadMode::ReadWrite;
        for _ in 0..54 {
            b.complete_save().unwrap();
        }
        b.complete_save().expect("writable device may continue");
    }
}
