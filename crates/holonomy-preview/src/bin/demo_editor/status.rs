//! Honest, frontend-facing status for the restricted demo session.
//!
//! This is not the PRD's full protection reporting: the demo uses a derived pad,
//! does not start the watchdog, and has not verified editor-wide memory locking
//! or capture exclusion. Unknown properties must never be presented as active.

use holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SavePhase {
    Saved,
    Modified,
    Unrepresentable,
    Saving,
    RecoveryRequired,
}

impl SavePhase {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Saved => "Saved",
            Self::Modified => "Unsaved changes",
            Self::Unrepresentable => "Text cannot be saved without changes",
            Self::Saving => "Saving; editing and closing paused",
            Self::RecoveryRequired => "Recovery required; saving blocked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SessionStatus {
    pub(super) phase: SavePhase,
    pub(super) last_confirmed_cursor: u64,
    /// Only a cursor limit, not evidence that the underlying pad is fresh.
    pub(super) remaining_cursor_saves: Option<u64>,
}

impl SessionStatus {
    pub(super) fn new(
        cursor: u64,
        saving: bool,
        recovery: bool,
        invalid: bool,
        dirty: bool,
    ) -> Self {
        let phase = if recovery {
            SavePhase::RecoveryRequired
        } else if saving {
            SavePhase::Saving
        } else if invalid {
            SavePhase::Unrepresentable
        } else if dirty {
            SavePhase::Modified
        } else {
            SavePhase::Saved
        };
        let remaining_cursor_saves = if saving || recovery {
            None
        } else {
            SAVES_BEFORE_DOMAIN_REUSE
                .saturating_sub(1)
                .checked_sub(cursor)
        };
        Self {
            phase,
            last_confirmed_cursor: cursor,
            remaining_cursor_saves,
        }
    }

    pub(super) fn cursor_label(self) -> String {
        match (self.phase, self.remaining_cursor_saves) {
            (SavePhase::Saving, _) => format!("Last confirmed cursor: {}; current image may be changing.", self.last_confirmed_cursor),
            (SavePhase::RecoveryRequired, _) => format!("Last confirmed cursor: {}; current image cursor is unknown. Recovery is required.", self.last_confirmed_cursor),
            (_, Some(remaining)) => format!("Cursor: {} · {remaining} saves before the configured cursor limit (not a freshness guarantee).", self.last_confirmed_cursor),
            (_, None) => format!("Cursor: {} is outside the configured save range.", self.last_confirmed_cursor),
        }
    }
}

pub(super) const PROTECTION: &str = "Experimental encrypted container with a deterministic demo pad; no physical USB protection and no one-time-pad claim.";
pub(super) const PAD_FRESHNESS: &str = "Not verified. The demo pad is deterministic and cannot be ratcheted; cursor counts are not durable pad-use accounting.";
pub(super) const MEMORY: &str = "Editor-wide memory locking and dump exclusion have not been verified; plaintext may remain in memory, swap, or copies.";
pub(super) const WATCHDOG: &str =
    "Not running; no reaper priority or disconnect wipe is active in this demo.";

pub(super) fn capture() -> &'static str {
    if cfg!(target_os = "linux") {
        "Not active. Linux has no portable window capture exclusion; screenshots and screen recording may show this document."
    } else {
        "Not attempted in this demo; screenshots and screen recording may show this document."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_limit_is_not_presented_as_fresh_pad_budget() {
        let blank = SessionStatus::new(0, false, false, false, false);
        assert_eq!(blank.remaining_cursor_saves, Some(53));
        assert_eq!(blank.phase, SavePhase::Saved);
        assert!(blank.cursor_label().contains("not a freshness guarantee"));
        assert!(PAD_FRESHNESS.contains("Not verified"));
        assert!(PAD_FRESHNESS.contains("deterministic"));
        assert_eq!(
            SessionStatus::new(53, false, false, false, false).remaining_cursor_saves,
            Some(0)
        );
        assert_eq!(
            SessionStatus::new(54, false, false, false, false).remaining_cursor_saves,
            None
        );
    }

    #[test]
    fn uncertainty_and_recovery_override_normal_status() {
        let saving = SessionStatus::new(12, true, false, false, true);
        assert_eq!(saving.phase, SavePhase::Saving);
        assert_eq!(saving.remaining_cursor_saves, None);
        assert!(saving.cursor_label().contains("may be changing"));
        let recovery = SessionStatus::new(12, false, true, false, true);
        assert_eq!(recovery.phase, SavePhase::RecoveryRequired);
        assert_eq!(recovery.remaining_cursor_saves, None);
        assert!(recovery.cursor_label().contains("unknown"));
        assert_eq!(
            SessionStatus::new(0, false, false, true, true).phase,
            SavePhase::Unrepresentable
        );
        assert_eq!(
            SessionStatus::new(0, false, false, false, true).phase,
            SavePhase::Modified
        );
    }

    #[test]
    fn protection_claims_describe_the_unintegrated_state() {
        assert!(PROTECTION.contains("no physical USB protection"));
        assert!(MEMORY.contains("not been verified"));
        assert!(WATCHDOG.contains("Not running"));
        assert!(capture().contains("may show"));
    }
}
