//! What the window may claim about itself — `docs/TO-APP.md` M6.
//!
//! A security panel is only worth having if every number in it comes from the thing it describes.
//! So this module is not a formatter over constants: [`Facts`] is assembled from the live resident
//! set, the live world-line handle and a real `mlock` probe, and each line is a pure function of
//! those fields. The tests mutate a field and watch the line change, which is what makes a
//! hard-coded claim a test failure rather than a matter of review.
//!
//! The three questions, and where the answers come from:
//!
//! 1. **What is resident?** `ResidentSet::resident_bytes`, `allocated_bytes`, `budget_bytes` and
//!    `resident_segments` — the numbers the L-6 rehearsal holds to their bound.
//! 2. **Is it protected?** `holonomy_seal::probe_capability()` — an actual `mlock` of a probe page in
//!    this process, today, not a build-time assumption — reported with the rung's own wording
//!    (`LockCapability::claim`) and the mechanism note (`describe_how`).
//! 3. **What does the file say?** The world-line's committed segment and witness counts, whatever a
//!    previous run left past the last witness, and how many segments the last full authentication
//!    verified.
//!
//! What it must never say: that memory is locked when the probe says otherwise, or that a document
//! is verified when no `verify` has run. `None` is rendered as "not verified", not as zero.

use holonomy_seal::{describe_how, probe_capability, LockCapability};
use holonomy_text::{ResidentSet, SegmentSource};

use crate::store::Document;

/// Every number the panel shows, and the state each came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    /// Bytes of document plaintext currently held in memory.
    pub resident_bytes: usize,
    /// Bytes of capacity currently allocated, editing slack included.
    pub allocated_bytes: usize,
    /// The resident set's bound.
    pub budget_bytes: usize,
    /// How many leaves are resident.
    pub resident_segments: usize,
    /// The strongest memory protection this process achieved, as probed.
    pub capability: LockCapability,
    /// Committed segments in the world-line.
    pub segments: u64,
    /// Committed witnesses, genesis included.
    pub witnesses: u64,
    /// Bytes past the last committed witness that opening discarded.
    pub recovered_bytes: u64,
    /// Segments the last full authentication verified, if one has run.
    pub verified_segments: Option<u64>,
}

impl Facts {
    /// Assembles the facts from the live resident set, the live document and a real lock probe.
    #[must_use]
    pub fn gather<S: SegmentSource>(
        residency: &ResidentSet<S>,
        document: &Document,
        verified_segments: Option<u64>,
    ) -> Self {
        Self {
            resident_bytes: residency.resident_bytes(),
            allocated_bytes: residency.allocated_bytes(),
            budget_bytes: residency.budget_bytes(),
            resident_segments: residency.resident_segments(),
            capability: probe_capability(),
            segments: document.segment_count(),
            witnesses: document.witness_count(),
            recovered_bytes: document.trailing_bytes(),
            verified_segments,
        }
    }

    /// `resident: 4 KiB in 3 leaves, 6.5 KiB allocated of 4 MiB budget`.
    #[must_use]
    pub fn resident_line(&self) -> String {
        format!(
            "resident: {} in {} {}, {} allocated of {} budget",
            human(self.resident_bytes),
            self.resident_segments,
            if self.resident_segments == 1 {
                "leaf"
            } else {
                "leaves"
            },
            human(self.allocated_bytes),
            human(self.budget_bytes)
        )
    }

    /// What this process can honestly claim about the memory it holds, in the rung's own words.
    #[must_use]
    pub fn lock_line(&self) -> String {
        format!("lock: {}", self.capability.claim())
    }

    /// How that protection is attempted on this platform.
    #[must_use]
    pub fn mechanism_line(&self) -> String {
        format!("mechanism: {}", describe_how())
    }

    /// The file's own numbers.
    #[must_use]
    pub fn worldline_line(&self) -> String {
        let verification = match self.verified_segments {
            Some(count) if count == self.segments => format!("verified {count}"),
            Some(count) => format!("verified {count} of {}", self.segments),
            None => "not verified".to_owned(),
        };
        format!(
            "world-line: {} segments, {} witnesses, {verification}",
            self.segments, self.witnesses
        )
    }

    /// What a previous run left behind, and what this one did about it.
    #[must_use]
    pub fn recovery_line(&self) -> String {
        match self.recovered_bytes {
            0 => "recovery: nothing past the last witness".to_owned(),
            1 => "recovery: 1 byte past the last witness was discarded on open".to_owned(),
            bytes => {
                format!("recovery: {bytes} bytes past the last witness were discarded on open")
            }
        }
    }

    /// The panel, top to bottom.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        vec![
            self.resident_line(),
            self.lock_line(),
            self.worldline_line(),
            self.recovery_line(),
        ]
    }
}

/// Bytes as a person reads them: `0 B`, `1023 B`, `1 KiB`, `1.5 KiB`, `1 MiB`.
///
/// Integer arithmetic on purpose — a panel that rounds through `f64` can show `2 KiB` for a
/// thousand and twenty-eight bytes, and the budget numbers are exact.
#[must_use]
pub fn human(bytes: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = KIB * 1024;
    if bytes < KIB {
        return format!("{bytes} B");
    }
    let (unit, name) = if bytes < MIB { (KIB, "KiB") } else { (MIB, "MiB") };
    let whole = bytes / unit;
    let tenth = (bytes % unit) * 10 / unit;
    if tenth == 0 {
        format!("{whole} {name}")
    } else {
        format!("{whole}.{tenth} {name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            resident_bytes: 4096,
            allocated_bytes: 6656,
            budget_bytes: 4 * 1024 * 1024,
            resident_segments: 3,
            capability: LockCapability::ResidentNoDump,
            segments: 5,
            witnesses: 6,
            recovered_bytes: 0,
            verified_segments: Some(5),
        }
    }

    #[test]
    fn bytes_read_as_a_person_reads_them() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1023), "1023 B");
        assert_eq!(human(1024), "1 KiB");
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(human(2048), "2 KiB");
        assert_eq!(human(1024 * 1024), "1 MiB");
        assert_eq!(human(1024 * 1024 + 512 * 1024), "1.5 MiB");
    }

    #[test]
    fn every_line_moves_when_its_fact_moves() {
        // The point of the module: a line is a function of the fact, so a hard-coded claim cannot
        // pass. One field at a time, one line at a time.
        let base = facts();
        assert!(base.resident_line().contains("4 KiB"));
        let mut changed = base;
        changed.resident_bytes = 8192;
        assert_ne!(base.resident_line(), changed.resident_line());
        assert!(changed.resident_line().contains("8 KiB"));

        let mut changed = base;
        changed.resident_segments = 1;
        assert!(changed.resident_line().contains("1 leaf"));

        let mut changed = base;
        changed.capability = LockCapability::None;
        assert_ne!(base.lock_line(), changed.lock_line());
        assert_eq!(changed.lock_line(), format!("lock: {}", LockCapability::None.claim()));

        let mut changed = base;
        changed.segments = 7;
        assert_ne!(base.worldline_line(), changed.worldline_line());

        let mut changed = base;
        changed.verified_segments = None;
        assert!(changed.worldline_line().contains("not verified"));

        let mut changed = base;
        changed.recovered_bytes = 7;
        assert_ne!(base.recovery_line(), changed.recovery_line());
        assert!(changed.recovery_line().contains('7'));
    }

    #[test]
    fn a_partial_verification_says_so() {
        let mut facts = facts();
        facts.verified_segments = Some(2);
        assert!(
            facts.worldline_line().contains("verified 2 of 5"),
            "{}",
            facts.worldline_line()
        );
    }
}
