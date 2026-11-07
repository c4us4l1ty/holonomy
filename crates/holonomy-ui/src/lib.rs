//! Holonomy's unfinished integration shell around `wordcraft`.
//!
//! The container save path lives in `holonomy-wordcraft` and `holonomy-cli`; this crate currently
//! reports completed store results and capture-exclusion status. Its reporting layer does not
//! perform I/O, track pad use across containers, or prove that a reported ratchet reached hardware.
//! The binary is still a placeholder, and this crate is not a production editor.

#![deny(missing_docs)]

/// OS-level capture exclusion, and the platforms that do not have it.
pub mod capture;
/// Projection of completed container saves into UI-visible status.
pub mod shell;

pub use capture::{CaptureExclusion, CaptureReport};
pub use shell::{SaveOutcome, Session, SessionError};
