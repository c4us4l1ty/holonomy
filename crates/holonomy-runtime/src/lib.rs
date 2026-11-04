//! The non-cryptographic machinery that has to be right for the rest to mean anything: the PHY
//! reaper, and the secret arenas it is required to zero.
//!
//! Deliberately small. `v5.0.0-OMEGA` folded anti-debugging, key selection and the watchdog into
//! one "branchless" subsystem; v5.1.0 keeps only the part that works — a watchdog whose emergency
//! thread performs no I/O at all — and moves everything else to where it can be tested.
//!
//! # The reaper's contract
//!
//! `Thread A` may block forever in `pread` after a USB yank (the SCSI error handler holds it in
//! `TASK_UNINTERRUPTIBLE` for 5–30 s). `Thread B` therefore never touches a descriptor, a path, or
//! the allocator: it sleeps for 1.5 ms, reads two atomics, and either loops or wipes. See
//! [`reaper`].
//!
//! **Known ordering issue, flagged not fixed:** `EmergencyActions::terminate` currently runs after
//! `blackout`, so an unresponsive compositor can delay termination. The correct order is
//! wipe → terminate → best-effort blackout. This changes visible behaviour on Windows and macOS
//! (the window closes rather than blanks) so it is awaiting a decision — see
//! `docs/AUDIT-v5.1.0.md` §8 question 4.
//!
//! # What the budget means
//!
//! The 15 ms ceiling decomposes as `1.5 ms` poll quantum `+` measured wipe `+` measured blackout.
//! [`scrub::worst_case_wipe_ns`] exists so the middle term is computed from the size of the secret
//! set rather than assumed, and [`reaper::ReaperGuard::measure`] so it is measured on the host that
//! will run it. Wiping all 180 MiB of steady state at legacy DDR2 bandwidth takes ~45 ms, which is
//! **over** the ceiling — the reaper therefore wipes the registered secret arenas (keys, scanout,
//! plaintext pages), a few MiB, and evaporates the rest by `_exit` rather than by scrubbing it.
//! That is the honest accounting the old PRD did not do.

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

/// The decoupled, zero-I/O PHY reaper.
pub mod reaper;
/// Secret arenas and the bounded wipe the reaper performs.
pub mod scrub;

pub use reaper::{
    EmergencyActions, EmergencyTimeline, FireReason, PhyHeartbeat, ReaperDecision, ReaperGuard,
    SchedClass,
};
pub use scrub::{SecretAreas, ScrubError};
