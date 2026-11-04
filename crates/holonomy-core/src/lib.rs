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
//! | `FPE-20` over a 6.80 GiB zone | `FPE-22` — the 20-bit domain could not reach 41% of the drive |
//! | `±0.015%` Hamming tolerance (9.83σ) | `±0.0053%` (3.5σ) plus a longest-run gate |

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod budgets;
pub mod capsule;
pub mod duress;
pub mod feistel;
pub mod layout;
pub mod stats;
pub mod stratum;
pub mod unlock;

pub use capsule::{CapsuleWire, EpochSalt, StratumRootCapsuleV5};
pub use stratum::{ActiveStratum, StratumId};
pub use unlock::{ProtectionLayer, SealedPassphrase, Topology, UnlockError, Unlocked};

/// The version string written into export metadata and printed by `--version`.
pub const VERSION: &str = "5.1.0";
