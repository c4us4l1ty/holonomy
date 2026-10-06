//! Export: streaming HTML from CAGR leaves, and PDF via `pdf-writer`.
//!
//! Both write to a **pre-opened** sink. The jail has no `open`, so an exporter that reaches for a path
//! is a bug that only shows up once seccomp is installed -- as a `SIGSYS` and an exit 137. Neither
//! module here opens anything; both take a `W: Write`, and the session passes a descriptor it opened
//! during boot. See [`holonomy_jail::seccomp::table::ALLOWLIST`] for why there is no `open` in it.
//!
//! # What comes out of what comes in
//!
//! | Document      | HTML                            | PDF                    |
//! |---------------|---------------------------------|------------------------|
//! | `STYLE_BOLD`  | `<b>`                           | `Helvetica-Bold`       |
//! | `STYLE_ITALIC`| `<i>`                           | `Helvetica-Oblique`    |
//! | `STYLE_CODE`  | `<code>`                        | `Courier`              |
//! | `STYLE_HEADER`| `<h1>`, at 1.35x the body size   | at 1.35x the body size |
//!
//! So a document styled one way exports styled the same way in both formats, and the tests for each
//! assert the same mapping. That is the property worth having: an export that silently dropped a
//! style would still open.
//!
//! # Why `pdf-writer` and not H2's Typst pipeline
//!
//! Typst would pull a multi-megabyte dependency tree into a 2.5 MiB binary and need `fork`/`exec` to
//! run -- impossible inside a jail with no `execve`. `pdf-writer` is 80 KB of dependency, writes to a
//! `Vec<u8>` the caller then hands to a descriptor, and embeds nothing. PROJECT.md §2.3 records this as
//! a deliberate deviation.
//!
//! # The one thing a reader cannot check for us
//!
//! Base-14 fonts are *not embedded*, so the metrics that decide where a line breaks are the
//! exporter's, read from the Adobe AFM, while the glyphs drawn are the reader's. If those two disagree
//! the document still opens and the line breaks in the wrong place -- which is why [`fonts`] is
//! transcribed, cross-checked, and tested rather than approximated, and why the PDF exporter reports
//! [`PdfStats::overflowing_runs`] instead of quietly overflowing.
//!
//! [`holonomy_jail::seccomp::table::ALLOWLIST`]: https://docs.rs/holonomy-jail
//! [`fonts`]: crate::fonts
//! [`PdfStats::overflowing_runs`]: crate::pdf::PdfStats::overflowing_runs

pub mod asset;
mod fonts;
pub mod html;
pub mod pdf;

pub use html::{export_body, HtmlError, HtmlOptions, HtmlStats};
pub use pdf::{export, PageSize, PdfError, PdfOptions, PdfStats};

use std::io::Write;

/// What to write, so a caller picks a format once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Streaming HTML.
    Html,
    /// PDF via `pdf-writer`.
    Pdf,
}

impl Format {
    /// The conventional extension.
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Pdf => "pdf",
        }
    }

    /// Parse a `--export-*` argument's format name.
    ///
    /// Returns `None` for anything unknown rather than defaulting: a typo in a CLI flag should say so
    /// instead of quietly producing an HTML file where the user asked for a PDF.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "html" => Some(Self::Html),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }
}

/// Export `editor` in `format` to `sink`.
pub fn write<W: Write>(
    editor: &holonomy_text::Editor,
    sink: &mut W,
    format: Format,
    title: &str,
) -> Result<Report, ExportError> {
    match format {
        Format::Html => {
            let stats = html::export(
                editor,
                sink,
                &HtmlOptions {
                    title: title.to_string(),
                    ..HtmlOptions::default()
                },
            )?;
            Ok(Report {
                format,
                bytes: stats.bytes,
            })
        }
        Format::Pdf => {
            let stats = pdf::export(
                editor,
                sink,
                &PdfOptions {
                    title: title.to_string(),
                    ..PdfOptions::default()
                },
            )?;
            Ok(Report {
                format,
                bytes: stats.bytes,
            })
        }
    }
}

/// What an export cost, for the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// The format written.
    pub format: Format,
    /// Bytes handed to the sink.
    pub bytes: u64,
}

/// Either format's failure.
#[derive(Debug)]
pub enum ExportError {
    /// The HTML path failed.
    Html(HtmlError),
    /// The PDF path failed.
    Pdf(PdfError),
    /// An image could not be decoded for the PDF's `/XObject`.
    ///
    /// Named, with the asset's id in it, because an export that silently dropped a picture would be a
    /// document that changes between formats -- and a caller needs to know *which* asset, not merely
    /// that something went wrong.
    Asset(String),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Html(e) => write!(f, "{e}"),
            Self::Pdf(e) => write!(f, "{e}"),
            Self::Asset(m) => write!(f, "image: {m}"),
        }
    }
}

impl std::error::Error for ExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Html(e) => Some(e),
            Self::Pdf(e) => Some(e),
            // No source: the message is the diagnosis, and there is no upstream error type to point at.
            Self::Asset(_) => None,
        }
    }
}

impl From<HtmlError> for ExportError {
    fn from(e: HtmlError) -> Self {
        Self::Html(e)
    }
}

impl From<PdfError> for ExportError {
    fn from(e: PdfError) -> Self {
        Self::Pdf(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_parse_and_name_themselves() {
        assert_eq!(Format::parse("html"), Some(Format::Html));
        assert_eq!(Format::parse("pdf"), Some(Format::Pdf));
        assert_eq!(Format::Html.extension(), "html");
        assert_eq!(Format::Pdf.extension(), "pdf");
    }

    #[test]
    fn an_unknown_format_is_refused_rather_than_defaulted() {
        // A typo must not silently produce HTML where the user asked for a PDF.
        for s in ["HTML", "Pdf", "htm", "pdf ", "", "txt", "odt"] {
            assert_eq!(Format::parse(s), None, "{s:?} should not parse");
        }
    }
}
