//! The Ledger — Block 1's gate machinery.
//!
//! Every previous generation of Holonomy learned the same lesson separately: a performance or
//! security claim that is not measured on every commit is a rumor. H1 invented the counting
//! allocator for its keystroke gate; v5.1.0 wrote honest budgets into its audits. This crate
//! makes both permanent:
//!
//! * [`alloc`] — a global allocator that counts, so "no allocation on this path" is a failing
//!   test when it stops being true.
//! * [`laws`] — the VISION.md constitutive budgets as data, single-sourced here and rendered
//!   into `docs/GATES.md` by `xtask` so the human-readable contract cannot drift from the code.
//! * [`timing`] — minimal measurement harness used to turn laws from `Spec` into `Gated`.

/// The counting allocator and allocation-free claim scopes.
pub mod alloc;
/// The constitutive laws, as compile-checked data (source of truth for `docs/GATES.md`).
pub mod laws;
/// Timing measurement for gates.
pub mod timing;
