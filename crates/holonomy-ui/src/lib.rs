//! Holonomy's shell around `wordcraft`.
//!
//! # Division of labour
//!
//! `wordcraft` is the editor: document model (`wordcraft-doc`), text shaping and font handling
//! (`wordcraft-fonts`, `skrifa` + `harfrust`), layout (`wordcraft-layout`), CPU rasterisation
//! (`wordcraft-render`, `vello_cpu`) and the egui front end (`wordcraft-ui-egui`). Holonomy adds
//! three things and nothing else:
//!
//! 1. **Container binding** — a document is a `.wavefunction` slot set, sealed with a stratum's
//!    `K_DEM`, not a file on disk.
//! 2. **The save cadence** — every save advances the pad cursor, possibly ratchets, regenerates
//!    `σ_epoch`, re-permutes 2,047 slots and co-evolves the duress stratum.
//! 3. **The session's honest reporting** — which protection layer is active, whether the reaper
//!    got real-time priority, whether capture exclusion succeeded.
//!
//! # What it deliberately does *not* do
//!
//! It does not render the document itself, and it does not implement a text engine. The previous
//! repository did both, in 142,500 lines, and the audit's conclusion was that the result was
//! neither better than `wordcraft` nor maintainable. [`shell`] is where the integration happens;
//! everything below it is upstream.

#![deny(missing_docs)]

/// OS-level capture exclusion, and the platforms that do not have it.
pub mod capture;
/// The session shell: what a save does and what it may claim afterwards.
pub mod shell;

pub use capture::{CaptureExclusion, CaptureReport};
pub use shell::{Session, SessionConfig, SessionError};
