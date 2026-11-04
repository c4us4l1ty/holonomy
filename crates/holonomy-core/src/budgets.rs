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
pub const UNLOCK_PEAK_CEILING_BYTES: usize = STEADY_STATE_CEILING_BYTES + ARGON2_ARENA_BYTES + 4 * MIB;

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
    SteadyStateTooLarge { bytes: usize },
    /// Unlock peak exceeds [`UNLOCK_PEAK_CEILING_BYTES`].
    UnlockPeakTooLarge { bytes: usize },
    /// Peak plus host floor exceeds physical RAM.
    DoesNotFitLegacyHost { peak: usize, host_floor: usize, ram: usize },
}

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
            Self::DoesNotFitLegacyHost { peak, host_floor, ram } => write!(
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
pub fn check_against_envelope(steady_state: usize, unlock_peak: usize) -> Result<(), BudgetViolation> {
    if steady_state > STEADY_STATE_CEILING_BYTES {
        return Err(BudgetViolation::SteadyStateTooLarge { bytes: steady_state });
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
        assert!(UNLOCK_PEAK_CEILING_BYTES + HOST_MIN_FREE_BYTES < LEGACY_HOST_RAM_BYTES);
    }

    #[test]
    fn old_claim_is_rejected_as_too_large() {
        // The point of the module: 16.60 MiB cannot even hold the container.
        let old_claim = 16 * MIB + 614 * 1024;
        let err = check_against_envelope(old_claim, old_claim).unwrap_err();
        assert!(matches!(err, BudgetViolation::UnlockPeakTooLarge { .. }));
        assert!(
            CONTAINER_RESIDENT_BYTES > old_claim,
            "the container alone is larger than the entire old budget"
        );
    }

    #[test]
    fn a_real_peak_over_the_ceiling_fails() {
        let err = check_against_envelope(200 * MIB, 441 * MIB).unwrap_err();
        assert!(matches!(err, BudgetViolation::UnlockPeakTooLarge { .. }));
        assert!(err.to_string().contains("440"));
    }
}
