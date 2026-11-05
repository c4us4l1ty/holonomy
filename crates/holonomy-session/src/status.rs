//! The status line, and the rule that it reports rather than asserts.
//!
//! Every field here is derived from live state — the session's pad budget, the lock report from
//! `holonomy-crypto::lock`, the capture-exclusion result — and none of it is a compile-time constant
//! dressed as a measurement. That distinction is the whole point: the previous draft of this project
//! claimed `16.60 MiB` and "zero heap" in a table, and the table was wrong because nothing forced it
//! to be derived from anything.

use holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE;
use holonomy_crypto::lock::LockCapability;

/// Which capture-exclusion rung the platform actually provided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureState {
    /// The window is excluded from screen capture by the OS.
    Excluded,
    /// The platform has the facility but it was refused (older Windows build, no entitlement).
    Refused,
    /// The platform has no such facility. Linux is here, and says so.
    Unsupported,
    /// Not yet attempted.
    Unknown,
}

impl CaptureState {
    /// The sentence to show. Deliberately not reassuring where there is nothing to be reassured by.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Excluded => "window excluded from screen capture",
            Self::Refused => "capture exclusion was REFUSED: screenshots can see this document",
            Self::Unsupported => "this platform cannot exclude a window from capture: screenshots can see this document",
            Self::Unknown => "capture exclusion not determined",
        }
    }

    /// Whether the user may be told the screen is safe.
    pub fn is_safe(self) -> bool {
        matches!(self, Self::Excluded)
    }
}

/// Everything the bottom of the window shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLine {
    /// The protection layer in force.
    pub protection: holonomy_core::unlock::ProtectionLayer,
    /// Saves remaining before the pad window closes, assuming no ratchet.
    pub fresh_saves_remaining: u64,
    /// Whether the USB device is writable in this session.
    pub device_writable: bool,
    /// Which rung of the page-locking ladder was obtained.
    pub lock: LockCapability,
    /// What the OS said about capture exclusion.
    pub capture: CaptureState,
    /// Whether the reaper obtained `SCHED_FIFO`.
    pub realtime: bool,
}

impl StatusLine {
    /// The line as plain text, one clause per fact.
    ///
    /// Not a table and not a badge: each clause is a full sentence fragment that a user can act on,
    /// because a status line that says `PAD: OK` tells nobody anything.
    pub fn render(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        if self.protection.pad_fresh == Some(true) {
            parts.push(format!(
                "one-time pad: {} of {SAVES_BEFORE_DOMAIN_REUSE} fresh saves left",
                self.fresh_saves_remaining
            ));
        } else if self.protection.pad_fresh.is_none() {
            parts.push(format!(
                "pad freshness not established by this session; {SAVES_BEFORE_DOMAIN_REUSE}-save \
                 accounting is not evidence that no sector was reused"
            ));
        } else if self.device_writable {
            parts.push(
                "pad spent; the next save will ratchet it (needs the stick to stay inserted)"
                    .to_string(),
            );
        } else {
            parts.push(format!(
                "PAD SPENT: this container is protected by ML-KEM-1024 + X25519 + \
                 XChaCha20-Poly1305 only, not by a one-time pad ({} saves past the window)",
                self.past_window_by()
            ));
        }

        parts.push(match self.lock {
            LockCapability::DirectMapExcluded => {
                "memory excluded from the kernel direct map".to_string()
            }
            LockCapability::ResidentNoDump => {
                "memory resident and excluded from core dumps".to_string()
            }
            LockCapability::Resident => {
                "memory resident but dumpable: enable process protection".to_string()
            }
            LockCapability::None => {
                "MEMORY NOT LOCKED: swap and core dumps may contain the document".to_string()
            }
        });

        parts.push(self.capture.describe().to_string());
        parts.push(if self.realtime {
            "reaper at real-time priority".to_string()
        } else {
            "reaper at normal priority (SCHED_FIFO needs privileges; detection is unaffected)"
                .to_string()
        });

        parts.join(" · ")
    }

    /// How many saves past the 54-save window this container is, or 0.
    pub fn past_window_by(&self) -> u64 {
        if self.protection.pad_fresh != Some(false) {
            0
        } else {
            SAVES_BEFORE_DOMAIN_REUSE
                .saturating_sub(self.fresh_saves_remaining)
                .max(1)
        }
    }

    /// Whether every claim the line makes is a *good* state. Used by `--check` to decide the exit
    /// code; never used to hide a field, because a hidden field is a claim the user cannot audit.
    pub fn is_healthy(&self) -> bool {
        self.protection.pad_fresh == Some(true)
            && self.capture.is_safe()
            && self.lock >= LockCapability::ResidentNoDump
    }

    /// The clauses that are not healthy, for the check report.
    pub fn problems(&self) -> Vec<String> {
        let mut v = Vec::new();
        match self.protection.pad_fresh {
            Some(true) => {}
            Some(false) => v.push("pad window exhausted".to_string()),
            None => v.push("pad freshness not established".to_string()),
        }
        if !self.capture.is_safe() {
            v.push(self.capture.describe().to_string());
        }
        if self.lock < LockCapability::ResidentNoDump {
            v.push(self.lock.claim().to_string());
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use holonomy_core::unlock::ProtectionLayer;

    fn healthy() -> StatusLine {
        StatusLine {
            protection: ProtectionLayer {
                pad: true,
                pad_fresh: Some(true),
                post_quantum: true,
            },
            fresh_saves_remaining: 30,
            device_writable: false,
            lock: LockCapability::DirectMapExcluded,
            capture: CaptureState::Excluded,
            realtime: true,
        }
    }

    #[test]
    fn a_healthy_line_says_so_without_hedging() {
        let s = healthy();
        assert!(s.is_healthy());
        let text = s.render();
        assert!(text.contains("30 of 54"), "{text}");
        assert!(text.contains("direct map"), "{text}");
        assert!(text.contains("excluded from screen capture"), "{text}");
        assert!(s.problems().is_empty());
    }

    #[test]
    fn an_exhausted_pad_is_stated_as_a_downgrade_not_a_number() {
        let mut s = healthy();
        s.protection.pad_fresh = Some(false);
        s.fresh_saves_remaining = 0;
        let text = s.render();
        assert!(text.contains("PAD SPENT"), "{text}");
        assert!(text.contains("not by a one-time pad"), "{text}");
        assert!(!s.is_healthy());
        assert!(s.problems().iter().any(|p| p.contains("pad window")));
    }

    #[test]
    fn a_writable_device_gets_a_different_message_from_a_blocked_one() {
        let mut spent = healthy();
        spent.protection.pad_fresh = Some(false);
        spent.fresh_saves_remaining = 0;

        let mut writable = spent.clone();
        writable.device_writable = true;

        assert!(writable.render().contains("will ratchet"));
        assert!(spent.render().contains("PAD SPENT"));
    }

    #[test]
    fn linux_capture_unsupported_is_never_dressed_up() {
        // The README's claim that Linux has no equivalent must reach the user, not stay a doc.
        let mut s = healthy();
        s.capture = CaptureState::Unsupported;
        assert!(!s.is_healthy());
        assert!(s.render().contains("cannot exclude a window from capture"));
        assert!(s.problems().iter().any(|p| p.contains("cannot exclude")));
    }

    #[test]
    fn an_unlocked_neighbourhood_is_not_a_healthy_rung() {
        // `Resident` means dumpable. Reporting that as fine would be the whole audit's failure mode.
        let mut s = healthy();
        s.lock = LockCapability::Resident;
        assert!(!s.is_healthy());
        assert!(s.render().contains("dumpable"));
    }

    #[test]
    fn normal_priority_is_reported_as_expected_not_as_a_fault() {
        let mut s = healthy();
        s.realtime = false;
        let text = s.render();
        assert!(text.contains("normal priority"));
        assert!(text.contains("detection is unaffected"));
        // and it does not make the session unhealthy: priority is latency, not function
        assert!(s.is_healthy());
    }

    #[test]
    fn past_window_is_reported_as_at_least_one() {
        // `past_window_by` exists only for the message; reporting 0 past the window would read as
        // "you are at the boundary" when the budget is already gone.
        let mut s = healthy();
        s.protection.pad_fresh = Some(false);
        s.fresh_saves_remaining = 0;
        assert!(s.past_window_by() >= 1);
    }
}

#[cfg(test)]
mod unknown_freshness_tests {
    use super::*;
    use holonomy_core::unlock::ProtectionLayer;

    /// A caller that cannot establish freshness must not get the fresh-pad sentence, and must not
    /// be told a definite reuse story either.
    #[test]
    fn unknown_freshness_is_stated_as_unknown() {
        let line = StatusLine {
            protection: ProtectionLayer {
                pad: true,
                pad_fresh: None,
                post_quantum: true,
            },
            fresh_saves_remaining: 54,
            device_writable: false,
            lock: LockCapability::DirectMapExcluded,
            capture: CaptureState::Excluded,
            realtime: true,
        };
        let text = line.render();
        assert!(text.contains("freshness not established"), "{text}");
        assert!(!text.contains("fresh saves left"), "{text}");
        assert!(!text.contains("PAD SPENT"), "{text}");
        assert!(!line.is_healthy());
        assert!(line
            .problems()
            .iter()
            .any(|p| p.contains("not established")));
        assert_eq!(line.past_window_by(), 0);
    }
}
