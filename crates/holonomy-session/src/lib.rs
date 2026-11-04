//! The session layer: everything the editor needs that does not itself draw anything.
//!
//! # Why this crate exists
//!
//! `holonomy-ui` owns the `wordcraft` integration, and with it six `wordcraft` crates plus their
//! pinned `craft-*` git dependencies. In the environment this project was authored, that graph does
//! not resolve; on a GitHub runner it resolves but transitively pulls `wordcraft-engine` →
//! `wordcraft-pdf` → `craft-pdf`, which does not compile. Either way the consequence was the same
//! and it was bad: **the only crate that could tell a user whether their container was intact could
//! not be built**, and the measured-memory gate had nothing to measure.
//!
//! So the session logic lives here, with no UI and no `wordcraft` in its dependency graph:
//! pad accounting, the save cadence, the wipe sequence, the status line and the health checks that
//! `holonomy --check` runs. It is small, it compiles in CI, and it has tests that execute.
//!
//! # What is deliberately not here
//!
//! No rendering, no window, no egui, no document model. Those stay in `holonomy-ui`, which is a thin
//! adapter over this crate once its dependency graph becomes buildable. The rule this crate is
//! meant to enforce: *nothing that decides whether your secrets get wiped may live in a crate that
//! cannot be compiled.*

pub mod check;
pub mod directory;
pub mod salt;
pub mod seal;
pub mod session;
pub mod status;

pub use check::{CheckReport, CheckResult, Severity};
pub use directory::{DirectoryError, DirectoryLedger};
pub use salt::{ContainerHeader, SaltBytes, SaltOrigin};
pub use seal::{open_payload_slot, seal_container, SealError, SealedContainer, SealedPayloadSlot};
pub use session::{SaveOutcome, Session, SessionError};
pub use status::StatusLine;

/// Version reported by `--version` and stamped into check output.
pub const VERSION: &str = holonomy_core::VERSION;
