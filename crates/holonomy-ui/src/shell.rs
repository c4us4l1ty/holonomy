//! Reporting for a container-backed UI session.
//!
//! This module does not perform saves, manage pad history, or infer protection from device
//! permissions. A frontend calls [`Session::record_successful_save`] only after the underlying
//! `DocumentSession` reports success, and passes that operation's actual [`SaveReport`]. If a
//! write may have replaced the image before failing, the frontend must call
//! [`Session::mark_recovery_required`] and reopen through the recovery workflow before reporting
//! another cursor.

use holonomy_cli::store::SaveReport;
use holonomy_core::unlock::ProtectionLayer;

use crate::capture::CaptureReport;

/// What one completed store operation reported. Pad freshness is not inferred here.
#[derive(Debug, Clone)]
pub struct SaveOutcome {
    /// Cursor observed from the document session after the successful save.
    pub cursor: u64,
    /// Protection layer returned by the store for this image.
    pub protection: ProtectionLayer,
    /// Sector count reported by the pad source, or `None` when no ratchet was attempted.
    /// A reported count is not independent proof that device writes persisted.
    pub ratcheted: Option<u32>,
    /// Capture-exclusion status observed by the frontend.
    pub capture: CaptureReport,
}

impl SaveOutcome {
    /// Explain the save using only the store's report and the frontend's capture status.
    pub fn status_line(&self) -> String {
        let ratchet = match self.ratcheted {
            Some(count) => format!(
                "pad source reported overwriting {count} sector(s); persistence and freshness are not independently verified"
            ),
            None => "pad ratchet not attempted".into(),
        };
        format!(
            "{}\n{ratchet}\n{}",
            self.protection.describe(),
            self.capture.describe()
        )
    }
}

/// Errors while projecting a completed store result into UI state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    /// A prior save failure left the on-disk state uncertain; this session must be reopened.
    RecoveryRequired,
    /// A completed save did not advance exactly one cursor from the last confirmed state.
    UnexpectedCursor {
        /// The next cursor after the last confirmed save.
        expected: u64,
        /// The cursor reported by the document session.
        actual: u64,
    },
    /// The cursor cannot advance beyond the integer range.
    CursorExhausted,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RecoveryRequired => {
                f.write_str("the container state is uncertain; reopen it through recovery")
            }
            Self::UnexpectedCursor { expected, actual } => write!(
                f,
                "store reported cursor {actual}, but the next confirmed cursor must be {expected}"
            ),
            Self::CursorExhausted => f.write_str("the confirmed save cursor cannot advance"),
        }
    }
}

impl std::error::Error for SessionError {}

/// UI-facing projection of a real document session's confirmed save state.
///
/// This type is deliberately not a save cadence or pad-budget tracker. A successful store report
/// can say that a pad ratchet was attempted, but cannot establish freshness across other devices,
/// copies, snapshots, or rollbacks.
pub struct Session {
    cursor: Option<u64>,
    latest: Option<SaveOutcome>,
    capture: CaptureReport,
    recovery_required: bool,
}

impl Session {
    /// Start from the cursor returned by a successfully opened document session.
    pub fn new(cursor: u64, capture: CaptureReport) -> Self {
        Self {
            cursor: Some(cursor),
            latest: None,
            capture,
            recovery_required: false,
        }
    }

    /// Last confirmed cursor, or `None` while a failed save requires recovery.
    pub fn cursor(&self) -> Option<u64> {
        self.cursor
    }

    /// Record a successful save using the store's result and the session's observed cursor.
    ///
    /// This function only projects an already-completed operation. It does not cause I/O. Callers
    /// must not call it after an error, including errors that may have happened after replacement.
    pub fn record_successful_save(
        &mut self,
        cursor: u64,
        report: &SaveReport,
    ) -> Result<&SaveOutcome, SessionError> {
        if self.recovery_required {
            return Err(SessionError::RecoveryRequired);
        }
        let previous = self.cursor.ok_or(SessionError::RecoveryRequired)?;
        let expected = previous
            .checked_add(1)
            .ok_or(SessionError::CursorExhausted)?;
        if cursor != expected {
            return Err(SessionError::UnexpectedCursor {
                expected,
                actual: cursor,
            });
        }
        self.cursor = Some(cursor);
        self.latest = Some(SaveOutcome {
            cursor,
            protection: report.protection,
            ratcheted: report.ratcheted,
            capture: self.capture,
        });
        Ok(self.latest.as_ref().expect("just recorded"))
    }

    /// Hide the last confirmed cursor and status after any uncertain store failure.
    ///
    /// The frontend must discard this session and reopen only after following the documented
    /// recovery process. A later successful-looking save cannot clear this state.
    pub fn mark_recovery_required(&mut self) {
        self.cursor = None;
        self.latest = None;
        self.recovery_required = true;
    }

    /// Whether the UI should block ordinary save attempts and require recovery.
    pub fn recovery_required(&self) -> bool {
        self.recovery_required
    }

    /// Update the capture status for subsequent save reports.
    pub fn update_capture(&mut self, capture: CaptureReport) {
        self.capture = capture;
    }

    /// The latest successful store report, if any.
    pub fn latest_save(&self) -> Option<&SaveOutcome> {
        self.latest.as_ref()
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

    fn report(ratcheted: Option<u32>) -> SaveReport {
        SaveReport {
            ratcheted,
            pad_sectors: 32_768,
            plaintext_bytes: 1,
            slots: 1,
            digest: [0xA5; 32],
            protection: ProtectionLayer {
                pad: true,
                pad_fresh: None,
                post_quantum: true,
            },
        }
    }

    #[test]
    fn save_status_uses_the_store_report_without_inventing_freshness() {
        let mut session = Session::new(0, capture());
        let saved = session
            .record_successful_save(1, &report(Some(32_768)))
            .expect("cursor advanced");
        assert_eq!(saved.cursor, 1);
        assert_eq!(saved.protection.pad_fresh, None);
        assert_eq!(saved.ratcheted, Some(32_768));
        let status = saved.status_line();
        assert!(status.contains("freshness not established"));
        assert!(status.contains("reported overwriting 32768 sector(s)"));
        assert!(status.contains("not independently verified"));
    }

    #[test]
    fn no_ratchet_is_reported_as_unattempted_not_as_fresh_or_reused() {
        let mut session = Session::new(7, capture());
        let saved = session
            .record_successful_save(8, &report(None))
            .expect("cursor advanced");
        assert_eq!(saved.ratcheted, None);
        assert_eq!(saved.protection.pad_fresh, None);
        assert!(saved.status_line().contains("pad ratchet not attempted"));
    }

    #[test]
    fn cursor_and_status_do_not_advance_on_an_unexpected_report() {
        let mut session = Session::new(7, capture());
        assert_eq!(
            session
                .record_successful_save(9, &report(Some(1)))
                .unwrap_err(),
            SessionError::UnexpectedCursor {
                expected: 8,
                actual: 9,
            }
        );
        assert_eq!(session.cursor(), Some(7));
        assert!(session.latest_save().is_none());
    }

    #[test]
    fn an_uncertain_write_hides_cursor_and_blocks_further_status_updates() {
        let mut session = Session::new(7, capture());
        session.mark_recovery_required();
        assert!(session.recovery_required());
        assert_eq!(session.cursor(), None);
        assert!(session.latest_save().is_none());
        assert_eq!(
            session
                .record_successful_save(8, &report(Some(32_768)))
                .unwrap_err(),
            SessionError::RecoveryRequired
        );
    }
}
