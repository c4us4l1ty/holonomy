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

/// Display only the last successful save's fingerprint, never as proof of the
/// current image while a save may have changed it or recovery is required.
pub(super) fn fingerprint_label(phase: SavePhase, digest: Option<[u8; 32]>) -> String {
    match phase {
        SavePhase::Saving => "Unavailable while saving; the image may be changing.".into(),
        SavePhase::RecoveryRequired => "Unavailable: the current image is uncertain and requires recovery.".into(),
        _ => match digest {
            Some(digest) => {
                let hex = digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
                format!("Last successful save: {hex}\nUnkeyed fingerprint of the saved image; not a durability or authenticity proof. Sharing it may correlate container copies.")
            }
            None => "Not recorded in this session. Opening or creating a container does not record a save fingerprint.".into(),
        },
    }
}

#[cfg(test)]
mod fingerprint_tests {
    use super::*;

    #[test]
    fn fingerprint_is_hidden_when_disk_state_is_uncertain() {
        let digest = [0xabu8; 32];
        let hex = "ab".repeat(32);
        let saved = fingerprint_label(SavePhase::Saved, Some(digest));
        assert!(saved.contains(&hex));
        assert!(saved.contains("not a durability or authenticity proof"));
        assert!(fingerprint_label(SavePhase::Modified, Some(digest)).contains(&hex));
        assert!(!fingerprint_label(SavePhase::Saving, Some(digest)).contains(&hex));
        assert!(!fingerprint_label(SavePhase::RecoveryRequired, Some(digest)).contains(&hex));
        assert!(fingerprint_label(SavePhase::Saved, None).contains("Not recorded"));
    }
}

/// A last-save result is not a statement about a pending or uncertain image.
pub(super) fn ratchet_label(phase: SavePhase, last_result: Option<Option<u32>>) -> String {
    match phase {
        SavePhase::Saving => "Unavailable while saving; the result is not yet confirmed.".into(),
        SavePhase::RecoveryRequired => "Unavailable: the last save and pad state are uncertain; recovery is required.".into(),
        _ => match last_result {
            None => "No save result recorded in this session.".into(),
            Some(None) => "Ratchet not attempted for the last save; pad freshness is not verified.".into(),
            Some(Some(count)) => format!("The source reported {count} sectors ratcheted for the last save. This is not independent verification of device writes or pad freshness."),
        },
    }
}

#[cfg(test)]
mod ratchet_tests {
    use super::*;

    #[test]
    fn ratchet_status_uses_save_result_and_hides_uncertainty() {
        assert!(ratchet_label(SavePhase::Saved, None).contains("No save result"));
        assert!(ratchet_label(SavePhase::Saved, Some(None)).contains("not attempted"));
        let reported = ratchet_label(SavePhase::Saved, Some(Some(32768)));
        assert!(reported.contains("reported 32768"));
        assert!(reported.contains("not independent verification"));
        assert!(!ratchet_label(SavePhase::Saving, Some(Some(32768))).contains("32768"));
        assert!(ratchet_label(SavePhase::RecoveryRequired, Some(None)).contains("uncertain"));
    }
}

/// What the last successful save says protected it, hidden while the answer is not trustworthy.
pub(super) fn protection_label(phase: SavePhase, last: Option<&str>) -> String {
    match phase {
        SavePhase::Saving => "Unavailable while saving.".into(),
        SavePhase::RecoveryRequired => {
            "Unavailable: the last save and pad state are uncertain.".into()
        }
        _ => match last {
            None => "No save result recorded in this session.".into(),
            Some(description) => format!(
                "{description}. This is the layer attached by the container store, which cannot \
                 establish pad freshness."
            ),
        },
    }
}

#[cfg(test)]
mod protection_tests {
    use super::*;

    #[test]
    fn protection_status_comes_from_the_save_and_hides_uncertainty() {
        assert!(protection_label(SavePhase::Saved, None).contains("No save result"));
        let text = protection_label(SavePhase::Saved, Some("one-time pad (freshness not established by this layer) + ML-KEM"));
        assert!(text.contains("one-time pad"));
        assert!(text.contains("cannot establish pad freshness"));
        assert!(!protection_label(SavePhase::Saving, Some("one-time pad")).contains("one-time"));
        assert!(protection_label(SavePhase::RecoveryRequired, None).contains("uncertain"));
    }
}
