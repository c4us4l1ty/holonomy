//! Export: streaming HTML from CAGR leaves, and PDF via `pdf-writer`.
//!
//! Both write to a **pre-opened** fd. The jail has no `open`, so an exporter that
//! reaches for a path is a bug that only shows up once seccomp is installed.
//!
//! `pdf-writer` (80 KB) replaced H2's Typst pipeline, which would have pulled a
//! multi-megabyte dependency tree into a 2.5 MiB binary and needed `fork`/`exec` —
//! impossible inside the jail. PROJECT.md §2.3 records this as a deliberate deviation.
//!
//! Lands in Phase 8. Gate: HTML escapes correctly and applies spans; the PDF opens and
//! its text extracts back to the source. See PROJECT.md §5 Phase 8.

pub use holonomy_jail::PHASE_0_PLACEHOLDER;
