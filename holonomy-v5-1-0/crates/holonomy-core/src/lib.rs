//! Geometry, capsule layout, sector permutation, duress bookkeeping and statistics for the
//! `.wavefunction` container, at `v5.1.0`.
//!
//! This crate is deliberately **pure**: no I/O, no OS calls, no cryptographic primitives. Every
//! claim it makes is arithmetic or a state-machine invariant, and every one of them is a test.
//! The things that need a disk live in `holonomy-usb`; the things that need a cipher live in
//! `holonomy-crypto`; the things that need a thread live in `holonomy-runtime`.
//!
//! # What changed from `v5.0.0-OMEGA`
//!
//! Five invariants were removed outright rather than repaired, because they were unsound rather
//! than unfinished. The reasoning lives in `docs/PRD-v5.1.0.md`; the code that remains here is
//! what survived:
//!
//! | v5.0.0 | v5.1.0 |
//! | :--- | :--- |
//! | `CryptScanline` 1-bit pre-baked pages | deleted — 10,000 pages needed 1,027 MiB, 8× the container |
//! | SPOC ±42 constant-variance flicker | deleted — defeated by cross-correlation; an epilepsy hazard |
//! | `.text`/`RIP`/`RDTSC` mixed into the keystream | deleted — non-deterministic by construction |
//! | two live `vault_handles` selected branchlessly | replaced by one [`stratum::ActiveStratum`] |
//! | 16.60 MiB RSS / zero-heap | replaced by measured budgets (see [`budgets`]) |
//! | 20-bit sector permutation | 22-bit permutation — the smaller domain could not reach 41% of the drive; neither is a reviewed FPE |
//! | `±0.015%` Hamming tolerance (9.83σ) | `±0.0053%` (3.5σ) plus a longest-run gate |

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// The real memory budget: 180 MiB steady, 440 MiB unlock peak, and the gate that checks it.
pub mod budgets;
/// `StratumRootCapsuleV5`: the 65,536-byte, 4,096-aligned stratum root node.
pub mod capsule;
/// Forward-only duress co-evolution: effort-matched, generated-not-stored decoy deltas.
pub mod duress;
/// Custom 22-bit sector permutation; bijective placement only, not a reviewed FPE or PRP.
pub mod feistel;
/// Container geometry, as exact constants.
pub mod layout;
/// Indistinguishability gates: 3.5σ monobit and the longest-run test.
pub mod stats;
/// Strata, capability keys and the one-stratum-per-unlock rule.
pub mod stratum;
/// The unlock state machine: passphrase in, exactly one `ActiveStratum` out.
pub mod unlock;

pub use capsule::{CapsuleWire, EpochSalt, StratumRootCapsuleV5};
pub use stratum::{ActiveStratum, StratumId};
pub use unlock::{ProtectionLayer, SealedPassphrase, Topology, UnlockError, Unlocked};

/// The version string written into export metadata and printed by `--version`.
pub const VERSION: &str = "5.1.0";
