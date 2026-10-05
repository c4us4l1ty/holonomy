//! The Holonomy session, as a library.
//!
//! `main.rs` owns the boot *order*, which is load-bearing and not negotiable (PROJECT.md §5
//! Phase 7):
//!
//! ```text
//!   mlockall -> open container -> allocate -> open DRM -> open evdev
//!     -> unshare -> no_new_privs -> seccomp
//! ```
//!
//! Nothing after seccomp may allocate. Everything that needs a *descriptor* is therefore opened
//! before the chain runs, and this crate's [`session`] only ever consumes descriptors: the container
//! fd, the export fds, the DRM card and the evdev node. See [`session`]'s module docs for why the
//! loop lives in a library rather than in `main`.

pub mod args;
pub mod session;
#[cfg(feature = "desktop")]
pub mod windowed;

pub use args::{Args, ExportTarget, ParseError};
pub use session::{Exit, ExportSink, Session, SessionError, SessionStats, TEST_CHART_PNG};
