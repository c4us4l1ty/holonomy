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

use holonomy_seal::{describe_how, probe_capability, CanarySet, LockCapability, SecretArenas};
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
    /// Whether an emergency memory reaper is armed for this session.
    pub reaper_armed: bool,
    /// Number of secret/plaintext arenas currently registered with the reaper.
    pub armed_arenas: usize,
    /// Total canary buffers sampled by the reaper registry.
    pub canaries_sampled: usize,
    /// Canary buffers still carrying non-zero bytes (`canaries_sampled` before wipe; `0` after).
    pub ghost_canaries: usize,
    /// Whether the emergency reaper tripwire has fired.
    pub reaper_tripped: bool,
    /// Whether the watcher waits for the editor to stop before registering plaintext and wiping.
    pub reaper_cooperative: bool,
}

impl Facts {
    /// Assembles the facts from the live resident set, the live document and a real lock probe.
    #[must_use]
    pub fn gather<S: SegmentSource>(
        residency: &ResidentSet<S>,
        document: &Document,
        verified_segments: Option<u64>,
    ) -> Self {
        Self::gather_with(residency, document, probe_capability(), verified_segments)
    }

    /// Assembles the facts from the live resident set, the live document and a previously probed
    /// lock capability, so an open security panel refreshes live counts without re-probing `mlock`
    /// on idle ticks (L-5).
    #[must_use]
    pub fn gather_with<S: SegmentSource>(
        residency: &ResidentSet<S>,
        document: &Document,
        capability: LockCapability,
        verified_segments: Option<u64>,
    ) -> Self {
        Self {
            resident_bytes: residency.resident_bytes(),
            allocated_bytes: residency.allocated_bytes(),
            budget_bytes: residency.budget_bytes(),
            resident_segments: residency.resident_segments(),
            capability,
            segments: document.segment_count(),
            witnesses: document.witness_count(),
            recovered_bytes: document.trailing_bytes(),
            verified_segments,
            reaper_armed: false,
            armed_arenas: 0,
            canaries_sampled: 0,
            ghost_canaries: 0,
            reaper_tripped: false,
            reaper_cooperative: false,
        }
    }

    /// Enriches these facts with live [`SecretArenas`] and [`CanarySet`] observations (`L-7`,
    /// `UI-SPEC.md` §6).
    #[must_use]
    pub fn with_reaper(
        mut self,
        arenas: &SecretArenas,
        canaries: &CanarySet,
        tripped: bool,
    ) -> Self {
        let report = canaries.sample_ghosts();
        self.reaper_armed = true;
        self.armed_arenas = arenas.armed_count();
        self.canaries_sampled = report.canaries_sampled;
        self.ghost_canaries = report.ghost_canaries;
        self.reaper_tripped = tripped;
        self
    }

    /// Reports a cooperative watcher that cannot wipe until the editor yields control.
    #[must_use]
    pub fn with_cooperative_reaper(
        self,
        arenas: &SecretArenas,
        canaries: &CanarySet,
        tripped: bool,
    ) -> Self {
        let mut facts = self.with_reaper(arenas, canaries, tripped);
        facts.reaper_cooperative = true;
        facts
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

    /// Live emergency memory-reaper status (`UI-SPEC.md` §6, `L-7`).
    #[must_use]
    pub fn reaper_line(&self) -> String {
        if !self.reaper_armed {
            return "reaper: unarmed".to_owned();
        }
        if self.reaper_cooperative && !self.reaper_tripped {
            return format!(
                "reaper: cooperative ({} canaries; editor must yield before wipe)",
                self.canaries_sampled
            );
        }
        if self.reaper_tripped {
            if self.armed_arenas == 0 {
                // Nothing was registered to scrub. "0/8 zeroized" would read as a defeat when the
                // truth is that the set was empty, which is the failure worth naming (D-89).
                return format!(
                    "reaper: TRIPPED (no arena armed — nothing was proven wiped, {} canaries live)",
                    self.ghost_canaries
                );
            }
            let zeroized = self.canaries_sampled.saturating_sub(self.ghost_canaries);
            return format!(
                "reaper: TRIPPED ({} arenas, {zeroized}/{} canaries zeroized){}",
                self.armed_arenas,
                self.canaries_sampled,
                if self.ghost_canaries == 0 {
                    ""
                } else {
                    " — GHOSTS SURVIVED"
                }
            );
        }
        format!(
            "reaper: armed ({} arenas, {}/{} canaries live)",
            self.armed_arenas, self.ghost_canaries, self.canaries_sampled
        )
    }

    /// The panel, top to bottom.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        vec![
            self.resident_line(),
            self.lock_line(),
            self.reaper_line(),
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
    let (unit, name) = if bytes < MIB {
        (KIB, "KiB")
    } else {
        (MIB, "MiB")
    };
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
            reaper_armed: true,
            armed_arenas: 6,
            canaries_sampled: 4,
            ghost_canaries: 4,
            reaper_tripped: false,
            reaper_cooperative: false,
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
        assert_eq!(
            changed.lock_line(),
            format!("lock: {}", LockCapability::None.claim())
        );

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

        let mut changed = base;
        changed.armed_arenas = 11;
        assert_ne!(base.reaper_line(), changed.reaper_line());
        assert!(changed.reaper_line().contains("11 arenas"));

        let mut changed = base;
        changed.reaper_tripped = true;
        changed.ghost_canaries = 0;
        assert!(changed.reaper_line().contains("TRIPPED"));
        assert!(changed.reaper_line().contains("4/4 canaries zeroized"));
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

    // --- D-89: the reaper line distinguishes a clean wipe from one that proved nothing ----------

    #[test]
    fn a_wipe_with_no_arena_armed_is_not_reported_as_zeroized() {
        // The defect this pins: a tripped reaper over an empty registry previously rendered
        // "TRIPPED (0 arenas, 4/4 canaries zeroized)" from a stale `ghost_canaries` count, which
        // reads as a successful wipe when nothing had been registered to wipe.
        let mut facts = facts();
        facts.reaper_tripped = true;
        facts.armed_arenas = 0;
        let line = facts.reaper_line();
        assert!(line.contains("no arena armed"), "{line}");
        assert!(line.contains("nothing was proven wiped"), "{line}");
        assert!(
            !line.contains("zeroized"),
            "an empty registry must not be described as zeroized: {line}"
        );
    }

    #[test]
    fn a_tripped_reaper_with_a_surviving_canary_says_ghosts_survived() {
        let mut facts = facts();
        facts.reaper_tripped = true;
        facts.armed_arenas = 3;
        facts.canaries_sampled = 8;
        facts.ghost_canaries = 1;
        let line = facts.reaper_line();
        assert!(line.contains("GHOSTS SURVIVED"), "{line}");
        assert!(line.contains("7/8 canaries zeroized"), "{line}");
    }
}
