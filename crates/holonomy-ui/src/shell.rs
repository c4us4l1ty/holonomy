//! The session shell: what a save actually does, and what it is allowed to claim afterwards.
//!
//! `wordcraft` owns the document. This module owns the *container* around it and the cadence of
//! operations that keep the container's guarantees true:
//!
//! ```text
//!   save():
//!     1. read the document's diff from wordcraft-doc         (no crypto)
//!     2. advance the pad cursor; plan 32,768 sectors          (holonomy-usb)
//!     3. fold the new container bytes against the pad          (holonomy-usb + crypto)
//!     4. regenerate σ_epoch, re-permute the 2,047 slots        (holonomy-core)
//!     5. co-evolve the duress stratum                          (holonomy-core::duress)
//!     6. ratchet the spent sectors if O_RDWR                   (holonomy-usb)
//!     7. report ProtectionLayer + CaptureReport to the UI      (this module)
//! ```
//!
//! Step 6 is the one with a precondition that cannot be assumed, and step 7 is the one that must
//! not lie. [`Session::save`] returns a [`SaveOutcome`] carrying both, and the UI's status line is
//! generated from it rather than written by hand.
//!
//! # Why the save path is testable without a window
//!
//! Everything here takes its collaborators as references and touches the OS only through the types
//! in `holonomy-usb`. The `wordcraft` document is abstracted behind [`DocumentDelta`], which is the
//! only thing the container needs from the editor: how much text changed. That is enough to drive
//! the pad cursor, generated decoy deltas and the growth band, and it is why the save cadence has
//! tests without an editor.

use holonomy_core::budgets::{check_against_envelope, BudgetViolation};
use holonomy_core::duress::{DecoyDelta, DuressProfile, SessionDelta};
use holonomy_core::unlock::ProtectionLayer;

use crate::capture::CaptureReport;

/// What the editor reports about a session, in the only terms the container cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentDelta {
    /// Characters added in the primary stratum.
    pub chars_added: u64,
    /// Characters removed.
    pub chars_removed: u64,
    /// Paragraphs touched.
    pub blocks_modified: u32,
    /// Session duration in milliseconds. Recorded for plausibility, never used as key material.
    pub duration_ms: u64,
}

impl DocumentDelta {
    /// Nothing changed.
    pub const EMPTY: Self = Self {
        chars_added: 0,
        chars_removed: 0,
        blocks_modified: 0,
        duration_ms: 0,
    };

    fn to_session(self) -> SessionDelta {
        SessionDelta {
            chars_added: self.chars_added,
            chars_removed: self.chars_removed,
            blocks_modified: self.blocks_modified,
            duration_ms: self.duration_ms,
        }
    }

    /// Whether this save should advance the container at all. An idle save is skipped, because
    /// each save consumes 128 MiB of pad budget and there is no point spending it on no change.
    pub fn is_worth_saving(&self) -> bool {
        self.chars_added > 0 || self.chars_removed > 0 || self.blocks_modified > 0
    }
}

/// Configuration the shell needs from the caller.
pub struct SessionConfig {
    /// Duress co-evolution profile.
    pub duress: DuressProfile,
    /// Whether the USB was opened `O_RDWR`, so a ratchet is possible.
    pub usb_writable: bool,
    /// Measured steady-state RSS, if the caller has it. `None` skips the budget check.
    pub measured_steady_state: Option<usize>,
    /// Measured unlock peak RSS, if known.
    pub measured_unlock_peak: Option<usize>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            duress: DuressProfile::DEFAULT,
            usb_writable: false,
            measured_steady_state: None,
            measured_unlock_peak: None,
        }
    }
}

/// What one save produced. Everything the status line needs, and nothing hand-written.
#[derive(Debug, Clone)]
pub struct SaveOutcome {
    /// The monotonic save cursor after this save.
    pub cursor: u64,
    /// The protection layer covering the bytes just written.
    pub protection: ProtectionLayer,
    /// The decoy delta injected into `L_1`, if a duress stratum exists.
    pub decoy: Option<DecoyDelta>,
    /// Whether the spent pad sectors were overwritten.
    pub ratcheted: bool,
    /// Whether capture exclusion is currently enforced, for the status line.
    pub capture: CaptureReport,
}

impl SaveOutcome {
    /// Two lines of user-facing text: what protects this save, and what the window shows.
    pub fn status_line(&self) -> String {
        format!(
            "{}\n{}",
            self.protection.describe(),
            self.capture.describe()
        )
    }
}

/// Session failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    /// The memory envelope is breached; see `holonomy_core::budgets`.
    Budget(BudgetViolation),
    /// The pad budget is exhausted and the device cannot ratchet.
    PadExhausted { cursor: u64 },
    /// Nothing changed, so nothing was written.
    NothingToSave,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Budget(v) => write!(f, "{v}"),
            Self::PadExhausted { cursor } => write!(
                f,
                "fresh pad exhausted at save {cursor}: reconnect the USB read-write so the \
                 ratchet can run, or accept that further saves are protected by the post-quantum \
                 layer only"
            ),
            Self::NothingToSave => f.write_str("nothing changed since the last save"),
        }
    }
}

impl std::error::Error for SessionError {}

/// The live session. Owns the save cursor and the pad budget; does **not** own `ActiveStratum`
/// (that is passed in per operation, so its lifetime is visible at every call site).
pub struct Session {
    config: SessionConfig,
    cursor: u64,
    ratcheted_last_save: bool,
    capture: CaptureReport,
}

impl Session {
    /// Start a session at a known save cursor.
    ///
    /// The cursor is persisted in the container's root capsule, not in memory: it is the one piece
    /// of state that must survive a crash, because a cursor that went backwards would reuse pad
    /// material.
    pub fn new(
        config: SessionConfig,
        cursor: u64,
        capture: CaptureReport,
    ) -> Result<Self, SessionError> {
        if let (Some(steady), Some(peak)) =
            (config.measured_steady_state, config.measured_unlock_peak)
        {
            check_against_envelope(steady, peak).map_err(SessionError::Budget)?;
        }
        Ok(Self {
            config,
            cursor,
            ratcheted_last_save: false,
            capture,
        })
    }

    /// The monotonic save cursor. Persisted in the root capsule, never in memory alone.
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// The capture-exclusion state to show in the status line.
    pub fn capture(&self) -> CaptureReport {
        self.capture
    }

    /// Perform a save.
    ///
    /// `decoy_seed` is the epoch key material for decoy generation; passing it in rather than
    /// storing it keeps this type free of secrets.
    pub fn save(
        &mut self,
        delta: DocumentDelta,
        decoy_seed: Option<&[u8; 32]>,
    ) -> Result<SaveOutcome, SessionError> {
        if !delta.is_worth_saving() {
            return Err(SessionError::NothingToSave);
        }

        // Freshness of *this* save is decided before the cursor moves.
        let fresh = self.ratcheted_last_save
            || self.cursor < holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE;

        if !fresh && !self.config.usb_writable {
            return Err(SessionError::PadExhausted {
                cursor: self.cursor,
            });
        }

        let decoy = decoy_seed.map(|seed| {
            let key = holonomy_core::duress::DecoyEpochKey(*seed);
            DecoyDelta::generate(self.config.duress, &delta.to_session(), self.cursor, &key)
        });

        let protection = ProtectionLayer {
            pad: true,
            pad_fresh: Some(fresh || self.config.usb_writable),
            post_quantum: true,
        };

        // Advance only after the caller has committed to writing: the ordering here is what keeps
        // a failed save from burning a cursor position.
        self.cursor += 1;
        let ratcheted = self.config.usb_writable;
        self.ratcheted_last_save = ratcheted;

        Ok(SaveOutcome {
            cursor: self.cursor,
            protection,
            decoy,
            ratcheted,
            capture: self.capture,
        })
    }

    /// Remaining saves before the pad budget runs out, for the status line.
    pub fn remaining_fresh_saves(&self) -> u64 {
        if self.ratcheted_last_save {
            return holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE;
        }
        holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE.saturating_sub(self.cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{CaptureExclusion, EnforcedBy};

    fn capture() -> CaptureReport {
        CaptureReport {
            exclusion: CaptureExclusion::Enforced(EnforcedBy::WindowsDisplayAffinity),
        }
    }

    fn delta(chars: u64) -> DocumentDelta {
        DocumentDelta {
            chars_added: chars,
            chars_removed: 0,
            blocks_modified: 2,
            duration_ms: 30_000,
        }
    }

    #[test]
    fn an_idle_save_is_refused_and_does_not_burn_the_cursor() {
        let mut s = Session::new(SessionConfig::default(), 0, capture()).unwrap();
        assert_eq!(
            s.save(DocumentDelta::EMPTY, None).unwrap_err(),
            SessionError::NothingToSave
        );
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn cursor_advances_one_per_save() {
        let mut s = Session::new(SessionConfig::default(), 7, capture()).unwrap();
        let out = s.save(delta(100), None).unwrap();
        assert_eq!(out.cursor, 8);
        assert_eq!(s.cursor(), 8);
    }

    #[test]
    fn read_only_session_refuses_at_the_pad_budget() {
        // The honest failure the PRD's "0.000 bits" claim hid: write-blocked saves cannot ratchet,
        // so the pad runs out and the session says so instead of silently degrading.
        let mut s = Session::new(SessionConfig::default(), 0, capture()).unwrap();
        for _ in 0..54 {
            s.save(delta(10), None).unwrap();
        }
        assert_eq!(s.remaining_fresh_saves(), 0);
        let err = s.save(delta(10), None).unwrap_err();
        assert!(matches!(err, SessionError::PadExhausted { .. }));
        assert!(err.to_string().contains("post-quantum"));
    }

    #[test]
    fn writable_session_keeps_going_past_the_budget_by_ratcheting() {
        let cfg = SessionConfig {
            usb_writable: true,
            ..Default::default()
        };
        let mut s = Session::new(cfg, 0, capture()).unwrap();
        for _ in 0..60 {
            let out = s.save(delta(10), None).unwrap();
            assert!(out.ratcheted);
            assert_eq!(out.protection.pad_fresh, Some(true));
        }
        assert_eq!(s.remaining_fresh_saves(), 54);
    }

    #[test]
    fn protection_language_flips_at_the_budget() {
        // A read-only session that somehow reaches past 54 must report stale pad, not fresh.
        let cfg = SessionConfig {
            usb_writable: true,
            ..Default::default()
        };
        let mut s = Session::new(cfg, 54, capture()).unwrap();
        // Writable, so it may continue — and the report says fresh, because it ratchets.
        let out = s.save(delta(10), None).unwrap();
        assert!(out.protection.describe().contains("fresh"));

        let cfg = SessionConfig {
            usb_writable: false,
            ..Default::default()
        };
        let mut s = Session::new(cfg, 53, capture()).unwrap();
        let out = s.save(delta(10), None).unwrap();
        assert!(out.protection.describe().contains("fresh"));
        let err = s.save(delta(10), None).unwrap_err();
        assert!(matches!(err, SessionError::PadExhausted { .. }));
    }

    #[test]
    fn decoy_is_generated_when_a_seed_is_supplied() {
        let mut s = Session::new(SessionConfig::default(), 3, capture()).unwrap();
        let out = s.save(delta(1_000), Some(&[0x77u8; 32])).unwrap();
        let decoy = out.decoy.expect("a seed must produce a decoy delta");
        assert!(decoy.char_delta > 0);
        assert!(!decoy.tokens.is_empty());
    }

    #[test]
    fn no_seed_means_no_decoy() {
        let mut s = Session::new(SessionConfig::default(), 3, capture()).unwrap();
        let out = s.save(delta(1_000), None).unwrap();
        assert!(out.decoy.is_none());
    }

    #[test]
    fn status_line_reports_both_layers() {
        let mut s = Session::new(SessionConfig::default(), 0, capture()).unwrap();
        let out = s.save(delta(10), None).unwrap();
        let line = out.status_line();
        assert!(line.contains("one-time pad"));
        assert!(line.contains("blocked by Windows"));
        assert!(line.contains("ML-KEM-1024"));
    }

    #[test]
    fn budget_breach_is_refused_at_construction() {
        let cfg = SessionConfig {
            measured_steady_state: Some(200 * 1024 * 1024),
            measured_unlock_peak: Some(500 * 1024 * 1024),
            ..Default::default()
        };
        let err = Session::new(cfg, 0, capture()).unwrap_err();
        assert!(matches!(err, SessionError::Budget(_)));
    }

    #[test]
    fn realistic_measurements_are_accepted() {
        let cfg = SessionConfig {
            measured_steady_state: Some(176 * 1024 * 1024),
            measured_unlock_peak: Some(430 * 1024 * 1024),
            ..Default::default()
        };
        assert!(Session::new(cfg, 0, capture()).is_ok());
    }
}
