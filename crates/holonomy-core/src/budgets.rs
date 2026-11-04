//! The real memory budget, as constants, so the NFR cannot drift from the code again.
//!
//! # What was wrong with `v5.0.0-OMEGA` §6.2
//!
//! It published a plausible-looking table — scanout 7.81, atlas 0.13, Fenwick 0.50, CryptRope
//! 6.50, piece table 1.20, sentinel 0.46 — totalling **16.60 MiB** for a 10,000-page document on a
//! 2 GiB machine. The table omitted the three largest allocations in the system:
//!
//! | omitted | size |
//! | :--- | ---: |
//! | the `mlock`ed 128 MiB `.wavefunction` container (it must be resident to be folded/decrypted) | 128 MiB |
//! | the 128 MiB USB pad, or 32,768 random 4 KiB sector reads per save | 128 MiB |
//! | the `Argon2id` arena, `m = 262,144 KiB` | 256 MiB |
//!
//! `16.60 MiB` was off by a factor of 16 in steady state and 33 at the unlock peak. That is not a
//! rounding error; it is a budget for a different program.
//!
//! # The numbers that are actually true
//!
//! * **Steady state `≤ 180 MiB`**: 128 MiB container + ~50 MiB for `wordcraft`'s document model,
//!   glyph cache, `vello_cpu` scratch surfaces and the egui context. The 128 MiB term dominates
//!   and is not negotiable — a container that is not resident cannot be authenticated, because
//!   `CMT-4` binds the whole `K_DEM`-encrypted payload.
//! * **Unlock peak `≤ 440 MiB`**: steady state plus the 256 MiB `Argon2id` arena, which is freed
//!   before the first slot is decrypted and therefore never overlaps the payload working set by
//!   more than the arena-free-to-first-decrypt gap.
//!
//! Both fit inside 2,048 MiB with the host OS still running, which is the point the old table
//! obscured rather than established. [`check_against_envelope`] is the gate.
//!
//! # What is no longer claimed
//!
//! The "17.5 MiB RSS / zero-heap ceiling" invariant is gone. It was incompatible with
//! `wordcraft`, whose document model, layout engine and rasteriser all allocate. v5.1.0 keeps
//! *sensitive* data out of the global allocator through `zeroize`d, page-locked arenas owned by
//! `holonomy-crypto`; it does not pretend the rest of the editor is allocation-free.

use crate::layout::CONTAINER_BYTES;

/// MiB in bytes.
pub const MIB: usize = 1024 * 1024;

/// The resident container, `mlock`ed for the lifetime of an unlock.
pub const CONTAINER_RESIDENT_BYTES: usize = CONTAINER_BYTES;

/// `Argon2id` Stage 1 arena, `m = 262,144 KiB`. Transient.
pub const ARGON2_ARENA_BYTES: usize = 256 * MIB;

/// Upper bound on `wordcraft` state: document model, glyph cache, layout arenas and two
/// `vello_cpu` surfaces for a 1280×800 window.
///
/// Measured, not guessed: egui + vello_cpu residency for an idle editor with a short document is
/// a few MiB; this ceiling allows a large document and a full glyph cache without pretending to
/// be small. It is an *allowance*, and [`check_against_envelope`] fails if the sum breaches the
/// envelope — it does not fail if a build happens to use less.
pub const WORDCRAFT_STATE_ALLOWANCE_BYTES: usize = 50 * MIB;

/// Small fixed structures: reaper stack, sentinel side-channel, sector-index buffers.
pub const FIXED_OVERHEAD_BYTES: usize = 2 * MIB;

/// Steady-state ceiling. `128 + 50 + 2 = 180`.
pub const STEADY_STATE_CEILING_BYTES: usize =
    CONTAINER_RESIDENT_BYTES + WORDCRAFT_STATE_ALLOWANCE_BYTES + FIXED_OVERHEAD_BYTES;

/// Unlock-peak ceiling. `180 + 256 = 436`, rounded up to the documented 440.
pub const UNLOCK_PEAK_CEILING_BYTES: usize =
    STEADY_STATE_CEILING_BYTES + ARGON2_ARENA_BYTES + 4 * MIB;

/// The target machine's physical RAM: 2,048 MiB.
pub const LEGACY_HOST_RAM_BYTES: usize = 2048 * MIB;

/// Headroom the host OS needs to be considered "still running".
///
/// A 2 GiB machine running a desktop session plus a browser does not have 1.5 GiB free. Requiring
/// a fixed 512 MiB floor is a deliberately conservative rule of thumb: below it, the unlock peak
/// is more likely to hit the swap file, and a swapped-out container is a copy of the container on
/// disk that the user believed was never written anywhere.
pub const HOST_MIN_FREE_BYTES: usize = 512 * MIB;

/// Why a budget fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetViolation {
    /// Steady state exceeds [`STEADY_STATE_CEILING_BYTES`].
    SteadyStateTooLarge {
        /// The measured or projected figure.
        bytes: usize,
    },
    /// Unlock peak exceeds [`UNLOCK_PEAK_CEILING_BYTES`].
    UnlockPeakTooLarge {
        /// The measured or projected figure.
        bytes: usize,
    },
    /// A published figure claims less than the system's mandatory terms.
    ///
    /// This is the direction an over-budget check cannot catch, and it is exactly how §6.2 came
    /// to say 16.60 MiB: a number too *small* is not a violation of an upper bound, so a gate that
    /// only rejects large values passes a false claim silently.
    ClaimBelowMandatoryFloor {
        /// The figure being claimed.
        claimed: usize,
        /// The sum of terms that certainly exist: the resident container plus the transient
        /// `Argon2id` arena, which cannot both be zero.
        floor: usize,
    },
    /// Peak plus host floor exceeds physical RAM.
    DoesNotFitLegacyHost {
        /// The unlock peak.
        peak: usize,
        /// [`HOST_MIN_FREE_BYTES`].
        host_floor: usize,
        /// [`LEGACY_HOST_RAM_BYTES`].
        ram: usize,
    },
}

impl BudgetViolation {
    /// Whether this describes a claim that is too small rather than one that is too large.
    pub fn is_understatement(&self) -> bool {
        matches!(self, Self::ClaimBelowMandatoryFloor { .. })
    }
}

/// The terms that must be paid at least once in any real session.
///
/// The container has to be resident to be authenticated (`CMT-4` binds the whole payload), and the
/// `Argon2id` arena has to exist to derive the root secret. They do not overlap in time, so the
/// floor is the larger of the two rather than the sum — but any claim below it is describing a
/// program that either skips authentication or skips key derivation.
pub const MANDATORY_FLOOR_BYTES: usize = if CONTAINER_RESIDENT_BYTES > ARGON2_ARENA_BYTES {
    CONTAINER_RESIDENT_BYTES
} else {
    ARGON2_ARENA_BYTES
};

impl std::fmt::Display for BudgetViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SteadyStateTooLarge { bytes } => write!(
                f,
                "steady state {} MiB exceeds the {} MiB ceiling",
                bytes / MIB,
                STEADY_STATE_CEILING_BYTES / MIB
            ),
            Self::UnlockPeakTooLarge { bytes } => write!(
                f,
                "unlock peak {} MiB exceeds the {} MiB ceiling",
                bytes / MIB,
                UNLOCK_PEAK_CEILING_BYTES / MIB
            ),
            Self::ClaimBelowMandatoryFloor { claimed, floor } => write!(
                f,
                "claim of {} MiB is below the {} MiB floor: the container and the Argon2id arena \
                 cannot both be absent",
                claimed / MIB,
                floor / MIB
            ),
            Self::DoesNotFitLegacyHost {
                peak,
                host_floor,
                ram,
            } => write!(
                f,
                "peak {} MiB + host floor {} MiB exceeds {} MiB of physical RAM",
                peak / MIB,
                host_floor / MIB,
                ram / MIB
            ),
        }
    }
}

/// Verify a measured (or projected) footprint against the envelope.
///
/// Call this in CI with measured RSS. A budget that is only checked by hand is a budget that
/// drifts, which is exactly how §6.2 came to say 16.60 MiB.
pub fn check_against_envelope(
    steady_state: usize,
    unlock_peak: usize,
) -> Result<(), BudgetViolation> {
    // Both directions first: an understated claim is not an upper-bound violation, so the order
    // matters. Checking the ceiling first would let `16.60 MiB` through as "fine".
    if unlock_peak < MANDATORY_FLOOR_BYTES {
        return Err(BudgetViolation::ClaimBelowMandatoryFloor {
            claimed: unlock_peak,
            floor: MANDATORY_FLOOR_BYTES,
        });
    }
    if steady_state < ARGON2_ARENA_BYTES.min(CONTAINER_RESIDENT_BYTES) {
        return Err(BudgetViolation::ClaimBelowMandatoryFloor {
            claimed: steady_state,
            floor: ARGON2_ARENA_BYTES.min(CONTAINER_RESIDENT_BYTES),
        });
    }
    if steady_state > STEADY_STATE_CEILING_BYTES {
        return Err(BudgetViolation::SteadyStateTooLarge {
            bytes: steady_state,
        });
    }
    if unlock_peak > UNLOCK_PEAK_CEILING_BYTES {
        return Err(BudgetViolation::UnlockPeakTooLarge { bytes: unlock_peak });
    }
    if unlock_peak + HOST_MIN_FREE_BYTES > LEGACY_HOST_RAM_BYTES {
        return Err(BudgetViolation::DoesNotFitLegacyHost {
            peak: unlock_peak,
            host_floor: HOST_MIN_FREE_BYTES,
            ram: LEGACY_HOST_RAM_BYTES,
        });
    }
    Ok(())
}

/// What a measured process actually did.
///
/// [`check_against_envelope`] demands a *floor* as well as a ceiling, because it validates a
/// **claim**: a figure published about the product must be big enough to describe the thing it
/// claims to describe, and that is how the old 16.60 MiB number fails. A **measurement** answers a
/// different question. A process that verified constants and probed the host -- which is what
/// `holonomy --check` does -- legitimately occupies a few MiB, and comparing that to the floor
/// reports a correct measurement as a violation of a rule about something else. That conflation
/// made `--check` fail its own CI gate with "claim of 17 MiB is below the 128 MiB floor" while
/// every actual gate passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workload {
    /// The process opened the container and derived keys. The floor applies.
    Unlocked,
    /// The process opened the container only. The container floor applies, not the arena floor.
    Container,
    /// The process opened nothing: constants, host probes, and its own memory. Only the ceiling
    /// applies, and no statement is made about the product's footprint.
    Idle,
}

impl Workload {
    /// The floor this workload must clear, if any.
    pub fn floor_bytes(self) -> usize {
        match self {
            Workload::Unlocked => MANDATORY_FLOOR_BYTES,
            Workload::Container => CONTAINER_RESIDENT_BYTES,
            Workload::Idle => 0,
        }
    }
}

/// Verify a measurement against the envelope, given what the process did.
///
/// Unlike [`check_against_envelope`], which judges a published claim, this never fails for being
/// *small*. It fails for being too large, or for being too small given a workload that should have
/// produced a large footprint -- a process claiming to have unlocked a 64 MiB container while
/// measuring 20 MiB has not unlocked it.
pub fn check_measurement(measured: usize, workload: Workload) -> Result<(), BudgetViolation> {
    if measured > UNLOCK_PEAK_CEILING_BYTES {
        return Err(BudgetViolation::UnlockPeakTooLarge { bytes: measured });
    }
    let floor = workload.floor_bytes();
    if measured < floor {
        // Reaching here with a floor of zero is impossible, so this arm only fires for a real
        // discrepancy: the workload says the footprint should be at least `floor` and it is not.
        return Err(BudgetViolation::ClaimBelowMandatoryFloor {
            claimed: measured,
            floor,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_numbers_are_the_computed_numbers() {
        assert_eq!(CONTAINER_RESIDENT_BYTES / MIB, 128);
        assert_eq!(ARGON2_ARENA_BYTES / MIB, 256);
        assert_eq!(STEADY_STATE_CEILING_BYTES / MIB, 180);
        // 440 is the documented figure; the arithmetic is 436 + 4 MiB of slack.
        assert_eq!(UNLOCK_PEAK_CEILING_BYTES / MIB, 440);
    }

    #[test]
    fn envelope_holds_on_the_legacy_host() {
        // 440 + 512 = 952 MiB, well inside 2,048 MiB.
        check_against_envelope(STEADY_STATE_CEILING_BYTES, UNLOCK_PEAK_CEILING_BYTES)
            .expect("the documented envelope must fit a 2 GiB host");
        // A relationship, not a literal: the peak plus the host floor must leave the host room.
        let headroom = LEGACY_HOST_RAM_BYTES - UNLOCK_PEAK_CEILING_BYTES - HOST_MIN_FREE_BYTES;
        assert!(
            headroom > MIB,
            "only {headroom} bytes of headroom on the legacy host"
        );
    }

    #[test]
    fn old_claim_is_rejected_as_an_understatement() {
        // The point of the module: 16.60 MiB is not "too large", it is impossible. A one-sided
        // upper-bound gate would accept it, which is precisely how §6.2 survived review.
        let old_claim = 16 * MIB + 614 * 1024;
        let err = check_against_envelope(old_claim, old_claim).unwrap_err();
        assert!(
            err.is_understatement(),
            "a claim smaller than the container must be reported as an understatement, got {err:?}"
        );
        assert!(
            !err.to_string().contains("exceeds"),
            "it does not exceed anything: {err}"
        );
        assert!(
            CONTAINER_RESIDENT_BYTES > old_claim,
            "the container alone is larger than the entire old budget"
        );
    }

    #[test]
    fn a_real_peak_over_the_ceiling_fails() {
        // Steady state has to be legal for the *peak* check to be the one that fires; passing an
        // illegal steady value just tests the first branch twice.
        let err = check_against_envelope(170 * MIB, 441 * MIB).unwrap_err();
        assert!(
            matches!(err, BudgetViolation::UnlockPeakTooLarge { .. }),
            "got {err:?}"
        );
        assert!(err.to_string().contains("440"));
    }

    #[test]
    fn an_idle_measurement_is_not_judged_against_the_claim_floor() {
        // The bug this exists to prevent: `holonomy --check` verified constants and probed the
        // host, measured 17 MiB, and was told "claim of 17 MiB is below the 128 MiB floor". The
        // measurement was right; the rule was being applied to the wrong kind of number.
        assert!(check_measurement(17 * MIB, Workload::Idle).is_ok());
        assert!(check_measurement(1 * MIB, Workload::Idle).is_ok());
        // The same figure against `check_against_envelope` is still a violation, because there it
        // is a claim about the product rather than an observation of one process.
        assert!(check_against_envelope(17 * MIB, 17 * MIB)
            .unwrap_err()
            .is_understatement());
    }

    #[test]
    fn a_workload_that_should_be_big_cannot_measure_small() {
        // The floor is not abandoned, it is attached to the workload. A process claiming to have
        // unlocked the container and reporting 20 MiB has not unlocked it.
        let err = check_measurement(20 * MIB, Workload::Unlocked).unwrap_err();
        assert!(err.is_understatement(), "{err:?}");
        // The floor is the larger of the container and the Argon2id arena -- 256 MiB, not the
        // container's 128. Assert the rendered number against the constant rather than a literal,
        // because writing 128 here was wrong and only a failed run said so.
        assert!(
            err.to_string()
                .contains(&(MANDATORY_FLOOR_BYTES / MIB).to_string()),
            "the message must name the floor it enforced: {err}"
        );
        assert_eq!(Workload::Unlocked.floor_bytes(), MANDATORY_FLOOR_BYTES);
        // And the container-only rung sits below the full unlock floor, but is still a floor.
        assert!(Workload::Container.floor_bytes() < Workload::Unlocked.floor_bytes());
        assert!(check_measurement(20 * MIB, Workload::Container).unwrap_err().is_understatement());
        assert!(check_measurement(CONTAINER_RESIDENT_BYTES, Workload::Container).is_ok());
    }

    #[test]
    fn a_measurement_is_still_capped_by_the_peak_ceiling() {
        // Idle means "no floor", not "no ceiling". The one direction that always applies is up.
        assert!(check_measurement(UNLOCK_PEAK_CEILING_BYTES, Workload::Idle).is_ok());
        let err = check_measurement(UNLOCK_PEAK_CEILING_BYTES + 1, Workload::Idle).unwrap_err();
        assert!(matches!(err, BudgetViolation::UnlockPeakTooLarge { .. }), "{err:?}");
        // ... and a real unlock that fits the ceiling passes on both counts.
        assert!(check_measurement(300 * MIB, Workload::Unlocked).is_ok());
    }

    #[test]
    fn the_gate_is_two_sided() {
        // Under and over are distinct failures, and both are refused.
        assert!(check_against_envelope(MIB, MIB)
            .unwrap_err()
            .is_understatement());
        assert!(!check_against_envelope(181 * MIB, 440 * MIB)
            .unwrap_err()
            .is_understatement());
        assert!(check_against_envelope(180 * MIB, 440 * MIB).is_ok());
    }
}
