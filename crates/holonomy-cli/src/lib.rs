//! Experimental container store shared by the CLI and future editor integrations.
//!
//! This library is not yet a security-reviewed or stable API. It does not provide
//! USB provisioning, complete pad accounting, or a running editor session.

#![deny(missing_docs)]

/// Conservative document-session state for future frontends.
pub mod document;
/// Container and pad operations used by the command-line frontend.
pub mod store;
